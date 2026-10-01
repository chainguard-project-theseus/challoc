/*
 * Guard slabs end to end, under LD_PRELOAD (design 5 and 14).
 *
 * Every `guard_slab_interval`-th 64 KiB of a class's slabs is a guard slab
 * that is never handed out. It is committed read-write like its neighbours,
 * so the class region's committed prefix stays one VMA, and its first page
 * carries a guard region (`MADV_GUARD_INSTALL`, kernel 6.13+) so a write
 * running off the previous slab faults. On a kernel without the feature
 * there are no guard slabs at all (the `PROT_NONE` alternative would split
 * the region into one VMA per guard).
 *
 *   guard_slab fault   fill the 64-byte class past its first guard, locate
 *                      the guard slab as the one slab-sized gap in the sorted
 *                      addresses, check that the last byte of the last slot
 *                      before it is writable and that the guard's first and
 *                      last page bytes fault (SIGSEGV caught with siglongjmp).
 *                      Skipped with a note where the kernel has no
 *                      MADV_GUARD_INSTALL (the driver skips it too).
 *   guard_slab vmas    commit 10,000 slabs of the 16-byte class (640 MiB of
 *                      address space) and count the /proc/self/maps lines
 *                      that intersect the span: the guard slabs (one in eight
 *                      slabs on a guard kernel, none otherwise) must not split
 *                      the committed range, so the count stays under
 *                      VMA_BOUND. Runs on every kernel and reports which case
 *                      it saw.
 *
 * Nothing here reads allocator internals: the geometry comes from the public
 * mallctl keys (opt.guards, opt.guard_slab_interval, arenas.page,
 * arenas.bin.<i>.{size,slab_size}) and the guard slab is found as the gap
 * that no allocation came from.
 */
#include "common.h"

#include <setjmp.h>
#include <signal.h>
#include <stdbool.h>
#include <sys/mman.h>

/* Kernel 6.13; glibc's headers may predate it. */
#ifndef MADV_GUARD_INSTALL
#define MADV_GUARD_INSTALL 102
#endif

/* The committed span of one class must stay a handful of VMAs however many
 * guard slabs it holds. One is the expected number; the bound leaves room
 * for a neighbouring reservation line on either side and for the kernel
 * splitting at the ends of the span. */
#define VMA_BOUND 8

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

/* Bin index of the class whose slot size is exactly `size`. */
static unsigned bin_of(size_t size)
{
    unsigned nbins = ctl_uint("arenas.nbins");
    for (unsigned i = 0; i < nbins; i++) {
        char name[64];
        snprintf(name, sizeof name, "arenas.bin.%u.size", i);
        if (ctl_size(name) == size)
            return i;
    }
    REQUIRE(0, "no %zu-byte bin", size);
    return 0;
}

/* Guard geometry of one class, from the public keys. `group` is one page
 * group of slabs (the slab, or the page when that is larger); one guard
 * group per `effective` groups (heap::guard_interval_for: one guard per
 * `interval` units of 64 KiB, so classes with smaller groups have
 * proportionally more groups between guards). */
struct geom {
    size_t size, slab, page, group, per_slab;
    unsigned interval, effective;
    bool guards;
};

static struct geom geom_of(size_t size)
{
    struct geom g;
    g.size = size;
    g.guards = ctl_bool("opt.guards");
    g.interval = ctl_uint("opt.guard_slab_interval");
    unsigned bin = bin_of(size);
    char name[64];
    snprintf(name, sizeof name, "arenas.bin.%u.slab_size", bin);
    g.slab = ctl_size(name);
    g.page = ctl_size("arenas.page");
    g.group = g.slab > g.page ? g.slab : g.page;
    g.per_slab = g.slab / size;
    size_t unit = g.page > 65536 ? g.page : 65536;
    g.effective = g.group >= unit ? g.interval : g.interval * (unsigned)(unit / g.group);
    REQUIRE(g.slab >= 4096 && g.slab % size == 0 && g.page >= 4096, "geometry: slab %zu page %zu", g.slab, g.page);
    return g;
}

/* Does this kernel install guard regions? Same probe the allocator makes. */
static int guard_install_available(void)
{
    long page = sysconf(_SC_PAGESIZE);
    void *p = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(p != MAP_FAILED, "mmap scratch");
    int ok = madvise(p, page, MADV_GUARD_INSTALL) == 0;
    munmap(p, 2 * page);
    return ok;
}

static int cmp_ptr(const void *a, const void *b)
{
    uintptr_t x = (uintptr_t)*(void *const *)a, y = (uintptr_t)*(void *const *)b;
    return x < y ? -1 : x > y;
}

/* ---- fault ---------------------------------------------------------- */

static sigjmp_buf fault_jb;
static volatile sig_atomic_t fault_seen;
static void *volatile fault_addr;

static void on_segv(int sig, siginfo_t *si, void *uc)
{
    (void)sig;
    (void)uc;
    fault_addr = si->si_addr;
    fault_seen = 1;
    siglongjmp(fault_jb, 1);
}

