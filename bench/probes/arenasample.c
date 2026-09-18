// arenasample.c: LD_PRELOAD sidecar for the arena RSS attribution (Phase 1.5
// acceptance remediation). A thread calls malloc_stats_print every
// ARENASAMPLE_MS (default 1000) and prints one line per sample: the process
// VmRSS, how many arenas hold active slabs, and the sums over arenas of the
// "small:" line (allocated = curregs * size, i.e. live plus thread-cached
// slots; active = bytes of non-Empty slabs; dirty = bytes of Empty slabs
// waiting for decay; retained = purged). active - allocated is the space in
// partially filled slabs, dirty is the decay backlog. With BIN_SIZE set the
// bin row of that size is summed across arenas too (curregs, curslabs,
// nonfull, empty). The sample with the largest active is repeated at exit as
// "peak". Build: cc -O2 -shared -fPIC -o arenasample.so arenasample.c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
#include <time.h>
extern void malloc_stats_print(void (*cb)(void *, const char *), void *cbopaque, const char *opts) __attribute__((weak));
static char buf[8 << 20]; static size_t used;
static void cb(void *o, const char *s) { size_t l = strlen(s); if (used + l < sizeof buf) { memcpy(buf + used, s, l); used += l; } }
static long rss_kb(void) {
    FILE *f = fopen("/proc/self/status", "r"); char line[256]; long v = 0;
    if (!f) return 0;
    while (fgets(line, sizeof line, f)) if (sscanf(line, "VmRSS: %ld", &v) == 1) break;
    fclose(f); return v;
}
static char peak[768]; static double peak_active = -1;
// ARENASAMPLE_SMAPS=1: Rss (KiB) of /proc/self/smaps summed by mapping name
// (challoc's named anon regions, libraries, stacks), so the split of the
// process RSS between the class regions, the metadata region and the rest is
// visible next to the allocator's own figures.
static void smaps(void) {
    FILE *f = fopen("/proc/self/smaps", "r"); if (!f) return;
    char line[512]; char name[128] = "?"; long classes = 0, meta = 0, huge = 0, anon = 0, file = 0, stack = 0, other = 0;
    while (fgets(line, sizeof line, f)) {
        long kb;
        if (strchr(line, '-') && strchr(line, ' ') && line[0] != ' ' && strncmp(line, "Rss:", 4) != 0 && !strstr(line, " kB")) {
            char *p = strrchr(line, ' '); if (!p) p = line; while (*p == ' ') p++;
            snprintf(name, sizeof name, "%s", p); name[strcspn(name, "\n")] = 0;
            if (line[strlen(line) - 2] == ' ') name[0] = 0;
        } else if (sscanf(line, "Rss: %ld kB", &kb) == 1) {
            if (strstr(name, "challoc:classes")) classes += kb;
            else if (strstr(name, "challoc:meta")) meta += kb;
            else if (strstr(name, "challoc:huge")) huge += kb;
            else if (strstr(name, "stack")) stack += kb;
            else if (name[0] == 0 || strstr(name, "[anon")) anon += kb;
            else if (name[0] == '/') file += kb;
            else other += kb;
        }
    }
    fclose(f);
    fprintf(stderr, "arenasample smaps MiB: classes=%.1f meta=%.1f huge=%.1f anon=%.1f stack=%.1f file=%.1f other=%.1f\n",
        classes / 1024.0, meta / 1024.0, huge / 1024.0, anon / 1024.0, stack / 1024.0, file / 1024.0, other / 1024.0);
}
static void sample(int t, const char *tag) {
    const char *want = getenv("BIN_SIZE");
    used = 0; malloc_stats_print(cb, NULL, NULL); buf[used] = 0;
    double alloc = 0, active = 0, dirty = 0, retained = 0, meta = -1; int arenas = 0, arena_active = 0;
    double l_alloc = 0, l_active = 0, l_dirty = 0, l_retained = 0, l_runs = 0, resident = -1;
    double b_regs = 0, b_slabs = 0, b_nonfull = 0, b_empty = 0; int b_arenas = 0;
    char *save; char *line = strtok_r(buf, "\n", &save);
    while (line) {
        unsigned long a, b, c, d;
        unsigned long m;
        if (strncmp(line, "arenas[", 7) == 0) { arenas++; }
        else if (meta < 0 && sscanf(line, "  metadata: %lu", &m) == 1) { meta = m; }
        else if (resident < 0 && strncmp(line, "Allocated:", 10) == 0) { char *r = strstr(line, "resident: "); if (r) resident = strtoul(r + 10, NULL, 10); }
        else if (sscanf(line, "  large: %lu %lu %lu %lu (%lu huge runs)", &a, &b, &c, &d, &m) == 5) {
            l_alloc += a; l_active += b; l_dirty += c; l_retained += d; l_runs += m;
        }
        else if (sscanf(line, "  small: %lu %lu %lu %lu", &a, &b, &c, &d) == 4) {
            alloc += a; active += b; dirty += c; retained += d; if (b) arena_active++;
        } else if (want) {
            unsigned long size, ind, nregs, slab, regs, slabs, nonfull, empty, purged;
            if (sscanf(line, " %lu %lu %lu %lu %lu %lu %lu %lu %lu", &size, &ind, &nregs, &slab, &regs, &slabs, &nonfull, &empty, &purged) == 9
                && size == strtoul(want, NULL, 10)) {
                b_regs += regs; b_slabs += slabs; b_nonfull += nonfull; b_empty += empty; if (slabs || empty) b_arenas++;
            }
        }
        line = strtok_r(NULL, "\n", &save);
    }
    char out[768];
    int n = snprintf(out, sizeof out, "arenasample %s t=%d rss=%.1fMiB arenas=%d active_arenas=%d allocated=%.1fMiB active=%.1fMiB partial_waste=%.1fMiB dirty=%.1fMiB retained=%.1fMiB | large: allocated=%.1fMiB active=%.1fMiB dirty=%.1fMiB retained=%.1fMiB runs=%.0f | metadata=%.1fMiB resident=%.1fMiB",
        tag, t, rss_kb() / 1024.0, arenas, arena_active, alloc / 1048576, active / 1048576, (active - alloc) / 1048576, dirty / 1048576, retained / 1048576,
        l_alloc / 1048576, l_active / 1048576, l_dirty / 1048576, l_retained / 1048576, l_runs, meta / 1048576, resident / 1048576);
    if (want && n > 0 && (size_t)n < sizeof out)
        snprintf(out + n, sizeof out - n, " bin%s: curregs=%.0f curslabs=%.0f nonfull=%.0f empty=%.0f arenas=%d", want, b_regs, b_slabs, b_nonfull, b_empty, b_arenas);
    fprintf(stderr, "%s\n", out);
    if (getenv("ARENASAMPLE_SMAPS")) smaps();
    if (active > peak_active) { peak_active = active; snprintf(peak, sizeof peak, "%s", out); }
}
static void *run(void *a) {
    int ms = getenv("ARENASAMPLE_MS") ? atoi(getenv("ARENASAMPLE_MS")) : 1000;
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    for (int t = 1; ; t++) { nanosleep(&ts, NULL); sample(t, "sample"); }
    return a;
}
__attribute__((destructor)) static void finish(void) {
    if (!malloc_stats_print) return;
    sample(0, "exit");
    if (peak_active >= 0) fprintf(stderr, "arenasample peak: %s\n", peak);
}
__attribute__((constructor)) static void start(void) { if (malloc_stats_print) { pthread_t th; pthread_create(&th, NULL, run, NULL); pthread_detach(th); } }
