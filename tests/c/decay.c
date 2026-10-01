/*
 * Decay, purge and the background thread end to end under
 * LD_PRELOAD. The driver in src/api/c_programs.rs runs one mode per process
 * with MALLOC_CONF set for it:
 *
 *   trim        (defaults)                 free a lot, malloc_trim(0) drops
 *                                          RSS; calloc afterwards is zero
 *   decay       dirty_decay_ms:200         free a lot, keep the slow path
 *                                          ticking: RSS drops within 200 ms
 *                                          and not at once
 *   curve       dirty_decay_ms:2000        the same over a 2 s window: part
 *                                          of the freed set is back by
 *                                          mid-window (the smoothstep
 *                                          curve), all of it by
 *                                          the end
 *   never       dirty_decay_ms:-1          RSS never drops on its own;
 *                                          malloc_trim still purges
 *   background  background_thread:true,    the thread appears, RSS drops
 *               dirty_decay_ms:200         with nobody touching the
 *                                          allocator, and a fork child has
 *                                          no thread but can still purge
 *   demand      dirty_decay_ms:200,        8 MiB slab bursts every 150 ms:
 *               hpa_peak_demand_window_ms: from the third burst on the
 *               4000                       freed slabs are kept between
 *                                          bursts (almost no minor faults);
 *                                          idle, they stay past the curve's
 *                                          window and go once the demand
 *                                          window passes the last burst
 *   demand-off  dirty_decay_ms:200         the same bursts without the
 *                                          window: the curve purges most of
 *                                          each burst before the next, which
 *                                          faults it back in
 *   hugedemand  hpa_peak_demand_window_ms: a 100 MiB run (above
 *               4000                       huge_retain_max) freed and
 *                                          allocated again: purged at its
 *                                          first free, kept from the second,
 *                                          so no faults from the third
 *                                          iteration on; a one-off 192 MiB
 *                                          run is purged at free
 *   hugedemand-off (defaults)              every iteration is purged at
 *                                          free and faults again
 *
 * RSS comes from /proc/self/statm. The freed working set is ~48 MiB (32 MiB
 * of 1 KiB blocks, 16 MiB of huge runs) so a purge is unmistakable next to
 * the noise of stdio and the caches.
 */
#include "common.h"

#include <dirent.h>
#include <limits.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>

enum { SMALL_N = 32768, SMALL_SIZE = 1024, HUGE_N = 4, HUGE_SIZE = 4 << 20 };
#define MIB (1024UL * 1024UL)

/* Number of threads in this process and whether one is named challoc-bg. */
static int count_tasks(int *bg)
{
    DIR *d = opendir("/proc/self/task");
    REQUIRE(d != NULL, "opendir /proc/self/task: %s", strerror(errno));
    int n = 0;
    *bg = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.')
            continue;
        n++;
        char path[sizeof "/proc/self/task/" + NAME_MAX + sizeof "/comm"], comm[32] = "";
        snprintf(path, sizeof path, "/proc/self/task/%s/comm", e->d_name);
        FILE *f = fopen(path, "r");
        if (f) {
            if (fgets(comm, sizeof comm, f) && strncmp(comm, "challoc-bg", 10) == 0)
                *bg = 1;
            fclose(f);
        }
    }
    closedir(d);
    return n;
}

/* Allocate and touch the working set. */
static void **grab(void)
{
    void **blocks = malloc((SMALL_N + HUGE_N) * sizeof *blocks);
    REQUIRE(blocks != NULL, "malloc table");
    for (int i = 0; i < SMALL_N; i++) {
        blocks[i] = malloc(SMALL_SIZE);
        REQUIRE(blocks[i] != NULL, "malloc(%d) #%d", SMALL_SIZE, i);
        memset(blocks[i], 0x5a, SMALL_SIZE);
    }
    for (int i = 0; i < HUGE_N; i++) {
        blocks[SMALL_N + i] = malloc(HUGE_SIZE);
        REQUIRE(blocks[SMALL_N + i] != NULL, "malloc(%d) huge #%d", HUGE_SIZE, i);
        memset(blocks[SMALL_N + i], 0x5a, HUGE_SIZE);
    }
    return blocks;
}

static void release(void **blocks)
{
    for (int i = 0; i < SMALL_N + HUGE_N; i++)
        free(blocks[i]);
    free(blocks);
}

/* Drive the slow path: a class with a 1024-slot cache refills and flushes
 * every 512 operations, and periodic work runs every 64 slow paths. 4096
 * blocks of one class per round is 16 slow paths, so a few rounds suffice;
 * do plenty. Uses a class the working set does not, so its slabs stay out
 * of the RSS comparison. */
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

