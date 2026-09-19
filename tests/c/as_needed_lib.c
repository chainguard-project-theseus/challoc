/*
 * Phase 3 prerequisite (e), the two-level shape (Ruby: the `ruby` binary
 * calls no allocator function itself, libruby.so does): a shared library
 * linked -lchalloc that allocates and reports which object serves malloc.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

const char *lib_malloc_from(void)
{
    void *p = malloc(64);
    if (!p)
        return "malloc failed";
    memset(p, 1, 64);
    free(p);
    Dl_info info;
    if (!dladdr((void *)&malloc, &info) || !info.dli_fname)
        return "?";
    const char *from = strrchr(info.dli_fname, '/');
    return from ? from + 1 : info.dli_fname;
}
