# challoc maintainability review, 2026-09-20

A top-down reading of the tree at ea9532a (library code e60527d) with one
question: is the code well structured and well tested, and what would make
it easier to maintain and extend without changing what it does or how fast
it runs. Eight read-only reviews ran in parallel, one per module group
(heap; class/slab/bitmap/arena; huge/slice/layout/meta;
purge/policy/thp/stats/config; api; sys/rseq/tls/lock; the test
architecture; build and bench tooling). This file is the synthesis and the
plan the cleanup lanes execute. The two earlier reviews are
`review-2026-09-17.md` (performance) and `review-2026-09-18.md`
(pre-release); section 3 calibrates against the second.

## 0. Summary

**Structure: sound at the module boundary, oversized inside it.** The
module map in design 19 holds: every file has the responsibility the
design says, the fast paths are where the design puts them, the syscall
layer is one place, the API layer has one consistent three-layer shape
(Rust logic, `extern "C"` wrappers, tests). The problem is file size and
what accumulates inside a file that is never split: `heap.rs` is 7265
lines holding five concerns (caches, `Shared`, `Heap`, the per-CPU mode,
thread lifecycle) plus 3336 lines of tests; `class.rs` is 5016 lines
holding the slab table, the sets, the claim/release protocol, the group
handoff, decay, prefault, the zero-check machinery and sixteen abort
functions plus 2213 lines of tests; `purge.rs`, `huge.rs`, `arena.rs`
each hold two to four separable things. Nothing is in the wrong file;
several files are four files.

**Tests: strong on logic, uneven on plumbing.** 554 Rust tests (411
host-runnable pure logic, 120 in-process api tests, 23 cgroup) plus 45 C
programs under `LD_PRELOAD`, two vendored suites, a fuzzer and an
exhaustive interleaving model of the slab protocol. The allocator's
invariants are exercised from both sides (a full-structure audit after
every huge-tier operation, brute-force references for size classes and
pointer classification, 53 abort paths under `should_panic`). The
weaknesses are architectural, not coverage: the C-test driver is 76
hand-written functions with copied boilerplate that recompile the same
program per mode; kernel-gated tests skip silently and print green (the
end-to-end guard-slab fault test has never run on a local kernel); test
helpers are copied across files (eight `xorshift`, five `page_size`, four
synthetic-region builders, three mmap fixtures); the model is coupled to
the protocol it models by prose only; and nothing documents the test
pyramid or says which targets `make test` does not run.

**Code hygiene: the iteration log leaked into the code.** About 200 doc
comments cite plan task numbers, dates or "the human's decision"; a dozen
describe behaviour the code no longer has (a function that no longer
exists named three times, a SAFETY comment that is wrong about how a
state byte decodes, a division that became a shift, a lock order missing
two locks). Roughly forty `pub` items have no caller outside tests.
Every production `unsafe` block in the tree has a SAFETY comment, and none
was found wrong except the one above.

**Tooling: correct, repetitive.** The Makefile has no real duplicates
(the apparent ones are target-specific variables) but thirteen copies of
one `foreach` idiom and seven copy-paste consumer targets; five consumer
harnesses re-spell the same describe/read-result/report blocks; the
Dockerfile is one `RUN` that the last merge conflict broke; the GCE driver
has eleven hand-written failure lines with ragged fields.

**Verdict.** Maintainable today by the people who wrote it, expensive for
anyone else: the knowledge is in the files but a reader has to find it in
7000-line units under stale signposts. The cleanup below is large in
lines and small in risk, because almost all of it is provably
code-neutral.

## 1. Method and acceptance

The constraint is "no performance change". The release profile is fat
LTO, one codegen unit, line tables only. `scripts/codegen-diff.sh`
(added at 0f5dee4) captures both quantum variants' release `.so` and
compares a rebuilt tree against them at three levels:

1. `.text` hash identical: byte-identical machine code. Comments, tests,
   dead-code removal, visibility changes and constant consolidation land
   here. (The full `.so` hash is not usable: panic locations embed
   `file:line`, so a comment shifts `.rodata`.)
2. `.text` differs, every function matches a baseline function exactly or
   modulo relocation immediates, `.text` size identical: code moved
   between files (path strings change length, PC-relative offsets shift,
   instructions do not). Module splits land here.
3. Anything else is a real code change and goes through the repository's
   own protocol: `make quick` on both reference VMs and, for a fast-path
   file, the per-operation attribution and a scorecard.

