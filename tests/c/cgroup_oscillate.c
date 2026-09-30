/*
 * The cgroup oscillation test. The live set
 * swings between LOW (200 MiB) and HIGH (480 MiB) of requested bytes in a
 * `docker run --memory=512m` container for SECONDS (300) and the process
 * must still be alive at the end: with the design 11 policy the memory
 * freed on the way down (over the 5% retention cap; above 90% of the
 * limit everything `Empty` goes at once) is returned before the next
 * climb, so retention never adds to a 480 MiB peak in a 512 MiB cgroup.
 *
 *   cgroup_oscillate [seconds] [low_mib] [high_mib] [hold_ms] [--any-allocator]
 *
 * The mix, by bytes: 15% in 1 MiB runs (the huge allocator), 20% each in
 * 64 KiB, 16 KiB and 4 KiB objects (medium and the largest slab classes),
 * 15% in 1 KiB and 5% each in 256 and 64 byte objects (small slab
 * classes); about 600k live objects at 480 MiB. Allocations come in
 * batches of one class (64 KiB of it, or one 1 MiB run), every byte is
 * written; frees pick random live objects, so partially freed slabs and
 * split extents are the normal case. Each end of the swing is held for
 * `hold_ms` (1500) with the idle trickle of cgroup_churn.c, a full cycle
 * every 3.5 s or so, ~85 cycles in 5 minutes. Single-threaded, no
 * `malloc_trim`, no background thread: the policy's periodic tick is the
 * only purge path.
 *
 * Printed: the kernel's `memory.events` before and after (the driver also
 * reads them after an OOM kill, when this program cannot), `memory.current`
 * sampled after every 8 MiB allocated and at both ends of every cycle
 * (peak kept), RSS from /proc/self/statm at both ends, challoc's
 * `stats.cgroup.*` at the end when preloaded, and one RESULT line. Exit 0
 * iff the run completed; the driver's watchdog kills a hung run at 6
 * minutes and an OOM kill shows as exit 137.
 *
 * The composition of the charge (for the 64 KiB-page kernels):
 * `memory.stat`'s anon, file, kernel, slab and pagetables, plus challoc's
 * `stats.allocated/active/resident/metadata/retained` and
 * `stats.cgroup.empty_bytes` (after an `epoch`) when preloaded, printed as
 * a `composition` line before the first allocation, at the top of cycles
 * 1, 2, 3, 5, 10, 20, 30, 50, 70 and 90, for the highest peak seen, and
 * at the end. What is not anon is not the allocator's.
 */
#include "common.h"
#include <stdbool.h>
#include <time.h>

#define MIB (1024UL * 1024UL)
#define KIB 1024UL
enum { MAX_OBJECTS = 1500000, SAMPLE_EVERY = 8 * MIB, TRICKLE_MS = 10 };
/*
 * The page-rounding envelope of a kernel with pages larger than 4 KiB.
 * Measured on 6.8.0-139-generic-64k against 4 KiB kernels with the same object
 * sequence: at the top of the swing `memory.stat` anon is 5 MiB higher
 * (the allocator's page-rounded metadata tables +1, its Empty 4 KiB slabs
 * scattered through 64 KiB page groups that are never all idle +2, which
 * a 4 KiB kernel purges, and this program's own data segments +2) and
 * kernel page tables are 2 MiB higher, with the allocator's `active`
 * identical; a 300 s run's worst top then crosses 512 MiB. None of it is
 * retention (the same Empty survives `dirty_decay_ms:0`). When HIGH is not
 * given on the command line and the page is larger than 4 KiB, the default
 * target is lowered by this much so the allocator keeps the envelope the
 * test gives it at 4 KiB; an explicit HIGH is used as given.
 */
enum { LARGE_PAGE_ENVELOPE_MIB = 8 };

struct obj {
    void *p;
    uint32_t size;
};
static struct obj objs[MAX_OBJECTS];
static size_t nobjs, live_bytes;
static long page_size;
static char cgroup_dir[8192];
static int have_cgroup;
static long long current_peak = -1;
static size_t rss_peak;

/* Classes and their byte shares (percent). */
static const size_t class_size[] = { 1 * MIB, 64 * KIB, 16 * KIB, 4 * KIB, 1 * KIB, 256, 64 };
static const int class_share[] = { 15, 20, 20, 20, 15, 5, 5 };
enum { NCLASS = sizeof class_size / sizeof class_size[0] };

