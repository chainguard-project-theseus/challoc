/*
 * Phase 1 task 5: the jemalloc and glibc extras under LD_PRELOAD, from C.
 * Every non-standard symbol is declared weak so this program links against
 * nothing but libc: bare, `require_challoc` fails first; under the preload
 * the weak references resolve to libchalloc.so. Exercised here:
 *
 *   - mallocx / rallocx / xallocx / sallocx / dallocx / sdallocx / nallocx
 *     with every MALLOCX_* flag kind, plus their je_ aliases;
 *   - mallctl by name, mallctlnametomib + mallctlbymib with the numeric
 *     component patched in exactly as Valkey's defragmenter does it,
 *     including experimental.utilization.batch_query over live pointers;
 *   - stats.* frozen until epoch, arenas.create / lookup, arena.<i>.purge,
 *     thread.* counters and pointers, tcache.create / flush / destroy,
 *     the jemalloc error codes;
 *   - malloc_stats_print with a callback, with "J", and through the
 *     malloc_message hook that the program installs;
 *   - malloc_conf defined by the library and null, je_malloc_conf only
 *     referenced (a weak reference stays null);
 *   - mallopt, mallinfo, mallinfo2, malloc_info, malloc_stats;
 *   - get_defrag_hint / je_get_defrag_hint (Redis's extension, design 13.5)
 *     checked against experimental.utilization.query for the same pointers.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <sched.h>
#include "common.h"

/* The jemalloc entry points and the MALLOCX_* flags are declared (weak) in
 * common.h; the configuration symbols and the je_ aliases are this
 * program's own. */
extern const char *malloc_conf WEAK;
extern const char *je_malloc_conf WEAK;

extern void *je_malloc(size_t) WEAK;
extern void je_free(void *) WEAK;
extern void *je_calloc(size_t, size_t) WEAK;
extern void *je_realloc(void *, size_t) WEAK;
extern int je_posix_memalign(void **, size_t, size_t) WEAK;
extern void *je_aligned_alloc(size_t, size_t) WEAK;
extern void *je_memalign(size_t, size_t) WEAK;
extern void *je_valloc(size_t) WEAK;
extern void *je_pvalloc(size_t) WEAK;
extern size_t je_malloc_usable_size(void *) WEAK;
extern void *je_mallocx(size_t, int) WEAK;
extern void *je_rallocx(void *, size_t, int) WEAK;
extern size_t je_xallocx(void *, size_t, size_t, int) WEAK;
extern size_t je_sallocx(const void *, int) WEAK;
extern void je_dallocx(void *, int) WEAK;
extern void je_sdallocx(void *, size_t, int) WEAK;
extern size_t je_nallocx(size_t, int) WEAK;
extern int je_mallctl(const char *, void *, size_t *, void *, size_t) WEAK;
extern int je_mallctlnametomib(const char *, size_t *, size_t *) WEAK;
extern int je_mallctlbymib(const size_t *, size_t, void *, size_t *, void *, size_t) WEAK;
extern void je_malloc_stats_print(void (*)(void *, const char *), void *, const char *) WEAK;
extern void (*je_malloc_message)(void *, const char *) WEAK;
extern int get_defrag_hint(void *) WEAK;
extern int je_get_defrag_hint(void *) WEAK;

#define MEDIUM_MAX (256 * 1024)

static size_t get_size(const char *name)
{
    size_t v = 0, len = sizeof v;
    int rc = mallctl(name, &v, &len, NULL, 0);
    REQUIRE(rc == 0, "mallctl(%s) = %d", name, rc);
    return v;
}

static unsigned get_unsigned(const char *name)
{
    unsigned v = 0;
    size_t len = sizeof v;
    int rc = mallctl(name, &v, &len, NULL, 0);
    REQUIRE(rc == 0, "mallctl(%s) = %d", name, rc);
    return v;
}

static uint64_t epoch(void)
{
    uint64_t e = 1;
    size_t len = sizeof e;
    REQUIRE(mallctl("epoch", &e, &len, &e, sizeof e) == 0, "epoch");
    return e;
}

