# The T2A (Ampere Altra) larson rows: a first reading (Phase 2.5 task 4b)

Date 2026-09-19 (12:00..12:37 UTC). `challoc-matrix-cpu-t2a`,
`t2a-standard-8` in `us-central1-a` (`challoc-bench matrix lab cpu-t2a`),
Neoverse N1 (MIDR 0x413fd0c1), 8 vCPU, 64 KiB L1D, 1 MiB L2, Ubuntu 24.04
`7.0.0-1011-gcp`, no PMU (GCE refuses `--performance-monitoring-unit` on
T2A). Tree a50687f via `target/quick/new`, jemalloc 5.3.1. Deleted at the
end of the iteration. Logs: `bench/results/matrix/2026-09-19-4b-t2a-*`.

Two instruments, both without a PMU: `bench/sampler.so` (SIGPROF at 1000 Hz;
usable on the single-threaded model, but on larson it delivered only
200..1300 samples in a run, too few to annotate: `2026-09-19-4b-t2a-sampler.log`)
and `perf record -e cpu-clock -F 2000 -p <container pid>` with `perf
annotate` over a `docker cp` symfs (`linux-tools-7.0.0-1011-gcp` installed
on the VM), which gave 20..60k samples per run.

## 1. Rows and split (perf cpu-clock, share of all samples)

| row | allocator | throughput | program | malloc (`operator new[]` alias) | free (`operator delete[]` alias) | rest of the allocator | kernel |
|---|---|---:|---:|---:|---:|---:|---:|
| larson1 | challoc | 20.8 M ops/s | `exercise_heap` 47.7% | 24.7% | 22.3% | 1.6% (flush, release_owned, release_slab, free_slow, LSE helpers 0.25%) | 1.9% |
| larson1 | jemalloc | 26.3 M ops/s | 60.8% | 18.0% | 16.7% | 1.5% | 0.5% |
| larson10 | challoc | 151 M ops/s | 53.2% | 23.0% | 19.9% | 1.3% (LSE helpers 0.24%) | 1.1% |
| larson10 | jemalloc | 173 M ops/s | 63.8% | 18.1% | 12.7% | 2.3% | 0.9% |
| valkey1-free | challoc | write 5.44 M SET/s | 61.7% (`set_overwrite` 32.4, `main` 23.0, `set_new` 5.3) | `je_malloc` 5.3% | 19.8% | 8.6% | 4.5% |
| valkey1-free | jemalloc | write 6.24 M SET/s | 76.2% | 7.5% | 11.5% | 2.5% | 2.0% |

In time per operation (larson1: 48.0 ns per malloc+free pair under challoc,
38.1 under jemalloc): the program's own code costs the same, **22.9 vs
23.2 ns**; challoc's `malloc` + `free` pair costs **22.5 ns against
jemalloc's 13.2**, and that 9.4 ns is the whole +26% of the row. larson10
reads the same way (the pair 20.4 vs 13.7 ns of 47.4 vs 41.3 per op on 10
threads). The same split on the C4A (Axion, Neoverse V2) is +6.6% for the
row, so the code that costs 9 ns extra on the N1 costs about 2 ns extra on
the V2. The sampler's valkey1-free run (both phases) agrees: the program
79.4 vs 80.2 samples per M ops, the allocator 46.9 vs 23.0, of which
challoc's `free` alone is 25.9 against jemalloc's `free` 11.9.

Not the mechanism: the outlined LSE helpers (`__aarch64_ldadd4_acq_rel`
0.12%, `__aarch64_ldclr8_rel` 0.09%, `__aarch64_cas4_acq` 0.03% in larson1;
0.24% together in larson10), the guard-slab division of the x86 file
(`guards:false` changes nothing here: larson1 21.29 vs 21.31 M ops/s,
valkey1-free 5.70 vs 5.63 M SET/s, two rounds each; the aarch64 path is
the plain `free`, which has no division), kernel time (1.9% vs 0.5%).

## 2. The instruction-level reading: challoc's `free` is a six-load dependent chain

