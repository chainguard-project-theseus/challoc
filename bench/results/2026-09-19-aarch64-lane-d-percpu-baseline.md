# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), 2026-09-19, aarch64, mode `baseline`

- commit: 675f357
- date: 2026-09-19T20:39:05Z
- host: Neoverse-V2, 16 CPUs; kernel 7.0.0-1011-gcp; docker Docker version 29.1.3
- features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
- mode: baseline
- baseline half: the default configuration alone (the key is accepted and ignored before task 8 lands; nothing here runs under it). The percpu half is the same script with MODE=percpu after the merge.
- steps: test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle; ROUNDS=5 SOAK_DURATION=1200 RUBY_SOAK_DURATION=600 IDLE_THREADS=48 CALLOC_ROUNDS=100 CALLOC_MAX_SECS=300 OSC_SECS=300 VALKEY_FULL=0
- driver: bench/percpu-campaign.sh (`make percpu-campaign MODE=baseline`), raw logs .tmp-percpu-baseline/ on the host that ran it
- load at start: 0.35 0.54 0.60

## test: `make test`
-- test: make test : rc=0 in 60 s

```
thread 'bitmap::tests::a_release_outside_the_sub_word_view_aborts' (406) panicked at src/bitmap.rs:174:9:
thread 'bitmap::tests::a_cleared_padding_bit_aborts_the_allocated_count' (405) panicked at src/bitmap.rs:200:9:
thread 'class::tests::a_purge_of_an_empty_slab_the_counts_do_not_know_aborts' (432) panicked at src/class.rs:2550:5:
thread 'class::tests::a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts' (431) panicked at src/class.rs:2550:5:
thread 'class::tests::a_plan_over_a_slab_with_live_above_nslots_aborts' (430) panicked at src/class.rs:1836:9:
thread 'class::tests::check_after_release_aborts' (443) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_handed_out_uncommitted_slab_aborts' (445) panicked at src/class.rs:2516:5:
thread 'class::tests::a_corrupt_owner_word_aborts_the_next_take_group' (426) panicked at src/class.rs:2470:5:
thread 'class::tests::check_of_unclaimed_slot_aborts' (446) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_uncommitted_slab_aborts' (447) panicked at src/class.rs:2464:5:
thread 'class::tests::check_of_a_guard_slab_slot_aborts' (444) panicked at src/class.rs:2501:5:
thread 'class::tests::check_zero_aborts_on_a_dirty_purged_slab' (449) panicked at src/class.rs:2595:5:
thread 'class::tests::check_through_the_wrong_arena_aborts' (448) panicked at src/class.rs:2524:5:
thread 'class::tests::double_release_aborts' (472) panicked at src/class.rs:2542:5:
thread 'class::tests::guard_interval_must_be_a_power_of_two' (478) panicked at src/class.rs:723:9:
thread 'class::tests::duplicate_apart_in_one_slab_group_aborts' (473) panicked at src/class.rs:2542:5:
thread 'class::tests::duplicate_in_one_batch_aborts' (474) panicked at src/class.rs:2542:5:
thread 'class::tests::release_of_interior_pointer_aborts' (493) panicked at src/class.rs:2536:5:
thread 'class::tests::release_outside_region_aborts' (494) panicked at src/class.rs:2536:5:
thread 'class::tests::release_into_an_uncommitted_slab_of_the_group_aborts' (491) panicked at src/class.rs:2510:5:
thread 'class::tests::release_through_the_wrong_arena_aborts' (496) panicked at src/class.rs:2524:5:
thread 'heap::tests::a_cached_pair_kept_by_the_split_is_caught_when_released' (560) panicked at src/class.rs:2542:5:
thread 'heap::tests::a_refill_of_a_cache_that_is_not_empty_aborts' (569) panicked at src/heap.rs:2185:13:
thread 'heap::tests::a_scheduled_pop_aborts_when_the_run_reaches_past_the_length' (570) panicked at src/heap.rs:1629:13:
thread 'heap::tests::an_others_node_for_the_home_arena_aborts' (573) panicked at src/heap.rs:2079:9:
thread 'heap::tests::cached_double_free_aborts_at_the_flush' (576) panicked at src/class.rs:2542:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (586) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (586) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (586) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (586) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (586) panicked at src/class.rs:2542:5:
thread 'heap::tests::flush_aborts_on_a_cached_pair_straddling_the_split' (592) panicked at src/heap.rs:2787:5:
thread 'heap::tests::free_into_an_uncommitted_slab_aborts' (595) panicked at src/class.rs:2464:5:
thread 'heap::tests::free_of_a_misaligned_pointer_aborts' (596) panicked at src/heap.rs:2676:5:
thread 'heap::tests::free_of_a_pointer_outside_every_region_aborts' (598) panicked at src/heap.rs:2676:5:
thread 'heap::tests::free_of_a_never_allocated_slot_aborts' (597) panicked at src/class.rs:2516:5:
thread 'heap::tests::free_of_an_unallocated_huge_pointer_aborts' (599) panicked at src/huge.rs:1347:5:
thread 'heap::tests::freeing_caches_that_were_not_flushed_aborts' (600) panicked at src/heap.rs:1231:17:
thread 'heap::tests::sized_free_past_the_watermark_aborts_at_the_flush' (616) panicked at src/class.rs:2464:5:
thread 'heap::tests::stale_double_free_aborts_at_the_check' (621) panicked at src/class.rs:2516:5:
```

