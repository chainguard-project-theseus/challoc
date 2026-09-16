/* challoc stand-in for glibc's <support/test-pointer.h>: only
 * support_address_diff, which tst-realloc.c uses to check that a block
 * did not move. Computed on integers so unrelated pointers are fine. */
#ifndef CHALLOC_SUPPORT_TEST_POINTER_H
#define CHALLOC_SUPPORT_TEST_POINTER_H

#include <stdint.h>

static inline intptr_t support_address_diff(const void *a, const void *b)
{
    return (intptr_t)((uintptr_t)a - (uintptr_t)b);
}

#endif
