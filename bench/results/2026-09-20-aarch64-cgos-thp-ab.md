# THP on a THP=`always` host, same host, same run: the four `thp` settings and jemalloc on cgos-arm64 (Phase 2.5 task 14/16, lane E)

Date 2026-09-20 (06:36..07:00 UTC). The same-host A/B that section 5 of
`2026-09-20-chainguard-os-cells.md` asked for: on the 6.18 Chainguard OS
cell the challoc-vs-jemalloc gap on the Valkey model rows read +6.4%
(`valkey1`) / +4.2% (`valkey1-free`) on 2026-09-19 at `4864d4b` and +11.7%
/ +12.9% on 2026-09-20 at `e0fdf71` (library `0c60b1a`), on different
VMs and days; the one merged change on that mechanism between the two is
task 16's first half (`thp:never` now issues `MADV_NOHUGEPAGE` on every
reservation, cdca1f1/71f54e0). This file measures the four `thp` settings
and jemalloc interleaved in one run on one host at the merged tree
(`f2d9558`, library and cell code alike; nothing under `src/` differs).

## 1. Host, image, tree, commands

| | |
|---|---|
| VM | `challoc-lane-e-matrix-cgos-arm64`, c4a-standard-8 (8 vCPU Axion, Neoverse-V2: `CPU implementer 0x41 part 0xd4f`), `us-east4-a`, created 06:36 UTC by `challoc-bench matrix lab cgos-arm64` and deleted at the end (`matrix clean`) |
| image | `kirkland-server-hypervisor-arm64-20260916-1358` (project `wolfi-vm`), the same image family as both earlier cells |
| kernel | `6.18.49-r2-gcp-6.18 #Chainguard SMP PREEMPT_NONE` |
| glibc (host) | `glibc-2.44-r6` (the container-built binaries run natively on it) |
| **host THP, as shipped and not written by this run** | `enabled [always] madvise never`; `defrag always defer defer+madvise [madvise] never`; `hugepages-2048kB always [inherit] madvise never`; every mTHP size 16kB..1024kB `never`; `shrink_underused 1`; `khugepaged: max_ptes_none=511 pages_to_scan=4096 scan_sleep_millisecs=10000 alloc_sleep_millisecs=60000 defrag=1` (a huge page is taken at fault time inside one VMA, never by a compaction stall; the daemon at the shipped pace collapses nothing inside a 10 s run: `pages_collapsed` 8 -> 14 over the whole session, the host's own mappings) |
| tree | `f2d9558` (main; `loop/lane-e` fast-forwarded onto it). The library is `f2d9558`'s `cargo build --release` in the `challoc-dev:arm64` image built on the VM (`make image` 50 s, `make build` 8 s, `make bench-build` 16 s, all rc 0) |
| script | `bench/gce/thp-ab.sh` (this iteration; `prep` then `run`), launched as one transient systemd unit `challoc-thpab-1789886403.service` (a plain ssh job dies with the session on this OS); log `~/logs/thp-ab.log`, per-run outputs `~/thp-ab-out/` |
| rows | `valkey_model -n 850000 -t 10 -s` (valkey1), `valkey_model -n 850000 -t 10 -s -f free` (valkey1-free), `larson 5 8 1000 5000 100 4141 16` (larson16), the `bench/lib.sh` arguments; `~/native/target/bench-bin/` holds `rt larson valkey_model libchalloc.so libjemalloc.so.2 thp-ab-thp thp-ab-features` copied out of the container |
| protocol | one discarded warm-up round, then **7 timed rounds**, interleaved round > row > setting (so every setting sees the same drift); each run `env [MALLOC_CONF=<conf>] LD_PRELOAD=<lib> <row>` natively as the login user; per run the score (`write_mops` / larson `Throughput`/1e6), peak `Rss` and peak `AnonHugePages` from `/proc/<pid>/smaps_rollup` sampled every 200 ms, `THP_enabled` from `/proc/<pid>/status` about 1 s in, and the deltas of `thp_fault_alloc thp_fault_fallback thp_split_page thp_deferred_split_page` in `/proc/vmstat` around the run (host-wide; nothing else ran on the VM) |
| settings | (a) `MALLOC_CONF` unset = `thp:never` (the shipped default; `MADV_NOHUGEPAGE` on the three reservations since task 16); (b) `thp:auto` (no advice either way on this host: the pre-task-16 behaviour, see the report line below); (c) `thp:always` (`MADV_HUGEPAGE`d 2 MiB chunks and huge runs); (d) `thp:never,thp_process:advised` (the task 16 prctl on top of the default); (j) jemalloc 5.3 (`/usr/lib/libjemalloc.so.2` of the dev image), no conf |
| load | `/proc/loadavg` 1.80 at start, 1.18..6.92 during the runs (the 16-thread larson rows raise the 1-minute average that the following Valkey rows then see decaying; interleaving puts every setting under the same pattern), 1.41 at the end |
| exits | 120 of 120 runs exit 0 |

The log's header, verbatim:

```
# thp-ab on challoc-lane-e-matrix-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, c4a-standard-8, image kirkland-server-hypervisor-arm64-20260916-1358, 2026-09-20T06:41:18Z, tree f2d9558
# glibc: GNU C Library (glibc-2.44-r6) stable release version 2.44.
# cpu: CPU implementer	: 0x41 CPU part	: 0xd4f
# host THP (as shipped, not written by this script): enabled [always] madvise never; defrag always defer defer+madvise [madvise] never; hugepages-2048kB always [inherit] madvise never; shrink_underused 1
# mTHP sizes: 1024kB=never 128kB=never 16kB=never 2048kB=inherit 256kB=never 32kB=never 512kB=never 64kB=never
# khugepaged: alloc_sleep_millisecs=60000 defrag=1 full_scans=4 max_ptes_none=511 max_ptes_shared=256 max_ptes_swap=64 pages_collapsed=14 pages_to_scan=4096 scan_sleep_millisecs=10000 
# rows: valkey1 valkey1-free larson16; settings: a b c d j; 7 timed rounds after one warm-up, interleaved round > row > setting
# valkey_model -n 850000 -t 10; larson 5 8 1000 5000 100 4141 16
# load at start: 1.80 0.51 0.17
# a: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
# j: /home/dlorenc/native/target/bench-bin/libjemalloc.so.2 mapped=4
```

## 2. What each setting is on this host: the `thp:` report line, the VMA flags, the feature line (`tests/c/thp.c` and `tests/c/features.c` natively, verbatim)

```
#### the thp: report line, the feature line and the kernel's view per setting (tests/c/thp.c and features.c, natively) (06:41:18)
-- setting a: challoc default (thp:never); MALLOC_CONF=''; thp-ab-thp never
  thp never: opt.thp=never sysfs thp=always (enabled file: always) mthp=2M pmd 2048 kB (page 4096) pmd_enabled=1 sub_pmd 0 kB
  thp never: thp_except_advised=yes stats.thp.process=0 PR_GET_THP_DISABLE=0 THP_enabled=1
  thp never: report `thp: never -> off (thp:never) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)`
  thp never: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 nh=1 THPeligible=0 AnonHugePages=0 kB
  thp never: huge run 8 MiB: VMA hg=0 nh=1 THPeligible=0 AnonHugePages=0 kB (global thp always)
  thp never: global THP is always: AnonHugePages 0 kB (class region), 0 kB (huge run) after 8 + 8 MiB touched: the both-ways row
  ok thp
  features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no populate_write=yes thp=always mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=28 mm_cid=yes slice_ext=no-slice_ctrl mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
-- setting b: challoc thp:auto; MALLOC_CONF='thp:auto'; thp-ab-thp auto
  thp auto: opt.thp=auto sysfs thp=always (enabled file: always) mthp=2M pmd 2048 kB (page 4096) pmd_enabled=1 sub_pmd 0 kB
  thp auto: thp_except_advised=yes stats.thp.process=0 PR_GET_THP_DISABLE=0 THP_enabled=1
  thp auto: report `thp: auto -> off (no folio size below the PMD is enabled in sysfs) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)`
  thp auto: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 nh=0 THPeligible=1 AnonHugePages=0 kB
  thp auto: huge run 8 MiB: VMA hg=0 nh=0 THPeligible=1 AnonHugePages=8192 kB (global thp always)
  thp auto: inert on this host (no size below the PMD enabled)
  ok thp
  features: 1 failures
-- setting c: challoc thp:always; MALLOC_CONF='thp:always'; thp-ab-thp always
  thp always: opt.thp=always sysfs thp=always (enabled file: always) mthp=2M pmd 2048 kB (page 4096) pmd_enabled=1 sub_pmd 0 kB
  thp always: thp_except_advised=yes stats.thp.process=0 PR_GET_THP_DISABLE=0 THP_enabled=1
  thp always: report `thp: always -> chunks (active) slab_min 1 pmd 2097152 meta off huge on class_ranges 4 (8388608 bytes, 4 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)`
  thp always: mode 2 class_ranges 9 class_bytes 18874368 failures 0; VMA hg=1 nh=0 THPeligible=1 AnonHugePages=10240 kB
  thp always: huge run 8 MiB: VMA hg=1 nh=0 THPeligible=1 AnonHugePages=8192 kB (global thp always)
  ok thp
  features: 1 failures
-- setting d: challoc thp:never,thp_process:advised; MALLOC_CONF='thp:never,thp_process:advised'; thp-ab-thp process
  FAIL tests/c/thp.c:381: nh under thp:auto (MADV_NOHUGEPAGE is never's)
  thp: 1 failures
  thp process: opt.thp=never sysfs thp=always (enabled file: always) mthp=2M pmd 2048 kB (page 4096) pmd_enabled=1 sub_pmd 0 kB
  thp process: thp_except_advised=yes stats.thp.process=1 PR_GET_THP_DISABLE=3 THP_enabled=1
  thp process: report `thp: never -> off (thp:never) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process advised: PR_THP_DISABLE_EXCEPT_ADVISED in force`
  thp process: accepted (6.18+); THP_enabled reads 1 under PR_SET_THP_DISABLE(1, EXCEPT_ADVISED)
  thp auto: forced to never by a cgroup limit under 1 GiB; only the never checks apply
  thp never: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 nh=1 THPeligible=0 AnonHugePages=0 kB
  thp never: huge run 8 MiB: VMA hg=0 nh=1 THPeligible=0 AnonHugePages=0 kB (global thp always)
  thp process: host always, prctl in force: AnonHugePages 0 kB (class region), 0 kB (huge run), THPeligible 0 / 0: the both-ways row
  thp never: global THP is always: AnonHugePages 0 kB (class region), 0 kB (huge run) after 8 + 8 MiB touched: the both-ways row
  features: 1 failures
```

