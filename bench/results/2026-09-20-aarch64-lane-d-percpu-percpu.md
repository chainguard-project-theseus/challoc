# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), 2026-09-20, aarch64, mode `percpu`

- commit: 0c60b1a
- date: 2026-09-20T04:51:36Z
- host: Neoverse-V2, 16 CPUs; kernel 7.0.0-1011-gcp; docker Docker version 29.1.3
- features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
- mode: percpu, challoc under CHALLOC_CONF=percpu_arena:percpu in every step but ab (there the variant percpu=tree:percpu_arena:percpu carries the key and default=tree is the same run default)
- steps: test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle; ROUNDS=5 SOAK_DURATION=1200 RUBY_SOAK_DURATION=600 IDLE_THREADS=48 CALLOC_ROUNDS=100 CALLOC_MAX_SECS=300 OSC_SECS=300 VALKEY_FULL=0
- driver: bench/percpu-campaign.sh (`make percpu-campaign MODE=percpu`), raw logs .tmp-percpu-percpu/ on the host that ran it
- load at start: 4.18 2.22 0.91

## test: `make test` under CHALLOC_CONF=percpu_arena:percpu
-- test: make test (CHALLOC_CONF=percpu_arena:percpu): rc=2 in 28 s; cgroup memory.peak max 787.4 MiB over 1 container cgroup(s)

```
thread 'bitmap::tests::a_cleared_padding_bit_aborts_the_allocated_count' (405) panicked at src/bitmap.rs:200:9:
thread 'bitmap::tests::a_release_outside_the_sub_word_view_aborts' (406) panicked at src/bitmap.rs:174:9:
thread 'class::tests::a_purge_of_an_empty_slab_the_counts_do_not_know_aborts' (432) panicked at src/class.rs:2550:5:
thread 'class::tests::a_corrupt_owner_word_aborts_the_next_take_group' (425) panicked at src/class.rs:2470:5:
thread 'class::tests::a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts' (431) panicked at src/class.rs:2550:5:
thread 'class::tests::check_of_handed_out_uncommitted_slab_aborts' (445) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_unclaimed_slot_aborts' (446) panicked at src/class.rs:2516:5:
thread 'class::tests::a_plan_over_a_slab_with_live_above_nslots_aborts' (430) panicked at src/class.rs:1836:9:
thread 'class::tests::check_through_the_wrong_arena_aborts' (448) panicked at src/class.rs:2524:5:
thread 'class::tests::check_of_a_guard_slab_slot_aborts' (444) panicked at src/class.rs:2501:5:
thread 'class::tests::check_after_release_aborts' (443) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_uncommitted_slab_aborts' (447) panicked at src/class.rs:2464:5:
thread 'class::tests::check_zero_aborts_on_a_dirty_purged_slab' (449) panicked at src/class.rs:2595:5:
thread 'class::tests::double_release_aborts' (468) panicked at src/class.rs:2542:5:
thread 'class::tests::duplicate_apart_in_one_slab_group_aborts' (469) panicked at src/class.rs:2542:5:
thread 'class::tests::guard_interval_must_be_a_power_of_two' (474) panicked at src/class.rs:723:9:
thread 'class::tests::duplicate_in_one_batch_aborts' (470) panicked at src/class.rs:2542:5:
thread 'class::tests::release_into_an_uncommitted_slab_of_the_group_aborts' (492) panicked at src/class.rs:2510:5:
thread 'class::tests::release_of_interior_pointer_aborts' (493) panicked at src/class.rs:2536:5:
thread 'class::tests::release_outside_region_aborts' (494) panicked at src/class.rs:2536:5:
thread 'class::tests::release_through_the_wrong_arena_aborts' (496) panicked at src/class.rs:2524:5:
thread 'config::linux_tests::load_with_a_clean_environment_gives_the_defaults' (504) panicked at src/config.rs:1771:9:
thread 'heap::tests::a_cached_pair_kept_by_the_split_is_caught_when_released' (598) panicked at src/class.rs:2542:5:
thread 'heap::tests::a_refill_of_a_cache_that_is_not_empty_aborts' (618) panicked at src/heap.rs:2559:13:
thread 'heap::tests::a_scheduled_pop_aborts_when_the_run_reaches_past_the_length' (619) panicked at src/heap.rs:1993:13:
thread 'heap::tests::an_others_node_for_the_home_arena_aborts' (622) panicked at src/heap.rs:2453:9:
thread 'heap::tests::cached_double_free_aborts_at_the_flush' (625) panicked at src/class.rs:2542:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (635) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (635) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (635) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (635) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (635) panicked at src/class.rs:2542:5:
thread 'heap::tests::flush_aborts_on_a_cached_pair_straddling_the_split' (641) panicked at src/heap.rs:3898:5:
thread 'heap::tests::free_into_an_uncommitted_slab_aborts' (644) panicked at src/class.rs:2464:5:
thread 'heap::tests::free_of_a_pointer_outside_every_region_aborts' (647) panicked at src/heap.rs:3787:5:
thread 'heap::tests::free_of_a_misaligned_pointer_aborts' (645) panicked at src/heap.rs:3787:5:
thread 'heap::tests::free_of_an_unallocated_huge_pointer_aborts' (648) panicked at src/huge.rs:1347:5:
thread 'heap::tests::freeing_caches_that_were_not_flushed_aborts' (649) panicked at src/heap.rs:1573:17:
thread 'heap::tests::free_of_a_never_allocated_slot_aborts' (646) panicked at src/class.rs:2516:5:
thread 'heap::tests::sized_free_past_the_watermark_aborts_at_the_flush' (666) panicked at src/class.rs:2464:5:
```

