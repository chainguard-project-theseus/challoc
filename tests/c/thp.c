/*
 * Transparent huge pages behind the `thp` key (plan Phase 2.5 task 9,
 * design 7, 10 and 17): the advice is issued where the key and the kernel
 * say so, and nowhere under the default. One mode per process:
 *
 *   thp never    no key: after 8 MiB of 64-byte blocks the VMA holding
 *                them has no `hg` in its VmFlags, stats.thp.mode is 0 and
 *                no range was advised (byte for byte the allocator before
 *                the key had a mechanism);
 *   thp always   MALLOC_CONF=thp:always: opt.thp reads "always" (or
 *                "never" when a cgroup limit under 1 GiB forced it, a pass
 *                with a note); with the PMD size enabled in sysfs (the
 *                feature line's mthp lists 2M) the mode is 2, the VMA has
 *                `hg`, THPeligible 1 and, once 8 MiB are touched, at least
 *                4 MiB of AnonHugePages; with only smaller sizes enabled
 *                the mode is 1 and the VMA has `hg`; with nothing enabled
 *                the mode is 0;
 *   thp auto     MALLOC_CONF=thp:auto: the mode is 1 only when the line
 *                lists a size below 2M, in which case the VMA has `hg` and
 *                the kernel's per-size anon_fault_alloc counter for the
 *                smallest such size rose by at least half the folios the
 *                8 MiB could take; otherwise the mode is 0 and nothing is
 *                advised (the host requirement is documented: on Ubuntu's
 *                default sysfs `auto` is inert).
 *
 * The mode challoc decided is read through stats.thp.mode; what the kernel
 * enables comes from the feature line, so the program checks the two
 * against each other and against /proc/self/smaps.
 */
#include "common.h"
#include <fcntl.h>

extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));
__attribute__((weak)) const char *challoc_features(void);

#define BLOCKS (8u << 20 >> 6) /* 8 MiB of 64-byte blocks */

static size_t stat_of(const char *name)
{
    size_t v = 0, len = sizeof v;
    REQUIRE(mallctl(name, &v, &len, NULL, 0) == 0, "mallctl(%s)", name);
    return v;
}

/* The smaps entry holding `p`: AnonHugePages (kB), THPeligible (-1 when
 * the kernel does not print it) and whether VmFlags has `hg`. */
static int smaps_of(const void *p, long *anon_huge_kb, int *eligible, int *hg)
{
    FILE *f = fopen("/proc/self/smaps", "r");
    REQUIRE(f, "fopen(/proc/self/smaps)");
    char line[512];
    int in = 0, found = 0;
    *anon_huge_kb = 0;
    *eligible = -1;
    *hg = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        char perms[8];
        if (sscanf(line, "%lx-%lx %7s", &lo, &hi, perms) == 3 && strchr(line, '-') < strchr(line, ' ')) {
            if (found)
                break;
            in = (uintptr_t)p >= lo && (uintptr_t)p < hi;
            if (in)
                found = 1;
            continue;
        }
        if (!in)
            continue;
        long v;
        if (sscanf(line, "AnonHugePages: %ld kB", &v) == 1)
            *anon_huge_kb = v;
        else if (sscanf(line, "THPeligible: %ld", &v) == 1)
            *eligible = (int)v;
        else if (strncmp(line, "VmFlags:", 8) == 0) {
            for (char *tok = strtok(line + 8, " \n"); tok; tok = strtok(NULL, " \n"))
                if (strcmp(tok, "hg") == 0)
                    *hg = 1;
        }
    }
    fclose(f);
    return found;
}

/* `key=` value of the feature line into `out`. */
static void feature_value(const char *line, const char *key, char *out, size_t n)
{
    char pat[32];
    snprintf(pat, sizeof pat, " %s=", key);
    const char *at = strstr(line, pat);
    REQUIRE(at, "no %s in `%s`", key, line);
    at += strlen(pat);
    size_t k = strcspn(at, " ");
    if (k >= n)
        k = n - 1;
    memcpy(out, at, k);
    out[k] = 0;
}

/* The smallest size below 2M in an mthp list (`64K,2M`), in kB; 0 if none. */
static unsigned long smallest_sub_pmd_kb(const char *mthp)
{
    unsigned long best = 0;
    char copy[128];
    strncpy(copy, mthp, sizeof copy - 1);
    copy[sizeof copy - 1] = 0;
    for (char *tok = strtok(copy, ","); tok; tok = strtok(NULL, ",")) {
        char *end;
        unsigned long v = strtoul(tok, &end, 10);
        unsigned long kb = *end == 'M' ? v << 10 : *end == 'K' ? v : *end == 'G' ? v << 20 : 0;
        if (kb && kb < 2048 && (!best || kb < best))
            best = kb;
    }
    return best;
}

static unsigned long read_counter(const char *path)
{
    FILE *f = fopen(path, "r");
    if (!f)
        return 0;
    unsigned long v = 0;
    if (fscanf(f, "%lu", &v) != 1)
        v = 0;
    fclose(f);
    return v;
}

