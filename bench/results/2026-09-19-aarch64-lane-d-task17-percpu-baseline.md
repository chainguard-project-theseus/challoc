# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), 2026-09-19, aarch64, mode `baseline`

- commit: 79e3931
- date: 2026-09-19T23:35:59Z
- host: Neoverse-V2, 16 CPUs; kernel 7.0.0-1011-gcp; docker Docker version 29.1.3
- features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
- mode: baseline
- baseline half: the default configuration alone (the key is accepted and ignored before task 8 lands; nothing here runs under it). The percpu half is the same script with MODE=percpu after the merge.
- steps: cgroup; ROUNDS=5 SOAK_DURATION=1200 RUBY_SOAK_DURATION=600 IDLE_THREADS=48 CALLOC_ROUNDS=100 CALLOC_MAX_SECS=300 OSC_SECS=20 VALKEY_FULL=0
- driver: bench/percpu-campaign.sh (`make percpu-campaign MODE=baseline`), raw logs .tmp-percpu-baseline/ on the host that ran it
- load at start: 1.00 0.99 0.92

## cgroup: the container tests (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation 20 s at 512 MiB)
-- cgroup-churn: make test-cgroup-churn : rc=0 in 10 s; cgroup memory.peak max 406.1 MiB over 4 container cgroup(s)
-- cgroup-lazy: make test-cgroup-lazy : rc=0 in 8 s; cgroup memory.peak max 205.2 MiB over 2 container cgroup(s)
-- cgroup-oscillate: make test-cgroup-oscillate OSC_SECS=20 : rc=0 in 24 s; cgroup memory.peak max 500.6 MiB over 1 container cgroup(s)

```
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 512 MiB decay_ms 7917 retention_cap 25 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 512 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 9 MiB full_purges 0
RESULT churn mode=purge mib=400 threshold=100 rss_peak=403 rss_2s=16 rss_min=16 below_ms=52 current_peak=405 current_2s=17
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=400 threshold=100 rss_peak=403 rss_2s=317 rss_min=317 below_ms=-1 current_peak=405 current_2s=320
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 400 100' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.limit 256 MiB decay_ms 8222 retention_cap 12 MiB empty_bytes 0 MiB full_purges 0
challoc: stats.cgroup.limit 256 MiB decay_ms 10000 retention_cap 12 MiB empty_bytes 4 MiB full_purges 0
RESULT churn mode=purge mib=200 threshold=50 rss_peak=202 rss_2s=9 rss_min=9 below_ms=80 current_peak=205 current_2s=11
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc args='purge 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
challoc: stats.cgroup.* ENOENT (cgroup not tracked)
RESULT churn mode=keep mib=200 threshold=50 rss_peak=202 rss_2s=160 rss_min=160 below_ms=-1 current_peak=204 current_2s=162
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME churn alloc=challoc-off args='keep 200 50' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=lazy mib=200 rss_peak=202 rss_2s=202 rss_min=202 current_peak=204 current_2s=204 event_ms=79 lazy_purges=35 eager_purges=0 advice_2s=lazy pglazyfree_pages=48576 alloc_ms=56 reuse_ms=5 pressure_ms=102 pglazyfreed_pages=17866 current_pressure=511 reclaim_on_request=0
memory.events after: low 0 high 0 max 352 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc args='lazy 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
RESULT lazy mode=eager mib=200 rss_peak=202 rss_2s=12 rss_min=12 current_peak=204 current_2s=14 event_ms=79 lazy_purges=0 eager_purges=35 advice_2s=eager pglazyfree_pages=0 alloc_ms=56 reuse_ms=44 pressure_ms=91 pglazyfreed_pages=0 current_pressure=388 reclaim_on_request=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME lazy alloc=challoc-eager args='eager 200' status=0 verdict=ok oom_kill=0
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
memory.events before: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0
challoc: stats.cgroup.limit 512 MiB current 293 MiB decay_ms 10000 retention_cap 25 MiB empty_bytes 25 MiB full_purges 0 pressure_events 0
RESULT oscillate cycles=7 seconds=21 low=200 high=480 current_peak=500 rss_peak=498 max_before=0 max_after=0 high_before=0 high_after=0 oom_kill_before=0 oom_kill_after=0
memory.events after: low 0 high 0 max 0 oom 0 oom_kill 0 oom_group_kill 0 sock_throttled 0 
OUTCOME oscillate alloc=challoc args='20' status=0 verdict=ok oom_kill=0
```

## done: 50 s, load at end 0.47 0.85 0.88
