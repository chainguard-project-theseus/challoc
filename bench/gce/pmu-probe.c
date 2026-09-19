// pmu-probe.c: does perf_event_open count on this host? For hosts without
// a perf binary (Chainguard OS has none, and Wolfi packages none), the
// question `challoc-bench pmu-check` answers with `perf stat`: open the
// hardware cycles and instructions counters for this process, spin, read
// them back. Prints `pmu cycles=<n> instructions=<n>` or the errno per
// counter (ENOENT: no PMU exposed, EACCES: perf_event_paranoid). Built in
// the dev container (`cc -O2 bench/gce/pmu-probe.c -o pmu-probe`), runs
// natively (same glibc).
#include <errno.h>
#include <linux/perf_event.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

static int open_counter(unsigned config) {
    struct perf_event_attr a;
    memset(&a, 0, sizeof a);
    a.type = PERF_TYPE_HARDWARE;
    a.size = sizeof a;
    a.config = config;
    a.disabled = 1;
    a.exclude_kernel = 1;
    a.exclude_hv = 1;
    return (int)syscall(__NR_perf_event_open, &a, 0, -1, -1, 0);
}

int main(void) {
    int fc = open_counter(PERF_COUNT_HW_CPU_CYCLES);
    int ec = fc < 0 ? errno : 0;
    int fi = open_counter(PERF_COUNT_HW_INSTRUCTIONS);
    int ei = fi < 0 ? errno : 0;
    volatile unsigned long x = 1;
    if (fc >= 0) ioctl(fc, PERF_EVENT_IOC_ENABLE, 0);
    if (fi >= 0) ioctl(fi, PERF_EVENT_IOC_ENABLE, 0);
    for (unsigned long i = 0; i < 100000000UL; i++) x = x * 3 + i;
    long long cycles = -1, instructions = -1;
    if (fc >= 0) { ioctl(fc, PERF_EVENT_IOC_DISABLE, 0); if (read(fc, &cycles, sizeof cycles) != sizeof cycles) cycles = -1; }
    if (fi >= 0) { ioctl(fi, PERF_EVENT_IOC_DISABLE, 0); if (read(fi, &instructions, sizeof instructions) != sizeof instructions) instructions = -1; }
    printf("pmu cycles=%lld instructions=%lld", cycles, instructions);
    if (ec) printf(" cycles_open_errno=%d(%s)", ec, strerror(ec));
    if (ei) printf(" instructions_open_errno=%d(%s)", ei, strerror(ei));
    printf(" paranoid=");
    FILE *f = fopen("/proc/sys/kernel/perf_event_paranoid", "r");
    int p = -99; if (f) { if (fscanf(f, "%d", &p) != 1) p = -99; fclose(f); }
    printf("%d x=%lu\n", p, (unsigned long)x);
    return (fc >= 0 && cycles > 0) ? 0 : 1;
}