static void test_allocx(void)
{
    static const size_t sizes[] = { 0, 1, 16, 100, 1000, 4096, 65536, MEDIUM_MAX, MEDIUM_MAX + 1, 1 << 20 };
    static const int lgs[] = { 0, 4, 6, 12, 16, 20 };
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        for (size_t j = 0; j < sizeof lgs / sizeof lgs[0]; j++) {
            int flags = MALLOCX_LG_ALIGN(lgs[j]);
            void *p = mallocx(sizes[i], flags);
            REQUIRE(p != NULL, "mallocx(%zu, lg %d)", sizes[i], lgs[j]);
            CHECK(((uintptr_t)p & ((1u << lgs[j]) - 1)) == 0, "alignment %zu lg %d", sizes[i], lgs[j]);
            size_t usable = sallocx(p, flags);
            CHECK(usable >= sizes[i], "sallocx %zu < %zu", usable, sizes[i]);
            CHECK(usable == nallocx(sizes[i], flags), "nallocx mismatch %zu lg %d", sizes[i], lgs[j]);
            CHECK(usable == malloc_usable_size(p), "usable mismatch");
            CHECK(xallocx(p, sizes[i] + 1, 0, flags) == usable, "xallocx reports the current size");
            fill(p, usable, (uint32_t)i);
            dallocx(p, flags);
            void *z = je_mallocx(sizes[i], flags | MALLOCX_ZERO);
            REQUIRE(z != NULL, "je_mallocx");
            CHECK(is_zero(z, je_sallocx(z, 0)), "MALLOCX_ZERO %zu lg %d", sizes[i], lgs[j]);
            je_sdallocx(z, sizes[i], flags);
        }
    }
    /* rallocx keeps content and honours ZERO for the grown tail. */
    unsigned char *p = mallocx(10, 0);
    REQUIRE(p != NULL, "mallocx(10)");
    for (int i = 0; i < 10; i++)
        p[i] = (unsigned char)i;
    unsigned char *q = rallocx(p, 5000, MALLOCX_ZERO);
    REQUIRE(q != NULL, "rallocx");
    CHECK(q != p, "class change moves");
    for (int i = 0; i < 10; i++)
        CHECK(q[i] == i, "rallocx content byte %d", i);
    CHECK(is_zero(q + 10, sallocx(q, 0) - 10), "rallocx ZERO tail");
    unsigned char *r = je_rallocx(q, MEDIUM_MAX + 1, MALLOCX_LG_ALIGN(16));
    REQUIRE(r != NULL, "je_rallocx huge");
    CHECK(((uintptr_t)r & 0xffff) == 0, "huge rallocx alignment");
    CHECK(r[9] == 9, "huge rallocx content");
    CHECK(je_xallocx(r, 1, 0, 0) == je_sallocx(r, 0), "je_xallocx");
    je_dallocx(r, 0);
    CHECK(nallocx(SIZE_MAX, 0) == 0, "nallocx(SIZE_MAX) = 0");
    CHECK(je_nallocx(1, 0) == QUANTUM, "je_nallocx(1)");
    dallocx(NULL, 0);
    sdallocx(NULL, 0, 0);
    /* je_ aliases of the standard calls. */
    void *a = je_malloc(100);
    REQUIRE(a != NULL, "je_malloc");
    CHECK(je_malloc_usable_size(a) == malloc_usable_size(a), "je_malloc_usable_size");
    a = je_realloc(a, 5000);
    REQUIRE(a != NULL, "je_realloc");
    je_free(a);
    void *c = je_calloc(100, 100);
    REQUIRE(c != NULL && is_zero(c, 10000), "je_calloc");
    je_free(c);
    void *m = NULL;
    CHECK(je_posix_memalign(&m, 4096, 100) == 0 && ((uintptr_t)m & 4095) == 0, "je_posix_memalign");
    je_free(m);
    m = je_aligned_alloc(256, 100);
    CHECK(m != NULL && ((uintptr_t)m & 255) == 0, "je_aligned_alloc");
    je_free(m);
    m = je_memalign(512, 100);
    CHECK(m != NULL && ((uintptr_t)m & 511) == 0, "je_memalign");
    je_free(m);
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    m = je_valloc(100);
    CHECK(m != NULL && ((uintptr_t)m & (page - 1)) == 0, "je_valloc");
    je_free(m);
    m = je_pvalloc(page + 1);
    CHECK(m != NULL && ((uintptr_t)m & (page - 1)) == 0 && je_malloc_usable_size(m) >= 2 * page,
          "je_pvalloc: %p usable %zu", m, m ? je_malloc_usable_size(m) : 0);
    je_free(m);
}

