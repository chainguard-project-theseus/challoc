/*
 * Prefaulted fresh slabs (plan Phase 2.5 task 11, design 8; purge.rs
 * `prefault`, class.rs `prefault_next`), both ways. Run as
 * LD_PRELOAD=target/release/libchalloc.so ./prefault MODE with
 *
 *   on    MALLOC_CONF=prefault:true,background_thread:true: after a burst
 *         of one class's refills the background thread's pass commits the
 *         class's next fresh slab and populates it (MADV_POPULATE_WRITE
 *         where the kernel has it, the feature line's populate_write=yes,
 *         else a touch per page), so every page of that slab is resident
 *         (mincore) before any allocation touched it; stats.prefault.slabs
 *         counts it, opt.prefault reads true, the report's `Prefault:`
 *         line says on; and the next allocations of the class, once the
 *         slab in service is used up, come from the prefaulted slab (the
 *         claim path takes the lowest committed empty slab, which it is).
 *   off   the default: nothing past the allocations is resident in the
 *         class region (the slab after the last one in service has no
 *         resident page), stats.prefault.slabs is 0, the line says
 *         `off (prefault:false)`.
 *   inert MALLOC_CONF=prefault:true without background_thread: the key is
 *         stored (opt.prefault true) and nothing runs (stats.prefault
 *         .enabled 0, the line says why), the same residency as off.
 *
 * The class region's geometry comes from the public keys alone
 * (arenas.bin.<i>.{size,slab_size}, opt.guard_slab_interval, arenas.page):
 * slabs are consecutive in the region, so the slab after the highest
 * block's slab is the next fresh one, or the one after that when the next
 * is a guard slab (every guard_slab_interval-th page group). The program
 * uses a class whose slab is one page group (the 64-byte class: 64 KiB
 * slabs at 4 KiB pages) so a slab is one commit unit.
 */
#include "common.h"
#include <sys/mman.h>
#include <time.h>

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));
__attribute__((weak)) const char *challoc_features(void);

typedef void (*stats_print_fn)(void (*cb)(void *, const char *), void *cbopaque, const char *opts);

static char report[1 << 18];
static size_t report_len;

static void stats_cb(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

static size_t stat_of(const char *name)
{
    size_t v = 0, len = sizeof v;
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s)", name);
    return v;
}

/* Resident pages of [addr, addr + len) per mincore; -1 when the range is
 * not mapped at all (it always is: the class region is one reservation). */
static long resident_pages(uintptr_t addr, size_t len, size_t page)
{
    size_t n = len / page;
    unsigned char *vec = malloc(n);
    REQUIRE(vec, "malloc(vec)");
    long r = -1;
    if (mincore((void *)addr, len, vec) == 0) {
        r = 0;
        for (size_t i = 0; i < n; i++)
            r += vec[i] & 1;
    }
    free(vec);
    return r;
}

/* The bin index and slab size of the class serving `size`. */
static void class_of(size_t size, size_t *bin_size, size_t *slab_size)
{
    unsigned nbins = 0;
    size_t len = sizeof nbins;
    REQUIRE(mallctl("arenas.nbins", &nbins, &len, NULL, 0) == 0, "arenas.nbins");
    for (unsigned i = 0; i < nbins; i++) {
        char key[64];
        size_t bs = 0, ss = 0;
        len = sizeof bs;
        snprintf(key, sizeof key, "arenas.bin.%u.size", i);
        REQUIRE(mallctl(key, &bs, &len, NULL, 0) == 0, "%s", key);
        len = sizeof ss;
        snprintf(key, sizeof key, "arenas.bin.%u.slab_size", i);
        REQUIRE(mallctl(key, &ss, &len, NULL, 0) == 0, "%s", key);
        if (bs >= size) {
            *bin_size = bs;
            *slab_size = ss;
            return;
        }
    }
    REQUIRE(0, "no class for %zu", size);
}

