/*
 * The cgroup churn test. Allocate a working set (400 MiB by default) in a
 * mix of 1 KiB slab objects, 32 KiB medium objects and 1 MiB huge runs,
 * touch it, free all of it and idle. In a
 * `docker run --memory=512m` container with the design 11 policy on
 * (the default) held memory (RSS less `LazyFree`, below) must fall below
 * 100 MiB within 2 s, and some of it must go eagerly: the freed memory
 * exceeds the retention cap (5% of the limit) and the first decay walk
 * purges it. With the policy forced off (`MALLOC_CONF=cgroup:false`) the
 * same run keeps the memory: the default 10 s decay window lets at most a
 * few percent go in 2 s. RSS (`/proc/self/statm`), its `LazyFree` part
 * (`/proc/self/smaps_rollup`) and the cgroup's `memory.current` are
 * printed every 100 ms. The purge criterion is on *held* memory, RSS less
 * `LazyFree`; the keep criterion is on RSS.
 *
 * Why held memory: the purge is eager by default. With `lazy_band:true`
 * it is eager while the excess over the cap was first seen at 70% of the
 * limit or above, and lazy (`MADV_FREE`, which stays in RSS until the
 * kernel reclaims it) under 70% (design 8 and 11): when walks return the
 * slab and medium objects before the huge runs are freed, retention comes
 * back within the cap, which ends that eager return, and the huge runs'
 * excess is then a new one, seen under 70%, that goes lazily. Either way
 * the allocator no longer holds the memory; lazily freed pages stay in RSS
 * and `memory.current` until the kernel reclaims them, so the criterion
 * holds under both settings. The eager return at 70% and above is checked
 * through `stats.cgroup.eager_purges`, which must rise after the frees.
 * So a passing run may end with RSS well over the threshold; the RSS fall
 * itself is pinned by `cgroup_policy.c` (after freeing at 80% of the
 * limit, RSS back to its base plus the cap and one extent) and by `cgroup_lazy.c`'s `eager` control
 * (RSS under a quarter of the churn with the default eager purges).
 *
 *   cgroup_churn MODE [MiB] [threshold_MiB] [--any-allocator]
 *
 *   MODE  purge   exit 0 iff RSS less LazyFree < threshold (100 MiB) at
 *                 some point in the 2 s window and `eager_purges` rose
 *                 (challoc, policy on, under a limit)
 *         keep    exit 0 iff RSS >= threshold at the end of the window
 *                 (the control: challoc with cgroup:false)
 *         report  no criterion; print and exit 0 (jemalloc, mimalloc)
 *
 *   --any-allocator  skip `require_challoc` (the runs under other
 *                    allocators).
 *
 * `report` and `--any-allocator` are bench probes, not tests: they print
 * the RSS curve for comparison (`make test-cgroup-compare`) and pass
 * whatever it shows. The tests are `purge` and `keep` under challoc.
 *
 * Idle means what an idle server does: the program sleeps in 10 ms steps
 * and, at each step, allocates and frees a burst of small objects (an
 * event loop's transient allocations). This matters: without a background
 * thread challoc's decay runs from the periodic tick of the slow path
 * (design 6.4, every 64 slow paths), and a curve walk waits until its
 * oldest `Empty` member is one step old, so a process that frees and then
 * makes no allocation at all purges nothing under either setting. The
 * trickle is the same in both modes; what differs is the policy: on, the
 * first walk after the frees sees retention over the cap and purges the
 * whole coalesced extent; off, the 10 s curve keeps 94% of it at 2 s.
 * The free path's own periodic tick can run walks during the frees, and
 * the background thread may start on its own after them (the lazy start,
 * design 8.1: only an allocation's tick creates it); nothing else here
 * can purge: one program thread (no thread-exit flush), no `malloc_trim`.
 */
#include "cgroup.h"
#include <stdbool.h>
#include <time.h>

#define MIB (1024UL * 1024UL)
enum { WINDOW_MS = 2000, STEP_MS = 100, TRICKLE_MS = 10 };

static int have_cgroup;

/* The `LazyFree` bytes of `/proc/self/smaps_rollup`: resident pages
 * marked `MADV_FREE` and not yet reclaimed. 0 when the kernel does not
 * report it. */
static size_t lazyfree_bytes(void)
{
    FILE *f = fopen("/proc/self/smaps_rollup", "r");
    if (!f)
        return 0;
    char line[256];
    unsigned long kb = 0;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "LazyFree: %lu kB", &kb) == 1)
            break;
    fclose(f);
    return (size_t)kb * 1024;
}

/* RSS less its `LazyFree` part: the memory the process still holds. */
static size_t held_bytes(size_t rss)
{
    size_t lazy = lazyfree_bytes();
    return lazy < rss ? rss - lazy : 0;
}

/* One idle step: the event loop's transient allocations, 4096 objects of
 * 48 bytes allocated and freed, about 16 slow paths of the 48-byte class
 * under challoc (a refill or flush every 512 operations), so the periodic
 * tick (every 64 slow paths) runs every fourth step. */
