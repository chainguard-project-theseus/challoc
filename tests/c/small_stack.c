/*
 * Every exported entry point on a thread with the smallest stack glibc
 * allows (PTHREAD_STACK_MIN, 16 KiB on x86_64), after the thread has
 * already used HEADROOM bytes of it for its own frames: a correct program
 * that works under glibc's malloc and jemalloc must not die of a stack
 * overflow inside the allocator (hunt 943666c7: the statistics entry points
 * and the deferred cgroup discovery under `fork` and the periodic tick
 * once needed 20 to 50 KiB; they run on the allocator's cold stacks now,
 * `sys::cold`).
 *
 * The driver (no argument) runs every operation of the table in a fresh
 * process (this program re-executed with the operation's name), so each
 * meets the allocator in the state its row names: `cold` right after init
 * (the cgroup not yet discovered, the thread's arena not yet built),
 * `warm` after the discovery's deferral has run out with nothing having
 * ticked (the operation's own tick, or `fork`'s prepare handler, runs the
 * discovery). The operation runs on a new PTHREAD_STACK_MIN thread; a
 * child that dies of a signal fails the row.
 */
#include "common.h"
#include <fcntl.h>
#include <limits.h>

/* What the thread has used of its stack before the operation: what a
 * small program's own frames and libc's (`printf`) take on such a thread.
 * glibc's malloc leaves about 9 KiB of the 16 for it on `malloc` and
 * `fork` (5 on `malloc_info`); challoc leaves 8 on every row here, so 6
 * keeps a margin for other builds and compilers. */
#define HEADROOM 6144

extern void *_Znwm(size_t) WEAK;
extern void _ZdlPv(void *) WEAK;
extern void _ZdlPvm(void *, size_t) WEAK;
extern void *_ZnwmSt11align_val_t(size_t, size_t) WEAK;
extern void _ZdlPvSt11align_val_t(void *, size_t) WEAK;
extern int je_mallctl(const char *, void *, size_t *, void *, size_t) WEAK;
extern void je_malloc_stats_print(void (*)(void *, const char *), void *, const char *) WEAK;
extern int get_defrag_hint(void *) WEAK;

static void discard(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
}

static int ctl_read_exact(const char *name, size_t len)
{
    char buf[64];
    return mallctl(name, buf, &len, NULL, 0);
}

static void *volatile keep;

static void op_malloc_family(void)
{
    void *p = malloc(100);
    p = realloc(p, 5000);
    free(p);
    free(calloc(10, 100));
    void *q = NULL;
    CHECK(posix_memalign(&q, 256, 1000) == 0, "posix_memalign");
    free(q);
    free(aligned_alloc(64, 640));
    free(memalign(128, 300));
    free(valloc(100));
    free(pvalloc(100));
    free(reallocarray(NULL, 10, 10));
    p = malloc(48);
    CHECK(malloc_usable_size(p) >= 48, "usable");
    free_sized(p, 48);
    free_aligned_sized(aligned_alloc(64, 64), 64, 64);
    size_t u = 0;
    p = challoc_malloc_usable(70, &u);
    p = challoc_realloc_usable(p, 700, &u);
    challoc_free_usable(p);
    challoc_free_usable(challoc_calloc_usable(3, 30, &u));
}

static void op_many(void)
{
    static void *a[4000];
    for (int round = 0; round < 3; round++) {
        for (int i = 0; i < 4000; i++)
            a[i] = malloc(16 + (size_t)(i % 97) * 24);
        for (int i = 0; i < 4000; i++)
            free(a[i]);
    }
}

static void op_huge(void)
{
    void *p = malloc(64 << 20);
    p = realloc(p, 96 << 20);
    free(p);
}

static void op_jemalloc(void)
{
    void *p = mallocx(100, 0);
    p = rallocx(p, 300, MALLOCX_ZERO);
    CHECK(xallocx(p, 300, 0, 0) >= 300, "xallocx");
    CHECK(sallocx(p, 0) >= 300, "sallocx");
    CHECK(get_defrag_hint(p) >= 0, "get_defrag_hint");
    dallocx(p, 0);
    sdallocx(mallocx(64, MALLOCX_LG_ALIGN(6)), 64, MALLOCX_LG_ALIGN(6));
    CHECK(nallocx(100, 0) >= 100, "nallocx");
    dallocx(mallocx(100, MALLOCX_TCACHE_NONE), MALLOCX_TCACHE_NONE);
}

