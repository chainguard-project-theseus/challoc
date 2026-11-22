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

## cb, local cell (indicative only: one develop session, never a verdict of record)

`python3 bench/cb run cow` (this suite; small dataset: 1M keys, 0.2M at 1 KiB),
base 8dc46f6 vs head, one block: valkey-cow 72.4 -> 18.4 MiB, valkey-cow-d1k
26.5 -> 7.6 MiB, peak -25.8% / -4.5%, SET outside the save 1.21M -> 1.24M,
RSS at the end of the recovery 101.0% -> 98.6% of its pre-save value.

`cb run kv-valkey --rows valkey-ops,valkey-ops-d1k --variants base,head`, 4
paired blocks (both on the runpath build with contrib/valkey 0001 to 0004;
base binds no epoch): valkey-ops +0.2% [-1.3%, +1.6%] same; valkey-ops-d1k
+0.2% [-3.0%, +3.5%] inconclusive.

`cb run quick`, 3 paired blocks: larson1 -0.8% [-7.2%, +5.9%], larson10 -0.6%
[-2.4%, +1.2%], xmalloc -4.2% better, alloc-test1 +0.1% [-0.8%, +1.0%],
valkey1 -3.9% better, valkey1-free -0.3% (wide), churn1024 same; instructions
per op +0.0% to +1.9% (positive percentages are worse for the candidate).

The harness tier (`make quick`, `make bench-launch SUITE=cow`) runs on GCE
from a checkout with the internal cell overlay; these are not those.

## Harness tier (cb launch, gate cells ref-amd64 and ref-arm64, base main)

`cow` (one variant per VM, 3 replicas, head 986f766; challoc base vs head;
run `20261119t0144z-986f766-cow-m3jjz`):

| row | amd64 | arm64 |
|---|---|---|
| valkey-cow (64 B, 10%) | -85.1% (1060 -> 158 MiB), better | -85.1%, better |
| valkey-cow.peak_mib | -37.9%, better | -37.9%, better |
| valkey-cow-d1k (1 KiB, 10%) | -90.9% (876 -> 79 MiB), better | -90.9%, better |
| valkey-cow-d1k.peak_mib | -16.3%, better | -16.3%, better |
| valkey-cow-tput.set_inside_rps | +78% (423k -> 769k), better | +58% (326k -> 514k), better |
| valkey-cow-tput (outside the save) | inconclusive [-20.7%, +13.0%] | inconclusive [-12.2%, +20.0%] |
| valkey-cow.rss_after_mib (right after the save) | +8.1%, worse | +8.1%, worse |
| valkey-cow-rec (RSS 120 s later, activedefrag) | -1.1%, sub-threshold | -1.2%, sub-threshold |
| valkey-cow-rec.recover_s | 24 s | 32 s |

`kv-valkey` valkey-ops and valkey-ops-d1k, paired, head 1ba8461 (run
`20261119t0240z-1ba8461-kv-valkey-3ju3v`): amd64
+0.2% [-0.4%, +0.9%] same and +0.3% [-0.7%, +1.2%] same; arm64 +0.0%
[-0.6%, +0.6%] same and +0.4% [-1.7%, +2.6%] same. (At 986f766, run
`20261119t0202z-986f766-kv-valkey-37gee`: every row within +1.1%.)

`quick`, paired, head 1ba8461 (run `20261119t0229z-1ba8461-quick-xkp4c`): every row same, inconclusive or
sub-threshold (at most +0.8%) except **xmalloc on ref-arm64: +4.5% [+1.3%,
+7.9%], worse** (instr/op +1.0%; at 986f766, run
`20261119t0132z-986f766-quick-2hr7u`, +3.5% [+1.9%, +5.1%]). On
ref-amd64 xmalloc is +0.6%, same. Positive percentages are the candidate
worse.

The xmalloc regression was `Heap::free` inlining the epoch's deflection
lookup (`Shared::cow_deflects`) into libc's `free_slow`: on aarch64 that
took a callee-saved register, saved and restored on every slow free,
xmalloc's cross-thread frees included. 0bba0c1 makes the epoch branch a
cold tail call (`Heap::free_epoch`) behind one load of `cow::in_force`;
`free_slow` on aarch64 then differs from main only by that load and its
branch on the home branch. `quick` re-run, paired, head 0bba0c1 (run
`20261119t0302z-0bba0c1-quick-spv6c`): xmalloc
on ref-arm64 -1.3% [-4.3%, +1.8%] (instr/op -0.0%; was +4.5% worse), on
ref-amd64 +0.1% same; no row worse on either cell (alloc-test1 amd64
+0.7% and valkey1 +1.2..2.4% inconclusive within their margins).

# Redis 8.10.3

`contrib/redis/0005` and `0006`, the Redis counterparts of Valkey's 0003
and 0004, with two Redis-specific additions: 0005 also stops
`decrRefCount` writing the count of an object it is about to free, and
0006 makes `dbSetValue` allocate a new object instead of swapping the new
value into the old one while the epoch is open. Redis's 64-byte values are
raw strings (its embstr limit is 44 bytes), so they take that swap, which
writes the old object; Valkey's 64-byte values are embedded and do not.

## Session measurements

Not harness-tier: one develop session, amd64 Xeon 6975P-C, 8 vCPU, 31
GiB, kernel 6.18.62, one repetition (`run-redis.sh`,
`results-redis.jsonl`), the same cowbench cells as Valkey's. Builds:
stock 8.10.3 under its bundled jemalloc 5.3.0 (je); + 0005 (je5); + 0001
to 0004 on challoc main 053c930, quantum8 (ch); + 0001 to 0006 (cow).

