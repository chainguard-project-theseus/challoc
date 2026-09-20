/*
 * Smoke test for challoc's libc entry points (Phase 0 task 11), run under
 * LD_PRELOAD=libchalloc.so by the Rust test in src/api/libc.rs (which
 * compiles it without -Itests/c: common.h is found next to this file).
 * Fails fast when it is not actually running on challoc so a broken
 * preload cannot pass by accident. QUANTUM is passed by the build
 * (-DQUANTUM=16 or 8).
 *
 * The thorough per-feature programs are Phase 0 task 12; this one touches
 * every entry point once, checks errno semantics, and forks while another
 * thread is allocating so the atfork handlers get exercised.
 */
#include "common.h"

/* Sizes gcc must not see as constants (-Walloc-size-larger-than). */
static volatile size_t huge_n = SIZE_MAX;

static const size_t sizes[] = {0, 1, 7, 8, 15, 16, 17, 48, 100, 1000, 1024, 1025,
                               4096, 65536, 262144, 262145, 1 << 20, 10 << 20};
#define NSIZES (sizeof sizes / sizeof sizes[0])

static void test_basic(void)
{
    void *ptrs[NSIZES];
    for (size_t i = 0; i < NSIZES; i++) {
        size_t n = sizes[i];
        unsigned char *p = malloc(n);
        CHECK(p != NULL, "malloc(%zu)", n);
        CHECK(((uintptr_t)p % QUANTUM) == 0, "malloc(%zu) misaligned %p", n, (void *)p);
        size_t u = malloc_usable_size(p);
        CHECK(u >= n, "usable %zu < %zu", u, n);
        fill(p, u, (unsigned)i);
        ptrs[i] = p;
    }
    for (size_t i = 0; i < NSIZES; i++)
        CHECK(check_fill(ptrs[i], malloc_usable_size(ptrs[i]), (unsigned)i), "content %zu", sizes[i]);
    for (size_t i = 0; i < NSIZES; i++)
        free(ptrs[i]);

    void *a = malloc(0), *b = malloc(0);
    CHECK(a && b && a != b, "malloc(0) distinct");
    free(a);
    free(b);
    free(NULL);
    CHECK(malloc_usable_size(NULL) == 0, "usable(NULL)");
}

static void test_calloc(void)
{
    for (size_t i = 0; i < NSIZES; i++) {
        size_t n = sizes[i] ? sizes[i] : 1;
        unsigned char *p = malloc(n);
        memset(p, 0xAB, n);
        free(p);
        unsigned char *q = calloc(1, n);
        CHECK(q != NULL, "calloc(1, %zu)", n);
        int zero = 1;
        for (size_t k = 0; k < n; k++)
            zero &= q[k] == 0;
        CHECK(zero, "calloc(%zu) not zero", n);
        free(q);
    }
    errno = 0;
    sink = calloc(huge_n, 2);
    CHECK(sink == NULL && errno == ENOMEM, "calloc overflow");
}

static void test_realloc(void)
{
    static const size_t chain[] = {10, 100, 1000, 10000, 100000, 262145, 2 << 20, 100000, 1000, 10};
    size_t n = sizeof chain / sizeof chain[0];
    unsigned char *p = malloc(chain[0]);
    fill(p, chain[0], 7);
    size_t prev = chain[0];
    for (size_t i = 1; i < n; i++) {
        p = realloc(p, chain[i]);
        CHECK(p != NULL, "realloc to %zu", chain[i]);
        CHECK(check_fill(p, prev < chain[i] ? prev : chain[i], 7), "realloc content at %zu", chain[i]);
        fill(p, chain[i], 7);
        prev = chain[i];
    }
    uintptr_t before = (uintptr_t)p;
    p = realloc(p, prev);
    CHECK((uintptr_t)p == before, "same class keeps pointer");
    sink = realloc(p, 0);
    CHECK(sink == NULL, "realloc(p, 0)");

    errno = 0;
    sink = reallocarray(NULL, huge_n, 2);
    CHECK(sink == NULL && errno == ENOMEM, "reallocarray overflow");
    void *r = reallocarray(NULL, 8, 8);
    CHECK(r && malloc_usable_size(r) >= 64, "reallocarray");
    free(r);
}

