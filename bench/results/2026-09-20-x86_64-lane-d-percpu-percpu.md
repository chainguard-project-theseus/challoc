# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), 2026-09-20, x86_64, mode `percpu`

- commit: 0c60b1a
- date: 2026-09-20T04:52:11Z
- host: INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz, 16 CPUs; kernel 7.0.0-1011-gcp; docker Docker version 29.1.3
- features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
- mode: percpu, challoc under CHALLOC_CONF=percpu_arena:percpu in every step but ab (there the variant percpu=tree:percpu_arena:percpu carries the key and default=tree is the same run default)
- steps: test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle; ROUNDS=5 SOAK_DURATION=1200 RUBY_SOAK_DURATION=600 IDLE_THREADS=48 CALLOC_ROUNDS=100 CALLOC_MAX_SECS=300 OSC_SECS=300 VALKEY_FULL=1
- driver: bench/percpu-campaign.sh (`make percpu-campaign MODE=percpu`), raw logs .tmp-percpu-percpu/ on the host that ran it
- load at start: 5.67 3.19 1.33

## test: `make test` under CHALLOC_CONF=percpu_arena:percpu
-- test: make test (CHALLOC_CONF=percpu_arena:percpu): rc=2 in 26 s; cgroup memory.peak max 723.8 MiB over 1 container cgroup(s)

```
thread 'bitmap::tests::a_cleared_padding_bit_aborts_the_allocated_count' (405) panicked at src/bitmap.rs:200:9:
thread 'bitmap::tests::a_release_outside_the_sub_word_view_aborts' (406) panicked at src/bitmap.rs:174:9:
thread 'class::tests::a_purge_of_an_empty_slab_the_counts_do_not_know_aborts' (436) panicked at src/class.rs:2550:5:
thread 'class::tests::check_of_a_guard_slab_slot_aborts' (444) panicked at src/class.rs:2501:5:
thread 'class::tests::check_of_handed_out_uncommitted_slab_aborts' (445) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_unclaimed_slot_aborts' (446) panicked at src/class.rs:2516:5:
thread 'class::tests::check_of_uncommitted_slab_aborts' (447) panicked at src/class.rs:2464:5:
thread 'class::tests::check_through_the_wrong_arena_aborts' (448) panicked at src/class.rs:2524:5:
thread 'class::tests::check_zero_aborts_on_a_dirty_purged_slab' (449) panicked at src/class.rs:2595:5:
thread 'class::tests::a_corrupt_owner_word_aborts_the_next_take_group' (430) panicked at src/class.rs:2470:5:
thread 'class::tests::a_plan_over_a_slab_with_live_above_nslots_aborts' (434) panicked at src/class.rs:1836:9:
thread 'class::tests::a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts' (435) panicked at src/class.rs:2550:5:
thread 'class::tests::check_after_release_aborts' (443) panicked at src/class.rs:2516:5:
thread 'class::tests::double_release_aborts' (472) panicked at src/class.rs:2542:5:
thread 'class::tests::duplicate_apart_in_one_slab_group_aborts' (473) panicked at src/class.rs:2542:5:
thread 'class::tests::guard_interval_must_be_a_power_of_two' (478) panicked at src/class.rs:723:9:
thread 'class::tests::duplicate_in_one_batch_aborts' (474) panicked at src/class.rs:2542:5:
thread 'class::tests::release_into_an_uncommitted_slab_of_the_group_aborts' (491) panicked at src/class.rs:2510:5:
thread 'class::tests::release_of_interior_pointer_aborts' (492) panicked at src/class.rs:2536:5:
thread 'class::tests::release_outside_region_aborts' (494) panicked at src/class.rs:2536:5:
thread 'class::tests::release_through_the_wrong_arena_aborts' (496) panicked at src/class.rs:2524:5:
thread 'config::linux_tests::load_with_a_clean_environment_gives_the_defaults' (504) panicked at src/config.rs:1771:9:
thread 'heap::tests::a_cached_pair_kept_by_the_split_is_caught_when_released' (579) panicked at src/class.rs:2542:5:
thread 'heap::tests::a_refill_of_a_cache_that_is_not_empty_aborts' (615) panicked at src/heap.rs:2559:13:
thread 'heap::tests::a_scheduled_pop_aborts_when_the_run_reaches_past_the_length' (616) panicked at src/heap.rs:1993:13:
thread 'heap::tests::an_others_node_for_the_home_arena_aborts' (619) panicked at src/heap.rs:2453:9:
thread 'heap::tests::cached_double_free_aborts_at_the_flush' (622) panicked at src/class.rs:2542:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (632) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (632) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (632) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (632) panicked at src/heap.rs:3812:5:
thread 'heap::tests::double_free_scan_compares_the_top_four_entries' (632) panicked at src/class.rs:2542:5:
thread 'heap::tests::flush_aborts_on_a_cached_pair_straddling_the_split' (638) panicked at src/heap.rs:3898:5:
thread 'heap::tests::free_into_an_uncommitted_slab_aborts' (641) panicked at src/class.rs:2464:5:
thread 'heap::tests::free_of_a_pointer_outside_every_region_aborts' (644) panicked at src/heap.rs:3787:5:
thread 'heap::tests::free_of_a_never_allocated_slot_aborts' (643) panicked at src/class.rs:2516:5:
thread 'heap::tests::free_of_an_unallocated_huge_pointer_aborts' (645) panicked at src/huge.rs:1347:5:
thread 'heap::tests::free_of_a_misaligned_pointer_aborts' (642) panicked at src/heap.rs:3787:5:
thread 'heap::tests::freeing_caches_that_were_not_flushed_aborts' (646) panicked at src/heap.rs:1573:17:
thread 'heap::tests::sized_free_past_the_watermark_aborts_at_the_flush' (662) panicked at src/class.rs:2464:5:
```

