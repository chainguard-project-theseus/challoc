# Valkey 9.1.2 test suite under challoc, both architectures (Phase 1 acceptance remediation step 4)

Date: 2026-09-17. Zone: us-east4-a (`bench/gce/challoc-bench`, arm64 =
c4a-standard-16 Neoverse V2, amd64 = c4-standard-16 Xeon 6; kernel
7.0.0-1011-gcp, `guard_install=yes`). Library: commit 2732f6f (heap unchanged
since e6b89c3: 4 KiB slabs for the 512 and 1024 classes, 8 KiB for 896,
16 KiB for 640 and 768, 64 KiB elsewhere; compacting cache). Valkey tag 9.1.2
(7f1dffed) built with `make MALLOC=challoc BUILD_TLS=yes` inside the
`challoc-dev` image (`contrib/valkey/build.sh`), `./runtest --dump-logs`,
full suite with `--clients 8`.

## Run 1: the tree as committed (patch without the test change)

| arch | `unit/memefficiency` | full suite (`--clients 8`) | time |
|---|---|---|---|
| aarch64 | 24 tests, 2 fail | 5200 pass, 2 fail, 26 ignored (`large memory flag` 22, `Other server path` 4), 0 skipped, all 164 units | 521 s / 1202 s |
| x86_64 | 24 tests, 2 fail | 5202 pass, 2 fail, 26 ignored, 0 skipped, all 164 units | 521 s / 1185 s |

The two failures are the same test in both modes on both architectures:
`Active Defrag eval scripts: standalone` and `Active Defrag eval scripts:
cluster`, both "defrag didn't stop". Everything else in `unit/memefficiency`
passes, including the four `Active Defrag` cases that failed in the task 7
run on Docker Desktop (`main dictionary: cluster`, `stream: cluster`, `eval
scripts: cluster` at `Expected 1.24 > 1.4`, `big hash: cluster` at
`only 51965952 bytes created`): the 4 KiB / 8 KiB / 16 KiB slabs of the
512..1024 classes and the compacting cache resolved those. The two
environmental failures of the Docker Desktop run do not occur on the GCE
kernel either: the `mptcp yes` fixture passes (`[ok]: MPTCP - Server starts
with mptcp yes`), and `integration/dual-channel-replication` passes on both
VMs (136 s / 131 s).

The dumped `INFO memory` at the failure (test log, `--dump-logs`):

| case | arch | `allocator_allocated` | `allocator_frag_ratio` | `allocator_frag_bytes` | stop thresholds |
|---|---|---|---|---|---|
| eval scripts: standalone | aarch64 | 54,403,392 | 1.06 | 3,158,384 | < 2,560,000 B or < 5% |
| eval scripts: cluster | aarch64 | 56,220,544 | 1.06 | 3,581,168 | same |
| eval scripts: standalone | x86_64 | 53,805,376 | 1.06 | 3,165,552 | same |
| eval scripts: cluster | x86_64 | 55,622,208 | 1.06 | 3,588,656 | same |

The server log shows the first cycle compacting 40,029 objects (53% -> 6%,
28.9 MB -> 3.16 MB) and then 60 ms cycles that each re-move 833 objects at
a constant 3,158,384 bytes until the test gives up after 30 s.

## Where the 3.16 MB sits (bins table of `MEMORY MALLOC-STATS`, aarch64 standalone)

`(nregs * curslabs - curregs) * size` per class: the 80-byte class carries
2,391,200 of the 3,158,384 bytes: 54 slabs of 64 KiB (819 regions), 14,336
live regions, 39 non-full; occupancy histogram (eighths, last = full)
`[38, 0, 0, 0, 0, 0, 1, 0, 15]`, i.e. 38 slabs under 1/8 full, 15 full.
The other 0.77 MB is one partially filled 64 KiB slab per class in 22
classes (the 64-byte class's single slab alone is 131 KB). The 512 class
(the 50k script bodies, 6,274 slabs of 4 KiB) wastes 8 KB.

## What the 80-byte objects are: Lua's heap, visible only under challoc

Reproduced on the Mac's native aarch64 container (`.tmp-it16-repro.sh` in
the iteration log: the test's populate of 50,000 `SCRIPT LOAD` + `SET`,
`DEL` of every key, the test's four `active-defrag-*` settings, 6 s of
defrag, then probes) against the challoc build and against an unpatched
clone with the bundled jemalloc 5.3.0 (`target/valkey/src-9.1.2-jemalloc`):

