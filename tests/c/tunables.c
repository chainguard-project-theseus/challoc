/*
 * Phase 2 task 3: the opt-in hardening tunables under LD_PRELOAD. The
 * driver in src/api/ctests.rs runs one mode per process, with MALLOC_CONF
 * set for the "on" modes and unset for their controls, which prove the
 * default paths do not do the same thing by accident.
 *
 *   tunables zero     MALLOC_CONF=zero_on_free:true: a written block reads
 *                     as zero after free() (still mapped: slots stay
 *                     committed, a retained huge run too) and when the same
 *                     address comes back from malloc, through free, the
 *                     sized path (sdallocx, free_sized) and a huge run
 *   tunables dirty    no key: the same blocks keep their bytes, and the
 *                     LIFO reuse hands them back dirty
 *   tunables random   MALLOC_CONF=slot_randomize:true: the addresses of one
 *                     class's first allocations in a fresh process are not
 *                     monotone, cover a contiguous set of slots, and every
 *                     block is distinct and writable
 *   tunables ordered  no key: the same sequence is ascending
 */
#include "common.h"

extern void sdallocx(void *, size_t, int) __attribute__((weak));
extern void free_sized(void *, size_t) __attribute__((weak));
extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

static int opt_bool(const char *key)
{
    REQUIRE(mallctl != NULL, "mallctl not exported");
    unsigned char v = 0;
    size_t len = sizeof v;
    char name[64];
    snprintf(name, sizeof name, "opt.%s", key);
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s)", name);
    return v != 0;
}

static const size_t sizes[] = {16, 64, 1000, 1024, 4096, 32768, 1 << 20};
#define NSIZES (sizeof sizes / sizeof sizes[0])

/* The pattern must not be zero anywhere. */
static void dirty(void *p, size_t n)
{
    memset(p, 0xA5, n);
}

/* Free `p` through `how` (0 free, 1 sdallocx, 2 free_sized), then report
 * whether its `n` bytes read as zero right after and whether malloc(n)
 * gives the address back (the LIFO cache does; a retained huge run does
 * too) and, if so, whether the block is zero then. */
static void round_trip(size_t n, int how, int *zero_after_free, int *came_back, int *zero_on_reuse)
{
    void *p = malloc(n);
    REQUIRE(p != NULL, "malloc(%zu)", n);
    REQUIRE(malloc_usable_size(p) >= n, "usable");
    /* The requested `n` bytes, not the usable size: glibc's fortified
     * memset knows malloc(n)'s object size and would trap on more. The
     * allocator clears the whole class size (or the run's usable bytes),
     * of which these are the first n. */
    size_t usable = n;
    dirty(p, usable);
    switch (how) {
    case 0: free(p); break;
    case 1: REQUIRE(sdallocx != NULL, "sdallocx"); sdallocx(p, n, 0); break;
    default: REQUIRE(free_sized != NULL, "free_sized"); free_sized(p, n); break;
    }
    /* The memory is still mapped: a slot's slab stays committed and a
     * 1 MiB run is far under huge_retain_max. Reading freed memory is the
     * point of the test, not a use-after-free bug. */
    *zero_after_free = is_zero(p, usable);
    *came_back = 0;
    *zero_on_reuse = 0;
    void *held[64];
    int nheld = 0;
    for (int i = 0; i < 64; i++) {
        void *q = malloc(n);
        REQUIRE(q != NULL, "malloc(%zu)", n);
        if (q == p) {
            *came_back = 1;
            *zero_on_reuse = is_zero(q, usable);
            /* keep it out of the way of the next round */
            held[nheld++] = q;
            break;
        }
        held[nheld++] = q;
    }
    for (int i = 0; i < nheld; i++)
        free(held[i]);
}

