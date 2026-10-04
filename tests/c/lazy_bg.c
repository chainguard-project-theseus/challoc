/*
 * The lazy background thread (design 8.1) end to end under LD_PRELOAD,
 * with `background_thread` unset (the default). The driver in
 * src/api/c_programs.rs runs one mode per process with MALLOC_CONF set for
 * it:
 *
 *   small    (defaults)                a small allocation workload with the
 *                                      slow path ticking for 1.5 s: the
 *                                      process stays single-threaded and
 *                                      unshare(CLONE_NEWUSER) works
 *   burst    dirty_decay_ms:500        96 MiB freed at once: the next ticks
 *                                      start the thread, RSS drops with
 *                                      nobody touching the allocator; a
 *                                      fork child stays single-threaded
 *                                      through a small workload of its own;
 *                                      exec gives a fresh single-threaded
 *                                      image (the `small` mode again)
 *   hold     dirty_decay_ms:5000       8 MiB left dirty: no thread at once,
 *                                      one after about a second of ticks
 *   optout   background_thread:false,  the burst never starts a thread
 *            dirty_decay_ms:500
 *   seccomp  dirty_decay_ms:500        a seccomp filter fails clone and
 *                                      clone3 with EPERM: the burst starts
 *                                      nothing, the process keeps working and
 *                                      purges in the foreground
 *   nproc    dirty_decay_ms:500        the same with RLIMIT_NPROC at 1 (as
 *                                      nobody when run as root): clone
 *                                      fails with EAGAIN; once the limit is
 *                                      raised again, a later burst starts
 *                                      the thread (the lazy start retries)
 *   join     (defaults)                512 threads exit, 96 MiB is freed,
 *                                      then they are joined, on default and
 *                                      on user stacks: glibc frees their TLS
 *                                      with its stack cache lock held, and a
 *                                      free's tick must not create the
 *                                      thread there (pthread_create takes
 *                                      that lock); a watchdog kills a hang.
 *                                      The thread still starts afterwards
 *   forkburst (defaults)               a fork child of a clean parent frees
 *                                      96 MiB of its own: that starts the
 *                                      thread in the child
 *   forkdirty (defaults)               a parent forks with 96 MiB freed and
 *                                      not yet looked at: the child, with a
 *                                      small workload of its own, stays
 *                                      single-threaded; the parent starts
 *                                      the thread
 *   sched    (defaults)                a worker thread pinned to one CPU
 *                                      under SCHED_BATCH at nice 10 starts
 *                                      the thread: it runs SCHED_OTHER with
 *                                      the main thread's CPU affinity (and
 *                                      nice value, where lowering it is
 *                                      allowed)
 *
 * Threads are counted in /proc/self/task; the thread names itself
 * challoc-bg. Each skip has a claim name of its own (lazy_bg/unshare,
 * lazy_bg/userns, lazy_bg/nproc, lazy_bg/sched), so an expected skip of
 * one claim does not hide a new skip of another (ci/skip-summary.sh).
 */
#include "common.h"

#include <dirent.h>
#include <limits.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <stddef.h>
#include <sys/prctl.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <sys/syscall.h>

#define MIB (1024UL * 1024UL)

static char *self_path;

/* Number of threads in this process and whether one is named challoc-bg. */
static int count_tasks(int *bg)
{
    DIR *d = opendir("/proc/self/task");
    REQUIRE(d != NULL, "opendir /proc/self/task: %s", strerror(errno));
    int n = 0;
    *bg = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.')
            continue;
        n++;
        char path[sizeof "/proc/self/task/" + NAME_MAX + sizeof "/comm"], comm[32] = "";
        snprintf(path, sizeof path, "/proc/self/task/%s/comm", e->d_name);
        FILE *f = fopen(path, "r");
        if (f) {
            if (fgets(comm, sizeof comm, f) && strncmp(comm, "challoc-bg", 10) == 0)
                *bg = 1;
            fclose(f);
        }
    }
    closedir(d);
    return n;
}

static int tasks(void)
{
    int bg;
    return count_tasks(&bg);
}

/* Drive the slow path (as decay.c's tick): a class the working sets do
 * not use, 16 slow paths per round, periodic work every 64. */
