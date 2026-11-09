/*
 * raw_clone: what challoc does on a thread glibc did not create. Premises
 * C9 to C12, C14, C15 and L2 of verus/kani/premises.md put such a thread
 * (a raw `clone`, whose thread pointer names no glibc descriptor of its
 * own) outside the premises, so no theorem covers it and this program asserts
 * nothing about it: it records what happens. Each variant runs in a
 * forked supervisor so that whatever the clone child does (a fault, a
 * hang, a corrupted heap) cannot take the reporting process with it:
 *
 *   shared   clone(CLONE_VM | SIGCHLD) without CLONE_SETTLS: the child
 *            shares the supervisor's memory and thread pointer, so its
 *            TLS (errno, challoc's heap pointer, the rseq area) is the
 *            supervisor's thread's, while the supervisor waits in
 *            waitpid. The child allocates, fills and frees blocks of
 *            every tier and leaves some live; the supervisor then checks
 *            and frees them.
 *   own      clone(CLONE_VM | CLONE_SETTLS | SIGCHLD) with the thread
 *            pointer in the middle of a zeroed 64 KiB area: TLS reads
 *            within 32 KiB of it see zeros, and no glibc descriptor is there for pthread_self, pthread_setspecific or
 *            __errno_location to find.
 *
 * For each variant it prints one line, `raw_clone: <variant>: ...`,
 * naming how the child ended (its exit status or signal, or a timeout
 * after TIMEOUT_S seconds), how far it got, and, for `shared`, whether
 * the supervisor could use the blocks afterwards; then `ok raw_clone`.
 * The program fails only when it cannot run the experiment (mmap, clone,
 * fork or waitpid fails).
 */
#include "common.h"

#include <sched.h>
#include <sys/mman.h>

#define TIMEOUT_S 20
#define KEEP 64
#define ROUNDS 2000

/* The supervisor's exit status when it could not run the experiment
 * (mmap, clone or waitpid failed): the program fails on it. */
#define SETUP_FAILED 99
#define SETUP(cond, ...)                                                   \
    do {                                                                   \
        if (!(cond)) {                                                     \
            fprintf(stderr, "FATAL %s:%d: ", __FILE__, __LINE__);          \
            fprintf(stderr, __VA_ARGS__);                                  \
            fputc('\n', stderr);                                           \
            return SETUP_FAILED;                                           \
        }                                                                  \
    } while (0)

/* Shared between the top-level process, the supervisor and the clone
 * child (MAP_SHARED, created before the fork). */
struct report {
    volatile pid_t child;  /* the clone child's pid, for the timeout kill */
    volatile int stage;    /* 1 started, 2 allocated, 3 returning */
    volatile int allocs;   /* allocations that returned a block */
    volatile int status;   /* the child's wait status */
    volatile int reaped;   /* the supervisor reaped the child */
    volatile int used;     /* the supervisor checked and freed the kept blocks */
    void *volatile keep[KEEP];
    volatile size_t keep_n[KEEP];
};

static struct report *rep;

static size_t round_size(int i)
{
    static const size_t tiers[] = {8, 24, 64, 200, 1000, 4096, 20000, 300000, 2 << 20};
    return tiers[i % (int)(sizeof tiers / sizeof tiers[0])] + (size_t)(i % 7);
}

/* The clone child: no glibc calls but the allocator's entry points and
 * memset, which needs no TLS. Returning ends the task through the
 * clone wrapper's exit system call. */
static int child_fn(void *arg)
{
    (void)arg;
    rep->stage = 1;
    for (int i = 0; i < ROUNDS; i++) {
        size_t n = round_size(i);
        unsigned char *p = malloc(n);
        if (!p)
            return 10;
        rep->allocs++;
        memset(p, i & 0xff, n);
        if (i < KEEP) {
            rep->keep[i] = p;
            rep->keep_n[i] = n;
        } else {
            free(p);
        }
    }
    rep->stage = 2;
    void *c = calloc(100, 100);
    if (!c)
        return 11;
    rep->allocs++;
    free(c);
    rep->stage = 3;
    return 0;
}

static int supervise(void *arg)
{
    int own_tls = *(int *)arg;
    alarm(TIMEOUT_S);
    size_t stack_size = 1 << 20;
    unsigned char *stack = mmap(NULL, stack_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    SETUP(stack != MAP_FAILED, "mmap stack");
    int flags = CLONE_VM | SIGCHLD;
    void *tls = NULL;
    if (own_tls) {
        size_t tls_size = 1 << 16;
        unsigned char *area = mmap(NULL, tls_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        SETUP(area != MAP_FAILED, "mmap tls");
        /* Zeros on both sides: static TLS sits below the thread pointer
         * on x86_64 and above it on aarch64. */
        tls = area + tls_size / 2;
        flags |= CLONE_SETTLS;
    }
    pid_t pid = clone(child_fn, stack + stack_size, flags, NULL, NULL, tls, NULL);
    SETUP(pid > 0, "clone: %s", strerror(errno));
    rep->child = pid;
    int status = 0;
    while (waitpid(pid, &status, __WALL) != pid)
        SETUP(errno == EINTR, "waitpid: %s", strerror(errno));
    rep->status = status;
    rep->reaped = 1;
    if (!own_tls && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        for (int i = 0; i < KEEP; i++) {
            const unsigned char *b = rep->keep[i];
            size_t n = rep->keep_n[i];
            if (!b || b[0] != (unsigned char)i || b[n - 1] != (unsigned char)i || malloc_usable_size((void *)b) < n)
                return 20;
            free((void *)b);
        }
        void *p = malloc(1000);
        if (!p)
            return 21;
        free(p);
        rep->used = 1;
    }
    return 0;
}

static void run(const char *name, int own_tls)
{
    memset(rep, 0, sizeof *rep);
    int sup = fork_and_wait(supervise, &own_tls);
    CHECK(sup != SETUP_FAILED, "%s: the supervisor could not run the experiment", name);
    if (rep->child > 0 && !rep->reaped)
        kill(rep->child, SIGKILL);
    char how[96];
    if (!rep->reaped)
        snprintf(how, sizeof how, "not reaped (supervisor %s %d)", sup < 0 ? "signal" : "exit", sup < 0 ? -sup : sup);
    else if (WIFEXITED(rep->status))
        snprintf(how, sizeof how, "exited %d", WEXITSTATUS(rep->status));
    else if (WIFSIGNALED(rep->status))
        snprintf(how, sizeof how, "killed by signal %d (%s)", WTERMSIG(rep->status), strsignal(WTERMSIG(rep->status)));
    else
        snprintf(how, sizeof how, "status %#x", rep->status);
    printf("raw_clone: %s: child %s at stage %d after %d allocations; supervisor %s %d%s%s\n", name, how, rep->stage,
           rep->allocs, sup < 0 ? "signal" : "exit", sup < 0 ? -sup : sup,
           sup == -SIGALRM ? " (timeout)" : "", rep->used ? "; supervisor checked and freed the child's blocks" : "");
    fflush(stdout);
}

int main(void)
{
    require_challoc();
    rep = mmap(NULL, sizeof *rep, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    REQUIRE(rep != MAP_FAILED, "mmap report");
    /* challoc is initialised and this thread has a heap before any fork. */
    void *warm = malloc(100);
    REQUIRE(warm != NULL, "malloc(100)");
    free(warm);
    run("shared", 0);
    run("own", 1);
    return finish("raw_clone");
}
