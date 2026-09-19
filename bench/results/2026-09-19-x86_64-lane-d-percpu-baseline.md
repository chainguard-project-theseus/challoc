# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), 2026-09-19, x86_64, mode `baseline`

- commit: 675f357
- date: 2026-09-19T20:39:10Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs; kernel 7.0.0-1011-gcp; docker Docker version 29.1.3
- features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
- mode: baseline
- baseline half: the default configuration alone (the key is accepted and ignored before task 8 lands; nothing here runs under it). The percpu half is the same script with MODE=percpu after the merge.
- steps: test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle; ROUNDS=5 SOAK_DURATION=1200 RUBY_SOAK_DURATION=600 IDLE_THREADS=48 CALLOC_ROUNDS=100 CALLOC_MAX_SECS=300 OSC_SECS=300 VALKEY_FULL=1
- driver: bench/percpu-campaign.sh (`make percpu-campaign MODE=baseline`), raw logs .tmp-percpu-baseline/ on the host that ran it
- load at start: 0.22 0.69 0.82

## test: `make test`
-- test: make test : rc=0 in 65 s

```
thread 'bitmap::tests::a_cleared_padding_bit_aborts_the_allocated_count' (405) panicked at src/bitmap.rs:200:9:
thread 'bitmap::tests::a_release_outside_the_sub_word_view_aborts' (406) panicked at src/bitmap.rs:174:9:
thread 'class::tests::a_corrupt_owner_word_aborts_the_next_take_group' (430) panicked at src/class.rs:2470:5:
thread 'class::tests::a_plan_over_a_slab_with_live_above_nslots_aborts' (434) panicked at src/class.rs:1836:9:
thread 'class::tests::a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts' (435) panicked at src/class.rs:2550:5:
thread 'class::tests::check_after_release_aborts' (443) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_a_guard_slab_slot_aborts' (444) panicked at src/class.rs:2501:5:
thread 'class::tests::a_purge_of_an_empty_slab_the_counts_do_not_know_aborts' (436) panicked at src/class.rs:2550:5:
thread 'class::tests::check_of_uncommitted_slab_aborts' (447) panicked at src/class.rs:2464:5:
thread 'class::tests::check_of_unclaimed_slot_aborts' (446) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_handed_out_uncommitted_slab_aborts' (445) panicked at src/class.rs:2516:5:
thread 'class::tests::check_zero_aborts_on_a_dirty_purged_slab' (449) panicked at src/class.rs:2595:5:
thread 'class::tests::check_through_the_wrong_arena_aborts' (448) panicked at src/class.rs:2524:5:
thread 'class::tests::duplicate_apart_in_one_slab_group_aborts' (466) panicked at src/class.rs:2542:5:
thread 'class::tests::duplicate_in_one_batch_aborts' (467) panicked at src/class.rs:2542:5:
thread 'class::tests::double_release_aborts' (465) panicked at src/class.rs:2542:5:
thread 'class::tests::guard_interval_must_be_a_power_of_two' (474) panicked at src/class.rs:723:9:
thread 'class::tests::release_into_an_uncommitted_slab_of_the_group_aborts' (491) panicked at src/class.rs:2510:5:
thread 'class::tests::release_of_interior_pointer_aborts' (493) panicked at src/class.rs:2536:5:
thread 'class::tests::release_outside_region_aborts' (494) panicked at src/class.rs:2536:5:
thread 'class::tests::release_through_the_wrong_arena_aborts' (496) panicked at src/class.rs:2524:5:
thread 'heap::tests::a_cached_pair_kept_by_the_split_is_caught_when_released' (559) panicked at src/class.rs:2542:5:
thread 'heap::tests::a_refill_of_a_cache_that_is_not_empty_aborts' (564) panicked at src/heap.rs:2185:13:
thread 'heap::tests::an_others_node_for_the_home_arena_aborts' (570) panicked at src/heap.rs:2079:9:
thread 'heap::tests::cached_double_free_aborts_at_the_flush' (573) panicked at src/class.rs:2542:5:
thread 'heap::tests::a_scheduled_pop_aborts_when_the_run_reaches_past_the_length' (567) panicked at src/heap.rs:1629:13:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (583) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (583) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (583) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (583) panicked at src/heap.rs:2701:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (583) panicked at src/class.rs:2542:5:
thread 'heap::tests::flush_aborts_on_a_cached_pair_straddling_the_split' (589) panicked at src/heap.rs:2787:5:
thread 'heap::tests::free_of_a_misaligned_pointer_aborts' (593) panicked at src/heap.rs:2676:5:
thread 'heap::tests::free_of_an_unallocated_huge_pointer_aborts' (596) panicked at src/huge.rs:1347:5:
thread 'heap::tests::free_of_a_pointer_outside_every_region_aborts' (595) panicked at src/heap.rs:2676:5:
thread 'heap::tests::freeing_caches_that_were_not_flushed_aborts' (597) panicked at src/heap.rs:1231:17:
thread 'heap::tests::free_into_an_uncommitted_slab_aborts' (592) panicked at src/class.rs:2464:5:
thread 'heap::tests::free_of_a_never_allocated_slot_aborts' (594) panicked at src/class.rs:2516:5:
thread 'heap::tests::sized_free_past_the_watermark_aborts_at_the_flush' (613) panicked at src/class.rs:2464:5:
thread 'heap::tests::stale_double_free_aborts_at_the_check' (618) panicked at src/class.rs:2516:5:
```

