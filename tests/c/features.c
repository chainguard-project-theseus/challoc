/*
 * The feature report (design 16 and 17).
 *
 * challoc_features() returns a static NUL-terminated line of key=value
 * pairs: the kernel probes, then pac. The same bytes appear after
 * "Kernel and CPU features: " in malloc_stats_print, so a program can read
 * the host's floor and ceiling either way. The program prints the line
 * (so a test log shows what each host offers) and checks: the pointer is
 * stable, the line is well formed, every key
 * is present exactly once, and the report's line is identical.
 *
 * The symbol is reached through a weak declaration so the program links
 * against libc alone and fails bare in require_challoc.
 */
#include "common.h"

static char report[1 << 18];

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

    /* Every key once. */
    static const char *const keys[] = {"page_size", "guard_install", "mseal", "anon_name", "thp",
                                       "mthp",      "rseq",          "rseq_size", "pac"};
    size_t nkeys = sizeof keys / sizeof keys[0];
    for (size_t i = 0; i < nkeys; i++)
        CHECK(has_key(line, keys[i]) == 1, "key %s appears %d times", keys[i], has_key(line, keys[i]));
    CHECK((size_t)tokens == nkeys, "%d tokens, expected %zu", tokens, nkeys);

    /* The THP keys: thp is a sysfs setting or none,
     * mthp a comma list of sizes or none. */
    {
        const char *v = strstr(line, " thp=") + 5;
        CHECK(strncmp(v, "never", 5) == 0 || strncmp(v, "madvise", 7) == 0 || strncmp(v, "always", 6) == 0 ||
                  strncmp(v, "none", 4) == 0,
              "thp has value `%.12s`", v);
        v = strstr(line, " mthp=") + 6;
        CHECK((v[0] >= '1' && v[0] <= '9') || strncmp(v, "none", 4) == 0, "mthp has value `%.12s`", v);
    }

    /* The rseq keys: rseq is yes/no, rseq_size a byte count; a
     * registration always reports at least the original 20 bytes. */
    {
        const char *r = strstr(line, " rseq=") + 6;
        const char *sz = strstr(line, " rseq_size=") + 11;
        CHECK(strncmp(r, "yes", 3) == 0 || strncmp(r, "no", 2) == 0, "rseq has value `%.8s`", r);
        CHECK(sz[0] >= '0' && sz[0] <= '9', "rseq_size has value `%.8s`", sz);
        long size = strtol(sz, NULL, 10);
        if (strncmp(r, "yes", 3) == 0)
            CHECK(size >= 20, "rseq=yes with rseq_size=%ld", size);
        printf("rseq: rseq=%.3s rseq_size=%ld\n", r, size);
    }

    /* pac: yes or no, never yes off aarch64. */
    {
        const char *v = strstr(line, " pac=") + 5;
        CHECK(strcmp(v, "yes") == 0 || strcmp(v, "no") == 0, "pac has value `%.12s`", v);
#if !defined(__aarch64__)
        CHECK(strcmp(v, "no") == 0, "pac=%s off aarch64", v);
#endif
    }

    /* The report carries the same line. */
    stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
    stats_report(msp, "", report, sizeof report);
    const char *at = strstr(report, "Kernel and CPU features: ");
    REQUIRE(at != NULL, "no `Kernel and CPU features:` line in malloc_stats_print");
    at += strlen("Kernel and CPU features: ");
    const char *end = strchr(at, '\n');
    REQUIRE(end != NULL, "unterminated features line in the report");
    CHECK((size_t)(end - at) == len && memcmp(at, line, len) == 0, "report line `%.*s` != challoc_features() `%s`",
          (int)(end - at), at, line);
    return finish("features");
}
