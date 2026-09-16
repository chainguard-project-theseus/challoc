/*
 * Threads that exit with full caches. The thread-exit destructor must hand
 * every cached slot back to its slab, so across many short-lived threads
 * the set of distinct addresses stays bounded: if the caches leaked, each
 * thread would consume fresh slots and the count would grow linearly.
 * Also: blocks that outlive their allocating thread stay intact and can be
 * freed by main; a pthread key destructor that allocates after challoc's
 * own destructor has run is served through the direct path; a thread that
 * exits with pthread_exit instead of returning behaves the same.
 */
#include "common.h"

/* Classes whose caches the thread fills: the default capacity is 256 slots
 * up to 128 bytes, halving per doubling of the class size, minimum 8. */
static const size_t classes[] = {QUANTUM, 48, 128, 256, 1024, 4096, 65536};
#define NCLASSES (sizeof classes / sizeof classes[0])
#define FILL 300 /* more than the biggest cache capacity */

/* ---- distinct-address bookkeeping (sorted array, binary insertion) ------- */

struct addrset {
    uintptr_t *a;
    size_t n, cap;
};

static int addrset_add(struct addrset *s, uintptr_t v)
{
    size_t lo = 0, hi = s->n;
    while (lo < hi) {
        size_t mid = (lo + hi) / 2;
        if (s->a[mid] < v)
            lo = mid + 1;
        else
            hi = mid;
    }
    if (lo < s->n && s->a[lo] == v)
        return 0;
    if (s->n == s->cap) {
        s->cap = s->cap ? s->cap * 2 : 4096;
        s->a = realloc(s->a, s->cap * sizeof *s->a);
        REQUIRE(s->a, "addrset grow");
    }
    memmove(&s->a[lo + 1], &s->a[lo], (s->n - lo) * sizeof *s->a);
    s->a[lo] = v;
    s->n++;
    return 1;
}

/* ---- the exiting thread -------------------------------------------------- */

struct job {
    uintptr_t seen[NCLASSES][FILL]; /* every address the thread cached */
    unsigned char *survivor;        /* a block left live for main to free */
    size_t survivor_n;
    int use_pthread_exit;
};

static void *fill_caches_and_exit(void *arg)
{
    struct job *j = arg;
    void *ptrs[FILL];
    for (size_t c = 0; c < NCLASSES; c++) {
        for (int i = 0; i < FILL; i++) {
            ptrs[i] = malloc(classes[c]);
            REQUIRE(ptrs[i], "malloc(%zu)", classes[c]);
            memset(ptrs[i], 0x77, classes[c]);
            j->seen[c][i] = (uintptr_t)ptrs[i];
        }
        /* Free them all: the cache ends up full (cap slots), the rest went
         * back to the slab through flushes. */
        for (int i = 0; i < FILL; i++)
            free(ptrs[i]);
    }
    j->survivor_n = 3000;
    j->survivor = malloc(j->survivor_n);
    REQUIRE(j->survivor, "survivor");
    fill(j->survivor, j->survivor_n, 0x5EED);
    if (j->use_pthread_exit)
        pthread_exit(NULL);
    return NULL;
}