## ab: the Phase 0 set, larson64 and the Valkey model rows, challoc against jemalloc (5 rounds, one run)
-- ab: make ab VARIANTS=default=tree jemalloc BENCHES=larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free ROUNDS=5 RESULT=bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ab.md TITLE=per-CPU campaign (baseline) HOST_KIND=GCE x86_64 lane VM : rc=0 in 886 s
- variants: default=tree jemalloc; rows: larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free; table: `2026-09-19-x86_64-lane-d-percpu-baseline-ab.md` (the Medians section below)

## Medians

| program | unit | default | default RSS MiB | jemalloc | jemalloc RSS MiB | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 47.6 | 17.2 | 47.5 | 18.5 | +0.2% |
| larson10 | M ops/s | 284.9 | 96.2 | 297.1 | 93.3 | -4.1% |
| larson16 | M ops/s | 313.5 | 118.5 | 344.2 | 129.1 | -8.9% |
| larson64 | M ops/s | 344.3 | 476.1 | 373.0 | 413.9 | -7.7% |
| xmalloc | M frees/s | 335.404 | 52.5 | 333.781 | 91.6 | +0.5% |
| alloc-test1 | s | 2.851 | 13.9 | 2.703 | 13.3 | -5.2% |
| alloc-testN | s | 4.430 | 17.4 | 4.097 | 16.9 | -7.5% |
| cfrac | s | 3.650 | 3.7 | 3.461 | 5.7 | -5.2% |
| espresso | s | 4.042 | 4.1 | 4.047 | 6.3 | +0.1% |
| mstress | s | 0.633 | 334.0 | 1.065 | 286.3 | +68.2% |
| glibc-simple | s | 1.852 | 2.7 | 1.824 | 5.1 | -1.5% |
| redis | k req/s | 705.2 | 30.0 | 576.7 | 32.6 | +22.3% |
| valkey1 | M ops/s | 15.553 | 585.6 | 15.454 | 592.5 | +0.6% |
| valkey1-free | M ops/s | 16.053 | 585.6 | 13.623 | 592.5 | +17.8% |

Positive percentages mean the variant did worse than default (slower or lower throughput).

## calloc: the calloc-zero campaign (`check_zero:true` under the api:: load; 100 x stress 8 50 10 per configuration, at most 300 s each, then 10 x stress 16 100 20 standalone)
-- calloc: make run CMD=ROUNDS=100 MAX_SECS=300 STANDALONE=10 SKIP_SUITE=1 CONFIGS="default:check_zero:true" sh bench/probes/calloc-zero-campaign.sh : rc=0 in 75 s

