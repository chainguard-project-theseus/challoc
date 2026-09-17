# challoc architecture review: hardware counters on the GCE reference VMs, 2026-09-17

Measurements taken for the step-back architecture review (`docs/review-2026-09-17.md`)
before any code was read. Commit `dacbedd` (the iteration-12 sweep: the working tree
includes the UNMEASURED address-ordered compacting cache flush in `src/heap.rs`).

- hosts: challoc-bench-arm64 (c4a-standard-16, Google Axion, Neoverse V2) and
  challoc-bench-amd64 (c4-standard-16, Intel Xeon Platinum 8581C), 16 vCPUs each,
  Ubuntu 24.04 kernel 7.0.0-1011-gcp, 4 KiB pages, THP `madvise`,
  `guard_install=yes`; container: Wolfi (rustc 1.98.1, gcc 15.3.0); jemalloc =
  Wolfi's libjemalloc.so.2 (5.3.1), mimalloc = v3.5.2 built by `bench/build.sh`
- method: the benchmark runs detached in the dev container (`sleep 1.5` before
  `exec` so perf attaches first); `sudo perf stat -e <six events> --inherit -p
  <container init pid>` on the VM host counts the whole container (the
  benchmark process, its threads, and the `sh -c` parent, which is negligible).
  Two rounds per (program, allocator). Perf must run as root: attaching to
  the container's root-owned process from an unprivileged user fails with
  EACCES even at `perf_event_paranoid = -1` (the challoc-bench `perf-stat`
  subcommand has the same limitation; use `sudo perf`).
- programs: `larson 5 8 1000 5000 100 4141 1` (larson1), `... 10` (larson10),
  `xmalloc-test -w 10 -t 5 -s 64` (xmalloc), `alloc-test 1` (alloctest)
- columns: raw cycles and instructions for the whole run; IPC; misses per
  1000 instructions (branch, L1D load, dTLB load, iTLB load); cycles and
  instructions per benchmark operation where the program reports an
  operation count (larson: throughput x relative time; xmalloc: frees/s x 5 s;
  alloc-test reports none). Compare allocators within a program on the
  per-operation columns; the raw columns differ with run length.

## perf stat, arm64 (C4A, Neoverse V2)

Event semantics on this core: `dTLB-load-misses` is `L1D_TLB_REFILL` (L1 data TLB refills, most of which hit the 2048-entry L2 TLB), `L1-dcache-load-misses` is `L1D_CACHE_REFILL`. All six events were scheduled with one multiplexed out (83% running time), so counts are scaled by perf.

