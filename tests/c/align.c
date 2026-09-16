/*
 * Alignment for every power of two from 1 to 64 KiB through aligned_alloc,
 * posix_memalign and memalign, plus valloc and pvalloc. Every block is
 * filled before any is verified or freed so overlapping blocks are caught,
 * and the same run repeats three times so blocks that come back from the
 * thread cache are tested as well as fresh ones.
 */
#include "common.h"

#define MAX_ALIGN (64 * 1024)
#define NREQ 8
#define NLIVE 512

struct live {
    void *p;
    size_t n;
    uint32_t seed;
};

static struct live live[NLIVE];
static size_t nlive;

static void remember(void *p, size_t n, uint32_t seed)
{
    REQUIRE(nlive < NLIVE, "live table full");
    fill(p, n, seed);
    live[nlive].p = p;
    live[nlive].n = n;
    live[nlive].seed = seed;
    nlive++;
}

static void verify_and_free_all(void)
{
    for (size_t i = 0; i < nlive; i++)
        CHECK(check_fill(live[i].p, live[i].n, live[i].seed), "block %zu (%zu bytes) corrupted", i, live[i].n);
    for (size_t i = 0; i < nlive; i++)
        free(live[i].p);
    nlive = 0;
}

static size_t request_size(size_t align, int k)
{
    switch (k) {
    case 0: return 1;
    case 1: return align > 1 ? align - 1 : 1;
    case 2: return align;
    case 3: return align + 1;
    case 4: return 3 * align + 5;
    case 5: return 1000;
    case 6: return 300000; /* above MEDIUM_MAX: huge tier */
    default: return (5u << 20) + 13;
    }
}

static void one_round(uint32_t round)
{
    uint32_t seed = round * 1000u;
    for (size_t align = 1; align <= MAX_ALIGN; align <<= 1) {
        for (int k = 0; k < NREQ; k++) {
            size_t n = request_size(align, k);
            /* Multi-MiB requests only for a few alignments; they dominate run time. */
            if (n > (1u << 20) && align != 1 && align != 4096 && align != MAX_ALIGN)
                continue;

            void *a = aligned_alloc(align, n);
            CHECK(a != NULL, "aligned_alloc(%zu, %zu)", align, n);
            CHECK(((uintptr_t)a % align) == 0, "aligned_alloc(%zu, %zu) = %p misaligned", align, n, a);
            CHECK(((uintptr_t)a % QUANTUM) == 0, "aligned_alloc result not quantum aligned");
            CHECK(malloc_usable_size(a) >= n, "aligned_alloc(%zu, %zu) usable %zu", align, n, malloc_usable_size(a));
            remember(a, n, seed++);

            void *m = memalign(align, n);
            CHECK(m != NULL && ((uintptr_t)m % align) == 0, "memalign(%zu, %zu) = %p", align, n, m);
            CHECK(malloc_usable_size(m) >= n, "memalign usable");
            remember(m, n, seed++);

            if (align % sizeof(void *) == 0) {
                void *q = NULL;
                int rc = posix_memalign(&q, align, n);
                CHECK(rc == 0 && q != NULL && ((uintptr_t)q % align) == 0,
                      "posix_memalign(%zu, %zu) rc=%d p=%p", align, n, rc, q);
                CHECK(malloc_usable_size(q) >= n, "posix_memalign usable");
                remember(q, n, seed++);
            } else {
                void *q = (void *)1;
                CHECK(posix_memalign(&q, align, n) == EINVAL && q == (void *)1,
                      "posix_memalign(%zu) below sizeof(void *) must be EINVAL", align);
            }
        }
        verify_and_free_all();
    }

    long page = sysconf(_SC_PAGESIZE);
    for (size_t n = 1; n <= 3 * (size_t)page; n += (size_t)page / 2) {
        void *v = valloc(n);
        CHECK(v && ((uintptr_t)v % (uintptr_t)page) == 0, "valloc(%zu) = %p", n, v);
        CHECK(malloc_usable_size(v) >= n, "valloc usable");
        remember(v, n, seed++);
        void *pv = pvalloc(n);
        size_t rounded = ((n + (size_t)page - 1) / (size_t)page) * (size_t)page;
        CHECK(pv && ((uintptr_t)pv % (uintptr_t)page) == 0, "pvalloc(%zu) = %p", n, pv);
        CHECK(malloc_usable_size(pv) >= rounded, "pvalloc(%zu) usable %zu < %zu", n, malloc_usable_size(pv), rounded);
        remember(pv, rounded, seed++);
    }
    verify_and_free_all();
}

