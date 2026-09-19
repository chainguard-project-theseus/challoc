# Phase 2.5 task 12, lane C: spin-wait instructions in the lock and cache-geometry-sized defaults

Date 2026-09-19 (18:25..19:15 UTC), lane C (`loop/lane-c`), base commit
9ae5a60 (the `old` of every A/B). The lock commit is 5f0a138 (the tree
measured as `new`, `base` and `wfe`, section 2; run 3 is that tree
exactly), the geometry record 23bb3a9 (the geometry code that was
measured and dropped is described in section 3 and exists in no commit). Four VMs,
all on `ubuntu-2404-lts-*` with kernel 7.0.0-1011-gcp, the Wolfi
container, rust 1.98.1, jemalloc 5.3.1, mimalloc-bench 06a4c4f; all
deleted at the end (`gcloud compute instances list --filter='name~challoc-lane-c'`
empty):

| VM | machine | zone | CPU | L1D / L2 | probe |
|---|---|---|---|---|---|
| `challoc-lane-c-matrix-cpu-c2` | c2-standard-16 | us-east4-a | Intel Xeon @ 3.10GHz, Cascade Lake, 2 threads per core | 32K / 1M | `waitpkg=no`, spin hint `baseline` |
| `challoc-lane-c-amd64` | c4-standard-16 | us-east4-a | Xeon Platinum 8581C, Emerald Rapids (the reference shape), 2 threads per core | 48K / 2M | `waitpkg=no`, spin hint `baseline` |
| `challoc-lane-c-matrix-cpu-t2a` | t2a-standard-16 | us-central1-a | Ampere Altra, Neoverse N1 | 64K / 1M | `evtstrm`, spin hint `wfe` by the probe |
| `challoc-lane-c-arm64` | c4a-standard-16 | us-east4-a | Google Axion, Neoverse V2 (the reference shape) | 64K / 2M | `evtstrm`, spin hint `wfe` by the probe |

Method: `make ab` on the VM (`bench/ab.sh`: 5 timed rounds after one
discarded warm-up, variants interleaved per round, every score listed
sorted, RSS the median maxrss; positive percentages mean the variant did
worse). Raw records next to this one: the VM-side `make ab` logs, which
carry the driver lines, the load per round and the whole result from
`## Sorted scores` on: `2026-09-19-lane-c-ab-{c2,c4,t2a}-spin.log` (run
1), `2026-09-19-lane-c-ab-{c4,t2a}-spin-2.log` and
`2026-09-19-lane-c-ab-c4a-spin.log` (run 2), `2026-09-19-lane-c-spin-c2-2.md`
(run 3, the one `ab.sh` file that survived: the others, written under
`bench/results/` on the VMs, were deleted by later `sync --delete`
passes; the logs live in `~/logs`), `2026-09-19-lane-c-ab-{c2,t2a}-geometry.log`.

## 1. What landed

- `src/lock.rs`: the spin between two polls of the futex word is a
  `SpinHint` read once per slow-path entry: `Baseline` (`pause` / `isb`,
  `core::hint::spin_loop`, what the lock always did), `Umwait` (x86_64
  WAITPKG: `umonitor` the lock word, re-check, `umwait` in C0.1 with a
  1024-tick TSC deadline; arming before the check closes the lost-wake
  window), `Wfe` (aarch64: `ldaxr` of the word arms the exclusive monitor,
  `wfe` if it read locked; the holder's store clears the monitor and
  generates the wake-up event, Arm ARM B2.9; the timer event stream,
  `HWCAP_EVTSTRM`, 10 kHz on Linux, bounds a `wfe` no store ends; the same
  pattern as Linux's `__cmpwait_case`). `suggest_spin_hint` (the CPU
  probe's) and `set_spin_hint` (the configuration's, kept against later
  suggestions); `spin_hint()` reports the one in use. Nothing on the
  fast path changed; the protocol is the same under every hint.
