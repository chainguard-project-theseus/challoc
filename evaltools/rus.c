#define _GNU_SOURCE
#include <stdio.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
int main(int argc, char **argv) {
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC, &a);
    pid_t p = fork();
    if (p == 0) { execvp(argv[1], argv + 1); _exit(127); }
    int st; waitpid(p, &st, 0);
    clock_gettime(CLOCK_MONOTONIC, &b);
    struct rusage r; getrusage(RUSAGE_CHILDREN, &r);
    fprintf(stderr, "rus wall_s=%.3f user_s=%.3f sys_s=%.3f minflt=%ld majflt=%ld nvcsw=%ld nivcsw=%ld maxrss_kb=%ld\n",
            (b.tv_sec - a.tv_sec) + (b.tv_nsec - a.tv_nsec) / 1e9,
            r.ru_utime.tv_sec + r.ru_utime.tv_usec / 1e6, r.ru_stime.tv_sec + r.ru_stime.tv_usec / 1e6,
            r.ru_minflt, r.ru_majflt, r.ru_nvcsw, r.ru_nivcsw, r.ru_maxrss);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