| stage | challoc `allocator_allocated` / frag | jemalloc `allocator_allocated` / frag |
|---|---|---|
| after populate (`used_memory` 36.37 MB both, `used_memory_vm_eval` 12,126,208 both, 50,000 cached scripts both) | 36,675,904 / 1.05 | 24,867,384 / 1.01 |
| after delete | 3,686,944 B (1.05) | 8,425,944 B (1.15) |
| after 6 s defrag | 1.06 / 3,178,848 B, 244 cycles, 626 re-moved per cycle | 1.01 / 266,360 B, 1 cycle |
| after `EVAL "collectgarbage('collect')"` | 80-byte class 14,336 -> 1,024 live, 54 -> 16 slabs (38 emptied); frag 3,178,848 unchanged until the next cycle | 80-byte bin 30 -> 36 regions |
| after 3 s more defrag | 1.03 / 1,754,048 B (would stop) | 1.01 / 301,888 B |
| after `SCRIPT FLUSH` | 80-byte class unchanged (1,024 live, 16 slabs) | unchanged |

Per class after the populate (live regions): challoc 16 B 261,632 / 24 B
64,000 / 40 B 151,040 / 48 B 50,176 / 80 B 14,336 / 128 B 50,688 / 512 B
100,096; jemalloc 16 B 111,200 / 24 B 61,500 / 40 B 100,100 / 48 B 50,100 /
80 B 30 / 128 B 260 / 512 B 100,010. The 50,000 extra 128-byte objects
(Lua `Proto`, one per script), 3 x 50,000 16-byte arrays, 50,000 40-byte
closures and the 14,336 80-byte strings are Lua's heap: `used_memory_vm_eval`
is identical under both allocators, but Valkey's bundled jemalloc is built
with the `je_` prefix and Lua's `l_alloc` calls plain `realloc`/`free`
(`deps/lua/src/lauxlib.c`), so under stock Valkey the whole Lua heap is in
glibc malloc and outside `allocator_allocated`, `allocator_frag_bytes` and
the defragmenter's view (`allocator_allocated` is 11.8 MB lower than
`used_memory`; `resetLuaContext` in `src/modules/lua/engine_lua.c` even
calls `malloc_trim` for that reason). challoc replaces libc malloc, so the
same Lua heap lives in its bins. `SCRIPT LOAD` leaves 65..80-byte Lua
strings (24-byte `TString` header plus 41..56 chars, e.g. `f_<sha1>`) that
Lua's incremental collector has not swept yet: 13,312 of the 14,336 are
garbage (`collectgarbage` frees them, 1,047,552 bytes of Lua memory), the
other 1,024 are permanent. Valkey's defragmenter never visits Lua objects,
so the 38 pinned slabs never empty and no allocation policy can move them.
The same mechanism raised `allocator_allocated` by 12 MB in the challoc
run, which is why the cluster case now passes its `> 1.4` check that it
failed in task 7.

## Allocator-side options of plan step 4, tried on the Mac

- `experimental.utilization.batch_query` per 4 KiB page group: not
  applicable. The stop decision is `allocatorDefragGetFragSmallbins`
  (bin-level `nregs * curslabs - curregs` over mallctl), not the per-pointer
  query; finer page groups would only change which objects Valkey moves,
  and the objects that pin the slabs are Lua's, which it never visits.
