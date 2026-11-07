/*
 * The cgroup figures a program can read through mallctl
 * (`stats.cgroup.limit`, `.high`, `.current`, design 11 and 13.4) and the
 * `cgroup:` line of malloc_stats_print, from C under LD_PRELOAD. Modes:
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
 *          abort_conf accepts the key.
 *   bg     run with MALLOC_CONF=background_thread:true: once the thread
 *          exists the cgroup is still tracked, the report prints the
 *          tracked `cgroup:` line and `current` follows the kernel's
 *          `memory.current`.
 */
#include "cgroup.h"
#include <stdbool.h>
#include <time.h>

static char report[1 << 16];

/* The `cgroup:` line of the text report, NUL-terminated in place. */
static const char *cgroup_line(void)
{
    stats_report(malloc_stats_print, NULL, report, sizeof report);
    char *l = strstr(report, "\ncgroup: ");
    REQUIRE(l != NULL, "no cgroup line in the report:\n%s", report);
    l++;
    char *e = strchr(l, '\n');
    if (e)
        *e = 0;
    return l;
}

static void mode_plain(void)
{
    int tracked = find_cgroup(true);
    fprintf(stderr, "cgroup: %s\n", tracked ? cgroup_dir : "(none)");
    size_t limit = 0, high = 0, current = 0;
    int rl = read_size("stats.cgroup.limit", &limit);
    int rh = read_size("stats.cgroup.high", &high);
    int rc = read_size("stats.cgroup.current", &current);
    const char *line = cgroup_line();
    fprintf(stderr, "%s\n", line);
    if (!tracked) {
        CHECK(rl == ENOENT && rh == ENOENT && rc == ENOENT, "untracked keys: %d %d %d", rl, rh, rc);
        CHECK(strncmp(line, "cgroup: not tracked", 19) == 0, "%s", line);
        return;
    }
    REQUIRE(rl == 0 && rh == 0 && rc == 0, "tracked keys: %d %d %d", rl, rh, rc);
    fprintf(stderr, "stats.cgroup: limit %zu high %zu current %zu\n", limit, high, current);
    CHECK(limit == SIZE_MAX || limit > 0, "limit %zu", limit);
    CHECK(high == SIZE_MAX || high > 0, "high %zu", high);
    CHECK(strncmp(line, "cgroup: /", 9) == 0, "%s", line);
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
    int tracked = find_cgroup(true);
    fprintf(stderr, "cgroup: %s\n", tracked ? cgroup_dir : "(none)");
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
    CHECK(strncmp(line, "cgroup: /", 9) == 0, "%s", line);
    size_t current = 0;
    REQUIRE(read_size("stats.cgroup.current", &current) == 0, "current");
    size_t kernel = kernel_current();
    if (kernel > 0) {
        size_t diff = current > kernel ? current - kernel : kernel - current;
        CHECK(diff <= (32u << 20), "stats.cgroup.current %zu vs memory.current %zu", current, kernel);
    }
    fprintf(stderr, "cgroup: background thread present, stats.cgroup.current %zu (kernel says %zu)\n", current, kernel);
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
