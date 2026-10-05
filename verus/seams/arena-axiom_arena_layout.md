# `axiom_arena_layout`

Seam S54 of `verus/SEAMS.md`: src/arena.rs:532. Kind: axiom. Category: core library.
Discharged by Kani.

## Contract

As changed by the library-and-control seam audit on base 8635213
(`verus/audit/library-control.md`; Kani rerun pending), from the
contract checked at commit 7d4e997 (the record was written at
ae82d5b), and on base 77c59fd by the frozen set-table permission: the
`sets_perm` field is `Tracked<&'static PointsTo<SetTable>>`, zero-sized
with alignment 1 as `Tracked<PointsTo<SetTable>>` was (both are
`PhantomData`), so the harness, which checks the field at its type, is
unchanged in substance (Kani rerun pending):

```rust
axiom fn axiom_arena_layout()
    ensures
        field_fits::<Arena, u16>(A_INDEX, A_DECAY_NS as int),
        field_fits::<Arena, AtomicU64>(A_DECAY_NS, A_SETS as int),
        field_fits::<Arena, *const SetTable>(A_SETS, A_SETS_PERM as int),
        field_fits::<Arena, Tracked<&'static PointsTo<SetTable>>>(A_SETS_PERM, A_CLASSES as int),
        field_fits::<Arena, [ClassState; NCLASSES]>(A_CLASSES, A_LARGE as int),
        NCLASSES * size_of::<ClassState>() <= isize::MAX,
        field_fits::<Arena, AtomicU64>(A_LARGE, A_STATS as int),
        // Over several lines: `api`'s lock inventory test reads a line
        // `name: ...Lock<...>` as a lock field.
        field_fits::<
            Arena,
            Lock<ArenaStats>,
        >(A_STATS, A_LAST_EXIT_PURGE as int),
        field_fits::<Arena, AtomicU64>(A_LAST_EXIT_PURGE, size_of::<Arena>() as int),
        size_of::<Arena>() <= isize::MAX,
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `arena::kani_layout::arena_layout` (`src/arena.rs`):
each of the eight `A_*` offsets equals its field's `offset_of!`, and
for each field, with its type `T` and the next field's offset (the
last: `size_of::<Arena>()`) as `next`, `field_fits::<Arena, T>(off, next)`
holds (restated with its `int` sum in `u128`), as does
`size_of::<Arena>() <= isize::MAX` and
`NCLASSES * size_of::<ClassState>() <= isize::MAX` (in `u128`).

Reproduce: `sh verus/kani/run.sh --harness arena::kani_layout::arena_layout` (both quanta;
`--harness` takes the full path).

## Translation

- An axiom: no `requires`.
- Each `ensures field_fits::<Arena, T>(A_X, A_NEXT as int)`: one
  `assert!`, with the same types in the same order. `field_fits` is
  `off % align_of::<T>() == 0`, `align_of::<Arena>() % align_of::<T>() == 0`
  and `off + size_of::<T>() <= next`, with vstd's `size_of`/`align_of`
  read as `core::mem`'s (vstd's are uninterpreted for these types).
- `ensures NCLASSES * size_of::<ClassState>() <= isize::MAX` (the
  `requires` of `axiom_array_layout` at the `classes` array): the
  `assert!` after the `classes` field's, its product in `u128`.
- `ensures size_of::<Arena>() <= isize::MAX`: the last assertion.

The layout is the target's: Kani evaluates it for x86_64; every
build, for either target and quantum, also evaluates the same conditions
in a `const` assertion (`fits`, next to the axiom), which is the
stronger check (rustc's own layout, on every build) and the reason a
layout change cannot ship unnoticed. `verus/kani/run.sh` runs the
harness under both quanta, since `NCLASSES` sizes the arrays. The
ghost field types (`Tracked<PointsTo<..>>`) are zero-sized in builds;
the axiom is about the build's layout, which is what the pointers the
proofs place address.

Common to every Kani record: Kani checks the MIR of a debug build
(overflow checks and debug assertions on) for x86_64, with CBMC's
bit-precise semantics for the integer operations and Kani's own models
of the `core` intrinsics it lowers. The shipped library is a release
build: it drops the overflow checks and debug assertions (where a
harness shows they never fire under the contract, the two builds
agree), and it is also built for aarch64, whose layouts and pointer
width match x86_64's for every type here (both are LP64 targets;
the layout records name the one check that runs per target). The
`core` Kani checks is its own nightly's (2026-08-21); the shipped
library is built with rustc 1.98, whose `core` functions here are the
same unless they changed between the two, which a rerun with a newer
Kani would show. Policy for every Kani record: a trusted body must
return under its `requires` without a panic, except an abort seam
(whose contract is that it does not return) and the overflow aborts
a contract's comment declares (`fig_*`, `stats_allocated`); a panic
in challoc aborts the process (`panic = "abort"`), so it never yields
a wrong result, but the proofs' "no panic" claim rests on this policy.

## Review

Fresh-context subagent, 2026-10-02: the harness against the contract
and this translation; its findings are folded in.

Reconciled onto commit 7d4e997, 2026-10-03: the contract above
compared with the ledger's entry there, and the harness re-run under
both quanta with the same Kani (it passes). A fresh-context subagent
reviewed the reconciliation.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
