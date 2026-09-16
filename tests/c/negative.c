/*
 * Phase 0 task 14: negative tests. Each mode performs one invalid operation
 * that challoc must refuse by aborting (or that the guard page must turn
 * into a fault). The driver in src/api/ctests.rs runs one mode per process
 * and checks the signal and the `challoc: ...` line on stderr. Reaching the
 * end of a mode means the allocator let the bug through; the program then
 * exits 0, which the driver treats as a failure.
 *
 *   negative double_free_stale    free(p) twice with the thread cache flushed
 *                                 in between: caught by the slot bit on free
 *   negative double_free_cached   free(p) twice back to back: caught when the
 *                                 cache flushes the duplicates to the slab
 *   negative interior_free        free(p + size/2)
 *   negative stack_free           free of a stack address
 *   negative huge_overflow        write one byte past a huge allocation
 */
#include "common.h"

/* Enough blocks of one class that freeing them all flushes the thread cache
 * several times (the largest cache holds 256 slots and flushes 128). They
 * are allocated before the victim so a later malloc never pops it. */
#define HOLD 2048
static void *volatile hold[HOLD];

static void *victim(size_t size)
{
    for (int i = 0; i < HOLD; i++) {
        hold[i] = malloc(size);
        REQUIRE(hold[i] != NULL, "malloc(%zu) failed", size);
    }
    void *p = malloc(size);
    REQUIRE(p != NULL, "malloc(%zu) failed", size);
    memset(p, 0xA5, size);
    return p;
}

static void drain(void)
{
    for (int i = 0; i < HOLD; i++) {
        free(hold[i]);
        hold[i] = NULL;
    }
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2, "usage: negative <mode>");
    require_challoc();
    const char *mode = argv[1];
    printf("negative: %s\n", mode);
    fflush(stdout);

    /* Every bad pointer goes through a volatile so gcc neither folds the
     * operation away nor rejects it at compile time (-Wfree-nonheap-object,
     * -Wuse-after-free are errors under -Werror). */
    void *volatile bad;

    if (strcmp(mode, "double_free_stale") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad);
        drain(); /* p leaves the cache and its slot bit clears */
        free(bad);
    } else if (strcmp(mode, "double_free_cached") == 0) {
        void *p = victim(64);
        bad = p;
        free(bad);
        free(bad); /* both copies now sit in the thread cache */
        drain();   /* the flush releases them to the slab */
    } else if (strcmp(mode, "interior_free") == 0) {
        void *p = malloc(64);
        REQUIRE(p != NULL, "malloc failed");
        REQUIRE(malloc_usable_size(p) == 64, "usable %zu", malloc_usable_size(p));
        bad = (char *)p + 32;
        free(bad);
    } else if (strcmp(mode, "stack_free") == 0) {
        char buf[64];
        memset(buf, 0x5A, sizeof buf);
        bad = buf;
        free(bad);
    } else if (strcmp(mode, "huge_overflow") == 0) {
        size_t size = 1u << 20; /* above the largest slab class */
        bad = malloc(size);
        REQUIRE(bad != NULL, "malloc(%zu) failed", size);
        size_t usable = malloc_usable_size(bad);
        REQUIRE(usable >= size, "usable %zu < %zu", usable, size);
        /* Through the volatile so gcc cannot pair the index with the
         * malloc size (-Warray-bounds is an error under -Werror). */
        volatile unsigned char *p = bad;
        p[usable - 1] = 1; /* last byte inside the block is fine */
        p[usable] = 1;     /* first byte of the guard page must fault */
        printf("wrote past the end of %p (usable %zu)\n", bad, usable);
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }

    printf("survived %s\n", mode);
    return 0;
}
