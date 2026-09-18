# challoc Valkey throughput and p99 diagnosis, 2026-09-18, aarch64 (GCE C4A)

Phase 1.5 task 4d (`docs/plan.md`). The soak's throughput and p99 rows have
read 2..8% behind Valkey's bundled jemalloc in every soak and did not move
with the per-thread arena default of task 4c, so this run attributes the gap
with hardware counters on the arm64 reference VM. Two soaks: run 1 attributes
(the tree at c18979d, `DURATION=600`), run 2 measures the fix the attribution
supports (the working tree of this iteration, the full 1200 s protocol).

- host: challoc-bench-arm64 (c4a-standard-16, Google Axion / Neoverse V2, 16
  vCPUs), Ubuntu 24.04 kernel 7.0.0-1011-gcp, perf 7.0.14 on the host attached
  to the container's `valkey-server`; container: Wolfi (rustc 1.98.1)
- servers: Valkey 9.1.2 from `contrib/valkey/build.sh` (`MALLOC=challoc`,
  quantum8 `libchalloc.so` linked dynamically) and the same tag built with its
  bundled jemalloc 5.3.0; `activedefrag yes`, `io-threads` default (1), so
  every command runs on the main thread
- workload: `bench/valkey-soak.sh` (`make valkey-soak`), write phase
  (valkey-benchmark `-t set -d 512 -r 500000 -n 20000000 -c 50 -P 16`, then
  memtier SET 16..1024 B, 4 x 10 clients, `-P 16`, 30 s, alternating), delete
  phase (valkey-benchmark `del`, then memtier DEL:SET 4:1), 60 s settle;
  `OCCUPANCY=1` for `MEMORY MALLOC-STATS` every 20 s
