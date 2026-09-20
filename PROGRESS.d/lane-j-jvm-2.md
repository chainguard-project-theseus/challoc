## Done

- Phase 3 task 5 (a): second iteration on the JVM rows done (the human's
  reading of the first: parity with glibc because a 5-minute run with one
  buffer size never lets glibc's arenas fragment). Harness 359ae7f,
  1f49f91 (the NMT peak snapshot waits out the gap instead of dropping the
  peak), 585e20b (the soak's own OUT); results 3578c2c. Measured on the
  lane's VMs `challoc-lane-j-{arm64,amd64}` (C4A / C4, kernel
  7.0.0-1011-gcp, Wolfi openjdk-21 21.0.12.1-r2), library from f862134
  (`src/` identical to main 6208172). (1) NMT column: the server row's
  JVMs run with `-XX:NativeMemoryTracking=summary`, `jcmd VM.native_memory
  summary` at the peak-Rss sample and at the end, "allocator overhead" =
  Rss minus NMT committed as a column of the server table plus the
  category table. At peak the allocators add beyond the JVM's request:
  x86_64 glibc-trim 50 MiB, glibc-arena2 132, tcmalloc 156, glibc 160,
  challoc 194, jemalloc 203, mimalloc 286; aarch64 glibc-arena2 43,
  glibc-trim 78, tcmalloc 87, glibc 123, challoc 201, jemalloc 277,
  mimalloc 359. At the end (2 s after the load) challoc is second only to
  glibc-trim on both (58 / 78 MiB against glibc's 111 / 88). (2) The
  seventh variant `glibc-trim` (`-XX:TrimNativeHeapInterval=5000`,
  accepted by this JDK 21): the fair glibc for a Java image; server peak
  Rss challoc +5.7% (x86_64) / +10.4% (aarch64) above it, VmHWM +2.5% /
  +3.1%, req/s within 3.7%, CPU per request within 1%. Full seven-variant
  server tables in `bench/results/2026-09-20-{aarch64,x86_64}-jvm.md`
  (re-run with the peak-snapshot fix; the first run's peaks are in the
  readings as the run-to-run band: +-15 MiB on x86_64, glibc and mimalloc
  +-40 on aarch64). NMT's 16-byte header moves `malloc(65536)` to 65552,
  one class up in every size-class allocator: challoc +5..9 MiB (x86_64) /
  +34..36 (aarch64) at peak against the NMT-less first iteration, mimalloc
  +100..138, glibc none. (3) The mixed-size soak
  (`bench/results/2026-09-20-{aarch64,x86_64}-jvm-soak.md`, series CSVs
  beside them), 1200 s per allocator in full, both VMs in parallel:
  x86_64 peak / final / slope (MiB, MiB/min over the last 600 s): challoc
  1237 / 1017 / -2.96, glibc 1260 / 1260 / +2.19, glibc-arena2 1021 / 984 /
  +0.20, glibc-trim 1070 / 817 / -2.32, jemalloc 1308 / 1152 / +4.55;
  aarch64: challoc 1271 / 1082 / -3.68, glibc 1433 / 1327 / -0.98,
  glibc-arena2 1099 / 1031 / -1.91, glibc-trim 1145 / 889 / -2.59,
  jemalloc 2066 / 1654 / -1.85. Allocator overhead at the end: challoc
  294 / 398 MiB against glibc 563 / 613, glibc-arena2 200 / 265,
  glibc-trim 6 / 4, jemalloc 339 / 928. challoc's slope is the lowest on
  both; stock glibc grows 1032 -> 1260 MiB over the x86_64 run (the
  fragmentation the human predicted), jemalloc +4.55 MiB/min there.
  Rows past 10% (attributed in the files): challoc against glibc-trim on
  soak peak (+15.5% / +11.0%), final (+24.5% / +21.6%) and tail mean
  (+16.2% / +22.4%), and against glibc-arena2 on peak (+21.1% / +15.7%);
  server peak against glibc-trim on aarch64 (+10.4%). Mechanism (from
  challoc's stats print, smaps by name and NMT): at the end of every lull
  challoc holds ~300 (x86_64) / ~400 (aarch64) MiB beyond the JVM's
  request, 301 MiB of it dirty pages inside the 10 s decay (222 MiB the
  huge allocator's empty extents: every direct buffer above 256 KiB,
  three quarters of the soak's bytes) and the rest half-occupied slabs
  (active 305 MB against allocated 171 MB), where `malloc_trim(0)` every
  5 s returns everything and two arenas keep a fixed level. CPU per
  request in the high windows +7.9% / +8.9% against glibc (inside the
  band): `perf stat` on both VM hosts, equal instructions, +2.7% / +4.0%
  cycles, 2x / 90x glibc's minor faults (the purged direct-buffer pages
  faulting back in), the allocator's own code 0.9% / 1.4% of samples.

## Design decisions

- Phase 3 task 5 (a): the server row's JVMs run with
  `-XX:NativeMemoryTracking=summary` and the harness takes `jcmd <pid>
  VM.native_memory summary scale=KB` at the peak-Rss sample (whenever a
  sample sets a new peak, at most once per 30 s, so the snapshot on record
  is the one at the highest Rss the run reached) and at the end; the
  server table carries NMT committed and "allocator overhead" = Rss minus
  NMT committed at the same instant, and a separate table has the
  categories (Java Heap, Class, Thread, Code, GC, Compiler, Internal,
  Other, the malloc total, NMT's own overhead). NMT costs a 16-byte header
  per malloc plus its tables, the same bytes for every allocator, so the
  comparisons hold; the header does turn the row's `malloc(65536)` into
  `malloc(65552)`, one class up (80 KiB) in challoc (`src/layout.rs`:
  four classes per doubling up to `MEDIUM_MAX`), jemalloc and mimalloc,
  so the NMT'd server row is compared with the NMT-less first iteration
  (469d72d) in the results files' reading rather than treated as the same
  measurement.
- Phase 3 task 5 (a): the seventh variant `glibc-trim` is glibc with the
  JVM's own `-XX:+UnlockExperimentalVMOptions
  -XX:TrimNativeHeapInterval=5000` (a `malloc_trim(0)` from the JVM's
  service thread every 5 s): Wolfi's openjdk-21 21.0.12.1 accepts the
  flag (`java -XX:+UnlockExperimentalVMOptions
  -XX:TrimNativeHeapInterval=5000 -version` exits 0, with or without the
  Unlock flag: it is a product flag in this 21u), so no `jcmd
  System.trim_native_heap` sidecar was needed; the script checks the flag
  at start and refuses the variant if a JDK rejects it, rather than
  silently measuring plain glibc.
- Phase 3 task 5 (a): the soak is a mode of `HttpBench` (`soak PORT CORE
  MAX KEEPALIVE_S MIN_BYTES MAX_BYTES`), not a second program: the same
  endpoints and reply path, a `ThreadPoolExecutor(16, 400, 2 s,
  SynchronousQueue, CallerRunsPolicy)` so a request beyond the live
  threads starts a thread and idle threads retire after 2 s, and a
  `MixedBlobHandler` drawing the body size log-uniform in [256, 1 MiB]
  per request (`exp(ln min + u (ln max - ln min))`), one
  `ByteBuffer.allocateDirect(n)` per request, copied out to the socket in
  64 KiB heap chunks so the Java heap sees no humongous arrays and the
  native side is what varies. The generator is two `ab -k` processes per
  60 s window (json and blob, half the connections each: `-c 128` x2 in
  the high windows, `-c 4` x2 in the low), 20 windows for the 1200 s;
  req/s, p99 and the server's CPU per request are per window, Rss and
  threads at each window's end, `/stats` gives the pool's live and
  largest size. The slope is the least-squares fit of Rss over the
  samples of the last 600 s in MiB per minute; the series CSVs
  (`<result>-<allocator>.csv`, `-windows.csv`) are copied beside the
  results file. `-XX:MaxDirectMemorySize` stays at its default (= -Xmx
  512m), so up to 512 MiB of direct buffers are live before
  `Bits.reserveMemory` forces a GC: the soak's Rss sits around 1.0..1.4
  GiB under every allocator (2.1 under aarch64's 64 KiB-page jemalloc)
  and the allocator's share is what differs. The soak's raw output has
  its own OUT (`target/jvm/soak-out`, 585e20b) because every run starts
  with `rm -rf OUT` and the server row's re-run erased the amd64 soak's
  stats print before it was read (the aarch64 print is complete and is
  the one quoted; the amd64 file says so).
- Phase 3 task 5 (a): the pool did not reach 256 threads (largest 72..122,
  peak thread count 68..157) because a single `ab` process at `-c 128` on
  the log-uniform bodies is CPU-bound (93..99% of a core, 4.2..5.3 GiB/s);
  the pool still cycled 16 -> ~100 -> 16 every two minutes for the whole
  run, which is the thread churn the soak wanted. Kept as is and stated
  in the files; a multi-process generator would raise the ceiling.

## Notes for the human

- Phase 3 task 5 (a): the verdict on "is it really better than glibc?",
  from the soak's numbers only (the files end with the three-sentence
  form): better than glibc as shipped on every RSS figure on both
  architectures (x86_64 peak 1237 against 1260 MiB, tail mean 1119 against
  1161, 294 against 563 MiB held beyond the JVM's request, flat against
  +2.2 MiB/min; aarch64 peak 1271 against 1433, tail mean 1175 against
  1328, 398 against 613 MiB) and far better than jemalloc; not better than
  glibc tuned the way a good Java image ships it: glibc-trim's tail mean
  is 16% / 22% below challoc's and glibc-arena2's 14% / 13%, at equal
  req/s and 6..9% less server CPU per request in the high windows. The
  gap is the ~300 / ~400 MiB challoc holds at the end of every lull in
  dirty pages inside its 10 s decay window and half-occupied slabs,
  against a `malloc_trim(0)` every 5 s that returns everything: a
  decay-policy question for lane B (`src/purge.rs`, `src/policy.rs`),
  e.g. whether a JVM-shaped process should decay faster or purge the huge
  allocator's empty extents eagerly (`huge_retain_max` is 64 MiB and 222
  MiB of empty extents were resident at the final sample).
- Phase 3 task 5 (a): what the coordinator must check by hand: the
  Makefile and bench/README.md conflicts against lane G's postgres blocks
  were resolved in the rebase onto 6208172 (mine first, then postgres;
  the `.PHONY` line is the union; `make -n jvm-soak` and `make -n
  postgres-bench` both parse); the results files' `commit:` headers were
  rewritten from the pre-rebase hashes to the rebased ones (4638d26 ->
  359ae7f, b469c1d -> 1f49f91, 585ef86 -> 585e20b; `src/` is identical
  between f862134 and 6208172); the JVM rows are still not scorecard
  columns (plan acceptance text).

- Phase 3 task 5 (a): the arm64 lane VM's Docker bridge truncated the
  wolfi APKINDEX (apk saw 16 packages instead of 108001, "temporary
  error (try again later)" then "zlib-dev (no such package)"), so the
  image build failed four times; the same fetch with `--network=host`
  worked, the image was built once by hand with `docker build
  --network=host` and the Makefile's exact command, after which `make
  image` hit the cache. The amd64 VM built with the bridge at the same
  MTU 1460 without trouble. Not a harness change; noted for the driver's
  provisioning if it recurs.