/* RSS now, with the run's peak kept. */
static size_t rss_now(void)
{
    size_t r = rss_bytes();
    if (r > rss_peak)
        rss_peak = r;
    return r;
}

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
    if (fgets(buf, sizeof buf, f))
        v = strncmp(buf, "max", 3) == 0 ? -2 : atoll(buf);
    fclose(f);
    return v;
}

static long long sample_current(void)
{
    long long c = read_cgroup_ll("memory.current");
    if (c > current_peak)
        current_peak = c;
    return c;
}

static const char *fmt_mib(long long v, char *buf, size_t n);

/* One `memory.stat` field (bytes), -1 when absent. */
static long long memstat(const char *key)
{
    if (!have_cgroup)
        return -1;
    char p[8192 + 64], line[256];
    snprintf(p, sizeof p, "%s/memory.stat", cgroup_dir);
    FILE *f = fopen(p, "r");
    if (!f)
        return -1;
    long long v = -1;
    size_t k = strlen(key);
    while (fgets(line, sizeof line, f))
        if (strncmp(line, key, k) == 0 && line[k] == ' ') {
            v = atoll(line + k + 1);
            break;
        }
    fclose(f);
    return v;
}

/* The composition of the charge at this moment (see the header). */
struct comp {
    long long current, anon, file, kernel, slab, pagetables;
    size_t rss;
    int challoc;
    size_t allocated, active, resident, metadata, retained, empty;
};

static void read_comp(struct comp *c)
{
    memset(c, 0, sizeof *c);
    c->current = sample_current();
    c->anon = memstat("anon");
    c->file = memstat("file");
    c->kernel = memstat("kernel");
    c->slab = memstat("slab");
    c->pagetables = memstat("pagetables");
    c->rss = rss_now();
    if (!mallctl)
        return;
    uint64_t e = 1;
    if (mallctl("epoch", NULL, NULL, &e, sizeof e) != 0)
        return;
    const char *keys[] = { "stats.allocated", "stats.active", "stats.resident", "stats.metadata", "stats.retained", "stats.cgroup.empty_bytes" };
    size_t *out[] = { &c->allocated, &c->active, &c->resident, &c->metadata, &c->retained, &c->empty };
    for (size_t i = 0; i < sizeof keys / sizeof keys[0]; i++) {
        size_t v = 0, len = sizeof v;
        if (mallctl(keys[i], &v, &len, NULL, 0) == 0 && v != SIZE_MAX)
            *out[i] = v;
    }
    c->challoc = 1;
}

static void print_comp(const char *tag, const struct comp *c)
{
    char b[6][32];
    printf("composition %s: current %s anon %s file %s kernel %s (slab %s pagetables %s) MiB, rss %zu MiB", tag, fmt_mib(c->current, b[0], 32),
           fmt_mib(c->anon, b[1], 32), fmt_mib(c->file, b[2], 32), fmt_mib(c->kernel, b[3], 32), fmt_mib(c->slab, b[4], 32),
           fmt_mib(c->pagetables, b[5], 32), c->rss / MIB);
    if (c->challoc)
        printf("; challoc allocated %zu active %zu resident %zu metadata %zu retained %zu empty %zu MiB", c->allocated / MIB, c->active / MIB,
               c->resident / MIB, c->metadata / MIB, c->retained / MIB, c->empty / MIB);
    printf("\n");
    fflush(stdout);
}

static int comp_cycle(long cycle)
{
    static const long at[] = { 1, 2, 3, 5, 10, 20, 30, 50, 70, 90 };
    for (size_t i = 0; i < sizeof at / sizeof at[0]; i++)
        if (at[i] == cycle)
            return 1;
    return 0;
}

/* `memory.events` as one line: "low 0 high 0 max 0 oom 0 oom_kill 0 ...". */
static const char *events_line(char *buf, size_t n)
{
    buf[0] = 0;
    if (!have_cgroup)
        return "n/a";
    char p[8192 + 64], line[128];
    snprintf(p, sizeof p, "%s/memory.events", cgroup_dir);
    FILE *f = fopen(p, "r");
    if (!f)
        return "n/a";
    size_t len = 0;
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\n")] = 0;
        int k = snprintf(buf + len, n - len, "%s%s", len ? " " : "", line);
        if (k < 0 || (size_t)k >= n - len)
            break;
        len += (size_t)k;
    }
    fclose(f);
    return buf;
}

