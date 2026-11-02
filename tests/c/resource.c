/*
 * Resource failures under LD_PRELOAD (docs/design.md 14.1): a correct
 * program under a resource limit gets memory or NULL with ENOMEM, never a
 * `challoc:` abort, and once the pressure lifts it allocates again (no
 * failure is remembered). The driver in src/api/c_programs.rs runs one
 * mode per process; a mode whose limit must be in force before the
 * allocator starts re-executes this program under it (`run_child`), so
 * the child's first malloc is challoc's init.
 *
 *   data_init   RLIMIT_DATA of 32 MiB from the start, under the ~85 MiB
 *               the eager metadata tables would take (design 2): a 15 MB
 *               working set runs; then the soft limit is cut to the current
 *               VmData, a 64 MiB malloc returns NULL/ENOMEM, and after
 *               the limit is raised again it succeeds
 *   as_min      RLIMIT_AS just above the documented minimum layout
 *               (design 2: about 435 GiB at quantum 16, 468 at 8): 16
 *               children in a row all start (the reservation order is
 *               random per process, so one start would not show that
 *               every order fits)
 *   arenas_create  arenas.create until EAGAIN, then 32 threads each
 *               allocate small, medium and huge blocks: every block
 *               arrives (create keeps room for the automatic arenas)
 *   narenas     (narenas:1000) 600 concurrent threads, more automatic
 *               arenas than the metadata region holds: every thread is
 *               served (an unbuildable home falls back to arena 0), and
 *               the main thread then holds 20000 huge blocks (no failed
 *               build stranded the region's room)
 *   data_threads  RLIMIT_DATA of 3 to 128 MiB from the start; 16 threads at
 *               once each allocate 32 B, then 5 MiB to 2 GiB through
 *               malloc, calloc, posix_memalign, aligned_alloc, mallocx
 *               and realloc, taking NULL where the limit refuses: no
 *               abort (a thread whose arena the limit refuses is served
 *               from arena 0, or gets NULL)
 *   data_thread RLIMIT_DATA cut to VmData, then a new thread whose arena
 *               cannot be built allocates 16 B and 1 MiB: NULL/ENOMEM or
 *               a block, never an abort; after the limit is raised a new
 *               thread allocates normally
 *   mlock_huge  a freed huge run left mlock()ed (no munlock before free),
 *               then huge mallocs placed there: every one arrives
 *               (MADV_GUARD_INSTALL refuses a locked range; the guard is a
 *               PROT_NONE page instead)
 *   mlockall    mlockall(MCL_CURRENT|MCL_FUTURE), then small, slab-guard
 *               and huge allocations all arrive (a smoke test of the
 *               locked paths); skipped when mlockall is refused
 *               (RLIMIT_MEMLOCK)
 *   map_count   mlockall(MCL_CURRENT), then vm.max_map_count filled with
 *               mappings:
 *               4 KiB mallocs end in NULL/ENOMEM, never an abort (a guard
 *               slab that can be neither marked nor split is not handed
 *               out); after the mappings are unmapped they arrive again
 */
#include "common.h"

#include <sched.h>
#include <sys/mman.h>
#include <sys/resource.h>

#define MIB ((size_t)1 << 20)
#define GIB ((size_t)1 << 30)

static const char *self_path;

/* VmData of this process, in bytes. */
static size_t vm_data(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    REQUIRE(f, "open /proc/self/status: %s", strerror(errno));
    char line[256];
    size_t kb = 0;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "VmData: %zu kB", &kb) == 1)
            break;
    fclose(f);
    REQUIRE(kb > 0, "no VmData line");
    return kb * 1024;
}

static void set_soft(int res, rlim_t soft)
{
    struct rlimit r;
    REQUIRE(getrlimit(res, &r) == 0, "getrlimit: %s", strerror(errno));
    r.rlim_cur = soft;
    REQUIRE(setrlimit(res, &r) == 0, "setrlimit(%d, %llu): %s", res, (unsigned long long)soft,
            strerror(errno));
}

/* Run this program as `<self> <mode>` under soft limit `soft` of `res`;
 * the child's exit status, or -signal when it was killed. */
