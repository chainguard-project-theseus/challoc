/*
 * Transparent huge pages behind the `thp` key (plan Phase 2.5 task 9,
 * design 7, 10 and 17): the advice is issued where the key and the kernel
 * say so, and nowhere under the default. One mode per process:
 *
 *   thp never    no key: after 8 MiB of 64-byte blocks the VMA holding
 *                them has no `hg` in its VmFlags, stats.thp.mode is 0 and
 *                no range was advised (byte for byte the allocator before
 *                the key had a mechanism); and, since plan Phase 2.5 task
 *                16, the VMA has `nh` (MADV_NOHUGEPAGE issued on the
 *                reservation) on every kernel with THP, so that on a host
 *                whose global mode is `always` (read from sysfs; Chainguard
 *                OS and Debian's kernels) the 8 MiB touched show
 *                AnonHugePages 0 kB and THPeligible 0. The huge region is
 *                the other way (the 2026-09-20 decision, option (d) of
 *                bench/results/2026-09-20-aarch64-cgos-thp-ab.md): the
 *                8 MiB huge run carries no `nh`, stats.thp.nohuge_ranges
 *                reads 2 (classes, metadata), the thp: line says `huge
 *                follows host`, and on an `always` host the run shows
 *                THPeligible 1 and AnonHugePages again, as jemalloc's
 *                large allocations do. On a `madvise` host the
 *                AnonHugePages checks pass trivially and the program says
 *                so; the `nh` checks are the both-ways part there;
 *   thp always   MALLOC_CONF=thp:always: opt.thp reads "always" (or
 *                "never" when a cgroup limit under 1 GiB forced it, a pass
 *                with a note); with the PMD size enabled in sysfs (the
 *                feature line's mthp lists it: 2M at 4 KiB pages) and the
 *                PMD no larger than 2 MiB the mode is 2, the VMA has `hg`,
 *                THPeligible 1 and, once 8 MiB are touched, at least 4 MiB
 *                of AnonHugePages; with only sizes below the PMD enabled
 *                (or a PMD over 2 MiB, 512 MiB at 64 KiB pages, which
 *                challoc never commits in chunks) the mode is 1 and the
 *                VMA has `hg`; with nothing usable enabled the mode is 0;
 *   thp auto     MALLOC_CONF=thp:auto: the mode is 1 only when the line
 *                lists a size below the PMD, in which case the VMA has `hg`
 *                and the kernel's per-size anon_fault_alloc counter for the
 *                smallest such size rose by at least half the folios the
 *                8 MiB could take; otherwise the mode is 0 and nothing is
 *                advised (the host requirement is documented: on Ubuntu's
 *                default sysfs `auto` is inert).
 *
 *   thp process  MALLOC_CONF=thp_process:advised,thp:auto (plan Phase 2.5
 *                task 16, the second half): the process-wide
 *                prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED),
 *                both ways by the kernel's own answer. The feature line's
 *                thp_except_advised= key says what the probe found; with
 *                `yes` (6.18+) stats.thp.process reads 1 (in force),
 *                prctl(PR_GET_THP_DISABLE) reads 3 (1 | EXCEPT_ADVISED),
 *                the report's thp: line says `process advised:
 *                PR_THP_DISABLE_EXCEPT_ADVISED in force` and names the
 *                host's global mode, and /proc/self/status's THP_enabled
 *                field is printed as the kernel writes it; on a host whose
 *                global mode is `always` the 8 MiB of small blocks and the
 *                8 MiB huge run (unadvised: `auto` is inert on Ubuntu's
 *                sysfs and, unlike `never`, issues no MADV_NOHUGEPAGE) show
 *                AnonHugePages 0 kB, the row that moves. With `no` (EINVAL
 *                below 6.18) stats.thp.process reads 2, PR_GET_THP_DISABLE
 *                is unchanged (0) and the line says the call was skipped
 *                with the reason; with `inherited` the parent's policy is
 *                kept (3) and said so. Every other mode checks that
 *                stats.thp.process is 0 (nothing issued by default).
 *
 * The mode challoc decided is read through stats.thp.mode; what the kernel
 * enables comes from the feature line and the PMD size from sysfs
 * (hpage_pmd_size: 2 MiB at 4 KiB pages, 512 MiB at 64 KiB), so the
 * program checks the two against each other and against /proc/self/smaps.
 */
#include "common.h"
#include <fcntl.h>
#include <sys/prctl.h>