| program | alloc | round | cycles | instr | IPC | br-miss/ki | L1D-miss/ki | dTLB/ki | iTLB/ki | cyc/op | ins/op | L1D/op | dTLB/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| alloctest | challoc | r1 | 7.76e+09 | 1.4e+10 | 1.81 | 19.04 | 6.41 | 8.97 | 0.001 | - | - | - | - |
| alloctest | jemalloc | r1 | 7.64e+09 | 1.33e+10 | 1.74 | 20.56 | 5.75 | 7.34 | 0.004 | - | - | - | - |
| alloctest | mimalloc | r1 | 7.35e+09 | 1.14e+10 | 1.56 | 24.64 | 13.53 | 0.04 | 0.003 | - | - | - | - |
| alloctest | challoc | r2 | 7.75e+09 | 1.4e+10 | 1.81 | 19.08 | 6.40 | 9.01 | 0.001 | - | - | - | - |
| alloctest | jemalloc | r2 | 7.52e+09 | 1.33e+10 | 1.77 | 20.52 | 5.75 | 7.21 | 0.005 | - | - | - | - |
| alloctest | mimalloc | r2 | 7.34e+09 | 1.14e+10 | 1.56 | 24.57 | 13.53 | 0.04 | 0.003 | - | - | - | - |
| larson1 | challoc | r1 | 1.41e+10 | 4.83e+10 | 3.43 | 0.06 | 9.29 | 14.87 | 0.004 | 67.5 | 231.4 | 2.15 | 3.44 |
| larson1 | jemalloc | r1 | 1.4e+10 | 4.74e+10 | 3.38 | 0.10 | 9.24 | 7.74 | 0.003 | 65.9 | 223.0 | 2.06 | 1.73 |
| larson1 | mimalloc | r1 | 1.4e+10 | 3.88e+10 | 2.77 | 0.48 | 28.56 | 0.03 | 0.004 | 69.1 | 191.4 | 5.47 | 0.01 |
| larson1 | challoc | r2 | 1.41e+10 | 4.85e+10 | 3.45 | 0.05 | 9.26 | 14.76 | 0.004 | 67.3 | 232.0 | 2.15 | 3.42 |
| larson1 | jemalloc | r2 | 1.4e+10 | 4.75e+10 | 3.39 | 0.10 | 9.23 | 7.58 | 0.003 | 65.9 | 223.2 | 2.06 | 1.69 |
| larson1 | mimalloc | r2 | 1.4e+10 | 3.87e+10 | 2.77 | 0.49 | 28.53 | 0.03 | 0.004 | 69.3 | 191.8 | 5.47 | 0.01 |
| larson10 | challoc | r1 | 1.39e+11 | 3.89e+11 | 2.80 | 0.08 | 12.50 | 16.47 | 0.004 | 83.3 | 233.0 | 2.91 | 3.84 |
| larson10 | jemalloc | r1 | 1.39e+11 | 4.02e+11 | 2.89 | 0.11 | 9.95 | 8.48 | 0.004 | 77.3 | 223.5 | 2.22 | 1.90 |
| larson10 | mimalloc | r1 | 1.39e+11 | 3.32e+11 | 2.39 | 0.52 | 29.40 | 0.86 | 0.005 | 80.5 | 192.2 | 5.65 | 0.17 |
| larson10 | challoc | r2 | 1.4e+11 | 3.9e+11 | 2.80 | 0.08 | 12.45 | 16.71 | 0.004 | 83.3 | 232.9 | 2.90 | 3.89 |
| larson10 | jemalloc | r2 | 1.39e+11 | 3.98e+11 | 2.86 | 0.10 | 9.98 | 8.41 | 0.003 | 78.1 | 223.5 | 2.23 | 1.88 |
| larson10 | mimalloc | r2 | 1.39e+11 | 3.31e+11 | 2.38 | 0.52 | 29.62 | 0.82 | 0.005 | 80.9 | 192.4 | 5.70 | 0.16 |
| xmalloc | challoc | r1 | 1.3e+11 | 5.04e+11 | 3.88 | 0.22 | 1.12 | 0.31 | 0.014 | 59.9 | 232.4 | 0.26 | 0.07 |
| xmalloc | jemalloc | r1 | 1.92e+11 | 6.12e+11 | 3.20 | 0.60 | 5.28 | 0.24 | 0.008 | 94.1 | 300.6 | 1.59 | 0.07 |
| xmalloc | mimalloc | r1 | 1.69e+11 | 1.14e+11 | 0.67 | 0.42 | 17.24 | 0.36 | 0.068 | 319.3 | 215.1 | 3.71 | 0.08 |
| xmalloc | challoc | r2 | 1.26e+11 | 4.63e+11 | 3.68 | 0.22 | 1.16 | 0.37 | 0.015 | 63.3 | 232.9 | 0.27 | 0.09 |
| xmalloc | jemalloc | r2 | 1.92e+11 | 6.02e+11 | 3.14 | 0.60 | 5.27 | 0.24 | 0.008 | 95.8 | 300.6 | 1.58 | 0.07 |
| xmalloc | mimalloc | r2 | 1.69e+11 | 1.22e+11 | 0.72 | 0.39 | 16.70 | 0.37 | 0.066 | 303.9 | 218.6 | 3.65 | 0.08 |

## perf stat, x86_64 (C4, Xeon Platinum 8581C, 16 vCPUs = 8 cores x 2 hyperthreads)

Event semantics on this core: `dTLB-load-misses` is `DTLB_LOAD_MISSES.MISS_CAUSES_A_WALK` (second-level TLB misses, i.e. page walks), so it is not comparable with the Arm column; `L1-dcache-load-misses` is `L1D.REPLACEMENT`. The 10-thread rows run on 8 physical cores, so two threads share a core's L1 and pipeline; the cycles-per-op growth from 1 to 10 threads includes that sharing for every allocator.

