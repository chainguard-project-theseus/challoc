# Ruby 3.4 benchmark rows, 2026-09-19, x86_64

- commit: 4c7ced6
- date: 2026-09-19T02:54:51Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; gce-c4
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 0.39 0.36 1.29; at end: 1.00 0.78 1.19; 310 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 0bbd494d4a88952d)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped
  - glibc: the --without-jemalloc ruby, glibc malloc
  - mimalloc: the --without-jemalloc ruby with LD_PRELOAD=/src/target/bench-bin/libmimalloc.so (v3.5.2)

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.041 0.041 0.042 0.042 0.042 | 14216 |
| startup | jemalloc | 0.040 0.040 0.040 0.041 0.041 | 15324 |
| startup | glibc | 0.040 0.041 0.042 0.042 0.042 | 12600 |
| startup | mimalloc | 0.039 0.039 0.039 0.039 0.040 | 13860 |
| so_binary_trees | challoc | 3.853 3.858 3.859 3.865 3.924 | 28248 |
| so_binary_trees | jemalloc | 3.826 3.836 3.837 3.844 3.913 | 29460 |
| so_binary_trees | glibc | 3.744 3.804 3.831 3.864 3.903 | 26516 |
| so_binary_trees | mimalloc | 3.736 3.815 3.835 3.854 3.940 | 27396 |
| hash_aref_str | challoc | 0.281 0.282 0.289 0.290 0.292 | 10968 |
| hash_aref_str | jemalloc | 0.276 0.279 0.281 0.289 0.299 | 12228 |
| hash_aref_str | glibc | 0.273 0.290 0.292 0.297 0.316 | 9916 |
| hash_aref_str | mimalloc | 0.278 0.284 0.289 0.290 0.298 | 10392 |
| array_sample_100k__1k | challoc | 0.438 0.441 0.441 0.448 0.449 | 34136 |
| array_sample_100k__1k | jemalloc | 0.439 0.444 0.445 0.449 0.460 | 34920 |
| array_sample_100k__1k | glibc | 0.441 0.449 0.450 0.462 0.476 | 30212 |
| array_sample_100k__1k | mimalloc | 0.435 0.449 0.449 0.451 0.455 | 49068 |
| array_sort_block | challoc | 4.095 4.099 4.104 4.124 4.129 | 31240 |
| array_sort_block | jemalloc | 4.115 4.124 4.136 4.152 4.197 | 32604 |
| array_sort_block | glibc | 3.969 4.085 4.095 4.102 4.112 | 29536 |
| array_sort_block | mimalloc | 3.991 4.085 4.097 4.102 4.103 | 49200 |
| so_nbody | challoc | 0.680 0.686 0.688 0.692 0.713 | 11228 |
| so_nbody | jemalloc | 0.678 0.682 0.683 0.686 0.695 | 12396 |
| so_nbody | glibc | 0.650 0.663 0.665 0.668 0.674 | 9984 |
| so_nbody | mimalloc | 0.636 0.657 0.657 0.675 0.680 | 10580 |
| app_fib | challoc | 0.255 0.255 0.266 0.266 0.268 | 10968 |
| app_fib | jemalloc | 0.249 0.253 0.255 0.259 0.264 | 12160 |
| app_fib | glibc | 0.245 0.251 0.253 0.253 0.254 | 9832 |
| app_fib | mimalloc | 0.250 0.253 0.253 0.260 0.279 | 10552 |
| erb_render | challoc | 0.623 0.629 0.631 0.634 0.636 | 12144 |
| erb_render | jemalloc | 0.769 0.773 0.778 0.783 0.798 | 13212 |
| erb_render | glibc | 0.655 0.670 0.675 0.678 0.681 | 10416 |
| erb_render | mimalloc | 0.630 0.637 0.638 0.644 0.664 | 11676 |
| gem_list | challoc | 0.110 0.111 0.111 0.112 0.113 | 27808 |
| gem_list | jemalloc | 0.108 0.110 0.111 0.112 0.115 | 28516 |
| gem_list | glibc | 0.109 0.111 0.113 0.114 0.117 | 25968 |
| gem_list | mimalloc | 0.106 0.107 0.108 0.109 0.112 | 27236 |
| gc_churn | challoc | 1.799 1.810 1.811 1.823 1.824 | 392184 |
| gc_churn | jemalloc | 1.785 1.799 1.807 1.833 1.835 | 364720 |
| gc_churn | glibc | 1.725 1.743 1.768 1.771 1.793 | 324384 |
| gc_churn | mimalloc | 1.878 1.884 1.886 1.890 1.899 | 363796 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | glibc s | glibc RSS MiB | mimalloc s | mimalloc RSS MiB | challoc vs jemalloc time | RSS | challoc vs glibc time | RSS | challoc vs mimalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| startup | 0.042 | 13.9 | 0.040 | 15.0 | 0.042 | 12.3 | 0.039 | 13.5 | +5.0% | -7.2% | +0.0% | +12.8% | +7.7% | +2.6% |
| so_binary_trees | 3.859 | 27.6 | 3.837 | 28.8 | 3.831 | 25.9 | 3.835 | 26.8 | +0.6% | -4.1% | +0.7% | +6.5% | +0.6% | +3.1% |
| hash_aref_str | 0.289 | 10.7 | 0.281 | 11.9 | 0.292 | 9.7 | 0.289 | 10.1 | +2.8% | -10.3% | -1.0% | +10.6% | +0.0% | +5.5% |
| array_sample_100k__1k | 0.441 | 33.3 | 0.445 | 34.1 | 0.450 | 29.5 | 0.449 | 47.9 | -0.9% | -2.2% | -2.0% | +13.0% | -1.8% | -30.4% |
| array_sort_block | 4.104 | 30.5 | 4.136 | 31.8 | 4.095 | 28.8 | 4.097 | 48.0 | -0.8% | -4.2% | +0.2% | +5.8% | +0.2% | -36.5% |
| so_nbody | 0.688 | 11.0 | 0.683 | 12.1 | 0.665 | 9.8 | 0.657 | 10.3 | +0.7% | -9.4% | +3.5% | +12.5% | +4.7% | +6.1% |
| app_fib | 0.266 | 10.7 | 0.255 | 11.9 | 0.253 | 9.6 | 0.253 | 10.3 | +4.3% | -9.8% | +5.1% | +11.6% | +5.1% | +3.9% |
| erb_render | 0.631 | 11.9 | 0.778 | 12.9 | 0.675 | 10.2 | 0.638 | 11.4 | -18.9% | -8.1% | -6.5% | +16.6% | -1.1% | +4.0% |
| gem_list | 0.111 | 27.2 | 0.111 | 27.8 | 0.113 | 25.4 | 0.108 | 26.6 | +0.0% | -2.5% | -1.8% | +7.1% | +2.8% | +2.1% |
| gc_churn | 1.811 | 383.0 | 1.807 | 356.2 | 1.768 | 316.8 | 1.886 | 355.3 | +0.2% | +7.5% | +2.4% | +20.9% | -4.0% | +7.8% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571264 | 225 | 383.0 | 375.0 | -2.1% | 383.0 |
| jemalloc | 52 | 24 | 28 | 2571264 | 225 | 352.6 | 344.1 | -2.4% | 356.2 |
| glibc | 53 | 23 | 30 | 2571264 | 225 | 293.9 | 286.8 | -2.4% | 316.8 |
| mimalloc | 54 | 25 | 29 | 2571264 | 225 | 355.3 | 347.2 | -2.3% | 355.3 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc rows).
