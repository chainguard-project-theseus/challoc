/*
 * realloc of a huge run that grows (src/huge/grow.rs, design 6.3 and 7).
 * Run under LD_PRELOAD=libchalloc.so by src/api/c_programs.rs, with
 * MADV_GUARD_INSTALL guards and under guards:false.
 *
 * Claims:
 *   - one buffer grown by exact sizes (a byte past each page, then the
 *     rest of the page, as an appending caller asks) keeps its address
 *     once it is the last run (at most one move), keeps its bytes, and the
 *     page past the grown run still faults;
 *   - two buffers grown in turn, which cannot both be the last run, move
 *     only now and then;
 *   - a grown run has the usable size a new run of its size has
 *     (nallocx), so free_sized and sdallocx with that size accept it;
 *   - rallocx grows in place too, and under MALLOCX_ZERO the bytes past the
 *     old size read zero, moved or not; challoc_realloc_usable reports the
 *     grown usable size; xallocx still never resizes;
 *   - a run freed after growing, its bytes written, is handed out zeroed
 *     by calloc;
 *   - while a fork epoch is open (challoc_cow_begin) a run does not grow
 *     into a free extent left by a free during the epoch (its pages may be
 *     the child's), but does grow past the high-water mark; after
 *     challoc_cow_end it grows into such an extent again; in an epoch it
 *     grows into a `Purged` extent while no MADV_FREE batch has gone out,
 *     and moves after one (a note line where lazy purges are off);
 *   - a run grown in place by realloc, rallocx and challoc_realloc_usable
 *     counts as the move it replaces in thread.allocated and
 *     thread.deallocated, so the two grow by the same amount once it is
 *     freed;
 *   - stats.allocated returns to where it started.
 */
#include "common.h"
extern int challoc_cow_begin(unsigned) WEAK;
extern int challoc_cow_end(void) WEAK;

#ifndef MALLOCX_ZERO
#define MALLOCX_ZERO 0x40
#endif

#define START (300u << 10)
#define END (48u << 20)

static size_t allocated(void)
{
    uint64_t e = 1;
    size_t n = sizeof e;
    mallctl("epoch", &e, &n, &e, sizeof e);
    return ctl_size("stats.allocated");
}

static uint64_t thread_u64(const char *name)
{
    uint64_t v = 0;
    size_t n = sizeof v;
    REQUIRE(mallctl(name, &v, &n, NULL, 0) == 0, "%s", name);
    return v;
}

static void touch_guard(void *arg)
{
    char *p = arg;
    size_t u = malloc_usable_size(p);
    ((volatile char *)p)[u] = 1;
}

static int all_zero(const char *p, size_t n)
{
    for (size_t i = 0; i < n; i++)
        if (p[i])
            return 0;
    return 1;
}