#ifndef PR_SET_THP_DISABLE
#define PR_SET_THP_DISABLE 41
#endif
#ifndef PR_GET_THP_DISABLE
#define PR_GET_THP_DISABLE 42
#endif
#ifndef PR_THP_DISABLE_EXCEPT_ADVISED
#define PR_THP_DISABLE_EXCEPT_ADVISED (1 << 1)
#endif

typedef void (*stats_print_fn)(void (*cb)(void *, const char *), void *cbopaque, const char *opts);

static char report[1 << 18];
static size_t report_len;

static void stats_cb(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

/* The `THP_enabled:` field of /proc/self/status, -1 when the kernel does
 * not print it. */
static long thp_enabled_status(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f)
        return -1;
    char line[256];
    long v = -1;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "THP_enabled: %ld", &v) == 1)
            break;
    fclose(f);
    return v;
}

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
 * the kernel does not print it) and whether VmFlags has `hg` and `nh`. */
static int smaps_of(const void *p, long *anon_huge_kb, int *eligible, int *hg, int *nh)
{
    FILE *f = fopen("/proc/self/smaps", "r");
    REQUIRE(f, "fopen(/proc/self/smaps)");
    char line[512];
    int in = 0, found = 0;
    *anon_huge_kb = 0;
    *eligible = -1;
    *hg = 0;
    *nh = 0;
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
            for (char *tok = strtok(line + 8, " \n"); tok; tok = strtok(NULL, " \n")) {
                if (strcmp(tok, "hg") == 0)
                    *hg = 1;
                else if (strcmp(tok, "nh") == 0)
                    *nh = 1;
            }
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

/* One token of an mthp list (`64K`, `2M`, `512M`, `1G`) in kB; 0 if malformed. */
static unsigned long token_kb(const char *tok)
{
    char *end;
    unsigned long v = strtoul(tok, &end, 10);
    return *end == 'M' ? v << 10 : *end == 'K' ? v : *end == 'G' ? v << 20 : 0;
}

/* The smallest size below `pmd_kb` in an mthp list (`64K,2M`), in kB; 0 if none. */
static unsigned long smallest_sub_pmd_kb(const char *mthp, unsigned long pmd_kb)
{
    unsigned long best = 0;
    char copy[128];
    strncpy(copy, mthp, sizeof copy - 1);
    copy[sizeof copy - 1] = 0;
    for (char *tok = strtok(copy, ","); tok; tok = strtok(NULL, ",")) {
        unsigned long kb = token_kb(tok);
        if (kb && kb < pmd_kb && (!best || kb < best))
            best = kb;
    }
    return best;
}

/* Whether an mthp list has a token of exactly `kb` (a token compare: `512M`
 * contains `2M` as a substring and is not it). */
static int lists_kb(const char *mthp, unsigned long kb)
{
    char copy[128];
    strncpy(copy, mthp, sizeof copy - 1);
    copy[sizeof copy - 1] = 0;
    for (char *tok = strtok(copy, ","); tok; tok = strtok(NULL, ","))
        if (token_kb(tok) == kb)
            return 1;
    return 0;
}

/* The bracketed value of /sys/kernel/mm/transparent_hugepage/enabled
 * (`always`, `madvise`, `never`), or "none" when the file is absent. */
static const char *global_thp(char *out, size_t n)
{
    FILE *f = fopen("/sys/kernel/mm/transparent_hugepage/enabled", "r");
    if (!f) {
        snprintf(out, n, "none");
        return out;
    }
    char line[128] = "";
    if (!fgets(line, sizeof line, f))
        line[0] = 0;
    fclose(f);
    char *lb = strchr(line, '['), *rb = lb ? strchr(lb, ']') : NULL;
    if (lb && rb) {
        *rb = 0;
        snprintf(out, n, "%s", lb + 1);
    } else {
        snprintf(out, n, "unknown");
    }
    return out;
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
    REQUIRE(argc == 2, "usage: thp never|always|auto|process");
    REQUIRE(mallctl != NULL && challoc_features != NULL, "mallctl/challoc_features not exported");
    const char *mode = argv[1];

    const char *opt = NULL;
    size_t len = sizeof opt;
    REQUIRE(mallctl("opt.thp", &opt, &len, NULL, 0) == 0 && opt, "opt.thp");
    const char *line = challoc_features();
    char thp[16], mthp[128], global[16];
    feature_value(line, "thp", thp, sizeof thp);
    feature_value(line, "mthp", mthp, sizeof mthp);
    global_thp(global, sizeof global);
    /* The PMD size: sysfs, else the page size's rule (page * page / 8). */
    long page = sysconf(_SC_PAGESIZE);
    unsigned long pmd_kb = read_counter("/sys/kernel/mm/transparent_hugepage/hpage_pmd_size") >> 10;
    if (!pmd_kb)
        pmd_kb = ((unsigned long)page * (unsigned long)page / 8) >> 10;
    int pmd_enabled = lists_kb(mthp, pmd_kb);
    int pmd_chunks = pmd_kb <= 2048; /* challoc commits chunks of a 2 MiB PMD only (thp.rs) */
    unsigned long sub_kb = smallest_sub_pmd_kb(mthp, pmd_kb);
    printf("thp %s: opt.thp=%s sysfs thp=%s (enabled file: %s) mthp=%s pmd %lu kB (page %ld) pmd_enabled=%d sub_pmd %lu kB\n", mode, opt,
           thp, global, mthp, pmd_kb, page, pmd_enabled, sub_kb);
    CHECK(strcmp(thp, global) == 0, "feature line thp=%s but sysfs enabled reads %s", thp, global);

    /* Plan Phase 2.5 task 16: the process-wide policy, read from the
     * allocator (stats.thp.process, the thp: line) and from the kernel
     * (PR_GET_THP_DISABLE, THP_enabled). */
    int process_mode = strcmp(mode, "process") == 0;
    size_t process = stat_of("stats.thp.process");
    long get = prctl(PR_GET_THP_DISABLE, 0, 0, 0, 0);
    long status_enabled = thp_enabled_status();
    char except[16];
    feature_value(line, "thp_except_advised", except, sizeof except);
    stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
    msp(stats_cb, NULL, "");
    report[report_len] = 0;
    const char *thp_report = strstr(report, "\nthp: ");
    REQUIRE(thp_report != NULL, "no thp: line in malloc_stats_print");
    thp_report++;
    const char *thp_report_end = strchr(thp_report, '\n');
    REQUIRE(thp_report_end != NULL, "unterminated thp: line");
    printf("thp %s: thp_except_advised=%s stats.thp.process=%zu PR_GET_THP_DISABLE=%ld THP_enabled=%ld\n", mode, except, process, get,
           status_enabled);
    printf("thp %s: report `%.*s`\n", mode, (int)(thp_report_end - thp_report), thp_report);
    {
        char host[48];
        snprintf(host, sizeof host, "; host %s process ", global);
        CHECK(strstr(thp_report, host) != NULL && strstr(thp_report, host) < thp_report_end,
              "thp: line does not name the host's global mode `%s`", global);
    }
    CHECK(!strcmp(except, "yes") || !strcmp(except, "no") || !strcmp(except, "inherited"), "thp_except_advised=%s", except);
    if (process_mode) {
        const char *popt = NULL;
        size_t plen = sizeof popt;
        REQUIRE(mallctl("opt.thp_process", &popt, &plen, NULL, 0) == 0 && popt, "opt.thp_process");
        CHECK(strcmp(popt, "advised") == 0, "opt.thp_process `%s` under thp_process:advised", popt);
        if (!strcmp(except, "yes")) {
            CHECK(process == 1, "stats.thp.process %zu with thp_except_advised=yes", process);
            CHECK(get == (1 | PR_THP_DISABLE_EXCEPT_ADVISED), "PR_GET_THP_DISABLE %ld, expected 3 (1 | EXCEPT_ADVISED)", get);
            CHECK(strstr(thp_report, "process advised: PR_THP_DISABLE_EXCEPT_ADVISED in force") != NULL, "thp: line does not say in force");
            printf("thp process: accepted (6.18+); THP_enabled reads %ld under PR_SET_THP_DISABLE(1, EXCEPT_ADVISED)\n", status_enabled);
        } else if (!strcmp(except, "no")) {
            CHECK(process == 2, "stats.thp.process %zu with thp_except_advised=no", process);
            CHECK(get == 0, "PR_GET_THP_DISABLE %ld after a refused call", get);
            CHECK(strstr(thp_report, "process advised: skipped, the kernel refuses") != NULL, "thp: line does not say skipped");
            skip_note("thp/process", "the kernel refuses PR_THP_DISABLE_EXCEPT_ADVISED (EINVAL before 6.18); nothing issued");
        } else {
            CHECK(process == 3, "stats.thp.process %zu with thp_except_advised=inherited", process);
            CHECK(get != 0, "PR_GET_THP_DISABLE %ld but the probe said inherited", get);
            CHECK(strstr(thp_report, "process advised: skipped, the process inherited") != NULL, "thp: line does not say inherited");
            skip_note("thp/process", "the process inherited a THP policy (PR_GET_THP_DISABLE %ld); nothing issued", get);
        }
        /* The regions below run the `auto` checks: the key is thp:auto. */
        mode = "auto";
    } else {
        CHECK(process == 0, "stats.thp.process %zu without thp_process:advised", process);
        CHECK(strstr(thp_report, "process default (nothing issued)") != NULL, "thp: line does not say default");
    }

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
    int eligible, hg, nh;
    REQUIRE(smaps_of(blocks[BLOCKS / 2], &anon_kb, &eligible, &hg, &nh), "no smaps entry holds a block");
    size_t smode = stat_of("stats.thp.mode");
    size_t class_ranges = stat_of("stats.thp.class_ranges");
    size_t class_bytes = stat_of("stats.thp.class_bytes");
    size_t madv_failures = stat_of("stats.thp.failures");
    printf("thp %s: mode %zu class_ranges %zu class_bytes %zu failures %zu; VMA hg=%d nh=%d THPeligible=%d AnonHugePages=%ld kB\n",
           mode, smode, class_ranges, class_bytes, madv_failures, hg, nh, eligible, anon_kb);
    CHECK(madv_failures == 0, "%zu madvise failures", madv_failures);
    /* One huge run (8 MiB, committed whole in the huge region): unlike a
     * class region, whose groups are committed a few slabs at a time, its
     * interior holds whole aligned PMD ranges in one read-write VMA, so
     * on a host whose global mode is `always` the kernel backs them with
     * huge pages at the fault unless the reservation carries `nh`. Under
     * `never` it does not (the huge region follows the host), so this is
     * the row that shows the host's policy on such a host. */
    unsigned char *big = malloc(8u << 20);
    REQUIRE(big, "malloc(8 MiB)");
    memset(big, 0x5b, 8u << 20);
    long big_anon_kb;
    int big_eligible, big_hg, big_nh;
    REQUIRE(smaps_of(big + (4u << 20), &big_anon_kb, &big_eligible, &big_hg, &big_nh), "no smaps entry holds the huge run");
    printf("thp %s: huge run 8 MiB: VMA hg=%d nh=%d THPeligible=%d AnonHugePages=%ld kB (global thp %s)\n", mode, big_hg, big_nh,
           big_eligible, big_anon_kb, global);
    if (process_mode && process == 1) {
        /* In force: nothing unadvised gets a huge page, whatever the host
         * says. Under `auto` on Ubuntu's sysfs the regions are unadvised
         * (no hg) and carry no nh either, so on an `always` host this is
         * the row the prctl moves; on a `madvise` host it holds anyway. */
        CHECK(!nh && !big_nh, "nh under thp:auto (MADV_NOHUGEPAGE is never's)");
        if (!hg && !big_hg) {
            CHECK(anon_kb == 0, "AnonHugePages %ld kB in an unadvised class region with the prctl in force (host %s)", anon_kb, global);
            CHECK(big_anon_kb == 0, "AnonHugePages %ld kB in an unadvised huge run with the prctl in force (host %s)", big_anon_kb, global);
            CHECK(eligible <= 0 && big_eligible <= 0, "THPeligible %d / %d unadvised with the prctl in force", eligible, big_eligible);
            printf("thp process: host %s, prctl in force: AnonHugePages %ld kB (class region), %ld kB (huge run), THPeligible %d / %d: %s\n",
                   global, anon_kb, big_anon_kb, eligible, big_eligible,
                   strcmp(global, "always") == 0 ? "the both-ways row" : "trivially zero on this host");
        } else {
            printf("thp process: the regions are advised (hg) on this host, so the prctl leaves them eligible: not the row to read\n");
        }
    } else if (process_mode) {
        printf("thp process: nothing in force (%s); the regions follow the host's global mode %s: AnonHugePages %ld / %ld kB\n", except, global,
               anon_kb, big_anon_kb);
    }

    if (strcmp(mode, "never") == 0) {
        CHECK(smode == 0, "mode %zu under never", smode);
        CHECK(class_ranges == 0 && class_bytes == 0, "ranges advised under never");
        CHECK(!hg, "VmFlags has hg under never");
        CHECK(stat_of("stats.thp.huge_ranges") == 0 && stat_of("stats.thp.meta_ranges") == 0, "advice under never");
        CHECK(strstr(thp_report, " (classes, metadata; huge follows host)") != NULL, "thp: line does not say what never covers");
        size_t nohuge = stat_of("stats.thp.nohuge_ranges");
        /* Plan Phase 2.5 task 16 and the 2026-09-20 decision: never means
         * never for the class and metadata regions on every host; the huge
         * region follows the host's policy. */
        if (strcmp(thp, "none") == 0) {
            CHECK(nohuge == 0, "stats.thp.nohuge_ranges %zu on a kernel without THP", nohuge);
            printf("thp never: the kernel has no THP; nothing to forbid\n");
        } else {
            CHECK(nohuge == 2, "stats.thp.nohuge_ranges %zu under never (the class and metadata reservations)", nohuge);
            CHECK(nh, "VmFlags has no nh under never (MADV_NOHUGEPAGE not issued on the class reservation)");
            CHECK(!big_nh, "VmFlags has nh on the huge run under never (the huge region follows the host's policy)");
            CHECK(eligible <= 0, "THPeligible %d in the class region under never", eligible);
            if (strcmp(global, "always") == 0) {
                printf("thp never: global THP is always: AnonHugePages %ld kB (class region), %ld kB (huge run) after 8 + 8 MiB touched: the both-ways rows\n",
                       anon_kb, big_anon_kb);
                CHECK(anon_kb == 0, "AnonHugePages %ld kB in the class region under never on an always host", anon_kb);
                CHECK(big_eligible != 0, "THPeligible %d on the huge run under never on an always host (the run should follow the host)", big_eligible);
                CHECK(big_anon_kb >= 2048, "AnonHugePages %ld kB in the 8 MiB huge run under never on an always host (the run should follow the host)",
                      big_anon_kb);
            } else {
                CHECK(big_eligible <= 0, "THPeligible %d on the unadvised huge run on a %s host", big_eligible, global);
                printf("thp never: global THP is %s, so AnonHugePages %ld / %ld kB is 0 with or without the advice; the nh flags are the check here\n",
                       global, anon_kb, big_anon_kb);
            }
        }
    } else if (strcmp(mode, "always") == 0) {
        if (pmd_enabled && pmd_chunks) {
            CHECK(smode == 2, "mode %zu with the %lu kB PMD enabled", smode, pmd_kb);
            CHECK(hg, "no hg with mode 2");
            CHECK(class_ranges >= 4 && class_bytes >= 8u << 20, "%zu chunk ranges (%zu bytes) for 8 MiB", class_ranges, class_bytes);
            CHECK(eligible != 0, "THPeligible %d with hg and the PMD enabled", eligible);
            /* Every chunk was committed whole and the kernel could give a
             * 2 MiB page at the first touch (defrag=madvise compacts for
             * an advised VMA); be lenient by half for a busy host. */
            CHECK(anon_kb >= 4096, "AnonHugePages %ld kB after 8 MiB touched", anon_kb);
        } else if (sub_kb) {
            /* Sizes below the PMD only, or a PMD challoc does not chunk
             * (over 2 MiB: 512 MiB at 64 KiB pages): the folio path. */
            CHECK(smode == 1, "mode %zu with sub-PMD sizes (%lu kB) and %s", smode, sub_kb,
                  pmd_enabled ? "a PMD too large to chunk" : "the PMD not enabled");
            CHECK(hg, "no hg with mode 1");
            CHECK(class_ranges >= 1, "no group advised");
            CHECK(class_bytes < (size_t)pmd_kb << 10 || pmd_chunks, "%zu bytes advised: PMD-sized chunks on a %lu kB PMD", class_bytes, pmd_kb);
        } else {
            CHECK(smode == 0, "mode %zu with nothing usable enabled (pmd %lu kB enabled=%d)", smode, pmd_kb, pmd_enabled);
            CHECK(!hg, "hg with nothing usable enabled");
            CHECK(class_ranges == 0, "%zu ranges advised with mode 0", class_ranges);
            skip_note("thp/always", "inert on this host (%s)",
                      pmd_enabled ? "the PMD is over 2 MiB and no smaller size is enabled" : "no size enabled");
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
            skip_note("thp/auto", "inert on this host (%s)", sub_kb ? "the PMD size is madvise-gated" : "no size below the PMD enabled");
        }
    } else {
        REQUIRE(0, "unknown mode %s", mode);
    }

    free(big);
    for (size_t i = 0; i < BLOCKS; i++)
        free(blocks[i]);
    return finish("thp");
}
