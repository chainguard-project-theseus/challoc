/* execloop: run a command N times in sequence, each a fresh fork + exec,
 * and time the loop: the startup rows of bench/rows.toml (exec-to-exit of
 * a small program under the allocator, bench/drivers/startup.sh).
 *
 *   execloop N cmd [args...]
 *
 * The children inherit the environment (LD_PRELOAD, MALLOC_CONF), stdin is
 * /dev/null and their output goes to /dev/null, so a terminal or a log
 * file does not enter the timing. Any child that exits non-zero or dies on
 * a signal stops the loop with exit status 1. Prints one line:
 *   execloop n=N wall_s=W per_run_ms=P
 * The loop itself allocates nothing after its start. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: execloop N cmd [args...]\n");
        return 2;
    }
    char *end;
    long n = strtol(argv[1], &end, 10);
    if (*end || n < 1) {
        fprintf(stderr, "execloop: N must be a positive integer, not %s\n", argv[1]);
        return 2;
    }
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_addopen(&fa, 0, "/dev/null", O_RDONLY, 0);
    posix_spawn_file_actions_addopen(&fa, 1, "/dev/null", O_WRONLY, 0);
    posix_spawn_file_actions_adddup2(&fa, 1, 2);
    double t0 = now();
    for (long i = 0; i < n; i++) {
        pid_t pid;
        int rc = posix_spawnp(&pid, argv[2], &fa, NULL, &argv[2], environ);
        if (rc != 0) {
            fprintf(stderr, "execloop: %s: %s\n", argv[2], strerror(rc));
            return 1;
        }
        int st;
        while (waitpid(pid, &st, 0) < 0) {
            if (errno != EINTR) {
                perror("execloop: waitpid");
                return 1;
            }
        }
        if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
            fprintf(stderr, "execloop: run %ld of %s: %s %d\n", i + 1, argv[2], WIFEXITED(st) ? "exit" : "signal",
                    WIFEXITED(st) ? WEXITSTATUS(st) : WTERMSIG(st));
            return 1;
        }
    }
    double w = now() - t0;
    printf("execloop n=%ld wall_s=%.6f per_run_ms=%.4f\n", n, w, w * 1e3 / (double)n);
    return 0;
}