## ab: the Phase 0 set, larson64 and the Valkey model rows, challoc against jemalloc (5 rounds, one run)
-- ab: make ab VARIANTS=default=tree jemalloc BENCHES=larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free ROUNDS=5 RESULT=bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ab.md TITLE=per-CPU campaign (baseline) HOST_KIND=GCE aarch64 lane VM : rc=0 in 871 s
- variants: default=tree jemalloc; rows: larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free; table: `2026-09-19-aarch64-lane-d-percpu-baseline-ab.md` (the Medians section below)

## Medians

| program | unit | default | default RSS MiB | jemalloc | jemalloc RSS MiB | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 | 16.1 | 45.0 | 18.9 | -5.8% |
| larson10 | M ops/s | 367.3 | 112.3 | 381.1 | 131.2 | -3.6% |
| larson16 | M ops/s | 594.7 | 164.0 | 613.1 | 206.0 | -3.0% |
| larson64 | M ops/s | 591.4 | 562.0 | 611.2 | 525.3 | -3.2% |
| xmalloc | M frees/s | 586.022 | 54.4 | 475.170 | 76.7 | +23.3% |
| alloc-test1 | s | 2.571 | 13.0 | 2.454 | 12.3 | -4.6% |
| alloc-testN | s | 2.301 | 16.3 | 2.199 | 16.6 | -4.4% |
| cfrac | s | 4.464 | 2.7 | 4.430 | 4.7 | -0.8% |
| espresso | s | 3.652 | 3.6 | 3.747 | 6.2 | +2.6% |
| mstress | s | 0.374 | 318.7 | 0.646 | 291.3 | +72.7% |
| glibc-simple | s | 2.167 | 2.3 | 2.525 | 4.4 | +16.5% |
| redis | k req/s | 498.0 | 29.1 | 411.2 | 31.1 | +21.1% |
| valkey1 | M ops/s | 7.857 | 584.6 | 7.730 | 585.7 | +1.6% |
| valkey1-free | M ops/s | 7.777 | 584.6 | 7.381 | 585.7 | +5.4% |

Positive percentages mean the variant did worse than default (slower or lower throughput).

## calloc: the calloc-zero campaign (`check_zero:true` under the api:: load; 100 x stress 8 50 10 per configuration, at most 300 s each, then 10 x stress 16 100 20 standalone)
-- calloc: make run CMD=ROUNDS=100 MAX_SECS=300 STANDALONE=10 SKIP_SUITE=1 CONFIGS="default:check_zero:true" sh bench/probes/calloc-zero-campaign.sh : rc=0 in 51 s

