/*
 * challoc linked statically: the program is linked against
 * target/release/libchalloc.a (Cargo's `staticlib`) and nothing else of
 * challoc's, by the `api::` test `static_link_serves_every_malloc` (#58).
 * Run bare. It checks:
 *  - no libchalloc.so is loaded (the archive, not a stray shared library,
 *    serves the calls);
 *  - the program's malloc is the one the dynamic linker resolves for
 *    everyone (`dlsym(RTLD_DEFAULT, "malloc")`), so libc's own
 *    allocations (`strdup`) come from challoc as well: challoc's
 *    `malloc_usable_size` aborts on a pointer it did not allocate;
 *  - threads, a huge allocation and `calloc` work.
 * It does not include common.h: that header declares challoc's extensions
 * weak, and a weak undefined reference does not pull an archive member.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <link.h>
#include <malloc.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const char *challoc_version(void);

static int failures;

static void fail(const char *what)
{
    fprintf(stderr, "FAIL static_link: %s\n", what);
    failures++;
}

static int saw_shared;

static int each_object(struct dl_phdr_info *info, size_t size, void *arg)
{
    (void)size;
    (void)arg;
    if (info->dlpi_name && strstr(info->dlpi_name, "libchalloc"))
        saw_shared = 1;
    return 0;
}

/* Returns NULL after a clean run, or the size whose malloc failed (never
 * 0: sizes start at 1). */
static void *worker(void *arg)
{
    (void)arg;
    for (unsigned i = 0; i < 20000; i++) {
        size_t n = (i * 37u) % 4096 + 1;
        unsigned char *p = malloc(n);
        if (!p)
            return (void *)n;
        memset(p, (int)i, n);
        free(p);
    }
    return NULL;
}

int main(void)
{
    const char *v = challoc_version();
    if (!v || !*v)
        fail("challoc_version is empty");

    dl_iterate_phdr(each_object, NULL);
    if (saw_shared)
        fail("a libchalloc shared object is loaded");

    void *global = dlsym(RTLD_DEFAULT, "malloc");
    if (global != (void *)malloc)
        fail("dlsym(RTLD_DEFAULT, \"malloc\") is not the program's malloc");

    /* libc allocates through the global malloc; a foreign pointer would
     * abort in challoc's malloc_usable_size. */
    char *dup = strdup("static");
    if (!dup || malloc_usable_size(dup) < 7)
        fail("strdup's block is not challoc's");
    free(dup);

    pthread_t t[4];
    int started = 0;
    while (started < 4 && pthread_create(&t[started], NULL, worker, NULL) == 0)
        started++;
    if (started < 4)
        fail("pthread_create");
    for (int i = 0; i < started; i++) {
        void *r = NULL;
        pthread_join(t[i], &r);
        if (r) {
            fprintf(stderr, "worker %d: malloc(%zu) returned NULL\n", i, (size_t)r);
            fail("a worker's malloc returned NULL");
        }
    }

    size_t big = (size_t)8 << 20;
    unsigned char *z = calloc(1, big);
    if (!z || z[0] != 0 || z[big - 1] != 0)
        fail("calloc of a huge block");
    free(z);

    if (failures)
        return 1;
    printf("ok static_link (challoc %s)\n", v);
    return 0;
}
