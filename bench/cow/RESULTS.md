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

# Client throughput during the drain

After a save with the fork epoch, 0006 (Redis) and 0004 (Valkey) hold a
defrag cycle at active-defrag-cycle-max (25 by default) while challoc
reports a size class draining. What that costs clients: `cowbench.py
--drain-bench` (valkey-cow.sh MODE=drain, the `*-cow-drain` rows) turns
activedefrag on before the save, overwrites a tenth of the keys during
it, and runs the benchmark (50 clients, pipeline 16, 1M requests per
pass, GET then SET) with no child before the save (quiet) and right
after the child exits (drain). The comparison is one build with the
epoch (cow, head) and without it (noepoch, `cow_epoch:false`: no epoch,
so no drain), both with activedefrag on. After the passes it samples
`active_defrag_running` (INFO memory) and `active_defrag_hits` (INFO
stats) once a second: `drain_s` runs from the child's exit to the first
of three samples with no cycle running, `moves_s` to the last sample on
which challoc's `drain_moves` (the moves its utilisation queries asked
for: the drain's own work) still rose, and `hits_s` the same for the
server's `active_defrag_hits`, which also count the moves the server's
own fragmentation rule makes (noepoch's are the two passes, about 2.5
s). The table in this section predates that split: its moves column
followed the hits, so it is headed `hits_s` here. Before this was fixed the sampler read
`active_defrag_running` from INFO stats, where neither server prints it,
so every sample read 0 and `drain_s` was the length of the passes; the
figures below are from the fixed sampler.

## Session measurements

Not harness-tier: one develop session, amd64 Xeon 6975P-C, 4 vCPU, 15
GiB, kernel 6.18.62, two repetitions, medians (`run-drain.sh`,
`results-drain.jsonl`). Builds: tests/consumers/redis-build.sh and
valkey-build.sh at challoc main deda0db (quantum8), Redis 8.10.3 with
contrib/redis 0001 to 0006, Valkey 9.1.2 with contrib/valkey 0001 to
0004. Requests/s, p99 in ms:

| cell | GET noepoch | GET cow | SET noepoch | SET cow |
|---|---|---|---|---|
| Redis 64 B x 10M | 766k, 2.12 | 551k (-28%), 2.63 | 1003k, 1.33 | 602k (-40%), 2.65 |
| Redis 1 KiB x 2M | 853k, 1.77 | 646k (-24%), 1.98 | 770k, 1.38 | 551k (-28%), 1.96 |
| Valkey 64 B x 10M | 1263k, 1.40 | 815k (-35%), 1.69 | 1160k, 1.16 | 834k (-28%), 1.41 |
| Valkey 1 KiB x 2M | 814k, 2.18 | 623k (-23%), 1.78 | 750k, 1.41 | 554k (-26%), 1.97 |

