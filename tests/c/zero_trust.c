/*
 * Zeroing entry points after a heap bug wrote memory challoc has not handed
 * out yet. The SECURITY.md attacker model includes a linear overflow or
 * underflow into a neighbour and a write through a stale pointer; neither
 * may make a later zeroing allocation return a non-zero byte. Every cell
 * of the matrix runs in its own forked child, so the size classes it uses
 * are untouched (Fresh) when it starts:
 *
 *   scenario (what wrote the memory)            entry point (what must be zero)
 *   next     1 byte past a fresh slot            calloc
 *   run      64 KiB past a fresh slot            challoc_calloc_usable (usable size)
 *   purged   1 byte into a Purged slab           mallocx(MALLOCX_ZERO) (usable size)
 *   huge-uaf stale write to a purged huge run    mallocx(MALLOCX_ZERO | TCACHE_NONE)
 *   huge-under underflow from a live huge run    rallocx(MALLOCX_ZERO), the tail
 *              into a purged huge neighbour
 */
#include "common.h"

#define H (1u << 20)    /* a huge size (over MEDIUM_MAX) */
#define PURGED_S 4096   /* 16 slots to a 64 KiB slab */
#define PURGED_SLAB 65536
#define RUN_SPAN 65536  /* how far the `run` overflow writes */

enum entry { E_CALLOC, E_CALLOC_USABLE, E_MALLOCX, E_MALLOCX_DIRECT, E_RALLOCX, NENTRY };
static const char *const entry_name[NENTRY] = {"calloc", "calloc_usable", "mallocx_zero", "mallocx_zero_direct",
                                                "rallocx_zero"};

/* One zeroing allocation of `n` bytes through `e`; returns the block and
 * the range that must read zero in *from..*to. */
static unsigned char *zero_alloc(enum entry e, size_t n, size_t *from, size_t *to)
{
    unsigned char *p = NULL;
    size_t u = 0;
    *from = 0;
    switch (e) {
    case E_CALLOC:
        p = calloc(1, n);
        u = n;
        break;
    case E_CALLOC_USABLE:
        p = challoc_calloc_usable(1, n, &u);
        break;
    case E_MALLOCX:
        p = mallocx(n, MALLOCX_ZERO);
        u = p ? sallocx(p, 0) : 0;
        break;
    case E_MALLOCX_DIRECT:
        p = mallocx(n, MALLOCX_ZERO | MALLOCX_TCACHE_NONE);
        u = p ? sallocx(p, 0) : 0;
        break;
    case E_RALLOCX: {
        unsigned char *q = mallocx(16, 0);
        REQUIRE(q != NULL, "mallocx(16)");
        memset(q, 0x5A, 16);
        p = rallocx(q, n, MALLOCX_ZERO);
        if (p) {
            u = sallocx(p, 0);
            for (int i = 0; i < 16; i++)
                CHECK(p[i] == 0x5A, "rallocx(%zu) lost byte %d", n, i);
        }
        *from = 16;
        break;
    }
    default:
        REQUIRE(0, "entry %d", (int)e);
    }
    REQUIRE(p != NULL, "%s(%zu) returned NULL", entry_name[e], n);
    *to = u;
    return p;
}

/* `count` zeroing allocations of `n` bytes through `e`, each checked; the
 * blocks stay live so every one is a new slot. Returns the non-zero ones. */
static int take_zeroed(enum entry e, size_t n, int count, const char *what)
{
    int bad = 0;
    for (int i = 0; i < count; i++) {
        size_t from, to;
        unsigned char *p = zero_alloc(e, n, &from, &to);
        if (!is_zero(p + from, to - from)) {
            bad++;
            size_t k = from;
            while (k < to && p[k] == 0)
                k++;
            CHECK(0, "%s/%s: block %d (%p, %zu bytes) reads %#x at offset %zu", what, entry_name[e], i, (void *)p, n,
                  p[k], k);
        }
    }
    return bad;
}

/* The heap bugs write through a volatile copy of the pointer, so gcc
 * neither diagnoses nor folds them. */
static void bug_write(void *p, ptrdiff_t off, unsigned char v, size_t n)
{
    unsigned char *volatile q = p;
    for (size_t i = 0; i < n; i++)
        q[off + (ptrdiff_t)i] = v;
}

static void flush_and_purge(void)
{
    REQUIRE(mallctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "thread.tcache.flush");
    REQUIRE(mallctl("arena.4096.purge", NULL, NULL, NULL, 0) == 0, "arena.<all>.purge");
}

static int guard_install(void)
{
    return challoc_features && strstr(challoc_features(), "guard_install=yes") != NULL;
}

struct cell {
    const char *scenario;
    enum entry e;
};

/* A fresh slot, then the next slot's first byte written: the overflow of an
 * off-by-one. Two sizes: a small class and a medium one. */
static int next_cell(struct cell *c)
{
    static const size_t sizes[] = {48, 2560};
    for (size_t i = 0; i < 2; i++) {
        unsigned char *p = malloc(sizes[i]);
        REQUIRE(p != NULL, "malloc");
        bug_write(p, (ptrdiff_t)sizes[i], 0xA5, 1);
        take_zeroed(c->e, sizes[i], 64, c->scenario);
    }
    return failures;
}

