/* <jemalloc/jemalloc.h> as shipped by challoc-jemalloc-compat-dev: jemalloc
 * 5.3's public API, unprefixed, as exported by challoc's libjemalloc.so.2
 * (docs/design.md sections 13.2 to 13.4; docs/plan.md Phase 3 task 3).
 *
 * This is the header a jemalloc consumer built against jemalloc-dev sees
 * (Ruby's configure probes <jemalloc/jemalloc.h> for `malloc_conf`; KeyDB
 * and RethinkDB call mallctl and the mallocx family), reduced to what
 * challoc guarantees. Every function declared here is a real exported
 * symbol of libjemalloc.so.2, under its plain name and under the je_ prefix
 * (both exist in the library, so nothing here is a rename macro:
 * JEMALLOC_MANGLE and JEMALLOC_NO_RENAME have no effect, and this header
 * can be included together with challoc's own <challoc/jemalloc.h>).
 *
 * The standard entry points (malloc, free, calloc, realloc, posix_memalign,
 * aligned_alloc, memalign, valloc, malloc_usable_size) are not redeclared:
 * they come from <stdlib.h> and <malloc.h> with glibc's declarations, so a
 * C++ translation unit sees one exception specification for malloc, and
 * malloc_usable_size takes `void *` as glibc's does (jemalloc's
 * JEMALLOC_USABLE_SIZE_CONST is empty on glibc for the same reason).
 *
 * Copyright (c) Chainguard, Inc. Licensed under the same terms as challoc.
 */
#ifndef JEMALLOC_H_
#define JEMALLOC_H_

#include <limits.h>
#include <malloc.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <strings.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The version mallctl("version") reports: jemalloc's
 * <major>.<minor>.<bugfix>-<nrev>-g<gid> shape with challoc in the gid
 * field (src/api/mallctl.rs VERSION_STR; jemalloc.pc carries 5.3.0). */
#define JEMALLOC_VERSION "5.3.0-0-gchalloc"
#define JEMALLOC_VERSION_MAJOR 5
#define JEMALLOC_VERSION_MINOR 3
#define JEMALLOC_VERSION_BUGFIX 0
#define JEMALLOC_VERSION_NREV 0
#define JEMALLOC_VERSION_GID "challoc"
#define JEMALLOC_VERSION_GID_IDENT challoc

/* Marks this as challoc's header, for code that wants to tell. */
#define CHALLOC 1
#define JEMALLOC_CHALLOC 1

/* challoc's size-returning entry points (design 13.7) exist in this library
 * too; they are declared at the end of this header, so a consumer of the
 * compat package needs no second header (challoc-dev ships them as
 * <challoc/challoc.h>; the two -dev packages stay co-installable). */
#define JEMALLOC_CHALLOC_USABLE 1

/* jemalloc_defs.h flags a consumer may test. glibc has memalign and valloc,
 * so jemalloc overrides them and declares them; here <malloc.h> does. */
#define JEMALLOC_HAVE_ATTR
#define JEMALLOC_OVERRIDE_MEMALIGN
#define JEMALLOC_OVERRIDE_VALLOC
#define JEMALLOC_USABLE_SIZE_CONST
#define JEMALLOC_USE_CXX_THROW

