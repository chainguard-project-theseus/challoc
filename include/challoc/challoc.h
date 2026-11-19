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
 * Copyright (c) Chainguard, Inc.
 * SPDX-License-Identifier: Apache-2.0
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

/* free(ptr); returns the usable size ptr had, 0 for NULL. free's check (a
 * stale, interior or foreign pointer aborts at the call; a slot still held
 * in a thread cache is not checked exactly, double_free_scan:true narrows
 * this). */
size_t challoc_free_usable(void *ptr);

/* Fork epochs (design 5, "Fork epochs", and 13.7). While a child made by
 * fork(2) shares the parent's pages, every write of the parent to one of
 * them copies the page, and the parent's new allocations land on shared
 * pages: a snapshotting server (Valkey's or Redis's BGSAVE, an AOF rewrite,
 * a full resync) copies a 4 KiB page for each small value it writes while
 * the child runs. Between challoc_cow_begin() and challoc_cow_end() the
 * allocations come from pages the child does not share, and frees of
 * pre-fork blocks are not handed out again, so they copy nothing:
 *
 *     challoc_cow_begin(0);
 *     pid_t pid = fork();
 *     if (pid == -1) challoc_cow_end();
 *     ...
 *     waitpid(pid, ...);          // or wherever the child is reaped
 *     challoc_cow_end();
 *
 * This covers the size classes served from slabs (up to the largest medium
 * class). Requests above it come from the huge region, which the epoch does
 * not steer: begin purges every free huge run, but a huge block freed during
 * the epoch is retained (below huge_retain_max) and a later huge request
 * may reuse its pages while the child still shares them.
 *
 * The bytes written during the epoch stay above the old allocations as
 * new ones (they cost new memory instead of copies) until they are freed or
 * a defragmenter moves them: after challoc_cow_end() the utilisation
 * queries (experimental.utilization.*) and get_defrag_hint() report them as
 * worth moving while the holes the epoch left below are not refilled.
 * A cap ends the epoch's effect early: once the epoch has allocated as many
 * slab bytes as were in use when it began (at least 64 MiB), past which it
 * would cost more than copying every page, allocation returns to the usual
 * address-ordered first fit until challoc_cow_end().
 *
 * challoc_cow_begin() is called by the thread that forks, just before
 * fork(2): it returns that thread's cached blocks to their slabs and
 * purges every free slab (as malloc_trim(0)), so the child pins none of the
 * parent's free memory. Epochs nest; only the outermost begin and end act,
 * one at a time: a begin while another thread's outermost begin or end is
 * under way opens nothing and returns EBUSY (do not call challoc_cow_end()
 * for it). Returns 0, EINVAL for flags other than 0, ENOTSUP with
 * MALLOC_CONF cow_epoch:false, EBUSY as above. challoc_cow_end() returns 0,
 * also when no epoch is open, or EAGAIN when other threads' begins and ends
 * kept changing the count under it: the end did not count, so call it
 * again (a program whose begins and ends all run on one thread never
 * sees it).
 * The epoch belongs to the parent: a child made by fork(2), whether or not
 * the fork was the one the epoch was opened for, starts with no epoch open
 * and need not call challoc_cow_end().
 * Defining CHALLOC_COW is how this header announces them. */
#define CHALLOC_COW 1
int challoc_cow_begin(unsigned flags);
int challoc_cow_end(void);

/* One figure of the fork epochs, by index; 0 for an unknown index or
 * before the allocator is initialised. challoc_cow_stats() reads them all. */
enum {
    CHALLOC_COW_EPOCHS = 0,     /* epochs begun */
    CHALLOC_COW_OPEN = 1,       /* epochs open (nesting depth) */
    CHALLOC_COW_IN_FORCE = 2,   /* 1 while the open epoch's floors hold */
    CHALLOC_COW_EXPIRED = 3,    /* epochs whose cap ended the floors early */
    CHALLOC_COW_CLAIMED = 4,    /* bytes allocated under the current or last epoch's floors */
    CHALLOC_COW_CAP = 5,        /* that epoch's cap, in bytes */
    CHALLOC_COW_DEFLECTED = 6,  /* frees kept from being handed out again */
    CHALLOC_COW_DRAIN_MOVES = 7, /* moves the utilisation queries asked for */
    CHALLOC_COW_DRAINING = 8,   /* (arena, size class) pairs still draining */
    CHALLOC_COW_NSTATS = 9
};
unsigned long long challoc_cow_stat(unsigned which);

typedef struct {
    unsigned long long epochs, open, in_force, expired, claimed, cap, deflected, drain_moves, draining;
} challoc_cow_stats_t;

static inline void challoc_cow_stats(challoc_cow_stats_t *s) {
    s->epochs = challoc_cow_stat(CHALLOC_COW_EPOCHS);
    s->open = challoc_cow_stat(CHALLOC_COW_OPEN);
    s->in_force = challoc_cow_stat(CHALLOC_COW_IN_FORCE);
    s->expired = challoc_cow_stat(CHALLOC_COW_EXPIRED);
    s->claimed = challoc_cow_stat(CHALLOC_COW_CLAIMED);
    s->cap = challoc_cow_stat(CHALLOC_COW_CAP);
    s->deflected = challoc_cow_stat(CHALLOC_COW_DEFLECTED);
    s->drain_moves = challoc_cow_stat(CHALLOC_COW_DRAIN_MOVES);
    s->draining = challoc_cow_stat(CHALLOC_COW_DRAINING);
}

#ifdef __cplusplus
}
#endif

#endif /* CHALLOC_CHALLOC_H */
