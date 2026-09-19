# Ruby 3.4 benchmark rows, 2026-09-19, aarch64

- commit: 675f357
- date: 2026-09-19T21:51:17Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE aarch64 lane VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 0.75 1.66 1.92; at end: 1.00 1.32 1.72; 215 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 4e95eb3737dc3098)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.038 0.039 0.039 0.039 0.040 | 13600 |
| startup | jemalloc | 0.036 0.037 0.037 0.038 0.038 | 14944 |
| so_binary_trees | challoc | 4.664 4.673 4.685 4.696 4.698 | 27540 |
| so_binary_trees | jemalloc | 4.657 4.657 4.659 4.698 4.726 | 35420 |
| hash_aref_str | challoc | 0.338 0.342 0.346 0.349 0.354 | 10140 |
| hash_aref_str | jemalloc | 0.342 0.343 0.348 0.348 0.349 | 11424 |
| array_sample_100k__1k | challoc | 0.426 0.427 0.427 0.429 0.433 | 33308 |
| array_sample_100k__1k | jemalloc | 0.425 0.428 0.428 0.428 0.431 | 34780 |
| array_sort_block | challoc | 5.304 5.308 5.308 5.313 5.356 | 30700 |
| array_sort_block | jemalloc | 5.317 5.320 5.320 5.321 5.324 | 32216 |
| so_nbody | challoc | 0.824 0.824 0.825 0.826 0.837 | 10380 |
| so_nbody | jemalloc | 0.821 0.821 0.824 0.825 0.827 | 11556 |
| app_fib | challoc | 0.291 0.291 0.291 0.292 0.293 | 10136 |
| app_fib | jemalloc | 0.289 0.290 0.291 0.293 0.294 | 11424 |
| erb_render | challoc | 0.828 0.832 0.833 0.834 0.835 | 11720 |
| erb_render | jemalloc | 0.965 0.965 0.967 0.972 0.976 | 13136 |
| gem_list | challoc | 0.108 0.108 0.111 0.115 0.117 | 27012 |
| gem_list | jemalloc | 0.105 0.107 0.107 0.107 0.112 | 28100 |
| gc_churn | challoc | 1.592 1.593 1.595 1.608 1.621 | 392248 |
| gc_churn | jemalloc | 1.612 1.618 1.640 1.643 1.675 | 369564 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.039 | 13.3 | 0.037 | 14.6 | +5.4% | -9.0% |
| so_binary_trees | 4.685 | 26.9 | 4.659 | 34.6 | +0.6% | -22.2% |
| hash_aref_str | 0.346 | 9.9 | 0.348 | 11.2 | -0.6% | -11.2% |
| array_sample_100k__1k | 0.427 | 32.5 | 0.428 | 34.0 | -0.2% | -4.2% |
| array_sort_block | 5.308 | 30.0 | 5.320 | 31.5 | -0.2% | -4.7% |
| so_nbody | 0.825 | 10.1 | 0.824 | 11.3 | +0.1% | -10.2% |
| app_fib | 0.291 | 9.9 | 0.291 | 11.2 | +0.0% | -11.3% |
| erb_render | 0.833 | 11.4 | 0.967 | 12.8 | -13.9% | -10.8% |
| gem_list | 0.111 | 26.4 | 0.107 | 27.4 | +3.7% | -3.9% |
| gc_churn | 1.595 | 383.1 | 1.640 | 360.9 | -2.7% | +6.1% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571262 | 225 | 383.2 | 375.1 | -2.1% | 383.1 |
| jemalloc | 54 | 25 | 29 | 2571262 | 225 | 360.9 | 194.8 | -46.0% | 360.9 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
