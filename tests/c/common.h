/*
 * Shared scaffolding for the C test programs in tests/c/. Each program is compiled by
 * scripts/ctest-cc.sh (from the Rust driver in src/api/c_programs.rs or one of
 * the shell drivers) with -DQUANTUM=16 or 8, read from the built library,
 * and run under LD_PRELOAD=libchalloc.so. Every program checks its own
 * invariants, prints "ok <name>" on success (`finish`) and exits non-zero
 * on failure. `require_challoc` makes a program fail fast when the preload
 * did not take effect so a broken preload cannot pass by accident.
 *
 * A program that cannot check its main claim on this host (a kernel
 * without the feature, a container without the cgroup mount) still passes
 * but prints one `SKIP <name> <reason>` line (`skip_note`); the Rust
 * driver fails the test on that line unless CHALLOC_TEST_ALLOW_SKIP is set
 * (docs/testing.md, "Skips").
 */
#ifndef CHALLOC_TESTS_COMMON_H
#define CHALLOC_TESTS_COMMON_H

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <malloc.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef QUANTUM
#error "compile with -DQUANTUM=16 or -DQUANTUM=8 (scripts/ctest-cc.sh does)"
#endif

static int failures;

/* Results gcc must not fold away: at -O2 it assumes malloc succeeds and
 * removes malloc/free pairs whose block is never used. */
static void *volatile sink;

#define CHECK(cond, ...)                                                   \
    do {                                                                   \
        if (!(cond)) {                                                     \
            __sync_fetch_and_add(&failures, 1);                            \
            fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__);           \
            fprintf(stderr, __VA_ARGS__);                                  \
            fputc('\n', stderr);                                           \
        }                                                                  \
    } while (0)

/* Abort the run when a failure would make what follows meaningless. */
#define REQUIRE(cond, ...)                                                 \
    do {                                                                   \
        if (!(cond)) {                                                     \
            fprintf(stderr, "FATAL %s:%d: ", __FILE__, __LINE__);          \
            fprintf(stderr, __VA_ARGS__);                                  \
            fputc('\n', stderr);                                           \
            exit(1);                                                       \
        }                                                                  \
    } while (0)

/* The pass-with-a-note line: `SKIP <name> <reason>` on stdout, one line,
 * so `grep '^SKIP '` over a test log lists every claim that went
 * unchecked on this host. `name` is the program, or `program/mode`. */
static inline void skip_note(const char *name, const char *fmt, ...)
{
    va_list ap;
    printf("SKIP %s ", name);
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    putchar('\n');
    fflush(stdout);
}

/*
 * The non-standard entry points, every one weak so a program links against
 * nothing but libc: bare, `require_challoc` fails before any is called;
 * under the preload the references resolve to libchalloc.so. `malloc_conf`
 * and `je_malloc_conf` are not here: tests/c/conf.c defines them, and a
 * weak declaration before a definition would make the definition weak.
 */
#define WEAK __attribute__((weak))
extern void *mallocx(size_t, int) WEAK;
extern void *rallocx(void *, size_t, int) WEAK;
extern size_t xallocx(void *, size_t, size_t, int) WEAK;
extern size_t sallocx(const void *, int) WEAK;
extern void dallocx(void *, int) WEAK;
extern void sdallocx(void *, size_t, int) WEAK;
extern size_t nallocx(size_t, int) WEAK;
extern int mallctl(const char *, void *, size_t *, void *, size_t) WEAK;
extern int mallctlnametomib(const char *, size_t *, size_t *) WEAK;
extern int mallctlbymib(const size_t *, size_t, void *, size_t *, void *, size_t) WEAK;
extern void malloc_stats_print(void (*)(void *, const char *), void *, const char *) WEAK;
extern void (*malloc_message)(void *, const char *) WEAK;
extern const char *challoc_version(void) WEAK;
extern const char *challoc_features(void) WEAK;
/* C23 names; glibc's headers may or may not declare them yet. */
extern void free_sized(void *, size_t) WEAK;
extern void free_aligned_sized(void *, size_t, size_t) WEAK;
/* challoc's size-returning entry points (design 13.7), weak here for the
 * negative modes; tests/c/usable_api.c includes <challoc/challoc.h>. */
extern void *challoc_malloc_usable(size_t, size_t *) WEAK;
extern void *challoc_calloc_usable(size_t, size_t, size_t *) WEAK;
extern void *challoc_realloc_usable(void *, size_t, size_t *) WEAK;
extern size_t challoc_free_usable(void *) WEAK;

