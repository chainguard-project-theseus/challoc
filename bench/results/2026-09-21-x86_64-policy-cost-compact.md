# Policy cost: the compacting cache off versus on, 2026-09-21, x86_64 (lane policy-cost)

The x86_64 half of `2026-09-21-aarch64-policy-cost-compact.md`, which
has the question, the variants and the reading of the mechanism; this
file records the same rows on the x86_64 VM and reads what differs.
Measurement only, no default changed, nothing under `src/` changed.

- host: `challoc-policy-cost-amd64` (c4-standard-16, Intel Xeon
  Platinum 8581C, Emerald Rapids), 16 vCPUs, Ubuntu 24.04 kernel
  `7.0.0-1011-gcp`, 4 KiB pages, THP `madvise`; container: Wolfi
  (rustc 1.98.1); jemalloc = Wolfi's libjemalloc.so.2 5.3.1-r3. Zone
  us-east4-a. Nothing else ran on the VM.
- commit: `f53418d` on `loop/policy-cost` (harness hooks only; `src/`
  is byte for byte main `2078f67`). One library build, sha256 prefix
  `d4f7099f2b37ad12`, `quantum16`, the same file for both challoc
  variants.
- variants and method: as the aarch64 file (`default` = MALLOC_CONF
  unset, `tcache_compact_shift` 3; `compact0` =
  `MALLOC_CONF=tcache_compact_shift:0`; `jemalloc`; `quick`,
  `quick-perf`, `ab` with 5 rounds, then the Ruby soak; the Valkey soak
  of this topic ran on aarch64 only).

## 1. `quick` (one round): `default` (old) vs `compact0` (new)

| row | unit | old (default) | new (compact0) | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.8 (17.4 MiB) | 45.3 (16.6 MiB) | 46.3 (18.0 MiB) | +1.1% | +2.2% |
| larson10 | M ops/s | 280.4 (95.8 MiB) | 283.1 (96.0 MiB) | 296.6 (93.3 MiB) | -1.0% | +4.8% |
| xmalloc | M frees/s | 352.178 (52.4 MiB) | 339.528 (52.2 MiB) | 340.290 (90.6 MiB) | +3.7% | +0.2% |
| alloc-test1 | s | 3.048 (14.1 MiB) | 3.061 (14.1 MiB) | 2.881 (13.4 MiB) | +0.4% | +6.2% |
| valkey1 | M ops/s | 15.216 (585.3 MiB) | 14.895 (585.1 MiB) | 13.270 (592.1 MiB) | +2.2% | -10.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 6801 (49.2%) | 6798 -> 6675 (49.8%) | new DOES NOT COMPACT (end occupancy >= 90%) | 22075290 ops/s |

Load 3.12 at start, 127 s, `old and new are the same build
(d4f7099f2b37ad12)`. NO-GO on the churn row only, as on aarch64: the
schedule off leaves the churn probe's class at 6801 slabs and 49%
(jemalloc's 4 KiB-page build: 6798 -> 6675, 49.8%), the default drains
it to 3462 slabs at 97% in 8 s.

## 2. `quick-perf`: per-operation counters, whole run

| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | default | 46.31 | 72.9 | 244.0 | 1.642 | 0.002 |
| larson1 | compact0 | 45.37 | 75.0 | 244.1 | 1.625 | 0.001 |
| larson1 | jemalloc | 46.23 | 72.8 | 200.9 | 2.115 | 0.001 |
| valkey1 | default | 24.57 | 152.0 | 235.6 | 3.281 | 0.906 |
| valkey1 | compact0 | 26.41 | 141.9 | 212.0 | 3.275 | 0.904 |
| valkey1 | jemalloc | 28.11 | 133.5 | 152.3 | 3.331 | 0.950 |
| valkey1-free | default | 24.18 | 154.7 | 234.7 | 4.555 | 0.925 |
| valkey1-free | compact0 | 25.68 | 145.5 | 211.4 | 4.586 | 0.932 |
| valkey1-free | jemalloc | 24.80 | 151.6 | 158.7 | 4.697 | 1.205 |

Load 2.32 at start. Per phase of the model (`valkey_model -n 850000 -t 5`):

| row | variant | write M ops/s | delete M ops/s | RSS MiB after delete (live 98 MiB) | VmHWM MiB |
|---|---|---:|---:|---:|---:|
| valkey1 | default | 14.966 | 34.168 | 161.4 | 585.6 |
| valkey1 | compact0 | 15.245 | 37.568 | 354.9 | 585.7 |
| valkey1 | jemalloc | 14.927 | 41.299 | 398.0 | 592.5 |
| valkey1-free | default | 14.907 | 33.450 | 161.5 | 585.7 |
| valkey1-free | compact0 | 15.161 | 36.192 | 358.6 | 585.8 |
| valkey1-free | jemalloc | 13.174 | 36.423 | 392.1 | 592.5 |

Reading: larson's instruction count is identical to the tenth (244.0 vs
244.1), the schedule's mode test being the only thing on its path; the
2 cycles/op between its two runs is one run's noise (the quick and ab
rows have the two variants swapping places). The model's write phase
is the same within 2%. The delete phase pays: 34.2 vs 37.6 M ops/s
(-9%) through `sdallocx`, 33.5 vs 36.2 (-8%) through `free`, 24 fewer
instructions and 10 fewer cycles per operation over the whole run with
the schedule off (235.6 -> 212.0, 152.0 -> 141.9); less than aarch64's
-18% / 33 cycles, the same mechanism. And it buys the same thing: 161
MiB resident after the delete phase for 98 MiB live against 355..359
MiB with the schedule off and 392..398 MiB under jemalloc (whose decay
on this 4 KiB-page build has started returning pages within the 5 s,
unlike on aarch64).

