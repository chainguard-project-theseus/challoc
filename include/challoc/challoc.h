/* challoc's own extensions of the allocator API, as exported by
 * libchalloc.so (docs/design.md section 13.7). Everything jemalloc-shaped is
 * in <challoc/jemalloc.h>; this header declares only the entry points that
 * exist under no other allocator, so a consumer includes it behind the
 * feature test the jemalloc header provides:
 *
 *     #include <challoc/jemalloc.h>
 *     #ifdef JEMALLOC_CHALLOC_USABLE
 *     #include <challoc/challoc.h>
 *     #endif
 *
 * Copyright (c) Chainguard, Inc. Licensed under the same terms as challoc.
 */
#ifndef CHALLOC_CHALLOC_H
#define CHALLOC_CHALLOC_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The crate version ("0.1.0"), a NUL-terminated static string; the dlsym
 * probe for whether challoc is the loaded allocator (design 13.6). */
const char *challoc_version(void);

/* The `features:` line of the report: which kernel and CPU features the
 * runtime probe found (design 16). Initialises the allocator if needed. */
const char *challoc_features(void);

/* The size-returning allocation entry points (design 13.7). Each is the
 * standard function of the same name plus the usable size of the block,
 * exactly what malloc_usable_size(p) would return for it at that moment,
 * taken from the classification the allocation or the free already made
 * instead of a second lookup. Semantics are otherwise malloc/calloc/realloc/
 * free's: errno = ENOMEM and a null return on failure (then *usable is 0),
 * the same alignment, the same tiers, the same aborts on a pointer that is
 * not a live allocation. `usable` may be NULL for the three allocating
 * functions, which then are plain malloc/calloc/realloc.
 *
 * Valkey's zmalloc calls malloc_usable_size after every allocation and
 * before every unsized zfree for its used_memory accounting; that query was
 * the one allocator-side cost above jemalloc's in its write phase on
 * x86_64. Defining CHALLOC_USABLE is how this header announces the four. */
#define CHALLOC_USABLE 1

/* malloc(size); *usable = the block's usable size. */
void *challoc_malloc_usable(size_t size, size_t *usable);

/* calloc(n, size); *usable = the block's usable size, all of which reads as
 * zero (not only the n * size bytes asked for). */
void *challoc_calloc_usable(size_t n, size_t size, size_t *usable);

/* realloc(ptr, size); *usable = the usable size of the block returned (0
 * when it is NULL, realloc(ptr, 0) included). */
void *challoc_realloc_usable(void *ptr, size_t size, size_t *usable);

/* free(ptr); returns the usable size ptr had, 0 for NULL. The exact check of
 * free (a stale, interior or foreign pointer aborts at the call). */
size_t challoc_free_usable(void *ptr);

#ifdef __cplusplus
}
#endif

#endif /* CHALLOC_CHALLOC_H */
