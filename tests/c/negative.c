/*
 * Negative tests. Each mode performs one invalid operation
 * that challoc must refuse by aborting (or that the guard page must turn
 * into a fault). The driver in src/api/c_programs.rs runs one mode per process
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
 *   negative interior_free        free(p + size/2)
 *   negative stack_free           free of a stack address
 *   negative huge_overflow        write one byte past a huge allocation
 *   negative double_free_straddle free(p) twice, then exactly enough other
 *                                 frees that the flush's sorted batch has one
 *                                 copy at the split point and the other just
 *                                 below it: without the boundary compare one
 *                                 copy is released and the other stays cached
 *   negative sized_guard          sdallocx of a slot start inside a guard
 *                                 slab, then malloc: the sized path must
 *                                 reject the guard slab, not cache it and hand
 *                                 the guard memory out
 *   negative guard_free           plain free of the same guard-slab slot: the
 *                                 exact bit test finds the slot clear and the
 *                                 slab a guard
 *   negative realloc_stale        realloc(p, n) after free(p) and a flush: the
 *                                 bit test of realloc's check_live
 *   negative sallocx_stale        sallocx(p, 0) after free(p) and a flush
 *   negative dallocx_none_twice   dallocx(p, MALLOCX_TCACHE_NONE) twice: the
 *                                 uncached release checks the bit at once
 *   negative dallocx_bad_tcache   dallocx(p, MALLOCX_TCACHE(4000)), an id
 *                                 tcache.create never returned: aborts at
 *                                 the call, naming the id
 *   negative free_sized_huge_hint free_sized(slab pointer, 1 MiB): a huge
 *                                 size hint on a slab pointer aborts at the
 *                                 call (the hint names no class)
 *   negative sized_huge_mismatch  sdallocx(1 MiB run, 64, 0): the size does not
 *                                 round to the run's pages, so the call aborts
 *   negative free_sized_huge_mismatch
 *                                 free_sized(1 MiB run, 4 MiB): the same
 *   negative huge_double_free     free of a 1 MiB run twice
 *   negative huge_interior_free   free of the second page of a 1 MiB run
 *   negative free_usable_interior challoc_free_usable(p + size/2): the
 *                                 size-returning free (design 13.7) keeps
 *                                 free's exact check and its message
 *   negative free_usable_stale    challoc_free_usable(p) after free(p) and a
 *                                 flush: the slot bit
 *   negative free_usable_cached   free(p) then challoc_free_usable(p) back to
 *                                 back: caught at the flush, or at the call
 *                                 with double_free_scan
 *   negative free_usable_huge_twice free of a 1 MiB run, then
 *                                 challoc_free_usable of it
 *   negative usable_cached        malloc_usable_size(p) after free(p) while the
 *                                 copy is still cached (double_free_scan: the
 *                                 state byte says cached; the bit is set)
 *   negative realloc_cached_small realloc(p, 64) the same way (in place)
 *   negative realloc_cached_large realloc(p, 4096) the same way (moves)
 *   negative sallocx_cached       sallocx(p, 0) the same way
 *   negative route_<route>_<case> the free-route matrix, run with
 *                                 double_free_scan:true (the per-slot state:
 *                                 every route checks at the call). <route>
 *                                 is the second, bad free: free, sdallocx,
 *                                 free_sized, dallocx_none (MALLOCX_TCACHE_NONE),
 *                                 sdallocx_none or free_usable. <case>:
 *                                   same    the route frees p twice back to back
 *                                   mixed   free(p), then the route frees p while
 *                                           the first copy is still cached
 *                                   cross   another thread frees p (its copy stays
 *                                           in that thread's cache), then the route
 *                                           frees p on this thread
 *                                   forged  the route frees a slot start of p's
 *                                           slab that challoc never handed out and
 *                                           no refill claimed (its bit is clear)
 *                                   forged_cached
 *                                           the route frees the slot after p, which
 *                                           the refill that handed out p claimed
 *                                           into the cache (its bit is set; only
 *                                           the state byte says it is not live)
 *                                   size    a sized route with the wrong size
 *   negative sized_never_allocated_SIZE
 *                                 sdallocx(p, SIZE, 0) of slot 0 of the slab
 *                                 after the one the first SIZE-byte block
 *                                 came from, a slot never handed out: with
 *                                 exact_sized_free the call aborts
 *   negative free_sized_never_allocated_SIZE
 *                                 the same through C23 free_sized
 */
