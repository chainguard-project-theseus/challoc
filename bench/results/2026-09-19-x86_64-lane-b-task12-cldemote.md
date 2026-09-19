# 2026-09-19, x86_64: CLDEMOTE on the release path (Phase 2.5 task 12, lane B)

Outcome: **measured present on the C4 and dropped**, the probe, the report key (`cldemote=yes` on
the 8581C) and a both-ways test of the instruction kept (`src/sys/cldemote.rs`). The hint costs
the gated rows 5% (xmalloc-test) and 13..17% (the Valkey model) in cycles per operation over
three rounds, and leaves the `io-threads 4` Valkey soak, its intended workload, within noise.

- base: fc081b1 (Phase 2.5 task 8 lane B commit on top of 675f357)
- host: `challoc-lane-b-amd64`, c4-standard-16 (Xeon Platinum 8581C, Emerald Rapids), Ubuntu
  24.04 kernel 7.0.0-1011-gcp, the dev image (Wolfi, glibc 2.44), created for this lane; the C4A
  lane VM `challoc-lane-b-arm64` for the no-op side. Positive percentages: new did worse.
- probe: `cpuid` leaf 7 subleaf 0 `ECX[25]` (`sys::cpu`, `X86::cldemote`, since task 2); the
  hypervisor exposes it here (`cldemote=yes` in the line, `cldemote` in `/proc/cpuinfo` flags;
  task 2's `bench/results/2026-09-19-feature-report.md` recorded the same), so the item is
  measured, not "absent on GCE".

## 1. The candidate

Mechanism attacked: the two cache lines a freeing thread writes last and the next claimer's core
reads first, in the batch release lane B owns (`class.rs`, `ClassState::release_owned`):

1. the class bitmap word whose bits `release_word` clears with a `fetch_and` (the refill's
   `claim_with` reads it next; on a cross-thread free, from another core);
2. the slab descriptor line whose `live` count `dec_live` lowers (the claimer's `add_live`).

`cldemote [addr]` after each of the two RMWs, behind a process-wide `AtomicBool` set from the
probe at init (`sys::cldemote::configure`, `demote`), a no-op on arm64 and when the bit is absent.
A variant that also demoted the first data line of every released slot (`DATA_LINES`) was
written but not measured: the two-line form already lost.

The intended win: on xmalloc-test (a producer allocates, a consumer frees; the consumer's flush
releases into slabs the producer refills from) and Valkey with `io-threads 4` (the io threads
allocate the argv objects, the main thread or another io thread frees them:
`bench/results/2026-09-19-aarch64-gce-valkey-soak-iothreads4.md`), the line would be in L3 when
the other core's RMW arrives instead of needing a cross-core transfer.

## 2. quick (go/no-go, one round, `challoc-bench quick amd64 OLD=HEAD`, load 4.3 at start)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.3 (16.8 MiB) | 44.9 (16.9 MiB) | 44.6 (18.1 MiB) | +0.9% | -0.7% |
| larson10 | M ops/s | 294.7 (96.7 MiB) | 289.4 (96.8 MiB) (median of 3) | 296.7 (91.4 MiB) | +1.8% | +2.5% |
| xmalloc | M frees/s | 322.463 (55.1 MiB) | 317.449 (52.0 MiB) | 333.517 (99.0 MiB) | +1.6% | +5.1% |
| alloc-test1 | s | 3.372 (13.9 MiB) | 3.435 (13.8 MiB) | 3.056 (13.3 MiB) | +1.9% | +12.4% |
| valkey1 | M ops/s | 14.134 (585.1 MiB) | 14.539 (585.1 MiB) | 14.237 (592.0 MiB) | -2.8% | -2.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6650 (50.0%) | new compacts | |

`quick: go` (156 s; old `a0d2d2e09538aee6`, new `51c0e5c1e4e203c9`). The C4A `quick` of the same
tree (the hint compiled out): go, every row within 1.8% (`.tmp-arm64-chain1.log`, one round).
The gate passed; the attribution below is what decided.

## 3. Attribution (`challoc-bench quick-perf amd64`, perf stat on the whole process, per operation)

Three rounds, the first with `jemalloc` for scale (load 3.4..4.1 at start on every round: the
lane's other VM job was the `make test` that had just finished; both variants saw the same host).

| round | row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---|---:|---:|---:|---:|---:|
| 1 | xmalloc | old | 318.27 | 138.4 | 371.2 | 0.281 | 0.003 |
| 1 | xmalloc | new | 308.63 | 142.1 | 371.5 | 0.296 | 0.003 |
| 1 | xmalloc | jemalloc | 330.40 | 153.2 | 235.2 | 0.392 | 0.003 |
| 1 | larson1 | old | 44.86 | 73.7 | 235.7 | 1.604 | 0.001 |
| 1 | larson1 | new | 44.50 | 75.2 | 237.1 | 1.620 | 0.002 |
| 1 | larson1 | jemalloc | 45.73 | 72.3 | 200.9 | 2.120 | 0.001 |
| 1 | valkey1 | old | 25.26 | 149.3 | 227.1 | 3.284 | 0.898 |
| 1 | valkey1 | new | 20.88 | 177.4 | 241.0 | 3.365 | 0.910 |
| 1 | valkey1 | jemalloc | 27.83 | 134.4 | 152.1 | 3.358 | 0.949 |
| 2 | xmalloc | old | 322.82 | 136.5 | 371.1 | 0.276 | 0.002 |
| 2 | xmalloc | new | 303.85 | 143.5 | 371.5 | 0.298 | 0.003 |
| 2 | valkey1 | old | 23.11 | 150.6 | 231.6 | 3.293 | 0.907 |
| 2 | valkey1 | new | 20.91 | 170.8 | 242.9 | 3.387 | 0.906 |
| 3 | xmalloc | old | 324.24 | 136.2 | 371.2 | 0.274 | 0.002 |
| 3 | xmalloc | new | 306.43 | 142.9 | 371.4 | 0.291 | 0.003 |
| 3 | valkey1 | old | 24.95 | 152.2 | 227.1 | 3.275 | 0.905 |
| 3 | valkey1 | new | 21.32 | 178.6 | 242.9 | 3.381 | 0.910 |

(`valkey1` here is the whole process divided by both phases' operations, so its M ops/s is not
the `quick` row's write-phase score.) Reading:

- xmalloc-test: **+2.7, +5.1, +4.9% cycles per operation**, +5..8% L1D misses per operation,
  instructions +0.1% (the two `cldemote` per released slab word amortise over the batch). The
  cross-thread workload the hint was for loses: the consumer's flush releases a slab's words,
  but the producer's refill takes the *same* consumer's slabs only some of the time; the rest
  of the released words and descriptors are re-read by the consumer itself (`dec_live` →
  `on_live_zero`, `reopen`, the next flush into the same slab), now from L3.
- Valkey model (single thread, 585 MiB heap): **+18.8, +13.4, +17.3% cycles per operation** and
  +6% instructions per operation (`sdallocx` batches release into slabs the same thread
  refills from at once; every demoted bitmap word and descriptor comes back as an L3 hit, about
  40 cycles instead of 5 on this part, and the descriptor is re-read within the same
  `release_owned` for `state()` and by `on_live_zero`). L1D misses per operation +2..3%: the
  demoted lines are not counted as L1D load misses when the following access is an RFO, which
  is why the cycles move more than the miss counter.
- larson1: +2% cycles, one round: within its spread, but not a gain.

## 4. The `io-threads 4` Valkey soak, old vs new (challoc only, same Valkey build)

`make valkey-soak ALLOCS=challoc IO_THREADS=4 DURATION=600 SETTLE=60` twice on the C4 (old =
fc081b1's quantum8 library, new = the same with the hint; the challoc-linked `valkey-server`
9.1.2 build from `make valkey STAGE=build`, `io-threads=4` read back from `CONFIG GET`; old
first, then new, 20:36..20:59 UTC; the perf sidecar `bench/probes/valkey-perf-sidecar.sh`
attached from the host, 60 s windows at t=100 and t=400). One round each: **a shape check, not
an acceptance row**; the jemalloc column is absent by design (old/new is the question). Driver:
`.tmp-soak-ab.sh` (in the worktree, git-ignored), logs `~/logs/soak-ab/` on the VM.

| measure | old (fc081b1) | new (hint) | new vs old |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 1310541 | 1329429 | -1.4% (new faster) |
| write: valkey-benchmark p99 ms | 0.799 | 0.783 | -2.0% |
| write: memtier req/s | 1297617 | 1319580 | -1.7% |
| write: memtier p99 ms | 0.631 | 0.623 | -1.3% |
| delete: valkey-benchmark req/s | 2049571 | 2128240 | -3.8% |
| delete: valkey-benchmark p99 ms | 0.423 | 0.415 | -1.9% |
| delete: memtier req/s | 1765716 | 1840262 | -4.2% |
| delete: memtier p99 ms | 0.471 | 0.431 | -8.5% |
| peak RSS MiB (VmHWM) | 709.4 | 714.2 | +0.7% |
| RSS MiB at end of write phase | 657.6 | 634.1 | -3.6% |
| RSS MiB at end of delete phase | 125.3 | 134.5 | +7.3% |
| RSS MiB after settle | 125.1 | 133.2 | +6.5% |
| allocator_frag_ratio after settle | 1.48 | 1.61 | |
| active_defrag_hits | 374316 | 421180 | |

Sidecar, per command (whole server process, all threads; `total_commands_processed` deltas
between the window snapshots):

| window | variant | commands | cycles/cmd | instr/cmd | L1D miss/cmd | dTLB miss/cmd | branch miss/cmd |
|---|---|---:|---:|---:|---:|---:|---:|
| write t=100..161 | old | 79.2 M | 10362 | 10135 | 61.1 | 4.1 | 7.7 |
| write t=100..161 | new | 78.3 M | 10244 | 10143 | 61.6 | 3.9 | 7.5 |
| delete t=400..462 | old | 111.5 M | 7041 | 8037 | 33.1 | 0.6 | 5.8 |
| delete t=400..461 | new | 113.8 M | 6999 | 7880 | 33.2 | 0.4 | 4.6 |

Cycles per command -1.1% (write) and -0.6% (delete), L1D misses per command equal: the hint does
nothing measurable to the soak, whose cycles are the io threads' queue polling (the aarch64
iothreads4 file: `spmcDequeue` and the queue atomics are 17% of the window, and the allocator's
own code under 8%). The one-round throughput differences (1..4%, new run second on a warmer
host) and the +6.5% RSS after settle are inside what two runs of this soak differ by; the
sidecar counters are the evidence and they are flat.

## 5. Decision

Dropped, per task 12 ("dropped if it does not pay"): a fast-path change must pay on the workload
it is for, and this one costs the two rows that gate every change (xmalloc-test, the Valkey
model) 5% and 15% in cycles per operation while the soak does not move. The mechanism is sound
only when the release path knows the next claimer is another core, which it does not: the same
`release_owned` serves a thread's own cache flush (single-threaded consumers, the common case)
and a cross-arena release. Kept: the probe and the `cldemote=` key (task 2), `src/sys/cldemote.rs`
with `configure` (installed from the probe at init), `enabled` and `demote`, and its both-ways
container test (`demote_follows_the_probe_and_never_changes_memory`: on the C4 `present=true
enabled=true`, the instruction runs over a written buffer and unmapped addresses and changes
nothing; on the C4A and the Mac VM `present=false enabled=false`). `class.rs` is byte-identical
to the base after the drop.

A possible future form, for the human: issue the hint only in a flush that `Heap::release_routed`
knows is a foreign-arena release (the sized-free routing or the `others` node flush, heap.rs,
lane A's file), where the claimer is by construction another thread; the io-threads soak's flat
counters suggest even that has little to win on this CPU.

## 6. Tests

- Host `cargo test --features std`: 298 pass (task 8's 287 + the class and cldemote modules'
  additions, none of them on the host beyond compilation).
- `make test` with the hint in (the measured library): C4 377 + 115 + 23 pass, `cldemote:
  present=true enabled=true`, six containers `verdict=ok`; C4A 377 + 115 + 23, `present=false`,
  `verdict=ok`; Mac VM 377 + 115 + 23, `present=false`, `verdict=ok`.
- `make test` with the final code (call sites dropped): appended below.

Final code (`class.rs` restored to the base; `src/sys/cldemote.rs` with `configure`, `enabled`,
`demote` and the test; `configure` called from `Features::probe`): C4 377 + 115 + 23 pass,
`cldemote: present=true enabled=true`, six containers `verdict=ok`; C4A 377 + 115 + 23,
`present=false enabled=false`, `verdict=ok`; Mac VM 377 + 115 + 23, `present=false`,
`verdict=ok`. Host `cargo test --features std`: 298 pass.

## 7. Final `quick` (C4, the shipped tree vs fc081b1: the fast paths are the same source, the
library differs by the unused module; one round, load 4 at start)

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 44.5 (17.1 MiB) | 45.5 (17.1 MiB) | 44.2 (17.8 MiB) | -2.2% | -2.9% |
| larson10 | M ops/s | 262.8 (97.2 MiB) | 270.7 (95.1 MiB) | 290.2 (92.8 MiB) | -2.9% | +7.2% |
| xmalloc | M frees/s | 317.154 (54.1 MiB) | 326.780 (52.8 MiB) | 324.829 (95.6 MiB) | -2.9% | -0.6% |
| alloc-test1 | s | 3.436 (13.9 MiB) | 3.418 (13.9 MiB) | 3.239 (13.3 MiB) | -0.5% | +5.5% |
| valkey1 | M ops/s | 14.664 (585.2 MiB) | 13.983 (585.1 MiB) | 14.780 (592.0 MiB) | +4.9% | +5.7% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6655 (50.0%) | new compacts | |

`quick: go` (old `a0d2d2e09538aee6`, new `9de61aec96f673ee`). With identical hot-path source the
row-to-row spread here (-2.9..+4.9%) is the single-round noise of a host that had just run
`make test`; no `ab` was run, since nothing on a fast path changed in the shipped code.
