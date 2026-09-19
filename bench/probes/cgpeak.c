/*
 * cgpeak: the cgroup's memory.peak per harness phase (plan Phase 2.5 task
 * 17, docs/research.md kernel table, 6.12 "memory.peak write resets").
 *
 * From 6.12 a write to memory.peak resets the high-water mark, but the reset
 * is scoped to the open file description that wrote it: a later read through
 * the same descriptor shows the peak since the reset, any other descriptor
 * still shows the peak since the cgroup's birth. A shell's `echo reset >
 * memory.peak` therefore resets nothing a `cat` can see. This sidecar holds
 * one descriptor for the whole run and is driven by the phase file the soak
 * harness already writes:
 *
 *   cgpeak DIR      DIR/phase names the current phase (the harness rewrites
 *                   it at each boundary), DIR/stop ends the run.
 *
 * On start it finds this process's cgroup2 directory (the walk of
 * scripts/cgroup-tests.sh and src/sys/cgroup.rs: /proc/self/cgroup's `0::`
 * line under the cgroup2 mount of /proc/self/mountinfo), opens memory.peak
 * read-write and writes a reset. DIR/cgpeak.state then reads `yes <path>` or
 * `no <reason>` (the tree is read-only in a stock Docker container; a kernel
 * before 6.12 refuses the write with EINVAL; nsdelegate refuses it from
 * inside a cgroup namespace) and, when `no`, the sidecar exits and the
 * harness keeps VmHWM alone. When `yes` it polls DIR/phase every 100 ms: at
 * each change it records the ending phase's peak (bytes, the value read
 * through its descriptor) as DIR/cgpeak.<phase>, resets, and acknowledges
 * with DIR/cgpeak.phase = the new name, so the harness can wait for the
 * reset before it starts the phase's load; DIR/cgpeak.<phase>.file is the
 * cgroup's page cache (memory.stat `file`, bytes) at that moment, since the
 * peak counts it too. DIR/stop records the last phase and exits. Nothing here touches the allocator under test: the peak is the
 * whole container's (server, load generators and the harness), which is the
 * "as the cgroup sees it" figure the plan asks for, next to the server's
 * VmHWM.
 *
 * Built on demand by bench/lib.sh (ensure_probe cgpeak) like the other
 * probes; the Makefile's CGROUP_PEAK=1 gives the container a writable
 * cgroup tree.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static char dir[4096];

static void put_file(const char *name, const char *text)
{
    char path[4400], tmp[4500];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    FILE *f = fopen(tmp, "w");
    if (!f)
        return;
    fputs(text, f);
    fputc('\n', f);
    fclose(f);
    rename(tmp, path);
}

/* The first line of DIR/NAME without its newline, "" when absent. */
static void get_file(const char *name, char *out, size_t n)
{
    char path[4400];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    out[0] = 0;
    FILE *f = fopen(path, "r");
    if (!f)
        return;
    if (fgets(out, (int)n, f))
        out[strcspn(out, "\r\n")] = 0;
    fclose(f);
}

static int exists(const char *name)
{
    char path[4400];
    struct stat st;
    snprintf(path, sizeof path, "%s/%s", dir, name);
    return stat(path, &st) == 0;
}

/* /sys/fs/cgroup<path>: the cgroup2 mount point and this process's unified path. */
static int cgroup_dir(char *out, size_t n)
{
    char line[4096], mount[4096] = "", path[4096] = "";
    FILE *f = fopen("/proc/self/mountinfo", "r");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f)) {
        char *sep = strstr(line, " - cgroup2 ");
        if (!sep)
            continue;
        /* field 5 is the mount point */
        char *save = NULL, *tok = strtok_r(line, " ", &save);
        for (int i = 1; tok && i < 5; i++)
            tok = strtok_r(NULL, " ", &save);
        if (tok) {
            snprintf(mount, sizeof mount, "%s", tok);
            break;
        }
    }
    fclose(f);
    if (!mount[0])
        return 0;
    f = fopen("/proc/self/cgroup", "r");
    if (!f)
        return 0;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, "0::", 3) == 0) {
            snprintf(path, sizeof path, "%s", line + 3);
            path[strcspn(path, "\r\n")] = 0;
            break;
        }
    }
    fclose(f);
    if (strcmp(path, "/") == 0)
        path[0] = 0;
    snprintf(out, n, "%s%s", mount, path);
    return 1;
}

