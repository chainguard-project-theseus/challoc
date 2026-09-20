# aarch64 larson1 scorecard drift, attributed (coordinator, 2026-09-20)

The question (`.ralph/handoff-2026-09-19.md`, "WOUND DOWN"): the aarch64
scorecard's larson1 row read +7.6% at 34af56c and +8.4% at 856f103
against a two-day band of +5.9..+6.6% (0f5fea8: +6.1%); larson10 (+7.3%)
and alloc-test1 (+7.5%) sat at the top of their bands too; x86_64 larson1
was unaffected (-1.8%). Candidates named there: task 8's `PUSH_PERCPU`
test in the entry points, lane C's `lock.rs` slice hooks on the refill and
flush path, lane B's `MADV_NOHUGEPAGE` on reservations.

Host: the reference VM `challoc-bench-arm64` (C4A c4a-standard-16, Google
Axion / Neoverse V2, kernel 7.0.0-1011-gcp), perf on the VM host, the rows
in the dev container, `old` = 0f5fea8, `new` = the tree at 856f103 (the
library code of `main` 806f158). No other job on the VM, load under 0.5.

## 1. Per-operation counters (`challoc-bench quick-perf arm64 OLD=0f5fea8`)

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old (0f5fea8) | 42.19 | 71.0 | 255.4 | 2.100 | 3.986 |
| larson1 | new (856f103) | 41.50 | 72.2 | 264.5 | 2.108 | 3.989 |
| larson1 | jemalloc | 44.71 | 66.6 | 224.8 | 2.073 | 1.729 |
| larson10 | old | 367.48 | 81.3 | 255.4 | 2.249 | 4.209 |
| larson10 | new | 358.18 | 83.5 | 264.5 | 2.258 | 4.199 |
| larson10 | jemalloc | 380.19 | 78.4 | 224.8 | 2.209 | 1.854 |
| alloc-test1 | old | - | 7.93 G | 14.95 G | 89.4 M | 126.0 M |
| alloc-test1 | new | - | 7.98 G | 15.40 G | 89.4 M | 127.9 M |
| alloc-test1 | jemalloc | - | 7.49 G | 13.32 G | 76.0 M | 96.9 M |

Reading: +9.1 instructions and +1.2 (larson1) / +2.2 (larson10) cycles
per operation, L1D and dTLB misses unchanged to the third digit. The
drift is an instruction-stream cost on the thread paths, not data
placement (rules out the reservation-advice candidate: `MADV_NOHUGEPAGE`
is also only issued under `thp:never`, not the default) and not misses.
alloc-test1: +3.0% instructions, +0.6% cycles, misses flat.

## 2. Bisect by instruction count (one build per commit, larson1, two runs each)

Every `src/` commit in `0f5fea8..856f103`, in `main`'s order, built as
`NEW=<commit>` against `OLD=0f5fea8` and run under `quick-perf.sh`
(`ROWS=larson1 VARIANTS=new`); raw files in the VM's
`~/logs/drift-bisect/`.

