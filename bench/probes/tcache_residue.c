// tcache_residue.c: thread-cache residue (issue #27). Two modes.
//
//   tcache_residue residue T WORK_S
//     T threads. Phase 1: every thread touches 24 size classes once (4
//     blocks each, freed again): the first-use pull of each (thread,
//     class) cache. Phase 2: every thread runs WORK_S seconds of a
//     slow-path-heavy loop on one class (2500 blocks allocated, then
//     freed), so its other 23 caches are quiet across several cache GC
//     intervals. Then the threads park (alive) and the RSS is read.
//   tcache_residue burst T CYCLES BURST_MS IDLE_MS
//     T threads alternate a BURST_MS burst over 8 classes (300 blocks per
//     class allocated, then freed, repeatedly) with an IDLE_MS idle, the
//     server thread pool and the idle worker; the RSS is read at the end
//     of every idle.
//
// Every RSS reading is taken after `epoch` and `arena.4096.purge` where the
// allocator has mallctl (weak), so it counts cached and live memory, not
// unpurged empty pages. Output, one line:
//   result rss_first_kb=.. rss_settle_kb=.. rss_join_kb=.. hwm_kb=.. hot_mops=..
//   result rss_idle_kb=.. rss_end_kb=.. hwm_kb=.. burst_mops=..
#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

static const size_t SIZES[24] = {16, 32, 48, 64, 80, 96, 128, 160, 192, 256, 320, 384,
                                 512, 640, 768, 1024, 1280, 1536, 2048, 3072, 4096, 6144, 8192, 16384};
static const size_t BURST_SIZES[8] = {32, 64, 128, 256, 512, 1024, 2048, 4096};

static pthread_barrier_t bar;
static double work_s, burst_s;
static int cycles;
static volatile int stop;
static uint64_t *ops;

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static long status_kb(const char *key) {
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    long v = -1;
    size_t n = strlen(key);
    if (!f) return -1;
    while (fgets(line, sizeof line, f))
        if (!strncmp(line, key, n)) { v = strtol(line + n, NULL, 10); break; }
    fclose(f);
    return v;
}

static long settle_kb(void) {
    if (mallctl) {
        uint64_t e = 1;
        size_t l = sizeof e;
        mallctl("epoch", &e, &l, &e, sizeof e);
        mallctl("arena.4096.purge", NULL, NULL, NULL, 0);
    }
    usleep(200000);
    return status_kb("VmRSS:");
}

static void touch_free(size_t size, int n) {
    void *p[8];
    for (int i = 0; i < n; i++) { p[i] = malloc(size); memset(p[i], 1, 16); }
    for (int i = 0; i < n; i++) free(p[i]);
}

static void *residue_thread(void *arg) {
    uintptr_t id = (uintptr_t)arg;
    for (int c = 0; c < 24; c++) touch_free(SIZES[c], 4);
    pthread_barrier_wait(&bar); // phase 1 done
    pthread_barrier_wait(&bar); // main read rss_first
    size_t hot = SIZES[1 + id % 4];
    enum { N = 2500 };
    void **p = malloc(N * sizeof *p);
    uint64_t n = 0;
    double end = now() + work_s;
    while (now() < end) {
        for (int i = 0; i < N; i++) { p[i] = malloc(hot); *(char *)p[i] = 1; }
        for (int i = 0; i < N; i++) free(p[i]);
        n += 2 * N;
    }
    free(p);
    ops[id] = n;
    pthread_barrier_wait(&bar); // phase 2 done
    pthread_barrier_wait(&bar); // main read rss_settle
    return NULL;
}

static void *burst_thread(void *arg) {
    uintptr_t id = (uintptr_t)arg;
    enum { PER = 300 };
    void **p = malloc(8 * PER * sizeof *p);
    uint64_t n = 0;
    for (int c = 0; c < cycles; c++) {
        pthread_barrier_wait(&bar); // burst starts
        double end = now() + burst_s;
        while (now() < end) {
            for (int k = 0; k < 8; k++)
                for (int i = 0; i < PER; i++) { p[k * PER + i] = malloc(BURST_SIZES[k]); *(char *)p[k * PER + i] = 1; }
            for (int i = 8 * PER - 1; i >= 0; i--) free(p[i]);
            n += 2 * 8 * PER;
        }
        pthread_barrier_wait(&bar); // burst done, idle
    }
    ops[id] = n;
    pthread_barrier_wait(&bar); // main read the last idle rss
    free(p);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: tcache_residue residue T WORK_S | burst T CYCLES BURST_MS IDLE_MS\n"); return 2; }
    int burst = !strcmp(argv[1], "burst");
    int t = atoi(argv[2]);
    if (t < 1 || t > 4096) { fprintf(stderr, "bad thread count\n"); return 2; }
    pthread_t *th = calloc(t, sizeof *th);
    ops = calloc(t, sizeof *ops);
    pthread_barrier_init(&bar, NULL, t + 1);
    pthread_attr_t at;
    pthread_attr_init(&at);
    pthread_attr_setstacksize(&at, 256 * 1024);
    if (!burst) {
        work_s = argc > 3 ? atof(argv[3]) : 32;
        for (int i = 0; i < t; i++) pthread_create(&th[i], &at, residue_thread, (void *)(uintptr_t)i);
        pthread_barrier_wait(&bar);
        long first = settle_kb();
        double t0 = now();
        pthread_barrier_wait(&bar);
        pthread_barrier_wait(&bar);
        double el = now() - t0;
        long settle = settle_kb();
        pthread_barrier_wait(&bar);
        for (int i = 0; i < t; i++) pthread_join(th[i], NULL);
        long join = settle_kb();
        uint64_t n = 0;
        for (int i = 0; i < t; i++) n += ops[i];
        printf("result rss_first_kb=%ld rss_settle_kb=%ld rss_join_kb=%ld hwm_kb=%ld hot_mops=%.3f\n", first, settle, join,
               status_kb("VmHWM:"), n / el / 1e6);
    } else {
        cycles = argc > 3 ? atoi(argv[3]) : 3;
        burst_s = (argc > 4 ? atof(argv[4]) : 2000) / 1000;
        double idle_s = (argc > 5 ? atof(argv[5]) : 12000) / 1000;
        for (int i = 0; i < t; i++) pthread_create(&th[i], &at, burst_thread, (void *)(uintptr_t)i);
        long idle_max = 0, last = 0;
        double busy = 0;
        for (int c = 0; c < cycles; c++) {
            double b0 = now();
            pthread_barrier_wait(&bar);
            pthread_barrier_wait(&bar);
            busy += now() - b0;
            usleep((useconds_t)((idle_s - 0.2) * 1e6));
            last = settle_kb();
            if (last > idle_max) idle_max = last;
        }
        pthread_barrier_wait(&bar);
        for (int i = 0; i < t; i++) pthread_join(th[i], NULL);
        uint64_t n = 0;
        for (int i = 0; i < t; i++) n += ops[i];
        printf("result rss_idle_kb=%ld rss_end_kb=%ld hwm_kb=%ld burst_mops=%.3f\n", idle_max, last, status_kb("VmHWM:"), n / busy / 1e6);
    }
    return 0;
}