```
== calloc-zero campaign 2026-09-19T20:54:36Z arch aarch64 ROUNDS=100 MAX_SECS=300 STANDALONE=10
== step 2: full suite
suite: skipped (SKIP_SUITE)
probe: kernel 7.0: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
== step 3: stress 8 50 10 under the api:: load, 100 rounds per configuration
RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 26 s, conf=check_zero:true, load runs so far 2
RESULT load: 3 runs of cargo test api::, 0 failed (1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 12.66s;1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 12.96s;1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 13.08s;)
== step 4: stress 16 100 20 standalone, 10 runs, check_zero:true
RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 4 s
== done 2026-09-19T20:55:27Z load: 16.81 10.54 6.59
```

## cgroup: the container tests (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation 300 s at 512 MiB)
-- cgroup-churn: make test-cgroup-churn : rc=0 in 10 s
-- cgroup-lazy: make test-cgroup-lazy : rc=0 in 7 s
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=300 : rc=0 in 302 s

```
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 512 MiB decay_ms 6164 retention_cap 25 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 512 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 9 MiB full_purges 0
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=52 current_peak=406 current_2s=18
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=317 rss_min=317 below_ms=-1 current_peak=405 current_2s=320
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 256 MiB decay_ms 6190 retention_cap 12 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 256 MiB decay_ms 10000 retention_cap 12 MiB empty_bytes 4 MiB full_purges 0
RESULT churn mode=purge mib=200 threshold=50 rss_peak=202 rss_2s=9 rss_min=9 below_ms=82 current_peak=204 current_2s=11
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=200 threshold=50 rss_peak=202 rss_2s=160 rss_min=160 below_ms=-1 current_peak=205 current_2s=163
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=lazy mib=200 rss_peak=202 rss_2s=202 rss_min=202 current_peak=207 current_2s=207 event_ms=82 lazy_purges=35 eager_purges=0 advice_2s=lazy pglazyfree_pages=48576 alloc_ms=63 reuse_ms=5 pressure_ms=109 pglazyfreed_pages=18556 current_pressure=511 reclaim_on_request=0
memory.events after: low 0 high 0 max 365 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=eager mib=200 rss_peak=202 rss_2s=12 rss_min=12 current_peak=205 current_2s=15 event_ms=82 lazy_purges=0 eager_purges=35 advice_2s=eager pglazyfree_pages=0 alloc_ms=61 reuse_ms=48 pressure_ms=95 pglazyfreed_pages=0 current_pressure=389 reclaim_on_request=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
challoc: stats.cgroup.limit 512 MiB current 294 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 22 MiB full_purges 0 pressure_events 0
RESULT oscillate cycles=98 seconds=301 low=200 high=480 current_peak=502 rss_peak=501 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
```

## valkey-suite: `make valkey STAGE=memefficiency`
-- valkey-memefficiency: make valkey STAGE=memefficiency : rc=0 in 438 s

```
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=aa1de31696114f97
[ok]: Memory efficiency with values in range 32 (130 ms)
[ok]: Memory efficiency with values in range 64 (132 ms)
[ok]: Memory efficiency with values in range 128 (133 ms)
[ok]: Memory efficiency with values in range 1024 (145 ms)
[ok]: Memory efficiency with values in range 16384 (339 ms)
[ok]: Active defrag - AOF loading (1559 ms)
[ok]: Active Defrag main dictionary: standalone (20534 ms)
[ok]: Active Defrag eval scripts: standalone (6052 ms)
[ok]: Active Defrag big hash: standalone (20264 ms)
[ok]: Active Defrag big list: standalone (19757 ms)
[ok]: Active Defrag big set: standalone (20958 ms)
[ok]: Active Defrag big zset: standalone random-score (21236 ms)
[ok]: Active Defrag big zset: standalone fixed-score (21258 ms)
[ok]: Active Defrag stream: standalone (20536 ms)
[ok]: Active Defrag pubsub: standalone (49523 ms)
[ok]: Setup start_cluster (1914 ms)
[ok]: Active Defrag main dictionary: cluster (19123 ms)
[ok]: Setup start_cluster (1912 ms)
[ok]: Active Defrag eval scripts: cluster (6159 ms)
[ok]: Setup start_cluster (1912 ms)
[ok]: Active Defrag big hash: cluster (20267 ms)
[ok]: Setup start_cluster (1913 ms)
[ok]: Active Defrag big list: cluster (19485 ms)
[ok]: Setup start_cluster (1912 ms)
[ok]: Active Defrag big set: cluster (20773 ms)
[ok]: Setup start_cluster (1912 ms)
[ok]: Active Defrag big zset: cluster random-score (21232 ms)
[ok]: Setup start_cluster (1912 ms)
[ok]: Active Defrag big zset: cluster fixed-score (21213 ms)
[ok]: Setup start_cluster (1913 ms)
[ok]: Active Defrag stream: cluster (20516 ms)
[ok]: Setup start_cluster (1913 ms)
[ok]: Active Defrag pubsub: cluster (49683 ms)
\o/ All tests passed without errors!
```

