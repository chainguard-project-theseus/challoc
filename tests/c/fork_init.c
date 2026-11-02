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
 * happens once per process. Run bare, with the library's path as the
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
    if (hung || failed) {
        fprintf(stderr, "fork during init: %d of %d children hung, %d failed\n", hung, RUNS, failed);
        return 1;
    }
    printf("ok fork_init (%d runs)\n", RUNS);
    return 0;
}
