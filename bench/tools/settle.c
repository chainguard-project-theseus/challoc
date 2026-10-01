/* settle.so: preloaded after the allocator under test (rows with
 * `settle = true`). At exit it forces `epoch` and `arena.4096.purge` where
 * the allocator has mallctl and prints the resident set as one line,
 *   cb-settle rss_kb=<VmRSS> hwm_kb=<VmHWM>
 * to stderr, which bench/cb adds to the run's extra metrics (settle_mib,
 * hwm_mib). */
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

static long status_kb(const char *key) {
    char buf[4096];
    long v = -1;
    size_t n = strlen(key);
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    while (fgets(buf, sizeof buf, f))
        if (!strncmp(buf, key, n)) { v = strtol(buf + n, NULL, 10); break; }
    fclose(f);
    return v;
}

__attribute__((destructor)) static void settle(void) {
    if (mallctl) {
        uint64_t e = 1;
        size_t l = sizeof e;
        mallctl("epoch", &e, &l, &e, sizeof e);
        mallctl("arena.4096.purge", NULL, NULL, NULL, 0);
    }
    usleep(100000);
    char line[128];
    int n = snprintf(line, sizeof line, "cb-settle rss_kb=%ld hwm_kb=%ld\n", status_kb("VmRSS:"), status_kb("VmHWM:"));
    if (n > 0) (void)!write(2, line, (size_t)n);
}