/* Attribute macros of jemalloc_macros.h, for code written against them. */
#ifdef __GNUC__
#define JEMALLOC_ATTR(s) __attribute__((s))
#define JEMALLOC_ALIGNED(s) JEMALLOC_ATTR(aligned(s))
#define JEMALLOC_ALLOC_SIZE(s) JEMALLOC_ATTR(alloc_size(s))
#define JEMALLOC_ALLOC_SIZE2(s1, s2) JEMALLOC_ATTR(alloc_size(s1, s2))
#define JEMALLOC_EXPORT JEMALLOC_ATTR(visibility("default"))
#define JEMALLOC_FORMAT_PRINTF(s, i) JEMALLOC_ATTR(format(printf, s, i))
#define JEMALLOC_NOINLINE JEMALLOC_ATTR(noinline)
#define JEMALLOC_NOTHROW JEMALLOC_ATTR(nothrow)
#define JEMALLOC_SECTION(s) JEMALLOC_ATTR(section(s))
#define JEMALLOC_RESTRICT_RETURN
#define JEMALLOC_ALLOCATOR
#else
#define JEMALLOC_ATTR(s)
#define JEMALLOC_ALIGNED(s)
#define JEMALLOC_ALLOC_SIZE(s)
#define JEMALLOC_ALLOC_SIZE2(s1, s2)
#define JEMALLOC_EXPORT
#define JEMALLOC_FORMAT_PRINTF(s, i)
#define JEMALLOC_NOINLINE
#define JEMALLOC_NOTHROW
#define JEMALLOC_SECTION(s)
#define JEMALLOC_RESTRICT_RETURN
#define JEMALLOC_ALLOCATOR
#endif
#if defined(__cplusplus) && defined(JEMALLOC_USE_CXX_THROW)
#define JEMALLOC_CXX_THROW throw()
#else
#define JEMALLOC_CXX_THROW
#endif

/* mallocx() / rallocx() / xallocx() / sallocx() / dallocx() / sdallocx() /
 * nallocx() flags, bit for bit jemalloc's (src/arena.rs decodes them). */
#define MALLOCX_LG_ALIGN(la) ((int)(la))
#if defined(__SIZEOF_POINTER__) && __SIZEOF_POINTER__ == 4
#define MALLOCX_ALIGN(a) ((int)(ffs((int)(a)) - 1))
#else
#define MALLOCX_ALIGN(a) \
    ((int)(((size_t)(a) < (size_t)INT_MAX) ? ffs((int)(a)) - 1 : ffs((int)(((size_t)(a)) >> 32)) + 31))
#endif
#define MALLOCX_ZERO ((int)0x40)
/* Bias tcache index bits so that 0 encodes "automatic tcache management". */
#define MALLOCX_TCACHE(tc) ((int)(((tc) + 2) << 8))
#define MALLOCX_TCACHE_NONE MALLOCX_TCACHE(-1)
/* Bias arena index bits so that 0 encodes "use an automatically chosen arena". */
#define MALLOCX_ARENA(a) ((((int)(a)) + 1) << 20)

/* Special arena indices for the arena.<i>.* and stats.arenas.<i>.* keys.
 * challoc never destroys an arena (arena.<i>.destroy purges it and keeps
 * it), so stats.arenas.MALLCTL_ARENAS_DESTROYED.* reads all zero and
 * arena.MALLCTL_ARENAS_DESTROYED.* is ENOENT. */
#define MALLCTL_ARENAS_ALL 4096
#define MALLCTL_ARENAS_DESTROYED 4097

/* A program may define this string, as jemalloc documents; the library
 * defines it null and reads the program's definition at initialisation
 * (design.md section 12; Ruby's configure links a read of it). */
extern const char *malloc_conf;

/* Hook consulted by malloc_stats_print() and by challoc's own messages when
 * set; null (the default) means stderr. */
extern void (*malloc_message)(void *cbopaque, const char *s);

/* Non-standard API. */
void *mallocx(size_t size, int flags) JEMALLOC_ATTR(malloc) JEMALLOC_ALLOC_SIZE(1);
void *rallocx(void *ptr, size_t size, int flags) JEMALLOC_ALLOC_SIZE(2);
size_t xallocx(void *ptr, size_t size, size_t extra, int flags);
size_t sallocx(const void *ptr, int flags) JEMALLOC_ATTR(pure);
void dallocx(void *ptr, int flags);
void sdallocx(void *ptr, size_t size, int flags);
size_t nallocx(size_t size, int flags) JEMALLOC_ATTR(pure);

int mallctl(const char *name, void *oldp, size_t *oldlenp, void *newp, size_t newlen);
int mallctlnametomib(const char *name, size_t *mibp, size_t *miblenp);
int mallctlbymib(const size_t *mib, size_t miblen, void *oldp, size_t *oldlenp, void *newp, size_t newlen);

void malloc_stats_print(void (*write_cb)(void *, const char *), void *cbopaque, const char *opts);