static void tick(void)
{
    enum { N = 4096, ROUNDS = 16 };
    static void *volatile blocks[N];
    for (int r = 0; r < ROUNDS; r++) {
        for (int i = 0; i < N; i++) {
            blocks[i] = malloc(48);
            REQUIRE(blocks[i] != NULL, "malloc(48)");
        }
        for (int i = 0; i < N; i++)
            free(blocks[i]);
    }
}

/* Allocate, touch and free `small` bytes of 1 KiB blocks and `huge`
 * bytes of 4 MiB runs. */
static void churn(size_t small, size_t huge)
{
    size_t ns = small / 1024, nh = huge / (4 * MIB);
    void **b = malloc((ns + nh + 1) * sizeof *b);
    REQUIRE(b != NULL, "malloc table");
    for (size_t i = 0; i < ns; i++) {
        b[i] = malloc(1024);
        REQUIRE(b[i] != NULL, "malloc(1024)");
        memset(b[i], 0x5a, 1024);
    }
    for (size_t i = 0; i < nh; i++) {
        b[ns + i] = malloc(4 * MIB);
        REQUIRE(b[ns + i] != NULL, "malloc(4 MiB)");
        memset(b[ns + i], 0x5a, 4 * MIB);
    }
    for (size_t i = 0; i < ns + nh; i++)
        free(b[i]);
    free(b);
}

/* unshare(CLONE_NEWUSER) in a fork child (so this process keeps its
 * namespaces): 0 when it worked, else the errno. EINVAL is what a
 * multithreaded caller gets. */
static int unshare_child(void *arg)
{
    (void)arg;
    return unshare(CLONE_NEWUSER) == 0 ? 0 : errno;
}

/* The claim of `small`: a single-threaded process can unshare a user
 * namespace. Run in this process when it is single-threaded; a sandbox
 * that refuses user namespaces (Docker's seccomp profile: EPERM) is a
 * skip, as is EINVAL with one task (a kernel without user namespaces);
 * a multithreaded caller's EINVAL is a failure. */
static void check_unshare(const char *who)
{
    int rc = unshare(CLONE_NEWUSER) == 0 ? 0 : errno;
    if (rc == 0) {
        printf("%s: unshare(CLONE_NEWUSER) ok\n", who);
    } else if (rc == EINVAL && tasks() > 1) {
        CHECK(0, "%s: unshare(CLONE_NEWUSER) EINVAL: the process is multithreaded (%d tasks)", who, tasks());
    } else if (rc == EINVAL) {
        /* Single-threaded and still EINVAL: a kernel without user
         * namespaces (CONFIG_USER_NS off, the floor kernels). The task
         * count above is the claim that remains. */
        skip_note("lazy_bg/userns", "%s: unshare(CLONE_NEWUSER) EINVAL with one task: no user namespaces in this kernel", who);
    } else {
        skip_note("lazy_bg/unshare", "%s: unshare(CLONE_NEWUSER) refused here: %s", who, strerror(rc));
    }
}

/* Tick for `ms` with a small workload (1 MiB of blocks, nothing huge):
 * well under the lazy start's floor. Returns the most tasks seen. */
static int small_workload(long ms)
{
    int most = 0;
    long end = now_ms() + ms;
    while (now_ms() < end) {
        churn(1 * MIB, 0);
        tick();
        int n = tasks();
        if (n > most)
            most = n;
        sleep_ms(20);
    }
    return most;
}

static void mode_small(const char *who)
{
    CHECK(tasks() == 1, "%s: %d tasks at start", who, tasks());
    int most = small_workload(1500);
    CHECK(most == 1, "%s: a small workload started a thread (%d tasks)", who, most);
    check_unshare(who);
}

/* Tick until the background thread shows up or `ms` pass; the time it took,
 * or -1. */
static long wait_thread(long ms)
{
    long t0 = now_ms();
    while (now_ms() - t0 < ms) {
        tick();
        int bg;
        if (count_tasks(&bg) == 2 && bg)
            return now_ms() - t0;
        sleep_ms(10);
    }
    return -1;
}

static size_t drop(size_t before, size_t after)
{
    return before > after ? before - after : 0;
}

static int child_small(void *arg)
{
    (void)arg;
    if (tasks() != 1)
        return 2;
    if (small_workload(1500) != 1)
        return 3;
    int rc = unshare_child(NULL);
    if (rc == EINVAL && tasks() > 1)
        return 4;
    return 0;
}

