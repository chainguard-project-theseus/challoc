/*
 * psi_return: how long a neighbour in the same memory cgroup waits for
 * memory the allocator under test has freed but not yet returned (the
 * E-psi experiment, issue #19).
 *
 *   psi_return [cycles] [high_mib] [low_mib] [child_mib] [cg_high_mib]
 *
 * The process first moves itself into a child cgroup `cb-work` of its own
 * cgroup with memory.high = cg_high_mib (a privileged container: the
 * container's cgroup is the root of its cgroup namespace, where a host
 * mounted with nsdelegate refuses the PSI trigger and memory.high, so
 * every process of the container is moved to a sibling `cb-init` first)
 * and re-executes itself, so the allocator discovers `cb-work` at init.
 * When that is impossible it runs where it is and says so (cgroup=own).
 *
 * Each cycle: the parent (the allocator under test, malloc only)
 * allocates a mixed-size live set up to high_mib, frees random objects
 * down to low_mib and goes idle; a forked child then touches child_mib of
 * fresh anonymous memory (mmap, no malloc). low + child fits under
 * memory.high only once the parent has returned what it freed, so the
 * child is throttled (a memory stall, what the PSI trigger watches) until
 * the allocator gives the pages back. return_ms is the child's time to
 * touch its memory; the same touch with nothing retained takes base_ms.
 *
 * One line: result return_ms_p50=.. return_ms_p99=.. return_ms_mean=..
 * base_ms=.. cg_peak_kb=.. rss_peak_kb=.. psi_events=.. high_events=..
 * cgroup=work|own.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MIB (1024UL * 1024UL)
enum { MAX_OBJECTS = 2000000, MAX_CYCLES = 1000 };

static void *objs[MAX_OBJECTS];
static uint32_t sizes[MAX_OBJECTS];
static size_t nobjs, live;
static char cgdir[4096];
static uint64_t rng = 0x9e3779b97f4a7c15ULL;

static uint64_t next(void) {
    rng ^= rng << 13;
    rng ^= rng >> 7;
    rng ^= rng << 17;
    return rng;
}

static double now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static int write_file(const char *path, const char *s) {
    int fd = open(path, O_WRONLY);
    if (fd < 0)
        return -1;
    ssize_t n = write(fd, s, strlen(s));
    int e = errno;
    close(fd);
    errno = e;
    return n == (ssize_t)strlen(s) ? 0 : -1;
}

static long long read_ll(const char *path, const char *key) {
    char buf[8192];
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return -1;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0)
        return -1;
    buf[n] = 0;
    if (!key)
        return strncmp(buf, "max", 3) == 0 ? -1 : atoll(buf);
    for (char *s = buf; (s = strstr(s, key)); s++)
        if ((s == buf || s[-1] == '\n') && s[strlen(key)] == ' ')
            return atoll(s + strlen(key) + 1);
    return -1;
}

static long long cg(const char *file, const char *key) {
    char p[4200];
    snprintf(p, sizeof p, "%s/%s", cgdir, file);
    return read_ll(p, key);
}

/* The cgroup2 directory of this process (cgroupns: "0::/..." under
 * /sys/fs/cgroup). */
static void find_cgroup(void) {
    char line[2048] = "";
    FILE *f = fopen("/proc/self/cgroup", "r");
    if (f) {
        while (fgets(line, sizeof line, f))
            if (strncmp(line, "0::", 3) == 0)
                break;
        fclose(f);
    }
    line[strcspn(line, "\n")] = 0;
    const char *rel = strncmp(line, "0::", 3) == 0 ? line + 3 : "/";
    snprintf(cgdir, sizeof cgdir, "/sys/fs/cgroup%s", strcmp(rel, "/") == 0 ? "" : rel);
}

/* Move every process of the namespace root into cb-init, enable the
 * memory controller below the root and put ourselves into cb-work with
 * memory.high. 0 on success. */
