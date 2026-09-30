/* Per-CPU caches (docs/design.md section 15), both
 * ways. Run as LD_PRELOAD=target/release/libchalloc.so ./percpu MODE with
 * MALLOC_CONF=percpu_arena:MODE (percpu or off; the driver in
 * src/api/ctests.rs sets it). Under `off` the mode must be off whatever
 * the kernel offers; under `percpu` it is on where glibc registered an
 * rseq area covering `mm_cid` (the reference VMs) and off with a note
 * elsewhere (the Mac's Docker VM), and the program prints which. Either
 * way the same workload must hold: 64 threads (more than the VMs' CPUs,
 * so ids are shared over time and threads migrate) allocate and free at
 * random across the small classes with a per-thread tag written into
 * every block and checked before its free (a slot handed to two threads
 * at once would show up as a foreign tag), calloc keeps reading as zero
 * while dirty blocks are freed around it, `thread.allocated` and
 * `thread.deallocated` move by exactly the usable size of one allocation
 * and one free on the calling thread, `thread.tcache.flush` returns 0, and
 * `stats.percpu.*` agree with the mode. */
#include "common.h"
#include <pthread.h>
#include <stdint.h>

#define NTHREADS 64
#define OPS 20000
#define LIVE 256

static mallctl_fn ctl;

static int get_bool(const char *key, bool *out)
{
    size_t len = sizeof *out;
    return ctl(key, out, &len, NULL, 0);
}

static int get_size(const char *key, size_t *out)
{
    size_t len = sizeof *out;
    return ctl(key, out, &len, NULL, 0);
}

static uint64_t get_u64(const char *key)
{
    uint64_t v = 0;
    size_t len = sizeof v;
    CHECK(ctl(key, &v, &len, NULL, 0) == 0, "%s", key);
    return v;
}

static const size_t sizes[] = { 8, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096 };
#define NSIZES (sizeof sizes / sizeof sizes[0])

struct job {
    int index;
    uint64_t ops;
};

