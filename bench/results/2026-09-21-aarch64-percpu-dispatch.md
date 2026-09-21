# Per-CPU dispatch at zero cost on the default path, aarch64

Lane percpu-dispatch, 2026-09-21, VM `challoc-percpu-dispatch-arm64`
(c4a-standard-16, Google Axion / Neoverse V2, kernel 7.0.0-1011-gcp, glibc
2.44, `rseq=yes rseq_size=33 mm_cid=yes`, 16 possible CPUs). `old` is
`main` 2078f67 (the tail-call dispatch of 4864d4b), `new` the lane's tree
(da39e2e the shape, cc547ed the mode's frameless hit paths; docs dd7f9b5),
jemalloc Wolfi's 5.3.1. The x86_64 file beside this one has the same
sections for the C4: `2026-09-21-x86_64-percpu-dispatch.md`.

## 0. The question and the answer

Commit 4864d4b (Phase 2.5 task 8) made the entry points test a
`PUSH_PERCPU` bit of the flag byte and tail-call a per-CPU wrapper when it
is set. The bit is never set under the shipped default
(`percpu_arena:auto` = thread caches), yet the dispatch cost the default
path +9.1 instructions and +1.8% cycles per larson1 operation on the C4A
(`bench/results/2026-09-20-aarch64-larson1-drift.md`: 255.4 -> 264.5
instructions, 70.9 -> 72.2 cycles; three of the nine on the hit paths,
`tbnz` in `malloc`, `mov` + `tbnz` in `free`, the rest in the same
commit's amortised paths), the attributed cause of the aarch64 larson1
scorecard band moving from +6.1% to +7.5..8.5% behind jemalloc.

The shape landed here is (a) of the brief: the mode rides a branch the
fast paths already take. A heap in the mode (`Heap::percpu`) never
installs a home cache (`Heap::ensure_cache` refuses its home arena), so
every home header stays `CacheHeader::EMPTY`, `len == cap == 0`;
`malloc`'s inlined pop falls through on `len == 0`, `free`'s and the
sized free's inlined push on `len == cap`, and each takes the cold tail
call it has anyway (`malloc_slow`, `free_slow`, `free_sized_slow`), where
`Heap::alloc`, `Heap::alloc_zeroed` and `Heap::free` test the heap's
`percpu` byte and go to `Heap::percpu_alloc` / `Heap::percpu_free` (the
pop or push section on the running CPU's header, out of line, hit path
frameless, misses as cold tail calls). `PUSH_PERCPU` is gone from the
flag byte, the wrappers (`percpu_malloc`, `percpu_free`,
`percpu_free_sized`, their `_miss` twins, `Heap::pop_percpu_mode`,
`percpu_pop_raw`, `percpu_push_raw`, `percpu_push_miss`) are gone, and
`malloc`, `malloc_fast`, `free`, `free_sized_hint`, `Heap::alloc` and
`Heap::alloc_zeroed` are their pre-4864d4b bodies again. The rseq
critical-section properties of design 15 are untouched: the sections are
the same `asm!` blocks (`rseq::pop_raw`, `push_raw`), `mm_cid` is still
read inside them, the commit store is the same, the abort handler the
same; only who calls them moved from an entry-point wrapper to a
slow-path function.

Rejected, and why:

- (b) GNU ifunc resolvers for the dispatching exports: not attempted. The
  default path's cost is already zero under (a) (the codegen list below is
  the pre-4864d4b entry points byte for byte on both architectures), and
  (b)'s proof burden (LD_PRELOAD, DT_NEEDED, `dlopen`, `RTLD_NEXT`,
  seccomp, `glibc.pthread.rseq=0`, the compat soname, the loader's own
  `malloc` before IRELATIVE, both archs) buys nothing over (a) for the
  default path. What (b) would buy is the *mode's* cost (below): a
  resolver could hand the mode its own entry points and the tail-call
  wrapper's +33..47 instructions instead of the slow-path prologue this
  shape charges it. Recorded as the option if the mode ever needs to be
  cheap; it also needs the configuration (`CHALLOC_CONF`, `MALLOC_CONF`,
  `/etc/challoc.conf`, the rseq probe's weak glibc symbols) readable
  before relocation, which the probe's GOT loads are not.
- (c) a cargo feature: not needed. (a) keeps `percpu_arena:percpu`
  selectable at run time in the same binary, which is design 17's rule
  (a default-on probed feature is within noise when absent), so nothing is
  compiled out and the packaging does not change.
- A TLS-slot sentinel for heaps in the mode (`existing_heap` returning
  `None` on a `TLS_PERCPU` value, the drift file's "mode-specific heap
  pointer" option): the same zero cost on the default path as (a) (the
  compare against the sentinel already exists, only its immediate
  changes), but it moves the thread heap out of the slot the mode still
  needs for its byte counters and its rseq area, so it would need a second
  TLS slot and a second accessor; (a) needs neither. Not built.

The mode's own cost under this shape (the mode is opt-in, measured behind
thread caches on every row before this change, and remains so): what the
mode now pays per operation is the entry point's wasted inlined fast path
(the `len` load and test on `malloc`, the whole slot check on `free`),
the slow path's prologue (the init-state load, the second TLS read,
`class_of` again or the exact `classify_free` in place of the inlined
check) and the call into `percpu_alloc`/`percpu_free`, before the same
section as before. The numbers are in section 2 (`new:percpu_arena:percpu`
against `old:percpu_arena:percpu`).

