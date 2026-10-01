/*
 * The cgroup policy of design 11 end to end under
 * LD_PRELOAD, from C, through the public keys only (`stats.cgroup.*`,
 * `opt.dirty_decay_ms`) and /proc/self/statm. Modes:
 *
 *   bands       Without a cgroup2: the keys are ENOENT. Tracked without a
 *               limit (the default Docker profile): `decay_ms` is the
 *               configured period, `retention_cap` is SIZE_MAX, nothing
 *               changes after a churn. Under a limit L (`docker run
 *               --memory=512m`): `retention_cap` is L/20; the program
 *               fills the cgroup to 80% of L with touched 8 MiB blocks and
 *               reads a `decay_ms` inside the window the table gives for
 *               the sampled usage (the configured period scaled linearly
 *               towards 1000 ms between 70% and 90%); frees, and within
 *               1.5 s of ticking `empty_bytes` is under the cap and RSS is
 *               back to within the cap (plus a block or two of slop); then
 *               fills to 94%, reads `decay_ms` 0, frees, and one tick
 *               later (under 200 ms) everything is purged.
 *   high        The `memory.events` `high` row: needs `memory.high` below
 *               `memory.max`, which `docker run` cannot set, so the program
 *               tries to make a child cgroup with the memory controller,
 *               move a child process into it, set its `memory.high` to
 *               96 MiB and run `high-child` there; every step that the
 *               mount refuses (read-only under Docker's default profile;
 *               `cgroup.subtree_control` EBUSY while the container's own
 *               cgroup has processes) is a pass with a "skip:" line.
 *   high-child  Inside that cgroup: allocate past `memory.high`, tick, and
 *               see `stats.cgroup.full_purges` become 1.
 */
#include "common.h"
#include <fcntl.h>
#include <stdbool.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>

#define MIB (1024UL * 1024UL)
enum { CHUNK = 8 * MIB, MAX_CHUNKS = 4096 };

static char report[1 << 16];
static size_t report_len;
static char cgroup_dir[8192];