static void mode_burst(void)
{
    CHECK(tasks() == 1, "%d tasks at start", tasks());
    size_t base = rss_bytes();
    churn(64 * MIB, 32 * MIB);
    size_t full = rss_bytes();
    long t = wait_thread(2000);
    printf("burst: thread after %ld ms, rss %zu MiB over the start\n", t, drop(full, base) / MIB);
    CHECK(t >= 0, "no background thread after a 96 MiB burst");
    /* The control: a multithreaded process cannot unshare a user namespace
     * (EPERM where seccomp refuses it first). */
    int rc = unshare(CLONE_NEWUSER) == 0 ? 0 : errno;
    CHECK(rc != 0, "unshare(CLONE_NEWUSER) worked with the background thread running");
    printf("burst: unshare with the thread: %s\n", strerror(rc));
    /* No allocator activity from here: only the thread can purge. */
    size_t late = full;
    for (int i = 0; i < 60 && drop(full, late) < 80 * MIB; i++) {
        sleep_ms(50);
        late = rss_bytes();
    }
    printf("burst: rss %zu -> %zu MiB idle\n", full / MIB, late / MIB);
    CHECK(drop(full, late) >= 80 * MIB, "the idle process released only %zu MiB", drop(full, late) / MIB);

    /* A fork child has no thread and, with a small workload of its own,
     * does not start one (what it inherited is not its own). */
    fflush(stdout);
    rc = fork_and_wait(child_small, NULL);
    CHECK(rc == 0, "fork child: %d (2: threads at start, 3: a small workload started one, 4: unshare EINVAL)", rc);
    CHECK(tasks() == 2, "the parent lost its thread across the fork: %d tasks", tasks());

    /* exec: a fresh image, single-threaded until it builds up dirty
     * memory of its own. */
    if (failures)
        return;
    printf("burst: exec small\n");
    fflush(stdout);
    char *argv[] = { self_path, "exec-small", NULL };
    execv(self_path, argv);
    REQUIRE(0, "execv %s: %s", self_path, strerror(errno));
}

static void mode_hold(void)
{
    churn(4 * MIB, 4 * MIB);
    /* 8 MiB dirty, over the floor and under the burst threshold: the first
     * look starts the stretch, so nothing within half a second. */
    long t0 = now_ms();
    long t = -1;
    while (now_ms() - t0 < 3000) {
        tick();
        int bg;
        if (count_tasks(&bg) == 2 && bg) {
            t = now_ms() - t0;
            break;
        }
        sleep_ms(20);
    }
    printf("hold: thread after %ld ms\n", t);
    CHECK(t >= 0, "no background thread after 3 s with 8 MiB dirty");
    CHECK(t < 0 || t >= 800, "the thread started after only %ld ms", t);
}

static void mode_optout(void)
{
    churn(64 * MIB, 32 * MIB);
    long t = wait_thread(1500);
    CHECK(t < 0, "background_thread:false started a thread after %ld ms", t);
    check_unshare("optout");
}

static void install_clone_filter(void)
{
    struct sock_filter f[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_clone, 2, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_clone3, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (EPERM & SECCOMP_RET_DATA)),
    };
    struct sock_fprog prog = { .len = sizeof f / sizeof f[0], .filter = f };
    REQUIRE(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0, "PR_SET_NO_NEW_PRIVS: %s", strerror(errno));
    REQUIRE(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog) == 0, "PR_SET_SECCOMP: %s", strerror(errno));
}

static void *noop(void *arg)
{
    return arg;
}

/* A process that cannot create threads: the burst starts nothing, the
 * lazy start gives up for good, and decay keeps purging in the
 * foreground. */
static void refused(const char *who)
{
    churn(64 * MIB, 32 * MIB);
    size_t full = rss_bytes();
    long t = wait_thread(1000);
    CHECK(t < 0, "%s: a thread started where clone fails", who);
    /* Foreground purging still works: ticks past the 500 ms window. */
    size_t late = full;
    for (int i = 0; i < 40 && drop(full, late) < 80 * MIB; i++) {
        tick();
        sleep_ms(50);
        late = rss_bytes();
    }
    printf("%s: rss %zu -> %zu MiB with ticks\n", who, full / MIB, late / MIB);
    CHECK(drop(full, late) >= 80 * MIB, "%s: foreground decay released only %zu MiB", who, drop(full, late) / MIB);
}

