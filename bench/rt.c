/* rt: run a command and append "elapsed user sys maxrss_kb exit" to $RT_OUT
 * (or stderr). Wolfi has no GNU time, so this replaces `/usr/bin/time -v`
 * for the benchmark protocol in docs/plan.md. Invoke as
 *   rt env LD_PRELOAD=... prog args
 * so the allocator under test is loaded into `prog`, not into rt. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static double secs(struct timeval tv) { return tv.tv_sec + tv.tv_usec / 1e6; }

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: rt cmd [args...]\n");
        return 2;
    }
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 2;
    }
    if (pid == 0) {
        execvp(argv[1], argv + 1);
        perror("execvp");
        _exit(127);
    }
    int st = 0;
    struct rusage ru;
    if (wait4(pid, &st, 0, &ru) < 0) {
        perror("wait4");
        return 2;
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double elapsed = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9;
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
    const char *out = getenv("RT_OUT");
    FILE *f = out ? fopen(out, "a") : stderr;
    if (!f) {
        perror("RT_OUT");
        return 2;
    }
    fprintf(f, "%.3f %.3f %.3f %ld %d\n", elapsed, secs(ru.ru_utime),
            secs(ru.ru_stime), ru.ru_maxrss, code);
    if (out) fclose(f);
    return code;
}