/* jemalloc.h's flag encodings. */
#define MALLOCX_LG_ALIGN(la) ((int)(la))
#define MALLOCX_ZERO ((int)0x40)
#define MALLOCX_TCACHE(tc) ((int)(((tc) + 2) << 8))
#define MALLOCX_TCACHE_NONE MALLOCX_TCACHE(-1)
#define MALLOCX_ARENA(a) ((((int)(a)) + 1) << 20)
#define MALLCTL_ARENAS_ALL 4096

/* `mallctl` looked up at run time (`dlsym`), for the programs whose point
 * is that the binary carries no reference to the allocator at all
 * (tests/c/tls_threads.c); NULL when nothing exports it. */
typedef int (*mallctl_fn)(const char *, void *, size_t *, void *, size_t);
static inline mallctl_fn mallctl_dl(void)
{
    return (mallctl_fn)dlsym(RTLD_DEFAULT, "mallctl");
}

/* Fill `n` bytes with a pattern derived from `seed`; `check_fill` verifies
 * it. The pattern depends on the offset so a block shifted by one byte or a
 * block overwritten by a neighbour's pattern is caught. */
static inline void fill(void *p, size_t n, uint32_t seed)
{
    unsigned char *b = p;
    for (size_t i = 0; i < n; i++)
        b[i] = (unsigned char)(seed * 31u + i * 7u + (i >> 8));
}

static inline int check_fill(const void *p, size_t n, uint32_t seed)
{
    const unsigned char *b = p;
    for (size_t i = 0; i < n; i++)
        if (b[i] != (unsigned char)(seed * 31u + i * 7u + (i >> 8)))
            return 0;
    return 1;
}

static inline int is_zero(const void *p, size_t n)
{
    const unsigned char *b = p;
    for (size_t i = 0; i < n; i++)
        if (b[i])
            return 0;
    return 1;
}

/* xorshift64*: small, seedable, good enough for allocation patterns. */
static inline uint64_t rng_next(uint64_t *s)
{
    uint64_t x = *s;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *s = x;
    return x * 0x2545F4914F6CDD1DULL;
}

/* Sleep, restarted across signals. */
static inline void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR)
        ;
}

/* CLOCK_MONOTONIC in milliseconds. */
static inline long now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

/* Resident set of this process from /proc/self/statm, in bytes. */
static inline size_t rss_bytes(void)
{
    FILE *f = fopen("/proc/self/statm", "r");
    REQUIRE(f != NULL, "open /proc/self/statm: %s", strerror(errno));
    unsigned long size, resident;
    REQUIRE(fscanf(f, "%lu %lu", &size, &resident) == 2, "parse statm");
    fclose(f);
    return (size_t)resident * (size_t)sysconf(_SC_PAGESIZE);
}

/* Run `child_fn(arg)` in a forked child that `_exit`s with its return
 * value, wait for it, and return the child's exit status, or -signal when
 * it died of one. The child never returns into the caller's code. */
static inline int fork_and_wait(int (*child_fn)(void *), void *arg)
{
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0)
        _exit(child_fn(arg) & 0xff);
    int status = 0;
    while (waitpid(pid, &status, 0) != pid)
        REQUIRE(errno == EINTR, "waitpid: %s", strerror(errno));
    if (WIFEXITED(status))
        return WEXITSTATUS(status);
    if (WIFSIGNALED(status))
        return -WTERMSIG(status);
    return -1;
}

/* Run `fn(arg)` in a forked child and return 1 iff the child died of
 * `sig` (the allocator's abort is SIGABRT, a guard page SIGSEGV); a child
 * that survives, exits or dies of another signal returns 0. */
static inline int expect_signal(int sig, void (*fn)(void *), void *arg)
{
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        fn(arg);
        _exit(0);
    }
    int status = 0;
    while (waitpid(pid, &status, 0) != pid)
        REQUIRE(errno == EINTR, "waitpid: %s", strerror(errno));
    return WIFSIGNALED(status) && WTERMSIG(status) == sig;
}

/* Exits 1 unless challoc is really serving malloc. malloc(1) on glibc has
 * usable size 24 (x86_64) or so, never exactly QUANTUM, and glibc does not
 * export challoc_version. */
static inline void require_challoc(void)
{
    void *probe = malloc(1);
    size_t u = malloc_usable_size(probe);
    if (u != QUANTUM || dlsym(RTLD_DEFAULT, "challoc_version") == NULL) {
        fprintf(stderr, "not running under challoc (usable(1) = %zu)\n", u);
        exit(1);
    }
    free(probe);
}

static inline int finish(const char *name)
{
    if (failures) {
        fprintf(stderr, "%s: %d failures\n", name, failures);
        return 1;
    }
    printf("ok %s\n", name);
    return 0;
}

#endif
