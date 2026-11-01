/*
 * A restart of the background thread from a thread with no heap. A raw
 * pthread that has not allocated (its TLS slot is empty) stops the
 * background thread through mallctl("background_thread"), waits until it
 * has exited, and starts it again. The restart joins the old thread
 * first: the thread runs on challoc's own stack, so glibc frees its DTV
 * inside pthread_join, and that free creates this thread's heap in the
 * slot. `create` (src/purge/background.rs) must park that heap on
 * TLS_CREATING for pthread_create, whose calloc of the new DTV then takes
 * the direct path, and put the same heap back after.
 *
 * Checked on that thread right after the restart, before it allocates:
 *   - thread.allocated reads (rc 0): the slot holds a heap, not the
 *     TLS_CREATING sentinel (a restart that parks and never restores
 *     leaves every later call of the thread on the direct path, where
 *     the thread.* keys answer EAGAIN);
 *   - thread.deallocated > 0: the join's free ran on a heap in this
 *     thread's slot, and that heap is still the slot's (a restart that
 *     restores an empty slot loses it, and the read creates a fresh heap
 *     with no frees). This also shows the path is covered: a heap present
 *     before the restart is parked across the join, which then frees
 *     nothing on it;
 *   - thread.allocated == 0: pthread_create's calloc did not run on that
 *     heap, so the heap was parked across it.
 * Then two malloc/free cycles move thread.allocated and
 * thread.deallocated by at least the requested sizes. Three rounds, each
 * from a fresh thread.
 */
#include "common.h"

#include <sched.h>

#define ROUNDS 3
#define EXIT_WAIT_MS 10000

static bool bg_present(void)
{
    bool on = false;
    size_t len = sizeof on;
    REQUIRE(mallctl("background_thread", &on, &len, NULL, 0) == 0, "mallctl(background_thread) read");
    return on;
}

static int bg_set(bool on)
{
    return mallctl("background_thread", NULL, NULL, &on, sizeof on);
}

/* A thread.* counter, or the mallctl error with *v left 0. */
static int counter(const char *name, uint64_t *v)
{
    size_t len = sizeof *v;
    *v = 0;
    return mallctl(name, v, &len, NULL, 0);
}

/* One malloc/free of `size` on this thread's heap moves its counters. */
static void cycle(int round, size_t size, uint32_t seed)
{
    uint64_t a0, d0, a1, d1;
    CHECK(counter("thread.allocated", &a0) == 0, "round %d: thread.allocated before malloc(%zu)", round, size);
    CHECK(counter("thread.deallocated", &d0) == 0, "round %d: thread.deallocated before malloc(%zu)", round, size);
    void *p = malloc(size);
    REQUIRE(p != NULL, "round %d: malloc(%zu)", round, size);
    fill(p, size, seed);
    sink = p;
    CHECK(counter("thread.allocated", &a1) == 0 && a1 >= a0 + size,
          "round %d: malloc(%zu) moved thread.allocated %llu -> %llu: not served from this thread's heap", round, size,
          (unsigned long long)a0, (unsigned long long)a1);
    CHECK(check_fill(p, size, seed), "round %d: block of %zu bytes lost its contents", round, size);
    free(p);
    CHECK(counter("thread.deallocated", &d1) == 0 && d1 >= d0 + size,
          "round %d: free of %zu bytes moved thread.deallocated %llu -> %llu", round, size, (unsigned long long)d0,
          (unsigned long long)d1);
}

/* Runs with an empty slot: nothing here allocates before the restart. */
static void *restart_from_empty_slot(void *arg)
{
    int round = (int)(intptr_t)arg;
    REQUIRE(bg_set(false) == 0, "round %d: background_thread off", round);
    long t0 = now_ms();
    while (bg_present()) {
        REQUIRE(now_ms() - t0 < EXIT_WAIT_MS, "round %d: the background thread did not exit in %d ms", round,
                EXIT_WAIT_MS);
        sched_yield();
    }
    int rc = bg_set(true);
    REQUIRE(rc == 0, "round %d: background_thread on: rc %d", round, rc);
    CHECK(bg_present(), "round %d: no background thread after the restart", round);

    uint64_t a, d;
    rc = counter("thread.allocated", &a);
    CHECK(rc == 0, "round %d: thread.allocated rc %d after the restart: the slot holds no heap (left parked?)", round,
          rc);
    CHECK(a == 0,
          "round %d: thread.allocated %llu after the restart: pthread_create's calloc ran on the heap the join created",
          round, (unsigned long long)a);
    rc = counter("thread.deallocated", &d);
    CHECK(rc == 0 && d > 0,
          "round %d: thread.deallocated rc %d, %llu after the restart: the heap the join's free created is not in the "
          "slot (lost by the restart, or the join freed nothing on it)",
          round, rc, (unsigned long long)d);
    printf("bg_restart: round %d: the join's free left %llu bytes on this thread's heap\n", round,
           (unsigned long long)d);

    cycle(round, 64, (uint32_t)round);
    cycle(round, 200, (uint32_t)round + 100);
    return NULL;
}

int main(void)
{
    require_challoc();
    REQUIRE(bg_set(true) == 0, "background_thread on");
    REQUIRE(bg_present(), "no background thread after mallctl(background_thread, true)");
    for (int round = 0; round < ROUNDS; round++) {
        pthread_t th;
        REQUIRE(pthread_create(&th, NULL, restart_from_empty_slot, (void *)(intptr_t)round) == 0, "pthread_create");
        REQUIRE(pthread_join(th, NULL) == 0, "pthread_join");
    }
    return finish("bg_restart");
}
