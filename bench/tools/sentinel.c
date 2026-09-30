/* sentinel: a fixed-work health probe for bench/cb (the start of every
 * block). Two parts, each timed on CLOCK_MONOTONIC:
 *   alu    a chain of dependent integer multiply-adds (tens of ms on a
 *          current core); moves with frequency and steal
 *   chase  a random single-cycle pointer chase over 64 MiB (Sattolo's
 *          shuffle, fixed seed); moves with memory latency and neighbours
 * Prints one JSON object: {"alu_ms":..,"chase_ms":..,"total_ms":..}. The
 * absolute numbers only mean something on one host; cb flags a block whose
 * sentinel is more than 3 MAD from the host's median. The memory comes from
 * mmap, not malloc, so the allocator under test (never preloaded here
 * anyway) cannot change it. */
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <time.h>

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

int main(void) {
    const size_t n = (64u << 20) / sizeof(uint32_t);
    uint32_t *a = mmap(NULL, n * sizeof *a, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    uint64_t s = 0x9e3779b97f4a7c15ull;
    for (size_t i = 0; i < n; i++) a[i] = (uint32_t)i;
    for (size_t i = n - 1; i > 0; i--) { /* Sattolo: one cycle through all */
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        size_t j = s % i;
        uint32_t t = a[i];
        a[i] = a[j];
        a[j] = t;
    }
    double t0 = now();
    volatile uint64_t x = 1;
    uint64_t y = x;
    for (uint32_t i = 0; i < 15000000u; i++) y = y * 6364136223846793005ull + i;
    x = y;
    double t1 = now();
    uint32_t p = 0;
    for (uint32_t i = 0; i < 150000u; i++) p = a[p];
    double t2 = now();
    printf("{\"alu_ms\":%.3f,\"chase_ms\":%.3f,\"total_ms\":%.3f,\"sink\":%u}\n",
           (t1 - t0) * 1e3, (t2 - t1) * 1e3, (t2 - t0) * 1e3, (unsigned)(p ^ (uint32_t)x));
    return 0;
}