static int run_child(const char *mode, int res, rlim_t soft)
{
    fflush(NULL);
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        set_soft(res, soft);
        execl(self_path, self_path, mode, (char *)NULL);
        _exit(127);
    }
    int st;
    REQUIRE(waitpid(pid, &st, 0) == pid, "waitpid: %s", strerror(errno));
    return WIFEXITED(st) ? WEXITSTATUS(st) : -WTERMSIG(st);
}

/* A small correct program: 64 x 100 KB, 1000 small blocks, one 8 MiB
 * buffer, each filled and checked, then freed. */
static void workload(void)
{
    void *mid[64], *small[1000];
    for (int i = 0; i < 64; i++) {
        mid[i] = malloc(100000);
        REQUIRE(mid[i], "malloc(100000) #%d: %s", i, strerror(errno));
        fill(mid[i], 100000, (uint32_t)i);
    }
    for (int i = 0; i < 1000; i++) {
        small[i] = malloc(16 + (size_t)(i % 200));
        REQUIRE(small[i], "malloc small #%d: %s", i, strerror(errno));
        fill(small[i], 16, (uint32_t)i);
    }
    void *big = malloc(8 * MIB);
    REQUIRE(big, "malloc(8 MiB): %s", strerror(errno));
    fill(big, 8 * MIB, 7);
    CHECK(check_fill(big, 8 * MIB, 7), "8 MiB buffer changed");
    for (int i = 0; i < 64; i++) {
        CHECK(check_fill(mid[i], 100000, (uint32_t)i), "100 KB #%d changed", i);
        free(mid[i]);
    }
    for (int i = 0; i < 1000; i++) {
        CHECK(check_fill(small[i], 16, (uint32_t)i), "small #%d changed", i);
        free(small[i]);
    }
    free(big);
}

static int child_data_init(void)
{
    require_challoc();
    workload();
    /* No room left: NULL with ENOMEM, not an abort. */
    struct rlimit r;
    REQUIRE(getrlimit(RLIMIT_DATA, &r) == 0, "getrlimit");
    set_soft(RLIMIT_DATA, vm_data());
    errno = 0;
    void *p = malloc(64 * MIB);
    CHECK(p == NULL && errno == ENOMEM, "malloc(64 MiB) at the limit = %p errno %d", p, errno);
    free(p);
    /* The limit lifted: the same request succeeds. */
    set_soft(RLIMIT_DATA, r.rlim_max);
    p = malloc(64 * MIB);
    CHECK(p != NULL, "malloc(64 MiB) after the limit was raised: %s", strerror(errno));
    if (p)
        fill(p, 64 * MIB, 3);
    free(p);
    workload();
    return finish("resource child");
}

/* Large requests of every kind, each taking NULL as a valid answer. */
static volatile int big_go;

static void *big_requests(void *arg)
{
    (void)arg;
    while (!big_go)
        sched_yield();
    void *small = malloc(32);
    for (size_t sz = 5 * MIB; sz <= 2 * GIB; sz *= 4) {
        void *p = malloc(sz);
        free(p);
        p = calloc(1, sz);
        free(p);
        p = NULL;
        if (posix_memalign(&p, 1 << 16, sz) == 0)
            free(p);
        p = aligned_alloc(1 << 12, sz);
        free(p);
        if (mallocx) {
            p = mallocx(sz, 0);
            if (p)
                sdallocx(p, sz, 0);
        }
        void *r = malloc(64);
        void *q = r ? realloc(r, sz) : NULL;
        free(q ? q : r);
    }
    free(small);
    return NULL;
}

static int child_data_threads(void)
{
    require_challoc();
    /* Small stacks: a thread's stack is a private writable mapping the
     * data limit counts too. */
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 64 * 1024);
    /* glibc may refuse a thread's stack under the limit (64 KiB pages
     * make the floor higher on aarch64): the threads that start run. */
    pthread_t t[16];
    int n = 0;
    while (n < 16 && pthread_create(&t[n], &attr, big_requests, NULL) == 0)
        n++;
    big_go = 1;
    if (n == 0)
        big_requests(NULL);
    for (int i = 0; i < n; i++)
        pthread_join(t[i], NULL);
    pthread_attr_destroy(&attr);
    /* Fewer than two threads reach no unbuilt automatic arena: say so
     * (exit 3), so the parent knows this run covered nothing. */
    if (failures == 0 && n < 2)
        return 3;
    return finish("resource child");
}

static int child_as_min(void)
{
    require_challoc();
    workload();
    return finish("resource child");
}

