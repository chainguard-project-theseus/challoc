/*
 * Lifecycle tests for the debug TLS check (cfg(challoc_tls_check)): run
 * under LD_PRELOAD of a check build. Every slot-to-heap use is checked by
 * the library; a broken link aborts the process (SIGABRT). This program
 * drives the lifecycles that could break it and reads the check's counters
 * (challoc_tls_check_stats) to show the check ran.
 *
 *   lifecycle fork     fork from a thread while 8 others allocate; the
 *                      child allocates on the forking thread, spawns
 *                      threads, forks a grandchild
 *   lifecycle exitrace producers allocate, hand blocks to a consumer and
 *                      exit at once, racing its frees
 *   lifecycle churn    20000 short-lived threads in waves of 64
 *   lifecycle dlopen   dlopen/dlclose a library that allocates in its
 *                      constructor, destructor and a thread, 200 times
 *   lifecycle keyafter a pthread key whose destructor allocates after
 *                      challoc's destructor ran
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static void stats(const char *tag)
{
    void (*f)(size_t *) = (void (*)(size_t *))dlsym(RTLD_DEFAULT, "challoc_tls_check_stats");
    if (!f) {
        fprintf(stderr, "FATAL: challoc_tls_check_stats missing (not a check build?)\n");
        exit(3);
    }
    size_t s[5];
    f(s);
    printf("%s: uses=%zu published=%zu exited=%zu rehomed=%zu deep=%zu\n", tag, s[0], s[1], s[2], s[3], s[4]);
    fflush(stdout);
}

static size_t stat_at(int i)
{
    void (*f)(size_t *) = (void (*)(size_t *))dlsym(RTLD_DEFAULT, "challoc_tls_check_stats");
    size_t s[5];
    if (!f) {
        fprintf(stderr, "FATAL: challoc_tls_check_stats missing (not a check build?)\n");
        exit(3);
    }
    f(s);
    return s[i];
}

static uint64_t rng(uint64_t *s)
{
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    return *s;
}

static void churn(uint64_t seed, int n)
{
    void *p[64] = {0};
    for (int i = 0; i < n; i++) {
        uint64_t v = rng(&seed);
        int k = v % 64;
        free(p[k]);
        size_t sz = (v >> 8) % 4 == 0 ? (v >> 16) % 300000 : (v >> 16) % 2048 + 1;
        p[k] = (v >> 40) % 3 ? malloc(sz) : calloc(1, sz);
        if (p[k])
            memset(p[k], (int)k, sz < 64 ? sz : 64);
    }
    for (int k = 0; k < 64; k++)
        free(p[k]);
}

/* ---- fork ---- */
static atomic_int stop;
static void *busy(void *arg)
{
    uint64_t s = (uintptr_t)arg * 77 + 1;
    while (!atomic_load(&stop))
        churn(s++, 200);
    return NULL;
}
static void *child_worker(void *arg)
{
    churn((uintptr_t)arg + 5, 2000);
    return NULL;
}
static int child_body(int depth, size_t r0)
{
    churn(99 + depth, 5000);
    pthread_t t[4];
    for (int i = 0; i < 4; i++)
        pthread_create(&t[i], NULL, child_worker, (void *)(uintptr_t)i);
    for (int i = 0; i < 4; i++)
        pthread_join(t[i], NULL);
    if (stat_at(3) <= r0) { /* re-homed in this process, not inherited */
        fprintf(stderr, "FAIL: child at depth %d never re-homed its heap\n", depth);
        return 1;
    }
    if (depth < 2) {
        size_t r1 = stat_at(3);
        pid_t g = fork();
        if (g == 0)
            _exit(child_body(depth + 1, r1));
        int st;
        waitpid(g, &st, 0);
        if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
            fprintf(stderr, "FAIL: grandchild status %#x\n", st);
            return 1;
        }
    }
    return 0;
}
static void *forker(void *arg)
{
    (void)arg;
    int bad = 0;
    for (int r = 0; r < 50; r++) {
        churn(1000 + r, 300); /* this thread is mid-workload */
        void *held = malloc(1000);
        size_t r0 = stat_at(3);
        pid_t c = fork();
        if (c == 0)
            _exit(child_body(0, r0));
        free(held);
        int st;
        waitpid(c, &st, 0);
        if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
            fprintf(stderr, "FAIL: fork round %d child status %#x\n", r, st);
            bad = 1;
        }
    }
    return (void *)(uintptr_t)bad;
}
static int t_fork(void)
{
    pthread_t b[8], f;
    for (int i = 0; i < 8; i++)
        pthread_create(&b[i], NULL, busy, (void *)(uintptr_t)i);
    void *r;
    pthread_create(&f, NULL, forker, NULL);
    pthread_join(f, &r);
    atomic_store(&stop, 1);
    for (int i = 0; i < 8; i++)
        pthread_join(b[i], NULL);
    return (int)(uintptr_t)r;
}

