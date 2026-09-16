# bench/

Benchmark protocol for challoc (docs/plan.md, "Benchmark protocol"). Everything
runs inside the dev container on the native architecture; emulated x86_64 is
never used for numbers.

    make bench            build the programs (once), cargo build --release,
                          run the protocol, write bench/results/<date>-<arch>.md
    make bench-build      just fetch and build the programs

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
- `results/`: one file per run, named `<date>-<arch>.md`, with the commit
  hash, toolchain versions, kernel and CPU in the header.

Benchmark programs and their parameters follow mimalloc-bench's `bench.sh`
(`larson 5 8 1000 5000 100 4141 N`, `mstress N 50 25`, `xmalloc-test -w N -t 5
-s 64`, `alloc-test 1` and `alloc-test min(N,16)`, redis `-r 1000000 -P 16
lpush/lrange`) with N = `nproc`, except redis uses `-n 1000000` instead of
`100000` so a run lasts more than a second.
