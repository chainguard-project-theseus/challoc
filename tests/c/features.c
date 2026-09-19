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
    CHECK(len > 0 && len < 512, "line length %zu", len);
    CHECK(strchr(line, '\n') == NULL, "line contains a newline");
    printf("features: %s\n", line);

    /* Well formed: tokens separated by single spaces, each key=value. */
    char copy[512];
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
    static const char *const kernel_keys[] = {"page_size", "guard_install", "mseal", "process_madvise", "anon_name"};
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

    return finish("features");
}
