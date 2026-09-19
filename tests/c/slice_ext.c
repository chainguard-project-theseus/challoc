/* rseq time slice extension (docs/design.md section 17, plan Phase 2.5
 * task 15), both ways. Run as LD_PRELOAD=target/release/libchalloc.so
 * ./slice_ext MODE with MALLOC_CONF=slice_ext:MODE (on or off; the driver
 * in src/api/ctests.rs sets it). The program reads the kernel's own view
 * through prctl(PR_RSEQ_SLICE_EXTENSION, GET), which says whether the
 * calling thread is enabled (1), not (0), or the kernel has no such
 * option (ENOTSUPP/EINVAL), and asserts what the allocator must have done
 * on this host:
 *
 * - under `off`: no thread is ever enabled, whatever the kernel;
 * - under `on` where the kernel has the option (bit 4 of this thread's
 *   rseq `flags`): the init thread is enabled after the first malloc, a
 *   new thread is not until its first lock hold (its first allocation:
 *   heap creation takes locks; the enable is per task, `rseq_fork` resets
 *   a CLONE_VM child's rseq state), a fork child inherits the forking
 *   thread's enable, and the report line says `Slice extension: on`;
 * - under `on` where the kernel lacks it (Ubuntu's 7.0 GCE kernels, the
 *   Mac's 6.10, every arm64 kernel to date): nothing is enabled, the two
 *   slice_ctrl bytes of this thread stay zero across a burst of
 *   allocation, the report says `off (kernel-disabled)` or
 *   `off (no-slice_ctrl)` and notes the refused request.
 *
 * Either way 32 threads allocate and free at random with a per-thread tag
 * (the same workload correctness as the other suites), so the request and
 * yield path runs under real contention where it is on, and
 * `opt.slice_ext` reads back the mode. */
#include "common.h"
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/prctl.h>
#include <sys/wait.h>

#define PR_RSEQ_SLICE_EXTENSION 79
#define PR_RSEQ_SLICE_EXTENSION_GET 1
#define RSEQ_FLAG_SLICE_EXT_AVAILABLE (1u << 4)
#define RSEQ_FLAG_SLICE_EXT_ENABLED (1u << 5)
#define NTHREADS 32
#define OPS 40000
#define LIVE 128

extern __attribute__((weak)) const ptrdiff_t __rseq_offset;
extern __attribute__((weak)) const unsigned int __rseq_size;

static int (*ctl)(const char *, void *, size_t *, void *, size_t);
static void (*stats_print)(void (*)(void *, const char *), void *, const char *);

static uintptr_t thread_pointer(void)
{
    uintptr_t tp;
#if defined(__x86_64__)
    __asm__("mov %%fs:0, %0" : "=r"(tp));
#elif defined(__aarch64__)
    __asm__("mrs %0, tpidr_el0" : "=r"(tp));
#else
#error "unsupported architecture"
#endif
    return tp;
}

static volatile unsigned char *area(void)
{
    return (volatile unsigned char *)(thread_pointer() + __rseq_offset);
}

static unsigned area_flags(void)
{
    return *(volatile unsigned *)(area() + 16);
}

/* 1 enabled, 0 not, -1 the kernel has no such option. */
static int kernel_enabled(void)
{
    long r = prctl(PR_RSEQ_SLICE_EXTENSION, PR_RSEQ_SLICE_EXTENSION_GET, 0, 0, 0);
    return r < 0 ? -1 : (int)r;
}

static const size_t sizes[] = { 8, 16, 32, 48, 64, 96, 128, 256, 512, 1024, 2048, 4096, 16384 };
#define NSIZES (sizeof sizes / sizeof sizes[0])

struct job {
    int index;
    int available;
    int enabled_before;
    int enabled_after;
};

static void churn(int index, int ops)
{
    uint64_t tag = ((uint64_t)index + 1) * 0x0101010101010101ULL;
    uint64_t rng = 0x9E3779B97F4A7C15ULL ^ (uint64_t)(index + 1);
    uint64_t *live[LIVE];
    size_t nlive = 0;
    for (int i = 0; i < ops; i++) {
        uint64_t r = rng_next(&rng);
        if (nlive < LIVE && ((r & 1) == 0 || nlive == 0)) {
            size_t sz = sizes[(r >> 8) % NSIZES];
            uint64_t *p = malloc(sz);
            REQUIRE(p != NULL, "malloc(%zu)", sz);
            p[0] = tag;
            live[nlive++] = p;
        } else {
            size_t k = (r >> 8) % nlive;
            CHECK(live[k][0] == tag, "thread %d: foreign tag %016llx", index, (unsigned long long)live[k][0]);
            free(live[k]);
            live[k] = live[--nlive];
        }
    }
    while (nlive)
        free(live[--nlive]);
}

static void *worker(void *arg)
{
    struct job *j = arg;
    j->enabled_before = kernel_enabled();
    churn(j->index, OPS);
    j->enabled_after = kernel_enabled();
    return NULL;
}