## valkey-soak: `make valkey-soak` (challoc and bundled jemalloc, 1200 s of load each, the Phase 1 protocol; the jemalloc soak saved as the cached baseline)
-- valkey-soak: make valkey-soak DURATION=1200 RESULT_DIR=bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-soak SAVE_BASELINE=1 COMMIT=675f357 : rc=0 in 2592 s
- summary: `2026-09-19-aarch64-lane-d-percpu-baseline-soak/2026-09-19-aarch64-valkey-soak.md` (CSVs beside it)

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 938347 | 893210 | -4.8% |
| write: valkey-benchmark p99 ms | 1.047 | 1.103 | -5.1% |
| write: memtier req/s | 960765 | 938768 | -2.3% |
| write: memtier p99 ms | 0.95900 | 0.99900 | -4.0% |
| delete: valkey-benchmark req/s | 1422029 | 1441230 | +1.4% |
| delete: valkey-benchmark p99 ms | 0.719 | 0.703 | +2.3% |
| delete: memtier req/s | 1246858 | 1265327 | +1.5% |
| delete: memtier p99 ms | 0.77500 | 0.76700 | +1.0% |
| peak RSS MiB (VmHWM) | 579.3 | 573.2 | +1.1% |
| peak sampled used_memory_rss MiB | 579.2 | 573.2 | +1.0% |
| RSS MiB at end of write phase | 579.2 | 573.1 | +1.1% |
| RSS MiB at end of delete phase | 130.9 | 129.8 | +0.8% |
| RSS MiB after settle | 130.9 | 129.8 | +0.8% |
| allocator_frag_ratio after settle | 1.81 | 1.80 | +0.6% |
| active_defrag_hits | 247337 | 255712 | +3.4% |
| keys after settle | 105949 | 103443 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-soak/2026-09-19-aarch64-valkey-soak-challoc.csv` (131 rows)
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-soak/2026-09-19-aarch64-valkey-soak-jemalloc.csv` (127 rows)


## ruby-bench: the Ruby rows, challoc against jemalloc (5 rounds)
-- ruby-bench: make ruby-bench ALLOCS=challoc jemalloc ROUNDS=5 RESULT=bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby.md HOST_KIND=GCE aarch64 lane VM : rc=0 in 215 s
- table: `2026-09-19-aarch64-lane-d-percpu-baseline-ruby.md`

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


## ruby-test: `make ruby STAGE=test` (Ruby's test-all on the --with-jemalloc build)
-- ruby-test: make ruby STAGE=test : rc=2 in 397 s

```
== challoc as libjemalloc.so.2 -> /src/target/ruby/jemalloc-compat/stage/lib
== ruby v3_4_10 -> /src/target/ruby/src-v3_4_10
== make -j16 (challoc; log: /src/target/ruby/logs/build-challoc-aarch64.log)
== check (challoc)
== make test-all TESTOPTS='-j16 -q --tty=no'  (challoc; log: /src/target/ruby/logs/test-all-challoc-aarch64.log)
32827 tests, 6586202 assertions, 7 failures, 2 errors, 409 skips
== test-all (challoc, aarch64): rc=2, 395 s
== 32827 tests, 6586202 assertions, 7 failures, 2 errors, 409 skips
```

