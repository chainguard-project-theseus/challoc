/* challoc stand-in for glibc's test-skeleton.c / support/test-driver.c.
 *
 * Every source in malloc/ ends with `#define TEST_FUNCTION do_test ()` and
 * `#include "../test-skeleton.c"`, which resolves to this file. It supplies
 * main(): refuse to run unless challoc is actually serving malloc (so a run
 * without the preload fails, like every other C test in this repo), run the
 * test, and print `ok <TEST_NAME>` when do_test returns 0.
 *
 * glibc's driver also enforces a timeout with alarm(2). This one does not:
 * tst-mallocfork.c installs its own SIGALRM handler, and the Rust driver
 * that runs these programs has no timeout for the other C tests either.
 *
 * Compile with -D_GNU_SOURCE -DQUANTUM=<n> -DTEST_NAME="<name>" and
 * -I tests/glibc/include for the <libc-diag.h> and <support/...> shims. */
#include <dlfcn.h>
#include <malloc.h>
#include <stdio.h>
#include <stdlib.h>

#ifndef TEST_NAME
#error "compile with -DTEST_NAME=\"tst-...\""
#endif
#ifndef QUANTUM
#error "compile with -DQUANTUM=16 or -DQUANTUM=8"
#endif

int main(void)
{
    /* Same check as require_challoc() in tests/c/common.h. */
    void *probe = malloc(1);
    size_t u = malloc_usable_size(probe);
    if (u != QUANTUM || dlsym(RTLD_DEFAULT, "challoc_tls_get") == NULL) {
        fprintf(stderr, "%s: not running under challoc (usable(1) = %zu)\n", TEST_NAME, u);
        return 1;
    }
    free(probe);

    int r = TEST_FUNCTION;
    if (r != 0) {
        fprintf(stderr, "%s: do_test returned %d\n", TEST_NAME, r);
        return r;
    }
    printf("ok %s\n", TEST_NAME);
    return 0;
}
