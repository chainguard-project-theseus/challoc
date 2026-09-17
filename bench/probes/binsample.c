// binsample.c: LD_PRELOAD sidecar. A thread prints, once per second, the
// bin row for size S (env BIN_SIZE, default 64) from malloc_stats_print's
// text: curslabs, curregs, nonfull, plus the arena's total.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
extern void malloc_stats_print(void (*cb)(void *, const char *), void *cbopaque, const char *opts) __attribute__((weak));
static char buf[1 << 20]; static size_t used;
static void cb(void *o, const char *s) { size_t l = strlen(s); if (used + l < sizeof buf) { memcpy(buf + used, s, l); used += l; } }
static void *run(void *a) {
    const char *want = getenv("BIN_SIZE") ? getenv("BIN_SIZE") : "64";
    for (int t = 1; ; t++) {
        sleep(1);
        used = 0; malloc_stats_print(cb, NULL, NULL); buf[used] = 0;
        char *line = strtok(buf, "\n"); int inbins = 0;
        while (line) {
            if (strstr(line, "bins:")) inbins = 1;
            else if (inbins) {
                char first[32]; if (sscanf(line, " %31s", first) == 1 && strcmp(first, want) == 0) { fprintf(stderr, "t=%d %s\n", t, line); }
                if (strstr(line, "large")) inbins = 0;
            }
            line = strtok(NULL, "\n");
        }
    }
    return a;
}
__attribute__((constructor)) static void start(void) { if (malloc_stats_print) { pthread_t th; pthread_create(&th, NULL, run, NULL); pthread_detach(th); } }
