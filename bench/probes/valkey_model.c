// valkey_model.c: Valkey's allocator-visible shape without Valkey, single
// thread (docs/plan.md Phase 1.5 task 5c; the rows `valkey1`,
// `valkey1-free` and `valkey1-usable` of bench/lib.sh).
//
// What the soak's server does to the allocator, per key (Valkey 9.1.2,
// src/zmalloc.c, src/db.c): every key holds a dict entry (40 B), a key sds
// (about 24 B), an object header (16 B) and a value sds (the value plus a
// 5-byte header); a SET on an existing key allocates a new object and value
// and frees the old two (the entry and key stay); a DEL frees all four; a
// DEL of a missing key touches nothing. Frees go through
// `je_sdallocx(ptr, size, 0)` with the size Valkey knows, so this program
// frees through `sdallocx` when the loaded allocator exports it (weak
// reference; challoc and jemalloc do, glibc and mimalloc do not, so those
// fall back to `free`), or `-f free` / `-f sdallocx` to force either. The
// unsized `zfree(ptr)` (dict entries, objects, robj values) first asks
// `zmalloc_size(ptr)` = `je_malloc_usable_size` for the accounting and then
// calls `je_sdallocx(ptr, that size, 0)`; `-u` models that: every free is
// preceded by a `malloc_usable_size` query of the pointer (the row
// `valkey1-usable`, plan Phase 1.5 task 14).
//
// Protocol, mirroring bench/valkey-soak.sh in miniature:
//
//   fill      N keys with values drawn uniformly from MIN..MAX bytes
//             (default 16..1024, memtier's --data-size-range); with the
//             default N = 850000 the live heap is about 500 MiB, the soak's
//             write-phase heap, so the descriptor of a randomly chosen
//             key's value is not in cache when it is freed (the mechanism
//             of bench/results/2026-09-18-aarch64-gce-valkey-throughput.md)
//   write     W seconds of SET over random keys (overwrite: allocate new
//             value and object, free the old pair); the heap stays full
//   delete    D seconds of DEL:SET at RATIO:1 over random keys (DEL of a
//             key that is gone is a miss, as memtier's random DELs are);
//             the key count drains toward N/(RATIO+1) as in the soak
//
// Output: one line per phase with ops/s, live keys and RSS (VmRSS/VmHWM
// from /proc/self/status), plus, with -s and a mallctl-exporting allocator,
// stats.allocated/active/resident after an epoch bump; the last line is
// machine-readable:
//   ops total=<n> write_mops=<x> delete_mops=<y> total_mops=<z> live=<k> rss_kb=<r> hwm_kb=<h> free=<mode>
// bench/lib.sh scores the row by write_mops (the phase whose gap the soak
// shows widest and the one that reproduces the large-heap free miss); the
// delete phase's rate and the RSS after it are in the log for attribution.
//
//   -n N        keys (default 850000)
//   -t T        seconds: T/2 write, T/2 delete (default 10)
//   -w W -d D   write and delete seconds separately (override -t)
//   -r R        DEL:SET ratio in the delete phase (default 4)
//   -m MIN -M MAX  value size range (default 16 1024)
//   -f MODE     free|sdallocx|auto (default auto)
//   -u          call malloc_usable_size(p) before every free, as zfree does
//   -S SEED     xorshift seed
//   -s          print allocator stats through mallctl at each phase end
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
#include <malloc.h>

extern void sdallocx(void *, size_t, int) __attribute__((weak));
extern int mallctl(const char *, void *, size_t *, void *, size_t) __attribute__((weak));

enum { KEY_BYTES = 24, ENTRY_BYTES = 40, OBJ_BYTES = 16, SDS_HDR = 5 };

struct key {
    char *val, *obj, *key, *ent;
    uint32_t vsize;   // bytes requested for val (value + SDS_HDR); 0 = not live
};

static uint64_t rs = 0x9e3779b97f4a7c15ull;
static uint64_t rnd(void) { rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17; return rs; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }

static int use_sdallocx, query_usable;
static unsigned long long usable_bytes;   // what the queries returned (zfree feeds it to used_memory)
static void release(void *p, size_t size) {
    if (query_usable) usable_bytes += malloc_usable_size(p);
    if (use_sdallocx) sdallocx(p, size, 0); else free(p);
}
static char *take(size_t size) {
    char *p = malloc(size);
    if (!p) { fprintf(stderr, "valkey_model: malloc(%zu) failed\n", size); exit(1); }
    p[0] = 1; p[size - 1] = 1;   // touch first and last byte, as a memcpy of the value would
    return p;
}

