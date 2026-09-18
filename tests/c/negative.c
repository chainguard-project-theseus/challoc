/*
 * Phase 0 task 14: negative tests. Each mode performs one invalid operation
 * that challoc must refuse by aborting (or that the guard page must turn
 * into a fault). The driver in src/api/ctests.rs runs one mode per process
 * and checks the signal and the `challoc: ...` line on stderr. Reaching the
 * end of a mode means the allocator let the bug through; the program then
 * exits 0, which the driver treats as a failure.
 *
 *   negative double_free_stale    free(p) twice with the thread cache flushed
 *                                 in between: caught by the slot bit on free
 *   negative double_free_cached   free(p) twice back to back: caught when the
 *                                 cache flushes the duplicates to the slab
 *   negative sized_double_free    sdallocx(p, size, 0) twice back to back: the
 *                                 sized path skips the slot bit, so the call
 *                                 returns and the flush catches the duplicate
 *   negative sized_mismatch       sdallocx(p, wrong size, 0): the hint's class
 *                                 is not the pointer's, caught at the call
 *   negative free_sized_mismatch  C23 free_sized(p, wrong size), the same
 *   negative usable_size_freed    malloc_usable_size(p) after free(p) and a
 *                                 flush: the query keeps the exact bit test
 *                                 (plan Phase 1.5 task 14)
 *   negative interior_free        free(p + size/2)
 *   negative stack_free           free of a stack address
 *   negative huge_overflow        write one byte past a huge allocation
 *   negative double_free_straddle free(p) twice, then exactly enough other
 *                                 frees that the flush's sorted batch has one
 *                                 copy at the split point and the other just
 *                                 below it: without the boundary compare one
 *                                 copy is released and the other stays cached
 *                                 (plan Phase 1.5 task 9a)
 *   negative sized_guard          sdallocx of a slot start inside a guard
 *                                 slab, then malloc: the sized path must
 *                                 reject the guard slab, not cache it and hand
 *                                 the guard memory out (task 9b)
 */
#include "common.h"
#include <stdbool.h>

/* From the preloaded allocator; weak so the binary links without it (the
 * driver only runs these modes with libchalloc loaded). */
extern void sdallocx(void *, size_t, int) __attribute__((weak));
extern void free_sized(void *, size_t) __attribute__((weak));
extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

/* Enough blocks of one class that freeing them all flushes the thread cache
 * several times (the largest cache holds 1024 slots and flushes 512). They
 * are allocated before the victim so a later malloc never pops it. */
#define HOLD 4096
static void *volatile hold[HOLD];

static void *victim(size_t size)
{
    for (int i = 0; i < HOLD; i++) {
        hold[i] = malloc(size);
        REQUIRE(hold[i] != NULL, "malloc(%zu) failed", size);
    }
    void *p = malloc(size);
    REQUIRE(p != NULL, "malloc(%zu) failed", size);
    memset(p, 0xA5, size);
    return p;
}

static void drain(void)
{
    for (int i = 0; i < HOLD; i++) {
        free(hold[i]);
        hold[i] = NULL;
    }
}

static unsigned ctl_uint(const char *name)
{
    unsigned v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl != NULL, "mallctl not exported");
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