static void test_mallctl_basics(void)
{
    const char *version = NULL;
    size_t len = sizeof version;
    REQUIRE(mallctl("version", &version, &len, NULL, 0) == 0, "version");
    CHECK(version && strncmp(version, "5.3.0-0-g", 9) == 0, "version %s", version ? version : "(null)");
    CHECK(strstr(version, "challoc") != NULL, "version names challoc: %s", version);
    /* Classes up to 4096 bytes (64 KiB slabs): 28 for quantum16, 32 for quantum8. */
    CHECK(get_unsigned("arenas.nbins") >= 28 && get_unsigned("arenas.nbins") <= 32, "nbins");
    CHECK(get_size("arenas.quantum") == QUANTUM, "quantum");
    CHECK(get_size("arenas.page") == (size_t)sysconf(_SC_PAGESIZE), "page");
    bool b = true;
    len = sizeof b;
    CHECK(mallctl("config.debug", &b, &len, NULL, 0) == 0 && !b, "config.debug");
    CHECK(mallctl("opt.tcache", &b, &len, NULL, 0) == 0 && b, "opt.tcache");
    ssize_t decay = 0;
    len = sizeof decay;
    CHECK(mallctl("opt.dirty_decay_ms", &decay, &len, NULL, 0) == 0 && decay == 10000, "opt.dirty_decay_ms %zd", decay);
    unsigned narenas = 0;
    len = sizeof narenas;
    /* The default resolves to min(4 * ncpus, 64) from the affinity mask. */
    unsigned want_narenas = 4;
    cpu_set_t set;
    if (sched_getaffinity(0, sizeof set, &set) == 0) {
        unsigned n = 4u * (unsigned)CPU_COUNT(&set);
        want_narenas = n < 64 ? n : 64;
    }
    CHECK(mallctl("opt.narenas", &narenas, &len, NULL, 0) == 0 && narenas == want_narenas, "opt.narenas %u (want %u)", narenas, want_narenas);
    const char *thp = NULL;
    len = sizeof thp;
    CHECK(mallctl("opt.thp", &thp, &len, NULL, 0) == 0 && thp && strcmp(thp, "never") == 0, "opt.thp");
    bool bg = true;
    len = sizeof bg;
    CHECK(je_mallctl("background_thread", &bg, &len, NULL, 0) == 0 && !bg, "background_thread");
    /* Error codes. */
    CHECK(mallctl("no.such.key", NULL, NULL, NULL, 0) == ENOENT, "ENOENT");
    CHECK(mallctl("prof.active", NULL, NULL, NULL, 0) == ENOENT, "prof.* ENOENT");
    CHECK(mallctl("arenas", NULL, NULL, NULL, 0) == ENOENT, "interior node");
    uint32_t small = 0;
    len = sizeof small;
    CHECK(mallctl("arenas.page", &small, &len, NULL, 0) == EINVAL, "EINVAL on size mismatch");
    CHECK(small == (uint32_t)sysconf(_SC_PAGESIZE), "prefix still copied");
    uint64_t one = 1;
    CHECK(mallctl("version", NULL, NULL, &one, sizeof one) == EPERM, "EPERM on read-only");
    CHECK(mallctl("stats.allocated", NULL, NULL, &one, sizeof one) == EPERM, "EPERM on stats");
    uint32_t bad = 1;
    CHECK(mallctl("epoch", NULL, NULL, &bad, sizeof bad) == EINVAL, "EINVAL on newlen");
    CHECK(mallctl("arena.0.purge", NULL, NULL, &one, sizeof one) == EPERM, "void key with a value");
    CHECK(mallctl("arena.4000.purge", NULL, NULL, NULL, 0) == ENOENT, "no such arena");
}

static void test_stats_and_epoch(void)
{
    uint64_t e0 = epoch();
    size_t a0 = get_size("stats.allocated");
    enum { N = 400, SZ = 300 };
    void *volatile blocks[N];
    for (int i = 0; i < N; i++) {
        blocks[i] = malloc(SZ);
        REQUIRE(blocks[i] != NULL, "malloc");
        memset(blocks[i], 1, SZ);
    }
    CHECK(get_size("stats.allocated") == a0, "frozen until epoch");
    uint64_t e1 = epoch();
    CHECK(e1 == e0 + 1, "epoch increments %llu -> %llu", (unsigned long long)e0, (unsigned long long)e1);
    size_t allocated = get_size("stats.allocated");
    size_t active = get_size("stats.active");
    size_t resident = get_size("stats.resident");
    size_t mapped = get_size("stats.mapped");
    size_t retained = get_size("stats.retained");
    size_t metadata = get_size("stats.metadata");
    CHECK(allocated >= a0 + N * SZ, "allocated %zu -> %zu", a0, allocated);
    CHECK(resident >= active && active >= allocated, "resident %zu >= active %zu >= allocated %zu", resident, active, allocated);
    CHECK(mapped == resident + retained, "mapped %zu == resident %zu + retained %zu", mapped, resident, retained);
    CHECK(metadata > 0, "metadata");
    /* What Redis/Valkey read for INFO memory. */
    size_t pdirty = get_size("stats.arenas.4096.pdirty");
    size_t pmuzzy = get_size("stats.arenas.4096.pmuzzy");
    size_t pactive = get_size("stats.arenas.4096.pactive");
    CHECK(pmuzzy == 0, "pmuzzy");
    CHECK(pactive * (size_t)sysconf(_SC_PAGESIZE) == active, "pactive %zu pages vs active %zu", pactive, active);
    (void)pdirty;
    CHECK(get_size("stats.arenas.0.small.allocated") + get_size("stats.arenas.0.large.allocated") <= allocated, "arena 0 within totals");
    CHECK(get_size("stats.arenas.4096.resident") == resident, "ARENAS_ALL resident");
    for (int i = 0; i < N; i++)
        free(blocks[i]);
}

