/*
 * challoc's size-returning entry points from C (docs/design.md section
 * 13.7): challoc_malloc_usable, challoc_calloc_usable,
 * challoc_realloc_usable and challoc_free_usable, declared in
 * <challoc/challoc.h> (include/, on the include path through -I). Run
 * under LD_PRELOAD=libchalloc.so by src/api/c_programs.rs
 * (the driver runs it under the default configuration and the hardening
 * keys, so every push and pop variant answers the same).
 *
 * Claims: on every tier (slab classes, the classes above the bins, huge
 * runs) the stored or returned usable size equals malloc_usable_size of the
 * pointer; the pointer is quantum-aligned; calloc_usable reads as zero over
 * the whole usable size; realloc_usable keeps the pointer within the class
 * and preserves content across a move; a null `usable` is accepted; the null
 * pointer cases (free_usable(NULL) == 0, realloc_usable(NULL, n) allocates,
 * realloc_usable(p, 0) frees and reports 0); ENOMEM with *usable == 0 on a
 * failed allocation and the block intact after a failed realloc_usable;
 * blocks made by malloc, calloc, aligned_alloc and mallocx are freed by
 * free_usable with their class size, cross-thread too. The negative cases
 * (free_usable of an interior, stale, cached-twice or non-live huge pointer)
 * are modes of negative.c.
 */
#include "common.h"
#include <challoc/challoc.h>

#ifndef CHALLOC_USABLE
#error "<challoc/challoc.h> must define CHALLOC_USABLE"
#endif

/* Sizes gcc must not see as constants (-Walloc-size-larger-than). */
static volatile size_t huge_n = SIZE_MAX;

static const size_t sizes[] = {0, 1, 7, 8, 15, 16, 17, 48, 100, 1000, 1024, 1025, 4096, 5000,
                               16384, 65536, 262144, 262145, 1 << 20, 10 << 20};
#define NSIZES (sizeof sizes / sizeof sizes[0])

static void test_tiers(void)
{
    for (size_t i = 0; i < NSIZES; i++) {
        size_t n = sizes[i], u = SIZE_MAX;
        unsigned char *p = challoc_malloc_usable(n, &u);
        CHECK(p != NULL, "challoc_malloc_usable(%zu)", n);
        CHECK(((uintptr_t)p % QUANTUM) == 0, "challoc_malloc_usable(%zu) misaligned %p", n, (void *)p);
        CHECK(u == malloc_usable_size(p), "malloc_usable(%zu): usable %zu != malloc_usable_size %zu", n, u,
              malloc_usable_size(p));
        CHECK(u >= n, "usable %zu < %zu", u, n);
        fill(p, u, (unsigned)i);
        CHECK(check_fill(p, u, (unsigned)i), "content %zu", n);
        size_t f = challoc_free_usable(p);
        CHECK(f == u, "challoc_free_usable(%zu) returned %zu, usable was %zu", n, f, u);

        /* Dirty a block of the size and free it, then calloc_usable: the
         * reused memory must read as zero over the whole usable size. */
        unsigned char *d = malloc(n ? n : 1);
        memset(d, 0xAB, malloc_usable_size(d));
        free(d);
        size_t uc = SIZE_MAX;
        unsigned char *q = challoc_calloc_usable(1, n, &uc);
        CHECK(q != NULL, "challoc_calloc_usable(1, %zu)", n);
        CHECK(uc == malloc_usable_size(q), "calloc_usable(%zu): usable %zu != %zu", n, uc, malloc_usable_size(q));
        CHECK(is_zero(q, uc), "calloc_usable(%zu) not zero over its %zu usable bytes", n, uc);
        fill(q, uc, 7);

        /* Same size: the pointer and the size stay (realloc_usable(q, 0)
         * would free, so 0 asks for 1: still the smallest class). */
        size_t ur = SIZE_MAX;
        unsigned char *r = challoc_realloc_usable(q, n ? n : 1, &ur);
        CHECK(r == q && ur == uc, "realloc_usable(%zu) in place: %p/%zu -> %p/%zu", n, (void *)q, uc, (void *)r, ur);
        /* A move across tiers keeps the content. */
        size_t grown = 2 * n + 4097, ug = SIZE_MAX;
        unsigned char *g = challoc_realloc_usable(r, grown, &ug);
        CHECK(g != NULL, "realloc_usable(%zu -> %zu)", n, grown);
        CHECK(ug == malloc_usable_size(g) && ug >= grown, "realloc_usable(%zu -> %zu): usable %zu", n, grown, ug);
        CHECK(check_fill(g, uc < grown ? uc : grown, 7), "realloc_usable(%zu -> %zu) content", n, grown);
        CHECK(challoc_free_usable(g) == ug, "free_usable after realloc_usable(%zu)", n);
    }
}