## ab: the Phase 0 set, larson64 and the Valkey model rows, challoc against jemalloc (5 rounds, one run)
-- ab: make ab VARIANTS=default=tree percpu=tree:percpu_arena:percpu jemalloc BENCHES=larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free ROUNDS=5 RESULT=bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ab.md TITLE=per-CPU campaign (percpu) HOST_KIND=GCE aarch64 lane VM (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 1307 s; cgroup memory.peak max 617.9 MiB over 1 container cgroup(s)
- variants: default=tree percpu=tree:percpu_arena:percpu jemalloc; rows: larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free; table: `2026-09-20-aarch64-lane-d-percpu-percpu-ab.md` (the Medians section below)

## Medians

| program | unit | default | default RSS MiB | percpu | percpu RSS MiB | jemalloc | jemalloc RSS MiB | percpu vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 41.8 | 16.1 | 39.0 | 16.4 | 45.1 | 18.9 | +7.2% | -7.3% |
| larson10 | M ops/s | 364.6 | 112.4 | 326.3 | 121.9 | 387.7 | 131.9 | +11.7% | -6.0% |
| larson16 | M ops/s | 588.2 | 166.4 | 521.0 | 167.8 | 612.4 | 206.4 | +12.9% | -4.0% |
| larson64 | M ops/s | 587.2 | 561.0 | 486.1 | 317.0 | 614.5 | 527.0 | +20.8% | -4.4% |
| xmalloc | M frees/s | 576.838 | 53.5 | 488.121 | 78.5 | 477.874 | 78.0 | +18.2% | +20.7% |
| alloc-test1 | s | 2.618 | 13.0 | 2.766 | 13.0 | 2.462 | 12.3 | +5.7% | -6.0% |
| alloc-testN | s | 2.290 | 16.5 | 2.414 | 16.5 | 2.206 | 16.5 | +5.4% | -3.7% |
| cfrac | s | 4.461 | 2.8 | 4.792 | 2.8 | 4.423 | 4.7 | +7.4% | -0.9% |
| espresso | s | 3.654 | 3.7 | 3.776 | 3.7 | 3.743 | 6.2 | +3.3% | +2.4% |
| mstress | s | 0.373 | 320.5 | 0.379 | 319.9 | 0.642 | 291.7 | +1.6% | +72.1% |
| glibc-simple | s | 2.179 | 2.4 | 2.544 | 2.4 | 2.526 | 4.4 | +16.8% | +15.9% |
| redis | k req/s | 499.8 | 29.2 | 491.6 | 29.2 | 411.0 | 31.1 | +1.7% | +21.6% |
| valkey1 | M ops/s | 7.611 | 584.6 | 7.218 | 584.6 | 7.705 | 585.7 | +5.4% | -1.2% |
| valkey1-free | M ops/s | 7.226 | 584.6 | 6.606 | 584.6 | 7.182 | 585.6 | +9.4% | +0.6% |

Positive percentages mean the variant did worse than default (slower or lower throughput).

## calloc: the calloc-zero campaign (`check_zero:true` under the api:: load; 100 x stress 8 50 10 per configuration, at most 300 s each, then 10 x stress 16 100 20 standalone)
-- calloc: make run CMD=ROUNDS=100 MAX_SECS=300 STANDALONE=10 SKIP_SUITE=1 CONFIGS="percpu:check_zero:true,percpu_arena:percpu default:check_zero:true" sh bench/probes/calloc-zero-campaign.sh (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 83 s; cgroup memory.peak max 3247.0 MiB over 1 container cgroup(s)

```
== calloc-zero campaign 2026-09-20T05:13:52Z arch aarch64 ROUNDS=100 MAX_SECS=300 STANDALONE=10
== step 2: full suite
suite: skipped (SKIP_SUITE)
probe: kernel 7.0: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
== step 3: stress 8 50 10 under the api:: load, 100 rounds per configuration
RESULT percpu: 100 ok, 0 failed, 100 runs of stress 8 50 10, 27 s, conf=check_zero:true,percpu_arena:percpu, load runs so far 2
RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 27 s, conf=check_zero:true, load runs so far 4
RESULT load: 5 runs of cargo test api::, 5 failed (1 test result: FAILED. 115 passed; 2 failed; 0 ignored; 0 measured; 400 filtered out; finished in 12.95s;1 test result: FAILED. 115 passed; 2 failed; 0 ignored; 0 measured; 400 filtered out; finished in 12.98s;1 test result: FAILED. 115 passed; 2 failed; 0 ignored; 0 measured; 400 filtered out; finished in 13.36s;1 test result: FAILED. 115 passed; 2 failed; 0 ignored; 0 measured; 400 filtered out; finished in 13.42s;1 test result: FAILED. 115 passed; 2 failed; 0 ignored; 0 measured; 400 filtered out; finished in 13.53s;)
== step 4: stress 16 100 20 standalone, 10 runs, check_zero:true
RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 5 s
== done 2026-09-20T05:15:12Z load: 22.21 12.99 8.36
```

## cgroup: the container tests under CHALLOC_CONF=percpu_arena:percpu (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation 300 s at 512 MiB)
-- cgroup-churn: make test-cgroup-churn (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 12 s; cgroup memory.peak max 406.6 MiB over 4 container cgroup(s)
-- cgroup-lazy: make test-cgroup-lazy (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 8 s; cgroup memory.peak max 204.9 MiB over 2 container cgroup(s)
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=300 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 304 s; cgroup memory.peak max 502.5 MiB over 1 container cgroup(s)

```
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 512 MiB decay_ms 6823 retention_cap 25 MiB empty_bytes 0 MiB full_purges 0 purge_advice eager lazy_purges 0 eager_purges 0
challoc: stats.cgroup.limit 512 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 9 MiB full_purges 0 purge_advice lazy lazy_purges 30 eager_purges 5
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=55 current_peak=406 current_2s=18
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=317 rss_min=317 below_ms=-1 current_peak=406 current_2s=320
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 256 MiB decay_ms 7579 retention_cap 12 MiB empty_bytes 0 MiB full_purges 0 purge_advice eager lazy_purges 0 eager_purges 0
challoc: stats.cgroup.limit 256 MiB decay_ms 10000 retention_cap 12 MiB empty_bytes 4 MiB full_purges 0 purge_advice lazy lazy_purges 28 eager_purges 5
RESULT churn mode=purge mib=200 threshold=50 rss_peak=202 rss_2s=9 rss_min=9 below_ms=81 current_peak=204 current_2s=11
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=200 threshold=50 rss_peak=202 rss_2s=161 rss_min=161 below_ms=-1 current_peak=205 current_2s=163
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=lazy mib=200 rss_peak=202 rss_2s=202 rss_min=202 current_peak=204 current_2s=204 event_ms=82 lazy_purges=35 eager_purges=0 advice_2s=lazy pglazyfree_pages=48571 alloc_ms=58 reuse_ms=5 pressure_ms=105 pglazyfreed_pages=15398 current_pressure=511 reclaim_on_request=0
memory.events after: low 0 high 0 max 280 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=eager mib=200 rss_peak=202 rss_2s=12 rss_min=12 current_peak=204 current_2s=14 event_ms=81 lazy_purges=0 eager_purges=35 advice_2s=eager pglazyfree_pages=0 alloc_ms=58 reuse_ms=46 pressure_ms=92 pglazyfreed_pages=0 current_pressure=398 reclaim_on_request=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
challoc: stats.cgroup.limit 512 MiB current 293 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 20 MiB full_purges 0 pressure_events 0
RESULT oscillate cycles=98 seconds=301 low=200 high=480 current_peak=502 rss_peak=501 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
```

## valkey-suite: `make valkey STAGE=memefficiency` under CHALLOC_CONF=percpu_arena:percpu
-- valkey-memefficiency: make valkey STAGE=memefficiency (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 438 s; cgroup memory.peak max 1170.4 MiB over 1 container cgroup(s)

```
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=a5266dd2ce50b7a7
[ok]: Memory efficiency with values in range 32 (129 ms)
[ok]: Memory efficiency with values in range 64 (130 ms)
[ok]: Memory efficiency with values in range 128 (132 ms)
[ok]: Memory efficiency with values in range 1024 (146 ms)
[ok]: Memory efficiency with values in range 16384 (343 ms)
[ok]: Active defrag - AOF loading (1624 ms)
[ok]: Active Defrag main dictionary: standalone (20616 ms)
[ok]: Active Defrag eval scripts: standalone (6056 ms)
[ok]: Active Defrag big hash: standalone (20283 ms)
[ok]: Active Defrag big list: standalone (19494 ms)
[ok]: Active Defrag big set: standalone (20674 ms)
[ok]: Active Defrag big zset: standalone random-score (21236 ms)
[ok]: Active Defrag big zset: standalone fixed-score (21225 ms)
[ok]: Active Defrag stream: standalone (20533 ms)
[ok]: Active Defrag pubsub: standalone (49750 ms)
[ok]: Setup start_cluster (1911 ms)
[ok]: Active Defrag main dictionary: cluster (19005 ms)
[ok]: Setup start_cluster (1910 ms)
[ok]: Active Defrag eval scripts: cluster (6148 ms)
[ok]: Setup start_cluster (1910 ms)
[ok]: Active Defrag big hash: cluster (20264 ms)
[ok]: Setup start_cluster (1910 ms)
[ok]: Active Defrag big list: cluster (19486 ms)
[ok]: Setup start_cluster (1910 ms)
[ok]: Active Defrag big set: cluster (20756 ms)
[ok]: Setup start_cluster (1911 ms)
[ok]: Active Defrag big zset: cluster random-score (21215 ms)
[ok]: Setup start_cluster (1911 ms)
[ok]: Active Defrag big zset: cluster fixed-score (21228 ms)
[ok]: Setup start_cluster (1911 ms)
[ok]: Active Defrag stream: cluster (20535 ms)
[ok]: Setup start_cluster (1911 ms)
[ok]: Active Defrag pubsub: cluster (49781 ms)
\o/ All tests passed without errors!
```

## valkey-soak: `make valkey-soak` (challoc and bundled jemalloc, 1200 s of load each, the Phase 1 protocol, challoc under CHALLOC_CONF=percpu_arena:percpu; the jemalloc soak saved as the cached baseline)
-- valkey-soak: make valkey-soak DURATION=1200 RESULT_DIR=bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-soak SAVE_BASELINE=1 COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 2644 s; cgroup memory.peak max 1499.6 MiB over 1 container cgroup(s)
- summary: `2026-09-20-aarch64-lane-d-percpu-percpu-soak/2026-09-20-aarch64-valkey-soak.md` (CSVs beside it)

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 920471 | 908954 | -1.3% |
| write: valkey-benchmark p99 ms | 1.071 | 1.079 | -0.7% |
| write: memtier req/s | 924892 | 953954 | +3.1% |
| write: memtier p99 ms | 1.10300 | 0.96700 | +14.1% |
| delete: valkey-benchmark req/s | 1389863 | 1453507 | +4.6% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% |
| delete: memtier req/s | 1153875 | 1281876 | +11.1% |
| delete: memtier p99 ms | 0.96700 | 0.74300 | +30.1% |
| peak RSS MiB (VmHWM) | 575.2 | 574.1 | +0.2% |
| cgroup memory.peak MiB, write phase | 915.4 | 912.1 | +0.4% |
| cgroup memory.peak MiB, delete phase | 895.6 | 893.5 | +0.2% |
| cgroup memory.peak MiB, settle phase | 452.2 | 448.8 | +0.8% |
| peak sampled used_memory_rss MiB | 575.1 | 574.1 | +0.2% |
| RSS MiB at end of write phase | 575.1 | 573.7 | +0.2% |
| RSS MiB at end of delete phase | 131.8 | 129.9 | +1.5% |
| RSS MiB after settle | 131.8 | 129.9 | +1.5% |
| allocator_frag_ratio after settle | 1.78 | 1.81 | -1.7% |
| active_defrag_hits | 248404 | 242153 | -2.5% |
| keys after settle | 108899 | 103085 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-soak/2026-09-20-aarch64-valkey-soak-challoc.csv` (132 rows)
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-soak/2026-09-20-aarch64-valkey-soak-jemalloc.csv` (131 rows)


## ruby-bench: the Ruby rows, challoc against jemalloc (5 rounds), challoc under CHALLOC_CONF=percpu_arena:percpu
-- ruby-bench: make ruby-bench ALLOCS=challoc jemalloc ROUNDS=5 RESULT=bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby.md HOST_KIND=GCE aarch64 lane VM (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 218 s; cgroup memory.peak max 522.6 MiB over 1 container cgroup(s)
- table: `2026-09-20-aarch64-lane-d-percpu-percpu-ruby.md`

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


## ruby-test: `make ruby STAGE=test` (Ruby's test-all on the --with-jemalloc build) under CHALLOC_CONF=percpu_arena:percpu
-- ruby-test: make ruby STAGE=test (CHALLOC_CONF=percpu_arena:percpu): rc=2 in 386 s; cgroup memory.peak max 2274.4 MiB over 1 container cgroup(s)

```
== challoc as libjemalloc.so.2 -> /src/target/ruby/jemalloc-compat/stage/lib
== ruby v3_4_10 -> /src/target/ruby/src-v3_4_10
== make -j16 (challoc; log: /src/target/ruby/logs/build-challoc-aarch64.log)
== check (challoc)
== make test-all TESTOPTS='-j16 -q --tty=no'  (challoc; log: /src/target/ruby/logs/test-all-challoc-aarch64.log)
32858 tests, 6587762 assertions, 7 failures, 2 errors, 409 skips
== test-all (challoc, aarch64): rc=2, 384 s
== 32858 tests, 6587762 assertions, 7 failures, 2 errors, 409 skips
```

## ruby-soak: `make ruby-soak` 16 workers, 600 s, challoc against jemalloc, challoc under CHALLOC_CONF=percpu_arena:percpu
-- ruby-soak: make ruby-soak ALLOCS=challoc jemalloc DURATION=600 RESULT=bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak.md COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 1209 s; cgroup memory.peak max 224.8 MiB over 1 container cgroup(s)
- table: `2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak.md`; arenas from the report: Arenas: 16 (64 automatic), explicit caches: 0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 187.8 | 199.0 | -5.6% |
| VmHWM at the end | 191.7 | 202.9 | -5.5% |
| cgroup memory.peak, start..compact | 119.9 | 137.2 | -12.6% |
| cgroup memory.peak, compact..end | 200.3 | 208.7 | -4.0% |
| at minute 1 (t=60) | 70.7 | 98.6 | -28.3% |
| at minute 5, before GC.compact (t=295) | 105.4 | 130.0 | -18.9% |
| at minute 5, after GC.compact (t=300) | 110.2 | 130.6 | -15.6% |
| at minute 10 (t=600) | 186.8 | 169.0 | +10.5% |
| after the last full GC (final) | 186.8 | 169.0 | +10.5% |
| lowest right after a periodic full GC | 66.6 | 91.8 | -27.5% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 37.2 | 70.1 | 0.0 | 41.0 | 108.6 | 30.8 |
| 30 | fullgc | 66.6 | 93.8 | 11.8 | 91.8 | 133.4 | 82.9 |
| 60 | fullgc | 70.7 | 97.2 | 14.1 | 98.6 | 136.4 | 87.0 |
| 90 | fullgc | 77.3 | 101.2 | 16.1 | 104.4 | 141.6 | 81.8 |
| 120 | fullgc | 78.9 | 102.9 | 19.1 | 102.8 | 139.2 | 96.2 |
| 150 | fullgc | 83.0 | 109.4 | 20.7 | 113.6 | 151.9 | 87.4 |
| 180 | fullgc | 90.8 | 116.4 | 18.9 | 123.1 | 158.9 | 96.4 |
| 210 | fullgc | 94.4 | 126.1 | 20.3 | 123.8 | 158.4 | 101.0 |
| 240 | fullgc | 99.8 | 132.7 | 20.3 | 126.4 | 162.2 | 121.1 |
| 270 | fullgc | 102.0 | 132.8 | 23.3 | 129.4 | 161.6 | 125.8 |
| 300 | compact | 110.2 | 140.0 | 19.8 | 130.6 | 160.4 | 126.9 |
| 330 | fullgc | 136.0 | 167.9 | 29.2 | 147.0 | 172.2 | 115.1 |
| 360 | fullgc | 153.6 | 183.6 | 26.5 | 184.2 | 213.7 | 73.8 |
| 390 | fullgc | 157.4 | 190.2 | 35.8 | 163.0 | 187.3 | 110.8 |
| 420 | fullgc | 163.6 | 192.6 | 39.5 | 174.5 | 197.6 | 100.5 |
| 450 | fullgc | 167.4 | 196.1 | 42.7 | 183.7 | 207.0 | 91.1 |
| 480 | fullgc | 169.3 | 196.4 | 48.5 | 181.9 | 201.4 | 96.8 |
| 510 | fullgc | 174.8 | 201.4 | 47.0 | 190.1 | 207.1 | 91.1 |
| 540 | fullgc | 178.2 | 203.3 | 48.6 | 187.9 | 203.6 | 94.7 |
| 570 | fullgc | 182.4 | 206.2 | 50.7 | 191.7 | 202.2 | 96.1 |
| 600 | fullgc | 186.8 | 211.6 | 49.8 | 169.0 | 176.7 | 121.7 |
| 600 | final-fullgc | 186.8 | 211.6 | 49.8 | 169.0 | 176.7 | 121.7 |

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-challoc-mstats.txt`; summary line: `jobs=138656 live_objects=69342 gc_count=16528 major=151 minor=16377 heap_pages=330 rss_final_kb=191296 hwm_kb=196328 mallctl=yes alloc_allocated=101579280 alloc_active=158343168 alloc_resident=221966336 alloc_retained=52105216 alloc_mapped=274071552 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=122744 cgroup_peak_after_compact_kb=205124`
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=138970 live_objects=69496 gc_count=16478 major=145 minor=16333 heap_pages=326 rss_final_kb=173016 hwm_kb=207800 mallctl=yes alloc_allocated=95118784 alloc_active=146407424 alloc_resident=185270272 alloc_retained=127598592 alloc_mapped=217382912 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=140448 cgroup_peak_after_compact_kb=213736`

## ruby-idle: `make ruby-soak` 16 workers + 48 mostly idle threads on 16 CPUs, 300 s, challoc under CHALLOC_CONF=percpu_arena:percpu (RSS bounded by cores, not threads)
-- ruby-idle: make ruby-soak ALLOCS=challoc jemalloc THREADS=16 IDLE_THREADS=48 DURATION=300 COMPACT_AT=150 RESULT=bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle.md COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 611 s; cgroup memory.peak max 219.7 MiB over 1 container cgroup(s)
- table: `2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle.md`; arenas from the report: Arenas: 16 (64 automatic), explicit caches: 0; summary lines: threads=16 idle_threads=48 idle_bursts=2070 nproc=16;threads=16 idle_threads=48 idle_bursts=2072 nproc=16;

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 112.0 | 191.2 | -41.4% |
| VmHWM at the end | 112.7 | 191.8 | -41.2% |
| cgroup memory.peak, start..compact | 93.4 | 186.9 | -50.0% |
| cgroup memory.peak, compact..end | 120.4 | 197.7 | -39.1% |
| at minute 1 (t=60) | 72.7 | 166.1 | -56.2% |
| at minute 5, before GC.compact (t=145) | 85.2 | 177.8 | -52.1% |
| at minute 5, after GC.compact (t=150) | 86.1 | 178.9 | -51.9% |
| at minute 10 (t=300) | 111.3 | 179.7 | -38.1% |
| after the last full GC (final) | 111.3 | 127.0 | -12.4% |
| lowest right after a periodic full GC | 67.8 | 161.4 | -58.0% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 52.9 | 132.4 | 0.0 | 105.2 | 329.1 | 106.3 |
| 30 | fullgc | 67.8 | 142.3 | 14.1 | 161.4 | 344.5 | 154.9 |
| 60 | fullgc | 72.7 | 146.9 | 15.9 | 166.1 | 342.8 | 167.2 |
| 90 | fullgc | 78.9 | 149.7 | 20.1 | 173.4 | 350.9 | 162.5 |
| 120 | fullgc | 80.7 | 152.1 | 23.4 | 173.9 | 344.7 | 172.8 |
| 150 | compact | 86.1 | 159.8 | 22.8 | 178.9 | 346.3 | 175.1 |
| 180 | fullgc | 94.1 | 167.6 | 20.1 | 185.1 | 349.9 | 203.6 |
| 210 | fullgc | 98.0 | 176.6 | 21.3 | 184.6 | 350.6 | 214.8 |
| 240 | fullgc | 101.4 | 181.5 | 20.9 | 187.9 | 354.6 | 222.9 |
| 270 | fullgc | 105.9 | 183.6 | 23.1 | 188.8 | 348.9 | 228.6 |
| 300 | fullgc | 111.3 | 187.1 | 24.0 | 179.7 | 339.3 | 238.1 |
| 300 | final-fullgc | 111.3 | 187.1 | 24.0 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-challoc-mstats.txt`; summary line: `jobs=69044 live_objects=34536 gc_count=13681 major=31 minor=13650 heap_pages=162 rss_final_kb=114008 hwm_kb=115372 mallctl=yes alloc_allocated=115653712 alloc_active=155766784 alloc_resident=196308992 alloc_retained=25067520 alloc_mapped=221376512 compacted=true threads=16 idle_threads=48 idle_bursts=2072 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=95604 cgroup_peak_after_compact_kb=123248`
- jemalloc: `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-20-aarch64-lane-d-percpu-percpu-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=68995 live_objects=34508 gc_count=13824 major=29 minor=13795 heap_pages=162 rss_final_kb=130016 hwm_kb=196440 mallctl=yes alloc_allocated=109506144 alloc_active=207159296 alloc_resident=232587264 alloc_retained=372899840 alloc_mapped=357433344 compacted=true threads=16 idle_threads=48 idle_bursts=2070 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=191388 cgroup_peak_after_compact_kb=202444`

## done: 7258 s, load at end 1.00 1.00 1.06
