/*
 * The allocator's entry points on a thread with the smallest stack glibc
 * allows (PTHREAD_STACK_MIN, 16 KiB on x86_64), after the thread has
 * already used HEADROOM bytes of it for its own frames: a correct program
 * that works under glibc's malloc and jemalloc must not die of a stack
 * overflow inside the allocator. The frames are bounded where they are:
 * the deferred cgroup discovery (which the tick of any `malloc`, `fork`'s
 * prepare handler or a read of the figures may run) builds its record in
 * place with static scan buffers, and an arena's build writes its
 * statistics record in place (design 13.8).
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
 * `fork`; challoc leaves 8 on every row here, so 6 keeps a margin for
 * other builds and compilers. */
#define HEADROOM 6144

extern void *_Znwm(size_t) WEAK;
extern void _ZdlPv(void *) WEAK;
extern void _ZdlPvm(void *, size_t) WEAK;
extern void *_ZnwmSt11align_val_t(size_t, size_t) WEAK;
extern void _ZdlPvSt11align_val_t(void *, size_t) WEAK;
extern int je_mallctl(const char *, void *, size_t *, void *, size_t) WEAK;
extern void je_malloc_stats_print(void (*)(void *, const char *), void *, const char *) WEAK;
extern int get_defrag_hint(void *) WEAK;

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

/* The case a fork from an SA_ONSTACK handler must not break: the handler
 * forks on the alternate stack, and a second SA_ONSTACK signal raised
 * from it (pending until it returns) finds the first handler's frames
 * intact (its canary). */
static volatile sig_atomic_t alt_ok;

static void on_usr2(int sig)
{
    (void)sig;
}

static void on_usr1(int sig)
{
    (void)sig;
    volatile unsigned long canary = 0x5a5a5a5a5a5a5a5aUL;
    pid_t pid = fork();
    if (pid == 0) {
        free(malloc(100));
        _exit(0);
    }
    raise(SIGUSR2);
    int st = 0;
    while (pid > 0 && waitpid(pid, &st, 0) != pid && errno == EINTR)
        ;
    alt_ok = canary == 0x5a5a5a5a5a5a5a5aUL && pid > 0 && WIFEXITED(st) && WEXITSTATUS(st) == 0;
}

static void op_fork_altstack(void)
{
    static char alt[65536];
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt};
    REQUIRE(sigaltstack(&ss, NULL) == 0, "sigaltstack");
    struct sigaction sa = {.sa_handler = on_usr1, .sa_flags = SA_ONSTACK};
    sigemptyset(&sa.sa_mask);
    REQUIRE(sigaction(SIGUSR1, &sa, NULL) == 0, "sigaction");
    sa.sa_handler = on_usr2;
    REQUIRE(sigaction(SIGUSR2, &sa, NULL) == 0, "sigaction");
    for (int i = 0; i < 3; i++) {
        alt_ok = 0;
        raise(SIGUSR1);
        CHECK(alt_ok, "fork from an SA_ONSTACK handler, round %d", i);
    }
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
    {"cgroup", op_cgroup, false},
    {"fork", op_fork, false},
    {"fork_warm", op_fork, true},
    {"fork_altstack", op_fork_altstack, true},
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