static void test_aligned(void)
{
    long page = sysconf(_SC_PAGESIZE);
    for (size_t align = QUANTUM; align <= 65536; align <<= 1) {
        size_t reqs[] = {1, align - 1, align, align + 1, 3 * align, 300000};
        for (size_t i = 0; i < sizeof reqs / sizeof reqs[0]; i++) {
            size_t n = reqs[i];
            unsigned char *p = aligned_alloc(align, n);
            CHECK(p && ((uintptr_t)p % align) == 0, "aligned_alloc(%zu, %zu) = %p", align, n, (void *)p);
            CHECK(malloc_usable_size(p) >= n, "aligned usable");
            fill(p, n, 5);
            CHECK(check_fill(p, n, 5), "aligned content");
            free_aligned_sized(p, align, n);

            p = memalign(align, n);
            CHECK(p && ((uintptr_t)p % align) == 0, "memalign(%zu, %zu)", align, n);
            /* free_sized is defined only for malloc/calloc/realloc memory
             * (C23 7.24.3.4); aligned memory may sit in a larger class than
             * the size names and the size check would abort. */
            free_aligned_sized(p, align, n);

            void *q = NULL;
            CHECK(posix_memalign(&q, align, n) == 0 && q && ((uintptr_t)q % align) == 0,
                  "posix_memalign(%zu, %zu)", align, n);
            free(q);
        }
    }
    void *v = valloc(10);
    CHECK(v && ((uintptr_t)v % page) == 0, "valloc");
    free(v);
    void *pv = pvalloc((size_t)page + 1);
    CHECK(pv && ((uintptr_t)pv % page) == 0 && malloc_usable_size(pv) >= 2 * (size_t)page, "pvalloc");
    free(pv);
}

static void test_errno(void)
{
    void *p = malloc(100), *h = malloc(1 << 20);
    void *many[1000];
    for (int i = 0; i < 1000; i++)
        many[i] = malloc(64);
    errno = 42;
    free(p);
    free(h);
    for (int i = 0; i < 1000; i++)
        free(many[i]);
    CHECK(errno == 42, "free changed errno to %d", errno);

    errno = 0;
    sink = malloc(huge_n / 2);
    CHECK(sink == NULL && errno == ENOMEM, "malloc ENOMEM");
    errno = 0;
    sink = aligned_alloc(3, 8);
    CHECK(sink == NULL && errno == EINVAL, "aligned_alloc EINVAL");
    errno = 0;
    void *q = (void *)1;
    CHECK(posix_memalign(&q, 3, 8) == EINVAL && errno == 0 && q == (void *)1, "posix_memalign EINVAL");
    CHECK(posix_memalign(&q, 16, huge_n / 2) == ENOMEM, "posix_memalign ENOMEM");
}

/* ---- errno across a free that starts the background thread ------------ */

static int background_present(void)
{
    bool present = false;
    size_t sz = sizeof present;
    CHECK(mallctl("background_thread", &present, &sz, NULL, 0) == 0, "mallctl(background_thread) read");
    return present;
}

/* Under MALLOC_CONF=background_thread:true the thread is created by the
 * first periodic tick after init, never by init itself (design 8.1), and
 * the periodic work runs every 64 slow paths of a heap (PERIODIC_EVERY).
 * A fresh thread starts at zero: 64-byte blocks are cached 1024 deep
 * (MAX_CAP) and refilled 512 at a time, so allocating 20480 of them costs
 * 40 slow paths (plus a scheduled probe per 4096 allocations), and freeing
 * them costs one slow path per 512 once the cache is full, 38 more. The
 * 64th slow path of this thread is therefore a `free` (measured: the
 * thread appears between the 7680th and the 10240th free), and that free
 * creates the background thread (`pthread_create`, which may touch errno).
 * The two `background_present` checks make the test fail loudly, rather
 * than stop covering the path, if those constants move. */
static void *errno_background_thread(void *arg)
{
    (void)arg;
    enum { N = 20480 };
    static void *blocks[N];
    for (int i = 0; i < N; i++) {
        blocks[i] = malloc(64);
        CHECK(blocks[i] != NULL, "malloc(64) #%d", i);
    }
    CHECK(!background_present(), "background thread present before the frees: the allocations reached the tick");
    errno = 42;
    for (int i = 0; i < N; i++)
        free(blocks[i]);
    CHECK(errno == 42, "a free that started the background thread changed errno to %d", errno);
    CHECK(background_present(), "the frees did not start the background thread: the path is not covered");
    return NULL;
}

static void test_errno_background(void)
{
    CHECK(mallctl != NULL, "mallctl not exported");
    CHECK(!background_present(), "background thread present at start");
    pthread_t th;
    CHECK(pthread_create(&th, NULL, errno_background_thread, NULL) == 0, "errno thread");
    pthread_join(th, NULL);
}