/* Redis's extension of its bundled jemalloc (design.md section 13.5), which
 * KeyDB's defragmenter calls (as je_get_defrag_hint) on every allocation it
 * considers moving: 1 when ptr is a live slab allocation (any slab class,
 * the ones above the bins included) whose slab has a free slot, is not the
 * slab new allocations are placed in, and is used at or below the average
 * of the class's other slabs; 0 otherwise (a huge run, a full slab, a freed
 * or foreign pointer, null).
 * Defining JEMALLOC_FRAG_HINT is how Redis's jemalloc_macros.h announces
 * it (KeyDB's zmalloc.h derives HAVE_DEFRAG from USE_JEMALLOC and it). */
#define JEMALLOC_FRAG_HINT
int get_defrag_hint(void *ptr);

/* The same entry points under jemalloc's je_ prefix; real symbols of the
 * library, not renames (a jemalloc built --with-jemalloc-prefix=je_ exports
 * these, and challoc exports both spellings). */
void *je_malloc(size_t size) JEMALLOC_ATTR(malloc) JEMALLOC_ALLOC_SIZE(1);
void *je_calloc(size_t num, size_t size) JEMALLOC_ATTR(malloc) JEMALLOC_ALLOC_SIZE2(1, 2);
int je_posix_memalign(void **memptr, size_t alignment, size_t size) JEMALLOC_ATTR(nonnull(1));
void *je_aligned_alloc(size_t alignment, size_t size) JEMALLOC_ATTR(malloc) JEMALLOC_ALLOC_SIZE(2);
void *je_realloc(void *ptr, size_t size) JEMALLOC_ALLOC_SIZE(2);
void je_free(void *ptr);
void *je_memalign(size_t alignment, size_t size) JEMALLOC_ATTR(malloc);
void *je_valloc(size_t size) JEMALLOC_ATTR(malloc);
size_t je_malloc_usable_size(const void *ptr);
void *je_mallocx(size_t size, int flags) JEMALLOC_ATTR(malloc) JEMALLOC_ALLOC_SIZE(1);
void *je_rallocx(void *ptr, size_t size, int flags) JEMALLOC_ALLOC_SIZE(2);
size_t je_xallocx(void *ptr, size_t size, size_t extra, int flags);
size_t je_sallocx(const void *ptr, int flags) JEMALLOC_ATTR(pure);
void je_dallocx(void *ptr, int flags);
void je_sdallocx(void *ptr, size_t size, int flags);
size_t je_nallocx(size_t size, int flags) JEMALLOC_ATTR(pure);
int je_mallctl(const char *name, void *oldp, size_t *oldlenp, void *newp, size_t newlen);
int je_mallctlnametomib(const char *name, size_t *mibp, size_t *miblenp);
int je_mallctlbymib(const size_t *mib, size_t miblen, void *oldp, size_t *oldlenp, void *newp, size_t newlen);
void je_malloc_stats_print(void (*write_cb)(void *, const char *), void *cbopaque, const char *opts);
int je_get_defrag_hint(void *ptr);
extern void (*je_malloc_message)(void *cbopaque, const char *s);

/* challoc's own entry points (design 13.7; JEMALLOC_CHALLOC_USABLE above).
 * Each is the standard function of the same name plus the block's usable
 * size, what malloc_usable_size(p) returns for it, from the classification
 * the allocation or the free already made; errno, alignment, tiers and the
 * aborts are malloc/calloc/realloc/free's, *usable is 0 on a null return,
 * and `usable` may be NULL. challoc_calloc_usable zeroes the whole usable
 * size; challoc_free_usable keeps free's exact check and returns 0 for NULL. */
void *challoc_malloc_usable(size_t size, size_t *usable) JEMALLOC_ATTR(malloc);
void *challoc_calloc_usable(size_t n, size_t size, size_t *usable) JEMALLOC_ATTR(malloc);
void *challoc_realloc_usable(void *ptr, size_t size, size_t *usable);
size_t challoc_free_usable(void *ptr);
const char *challoc_version(void);

#ifdef __cplusplus
}
#endif

#endif /* JEMALLOC_H_ */