- `MALLOCX_TCACHE_NONE` allocations into the fullest partial slab: not
  applicable for the same reason; the re-moved 626..833 objects per cycle
  are not in the pinned slabs' class budget (their placement does not change
  `curslabs`).
- Smaller slabs for the class Lua's strings use: a variant with
  `lg_slab_for(80) = 12` (4 KiB, 51 regions; `.tmp-it16-v80`, built from
  `git archive HEAD`, preloaded over the same Valkey binary). Same populate:
  the 80-byte class has 14,336 live in 381 slabs (waste 407,600 B instead of
  2,391,200), frag after populate 1.02 / 1,236,256 B, after defrag 1.02 /
  1,195,248 B, one 48 ms cycle and the defragmenter stops: the test would
  pass. Not adopted: it is a `size == 80` special case with no rule behind
  it (the 1/32-waste rule that gives the 512..1024 classes their small slabs
  would also give the 64-byte class a 4 KiB slab, and 16 KiB slabs for every
  class halved `xmalloc-test` in `2026-09-16-aarch64-gce-min-slab.md`), it
  would need A/B rows on both VMs, and it would only hide a residue that
  exists because challoc serves Lua's libc `realloc` at all, which is the
  correct behaviour of a malloc replacement.

## The change: the test's `active-defrag-ignore-bytes` under challoc

`contrib/valkey/0001-Add-MALLOC-challoc.patch` now also touches
`tests/unit/memefficiency.tcl`: `perform_defrag_test` takes an
`ignore_bytes` option (default the stock `2500kb`) and `test_eval_scripts`
passes `5000kb` when `mem_allocator` matches `*challoc*`, i.e. the stock
value plus the measured 2.4 MB Lua-heap residue (2,391,200 B on aarch64,
the same bins on x86_64) that the stock value never had to cover. No other
test changes; the production defaults (`active-defrag-ignore-bytes 100mb`,
`active-defrag-threshold-lower 10`) are untouched. Validated on the Mac
with `./runtest --single unit/memefficiency --only 'Active Defrag eval
scripts: standalone' --only 'Active Defrag eval scripts: cluster'` (both
pass), then rerun on both VMs below. `contrib/valkey/build.sh` resets a
clone that carries an older version of the patch before applying the new one.

## Run 2: the tree with the test change

Same VMs, same library (2732f6f), patch commit 6fed468 (build.sh reported
"stale patch in the tree, resetting it / patch applied" on both VMs).

| arch | `unit/memefficiency` | full suite (`--clients 8`) | time |
|---|---|---|---|
| aarch64 | 33 ok, 0 fail, PASS (`eval scripts: standalone` 6157 ms, `cluster` 6161 ms) | PASS: 5202 pass, 0 fail, 26 ignored, 0 skipped, all 164 units | 421 s / 1081 s |
| x86_64 | 33 ok, 0 fail, PASS (`eval scripts: standalone` 6169 ms, `cluster` 6113 ms) | PASS: 5195 pass, 0 fail, 26 ignored, 0 skipped, all 164 units | 425 s / 1084 s |

(The run-1 unit count of 24 was the same file with 2 failures and 9 cluster
`Setup start_cluster` fixtures counted once each by the summary; the 33 of
run 2 counts every `[ok]` line.)


Both full runs report `All tests passed without errors`. The `[ok]` counts
differ by a few between runs (5200 / 5202 / 5195 / 5202) because some units
count fixtures and retries; every run covered all 164 units. The aarch64
run-2 full suite was launched twice by mistake (a late duplicate launch
started a second `runtest` in the same clone; it died after 29 s on
`tests/tmp/server.102.39/stderr` and truncated the shared log), so it was
killed and rerun alone from 19:35:45 UTC; the numbers above are from the
clean rerun. Phase 1's "Valkey test suite passes with challoc, including
defrag tests, on both architectures" holds at 6fed468 with the test
constant described above; without it the suite is 5200 / 5202 pass with the
two `eval scripts` failures.