static void parent_data_init(void)
{
    int rc = run_child("child_data_init", RLIMIT_DATA, 32 * MIB);
    CHECK(rc == 0, "child under RLIMIT_DATA = 32 MiB: %s %d", rc < 0 ? "signal" : "exit",
          rc < 0 ? -rc : rc);
}

static void parent_as_min(void)
{
    /* design 2's minimum layout plus 2 GiB for the program itself. */
    size_t gib = QUANTUM == 8 ? 469 : 437;
    for (int i = 0; i < 16; i++) {
        int rc = run_child("child_as_min", RLIMIT_AS, gib * GIB);
        CHECK(rc == 0, "run %d under RLIMIT_AS = %zu GiB: %s %d", i, gib, rc < 0 ? "signal" : "exit",
              rc < 0 ? -rc : rc);
    }
}

/* Run `n` threads at once: each allocates small, medium and huge blocks
 * only when all exist and frees them only when all have allocated, so
 * every thread is bound to an arena of its own while the others live. */
static pthread_barrier_t start, done;
/* Threads whose explicit MALLOCX_ARENA(home) was refused. */
static int explicit_refused;

static void *gated(void *arg)
{
    (void)arg;
    pthread_barrier_wait(&start);
    void *a = malloc(16), *b = calloc(1, 5000), *c = malloc(1 << 20);
    CHECK(a && b && c, "thread: malloc(16)=%p calloc(5000)=%p malloc(1 MiB)=%p errno %d", a, b, c,
          errno);
    if (b)
        CHECK(is_zero(b, 5000), "calloc not zero");
    /* The mallocx routes on the thread's own (implicit) arena too. */
    void *d = NULL, *e = NULL, *f = NULL;
    if (mallocx) {
        d = mallocx(1 << 20, 0);
        e = mallocx(16, MALLOCX_TCACHE_NONE);
        CHECK(d && e, "thread: mallocx(1 MiB)=%p mallocx(16, TCACHE_NONE)=%p errno %d", d, e, errno);
        /* Named explicitly, the home is served only if it was built:
         * an unbuildable one is NULL/ENOMEM, never arena 0. */
        mallctl_fn ctl = mallctl_dl();
        unsigned home = 0;
        size_t len = sizeof home;
        if (ctl && ctl("thread.arena", &home, &len, NULL, 0) == 0) {
            /* (mallocx reports failure by NULL alone, as jemalloc's.) */
            f = mallocx(16, MALLOCX_ARENA(home));
            if (!f)
                __sync_fetch_and_add(&explicit_refused, 1);
        }
    }
    /* Many size classes: each first use is a slow path, which on a
     * thread served from arena 0 tries its home's build again. */
    mallctl_fn fctl = mallctl_dl();
    for (int it = 0; it < 100; it++) {
        for (size_t sz = 16; sz <= 64 * 1024; sz += sz / 2 + 16) {
            void *q = malloc(sz);
            CHECK(q, "thread: malloc(%zu) errno %d", sz, errno);
            free(q);
        }
        if (fctl)
            fctl("thread.tcache.flush", NULL, NULL, NULL, 0);
    }
    pthread_barrier_wait(&done);
    free(a);
    free(b);
    free(c);
    free(d);
    free(e);
    free(f);
    return NULL;
}

static void run_threads(int n)
{
    pthread_t *t = calloc((size_t)n, sizeof *t);
    REQUIRE(t, "calloc");
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 64 * 1024);
    pthread_barrier_init(&start, NULL, (unsigned)n);
    pthread_barrier_init(&done, NULL, (unsigned)n);
    for (int i = 0; i < n; i++)
        REQUIRE(pthread_create(&t[i], &attr, gated, NULL) == 0, "pthread_create %d", i);
    for (int i = 0; i < n; i++)
        pthread_join(t[i], NULL);
    pthread_barrier_destroy(&start);
    pthread_barrier_destroy(&done);
    pthread_attr_destroy(&attr);
    free(t);
}

