# Phase 2.5 task 8, the fast-path follow-up: per-CPU mode cost, aarch64

Lane A, 2026-09-19/20, reference VM challoc-bench-arm64 (C4A, Google Axion,
Neoverse V2, 16 vCPUs, kernel 7.0.0-1011-gcp, glibc 2.44, `__rseq_size` 33,
`nr_cpu_ids` 16). `old` is dfe3f60 (main after lane B's merge; the per-CPU
code there is byte for byte task 8's landed form, 94fa9aa), `new` the tree
after each mechanism (commits in PROGRESS.md). The mode is
`MALLOC_CONF=percpu_arena:percpu` on either library; the default (`auto` =
thread caches) is the library as shipped. Method: bench/README.md
(`challoc-bench quick-perf` for the per-operation attribution, `make quick
OLD=dfe3f60 NEW_CONF=percpu_arena:percpu` for the mode against the base's
default and `make quick OLD=dfe3f60` for the default against itself, then
`make ab` 5 rounds).

The predecessor's attribution (`2026-09-19-aarch64-task8-percpu.md`, section
3): the mode is instructions, not misses (+83 instructions and +9.7 cycles
per larson operation with L1D misses flat; +183 / +68 on the Valkey model).
Two mechanisms named there and attacked here:

- (a) the section sat in a function called from the wrapper the entry
  point tail-calls, which returned an enum the wrapper decoded: two frames,
  the decode, a `CLASSES` bounds check and its panic call per operation;
- (b) in compact mode one pop in 8 is a scheduled pop, and under the mode
  it claimed one slot under the class lock (the thread run amortises one
  lock over `cap / 4`).

## 1. Mechanism (a): the section inlined into a frameless tail-call wrapper

The wrapper (`api::libc::percpu_malloc`, `percpu_free`, `percpu_free_sized`)
now inlines the hit path (`Heap::percpu_pop_raw`, `heap::percpu_push_raw`:
the heap's `percpu` byte, the slot check for a free, the section, the byte
count) and tests one bit of the section's raw status word
(`rseq::pop_raw`/`push_raw`); every miss (a heap off the mode, empty, full,
declined, scheduled, out of range) is one cold tail call that finishes the
operation (`percpu_*_miss`). The x86_64 pop compares `len` with `zeroed`
inside the asm (one register fewer). `malloc`/`free` themselves are
unchanged: the `PUSH_PERCPU` test and the tail call were already there.

Disassembly (aarch64, the Mac dev container, `objdump -d` of the release
library; the exported `malloc` is the `je_malloc` symbol):

| function | old (dfe3f60) | new | note |
|---|---:|---:|---|
| `malloc` (thread path, the small-size hit) | 36 instructions | 36 | identical stream; the dispatch is `tbnz w2, #3` + `b percpu_malloc` |
| `percpu_malloc` | 15 (+ `pop_percpu_mode` 120, called) | 66, frameless | hit path 48 instructions, the rest the status exits |
| `percpu_free` | 15 (+ `percpu_free_fast` 314, called) | 106, frameless | the `checked_slot_any` bit test inline (as the thread `free` has `checked_slot`) |
| `percpu_free_sized` | 24 (+ `percpu_free_fast`) | 89, frameless | |