static void append(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

/* One line of the text report starting with `prefix`, NUL-terminated in
 * place, or NULL. */
static const char *report_line(const char *prefix)
{
    report_len = 0;
    malloc_stats_print(append, NULL, NULL);
    report[report_len] = 0;
    char key[64];
    snprintf(key, sizeof key, "\n%s", prefix);
    char *l = strstr(report, key);
    if (!l)
        return NULL;
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

static size_t get_size(const char *key)
{
    size_t v = 0;
    REQUIRE(read_size(key, &v) == 0, "mallctl %s", key);
    return v;
}

static long base_decay_ms(void)
{
    ssize_t v = 0;
    size_t len = sizeof v;
    REQUIRE(mallctl("opt.dirty_decay_ms", &v, &len, NULL, 0) == 0, "opt.dirty_decay_ms");
    return (long)v;
}

/* The cgroup directory, found the way the library finds it. */
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

static size_t read_cgroup_u64(const char *dir, const char *file)
{
    char p[8192 + 64];
    snprintf(p, sizeof p, "%s/%s", dir, file);
    FILE *f = fopen(p, "r");
    if (!f)
        return 0;
    unsigned long long v = 0;
    if (fscanf(f, "%llu", &v) != 1)
        v = 0;
    fclose(f);
    return (size_t)v;
}

static size_t kernel_current(void)
{
    return read_cgroup_u64(cgroup_dir, "memory.current");
}

/* Drive the slow path (as tests/c/decay.c): the 48-byte class refills and
 * flushes every 512 operations and periodic work (the cgroup sample, the
 * policy, decay) runs every 64 slow paths. */
static void tick(void)
{
    enum { N = 4096, ROUNDS = 16 };
    static void *volatile blocks[N];
    for (int r = 0; r < ROUNDS; r++) {
        for (int i = 0; i < N; i++) {
            blocks[i] = malloc(48);
            REQUIRE(blocks[i] != NULL, "malloc(48)");
        }
        for (int i = 0; i < N; i++)
            free(blocks[i]);
    }
}

static void *chunks[MAX_CHUNKS];
static int nchunks;

/* Touched 8 MiB blocks until the kernel's memory.current reaches `target`
 * (the cgroup's charge includes everything else in the container, which is
 * what the policy sees too). Never more than `cap` bytes of our own. */
static size_t fill_to(size_t target, size_t cap)
{
    size_t mine = 0;
    while (kernel_current() < target && mine + CHUNK <= cap && nchunks < MAX_CHUNKS) {
        void *p = malloc(CHUNK);
        REQUIRE(p != NULL, "malloc(8 MiB) #%d", nchunks);
        memset(p, 0x5a, CHUNK);
        chunks[nchunks++] = p;
        mine += CHUNK;
    }
    return mine;
}

static void release_all(void)
{
    for (int i = 0; i < nchunks; i++)
        free(chunks[i]);
    nchunks = 0;
}

/* The table's period for usage `u` of limit `l`, in ms. */
static long expected_decay_ms(long base, size_t l, size_t u)
{
    if (base < 0)
        return -1;
    double frac = (double)u / (double)l;
    if (frac < 0.70)
        return base;
    if (frac >= 0.90)
        return 0;
    if (base <= 1000)
        return base;
    return base - (long)((double)(base - 1000) * (frac - 0.70) / 0.20);
}

static void mode_bands(void)
{
    int tracked = has_cgroup2();
    size_t v;
    if (!tracked) {
        CHECK(read_size("stats.cgroup.decay_ms", &v) == ENOENT, "decay_ms without a cgroup2");
        CHECK(read_size("stats.cgroup.retention_cap", &v) == ENOENT, "retention_cap without a cgroup2");
        CHECK(read_size("stats.cgroup.empty_bytes", &v) == ENOENT, "empty_bytes without a cgroup2");
        CHECK(read_size("stats.cgroup.full_purges", &v) == ENOENT, "full_purges without a cgroup2");
        const char *l = report_line("cgroup policy: ");
        CHECK(l && strcmp(l, "cgroup policy: off (not tracked)") == 0, "%s", l ? l : "(no policy line)");
        skip_note("cgroup_policy/bands", "no cgroup2, policy keys are ENOENT");
        return;
    }
    long base = base_decay_ms();
    size_t limit = get_size("stats.cgroup.limit");
    size_t high = get_size("stats.cgroup.high");
    size_t l = limit < high ? limit : high;
    size_t decay_ms = get_size("stats.cgroup.decay_ms");
    size_t cap = get_size("stats.cgroup.retention_cap");
    size_t full = get_size("stats.cgroup.full_purges");
    const char *line = report_line("cgroup policy: ");
    printf("%s\nbase %ld ms, limit %zu, high %zu, decay_ms %zu, retention_cap %zu\n", line ? line : "(none)", base, limit, high,
            decay_ms, cap);
    CHECK(full == 0, "full_purges %zu at start", full);
    if (l == SIZE_MAX) {
        /* No limit: the defaults, and nothing the churn does changes them. */
        CHECK(decay_ms == (base < 0 ? SIZE_MAX : (size_t)base), "decay_ms %zu with no limit, base %ld", decay_ms, base);
        CHECK(cap == SIZE_MAX, "retention_cap %zu with no limit", cap);
        CHECK(line && strstr(line, "defaults (no limit)") != NULL, "%s", line ? line : "(none)");
        fill_to(SIZE_MAX, 64 * MIB);
        release_all();
        tick();
        size_t empty = get_size("stats.cgroup.empty_bytes");
        CHECK(get_size("stats.cgroup.decay_ms") == decay_ms, "decay_ms changed without a limit");
        CHECK(get_size("stats.cgroup.retention_cap") == SIZE_MAX, "retention_cap changed without a limit");
        printf("empty_bytes %zu after freeing 64 MiB, no limit\n", empty);
        CHECK(empty >= 32 * MIB, "empty_bytes %zu after freeing 64 MiB (nothing should have been purged yet)", empty);
        skip_note("cgroup_policy/bands", "no cgroup limit, bands not exercised");
        return;
    }
    CHECK(cap == l / 20, "retention_cap %zu, limit %zu", cap, l);
    size_t base_rss = rss_bytes();
    size_t before = kernel_current();
    printf("limit %zu MiB, memory.current %zu MiB before, rss %zu MiB\n", l / MIB, before / MIB, base_rss / MIB);

    /* Step 1: 80%, the scaled band. */
    size_t mine = fill_to(l * 80 / 100, l * 88 / 100);
    sleep_ms(15);
    tick();
    size_t current = get_size("stats.cgroup.current");
    decay_ms = get_size("stats.cgroup.decay_ms");
    line = report_line("cgroup policy: ");
    printf("80%%: allocated %zu MiB, current %zu MiB (%.1f%%), decay_ms %zu\n%s\n", mine / MIB, current / MIB,
            100.0 * (double)current / (double)l, decay_ms, line ? line : "(none)");
    {
        /* The sample the policy saw and the one read here may differ by a
         * little; allow 3% of the limit either way. */
        size_t slop = l * 3 / 100;
        long a = expected_decay_ms(base, l, current > slop ? current - slop : 0);
        long b = expected_decay_ms(base, l, current + slop);
        long lo = a < b ? a : b, hi = a < b ? b : a;
        CHECK((long)decay_ms >= lo && (long)decay_ms <= hi, "decay_ms %zu outside [%ld, %ld] for usage %zu of %zu", decay_ms, lo,
              hi, current, l);
        if (current >= l * 70 / 100 && current < l * 90 / 100)
            CHECK(decay_ms < (size_t)base && decay_ms >= 1000, "decay_ms %zu not scaled in the 70..90%% band", decay_ms);
        else
            fprintf(stderr, "note: usage %.1f%% is outside the 70..90%% band\n", 100.0 * (double)current / (double)l);
    }
    release_all();
    long t0 = now_ms();
    size_t empty = SIZE_MAX;
    long elapsed = 0;
    while (elapsed < 3000) {
        tick();
        empty = get_size("stats.cgroup.empty_bytes");
        elapsed = now_ms() - t0;
        if (empty <= cap)
            break;
        sleep_ms(5);
    }
    size_t rss1 = rss_bytes();
    line = report_line("cgroup policy: ");
    printf("80%% freed: empty_bytes %zu MiB (cap %zu MiB) after %ld ms, rss %zu MiB (base %zu MiB)\n%s\n", empty / MIB, cap / MIB,
            elapsed, rss1 / MIB, base_rss / MIB, line ? line : "(none)");
    CHECK(empty <= cap, "empty_bytes %zu still over the cap %zu after %ld ms", empty, cap, elapsed);
    CHECK(elapsed < 1500, "the retention cap took %ld ms", elapsed);
    /* What is kept is under the cap, plus up to one whole extent the
     * huge walk never splits and the tick's own slabs. */
    CHECK(rss1 <= base_rss + cap + 2 * CHUNK + 8 * MIB, "rss %zu MiB after freeing, base %zu MiB, cap %zu MiB", rss1 / MIB,
          base_rss / MIB, cap / MIB);
    sleep_ms(15);
    tick();
    current = get_size("stats.cgroup.current");
    decay_ms = get_size("stats.cgroup.decay_ms");
    printf("after the purge: current %zu MiB (%.1f%%), decay_ms %zu\n", current / MIB, 100.0 * (double)current / (double)l,
            decay_ms);
    if (current < l * 70 / 100)
        CHECK(decay_ms == (size_t)base, "decay_ms %zu under 70%% (base %ld)", decay_ms, base);

    /* Step 2: 94%, the immediate band. */
    mine = fill_to(l * 94 / 100, l * 96 / 100);
    sleep_ms(15);
    tick();
    current = get_size("stats.cgroup.current");
    decay_ms = get_size("stats.cgroup.decay_ms");
    line = report_line("cgroup policy: ");
    printf("94%%: allocated %zu MiB, current %zu MiB (%.1f%%), decay_ms %zu\n%s\n", mine / MIB, current / MIB,
            100.0 * (double)current / (double)l, decay_ms, line ? line : "(none)");
    if (current >= l * 90 / 100) {
        CHECK(decay_ms == 0, "decay_ms %zu at %.1f%%", decay_ms, 100.0 * (double)current / (double)l);
        CHECK(line && strstr(line, "over 90%") != NULL, "%s", line ? line : "(none)");
    } else {
        fprintf(stderr, "note: could not reach 90%% (allocated %zu MiB)\n", mine / MIB);
    }
    t0 = now_ms();
    release_all();
    tick();
    long dt = now_ms() - t0;
    empty = get_size("stats.cgroup.empty_bytes");
    size_t rss2 = rss_bytes();
    printf("94%% freed: one tick later (%ld ms) empty_bytes %zu, rss %zu MiB (base %zu MiB)\n", dt, empty, rss2 / MIB,
            base_rss / MIB);
    if (current >= l * 90 / 100) {
        CHECK(dt < 200, "the immediate purge took %ld ms", dt);
        /* The tick's own last frees leave one small slab `Empty` after
         * its last periodic pass; no 8 MiB block may remain. */
        CHECK(empty < CHUNK, "empty_bytes %zu after an immediate purge", empty);
        CHECK(rss2 <= base_rss + 2 * CHUNK + 8 * MIB, "rss %zu MiB after the immediate purge, base %zu MiB", rss2 / MIB, base_rss / MIB);
    }
    CHECK(get_size("stats.cgroup.full_purges") == 0, "a full purge ran without a memory.high event");
    printf("bands: limit %zu MiB, scaled decay_ms observed, immediate purge in %ld ms\n", l / MIB, dt);
}

static int write_str(const char *path, const char *s)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0)
        return -1;
    ssize_t n = write(fd, s, strlen(s));
    int saved = errno;
    close(fd);
    errno = saved;
    return n == (ssize_t)strlen(s) ? 0 : -1;
}

