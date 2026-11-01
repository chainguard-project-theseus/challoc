/*
 * export_results: every nullable C export hands back what its Rust entry
 * point returned. Under LD_PRELOAD=libchalloc.so, for the libc names and
 * their je_ twins, challoc's size-returning entry points, and jemalloc's
 * mallocx and rallocx with their je_ twins:
 *
 *   - a request no allocator can serve (half the address space, or a
 *     count times a size that overflows) returns NULL, with errno ENOMEM
 *     for the libc and challoc names (posix_memalign returns ENOMEM) and
 *     errno untouched for the *allocx names, as jemalloc's;
 *   - a 64-byte request returns a block.
 *
 * An export that dropped its entry point's block and returned NULL fails
 * the second check. For the libc and challoc names Verus rejects it too
 * (their ensures say a NULL result comes with errno ENOMEM, through
 * vspec::errno::axiom_errno_at_return), given the export body's shape,
 * which api::tests::exports_return_their_callees_result checks; for
 * mallocx and rallocx, whose contract says nothing about errno, only the
 * two tests do. tests/c/cxx_new.cpp checks the C++ nothrow forms.
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

/* Sizes gcc must not see as constants (-Walloc-size-larger-than), and a
 * block it must not see realloc consume (-Wuse-after-free: a failed
 * realloc keeps it). */
static volatile size_t huge_n = SIZE_MAX;
static void *volatile kept;

/* `call` returns NULL and leaves errno at `want`, from 0 before it. */
#define NULL_ERRNO(call, want)                                                  \
    do {                                                                        \
        errno = 0;                                                              \
        sink = (call);                                                          \
        int e_ = errno;                                                         \
        CHECK(sink == NULL && e_ == (want), "%s: got = %p errno %d, want = NULL errno %d", \
              #call, sink, e_, (want));                                         \
    } while (0)

/* `call` returns a block, which is freed. */
#define BLOCK(call)                                                             \
    do {                                                                        \
        sink = (call); /* through sink: gcc must not drop the pair */           \
        CHECK(sink != NULL, "%s: got = NULL, want = a block", #call);           \
        free(sink);                                                             \
    } while (0)

/* A fresh 64-byte block for realloc and rallocx to start from. */
static void *block(void)
{
    void *p = malloc(64);
    REQUIRE(p != NULL, "malloc(64)");
    return p;
}

static void test_libc(void)
{
    size_t big = huge_n / 2, n = huge_n / 2 + 1;

    NULL_ERRNO(malloc(big), ENOMEM);
    NULL_ERRNO(je_malloc(big), ENOMEM);
    BLOCK(malloc(64));
    BLOCK(je_malloc(64));

    NULL_ERRNO(calloc(n, 2), ENOMEM);
    NULL_ERRNO(je_calloc(n, 2), ENOMEM);
    NULL_ERRNO(calloc(1, big), ENOMEM);
    BLOCK(calloc(4, 16));
    BLOCK(je_calloc(4, 16));

    /* A failed realloc keeps the block. */
    kept = block();
    NULL_ERRNO(realloc(kept, big), ENOMEM);
    NULL_ERRNO(je_realloc(kept, big), ENOMEM);
    NULL_ERRNO(reallocarray(kept, n, 2), ENOMEM);
    NULL_ERRNO(realloc(NULL, big), ENOMEM);
    BLOCK(realloc(kept, 128));
    BLOCK(je_realloc(NULL, 64));
    BLOCK(reallocarray(NULL, 4, 16));

    NULL_ERRNO(aligned_alloc(64, big), ENOMEM);
    NULL_ERRNO(je_aligned_alloc(64, big), ENOMEM);
    NULL_ERRNO(aligned_alloc(3, 64), EINVAL);
    NULL_ERRNO(memalign(64, big), ENOMEM);
    NULL_ERRNO(je_memalign(64, big), ENOMEM);
    NULL_ERRNO(memalign(3, 64), EINVAL);
    BLOCK(aligned_alloc(64, 64));
    BLOCK(je_aligned_alloc(64, 64));
    BLOCK(memalign(64, 64));
    BLOCK(je_memalign(64, 64));

    NULL_ERRNO(valloc(big), ENOMEM);
    NULL_ERRNO(je_valloc(big), ENOMEM);
    NULL_ERRNO(pvalloc(big), ENOMEM);
    NULL_ERRNO(je_pvalloc(big), ENOMEM);
    BLOCK(valloc(64));
    BLOCK(je_valloc(64));
    BLOCK(pvalloc(64));
    BLOCK(je_pvalloc(64));

    void *q = NULL;
    errno = 0;
    CHECK(posix_memalign(&q, 64, big) == ENOMEM && errno == ENOMEM, "posix_memalign(64, big): errno %d", errno);
    errno = 0;
    CHECK(je_posix_memalign(&q, 64, big) == ENOMEM && errno == ENOMEM, "je_posix_memalign(64, big): errno %d", errno);
    CHECK(posix_memalign(&q, 64, 64) == 0 && q != NULL, "posix_memalign(64, 64) = %p", q);
    free(q);
    q = NULL;
    CHECK(je_posix_memalign(&q, 64, 64) == 0 && q != NULL, "je_posix_memalign(64, 64) = %p", q);
    free(q);
}

static void test_usable(void)
{
    size_t big = huge_n / 2, n = huge_n / 2 + 1, u = 0;

    NULL_ERRNO(challoc_malloc_usable(big, &u), ENOMEM);
    NULL_ERRNO(challoc_calloc_usable(n, 2, &u), ENOMEM);
    void *p = block();
    NULL_ERRNO(challoc_realloc_usable(p, big, &u), ENOMEM);
    BLOCK(challoc_malloc_usable(64, &u));
    BLOCK(challoc_calloc_usable(4, 16, &u));
    BLOCK(challoc_realloc_usable(p, 128, &u));
}

static void test_allocx(void)
{
    size_t big = huge_n / 2;
    void *p = block();

    /* *allocx leave errno alone on failure: 0 stays 0. */
    NULL_ERRNO(mallocx(big, 0), 0);
    NULL_ERRNO(je_mallocx(big, 0), 0);
    NULL_ERRNO(rallocx(p, big, 0), 0);
    NULL_ERRNO(je_rallocx(p, big, 0), 0);
    NULL_ERRNO(rallocx(NULL, big, 0), 0);
    BLOCK(mallocx(64, 0));
    BLOCK(je_mallocx(64, MALLOCX_ZERO));
    BLOCK(je_rallocx(NULL, 64, 0));
    BLOCK(rallocx(p, 128, 0));
}

int main(void)
{
    require_challoc();
    test_libc();
    test_usable();
    test_allocx();
    return finish("export_results");
}
