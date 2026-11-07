/*
 * challoc loaded late, by `dlopen`, into a process whose allocator is
 * glibc's (a plugin with a DT_NEEDED on libchalloc.so.0): the program's
 * malloc stays glibc's, and loading the library must not initialise
 * challoc (no region reservation, no fork handlers for an allocator
 * nothing uses). Run bare, with the library's path as the argument, by the
 * `api::` test `dlopen_late_does_not_initialise`; not a
 * preloaded row (it is the one program that wants glibc's malloc).
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static long vm_size_kib(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    long kib = -1;
    while (f && fgets(line, sizeof line, f))
        if (strncmp(line, "VmSize:", 7) == 0)
            kib = strtol(line + 7, NULL, 10);
    if (f)
        fclose(f);
    return kib;
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: dlopen_late LIB\n");
        return 2;
    }
    if (dlsym(RTLD_DEFAULT, "challoc_version")) {
        fprintf(stderr, "challoc is already loaded: run without the preload\n");
        return 2;
    }
    long before = vm_size_kib();
    void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 1;
    }
    if (!dlsym(h, "challoc_version")) {
        fprintf(stderr, "no challoc_version in %s\n", argv[1]);
        return 1;
    }
    long after = vm_size_kib();
    /* Init reserves terabytes of address space; the library's own
     * mapping is a few MiB. */
    if (before < 0 || after - before > 1L << 20) {
        fprintf(stderr, "VmSize grew from %ld to %ld KiB: challoc initialised at load\n", before, after);
        return 1;
    }
    pid_t pid = fork();
    if (pid == 0)
        _exit(0);
    int st = 0;
    if (pid < 0 || waitpid(pid, &st, 0) != pid || !WIFEXITED(st)) {
        fprintf(stderr, "fork after the load failed\n");
        return 1;
    }
    printf("ok dlopen_late (VmSize %ld -> %ld KiB)\n", before, after);
    return 0;
}