/* The end of the read-write mapping holding `p` (/proc/self/maps). */
static uintptr_t mapping_end(const void *p)
{
    FILE *f = fopen("/proc/self/maps", "r");
    REQUIRE(f != NULL, "open /proc/self/maps: %s", strerror(errno));
    char line[512];
    uintptr_t end = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        char perms[8];
        if (sscanf(line, "%lx-%lx %7s", &lo, &hi, perms) == 3 && lo <= (uintptr_t)p && (uintptr_t)p < hi &&
            perms[0] == 'r' && perms[1] == 'w')
            end = hi;
    }
    fclose(f);
    REQUIRE(end != 0, "no read-write mapping holds %p", p);
    return end;
}

/* A fresh slot, then up to RUN_SPAN bytes past it, never past the end of
 * the committed run: an overflow across the slots after it (read-write,
 * never handed out). */
static int run_cell(struct cell *c)
{
    static const size_t sizes[] = {48, 2560};
    for (size_t i = 0; i < 2; i++) {
        unsigned char *p = malloc(sizes[i]);
        REQUIRE(p != NULL, "malloc");
        size_t span = mapping_end(p) - (uintptr_t)(p + sizes[i]);
        if (span > RUN_SPAN)
            span = RUN_SPAN;
        bug_write(p, (ptrdiff_t)sizes[i], 0xA5, span);
        take_zeroed(c->e, sizes[i], (int)(span / sizes[i]) + 64, c->scenario);
    }
    return failures;
}

static int cmp_ptr(const void *a, const void *b)
{
    uintptr_t x = (uintptr_t) * (void *const *)a, y = (uintptr_t) * (void *const *)b;
    return x < y ? -1 : x > y;
}

/* Two adjacent slabs held whole; the upper one freed and purged (Purged:
 * MADV_DONTNEED, read-write), then one byte written past the lower slab's
 * last slot, into the Purged slab's first. */
static int purged_cell(struct cell *c)
{
    enum { N = 96 };
    unsigned char *blk[N];
    for (int i = 0; i < N; i++) {
        blk[i] = malloc(PURGED_S);
        REQUIRE(blk[i] != NULL, "malloc");
    }
    qsort(blk, N, sizeof blk[0], cmp_ptr);
    int a = -1;
    for (int i = N - 32; i >= 0 && a < 0; i--) {
        uintptr_t base = (uintptr_t)blk[i];
        if (base % PURGED_SLAB)
            continue;
        int ok = 1;
        for (int k = 0; k < 32 && ok; k++)
            ok = (uintptr_t)blk[i + k] == base + (uintptr_t)k * PURGED_S;
        if (ok)
            a = i;
    }
    REQUIRE(a >= 0, "no two adjacent slabs of %d-byte slots held whole", PURGED_S);
    for (int k = 16; k < 32; k++)
        free(blk[a + k]);
    flush_and_purge();
    bug_write(blk[a + 15], PURGED_S, 0xA5, 1);
    take_zeroed(c->e, PURGED_S, 48, c->scenario);
    return failures;
}

/* A huge run freed and purged, then written through the stale pointer. */
static int huge_uaf_cell(struct cell *c)
{
    unsigned char *volatile p = malloc(H);
    REQUIRE(p != NULL, "malloc(H)");
    memset(p, 0x11, H);
    free(p);
    flush_and_purge();
    bug_write(p, 0, 0xA5, 1);
    bug_write(p, H / 2, 0xA5, 1);
    bug_write(p, H - 1, 0xA5, 1);
    take_zeroed(c->e, H, 2, c->scenario);
    return failures;
}

/* Two adjacent huge runs; the lower one freed and purged (its guard page
 * goes with it), then a linear underflow from the upper one into it. */
static int huge_under_cell(struct cell *c)
{
    size_t pg = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *a = malloc(H), *b = malloc(H);
    REQUIRE(a != NULL && b != NULL, "malloc(H)");
    if (b != a + H + pg) {
        skip_note("zero_trust/huge-under", "runs not adjacent (%p, %p)", (void *)a, (void *)b);
        return 0;
    }
    free(a);
    flush_and_purge();
    bug_write(b, -(ptrdiff_t)(pg + 256), 0xA5, pg + 256);
    take_zeroed(c->e, H, 2, c->scenario);
    return failures;
}

/* 0 or 1, never the failure count: `fork_and_wait` keeps only the low
 * byte of the exit status, so 256 failures would read as success. */
static int run_one(void *arg)
{
    struct cell *c = arg;
    failures = 0;
    if (!strcmp(c->scenario, "next"))
        next_cell(c);
    else if (!strcmp(c->scenario, "run"))
        run_cell(c);
    else if (!strcmp(c->scenario, "purged"))
        purged_cell(c);
    else if (!strcmp(c->scenario, "huge-uaf"))
        huge_uaf_cell(c);
    else
        huge_under_cell(c);
    return failures != 0;
}

int main(void)
{
    require_challoc();
    static const char *const scenarios[] = {"next", "run", "purged", "huge-uaf", "huge-under"};
    int gi = guard_install();
    for (size_t s = 0; s < sizeof scenarios / sizeof scenarios[0]; s++) {
        if (!strcmp(scenarios[s], "huge-under") && !gi) {
            /* Without MADV_GUARD_INSTALL a freed run keeps a PROT_NONE
             * guard, so the underflow faults before it reaches the run. */
            skip_note("zero_trust/huge-under", "no guard_install: the freed run's guard stays PROT_NONE");
            continue;
        }
        for (int e = 0; e < NENTRY; e++) {
            struct cell c = {scenarios[s], (enum entry)e};
            int r = fork_and_wait(run_one, &c);
            CHECK(r == 0, "%s/%s: child %s %d", scenarios[s], entry_name[e], r < 0 ? "died of signal" : "exited",
                  r < 0 ? -r : r);
        }
    }
    return finish("zero_trust");
}
