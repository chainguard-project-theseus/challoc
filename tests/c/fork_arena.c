/*
 * No arena is built in the fork window (design 9). A prepare handler
 * registered before challoc initialises runs after challoc's, on the
 * forking thread, with every allocator lock held. An allocation of the
 * handler's whose home arena is not built yet is served from arena 0
 * (building it would make locks no fork handler holds, which the window
 * would serve as acquired), so the arena count is the same after the
 * fork; an `arenas.create` in the window is refused with EAGAIN. Another thread allocates throughout.
 */
#include "common.h"

const char *malloc_conf = "narenas:16";

static void *volatile keep;
static volatile int window_create_rc = -1;

static void prepare_handler(void)
{
    for (size_t n = 16; n <= 64 * 1024; n *= 2)
        free(malloc(n));
    keep = malloc(24);
    unsigned a = 0;
    size_t alen = sizeof a;
    window_create_rc = mallctl("arenas.create", &a, &alen, NULL, 0);
}

/* Registered from the program's .preinit_array, which runs before every
 * library's constructor, challoc's among them: before challoc registers
 * its own handlers, so these run inside challoc's fork window (design 9).
 * A handler registered later, from a constructor, runs outside it
 * (tests/c/atfork_pool.c). */
static void register_first(void)
{
    if (pthread_atfork(prepare_handler, NULL, NULL) != 0)
        _exit(3);
}

__attribute__((section(".preinit_array"), used)) static void (*const preinit_register)(void) = register_first;

/* malloc_stats_print hands its text over in 1 KiB chunks that can split a
 * line anywhere, so the callback accumulates the whole report and the
 * `Arenas:` line is parsed afterwards. */
static char report[1 << 16];
static size_t report_len;

static void append(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

/* The arenas built: the report's `Arenas:` count, those the scan found. */
static unsigned built_arenas(void)
{
    report_len = 0;
    malloc_stats_print(append, NULL, "ab");
    report[report_len] = 0;
    const char *a = strstr(report, "Arenas: ");
    REQUIRE(a != NULL, "no Arenas: line");
    unsigned seen = (unsigned)strtoul(a + 8, NULL, 10);
    REQUIRE(seen > 0, "Arenas: 0");
    return seen;
}

static volatile int stop, warm;

static void *allocator(void *arg)
{
    (void)arg;
    while (!stop) {
        void *p[32];
        for (int i = 0; i < 32; i++)
            p[i] = malloc(16 + (size_t)i * 48);
        for (int i = 0; i < 32; i++)
            free(p[i]);
        /* Its home is built from here on: the counts below see only
         * what the forks build. */
        warm = 1;
    }
    return NULL;
}

static volatile int round_rc;

/* A thread whose first allocator call reads its arena: its heap is homed
 * on an arena that may not be built yet. It forks. */
static void *forker(void *arg)
{
    (void)arg;
    unsigned home = 0;
    size_t len = sizeof home;
    REQUIRE(mallctl("thread.arena", &home, &len, NULL, 0) == 0, "thread.arena");
    unsigned before = built_arenas();
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0)
        _exit(0);
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        REQUIRE(errno == EINTR, "waitpid");
    unsigned after = built_arenas();
    round_rc = 0;
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0)
        round_rc = 1;
    else if (window_create_rc != EAGAIN)
        round_rc = 2;
    else if (after != before)
        round_rc = 3;
    if (round_rc)
        fprintf(stderr, "home %u: built %u -> %u, arenas.create in the window %d, child %#x\n", home, before,
                after, window_create_rc, st);
    return NULL;
}

int main(void)
{
    require_challoc();
    pthread_t al;
    REQUIRE(pthread_create(&al, NULL, allocator, NULL) == 0, "pthread_create");
    while (!warm)
        ;
    for (int round = 0; round < 12; round++) {
        pthread_t th;
        REQUIRE(pthread_create(&th, NULL, forker, NULL) == 0, "pthread_create");
        REQUIRE(pthread_join(th, NULL) == 0, "pthread_join");
        CHECK(round_rc == 0, "round %d: %d", round, round_rc);
    }
    stop = 1;
    REQUIRE(pthread_join(al, NULL) == 0, "pthread_join");
    if (failures)
        return 1;
    printf("ok fork_arena\n");
    return 0;
}