/* Write one byte at `addr`; 1 if it faulted, 0 if the write went through. */
static int write_faults(uintptr_t addr)
{
    fault_seen = 0;
    fault_addr = NULL;
    if (sigsetjmp(fault_jb, 1) == 0) {
        *(volatile unsigned char *)addr = 0x5A;
        return 0;
    }
    return 1;
}

static int read_faults(uintptr_t addr)
{
    fault_seen = 0;
    fault_addr = NULL;
    if (sigsetjmp(fault_jb, 1) == 0) {
        sink = (void *)(uintptr_t)(*(volatile unsigned char *)addr);
        return 0;
    }
    return 1;
}

static int mode_fault(void)
{
    if (!guard_install_available()) {
        skip_note("guard_slab/fault", "MADV_GUARD_INSTALL unavailable (kernel < 6.13), no guard slabs exist");
        return finish("guard_slab");
    }
    struct geom g = geom_of(64);
    REQUIRE(g.guards && g.interval >= 2, "guards off (opt.guards %d, interval %u)", g.guards, g.interval);

    /* Enough blocks that the arena walks past at least one guard group
     * however the class was left by earlier allocations. */
    size_t n = g.per_slab * (g.group / g.slab) * (g.effective + 2);
    void **a = malloc(n * sizeof *a);
    REQUIRE(a != NULL, "malloc failed");
    for (size_t i = 0; i < n; i++) {
        a[i] = malloc(g.size);
        REQUIRE(a[i] != NULL, "malloc(%zu) failed", g.size);
    }
    qsort(a, n, sizeof *a, cmp_ptr);

    /* The guard is the one page group of slabs inside the used range that
     * no block came from: consecutive blocks two groups apart. */
    uintptr_t guard = 0, before = 0, after = 0;
    for (size_t i = 1; i < n; i++) {
        uintptr_t lo = (uintptr_t)a[i - 1] & ~(uintptr_t)(g.group - 1);
        uintptr_t hi = (uintptr_t)a[i] & ~(uintptr_t)(g.group - 1);
        if (hi - lo == 2 * g.group) {
            guard = lo + g.group;
            before = (uintptr_t)a[i - 1];
            after = (uintptr_t)a[i];
            break;
        }
    }
    REQUIRE(guard != 0, "no one-group gap among %zu blocks (slab %zu, page %zu, interval %u)", n, g.slab, g.page,
            g.effective);
    /* Slabs fill in address order before the next one is claimed, so the
     * slot right below the guard and the one right above it are ours. */
    CHECK(before + g.size == guard, "highest block below the guard is %#lx, not %#lx", (unsigned long)before,
          (unsigned long)(guard - g.size));
    CHECK(after == guard + g.group, "lowest block above the guard is %#lx, not %#lx", (unsigned long)after,
          (unsigned long)(guard + g.group));

    struct sigaction sa, old;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&sa.sa_mask);
    REQUIRE(sigaction(SIGSEGV, &sa, &old) == 0, "sigaction");

    /* The last byte of the last slot before the guard is ordinary memory. */
    uintptr_t last = guard - 1;
    CHECK(!write_faults(last), "last byte before the guard (%#lx) faulted at %p", (unsigned long)last, fault_addr);
    CHECK(*(volatile unsigned char *)last == 0x5A, "write to %#lx did not stick", (unsigned long)last);

    /* The guard's first page faults on write and on read, first and last byte. */
    int w0 = write_faults(guard);
    void *at = fault_addr;
    CHECK(w0, "write to the guard slab's first byte %#lx did not fault", (unsigned long)guard);
    CHECK(!w0 || at == (void *)guard, "fault address %p, wrote %#lx", at, (unsigned long)guard);
    uintptr_t page_last = guard + g.page - 1;
    int w1 = write_faults(page_last);
    CHECK(w1, "write to the guard page's last byte %#lx did not fault", (unsigned long)page_last);
    int r0 = read_faults(guard);
    CHECK(r0, "read of the guard slab's first byte %#lx did not fault", (unsigned long)guard);
    /* The first slot after the guard is ours and writable. */
    CHECK(!write_faults(after), "first block after the guard (%#lx) faulted", (unsigned long)after);

    REQUIRE(sigaction(SIGSEGV, &old, NULL) == 0, "sigaction restore");
    printf("guard_slab fault: %zu-byte class, slab %zu, guard every %u groups of %zu; guard slab at %#lx: "
           "last slot byte %#lx writable, guard bytes %#lx and %#lx fault (SIGSEGV, si_addr %p), read faults %d\n",
           g.size, g.slab, g.effective, g.group, (unsigned long)guard, (unsigned long)last, (unsigned long)guard,
           (unsigned long)page_last, at, r0);

    for (size_t i = 0; i < n; i++)
        free(a[i]);
    free(a);
    return finish("guard_slab");
}

/* ---- vmas ----------------------------------------------------------- */

