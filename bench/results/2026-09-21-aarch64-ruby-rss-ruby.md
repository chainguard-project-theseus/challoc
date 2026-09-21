# Ruby 3.4 benchmark rows, 2026-09-21, aarch64

- commit: 6b29fa9
- date: 2026-09-21T05:59:04Z
- host: Neoverse-V2, 16 CPUs visible to the container; lane ruby-rss VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 5.23 5.71 4.30; at end: 3.12 5.09 4.16; 42 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 6287c62a3cb3cc32)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.041 0.041 0.041 0.041 0.041 | 13604 |
| startup | jemalloc | 0.039 0.039 0.039 0.039 0.039 | 14948 |
| gem_list | challoc | 0.117 0.117 0.119 0.119 0.119 | 27080 |
| gem_list | jemalloc | 0.114 0.114 0.115 0.116 0.117 | 28108 |
| erb_render | challoc | 0.844 0.845 0.845 0.848 0.865 | 11724 |
| erb_render | jemalloc | 0.965 0.967 0.969 0.973 0.987 | 13248 |
| gc_churn | challoc | 1.610 1.626 1.630 1.634 1.634 | 391560 |
| gc_churn | jemalloc | 1.633 1.644 1.648 1.651 1.654 | 370332 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.041 | 13.3 | 0.039 | 14.6 | +5.1% | -9.0% |
| gem_list | 0.119 | 26.4 | 0.115 | 27.4 | +3.5% | -3.7% |
| erb_render | 0.845 | 11.4 | 0.969 | 12.9 | -12.8% | -11.5% |
| gc_churn | 1.630 | 382.4 | 1.648 | 361.7 | -1.1% | +5.7% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571262 | 225 | 382.4 | 374.3 | -2.1% | 382.4 |
| jemalloc | 54 | 25 | 29 | 2571262 | 225 | 360.1 | 201.6 | -44.0% | 361.7 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