`rdb_last_cow_size`, MiB:

| cell | je | je5 | ch | cow | ch / cow |
|---|---|---|---|---|---|
| 64 B x 10M keys, 10% SET | 1141 | 1141 | 1175 | 235 | **5.0x** |
| 64 B x 10M keys, 10% DEL | 623 | 620 | 651 | 237 | 2.7x |
| 1 KiB x 2M keys, 10% SET | 886 | 886 | 891 | 30 | **29.7x** |

What each patch does at 64 B, challoc (MiB, SET / DEL): none 1175 / 651;
0005 alone (the cow build with `cow_epoch:false`) 1175 / 646; 0006 alone
(0001 to 0004 and 0006) 616 / 619; both 235 / 237. 0005 saves nothing
alone, with either allocator: a freed kvobj's slot is handed out again to
the next command's argument objects, which write the page anyway. With
the epoch, which hands out no block freed during it, it is what halves
the copy again. Most of what remains at 64 B is the main dict's bucket
array (16M buckets, 128 MiB at 10M keys), which every overwrite and
delete writes in place.

Peak of parent RSS plus child dirty, MiB: 64 B SET ch 2582, cow 1751;
1 KiB SET ch 3462, cow 2842. SET throughput during the save
(redis-benchmark -c 50 -P 16): 64 B ch 312k, cow 529k ops/s; 1 KiB ch
262k, cow 539k. RSS after the save, activedefrag on: 64 B 1435 before,
1516 after the save, 1397 120 s later (back within 5% at 60 s); 1 KiB
2574, 2811, 2571 (16 s).

## Harness tier (cb launch, gate cells ref-amd64 and ref-arm64)

`cow --rows redis-cow,redis-cow-d1k,redis-cow-tput,redis-cow-rec
--variants 'noepoch=head@cow_epoch:false,head' --controls jemalloc`, head
1627280 (run `20261122t0045z-1627280-cow-hshnq`, one variant per VM, 3
replicas). Both challoc variants are the same library and the same
runpath build with 0001 to 0006; `noepoch` turns the epoch off, so the
pair is the epoch's effect with 0005 in both:

| row | amd64 | arm64 |
|---|---|---|
| redis-cow (64 B, 10%) | -80.0% (1176 -> 234 MiB), better | -80.0% (1176 -> 235), better |
| redis-cow.peak_mib | -32.2%, better | -32.2%, better |
| redis-cow-d1k (1 KiB, 10%) | -96.6% (891 -> 30 MiB), better | -96.6%, better |
| redis-cow-d1k.peak_mib | -17.9%, better | -17.9%, better |
| redis-cow-tput.set_inside_rps | +43% (425k -> 606k), better | +36% (310k -> 422k), better |
| redis-cow-tput (outside the save) | inconclusive [-10.6%, +19.6%] | inconclusive [-8.7%, +15.5%] |
| redis-cow.rss_after_mib (right after the save) | +7.8%, worse | +7.8%, worse |
| redis-cow-d1k.rss_after_mib | +9.4%, worse | +9.4%, worse |
| redis-cow-rec (RSS 120 s later, activedefrag) | -2.4% inconclusive | -0.4%, sub-threshold |
| redis-cow-rec.recover_s | 52 s | 84 s |

Against Redis as shipped (bundled jemalloc, no patches): redis-cow 1141
-> 234 MiB (-79.4%), redis-cow-d1k 887 -> 30 (-96.6%), SET during the save
+56% on amd64 (390k -> 606k) and +62% on arm64 (260k -> 422k).

The run's headline is REGRESSION, from `rss_after_mib` and `recover_s`:
the epoch's price, the bytes written during the save held above the old
data until the drain moves them (as on Valkey, +8.1%). `noepoch` never
rises above the 5% band, so its `recover_s` is 1. Redis takes longer to
drain than Valkey (52 and 84 s against 24 and 32 s): its defragmenter
walks the whole keyspace each cycle.

`kv-redis --rows redis-ops,redis-ops-d1k --controls '' --blocks 4`,
challoc against the bundled jemalloc, paired, once from main 053c930 (run
`20261122t0046z-053c930-kv-redis-hmbmz`, contrib/redis 0001 to 0004) and
once from head 1627280 (run `20261122t0046z-1627280-kv-redis-is7um`, 0001
to 0006). The two runs are not paired with each other; challoc's gap to
jemalloc is the comparison:

| row | main amd64 | head amd64 | main arm64 | head arm64 |
|---|---|---|---|---|
| redis-ops (challoc vs jemalloc) | +3.2% [+0.8, +5.7] | +1.9% [+1.5, +2.3] | +4.7% [+4.3, +5.1] | +4.4% [+2.5, +6.3] |
| redis-ops-d1k | -3.3% [-4.2, -2.5] | -3.2% [-5.1, -1.3] | -1.2% [-1.9, -0.5] | -1.4% [-3.4, +0.7] |

challoc's medians: redis-ops 980k -> 986k (amd64), 703k -> 699k (arm64);
redis-ops-d1k 631k -> 640k, 499k -> 497k. The patches move none of the
gaps beyond the runs' margins. The 2..5% gap on redis-ops at 64 B is
main's, not this change's. Positive percentages are challoc slower.