## ruby-soak: `make ruby-soak` 16 workers, 600 s, challoc against jemalloc
-- ruby-soak: make ruby-soak ALLOCS=challoc jemalloc DURATION=600 RESULT=bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak.md COMMIT=675f357 : rc=0 in 1208 s
- table: `2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak.md`; arenas from the report: Arenas: 17 (64 automatic), explicit caches: 0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 403.6 | 198.4 | +103.4% |
| VmHWM at the end | 403.4 | 201.1 | +100.6% |
| at minute 1 (t=60) | 285.2 | 96.7 | +194.9% |
| at minute 5, before GC.compact (t=295) | 315.6 | 131.4 | +140.2% |
| at minute 5, after GC.compact (t=300) | 323.4 | 130.3 | +148.2% |
| at minute 10 (t=600) | 397.0 | 160.6 | +147.2% |
| after the last full GC (final) | 397.0 | 160.6 | +147.2% |
| lowest right after a periodic full GC | 279.3 | 93.5 | +198.7% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.4 | 82.8 | 0.0 | 41.7 | 111.2 | 31.6 |
| 30 | fullgc | 279.3 | 306.9 | 11.2 | 93.5 | 133.6 | 86.8 |
| 60 | fullgc | 285.2 | 312.1 | 13.0 | 96.7 | 135.8 | 88.1 |
| 90 | fullgc | 290.9 | 314.1 | 19.8 | 101.2 | 140.7 | 83.2 |
| 120 | fullgc | 296.2 | 320.8 | 19.8 | 105.4 | 142.4 | 81.4 |
| 150 | fullgc | 300.8 | 326.5 | 23.2 | 115.5 | 151.9 | 83.9 |
| 180 | fullgc | 306.2 | 329.0 | 24.8 | 122.4 | 158.8 | 89.1 |
| 210 | fullgc | 308.9 | 338.7 | 27.5 | 124.2 | 160.6 | 111.3 |
| 240 | fullgc | 313.5 | 340.5 | 30.1 | 127.8 | 161.7 | 122.2 |
| 270 | fullgc | 315.5 | 341.5 | 32.0 | 129.0 | 161.2 | 122.6 |
| 300 | compact | 323.4 | 348.3 | 31.3 | 130.3 | 160.8 | 123.1 |
| 330 | fullgc | 346.6 | 375.3 | 34.5 | 145.3 | 169.9 | 114.0 |
| 360 | fullgc | 367.0 | 387.1 | 38.5 | 159.2 | 179.6 | 104.3 |
| 390 | fullgc | 368.5 | 385.3 | 49.5 | 169.3 | 198.3 | 89.6 |
| 420 | fullgc | 364.9 | 381.2 | 60.0 | 170.6 | 194.9 | 93.0 |
| 450 | fullgc | 376.5 | 393.3 | 53.3 | 175.9 | 200.1 | 88.1 |
| 480 | fullgc | 383.7 | 398.5 | 54.5 | 182.6 | 199.3 | 89.0 |
| 510 | fullgc | 386.0 | 393.3 | 63.9 | 183.5 | 200.2 | 88.1 |
| 540 | fullgc | 392.0 | 399.7 | 61.5 | 189.4 | 203.3 | 85.2 |
| 570 | fullgc | 390.7 | 396.9 | 67.6 | 195.2 | 210.6 | 78.0 |
| 600 | fullgc | 397.0 | 401.7 | 66.0 | 160.6 | 169.6 | 119.2 |
| 600 | final-fullgc | 397.0 | 401.7 | 66.0 | 160.6 | 169.6 | 119.2 |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-challoc-mstats.txt`; summary line: `jobs=136950 live_objects=68486 gc_count=16521 major=139 minor=16382 heap_pages=326 rss_final_kb=406488 hwm_kb=413108 mallctl=yes alloc_allocated=103981600 alloc_active=217980928 alloc_resident=421232640 alloc_retained=69160960 alloc_mapped=490393600 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=139314 live_objects=69670 gc_count=16510 major=145 minor=16365 heap_pages=330 rss_final_kb=164500 hwm_kb=205964 mallctl=yes alloc_allocated=95063688 alloc_active=145293312 alloc_resident=177799168 alloc_retained=124977152 alloc_mapped=209518592 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`

## ruby-idle: `make ruby-soak` 16 workers + 48 mostly idle threads on 16 CPUs, 300 s (RSS bounded by cores, not threads)
-- ruby-idle: make ruby-soak ALLOCS=challoc jemalloc THREADS=16 IDLE_THREADS=48 DURATION=300 COMPACT_AT=150 RESULT=bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle.md COMMIT=675f357 : rc=0 in 609 s
- table: `2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle.md`; arenas from the report: Arenas: 64 (64 automatic), explicit caches: 0; summary lines: threads=16 idle_threads=48 idle_bursts=2059 nproc=16 (challoc); threads=16 idle_threads=48 idle_bursts=2073 nproc=16 (jemalloc) (read from .tmp-percpu-baseline/ruby-idle.log after the run; the script's first form looked in the target volume)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 415.6 | 191.5 | +117.0% |
| VmHWM at the end | 415.6 | 191.5 | +117.0% |
| at minute 1 (t=60) | 352.1 | 170.1 | +107.0% |
| at minute 5, before GC.compact (t=145) | 369.6 | 177.2 | +108.6% |
| at minute 5, after GC.compact (t=150) | 378.3 | 180.9 | +109.1% |
| at minute 10 (t=300) | 415.6 | 187.8 | +121.3% |
| after the last full GC (final) | 415.6 | 141.1 | +194.5% |
| lowest right after a periodic full GC | 341.2 | 162.4 | +110.1% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 108.1 | 227.9 | 0.0 | 107.5 | 330.4 | 107.5 |
| 30 | fullgc | 341.2 | 440.0 | 23.6 | 162.4 | 343.8 | 162.2 |
| 60 | fullgc | 352.1 | 445.3 | 33.3 | 170.1 | 346.6 | 166.4 |
| 90 | fullgc | 364.8 | 457.9 | 34.9 | 178.4 | 355.8 | 157.1 |
| 120 | fullgc | 369.4 | 462.5 | 38.3 | 172.8 | 342.4 | 170.6 |
| 150 | compact | 378.3 | 471.1 | 41.9 | 180.9 | 351.1 | 173.8 |
| 180 | fullgc | 387.8 | 473.6 | 46.8 | 184.3 | 346.3 | 198.6 |
| 210 | fullgc | 402.3 | 497.2 | 44.8 | 186.5 | 348.5 | 208.4 |
| 240 | fullgc | 410.3 | 502.1 | 49.2 | 188.1 | 351.8 | 221.1 |
| 270 | fullgc | 414.3 | 504.7 | 52.4 | 188.5 | 349.2 | 227.7 |
| 300 | fullgc | 415.6 | 503.9 | 57.3 | 187.8 | 347.4 | 229.6 |
| 300 | final-fullgc | 415.6 | 503.9 | 57.3 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-challoc-mstats.txt`; summary line: `jobs=67747 live_objects=33888 gc_count=13861 major=34 minor=13827 heap_pages=154 rss_final_kb=425608 hwm_kb=425608 mallctl=yes alloc_allocated=116664592 alloc_active=235646976 alloc_resident=528474112 alloc_retained=59961344 alloc_mapped=588435456 compacted=true threads=16 idle_threads=48 idle_bursts=2059 nproc=16`
- jemalloc: `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-19-aarch64-lane-d-percpu-baseline-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=69341 live_objects=34686 gc_count=13813 major=29 minor=13784 heap_pages=168 rss_final_kb=144508 hwm_kb=196120 mallctl=yes alloc_allocated=111030232 alloc_active=192151552 alloc_resident=247136256 alloc_retained=357826560 alloc_mapped=372506624 compacted=true threads=16 idle_threads=48 idle_bursts=2073 nproc=16`

## done: 6768 s, load at end 0.92 0.98 1.06
