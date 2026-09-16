/*
 * One million random alloc/free/realloc operations spread over 8 threads,
 * each keeping a table of live blocks with fill patterns that are verified
 * on every free, realloc and at the end. Sizes are weighted towards the
 * small tier with a medium and a huge tail so every allocator path is
 * exercised. A fraction of the frees are handed to a sibling thread so the
 * cross-thread path sees traffic too.
 *
 * Usage: random_ops [ops] [threads] [seed]
 */
#include "common.h"

#define TABLE 1024
#define HANDOFF 64

struct blk {
    unsigned char *p;
    size_t n;
    uint32_t seed;
};

/* Blocks handed to a sibling for freeing: a tiny lock-protected mailbox. */
struct mailbox {
    pthread_mutex_t lock;
    struct blk slots[HANDOFF];
    int count;
};

struct worker {
    int id;
    int nthreads;
    long ops;
    uint64_t seed;
    struct mailbox *boxes;
    long done_alloc, done_free, done_realloc, done_handoff, done_foreign;
};

static size_t random_size(uint64_t *rng)
{
    uint64_t r = rng_next(rng);
    uint64_t bucket = r % 1000;
    uint64_t v = r >> 10;
    if (bucket < 500)
        return (size_t)(v % 128) + 1;               /* 1..128 */
    if (bucket < 850)
        return (size_t)(v % 1024) + 1;              /* up to SMALL_MAX */
    if (bucket < 990)
        return (size_t)(v % (256 * 1024)) + 1;      /* medium */
    return 256 * 1024 + (size_t)(v % (1 << 20)) + 1; /* huge, up to ~1.25 MiB */
}

static void verify(const struct blk *b, const char *what, int id)
{
    CHECK(check_fill(b->p, b->n, b->seed), "thread %d: %s: block %p (%zu bytes) corrupted", id, what,
          (void *)b->p, b->n);
}

static void drain_mailbox(struct worker *w, struct mailbox *box)
{
    struct blk got[HANDOFF];
    int n;
    pthread_mutex_lock(&box->lock);
    n = box->count;
    memcpy(got, box->slots, (size_t)n * sizeof got[0]);
    box->count = 0;
    pthread_mutex_unlock(&box->lock);
    for (int i = 0; i < n; i++) {
        verify(&got[i], "foreign free", w->id);
        free(got[i].p);
        w->done_foreign++;
    }
}

static void *run(void *arg)
{
    struct worker *w = arg;
    struct blk *table = calloc(TABLE, sizeof *table);
    REQUIRE(table, "table");
    uint64_t rng = w->seed;
    struct mailbox *mine = &w->boxes[w->id];
    struct mailbox *sibling = &w->boxes[(w->id + 1) % w->nthreads];

    for (long op = 0; op < w->ops; op++) {
        uint64_t r = rng_next(&rng);
        struct blk *b = &table[(r >> 3) % TABLE];
        unsigned kind = (unsigned)(r % 100);

        if (b->p == NULL || kind < 45) {
            /* allocate (over an empty slot, or replace after freeing) */
            if (b->p) {
                verify(b, "free before replace", w->id);
                free(b->p);
                w->done_free++;
            }
            size_t n = random_size(&rng);
            int zero = (rng_next(&rng) & 7) == 0;
            unsigned char *p = zero ? calloc(1, n) : malloc(n);
            REQUIRE(p, "thread %d: alloc(%zu) failed", w->id, n);
            CHECK(((uintptr_t)p % QUANTUM) == 0, "misaligned %p", (void *)p);
            CHECK(malloc_usable_size(p) >= n, "usable %zu < %zu", malloc_usable_size(p), n);
            if (zero)
                CHECK(is_zero(p, n), "thread %d: calloc(%zu) not zero", w->id, n);
            b->p = p;
            b->n = n;
            b->seed = (uint32_t)rng_next(&rng);
            fill(p, n, b->seed);
            w->done_alloc++;
        } else if (kind < 75) {
            verify(b, "free", w->id);
            free(b->p);
            b->p = NULL;
            w->done_free++;
        } else if (kind < 90) {
            verify(b, "realloc", w->id);
            size_t n = random_size(&rng);
            unsigned char *p = realloc(b->p, n);
            REQUIRE(p, "thread %d: realloc(%zu) failed", w->id, n);
            CHECK(check_fill(p, b->n < n ? b->n : n, b->seed), "thread %d: realloc lost content %zu -> %zu",
                  w->id, b->n, n);
            b->p = p;
            b->n = n;
            b->seed = (uint32_t)rng_next(&rng);
            fill(p, n, b->seed);
            w->done_realloc++;
        } else {
            /* hand the block to the sibling to free */
            pthread_mutex_lock(&sibling->lock);
            if (sibling->count < HANDOFF) {
                sibling->slots[sibling->count++] = *b;
                b->p = NULL;
                w->done_handoff++;
            }
            pthread_mutex_unlock(&sibling->lock);
        }
        if ((op & 255) == 0)
            drain_mailbox(w, mine);
    }

    for (int i = 0; i < TABLE; i++)
        if (table[i].p) {
            verify(&table[i], "final", w->id);
            free(table[i].p);
            w->done_free++;
        }
    free(table);
    return NULL;
}