| commit | subject | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| 0f5fea8 | config: KEYS is 25 after the lane merge | 42.21 / 42.06 | 70.9 / 71.2 | 255.5 / 255.4 | 2.098 / 2.096 | 4.008 / 4.095 |
| baa8bc2 | task 8 (lane C): percpu_arena config key | 42.27 / 42.31 | 70.9 / 70.8 | 255.4 / 255.4 | 2.092 / 2.098 | 3.984 / 4.008 |
| 14c7fad | task 8: per-CPU caches through rseq mm_cid | 36.76 / 36.84 | 80.7 / 80.7 | 337.2 / 337.2 | 2.121 / 2.113 | 2.948 / 2.954 |
| **4864d4b** | **task 8: per-CPU dispatch as tail calls, auto keeps thread caches** | 41.48 / 41.44 | 72.2 / 72.3 | **264.4 / 264.5** | 2.101 / 2.113 | 4.012 / 4.013 |
| 94fa9aa | task 8: aborted-section counter, the two mm_cid implementations | 41.28 / 41.24 | 72.5 / 72.6 | 264.5 / 264.5 | 2.097 / 2.106 | 4.004 / 4.028 |
| ae7ca63 | task 8 (lane B): rseq / mm_cid line in the feature report | 41.40 / 41.25 | 72.3 / 72.6 | 264.5 / 264.5 | 2.109 / 2.103 | 4.007 / 3.991 |
| ac6fdcd | task 12 (lane B): CLDEMOTE on the release path, dropped | 41.41 / 41.45 | 72.3 / 72.3 | 264.5 / 264.5 | 2.100 / 2.100 | 4.067 / 4.071 |
| 7ff53e5 | task 10 (lane B): the excess over the cap returned with its advice | 41.13 / 41.30 | 72.8 / 72.5 | 264.5 / 264.5 | 2.105 / 2.102 | 4.001 / 4.017 |
| a305a3e | task 9 (lane B): thp:always commits PMD chunks at a 2 MiB PMD only | 41.38 / 41.50 | 72.4 / 72.2 | 264.5 / 264.5 | 2.100 / 2.105 | 4.072 / 3.973 |
| cdca1f1 | task 16 (lane B): thp:never issues MADV_NOHUGEPAGE | 41.26 / 41.27 | 72.6 / 72.6 | 264.5 / 264.5 | 2.102 / 2.099 | 4.035 / 4.086 |
| b43532f | task 7: heap tests follow the PAC probe (Altra SIGILL) | 41.16 / 41.34 | 72.8 / 72.4 | 264.5 / 264.5 | 2.100 / 2.096 | 4.000 / 3.985 |
| ee0ecd8 | task 8: the per-CPU section inlined into a frameless wrapper | 41.66 / 41.43 | 71.9 / 72.3 | 264.5 / 264.5 | 2.104 / 2.106 | 4.081 / 4.112 |
| 01439ab | task 8: the run as a per-CPU header of its own | 41.65 / 41.52 | 71.9 / 72.1 | 264.5 / 264.5 | 2.111 / 2.118 | 4.002 / 3.990 |
| cd78247 | task 8 (lane C): model the per-CPU claim protocol | 41.46 / 41.54 | 72.2 / 72.1 | 264.5 / 264.5 | 2.107 / 2.129 | 3.998 / 3.989 |
| 80f52b4 | task 15 (lane C): rseq time slice extension for lock holders | 41.52 / 41.52 | 72.1 / 72.1 | 264.5 / 264.5 | 2.104 / 2.109 | 3.998 / 4.007 |
| 856f103 | PROGRESS: fold lane lane-e (the tree the scorecard measured) | 41.55 / 41.49 | 72.1 / 72.2 | 264.5 / 264.5 | 2.109 / 2.108 | 3.976 / 3.997 |

Reading:

- The whole step is one commit, 4864d4b (lane A, task 8): 255.4 -> 264.5
  instructions per operation, 70.9 -> 72.2 cycles (+1.8%), on the
  shipped default (`percpu_arena:auto` = thread caches). Its predecessor
  14c7fad had the mode on by default (337.2 instructions, 80.7 cycles:
  the +82 the task 8 file attributes to the section); 4864d4b turned that
  into a dispatch and left +9.1 behind.
- Every commit after it, from 94fa9aa to 856f103, has the identical
  instruction stream on this row (264.5 to the first decimal, 22 runs)
  and cycles within 71.9..72.8. So lane C's slice hooks (80f52b4), lane
  B's four commits and lane A's two later mechanisms (ee0ecd8, 01439ab)
  cost nothing on the thread paths, as each claimed.
- The scorecard movement from 34af56c (+7.6%) to 856f103 (+8.4%) is
  therefore noise of the 3-round scorecard, not code: the two trees have
  the same instructions and the same cycles per operation here. The
  step from the +6% band to +7.6% is 4864d4b's +1.8% of cycles plus the
  row's usual spread.