static void bounded_reuse(void)
{
    enum { ROUNDS = 200 };
    static struct job job;
    struct addrset seen[NCLASSES];
    memset(seen, 0, sizeof seen);
    unsigned char *survivors[ROUNDS];

    for (int r = 0; r < ROUNDS; r++) {
        memset(&job, 0, sizeof job);
        job.use_pthread_exit = r & 1;
        pthread_t t;
        REQUIRE(pthread_create(&t, NULL, fill_caches_and_exit, &job) == 0, "pthread_create");
        pthread_join(t, NULL);
        for (size_t c = 0; c < NCLASSES; c++)
            for (int i = 0; i < FILL; i++)
                addrset_add(&seen[c], job.seen[c][i]);
        survivors[r] = job.survivor;
        CHECK(check_fill(job.survivor, job.survivor_n, 0x5EED), "survivor of round %d corrupted", r);
    }
    /* Each round touches FILL slots per class. With reclamation the same
     * slots come back round after round (first fit picks the lowest free
     * addresses); allow generous slack for slots held by main's own caches
     * and by slabs that were mid-flush, but nothing like ROUNDS * FILL. */
    for (size_t c = 0; c < NCLASSES; c++) {
        size_t bound = 4 * FILL;
        CHECK(seen[c].n <= bound, "class %zu: %zu distinct slots over %d rounds of %d (bound %zu): caches leak",
              classes[c], seen[c].n, ROUNDS, FILL, bound);
        free(seen[c].a);
    }
    for (int r = 0; r < ROUNDS; r++) {
        CHECK(check_fill(survivors[r], 3000, 0x5EED), "survivor %d corrupted at the end", r);
        free(survivors[r]); /* cross-thread free of a dead thread's block */
    }
}

/* ---- allocation from a key destructor after ours ------------------------- */

static pthread_key_t late_key;
static volatile int late_ran;

static void late_destructor(void *arg)
{
    /* glibc runs destructors in key order and repeats while values are set;
     * ours may already have run, so this allocation takes the direct path. */
    (void)arg;
    void *ptrs[300];
    for (int i = 0; i < 300; i++) {
        ptrs[i] = malloc(64 + (size_t)i);
        if (!ptrs[i])
            _exit(30);
        memset(ptrs[i], 1, 64 + (size_t)i);
    }
    void *big = malloc(400000);
    if (!big)
        _exit(31);
    free(big);
    for (int i = 0; i < 300; i++)
        free(ptrs[i]);
    late_ran = 1;
}

static void *thread_with_late_destructor(void *arg)
{
    (void)arg;
    void *ptrs[300];
    for (int i = 0; i < 300; i++)
        ptrs[i] = malloc(32);
    for (int i = 0; i < 300; i++)
        free(ptrs[i]);
    REQUIRE(pthread_setspecific(late_key, (void *)1) == 0, "setspecific");
    return NULL;
}

static void late_allocation(void)
{
    REQUIRE(pthread_key_create(&late_key, late_destructor) == 0, "key_create");
    for (int r = 0; r < 20; r++) {
        late_ran = 0;
        pthread_t t;
        REQUIRE(pthread_create(&t, NULL, thread_with_late_destructor, NULL) == 0, "pthread_create");
        pthread_join(t, NULL);
        CHECK(late_ran, "late destructor did not run (round %d)", r);
    }
}

/* ---- many threads exiting at once ---------------------------------------- */

static void *quick(void *arg)
{
    (void)arg;
    void *ptrs[FILL];
    for (int i = 0; i < FILL; i++)
        ptrs[i] = malloc(16 + (size_t)(i % 4) * 16);
    for (int i = 0; i < FILL; i++)
        free(ptrs[i]);
    return NULL;
}

static void concurrent_exits(void)
{
    enum { T = 32 };
    for (int r = 0; r < 10; r++) {
        pthread_t t[T];
        for (int i = 0; i < T; i++)
            REQUIRE(pthread_create(&t[i], NULL, quick, NULL) == 0, "pthread_create");
        for (int i = 0; i < T; i++)
            pthread_join(t[i], NULL);
    }
    /* Detached threads exit without anyone joining them. */
    for (int i = 0; i < T; i++) {
        pthread_t t;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
        REQUIRE(pthread_create(&t, &attr, quick, NULL) == 0, "detached create");
        pthread_attr_destroy(&attr);
    }
    /* Give detached threads time to finish; their heaps are reclaimed by
     * the destructor whether or not anyone joins. */
    usleep(200000);
}

int main(void)
{
    require_challoc();
    bounded_reuse();
    late_allocation();
    concurrent_exits();
    return finish("thread_exit");
}
