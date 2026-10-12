/*
 * The setuid-family calls return while challoc's background thread runs
 * and while another thread starts it. glibc applies seteuid, setegid,
 * setresuid and the rest to every thread of a multithreaded process: it
 * takes its stack-list lock, sends each thread SIGSETXID (signal 33) and
 * waits for all of them to answer. A thread that keeps the signal blocked
 * never answers; neither does one that blocks it and then waits for that
 * lock (pthread_create on a user stack takes it). Either way the caller
 * waits forever.
 *
 * The driver runs this with background_thread:true. An idle thread
 * exists first: under glibc 2.44 a process's second thread starts with
 * SIGSETXID clear whatever its creator blocked, while later ones inherit
 * the creator's mask (Ruby has its timer thread by then). A tick
 * then starts the background thread, whose mask (SigBlk in /proc) must
 * leave SIGCANCEL (32) and SIGSETXID (33) clear. Then a helper thread
 * stops and restarts the background thread through
 * mallctl("background_thread") for RUN_MS while this thread calls each of
 * the four with the process's own ids, which needs no privilege and
 * still broadcasts. A watchdog alarm turns a hang into a failure.
 */
#include "common.h"

#include <dirent.h>
#include <limits.h>
#include <sched.h>

#define RUN_MS 1500

/* The tid of the thread named challoc-bg, or 0. */
static long find_bg(void)
{
    DIR *d = opendir("/proc/self/task");
    REQUIRE(d != NULL, "opendir /proc/self/task: %s", strerror(errno));
    long tid = 0;
    struct dirent *e;
    while (tid == 0 && (e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.')
            continue;
        char path[sizeof "/proc/self/task/" + NAME_MAX + sizeof "/comm"], comm[32] = "";
        snprintf(path, sizeof path, "/proc/self/task/%s/comm", e->d_name);
        FILE *f = fopen(path, "r");
        if (f) {
            if (fgets(comm, sizeof comm, f) && strncmp(comm, "challoc-bg", 10) == 0)
                tid = strtol(e->d_name, NULL, 10);
            fclose(f);
        }
    }
    closedir(d);
    return tid;
}

/* The SigBlk mask of thread `tid`. */
static unsigned long long sigblk(long tid)
{
    char path[64], line[256];
    snprintf(path, sizeof path, "/proc/self/task/%ld/status", tid);
    FILE *f = fopen(path, "r");
    REQUIRE(f != NULL, "open %s: %s", path, strerror(errno));
    unsigned long long mask = 0;
    int found = 0;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "SigBlk: %llx", &mask) == 1) {
            found = 1;
            break;
        }
    fclose(f);
    REQUIRE(found, "no SigBlk line in %s", path);
    return mask;
}

/* Drive the slow path (as decay.c's tick) so the thread starts. */
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

static bool bg_present(void)
{
    bool on = false;
    size_t len = sizeof on;
    REQUIRE(mallctl("background_thread", &on, &len, NULL, 0) == 0, "mallctl(background_thread) read");
    return on;
}

static volatile int done;
static int restarts;

/* Stop the background thread, wait until it has exited, start it again;
 * until `done`. */
static void *restarter(void *arg)
{
    (void)arg;
    while (!done) {
        bool off = false, on = true;
        REQUIRE(mallctl("background_thread", NULL, NULL, &off, sizeof off) == 0, "background_thread off");
        while (bg_present())
            sched_yield();
        REQUIRE(mallctl("background_thread", NULL, NULL, &on, sizeof on) == 0, "background_thread on");
        restarts++;
    }
    return NULL;
}

static const char *volatile current = "start";

static int idle_pipe[2];

/* A thread that only waits, so the background thread starts in a
 * process that already has two threads. */
static void *idle(void *arg)
{
    (void)arg;
    char c;
    while (read(idle_pipe[0], &c, 1) < 0 && errno == EINTR)
        ;
    return NULL;
}

static void on_alarm(int signum)
{
    (void)signum;
    static const char msg[] = "FATAL setxid: a setuid-family call hung: ";
    const char *c = current;
    if (write(STDERR_FILENO, msg, sizeof msg - 1) < 0 || write(STDERR_FILENO, c, strlen(c)) < 0
        || write(STDERR_FILENO, "\n", 1) < 0)
        _exit(1);
    _exit(1);
}

int main(void)
{
    require_challoc();
    pthread_t idler;
    REQUIRE(pipe(idle_pipe) == 0, "pipe: %s", strerror(errno));
    REQUIRE(pthread_create(&idler, NULL, idle, NULL) == 0, "pthread_create");
    tick();
    long bg = 0;
    for (int i = 0; i < 100 && bg == 0; i++) {
        bg = find_bg();
        if (bg == 0)
            sleep_ms(10);
    }
    REQUIRE(bg != 0, "background thread missing after a tick");
    unsigned long long mask = sigblk(bg);
    printf("setxid: challoc-bg SigBlk %016llx\n", mask);
    CHECK((mask & (1ULL << 31)) == 0, "challoc-bg blocks SIGCANCEL: SigBlk %016llx", mask);
    CHECK((mask & (1ULL << 32)) == 0, "challoc-bg blocks SIGSETXID: SigBlk %016llx", mask);

    signal(SIGALRM, on_alarm);
    alarm(20);
    pthread_t t;
    REQUIRE(pthread_create(&t, NULL, restarter, NULL) == 0, "pthread_create");
    uid_t uid = geteuid();
    gid_t gid = getegid();
    long calls = 0;
    for (long start = now_ms(); now_ms() - start < RUN_MS; calls++) {
        current = "setegid";
        CHECK(setegid(gid) == 0, "setegid(%u): %s", (unsigned)gid, strerror(errno));
        current = "seteuid";
        CHECK(seteuid(uid) == 0, "seteuid(%u): %s", (unsigned)uid, strerror(errno));
        current = "setresgid";
        CHECK(setresgid(-1, -1, -1) == 0, "setresgid(-1, -1, -1): %s", strerror(errno));
        current = "setresuid";
        CHECK(setresuid(-1, -1, -1) == 0, "setresuid(-1, -1, -1): %s", strerror(errno));
        if (failures)
            break;
    }
    current = "the restarter's last start";
    done = 1;
    REQUIRE(pthread_join(t, NULL) == 0, "pthread_join");
    REQUIRE(write(idle_pipe[1], "x", 1) == 1, "write");
    REQUIRE(pthread_join(idler, NULL) == 0, "pthread_join");
    alarm(0);
    printf("setxid: %ld rounds, %d restarts\n", calls, restarts);
    CHECK(restarts > 0, "the background thread never restarted");
    return finish("setxid");
}
