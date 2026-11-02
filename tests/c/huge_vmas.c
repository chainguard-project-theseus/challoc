/*
 * The VMAs the huge tier holds are bounded by its live runs (design 7;
 * hunt 943666c7, c-ec215453). A correct program places many huge runs,
 * frees them all and calls malloc_trim(0): /proc/self/maps must be back
 * near where it started, and the process must still be able to create a
 * thread and open a file. Before the fix a freed run's `PROT_NONE` guard
 * was never made read-write again, so in the fallback guard mode (kernels
 * before 6.13) and under `guards:false` (which was routed into it) every
 * huge run ever placed kept its own VMA: ~32k runs of 260 KiB left the
 * process at vm.max_map_count for good. The driver in
 * src/api/c_programs.rs runs one mode per process:
 *
 *   free     (any conf)  40000 runs of 260 KiB, one byte touched each, all
 *                        freed, trim, count, thread, fopen
 *   noguard              the same with madvise(MADV_GUARD_INSTALL |
 *                        MADV_GUARD_REMOVE) refused (EINVAL) by a seccomp
 *                        filter, as on a kernel before 6.13: the program
 *                        installs it and re-executes itself in `free` mode,
 *                        and checks challoc_features() says guard_install=no
 *   nopopulate           the same with MADV_POPULATE_WRITE refused as well,
 *                        so the run's first page is faulted in by a write
 *   mlock                up to 24 runs placed again in a freed span the
 *                        program mlock()s: the guard marker is refused on
 *                        a locked VMA and a `PROT_NONE` guard goes in
 *                        instead (`mixed_guards`), which a free must take
 *                        away as well. Fits the default RLIMIT_MEMLOCK
 *                        (8 MiB); skips below 10 runs' worth.
 *
 * At the limit malloc may return NULL while runs are live (a live guarded
 * run costs two VMAs in the fallback mode); that is not a failure here.
 */
#include "common.h"

#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <stddef.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <stdint.h>
#include <sys/resource.h>
#include <sys/syscall.h>

WEAK const char *challoc_features(void);

enum { RUN_SIZE = 260 * 1024, SLACK = 64 };

static long count_vmas(void)
{
    FILE *f = fopen("/proc/self/maps", "r");
    REQUIRE(f != NULL, "fopen /proc/self/maps: %s", strerror(errno));
    long n = 0;
    int c;
    while ((c = getc(f)) != EOF)
        n += c == '\n';
    fclose(f);
    return n;
}

static void *thr(void *a)
{
    return a;
}

static int churn(int nr)
{
    /* Warm up the tiers the counting itself uses before the baseline. */
    free(malloc(RUN_SIZE));
    long base = count_vmas();
    void **v = calloc((size_t)nr, sizeof *v);
    REQUIRE(v != NULL, "calloc");
    int n = 0;
    for (; n < nr; n++) {
        v[n] = malloc(RUN_SIZE);
        if (v[n] == NULL)
            break;
        ((volatile char *)v[n])[0] = 1;
    }
    long live = count_vmas();
    for (int i = 0; i < n; i++)
        free(v[i]);
    free(v);
    malloc_trim(0);
    long after = count_vmas();
    printf("%d runs placed: %ld VMAs before, %ld live, %ld after freeing them all\n", n, base, live, after);
    CHECK(n > 0, "no run placed");
    CHECK(after <= base + SLACK, "%ld VMAs after freeing every run (baseline %ld): the huge tier kept them", after,
          base);
    pthread_t t;
    int pe = pthread_create(&t, NULL, thr, NULL);
    CHECK(pe == 0, "pthread_create after freeing everything: %s", strerror(pe));
    if (pe == 0)
        pthread_join(t, NULL);
    FILE *f = fopen("/proc/self/status", "r");
    CHECK(f != NULL, "fopen after freeing everything: %s", strerror(errno));
    if (f)
        fclose(f);
    return finish("huge_vmas");
}

#if defined(__x86_64__)
#define NATIVE_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define NATIVE_ARCH AUDIT_ARCH_AARCH64
#endif

