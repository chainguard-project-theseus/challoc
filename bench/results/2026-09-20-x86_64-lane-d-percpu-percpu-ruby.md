# Ruby 3.4 benchmark rows, 2026-09-20, x86_64

- commit: 0c60b1a
- date: 2026-09-20T06:29:47Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; GCE x86_64 lane VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 0.56 1.34 1.53; at end: 1.00 1.17 1.42; 208 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 2798d6cdfc6b9264)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.042 0.042 0.042 0.042 0.043 | 14488 |
| startup | jemalloc | 0.041 0.041 0.041 0.041 0.043 | 15596 |
| so_binary_trees | challoc | 3.982 3.990 4.303 4.314 4.388 | 28528 |
| so_binary_trees | jemalloc | 3.949 4.075 4.289 4.291 4.308 | 29716 |
| hash_aref_str | challoc | 0.292 0.297 0.309 0.312 0.316 | 11308 |
| hash_aref_str | jemalloc | 0.293 0.296 0.305 0.313 0.322 | 12600 |
| array_sample_100k__1k | challoc | 0.458 0.463 0.493 0.494 0.495 | 34208 |
| array_sample_100k__1k | jemalloc | 0.458 0.478 0.494 0.496 0.496 | 35040 |
| array_sort_block | challoc | 4.211 4.222 4.582 4.591 4.615 | 31576 |
| array_sort_block | jemalloc | 4.221 4.559 4.570 4.580 4.621 | 32696 |
| so_nbody | challoc | 0.719 0.759 0.765 0.772 0.785 | 11500 |
| so_nbody | jemalloc | 0.692 0.758 0.759 0.760 0.767 | 12732 |
| app_fib | challoc | 0.264 0.265 0.282 0.282 0.284 | 11044 |
| app_fib | jemalloc | 0.267 0.281 0.283 0.283 0.288 | 12420 |
| erb_render | challoc | 0.699 0.700 0.761 0.762 0.770 | 12392 |
| erb_render | jemalloc | 0.803 0.858 0.866 0.866 1.008 | 13528 |
| gem_list | challoc | 0.115 0.117 0.118 0.118 0.123 | 28196 |
| gem_list | jemalloc | 0.113 0.115 0.116 0.116 0.117 | 28912 |
| gc_churn | challoc | 1.944 1.947 1.971 2.017 2.036 | 392436 |
| gc_churn | jemalloc | 1.928 1.985 2.024 2.052 2.061 | 393748 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.042 | 14.1 | 0.041 | 15.2 | +2.4% | -7.1% |
| so_binary_trees | 4.303 | 27.9 | 4.289 | 29.0 | +0.3% | -4.0% |
| hash_aref_str | 0.309 | 11.0 | 0.305 | 12.3 | +1.3% | -10.3% |
| array_sample_100k__1k | 0.493 | 33.4 | 0.494 | 34.2 | -0.2% | -2.4% |
| array_sort_block | 4.582 | 30.8 | 4.570 | 31.9 | +0.3% | -3.4% |
| so_nbody | 0.765 | 11.2 | 0.759 | 12.4 | +0.8% | -9.7% |
| app_fib | 0.282 | 10.8 | 0.283 | 12.1 | -0.4% | -11.1% |
| erb_render | 0.761 | 12.1 | 0.866 | 13.2 | -12.1% | -8.4% |
| gem_list | 0.118 | 27.5 | 0.116 | 28.2 | +1.7% | -2.5% |
| gc_churn | 1.971 | 383.2 | 2.024 | 384.5 | -2.6% | -0.3% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571264 | 225 | 383.3 | 375.2 | -2.1% | 383.2 |
| jemalloc | 54 | 25 | 29 | 2571264 | 225 | 384.5 | 376.3 | -2.1% | 384.5 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
