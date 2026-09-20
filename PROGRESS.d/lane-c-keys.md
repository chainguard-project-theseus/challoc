## Done

- Phase 2.5 task 16 (lane C, 2026-09-20), the key half: done, commit 3352bf0.
  `thp_process:default|advised` (default `default`) in `src/config.rs`:
  `config::ThpProcess { Default, Advised }` with `name()` and
  `requested()` (true only for `advised`), the field
  `Config::thp_process` and the accessor `Config::thp_process()`;
  `KEYS` 27 -> 29, the `get` arm, the `cstr_of` arms for `default` and
  `advised` in `src/api/mallctl.rs`, the `opt.thp_process: "default"`
  needle in the `src/api/print.rs` report test, the section 12 row in
  `docs/design.md`; `config::tests::thp_process_values` (both values,
  ten bad spellings left at the default and reported, the last pair wins,
  quoted values, `opt.` read-back, `thp:default` is still `thp:never` and
  not this key). The `prctl` itself is lane B's (`thp.rs`,
  `features.rs`): `Config::thp_process().requested()` says whether to
  issue it. Host `cargo test --features std`: 311 passed; `make test` in
  the lane's container (`TARGET_VOLUME=challoc-target-lane-c-arm64`): 402
  + 117 + 23 passed, `EXIT 0`. No VM, no numbers: cold-path keys.

- Phase 2.5 task 11 (lane C, 2026-09-20), the key half: done, commit
  3352bf0. `prefault:true|false` (default `false`) in `src/config.rs`:
  the field `Config::prefault`, the accessors `Config::prefault()` (the
  key as set) and `Config::prefault_active()` (`prefault &&
  background_thread`: the pass runs on the background thread, so without
  it the key is stored and readable but does nothing, as `tcache_max`
  without `tcache`); `KEYS` (after `background_thread`), the `get` arm,
  the `opt.prefault: false` needle in the report test, the section 12 row
  in `docs/design.md`; `config::tests::prefault_values`. The pass is lane
  B's (the thread and the commit path) and the claim side lane A's, as
  PROGRESS.md's task 11 plan says.

## Design decisions

- Phase 2.5 task 16 (lane C): the key is `thp_process`, values `default`
  and `advised`, following the plan text. `default` means "nothing
  issued", not "the kernel's default": the process keeps whatever policy
  it inherited (a parent's `PR_SET_THP_DISABLE` included), so
  `thp_process:default` is byte for byte the allocator without the key.
  No `never`/`always` values: a process-wide `PR_SET_THP_DISABLE` of 1
  without the except-advised flag would also disable the pages `thp:auto`
  advises, and `always` is not something `prctl` can grant; if lane B
  wants the plain disable as a value later, `ThpProcess` gains a variant
  and `requested()` stays the "issue a prctl" gate. The enum is stored on
  `Config` next to `thp`, not folded into `Thp`, because the two compose
  (`thp:auto,thp_process:advised` is the Chainguard OS configuration the
  plan aims at) and because `thp:default` already means `never` for
  jemalloc compatibility.

- Phase 2.5 task 11 (lane C): `prefault` is a plain bool with no
  cross-key validation at parse time (no `Issue` when
  `background_thread` is false): the parser is per pair and the last
  pair wins in either order, so a "prefault needs the thread" report
  would depend on key order; instead `Config::prefault_active()` gives
  lane B the one predicate to gate the pass on and the report can say
  `prefault: requested, no background thread` from it. `KEYS` order puts
  `prefault` right after `background_thread` (the report reads in that
  order) and `thp_process` right after `thp`; `opt.` mib indices of the
  later keys shift by one and two, which nothing pins (the mallctl tests
  compute them from `KEYS`).

## Notes for the human

- Phase 2.5 task 11 (lane C): the model test ("a fresh slab committed
  and populated by the background thread is taken by the next claim as
  any committed empty slab") was not added, on purpose. `src/model.rs`
  quotes the real `class.rs` lines for every step, and the step it would
  need does not exist yet: the prefault pass has to decide what a
  committed-but-unclaimed slab looks like to `next_slab` (in the `empty`
  set with its state left `Fresh`, so the claim still knows it is zero
  for `calloc`, which `MADV_POPULATE_WRITE` preserves; or state `Empty`
  and the zero knowledge lost; or a new state), and whether the cursor
  slab check in `take_group`/`commit next` still holds with a committed
  slab below the cursor that no claim has touched. That is the commit
  path's decision (lane B). Once it lands, the model op is small: under
  the class lock, `s = group_next; commit_slab(s); group_next = s + 1;`
  then the set insert, and the invariant is "no committed slab is in no
  set and unclaimed" (`model::check` line 1761 already has the leak
  check to extend). Lane C can add it in one iteration after the merge.

- Phase 2.5 task 16 (lane C): for lane B, the exact names to code
  against: `crate::config::ThpProcess::{Default, Advised}`,
  `config.thp_process()` (also the pub field `thp_process`),
  `ThpProcess::requested()`, `ThpProcess::name()` (`"default"`,
  `"advised"`); `config.prefault()`, `config.prefault_active()`, the pub
  field `prefault`. `README.md` has no key table (only the feature
  table), so it was left alone; the feature-table row for THP is lane
  B's to extend when the `prctl` lands. Files touched outside the
  partition: `src/api/mallctl.rs` (two `cstr_of` arms), `src/api/print.rs`
  (two needles in the report test), `docs/design.md` (two rows in the
  section 12 table).
