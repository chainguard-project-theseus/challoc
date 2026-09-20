# The x86_64 Valkey write-phase reading, attributed (Phase 2.5 acceptance row 5, lane A, 2026-09-20)

Question (`bench/results/2026-09-20-phase-2.5-acceptance.md` row 5 and
section 5): on the reference C4 challoc's default read 1.5..4.9% behind
bundled jemalloc in the Valkey soak's write phase in two same-protocol runs
(`2026-09-19-x86_64-lane-d-percpu-baseline-soak/2026-09-19-x86_64-valkey-soak.md`:
valkey-benchmark 773,744 vs 784,980 req/s, memtier 956,617 vs 1,003,870;
`2026-09-20-x86_64-task11-prefault-soak-default-soak.md`: 769,744 / 946,081
against the same jemalloc column), while the delete phase, p99 and both RSS
criteria were met and the C4A reads at parity since task 4e. The plan's
criterion (Phase 1 soak, task 4d/4e): throughput and p99 at least equal to
bundled jemalloc; if the attribution shows the gap is outside the allocator
(client saturation, kernel), record that with the evidence; the tie-breaker
for "at least equal" is cycles per command from the perf sidecar.

Verdict in the plan's words: **the reading is outside the allocator**. Both
servers are main-thread bound and neither client is saturated, so the
throughput is the main thread's clock divided by its cycles per command; the
cycles per SET are challoc's by 1.4..4.8% at equal clock, and the vCPU's
effective clock moved by up to 4% between the two halves of this one soak
(3.08 against 3.20 GHz in the same minute of the protocol), which is the
size of every write-phase reading so far (-4.0%, +1.5%, +1.9%, +0.4%,
+4.9%, +5.8%). Inside the allocator one entry point is above jemalloc's
cost and is named: `je_malloc_usable_size` (Valkey's `zmalloc_size`
accounting query), +117 cycles per valkey-benchmark SET and +63 per memtier
SET, the bitmap-word load of its exact-liveness check; `je_malloc` and
`je_sdallocx` are at parity. It is compensated in the write phase by fewer
cycles in Valkey's own code, libc and the kernel, so the write-phase
criterion is met in cycles and in this run's throughput; in the delete phase
it is the +3.3% cycles per valkey-benchmark DEL that this run also shows.
The proposed change is a design note (section 8), not code.

## 1. Method

- host: `challoc-bench-amd64` (c4-standard-16, Xeon Platinum 8581C at 2.30
  GHz nominal, 16 vCPUs), Ubuntu kernel 7.0.0-1011-gcp, perf 7.0.14 on the
  VM host attached to the container's `valkey-server`; container Wolfi
  (`challoc-dev:amd64`). Tree e3e5fc3 (`main`, byte-identical to the lane
  branch). `make valkey STAGE=build` ran first (24 s; the quantum8
  `libchalloc.so` and `valkey-server` relinked at 09:57, `NEEDED
  libchalloc.so.0`, rpath `/src/target/valkey/challoc/release`), because
  PROGRESS.md notes this VM's Valkey build had linked a stale library
  before (the soak never rebuilds it).
- servers: Valkey 9.1.2 `MALLOC=challoc` (quantum8, dynamic) and the same
  tag with its bundled jemalloc 5.3.0 (static); `activedefrag yes`,
  `io-threads` default (1): every command runs on the main thread.