static void *worker(void *arg)
{
    struct job *j = arg;
    uint64_t tag = ((uint64_t)j->index + 1) * 0x0101010101010101ULL;
    uint64_t rng = 0x9E3779B97F4A7C15ULL ^ (uint64_t)(j->index + 1);
    void *live[LIVE];
    size_t nlive = 0;
    for (int i = 0; i < OPS; i++) {
        uint64_t r = rng_next(&rng);
        if (nlive < LIVE && ((r & 1) == 0 || nlive == 0)) {
            size_t sz = sizes[(r >> 8) % NSIZES];
            uint64_t *p;
            if ((r >> 16) % 8 == 0) {
                p = calloc(1, sz);
                REQUIRE(p != NULL, "calloc(%zu)", sz);
                if (!is_zero(p, sz)) {
                    CHECK(0, "thread %d: calloc(%zu) not zero", j->index, sz);
                }
            } else {
                p = malloc(sz);
                REQUIRE(p != NULL, "malloc(%zu)", sz);
            }
            p[0] = tag;
            if (sz > sizeof tag)
                ((unsigned char *)p)[sz - 1] = (unsigned char)j->index;
            live[nlive++] = p;
        } else {
            size_t k = (size_t)(r >> 1) % nlive;
            uint64_t *p = live[k];
            live[k] = live[--nlive];
            if (p[0] != tag) {
                CHECK(0, "thread %d: block %p carries tag %llx, not %llx", j->index, (void *)p,
                      (unsigned long long)p[0], (unsigned long long)tag);
            }
            p[0] = ~tag; /* dirty: a later calloc must not see this */
            free(p);
        }
        j->ops++;
    }
    for (size_t k = 0; k < nlive; k++) {
        uint64_t *p = live[k];
        CHECK(p[0] == tag, "thread %d: survivor %p carries tag %llx", j->index, (void *)p, (unsigned long long)p[0]);
        p[0] = ~tag;
        free(p);
    }
    return NULL;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "percpu";
    require_challoc();
    ctl = mallctl_dl();
    REQUIRE(ctl != NULL, "mallctl");

    const char *opt = NULL;
    size_t len = sizeof opt;
    REQUIRE(ctl("opt.percpu_arena", &opt, &len, NULL, 0) == 0 && opt != NULL, "opt.percpu_arena");
    CHECK(strcmp(opt, mode) == 0, "opt.percpu_arena is %s, expected %s", opt, mode);
    bool enabled = false;
    REQUIRE(get_bool("stats.percpu.enabled", &enabled) == 0, "stats.percpu.enabled");
    size_t ids = 0, headers = 0, cached = 0;
    REQUIRE(get_size("stats.percpu.ids", &ids) == 0, "stats.percpu.ids");
    REQUIRE(get_size("stats.percpu.headers", &headers) == 0, "stats.percpu.headers");
    REQUIRE(get_size("stats.percpu.cached", &cached) == 0, "stats.percpu.cached");
    if (strcmp(mode, "off") == 0)
        CHECK(!enabled, "percpu_arena:off but the mode is on");
    else if (!enabled)
        skip_note("percpu/percpu", "mm_cid is not usable on this kernel or glibc, the per-CPU mode is off; the workload ran on thread caches");
    CHECK(enabled == (ids > 0), "ids %zu with enabled %d", ids, enabled);
    if (!enabled)
        CHECK(headers == 0 && cached == 0, "nothing installed while off (%zu, %zu)", headers, cached);

    /* thread.allocated / deallocated move by exactly the usable size. */
    uint64_t a0 = get_u64("thread.allocated"), d0 = get_u64("thread.deallocated");
    void *volatile block = malloc(200);
    REQUIRE(block != NULL, "malloc(200)");
    size_t usable = malloc_usable_size(block);
    CHECK(get_u64("thread.allocated") == a0 + usable, "thread.allocated moved by %llu, expected %zu",
          (unsigned long long)(get_u64("thread.allocated") - a0), usable);
    free(block);
    CHECK(get_u64("thread.deallocated") == d0 + usable, "thread.deallocated moved by %llu, expected %zu",
          (unsigned long long)(get_u64("thread.deallocated") - d0), usable);

    pthread_t threads[NTHREADS];
    struct job jobs[NTHREADS];
    for (int i = 0; i < NTHREADS; i++) {
        jobs[i].index = i;
        jobs[i].ops = 0;
        REQUIRE(pthread_create(&threads[i], NULL, worker, &jobs[i]) == 0, "pthread_create");
    }
    uint64_t ops = 0;
    for (int i = 0; i < NTHREADS; i++) {
        pthread_join(threads[i], NULL);
        ops += jobs[i].ops;
    }
    CHECK(ops == (uint64_t)NTHREADS * OPS, "ops %llu", (unsigned long long)ops);

    CHECK(ctl("thread.tcache.flush", NULL, NULL, NULL, 0) == 0, "thread.tcache.flush");
    size_t aborts = 0;
    REQUIRE(get_size("stats.percpu.headers", &headers) == 0, "stats.percpu.headers");
    REQUIRE(get_size("stats.percpu.cached", &cached) == 0, "stats.percpu.cached");
    REQUIRE(get_size("stats.percpu.aborts", &aborts) == 0, "stats.percpu.aborts");
    if (enabled) {
        CHECK(headers > 0, "the workload installed no per-CPU header");
        CHECK(cached <= ids * NSIZES * 1024, "cached %zu above the table's capacity", cached);
        /* Sections are aborted only when a preemption or a signal lands inside
         * one; over this short run that is a handful at most (0 has been seen),
         * so the count is reported, not required: the Rust test
         * `percpu_sections_abort_and_retry_under_signals` forces the path. */
    } else {
        CHECK(headers == 0 && cached == 0 && aborts == 0, "per-CPU state while off (%zu, %zu, %zu)", headers, cached, aborts);
    }
    printf("percpu: mode %s enabled=%d ids=%zu headers=%zu cached=%zu aborts=%zu after %d threads x %d ops\n", mode,
           enabled, ids, headers, cached, aborts, NTHREADS, OPS);
    return finish("percpu");
}
