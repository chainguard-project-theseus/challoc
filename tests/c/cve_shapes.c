/*
 * cve_shapes: the shapes of the CVEs in verus/RESULTS.md's "Vulnerability
 * classes ruled out" table, end to end under LD_PRELOAD=libchalloc.so,
 * one mode per CVE (src/api/c_programs.rs runs each as a row named after
 * it). The theorems there prove each entry point's contract; these runs
 * check that the shipped library, called from C, keeps it at the sizes
 * the CVEs were about:
 *
 *   cve-2006-7252   calloc, je_calloc and reallocarray with a count times
 *                   a size that wraps to a small product (0, 1, 2, 16,
 *                   4096, 2^32) return NULL with ENOMEM, and a failed
 *                   reallocarray keeps the old block intact.
 *   cve-2013-4332   pvalloc, valloc, memalign, aligned_alloc,
 *                   posix_memalign and their je_ twins at SIZE_MAX - k
 *                   (every k below 4096, around one and two pages, and
 *                   around the alignment), where
 *                   rounding up to the page or the alignment wraps, fail
 *                   with ENOMEM (posix_memalign returns it and leaves
 *                   *out as the caller left it).
 *   cve-2018-6485   memalign, aligned_alloc and posix_memalign with large
 *                   alignments: a size for which size + align wraps fails
 *                   with ENOMEM, an alignment of 2^47 up to 2^63 (larger
 *                   than the huge region, which Huge::run_pages refuses
 *                   on every host) fails, and a request that
 *                   succeeds is aligned, has at least `size` usable
 *                   bytes, and takes a write to its first and last byte.
 *   cve-2017-17426  malloc, je_malloc, calloc(1, n), realloc, je_realloc
 *                   and challoc_malloc_usable at SIZE_MAX - k fail with
 *                   ENOMEM while the thread cache of every class up to
 *                   1024 bytes holds freed slots (the glibc 2.26
 *                   bug was a tcache path with no overflow check), on the
 *                   main thread and on a fresh one. A failed realloc
 *                   keeps the block.
 *   cve-2018-6551   the same calls under tcache:false (the row sets it;
 *                   the mode checks opt.tcache reads false), so every
 *                   request reaches the class and huge paths and the
 *                   huge path's page rounding.
 *   cve-2005-4895   mallocx and je_mallocx at SIZE_MAX - k with every
 *                   MALLOCX_LG_ALIGN from 0 to 63 return NULL and leave
 *                   errno alone, as jemalloc's do, and nallocx answers 0
 *                   for each (theorem_nallocx_zero_rejects proves that a
 *                   0 from nallocx means a NULL from mallocx); rallocx to
 *                   such a size returns NULL and keeps the block;
 *                   xallocx with such a size, or with an `extra` that
 *                   makes size + extra wrap, returns the block's current
 *                   usable size and leaves it intact.
 *
 * Each mode prints `cve_shapes: <mode>: <n> requests` with the number of
 * requests it made, then `ok cve_shapes`.
 */
#include "common.h"

extern void *je_malloc(size_t) WEAK;
extern void *je_calloc(size_t, size_t) WEAK;
extern void *je_realloc(void *, size_t) WEAK;
extern int je_posix_memalign(void **, size_t, size_t) WEAK;
extern void *je_aligned_alloc(size_t, size_t) WEAK;
extern void *je_memalign(size_t, size_t) WEAK;
extern void *je_valloc(size_t) WEAK;
extern void *je_pvalloc(size_t) WEAK;
extern void *je_mallocx(size_t, int) WEAK;
extern void *je_rallocx(void *, size_t, int) WEAK;
extern size_t je_xallocx(void *, size_t, size_t, int) WEAK;
extern size_t je_nallocx(size_t, int) WEAK;

/* Sizes gcc must not see as constants (-Walloc-size-larger-than), and a
 * block it must not see realloc consume (-Wuse-after-free: a failed
 * realloc keeps it). */
static volatile size_t size_max = SIZE_MAX, one = 1;
static void *volatile kept;
static long requests;