With no child (quiet), where no epoch has opened, the two builds differ
by up to 8.6% (Valkey 64 B SET; the other seven cells within 3%): that
is this session's run-to-run spread, the yardstick for the drain
figures. (The earlier run's spread was 4.6%.)

How long the drain lasts (both repetitions; noepoch 1.7 to 2.7 s):

| cell | `drain_s` | `hits_s` | defrag hits |
|---|---|---|---|
| Redis 64 B | 319, 323 | 88, 70 | 5.57M |
| Redis 1 KiB | 304, 304 | 18, 14 | 0.87M |
| Valkey 64 B | 302, 309 | 302, 309 | 2.45M |
| Valkey 1 KiB | 305, 303 | 305, 303 | 0.45M |

On every cow run the cycle ran at active-defrag-cycle-max (25) until
challoc's 300 s drain window closed (`DRAIN_WINDOW_NS`, src/cow.rs:92,
from the child's exit), with `drain_floors` 6 to 9 at the end. On Redis
the hits stopped rising after 14 to 88 s and the cycle ran on with
nothing to move. On Valkey the hits still rose at the last samples, but
that does not show the drain still moving values: Valkey's hits include
its own fragmentation moves, and ran 2.4 to 3 times challoc's
`drain_moves` here (2.45M against 0.94M at 64 B). Sampling `drain_moves`
shows Valkey's drain moves ending 17 to 61 s after the save, as Redis's
do ("Ending the drain with its work", below).

## Harness tier (cb launch, gate cells ref-amd64 and ref-arm64)

`cow --base origin/main --rows valkey-cow-drain,valkey-cow-drain-d1k,redis-cow-drain,redis-cow-drain-d1k
--variants 'noepoch=head@cow_epoch:false,base,head' --controls ''`,
head f91c385, base main deda0db (run `20261122t0621z-f91c385-cow-sufvy`,
one variant per VM, 3 replicas). base and head build the same library
(sha256 413978c3c47ac8a9; this change touches no allocator code), so
head against noepoch is the drain's cost. Medians, noepoch -> head,
requests/s and p99 ms; every throughput and p99 change below is `worse`
for head except the two marked inconclusive:

| row | amd64 | arm64 |
|---|---|---|
| valkey-cow-drain GET | 1181k -> 763k (-35%); p99 0.82 -> 1.66 | 1171k -> 685k (-41%); p99 0.81 -> 1.83 |
| valkey-cow-drain SET | 1008k -> 744k (-26%, inconclusive); p99 0.95 -> 1.46 | 911k -> 646k (-29%); p99 1.00 -> 1.64 |
| valkey-cow-drain-d1k GET | 935k -> 615k (-34%); p99 1.05 -> 1.68 | 767k -> 573k (-25%); p99 1.19 -> 1.73 |
| valkey-cow-drain-d1k SET | 724k -> 474k (-35%); p99 1.42 -> 2.26 | 597k -> 424k (-29%); p99 1.78 -> 2.63 |
| redis-cow-drain GET | 815k -> 556k (-32%); p99 1.74 -> 2.63 | 615k -> 425k (-31%); p99 2.38 -> 3.25 |
| redis-cow-drain SET | 1009k -> 577k (-43%); p99 1.14 -> 2.25 | 751k -> 460k (-39%); p99 1.42 -> 3.44 |
| redis-cow-drain-d1k GET | 837k -> 661k (-21%); p99 1.13 -> 1.62 | 747k -> 582k (-22%); p99 1.21 -> 1.73 |
| redis-cow-drain-d1k SET | 647k -> 513k (-21%); p99 1.62 -> 2.14 | 561k -> 449k (-20%); p99 2.02 -> 2.35 (inconclusive) |

With no child (`set_quiet_rps`, `get_quiet_rps`) every row is
inconclusive within about +-10% on both cells. head against base (the
same library on different VMs, an A/A in effect): every row inconclusive
except redis-cow-drain SET on amd64, +10.6% [+5.9%, +15.5%] `worse`,
which is the spread between three VMs per variant, not a code change.
This run's `drain_s` came from the broken sampler (the passes' length)
and is left out; the session table above has the fixed figures.

So while the drain runs, clients get a fifth to two fifths less
throughput and a p99 1.2 to 2.4 times higher, on both servers and
architectures, more at 64 B than at 1 KiB, and the cycle runs for
challoc's whole 300 s drain window after every save, also after it has
nothing left to move. One ad-hoc check (1 KiB x 2M, one run each, not in
results-drain.jsonl): the same passes 120 s after the child exited, when
Redis's moves had ended, still cost Redis GET -31% (610k against
noepoch's 882k) and SET -29% (709k against 994k), and Valkey -19% and
-20%. So a cycle that moves nothing costs clients about as much as one
that does, for the rest of the window. Ending the forced cycle when the
drain's work ends is #871 ("Ending the drain with its work", below).

# Ending the drain with its work (#871)

