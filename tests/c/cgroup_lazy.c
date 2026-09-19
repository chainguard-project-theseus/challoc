/*
 * Phase 2.5 task 10: the two-advice purge under the cgroup policy's
 * headroom band, end to end in a `docker run --memory=512m` container
 * (scripts/cgroup-tests.sh `lazy`, `make test-cgroup-lazy`). The program
 * allocates a working set (200 MiB by default: half 1 MiB runs, a quarter
 * 32 KiB objects, a quarter 1 KiB objects, every byte written), frees all
 * of it and idles with the churn test's trickle (an event loop's transient
 * allocations, so the periodic tick runs the decay walks). With 200 MiB
 * freed in a 512 MiB cgroup usage is about 50% of the limit: the policy's
 * `Low` band, where retention over the 5% cap is purged at the first walk
 * and, since task 10, with `MADV_FREE`:
 *
 *   cgroup_lazy MODE [MiB] [--any-allocator]
 *
 *   MODE  lazy    defaults: within 2 s of the frees `stats.cgroup.lazy_purges`
 *                 rises, `stats.cgroup.purge_advice` reads 1 (lazy),
 *                 `stats.cgroup.empty_bytes` is under the cap, and yet RSS
 *                 and `memory.current` stay where they were (the pages are
 *                 mapped and charged until the kernel reclaims them) while
 *                 `memory.stat` `pglazyfree` grows by the purged pages (the
 *                 kernel's own count of pages marked lazy-free). Then the
 *                 working set is allocated again in the same shape and
 *                 timed (`reuse_ms`: the refault saving, since the pages are
 *                 still there) and freed again. Then a set of a class the
 *                 program has not used (8 KiB objects, 380 MiB) is
 *                 allocated and written: with 200 MiB lazy-free, 380 MiB
 *                 new and the container's page cache the demand exceeds the
 *                 limit, so the kernel must reclaim the lazy pages
 *                 (`pglazyfreed` rises) and the program survives (exit 0,
 *                 not 137); that is the proof the pages were reclaimable.
 *                 `memory.reclaim` is tried first when it is writable (a
 *                 privileged container on a host without `nsdelegate`).
 *         eager   the control, run with MALLOC_CONF=lazy_band:false: the
 *                 same run, `eager_purges` rises, `purge_advice` reads 0,
 *                 RSS falls under the churn test's threshold (a quarter of
 *                 the churn) within 2 s, `pglazyfree` does not move; the
 *                 reuse is timed too (the refaults the lazy mode saves).
 *
 *   --any-allocator  skip `require_challoc` (no criterion then: `report`).
 *
 * Single-threaded, no `malloc_trim`, no background thread: the periodic
 * tick is the only purge path, as in cgroup_churn.c.
 */
#include "common.h"
#include <fcntl.h>
#include <stdbool.h>
#include <time.h>

#define WEAK __attribute__((weak))
extern int mallctl(const char *, void *, size_t *, void *, size_t) WEAK;

#define MIB (1024UL * 1024UL)
enum { WINDOW_MS = 2000, STEP_MS = 100, TRICKLE_MS = 10, PRESSURE_MIB = 380, PRESSURE_OBJ = 8192 };

static long page_size;
static char cgroup_dir[8192];
static int have_cgroup;

static size_t rss_bytes(void)
{
    FILE *f = fopen("/proc/self/statm", "r");
    REQUIRE(f != NULL, "open /proc/self/statm: %s", strerror(errno));
    unsigned long size, resident;
    REQUIRE(fscanf(f, "%lu %lu", &size, &resident) == 2, "parse statm");
    fclose(f);
    return (size_t)resident * (size_t)page_size;
}

static void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR)
        ;
}

static long now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

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

/* The kernel's view of this cgroup's memory (memory.stat, cgroup v2): the
 * anonymous and file bytes, the inactive file list (where lazy-free pages
 * sit until reclaim) and the lazy-free counters, in pages: pglazyfree is
 * pages marked with MADV_FREE, pglazyfreed pages of those the kernel then
 * reclaimed. -1 when a field is absent. */