static void mode_high(const char *self)
{
    if (!has_cgroup2()) {
        skip_note("cgroup_policy/high", "no cgroup2");
        return;
    }
    char child[8192 + 64], p[8192 + 128];
    snprintf(child, sizeof child, "%s/challoc-policy-%d", cgroup_dir, (int)getpid());
    if (mkdir(child, 0755) != 0) {
        skip_note("cgroup_policy/high", "cannot create a child cgroup under %s (%s)", cgroup_dir, strerror(errno));
        return;
    }
    snprintf(p, sizeof p, "%s/memory.high", child);
    if (access(p, W_OK) != 0) {
        /* The memory controller must be enabled for children, which the
         * no-internal-process rule refuses while this cgroup has processes
         * (EBUSY: every container). Fall back to a sibling under the
         * parent, visible only without a cgroup namespace
         * (`--cgroupns=host`), where the parent already delegates memory
         * to the container's cgroup. */
        char sc[8192 + 64];
        snprintf(sc, sizeof sc, "%s/cgroup.subtree_control", cgroup_dir);
        if (write_str(sc, "+memory") != 0 || access(p, W_OK) != 0) {
            int saved = errno;
            rmdir(child);
            char *slash = strrchr(cgroup_dir, '/');
            if (!slash || slash == cgroup_dir || strstr(cgroup_dir, "/sys/fs/cgroup/") == NULL || strlen(slash) < 2) {
                skip_note("cgroup_policy/high", "no memory controller for a child cgroup (+memory to %s: %s) and no visible parent", sc,
                          strerror(saved));
                return;
            }
            snprintf(child, sizeof child, "%.*s/challoc-policy-%d", (int)(slash - cgroup_dir), cgroup_dir, (int)getpid());
            if (mkdir(child, 0755) != 0) {
                skip_note("cgroup_policy/high", "no memory controller for a child cgroup (%s) and cannot create a sibling %s (%s)",
                          strerror(saved), child, strerror(errno));
                return;
            }
            snprintf(p, sizeof p, "%s/memory.high", child);
            if (access(p, W_OK) != 0) {
                skip_note("cgroup_policy/high", "sibling cgroup %s has no memory.high (%s)", child, strerror(errno));
                rmdir(child);
                return;
            }
            printf("note: using a sibling cgroup %s (the container's own cgroup refused +memory: %s)\n", child, strerror(saved));
        }
    }
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        char buf[32];
        snprintf(buf, sizeof buf, "%d", (int)getpid());
        snprintf(p, sizeof p, "%s/cgroup.procs", child);
        if (write_str(p, buf) != 0) {
            fprintf(stderr, "skip: cannot move into %s (%s)\n", child, strerror(errno));
            _exit(75);
        }
        snprintf(p, sizeof p, "%s/memory.high", child);
        if (write_str(p, "100663296") != 0) {
            fprintf(stderr, "skip: cannot set memory.high in %s (%s)\n", child, strerror(errno));
            _exit(75);
        }
        execl(self, self, "high-child", (char *)NULL);
        _exit(127);
    }
    int status = 0;
    REQUIRE(waitpid(pid, &status, 0) == pid, "waitpid");
    for (int i = 0; i < 50 && rmdir(child) != 0; i++)
        sleep_ms(20);
    if (WIFEXITED(status) && WEXITSTATUS(status) == 75) {
        skip_note("cgroup_policy/high", "the child cgroup could not be used (see stderr)");
        return;
    }
    REQUIRE(WIFEXITED(status) && WEXITSTATUS(status) == 0, "high-child failed (status %d)", status);
    printf("high: memory.events high rise -> full purge, in a child cgroup\n");
}

