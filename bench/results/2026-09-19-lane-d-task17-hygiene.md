# 2026-09-19: measurement hygiene from the kernel table (Phase 2.5 task 17, lane D)

Three harness items from the 2026-09-19 kernel rows of `docs/research.md` (6.12 `memory.peak`
write resets, 6.12 per-size anon THP counters, 6.16 `PAGEMAP_SCAN` reports guard regions),
nothing under `src/`. One commit each on `loop/lane-d`: a5a05c3 (item 1), faaadd4 (item 2),
26e897e (item 3); this file and the run outputs in the results commit that follows.

- VMs: `challoc-lane-d-arm64` (c4a-standard-16, Axion / Neoverse V2) and `challoc-lane-d-amd64`
  (c4-standard-16, Xeon Platinum 8581C), Ubuntu 24.04, kernel `7.0.0-1011-gcp`, 4 KiB pages,
  Docker 27 with the systemd cgroup driver (cgroup2 mounted `rw,nsdelegate,memory_recursiveprot`
  on the host, `ro` in a stock container). Both created for this task and deleted at its end.
- The Mac dev container (Docker Desktop, kernel `6.10.14-linuxkit`, cgroupfs driver) for the
  plumbing and every fallback branch that a kernel before 6.12 / 6.13 / 6.16 takes.
