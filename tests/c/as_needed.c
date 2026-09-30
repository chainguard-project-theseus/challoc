/*
 * What `-Wl,--as-needed` does to a consumer of
 * libchalloc (tests/as-needed.sh links this file three ways and reads the
 * DT_NEEDED list of each). The program reports which object serves malloc
 * and, when built with -DREF_VERSION, calls challoc_version(); built with
 * -DREF_NONE it references nothing from any library but libc's puts.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef REF_VERSION
extern const char *challoc_version(void);
#endif
#ifdef VIA_LIB
extern const char *lib_malloc_from(void);
#endif

int main(void)
{
#ifndef REF_NONE
    void *p = malloc(64);
    if (!p)
        return 1;
    memset(p, 1, 64);
    Dl_info info;
    const char *from = "?";
    if (dladdr((void *)&malloc, &info) && info.dli_fname) {
        from = strrchr(info.dli_fname, '/');
        from = from ? from + 1 : info.dli_fname;
    }
    printf("malloc from %s\n", from);
    free(p);
#endif
#ifdef REF_VERSION
    printf("challoc_version %s\n", challoc_version());
#endif
#ifdef VIA_LIB
    printf("library's malloc from %s\n", lib_malloc_from());
#endif
    puts("ok as_needed");
    return 0;
}
