/*
 * Bounded random fuzz for the debug TLS check: T threads run random
 * malloc/calloc/realloc/free/posix_memalign/mallctl sequences, free each
 * other's blocks through a shared exchange, exit and are replaced at
 * random, and fork at random (the child runs a short random workload on
 * the forking thread and two threads of its own, then exits). Under a
 * check build every slot-to-heap use is checked; a broken link aborts.
 *
 *   fuzz SECONDS SEED [THREADS]
 *
 * The seed fixes each thread's operation sequence, not the interleaving
 * of the threads, so a rerun with it repeats the workload, not the run.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <malloc.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* Resolved from the preloaded library in main, so the program links bare. */
static int (*mallctl_p)(const char *, void *, size_t *, void *, size_t);
#define mallctl(...) (mallctl_p ? mallctl_p(__VA_ARGS__) : -1)
static void *(*mallocx_p)(size_t, int);
static void (*dallocx_p)(void *, int);

#define XN 8192
static void *_Atomic xch[XN];
static atomic_int stop;
static atomic_long ops, forks, spawned, child_fail, running;
static int nthreads;

static uint64_t rng(uint64_t *s)
{
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    return *s;
}

static size_t size_of(uint64_t v)
{
    switch (v % 16) {
    case 0:
        return (v >> 8) % 600000 + 1;
    case 1:
    case 2:
        return (v >> 8) % 32768 + 1;
    default:
        return (v >> 8) % 1024 + 1;
    }
}

static void step(uint64_t *s, void **mine, int nm)
{
    uint64_t v = rng(s);
    int k = (v >> 4) % nm;
    switch (v % 12) {
    case 0:
    case 1:
    case 2:
        free(mine[k]);
        mine[k] = malloc(size_of(rng(s)));
        break;
    case 3:
        free(mine[k]);
        mine[k] = calloc(1, size_of(rng(s)));
        break;
    case 4:
    case 5: {
        void *q = realloc(mine[k], size_of(rng(s)));
        if (q)
            mine[k] = q;
        break;
    }
    case 6: {
        free(mine[k]);
        void *q = NULL;
        mine[k] = posix_memalign(&q, (size_t)1 << ((v >> 8) % 13 + 3), size_of(rng(s))) == 0 ? q : NULL;
        break;
    }
    case 7:
    case 8: { /* hand a block to whoever comes next, take theirs */
        int x = (v >> 20) % XN;
        void *old = atomic_exchange(&xch[x], mine[k]);
        mine[k] = old;
        break;
    }
    case 9:
        free(mine[k]);
        mine[k] = NULL;
        break;
    case 10:
        switch ((v >> 8) % 6) {
        case 0:
            mallctl("thread.tcache.flush", NULL, NULL, NULL, 0);
            break;
        case 1: {
            uint64_t e = 1;
            size_t l = sizeof e;
            mallctl("epoch", &e, &l, &e, sizeof e);
            break;
        }
        case 2: {
            unsigned id;
            size_t l = sizeof id;
            if (mallctl("tcache.create", &id, &l, NULL, 0) == 0) {
                /* MALLOCX_TCACHE(id): the explicit-cache path (heap_at). */
                int fl = (int)((id + 2) << 8);
                if (mallocx_p && dallocx_p)
                    for (int j = 0; j < 16; j++)
                        dallocx_p(mallocx_p(size_of(rng(s)), fl), fl);
                mallctl("tcache.destroy", NULL, NULL, &id, sizeof id);
            }
            break;
        }
        case 3: {
            uint64_t a;
            size_t l = sizeof a;
            mallctl("thread.allocated", &a, &l, NULL, 0);
            break;
        }
        case 4:
            mallctl("arena.0.purge", NULL, NULL, NULL, 0);
            break;
        default: {
            size_t a;
            size_t l = sizeof a;
            mallctl("stats.allocated", &a, &l, NULL, 0);
            break;
        }
        }
        break;
    default:
        if (mine[k])
            memset(mine[k], (int)v, 8 < malloc_usable_size(mine[k]) ? 8 : malloc_usable_size(mine[k]));
        break;
    }
    atomic_fetch_add(&ops, 1);
}

static void *child_thread(void *arg)
{
    uint64_t s = (uintptr_t)arg | 1;
    void *m[32] = {0};
    for (int i = 0; i < 3000; i++)
        step(&s, m, 32);
    for (int i = 0; i < 32; i++)
        free(m[i]);
    return NULL;
}

