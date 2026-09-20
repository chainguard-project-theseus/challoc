# 2026-09-20: `slice_ext=` and `thp_except_advised=` in the feature report, and the `thp_process` prctl (Phase 2.5 tasks 15 and 16, lane B)

- change: (task 15) `challoc_features()` and the `Kernel and CPU features:` line gain
  `slice_ext=<word>` after `mm_cid` (`sys::features::SliceExt`, read through `slice::probe`
  and `slice::verdict`, never a second probe); (task 16, the prctl half) `thp_process:advised`
  issues `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)` once at init where the
  probe `sys::features::probe_thp_disable` (the call and its undo, only on a process with no
  inherited policy) found the kernel takes it; the line gains `thp_except_advised=yes|no|
  inherited` after `collapse`, the `thp:` report line ends `; host <global mode> process
  <outcome>`, `stats.thp.process` reads the outcome (0 nothing issued, 1 in force, 2 the kernel
  refused it, 3 an inherited policy kept, 4 the call failed); (task 11) the line also gains
  `populate_write=yes|no` after `anon_name` (`bench/results/2026-09-20-aarch64-lane-b-task11-
  prefault.md`). Cold paths only: probes at init, one line, one prctl.
- base: 48af80c (main with lane C's `thp_process` and `prefault` keys).
- hosts: `challoc-lane-b-arm64` (c4a-standard-16, Axion) and `challoc-lane-b-amd64`
  (c4-standard-16, Xeon 8581C), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, THP `madvise`; the Mac's
  Docker Desktop VM (Apple silicon, linuxkit 6.10.14, THP `always`); the dev container (Wolfi,
  glibc 2.44) everywhere.

## 1. The lines, both ways

| host | kernel | `thp_except_advised` | `slice_ext` | why |
|---|---|---|---|---|
| C4 (x86_64) | 7.0.0-1011-gcp | `yes` | `kernel-disabled` | 6.18+ takes the flag (`PR_GET_THP_DISABLE` 0 before and after the probe); `__rseq_size` 33, bit 4 clear (Ubuntu builds without `CONFIG_RSEQ_SLICE_EXTENSION`) |
| C4A (aarch64) | 7.0.0-1011-gcp | `yes` | `kernel-disabled` | the same; arm64 lacks the option's dependencies on every kernel |
| Mac Docker VM (aarch64) | 6.10.14-linuxkit | `no` | `no-slice_ctrl` | `EINVAL` for a flags word before 6.18; `__rseq_size` 20, no `slice_ctrl` |

The lines, verbatim (`tests/c/features.c` under the preload; identical to the report's):

```
C4:   page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes populate_write=yes thp=madvise mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=33 mm_cid=yes slice_ext=kernel-disabled lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
C4A:  page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes populate_write=yes thp=madvise mthp=2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=33 mm_cid=yes slice_ext=kernel-disabled mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
Mac:  page_size=4096 guard_install=no mseal=no process_madvise=no process_madvise_free=no anon_name=no populate_write=yes thp=always mthp=2M collapse=unprobed thp_except_advised=no rseq=yes rseq_size=20 mm_cid=no slice_ext=no-slice_ctrl mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=no cacheline=64 l1d=unknown l2=unknown
```

(`mseal=no process_madvise=no` on the VMs are Docker's default seccomp profile inside `make
test`, as in the task 2 and 8 files.) The `Slice extension:` line of the same report agrees
with the key on every host (`off (kernel-disabled)`, `off (no-slice_ctrl)`; the C4's line
under `slice_ext:on` adds `; slice_ext:on requested, unavailable`). No host in reach has the
extension on, so `slice_ext=on` and `available` are covered by the host tests
(`sys::features::tests::slice_ext_words_follow_the_verdicts`) and by lane C's 7.0.14 build
of the C4 kernel is the place to read them (`2026-09-19-x86_64-lane-c-task15-slice-ext.md`).

## 2. The prctl, both ways (`tests/c/thp.c process` under `thp_process:advised,thp:auto`)

| host | `thp_except_advised` | `stats.thp.process` | `PR_GET_THP_DISABLE` after init | `THP_enabled` (`/proc/self/status`) | the `thp:` line's tail |
|---|---|---|---|---|---|
| C4, C4A (7.0) | `yes` | 1 | 3 (`1 \| EXCEPT_ADVISED`) | 1 | `; host madvise process advised: PR_THP_DISABLE_EXCEPT_ADVISED in force` |
| Mac (6.10, `always`) | `no` | 2 | 0 | 1 | `; host always process advised: skipped, the kernel refuses PR_THP_DISABLE_EXCEPT_ADVISED (EINVAL before 6.18)` |

The kernel's `THP_enabled` field reads 1 with the flag in force on 7.0 (the field reports
"THP not disabled completely", the 6.18 semantics), so it does not separate `advised` from
nothing; `PR_GET_THP_DISABLE` does (3 against 0). On the Mac's `always` host without the
prctl the unadvised 8 MiB huge run under `thp:auto` showed `AnonHugePages 8192 kB`: the row
the prctl is for, which no host in reach can show moving (the 7.0 VMs are `madvise`, so the
same regions read 0 kB with or without it; the note for lane E in the lane's PROGRESS.d
file names the command for the cgos cells, kernel 6.18 with THP `always`). Every other mode
of `thp.c` (`never`, `always`, `auto`) checks `stats.thp.process == 0` and `process default
(nothing issued)`.

## 3. Tests

- Host (`cargo test --features std`, macOS): 317 passed, among them
  `sys::features::tests::{display_is_one_line_of_key_values, feature_line_is_the_display_with_
  a_nul, feature_line_fits, slice_ext_words_follow_the_verdicts}` (the longest line is 462
  bytes; `FeatureLine::CAP` went from 512 to 640), `thp::tests::process_outcome_follows_the_
  key_and_the_probe`, `api::print::tests::feature_line_slice_ext_agrees_with_the_slice_line`.
- Container (`make test`): `sys::features::linux_tests::probe_prints_and_matches_kernel_version`
  cross-checks `slice_ext` against `slice::probe` and the prctl probe against
  `PR_GET_THP_DISABLE` before and after (and, where `yes`, takes and undoes the flag once more,
  reading 3 then 0); `tests/c/features.c` checks the two keys' vocabulary, `slice_ext` against
  `rseq_size` and the `Slice extension:` line, `thp_except_advised` against
  `PR_GET_THP_DISABLE`; `tests/c/thp.c process` as in section 2.
- `make test` green with the final code: C4 408 + 120 + 23 passed, `EXIT 0` (churn/lazy
  containers `verdict=ok`); C4A 408 + 120 + 23 passed, `EXIT 0` (`verdict=ok` on every container row); Mac VM: the `sys::features`, `c_thp_*`,
  `c_features`, `api::print` and `c_prefault_both_ways` tests (the full `make test` was not run
  there this iteration).

## 4. Cost

Init only: `slice::probe()` once more (three GOT loads, one `flags` read; no syscall on a
kernel without the option), three `prctl`s at most (the query, the set, the undo; one more
under `advised`), one `madvise(MADV_POPULATE_WRITE)` and one `mincore` on a scratch page. No
fast path touched; the `quick` rows of the task 11 file (the same tree) are within one-round
spread of the base.
