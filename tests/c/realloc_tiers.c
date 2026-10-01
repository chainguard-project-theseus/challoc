/*
 * realloc growth and shrink across the small, medium and huge tiers:
 * content is preserved up to min(old, new), the pointer is kept when the
 * new size fits the same class or the same number of huge pages, and
 * realloc(NULL, n) / realloc(p, 0) behave like malloc / free. A randomised
 * walk over many blocks checks the same properties under interleaving, and
 * a thread pool does it concurrently.
 */
#include "common.h"

/* Sizes that straddle the tier boundaries (SMALL_MAX = 1 KiB,
 * MEDIUM_MAX = 256 KiB) and the biggest huge sizes worth touching. */
static const size_t chain[] = {
    1,       QUANTUM,   QUANTUM + 1, 100,      1023,     1024,     1025,       2000,
    4095,    4096,      4097,        65536,    100000,   262143,   262144,     262145,
    300000,  1 << 20,   (1 << 20) + 1, 4 << 20, 16 << 20, 4 << 20,  (1 << 20) + 1, 1 << 20,
    300000,  262145,    262144,      262143,   100000,   65536,    4097,       4096,
    4095,    2000,      1025,        1024,     1023,     100,      QUANTUM + 1, QUANTUM,
    1,
};
#define NCHAIN (sizeof chain / sizeof chain[0])

static void walk_chain(void)
{
    uint32_t seed = 77;
    unsigned char *p = realloc(NULL, chain[0]);
    REQUIRE(p != NULL, "realloc(NULL, %zu)", chain[0]);
    CHECK(malloc_usable_size(p) >= chain[0], "realloc(NULL, n) usable");
    fill(p, chain[0], seed);
    size_t prev = chain[0];
    for (size_t i = 1; i < NCHAIN; i++) {
        size_t n = chain[i];
        unsigned char *q = realloc(p, n);
        REQUIRE(q != NULL, "realloc to %zu", n);
        CHECK(((uintptr_t)q % QUANTUM) == 0, "realloc result misaligned");
        CHECK(malloc_usable_size(q) >= n, "usable %zu < %zu", malloc_usable_size(q), n);
        CHECK(check_fill(q, prev < n ? prev : n, seed), "content lost going %zu -> %zu", prev, n);
        /* Refill with the new size so the tail is covered too. */
        fill(q, n, ++seed);
        p = q;
        prev = n;
    }
    free(p);
}

static void same_class_keeps_pointer(void)
{
    /* Within one size class the pointer must not move. */
    static const size_t bases[] = {1, 17, 100, 1000, 5000, 100000};
    for (size_t i = 0; i < sizeof bases / sizeof bases[0]; i++) {
        size_t n = bases[i];
        unsigned char *p = malloc(n);
        REQUIRE(p, "malloc(%zu)", n);
        size_t usable = malloc_usable_size(p);
        fill(p, n, 5);
        /* Compare as integers: gcc's -Wuse-after-free rejects touching `p`
         * after it went through realloc, even just to compare it. */
        uintptr_t before = (uintptr_t)p;
        unsigned char *q = realloc(p, usable);
        CHECK((uintptr_t)q == before, "realloc(%zu -> usable %zu) moved %#zx -> %p", n, usable, before, (void *)q);
        q = realloc(q, n);
        CHECK((uintptr_t)q == before, "realloc back to %zu moved", n);
        CHECK(check_fill(q, n, 5), "content after same-class reallocs");
        /* One byte over the usable size must move to a bigger class. */
        unsigned char *r = realloc(q, usable + 1);
        REQUIRE(r, "grow past usable");
        CHECK(malloc_usable_size(r) > usable, "grow past usable did not grow");
        CHECK(check_fill(r, n, 5), "content after growth");
        free(r);
    }

    /* Huge: same page count keeps the pointer, a different count copies. */
    long page = sysconf(_SC_PAGESIZE);
    size_t big = 3 << 20;
    unsigned char *h = malloc(big);
    REQUIRE(h, "malloc(3 MiB)");
    fill(h, big, 9);
    uintptr_t h_addr = (uintptr_t)h;
    unsigned char *h2 = realloc(h, big - (size_t)page / 2);
    CHECK((uintptr_t)h2 == h_addr, "huge shrink inside the same page count moved");
    CHECK(check_fill(h2, big - (size_t)page / 2, 9), "huge same-pages content");
    unsigned char *h3 = realloc(h2, big + 2 * (size_t)page);
    REQUIRE(h3, "huge grow");
    CHECK(check_fill(h3, big - (size_t)page / 2, 9), "huge grow content");
    fill(h3, big + 2 * (size_t)page, 10);
    unsigned char *h4 = realloc(h3, (size_t)page);
    REQUIRE(h4, "huge shrink to one page");
    CHECK(check_fill(h4, (size_t)page, 10), "huge shrink content");
    free(h4);
}