static void in_child(uint64_t s)
{
    void *m[32] = {0};
    for (int i = 0; i < 2000; i++)
        step(&s, m, 32);
    pthread_t t[2];
    for (int i = 0; i < 2; i++)
        pthread_create(&t[i], NULL, child_thread, (void *)(uintptr_t)(s + i));
    for (int i = 0; i < 32; i++)
        free(m[i]);
    for (int i = 0; i < 2; i++)
        pthread_join(t[i], NULL);
    _exit(0);
}

static void *worker(void *arg);

static void spawn(uint64_t seed)
{
    pthread_t t;
    atomic_fetch_add(&spawned, 1);
    atomic_fetch_add(&running, 1);
    if (pthread_create(&t, NULL, worker, (void *)(uintptr_t)seed) == 0)
        pthread_detach(t);
    else
        atomic_fetch_sub(&running, 1);
}

static void *worker(void *arg)
{
    uint64_t s = (uintptr_t)arg | 1;
    void *m[64] = {0};
    long life = rng(&s) % 50000 + 100;
    for (long i = 0; i < life && !atomic_load(&stop); i++) {
        step(&s, m, 64);
        if (rng(&s) % 20000 == 0) {
            pid_t c = fork();
            if (c == 0)
                in_child(s);
            if (c > 0) {
                int st;
                waitpid(c, &st, 0);
                atomic_fetch_add(&forks, 1);
                if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
                    atomic_fetch_add(&child_fail, 1);
                    fprintf(stderr, "FAIL: fork child status %#x (signal %d)\n", st, WIFSIGNALED(st) ? WTERMSIG(st) : 0);
                }
            }
        }
    }
    /* Leave half the blocks for another thread to free after this exits. */
    for (int i = 0; i < 64; i++) {
        if (i % 2)
            free(atomic_exchange(&xch[rng(&s) % XN], m[i]));
        else
            free(m[i]);
    }
    if (!atomic_load(&stop))
        spawn(rng(&s));
    atomic_fetch_sub(&running, 1);
    return NULL;
}

int main(int argc, char **argv)
{
    int secs = argc > 1 ? atoi(argv[1]) : 10;
    uint64_t seed = argc > 2 ? strtoull(argv[2], NULL, 0) : 1;
    nthreads = argc > 3 ? atoi(argv[3]) : 16;
    mallctl_p = (int (*)(const char *, void *, size_t *, void *, size_t))dlsym(RTLD_DEFAULT, "mallctl");
    mallocx_p = (void *(*)(size_t, int))dlsym(RTLD_DEFAULT, "mallocx");
    dallocx_p = (void (*)(void *, int))dlsym(RTLD_DEFAULT, "dallocx");
    if (!mallctl_p) {
        fprintf(stderr, "FATAL: mallctl missing (challoc not preloaded?)\n");
        return 3;
    }
    for (int i = 0; i < nthreads; i++)
        spawn(seed * 1000003 + (uint64_t)i);
    sleep((unsigned)secs);
    atomic_store(&stop, 1);
    /* Every worker has reaped its fork child, so child_fail is final. */
    for (int i = 0; i < 600 && atomic_load(&running) > 0; i++)
        usleep(100000);
    if (atomic_load(&running) > 0) {
        fprintf(stderr, "FAIL: %ld workers still running 60 s after the stop\n", atomic_load(&running));
        atomic_fetch_add(&child_fail, 1);
    }
    void (*f)(size_t *) = (void (*)(size_t *))dlsym(RTLD_DEFAULT, "challoc_tls_check_stats");
    size_t st[5] = {0};
    if (f)
        f(st);
    printf("fuzz seed=%llu secs=%d ops=%ld threads_spawned=%ld forks=%ld child_fail=%ld uses=%zu published=%zu exited=%zu rehomed_parent=%zu deep=%zu\n",
           (unsigned long long)seed, secs, atomic_load(&ops), atomic_load(&spawned), atomic_load(&forks),
           atomic_load(&child_fail), st[0], st[1], st[2], st[3], st[4]);
    /* The check ran, heaps were published and the fork path was taken. */
    int rc = atomic_load(&child_fail) != 0 || !f || st[0] == 0 || st[1] == 0 || atomic_load(&forks) == 0;
    printf("%s fuzz\n", rc ? "FAIL" : "ok");
    fflush(stdout);
    _exit(rc);
}
