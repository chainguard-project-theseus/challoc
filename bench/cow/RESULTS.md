# Fork epochs: Valkey BGSAVE results (session measurements)

Not harness-tier: one develop session, amd64 Xeon 6975P-C, 4 vCPU,
15 GiB, kernel 6.18.62, `io-threads 1`, `rdbcompression no`, BGSAVE
stretched to about 60 s with `rdb-key-save-delay`, keys overwritten
uniformly at a constant rate while the child runs (`run.sh`,
`results.jsonl`). Builds: Valkey 9.1.2 + `contrib/valkey/0003` under its
bundled jemalloc 5.3.0 (je); + 0001, 0002, 0003 on challoc main 8dc46f6
(ch); + 0001 to 0004 on challoc 6547f81 of this branch (cow); challoc
quantum8.

## Copy-on-write, MiB (`rdb_last_cow_size`, median of 3, range)

| cell | je | ch | cow | ch / cow |
|---|---|---|---|---|
| 64 B x 10M keys, 1% overwritten | 458 [458-462] | 458 [458-458] | 139 [139-139] | 3.3x |
| 64 B x 10M keys, 10% | 1058 [1058-1062] | 1059 [1059-1060] | 158 [158-158] | **6.7x** |
| 1 KiB x 2M keys, 1% | 167 [166-168] | 147 [146-147] | 52 [52-52] | 2.8x |
| 1 KiB x 2M keys, 10% | 897 [897-898] | 876 [876-876] | 79 [79-79] | **11.1x** |

Peak of parent RSS plus child dirty, MiB (dataset 1059 and 2552):
64 B 10%: je 2152, ch 2142, cow 1328; 1 KiB 10%: je 3489, ch 3453, cow
2890. The remainder at 64 B is the hashtable's bucket arrays, which
Valkey writes in place (about 130 MiB at 10M keys).

## SET throughput, kops/s (valkey-benchmark -c 50 -P 16, n=3)

| | outside the save | inside the save |
|---|---|---|
| ch 64 B | 1149, 1210, 1205 | 381, 382, 369 |
| cow 64 B | 1224, 1205, 1261 | 743, 723, 739 |
| ch 1 KiB | 1007, 893, 1034 | 272, 265, 270 |
| cow 1 KiB | 968, 993, 938 | 558, 562, 551 |

Outside the save no epoch is open; the runs spread by up to 15%
between server instances, so these cannot resolve a 2% difference.

## RSS after the child exits (10% overwritten, `activedefrag yes`), MiB

| | before | after the save | after 120 s | back within 5% |
|---|---|---|---|---|
| ch 64 B | 1080 | 1082 | 1083 | at once |
| cow 64 B | 1078, 1080 | 1170, 1170 | 1071, 1071 | 28 s |
| ch 1 KiB | 2574, 2541 | 2577 | 2577 | at once |
| cow 1 KiB | 2557, 2549 | 2810, 2810 | 2572, 2572 | 16 s |

The epoch's growth (the bytes written during it) drains through the
defragmenter's moves (0004 runs a cycle at active-defrag-cycle-max
while challoc reports a drain), and the emptied slabs decay.
