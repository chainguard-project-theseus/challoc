/*
 * challoc's fork epochs from C (docs/design.md section 5, "Fork epochs",
 * and 13.7): challoc_cow_begin, challoc_cow_end and challoc_cow_stat,
 * declared in <challoc/challoc.h> (include/, on the include path through
 * -I). Run under LD_PRELOAD=libchalloc.so by src/api/c_programs.rs.
 *
 * Claims, around a real fork(2) whose child is alive while the parent
 * overwrites half of its blocks (each free followed by a malloc of the same
 * size, half the frees plain and half sized, as Valkey's SET does):
 *   - begin with flags other than 0 is EINVAL; begin and end nest;
 *   - while the epoch is open no malloc returns a block that existed before
 *     it (none is handed out again while the child may share its page),
 *     and the deflected frees are counted;
 *   - the child starts with no epoch open (the parent's is not its own);
 *   - end closes it; a block allocated during the epoch then reads as worth
 *     moving (experimental.utilization.batch_query reports one region in
 *     use, get_defrag_hint 1) while the holes below are open, and by the
 *     usual rule once they are refilled;
 *   - past the cap (the slab bytes in use at begin, at least 64 MiB) the
 *     floors stop holding and the epoch counts as expired;
 *   - with cow_epoch:false (mode "off") begin is ENOTSUP and end is 0.
 */
#include "common.h"
/* Weak, as common.h declares the other challoc entry points: the program
 * links against nothing but libc and runs under the preload. */
extern int challoc_cow_begin(unsigned) WEAK;
extern int challoc_cow_end(void) WEAK;
extern unsigned long long challoc_cow_stat(unsigned) WEAK;
#include <challoc/challoc.h>

#ifndef CHALLOC_COW
#error "<challoc/challoc.h> must define CHALLOC_COW"
#endif

extern int get_defrag_hint(void *) WEAK;

enum { N = 40000, SZ = 64 };

static void *old[N], *cur[N], *sorted[N];

static bool was_old(void *p)
{
    return bsearch(&p, sorted, N, sizeof sorted[0], cmp_ptr) != NULL;
}

/* `p`'s (nfree, nregs, size) from the utilisation query Valkey reads. */
static void util(void *p, size_t out[3])
{
    size_t mib[3], len = 3;
    REQUIRE(mallctlnametomib("experimental.utilization.batch_query", mib, &len) == 0, "util mib");
    size_t outlen = 3 * sizeof(size_t);
    REQUIRE(mallctlbymib(mib, len, out, &outlen, &p, sizeof p) == 0, "batch_query");
}

static void off(void)
{
    CHECK(challoc_cow_begin(0) == ENOTSUP, "cow_epoch:false: begin must be ENOTSUP");
    CHECK(challoc_cow_end() == 0, "end with no epoch open is 0");
    CHECK(challoc_cow_stat(CHALLOC_COW_OPEN) == 0, "no epoch open");
}

