# tidy-huge: maintainability pass over huge.rs, slice.rs, layout.rs, meta.rs

## Done

- Maintainability pass (lane tidy-huge, 2026-09-20): docs and test-only
  API of `src/huge.rs`, `src/meta.rs`, `src/layout.rs`, `src/slice.rs`
  with zero machine-code change (`scripts/codegen-diff.sh compare`
  `.text hash IDENTICAL` for q16 and q8 after every group). Docs: huge's
  module doc now describes `find` as it is (each free extent's start, then
  the mark, rounded up to the alignment; taken when the run fits below the
  extent's end or the extent touches the mark; `take` leaves the pages
  before and after as free extents), the free-page mirror is `class::Set`
  (the `TwoLevelBitmap` link dangled); meta's stats identities name the
  fields that exist (`committed_bump`/`committed_reserved` did not), its
  object and reservation lists match the callers (every bitmap is a
  reservation), and the `Meta` doc says heap.rs builds it with `Meta::new`;
  plan-task and dated citations stripped from layout.rs, slice.rs and two
  huge test docs (bench citations behind constants kept). Gated
  `#[cfg(test)]`: `Huge::free`, `npages`, `oldest_dirty_at`; `Meta::init`,
  `is_initialised`, `base`, `top`, `contains`; `layout::Tier`,
  `Class::tier`, `Class::bitmap_words`, `Regions::stride`, `gap`,
  `in_meta`; `slice::thread_enabled` (with `target_os = "linux"`); the
  slice test module and its lock are private. Deleted (no callers):
  `Huge::guard_install()`, `HugeDesc::usable()`, `HugeDesc::dirty_at()`.
  Tests (+7 on the host, +6 on Linux): `Regions::slot_in_class` agrees
  with `slab_slot` for every class and in-slab offset and is `None`
  outside the class; `Huge::alloc(0, ..)` gets one page; `huge::Table`
  backward-shift deletion across the wrap from the last slot to slot 0;
  `Table::ensure` failing cleanly on metadata exhaustion; `slice::Verdict`
  byte round trip and distinct report words, `Probe::verdict`, and the
  off-Linux inert paths (the verdict loop left `layout_constants`). Host
  `cargo test --features std`: 326 passed (was 319). `make test` in the
  container: 417 + 120 + 23 passed, cgroup and pagemap-guard steps `ok`.
  Commits 30cb2d6, 1914119, 04013e1, eb65199.

## Notes for the human

- Skipped, later-batch candidate: the unreachable `RunState::Active =>
  "Active"` arm in `Huge::prior_state` (huge.rs). Replacing the three-arm
  match with a two-way test shrank `.text` by 0x40 bytes (two functions
  reloc-only), so identity did not hold and it stays as it was.
- Skipped, later-batch candidate: `impl Default for Meta` and `Meta::empty`
  (meta.rs). Nothing calls them, but removing the impl block renumbers the
  v0 mangling disambiguator of `impl Meta` (`Ms1_` to `Ms0_`), which moves
  its six functions in `.text`: all 435 functions match exactly, the
  sections have the same sizes, only the hash differs. A comment on the
  impl says so. General gotcha for every lane in this batch: adding or
  removing an impl block in a module changes the symbol names, and so the
  `.text` order, of every later impl in that module even when no code
  changes; methods inside an impl can be gated or removed freely.
- Found, not changed (behaviour): `huge::Table::find` does not terminate
  when the table is completely full and the key is absent (`insert` lets
  `count` reach `cap`; a probe for an absent key ends only at an empty
  slot). Production keeps the load at or below one half through `ensure`,
  but `free_run`'s `ensure` is best-effort and a free adds up to one net
  key, so after a failed growth (metadata exhausted) a long free-only
  sequence could in principle fill the table and then spin in
  `free_before`/`free_after` instead of aborting. Candidates: make
  `insert` abort at `cap - 1`, or bound `find` to `cap` probes and abort.
  The new `table_growth_failure_leaves_the_table_working` test stops one
  slot short of full for this reason (its first form hung the host run).
- `make test`, first run: `api::ctests::c_thp_never` failed with
  "AnonHugePages 0 kB in the 8 MiB huge run under never on an always host"
  while another lane's Valkey soak container ran on the same host (load
  average 16-24). It passed alone right after (8192 kB) and in the full
  rerun. The kernel backs a fresh run with a PMD page at fault time only
  when one is available, so the check is sensitive to a loaded host;
  worth a retry or a tolerance in `tests/c/thp.c` if it recurs.
- docs/design.md edits for the coordinator (lane D owns docs):
  - Section 7, the alignment bullet ("Alignment requests above page size
    are satisfied by over-allocating and releasing the head and tail pages
    back to the bitmap"): the code does not over-allocate. `find` rounds
    the start of each free extent (and finally the high-water mark) up to
    the alignment and takes the extent when the run fits below its end or
    the extent touches the mark and can grow; `take` records the pages
    before the aligned start (and any left after the run) as free
    extents, a hole at the mark as a `Purged` one. An alignment above the
    region's size is refused. The first bullet's "`TwoLevelBitmap` over
    pages" is the `class::Set` shared with the arenas.
  - Section 19 (source layout) lacks four files: `src/slice.rs` (rseq
    time slice extension for lock holders, section 17), `src/thp.rs`
    (transparent huge pages: the `thp` key against the sysfs table,
    reservation advice, metadata folios, `MADV_HUGEPAGE` on huge runs;
    sections 7, 10, 17), `src/sys/cldemote.rs` (the `cldemote` cache-line
    hint: probed, reported, tested both ways, issued on no path; section
    17) and `src/sys/pac.rs` (pointer authentication of the TLS heap
    pointer on arm64 with FEAT_PAuth; sections 6.1, 6.2, 17).
