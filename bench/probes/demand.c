// demand.c: the demand-window shapes of issue #25 (design 8). Three modes.
//
//   demand churn STEPS PERIOD_MS NBUF CAP_MIB SETTLE_MS
//     The ML / inference step: NBUF buffers with sizes drawn once,
//     log-uniform 256 KiB..128 MiB (fixed seed, total at most CAP_MIB),
//     allocated, written one byte per 4 KiB, and freed, once per
//     PERIOD_MS for STEPS steps; the same sizes every step. At least one
//     buffer is above 64 MiB (huge_retain_max).
//   demand oneoff N GAP_MS SETTLE_MS
//     N one-off buffers of distinct sizes, 64..256 MiB, each allocated,
//     written and freed, GAP_MS apart; never reused.
//   demand lull T CYCLES BURST_MS IDLE_MS
//     A sidecar: T threads, CYCLES bursts of BURST_MS of mixed small
//     allocations (16..4096 bytes, log-uniform, 16384 live slots per
//     thread, random replacement), everything freed at the end of the
//     burst, then IDLE_MS idle; rss_settle is read at the end of the last
//     idle, with the threads alive.
//
// The background thread is turned on through mallctl where the allocator
// has it (weak), so decay runs while the process idles. A sampler thread
// reads /proc/self/statm every 10 ms (no allocation) for the time-weighted
// mean RSS. After the load, SETTLE_MS of idle, then the RSS (no forced
// purge). One output line:
//   result busy_s=.. minflt_per_op=.. minflt1_per_op=.. (plus one: a log-scale contrast) rss_mean_kb=.. rss_settle_kb=.. hwm_kb=.. [mops=..]
#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void sleep_s(double s) {
    if (s <= 0) return;
    struct timespec t = {(time_t)s, (long)((s - (time_t)s) * 1e9)};
    while (nanosleep(&t, &t) != 0) {}
}

static long page_kb;