- `src/sys/cpu.rs`: `HWCAP_EVTSTRM` and `event_stream()` (the gate of the
  explicit `wfe`), the pure decoder `spin_hint(isa)` (`Umwait` with
  WAITPKG, else `Baseline`), and `Cpu::probe` suggesting it: the one thing
  the feature report acts on. The report line is unchanged (no new key); the
  container test asserts the hint in use against `/proc/cpuinfo`'s
  `waitpkg` flag and `event_stream()` against `evtstrm`; host fixtures:
  every GCE x86 capture (WAITPKG hidden) gives `Baseline`, a composed
  Sapphire Rapids with `ECX[5]` gives `Umwait`, the Axion / Altra / Mac-VM
  HWCAPs give `Baseline` (they all carry `evtstrm`, so `spin_hint:wfe` is
  available there).
- `src/config.rs`: the `spin_hint` key (`auto` | `baseline` | `umwait` |
  `wfe`; `opt.spin_hint`), applied at the end of `Config::load`
  (`Config::apply_spin_hint`: `umwait` needs WAITPKG, `wfe` needs aarch64
  and the event stream, else `Issue::Unavailable` is reported and the key
  left at `auto`; the probe runs a second time only when the key is set).
  `auto` is `sys::cpu::spin_hint(isa)`: `umwait` with WAITPKG, else the
  baseline, **never `wfe`** (section 2.3).
- `src/api/mallctl.rs` (outside the lane's partition, one match arm):
  `cstr_of` knows the four `spin_hint` names so `opt.spin_hint` reads
  back a string instead of `?`.
- Tests: `lock::tests::{spin_hint_availability_follows_the_architecture,
  every_hint_wakes_under_contention, a_parked_waiter_is_released,
  spin_hint_costs}` (every hint this machine can run, forced per call;
  the Mac host runs the `wfe` path), `sys::cpu::tests::spin_hint_follows_waitpkg_and_evtstrm`,
  `config::linux_tests::spin_hint_key_installs_and_umwait_needs_waitpkg`,
  the container check in `probe_matches_proc_cpuinfo`. Host suite 284.
- Design 5 (the lock and the hint), 12 (the key), 17 (WAITPKG and the event
  stream rows split from CLDEMOTE, the caches row); 6.2 (the geometry model
  and why it was dropped).

## 2. The spin hint

### 2.1 Run 1: `old` (9ae5a60) against `new` (the lock change, `auto`)

The C2 and the C4 run the baseline both ways (WAITPKG hidden), so the row
is the mechanism's own cost (an atomic load of the hint per slow-path
entry, a `match` per spin round, the hint's code in the loop body). The
T2A's `new` runs `wfe`.

| CPU | row | old | new | new vs old | RSS old / new MiB | sorted lists |
|---|---|---:|---:|---:|---|---|
| C2 (baseline both) | larson16 M ops/s | 170.1 | 168.5 | +0.9% | 106.4 / 107.1 | overlap |
| C2 | larson10 M ops/s | 128.0 | 125.9 | +1.7% | 80.9 / 81.0 | overlap |
| C2 | xmalloc M frees/s | 236.960 | 229.011 | **+3.5%** | 52.6 / 52.8 | disjoint (234.5..238.1 vs 224.4..230.3) |
| C4 (baseline both) | larson16 | 311.7 | 308.2 | +1.1% | 116.4 / 117.4 | overlap |
| C4 | larson10 | 285.8 | 284.6 | +0.4% | 96.2 / 97.0 | overlap |
| C4 | xmalloc | 331.615 | 336.514 | -1.5% | 54.5 / 53.1 | overlap |
| T2A (old baseline, new `wfe`) | larson16 | 308.5 | 307.8 | +0.2% | 129.3 / 127.6 | overlap |
| T2A | larson10 | 191.4 | 192.4 | -0.5% | 88.0 / 86.3 | overlap |
| T2A | xmalloc | 330.202 | 318.768 | **+3.6%** | 55.7 / **66.9** | disjoint |

### 2.2 Run 2: the same binary at `spin_hint:baseline` and `spin_hint:wfe`

With the key, `base` and `wfe` are one binary and differ only in the
hint; `old` is 9ae5a60. On the two 2 MiB-L2 machines the rows are clean.
On the T2A the `base` and `wfe` variants also carried the geometry
derivation of section 3 (the tree at that moment fitted the ladder to the
1 MiB L2: 512 slots, `tcache_max` 16 KiB), so `base vs old` there is the
geometry, not the lock; `wfe vs base` is the hint alone.

| CPU | row | old | base | wfe | base vs old | wfe vs old | wfe vs base | RSS old / base / wfe MiB |
|---|---|---:|---:|---:|---:|---:|---:|---|
| C4 (no `wfe` on x86) | larson16 | 312.9 | 316.0 | | -1.0% | | | 118.8 / 118.9 |
| C4 | larson10 | 281.5 | 287.1 | | -2.0% | | | 96.0 / 96.3 |
| C4 | xmalloc | 340.730 | 337.982 | | +0.8% | | | 52.8 / 53.1 |
| C4A | larson16 | 598.3 | 595.8 | 597.8 | +0.4% | +0.1% | -0.3% | 169.1 / 166.1 / 166.8 |
| C4A | larson10 | 370.7 | 369.5 | 367.3 | +0.3% | +0.9% | +0.6% | 112.4 / 112.4 / 112.9 |
| C4A | xmalloc | 600.263 | 603.037 | 583.232 | -0.5% | **+2.9%** | **+3.3%** | 54.2 / 54.9 / **71.7** |
| T2A (base, wfe: derived geometry) | larson16 | 309.4 | 300.4 | 302.3 | +3.0% | +2.3% | -0.6% | 127.7 / 116.0 / 115.7 |
| T2A | larson10 | 192.3 | 187.6 | 188.3 | +2.5% | +2.1% | -0.4% | 86.7 / 76.6 / 76.6 |
| T2A | xmalloc | 330.765 | 321.792 | 311.455 | +2.8% | +6.2% | **+3.2%** | 57.9 / 50.7 / **64.4** |

The C4A's `wfe` sorted lists for xmalloc (580.0..587.1 against
602.6..603.6) and the T2A's (307.2..313.9 against 320.7..323.7) are
disjoint; every larson list overlaps.