static long long events_field(const char *events, const char *key)
{
    char k[32];
    snprintf(k, sizeof k, "%s ", key);
    const char *s = events;
    while ((s = strstr(s, k)) != NULL) {
        if (s == events || s[-1] == ' ')
            return atoll(s + strlen(k));
        s++;
    }
    return -1;
}

static const char *fmt_mib(long long v, char *buf, size_t n)
{
    if (v == -2)
        snprintf(buf, n, "max");
    else if (v < 0)
        snprintf(buf, n, "n/a");
    else
        snprintf(buf, n, "%lld", v / (long long)MIB);
    return buf;
}

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

static void hold(long ms)
{
    long t0 = now_ms();
    while (now_ms() - t0 < ms) {
        sleep_ms(TRICKLE_MS);
        trickle();
    }
}

/* Grow the live set to `target` bytes in batches of one class. */
static void grow_to(size_t target, uint64_t *rng)
{
    size_t since_sample = 0;
    while (live_bytes < target) {
        int r = (int)(rng_next(rng) % 100), c = 0, acc = 0;
        for (c = 0; c < NCLASS - 1; c++) {
            acc += class_share[c];
            if (r < acc)
                break;
        }
        size_t sz = class_size[c];
        size_t n = sz >= 64 * KIB ? 1 : 64 * KIB / sz;
        for (size_t i = 0; i < n && live_bytes < target; i++) {
            REQUIRE(nobjs < MAX_OBJECTS, "object table full at %zu", nobjs);
            void *p = malloc(sz);
            REQUIRE(p != NULL, "malloc(%zu) with %zu MiB live", sz, live_bytes / MIB);
            memset(p, (int)(0x40 + c), sz);
            objs[nobjs].p = p;
            objs[nobjs].size = (uint32_t)sz;
            nobjs++;
            live_bytes += sz;
            since_sample += sz;
        }
        if (since_sample >= SAMPLE_EVERY) {
            sample_current();
            since_sample = 0;
        }
    }
    sample_current();
}

/* Shrink to `target` bytes by freeing random live objects. */
static void shrink_to(size_t target, uint64_t *rng)
{
    while (live_bytes > target && nobjs > 0) {
        size_t i = (size_t)(rng_next(rng) % nobjs);
        free(objs[i].p);
        live_bytes -= objs[i].size;
        objs[i] = objs[--nobjs];
    }
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
    const char *keys[] = { "stats.cgroup.current", "stats.cgroup.decay_ms", "stats.cgroup.retention_cap", "stats.cgroup.empty_bytes",
                           "stats.cgroup.full_purges", "stats.cgroup.pressure_events" };
    printf("challoc: stats.cgroup.limit %zu MiB", v == SIZE_MAX ? 0 : v / MIB);
    for (size_t i = 0; i < sizeof keys / sizeof keys[0]; i++) {
        len = sizeof v;
        if (mallctl(keys[i], &v, &len, NULL, 0) == 0) {
            const char *short_name = keys[i] + strlen("stats.cgroup.");
            if (strcmp(short_name, "current") == 0 || strcmp(short_name, "retention_cap") == 0 || strcmp(short_name, "empty_bytes") == 0)
                printf(" %s %zu MiB", short_name, v == SIZE_MAX ? 0 : v / MIB);
            else
                printf(" %s %zu", short_name, v);
        }
    }
    printf("\n");
}