/* Valkey's allocator_defrag.c pattern: resolve each name once, then patch
 * the arena and bin indices into the MIB before every mallctlbymib. */
static void test_valkey_defrag_pattern(void)
{
    unsigned nbins = get_unsigned("arenas.nbins");
    size_t size_mib[8], nregs_mib[8], slab_mib[8], curregs_mib[8], curslabs_mib[8], nonfull_mib[8], util_mib[8];
    size_t size_len = 8, nregs_len = 8, slab_len = 8, curregs_len = 8, curslabs_len = 8, nonfull_len = 8, util_len = 8;
    REQUIRE(je_mallctlnametomib("arenas.bin.0.size", size_mib, &size_len) == 0 && size_len == 4, "size mib");
    REQUIRE(mallctlnametomib("arenas.bin.0.nregs", nregs_mib, &nregs_len) == 0 && nregs_len == 4, "nregs mib");
    REQUIRE(mallctlnametomib("arenas.bin.0.slab_size", slab_mib, &slab_len) == 0, "slab_size mib");
    REQUIRE(mallctlnametomib("stats.arenas.0.bins.0.curregs", curregs_mib, &curregs_len) == 0 && curregs_len == 6, "curregs mib");
    REQUIRE(mallctlnametomib("stats.arenas.0.bins.0.curslabs", curslabs_mib, &curslabs_len) == 0, "curslabs mib");
    REQUIRE(mallctlnametomib("stats.arenas.0.bins.0.nonfull_slabs", nonfull_mib, &nonfull_len) == 0, "nonfull mib");
    REQUIRE(mallctlnametomib("experimental.utilization.batch_query", util_mib, &util_len) == 0 && util_len == 3, "util mib");

    enum { N = 1000, SZ = 100 };
    void *ptrs[N];
    for (int i = 0; i < N; i++) {
        ptrs[i] = mallocx(SZ, MALLOCX_TCACHE_NONE);
        REQUIRE(ptrs[i] != NULL, "mallocx");
    }
    epoch();

    size_t prev = 0;
    unsigned my_bin = nbins;
    size_t my_nregs = 0, my_slab = 0, my_size = 0;
    for (unsigned j = 0; j < nbins; j++) {
        size_t size = 0, slab = 0, len = sizeof size;
        uint32_t nregs = 0;
        size_t nlen = sizeof nregs;
        size_mib[2] = j;
        nregs_mib[2] = j;
        slab_mib[2] = j;
        REQUIRE(mallctlbymib(size_mib, size_len, &size, &len, NULL, 0) == 0, "bin %u size", j);
        REQUIRE(mallctlbymib(nregs_mib, nregs_len, &nregs, &nlen, NULL, 0) == 0, "bin %u nregs", j);
        REQUIRE(je_mallctlbymib(slab_mib, slab_len, &slab, &len, NULL, 0) == 0, "bin %u slab_size", j);
        CHECK(size > prev, "bin sizes increase (%zu after %zu)", size, prev);
        CHECK(size * nregs <= slab, "bin %u: %zu * %u > %zu", j, size, nregs, slab);
        prev = size;
        if (my_bin == nbins && size >= SZ) {
            my_bin = j;
            my_nregs = nregs;
            my_slab = slab;
            my_size = size;
        }
        size_t curregs = 0, curslabs = 0, nonfull = 0;
        curregs_mib[2] = 0;
        curregs_mib[4] = j;
        curslabs_mib[2] = 0;
        curslabs_mib[4] = j;
        nonfull_mib[2] = 0;
        nonfull_mib[4] = j;
        REQUIRE(mallctlbymib(curregs_mib, curregs_len, &curregs, &len, NULL, 0) == 0, "curregs %u", j);
        REQUIRE(mallctlbymib(curslabs_mib, curslabs_len, &curslabs, &len, NULL, 0) == 0, "curslabs %u", j);
        REQUIRE(mallctlbymib(nonfull_mib, nonfull_len, &nonfull, &len, NULL, 0) == 0, "nonfull %u", j);
        CHECK(curregs <= curslabs * nregs, "bin %u curregs %zu > capacity", j, curregs);
        CHECK(nonfull <= curslabs, "bin %u nonfull %zu > curslabs %zu", j, nonfull, curslabs);
        if (j == my_bin)
            CHECK(curregs >= N, "our bin has our %d slots (curregs %zu)", N, curregs);
    }
    REQUIRE(my_bin < nbins, "no bin for %d bytes", SZ);

    size_t out[3 * N];
    size_t outlen = sizeof out;
    REQUIRE(mallctlbymib(util_mib, util_len, out, &outlen, ptrs, sizeof ptrs) == 0, "batch_query");
    size_t full = 0;
    for (int i = 0; i < N; i++) {
        size_t nfree = out[3 * i], nregs = out[3 * i + 1], size = out[3 * i + 2];
        CHECK(nregs == my_nregs, "ptr %d nregs %zu != %zu", i, nregs, my_nregs);
        /* The region bytes of the slab (nregs * size), not the slab size:
         * Valkey divides size by nregs and expects the bin size. */
        CHECK(size == my_nregs * my_size && size <= my_slab, "ptr %d size %zu != %zu", i, size, my_nregs * my_size);
        CHECK(nfree < nregs, "ptr %d nfree %zu", i, nfree);
        if (nfree == 0)
            full++;
    }
    CHECK(full < (size_t)N, "not every slab can be full with %d live", N);
    /* Single verbose query: jemalloc's extent_util_stats_verbose_t. */
    struct {
        void *slabcur_addr;
        size_t nfree, nregs, size, bin_nfree, bin_nregs;
    } u;
    size_t ulen = sizeof u;
    void *ptr0 = ptrs[0];
    REQUIRE(mallctl("experimental.utilization.query", &u, &ulen, &ptr0, sizeof ptr0) == 0, "utilization.query");
    CHECK(u.nregs == my_nregs && u.size == my_nregs * my_size, "verbose geometry");
    CHECK(u.bin_nregs >= u.nregs && u.bin_nfree <= u.bin_nregs, "verbose bin figures");
    CHECK(u.bin_nregs - u.bin_nfree >= N, "bin has our slots out");
    for (int i = 0; i < N; i++)
        dallocx(ptrs[i], MALLOCX_TCACHE_NONE);
}

