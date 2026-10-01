/*
 * The mapping shape a forked child inherits (the `connect20`
 * mechanism): what a PostgreSQL backend, or any forked
 * child of a challoc process, carries in its address space before it has
 * done anything, and what it costs to fork it and let it exit.
 *
 * The parent allocates the way a postmaster does at start-up: a few
 * objects of every size from 16 bytes to 256 KiB (every size class gets a
 * slab, so every class's tables in the metadata region get their first
 * page), a spread of small objects (heaps, caches) and a handful of huge
 * runs, all touched, all kept. Then it forks FORKS times (default 20).
 * Each child allocates a backend's worth of fresh objects (BACKEND_KIB,
 * default 512 KiB, spread over the sizes in the same way, so the child
 * commits its own slabs the way a backend does), reads its own
 * /proc/self/maps, /proc/self/smaps and /proc/self/status, and exits. The
 * first child prints what it saw:
 *
 *   maps            lines in /proc/self/maps by region (the kernel's
 *                   `[anon:challoc:classes|meta|huge]` names, so the kernel
 *                   must have PR_SET_VMA_ANON_NAME; anything else is
 *                   "other": the binary, libraries, stacks, glibc's heap),
 *                   split into readable-writable (committed) and `---p`
 *                   (reserved) lines;
 *   meta rw         the committed ranges of the metadata region in address
 *                   order: count, total bytes, the smallest and largest,
 *                   and the reserved gaps between consecutive committed
 *                   ranges (count, smallest, median, largest): a gap is an
 *                   uncommitted table tail between two tables' first pages;
 *   rss             Rss of the child summed per region from smaps, in KiB,
 *                   plus VmPTE and VmRSS from status;
 *
 * and the parent prints, over all FORKS children, the median and maximum
 * of fork() itself (microseconds) and of fork-to-exit (milliseconds, the
 * child's start-up allocations, its /proc reads and its teardown; the
 * /proc reads are the same work under every allocator).
 *
 * Nothing here is a criterion; the numbers go in the results file. Under
 * glibc (FORK_MAPS_ANY=1 skips the preload check) the region lines are
 * zero and "other" holds `[heap]`. Run in the dev container:
 *
 *   sh scripts/ctest-cc.sh bench/probes/fork_maps.c -o target/fork_maps
 *   LD_PRELOAD=target/release/libchalloc.so target/fork_maps
 *   FORK_MAPS_ANY=1 target/fork_maps                     # glibc
 *   strace -f -ff -o /tmp/fm -e trace=mprotect,madvise,mmap,munmap \
 *       env LD_PRELOAD=... target/fork_maps 1           # per-pid syscall files
 */
#include "common.h"
#include <sys/mman.h>

#define KIB 1024ul
#define MIB (1024ul * KIB)

/* The size ladder: every challoc class from 16 bytes to 256 KiB gets at
 * least one object (steps of 1/8 of a power of two cover the 4-per-octave
 * classes), then a few huge sizes. */
static size_t ladder[128];
static size_t nladder;

static void build_ladder(void)
{
    for (size_t lg = 4; lg < 18; lg++) {
        size_t base = 1ul << lg;
        for (size_t k = 0; k < 8; k++) {
            size_t s = base + k * (base / 8);
            if (s >= 256 * KIB)
                break;
            ladder[nladder++] = s;
        }
    }
    ladder[nladder++] = 256 * KIB;
    ladder[nladder++] = 1 * MIB;
    ladder[nladder++] = 3 * MIB;
}

/* Allocate `budget` bytes spread over the ladder, `per` objects of each
 * size per pass, touching every object; returns how many were kept. */
