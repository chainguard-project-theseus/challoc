## Done

- Phase 3 task 5 (c): done (213fc12 harness, a3121bb results). Node.js rows under
  challoc, glibc, jemalloc, mimalloc and tcmalloc on Wolfi's nodejs-22
  (v22.23.2, V8 12.4), both architectures, on the lane's VMs
  challoc-lane-n-{arm64,amd64}, results in
  bench/results/2026-09-20-{aarch64,x86_64}-node.md. Harness:
  bench/node-bench.sh, `make node-bench` (NODE_BENCH_VARS), bench/node/server.js
  and bench/node/json_churn.js, the `# Node.js rows (Phase 3 task 5c)` block in
  Dockerfile.dev (nodejs-22, apache2-utils), a node-bench paragraph in
  bench/README.md. Headline medians (challoc vs glibc / vs jemalloc, positive =
  challoc worse): aarch64 startup 0.021 s +10.5% / +5.0% (RSS -11.9% / +0.3%),
  json_churn 2.622 s +0.5% / -0.2% (RSS -10.3% / -0.3%), server 2568 req/s per
  endpoint -2.0% fewer than glibc / +1.6% more than jemalloc, p99 13 ms (glibc
  13, jemalloc 15), RSS peak 130.6 MiB (-3.2% / -4.5%), final 108.9 MiB (-17.2% /
  -9.6%); x86_64 startup 0.024 s +4.3% / +0.0% (RSS +3.7% / +0.4%), json_churn
  2.334 s -7.1% / -1.1% (RSS +2.8% / -0.1%), server 3694 req/s +5.5% more than
  glibc / -1.5% fewer than jemalloc, p99 9 ms (glibc 13, jemalloc 10..11), RSS
  peak 138.9 MiB (+0.7% / +3.5%), final 138.8 MiB (+9.8% / +7.8%). The one row
  past the 10% line, aarch64 startup, is attributed in the results file: system
  time only (sys 7.8 vs 6.2 ms, user equal), about 270 more mprotect calls and
  880 more minor faults from committing each size class's first slabs over the
  PROT_NONE class reservations while V8 deserialises its snapshot; challoc's own
  init costs nothing measurable (/bin/true 1 ms under both). Server row ran the
  full 5 minutes per allocator on both VMs.

## Design decisions

- Phase 3 task 5 (c): the load generator is `ab` from Wolfi's apache2-utils
  because Wolfi has neither `wrk` nor `oha` (apk search, 2026-09-20). ab is
  single-threaded and one URL per process, so the server row runs two ab
  processes at once (32 keep-alive connections each, /json and /blob) against
  one node process; `-t DURATION` must precede `-n` because ab's -t resets
  -n to 50000, and `-l` accepts the variable-length JSON body. Node's single
  event loop shared by both endpoints makes the two req/s figures equal (the 1
  MiB /blob responses set the pace); the row is still a same-generator
  comparison across allocators.
- Phase 3 task 5 (c): the server keeps an explicit Content-Length on the 1 MiB
  body (written as 64 KiB subarray views of one Buffer.allocUnsafe(1 << 20)),
  so ab sees a fixed-length keep-alive response rather than chunked encoding;
  the backing store is still one malloc(1 MiB) per request, freed when the GC
  collects the Buffer, which is what the row is for.
- Phase 3 task 5 (c): server RSS is the process's smaps_rollup Rss sampled every
  5 s (peak and final) plus VmHWM at the end, and the end-of-load smaps summed
  by mapping name (challoc:classes|huge|meta) so an RSS gap is placed without a
  second run; the process rows use bench/rt's maxrss as the Ruby and Python
  rows do.

## Notes for the human

- Phase 3 task 5 (c): on x86_64 the server's final RSS is +9.8% against glibc
  and +7.8% against jemalloc (under the threshold, not attributed as a gap):
  62 MiB of it is challoc:huge, the freed 1 MiB Buffer backing stores retained
  for reuse up to huge_retain_max (64 MiB default). If Node-shaped consumers
  matter for RSS, a lower default or decay of retained huge blocks is lane B's
  call (src/purge.rs, src/policy.rs); this lane changed nothing under src/.
- Phase 3 task 5 (c): the Dockerfile.dev block adds nodejs-22 and apache2-utils
  right after the python packages, marked `# Node.js rows (Phase 3 task 5c)`
  in the comment below the apk list, and the RUN's final check line gains
  `node --version && ab -V | head -1`; the JVM and PostgreSQL lanes' blocks
  will need unioning there and in the Makefile's .PHONY line.
- Phase 3 task 5 (c): json_churn's document is 180906 bytes (177 KiB), not the
  plan's "about 100 KiB": the generator's 400 records came out at ~450 bytes
  each and the size was only read from the results header after the runs. The
  rows were measured at that size (recorded in both files' header line) and
  the harness comments, README and Makefile help say so (63ebd0c). Shrinking
  it to 100 KiB is a one-line change to bench/node/json_churn.js (200 records)
  if the coordinator wants the plan's figure, followed by a re-run of that row.