/* Design 13.5: the hint KeyDB's defragmenter reads, against the
 * utilization query for the same pointer. A private arena keeps the bin
 * figures this test's own; every other slot of the first half is freed, so
 * those slabs are half full and the second half's stay full. With
 * jemalloc's rule (live * curslabs' <= curregs', the primes excluding the
 * current slab) and the query's figures including it, a hint of 1 implies
 * live * curslabs <= curregs + nregs, and a 0 on a slab with a free slot
 * that is not the current one implies live * curslabs > curregs - nregs;
 * the Rust unit test has the exact reference. */
static void test_defrag_hint(void)
{
    unsigned arena = 0;
    size_t len = sizeof arena;
    REQUIRE(mallctl("arenas.create", &arena, &len, NULL, 0) == 0, "arenas.create");
    int flags = MALLOCX_ARENA(arena) | MALLOCX_TCACHE_NONE;
    enum { SZ = 100 };
    size_t nregs = 0;
    size_t slab_size = 0;
    {
        /* The bin of SZ: its nregs and slab_size. */
        unsigned nbins = get_unsigned("arenas.nbins");
        char name[64];
        for (unsigned j = 0; j < nbins; j++) {
            snprintf(name, sizeof name, "arenas.bin.%u.size", j);
            if (get_size(name) >= SZ) {
                snprintf(name, sizeof name, "arenas.bin.%u.nregs", j);
                nregs = get_unsigned(name);
                snprintf(name, sizeof name, "arenas.bin.%u.slab_size", j);
                slab_size = get_size(name);
                break;
            }
        }
    }
    REQUIRE(nregs > 0 && slab_size > 0, "bin geometry for %d bytes", SZ);
    size_t n = 20 * nregs;
    void **ptrs = malloc(n * sizeof *ptrs);
    REQUIRE(ptrs, "malloc");
    for (size_t i = 0; i < n; i++) {
        ptrs[i] = mallocx(SZ, flags);
        REQUIRE(ptrs[i], "mallocx");
    }
    for (size_t i = 0; i < n / 2; i += 2) {
        dallocx(ptrs[i], flags);
    }
    CHECK(get_defrag_hint(ptrs[0]) == 0, "a freed slot is not a hint");
    size_t hints = 0, full_hinted = 0, sparse_seen = 0;
    for (size_t i = 0; i < n; i++) {
        if (i < n / 2 && i % 2 == 0)
            continue;
        void *p = ptrs[i];
        int hint = get_defrag_hint(p);
        CHECK(hint == 0 || hint == 1, "ptr %zu: hint %d", i, hint);
        CHECK(hint == je_get_defrag_hint(p), "ptr %zu: both spellings agree", i);
        struct {
            void *slabcur_addr;
            size_t nfree, nregs, size, bin_nfree, bin_nregs;
        } u;
        size_t ulen = sizeof u;
        REQUIRE(mallctl("experimental.utilization.query", &u, &ulen, &p, sizeof p) == 0, "utilization.query %zu", i);
        REQUIRE(u.nregs == nregs, "ptr %zu: nregs %zu != %zu", i, u.nregs, nregs);
        size_t live = u.nregs - u.nfree;
        size_t curslabs = u.bin_nregs / u.nregs, curregs = u.bin_nregs - u.bin_nfree;
        int in_slabcur = u.slabcur_addr && (char *)p >= (char *)u.slabcur_addr && (char *)p < (char *)u.slabcur_addr + slab_size;
        if (hint) {
            hints++;
            CHECK(u.nfree > 0, "ptr %zu: hinted with a full slab", i);
            CHECK(!in_slabcur, "ptr %zu: hinted in the current slab", i);
            CHECK(live * curslabs <= curregs + nregs, "ptr %zu: hinted above the average (%zu * %zu > %zu + %zu)", i, live, curslabs, curregs, nregs);
        } else if (u.nfree > 0 && !in_slabcur) {
            CHECK(live * curslabs + nregs > curregs, "ptr %zu: not hinted below the average (%zu * %zu + %zu <= %zu)", i, live, curslabs, nregs, curregs);
        }
        if (u.nfree == 0 && hint)
            full_hinted++;
        if (i < n / 2)
            sparse_seen++;
    }
    CHECK(full_hinted == 0, "%zu hints on full slabs", full_hinted);
    CHECK(hints >= nregs / 2 * 8, "%zu hints for %zu slots in half-full slabs", hints, sparse_seen);
    /* Not a hint: a huge run, a class above the bins, null, the stack. */
    void *h = mallocx(3 << 20, flags);
    void *m = mallocx(100 << 10, flags);
    REQUIRE(h && m, "huge and large");
    int on_stack = 0;
    CHECK(get_defrag_hint(h) == 0, "huge run");
    CHECK(get_defrag_hint(m) == 0, "class above the bins");
    CHECK(get_defrag_hint(NULL) == 0, "null");
    CHECK(get_defrag_hint(&on_stack) == 0, "stack");
    dallocx(h, flags);
    dallocx(m, flags);
    for (size_t i = 0; i < n; i++) {
        if (i < n / 2 && i % 2 == 0)
            continue;
        dallocx(ptrs[i], flags);
    }
    free(ptrs);
}