static void parent_arenas_create(void)
{
    mallctl_fn ctl = mallctl_dl();
    REQUIRE(ctl, "no mallctl");
    /* The automatic arenas: `arenas.narenas` before any create. */
    unsigned autos = 0, idx;
    size_t len = sizeof autos;
    REQUIRE(ctl("arenas.narenas", &autos, &len, NULL, 0) == 0, "arenas.narenas");
    len = sizeof idx;
    int n = 0, rc;
    while ((rc = ctl("arenas.create", &idx, &len, NULL, 0)) == 0)
        n++;
    CHECK(rc == EAGAIN, "arenas.create refused with %d, not EAGAIN", rc);
    /* The headroom is a handful of builds, not one per automatic arena:
     * explicit arenas still get most of the region (about 429 at
     * quantum 16), whatever narenas is (below the 4096-arena table). */
    CHECK(autos >= 4096 || n >= 300, "only %d explicit arenas with %u automatic ones", n, autos);
    /* The automatic arenas still fit: the last one can be built
     * (mallocx names it explicitly, so no fallback can serve it). */
    if (autos > 1 && mallocx) {
        void *p = mallocx(16, MALLOCX_ARENA(autos - 1));
        CHECK(p, "mallocx(16, MALLOCX_ARENA(%u)): automatic arena not buildable", autos - 1);
        if (p)
            free(p);
    } else {
        skip_note("resource/arenas_create", "one automatic arena (%u): no headroom to check", autos);
    }
    run_threads(32);
}

static void parent_narenas(void)
{
    run_threads(600);
    /* More homes than the metadata region holds: some explicit
     * requests for an unbuilt home must have been refused. */
    CHECK(explicit_refused > 0, "no thread's home was unbuildable (narenas too small for this host?)");
    /* The homes that did not fit stranded no metadata: the huge
     * allocator still has room for its tables. */
    enum { HUGE_N = 20000 };
    static void *held[HUGE_N];
    int got = 0;
    for (int i = 0; i < HUGE_N; i++) {
        held[i] = malloc(300 * 1024);
        got += held[i] != NULL;
    }
    CHECK(got == HUGE_N, "main held %d of %d 300 KB blocks after the fallback threads (errno %d)", got, HUGE_N,
          errno);
    for (int i = 0; i < HUGE_N; i++)
        free(held[i]);
}

static void *thread_limited(void *arg)
{
    (void)arg;
    errno = 0;
    void *a = malloc(16);
    CHECK(a || errno == ENOMEM, "malloc(16) NULL with errno %d", errno);
    errno = 0;
    void *b = malloc(1 << 20);
    CHECK(b || errno == ENOMEM, "malloc(1 MiB) NULL with errno %d", errno);
    free(a);
    free(b);
    return NULL;
}

static void *idle(void *arg)
{
    return arg;
}

/* Holds arena 1 (one bound heap) until released, so the next thread is
 * homed on an arena nobody has built. */
static pthread_mutex_t hold_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t hold_cv = PTHREAD_COND_INITIALIZER;
static int hold_state;

static void *holder(void *arg)
{
    (void)arg;
    void *p = malloc(16);
    CHECK(p, "holder malloc");
    pthread_mutex_lock(&hold_mu);
    hold_state = 1;
    pthread_cond_broadcast(&hold_cv);
    while (hold_state != 2)
        pthread_cond_wait(&hold_cv, &hold_mu);
    pthread_mutex_unlock(&hold_mu);
    free(p);
    return NULL;
}

static void parent_data_thread(void)
{
    /* Two threads' stacks made and cached by glibc first, so the limited
     * pthread_create below needs no new mapping; they allocate nothing,
     * so no arena is built yet. */
    pthread_t h, t;
    REQUIRE(pthread_create(&t, NULL, idle, NULL) == 0, "pthread_create");
    REQUIRE(pthread_create(&h, NULL, idle, NULL) == 0, "pthread_create");
    pthread_join(t, NULL);
    pthread_join(h, NULL);
    REQUIRE(pthread_create(&h, NULL, holder, NULL) == 0, "pthread_create holder");
    pthread_mutex_lock(&hold_mu);
    while (hold_state != 1)
        pthread_cond_wait(&hold_cv, &hold_mu);
    pthread_mutex_unlock(&hold_mu);
    struct rlimit r;
    REQUIRE(getrlimit(RLIMIT_DATA, &r) == 0, "getrlimit");
    set_soft(RLIMIT_DATA, vm_data());
    int rc = pthread_create(&t, NULL, thread_limited, NULL);
    CHECK(rc == 0, "pthread_create under the limit: %s", strerror(rc));
    if (rc == 0)
        pthread_join(t, NULL);
    set_soft(RLIMIT_DATA, r.rlim_max);
    pthread_mutex_lock(&hold_mu);
    hold_state = 2;
    pthread_cond_broadcast(&hold_cv);
    pthread_mutex_unlock(&hold_mu);
    pthread_join(h, NULL);
    /* Lifted: new threads are served again. */
    run_threads(8);
}

