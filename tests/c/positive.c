/*
 * Positive paths of the API that no other program reached from C. One mode per process, driven by src/api/ctests.rs:
 *
 *   positive free_sized       C23 free_sized / free_aligned_sized over
 *                             every tier (malloc, calloc, realloc and
 *                             aligned_alloc memory), the freed slots come
 *                             back from malloc, the thread cache flushes
 *                             clean (`thread.tcache.flush`), and
 *                             free_sized(NULL, n) is a no-op
 *   positive xallocx_extra    xallocx with `extra > 0`: the block never
 *                             moves, its bytes stay, the return is the real
 *                             usable size (never less than the request it
 *                             already satisfies), on a slab slot and on a
 *                             huge run
 *   positive malloc_trim_idle malloc_trim with nothing purgeable returns 0
 *                             and leaves every live block intact; after
 *                             the working set is freed it returns 1 and
 *                             the RSS falls; the trim right after returns 0
 */
#include "common.h"

static const size_t tiers[] = { 1, QUANTUM, 24, 64, 100, 1000, 1024, 1025, 4096, 65536, 262144, 262145, 1u << 20, 3u << 20 };
#define NTIERS (sizeof tiers / sizeof tiers[0])

static void mode_free_sized(void)
{
    REQUIRE(free_sized != NULL && free_aligned_sized != NULL, "free_sized not exported");
    REQUIRE(mallctl != NULL, "mallctl not exported");
    for (int round = 0; round < 3; round++) {
        void *held[NTIERS][4];
        for (size_t i = 0; i < NTIERS; i++) {
            size_t n = tiers[i];
            held[i][0] = malloc(n);
            held[i][1] = calloc(1, n);
            held[i][2] = realloc(malloc(n / 2 + 1), n);
            held[i][3] = aligned_alloc(64, n);
            for (int k = 0; k < 4; k++) {
                REQUIRE(held[i][k] != NULL, "alloc(%zu) #%d", n, k);
                CHECK(malloc_usable_size(held[i][k]) >= n, "usable(%zu) #%d", n, k);
                if (k == 1)
                    CHECK(is_zero(held[i][k], n), "calloc(%zu) not zero", n);
                fill(held[i][k], n, (uint32_t)(i * 4 + k));
            }
        }
        for (size_t i = 0; i < NTIERS; i++) {
            size_t n = tiers[i];
            for (int k = 0; k < 4; k++)
                CHECK(check_fill(held[i][k], n, (uint32_t)(i * 4 + k)), "block %zu #%d corrupted", n, k);
            free_sized(held[i][0], n);
            free_sized(held[i][1], n);
            free_sized(held[i][2], n);
            free_aligned_sized(held[i][3], 64, n);
        }
        /* The sized path defers the bit test to the flush: force it now. */
        CHECK(mallctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "thread.tcache.flush");
        /* The slots are back: the same sizes allocate again and are usable. */
        for (size_t i = 0; i < NTIERS; i++) {
            void *p = malloc(tiers[i]);
            REQUIRE(p != NULL, "malloc(%zu) after free_sized", tiers[i]);
            memset(p, 0xEE, tiers[i]);
            free(p);
        }
    }
    free_sized(NULL, 64);
    free_aligned_sized(NULL, 64, 64);
    printf("positive free_sized: %zu tiers x 4 flavours x 3 rounds\n", NTIERS);
}

static void mode_xallocx_extra(void)
{
    REQUIRE(mallocx != NULL && xallocx != NULL && sallocx != NULL && dallocx != NULL, "*allocx not exported");
    size_t sizes[] = { 100, 1000, 4096, 1u << 20 };
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        size_t n = sizes[i];
        unsigned char *p = mallocx(n, 0);
        REQUIRE(p != NULL, "mallocx(%zu)", n);
        size_t usable = sallocx(p, 0);
        CHECK(usable >= n, "sallocx %zu < %zu", usable, n);
        fill(p, usable, 7);
        unsigned char *before = p;
        /* Ask for the same size with room to grow: the answer is the real
         * size, at least the request, and the block stays put and intact. */
        size_t r = xallocx(p, n, usable - n + 64, 0);
        CHECK(r == usable, "xallocx(%zu, extra) reports %zu, sallocx %zu", n, r, usable);
        CHECK(r >= n, "xallocx shrank below the request: %zu < %zu", r, n);
        CHECK(p == before, "xallocx moved the block");
        CHECK(check_fill(p, usable, 7), "xallocx(%zu) changed the bytes", n);
        /* A request the block already satisfies, with extra: unchanged. */
        r = xallocx(p, 1, n, 0);
        CHECK(r == usable, "xallocx(1, extra %zu) reports %zu", n, r);
        /* A request beyond the block: an allocator that cannot grow in
         * place reports the current size, which is below the request, and
         * the block is still whole (jemalloc's contract: nothing moved). */
        r = xallocx(p, usable + 1, 4096, 0);
        CHECK(r == usable, "xallocx(usable + 1, 4096) reports %zu", r);
        CHECK(check_fill(p, usable, 7), "a refused xallocx changed the bytes");
        CHECK(sallocx(p, 0) == usable, "sallocx moved after xallocx");
        dallocx(p, 0);
    }
    printf("positive xallocx_extra: 4 sizes, in place, unchanged\n");
}