## 3. Where the 9 instructions are

Static: `objdump -d` of the two libraries, the exported entry points by
symbol address (aliases share the code: larson's `operator new[]` and
`delete[]` are `malloc` and `free`).

| function | old | new | on the hit path |
|---|---:|---:|---|
| `malloc` | 65 | 70 | +1: `tbnz w2, #3, percpu_malloc` on the flags byte already loaded for the PAC bit |
| `free` | 144 | 149 | +2: `mov x1, x0` (the pointer moves so the heap pointer can travel in `x0` to the tail call) and `tbnz w2, #3, percpu_free` |
| `sdallocx` | 180 | 192 | +1 `tbnz`, argument shuffles into the tail call (not larson's path) |
| `free_sized` | 142 | 151 | as `sdallocx` |
| `calloc` | 135 | 222 | the `alloc_zeroed` dispatch (not larson's path) |
| `malloc_usable_size` | 83 | 83 | unchanged |

Off the hit path in the same commit: two extra `mov`s on the
`pop_compact` call (one pop in 8), a `mov x0, x1` before the flush call,
`Heap::alloc_slow`'s `self.percpu` test per refill, and `claim_with`
growing 2516 -> 2997 instructions of text (the per-CPU claim variants
share its body). Dynamic: `perf record -e instructions:u` at 4 kHz on the
VM host puts the two entry points at 95.7 -> 99.2 (`free`) and 40.8 ->
42.6 (`malloc`) instructions per operation, the small functions
(`claim_with`, `release_owned`, `group_by_slab`) flat at 0.3..0.7 each,
and 3.8 of the 9 attributed to larson's own `exercise_heap`, whose code
did not change: on this PMU the sampled IP of an instruction-count event
lands on the instruction after a `ret` (17% of `malloc`'s samples sit on
the never-executed `mov x0, x8` that follows its `ret`), so the
per-symbol split is smeared across call boundaries by a few instructions
and the per-instruction histogram is unusable for counting. The honest
statement is: 3 of the 9 are the dispatch instructions on the hit paths,
the other 6 are in the amortised refill and flush paths of the same
commit, and this PMU cannot split them finer than that. Lane A's own
measurement at the time (`2026-09-19-aarch64-task8-percpu.md`, section
3: `new:percpu_arena:off` 264.5 against `old` 255.4 instructions, "+9
instructions and +1.3 cycles per larson operation") is the same number;
what was missing was the link from that row to the scorecard band, which
this file makes.

## 4. Consequences

- Nothing to fix for acceptance: the default-on change of task 8 (the
  dispatch) costs +1.8% of cycles on larson1 on the C4A, inside the
  "within noise" reading of the 3-round scorecard on its own but visible
  as a band shift over 20 lines; the acceptance file's row 2b ("no
  default-on feature moves a scorecard row outside noise") holds with
  this as its recorded cost. x86_64 pays the same instructions and reads
  -1.8% there (a wider front end).
- If lane A wants the 1.8% back: the dispatch bit lives in the flags byte
  the PAC test already loads, so the `tbnz` is the floor of this design;
  the alternative is a mode-specific heap pointer (a per-CPU process
  publishes no thread heap through the TLS slot, so `existing_heap`'s
  existing null check routes the mode without a second test), which
  moves the cost to the mode's entry. Not asked for by any acceptance
  row; recorded as an option, not a task.
- The scorecard trend note: read larson1 aarch64 against a +7.5..+8.5%
  band from 4864d4b on; the band did not move again through 856f103.

Raw material on the VM (not synced back): `~/logs/quick-perf/` (section
1), `~/logs/drift-bisect/` (section 2), `~/logs/drift-rec-instructions/`
and `~/logs/drift-rec-cycles/` (section 3's `perf record` data and
`perf report --sort dso,sym` tables), the container volume's
`target/drift/{old,new}.{dis,nm}` (the disassembly).