struct memstat {
    long long anon, file, inactive_file, pglazyfree, pglazyfreed;
};

static struct memstat read_memstat(void)
{
    struct memstat m = { -1, -1, -1, -1, -1 };
    if (!have_cgroup)
        return m;
    char p[8192 + 64];
    snprintf(p, sizeof p, "%s/memory.stat", cgroup_dir);
    FILE *f = fopen(p, "r");
    if (!f)
        return m;
    char key[64];
    long long v;
    while (fscanf(f, "%63s %lld", key, &v) == 2) {
        if (strcmp(key, "anon") == 0)
            m.anon = v;
        else if (strcmp(key, "file") == 0)
            m.file = v;
        else if (strcmp(key, "inactive_file") == 0)
            m.inactive_file = v;
        else if (strcmp(key, "pglazyfree") == 0)
            m.pglazyfree = v;
        else if (strcmp(key, "pglazyfreed") == 0)
            m.pglazyfreed = v;
    }
    fclose(f);
    return m;
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

static void print_memstat(const char *when, const struct memstat *m)
{
    char b1[32], b2[32], b3[32];
    printf("  memory.stat %s: anon %s file %s inactive_file %s pglazyfree %lld pglazyfreed %lld (pages)\n", when,
           fmt_mib(m->anon, b1, sizeof b1), fmt_mib(m->file, b2, sizeof b2), fmt_mib(m->inactive_file, b3, sizeof b3),
           m->pglazyfree, m->pglazyfreed);
}

/* One idle step: the churn test's trickle, 4096 objects of 48 bytes, so
 * the periodic tick runs every few steps. */
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

struct cg_stats {
    size_t limit, cap, empty, advice, lazy, eager;
    int ok;
};

static struct cg_stats challoc_stats(void)
{
    struct cg_stats s = { 0, 0, 0, 0, 0, 0, 0 };
    if (!mallctl)
        return s;
    size_t v, len = sizeof v;
    if (mallctl("stats.cgroup.limit", &v, &len, NULL, 0) != 0)
        return s;
    s.limit = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.retention_cap", &v, &len, NULL, 0) == 0)
        s.cap = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.empty_bytes", &v, &len, NULL, 0) == 0)
        s.empty = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.purge_advice", &v, &len, NULL, 0) == 0)
        s.advice = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.lazy_purges", &v, &len, NULL, 0) == 0)
        s.lazy = v;
    len = sizeof v;
    if (mallctl("stats.cgroup.eager_purges", &v, &len, NULL, 0) == 0)
        s.eager = v;
    s.ok = 1;
    return s;
}

static void print_stats(const char *when, const struct cg_stats *s)
{
    if (!s->ok) {
        printf("  challoc %s: stats.cgroup.* unavailable\n", when);
        return;
    }
    printf("  challoc %s: limit %zu MiB retention_cap %zu MiB empty_bytes %zu MiB purge_advice %s lazy_purges %zu eager_purges %zu\n",
           when, s->limit == SIZE_MAX ? 0 : s->limit / MIB, s->cap == SIZE_MAX ? 0 : s->cap / MIB, s->empty / MIB,
           s->advice ? "lazy" : "eager", s->lazy, s->eager);
}

/* The working set: the churn test's mix by bytes. */
struct set {
    size_t huge_n, med_n, small_n;
    void **huge_p, **med_p, **small_p;
};

static long grab(struct set *s, size_t total_mib, unsigned char fill)
{
    s->huge_n = total_mib / 2;
    s->med_n = (total_mib * MIB / 4) / (32 * 1024);
    s->small_n = (total_mib * MIB / 4) / 1024;
    s->huge_p = calloc(s->huge_n, sizeof *s->huge_p);
    s->med_p = calloc(s->med_n, sizeof *s->med_p);
    s->small_p = calloc(s->small_n, sizeof *s->small_p);
    REQUIRE(s->huge_p && s->med_p && s->small_p, "tables");
    long t0 = now_ms();
    for (size_t i = 0; i < s->huge_n; i++) {
        s->huge_p[i] = malloc(MIB);
        REQUIRE(s->huge_p[i] != NULL, "malloc(1 MiB) #%zu", i);
        memset(s->huge_p[i], fill, MIB);
    }
    for (size_t i = 0; i < s->med_n; i++) {
        s->med_p[i] = malloc(32 * 1024);
        REQUIRE(s->med_p[i] != NULL, "malloc(32 KiB) #%zu", i);
        memset(s->med_p[i], fill + 1, 32 * 1024);
    }
    for (size_t i = 0; i < s->small_n; i++) {
        s->small_p[i] = malloc(1024);
        REQUIRE(s->small_p[i] != NULL, "malloc(1 KiB) #%zu", i);
        memset(s->small_p[i], fill + 2, 1024);
    }
    return now_ms() - t0;
}

static void release(struct set *s)
{
    for (size_t i = 0; i < s->small_n; i++)
        free(s->small_p[i]);
    for (size_t i = 0; i < s->med_n; i++)
        free(s->med_p[i]);
    for (size_t i = 0; i < s->huge_n; i++)
        free(s->huge_p[i]);
    free(s->small_p);
    free(s->med_p);
    free(s->huge_p);
    memset(s, 0, sizeof *s);
}

/* Idle for the window with the trickle, printing every 100 ms; returns
 * the RSS at the end and the first time a purge of the wanted kind was
 * seen (or the RSS crossed under `threshold`, whichever the mode uses). */
static size_t idle(long window_ms, size_t threshold, size_t lazy0, size_t eager0, bool lazy, long *event_at, size_t *rss_min)
{
    char b1[32];
    long start = now_ms(), next_print = start + STEP_MS;
    size_t rss = rss_bytes();
    *rss_min = rss;
    *event_at = -1;
    for (;;) {
        sleep_ms(TRICKLE_MS);
        trickle();
        long t = now_ms();
        rss = rss_bytes();
        if (rss < *rss_min)
            *rss_min = rss;
        if (*event_at < 0) {
            struct cg_stats s = challoc_stats();
            bool purged = s.ok && (lazy ? s.lazy > lazy0 : s.eager > eager0);
            if (purged || (!lazy && rss < threshold))
                *event_at = t - start;
        }
        if (t >= next_print) {
            printf("  t+%4ld ms: rss %4zu MiB, memory.current %s\n", t - start, rss / MIB,
                   fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));
            next_print += STEP_MS;
        }
        if (t - start >= window_ms)
            break;
    }
    return rss;
}

