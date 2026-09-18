/*
 * Shared scaffolding for the Phase 0 task 12 programs in tests/c/. Each
 * program is compiled by the Rust test in src/api/ctests.rs with
 * -DQUANTUM=16 or 8 and run under LD_PRELOAD=libchalloc.so. Every program
 * checks its own invariants, prints "ok <name>" on success and exits
 * non-zero on failure. `require_challoc` makes a program fail fast when the
 * preload did not take effect so a broken preload cannot pass by accident.
 */
#ifndef CHALLOC_TESTS_COMMON_H
#define CHALLOC_TESTS_COMMON_H

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <malloc.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#ifndef QUANTUM
#error "compile with -DQUANTUM=16 or -DQUANTUM=8"
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