enum { TRIM_N = 16384, TRIM_SIZE = 1024, TRIM_HUGE = 4u << 20 };
#define MIB (1024UL * 1024UL)

static void mode_malloc_trim_idle(void)
{
    /* A fresh process, once `require_challoc`'s probe slot has been purged
     * (its slab went Empty when the cache flushed): nothing is freed, so
     * nothing is purgeable. */
    int r0 = malloc_trim(0);
    int r0b = malloc_trim(0);
    CHECK(r0b == 0, "malloc_trim on an idle fresh heap returned %d", r0b);

    void **blocks = malloc(TRIM_N * sizeof *blocks);
    REQUIRE(blocks != NULL, "pointer array");
    for (int i = 0; i < TRIM_N; i++) {
        blocks[i] = malloc(TRIM_SIZE);
        REQUIRE(blocks[i] != NULL, "malloc #%d", i);
        fill(blocks[i], TRIM_SIZE, (uint32_t)i);
    }
    unsigned char *huge = malloc(TRIM_HUGE);
    REQUIRE(huge != NULL, "malloc(huge)");
    fill(huge, TRIM_HUGE, 99);
    size_t rss_live = rss_bytes();
    /* stdio's FILE and its buffer (rss_bytes opens /proc/self/statm) are
     * allocations too, freed at fclose: one trim takes their slabs. */
    malloc_trim(0);

    /* Everything is live: a trim has nothing to purge and touches nothing. */
    for (int k = 0; k < 3; k++) {
        int r = malloc_trim(0);
        CHECK(r == 0, "malloc_trim #%d with everything live returned %d", k, r);
    }
    for (int i = 0; i < TRIM_N; i++)
        CHECK(check_fill(blocks[i], TRIM_SIZE, (uint32_t)i), "block %d changed across malloc_trim", i);
    CHECK(check_fill(huge, TRIM_HUGE, 99), "the huge run changed across malloc_trim");
    CHECK(rss_bytes() + MIB >= rss_live, "RSS fell across a trim with everything live: %zu -> %zu", rss_live, rss_bytes());

    /* Free the set: the first trim purges (returns 1, RSS falls by most of
     * the 20 MiB), the next has nothing left (0). */
    for (int i = 0; i < TRIM_N; i++)
        free(blocks[i]);
    free(huge);
    int r1 = malloc_trim(0);
    CHECK(r1 == 1, "malloc_trim after the frees returned %d", r1);
    size_t rss_after = rss_bytes();
    malloc_trim(0); /* the stdio slabs of rss_bytes again */
    size_t set = (size_t)TRIM_N * TRIM_SIZE + TRIM_HUGE;
    /* RSS falls only when the purge advice is MADV_DONTNEED; under a cgroup
     * limit in the policy's headroom band it is MADV_FREE and the pages
     * stay resident until the kernel wants them (tests/c/cgroup_lazy.c). */
    size_t advice = 0, len = sizeof advice;
    int lazy = mallctl != NULL && mallctl("stats.cgroup.purge_advice", &advice, &len, NULL, 0) == 0 && advice == 1;
    if (lazy)
        printf("positive malloc_trim_idle: purge advice is MADV_FREE here, RSS not asserted\n");
    else
        CHECK(rss_live > rss_after && rss_live - rss_after >= set / 2, "RSS %zu -> %zu after trimming %zu bytes", rss_live,
              rss_after, set);
    int r2 = malloc_trim(0);
    CHECK(r2 == 0, "malloc_trim right after a trim returned %d", r2);
    /* The heap still works. */
    for (int i = 0; i < TRIM_N; i++) {
        blocks[i] = malloc(TRIM_SIZE);
        REQUIRE(blocks[i] != NULL, "malloc #%d after trim", i);
        memset(blocks[i], 1, TRIM_SIZE);
    }
    for (int i = 0; i < TRIM_N; i++)
        free(blocks[i]);
    free(blocks);
    printf("positive malloc_trim_idle: fresh %d then %d, live 0, freed %d, again %d; RSS %zu -> %zu MiB\n", r0, r0b, r1, r2,
           rss_live / MIB, rss_after / MIB);
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: positive <free_sized|xallocx_extra|malloc_trim_idle>");
    require_challoc();
    const char *mode = argv[1];
    if (strcmp(mode, "free_sized") == 0)
        mode_free_sized();
    else if (strcmp(mode, "xallocx_extra") == 0)
        mode_xallocx_extra();
    else if (strcmp(mode, "malloc_trim_idle") == 0)
        mode_malloc_trim_idle();
    else
        REQUIRE(0, "unknown mode %s", mode);
    return finish("positive");
}