static long rss_kb(void) {
    char buf[128];
    int fd = open("/proc/self/statm", O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return -1;
    buf[n] = 0;
    char *p = strchr(buf, ' ');
    return p ? strtol(p + 1, NULL, 10) * page_kb : -1;
}

static long hwm_kb(void) {
    char buf[4096];
    int fd = open("/proc/self/status", O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return -1;
    buf[n] = 0;
    char *p = strstr(buf, "VmHWM:");
    return p ? strtol(p + 6, NULL, 10) : -1;
}

static long minflt(void) {
    struct rusage r;
    getrusage(RUSAGE_SELF, &r);
    return r.ru_minflt;
}

static volatile int sampling = 1;
static double rss_area, rss_t;

static void *sampler(void *arg) {
    (void)arg;
    double last = now();
    long last_rss = rss_kb();
    while (sampling) {
        sleep_s(0.01);
        double t = now();
        long r = rss_kb();
        rss_area += (double)(last_rss + r) / 2 * (t - last);
        rss_t += t - last;
        last = t;
        last_rss = r;
    }
    return NULL;
}

static uint64_t rng = 0x9e3779b97f4a7c15ull;
static uint64_t next(uint64_t *s) {
    uint64_t x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    return *s = x;
}
static double unit(uint64_t *s) { return (next(s) >> 11) * (1.0 / 9007199254740992.0); }
// lo * 2^(u * octaves), u uniform in [0, 1): log-uniform without libm.
static size_t loguni(uint64_t *s, double lo, int octaves) {
    double e = unit(s) * octaves;
    int i = (int)e;
    double f = e - i;
    return (size_t)(lo * (double)(1ull << i) * (1 + f * (0.6931472 + f * (0.2402265 + f * 0.0555041))));
}

static double busy;
static long faults;
static double ops_total;

static void touch(char *p, size_t n) {
    for (size_t i = 0; i < n; i += 4096) p[i] = (char)i;
    p[n - 1] = 1;
}

static void churn(int steps, double period, int nbuf, double cap_mib) {
    size_t sizes[64];
    if (nbuf > 64) nbuf = 64;
    for (;;) {
        double tot = 0, big = 0;
        for (int i = 0; i < nbuf; i++) {
            tot += (double)(sizes[i] = loguni(&rng, 256.0 * 1024, 9));
            if (sizes[i] > big) big = (double)sizes[i];
        }
        if (tot <= cap_mib * 1024 * 1024 && big > 64.0 * 1024 * 1024) break;
    }
    void *bufs[64];
    double t_next = now();
    long f0 = minflt();
    for (int s = 0; s < steps; s++) {
        double t0 = now();
        for (int i = 0; i < nbuf; i++) {
            bufs[i] = malloc(sizes[i]);
            if (!bufs[i]) abort();
            touch(bufs[i], sizes[i]);
        }
        for (int i = 0; i < nbuf; i++) free(bufs[i]);
        busy += now() - t0;
        t_next += period;
        sleep_s(t_next - now());
    }
    faults = minflt() - f0;
    ops_total = steps;
}

static void oneoff(int n, double gap) {
    long f0 = minflt();
    for (int i = 0; i < n; i++) {
        // Distinct sizes: stratified over 64..256 MiB, shuffled by the stride.
        int k = (i * 5) % n;
        size_t sz = (size_t)((64.0 + (192.0 * (k + unit(&rng))) / n) * 1024 * 1024);
        double t0 = now();
        char *p = malloc(sz);
        if (!p) abort();
        touch(p, sz);
        free(p);
        busy += now() - t0;
        sleep_s(gap);
    }
    faults = minflt() - f0;
    ops_total = n;
}

#define SLOTS 16384
static pthread_barrier_t bar;
static int l_threads, l_cycles;
static double l_burst, l_idle;
static double l_ops[256];
static long l_end_kb = -1;

static void *lull_worker(void *arg) {
    long id = (long)arg;
    uint64_t s = 0x2545f4914f6cdd1dull * (uint64_t)(id + 1);
    void **slot = calloc(SLOTS, sizeof *slot);
    double ops = 0;
    for (int c = 0; c < l_cycles; c++) {
        pthread_barrier_wait(&bar);
        double t0 = now();
        for (int i = 0; i < SLOTS; i++) slot[i] = malloc(loguni(&s, 16, 8));
        ops += SLOTS;
        while (now() - t0 < l_burst) {
            for (int k = 0; k < 1024; k++) {
                size_t j = next(&s) % SLOTS;
                free(slot[j]);
                slot[j] = malloc(loguni(&s, 16, 8));
                if (!slot[j]) abort();
                ((char *)slot[j])[0] = 1;
            }
            ops += 1024;
        }
        for (int i = 0; i < SLOTS; i++) free(slot[i]);
        pthread_barrier_wait(&bar);
        pthread_barrier_wait(&bar);
    }
    free(slot);
    l_ops[id] = ops;
    return NULL;
}

static void lull(void) {
    pthread_t th[256];
    pthread_barrier_init(&bar, NULL, (unsigned)l_threads + 1);
    for (long i = 0; i < l_threads; i++) pthread_create(&th[i], NULL, lull_worker, (void *)i);
    long f0 = minflt();
    for (int c = 0; c < l_cycles; c++) {
        double t0 = now();
        pthread_barrier_wait(&bar);
        pthread_barrier_wait(&bar);
        busy += now() - t0;
        sleep_s(l_idle);
        if (c == l_cycles - 1) l_end_kb = rss_kb();
        pthread_barrier_wait(&bar);
    }
    for (int i = 0; i < l_threads; i++) pthread_join(th[i], NULL);
    faults = minflt() - f0;
    ops_total = l_cycles;
}

int main(int argc, char **argv) {
    page_kb = sysconf(_SC_PAGESIZE) / 1024;
    if (argc < 2) return 2;
    if (mallctl) {
        bool on = true;
        if (mallctl("background_thread", NULL, NULL, &on, sizeof on) != 0)
            fprintf(stderr, "demand: background_thread did not turn on\n");
    }
    pthread_t smp;
    pthread_create(&smp, NULL, sampler, NULL);
    double settle = 0, mops = -1;
    if (!strcmp(argv[1], "churn") && argc == 7) {
        churn(atoi(argv[2]), atof(argv[3]) / 1000, atoi(argv[4]), atof(argv[5]));
        settle = atof(argv[6]) / 1000;
    } else if (!strcmp(argv[1], "oneoff") && argc == 5) {
        oneoff(atoi(argv[2]), atof(argv[3]) / 1000);
        settle = atof(argv[4]) / 1000;
    } else if (!strcmp(argv[1], "lull") && argc == 6) {
        l_threads = atoi(argv[2]);
        l_cycles = atoi(argv[3]);
        l_burst = atof(argv[4]) / 1000;
        l_idle = atof(argv[5]) / 1000;
        if (l_threads < 1 || l_threads > 256 || l_cycles < 1) return 2;
        lull();
        double o = 0;
        for (int i = 0; i < l_threads; i++) o += l_ops[i];
        mops = o / busy / 1e6;
    } else {
        fprintf(stderr, "usage: demand churn STEPS PERIOD_MS NBUF CAP_MIB SETTLE_MS | oneoff N GAP_MS SETTLE_MS | lull T CYCLES BURST_MS IDLE_MS\n");
        return 2;
    }
    sleep_s(settle);
    sampling = 0;
    pthread_join(smp, NULL);
    long rs = l_end_kb >= 0 ? l_end_kb : rss_kb();
    char line[256];
    int n = snprintf(line, sizeof line, "result busy_s=%.6f minflt_per_op=%.1f minflt1_per_op=%.1f rss_mean_kb=%.0f rss_settle_kb=%ld hwm_kb=%ld",
                     busy, faults / ops_total, faults / ops_total + 1, rss_t > 0 ? rss_area / rss_t : -1.0, rs, hwm_kb());
    if (mops >= 0) n += snprintf(line + n, sizeof line - (size_t)n, " mops=%.4f", mops);
    printf("%s\n", line);
    return 0;
}