static int noguard(char **argv, int nopopulate)
{
#ifndef NATIVE_ARCH
    skip_note("huge_vmas", "no seccomp arch for this target");
    printf("ok huge_vmas\n");
    return 0;
#else
    /* madvise(.., MADV_GUARD_INSTALL=102 | MADV_GUARD_REMOVE=103), and
     * with `nopopulate` MADV_POPULATE_WRITE=23 too (otherwise 102 again),
     * -> EINVAL. */
    unsigned third = nopopulate ? 23 : 102;
    struct sock_filter prog[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, NATIVE_ARCH, 0, 7),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_madvise, 0, 5),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 102, 2, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 103, 1, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, third, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EINVAL),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog fp = { .len = sizeof prog / sizeof prog[0], .filter = prog };
    REQUIRE(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0, "PR_SET_NO_NEW_PRIVS: %s", strerror(errno));
    REQUIRE(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &fp) == 0, "PR_SET_SECCOMP: %s", strerror(errno));
    char *args[] = { argv[0], "free-noguard", NULL };
    execv("/proc/self/exe", args);
    REQUIRE(0, "execv: %s", strerror(errno));
    return 1;
#endif
}

static int locked(void)
{
    /* Runs inside the default RLIMIT_MEMLOCK (8 MiB): lock only the span
     * they occupy, not the whole process. */
    struct rlimit rl;
    REQUIRE(getrlimit(RLIMIT_MEMLOCK, &rl) == 0, "getrlimit: %s", strerror(errno));
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t per = RUN_SIZE + page;
    long n = 24;
    if (rl.rlim_cur != RLIM_INFINITY && (rl.rlim_cur < (512u << 10) || (long)((rl.rlim_cur - (512u << 10)) / per) < n))
        n = rl.rlim_cur < (512u << 10) ? 0 : (long)((rl.rlim_cur - (512u << 10)) / per);
    if (n < 10) {
        skip_note("huge_vmas", "RLIMIT_MEMLOCK %lu fits fewer than 10 runs", (unsigned long)rl.rlim_cur);
        printf("ok huge_vmas\n");
        return 0;
    }
    free(malloc(RUN_SIZE));
    void *v[24];
    uintptr_t lo = UINTPTR_MAX, hi = 0;
    for (long i = 0; i < n; i++) {
        v[i] = malloc(RUN_SIZE);
        REQUIRE(v[i] != NULL, "malloc: %s", strerror(errno));
        uintptr_t a = (uintptr_t)v[i];
        lo = a < lo ? a : lo;
        hi = a + per > hi ? a + per : hi;
    }
    for (long i = 0; i < n; i++)
        free(v[i]);
    /* The freed span is read-write and lowest-address first fit puts the
     * same runs back in it; locked, the kernel refuses their guard markers
     * and each guard becomes a PROT_NONE page instead. */
    if (mlock((void *)lo, hi - lo) != 0) {
        skip_note("huge_vmas", "mlock of %zu bytes: %s", (size_t)(hi - lo), strerror(errno));
        printf("ok huge_vmas\n");
        return 0;
    }
    long base = count_vmas();
    for (long i = 0; i < n; i++) {
        v[i] = malloc(RUN_SIZE);
        REQUIRE(v[i] != NULL, "malloc: %s", strerror(errno));
        CHECK((uintptr_t)v[i] >= lo && (uintptr_t)v[i] + RUN_SIZE <= hi, "run %ld left the locked span", i);
        ((volatile char *)v[i])[0] = 1;
    }
    long live = count_vmas();
    for (long i = 0; i < n; i++)
        free(v[i]);
    malloc_trim(0);
    long after = count_vmas();
    munlock((void *)lo, hi - lo);
    printf("%ld runs placed in a locked span: %ld VMAs before, %ld live, %ld after freeing them all\n", n, base, live,
           after);
    CHECK(after <= base + 4, "%ld VMAs after freeing every locked run (baseline %ld): their PROT_NONE guards stayed",
          after, base);
    return finish("huge_vmas");
}

int main(int argc, char **argv)
{
    require_challoc();
    const char *mode = argc > 1 ? argv[1] : "free";
    if (!strcmp(mode, "free"))
        return churn(40000);
    if (!strcmp(mode, "free-noguard")) {
        REQUIRE(challoc_features != NULL, "challoc_features is not exported");
        const char *line = challoc_features();
        printf("features: %s\n", line);
        REQUIRE(strstr(line, "guard_install=no") != NULL, "the filter did not hide MADV_GUARD_INSTALL");
        return churn(40000);
    }
    if (!strcmp(mode, "noguard"))
        return noguard(argv, 0);
    if (!strcmp(mode, "nopopulate"))
        return noguard(argv, 1);
    if (!strcmp(mode, "mlock"))
        return locked();
    fprintf(stderr, "unknown mode %s\n", mode);
    return 2;
}
