/*
 * challoc fuzz driver (plan Phase 0 task 13).
 *
 * A seeded, weighted random exerciser for the libc entry points. Every
 * thread keeps a table of live blocks; each block is filled over its whole
 * usable size with a pattern derived from a per-block seed and its offset,
 * and the pattern is verified before the block is freed, reallocated,
 * handed to another thread, on a random read-only inspection, and at the
 * end. Any byte out of place, any misaligned or undersized result, any
 * calloc that is not zero, or any lost accounting is a failure, reported
 * together with the run's seed so it can be replayed.
 *
 * Operation weights (per mille) are in `WEIGHTS` below; sizes are drawn
 * from tiers that cover malloc(0), the small and medium classes, and huge
 * runs including multi-megabyte ones. Allocation flavours rotate through
 * malloc, calloc (with a random nmemb/size split), aligned_alloc,
 * posix_memalign and memalign with alignments up to 64 KiB, and realloc
 * covers grow, shrink, realloc(p, 0), realloc(NULL, n) and reallocarray.
 * A share of frees crosses threads through a per-thread mailbox.
 *
 * Usage: fuzz [ops] [threads] [seed|random] [table]
 *   ops      total operations across all threads (default 10,000,000)
 *   threads  1..64 (default 4)
 *   seed     64-bit seed, or `random` to draw one from getrandom (default
 *            0xC4A110C); the seed in use is always printed first
 *   table    live blocks per thread (default 2048)
 *
 * Exits 0 and prints "ok fuzz" on success, non-zero otherwise. Compile with
 * -DQUANTUM=16 (or 8) and -I tests/c; run under LD_PRELOAD=libchalloc.so.
 * `make fuzz` does all of that.
 */
#include "common.h"

#include <sys/random.h>

#define HANDOFF 128

/* Per-mille weights of the operations on a slot that holds a live block. An
 * empty slot is always allocated into. */
enum op { OP_ALLOC, OP_FREE, OP_REALLOC, OP_HANDOFF, OP_INSPECT, OP_COUNT };
static const unsigned WEIGHTS[OP_COUNT] = {
    [OP_ALLOC] = 330,   /* free, then allocate anew in the same slot */
    [OP_FREE] = 300,
    [OP_REALLOC] = 200,
    [OP_HANDOFF] = 70,
    [OP_INSPECT] = 100,
};

/* Allocation flavours, per mille. */
enum flavour { FL_MALLOC, FL_CALLOC, FL_ALIGNED_ALLOC, FL_POSIX_MEMALIGN, FL_MEMALIGN, FL_COUNT };
static const unsigned FLAVOURS[FL_COUNT] = {
    [FL_MALLOC] = 600,
    [FL_CALLOC] = 200,
    [FL_ALIGNED_ALLOC] = 80,
    [FL_POSIX_MEMALIGN] = 60,
    [FL_MEMALIGN] = 60,
};

struct blk {
    unsigned char *p;
    size_t n;      /* requested size */
    size_t usable; /* malloc_usable_size(p), the filled extent */
    uint32_t seed;
};

struct mailbox {
    pthread_mutex_t lock;
    struct blk slots[HANDOFF];
    int count;
};

struct counts {
    long alloc, free_, realloc_, handoff, foreign, inspect, aligned, zeroed;
    long bytes_filled;
};

struct worker {
    int id;
    int nthreads;
    long ops;
    int table_len;
    uint64_t seed;
    struct mailbox *boxes;
    struct counts c;
};

static uint64_t run_seed;

/* Weighted choice over a per-mille table. */
static unsigned pick(uint64_t *rng, const unsigned *weights, unsigned n)
{
    unsigned r = (unsigned)(rng_next(rng) % 1000);
    for (unsigned i = 0; i < n; i++) {
        if (r < weights[i])
            return i;
        r -= weights[i];
    }
    return n - 1;
}

