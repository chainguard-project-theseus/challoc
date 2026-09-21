# Ruby 3.4 benchmark rows, 2026-09-21, aarch64

- commit: aeafc1e
- date: 2026-09-21T08:29:45Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 5.10 5.51 4.24; at end: 2.93 4.88 4.08; 42 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 c6c05a531b866b13)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.041 0.041 0.041 0.041 0.042 | 13604 |
| startup | jemalloc | 0.039 0.039 0.040 0.040 0.040 | 14948 |
| gem_list | challoc | 0.119 0.119 0.119 0.120 0.121 | 27076 |
| gem_list | jemalloc | 0.114 0.115 0.115 0.115 0.116 | 28104 |
| erb_render | challoc | 0.845 0.846 0.847 0.849 0.850 | 11728 |
| erb_render | jemalloc | 0.970 0.970 0.970 0.974 0.977 | 13032 |
| gc_churn | challoc | 1.580 1.603 1.625 1.644 1.645 | 391560 |
| gc_churn | jemalloc | 1.632 1.635 1.636 1.639 1.643 | 370916 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.041 | 13.3 | 0.040 | 14.6 | +2.5% | -9.0% |
| gem_list | 0.119 | 26.4 | 0.115 | 27.4 | +3.5% | -3.7% |
| erb_render | 0.847 | 11.5 | 0.970 | 12.7 | -12.7% | -10.0% |
| gc_churn | 1.625 | 382.4 | 1.636 | 362.2 | -0.7% | +5.6% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571262 | 225 | 382.4 | 374.3 | -2.1% | 382.4 |
| jemalloc | 54 | 25 | 29 | 2571262 | 225 | 362.1 | 198.8 | -45.1% | 362.2 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
