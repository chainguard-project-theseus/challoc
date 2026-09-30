/* rt: run a command, report its wall time, rusage and (optionally) hardware
 * counters. Wolfi has no GNU time, so this replaces `/usr/bin/time -v`.
 * Invoke as
 *   rt env LD_PRELOAD=... prog args
 * so the allocator under test is loaded into `prog`, not into rt.
 *
 * Output goes to $RT_OUT (appended) or stderr:
 *
 *   default      one line "elapsed user sys maxrss_kb exit" (the format the
 *                shell harnesses under bench/ read; no counters are opened)
 *   RT_JSON=1    one JSON object per run (bench/cb reads it):
 *                {"wall_s":..,"user_s":..,"sys_s":..,"maxrss_kib":..,"exit":..,
 *                 "vcsw":..,"ivcsw":..,"pmc":{...}|null,"pmc_note":".."}
 *
 * Counters (RT_JSON=1 unless RT_PMU=0): two perf_event_open groups on the
 * child, opened between fork and exec (the child waits on a pipe), with
 * inherit (every thread and child process of the command is counted) and
 * enable_on_exec (rt's own exec setup is not):
 *   user group    cycles, instructions, L1D read misses, dTLB read misses
 *                 (exclude_kernel); the leader is cycles
 *   kernel group  cycles, instructions (exclude_user); needs
 *                 perf_event_paranoid <= 1 or CAP_PERFMON, skipped otherwise
 *   software      cpu-migrations, context-switches (no PMU needed)
 * Each counter is read on its own fd (no PERF_FORMAT_GROUP, which older
 * kernels refuse with inherit) with the enabled/running times, and scaled
 * when the PMU multiplexed it ("multiplexed": true). A counter that cannot
 * be opened is left out; when the user group's leader cannot be opened
 * (no PMU on the host, EPERM under a container's seccomp profile, EACCES
 * from perf_event_paranoid) "pmc" carries only what did open (possibly
 * nothing: null) and "pmc_note" says why. The run itself never depends on
 * the counters.
 *
 *   rt --probe   the old bench/gce/pmu-probe.c: count cycles and
 *                instructions of a spin loop in this process and print
 *                "pmu cycles=<n> instructions=<n> [..._open_errno=..] paranoid=<p>";
 *                exit 0 when cycles counted. For hosts without a perf binary.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/perf_event.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static double secs(struct timeval tv) { return tv.tv_sec + tv.tv_usec / 1e6; }

static int perf_open(struct perf_event_attr *a, pid_t pid, int group) {
    return (int)syscall(__NR_perf_event_open, a, pid, -1, group, PERF_FLAG_FD_CLOEXEC);
}

static int paranoid(void) {
    FILE *f = fopen("/proc/sys/kernel/perf_event_paranoid", "r");
    int p = -99;
    if (f) {
        if (fscanf(f, "%d", &p) != 1) p = -99;
        fclose(f);
    }
    return p;
}

#define CACHE(c) ((c) | (PERF_COUNT_HW_CACHE_OP_READ << 8) | (PERF_COUNT_HW_CACHE_RESULT_MISS << 16))

struct ctr {
    const char *name;
    uint32_t type;
    uint64_t config;
    int group;  /* 0 user group, 1 kernel group, 2 software (ungrouped) */
    int fd;
    int err;
};