Reading them: (a) `never -> off (thp:never)`, the class region and the
8 MiB huge run carry `nh` and `THPeligible 0`, `AnonHugePages 0 kB` after
8 + 8 MiB touched on this `always` host: the task 16 advice is in force.
(b) `auto -> off (no folio size below the PMD is enabled in sysfs)`,
`nh=0 THPeligible=1`, and the unadvised 8 MiB huge run reads
**`AnonHugePages=8192 kB`**: `auto` is inert as a mode on this host and
issues no `MADV_NOHUGEPAGE`, so its regions are exactly the pre-task-16
regions, eligible for the host's PMD pages wherever a whole aligned
2 MiB range sits inside one VMA (the huge runs; the class regions are
committed a few slabs at a time and get none, yesterday's section 6).
(c) `always -> chunks (active) slab_min 1 pmd 2097152 meta off huge on`,
`hg=1`, 10 MiB of AnonHugePages on the 8 MiB class test and 8 MiB on the
huge run. (d) `never -> off (thp:never) ...; host always process advised:
PR_THP_DISABLE_EXCEPT_ADVISED in force`, `thp_except_advised=yes`,
`stats.thp.process=1`, **`PR_GET_THP_DISABLE=3`**, the regions `nh=1
THPeligible=0`, 0 kB either way: the kernel took the prctl (the first
`always` host to accept it in this repository; lane B's 7.0 VMs are
`madvise` hosts and the Mac's 6.10 refuses it). `THP_enabled` in
`/proc/<pid>/status` reads **1 under every setting, (d) included**, on
this 6.18 kernel (the field means "not disabled completely", as lane B
read on 7.0); `PR_GET_THP_DISABLE` is the discriminator (3 against 0).

Two lines in that block are the C tests' own expectations, not the
library: `FAIL tests/c/thp.c:381: nh under thp:auto` under (d) is
`thp.c process` assuming `thp:auto` next to the prctl (the test's
documented pairing) while this run deliberately paired it with
`thp:never`, so the `nh` it complains about is the one (a) also has; and
`features: 1 failures` under (b), (c), (d) is `features.c`'s default-conf
checks (`collapse has value 2M ... under thp:never` at line 125: under
`thp:auto|always` the line reads `collapse=2M`, the probe having run;
and at line 136 under (d) `PR_GET_THP_DISABLE reads 3 (the probe must
restore 0)`, which is the prctl in force as asked). The feature line
under (b) and (c), verbatim:

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no populate_write=yes thp=always mthp=2M collapse=2M thp_except_advised=yes rseq=yes rseq_size=28 mm_cid=yes slice_ext=no-slice_ctrl mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

## 3. The table (rounds 1..7, sorted lists and medians; the script's summary, verbatim)

Scores are M ops/s (`write_mops` for the Valkey model, larson throughput
/ 1e6); `vs jemalloc` is challoc's median against jemalloc's, positive =
challoc worse; RSS and AnonHugePages are the per-run peaks, medians over
the 7 runs (max in parentheses for AnonHugePages); the counters are the
per-run `/proc/vmstat` deltas, medians.

| row | setting | sorted scores | median | vs jemalloc | median peak Rss MiB | median peak AnonHugePages MiB (max) | median thp_fault_alloc | median thp_deferred_split_page | THP_enabled |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | a | 7.604 7.767 7.897 8.048 8.615 8.646 8.993 | 8.048 | +16.1% | 584.9 | 0.0 (0.0) | 12 | 0 | 1 |
| valkey1 | b | 8.093 8.274 8.351 8.983 9.358 9.413 9.756 | 8.983 | +6.3% | 584.9 | 32.0 (32.0) | 32 | 0 | 1 |
| valkey1 | c | 8.768 8.923 9.278 9.288 9.737 10.055 10.060 | 9.288 | +3.2% | 640.2 | 636.0 (636.0) | 333 | 281 | 1 |
| valkey1 | d | 7.750 7.841 8.119 8.418 8.657 8.715 8.861 | 8.418 | +12.2% | 584.9 | 0.0 (0.0) | 12 | 0 | 1 |
| valkey1 | j | 8.691 8.739 8.911 9.592 9.807 10.355 10.451 | 9.592 | - | 596.7 | 584.0 (586.0) | 305 | 16 | 1 |
| valkey1-free | a | 7.702 7.825 8.094 8.251 8.271 8.540 8.729 | 8.251 | +9.0% | 584.9 | 0.0 (2.0) | 12 | 0 | 1 |
| valkey1-free | b | 7.726 8.086 8.834 8.975 9.081 9.351 9.548 | 8.975 | +1.1% | 584.9 | 32.0 (32.0) | 32 | 0 | 1 |
| valkey1-free | c | 8.105 8.942 9.333 9.352 9.506 9.602 9.937 | 9.352 | -3.1% | 640.2 | 636.0 (636.0) | 333 | 281 | 1 |
| valkey1-free | d | 7.207 7.943 7.949 8.036 8.467 8.702 8.875 | 8.036 | +11.4% | 584.9 | 0.0 (0.0) | 12 | 0 | 1 |
| valkey1-free | j | 8.812 8.942 9.012 9.072 9.847 9.860 9.900 | 9.072 | - | 596.7 | 584.0 (586.0) | 304 | 20 | 1 |
| larson16 | a | 307.5 307.9 307.9 307.9 307.9 308.0 308.1 | 307.9 | +5.5% | 175.0 | 2.0 (4.0) | 9 | 0 | 1 |
| larson16 | b | 306.0 306.8 307.0 307.2 308.5 308.9 309.7 | 307.2 | +5.7% | 179.7 | 4.0 (6.0) | 12 | 0 | 1 |
| larson16 | c | 305.1 306.3 306.4 307.0 308.4 308.7 309.4 | 307.0 | +5.8% | 244.7 | 188.0 (194.0) | 112 | 36 | 1 |
| larson16 | d | 300.9 302.5 303.0 303.6 304.1 304.9 305.4 | 303.6 | +6.8% | 171.8 | 0.0 (0.0) | 10 | 0 | 1 |
| larson16 | j | 324.5 324.6 325.6 325.8 326.1 327.4 327.5 | 325.8 | - | 270.5 | 170.0 (174.0) | 106 | 35 | 1 |

`thp_fault_fallback` and `thp_split_page` were 0 in every run (not shown;
the RUN lines in section 6 have them).

Derived from the medians (ratios of the medians above: in the first
three columns positive = the first-named setting is faster; the `vs
jemalloc` columns keep the table's convention, positive = challoc worse;
the RSS column positive = (c) larger):

| row | (a) vs (b): the task 16 advice alone | (d) vs (a): the prctl on top of the default | (c) vs (b): challoc's own advice | (a) vs jemalloc | (b) vs jemalloc | (c) vs jemalloc | (d) vs jemalloc | peak Rss (c) vs (a) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | -10.4% | +4.6% | +3.4% | +16.1% | +6.3% | +3.2% | +12.2% | +9.5% (584.9 -> 640.2 MiB) |
| valkey1-free | -8.1% | -2.6% | +4.2% | +9.0% | +1.1% | -3.1% | +11.4% | +9.5% (584.9 -> 640.2 MiB) |
| larson16 | +0.2% | -1.4% | -0.1% | +5.5% | +5.7% | +5.8% | +6.8% | +39.8% (175.0 -> 244.7 MiB) |

## 4. The reading

1. **(a) vs (b): the task 16 advice costs the Valkey model on this host,
   and it explains the movement.** With everything else equal (same
   binary, same host, interleaved), `thp:never`'s `MADV_NOHUGEPAGE` takes
   `valkey1` from 8.983 to 8.048 M ops/s
   (-10.4%) and `valkey1-free` from
   8.975 to 8.251
   (-8.1%); larson16 does not move
   (307.2 -> 307.9). Against jemalloc the
   same rows read (b) +6.3% / +1.1% and
   (a) +16.1% / +9.0%: yesterday's cell
   at the pre-task-16 tree read +6.4% / +4.2% and today's cell with the
   advice +11.7% / +12.9%, a 5..9-point widening that the same-host pair
   reproduces (8..10 points here at 7 rounds; the cells are 3-round
   tiers). The mechanism is the 32 MiB the pre-task-16 regions took (the
   `AnonHugePages` column: 32.0 MiB under (b), 0 under (a); `thp_fault_alloc`
   32 vs 12 per run: 20 more PMD faults over the run, 40 MiB of huge
   runs of which 32 MiB are resident at the peak, the model's tables in
   challoc's huge region), which the advice now forbids; the table lookups over those pages pay 4 KiB TLB reach
   instead of 2 MiB. This number favours anything but the shipped default
   on an `always` host: (b)'s behaviour (no `MADV_NOHUGEPAGE`) or (c).
2. **(c) is parity with jemalloc for the same RSS premium as yesterday.**
   `thp:always` reads +3.2% on `valkey1` and
   -3.1% on `valkey1-free` against jemalloc (both
   inside the 7-round spread: the two sorted lists overlap over most of
   their range, 8.77..10.06 against 8.69..10.45),
   with 636 MiB of AnonHugePages on the 585 MiB heap against jemalloc's
   584 MiB, and peak RSS 584.9 -> 640.2 MiB
   (+9.5%; jemalloc 596.7), 281
   deferred splits per run (challoc's purges splitting its 2 MiB pages;
   jemalloc 16..20 on this host, 269 on the x86_64 host yesterday).
   larson16 under (c): 307.0 against (a) 307.9
   (no gain) for 175.0 -> 244.7 MiB of
   retained memory (+39.8%; jemalloc
   270.5 MiB with 170 MiB of huge pages). Yesterday's file
   had `thp:always` at 7.39 vs jemalloc 7.38 (one run each, +9.5% RSS,
   +38% larson16 RSS); the 7-round medians say the same. These numbers
   favour (c) for the Valkey model's throughput at a fixed RSS cost and
   (a)/(b) for larson's retained memory.
3. **(d) does what it says and changes nothing measurable over (a).** With
   the prctl in force `valkey1` reads 8.418 against (a)'s
   8.048 (+4.6%), `valkey1-free`
   8.036 vs 8.251
   (-2.6%), larson16 303.6
   vs 307.9 (-1.4%); the sorted
   lists overlap over their whole range on the Valkey rows, so the three
   differences are the spread, in both directions. The counters agree:
   0 AnonHugePages and 12 (10) PMD faults per run under both, and (d)'s
   larson16 peak RSS 171.8 vs (a)'s 175.0 MiB is
   the process's non-challoc mappings (libc, stacks: the 2..4 MiB of
   AnonHugePages (a) still shows on larson16 at its peak is exactly what
   the prctl removes, and (d) reads 0.0 there). As expected: (a) already
   advises every challoc region, so the prctl has only the program's own
   and libc's mappings left to act on. On this host (d) is the belt to
   (a)'s braces; where the key is (b) or (c) it would be the only thing
   keeping unadvised ranges out of the host's `always`, which this run
   did not measure.

Not a verdict on the default: the numbers say the shipped default costs
the Valkey model 8..10% on an `always` host relative to the tree before
task 16 for no RSS gain on that heap (585 MiB either way; 32 MiB of huge
pages fewer), `thp:always` buys parity with jemalloc for +9.5% RSS on
that heap and +40% on larson's retained memory, and the prctl is free.
The `thp` default decision in PROGRESS.md ("Phase 2.5 decisions for the
human (coordinator, 2026-09-20)", item 2) has these rows now; the options
listed there (keep `never`; `always` only where the host reads
`thp=always`; a size-banded `always`) each have a row above that favours
it, and a fourth is visible in the (a)/(b) pair: keep `never` for the
class regions but leave the huge runs eligible (the 32 MiB that moved
here are huge runs), which is lane B's file.

## 5. Notes

- The spread: the Valkey rows' 7-run lists span 12..18% on this 8-vCPU
  VM (e.g. (a) 7.604..8.993), wider than the C4A reference pair's; the
  interleaving is what makes the medians comparable, and the (a)/(b) gap
  (10.4%) is of the order of the spread, so it is read from 7 interleaved
  rounds, not from any one pair. The 3-round cell tier that flagged the
  movement is noisier still, which is why the two cells' +6.4/+11.7 could
  not be read alone.
- The one 2.0 MiB `AnonHugePages` max under (a) on `valkey1-free` (one
  run) and the 2..4 MiB on larson16 under (a) are outside challoc's
  regions (the program's own mappings under the host's `always`; (d)
  reads 0.0 on both, the prctl having covered them).
- `bench/gce/challoc-bench`: `matrix ssh <cell>` (and `matrix sync`) took
  the OS Login posix user for every cell, which the server-hypervisor
  image refuses (it has the gcloud user through the `ssh-keys` metadata,
  runbook section 1); `matrix_lab_dest` now picks the user the way
  `matrix_cell` does (by image family). Found when the first `matrix ssh
  cgos-arm64` of this iteration was refused; fixed in the same commit.
- Nothing under `src/` changed; the library measured is `f2d9558`'s.

## 6. Every run (the RUN lines, verbatim; round 0 is the discarded warm-up; KiB; `secs` wall per run; `load` the 1-minute average at the end of the run)

```
RUN round=0 row=valkey1 set=a score=7.828 rss_kib=598840 ahp_kib=0 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.4 load=1.76
RUN round=0 row=valkey1 set=b score=8.694 rss_kib=598956 ahp_kib=32768 thp_enabled=1 fault_alloc=38 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.2 load=1.59
RUN round=0 row=valkey1 set=c score=9.093 rss_kib=655528 ahp_kib=651264 thp_enabled=1 fault_alloc=328 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.3 load=1.50
RUN round=0 row=valkey1 set=d score=7.591 rss_kib=598848 ahp_kib=0 thp_enabled=1 fault_alloc=14 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=1.42
RUN round=0 row=valkey1 set=j score=8.494 rss_kib=611020 ahp_kib=598016 thp_enabled=1 fault_alloc=304 fault_fallback=0 split=0 deferred_split=23 exit=0 secs=10.3 load=1.36
RUN round=0 row=valkey1-free set=a score=7.720 rss_kib=598848 ahp_kib=0 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=1.30
RUN round=0 row=valkey1-free set=b score=8.689 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=1.25
RUN round=0 row=valkey1-free set=c score=8.838 rss_kib=655512 ahp_kib=651264 thp_enabled=1 fault_alloc=336 fault_fallback=0 split=0 deferred_split=278 exit=0 secs=10.3 load=1.21
RUN round=0 row=valkey1-free set=d score=8.181 rss_kib=598912 ahp_kib=2048 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=1.18
RUN round=0 row=valkey1-free set=j score=9.233 rss_kib=611032 ahp_kib=598016 thp_enabled=1 fault_alloc=306 fault_fallback=0 split=0 deferred_split=20 exit=0 secs=10.3 load=1.23
RUN round=0 row=larson16 set=a score=307.7 rss_kib=183196 ahp_kib=4096 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=2.58
RUN round=0 row=larson16 set=b score=303.9 rss_kib=179092 ahp_kib=6144 thp_enabled=1 fault_alloc=20 fault_fallback=0 split=0 deferred_split=4 exit=0 secs=7.2 load=3.43
RUN round=0 row=larson16 set=c score=306.1 rss_kib=251760 ahp_kib=182272 thp_enabled=1 fault_alloc=116 fault_fallback=0 split=0 deferred_split=33 exit=0 secs=7.2 load=4.52
RUN round=0 row=larson16 set=d score=300.8 rss_kib=172324 ahp_kib=0 thp_enabled=1 fault_alloc=0 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=5.15
RUN round=0 row=larson16 set=j score=324.5 rss_kib=282276 ahp_kib=167936 thp_enabled=1 fault_alloc=109 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=6.02
RUN round=1 row=valkey1 set=a score=7.604 rss_kib=598848 ahp_kib=0 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=5.25
RUN round=1 row=valkey1 set=b score=8.274 rss_kib=598912 ahp_kib=32768 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=4.59
RUN round=1 row=valkey1 set=c score=9.278 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=334 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.3 load=4.04
RUN round=1 row=valkey1 set=d score=8.119 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=5 exit=0 secs=10.3 load=3.57
RUN round=1 row=valkey1 set=j score=8.911 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=311 fault_fallback=0 split=0 deferred_split=15 exit=0 secs=10.3 load=3.18
RUN round=1 row=valkey1-free set=a score=7.702 rss_kib=598844 ahp_kib=0 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.69
RUN round=1 row=valkey1-free set=b score=7.726 rss_kib=598848 ahp_kib=32768 thp_enabled=1 fault_alloc=32 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.43
RUN round=1 row=valkey1-free set=c score=8.105 rss_kib=655512 ahp_kib=651264 thp_enabled=1 fault_alloc=329 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.3 load=2.21
RUN round=1 row=valkey1-free set=d score=7.207 rss_kib=598832 ahp_kib=0 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.03
RUN round=1 row=valkey1-free set=j score=9.072 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=314 fault_fallback=0 split=0 deferred_split=22 exit=0 secs=10.3 load=2.03
RUN round=1 row=larson16 set=a score=307.9 rss_kib=179244 ahp_kib=2048 thp_enabled=1 fault_alloc=6 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.3 load=3.23
RUN round=1 row=larson16 set=b score=307.0 rss_kib=184580 ahp_kib=4096 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=4.06
RUN round=1 row=larson16 set=c score=305.1 rss_kib=260388 ahp_kib=198656 thp_enabled=1 fault_alloc=123 fault_fallback=0 split=0 deferred_split=42 exit=0 secs=7.2 load=5.25
RUN round=1 row=larson16 set=d score=304.1 rss_kib=178136 ahp_kib=0 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=6.35
RUN round=1 row=larson16 set=j score=325.6 rss_kib=262492 ahp_kib=147456 thp_enabled=1 fault_alloc=95 fault_fallback=0 split=0 deferred_split=37 exit=0 secs=7.1 load=6.56
RUN round=2 row=valkey1 set=a score=8.048 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=15 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=5.70
RUN round=2 row=valkey1 set=b score=8.983 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=28 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=5.06
RUN round=2 row=valkey1 set=c score=9.288 rss_kib=655528 ahp_kib=651264 thp_enabled=1 fault_alloc=339 fault_fallback=0 split=0 deferred_split=284 exit=0 secs=10.3 load=4.43
RUN round=2 row=valkey1 set=d score=8.418 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=18 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.3 load=3.98
RUN round=2 row=valkey1 set=j score=9.592 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=305 fault_fallback=0 split=0 deferred_split=16 exit=0 secs=10.3 load=3.52
RUN round=2 row=valkey1-free set=a score=8.094 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.13
RUN round=2 row=valkey1-free set=b score=8.975 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=39 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.80
RUN round=2 row=valkey1-free set=c score=9.333 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=333 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.3 load=2.53
RUN round=2 row=valkey1-free set=d score=7.949 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.3 load=2.29
RUN round=2 row=valkey1-free set=j score=8.942 rss_kib=611032 ahp_kib=598016 thp_enabled=1 fault_alloc=304 fault_fallback=0 split=0 deferred_split=20 exit=0 secs=10.3 load=2.09
RUN round=2 row=larson16 set=a score=307.9 rss_kib=177464 ahp_kib=2048 thp_enabled=1 fault_alloc=4 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=2.95
RUN round=2 row=larson16 set=b score=308.5 rss_kib=178568 ahp_kib=4096 thp_enabled=1 fault_alloc=5 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.23
RUN round=2 row=larson16 set=c score=308.4 rss_kib=247728 ahp_kib=190464 thp_enabled=1 fault_alloc=110 fault_fallback=0 split=0 deferred_split=33 exit=0 secs=7.2 load=4.54
RUN round=2 row=larson16 set=d score=303.0 rss_kib=179060 ahp_kib=0 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=5.46
RUN round=2 row=larson16 set=j score=327.5 rss_kib=278560 ahp_kib=176128 thp_enabled=1 fault_alloc=106 fault_fallback=0 split=0 deferred_split=37 exit=0 secs=7.1 load=5.87
RUN round=3 row=valkey1 set=a score=8.615 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=5 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=5.20
RUN round=3 row=valkey1 set=b score=9.413 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=37 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.2 load=4.56
RUN round=3 row=valkey1 set=c score=9.737 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=333 fault_fallback=0 split=0 deferred_split=279 exit=0 secs=10.3 load=4.08
RUN round=3 row=valkey1 set=d score=8.657 rss_kib=598952 ahp_kib=0 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.61
RUN round=3 row=valkey1 set=j score=9.807 rss_kib=611032 ahp_kib=600064 thp_enabled=1 fault_alloc=302 fault_fallback=0 split=0 deferred_split=23 exit=0 secs=10.3 load=3.21
RUN round=3 row=valkey1-free set=a score=8.251 rss_kib=598900 ahp_kib=0 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.87
RUN round=3 row=valkey1-free set=b score=8.834 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=2.58
RUN round=3 row=valkey1-free set=c score=9.352 rss_kib=655520 ahp_kib=651264 thp_enabled=1 fault_alloc=335 fault_fallback=0 split=0 deferred_split=284 exit=0 secs=10.3 load=2.34
RUN round=3 row=valkey1-free set=d score=8.467 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.13
RUN round=3 row=valkey1-free set=j score=9.012 rss_kib=611016 ahp_kib=598016 thp_enabled=1 fault_alloc=304 fault_fallback=0 split=0 deferred_split=12 exit=0 secs=10.3 load=1.96
RUN round=3 row=larson16 set=a score=307.9 rss_kib=179212 ahp_kib=2048 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=2.91
RUN round=3 row=larson16 set=b score=307.2 rss_kib=183992 ahp_kib=6144 thp_enabled=1 fault_alloc=13 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.52
RUN round=3 row=larson16 set=c score=306.3 rss_kib=247588 ahp_kib=184320 thp_enabled=1 fault_alloc=104 fault_fallback=0 split=0 deferred_split=35 exit=0 secs=7.1 load=5.00
RUN round=3 row=larson16 set=d score=300.9 rss_kib=178644 ahp_kib=0 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=5.96
RUN round=3 row=larson16 set=j score=324.5 rss_kib=275440 ahp_kib=174080 thp_enabled=1 fault_alloc=113 fault_fallback=0 split=0 deferred_split=43 exit=0 secs=7.1 load=6.85
RUN round=4 row=valkey1 set=a score=7.767 rss_kib=598852 ahp_kib=0 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=6.03
RUN round=4 row=valkey1 set=b score=8.351 rss_kib=598892 ahp_kib=32768 thp_enabled=1 fault_alloc=32 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=4.91
RUN round=4 row=valkey1 set=c score=8.768 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=333 fault_fallback=0 split=0 deferred_split=280 exit=0 secs=10.3 load=4.31
RUN round=4 row=valkey1 set=d score=7.841 rss_kib=598836 ahp_kib=0 thp_enabled=1 fault_alloc=7 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.80
RUN round=4 row=valkey1 set=j score=8.691 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=300 fault_fallback=0 split=0 deferred_split=7 exit=0 secs=10.3 load=3.37
RUN round=4 row=valkey1-free set=a score=7.825 rss_kib=598856 ahp_kib=0 thp_enabled=1 fault_alloc=20 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.2 load=3.01
RUN round=4 row=valkey1-free set=b score=8.086 rss_kib=598904 ahp_kib=32768 thp_enabled=1 fault_alloc=32 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.70
RUN round=4 row=valkey1-free set=c score=8.942 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=331 fault_fallback=0 split=0 deferred_split=280 exit=0 secs=10.3 load=2.44
RUN round=4 row=valkey1-free set=d score=7.943 rss_kib=598912 ahp_kib=0 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.21
RUN round=4 row=valkey1-free set=j score=8.812 rss_kib=611020 ahp_kib=598016 thp_enabled=1 fault_alloc=306 fault_fallback=0 split=0 deferred_split=9 exit=0 secs=10.3 load=2.03
RUN round=4 row=larson16 set=a score=307.9 rss_kib=182788 ahp_kib=4096 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.39
RUN round=4 row=larson16 set=b score=309.7 rss_kib=180632 ahp_kib=4096 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=4.27
RUN round=4 row=larson16 set=c score=306.4 rss_kib=251724 ahp_kib=194560 thp_enabled=1 fault_alloc=110 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.2 load=5.20
RUN round=4 row=larson16 set=d score=303.6 rss_kib=175792 ahp_kib=0 thp_enabled=1 fault_alloc=5 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=5.51
RUN round=4 row=larson16 set=j score=327.4 rss_kib=276992 ahp_kib=178176 thp_enabled=1 fault_alloc=105 fault_fallback=0 split=0 deferred_split=33 exit=0 secs=7.0 load=6.43
RUN round=5 row=valkey1 set=a score=7.897 rss_kib=598852 ahp_kib=0 thp_enabled=1 fault_alloc=19 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.2 load=5.59
RUN round=5 row=valkey1 set=b score=8.093 rss_kib=598896 ahp_kib=32768 thp_enabled=1 fault_alloc=33 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.4 load=4.96
RUN round=5 row=valkey1 set=c score=8.923 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=329 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.3 load=4.35
RUN round=5 row=valkey1 set=d score=7.750 rss_kib=598848 ahp_kib=0 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.84
RUN round=5 row=valkey1 set=j score=8.739 rss_kib=611016 ahp_kib=598016 thp_enabled=1 fault_alloc=307 fault_fallback=0 split=0 deferred_split=15 exit=0 secs=10.3 load=3.40
RUN round=5 row=valkey1-free set=a score=8.271 rss_kib=598908 ahp_kib=0 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.03
RUN round=5 row=valkey1-free set=b score=9.081 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=34 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.2 load=2.72
RUN round=5 row=valkey1-free set=c score=9.506 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=333 fault_fallback=0 split=0 deferred_split=280 exit=0 secs=10.3 load=2.34
RUN round=5 row=valkey1-free set=d score=8.036 rss_kib=598896 ahp_kib=0 thp_enabled=1 fault_alloc=14 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.13
RUN round=5 row=valkey1-free set=j score=9.900 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=300 fault_fallback=0 split=0 deferred_split=23 exit=0 secs=10.3 load=1.96
RUN round=5 row=larson16 set=a score=308.1 rss_kib=177252 ahp_kib=2048 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=3.32
RUN round=5 row=larson16 set=b score=308.9 rss_kib=185732 ahp_kib=6144 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.21
RUN round=5 row=larson16 set=c score=309.4 rss_kib=250636 ahp_kib=192512 thp_enabled=1 fault_alloc=114 fault_fallback=0 split=0 deferred_split=36 exit=0 secs=7.2 load=5.15
RUN round=5 row=larson16 set=d score=302.5 rss_kib=175972 ahp_kib=0 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=7.2 load=6.02
RUN round=5 row=larson16 set=j score=324.6 rss_kib=269276 ahp_kib=165888 thp_enabled=1 fault_alloc=101 fault_fallback=0 split=0 deferred_split=31 exit=0 secs=7.2 load=6.51
RUN round=6 row=valkey1 set=a score=8.993 rss_kib=598964 ahp_kib=0 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=5.74
RUN round=6 row=valkey1 set=b score=9.756 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=5.01
RUN round=6 row=valkey1 set=c score=10.055 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=327 fault_fallback=0 split=0 deferred_split=288 exit=0 secs=10.3 load=4.39
RUN round=6 row=valkey1 set=d score=8.861 rss_kib=598944 ahp_kib=0 thp_enabled=1 fault_alloc=7 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.87
RUN round=6 row=valkey1 set=j score=10.355 rss_kib=611032 ahp_kib=598016 thp_enabled=1 fault_alloc=315 fault_fallback=0 split=0 deferred_split=16 exit=0 secs=10.3 load=3.43
RUN round=6 row=valkey1-free set=a score=8.729 rss_kib=598956 ahp_kib=0 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.05
RUN round=6 row=valkey1-free set=b score=9.351 rss_kib=598964 ahp_kib=32768 thp_enabled=1 fault_alloc=31 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=2.81
RUN round=6 row=valkey1-free set=c score=9.602 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=331 fault_fallback=0 split=0 deferred_split=283 exit=0 secs=10.3 load=2.53
RUN round=6 row=valkey1-free set=d score=8.702 rss_kib=598964 ahp_kib=0 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.38
RUN round=6 row=valkey1-free set=j score=9.860 rss_kib=611008 ahp_kib=600064 thp_enabled=1 fault_alloc=304 fault_fallback=0 split=0 deferred_split=15 exit=0 secs=10.3 load=2.16
RUN round=6 row=larson16 set=a score=308.0 rss_kib=180632 ahp_kib=4096 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.24
RUN round=6 row=larson16 set=b score=306.0 rss_kib=184084 ahp_kib=4096 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=7.1 load=4.42
RUN round=6 row=larson16 set=c score=307.0 rss_kib=245320 ahp_kib=188416 thp_enabled=1 fault_alloc=112 fault_fallback=0 split=0 deferred_split=35 exit=0 secs=7.1 load=5.07
RUN round=6 row=larson16 set=d score=305.4 rss_kib=175216 ahp_kib=0 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=6.02
RUN round=6 row=larson16 set=j score=325.8 rss_kib=276964 ahp_kib=174080 thp_enabled=1 fault_alloc=106 fault_fallback=0 split=0 deferred_split=35 exit=0 secs=7.1 load=6.42
RUN round=7 row=valkey1 set=a score=8.646 rss_kib=598940 ahp_kib=0 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=5.59
RUN round=7 row=valkey1 set=b score=9.358 rss_kib=598960 ahp_kib=32768 thp_enabled=1 fault_alloc=36 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=4.96
RUN round=7 row=valkey1 set=c score=10.060 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=331 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.3 load=4.43
RUN round=7 row=valkey1 set=d score=8.715 rss_kib=598944 ahp_kib=0 thp_enabled=1 fault_alloc=20 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.4 load=3.90
RUN round=7 row=valkey1 set=j score=10.451 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=301 fault_fallback=0 split=0 deferred_split=28 exit=0 secs=10.3 load=3.45
RUN round=7 row=valkey1-free set=a score=8.540 rss_kib=598912 ahp_kib=2048 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=3.08
RUN round=7 row=valkey1-free set=b score=9.548 rss_kib=598940 ahp_kib=32768 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.4 load=2.76
RUN round=7 row=valkey1-free set=c score=9.937 rss_kib=655516 ahp_kib=651264 thp_enabled=1 fault_alloc=335 fault_fallback=0 split=0 deferred_split=280 exit=0 secs=10.3 load=2.49
RUN round=7 row=valkey1-free set=d score=8.875 rss_kib=598960 ahp_kib=0 thp_enabled=1 fault_alloc=13 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.2 load=2.26
RUN round=7 row=valkey1-free set=j score=9.847 rss_kib=611020 ahp_kib=598016 thp_enabled=1 fault_alloc=313 fault_fallback=0 split=0 deferred_split=21 exit=0 secs=10.3 load=2.06
RUN round=7 row=larson16 set=a score=307.5 rss_kib=179488 ahp_kib=2048 thp_enabled=1 fault_alloc=7 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=2.92
RUN round=7 row=larson16 set=b score=306.8 rss_kib=181528 ahp_kib=4096 thp_enabled=1 fault_alloc=13 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=4.13
RUN round=7 row=larson16 set=c score=308.7 rss_kib=250556 ahp_kib=196608 thp_enabled=1 fault_alloc=113 fault_fallback=0 split=0 deferred_split=42 exit=0 secs=7.2 load=4.82
RUN round=7 row=larson16 set=d score=304.9 rss_kib=174580 ahp_kib=0 thp_enabled=1 fault_alloc=7 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=5.96
RUN round=7 row=larson16 set=j score=326.1 rss_kib=282652 ahp_kib=174080 thp_enabled=1 fault_alloc=106 fault_fallback=0 split=0 deferred_split=32 exit=0 secs=7.2 load=6.92
```
