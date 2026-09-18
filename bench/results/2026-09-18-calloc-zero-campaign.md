# The calloc-zero reproduction campaign, 2026-09-18 (Phase 1.5 task 7b)

`docs/plan.md` Phase 1.5 task 7 (b); background `docs/review-2026-09-18.md`
sections 1.1 and 1.4. One `make test` at 6a9016e had mimalloc's stress test
abort with `alloc_items: Assertion p[i] == 0 failed` (a `calloc` returned
memory with a non-zero word) and 150 reruns did not reproduce it. This file
records the `check_zero` debug mode that makes the failure observable at
the allocator level, the campaign the plan asks for, and what it says.
Driver: `bench/probes/calloc-zero-campaign.sh` (runs inside the dev
container; knobs `ROUNDS`, `MAX_SECS`, `STANDALONE`, `CONFIGS`, `SKIP_SUITE`
go inside `make run CMD='...'`, `make run` forwards no environment).

## The mode

`MALLOC_CONF=check_zero:true` (off by default, `opt.check_zero` readable;
`Config::check_zero` -> `purge::set_check_zero`). With it on:

- `ClassState::claim`: after the bit loop of every pick flagged zero
  (`Fresh` or `Purged` slab at selection), the first and last word of each
  claimed slot is read; a non-zero word aborts with the slot, class, size,
  slab, arena, the slab's state at selection, the offending offset and word,
  the discard backend and advice, and the descriptor's current state, live
  count, owner, set-bit count and the pick size.
- `Huge::alloc`: when the run is reported zero, the first and last word of
  every usable page is read; a non-zero word aborts with the run, page
  count, prior state of the pages (`fresh`, `Purged`, ...), arena, backend.
- Invariants added while the campaign ran, all on lock-held or cold paths
  and all gated by the same flag: `take` aborts on a `Fresh` slab with
  `live != 0`; `take` on a `Fresh` slab reads the first word of every slot
  before any bit is claimed (`is Fresh before its first claim but slot ...
  reads ...`); `dec_live` aborts on a release into a slab that reads
  `Fresh`; `take_group` aborts if `next_slab` differs from the watermark or
  if a slab about to be initialised has a non-zero shadow/state/live
  (`take_group re-hands slab`); `SlabDesc` carries a shadow byte
  (`state + 1`, written by `init` and `set_state`, the former `_pad`) and
  `take`/`dec_live`/`on_live_zero` abort when it disagrees with the state
  byte (`written outside the API`).

Release fast paths are unchanged: the flag is tested only inside `claim`
(after `plan`), `take`, `dec_live`, `on_live_zero`, `take_group` and
`Huge::alloc`, which all hold a class, table or huge lock or are per-batch;
the shadow store rides on `set_state`/`init` (lock-held). `quick` against
5a069af: see the end of this file.

