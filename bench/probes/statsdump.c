/* statsdump: a shared object preloaded after the allocator under test
 * (LD_PRELOAD=<allocator>:statsdump.so) that calls malloc_stats_print at
 * process exit when the loaded allocator exports it (jemalloc, challoc), so
 * a program that cannot call it itself (a JVM, bench/drivers/jvm-bench.sh) still
 * leaves the allocator's own view (allocated, active, resident, retained,
 * per-class occupancy) on stderr. jemalloc's MALLOC_CONF=stats_print:true
 * does the same thing; challoc accepts that key but does not act on it
 * (src/config.rs, INERT_KEYS). Nothing here allocates. Under glibc,
 * mimalloc or tcmalloc the symbol is absent and nothing is printed.
 *
 *   cc -O2 -shared -fPIC -o statsdump.so statsdump.c
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <unistd.h>

typedef void (*stats_print_fn)(void (*)(void *, const char *), void *, const char *);

static void dump(void) {
    stats_print_fn f = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    if (!f) return;
    static const char hdr[] = "--- statsdump: malloc_stats_print at exit ---\n";
    (void)!write(2, hdr, sizeof hdr - 1);
    f(NULL, NULL, NULL);
}

__attribute__((constructor)) static void statsdump_init(void) { atexit(dump); }
