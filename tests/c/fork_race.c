/*
 * Two threads fork at once before challoc is initialised (#949). The
 * main thread forks first, before the program's first allocation:
 * challoc's prepare handler finds no init under way, closes the init/fork
 * gate and returns without taking the locks. Then this program's prepare
 * handler (registered from .preinit_array, so it runs after challoc's)
 * initialises challoc and starts a second thread, which forks with a
 * different signal mask while the main thread is still inside its own
 * fork's handlers. glibc runs the handlers of concurrent forks without
 * serialising them (its atfork lock is not held while a handler runs).
 *
 * challoc's handlers must keep the two forks apart: the second fork's
 * prepare handler waits until the first fork's parent handler has opened
 * the gate, and a parent handler releases only the locks its own prepare
 * handler took. Before #949 the second prepare handler went on, found
 * challoc initialised and took the locks, and the first fork's parent
 * handler, reading a process-wide "the prepare took the locks" flag,
 * released them and restored the second thread's saved signal mask onto
 * the main thread. The main thread's mask after `fork` is one check; the
 * first child allocating in every tier (a lock the second fork held at
 * the first fork's _Fork would stay held there) is the other.
 *
 * The program's prepare handler waits a bounded time for the second
 * thread's handlers to report that they ran inside the window, and the
 * output says whether they did (`overlap`, which challoc must not allow)
 * or not (`apart`). "Inside" is `a_window`: set by the main thread's
 * prepare handler before it starts the second thread and cleared by its
 * parent handler, which runs before challoc's opens the gate, so a
 * second prepare that challoc let through only after the gate opened
 * never sees it set. A watchdog (SIGALRM, default action) fails a hang.
 * Nothing allocates before the first fork: no stdio, and the check that
 * challoc is preloaded comes after it.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pthread_t main_thread;
static pthread_t second;
static volatile int started;
static int b_prepared, b_overlap, a_window, a_returned;
static pid_t b_child = -1;
static int b_status = -1;

static void pause_ms(long ms)
{
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0)
        ;
}

/* Wait up to `ms` for `*flag`. */
static int wait_flag(int *flag, long ms)
{
    for (long i = 0; i < ms; i++) {
        if (__atomic_load_n(flag, __ATOMIC_ACQUIRE))
            return 1;
        pause_ms(1);
    }
    return __atomic_load_n(flag, __ATOMIC_ACQUIRE);
}

static void mask_only(int signo)
{
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, signo);
    pthread_sigmask(SIG_SETMASK, &s, NULL);
}

static void *second_main(void *arg)
{
    (void)arg;
    mask_only(SIGUSR2);
    pid_t pid = fork();
    if (pid == 0)
        _exit(0);
    b_child = pid;
    if (pid > 0)
        while (waitpid(pid, &b_status, 0) != pid)
            ;
    return NULL;
}

static void prepare_handler(void)
{
    if (!pthread_equal(pthread_self(), main_thread)) {
        __atomic_store_n(&b_overlap, __atomic_load_n(&a_window, __ATOMIC_ACQUIRE), __ATOMIC_RELAXED);
        __atomic_store_n(&b_prepared, 1, __ATOMIC_RELEASE);
        return;
    }
    if (started)
        return;
    started = 1;
    /* The first allocation: challoc initialises here, inside the main
     * thread's fork, with the gate closed by this fork. */
    void *volatile p = malloc(48);
    if (!p)
        _exit(5);
    free(p);
    __atomic_store_n(&a_window, 1, __ATOMIC_RELEASE);
    if (pthread_create(&second, NULL, second_main, NULL) != 0)
        _exit(6);
    /* Give the second fork's handlers a chance to run inside this one. */
    wait_flag(&b_prepared, 1000);
}

static void parent_handler(void)
{
    /* The second fork's parent handlers wait for the main thread's fork
     * to return, so a release by the main thread's parent handler of
     * locks it never took would happen while the second fork holds them. */
    if (!pthread_equal(pthread_self(), main_thread))
        wait_flag(&a_returned, 2000);
    else
        __atomic_store_n(&a_window, 0, __ATOMIC_RELEASE);
}

static void register_first(void)
{
    if (pthread_atfork(prepare_handler, parent_handler, NULL) != 0)
        _exit(3);
}

__attribute__((section(".preinit_array"), used)) static void (*const preinit_register)(void) = register_first;

int main(void)
{
    alarm(20);
    main_thread = pthread_self();
    mask_only(SIGUSR1);
    pid_t pid = fork();
    if (pid == 0) {
        /* The child allocates in every tier, under its own watchdog (an
         * alarm is not inherited): a lock the second fork held at this
         * fork's _Fork would be held here for good. */
        alarm(5);
        static const size_t sizes[] = {24, 3000, 40000, 300000, 3 << 20};
        for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
            void *volatile p = malloc(sizes[i]);
            if (!p)
                _exit(7);
            free(p);
        }
        _exit(0);
    }
    sigset_t now;
    pthread_sigmask(SIG_SETMASK, NULL, &now);
    __atomic_store_n(&a_returned, 1, __ATOMIC_RELEASE);
    if (pid < 0)
        return 2;
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        ;
    if (started)
        pthread_join(second, NULL);
    int overlap = __atomic_load_n(&b_overlap, __ATOMIC_RELAXED);
    int ok = 1;
    if (sigismember(&now, SIGUSR1) != 1 || sigismember(&now, SIGUSR2) != 0) {
        fprintf(stderr, "fork_race: the main thread's mask after fork is not its own (SIGUSR1 %d, SIGUSR2 %d)\n",
                sigismember(&now, SIGUSR1), sigismember(&now, SIGUSR2));
        ok = 0;
    }
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        fprintf(stderr, "fork_race: first child status %#x\n", st);
        ok = 0;
    }
    if (!started || b_child <= 0 || !WIFEXITED(b_status) || WEXITSTATUS(b_status) != 0) {
        fprintf(stderr, "fork_race: second fork failed (started %d, pid %d, status %#x)\n", started, (int)b_child,
                b_status);
        ok = 0;
    }
    if (!dlsym(RTLD_DEFAULT, "challoc_version")) {
        fprintf(stderr, "challoc is not preloaded\n");
        return 1;
    }
    printf("fork_race: %s\n", overlap ? "overlap" : "apart");
    if (!ok)
        return 1;
    printf("ok fork_race\n");
    return 0;
}