- sidecar (root, on the VM host, `.tmp-it22-sidecar.sh` in the iteration log):
  for each server `perf stat -I 1000 -e task-clock,cycles,instructions,
  L1-dcache-load-misses,dTLB-load-misses,branch-misses -p <pid>` for the
  server's whole life (five PMU events, no multiplexing); `perf record -e
  cycles -F 2999 -p <pid> -- sleep 60` at t=100..160 (write phase) and in the
  delete phase (run 1: t=400..460; run 2: t=700..760); `INFO commandstats`,
  `INFO stats` and every thread's `utime+stime` from `/proc` before and after
  each record window. Per-operation columns divide the perf rows that fall
  inside a generator run (from `runs.csv`, T0 from `samples.csv`) by that
  run's mean request rate; the "window" figures divide the counters between
  two snapshots by the `total_commands_processed` delta. Symbols through
  `perf report` on the live pid (the container's mount namespace) or, after
  the process exited, `--symfs` with copies of the container's binaries.

## Run 1: attribution (c18979d, `DURATION=600`)

Soak summary (`2026-09-18-aarch64-gce-valkey-throughput-soak1-valkey-soak.md`;
positive means challoc did worse):

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 765834 | 850492 | +11.1% |
| write: valkey-benchmark p99 ms | 1.255 | 1.127 | +11.4% |
| write: memtier req/s | 862606 | 899876 | +4.3% |
| write: memtier p99 ms | 1.055 | 1.023 | +3.1% |
| delete: valkey-benchmark req/s | 1335273 | 1419519 | +6.3% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.695 | +5.8% |
| delete: memtier req/s | 1152770 | 1236664 | +7.3% |
| delete: memtier p99 ms | 0.815 | 0.767 | +6.3% |
| peak RSS MiB (VmHWM) | 573.7 | 574.1 | -0.1% |
| RSS MiB after settle | 124.4 | 133.6 | -6.9% |

The perf sampling costs both halves the same; the write valkey-benchmark row
is wider than in the unsampled soaks (+7.7% in
`2026-09-17-aarch64-gce-narenas-default-valkey-soak.md`), the others match.

### 1. The server is the bottleneck, in both halves

Main-thread CPU between the snapshots around each record window: challoc
100.4% (write) and 100.5% (delete) of one CPU, jemalloc 100.5% and 100.5%.
The bio threads and the allocators' background threads used 0..0.3 s over the
whole soak. Throughput is therefore `1 / (cycles per command)` on the main
thread, and every gap below is a cycles-per-command gap. (The generators'
CPU was not captured: the sidecar's `pgrep -x` cannot match 16-character
process names; with the server saturated it is not needed.)

### 2. Cycles per command

`perf stat` rows inside generator runs (medians over the runs of a phase;
`cyc/op` is per request):

| phase | tool | alloc | req/s | p99 ms | cyc/op | ins/op | IPC | L1D miss/op | dTLB miss/op | br-miss/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| write | valkey-benchmark | challoc | 758892 | 1.255 | 3718 | 9983 | 2.69 | 42.6 | 64.3 | 4.66 |
| write | valkey-benchmark | jemalloc | 850492 | 1.131 | 3335 | 9667 | 2.90 | 39.3 | 47.4 | 4.47 |
| write | memtier | challoc | 854341 | 1.095 | 3313 | 10058 | 3.05 | 43.1 | 53.1 | 7.96 |
| write | memtier | jemalloc | 899876 | 1.027 | 3141 | 9763 | 3.10 | 39.6 | 38.3 | 8.57 |
| delete | valkey-benchmark | challoc | 1335273 | 0.735 | 2130 | 7159 | 3.36 | 20.0 | 21.1 | 3.76 |
| delete | valkey-benchmark | jemalloc | 1419519 | 0.695 | 2003 | 6888 | 3.43 | 18.8 | 21.9 | 3.75 |
| delete | memtier | challoc | 1152770 | 0.823 | 2414 | 7697 | 3.19 | 26.3 | 28.4 | 5.86 |
| delete | memtier | jemalloc | 1236664 | 0.767 | 2255 | 7455 | 3.31 | 24.9 | 24.9 | 5.60 |

Snapshot windows (`total_commands_processed` deltas): write 3704 against
3431 cycles per command (+8.0%), 10240 against 9927 instructions (+3.2%);
delete 2417 against 2268 (+6.6%), 7681 against 7406 (+3.7%). challoc runs
3% more instructions per command and loses the rest in stalls: +3.3 L1D
misses and +17 dTLB refills per SET in the write phase.

### 3. Where the cycles land: DSO shares of the 60 s records

| phase | alloc | valkey-server (own code) | allocator | kernel | libc |
|---|---|---:|---:|---:|---:|
| write | challoc | 47.10% | 18.67% (`libchalloc.so`) | 29.94% | 4.27% |
| write | jemalloc | 47.24% | 14.31% (`je_*` inside the binary) | 31.90% | 6.54% |
| delete | challoc | 42.67% | 13.73% | 38.55% | 5.03% |
| delete | jemalloc | 46.37% | 8.07% | 38.29% | 7.25% |

Multiplied by the window's cycles per command:

| phase | | challoc | jemalloc | delta |
|---|---|---:|---:|---:|
| write | allocator | 691 | 491 | +200 |
| write | Valkey's own code | 1745 | 1621 | +124 |
| write | kernel (TCP, epoll, copy_to_user) | 1109 | 1094 | +15 |
| write | libc (memset, memcpy, memchr) | 158 | 224 | -66 |
| write | total | 3704 | 3431 | +273 |
| delete | allocator | 332 | 183 | +149 |
| delete | Valkey's own code | 1031 | 1052 | -21 |
| delete | kernel | 932 | 868 | +63 |
| delete | libc | 122 | 164 | -43 |
| delete | total | 2417 | 2268 | +149 |

The allocator is the gap: +200 of +273 cycles per command in the write
phase, the whole +149 in the delete phase. The kernel's share is the same
work in both halves (the network stack for the same request stream). The
remaining +124 in Valkey's own code in the write phase comes with the +17
dTLB refills per operation (review P3: 52 cache headers on 52 pages plus a
descriptor page per free) and possibly data placement; it is not separable
from this record.

### 4. Inside the allocator: symbol shares of all server samples

| phase | challoc | | jemalloc | |
|---|---|---:|---|---:|
| write | `challoc::arena::free` (the `free` / `sdallocx` path) | 8.97% | `je_sdallocx` | 2.06% |
| write | `<SlabTable>::check` (called from `usable_size`) | 4.20% | `je_malloc_usable_size` | 10.28% |
| write | `api::libc::usable_size` | 3.38% | | |
| write | `je_malloc` | 1.63% | `je_malloc` | 1.52% |
| write | `je_malloc_usable_size`, `je_sdallocx` wrappers, stats | 0.49% | flush, fill, stats, events | 0.45% |
| delete | `<SlabTable>::check` | 4.22% | `je_malloc_usable_size` | 4.25% |
| delete | `api::libc::usable_size` | 4.01% | | |
| delete | `challoc::arena::free` | 2.62% | `je_sdallocx` | 2.14% |
| delete | `je_malloc` | 1.63% | `je_malloc` | 1.45% |
| delete | flush sort, `claim`, wrappers | 1.25% | rest | 0.23% |

In cycles per command, write phase: challoc's free path 338 against
jemalloc's 71 (+267); the usable-size query 288 (`check` + `usable_size` +
wrapper) against 353 (-65); `malloc` 60 against 52.

How Valkey calls the allocator (`src/zmalloc.c` of 9.1.2): every allocation
is `je_malloc` followed by `zmalloc_size(ptr)` = `je_malloc_usable_size`
(`ztrymalloc_usable_internal`, to account `used_memory`); `zfree(ptr)` is
`zmalloc_size` then `je_sdallocx(ptr, size, 0)`; `zfree_with_size` (sds
strings and objects whose size the caller knows) is `je_sdallocx` alone.

Annotations (`perf annotate`, share of the symbol's cycles on one line):

- `challoc::arena::free`, write: 73% on `lsr x9, x9, x3` at 0x142e8, the
  consumer of the slab descriptor's bitmap-word load (the exact `free`
  check: the slot's bit must be set). The main thread frees the old value
  and object of a random key per SET, whose descriptor is not in cache on a
  550 MiB heap (21.9k slabs in the 640-byte class alone). In the delete
  phase the same line is 19% (the working set is 60 MiB).
- `<SlabTable>::check`, write: `ldr w12, [x12]` 17% and the `tbz` after the
  `lsr` 20%: the same descriptor word, loaded by `usable_size` for the
  allocation `zmalloc` has just made (the slot came from the thread cache,
  so nothing else has touched its descriptor).
- `challoc::api::libc::usable_size`: its own hot lines are the return from
  `check` and the `CLASSES[class].size()` load.
- `je_malloc` (challoc): 20% on `cbz w3` after the cache header `len` load,
  30% on `ret` (skid from the slot load): the review's malloc profile.
- `je_malloc_usable_size` (jemalloc): 58% on one instruction after the
  rtree leaf load (`isalloc` through `rtree_szind_read`): jemalloc pays one
  metadata miss per allocation there, as challoc does in `check`.
- `je_sdallocx` (jemalloc): no line above 17%; with the size hint jemalloc's
  free computes the bin from the size and pushes to the cache bin without
  touching any per-slab metadata.

So the structural difference is the free path: Valkey frees with a size
hint, jemalloc's sized free touches no metadata, and challoc's `sdallocx`
ignores the hint and runs the exact check, one descriptor miss per free.
The usable-size query costs both allocators one metadata miss per
allocation; challoc's is the cheaper of the two, but it is a miss the design
does not need, because the usable size of a slot is a property of its class.

### 5. Class table and slab geometry for Valkey's size mix

`MEMORY MALLOC-STATS` at the same points of both soaks (challoc arena 0,
jemalloc's merged arenas; only classes with more than 3000 live regions).
The quantum8 build's class sizes are jemalloc's, class for class, and the
live-region counts match to within 1..3%: Valkey's data lands in the same
classes under both allocators, so the class table is not a throughput
factor. The slabs differ: challoc 64 KiB below 512 bytes, 4..16 KiB for
512..1024 (the 1/32 waste rule of iteration 15); jemalloc 4..28 KiB.

t=280 s, end of the write phase:

| class | challoc regs/slab | slab | live regs | slabs | jemalloc regs/slab | slab | live regs | slabs |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 | 4096 | 64 KiB | 11483 | 3 | 256 | 4 KiB | 11201 | 44 |
| 24 | 2730 | 64 KiB | 3877 | 2 | 512 | 12 KiB | 1551 | 4 |
| 32 | 2048 | 64 KiB | 340290 | 167 | 128 | 4 KiB | 348953 | 2729 |
| 40 | 1638 | 64 KiB | 501010 | 306 | 512 | 20 KiB | 500120 | 977 |
| 48 | 1365 | 64 KiB | 3197 | 3 | 256 | 12 KiB | 3163 | 13 |
| 56 | 1170 | 64 KiB | 3084 | 3 | 512 | 28 KiB | 3011 | 7 |
| 64 | 1024 | 64 KiB | 50084 | 50 | 64 | 4 KiB | 51907 | 812 |
| 80 | 819 | 64 KiB | 6526 | 8 | 256 | 20 KiB | 6096 | 25 |
| 96 | 682 | 64 KiB | 6107 | 10 | 128 | 12 KiB | 6122 | 49 |
| 112 | 585 | 64 KiB | 8174 | 15 | 256 | 28 KiB | 7912 | 32 |
| 128 | 512 | 64 KiB | 12543 | 25 | 32 | 4 KiB | 12274 | 388 |
| 160 | 409 | 64 KiB | 11787 | 30 | 128 | 20 KiB | 12138 | 96 |
| 192 | 341 | 64 KiB | 12198 | 36 | 64 | 12 KiB | 12198 | 193 |
| 224 | 292 | 64 KiB | 11810 | 41 | 128 | 28 KiB | 12050 | 96 |
| 256 | 256 | 64 KiB | 11778 | 47 | 16 | 4 KiB | 11978 | 757 |
| 320 | 204 | 64 KiB | 23187 | 114 | 64 | 20 KiB | 23541 | 371 |
| 384 | 170 | 64 KiB | 23784 | 140 | 32 | 12 KiB | 24301 | 764 |
| 448 | 146 | 64 KiB | 23909 | 165 | 64 | 28 KiB | 24466 | 387 |
| 512 | 8 | 4 KiB | 23650 | 2962 | 8 | 4 KiB | 24374 | 3054 |
| 640 | 25 | 16 KiB | 547296 | 21901 | 32 | 20 KiB | 548451 | 17151 |
| 768 | 21 | 16 KiB | 47419 | 2260 | 16 | 12 KiB | 48487 | 3046 |
| 896 | 9 | 8 KiB | 47036 | 5230 | 32 | 28 KiB | 48100 | 1515 |
| 1024 | 4 | 4 KiB | 47253 | 11853 | 4 | 4 KiB | 48728 | 12185 |

t=580 s, delete phase:

| class | challoc regs/slab | slab | live regs | slabs | jemalloc regs/slab | slab | live regs | slabs |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 | 4096 | 64 KiB | 11615 | 3 | 256 | 4 KiB | 11191 | 44 |
| 24 | 2730 | 64 KiB | 3828 | 2 | 512 | 12 KiB | 1542 | 4 |
| 32 | 2048 | 64 KiB | 95405 | 117 | 128 | 4 KiB | 93919 | 1900 |
| 112 | 585 | 64 KiB | 3097 | 9 | 256 | 28 KiB | 2129 | 20 |
| 128 | 512 | 64 KiB | 4694 | 16 | 32 | 4 KiB | 3540 | 218 |
| 160 | 409 | 64 KiB | 3605 | 17 | 128 | 20 KiB | 3275 | 54 |
| 192 | 341 | 64 KiB | 3829 | 20 | 64 | 12 KiB | 3324 | 106 |
| 224 | 292 | 64 KiB | 3621 | 24 | 128 | 28 KiB | 3267 | 53 |
| 256 | 256 | 64 KiB | 3604 | 28 | 16 | 4 KiB | 3208 | 382 |
| 320 | 204 | 64 KiB | 6692 | 65 | 64 | 20 KiB | 6286 | 214 |
| 384 | 170 | 64 KiB | 6769 | 81 | 32 | 12 KiB | 6573 | 427 |
| 448 | 146 | 64 KiB | 6757 | 92 | 64 | 28 KiB | 6572 | 223 |
| 512 | 8 | 4 KiB | 6703 | 1290 | 8 | 4 KiB | 6563 | 1317 |
| 640 | 25 | 16 KiB | 13175 | 1062 | 32 | 20 KiB | 12931 | 914 |
| 768 | 21 | 16 KiB | 13081 | 1246 | 16 | 12 KiB | 13098 | 1675 |
| 896 | 9 | 8 KiB | 12949 | 2354 | 32 | 28 KiB | 12917 | 881 |
| 1024 | 4 | 4 KiB | 13378 | 4076 | 4 | 4 KiB | 13140 | 4366 |

### 6. Client-side p99 against server time

`INFO commandstats` over the write window: SET 0.323 us per call under
challoc against 0.223 under jemalloc (the command body, which includes the
frees of the old value and object and the two allocations); delete window:
SET 0.266 against 0.236, DEL 0.068 against 0.068. Both are three to four
orders of magnitude below the client p99 (0.7..1.3 ms). The generators are
closed loops: valkey-benchmark keeps 50 x 16 = 800 requests in flight, so
its latency is in-flight / throughput (Little's law: 800 / 765834 = 1.04 ms
against the measured p99 1.255; 800 / 850492 = 0.94 against 1.127), and the
p99 ratio (1.114) equals the throughput ratio (1.111) in every row of every
soak. The p99 criterion is the throughput criterion seen from the client;
there is no separate server-side latency effect to chase, and the client is
not saturated (the server is).

### 7. Conclusion of the attribution

The gap is inside the allocator and on the main thread: +200 of +273 cycles
per command (write), +149 of +149 (delete), and it is the exact `free`
check. Valkey frees with a size hint (`je_sdallocx`); jemalloc's sized free
derives the bin from the size and touches no per-slab metadata; challoc's
`free` / `sdallocx` loads the slab descriptor's bitmap word to test the
slot's bit, and on a heap that outgrows the cache that load misses on every
free of a randomly chosen key (338 against 71 cycles per SET). The
usable-size query looked like a second miss per allocation in this run's
shares and was tested in run 2: it is not (its descriptor line is the one
`free` just loaded), so `usable_size` keeps its check.

Keeping the exact check at the call means keeping the miss. Two allocator-
side designs would recover most of the 267 cycles per SET, both changing
*when* a double free is detected and therefore left to the human
(PROGRESS.md, notes): (a) jemalloc parity, `sdallocx` trusts the size hint
and the bit test moves to the cache flush, where `release` already clears
the bit and can abort on an already-clear one; (b) a one-call deferral,
`free` issues the descriptor load, pushes the slot and returns, and the
next allocator call on that heap completes the pending bit test. The
remaining +124 cycles in Valkey's own code in the write phase come with +15
dTLB refills per SET (62 against 47), the review's P3 item and plan task 5.

## Run 2: `usable_size` without the descriptor bit test (variant, `DURATION=1200`)

Variant: `usable_size` on a slab pointer validates the class region and
the slot start (`SlabTable::locate`) and returns the class size without
loading the slab descriptor; `free`, `realloc` and `sdallocx` unchanged.
Working tree of this iteration on top of c18979d, quantum8 rebuild by
`make valkey STAGE=build`, `make test` 223 + 76 green on both VMs, `make
quick` go on both VMs (larson1 -0.2% / +1.1%, larson10 +2.3% / +0.2%,
xmalloc -0.1% / +3.2%, alloc-test1 +0.0% / +0.7%, churn compacts). Same
soak protocol, `DURATION=1200` (600 s write, 600 s delete), same sidecar
with the record windows at t=100..160 and t=700..760
(`2026-09-18-aarch64-gce-valkey-throughput-soak2-valkey-soak.md`).

| measure | variant | jemalloc | variant vs jemalloc | run 1 (c18979d) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 788364 | 858890 | +8.9% | +11.1% |
| write: valkey-benchmark p99 ms | 1.231 | 1.127 | +9.2% | +11.4% |
| write: memtier req/s | 880783 | 907860 | +3.1% | +4.3% |
| write: memtier p99 ms | 1.071 | 1.039 | +3.1% | +3.1% |
| delete: valkey-benchmark req/s | 1361444 | 1436033 | +5.5% | +6.3% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.703 | +4.6% | +5.8% |
| delete: memtier req/s | 1207960 | 1240007 | +2.7% | +7.3% |
| delete: memtier p99 ms | 0.791 | 0.775 | +2.1% | +6.3% |
| peak RSS MiB (VmHWM) | 573.9 | 571.4 | +0.4% | -0.1% |
| RSS MiB after settle | 123.3 | 126.4 | -2.5% | -6.9% |

Per operation (medians over the phase's runs) and snapshot windows:

| phase | tool | alloc | req/s | cyc/op | ins/op | IPC | L1D miss/op | dTLB miss/op |
|---|---|---|---:|---:|---:|---:|---:|---:|
| write | valkey-benchmark | variant | 788364 | 3658 | 9665 | 2.64 | 41.8 | 61.7 |
| write | valkey-benchmark | jemalloc | 858890 | 3364 | 9671 | 2.87 | 42.3 | 47.3 |
| write | memtier | variant | 880783 | 3247 | 9732 | 3.01 | 42.5 | 50.9 |
| write | memtier | jemalloc | 907860 | 3168 | 9767 | 3.09 | 39.3 | 38.3 |
| delete | valkey-benchmark | variant | 1361444 | 2110 | 6957 | 3.30 | 21.1 | 21.3 |
| delete | valkey-benchmark | jemalloc | 1436033 | 2019 | 6890 | 3.41 | 19.2 | 22.6 |
| delete | memtier | variant | 1207960 | 2365 | 7468 | 3.16 | 26.9 | 27.7 |
| delete | memtier | jemalloc | 1240007 | 2284 | 7467 | 3.27 | 24.9 | 25.0 |

Windows: write 3720 against 3443 cycles per command (run 1: 3704 against
3431), 10135 against 9926 instructions (run 1: 10240 against 9927); delete
2348 against 2292 (run 1: 2417 against 2268), 7327 against 7442
instructions. Main thread 98.9..100.5% of one CPU in every window; SET
server call time 0.330 against 0.221 us (write).

DSO and symbol shares (write / delete):

| | variant | jemalloc |
|---|---:|---:|
| allocator share of the server's cycles | 18.26% / 12.60% | 14.06% / 7.96% |
| free path (`arena::free` / `je_sdallocx`) | 10.26% / 3.17% | 1.99% / 2.11% |
| usable-size query (`usable_size` / `je_malloc_usable_size`) | 5.62% / 6.39% | 10.12% / 4.19% |
| `malloc` | 1.93% / 1.56% | 1.50% / 1.45% |

Reading: the variant removes the check's instructions (write: 105 fewer
per command, the `check` symbol is gone) and no cycles (3720 against 3704
per command; the throughput rows sit where every earlier soak put them).
The annotation says why: the variant's `usable_size` has no line above 13%
(four loads and shifts share its cycles evenly, so nothing misses), while
`arena::free` keeps 74% of its cycles on the descriptor-word consumer. The
slot `zmalloc` queries after `malloc` is the one the LIFO cache handed back
last, and its descriptor line is the one the preceding `free` loaded, so
the usable-size check was hitting L1 all along; the miss the design pays is
the first touch in `free`, and it stays with the exact check. The variant
was reverted: a use-after-free query stops aborting for no measurable gain.

Protocol A/B rows of the variant (same libraries, Phase 0 set, which never
calls the usable-size query; recorded because the iteration built them):
`2026-09-18-x86_64-gce-usable-size.md` (larson1 +1.1%, larson10 -2.9%,
larson16 -0.5%, xmalloc +2.6%, alloc-test1 +4.9%, alloc-testN -0.1% against
old, all inside the rows' run-to-run spread) and
`2026-09-18-aarch64-gce-usable-size.md` (larson1 +0.0%, larson10 -0.0%,
larson16 +0.5%, xmalloc +0.3%, alloc-test1 -0.3%, alloc-testN -0.3%).