The old wrappers opened a frame (`stp x29, x30, [sp, #-32]!`, `str x19`)
for the call and decoded `Option<(*mut u8, bool)>` on return; the new ones
have no frame (no `bl` remains on any path: the misses are `b` to a cold
function). What is left over the thread pop, per side: the descriptor
store (`adrp`, `add`, `str` to `rseq_cs`), the `mm_cid` load and its range
check (3), the table index (`madd`), the loads of `shared`, `base` and `n`
(3), the status encode and its one-bit test (about 5), the byte count (7:
the `CLASSES` size and the heap's counter). The mode's `malloc` is 18
(dispatch) + 2 + 48 = 68 instructions against the thread path's 36.

Per-operation attribution (`challoc-bench quick-perf arm64 OLD=dfe3f60`,
perf on the VM host, one run each; the mode is `percpu_arena:percpu` on the
named library, `new` alone is the shipped default):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old, mode | 36.85 | 80.7 | 338.2 | 2.132 | 3.033 |
| larson1 | new (a), mode | 38.99 | 76.2 | 311.2 | 2.118 | 2.946 |
| larson1 | old, default | 41.34 | 72.4 | 264.5 | 2.097 | 4.019 |
| larson1 | new (a), default | 41.54 | 72.1 | 264.5 | 2.103 | 4.013 |
| larson1 | jemalloc | 44.85 | 66.4 | 224.8 | 2.066 | 1.736 |
| valkey1 | old, mode | 9.89 | 306.9 | 424.3 | 3.342 | 3.127 |
| valkey1 | new (a), mode | 10.12 | 299.7 | 372.5 | 3.346 | 3.193 |
| valkey1 | old, default | 12.95 | 234.3 | 247.3 | 3.086 | 2.785 |
| valkey1 | new (a), default | 12.88 | 235.4 | 248.4 | 3.094 | 2.790 |
| valkey1 | jemalloc | 13.91 | 217.5 | 170.7 | 2.999 | 2.702 |

(a) takes 27 instructions and 4.5 cycles off every larson operation in the
mode (the mode's cost over thread caches: +73.7 -> +46.7 instructions, +8.3
-> +4.1 cycles) and 52 instructions off the Valkey model's; the default's
stream and counters are unchanged (264.5 / 264.5 instructions, 72.4 / 72.1
cycles).

## 2. Mechanism (b): the run as a per-CPU header of its own

The thread run lives at the bottom of the cache's stack and a run pop moves
the top entry into the hole, two words no single commit store can make
consistent under an abort; that is why the landed form claimed one slot
under the lock per scheduled pop. Now the run is a second table of
`nr_cpu_ids` `[CacheHeader; NCLASSES]` (`Percpu::runs`): the header of
`(id, class)` holds up to `cap / 4` slots, never schedules (`compact_mask
== u32::MAX`) and is served by the same `pop` section as a cache. A
scheduled pop in compact mode pops the id's run (`Heap::percpu_run_pop`);
an exhausted or uninstalled run is rebuilt as the thread run is (the class
re-checked, then a block of `cap / 4` from the arena's lowest partial slab
under the class lock, `push_batch`ed onto the run lowest on top, the zero
prefix with it, the overflow of a migration released), so the lock is held
once per `cap / 4` scheduled pops. No new asm: the sections are `pop`,
`push_batch`, `take_all` and `install` on the second table. The drain
takes the run too; the GC returns a run whose class stopped compacting.
Test: `heap::linux_tests::percpu_scheduled_pops_come_from_a_run_claimed_a_block_at_a_time`
(the residue, the probe, one block claim of `cap / 4`, `cap / 4 - 1`
scheduled pops from the run with no further claim and each in the lowest
slab with room, conservation with the run counted, the rebuild, the drain).

Attribution after (a) + (b) (same protocol; `old, mode` is the landed form):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old, mode | 36.88 | 80.6 | 338.2 | 2.119 | 2.982 |
| larson1 | new (a+b), mode | 39.30 | 75.6 | 311.2 | 2.113 | 2.940 |
| larson1 | new (a+b), default | 41.44 | 72.2 | 264.5 | 2.114 | 4.017 |
| larson1 | jemalloc | 44.76 | 66.5 | 224.8 | 2.064 | 1.692 |
| valkey1 | old, mode | 9.86 | 308.0 | 425.1 | 3.342 | 3.185 |
| valkey1 | new (a+b), mode | 11.42 | 265.9 | 325.6 | 3.188 | 2.909 |
| valkey1 | new (a+b), default | 12.76 | 237.6 | 247.7 | 3.090 | 2.789 |
| valkey1 | jemalloc | 13.69 | 221.0 | 170.5 | 2.963 | 2.718 |

(b) takes another 47 instructions and 34 cycles off the Valkey model's
operation (the mode's cost over thread caches on that row: +177 -> +78
instructions, +70 -> +28 cycles) and leaves larson untouched (no
compaction there: 311.2 / 311.2). What remains on the Valkey model is the
per-side section cost of (a)'s table (+47 instructions) plus the scheduled
pop's cold path once in 8 pops and one block claim per `cap / 4` of them.

## 3. Fast tier: the mode against the default (`make quick OLD=dfe3f60 NEW_CONF=percpu_arena:percpu`)

`old` = dfe3f60 as shipped (thread caches), `new` = the tree after (a) + (b)
under `percpu_arena:percpu`; failing rows confirmed on the median of 3.

| row | unit | old (default) | new (mode) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.7 (15.9 MiB) | 39.4 (16.4 MiB) (median of 3) | 45.0 (18.1 MiB) | +5.8% | +14.2% |
| larson10 | M ops/s | 361.7 (112.0 MiB) | 331.8 (122.5 MiB) (median of 3) | 388.0 (132.2 MiB) | +9.0% | +16.9% |
| xmalloc | M frees/s | 582.915 (53.5 MiB) | 486.587 (74.9 MiB) (median of 3) | 484.133 (79.8 MiB) | +19.8% | -0.5% |
| alloc-test1 | s | 2.680 (12.9 MiB) | 2.810 (13.0 MiB) (median of 3) | 2.519 (12.3 MiB) | +4.9% | +11.6% |
| valkey1 | M ops/s | 7.138 (584.2 MiB) | 6.826 (584.3 MiB) (median of 3) | 7.313 (585.4 MiB) | +4.6% | +7.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3459 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 23215448 ops/s |

Before this work (the predecessor's section 2, same protocol at 9351eeb):
+15.1% / +18.7% / +23.9% / +10.2% / +9.7%. Verdict for the mode: NO-GO
(larson1, larson10 and xmalloc beyond the 5% gate; alloc-test1 and valkey1
inside it). The churn probe compacts under the mode (97.0%, the run).

## 4. Fast tier: the default against itself (`make quick OLD=dfe3f60`)

`old` = dfe3f60, `new` = the tree after (a) + (b), both as shipped (thread
caches); this is the acceptance for the change (the default must not
regress):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.7 (16.0 MiB) | 42.0 (16.0 MiB) | 45.1 (18.6 MiB) | -0.7% | +7.4% |
| larson10 | M ops/s | 363.2 (111.1 MiB) | 362.2 (112.3 MiB) | 385.7 (133.1 MiB) | +0.3% | +6.5% |
| xmalloc | M frees/s | 581.912 (56.6 MiB) | 575.125 (53.1 MiB) | 481.785 (79.7 MiB) | +1.2% | -16.2% |
| alloc-test1 | s | 2.683 (12.9 MiB) | 2.666 (13.0 MiB) | 2.503 (12.3 MiB) | -0.6% | +6.5% |
| valkey1 | M ops/s | 7.305 (584.2 MiB) | 7.225 (584.2 MiB) | 7.280 (585.5 MiB) | +1.1% | +0.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24245576 ops/s |

Verdict: GO (every row within noise; the thread paths are byte for byte
what they were, section 1).

## 5. Protocol tier (`make ab`, 5 rounds, same run; `2026-09-19-aarch64-task8-percpu-fastpath-ab.md`)

`old` = dfe3f60 as shipped, `new` = the tree after (a) + (b) under
`percpu_arena:percpu` (the mode), `new-off` = the same library as shipped
(thread caches, the default), jemalloc 5.3.1. Positive is worse than `old`.

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.7 | 15.9 | 39.6 | 16.3 | 42.0 | 16.0 | 45.0 | 18.5 | +5.3% | -0.7% | -7.3% |
| larson10 | M ops/s | 364.1 | 111.6 | 332.8 | 122.8 | 365.6 | 110.6 | 383.6 | 132.1 | +9.4% | -0.4% | -5.1% |
| xmalloc | M frees/s | 582.076 | 53.1 | 489.432 | 82.1 | 578.982 | 54.1 | 480.789 | 79.1 | +18.9% | +0.5% | +21.1% |
| valkey1 | M ops/s | 7.204 | 584.6 | 6.883 | 584.6 | 7.227 | 584.6 | 7.150 | 585.6 | +4.7% | -0.3% | +0.8% |
| alloc-testN | s | 2.323 | 16.4 | 2.434 | 16.2 | 2.306 | 16.3 | 2.217 | 16.6 | +4.8% | -0.7% | -4.6% |

The predecessor's `ab2` at 94fa9aa (`2026-09-19-aarch64-task8-percpu-ab2.md`)
had the mode at larson1 +14.8%, larson10 +18.2%, xmalloc +25.0%, valkey1
+10.1%, alloc-test1 +10.6%.

## 6. Verdict

The default (`new-off`, what ships) is within noise of `old` on every
`quick` and `ab` row on this VM: the acceptance for the change holds. The
mode after both mechanisms is roughly half as far from thread caches as it
was: larson1 +5.3%, larson10 +9.4%, xmalloc +18.9%, valkey1 +4.7%,
alloc-testN +4.8% on `ab` (from +14.8 / +18.2 / +25.0 / +10.1 / +10.6), with
valkey1 and the alloc-test rows now inside the 5% gate and larson1 at it;
it is not within noise, so `auto` keeps thread caches and
`percpu_arena:percpu` stays the opt-in. What is left, attributed: the
per-side section cost of section 1 (+47 instructions per larson operation
on this CPU: the descriptor store, the `mm_cid` load and range check, the
table index, three loads, the status word, the 7-instruction byte count),
which is the floor of the design as written; and the xmalloc and larson10
rows, whose cost this file does not attribute to an instruction (the
producer/consumer and cross-thread frees exercise `percpu_flush`, which
copies the stack out and back through two sections and releases by owner,
where the thread flush works in place and `free_other` batches per arena;
xmalloc's RSS 53 -> 82 MiB under the mode says the copies keep more slots
in flight). Not pursued here (the plan's rule: stop once the mode's gap is
recorded).
