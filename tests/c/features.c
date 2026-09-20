/*
 * The feature report (plan Phase 2.5 task 2, design 16 and 17).
 *
 * challoc_features() returns a static NUL-terminated line of key=value
 * pairs: the kernel probes, then the CPU. The same bytes appear after
 * "Kernel and CPU features: " in malloc_stats_print, so a program can read
 * the host's floor and ceiling either way. The program prints the line
 * (the container test shows it, and the results file records it per host)
 * and checks: the pointer is stable, the line is well formed, every key
 * this architecture reports is present exactly once, and the report's line
 * is identical.
 *
 * The symbol is reached through a weak declaration so the program links
 * against libc alone and fails bare in require_challoc.
 */
#include "common.h"
#include <sys/mman.h>
#include <sys/prctl.h>

__attribute__((weak)) const char *challoc_features(void);

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

static int has_key(const char *line, const char *key)
{
    size_t k = strlen(key);
    int found = 0;
    for (const char *p = line; (p = strstr(p, key)) != NULL; p += k) {
        if ((p == line || p[-1] == ' ') && p[k] == '=')
            found++;
    }
    return found;
}

int main(void)
{
    require_challoc();
    REQUIRE(challoc_features != NULL, "challoc_features is not exported");

    const char *line = challoc_features();
    REQUIRE(line != NULL, "challoc_features() returned NULL");
    CHECK(line == challoc_features(), "the pointer changed between calls");
    size_t len = strlen(line);
    CHECK(len > 0 && len < 640, "line length %zu", len); /* sys::features::FeatureLine::CAP */
    CHECK(strchr(line, '\n') == NULL, "line contains a newline");
    printf("features: %s\n", line);

    /* Well formed: tokens separated by single spaces, each key=value. */
    char copy[640];
    strncpy(copy, line, sizeof copy - 1);
    copy[sizeof copy - 1] = 0;
    int tokens = 0;
    for (char *tok = strtok(copy, " "); tok; tok = strtok(NULL, " ")) {
        char *eq = strchr(tok, '=');
        CHECK(eq != NULL && eq != tok && eq[1] != 0, "malformed token `%s`", tok);
        tokens++;
    }
    CHECK(strstr(line, "  ") == NULL, "double space in `%s`", line);
    CHECK(strncmp(line, "page_size=", 10) == 0, "line does not start with page_size=");

    /* Every key once: the kernel probes, then this architecture's CPU keys. */
    static const char *const kernel_keys[] = {"page_size",      "guard_install", "mseal",     "process_madvise", "process_madvise_free",
                                              "anon_name",      "populate_write", "thp",      "mthp",            "collapse",
                                              "thp_except_advised", "rseq",      "rseq_size", "mm_cid",          "slice_ext"};
#if defined(__aarch64__)
    static const char *const cpu_keys[] = {"mte", "mte3", "pac", "lse", "lse2", "bti", "sve", "cacheline", "l1d", "l2"};
#elif defined(__x86_64__)
    static const char *const cpu_keys[] = {"lam",  "uai",     "la57",      "la57_enabled", "cldemote", "waitpkg",
                                           "erms", "fsrm",    "avx512f",   "cacheline",    "l1d",      "l2"};
#else
#error "unsupported architecture"
#endif
    size_t nkernel = sizeof kernel_keys / sizeof kernel_keys[0];
    size_t ncpu = sizeof cpu_keys / sizeof cpu_keys[0];
    for (size_t i = 0; i < nkernel; i++)
        CHECK(has_key(line, kernel_keys[i]) == 1, "key %s appears %d times", kernel_keys[i], has_key(line, kernel_keys[i]));
    for (size_t i = 0; i < ncpu; i++)
        CHECK(has_key(line, cpu_keys[i]) == 1, "key %s appears %d times", cpu_keys[i], has_key(line, cpu_keys[i]));
    CHECK((size_t)tokens == nkernel + ncpu, "%d tokens, expected %zu", tokens, nkernel + ncpu);

    /* populate_write (plan Phase 2.5 task 11): yes/no, and usable as said:
     * a fresh private page is resident after one MADV_POPULATE_WRITE when
     * yes, and the advice is EINVAL when no. */
    {
        const char *v = strstr(line, " populate_write=") + 16;
        CHECK(strncmp(v, "yes", 3) == 0 || strncmp(v, "no", 2) == 0, "populate_write has value `%.8s`", v);
        long page = sysconf(_SC_PAGESIZE);
        void *p = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        REQUIRE(p != MAP_FAILED, "mmap");
        int rc = madvise(p, page, 23 /* MADV_POPULATE_WRITE */);
        unsigned char vec = 0;
        REQUIRE(mincore(p, page, &vec) == 0, "mincore");
        if (strncmp(v, "yes", 3) == 0)
            CHECK(rc == 0 && (vec & 1), "populate_write=yes but madvise rc %d (errno %d), resident %d", rc, errno, vec & 1);
        else
            CHECK(rc != 0, "populate_write=no but MADV_POPULATE_WRITE succeeded");
        munmap(p, page);
        printf("populate_write: %.*s (madvise rc %d)\n", (int)strcspn(v, " "), v, rc);
    }

    /* The THP keys (plan Phase 2.5 task 9): thp is a sysfs setting or none,
     * mthp a comma list of sizes or none, collapse unprobed/no/a size; the
     * default configuration never runs the collapse probe. */
    {
        const char *v = strstr(line, " thp=") + 5;
        CHECK(strncmp(v, "never", 5) == 0 || strncmp(v, "madvise", 7) == 0 || strncmp(v, "always", 6) == 0 ||
                  strncmp(v, "none", 4) == 0,
              "thp has value `%.12s`", v);
        v = strstr(line, " mthp=") + 6;
        CHECK((v[0] >= '1' && v[0] <= '9') || strncmp(v, "none", 4) == 0, "mthp has value `%.12s`", v);
        v = strstr(line, " collapse=") + 10;
        CHECK(strncmp(v, "unprobed", 8) == 0, "collapse has value `%.12s` under thp:never", v);
        /* Plan Phase 2.5 task 16: the prctl probe's answer, and the probe
         * left the process's policy alone (PR_GET_THP_DISABLE 0 unless the
         * parent set one, in which case the key says inherited). */
        v = strstr(line, " thp_except_advised=") + 20;
        CHECK(strncmp(v, "yes", 3) == 0 || strncmp(v, "no", 2) == 0 || strncmp(v, "inherited", 9) == 0,
              "thp_except_advised has value `%.12s`", v);
        long get = prctl(42 /* PR_GET_THP_DISABLE */, 0, 0, 0, 0);
        if (strncmp(v, "inherited", 9) == 0)
            CHECK(get > 0, "thp_except_advised=inherited but PR_GET_THP_DISABLE reads %ld", get);
        else
            CHECK(get == 0 || get == -1, "thp_except_advised=%.3s but PR_GET_THP_DISABLE reads %ld (the probe must restore 0)", v, get);
        printf("thp_except_advised: %.*s (PR_GET_THP_DISABLE %ld)\n", (int)strcspn(v, " "), v, get);
    }

    /* The rseq keys (plan Phase 2.5 task 8): rseq and mm_cid are yes/no,
     * rseq_size a byte count; mm_cid=yes needs rseq=yes and a size of at
     * least 28 (the field is at 24); a registration always reports at
     * least the original 20 bytes. */
    {
        const char *r = strstr(line, " rseq=") + 6;
        const char *sz = strstr(line, " rseq_size=") + 11;
        const char *mc = strstr(line, " mm_cid=") + 8;
        CHECK(strncmp(r, "yes", 3) == 0 || strncmp(r, "no", 2) == 0, "rseq has value `%.8s`", r);
        CHECK(sz[0] >= '0' && sz[0] <= '9', "rseq_size has value `%.8s`", sz);
        CHECK(strncmp(mc, "yes", 3) == 0 || strncmp(mc, "no", 2) == 0, "mm_cid has value `%.8s`", mc);
        long size = strtol(sz, NULL, 10);
        if (strncmp(mc, "yes", 3) == 0)
            CHECK(strncmp(r, "yes", 3) == 0 && size >= 28, "mm_cid=yes with rseq=%.3s rseq_size=%ld", r, size);
        if (strncmp(r, "yes", 3) == 0)
            CHECK(size >= 20, "rseq=yes with rseq_size=%ld", size);
        printf("rseq: rseq=%.3s rseq_size=%ld mm_cid=%.3s\n", r, size, mc);
    }

    /* The slice extension key (plan Phase 2.5 task 15): one of the probe's
     * words. `on` needs rseq=yes with a v2 registration (slice_ctrl ends at
     * 32); `no-rseq` needs rseq=no; `no-slice_ctrl` a size under 32;
     * `kernel-disabled` (the 7.0 GCE kernels: the option is compiled out)
     * a size of at least 32. The report's `Slice extension:` line agrees. */
    {
        const char *se = strstr(line, " slice_ext=") + 11;
        char word[24];
        size_t k = strcspn(se, " ");
        if (k >= sizeof word)
            k = sizeof word - 1;
        memcpy(word, se, k);
        word[k] = 0;
        const char *r = strstr(line, " rseq=") + 6;
        long size = strtol(strstr(line, " rseq_size=") + 11, NULL, 10);
        int known = !strcmp(word, "on") || !strcmp(word, "available") || !strcmp(word, "prctl-failed") ||
                    !strcmp(word, "kernel-disabled") || !strcmp(word, "no-slice_ctrl") || !strcmp(word, "no-rseq");
        CHECK(known, "slice_ext has value `%s`", word);
        if (!strcmp(word, "no-rseq"))
            CHECK(strncmp(r, "no", 2) == 0 || size == 0, "slice_ext=no-rseq with rseq=%.3s rseq_size=%ld", r, size);
        else if (!strcmp(word, "no-slice_ctrl"))
            CHECK(size > 0 && size < 32, "slice_ext=no-slice_ctrl with rseq_size=%ld", size);
        else
            CHECK(size >= 32, "slice_ext=%s with rseq_size=%ld", word, size);
        printf("slice_ext: %s\n", word);
    }

    /* The yes/no keys carry yes or no; the sizes a number with K/M or unknown. */
    for (size_t i = 0; i < ncpu; i++) {
        char pat[64];
        snprintf(pat, sizeof pat, "%s=", cpu_keys[i]);
        const char *at = strstr(line, pat);
        if (!at)
            continue;
        const char *v = at + strlen(pat);
        if (strcmp(cpu_keys[i], "cacheline") == 0 || strcmp(cpu_keys[i], "l1d") == 0 || strcmp(cpu_keys[i], "l2") == 0)
            CHECK((v[0] >= '1' && v[0] <= '9') || strncmp(v, "unknown", 7) == 0, "%s has value `%.12s`", cpu_keys[i], v);
        else
            CHECK(strncmp(v, "yes", 3) == 0 || strncmp(v, "no", 2) == 0, "%s has value `%.12s`", cpu_keys[i], v);
    }

    /* The report carries the same line. */
    stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
    msp(stats_cb, NULL, "");
    report[report_len] = 0;
    const char *at = strstr(report, "Kernel and CPU features: ");
    REQUIRE(at != NULL, "no `Kernel and CPU features:` line in malloc_stats_print");
    at += strlen("Kernel and CPU features: ");
    const char *end = strchr(at, '\n');
    REQUIRE(end != NULL, "unterminated features line in the report");
    CHECK((size_t)(end - at) == len && memcmp(at, line, len) == 0, "report line `%.*s` != challoc_features() `%s`",
          (int)(end - at), at, line);
    /* The `Slice extension:` line and the `slice_ext=` key tell one story:
     * `on` <-> `on (`; any other word <-> `off (`. */
    {
        const char *sl = strstr(report, "Slice extension: ");
        REQUIRE(sl != NULL, "no `Slice extension:` line in malloc_stats_print");
        const char *se = strstr(line, " slice_ext=") + 11;
        int on = strncmp(se, "on ", 3) == 0 || strncmp(se, "on\0", 3) == 0;
        if (on)
            CHECK(strncmp(sl + 17, "on (", 4) == 0, "slice_ext=on but `%.40s`", sl);
        else
            CHECK(strncmp(sl + 17, "off (", 5) == 0, "slice_ext=%.16s but `%.40s`", se, sl);
    }

    return finish("features");
}