## 3. `ab` (5 rounds, interleaved): `default`, `compact0`, `jemalloc`

Load 1.25 at start, 4.66 at the end, 980 s.

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | default | 44.2 44.9 45.5 46.2 46.4 | 17528 |
| larson1 | compact0 | 45.4 45.6 45.8 46.1 46.4 | 17252 |
| larson1 | jemalloc | 45.0 45.5 46.0 46.3 46.6 | 18696 |
| larson10 | default | 268.2 280.4 281.8 282.9 295.7 | 98916 |
| larson10 | compact0 | 274.4 283.8 287.6 289.3 293.3 | 98428 |
| larson10 | jemalloc | 293.9 300.1 302.0 309.2 311.0 | 96184 |
| larson16 | default | 309.4 312.8 313.1 315.1 316.2 | 120276 |
| larson16 | compact0 | 301.4 311.3 311.4 313.8 317.9 | 120748 |
| larson16 | jemalloc | 346.6 346.8 347.1 347.2 353.9 | 132860 |
| xmalloc | default | 324.215 329.431 337.198 346.643 352.989 | 54232 |
| xmalloc | compact0 | 328.700 331.212 333.694 339.400 340.649 | 54032 |
| xmalloc | jemalloc | 333.313 337.192 338.461 339.352 340.305 | 95304 |
| alloc-test1 | default | 3.078 3.080 3.091 3.111 3.111 | 14252 |
| alloc-test1 | compact0 | 3.063 3.073 3.085 3.086 3.090 | 14212 |
| alloc-test1 | jemalloc | 2.892 2.898 2.898 2.918 2.958 | 13652 |
| alloc-testN | default | 4.351 4.410 4.447 4.509 4.524 | 17796 |
| alloc-testN | compact0 | 4.374 4.377 4.382 4.383 4.419 | 17712 |
| alloc-testN | jemalloc | 4.001 4.038 4.039 4.057 4.100 | 17200 |
| valkey1 | default | 14.999 15.016 15.116 15.171 15.236 | 599788 |
| valkey1 | compact0 | 14.872 14.946 15.067 15.150 15.200 | 599752 |
| valkey1 | jemalloc | 14.420 14.764 14.832 14.924 14.976 | 606784 |
| valkey1-free | default | 14.727 14.856 14.993 15.042 15.247 | 599688 |
| valkey1-free | compact0 | 14.938 15.043 15.079 15.089 15.135 | 599768 |
| valkey1-free | jemalloc | 12.622 13.001 13.023 13.211 13.337 | 606684 |

