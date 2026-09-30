/* Per-thread heap test (also run under pac_pointers).
 *
 * Run as LD_PRELOAD=target/release/libchalloc.so ./tls_threads. The
 * binary has no link-time dependency on the library: `mallctl` is looked
 * up with dlsym (`mallctl_dl`), and `require_challoc` refuses to run on
 * another allocator. 64 threads start; each reads `thread.allocatedp`,
 * the address of its heap's allocation counter, allocates one block and
 * checks that its own `thread.allocated` moved by the block's usable size
 * (the counter behind the pointer is materialised by that read); the
 * main thread verifies the 64 counter addresses are distinct from each
 * other and from its own, so the TLS slot resolved to a different heap in
 * every thread. A barrier keeps every heap alive until all have been
 * observed (an exited thread's heap may be reused). Prints `ok 64 threads
 * ...` (what `tls::tests::c_program_with_64_threads_sees_distinct_slots`
 * greps) and `ok tls_threads` (what src/api/ctests.rs greps), and exits 0
 * iff every check held.
 *
 * Two drivers compile this file: src/api/ctests.rs through
 * scripts/ctest-cc.sh (-DQUANTUM from the library) and src/tls.rs with a
 * bare `cc` line and no QUANTUM, so the constant is defaulted here and the
 * "running under challoc" probe is `challoc_version`, not `require_challoc`
 * (which compares usable sizes against QUANTUM). */
#ifndef QUANTUM
#define QUANTUM 0 /* unused here, see above */
#endif
#include "common.h"

#define NTHREADS 64

static mallctl_fn ctl;

static pthread_barrier_t barrier;
static const uint64_t *observed[NTHREADS];

/* `thread.allocatedp`: the counter's address inside the calling thread's
 * heap, or NULL with a diagnostic. */
static const uint64_t *counter(const char *who)
{
    uint64_t *p = NULL;
    size_t len = sizeof p;
    int rc = ctl("thread.allocatedp", &p, &len, NULL, 0);
    CHECK(rc == 0 && p != NULL, "%s: thread.allocatedp rc %d, %p", who, rc, (void *)p);
    return rc == 0 ? p : NULL;
}

/* `thread.allocated` for the calling thread; the read also materialises
 * the value behind `thread.allocatedp`. */
static uint64_t allocated(const char *who)
{
    uint64_t v = 0;
    size_t len = sizeof v;
    int rc = ctl("thread.allocated", &v, &len, NULL, 0);
    CHECK(rc == 0, "%s: thread.allocated rc %d", who, rc);
    return v;
}

/* One allocation moves this thread's counter by its usable size, the
 * pointer sees the same value once it is read, and the counter's address
 * does not change. */
static void allocate_and_check(const char *who, const uint64_t *ap)
{
    uint64_t before = allocated(who);
    void *volatile block = malloc(64); /* volatile: gcc must not elide the pair */
    REQUIRE(block != NULL, "%s: malloc failed", who);
    size_t usable = malloc_usable_size(block);
    uint64_t after = allocated(who);
    CHECK(after == before + usable, "%s: thread.allocated %llu -> %llu, expected +%zu", who,
          (unsigned long long)before, (unsigned long long)after, usable);
    CHECK(*ap == after, "%s: *thread.allocatedp %llu, thread.allocated %llu", who,
          (unsigned long long)*ap, (unsigned long long)after);
    free(block);
    CHECK(counter(who) == ap, "%s: thread.allocatedp moved", who);
}

static void *worker(void *arg)
{
    char who[32];
    snprintf(who, sizeof who, "thread %lu", (unsigned long)(uintptr_t)arg);
    const uint64_t *ap = counter(who);
    if (ap != NULL)
        allocate_and_check(who, ap);
    observed[(uintptr_t)arg] = ap;
    pthread_barrier_wait(&barrier); /* every thread has a heap before any exits */
    if (ap != NULL)
        CHECK(counter(who) == ap, "%s: thread.allocatedp moved after the barrier", who);
    return NULL;
}

int main(void)
{
    pthread_t threads[NTHREADS];

    const char *(*version)(void) = (const char *(*)(void))dlsym(RTLD_DEFAULT, "challoc_version");
    ctl = mallctl_dl();
    if (!version || !ctl) {
        fprintf(stderr, "challoc_version/mallctl not found: is libchalloc.so preloaded?\n");
        return 2;
    }
    const char *v = version();
    REQUIRE(v != NULL && v[0] != '\0', "challoc_version returned an empty string");

    const uint64_t *main_ap = counter("main");
    REQUIRE(main_ap != NULL, "main: no heap counter");
    allocate_and_check("main", main_ap);

    REQUIRE(pthread_barrier_init(&barrier, NULL, NTHREADS) == 0, "pthread_barrier_init");
    for (uintptr_t i = 0; i < NTHREADS; i++)
        REQUIRE(pthread_create(&threads[i], NULL, worker, (void *)i) == 0, "pthread_create");
    for (int i = 0; i < NTHREADS; i++)
        pthread_join(threads[i], NULL);

    for (int i = 0; i < NTHREADS; i++) {
        CHECK(observed[i] != NULL && observed[i] != main_ap, "thread %d: bad heap counter %p", i, (const void *)observed[i]);
        for (int j = i + 1; j < NTHREADS; j++)
            CHECK(observed[i] != observed[j], "threads %d and %d share a heap (%p)", i, j, (const void *)observed[i]);
    }
    CHECK(counter("main") == main_ap, "main: thread.allocatedp moved after the threads");

    if (failures == 0)
        printf("ok %d threads, distinct heaps (challoc %s)\n", NTHREADS, v);
    return finish("tls_threads");
}