/* ---- threads: cross-thread frees, exit with full caches ---------------- */

#define PER_THREAD 2000

struct worker {
    void *keep[PER_THREAD / 2];
    void *from_main[PER_THREAD / 2];
};

static void *worker(void *arg)
{
    struct worker *w = arg;
    void *mine[PER_THREAD];
    for (int i = 0; i < PER_THREAD; i++) {
        size_t n = 16 + (size_t)(i % 37) * 24;
        mine[i] = malloc(n);
        memset(mine[i], i & 0xff, n);
    }
    for (int i = 0; i < PER_THREAD / 2; i++)
        free(w->from_main[i]); /* allocated by main: cross-thread free */
    for (int i = 0; i < PER_THREAD; i += 2)
        free(mine[i]);
    for (int i = 1; i < PER_THREAD; i += 2)
        w->keep[i / 2] = mine[i];
    /* Leave with well-stocked caches so the exit destructor flushes. */
    void *tmp[600];
    for (int i = 0; i < 600; i++)
        tmp[i] = malloc(48);
    for (int i = 0; i < 600; i++)
        free(tmp[i]);
    return NULL;
}

static void test_threads(void)
{
    enum { N = 8 };
    pthread_t t[N];
    static struct worker w[N];
    for (int i = 0; i < N; i++)
        for (int k = 0; k < PER_THREAD / 2; k++)
            w[i].from_main[k] = malloc(32 + (size_t)(k % 5) * 16);
    for (int i = 0; i < N; i++)
        CHECK(pthread_create(&t[i], NULL, worker, &w[i]) == 0, "pthread_create");
    for (int i = 0; i < N; i++)
        pthread_join(t[i], NULL);
    for (int i = 0; i < N; i++)
        for (int k = 0; k < PER_THREAD / 2; k++) {
            unsigned char *p = w[i].keep[k];
            CHECK(p[0] == (unsigned char)((2 * k + 1) & 0xff), "thread %d block %d content", i, k);
            free(p);
        }
}

/* ---- fork ---------------------------------------------------------------- */

static volatile int stop_hammer;

static void *hammer(void *arg)
{
    (void)arg;
    void *ring[64] = {0};
    unsigned i = 0;
    while (!stop_hammer) {
        free(ring[i % 64]);
        ring[i % 64] = malloc(16 + (i % 100) * 8);
        if (i % 500 == 0) {
            void *h = malloc(300000);
            free(h);
        }
        i++;
    }
    for (int k = 0; k < 64; k++)
        free(ring[k]);
    return NULL;
}

static int child_body(unsigned char *inherited)
{
    if (!check_fill(inherited, 1000, 11))
        return 2;
    void *ptrs[500];
    for (int i = 0; i < 500; i++) {
        ptrs[i] = malloc(8 + (size_t)i * 3);
        if (!ptrs[i])
            return 3;
        memset(ptrs[i], 1, 8 + (size_t)i * 3);
    }
    void *h = calloc(1, 4 << 20);
    if (!h || ((unsigned char *)h)[(4 << 20) - 1] != 0)
        return 4;
    free(h);
    for (int i = 0; i < 500; i++)
        free(ptrs[i]);
    free(inherited);
    return 0;
}

static void test_fork(void)
{
    pthread_t th;
    stop_hammer = 0;
    CHECK(pthread_create(&th, NULL, hammer, NULL) == 0, "hammer thread");
    unsigned char *inherited = malloc(1000);
    fill(inherited, 1000, 11);
    for (int round = 0; round < 20; round++) {
        pid_t pid = fork();
        CHECK(pid >= 0, "fork");
        if (pid == 0)
            _exit(child_body(inherited));
        int status = 0;
        CHECK(waitpid(pid, &status, 0) == pid, "waitpid");
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child round %d status %#x", round, status);
        /* Parent keeps allocating between forks. */
        void *p = malloc(777);
        free(p);
    }
    stop_hammer = 1;
    pthread_join(th, NULL);
    CHECK(check_fill(inherited, 1000, 11), "parent block intact after forks");
    free(inherited);
}

int main(int argc, char **argv)
{
    require_challoc();

    /* `libc_api background`, run with MALLOC_CONF=background_thread:true. */
    if (argc > 1 && strcmp(argv[1], "background") == 0) {
        test_errno_background();
        return finish("libc_api");
    }

    test_basic();
    test_calloc();
    test_realloc();
    test_aligned();
    test_errno();
    test_threads();
    test_fork();
    return finish("libc_api");
}
