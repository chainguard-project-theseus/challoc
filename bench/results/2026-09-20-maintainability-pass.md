# Maintainability pass, 2026-09-20: what changed in the machine code, and the numbers

The cleanup described in `docs/review-2026-09-20-maintainability.md` ran in
three batches. This file is the record CLAUDE.md asks for when `src/`
changes: the codegen comparison at each batch boundary and the quick-tier
rows on the reference pair. Baseline: `ea9532a` (library `e60527d`).

## Batch 1 (comments, tests, dead code, tooling): byte-identical

`scripts/codegen-diff.sh compare` at every merge and at the batch end
(`ddaf3b4`): `.text` hash identical in both variants (q16
`91f2b6b9ca6c5059`, q8 `de1d548d53808c5c`), every section the same size.
No benchmark needed.

## Batch 2 (module splits): every function exact or relocation-only

At `911a1ed` (heap/, class/, huge/, purge/ and sys/abort.rs split, both
variants): 435 functions, 116 exact, 319 reloc-only, 0 changed; `.text`
sizes identical (`03bea0`, `03c1c0`). Two toolchain effects seen and
reverted along the way are recorded in the script's header (the A53
erratum veneer, statics and the merged-globals blob).

Quick tier, `bench/gce/challoc-bench quick <arch> OLD=ea9532a` (one round,
working tree = `911a1ed`; tolerance 5%):

aarch64 (C4A, us-east4-a):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 41.7 (15.8 MiB) | 41.9 (15.9 MiB) | 45.1 (18.5 MiB) | -0.5% | +7.6% |
| larson10 | M ops/s | 364.7 (112.2 MiB) | 367.2 (113.2 MiB) | 386.3 (131.6 MiB) | -0.7% | +5.2% |
| xmalloc | M frees/s | 578.046 (53.1 MiB) | 576.362 (53.0 MiB) | 480.247 (80.2 MiB) | +0.3% | -16.7% |
| alloc-test1 | s | 2.823 (13.0 MiB) | 2.796 (13.0 MiB) | 2.592 (12.3 MiB) | -1.0% | +7.9% |
| valkey1 | M ops/s | 6.603 (584.3 MiB) | 6.559 (584.3 MiB) | 6.584 (585.3 MiB) | +0.7% | +0.4% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts (end occupancy >= 90%) | 23928366 ops/s |

x86_64 (C4, us-east4-a):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
| larson1 | M ops/s | 42.5 (16.4 MiB) | 42.6 (16.9 MiB) | 40.2 (17.4 MiB) | -0.2% | -5.6% |
| larson10 | M ops/s | 279.3 (96.3 MiB) | 273.1 (92.9 MiB) | 299.9 (94.3 MiB) | +2.3% | +9.8% |
| xmalloc | M frees/s | 344.137 (52.7 MiB) | 343.038 (52.6 MiB) | 333.264 (89.0 MiB) | +0.3% | -2.8% |
| alloc-test1 | s | 3.450 (14.0 MiB) | 3.437 (14.0 MiB) | 3.222 (13.1 MiB) | -0.4% | +6.7% |
| valkey1 | M ops/s | 14.711 (585.3 MiB) | 14.267 (585.3 MiB) | 13.716 (592.1 MiB) | +3.1% | -3.9% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 6798 -> 6678 (49.8%) | new compacts (end occupancy >= 90%) | 20832760 ops/s |