| program | alloc | round | cycles | instr | IPC | br-miss/ki | L1D-miss/ki | dTLB/ki | iTLB/ki | cyc/op | ins/op | L1D/op | dTLB/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| alloctest | challoc | r1 | 1.1e+10 | 1.53e+10 | 1.39 | 15.28 | 7.02 | 0.02 | 0.001 | - | - | - | - |
| alloctest | jemalloc | r1 | 1.06e+10 | 1.42e+10 | 1.34 | 16.45 | 6.85 | 0.01 | 0.000 | - | - | - | - |
| alloctest | mimalloc | r1 | 1.03e+10 | 1.29e+10 | 1.25 | 18.13 | 11.72 | 0.00 | 0.000 | - | - | - | - |
| alloctest | challoc | r2 | 1.1e+10 | 1.53e+10 | 1.39 | 15.30 | 7.00 | 0.01 | 0.000 | - | - | - | - |
| alloctest | jemalloc | r2 | 1.06e+10 | 1.42e+10 | 1.34 | 16.46 | 6.87 | 0.01 | 0.000 | - | - | - | - |
| alloctest | mimalloc | r2 | 1.03e+10 | 1.29e+10 | 1.25 | 18.13 | 11.73 | 0.00 | 0.000 | - | - | - | - |
| larson1 | challoc | r1 | 1.62e+10 | 5.12e+10 | 3.16 | 0.07 | 8.37 | 0.00 | 0.001 | 66.8 | 211.1 | 1.77 | 0.00 |
| larson1 | jemalloc | r1 | 1.62e+10 | 4.5e+10 | 2.77 | 0.49 | 10.78 | 0.00 | 0.002 | 72.5 | 200.9 | 2.17 | 0.00 |
| larson1 | mimalloc | r1 | 1.63e+10 | 3.46e+10 | 2.13 | 0.46 | 19.21 | 0.00 | 0.002 | 80.8 | 171.8 | 3.30 | 0.00 |
| larson1 | challoc | r2 | 1.63e+10 | 4.98e+10 | 3.06 | 0.07 | 8.14 | 0.00 | 0.001 | 68.9 | 211.1 | 1.72 | 0.00 |
| larson1 | jemalloc | r2 | 1.62e+10 | 4.46e+10 | 2.74 | 0.49 | 10.68 | 0.00 | 0.002 | 73.2 | 200.9 | 2.15 | 0.00 |
| larson1 | mimalloc | r2 | 1.63e+10 | 3.48e+10 | 2.14 | 0.50 | 18.61 | 0.00 | 0.002 | 80.8 | 172.9 | 3.22 | 0.00 |
| larson10 | challoc | r1 | 1.59e+11 | 2.67e+11 | 1.68 | 0.10 | 12.63 | 0.18 | 0.021 | 125.7 | 211.8 | 2.67 | 0.04 |
| larson10 | jemalloc | r1 | 1.58e+11 | 2.67e+11 | 1.68 | 0.47 | 10.93 | 0.03 | 0.005 | 119.5 | 201.1 | 2.20 | 0.01 |
| larson10 | mimalloc | r1 | 1.58e+11 | 2.17e+11 | 1.37 | 0.56 | 20.54 | 0.00 | 0.002 | 128.1 | 175.5 | 3.61 | 0.00 |
| larson10 | challoc | r2 | 1.59e+11 | 2.63e+11 | 1.66 | 0.10 | 12.57 | 0.19 | 0.026 | 127.9 | 211.8 | 2.66 | 0.04 |
| larson10 | jemalloc | r2 | 1.58e+11 | 2.78e+11 | 1.76 | 0.47 | 10.94 | 0.02 | 0.005 | 114.5 | 201.1 | 2.20 | 0.00 |
| larson10 | mimalloc | r2 | 1.58e+11 | 2.24e+11 | 1.42 | 0.56 | 20.38 | 0.00 | 0.003 | 123.9 | 175.5 | 3.58 | 0.00 |
| xmalloc | challoc | r1 | 2.34e+11 | 5.55e+11 | 2.37 | 0.14 | 0.50 | 0.01 | 0.005 | 95.8 | 227.5 | 0.11 | 0.00 |
| xmalloc | jemalloc | r1 | 2.44e+11 | 3.76e+11 | 1.54 | 0.97 | 1.67 | 0.01 | 0.003 | 153.0 | 235.3 | 0.39 | 0.00 |
| xmalloc | mimalloc | r1 | 2.44e+11 | 2.61e+11 | 1.07 | 0.22 | 2.84 | 0.00 | 0.015 | 169.3 | 180.5 | 0.51 | 0.00 |
| xmalloc | challoc | r2 | 2.33e+11 | 5.53e+11 | 2.37 | 0.14 | 0.50 | 0.01 | 0.007 | 96.1 | 227.5 | 0.11 | 0.00 |
| xmalloc | jemalloc | r2 | 2.43e+11 | 3.74e+11 | 1.54 | 0.97 | 1.66 | 0.01 | 0.003 | 153.1 | 235.2 | 0.39 | 0.00 |
| xmalloc | mimalloc | r2 | 2.43e+11 | 2.61e+11 | 1.07 | 0.22 | 2.80 | 0.00 | 0.016 | 169.6 | 182.2 | 0.51 | 0.00 |

