/*
 * The guard slabs seen through PAGEMAP_SCAN, never touched.
 *
 * guard_slab.c proves a guard by faulting on it. This program proves it
 * without a fault: `ioctl(PAGEMAP_SCAN)` on /proc/self/pagemap (kernel 6.7)
 * reports the categories of every page of a range, and from 6.16 a guard
 * region installed by MADV_GUARD_INSTALL is reported as PAGE_IS_GUARD
 * (docs/research.md, kernel table). The page-table walk reads nothing from
 * the pages themselves, so the check is safe on any kernel and can join the
 * container tests as a both-ways test: guard pages exactly where the
 * allocator put guard slabs, and nowhere else.
 *
 *   pagemap_guard on    default configuration. For the 16-, 64-, 256- and
 *                       1024-byte classes: fill past the first guard group,
 *                       find the guard slab as the one group-sized gap no
 *                       block came from (guard_slab.c's method, from the
 *                       public mallctl geometry), and scan:
 *                         6.16+      the guard slab's first page reads
 *                                    PAGE_IS_GUARD and not PAGE_IS_PRESENT (the
 *                                    kernel classifies the guard marker as a
 *                                    swap-type PTE first, so PAGE_IS_SWAPPED is
 *                                    set beside it: 0x112 on 7.0); the pages
 *                                    holding the last slot before it and the
 *                                    first slot after it read PAGE_IS_PRESENT
 *                                    and not PAGE_IS_GUARD; over the whole span
 *                                    the PAGE_IS_GUARD pages are exactly one per
 *                                    gap
 *                         6.13..6.15 the ioctl exists but rejects PAGE_IS_GUARD
 *                                    in its masks (EINVAL): the guard page is
 *                                    checked to be not present and reported as
 *                                    a swap-type entry (what the marker looks
 *                                    like to that category set, in a process
 *                                    that has nothing swapped) and its
 *                                    neighbours present; the program says the
 *                                    category is missing
 *                         < 6.13     no MADV_GUARD_INSTALL, so the allocator
 *                                    makes no guard slab: the span has no gap
 *                                    and, when the ioctl and the category exist
 *                                    (they do not, on such a kernel), no guard
 *                                    page; the program says which case it saw
 *                         < 6.7      no PAGEMAP_SCAN (ENOTTY): the gap check
 *                                    alone, said so
 *   pagemap_guard off   under MALLOC_CONF=guards:false: no gap in any class's
 *                       span and, where the category exists, no PAGE_IS_GUARD
 *                       page in it.
 *
 * Every capability is probed by the ioctl's own answer (ENOTTY, EINVAL on
 * an unknown category bit), never by a kernel version. Nothing here reads
 * allocator internals: the geometry comes from opt.guards,
 * opt.guard_slab_interval, arenas.page and arenas.bin.<i>.{size,slab_size}.
 */
#include "common.h"

#include <fcntl.h>
#include <stdbool.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <linux/types.h>
#include <linux/fs.h>

/* Kernel 6.7 (the ioctl) and 6.16 (PAGE_IS_GUARD); glibc's headers may predate them. */
#ifndef PAGEMAP_SCAN
struct page_region {
    __u64 start;
    __u64 end;
    __u64 categories;
};
struct pm_scan_arg {
    __u64 size;
    __u64 flags;
    __u64 start;
    __u64 end;
    __u64 walk_end;
    __u64 vec;
    __u64 vec_len;
    __u64 max_pages;
    __u64 category_inverted;
    __u64 category_mask;
    __u64 category_anyof_mask;
    __u64 return_mask;
};
#define PAGEMAP_SCAN _IOWR('f', 16, struct pm_scan_arg)
#endif
#ifndef PAGE_IS_WRITTEN
#define PAGE_IS_WRITTEN (1 << 1)
#endif
#ifndef PAGE_IS_PRESENT
#define PAGE_IS_PRESENT (1 << 3)
#endif
#ifndef PAGE_IS_SWAPPED
#define PAGE_IS_SWAPPED (1 << 4)
#endif
#ifndef PAGE_IS_HUGE
#define PAGE_IS_HUGE (1 << 6)
#endif
#ifndef PAGE_IS_GUARD
#define PAGE_IS_GUARD (1 << 8)
#endif
#ifndef MADV_GUARD_INSTALL
#define MADV_GUARD_INSTALL 102
#endif

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