int main(int argc, char **argv)
{
    require_challoc();
    long total_ops = argc > 1 ? atol(argv[1]) : 1000000;
    int nthreads = argc > 2 ? atoi(argv[2]) : 8;
    uint64_t seed = argc > 3 ? strtoull(argv[3], NULL, 0) : 0x5EEDu;
    REQUIRE(nthreads >= 1 && nthreads <= 64 && total_ops > 0, "bad arguments");

    struct mailbox *boxes = calloc((size_t)nthreads, sizeof *boxes);
    struct worker *workers = calloc((size_t)nthreads, sizeof *workers);
    pthread_t *threads = calloc((size_t)nthreads, sizeof *threads);
    REQUIRE(boxes && workers && threads, "setup");
    for (int i = 0; i < nthreads; i++) {
        pthread_mutex_init(&boxes[i].lock, NULL);
        workers[i].id = i;
        workers[i].nthreads = nthreads;
        workers[i].ops = total_ops / nthreads;
        workers[i].seed = seed * 0x9E3779B97F4A7C15ULL + (uint64_t)(i + 1) * 0xBF58476D1CE4E5B9ULL;
        workers[i].boxes = boxes;
    }
    for (int i = 0; i < nthreads; i++)
        REQUIRE(pthread_create(&threads[i], NULL, run, &workers[i]) == 0, "pthread_create");
    for (int i = 0; i < nthreads; i++)
        pthread_join(threads[i], NULL);

    /* Whatever is still in the mailboxes was never drained: free it here. */
    long allocs = 0, frees = 0, reallocs = 0, handoffs = 0, foreign = 0, leftover = 0;
    for (int i = 0; i < nthreads; i++) {
        for (int k = 0; k < boxes[i].count; k++) {
            CHECK(check_fill(boxes[i].slots[k].p, boxes[i].slots[k].n, boxes[i].slots[k].seed),
                  "leftover mailbox block corrupted");
            free(boxes[i].slots[k].p);
            leftover++;
        }
        allocs += workers[i].done_alloc;
        frees += workers[i].done_free;
        reallocs += workers[i].done_realloc;
        handoffs += workers[i].done_handoff;
        foreign += workers[i].done_foreign;
    }
    CHECK(allocs == frees + foreign + leftover, "accounting: %ld allocs vs %ld frees + %ld foreign + %ld leftover",
          allocs, frees, foreign, leftover);
    CHECK(handoffs == foreign + leftover, "handoffs %ld vs foreign %ld + leftover %ld", handoffs, foreign, leftover);
    printf("random_ops: %ld ops, %d threads: %ld allocs %ld frees %ld reallocs %ld cross-thread frees\n",
           total_ops, nthreads, allocs, frees, reallocs, foreign + leftover);
    free(boxes);
    free(workers);
    free(threads);
    return finish("random_ops");
}