```
== calloc-zero campaign 2026-09-19T20:55:01Z arch x86_64 ROUNDS=100 MAX_SECS=300 STANDALONE=10
== step 2: full suite
suite: skipped (SKIP_SUITE)
probe: kernel 7.0: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
== step 3: stress 8 50 10 under the api:: load, 100 rounds per configuration
RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 43 s, conf=check_zero:true, load runs so far 2
RESULT load: 3 runs of cargo test api::, 0 failed (1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 19.11s;1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 19.87s;1 test result: ok. 115 passed; 0 failed; 0 ignored; 0 measured; 375 filtered out; finished in 20.33s;)
== step 4: stress 16 100 20 standalone, 10 runs, check_zero:true
RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 8 s
== done 2026-09-19T20:56:16Z load: 20.89 12.33 7.52
```

## cgroup: the container tests (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation 300 s at 512 MiB)
-- cgroup-churn: make test-cgroup-churn : rc=0 in 10 s
-- cgroup-lazy: make test-cgroup-lazy : rc=0 in 7 s
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=300 : rc=0 in 303 s

```
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 512 MiB decay_ms 6911 retention_cap 25 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 512 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 9 MiB full_purges 0
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=48 current_peak=405 current_2s=17
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=318 rss_min=318 below_ms=-1 current_peak=405 current_2s=319
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 256 MiB decay_ms 6354 retention_cap 12 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 256 MiB decay_ms 10000 retention_cap 12 MiB empty_bytes 4 MiB full_purges 0
RESULT churn mode=purge mib=200 threshold=50 rss_peak=203 rss_2s=10 rss_min=10 below_ms=79 current_peak=203 current_2s=10
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=200 threshold=50 rss_peak=203 rss_2s=161 rss_min=161 below_ms=-1 current_peak=204 current_2s=162
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=lazy mib=200 rss_peak=203 rss_2s=203 rss_min=203 current_peak=204 current_2s=204 event_ms=79 lazy_purges=34 eager_purges=0 advice_2s=lazy pglazyfree_pages=48403 alloc_ms=94 reuse_ms=43 pressure_ms=157 pglazyfreed_pages=17867 current_pressure=511 reclaim_on_request=0
memory.events after: low 0 high 0 max 352 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=eager mib=200 rss_peak=203 rss_2s=13 rss_min=13 current_peak=204 current_2s=14 event_ms=78 lazy_purges=0 eager_purges=35 advice_2s=eager pglazyfree_pages=0 alloc_ms=92 reuse_ms=77 pressure_ms=148 pglazyfreed_pages=0 current_pressure=389 reclaim_on_request=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
challoc: stats.cgroup.limit 512 MiB current 350 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 25 MiB full_purges 0 pressure_events 0
RESULT oscillate cycles=97 seconds=302 low=200 high=480 current_peak=503 rss_peak=502 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
```

## valkey-suite: `make valkey STAGE=memefficiency` and STAGE=full
-- valkey-memefficiency: make valkey STAGE=memefficiency : rc=0 in 451 s

```
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=2cb625f621123289
[ok]: Memory efficiency with values in range 32 (132 ms)
[ok]: Memory efficiency with values in range 64 (124 ms)
[ok]: Memory efficiency with values in range 128 (128 ms)
[ok]: Memory efficiency with values in range 1024 (141 ms)
[ok]: Memory efficiency with values in range 16384 (326 ms)
[ok]: Active defrag - AOF loading (1527 ms)
[ok]: Active Defrag main dictionary: standalone (20858 ms)
[ok]: Active Defrag eval scripts: standalone (6076 ms)
[ok]: Active Defrag big hash: standalone (20601 ms)
[ok]: Active Defrag big list: standalone (19978 ms)
[ok]: Active Defrag big set: standalone (21566 ms)
[ok]: Active Defrag big zset: standalone random-score (21921 ms)
[ok]: Active Defrag big zset: standalone fixed-score (21943 ms)
[ok]: Active Defrag stream: standalone (21222 ms)
[ok]: Active Defrag pubsub: standalone (51256 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag main dictionary: cluster (19540 ms)
[ok]: Setup start_cluster (1971 ms)
[ok]: Active Defrag eval scripts: cluster (6227 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag big hash: cluster (20999 ms)
[ok]: Setup start_cluster (1969 ms)
[ok]: Active Defrag big list: cluster (20152 ms)
[ok]: Setup start_cluster (1971 ms)
[ok]: Active Defrag big set: cluster (21913 ms)
[ok]: Setup start_cluster (1973 ms)
[ok]: Active Defrag big zset: cluster random-score (21998 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag big zset: cluster fixed-score (21961 ms)
[ok]: Setup start_cluster (1970 ms)
[ok]: Active Defrag stream: cluster (20911 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag pubsub: cluster (50436 ms)
\o/ All tests passed without errors!
```

