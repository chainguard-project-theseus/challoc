# The `thp` default decision, re-measured on the same cell: the shipped `never` (huge region follows the host), `thp:auto`, `thp:always` and jemalloc on cgos-arm64 at the merged tree (Phase 2.5 task 16, lane E)

Date 2026-09-20 (12:24..13:10 UTC). The re-measurement PROGRESS.md
"Phase 2.5 defaults, the human's decisions (2026-09-20", item 1, asked
of lane E: this morning's same-host A/B
(`2026-09-20-aarch64-cgos-thp-ab.md`, tree `f2d9558`) read the then
shipped default, `thp:never` with `MADV_NOHUGEPAGE` on every
reservation, at +16.1% behind jemalloc on `valkey1` on a THP=`always`
host, and `thp:auto` (no advice either way) at +6.3%, the difference
being the 32 MiB of huge runs (the Valkey model's tables) the advice
kept off 2 MiB pages. The human chose that file's option (d): `never`
keeps advising the class superblock and the metadata region and leaves
the huge region to the host's policy. Lane B implemented it (4678fcd);
this file measures the shipped default at the merged tree (`6ea842d`,
the library and the cell code alike; nothing under `src/` is lane E's)
against `thp:auto`, `thp:always` and jemalloc, same cell, same method,
same script (`bench/gce/thp-ab.sh`, `SETS="a b c j"`), and checks lane
B's expectations (PROGRESS.md "Phase 2.5 task 16 (lane B): lane E's
next cgos iteration").

## 1. Host, image, tree, commands

| | |
|---|---|
| VM | `challoc-lane-e-matrix-cgos-arm64`, c4a-standard-8 (8 vCPU Axion, Neoverse-V2: `CPU implementer 0x41 part 0xd4f`), `us-east4-a`, created 12:25 UTC by `challoc-bench matrix lab cgos-arm64` (lane environment `CHALLOC_GCE_PREFIX=challoc-lane-e`) and deleted at the end (`matrix clean`). A different VM instance from this morning's (same name, same machine type, same image): see section 5 on the absolute level |
| image | `kirkland-server-hypervisor-arm64-20260916-1358` (project `wolfi-vm`), the same as this morning's |
| kernel | `6.18.49-r2-gcp-6.18 #Chainguard SMP PREEMPT_NONE` |
| glibc (host) | `glibc-2.44-r6` (the container-built binaries run natively on it) |
| host THP, as shipped and not written by this run | `enabled [always] madvise never`; `defrag always defer defer+madvise [madvise] never`; `hugepages-2048kB always [inherit] madvise never`; every mTHP size 16kB..1024kB `never`; `shrink_underused 1`; `khugepaged: max_ptes_none=511 pages_to_scan=4096 scan_sleep_millisecs=10000 alloc_sleep_millisecs=60000 defrag=1` (identical to this morning's header) |
| tree | `6ea842d` (main = `loop/lane-e`, 4678fcd merged). The library is `6ea842d`'s `cargo build --release` in the `challoc-dev:arm64` image built on the VM (`make image` 50 s, `make build` 10 s, `make bench-build` 17 s, copy-out 1 s, all rc 0; `bench/gce/thp-ab.sh prep`) |
| script | `bench/gce/thp-ab.sh` with `SETS="a b c j"` (this iteration made the setting list an environment variable; nothing else in it changed), `prep` then `run`, as one transient systemd unit `challoc-thpab-1789907175.service` (`sudo systemd-run --uid=$(id -u) ... --setenv=SETS="a b c j"`; a plain ssh job dies with the session on this OS); log `~/logs/thp-ab.log`, per-run outputs `~/thp-ab-out/` |
| rows | `valkey_model -n 850000 -t 10 -s` (valkey1), `valkey_model -n 850000 -t 10 -s -f free` (valkey1-free), `larson 5 8 1000 5000 100 4141 16` (larson16), the `bench/lib.sh` arguments; `~/native/target/bench-bin/` holds `rt larson valkey_model libchalloc.so libjemalloc.so.2 thp-ab-thp thp-ab-features` copied out of the container |
| protocol | as this morning: one discarded warm-up round, then **7 timed rounds**, interleaved round > row > setting; each run `env [MALLOC_CONF=<conf>] LD_PRELOAD=<lib> <row>` natively as the login user; per run the score (`write_mops` / larson `Throughput`/1e6), peak `Rss` and peak `AnonHugePages` from `/proc/<pid>/smaps_rollup` sampled every 200 ms, `THP_enabled` from `/proc/<pid>/status` about 1 s in, and the deltas of `thp_fault_alloc thp_fault_fallback thp_split_page thp_deferred_split_page` in `/proc/vmstat` around the run (host-wide; nothing else ran on the VM) |
| settings | (a) `MALLOC_CONF` unset = `thp:never`, **the shipped default at 6ea842d**: `MADV_NOHUGEPAGE` on the class superblock and the metadata region, the huge region unadvised (this morning's option (d) as chosen); (b) `thp:auto` (no advice either way on this host; unchanged since this morning); (c) `thp:always` (`MADV_HUGEPAGE`d 2 MiB chunks and huge runs; unchanged); (j) jemalloc 5.3 (`/usr/lib/libjemalloc.so.2` of the dev image), no conf. This morning's (d), `thp:never,thp_process:advised`, was not rerun (the prctl is unchanged by 4678fcd; on this default it would now also cover the huge region, which this morning's (d) row already shows: 0 kB either way) |
| load | `/proc/loadavg` 1.78 at start (the prep's compiles decaying), 1.28..6.35 during the runs (the 16-thread larson rows raise the 1-minute average, as this morning), 6.25 at the end |
| exits | 96 of 96 runs exit 0; `thp_fault_fallback` and `thp_split_page` 0 in every run; `THP_enabled` 1 in every run |
| after the A/B | the cell's own steps once on the same VM at the same tree (`MATRIX_ADOPT=challoc-lane-e-matrix-cgos-arm64 challoc-bench matrix cgos-arm64`, section 6) |

The log's header, verbatim:

```
# thp-ab on challoc-lane-e-matrix-cgos-arm64: aarch64 6.18.49-r2-gcp-6.18, 8 CPUs, c4a-standard-8, image kirkland-server-hypervisor-arm64-20260916-1358, 2026-09-20T12:27:33Z, tree 6ea842d
# glibc: GNU C Library (glibc-2.44-r6) stable release version 2.44.
# cpu: CPU implementer	: 0x41 CPU part	: 0xd4f
# host THP (as shipped, not written by this script): enabled [always] madvise never; defrag always defer defer+madvise [madvise] never; hugepages-2048kB always [inherit] madvise never; shrink_underused 1
# mTHP sizes: 1024kB=never 128kB=never 16kB=never 2048kB=inherit 256kB=never 32kB=never 512kB=never 64kB=never
# khugepaged: alloc_sleep_millisecs=60000 defrag=1 full_scans=2 max_ptes_none=511 max_ptes_shared=256 max_ptes_swap=64 pages_collapsed=8 pages_to_scan=4096 scan_sleep_millisecs=10000 
# rows: valkey1 valkey1-free larson16; settings: a b c j; 7 timed rounds after one warm-up, interleaved round > row > setting
# valkey_model -n 850000 -t 10; larson 5 8 1000 5000 100 4141 16
# load at start: 1.78 0.57 0.20
# a: /home/dlorenc/native/target/bench-bin/libchalloc.so mapped=4
# j: /home/dlorenc/native/target/bench-bin/libjemalloc.so.2 mapped=4
```

## 2. What the shipped default now is on this host: `tests/c/thp.c never` and the report line, natively, verbatim

```
-- setting a: challoc default (thp:never); MALLOC_CONF=''; thp-ab-thp never
  thp never: opt.thp=never sysfs thp=always (enabled file: always) mthp=2M pmd 2048 kB (page 4096) pmd_enabled=1 sub_pmd 0 kB
  thp never: thp_except_advised=yes stats.thp.process=0 PR_GET_THP_DISABLE=0 THP_enabled=1
  thp never: report `thp: never -> off (thp:never) nohuge 2 (classes, metadata; huge follows host) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host always process default (nothing issued)`
  thp never: mode 0 class_ranges 0 class_bytes 0 failures 0; VMA hg=0 nh=1 THPeligible=0 AnonHugePages=0 kB
  thp never: huge run 8 MiB: VMA hg=0 nh=0 THPeligible=1 AnonHugePages=8192 kB (global thp always)
  thp never: global THP is always: AnonHugePages 0 kB (class region), 8192 kB (huge run) after 8 + 8 MiB touched: the both-ways rows
  ok thp
  features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no populate_write=yes thp=always mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=28 mm_cid=yes slice_ext=no-slice_ctrl mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

The line lane B asked for, from the `always` host itself: **`AnonHugePages
0 kB (class region), 8192 kB (huge run)`**; the class region carries `nh`
with `THPeligible 0`, the 8 MiB huge run carries no `nh`, `THPeligible
1`, and took its 2 MiB pages from the host; the report line reads
`nohuge 2 (classes, metadata; huge follows host)`; `ok thp` (no
failure). This morning's (a) at `f2d9558` read `nh=1 THPeligible=0
AnonHugePages=0 kB` on the huge run too. Settings (b) and (c) print the
same blocks as this morning (`auto -> off (no folio size below the PMD
is enabled in sysfs)`, the unadvised 8 MiB huge run at `8192 kB`;
`always -> chunks (active)`, `hg=1`, 10240 kB on the class test), and the
`features: 1 failures` under (b) and (c) is `features.c`'s default-conf
check (`collapse has value 2M ... under thp:never`), as this morning.

## 3. The table (rounds 1..7, sorted lists and medians; the script's summary, verbatim), beside this morning's rows

Scores are M ops/s (`write_mops` for the Valkey model, larson throughput
/ 1e6); `vs jemalloc` is challoc's median against jemalloc's *in the
same run*, positive = challoc worse; RSS and AnonHugePages are the
per-run peaks, medians over the 7 runs (max in parentheses for
AnonHugePages); the counters are the per-run `/proc/vmstat` deltas,
medians. The `f2d9558` rows are this morning's file's table, copied for
the comparison; their absolute scores are another VM's (section 5) and
only their `vs jemalloc`, RSS and counter columns compare with today's.

| row | tree | setting | sorted scores | median | vs jemalloc | median peak Rss MiB | median peak AnonHugePages MiB (max) | median thp_fault_alloc | median thp_deferred_split_page |
|---|---|---|---|---:|---:|---:|---:|---:|---:|
| valkey1 | **6ea842d** | **a: the shipped default (`never`: classes + metadata advised, huge follows host)** | 6.579 6.681 6.694 6.727 6.818 6.839 6.855 | 6.727 | **+5.1%** | 584.8 | 32.0 (32.0) | 29 | 0 |
| valkey1 | 6ea842d | b: `thp:auto` | 6.748 6.775 6.778 6.823 6.888 6.907 6.933 | 6.823 | +3.8% | 584.8 | 32.0 (32.0) | 29 | 0 |
| valkey1 | 6ea842d | c: `thp:always` | 7.026 7.075 7.084 7.107 7.181 7.223 7.267 | 7.107 | -0.2% | 640.2 | 636.0 (636.0) | 331 | 281 |
| valkey1 | 6ea842d | j: jemalloc | 6.905 6.932 6.960 7.091 7.110 7.274 7.372 | 7.091 | - | 594.7 | 584.0 (586.0) | 305 | 4 |
| valkey1 | f2d9558 (morning) | a: `never`, every reservation advised | 7.604 7.767 7.897 8.048 8.615 8.646 8.993 | 8.048 | +16.1% | 584.9 | 0.0 (0.0) | 12 | 0 |
| valkey1 | f2d9558 (morning) | b: `thp:auto` | 8.093 8.274 8.351 8.983 9.358 9.413 9.756 | 8.983 | +6.3% | 584.9 | 32.0 (32.0) | 32 | 0 |
| valkey1 | f2d9558 (morning) | c: `thp:always` | 8.768 8.923 9.278 9.288 9.737 10.055 10.060 | 9.288 | +3.2% | 640.2 | 636.0 (636.0) | 333 | 281 |
| valkey1 | f2d9558 (morning) | d: `never,thp_process:advised` | 7.750 7.841 8.119 8.418 8.657 8.715 8.861 | 8.418 | +12.2% | 584.9 | 0.0 (0.0) | 12 | 0 |
| valkey1 | f2d9558 (morning) | j: jemalloc | 8.691 8.739 8.911 9.592 9.807 10.355 10.451 | 9.592 | - | 596.7 | 584.0 (586.0) | 305 | 16 |
| valkey1-free | **6ea842d** | **a: the shipped default** | 6.509 6.532 6.543 6.565 6.585 6.587 6.606 | 6.565 | **+6.4%** | 584.7 | 32.0 (32.0) | 28 | 0 |
| valkey1-free | 6ea842d | b: `thp:auto` | 6.502 6.506 6.559 6.610 6.626 6.699 6.750 | 6.610 | +5.8% | 584.7 | 32.0 (32.0) | 30 | 0 |
| valkey1-free | 6ea842d | c: `thp:always` | 6.792 6.924 6.956 6.959 7.023 7.098 7.105 | 6.959 | +0.8% | 640.2 | 636.0 (636.0) | 332 | 281 |
| valkey1-free | 6ea842d | j: jemalloc | 6.857 6.911 6.940 7.015 7.099 7.107 7.145 | 7.015 | - | 596.7 | 584.0 (584.0) | 302 | 3 |
| valkey1-free | f2d9558 (morning) | a | 7.702 7.825 8.094 8.251 8.271 8.540 8.729 | 8.251 | +9.0% | 584.9 | 0.0 (2.0) | 12 | 0 |
| valkey1-free | f2d9558 (morning) | b | 7.726 8.086 8.834 8.975 9.081 9.351 9.548 | 8.975 | +1.1% | 584.9 | 32.0 (32.0) | 32 | 0 |
| valkey1-free | f2d9558 (morning) | c | 8.105 8.942 9.333 9.352 9.506 9.602 9.937 | 9.352 | -3.1% | 640.2 | 636.0 (636.0) | 333 | 281 |
| valkey1-free | f2d9558 (morning) | d | 7.207 7.943 7.949 8.036 8.467 8.702 8.875 | 8.036 | +11.4% | 584.9 | 0.0 (0.0) | 12 | 0 |
| valkey1-free | f2d9558 (morning) | j | 8.812 8.942 9.012 9.072 9.847 9.860 9.900 | 9.072 | - | 596.7 | 584.0 (586.0) | 304 | 20 |
| larson16 | **6ea842d** | **a: the shipped default** | 297.1 297.4 297.4 298.7 298.7 299.0 299.1 | 298.7 | **+6.0%** | 173.7 | 2.0 (6.0) | 10 | 0 |
| larson16 | 6ea842d | b: `thp:auto` | 297.8 298.9 299.1 299.4 299.6 300.2 300.3 | 299.4 | +5.8% | 176.8 | 6.0 (8.0) | 9 | 0 |
| larson16 | 6ea842d | c: `thp:always` | 297.0 297.9 298.3 298.9 299.4 299.9 300.3 | 298.9 | +5.9% | 243.0 | 186.0 (198.0) | 114 | 38 |
| larson16 | 6ea842d | j: jemalloc | 316.6 316.9 317.7 317.7 318.1 318.3 319.1 | 317.7 | - | 263.5 | 160.0 (170.0) | 106 | 38 |
| larson16 | f2d9558 (morning) | a | 307.5 307.9 307.9 307.9 307.9 308.0 308.1 | 307.9 | +5.5% | 175.0 | 2.0 (4.0) | 9 | 0 |
| larson16 | f2d9558 (morning) | b | 306.0 306.8 307.0 307.2 308.5 308.9 309.7 | 307.2 | +5.7% | 179.7 | 4.0 (6.0) | 12 | 0 |
| larson16 | f2d9558 (morning) | c | 305.1 306.3 306.4 307.0 308.4 308.7 309.4 | 307.0 | +5.8% | 244.7 | 188.0 (194.0) | 112 | 36 |
| larson16 | f2d9558 (morning) | d | 300.9 302.5 303.0 303.6 304.1 304.9 305.4 | 303.6 | +6.8% | 171.8 | 0.0 (0.0) | 10 | 0 |
| larson16 | f2d9558 (morning) | j | 324.5 324.6 325.6 325.8 326.1 327.4 327.5 | 325.8 | - | 270.5 | 170.0 (174.0) | 106 | 35 |

Derived from today's medians (ratios of medians; in the first three
columns positive = the first-named setting is faster; the `vs jemalloc`
columns keep the table's convention, positive = challoc worse; the RSS
column positive = (c) larger), with this morning's figure for the same
column in brackets where the column existed:

| row | (a) vs (b): the default against no advice | (c) vs (a): `always` over the default | (c) vs (b) | (a) vs jemalloc | (b) vs jemalloc | (c) vs jemalloc | peak Rss (c) vs (a) |
|---|---:|---:|---:|---:|---:|---:|---:|
| valkey1 | -1.4% [-10.4%] | +5.6% | +4.2% [+3.4%] | +5.1% [+16.1%] | +3.8% [+6.3%] | -0.2% [+3.2%] | +9.5% (584.8 -> 640.2 MiB) [+9.5%] |
| valkey1-free | -0.7% [-8.1%] | +6.0% | +5.3% [+4.2%] | +6.4% [+9.0%] | +5.8% [+1.1%] | +0.8% [-3.1%] | +9.5% (584.7 -> 640.2 MiB) [+9.5%] |
| larson16 | -0.2% [+0.2%] | +0.1% | -0.2% [-0.1%] | +6.0% [+5.5%] | +5.8% [+5.7%] | +5.9% [+5.8%] | +39.9% (173.7 -> 243.0 MiB) [+39.8%] |

## 4. The reading, against lane B's expectations

The shipped default now reads as `thp:auto` on this host, which is what
the decision intended: (a) against (b) is -1.4% / -0.7% / -0.2% on the
three rows with the sorted lists overlapping (this morning -10.4% /
-8.1% / +0.2%), the default's `AnonHugePages` at the Valkey model's peak
is 32.0 MiB in every run (this morning's (a) 0.0; (b) 32.0 both times)
with `thp_fault_alloc` 29 per run (this morning's (a) 12, (b) 32), and
peak RSS is 584.8 MiB on both Valkey rows (this morning 584.9) and
173.7 MiB on larson16 (175.0), so the 32 MiB of huge runs came back for
no RSS change. Against jemalloc the default reads **+5.1% on `valkey1`**
(expected about +6%; this morning's (a) +16.1%), **+6.4% on
`valkey1-free`** (expected about +1%: refuted as a figure, but the
expectation was "reads as (b)", and (b) itself reads +5.8% in this run
against +1.1% this morning, so the row moved with jemalloc's own spread
between the two VMs, not with the default; the two lists overlap over
their whole range), and **+6.0% on larson16** (expected +5..6%, unmoved;
the 2.0 (6.0) MiB of `AnonHugePages` there are the same as this
morning's (a) 2.0 (4.0), the program's own and libc's mappings, since
larson has no huge runs and the class region still carries `nh`).
`thp:always` still buys parity with jemalloc (-0.2% / +0.8% / +5.9%)
for the same +9.5% peak RSS on the 585 MiB heap and +39.9% on larson's
retained memory as this morning, and `thp:auto` is now within the spread
of the default on every row, so the key has nothing left to buy on an
`always` host: the decision is confirmed on the cell it was made for.

## 5. Notes

- **The absolute level of this VM is not this morning's.** jemalloc's
  medians read 7.091 / 7.015 / 317.7 against 9.592 / 9.072 / 325.8 this
  morning (-26% / -23% / -2.5%), and the Valkey rows' 7-run lists are
  four times tighter (the default 6.58..6.86, 4%, against 7.60..8.99,
  18%). Same machine type, image, kernel and script; a different VM
  instance on a different Axion host (the memory-bound Valkey model sees
  it, the cache-resident larson barely does). Nothing in this file
  compares an absolute score across the two runs; every comparison is
  a within-run ratio, which the interleaving makes valid, or a ratio of
  such ratios.
- (d) of this morning (`thp_process:advised` on the default) was not
  rerun: 4678fcd did not touch the prctl, and on the new default the
  prctl now takes the huge region's 2 MiB pages away again (a forced
  process-wide `never`), which this morning's (d) row already measured
  (0 kB either way, +12.2% behind jemalloc on `valkey1`). Lane B's PROGRESS
  bullet says as much: `thp_process:advised` is now the key for "no huge
  page anywhere in the process", which `never` no longer claims.
- `features: 1 failures` under (b) and (c) and the `ok thp` under (a)
  are as this morning (section 2); `tests/c/thp.c never` passes on the
  `always` host with the new assertion (`no nh on the huge run`,
  `THPeligible != 0` there, at least 2 MiB of `AnonHugePages` on the
  run).
- The cell run after the A/B (section 6) adopted this VM
  (`MATRIX_ADOPT`); the driver's `matrix_clean` exit trap would have
  deleted the VM under its own name once the cell ended, so the driver
  was detached after it had launched the VM-side unit (12:44:09) and the
  polling, the fetch of `~/logs` and of `bench/results/scorecard-cgos-
  arm64.md` were done by hand with the driver's own rsync; the VM was
  deleted by `matrix clean` at the end.
- The lane touched nothing under `src/`; the library measured is
  `6ea842d`'s. The one code change is `bench/gce/thp-ab.sh`'s `SETS`
  variable.

## 6. The cell's own steps at 6ea842d on the same VM (`matrix-cell.sh cgos-arm64 ... cgos`)

`matrix-cell.sh cgos-arm64 kirkland-server-hypervisor-arm64-20260916-1358
cgos` as a transient systemd unit (`challoc-matrix-1789908249.service`),
12:44:09..13:01:43 UTC, wall 1054 s; the driver's log with the VM's
`~/logs` folded in is `bench/results/matrix/2026-09-20-cgos-arm64-thp-decision.log`.
Every step rc 0, outcome PASS (each of the day's earlier cgos-arm64
cells carried one failing test in one of the two `make test` profiles,
`2026-09-20-cgos-arm64*.log`; the last, at a999097, was
`probe_prints_and_matches_kernel_version` under `--privileged`; none
recurred):

| step | result |
|---|---|
| `make image` | rc 0 (cached from the A/B's prep) |
| `make test` (podman's default seccomp profile) | **411 passed + 120 passed + 23 passed, 0 failed** (unit tests 9.64 s, api tests with the C programs 22.90 s, the cgroup-limit tests 8.69 s), rc 0 in 80 s: the 411 + 120 + 23 lane B expected |
| probe (in podman, default profile) | `features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=no populate_write=yes thp=always mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=28 mm_cid=yes slice_ext=no-slice_ctrl mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M` (seccomp hides mseal and process_madvise, as on every podman cell) |
| probe (`--privileged`) and natively on the host | `mseal=yes process_madvise=pidfd_self process_madvise_free=yes`, the rest identical (the native line verbatim in section 2's features line) |
| `make test` with `DOCKER_FLAGS=--privileged` | 411 + 120 + 23 passed, 0 failed, rc 0 in 60 s |
| `make test-cgroup` | rc 0 in 318 s (the churn at 512 and 256 MiB, the lazy purge, the 300 s oscillation: `cycles=98 seconds=301 low=200 high=480 current_peak=503 rss_peak=502`, `memory.events` all 0, `ok cgroup_oscillate`) |
| `make scorecard ROUNDS=3` into `bench/results/scorecard-cgos-arm64.md` | rc 0 in 592 s; the line appended (and fetched into this tree), verbatim below |
| `MATRIX` line | `cell=cgos-arm64 image=kirkland-server-hypervisor-arm64-20260916-1358 kernel=6.18.49-r2-gcp-6.18 page=4096 machine=c4a-standard-8 zone=us-east4-a vcpus=8 tests=554/0 fuzz=- cgroup=PASS privileged=PASS:554/0 scorecard=PASS wall=1054 outcome=PASS` |

```
| 2026-09-20 | 6ea842d | +7.1% | +4.7% | +5.7% | -25.1% | +8.1% | +5.1% | +6.3% | +5.9% | +0.9% | -2.3% | -39.3% | -13.7% | -18.1% | +1.0% | +2.1% | 0.65 | 0.98 | - | - | - | - | 3 rounds, Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4a-standard-8 (Neoverse-V2), zone us-east4-a, 8 vCPU, podman --network=host; no soak for 6ea842d |
```

Read against the cell's earlier lines (`bench/scorecard.sh --render`
over that file; 3-round cell tier, so coarser than section 3): `valkey1`
/ `valkey1-free` +6.3% / +5.9% at 6ea842d, against +6.4% / +4.2% on
2026-09-19 at 4864d4b (before task 16), +11.7% / +12.9% at e0fdf71 and
+9.8% / +10.0% at a999097 (the every-reservation `never`): the cell's
own trend record is back where it was before task 16's first half, and
the other thirteen rows moved within their usual spread (larson16 +5.7%
against +5.4 / +5.1 / +5.1; RSS ratios 0.65 / 0.98 unchanged).

## 7. Every run (the RUN lines, verbatim; round 0 is the discarded warm-up; KiB; `secs` wall per run; `load` the 1-minute average at the end of the run)

```
RUN round=0 row=valkey1 set=a score=6.886 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=22 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.2 load=1.66
RUN round=0 row=valkey1 set=b score=6.775 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=27 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=1.56
RUN round=0 row=valkey1 set=c score=7.032 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=327 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.1 load=1.55
RUN round=0 row=valkey1 set=j score=7.081 rss_kib=611008 ahp_kib=598016 thp_enabled=1 fault_alloc=310 fault_fallback=0 split=0 deferred_split=6 exit=0 secs=10.1 load=1.47
RUN round=0 row=valkey1-free set=a score=6.667 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=22 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=1.39
RUN round=0 row=valkey1-free set=b score=6.561 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=45 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.3 load=1.33
RUN round=0 row=valkey1-free set=c score=6.965 rss_kib=655512 ahp_kib=651264 thp_enabled=1 fault_alloc=329 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.3 load=1.28
RUN round=0 row=valkey1-free set=j score=7.054 rss_kib=611016 ahp_kib=598016 thp_enabled=1 fault_alloc=316 fault_fallback=0 split=0 deferred_split=10 exit=0 secs=10.1 load=1.31
RUN round=0 row=larson16 set=a score=299.6 rss_kib=180084 ahp_kib=4096 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.3 load=2.36
RUN round=0 row=larson16 set=b score=299.6 rss_kib=186136 ahp_kib=6144 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.53
RUN round=0 row=larson16 set=c score=296.7 rss_kib=248892 ahp_kib=188416 thp_enabled=1 fault_alloc=109 fault_fallback=0 split=0 deferred_split=39 exit=0 secs=7.1 load=4.40
RUN round=0 row=larson16 set=j score=316.8 rss_kib=279828 ahp_kib=180224 thp_enabled=1 fault_alloc=107 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=5.49
RUN round=1 row=valkey1 set=a score=6.727 rss_kib=598828 ahp_kib=32768 thp_enabled=1 fault_alloc=29 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.3 load=4.87
RUN round=1 row=valkey1 set=b score=6.888 rss_kib=598816 ahp_kib=32768 thp_enabled=1 fault_alloc=36 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=4.35
RUN round=1 row=valkey1 set=c score=7.181 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=326 fault_fallback=0 split=0 deferred_split=284 exit=0 secs=10.1 load=3.84
RUN round=1 row=valkey1 set=j score=7.110 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=311 fault_fallback=0 split=0 deferred_split=6 exit=0 secs=10.1 load=3.40
RUN round=1 row=valkey1-free set=a score=6.587 rss_kib=598720 ahp_kib=32768 thp_enabled=1 fault_alloc=30 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.03
RUN round=1 row=valkey1-free set=b score=6.502 rss_kib=598724 ahp_kib=32768 thp_enabled=1 fault_alloc=23 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.80
RUN round=1 row=valkey1-free set=c score=7.098 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=331 fault_fallback=0 split=0 deferred_split=285 exit=0 secs=10.1 load=2.52
RUN round=1 row=valkey1-free set=j score=7.145 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=302 fault_fallback=0 split=0 deferred_split=4 exit=0 secs=10.1 load=2.29
RUN round=1 row=larson16 set=a score=299.0 rss_kib=177328 ahp_kib=2048 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.19
RUN round=1 row=larson16 set=b score=299.1 rss_kib=180928 ahp_kib=6144 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=4.29
RUN round=1 row=larson16 set=c score=300.3 rss_kib=256596 ahp_kib=186368 thp_enabled=1 fault_alloc=125 fault_fallback=0 split=0 deferred_split=36 exit=0 secs=7.1 load=4.96
RUN round=1 row=larson16 set=j score=319.1 rss_kib=269848 ahp_kib=163840 thp_enabled=1 fault_alloc=106 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=6.00
RUN round=2 row=valkey1 set=a score=6.579 rss_kib=598720 ahp_kib=32768 thp_enabled=1 fault_alloc=24 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=5.23
RUN round=2 row=valkey1 set=b score=6.933 rss_kib=598820 ahp_kib=32768 thp_enabled=1 fault_alloc=29 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.3 load=4.58
RUN round=2 row=valkey1 set=c score=7.075 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=333 fault_fallback=0 split=0 deferred_split=280 exit=0 secs=10.1 load=4.03
RUN round=2 row=valkey1 set=j score=6.905 rss_kib=608960 ahp_kib=598016 thp_enabled=1 fault_alloc=300 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.1 load=3.56
RUN round=2 row=valkey1-free set=a score=6.509 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.17
RUN round=2 row=valkey1-free set=b score=6.559 rss_kib=598720 ahp_kib=32768 thp_enabled=1 fault_alloc=35 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.84
RUN round=2 row=valkey1-free set=c score=6.924 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=337 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=2.55
RUN round=2 row=valkey1-free set=j score=6.857 rss_kib=608960 ahp_kib=598016 thp_enabled=1 fault_alloc=302 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.1 load=2.31
RUN round=2 row=larson16 set=a score=297.1 rss_kib=176156 ahp_kib=2048 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=3.21
RUN round=2 row=larson16 set=b score=297.8 rss_kib=183984 ahp_kib=6144 thp_enabled=1 fault_alloc=12 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.24
RUN round=2 row=larson16 set=c score=297.9 rss_kib=245196 ahp_kib=198656 thp_enabled=1 fault_alloc=112 fault_fallback=0 split=0 deferred_split=44 exit=0 secs=7.2 load=4.84
RUN round=2 row=larson16 set=j score=318.3 rss_kib=283720 ahp_kib=169984 thp_enabled=1 fault_alloc=111 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=5.73
RUN round=3 row=valkey1 set=a score=6.694 rss_kib=598804 ahp_kib=32768 thp_enabled=1 fault_alloc=38 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.3 load=5.00
RUN round=3 row=valkey1 set=b score=6.748 rss_kib=598820 ahp_kib=32768 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=4.46
RUN round=3 row=valkey1 set=c score=7.084 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=327 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=4.00
RUN round=3 row=valkey1 set=j score=6.932 rss_kib=608960 ahp_kib=598016 thp_enabled=1 fault_alloc=302 fault_fallback=0 split=0 deferred_split=4 exit=0 secs=10.1 load=3.54
RUN round=3 row=valkey1-free set=a score=6.532 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=28 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.15
RUN round=3 row=valkey1-free set=b score=6.610 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=30 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.82
RUN round=3 row=valkey1-free set=c score=6.792 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=341 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=2.54
RUN round=3 row=valkey1-free set=j score=7.099 rss_kib=611032 ahp_kib=598016 thp_enabled=1 fault_alloc=296 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.1 load=2.30
RUN round=3 row=larson16 set=a score=298.7 rss_kib=179580 ahp_kib=4096 thp_enabled=1 fault_alloc=10 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.27
RUN round=3 row=larson16 set=b score=300.3 rss_kib=180332 ahp_kib=4096 thp_enabled=1 fault_alloc=11 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.29
RUN round=3 row=larson16 set=c score=298.3 rss_kib=244060 ahp_kib=190464 thp_enabled=1 fault_alloc=110 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=4.88
RUN round=3 row=larson16 set=j score=316.9 rss_kib=264808 ahp_kib=163840 thp_enabled=1 fault_alloc=99 fault_fallback=0 split=0 deferred_split=34 exit=0 secs=7.1 load=5.93
RUN round=4 row=valkey1 set=a score=6.855 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=5.25
RUN round=4 row=valkey1 set=b score=6.775 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=32 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=4.68
RUN round=4 row=valkey1 set=c score=7.107 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=344 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.1 load=4.11
RUN round=4 row=valkey1 set=j score=7.091 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=308 fault_fallback=0 split=0 deferred_split=6 exit=0 secs=10.1 load=3.78
RUN round=4 row=valkey1-free set=a score=6.543 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.35
RUN round=4 row=valkey1-free set=b score=6.750 rss_kib=598828 ahp_kib=32768 thp_enabled=1 fault_alloc=34 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.99
RUN round=4 row=valkey1-free set=c score=6.956 rss_kib=655528 ahp_kib=651264 thp_enabled=1 fault_alloc=328 fault_fallback=0 split=0 deferred_split=279 exit=0 secs=10.1 load=2.68
RUN round=4 row=valkey1-free set=j score=6.940 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=300 fault_fallback=0 split=0 deferred_split=4 exit=0 secs=10.1 load=2.42
RUN round=4 row=larson16 set=a score=297.4 rss_kib=178136 ahp_kib=4096 thp_enabled=1 fault_alloc=21 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=7.2 load=3.30
RUN round=4 row=larson16 set=b score=299.6 rss_kib=182316 ahp_kib=4096 thp_enabled=1 fault_alloc=6 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.32
RUN round=4 row=larson16 set=c score=299.4 rss_kib=252000 ahp_kib=202752 thp_enabled=1 fault_alloc=115 fault_fallback=0 split=0 deferred_split=43 exit=0 secs=7.1 load=5.06
RUN round=4 row=larson16 set=j score=317.7 rss_kib=268508 ahp_kib=167936 thp_enabled=1 fault_alloc=99 fault_fallback=0 split=0 deferred_split=35 exit=0 secs=7.2 load=6.01
RUN round=5 row=valkey1 set=a score=6.839 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=31 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=5.24
RUN round=5 row=valkey1 set=b score=6.823 rss_kib=598824 ahp_kib=32768 thp_enabled=1 fault_alloc=29 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=4.59
RUN round=5 row=valkey1 set=c score=7.026 rss_kib=655512 ahp_kib=651264 thp_enabled=1 fault_alloc=331 fault_fallback=0 split=0 deferred_split=282 exit=0 secs=10.1 load=4.04
RUN round=5 row=valkey1 set=j score=6.960 rss_kib=611016 ahp_kib=600064 thp_enabled=1 fault_alloc=311 fault_fallback=0 split=0 deferred_split=6 exit=0 secs=10.1 load=3.57
RUN round=5 row=valkey1-free set=a score=6.585 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=35 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.17
RUN round=5 row=valkey1-free set=b score=6.506 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=27 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.84
RUN round=5 row=valkey1-free set=c score=7.105 rss_kib=655520 ahp_kib=651264 thp_enabled=1 fault_alloc=332 fault_fallback=0 split=0 deferred_split=279 exit=0 secs=10.3 load=2.56
RUN round=5 row=valkey1-free set=j score=7.107 rss_kib=611028 ahp_kib=598016 thp_enabled=1 fault_alloc=312 fault_fallback=0 split=0 deferred_split=9 exit=0 secs=10.1 load=2.14
RUN round=5 row=larson16 set=a score=297.4 rss_kib=177824 ahp_kib=2048 thp_enabled=1 fault_alloc=17 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=3.49
RUN round=5 row=larson16 set=b score=299.4 rss_kib=182592 ahp_kib=8192 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.1 load=4.65
RUN round=5 row=larson16 set=c score=297.0 rss_kib=247660 ahp_kib=194560 thp_enabled=1 fault_alloc=121 fault_fallback=0 split=0 deferred_split=34 exit=0 secs=7.1 load=5.34
RUN round=5 row=larson16 set=j score=318.1 rss_kib=278816 ahp_kib=174080 thp_enabled=1 fault_alloc=112 fault_fallback=0 split=0 deferred_split=38 exit=0 secs=7.1 load=6.35
RUN round=6 row=valkey1 set=a score=6.681 rss_kib=598756 ahp_kib=32768 thp_enabled=1 fault_alloc=33 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=5.53
RUN round=6 row=valkey1 set=b score=6.907 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=28 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=4.83
RUN round=6 row=valkey1 set=c score=7.223 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=332 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=4.24
RUN round=6 row=valkey1 set=j score=7.372 rss_kib=608968 ahp_kib=595968 thp_enabled=1 fault_alloc=305 fault_fallback=0 split=0 deferred_split=4 exit=0 secs=10.1 load=3.74
RUN round=6 row=valkey1-free set=a score=6.565 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=28 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.32
RUN round=6 row=valkey1-free set=b score=6.699 rss_kib=598804 ahp_kib=32768 thp_enabled=1 fault_alloc=40 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.3 load=2.81
RUN round=6 row=valkey1-free set=c score=7.023 rss_kib=655532 ahp_kib=651264 thp_enabled=1 fault_alloc=337 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=2.53
RUN round=6 row=valkey1-free set=j score=7.015 rss_kib=608976 ahp_kib=598016 thp_enabled=1 fault_alloc=303 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.1 load=2.29
RUN round=6 row=larson16 set=a score=299.1 rss_kib=183040 ahp_kib=6144 thp_enabled=1 fault_alloc=13 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=7.3 load=3.63
RUN round=6 row=larson16 set=b score=300.2 rss_kib=180144 ahp_kib=8192 thp_enabled=1 fault_alloc=9 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.2 load=4.70
RUN round=6 row=larson16 set=c score=298.9 rss_kib=248808 ahp_kib=184320 thp_enabled=1 fault_alloc=114 fault_fallback=0 split=0 deferred_split=32 exit=0 secs=7.1 load=5.23
RUN round=6 row=larson16 set=j score=317.7 rss_kib=281780 ahp_kib=163840 thp_enabled=1 fault_alloc=111 fault_fallback=0 split=0 deferred_split=39 exit=0 secs=7.1 load=6.01
RUN round=7 row=valkey1 set=a score=6.818 rss_kib=598816 ahp_kib=32768 thp_enabled=1 fault_alloc=26 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=5.24
RUN round=7 row=valkey1 set=b score=6.778 rss_kib=598832 ahp_kib=32768 thp_enabled=1 fault_alloc=36 fault_fallback=0 split=0 deferred_split=2 exit=0 secs=10.3 load=4.66
RUN round=7 row=valkey1 set=c score=7.267 rss_kib=655512 ahp_kib=651264 thp_enabled=1 fault_alloc=326 fault_fallback=0 split=0 deferred_split=279 exit=0 secs=10.1 load=4.10
RUN round=7 row=valkey1 set=j score=7.274 rss_kib=608980 ahp_kib=598016 thp_enabled=1 fault_alloc=305 fault_fallback=0 split=0 deferred_split=3 exit=0 secs=10.1 load=3.62
RUN round=7 row=valkey1-free set=a score=6.606 rss_kib=598728 ahp_kib=32768 thp_enabled=1 fault_alloc=25 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=3.04
RUN round=7 row=valkey1-free set=b score=6.626 rss_kib=598720 ahp_kib=32768 thp_enabled=1 fault_alloc=24 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=10.3 load=2.80
RUN round=7 row=valkey1-free set=c score=6.959 rss_kib=655524 ahp_kib=651264 thp_enabled=1 fault_alloc=329 fault_fallback=0 split=0 deferred_split=281 exit=0 secs=10.1 load=2.52
RUN round=7 row=valkey1-free set=j score=6.911 rss_kib=608980 ahp_kib=598016 thp_enabled=1 fault_alloc=316 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=10.1 load=2.29
RUN round=7 row=larson16 set=a score=298.7 rss_kib=177880 ahp_kib=2048 thp_enabled=1 fault_alloc=8 fault_fallback=0 split=0 deferred_split=1 exit=0 secs=7.2 load=3.47
RUN round=7 row=larson16 set=b score=298.9 rss_kib=181028 ahp_kib=6144 thp_enabled=1 fault_alloc=16 fault_fallback=0 split=0 deferred_split=0 exit=0 secs=7.3 load=4.11
RUN round=7 row=larson16 set=c score=299.9 rss_kib=251108 ahp_kib=182272 thp_enabled=1 fault_alloc=107 fault_fallback=0 split=0 deferred_split=42 exit=0 secs=7.1 load=5.22
RUN round=7 row=larson16 set=j score=316.6 rss_kib=268276 ahp_kib=163840 thp_enabled=1 fault_alloc=99 fault_fallback=0 split=0 deferred_split=35 exit=0 secs=7.2 load=6.25
```