/* Size tiers, per mille: zero, tiny, small, medium, huge, big huge. */
static size_t random_size(uint64_t *rng)
{
    uint64_t r = rng_next(rng);
    unsigned bucket = (unsigned)(r % 1000);
    uint64_t v = r >> 10;
    if (bucket < 10)
        return 0;
    if (bucket < 450)
        return (size_t)(v % 64) + 1;
    if (bucket < 800)
        return (size_t)(v % 1024) + 1; /* SMALL_MAX */
    if (bucket < 980)
        return (size_t)(v % (256 * 1024)) + 1; /* MEDIUM_MAX */
    if (bucket < 998)
        return 256 * 1024 + (size_t)(v % (2 << 20)) + 1; /* huge, up to ~2.25 MiB */
    return (size_t)(v % (8 << 20)) + (8 << 20); /* 8..16 MiB */
}

/* Power of two between QUANTUM and 64 KiB, biased towards the small end. */
static size_t random_align(uint64_t *rng)
{
    unsigned lg_min = QUANTUM == 8 ? 3 : 4;
    unsigned r = (unsigned)(rng_next(rng) % 100);
    unsigned lg = r < 70 ? lg_min + r % 4 : lg_min + r % (17 - lg_min);
    return (size_t)1 << lg;
}

static void fail_context(int id, long op)
{
    fprintf(stderr, "  (seed %#llx, thread %d, op %ld)\n", (unsigned long long)run_seed, id, op);
}

#define FCHECK(w, op, cond, ...)                                           \
    do {                                                                   \
        if (!(cond)) {                                                     \
            CHECK(0, __VA_ARGS__);                                         \
            fail_context((w)->id, op);                                     \
        }                                                                  \
    } while (0)

static void verify(struct worker *w, long op, const struct blk *b, const char *what)
{
    FCHECK(w, op, check_fill(b->p, b->usable, b->seed), "%s: block %p (%zu requested, %zu usable) corrupted",
           what, (void *)b->p, b->n, b->usable);
}

/* Fill the whole usable extent and record it. */
static void stamp(struct worker *w, struct blk *b, unsigned char *p, size_t n, uint64_t *rng)
{
    b->p = p;
    b->n = n;
    b->usable = malloc_usable_size(p);
    b->seed = (uint32_t)rng_next(rng);
    fill(p, b->usable, b->seed);
    w->c.bytes_filled += (long)b->usable;
}

static void do_alloc(struct worker *w, long op, struct blk *b, uint64_t *rng)
{
    size_t n = random_size(rng);
    unsigned fl = pick(rng, FLAVOURS, FL_COUNT);
    size_t align = QUANTUM;
    unsigned char *p = NULL;
    int zero = 0;

    switch (fl) {
    case FL_MALLOC:
        p = malloc(n);
        break;
    case FL_CALLOC: {
        /* Split n into nmemb * size with a random factor when possible. */
        size_t k = (rng_next(rng) % 4) + 1;
        size_t nm = n / k, sz = k;
        if (nm * sz != n) {
            nm = n;
            sz = 1;
        }
        p = calloc(nm, sz);
        zero = 1;
        break;
    }
    case FL_ALIGNED_ALLOC:
        align = random_align(rng);
        p = aligned_alloc(align, n);
        break;
    case FL_POSIX_MEMALIGN: {
        void *out = NULL;
        align = random_align(rng);
        int rc = posix_memalign(&out, align, n);
        FCHECK(w, op, rc == 0, "posix_memalign(%zu, %zu) = %d", align, n, rc);
        p = out;
        break;
    }
    case FL_MEMALIGN:
        align = random_align(rng);
        p = memalign(align, n);
        break;
    default:
        REQUIRE(0, "bad flavour");
    }

    REQUIRE(p, "thread %d op %ld: alloc flavour %u of %zu bytes failed (seed %#llx)", w->id, op, fl, n,
            (unsigned long long)run_seed);
    FCHECK(w, op, ((uintptr_t)p % align) == 0, "%p not aligned to %zu (flavour %u, %zu bytes)", (void *)p,
           align, fl, n);
    size_t usable = malloc_usable_size(p);
    FCHECK(w, op, usable >= n, "usable %zu < requested %zu", usable, n);
    if (zero) {
        FCHECK(w, op, is_zero(p, n), "calloc(%zu) not zero at %p", n, (void *)p);
        w->c.zeroed++;
    }
    if (align > (size_t)QUANTUM)
        w->c.aligned++;
    stamp(w, b, p, n, rng);
    w->c.alloc++;
}