static struct ctr ctrs[] = {
    {"cycles", PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES, 0, -1, 0},
    {"instructions", PERF_TYPE_HARDWARE, PERF_COUNT_HW_INSTRUCTIONS, 0, -1, 0},
    {"l1d_miss", PERF_TYPE_HW_CACHE, CACHE(PERF_COUNT_HW_CACHE_L1D), 0, -1, 0},
    {"dtlb_miss", PERF_TYPE_HW_CACHE, CACHE(PERF_COUNT_HW_CACHE_DTLB), 0, -1, 0},
    {"cycles_k", PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES, 1, -1, 0},
    {"instructions_k", PERF_TYPE_HARDWARE, PERF_COUNT_HW_INSTRUCTIONS, 1, -1, 0},
    {"migrations", PERF_TYPE_SOFTWARE, PERF_COUNT_SW_CPU_MIGRATIONS, 2, -1, 0},
    {"ctx_switches", PERF_TYPE_SOFTWARE, PERF_COUNT_SW_CONTEXT_SWITCHES, 2, -1, 0},
};
#define NCTR (sizeof ctrs / sizeof ctrs[0])

static void open_counters(pid_t pid) {
    int leader[2] = {-1, -1};
    for (size_t i = 0; i < NCTR; i++) {
        struct ctr *c = &ctrs[i];
        struct perf_event_attr a;
        memset(&a, 0, sizeof a);
        a.size = sizeof a;
        a.type = c->type;
        a.config = c->config;
        a.inherit = 1;
        a.exclude_hv = 1;
        a.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
        if (c->group == 0) a.exclude_kernel = 1;
        if (c->group == 1) a.exclude_user = 1;
        int g = c->group < 2 ? leader[c->group] : -1;
        if (g < 0) {
            /* A leader (or an ungrouped counter) starts disabled and is
             * enabled by the exec; members follow their leader. */
            a.disabled = 1;
            a.enable_on_exec = 1;
        }
        if (c->group < 2 && g < 0 && i > 0 && ctrs[i - 1].group == c->group) {
            /* the group's leader failed: the members are not opened */
            c->err = ctrs[i - 1].err;
            continue;
        }
        c->fd = perf_open(&a, pid, g);
        if (c->fd < 0) {
            c->err = errno;
            continue;
        }
        if (c->group < 2 && g < 0) leader[c->group] = c->fd;
    }
}

static const char *errname(int e) {
    switch (e) {
    case ENOENT: return "ENOENT";
    case ENODEV: return "ENODEV";
    case EOPNOTSUPP: return "EOPNOTSUPP";
    case EACCES: return "EACCES";
    case EPERM: return "EPERM";
    case ENOSYS: return "ENOSYS";
    case EINVAL: return "EINVAL";
    case EBUSY: return "EBUSY";
    default: return "errno";
    }
}

/* Appends the "pmc" and "pmc_note" members to f. */
static void print_counters(FILE *f) {
    int any = 0, mux = 0, have_k = 0;
    fprintf(f, ",\"pmc\":");
    for (size_t i = 0; i < NCTR; i++) {
        struct ctr *c = &ctrs[i];
        if (c->fd < 0) continue;
        uint64_t v[3];
        if (read(c->fd, v, sizeof v) != (ssize_t)sizeof v) continue;
        close(c->fd);
        c->fd = -1;
        if (v[2] == 0) continue; /* never scheduled */
        double val = (double)v[0];
        if (v[2] < v[1]) {
            val = val * (double)v[1] / (double)v[2];
            mux = 1;
        }
        fprintf(f, "%s\"%s\":%.0f", any ? "," : "{", c->name, val);
        any = 1;
        if (c->group == 1) have_k = 1;
    }
    if (any) {
        int have_u = ctrs[0].err == 0;
        fprintf(f, ",\"scope\":\"%s\",\"multiplexed\":%s}", have_u && have_k ? "uk" : have_u ? "u" : have_k ? "k" : "sw", mux ? "true" : "false");
    } else {
        fprintf(f, "null");
    }
    /* The note names every counter that did not open, once per errno. */
    fprintf(f, ",\"pmc_note\":\"");
    int first = 1;
    for (size_t i = 0; i < NCTR; i++) {
        if (!ctrs[i].err) continue;
        fprintf(f, "%s%s:%s", first ? "" : " ", ctrs[i].name, errname(ctrs[i].err));
        first = 0;
    }
    if (!first) fprintf(f, " paranoid=%d", paranoid());
    fprintf(f, "\"");
}

