# Ruby 3.4 benchmark rows, 2026-09-19, x86_64

- commit: 675f357
- date: 2026-09-19T22:08:52Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs visible to the container; GCE x86_64 lane VM
- kernel: 7.0.0-1011-gcp, container: Wolfi
- ruby: ruby 3.4.10 (2026-06-30 revision 2b0b7728dc) +PRISM [x86_64-linux] (tag v3_4_10 built by contrib/ruby/build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}
- benchmark-driver 0.17.0 (67a7729), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows
- load average at start: 0.67 1.27 1.51; at end: 0.99 1.14 1.41; 184 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb 10 10. Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to /src/target/ruby/jemalloc-compat/stage/lib/libjemalloc.so.2 (challoc, quantum16, sha256 d74436810319c034)
  - jemalloc: the same --with-jemalloc ruby with LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1); the stage is not mapped

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| startup | challoc | 0.041 0.041 0.041 0.042 0.042 | 14400 |
| startup | jemalloc | 0.040 0.040 0.040 0.041 0.041 | 15588 |
| so_binary_trees | challoc | 3.698 3.717 3.730 3.736 3.745 | 28464 |
| so_binary_trees | jemalloc | 3.692 3.697 3.708 3.725 3.729 | 29772 |
| hash_aref_str | challoc | 0.266 0.271 0.280 0.280 0.286 | 11172 |
| hash_aref_str | jemalloc | 0.265 0.271 0.273 0.275 0.278 | 12304 |
| array_sample_100k__1k | challoc | 0.422 0.425 0.426 0.427 0.438 | 34356 |
| array_sample_100k__1k | jemalloc | 0.426 0.429 0.433 0.436 0.438 | 35404 |
| array_sort_block | challoc | 3.949 3.963 3.971 3.982 3.984 | 31508 |
| array_sort_block | jemalloc | 3.988 3.999 4.008 4.067 4.146 | 32716 |
| so_nbody | challoc | 0.643 0.658 0.661 0.664 0.668 | 11472 |
| so_nbody | jemalloc | 0.658 0.660 0.667 0.671 0.678 | 12788 |
| app_fib | challoc | 0.243 0.244 0.246 0.250 0.254 | 11004 |
| app_fib | jemalloc | 0.244 0.248 0.250 0.254 0.254 | 12656 |
| erb_render | challoc | 0.592 0.597 0.600 0.622 0.623 | 12340 |
| erb_render | jemalloc | 0.742 0.744 0.745 0.754 0.758 | 13452 |
| gem_list | challoc | 0.105 0.106 0.109 0.109 0.110 | 28216 |
| gem_list | jemalloc | 0.106 0.108 0.110 0.111 0.115 | 29208 |
| gc_churn | challoc | 1.793 1.801 1.813 1.843 1.864 | 392496 |
| gc_churn | jemalloc | 1.774 1.828 1.834 1.846 1.898 | 374188 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | challoc vs jemalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|
| startup | 0.041 | 14.1 | 0.040 | 15.2 | +2.5% | -7.6% |
| so_binary_trees | 3.730 | 27.8 | 3.708 | 29.1 | +0.6% | -4.4% |
| hash_aref_str | 0.280 | 10.9 | 0.273 | 12.0 | +2.6% | -9.2% |
| array_sample_100k__1k | 0.426 | 33.6 | 0.433 | 34.6 | -1.6% | -3.0% |
| array_sort_block | 3.971 | 30.8 | 4.008 | 31.9 | -0.9% | -3.7% |
| so_nbody | 0.661 | 11.2 | 0.667 | 12.5 | -0.9% | -10.3% |
| app_fib | 0.246 | 10.7 | 0.250 | 12.4 | -1.6% | -13.1% |
| erb_render | 0.600 | 12.1 | 0.745 | 13.1 | -19.5% | -8.3% |
| gem_list | 0.109 | 27.6 | 0.110 | 28.5 | -0.9% | -3.4% |
| gc_churn | 1.813 | 383.3 | 1.834 | 365.4 | -1.1% | +4.9% |

## gc_churn: GC.stat and RSS around 10 forced full GCs (medians over the timed runs)

| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| challoc | 54 | 25 | 29 | 2571264 | 225 | 383.3 | 375.2 | -2.1% | 383.3 |
| jemalloc | 54 | 25 | 29 | 2571264 | 225 | 365.6 | 357.5 | -2.2% | 365.4 |

GC counts are the process total (bursts plus the 10 forced cycles); "RSS before" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows).
