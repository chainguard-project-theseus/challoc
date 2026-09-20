# Phase 2.5 task 8, the fast-path follow-up: per-CPU mode cost, x86_64

Lane A, 2026-09-19/20, reference VM challoc-bench-amd64 (C4, Intel Xeon 6
(Granite Rapids), 16 vCPUs, kernel 7.0.0-1011-gcp, glibc 2.44, `__rseq_size`
33, `nr_cpu_ids` 16). `old` is dfe3f60 (main after lane B's merge; the
per-CPU code there is byte for byte task 8's landed form, 94fa9aa), `new`
the tree after each mechanism (commits in PROGRESS.md). The mode is
`MALLOC_CONF=percpu_arena:percpu` on either library; the default (`auto` =
thread caches) is the library as shipped. Method: bench/README.md
(`challoc-bench quick-perf` for the per-operation attribution, `make quick
OLD=dfe3f60 NEW_CONF=percpu_arena:percpu` for the mode against the base's
default and `make quick OLD=dfe3f60` for the default against itself, then
`make ab` 5 rounds). The mechanisms, the code and the disassembly note are
in the aarch64 file (`2026-09-19-aarch64-task8-percpu-fastpath.md`,
sections 1 and 2; objdump ran on the Mac's aarch64 container); the x86_64
particular of mechanism (a) is that the pop section compares `len` with
`zeroed` inside the asm and drops the `zeroed` output register, so the
wrapper's hit path fits the caller-saved registers.

The predecessor's attribution (`2026-09-19-x86_64-task8-percpu.md`, section
3): +65 instructions and +6.3 cycles per larson operation with misses flat;
+180 / +107 on the Valkey model, the compact-mode locked one-slot claim.

## 1. Mechanism (a): the section inlined into a frameless tail-call wrapper

Per-operation attribution (`challoc-bench quick-perf amd64 OLD=dfe3f60
NEW=ee0ecd8`, perf on the VM host, one run each; the mode is
`percpu_arena:percpu` on the named library, the default the library as
shipped):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old, mode | 40.82 | 79.9 | 301.0 | 1.821 | 0.005 |
| larson1 | new (a), mode | 42.52 | 76.7 | 276.9 | 1.788 | 0.003 |
| larson1 | old, default | 44.19 | 74.1 | 245.2 | 1.610 | 0.002 |
| larson1 | new (a), default | 45.02 | 72.9 | 244.4 | 1.605 | 0.002 |
| larson1 | jemalloc | 44.89 | 72.5 | 200.9 | 2.124 | 0.001 |
| valkey1 | old, mode | 15.31 | 220.0 | 382.1 | 3.496 | 0.908 |
| valkey1 | new (a), mode | 16.73 | 201.5 | 352.2 | 3.538 | 0.913 |
| valkey1 | old, default | 23.19 | 145.3 | 241.4 | 3.313 | 0.904 |
| valkey1 | new (a), default | 23.56 | 143.0 | 240.4 | 3.322 | 0.908 |
| valkey1 | jemalloc | 26.28 | 128.1 | 152.9 | 3.351 | 0.942 |

(a) takes 24 instructions and 3.2 cycles off every larson operation in the
mode (the mode's cost over thread caches: +55.8 -> +32.5 instructions, +5.8
-> +3.8 cycles) and 30 instructions / 18.5 cycles off the Valkey model's;
the default's counters are unchanged within the run's noise (245.2 / 244.4
instructions, 74.1 / 72.9 cycles).

## 2. Mechanism (b): the run as a per-CPU header of its own

Attribution after (a) + (b) (same protocol; `old, mode` is the landed form):

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old, mode | 40.08 | 81.3 | 300.9 | 1.807 | 0.005 |
| larson1 | new (a+b), mode | 42.60 | 76.1 | 277.0 | 1.772 | 0.003 |
| larson1 | new (a+b), default | 44.78 | 73.0 | 244.0 | 1.614 | 0.001 |
| larson1 | jemalloc | 41.79 | 78.0 | 200.9 | 2.120 | 0.001 |
| valkey1 | old, mode | 15.03 | 224.3 | 389.3 | 3.526 | 0.915 |
| valkey1 | new (a+b), mode | 19.43 | 173.5 | 298.7 | 3.317 | 0.905 |
| valkey1 | new (a+b), default | 23.58 | 142.9 | 240.4 | 3.317 | 0.905 |
| valkey1 | jemalloc | 26.13 | 128.9 | 153.6 | 3.362 | 0.943 |

(b) takes another 54 instructions and 28 cycles off the Valkey model's
operation (the mode's cost over thread caches on that row: +148 -> +58
instructions, +81 -> +31 cycles; the model's L1D misses fall back to the
default's, 3.53 -> 3.32, the one-slot claims' bitmap lines gone) and leaves
larson untouched (277.0 / 276.9).

## 3. Fast tier: the mode against the default (`make quick OLD=dfe3f60 NEW_CONF=percpu_arena:percpu`)

| row | unit | old (default) | new (mode) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.7 (16.7 MiB) | 42.2 (17.4 MiB) (median of 3) | 45.0 (17.8 MiB) | +5.9% | +6.6% |
| larson10 | M ops/s | 288.2 (95.9 MiB) | 235.5 (94.2 MiB) (median of 3) | 289.6 (90.0 MiB) | +22.4% | +23.0% |
| xmalloc | M frees/s | 331.045 (52.7 MiB) | 284.103 (58.0 MiB) (median of 3) | 328.273 (98.1 MiB) | +16.5% | +15.5% |
| alloc-test1 | s | 3.439 (13.6 MiB) | 3.529 (13.6 MiB) | 3.228 (13.1 MiB) | +2.6% | +9.3% |
| valkey1 | M ops/s | 14.995 (585.2 MiB) | 10.764 (584.9 MiB) (median of 3) | 13.955 (591.7 MiB) | +39.3% | +29.6% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3461 (97.0%) | 6801 -> 3459 (97.0%) | 6798 -> 6658 (49.9%) | new compacts (end occupancy >= 90%) | 21028076 ops/s |

Before this work (the predecessor's section 2, same protocol at 9351eeb):
+11.1% / +18.9% / +23.8% / +8.2% / +67.5%. Verdict for the mode: NO-GO
(larson1 at the gate, larson10, xmalloc and valkey1 beyond it; alloc-test1
inside). The churn probe compacts under the mode (97.0%, the run). Note
the Valkey row: `quick` runs the model for 5 s (37 M overwrites of 850k
keys in the write phase, a balanced churn that fragments and compacts) and
reads +39%, where the 10 s `quick-perf` run above reads +21% in cycles
(19.43 against 23.58 M ops/s). A `quick-perf` at the 5 s length
(`VALKEY_MODEL_SECS=5`, `old` 23.28 / 144.5 cycles / 239.9 instructions per
operation, the mode 19.36 / 173.9 / 301.6, the default 23.63 / 142.5 /
240.6, jemalloc 26.73 / 125.9 / 154.7) reads the same +61 instructions and
+31 cycles per operation as at 10 s: the per-operation cost is the same,
and `quick`'s score is the write phase alone (37 M overwrites, where the
compact-mode scheduled pops and rebuilds are busiest), where the perf
counters average both phases.

## 4. Fast tier: the default against itself (`make quick OLD=dfe3f60`)

`old` = dfe3f60, `new` = the tree after (a) + (b), both as shipped (thread
caches); this is the acceptance for the change (the default must not
regress):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.3 (16.6 MiB) | 44.2 (16.7 MiB) | 43.2 (17.7 MiB) | -2.0% | -2.3% |
| larson10 | M ops/s | 278.5 (95.5 MiB) | 279.0 (95.2 MiB) | 298.4 (91.1 MiB) | -0.2% | +7.0% |
| xmalloc | M frees/s | 327.768 (53.1 MiB) | 319.391 (52.7 MiB) (median of 3) | 331.519 (88.3 MiB) | +2.6% | +3.8% |
| alloc-test1 | s | 3.442 (13.6 MiB) | 3.433 (13.5 MiB) | 3.221 (13.0 MiB) | -0.3% | +6.6% |
| valkey1 | M ops/s | 14.996 (585.2 MiB) | 15.096 (585.2 MiB) | 14.909 (591.8 MiB) | -0.7% | -1.2% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6666 (49.9%) | new compacts (end occupancy >= 90%) | 20760793 ops/s |

Verdict: GO (every row within noise; the xmalloc row's +2.6% is a median of
3 inside its usual spread, and the thread paths are byte for byte what they
were).

## 5. Protocol tier (`make ab`, 5 rounds, same run; `2026-09-19-x86_64-task8-percpu-fastpath-ab.md`)

`old` = dfe3f60 as shipped, `new` = the tree after (a) + (b) under
`percpu_arena:percpu` (the mode), `new-off` = the same library as shipped
(thread caches, the default), jemalloc 5.3.1. Positive is worse than `old`.

| program | unit | old | old RSS MiB | new | new RSS MiB | new-off | new-off RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | new-off vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.5 | 16.6 | 42.9 | 17.3 | 44.9 | 16.6 | 44.9 | 17.7 | +3.7% | -0.9% | -0.9% |
| larson10 | M ops/s | 275.3 | 94.7 | 236.1 | 94.5 | 277.8 | 95.3 | 290.5 | 92.1 | +16.6% | -0.9% | -5.2% |
| xmalloc | M frees/s | 329.693 | 53.1 | 284.095 | 58.2 | 330.236 | 52.8 | 329.211 | 93.5 | +16.1% | -0.2% | +0.1% |
| valkey1 | M ops/s | 15.030 | 585.6 | 10.908 | 585.4 | 14.980 | 585.7 | 14.965 | 592.3 | +37.8% | +0.3% | +0.4% |
| alloc-testN | s | 4.544 | 17.1 | 4.722 | 17.1 | 4.607 | 17.1 | 4.169 | 16.7 | +3.9% | +1.4% | -8.3% |

The predecessor's `ab2` at 94fa9aa (`2026-09-19-x86_64-task8-percpu-ab2.md`)
had the mode at larson1 +9.0%, larson10 +20.6%, xmalloc +21.8%, valkey1
+56.9%, alloc-test1 +7.2%.

## 6. Verdict

The default (`new-off`, what ships) is within noise of `old` on every
`quick` and `ab` row on this VM: the acceptance for the change holds. The
mode after both mechanisms: larson1 +3.7%, larson10 +16.6%, xmalloc +16.1%,
valkey1 +37.8%, alloc-testN +3.9% on `ab` (from +9.0 / +20.6 / +21.8 /
+56.9 / +7.2), the single-threaded larson and alloc-test rows inside the 5%
gate, the rest not; it is not within noise, so `auto` keeps thread caches
and `percpu_arena:percpu` stays the opt-in. Attributed: the per-side
section cost (+33 instructions per larson operation here, section 1); the
Valkey model at `quick`'s 5 s (section 3's note); and the larson10 / xmalloc
rows, which this file does not attribute to an instruction (see the aarch64
file's verdict: the per-CPU flush copies the stack out and back, where the
thread flush works in place). Not pursued here (the plan's rule: stop once
the mode's gap is recorded).
