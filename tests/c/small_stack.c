/*
 * The allocator's entry points on a thread with the smallest stack glibc
 * allows (PTHREAD_STACK_MIN, 16 KiB on x86_64), after the thread has
 * already used HEADROOM bytes of it for its own frames: a correct program
 * that works under glibc's malloc and jemalloc must not die of a stack
 * overflow inside the allocator. The frames are bounded where they are:
 * the deferred cgroup discovery (which the tick of any `malloc`, `fork`'s
 * prepare handler or a read of the figures may run) builds its record in
 * place with static scan buffers, an arena's build writes its
 * statistics record in place, and the statistics entry points collect
 * into scratch records in the metadata (design 13.8).
 *
 * The driver (no argument) runs every operation of the table in a fresh
 * process (this program re-executed with the operation's name), so each
 * meets the allocator in the state its row names: `cold` right after init
 * (the cgroup not yet discovered, the thread's arena not yet built),
 * `warm` after the discovery's deferral has run out with nothing having
 * ticked (the operation's own tick, or `fork`'s prepare handler, runs the
 * discovery). The operation runs on a new PTHREAD_STACK_MIN thread (or
 * the larger stack its row names); a child that dies of a signal fails
 * the row.
 */
#include "common.h"
#include <fcntl.h>
#include <limits.h>
#include <setjmp.h>

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

static void discard(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
}

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

/* Many threads, each on a PTHREAD_STACK_MIN stack with CONTEND_USED of
 * it used (HEADROOM less the rounds' own frames), reading the statistics
 * at once while they allocate: each call takes its own scratch records
 * (design 13.8), so none waits on or overwrites another's. */
#define CONTENDERS 8
#define CONTEND_USED (HEADROOM - 2048)

static void contend_round(int i)
{
    void *p[8];
    for (int k = 0; k < 8; k++)
        p[k] = malloc(16 + (size_t)((i * 8 + k) % 97) * 40);
    switch (i % 4) {
    case 0:
        op_stats_print();
        break;
    case 1:
        op_mallctl();
        break;
    case 2:
        op_glibc();
        op_malloc_info();
        break;
    default:
        malloc_stats();
        break;
    }
    for (int k = 0; k < 8; k++)
        free(p[k]);
}

__attribute__((noinline)) static void contend_deep(int i)
{
    volatile char used[CONTEND_USED];
    for (size_t k = 0; k < sizeof used; k += 64)
        used[k] = (char)k;
    for (int round = 0; round < 20; round++)
        contend_round(i + round);
    __asm__ volatile("" ::"r"(used) : "memory");
}

static void *contender(void *arg)
{
    contend_deep((int)(intptr_t)arg);
    return NULL;
}

static void op_stats_contend(void)
{
    pthread_attr_t at;
    REQUIRE(pthread_attr_init(&at) == 0, "attr");
    REQUIRE(pthread_attr_setstacksize(&at, PTHREAD_STACK_MIN) == 0, "setstacksize");
    /* malloc_stats writes to stderr: /dev/null for the whole row. */
    int saved = dup(2), nul = open("/dev/null", O_WRONLY);
    REQUIRE(saved >= 0 && nul >= 0, "dup");
    dup2(nul, 2);
    pthread_t th[CONTENDERS];
    for (int i = 0; i < CONTENDERS; i++)
        REQUIRE(pthread_create(&th[i], &at, contender, (void *)(intptr_t)i) == 0, "pthread_create");
    for (int i = 0; i < CONTENDERS; i++)
        REQUIRE(pthread_join(th[i], NULL) == 0, "pthread_join");
    dup2(saved, 2);
    close(nul);
    close(saved);
}

/* A malloc_stats_print callback that needs 512 KiB of stack, on a thread
 * that has it: the callback runs on the caller's stack, as under
 * jemalloc, so the report must not move to a stack of its own. */
#define BIG_CALLBACK (512 * 1024)

static volatile size_t big_calls;

__attribute__((noinline)) static void big_callback(void *opaque, const char *s)
{
    (void)opaque;
    volatile char frame[BIG_CALLBACK];
    frame[0] = s[0];
    frame[sizeof frame - 1] = s[0];
    for (size_t k = 0; k < sizeof frame; k += 4096)
        frame[k] = (char)k;
    __asm__ volatile("" ::"r"(frame) : "memory");
    big_calls++;
}

