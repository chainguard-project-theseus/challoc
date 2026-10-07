/*
 * Purge over locked memory under LD_PRELOAD. madvise(MADV_DONTNEED) and
 * madvise(MADV_FREE) fail with EINVAL on a range that holds mlock()ed
 * pages; the purge must leave such memory dirty (`Empty`, never assumed
 * zero) and carry on, and purge it once the lock is gone. The driver in
 * src/api/c_programs.rs runs one mode per process:
 *
 *   huge        (defaults)                 a freed, locked 1 MiB huge run:
 *                                          malloc_trim(0) survives, calloc
 *                                          of the run is zero, munlock and
 *                                          the next trim purge it
 *   slab        (defaults)                 the same for 2 KiB blocks whose
 *                                          slabs are locked
 *   free        huge_dirty_decay_ms:0      the purge at free time of a
 *                                          locked huge run
 *   decay       dirty_decay_ms:100         the decay walk (slow path ticks)
 *                                          over a locked run and locked slabs
 *   background  background_thread:true,    the same from the background
 *               dirty_decay_ms:100         thread
 *
 * Each mode reads the `failed_discards` count of malloc_stats_print's
 * `purge advice:` line, so a run that never reached the locked memory
 * fails instead of passing. The 1 MiB locked (2 MiB in the decay modes)
 * fits the default RLIMIT_MEMLOCK (8 MiB); a host where mlock fails skips.
 */
#include "common.h"

#include <sys/mman.h>

enum { HUGE_SIZE = 1 << 20, SMALL_SIZE = 2048, SMALL_N = 512 };

static long page_size(void)
{
    return sysconf(_SC_PAGESIZE);
}

/* Pages of `[p, p + len)` resident, through mincore. */
static size_t resident(uintptr_t p, size_t len)
{
    size_t page = (size_t)page_size();
    uintptr_t lo = p & ~(page - 1);
    uintptr_t hi = (p + len + page - 1) & ~(page - 1);
    size_t n = (hi - lo) / page;
    unsigned char vec[n];
    REQUIRE(mincore((void *)lo, hi - lo, vec) == 0, "mincore: %s", strerror(errno));
    size_t r = 0;
    for (size_t i = 0; i < n; i++)
        r += vec[i] & 1;
    return r;
}

/* The report's `failed_discards N` (ranges whose discard failed so far),
 * -1 when the report has no such count. */
struct grab {
    long failed;
};

static void grab_line(void *opaque, const char *s)
{
    struct grab *g = opaque;
    const char *f = strstr(s, "failed_discards ");
    if (f)
        g->failed = strtol(f + strlen("failed_discards "), NULL, 10);
}

static long failed_discards(void)
{
    struct grab g = {-1};
    malloc_stats_print(grab_line, &g, NULL);
    return g.failed;
}

/* mlock, or skip the mode when the host refuses (a lower RLIMIT_MEMLOCK). */
static int lock(const char *mode, uintptr_t p, size_t len)
{
    if (mlock((void *)p, len) == 0)
        return 1;
    skip_note(mode, "mlock(%zu) failed: %s", len, strerror(errno));
    return 0;
}

static void unlock(uintptr_t p, size_t len)
{
    REQUIRE(munlock((void *)p, len) == 0, "munlock: %s", strerror(errno));
}

/* Allocate and touch a huge run, lock it, free it. Returns its address,
 * 0 when the mode skips. */
static uintptr_t locked_huge(const char *mode)
{
    unsigned char *p = malloc(HUGE_SIZE);
    REQUIRE(p != NULL, "malloc huge");
    memset(p, 1, HUGE_SIZE);
    uintptr_t a = (uintptr_t)p;
    if (!lock(mode, a, HUGE_SIZE)) {
        free(p);
        return 0;
    }
    free(p);
    return a;
}

/* Allocate and touch SMALL_N blocks, lock them, free them; `addrs` keeps
 * the addresses for the munlock. 0 when the mode skips. */
static int locked_slabs(const char *mode, uintptr_t *addrs)
{
    static void *blocks[SMALL_N];
    for (int i = 0; i < SMALL_N; i++) {
        blocks[i] = malloc(SMALL_SIZE);
        REQUIRE(blocks[i] != NULL, "malloc(%d) #%d", SMALL_SIZE, i);
        memset(blocks[i], 1, SMALL_SIZE);
        addrs[i] = (uintptr_t)blocks[i];
    }
    int locked = 0;
    while (locked < SMALL_N && lock(mode, addrs[locked], SMALL_SIZE))
        locked++;
    if (locked < SMALL_N)
        for (int j = 0; j < locked; j++)
            unlock(addrs[j], SMALL_SIZE);
    for (int i = 0; i < SMALL_N; i++)
        free(blocks[i]);
    return locked == SMALL_N;
}

static void unlock_slabs(const uintptr_t *addrs)
{
    for (int i = 0; i < SMALL_N; i++)
        unlock(addrs[i], SMALL_SIZE);
}

/* calloc a run of the locked size: it reuses the locked run, which stayed
 * dirty, so calloc must clear it. */
static void check_huge_calloc(uintptr_t old)
{
    unsigned char *p = calloc(1, HUGE_SIZE);
    REQUIRE(p != NULL, "calloc huge");
    CHECK((uintptr_t)p == old, "calloc(%d) did not reuse the locked run", HUGE_SIZE);
    CHECK(is_zero(p, HUGE_SIZE), "calloc(%d) not zero after a failed purge", HUGE_SIZE);
    free(p);
}

/* calloc as many small blocks: most land in the locked slabs, which
 * stayed dirty, and every one must be zero. */