static void trickle(void)
{
    enum { N = 4096 };
    static void *volatile blocks[N];
    for (int i = 0; i < N; i++) {
        blocks[i] = malloc(48);
        REQUIRE(blocks[i] != NULL, "malloc(48)");
    }
    for (int i = 0; i < N; i++)
        free(blocks[i]);
}

static void challoc_policy_line(void)
{
    if (!mallctl)
        return;
    size_t v, len = sizeof v;
    int r = mallctl("stats.cgroup.limit", &v, &len, NULL, 0);
    if (r == ENOENT) {
        printf("challoc: stats.cgroup.* ENOENT (cgroup not tracked)\n");
        return;
    }
    if (r != 0)
        return;
    size_t limit = v, decay_ms = 0, cap = 0, empty = 0, full = 0, advice = 0, lazy = 0, eager = 0;
    len = sizeof v;
    if (mallctl("stats.cgroup.decay_ms", &v, &len, NULL, 0) == 0)
        decay_ms = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.retention_cap", &v, &len, NULL, 0) == 0)
        cap = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.empty_bytes", &v, &len, NULL, 0) == 0)
        empty = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.full_purges", &v, &len, NULL, 0) == 0)
        full = v;
    /* The two-advice purge: which advice the next batch takes and how
     * many batches each has issued. A failing run
     * shows here whether the freed memory was returned lazily (resident
     * until the kernel reclaims it) rather than not purged at all. */
    len = sizeof v;
    if (mallctl("stats.cgroup.purge_advice", &v, &len, NULL, 0) == 0)
        advice = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.lazy_purges", &v, &len, NULL, 0) == 0)
        lazy = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.eager_purges", &v, &len, NULL, 0) == 0)
        eager = v;
    printf("challoc: stats.cgroup.limit %zu MiB decay_ms %zu retention_cap %zu MiB empty_bytes %zu MiB full_purges %zu "
           "purge_advice %s lazy_purges %zu eager_purges %zu\n",
           limit == SIZE_MAX ? 0 : limit / MIB, decay_ms, cap == SIZE_MAX ? 0 : cap / MIB, empty / MIB, full,
           advice ? "lazy" : "eager", lazy, eager);
}

/* `stats.cgroup.eager_purges`: the batches discarded with `MADV_DONTNEED`
 * so far; 0 when it cannot be read. */
static size_t eager_purges(void)
{
    size_t v = 0, len = sizeof v;
    if (!mallctl || mallctl("stats.cgroup.eager_purges", &v, &len, NULL, 0) != 0)
        return 0;
    return v;
}