## 1. Codegen (`scripts/codegen-diff.sh capture` at 2078f67, `compare` at the tree, q16, aarch64)

`.text` 3500e4f295997c51 -> a new hash (differs, as intended); functions
541 -> 535 (the five wrappers gone, `percpu_alloc`/`percpu_free` and
`percpu_alloc_miss` new); exact 60, reloc-only 437, changed 38. The entry
points and the paths behind them, instructions per function (objdump;
the aliases `free`/`je_free`/`operator delete` share `_ZdaPv`'s code,
`operator new[]` is `_Znam`, `malloc`/`je_malloc` share `je_malloc`'s):

| function | old (2078f67) | new (tree) | drift file's pre-4864d4b count | note |
|---|---:|---:|---:|---|
| `malloc` (`je_malloc`) | 70 | 65 | 65 | the `tbnz` on the flags byte and the tail call gone; the pre-4864d4b body |
| `free` (`_ZdaPv`) | 149 | 144 | 144 | the `mov x1, x0` and the `tbnz` gone |
| `sdallocx` (`je_sdallocx`) | 192 | 180 | 180 | the `tbnz` and the argument shuffle gone |
| `free_sized` (`_ZdaPvm`) | 151 | 142 | 142 | as `sdallocx` |
| `calloc` | 222 | 132 | 135 | `Heap::alloc_zeroed` without the inlined per-CPU arm |
| `operator new[]` (`_Znam`) | 153 | 66 | - | `malloc_fast` without the inlined per-CPU arm |
| `malloc_usable_size` | 83 | 83 | 83 | unchanged |
| `malloc_slow` | 189 | 123 | - | `Heap::alloc` inlined: the `percpu` byte test and a call replace the inlined section |
| `free_slow` | 208 | 207 | - | `Heap::free` inlined: the `percpu` byte test and a tail call to `percpu_free` |
| `free_sized_slow` | 207 | 206 | - | as `free_slow` |
| `percpu_malloc` / `percpu_free` / `percpu_free_sized` (the wrappers) | 66 / 106 / 89 | gone | - | |
| `Heap::percpu_alloc` / `Heap::percpu_free` (the mode's paths, new) | - | 65 / 137 | - | hit path frameless, misses cold tail calls |

`alloc_fast`, `pop_fast` and `push_fast` are inlined into the above (no
symbols); their bodies are the pre-4864d4b ones (the `PUSH_PERCPU` arm
removed from `Heap::alloc` and `Heap::alloc_zeroed`, nothing else).
The rest of the changed list is the per-CPU slow paths (`percpu_run_pop`,
`percpu_free_slow`, `percpu_scheduled`, `percpu_alloc_slow`,
`percpu_ensure`, `percpu_pop_plain`: the `Shared::percpu_on` read moved
from the flag byte to `Percpu::state`), `ensure_cache` (+1 test),
`alloc_aligned` 294 -> 211, `jemalloc::alloc_with` 526 -> 399, `realloc`
588 -> 566 and `free_aligned_sized` 185 -> 177 (each inlined `Heap::alloc`
or `Heap::alloc_zeroed` and lost the per-CPU arm), `cxx::oom`, `init`
(6807 -> 6798, `PUSH_PERCPU` no longer written), `create_heap_for_thread`,
`set_arena`/`set_tcache_enabled` (`percpu_for` reads `Percpu::state`),
`STATS_PERCPU` and `background::start`. Nothing on the thread paths
changed but the removal.

## 2. Per-operation counters (`challoc-bench quick-perf arm64 OLD=2078f67`, perf on the VM host)

Two runs: the shape alone (dd7f9b5) and the final tree (cc547ed, the
mode's hit paths frameless). `old` and `new` are the default
(`percpu_arena:auto`, thread caches); `old:percpu` / `new:percpu` carry
`MALLOC_CONF=percpu_arena:percpu`.

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old (2078f67) | 41.55 / 41.40 | 72.1 / 72.3 | 264.5 / 264.5 | 2.103 / 2.101 | 4.007 / 4.075 |
| larson1 | new (dd7f9b5 / cc547ed) | 42.03 / 41.83 | 71.2 / 71.6 | 255.5 / 255.5 | 2.096 / 2.102 | 4.012 / 4.006 |
| larson1 | jemalloc | 44.78 / 44.57 | 66.5 / 66.8 | 224.8 / 224.8 | 2.069 / 2.073 | 1.729 / 1.740 |
| larson1 | new:percpu_arena:percpu | 24.46 / 24.90 | 121.7 / 119.5 | 449.0 / 438.0 | 2.224 / 2.216 | 3.820 / 3.816 |
| larson1 | old:percpu_arena:percpu | 39.48 / 39.35 | 75.3 / 75.5 | 311.2 / 311.2 | 2.108 / 2.136 | 2.802 / 2.870 |
| valkey1 | old | 13.15 / 13.09 | 230.4 / 231.5 | 245.2 / 245.2 | 3.085 / 3.083 | 2.807 / 2.744 |
| valkey1 | new | 12.80 / 13.28 | 236.7 / 228.2 | 244.3 / 241.0 | 3.099 / 3.083 | 2.786 / 2.772 |
| valkey1 | jemalloc | 13.82 / 13.86 | 218.9 / 218.1 | 170.9 / 171.2 | 2.971 / 2.999 | 2.690 / 2.723 |
| valkey1 | new:percpu_arena:percpu | 9.53 / 9.62 | 318.8 / 315.8 | 523.4 / 511.0 | 4.395 / 4.395 | 4.549 / 4.558 |
| valkey1 | old:percpu_arena:percpu | 11.35 / 11.17 | 267.4 / 271.9 | 324.9 / 326.5 | 3.260 / 3.254 | 2.874 / 2.879 |
| valkey1-free | old | 13.13 / 12.87 | 231.2 / 235.7 | 247.3 / 249.1 | 4.070 / 4.078 | 3.912 / 3.867 |
| valkey1-free | new | 12.99 / 12.77 | 233.6 / 237.4 | 244.4 / 244.2 | 4.072 / 4.072 | 3.888 / 3.871 |
| valkey1-free | jemalloc | 13.32 / 13.60 | 227.2 / 222.5 | 173.9 / 173.7 | 3.432 / 3.434 | 3.359 / 3.354 |
| valkey1-free | new:percpu_arena:percpu | 9.79 / 9.79 | 310.4 / 310.4 | 472.6 / 462.2 | 4.388 / 4.401 | 4.580 / 4.640 |
| valkey1-free | old:percpu_arena:percpu | 10.87 / 11.11 | 279.3 / 273.4 | 323.5 / 322.0 | 4.253 / 4.254 | 3.999 / 3.987 |

Reading, the default path: larson1 264.5 -> 255.5 instructions per
operation, exactly the -9.0 the drift file attributed to 4864d4b (its
pre-4864d4b figure is 255.4), cycles 72.1..72.3 -> 71.2..71.6 against the
drift file's pre-4864d4b band 70.9..71.2 and its post-4864d4b 72.2..72.8:
back in the band. valkey1 -1..-4 instructions, cycles within the row's
run-to-run spread (the two runs straddle: +2.7% then -1.4%); valkey1-free
-3..-5 instructions, cycles within spread. Misses unchanged to the second
digit: an instruction-stream change, as the drift was.

Reading, the mode (opt-in): larson1 311 -> 438 instructions and 75.5 ->
119.5 cycles per operation (+58%), valkey1 326 -> 511 and 272 -> 316
(+16%), valkey1-free 322 -> 462 and 273 -> 310 (+14%). This is the price
the shape charges the mode, by construction (section 0): `malloc` in the
mode is the entry point's fast path up to the `len == 0` test (wasted:
the home header is `EMPTY`), the `malloc_slow` prologue (the init-state
acquire load, `class_of` again, the second TLS read and PAC test, the
`percpu` byte) and the call into `percpu_alloc`; `free` is the wasted
inlined slot check, `free_slow`'s prologue, and `percpu_free`'s exact
`classify_free` (the bit test with the aborts by name, ~35 instructions)
before the same `push` section as before. The frameless hit paths
(cc547ed) took 11 instructions back on larson1 (449 -> 438) and 12 on
valkey1 (523 -> 511); the rest is the duplicated check and the prologue,
which only a slow path that receives the class and heap from the entry
point could remove, and that costs the default path a register move it
does not pay today (not done; see the notes in PROGRESS.d).

## 3. Fast tier (`challoc-bench quick arm64 OLD=2078f67`)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.0 (15.6 MiB) | 42.2 (16.0 MiB) | 45.1 (19.4 MiB) | -0.5% | +6.9% |
| larson10 | M ops/s | 362.1 (112.1 MiB) | 366.4 (112.1 MiB) | 389.4 (132.4 MiB) | -1.2% | +6.3% |
| xmalloc | M frees/s | 576.627 (53.3 MiB) | 579.736 (54.8 MiB) | 478.078 (78.4 MiB) | -0.5% | -17.5% |
| alloc-test1 | s | 2.676 (13.1 MiB) | 2.632 (13.1 MiB) | 2.514 (12.3 MiB) | -1.6% | +4.7% |
| valkey1 | M ops/s | 7.183 (584.3 MiB) | 7.166 (584.3 MiB) | 7.124 (585.5 MiB) | +0.2% | -0.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24039700 ops/s |

`quick: go` (new within 5% of old on every row; churn compacts). Tree cc547ed, one round, 2026-09-21T04:23Z, load 0.51.

## 4. Protocol tier (`challoc-bench ab arm64`, 5 rounds, same run, `2026-09-21-aarch64-percpu-dispatch-ab.md`)

AB_PLACEHOLDER

## 5. Tests

`make test` on the VM at dd7f9b5 and again at cc547ed: 481 + 138 + 26
passed, 0 failed (unit, `api::` with the C tests, cgroup limit), the
cgroup churn (`purge 400 100`, `keep 400 100`, `purge 200 50`, `keep 200
50`), lazy (`lazy 200`, `eager 200`) and pagemap-guard steps `ok`. The
per-CPU set inside it with the mode on: `c_percpu_both_ways` (`percpu:
mode percpu enabled=1 ids=16 headers=261 cached=64079 aborts=1 after 64
threads x 20000 ops`, the `off` way, and the `pac_pointers:true` way),
`c_tls_threads_with_pac_pointers` under `percpu_arena:percpu`,
`heap::linux_tests::percpu_*` (8 tests: the mode follows the probe and
the key with no bit in the flag byte, plain allocations through the CPU
caches with `cache(c).is_none()` on the thread heap, the flush's high
half, the run, slot conservation under migration, aborted sections,
heaps off the mode), `rseq::tests`, the `model::percpu_*` host tests
(`cargo test --features std` on the Mac: 375 passed). `bench/percpu-campaign.sh`
is unchanged and its `percpu` half still puts challoc under
`CHALLOC_CONF=percpu_arena:percpu` through the Makefile.

A note on method: the first `make test` and `codegen-diff compare` on the
VMs ran against the *baseline* `.so` (`.text` IDENTICAL): the sources had
been edited before the baseline capture built on the VM, so their synced
mtimes predated cargo's fingerprint and `cargo build --release` called
them fresh. Touching the changed files before the sync fixed it; both
numbers above are from the rebuilt library (the hashes differ, the
`quick`/`ab` builds use fresh target dirs and were never affected).
