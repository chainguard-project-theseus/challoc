/*
 * mseal of the reservations' never-committed ranges (plan Phase 2 task 2,
 * design 10 step 5 and 14).
 *
 * After init, on a kernel with mseal (6.10+, not filtered by seccomp), the
 * gaps between the class regions and the alignment slack around the huge
 * and metadata regions are sealed: munmap, mprotect and MAP_FIXED over them
 * fail with EPERM for the rest of the process. The regions themselves are
 * not sealed (they are committed and purged for the life of the process),
 * so the allocator must keep working afterwards: every tier allocates and
 * frees, arenas.create builds an arena, a thread exits, a fork child
 * allocates.
 *
 * Sealed VMAs are found through /proc/self/smaps (`sl` in VmFlags), so the
 * program does not depend on the allocator's internals. When the program's
 * own mseal probe fails the allocator cannot have sealed anything either;
 * the program says so and passes, and asserts that nothing is sealed.
 *
 * The VMA count is read before and after the first malloc with raw
 * syscalls (a FILE* would allocate and initialise the allocator early).
 */
#include "common.h"

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>

/* 462 on both x86_64 and aarch64 (asm-generic); glibc's headers may predate it. */
#ifndef SYS_mseal
#define SYS_mseal 462
#endif

typedef int (*mallctl_fn)(const char *name, void *oldp, size_t *oldlenp, void *newp, size_t newlen);
typedef void (*stats_print_fn)(void (*cb)(void *, const char *), void *cbopaque, const char *opts);

static int stats_sealed, stats_total;

/* malloc_stats_print callback: pick out "Sealed ranges: N of M". */
static void stats_cb(void *opaque, const char *s)
{
    (void)opaque;
    const char *at = strstr(s, "Sealed ranges: ");
    if (at)
        sscanf(at, "Sealed ranges: %d of %d", &stats_sealed, &stats_total);
}

/* Lines in /proc/self/maps without touching malloc. */
static int count_maps_lines(void)
{
    static char buf[1 << 20];
    int fd = open("/proc/self/maps", O_RDONLY);
    REQUIRE(fd >= 0, "open /proc/self/maps");
    size_t n = 0;
    for (;;) {
        ssize_t r = read(fd, buf + n, sizeof buf - n);
        REQUIRE(r >= 0, "read /proc/self/maps");
        if (r == 0)
            break;
        n += (size_t)r;
        REQUIRE(n < sizeof buf, "/proc/self/maps larger than %zu bytes", sizeof buf);
    }
    close(fd);
    int lines = 0;
    for (size_t i = 0; i < n; i++)
        lines += buf[i] == '\n';
    return lines;
}

static int mseal_available(void)
{
    long page = sysconf(_SC_PAGESIZE);
    void *p = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(p != MAP_FAILED, "mmap scratch");
    long r = syscall(SYS_mseal, p, (size_t)page, 0UL);
    if (r != 0) {
        munmap(p, page);
        return 0;
    }
    return 1; /* sealed: stays mapped, one PROT_NONE page */
}

struct range {
    uintptr_t lo, hi;
    int named; /* [anon:challoc:...] */
};

/* Collect the sealed VMAs (VmFlags contains `sl`) from /proc/self/smaps. */
static int sealed_ranges(struct range *out, int max)
{
    FILE *f = fopen("/proc/self/smaps", "r");
    REQUIRE(f != NULL, "fopen /proc/self/smaps");
    char line[512];
    struct range cur = {0, 0, 0};
    int have = 0, n = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        char perms[8];
        if (sscanf(line, "%lx-%lx %7s", &lo, &hi, perms) == 3 && strchr(line, ' ') && strlen(perms) == 4) {
            cur.lo = lo;
            cur.hi = hi;
            cur.named = strstr(line, "[anon:challoc:") != NULL;
            have = 1;
            continue;
        }
        if (have && strncmp(line, "VmFlags:", 8) == 0) {
            char *s = line + 8;
            int sealed = 0;
            for (char *tok = strtok(s, " \n"); tok; tok = strtok(NULL, " \n"))
                if (strcmp(tok, "sl") == 0)
                    sealed = 1;
            if (sealed && n < max)
                out[n++] = cur;
            have = 0;
        }
    }
    fclose(f);
    return n;
}

static void *thread_main(void *arg)
{
    (void)arg;
    void *p = malloc(300);
    REQUIRE(p != NULL, "malloc in thread");
    fill(p, 300, 9);
    free(p);
    return NULL;
}

static void allocator_still_works(void)
{
    size_t sizes[] = {1, QUANTUM, 100, 1024, 4096, 100 * 1024, 300 * 1024, 5 << 20};
    void *held[sizeof sizes / sizeof sizes[0]];
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        held[i] = malloc(sizes[i]);
        REQUIRE(held[i] != NULL, "malloc(%zu)", sizes[i]);
        fill(held[i], sizes[i], (uint32_t)i);
    }
    for (size_t i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        CHECK(check_fill(held[i], sizes[i], (uint32_t)i), "block %zu corrupted", i);
        free(held[i]);
    }

    mallctl_fn mallctl = (mallctl_fn)dlsym(RTLD_DEFAULT, "mallctl");
    REQUIRE(mallctl != NULL, "dlsym(mallctl)");
    unsigned arena = 0;
    size_t len = sizeof arena;
    CHECK(mallctl("arenas.create", &arena, &len, NULL, 0) == 0, "arenas.create");
    CHECK(arena >= 1, "arena index %u", arena);

    pthread_t t;
    REQUIRE(pthread_create(&t, NULL, thread_main, NULL) == 0, "pthread_create");
    REQUIRE(pthread_join(t, NULL) == 0, "pthread_join");

    pid_t pid = fork();
    REQUIRE(pid >= 0, "fork");
    if (pid == 0) {
        void *p = malloc(2000);
        if (!p)
            _exit(2);
        fill(p, 2000, 3);
        void *q = malloc(1 << 20);
        if (!q)
            _exit(3);
        free(q);
        free(p);
        _exit(0);
    }
    int st = 0;
    REQUIRE(waitpid(pid, &st, 0) == pid, "waitpid");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "fork child status %#x", st);
}

