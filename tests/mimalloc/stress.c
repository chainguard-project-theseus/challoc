/* challoc driver for mimalloc's test/test-stress.c (plan Phase 0 task 16).
 *
 * The mimalloc source is included verbatim with USE_STD_MALLOC so every
 * custom_calloc/realloc/free is the libc entry point that LD_PRELOAD
 * routes to challoc, and its main() is renamed so this file can wrap it:
 * refuse to run unless challoc is actually serving malloc (a run without
 * the preload must fail, like every other C test in this repo), run the
 * stress test with the caller's [THREADS] [SCALE] [ITER], and print
 * `ok stress` when it returns 0. NDEBUG is deliberately left undefined so
 * the assert() in alloc_items() checks that calloc really zeroed the block.
 *
 * Compile with -DQUANTUM=<n> -Itests/c -Itests/mimalloc/include. */
#include "common.h"

#define USE_STD_MALLOC 1
#define main test_stress_main
#include "test-stress.c"
#undef main

int main(int argc, char **argv)
{
    require_challoc();
    int r = test_stress_main(argc, argv);
    if (r != 0) {
        fprintf(stderr, "stress: test-stress main returned %d\n", r);
        return r;
    }
    printf("ok stress\n");
    return 0;
}