The build is reproducible (a forced rebuild of the same tree hashes the
same), so level 1 is a proof, not a heuristic. Baseline at ea9532a:
q16 `.text` 91f2b6b9ca6c5059, q8 de1d548d53808c5c, 435 functions each;
`make test` 411 + 120 + 23 passed, cgroup and pagemap-guard steps `ok`.

## 2. Findings

### 2.1 heap.rs

Five concerns, 3929 lines of code, 2113 lines of host tests, 1223 of
Linux tests. Per-CPU mode (~950 lines: `Percpu`, `PercpuState`,
`CpuCache`, two `impl Heap` blocks, the push wrappers) is the clearest
candidate to leave; `Shared` (~900), thread lifecycle (~140) and the
flush-grouping algorithms (~85) follow. The `others` list walk is written
seven times; the "load `len`, zero means slow, else pop" shape four times
(`alloc_fast`, `alloc_zeroed`, `alloc_in`, the per-CPU off-mode arm);
the mode-to-mask rule once as a method and four times by hand in the
per-CPU code; the compaction probe gate twice; the flush split/group/
straddle-check twice (thread and per-CPU). `pop_compact`,
`pop_compact_run`, `percpu_scheduled` are at nesting depth six. 36 doc
lines carry iteration history; `CacheHeader`'s doc describes the design
the module doc says was rejected; `push_flags()`'s doc lists two of four
bits; one paragraph is glued to the wrong item. Test-only accessors left
`pub`: `slow_ticks`, `tid`, `shared`, five `CacheHeader` readers,
`DEFAULT_GC_INTERVAL_NS` (duplicating the config default). Item 30 of the
2026-09-18 review (two formulas for huge-run pages) is the one unfixed
correctness-adjacent item: `usable_pages(size) + GUARD_PAGES` at
allocation, `run.pages` at free, a `saturating_sub` in stats.rs where the
drift was seen.

### 2.2 class.rs, slab.rs, bitmap.rs, arena.rs

`class.rs` is five modules: the table (`TableInner`, `SlabTable`, 576
lines), the sets (`Lazy`, `Set`, `SetTable`), the protocol
(`ClassState` claim/take/release/reopen, the hot half), the handoff
(`transfer_group` at 100 lines), decay and prefault, the zero-check
machinery, sixteen `#[cold]` aborts, 2213 lines of tests. The
empty/purged set bits and their counts are two sources of truth written
inline at eleven sites while `partial` has a helper; the same
`checked_sub(1).unwrap_or_else(slab_count_underflow)` appears six times;
the empty-set walk is written three times; the group range twice more.
Stale: `ClassState::commit_next` is named three times and does not exist;
the `SlabDesc::at` SAFETY comment says garbage decodes as `Purged` when
zero is `Fresh` and above 3 aborts; `check_live` says "a 32-bit division"
that became a shift. `arena.rs` mixes arenas, explicit thread caches and
`MALLOCX_*` routing, and imports `heap.rs` which imports it back.
`slab.rs` and `bitmap.rs` are clean. The model (`model.rs`) refers to the
protocol by name only; a reordering in `class.rs` does not fail it.

### 2.3 huge.rs, slice.rs, layout.rs, meta.rs

`huge.rs` holds a self-contained open-addressing `Table`, an OS shim in
two cfg copies, the extent machinery and decay/stats, 47% tests. `take`
(98 lines, eight arguments) and `free_run` (108) each update four
structures (table keys, free bitmap, stats, empty list) at six-plus
sites; the audit that checks them is 70 lines away. The `mprotect(RW)`
commit shim exists three times (huge, meta, class). `slice.rs` carries
its own syscall numbers and two hand-written `asm!` syscall blocks
because `sys::raw` is private, escaping the header check that `nr.rs`
has. `layout.rs` mixes size-class math, address-space parameters and
the Linux reservation; the seam is clean. `meta.rs` is clean apart from
a stale identity in a doc and seven never-called constructors and
accessors. Coverage gap worth closing: `slot_in_class` (the sized-free
classifier) has no direct test.

### 2.4 purge.rs, policy.rs, thp.rs, stats.rs, config.rs