static void test_arenas_threads_tcaches(void)
{
    unsigned arena = 0;
    size_t len = sizeof arena;
    REQUIRE(mallctl("arenas.create", &arena, &len, NULL, 0) == 0 && arena >= 1, "arenas.create");
    CHECK(get_unsigned("arenas.narenas") > arena, "narenas covers the new arena");
    void *p = mallocx(200, MALLOCX_ARENA(arena));
    void *h = mallocx(4 << 20, MALLOCX_ARENA(arena) | MALLOCX_TCACHE_NONE);
    /* 64 KiB is above tcache_max (32 KiB): a shared class, which the
     * default path takes from arena 0; MALLOCX_ARENA must keep it here. */
    void *m = mallocx(64 << 10, MALLOCX_ARENA(arena));
    REQUIRE(p && h && m, "arena allocations");
    unsigned owner = 99;
    len = sizeof owner;
    void *q = p;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == 0 && owner == arena, "arenas.lookup small -> %u", owner);
    q = h;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == 0 && owner == arena, "arenas.lookup huge -> %u", owner);
    q = m;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == 0 && owner == arena, "arenas.lookup 64 KiB (above tcache_max) -> %u", owner);
    void *d0 = mallocx(64 << 10, 0);
    q = d0;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == 0 && owner == 0, "default path 64 KiB -> arena 0 (got %u)", owner);
    dallocx(d0, 0);
    int on_stack = 0;
    q = &on_stack;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == EFAULT, "arenas.lookup garbage");
    epoch();
    char key[64];
    snprintf(key, sizeof key, "stats.arenas.%u.small.allocated", arena);
    CHECK(get_size(key) >= 200, "%s", key);
    snprintf(key, sizeof key, "stats.arenas.%u.large.allocated", arena);
    CHECK(get_size(key) >= (4u << 20) + (64u << 10), "%s covers the 64 KiB MALLOCX_ARENA allocation", key);
    ssize_t ms = 123;
    snprintf(key, sizeof key, "arena.%u.dirty_decay_ms", arena);
    CHECK(mallctl(key, NULL, NULL, &ms, sizeof ms) == 0, "set %s", key);
    ssize_t back = 0;
    len = sizeof back;
    CHECK(mallctl(key, &back, &len, NULL, 0) == 0 && back == 123, "%s = %zd", key, back);
    dallocx(p, 0);
    dallocx(h, 0);
    dallocx(m, 0);
    snprintf(key, sizeof key, "arena.%u.purge", arena);
    CHECK(mallctl(key, NULL, NULL, NULL, 0) == 0, "%s", key);
    CHECK(mallctl("arena.4096.purge", NULL, NULL, NULL, 0) == 0, "arena.4096.purge (MEMORY PURGE)");
    CHECK(mallctl("arena.0.decay", NULL, NULL, NULL, 0) == 0, "arena.0.decay");
    CHECK(mallctl("arena.0.destroy", NULL, NULL, NULL, 0) == EFAULT, "arena.0.destroy");

    /* thread.* */
    uint64_t allocated = 0, *allocatedp = NULL;
    len = sizeof allocated;
    REQUIRE(mallctl("thread.allocated", &allocated, &len, NULL, 0) == 0, "thread.allocated");
    len = sizeof allocatedp;
    REQUIRE(mallctl("thread.allocatedp", &allocatedp, &len, NULL, 0) == 0 && allocatedp, "thread.allocatedp");
    CHECK(*allocatedp == allocated, "allocatedp agrees");
    void *volatile t = malloc(1000);
    uint64_t again = 0;
    len = sizeof again;
    REQUIRE(mallctl("thread.allocated", &again, &len, NULL, 0) == 0, "thread.allocated again");
    CHECK(again >= allocated + 1000, "thread.allocated grew %llu -> %llu", (unsigned long long)allocated, (unsigned long long)again);
    CHECK(*allocatedp == again, "allocatedp refreshed by the read");
    free(t);
    uint64_t deallocated = 0;
    len = sizeof deallocated;
    REQUIRE(mallctl("thread.deallocated", &deallocated, &len, NULL, 0) == 0 && deallocated >= 1000, "thread.deallocated");
    CHECK(mallctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "thread.tcache.flush");
    bool enabled = false;
    len = sizeof enabled;
    CHECK(mallctl("thread.tcache.enabled", &enabled, &len, NULL, 0) == 0 && enabled, "thread.tcache.enabled");
    unsigned home = 99;
    len = sizeof home;
    CHECK(mallctl("thread.arena", &home, &len, NULL, 0) == 0 && home == 0, "thread.arena = %u", home);
    CHECK(mallctl("thread.arena", NULL, NULL, &arena, sizeof arena) == 0, "set thread.arena");
    void *volatile in_arena = malloc(300);
    len = sizeof owner;
    q = in_arena;
    CHECK(mallctl("arenas.lookup", &owner, &len, &q, sizeof q) == 0 && owner == arena, "malloc lands in the thread's arena");
    free(in_arena);
    CHECK(mallctl("thread.arena", NULL, NULL, &home, sizeof home) == 0, "restore thread.arena");
    unsigned bad_arena = 4000;
    CHECK(mallctl("thread.arena", NULL, NULL, &bad_arena, sizeof bad_arena) == EFAULT, "thread.arena to a missing arena");

    /* tcache.* */
    unsigned tc = 0;
    len = sizeof tc;
    REQUIRE(mallctl("tcache.create", &tc, &len, NULL, 0) == 0, "tcache.create");
    void *e = mallocx(200, MALLOCX_TCACHE(tc));
    REQUIRE(e != NULL, "mallocx with an explicit cache");
    dallocx(e, MALLOCX_TCACHE(tc));
    CHECK(mallctl("tcache.flush", NULL, NULL, &tc, sizeof tc) == 0, "tcache.flush");
    CHECK(mallctl("tcache.destroy", NULL, NULL, &tc, sizeof tc) == 0, "tcache.destroy");
    CHECK(mallctl("tcache.destroy", NULL, NULL, &tc, sizeof tc) == EFAULT, "tcache.destroy twice");
}

