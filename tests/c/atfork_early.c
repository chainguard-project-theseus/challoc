/*
 * Atfork handlers registered before challoc initialises, which allocate.
 * glibc runs prepare handlers in the reverse order of registration and
 * parent and child handlers in order, so these run on the forking thread
 * while challoc's handlers hold every allocator lock: the prepare after
 * challoc's prepare, the parent and child before challoc's release. Their
 * allocations take the held locks as acquired and their releases do
 * nothing (the fork window, design 9), so `fork` returns in both
 * processes. The prepare handler allocates in every tier (a cold small
 * class, a large class, huge), the parent and child handlers allocate too.
 *
 * By default the program's .preinit_array registers the handlers
 * before its first allocation. With -DLINKED_LIB they come from
 * tests/c/atfork_lib.c, a library the program links, whose constructor
 * runs before challoc initialises (the `api::` test
 * `atfork_handlers_of_linked_libraries_may_allocate`).
 */
#include "common.h"
#include <spawn.h>

extern char **environ;

#ifdef LINKED_LIB
extern int atfork_lib_prepared, atfork_lib_parent, atfork_lib_child;
#define PREPARED atfork_lib_prepared
#define PARENT_AFTER atfork_lib_parent
#define CHILD_AFTER atfork_lib_child
#define NAME "atfork_early (linked library)"
#else
static volatile int prepared, parent_after, child_after;
#define PREPARED prepared
#define PARENT_AFTER parent_after
#define CHILD_AFTER child_after
#define NAME "atfork_early"

static void prepare(void)
{
    static const size_t sizes[] = {24, 3000, 40000, 300000, 3 << 20};
    void *p[sizeof sizes / sizeof sizes[0]];
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        p[i] = malloc(sizes[i]);
        if (p[i])
            memset(p[i], 0x5a, 16);
    }
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++)
        free(p[i]);
    prepared++;
}

static void parent(void)
{
    free(malloc(5000));
    parent_after++;
}

static void child(void)
{
    free(malloc(5000));
    child_after++;
}

/* Registered from the program's .preinit_array, which runs before every
 * library's constructor, challoc's among them: before challoc registers
 * its own handlers, so these run inside challoc's fork window (design 9).
 * A handler registered later, from a constructor, runs outside it
 * (tests/c/atfork_pool.c). */
static void register_first(void)
{
    /* No allocation before this: glibc's pthread_atfork does not allocate
     * for the first handlers. */
    if (pthread_atfork(prepare, parent, child) != 0)
        _exit(3);
}

__attribute__((section(".preinit_array"), used)) static void (*const preinit_register)(void) = register_first;
#endif

static int work(void)
{
    require_challoc();
    for (int round = 0; round < 3; round++) {
        pid_t pid = fork();
        REQUIRE(pid >= 0, "fork: %s", strerror(errno));
        if (pid == 0) {
            void *p = malloc(5000);
            free(p);
            _exit(CHILD_AFTER == 1 ? 0 : 2);
        }
        int st = 0;
        while (waitpid(pid, &st, 0) != pid)
            REQUIRE(errno == EINTR, "waitpid: %s", strerror(errno));
        CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "child status %#x", st);
    }
    CHECK(PREPARED == 3 && PARENT_AFTER == 3, "prepare %d parent %d", PREPARED, PARENT_AFTER);
    return failures ? 1 : 0;
}

/* The forks run in a worker process this one watches: a deadlock in
 * `fork` holds every signal blocked (the fork handlers block them), so
 * the worker cannot time itself out. `posix_spawn` runs no atfork
 * handler, so the watcher never forks through the handlers under test. */
int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "work") == 0)
        return work();
    require_challoc();
    char *args[] = {argv[0], "work", NULL};
    pid_t pid;
    REQUIRE(posix_spawn(&pid, "/proc/self/exe", NULL, NULL, args, environ) == 0, "posix_spawn");
    int st = 0;
    long t0 = now_ms();
    for (;;) {
        pid_t r = waitpid(pid, &st, WNOHANG);
        if (r == pid)
            break;
        REQUIRE(r == 0 || errno == EINTR, "waitpid: %s", strerror(errno));
        if (now_ms() - t0 > 20000) {
            kill(pid, SIGKILL);
            waitpid(pid, &st, 0);
            CHECK(0, "fork did not return within 20 s: deadlock in the fork handlers");
            break;
        }
        sleep_ms(20);
    }
    if (!failures)
        CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "worker status %#x", st);
    if (failures)
        return 1;
    printf("ok %s\n", NAME);
    return 0;
}