static void op_cxx(void)
{
    void *p = _Znwm(100);
    _ZdlPv(p);
    _ZdlPvm(_Znwm(64), 64);
    _ZdlPvSt11align_val_t(_ZnwmSt11align_val_t(100, 256), 256);
}

static void op_glibc(void)
{
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    struct mallinfo m = mallinfo();
#pragma GCC diagnostic pop
    struct mallinfo2 m2 = mallinfo2();
    keep = (void *)(uintptr_t)(m.uordblks + m2.uordblks);
    keep = (void *)(uintptr_t)mallopt(M_ARENA_MAX, 8);
    malloc_trim(0);
}

static void op_malloc_info(void)
{
    FILE *f = fopen("/dev/null", "w");
    REQUIRE(f, "fopen /dev/null");
    CHECK(malloc_info(0, f) == 0, "malloc_info");
    fclose(f);
}

static void op_malloc_stats(void)
{
    int saved = dup(2), nul = open("/dev/null", O_WRONLY);
    REQUIRE(saved >= 0 && nul >= 0, "dup");
    dup2(nul, 2);
    malloc_stats();
    dup2(saved, 2);
    close(nul);
    close(saved);
}

static void op_stats_print(void)
{
    static const char *const opts[] = {
        "", "J", "g", "a", "b", "ga", "gab", "Jg", "Ja", "Jb", "Jga", "Jgab", "Jmdablxeh", "mdablxeh",
    };
    for (size_t i = 0; i < sizeof opts / sizeof opts[0]; i++)
        malloc_stats_print(discard, NULL, opts[i]);
    malloc_stats_print(discard, NULL, NULL);
    je_malloc_stats_print(discard, NULL, "J");
}

static void op_mallctl(void)
{
    static const struct {
        const char *name;
        size_t len;
    } reads[] = {
        {"version", sizeof(char *)},
        {"stats.allocated", sizeof(size_t)},
        {"stats.active", sizeof(size_t)},
        {"stats.metadata", sizeof(size_t)},
        {"stats.resident", sizeof(size_t)},
        {"stats.mapped", sizeof(size_t)},
        {"stats.retained", sizeof(size_t)},
        {"stats.arenas.0.pactive", sizeof(size_t)},
        {"stats.arenas.0.pdirty", sizeof(size_t)},
        {"stats.arenas.0.small.allocated", sizeof(size_t)},
        {"stats.arenas.0.large.allocated", sizeof(size_t)},
        {"stats.arenas.4096.pactive", sizeof(size_t)},
        {"stats.arenas.0.bins.0.curregs", sizeof(size_t)},
        {"stats.arenas.0.bins.0.curslabs", sizeof(size_t)},
        {"stats.arenas.0.bins.0.nonfull_slabs", sizeof(size_t)},
        {"arenas.narenas", sizeof(unsigned)},
        {"arenas.bin.0.size", sizeof(size_t)},
        {"opt.narenas", sizeof(unsigned)},
        {"thread.arena", sizeof(unsigned)},
        {"thread.allocatedp", sizeof(uint64_t *)},
    };
    uint64_t e = 1;
    size_t elen = sizeof e;
    CHECK(mallctl("epoch", &e, &elen, &e, sizeof e) == 0, "epoch");
    for (size_t i = 0; i < sizeof reads / sizeof reads[0]; i++)
        CHECK(ctl_read_exact(reads[i].name, reads[i].len) == 0, "%s", reads[i].name);
    CHECK(je_mallctl("stats.allocated", &e, &elen, NULL, 0) == 0, "je_mallctl");
    unsigned a = 0;
    size_t alen = sizeof a;
    CHECK(mallctl("arenas.create", &a, &alen, NULL, 0) == 0, "arenas.create");
    CHECK(mallctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "tcache.flush");
    CHECK(mallctl("arena.0.purge", NULL, NULL, NULL, 0) == 0, "arena.0.purge");
    unsigned tc = 0;
    size_t tclen = sizeof tc;
    if (mallctl("tcache.create", &tc, &tclen, NULL, 0) == 0)
        CHECK(mallctl("tcache.destroy", NULL, NULL, &tc, sizeof tc) == 0, "tcache.destroy");
    size_t mib[8], miblen = 8;
    CHECK(mallctlnametomib("stats.arenas.0.bins.0.curregs", mib, &miblen) == 0, "nametomib");
    size_t v = 0, vlen = sizeof v;
    CHECK(mallctlbymib(mib, miblen, &v, &vlen, NULL, 0) == 0, "bymib");
}