__attribute__((weak)) const char *challoc_features(void);

/* Whether classes have guard slabs: the kernel installs guard markers
 * and the configuration keeps a guard interval (`guards:false` or
 * `guard_slab_interval:0` sets it to 0). */
static int guard_slabs(void)
{
    const char *f = challoc_features ? challoc_features() : NULL;
    if (!f || !strstr(f, "guard_install=yes"))
        return 0;
    mallctl_fn ctl = mallctl_dl();
    bool guards = false;
    unsigned interval = 0;
    size_t blen = sizeof guards, ulen = sizeof interval;
    return ctl && ctl("opt.guards", &guards, &blen, NULL, 0) == 0 && guards &&
           ctl("opt.guard_slab_interval", &interval, &ulen, NULL, 0) == 0 && interval != 0;
}

static void parent_mlock_huge(void)
{
    void *p = malloc(1 << 20);
    REQUIRE(p, "malloc(1 MiB)");
    if (mlock(p, malloc_usable_size(p)) != 0) {
        skip_note("resource/mlock_huge", "mlock(1 MiB) refused: %s", strerror(errno));
        free(p);
        return;
    }
    /* Freed while locked: the next huge runs are placed in that range. */
    free(p);
    for (int i = 0; i < 8; i++) {
        errno = 0;
        void *q = malloc(307200);
        CHECK(q, "malloc(307200) #%d over a locked extent: errno %d", i, errno);
        if (q)
            fill(q, 307200, (uint32_t)i);
        free(q);
    }
    /* A larger run over the same extent covers the guard page the
     * smaller ones left PROT_NONE: its pages must be committed. */
    void *big = malloc(700000);
    CHECK(big, "malloc(700000) over a locked extent: errno %d", errno);
    if (big) {
        fill(big, 700000, 9);
        CHECK(check_fill(big, 700000, 9), "700000-byte run changed");
    }
    free(big);
}

static int lock_all(const char *mode, int flags)
{
    if (mlockall(flags) == 0)
        return 1;
    skip_note(mode, "mlockall refused: %s", strerror(errno));
    return 0;
}

static void parent_mlockall(void)
{
    if (!lock_all("resource/mlockall", MCL_CURRENT | MCL_FUTURE))
        return;
    /* Enough 4 KiB blocks to cross guard slabs, and huge runs. */
    enum { N = 2048 };
    static void *small[N];
    for (int i = 0; i < N; i++) {
        small[i] = malloc(4096);
        CHECK(small[i], "malloc(4096) #%d under mlockall: errno %d", i, errno);
        if (small[i])
            fill(small[i], 4096, (uint32_t)i);
    }
    for (int i = 0; i < 8; i++) {
        void *q = malloc(307200);
        CHECK(q, "malloc(307200) #%d under mlockall: errno %d", i, errno);
        if (q)
            fill(q, 307200, (uint32_t)i);
        free(q);
    }
    for (int i = 0; i < N; i++) {
        if (small[i])
            CHECK(check_fill(small[i], 4096, (uint32_t)i), "4 KiB block #%d changed", i);
        free(small[i]);
    }
    munlockall();
}

static size_t count_maps(void)
{
    FILE *f = fopen("/proc/self/maps", "r");
    REQUIRE(f, "open /proc/self/maps");
    size_t n = 0;
    int c;
    while ((c = fgetc(f)) != EOF)
        n += c == '\n';
    fclose(f);
    return n;
}