static char report[1 << 16];
static size_t report_len;
static void *report_opaque;

static void append(void *opaque, const char *s)
{
    report_opaque = opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
        report[report_len] = 0;
    }
}

static void test_reports(void)
{
    int marker = 0;
    report_len = 0;
    report[0] = 0;
    malloc_stats_print(append, &marker, NULL);
    CHECK(report_opaque == &marker, "cbopaque passed through");
    CHECK(strstr(report, "___ Begin jemalloc statistics ___") == report, "report start:\n%s", report);
    CHECK(strstr(report, "Allocated: ") != NULL, "Allocated line");
    CHECK(strstr(report, "arenas[0]:") != NULL, "arena section");
    CHECK(strstr(report, "--- End jemalloc statistics ---\n") != NULL, "report end");
    report_len = 0;
    report[0] = 0;
    je_malloc_stats_print(append, &marker, "J");
    CHECK(strstr(report, "\"jemalloc\": {") != NULL, "JSON report:\n%s", report);
    CHECK(strstr(report, "\"stats.arenas\"") != NULL, "JSON arenas");
    /* With no callback the report goes through malloc_message, or the
     * je_ spelling of it. */
    CHECK(&malloc_message != NULL && malloc_message == NULL, "malloc_message defaults to NULL");
    CHECK(&je_malloc_message != NULL && je_malloc_message == NULL, "je_malloc_message defaults to NULL");
    malloc_message = append;
    report_len = 0;
    report[0] = 0;
    report_opaque = &marker;
    malloc_stats_print(NULL, NULL, "ga");
    malloc_message = NULL;
    CHECK(report_opaque == NULL, "malloc_message gets a NULL cbopaque");
    CHECK(strcmp(report, "--- End jemalloc statistics ---\n") == 0, "hooked report with `ga`:\n%s", report);
    je_malloc_message = append;
    report_len = 0;
    report[0] = 0;
    je_malloc_stats_print(NULL, NULL, "gab");
    je_malloc_message = NULL;
    CHECK(strcmp(report, "--- End jemalloc statistics ---\n") == 0, "je_malloc_message hooked report:\n%s", report);
    /* The library reads a program's malloc_conf / je_malloc_conf (conf.c
     * covers that). It defines malloc_conf itself, null, so a program that
     * only reads it links (Ruby's configure probe); je_malloc_conf is only
     * referenced, so this weak reference stays null. */
    CHECK(&malloc_conf != NULL && malloc_conf == NULL, "malloc_conf is defined by the library and null");
    CHECK(&je_malloc_conf == NULL, "je_malloc_conf is not defined by the library");

    /* glibc extras. */
    CHECK(mallopt(M_TRIM_THRESHOLD, 0) == 1 && mallopt(M_ARENA_MAX, 1) == 1, "mallopt");
    void *volatile big = malloc(1 << 20);
    struct mallinfo2 mi = mallinfo2();
    CHECK(mi.hblks >= 1 && mi.hblkhd >= (1u << 20), "mallinfo2 hblks %zu hblkhd %zu", mi.hblks, mi.hblkhd);
    CHECK(mi.arena >= mi.uordblks && mi.fordblks == mi.arena - mi.uordblks, "mallinfo2 identities");
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    struct mallinfo m = mallinfo();
#pragma GCC diagnostic pop
    CHECK(m.hblks == (int)mi.hblks, "mallinfo agrees with mallinfo2");
    free(big);
    FILE *f = tmpfile();
    REQUIRE(f != NULL, "tmpfile");
    CHECK(malloc_info(0, f) == 0, "malloc_info");
    rewind(f);
    static char xml[1 << 16];
    size_t n = fread(xml, 1, sizeof xml - 1, f);
    xml[n] = 0;
    fclose(f);
    CHECK(strncmp(xml, "<malloc version=\"1\">", 20) == 0, "malloc_info xml:\n%s", xml);
    CHECK(strstr(xml, "</malloc>") != NULL, "malloc_info closes");
    CHECK(malloc_info(1, stdout) == -1 && errno == EINVAL, "malloc_info options");
    malloc_stats();
}

