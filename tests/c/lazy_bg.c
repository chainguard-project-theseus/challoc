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
 *
 * Threads are counted in /proc/self/task; the thread names itself
 * challoc-bg.
 */
#include "common.h"

#include <dirent.h>
#include <limits.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <stddef.h>
#include <sys/prctl.h>
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
 * skip, a multithreaded caller's EINVAL a failure. */
static void check_unshare(const char *who)
{
    int rc = unshare(CLONE_NEWUSER) == 0 ? 0 : errno;
    if (rc == 0) {
        printf("%s: unshare(CLONE_NEWUSER) ok\n", who);
    } else if (rc == EINVAL) {
        CHECK(0, "%s: unshare(CLONE_NEWUSER) EINVAL: the process is multithreaded (%d tasks)", who, tasks());
    } else {
        skip_note("lazy_bg", "%s: unshare(CLONE_NEWUSER) refused here: %s", who, strerror(rc));
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
    if (rc == EINVAL)
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

static void mode_seccomp(void)
{
    install_clone_filter();
    pthread_t th;
    CHECK(pthread_create(&th, NULL, noop, NULL) != 0, "the filter let pthread_create through");
    churn(64 * MIB, 32 * MIB);
    size_t full = rss_bytes();
    long t = wait_thread(1000);
    CHECK(t < 0, "a thread started under a filter that refuses clone");
    /* Foreground purging still works: ticks past the 500 ms window. */
    size_t late = full;
    for (int i = 0; i < 40 && drop(full, late) < 80 * MIB; i++) {
        tick();
        sleep_ms(50);
        late = rss_bytes();
    }
    printf("seccomp: rss %zu -> %zu MiB with ticks\n", full / MIB, late / MIB);
    CHECK(drop(full, late) >= 80 * MIB, "foreground decay released only %zu MiB", drop(full, late) / MIB);
    /* A second burst: still nothing, still working. */
    churn(64 * MIB, 32 * MIB);
    CHECK(wait_thread(500) < 0, "a thread started on the second burst");
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: lazy_bg <small|burst|hold|optout|seccomp>");
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
    else
        REQUIRE(0, "unknown mode %s", argv[1]);
    return finish("lazy_bg");
}