/* Ask the kernel to reclaim `bytes` from this cgroup (memory.reclaim,
 * 5.19+). 1 on success, 0 when the file is absent or not writable (the
 * default Docker profile mounts the cgroup read-only). */
static int try_memory_reclaim(size_t bytes)
{
    if (!have_cgroup)
        return 0;
    char p[8192 + 64], v[32];
    snprintf(p, sizeof p, "%s/memory.reclaim", cgroup_dir);
    int fd = open(p, O_WRONLY);
    if (fd < 0) {
        printf("  memory.reclaim: not writable (%s)\n", strerror(errno));
        return 0;
    }
    int n = snprintf(v, sizeof v, "%zu", bytes);
    ssize_t w = write(fd, v, (size_t)n);
    int err = errno;
    close(fd);
    if (w != n) {
        printf("  memory.reclaim: write failed (%s)\n", strerror(err));
        return 0;
    }
    printf("  memory.reclaim: reclaimed %zu MiB on request\n", bytes / MIB);
    return 1;
}

int main(int argc, char **argv)
{
    const char *mode = NULL;
    size_t total_mib = 200;
    bool any = false;
    int pos = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--any-allocator") == 0)
            any = true;
        else if (pos == 0)
            mode = argv[i], pos++;
        else if (pos == 1)
            total_mib = strtoul(argv[i], NULL, 10), pos++;
        else
            REQUIRE(0, "usage: cgroup_lazy lazy|eager|report [MiB] [--any-allocator]");
    }
    REQUIRE(mode != NULL, "usage: cgroup_lazy lazy|eager|report [MiB] [--any-allocator]");
    bool lazy = strcmp(mode, "lazy") == 0, eager = strcmp(mode, "eager") == 0;
    REQUIRE(lazy || eager || strcmp(mode, "report") == 0, "unknown mode %s", mode);
    REQUIRE(total_mib >= 16, "sizes");
    if (!any)
        require_challoc();
    page_size = sysconf(_SC_PAGESIZE);
    have_cgroup = find_cgroup();
    char b1[32], b2[32];
    long long limit = read_cgroup_ll("memory.max"), cur0 = read_cgroup_ll("memory.current");
    size_t rss0 = rss_bytes();
    printf("cgroup_lazy %s: %zu MiB churn; cgroup %s memory.max %s memory.current %s, rss %zu MiB, page %ld\n", mode, total_mib,
           have_cgroup ? cgroup_dir : "(none)", fmt_mib(limit, b1, sizeof b1), fmt_mib(cur0, b2, sizeof b2), rss0 / MIB, page_size);
    struct memstat ms0 = read_memstat();
    print_memstat("at start", &ms0);
    struct cg_stats st0 = challoc_stats();
    print_stats("at start", &st0);
    if ((lazy || eager) && (!st0.ok || limit <= 0)) {
        printf("cgroup_lazy: no tracked cgroup limit here; the modes with a criterion need `docker run --memory=L`\n");
        REQUIRE(st0.ok && limit > 0, "no tracked cgroup limit");
    }
    /* The band the purge runs in: the working set plus what is already
     * charged must stay under 70% of the limit, else the run is not the
     * one described (say so instead of failing on the advice). */
    if (limit > 0) {
        long long usage = (cur0 < 0 ? 0 : cur0) + (long long)(total_mib * MIB);
        printf("  expected usage at the frees: %lld MiB of %lld MiB (%lld%%)%s\n", usage / (long long)MIB, limit / (long long)MIB,
               usage * 100 / limit, usage * 10 >= limit * 7 ? " -- ABOVE the 70% band, the criterion does not apply" : "");
        REQUIRE(usage * 10 < limit * 7, "usage would be in or above the scaled band; lower the churn or raise the limit");
    }

    /* 1. Allocate, free, idle: the purge and its advice. */
    struct set s;
    long alloc_ms = grab(&s, total_mib, 0x5a);
    size_t rss_peak = rss_bytes();
    long long cur_peak = read_cgroup_ll("memory.current");
    printf("allocated %zu MiB in %ld ms (%zu x 1 MiB, %zu x 32 KiB, %zu x 1 KiB): rss %zu MiB, memory.current %s\n", total_mib,
           alloc_ms, s.huge_n, s.med_n, s.small_n, rss_peak / MIB, fmt_mib(cur_peak, b1, sizeof b1));
    REQUIRE(rss_peak >= rss0 + total_mib * MIB * 9 / 10, "rss did not grow with the working set (%zu MiB)", rss_peak / MIB);
    struct cg_stats st_peak = challoc_stats();
    print_stats("at the peak", &st_peak);
    release(&s);
    size_t rss_freed = rss_bytes();
    struct memstat ms_freed = read_memstat();
    printf("freed: rss %zu MiB, memory.current %s\n", rss_freed / MIB, fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));
    long event_at;
    size_t rss_min;
    size_t threshold = total_mib * MIB / 4;
    size_t rss_2s = idle(WINDOW_MS, threshold, st_peak.lazy, st_peak.eager, lazy, &event_at, &rss_min);
    long long cur_2s = read_cgroup_ll("memory.current");
    struct memstat ms_2s = read_memstat();
    struct cg_stats st_2s = challoc_stats();
    printf("at %d ms: rss %zu MiB (min %zu MiB), memory.current %s, first %s purge after %ld ms\n", WINDOW_MS, rss_2s / MIB, rss_min / MIB,
           fmt_mib(cur_2s, b1, sizeof b1), lazy ? "lazy" : "eager", event_at);
    print_stats("at 2 s", &st_2s);
    print_memstat("at 2 s", &ms_2s);
    long long lazyfree_pages = ms_2s.pglazyfree >= 0 && ms_freed.pglazyfree >= 0 ? ms_2s.pglazyfree - ms_freed.pglazyfree : -1;
    long long churn_pages = (long long)(total_mib * MIB) / page_size;
    printf("  pglazyfree rose by %lld pages (%lld MiB) over the %lld pages of the churn\n", lazyfree_pages,
           lazyfree_pages < 0 ? -1 : lazyfree_pages * page_size / (long long)MIB, churn_pages);

    /* 2. The same shape again, timed: the refault saving. */
    long reuse_ms = grab(&s, total_mib, 0x6a);
    size_t rss_reuse = rss_bytes();
    printf("reallocated %zu MiB in %ld ms (first time %ld ms): rss %zu MiB, memory.current %s\n", total_mib, reuse_ms, alloc_ms,
           rss_reuse / MIB, fmt_mib(read_cgroup_ll("memory.current"), b1, sizeof b1));
    /* The contents the allocator handed back: malloc makes no promise, but
     * a reuse of MADV_FREE pages the kernel kept shows the old fill, and a
     * calloc of the same class must be zero either way (the sticky flag). */
    void *z = calloc(1, MIB);
    REQUIRE(z != NULL, "calloc(1 MiB)");
    const unsigned char *zb = z;
    size_t nonzero = 0;
    for (size_t i = 0; i < MIB; i += 64)
        nonzero += zb[i] != 0;
    CHECK(nonzero == 0, "calloc(1 MiB) after the purge has %zu non-zero sampled bytes", nonzero);
    free(z);
    release(&s);
    long event2_at;
    size_t rss_min2;
    struct cg_stats st_before2 = challoc_stats();
    idle(500, threshold, st_before2.lazy, st_before2.eager, lazy, &event2_at, &rss_min2);
    struct cg_stats st_after2 = challoc_stats();
    print_stats("after the second free", &st_after2);
    struct memstat ms_before_pressure = read_memstat();
    print_memstat("before pressure", &ms_before_pressure);

    /* 3. Pressure from a class the program has not used: the kernel must
     * reclaim the lazy-free pages (or the container's page cache) for the
     * new set to fit under the limit. */
    long long lazy_before = ms_before_pressure.pglazyfreed;
    int reclaimed_on_request = 0;
    if (lazy)
        reclaimed_on_request = try_memory_reclaim(total_mib * MIB);
    size_t pn = (size_t)PRESSURE_MIB * MIB / PRESSURE_OBJ;
    void **pp = calloc(pn, sizeof *pp);
    REQUIRE(pp != NULL, "pressure table");
    long t0 = now_ms();
    for (size_t i = 0; i < pn; i++) {
        pp[i] = malloc(PRESSURE_OBJ);
        REQUIRE(pp[i] != NULL, "malloc(%d) #%zu", PRESSURE_OBJ, i);
        memset(pp[i], 0x7a, PRESSURE_OBJ);
    }
    long pressure_ms = now_ms() - t0;
    long long cur_pressure = read_cgroup_ll("memory.current"), peak = read_cgroup_ll("memory.peak");
    struct memstat ms_pressure = read_memstat();
    size_t rss_pressure = rss_bytes();
    printf("pressure: %d MiB of %d B objects in %ld ms: rss %zu MiB, memory.current %s, memory.peak %s\n", PRESSURE_MIB, PRESSURE_OBJ,
           pressure_ms, rss_pressure / MIB, fmt_mib(cur_pressure, b1, sizeof b1), fmt_mib(peak, b2, sizeof b2));
    print_memstat("under pressure", &ms_pressure);
    long long reclaimed_pages = ms_pressure.pglazyfreed >= 0 && lazy_before >= 0 ? ms_pressure.pglazyfreed - lazy_before : -1;
    printf("  pglazyfreed rose by %lld pages (%lld MiB): lazy-free pages the kernel reclaimed\n", reclaimed_pages,
           reclaimed_pages < 0 ? -1 : reclaimed_pages * page_size / (long long)MIB);
    for (size_t i = 0; i < pn; i++)
        free(pp[i]);
    free(pp);
    struct cg_stats st_end = challoc_stats();
    print_stats("at the end", &st_end);

    printf("RESULT lazy mode=%s mib=%zu rss_peak=%zu rss_2s=%zu rss_min=%zu current_peak=%lld current_2s=%lld event_ms=%ld "
           "lazy_purges=%zu eager_purges=%zu advice_2s=%s pglazyfree_pages=%lld alloc_ms=%ld reuse_ms=%ld pressure_ms=%ld "
           "pglazyfreed_pages=%lld current_pressure=%lld reclaim_on_request=%d\n",
           mode, total_mib, rss_peak / MIB, rss_2s / MIB, rss_min / MIB, cur_peak < 0 ? -1 : cur_peak / (long long)MIB,
           cur_2s < 0 ? -1 : cur_2s / (long long)MIB, event_at, st_2s.lazy - st0.lazy, st_2s.eager - st0.eager,
           st_2s.advice ? "lazy" : "eager", lazyfree_pages, alloc_ms, reuse_ms, pressure_ms, reclaimed_pages,
           cur_pressure < 0 ? -1 : cur_pressure / (long long)MIB, reclaimed_on_request);

    if (lazy) {
        CHECK(st_2s.lazy > st_peak.lazy, "no lazy purge within %d ms of the frees (lazy_purges %zu -> %zu)", WINDOW_MS, st_peak.lazy, st_2s.lazy);
        CHECK(st_2s.eager == st_peak.eager, "eager purges ran under the headroom band (%zu -> %zu)", st_peak.eager, st_2s.eager);
        CHECK(st_2s.advice == 1, "purge_advice reads eager at 2 s under %lld%% usage", cur_2s > 0 && limit > 0 ? cur_2s * 100 / limit : -1);
        CHECK(st_2s.empty <= st_2s.cap + 4 * MIB, "empty_bytes %zu MiB still over the cap %zu MiB at 2 s", st_2s.empty / MIB, st_2s.cap / MIB);
        /* The pages stay: RSS and the charge do not fall with a lazy purge. */
        CHECK(rss_min >= rss_peak / 2, "rss fell to %zu MiB (peak %zu): the purge was not lazy", rss_min / MIB, rss_peak / MIB);
        if (lazyfree_pages >= 0)
            CHECK(lazyfree_pages >= churn_pages / 2, "pglazyfree rose by %lld pages, expected at least half of %lld", lazyfree_pages, churn_pages);
        else
            printf("  (memory.stat has no pglazyfree on this kernel; the kernel-side count is not checked)\n");
        /* Reclaimable: the pressure set fit (we are alive) and the kernel
         * took lazy-free pages back, unless memory.reclaim already did. */
        if (limit > 0)
            CHECK(cur_pressure <= limit, "memory.current %lld over the limit after the pressure set", cur_pressure);
        if (reclaimed_pages >= 0)
            CHECK(reclaimed_pages > 0 || reclaimed_on_request, "pglazyfreed did not rise under pressure: nothing reclaimed");
    } else if (eager) {
        CHECK(st_2s.eager > st_peak.eager, "no eager purge within %d ms of the frees", WINDOW_MS);
        CHECK(st_2s.lazy == st_peak.lazy, "lazy purges ran with lazy_band:false (%zu -> %zu)", st_peak.lazy, st_2s.lazy);
        CHECK(st_2s.advice == 0, "purge_advice reads lazy with lazy_band:false");
        CHECK(rss_min < threshold, "rss never fell below %zu MiB within %d ms (min %zu MiB)", threshold / MIB, WINDOW_MS, rss_min / MIB);
        if (lazyfree_pages >= 0)
            CHECK(lazyfree_pages == 0, "pglazyfree rose by %lld pages under an eager purge", lazyfree_pages);
    }
    return finish("cgroup_lazy");
}
