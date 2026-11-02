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
 *               served (an unbuildable home falls back to arena 0)
 *   data_thread RLIMIT_DATA cut to VmData, then a new thread whose arena
 *               cannot be built allocates 16 B and 1 MiB: NULL/ENOMEM or
 *               a block, never an abort; after the limit is raised a new
 *               thread allocates normally
 */
#include "common.h"

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

static void *gated(void *arg)
{
    (void)arg;
    pthread_barrier_wait(&start);
    void *a = malloc(16), *b = calloc(1, 5000), *c = malloc(1 << 20);
    CHECK(a && b && c, "thread: malloc(16)=%p calloc(5000)=%p malloc(1 MiB)=%p errno %d", a, b, c,
          errno);
    if (b)
        CHECK(is_zero(b, 5000), "calloc not zero");
    pthread_barrier_wait(&done);
    free(a);
    free(b);
    free(c);
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
    unsigned idx;
    size_t len = sizeof idx;
    int n = 0, rc;
    while ((rc = ctl("arenas.create", &idx, &len, NULL, 0)) == 0)
        n++;
    CHECK(rc == EAGAIN, "arenas.create refused with %d, not EAGAIN", rc);
    CHECK(n > 0, "no explicit arena created");
    /* The automatic arenas still fit: the last one can be built (mallocx
     * names it explicitly, so no fallback can serve it). */
    unsigned narenas = 0;
    len = sizeof narenas;
    REQUIRE(ctl("opt.narenas", &narenas, &len, NULL, 0) == 0, "opt.narenas");
    if (narenas > 1 && mallocx) {
        void *p = mallocx(16, MALLOCX_ARENA(narenas - 1));
        CHECK(p, "mallocx(16, MALLOCX_ARENA(%u)): automatic arena not buildable", narenas - 1);
        if (p)
            free(p);
    }
    run_threads(32);
}

static void parent_narenas(void)
{
    run_threads(600);
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

int main(int argc, char **argv)
{
    self_path = "/proc/self/exe";
    const char *mode = argc > 1 ? argv[1] : "";
    if (!strcmp(mode, "child_data_init"))
        return child_data_init();
    if (!strcmp(mode, "child_as_min"))
        return child_as_min();
    require_challoc();
    if (!strcmp(mode, "data_init"))
        parent_data_init();
    else if (!strcmp(mode, "as_min"))
        parent_as_min();
    else if (!strcmp(mode, "arenas_create"))
        parent_arenas_create();
    else if (!strcmp(mode, "narenas"))
        parent_narenas();
    else if (!strcmp(mode, "data_thread"))
        parent_data_thread();
    else
        REQUIRE(0, "unknown mode '%s'", mode);
    return finish("resource");
}
