## Done

- Phase 3 task 5 (a): JVM rows done. Harness a70dc91 (`bench/jvm-bench.sh`,
  `make jvm-bench` with `JVM_BENCH_VARS`, `bench/jvm/HttpBench.java`,
  `bench/probes/statsdump.c`, the `# JVM rows (Phase 3 task 5a)` block in
  Dockerfile.dev: `openjdk-21`, `apache2-utils`; the `bench/README.md`
  paragraph), results 469d72d (`bench/results/2026-09-20-aarch64-jvm.md`,
  `bench/results/2026-09-20-x86_64-jvm.md`), measured on the lane's VMs
  `challoc-lane-j-{arm64,amd64}` (C4A / C4, kernel 7.0.0-1011-gcp, Wolfi
  openjdk-21 21.0.12.1-r2, ab 2.4.68), library from 693f14e. Six variants:
  challoc, glibc, glibc with `MALLOC_ARENA_MAX=2`, jemalloc, mimalloc,
  tcmalloc. Rows: `java -version` (5 interleaved rounds after a warm-up)
  and the 200-thread `com.sun.net.httpserver` server (`-Xms512m -Xmx512m
  -XX:+AlwaysPreTouch`, /json and 64 KiB direct-ByteBuffer /blob under two
  `ab -k -c 32` for the full 300 s per allocator, RSS sampled every 5 s).
  Headline, aarch64: startup 0.024 s / 39.1 MiB against glibc 0.022 / 67.1
  and jemalloc 0.023 / 42.0; server peak Rss 845 MiB against glibc 844,
  glibc-arena2 798, jemalloc 981; req/s 92969 / 64500 (json / blob)
  against glibc 93673 / 64723, jemalloc 88791 / 62823; CPU per request
  46.3 us against 46.0 / 47.1. x86_64: startup 0.029 s / 41.9 MiB against
  glibc 0.027 / 39.4 and jemalloc 0.028 / 44.2; server peak Rss 859 MiB
  against glibc 871, glibc-arena2 842, jemalloc 1035; req/s 84119 / 47503
  against glibc 86812 / 49028, jemalloc 87500 / 49208; CPU per request
  57.3 us against 57.6 / 57.5. No row is more than 10% behind the best of
  glibc and jemalloc on time or RSS on either architecture; the nearest
  is startup (+9.1% aarch64, +7.4% x86_64: a 2 ms constant), attributed
  with `perf stat` on the VM hosts to about 1270 / 720 more first-touch
  page faults and 9..12 M more instructions than glibc (per-class slab
  pages and per-thread heaps for the JVM's 15 startup threads; jemalloc
  pays the same count). x86_64 serves 3.2% fewer requests than glibc at
  equal CPU per request; inside the band, mechanism not named, see the
  results file.

## Design decisions

- Phase 3 task 5 (a): the load generator is `ab` (Wolfi's `apache2-utils`);
  `wrk` and `oha` are not in Wolfi (checked 2026-09-20). Two `ab -k -r -l
  -c 32` processes run concurrently, one per endpoint, for the 64
  connections and "both endpoints" of the plan; `-n` after `-t` because
  `-t` alone caps ab at 50000 requests. Each ab runs under `bench/rt` so
  its CPU share is on record (63..87% of a core in the runs: not saturated).
- Phase 3 task 5 (a): `-Dsun.net.httpserver.nodelay=true` in the default
  `JAVA_OPTS`. The JDK server leaves TCP_NODELAY off and writes headers
  and body separately, so the small JSON response waits for the client's
  delayed ACK: 775 req/s at p99 44 ms against 76k req/s on /blob in the
  Mac container without it. Without the flag the JSON row measures the
  ACK timer, not the allocator.
- Phase 3 task 5 (a): `-XX:+AlwaysPreTouch` beside the plan's fixed
  `-Xms/-Xmx`, so the Java heap is resident from the start under every
  allocator and the RSS columns differ by native memory only;
  `-XX:MaxDirectMemorySize` left at its default (= `-Xmx`), so the direct
  buffers churn up to 512 MiB before `Bits.reserveMemory` forces a GC.
- Phase 3 task 5 (a): the server's CPU per request (`/proc/<pid>/stat`
  over the measured window divided by the requests both generators
  completed) is a column beside req/s: it is the allocator's cost per unit
  of work whether or not the closed-loop generator sets the rate, and it
  is what separated "3% fewer requests" from "3% more CPU" on x86_64.
- Phase 3 task 5 (a): challoc's `MALLOC_CONF=stats_print:true` is inert
  (`src/config.rs` INERT_KEYS), so the server row preloads
  `bench/probes/statsdump.so` after the allocator: an `atexit` hook that
  calls `malloc_stats_print` through `dlsym` where the allocator exports
  it (challoc, jemalloc) and does nothing elsewhere. Harness only, no
  `src/` change (lane J changes nothing under `src/`).
- Phase 3 task 5 (a): the plan's "idle RSS" is the server's Rss 10 s after
  it answered, before the load (the `idle Rss` column), beside `java
  -version`'s max RSS; the 5-minute rows ran in full, no shortening.

## Notes for the human

- Phase 3 task 5 (a): glibc's `java -version` max RSS is 67.1 MiB on the
  aarch64 VM against 39.4 MiB on x86_64 (glibc-arena2: 41.3 / 39.3), a
  Wolfi aarch64 glibc arena artefact of the JVM's 15 startup threads, not
  a challoc matter; recorded in the aarch64 file.
- Phase 3 task 5 (a): ab's percentiles are integer milliseconds, so p99 at
  2 ms cannot separate the allocators at these rates; only glibc-arena2 on
  aarch64 stood out (4 ms, arena-lock contention across 200 threads).
- Phase 3 task 5 (a): the JVM rows are not scorecard columns; that is the
  coordinator's decision afterwards (plan acceptance text). The
  Dockerfile.dev block must be unioned with the other lanes' blocks at the
  merge; the Mac image was built under the lane-private tag
  `challoc-dev:lane-j`, the shared `challoc-dev:arm64` tag was not touched.