/* /proc/self/maps lines whose range intersects [lo, hi); `total` gets the
 * process-wide line count. */
static int maps_lines_in(uintptr_t lo, uintptr_t hi, int *total, int *rw_in)
{
    FILE *f = fopen("/proc/self/maps", "r");
    REQUIRE(f != NULL, "fopen /proc/self/maps");
    char line[512];
    int in = 0, all = 0, rw = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long a, b;
        char perms[8];
        all++;
        if (sscanf(line, "%lx-%lx %7s", &a, &b, perms) != 3)
            continue;
        if (a < hi && b > lo) {
            in++;
            if (perms[0] == 'r' && perms[1] == 'w')
                rw++;
        }
    }
    fclose(f);
    *total = all;
    *rw_in = rw;
    return in;
}

static int mode_vmas(long want_slabs)
{
    int available = guard_install_available();
    struct geom g = geom_of(16);
    /* Slabs seen, indexed from an anchor below the first block: the class
     * region is at least 4 GiB, the map covers 4 GiB around the start. */
    enum { MAP_SLABS = 1 << 16 };
    static unsigned char seen[MAP_SLABS];
    uintptr_t anchor = 0, lo = UINTPTR_MAX, hi = 0;
    size_t n = (size_t)want_slabs * g.per_slab;

    int before_total, before_rw;
    void *first = malloc(g.size);
    REQUIRE(first != NULL, "malloc failed");
    anchor = ((uintptr_t)first & ~(uintptr_t)(g.slab - 1)) - (uintptr_t)(MAP_SLABS / 16) * g.slab;
    maps_lines_in((uintptr_t)first, (uintptr_t)first + 1, &before_total, &before_rw);
    sink = first;

    for (size_t i = 0; i < n; i++) {
        void *p = malloc(g.size);
        REQUIRE(p != NULL, "malloc(%zu) #%zu failed", g.size, i);
        uintptr_t u = (uintptr_t)p;
        REQUIRE(u >= anchor && (u - anchor) / g.slab < MAP_SLABS, "block %p outside the slab map", p);
        seen[(u - anchor) / g.slab] = 1;
        if (u < lo)
            lo = u;
        if (u > hi)
            hi = u;
        sink = p;
    }
    uintptr_t lo_slab = lo & ~(uintptr_t)(g.slab - 1), hi_slab = hi & ~(uintptr_t)(g.slab - 1);
    size_t span = (hi_slab - lo_slab) / g.slab + 1;
    size_t touched = 0;
    for (size_t i = (lo - anchor) / g.slab; i <= (hi - anchor) / g.slab; i++)
        touched += seen[i];
    size_t untouched = span - touched;
    REQUIRE(touched >= (size_t)want_slabs, "%zu slabs allocated from, wanted %ld", touched, want_slabs);

    int total, rw_in;
    int in = maps_lines_in(lo_slab, hi_slab + g.slab, &total, &rw_in);

    /* Guard groups inside the span: one per `effective` groups, at group
     * indices k*effective - 1 from the class base; the span's offset from
     * the base is unknown, so allow one either way. */
    size_t groups = span * g.slab / g.group;
    size_t expect = available && g.guards && g.interval ? groups / g.effective : 0;
    if (available) {
        CHECK(untouched + 1 >= expect && untouched <= expect + 1, "%zu slabs never allocated from in a %zu-slab span, "
              "expected about %zu guard slabs (interval %u)", untouched, span, expect, g.effective);
        CHECK(untouched > 0, "no guard slab in %zu slabs although the kernel installs guard regions", span);
    } else {
        CHECK(untouched == 0, "%zu slabs never allocated from, but no guard slabs exist without MADV_GUARD_INSTALL",
              untouched);
    }
    CHECK(in <= VMA_BOUND, "%d maps lines intersect the %zu-slab span (bound %d)", in, span, VMA_BOUND);
    CHECK(rw_in >= 1, "no rw line intersects the span");
    printf("guard_slab vmas: %s; %zu-byte class, slab %zu; %zu slabs allocated from over a %zu-slab span "
           "(%zu MiB), %zu guard slabs (expected %zu); maps lines intersecting the span %d (%d rw, bound %d); "
           "process maps lines %d -> %d\n",
           available ? "MADV_GUARD_INSTALL available, guard slabs on" : "MADV_GUARD_INSTALL unavailable, no guard slabs",
           g.size, g.slab, touched, span, span * g.slab >> 20, untouched, expect, in, rw_in, VMA_BOUND, before_total,
           total);
    return finish("guard_slab");
}

int main(int argc, char **argv)
{
    REQUIRE(argc >= 2, "usage: guard_slab fault|vmas [slabs]");
    require_challoc();
    if (strcmp(argv[1], "fault") == 0)
        return mode_fault();
    if (strcmp(argv[1], "vmas") == 0)
        return mode_vmas(argc >= 3 ? atol(argv[2]) : 10000);
    REQUIRE(0, "unknown mode %s", argv[1]);
    return 1;
}
