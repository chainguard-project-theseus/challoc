# Ruby 3.4 benchmark rows, 2026-09-19, aarch64

- commit: 4864d4b
- date: 2026-09-19T23:16:20Z
- host: Neoverse-V2 (Axion), 8 CPUs visible to the container; Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4a-standard-8 (Neoverse-V2 (Axion)), zone us-east4-a, 8 vCPU, podman --network=host --privileged
- kernel: 6.18.49-r2-gcp-6.18, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [aarch64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 2.29 2.68 2.55; at end: 1.01 1.54 2.06; 339 s total
- method: 3 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 96e2c8dde777c044)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - glibc: the --without-jemalloc ruby, glibc malloc
  - mimalloc: the --without-jemalloc ruby with LD_PRELOAD=/src/target/bench-bin/libmimalloc.so (v3.5.2)
  - tcmalloc: the --without-jemalloc ruby with LD_PRELOAD=/usr/lib/libtcmalloc_minimal.so.4 (Wolfi tcmalloc-minimal 2.18.1)

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.041 0.042 0.042 | 13524 |
| startup | jemalloc | 0.039 0.039 0.039 | 17908 |
| startup | glibc | 0.040 0.040 0.045 | 11868 |
| startup | mimalloc | 0.039 0.039 0.039 | 13128 |
| startup | tcmalloc | 0.040 0.040 0.041 | 20432 |
| so_binary_trees | challoc | 4.668 4.680 4.698 | 27540 |
| so_binary_trees | jemalloc | 4.645 4.664 4.714 | 28888 |
| so_binary_trees | glibc | 4.674 4.678 4.679 | 25880 |
| so_binary_trees | mimalloc | 4.661 4.676 4.682 | 26756 |
| so_binary_trees | tcmalloc | 4.674 4.679 4.680 | 31800 |
| hash_aref_str | challoc | 0.336 0.355 0.362 | 10072 |
| hash_aref_str | jemalloc | 0.342 0.349 0.362 | 11404 |
| hash_aref_str | glibc | 0.345 0.361 0.363 | 9112 |
| hash_aref_str | mimalloc | 0.351 0.353 0.359 | 9732 |
| hash_aref_str | tcmalloc | 0.342 0.346 0.350 | 14648 |
| array_sample_100k__1k | challoc | 0.426 0.428 0.429 | 33428 |
| array_sample_100k__1k | jemalloc | 0.428 0.429 0.437 | 34768 |
| array_sample_100k__1k | glibc | 0.438 0.440 0.445 | 29400 |
| array_sample_100k__1k | mimalloc | 0.429 0.434 0.435 | 50296 |
| array_sample_100k__1k | tcmalloc | 0.427 0.427 0.433 | 36024 |
| array_sort_block | challoc | 5.307 5.314 5.347 | 30572 |
| array_sort_block | jemalloc | 5.293 5.311 5.332 | 32128 |
| array_sort_block | glibc | 5.228 5.251 5.265 | 28536 |
| array_sort_block | mimalloc | 5.296 5.298 5.318 | 48248 |
| array_sort_block | tcmalloc | 5.290 5.299 5.304 | 35000 |
| so_nbody | challoc | 0.824 0.824 0.825 | 10380 |
| so_nbody | jemalloc | 0.820 0.820 0.821 | 11540 |
| so_nbody | glibc | 0.832 0.836 0.858 | 9228 |
| so_nbody | mimalloc | 0.820 0.820 0.821 | 9864 |
| so_nbody | tcmalloc | 0.821 0.821 0.824 | 14908 |
| app_fib | challoc | 0.291 0.292 0.292 | 10072 |
| app_fib | jemalloc | 0.293 0.299 0.358 | 11420 |
| app_fib | glibc | 0.288 0.289 0.290 | 9112 |
| app_fib | mimalloc | 0.289 0.289 0.290 | 9732 |
| app_fib | tcmalloc | 0.292 0.292 0.293 | 14632 |
| erb_render | challoc | 0.839 0.841 0.845 | 11656 |
| erb_render | jemalloc | 0.971 0.974 0.976 | 13196 |
| erb_render | glibc | 0.866 0.867 0.878 | 10044 |
| erb_render | mimalloc | 0.845 0.851 0.868 | 11180 |
| erb_render | tcmalloc | 0.759 0.761 0.762 | 16104 |
| gem_list | challoc | 0.119 0.120 0.120 | 27008 |
| gem_list | jemalloc | 0.114 0.114 0.115 | 30808 |
| gem_list | glibc | 0.120 0.120 0.122 | 25472 |
| gem_list | mimalloc | 0.115 0.116 0.116 | 26472 |
| gem_list | tcmalloc | 0.114 0.115 0.117 | 32276 |
| gc_churn | challoc | 1.633 1.661 1.684 | 391480 |
| gc_churn | jemalloc | 1.662 1.666 1.695 | 373224 |
| gc_churn | glibc | 1.573 1.579 1.582 | 323612 |
| gc_churn | mimalloc | 1.648 1.656 1.660 | 362928 |
| gc_churn | tcmalloc | 1.598 1.611 1.614 | 344848 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | glibc s | glibc RSS MiB | mimalloc s | mimalloc RSS MiB | tcmalloc s | tcmalloc RSS MiB | challoc vs jemalloc time | RSS | challoc vs glibc time | RSS | challoc vs mimalloc time | RSS | challoc vs tcmalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| startup | 0.042 | 13.2 | 0.039 | 17.5 | 0.040 | 11.6 | 0.039 | 12.8 | 0.040 | 20.0 | +7.7% | -24.5% | +5.0% | +14.0% | +7.7% | +3.0% | +5.0% | -33.8% |
| so_binary_trees | 4.680 | 26.9 | 4.664 | 28.2 | 4.678 | 25.3 | 4.676 | 26.1 | 4.679 | 31.1 | +0.3% | -4.7% | +0.0% | +6.4% | +0.1% | +2.9% | +0.0% | -13.4% |
| hash_aref_str | 0.355 | 9.8 | 0.349 | 11.1 | 0.361 | 8.9 | 0.353 | 9.5 | 0.346 | 14.3 | +1.7% | -11.7% | -1.7% | +10.5% | +0.6% | +3.5% | +2.6% | -31.2% |
| array_sample_100k__1k | 0.428 | 32.6 | 0.429 | 34.0 | 0.440 | 28.7 | 0.434 | 49.1 | 0.427 | 35.2 | -0.2% | -3.9% | -2.7% | +13.7% | -1.4% | -33.5% | +0.2% | -7.2% |
| array_sort_block | 5.314 | 29.9 | 5.311 | 31.4 | 5.251 | 27.9 | 5.298 | 47.1 | 5.299 | 34.2 | +0.1% | -4.8% | +1.2% | +7.1% | +0.3% | -36.6% | +0.3% | -12.7% |
| so_nbody | 0.824 | 10.1 | 0.820 | 11.3 | 0.836 | 9.0 | 0.820 | 9.6 | 0.821 | 14.6 | +0.5% | -10.1% | -1.4% | +12.5% | +0.5% | +5.2% | +0.4% | -30.4% |
| app_fib | 0.292 | 9.8 | 0.299 | 11.2 | 0.289 | 8.9 | 0.289 | 9.5 | 0.292 | 14.3 | -2.3% | -11.8% | +1.0% | +10.5% | +1.0% | +3.5% | +0.0% | -31.2% |
| erb_render | 0.841 | 11.4 | 0.974 | 12.9 | 0.867 | 9.8 | 0.851 | 10.9 | 0.761 | 15.7 | -13.7% | -11.7% | -3.0% | +16.0% | -1.2% | +4.3% | +10.5% | -27.6% |
| gem_list | 0.120 | 26.4 | 0.114 | 30.1 | 0.120 | 24.9 | 0.116 | 25.9 | 0.115 | 31.5 | +5.3% | -12.3% | +0.0% | +6.0% | +3.4% | +2.0% | +4.3% | -16.3% |
| gc_churn | 1.661 | 382.3 | 1.666 | 364.5 | 1.579 | 316.0 | 1.656 | 354.4 | 1.611 | 336.8 | -0.3% | +4.9% | +5.2% | +21.0% | +0.3% | +7.9% | +3.1% | +13.5% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571262 | 225 | 382.3 | 374.3 | -2.1% | 382.3 |
| jemalloc | 54 | 25 | 29 | 2571262 | 225 | 355.4 | 202.6 | -43.0% | 364.5 |
| glibc | 53 | 23 | 30 | 2571262 | 225 | 294.0 | 286.9 | -2.4% | 316.0 |
| mimalloc | 54 | 25 | 29 | 2571262 | 225 | 354.6 | 346.5 | -2.3% | 354.4 |
| tcmalloc | 54 | 24 | 30 | 2571262 | 219 | 335.6 | 328.0 | -2.3% | 336.8 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