/* calloc a block of each size that came back from purged memory and check
 * it is zero. The class blocks come from purged slabs, the huge one from a
 * purged extent; both paths must clear when they cannot trust the memory. */
static void check_calloc_zero(void)
{
    for (int i = 0; i < 64; i++) {
        unsigned char *p = calloc(1, SMALL_SIZE);
        REQUIRE(p != NULL, "calloc small");
        CHECK(is_zero(p, SMALL_SIZE), "calloc(%d) #%d not zero after purge", SMALL_SIZE, i);
        sink = p;
        memset(p, 0xff, SMALL_SIZE);
        free(p);
    }
    unsigned char *h = calloc(1, HUGE_SIZE);
    REQUIRE(h != NULL, "calloc huge");
    CHECK(is_zero(h, HUGE_SIZE), "calloc(%d) huge not zero after purge", HUGE_SIZE);
    sink = h;
    memset(h, 0xff, HUGE_SIZE);
    free(h);
}

/* Bytes released between two samples; 0 when RSS grew (the tick blocks
 * and stdio add a little). */
static size_t drop(size_t before, size_t after)
{
    return before > after ? before - after : 0;
}

static void report(const char *what, size_t before, size_t after)
{
    printf("%s: rss %zu -> %zu MiB\n", what, before / MIB, after / MIB);
}

/* The freed set is ~48 MiB; a purge frees most of it. Cached slots (at most
 * 128 KiB per class) and stdio stay. */
static const size_t DROP_MIN = 40 * MIB;
static const size_t STAY_MAX = 8 * MIB;

static void mode_trim(void)
{
    void **b = grab();
    size_t full = rss_bytes();
    release(b);
    size_t freed = rss_bytes();
    CHECK(drop(full, freed) < STAY_MAX, "free alone released %zu MiB", drop(full, freed) / MIB);
    int rc = malloc_trim(0);
    size_t trimmed = rss_bytes();
    report("trim", full, trimmed);
    CHECK(rc == 1, "malloc_trim returned %d, expected 1", rc);
    CHECK(drop(full, trimmed) >= DROP_MIN, "malloc_trim released only %zu MiB", drop(full, trimmed) / MIB);
    check_calloc_zero();
}

static void mode_decay(void)
{
    void **b = grab();
    size_t full = rss_bytes();
    release(b);
    tick();
    size_t early = rss_bytes();
    report("decay early", full, early);
    CHECK(drop(full, early) < STAY_MAX, "purged %zu MiB before the decay window", drop(full, early) / MIB);
    sleep_ms(400);
    tick();
    size_t late = rss_bytes();
    report("decay late", full, late);
    CHECK(drop(full, late) >= DROP_MIN, "decay released only %zu MiB", drop(full, late) / MIB);
    check_calloc_zero();
}

/* Progressive decay (design section 8): with a 2 s window, at 1 s the
 * slow path has purged part of the freed set (the curve is at 0.5, and
 * 0.32..0.68 across a 0.8..1.2 s timing slop; the 32 MiB of 1 KiB slabs
 * follow it slab by slab, the huge runs extent by extent), and by 2.1 s
 * all of it. */
static void mode_curve(void)
{
    void **b = grab();
    size_t full = rss_bytes();
    release(b);
    tick();
    size_t early = rss_bytes();
    report("curve early", full, early);
    CHECK(drop(full, early) < STAY_MAX, "purged %zu MiB in the first step", drop(full, early) / MIB);
    sleep_ms(1000);
    tick();
    size_t mid = rss_bytes();
    report("curve mid", full, mid);
    CHECK(drop(full, mid) >= 6 * MIB, "mid-window purge released only %zu MiB", drop(full, mid) / MIB);
    CHECK(drop(full, mid) < DROP_MIN, "mid-window purge released everything (%zu MiB)", drop(full, mid) / MIB);
    sleep_ms(1100);
    tick();
    size_t late = rss_bytes();
    report("curve late", full, late);
    CHECK(drop(full, late) >= DROP_MIN, "decay released only %zu MiB by the end of the window", drop(full, late) / MIB);
    CHECK(late <= mid, "RSS rose from mid-window (%zu) to the end (%zu)", mid / MIB, late / MIB);
    check_calloc_zero();
}

