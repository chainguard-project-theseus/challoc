/*
 * Resource failures under LD_PRELOAD (docs/design.md 14.1): a correct
 * program under a resource limit gets memory or NULL with ENOMEM, never a
 * `challoc:` abort, and once the pressure lifts it allocates again (no
 * failure is remembered). The driver in src/api/c_programs.rs runs one
 * mode per process; a mode whose limit must be in force before the
 * allocator starts re-executes this program under it (`run_child`), so
 * the child's first malloc is challoc's init.
 *
 *   data_init   RLIMIT_DATA of 32 MiB from the start, under the ~85 MiB
 *               the eager metadata tables would take (design 2): a 15 MB
 *               working set runs; then the soft limit is cut to the current
 *               VmData, a 64 MiB malloc returns NULL/ENOMEM, and after
 *               the limit is raised again it succeeds
 *   as_min      RLIMIT_AS just above the documented minimum layout
 *               (design 2: about 435 GiB at quantum 16, 468 at 8): 16
 *               children in a row all start (the reservation order is
 *               random per process, so one start would not show that
 *               every order fits)
 */
#include "common.h"

#include <sys/resource.h>

#define MIB ((size_t)1 << 20)
#define GIB ((size_t)1 << 30)

static const char *self_path;

/* VmData of this process, in bytes. */
static size_t vm_data(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    REQUIRE(f, "open /proc/self/status: %s", strerror(errno));
    char line[256];
    size_t kb = 0;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "VmData: %zu kB", &kb) == 1)
            break;
    fclose(f);
    REQUIRE(kb > 0, "no VmData line");
    return kb * 1024;
}

static void set_soft(int res, rlim_t soft)
{
    struct rlimit r;
    REQUIRE(getrlimit(res, &r) == 0, "getrlimit: %s", strerror(errno));
    r.rlim_cur = soft;
    REQUIRE(setrlimit(res, &r) == 0, "setrlimit(%d, %llu): %s", res, (unsigned long long)soft,
            strerror(errno));
}

/* Run this program as `<self> <mode>` under soft limit `soft` of `res`;
 * the child's exit status, or -signal when it was killed. */
static int run_child(const char *mode, int res, rlim_t soft)
{
    fflush(NULL);
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork: %s", strerror(errno));
    if (pid == 0) {
        set_soft(res, soft);
        execl(self_path, self_path, mode, (char *)NULL);
        _exit(127);
    }
    int st;
    REQUIRE(waitpid(pid, &st, 0) == pid, "waitpid: %s", strerror(errno));
    return WIFEXITED(st) ? WEXITSTATUS(st) : -WTERMSIG(st);
}

/* A small correct program: 64 x 100 KB, 1000 small blocks, one 8 MiB
 * buffer, each filled and checked, then freed. */
static void workload(void)
{
    void *mid[64], *small[1000];
    for (int i = 0; i < 64; i++) {
        mid[i] = malloc(100000);
        REQUIRE(mid[i], "malloc(100000) #%d: %s", i, strerror(errno));
        fill(mid[i], 100000, (uint32_t)i);
    }
    for (int i = 0; i < 1000; i++) {
        small[i] = malloc(16 + (size_t)(i % 200));
        REQUIRE(small[i], "malloc small #%d: %s", i, strerror(errno));
        fill(small[i], 16, (uint32_t)i);
    }
    void *big = malloc(8 * MIB);
    REQUIRE(big, "malloc(8 MiB): %s", strerror(errno));
    fill(big, 8 * MIB, 7);
    CHECK(check_fill(big, 8 * MIB, 7), "8 MiB buffer changed");
    for (int i = 0; i < 64; i++) {
        CHECK(check_fill(mid[i], 100000, (uint32_t)i), "100 KB #%d changed", i);
        free(mid[i]);
    }
    for (int i = 0; i < 1000; i++) {
        CHECK(check_fill(small[i], 16, (uint32_t)i), "small #%d changed", i);
        free(small[i]);
    }
    free(big);
}

static int child_data_init(void)
{
    require_challoc();
    workload();
    /* No room left: NULL with ENOMEM, not an abort. */
    struct rlimit r;
    REQUIRE(getrlimit(RLIMIT_DATA, &r) == 0, "getrlimit");
    set_soft(RLIMIT_DATA, vm_data());
    errno = 0;
    void *p = malloc(64 * MIB);
    CHECK(p == NULL && errno == ENOMEM, "malloc(64 MiB) at the limit = %p errno %d", p, errno);
    free(p);
    /* The limit lifted: the same request succeeds. */
    set_soft(RLIMIT_DATA, r.rlim_max);
    p = malloc(64 * MIB);
    CHECK(p != NULL, "malloc(64 MiB) after the limit was raised: %s", strerror(errno));
    if (p)
        fill(p, 64 * MIB, 3);
    free(p);
    workload();
    return finish("resource child");
}

static int child_as_min(void)
{
    require_challoc();
    workload();
    return finish("resource child");
}

static void parent_data_init(void)
{
    int rc = run_child("child_data_init", RLIMIT_DATA, 32 * MIB);
    CHECK(rc == 0, "child under RLIMIT_DATA = 32 MiB: %s %d", rc < 0 ? "signal" : "exit",
          rc < 0 ? -rc : rc);
}

static void parent_as_min(void)
{
    /* design 2's minimum layout plus 2 GiB for the program itself. */
    size_t gib = QUANTUM == 8 ? 469 : 437;
    for (int i = 0; i < 16; i++) {
        int rc = run_child("child_as_min", RLIMIT_AS, gib * GIB);
        CHECK(rc == 0, "run %d under RLIMIT_AS = %zu GiB: %s %d", i, gib, rc < 0 ? "signal" : "exit",
              rc < 0 ? -rc : rc);
    }
}

int main(int argc, char **argv)
{
    self_path = "/proc/self/exe";
    const char *mode = argc > 1 ? argv[1] : "";
    if (!strcmp(mode, "child_data_init"))
        return child_data_init();
    if (!strcmp(mode, "child_as_min"))
        return child_as_min();
    require_challoc();
    if (!strcmp(mode, "data_init"))
        parent_data_init();
    else if (!strcmp(mode, "as_min"))
        parent_as_min();
    else
        REQUIRE(0, "unknown mode '%s'", mode);
    return finish("resource");
}
