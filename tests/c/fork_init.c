/*
 * A fork while another thread initialises challoc (design 9, the gate
 * between init and fork). challoc is loaded late by `dlopen` into a
 * glibc-malloc process, so nothing has initialised it: thread A's first
 * call (`mallocx`) runs init while the main thread forks after a delay
 * that varies across the runs, and the child calls `mallocx` too. Before
 * the gate the child could copy `STATE` at INITIALISING with A's thread
 * ID and wait forever for a thread it does not have. Now the prepare
 * handler waits for an init under way, or keeps one from starting until
 * the fork is done, and every child's call completes (a SIGALRM watchdog
 * kills a child that hangs). Each run is a fresh process, since init
 * happens once per process. A second pass forks with challoc loaded but
 * not initialised and no other thread, where the only allocating code is
 * an atfork child handler registered before challoc's (before the
 * `dlopen`): it runs in the child before challoc's child handler opens
 * the gate, as the forking thread under a new kernel thread ID, and its
 * `mallocx` (challoc's init) must pass the gate. Run bare, with the library's path as the
 * argument, by the `api::` test
 * `atfork_handlers_of_linked_libraries_may_allocate`.
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

#define RUNS 30

typedef void *(*mallocx_fn)(size_t, int);
typedef void (*sdallocx_fn)(void *, size_t, int);

static mallocx_fn xmalloc;
static sdallocx_fn xfree;

static void *first_call(void *arg)
{
    (void)arg;
    void *p = xmalloc(64, 0);
    if (!p)
        _exit(5);
    xfree(p, 64, 0);
    return NULL;
}

static void pause_us(long us)
{
    struct timespec t = {us / 1000000, (us % 1000000) * 1000};
    while (nanosleep(&t, &t) != 0)
        ;
}

/* One run, in a fresh process: 0 when the child's call completed. */
static int one_run(const char *lib, long delay_us)
{
    void *h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 2;
    }
    xmalloc = (mallocx_fn)dlsym(h, "mallocx");
    xfree = (sdallocx_fn)dlsym(h, "sdallocx");
    if (!xmalloc || !xfree) {
        fprintf(stderr, "no mallocx/sdallocx in %s\n", lib);
        return 2;
    }
    pthread_t a;
    if (pthread_create(&a, NULL, first_call, NULL) != 0)
        return 2;
    pause_us(delay_us);
    pid_t pid = fork();
    if (pid < 0)
        return 2;
    if (pid == 0) {
        alarm(10);
        void *p = xmalloc(128, 0);
        if (!p)
            _exit(4);
        xfree(p, 128, 0);
        _exit(0);
    }
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        ;
    pthread_join(a, NULL);
    if (WIFSIGNALED(st) && WTERMSIG(st) == SIGALRM)
        return 3;
    return WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : 4;
}

static void child_first(void)
{
    alarm(10);
    void *p = xmalloc(64, 0);
    if (!p)
        _exit(4);
    xfree(p, 64, 0);
}

/* The second pass, in a fresh process: 0 when the child completed. */
static int child_handler_run(const char *lib)
{
    if (pthread_atfork(NULL, NULL, child_first) != 0)
        return 2;
    void *h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
    if (!h)
        return 2;
    xmalloc = (mallocx_fn)dlsym(h, "mallocx");
    xfree = (sdallocx_fn)dlsym(h, "sdallocx");
    if (!xmalloc || !xfree)
        return 2;
    pid_t pid = fork();
    if (pid < 0)
        return 2;
    if (pid == 0)
        _exit(0);
    int st = 0;
    while (waitpid(pid, &st, 0) != pid)
        ;
    if (WIFSIGNALED(st) && WTERMSIG(st) == SIGALRM)
        return 3;
    return WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : 4;
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: fork_init LIB\n");
        return 2;
    }
    if (dlsym(RTLD_DEFAULT, "challoc_version")) {
        fprintf(stderr, "challoc is already loaded: run without the preload\n");
        return 2;
    }
    int hung = 0, failed = 0;
    for (int run = 0; run < RUNS; run++) {
        long delay_us = run * 20;
        pid_t pid = fork();
        if (pid < 0)
            return 2;
        if (pid == 0)
            _exit(one_run(argv[1], delay_us));
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            ;
        int rc = WIFEXITED(st) ? WEXITSTATUS(st) : 100 + (WIFSIGNALED(st) ? WTERMSIG(st) : 0);
        if (rc == 3)
            hung++;
        else if (rc != 0) {
            failed++;
            fprintf(stderr, "run %d (delay %ld us): status %d\n", run, delay_us, rc);
        }
    }
    for (int run = 0; run < 5; run++) {
        pid_t pid = fork();
        if (pid < 0)
            return 2;
        if (pid == 0)
            _exit(child_handler_run(argv[1]));
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            ;
        int rc = WIFEXITED(st) ? WEXITSTATUS(st) : 100;
        if (rc == 3)
            hung++;
        else if (rc != 0) {
            failed++;
            fprintf(stderr, "child-handler run %d: status %d\n", run, rc);
        }
    }
    if (hung || failed) {
        fprintf(stderr, "fork during init: %d of %d children hung, %d failed\n", hung, RUNS + 5, failed);
        return 1;
    }
    printf("ok fork_init (%d runs, %d with a child handler)\n", RUNS, 5);
    return 0;
}