static void mode_never(void)
{
    void **b = grab();
    size_t full = rss_bytes();
    release(b);
    sleep_ms(400);
    tick();
    size_t later = rss_bytes();
    report("never", full, later);
    CHECK(drop(full, later) < STAY_MAX, "dirty_decay_ms:-1 still purged %zu MiB", drop(full, later) / MIB);
    CHECK(malloc_trim(0) == 1, "malloc_trim did not purge");
    size_t trimmed = rss_bytes();
    report("never+trim", full, trimmed);
    CHECK(drop(full, trimmed) >= DROP_MIN, "malloc_trim released only %zu MiB", drop(full, trimmed) / MIB);
}

static void mode_background(void)
{
    int bg;
    int tasks0 = count_tasks(&bg);
    CHECK(tasks0 == 1 && !bg, "unexpected threads before the first tick: %d (bg %d)", tasks0, bg);
    /* The thread is created from the slow path, not from init. It names
     * itself once running, so give it a moment. */
    tick();
    int tasks1 = 0;
    for (int i = 0; i < 100; i++) {
        tasks1 = count_tasks(&bg);
        if (tasks1 == 2 && bg)
            break;
        sleep_ms(10);
    }
    CHECK(tasks1 == 2 && bg, "background thread missing after a tick: %d tasks (bg %d)", tasks1, bg);

    void **b = grab();
    size_t full = rss_bytes();
    release(b);
    /* No allocator activity from here: only the thread can purge. */
    size_t late = 0;
    for (int i = 0; i < 40; i++) {
        sleep_ms(50);
        late = rss_bytes();
        if (drop(full, late) >= DROP_MIN)
            break;
    }
    report("background", full, late);
    CHECK(drop(full, late) >= DROP_MIN, "background thread released only %zu MiB", drop(full, late) / MIB);
    check_calloc_zero();

    /* The child has no background thread and does not get one back on its
     * own; it can still allocate, free and purge. */
    fflush(stdout);
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        int cbg;
        int ctasks = count_tasks(&cbg);
        if (ctasks != 1 || cbg) {
            fprintf(stderr, "child: %d tasks (bg %d)\n", ctasks, cbg);
            _exit(2);
        }
        void **cb = grab();
        size_t cfull = rss_bytes();
        release(cb);
        tick();
        int tasks_after_tick = count_tasks(&cbg);
        if (tasks_after_tick != 1 || cbg) {
            fprintf(stderr, "child grew a background thread after a tick\n");
            _exit(3);
        }
        if (malloc_trim(0) != 1) {
            fprintf(stderr, "child: malloc_trim purged nothing\n");
            _exit(4);
        }
        size_t ctrim = rss_bytes();
        if (drop(cfull, ctrim) < DROP_MIN) {
            fprintf(stderr, "child: malloc_trim released only %zu MiB\n", drop(cfull, ctrim) / MIB);
            _exit(5);
        }
        _exit(0);
    }
    int status;
    REQUIRE(waitpid(pid, &status, 0) == pid, "waitpid: %s", strerror(errno));
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child failed: status %#x", status);
    /* The parent still has its thread. */
    int tasks2 = count_tasks(&bg);
    CHECK(tasks2 == 2 && bg, "parent lost its background thread: %d tasks (bg %d)", tasks2, bg);
}

static long minflt(void)
{
    struct rusage r;
    REQUIRE(getrusage(RUSAGE_SELF, &r) == 0, "getrusage: %s", strerror(errno));
    return r.ru_minflt;
}

/* Write every 4 KiB of [p, p + n) through a volatile pointer: a memset
 * right before free is a dead store the compiler may drop. */
static void touch(void *p, size_t n)
{
    volatile unsigned char *c = p;
    for (size_t i = 0; i < n; i += 4096)
        c[i] = 0x5a;
}

enum { BURSTS = 8, BURST_N = 8192, BURST_SIZE = 1024, BURST_GAP_MS = 150 };

/* One 8 MiB burst of 1 KiB blocks, touched and freed, then a tick so the
 * walks see the fall. Returns the minor faults it took. */
static long burst(void)
{
    static void *blocks[BURST_N];
    long f0 = minflt();
    for (int i = 0; i < BURST_N; i++) {
        blocks[i] = malloc(BURST_SIZE);
        REQUIRE(blocks[i] != NULL, "malloc(%d) #%d", BURST_SIZE, i);
        touch(blocks[i], BURST_SIZE);
    }
    for (int i = 0; i < BURST_N; i++)
        free(blocks[i]);
    tick();
    return minflt() - f0;
}

