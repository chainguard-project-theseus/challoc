/*
 * secure_conf: the shape of CVE-2023-4911 (glibc's GLIBC_TUNABLES, parsed
 * by a setuid program's loader) end to end. In a secure-execution process
 * (AT_SECURE set) challoc must read neither MALLOC_CONF nor CHALLOC_CONF
 * (config::load, theorem_secure_conf in src/config.rs). The driver,
 * `c_cve_2023_4911` in src/api/c_programs.rs, links this program against
 * libchalloc.so with an absolute run path (the runtime linker drops
 * LD_PRELOAD paths in a secure-execution process) and runs it:
 *
 *   secure_conf probe        prints `AT_SECURE=<n>` and nothing else, so
 *                            the driver can tell whether the kernel gave
 *                            its setgid copy secure execution;
 *   secure_conf SECURE TCACHE
 *                            requires getauxval(AT_SECURE) to be SECURE
 *                            (0 or 1) and challoc to serve malloc, then
 *                            checks that opt.tcache reads TCACHE (0 or 1);
 *                            with SECURE 1 it also checks MALLOC_CONF and
 *                            CHALLOC_CONF are still in the environment,
 *                            so it is challoc, not the runtime linker,
 *                            that left them unread. Then it allocates and
 *                            frees a few thousand blocks and prints
 *                            `ok secure_conf`.
 *
 * Under MALLOC_CONF=abort_conf:true,no_such_key:1,tcache:false the plain
 * copy aborts on its first allocation (the driver's control), and the
 * setgid copy must not: opt.tcache keeps its default (true) and stderr
 * carries no `challoc:` line.
 */
#include "common.h"

#include <sys/auxv.h>

static void churn(void)
{
    enum { N = 4096 };
    void *volatile blocks[N];
    for (int i = 0; i < N; i++) {
        size_t size = (size_t)(i % 300) + 1;
        blocks[i] = malloc(size);
        REQUIRE(blocks[i] != NULL, "malloc(%zu) failed", size);
        memset(blocks[i], (int)i, size);
    }
    for (int i = 0; i < N; i++)
        free(blocks[i]);
}

int main(int argc, char **argv)
{
    unsigned long secure = getauxval(AT_SECURE);
    if (argc == 2 && strcmp(argv[1], "probe") == 0) {
        printf("AT_SECURE=%lu\n", secure);
        return 0;
    }
    REQUIRE(argc == 3, "usage: secure_conf probe | secure_conf SECURE TCACHE");
    unsigned long want_secure = strtoul(argv[1], NULL, 10);
    bool want_tcache = strcmp(argv[2], "1") == 0;
    printf("secure_conf: AT_SECURE=%lu\n", secure);
    fflush(stdout);
    REQUIRE(secure == want_secure, "AT_SECURE %lu, want = %lu", secure, want_secure);
    require_challoc();
    bool tcache = ctl_bool("opt.tcache");
    CHECK(tcache == want_tcache, "opt.tcache: got = %d, want = %d", tcache, want_tcache);
    if (secure) {
        CHECK(getenv("MALLOC_CONF") != NULL, "MALLOC_CONF was removed from the environment");
        CHECK(getenv("CHALLOC_CONF") != NULL, "CHALLOC_CONF was removed from the environment");
    }
    churn();
    return finish("secure_conf");
}