static void mode_high_child(void)
{
    REQUIRE(has_cgroup2(), "no cgroup2 in the child");
    size_t high = get_size("stats.cgroup.high");
    size_t limit = get_size("stats.cgroup.limit");
    printf("child in %s: high %zu, limit %zu\n", cgroup_dir, high, limit);
    CHECK(high == 96 * MIB, "stats.cgroup.high %zu, expected 96 MiB", high);
    CHECK(get_size("stats.cgroup.full_purges") == 0, "full_purges at start");
    size_t events0 = read_cgroup_u64(cgroup_dir, "memory.events");
    /* Something to purge, then past `memory.high`: the kernel counts a
     * `high` event and throttles this process a little. */
    fill_to(SIZE_MAX, 32 * MIB);
    release_all();
    fill_to(SIZE_MAX, 112 * MIB);
    size_t full = 0;
    long t0 = now_ms();
    while (now_ms() - t0 < 5000) {
        sleep_ms(15);
        tick();
        full = get_size("stats.cgroup.full_purges");
        if (full > 0)
            break;
    }
    const char *line = report_line("cgroup: ");
    printf("%s\n%s\nfull_purges %zu after %ld ms\n", line ? line : "(none)", report_line("cgroup policy: "), full, now_ms() - t0);
    (void)events0;
    CHECK(full >= 1, "no full purge after exceeding memory.high (full_purges %zu)", full);
    release_all();
}

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(mallctl != NULL && malloc_stats_print != NULL, "jemalloc extras not preloaded");
    const char *mode = argc > 1 ? argv[1] : "bands";
    if (strcmp(mode, "bands") == 0)
        mode_bands();
    else if (strcmp(mode, "high") == 0)
        mode_high(argv[0]);
    else if (strcmp(mode, "high-child") == 0) {
        mode_high_child();
        return finish("cgroup_policy child");
    } else
        REQUIRE(0, "unknown mode %s", mode);
    return finish("cgroup_policy");
}
