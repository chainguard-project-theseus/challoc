/*
 * The cgroup v2 files of this process, for the cgroup programs in
 * tests/c/ (cgroup.c, cgroup_policy.c, cgroup_churn.c, cgroup_lazy.c,
 * cgroup_oscillate.c), on top of common.h.
 */
#ifndef CHALLOC_TESTS_CGROUP_H
#define CHALLOC_TESTS_CGROUP_H

#include "common.h"

/* The cgroup directory (the cgroup2 mount point plus the `0::` path), set
 * by `find_cgroup`; empty until it finds one. */
static char cgroup_dir[8192];

/* Find the cgroup directory the way the library finds it (design 11):
 * the `0::/` line of /proc/self/cgroup under the first cgroup2 mount of
 * /proc/self/mountinfo. Returns 1 with `cgroup_dir` set, or 0 when there
 * is no unified path or no cgroup2 mount, or when either file cannot be
 * opened (fatal instead with `required`). */
static inline int find_cgroup(bool required)
{
    char line[4096], path[4096] = "", mount[4096] = "";
    FILE *f = fopen("/proc/self/cgroup", "r");
    REQUIRE(f != NULL || !required, "open /proc/self/cgroup");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f))
        if (strncmp(line, "0::/", 4) == 0) {
            snprintf(path, sizeof path, "%s", line + 3);
            path[strcspn(path, "\n")] = 0;
        }
    fclose(f);
    f = fopen("/proc/self/mountinfo", "r");
    REQUIRE(f != NULL || !required, "open /proc/self/mountinfo");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f)) {
        char *sep = strstr(line, " - ");
        if (sep && strncmp(sep + 3, "cgroup2 ", 8) == 0 && !mount[0]) {
            /* field 5 (0-based 4) of the part before the separator */
            *sep = 0;
            char *save = NULL, *tok = strtok_r(line, " ", &save);
            for (int i = 0; tok && i < 4; i++)
                tok = strtok_r(NULL, " ", &save);
            if (tok)
                snprintf(mount, sizeof mount, "%s", tok);
        }
    }
    fclose(f);
    if (!path[0] || !mount[0])
        return 0;
    snprintf(cgroup_dir, sizeof cgroup_dir, "%s%s", mount, strcmp(path, "/") == 0 ? "" : path);
    return 1;
}

/* A one-number file of the cgroup directory: its value, -2 for "max", -1
 * without a cgroup or when the file cannot be read. */
static inline long long read_cgroup_ll(const char *file)
{
    if (!cgroup_dir[0])
        return -1;
    char p[sizeof cgroup_dir + 64];
    snprintf(p, sizeof p, "%s/%s", cgroup_dir, file);
    FILE *f = fopen(p, "r");
    if (!f)
        return -1;
    char buf[64];
    long long v = -1;
    if (fgets(buf, sizeof buf, f))
        v = strncmp(buf, "max", 3) == 0 ? -2 : atoll(buf);
    fclose(f);
    return v;
}

/* The leading number of `<dir>/<file>`, 0 when it cannot be read. */
static inline size_t read_cgroup_u64(const char *dir, const char *file)
{
    char p[sizeof cgroup_dir + 64];
    snprintf(p, sizeof p, "%s/%s", dir, file);
    FILE *f = fopen(p, "r");
    if (!f)
        return 0;
    unsigned long long v = 0;
    if (fscanf(f, "%llu", &v) != 1)
        v = 0;
    fclose(f);
    return (size_t)v;
}

/* The kernel's own memory.current for this cgroup, or 0 without one. */
static inline size_t kernel_current(void)
{
    return read_cgroup_u64(cgroup_dir, "memory.current");
}

/* A `read_cgroup_ll` value for a log line: "<n> MiB", "max" or "n/a". */
static inline const char *fmt_mib(long long v, char *buf, size_t n)
{
    if (v == -2)
        snprintf(buf, n, "max");
    else if (v < 0)
        snprintf(buf, n, "n/a");
    else
        snprintf(buf, n, "%lld MiB", v / (1024LL * 1024));
    return buf;
}

#endif
