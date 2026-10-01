/*
 * The cgroup churn test. Allocate a working set (400 MiB by default) in a
 * mix of 1 KiB slab objects, 32 KiB medium objects and 1 MiB huge runs,
 * touch it, free all of it and idle. In a
 * `docker run --memory=512m` container with the design 11 policy on
 * (the default) RSS must fall below 100 MiB within 2 s: the freed memory
 * exceeds the retention cap (5% of the limit) and the first decay walk
 * purges it. With the policy forced off (`MALLOC_CONF=cgroup:false`) the
 * same run keeps the memory: the default 10 s decay window lets at most a
 * few percent go in 2 s. Both RSS (`/proc/self/statm`) and the cgroup's
 * `memory.current` are printed every 100 ms; the criterion is on RSS.
 *
 *   cgroup_churn MODE [MiB] [threshold_MiB] [--any-allocator]
 *
 *   MODE  purge   exit 0 iff RSS < threshold (100 MiB) at some point in
 *                 the 2 s window (challoc, policy on, under a limit)
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
 * Nothing else here can purge: single-threaded (no thread-exit flush), no
 * `malloc_trim`, no background thread.
 */
#include "common.h"
#include <stdbool.h>
#include <time.h>

#define MIB (1024UL * 1024UL)
enum { WINDOW_MS = 2000, STEP_MS = 100, TRICKLE_MS = 10 };

static char cgroup_dir[8192];
static int have_cgroup;

/* The cgroup directory, found the way the library finds it (design 11). */
static int find_cgroup(void)
{
    char line[4096], path[4096] = "", mount[4096] = "";
    FILE *f = fopen("/proc/self/cgroup", "r");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f))
        if (strncmp(line, "0::/", 4) == 0) {
            snprintf(path, sizeof path, "%s", line + 3);
            path[strcspn(path, "\n")] = 0;
        }
    fclose(f);
    f = fopen("/proc/self/mountinfo", "r");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f)) {
        char *sep = strstr(line, " - ");
        if (sep && strncmp(sep + 3, "cgroup2 ", 8) == 0 && !mount[0]) {
            *sep = 0;
            char *save = NULL, *tok = strtok_r(line, " ", &save);
            for (int i = 0; tok && i < 4; i++)
                tok = strtok_r(NULL, " ", &save);
            if (tok)
                snprintf(mount, sizeof mount, "%s", tok);
        }
    }
    fclose(f);
    if (!path[0] || !mount[0])
        return 0;
    snprintf(cgroup_dir, sizeof cgroup_dir, "%s%s", mount, strcmp(path, "/") == 0 ? "" : path);
    return 1;
}

static long long read_cgroup_ll(const char *file)
{
    if (!have_cgroup)
        return -1;
    char p[8192 + 64];
    snprintf(p, sizeof p, "%s/%s", cgroup_dir, file);
    FILE *f = fopen(p, "r");
    if (!f)
        return -1;
    char buf[64];
    long long v = -1;
    if (fgets(buf, sizeof buf, f)) {
        if (strncmp(buf, "max", 3) == 0)
            v = -2;
        else
            v = atoll(buf);
    }
    fclose(f);
    return v;
}

static const char *fmt_mib(long long v, char *buf, size_t n)
{
    if (v == -2)
        snprintf(buf, n, "max");
    else if (v < 0)
        snprintf(buf, n, "n/a");
    else
        snprintf(buf, n, "%lld MiB", v / (long long)MIB);
    return buf;
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
    have_cgroup = find_cgroup();
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

    /* Idle for the window, printing both figures every 100 ms. */
    size_t threshold = threshold_mib * MIB, rss = rss_freed, rss_min = rss_freed;
    long below_at = -1;
    long next_print = freed_at + STEP_MS;
    for (;;) {
        sleep_ms(TRICKLE_MS);
        trickle();
        long t = now_ms();
        rss = rss_bytes();
        if (rss < rss_min)
            rss_min = rss;
        if (rss < threshold && below_at < 0)
            below_at = t - freed_at;
        if (t >= next_print) {
            printf("  t+%4ld ms: rss %4zu MiB, memory.current %s\n", t - freed_at, rss / MIB,
                   fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));
            next_print += STEP_MS;
        }
        if (t - freed_at >= WINDOW_MS)
            break;
    }
    long long cur_end = read_cgroup_ll("memory.current");
    printf("at %d ms: rss %zu MiB (min %zu MiB%s%ld ms), memory.current %s\n", WINDOW_MS, rss / MIB, rss_min / MIB,
           below_at >= 0 ? ", under the threshold after " : ", never under the threshold; ", below_at >= 0 ? below_at : (long)WINDOW_MS,
           fmt_mib(cur_end, b1, sizeof b1));
    challoc_policy_line();
    /* One machine-readable line for the driver's table. */
    printf("RESULT churn mode=%s mib=%zu threshold=%zu rss_peak=%zu rss_2s=%zu rss_min=%zu below_ms=%ld current_peak=%lld current_2s=%lld\n",
           mode, total_mib, threshold_mib, rss_peak / MIB, rss / MIB, rss_min / MIB, below_at, cur_peak < 0 ? -1 : cur_peak / (long long)MIB,
           cur_end < 0 ? -1 : cur_end / (long long)MIB);

    if (strcmp(mode, "purge") == 0)
        CHECK(below_at >= 0, "rss never fell below %zu MiB within %d ms (min %zu MiB, at %d ms %zu MiB)", threshold_mib, WINDOW_MS,
              rss_min / MIB, WINDOW_MS, rss / MIB);
    else if (strcmp(mode, "keep") == 0)
        CHECK(rss >= threshold, "rss %zu MiB at %d ms is under %zu MiB with the policy off", rss / MIB, WINDOW_MS, threshold_mib);
    return finish("cgroup_churn");
}