/* Bursts a retry apart: clone still fails, so none starts a thread, and
 * after the third failure the lazy start stops trying. */
static void refused_again(const char *who)
{
    for (int i = 0; i < 3; i++) {
        sleep_ms(1100);
        churn(64 * MIB, 32 * MIB);
        CHECK(wait_thread(500) < 0, "%s: a thread started on burst %d", who, i + 2);
    }
}

static void mode_seccomp(void)
{
    install_clone_filter();
    pthread_t th;
    CHECK(pthread_create(&th, NULL, noop, NULL) != 0, "the filter let pthread_create through");
    refused("seccomp");
    refused_again("seccomp");
}

static void mode_nproc(void)
{
    if (geteuid() == 0) {
        /* RLIMIT_NPROC does not bind root: become nobody (and stay
         * dumpable, so /proc/self stays readable). */
        if (setgid(65534) != 0 || setuid(65534) != 0) {
            skip_note("lazy_bg/nproc", "nproc: cannot drop to nobody: %s", strerror(errno));
            return;
        }
        REQUIRE(prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) == 0, "PR_SET_DUMPABLE: %s", strerror(errno));
    }
    struct rlimit old;
    REQUIRE(getrlimit(RLIMIT_NPROC, &old) == 0, "getrlimit(RLIMIT_NPROC): %s", strerror(errno));
    struct rlimit one = { 1, old.rlim_max };
    REQUIRE(setrlimit(RLIMIT_NPROC, &one) == 0, "setrlimit(RLIMIT_NPROC): %s", strerror(errno));
    pthread_t th;
    int rc = pthread_create(&th, NULL, noop, NULL);
    if (rc == 0) {
        pthread_join(th, NULL);
        skip_note("lazy_bg/nproc", "nproc: RLIMIT_NPROC 1 did not refuse a thread here");
        return;
    }
    printf("nproc: pthread_create: %s\n", strerror(rc));
    refused("nproc");
    /* The limit frees up (the soft limit back to the hard one): the lazy
     * start, which has failed at most twice by now, retries a second
     * after its last failure and the next burst starts the thread. */
    REQUIRE(setrlimit(RLIMIT_NPROC, &old) == 0, "setrlimit(RLIMIT_NPROC) back: %s", strerror(errno));
    sleep_ms(1100);
    churn(64 * MIB, 32 * MIB);
    long t = wait_thread(2500);
    printf("nproc: thread %ld ms after the limit was raised\n", t);
    CHECK(t >= 0, "nproc: the lazy start did not retry once the limit freed up");
}

/* The join hazard: `user` threads on stacks of their own (glibc frees
 * their TLS in pthread_join itself) or on glibc's (the stack cache trim
 * frees them). Returns 0 when the thread started with no hang, 5 when it
 * never started. */
static int join_child(void *arg)
{
    enum { N = 512, STK = 64 * 1024, ROUNDS = 20 };
    int user = arg != NULL;
    char *stacks = NULL;
    static pthread_attr_t attr[N];
    static pthread_t t[N];
    static void *big[24];
    if (user) {
        stacks = mmap(NULL, (size_t)N * STK, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stacks == MAP_FAILED)
            return 6;
        for (int i = 0; i < N; i++) {
            pthread_attr_init(&attr[i]);
            pthread_attr_setstack(&attr[i], stacks + (size_t)i * STK, STK);
        }
    }
    for (int round = 0; round < ROUNDS; round++) {
        for (int i = 0; i < 24; i++) {
            big[i] = malloc(4 * MIB);
            if (big[i] == NULL)
                return 7;
            memset(big[i], 1, 4 * MIB);
        }
        for (int i = 0; i < N; i++)
            if (pthread_create(&t[i], user ? &attr[i] : NULL, noop, NULL) != 0)
                return 8;
        sleep_ms(150);
        for (int i = 0; i < 24; i++)
            free(big[i]);
        for (int i = 0; i < N; i++)
            pthread_join(t[i], NULL);
        int bg;
        if (count_tasks(&bg) == 2 && bg) {
            printf("join %s: thread after round %d\n", user ? "user" : "default", round);
            return 0;
        }
    }
    long ms = wait_thread(2000);
    printf("join %s: thread %ld ms after the rounds\n", user ? "user" : "default", ms);
    return ms >= 0 ? 0 : 5;
}

