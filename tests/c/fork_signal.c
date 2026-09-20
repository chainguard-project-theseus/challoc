/*
 * fork from a signal handler that runs while this thread is inside the fork
 * handlers with every allocator lock held. This is the race in glibc's
 * tst-mallocfork made deterministic: glibc restores the caller's signal mask
 * before the parent-side atfork handlers run, so a SIGALRM delivered there
 * (from the child that fork() just created, in glibc's test) runs its
 * handler with challoc's locks held by this very thread. If challoc did not
 * block signals for the duration of its handlers, the handler's fork would
 * re-enter the prepare handler and wait forever on the first class lock.
 *
 * To land inside the window on purpose, this program registers its own
 * atfork handlers before challoc initialises (registration order decides
 * the order handlers run: prepare in reverse, parent and child in order), so
 * its prepare handler runs after challoc's prepare and its parent and child
 * handlers run before challoc's release. Each of them raises SIGALRM. With
 * signals blocked the raise stays pending until challoc's release restores
 * the mask; the handler then forks once (disarming itself first) and the
 * grandchild exits cleanly. The handler records whether it ran inside one
 * of this program's atfork handlers: that would mean the raise was not
 * blocked, either because challoc's prepare had not run yet (challoc
 * initialised before main, so its handlers were registered first and the
 * ordering above does not hold) or because it did not block signals; both
 * fail the test. A watchdog timer on SIGUSR1 turns the deadlock into a
 * fast, loud failure instead of a hung test suite.
 */
#include "common.h"
#include <signal.h>
#include <sys/wait.h>
#include <time.h>

static volatile sig_atomic_t armed;      /* raise in the atfork handlers */
static volatile sig_atomic_t handled;    /* SIGALRM handler runs */
static volatile sig_atomic_t nested_ok;  /* the handler's fork+wait worked */
static volatile sig_atomic_t seen_prepare, seen_parent, seen_child;
static volatile sig_atomic_t in_atfork;   /* inside one of our atfork handlers */
static volatile sig_atomic_t ran_inside;  /* SIGALRM handler ran while in_atfork */

static void on_alarm(int signum)
{
    (void)signum;
    if (in_atfork)
        ran_inside++; /* the raise was not blocked: wrong handler order or no mask */
    armed = 0; /* the nested fork runs the atfork handlers again */
    pid_t child = fork();
    if (child == 0)
        _exit(0);
    int status = -1;
    if (child > 0 && TEMP_FAILURE_RETRY(waitpid(child, &status, 0)) == child && status == 0)
        nested_ok++;
    handled++;
}

static void on_prepare(void)
{
    seen_prepare++;
    in_atfork = 1;
    if (armed)
        raise(SIGALRM);
    in_atfork = 0;
}

static void on_parent(void)
{
    seen_parent++;
    in_atfork = 1;
    if (armed)
        raise(SIGALRM);
    in_atfork = 0;
}

static void on_child(void)
{
    seen_child++;
    in_atfork = 1;
    if (armed)
        raise(SIGALRM);
    in_atfork = 0;
}

static void on_watchdog(int signum)
{
    (void)signum;
    static const char msg[] = "FATAL fork_signal: deadlock in the fork handlers (watchdog)\n";
    (void)!write(2, msg, sizeof msg - 1);
    _exit(2);
}

int main(void)
{
    if (!challoc_version) {
        fprintf(stderr, "not running under challoc\n");
        return 1;
    }
    /* Nothing has allocated yet, so challoc has not initialised and our
     * handlers are registered ahead of its (the SIGALRM handler's
     * `ran_inside` check below verifies that). glibc keeps the first 48
     * handlers in a static buffer, so pthread_atfork itself does not
     * allocate. */
    REQUIRE(pthread_atfork(on_prepare, on_parent, on_child) == 0, "pthread_atfork");
    require_challoc();

    struct sigaction sa = {.sa_handler = on_alarm};
    sigemptyset(&sa.sa_mask);
    REQUIRE(sigaction(SIGALRM, &sa, NULL) == 0, "sigaction(SIGALRM)");
    struct sigaction wd = {.sa_handler = on_watchdog};
    sigemptyset(&wd.sa_mask);
    REQUIRE(sigaction(SIGUSR1, &wd, NULL) == 0, "sigaction(SIGUSR1)");
    timer_t timer;
    struct sigevent ev = {.sigev_notify = SIGEV_SIGNAL, .sigev_signo = SIGUSR1};
    REQUIRE(timer_create(CLOCK_MONOTONIC, &ev, &timer) == 0, "timer_create");
    struct itimerspec its = {.it_value = {.tv_sec = 20}};
    REQUIRE(timer_settime(timer, 0, &its, NULL) == 0, "timer_settime");

    for (int round = 0; round < 3; round++) {
        handled = 0;
        nested_ok = 0;
        armed = 1;
        pid_t child = fork();
        REQUIRE(child >= 0, "fork: %s", strerror(errno));
        if (child == 0) {
            /* on_child raised SIGALRM before challoc's release restored the
             * mask; the handler must have run exactly once by now. */
            int ok = seen_child >= 1 && handled == 1 && nested_ok == 1;
            void *volatile p = malloc(1000);
            if (!p)
                ok = 0;
            free(p);
            _exit(ok ? 0 : 3);
        }
        int status = -1;
        REQUIRE(TEMP_FAILURE_RETRY(waitpid(child, &status, 0)) == child, "waitpid");
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "round %d: child status 0x%x", round, status);
        /* Two raises (prepare, parent) were pending together, so one run. */
        CHECK(handled == 1, "round %d: handler ran %d times, expected 1", round, (int)handled);
        CHECK(nested_ok == 1, "round %d: nested fork failed", round);
        CHECK(armed == 0, "round %d: handler did not disarm", round);
        CHECK(ran_inside == 0, "round %d: SIGALRM delivered inside an atfork handler %d time(s): the raise was not blocked",
              round, (int)ran_inside);
    }
    CHECK(seen_prepare >= 3 && seen_parent >= 3, "atfork handlers ran %d/%d times", (int)seen_prepare,
          (int)seen_parent);

    /* Every lock must be free again: allocate in each tier. */
    static const size_t ns[] = {1, 100, 4096, 70000, 3 << 20};
    for (size_t i = 0; i < sizeof ns / sizeof ns[0]; i++) {
        void *volatile p = malloc(ns[i]);
        CHECK(p != NULL, "malloc(%zu) after forks", ns[i]);
        free(p);
    }
    timer_delete(timer);
    return finish("fork_signal");
}
