/*
 * fork with allocation in both parent and child. The child must see the
 * parent's blocks intact, allocate and free in every tier, create threads of
 * its own (its heap and the class locks must be usable after the atfork
 * handlers), fork a grandchild, and exit cleanly; the parent keeps
 * allocating throughout, including from other threads that are mid-malloc
 * at the moment of the fork.
 *
 * `fork_alloc mallctl` forks while other threads
 * loop on `mallctl("epoch")` and `tcache.create`/`tcache.destroy`, the
 * calls that take the statistics and explicit-cache locks the fork handler
 * used to skip. Each child allocates, calls `epoch` and creates and
 * destroys a cache itself; a child that inherits one of those locks held
 * by a thread that no longer exists hangs, so it runs under an alarm and
 * the parent kills anything the alarm missed, and both count as failures
 * instead of a hang.
 */
#include "common.h"
#include <signal.h>
#include <sys/wait.h>
#include <time.h>

#define INHERITED 64

struct inherited {
    unsigned char *p;
    size_t n;
    uint32_t seed;
};

static struct inherited inh[INHERITED];

static void make_inherited(void)
{
    static const size_t ns[] = {1, 24, 100, 1000, 4096, 70000, 262145, 3 << 20};
    for (int i = 0; i < INHERITED; i++) {
        inh[i].n = ns[i % 8] + (size_t)i;
        inh[i].seed = 1000u + (uint32_t)i;
        inh[i].p = malloc(inh[i].n);
        REQUIRE(inh[i].p, "inherited malloc(%zu)", inh[i].n);
        fill(inh[i].p, inh[i].n, inh[i].seed);
    }
}

static int inherited_ok(void)
{
    for (int i = 0; i < INHERITED; i++)
        if (!check_fill(inh[i].p, inh[i].n, inh[i].seed))
            return 0;
    return 1;
}

/* Allocate, fill, verify and free across all tiers; used by both sides. */
static int churn(uint64_t seed, int rounds)
{
    uint64_t rng = seed;
    unsigned char *ptrs[512];
    size_t ns[512];
    for (int r = 0; r < rounds; r++) {
        for (int i = 0; i < 512; i++) {
            uint64_t v = rng_next(&rng);
            ns[i] = (v % 8 == 0) ? 262145 + (size_t)((v >> 8) % 100000) : (size_t)((v >> 8) % 3000) + 1;
            ptrs[i] = (i % 3 == 0) ? calloc(1, ns[i]) : malloc(ns[i]);
            if (!ptrs[i])
                return 0;
            if (i % 3 == 0 && !is_zero(ptrs[i], ns[i]))
                return 0;
            fill(ptrs[i], ns[i], (uint32_t)i + (uint32_t)r);
        }
        for (int i = 0; i < 512; i++) {
            if (!check_fill(ptrs[i], ns[i], (uint32_t)i + (uint32_t)r))
                return 0;
            free(ptrs[i]);
        }
    }
    return 1;
}

static void *child_thread(void *arg)
{
    *(int *)arg = churn(0xC411D ^ (uintptr_t)arg, 3);
    return NULL;
}