`perf annotate` of the two entry points (`.tmp` symfs, aarch64 `objdump`
around the hot addresses):

- challoc `malloc` (60 instruction lines against jemalloc's 54): the samples
  sit on `add x8, x0, #0xf` (34%, the size rounding at entry, i.e. the call
  itself) and the `ret` (15%), with the class-table loads at 3..6%: a short
  path, no single stall.
- challoc `free` (125 instruction lines against jemalloc's 45). The hot
  instruction is `lsr x12, x13, x12` at `+0x2afd8`, **24.2%** of `free` in
  larson1 and **68.1%** in valkey1-free; it consumes the bitmap word loaded
  by `ldr x13, [x13, x14]` just before it (the slot's bit test). The path
  to it is a chain of dependent loads and multiplies:
  1. `ldr x11, [x11, #24]` the region table entry for the class (region base),
  2. `ldrb w12, [x14, #14]`, `ldp w17, w16, [x14, #4]`, `ldr w14, [x14]` the class geometry (slot shift, slot count, the multiply-shift reciprocal), then `lsl`/`bic`/`mul`/`lsr`/`cmp`/`umull`/`cmp` for the slab and slot index and the slot-alignment check,
  3. `madd x14, x9, x14, x10` (+ `ldr x11, [x14, #656]`) the `ClassState` (stride 384) and its committed-slab count,
  4. `ldrh w12, [x11, #24]`, `ldr x14, [x11]` the bitmap geometry and base,
  5. `ldr x13, [x13, x14]` the bitmap word, `lsr`, `tbz` the bit,
  6. `ldrb w12, [__dso_handle+256]` (the arena-check flag), `ldrh w12, [x8, #12]` (the heap's arena), `ldr x11, [x11, #8]`, `ldrh w10, [x11, x10, lsl #1]` (the slab's owner word), `cmp` the ownership check,
  then the push. jemalloc's `free` on the same CPU is two loads deep
  (rtree leaf, then the tcache bin) and its hot lines are the `cmp`/`tbz`
  after the rtree load (15..17%) and the bin arithmetic (14..18%).

Reading: the N1 core (4-wide decode, a 128-entry window, one divider,
two load pipes) does not hide a ~50-instruction chain of six dependent
loads and three multiplies behind the caller's work the way the V2 (and
Golden Cove) do; each `free` pays the chain's latency. On larson the
chain's loads hit L1/L2 (the working set is a few MiB) and the cost is
latency, 9 ns per pair; on valkey1-free the bitmap word of a random slot
in a 500 MiB heap is cold and its consumer alone is 68% of `free`
(jemalloc's rtree leaf misses too, but it is one load, not the fifth of
six). The 1 MiB L2 (against the V2's 2 MiB) is the second-order term: the
class geometry, `ClassState` and bitmap pages compete with the program's
data for half the space.

## 3. What would prove it, and the candidate fix

- Prove: an N1 with a PMU (none on GCE) or the sampler's share of `lsr
  +0x2afd8` after each change; on the C4A, `quick-perf larson1` instr/op
  and L1D misses per op before and after (the V2 hides the chain, so the
  aarch64 gate is "no regression", the win is measured on a T2A lab VM's
  larson1/valkey1-free rate: `challoc-bench matrix lab cpu-t2a` then the
  `bench_args larson1` command under both libraries).
- Candidate fix (a task for the plan, not this iteration): shorten the
  plain `free`'s dependent chain. The region-table and class-geometry loads
  (steps 1..2) can become arithmetic on constants once the class is known
  from the region index (per-class regions are fixed at reservation time:
  base and slot shift are compile-time per class, only the region base is
  a load); the owner check (step 6) can be folded into the bitmap word's
  neighbour or skipped on the fast path when the process has one arena
  (`multi_arena == false`, already a flag), which is Valkey's and Ruby's
  case; the `ClassState` committed-count check (step 3) can move to the
  flush like the sized path's bit test did (task 4e). Expected: the pair
  from 22.5 toward 15 ns on the N1; on the V2 a few percent on larson1.