static void do_realloc(struct worker *w, long op, struct blk *b, uint64_t *rng)
{
    verify(w, op, b, "realloc");
    size_t n = random_size(rng);
    unsigned char *p;
    if ((rng_next(rng) & 15) == 0) {
        /* reallocarray with a split, like calloc */
        size_t k = (rng_next(rng) % 4) + 1;
        size_t nm = n / k, sz = k;
        if (nm * sz != n) {
            nm = n;
            sz = 1;
        }
        p = reallocarray(b->p, nm, sz);
    } else {
        p = realloc(b->p, n);
    }
    if (n == 0) {
        /* glibc semantics: freed, null returned. */
        FCHECK(w, op, p == NULL, "realloc(%p, 0) returned %p", (void *)b->p, (void *)p);
        b->p = NULL;
        w->c.free_++;
        w->c.realloc_++;
        return;
    }
    REQUIRE(p, "thread %d op %ld: realloc(%zu -> %zu) failed (seed %#llx)", w->id, op, b->n, n,
            (unsigned long long)run_seed);
    /* The contract copies min(old usable, new size). */
    size_t kept = b->usable < n ? b->usable : n;
    FCHECK(w, op, check_fill(p, kept, b->seed), "realloc %zu -> %zu lost content (%p -> %p)", b->n, n,
           (void *)b->p, (void *)p);
    FCHECK(w, op, malloc_usable_size(p) >= n, "realloc usable %zu < %zu", malloc_usable_size(p), n);
    FCHECK(w, op, ((uintptr_t)p % QUANTUM) == 0, "realloc result %p misaligned", (void *)p);
    stamp(w, b, p, n, rng);
    w->c.realloc_++;
}

static void drain_mailbox(struct worker *w, long op, struct mailbox *box)
{
    struct blk got[HANDOFF];
    int n;
    pthread_mutex_lock(&box->lock);
    n = box->count;
    memcpy(got, box->slots, (size_t)n * sizeof got[0]);
    box->count = 0;
    pthread_mutex_unlock(&box->lock);
    for (int i = 0; i < n; i++) {
        verify(w, op, &got[i], "foreign free");
        free(got[i].p);
        w->c.foreign++;
    }
}

static void *run(void *arg)
{
    struct worker *w = arg;
    struct blk *table = calloc((size_t)w->table_len, sizeof *table);
    REQUIRE(table, "table");
    uint64_t rng = w->seed;
    struct mailbox *mine = &w->boxes[w->id];

    for (long op = 0; op < w->ops; op++) {
        uint64_t r = rng_next(&rng);
        struct blk *b = &table[(r >> 3) % (uint64_t)w->table_len];

        if (b->p == NULL) {
            do_alloc(w, op, b, &rng);
        } else {
            switch (pick(&rng, WEIGHTS, OP_COUNT)) {
            case OP_ALLOC:
                verify(w, op, b, "free before replace");
                free(b->p);
                b->p = NULL;
                w->c.free_++;
                do_alloc(w, op, b, &rng);
                break;
            case OP_FREE:
                verify(w, op, b, "free");
                free(b->p);
                b->p = NULL;
                w->c.free_++;
                break;
            case OP_REALLOC:
                do_realloc(w, op, b, &rng);
                break;
            case OP_HANDOFF: {
                /* Hand the block to a random other thread to free. */
                if (w->nthreads == 1)
                    break;
                int to = (int)(rng_next(&rng) % (uint64_t)(w->nthreads - 1));
                if (to >= w->id)
                    to++;
                struct mailbox *box = &w->boxes[to];
                verify(w, op, b, "handoff");
                pthread_mutex_lock(&box->lock);
                if (box->count < HANDOFF) {
                    box->slots[box->count++] = *b;
                    b->p = NULL;
                    w->c.handoff++;
                }
                pthread_mutex_unlock(&box->lock);
                break;
            }
            case OP_INSPECT:
                verify(w, op, b, "inspect");
                FCHECK(w, op, malloc_usable_size(b->p) == b->usable, "usable size of %p changed: %zu -> %zu",
                       (void *)b->p, b->usable, malloc_usable_size(b->p));
                w->c.inspect++;
                break;
            }
        }
        if ((op & 127) == 0)
            drain_mailbox(w, op, mine);
        if (failures > 16) {
            fprintf(stderr, "thread %d: too many failures, stopping\n", w->id);
            break;
        }
    }

    for (int i = 0; i < w->table_len; i++)
        if (table[i].p) {
            verify(w, w->ops, &table[i], "final");
            free(table[i].p);
            w->c.free_++;
        }
    free(table);
    return NULL;
}

