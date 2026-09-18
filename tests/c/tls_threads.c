/* Per-thread heap test (docs/plan.md, Phase 0 task 4; Phase 1.5 task 13).
 *
 * Run as LD_PRELOAD=target/release/libchalloc.so ./tls_threads. Looks up
 * challoc_version (the probe that says challoc is the loaded allocator) and
 * mallctl with dlsym so the binary has no link-time dependency on the
 * library, then starts 64 threads. Each thread reads `thread.allocatedp`,
 * the address of its heap's allocation counter, allocates one block and
 * checks that its own `thread.allocated` moved by the block's usable size
 * (the counter behind the pointer is materialised by that read); the
 * main thread verifies the 64 counter addresses are distinct from each
 * other and from its own, so the TLS slot resolved to a different heap in
 * every thread. A barrier keeps every heap alive until all have been
 * observed (an exited thread's heap may be reused). Exits non-zero on any
 * failure. */
#include <dlfcn.h>
#include <malloc.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define NTHREADS 64

static const char *(*version)(void);
static int (*ctl)(const char *, void *, size_t *, void *, size_t);

static pthread_barrier_t barrier;
static const uint64_t *observed[NTHREADS];
static int failures;

static void fail(void)
{
    __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
}

/* `thread.allocatedp`: the counter's address inside the calling thread's
 * heap, or NULL with a diagnostic. */
static const uint64_t *counter(const char *who)
{
    uint64_t *p = NULL;
    size_t len = sizeof p;
    int rc = ctl("thread.allocatedp", &p, &len, NULL, 0);
    if (rc != 0 || p == NULL) {
        fprintf(stderr, "%s: thread.allocatedp rc %d, %p\n", who, rc, (void *)p);
        fail();
        return NULL;
    }
    return p;
}

/* `thread.allocated` for the calling thread; the read also materialises
 * the value behind `thread.allocatedp`. */
static uint64_t allocated(const char *who)
{
    uint64_t v = 0;
    size_t len = sizeof v;
    int rc = ctl("thread.allocated", &v, &len, NULL, 0);
    if (rc != 0) {
        fprintf(stderr, "%s: thread.allocated rc %d\n", who, rc);
        fail();
    }
    return v;
}

/* One allocation moves this thread's counter by its usable size, the
 * pointer sees the same value once it is read, and the counter's address
 * does not change. */
static void allocate_and_check(const char *who, const uint64_t *ap)
{
    uint64_t before = allocated(who);
    void *volatile block = malloc(64); /* volatile: gcc must not elide the pair */
    if (block == NULL) {
        fprintf(stderr, "%s: malloc failed\n", who);
        fail();
        return;
    }
    size_t usable = malloc_usable_size(block);
    uint64_t after = allocated(who);
    if (after != before + usable) {
        fprintf(stderr, "%s: thread.allocated %llu -> %llu, expected +%zu\n", who,
                (unsigned long long)before, (unsigned long long)after, usable);
        fail();
    }
    if (*ap != after) {
        fprintf(stderr, "%s: *thread.allocatedp %llu, thread.allocated %llu\n", who,
                (unsigned long long)*ap, (unsigned long long)after);
        fail();
    }
    free(block);
    if (counter(who) != ap) {
        fprintf(stderr, "%s: thread.allocatedp moved\n", who);
        fail();
    }
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
    if (ap != NULL && counter(who) != ap) {
        fprintf(stderr, "%s: thread.allocatedp moved after the barrier\n", who);
        fail();
    }
    return NULL;
}

int main(void)
{
    pthread_t threads[NTHREADS];

    version = (const char *(*)(void))dlsym(RTLD_DEFAULT, "challoc_version");
    ctl = (int (*)(const char *, void *, size_t *, void *, size_t))dlsym(RTLD_DEFAULT, "mallctl");
    if (!version || !ctl) {
        fprintf(stderr, "challoc_version/mallctl not found: is libchalloc.so preloaded?\n");
        return 2;
    }
    const char *v = version();
    if (v == NULL || v[0] == '\0') {
        fprintf(stderr, "challoc_version returned an empty string\n");
        return 1;
    }

    const uint64_t *main_ap = counter("main");
    if (main_ap == NULL)
        return 1;
    allocate_and_check("main", main_ap);

    if (pthread_barrier_init(&barrier, NULL, NTHREADS) != 0) {
        perror("pthread_barrier_init");
        return 2;
    }
    for (uintptr_t i = 0; i < NTHREADS; i++) {
        if (pthread_create(&threads[i], NULL, worker, (void *)i) != 0) {
            perror("pthread_create");
            return 2;
        }
    }
    for (int i = 0; i < NTHREADS; i++)
        pthread_join(threads[i], NULL);

    for (int i = 0; i < NTHREADS; i++) {
        if (observed[i] == NULL || observed[i] == main_ap) {
            fprintf(stderr, "thread %d: bad heap counter %p\n", i, (const void *)observed[i]);
            failures++;
        }
        for (int j = i + 1; j < NTHREADS; j++) {
            if (observed[i] == observed[j]) {
                fprintf(stderr, "threads %d and %d share a heap (%p)\n", i, j, (const void *)observed[i]);
                failures++;
            }
        }
    }
    if (counter("main") != main_ap) {
        fprintf(stderr, "main: thread.allocatedp moved after the threads\n");
        failures++;
    }

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("ok %d threads, distinct heaps (challoc %s)\n", NTHREADS, v);
    return 0;
}