## ab: the Phase 0 set, larson64 and the Valkey model rows, challoc against jemalloc (5 rounds, one run)
-- ab: make ab VARIANTS=default=tree percpu=tree:percpu_arena:percpu jemalloc BENCHES=larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free ROUNDS=5 RESULT=bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ab.md TITLE=per-CPU campaign (percpu) HOST_KIND=GCE x86_64 lane VM (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 1368 s; cgroup memory.peak max 611.1 MiB over 1 container cgroup(s)
- variants: default=tree percpu=tree:percpu_arena:percpu jemalloc; rows: larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free; table: `2026-09-20-x86_64-lane-d-percpu-percpu-ab.md` (the Medians section below)

## Medians

| program | unit | default | default RSS MiB | percpu | percpu RSS MiB | jemalloc | jemalloc RSS MiB | percpu vs default | jemalloc vs default |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 45.1 | 17.0 | 43.1 | 17.8 | 44.9 | 18.0 | +4.6% | +0.4% |
| larson10 | M ops/s | 281.2 | 95.6 | 233.3 | 94.2 | 289.9 | 92.6 | +20.5% | -3.0% |
| larson16 | M ops/s | 307.3 | 119.6 | 252.4 | 108.3 | 335.1 | 127.8 | +21.8% | -8.3% |
| larson64 | M ops/s | 339.1 | 474.3 | 261.6 | 269.7 | 372.7 | 414.9 | +29.6% | -9.0% |
| xmalloc | M frees/s | 330.647 | 52.9 | 296.144 | 57.7 | 328.042 | 91.8 | +11.7% | +0.8% |
| alloc-test1 | s | 3.401 | 13.9 | 3.406 | 13.8 | 3.097 | 13.3 | +0.1% | -8.9% |
| alloc-testN | s | 4.563 | 17.3 | 4.761 | 17.4 | 4.132 | 16.9 | +4.3% | -9.4% |
| cfrac | s | 4.180 | 3.6 | 4.359 | 3.7 | 3.977 | 5.7 | +4.3% | -4.9% |
| espresso | s | 4.744 | 4.1 | 4.742 | 4.2 | 4.661 | 6.3 | -0.0% | -1.7% |
| mstress | s | 0.638 | 337.6 | 0.640 | 316.8 | 1.078 | 284.1 | +0.3% | +69.0% |
| glibc-simple | s | 2.182 | 2.6 | 2.292 | 2.7 | 2.092 | 4.9 | +5.0% | -4.1% |
| redis | k req/s | 637.3 | 30.1 | 623.4 | 30.2 | 507.9 | 32.3 | +2.2% | +25.5% |
| valkey1 | M ops/s | 14.488 | 585.6 | 10.803 | 585.4 | 14.667 | 592.5 | +34.1% | -1.2% |
| valkey1-free | M ops/s | 14.361 | 585.6 | 11.572 | 585.4 | 13.046 | 592.5 | +24.1% | +10.1% |

Positive percentages mean the variant did worse than default (slower or lower throughput).

## calloc: the calloc-zero campaign (`check_zero:true` under the api:: load; 100 x stress 8 50 10 per configuration, at most 300 s each, then 10 x stress 16 100 20 standalone)
-- calloc: make run CMD=ROUNDS=100 MAX_SECS=300 STANDALONE=10 SKIP_SUITE=1 CONFIGS="percpu:check_zero:true,percpu_arena:percpu default:check_zero:true" sh bench/probes/calloc-zero-campaign.sh (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 120 s; cgroup memory.peak max 3100.8 MiB over 1 container cgroup(s)

```
== calloc-zero campaign 2026-09-20T05:15:25Z arch x86_64 ROUNDS=100 MAX_SECS=300 STANDALONE=10
== step 2: full suite
suite: skipped (SKIP_SUITE)
probe: kernel 7.0: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
== step 3: stress 8 50 10 under the api:: load, 100 rounds per configuration
RESULT percpu: 100 ok, 0 failed, 100 runs of stress 8 50 10, 41 s, conf=check_zero:true,percpu_arena:percpu, load runs so far 2
RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 43 s, conf=check_zero:true, load runs so far 4
RESULT load: 5 runs of cargo test api::, 5 failed (1 test result: FAILED. 116 passed; 1 failed; 0 ignored; 0 measured; 400 filtered out; finished in 19.31s;1 test result: FAILED. 116 passed; 1 failed; 0 ignored; 0 measured; 400 filtered out; finished in 20.01s;1 test result: FAILED. 116 passed; 1 failed; 0 ignored; 0 measured; 400 filtered out; finished in 20.24s;1 test result: FAILED. 116 passed; 1 failed; 0 ignored; 0 measured; 400 filtered out; finished in 20.67s;1 test result: FAILED. 116 passed; 1 failed; 0 ignored; 0 measured; 400 filtered out; finished in 20.70s;)
== step 4: stress 16 100 20 standalone, 10 runs, check_zero:true
RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 8 s
== done 2026-09-20T05:17:23Z load: 27.82 15.97 9.74
```

## cgroup: the container tests under CHALLOC_CONF=percpu_arena:percpu (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation 300 s at 512 MiB)
-- cgroup-churn: make test-cgroup-churn (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 12 s; cgroup memory.peak max 406.1 MiB over 4 container cgroup(s)
-- cgroup-lazy: make test-cgroup-lazy (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 8 s; cgroup memory.peak max 204.8 MiB over 2 container cgroup(s)
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=300 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 302 s; cgroup memory.peak max 502.9 MiB over 1 container cgroup(s)

```
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 512 MiB decay_ms 7563 retention_cap 25 MiB empty_bytes 0 MiB full_purges 0 purge_advice eager lazy_purges 0 eager_purges 0
challoc: stats.cgroup.limit 512 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 9 MiB full_purges 0 purge_advice lazy lazy_purges 30 eager_purges 5
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=46 current_peak=405 current_2s=18
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=314 rss_min=314 below_ms=-1 current_peak=404 current_2s=315
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 256 MiB decay_ms 7083 retention_cap 12 MiB empty_bytes 0 MiB full_purges 0 purge_advice eager lazy_purges 0 eager_purges 0
challoc: stats.cgroup.limit 256 MiB decay_ms 10000 retention_cap 12 MiB empty_bytes 4 MiB full_purges 0 purge_advice lazy lazy_purges 28 eager_purges 5
RESULT churn mode=purge mib=200 threshold=50 rss_peak=203 rss_2s=10 rss_min=10 below_ms=81 current_peak=203 current_2s=10
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=200 threshold=50 rss_peak=203 rss_2s=161 rss_min=161 below_ms=-1 current_peak=203 current_2s=162
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=lazy mib=200 rss_peak=203 rss_2s=203 rss_min=203 current_peak=204 current_2s=204 event_ms=80 lazy_purges=35 eager_purges=0 advice_2s=lazy pglazyfree_pages=48571 alloc_ms=109 reuse_ms=44 pressure_ms=172 pglazyfreed_pages=15445 current_pressure=511 reclaim_on_request=0
memory.events after: low 0 high 0 max 281 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=eager mib=200 rss_peak=203 rss_2s=13 rss_min=13 current_peak=203 current_2s=13 event_ms=78 lazy_purges=0 eager_purges=35 advice_2s=eager pglazyfree_pages=0 alloc_ms=99 reuse_ms=80 pressure_ms=162 pglazyfreed_pages=0 current_pressure=397 reclaim_on_request=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
challoc: stats.cgroup.limit 512 MiB current 288 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 23 MiB full_purges 0 pressure_events 0
RESULT oscillate cycles=96 seconds=300 low=200 high=480 current_peak=502 rss_peak=502 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME oscillate alloc=challoc args='300' status=0 verdict=ok oom_kill=0
```

## valkey-suite: `make valkey STAGE=memefficiency` and STAGE=full under CHALLOC_CONF=percpu_arena:percpu
-- valkey-memefficiency: make valkey STAGE=memefficiency (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 457 s; cgroup memory.peak max 852.4 MiB over 1 container cgroup(s)

```
Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=7bf98abf4114532b
[ok]: Memory efficiency with values in range 32 (141 ms)
[ok]: Memory efficiency with values in range 64 (142 ms)
[ok]: Memory efficiency with values in range 128 (141 ms)
[ok]: Memory efficiency with values in range 1024 (155 ms)
[ok]: Memory efficiency with values in range 16384 (356 ms)
[ok]: Active defrag - AOF loading (1633 ms)
[ok]: Active Defrag main dictionary: standalone (21179 ms)
[ok]: Active Defrag eval scripts: standalone (6277 ms)
[ok]: Active Defrag big hash: standalone (21017 ms)
[ok]: Active Defrag big list: standalone (19886 ms)
[ok]: Active Defrag big set: standalone (21930 ms)
[ok]: Active Defrag big zset: standalone random-score (22040 ms)
[ok]: Active Defrag big zset: standalone fixed-score (21969 ms)
[ok]: Active Defrag stream: standalone (21284 ms)
[ok]: Active Defrag pubsub: standalone (51229 ms)
[ok]: Setup start_cluster (1974 ms)
[ok]: Active Defrag main dictionary: cluster (19550 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag eval scripts: cluster (6221 ms)
[ok]: Setup start_cluster (1970 ms)
[ok]: Active Defrag big hash: cluster (21009 ms)
[ok]: Setup start_cluster (1970 ms)
[ok]: Active Defrag big list: cluster (19856 ms)
[ok]: Setup start_cluster (1971 ms)
[ok]: Active Defrag big set: cluster (21899 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag big zset: cluster random-score (21991 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag big zset: cluster fixed-score (21964 ms)
[ok]: Setup start_cluster (1974 ms)
[ok]: Active Defrag stream: cluster (21118 ms)
[ok]: Setup start_cluster (1972 ms)
[ok]: Active Defrag pubsub: cluster (51997 ms)
\o/ All tests passed without errors!
```

-- valkey-full: make valkey STAGE=full (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 967 s; cgroup memory.peak max 2105.8 MiB over 1 container cgroup(s)

```
[ok]: GEODIST simple & unit (1 ms)
\o/ All tests passed without errors!
```

## valkey-soak: `make valkey-soak` (challoc and bundled jemalloc, 1200 s of load each, the Phase 1 protocol, challoc under CHALLOC_CONF=percpu_arena:percpu; the jemalloc soak saved as the cached baseline)
-- valkey-soak: make valkey-soak DURATION=1200 RESULT_DIR=bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-soak SAVE_BASELINE=1 COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 2594 s; cgroup memory.peak max 1131.4 MiB over 1 container cgroup(s)
- summary: `2026-09-20-x86_64-lane-d-percpu-percpu-soak/2026-09-20-x86_64-valkey-soak.md` (CSVs beside it)

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 753753 | 723408 | -4.0% |
| write: valkey-benchmark p99 ms | 1.247 | 1.295 | -3.7% |
| write: memtier req/s | 921696 | 929724 | +0.9% |
| write: memtier p99 ms | 0.99100 | 0.99100 | +0.0% |
| delete: valkey-benchmark req/s | 1335966 | 1377653 | +3.1% |
| delete: valkey-benchmark p99 ms | 0.735 | 0.711 | +3.4% |
| delete: memtier req/s | 1216804 | 1242349 | +2.1% |
| delete: memtier p99 ms | 0.79100 | 0.78300 | +1.0% |
| peak RSS MiB (VmHWM) | 575.8 | 573.9 | +0.3% |
| cgroup memory.peak MiB, write phase | 852.5 | 849.5 | +0.4% |
| cgroup memory.peak MiB, delete phase | 841.6 | 838.9 | +0.3% |
| cgroup memory.peak MiB, settle phase | 390.9 | 394.8 | -1.0% |
| peak sampled used_memory_rss MiB | 575.8 | 573.9 | +0.3% |
| RSS MiB at end of write phase | 575.5 | 573.7 | +0.3% |
| RSS MiB at end of delete phase | 126.2 | 131.2 | -3.8% |
| RSS MiB after settle | 126.2 | 131.2 | -3.8% |
| allocator_frag_ratio after settle | 1.75 | 1.78 | -1.7% |
| active_defrag_hits | 251895 | 269438 | +7.0% |
| keys after settle | 103692 | 103510 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-soak/2026-09-20-x86_64-valkey-soak-challoc.csv` (128 rows)
- jemalloc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-soak/2026-09-20-x86_64-valkey-soak-jemalloc.csv` (129 rows)


## ruby-bench: the Ruby rows, challoc against jemalloc (5 rounds), challoc under CHALLOC_CONF=percpu_arena:percpu
-- ruby-bench: make ruby-bench ALLOCS=challoc jemalloc ROUNDS=5 RESULT=bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby.md HOST_KIND=GCE x86_64 lane VM (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 211 s; cgroup memory.peak max 510.7 MiB over 1 container cgroup(s)
- table: `2026-09-20-x86_64-lane-d-percpu-percpu-ruby.md`

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


## ruby-test: `make ruby STAGE=test` (Ruby's test-all on the --with-jemalloc build) under CHALLOC_CONF=percpu_arena:percpu
-- ruby-test: make ruby STAGE=test (CHALLOC_CONF=percpu_arena:percpu): rc=2 in 62 s; cgroup memory.peak max 2163.0 MiB over 1 container cgroup(s)

```
== challoc as libjemalloc.so.2 -> /src/target/ruby/jemalloc-compat/stage/lib
== ruby v3_4_10 -> /src/target/ruby/src-v3_4_10
== make -j16 (challoc; log: /src/target/ruby/logs/build-challoc-x86_64.log)
== check (challoc)
== make test-all TESTOPTS='-j16 -q --tty=no'  (challoc; log: /src/target/ruby/logs/test-all-challoc-x86_64.log)
32845 tests, 6587987 assertions, 7 failures, 2 errors, 407 skips
== test-all (challoc, x86_64): rc=2, 60 s
== 32845 tests, 6587987 assertions, 7 failures, 2 errors, 407 skips
```

## ruby-soak: `make ruby-soak` 16 workers, 600 s, challoc against jemalloc, challoc under CHALLOC_CONF=percpu_arena:percpu
-- ruby-soak: make ruby-soak ALLOCS=challoc jemalloc DURATION=600 RESULT=bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak.md COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 1211 s; cgroup memory.peak max 218.7 MiB over 1 container cgroup(s)
- table: `2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak.md`; arenas from the report: Arenas: 16 (64 automatic), explicit caches: 0

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 209.9 | 179.1 | +17.2% |
| VmHWM at the end | 213.1 | 183.7 | +16.0% |
| cgroup memory.peak, start..compact | 155.0 | 118.3 | +31.0% |
| cgroup memory.peak, compact..end | 218.7 | 185.1 | +18.2% |
| at minute 1 (t=60) | 93.1 | 73.2 | +27.2% |
| at minute 5, before GC.compact (t=295) | 148.2 | 111.4 | +33.0% |
| at minute 5, after GC.compact (t=300) | 146.9 | 116.5 | +26.1% |
| at minute 10 (t=600) | 207.7 | 140.2 | +48.1% |
| after the last full GC (final) | 207.7 | 140.2 | +48.1% |
| lowest right after a periodic full GC | 91.8 | 71.2 | +28.9% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=300 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 31.2 | 61.8 | 0.0 | 39.0 | 56.5 | 17.3 |
| 30 | fullgc | 99.3 | 124.1 | 14.4 | 71.2 | 78.7 | 33.0 |
| 60 | fullgc | 93.1 | 117.9 | 36.5 | 73.2 | 79.9 | 34.9 |
| 90 | fullgc | 91.8 | 114.0 | 45.8 | 73.2 | 79.0 | 35.8 |
| 120 | fullgc | 107.7 | 131.5 | 34.4 | 89.5 | 93.8 | 57.2 |
| 150 | fullgc | 113.7 | 136.5 | 42.7 | 91.9 | 95.9 | 64.2 |
| 180 | fullgc | 116.4 | 140.3 | 41.8 | 91.1 | 94.2 | 65.9 |
| 210 | fullgc | 111.5 | 138.2 | 50.5 | 91.6 | 94.2 | 66.0 |
| 240 | fullgc | 120.5 | 145.7 | 46.5 | 97.4 | 99.0 | 61.2 |
| 270 | fullgc | 116.8 | 141.8 | 53.5 | 95.6 | 96.2 | 64.0 |
| 300 | compact | 146.9 | 173.0 | 41.2 | 116.5 | 115.8 | 44.5 |
| 330 | fullgc | 194.0 | 217.2 | 37.9 | 145.5 | 142.0 | 75.2 |
| 360 | fullgc | 169.8 | 190.5 | 81.3 | 130.8 | 126.8 | 90.6 |
| 390 | fullgc | 184.3 | 205.0 | 73.7 | 145.5 | 141.3 | 76.1 |
| 420 | fullgc | 182.3 | 202.0 | 85.3 | 154.4 | 149.3 | 68.1 |
| 450 | fullgc | 185.5 | 204.1 | 87.5 | 154.3 | 148.3 | 69.1 |
| 480 | fullgc | 190.2 | 207.2 | 92.5 | 165.1 | 157.9 | 59.6 |
| 510 | fullgc | 195.4 | 211.7 | 90.8 | 168.1 | 160.6 | 56.9 |
| 540 | fullgc | 197.4 | 213.5 | 94.7 | 168.0 | 159.4 | 58.0 |
| 570 | fullgc | 202.9 | 218.2 | 92.5 | 170.9 | 161.8 | 55.7 |
| 600 | fullgc | 207.7 | 223.8 | 90.5 | 140.2 | 129.8 | 87.7 |
| 600 | final-fullgc | 207.7 | 223.8 | 90.5 | 140.2 | 129.8 | 87.7 |

## Samples

- challoc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-challoc.csv` (122 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-challoc-mstats.txt`; summary line: `jobs=147838 live_objects=73930 gc_count=16222 major=1362 minor=14860 heap_pages=342 rss_final_kb=212668 hwm_kb=218216 mallctl=yes alloc_allocated=105330368 alloc_active=159567872 alloc_resident=234766336 alloc_retained=94855168 alloc_mapped=329621504 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=158748 cgroup_peak_after_compact_kb=223984`
- jemalloc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-jemalloc.csv` (122 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-soak-jemalloc-mstats.txt`; summary line: `jobs=145121 live_objects=72570 gc_count=15986 major=1317 minor=14669 heap_pages=342 rss_final_kb=143548 hwm_kb=188068 mallctl=yes alloc_allocated=96348128 alloc_active=109551616 alloc_resident=136126464 alloc_retained=92000256 alloc_mapped=166998016 compacted=true threads=16 idle_threads=0 idle_bursts=0 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=121116 cgroup_peak_after_compact_kb=189532`

## ruby-idle: `make ruby-soak` 16 workers + 48 mostly idle threads on 16 CPUs, 300 s, challoc under CHALLOC_CONF=percpu_arena:percpu (RSS bounded by cores, not threads)
-- ruby-idle: make ruby-soak ALLOCS=challoc jemalloc THREADS=16 IDLE_THREADS=48 DURATION=300 COMPACT_AT=150 RESULT=bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle.md COMMIT=0c60b1a CGROUP_PEAK=1 (CHALLOC_CONF=percpu_arena:percpu): rc=0 in 611 s; cgroup memory.peak max 193.7 MiB over 1 container cgroup(s)
- table: `2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle.md`; arenas from the report: Arenas: 16 (64 automatic), explicit caches: 0; summary lines: threads=16 idle_threads=48 idle_bursts=2084 nproc=16;threads=16 idle_threads=48 idle_bursts=2096 nproc=16;

## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| peak sampled VmRSS | 158.5 | 165.7 | -4.3% |
| VmHWM at the end | 159.0 | 165.8 | -4.1% |
| cgroup memory.peak, start..compact | 123.0 | 154.7 | -20.5% |
| cgroup memory.peak, compact..end | 166.0 | 169.0 | -1.8% |
| at minute 1 (t=60) | 90.7 | 138.5 | -34.5% |
| at minute 5, before GC.compact (t=145) | 116.2 | 149.3 | -22.2% |
| at minute 5, after GC.compact (t=150) | 114.8 | 148.7 | -22.8% |
| at minute 10 (t=300) | 156.7 | 165.7 | -5.4% |
| after the last full GC (final) | 156.7 | 119.5 | +31.1% |
| lowest right after a periodic full GC | 77.1 | 124.4 | -38.0% |

Positive percentages: the first-named allocator is larger.
## RSS over time (MiB; every 30th second is a forced full GC, the row at t=150 is GC.compact)

| t s | event | challoc VmRSS | challoc resident | challoc retained | jemalloc VmRSS | jemalloc resident | jemalloc retained |
|---:|---|---:|---:|---:|---:|---:|---:|
| 0 | start | 47.2 | 124.1 | 0.0 | 101.6 | 181.6 | 57.8 |
| 30 | fullgc | 77.1 | 150.4 | 27.6 | 124.4 | 182.7 | 89.4 |
| 60 | fullgc | 90.7 | 164.4 | 33.8 | 138.5 | 195.1 | 80.2 |
| 90 | fullgc | 96.2 | 168.7 | 37.2 | 137.2 | 193.2 | 82.1 |
| 120 | fullgc | 93.3 | 166.6 | 43.2 | 133.8 | 189.4 | 86.0 |
| 150 | compact | 114.8 | 186.2 | 38.4 | 148.7 | 202.9 | 118.2 |
| 180 | fullgc | 117.2 | 190.1 | 44.1 | 146.8 | 200.9 | 120.3 |
| 210 | fullgc | 121.6 | 197.3 | 45.7 | 138.5 | 191.4 | 129.7 |
| 240 | fullgc | 115.3 | 189.5 | 55.2 | 133.6 | 186.4 | 134.8 |
| 270 | fullgc | 125.7 | 200.4 | 50.7 | 138.0 | 189.7 | 131.5 |
| 300 | fullgc | 156.7 | 233.2 | 39.5 | 165.7 | 214.8 | 110.1 |
| 300 | final-fullgc | 156.7 | 233.2 | 39.5 | 0.0 | - | - |

## Samples

- challoc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-challoc.csv` (62 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-challoc-mstats.txt`; summary line: `jobs=72547 live_objects=36284 gc_count=13519 major=1268 minor=12251 heap_pages=205 rss_final_kb=160496 hwm_kb=162816 mallctl=yes alloc_allocated=120735440 alloc_active=170192896 alloc_resident=244645888 alloc_retained=41377792 alloc_mapped=286023680 compacted=true threads=16 idle_threads=48 idle_bursts=2096 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=125936 cgroup_peak_after_compact_kb=169944`
- jemalloc: `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-jemalloc.csv` (62 rows), allocator report `bench/results/2026-09-20-x86_64-lane-d-percpu-percpu-ruby-idle-jemalloc-mstats.txt`; summary line: `jobs=71108 live_objects=35568 gc_count=13495 major=1282 minor=12213 heap_pages=204 rss_final_kb=122396 hwm_kb=169792 mallctl=yes alloc_allocated=111758208 alloc_active=124821504 alloc_resident=175259648 alloc_retained=165445632 alloc_mapped=304840704 compacted=true threads=16 idle_threads=48 idle_bursts=2084 nproc=16 cgroup_peak=yes cgroup_peak_before_compact_kb=158408 cgroup_peak_after_compact_kb=173040`

## done: 7959 s, load at end 1.04 1.01 1.09
