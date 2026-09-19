## Done

- Phase 2.5 task 8 (lane C, the key; 2026-09-19): `percpu_arena` is a real
  config key (baa8bc2): `config::PercpuArena::{Auto, Percpu, Off}`, default
  `auto`, field `Config::percpu_arena` (after `narenas`, read at init like
  `thp` and `pac_pointers`), `KEYS.len()` 26, `opt.percpu_arena` readable
  through mallctl (`cstr_of` arms `percpu` and `off`), the key removed from
  `IGNORED_KEYS`; helpers `PercpuArena::enabled(mm_cid_usable)` (`auto` and
  `percpu` follow the probe, `off` never) and `forced()` (only `percpu`
  earns a report note when the probe fails). Tests: `percpu_arena_values`
  (three values, jemalloc aliases, eight bad values reported and left at
  the default, last pair wins, quoted value, the two helpers), the defaults
  table, the Redis/Valkey conf-string test. Host `cargo test --features
  std` 298 green; in the Mac dev container (lane volume) `cargo build
  --release` and the two `make test` cargo lines 376 + 115 green. Design
  12's table has the row, 15 names the key, README's `mm_cid` row points at
  it. No per-CPU behaviour, no `src/model.rs` change (stage 2); no VM used.

## Design decisions

- Phase 2.5 task 8 (lane C): jemalloc's own `percpu_arena` spellings are
  accepted as aliases, `disabled` as `off` and `phycpu` as `percpu`, the
  way `thp` accepts `default` as `never`. Reason: until this commit the key
  was accepted with any value, so a Valkey or Redis conf string written
  against jemalloc (`percpu_arena:phycpu,abort_conf:true`) parsed clean;
  rejecting those spellings now would make `abort_conf` abort a
  configuration that worked yesterday. `phycpu` maps to `percpu` as the
  nearest mode (a concurrency id is not a physical CPU; no hyperthread
  pairing exists here). Anything else (`true`, `on`, mixed case) is a bad
  value, reported and left at `auto`.
- Phase 2.5 task 8 (lane C): under a failed probe `percpu` and `auto`
  behave the same (thread caches); the only difference is the report note
  (`PercpuArena::forced`). Reason: a forced request cannot read an `mm_cid`
  field the kernel does not write, and the plan says the fallback holds
  with a note, not an abort.

## Notes for the human

- Phase 2.5 task 8 (lane C): lane A reads the mode through
  `shared.config.percpu_arena` (type `config::PercpuArena`) and decides with
  `percpu_arena.enabled(mm_cid_usable)`; `percpu_arena.forced()` says whether
  a failed probe goes into the feature report. `src/api/mallctl.rs` (not in
  any lane's partition) got two `cstr_of` arms, as the plan's key item asks.