/* The demand floor for slabs (design 8): recurring bursts are kept between
 * bursts once the second has shown the level recurs; without the window
 * the curve purges each burst before the next. */
static void mode_demand(int on)
{
    long f[BURSTS];
    for (int i = 0; i < BURSTS; i++) {
        if (i > 0)
            sleep_ms(BURST_GAP_MS);
        f[i] = burst();
        printf("demand%s burst %d: %ld minor faults\n", on ? "" : "-off", i, f[i]);
    }
    CHECK(f[0] >= 256, "the first burst took only %ld faults", f[0]);
    for (int i = 2; i < BURSTS; i++) {
        if (on)
            CHECK(f[i] * 8 <= f[0], "burst %d took %ld faults (first %ld): not kept", i, f[i], f[0]);
        else
            CHECK(f[i] * 4 >= f[0], "burst %d took %ld faults (first %ld) without a window", i, f[i], f[0]);
    }
    if (!on)
        return;
    /* Idle with the slow path ticking: kept well past the curve's 200 ms,
     * released within the 4 s window, an epoch and a hold. */
    size_t kept = rss_bytes();
    long t0 = now_ms(), released = -1;
    size_t at1s = 0;
    while (now_ms() - t0 < 7000) {
        sleep_ms(100);
        tick();
        size_t r = rss_bytes();
        if (!at1s && now_ms() - t0 >= 1000)
            at1s = r;
        if (drop(kept, r) >= 6 * MIB) {
            released = now_ms() - t0;
            break;
        }
    }
    printf("demand idle: rss %zu MiB kept, %zu MiB at 1 s, released after %ld ms\n", kept / MIB, at1s / MIB, released);
    CHECK(at1s != 0 && drop(kept, at1s) < 2 * MIB, "the floor released %zu MiB within 1 s", drop(kept, at1s) / MIB);
    CHECK(released >= 0, "not released within 7 s of idle");
}

/* The huge tier's recurrence test (design 7): a run above huge_retain_max
 * (64 MiB) reallocated every iteration stays resident from its second
 * free on; a one-off larger run is still purged at free. */
static void mode_hugedemand(int on)
{
    enum { ITERS = 5 };
    const size_t big = 100 * MIB, oneoff = 192 * MIB;
    long f[ITERS];
    for (int i = 0; i < ITERS; i++) {
        long f0 = minflt();
        unsigned char *p = malloc(big);
        REQUIRE(p != NULL, "malloc(100 MiB) #%d", i);
        touch(p, big);
        free(p);
        f[i] = minflt() - f0;
        printf("hugedemand%s iteration %d: %ld minor faults\n", on ? "" : "-off", i, f[i]);
    }
    CHECK(f[0] >= 40, "the first iteration took only %ld faults", f[0]);
    for (int i = 2; i < ITERS; i++) {
        if (on)
            CHECK(f[i] * 10 <= f[0], "iteration %d took %ld faults (first %ld): not kept", i, f[i], f[0]);
        else
            CHECK(f[i] * 2 >= f[0], "iteration %d took %ld faults (first %ld) without a window", i, f[i], f[0]);
    }
    unsigned char *q = malloc(oneoff);
    REQUIRE(q != NULL, "malloc(192 MiB)");
    touch(q, oneoff);
    size_t full = rss_bytes();
    free(q);
    size_t after = rss_bytes();
    report("hugedemand one-off", full, after);
    CHECK(drop(full, after) >= oneoff - 16 * MIB, "the one-off run released only %zu MiB at free", drop(full, after) / MIB);
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: decay <trim|decay|curve|never|background|demand|demand-off|hugedemand|hugedemand-off>");
    printf("decay: start %s\n", argv[1]);
    fflush(stdout);
    require_challoc();
    if (strcmp(argv[1], "trim") == 0)
        mode_trim();
    else if (strcmp(argv[1], "decay") == 0)
        mode_decay();
    else if (strcmp(argv[1], "curve") == 0)
        mode_curve();
    else if (strcmp(argv[1], "never") == 0)
        mode_never();
    else if (strcmp(argv[1], "background") == 0)
        mode_background();
    else if (strcmp(argv[1], "demand") == 0)
        mode_demand(1);
    else if (strcmp(argv[1], "demand-off") == 0)
        mode_demand(0);
    else if (strcmp(argv[1], "hugedemand") == 0)
        mode_hugedemand(1);
    else if (strcmp(argv[1], "hugedemand-off") == 0)
        mode_hugedemand(0);
    else
        REQUIRE(0, "unknown mode %s", argv[1]);
    return finish("decay");
}
