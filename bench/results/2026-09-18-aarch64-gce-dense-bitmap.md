# Phase 1.5 task 4f: dense per-class slot bitmap, aarch64 (GCE C4A)

Commit 5304cd4 (code) against 7efdae8 (task 4e landed; `old` in every
table). VM `challoc-bench-arm64`, c4a-standard-16 (Neoverse V2), zone
`us-east4-a`, Ubuntu 24.04 / 7.0.0-1011-gcp, 4 KiB pages; jemalloc rows are
Wolfi's 5.3 preloaded, same run. Companion files: the harness output of the
A/B (`-ab.md`), the soak summary and CSVs (`-soak-valkey-soak*.{md,csv}`).
x86_64 in `2026-09-18-x86_64-gce-dense-bitmap.md`.

## 0. Mechanism and attribution (before the change)

Task 4e's attribution (`2026-09-18-aarch64-gce-sized-free.md` 4.3): after
the sized free stopped loading the slab descriptor, the descriptor miss
moved to Valkey's `zmalloc_size` (= `malloc_usable_size`) call on the
`zfree(ptr)` path, `SlabTable::check` at 11.2% of server cycles in the
write phase (usable-size query 492 cycles per SET against jemalloc's 327),
and plain `free` (Ruby, `valkey1-free`) pays the same miss on every call:
one 64-byte descriptor line per slab, 9 MB for a 550 MiB heap of 1 KiB
values, a cache miss per check. The aarch64 `valkey1-free` row before the
change (this file, section 2): 247.8 cycles per operation against
jemalloc's 225.3, with 4.09 L1D misses per operation. The change moves the
allocation bits into one contiguous bitmap per class (one bit per slot,
70 KB for that heap, L2-resident) and the arena into a dense per-group
owner array, so the check and the routing read two dense structures and
never the descriptor. The trap the plan named (adjacent slabs of different
arenas sharing a bitmap line, so the check sees coherence traffic) is
designed around by handing slabs to arenas in line-sized groups (the slabs
whose bits fill one 64-byte line: 128 for the 1024-byte class, 1 for the
classes below 256 bytes, whose slabs have a line or more of bits each), and
the gauge for it is the 10-thread L1D misses per operation below.

## 1. `quick` (go/no-go, one round, working tree against 7efdae8)

```
| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.0 (16.6 MiB) | 42.5 (16.8 MiB) | 45.1 (18.1 MiB) | +1.2% | +6.1% |
| larson10 | M ops/s | 375.0 (118.7 MiB) | 367.3 (115.9 MiB) | 386.6 (131.9 MiB) | +2.1% | +5.3% |
| xmalloc | M frees/s | 567.279 (81.4 MiB) | 557.299 (85.0 MiB) | 481.088 (78.0 MiB) | +1.8% | -13.7% |
| alloc-test1 | s | 2.648 (13.0 MiB) | 2.653 (13.1 MiB) | 2.537 (12.3 MiB) | +0.2% | +4.6% |
| valkey1 | M ops/s | 7.102 (586.8 MiB) | 7.043 (584.5 MiB) | 7.736 (585.5 MiB) | +0.8% | +9.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 24602571 ops/s |
quick: go (new within 5% of old on every row; churn compacts)
```

`make test` on the VM: 226 + 79 pass (the negative modes included).

## 2. `quick-perf`: per-operation counters (one run each, `sudo perf stat` from the host)

```
| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |
|---|---|---:|---:|---:|---:|---:|
| larson1 | old | 200.00 | 15.0 | 51.6 | 0.525 | 0.677 |
| larson1 | new | 200.00 | 15.0 | 52.0 | 0.442 | 0.842 |
| larson1 | jemalloc | 200.00 | 14.9 | 50.4 | 0.463 | 0.380 |
| larson10 | old | 200.02 | 149.2 | 449.8 | 4.882 | 6.627 |
| larson10 | new | 200.02 | 149.1 | 450.0 | 4.088 | 7.740 |
| larson10 | jemalloc | 199.99 | 148.8 | 433.5 | 4.269 | 3.600 |
| valkey1 | old | 13.34 | 227.3 | 227.7 | 3.034 | 2.880 |
| valkey1 | new | 13.44 | 225.6 | 227.1 | 3.056 | 2.871 |
| valkey1 | jemalloc | 14.07 | 215.2 | 165.6 | 3.038 | 2.715 |
| valkey1-free | old | 12.23 | 247.8 | 219.4 | 4.089 | 4.032 |
| valkey1-free | new | 13.31 | 227.9 | 214.8 | 4.042 | 3.863 |
| valkey1-free | jemalloc | 13.43 | 225.3 | 164.4 | 3.445 | 3.374 |
```

`dTLB miss` is `L1D_TLB_REFILL` on this core; `larson` "ops" here are the
harness's fixed 200 M operations (the M ops/s column is not a throughput).

Reading. The plan's gauge for the grouping trap passes: 10-thread L1D
misses per operation 4.882 -> 4.088 (-16%, now below jemalloc's 4.269),
one-thread 0.525 -> 0.442; a rise would have meant arenas sharing bitmap
lines. Cycles per operation are unchanged on both larson rows (15.0 ->
15.0, 149.2 -> 149.1) and instructions within 0.4. `valkey1-free` (plain
`free`, the row this task is for) goes from 247.8 to 227.9 cycles per
operation (-8%) and 12.23 -> 13.31 M ops/s, against jemalloc's 225.3 /
13.43: the exact check is now at jemalloc's cost. `valkey1` (`sdallocx`)
is unchanged within one run, as expected. L1D TLB refills per larson
operation rise (0.677 -> 0.842 at one thread, 6.63 -> 7.74 at ten) while
`valkey1-free`'s fall (4.03 -> 3.86): on a small heap every class's
metadata used to be one page (descriptor lines with the bits inline) and
is now two (the bitmap page the check reads, the descriptor page the
refill and flush update) plus, with several arenas, the owner-array page;
on a large heap the check's footprint shrinks from one page per 64 slabs
to one per 8192 (the 1024-byte class), which is what the Valkey rows see.
The small-heap page count is a cost of the dense layout (a chunked layout
that put a group's descriptors next to its bits would spread a large
heap's bits over one page per few hundred slabs again); it does not show
in cycles here.

## 3. A/B protocol rows, aarch64 (5 rounds plus warm-up, interleaved, same run; harness output in `-ab.md`)

`make ab VARIANTS='old=7efdae8 new=HEAD jemalloc' BENCHES='larson1 larson10
larson16 xmalloc alloc-test1 alloc-testN valkey1 valkey1-free'`, 05:57 UTC;
load average at start: 0.25 1.00 1.08; at end: 5.55 5.54 4.06; 936 s total.

### Sorted scores

| program | variant | sorted scores | maxrss KiB |
|---|---|---|---:|
| larson1 | old | 42.9 43.0 43.0 43.0 43.0 | 16948 |
| larson1 | new | 42.2 42.3 42.5 42.5 42.6 | 16948 |
| larson1 | jemalloc | 45.1 45.1 45.1 45.1 45.1 | 18576 |
| larson10 | old | 370.7 371.4 371.6 373.7 374.6 | 119472 |
| larson10 | new | 365.5 366.7 369.8 370.7 373.1 | 119220 |
| larson10 | jemalloc | 381.5 385.4 386.5 386.8 387.3 | 135488 |
| larson16 | old | 594.7 598.6 599.0 601.0 605.6 | 183464 |
| larson16 | new | 593.4 594.1 595.7 596.5 597.6 | 186788 |
| larson16 | jemalloc | 609.3 609.4 609.7 612.5 615.3 | 211432 |
| xmalloc | old | 564.232 566.481 566.839 567.307 568.444 | 85428 |
| xmalloc | new | 554.446 556.111 556.606 557.047 557.413 | 85432 |
| xmalloc | jemalloc | 474.784 478.266 480.828 482.105 486.217 | 84940 |
| alloc-test1 | old | 2.650 2.651 2.655 2.664 2.678 | 13312 |
| alloc-test1 | new | 2.660 2.666 2.667 2.672 2.689 | 13440 |
| alloc-test1 | jemalloc | 2.535 2.544 2.545 2.550 2.556 | 12636 |
| alloc-testN | old | 2.258 2.258 2.258 2.262 2.262 | 21108 |
| alloc-testN | new | 2.276 2.277 2.281 2.286 2.286 | 21244 |
| alloc-testN | jemalloc | 2.206 2.207 2.208 2.214 2.220 | 16860 |
| valkey1 | old | 6.954 6.985 7.082 7.084 7.105 | 601208 |
| valkey1 | new | 6.959 7.051 7.095 7.099 7.101 | 598948 |
| valkey1 | jemalloc | 7.620 7.706 7.760 7.785 7.815 | 599884 |
| valkey1-free | old | 6.626 6.775 6.782 6.791 6.797 | 601208 |
| valkey1-free | new | 7.189 7.278 7.317 7.333 7.339 | 598952 |
| valkey1-free | jemalloc | 7.135 7.137 7.144 7.183 7.313 | 599636 |

### Medians

| program | unit | old | old RSS MiB | new | new RSS MiB | jemalloc | jemalloc RSS MiB | new vs old | jemalloc vs old |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 43.0 | 16.6 | 42.5 | 16.6 | 45.1 | 18.1 | +1.2% | -4.7% |
| larson10 | M ops/s | 371.6 | 116.7 | 369.8 | 116.4 | 386.5 | 132.3 | +0.5% | -3.9% |
| larson16 | M ops/s | 599.0 | 179.2 | 595.7 | 182.4 | 609.7 | 206.5 | +0.6% | -1.8% |
| xmalloc | M frees/s | 566.839 | 83.4 | 556.606 | 83.4 | 480.828 | 82.9 | +1.8% | +17.9% |
| alloc-test1 | s | 2.655 | 13.0 | 2.667 | 13.1 | 2.545 | 12.3 | +0.5% | -4.1% |
| alloc-testN | s | 2.258 | 20.6 | 2.281 | 20.7 | 2.208 | 16.5 | +1.0% | -2.2% |
| valkey1 | M ops/s | 7.082 | 587.1 | 7.095 | 584.9 | 7.760 | 585.8 | -0.2% | -8.7% |
| valkey1-free | M ops/s | 6.782 | 587.1 | 7.317 | 584.9 | 7.144 | 585.6 | -7.3% | -5.1% |

Positive percentages mean the variant did worse than old (slower or lower throughput).

Reading. Every larson/xmalloc/alloc-test row is within the plan's 3% of
old: larson1 +1.2% (43.0 -> 42.5, lists 42.9..43.0 against 42.2..42.6, a
real 1%), larson10 +0.5% and larson16 +0.6% (lists overlap), xmalloc +1.8%
(566.8 -> 556.6 M frees/s, lists separated, still 16% ahead of jemalloc),
alloc-test1 +0.5%, alloc-testN +1.0%; RSS unchanged on every row.
`valkey1` (frees through `sdallocx`, which never read the bit) is
unchanged (7.082 -> 7.095). `valkey1-free` (plain `free`, the row this
task is for) goes 6.782 -> 7.317 M ops/s (+7.9%; lists 6.63..6.80 against
7.19..7.34), and jemalloc's 7.144 is now 2.4% behind it: the exact check
on a 550 MiB heap costs less than jemalloc's rtree lookup. The 1..2% on
the small-heap rows is the second metadata page per class (section 2) and
the four extra instructions of the bit-index arithmetic on the free path;
it is inside the criterion and the Valkey rows are the target.

## 4. The 20-minute Valkey soak with the perf sidecar (acceptance run, code of 5304cd4)

`make valkey STAGE=build && make valkey-soak DURATION=1200 SETTLE=60` (the
full protocol: `ALLOCS="challoc jemalloc"`, 600 s write phase then 600 s
delete phase per allocator, Valkey 9.1.2 rebuilt against the tree's
`libchalloc.so` and the same tag with bundled jemalloc 5.3.0), 06:17..07:00
UTC, with `bench/probes/valkey-perf-sidecar.sh ~/logs/4f-soak 700 1205` on
the VM host (`perf stat -I 1000` over each server's life, 60 s `perf
record` windows, snapshots of `INFO commandstats`/`INFO stats`/per-thread
CPU around each window). The sidecar was started after the soak's Valkey
rebuild had already let the challoc server begin, so it attached to that
server at t=213 and its write window is t=213..275 against jemalloc's
t=100..161; the write phase alternates 22 s valkey-benchmark and 30 s
memtier runs (generator table in the soak summary), so each 60 s window
holds about one run of each, both slowed by `perf record` the same way
(the runs inside the windows: challoc 755k / 740k req/s, jemalloc's
likewise below the unrecorded 900k+); the delete windows are t=700..761
for both (memtier DEL:SET 4:1). Per-command figures divide the `perf
stat` rows between two snapshots by that window's
`total_commands_processed` delta (task 4d's method; `percmd.py` in the
sidecar's log directory). The soak summary is in
`2026-09-18-aarch64-gce-dense-bitmap-soak-valkey-soak.md` (with the two
`-challoc.csv` / `-jemalloc.csv` sample files) next to this file.

### 4.1 Cycles per command (the plan's tie-breaker)

| window | alloc | commands/s | cyc/cmd | ins/cmd | IPC | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd | main thread | SET us/call |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | jemalloc | 901k | 3232 | 9835 | 0.33 | 39.7 | 38.8 | 7.1 | 100.1% | 0.183 |
| write, under `perf record`, t=100..161 | jemalloc | 714k | 3407 | 9725 | 0.35 | 41.0 | 43.0 | 6.7 | 100.1% | 0.222 |
| write, under `perf record`, t=213..275 | challoc | 735k | 3313 | 9773 | 0.34 | 45.2 | 65.9 | 6.5 | 100.1% | 0.226 |
| write tail, t=161..700 | jemalloc | 951k | 3071 | 9336 | 0.33 | 37.0 | 39.4 | 6.6 | 100.1% | 0.182 |
| write tail, t=275..700 | challoc | 986k | 2962 | 9249 | 0.32 | 39.4 | 56.3 | 6.3 | 100.1% | 0.186 |
| delete, under `perf record` (memtier DEL:SET 4:1), t=700..762 | jemalloc | 1080k | 2270 | 7431 | 0.31 | 24.0 | 24.2 | 5.1 | 100.0% | 0.234 |
| delete, under `perf record` (memtier DEL:SET 4:1), t=700..761 | challoc | 1043k | 2335 | 7366 | 0.32 | 25.2 | 27.5 | 5.4 | 100.2% | 0.242 |

Against task 4e (same method, `2026-09-18-aarch64-gce-sized-free.md` 4.1:
write under `perf record` 3430 against 3343, +2.6%; delete 2330 against
2237, +4.2%): the recorded write window is now 3313 against 3407 (-2.8%,
challoc ahead per command; the two windows are 110 s apart in the same
alternating generator schedule, so treat the sign as "at parity or
better" rather than a measured lead), the long write-tail windows 2962
against 3071 (-3.5%, the caveat being that challoc's is 114 s shorter), and
the delete window 2335 against 2270 (+2.9%, from +4.2%). Both servers are
main-thread bound in every window (100.0..100.2% of one CPU), so
throughput is cycles per command. SET server time (`INFO commandstats`):
0.226 us per call against 0.222 in the recorded write windows. L1D TLB
refills per command stay above jemalloc's (65.9 / 56.3 / 27.5 against
43.0 / 39.4 / 24.2): the bitmap made the check's footprint small, but
the refills and flushes now touch a descriptor page and a bitmap page,
and the value pages dominate either way; this is the residue for a
later look, not a regression against task 4e's 58 / 43 in a comparable
window (task 5's soak had 56 for the write phase).

### 4.2 Soak summary (`2026-09-18-aarch64-gce-dense-bitmap-soak-valkey-soak.md`)

| measure | challoc | jemalloc | challoc vs jemalloc | task 4e (same protocol) |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | 889639 | 855087 | -3.9% | -0.1% |
| write: valkey-benchmark p99 ms | 1.087 | 1.143 | -4.9% | +0.0% |
| write: memtier req/s | 929018 | 912194 | -1.8% | -0.8% |
| write: memtier p99 ms | 0.983 | 1.023 | -3.9% | -1.6% |
| delete: valkey-benchmark req/s | 1387341 | 1444188 | +4.1% | +3.8% |
| delete: valkey-benchmark p99 ms | 0.727 | 0.695 | +4.6% | +4.6% |
| delete: memtier req/s | 1210330 | 1259112 | +4.0% | +3.6% |
| delete: memtier p99 ms | 0.783 | 0.759 | +3.2% | +3.2% |
| peak RSS MiB (VmHWM) | 578.0 | 572.1 | +1.0% | +0.6% |
| RSS MiB after settle | 128.8 | 126.3 | +2.0% | -2.1% |

Positive means challoc did worse. The write phase, where the descriptor
miss was, is now ahead of bundled jemalloc on both generators' throughput
and p99 (1.8..4.9%, outside the generators' usual 3..5% only on the
valkey-benchmark rows, and the tie-breaker agrees on the sign); the delete
phase is unchanged from task 4e at 3.2..4.6% behind on throughput and p99,
which section 4.3 attributes. Peak RSS +1.0% and RSS after settle +2.0%
(the settle figure flips sign between runs: -2.1% in task 4e, -6.9% and
-2.5% in task 4d's) are within the Phase 1 criteria (+5%, 10%).

### 4.3 Where the cycles land now (`perf report` on the records, `--symfs` over the target volume)

DSO shares of the 60 s records (challoc; the jemalloc server's symbols
did not resolve after the fact, so its rows are the DSO split only):

| phase | alloc | valkey-server (own code) | allocator | kernel | libc |
|---|---|---:|---:|---:|---:|
| write | challoc | 47.48% | 13.58% (`libchalloc.so`) | 34.70% | 4.21% |
| write | jemalloc | 60.48% (incl. `je_*`) | | 33.14% | 6.37% |
| delete | challoc | 43.78% | 12.77% | 38.32% | 5.11% |
| delete | jemalloc | 54.08% (incl. `je_*`) | | 38.52% | 7.39% |

Symbol shares of all server samples, and in cycles per command (share times
the window's cycles per command), against task 4e's file:

| phase | symbol | 4e share | now | 4e cyc/cmd | now cyc/cmd | jemalloc (4e) |
|---|---|---:|---:|---:|---:|---:|
| write | usable-size query (`api::libc::usable_size`, the bit test now inlined in it) | 11.16% + 3.19% | 9.88% | 492 | 327 | 327 |
| write | free path `je_sdallocx` | 2.31% | 1.92% | 79 | 64 | 66 |
| write | `je_malloc` | 1.36% | 1.45% | 47 | 48 | 50 |
| delete | usable-size query | 4.65% + 3.93% | 8.81% | 200 | 206 | 96 |
| delete | free path | 2.16% | 1.52% | 50 | 35 | 49 |
| delete | `je_malloc` | 1.40% | 1.20% | 33 | 28 | |

The write phase is the target and is where the descriptor miss was: the
usable-size query goes from 492 to 327 cycles per SET, exactly jemalloc's
327 in task 4e's run, and `libchalloc.so` from 18.34% to 13.58% of the
server. `perf annotate` of `usable_size` in the write record puts 40% of
its cycles on the two instructions that consume the bitmap word (`lsr`
and `tbz` after the `ldr`), so the word is still a cache miss on this
heap, only a much shorter one than the descriptor line: the class bitmaps
of a 550 MiB Valkey heap are a few hundred KB spread over every class, and
the server's random SETs stream several MB of values through the 2 MB L2
per second, so the word comes from the system-level cache rather than
from L2; the descriptor line came from DRAM. The delete phase did not
move (206 against 200 cycles per command in the query, jemalloc 96 in
task 4e): there the bitmap-word consumers carry 23% of the query's cycles
and the rest is spread over the classify loads (the `CLASSES` and table
lines, 8..11% each), i.e. the query is no longer one miss but a chain of
short ones; jemalloc's rtree leaf for a run of adjacent DELs stays hot
where challoc walks two lines per pointer. That is the +2.9% of the
delete window and the next thing to look at on this path (an obvious
candidate: `Regions::classify` and the table line share a load or two
that the `free` fast path already avoids by reading geometry from the
dense arrays). The remaining `ldrh wzr` in the write record is the owner
word loaded and discarded by `SlabTable::check`'s callers that do not
need it; commit 73b6df1 gives `usable_size`/`realloc` a `check_live`
without it (section 6).

## 5. Ruby rows (`make ruby-bench`, 5 rounds plus warm-up; `2026-09-18-aarch64-ruby.md`, code 73b6df1)

Ruby 3.4.10 `--with-jemalloc` frees through plain `free`. Medians, wall
seconds, challoc against the same binary with Wolfi's jemalloc 5.3.1
preloaded; "before" is the previous run of the same rows on this VM
(`2026-09-17-aarch64-ruby.md`, commit a10f012):

| row | challoc before | challoc now | jemalloc now | challoc vs jemalloc before -> now |
|---|---:|---:|---:|---:|
| so_binary_trees | 4.686 | 4.687 | 4.664 | +0.3% -> +0.5% |
| hash_aref_str | 0.357 | 0.352 | 0.351 | +3.2% -> +0.3% |
| array_sample_100k__1k | 0.429 | 0.427 | 0.426 | -0.2% -> +0.2% |
| array_sort_block | 5.309 | 5.298 | 5.306 | -0.2% -> -0.2% |
| so_nbody | 0.824 | 0.823 | 0.822 | +0.2% -> +0.1% |
| app_fib | 0.292 | 0.292 | 0.291 | +0.7% -> +0.3% |
| erb_render | 0.897 | 0.857 | 0.962 | -7.1% -> -10.9% |
| gem_list | 0.119 | 0.115 | 0.111 | +4.4% -> +3.6% |
| gc_churn | 1.631 | 1.629 | 1.640 | -1.3% -> -0.7% |

RSS: challoc below jemalloc on every row but `gc_churn` (382.3 against
365.0 MiB, +4.8%; +6.6% before). The Ruby rows are not allocator-bound
(within 0.5% of jemalloc on seven of nine, `startup` is 3 ms either way);
nothing moved outside the run-to-run spread, in either direction, which
is the expected result of a change that only shortens a miss Ruby's
small heaps rarely take.

## 6. Scorecard line (`bench/results/scorecard-aarch64.md`, 3 rounds, challoc against jemalloc, positive = challoc behind; code 73b6df1)

```
| 2026-09-18 | 27de567 | +5.4% | +4.9% | +3.2% | -15.5% | +3.9% | +2.1% | +10.3% | +7.2% | +0.1% | -2.3% | -37.9% | -4.6% | -13.5% | 0.88 | 1.00 | -0.1% | +3.8% | +0.6% | -2.1% | 3 rounds; soak bench/results/2026-09-18-aarch64-gce-sized-free-soak-valkey-soak.md |
| 2026-09-18 | 73b6df1 | +6.6% | +3.8% | +2.9% | -14.4% | +4.3% | +3.4% | +15.4% | +1.7% | +0.1% | -2.0% | -38.3% | -5.4% | -14.5% | 0.89 | 1.00 | - | - | - | - | 3 rounds; no soak for 73b6df1 |
```

Columns: larson1, larson10, larson16, xmalloc, alloc-test1, alloc-testN,
valkey1, valkey1-free, cfrac, espresso, mstress, glibc-simple, redis, RSS
ratios (larson10, valkey1), soak columns. `valkey1-free` +7.2% -> +1.7%
behind jemalloc is this task's row (the 5-round A/B has it 2.4% ahead).
`valkey1` reads +15.4% from +10.3% because jemalloc's three runs came in
at 8.17..8.24 M ops/s in this scorecard against 7.62..7.82 in the A/B and
task 4e's scorecard, while challoc's 7.10..7.44 match its A/B list
(7.08..7.10 there): a jemalloc-side swing of the 3-round tier, not a
challoc change (section 3: `valkey1` unchanged within 0.2% same-run).
The larson/alloc-test columns moved by their usual 1..2% in both
directions. The soak columns are empty because the acceptance soak
(section 4) ran on 5304cd4, one load on the `usable_size` path away from
73b6df1; section 7 has the 600 s challoc soak at 73b6df1 with the sidecar.

## 7. `check_live` (73b6df1): the 600 s challoc soak with the sidecar

Section 4.3's write record showed `SlabTable::check` loading the owner
word for callers that discard it (`usable_size`, `realloc`, `rallocx`);
73b6df1 gives them `check_live` (the bit test alone). Gate: `make test`
226 + 79 on both VMs, `quick` go on x86_64 against 5304cd4 (every row
within 2.2%, `valkey1` +2.0% as the median of 3). `valkey_model` frees
without a usable-size query, so the row that sees this is the real
server: `make valkey STAGE=build && make valkey-soak ALLOCS=challoc
DURATION=600 SETTLE=30` (300 s write, 300 s delete; results under
`target/`, not an acceptance run) with `valkey-perf-sidecar.sh
~/logs/4f-soak2 400 605`, 07:15..07:26 UTC, on an otherwise idle VM. The
t=30..100 and t=100..161 write windows have the same heap history as the
full soak's, so they compare with section 4.1's jemalloc rows; the delete
window (t=400..461, after a 300 s write phase) does not compare exactly
with the full soak's t=700..761.

| window | alloc (code) | commands/s | cyc/cmd | ins/cmd | IPC | L1D miss/cmd | dTLB refill/cmd | br-miss/cmd | main thread | SET us/call |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| write, unrecorded, t=30..100 | challoc (73b6df1) | 937k | 3109 | 9710 | 0.32 | 41.5 | 52.9 | 7.3 | 100.1% | 0.187 |
| write, unrecorded, t=30..100 | jemalloc (section 4.1) | 901k | 3232 | 9835 | 0.33 | 39.7 | 38.8 | 7.1 | 100.1% | 0.183 |
| write, under `perf record`, t=100..161 | challoc (73b6df1) | 740k | 3285 | 9763 | 0.34 | 43.5 | 58.3 | 6.9 | 100.1% | 0.227 |
| write, under `perf record`, t=100..161 | jemalloc (section 4.1) | 714k | 3407 | 9725 | 0.35 | 41.0 | 43.0 | 6.7 | 100.1% | 0.222 |
| delete, under `perf record`, t=400..461 (300 s write phase) | challoc (73b6df1) | 1066k | 2288 | 7261 | 0.32 | 24.2 | 25.5 | 5.0 | 100.1% | 0.240 |

The write windows are 3.8% (unrecorded) and 3.6% (recorded) fewer cycles
per command than bundled jemalloc's, in windows with the same heap
history and generator schedule; against 5304cd4's recorded window (3313,
110 s later in the schedule) the tweak is within the window-to-window
spread, as one discarded L2 load should be. `libchalloc.so` is 13.92% of
the write record and 12.53% of the delete record (13.58% / 12.77% at
5304cd4). The records' symbols did not resolve after the Valkey rebuild
re-staged the library (a build-id mismatch in perf's cache), so there is
no per-symbol split for this run.
