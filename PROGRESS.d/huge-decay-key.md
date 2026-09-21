## Done

- JVM lull memory (lane huge-decay, 2026-09-20): the `huge_dirty_decay_ms` key
  shipped with today's behaviour as its default, measured, and the JVM gap
  explained; the default is not proposed for change. Code 27f2f59 (`src/config.rs`:
  the key, `-1`..`1000000000000` like `dirty_decay_ms`, `Option<i64>` field, unset =
  follow arena 0's live period, `opt.huge_dirty_decay_ms` reads the period in
  force; `src/heap/shared.rs`: `Shared::huge_decay_ns`, init sets
  `Huge::set_purge_on_free` for `0`; `src/purge.rs`: `decay` walks the huge
  extents on that period through the huge `Decayer`, still scaled by the cgroup
  band; `src/huge/mod.rs`: the flag and `free_run`'s purge-at-free; `print.rs`:
  the `Huge:` and `cgroup policy:` lines name the huge period when it differs;
  `arena.<i>.dirty_decay_ms` unchanged), harness ad72218 (`bench/jvm-bench.sh`:
  `challoc-<label>` variants from `CHALLOC_VARIANTS="<label>=<conf>"`, the
  `challoc-trim` variant; Makefile forwards `CHALLOC_VARIANTS`). Tests: host
  `cargo test --features std` 371/0; container `make test` 475 + 136 + 26, 0
  failed (`config::huge_dirty_decay_ms_follows_dirty_decay_ms_until_set`, the
  numeric-range row, the table test's documented exception, `opt.` read-back,
  `heap::huge_dirty_decay_ms_gives_the_huge_tier_its_own_period`: follows arena 0
  through `set_decay_ms`, 2000 purges a freed run at 2 s while slabs keep 10 s,
  0 at the free, -1 never but `malloc_trim` does). Codegen (aarch64, q16 and q8,
  `scripts/codegen-diff.sh`): every per-allocation function (malloc/free/calloc/
  sdallocx, alloc_fast, alloc_zeroed, pop/push, classify_free, sized_slot,
  checked_slot*, claim_with, take, release_owned, release_word, dec_live) exact or
  reloc-only; changed: `Shared::free_huge` (the huge free path, one relaxed load,
  664 -> 668 insns), `purge::decay`, `api::init`, `print::policy_line`,
  `print::malloc_stats_print`, four `config::TABLE` closures (index shift), and
  `Huge::decay_with` / `Huge::purge_empty` by field offsets only (same counts).
  Measured on `challoc-huge-decay-{arm64,amd64}` (C4A / C4, kernel 7.0.0-1011-gcp,
  Wolfi openjdk-21 21.0.12.1-r2), the JVM mixed-size soak, 1200 s per variant,
  both archs in parallel, `bench/results/2026-09-20-{aarch64,x86_64}-jvm-huge-decay.md`
  (tail mean Rss MiB over the last 600 s / CPU us per request in the high
  windows / blob req/s in the low windows):

  | variant | aarch64 | x86_64 |
  |---|---|---|
  | challoc default (huge follows the 10 s slab period) | 1185 / 53.2 / 35281 | 1121 / 68.8 / 27145 |
  | `huge_dirty_decay_ms:2000` | 1181 (-0.4%) / 54.9 (+3%) / 33760 (-4%) | 1105 (-1.4%) / 71.5 (+4%) / 25571 (-6%) |
  | `huge_dirty_decay_ms:500` | 1169 (-1.4%) / 58.1 (+9%) / 32270 (-9%) | 1093 (-2.5%) / 75.4 (+10%) / 24076 (-11%) |
  | `huge_dirty_decay_ms:0` | 1088 (-8.2%) / 67.2 (+26%) / 29687 (-16%) | 1050 (-6.3%) / 83.7 (+22%) / 22775 (-16%) |
  | glibc `MALLOC_ARENA_MAX=2` | 981 / 49.4 / 36873 | 986 / 64.2 / 28545 |
  | glibc `TrimNativeHeapInterval=5000` | 969 / 50.8 / 36690 | 865 / 63.9 / 28247 |
  | challoc-trim (challoc under `TrimNativeHeapInterval=5000`) | 1116 (-5.8%) / 51.2 (-4%) / 36334 (+3%) | 1081 (-3.6%) / 67.9 (-1%) / 27855 (+3%) |

  `challoc:huge` at the end: 238 / 235 / 171 / 15 MiB (aarch64), 201 / 183 / 109 /
  42 (x86_64) for default / 2000 / 500 / 0. Mechanism (the results files' Reading):
  the extents are not idle in the lulls; at 8 connections the JVM still moves
  ~4 GiB/s of direct buffers, its direct-memory limit forces a GC every ~120 ms,
  the Cleaner frees one cycle's ~200 MiB of huge runs in a burst and the next
  cycle reuses them before any age-based window has a step to purge, so 2000
  changes nothing and anything shorter than the cycle purges and refaults every
  burst (0: 7.5 M / 6.0 M eager purges, 4.2 / 3.3 TB in 1200 s); the "222 MiB
  inside the window" of `2026-09-20-*-jvm-soak.md` was one burst caught by the
  exit snapshot. Valkey guard (same files): `make valkey-soak ALLOCS=challoc
  DURATION=1200`, default against `CHALLOC_CONF=huge_dirty_decay_ms:0` (the
  strongest candidate): x86_64 unchanged (throughput within 0.7%, write p99 1.239 -> 1.247 / 0.975 -> 0.975 ms, VmHWM 577.8 -> 577.8, after settle 129.9 -> 130.2 MiB); aarch64 RSS unchanged (VmHWM 580.5 -> 576.1, settle 131.4 -> 128.3), throughput -2..-3% and write p99 1.047 -> 1.079 / 0.951 -> 1.015 ms, at the edge of the two-soak spread of 2026-09-17 in one run each, with only the dict-rehash frees (a handful of runs per phase) for the key to act on. Quick (`make quick OLD=main`, one round each,
  both archs, all `go`): the code change alone valkey1 +4.7% (aarch64, one
  round) / +1.1% (x86_64); `NEW_CONF=huge_dirty_decay_ms:0` valkey1 -0.2% / +1.1%,
  larson10 -0.2% / +5.0% (at the gate, the other two rounds of the same binaries
  -1.6% / +0.6%); `NEW_CONF=huge_dirty_decay_ms:500` valkey1 +0.5% / +0.8%; the
  three rounds put the aarch64 +4.7% down as noise (the codegen has no
  fast-path change). Docs: design 7 (the huge tier's period and why), design 8
  (one sentence), the design 12 row, `docs/tuning.md` (the key's row after
  `huge_retain_max` in `TABLE` order, 30 rows checked; the JVM section's
  measured table; section 7). VMs deleted.

## Notes for the human

- `huge_dirty_decay_ms`: keep the default (follow `dirty_decay_ms`). No value
  closes most of the JVM gap without paying for it in CPU: 0 closes about half
  of the gap to glibc-arena2 (a quarter of glibc-trim's) for +22..26% CPU per
  request and -16% blob req/s in the lulls; 500 and 2000 are inside the noise on
  RSS and cost 3..10% CPU. The key is right for a process that frees a large
  buffer and does not ask for it again within the window (a phase's buffer);
  the JVM re-requests the same sizes within 120 ms. It is inert for Valkey (the
  guard rows). What would close the gap is not a shorter window but glibc-trim's
  shape: a purge of whatever is free *now* regardless of age, on a timer (every
  5 s), so each burst is purged at most once per period instead of once per
  cycle; `challoc-trim` (the JVM calling challoc's `malloc_trim(0)` every 5 s,
  companion files `…-jvm-huge-decay-trim.md`) measured that on challoc itself:
  tail mean -5.8% (aarch64) / -3.6% (x86_64) against the default, peak -7.6% / -3.1%, at CPU per request -4% / -1% and +3% blob req/s in the lulls, `challoc:classes` 255 -> 167 MiB besides `challoc:huge` 238 -> 81 (the age-blind purge drains the class tier's ~90 MiB of fully empty slabs too); more than `huge_dirty_decay_ms:500` buys at +9..10% CPU, for nothing. A `purge_interval_ms`-style key (the background thread or the tick running `purge_all` every N s, the huge tier and the empty slabs only, no cache flush) is the small experiment that follows from this; the remaining 100..135 MiB to glibc-arena2 are half-occupied slabs and thread caches, which no purge returns. A lull-aware purge would add the trigger: fire the age-blind purge
  when the huge tier's live pages or the pool's thread count fall (the lull's
  start) rather than on a timer, so a steady high window pays nothing and a
  lull gives the memory back once; lane B's files (`src/purge.rs`,
  `src/policy.rs`) own it.