/* fork_and_wait with a watchdog: a child still running after `ms` is
 * killed (its signals may all be blocked mid-hang) and reported as -1. */
static int fork_and_watch(int (*child_fn)(void *), void *arg, long ms)
{
    fflush(stdout);
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        int rc = child_fn(arg);
        fflush(stdout);
        _exit(rc & 0xff);
    }
    long t0 = now_ms();
    int status = 0;
    for (;;) {
        pid_t r = waitpid(pid, &status, WNOHANG);
        REQUIRE(r >= 0 || errno == EINTR, "waitpid: %s", strerror(errno));
        if (r == pid)
            break;
        if (now_ms() - t0 > ms) {
            kill(pid, SIGKILL);
            waitpid(pid, &status, 0);
            return -1;
        }
        sleep_ms(20);
    }
    if (WIFEXITED(status))
        return WEXITSTATUS(status);
    return -100 - (WIFSIGNALED(status) ? WTERMSIG(status) : 0);
}

static void mode_join(void)
{
    int rc = fork_and_watch(join_child, NULL, 60000);
    CHECK(rc == 0, "join, default stacks: %d (-1: hung, 5: never started)", rc);
    rc = fork_and_watch(join_child, (void *)1, 60000);
    CHECK(rc == 0, "join, user stacks: %d (-1: hung, 5: never started)", rc);
}

static int child_burst(void *arg)
{
    (void)arg;
    if (tasks() != 1)
        return 2;
    churn(0, 96 * MIB);
    long t = wait_thread(2000);
    printf("forkburst: child thread after %ld ms\n", t);
    return t >= 0 ? 0 : 3;
}

static void mode_forkburst(void)
{
    CHECK(tasks() == 1, "%d tasks at start", tasks());
    int rc = fork_and_watch(child_burst, NULL, 30000);
    CHECK(rc == 0, "fork child of a clean parent: %d (2: threads at start, 3: its own 96 MiB started nothing)", rc);
    CHECK(tasks() == 1, "the parent started a thread: %d tasks", tasks());
}

/* A parent that forks with 96 MiB freed and not looked at yet (huge frees
 * do not tick): the child inherits it as dirty memory that is not its own
 * and stays single-threaded through a small workload. */
static int child_dirty(void *arg)
{
    (void)arg;
    if (tasks() != 1)
        return 2;
    int most = small_workload(1500);
    printf("forkdirty: child at most %d tasks\n", most);
    return most == 1 ? 0 : 3;
}

static void mode_forkdirty(void)
{
    CHECK(tasks() == 1, "%d tasks at start", tasks());
    churn(0, 96 * MIB);
    CHECK(tasks() == 1, "huge frees started a thread: %d tasks", tasks());
    int rc = fork_and_watch(child_dirty, NULL, 30000);
    CHECK(rc == 0, "fork child of a dirty parent: %d (2: threads at start, 3: the inherited memory started one)", rc);
    /* The control: the same memory is the parent's own. */
    long t = wait_thread(2000);
    printf("forkdirty: parent thread after %ld ms\n", t);
    CHECK(t >= 0, "the parent's own 96 MiB started nothing");
}

