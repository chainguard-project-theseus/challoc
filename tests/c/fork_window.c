/*
 * What an atfork handler registered before challoc initialises may not
 * change in the fork window (design 9): its child handler runs in the
 * child before challoc's, with every allocator lock held, and turning
 * the background thread on there would start a thread that challoc's
 * child handler then forgets while it runs on the recorded stack, so a
 * later request started a second thread on the same stack (stack
 * smashing, two `challoc-bg` threads). In the window
 * `mallctl("background_thread", true)` is refused with EAGAIN and
 * changes nothing; after the fork the child turns the thread on and off
 * repeatedly and never has more than one.
 */
#include "common.h"

static volatile int window_rc = -1;

/* bg_threads() once the count is at most one, or after 2 s. Turning the
 * thread on joins the one the last "off" stopped before it starts the
 * next, but pthread_join returns at the exiting thread's CLONE_CHILD_CLEARTID
 * wake-up, which the kernel makes before it unlinks the thread from
 * /proc/self/task: a preempted exit can still be listed, as challoc-bg,
 * next to its successor. That entry goes when the exit finishes; a
 * second live thread on the same stack (the bug) stays. */
static int bg_threads_settled(void)
{
    int n = bg_threads();
    for (int i = 0; n > 1 && i < 2000; i++) {
        usleep(1000);
        n = bg_threads();
    }
    return n;
}

static void child_handler(void)
{
    bool on = true;
    window_rc = mallctl("background_thread", NULL, NULL, &on, sizeof on);
}

ATFORK_FIRST(NULL, NULL, child_handler);

static void churn(void)
{
    for (int r = 0; r < 10; r++) {
        void *p[64];
        for (int i = 0; i < 64; i++)
            p[i] = malloc(70000 + (size_t)i * 4096);
        for (int i = 0; i < 64; i++)
            free(p[i]);
    }
}

static int in_child(void)
{
    if (window_rc != EAGAIN) {
        fprintf(stderr, "mallctl(background_thread) in the fork window returned %d, want EAGAIN\n", window_rc);
        return 4;
    }
    bool st = true;
    size_t len = sizeof st;
    if (mallctl("background_thread", &st, &len, NULL, 0) != 0 || st) {
        fprintf(stderr, "the background thread is on after the refused request\n");
        return 5;
    }
    for (int k = 0; k < 12; k++) {
        bool v = (k & 1) == 0;
        if (mallctl("background_thread", NULL, NULL, &v, sizeof v) != 0)
            return 6;
        churn();
        int n = bg_threads_settled();
        if (n > 1) {
            fprintf(stderr, "%d challoc-bg threads\n", n);
            return 7;
        }
    }
    return 0;
}

int main(void)
{
    require_challoc();
    bool off = false;
    REQUIRE(mallctl("background_thread", NULL, NULL, &off, sizeof off) == 0, "background_thread off");
    for (int round = 0; round < 5; round++) {
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
    printf("ok fork_window\n");
    return 0;
}
