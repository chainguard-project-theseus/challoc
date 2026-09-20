# tidy-huge: batch three, deduplication on the huge tier, meta and layout

## Done

- Maintainability pass, batch three (lane tidy-huge, 2026-09-20): the six
  tasks of the lane, all landed; commits 3a8f2e6 (table), 9ea581a (huge),
  4d42f13 (meta), e50a694 (layout split), on `loop/tidy-huge` rebased on
  main 911a1ed. (1) `huge::Table::find` can no longer spin: `insert`
  aborts ("huge: descriptor table full", the existing message) when it
  would take the last slot, so the table always has the empty slot a
  probe for an absent key ends at; tests
  `the_table_never_fills_its_last_slot` (fills to `cap - 1` without
  `ensure`, absent keys still absent, remove/re-add at the limit) and
  `the_table_aborts_before_its_last_slot_fills` (the one batch one left
  unwritten). (2) `Inner::add_extent(d)` / `drop_extent(d)` / `retire(meta,
  d)` fold the table keys (one for a run, two for a free extent) and the
  counters; `take` (410 -> 307 instructions, six parameters instead of
  eight, `#[allow(too_many_arguments)]` gone) and `free_run` are rewritten
  on them; `take` takes a `Placement { start, pages, ext }` that `alloc`
  builds at the call; `HugeStats::add`/`sub` cover every state (the audit
  counts every extent through `add`); `HugeStats.table_cap` is filled in
  by `Huge::stats` from the table, no longer written at three sites.
  (3) `Table::layout(lg_cap)` is the one source of the storage `Layout`
  for `storage` and the new `release` that `ensure` calls. (4) meta:
  `impl Default`, `Meta::empty`, `Inner::empty`, the test-only `init` and
  `is_initialised`, the `len == 0` guards in `Inner::alloc`/`reserve` and
  the `empty_then_init` test are gone; huge: `prior_state`'s `Active` arm
  is `unreachable!()`. (5) `Regions::in_meta` stays (`#[cfg(test)]`),
  `Meta::contains` is gone; its one user outside meta.rs
  (`heap/tests.rs`, the arena placement check) asks `Regions`. (6)
  `src/layout.rs` (1715 lines) is `src/layout/`: `mod.rs` 515 (size-class
  math and the two statics `CLASSES`, `SMALL_LOOKUP`, which must not
  change file), `regions.rs` 478 (address-space parameters, `Mapping`,
  `Kind`, `Regions`, `mod linux`; re-exported from `layout`, no caller
  changed), `tests.rs` 493, `linux_tests.rs` 245. Codegen: baseline
  recaptured at 911a1ed (the `.tmp-codegen` one was ea9532a, pre batch
  two); `compare` with everything: q16 `.text` 03bea0 -> 03bde0, 435 ->
  433 functions, exact 44, reloc-only 374, changed 15; q8 03c1c0 ->
  03c0e0, exact 44, reloc-only 373, changed 16; the changed list and the
  instruction counts (61397 / 61589) are identical with and without the
  layout split, so every layout function is exact or reloc-only. Changed
  functions, each with its cause: `huge::table::Table::storage` and
  `Table::ensure` (task 3: `layout()`/`release()`; the `Layout` `.unwrap()`
  is gone, which is why `core::result::unwrap_failed` and `LayoutError`'s
  `Debug::fmt` left the binary); `huge::HugeStats::sub` (task 2: the
  `Active` arm; `add` is inlined); `huge::Inner::add_extent` (task 2: new;
  `drop_extent` and `retire` are inlined into their callers; `insert_keys`
  left); `huge::Huge::take` (task 2: rewritten); `huge::Huge::stats` (task
  2: `table_cap` from the table); `huge::Huge::prior_state` (task 4);
  `meta::Inner::alloc`, `meta::Meta::reserve`, `meta::Meta::reserve_committed`
  (task 4: the `len == 0` guard; `alloc` grew 108 -> 116 by duplicated
  epilogues where the early return was); `heap::shared::Shared::alloc_huge`
  (+54/-51: `Huge::alloc` is inlined into it by LTO, the `Placement`, the
  new `take`, the removed `table_cap` store), `Shared::free_huge`
  (+93/-103: `Huge::free_run` inlined), `api::init` (+94/-85 q16, +89/-79
  q8: `Huge::new`'s stats literal and the meta paths inlined, register
  renaming around them); `core::slice::sort::...::panic_on_ord_violation`,
  `__aarch64_ldset8_acq` and (q8 only) `<bool as Display>::fmt` differ by
  trailing `nop` padding only (two functions fewer shifts the alignment).
  Nothing changed under `layout/`, `slab.rs` or `bitmap.rs`. Tests: host
  `cargo test --features std` 345 (341 on main: +2 table, +3 huge
  (`runs_have_one_key_and_free_extents_two`,
  `prior_state_names_the_source_of_the_pages`,
  `huge_stats_abort_on_a_run_count_going_below_zero`), -1 meta);
  container `make test` 445 + 133 + 25 (main 441 + 133 + 25), cgroup
  limit/churn/lazy and pagemap-guard steps `ok`, api's
  `fork_lock_list_matches_the_source_tree` passes with the layout split
  (no lock lives there). `randomised_model_check` and the full `audit`
  after every step kept and green throughout.

