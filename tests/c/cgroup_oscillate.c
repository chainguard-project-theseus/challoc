/*
 * Phase 2 task 6 (b): the oscillation test of plan Phase 2. The live set
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
 */
#include "common.h"
#include <stdbool.h>
#include <time.h>

#define WEAK __attribute__((weak))
extern int mallctl(const char *, void *, size_t *, void *, size_t) WEAK;

#define MIB (1024UL * 1024UL)
#define KIB 1024UL
enum { MAX_OBJECTS = 1500000, SAMPLE_EVERY = 8 * MIB, TRICKLE_MS = 10 };

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

static size_t rss_bytes(void)
{
    FILE *f = fopen("/proc/self/statm", "r");
    REQUIRE(f != NULL, "open /proc/self/statm: %s", strerror(errno));
    unsigned long size, resident;
    REQUIRE(fscanf(f, "%lu %lu", &size, &resident) == 2, "parse statm");
    fclose(f);
    size_t r = (size_t)resident * (size_t)page_size;
    if (r > rss_peak)
        rss_peak = r;
    return r;
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
    have_cgroup = find_cgroup();
    char b1[32], b2[32], ev0[512], ev1[512];
    long long limit = read_cgroup_ll("memory.max");
    events_line(ev0, sizeof ev0);
    printf("cgroup_oscillate: %zu..%zu MiB live for %ld s, hold %ld ms; cgroup %s memory.max %s MiB memory.current %s MiB rss %zu MiB\n",
           low_mib, high_mib, seconds, hold_ms, have_cgroup ? cgroup_dir : "(none)", fmt_mib(limit, b1, sizeof b1),
           fmt_mib(sample_current(), b2, sizeof b2), rss_bytes() / MIB);
    printf("memory.events before: %s\n", ev0);
    fflush(stdout);

    uint64_t rng = 0x9E3779B97F4A7C15ULL;
    long t_start = now_ms(), deadline = t_start + seconds * 1000;
    long cycles = 0;
    grow_to(low_mib * MIB, &rng);
    while (now_ms() < deadline) {
        long t0 = now_ms();
        grow_to(high_mib * MIB, &rng);
        long t_up = now_ms();
        long long cur_hi = sample_current();
        size_t rss_hi = rss_bytes();
        hold(hold_ms);
        shrink_to(low_mib * MIB, &rng);
        long t_down = now_ms();
        long long cur_lo = sample_current();
        size_t rss_lo = rss_bytes();
        hold(hold_ms);
        cycles++;
        printf("cycle %3ld at %5ld s: up %zu MiB in %ld ms (rss %zu, current %s MiB), down to %zu MiB in %ld ms (rss %zu, current %s MiB), %zu objects\n",
               cycles, (now_ms() - t_start) / 1000, high_mib, t_up - t0, rss_hi / MIB, fmt_mib(cur_hi, b1, sizeof b1), low_mib,
               t_down - t_up - hold_ms, rss_lo / MIB, fmt_mib(cur_lo, b2, sizeof b2), nobjs);
        fflush(stdout);
    }
    events_line(ev1, sizeof ev1);
    printf("memory.events after: %s\n", ev1);
    challoc_policy_line();
    long long peak_mib = current_peak < 0 ? -1 : current_peak / (long long)MIB;
    printf("RESULT oscillate cycles=%ld seconds=%ld low=%zu high=%zu current_peak=%lld rss_peak=%zu max_before=%lld max_after=%lld high_before=%lld high_after=%lld oom_kill_before=%lld oom_kill_after=%lld\n",
           cycles, (now_ms() - t_start) / 1000, low_mib, high_mib, peak_mib, rss_peak / MIB, events_field(ev0, "max"), events_field(ev1, "max"),
           events_field(ev0, "high"), events_field(ev1, "high"), events_field(ev0, "oom_kill"), events_field(ev1, "oom_kill"));
    shrink_to(0, &rng);
    CHECK(cycles >= 1, "no cycle completed");
    return finish("cgroup_oscillate");
}