static int enter_work(long high_mib) {
    char p[4200], q[4200], buf[64];
    find_cgroup();
    if (strstr(cgdir, "/cb-work"))
        return 0;
    /* A previous run moved this container's processes into cb-init. */
    size_t n = strlen(cgdir);
    if (n >= 8 && strcmp(cgdir + n - 8, "/cb-init") == 0)
        cgdir[n - 8] = 0;
    snprintf(p, sizeof p, "%s/cb-init", cgdir);
    if (mkdir(p, 0755) && errno != EEXIST)
        return -1;
    snprintf(p, sizeof p, "%s/cgroup.procs", cgdir);
    snprintf(q, sizeof q, "%s/cb-init/cgroup.procs", cgdir);
    for (int round = 0; round < 5; round++) {
        FILE *f = fopen(p, "r");
        if (!f)
            return -1;
        int any = 0;
        long pid;
        while (fscanf(f, "%ld", &pid) == 1) {
            snprintf(buf, sizeof buf, "%ld", pid);
            if (write_file(q, buf) == 0)
                any = 1;
        }
        fclose(f);
        if (!any)
            break;
    }
    snprintf(p, sizeof p, "%s/cgroup.subtree_control", cgdir);
    if (write_file(p, "+memory"))
        return -1;
    snprintf(p, sizeof p, "%s/cb-work", cgdir);
    if (mkdir(p, 0755) && errno != EEXIST)
        return -1;
    snprintf(p, sizeof p, "%s/cb-work/memory.high", cgdir);
    snprintf(buf, sizeof buf, "%ld", high_mib * (long)MIB);
    if (write_file(p, buf))
        return -1;
    snprintf(p, sizeof p, "%s/cb-work/cgroup.procs", cgdir);
    snprintf(buf, sizeof buf, "%ld", (long)getpid());
    return write_file(p, buf);
}

static const size_t class_size[] = { 1 << 20, 64 << 10, 16 << 10, 4 << 10, 1 << 10, 256, 64 };
static const int class_share[] = { 15, 20, 20, 20, 15, 5, 5 };

static void grow_to(size_t target) {
    while (live < target && nobjs < MAX_OBJECTS) {
        int r = (int)(next() % 100), c = 0;
        for (int acc = 0; c < 6 && r >= acc + class_share[c]; c++)
            acc += class_share[c];
        size_t sz = class_size[c];
        size_t batch = sz >= (64 << 10) ? 1 : (64 << 10) / sz;
        for (size_t i = 0; i < batch && nobjs < MAX_OBJECTS; i++) {
            char *o = malloc(sz);
            if (!o) {
                fprintf(stderr, "psi_return: malloc(%zu) failed\n", sz);
                exit(1);
            }
            memset(o, 1, sz);
            objs[nobjs] = o;
            sizes[nobjs++] = (uint32_t)sz;
            live += sz;
        }
    }
}

static void shrink_to(size_t target) {
    while (live > target && nobjs) {
        size_t i = next() % nobjs;
        free(objs[i]);
        live -= sizes[i];
        objs[i] = objs[--nobjs];
        sizes[i] = sizes[nobjs];
    }
}

static long rss_kb(const char *key) {
    char line[256];
    long v = -1;
    FILE *f = fopen("/proc/self/status", "r");
    if (!f)
        return -1;
    while (fgets(line, sizeof line, f))
        if (strncmp(line, key, strlen(key)) == 0)
            v = atol(line + strlen(key));
    fclose(f);
    return v;
}

/* The child: on each byte from `go`, touch `bytes` of fresh memory and
 * write the milliseconds it took to `back`. */
static void child_loop(int go, int back, size_t bytes) {
    char c;
    long page = sysconf(_SC_PAGESIZE);
    while (read(go, &c, 1) == 1) {
        double t = now_ms();
        char *m = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (m == MAP_FAILED)
            _exit(1);
        for (size_t o = 0; o < bytes; o += (size_t)page)
            m[o] = 1;
        double ms = now_ms() - t;
        munmap(m, bytes);
        if (write(back, &ms, sizeof ms) != sizeof ms)
            _exit(1);
    }
    _exit(0);
}