#include "common.h"
#include <pthread.h>
#include <stdbool.h>

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

static ssize_t ctl_ssize(const char *name)
{
    ssize_t v = 0;
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

/* Fill enough of the 64-byte class that the arena walks past its first
 * guard group; the guard is the one page group of slabs in the used range
 * that no block came from. Its body past the first page is read-write
 * memory (or PROT_NONE on a kernel without MADV_GUARD_INSTALL); nothing
 * here touches it. Returns a slot start in the body of the guard region:
 * past the first page when the region is more than one page, else its
 * middle. */
static uintptr_t guard_slot_of_64(void);

/* Slot 0 of the slab after the one the first SIZE-byte block came from:
 * a slot start of the class that no allocation has returned. */
static uintptr_t never_allocated_slot(size_t size);

/* Bin index of the SIZE-byte class. */
static unsigned bin_of(size_t size)
{
    unsigned nbins = ctl_uint("arenas.nbins");
    for (unsigned i = 0; i < nbins; i++) {
        char name[64];
        snprintf(name, sizeof name, "arenas.bin.%u.size", i);
        if (ctl_size(name) == size)
            return i;
    }
    REQUIRE(0, "no %zu-byte bin", size);
    return 0;
}

/* The route matrix's second free of `p` (`size` bytes). */
static void route_free(const char *route, void *p, size_t size)
{
    if (strcmp(route, "free") == 0) {
        free(p);
    } else if (strcmp(route, "sdallocx") == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        sdallocx(p, size, 0);
    } else if (strcmp(route, "free_sized") == 0) {
        REQUIRE(free_sized != NULL, "free_sized not exported");
        free_sized(p, size);
    } else if (strcmp(route, "dallocx_none") == 0) {
        REQUIRE(dallocx != NULL, "dallocx not exported");
        dallocx(p, MALLOCX_TCACHE_NONE);
    } else if (strcmp(route, "sdallocx_none") == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        sdallocx(p, size, MALLOCX_TCACHE_NONE);
    } else if (strcmp(route, "free_usable") == 0) {
        REQUIRE(challoc_free_usable != NULL, "challoc_free_usable not exported");
        (void)challoc_free_usable(p);
    } else {
        REQUIRE(0, "unknown route %s", route);
    }
}

static void *volatile cross_p;
static pthread_mutex_t cross_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cross_cv = PTHREAD_COND_INITIALIZER;
static int cross_stage;

/* The other thread of `cross`: free p, then stay alive (its cache keeps
 * the copy) until the process ends. */
static void *cross_thread(void *arg)
{
    (void)arg;
    free(cross_p);
    pthread_mutex_lock(&cross_mu);
    cross_stage = 1;
    pthread_cond_broadcast(&cross_cv);
    while (cross_stage != 2)
        pthread_cond_wait(&cross_cv, &cross_mu);
    pthread_mutex_unlock(&cross_mu);
    return NULL;
}

static void route_matrix(const char *spec)
{
    char route[32];
    /* The case is the suffix after the route: the one case name with an
     * underscore is matched first. */
    const char *cs = strstr(spec, "_forged_cached");
    if (cs == NULL)
        cs = strrchr(spec, '_');
    REQUIRE(cs != NULL && (size_t)(cs - spec) < sizeof route, "bad route mode %s", spec);
    memcpy(route, spec, (size_t)(cs - spec));
    route[cs - spec] = 0;
    const char *c = cs + 1;
    void *volatile bad;
    void *p = victim(64);
    if (strcmp(c, "same") == 0) {
        bad = p;
        route_free(route, bad, 64);
        route_free(route, bad, 64);
        printf("second free of %p returned\n", bad);
        fflush(stdout);
    } else if (strcmp(c, "mixed") == 0) {
        bad = p;
        free(bad);
        route_free(route, bad, 64);
        printf("second free of %p returned\n", bad);
        fflush(stdout);
    } else if (strcmp(c, "cross") == 0) {
        pthread_t t;
        cross_p = p;
        REQUIRE(pthread_create(&t, NULL, cross_thread, NULL) == 0, "pthread_create failed");
        pthread_mutex_lock(&cross_mu);
        while (cross_stage != 1)
            pthread_cond_wait(&cross_cv, &cross_mu);
        pthread_mutex_unlock(&cross_mu);
        bad = p;
        route_free(route, bad, 64);
        printf("cross free of %p returned\n", bad);
        fflush(stdout);
        drain(); /* this thread's flushes; the other copy stays cached */
        for (int i = 0; i < HOLD; i++)
            hold[i] = malloc(64); /* a refill may claim p again */
        pthread_mutex_lock(&cross_mu);
        cross_stage = 2;
        pthread_cond_broadcast(&cross_cv);
        pthread_mutex_unlock(&cross_mu);
        pthread_join(t, NULL); /* its exit flush releases the other copy */
    } else if (strcmp(c, "forged") == 0) {
        /* A slot of p's slab well past the refill batch that handed p
         * out: no refill has claimed it. */
        bad = (char *)p + 64 * 600;
        route_free(route, bad, 64);
        printf("forged free of %p returned\n", bad);
        fflush(stdout);
        drain();
    } else if (strcmp(c, "forged_cached") == 0) {
        bad = (char *)p + 64; /* the next entry of the refill batch, still cached */
        route_free(route, bad, 64);
        printf("forged free of the cached %p returned\n", bad);
        fflush(stdout);
    } else if (strcmp(c, "size") == 0) {
        bad = p;
        route_free(route, bad, 128);
        printf("mis-sized free of %p returned\n", bad);
        fflush(stdout);
    } else {
        REQUIRE(0, "unknown case %s", c);
    }
    drain();
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

    if (strncmp(mode, "route_", 6) == 0) {
        route_matrix(mode + 6);
    } else if (strcmp(mode, "usable_cached") == 0 || strcmp(mode, "realloc_cached_small") == 0
               || strcmp(mode, "realloc_cached_large") == 0 || strcmp(mode, "sallocx_cached") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad); /* the copy stays in the thread cache, its bit set */
        size_t r = 0;
        if (strcmp(mode, "usable_cached") == 0) {
            r = malloc_usable_size(bad);
        } else if (strcmp(mode, "realloc_cached_small") == 0) {
            r = (size_t)realloc(bad, 64);
        } else if (strcmp(mode, "realloc_cached_large") == 0) {
            r = (size_t)realloc(bad, 4096);
        } else {
            REQUIRE(sallocx != NULL, "sallocx not exported");
            r = sallocx(bad, 0);
        }
        printf("%s of the cached %p returned %#zx\n", mode, bad, r);
        fflush(stdout);
    } else if (strcmp(mode, "double_free_stale") == 0) {
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
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        bad = (void *)guard_slot_of_64();
        sdallocx(bad, 64, 0); /* must abort: the slot is in a guard slab */
        void *q = malloc(64);
        printf("sized free of guard memory %p was cached: malloc(64) returned %p\n", bad, q);
    } else if (strcmp(mode, "guard_free") == 0) {
        bad = (void *)guard_slot_of_64();
        free(bad); /* must abort: the slot bit is clear and the slab is a guard */
        void *q = malloc(64);
        printf("free of guard memory %p was accepted: malloc(64) returned %p\n", bad, q);
    } else if (strcmp(mode, "realloc_stale") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad);
        drain(); /* p leaves the cache and its slot bit clears */
        void *q = realloc(bad, 128); /* must abort: the slot is not allocated */
        printf("realloc of the freed %p returned %p\n", bad, q);
    } else if (strcmp(mode, "sallocx_stale") == 0) {
        REQUIRE(sallocx != NULL, "sallocx not exported");
        void *p = victim(64);
        bad = p;
        free(bad);
        drain();
        size_t u = sallocx(bad, 0); /* must abort: the slot is not allocated */
        printf("sallocx of the freed %p returned %zu\n", bad, u);
    } else if (strcmp(mode, "dallocx_none_twice") == 0) {
        REQUIRE(dallocx != NULL, "dallocx not exported");
        void *p = victim(64);
        bad = p;
        dallocx(bad, MALLOCX_TCACHE_NONE); /* straight to the slab bitmap */
        dallocx(bad, MALLOCX_TCACHE_NONE); /* must abort: the bit is already clear */
        printf("second uncached dallocx of %p returned\n", bad);
    } else if (strcmp(mode, "dallocx_bad_tcache") == 0) {
        REQUIRE(dallocx != NULL, "dallocx not exported");
        void *p = victim(64);
        bad = p;
        dallocx(bad, MALLOCX_TCACHE(4000)); /* must abort: no explicit cache 4000 */
        printf("dallocx(%p, MALLOCX_TCACHE(4000)) returned\n", bad);
    } else if (strcmp(mode, "free_sized_huge_hint") == 0) {
        REQUIRE(free_sized != NULL, "free_sized not exported");
        void *p = victim(64);
        bad = p;
        free_sized(bad, 1u << 20); /* must abort: a huge hint on a slab slot */
        printf("free_sized(%p, 1 MiB) returned\n", bad);
    } else if (strcmp(mode, "sized_huge_mismatch") == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        bad = malloc(1u << 20);
        REQUIRE(bad != NULL, "malloc(1 MiB) failed");
        sdallocx(bad, 64, 0); /* must abort: 64 bytes is not the run's size */
        printf("sdallocx(%p, 64, 0) of a 1 MiB run returned\n", bad);
    } else if (strcmp(mode, "free_sized_huge_mismatch") == 0) {
        REQUIRE(free_sized != NULL, "free_sized not exported");
        bad = malloc(1u << 20);
        REQUIRE(bad != NULL, "malloc(1 MiB) failed");
        free_sized(bad, 4u << 20); /* must abort: 4 MiB is not the run's size */
        printf("free_sized(%p, 4 MiB) of a 1 MiB run returned\n", bad);
    } else if (strcmp(mode, "huge_double_free") == 0) {
        size_t size = 1u << 20;
        bad = malloc(size);
        REQUIRE(bad != NULL, "malloc(%zu) failed", size);
        memset(bad, 0x5A, size);
        free(bad);
        free(bad); /* must abort: the run is no longer live */
        printf("second free of the huge run %p returned\n", bad);
    } else if (strcmp(mode, "huge_interior_free") == 0) {
        size_t size = 1u << 20, page = (size_t)sysconf(_SC_PAGESIZE);
        char *p = malloc(size);
        REQUIRE(p != NULL, "malloc(%zu) failed", size);
        memset(p, 0x5A, size);
        bad = p + page; /* page-aligned, inside the run, not its start */
        free(bad);      /* must abort: not the start of a huge allocation */
        printf("free of the interior page %p of %p returned\n", bad, (void *)p);
    } else if (strcmp(mode, "free_usable_interior") == 0) {
        REQUIRE(challoc_free_usable != NULL, "challoc_free_usable not exported");
        void *p = malloc(64);
        REQUIRE(p != NULL, "malloc failed");
        bad = (char *)p + 32;
        size_t u = challoc_free_usable(bad); /* must abort: not a slot start */
        printf("free_usable of the interior %p returned %zu\n", bad, u);
    } else if (strcmp(mode, "free_usable_stale") == 0) {
        REQUIRE(challoc_free_usable != NULL, "challoc_free_usable not exported");
        void *p = victim(64);
        bad = p;
        free(bad);
        drain(); /* p leaves the cache and its slot bit clears */
        size_t u = challoc_free_usable(bad); /* must abort: the slot is not allocated */
        printf("free_usable of the freed %p returned %zu\n", bad, u);
    } else if (strcmp(mode, "free_usable_cached") == 0) {
        REQUIRE(challoc_free_usable != NULL, "challoc_free_usable not exported");
        void *p = victim(64);
        bad = p;
        free(bad);
        size_t u = challoc_free_usable(bad); /* with double_free_scan: aborts here */
        printf("free_usable of the cached %p returned %zu\n", bad, u);
        fflush(stdout);
        drain(); /* else the flush releases the duplicates to the slab */
    } else if (strcmp(mode, "free_usable_huge_twice") == 0) {
        REQUIRE(challoc_free_usable != NULL, "challoc_free_usable not exported");
        size_t size = 1u << 20;
        bad = malloc(size);
        REQUIRE(bad != NULL, "malloc(%zu) failed", size);
        free(bad);
        size_t u = challoc_free_usable(bad); /* must abort: the run is no longer live */
        printf("free_usable of the freed huge run %p returned %zu\n", bad, u);
    } else if (strncmp(mode, "sized_never_allocated_", 22) == 0) {
        REQUIRE(sdallocx != NULL, "sdallocx not exported");
        size_t size = strtoul(mode + 22, NULL, 10);
        bad = (void *)never_allocated_slot(size);
        sdallocx(bad, size, 0); /* with exact_sized_free: must abort */
        printf("sdallocx of the never-allocated %p returned\n", bad);
    } else if (strncmp(mode, "free_sized_never_allocated_", 27) == 0) {
        REQUIRE(free_sized != NULL, "free_sized not exported");
        size_t size = strtoul(mode + 27, NULL, 10);
        bad = (void *)never_allocated_slot(size);
        free_sized(bad, size); /* with exact_sized_free: must abort */
        printf("free_sized of the never-allocated %p returned\n", bad);
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }

    printf("survived %s\n", mode);
    return 0;
}