- Library: HEAD 79e3931 (main fa27979 plus the lane's acceptance commits), no `src/` change in
  this task; `make test` on the amd64 VM at the end of the task: 384 + 116 + 23 = 523 passed,
  0 failed, the container tests `OUTCOME ... verdict=ok` x 6, `pagemap_guard on` and `off` ok.

## 1. `memory.peak` reset per phase (a5a05c3)

What the kernel gives, found by probing, not by the version: from 6.12 a write to
`memory.peak` resets the high-water mark, **for the open file description that wrote it**
(`mm/memcontrol.c`, the per-fd `local_watermark`): `echo reset > memory.peak; cat memory.peak`
resets nothing a `cat` can see (measured on 7.0: the `cat` still read the birth peak,
4939776, while `memory.current` was 2822144). So the harness holds one descriptor per run in a
sidecar, `bench/probes/cgpeak.c`, driven by the phase file the Valkey soak already writes
(`$D/phase`, `$D/stop`), which records `cgpeak.<phase>` (the peak, bytes) and
`cgpeak.<phase>.file` (the cgroup's page cache at the phase end, since the peak counts it),
acknowledges each reset in `cgpeak.phase` so the load starts after it, and writes its verdict
to `cgpeak.state`; `bench/ruby/soak.rb` holds its own descriptor (Ruby `IO#pread`) and resets
at start and right after the `GC.compact` sample. `bench/lib.sh cgroup_peak_*` wraps the
sidecar.

Where the write happens, and which side: **inside the container**, but only with `make ...
CGROUP_PEAK=1`, which adds `--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw` to
`DOCKER_BASE`. Probed on both kernels:

| container | 6.10 (Mac) | 7.0 (VMs) |
|---|---|---|
| stock `docker run` | `/sys/fs/cgroup` mounted `ro`; `/proc/self/cgroup` = `0::/`; write: `Read-only file system` | the same (`ro,nsdelegate`) |
| `--cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw` | mount `rw`, path `/sys/fs/cgroup/docker/<id>`; write: `Invalid argument` (no write handler before 6.12) | mount `rw`, path `/sys/fs/cgroup/system.slice/docker-<id>.scope`; write accepted, per-phase peaks recorded |

Without the flags the nsdelegate boundary would refuse the write from inside the namespace
even on a writable mount (`cgroup_file_write`: a non-init namespace may not write its own
root's control files), so the flag pair is the container-side answer; a host-side reset was
not needed for the soaks. The campaign, which runs on the VM host, takes the host side
anyway: `bench/percpu-campaign.sh` polls `/sys/fs/cgroup/system.slice/docker-*.scope/memory.peak`
(or `/sys/fs/cgroup/docker/*/memory.peak`) every 2 s while a step runs and records the
largest per-container peak on the step's line; a `docker run --rm` container is a fresh cgroup,
so its `memory.peak` is the row's peak with no reset (and `memory.peak` is world-readable, no
sudo). The soak steps of the campaign get `CGROUP_PEAK=1`.

Fallbacks, each printed once with its reason and the rows left `-`, VmHWM kept:

- Mac 6.10, stock mount: `not reset per phase, VmHWM only: /sys/fs/cgroup/memory.peak: the
  container's cgroup tree is read-only (Docker's default); pass CGROUP_PEAK=1 to make ...`
- Mac 6.10, `CGROUP_PEAK=1`: `... the kernel 6.10.14-linuxkit does not accept a write to
  /sys/fs/cgroup/docker/<id>/memory.peak (the reset is 6.12+)`
- (5.10, 6.1, 6.8 cells were not created for this task; on them the second line is the one the
  sidecar prints, by the same EINVAL.)

Runs on the arm64 VM (7.0, `CGROUP_PEAK=1`), the new columns filled:

**Valkey soak**, `make valkey-soak CGROUP_PEAK=1 DURATION=120 SETTLE=30`
(`2026-09-19-aarch64-lane-d-task17-soak/2026-09-19-aarch64-valkey-soak.md`; 2 minutes of load
per allocator, a plumbing run, not the 20-minute protocol):

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak RSS MiB (VmHWM) | 575.7 | 568.9 | +1.2% |
| cgroup memory.peak MiB, write phase | 598.4 | 587.8 | +1.8% |
| cgroup memory.peak MiB, delete phase | 578.4 | 570.7 | +1.3% |
| cgroup memory.peak MiB, settle phase | 140.1 | 148.3 | -5.5% |

with the header line `cgroup page cache (memory.stat file) at the end of the write / delete /
settle phases: 0.1 / 0.1 / 0.1 MiB` (challoc) and `0.2 / 0.2 / 0.2` (jemalloc). The cgroup's
write-phase peak is 22 MiB (challoc) / 19 MiB (jemalloc) above the server's VmHWM: the load
generators (valkey-benchmark's 50 clients, memtier's 4 x 10) and the harness, which the cgroup
counts and VmHWM does not; the settle phase is the server after the deletes plus the idle
harness. The first run of the column (kept as `...-task17-soak-run1/`) had jemalloc at 907.5 /
889.9 / 467.8 MiB against challoc's 596.3 / 576.9 / 141.9 at equal VmHWM (574.2 / 570.7): the
jemalloc Valkey server was built inside the container right before its soak and the build's
page cache stayed charged to the cgroup. The soak now builds that server before any soak runs
and the sidecar records the page cache per phase so the reader can see it (0.1..0.2 MiB in the
second run); both changes are in a5a05c3.

**Ruby soak**, `make ruby-soak CGROUP_PEAK=1 DURATION=120 COMPACT_AT=60 ALLOCS='challoc
jemalloc'` (`2026-09-19-aarch64-lane-d-task17-ruby-soak.md`, 16 threads, 2 minutes):

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 296.2 | 108.8 | +172.2% |
| VmHWM at the end | 296.2 | 109.9 | +169.5% |
| cgroup memory.peak, start..compact | 355.0 | 166.7 | +113.0% |
| cgroup memory.peak, compact..end | 366.5 | 177.2 | +106.8% |

The cgroup rows sit 57..70 MiB above VmHWM for both allocators alike (the container's page
cache of the Ruby install read at start, the harness shell and the staged library), so the
ratio is what VmHWM says, not what the cgroup says; the column is there to make that visible.
(The gap itself is the 16-thread per-thread-arena shape the acceptance campaign measured at
403 / 199 MiB over 600 s, `2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak.md`; task 8's
`percpu_arena:percpu` is the answer under test, not this task's.) The run also fixed a latent
`set -e` exit in `bench/ruby-soak.sh` (a bare `COMMIT=...$(tree_dirty && echo ...)` took the
substitution's status 1 on a clean tree and the script left silently; the campaign passes
`COMMIT=` and never hit it).

**Campaign rows**, `make percpu-campaign STEPS=cgroup OSC_SECS=20 LANE=lane-d-task17`
(`2026-09-19-aarch64-lane-d-task17-percpu-baseline.md`), the host-side per-step peak:

```
-- cgroup-churn: make test-cgroup-churn : rc=0 in 10 s; cgroup memory.peak max 406.1 MiB over 4 container cgroup(s)
-- cgroup-lazy: make test-cgroup-lazy : rc=0 in 8 s; cgroup memory.peak max 205.2 MiB over 2 container cgroup(s)
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=20 : rc=0 in 24 s; cgroup memory.peak max 500.6 MiB over 1 container cgroup(s)
```

(406 MiB for a 400 MiB churn under a 512 MiB limit, 205 for the 200 MiB lazy churn, 501 for
the 200..480 MiB oscillation: the rows read what the programs do.)

## 2. THP counters around a row (faaadd4)

`CHALLOC_THP_COUNTERS=1` in `quick`, `ab` and `challoc-bench quick-perf`; the table and the
reading are in `2026-09-19-aarch64-lane-d-task17-thp-counters.md` (with
`...-thp-counters-ab-auto.md` and `...-ab-always.md`). In one paragraph: the dTLB column of
task 9 reproduces (auto state, `thp:auto`: larson1 3.98 -> 3.05, valkey1 2.82 -> 2.45 refills
per operation) and the folio counts behind it are 71..76 x 64 KiB for larson1 (4.4..4.75 MiB,
the +2.2 MiB RSS) and 2788 x 64 KiB = 174 MiB of valkey1's 585 MiB heap (only the classes with
64 KiB slabs are advised; the page-slab classes and the metadata keep 4 KiB pages, where the
remaining refills are); under `thp:always` with the PMD, 318 PMD faults cover the whole heap
and 280..282 of them are split again by the delete phase's purges within the run
(`thp_split_pmd`, the RSS price); `thp_collapse_alloc=1` per run whose `thp` is not `never` is
the `MADV_COLLAPSE` probe. In the default sysfs state `thp:auto` shows no folio at all, the
counter form of "inert on Ubuntu's stock table".

## 3. Guard slabs through `PAGEMAP_SCAN` (26e897e)

`tests/c/pagemap_guard.c`, `scripts/pagemap-guard-test.sh`, `make test-pagemap-guard` in
`make test` (after the cgroup targets). Both ways: under the default the guard slabs of the
16-, 64-, 256- and 1024-byte classes are located as the one-group gaps no allocation came from
(guard_slab.c's method from the public `mallctl` geometry) and scanned with
`ioctl(PAGEMAP_SCAN)` on `/proc/self/pagemap`, never touched; under `guards:false` no gap and no
guard page. Capabilities are probed by the ioctl's own answers (ENOTTY: no ioctl; EINVAL on
`PAGE_IS_GUARD` in the masks: no guard category) and by the allocator's `MADV_GUARD_INSTALL`
probe.

| kernel | what the program saw | branch | result |
|---|---|---|---|
| 7.0.0-1011-gcp, arm64 VM | `PAGEMAP_SCAN available with PAGE_IS_GUARD (6.16+); MADV_GUARD_INSTALL available` | guard page categories `0x112` (`PAGE_IS_GUARD | PAGE_IS_SWAPPED | PAGE_IS_WRITTEN`, not present), the slot pages before and after `0xa` (`PRESENT | WRITTEN`, not guard), exactly 1 `PAGE_IS_GUARD` page per guard slab and none elsewhere in the span, all four classes; `off`: no gap, no guard page | ok / ok |
| 7.0.0-1011-gcp, amd64 VM | the same | the same (`0x112` / `0xa`) | ok / ok; `make test` 523/0 with the target |
| 6.10.14-linuxkit, Mac container | `PAGEMAP_SCAN available, PAGE_IS_GUARD rejected (Invalid argument: kernel before 6.16); MADV_GUARD_INSTALL unavailable (kernel before 6.13: the allocator makes no guard slab)` | no one-group gap in any class's span (40960 / 10240 / 2560 / 520 blocks), the fallback branch named | ok / ok |

One correction the run forced: the first form asserted the guard page "not swapped"; on 7.0
the kernel's `pagemap_page_category` classifies the guard marker as a swap-type PTE first
(`PAGE_IS_SWAPPED | PAGE_IS_WRITTEN`) and then adds `PAGE_IS_GUARD`, so the test now expects
the swap bit with the guard bit, and the 6.13..6.15 fallback (ioctl without the category)
checks "not present, reported as a swap-type entry" rather than "neither". No 6.13..6.15 host
was available to run that branch; the 5.10 / 6.1 / 6.8 cells of task 3 would take the
"before 6.7" (5.10, 6.1) and "before 6.13" (6.8) branches, which the 6.10 run exercises in
substance (ioctl absent vs. present without guard slabs).

Not done here, for the coordinator: the test is a Makefile target, not a `src/api/ctests.rs`
driver, because lane D edits nothing under `src/`; folding it into the cargo set is one
`run_c("pagemap_guard", &["on"])` plus a `run_bin_env(... "off" ..., MALLOC_CONF=guards:false)`
driver, after which the Makefile target can stay as the standalone form or go.

## Acceptance (plan task 17)

- "the harness changes land with one re-run each showing the new columns": item 1 three
  re-runs above (Valkey soak, Ruby soak, campaign step), item 2 the `quick-perf`, `ab` and
  `quick` runs in the counters file. Met.
- "the guard check passes on 6.16+ and is skipped with a reason below": passes on 7.0 (both
  VMs, both ways); on 6.10 it runs its fallback branch and passes by it, saying which. Met.
