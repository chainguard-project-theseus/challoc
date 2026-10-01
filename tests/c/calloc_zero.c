/*
 * calloc zeroing: fresh slots, slots reused straight from the thread cache
 * (LIFO, dirtied just before), slots returned to the slab by another
 * thread, slots reclaimed from an exited thread's cache, and huge runs that
 * were dirtied and retained. Also the overflow and zero-size rules.
 */
#include "common.h"

static const size_t sizes[] = {1, QUANTUM, 24, 64, 100, 256, 1000, 1024, 1025, 4096,
                               10000, 65536, 262144, 262145, 1 << 20, 4 << 20, 12 << 20};
#define NSIZES (sizeof sizes / sizeof sizes[0])

static void check_zero_block(void *p, size_t n, const char *what)
{
    CHECK(p != NULL, "%s: calloc(%zu) returned NULL", what, n);
    if (!p)
        return;
    CHECK(malloc_usable_size(p) >= n, "%s: usable", what);
    CHECK(is_zero(p, n), "%s: calloc(%zu) not zero", what, n);
}

/* Dirty then free `count` blocks of `n` bytes, then calloc `count` blocks of
 * the same size: with a LIFO cache the callocs get the dirty slots back. */
static void reuse_same_thread(size_t n, int count)
{
    void *ptrs[300];
    REQUIRE(count <= 300, "count");
    for (int i = 0; i < count; i++) {
        ptrs[i] = malloc(n);
        REQUIRE(ptrs[i], "malloc(%zu)", n);
        memset(ptrs[i], 0xFF, n);
    }
    for (int i = 0; i < count; i++)
        free(ptrs[i]);
    for (int i = 0; i < count; i++) {
        ptrs[i] = calloc(1, n);
        check_zero_block(ptrs[i], n, "same-thread reuse");
        if (ptrs[i])
            memset(ptrs[i], 0xAA, n); /* dirty again so a later round has to re-zero */
    }
    for (int i = 0; i < count; i++)
        free(ptrs[i]);
}

/* nmemb * size splits: every product must be fully zeroed. */
static void split_arguments(void)
{
    static const size_t pairs[][2] = {{0, 0}, {0, 100}, {100, 0}, {1, 1}, {3, 7}, {7, 3}, {16, 16},
                                      {1000, 3}, {3, 1000}, {4096, 65}, {1, 1 << 20}, {1 << 20, 1}};
    for (size_t i = 0; i < sizeof pairs / sizeof pairs[0]; i++) {
        size_t a = pairs[i][0], b = pairs[i][1];
        void *p = calloc(a, b);
        check_zero_block(p, a * b, "split arguments");
        free(p);
    }
    void *z1 = calloc(0, 0), *z2 = calloc(0, 10);
    CHECK(z1 && z2 && z1 != z2, "calloc(0, ...) must give unique pointers");
    free(z1);
    free(z2);

    volatile size_t big = (SIZE_MAX >> 1) + 1;
    errno = 0;
    sink = calloc(big, 2);
    CHECK(sink == NULL && errno == ENOMEM, "calloc overflow (2^63 * 2)");
    errno = 0;
    sink = calloc(big, 4);
    CHECK(sink == NULL && errno == ENOMEM, "calloc overflow (2^63 * 4)");
    errno = 0;
    volatile size_t sqrt_max = ((size_t)1 << 32) + 1; /* squares to just over SIZE_MAX */
    sink = calloc(sqrt_max, sqrt_max);
    CHECK(sink == NULL && errno == ENOMEM, "calloc overflow (near sqrt)");
    errno = 0;
    volatile size_t quarter = SIZE_MAX / 4; /* volatile: gcc warns on constant oversize */
    sink = calloc(quarter, 1);
    CHECK(sink == NULL && errno == ENOMEM, "calloc too large without overflow");
}

/* ---- cross-thread: a helper dirties and frees, main callocs ------------ */

struct dirty_job {
    size_t n;
    int count;
    void **out;   /* blocks the helper leaves live (freed by main) */
    int leave;    /* how many to leave live */
};