-- valkey-full: make valkey STAGE=full : rc=0 in 962 s

```
[ok]: GEODIST simple & unit (0 ms)
\o/ All tests passed without errors!
```

## valkey-soak: `make valkey-soak` (challoc and bundled jemalloc, 1200 s of load each, the Phase 1 protocol; the jemalloc soak saved as the cached baseline)
-- valkey-soak: make valkey-soak DURATION=1200 RESULT_DIR=bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-soak SAVE_BASELINE=1 COMMIT=675f357 : rc=0 in 2622 s
- summary: `2026-09-19-x86_64-lane-d-percpu-baseline-soak/2026-09-19-x86_64-valkey-soak.md` (CSVs beside it)

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 773744 | 784980 | +1.5% |
| write: valkey-benchmark p99 ms | 1.223 | 1.239 | -1.3% |
| write: memtier req/s | 956617 | 1003870 | +4.9% |
| write: memtier p99 ms | 0.96700 | 0.95100 | +1.7% |
| delete: valkey-benchmark req/s | 1482069 | 1483112 | +0.1% |
| delete: valkey-benchmark p99 ms | 0.663 | 0.687 | -3.5% |
| delete: memtier req/s | 1347599 | 1319201 | -2.1% |
| delete: memtier p99 ms | 0.72700 | 0.75900 | -4.2% |
| peak RSS MiB (VmHWM) | 586.2 | 584.1 | +0.4% |
| peak sampled used_memory_rss MiB | 586.1 | 584.1 | +0.3% |
| RSS MiB at end of write phase | 586.0 | 583.8 | +0.4% |
| RSS MiB at end of delete phase | 128.8 | 133.5 | -3.5% |
| RSS MiB after settle | 128.7 | 133.5 | -3.6% |
| allocator_frag_ratio after settle | 1.80 | 1.82 | -1.1% |
| active_defrag_hits | 256586 | 264978 | +3.3% |
| keys after settle | 103350 | 103459 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-soak/2026-09-19-x86_64-valkey-soak-challoc.csv` (130 rows)
- jemalloc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-soak/2026-09-19-x86_64-valkey-soak-jemalloc.csv` (130 rows)


## ruby-bench: the Ruby rows, challoc against jemalloc (5 rounds)
-- ruby-bench: make ruby-bench ALLOCS=challoc jemalloc ROUNDS=5 RESULT=bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby.md HOST_KIND=GCE x86_64 lane VM : rc=0 in 185 s
- table: `2026-09-19-x86_64-lane-d-percpu-baseline-ruby.md`

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


## ruby-test: `make ruby STAGE=test` (Ruby's test-all on the --with-jemalloc build)
-- ruby-test: make ruby STAGE=test : rc=2 in 59 s

```
== challoc as libjemalloc.so.2 -> /src/target/ruby/jemalloc-compat/stage/lib
== ruby v3_4_10 -> /src/target/ruby/src-v3_4_10
== make -j16 (challoc; log: /src/target/ruby/logs/build-challoc-x86_64.log)
== check (challoc)
== make test-all TESTOPTS='-j16 -q --tty=no'  (challoc; log: /src/target/ruby/logs/test-all-challoc-x86_64.log)
32859 tests, 6587880 assertions, 7 failures, 2 errors, 407 skips
== test-all (challoc, x86_64): rc=2, 58 s
== 32859 tests, 6587880 assertions, 7 failures, 2 errors, 407 skips
```