static void op_big_callback(void)
{
    big_calls = 0;
    malloc_stats_print(big_callback, NULL, NULL);
    malloc_stats_print(big_callback, NULL, "J");
    CHECK(big_calls > 2, "big callback ran %zu times", (size_t)big_calls);
}

/* A signal handler raised from inside a report's callback that reads the
 * statistics itself: no allocator lock is held across a callback, so
 * the nested reports neither deadlock nor see the outer one's records.
 * On a 64 KiB stack: the row checks re-entrancy, and the signal frame
 * alone (with the vector state) is several KiB. */
static volatile size_t nested_calls, handled;
static volatile sig_atomic_t in_handler;

static void on_stats_signal(int sig)
{
    (void)sig;
    in_handler = 1;
    struct mallinfo2 m = mallinfo2();
    keep = (void *)(uintptr_t)m.uordblks;
    size_t v = 0, vlen = sizeof v;
    CHECK(mallctl("stats.arenas.0.pactive", &v, &vlen, NULL, 0) == 0, "pactive in handler");
    malloc_stats_print(discard, NULL, "gb");
    handled++;
    in_handler = 0;
}

static void raising_callback(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
    if (!in_handler && nested_calls++ % 8 == 0)
        raise(SIGUSR1);
}

static void op_signal_in_callback(void)
{
    struct sigaction sa = {.sa_handler = on_stats_signal};
    sigemptyset(&sa.sa_mask);
    REQUIRE(sigaction(SIGUSR1, &sa, NULL) == 0, "sigaction");
    nested_calls = handled = 0;
    malloc_stats_print(raising_callback, NULL, "b");
    CHECK(nested_calls > 1 && handled > 0, "callback ran %zu times, the handler %zu", (size_t)nested_calls,
          (size_t)handled);
}

/* A report whose callback never returns: it `longjmp`s out of
 * malloc_stats_print, or its thread exits from it (`pthread_exit`). The
 * scratch records the abandoned call held go back to the pool (design
 * 13.8): the metadata does not grow with the number of such calls. */
static jmp_buf escape;

static void jump_out(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
    longjmp(escape, 1);
}

static size_t metadata_now(void)
{
    uint64_t e = 1;
    size_t elen = sizeof e, m = 0, mlen = sizeof m;
    CHECK(mallctl("epoch", &e, &elen, &e, sizeof e) == 0, "epoch");
    CHECK(mallctl("stats.metadata", &m, &mlen, NULL, 0) == 0, "stats.metadata");
    return m;
}

static void op_stats_longjmp(void)
{
    size_t before = metadata_now();
    for (int i = 0; i < 20000; i++) {
        if (setjmp(escape) == 0)
            malloc_stats_print(jump_out, NULL, i % 2 ? "J" : "g");
    }
    size_t after = metadata_now();
    CHECK(after < before + (1u << 20), "metadata grew from %zu to %zu over 20000 abandoned reports", before, after);
}

static void exit_out(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
    pthread_exit(NULL);
}

static void *report_and_exit(void *arg)
{
    (void)arg;
    malloc_stats_print(exit_out, NULL, "g");
    return NULL;
}

static void op_stats_thread_exit(void)
{
    size_t before = metadata_now();
    for (int i = 0; i < 300; i++) {
        pthread_attr_t at;
        REQUIRE(pthread_attr_init(&at) == 0, "attr");
        REQUIRE(pthread_attr_setstacksize(&at, PTHREAD_STACK_MIN) == 0, "setstacksize");
        pthread_t th;
        REQUIRE(pthread_create(&th, &at, report_and_exit, NULL) == 0, "pthread_create");
        REQUIRE(pthread_join(th, NULL) == 0, "pthread_join");
    }
    size_t after = metadata_now();
    CHECK(after < before + (1u << 20), "metadata grew from %zu to %zu over 300 threads exiting in a report", before, after);
}

/* The pool full: PARKED threads each hold an object, parked inside a
 * report's callback, while this thread abandons reports by longjmp from
 * progressively deeper on its stack (so its own abandoned objects are
 * never above its next call). No object is allocated outside the pool:
 * past it a report says "(no memory)", and the metadata stays within the
 * pool's bound. While every slot is held, each entry point's fallback is
 * checked too: mallctl's epoch write and an arena read are EAGAIN,
 * mallinfo2 is all zeros, malloc_info and malloc_stats say "(no memory)".
 * (Not covered here: a reserved slot whose metadata allocation fails,
 * `pool_unreserve`; starving the metadata region from a test would take
 * every other allocation with it.) */