static void mode_zero(int on)
{
    CHECK(opt_bool("zero_on_free") == on, "opt.zero_on_free is not %d", on);
    for (size_t i = 0; i < NSIZES; i++) {
        for (int how = 0; how < 3; how++) {
            if (how && sizes[i] > 32768)
                continue; /* the sized paths are for slab slots */
            int zaf, back, zor;
            round_trip(sizes[i], how, &zaf, &back, &zor);
            const char *w = how == 0 ? "free" : how == 1 ? "sdallocx" : "free_sized";
            if (on) {
                CHECK(zaf, "%s(%zu): the freed block is not zero", w, sizes[i]);
                CHECK(back, "%s(%zu): the address never came back", w, sizes[i]);
                CHECK(!back || zor, "%s(%zu): the reused block is not zero", w, sizes[i]);
            } else {
                CHECK(!zaf, "%s(%zu): the default free cleared the block", w, sizes[i]);
                CHECK(back, "%s(%zu): the address never came back", w, sizes[i]);
                CHECK(!back || !zor, "%s(%zu): the default reuse handed out a zero block", w, sizes[i]);
            }
        }
    }
    printf("zero_on_free=%d: %zu sizes through free, sdallocx and free_sized\n", on, NSIZES);
}

/* The 64-byte class's cache holds 1024 slots and refills 512 at a time:
 * the first 512 allocations of a fresh process are one batch. */
#define N 512

static void mode_random(int on)
{
    CHECK(opt_bool("slot_randomize") == on, "opt.slot_randomize is not %d", on);
    /* A fresh process: the first malloc(64) refills the 64-byte cache with
     * one batch of the first slab's slots (512 of 1024). */
    void *ps[N];
    for (int i = 0; i < N; i++) {
        ps[i] = malloc(64);
        REQUIRE(ps[i] != NULL, "malloc(64)");
        memset(ps[i], i + 1, 64);
    }
    int up = 0, down = 0;
    for (int i = 1; i < N; i++) {
        if ((uintptr_t)ps[i] > (uintptr_t)ps[i - 1]) up++; else down++;
    }
    /* Distinct, 64-byte slot starts, and from at most two slabs (one
     * batch, or two if something before us took part of the first). */
    uintptr_t sorted[N];
    for (int i = 0; i < N; i++)
        sorted[i] = (uintptr_t)ps[i];
    for (int i = 1; i < N; i++)
        for (int j = i; j > 0 && sorted[j - 1] > sorted[j]; j--) {
            uintptr_t t = sorted[j]; sorted[j] = sorted[j - 1]; sorted[j - 1] = t;
        }
    int dups = 0, misaligned = 0;
    for (int i = 0; i < N; i++) {
        if (i > 0 && sorted[i] == sorted[i - 1]) dups++;
        if (sorted[i] % 64) misaligned++;
    }
    CHECK(dups == 0, "%d duplicate blocks", dups);
    CHECK(misaligned == 0, "%d blocks not slot aligned", misaligned);
    CHECK(sorted[N - 1] - sorted[0] < 2 * 65536, "the %d blocks span more than two 64 KiB slabs", N);
    for (int i = 0; i < N; i++) {
        unsigned char *b = ps[i];
        int ok = 1;
        for (int k = 0; k < 64; k++)
            if (b[k] != (unsigned char)(i + 1)) ok = 0;
        CHECK(ok, "block %d overwritten", i);
    }
    if (on) {
        CHECK(down > 0 && up > 0, "slot_randomize on: the order is monotone (up %d, down %d)", up, down);
        /* 255 steps of a random permutation: about half go down. */
        CHECK(down > N / 8 && up > N / 8, "slot_randomize on: too close to sorted (up %d, down %d)", up, down);
    } else {
        CHECK(down == 0, "default: the order is not ascending (up %d, down %d)", up, down);
    }
    for (int i = 0; i < N; i++)
        free(ps[i]);
    printf("slot_randomize=%d: %d steps up, %d down\n", on, up, down);
}

int main(int argc, char **argv)
{
    require_challoc();
    const char *mode = argc > 1 ? argv[1] : "";
    if (!strcmp(mode, "zero")) mode_zero(1);
    else if (!strcmp(mode, "dirty")) mode_zero(0);
    else if (!strcmp(mode, "random")) mode_random(1);
    else if (!strcmp(mode, "ordered")) mode_random(0);
    else REQUIRE(0, "unknown mode %s", mode);
    return finish("tunables");
}
