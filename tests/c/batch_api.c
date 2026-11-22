/*
 * challoc_free_batch from C (proof of concept; <challoc/challoc.h>). Run under
 * LD_PRELOAD=libchalloc.so by src/api/c_programs.rs, under the default
 * configuration, guards:false, and the hardening keys (where the batch
 * frees every pointer through free).
 *
 * Claims: a batch of mixed sizes (slab classes, the classes above the bins,
 * huge runs) and nulls is freed and its return is the sum of
 * malloc_usable_size of its pointers; a short batch (below the threshold)
 * the same; the freed memory is reused by later allocations of the same
 * shape (the slabs got their slots back: RSS stays bounded over rounds);
 * blocks allocated on another thread are freed too. Negative modes, each in
 * a forked child that must die of SIGABRT: a pointer twice in one batch, a
 * pointer freed before (its bit clear), an interior pointer, a stack
 * address.
 */
#include "common.h"
#include <challoc/challoc.h>
#include <pthread.h>
#include <signal.h>

#ifndef CHALLOC_FREE_BATCH
#error "<challoc/challoc.h> must define CHALLOC_FREE_BATCH"
#endif

static const size_t sizes[] = {1, 8, 16, 24, 48, 100, 512, 1000, 4096, 5000, 16384, 65536, 262145, 1 << 20};
#define NSIZES (sizeof sizes / sizeof sizes[0])

static size_t fill_batch(void **v, size_t n, uint64_t *seed)
{
    size_t want = 0;
    for (size_t i = 0; i < n; i++) {
        if (i % 97 == 13) {
            v[i] = NULL;
            continue;
        }
        size_t s = sizes[rng_next(seed) % NSIZES];
        v[i] = malloc(s);
        REQUIRE(v[i] != NULL, "malloc(%zu)", s);
        memset(v[i], 0x5a, s);
        want += malloc_usable_size(v[i]);
    }
    return want;
}

static void test_mixed(void)
{
    uint64_t seed = 42;
    static void *v[20000];
    for (size_t n = 0; n <= 20000; n = n ? n * 4 : 1) {
        size_t want = fill_batch(v, n, &seed);
        size_t got = challoc_free_batch(v, n);
        CHECK(got == want, "batch of %zu: returned %zu, usable sum %zu", n, got, want);
    }
    CHECK(challoc_free_batch(NULL, 0) == 0, "empty batch");
}

/* Twenty rounds of 200k 48-byte blocks freed by batch: the slots go back to
 * their slabs, so the next round reuses them and RSS stays near one round. */
static void test_reuse(void)
{
    enum { N = 200000 };
    static void *v[N];
    size_t rss1 = 0;
    for (int r = 0; r < 20; r++) {
        for (size_t i = 0; i < N; i++) {
            v[i] = malloc(48);
            memset(v[i], r, 48);
        }
        /* Shuffle so the batch spans the slabs in no order. */
        uint64_t seed = (uint64_t)r + 1;
        for (size_t i = N - 1; i > 0; i--) {
            size_t j = rng_next(&seed) % (i + 1);
            void *t = v[i];
            v[i] = v[j];
            v[j] = t;
        }
        CHECK(challoc_free_batch(v, N) == (size_t)N * 48, "round %d", r);
        if (r == 1)
            rss1 = rss_bytes();
    }
    size_t rss = rss_bytes();
    CHECK(rss < rss1 + (8u << 20), "RSS grew from %zu to %zu over the rounds", rss1, rss);
}

static void *alloc_thread(void *arg)
{
    void **v = arg;
    for (size_t i = 0; i < 5000; i++)
        v[i] = malloc(64 + i % 200);
    return NULL;
}

static void test_cross_thread(void)
{
    static void *v[5000];
    pthread_t t;
    REQUIRE(pthread_create(&t, NULL, alloc_thread, v) == 0, "pthread_create");
    pthread_join(t, NULL);
    size_t want = 0;
    for (size_t i = 0; i < 5000; i++)
        want += malloc_usable_size(v[i]);
    CHECK(challoc_free_batch(v, 5000) == want, "cross-thread batch");
}

static void *neg[1000];

static void dup_in_batch(void *arg)
{
    (void)arg;
    for (size_t i = 0; i < 999; i++)
        neg[i] = malloc(64);
    neg[999] = neg[500];
    challoc_free_batch(neg, 1000);
}

static void stale_in_batch(void *arg)
{
    (void)arg;
    for (size_t i = 0; i < 1000; i++)
        neg[i] = malloc(64);
    void *stale = neg[7];
    challoc_free_batch(neg, 500);
    for (size_t i = 0; i < 500; i++)
        neg[i] = malloc(32);
    neg[100] = stale;
    challoc_free_batch(neg, 1000);
}

static void interior_in_batch(void *arg)
{
    (void)arg;
    for (size_t i = 0; i < 1000; i++)
        neg[i] = malloc(64);
    neg[3] = (char *)neg[3] + 16;
    challoc_free_batch(neg, 1000);
}

static void stack_in_batch(void *arg)
{
    (void)arg;
    char local[64];
    for (size_t i = 0; i < 1000; i++)
        neg[i] = malloc(64);
    neg[640] = local;
    challoc_free_batch(neg, 1000);
}

int main(void)
{
    require_challoc();
    test_mixed();
    test_reuse();
    test_cross_thread();
    CHECK(expect_signal(SIGABRT, dup_in_batch, NULL), "a pointer twice in one batch must abort");
    CHECK(expect_signal(SIGABRT, stale_in_batch, NULL), "a freed pointer in a batch must abort");
    CHECK(expect_signal(SIGABRT, interior_in_batch, NULL), "an interior pointer in a batch must abort");
    CHECK(expect_signal(SIGABRT, stack_in_batch, NULL), "a stack address in a batch must abort");
    return finish("batch_api");
}
