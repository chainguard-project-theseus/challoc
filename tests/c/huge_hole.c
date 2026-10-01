/*
 * Alignment holes in the huge region are usable memory.
 *
 * In guard-install mode (a kernel with MADV_GUARD_INSTALL) the huge
 * allocator must commit the hole it leaves between the old high-water mark
 * and an aligned run's start, which it records as a free extent: a hole
 * left PROT_NONE faults on the first write of the first block placed in
 * it. The PROT_NONE-guard fallback mode commits every block's own pages on
 * allocation, so only a kernel with guard regions (6.13+) exercises the
 * guard-install path. The driver in src/api/c_programs.rs runs this
 * program in the probed guard mode and again with MALLOC_CONF=guards:false.
 *
 * Two hole shapes, in this order so the second finds the mark it needs:
 *
 *   1. fresh region: a = malloc(RUN), b = aligned_alloc(8 MiB, RUN). The
 *      mark sits RUN + one guard page above a 4 MiB-aligned base, so the
 *      hole below b is at least about 3.7 MiB and takes several RUN-sized
 *      fillers, each written and read back.
 *   2. the region below the mark is full apart from a tail too small for a
 *      RUN: e = malloc(RUN), f = malloc(RUN) land at the mark; free(f)
 *      leaves an Empty extent touching the mark; b2 = aligned_alloc(8 MiB,
 *      RUN) is placed above it and the hole is folded into that extent.
 *      Fillers land at f's old address first, then in the hole.
 *
 * Any filler that lands between the previous mark and the aligned run is
 * in a hole; the test needs at least one per shape and writes all of them.
 */
#include "common.h"

enum { RUN = 300 * 1024, ALIGN = 8 << 20, MAX_FILL = 128 };

static unsigned char *held[MAX_FILL];
static int nheld;

/* Allocate RUN-sized blocks until one lands above `top`; fill each one
 * that lands in (lo, top) and count them. The block above `top` is freed.
 * Bounds are integers: gcc's -Wuse-after-free rejects a pointer derived
 * from a freed block even when it is only compared. */
static int fill_below(uintptr_t lo, uintptr_t top, uint32_t seed)
{
    int in_hole = 0;
    for (;;) {
        unsigned char *c = malloc(RUN);
        REQUIRE(c != NULL, "malloc(%d)", RUN);
        if ((uintptr_t)c > top) {
            free(c);
            break;
        }
        REQUIRE(nheld < MAX_FILL, "more than %d fillers", MAX_FILL);
        CHECK(malloc_usable_size(c) >= RUN, "usable %zu < %d", malloc_usable_size(c), RUN);
        if ((uintptr_t)c > lo)
            in_hole++;
        fill(c, RUN, seed + (uint32_t)nheld); /* faults if the hole is PROT_NONE */
        held[nheld++] = c;
    }
    return in_hole;
}

static void check_and_free_all(uint32_t seed)
{
    for (int i = 0; i < nheld; i++)
        CHECK(check_fill(held[i], RUN, seed + (uint32_t)i), "filler %d corrupted", i);
    for (int i = 0; i < nheld; i++)
        free(held[i]);
    nheld = 0;
}

static void hole_above_a_fresh_mark(void)
{
    unsigned char *a = malloc(RUN);
    REQUIRE(a != NULL, "malloc(a)");
    fill(a, RUN, 1);
    unsigned char *b = aligned_alloc(ALIGN, RUN);
    REQUIRE(b != NULL, "aligned_alloc(%d, %d)", ALIGN, RUN);
    CHECK((uintptr_t)b % ALIGN == 0, "b %p not %d-aligned", (void *)b, ALIGN);
    CHECK(b > a, "aligned run %p below the first run %p", (void *)b, (void *)a);
    fill(b, RUN, 2);

    int in_hole = fill_below((uintptr_t)a, (uintptr_t)b, 100);
    CHECK(in_hole >= 1, "no filler landed in the hole below b (%d fillers)", in_hole);
    printf("shape 1: %d fillers in the hole below %p\n", in_hole, (void *)b);

    CHECK(check_fill(a, RUN, 1), "a corrupted");
    CHECK(check_fill(b, RUN, 2), "b corrupted");
    check_and_free_all(100);
    /* Keep a and b so the space below the mark stays occupied for shape 2
     * apart from the hole, which the fillers just proved usable and which
     * is now one free extent again, below b's run and away from the mark. */
    held[nheld++] = a;
    held[nheld++] = b;
}

static void hole_folded_into_an_empty_extent(void)
{
    /* Fill the old hole again so nothing free is left below the mark but
     * a tail smaller than a RUN, then add two runs at the mark. */
    unsigned char *top = held[nheld - 1];
    (void)fill_below((uintptr_t)top, (uintptr_t)top, 200);
    unsigned char *e = malloc(RUN);
    unsigned char *f = malloc(RUN);
    REQUIRE(e != NULL && f != NULL, "malloc(e), malloc(f)");
    CHECK(e > top && f > e, "e %p f %p not at the mark above %p", (void *)e, (void *)f, (void *)top);
    fill(e, RUN, 3);
    fill(f, RUN, 4);
    uintptr_t f_old = (uintptr_t)f;
    free(f); /* Empty extent touching the mark (under huge_retain_max) */

    unsigned char *b2 = aligned_alloc(ALIGN, RUN);
    REQUIRE(b2 != NULL, "aligned_alloc(%d, %d)", ALIGN, RUN);
    CHECK((uintptr_t)b2 % ALIGN == 0, "b2 %p not %d-aligned", (void *)b2, ALIGN);
    CHECK((uintptr_t)b2 > f_old, "aligned run %p below the freed run %#zx", (void *)b2, (size_t)f_old);
    fill(b2, RUN, 5);

    int first = nheld;
    int in_hole = fill_below(f_old, (uintptr_t)b2, 300);
    CHECK(nheld > first && (uintptr_t)held[first] == f_old, "first filler %p did not reuse the freed run %#zx",
          nheld > first ? (void *)held[first] : NULL, (size_t)f_old);
    CHECK(in_hole >= 1, "no filler landed in the hole between %#zx and %p (%d fillers)", (size_t)f_old, (void *)b2, in_hole);
    printf("shape 2: %d fillers in the hole below %p\n", in_hole, (void *)b2);

    CHECK(check_fill(e, RUN, 3), "e corrupted");
    CHECK(check_fill(b2, RUN, 5), "b2 corrupted");
    for (int i = first; i < nheld; i++)
        CHECK(check_fill(held[i], RUN, 300 + (uint32_t)i), "filler %d corrupted", i);
    for (int i = first; i < nheld; i++)
        free(held[i]);
    nheld = first;
    free(e);
    free(b2);
    /* a, b and the shape-2 refill of the first hole. */
    CHECK(check_fill(held[0], RUN, 1), "a corrupted");
    CHECK(check_fill(held[1], RUN, 2), "b corrupted");
    for (int i = 2; i < nheld; i++)
        CHECK(check_fill(held[i], RUN, 200 + (uint32_t)i), "refill %d corrupted", i);
    for (int i = 0; i < nheld; i++)
        free(held[i]);
    nheld = 0;
}

int main(void)
{
    require_challoc();
    hole_above_a_fresh_mark();
    hole_folded_into_an_empty_extent();
    return finish("huge_hole");
}