/* Runs in the child. Returns the exit status. */
static int child_body(int round)
{
    if (!inherited_ok())
        return 10;
    if (!churn(0xC0DE + (uint64_t)round, 4))
        return 11;
    /* Free half of what we inherited (the parent still owns its copy). */
    for (int i = 0; i < INHERITED; i += 2)
        free(inh[i].p);
    /* The child's own threads: they need fresh heaps after fork. */
    enum { T = 4 };
    pthread_t t[T];
    int ok[T] = {0};
    for (int i = 0; i < T; i++)
        if (pthread_create(&t[i], NULL, child_thread, &ok[i]) != 0)
            return 12;
    for (int i = 0; i < T; i++)
        pthread_join(t[i], NULL);
    for (int i = 0; i < T; i++)
        if (!ok[i])
            return 13;
    /* realloc the other inherited half across tiers. */
    for (int i = 1; i < INHERITED; i += 2) {
        size_t n = inh[i].n * 2 + 1;
        unsigned char *q = realloc(inh[i].p, n);
        if (!q || !check_fill(q, inh[i].n, inh[i].seed))
            return 14;
        free(q);
    }
    /* Grandchild. */
    if (round % 5 == 0) {
        pid_t g = fork();
        if (g < 0)
            return 15;
        if (g == 0)
            _exit(churn(0x6C, 2) ? 0 : 1);
        int status = 0;
        if (waitpid(g, &status, 0) != g || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            return 16;
    }
    return 0;
}

/* Background threads allocating while the main thread forks. */
static volatile int stop;

static void *background(void *arg)
{
    uint64_t rng = 0xBA5E ^ (uintptr_t)arg;
    void *ring[256] = {0};
    long n = 0;
    while (!stop) {
        int k = (int)(rng_next(&rng) % 256);
        free(ring[k]);
        size_t sz = (size_t)(rng_next(&rng) % 2000) + 1;
        if (n % 300 == 0)
            sz = 300000 + (size_t)(rng_next(&rng) % 100000);
        ring[k] = malloc(sz);
        REQUIRE(ring[k], "background malloc");
        memset(ring[k], 0x42, sz);
        n++;
    }
    for (int k = 0; k < 256; k++)
        free(ring[k]);
    return NULL;
}

/* fork from a thread that is not the main thread: only that thread survives
 * in the child, and it must be able to allocate and create threads. */
static void *forker(void *arg)
{
    int *result = arg;
    void *mine = malloc(777);
    REQUIRE(mine, "forker malloc");
    fill(mine, 777, 77);
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork from thread");
    if (pid == 0) {
        if (!check_fill(mine, 777, 77))
            _exit(20);
        _exit(child_body(1));
    }
    int status = 0;
    *result = waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    if (!*result)
        fprintf(stderr, "forker: child status %#x\n", status);
    free(mine);
    return NULL;
}

/* ---- mallctl mode ------------------------------------------------------ */

static int mallctl_epoch(void)
{
    uint64_t epoch = 1;
    size_t sz = sizeof epoch;
    return mallctl("epoch", &epoch, &sz, &epoch, sizeof epoch);
}

/* A thread that keeps the statistics and explicit-cache locks busy. */
static void *mallctl_churn(void *arg)
{
    uint64_t rng = 0x3A11 ^ (uintptr_t)arg;
    long rounds = 0;
    while (!stop) {
        REQUIRE(mallctl_epoch() == 0, "epoch");
        unsigned id;
        size_t sz = sizeof id;
        REQUIRE(mallctl("tcache.create", &id, &sz, NULL, 0) == 0, "tcache.create");
        if (rng_next(&rng) % 4 == 0) {
            void *p = malloc((size_t)(rng_next(&rng) % 3000) + 1);
            REQUIRE(p, "mallctl churn malloc");
            free(p);
        }
        REQUIRE(mallctl("tcache.destroy", NULL, NULL, &id, sizeof id) == 0, "tcache.destroy");
        rounds++;
    }
    return (void *)rounds;
}

/* Runs in the child: the calls whose locks the fork must have released. */
static int mallctl_child(void)
{
    alarm(20);
    void *p = malloc(100);
    if (!p)
        return 30;
    free(p);
    if (mallctl_epoch() != 0)
        return 31;
    unsigned id;
    size_t sz = sizeof id;
    if (mallctl("tcache.create", &id, &sz, NULL, 0) != 0)
        return 32;
    if (mallctl("tcache.destroy", NULL, NULL, &id, sizeof id) != 0)
        return 33;
    if (!churn(0xF0, 1))
        return 34;
    return 0;
}

/* waitpid with a deadline: a child still alive after `secs` is killed and
 * reported as such. */
static int wait_child(pid_t pid, int *status, int secs)
{
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_sec += secs;
    for (;;) {
        pid_t r = waitpid(pid, status, WNOHANG);
        if (r == pid)
            return 1;
        if (r < 0)
            return 0;
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        if (now.tv_sec > deadline.tv_sec || (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
            fprintf(stderr, "child %d still alive after %d s: killing it\n", (int)pid, secs);
            kill(pid, SIGKILL);
            waitpid(pid, status, 0);
            return 0;
        }
        struct timespec ts = {0, 200000};
        nanosleep(&ts, NULL);
    }
}

static int mallctl_mode(void)
{
    REQUIRE(mallctl != NULL, "mallctl not exported");
    enum { T = 3, ROUNDS = 200 };
    pthread_t th[T];
    for (int i = 0; i < T; i++)
        REQUIRE(pthread_create(&th[i], NULL, mallctl_churn, (void *)(uintptr_t)i) == 0, "mallctl thread");
    /* Let the threads reach their loops before the first fork. */
    struct timespec ts = {0, 20000000};
    nanosleep(&ts, NULL);
    for (int round = 0; round < ROUNDS; round++) {
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork round %d", round);
        if (pid == 0)
            _exit(mallctl_child());
        int status = 0;
        CHECK(wait_child(pid, &status, 30), "mallctl child round %d did not finish", round);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "mallctl child round %d status %#x (code %d)", round,
              status, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        if (failures)
            break;
    }
    stop = 1;
    long total = 0;
    for (int i = 0; i < T; i++) {
        void *r = NULL;
        pthread_join(th[i], &r);
        total += (long)(uintptr_t)r;
    }
    printf("fork_alloc: mallctl mode, %d forks, %ld epoch/tcache rounds in %d threads\n", ROUNDS, total, T);
    return finish("fork_alloc");
}

int main(int argc, char **argv)
{
    require_challoc();
    if (argc > 1 && strcmp(argv[1], "mallctl") == 0)
        return mallctl_mode();
    REQUIRE(argc == 1, "unknown mode %s", argv[1]);
    make_inherited();

    enum { BG = 3 };
    pthread_t bg[BG];
    for (int i = 0; i < BG; i++)
        REQUIRE(pthread_create(&bg[i], NULL, background, (void *)(uintptr_t)i) == 0, "background create");

    for (int round = 0; round < 30; round++) {
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork round %d", round);
        if (pid == 0)
            _exit(child_body(round));
        /* Parent allocates while the child runs. */
        CHECK(churn(0x9A4E + (uint64_t)round, 1), "parent churn round %d", round);
        int status = 0;
        CHECK(waitpid(pid, &status, 0) == pid, "waitpid round %d", round);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child round %d exited with status %#x (code %d)",
              round, status, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        CHECK(inherited_ok(), "parent blocks damaged after round %d", round);
    }

    /* Fork from a non-main thread as well, while the background threads run. */
    for (int i = 0; i < 5; i++) {
        pthread_t ft;
        int ok = 0;
        REQUIRE(pthread_create(&ft, NULL, forker, &ok) == 0, "forker create");
        pthread_join(ft, NULL);
        CHECK(ok, "fork from thread round %d", i);
    }
    stop = 1;
    for (int i = 0; i < BG; i++)
        pthread_join(bg[i], NULL);

    CHECK(inherited_ok(), "parent blocks intact at the end");
    for (int i = 0; i < INHERITED; i++)
        free(inh[i].p);
    CHECK(churn(0xF1A1, 2), "parent churn after all forks");
    return finish("fork_alloc");
}