/* `call` returns NULL and leaves errno at `want`, from 0 before it. */
#define NULL_ERRNO(call, want, k)                                                         \
    do {                                                                                  \
        errno = 0;                                                                        \
        sink = (call);                                                                    \
        int e_ = errno;                                                                   \
        requests++;                                                                       \
        CHECK(sink == NULL && e_ == (want), "%s (k = %zu): got = %p errno %d, want = NULL errno %d", \
              #call, (size_t)(k), sink, e_, (want));                                      \
        if (sink)                                                                         \
            free(sink);                                                                   \
    } while (0)

/* The distances below SIZE_MAX the modes try: every k below 4096, the
 * values around one and two pages of this host (16 KiB and 64 KiB pages
 * included), and around every third power of two from 2^13 to 2^46. */
static size_t near[4096 + 64];
static size_t nnear;

static void near_init(void)
{
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    nnear = 0;
    for (size_t k = 0; k < 4096; k++)
        near[nnear++] = k;
    const size_t pages[] = {page - 2, page - 1, page, page + 1, 2 * page - 1, 2 * page, 2 * page + QUANTUM};
    for (size_t i = 0; i < sizeof pages / sizeof pages[0]; i++)
        near[nnear++] = pages[i];
    for (int lg = 13; lg <= 46; lg += 3) {
        size_t p = (size_t)1 << lg;
        near[nnear++] = p - 1;
        near[nnear++] = p;
        near[nnear++] = p + 1;
    }
}

/* A block of `n` bytes filled with `seed`'s pattern. */
static unsigned char *filled(size_t n, uint32_t seed)
{
    unsigned char *p = malloc(n);
    REQUIRE(p != NULL, "malloc(%zu)", n);
    fill(p, n, seed);
    return p;
}

/* ---- CVE-2006-7252: a count times a size that wraps ------------------- */

static void cve_2006_7252(void)
{
    /* Pairs whose product is 2^64 + r for a small r (r in the comment),
     * so a calloc that multiplied without checking would allocate r. */
    const size_t two63 = (size_max >> 1) + 1, two32 = (size_t)1 << 32;
    const size_t pairs[][2] = {
        {two63, 2},             /* 0 */
        {two32, two32},         /* 0 */
        {two63 + 1, 2},         /* 2 */
        {two32 + 1, two32 - 1}, /* 2^64 - 1: no wrap, still too large */
        {(size_max / 3) + 1, 3}, /* 2 */
        {(size_max >> 4) + 2, 16}, /* 16 */
        {(size_max >> 12) + 2, 4096}, /* 4096 */
        {two32 + 1, two32},     /* 2^32 */
        {size_max, size_max},   /* 1 */
        {size_max, 2},          /* 2^64 - 2 */
    };
    for (size_t i = 0; i < sizeof pairs / sizeof pairs[0]; i++) {
        size_t a = pairs[i][0], b = pairs[i][1];
        NULL_ERRNO(calloc(a, b), ENOMEM, i);
        NULL_ERRNO(calloc(b, a), ENOMEM, i);
        NULL_ERRNO(je_calloc(a, b), ENOMEM, i);
        NULL_ERRNO(reallocarray(NULL, a, b), ENOMEM, i);
        kept = filled(100, (uint32_t)i);
        NULL_ERRNO(reallocarray(kept, a, b), ENOMEM, i);
        CHECK(check_fill(kept, 100, (uint32_t)i), "reallocarray pair %zu damaged the kept block", i);
        free(kept);
    }
}

/* ---- CVE-2013-4332: rounding to the page or the alignment wraps ------- */

static void aligned_near_max(size_t align, size_t k)
{
    size_t n = size_max - k;
    NULL_ERRNO(memalign(align, n), ENOMEM, k);
    NULL_ERRNO(je_memalign(align, n), ENOMEM, k);
    NULL_ERRNO(aligned_alloc(align, n), ENOMEM, k);
    NULL_ERRNO(je_aligned_alloc(align, n), ENOMEM, k);
    void *const mark = (void *)&mark;
    void *q = mark;
    errno = 0;
    int rc = posix_memalign(&q, align, n);
    requests++;
    CHECK(rc == ENOMEM && q == mark, "posix_memalign(%zu, SIZE_MAX - %zu): rc %d out %p", align, k, rc, q);
    q = mark;
    rc = je_posix_memalign(&q, align, n);
    requests++;
    CHECK(rc == ENOMEM && q == mark, "je_posix_memalign(%zu, SIZE_MAX - %zu): rc %d out %p", align, k, rc, q);
}

static void cve_2013_4332(void)
{
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    for (size_t i = 0; i < nnear; i++) {
        size_t k = near[i], n = size_max - k;
        NULL_ERRNO(valloc(n), ENOMEM, k);
        NULL_ERRNO(je_valloc(n), ENOMEM, k);
        NULL_ERRNO(pvalloc(n), ENOMEM, k);
        NULL_ERRNO(je_pvalloc(n), ENOMEM, k);
        aligned_near_max(QUANTUM, k);
        aligned_near_max(page, k);
    }
    /* Around each alignment: size + align - 1 wraps for every k up to
     * align - 2 and is SIZE_MAX at align - 1; every one must fail. */
    for (int lg = 4; lg <= 30; lg += 2) {
        size_t align = one << lg;
        const size_t ks[] = {0, 1, align / 2, align - 1, align, align + 1, 2 * align - 1};
        for (size_t i = 0; i < sizeof ks / sizeof ks[0]; i++)
            aligned_near_max(align, ks[i]);
    }
}

/* ---- CVE-2018-6485: large alignments --------------------------------- */

/* A successful aligned request: aligned, `n` usable bytes, both ends
 * writable. */
static void aligned_block(void *p, size_t align, size_t n, const char *what)
{
    CHECK(p != NULL, "%s(%zu, %zu) = NULL", what, align, n);
    if (!p)
        return;
    CHECK(((uintptr_t)p & (align - 1)) == 0, "%s(%zu, %zu) = %p misaligned", what, align, n, p);
    CHECK(malloc_usable_size(p) >= n, "%s(%zu, %zu): usable %zu", what, align, n, malloc_usable_size(p));
    volatile unsigned char *b = p; /* volatile: the reads below are real loads */
    b[0] = 0x5a;
    b[n - 1] = 0xa5;
    CHECK((n == 1 || b[0] == 0x5a) && b[n - 1] == 0xa5, "%s(%zu, %zu): ends", what, align, n);
    free(p);
}

static void cve_2018_6485(void)
{
    /* size + align wraps: size = SIZE_MAX - align + 1 + j. */
    for (int lg = 4; lg <= 62; lg++) {
        size_t align = one << lg;
        for (size_t j = 0; j < 4; j++)
            aligned_near_max(align, align - 1 - j);
    }
    /* An alignment of 2^47 or more is larger than the huge region
     * (at most 2^40 bytes), so Huge::run_pages refuses it
     * (`align > npages << lg_page`) whatever the host's address space. */
    for (int lg = 47; lg <= 63; lg++) {
        size_t align = one << lg;
        NULL_ERRNO(memalign(align, 1), ENOMEM, lg);
        NULL_ERRNO(aligned_alloc(align, align), ENOMEM, lg);
        void *const mark = (void *)&mark;
        void *q = mark;
        int rc = posix_memalign(&q, align, 64);
        requests++;
        CHECK(rc == ENOMEM && q == mark, "posix_memalign(2^%d, 64): rc %d out %p", lg, rc, q);
    }
    /* Requests that succeed get what they asked for. */
    const int lgs[] = {12, 16, 20, 21, 22, 24};
    for (size_t i = 0; i < sizeof lgs / sizeof lgs[0]; i++) {
        size_t align = one << lgs[i];
        const size_t ns[] = {1, align - 1, align, align + 1, 3 * align + 1, ((size_t)4 << 20) + 1};
        for (size_t j = 0; j < sizeof ns / sizeof ns[0]; j++) {
            size_t n = ns[j];
            aligned_block(memalign(align, n), align, n, "memalign");
            aligned_block(aligned_alloc(align, n), align, n, "aligned_alloc");
            void *q = NULL;
            int rc = posix_memalign(&q, align, n);
            CHECK(rc == 0, "posix_memalign(%zu, %zu) rc %d", align, n, rc);
            aligned_block(q, align, n, "posix_memalign");
            requests += 3;
        }
    }
}

/* ---- CVE-2017-17426 and CVE-2018-6551: malloc near SIZE_MAX ----------- */

/* Leave freed slots in the thread's cache of every class up to 1024 bytes
 * (a quantum apart, so every class is hit) and of some larger ones. */
static void warm_caches(void)
{
    enum { N = 64 };
    void *blocks[N];
    for (size_t n = 1; n <= 4096; n += n < 1024 ? QUANTUM : n / 4) {
        for (int i = 0; i < N; i++)
            blocks[i] = filled(n, (uint32_t)n);
        for (int i = 0; i < N; i++)
            free(blocks[i]);
    }
}

static void malloc_near_max(void)
{
    warm_caches();
    for (size_t i = 0; i < nnear; i++) {
        size_t k = near[i], n = size_max - k;
        NULL_ERRNO(malloc(n), ENOMEM, k);
        NULL_ERRNO(je_malloc(n), ENOMEM, k);
        NULL_ERRNO(calloc(1, n), ENOMEM, k);
        NULL_ERRNO(realloc(NULL, n), ENOMEM, k);
        size_t u = 1;
        NULL_ERRNO(challoc_malloc_usable(n, &u), ENOMEM, k);
        CHECK(u == 0, "challoc_malloc_usable(SIZE_MAX - %zu): usable %zu, want = 0", k, u);
        if (i % 64 == 0) {
            kept = filled(48, (uint32_t)k);
            NULL_ERRNO(realloc(kept, n), ENOMEM, k);
            NULL_ERRNO(je_realloc(kept, n), ENOMEM, k);
            CHECK(check_fill(kept, 48, (uint32_t)k), "realloc(p, SIZE_MAX - %zu) damaged the block", k);
            free(kept);
        }
    }
}

static void *malloc_near_max_thread(void *arg)
{
    (void)arg;
    malloc_near_max();
    return NULL;
}

static void malloc_near_max_both(void)
{
    malloc_near_max();
    pthread_t t;
    REQUIRE(pthread_create(&t, NULL, malloc_near_max_thread, NULL) == 0, "pthread_create");
    pthread_join(t, NULL);
}

/* ---- CVE-2005-4895: mallocx, rallocx and xallocx near SIZE_MAX -------- */

static void cve_2005_4895(void)
{
    warm_caches();
    for (int lg = 0; lg <= 63; lg++) {
        int flags = MALLOCX_LG_ALIGN(lg);
        for (size_t i = 0; i < nnear; i += lg == 0 ? 1 : 7) {
            size_t k = near[i], n = size_max - k;
            requests += 2;
            CHECK(nallocx(n, flags) == 0, "nallocx(SIZE_MAX - %zu, lg %d) = %zu", k, lg, nallocx(n, flags));
            CHECK(je_nallocx(n, flags) == 0, "je_nallocx(SIZE_MAX - %zu, lg %d)", k, lg);
            NULL_ERRNO(mallocx(n, flags), 0, k);
            NULL_ERRNO(je_mallocx(n, flags | MALLOCX_ZERO), 0, k);
        }
        size_t base = lg <= 20 ? one << lg : 64;
        kept = mallocx(base, lg <= 20 ? flags : 0);
        REQUIRE(kept != NULL, "mallocx(%zu, lg %d)", base, lg);
        size_t usable = sallocx(kept, 0);
        fill(kept, usable, (uint32_t)lg);
        for (size_t i = 0; i < nnear; i += 13) {
            size_t k = near[i], n = size_max - k;
            NULL_ERRNO(rallocx(kept, n, flags), 0, k);
            NULL_ERRNO(je_rallocx(kept, n, flags | MALLOCX_ZERO), 0, k);
            requests += 4;
            CHECK(xallocx(kept, n, 0, flags) == usable, "xallocx(p, SIZE_MAX - %zu, 0, lg %d)", k, lg);
            CHECK(je_xallocx(kept, n, k, flags) == usable, "je_xallocx(p, SIZE_MAX - %zu, %zu, lg %d)", k, k, lg);
            /* size + extra wraps past SIZE_MAX: always for (n, n), for
             * (64, n) when k < 64. */
            CHECK(xallocx(kept, 64, n, flags) == usable, "xallocx(p, 64, SIZE_MAX - %zu, lg %d)", k, lg);
            CHECK(xallocx(kept, n, n, flags) == usable, "xallocx(p, SIZE_MAX - %zu, SIZE_MAX - %zu, lg %d)", k, k, lg);
        }
        CHECK(sallocx(kept, 0) == usable && check_fill(kept, usable, (uint32_t)lg),
              "the block at lg %d changed under the failed rallocx and xallocx calls", lg);
        dallocx(kept, 0);
    }
}

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(argc == 2, "usage: cve_shapes <mode>");
    const char *mode = argv[1];
    printf("cve_shapes: %s\n", mode);
    fflush(stdout);
    near_init();
    if (strcmp(mode, "cve-2006-7252") == 0) {
        cve_2006_7252();
    } else if (strcmp(mode, "cve-2013-4332") == 0) {
        cve_2013_4332();
    } else if (strcmp(mode, "cve-2018-6485") == 0) {
        cve_2018_6485();
    } else if (strcmp(mode, "cve-2017-17426") == 0) {
        REQUIRE(ctl_bool("opt.tcache"), "cve-2017-17426 runs with the thread cache on");
        malloc_near_max_both();
    } else if (strcmp(mode, "cve-2018-6551") == 0) {
        REQUIRE(!ctl_bool("opt.tcache"), "cve-2018-6551 runs under tcache:false");
        malloc_near_max_both();
    } else if (strcmp(mode, "cve-2005-4895") == 0) {
        cve_2005_4895();
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }
    printf("cve_shapes: %s: %ld requests\n", mode, requests);
    return finish("cve_shapes");
}