static size_t allocate_spread(void **keep, size_t cap, size_t budget, size_t per)
{
    size_t n = 0, live = 0;
    uint64_t rng = 0x5EED0000 + budget;
    while (live < budget && n + nladder * per < cap) {
        for (size_t i = 0; i < nladder; i++) {
            for (size_t j = 0; j < per; j++) {
                size_t sz = ladder[i];
                if (sz > 4096)
                    sz -= rng_next(&rng) % 16; /* not always the exact class size */
                unsigned char *p = malloc(sz);
                REQUIRE(p, "malloc(%zu)", sz);
                p[0] = 1;
                p[sz - 1] = 2;
                if (sz >= 4096)
                    for (size_t o = 0; o < sz; o += 4096)
                        p[o] = 3;
                keep[n++] = p;
                live += sz;
            }
        }
    }
    return n;
}

struct region {
    const char *name;
    long lines, rw, none, rss_kb;
};

static struct region regions[] = {
    { "classes", 0, 0, 0, 0 },
    { "meta", 0, 0, 0, 0 },
    { "huge", 0, 0, 0, 0 },
    { "other", 0, 0, 0, 0 },
};

static int region_of(const char *line)
{
    if (strstr(line, "[anon:challoc:classes]"))
        return 0;
    if (strstr(line, "[anon:challoc:meta]"))
        return 1;
    if (strstr(line, "[anon:challoc:huge]"))
        return 2;
    return 3;
}

static int cmp_ul(const void *a, const void *b)
{
    unsigned long x = *(const unsigned long *)a, y = *(const unsigned long *)b;
    return x < y ? -1 : x > y;
}

static long status_kb(const char *key)
{
    FILE *f = fopen("/proc/self/status", "r");
    REQUIRE(f, "status");
    char line[256];
    long v = -1;
    while (fgets(line, sizeof line, f))
        if (strncmp(line, key, strlen(key)) == 0)
            sscanf(line + strlen(key), " %ld", &v);
    fclose(f);
    return v;
}

/* Read smaps once: header lines count into the regions and the Rss lines
 * under them; the metadata region's rw ranges are collected. */
static void read_smaps(void)
{
    FILE *f = fopen("/proc/self/smaps", "r");
    REQUIRE(f, "smaps");
    static unsigned long rw_lo[8192], rw_hi[8192];
    size_t nrw = 0;
    int cur = 3;
    char line[512];
    unsigned long lo, hi;
    char perms[8];
    while (fgets(line, sizeof line, f)) {
        if (sscanf(line, "%lx-%lx %7s", &lo, &hi, perms) == 3 && strchr(line, '-') < strchr(line, ' ')) {
            cur = region_of(line);
            regions[cur].lines++;
            if (perms[0] == 'r' && perms[1] == 'w')
                regions[cur].rw++;
            else if (perms[0] == '-' && perms[1] == '-' && perms[2] == '-')
                regions[cur].none++;
            if (cur == 1 && perms[0] == 'r' && perms[1] == 'w' && nrw < 8192) {
                rw_lo[nrw] = lo;
                rw_hi[nrw] = hi;
                nrw++;
            }
        } else if (strncmp(line, "Rss:", 4) == 0) {
            long kb;
            if (sscanf(line + 4, " %ld", &kb) == 1)
                regions[cur].rss_kb += kb;
        }
    }
    fclose(f);

    long total = 0;
    for (int r = 0; r < 4; r++)
        total += regions[r].lines;
    printf("fork_maps: child maps total %ld:", total);
    for (int r = 0; r < 4; r++)
        printf(" %s %ld (rw %ld, none %ld)", regions[r].name, regions[r].lines, regions[r].rw, regions[r].none);
    printf("\n");

    if (nrw > 0) {
        unsigned long bytes = 0, smallest = ~0ul, largest = 0;
        static unsigned long gaps[8192];
        size_t ngaps = 0;
        for (size_t i = 0; i < nrw; i++) {
            unsigned long len = rw_hi[i] - rw_lo[i];
            bytes += len;
            if (len < smallest)
                smallest = len;
            if (len > largest)
                largest = len;
            if (i + 1 < nrw)
                gaps[ngaps++] = rw_lo[i + 1] - rw_hi[i];
        }
        qsort(gaps, ngaps, sizeof gaps[0], cmp_ul);
        printf("fork_maps: meta rw ranges %zu, %lu KiB (smallest %lu KiB, largest %lu KiB); gaps %zu",
               nrw, bytes / KIB, smallest / KIB, largest / KIB, ngaps);
        if (ngaps)
            printf(" (smallest %lu KiB, median %lu KiB, largest %lu MiB)", gaps[0] / KIB, gaps[ngaps / 2] / KIB,
                   gaps[ngaps - 1] / MIB);
        printf("\n");
        if (getenv("FORK_MAPS_RANGES"))
            for (size_t i = 0; i < nrw; i++)
                printf("fork_maps:   meta rw %lx-%lx %lu KiB\n", rw_lo[i], rw_hi[i], (rw_hi[i] - rw_lo[i]) / KIB);
    }
    printf("fork_maps: child rss KiB:");
    for (int r = 0; r < 4; r++)
        printf(" %s %ld", regions[r].name, regions[r].rss_kb);
    printf("; VmRSS %ld VmPTE %ld VmSize %ld\n", status_kb("VmRSS:"), status_kb("VmPTE:"), status_kb("VmSize:"));
}

