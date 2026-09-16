/* TLS accessor test (docs/plan.md, Phase 0 task 4).
 *
 * Run as LD_PRELOAD=target/release/libchalloc.so ./tls_threads. Looks up the
 * diagnostic exports challoc_tls_get/challoc_tls_set with dlsym so the
 * binary has no link-time dependency on the library, then starts 64 threads.
 * The slot holds the thread's heap pointer once it has allocated, so each
 * thread checks its slot starts null, allocates, and then reads a non-null
 * value that the main thread verifies is distinct across all 64 threads.
 * Each thread also round-trips a value through challoc_tls_set/get while it
 * makes no allocation, restoring the heap pointer before continuing. Exits
 * non-zero on any failure. */
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define NTHREADS 64

static void *(*tls_get)(void);
static void (*tls_set)(void *);

static pthread_barrier_t barrier;
static void *observed[NTHREADS];
static int failures;

static void *worker(void *arg)
{
    uintptr_t i = (uintptr_t)arg;
    uint64_t local = i;
    void *mine = &local;

    if (tls_get() != NULL) {
        fprintf(stderr, "thread %lu: slot not null at start\n", (unsigned long)i);
        __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
    }
    void *volatile block = malloc(64); /* volatile: gcc must not elide the pair */
    void *heap = tls_get();
    if (heap == NULL || (uintptr_t)heap <= 2) {
        fprintf(stderr, "thread %lu: slot %p after malloc, expected a heap\n", (unsigned long)i, heap);
        __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
    }
    /* Round-trip through the accessor with no allocation in between. */
    tls_set(mine);
    if (tls_get() != mine) {
        fprintf(stderr, "thread %lu: slot %p, expected %p\n", (unsigned long)i, tls_get(), mine);
        __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
    }
    tls_set(heap);
    free(block);
    observed[i] = heap;
    pthread_barrier_wait(&barrier); /* every thread has a heap before any exits */
    if (tls_get() != heap) {
        fprintf(stderr, "thread %lu: slot %p after barrier, expected %p\n", (unsigned long)i,
                tls_get(), heap);
        __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
    }
    return NULL;
}

int main(void)
{
    pthread_t threads[NTHREADS];

    tls_get = (void *(*)(void))dlsym(RTLD_DEFAULT, "challoc_tls_get");
    tls_set = (void (*)(void *))dlsym(RTLD_DEFAULT, "challoc_tls_set");
    if (!tls_get || !tls_set) {
        fprintf(stderr, "challoc_tls_get/set not found: is libchalloc.so preloaded?\n");
        return 2;
    }

    void *volatile block = malloc(16);
    void *main_heap = tls_get();
    if (main_heap == NULL || (uintptr_t)main_heap <= 2) {
        fprintf(stderr, "main: slot %p after malloc, expected a heap\n", main_heap);
        return 1;
    }
    free(block);

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
        if (observed[i] == NULL || observed[i] == main_heap) {
            fprintf(stderr, "thread %d: bad slot %p\n", i, observed[i]);
            failures++;
        }
        for (int j = i + 1; j < NTHREADS; j++) {
            if (observed[i] == observed[j]) {
                fprintf(stderr, "threads %d and %d share slot value %p\n", i, j, observed[i]);
                failures++;
            }
        }
    }
    if (tls_get() != main_heap) {
        fprintf(stderr, "main: slot %p after threads, expected %p\n", tls_get(), main_heap);
        failures++;
    }

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("ok %d threads, distinct TLS slots\n", NTHREADS);
    return 0;
}