static void rss(long *rss_kb, long *hwm_kb) {
    *rss_kb = *hwm_kb = 0;
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return;
    char line[256];
    while (fgets(line, sizeof line, f)) {
        if (!strncmp(line, "VmRSS:", 6)) *rss_kb = atol(line + 6);
        else if (!strncmp(line, "VmHWM:", 6)) *hwm_kb = atol(line + 6);
    }
    fclose(f);
}
static int want_stats;
static void report_mem(void) {
    long r, h; rss(&r, &h);
    printf(" rss %.1f MiB hwm %.1f MiB", r / 1024.0, h / 1024.0);
    if (want_stats && mallctl) {
        uint64_t e = 1; size_t l = sizeof e; mallctl("epoch", &e, &l, &e, sizeof e);
        size_t allocated = 0, active = 0, resident = 0; l = sizeof allocated;
        mallctl("stats.allocated", &allocated, &l, NULL, 0);
        mallctl("stats.active", &active, &l, NULL, 0);
        mallctl("stats.resident", &resident, &l, NULL, 0);
        printf("; allocated %.1f MiB active %.1f MiB resident %.1f MiB", allocated / 1048576.0, active / 1048576.0, resident / 1048576.0);
    }
    printf("\n");
}

static size_t vmin = 16, vmax = 1024;
static uint32_t value_size(void) { return (uint32_t)(vmin + rnd() % (vmax - vmin + 1) + SDS_HDR); }

static size_t live_keys, live_bytes;
static void set_new(struct key *k) {   // SET of a missing key: four allocations
    k->vsize = value_size();
    k->key = take(KEY_BYTES); k->ent = take(ENTRY_BYTES); k->obj = take(OBJ_BYTES); k->val = take(k->vsize);
    live_keys++; live_bytes += KEY_BYTES + ENTRY_BYTES + OBJ_BYTES + k->vsize;
}
static void set_overwrite(struct key *k) {   // SET of a live key: new object and value, old pair freed
    uint32_t ns = value_size();
    char *nv = take(ns), *no = take(OBJ_BYTES);
    release(k->val, k->vsize); release(k->obj, OBJ_BYTES);
    live_bytes += ns; live_bytes -= k->vsize;
    k->val = nv; k->obj = no; k->vsize = ns;
}
static void del(struct key *k) {   // DEL of a live key: all four freed
    release(k->val, k->vsize); release(k->obj, OBJ_BYTES); release(k->key, KEY_BYTES); release(k->ent, ENTRY_BYTES);
    live_keys--; live_bytes -= KEY_BYTES + ENTRY_BYTES + OBJ_BYTES + k->vsize;
    k->vsize = 0; k->val = k->obj = k->key = k->ent = NULL;
}

static void usage(void) {
    fprintf(stderr, "usage: valkey_model [-n keys] [-t secs | -w write_secs -d delete_secs] [-r del:set] [-m min -M max] [-f free|sdallocx|auto] [-u] [-S seed] [-s]\n");
    exit(2);
}

