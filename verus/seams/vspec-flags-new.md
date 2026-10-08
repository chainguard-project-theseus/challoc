# `new` (`vspec/flags.rs`)

Seam S314 of `verus/SEAMS.md`: src/vspec/flags.rs:64. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At the commit that adds it (package FFLAGS):

```rust
pub fn new(v: u8) -> (r: RouteByte)
    ensures
        fixed(&r) == v & !ROUTE,
    opens_invariants none
    no_unwind
```

The body: `RouteByte { b: AtomicU8::new(v) }`, the only place a
`RouteByte` is built (its field is private to `vspec/flags.rs`, and it
has no `Clone`, `Copy` or `Default`). Its one call site is
`Shared::put_flags` (`init`), which writes the result into the
unpublished `Shared`'s `free_flags` field with `ptr_mut_write`, from the
same `flags` it writes to `push_flags`, and ensures the field
permission's value has `fixed` bits `flags & !FREE_ROUTE`;
`axiom_shared_built` carries that value into the built `Shared`
(`shared_at(p).free_flags == parts.free_flags.value()`, beside the same
equation for every other field), and `init` proves `Shared::wf`'s
conjunct from it.

## Statement

`core`'s `AtomicU8::new` is a `const fn` that returns an atomic holding
`v` and cannot panic. `fixed` is an uninterpreted spec function of the
`RouteByte` value, so stating it of the fresh value introduces no
contradiction: vstd's `AtomicU8` is an opaque external type with no
`PartialEq` and specifications without `ensures`, so Verus knows
nothing else of `new`'s result and can prove no two results equal
unless they are one value. The `ensures` is the start of the byte's
invariant: the made byte's bits outside `ROUTE` are `v`'s; that every
later value keeps them is C11 coherence over the only writes the type
allows (`new`'s, then `fetch_or`s within `ROUTE`), argued and
model-checked in `vspec-flags-load.md` (parts 1 and 2), whose models
run this wrapper as `init`'s:

Models: `loom_flags_route_byte_keeps_push_flags`, `loom_flags_two_builders`, `loom_flags_relaxed_publish_is_a_race`

## Translation

- **No `requires`**: every `v`.
- **`ensures fixed(&r) == v & !ROUTE`**: the made atomic holds `v`, so
  its bits outside `ROUTE` are `v & !ROUTE`. The value keeps its `fixed`
  bits when moved into place (`put_flags`' `ptr_mut_write`, a plain
  write of the byte before `READY`), and nothing verified can replace
  it afterwards: the field permission is consumed by
  `axiom_shared_built`, and through `&RouteByte` Verus treats the value
  as unchanging. That the real byte agrees is part 1 of
  `vspec-flags-load.md`.
- **`opens_invariants none`**: read from the source.
- **Gaps.** Those of `vspec-flags-load.md` (a raw write over the field
  that the `run.sh` check does not name is caught only by review).

## Review

See `vspec-flags-load.md`.

## Tests

`sh verus/loom/run.sh`; see `vspec-flags-load.md`.