Unit tests: `class::tests::check_zero_passes_fresh_and_purged_slabs`,
`check_zero_aborts_on_a_dirty_purged_slab` (a purged slab dirtied behind the
allocator's back aborts at the next claim naming `Purged`),
`huge::linux_tests::check_zero_passes_fresh_and_purged_runs`,
`check_zero_aborts_on_a_dirty_purged_run`; `config` key tests (20 keys);
ctests `c_conf_...` with `check_zero:true` and
`mimalloc_test_stress_check_zero` (stress 8 50 10 with the mode on, kept in
`make test` so a recurrence is diagnosable).

## Hosts

| host | arch | kernel | probe line | THP | nproc |
|---|---|---|---|---|---|
| Mac dev container (Docker Desktop) | aarch64 | 6.10.14-linuxkit | `page_size=4096 guard_install=no mseal=no process_madvise=no anon_name=no` | `[always] madvise never` | 10 |
| challoc-bench-arm64 (GCE C4A) | aarch64 | 7.0.0-1011-gcp | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes` | `always [madvise] never` | 16 |
| challoc-bench-amd64 (GCE C4) | x86_64 | 7.0.0-1011-gcp | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes` | `always [madvise] never` | 16 |

Discard backend everywhere: `madvise loop, MADV_DONTNEED` (no
`process_madvise` under the container's seccomp on any of the three). Host
load during the runs: Mac 20..26, VMs 19..20 (the load loop is the point).

Full suite at the campaign tree (step 2 of the script, each host once):
`236 passed` (lib) + `81 passed` (api::) on all three; probe lines above
come from that run.

## Round 1: the protocol as written (300 runs per configuration)

`stress 8 50 10` under a continuous background loop of `cargo test
--features std api::` (the load the one failure needed), every run with
`MALLOC_CONF=check_zero:true` plus the configuration; then `stress 16 100
20` standalone, 20 runs.

| configuration | Mac | arm64 VM | x86_64 VM |
|---|---|---|---|
| `check_zero:true` (default) | 2 / 300 | 1 / 300 | 0 / 300 |
| `+dirty_decay_ms:-1` | 2 / 300 | 0 / 300 | 0 / 300 |
| `+tcache_compact_shift:0` | 0 / 300 | 0 / 300 | 1 / 300 |
| `+narenas:1` | 7 / 300 | 7 / 300 | 3 / 300 |
| `stress 16 100 20` standalone | 0 / 20 | 0 / 20 | 0 / 20 |
| load loop `cargo test api::` (81 tests each) | 29 runs, 0 failed | 25 runs, 0 failed | 24 runs, 0 failed |

Failures are `check_zero` aborts; every one of the 25 is the same shape
(class-level, `Fresh`, word at offset 0; the huge check never fired):

```
challoc: check_zero: slot 0xf5665cc00000 (class 42, size 57344, slab 12, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4fcb9 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xe1a673600000 (class 26, size 3584, slab 32, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e429 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xde2f6d760000 (class 30, size 7168, slab 27, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e699 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xb37eedcb0000 (class 1, size 32, slab 11, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5bd (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xc7e2910c03c0 (class 0, size 16, slab 12, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5b8 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xd33ae0660000 (class 7, size 128, slab 38, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5a9 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
challoc: check_zero: slot 0xce3f10d20000 (class 3, size 64, slab 18, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5b1 (discard backend madvise loop, MADV_DONTNEED) [src/class.rs:1758]
```

The word is always `items ^ cookie` with the stress test's cookie
`0xbf58476d1ce4e5b9` (`tests/mimalloc/test-stress.c:120`): `0x1900` = 6400
items = 51200 B (class 42), `0x190` = 400 = 3200 B (class 26), `0x320` =
800 = 6400 B (class 30), `4` = 32 B (class 1), `1` = 8 B (class 0), `0x10` =
128 B (class 7), `8` = 64 B (class 3), `0x64` = 800 B (class 18, round 3),
`0xc8` = 1600 B (class 22, round 2). That is `alloc_items`'s `p[0] = items ^
cookie`, the first word of a block whose size falls in exactly the class
of the slab it was found in: the slot had already been handed to another
thread as the start of a block of the same class.

With the default arena count every round-1 hit is class 42, the one class
all 8 threads share through arena 0 (above `tcache_max`, direct path); with
`narenas:1` (every class shared by every thread) all sizes fail. The rate
follows the amount of concurrent traffic on one `ClassState`.

## Round 2: the same protocol with the descriptor's state in the message

VMs only (the Mac was still on round 1). `... descriptor now Active live 2
owner 0, 2 of 18 bits set, pick of 1 slots)`: `live` always equalled the
set-bit count in all 12 hits, so the live count and the bitmap were intact
and consistent. Counts: arm64 default 1, nodecay 4, nocompact 1, narenas1
2 (of 300 each); x86_64 0, 3, 0, 0. Load loop: arm64 25 runs 0 failed;
x86_64 24 runs, 1 failed:
`api::mallctl::tests::stats_are_frozen_between_epochs_and_arenas_all_sums`
panicked at `src/api/mallctl.rs:1196` (the `stats.allocated` growth check
under load; unrelated to this task, recorded for the flake list).

## Round 3: `narenas:1` as the base, caches on and off (400 runs each)

With the shadow byte, `fresh_with_live`, `release_into_fresh` and the
watermark check in place.

| configuration | Mac | arm64 VM | x86_64 VM |
|---|---|---|---|
| `check_zero:true,narenas:1` | 11 / 400 | 8 / 400 | 5 / 400 |
| `check_zero:true,narenas:1,tcache:false` | 39 / 400 | 31 / 400 | 22 / 400 |
| load loop | 34 runs, 1 failed | 44 runs, 0 failed | 34 runs, 0 failed |

All 116 aborts were the post-claim dirty-slot check. The shadow never
disagreed with the state byte; no `Fresh` slab ever had `live != 0` at
`take`; no release ever reached a `Fresh` slab; `take_group` never
re-handed a slab. So the descriptor is not corrupted and the slab really
is in its first life when the duplicate appears. Caches off triples the
rate: the mechanism is in the class claim/release machinery, not in the
thread caches.

The Mac's one load-loop failure is the original symptom, verbatim:

```
test api::ctests::mimalloc_test_stress ... FAILED
stderr: challoc-stress-25700-43: /src/tests/mimalloc/test-stress.c:166: alloc_items: Assertion `p[i] == 0' failed.
```

(default configuration, no `check_zero`; 1 of 34 runs under this load).
The plain test sees it only when every pick of a refill was zero-flagged
(`Claim.zeroed` is the AND over picks, so a mixed refill makes `calloc`
memset and hides the dirty slot); `check_zero` tests each pick.

## Round 4: the pre-claim scan (`tcache:false`, 200 runs)

`take` on a `Fresh` slab now reads the first word of every slot under the
class lock before any bit is claimed.

| host | dirty after claim | dirty before first claim | re-hand / watermark / shadow |
|---|---|---|---|
| Mac | 28 / 200 | 0 | 0 |
| arm64 VM | 19 / 200 | 0 | 0 |
| x86_64 VM | 21 / 200 | 0 | 0 |

Every slot of every failing slab read zero under the lock at `take`; a few
instructions later, after this thread's `claim_word` had set its bit, the
slot's first word was another thread's `p[0]`. The enriched line for such a
hit:

```
challoc: check_zero: slot 0xc24f93d30000 (class 3, size 64, slab 19, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5b1 (discard backend madvise loop, MADV_DONTNEED; descriptor at 0xf2a7edff9940 now Active live 524 owner 0, 524 of 1024 bits set, pick of 7 slots) [src/class.rs:1834]
```

(`live 524` is other threads filling the slab after `take` released the
lock; at `take` it was 0 with 0 bits set.)

## Thread count (Mac, no load loop, `check_zero:true,narenas:1,tcache:false`)

| threads | runs | failed | wall |
|---|---|---|---|
| `stress 1 50 10` | 150 | 0 | 1 s (7 ms per run) |
| `stress 2 50 10` | 150 | 0 | 5 s |
| `stress 4 50 10` | 150 | 1 | 11 s |

The 4-thread hit is the one that names the mechanism:

```
challoc: check_zero: slot 0xcb8419010000 (class 0, size 16, slab 1, arena 0) was handed out as zero from a Fresh slab but its word at offset 0 reads 0xbf58476d1ce4e5bb (discard backend madvise loop, MADV_DONTNEED; descriptor at 0xffff83000040 now Active live 1 owner 0, 1 of 4096 bits set, pick of 1 slots) [src/class.rs:1834]
```

`1 of 4096 bits set`: the only bit set in the slab is this thread's own,
yet the slot holds another thread's block start (`2 ^ cookie`, a 16-byte
block). The other thread does not own the slot any more: it allocated it
**and freed it again** between this thread's reservation and this thread's
bit claim.