static void parent_map_count(void)
{
    /* Without guard slabs a slab commit only moves the boundary of the
     * read-write mapping and needs no new one: nothing to exhaust. */
    if (!guard_slabs()) {
        skip_note("resource/map_count", "no guard slabs (guard_install=no or guards:false)");
        return;
    }
    size_t limit = 65530;
    FILE *f = fopen("/proc/sys/vm/max_map_count", "r");
    if (f) {
        if (fscanf(f, "%zu", &limit) != 1)
            limit = 65530;
        fclose(f);
    }
    if (limit > 1000000) {
        skip_note("resource/map_count", "vm.max_map_count %zu too large to fill", limit);
        return;
    }
    size_t cap = limit + 16, n = 0;
    void **fill_maps = calloc(cap, sizeof *fill_maps);
    void **blocks = calloc(8192, sizeof *blocks);
    REQUIRE(fill_maps && blocks, "calloc");
    /* MCL_CURRENT: the class regions' VMAs are locked, so a guard slab
     * gets no marker and needs a VMA split to be PROT_NONE. */
    if (!lock_all("resource/map_count", MCL_CURRENT)) {
        free(blocks);
        free(fill_maps);
        return;
    }
    for (int i = 0; i < 4; i++) {
        blocks[i] = malloc(4096);
        REQUIRE(blocks[i], "malloc(4096) before the fill");
        fill(blocks[i], 4096, (uint32_t)i);
    }
    /* Alternate protections so neighbouring fillers never merge. */
    for (size_t before = count_maps(); n < cap && before + n < limit; n++) {
        void *m = mmap(NULL, 4096, (n & 1) ? PROT_READ : PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (m == MAP_FAILED)
            break;
        fill_maps[n] = m;
    }
    int got = 4, refused = 0;
    for (int i = 4; i < 8192; i++) {
        errno = 0;
        blocks[i] = malloc(4096);
        if (!blocks[i]) {
            CHECK(errno == ENOMEM, "malloc(4096) NULL with errno %d", errno);
            refused = 1;
            break;
        }
        fill(blocks[i], 4096, (uint32_t)i);
        got++;
    }
    CHECK(refused, "the map count never ran out (%d blocks)", got);
    /* The pressure lifts: allocation works again. */
    for (size_t i = 0; i < n; i++)
        munmap(fill_maps[i], 4096);
    for (int i = got; i < 8192 && i < got + 512; i++) {
        blocks[i] = malloc(4096);
        CHECK(blocks[i], "malloc(4096) after the mappings went: errno %d", errno);
        if (blocks[i])
            fill(blocks[i], 4096, (uint32_t)i);
    }
    for (int i = 0; i < 8192; i++) {
        if (blocks[i])
            CHECK(check_fill(blocks[i], 4096, (uint32_t)i), "4 KiB block #%d changed", i);
        free(blocks[i]);
    }
    free(blocks);
    free(fill_maps);
    munlockall();
}

int main(int argc, char **argv)
{
    self_path = "/proc/self/exe";
    const char *mode = argc > 1 ? argv[1] : "";
    if (!strcmp(mode, "child_data_init"))
        return child_data_init();
    if (!strcmp(mode, "child_as_min"))
        return child_as_min();
    if (!strcmp(mode, "child_data_threads"))
        return child_data_threads();
    require_challoc();
    if (!strcmp(mode, "data_init"))
        parent_data_init();
    else if (!strcmp(mode, "as_min"))
        parent_as_min();
    else if (!strcmp(mode, "data_threads")) {
        /* glibc refuses thread stacks under the tighter limits (64 KiB
         * pages raise the floor on aarch64): a run that started fewer
         * than two threads exits 3 and covers nothing, and at least one
         * run must start them. */
        static const size_t limits[] = {3, 4, 8, 16, 32, 64, 128};
        int threaded = 0;
        for (size_t l = 0; l < sizeof limits / sizeof limits[0]; l++)
            for (int i = 0; i < 4; i++) {
                int rc = run_child("child_data_threads", RLIMIT_DATA, limits[l] * MIB);
                CHECK(rc == 0 || rc == 3, "16 threads under RLIMIT_DATA = %zu MiB: %s %d", limits[l],
                      rc < 0 ? "signal" : "exit", rc < 0 ? -rc : rc);
                threaded += rc == 0;
            }
        CHECK(threaded > 0, "no run under a data limit started two threads");
    }
    else if (!strcmp(mode, "arenas_create"))
        parent_arenas_create();
    else if (!strcmp(mode, "narenas"))
        parent_narenas();
    else if (!strcmp(mode, "data_thread"))
        parent_data_thread();
    else if (!strcmp(mode, "mlock_huge"))
        parent_mlock_huge();
    else if (!strcmp(mode, "mlockall"))
        parent_mlockall();
    else if (!strcmp(mode, "map_count"))
        parent_map_count();
    else
        REQUIRE(0, "unknown mode '%s'", mode);
    return finish("resource");
}