static size_t page_size;
static int pm_fd = -1;
/* Probed once: does the ioctl exist, does it know PAGE_IS_GUARD? */
static bool have_scan, have_guard_cat;
static int scan_errno, guard_cat_errno;

/* ---- geometry from the public keys (as tests/c/guard_slab.c) ------------ */

static unsigned ctl_uint(const char *name)
{
    unsigned v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl != NULL, "mallctl not exported");
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

static bool ctl_bool(const char *name)
{
    bool v = false;
    size_t len = sizeof v;
    REQUIRE(mallctl != NULL, "mallctl not exported");
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

static size_t ctl_size(const char *name)
{
    size_t v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl != NULL, "mallctl not exported");
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s) failed", name);
    return v;
}

/* Bin index of the class whose slot size is exactly `size`, -1 if none. */
static int bin_of(size_t size)
{
    unsigned nbins = ctl_uint("arenas.nbins");
    for (unsigned i = 0; i < nbins; i++) {
        char name[64];
        snprintf(name, sizeof name, "arenas.bin.%u.size", i);
        if (ctl_size(name) == size)
            return (int)i;
    }
    return -1;
}

struct geom {
    size_t size, slab, page, group, per_slab;
    unsigned interval, effective;
    bool guards;
};

static bool geom_of(size_t size, struct geom *g)
{
    int bin = bin_of(size);
    if (bin < 0)
        return false;
    g->size = size;
    g->guards = ctl_bool("opt.guards");
    g->interval = ctl_uint("opt.guard_slab_interval");
    char name[64];
    snprintf(name, sizeof name, "arenas.bin.%d.slab_size", bin);
    g->slab = ctl_size(name);
    g->page = ctl_size("arenas.page");
    g->group = g->slab > g->page ? g->slab : g->page;
    g->per_slab = g->slab / size;
    size_t unit = g->page > 65536 ? g->page : 65536;
    g->effective = g->group >= unit ? g->interval : g->interval * (unsigned)(unit / g->group);
    REQUIRE(g->slab >= 4096 && g->slab % size == 0 && g->page >= 4096, "geometry: slab %zu page %zu", g->slab, g->page);
    return true;
}

