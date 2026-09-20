# tidy-heap: batch three, deduplication with proof

## Done

- Maintainability pass, batch three (lane tidy-heap, 2026-09-20): the seven
  dedup items of `src/heap/*`, `src/arena.rs`, `src/stats.rs` landed in five
  commits (5615f0a, 06994e8, 52a573c, 5f5e04c, 0758dd9), cold and scheduled
  paths only. (1) `Heap::nodes(head)` iterates an `others` list for the six
  cold walks (`stats`, `set_tcache_enabled`, `destroy`,
  `flush_thread_caches`, `cache_in`, the GC); `other_caches` on the
  cross-arena free path keeps its loop. (2) `Heap::probe_verdict(shared,
  arena, class, prev_refilled) -> (u32, Verdict)` is the compaction probe
  gate `pop_compact` and `percpu_scheduled` each spelled out;
  `CacheHeader::mask_for` is the one mode-to-mask rule (review 2026-09-18
  item 25), applied by `set_mode` and the new `set_mode_volatile` at the
  four per-CPU store sites. (3) Item 30: `Huge::alloc_run` returns `Placed
  { ptr, zeroed, pages }` with the pages read from the descriptor `take`
  wrote; `Shared::alloc_huge` adds those to the arena; the arena's
  `(large_runs, large_pages)` is one `AtomicU64` (runs high, pages low) so
  a reader's pair is always consistent, and `stats::collect_arena_with`
  subtracts the guards exactly behind a release `assert!` (no
  `saturating_sub`). (4) Item 16: `Shared::owner` resolves through
  `Arenas::get_existing` and aborts "arena N does not exist" for an index no
  arena was built for; `ensure_cache` validates the arena before creating an
  `others` node; the two paths that legitimately meet an unbuilt automatic
  arena (`Shared::alloc_huge` on a heap whose first allocation is huge,
  `percpu_ensure` at an id's first header install) resolve it through
  `Shared::arena`. (5) `Heap::pop_fast(class, flags) -> Option<(ptr,
  zeroed)>` is the one "load `len`, zero means slow, else pop" body behind
  `alloc_fast`, `alloc_zeroed` and the per-CPU off-mode arm. (6)
  `Shared::set_slot` is the bit test `checked_slot` and `checked_slot_any`
  share. (7) `Flags`, `Tcache`, the `MALLOCX_*` constants and builders and
  the `alloc`/`alloc_huge`/`free` routing moved from arena.rs to
  `src/heap/mallocx.rs`, re-exported by arena.rs so `api/` is unchanged.
  Proof (`scripts/codegen-diff.sh compare .tmp-codegen`, baseline 911a1ed):
  tasks 5 and 6 are `.text` hash IDENTICAL in q16 and q8 against the tree
  before them; task 7 is `.text` size identical, 436 functions exact or
  reloc-only, 0 changed. The final tree against 911a1ed: q16 `.text`
  0x3bea0 -> 0x3bec0, 435 -> 436 functions (`Huge::alloc_run` is now its
  own symbol), exact 58, reloc-only 354, changed 24; q8 exact 44,
  reloc-only 366, changed 26. No per-allocation path is in either list
  (`malloc`, `free`, `calloc`, `sdallocx`, `alloc_fast`, `alloc_zeroed`,
  `pop`/`push`, `classify_free`, `sized_slot`, `checked_slot*`,
  `percpu_pop_raw`, `percpu_push_raw`, `percpu_pop_miss`, `refill` all
  exact). The changed functions, with why (`.tmp-codegen/report-q16.txt`
  has the diffs): `Heap::pop_compact` (probe_verdict; owner),
  `Heap::rebuild_run`, `Heap::fragmented`, `Shared::free_direct`,
  `Shared::release_routed`, `Shared::free_huge` (owner: one atomic load and
  a null test instead of the inlined `Arenas::get` and its `materialise`
  call), `Heap::ensure_cache` (owner, and the refill counter read before
  `cache_slot`), `Heap::destroy`, `Heap::set_tcache_enabled`,
  `Heap::materialise_stats`, `Heap::tick` (the GC walk), `Tcaches::flush`,
  `api::mallctl::flush_caller`, `malloc_trim` (they inline
  `flush_thread_caches`/`stats`: the `nodes` iterator reads `next` before
  the body, one instruction moved), `percpu_scheduled`, `percpu_run_pop`
  (probe_verdict, set_mode_volatile), `percpu_ensure` (set_mode_volatile;
  `Shared::arena` before the stack allocation), `Huge::alloc_run` vs the
  old inlined `Shared::alloc_huge` (returns the descriptor's pages; now a
  separate symbol) and the new 64-instruction `Shared::alloc_huge`
  (resolves the arena first, adds `r.pages`), `api::libc::alloc_huge` (now
  inlines the small `Shared::alloc_huge`, including the `large_word`
  range assert), `Arena::init` (one `AtomicU64` store for two),
  `stats::collect_arena_with` (the exact subtraction and its assert),
  `Arenas::materialise` (LTO re-inlined the under-lock re-check now that
  `Shared::owner` no longer calls `Arenas::get`; cold, arena creation),
  and three toolchain entries that are trailing `nop` padding only
  (`core::slice::sort::...::panic_on_ord_violation`, and in q8
  `core::fmt::Display::fmt` and `__aarch64_ldset8_acq`). Tests: host
  `cargo test --features std` 350 passed (341 before); container `make
  test` 450 + 133 + 25 passed (main 441 + 133 + 25), cgroup limit/churn/lazy
  and pagemap-guard steps `ok`. New tests (heap/tests.rs):
  `others_nodes_iterate_newest_first_and_survive_freeing`,
  `probe_verdict_applies_the_gate_and_records_it`,
  `set_mode_volatile_derives_the_same_mask_as_set_mode`,
  `huge_run_pages_are_counted_from_the_descriptor`,
  `a_free_routed_to_an_unbuilt_arena_aborts` (plants an owner word for an
  unbuilt arena, expects the abort), `a_huge_first_allocation_builds_the_home_arena`,
  `a_huge_allocation_in_an_arena_that_does_not_exist_aborts`,
  `pop_fast_pops_the_top_with_the_zero_flag`,
  `checked_slot_any_is_checked_slot_without_the_arena_test`; the stats
  lifecycle test also pins `arena0.large()` to the descriptor's pages.

## Notes for the human

- Design 19 needs one line for the new module, under `src/heap.rs` (or the
  heap/ list once it exists there): `src/heap/mallocx.rs   MALLOCX_* flag
  decoding (Flags, Tcache) and the routing of mallocx/dallocx/sdallocx onto
  heaps, explicit caches and the direct paths (design 13.2); arena.rs
  re-exports it`. The heap module doc's file list already carries it.
  arena.rs still imports `Heap` and `Shared` (for `Arenas::get` and
  `Tcaches`), so the heap<->arena import cycle is thinner, not gone.
- No lock moved files (heap/mallocx.rs holds none); the fork-lock inventory
  in api/mod.rs is untouched and `fork_lock_list_matches_the_source_tree`
  passes.
- Task 5, partly: routing `alloc_zeroed` through one merged `match` on the
  shared `Option` was not byte-identical (`calloc` +9 instructions,
  `jemalloc::alloc_with` +13, `.text` +0x60) and was reverted; the landed
  form keeps `alloc_zeroed`'s early-return shape and calls `pop_fast` in
  the thread arm, which is `.text` IDENTICAL. `alloc_fast` and the per-CPU
  off-mode arm go through `pop_fast` too.
- Task 3: `Huge::alloc` keeps its `(ptr, zeroed)` shape as a `#[cfg(test)]`
  shim because huge/tests.rs (lane tidy-huge's file) returns it as such;
  tidy-huge may fold it into `alloc_run`. The "alloc and free disagree"
  test could not be constructed: `take` writes `pages = usable_pages(size)
  + GUARD_PAGES`, the same formula the heap used, so the two never differed
  for one run; the `saturating_sub` was guarding a torn read of two
  separate atomics during a concurrent alloc (runs incremented before
  pages), which the single word removes.
- Behaviour that moved, all on cold paths and unobservable except by the
  abort: `Shared::alloc_huge` resolves (and, for an automatic index,
  builds) the arena before placing the run rather than after; an index
  that does not exist aborts with the same message before the run is
  placed. `ensure_cache` reads the class's refill counter (one atomic
  load) on every slow path, not only when it creates the header.
  `Arena::large()` returns a consistent pair under a concurrent huge
  alloc/free where it could return a torn one.
- Not done from item 16: the per-slab-switch compare of the owner word
  with `desc.arena()` in `release_owned` is class.rs (lane tidy-class).
- The per-CPU changes (`percpu_scheduled`, `percpu_run_pop`,
  `percpu_ensure`) compiled and were compared but were not executed here:
  Docker on this Mac has no usable `mm_cid`, so `c_percpu_both_ways`
  SKIPs and the per-CPU tests skip. The coordinator's `quick` tier on the
  VMs (kernel with MM_CID) is the first run of that code.
