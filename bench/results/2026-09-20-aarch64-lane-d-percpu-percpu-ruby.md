# Ruby 3.4 benchmark rows, 2026-09-20, aarch64

- commit: 0c60b1a
- date: 2026-09-20T06:12:01Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE aarch64 lane VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 0.68 1.58 1.85; at end: 1.00 1.27 1.66; 216 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 27db1d715962c256)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.039 0.039 0.040 0.040 0.040 | 13644 |
| startup | jemalloc | 0.037 0.037 0.037 0.037 0.038 | 14948 |
| so_binary_trees | challoc | 4.672 4.674 4.680 4.687 4.772 | 27588 |
| so_binary_trees | jemalloc | 4.638 4.657 4.673 4.689 4.695 | 28892 |
| hash_aref_str | challoc | 0.348 0.349 0.352 0.353 0.360 | 10180 |
| hash_aref_str | jemalloc | 0.341 0.342 0.349 0.355 0.359 | 11424 |
| array_sample_100k__1k | challoc | 0.425 0.427 0.427 0.429 0.431 | 33352 |
| array_sample_100k__1k | jemalloc | 0.421 0.424 0.428 0.428 0.428 | 34776 |
| array_sort_block | challoc | 5.309 5.314 5.316 5.322 5.347 | 30748 |
| array_sort_block | jemalloc | 5.292 5.313 5.316 5.317 5.319 | 32216 |
| so_nbody | challoc | 0.823 0.823 0.824 0.824 0.828 | 10440 |
| so_nbody | jemalloc | 0.821 0.821 0.822 0.822 0.823 | 11556 |
| app_fib | challoc | 0.291 0.291 0.292 0.292 0.292 | 10164 |
| app_fib | jemalloc | 0.291 0.291 0.291 0.292 0.292 | 11424 |
| erb_render | challoc | 0.888 0.889 0.889 0.892 0.896 | 11704 |
| erb_render | jemalloc | 0.961 0.961 0.965 0.965 0.967 | 13136 |
| gem_list | challoc | 0.112 0.113 0.114 0.114 0.115 | 27056 |
| gem_list | jemalloc | 0.108 0.108 0.108 0.109 0.109 | 28104 |
| gc_churn | challoc | 1.600 1.607 1.611 1.624 1.627 | 391532 |
| gc_churn | jemalloc | 1.611 1.616 1.618 1.620 1.633 | 374452 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.040 | 13.3 | 0.037 | 14.6 | +8.1% | -8.7% |
| so_binary_trees | 4.680 | 26.9 | 4.673 | 28.2 | +0.1% | -4.5% |
| hash_aref_str | 0.352 | 9.9 | 0.349 | 11.2 | +0.9% | -10.9% |
| array_sample_100k__1k | 0.427 | 32.6 | 0.428 | 34.0 | -0.2% | -4.1% |
| array_sort_block | 5.316 | 30.0 | 5.316 | 31.5 | +0.0% | -4.6% |
| so_nbody | 0.824 | 10.2 | 0.822 | 11.3 | +0.2% | -9.7% |
| app_fib | 0.292 | 9.9 | 0.291 | 11.2 | +0.3% | -11.0% |
| erb_render | 0.889 | 11.4 | 0.965 | 12.8 | -7.9% | -10.9% |
| gem_list | 0.114 | 26.4 | 0.108 | 27.4 | +5.6% | -3.7% |
| gc_churn | 1.611 | 382.4 | 1.618 | 365.7 | -0.4% | +4.6% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571262 | 225 | 382.4 | 374.3 | -2.1% | 382.4 |
| jemalloc | 54 | 25 | 29 | 2571262 | 225 | 359.2 | 199.3 | -44.5% | 365.7 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