Before #871 a pair (arena x size class) drained while it had a hole
below its drain floor and any live block above it, so
`challoc_cow_stat(CHALLOC_COW_DRAINING)` stayed nonzero, and Redis's and
Valkey's patches held the defrag cycle at active-defrag-cycle-max, for
challoc's whole 300 s window. Sampling each pair's state once a second
(a throwaway print of the drain pairs in the `fork epochs:` report, on
Redis 64 B and 1 KiB and Valkey 64 B) showed why: the pairs with real
work (Redis's 40- and 80-byte classes at 64 B, the 1280-byte class at
1 KiB, Valkey's 64- and 96-byte classes) ended their drains 15 to 60 s
after the save, when their holes ran out or their blocks above the
floor were gone. What kept the figure up for the rest of the window
were 6 to 10 other pairs, each with one slab above the floor holding 1
to 8 live blocks that no defragmenter ever moved (the same counts at
every sample to the window's end), and holes below. A small Redis case
(tests/unit/fork-drain.tcl's 200k keys) also left pairs of another
arena with two slabs above the floor, about 500 blocks each, which
nothing moved either.

challoc now ends a pair's drain when the work worth a forced cycle is
done (src/cow.rs, `draining_pairs`; docs/design.md section 5,
"Fork epochs"):

- no hole below the floor, as before;
- the live blocks above the floor sit in fewer than two slabs
  (`DRAIN_MIN_SLABS`): moving them gives back at most one slab;
- no drain move asked for in the last 10 s while the figure is read
  (`DRAIN_STALL_NS`): the defragmenter, running flat out, found nothing
  left it can move.

An ended drain stays ended until the next epoch, so the figure only
falls. The 300 s window stays as the bound for a program that never
reads the figure.

## Session measurements

One develop session, amd64 Xeon 6975P-C, 4 vCPU, 15 GiB, two
repetitions, medians; `results-drain-end.jsonl`. The servers are
tests/consumers/redis-build.sh and valkey-build.sh builds (Redis
8.10.3 with contrib/redis 0001 to 0006, Valkey 9.1.2 with contrib/valkey
0001 to 0004, quantum8); each run is `cowbench.py --drain-bench
--drain-wait 330 --late-s 120` (run-drain.sh's command) with the staged
libchalloc.so.0 swapped between three variants: base (#863's tree, the
drain as it was), head (this change on #863's tree; the same diff to
src/ as on main) and noepoch (base with `cow_epoch:false`, no epoch, so
no drain). `drain_s` is until the cycles stopped, `moves_s` until
challoc's `drain_moves` last rose; every run's cycles stopped within
the 330 s cap.

| cell | `drain_s` base | `drain_s` head | `moves_s` base | `moves_s` head | `drain_floors` at the end, base / head |
|---|---|---|---|---|---|
| Redis 64 B x 10M | 302, 303 | 101, 116 | 60, 61 | 57, 66 | 8 / 0 |
| Redis 1 KiB x 2M | 300, 301 | 22, 21 | 15, 14 | 13, 14 | 9 / 0 |
| Valkey 64 B x 10M | 302, 308 | 42, 56 | 41, 61 | 42, 41 | 6 / 0 |
| Valkey 1 KiB x 2M | 303, 304 | 18, 16 | 17, 22 | 11, 10 | 7 / 0 |

With head the cycles stop 1 to 15 s after the last drain move, except
Redis at 64 B, where they run about 40 s longer: challoc's figure is 0
from about 62 s (sampled once every 2 s in a separate run), and Redis
finishes the cycle it had started, a full scan of 10M keys at
active-defrag-cycle-max (computeDefragCycles does not lower a running
cycle's effort, and the hits it makes then are its own fragmentation
moves, a few hundred a second). That part is the server's, not the
drain's.

The passes 120 s after the child's exit (late), against noepoch's,
requests/s and p99 ms:

| cell | noepoch | base | head |
|---|---|---|---|
| Redis 64 B GET | 1319k, 1.12 | 995k (-25%), 1.62 | 1356k (+3%), 1.13 |
| Redis 64 B SET | 1300k, 0.86 | 1015k (-22%), 1.24 | 1296k (-0%), 1.09 |
| Redis 1 KiB GET | 918k, 1.29 | 708k (-23%), 1.66 | 903k (-2%), 1.32 |
| Redis 1 KiB SET | 1002k, 1.03 | 745k (-26%), 1.68 | 969k (-3%), 1.69 |
| Valkey 64 B GET | 1299k, 1.11 | 971k (-25%), 1.54 | 1319k (+2%), 0.97 |
| Valkey 64 B SET | 1209k, 0.87 | 883k (-27%), 1.44 | 1217k (+1%), 0.99 |
| Valkey 1 KiB GET | 873k, 1.43 | 663k (-24%), 2.35 | 854k (-2%), 1.66 |
| Valkey 1 KiB SET | 946k, 1.38 | 716k (-24%), 1.57 | 940k (-1%), 1.44 |

The passes right after the child's exit, while the drain moves values,
cost the same with base and head (within 3% of each other on every
cell), as they should: the drain's work is unchanged. RSS at the end of
the run, as a share of RSS before the save, is the same for base and
head (Redis 64 B 0.924 and 0.924, Redis 1 KiB 0.994 and 0.994, Valkey
64 B 0.989 and 0.992, Valkey 1 KiB 0.998 and 0.999): ending the drain
early gives back no less memory.

## Harness tier (cb launch, gate cells ref-amd64 and ref-arm64)

`cow --base origin/main --cells ref-amd64,ref-arm64 --rows
valkey-cow-drain,valkey-cow-drain-d1k,redis-cow-drain,redis-cow-drain-d1k,valkey-cow-rec,redis-cow-rec
--variants 'noepoch=head@cow_epoch:false,base,head' --controls ''`, run
`20261122t1639z-98ae215-cow-pp2v5`, base main fa5de545 (#863 merged;
#868, which touches no drain code, landed after it), one variant per
VM, 3 replicas. Medians:

| row | cell | `drain_s` base -> head | `moves_s` base -> head |
|---|---|---|---|
| redis-cow-drain | amd64 | 304 -> 88 | 51 -> 53 |
| redis-cow-drain | arm64 | 307 -> 141 | 84 -> 83 |
| redis-cow-drain-d1k | amd64 | 303 -> 17 | 11 -> 11 |
| redis-cow-drain-d1k | arm64 | 305 -> 28 | 19 -> 18 |
| valkey-cow-drain | amd64 | 305 -> 34 | 32 -> 34 |
| valkey-cow-drain | arm64 | 307 -> 73 | 49 -> 51 |
| valkey-cow-drain-d1k | amd64 | 301 -> 14 | 10 -> 9 |
| valkey-cow-drain-d1k | arm64 | 303 -> 24 | 23 -> 23 |

Every run's cycles stopped within DRAIN_WAIT (`drain_ended` 100 on
every row). The late passes (120 s after the child's exit), against
noepoch, requests/s; base is `worse` than noepoch on every row but three
(inconclusive), head is inconclusive against noepoch on every row but
Redis 64 B on arm64:

| row | cell | GET noepoch / base / head | SET noepoch / base / head |
|---|---|---|---|
| redis-cow-drain | amd64 | 1182k / 989k (-16%) / 1456k | 1032k / 885k (-14%) / 1307k |
| redis-cow-drain | arm64 | 1058k / 782k (-26%) / 765k (-28%) | 981k / 756k (-23%) / 736k (-25%) |
| redis-cow-drain-d1k | amd64 | 907k / 739k (-19%) / 948k | 853k / 684k (-20%) / 895k |
| redis-cow-drain-d1k | arm64 | 782k / 518k (-34%) / 810k | 771k / 527k (-32%) / 823k |
| valkey-cow-drain | amd64 | 1192k / 996k (-16%) / 1218k | 1016k / 886k (-13%) / 1044k |
| valkey-cow-drain | arm64 | 1155k / 826k (-28%) / 1161k | 882k / 644k (-27%) / 887k |
| valkey-cow-drain-d1k | amd64 | 949k / 660k (-30%) / 855k | 879k / 620k (-29%) / 784k |
| valkey-cow-drain-d1k | arm64 | 773k / 568k (-27%) / 755k | 764k / 568k (-26%) / 762k |

The late p99 follows: base 1.24 to 1.94 ms against noepoch's 0.82 to
1.25 (`worse` on every row), head within noise of noepoch on every row
but Redis 64 B on arm64 (1.46 and 1.49 ms against 0.88 and 0.95).

Redis 64 B on arm64 is the session's Redis 64 B tail again, longer: the
drain's moves end at 83 s, challoc reports nothing draining soon after,
and Redis finishes the full scan of 10M keys it was running at
active-defrag-cycle-max, about 55 s on this cell, so at 120 s the
cycle still runs. Shortening that would need the server to stop a
cycle the drain started when `CHALLOC_COW_DRAINING` falls to 0, a
change to contrib/redis 0006 that this change does not make. Valkey
stops its cycle within seconds on both cells.

The passes right after the child's exit, while values move, cost the
same with base and head (the drain's work is unchanged). The `*-cow-rec`
rows (RSS 120 s after the save with activedefrag on, as a percentage of
RSS before it) end within half a point of base with head: Redis 97.4
-> 97.9 (amd64) and 97.7 -> 97.7 (arm64), Valkey 99.2 -> 99.6 and 99.1
-> 99.4, with `recover_s` the same for both (52, 88, 24 and 32 s; noepoch's 1 s
is no epoch at all).