Reading: go on both. The x86_64 larson10 (+2.3%) and valkey1 (+3.1%)
rows are the widest; with every function's instructions identical at this
commit they can only be placement or one-round noise, and the scorecard
after batch 3 says whether they sit in their historic bands
(`bench/results/scorecard-x86_64.md`; the x86_64 larson16 band is
+6..13%, valkey1's is wide too).

## Batch 3 (deduplication and the requested behaviour fixes)

Six lanes (heap, class, huge, purge, api, sys), each with a per-function
comparison against the tree it started from and a `make test` run;
every changed function is named with a reason in the lane's PROGRESS.md
bullet. The rule held: no per-allocation function changed (`malloc`,
`free`, `calloc`, `sdallocx`, `alloc_fast`, `alloc_zeroed`, `pop`/`push`,
`classify_free`, `sized_slot`, `checked_slot*`, `claim_with`, `take`,
`release_owned`, `release_word`, `dec_live`, `locate`, `check_*`, the
rseq sections, the TLS accessor, the lock fast path); the two items that
would have changed one (`Pick::EMPTY` at the refill site, the set/count
helpers inside `claim_with`) were kept as they were.

Size: `.text` q16 `03bea0` -> `03cac0` (+0xc20, about 3 KiB: the config
key table's closures and the mallctl readers replacing a match, minus the
dead code), `.data.rel.ro` `002d28` -> `003680` (the two tables).

Behaviour changes, each with a test: the huge descriptor table aborts
before its last slot fills (`find` always terminates); `Cgroup::events()`
samples all five `memory.events` counters; `stats.arenas.4097.*`
(`MALLCTL_ARENAS_DESTROYED`) answers zeros; a corrupt owner word on the
cross-arena free path aborts instead of materialising an arena;
`opt.tcache_max` and `opt.huge_retain_max` read as `size_t`; the huge
run's page count comes from its descriptor and the arena's run/page
counters are one word with an exact subtraction.

Final `make test` on `main` (208c23c): 473 + 136 + 26 passed, every C
step `ok`.

Final comparison against `ea9532a` (whole pass, `scripts/codegen-diff.sh
compare`): q16 `.text` `03bea0` -> `03cac0`, `.rodata` `005fc0` ->
`005f9c`, `.data.rel.ro` `002d28` -> `003680`; 435 -> 536 functions,
instructions 61445 -> 62221 (+1.3%). The whole-pass match is 31 exact +
322 reloc-only + 183 unmatched, but that number is not "183 functions
changed": the 101 new functions (the config key table's parse/read
closures, the mallctl readers) have no baseline counterpart and every
inlined callee they replaced counts once more. The attribution that
matters is per batch: batch 2 zero changed; batch 3 every changed
function named in its lane's PROGRESS.md bullet, none on a per-allocation
path.

## Scorecard for the final tree

`bench/gce/challoc-bench scorecard` on both reference VMs, 3 rounds, tree
`3c6602b` (the lines are in `bench/results/scorecard-{aarch64,x86_64}.md`
tagged `3c6602b+`: the VM-side `tree_dirty` check reported uncommitted
`src/` changes although the local tree was clean at the sync; the cause
was not established and is worth one `git status` on a VM after a sync).

aarch64, against the four earlier lines of 2026-09-20 (856f103, 8e2d201,
a999097, 5a1e715): every column inside their spread: larson1 +7.7%
(+7.4..+8.4), larson10 +6.3% (+5.0..+7.3), larson16 +3.6% (+3.7..+5.4;
better), xmalloc -17.2%, alloc-test1 +6.5% (+5.2..+7.5), alloc-testN
+4.6%, valkey1 +0.5% (-0.6..+1.4), valkey1-free -1.1%, cfrac +1.1%,
espresso -2.4%, mstress -41.4%, glibc-simple -14.2%, redis -17.3%,
python1 +0.2%, python1-malloc -0.1%; RSS ratios 0.85 / 1.00 unchanged.

x86_64, same comparison: larson1 +1.4% (-1.8..+0.9; the row's history
spans -8.8..+4.9), larson10 +6.8% (+1.1..+5.2 today, +8.4 in the history),
larson16 +12.8% (+7.9..+12.6 today, +12.9 in the history), xmalloc +1.0%,
alloc-test1 +6.8% (+6.5..+6.7), alloc-testN +8.9% (+8.2..+10.0), valkey1
-1.6% (-2.9..+0.4), valkey1-free -11.6%, cfrac +4.5%, espresso -0.3%,
mstress -40.2%, glibc-simple +2.1%, redis -21.2%, python1 -0.7%,
python1-malloc +0.4%; RSS ratios 1.06 / 0.99 unchanged.

Reading: no row leaves its band on either architecture. The x86_64
larson rows land at the high end of theirs, as the quick tier's larson10
did; with every per-allocation function byte-identical to the baseline
that is placement or the row's own noise (the same code scored +1.1% and
+8.4% on different days). If the next scorecards keep larson10 and
larson16 at the top of the band, `ab old=ea9532a new=HEAD ROUNDS=5` on
amd64 is the protocol-tier tie-breaker.