#define PARKED 64

static pthread_barrier_t parked_in;
static volatile int unpark;

static void park(void *opaque, const char *s)
{
    (void)s;
    int *first = opaque;
    if (*first) {
        *first = 0;
        pthread_barrier_wait(&parked_in);
        while (!unpark) {
            struct timespec t = {0, 1000 * 1000};
            nanosleep(&t, NULL);
        }
    }
}

static void *parked_report(void *arg)
{
    (void)arg;
    int first = 1;
    malloc_stats_print(park, &first, "g");
    return NULL;
}

static volatile size_t no_memory_lines;

static void jump_or_count(void *opaque, const char *s)
{
    (void)opaque;
    if (strstr(s, "(no memory)"))
        no_memory_lines++;
    longjmp(escape, 1);
}

__attribute__((noinline)) static void abandon_at(int depth)
{
    volatile char pad[256];
    pad[0] = (char)depth;
    if (depth > 0) {
        abandon_at(depth - 1);
    } else if (setjmp(escape) == 0) {
        malloc_stats_print(jump_or_count, NULL, "g");
    }
    __asm__ volatile("" ::"r"(pad) : "memory");
}

/* What `fn` writes to a FILE, in a buffer the caller frees. */
static char *captured(void (*fn)(FILE *))
{
    char *buf = NULL;
    size_t len = 0;
    FILE *f = open_memstream(&buf, &len);
    REQUIRE(f, "open_memstream");
    fn(f);
    fclose(f);
    return buf;
}

static void info_to(FILE *f)
{
    CHECK(malloc_info(0, f) == 0, "malloc_info");
}

/* malloc_stats writes to stderr: fd 2 goes to a temporary file. */
static void stats_to(FILE *f)
{
    fflush(stderr);
    int saved = dup(2);
    FILE *tmp = tmpfile();
    REQUIRE(saved >= 0 && tmp, "dup/tmpfile");
    dup2(fileno(tmp), 2);
    malloc_stats();
    fflush(stderr);
    dup2(saved, 2);
    close(saved);
    rewind(tmp);
    char line[512];
    while (fgets(line, sizeof line, tmp))
        fputs(line, f);
    fclose(tmp);
}

static void check_fallbacks(void)
{
    uint64_t e = 1;
    size_t elen = sizeof e;
    CHECK(mallctl("epoch", &e, &elen, &e, sizeof e) == EAGAIN, "epoch with the pool full is not EAGAIN");
    size_t v = 0, vlen = sizeof v;
    CHECK(mallctl("stats.arenas.0.pactive", &v, &vlen, NULL, 0) == EAGAIN,
          "stats.arenas.0.pactive with the pool full is not EAGAIN");
    struct mallinfo2 m = mallinfo2();
    CHECK(m.arena == 0 && m.ordblks == 0 && m.hblks == 0 && m.hblkhd == 0 && m.uordblks == 0 && m.fordblks == 0 &&
              m.keepcost == 0,
          "mallinfo2 with the pool full is not zero");
    char *info = captured(info_to);
    CHECK(info && strstr(info, "(no memory)"), "malloc_info with the pool full: %s", info ? info : "(null)");
    free(info);
    char *st = captured(stats_to);
    CHECK(st && strstr(st, "(no memory)"), "malloc_stats with the pool full: %s", st ? st : "(null)");
    free(st);
}

static void op_stats_pool_full(void)
{
    size_t before = metadata_now();
    REQUIRE(pthread_barrier_init(&parked_in, NULL, PARKED + 1) == 0, "barrier");
    pthread_t th[PARKED];
    unpark = 0;
    for (int i = 0; i < PARKED; i++)
        REQUIRE(pthread_create(&th[i], NULL, parked_report, NULL) == 0, "pthread_create");
    pthread_barrier_wait(&parked_in);
    for (int i = 0; i < 5000; i++)
        abandon_at(i % 40);
    /* From progressively deeper: each abandons the object at a lower mark. */
    for (int i = 0; i < 2000; i++)
        abandon_at(i % 200);
    check_fallbacks();
    unpark = 1;
    for (int i = 0; i < PARKED; i++)
        REQUIRE(pthread_join(th[i], NULL) == 0, "pthread_join");
    size_t after = metadata_now();
    CHECK(no_memory_lines > 0, "no report found the pool full");
    CHECK(after < before + PARKED * 13 * 1024 + (1u << 20), "metadata grew from %zu to %zu with the pool full", before,
          after);
}