### 2.3 Reading and decision

- The mechanism costs nothing on the reference shapes: C4 run 1 +1.1 /
  +0.4 / -1.5, run 2 -1.0 / -2.0 / +0.8; C4A +0.4 / +0.3 / -0.5; all lists
  overlap. The C2's run 1 xmalloc (+3.5%, disjoint lists) with the same
  `pause` in both binaries is either code placement or noise the 5 rounds
  did not cover; run 3 (section 2.4) repeats it with the final tree.
- **`wfe` loses xmalloc-test on both aarch64 CPUs**: -2.9..-3.6% with a
  quarter to a third more RSS (T2A 50.7 to 64.4 MiB, C4A 54.9 to 71.7),
  and gains nothing on larson16 or larson10 (the row the item named,
  within noise both ways). Mechanism: xmalloc-test's five freeing threads
  flush into the allocating threads' arenas and contend on the class
  locks; a `wfe` waiter is handed the lock later than a `pause` re-poller
  (the wake-up event, then the CAS race), the allocating threads run
  ahead, and the residue of cached slots grows. `lock::tests::spin_hint_costs`
  (8 threads on one counter, run inside the parallel test suite so
  indicative only) says the same at the micro level: T2A baseline 111.8..122.4
  ns per contended acquire against `wfe` 121.5..251.5; uncontended pairs
  49..72 ns under either, as they never spin.
- Decision: **`auto` never chooses `wfe`**; it stays in the tree behind
  `spin_hint:wfe` (the code path costs nothing when not selected, and a
  power-bound deployment or a CPU with a different wake latency can turn
  it on and measure). `umwait` is chosen by `auto` where WAITPKG is
  exposed; no GCE guest exposes it, so its only evidence is the fallback
  above and the host fixture; it too can be forced off with
  `spin_hint:baseline`. The item's named row, larson16 on two-threads-
  per-core x86 (+6..9% behind jemalloc on the C4, +10.7% on the C2), is
  **not moved** by anything the lock can do without WAITPKG: recorded as
  no gain, code kept at zero cost.

### 2.4 Run 3: the final tree on the Cascade Lake (`old` against `new`, constants everywhere)

The committed tree (the key, `auto` = baseline here, no geometry code),
19:02..19:06 UTC, `2026-09-19-lane-c-spin-c2-2.md`:

| row | old | new | new vs old | RSS old / new MiB | sorted lists |
|---|---:|---:|---:|---|---|
| larson16 | 164.8 | 163.6 | +0.7% | 105.7 / 107.3 | overlap (155.4..178.3 vs 161.3..171.3) |
| larson10 | 125.1 | 128.3 | -2.5% | 78.1 / 79.6 | overlap |
| xmalloc | 237.148 | 230.945 | +2.7% | 52.7 / 52.8 | overlap by one score (231.8..238.9 vs 229.8..234.5) |

xmalloc on the Cascade Lake is 2.7..3.5% behind twice with the same
`pause` in both binaries, where the C4 (same code, two runs) and the
C4A show nothing; the mechanism is one atomic load and a `match` on the
slow path, so the likeliest cause is code placement on this CPU (the
memory note on page aliasing), and it is left open: a sampler split on a
C2 lab (no PMU there) is the next step if the row matters. Larson16, the
item's row, is within noise on every CPU.

## 3. Cache-geometry-sized defaults

### 3.1 The model

The ladder's top (`tcache_nslots_small_max` 1024 for classes up to 128
bytes, halving per doubling, floor 8) and `tcache_max` (32 KiB) were
chosen on the reference CPUs, both 2 MiB L2 per core (Axion 64 KiB L1D,
Emerald Rapids 48 KiB). The fast path touches one line of the header and
one of the slot stack per class whatever the capacity, so the L1D does
not bind (and cannot: 64K and 48K must give the same values). What scales
with the capacity is what a flush or refill of `cap / 2` slots streams:
the half stack (4 KiB at 1024) and the descriptor line and bitmap word of
every slab it touches, at worst one 64-byte line per slot, 32 KiB at 1024
(1.6% of a 2 MiB L2, 3% of 1 MiB, 6% of 512 KiB), which has to stay
L2-resident next to the program's working set for the flush not to miss
per slot; the largest cached class bounds what `MIN_CAP` slots of it keep
warm (8 x 32 KiB at the default `tcache_max`). The rule: the ladder's top
and `tcache_max` halve per full halving of the L2 below 2 MiB (1 MiB: 512
slots and 16 KiB; 512 KiB: 256 and 8 KiB), floors 128 slots and 8 KiB, an
unknown L2 keeps the constants, a key a source set keeps its value, and
2 MiB and above gives the constants exactly (a host test asserted the
ladder class by class on the two reference geometries; the reference
scorecards would not have moved). It was implemented in `config.rs`
(`nslots_for_l2`, `tcache_max_for_l2`, `Config::fit_to_caches` at the end
of `load` from a second `Cpu::probe`, `explicit` bits per key) and
removed again after the numbers below; the T2A run 2 rows of section 2.2
were taken with it in force.

### 3.2 Cascade Lake (32K / 1M): the constants against the derived values, one binary, `MALLOC_CONF`

`fixed` = the constants (1024, 32 KiB); `ladder512` = `tcache_nslots_small_max:512`
(what the rule gives a 1 MiB L2); `ladder256` = one step further;
`both` = `tcache_nslots_small_max:512,tcache_max:16384` (the rule in full).
5 rounds, 18:41..18:53 UTC, `2026-09-19-lane-c-ab-c2-geometry.log`.

| row | unit | fixed | ladder512 | ladder256 | both | 512 vs fixed | 256 vs fixed | both vs fixed | RSS fixed / 512 / 256 / both MiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| larson1 | M ops/s | 23.9 | 23.6 | 22.5 | 23.4 | **+1.3%** (lists 23.8..24.1 vs 23.2..23.6) | **+6.2%** | +2.1% | 14.7 / 14.1 / 14.0 / 14.2 |
| larson10 | M ops/s | 125.6 | 119.2 | 126.6 | 119.9 | +5.4% (noisy: 113.6..129.6 vs 115.4..128.1) | -0.8% | +4.8% | 80.4 / **68.6** / **63.7** / 68.1 |
| valkey1 | M ops/s | 4.151 | 4.124 | 4.070 | 4.126 | +0.7% | +2.0% | +0.6% | 584.9 / 584.7 / 584.7 / 584.7 |
| redis | k req/s | 437.8 | 437.3 | 440.5 | 438.6 | +0.1% | -0.6% | -0.2% | 30.1 / 30.1 / 30.1 / 29.9 |
| alloc-testN | s | 5.274 | 5.240 | 5.264 | 5.231 | -0.6% | -0.2% | -0.8% | 17.3 / 16.9 / 16.4 / 16.8 |