static bool ctl_bool(const char *name)
{
    bool v = false;
    size_t len = sizeof v;
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

static ssize_t ctl_ssize(const char *name)
{
    ssize_t v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

static size_t ctl_size(const char *name)
{
    size_t v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

/* The thread cache's capacity for a class of at most 128 bytes
 * (`config::cap_for`: `tcache_nslots_small_max` shifted by `lg_tcache_nslots_mul
 * - 1` doublings and clamped to the max). */
static unsigned small_cache_cap(void)
{
    unsigned max = ctl_uint("opt.tcache_nslots_small_max");
    ssize_t lg_mul = ctl_ssize("opt.lg_tcache_nslots_mul");
    unsigned cap = lg_mul >= 1 ? max : max >> (1 - lg_mul);
    return cap < 8 ? 8 : cap;
}

static int cmp_ptr(const void *a, const void *b)
{
    uintptr_t x = (uintptr_t)*(void *const *)a, y = (uintptr_t)*(void *const *)b;
    return x < y ? -1 : x > y;
}

/* Bin index of the 64-byte class. */
static unsigned bin_of_64(void)
{
    unsigned nbins = ctl_uint("arenas.nbins");
    for (unsigned i = 0; i < nbins; i++) {
        char name[64];
        snprintf(name, sizeof name, "arenas.bin.%u.size", i);
        if (ctl_size(name) == 64)
            return i;
    }
    REQUIRE(0, "no 64-byte bin");
    return 0;
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: negative <mode>");
    require_challoc();
    const char *mode = argv[1];
    printf("negative: %s\n", mode);
    fflush(stdout);

    /* Every bad pointer goes through a volatile so gcc neither folds the
     * operation away nor rejects it at compile time (-Wfree-nonheap-object,
     * -Wuse-after-free are errors under -Werror). */
    void *volatile bad;

    if (strcmp(mode, "double_free_stale") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad);
        drain(); /* p leaves the cache and its slot bit clears */
        free(bad);
    } else if (strcmp(mode, "double_free_cached") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad);
        free(bad); /* both copies now sit in the thread cache */
        drain();   /* the flush releases them to the slab */
    } else if (strcmp(mode, "sized_double_free") == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        void *p = victim(64);
        bad = p;
        sdallocx(bad, 64, 0);
        sdallocx(bad, 64, 0); /* not caught here: no descriptor load on the sized path */
        printf("sized double free of %p returned\n", bad);
        fflush(stdout);
        drain(); /* the flush releases the duplicates to the slab */
    } else if (strcmp(mode, "sized_mismatch") == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        void *p = victim(64);
        bad = p;
        sdallocx(bad, 1000, 0); /* class of 1000 bytes is not the class of 64 */
    } else if (strcmp(mode, "free_sized_mismatch") == 0) {
        REQUIRE(free_sized != NULL, "free_sized not exported");
        void *p = victim(64);
        bad = p;
        free_sized(bad, 65); /* one byte over the class */
    } else if (strcmp(mode, "usable_size_freed") == 0) {
        void *p = victim(64);
        bad = p;
        REQUIRE(malloc_usable_size(bad) == 64, "usable %zu", malloc_usable_size(bad));
        free(bad);
        drain(); /* p leaves the cache and its slot bit clears */
        size_t u = malloc_usable_size(bad); /* must abort: the slot is not allocated */
        printf("usable size of the freed %p returned %zu\n", bad, u);
    } else if (strcmp(mode, "interior_free") == 0) {
        void *p = malloc(64);
        REQUIRE(p != NULL, "malloc failed");
        REQUIRE(malloc_usable_size(p) == 64, "usable %zu", malloc_usable_size(p));
        bad = (char *)p + 32;
        free(bad);
    } else if (strcmp(mode, "stack_free") == 0) {
        char buf[64];
        memset(buf, 0x5A, sizeof buf);
        bad = buf;
        free(bad);
    } else if (strcmp(mode, "huge_overflow") == 0) {
        size_t size = 1u << 20; /* above the largest slab class */
        bad = malloc(size);
        REQUIRE(bad != NULL, "malloc(%zu) failed", size);
        size_t usable = malloc_usable_size(bad);
        REQUIRE(usable >= size, "usable %zu < %zu", usable, size);
        /* Through the volatile so gcc cannot pair the index with the
         * malloc size (-Warray-bounds is an error under -Werror). */
        volatile unsigned char *p = bad;
        p[usable - 1] = 1; /* last byte inside the block is fine */
        p[usable] = 1;     /* first byte of the guard page must fault */
        printf("wrote past the end of %p (usable %zu)\n", bad, usable);
    } else if (strcmp(mode, "double_free_straddle") == 0) {
        /* The flush sorts the full cache (cap entries) and releases the
         * highest cap/2. With the pair, rest - 1 lower and cap - 2 - (rest
         * - 1) higher blocks cached (rest = cap - cap/2), the sorted copies
         * sit at indices rest - 1 and rest: one on each side of the split. */
        unsigned cap = small_cache_cap();
        REQUIRE(cap >= 8 && cap % 2 == 0, "cache cap %u", cap);
        size_t n = 4 * (size_t)cap;
        void **a = malloc(n * sizeof *a);
        REQUIRE(a != NULL, "malloc failed");
        for (size_t i = 0; i < n; i++) {
            a[i] = malloc(64);
            REQUIRE(a[i] != NULL, "malloc(64) failed");
        }
        REQUIRE(mallctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "tcache.flush failed");
        qsort(a, n, sizeof *a, cmp_ptr);
        size_t mid = n / 2;
        unsigned rest = cap - cap / 2, lower = rest - 1, higher = cap - 2 - lower;
        printf("cap %u: pair at sorted indices %u and %u of %u\n", cap, lower, lower + 1, cap);
        fflush(stdout);
        bad = a[mid];
        free(bad);
        free(bad); /* both copies now sit in the (empty) thread cache */
        for (unsigned i = 1; i <= lower; i++)
            free(a[mid - i]);
        for (unsigned i = 1; i <= higher; i++)
            free(a[mid + i]);
        free(a[0]); /* the cache holds cap entries: this push flushes first */
        void *q = malloc(64);
        printf("flush kept a copy: malloc(64) returned %p, the pair was %p\n", q, bad);
    } else if (strcmp(mode, "sized_guard") == 0) {
        /* Fill enough of the 64-byte class that the arena walks past its
         * first guard group; the guard is the one page group of slabs in
         * the used range that no block came from. Its body past the first
         * page is read-write memory (or PROT_NONE on a kernel without
         * MADV_GUARD_INSTALL); nothing here touches it. */
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        unsigned interval = ctl_uint("opt.guard_slab_interval");
        REQUIRE(ctl_bool("opt.guards") && interval > 0, "guards are off (interval %u)", interval);
        unsigned bin = bin_of_64();
        char name[64];
        snprintf(name, sizeof name, "arenas.bin.%u.slab_size", bin);
        size_t slab = ctl_size(name), page = ctl_size("arenas.page");
        size_t group = slab > page ? slab : page; /* one page group of slabs */
        /* One guard per `interval` units of 64 KiB (heap::guard_interval_for):
         * a class with smaller page groups has proportionally more groups
         * between guards. */
        size_t unit = page > 65536 ? page : 65536;
        size_t effective = group >= unit ? interval : interval * (unit / group);
        size_t per_slab = slab / 64;
        size_t n = per_slab * (group / slab) * (effective + 2);
        void **a = malloc(n * sizeof *a);
        REQUIRE(a != NULL, "malloc failed");
        for (size_t i = 0; i < n; i++) {
            a[i] = malloc(64);
            REQUIRE(a[i] != NULL, "malloc(64) failed");
        }
        qsort(a, n, sizeof *a, cmp_ptr);
        uintptr_t guard = 0;
        for (size_t i = 1; i < n; i++) {
            uintptr_t lo = (uintptr_t)a[i - 1] & ~(uintptr_t)(group - 1);
            uintptr_t hi = (uintptr_t)a[i] & ~(uintptr_t)(group - 1);
            if (hi - lo == 2 * group) {
                guard = lo + group;
                break;
            }
        }
        REQUIRE(guard != 0, "no one-group gap among %zu blocks (slab %zu, page %zu)", n, slab, page);
        /* A slot start in the body of the guard region: past the first page
         * when the region is more than one page, else its middle. */
        uintptr_t body = group > page ? guard + page : guard + group / 2;
        body &= ~(uintptr_t)63;
        printf("guard region at %#lx, sized free of slot %#lx\n", (unsigned long)guard, (unsigned long)body);
        fflush(stdout);
        bad = (void *)body;
        sdallocx(bad, 64, 0); /* must abort: the slot is in a guard slab */
        void *q = malloc(64);
        printf("sized free of guard memory %p was cached: malloc(64) returned %p\n", bad, q);
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }

    printf("survived %s\n", mode);
    return 0;
}
