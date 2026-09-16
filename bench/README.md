# bench/

Benchmark protocol for challoc (docs/plan.md, "Benchmark protocol"). Everything
runs inside the dev container on the native architecture; emulated x86_64 is
never used for numbers.

    make bench            build the programs (once), cargo build --release,
                          run the protocol, write bench/results/<date>-<arch>.md
    make bench-build      just fetch and build the programs
    make valkey-soak      Valkey soak under challoc and its bundled jemalloc
                          (Phase 1 task 8); needs `make valkey STAGE=build`

Files:

- `build.sh`: clones pinned mimalloc-bench, redis 6.2.7 and mimalloc, builds
  `cfrac espresso larson mstress xmalloc-test alloc-test glibc-simple`,
  `redis-server`/`redis-benchmark`/`redis-cli` (`MALLOC=libc`) and
  `libmimalloc.so` into `target/bench-bin/`, plus the two helpers below.
- `rt.c`: `/usr/bin/time -v` replacement (Wolfi has no GNU time): wall time,
  user, sys, max RSS and exit code from `wait4`, appended to `$RT_OUT`.
- `sampler.c`: `perf record` replacement (Wolfi has no perf): a SIGPROF PC
  sampler preloaded after the allocator; writes `count module offset` lines.
- `run.sh`: one warm-up plus `RUNS` (5) timed runs per benchmark and
  allocator (`challoc jemalloc glibc mimalloc`), medians and raw runs into the
  results file. larson and xmalloc-test are fixed-duration, so they are scored
  by the throughput they print; redis by redis-benchmark's requests/second.
- `profile.sh`: larson under challoc with the sampler, attributed to symbols
  (`nm`) and instructions (`objdump`), flagging lock and atomic RMW
  instructions. Appended to the results file by `run.sh`.
- `valkey-soak.sh`: starts `valkey-server` from the challoc tree built by
  `contrib/valkey/build.sh` and from an unpatched clone of the same tag built
  with bundled jemalloc 5.3.0 (`target/valkey/src-<tag>-jemalloc`, built on
  first use), runs a write-heavy phase (valkey-benchmark `-t set`, then
  memtier_benchmark SET-only with 16-1024 byte values) and a delete-heavy phase
  (valkey-benchmark `del`, then memtier DEL:SET 4:1) of `DURATION/2` seconds
  each over a 500k-key keyspace, then `SETTLE` idle seconds. `INFO memory` and
  `INFO stats` are sampled every `INTERVAL` seconds into
  `results/<date>-<arch>-valkey-soak-<alloc>.csv` (used_memory, used_memory_rss,
  mem_fragmentation_ratio, allocator_frag_ratio, allocator_rss_ratio,
  active_defrag_hits, plus the throughput and client-side p99 of the most
  recent generator run); `results/<date>-<arch>-valkey-soak.md` compares
  throughput, p99, peak RSS (VmHWM) and post-settle RSS against jemalloc.
  Defaults are the Phase 1 acceptance protocol (20 minutes of load per
  allocator); `DURATION=120 SETTLE=30 CHUNK=10 VB_REQS=1000000
  RESULT_DIR=target/valkey/soak-results` is a five-minute smoke run.
- `results/`: one file per run, named `<date>-<arch>.md`, with the commit
  hash, toolchain versions, kernel and CPU in the header.

Benchmark programs and their parameters follow mimalloc-bench's `bench.sh`
(`larson 5 8 1000 5000 100 4141 N`, `mstress N 50 25`, `xmalloc-test -w N -t 5
-s 64`, `alloc-test 1` and `alloc-test min(N,16)`, redis `-r 1000000 -P 16
lpush/lrange`) with N = `nproc`, except redis uses `-n 1000000` instead of
`100000` so a run lasts more than a second.
