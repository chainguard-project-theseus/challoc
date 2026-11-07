/*
 * The lazy background start in the fork window (design 8.1, 9): an atfork
 * child handler registered before challoc's (from .preinit_array) runs in
 * the child before challoc's child handler, with every allocator lock
 * held. Here it frees a 96 MiB burst of touched memory and keeps allocating, so
 * its slow paths tick past the lazy start's threshold. `start_requested`
 * and `start` refuse there (a thread started in the window would be
 * forgotten by challoc's child handler, and a later start would run a
 * second one on the same stack); the request stays for a tick after the
 * fork. The child then keeps allocating and never has more than one
 * `challoc-bg` thread. The parent forks with the lazy start armed and no
 * thread yet (`background_thread` unset, the default).
 */
#include "common.h"

const char *malloc_conf = "dirty_decay_ms:500";

static volatile int in_window_threads = -1;

/* Slow paths for `ms` milliseconds: the lazy start looks every 100 ms. */
static void tick_for(long ms)
{
    struct timespec t0, t;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    do {
        void *q[16];
        for (int i = 0; i < 16; i++)
            q[i] = malloc(70000 + (size_t)i * 4096);
        for (int i = 0; i < 16; i++)
            free(q[i]);
        clock_gettime(CLOCK_MONOTONIC, &t);
    } while ((t.tv_sec - t0.tv_sec) * 1000L + (t.tv_nsec - t0.tv_nsec) / 1000000L < ms);
}

/* A look first (it records the dirty baseline), then a 96 MiB burst
 * freed, then more looks: past the lazy start's threshold. */
static void burst_and_tick(void)
{
    enum { NS = 64 * 1024, NH = 8 };
    static void *s[NS], *h[NH];
    tick_for(250);
    /* Touched, so it counts as dirty: 64 MiB of 1 KiB blocks and 32 MiB
     * of 4 MiB ones (lazy_bg.c's burst). */
    for (int i = 0; i < NS; i++) {
        s[i] = malloc(1024);
        if (s[i])
            memset(s[i], 0x5a, 1024);
    }
    for (int i = 0; i < NH; i++) {
        h[i] = malloc(4u << 20);
        if (h[i])
            memset(h[i], 0x5a, 4u << 20);
    }
    for (int i = 0; i < NS; i++)
        free(s[i]);
    for (int i = 0; i < NH; i++)
        free(h[i]);
    tick_for(300);
}

static void child_handler(void)
{
    burst_and_tick();
    in_window_threads = bg_threads();
}

ATFORK_FIRST(NULL, NULL, child_handler);

static int in_child(void)
{
    if (in_window_threads != 0) {
        fprintf(stderr, "%d challoc-bg threads started in the fork window\n", in_window_threads);
        return 4;
    }
    for (int k = 0; k < 4; k++) {
        burst_and_tick();
        struct timespec t = {0, 100 * 1000 * 1000};
        nanosleep(&t, NULL);
        int n = bg_threads();
        if (n > 1) {
            fprintf(stderr, "%d challoc-bg threads\n", n);
            return 5;
        }
    }
    return 0;
}

int main(void)
{
    require_challoc();
    for (int round = 0; round < 3; round++) {
        if (bg_threads() != 0) {
            /* A burst in an earlier round's parent started the thread:
             * this round would not exercise the lazy start. */
            break;
        }
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork: %s", strerror(errno));
        if (pid == 0)
            _exit(in_child());
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            REQUIRE(errno == EINTR, "waitpid: %s", strerror(errno));
        CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "round %d: child status %#x", round, st);
    }
    if (failures)
        return 1;
    printf("ok fork_window_lazy\n");
    return 0;
}