int main(int argc, char **argv)
{
    require_challoc();
    if (argc > 1 && strcmp(argv[1], "off") == 0) {
        off();
        return finish("cow_epoch");
    }

    for (int i = 0; i < N; i++) {
        old[i] = cur[i] = malloc(SZ);
        REQUIRE(old[i] != NULL, "malloc");
        memset(old[i], 0x5a, SZ);
    }
    memcpy(sorted, old, sizeof old);
    qsort(sorted, N, sizeof sorted[0], cmp_ptr);

    CHECK(challoc_cow_begin(1) == EINVAL, "begin(1) must be EINVAL");
    CHECK(challoc_cow_stat(CHALLOC_COW_OPEN) == 0, "a refused begin opens nothing");
    CHECK(challoc_cow_end() == 0, "end with no epoch open is 0");

    REQUIRE(challoc_cow_begin(0) == 0, "begin");
    CHECK(challoc_cow_stat(CHALLOC_COW_OPEN) == 1 && challoc_cow_stat(CHALLOC_COW_IN_FORCE) == 1, "epoch open");
    CHECK(challoc_cow_begin(0) == 0 && challoc_cow_stat(CHALLOC_COW_OPEN) == 2, "nested begin");
    CHECK(challoc_cow_end() == 0 && challoc_cow_stat(CHALLOC_COW_OPEN) == 1, "nested end");
    CHECK(challoc_cow_stat(CHALLOC_COW_IN_FORCE) == 1, "the outer epoch still holds");
    CHECK(challoc_cow_stat(CHALLOC_COW_CAP) >= (64u << 20), "cap at least 64 MiB");

    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork");
    if (pid == 0) {
        /* The snapshotting child: reads its copy and exits. The parent's
         * epoch is not its own: it starts with none open. */
        unsigned long sum = 0;
        for (int i = 0; i < N; i++)
            sum += ((unsigned char *)old[i])[0];
        if (challoc_cow_stat(CHALLOC_COW_OPEN) != 0 || challoc_cow_stat(CHALLOC_COW_IN_FORCE) != 0)
            _exit(4);
        usleep(100 * 1000);
        _exit(sum == (unsigned long)N * 0x5a ? 0 : 3);
    }

    /* The parent overwrites every other block while the child runs. */
    int reused = 0;
    for (int i = 0; i < N; i += 2) {
        if (i % 4 == 0)
            free(cur[i]);
        else
            sdallocx(cur[i], SZ, 0);
        void *p = malloc(SZ);
        REQUIRE(p != NULL, "malloc in the epoch");
        if (was_old(p))
            reused++;
        memset(p, 0xa5, SZ);
        cur[i] = p;
    }
    CHECK(reused == 0, "%d blocks of before the epoch handed out again during it", reused);
    CHECK(challoc_cow_stat(CHALLOC_COW_DEFLECTED) >= N / 2, "deflected frees %llu < %d",
          challoc_cow_stat(CHALLOC_COW_DEFLECTED), N / 2);
    CHECK(challoc_cow_stat(CHALLOC_COW_CLAIMED) >= (unsigned long long)(N / 2) * SZ, "claimed %llu",
          challoc_cow_stat(CHALLOC_COW_CLAIMED));

    int status = 0;
    REQUIRE(waitpid(pid, &status, 0) == pid, "waitpid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child read its snapshot (status %#x)", status);
    CHECK(challoc_cow_end() == 0, "end");
    CHECK(challoc_cow_stat(CHALLOC_COW_OPEN) == 0 && challoc_cow_stat(CHALLOC_COW_IN_FORCE) == 0, "epoch closed");
    CHECK(challoc_cow_stat(CHALLOC_COW_DRAINING) > 0, "a size class drains");

    /* The drain: a block of the epoch moves back below while holes are
     * open there (each flush of the FOREIGN cache returned deflected frees to
     * their slabs). */
    void *fresh = cur[N - 2];
    size_t u[3];
    util(fresh, u);
    CHECK(u[1] > 1 && u[0] == u[1] - 1, "drain: nfree %zu nregs %zu, want nregs - 1", u[0], u[1]);
    CHECK(get_defrag_hint(fresh) == 1, "drain: get_defrag_hint wants the move");

    /* Refill every hole: the drain is over and the usual rule answers (the
     * epoch's slabs are full). */
    static void *fill[N];
    for (int i = 0; i < N; i++)
        fill[i] = malloc(SZ);
    CHECK(get_defrag_hint(fresh) == 0, "after the holes are refilled the full slab is not worth moving");
    for (int i = 0; i < N; i++)
        free(fill[i]);

    /* The cap: allocate past it under a new epoch. */
    REQUIRE(challoc_cow_begin(0) == 0, "begin 2");
    unsigned long long cap = challoc_cow_stat(CHALLOC_COW_CAP);
    size_t n = (size_t)(cap / SZ) + 4096;
    void **many = malloc(n * sizeof *many);
    REQUIRE(many != NULL, "array");
    for (size_t i = 0; i < n; i++)
        many[i] = malloc(SZ);
    CHECK(challoc_cow_stat(CHALLOC_COW_IN_FORCE) == 0, "past the cap the floors stop holding");
    CHECK(challoc_cow_stat(CHALLOC_COW_EXPIRED) == 1, "expired %llu", challoc_cow_stat(CHALLOC_COW_EXPIRED));
    CHECK(challoc_cow_stat(CHALLOC_COW_OPEN) == 1, "the epoch stays open until end");
    for (size_t i = 0; i < n; i++)
        free(many[i]);
    free(many);
    CHECK(challoc_cow_end() == 0, "end 2");
    CHECK(challoc_cow_stat(CHALLOC_COW_EPOCHS) == 2, "two outermost epochs begun");

    for (int i = 0; i < N; i++)
        free(cur[i]);
    return finish("cow_epoch");
}