static void edge_cases(void)
{
    /* realloc(p, 0) frees and returns NULL (glibc behaviour). */
    void *p = malloc(100);
    REQUIRE(p, "malloc");
    sink = realloc(p, 0);
    CHECK(sink == NULL, "realloc(p, 0) returned %p", sink);

    /* realloc(NULL, 0) is malloc(0): a unique non-null pointer. */
    void *a = realloc(NULL, 0), *b = realloc(NULL, 0);
    CHECK(a && b && a != b, "realloc(NULL, 0) must behave like malloc(0)");
    free(a);
    free(b);

    /* Failure leaves the old block intact and sets ENOMEM. */
    unsigned char *keep = malloc(500);
    REQUIRE(keep, "malloc(500)");
    fill(keep, 500, 3);
    /* A failed realloc leaves the block valid; go through a volatile so
     * gcc does not treat the later uses as use-after-free. */
    unsigned char *volatile keep_v = keep;
    volatile size_t too_big = SIZE_MAX / 2;
    errno = 0;
    sink = realloc(keep, too_big);
    CHECK(sink == NULL && errno == ENOMEM, "realloc to SIZE_MAX/2 must fail with ENOMEM");
    CHECK(check_fill(keep_v, 500, 3), "failed realloc damaged the old block");
    free(keep_v);

    /* reallocarray overflow and success. */
    errno = 0;
    volatile size_t half = (SIZE_MAX >> 1) + 1;
    sink = reallocarray(NULL, half, 2);
    CHECK(sink == NULL && errno == ENOMEM, "reallocarray overflow");
    unsigned char *r = reallocarray(NULL, 100, 30);
    REQUIRE(r, "reallocarray(100, 30)");
    fill(r, 3000, 4);
    r = reallocarray(r, 200, 30);
    REQUIRE(r, "reallocarray grow");
    CHECK(check_fill(r, 3000, 4), "reallocarray content");
    free(r);
}

/* Randomised walk: many live blocks, each realloc'd to a random size in a
 * random tier, verified before and after. */
#define NBLOCKS 256

struct blk {
    unsigned char *p;
    size_t n;
    uint32_t seed;
};

static size_t random_size(uint64_t *rng)
{
    uint64_t r = rng_next(rng);
    switch (r % 10) {
    case 0: case 1: case 2: case 3: return (size_t)(r >> 8) % 64 + 1;            /* tiny */
    case 4: case 5: case 6:         return (size_t)(r >> 8) % 1024 + 1;          /* small */
    case 7: case 8:                 return (size_t)(r >> 8) % (256 * 1024) + 1;  /* medium */
    default:                        return 256 * 1024 + (size_t)(r >> 8) % (2 << 20); /* huge */
    }
}

static void random_walk(uint64_t seed, int iters)
{
    struct blk blks[NBLOCKS];
    memset(blks, 0, sizeof blks);
    uint64_t rng = seed;
    for (int it = 0; it < iters; it++) {
        struct blk *b = &blks[rng_next(&rng) % NBLOCKS];
        if (b->p)
            CHECK(check_fill(b->p, b->n, b->seed), "block corrupted before realloc (size %zu)", b->n);
        size_t n = random_size(&rng);
        unsigned char *q = realloc(b->p, n);
        REQUIRE(q, "realloc(%zu)", n);
        CHECK(malloc_usable_size(q) >= n, "usable");
        if (b->p)
            CHECK(check_fill(q, b->n < n ? b->n : n, b->seed), "content lost %zu -> %zu", b->n, n);
        b->p = q;
        b->n = n;
        b->seed = (uint32_t)rng_next(&rng);
        fill(q, n, b->seed);
    }
    for (int i = 0; i < NBLOCKS; i++)
        if (blks[i].p) {
            CHECK(check_fill(blks[i].p, blks[i].n, blks[i].seed), "final content");
            free(blks[i].p);
        }
}

static void *thread_body(void *arg)
{
    random_walk(0x9E3779B97F4A7C15ULL ^ (uintptr_t)arg, 4000);
    return NULL;
}

int main(void)
{
    require_challoc();
    walk_chain();
    same_class_keeps_pointer();
    edge_cases();
    random_walk(12345, 20000);

    pthread_t t[8];
    for (int i = 0; i < 8; i++)
        REQUIRE(pthread_create(&t[i], NULL, thread_body, (void *)(uintptr_t)(i + 1)) == 0, "pthread_create");
    for (int i = 0; i < 8; i++)
        pthread_join(t[i], NULL);

    return finish("realloc_tiers");
}