## ruby-soak: `make ruby-soak` 16 workers, 600 s, challoc against jemalloc
-- ruby-soak: make ruby-soak ALLOCS=challoc jemalloc DURATION=600 RESULT=bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak.md COMMIT=675f357 : rc=0 in 1208 s
- table: `2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak.md`; arenas from the report: Arenas: 17 (64 automatic), explicit caches: 0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 419.1 | 184.1 | +127.6% |
| VmHWM at the end | 421.8 | 185.9 | +126.9% |
| at minute 1 (t=60) | 308.9 | 72.5 | +326.1% |
| at minute 5, before GC.compact (t=295) | 347.2 | 115.3 | +201.1% |
| at minute 5, after GC.compact (t=300) | 362.8 | 116.2 | +212.2% |
| at minute 10 (t=600) | 413.0 | 167.0 | +147.3% |
| after the last full GC (final) | 413.0 | 167.0 | +147.3% |
| lowest right after a periodic full GC | 302.0 | 72.5 | +316.6% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 40.7 | 83.0 | 0.0 | 39.2 | 55.9 | 15.5 |
| 30 | fullgc | 302.0 | 330.3 | 8.1 | 72.7 | 79.5 | 32.7 |
| 60 | fullgc | 308.9 | 337.4 | 15.1 | 72.5 | 79.3 | 36.0 |
| 90 | fullgc | 313.0 | 337.0 | 24.5 | 74.1 | 80.4 | 34.9 |
| 120 | fullgc | 329.1 | 353.5 | 19.4 | 89.8 | 94.8 | 59.7 |
| 150 | fullgc | 333.0 | 359.3 | 25.3 | 92.8 | 97.1 | 63.5 |
| 180 | fullgc | 331.3 | 352.4 | 35.5 | 91.4 | 94.8 | 65.8 |
| 210 | fullgc | 328.9 | 359.7 | 38.4 | 90.6 | 93.7 | 67.0 |
| 240 | fullgc | 335.5 | 360.8 | 40.5 | 97.6 | 99.4 | 61.3 |
| 270 | fullgc | 332.8 | 356.9 | 47.4 | 96.4 | 97.7 | 63.0 |
| 300 | compact | 362.8 | 389.7 | 34.1 | 116.2 | 115.7 | 45.1 |
| 330 | fullgc | 398.0 | 416.4 | 40.8 | 163.1 | 159.2 | 58.1 |
| 360 | fullgc | 377.4 | 396.6 | 61.8 | 129.9 | 125.8 | 91.5 |
| 390 | fullgc | 381.2 | 398.3 | 62.2 | 147.5 | 142.9 | 74.5 |
| 420 | fullgc | 390.3 | 405.5 | 58.3 | 147.0 | 141.6 | 75.7 |
| 450 | fullgc | 390.8 | 400.2 | 68.3 | 157.0 | 150.9 | 66.5 |
| 480 | fullgc | 398.0 | 402.7 | 69.0 | 159.9 | 152.9 | 64.5 |
| 510 | fullgc | 395.9 | 399.3 | 75.4 | 166.6 | 158.5 | 58.9 |
| 540 | fullgc | 406.6 | 409.5 | 67.9 | 171.0 | 162.2 | 55.3 |
| 570 | fullgc | 409.6 | 411.7 | 69.4 | 172.4 | 163.0 | 54.5 |
| 600 | fullgc | 413.0 | 414.4 | 71.3 | 167.0 | 156.4 | 61.2 |
| 600 | final-fullgc | 413.0 | 414.4 | 71.3 | 167.0 | 156.4 | 61.2 |

## Samples

- challoc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-challoc-mstats.txt`; summary line: `jobs=152220 live_objects=76120 gc_count=16396 major=1353 minor=15043 heap_pages=352 rss_final_kb=422888 hwm_kb=431936 mallctl=yes alloc_allocated=111660096 alloc_active=228794368 alloc_resident=434630656 alloc_retained=74698752 alloc_mapped=509329408 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`
- jemalloc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=149536 live_objects=74782 gc_count=16348 major=1377 minor=14971 heap_pages=346 rss_final_kb=171044 hwm_kb=190368 mallctl=yes alloc_allocated=98489672 alloc_active=111685632 alloc_resident=164020224 alloc_retained=64139264 alloc_mapped=194859008 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16`

