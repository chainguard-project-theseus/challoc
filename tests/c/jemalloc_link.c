/*
 * A link-only jemalloc consumer. Unlike jemalloc_api.c this
 * program declares the jemalloc entry points strongly and is linked with
 * `$(pkg-config --cflags --libs jemalloc)` against the libjemalloc.so.2 that
 * scripts/jemalloc-compat.sh stages, never through LD_PRELOAD, the way Ruby
 * or KeyDB link `-ljemalloc`. It checks that
 *
 *   - every malloc entry point it can name resolves into a file called
 *     libjemalloc.so.2 under the directory given as argv[1], so linking the
 *     library replaces malloc for the whole process;
 *   - that library is challoc (`require_challoc`, `version`);
 *   - the jemalloc API and the je_ prefix work through the link;
 *   - `malloc_conf` is defined by that library and null (Ruby's configure
 *     probe for `--with-jemalloc` links exactly such a read and would fail
 *     against a library that only references the symbol).
 *
 * Run bare it either fails to load (no libjemalloc.so.2 on the library
 * path) or, when Wolfi's real jemalloc is installed, fails `require_challoc`
 * and the directory check.
 */
#include "common.h"

extern void *mallocx(size_t, int);
extern void *rallocx(void *, size_t, int);
extern size_t sallocx(const void *, int);
extern void dallocx(void *, int);
extern size_t nallocx(size_t, int);
extern int mallctl(const char *, void *, size_t *, void *, size_t);
extern void *je_malloc(size_t);
extern void je_free(void *);
extern size_t je_malloc_usable_size(void *);
extern int je_mallctl(const char *, void *, size_t *, void *, size_t);
extern const char *malloc_conf;

#define MALLOCX_ZERO ((int)0x40)

static const char *libdir;

/* `sym` must resolve, in the global lookup scope, into <libdir>/libjemalloc.so.2. */
static void check_from(const char *sym)
{
    void *addr = dlsym(RTLD_DEFAULT, sym);
    Dl_info info;
    REQUIRE(addr != NULL, "%s: %s", sym, dlerror());
    REQUIRE(dladdr(addr, &info) != 0 && info.dli_fname != NULL, "%s: dladdr failed", sym);
    const char *base = strrchr(info.dli_fname, '/');
    base = base ? base + 1 : info.dli_fname;
    CHECK(strcmp(base, "libjemalloc.so.2") == 0, "%s comes from %s", sym, info.dli_fname);
    if (libdir)
        CHECK(strncmp(info.dli_fname, libdir, strlen(libdir)) == 0 && info.dli_fname[strlen(libdir)] == '/',
              "%s comes from %s, not from %s", sym, info.dli_fname, libdir);
}

int main(int argc, char **argv)
{
    libdir = argc > 1 ? argv[1] : NULL;
    require_challoc();

    static const char *const syms[] = {
        "malloc", "free", "calloc", "realloc", "posix_memalign", "aligned_alloc", "memalign",
        "malloc_usable_size", "mallocx", "rallocx", "sallocx", "dallocx", "nallocx",
        "mallctl", "mallctlnametomib", "mallctlbymib", "malloc_stats_print",
        "je_malloc", "je_free", "je_mallocx", "je_mallctl",
    };
    for (size_t i = 0; i < sizeof syms / sizeof syms[0]; i++)
        check_from(syms[i]);

    /* Ruby's probe: `if (malloc_conf) printf(...)`. Linking proves the
     * library defines the symbol; nothing here sets a string, so it is
     * null. Its address is not checked with check_from: on x86_64 a PIE
     * that reads extern data gets a COPY relocation, so `&malloc_conf` is
     * the executable's copy (aarch64 reads it through the GOT). */
    if (malloc_conf)
        printf("malloc_conf=%s\n", malloc_conf);
    CHECK(malloc_conf == NULL, "malloc_conf is %p", (const void *)malloc_conf);

    const char *version = NULL;
    size_t len = sizeof version;
    CHECK(mallctl("version", &version, &len, NULL, 0) == 0, "mallctl version");
    CHECK(version && strstr(version, "challoc"), "version %s", version ? version : "(null)");
    const char *jv = NULL;
    len = sizeof jv;
    CHECK(je_mallctl("version", &jv, &len, NULL, 0) == 0 && jv == version, "je_mallctl version");

    void *p = mallocx(100, MALLOCX_ZERO);
    REQUIRE(p != NULL, "mallocx");
    CHECK(is_zero(p, 100), "MALLOCX_ZERO");
    CHECK(sallocx(p, 0) >= 100 && sallocx(p, 0) == nallocx(100, 0), "sallocx %zu nallocx %zu",
          sallocx(p, 0), nallocx(100, 0));
    fill(p, 100, 1);
    p = rallocx(p, 5000, 0);
    REQUIRE(p != NULL, "rallocx");
    CHECK(check_fill(p, 100, 1), "rallocx kept the contents");
    dallocx(p, 0);

    void *q = je_malloc(1);
    REQUIRE(q != NULL, "je_malloc");
    CHECK(je_malloc_usable_size(q) == QUANTUM, "je_malloc_usable_size %zu", je_malloc_usable_size(q));
    sink = q;
    je_free(q);

    return finish("jemalloc_link");
}
