/*
 * The fork cost of a Valkey-shaped heap (plan Phase 2.5 task 9): what a
 * BGSAVE pays for the allocator's page geometry. The program allocates
 * `MiB` (default 64; the measurements use 500) of values drawn uniformly
 * from 16..1024 bytes, each behind a 16-byte object as Valkey's are,
 * touches every byte, forks, and measures in the parent:
 *
 *   fork_us       the fork() call itself (page tables copied);
 *   burst_ms      a write burst over every value while the child is alive
 *                 (one byte per value: every page holding a value is
 *                 written once, so it is copied);
 *   burst_faults  the parent's minor faults during the burst (getrusage);
 *   total_ms      fork() to the child's exit, the child leaving as soon as
 *                 the burst is over;
 *   maxrss_kb     the parent's peak RSS.
 *
 * The child touches nothing. Under `thp:never` every fault copies one
 * page; under `thp:auto` (64 KiB folios) and `thp:always` (2 MiB pages)
 * the kernel's copy-on-write of a large folio decides the cost, which is
 * what the measurement records. The program passes when the child exits
 * cleanly and the burst faulted at least a quarter of the pages it wrote
 * (proof that the copy-on-write happened while the child was alive).
 * Everything else is printed for the results file; nothing here is a
 * criterion, the criterion (fork time within 5% of `never`) is applied
 * to the printed numbers. A bench probe, not a test (it left `make test`
 * with the 2026-09-20 maintainability pass: its only check, that the
 * burst faulted, is the kernel's copy-on-write, not the allocator's;
 * fork correctness is tests/c/fork_alloc.c and fork_signal.c). Build and
 * run in the dev container:
 *
 *   sh scripts/ctest-cc.sh bench/probes/fork_cost.c -o target/fork_cost
 *   LD_PRELOAD=target/release/libchalloc.so target/fork_cost 500
 *   MALLOC_CONF=thp:auto LD_PRELOAD=... target/fork_cost 500
 */
#include "common.h"
#include <sys/resource.h>

/* Milliseconds with sub-millisecond precision (fork_us needs it). */
static double now_msf(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

int main(int argc, char **argv)
{
    require_challoc();
    size_t mib = argc > 1 ? (size_t)strtoul(argv[1], NULL, 10) : 64;
    REQUIRE(mib >= 1 && mib <= 65536, "MiB out of range");
    size_t budget = mib << 20;
    size_t cap = budget / 16 + 16; /* worst case: every value 16 bytes */
    unsigned char **vals = malloc(cap * sizeof *vals);
    REQUIRE(vals, "pointer array");
    uint64_t rng = 0x5EED0000 + mib;
    size_t n = 0, live = 0;
    while (live < budget && n < cap) {
        size_t sz = 16 + (size_t)(rng_next(&rng) % (1024 - 16 + 1));
        void *obj = malloc(16);
        unsigned char *v = malloc(sz);
        REQUIRE(obj && v, "malloc");
        memset(obj, 1, 16);
        memset(v, (int)(n & 0xff) | 1, sz);
        vals[n++] = v;
        live += sz + 16;
        sink = obj;
    }
    printf("fork_cost: live %zu MiB in %zu values (16..1024 bytes)\n", live >> 20, n);

    int pipefd[2];
    REQUIRE(pipe(pipefd) == 0, "pipe");
    struct rusage before, after;
    REQUIRE(getrusage(RUSAGE_SELF, &before) == 0, "getrusage");
    double t0 = now_msf();
    pid_t pid = fork();
    double t1 = now_msf();
    REQUIRE(pid >= 0, "fork");
    if (pid == 0) {
        close(pipefd[1]);
        char c;
        while (read(pipefd[0], &c, 1) < 0)
            ;
        _exit(0);
    }
    close(pipefd[0]);
    /* The write burst: one byte per value while the child holds the pages. */
    double t2 = now_msf();
    for (size_t i = 0; i < n; i++)
        vals[i][0] ^= 0x80;
    double t3 = now_msf();
    REQUIRE(getrusage(RUSAGE_SELF, &after) == 0, "getrusage");
    REQUIRE(write(pipefd[1], "x", 1) == 1, "pipe write");
    int status = 0;
    REQUIRE(waitpid(pid, &status, 0) == pid, "waitpid");
    double t4 = now_msf();
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child status %d", status);

    long faults = after.ru_minflt - before.ru_minflt;
    long page = sysconf(_SC_PAGESIZE);
    /* Values are packed in slabs, so distinct pages written is about
     * live / page; the check asks for a quarter of that. */
    long pages = (long)(live / (size_t)page);
    CHECK(faults >= pages / 4, "burst faulted %ld times for %ld pages written (child alive)", faults, pages);
    printf("fork_cost: fork_us=%.0f burst_ms=%.1f burst_faults=%ld pages=%ld total_ms=%.1f maxrss_kb=%ld\n",
           (t1 - t0) * 1e3, t3 - t2, faults, pages, t4 - t0, after.ru_maxrss);
    for (size_t i = 0; i < n; i++)
        free(vals[i]);
    free(vals);
    return finish("fork_cost");
}
