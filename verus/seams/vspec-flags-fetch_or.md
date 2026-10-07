# `fetch_or` (`vspec/flags.rs`)

Seam S314 of `verus/SEAMS.md`: src/vspec/flags.rs:93. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At the commit that adds it (package FFLAGS):

```rust
pub fn fetch_or(b: &RouteByte, v: u8, ordering: Ordering) -> (prev: u8)
    requires
        v & !ROUTE == 0,
    ensures
        prev & !ROUTE == fixed(b),
    opens_invariants none
    no_unwind
```

The body: `b.b.fetch_or(v, ordering)`. Its one call site is
`Shared::set_multi_arena` (`FREE_ROUTE`, `Relaxed`; its result unused),
which `Arenas::build` calls for every arena after the first; the
`requires` holds there because `FREE_ROUTE` is `ROUTE` (8; a
compile-time assertion in `heap/shared.rs`, and a `bit_vector` step at
the call). Before this package `set_multi_arena` called `core`'s
`fetch_or` directly (verified, with vstd's unconstrained result).

## Statement

`core`'s `fetch_or` is one atomic read-modify-write for every ordering
(it cannot panic): it reads the write just before its own in the byte's
modification order and writes `prev | v`. With `v` within `ROUTE` the
written value keeps `prev`'s bits outside `ROUTE`, which is the step of
the induction in `vspec-flags-load.md` (part 1); and `prev`, a value of
that order, has them as every value does (`fixed(b)`). The models of
`vspec-flags-load.md` (part 2) run this wrapper as the builders', one
and two of them concurrently, and check `prev` and the joined value;
the control `loom_flags_bit_outside_route_is_caught` runs it with a
`v` the `requires` excludes and fails:

Models: `loom_flags_route_byte_keeps_push_flags`, `loom_flags_two_builders`, `loom_flags_bit_outside_route_is_caught`

## Translation

- **`requires v & !ROUTE == 0`**: the bits the write may add; this is
  what keeps the invariant `load`'s `ensures` relies on. Verus checks it
  at the one call site, `set_multi_arena`, verified code; `run.sh`
  checks by name that this is the only call outside the loom models.
- **`ensures prev & !ROUTE == fixed(b)`**: part 1 of
  `vspec-flags-load.md`; no caller uses it today. Its premise, that the
  caller happens after `init`'s write of the byte, holds as there:
  `Arenas::build` runs with the published `Shared`, whose `&Shared` came
  from `api::shared` or a seam that hands out the published one, after
  an `Acquire` load of `STATE` that saw `READY` (the premise rests on
  where the reference came from, not on `Shared::wf`, a value
  predicate).
- **`opens_invariants none`**: read from the source.
- **Gaps.** Those of `vspec-flags-load.md`.

## Review

See `vspec-flags-load.md`.

## Tests

`sh verus/loom/run.sh`; see `vspec-flags-load.md`.
