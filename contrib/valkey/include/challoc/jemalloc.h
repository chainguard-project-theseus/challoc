/* challoc's jemalloc-compatible API, as exported by libchalloc.so.
 *
 * This header declares the je_-prefixed entry points and the mallctl
 * namespace that challoc guarantees (design.md sections 13.1 to 13.4), for
 * programs written against a jemalloc built with --with-jemalloc-prefix=je_
 * such as Valkey. Every function declared here is a real exported symbol of
 * libchalloc.so (both the je_ name and the unprefixed name exist); the flag
 * macros use jemalloc's bit layout, which is what src/arena.rs decodes.
 *
 * It lives under <challoc/...> rather than <jemalloc/...> so that it never
 * collides with a jemalloc-dev header installed on the same system.
 *
 * Copyright (c) Chainguard, Inc. Licensed under the same terms as challoc.
 */
#ifndef CHALLOC_JEMALLOC_H
#define CHALLOC_JEMALLOC_H

#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <strings.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The version mallctl("version") reports: jemalloc's
 * <major>.<minor>.<bugfix>-<nrev>-g<gid> shape with challoc in the gid field
 * (src/api/mallctl.rs VERSION_STR). */
#define JEMALLOC_VERSION "5.3.0-0-gchalloc"
#define JEMALLOC_VERSION_MAJOR 5
#define JEMALLOC_VERSION_MINOR 3
#define JEMALLOC_VERSION_BUGFIX 0
#define JEMALLOC_VERSION_NREV 0
#define JEMALLOC_VERSION_GID "challoc"
#define JEMALLOC_VERSION_GID_IDENT challoc

/* Marks this as challoc's header, for code that wants to tell. */
#define CHALLOC 1

/* mallocx() / rallocx() / xallocx() / sallocx() / dallocx() / sdallocx() /
 * nallocx() flags, bit for bit jemalloc's. */
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

/* Standard allocator entry points under jemalloc's prefix. */
void *je_malloc(size_t size);
void *je_calloc(size_t num, size_t size);
int je_posix_memalign(void **memptr, size_t alignment, size_t size);
void *je_aligned_alloc(size_t alignment, size_t size);
void *je_realloc(void *ptr, size_t size);
void je_free(void *ptr);
void *je_memalign(size_t alignment, size_t size);
void *je_valloc(size_t size);
size_t je_malloc_usable_size(const void *ptr);

/* Non-standard API. */
void *je_mallocx(size_t size, int flags);
void *je_rallocx(void *ptr, size_t size, int flags);
size_t je_xallocx(void *ptr, size_t size, size_t extra, int flags);
size_t je_sallocx(const void *ptr, int flags);
void je_dallocx(void *ptr, int flags);
void je_sdallocx(void *ptr, size_t size, int flags);
size_t je_nallocx(size_t size, int flags);

int je_mallctl(const char *name, void *oldp, size_t *oldlenp, void *newp, size_t newlen);
int je_mallctlnametomib(const char *name, size_t *mibp, size_t *miblenp);
int je_mallctlbymib(const size_t *mib, size_t miblen, void *oldp, size_t *oldlenp, void *newp, size_t newlen);

void je_malloc_stats_print(void (*write_cb)(void *, const char *), void *cbopaque, const char *opts);

/* Redis's extension of its bundled jemalloc (design.md section 13.5), which
 * KeyDB's defragmenter calls on every allocation it considers moving: 1
 * when ptr is a live small allocation whose slab has a free slot, is not
 * the slab new allocations are placed in, and is used at or below the
 * average of the bin's other slabs; 0 otherwise (a huge run, a class above
 * the bins, a full slab, a freed or foreign pointer, null). Defining
 * JEMALLOC_FRAG_HINT is how Redis's header announces it (zmalloc.h derives
 * HAVE_DEFRAG from it). */
#define JEMALLOC_FRAG_HINT
int je_get_defrag_hint(void *ptr);

/* Hook consulted by je_malloc_stats_print() and by challoc's own messages
 * when set; NULL (the default) means stderr. */
extern void (*je_malloc_message)(void *cbopaque, const char *s);

/* A program may define this string; challoc reads it at initialisation the
 * way jemalloc does (design.md section 12). challoc itself does not define
 * it, so only reference it from a program that provides it. */
extern const char *je_malloc_conf;

#ifdef __cplusplus
}
#endif

#endif /* CHALLOC_JEMALLOC_H */
