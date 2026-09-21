## Done

- Ruby threaded RSS (lane ruby-rss, 2026-09-21): in progress. Goal: a
  default under which the 16-thread Ruby soak's peak and t=600 RSS are
  within ~10% of jemalloc's on both architectures, no suite regression.
  Attribution first (`bench/results/2026-09-21-<arch>-ruby-rss-attribution-*.md`),
  then the smallest default change. Harness: `bench/ruby-soak.sh` takes
  `VARIANTS="name=malloc_conf ..."` (any challoc configuration as a named
  variant with its own "vs challoc" and "vs jemalloc" columns) and
  `TRIM=1` (`malloc_trim(0)` through fiddle before every sample); the
  CSV and the "Work done" table carry the process CPU time (`cpu_s`,
  CPU ms per job) so a fix is judged on cost too.

## Design decisions

## Notes for the human