static int probe(void) {
    struct perf_event_attr a;
    int fd[2], err[2] = {0, 0};
    uint64_t cfg[2] = {PERF_COUNT_HW_CPU_CYCLES, PERF_COUNT_HW_INSTRUCTIONS};
    for (int i = 0; i < 2; i++) {
        memset(&a, 0, sizeof a);
        a.type = PERF_TYPE_HARDWARE;
        a.size = sizeof a;
        a.config = cfg[i];
        a.disabled = 1;
        a.exclude_kernel = 1;
        a.exclude_hv = 1;
        fd[i] = perf_open(&a, 0, -1);
        if (fd[i] < 0) err[i] = errno;
    }
    volatile unsigned long x = 1;
    for (int i = 0; i < 2; i++)
        if (fd[i] >= 0) ioctl(fd[i], PERF_EVENT_IOC_ENABLE, 0);
    for (unsigned long i = 0; i < 100000000UL; i++) x = x * 3 + i;
    long long v[2] = {-1, -1};
    for (int i = 0; i < 2; i++) {
        if (fd[i] < 0) continue;
        ioctl(fd[i], PERF_EVENT_IOC_DISABLE, 0);
        if (read(fd[i], &v[i], sizeof v[i]) != sizeof v[i]) v[i] = -1;
    }
    printf("pmu cycles=%lld instructions=%lld", v[0], v[1]);
    if (err[0]) printf(" cycles_open_errno=%d(%s)", err[0], strerror(err[0]));
    if (err[1]) printf(" instructions_open_errno=%d(%s)", err[1], strerror(err[1]));
    printf(" paranoid=%d x=%lu\n", paranoid(), (unsigned long)x);
    return (fd[0] >= 0 && v[0] > 0) ? 0 : 1;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--probe") == 0) return probe();
    if (argc < 2) {
        fprintf(stderr, "usage: rt cmd [args...] | rt --probe\n");
        return 2;
    }
    const char *js = getenv("RT_JSON");
    int json = js && *js && strcmp(js, "0") != 0;
    const char *pm = getenv("RT_PMU");
    int pmu = json && !(pm && strcmp(pm, "0") == 0);
    int sync[2];
    if (pipe2(sync, O_CLOEXEC) < 0) {
        perror("pipe");
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
        char c;
        close(sync[1]);
        /* Wait until the parent has attached the counters (or given up). */
        while (read(sync[0], &c, 1) < 0 && errno == EINTR) {
        }
        execvp(argv[1], argv + 1);
        perror("execvp");
        _exit(127);
    }
    close(sync[0]);
    if (pmu) open_counters(pid);
    close(sync[1]); /* EOF releases the child */
    int st = 0;
    struct rusage ru;
    while (wait4(pid, &st, 0, &ru) < 0) {
        if (errno != EINTR) {
            perror("wait4");
            return 2;
        }
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
    if (json) {
        fprintf(f, "{\"wall_s\":%.6f,\"user_s\":%.3f,\"sys_s\":%.3f,\"maxrss_kib\":%ld,\"exit\":%d,\"signal\":%d,\"vcsw\":%ld,\"ivcsw\":%ld",
                elapsed, secs(ru.ru_utime), secs(ru.ru_stime), ru.ru_maxrss, code,
                WIFSIGNALED(st) ? WTERMSIG(st) : 0, ru.ru_nvcsw, ru.ru_nivcsw);
        if (pmu) print_counters(f);
        else fprintf(f, ",\"pmc\":null,\"pmc_note\":\"disabled\"");
        fprintf(f, "}\n");
    } else {
        fprintf(f, "%.3f %.3f %.3f %ld %d\n", elapsed, secs(ru.ru_utime),
                secs(ru.ru_stime), ru.ru_maxrss, code);
    }
    if (out) fclose(f);
    return code;
}