static uintptr_t guard_slot_of_64(void)
{
    unsigned interval = ctl_uint("opt.guard_slab_interval");
    REQUIRE(ctl_bool("opt.guards") && interval > 0, "guards are off (interval %u)", interval);
    unsigned bin = bin_of(64);
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
    uintptr_t body = group > page ? guard + page : guard + group / 2;
    body &= ~(uintptr_t)63;
    printf("guard region at %#lx, slot in its body %#lx\n", (unsigned long)guard, (unsigned long)body);
    fflush(stdout);
    return body;
}

static uintptr_t never_allocated_slot(size_t size)
{
    unsigned bin = bin_of(size);
    char name[96];
    snprintf(name, sizeof name, "arenas.bin.%u.slab_size", bin);
    size_t slab = ctl_size(name);
    void *first = malloc(size);
    REQUIRE(first != NULL, "malloc(%zu) failed", size);
    /* The target is unallocated only while the class has one slab in use:
     * a refill that reached the next slab would have claimed its slot 0. */
    uint64_t epoch = 1;
    REQUIRE(mallctl("epoch", NULL, NULL, &epoch, sizeof epoch) == 0, "epoch failed");
    snprintf(name, sizeof name, "stats.arenas.0.bins.%u.curslabs", bin);
    size_t cur = ctl_size(name);
    REQUIRE(cur == 1, "class %zu has %zu slabs in use, want 1", size, cur);
    uintptr_t target = ((uintptr_t)first & ~(uintptr_t)(slab - 1)) + slab;
    printf("class %zu: slab %zu bytes, first block %p, target %#lx\n", size, slab, first,
           (unsigned long)target);
    fflush(stdout);
    return target;
}