## Second event group, larson (one round)

Per operation (ops = throughput x 5 s). arm64 events: `L1-dcache-loads`, `l2d_cache_refill` (L2 misses), `l2d_cache` (L2 accesses), `stall_backend`, `stall_frontend` (cycles). x86_64 only had `L1-dcache-loads` (the LLC events were not counted on this PMU level).

| arch | program | alloc | M ops/s | loads/op | L1D-miss/op | L2-miss/op | L2-acc/op | backend-stall cyc/op | frontend-stall cyc/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| arm64 | larson1 | challoc | 42.4 | 79.8 | 2.13 | 0.042 | 4.48 | 28.1 | 0.38 |
| arm64 | larson1 | jemalloc | 42.9 | 84.7 | 2.04 | 0.043 | 4.18 | 25.7 | 0.54 |
| arm64 | larson1 | mimalloc | 41.1 | 72.5 | 5.45 | 0.045 | 11.04 | 31.7 | 0.72 |
| arm64 | larson10 | challoc | 340.2 | 81.8 | 2.89 | 0.225 | 6.28 | 43.6 | 0.65 |
| arm64 | larson10 | jemalloc | 363.6 | 86.3 | 2.21 | 0.174 | 4.54 | 36.0 | 0.75 |
| arm64 | larson10 | mimalloc | 347.4 | 74.5 | 5.65 | 0.196 | 11.49 | 42.1 | 1.03 |
| amd64 | larson1 | challoc | 47.4 | 60.6 | 1.77 | - | - | - | - |
| amd64 | larson1 | jemalloc | 45.0 | 58.2 | 2.14 | - | - | - | - |
| amd64 | larson1 | mimalloc | 39.4 | 47.4 | 3.20 | - | - | - | - |
| amd64 | larson10 | challoc | 251.4 | 60.8 | 2.67 | - | - | - | - |
| amd64 | larson10 | jemalloc | 288.9 | 58.2 | 2.17 | - | - | - | - |
| amd64 | larson10 | mimalloc | 254.9 | 47.8 | 3.57 | - | - | - | - |

## perf record, larson, cycles by DSO and symbol

`perf record -e cycles -F 2999` attached to the container (arm64) or system-wide (x86_64, where the per-pid attach missed the worker threads). The C++ operator bodies are the exported `malloc`/`free` fast paths (identical code folded at link time, so perf names them `operator new[]`/`operator delete[]`). Shares are of all samples in the process.

| arch | program | alloc | larson exercise_heap | allocator total | operator delete[] (free) | operator new[] (malloc) | rest of allocator |
|---|---|---|---:|---:|---:|---:|---:|
| arm64 | larson1 | challoc | 50.09% | 48.06% | 30.15% | 16.80% | 1.11% |
| arm64 | larson10 | challoc | 60.48% | 38.04% | 24.36% | 12.01% | 1.67% |
| arm64 | larson10 | jemalloc | 56.55% | 41.05% | 27.85% | 10.36% | 2.84% |
| amd64 | larson1 | challoc | 53.94% | 41.10% | 27.51% | 11.86% | 1.73% |
| amd64 | larson10 | challoc | 66.19% | 30.87% (top 12 symbols) | 18.34% | 11.02% | 1.51% |
| amd64 | larson10 | jemalloc | 56.47% | 37.94% (top 12 symbols) | 26.63% | 6.17% | 5.14% |