static void bad_arguments(void)
{
    static const size_t bad[] = {0, 3, 6, 12, 24, 1000, 65535};
    for (size_t i = 0; i < sizeof bad / sizeof bad[0]; i++) {
        errno = 0;
        sink = aligned_alloc(bad[i], 64);
        CHECK(sink == NULL && errno == EINVAL, "aligned_alloc(%zu) must fail with EINVAL", bad[i]);
        errno = 0;
        sink = memalign(bad[i], 64);
        CHECK(sink == NULL && errno == EINVAL, "memalign(%zu) must fail with EINVAL", bad[i]);
        void *q = (void *)1;
        errno = 0;
        CHECK(posix_memalign(&q, bad[i], 64) == EINVAL && errno == 0 && q == (void *)1,
              "posix_memalign(%zu) must return EINVAL and leave errno", bad[i]);
    }
    /* Size 0 with a valid alignment must still give a unique, aligned block. */
    void *a = aligned_alloc(4096, 0), *b = aligned_alloc(4096, 0);
    CHECK(a && b && a != b && ((uintptr_t)a % 4096) == 0 && ((uintptr_t)b % 4096) == 0, "aligned_alloc(4096, 0)");
    free(a);
    free(b);
}

/* Several threads doing the same alignment sweep at once so the class
 * chosen for an alignment is exercised under contention too. */
static void *thread_sweep(void *arg)
{
    uint32_t id = (uint32_t)(uintptr_t)arg;
    void *ptrs[17 * 3];
    for (int rep = 0; rep < 20; rep++) {
        int n = 0;
        for (size_t align = 1; align <= MAX_ALIGN; align <<= 1) {
            size_t sz = align + (size_t)rep * 3 + id;
            void *a = aligned_alloc(align, sz);
            void *m = memalign(align, sz / 2 + 1);
            void *q = NULL;
            int rc = align % sizeof(void *) == 0 ? posix_memalign(&q, align, sz * 2) : 0;
            if (align % sizeof(void *) != 0)
                q = malloc(sz);
            CHECK(a && m && q && rc == 0, "thread %u align %zu", id, align);
            CHECK(((uintptr_t)a % align) == 0 && ((uintptr_t)m % align) == 0 && ((uintptr_t)q % align) == 0,
                  "thread %u align %zu misaligned", id, align);
            fill(a, sz, id + 1);
            fill(m, sz / 2 + 1, id + 2);
            fill(q, align % sizeof(void *) == 0 ? sz * 2 : sz, id + 3);
            ptrs[n++] = a;
            ptrs[n++] = m;
            ptrs[n++] = q;
        }
        for (int i = 0; i < n; i++)
            free(ptrs[i]);
    }
    return NULL;
}

int main(void)
{
    require_challoc();
    for (uint32_t round = 0; round < 3; round++)
        one_round(round);
    bad_arguments();

    pthread_t t[4];
    for (int i = 0; i < 4; i++)
        REQUIRE(pthread_create(&t[i], NULL, thread_sweep, (void *)(uintptr_t)i) == 0, "pthread_create");
    for (int i = 0; i < 4; i++)
        pthread_join(t[i], NULL);

    return finish("align");
}