static uint64_t parse_seed(const char *s)
{
    if (strcmp(s, "random") == 0) {
        uint64_t v = 0;
        REQUIRE(getrandom(&v, sizeof v, 0) == (ssize_t)sizeof v, "getrandom");
        return v;
    }
    return strtoull(s, NULL, 0);
}

int main(int argc, char **argv)
{
    require_challoc();
    long total_ops = argc > 1 ? atol(argv[1]) : 10000000;
    int nthreads = argc > 2 ? atoi(argv[2]) : 4;
    run_seed = argc > 3 ? parse_seed(argv[3]) : 0xC4A110CULL;
    int table_len = argc > 4 ? atoi(argv[4]) : 2048;
    REQUIRE(total_ops > 0 && nthreads >= 1 && nthreads <= 64 && table_len >= 1 && table_len <= (1 << 20),
            "usage: fuzz [ops] [threads] [seed|random] [table]");
    printf("fuzz: seed %#llx, %ld ops, %d threads, table %d\n", (unsigned long long)run_seed, total_ops, nthreads,
           table_len);
    fflush(stdout);

    struct mailbox *boxes = calloc((size_t)nthreads, sizeof *boxes);
    struct worker *workers = calloc((size_t)nthreads, sizeof *workers);
    pthread_t *threads = calloc((size_t)nthreads, sizeof *threads);
    REQUIRE(boxes && workers && threads, "setup");
    for (int i = 0; i < nthreads; i++) {
        pthread_mutex_init(&boxes[i].lock, NULL);
        workers[i].id = i;
        workers[i].nthreads = nthreads;
        workers[i].ops = total_ops / nthreads;
        workers[i].table_len = table_len;
        workers[i].seed = run_seed * 0x9E3779B97F4A7C15ULL + (uint64_t)(i + 1) * 0xBF58476D1CE4E5B9ULL;
        workers[i].boxes = boxes;
    }
    for (int i = 0; i < nthreads; i++)
        REQUIRE(pthread_create(&threads[i], NULL, run, &workers[i]) == 0, "pthread_create");
    for (int i = 0; i < nthreads; i++)
        pthread_join(threads[i], NULL);

    struct counts t = {0};
    long leftover = 0;
    for (int i = 0; i < nthreads; i++) {
        for (int k = 0; k < boxes[i].count; k++) {
            struct blk *b = &boxes[i].slots[k];
            CHECK(check_fill(b->p, b->usable, b->seed), "leftover mailbox block %p corrupted", (void *)b->p);
            free(b->p);
            leftover++;
        }
        t.alloc += workers[i].c.alloc;
        t.free_ += workers[i].c.free_;
        t.realloc_ += workers[i].c.realloc_;
        t.handoff += workers[i].c.handoff;
        t.foreign += workers[i].c.foreign;
        t.inspect += workers[i].c.inspect;
        t.aligned += workers[i].c.aligned;
        t.zeroed += workers[i].c.zeroed;
        t.bytes_filled += workers[i].c.bytes_filled;
    }
    CHECK(t.alloc == t.free_ + t.foreign + leftover, "accounting: %ld allocs vs %ld frees + %ld foreign + %ld leftover",
          t.alloc, t.free_, t.foreign, leftover);
    CHECK(t.handoff == t.foreign + leftover, "handoffs %ld vs foreign %ld + leftover %ld", t.handoff, t.foreign,
          leftover);
    printf("fuzz: %ld allocs (%ld aligned, %ld calloc) %ld frees (%ld cross-thread) %ld reallocs %ld inspections, "
           "%.1f MiB filled\n",
           t.alloc, t.aligned, t.zeroed, t.free_ + t.foreign + leftover, t.foreign + leftover, t.realloc_,
           t.inspect, (double)t.bytes_filled / (1024.0 * 1024.0));
    free(boxes);
    free(workers);
    free(threads);
    if (failures)
        fprintf(stderr, "fuzz: replay with: fuzz %ld %d %#llx %d\n", total_ops, nthreads,
                (unsigned long long)run_seed, table_len);
    return finish("fuzz");
}
