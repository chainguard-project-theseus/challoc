/*
 * cxx_new: the C++ allocation operators are served by challoc itself
 * (src/api/cxx.rs), not by libstdc++ forwarding to malloc. Checks, under
 * LD_PRELOAD=libchalloc.so:
 *
 *   - every operator new/delete signature resolves into libchalloc.so;
 *   - new/new[]/nothrow/aligned/sized round trips across 8 threads with
 *     cross-thread deletes, memory filled and verified;
 *   - operator new(0) returns distinct non-null pointers;
 *   - an impossible request throws std::bad_alloc through challoc's frames
 *     and the nothrow form returns nullptr;
 *   - an installed std::new_handler is called until it gives up, then
 *     bad_alloc is thrown (or nullptr returned).
 *
 * Compiled by src/api/ctests.rs with the container's C++ compiler.
 */
#include <dlfcn.h>
#include <malloc.h>
#include <pthread.h>

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>

#ifndef QUANTUM
#error "compile with -DQUANTUM=16 or -DQUANTUM=8"
#endif

static std::atomic<int> failures;
static void *volatile sink;

#define CHECK(cond, ...)                                                   \
    do {                                                                   \
        if (!(cond)) {                                                     \
            failures++;                                                    \
            fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__);           \
            fprintf(stderr, __VA_ARGS__);                                  \
            fputc('\n', stderr);                                           \
        }                                                                  \
    } while (0)

static void require_challoc()
{
    void *probe = malloc(1);
    size_t u = malloc_usable_size(probe);
    if (u != QUANTUM || dlsym(RTLD_DEFAULT, "challoc_tls_get") == nullptr) {
        fprintf(stderr, "not running under challoc (usable(1) = %zu)\n", u);
        exit(1);
    }
    free(probe);
}

static void fill(void *p, size_t n, uint32_t seed)
{
    unsigned char *b = static_cast<unsigned char *>(p);
    for (size_t i = 0; i < n; i++)
        b[i] = static_cast<unsigned char>(seed * 31u + i * 7u + (i >> 8));
}

static bool check_fill(const void *p, size_t n, uint32_t seed)
{
    const unsigned char *b = static_cast<const unsigned char *>(p);
    for (size_t i = 0; i < n; i++)
        if (b[i] != static_cast<unsigned char>(seed * 31u + i * 7u + (i >> 8)))
            return false;
    return true;
}

static uint64_t rng_next(uint64_t *s)
{
    uint64_t x = *s;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *s = x;
    return x * 0x2545F4914F6CDD1DULL;
}

/* ---- symbol resolution --------------------------------------------- */

static void check_module(const char *what, void *fn)
{
    Dl_info info;
    const char *m = (dladdr(fn, &info) && info.dli_fname) ? info.dli_fname : "?";
    CHECK(strstr(m, "libchalloc") != nullptr, "%s resolved to %s, not libchalloc", what, m);
}

