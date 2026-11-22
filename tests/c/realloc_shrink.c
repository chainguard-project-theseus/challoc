/*
 * realloc of a huge run that shrinks (src/huge/shrink.rs, design 6.3 and
 * 7). Run under LD_PRELOAD=libchalloc.so by src/api/c_programs.rs, with
 * MADV_GUARD_INSTALL guards and under guards:false.
 *
 * Claims:
 *   - a run trimmed to fewer pages, still above MEDIUM_MAX, keeps its
 *     address and its bytes; the pages it released are the first free
 *     extent a new run is placed in, and calloc hands them out zeroed
 *     although the run wrote them;
 *   - a shrunk run has the usable size a new run of its size has
 *     (nallocx), so free_sized and sdallocx with that size accept it,
 *     and the page past it faults;
 *   - a read buffer doubled to 64 MiB and trimmed to its length (Ruby's
 *     IO#read) is never copied at the trim;
 *   - rallocx shrinks in place too, MALLOCX_ZERO or not;
 *     challoc_realloc_usable reports the shrunk usable size;
 *   - a shrink to MEDIUM_MAX or less moves to a slab class, its bytes
 *     kept;
 *   - a run shrunk in place by realloc, rallocx and challoc_realloc_usable
 *     counts as the move it replaces in thread.allocated and
 *     thread.deallocated, so the two grow by the same amount once it is
 *     freed;
 *   - in a fork epoch (challoc_cow_begin) a run shrinks in place, and
 *     does not grow back into the pages it released (they may be the
 *     child's);
 *   - under zero_on_free (argument `zero_on_free`) a trim moves, so the
 *     freed bytes are cleared before the next run is placed on them;
 *   - stats.allocated returns to where it started.
 */
#include "common.h"
extern int challoc_cow_begin(unsigned) WEAK;
extern int challoc_cow_end(void) WEAK;

#ifndef MALLOCX_ZERO
#define MALLOCX_ZERO 0x40
#endif