/* ---- exitrace ---- */
#define QN 4096
static void *_Atomic q[QN];
static atomic_int producers_left;
static void *producer(void *arg)
{
    uint64_t s = (uintptr_t)arg + 3;
    for (int i = 0; i < 64; i++) {
        size_t sz = rng(&s) % 5000 + 1;
        void *p = malloc(sz);
        int k = rng(&s) % QN;
        void *old = atomic_exchange(&q[k], p);
        free(old); /* a free of another thread's block, maybe of an exited one */
    }
    atomic_fetch_sub(&producers_left, 1);
    return NULL; /* exits at once: its heap's destructor races the consumer's frees */
}
static void *consumer(void *arg)
{
    (void)arg;
    uint64_t s = 11;
    while (atomic_load(&producers_left) > 0) {
        int k = rng(&s) % QN;
        free(atomic_exchange(&q[k], NULL));
    }
    return NULL;
}
static int t_exitrace(void)
{
    for (int round = 0; round < 50; round++) {
        atomic_store(&producers_left, 128);
        pthread_t c[2];
        for (int i = 0; i < 2; i++)
            pthread_create(&c[i], NULL, consumer, NULL);
        pthread_t p[128];
        for (int i = 0; i < 128; i++) {
            pthread_create(&p[i], NULL, producer, (void *)(uintptr_t)(round * 1000 + i));
            pthread_detach(p[i]);
        }
        for (int i = 0; i < 2; i++)
            pthread_join(c[i], NULL);
    }
    for (int k = 0; k < QN; k++)
        free(atomic_exchange(&q[k], NULL));
    return 0;
}

/* ---- churn ---- */
static void *short_lived(void *arg)
{
    /* Enough calls that the thread gets a heap (the first few are served
     * without one). */
    churn((uintptr_t)arg + 1, 200);
    return NULL;
}
static int t_churn(void)
{
    size_t pub0 = stat_at(1);
    for (int w = 0; w < 20000 / 64; w++) {
        pthread_t t[64];
        for (int i = 0; i < 64; i++)
            pthread_create(&t[i], NULL, short_lived, (void *)(uintptr_t)(w * 64 + i));
        for (int i = 0; i < 64; i++)
            pthread_join(t[i], NULL);
    }
    size_t pub = stat_at(1) - pub0;
    if (pub < 20000 / 64 * 64) {
        fprintf(stderr, "FAIL: %zu heaps published for %d threads\n", pub, 20000 / 64 * 64);
        return 1;
    }
    return 0;
}

/* ---- dlopen ---- */
static int t_dlopen(const char *lib)
{
    for (int i = 0; i < 200; i++) {
        void *h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
        if (!h) {
            fprintf(stderr, "FAIL: dlopen %s: %s\n", lib, dlerror());
            return 1;
        }
        void (*run)(int) = (void (*)(int))dlsym(h, "tlslib_run");
        run(i);
        dlclose(h);
    }
    return 0;
}

/* ---- keyafter ---- */
static pthread_key_t late;
static void late_dtor(void *v)
{
    free(v);
    void *p = malloc(100); /* after challoc's destructor: served without a heap */
    free(p);
}
static void *keyafter_thread(void *arg)
{
    (void)arg;
    pthread_setspecific(late, malloc(10));
    churn(5, 100);
    return NULL;
}
static int t_keyafter(void)
{
    free(malloc(1)); /* challoc's key exists before ours */
    pthread_key_create(&late, late_dtor);
    for (int i = 0; i < 2000; i++) {
        pthread_t t;
        pthread_create(&t, NULL, keyafter_thread, NULL);
        pthread_join(t, NULL);
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (argc < 2)
        return 2;
    free(malloc(1));
    stats("start");
    int rc;
    if (!strcmp(argv[1], "fork"))
        rc = t_fork();
    else if (!strcmp(argv[1], "exitrace"))
        rc = t_exitrace();
    else if (!strcmp(argv[1], "churn"))
        rc = t_churn();
    else if (!strcmp(argv[1], "dlopen"))
        rc = t_dlopen(argc > 2 ? argv[2] : "./libtlslib.so");
    else if (!strcmp(argv[1], "keyafter"))
        rc = t_keyafter();
    else
        return 2;
    stats(argv[1]);
    if (stat_at(0) == 0) {
        fprintf(stderr, "FAIL: the check saw no use\n");
        rc = 1;
    }
    printf("%s %s\n", rc ? "FAIL" : "ok", argv[1]);
    return rc;
}