/* Does this kernel install guard regions? The allocator's own probe. */
static bool guard_install_available(void)
{
    void *p = mmap(NULL, 2 * page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(p != MAP_FAILED, "mmap scratch");
    bool ok = madvise(p, page_size, MADV_GUARD_INSTALL) == 0;
    munmap(p, 2 * page_size);
    return ok;
}

static int cmp_ptr(const void *a, const void *b)
{
    uintptr_t x = (uintptr_t)*(void *const *)a, y = (uintptr_t)*(void *const *)b;
    return x < y ? -1 : x > y;
}

/* ---- PAGEMAP_SCAN ---------------------------------------------------------- */

/* Scan [lo, hi) with `return_mask`; regions into `regs` (up to `n`), the
 * count returned, -1 with errno on failure. Every page is reported
 * (category_mask 0 selects all), so a page absent from every region has
 * no category at all. */
static long scan(uintptr_t lo, uintptr_t hi, __u64 return_mask, struct page_region *regs, size_t n)
{
    struct pm_scan_arg a;
    memset(&a, 0, sizeof a);
    a.size = sizeof a;
    a.start = lo;
    a.end = hi;
    a.vec = (uintptr_t)regs;
    a.vec_len = n;
    a.return_mask = return_mask;
    return ioctl(pm_fd, PAGEMAP_SCAN, &a);
}

/* The categories of the page holding `addr` (masked by `return_mask`). */
static __u64 page_cats(uintptr_t addr, __u64 return_mask)
{
    struct page_region regs[8];
    uintptr_t p = addr & ~(uintptr_t)(page_size - 1);
    long n = scan(p, p + page_size, return_mask, regs, 8);
    REQUIRE(n >= 0, "PAGEMAP_SCAN of %#lx: %s", (unsigned long)p, strerror(errno));
    for (long i = 0; i < n; i++)
        if (regs[i].start <= addr && addr < regs[i].end)
            return regs[i].categories;
    return 0;
}

/* Pages of [lo, hi) that carry every bit of `want`. */
static size_t count_pages_with(uintptr_t lo, uintptr_t hi, __u64 want)
{
    struct page_region regs[256];
    size_t pages = 0;
    uintptr_t at = lo;
    while (at < hi) {
        struct pm_scan_arg a;
        memset(&a, 0, sizeof a);
        a.size = sizeof a;
        a.start = at;
        a.end = hi;
        a.vec = (uintptr_t)regs;
        a.vec_len = 256;
        a.return_mask = want;
        long n = ioctl(pm_fd, PAGEMAP_SCAN, &a);
        REQUIRE(n >= 0, "PAGEMAP_SCAN of %#lx..%#lx: %s", (unsigned long)at, (unsigned long)hi, strerror(errno));
        for (long i = 0; i < n; i++)
            if ((regs[i].categories & want) == want)
                pages += (regs[i].end - regs[i].start) / page_size;
        if (a.walk_end <= at)
            break;
        at = a.walk_end;
    }
    return pages;
}

static void probe_scan(void)
{
    pm_fd = open("/proc/self/pagemap", O_RDONLY | O_CLOEXEC);
    REQUIRE(pm_fd >= 0, "open /proc/self/pagemap: %s", strerror(errno));
    void *p = mmap(NULL, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(p != MAP_FAILED, "mmap scratch");
    *(volatile char *)p = 1;
    struct page_region regs[4];
    long n = scan((uintptr_t)p, (uintptr_t)p + page_size, PAGE_IS_PRESENT, regs, 4);
    if (n < 0) {
        scan_errno = errno;
        have_scan = false;
    } else {
        have_scan = true;
        CHECK(n == 1 && (regs[0].categories & PAGE_IS_PRESENT), "a written scratch page is not reported present (%ld regions)", n);
        n = scan((uintptr_t)p, (uintptr_t)p + page_size, PAGE_IS_GUARD | PAGE_IS_PRESENT, regs, 4);
        if (n < 0) {
            guard_cat_errno = errno;
            have_guard_cat = false;
        } else {
            have_guard_cat = true;
            CHECK(!(regs[0].categories & PAGE_IS_GUARD), "a written scratch page is reported as a guard");
        }
    }
    munmap(p, page_size);
}

static const char *capability_line(void)
{
    static char line[256];
    if (!have_scan)
        snprintf(line, sizeof line, "PAGEMAP_SCAN unavailable (%s: kernel before 6.7)", strerror(scan_errno));
    else if (!have_guard_cat)
        snprintf(line, sizeof line, "PAGEMAP_SCAN available, PAGE_IS_GUARD rejected (%s: kernel before 6.16)", strerror(guard_cat_errno));
    else
        snprintf(line, sizeof line, "PAGEMAP_SCAN available with PAGE_IS_GUARD (6.16+)");
    return line;
}

/* ---- one class ------------------------------------------------------------ */

/* Fill the class past its guard groups, find the guard slabs, scan. `expect`
 * says whether guard slabs must exist (guards on, kernel installs them). */
static void check_class(size_t size, bool expect)
{
    struct geom g;
    if (!geom_of(size, &g)) {
        skip_note("pagemap_guard", "no %zu-byte bin in this build", size);
        return;
    }
    size_t n = g.per_slab * (g.group / g.slab) * (g.effective + 2);
    void **a = malloc(n * sizeof *a);
    REQUIRE(a != NULL, "malloc failed");
    for (size_t i = 0; i < n; i++) {
        a[i] = malloc(g.size);
        REQUIRE(a[i] != NULL, "malloc(%zu) failed", g.size);
        /* Our own block: written so its page is present. Never the guard. */
        *(volatile unsigned char *)a[i] = 0x5A;
    }
    qsort(a, n, sizeof *a, cmp_ptr);
    uintptr_t lo = (uintptr_t)a[0] & ~(uintptr_t)(g.group - 1);
    uintptr_t hi = ((uintptr_t)a[n - 1] & ~(uintptr_t)(g.group - 1)) + g.group;

    /* Every one-group gap between consecutive blocks is a guard slab. */
    enum { MAX_GAPS = 16 };
    uintptr_t guards[MAX_GAPS], befores[MAX_GAPS], afters[MAX_GAPS];
    size_t gaps = 0;
    for (size_t i = 1; i < n; i++) {
        uintptr_t glo = (uintptr_t)a[i - 1] & ~(uintptr_t)(g.group - 1);
        uintptr_t ghi = (uintptr_t)a[i] & ~(uintptr_t)(g.group - 1);
        if (ghi - glo == 2 * g.group && gaps < MAX_GAPS) {
            guards[gaps] = glo + g.group;
            befores[gaps] = (uintptr_t)a[i - 1];
            afters[gaps] = (uintptr_t)a[i];
            gaps++;
        } else if (ghi - glo > 2 * g.group) {
            CHECK(0, "%zu-byte class: a %zu-group hole at %#lx that no allocation came from (not a guard slab)", g.size,
                  (size_t)((ghi - glo) / g.group - 1), (unsigned long)(glo + g.group));
        }
    }

    __u64 mask = PAGE_IS_PRESENT | PAGE_IS_SWAPPED | PAGE_IS_WRITTEN | (have_guard_cat ? PAGE_IS_GUARD : 0);
    if (expect) {
        REQUIRE(gaps >= 1, "%zu-byte class: no one-group gap among %zu blocks (slab %zu, group %zu, interval %u)", g.size, n,
                g.slab, g.group, g.effective);
        for (size_t k = 0; k < gaps; k++) {
            uintptr_t guard = guards[k];
            CHECK(befores[k] + g.size == guard, "highest block below the guard at %#lx is %#lx", (unsigned long)guard,
                  (unsigned long)befores[k]);
            CHECK(afters[k] == guard + g.group, "lowest block above the guard at %#lx is %#lx", (unsigned long)guard,
                  (unsigned long)afters[k]);
            if (!have_scan)
                continue;
            __u64 cg = page_cats(guard, mask), cb = page_cats(befores[k], mask), ca = page_cats(afters[k], mask);
            CHECK(cb & PAGE_IS_PRESENT, "the page of the last slot before the guard (%#lx) is not present (%#llx)",
                  (unsigned long)befores[k], (unsigned long long)cb);
            CHECK(ca & PAGE_IS_PRESENT, "the page of the first slot after the guard (%#lx) is not present (%#llx)",
                  (unsigned long)afters[k], (unsigned long long)ca);
            CHECK(!(cg & PAGE_IS_PRESENT), "the guard page %#lx is present (%#llx)", (unsigned long)guard, (unsigned long long)cg);
            /* The marker is a swap-type PTE to pagemap_page_category, so
             * PAGE_IS_SWAPPED comes with PAGE_IS_GUARD; before 6.16 it is
             * all the ioctl can say about a guard. */
            CHECK(cg & PAGE_IS_SWAPPED, "the guard page %#lx is not reported as a swap-type entry (%#llx)", (unsigned long)guard,
                  (unsigned long long)cg);
            if (k == 0)
                printf("pagemap_guard: %zu-byte class: categories of the guard page %#llx, of the slot pages before and after %#llx / %#llx\n",
                       g.size, (unsigned long long)cg, (unsigned long long)cb, (unsigned long long)ca);
            if (have_guard_cat) {
                CHECK(cg & PAGE_IS_GUARD, "the guard page %#lx is not reported PAGE_IS_GUARD (%#llx)", (unsigned long)guard,
                      (unsigned long long)cg);
                CHECK(!(cb & PAGE_IS_GUARD), "the slot page before the guard (%#lx) is reported PAGE_IS_GUARD", (unsigned long)befores[k]);
                CHECK(!(ca & PAGE_IS_GUARD), "the slot page after the guard (%#lx) is reported PAGE_IS_GUARD", (unsigned long)afters[k]);
            }
        }
        size_t guard_pages = have_scan && have_guard_cat ? count_pages_with(lo, hi, PAGE_IS_GUARD) : 0;
        size_t in_slab = have_scan && have_guard_cat ? count_pages_with(guards[0], guards[0] + g.group, PAGE_IS_GUARD) : 0;
        if (have_scan && have_guard_cat) {
            /* One guard page per guard slab (its first page), none anywhere else. */
            CHECK(in_slab >= 1, "the guard slab at %#lx has no PAGE_IS_GUARD page", (unsigned long)guards[0]);
            CHECK(guard_pages == gaps * in_slab, "%zu PAGE_IS_GUARD pages in the span %#lx..%#lx, %zu guard slabs x %zu pages expected",
                  guard_pages, (unsigned long)lo, (unsigned long)hi, gaps, in_slab);
        }
        printf("pagemap_guard on: %zu-byte class, slab %zu, group %zu, guard every %u groups: %zu blocks over %zu groups, %zu guard slab(s), first at %#lx; %s%s\n",
               g.size, g.slab, g.group, g.effective, n, (size_t)((hi - lo) / g.group), gaps, (unsigned long)guards[0],
               !have_scan ? "gap check only (no PAGEMAP_SCAN)"
               : !have_guard_cat ? "guard page not present, a swap-type entry (the marker), neighbours present (no PAGE_IS_GUARD on this kernel)"
                                 : "guard page PAGE_IS_GUARD (a swap-type entry, not present), neighbours present and not guard",
               have_scan && have_guard_cat ? "" : "");
        if (have_scan && have_guard_cat)
            printf("pagemap_guard on: %zu-byte class: %zu PAGE_IS_GUARD page(s) in the span, %zu per guard slab, none elsewhere\n",
                   g.size, guard_pages, in_slab);
    } else {
        CHECK(gaps == 0, "%zu-byte class: %zu one-group gap(s) although no guard slab should exist (first at %#lx)", g.size, gaps,
              gaps ? (unsigned long)guards[0] : 0UL);
        size_t guard_pages = have_scan && have_guard_cat ? count_pages_with(lo, hi, PAGE_IS_GUARD) : 0;
        CHECK(guard_pages == 0, "%zu-byte class: %zu PAGE_IS_GUARD page(s) in a span that should have none", g.size, guard_pages);
        printf("pagemap_guard: %zu-byte class, %zu blocks over %zu groups: no gap%s\n", g.size, n, (size_t)((hi - lo) / g.group),
               have_scan && have_guard_cat ? ", no PAGE_IS_GUARD page in the span" : (have_scan ? " (PAGE_IS_GUARD not known to this kernel)" : " (no PAGEMAP_SCAN)"));
    }
    for (size_t i = 0; i < n; i++)
        free(a[i]);
    free(a);
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: pagemap_guard on|off");
    require_challoc();
    page_size = (size_t)sysconf(_SC_PAGESIZE);
    probe_scan();
    bool install = guard_install_available();
    bool guards = ctl_bool("opt.guards");
    const char *mode = argv[1];
    printf("pagemap_guard %s: %s; MADV_GUARD_INSTALL %s; opt.guards %s, opt.guard_slab_interval %u; page %zu\n", mode, capability_line(),
           install ? "available (6.13+)" : "unavailable (kernel before 6.13: the allocator makes no guard slab)", guards ? "true" : "false",
           ctl_uint("opt.guard_slab_interval"), page_size);

    bool expect;
    if (strcmp(mode, "on") == 0) {
        REQUIRE(guards, "opt.guards is false under the default configuration");
        expect = install;
        if (!install)
            printf("pagemap_guard on: no guard slabs on this kernel; checking that none exists (the fallback branch)\n");
    } else if (strcmp(mode, "off") == 0) {
        REQUIRE(!guards, "opt.guards is true although MALLOC_CONF=guards:false was set");
        expect = false;
    } else {
        REQUIRE(0, "unknown mode %s", mode);
        return 1;
    }
    static const size_t sizes[] = { 16, 64, 256, 1024 };
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++)
        check_class(sizes[i], expect);
    printf("pagemap_guard %s: %s: %s\n", mode, capability_line(),
           expect ? (have_scan && have_guard_cat ? "guard slabs reported as guard regions, both ways"
                                                 : (have_scan ? "guard slabs found by the gap and checked not present, swap-type entries (fallback: no PAGE_IS_GUARD on this kernel)"
                                                              : "guard slabs found by the gap alone (fallback: no PAGEMAP_SCAN)"))
                  : "no guard slab and no guard page in any span");
    return finish("pagemap_guard");
}
