/*
 * Phase 2 task 4: the cgroup figures a program can read through mallctl
 * (`stats.cgroup.limit`, `.high`, `.current`, `.pressure_events`, design
 * 11 and 13.4) and the `cgroup:` line of malloc_stats_print, from C under
 * LD_PRELOAD. Modes:
 *
 *   plain  the default configuration: the keys read when the process has a
 *          cgroup2 (found here the same way the library finds it, from
 *          /proc/self/cgroup and /proc/self/mountinfo) and are ENOENT when
 *          it has none; the limit is SIZE_MAX ("max") or a number, and
 *          equals CHALLOC_EXPECT_CGROUP_LIMIT when the driver set it (the
 *          `docker run --memory=512m` invocation of `make test`);
 *          `current` grows by at least half of a 64 MiB allocation this
 *          program touches, after the 10 ms sampling window has passed.
 *   off    run with MALLOC_CONF=cgroup:false,abort_conf:true: nothing is
 *          tracked, the keys are ENOENT, the report says why, and
 *          abort_conf accepts the key (it is no longer inert).
 *   bg     run with MALLOC_CONF=background_thread:true: once the thread
 *          exists the report says whether the PSI trigger was armed, and
 *          `pressure_events` reads 0 in a process under no pressure.
 */
#include "common.h"
#include <stdbool.h>
#include <time.h>

#define WEAK __attribute__((weak))
extern int mallctl(const char *, void *, size_t *, void *, size_t) WEAK;
extern void malloc_stats_print(void (*)(void *, const char *), void *, const char *) WEAK;

static char report[1 << 16];
static size_t report_len;

