// churn.c: model of the Valkey post-delete steady state for one size class.
// N blocks of S bytes are allocated (100% occupancy), a random 56% are freed
// (44% occupancy, the soak's residue), then balanced random churn runs for
// T seconds: pick a random live block, free it, allocate a new one.
// Every second print the bin's curslabs/curregs through mallctl so the
// drain (or not) of the slab layout is visible. Works under challoc and
// jemalloc (weak mallctl).
//
//   churn S N T FRAC PIN     (defaults 1024 30000 20 0.56 0)
//
// PIN (Phase 2.5 task 4g) is the fraction of the survivors that the churn
// never frees: values whose keys the soak's load generators never select
// again (bench/results/2026-09-17-aarch64-gce-compacting-cache-soak3.md).
// They are picked at random over the survivors, so they are scattered over
// the address range like the soak's; the probe prints how many distinct
// 64 KiB slabs hold one (the floor no cache policy can go below: a slab
// with a pinned block cannot empty). PIN 0 is the quick tier's row.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));
static uint64_t rs = 0x9e3779b97f4a7c15ull;
static uint64_t rnd(void) { rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17; return rs; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }
static unsigned find_bin(size_t s) {
    unsigned nb; size_t l = sizeof nb; mallctl("arenas.nbins", &nb, &l, NULL, 0);
    for (unsigned j = 0; j < nb; j++) { char k[64]; size_t sz; l = sizeof sz; snprintf(k, sizeof k, "arenas.bin.%u.size", j); mallctl(k, &sz, &l, NULL, 0); if (sz == s) return j; }
    return 0;
}
static void report(unsigned j, const char *tag, double t) {
    uint64_t e = 1; size_t l = sizeof e; mallctl("epoch", &e, &l, &e, sizeof e);
    char k[64]; size_t curslabs = 0, curregs = 0, nonfull = 0; l = sizeof curslabs;
    snprintf(k, sizeof k, "stats.arenas.0.bins.%u.curslabs", j); mallctl(k, &curslabs, &l, NULL, 0);
    snprintf(k, sizeof k, "stats.arenas.0.bins.%u.curregs", j); mallctl(k, &curregs, &l, NULL, 0);
    snprintf(k, sizeof k, "stats.arenas.0.bins.%u.nonfull_slabs", j); mallctl(k, &nonfull, &l, NULL, 0);
    size_t resident = 0, active = 0, allocated = 0; l = sizeof resident;
    mallctl("stats.resident", &resident, &l, NULL, 0); mallctl("stats.active", &active, &l, NULL, 0); mallctl("stats.allocated", &allocated, &l, NULL, 0);
    unsigned nregs; l = sizeof nregs; snprintf(k, sizeof k, "arenas.bin.%u.nregs", j); mallctl(k, &nregs, &l, NULL, 0);
    printf("%6.1f %-8s curslabs %6zu curregs %7zu nonfull %6zu occ %5.1f%%  active %6.1f MiB resident %6.1f MiB\n", t, tag, curslabs, curregs, nonfull,
           curslabs ? 100.0 * curregs / ((double)curslabs * nregs) : 0, active / 1048576.0, resident / 1048576.0);
    fflush(stdout);
}
int main(int argc, char **argv) {
    size_t S = argc > 1 ? strtoul(argv[1], 0, 0) : 1024;
    size_t N = argc > 2 ? strtoul(argv[2], 0, 0) : 30000;
    double T = argc > 3 ? atof(argv[3]) : 20;
    double frac = argc > 4 ? atof(argv[4]) : 0.56;
    double pin = argc > 5 ? atof(argv[5]) : 0;
    if (!mallctl) { fprintf(stderr, "no mallctl\n"); return 1; }
    unsigned j = find_bin(S);
    char **live = calloc(N, sizeof *live);
    for (size_t i = 0; i < N; i++) { live[i] = malloc(S); live[i][0] = 1; }
    double t0 = now();
    report(j, "full", 0);
    // random 56% deletion, compact the live array
    size_t n = 0;
    for (size_t i = 0; i < N; i++) { if ((rnd() % 1000) < frac * 1000) free(live[i]); else live[n++] = live[i]; }
    report(j, "deleted", now() - t0);
    sleep(1); report(j, "deleted", now() - t0);
    // pinned survivors: move a random PIN fraction to the front of the live
    // array (partial Fisher-Yates) and count the 64 KiB slabs they hold
    size_t pinned = (size_t)(pin * n);
    if (pinned > n) pinned = n;
    for (size_t i = 0; i < pinned; i++) { size_t k = i + rnd() % (n - i); char *t = live[i]; live[i] = live[k]; live[k] = t; }
    if (pinned) {
        uintptr_t *sl = calloc(pinned, sizeof *sl); size_t ns = 0;
        for (size_t i = 0; i < pinned; i++) sl[i] = (uintptr_t)live[i] >> 16;
        for (size_t i = 0; i < pinned; i++) { size_t k; for (k = 0; k < ns; k++) if (sl[k] == sl[i]) break; if (k == ns) sl[ns++] = sl[i]; }
        printf("pinned %zu of %zu survivors in %zu distinct 64 KiB slabs (the floor)\n", pinned, n, ns);
        free(sl);
    }
    if (pinned >= n) { fprintf(stderr, "nothing to churn\n"); return 1; }
    // balanced churn: free a random live block that is not pinned, allocate a replacement
    double next = now() - t0 + 1; unsigned long ops = 0;
    while (now() - t0 < T + 2) {
        size_t i = pinned + rnd() % (n - pinned); free(live[i]); live[i] = malloc(S); live[i][0] = 1; ops++;
        if ((ops & 1023) == 0 && now() - t0 >= next) { report(j, "churn", now() - t0); next += 1; }
    }
    printf("ops %lu (%.0f/s)\n", ops, ops / T);
    report(j, "end", now() - t0);
    return 0;
}
