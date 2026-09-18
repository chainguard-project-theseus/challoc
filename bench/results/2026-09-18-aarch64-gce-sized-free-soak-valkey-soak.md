# challoc Valkey soak, 2026-09-18, aarch64

- commit: 27de567 (the soak ran on the working tree of iteration 24 before the code was committed; its `src/` is exactly what 27de567 committed, library hash a84da39114482df0 as in the quick/A/B `new` builds; HEAD at run time was c258815)
- host: Neoverse-V2, 16 CPUs visible to the container
- kernel: 7.0.0-1011-gcp, container: Wolfi
- challoc server: Valkey server v=9.1.2 sha=7f1dffed:1 malloc=jemalloc-5.3.0-challoc bits=64 build=29c2469f316c2d7b
- jemalloc server: Valkey server v=9.1.2 sha=7f1dffed:0 malloc=jemalloc-5.3.0 bits=64 build=c645b848879441b2
- clients: valkey-benchmark from the challoc tree, memtier_benchmark v=2.5.1 sha=5f634d17:0 bits=64 libevent=2.1.13-stable openssl=OpenSSL 3.6.4 25 Aug 2026 prometheus=yes
- workload: write phase 600 s (valkey-benchmark -t set -d 512 -r 500000 -n 20000000 -c 50 -P 16, then memtier SET-only 16-1024 B over 500000 keys, 4 x 10 clients, -P 16, 30 s), delete phase 600 s (valkey-benchmark del key:__rand_int__, then memtier DEL:SET 4:1), settle 60 s idle; activedefrag yes; INFO sampled every 10 s; memtier key pattern: stock (memtier's fixed-seed key stream, the acceptance protocol)
- throughput is the mean requests/s over a tool's runs in a phase; p99 is the median of the per-run p99 (ms, client-side); peak RSS is the server's VmHWM; the RSS columns are used_memory_rss (MiB) at the last sample of each phase

## Summary

| measure | challoc | jemalloc | challoc vs jemalloc |
|---|---:|---:|---:|
| write: valkey-benchmark req/s | 861831 | 860585 | -0.1% |
| write: valkey-benchmark p99 ms | 1.127 | 1.127 | +0.0% |
| write: memtier req/s | 915042 | 908176 | -0.8% |
| write: memtier p99 ms | 1.00700 | 1.02300 | -1.6% |
| delete: valkey-benchmark req/s | 1383823 | 1436061 | +3.8% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% |
| delete: memtier req/s | 1206980 | 1250917 | +3.6% |
| delete: memtier p99 ms | 0.78300 | 0.75900 | +3.2% |
| peak RSS MiB (VmHWM) | 578.0 | 574.5 | +0.6% |
| peak sampled used_memory_rss MiB | 578.0 | 574.5 | +0.6% |
| RSS MiB at end of write phase | 577.7 | 574.1 | +0.6% |
| RSS MiB at end of delete phase | 127.7 | 130.5 | -2.1% |
| RSS MiB after settle | 127.7 | 130.5 | -2.1% |
| allocator_frag_ratio after settle | 1.77 | 1.80 | -1.7% |
| active_defrag_hits | 229340 | 235153 | +2.5% |
| keys after settle | 103768 | 102066 | - |

Positive percentages mean challoc did worse than jemalloc (lower throughput, higher p99, more RSS). Acceptance (docs/plan.md Phase 1): throughput and p99 at least equal, peak RSS at most +5%, RSS after settle within 10%.

## Samples

- challoc: `/src/bench/results/2026-09-18-aarch64-valkey-soak-challoc.csv` (129 rows)
- jemalloc: `/src/bench/results/2026-09-18-aarch64-valkey-soak-jemalloc.csv` (129 rows)

## Generator runs

### challoc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,874776,1.151
write,memtier,set,23,53,30,942160,0.99100
write,valkey-benchmark,set,53,76,23,880320,1.119
write,memtier,set,76,106,30,898268,1.30300
write,valkey-benchmark,set,106,134,28,703928,1.535
write,memtier,set,134,164,30,759160,1.39100
write,valkey-benchmark,set,164,187,23,879160,1.127
write,memtier,set,187,217,30,931758,0.99900
write,valkey-benchmark,set,217,240,23,876424,1.127
write,memtier,set,240,270,30,925572,1.00700
write,valkey-benchmark,set,270,293,23,872372,1.135
write,memtier,set,293,323,30,929337,1.00700
write,valkey-benchmark,set,323,346,23,880592,1.119
write,memtier,set,346,376,30,932827,1.00700
write,valkey-benchmark,set,376,399,23,879740,1.119
write,memtier,set,399,429,30,932460,0.99900
write,valkey-benchmark,set,429,451,22,871878,1.135
write,memtier,set,452,482,30,941530,0.99100
write,valkey-benchmark,set,482,504,22,871954,1.135
write,memtier,set,504,534,30,927609,1.00700
write,valkey-benchmark,set,534,557,23,873134,1.135
write,memtier,set,557,587,30,922928,1.01500
write,valkey-benchmark,set,587,610,23,877693,1.119
write,memtier,set,610,640,30,936897,0.99900
delete,valkey-benchmark,del,640,655,15,1360729,1.143
delete,memtier,del:set=4:1,655,685,30,1224422,0.80700
delete,valkey-benchmark,del,685,699,14,1404593,0.727
delete,memtier,del:set=4:1,699,729,30,1000830,1.04700
delete,valkey-benchmark,del,729,747,18,1148369,0.951
delete,memtier,del:set=4:1,747,777,30,1125306,0.99100
delete,valkey-benchmark,del,777,791,14,1396453,0.727
delete,memtier,del:set=4:1,791,821,30,1228404,0.79100
delete,valkey-benchmark,del,821,835,14,1420454,0.711
delete,memtier,del:set=4:1,835,865,30,1232813,0.78300
delete,valkey-benchmark,del,865,879,14,1403607,0.727
delete,memtier,del:set=4:1,879,909,30,1229150,0.78300
delete,valkey-benchmark,del,909,924,15,1412230,0.719
delete,memtier,del:set=4:1,924,954,30,1224607,0.79100
delete,valkey-benchmark,del,954,968,14,1400560,0.727
delete,memtier,del:set=4:1,968,998,30,1235114,0.77500
delete,valkey-benchmark,del,998,1012,14,1411931,0.719
delete,memtier,del:set=4:1,1012,1042,30,1250760,0.76700
delete,valkey-benchmark,del,1042,1056,14,1405284,0.727
delete,memtier,del:set=4:1,1056,1086,30,1232961,0.78300
delete,valkey-benchmark,del,1086,1101,15,1407559,0.719
delete,memtier,del:set=4:1,1101,1131,30,1240021,0.78300
delete,valkey-benchmark,del,1131,1145,14,1396063,0.727
delete,memtier,del:set=4:1,1145,1175,30,1228710,0.79100
delete,valkey-benchmark,del,1175,1189,14,1421868,0.711
delete,memtier,del:set=4:1,1189,1219,30,1237637,0.78300
```

### jemalloc

```
phase,tool,test,start_s,end_s,seconds,rps,p99_ms
write,valkey-benchmark,set,0,23,23,878349,1.135
write,memtier,set,23,53,30,949875,1.00700
write,valkey-benchmark,set,53,76,23,881601,1.127
write,memtier,set,76,106,30,892040,1.29500
write,valkey-benchmark,set,106,134,28,695749,1.543
write,memtier,set,134,164,30,770798,1.37500
write,valkey-benchmark,set,164,187,23,878580,1.127
write,memtier,set,187,217,30,926596,1.02300
write,valkey-benchmark,set,217,240,23,875695,1.127
write,memtier,set,240,270,30,928071,1.00700
write,valkey-benchmark,set,270,293,23,876655,1.127
write,memtier,set,293,323,30,929488,1.00700
write,valkey-benchmark,set,323,346,23,877039,1.119
write,memtier,set,346,376,30,922187,1.02300
write,valkey-benchmark,set,376,399,23,873515,1.135
write,memtier,set,399,429,30,919120,1.03100
write,valkey-benchmark,set,429,451,22,876847,1.127
write,memtier,set,451,481,30,910966,1.04700
write,valkey-benchmark,set,481,504,23,867528,1.143
write,memtier,set,504,534,30,924716,1.01500
write,valkey-benchmark,set,534,557,23,872905,1.135
write,memtier,set,557,587,30,908170,1.04700
write,valkey-benchmark,set,587,610,23,872562,1.135
write,memtier,set,610,640,30,916084,1.03900
delete,valkey-benchmark,del,640,655,15,1388985,1.135
delete,memtier,del:set=4:1,655,685,30,1260036,0.79900
delete,valkey-benchmark,del,685,698,13,1468752,0.695
delete,memtier,del:set=4:1,698,728,30,1047579,1.01500
delete,valkey-benchmark,del,728,745,17,1208459,0.903
delete,memtier,del:set=4:1,745,775,30,1157804,0.96700
delete,valkey-benchmark,del,775,789,14,1470372,0.695
delete,memtier,del:set=4:1,789,819,30,1286066,0.75100
delete,valkey-benchmark,del,819,832,13,1464558,0.695
delete,memtier,del:set=4:1,832,862,30,1272836,0.76700
delete,valkey-benchmark,del,862,876,14,1467890,0.695
delete,memtier,del:set=4:1,876,906,30,1278087,0.75100
delete,valkey-benchmark,del,906,920,14,1450642,0.703
delete,memtier,del:set=4:1,920,950,30,1282526,0.75900
delete,valkey-benchmark,del,950,963,13,1461241,0.695
delete,memtier,del:set=4:1,963,993,30,1285091,0.75900
delete,valkey-benchmark,del,993,1007,14,1466276,0.695
delete,memtier,del:set=4:1,1007,1037,30,1286901,0.75900
delete,valkey-benchmark,del,1037,1051,14,1439470,0.711
delete,memtier,del:set=4:1,1051,1081,30,1290428,0.75900
delete,valkey-benchmark,del,1081,1095,14,1457620,0.703
delete,memtier,del:set=4:1,1095,1125,30,1271234,0.76700
delete,valkey-benchmark,del,1125,1139,14,1459321,0.695
delete,memtier,del:set=4:1,1139,1169,30,1265558,0.76700
delete,valkey-benchmark,del,1169,1182,13,1465202,0.695
delete,memtier,del:set=4:1,1182,1212,30,1277776,0.75900
```