static void append(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

/* The `cgroup:` line of the text report, NUL-terminated in place. */
static const char *cgroup_line(void)
{
    report_len = 0;
    malloc_stats_print(append, NULL, NULL);
    report[report_len] = 0;
    char *l = strstr(report, "\ncgroup: ");
    REQUIRE(l != NULL, "no cgroup line in the report:\n%s", report);
    l++;
    char *e = strchr(l, '\n');
    if (e)
        *e = 0;
    return l;
}

static int read_size(const char *key, size_t *out)
{
    size_t len = sizeof *out;
    return mallctl(key, out, &len, NULL, 0);
}

/* The cgroup directory (mount point plus the `0::` path), found the way
 * the library finds it; empty when there is no unified path or no cgroup2
 * mount. */
static char cgroup_dir[8192];

static int has_cgroup2(void)
{
    char line[4096], path[4096] = "", mount[4096] = "";
    FILE *f = fopen("/proc/self/cgroup", "r");
    REQUIRE(f != NULL, "open /proc/self/cgroup");
    while (fgets(line, sizeof line, f))
        if (strncmp(line, "0::/", 4) == 0) {
            snprintf(path, sizeof path, "%s", line + 3);
            path[strcspn(path, "\n")] = 0;
        }
    fclose(f);
    f = fopen("/proc/self/mountinfo", "r");
    REQUIRE(f != NULL, "open /proc/self/mountinfo");
    while (fgets(line, sizeof line, f)) {
        char *sep = strstr(line, " - ");
        if (sep && strncmp(sep + 3, "cgroup2 ", 8) == 0 && !mount[0]) {
            /* field 5 (0-based 4) of the part before the separator */
            *sep = 0;
            char *save = NULL, *tok = strtok_r(line, " ", &save);
            for (int i = 0; tok && i < 4; i++)
                tok = strtok_r(NULL, " ", &save);
            if (tok)
                snprintf(mount, sizeof mount, "%s", tok);
        }
    }
    fclose(f);
    fprintf(stderr, "cgroup: unified path %s, cgroup2 mount %s\n", path[0] ? path : "(none)", mount[0] ? mount : "(none)");
    if (!path[0] || !mount[0])
        return 0;
    snprintf(cgroup_dir, sizeof cgroup_dir, "%s%s", mount, strcmp(path, "/") == 0 ? "" : path);
    return 1;
}

/* The kernel's own memory.current for this cgroup, or 0 without one. */
static size_t kernel_current(void)
{
    char p[8192 + 32];
    snprintf(p, sizeof p, "%s/memory.current", cgroup_dir);
    FILE *f = fopen(p, "r");
    if (!f)
        return 0;
    unsigned long long v = 0;
    if (fscanf(f, "%llu", &v) != 1)
        v = 0;
    fclose(f);
    return (size_t)v;
}

static void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

static void mode_plain(void)
{
    int tracked = has_cgroup2();
    size_t limit = 0, high = 0, current = 0, events = 0;
    int rl = read_size("stats.cgroup.limit", &limit);
    int rh = read_size("stats.cgroup.high", &high);
    int rc = read_size("stats.cgroup.current", &current);
    int re = read_size("stats.cgroup.pressure_events", &events);
    const char *line = cgroup_line();
    fprintf(stderr, "%s\n", line);
    if (!tracked) {
        CHECK(rl == ENOENT && rh == ENOENT && rc == ENOENT && re == ENOENT, "untracked keys: %d %d %d %d", rl, rh, rc, re);
        CHECK(strncmp(line, "cgroup: not tracked", 19) == 0, "%s", line);
        return;
    }
    REQUIRE(rl == 0 && rh == 0 && rc == 0 && re == 0, "tracked keys: %d %d %d %d", rl, rh, rc, re);
    fprintf(stderr, "stats.cgroup: limit %zu high %zu current %zu pressure_events %zu\n", limit, high, current, events);
    CHECK(limit == SIZE_MAX || limit > 0, "limit %zu", limit);
    CHECK(high == SIZE_MAX || high > 0, "high %zu", high);
    CHECK(events == 0, "pressure_events %zu without a background thread", events);
    CHECK(strncmp(line, "cgroup: /", 9) == 0, "%s", line);
    CHECK(strstr(line, "psi not armed (no background thread)") != NULL || strstr(line, "psi no memory.pressure") != NULL,
          "psi state without a thread: %s", line);
    const char *want = getenv("CHALLOC_EXPECT_CGROUP_LIMIT");
    if (want) {
        size_t w = strtoull(want, NULL, 10);
        CHECK(limit == w, "effective limit %zu, docker gave %zu", limit, w);
        fprintf(stderr, "cgroup: effective limit %zu matches CHALLOC_EXPECT_CGROUP_LIMIT\n", limit);
    }
    /* `current` is this cgroup's charge, which includes this process and,
     * under `cargo test`, the other test processes of the container: it
     * moves on its own, so the check is against the kernel's figure read
     * microseconds before the sampled read (which refreshes the sample once
     * the 10 ms window has passed), not against growth. The growth from a
     * touched 64 MiB is printed for the record. */
    if (kernel_current() > 0) {
        CHECK(current > 0, "current %zu", current);
        size_t n = 64u << 20;
        unsigned char *p = malloc(n);
        REQUIRE(p != NULL, "malloc 64 MiB");
        memset(p, 1, n);
        sink = p;
        sleep_ms(30);
        size_t kernel = kernel_current();
        size_t after = 0;
        REQUIRE(read_size("stats.cgroup.current", &after) == 0, "current after");
        fprintf(stderr, "stats.cgroup.current %zu -> %zu after touching 64 MiB (kernel says %zu)\n", current, after, kernel);
        size_t diff = after > kernel ? after - kernel : kernel - after;
        CHECK(diff <= (32u << 20), "stats.cgroup.current %zu vs memory.current %zu", after, kernel);
        free(p);
    }
}

static void mode_off(void)
{
    size_t v = 0;
    CHECK(read_size("stats.cgroup.limit", &v) == ENOENT, "limit with cgroup:false");
    CHECK(read_size("stats.cgroup.current", &v) == ENOENT, "current with cgroup:false");
    const char *line = cgroup_line();
    fprintf(stderr, "%s\n", line);
    CHECK(strcmp(line, "cgroup: not tracked (cgroup:false)") == 0, "%s", line);
    bool on = true;
    size_t len = sizeof on;
    REQUIRE(mallctl("opt.cgroup", &on, &len, NULL, 0) == 0, "opt.cgroup");
    CHECK(!on, "opt.cgroup reads true");
    CHECK(strstr(report, "opt.cgroup: false\n") != NULL, "report lists opt.cgroup: false");
    CHECK(strstr(report, "opt.cgroup: false (accepted") == NULL, "cgroup is not inert any more");
}

static void mode_bg(void)
{
    int tracked = has_cgroup2();
    /* The thread is created on a periodic tick: churn until it exists. */
    bool present = false;
    for (int i = 0; i < 2000 && !present; i++) {
        for (int j = 0; j < 256; j++) {
            void *p = malloc((size_t)(j % 7 + 1) * 700);
            sink = p;
            free(p);
        }
        size_t len = sizeof present;
        REQUIRE(mallctl("background_thread", &present, &len, NULL, 0) == 0, "background_thread");
        if (!present)
            sleep_ms(1);
    }
    REQUIRE(present, "background thread never appeared");
    sleep_ms(50);
    const char *line = cgroup_line();
    fprintf(stderr, "%s\n", line);
    if (!tracked) {
        CHECK(strncmp(line, "cgroup: not tracked", 19) == 0, "%s", line);
        return;
    }
    int armed = strstr(line, "psi armed (some ") != NULL;
    int unavailable = strstr(line, "psi unavailable (") != NULL || strstr(line, "psi no memory.pressure") != NULL;
    CHECK(armed || unavailable, "psi state with a thread: %s", line);
    fprintf(stderr, "cgroup: PSI trigger %s\n", armed ? "armed" : "unavailable");
    size_t events = 1;
    REQUIRE(read_size("stats.cgroup.pressure_events", &events) == 0, "pressure_events");
    CHECK(events == 0, "pressure_events %zu with no pressure", events);
    printf("psi %s\n", armed ? "armed" : "unavailable");
}

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(mallctl != NULL && malloc_stats_print != NULL, "jemalloc extras not preloaded");
    const char *mode = argc > 1 ? argv[1] : "plain";
    if (strcmp(mode, "plain") == 0)
        mode_plain();
    else if (strcmp(mode, "off") == 0)
        mode_off();
    else if (strcmp(mode, "bg") == 0)
        mode_bg();
    else
        REQUIRE(0, "unknown mode %s", mode);
    return finish("cgroup");
}