## The mechanism (named; the fix is part (c))

`ClassState::claim` (`src/class.rs`) is two steps. `plan` takes the class
lock, `take` reserves `n` slots in the slab's `live` count, records
`Pick.zero = state.is_zero()` (`Fresh` or `Purged` at selection) and, when
the reservation does not fill the slab, puts the now-`Active` slab into the
partial set; `plan` returns and **drops the lock**. Only then does `claim`
run the bit loop (`AtomicBitmap::claim_from`) that decides *which* slots the
reservation gets. Between the two steps any other thread's `plan` finds the
slab in the partial set, `take`s its own reservation (not zero-flagged, the
slab is `Active`), claims a slot, hands it to the program, and, when the
program frees it quickly (the stress test frees most blocks within
microseconds), `release_word` clears the bit again while the memory keeps
the program's data. The first thread's `claim_from` then takes that clear
bit, and `Claim.zeroed` (the AND of the picks' `zero`) says the slot reads
zero. It does not. `calloc` skips its `memset` (or the cache's zero prefix
covers the slot), and the program reads the previous owner's first word:
`alloc_items: Assertion p[i] == 0 failed`.

The design's argument (`src/class.rs` header: "a slab that was `Fresh` or
`Purged` at selection is guaranteed zero for the thread that selected it")
holds for the slab's memory at selection time, not for the slots the bit
loop later wins: the bitmap gives no ownership between reservation and bit
claim, so a reservation made on a zero slab can be satisfied by slots
another thread used and freed in the meantime. The window is the unlocked
gap inside one `claim`, widened arbitrarily by preemption, which is why
the failure needed the oversubscribed `make test` host (and why `narenas:1`
and `tcache:false`, which put more threads' one-slot claims and frees on
one `ClassState`, raise the rate to 2..10% of runs). `Purged` picks have
the same window (not seen here only because a 1 s run purges little);
`Huge::alloc` does not (`Huge::take` records the run under the huge lock).
Nothing is handed out twice and no descriptor or bitmap is corrupted: the
campaign's shadow, re-hand, watermark, `Fresh`-with-live and pre-claim-scan
checks were all silent through 184 hits.

Fix shape for part (c) (not landed here; more than a line, touches the
refill's lock scope, needs `quick` on both VMs): decide the slots of a
zero-flagged pick under the lock. In `take`, when `state.is_zero()` (the
slab has `live == 0` and no bits set at that moment, so no contention),
set bits `0..n` directly and record in the `Pick` that its slots are
`0..n`; `claim` writes `slot_ptr(slab, k)` for those picks and runs the bit
loop only for non-zero picks. Alternatively keep a zero slab out of the
partial set until its reserver has claimed. Test that fails on the pre-fix
code: split `claim` into `plan` and a `finish(picks)` (both in-module), run
thread A's `plan` on a fresh class (one zero pick of 1 slot of a 4096-slot
slab), then as thread B `claim` 1 slot (gets slot 0), write `0xAB`,
`release` it, then A's `finish`: pre-fix A gets slot 0 with `zeroed ==
true` and reads `0xAB`; post-fix A's bit was set in `plan`, B gets slot 1,
A's slot reads zero.

## What the campaign excludes

The discard path (`Purged` never appears; `dirty_decay_ms:-1` fails at the
same rate), the compaction path, guard slabs (the Mac has none,
`guard_install=no`), thread caches as the source (rate triples without
them), descriptor corruption (shadow, live and bitmap consistent),
`take_group` re-handing or watermark drift, the huge allocator, double
hand-out (the 4-thread hit has one bit set), and the kernel's zeroing (the
memory is zero under the lock and is dirtied by a same-class block start in
the next microseconds).



## `quick` on the arm64 VM, working tree against 5a069af (go)

All the mode's tests are on lock-held or cold paths (`claim` after `plan`,
`take`, `dec_live`, `on_live_zero`, `take_group`, `Huge::alloc`) and off by
default; the shadow store is one byte next to the state store under the
class lock. `bench/gce/challoc-bench quick arm64 OLD=5a069af`, one round
each, aarch64, nproc 16, load at start 2.82, 118 s (old 41bc0b57b900cf1f,
new 15d34fc7b02bd13b):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---|---|---|---|---|
| larson1 | M ops/s | 42.9 (15.7 MiB) | 42.9 (15.8 MiB) | 45.2 (18.5 MiB) | +0.0% | +5.4% |
| larson10 | M ops/s | 378.5 (113.5 MiB) | 378.3 (113.3 MiB) | 389.6 (132.1 MiB) | +0.1% | +3.0% |
| xmalloc | M frees/s | 563.703 (53.3 MiB) | 565.356 (53.3 MiB) | 476.346 (78.4 MiB) | -0.3% | -15.7% |
| alloc-test1 | s | 2.573 (12.9 MiB) | 2.545 (12.9 MiB) | 2.468 (12.3 MiB) | -1.1% | +3.1% |
| valkey1 | M ops/s | 7.441 (584.2 MiB) | 7.668 (584.1 MiB) | 8.517 (585.4 MiB) | -3.0% | +11.1% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go (new within 5% of old on every row; churn compacts)`. Positive
percentages mean new did worse. No scorecard: the mode is off by default
and changes no default behaviour.

Final tree, Mac container: lib `236 passed`, api `81 passed` (round-3 step
2 plus the round-4 load loop, 12 runs); host `cargo test --features std`
178 passed.
