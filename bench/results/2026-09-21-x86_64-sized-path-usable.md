# The size-returning entry points for Valkey (lane sized-path, task 1), x86_64

Date 2026-09-21. Lane `sized-path` (worktree `../challoc-wt/sized-path`,
branch `loop/sized-path` from `main` 2078f67), lane VM
`challoc-sized-path-amd64` (c4-standard-16, Xeon Platinum 8581C, Emerald
Rapids, `--performance-monitoring-unit=standard`, Ubuntu kernel
7.0.0-1011-gcp, perf 7.0.14 on the host; container Wolfi
`challoc-dev:amd64`). Code commit 63bd7ee. The aarch64 file
(`2026-09-21-aarch64-sized-path-usable.md`) is the guard.

## 1. What changed and why

The write-phase attribution
(`2026-09-20-x86_64-valkey-write-phase-attribution.md`, sections 6 and 8)
named one allocator-side term above jemalloc's cost in the Valkey soak:
`je_malloc_usable_size`, +117 cycles per valkey-benchmark SET and +63 per
memtier SET (346 against jemalloc's 229), the bitmap-word load of the
exact liveness test on a slot the sized free had not touched since its
refill, on top of a second classification of a pointer the allocator had
just classified. Valkey asks it after every `je_malloc`/`je_calloc`/
`je_realloc` (`ztry*_usable_internal`, for `used_memory`) and before every
unsized `zfree` (then `je_sdallocx`). The note proposed dropping the
liveness test; the decision was to keep every check and remove the second
lookup: four entry points that return the usable size from the
classification the allocation or the free already made (design 13.7,
`<challoc/challoc.h>`):

```c
void  *challoc_malloc_usable(size_t size, size_t *usable);
void  *challoc_calloc_usable(size_t n, size_t size, size_t *usable);
void  *challoc_realloc_usable(void *p, size_t size, size_t *usable);
size_t challoc_free_usable(void *p);   /* frees p, returns its usable size */
```

`challoc_malloc_usable` is `malloc`'s pop plus `CLASSES[c].size()` from
the class the pop used; `challoc_free_usable` is `free`'s exact check and
push (`heap::free_fast_class`: the bitmap-word test, the owner word with
several arenas) reporting the class it verified. The Valkey patch
(`contrib/valkey/0001-Add-MALLOC-challoc.patch`) puts `zmalloc.c`'s three
`ztry*_usable_internal` on the pair and the unsized `zfree` on
`challoc_free_usable`; `zmalloc_size` stays `je_malloc_usable_size` for
its other callers. Per Valkey SET under the old patch: two `je_malloc` +
two `je_malloc_usable_size` (after the allocations) + `je_malloc_usable_size`
+ `je_sdallocx` for each unsized free; under the new one: two
`challoc_malloc_usable` and one `challoc_free_usable` per unsized free.

## 2. Codegen accounting (`scripts/codegen-diff.sh`, capture at 2078f67, compare at 63bd7ee, Mac container, aarch64)

The rule for this task: the fast paths of every existing per-allocation
function stay byte-identical. Both variants (`q16`, `q8`): 541 -> 548
functions, the instruction total +1860 (`q16`: 63141 -> 65001), of which
the seven new functions are 1851 (`free_usable_slow` 314,
`malloc_usable_huge` 104, `malloc_usable_class` 180,
`challoc_calloc_usable` 299, `challoc_free_usable` 146,
`challoc_malloc_usable` 84, `challoc_realloc_usable` 724; the exports of
`malloc_usable` and `free_usable` are the whole inlined fast paths, the
other two are not fast paths). Per existing entry point (the per-function
verdict, `.tmp-codegen` report and a name-by-name check):

| function | verdict | instructions |
|---|---|---:|
| `malloc` / `je_malloc` | reloc-only (identical instructions, shifted immediates) | 70 -> 70 |
| `free` / `je_free` | reloc-only | 149 -> 149 |
| `sdallocx` / `je_sdallocx` | reloc-only | 192 -> 192 |
| `calloc` / `je_calloc` | reloc-only | 222 -> 222 |
| `realloc` / `je_realloc` | reloc-only | 1 -> 1 |
| `malloc_usable_size` / `je_malloc_usable_size` | reloc-only | 83 -> 83 |
| `free_sized`, `free_aligned_sized`, `dallocx`, `aligned_alloc` | reloc-only | unchanged |

"Reloc-only" is what adding code does to every function of a fat-LTO
library: `.rodata` and the GOT move, so every PC-relative immediate
shifts while the instruction text is identical (60 functions stayed
byte-exact, 476 reloc-only in `q16`; 81 and 457 in `q8`). The `changed`
list beyond the seven new functions holds no allocation function: `q16`
`malloc_stats_print` (an `adrp` the linker relaxed to `adr` plus a
veneer), `purge::background::main` (a prologue reordering in the
background thread's cold main loop), `core::slice::sort::...
panic_on_ord_violation` (two alignment nops), a `STATS_PREFAULT`
`call_once` and a `Display::fmt` (a merged-globals slot offset); `q8`
the last two plus `__aarch64_ldset8_acq` (alignment). None is on an
allocation path.

Static shape of the new entry points (aarch64, `objdump`): `challoc_malloc_usable`
84 instructions against `malloc`'s 70 (the class-size load and the store
through `usable`, the same `pop_compact` call and frame as `malloc`);
`challoc_free_usable` 146 against `free`'s 149 (the same `cached_twice`
call and frame; task 2 of this lane removes both calls).

## 3. Tests

- Valkey 9.1.2 (7f1dffed) with the new patch, `make valkey STAGE=all`
  (`contrib/valkey/build.sh`: the quantum8 library, the patched tree,
  `make MALLOC=challoc BUILD_TLS=yes`, the `malloc=jemalloc-5.3.0-challoc`
  and `DT_NEEDED libchalloc.so.0` checks, then `./runtest` through
  tclsh9.0): on this VM `memefficiency: PASS (436 s)` and the full suite
  `164/164 done`, `All tests passed without errors`, `full: PASS (956 s)`;
  on the C4A `memefficiency: PASS (421 s)`, `full: PASS (912 s)`. The
  build reported `stale patch in the tree, resetting it` / `patch
  applied` on both, since the same clone had carried the previous patch
  (kept as `target/valkey/src-9.1.2-oldpatch` for the soak's `challoc-old`
  server).
- `make test` on both lane VMs at 63bd7ee (the tree synced; the first run
  had failed one row, `c_usable_api`, on a `MALLOCX_ALIGN` the test
  header does not define and an n = 0 `realloc_usable` case, both fixed
  before the commit): x86_64 481 + 145 + 26 passed, aarch64 481 + 145 +
  26, the cgroup (`ok cgroup_churn` x6, `ok cgroup_lazy` x2, `ok
  cgroup_policy` x4) and `pagemap_guard` steps green, the host's usual
  skips (mseal under Docker's seccomp, the PSI trigger, thp auto, the
  rseq slice extension). New rows: `c_usable_api` (three
  configurations: default, `percpu_arena:percpu`, `zero_on_free,
  double_free_scan,pac_pointers`), `c_negative_free_usable_{interior,
  stale,cached,huge_twice}` (the cached mode twice: at the flush by
  default, at the call with `double_free_scan`), the four names in
  `jemalloc_symbols_in_release_so`; unit tests
  `usable_entry_points_agree_with_malloc_usable_size_on_every_tier` and
  `usable_entry_points_null_and_failure_cases` in `src/api/libc.rs`. Host
  `cargo test --features std` (pure logic) compiles and passes as before
  (the `api::` group is Linux-only).
- `scripts/check-needed.sh`: unchanged and still run by `build.sh`
  (`ok libchalloc.so.0` on both VMs' `valkey-server`).

## 4. `quick-perf`: the write phase per SET, old shape against the pair (C4)

`bench/gce/challoc-bench quick-perf amd64 OLD=HEAD~1 ROWS='valkey1-write
valkey1-write-zmalloc valkey1-write-pair valkey1-usable valkey1 larson1'
VARIANTS='old new jemalloc'`: `old` is 2078f67 (the tree before this
lane), `new` 63bd7ee, jemalloc 5.3.1 from the image; one run per cell,
`perf stat` over the whole process divided by the program's operation
count (`valkey1-write*` rows: per SET, two allocations and two frees over
a 500 MiB heap). The three write rows are the same program with three
allocator shapes: `valkey1-write` is the sized path alone (`malloc`,
`sdallocx`, no accounting query); `valkey1-write-zmalloc` (`-u -a`) is
Valkey's unsized `zmalloc` shape, `malloc_usable_size` after each
allocation and before each free (four queries per SET);
`valkey1-write-pair` (`-p`) is the pair, `challoc_malloc_usable` for each
allocation and `challoc_free_usable` for each free (the row exists only
under `new`: `old` and jemalloc do not export the pair, and the program
refuses to run, hence no line for them).

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| valkey1-write | old | 11.90 | 282.8 | 423.4 | 4.633 | 0.943 |
| valkey1-write | new | 12.14 | 277.4 | 423.2 | 4.632 | 0.950 |
| valkey1-write | jemalloc | 12.13 | 277.3 | 330.6 | 4.870 | 1.034 |
| valkey1-write-zmalloc | old | 8.38 | 402.3 | 709.0 | 7.271 | 0.990 |
| valkey1-write-zmalloc | new | 8.27 | 407.7 | 709.2 | 7.220 | 0.980 |
| valkey1-write-zmalloc | jemalloc | 4.83 | 698.6 | 775.9 | 11.868 | 3.275 |
| valkey1-write-pair | new | 9.33 | 361.2 | 442.9 | 6.863 | 0.976 |
| valkey1-usable | old | 16.78 | 200.7 | 351.8 | 4.491 | 0.915 |
| valkey1-usable | new | 16.45 | 204.9 | 353.6 | 4.525 | 0.927 |
| valkey1-usable | jemalloc | 14.29 | 235.6 | 304.5 | 6.580 | 2.022 |
| valkey1 | old | 20.33 | 165.6 | 273.7 | 3.260 | 0.897 |
| valkey1 | new | 20.15 | 167.1 | 273.9 | 3.299 | 0.906 |
| valkey1 | jemalloc | 22.78 | 147.7 | 184.8 | 3.347 | 0.954 |
| larson1 | old | 42.27 | 77.2 | 244.2 | 1.647 | 0.002 |
| larson1 | new | 44.36 | 73.1 | 244.3 | 1.626 | 0.002 |
| larson1 | jemalloc | 44.48 | 72.4 | 200.9 | 2.129 | 0.001 |

Reading, per SET on the C4: the old shape costs 407.7 cycles, 709
instructions and 7.22 L1D misses; the pair 361.2, 443 and 6.86 (**-46
cycles, -11.4%; -266 instructions; -0.36 L1D misses**); the sized path
with no accounting at all 277.4, 423 and 4.63. So the pair removes the
second classification and its class-table work entirely (the pair is 20
instructions per SET above the no-query path, the old shape 286), and
about a third of the query's cycles; what remains, +84 cycles over the
bare sized path, is the exact liveness test that `challoc_free_usable`
keeps by decision (the bitmap word of a slot nothing touched since the
refill is now missed in the free instead of in the query: the +2.2 L1D
misses per SET for two frees, against +2.6 for the old shape's four
queries) plus the class-size stores. jemalloc under the same old shape
reads 698.6 cycles per SET (its `malloc_usable_size` is `sallocx`'s
rtree walk, 11.9 L1D misses), so on the model the old shape was already
in challoc's favour; the soak is what says how the terms land in Valkey,
where only the object's free is unsized (`zfree`) and the value's is
`zfree_with_size` (sized, no query), so the pair's free side is one
`challoc_free_usable` per SET, not two as here. `valkey1-usable` (the
existing row, one query before each free) and `valkey1`, `larson1` are
unchanged between `old` and `new` (the library's existing paths are
byte-identical, section 2; the differences are the run's band).

## 5. The soak: old patch, new patch and jemalloc in one run, with the perf sidecar (C4)

`make valkey-soak ALLOCS="challoc-old challoc jemalloc" DURATION=1200
OCCUPANCY=1` on `challoc-sized-path-amd64`, 04:31..05:36 UTC: three
servers in sequence, each the acceptance protocol (write phase 600 s
alternating valkey-benchmark `-t set -d 512 -r 500000 -n 20000000 -c 50
-P 16` and memtier SET 16..1024 B, 4 x 10 clients, `-P 16`, 30 s; delete
phase 600 s alternating valkey-benchmark `del` and memtier DEL:SET 4:1;
settle 60 s), `activedefrag yes`, `io-threads` default. `challoc-old` is
Valkey 9.1.2 built with the contrib patch of 2078f67
(`target/valkey/src-9.1.2-oldpatch`, kept from the warm-up build),
`challoc` the same tag with the new patch, both dynamically linked to
the one quantum8 `libchalloc.so` of 63bd7ee through their rpath (so the
only difference between the first two servers is `zmalloc.c`); `jemalloc`
the tag with its bundled jemalloc 5.3.0. The sidecar
(`bench/probes/valkey-perf-sidecar.sh`: `perf stat -I 1000` for each
server's life, 60 s `perf record -e cycles -F 2999` at t=100 (write) and
t=700 (delete), INFO snapshots around each window) ran as two instances:
the first with its then two-name loop, so its `challoc` files are the
old-patch server and its `jemalloc` files the new-patch server, the
second attached to the jemalloc server alone (the `ALLOCS` fix to the
sidecar, 9b1e26f, landed after the launch; the file names in
`bench/results/2026-09-21-x86_64-sized-path-usable-soak-*` carry the
server's real name). Summary (`-soak.md`), samples (`-soak-<server>.csv`),
`MEMORY MALLOC-STATS` every 20 s (`-soak-<server>-mstats/`), the per-second
counters (`-soak-perf-stat-<server>.csv`), the snapshots
(`-soak-snapshots-<server>.txt`) and the perf reports
(`-soak-perf-report.txt`) are beside this file.

### 5.1 The soak's own table (positive = worse than jemalloc)

| measure | challoc-old | challoc (new patch) | jemalloc | new vs jemalloc | old vs jemalloc | new vs old |
|---|---:|---:|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 750205 | 773389 | 708770 | -8.4% | -5.5% | **+3.1%** |
| write: valkey-benchmark p99 ms | 1.303 | 1.263 | 1.367 | -7.6% | -4.7% | -3.1% |
| write: memtier req/s | 925029 | 952707 | 917881 | -3.7% | -0.8% | **+3.0%** |
| write: memtier p99 ms | 1.031 | 0.991 | 1.007 | -1.6% | +2.4% | -3.9% |
| delete: valkey-benchmark req/s | 1293452 | 1339362 | 1365599 | +2.0% | +5.6% | **+3.5%** |
| delete: valkey-benchmark p99 ms | 0.783 | 0.751 | 0.719 | +4.5% | +8.9% | -4.1% |
| delete: memtier req/s | 1198709 | 1242060 | 1230747 | -0.9% | +2.7% | **+3.6%** |
| delete: memtier p99 ms | 0.815 | 0.791 | 0.783 | +1.0% | +4.1% | -2.9% |
| peak RSS MiB (VmHWM) | 577.3 | 579.6 | 573.0 | +1.2% | +0.8% | +0.4% |
| RSS MiB after settle | 130.9 | 130.3 | 130.1 | +0.2% | +0.6% | -0.5% |
| allocator_frag_ratio after settle | 1.79 | 1.79 | 1.78 | | | |
| active_defrag_hits | 247124 | 245604 | 269608 | | | |

The last column is what this task changed: the same library, the same
run, only `zmalloc.c` differs, and both phases and both generators move
by +3.0..3.6% in throughput with the p99 down 3..4%. The criteria of the
plan hold in both patches (write throughput and p99 ahead of jemalloc;
peak RSS +1.2%, RSS after settle +0.2%); the delete phase, which the
09-20 file left at +3.8..3.9% behind jemalloc, reads +2.0% (valkey-benchmark)
and -0.9% (memtier) with the new patch against +5.6% / +2.7% with the old
one in this run.

### 5.2 Per operation (`perf stat` seconds inside each generator run over its request rate; medians over the runs of a phase)

| phase | tool | server | runs | req/s | p99 ms | cyc/op | ins/op | IPC | L1D miss/op | dTLB miss/op | br-miss/op | GHz | ns/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write | valkey-benchmark | challoc-old | 11 | 750920 | 1.303 | 4357 | 8355 | 1.92 | 37.1 | 3.3 | 4.38 | 3.267 | 1335 |
| write | valkey-benchmark | challoc (new) | 11 | 773934 | 1.263 | **4229** | **7787** | 1.84 | 35.4 | 3.3 | 4.75 | 3.265 | 1294 |
| write | valkey-benchmark | jemalloc | 10 | 710967 | 1.359 | 4605 | 8208 | 1.79 | 36.6 | 3.6 | 3.28 | 3.272 | 1408 |
| write | memtier | challoc-old | 11 | 922940 | 1.031 | 3512 | 8302 | 2.37 | 37.2 | 2.4 | 5.97 | 3.241 | 1085 |
| write | memtier | challoc (new) | 11 | 955720 | 0.991 | **3392** | **7798** | 2.29 | 35.0 | 2.5 | 5.78 | 3.232 | 1048 |
| write | memtier | jemalloc | 10 | 916847 | 1.015 | 3545 | 8190 | 2.31 | 34.5 | 2.5 | 5.58 | 3.251 | 1090 |
| delete | valkey-benchmark | challoc-old | 13 | 1295085 | 0.783 | 2531 | 5976 | 2.36 | 13.1 | 0.2 | 5.15 | 3.268 | 774 |
| delete | valkey-benchmark | challoc (new) | 13 | 1342823 | 0.751 | **2443** | **5628** | 2.30 | 12.5 | 0.2 | 5.02 | 3.268 | 746 |
| delete | valkey-benchmark | jemalloc | 13 | 1373721 | 0.719 | 2386 | 5818 | 2.44 | 12.9 | 0.1 | 2.90 | 3.276 | 729 |
| delete | memtier | challoc-old | 13 | 1202407 | 0.815 | 2690 | 6402 | 2.38 | 19.1 | 0.5 | 4.61 | 3.233 | 830 |
| delete | memtier | challoc (new) | 13 | 1243926 | 0.791 | **2606** | **6015** | 2.31 | 18.0 | 0.4 | 4.67 | 3.238 | 803 |
| delete | memtier | jemalloc | 13 | 1232387 | 0.783 | 2638 | 6293 | 2.38 | 18.0 | 0.3 | 3.99 | 3.253 | 811 |

The record windows (counters between the two snapshots over the
`total_commands_processed` delta): write 3762 (old) / 3707 (new) / 3980
(jemalloc) cycles per command at 3.208 / 3.208 / 3.222 GHz; delete 2611
/ 2542 / 2580 at 3.175 / 3.210 / 3.226 GHz.

Reading. Per valkey-benchmark SET the new patch runs 128 fewer cycles
(-2.9%) and 568 fewer instructions (-6.8%) than the old one at the same
3.27 GHz; per memtier SET -120 cycles (-3.4%), -504 instructions; per
DEL -88 and -84 cycles (-3.5%, -3.1%), -348 and -387 instructions. The
L1D misses per SET fall by 1.7..2.2 (37.1 -> 35.4; 37.2 -> 35.0): the
old shape's four queries missed the bitmap word of a slot nothing had
touched since the refill; `challoc_free_usable` misses it once, in the
one unsized free per SET, and `challoc_malloc_usable` never loads it.
Against jemalloc the write phase is now -8.2% cycles per valkey-benchmark
SET (was -5.4%) and -4.3% per memtier SET (was -0.9%); the delete phase,
where the old patch was +6.1% and +2.0%, is +2.4% (valkey-benchmark) and
-1.2% (memtier). The clock is the same for the three servers to within
0.5% in every row (the three ran within 65 minutes of each other), so
the throughput rows above are these cycle rows.

### 5.3 Where the cycles went (perf record, 60 s windows; DSO shares times the window's cycles per command)

| record | server | valkey-server own | libchalloc.so | kernel | libc | allocator cycles/cmd | `je_malloc_usable_size` | `challoc_malloc_usable` | `challoc_free_usable` | `je_malloc` | `je_sdallocx` | `zmalloc_usable` (Valkey) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write | challoc-old | 50.52% | 12.00% | 30.47% | 7.01% | 451 (+46 PLT) | 7.60% = 286 | | | 1.98% = 74 | 2.33% = 88 | 4.97% = 187 |
| write | challoc (new) | 55.20% | 5.91% | 31.09% | 7.80% | **219** (+12 PLT) | 0.00% | 2.25% = 83 | 2.73% = 101 | | 0.83% = 31 | 7.68% = 285 |
| write | jemalloc | 60.43% (7.99% allocator) | | 28.04% | 11.52% | 318 | 4.83% = 192 | | | 1.74% = 69 | 1.43% = 57 | 0.68% = 27 |
| delete | challoc-old | 49.94% | 10.34% | 31.63% | 8.09% | 270 (+13 PLT) | 5.95% = 155 | | | 1.46% = 38 | 2.21% = 58 | 0.69% = 18 |
| delete | challoc (new) | 51.29% | 5.91% | 34.36% | 8.42% | **150** (+8 PLT) | 0.00% | 1.96% = 50 | 2.90% = 74 | | 0.15% = 4 | 0.74% = 19 |
| delete | jemalloc | 50.07% (5.36% allocator) | | 37.88% | 12.04% | 138 | 2.92% = 75 | | | 1.60% = 41 | 0.77% = 20 | 0.57% = 15 |

(jemalloc's allocator is the `je_*`/`tcache_*`/`arena_*`/`edata_*`
symbols inside its statically linked `valkey-server`.) The criterion of
the task: `je_malloc_usable_size` is 0.00% of the new patch's records
(286 cycles per command in the write window and 155 in the delete window
under the old patch, 192 and 75 under jemalloc), and the allocator's
cycles per command fall from 451 to 219 in the write window and from 270
to 150 in the delete window, below jemalloc's 318 and 138 in the first
and at parity in the second. The two new entry points cost 83 + 101
cycles per command in the write window (the free's 101 is `free`'s exact
check, the bitmap-word load the decision kept; `je_sdallocx` is left
with the `zfree_with_size` frees, 31). Part of the removed time
reappears in Valkey's own code: `zmalloc_usable` (the wrapper into which
`ztrymalloc_usable_internal` is inlined, with the `used_memory` atomic
add) rises from 4.97% to 7.68% (187 -> 285 cycles per command), and the
`valkey-server` DSO from 50.52% to 55.20%; the instruction after a call
returns is where the caller's first use of the result waits, and the
new patch's callers consume `*usable` right after the call where the
old ones consumed the query's result inside `je_malloc_usable_size`. Net
in the write window: -55 cycles per command (-1.5%), the per-SET rows
above -128 and -120; the kernel and libc shares are the same work
(`_copy_to_iter` 5.55% against 5.60%).

## 6. Conclusion

- The pair does what the design note asked for without dropping a
  check: in Valkey the accounting query is gone (0.00% of the write and
  delete records), the allocator's write-phase cycles per command halve
  (451 -> 219, below jemalloc's 318), and the server runs 3.0..3.6% more
  requests per second in both phases and with both generators against
  the old patch in the same run, p99 down 3..4%, RSS unchanged (peak
  +0.4%, settle -0.5%). Against jemalloc: write phase -8.4% / -3.7%
  (throughput, challoc ahead), delete phase +2.0% / -0.9%, where the old
  patch read +5.6% / +2.7%.
- The remaining allocator-side term in the delete phase is the exact
  liveness check `challoc_free_usable` keeps (74 cycles per command
  there, 101 in the write window; jemalloc's whole `sdallocx` is 20 and
  57): the bitmap word of a slot the sized path left cold. That is the
  decision's price; the sized alternative (a `challoc_sdallocx_usable`
  that trusts the size and defers the bit test to the flush like
  `sdallocx`) would remove it and is the next thing to measure if the
  delete phase's +2.0% on valkey-benchmark matters.
- The model rows (section 4) predicted the direction and the shape
  (about a third of the query's cycles saved with the check kept); the
  soak gains more than the model because Valkey's free side is one
  unsized free per SET, not two, and because the model's fixed 850k-key
  heap keeps every query an L2 hit where Valkey's also misses the
  class-table line.