## Notes for the human

- `Table::find` (task 1): chose the abort in `insert`, not a bounded probe.
  Both end in the same abort: with the table one slot short of full a free
  that finds no neighbour must insert two keys for its free extent, so a
  bounded `find` would return not-found from `free_before`/`free_after`
  and then hit "descriptor table full" at the survivor's `add_extent` two
  calls later, having paid a probe counter in `find` (called on every
  huge free and `usable_size`) for nothing. The abort in `insert` is a one
  token change (`count + 1 < cap`), keeps `find` exact, and fits the rule
  (a table that cannot grow is a metadata region that is exhausted, and
  the free path has no way to report that). The message is unchanged; it
  now fires one insert earlier than before.
- Two aborts became reachable that were not (both on inconsistent state
  only, both the checked form of a subtraction the release profile would
  otherwise wrap): a run count going below zero in `free_run` aborts with
  "huge: Active extent count underflow" (`HugeStats::sub` replaces the
  bare `runs -= 1`), and `Table::layout` panics ("huge: descriptor table
  layout") where `storage` returned `None` for a `Layout` too large to
  exist (impossible: the table lives in the metadata region).
- One line outside the lane's files, as task 5 directed: `src/heap/tests.rs`
  `s.meta.contains(..)` -> `s.regions.in_meta(..)`. No other file changed.
- For lane tidy-heap (`Huge::alloc` returning the placed pages): `alloc`
  builds `Placement { start: s, pages: need, ext }` at the `take` call and
  is otherwise untouched (signature, returns, doc); the pages to return are
  `need`, already in scope, or the placement's `pages`. `find` could
  return `Option<Placement>` directly (it computes exactly those three
  values) once the `let Some((s, ext)) = self.find(..)` line, which
  carries a return, is theirs to edit; `prior_state(s, ext)` would take
  the placement too.
- Design 19 (docs/design.md, the source table at 2688 and 2693) still
  lists `src/layout.rs` and `src/huge.rs`; suggested rows:
  `src/layout/            size classes (mod.rs), regions, pointer
  classification and the Linux reservation (regions.rs)` and `src/huge/
  huge allocator (mod.rs), its descriptor table (table.rs)`. Design 7's
  table bullet (line 906) could gain one sentence: "The table keeps its
  load at or below one half through `ensure`; `insert` aborts rather than
  fill the last slot, so a probe for an absent key always ends."
- Task 6 landed as `layout/{mod,regions}.rs`, not the review's
  `{mod,classes,regions}`: the class math holds the two statics and
  `codegen-diff.sh`'s own caveat is that a static changing file moves its
  merged-globals slot and flips encodings in unrelated functions; keeping
  it in `mod.rs` keeps the module path `layout::CLASSES` and the compare
  clean. Nothing dropped.
- The lane's `.tmp-codegen` baseline is now 911a1ed; a coordinator compare
  on `main` after the merge should recapture first.