int main(int argc, char **argv) {
    size_t n = 850000;
    double tw = -1, td = -1, total = 10;
    unsigned ratio = 4;
    const char *mode = "auto";
    int opt;
    while ((opt = getopt(argc, argv, "n:t:w:d:r:m:M:f:S:suh")) != -1) {
        switch (opt) {
        case 'n': n = strtoul(optarg, 0, 0); break;
        case 't': total = atof(optarg); break;
        case 'w': tw = atof(optarg); break;
        case 'd': td = atof(optarg); break;
        case 'r': ratio = (unsigned)strtoul(optarg, 0, 0); break;
        case 'm': vmin = strtoul(optarg, 0, 0); break;
        case 'M': vmax = strtoul(optarg, 0, 0); break;
        case 'f': mode = optarg; break;
        case 'S': rs = strtoull(optarg, 0, 0); if (!rs) rs = 1; break;
        case 's': want_stats = 1; break;
        case 'u': query_usable = 1; break;
        default: usage();
        }
    }
    if (tw < 0) tw = total / 2;
    if (td < 0) td = total / 2;
    if (!n || vmin < 1 || vmax < vmin || ratio == 0) usage();
    if (!strcmp(mode, "sdallocx")) {
        if (!sdallocx) { fprintf(stderr, "valkey_model: -f sdallocx but the loaded allocator does not export sdallocx\n"); return 2; }
        use_sdallocx = 1;
    } else if (!strcmp(mode, "free")) {
        use_sdallocx = 0;
    } else if (!strcmp(mode, "auto")) {
        use_sdallocx = sdallocx != NULL;
    } else usage();
    const char *freename = use_sdallocx ? "sdallocx" : "free";

    printf("valkey_model: n=%zu values %zu..%zu B (+%d B sds header; %d B object, %d B key, %d B entry per key), free=%s%s%s, seed=%#llx, write %.1f s, delete %.1f s at DEL:SET %u:1%s\n",
           n, vmin, vmax, SDS_HDR, OBJ_BYTES, KEY_BYTES, ENTRY_BYTES, freename,
           query_usable ? " (malloc_usable_size before every free)" : "",
           !strcmp(mode, "auto") ? (sdallocx ? " (auto: sdallocx exported)" : " (auto: no sdallocx, plain free)") : "",
           (unsigned long long)rs, tw, td, ratio, mallctl ? ", mallctl available" : ", no mallctl");
    fflush(stdout);

    // The key table itself is one large allocation from the allocator under
    // test, as Valkey's dict tables are.
    struct key *keys = calloc(n, sizeof *keys);
    if (!keys) { fprintf(stderr, "valkey_model: calloc of the key table failed\n"); return 1; }

    // ---- fill --------------------------------------------------------------
    double t0 = now();
    for (size_t i = 0; i < n; i++) set_new(&keys[i]);
    printf("fill: %zu keys, %zu allocations, %.2f s, %.1f MiB requested;", n, 4 * n, now() - t0, live_bytes / 1048576.0);
    report_mem();
    fflush(stdout);

    // ---- write phase: SET only, every key live -------------------------------
    unsigned long sets_w = 0;
    double tstart = now(), tend = tstart + tw, tnow = tstart;
    while (tnow < tend) {
        for (int i = 0; i < 1024; i++) { set_overwrite(&keys[rnd() % n]); }
        sets_w += 1024;
        tnow = now();
    }
    double dw = tnow - tstart;
    double write_mops = dw > 0 ? sets_w / dw / 1e6 : 0;
    printf("write: %lu SET in %.2f s = %.3f M ops/s (%lu frees); %zu keys live, %.1f MiB requested;", sets_w, dw, write_mops, 2 * sets_w, live_keys, live_bytes / 1048576.0);
    report_mem();
    fflush(stdout);

    // ---- delete phase: DEL:SET ratio:1 over random keys --------------------------
    unsigned long dels = 0, del_hits = 0, sets_d = 0, overwrites = 0;
    tstart = now(); tend = tstart + td; tnow = tstart;
    unsigned span = ratio + 1;
    while (tnow < tend) {
        for (int i = 0; i < 1024; i++) {
            uint64_t r = rnd();
            struct key *k = &keys[r % n];
            if ((r >> 40) % span < ratio) {
                dels++;
                if (k->vsize) { del(k); del_hits++; }
            } else {
                sets_d++;
                if (k->vsize) { set_overwrite(k); overwrites++; } else set_new(k);
            }
        }
        tnow = now();
    }
    double dd = tnow - tstart;
    unsigned long ops_d = dels + sets_d;
    double delete_mops = dd > 0 ? ops_d / dd / 1e6 : 0;
    printf("delete: %lu ops in %.2f s = %.3f M ops/s (DEL %lu, hits %lu; SET %lu, overwrites %lu; %lu frees); %zu keys live, %.1f MiB requested;",
           ops_d, dd, delete_mops, dels, del_hits, sets_d, overwrites, 4 * del_hits + 2 * overwrites, live_keys, live_bytes / 1048576.0);
    report_mem();

    long r, h; rss(&r, &h);
    unsigned long ops_total = sets_w + ops_d;
    double total_mops = (dw + dd) > 0 ? ops_total / (dw + dd) / 1e6 : 0;
    printf("ops total=%lu write_mops=%.3f delete_mops=%.3f total_mops=%.3f live=%zu rss_kb=%ld hwm_kb=%ld free=%s usable=%d usable_bytes=%llu\n",
           ops_total, write_mops, delete_mops, total_mops, live_keys, r, h, freename, query_usable, usable_bytes);
    // Nothing is freed on exit, as in a server that is shut down.
    return live_keys <= n ? 0 : 1;
}