int main(void)
{
    /* ld.so or libc may already have called malloc before main; if so the
     * allocator is initialised and `before` already includes its VMAs, and
     * the delta printed below is small. The bound check holds either way. */
    int before = count_maps_lines();
    require_challoc(); /* first malloc from main: init, reservations, sealing */
    int after = count_maps_lines();
    int available = mseal_available();

    static struct range sealed[512];
    int n = sealed_ranges(sealed, 512);
    long page = sysconf(_SC_PAGESIZE);

    if (!available) {
        /* The allocator's probe failed the same way: nothing is sealed. */
        CHECK(n == 0, "%d sealed VMAs although mseal is unavailable", n);
        stats_sealed = stats_total = -1;
        stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
        REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
        msp(stats_cb, NULL, "");
        CHECK(stats_sealed == 0 && stats_total > 0, "allocator reports %d of %d sealed", stats_sealed, stats_total);
        printf("mseal: unavailable (kernel < 6.10 or seccomp), allocator sealed nothing; maps lines %d -> %d\n",
               before, after);
        allocator_still_works();
        return finish("mseal");
    }

    /* What the allocator says it sealed: "Sealed ranges: N of M". */
    stats_sealed = stats_total = -1;
    stats_print_fn msp = (stats_print_fn)dlsym(RTLD_DEFAULT, "malloc_stats_print");
    REQUIRE(msp != NULL, "dlsym(malloc_stats_print)");
    msp(stats_cb, NULL, "");
    CHECK(stats_sealed >= 0 && stats_total >= 0, "no `Sealed ranges: N of M` line in malloc_stats_print");
    CHECK(stats_sealed == stats_total, "allocator sealed %d of %d ranges", stats_sealed, stats_total);
    /* NCLASSES + 1 class gaps (the leading one may be empty) plus up to
     * four slack pieces around the huge and metadata regions. */
    int nclasses = QUANTUM == 16 ? 52 : 56;
    CHECK(stats_total >= nclasses && stats_total <= nclasses + 5, "%d never-committed ranges, expected %d..%d",
          stats_total, nclasses, nclasses + 5);

    /* Sealed VMAs from smaps. The gap after every class is at least one
     * maximal slab (4 MiB) long, so at least NCLASSES sealed VMAs are that
     * big; the slack pieces may be smaller, and adjacent reservations can
     * merge two sealed ranges into one VMA (at most twice: three
     * mappings). The two probe pages (ours and challoc's) are one or two
     * pages and may merge with each other. */
    int named = 0, big = 0;
    struct range *biggest = NULL;
    for (int i = 0; i < n; i++) {
        if (sealed[i].named)
            named++;
        if (sealed[i].hi - sealed[i].lo >= (uintptr_t)4 << 20)
            big++;
        if (!biggest || sealed[i].hi - sealed[i].lo > biggest->hi - biggest->lo)
            biggest = &sealed[i];
    }
    printf("mseal: available; allocator sealed %d of %d ranges; %d sealed VMAs (%d named challoc, %d of 4 MiB or "
           "more); maps lines %d -> %d (+%d) across init\n",
           stats_sealed, stats_total, n, named, big, before, after, after - before);
    CHECK(big >= nclasses - 2 && big <= stats_total, "%d sealed VMAs of 4 MiB or more, expected %d..%d", big,
          nclasses - 2, stats_total);
    CHECK(n <= stats_total + 2, "%d sealed VMAs for %d ranges and two probe pages", n, stats_total);
    /* Bound on init's VMA growth: reservations (3), probe pages (2), the
     * metadata and slab commits of the first malloc, and two VMAs per
     * sealed range. */
    CHECK(after - before <= 2 * stats_total + 32, "init grew the VMA count by %d", after - before);
    REQUIRE(biggest != NULL, "no sealed VMA");

    /* The plan's criterion: munmap of a reserved range fails with EPERM. */
    void *lo = (void *)biggest->lo;
    size_t len = biggest->hi - biggest->lo;
    errno = 0;
    CHECK(munmap(lo, len) == -1 && errno == EPERM, "munmap of a sealed gap: rc/errno %d", errno);
    errno = 0;
    CHECK(munmap(lo, page) == -1 && errno == EPERM, "munmap of one sealed page: errno %d", errno);
    errno = 0;
    CHECK(mprotect(lo, page, PROT_READ | PROT_WRITE) == -1 && errno == EPERM, "mprotect of a sealed gap: errno %d",
          errno);
    errno = 0;
    void *fixed = mmap(lo, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    CHECK(fixed == MAP_FAILED && errno == EPERM, "MAP_FIXED over a sealed gap: %p errno %d", fixed, errno);
    if (fixed != MAP_FAILED)
        munmap(fixed, page);

    /* Sealing does not stop the allocator from committing, purging,
     * building arenas, ending threads or forking. */
    allocator_still_works();
    return finish("mseal");
}
