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
 *   mlock                mlockall(MCL_CURRENT | MCL_FUTURE), then 300 runs:
 *                        the guard marker is refused on locked VMAs and a
 *                        `PROT_NONE` guard goes in instead (`mixed_guards`),
 *                        which a free must take away as well. Skips when
 *                        the memlock limit cannot be raised to fit.
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

static int noguard(char **argv)
{
#ifndef NATIVE_ARCH
    skip_note("huge_vmas", "no seccomp arch for this target");
    printf("ok huge_vmas\n");
    return 0;
#else
    /* madvise(.., MADV_GUARD_INSTALL=102 | MADV_GUARD_REMOVE=103) -> EINVAL. */
    struct sock_filter prog[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, NATIVE_ARCH, 0, 6),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_madvise, 0, 4),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 102, 1, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 103, 0, 1),
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
    enum { N = 300 };
    /* The runs, their guards' neighbours and the other tiers' pages. */
    rlim_t want = (rlim_t)N * (RUN_SIZE + 64 * 1024) + (128u << 20);
    struct rlimit rl;
    if (getrlimit(RLIMIT_MEMLOCK, &rl) == 0 && rl.rlim_cur != RLIM_INFINITY && rl.rlim_cur < want) {
        rl.rlim_cur = want;
        if (rl.rlim_max != RLIM_INFINITY && rl.rlim_max < want)
            rl.rlim_max = want;
        if (setrlimit(RLIMIT_MEMLOCK, &rl) != 0) {
            skip_note("huge_vmas", "cannot raise RLIMIT_MEMLOCK to %lu: %s", (unsigned long)want, strerror(errno));
            printf("ok huge_vmas\n");
            return 0;
        }
    }
    if (mlockall(MCL_CURRENT | MCL_FUTURE) != 0) {
        skip_note("huge_vmas", "mlockall: %s", strerror(errno));
        printf("ok huge_vmas\n");
        return 0;
    }
    return churn(N);
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
        return noguard(argv);
    if (!strcmp(mode, "mlock"))
        return locked();
    fprintf(stderr, "unknown mode %s\n", mode);
    return 2;
}