int main(void)
{
    require_challoc();
    REQUIRE(mallocx && rallocx && xallocx && sallocx && dallocx && sdallocx && nallocx, "*allocx exported");
    REQUIRE(mallctl && mallctlnametomib && mallctlbymib && malloc_stats_print, "mallctl family exported");
    REQUIRE(je_malloc && je_free && je_calloc && je_realloc && je_posix_memalign && je_aligned_alloc && je_malloc_usable_size, "je_ libc aliases exported");
    REQUIRE(je_memalign && je_valloc && je_pvalloc, "je_memalign/je_valloc/je_pvalloc exported");
    REQUIRE(je_mallocx && je_rallocx && je_xallocx && je_sallocx && je_dallocx && je_sdallocx && je_nallocx, "je_*allocx exported");
    REQUIRE(je_mallctl && je_mallctlnametomib && je_mallctlbymib && je_malloc_stats_print, "je_mallctl family exported");
    REQUIRE(get_defrag_hint && je_get_defrag_hint, "get_defrag_hint exported under both spellings");
    test_allocx();
    test_mallctl_basics();
    test_stats_and_epoch();
    test_valkey_defrag_pattern();
    test_defrag_hint();
    test_arenas_threads_tcaches();
    test_reports();
    return finish("jemalloc_api");
}