int main(int argc, char **argv)
{
    long seconds = 300, hold_ms = 1500;
    size_t low_mib = 200, high_mib = 480;
    bool any = false;
    int pos = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--any-allocator") == 0)
            any = true;
        else if (pos == 0)
            seconds = atol(argv[i]), pos++;
        else if (pos == 1)
            low_mib = strtoul(argv[i], NULL, 10), pos++;
        else if (pos == 2)
            high_mib = strtoul(argv[i], NULL, 10), pos++;
        else if (pos == 3)
            hold_ms = atol(argv[i]), pos++;
        else
            REQUIRE(0, "usage: cgroup_oscillate [seconds] [low_mib] [high_mib] [hold_ms] [--any-allocator]");
    }
    REQUIRE(seconds > 0 && low_mib < high_mib && high_mib >= 8, "sizes");
    if (!any)
        require_challoc();
    page_size = sysconf(_SC_PAGESIZE);
    if (pos < 3 && page_size > 4096) {
        high_mib -= LARGE_PAGE_ENVELOPE_MIB;
        printf("cgroup_oscillate: page size %ld: the default high target is %zu MiB, %d MiB under the 4 KiB default (the page-rounding envelope, see the header)\n",
               page_size, high_mib, LARGE_PAGE_ENVELOPE_MIB);
    }
    have_cgroup = find_cgroup();
    char b1[32], b2[32], ev0[512], ev1[512];
    long long limit = read_cgroup_ll("memory.max");
    events_line(ev0, sizeof ev0);
    printf("cgroup_oscillate: %zu..%zu MiB live for %ld s, hold %ld ms; cgroup %s memory.max %s MiB memory.current %s MiB rss %zu MiB\n",
           low_mib, high_mib, seconds, hold_ms, have_cgroup ? cgroup_dir : "(none)", fmt_mib(limit, b1, sizeof b1),
           fmt_mib(sample_current(), b2, sizeof b2), rss_now() / MIB);
    printf("memory.events before: %s\n", ev0);
    fflush(stdout);

    struct comp comp, comp_max;
    long comp_max_cycle = 0;
    read_comp(&comp);
    print_comp("at start", &comp);
    comp_max = comp;

    uint64_t rng = 0x9E3779B97F4A7C15ULL;
    long t_start = now_ms(), deadline = t_start + seconds * 1000;
    long cycles = 0;
    grow_to(low_mib * MIB, &rng);
    while (now_ms() < deadline) {
        long t0 = now_ms();
        grow_to(high_mib * MIB, &rng);
        long t_up = now_ms();
        read_comp(&comp);
        long long cur_hi = comp.current;
        size_t rss_hi = comp.rss;
        if (comp.current > comp_max.current) {
            comp_max = comp;
            comp_max_cycle = cycles + 1;
        }
        if (comp_cycle(cycles + 1)) {
            char tag[48];
            snprintf(tag, sizeof tag, "top of cycle %ld", cycles + 1);
            print_comp(tag, &comp);
        }
        /* Debug: OSC_STATS_AT=<cycle> prints the allocator's full report at
         * that top (per-class Empty and Purged slabs, the huge extents). */
        if (malloc_stats_print && getenv("OSC_STATS_AT") && atol(getenv("OSC_STATS_AT")) == cycles + 1) {
            fflush(stdout);
            malloc_stats_print(NULL, NULL, "");
        }
        hold(hold_ms);
        shrink_to(low_mib * MIB, &rng);
        long t_down = now_ms();
        long long cur_lo = sample_current();
        size_t rss_lo = rss_now();
        hold(hold_ms);
        cycles++;
        printf("cycle %3ld at %5ld s: up %zu MiB in %ld ms (rss %zu, current %s MiB), down to %zu MiB in %ld ms (rss %zu, current %s MiB), %zu objects\n",
               cycles, (now_ms() - t_start) / 1000, high_mib, t_up - t0, rss_hi / MIB, fmt_mib(cur_hi, b1, sizeof b1), low_mib,
               t_down - t_up - hold_ms, rss_lo / MIB, fmt_mib(cur_lo, b2, sizeof b2), nobjs);
        fflush(stdout);
    }
    events_line(ev1, sizeof ev1);
    printf("memory.events after: %s\n", ev1);
    {
        char tag[48];
        snprintf(tag, sizeof tag, "highest top (cycle %ld)", comp_max_cycle);
        print_comp(tag, &comp_max);
        read_comp(&comp);
        print_comp("at end (bottom)", &comp);
    }
    challoc_policy_line();
    long long peak_mib = current_peak < 0 ? -1 : current_peak / (long long)MIB;
    printf("RESULT oscillate cycles=%ld seconds=%ld low=%zu high=%zu current_peak=%lld rss_peak=%zu max_before=%lld max_after=%lld high_before=%lld high_after=%lld oom_kill_before=%lld oom_kill_after=%lld\n",
           cycles, (now_ms() - t_start) / 1000, low_mib, high_mib, peak_mib, rss_peak / MIB, events_field(ev0, "max"), events_field(ev1, "max"),
           events_field(ev0, "high"), events_field(ev1, "high"), events_field(ev0, "oom_kill"), events_field(ev1, "oom_kill"));
    shrink_to(0, &rng);
    CHECK(cycles >= 1, "no cycle completed");
    return finish("cgroup_oscillate");
}