static char report[65536];
static size_t report_len;
static void capture(void *opaque, const char *s)
{
    (void)opaque;
    size_t n = strlen(s);
    if (report_len + n < sizeof report) {
        memcpy(report + report_len, s, n);
        report_len += n;
    }
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2 && (strcmp(argv[1], "on") == 0 || strcmp(argv[1], "off") == 0), "usage: slice_ext on|off");
    int mode_on = strcmp(argv[1], "on") == 0;
    require_challoc();
    ctl = dlsym(RTLD_DEFAULT, "mallctl");
    stats_print = dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(ctl && stats_print, "mallctl / malloc_stats_print");
    REQUIRE(&__rseq_size != NULL && &__rseq_offset != NULL, "glibc without __rseq_size");

    /* The allocator initialised on the first malloc above; what did it do? */
    unsigned size = __rseq_size;
    int available = size >= 32 && (area_flags() & RSEQ_FLAG_SLICE_EXT_AVAILABLE) != 0;
    int main_enabled = kernel_enabled();
    int flags_enabled = size >= 32 && (area_flags() & RSEQ_FLAG_SLICE_EXT_ENABLED) != 0;
    printf("slice_ext: mode %s, __rseq_size %u, available %d, main thread enabled %d (flags bit 5: %d)\n",
           argv[1], size, available, main_enabled, flags_enabled);

    const char *opt = NULL;
    size_t len = sizeof opt;
    CHECK(ctl("opt.slice_ext", &opt, &len, NULL, 0) == 0 && opt && strcmp(opt, argv[1]) == 0,
          "opt.slice_ext reads %s", opt ? opt : "(null)");

    int expect_on = mode_on && available;
    if (expect_on) {
        CHECK(main_enabled == 1, "the init thread is not enabled (%d)", main_enabled);
        CHECK(flags_enabled, "bit 5 clear on the init thread");
    } else {
        CHECK(main_enabled <= 0, "enabled under %s with available=%d", argv[1], available);
        CHECK(!flags_enabled, "bit 5 set under %s", argv[1]);
    }

    pthread_t t[NTHREADS];
    struct job jobs[NTHREADS];
    for (int i = 0; i < NTHREADS; i++) {
        jobs[i].index = i;
        jobs[i].available = available;
        REQUIRE(pthread_create(&t[i], NULL, worker, &jobs[i]) == 0, "pthread_create");
    }
    int before_enabled = 0, after_enabled = 0;
    for (int i = 0; i < NTHREADS; i++) {
        pthread_join(t[i], NULL);
        before_enabled += jobs[i].enabled_before == 1;
        after_enabled += jobs[i].enabled_after == 1;
    }
    printf("slice_ext: %d threads: %d enabled before their first allocation, %d after\n", NTHREADS, before_enabled, after_enabled);
    /* The enable is per task: a new thread is not enabled by the init
     * thread's prctl; its own first lock hold enables it (where on). */
    CHECK(before_enabled == 0, "%d threads enabled before any hold: the enable is not per task?", before_enabled);
    if (expect_on)
        CHECK(after_enabled == NTHREADS, "only %d of %d threads enabled by their holds", after_enabled, NTHREADS);
    else
        CHECK(after_enabled == 0, "%d threads enabled under %s/available=%d", after_enabled, argv[1], available);

    /* A fork child inherits the forking thread's enable (rseq_fork copies
     * the parent's rseq state to a plain fork child). */
    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork");
    if (pid == 0) {
        int child = kernel_enabled();
        churn(99, 2000);
        _exit(child == (expect_on ? 1 : (available ? 0 : -1)) ? 0 : 3);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "fork child: enabled state not inherited (status %d)", st);

    /* The fallback writes nothing: this thread's slice_ctrl bytes. */
    if (size >= 32) {
        unsigned char req = area()[28], granted = area()[29];
        CHECK(req == 0, "request left set after the churn");
        if (!expect_on)
            CHECK(granted == 0, "granted set without the extension");
    }

    stats_print(capture, NULL, "");
    const char *line = strstr(report, "Slice extension: ");
    REQUIRE(line != NULL, "no Slice extension line in the report");
    const char *eol = strchr(line, '\n');
    printf("%.*s\n", (int)(eol ? eol - line : 60), line);
    if (expect_on) {
        CHECK(strncmp(line, "Slice extension: on (", 21) == 0, "report says off where the probe passed");
    } else {
        CHECK(strncmp(line, "Slice extension: off (", 22) == 0, "report says on where nothing is enabled");
        if (mode_on)
            CHECK(strstr(line, "slice_ext:on requested, unavailable") != NULL, "a refused `on` is not noted");
        else
            CHECK(strstr(line, "slice_ext:off") != NULL, "`off` not named as the reason");
    }
    return finish("slice_ext");
}