## ruby-idle: `make ruby-soak` 16 workers + 48 mostly idle threads on 16 CPUs, 300 s (RSS bounded by cores, not threads)
-- ruby-idle: make ruby-soak ALLOCS=challoc jemalloc THREADS=16 IDLE_THREADS=48 DURATION=300 COMPACT_AT=150 RESULT=bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle.md COMMIT=675f357 : rc=0 in 609 s
- table: `2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle.md`; arenas from the report: Arenas: 64 (64 automatic), explicit caches: 0; summary lines: threads=16 idle_threads=48 idle_bursts=2093 nproc=16 (challoc); threads=16 idle_threads=48 idle_bursts=2105 nproc=16 (jemalloc) (read from .tmp-percpu-baseline/ruby-idle.log after the run; the script's first form looked in the target volume)

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 462.8 | 156.0 | +196.7% |
| VmHWM at the end | 462.8 | 160.2 | +188.9% |
| at minute 1 (t=60) | 378.5 | 135.6 | +179.1% |
| at minute 5, before GC.compact (t=145) | 404.2 | 144.2 | +180.3% |
| at minute 5, after GC.compact (t=150) | 413.6 | 144.8 | +185.6% |
| at minute 10 (t=300) | 462.8 | 115.3 | +301.4% |
| after the last full GC (final) | 462.8 | 115.3 | +301.4% |
| lowest right after a periodic full GC | 361.3 | 132.5 | +172.7% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 108.2 | 231.9 | 0.0 | 100.8 | 179.8 | 54.6 |
| 30 | fullgc | 361.3 | 461.3 | 20.3 | 135.8 | 193.2 | 82.0 |
| 60 | fullgc | 378.5 | 476.8 | 29.3 | 135.6 | 192.4 | 82.9 |
| 90 | fullgc | 379.4 | 474.2 | 41.0 | 133.4 | 189.2 | 86.1 |
| 120 | fullgc | 397.6 | 489.1 | 37.6 | 148.6 | 203.0 | 114.6 |
| 150 | compact | 413.6 | 506.1 | 41.0 | 144.8 | 198.7 | 121.9 |
| 180 | fullgc | 424.8 | 513.3 | 45.5 | 140.3 | 193.8 | 126.8 |
| 210 | fullgc | 428.1 | 521.7 | 51.3 | 132.5 | 185.3 | 135.3 |
| 240 | fullgc | 439.8 | 528.9 | 52.7 | 137.4 | 189.0 | 131.7 |
| 270 | fullgc | 435.0 | 524.6 | 62.1 | 135.6 | 186.7 | 134.0 |
| 300 | fullgc | 462.8 | 556.8 | 52.2 | 115.3 | 162.5 | 158.4 |
| 300 | final-fullgc | 462.8 | 556.8 | 52.2 | 115.3 | 162.5 | 158.4 |

## Samples

- challoc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-challoc-mstats.txt`; summary line: `jobs=71993 live_objects=36008 gc_count=13496 major=1296 minor=12200 heap_pages=204 rss_final_kb=473908 hwm_kb=473908 mallctl=yes alloc_allocated=121486432 alloc_active=250077184 alloc_resident=583872512 alloc_retained=54636544 alloc_mapped=638509056 compacted=true threads=16 idle_threads=48 idle_bursts=2093 nproc=16`
- jemalloc: `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-19-x86_64-lane-d-percpu-baseline-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=73898 live_objects=36962 gc_count=13517 major=1259 minor=12258 heap_pages=204 rss_final_kb=118108 hwm_kb=164076 mallctl=yes alloc_allocated=113160472 alloc_active=126980096 alloc_resident=170426368 alloc_retained=166084608 alloc_mapped=300531712 compacted=true threads=16 idle_threads=48 idle_bursts=2105 nproc=16`

## done: 7452 s, load at end 1.07 1.05 1.12