### 3.3 Altra (64K / 1M): `fixed` (`MALLOC_CONF` 1024, 32 KiB) against `derived` (the rule in force)

5 rounds, 18:55..18:59 UTC, the tree with the derivation, `2026-09-19-lane-c-ab-t2a-geometry.log`.

| row | unit | fixed | derived | derived vs fixed | RSS fixed / derived MiB |
|---|---|---:|---:|---:|---|
| larson1 | M ops/s | 21.3 | 20.7 | **+2.9%** (disjoint) | 13.6 / 12.9 |
| larson10 | M ops/s | 193.2 | 186.7 | **+3.5%** (disjoint) | 86.1 / **77.1** |
| valkey1 | M ops/s | 6.438 | 6.236 | **+3.2%** | 584.5 / 584.5 |
| redis | k req/s | 302.3 | 300.6 | +0.6% | 29.2 / 29.2 |
| alloc-testN | s | 3.272 | 3.283 | +0.3% | 16.6 / 15.9 |

### 3.4 Reading and decision

No throughput row improves on either small-cache CPU; larson1 loses
1.3% on the Cascade Lake and 2.9% on the Altra with disjoint lists,
larson10 and valkey1 lose 3.5% and 3.2% on the Altra, and the 256-slot
step loses 6.2% on larson1. What the smaller ladder buys is RSS, 12..15%
on larson10 (the residue of cached slots, a fifth fewer at 512), which the
ladder was never sized for and which `tcache_nslots_small_max:512` already
offers a deployment that wants it. **The derivation is dropped**: the
constants stand on every CPU, `opt.tcache_nslots_small_max` and
`opt.tcache_max` report them, the model above is the record (design 6.2,
"Cache geometry"), and the caches row of design 17 stays "reported only".
The 6.2 flush-rate argument for 1024 (fewer, larger flushes touch fewer
descriptor lines per slot) beats L2 residency on a 1 MiB part as it does
on a 2 MiB one.

## 4. Tests

- Host (macOS, aarch64, rustc 1.94.1): `cargo test --features std`, 284
  passed (the `wfe` path runs in `every_hint_wakes_under_contention`,
  `a_parked_waiter_is_released` and `spin_hint_costs` on this machine).
- `make test` on the VMs, lock change only (run 1): C2 356 + 108 + 23,
  T2A 356 + 108 + 23, C4 356 + 108 + 23, all 0 failed; the container check
  printed `spin_hint=baseline` on the C2 and C4 and `spin_hint=wfe` on the
  T2A. Final tree (the key, `auto` never `wfe`, no geometry code): C2
  357 + 108 + 23, T2A 357 + 108 + 23, C4 357 + 108 + 23, all 0 failed;
  the container checks printed `spin_hint=baseline event_stream=false`
  on the C2 and C4 and `spin_hint=baseline event_stream=true` on the T2A
  and C4A, and the key test `own wfe available=true` on both aarch64 VMs,
  `own umwait available=false` on both x86 VMs. The C4A's first final run
  was 357 + 107/1 + (not run): `api::tests::fork_handlers_take_and_release_every_lock`
  asserted "huge still locked" after `fork_parent`, a snapshot of
  `is_locked()` taken while the other api tests allocate in the same
  process (the same test passed on the C4A's previous run of this tree
  and on the three other VMs); the rerun on the C4A passed, 357 + 108 +
  23, 0 failed.
- The lock's own cost, `lock::tests::spin_hint_costs` (8 threads on one
  counter, 100 000 acquisitions each; run inside the parallel suite, so
  indicative): baseline 57..174 ns per contended acquire across the four
  VMs and runs; `wfe` 121..344 ns on the T2A, 177..210 on the C4A,
  always above the same run's baseline; uncontended pairs 38..72 ns
  under either hint (they never spin).