static void no(const char *fmt, const char *arg)
{
    char msg[9000];
    snprintf(msg, sizeof msg, fmt, arg);
    char state[9100];
    snprintf(state, sizeof state, "no %s", msg);
    put_file("cgpeak.state", state);
}

/* memory.stat's `file` line (the cgroup's page cache, bytes): the peak
 * counts it too, so a build or a large log in the container inflates the
 * column; the harness prints it beside the peak. -1 when unreadable. */
static long long read_file_cache(const char *cg)
{
    char path[8400], line[256];
    snprintf(path, sizeof path, "%s/memory.stat", cg);
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    long long v = -1;
    while (fgets(line, sizeof line, f))
        if (strncmp(line, "file ", 5) == 0) {
            v = atoll(line + 5);
            break;
        }
    fclose(f);
    return v;
}

static long long read_peak(int fd)
{
    char buf[64];
    ssize_t n = pread(fd, buf, sizeof buf - 1, 0);
    if (n <= 0)
        return -1;
    buf[n] = 0;
    return atoll(buf);
}

static int reset(int fd)
{
    return pwrite(fd, "reset", 5, 0) == 5;
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: cgpeak DIR\n");
        return 2;
    }
    snprintf(dir, sizeof dir, "%s", argv[1]);

    char cg[8192], peak[8320];
    if (!cgroup_dir(cg, sizeof cg)) {
        no("no cgroup2 mount in /proc/self/mountinfo%s", "");
        return 0;
    }
    snprintf(peak, sizeof peak, "%s/memory.peak", cg);
    int fd = open(peak, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        if (e == EROFS)
            no("%s: the container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make for --cgroupns=host and a read-write /sys/fs/cgroup", peak);
        else if (e == ENOENT)
            no("%s does not exist (no memory controller here)", peak);
        else {
            char why[8500];
            snprintf(why, sizeof why, "open %s for writing: %s", peak, strerror(e));
            no("%s", why);
        }
        return 0;
    }
    if (!reset(fd)) {
        int e = errno;
        char why[8500];
        if (e == EINVAL) {
            char release[65] = "";
            FILE *f = fopen("/proc/sys/kernel/osrelease", "r");
            if (f) {
                if (fgets(release, sizeof release, f))
                    release[strcspn(release, "\r\n")] = 0;
                fclose(f);
            }
            snprintf(why, sizeof why, "the kernel %s does not accept a write to %s (the reset is 6.12+)", release, peak);
        } else if (e == EPERM || e == EACCES) {
            snprintf(why, sizeof why, "the write to %s is refused from inside the cgroup namespace (nsdelegate): pass CGROUP_PEAK=1 to make", peak);
        } else {
            snprintf(why, sizeof why, "write to %s: %s", peak, strerror(e));
        }
        no("%s", why);
        return 0;
    }
    char state[9100];
    snprintf(state, sizeof state, "yes %s", peak);
    put_file("cgpeak.state", state);

    char cur[256] = "", next[256];
    get_file("phase", cur, sizeof cur);
    if (!cur[0])
        strcpy(cur, "start");
    put_file("cgpeak.phase", cur);
    struct timespec ts = { 0, 100 * 1000 * 1000 };
    for (;;) {
        int stop = exists("stop");
        get_file("phase", next, sizeof next);
        if (stop || (next[0] && strcmp(next, cur) != 0)) {
            long long v = read_peak(fd);
            char name[300], val[32];
            snprintf(name, sizeof name, "cgpeak.%s", cur);
            snprintf(val, sizeof val, "%lld", v);
            put_file(name, val);
            snprintf(name, sizeof name, "cgpeak.%s.file", cur);
            snprintf(val, sizeof val, "%lld", read_file_cache(cg));
            put_file(name, val);
            if (stop)
                break;
            if (!reset(fd)) {
                snprintf(state, sizeof state, "no reset of %s failed mid-run: %s", peak, strerror(errno));
                put_file("cgpeak.state", state);
                break;
            }
            strcpy(cur, next);
            put_file("cgpeak.phase", cur);
        }
        nanosleep(&ts, NULL);
    }
    close(fd);
    return 0;
}
