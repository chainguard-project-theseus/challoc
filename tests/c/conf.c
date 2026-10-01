/*
 * The malloc_conf sources end to end under LD_PRELOAD. The
 * driver in src/api/c_programs.rs builds this program once per case:
 *
 *   -DCONF='"..."'  defines `je_malloc_conf` with that string, statically
 *                   initialised as Redis does it; with -DPLAIN it defines
 *                   `malloc_conf` instead, which the library also defines
 *                   (null) so Ruby's configure probe links, and the
 *                   program's definition must win. The allocator reads the
 *                   symbol on the first malloc, which may run before main
 *                   (stdio, libc start-up), so a runtime assignment would
 *                   come too late. The binary is linked with -rdynamic so
 *                   the executable exports the symbol; a program that links
 *                   against libchalloc gets that from the linker for free,
 *                   a bare LD_PRELOAD does not (exactly as with jemalloc).
 *   environment     the driver sets MALLOC_CONF / CHALLOC_CONF.
 *
 * Every case allocates and frees a few thousand blocks so a configuration
 * that changes the caches (tcache:false, small caps) is exercised, then
 * prints `ok conf`. abort_conf in the strings turns any parse problem into
 * an abort on the first allocation, which the driver checks for.
 */
#include "common.h"

#ifdef CONF
# ifdef PLAIN
const char *malloc_conf = CONF;
# else
const char *je_malloc_conf = CONF;
# endif
#endif

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

int main(void)
{
    printf("conf: start\n");
    fflush(stdout);
    require_challoc();
    churn();
    printf("ok conf\n");
    return 0;
}