/* The challoc-bg thread's id, or 0. */
static pid_t bg_tid(void)
{
    DIR *d = opendir("/proc/self/task");
    REQUIRE(d != NULL, "opendir /proc/self/task: %s", strerror(errno));
    pid_t tid = 0;
    struct dirent *e;
    while (tid == 0 && (e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.')
            continue;
        char path[sizeof "/proc/self/task/" + NAME_MAX + sizeof "/comm"], comm[32] = "";
        snprintf(path, sizeof path, "/proc/self/task/%s/comm", e->d_name);
        FILE *f = fopen(path, "r");
        if (f) {
            if (fgets(comm, sizeof comm, f) && strncmp(comm, "challoc-bg", 10) == 0)
                tid = (pid_t)atoi(e->d_name);
            fclose(f);
        }
    }
    closedir(d);
    return tid;
}

static long sched_worker_ms;

static void *sched_worker(void *arg)
{
    int cpu = (int)(intptr_t)arg;
    cpu_set_t one;
    CPU_ZERO(&one);
    CPU_SET(cpu, &one);
    REQUIRE(sched_setaffinity(0, sizeof one, &one) == 0, "pin the worker: %s", strerror(errno));
    struct sched_param p = { 0 };
    REQUIRE(sched_setscheduler(0, SCHED_BATCH, &p) == 0, "SCHED_BATCH: %s", strerror(errno));
    REQUIRE(setpriority(PRIO_PROCESS, 0, 10) == 0, "nice 10: %s", strerror(errno));
    churn(64 * MIB, 32 * MIB);
    /* wait_thread counts on two tasks; here the main thread waits too. */
    long t0 = now_ms();
    sched_worker_ms = -1;
    while (now_ms() - t0 < 2000) {
        tick();
        if (bg_tid() != 0) {
            sched_worker_ms = now_ms() - t0;
            break;
        }
        sleep_ms(10);
    }
    return NULL;
}

static void mode_sched(void)
{
    cpu_set_t main_set;
    REQUIRE(sched_getaffinity(0, sizeof main_set, &main_set) == 0, "sched_getaffinity: %s", strerror(errno));
    if (CPU_COUNT(&main_set) < 2) {
        skip_note("lazy_bg/sched", "sched: one CPU allowed, a pinned worker shows nothing");
        return;
    }
    int cpu = -1;
    for (int i = CPU_SETSIZE - 1; i >= 0 && cpu < 0; i--)
        if (CPU_ISSET(i, &main_set))
            cpu = i;
    errno = 0;
    int main_nice = getpriority(PRIO_PROCESS, 0);
    REQUIRE(errno == 0, "getpriority: %s", strerror(errno));
    pthread_t th;
    REQUIRE(pthread_create(&th, NULL, sched_worker, (void *)(intptr_t)cpu) == 0, "pthread_create");
    pthread_join(th, NULL);
    CHECK(sched_worker_ms >= 0, "sched: the worker's burst started no thread");
    pid_t tid = bg_tid();
    if (tid == 0)
        return;
    cpu_set_t bg_set;
    REQUIRE(sched_getaffinity(tid, sizeof bg_set, &bg_set) == 0, "sched_getaffinity(bg): %s", strerror(errno));
    CHECK(CPU_EQUAL(&bg_set, &main_set), "sched: challoc-bg has %d CPUs, the main thread %d (it kept the worker's pin)",
          CPU_COUNT(&bg_set), CPU_COUNT(&main_set));
    int policy = sched_getscheduler(tid);
    CHECK(policy == SCHED_OTHER, "sched: challoc-bg runs policy %d, not SCHED_OTHER", policy);
    errno = 0;
    int nice = getpriority(PRIO_PROCESS, (id_t)tid);
    REQUIRE(errno == 0, "getpriority(bg): %s", strerror(errno));
    /* Back to the main thread's nice value where lowering it is allowed
     * (CAP_SYS_NICE or RLIMIT_NICE), else the worker's. */
    CHECK(nice == main_nice || nice == 10, "sched: challoc-bg at nice %d (main %d, worker 10)", nice, main_nice);
    printf("sched: challoc-bg on %d CPUs, SCHED_OTHER, nice %d (main %d, worker 10)\n", CPU_COUNT(&bg_set), nice,
           main_nice);
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: lazy_bg <small|burst|hold|optout|seccomp|nproc|join|forkburst|forkdirty|sched>");
    self_path = argv[0];
    printf("lazy_bg: start %s\n", argv[1]);
    fflush(stdout);
    require_challoc();
    if (strcmp(argv[1], "small") == 0)
        mode_small("small");
    else if (strcmp(argv[1], "exec-small") == 0)
        mode_small("exec");
    else if (strcmp(argv[1], "burst") == 0)
        mode_burst();
    else if (strcmp(argv[1], "hold") == 0)
        mode_hold();
    else if (strcmp(argv[1], "optout") == 0)
        mode_optout();
    else if (strcmp(argv[1], "seccomp") == 0)
        mode_seccomp();
    else if (strcmp(argv[1], "nproc") == 0)
        mode_nproc();
    else if (strcmp(argv[1], "join") == 0)
        mode_join();
    else if (strcmp(argv[1], "forkburst") == 0)
        mode_forkburst();
    else if (strcmp(argv[1], "forkdirty") == 0)
        mode_forkdirty();
    else if (strcmp(argv[1], "sched") == 0)
        mode_sched();
    else
        REQUIRE(0, "unknown mode %s", argv[1]);
    return finish("lazy_bg");
}
