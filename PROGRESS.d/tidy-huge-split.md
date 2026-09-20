# tidy-huge: batch two, huge.rs split into src/huge/

## Done

- Maintainability pass, batch two (lane tidy-huge, 2026-09-20): `src/huge.rs`
  (2682 lines) is the directory module `src/huge/`: `mod.rs` 1217 lines
  (module doc, `HugeDesc`/`RunState`/`RunInfo`/`HugeStats`, the `os` shim,
  `Inner`, `Huge`, the cold aborts), `table.rs` 298 (`Entry`,
  `LG_INITIAL_TABLE`, the open-addressing `Table`, and its own test module
  with the four tests that drive `Table` directly: wrap-around,
  growth-failure, the two `should_panic`), `tests.rs` 891 (the host tests),
  `linux_tests.rs` 302. Code motion only: no function body, attribute or
  name changed; every `crate::huge::` path other files use still names an
  item defined in `mod.rs`, so no other file changed.
  `scripts/codegen-diff.sh compare .tmp-codegen` against ea9532a: q16 and
  q8 both `.text` size identical (`03bea0`, `03c1c0`), 435 functions
  `exact`, 0 `reloc-only`, 0 `changed` (the `.text` hash differs only
  through symbol order and path strings). Host `cargo test --features std`
  341 passed (38 huge tests before and after). Container `make test`: 442
  passed in the non-api binary, 123 passed in the api binary once the one
  path-keyed literal below is updated (122 + 1 failed without it), cgroup
  limit/churn/lazy and pagemap-guard steps `ok`. Commit 16293f8.

## Notes for the human

- Needs a two-literal edit in a file this lane may not touch, at merge:
  `api::tests::fork_lock_list_matches_the_source_tree` keys every lock by
  its path under `src/` and now finds `huge/mod.rs:Huge.inner`; change
  `"huge.rs:Huge.inner"` to `"huge/mod.rs:Huge.inner"` in `FORK_LOCKS`
  (src/api/mod.rs:522) and in `assert_locks` (src/api/mod.rs:560). Verified
  in the container with the edit applied temporarily: api 123 passed; the
  edit was reverted before the commit. Note the inventory's `mod tests {`
  stop rule no longer applies to huge (its tests are separate files), which
  is why the key lists `huge/mod.rs` and nothing from `tests.rs`.
- `pub(super)` surface opened in `huge/table.rs`: `struct Table`, its
  `count` field, and `Table::{new, cap, get, insert, remove, ensure}` (what
  `Inner`, `Huge` and the audit use). `Entry`, `LG_INITIAL_TABLE`,
  `storage`, `hash`, `find`, `slot` and the other fields stay private;
  `table.rs`'s tests reach `Env` and `page_size` via `crate::huge::tests`
  (already `pub(super)`). No `pub use` re-exports were needed. One import
  narrowed in `mod.rs`: `use crate::meta::Meta` (the `meta::` module path
  was used only by `Table::storage` and the tests; `tests.rs` imports
  `crate::meta` itself). `huge::tests::table_grows_and_lookups_survive_backward_shift`
  stays in `tests.rs` because it drives `Huge`, not `Table`.
- Skipped, as instructed: `add_free_extent`/`drop_extent`, `Huge::alloc`
  returning its pages (batch three); the `impl Default for Meta` /
  `Meta::empty` leftovers (meta.rs).
- Observed once, not reproduced: the first `make test` run hung for 40
  minutes in `lock::tests::a_parked_waiter_is_released` (two threads in
  `futex_wait`, host load 7-17 from unrelated Go link jobs); the rerun
  finished the same binary in 6.4 s. Lane C's file; worth a look at the
  20 ms window if it recurs.
- Design 19 lines to write, replacing `src/huge.rs           huge allocator`:
  ```
  src/huge/mod.rs       huge allocator: extents, guards, first fit, decay, the audit
  src/huge/table.rs     the open-addressing descriptor table (page number to HugeDesc)
  src/huge/tests.rs, src/huge/linux_tests.rs  huge tests: host, and the ones that need a kernel
  ```
  Also stale after the move: `huge.rs` named in src/bitmap.rs:431,
  src/class.rs:241,276-277, src/purge.rs:67 (prose, other lanes' files).