static double now_us(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e6 + ts.tv_nsec / 1e3;
}

static int cmp_d(const void *a, const void *b)
{
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

int main(int argc, char **argv)
{
    if (!getenv("FORK_MAPS_ANY"))
        require_challoc();
    build_ladder();
    int forks = argc > 1 ? atoi(argv[1]) : 20;
    if (forks < 1)
        forks = 1;
    size_t backend_kib = getenv("BACKEND_KIB") ? strtoul(getenv("BACKEND_KIB"), NULL, 10) : 512;

    /* The postmaster's heap: about 8 MiB over every size, four per size. */
    static void *keep[65536];
    size_t n = allocate_spread(keep, 65536, 8 * MIB, 4);
    printf("fork_maps: parent kept %zu objects over %zu sizes; VmRSS %ld KiB\n", n, nladder, status_kb("VmRSS:"));
    {
        FILE *f = fopen("/proc/self/maps", "r");
        long lines = 0, meta = 0, classes = 0, huge = 0;
        char line[512];
        while (fgets(line, sizeof line, f)) {
            lines++;
            int r = region_of(line);
            classes += r == 0;
            meta += r == 1;
            huge += r == 2;
        }
        fclose(f);
        printf("fork_maps: parent maps %ld: classes %ld meta %ld huge %ld other %ld\n", lines, classes, meta, huge,
               lines - classes - meta - huge);
    }

    static double fork_us[4096], total_ms[4096];
    if (forks > 4096)
        forks = 4096;
    for (int i = 0; i < forks; i++) {
        double t0 = now_us();
        pid_t pid = fork();
        double t1 = now_us();
        REQUIRE(pid >= 0, "fork");
        if (pid == 0) {
            static void *mine[16384];
            size_t m = allocate_spread(mine, 16384, backend_kib * KIB, 1);
            if (i == 0) {
                printf("fork_maps: child allocated %zu objects (%zu KiB)\n", m, backend_kib);
                read_smaps();
                fflush(stdout);
            }
            _exit(0);
        }
        int status = 0;
        REQUIRE(waitpid(pid, &status, 0) == pid, "waitpid");
        double t2 = now_us();
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child %d status %d", i, status);
        fork_us[i] = t1 - t0;
        total_ms[i] = (t2 - t0) / 1e3;
    }
    qsort(fork_us, forks, sizeof fork_us[0], cmp_d);
    qsort(total_ms, forks, sizeof total_ms[0], cmp_d);
    printf("fork_maps: forks %d: fork_us median %.0f max %.0f; fork-to-exit ms median %.2f max %.2f\n", forks,
           fork_us[forks / 2], fork_us[forks - 1], total_ms[forks / 2], total_ms[forks - 1]);
    for (size_t i = 0; i < n; i++)
        free(keep[i]);
    return finish("fork_maps");
}
