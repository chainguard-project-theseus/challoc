/* challoc stand-in for glibc's <support/check.h>: only FAIL_EXIT1, which
 * tst-realloc.c uses. Prints the location and message, exits 1. */
#ifndef CHALLOC_SUPPORT_CHECK_H
#define CHALLOC_SUPPORT_CHECK_H

#include <stdio.h>
#include <stdlib.h>

#define FAIL_EXIT1(...)                                            \
    do {                                                           \
        fprintf(stderr, "error: %s:%d: ", __FILE__, __LINE__);     \
        fprintf(stderr, __VA_ARGS__);                              \
        fputc('\n', stderr);                                       \
        exit(1);                                                   \
    } while (0)

#endif