## narenas A/B, larson 10 threads, x86_64

Same protocol. `MALLOC_CONF=narenas:N` shards threads over N arenas in both allocators (challoc default is 1 arena; jemalloc's default is 4 x CPUs, so each larson thread gets its own arena and its own slabs).

| program | alloc | round | cycles | instr | IPC | br-miss/ki | L1D-miss/ki | dTLB/ki | iTLB/ki | cyc/op | ins/op | L1D/op | dTLB/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson10 | challoc-a1 | r1 | 1.59e+11 | 2.73e+11 | 1.72 | 0.10 | 12.53 | 0.24 | 0.024 | 123.0 | 211.8 | 2.65 | 0.05 |
| larson10 | challoc-a16 | r1 | 1.59e+11 | 2.92e+11 | 1.84 | 0.12 | 7.79 | 0.05 | 0.003 | 118.4 | 217.6 | 1.69 | 0.01 |
| larson10 | jemalloc-a1 | r1 | 1.57e+11 | 2.3e+11 | 1.46 | 0.60 | 14.27 | 0.20 | 0.009 | 139.2 | 203.8 | 2.91 | 0.04 |
| larson10 | jemalloc-def | r1 | 1.58e+11 | 2.73e+11 | 1.73 | 0.47 | 10.92 | 0.03 | 0.005 | 116.2 | 201.1 | 2.20 | 0.01 |
| larson10 | challoc-a1 | r2 | 1.59e+11 | 2.64e+11 | 1.66 | 0.10 | 12.51 | 0.18 | 0.013 | 127.7 | 211.8 | 2.65 | 0.04 |
| larson10 | challoc-a16 | r2 | 1.59e+11 | 3.02e+11 | 1.90 | 0.12 | 7.80 | 0.04 | 0.005 | 114.8 | 217.6 | 1.70 | 0.01 |
| larson10 | jemalloc-a1 | r2 | 1.57e+11 | 2.34e+11 | 1.49 | 0.60 | 14.24 | 0.20 | 0.009 | 136.7 | 203.8 | 2.90 | 0.04 |
| larson10 | jemalloc-def | r2 | 1.58e+11 | 2.84e+11 | 1.80 | 0.47 | 10.97 | 0.02 | 0.006 | 112.0 | 201.1 | 2.21 | 0.00 |

- larson10.challoc-a1.r1: Throughput = 258130006 operations per second, relative time: 3.874s.
- larson10.challoc-a1.r2: Throughput = 249652920 operations per second, relative time: 4.006s.
- larson10.challoc-a16.r1: Throughput = 268693487 operations per second, relative time: 3.722s.
- larson10.challoc-a16.r2: Throughput = 277424285 operations per second, relative time: 3.605s.
- larson10.jemalloc-a1.r1: Throughput = 226033158 operations per second, relative time: 4.424s.
- larson10.jemalloc-a1.r2: Throughput = 230148147 operations per second, relative time: 4.345s.
- larson10.jemalloc-def.r1: Throughput = 271805303 operations per second, relative time: 3.679s.
- larson10.jemalloc-def.r2: Throughput = 282373852 operations per second, relative time: 3.541s.


Reading: sharing slabs between threads costs jemalloc 19% of its cycles per
operation (`narenas:1` 138 vs default 114 cyc/op, L1D misses 2.9 vs 2.2 per
op) and costs challoc the whole of its 10-thread gap: with `narenas:16` challoc
goes from 125 to 117 cyc/op and 2.65 to 1.70 L1D misses per op, below
jemalloc's default (2.2), at the price of 6 more instructions per op (the
`multi_arena` arena check on `free` and the arena routing). Throughput:
challoc `narenas:16` 269/277 M ops/s against jemalloc default 272/282 and
challoc default 258/250.