static int cmp(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

static void idle_ms(double ms) {
    struct timespec t = { (time_t)(ms / 1e3), (long)((ms - (time_t)(ms / 1e3) * 1e3) * 1e6) };
    nanosleep(&t, NULL);
}

int main(int argc, char **argv) {
    int cycles = argc > 1 ? atoi(argv[1]) : 30;
    long high = argc > 2 ? atol(argv[2]) : 448, low = argc > 3 ? atol(argv[3]) : 192;
    long cmib = argc > 4 ? atol(argv[4]) : 384, cg_high = argc > 5 ? atol(argv[5]) : 640;
    if (cycles < 1 || cycles > MAX_CYCLES || low >= high) {
        fprintf(stderr, "usage: psi_return [cycles] [high_mib] [low_mib] [child_mib] [cg_high_mib]\n");
        return 2;
    }
    const char *where = getenv("CB_PSI_WORK");
    if (!where) {
        setenv("CB_PSI_WORK", enter_work(cg_high) == 0 ? "work" : "own", 1);
        if (strcmp(getenv("CB_PSI_WORK"), "own") == 0)
            fprintf(stderr, "psi_return: no child cgroup (%s); running in %s\n", strerror(errno), cgdir);
        execv("/proc/self/exe", argv);
        perror("psi_return: execv");
        return 1;
    }
    find_cgroup();
    printf("psi_return: cgroup %s (%s), memory.high %lld, memory.max %lld; parent %ld..%ld MiB, child %ld MiB, %d cycles\n",
           cgdir, where, cg("memory.high", NULL), cg("memory.max", NULL), low, high, cmib, cycles);
    int go[2], back[2];
    if (pipe(go) || pipe(back))
        return 1;
    size_t cbytes = (size_t)cmib * MIB;
    pid_t pid = fork();
    if (pid == 0) {
        close(go[1]);
        close(back[0]);
        child_loop(go[0], back[1], cbytes);
    }
    close(go[0]);
    close(back[1]);
    double base, ms[MAX_CYCLES];
    long long high0 = cg("memory.events", "high"), cg_peak = 0;
    /* The touch with nothing allocated: the floor of return_ms. */
    if (write(go[1], "g", 1) != 1 || read(back[0], &base, sizeof base) != sizeof base)
        return 1;
    for (int i = 0; i < cycles; i++) {
        grow_to((size_t)high * MIB);
        long long c = cg("memory.current", NULL);
        if (c > cg_peak)
            cg_peak = c;
        shrink_to((size_t)low * MIB);
        idle_ms(20);
        if (write(go[1], "g", 1) != 1 || read(back[0], &ms[i], sizeof ms[i]) != sizeof ms[i])
            return 1;
        idle_ms(300);
    }
    close(go[1]);
    waitpid(pid, NULL, 0);
    double sum = 0;
    for (int i = 0; i < cycles; i++)
        sum += ms[i];
    qsort(ms, (size_t)cycles, sizeof ms[0], cmp);
    int p99 = (int)(0.99 * (cycles - 1) + 0.5);
    size_t pe = 0, sz = sizeof pe;
    int (*mallctl)(const char *, void *, size_t *, void *, size_t) = (int (*)(const char *, void *, size_t *, void *, size_t))dlsym(RTLD_DEFAULT, "mallctl");
    if (mallctl && mallctl("stats.cgroup.pressure_events", &pe, &sz, NULL, 0))
        pe = 0;
    void (*stats_print)(void (*)(void *, const char *), void *, const char *) =
        (void (*)(void (*)(void *, const char *), void *, const char *))dlsym(RTLD_DEFAULT, "malloc_stats_print");
    if (stats_print && mallctl)
        stats_print(NULL, NULL, "gmdablxe");
    fflush(stdout);
    printf("result return_ms_p50=%.2f return_ms_p99=%.2f return_ms_mean=%.2f return_ms_max=%.2f base_ms=%.2f cg_peak_kb=%lld rss_peak_kb=%ld psi_events=%zu high_events=%lld cgroup=%s\n",
           ms[cycles / 2], ms[p99], sum / cycles, ms[cycles - 1], base, cg_peak / 1024, rss_kb("VmHWM:"), pe,
           cg("memory.events", "high") - high0, where);
    return 0;
}