static void test_symbols()
{
    using nt = const std::nothrow_t &;
    using av = std::align_val_t;
    check_module("operator new", reinterpret_cast<void *>(static_cast<void *(*)(size_t)>(operator new)));
    check_module("operator new[]", reinterpret_cast<void *>(static_cast<void *(*)(size_t)>(operator new[])));
    check_module("operator new(nothrow)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, nt)>(operator new)));
    check_module("operator new[](nothrow)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, nt)>(operator new[])));
    check_module("operator new(align)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, av)>(operator new)));
    check_module("operator new[](align)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, av)>(operator new[])));
    check_module("operator new(align, nothrow)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, av, nt)>(operator new)));
    check_module("operator new[](align, nothrow)", reinterpret_cast<void *>(static_cast<void *(*)(size_t, av, nt)>(operator new[])));
    check_module("operator delete", reinterpret_cast<void *>(static_cast<void (*)(void *)>(operator delete)));
    check_module("operator delete[]", reinterpret_cast<void *>(static_cast<void (*)(void *)>(operator delete[])));
    check_module("operator delete(nothrow)", reinterpret_cast<void *>(static_cast<void (*)(void *, nt)>(operator delete)));
    check_module("operator delete[](nothrow)", reinterpret_cast<void *>(static_cast<void (*)(void *, nt)>(operator delete[])));
    check_module("operator delete(size)", reinterpret_cast<void *>(static_cast<void (*)(void *, size_t)>(operator delete)));
    check_module("operator delete[](size)", reinterpret_cast<void *>(static_cast<void (*)(void *, size_t)>(operator delete[])));
    check_module("operator delete(align)", reinterpret_cast<void *>(static_cast<void (*)(void *, av)>(operator delete)));
    check_module("operator delete[](align)", reinterpret_cast<void *>(static_cast<void (*)(void *, av)>(operator delete[])));
    check_module("operator delete(align, nothrow)", reinterpret_cast<void *>(static_cast<void (*)(void *, av, nt)>(operator delete)));
    check_module("operator delete[](align, nothrow)", reinterpret_cast<void *>(static_cast<void (*)(void *, av, nt)>(operator delete[])));
    check_module("operator delete(size, align)", reinterpret_cast<void *>(static_cast<void (*)(void *, size_t, av)>(operator delete)));
    check_module("operator delete[](size, align)", reinterpret_cast<void *>(static_cast<void (*)(void *, size_t, av)>(operator delete[])));
}

/* ---- round trips ---------------------------------------------------- */

struct Block {
    void *p;
    size_t n;
    uint32_t seed;
};

#define PER_THREAD 512
#define HANDOFF 64
#define THREADS 8

static Block handoff[THREADS][HANDOFF];

static void *thread_body(void *arg)
{
    int id = static_cast<int>(reinterpret_cast<intptr_t>(arg));
    uint64_t rng = 0x9E3779B97F4A7C15ULL ^ (uint64_t)(id + 1) * 0x100000001B3ULL;
    Block live[PER_THREAD];
    memset(live, 0, sizeof live);

    for (int iter = 0; iter < 20000; iter++) {
        Block &b = live[rng_next(&rng) % PER_THREAD];
        if (b.p) {
            CHECK(check_fill(b.p, b.n, b.seed), "thread %d: block %p corrupt", id, b.p);
            switch (rng_next(&rng) % 4) {
            case 0: operator delete(b.p); break;
            case 1: operator delete[](b.p); break;
            case 2: operator delete(b.p, b.n); break;
            default: operator delete[](b.p, b.n); break;
            }
            b.p = nullptr;
        }
        /* Sizes 1..4096 mostly, 1/16 of the time up to 300 KiB (huge). */
        size_t n = (rng_next(&rng) % 16 == 0) ? 1 + rng_next(&rng) % (300 << 10) : 1 + rng_next(&rng) % 4096;
        uint32_t seed = static_cast<uint32_t>(rng_next(&rng));
        void *p;
        switch (rng_next(&rng) % 4) {
        case 0: p = operator new(n); break;
        case 1: p = new char[n]; break;
        case 2:
            p = operator new(n, std::nothrow);
            CHECK(p != nullptr, "thread %d: nothrow new(%zu) failed", id, n);
            break;
        default: {
            size_t align = size_t(16) << (rng_next(&rng) % 9); /* 16 .. 4096 */
            p = operator new(n, std::align_val_t(align));
            CHECK((reinterpret_cast<uintptr_t>(p) & (align - 1)) == 0, "thread %d: new(%zu, align %zu) = %p misaligned", id, n, align, p);
            break;
        }
        }
        CHECK(p != nullptr, "thread %d: new(%zu) returned null", id, n);
        CHECK(malloc_usable_size(p) >= n, "thread %d: usable %zu < %zu", id, malloc_usable_size(p), n);
        fill(p, n, seed);
        b = Block{p, n, seed};
    }

    /* Hand HANDOFF live blocks to main; delete the rest here. */
    int given = 0;
    for (int i = 0; i < PER_THREAD; i++) {
        if (!live[i].p)
            continue;
        if (given < HANDOFF) {
            handoff[id][given++] = live[i];
        } else {
            CHECK(check_fill(live[i].p, live[i].n, live[i].seed), "thread %d: block %p corrupt at exit", id, live[i].p);
            operator delete[](live[i].p, live[i].n);
        }
    }
    return nullptr;
}

static void test_threads()
{
    pthread_t t[THREADS];
    for (int i = 0; i < THREADS; i++)
        pthread_create(&t[i], nullptr, thread_body, reinterpret_cast<void *>(static_cast<intptr_t>(i)));
    for (int i = 0; i < THREADS; i++)
        pthread_join(t[i], nullptr);
    int handed = 0;
    for (int i = 0; i < THREADS; i++) {
        for (int j = 0; j < HANDOFF; j++) {
            Block &b = handoff[i][j];
            if (!b.p)
                continue;
            handed++;
            CHECK(check_fill(b.p, b.n, b.seed), "handed-off block %p from thread %d corrupt", b.p, i);
            operator delete(b.p);
        }
    }
    CHECK(handed > THREADS * HANDOFF / 2, "only %d blocks handed off", handed);
}

static void test_zero_and_aligned()
{
    void *a = operator new(0);
    void *b = operator new[](0);
    CHECK(a != nullptr && b != nullptr && a != b, "new(0) must give distinct non-null pointers (%p, %p)", a, b);
    operator delete(a);
    operator delete[](b);

    void *c = operator new(100, std::align_val_t(4096));
    CHECK(c != nullptr && (reinterpret_cast<uintptr_t>(c) & 4095) == 0, "new(100, align 4096) = %p", c);
    fill(c, 100, 7);
    CHECK(check_fill(c, 100, 7), "aligned block corrupt");
    operator delete(c, std::align_val_t(4096));

    void *d = operator new[](1 << 20, std::align_val_t(1 << 16), std::nothrow);
    CHECK(d != nullptr && (reinterpret_cast<uintptr_t>(d) & 0xffff) == 0, "new[](1 MiB, align 64 KiB, nothrow) = %p", d);
    operator delete[](d, size_t(1 << 20), std::align_val_t(1 << 16));

    void *e = operator new(64, std::align_val_t(64), std::nothrow);
    CHECK(e != nullptr && (reinterpret_cast<uintptr_t>(e) & 63) == 0, "new(64, align 64, nothrow) = %p", e);
    operator delete(e, std::align_val_t(64), std::nothrow);
}

/* ---- failure protocol ------------------------------------------------ */

static const size_t IMPOSSIBLE = SIZE_MAX / 4;

static void test_bad_alloc()
{
    bool caught = false;
    try {
        sink = operator new(IMPOSSIBLE);
        CHECK(false, "new(SIZE_MAX / 4) returned %p", sink);
    } catch (const std::bad_alloc &) {
        caught = true;
    }
    CHECK(caught, "new(SIZE_MAX / 4) did not throw std::bad_alloc");

    caught = false;
    try {
        sink = new char[IMPOSSIBLE];
        CHECK(false, "new char[SIZE_MAX / 4] returned %p", sink);
    } catch (const std::bad_alloc &) {
        caught = true;
    }
    CHECK(caught, "new char[SIZE_MAX / 4] did not throw std::bad_alloc");

    caught = false;
    try {
        sink = operator new(IMPOSSIBLE, std::align_val_t(4096));
        CHECK(false, "aligned new(SIZE_MAX / 4) returned %p", sink);
    } catch (const std::bad_alloc &) {
        caught = true;
    }
    CHECK(caught, "aligned new(SIZE_MAX / 4) did not throw std::bad_alloc");

    sink = operator new(IMPOSSIBLE, std::nothrow);
    CHECK(sink == nullptr, "nothrow new(SIZE_MAX / 4) returned %p", sink);
    sink = operator new[](IMPOSSIBLE, std::align_val_t(4096), std::nothrow);
    CHECK(sink == nullptr, "nothrow aligned new[](SIZE_MAX / 4) returned %p", sink);

    /* The allocator must still work after the failures. */
    void *p = operator new(100);
    CHECK(p != nullptr, "new(100) after bad_alloc failed");
    operator delete(p);
}

static std::atomic<int> handler_calls;

static void counting_handler()
{
    if (++handler_calls == 3)
        std::set_new_handler(nullptr);
}

static void test_new_handler()
{
    handler_calls = 0;
    std::set_new_handler(counting_handler);
    bool caught = false;
    try {
        sink = operator new(IMPOSSIBLE);
        CHECK(false, "new with handler returned %p", sink);
    } catch (const std::bad_alloc &) {
        caught = true;
    }
    CHECK(caught, "new with handler did not throw after the handler gave up");
    CHECK(handler_calls == 3, "handler called %d times, expected 3", handler_calls.load());
    CHECK(std::get_new_handler() == nullptr, "handler still installed");

    handler_calls = 0;
    std::set_new_handler(counting_handler);
    sink = operator new(IMPOSSIBLE, std::nothrow);
    CHECK(sink == nullptr, "nothrow new with handler returned %p", sink);
    CHECK(handler_calls == 3, "nothrow: handler called %d times, expected 3", handler_calls.load());
    std::set_new_handler(nullptr);
}

int main()
{
    require_challoc();
    test_symbols();
    test_zero_and_aligned();
    test_threads();
    test_bad_alloc();
    test_new_handler();
    if (failures) {
        fprintf(stderr, "cxx_new: %d failures\n", failures.load());
        return 1;
    }
    printf("ok cxx_new\n");
    return 0;
}