static void test_null_and_errors(void)
{
    CHECK(challoc_free_usable(NULL) == 0, "free_usable(NULL)");

    /* A null usable makes them plain malloc/calloc/realloc. */
    unsigned char *p = challoc_malloc_usable(100, NULL);
    CHECK(p != NULL && malloc_usable_size(p) >= 100, "malloc_usable(100, NULL)");
    unsigned char *q = challoc_calloc_usable(2, 50, NULL);
    CHECK(q != NULL && is_zero(q, 100), "calloc_usable(2, 50, NULL)");
    unsigned char *r = challoc_realloc_usable(q, 5000, NULL);
    CHECK(r != NULL && malloc_usable_size(r) >= 5000, "realloc_usable(q, 5000, NULL)");
    free(p);
    free(r);

    /* realloc_usable(NULL, n) allocates; realloc_usable(p, 0) frees. */
    size_t u = SIZE_MAX;
    unsigned char *a = challoc_realloc_usable(NULL, 300, &u);
    CHECK(a != NULL && u == malloc_usable_size(a), "realloc_usable(NULL, 300)");
    fill(a, 300, 9);
    errno = 0;
    size_t u2 = SIZE_MAX;
    sink = challoc_realloc_usable(a, huge_n / 2, &u2);
    CHECK(sink == NULL && errno == ENOMEM && u2 == 0, "realloc_usable ENOMEM: errno %d usable %zu", errno, u2);
    CHECK(check_fill(a, 300, 9), "block intact after a failed realloc_usable");
    size_t u3 = SIZE_MAX;
    sink = challoc_realloc_usable(a, 0, &u3);
    CHECK(sink == NULL && u3 == 0, "realloc_usable(a, 0)");

    errno = 0;
    u = SIZE_MAX;
    sink = challoc_malloc_usable(huge_n / 2, &u);
    CHECK(sink == NULL && errno == ENOMEM && u == 0, "malloc_usable ENOMEM: errno %d usable %zu", errno, u);
    errno = 0;
    u = SIZE_MAX;
    sink = challoc_calloc_usable(huge_n, 2, &u);
    CHECK(sink == NULL && errno == ENOMEM && u == 0, "calloc_usable overflow: errno %d usable %zu", errno, u);
    errno = 0;
    u = SIZE_MAX;
    sink = challoc_calloc_usable(1, huge_n / 2, &u);
    CHECK(sink == NULL && errno == ENOMEM && u == 0, "calloc_usable ENOMEM: errno %d usable %zu", errno, u);

    /* free_usable preserves errno like free, across a cache flush too. */
    static void *many[4000];
    for (int i = 0; i < 4000; i++)
        many[i] = malloc(64);
    errno = 42;
    for (int i = 0; i < 4000; i++)
        CHECK(challoc_free_usable(many[i]) == 64, "free_usable of a 64-byte block #%d", i);
    CHECK(errno == 42, "free_usable changed errno to %d", errno);
}

static void test_other_makers(void)
{
    /* Blocks from every allocating entry point report their class. */
    void *a = aligned_alloc(4096, 5 * 1024);
    size_t au = malloc_usable_size(a); /* the 8 KiB class: 5 KiB rounded to a multiple of the alignment */
    CHECK(a != NULL && au == 8192 && challoc_free_usable(a) == au, "aligned_alloc block: usable %zu", au);
    void *c = calloc(3, 100);
    size_t cu = malloc_usable_size(c);
    CHECK(challoc_free_usable(c) == cu, "calloc block");
    if (mallocx) {
        void *m = mallocx(700, MALLOCX_LG_ALIGN(6)); /* 64-byte alignment */
        size_t mu = sallocx(m, 0);
        CHECK(challoc_free_usable(m) == mu, "mallocx block");
    }
    void *h = malloc(3u << 20);
    size_t hu = malloc_usable_size(h);
    CHECK(hu >= (3u << 20) && challoc_free_usable(h) == hu, "huge run: %zu", hu);
    /* And the other way round: malloc_usable blocks freed by every free. */
    size_t u;
    void *p1 = challoc_malloc_usable(200, &u);
    free(p1);
    void *p2 = challoc_malloc_usable(200, &u);
    if (sdallocx)
        sdallocx(p2, u, 0);
    else
        free(p2);
    void *p3 = challoc_malloc_usable(200, &u);
    if (free_sized)
        free_sized(p3, 200);
    else
        free(p3);
}

/* ---- threads: free_usable of another thread's blocks ---------------------- */

#define PER_THREAD 1000
struct worker {
    void *mine[PER_THREAD];
    size_t usable[PER_THREAD];
    void *from_main[PER_THREAD];
    size_t from_main_usable[PER_THREAD];
};

static void *worker(void *arg)
{
    struct worker *w = arg;
    for (int i = 0; i < PER_THREAD; i++) {
        size_t n = 16 + (size_t)(i % 37) * 24;
        w->mine[i] = challoc_malloc_usable(n, &w->usable[i]);
        CHECK(w->mine[i] != NULL && w->usable[i] == malloc_usable_size(w->mine[i]), "worker malloc_usable(%zu)", n);
        memset(w->mine[i], i & 0xff, w->usable[i]);
    }
    for (int i = 0; i < PER_THREAD; i++) /* main's blocks, freed here */
        CHECK(challoc_free_usable(w->from_main[i]) == w->from_main_usable[i], "cross-thread free_usable #%d", i);
    return NULL;
}

static void test_threads(void)
{
    enum { N = 4 };
    pthread_t t[N];
    static struct worker w[N];
    for (int i = 0; i < N; i++)
        for (int k = 0; k < PER_THREAD; k++) {
            w[i].from_main[k] = challoc_malloc_usable(32 + (size_t)(k % 5) * 16, &w[i].from_main_usable[k]);
            CHECK(w[i].from_main[k] != NULL, "main malloc_usable");
        }
    for (int i = 0; i < N; i++)
        CHECK(pthread_create(&t[i], NULL, worker, &w[i]) == 0, "pthread_create");
    for (int i = 0; i < N; i++)
        pthread_join(t[i], NULL);
    for (int i = 0; i < N; i++)
        for (int k = 0; k < PER_THREAD; k++) {
            unsigned char *p = w[i].mine[k];
            CHECK(p[0] == (unsigned char)(k & 0xff), "thread %d block %d content", i, k);
            CHECK(challoc_free_usable(p) == w[i].usable[k], "main frees thread %d block %d", i, k);
        }
}

int main(void)
{
    require_challoc();
    test_tiers();
    test_null_and_errors();
    test_other_makers();
    test_threads();
    return finish("usable_api");
}