int main(int argc, char **argv)
{
    const char *mode = NULL;
    size_t total_mib = 400, threshold_mib = 100;
    bool any = false;
    int pos = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--any-allocator") == 0)
            any = true;
        else if (pos == 0)
            mode = argv[i], pos++;
        else if (pos == 1)
            total_mib = strtoul(argv[i], NULL, 10), pos++;
        else if (pos == 2)
            threshold_mib = strtoul(argv[i], NULL, 10), pos++;
        else
            REQUIRE(0, "usage: cgroup_churn purge|keep|report [MiB] [threshold_MiB] [--any-allocator]");
    }
    REQUIRE(mode != NULL, "usage: cgroup_churn purge|keep|report [MiB] [threshold_MiB] [--any-allocator]");
    REQUIRE(strcmp(mode, "purge") == 0 || strcmp(mode, "keep") == 0 || strcmp(mode, "report") == 0, "unknown mode %s", mode);
    REQUIRE(total_mib >= 4 && threshold_mib >= 1, "sizes");
    if (!any)
        require_challoc();
    have_cgroup = find_cgroup(false);
    char b1[32], b2[32];
    long long limit = read_cgroup_ll("memory.max"), cur0 = read_cgroup_ll("memory.current");
    size_t rss0 = rss_bytes();
    printf("cgroup_churn %s: %zu MiB churn, threshold %zu MiB; cgroup %s memory.max %s memory.current %s, rss %zu MiB\n", mode,
           total_mib, threshold_mib, have_cgroup ? cgroup_dir : "(none)", fmt_mib(limit, b1, sizeof b1),
           fmt_mib(cur0, b2, sizeof b2), rss0 / MIB);

    /* The mix by bytes: half in 1 MiB runs (huge allocator), a quarter in
     * 32 KiB objects (a medium class), a quarter in 1 KiB objects (a slab
     * class). Every byte is written. */
    size_t huge_n = total_mib / 2, med_n = (total_mib * MIB / 4) / (32 * 1024), small_n = (total_mib * MIB / 4) / 1024;
    void **huge_p = calloc(huge_n, sizeof *huge_p);
    void **med_p = calloc(med_n, sizeof *med_p);
    void **small_p = calloc(small_n, sizeof *small_p);
    REQUIRE(huge_p && med_p && small_p, "tables");
    long t0 = now_ms();
    for (size_t i = 0; i < huge_n; i++) {
        huge_p[i] = malloc(MIB);
        REQUIRE(huge_p[i] != NULL, "malloc(1 MiB) #%zu", i);
        memset(huge_p[i], 0x5a, MIB);
    }
    for (size_t i = 0; i < med_n; i++) {
        med_p[i] = malloc(32 * 1024);
        REQUIRE(med_p[i] != NULL, "malloc(32 KiB) #%zu", i);
        memset(med_p[i], 0x5b, 32 * 1024);
    }
    for (size_t i = 0; i < small_n; i++) {
        small_p[i] = malloc(1024);
        REQUIRE(small_p[i] != NULL, "malloc(1 KiB) #%zu", i);
        memset(small_p[i], 0x5c, 1024);
    }
    size_t rss_peak = rss_bytes();
    long long cur_peak = read_cgroup_ll("memory.current");
    printf("allocated %zu MiB in %ld ms (%zu x 1 MiB, %zu x 32 KiB, %zu x 1 KiB): rss %zu MiB, memory.current %s\n", total_mib,
           now_ms() - t0, huge_n, med_n, small_n, rss_peak / MIB, fmt_mib(cur_peak, b1, sizeof b1));
    challoc_policy_line();
    size_t eager_peak = eager_purges();
    REQUIRE(rss_peak >= rss0 + total_mib * MIB * 9 / 10, "rss did not grow with the working set (%zu MiB)", rss_peak / MIB);

    /* Free everything, in an order that leaves each region's free pieces
     * adjacent (the huge runs coalesce into one extent). */
    t0 = now_ms();
    for (size_t i = 0; i < small_n; i++)
        free(small_p[i]);
    for (size_t i = 0; i < med_n; i++)
        free(med_p[i]);
    for (size_t i = 0; i < huge_n; i++)
        free(huge_p[i]);
    free(small_p);
    free(med_p);
    free(huge_p);
    long freed_at = now_ms();
    size_t rss_freed = rss_bytes();
    printf("freed in %ld ms: rss %zu MiB, memory.current %s\n", freed_at - t0, rss_freed / MIB,
           fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));

    /* Idle for the window, printing the figures every 100 ms. */
    size_t threshold = threshold_mib * MIB, rss = rss_freed, rss_min = rss_freed;
    size_t held = held_bytes(rss_freed), held_min = held;
    long below_at = -1;
    long next_print = freed_at + STEP_MS;
    for (;;) {
        sleep_ms(TRICKLE_MS);
        trickle();
        long t = now_ms();
        rss = rss_bytes();
        held = held_bytes(rss);
        if (rss < rss_min)
            rss_min = rss;
        if (held < held_min)
            held_min = held;
        if (held < threshold && below_at < 0)
            below_at = t - freed_at;
        if (t >= next_print) {
            printf("  t+%4ld ms: rss %4zu MiB (lazyfree %4zu MiB), memory.current %s\n", t - freed_at, rss / MIB, (rss - held) / MIB,
                   fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));
            next_print += STEP_MS;
        }
        if (t - freed_at >= WINDOW_MS)
            break;
    }
    long long cur_end = read_cgroup_ll("memory.current");
    printf("at %d ms: rss %zu MiB, held %zu MiB (min %zu MiB%s%ld ms), memory.current %s\n", WINDOW_MS, rss / MIB, held / MIB, held_min / MIB,
           below_at >= 0 ? ", under the threshold after " : ", never under the threshold; ", below_at >= 0 ? below_at : (long)WINDOW_MS,
           fmt_mib(cur_end, b1, sizeof b1));
    challoc_policy_line();
    /* One machine-readable line for the driver's table. */
    printf("RESULT churn mode=%s mib=%zu threshold=%zu rss_peak=%zu rss_2s=%zu rss_min=%zu below_ms=%ld current_peak=%lld current_2s=%lld "
           "held_2s=%zu held_min=%zu\n",
           mode, total_mib, threshold_mib, rss_peak / MIB, rss / MIB, rss_min / MIB, below_at, cur_peak < 0 ? -1 : cur_peak / (long long)MIB,
           cur_end < 0 ? -1 : cur_end / (long long)MIB, held / MIB, held_min / MIB);

    if (strcmp(mode, "purge") == 0) {
        CHECK(below_at >= 0, "held memory (rss less lazyfree) never fell below %zu MiB within %d ms (min %zu MiB, at %d ms %zu MiB, rss %zu MiB)",
              threshold_mib, WINDOW_MS, held_min / MIB, WINDOW_MS, held / MIB, rss / MIB);
        size_t eager_end = eager_purges();
        CHECK(eager_end > eager_peak, "no eager purge after the frees (eager_purges %zu at the peak, %zu at %d ms)", eager_peak, eager_end,
              WINDOW_MS);
    } else if (strcmp(mode, "keep") == 0)
        CHECK(rss >= threshold, "rss %zu MiB at %d ms is under %zu MiB with the policy off", rss / MIB, WINDOW_MS, threshold_mib);
    return finish("cgroup_churn");
}