`purge.rs` says "three things" and lists four: curve, batching and
advice, decay driver, background thread (394 lines, self-contained
`pub mod`), prefault (200, likewise). `config.rs` is tests first (38%)
and docs second; each key is named in `set`, `get` and `KEYS`, plus
design 12, tuning.md and the mallctl MIB test, and nothing structurally
checks that `set` accepts only known names. Advice-to-text lives in
three places; policy state is mirrored into purge statics because
`Batch::flush` has no `&Shared`; the THP-forced rule spans five files.
Stale: "one slab per class per pass" (the code keeps eight), churn-test
numbers that disagree with the results file, an assertion message that
contradicts the field doc fixed two commits earlier. `spin_hint` has no
host test. `Decayer::decay` has no direct test.

### 2.5 api/

The split is right and every module has the same shape. Sixty-five
export wrappers are identical five-line bodies. `mallctl.rs`'s namespace
is a proper static tree but its behaviour is a 372-line `match` over 118
leaf variants, so a read-only stat is four edits. `print.rs` line
formats are literals re-spelled in the file's tests, seven C tests,
tuning.md, design 16 and a soak harness. errno constants are defined in
three places. `realloc` and `rallocx` share a 20-line shape. The fork
lock-order module doc omits the two locks the code added. The
malloc/free/sized-free per-CPU triplets are deliberately shaped and must
not be touched.

### 2.6 sys/, rseq.rs, tls.rs, lock.rs

`sys/mod.rs` is the documented grab bag; the abort machinery and `Stderr`
would read better on their own. The syscall wrappers should stay
hand-written: the casts are what a reviewer needs to see. The largest
vestige in the tree is `features::Rseq` and `probe_rseq` (150 lines
duplicating `rseq::probe`, with the constants declared twice and the
thread-pointer asm spelled two ways) which nothing reads. Four
`getauxval` externs, three open/read/close helpers, four decimal parsers,
three `yn`. The ten rseq asm blocks share a textually identical
descriptor prologue and per-arch abort handler. `Cpu::probe` and
`Features::probe` have global side effects (spin hint, cldemote
configuration) that belong in init. Seven constants have zero
references. `PIDFD_SELF` is dated 6.14 in six places and 6.16 in one
test. `MINCORE` escapes the header check.

### 2.7 Test architecture

The pyramid (host logic, container logic, api in-process, C under
`LD_PRELOAD`, negative, vendored, fuzz, model, cgroup containers, GCE
matrix) is complete and undocumented. `make test` is not "everything
local": it omits the 10M fuzz, the oscillation test, `check-needed` and
the allocator comparison. `ctests.rs` is 76 hand-written functions;
`negative.c` is compiled fifteen times per run. Silent skips:
`c_negative_sized_guard` and `c_guard_slab_fault` return green on every
kernel before 6.13, which is every local kernel; `c_fork_cost_thp_modes`
needs an environment variable nothing sets; eight program-level
`skip:` exits print `ok`. The cc flag line is spelled in eleven places
and the shell copies hard-code `-DQUANTUM=16`, so a quantum8 build fails
every script-driven C test. `common.h` lacks the helpers seven programs
re-implement (`sleep_ms`, RSS reader, fork helper, weak jemalloc
externs); three programs bypass it. `fork_cost.c` is a measurement
program in the test suite. Review item 43's list: two done, two partial,
five open.

### 2.8 Tooling

Makefile: no real duplicates, thirteen `foreach` copies, seven
copy-paste consumer targets, three shell probes on every invocation, no
`help`, `percpu-campaign` missing from `.PHONY`, `make bench` legacy with
no callers. Dockerfile: one `RUN`, comments repeated three times outside
it because comments inside the continuation once broke a merge, `rust`
unpinned while the Wolfi package pins 1.98. Shell: three `set`
disciplines by design, two sidecars with none; identical describe,
result-reading, rounds-loop and report blocks in four or five harnesses;
`valkey-soak.sh` sources `lib.sh` and redefines five of its variables.
`challoc-bench`: sound dispatch, eleven hand-written `MATRIX ... FAIL`
lines with ragged fields, seven undocumented knobs, a stale CPU name.

## 3. Calibration: the 2026-09-18 list

Of the 43 items: P0 1-8 all done or done by recorded decision (3:
overflow checks priced and declined, mitigations in place; 4: clamp
counted and printed, no abort, rationale in code). P1 9-16: all done
except 16 (`Shared::owner` still materialises an arena on the free
path). P2 17-22: all done. P3 23-30: done except 25 (per-CPU mode/mask
still written by hand at four sites) and 30 (huge page formulas). 31
(vestigial) partial: `try_lock` and `Tcaches::len` are live now, the
`check` pair and the constants remain. 32 (history in docs) partial:
the named lines fixed, about 200 others remain. 33 (stale prose) done
except three sentences. 34 (complexity) partial. P4 35-43: 35, 38, 42
done; 41 partial (four of six); 43 partial (two of nine). The cleanup
below closes 25, 30, 31, 32, 33, 34's remaining pieces and 43; 16 and
41's remainder are behaviour questions for the human.