## Where the cycles land inside the fast paths (x86_64, larson 10 threads, default arenas)

`perf annotate` of the system-wide record (28.8k samples in `operator delete[]`
= `free`, 17.8k in `operator new[]` = `malloc`, 66% of all samples in larson's
own `exercise_heap`):

- `free`: 37.5% of its cycles sit on `btq %r9,%rcx`, the consumer of the
  descriptor bitmap-word load (`movq 0x10(%rsi,%rcx,8)`), i.e. the
  invalid-free check is one L1/L2 miss per call under sharing. The next
  largest are 4.6% after the `class_base[c]` load, 4.3% after the TLS load,
  4.6%/4.1% on the reciprocal multiply chain, 3.4% after the
  `committed_slabs` load, 3.9% on the cache push. Nothing else above 3%.
- `malloc`: 26.7% on `testl %ecx,%ecx` (consumer of the cache header `len`
  load) plus 17.8% on the following `cmpl 0x8(%rax)` (same line: the
  per-class cache header line is the miss), 16.9% right after the
  `SMALL_LOOKUP` table load, 11.2% on `retq` (skid from the slot load),
  7.1% after the `allocs` increment.
- `exercise_heap`: 33% of larson's own cycles sit on the instruction after
  `movb $0x61,(%rax)`, the first-byte store into the freshly allocated block.
  Under one shared arena, neighbouring slots of the same cache line belong
  to different threads (every class whose size is not a multiple of 64
  bytes has slots straddling lines), so the caller's first touch of its
  block is a coherence miss. This is the "gap sits in larson's own code"
  signature from the Mac sampler runs; the `narenas:16` row above removes
  it.

## Churn probe: does a frozen slab layout drain under balanced churn? (x86_64, single thread)

`.tmp-probe/churn.c` (kept in `bench/probes/churn.c` after this review): allocate
N = 30000 blocks of one size (100% occupancy), free a random 56% (the soak's
post-delete occupancy), then run a strict free-one/allocate-one churn for 20 s
and print the bin's `curslabs`/`curregs` through `mallctl` every second. This
is the Valkey delete-phase steady state reduced to one class and one thread.

| allocator and configuration | class | slabs at 44% | slabs after 20 s churn | occupancy after | ops/s |
|---|---:|---:|---:|---:|---:|
| challoc default (128-slot cache, compacting flush) | 1024 | 469 | 469 | 44.6% | 18.8 M |
| challoc default | 640 | 295 | 295 | 44.5% | 19.5 M |
| challoc `tcache_nslots_small_max:64` (8-slot cache) | 1024 | 469 | 469 | 44.3% | 18.8 M |
| challoc `tcache:false` (every op through the slab bitmaps) | 1024 | 469 | 208 (within 3 s) | 99.9% | 6.4 M |
| jemalloc 5.3.1 default (4-region slabs, 20-slot tcache) | 1024 | 6798 | 6481 | 51.3% | 18.0 M |
| jemalloc default (32-region slabs, 64-slot tcache) | 640 | 938 | 938 | 44.3% | 18.8 M |

Reading: with any per-thread cache, however small, the layout never moves:
under a free/allocate alternation the cache's length is constant, so it never
reaches empty or full, no refill and no flush ever happens, and every
allocation is served the slot that was just freed (LIFO). The flush policy
(oldest or highest address) is irrelevant because the flush never runs. With
the cache off, challoc's address-ordered first fit compacts the class from 469
to 208 slabs in under three seconds and `resident` follows after the decay
window: the slab machinery is right, the cache defeats it. jemalloc drains
slowly (4.7% of its 1 KiB slabs in 20 s) because its GC runs every 64 KiB of
allocation regardless of the cache level and forces the bin to refill from
the lowest slab a few slots at a time, and only its 4-region 1 KiB bin drains
at all in this window; its 32-region 640 bin is as frozen as challoc's.

## narenas A/B, larson 10 threads, aarch64