static void *dirty_and_free(void *arg)
{
    struct dirty_job *j = arg;
    void *ptrs[400];
    REQUIRE(j->count <= 400, "count");
    for (int i = 0; i < j->count; i++) {
        ptrs[i] = malloc(j->n);
        REQUIRE(ptrs[i], "malloc(%zu)", j->n);
        memset(ptrs[i], 0xEE, j->n);
    }
    /* Free most: the cache fills, flushes to the slab, and what is left in
     * the cache is released by the thread-exit destructor. */
    for (int i = 0; i < j->count - j->leave; i++)
        free(ptrs[i]);
    for (int i = 0; i < j->leave; i++)
        j->out[i] = ptrs[j->count - j->leave + i];
    return NULL;
}

static void reuse_from_other_thread(size_t n, int count)
{
    void *left[8];
    struct dirty_job j = {n, count, left, 8};
    pthread_t t;
    REQUIRE(pthread_create(&t, NULL, dirty_and_free, &j) == 0, "pthread_create");
    pthread_join(t, NULL);
    /* Cross-thread free of the leftovers from main, dirty still. */
    for (int i = 0; i < 8; i++)
        free(left[i]);

    void *ptrs[400];
    for (int i = 0; i < count; i++) {
        ptrs[i] = calloc(1, n);
        check_zero_block(ptrs[i], n, "other-thread reuse");
    }
    for (int i = 0; i < count; i++)
        free(ptrs[i]);
}

/* ---- huge: dirty, free (retained below retain_max), calloc again -------- */

static void huge_reuse(void)
{
    static const size_t hs[] = {262145, 1 << 20, 4 << 20, 20 << 20};
    for (size_t i = 0; i < sizeof hs / sizeof hs[0]; i++) {
        size_t n = hs[i];
        for (int rep = 0; rep < 3; rep++) {
            unsigned char *p = malloc(n);
            REQUIRE(p, "malloc(%zu)", n);
            memset(p, 0xCC, n);
            free(p);
            unsigned char *q = calloc(1, n);
            check_zero_block(q, n, "huge reuse");
            /* Touch the very last byte and the first: both must have been zero. */
            if (q) {
                q[0] = 1;
                q[n - 1] = 1;
            }
            free(q);
        }
    }
    /* Two dirty neighbours coalesce into one free extent; a calloc spanning
     * both must still be zero. */
    unsigned char *a = malloc(1 << 20), *b = malloc(1 << 20);
    REQUIRE(a && b, "neighbours");
    memset(a, 1, 1 << 20);
    memset(b, 1, 1 << 20);
    free(a);
    free(b);
    unsigned char *c = calloc(1, (2 << 20) - 4096);
    check_zero_block(c, (2 << 20) - 4096, "coalesced huge");
    free(c);
}

/* ---- calloc under concurrency ------------------------------------------ */

static void *calloc_hammer(void *arg)
{
    uint64_t rng = 0xC0FFEE ^ (uintptr_t)arg;
    void *ring[128] = {0};
    for (int i = 0; i < 20000; i++) {
        int k = (int)(rng_next(&rng) % 128);
        free(ring[k]);
        size_t n = (size_t)(rng_next(&rng) % 3000) + 1;
        if (i % 97 == 0)
            n = 262145 + (size_t)(rng_next(&rng) % 100000);
        unsigned char *p = calloc(1, n);
        CHECK(p && is_zero(p, n), "hammer calloc(%zu) not zero", n);
        if (p)
            memset(p, 0x5A, n);
        ring[k] = p;
    }
    for (int k = 0; k < 128; k++)
        free(ring[k]);
    return NULL;
}

int main(void)
{
    require_challoc();

    for (size_t i = 0; i < NSIZES; i++) {
        void *p = calloc(1, sizes[i]);
        check_zero_block(p, sizes[i], "fresh");
        free(p);
    }
    split_arguments();

    for (size_t i = 0; i < NSIZES; i++) {
        if (sizes[i] > 262144)
            continue; /* huge sizes have their own section */
        int count = sizes[i] <= 128 ? 300 : sizes[i] <= 4096 ? 100 : 20;
        for (int rep = 0; rep < 3; rep++)
            reuse_same_thread(sizes[i], count);
        reuse_from_other_thread(sizes[i], count);
    }
    huge_reuse();

    pthread_t t[8];
    for (int i = 0; i < 8; i++)
        REQUIRE(pthread_create(&t[i], NULL, calloc_hammer, (void *)(uintptr_t)(i + 1)) == 0, "pthread_create");
    for (int i = 0; i < 8; i++)
        pthread_join(t[i], NULL);

    return finish("calloc_zero");
}