static void op_cgroup(void)
{
    /* A read of the cgroup figures discovers the cgroup, waiting. */
    int rc = ctl_read_exact("stats.cgroup.limit", sizeof(size_t));
    CHECK(rc == 0 || rc == EINVAL, "stats.cgroup.limit: %d", rc);
}

static void op_fork(void)
{
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        free(malloc(10));
        _exit(0);
    }
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        REQUIRE(errno == EINTR, "waitpid");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "fork child status %#x", st);
}

static void op_init(void)
{
    /* The process's first allocation (main has not allocated). */
    require_challoc();
    op_malloc_family();
}

static const struct row {
    const char *name;
    void (*fn)(void);
    bool warm;
} rows[] = {
    {"init", op_init, false},
    {"malloc_family", op_malloc_family, false},
    {"many", op_many, false},
    {"many_warm", op_many, true},
    {"huge", op_huge, false},
    {"jemalloc", op_jemalloc, false},
    {"cxx", op_cxx, false},
    {"glibc", op_glibc, false},
    {"malloc_info", op_malloc_info, false},
    {"malloc_stats", op_malloc_stats, false},
    {"stats_print", op_stats_print, false},
    {"mallctl", op_mallctl, false},
    {"cgroup", op_cgroup, false},
    {"fork", op_fork, false},
    {"fork_warm", op_fork, true},
};
#define NROWS (sizeof rows / sizeof rows[0])

/* Use HEADROOM bytes of the stack in this frame, then run the operation
 * from below them. */
__attribute__((noinline)) static void deep(const struct row *r)
{
    volatile char used[HEADROOM];
    for (size_t i = 0; i < sizeof used; i += 64)
        used[i] = (char)i;
    r->fn();
    __asm__ volatile("" ::"r"(used) : "memory");
}

static void *body(void *arg)
{
    deep(arg);
    return NULL;
}

static int run_one(const struct row *r)
{
    if (r->warm) {
        /* Past the cgroup discovery's deferral (20 ms), with nothing
         * allocating meanwhile, so the next tick discovers. */
        struct timespec t = {0, 100 * 1000 * 1000};
        nanosleep(&t, NULL);
    }
    pthread_attr_t at;
    REQUIRE(pthread_attr_init(&at) == 0, "attr");
    REQUIRE(pthread_attr_setstacksize(&at, PTHREAD_STACK_MIN) == 0, "setstacksize");
    pthread_t th;
    REQUIRE(pthread_create(&th, &at, body, (void *)r) == 0, "pthread_create");
    REQUIRE(pthread_join(th, NULL) == 0, "pthread_join");
    if (failures)
        return 1;
    printf("done %s\n", r->name);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "init") != 0)
        require_challoc();
    if (argc > 1) {
        for (size_t i = 0; i < NROWS; i++)
            if (strcmp(argv[1], rows[i].name) == 0)
                return run_one(&rows[i]);
        REQUIRE(0, "no operation %s", argv[1]);
    }
    require_challoc();
    printf("stack %zu bytes, %d used before each operation\n", (size_t)PTHREAD_STACK_MIN, HEADROOM);
    fflush(stdout);
    for (size_t i = 0; i < NROWS; i++) {
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork: %s", strerror(errno));
        if (pid == 0) {
            execl("/proc/self/exe", argv[0], rows[i].name, (char *)NULL);
            _exit(127);
        }
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            REQUIRE(errno == EINTR, "waitpid");
        if (WIFSIGNALED(st))
            CHECK(0, "%s: killed by signal %d (%s)", rows[i].name, WTERMSIG(st), strsignal(WTERMSIG(st)));
        else
            CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "%s: exit status %d", rows[i].name, WEXITSTATUS(st));
    }
    if (failures)
        return 1;
    printf("ok small_stack\n");
    return 0;
}