/* A fork from inside a report's callback: the child's copy of the report
 * goes on with the scratch object the forking thread held, which the
 * child holds under its own thread ID (design 13.8), and a report the
 * child makes from inside that callback (a nested report: the row has a
 * 64 KiB stack), and one after it, complete. */
static volatile int forked_in_callback;
static volatile size_t child_lines;

static void count_line(void *opaque, const char *s)
{
    (void)opaque;
    (void)s;
    child_lines++;
}

static void fork_once(void *opaque, const char *s)
{
    (void)s;
    pid_t *child = opaque;
    if (forked_in_callback)
        return;
    forked_in_callback = 1;
    *child = fork();
    REQUIRE(*child >= 0, "fork: %s", strerror(errno));
    if (*child == 0) {
        child_lines = 0;
        malloc_stats_print(count_line, NULL, "g");
        if (child_lines == 0)
            _exit(3);
    }
}

static void op_stats_fork_in_callback(void)
{
    pid_t child = -1;
    forked_in_callback = 0;
    malloc_stats_print(fork_once, &child, "g");
    if (child == 0) {
        child_lines = 0;
        malloc_stats_print(count_line, NULL, "J");
        _exit(child_lines > 0 ? 0 : 4);
    }
    REQUIRE(child > 0, "the callback did not fork");
    int st;
    REQUIRE(waitpid(child, &st, 0) == child, "waitpid");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "child status %#x", st);
}

/* The first allocation call of `main` (main has not allocated). Not
 * challoc's init: the library initialises before `main` (ld.so's and
 * libc's own allocations), so init never runs on a thread like this one;
 * every_frame_fits_the_budget caps its frame instead. */
static void op_first_call(void)
{
    require_challoc();
    op_malloc_family();
}

static const struct row {
    const char *name;
    void (*fn)(void);
    bool warm;
    /* The thread's stack: at least PTHREAD_STACK_MIN (also when 0). */
    size_t stack;
} rows[] = {
    {"first_call", op_first_call, false, 0},
    {"malloc_family", op_malloc_family, false, 0},
    {"many", op_many, false, 0},
    {"many_warm", op_many, true, 0},
    {"huge", op_huge, false, 0},
    {"jemalloc", op_jemalloc, false, 0},
    {"cxx", op_cxx, false, 0},
    {"cgroup", op_cgroup, false, 0},
    {"fork", op_fork, false, 0},
    {"fork_warm", op_fork, true, 0},
    {"fork_altstack", op_fork_altstack, true, 0},
    {"glibc", op_glibc, false, 0},
    {"malloc_info", op_malloc_info, false, 0},
    {"malloc_stats", op_malloc_stats, false, 0},
    {"stats_print", op_stats_print, false, 0},
    {"stats_print_warm", op_stats_print, true, 0},
    {"mallctl", op_mallctl, false, 0},
    {"stats_contend", op_stats_contend, false, 0},
    {"signal_in_callback", op_signal_in_callback, false, 64 * 1024},
    {"big_callback", op_big_callback, false, 2 * BIG_CALLBACK},
    {"stats_longjmp", op_stats_longjmp, false, 0},
    {"stats_thread_exit", op_stats_thread_exit, false, 0},
    {"stats_pool_full", op_stats_pool_full, false, 1 << 20},
    {"stats_fork_in_callback", op_stats_fork_in_callback, false, 64 * 1024},
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
    /* A row's own size, at least PTHREAD_STACK_MIN (128 KiB on aarch64). */
    size_t stack = r->stack > (size_t)PTHREAD_STACK_MIN ? r->stack : (size_t)PTHREAD_STACK_MIN;
    REQUIRE(pthread_attr_setstacksize(&at, stack) == 0, "setstacksize");
    /* A deadlock fails the row (SIGALRM) rather than hang the driver. */
    alarm(60);
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
    if (argc > 1 && strcmp(argv[1], "first_call") != 0)
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