- workload: `make valkey-soak ALLOCS="challoc jemalloc" DURATION=1200
  OCCUPANCY=1` (the acceptance protocol: write phase 600 s alternating
  valkey-benchmark `-t set -d 512 -r 500000 -n 20000000 -c 50 -P 16` and
  memtier SET 16..1024 B, 4 x 10 clients, `-P 16`, 30 s; delete phase 600
  s alternating valkey-benchmark `del` and memtier DEL:SET 4:1; settle 60
  s). challoc 09:57..10:19 UTC, jemalloc 10:19..10:41. Soak summary:
  `2026-09-20-x86_64-valkey-write-phase-attribution-soak.md` (renamed from
  the soak's `<date>-x86_64-valkey-soak*`), samples `-soak-{challoc,jemalloc}.csv`,
  `MEMORY MALLOC-STATS` at t=580 and t=1180 in `-mstats-<alloc>-t{580,1180}.txt`.
- sidecar (root on the VM host, `bench/probes/valkey-perf-sidecar.sh
  ~/logs/wp 700 1205`): `perf stat -I 1000 -e task-clock,cycles,
  instructions,L1-dcache-load-misses,dTLB-load-misses,branch-misses -p
  <pid>` for each server's whole life (`-perf-stat-<alloc>.csv`); `perf
  record -e cycles -F 2999 -p <pid> -- sleep 60` at t=100..160 (write) and
  t=700..760 (delete); INFO commandstats / stats / memory and every
  thread's `utime+stime` before and after each window and at t=30 and
  t=1205 (`-snapshots-<alloc>.txt`). Symbols through `perf report
  --symfs` over the target volume and the image's libraries (the recipe of
  `bench/gce/perf-record.sh`; `-f`, because the data files had been
  chowned), kernel symbols with `--kallsyms /proc/kallsyms`; the four
  records' DSO tables, thread split, top 40 symbols, top 16 kernel symbols,
  the per-generator `--time` slices and the hottest lines of the three
  entry points are in `-perf-report.txt`.
- a second, unprivileged sampler every 5 s of each generator process's
  `/proc/<pid>/stat` (starttime, utime, stime, threads;
  `-generator-cpu.log`), because the sidecar's snapshots see only the
  generator alive at that instant and every generator run is a fresh
  process. Started 5 minutes into the challoc write phase, so it covers 8
  of 11 challoc write runs and everything after.
- per-operation columns: the `perf stat` seconds strictly inside a generator
  run (`runs.csv`, T0 from `samples.csv`) divided by that run's mean request
  rate, medians over the runs of a phase; "window" columns divide the
  counters between two snapshots by the `total_commands_processed` delta.
  Effective clock is `cycles / task-clock` (the main thread is the whole
  task-clock, section 3).

## 2. The soak (headline rows; positive means challoc did worse)

| measure | challoc | jemalloc | challoc vs jemalloc | 2026-09-19 (lane D VM) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 761992 | 731845 | **-4.0%** | +1.5% |
| write: valkey-benchmark p99 ms | 1.255 | 1.287 | -2.5% | -1.3% |
| write: memtier req/s | 912526 | 916554 | **+0.4%** | +4.9% |
| write: memtier p99 ms | 0.999 | 1.007 | -0.8% | +1.7% |
| delete: valkey-benchmark req/s | 1331038 | 1381126 | +3.8% | +0.1% |
| delete: valkey-benchmark p99 ms | 0.743 | 0.711 | +4.5% | -3.5% |
| delete: memtier req/s | 1200051 | 1246357 | +3.9% | -2.1% |
| delete: memtier p99 ms | 0.799 | 0.767 | +4.2% | -4.2% |
| peak RSS MiB (VmHWM) | 575.9 | 571.8 | +0.7% | +0.4% |
| RSS MiB after settle | 127.9 | 129.2 | -1.0% | -3.6% |

The three same-protocol readings on the C4 (yesterday's, today's default
run against yesterday's jemalloc column, and this one under the sidecar)
span -4.0..+1.9% on valkey-benchmark and +0.4..+5.8% on memtier: a spread
of 6..9 points around zero, of the size section 4 measures for the clock.
The perf sampling costs both halves alike (the aarch64 4d run said the same).
The RSS criteria hold again; the delete phase, met yesterday, reads 3.8..3.9%
behind today (section 6).

## 3. The server is the bottleneck, the client is not

Main thread CPU between the two snapshots of each record window (one CPU =
100%): challoc 99.9% (write) and 99.0% (delete), jemalloc 100.7% and 99.0%.
Everything else the server owns: the bio threads 0 s, `challoc-bg` 0.13 s
(write) and 0.19 s (delete) per 62 s, `jemalloc_bg_thd` 0.00..0.01 s. The
throughput is therefore `clock / cycles per command` on one thread, for
both servers.

Generators (`-generator-cpu.log`, CPU of the client process over its
sampled life; write-phase medians over 8..11 runs, min..max in brackets):

| phase | tool | challoc | jemalloc | threads |
|---|---|---:|---:|---:|
| write | valkey-benchmark | 39% [38..39] | 37% [35..39] | 1 |
| write | memtier | 97% [94..98] | 97% [96..100] | 5 (4 workers) |
| delete | valkey-benchmark | 59% [54..60] | 61% [60..64] | 1 |
| delete | memtier | 113% [109..114] | 114% [112..115] | 5 |

valkey-benchmark is single-threaded and uses 0.4 of a CPU in the write
phase, 0.6 in the delete phase; memtier's four workers use about 0.25 CPU
each. Neither client is saturated, and both use the same CPU under both
allocators. The p99 is the closed loop seen from the client (50 x 16 = 800
requests in flight; Little's law 800 / 761992 = 1.05 ms against the
measured 1.255, 800 / 731845 = 1.09 against 1.287; the p99 ratio 0.975
tracks the throughput ratio 0.960), as in the aarch64 4d run: there is no
separate latency effect.

INFO commandstats over the windows (server-side time per call, which
includes the allocator calls of the command body): write SET 0.219 us
(challoc) against 0.208 (jemalloc); delete SET 0.250 against 0.235, DEL
0.069 against 0.066. About one sixth of the 1.2 us the main thread spends
per command; the rest is the network stack and the event loop (section 5).

## 4. Cycles, instructions and the clock

Per operation, medians over the runs of a phase (`perf stat` seconds
inside each run):

| phase | tool | alloc | runs | req/s | p99 ms | cyc/op | ins/op | IPC | L1D miss/op | dTLB miss/op | br-miss/op | GHz | ns/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write | valkey-benchmark | challoc | 11 | 761992 | 1.255 | **4251** | 8298 | 1.95 | 37.3 | 3.3 | 2.52 | 3.260 | 1314 |
| write | valkey-benchmark | jemalloc | 11 | 731845 | 1.287 | **4464** | 8168 | 1.83 | 36.3 | 3.6 | 2.60 | 3.267 | 1366 |
| write | memtier | challoc | 11 | 912526 | 0.999 | 3458 | 8340 | 2.41 | 37.4 | 2.3 | 5.40 | 3.151 | 1095 |
| write | memtier | jemalloc | 11 | 916554 | 1.007 | 3506 | 8217 | 2.34 | 34.4 | 2.6 | 5.62 | 3.212 | 1092 |
| delete | valkey-benchmark | challoc | 13 | 1331038 | 0.743 | 2446 | 6003 | 2.45 | 13.3 | 0.2 | 3.18 | 3.260 | 751 |
| delete | valkey-benchmark | jemalloc | 13 | 1381126 | 0.711 | 2369 | 5824 | 2.46 | 13.1 | 0.1 | 2.88 | 3.280 | 722 |
| delete | memtier | challoc | 13 | 1200051 | 0.799 | 2646 | 6412 | 2.42 | 18.8 | 0.3 | 3.86 | 3.181 | 832 |
| delete | memtier | jemalloc | 13 | 1246357 | 0.767 | 2615 | 6292 | 2.41 | 18.1 | 0.3 | 3.63 | 3.270 | 799 |

Snapshot windows (the 60 s records; counters between the two snapshots
over the `total_commands_processed` delta):

| phase | alloc | window s | commands | cmd/s | cyc/cmd | ins/cmd | GHz | ns/cmd |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| write | challoc | 62 | 50751180 | 818567 | **3770** | 8331 | 3.081 | 1224 |
| write | jemalloc | 61 | 50324588 | 824993 | **3884** | 8134 | 3.204 | 1212 |
| delete | challoc | 62 | 74618110 | 1203518 | 2615 | 6306 | 3.143 | 832 |
| delete | jemalloc | 62 | 76876540 | 1239944 | 2607 | 6226 | 3.233 | 806 |

Reading. (1) In the write phase challoc runs fewer cycles per command than
jemalloc: -4.8% per valkey-benchmark SET at the same 3.26 GHz, -1.4% per
memtier SET, -2.9% in the window; it runs 1.5..2.4% more instructions
(+130 per SET, the allocator's, section 5) at a higher IPC. The
tie-breaker of task 4e reads in challoc's favour. (2) The effective clock
is not a constant of the run. Per 60 s of soak time (`cycles /
task-clock`, medians of the seconds):

| t (s) | 0 | 60 | 120 | 180 | 240 | 300 | 360 | 420 | 480 | 540 | 600 | 660 | 720 | 780 | 840 | 900 | 960 | 1020 | 1080 | 1140 | 1200 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc GHz | 3.114 | 3.123 | 3.109 | 3.162 | 3.251 | 3.221 | 3.224 | 3.248 | 3.247 | 3.166 | 3.162 | 3.164 | 3.133 | 3.181 | 3.173 | 3.237 | 3.193 | 3.247 | 3.196 | 3.220 | 3.238 |
| jemalloc GHz | 3.249 | 3.244 | 3.219 | 3.240 | 3.246 | 3.248 | 3.252 | 3.228 | 3.245 | 3.273 | 3.268 | 3.266 | 3.237 | 3.274 | 3.274 | 3.275 | 3.274 | 3.273 | 3.274 | 3.273 | 3.272 |

The clock ranges 3.08..3.27 GHz within one soak. It is higher during
valkey-benchmark runs (one busy client thread) than during memtier runs
(four), so it follows how many vCPUs are busy (the all-core turbo of the
host), and within the challoc half it rose from 3.11 to 3.25 GHz over the
first four minutes with the workload unchanged, so it also follows the
host's state at the time; the challoc half, which ran first, got the lower
clock in 19 of 21 minutes (up to -4.0% in the write windows, -2.8% in the
delete windows). The two halves of a soak are 21 minutes apart and the
runs of two days farther, and the throughput rows carry the clock ratio
directly: this is the run-to-run spread the plan calls generator noise,
and it is outside the allocator. In time per operation (cycles / GHz), the
write phase is -3.8% (valkey-benchmark) and +0.3% (memtier), the delete
phase +4.0% and +4.1%: the throughput rows to within a point.

## 5. Where the cycles land (perf record, write phase)

DSO shares of the 60 s records multiplied by the window's cycles per
command (jemalloc's allocator is the `je_*`, `tcache_*`, `arena_*`,
`extent_*`, `rtree_*`, `sz_*`, `edata_*`, `bin_*` symbols inside its
`valkey-server`):

| phase | alloc | valkey-server own | allocator | kernel | libc | cyc/cmd | allocator cyc | server cyc | kernel cyc | libc cyc |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write | challoc | 50.94% | 11.99% | 30.04% | 7.03% | 3770 | **452** | 1921 | 1133 | 265 |
| write | jemalloc | 52.44% | 9.67% | 29.72% | 8.16% | 3884 | **376** | 2037 | 1154 | 317 |
| delete | challoc | 52.03% | 10.44% | 29.19% | 8.33% | 2615 | 273 | 1361 | 763 | 218 |
| delete | jemalloc | 46.59% | 7.51% | 37.08% | 8.81% | 2607 | 196 | 1215 | 967 | 230 |

The write windows cover the same generator mix (818.6k against 825.0k
commands per second; each about 24 s of valkey-benchmark and 36 s of
memtier). The delete windows do not (challoc's covers 23 s of
valkey-benchmark DEL, jemalloc's 15 s; the DEL stream has the higher kernel
share per command), so the delete-phase DSO rows are not compared below;
the entry-point shares are.

Per generator inside the write record (`perf report --time` over each run's
seconds; shares of the slice normalised to the four DSO classes, which
carry 96..99% of the slice, times the per-run cycles per SET):

| SET of | | total | valkey-server own | allocator | kernel | libc | `je_malloc_usable_size` | `je_malloc` | `je_sdallocx` |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| valkey-benchmark | challoc | 4251 | 2270 | **526** | 1153 | 303 | **346** | 89 | 88 |
| valkey-benchmark | jemalloc | 4464 | 2501 | **406** | 1198 | 359 | **229** | 76 | 95 |
| valkey-benchmark | delta | -213 | -231 | +120 | -45 | -56 | **+117** | +13 | -7 |
| memtier | challoc | 3458 | 1793 | 425 | 988 | 253 | 250 | 74 | 98 |
| memtier | jemalloc | 3506 | 1862 | 383 | 949 | 312 | 187 | 83 | 99 |
| memtier | delta | -48 | -69 | +42 | +39 | -59 | **+63** | -9 | -1 |

(Raw slice shares, valkey-benchmark: challoc server 51.52% / allocator
11.94% / kernel 26.17% / libc 6.87%, `je_malloc_usable_size` 7.85%,
`je_malloc` 2.03%, `je_sdallocx` 1.99%; jemalloc 53.98 / 8.77 / 25.85 /
7.76, 4.94 / 1.64 / 2.05. Window-wide: `je_malloc_usable_size` 7.40%
against 5.02% = 279 against 195 cycles per command, `je_sdallocx` 2.42
against 2.40 = 91 against 93, `je_malloc` 2.07 against 1.96 = 78 against
76; every other allocator symbol is below 0.05%, so `calloc`, `realloc`,
plain `free`, the refill and the flush are not terms.)

The allocator is the only DSO class in which challoc spends more: +120
cycles per valkey-benchmark SET (+2.8% of the command) and +42 per memtier
SET, and it is one entry point, `je_malloc_usable_size` (+117 and +63);
`je_malloc` and `je_sdallocx` are at jemalloc's cost, as tasks 4c and 4e
intended (the sized free reads no per-slab metadata; the 09-18 aarch64
number for challoc's free path was 338 against 71). The kernel is the same
work in both halves: the write-phase kernel symbols are the TCP receive and
transmit paths (`_copy_to_iter` 5.55% against 5.48%, the `tcp_*`/`skb_*`/
`ep_*` family 12.89% against 12.88%), page-fault and purge symbols 0.09%
against 0.07%; there is no kernel term. Valkey's own code runs 231 fewer
cycles per SET under challoc (libc 56 fewer: `__memset_avx512_unaligned_erms`
3.15% against 3.87%); its symbol-level split differs between the two LTO
builds (`parseMultibulk` 2.20% against 10.18%, `zmalloc_usable` 4.73%
against 0.70%: the wrapper is inlined into its caller in one build and not
the other), so only the DSO total is comparable, and the per-operation
counters say where it comes from: the same L1D misses (37.3 against 36.3),
fewer dTLB misses (3.3 against 3.6 per SET) and a higher IPC (1.95 against
1.83) under challoc's per-class regions.

`MEMORY MALLOC-STATS` at t=580 (end of the write phase; classes with more
than 3000 live regions) says the class table is not a factor: Valkey's
data lands in the same classes with live-region counts equal to within 1%
under both allocators; only the slab geometry differs (challoc 64 KiB below
512 bytes, 4..16 KiB for 512..1024; jemalloc 4..28 KiB). Full tables in the
two `-mstats-*-t580.txt` files; the live regions of the value class (640
bytes): 547,789 in 21,922 slabs of 25 against 547,763 in 17,120 slabs of 32.

## 6. Inside `je_malloc_usable_size`

Valkey calls it on every allocation (`ztrymalloc_usable_internal`: `je_malloc`
then `zmalloc_size(ptr)` for the `used_memory` accounting) and on every
`zfree(ptr)` whose caller does not know the size (`zmalloc_size` then
`je_sdallocx`); `zfree_with_size` (sds strings, embedded objects) skips it.
The call count per SET was not measured: `perf probe -x libchalloc.so` refuses
`malloc`, `free`, `sdallocx`, `malloc_usable_size` and `calloc` ("Failed to
add events"; they are alias symbols, `realloc`/`rallocx`/`nallocx`/`sallocx`
probe fine and count 0 during SETs), so the costs here are per command.

challoc's query (`api::libc::usable_size`): `Regions::classify` (region ->
class -> slab and slot by the class reciprocal) then `SlabTable::check_live`
(the slot's bit in the dense per-class bitmap of task 4f) and
`CLASSES[class].size()`. Hottest lines of the function in the write record
(share of the symbol's samples): `btq %rcx,%r9` 20.9% (the bit test, the
consumer of the bitmap-word load), `testq %rdi,%rdi` 11.4% (the entry),
`notq %r8` 8.8%, `shrq $0x6` 4.9%, `shrq $0x20` 4.7%, `movl (%rax),%eax` 4.7%,
`subq 0x18(%r9,%rsi,8),%rdx` 3.7% (the classify chain: `SHARED` -> regions
-> `class_base[c]` -> `CLASSES[c]` -> table -> `bits_base` -> word, six
dependent loads before the test). In the delete record (a 60 MiB working
set, the bitmaps hot) the order inverts: `notq` 12.8%, `shrq $0x20` 7.4%,
`subq 0x18(...)` 6.9%, `btq` 6.6%: the arithmetic chain is the floor, the
bitmap word the write-phase excess. jemalloc's query is `rtree_szind_read`
(two dependent loads) and `sz_index2size_tab`: 49.8% of its samples sit on
the instruction after the rtree leaf load, one metadata miss per call, and
it still costs 229 cycles per SET to challoc's 346.

Why the word misses now: the sized free (task 4e) pushes without loading
the bitmap word, so the slot `je_malloc` pops next (LIFO: the one the last
sized free of a random key pushed) has a bitmap word nothing has touched
since its refill, and `zmalloc_size` right after `je_malloc` is its first
touch: one L1 miss per allocation (the +1.0 L1D miss per SET of section 4),
an L2 hit on this CPU (the bitmaps of a 550 MiB heap are about 200 KB), plus
the second query in `zfree` of the old value. The aarch64 run 2 of 4d
(09-18) tested `usable_size` without the bit test and found no gain because
`free` had just loaded the same descriptor line; 4e and 4f have since made
the query the only metadata touch on Valkey's SET path, so that premise is
gone.

The delete phase shows the same term without the compensation: per
valkey-benchmark DEL challoc runs 2446 against 2369 cycles (+3.3%) and per
memtier operation 2646 against 2615 (+1.2%); `je_malloc_usable_size` is
5.85% against 3.30% of the delete records (153 against 86 cycles per
command, +67, two queries per `dbDelete`), `je_sdallocx` 2.20% against
2.42% and `je_malloc` 1.58% against 1.67% at parity. The delete-phase
throughput rows (+3.8% / +3.9%) are that plus the clock (3.260 against
3.280 GHz on valkey-benchmark, 3.181 against 3.270 on memtier).

## 7. Conclusion

- The write-phase reading of rows 3 and 5 is outside the allocator. With
  both main threads saturated (99.9% / 100.7%) and neither client above 0.4
  (valkey-benchmark) or 1.0 (memtier, four workers) of a CPU, throughput is
  clock over cycles per command; challoc's cycles per SET are lower (4251
  against 4464 at 3.26 GHz on valkey-benchmark, 3458 against 3506 on
  memtier, 3770 against 3884 in the window), and the vCPU's effective clock
  varies by 4% between the halves of one soak, which is the amplitude of
  the readings (-4.0..+5.8% across three runs). In this run the write
  phase reads challoc ahead (-4.0% / +0.4%, p99 -2.5% / -0.8%); the
  criterion "throughput and p99 at least equal" is met on the tie-breaker
  in every run and on the throughput rows in this one.
- Inside the allocator the one term above jemalloc's cost on this CPU is
  `je_malloc_usable_size`: +117 cycles per valkey-benchmark SET, +63 per
  memtier SET, +67 per DEL, the bitmap-word load of the exact liveness
  check (an L1 miss per allocation since the sized free stopped touching
  the word) on top of a six-load classify chain. `je_malloc` and
  `je_sdallocx` are at parity; `calloc`, `realloc`, plain `free`, refill and
  flush are not terms. The kernel is identical work.
- The delete phase, met yesterday (+0.1% / -2.1%), reads +3.8% / +3.9% today
  and is the usable-size term (+3.3% cycles per DEL) plus the clock; it is
  the row the design note below would move.

## 8. Design note: the usable-size query (proposed, not implemented)

No code changed in this iteration (the tree under acceptance stays
byte-identical; the fast-path rule wants this attribution recorded first).
The change the data supports, for the human, since it trades a check like
task 4e did:

1. `usable_size` on a slab pointer answers from `Regions::classify` alone
   (class, slab and slot validated arithmetically; a pointer outside every
   region or not at a slot start still aborts) and returns
   `CLASSES[class].size()` without `check_live`; `realloc` keeps its check
   (it reads the old bytes). What changes: `malloc_usable_size` of a freed
   slot stops aborting at the call (the README security table row and
   design 6.3/13.2 would say so; the flush still catches the double free a
   cache's worth of frees later, as for `sdallocx`). Expected: up to 117
   cycles per SET on the C4 (2.7% of the command), less the classify chain
   that stays (the delete-phase annotate says the chain is about half of
   the function when the word is hot).
2. Or keep the check and shorten the chain: the word address needs
   `bits_base[c]` next to `class_base[c]` in the dense `Regions` arrays (one
   indexed load instead of `SHARED -> table(c) -> bits_base`), so the test
   is three dependent loads plus the word, like jemalloc's rtree. Smaller
   gain, no security change; inside lane A's partition (`src/heap.rs`,
   `src/layout.rs`).
3. Not a fix: a prefetch of the popped slot's bitmap word in `malloc`'s pop
   cannot hide an L2 hit that `zmalloc_usable` consumes within a few dozen
   instructions, and it adds instructions to the fast path the small-L3
   hosts pay for (task 4b's rule).

Measurement plan: the `valkey1-usable` row (`valkey_model -s -u`, the
flag that models `zfree`'s `malloc_usable_size` before every free) under
`quick-perf` on the C4 for cycles, instructions and L1D misses per SET,
`quick` on both reference VMs and a C3 lab cell, then this protocol with
the sidecar:
`je_malloc_usable_size` cycles per SET from 346 toward jemalloc's 229 and
the delete-phase cycles per DEL at parity, both recorded next to the soak.
