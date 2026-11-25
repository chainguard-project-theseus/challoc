/*
 * A fork before the program's first allocation, with atfork handlers
 * registered from the program's .preinit_array (before challoc's
 * constructor registers its own) that allocate (design 9, the gate
 * between init and fork). challoc's prepare handler finds no init under
 * way and closes the gate; then the forking thread's own handler
 * initialises challoc: in the parent from the prepare handler
 * (argument `prepare`), or in the child from the child handler, which
 * runs before challoc's opens the gate again (argument `child`). Both
 * must pass the gate: the forking thread is recognised by
 * `pthread_self`, which it keeps in the child (its kernel thread ID is
 * new there).
 *
 * Argument `signal` (#949): the prepare handler first raises SIGUSR1,
 * whose handler forks and reaps its child, while the outer fork holds
 * the gate closed with this thread in its forker slot (challoc's prepare
 * returned without blocking signals: challoc is not initialised). The
 * nested fork's prepare handler must see the slot as this thread's and
 * return rather than wait for the outer fork, which cannot go on until
 * the signal handler returns. This handler does not allocate inside the
 * nested fork, so challoc is still uninitialised when the nested fork's
 * parent handler opens the outer fork's gate, to `GATE_IDLE` (design 9,
 * residuals); the outer prepare handler's allocation afterwards is then
 * an ordinary init, and both forks return.
 *
 * A watchdog (SIGALRM, default action) fails a hang. Nothing allocates
 * before the fork: no stdio, and the check that challoc is preloaded
 * comes after it.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int mode_child, mode_signal;
static volatile sig_atomic_t raised, nested_status = -1;

static void touch(void)
{
    void *volatile p = malloc(48);
    if (!p)
        _exit(5);
    free(p);
}

/* SIGUSR1: fork from inside the outer fork's prepare handlers. */
static void on_usr1(int sig)
{
    (void)sig;
    pid_t pid = fork();
    if (pid == 0)
        _exit(0);
    int st = -1;
    if (pid > 0)
        while (waitpid(pid, &st, 0) != pid)
            ;
    nested_status = pid > 0 && WIFEXITED(st) ? WEXITSTATUS(st) : 100;
}

static void prepare_handler(void)
{
    if (mode_signal && raised == 1)
        return; /* the nested fork's turn: no allocation inside it */
    if (mode_signal && !raised) {
        raised = 1;
        raise(SIGUSR1);
        raised = 2;
    }
    if (!mode_child)
        touch();
}

static void child_handler(void)
{
    if (mode_child)
        touch();
}

/* .preinit_array entries get (argc, argv, envp): the mode is argv[1]. */
static void register_first(int argc, char **argv, char **envp)
{
    (void)envp;
    mode_child = argc > 1 && strcmp(argv[1], "child") == 0;
    mode_signal = argc > 1 && strcmp(argv[1], "signal") == 0;
    if (pthread_atfork(prepare_handler, NULL, child_handler) != 0)
        _exit(3);
}

__attribute__((section(".preinit_array"), used)) static void (*const preinit_register)(int, char **, char **) = register_first;

int main(void)
{
    alarm(20);
    if (mode_signal) {
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = on_usr1;
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGUSR1, &sa, NULL) != 0)
            return 2;
    }
    pid_t pid = fork();
    if (pid < 0)
        return 2;
    if (pid == 0) {
        touch();
        _exit(0);
    }
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        ;
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        fprintf(stderr, "child status %#x\n", st);
        return 1;
    }
    if (mode_signal && nested_status != 0) {
        fprintf(stderr, "nested fork status %d\n", (int)nested_status);
        return 1;
    }
    /* After the fork (dlsym may allocate): a run without the preload
     * fails, as require_challoc would make it. */
    if (!dlsym(RTLD_DEFAULT, "challoc_version")) {
        fprintf(stderr, "challoc is not preloaded\n");
        return 1;
    }
    printf("fork_first: %s handler\nok fork_first\n", mode_signal ? "signal" : mode_child ? "child" : "prepare");
    return 0;
}