static void check_small_calloc(const uintptr_t *addrs)
{
    static void *again[SMALL_N];
    int reused = 0;
    for (int i = 0; i < SMALL_N; i++) {
        unsigned char *p = calloc(1, SMALL_SIZE);
        REQUIRE(p != NULL, "calloc small #%d", i);
        CHECK(is_zero(p, SMALL_SIZE), "calloc(%d) #%d not zero after a failed purge", SMALL_SIZE, i);
        for (int j = 0; j < SMALL_N; j++)
            if ((uintptr_t)p == addrs[j]) {
                reused++;
                break;
            }
        again[i] = p;
    }
    CHECK(reused >= SMALL_N / 2, "only %d of %d calloc blocks reused the locked slabs", reused, SMALL_N);
    for (int i = 0; i < SMALL_N; i++)
        free(again[i]);
}

/* After the munlock: malloc_trim(0) purges what stayed dirty with no
 * failure, and the run (0 for none) and the slabs (NULL for none) leave
 * RSS. */
static void check_purged_after_unlock(uintptr_t huge, const uintptr_t *addrs)
{
    long f = failed_discards();
    malloc_trim(0);
    CHECK(failed_discards() == f, "a discard failed after munlock");
    if (huge)
        CHECK(resident(huge, HUGE_SIZE) == 0, "the unlocked run is still resident after malloc_trim");
    if (addrs) {
        size_t res = 0;
        for (int i = 0; i < SMALL_N; i++)
            res += resident(addrs[i], SMALL_SIZE);
        CHECK(res < SMALL_N / 2, "%zu pages of the unlocked slabs still resident after malloc_trim", res);
    }
}

static void mode_huge(void)
{
    long f0 = failed_discards();
    uintptr_t p = locked_huge("mlock_purge/huge");
    if (!p)
        return;
    malloc_trim(0);
    CHECK(failed_discards() > f0, "malloc_trim did not try the locked run");
    /* Retried at the next explicit purge, failing again. */
    long f1 = failed_discards();
    malloc_trim(0);
    CHECK(failed_discards() > f1, "the second malloc_trim did not retry the locked run");
    check_huge_calloc(p);
    unlock(p, HUGE_SIZE);
    check_purged_after_unlock(p, NULL);
}

static void mode_slab(void)
{
    static uintptr_t addrs[SMALL_N];
    long f0 = failed_discards();
    if (!locked_slabs("mlock_purge/slab", addrs))
        return;
    malloc_trim(0);
    CHECK(failed_discards() > f0, "malloc_trim did not try the locked slabs");
    check_small_calloc(addrs);
    unlock_slabs(addrs);
    check_purged_after_unlock(0, addrs);
}

static void mode_free(void)
{
    long f0 = failed_discards();
    uintptr_t p = locked_huge("mlock_purge/free");
    if (!p)
        return;
    CHECK(failed_discards() > f0, "free did not try to purge the locked run");
    check_huge_calloc(p);
    unlock(p, HUGE_SIZE);
    check_purged_after_unlock(p, NULL);
}

/* Drive the slow path (periodic work runs every 64 slow paths), with a
 * class nothing else here uses. */
static void tick(void)
{
    enum { N = 4096, ROUNDS = 4 };
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

/* Lock a huge run and slabs, free them and let decay find them: by the
 * foreground ticks, or by the background thread. */
static void mode_decay(const char *mode, int ticking)
{
    static uintptr_t addrs[SMALL_N];
    if (!ticking) {
        /* The thread is created from the slow path; from then on it does
         * the purging and the slow path does none. */
        tick();
        sleep_ms(50);
    }
    long f0 = failed_discards();
    uintptr_t p = locked_huge(mode);
    if (!p)
        return;
    if (!locked_slabs(mode, addrs)) {
        unlock(p, HUGE_SIZE);
        return;
    }
    /* Times from the frees: the 100 ms window has passed for all of it by
     * 200 ms, so every owner holding locked memory (the huge tier, the
     * 2 KiB class) has had its failed walk, which backs it off for a
     * second; from 200 to 400 ms nothing is retried. */
    long freed = now_ms();
    while (failed_discards() <= f0 && now_ms() < freed + 2000) {
        if (ticking)
            tick();
        sleep_ms(20);
    }
    CHECK(failed_discards() > f0, "decay did not try the locked memory within 2 s");
    while (now_ms() < freed + 200) {
        if (ticking)
            tick();
        sleep_ms(10);
    }
    long f1 = failed_discards();
    while (now_ms() < freed + 400) {
        if (ticking)
            tick();
        sleep_ms(10);
    }
    long retries = failed_discards() - f1;
    long late = now_ms() - freed;
    printf("%s: %ld failed discards by 200 ms, %ld more by %ld ms\n", mode, f1 - f0, retries, late);
    if (late > 900)
        skip_note(mode, "too slow to see the back-off (%ld ms for a 400 ms wait)", late);
    else
        CHECK(retries == 0, "decay retried the locked memory %ld times within its back-off", retries);
    check_huge_calloc(p);
    check_small_calloc(addrs);
    unlock(p, HUGE_SIZE);
    unlock_slabs(addrs);
    check_purged_after_unlock(p, addrs);
}

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(argc == 2, "usage: mlock_purge huge|slab|free|decay|background");
    printf("mlock_purge: start %s\n", argv[1]);
    fflush(stdout);
    if (strcmp(argv[1], "huge") == 0)
        mode_huge();
    else if (strcmp(argv[1], "slab") == 0)
        mode_slab();
    else if (strcmp(argv[1], "free") == 0)
        mode_free();
    else if (strcmp(argv[1], "decay") == 0)
        mode_decay("mlock_purge/decay", 1);
    else if (strcmp(argv[1], "background") == 0)
        mode_decay("mlock_purge/background", 0);
    else
        REQUIRE(0, "unknown mode %s", argv[1]);
    return finish("mlock_purge");
}