## 4. The plan

Three batches, each in parallel lanes over disjoint files, merged one at
a time with `scripts/ralph-lane.sh --merge`, `make test` at every merge.

**Batch 1, code-neutral (level 1: `.text` identical).** Eight lanes:
tidy-heap, tidy-class, tidy-huge, tidy-purge, tidy-api, tidy-sys,
tidy-tests, tidy-tooling. Every stale or historical doc comment
rewritten to say what the code does; every dead or test-only `pub`
deleted, privatised or `#[cfg(test)]`; the missing unit tests from
section 2 added; test helpers deduplicated within files; errno constants
consolidated; `MINCORE` header-checked. Tests: the C-test driver becomes
a table with one compile per program, skips become a structured `SKIP`
line that fails unless allowed, one cc flag script, `common.h` gains the
shared helpers, the nine item-43 tests, `fork_cost.c` moves to
bench/probes, the model becomes a directory, `docs/testing.md` documents
the pyramid. Tooling: Makefile macros with `make -n` byte-identical,
`help`, a layered Dockerfile with an identical package set, `lib.sh`
helpers with byte-identical result text, `matrix_fail`, documentation of
the undocumented knobs.

**Batch 2, code motion (level 2: every function exact or reloc-only,
`.text` size identical), then `make quick` on both reference VMs and one
scorecard.** `heap/` (percpu, shared, thread, flush, tests with a shared
`pub(crate)` test-util); `class/` (table, sets, handoff, decay, prefault,
verify, abort, tests); `huge/` (table, tests); `purge/` (background,
prefault, curve); `arena.rs` routing to its own module, breaking the
heap/arena import cycle; `sys/abort.rs`; constants hoisted out of
`mod linux` so the `purge.rs` mirror of `PIDFD_SELF` goes; a crate-wide
`#[cfg(test)]` test-util for `xorshift`, `page_size`, `align_up`, the
mmap fixture and the synthetic-region builder; design 19 rewritten to
match.

**Batch 3, deduplication with proof.** Each item lands only if its
functions are exact or reloc-only, or if it is off the per-allocation
path and `make quick` holds: the set/count helpers in `class::Inner`;
`walk_empty` and `min_nonzero`; `transfer_group` split at its invariant;
`probe_verdict` and a volatile `set_mode` for the per-CPU sites; the
`others` iterator for the six cold walks; `Huge::alloc` returning the
pages it placed (item 30) with `add_free_extent`/`drop_extent`; one
`sys::commit`; `slice.rs` numbers into `nr.rs`; `features::Rseq`
deleted; `sys::auxv`; `read_small_file`; the feature-line key table;
`Advice::name()`; `policy::advice(headroom)`; table-driven `KEYS` and
the read-only half of `ctl()`; `export_pair!`; the rseq descriptor and
abort-handler macros (byte-identical or dropped); `alloc_zeroed` through
`alloc_fast` (byte-identical or dropped); `json_report` split; the
inert-key plumbing reduced to its one key.

**Not done here, for the human (behaviour, not maintenance).** Item 16
(a non-materialising owner lookup on the free path); `Cgroup::events()`
returning a partial struct; `opt.tcache_max` typed `unsigned`;
`MALLCTL_ARENAS_DESTROYED` defined in both shipped headers and
implemented nowhere; `arena.<i>.destroy` documented as a purge;
`Batch::push`/`flush` as safe functions with an unsafe precondition;
`c_fork_cost` as a test with no criterion; the probes' side effects
moving into init (behaviour-identical but observable in test ordering).

## 5. Acceptance for the pass

Batch 1: `.text` hash identical in both variants at every merge; `make
test` green with at least the baseline counts. Batch 2: zero changed
functions, `.text` size identical, `make quick` within noise on both
VMs, one scorecard line per architecture. Batch 3: per item as above.
At the end: every lane worktree merged and clean, design 19 and
`docs/testing.md` current, one PROGRESS.md entry per merged lane, this
file's section 6 recording what landed and what was dropped.