#define MEDIUM_MAX (256u << 10)

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
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t base = allocated();

    /* Under zero_on_free (run with MALLOC_CONF=zero_on_free:true and the
     * argument `zero_on_free`) a freed run's bytes are cleared, so a trim
     * that would leave the released pages holding them moves instead:
     * the next run placed on those pages reads zero without calloc. */
    if (argc > 1 && strcmp(argv[1], "zero_on_free") == 0) {
        size_t big = 8u << 20, small = 1u << 20;
        char *p = malloc(big);
        REQUIRE(p, "malloc");
        memset(p, 0xa5, big);
        char *q = realloc(p, small);
        REQUIRE(q, "realloc down");
        CHECK(q != p, "shrank in place under zero_on_free");
        for (size_t i = 0; i < small; i += 4093)
            CHECK((unsigned char)q[i] == 0xa5, "byte %zu lost", i);
        char *r = malloc(big - small - 2 * page);
        REQUIRE(r, "malloc over the old run");
        CHECK(all_zero(r, big - small - 2 * page), "freed bytes reached the next owner under zero_on_free");
        free(r);
        free(q);
        return finish("realloc_shrink");
    }

    /* First, while the huge region is empty: the released pages are the
     * only free extent, so the next run that fits lands on them, and
     * calloc must clear what the shrunk run wrote there. */
    {
        size_t big = 8u << 20, small = 1u << 20;
        char *p = malloc(big);
        REQUIRE(p, "malloc");
        memset(p, 0x5a, big);
        char *q = realloc(p, small);
        REQUIRE(q, "realloc down");
        CHECK(q == p, "shrink moved the run");
        CHECK(malloc_usable_size(q) == nallocx(small, 0), "usable %zu, nallocx %zu", malloc_usable_size(q), nallocx(small, 0));
        for (size_t i = 0; i < small; i += 4093)
            CHECK((unsigned char)q[i] == 0x5a, "byte %zu lost", i);
        size_t want = big - small - 2 * page;
        char *c = calloc(1, want);
        REQUIRE(c, "calloc");
        CHECK(c == q + nallocx(small, 0) + page, "calloc %p not on the released pages after %p", (void *)c, (void *)q);
        CHECK(all_zero(c, want), "calloc over a shrunk run's released pages not zero");
        if (ctl_bool("opt.guards"))
            CHECK(expect_signal(SIGSEGV, touch_guard, q), "no fault past the shrunk run");
        free(c);
        /* The sized frees take the shrunk run's size. */
        sdallocx(q, small, 0);
    }

    /* A read buffer: doubled as the data comes, then trimmed to the
     * length read. */
    {
        size_t cap = 1u << 20, len = 0, total = (64u << 20) - 12345;
        char *buf = malloc(cap);
        REQUIRE(buf, "malloc buf");
        while (len < total) {
            size_t chunk = total - len < (1u << 20) ? total - len : (1u << 20);
            if (len + chunk > cap) {
                cap *= 2;
                buf = realloc(buf, cap);
                REQUIRE(buf, "realloc up %zu", cap);
            }
            memset(buf + len, (int)(len >> 20), chunk);
            len += chunk;
        }
        char *t = realloc(buf, len);
        REQUIRE(t, "realloc trim");
        CHECK(t == buf, "the trim copied the buffer");
        CHECK(malloc_usable_size(t) == nallocx(len, 0), "trimmed usable %zu, nallocx %zu", malloc_usable_size(t), nallocx(len, 0));
        for (size_t i = 0; i < len; i += 1u << 19)
            CHECK((unsigned char)t[i] == (unsigned char)(i >> 20), "trimmed byte %zu", i);
        printf("read buffer: %zu bytes trimmed from %zu in place\n", len, cap);
        free_sized(t, len);
    }

    /* rallocx, MALLOCX_ZERO or not, and challoc_realloc_usable. */
    {
        size_t r = 4u << 20;
        char *x = mallocx(r, 0);
        REQUIRE(x, "mallocx");
        fill(x, r, 3);
        char *y = rallocx(x, r / 2, 0);
        REQUIRE(y, "rallocx");
        CHECK(y == x, "rallocx moved on a shrink");
        CHECK(sallocx(y, 0) == nallocx(r / 2, 0), "rallocx usable %zu", sallocx(y, 0));
        CHECK(check_fill(y, r / 2, 3), "rallocx lost bytes");
        char *z = rallocx(y, r / 4, MALLOCX_ZERO);
        REQUIRE(z, "rallocx zero");
        CHECK(z == y, "rallocx MALLOCX_ZERO moved on a shrink");
        CHECK(check_fill(z, r / 4, 3), "rallocx MALLOCX_ZERO lost bytes");
        size_t u = 0;
        char *w = challoc_realloc_usable(z, r / 8 + 1, &u);
        REQUIRE(w, "challoc_realloc_usable");
        CHECK(w == z, "challoc_realloc_usable moved on a shrink");
        CHECK(u == nallocx(r / 8 + 1, 0) && u == malloc_usable_size(w), "realloc_usable reported %zu", u);
        CHECK(check_fill(w, r / 8, 3), "challoc_realloc_usable lost bytes");
        sdallocx(w, r / 8 + 1, 0);
    }

    /* To a slab class: a move, its bytes kept. */
    {
        size_t r = 2u << 20, s = MEDIUM_MAX / 2;
        char *x = malloc(r);
        REQUIRE(x, "malloc");
        fill(x, r, 9);
        char *y = realloc(x, s);
        REQUIRE(y, "realloc to a class");
        CHECK(y != x, "a shrink below MEDIUM_MAX stayed a run");
        CHECK(malloc_usable_size(y) == nallocx(s, 0), "class usable %zu", malloc_usable_size(y));
        CHECK(check_fill(y, s, 9), "class move lost bytes");
        free_sized(y, s);
    }

    /* thread.allocated and thread.deallocated: each shrink counts the
     * shrunk run's usable bytes allocated and the old run's deallocated,
     * as a move does, so once the run is freed the two have grown alike. */
    {
        uint64_t a0 = thread_u64("thread.allocated"), d0 = thread_u64("thread.deallocated");
        size_t s = 8u << 20, kept = 0, tu = 0;
        char *t = malloc(s);
        REQUIRE(t, "malloc for the thread counters");
        char *t2 = realloc(t, s / 2);
        REQUIRE(t2, "realloc for the thread counters");
        kept += t2 == t;
        char *t3 = rallocx(t2, s / 4, 0);
        REQUIRE(t3, "rallocx for the thread counters");
        kept += t3 == t2;
        char *t4 = challoc_realloc_usable(t3, s / 8, &tu);
        REQUIRE(t4, "challoc_realloc_usable for the thread counters");
        kept += t4 == t3;
        free(t4);
        uint64_t a1 = thread_u64("thread.allocated"), d1 = thread_u64("thread.deallocated");
        CHECK(a1 - a0 == d1 - d0, "thread.allocated +%llu, thread.deallocated +%llu (%zu of 3 shrank in place)",
              (unsigned long long)(a1 - a0), (unsigned long long)(d1 - d0), kept);
        CHECK(kept == 3, "%zu of 3 shrank in place for the thread counters", kept);
    }

    /* Fork epochs: the shrink goes through; the released pages are an
     * extent freed in the epoch, which the run does not grow back into. */
    if (challoc_cow_begin && challoc_cow_begin(0) == 0) {
        size_t s = 4u << 20;
        char *e1 = malloc(s), *e2 = malloc(1u << 20);
        REQUIRE(e1 && e2, "malloc in the epoch");
        fill(e1, s, 5);
        char *g = realloc(e1, s / 4);
        REQUIRE(g, "realloc down in the epoch");
        CHECK(g == e1, "did not shrink in place in the epoch");
        char *h = realloc(g, s / 2);
        REQUIRE(h, "realloc up in the epoch");
        CHECK(h != g, "grew into pages released in the epoch");
        CHECK(check_fill(h, s / 4, 5), "bytes lost in the epoch");
        REQUIRE(challoc_cow_end() == 0, "challoc_cow_end");
        free(h);
        free(e2);
    } else {
        printf("SKIP realloc_shrink epoch: challoc_cow_begin unavailable\n");
    }

    size_t after = allocated();
    CHECK(after <= base + (64u << 10), "stats.allocated %zu after, %zu before", after, base);
    return finish("realloc_shrink");
}
