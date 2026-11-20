/*
 * The main thread exits (`pthread_exit`) from inside a statistics
 * callback while other threads run on. The kernel keeps the exited
 * thread-group leader as a zombie until the last thread exits, so
 * `tgkill(pid, pid, 0)` still succeeds on it; when every other slot is
 * held, challoc reads the leader's state in /proc/self/stat to see that
 * it has exited, and the scratch object its abandoned call held goes
 * back to the pool (design 13.8, #468). With POOL_SLOTS - 1 threads
 * parked inside reports holding every other object:
 *   - while the main thread is still inside its report, one more call is
 *     EAGAIN (a live leader's object is never taken), also with no file
 *     descriptor to spare;
 *   - once it has exited there, with no file descriptor to spare (so
 *     /proc/self/stat cannot be opened) one more call is still EAGAIN:
 *     unreadable reads as live, the safe side;
 *   - with descriptors again, one more call gets its object (mallctl's
 *     epoch write is 0). Before the fix that call was EAGAIN too;
 *   - a 64th report then holds that object and finds no "(no memory)",
 *     and one more call is EAGAIN again: no live thread's object is taken
 *     for the exited leader's.
 */
#include "common.h"
#include <fcntl.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

/* stats.rs `pool::POOL_SLOTS`. */
#define POOL_SLOTS 64
#define PARKED (POOL_SLOTS - 1)

static pthread_barrier_t parked_in;
static volatile int unpark, leader_in, leader_go;

static void nap(void)
{
    struct timespec t = {0, 1000 * 1000};
    nanosleep(&t, NULL);
}

/* The main thread's callback: hold the report until told, then exit. */
static void hold_then_exit(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
    leader_in = 1;
    while (!leader_go)
        nap();
    pthread_exit(NULL);
}

static volatile int no_memory;

static void park(void *opaque, const char *s)
{
    if (strstr(s, "(no memory)"))
        no_memory = 1;
    int *first = opaque;
    if (*first) {
        *first = 0;
        pthread_barrier_wait(&parked_in);
        while (!unpark)
            nap();
    }
}

static void *parked_report(void *arg)
{
    (void)arg;
    int first = 1;
    malloc_stats_print(park, &first, "g");
    return NULL;
}

/* The leader's state letter from /proc/self/stat (after the last ')'). */
static char leader_state(void)
{
    char line[512];
    FILE *f = fopen("/proc/self/stat", "r");
    REQUIRE(f, "open /proc/self/stat: %s", strerror(errno));
    REQUIRE(fgets(line, sizeof line, f), "read /proc/self/stat");
    fclose(f);
    char *close = strrchr(line, ')');
    REQUIRE(close && close[1] == ' ' && close[2], "a stat line: %s", line);
    return close[2];
}

static int epoch(void)
{
    uint64_t e = 1;
    size_t elen = sizeof e;
    return mallctl("epoch", &e, &elen, &e, sizeof e);
}

/* `epoch()` with no file descriptor to spare: RLIMIT_NOFILE's soft limit
 * at the lowest free descriptor, so challoc's `open` of /proc/self/stat
 * fails with EMFILE (unreadable /proc reads as a live leader). */
static int epoch_without_fds(void)
{
    struct rlimit was;
    REQUIRE(getrlimit(RLIMIT_NOFILE, &was) == 0, "getrlimit");
    int lowest = dup(0);
    REQUIRE(lowest >= 0, "dup: %s", strerror(errno));
    close(lowest);
    struct rlimit none = {(rlim_t)lowest, was.rlim_max};
    REQUIRE(setrlimit(RLIMIT_NOFILE, &none) == 0, "setrlimit");
    int probe = open("/proc/self/stat", O_RDONLY);
    int probe_errno = errno;
    int r = epoch();
    REQUIRE(setrlimit(RLIMIT_NOFILE, &was) == 0, "setrlimit");
    REQUIRE(probe < 0 && probe_errno == EMFILE, "open with RLIMIT_NOFILE at %d did not fail with EMFILE", lowest);
    return r;
}

static void *survivor(void *arg)
{
    (void)arg;
    for (int waited = 0; !leader_in; waited++) {
        REQUIRE(waited < 10000, "the main thread did not reach its report's callback");
        nap();
    }
    REQUIRE(pthread_barrier_init(&parked_in, NULL, PARKED + 1) == 0, "barrier");
    pthread_t th[PARKED + 1];
    for (int i = 0; i < PARKED; i++)
        REQUIRE(pthread_create(&th[i], NULL, parked_report, NULL) == 0, "pthread_create");
    pthread_barrier_wait(&parked_in);
    REQUIRE(pthread_barrier_destroy(&parked_in) == 0, "barrier");
    int r = epoch();
    CHECK(r == EAGAIN, "epoch with %d reports parked and the leader in its report: %d, not EAGAIN", PARKED, r);
    r = epoch_without_fds();
    CHECK(r == EAGAIN, "epoch with no fds, %d reports parked and the leader in its report: %d, not EAGAIN", PARKED,
          r);
    leader_go = 1;
    for (int waited = 0; leader_state() != 'Z'; waited++) {
        REQUIRE(waited < 10000, "the main thread did not exit in its report");
        nap();
    }
    /* /proc unreadable: the leader reads as live, its object stays held. */
    r = epoch_without_fds();
    CHECK(r == EAGAIN, "epoch with no fds, %d reports parked and the leader gone: %d, not EAGAIN", PARKED, r);
    r = epoch();
    CHECK(r == 0, "epoch with %d reports parked and the leader gone: %d (%s)", PARKED, r,
          r == EAGAIN ? "the leader's object was not reclaimed" : strerror(r));
    /* One more report holds that object; then every object is a live
     * thread's again, and none is taken. */
    REQUIRE(pthread_barrier_init(&parked_in, NULL, 2) == 0, "barrier");
    REQUIRE(pthread_create(&th[PARKED], NULL, parked_report, NULL) == 0, "pthread_create");
    pthread_barrier_wait(&parked_in);
    CHECK(!no_memory, "a parked report found no object");
    r = epoch();
    CHECK(r == EAGAIN, "epoch with %d reports parked and the leader gone: %d, not EAGAIN", PARKED + 1, r);
    unpark = 1;
    for (int i = 0; i < PARKED + 1; i++)
        REQUIRE(pthread_join(th[i], NULL) == 0, "pthread_join");
    fflush(stdout);
    exit(finish("stats_leader_exit"));
}

int main(void)
{
    require_challoc();
    /* A hang fails the run rather than the driver. */
    alarm(60);
    pthread_t th;
    REQUIRE(pthread_create(&th, NULL, survivor, NULL) == 0, "pthread_create");
    malloc_stats_print(hold_then_exit, NULL, "g");
    REQUIRE(0, "the report returned past pthread_exit");
    return 1;
}
