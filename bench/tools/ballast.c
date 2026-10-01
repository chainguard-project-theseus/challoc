/* ballast: run a program with its cgroup filled to PCT percent of the
 * memory limit.
 *
 *   ballast PCT PROG [ARGS...]
 *
 * Reads the effective limit of the cgroup v2 the process is in (the
 * smaller of memory.max and memory.high) and memory.current, forks a child
 * that maps and touches limit * PCT / 100 - current bytes of anonymous
 * memory and then sleeps, and, once that child has touched every page,
 * runs PROG as a second child. The program runs with the cgroup at or
 * above PCT percent of its limit: the rows that measure an allocator's
 * behaviour near a container's limit (the cgroup policy's bands, design
 * 11). When PROG exits the ballast child is killed and reaped before
 * ballast exits with PROG's status, so the next run starts from an
 * uncharged cgroup. One line on stderr,
 *   cb-ballast limit_mib=<L> current_mib=<C> ballast_mib=<B>
 * records what was set up. Exits 2 when there is no limit, or the cgroup
 * is already above PCT percent. The ballast is a child: its pages count
 * in a parent's RUSAGE_CHILDREN peak and its page touching in counters
 * that follow children, so neither is a figure of PROG. */
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

#define CG "/sys/fs/cgroup/"

/* A cgroup v2 file holding one number or "max"; UINT64_MAX for "max" or
 * a missing file. */
static uint64_t read_u64(const char *name) {
    char buf[64];
    FILE *f = fopen(name, "r");
    if (!f) return UINT64_MAX;
    char *s = fgets(buf, sizeof buf, f);
    fclose(f);
    if (!s || !strncmp(buf, "max", 3)) return UINT64_MAX;
    return strtoull(buf, NULL, 10);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: ballast PCT PROG [ARGS...]\n");
        return 2;
    }
    long pct = strtol(argv[1], NULL, 10);
    uint64_t max = read_u64(CG "memory.max"), high = read_u64(CG "memory.high");
    uint64_t limit = max < high ? max : high;
    uint64_t current = read_u64(CG "memory.current");
    if (limit == UINT64_MAX || current == UINT64_MAX || pct <= 0 || pct >= 100) {
        fprintf(stderr, "ballast: no cgroup v2 memory limit at " CG " (or a bad PCT)\n");
        return 2;
    }
    uint64_t want = limit / 100 * (uint64_t)pct;
    if (current >= want) {
        fprintf(stderr, "ballast: usage %llu MiB already at %ld%% of %llu MiB\n",
                (unsigned long long)(current >> 20), pct, (unsigned long long)(limit >> 20));
        return 2;
    }
    size_t size = (size_t)(want - current);
    int fd[2];
    if (pipe(fd) != 0) {
        perror("ballast: pipe");
        return 2;
    }
    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid < 0) {
        perror("ballast: fork");
        return 2;
    }
    if (pid == 0) {
        close(fd[0]);
        prctl(PR_SET_PDEATHSIG, SIGKILL);
        if (getppid() != parent) _exit(0);
        char *p = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) _exit(1);
        madvise(p, size, MADV_NOHUGEPAGE);
        long page = sysconf(_SC_PAGESIZE);
        for (size_t i = 0; i < size; i += (size_t)page) p[i] = 1;
        char ok = 1;
        if (write(fd[1], &ok, 1) != 1) _exit(1);
        close(fd[1]);
        for (;;) pause();
    }
    close(fd[1]);
    char ok = 0;
    if (read(fd[0], &ok, 1) != 1 || ok != 1) {
        fprintf(stderr, "ballast: the child failed to map %zu bytes\n", size);
        kill(pid, SIGKILL);
        return 2;
    }
    close(fd[0]);
    fprintf(stderr, "cb-ballast limit_mib=%llu current_mib=%llu ballast_mib=%llu\n",
            (unsigned long long)(limit >> 20), (unsigned long long)(current >> 20),
            (unsigned long long)(size >> 20));
    pid_t prog = fork();
    if (prog < 0) {
        perror("ballast: fork");
        kill(pid, SIGKILL);
        return 2;
    }
    if (prog == 0) {
        execvp(argv[2], argv + 2);
        perror("ballast: exec");
        _exit(127);
    }
    int st = 0;
    while (waitpid(prog, &st, 0) < 0) {}
    kill(pid, SIGKILL);
    while (waitpid(pid, NULL, 0) < 0) {}
    if (WIFSIGNALED(st)) return 128 + WTERMSIG(st);
    return WEXITSTATUS(st);
}