| program | unit | default | default RSS MiB | compact0 | compact0 RSS MiB | jemalloc | jemalloc RSS MiB | compact0 vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.5 | 17.1 | 45.8 | 16.8 | 46.0 | 18.3 | -0.7% | -1.1% |
| larson10 | M ops/s | 281.8 | 96.6 | 287.6 | 96.1 | 302.0 | 93.9 | -2.0% | -6.7% |
| larson16 | M ops/s | 313.1 | 117.5 | 311.4 | 117.9 | 347.1 | 129.7 | +0.5% | -9.8% |
| xmalloc | M frees/s | 337.198 | 53.0 | 333.694 | 52.8 | 338.461 | 93.1 | +1.1% | -0.4% |
| alloc-test1 | s | 3.091 | 13.9 | 3.085 | 13.9 | 2.898 | 13.3 | -0.2% | -6.2% |
| alloc-testN | s | 4.447 | 17.4 | 4.382 | 17.3 | 4.039 | 16.8 | -1.5% | -9.2% |
| valkey1 | M ops/s | 15.116 | 585.7 | 15.067 | 585.7 | 14.832 | 592.6 | +0.3% | +1.9% |
| valkey1-free | M ops/s | 14.993 | 585.6 | 15.079 | 585.7 | 13.023 | 592.5 | -0.6% | +15.1% |

Positive percentages mean the variant did worse than `default`.

Reading: every row within 2% with interleaved lists; the model's
write-phase rows are equal (+0.3%, -0.6%). Nothing the `ab` tier
measures moves with the schedule; the cost is confined to the phase in
which the schedule works (section 2).

## 4. The churn probe

From section 1: `default` 6801 -> 3462 slabs (97.0%), `compact0` 6801
-> 6801 (49.2%), jemalloc 6798 -> 6675 (49.8%). As on aarch64.

## 5. Valkey soak

Ran on aarch64 only (`2026-09-21-aarch64-policy-cost-compact.md`,
section 5).

## 6. Ruby soak (600 s, 16 threads): `default` vs `compact0`

`bench/ruby-soak.sh` (Ruby 3.4.10 `--with-jemalloc` on the compat
library, `bench/ruby/soak.rb`, 16 threads, seed 4141, full GC every
30 s, `GC.compact` at 300 s), one process per allocator in sequence on
the idle VM (load 1.00 throughout: the GVL). `default` and jemalloc from
one invocation (`2026-09-21-x86_64-policy-cost-ruby-soak-default.md`),
`compact0` from a second with `CHALLOC_CONF=tcache_compact_shift:0`
(`...-ruby-soak-compact0.md`); the same stage build `d4f7099f2b37ad12`
under the jemalloc soname for both challoc runs.

| measure (MiB) | default | compact0 | jemalloc | compact0 vs default |
|---|---:|---:|---:|---:|
| peak sampled VmRSS | 417.9 | 417.4 | 179.1 | -0.1% |
| VmHWM at the end | 419.1 | 419.2 | 186.9 | +0.0% |
| at t=60 | 309.1 | 310.0 | 73.6 | +0.3% |
| at t=295 (before GC.compact) | 349.8 | 351.7 | 119.2 | +0.5% |
| at t=300 (after GC.compact) | 361.9 | 363.1 | 127.0 | +0.3% |
| at t=600 | 414.0 | 415.6 | 133.9 | +0.4% |
| lowest right after a periodic full GC | 302.4 | 301.1 | 73.5 | -0.4% |
| allocator: allocated / active / resident / retained, final | 106.0 / 212.9 / 417.3 / 66.3 | 106.4 / 212.1 / 418.8 / 65.2 | 94.8 / 107.3 / 123.4 / 94.7 | |
| jobs (600 s) | 151624 | 151776 | 151262 | +0.1% |
| GC count (major) | 16211 (1301) | 16558 (1421) | 16319 (1358) | |

Per class over the 17 arenas (`bench/probes/mstats-classes.sh` on the
`-challoc-mstats.txt` reports; 1280..4096 B, the long-lived and burst
classes): `default` 46.2% occupancy, 153.6 MiB of slabs for 71.0 MiB
live, `c_on` 0 in every class, `gated` 231..1501 per class; `compact0`
46.5%, 152.9 MiB for 71.1 MiB live, `c_on` 0, `gated` 0 (no probe ever
runs). Reading: as the 2026-09-19 soak found, the schedule never
engages under Ruby (every probe of a fragmented class is declined by
the refill-rate gate: the bursts refill the caches faster than
`COMPACT_MAX_REFILL_SLABS`), so turning it off changes nothing here:
RSS within 0.5% at every sample, allocator stats within 1.5 MiB, the
same job count. The Ruby RSS gap to jemalloc (+209% at t=600) is the
17-arena inventory of `2026-09-19-x86_64-ruby-soak.md`, untouched by
either setting.

## 7. Comparison and the trade

PENDING (the aarch64 file has the joint reading).