static void sleep_ms(long ms)
{
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

#define SIZE 64u

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(argc == 2, "usage: prefault on|off|inert");
    REQUIRE(mallctl != NULL && challoc_features != NULL, "mallctl/challoc_features not exported");
    const char *mode = argv[1];
    int on = strcmp(mode, "on") == 0;

    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t bin_size, slab_size;
    class_of(SIZE, &bin_size, &slab_size);
    size_t per_slab = slab_size / bin_size;
    unsigned guard_interval = 0;
    size_t len = sizeof guard_interval;
    REQUIRE(mallctl("opt.guard_slab_interval", &guard_interval, &len, NULL, 0) == 0, "opt.guard_slab_interval");
    _Bool opt_prefault = 0, opt_bg = 0;
    len = sizeof opt_prefault;
    REQUIRE(mallctl("opt.prefault", &opt_prefault, &len, NULL, 0) == 0, "opt.prefault");
    len = sizeof opt_bg;
    REQUIRE(mallctl("opt.background_thread", &opt_bg, &len, NULL, 0) == 0, "opt.background_thread");
    const char *line = challoc_features();
    const char *pw = strstr(line, " populate_write=");
    REQUIRE(pw != NULL, "no populate_write= in `%s`", line);
    int populate_write = strncmp(pw + 16, "yes", 3) == 0;
    printf("prefault %s: class %zu (slab %zu, %zu slots), page %zu, guard_slab_interval %u, opt.prefault %d, opt.background_thread %d, populate_write=%s\n",
           mode, bin_size, slab_size, per_slab, page, guard_interval, opt_prefault, opt_bg, populate_write ? "yes" : "no");
    CHECK(opt_prefault == (strcmp(mode, "off") != 0), "opt.prefault %d under %s", opt_prefault, mode);
    CHECK(stat_of("stats.prefault.enabled") == (size_t)on, "stats.prefault.enabled %zu under %s", stat_of("stats.prefault.enabled"), mode);
    CHECK(stat_of("stats.prefault.populate_write") == (size_t)populate_write, "stats.prefault.populate_write vs the feature line");

    /* The background thread is created by the periodic tick (every 64
     * slow paths), never by init: warm it up with allocation traffic of
     * other classes until the report says it is present (under `on`), so
     * the burst below is watched from its first commit. */
    if (on) {
        int present = 0;
        /* Batches large enough to overflow the caches both ways: the
         * allocations refill (slow paths) and the frees flush (more), so a
         * few rounds reach the tick; a malloc/free pair of one size would
         * pop and push the same cached slot forever. */
        enum { WARM = 4096 };
        static void *w[WARM];
        for (int round = 0; round < 200 && !present; round++) {
            for (int i = 0; i < WARM; i++) {
                w[i] = malloc(200 + 16 * (i % 40));
                REQUIRE(w[i], "malloc");
            }
            for (int i = 0; i < WARM; i++)
                free(w[i]);
            report_len = 0;
            stats_print_fn msp0 = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
            REQUIRE(msp0 != NULL, "dlsym(malloc_stats_print)");
            msp0(stats_cb, NULL, "a");
            report[report_len] = 0;
            present = strstr(report, "Background thread: present") != NULL;
            if (!present)
                sleep_ms(5);
        }
        report_len = 0;
        REQUIRE(present, "background thread not present after the warm-up");
    }

    /* A burst: 12 slabs' worth of the class, kept live, so every slab
     * came out of a fresh commit. */
    size_t nblocks = 12 * per_slab;
    unsigned char **blocks = malloc(nblocks * sizeof *blocks);
    REQUIRE(blocks, "malloc(blocks)");
    uintptr_t hi = 0;
    for (size_t i = 0; i < nblocks; i++) {
        blocks[i] = malloc(SIZE);
        REQUIRE(blocks[i], "malloc(%u)", SIZE);
        blocks[i][0] = 1;
        if ((uintptr_t)blocks[i] > hi)
            hi = (uintptr_t)blocks[i];
    }
    /* The slab holding the highest block, and the candidates after it:
     * the next slab, or the one after when the next is a guard. Whether a
     * slab is a guard is by its page group: every guard_interval-th page
     * group (1-based) is one. */
    size_t spp = slab_size >= page ? 1 : page / slab_size;
    uintptr_t slab_base = hi & ~(uintptr_t)(slab_size - 1);
    /* The region base is not public, but the slab index modulo the guard
     * interval (a power of two) is what the guard test needs, and the
     * region base is aligned to the class region stride, a power of two
     * above any slab, so the index modulo the interval reads off the
     * address. */
    uintptr_t candidates[2] = {slab_base + slab_size, slab_base + 2 * slab_size};
    int is_guard[2];
    for (int k = 0; k < 2; k++) {
        size_t slab_index = candidates[k] / slab_size;
        size_t page_group = slab_index / spp;
        is_guard[k] = guard_interval && ((page_group + 1) % guard_interval) == 0;
    }
    uintptr_t next = is_guard[0] ? candidates[1] : candidates[0];
    REQUIRE(!(is_guard[0] && is_guard[1]), "two guard slabs in a row");

    /* Give the pass time (it wakes on the commit; the thread itself is
     * created by the periodic tick): poll the slab's residency up to 3 s.
     * Under off and inert wait a fixed 300 ms so a pass that should not
     * exist has had every chance to show. */
    long full = (long)(slab_size / page);
    long res_next = -1;
    size_t slabs = 0;
    for (int i = 0; i < 300; i++) {
        slabs = stat_of("stats.prefault.slabs");
        res_next = resident_pages(next, slab_size, page);
        if (on && res_next == full)
            break;
        if (!on && i >= 30)
            break;
        sleep_ms(10);
    }
    long res_after = resident_pages(next + slab_size, slab_size, page);
    size_t passes = stat_of("stats.prefault.passes");
    size_t wakes = stat_of("stats.prefault.wakes");
    size_t bytes = stat_of("stats.prefault.bytes");
    printf("prefault %s: highest block in slab at %#lx; next fresh slab %#lx%s: %ld of %zu pages resident (the one after: %ld); "
           "stats.prefault passes %zu (active %zu) slabs %zu bytes %zu wakes %zu fallbacks %zu skipped %zu\n",
           mode, (unsigned long)slab_base, (unsigned long)next, is_guard[0] ? " (after a guard slab)" : "", res_next,
           slab_size / page, res_after, passes, stat_of("stats.prefault.active"), slabs, bytes, wakes, stat_of("stats.prefault.fallbacks"),
           stat_of("stats.prefault.skipped"));

    stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
    msp(stats_cb, NULL, "");
    report[report_len] = 0;
    const char *pl = strstr(report, "\nPrefault: ");
    REQUIRE(pl != NULL, "no Prefault: line in malloc_stats_print");
    pl++;
    printf("prefault %s: report `%.*s`\n", mode, (int)strcspn(pl, "\n"), pl);
    const char *bg = strstr(report, "\nBackground thread: ");
    REQUIRE(bg != NULL, "no Background thread: line");
    printf("prefault %s: `%.*s`\n", mode, (int)strcspn(bg + 1, "\n"), bg + 1);

    if (on) {
        CHECK(strncmp(pl, "Prefault: on (", 14) == 0, "line does not say on");
        CHECK(strstr(pl, populate_write ? "MADV_POPULATE_WRITE" : "touch per page") != NULL, "line names the wrong populate");
        CHECK(slabs >= 1, "no slab prefaulted within 3 s (passes %zu, wakes %zu)", passes, wakes);
        CHECK(wakes >= 1, "the commit path issued no wake");
        CHECK(res_next == full, "next fresh slab %#lx: %ld of %ld pages resident before any allocation touched it", (unsigned long)next,
              res_next, full);
        /* One slab per class per pass: the one after it is not resident. */
        CHECK(res_after == 0, "the slab after the prefaulted one has %ld resident pages", res_after);
        /* Its memory reads zero and the next allocations of the class,
         * once the slab in service is used up, come from it. (Read only
         * when resident: an uncommitted slab is PROT_NONE.) */
        if (res_next == full)
            CHECK(is_zero((void *)next, slab_size), "prefaulted slab is not zero");
        size_t from_next = 0, more = 2 * per_slab;
        unsigned char **extra = malloc(more * sizeof *extra);
        REQUIRE(extra, "malloc(extra)");
        for (size_t i = 0; i < more; i++) {
            extra[i] = malloc(SIZE);
            REQUIRE(extra[i], "malloc");
            if ((uintptr_t)extra[i] >= next && (uintptr_t)extra[i] < next + slab_size)
                from_next++;
        }
        printf("prefault on: %zu of the next %zu allocations came from the prefaulted slab\n", from_next, more);
        CHECK(from_next >= per_slab / 2, "the claim path did not take the prefaulted slab (%zu of %zu)", from_next, more);
        for (size_t i = 0; i < more; i++)
            free(extra[i]);
        free(extra);
    } else {
        CHECK(slabs == 0 && passes == 0 && bytes == 0, "prefault counters moved under %s: passes %zu slabs %zu", mode, passes, slabs);
        CHECK(res_next == 0, "next fresh slab has %ld resident pages under %s", res_next, mode);
        if (strcmp(mode, "off") == 0)
            CHECK(strncmp(pl, "Prefault: off (prefault:false)", 30) == 0, "line: `%.40s`", pl);
        else
            CHECK(strncmp(pl, "Prefault: off (prefault:true without background_thread)", 55) == 0, "line: `%.60s`", pl);
    }

    for (size_t i = 0; i < nblocks; i++)
        free(blocks[i]);
    free(blocks);
    return finish("prefault");
}
