/* sampler: a SIGPROF PC sampler loaded with LD_PRELOAD next to the allocator
 * under test. Wolfi does not package `perf`, so this stands in for
 * `perf record` in the Phase 0 acceptance check (docs/plan.md). It arms a
 * process-wide ITIMER_PROF at SAMPLER_HZ (default 1000) samples per CPU
 * second; the handler records the interrupted PC. At exit the samples are
 * sorted and written to $SAMPLER_OUT as lines
 *   count module offset
 * where `module` is the basename of the loaded object and `offset` is the
 * PC minus its load bias, so it matches `nm` and `objdump -d` addresses.
 * Map the offsets to symbols with `nm` and to instructions with `objdump -d`.
 *
 * The handler only stores a word; nothing here allocates after startup. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <link.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <ucontext.h>
#include <unistd.h>

#define MAX_SAMPLES (1u << 22)
static uintptr_t samples[MAX_SAMPLES];
static size_t nsamples;

static void on_prof(int sig, siginfo_t *si, void *ctx) {
    (void)sig;
    (void)si;
    ucontext_t *uc = ctx;
#if defined(__aarch64__)
    uintptr_t pc = uc->uc_mcontext.pc;
#elif defined(__x86_64__)
    uintptr_t pc = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
#else
#error unsupported architecture
#endif
    size_t i = __atomic_fetch_add(&nsamples, 1, __ATOMIC_RELAXED);
    if (i < MAX_SAMPLES) samples[i] = pc;
}

__attribute__((constructor)) static void sampler_start(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_prof;
    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    sigaction(SIGPROF, &sa, NULL);
    const char *hz_s = getenv("SAMPLER_HZ");
    long hz = hz_s ? atol(hz_s) : 1000;
    if (hz <= 0) hz = 1000;
    struct itimerval it;
    it.it_interval.tv_sec = 0;
    it.it_interval.tv_usec = 1000000 / hz;
    it.it_value = it.it_interval;
    setitimer(ITIMER_PROF, &it, NULL);
}

struct module {
    const char *name;
    uintptr_t bias, lo, hi;
};
static struct module mods[256];
static int nmods;

static int collect(struct dl_phdr_info *info, size_t sz, void *data) {
    (void)sz;
    (void)data;
    if (nmods == 256) return 0;
    uintptr_t lo = UINTPTR_MAX, hi = 0;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        if (ph->p_type != PT_LOAD) continue;
        uintptr_t s = info->dlpi_addr + ph->p_vaddr, e = s + ph->p_memsz;
        if (s < lo) lo = s;
        if (e > hi) hi = e;
    }
    if (lo >= hi) return 0;
    const char *name = info->dlpi_name && *info->dlpi_name ? info->dlpi_name : "main";
    const char *slash = strrchr(name, '/');
    mods[nmods].name = slash ? slash + 1 : name;
    mods[nmods].bias = info->dlpi_addr;
    mods[nmods].lo = lo;
    mods[nmods].hi = hi;
    nmods++;
    return 0;
}

static int cmp_uptr(const void *a, const void *b) {
    uintptr_t x = *(const uintptr_t *)a, y = *(const uintptr_t *)b;
    return x < y ? -1 : x > y;
}

__attribute__((destructor)) static void sampler_stop(void) {
    struct itimerval off;
    memset(&off, 0, sizeof off);
    setitimer(ITIMER_PROF, &off, NULL);
    signal(SIGPROF, SIG_IGN);
    const char *out = getenv("SAMPLER_OUT");
    if (!out) return;
    int fd = open(out, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) return;
    dl_iterate_phdr(collect, NULL);
    size_t n = nsamples < MAX_SAMPLES ? nsamples : MAX_SAMPLES;
    qsort(samples, n, sizeof samples[0], cmp_uptr);
    static char buf[1 << 16];
    size_t used = 0;
    for (size_t i = 0; i < n;) {
        size_t j = i;
        while (j < n && samples[j] == samples[i]) j++;
        const char *name = "?";
        uintptr_t off = samples[i];
        for (int m = 0; m < nmods; m++) {
            if (samples[i] >= mods[m].lo && samples[i] < mods[m].hi) {
                name = mods[m].name;
                off = samples[i] - mods[m].bias;
                break;
            }
        }
        int k = snprintf(buf + used, sizeof buf - used, "%zu %s %lx\n", j - i,
                         name, (unsigned long)off);
        if (k < 0) break;
        used += (size_t)k;
        if (used > sizeof buf - 128) {
            if (write(fd, buf, used) < 0) break;
            used = 0;
        }
        i = j;
    }
    if (used) (void)!write(fd, buf, used);
    if (nsamples > MAX_SAMPLES) {
        int k = snprintf(buf, sizeof buf, "# dropped %zu samples\n", nsamples - MAX_SAMPLES);
        (void)!write(fd, buf, (size_t)k);
    }
    close(fd);
}