| program | alloc | round | cycles | instr | IPC | br-miss/ki | L1D-miss/ki | dTLB/ki | iTLB/ki | cyc/op | ins/op | L1D/op | dTLB/op |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson10 | challoc-a1 | r1 | 1.4e+11 | 3.93e+11 | 2.81 | 0.07 | 12.48 | 16.71 | 0.004 | 82.9 | 233.0 | 2.91 | 3.89 |
| larson10 | challoc-a16 | r1 | 1.39e+11 | 4.17e+11 | 2.99 | 0.11 | 9.62 | 15.62 | 0.004 | 79.8 | 238.6 | 2.30 | 3.73 |
| larson10 | jemalloc-a1 | r1 | 1.39e+11 | 3.87e+11 | 2.78 | 0.13 | 10.27 | 8.36 | 0.003 | 80.4 | 223.7 | 2.30 | 1.87 |
| larson10 | jemalloc-def | r1 | 1.39e+11 | 4.01e+11 | 2.88 | 0.10 | 9.92 | 8.32 | 0.003 | 77.5 | 223.5 | 2.22 | 1.86 |
| larson10 | challoc-a1 | r2 | 1.4e+11 | 3.9e+11 | 2.79 | 0.07 | 12.49 | 16.56 | 0.004 | 83.5 | 233.0 | 2.91 | 3.86 |
| larson10 | challoc-a16 | r2 | 1.39e+11 | 4.21e+11 | 3.02 | 0.11 | 9.65 | 15.36 | 0.004 | 79.0 | 238.5 | 2.30 | 3.66 |
| larson10 | jemalloc-a1 | r2 | 1.39e+11 | 3.84e+11 | 2.76 | 0.13 | 10.32 | 8.37 | 0.003 | 80.9 | 223.6 | 2.31 | 1.87 |
| larson10 | jemalloc-def | r2 | 1.39e+11 | 4.03e+11 | 2.90 | 0.10 | 9.95 | 8.39 | 0.003 | 77.2 | 223.5 | 2.22 | 1.88 |

- larson10.challoc-a1.r1: Throughput = 337247450 operations per second, relative time: 2.965s.
- larson10.challoc-a1.r2: Throughput = 334756827 operations per second, relative time: 2.987s.
- larson10.challoc-a16.r1: Throughput = 349167988 operations per second, relative time: 2.864s.
- larson10.challoc-a16.r2: Throughput = 353051202 operations per second, relative time: 2.832s.
- larson10.jemalloc-a1.r1: Throughput = 345838910 operations per second, relative time: 2.892s.
- larson10.jemalloc-a1.r2: Throughput = 343819328 operations per second, relative time: 2.909s.
- larson10.jemalloc-def.r1: Throughput = 358884447 operations per second, relative time: 2.786s.
- larson10.jemalloc-def.r2: Throughput = 360181858 operations per second, relative time: 2.776s.

Reading: the same shape as on x86_64 with smaller amplitude. `narenas:16`
takes challoc from 83.2 to 79.4 cycles per operation and from 2.91 to 2.30
L1D misses per operation (jemalloc forced to one arena: 2.30, its default:
2.22); jemalloc loses 4% when forced to share (80.7 vs 77.4). The residual
2.4% between challoc `narenas:16` and jemalloc default on this core is the
one-thread residue: 3.7 L1 dTLB refills per operation against 1.9 (the
per-thread cache and descriptor pages, review P3/P4) and 15 more
instructions per operation. The aarch64 `exercise_heap` annotation has the
same signature as x86_64: 32% of larson's own cycles on the instruction
after `strb w11, [x0]`, the first-byte store into the new block.

## Instruction-level attribution, aarch64 (larson 10 threads, default arenas, system-wide record)

- `free` (`operator delete[]`): 31.6% of its cycles on `tbz w11, #0` (the
  consumer of the descriptor bitmap-word load), 9.1% on the
  `committed_slabs` load, 7.5%/7.4% on the class arithmetic, 7.2% after
  the arena-flag load. Same shape as x86_64.
- `malloc` (`operator new[]`): 37.7% on `str x9, [x8, #0x10]` (the `allocs`
  increment, i.e. the first store to the per-class cache header line after
  its `len`/`low_water` loads), 20% on `cbz w9` (the `len` test), 7.1% on the
  `len` load itself. The header line is the cost, as on x86_64.