int main(int argc, char **argv)
{
    require_challoc();
    (void)argc;
    (void)argv;
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t base = allocated();

    /* One buffer, grown by exact sizes: a byte into the next page, then
     * the rest of it. */
    size_t n = START, moves = 0, steps = 0;
    char *p = malloc(n);
    REQUIRE(p, "malloc");
    fill(p, n, 1);
    while (n < END) {
        size_t m = (n % page) ? (n + page - 1) / page * page : n + 1;
        char *q = realloc(p, m);
        REQUIRE(q, "realloc(%zu)", m);
        moves += q != p;
        steps++;
        CHECK(malloc_usable_size(q) == nallocx(m, 0), "usable %zu, nallocx %zu", malloc_usable_size(q), nallocx(m, 0));
        if ((n / page) % 512 == 0)
            CHECK(check_fill(q, n, 1), "bytes lost at %zu", n);
        memset(q + n, 0x5a, m - n);
        p = q;
        n = m;
        if ((n / page) % 512 == 0)
            fill(p, n, 1);
    }
    CHECK(moves <= 1, "one buffer: %zu moves, want at most 1", moves);
    printf("one buffer: %zu moves over %zu reallocs\n", moves, steps);
    if (ctl_bool("opt.guards"))
        CHECK(expect_signal(SIGSEGV, touch_guard, p), "no fault past the grown run");
    /* The sized frees take the grown run's size. */
    free_sized(p, n);

    /* Two buffers grown in turn: at most one of them is the last run. */
    size_t na = START, nb = START, both = 0;
    char *a = malloc(na), *b = malloc(nb);
    REQUIRE(a && b, "malloc");
    memset(a, 0xa1, na);
    memset(b, 0xb2, nb);
    while (na < END / 4) {
        char *q = realloc(a, na + page);
        REQUIRE(q, "realloc a");
        both += q != a;
        a = q;
        memset(a + na, 0xa1, page);
        na += page;
        q = realloc(b, nb + page);
        REQUIRE(q, "realloc b");
        both += q != b;
        b = q;
        memset(b + nb, 0xb2, page);
        nb += page;
    }
    for (size_t i = 0; i < na; i += 4093)
        CHECK((unsigned char)a[i] == 0xa1 && (unsigned char)b[i] == 0xb2, "interleaved bytes at %zu", i);
    /* Moving at every step would be all of them. */
    CHECK(both <= 100, "two buffers: %zu moves, want at most 100", both);
    printf("two buffers: %zu moves over %zu reallocs\n", both, (size_t)(2 * (END / 4 - START) / page));
    sdallocx(a, na, 0);
    sdallocx(b, nb, 0);

    /* rallocx, the last run: in place, its bytes kept. */
    size_t r = 1u << 20;
    char *x = mallocx(r, 0);
    REQUIRE(x, "mallocx");
    fill(x, r, 3);
    char *y = rallocx(x, r + 3 * page, 0);
    REQUIRE(y, "rallocx");
    CHECK(y == x, "rallocx moved the last run");
    CHECK(check_fill(y, r, 3), "rallocx lost bytes");
    CHECK(sallocx(y, 0) == nallocx(r + 3 * page, 0), "rallocx usable %zu", sallocx(y, 0));
    CHECK(xallocx(y, r + 8 * page, 0, 0) == sallocx(y, 0), "xallocx resized");
    /* MALLOCX_ZERO: what lies past the old size reads zero. */
    memset(y, 0x77, sallocx(y, 0));
    size_t old = sallocx(y, 0);
    char *z = rallocx(y, old + 2 * page, MALLOCX_ZERO);
    REQUIRE(z, "rallocx zero");
    CHECK(all_zero(z + old, 2 * page), "rallocx MALLOCX_ZERO tail not zero");
    /* challoc_realloc_usable reports the grown size. */
    size_t u = 0;
    char *w = challoc_realloc_usable(z, old + 5 * page, &u);
    REQUIRE(w, "challoc_realloc_usable");
    CHECK(u == nallocx(old + 5 * page, 0) && u == malloc_usable_size(w), "realloc_usable reported %zu", u);
    /* Freed, its bytes written, the grown run's pages come back zero
     * through calloc. */
    memset(w, 0x33, u);
    free(w);
    char *c = calloc(1, u);
    REQUIRE(c, "calloc");
    CHECK(all_zero(c, u), "calloc over a grown run's pages not zero");
    free(c);

    /* thread.allocated and thread.deallocated: each growth counts the
     * grown run's usable bytes allocated and the old run's deallocated, as
     * a move does, so once the run is freed the two have grown alike. */
    {
        uint64_t a0 = thread_u64("thread.allocated"), d0 = thread_u64("thread.deallocated");
        size_t s = 1u << 20, kept = 0, tu = 0;
        char *t = malloc(s);
        REQUIRE(t, "malloc for the thread counters");
        char *t2 = realloc(t, s + page);
        REQUIRE(t2, "realloc for the thread counters");
        kept += t2 == t;
        char *t3 = rallocx(t2, s + 2 * page, 0);
        REQUIRE(t3, "rallocx for the thread counters");
        kept += t3 == t2;
        char *t4 = challoc_realloc_usable(t3, s + 3 * page, &tu);
        REQUIRE(t4, "challoc_realloc_usable for the thread counters");
        kept += t4 == t3;
        free(t4);
        uint64_t a1 = thread_u64("thread.allocated"), d1 = thread_u64("thread.deallocated");
        CHECK(a1 - a0 == d1 - d0, "thread.allocated +%llu, thread.deallocated +%llu (%zu of 3 grew in place)",
              (unsigned long long)(a1 - a0), (unsigned long long)(d1 - d0), kept);
        CHECK(kept > 0, "no growth in place for the thread counters");
    }

    /* Fork epochs. */
    if (challoc_cow_begin && challoc_cow_begin(0) == 0) {
        /* First fit puts the second run right after the first and its
         * guard. */
        size_t s = 1u << 20, span = nallocx(s, 0) + page;
        char *e1 = malloc(s), *e2 = malloc(s);
        REQUIRE(e1 && e2, "malloc in the epoch");
        CHECK(e2 == e1 + span, "runs not adjacent: %p %p", (void *)e1, (void *)e2);
        free(e2);
        /* The extent behind `e1` was freed in the epoch: no growth. */
        char *g = realloc(e1, s + page);
        REQUIRE(g, "realloc in the epoch");
        CHECK(g != e1, "grew into an extent freed in the epoch");
        /* A run larger than any free extent (the 48 MiB buffers' are the
         * largest) lies at the mark: growth. */
        size_t big = 16u << 20;
        size_t top = 80u << 20;
        char *last = malloc(top);
        REQUIRE(last, "malloc last");
        char *g2 = realloc(last, top + page);
        CHECK(g2 == last, "did not grow past the mark in the epoch");
        REQUIRE(challoc_cow_end() == 0, "challoc_cow_end");
        free(g);
        free(g2);
        /* After the epoch: growth into a free extent behind a run. */
        char *f1 = malloc(big), *f2 = malloc(big), *f3 = malloc(big);
        REQUIRE(f1 && f2 && f3, "malloc after the epoch");
        CHECK(f2 == f1 + nallocx(big, 0) + page, "runs not adjacent after the epoch");
        free(f2);
        char *h = realloc(f1, big + page);
        CHECK(h == f1, "did not grow into the free extent after the epoch");
        free(h);
        free(f3);
        /* A `Purged` extent behind a run: an extent freed before the
         * epoch, which challoc_cow_begin's purge leaves `Purged`. With no
         * MADV_FREE batch so far (stats.cgroup.lazy_purges 0) its pages
         * are gone from both sides of a fork, so the run grows into it;
         * after one, it moves. */
        char *q1 = malloc(big), *q2 = malloc(big), *q3 = malloc(big);
        REQUIRE(q1 && q2 && q3, "malloc before the second epoch");
        CHECK(q2 == q1 + nallocx(big, 0) + page, "runs not adjacent before the second epoch");
        free(q2);
        REQUIRE(challoc_cow_begin(0) == 0, "challoc_cow_begin again");
        size_t lazy = ctl_size("stats.cgroup.lazy_purges");
        char *k = realloc(q1, big + page);
        REQUIRE(k, "realloc into a purged extent");
        if (lazy == 0) {
            CHECK(k == q1, "did not grow into a purged extent in the epoch");
            /* Not a SKIP line: the lazy half needs lazy_band and a cgroup
             * limit's headroom band, which no host here runs this under. */
            printf("realloc_grow epoch: eager purges, the lazy half not reached\n");
        } else {
            CHECK(k != q1, "grew into a lazily purged extent in the epoch");
        }
        REQUIRE(challoc_cow_end() == 0, "challoc_cow_end again");
        free(k);
        free(q3);
    } else {
        printf("SKIP realloc_grow epoch: challoc_cow_begin unavailable\n");
    }

    size_t after = allocated();
    CHECK(after <= base + (64u << 10), "stats.allocated %zu after, %zu before", after, base);
    return finish("realloc_grow");
}
