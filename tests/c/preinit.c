/*
 * Entry points called before any constructor has run. glibc 2.44's
 * `__newlocale` calls `free(NULL)` from a static initialiser, which
 * crashed an allocator that classified the pointer before testing it
 * for NULL. An ELF `.preinit_array` entry of the executable runs after
 * relocation but before the initialisers of every shared object (libc's
 * and libchalloc.so's included), so `early` below is as early as a
 * program can call the allocator. challoc has no constructor and
 * initialises on the first entry point that needs its state; `early`
 * starts with the calls that must not need it (`free(NULL)`,
 * `free_sized(NULL, 0)`, `malloc_usable_size(NULL)`), then allocates
 * and builds a locale the way glibc's static initialisers do. A
 * priority-101 constructor (the executable's first, run after every
 * shared object's) repeats the NULL calls. The results are recorded and checked in
 * `main`, after `require_challoc`: stdio is not used before libc's
 * initialisers have run.
 */
#include "common.h"

#include <locale.h>

enum {
    EARLY_RAN = 1 << 0,
    EARLY_USABLE_NULL = 1 << 1,
    EARLY_REALLOC_NULL = 1 << 2,
    EARLY_MALLOC = 1 << 3,
    EARLY_CALLOC = 1 << 4,
    EARLY_NEWLOCALE = 1 << 5,
    EARLY_SETLOCALE = 1 << 6,
    CTOR_RAN = 1 << 7,
    CTOR_USABLE_NULL = 1 << 8,
};

static volatile unsigned seen;
/* Allocations made in `early`, freed in `main` once challoc is known to
 * be serving them. */
static void *volatile early_block;
static void *volatile early_realloc;

static void early(int argc, char **argv, char **envp)
{
    (void)argc;
    (void)argv;
    (void)envp;
    unsigned s = EARLY_RAN;
    free(NULL);
    if (free_sized)
        free_sized(NULL, 0);
    if (malloc_usable_size(NULL) == 0)
        s |= EARLY_USABLE_NULL;
    /* realloc(NULL, 0) is malloc(0): a block or NULL, both valid. */
    void *r = realloc(NULL, 0);
    s |= EARLY_REALLOC_NULL;
    early_realloc = r;
    unsigned char *p = malloc(100);
    if (p) {
        memset(p, 0xA5, 100);
        s |= EARLY_MALLOC;
    }
    early_block = p;
    unsigned char *z = calloc(4, 64);
    if (z && is_zero(z, 256))
        s |= EARLY_CALLOC;
    free(z);
    locale_t l = newlocale(LC_ALL_MASK, "C.UTF-8", (locale_t)0);
    if (l) {
        s |= EARLY_NEWLOCALE;
        freelocale(l);
    }
    if (setlocale(LC_ALL, "C.UTF-8") != NULL)
        s |= EARLY_SETLOCALE;
    free(NULL);
    seen |= s;
}

__attribute__((section(".preinit_array"), used)) static void (*const preinit_entry)(int, char **, char **) = early;

__attribute__((constructor(101))) static void ctor(void)
{
    unsigned s = CTOR_RAN;
    free(NULL);
    if (free_sized)
        free_sized(NULL, 0);
    if (malloc_usable_size(NULL) == 0)
        s |= CTOR_USABLE_NULL;
    seen |= s;
}

int main(void)
{
    require_challoc();
    unsigned s = seen;
    CHECK(s & EARLY_RAN, "the .preinit_array entry did not run");
    CHECK(s & EARLY_USABLE_NULL, "malloc_usable_size(NULL) before init was not 0");
    CHECK(s & EARLY_REALLOC_NULL, "realloc(NULL, 0) before init did not return");
    CHECK(s & EARLY_MALLOC, "malloc(100) before init failed");
    CHECK(s & EARLY_CALLOC, "calloc(4, 64) before init failed or was not zero");
    CHECK(s & EARLY_NEWLOCALE, "newlocale(C.UTF-8) before init failed");
    CHECK(s & EARLY_SETLOCALE, "setlocale(C.UTF-8) before init failed");
    CHECK(s & CTOR_RAN, "the priority-101 constructor did not run");
    CHECK(s & CTOR_USABLE_NULL, "malloc_usable_size(NULL) in the constructor was not 0");
    CHECK(free_sized != NULL, "free_sized is not exported");
    unsigned char *p = early_block;
    if (p) {
        CHECK(malloc_usable_size(p) >= 100, "usable size of the early block");
        int intact = 1;
        for (int i = 0; i < 100; i++)
            intact &= p[i] == 0xA5;
        CHECK(intact, "the early block was overwritten");
        free(p);
    }
    free(early_realloc);
    void *q = malloc(100);
    CHECK(q != NULL, "malloc(100) after the early calls");
    free(q);
    return finish("preinit");
}
