/*
 * A child atfork handler registered before challoc initialises runs in
 * the child before challoc's own, in the fork window (design 9). If it
 * reads what another parent thread was building when the process forked
 * (the feature line of malloc_stats_print, the cgroup record of
 * `stats.cgroup.*`), that thread is not in the child: the read must not
 * wait for it (it spun forever there, with every signal blocked), but
 * reset the work as challoc's child handler would and do it itself.
 *
 * The driver (no argument) runs ROUNDS fresh processes (this program
 * re-executed with "round"), so the line and the record are unbuilt in
 * each. A round starts a thread that requests both while the main thread
 * forks; the child's handler requests both again. A child that has not
 * exited within the timeout is killed (SIGKILL: its signals are
 * blocked) and fails the round.
 */
#include "common.h"

#define ROUNDS 200
#define TIMEOUT_MS 10000

static void discard(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
}

static void read_both(void)
{
    malloc_stats_print(discard, NULL, "ab");
    size_t v = 0, len = sizeof v;
    (void)mallctl("stats.cgroup.limit", &v, &len, NULL, 0);
}

static void child_handler(void)
{
    read_both();
}

ATFORK_FIRST(NULL, NULL, child_handler);

static volatile int go;

static void *requester(void *arg)
{
    (void)arg;
    while (!go)
        ;
    read_both();
    return NULL;
}

/* Wait for `pid` up to `timeout_ms`: its exit status, or -1 after
 * killing it. */
static int wait_bounded(pid_t pid, int timeout_ms)
{
    for (int ms = 0; ms < timeout_ms; ms++) {
        int st = 0;
        pid_t r = waitpid(pid, &st, WNOHANG);
        if (r == pid)
            return WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
        REQUIRE(r == 0 || errno == EINTR, "waitpid: %s", strerror(errno));
        struct timespec t = {0, 1000 * 1000};
        nanosleep(&t, NULL);
    }
    kill(pid, SIGKILL);
    waitpid(pid, NULL, 0);
    return -1;
}

static int round_once(void)
{
    pthread_t th;
    REQUIRE(pthread_create(&th, NULL, requester, NULL) == 0, "pthread_create");
    go = 1;
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0)
        _exit(0);
    int rc = wait_bounded(pid, TIMEOUT_MS);
    REQUIRE(pthread_join(th, NULL) == 0, "pthread_join");
    if (rc != 0) {
        fprintf(stderr, "child %s\n", rc < 0 ? "hung in its fork handler (killed)" : "failed");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    require_challoc();
    if (argc > 1 && strcmp(argv[1], "round") == 0)
        return round_once();
    for (int i = 0; i < ROUNDS; i++) {
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork: %s", strerror(errno));
        if (pid == 0) {
            execl("/proc/self/exe", argv[0], "round", (char *)NULL);
            _exit(127);
        }
        int rc = wait_bounded(pid, 3 * TIMEOUT_MS);
        CHECK(rc == 0, "round %d: status %d", i, rc);
        if (failures)
            break;
    }
    if (failures)
        return 1;
    printf("ok fork_spin\n");
    return 0;
}