int main(int argc, char **argv)
{
    require_challoc();
    REQUIRE(argc == 2, "usage: thp never|always|auto");
    REQUIRE(mallctl != NULL && challoc_features != NULL, "mallctl/challoc_features not exported");
    const char *mode = argv[1];

    const char *opt = NULL;
    size_t len = sizeof opt;
    REQUIRE(mallctl("opt.thp", &opt, &len, NULL, 0) == 0 && opt, "opt.thp");
    const char *line = challoc_features();
    char thp[16], mthp[128];
    feature_value(line, "thp", thp, sizeof thp);
    feature_value(line, "mthp", mthp, sizeof mthp);
    int pmd_enabled = strstr(mthp, "2M") != NULL;
    unsigned long sub_kb = smallest_sub_pmd_kb(mthp);
    printf("thp %s: opt.thp=%s sysfs thp=%s mthp=%s\n", mode, opt, thp, mthp);

    if (strcmp(mode, "never") != 0 && strcmp(opt, "never") == 0) {
        printf("thp %s: forced to never by a cgroup limit under 1 GiB; only the never checks apply\n", mode);
        mode = "never";
    }
    CHECK(strcmp(opt, mode) == 0, "opt.thp `%s` for mode %s", opt, mode);

    char counter_path[160] = "";
    unsigned long before = 0;
    if (sub_kb) {
        snprintf(counter_path, sizeof counter_path,
                 "/sys/kernel/mm/transparent_hugepage/hugepages-%lukB/stats/anon_fault_alloc", sub_kb);
        before = read_counter(counter_path);
    }

    /* 8 MiB of one small class, every byte touched. */
    static unsigned char *blocks[BLOCKS];
    for (size_t i = 0; i < BLOCKS; i++) {
        blocks[i] = malloc(64);
        REQUIRE(blocks[i], "malloc(64)");
        memset(blocks[i], 0x5a, 64);
    }
    long anon_kb;
    int eligible, hg;
    REQUIRE(smaps_of(blocks[BLOCKS / 2], &anon_kb, &eligible, &hg), "no smaps entry holds a block");
    size_t smode = stat_of("stats.thp.mode");
    size_t class_ranges = stat_of("stats.thp.class_ranges");
    size_t class_bytes = stat_of("stats.thp.class_bytes");
    size_t madv_failures = stat_of("stats.thp.failures");
    printf("thp %s: mode %zu class_ranges %zu class_bytes %zu failures %zu; VMA hg=%d THPeligible=%d AnonHugePages=%ld kB\n",
           mode, smode, class_ranges, class_bytes, madv_failures, hg, eligible, anon_kb);
    CHECK(madv_failures == 0, "%zu madvise failures", madv_failures);

    if (strcmp(mode, "never") == 0) {
        CHECK(smode == 0, "mode %zu under never", smode);
        CHECK(class_ranges == 0 && class_bytes == 0, "ranges advised under never");
        CHECK(!hg, "VmFlags has hg under never");
        CHECK(stat_of("stats.thp.huge_ranges") == 0 && stat_of("stats.thp.meta_ranges") == 0, "advice under never");
    } else if (strcmp(mode, "always") == 0) {
        if (pmd_enabled) {
            CHECK(smode == 2, "mode %zu with 2M enabled", smode);
            CHECK(hg, "no hg with mode 2");
            CHECK(class_ranges >= 4 && class_bytes >= 8u << 20, "%zu chunk ranges (%zu bytes) for 8 MiB", class_ranges, class_bytes);
            CHECK(eligible != 0, "THPeligible %d with hg and 2M enabled", eligible);
            /* Every chunk was committed whole and the kernel could give a
             * 2 MiB page at the first touch (defrag=madvise compacts for
             * an advised VMA); be lenient by half for a busy host. */
            CHECK(anon_kb >= 4096, "AnonHugePages %ld kB after 8 MiB touched", anon_kb);
        } else if (strcmp(mthp, "none") != 0) {
            CHECK(smode == 1, "mode %zu with sub-PMD sizes only", smode);
            CHECK(hg, "no hg with mode 1");
            CHECK(class_ranges >= 1, "no group advised");
        } else {
            CHECK(smode == 0, "mode %zu with nothing enabled", smode);
            CHECK(!hg, "hg with nothing enabled");
        }
    } else if (strcmp(mode, "auto") == 0) {
        if (smode == 1) {
            CHECK(sub_kb != 0, "mode 1 but no sub-PMD size in `%s`", mthp);
            CHECK(hg, "no hg with mode 1");
            CHECK(class_ranges >= 1 && class_bytes >= 8u << 20, "ranges %zu bytes %zu", class_ranges, class_bytes);
            CHECK(eligible != 0, "THPeligible %d under mode 1", eligible);
            if (counter_path[0]) {
                unsigned long after = read_counter(counter_path);
                unsigned long folios = (8u << 20) / (sub_kb << 10);
                printf("thp auto: %s %lu -> %lu (+%lu, %lu folios touched)\n", counter_path, before, after,
                       after - before, folios);
                CHECK(after - before >= folios / 2, "anon_fault_alloc rose by %lu for %lu folios", after - before, folios);
            }
        } else {
            CHECK(smode == 0, "mode %zu under auto", smode);
            CHECK(!hg, "hg but mode 0");
            CHECK(class_ranges == 0, "ranges advised but mode 0");
            printf("thp auto: inert on this host (%s)\n", sub_kb ? "the PMD size is madvise-gated" : "no size below 2M enabled");
        }
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }

    for (size_t i = 0; i < BLOCKS; i++)
        free(blocks[i]);
    return finish("thp");
}
