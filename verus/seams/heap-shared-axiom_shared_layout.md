# `axiom_shared_layout`

Seam S152 of `verus/SEAMS.md`: src/heap/shared.rs:1082. Kind: axiom. Category: core library.
Discharged by Kani.

## Contract

As changed by the port of upstream #101 (`region_hi: usize` between
`push_flags` and `regions`, at `S_REGION_HI`), before that by the port
of upstream #87 (a first conjunct for the new
class lines, `[ClassLine; NCLASSES]` at `S_LINES`, ahead of
`free_flags`; the guard tests' conjunct after `regions` gone, `regions`
now ending at `S_TABLES`; `ClassLine` in place of `GuardTest` in the
array bound; Kani re-run pending, below), before that by the port of
upstream #78 (its first conjunct: the field
`multi_arena: AtomicBool` became `free_flags: AtomicU8`, offset
`S_FREE_FLAGS`; Kani re-run there, below), and before that by the
library-and-control seam audit on base 8635213
(`verus/audit/library-control.md`; its Kani re-run was pending until
the #78 port), from the
contract checked at commit 7d4e997 (the record was written at
ae82d5b):

```rust
axiom fn axiom_shared_layout()
    ensures
        field_fits::<Shared, [ClassLine; NCLASSES]>(S_LINES, S_FREE_FLAGS as int),
        field_fits::<Shared, AtomicU8>(S_FREE_FLAGS, S_PUSH_FLAGS as int),
        field_fits::<Shared, u8>(S_PUSH_FLAGS, S_REGION_HI as int),
        field_fits::<Shared, usize>(S_REGION_HI, S_REGIONS as int),
        field_fits::<Shared, Regions>(S_REGIONS, S_TABLES as int),
        field_fits::<Shared, [SlabTable; NCLASSES]>(S_TABLES, S_ARENA0 as int),
        NCLASSES * size_of::<ClassLine>() <= isize::MAX,
        NCLASSES * size_of::<SlabTable>() <= isize::MAX,
        field_fits::<Shared, Arena>(S_ARENA0, S_META as int),
        field_fits::<Shared, Meta>(S_META, S_HUGE as int),
        field_fits::<Shared, Huge>(S_HUGE, S_GC_INTERVAL_NS as int),
        field_fits::<Shared, u64>(S_GC_INTERVAL_NS, S_POLICY as int),
        field_fits::<Shared, Policy>(S_POLICY, S_CONFIG as int),
        field_fits::<Shared, Config>(S_CONFIG, S_KEY as int),
        field_fits::<Shared, ThreadKey>(S_KEY, S_ARENAS as int),
        field_fits::<Shared, Arenas>(S_ARENAS, S_TCACHES as int),
        field_fits::<Shared, Tcaches>(S_TCACHES, S_STATS as int),
        // Over several lines, as in `arena::axiom_arena_layout`.
        field_fits::<
            Shared,
            Lock<Snapshot>,
        >(S_STATS, S_CGROUP as int),
        field_fits::<Shared, Cgroup>(S_CGROUP, size_of::<Shared>() as int),
        size_of::<Shared>() <= isize::MAX,
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::shared::kani_layout::shared_layout` (`src/heap/shared.rs`):
each of the seventeen `S_*` offsets (sixteen before upstream #101)
equals its field's `offset_of!`,
and for each field, with its type `T` and the next field's offset (the
last: `size_of::<Shared>()`) as `next`, `field_fits::<Shared, T>(off, next)`
holds (restated with its `int` sum in `u128`), as does
`size_of::<Shared>() <= isize::MAX`, `NCLASSES * size_of::<ClassLine>()
<= isize::MAX` and `NCLASSES * size_of::<SlabTable>() <= isize::MAX`
(in `u128`).

Reproduce: `sh verus/kani/run.sh --harness heap::shared::kani_layout::shared_layout` (both quanta;
`--harness` takes the full path).

## Translation

- An axiom: no `requires`.
- Each `ensures field_fits::<Shared, T>(S_X, S_NEXT as int)`: one
  `assert!`, with the same types in the same order (`field_fits` as in
  `arena::axiom_arena_layout`'s record).
- `ensures NCLASSES * size_of::<ClassLine>() <= isize::MAX` and the
  same for `SlabTable` (the `requires` of `axiom_array_layout` at the
  two arrays): the two `assert!`s after the `tables` field's, their
  products in `u128`.
- `ensures size_of::<Shared>() <= isize::MAX`: the last assertion.

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

Reconciled onto the port of upstream #78, 2026-10-06: the contract
above is the ledger's entry there, and the harness re-run under both
quanta with the same Kani passes (0 of 345 checks failed in each),
which also settles the audit's pending re-run.

Ported onto upstream #87, 2026-10-06: the contract above is the
ledger's entry there, and the harness asserts it (the class lines'
offset and fit first, the guard tests' gone); the build's `const`
assertion of the same conditions passes on both targets and quanta.
The Kani re-run is pending: Kani was not available where the port ran.

Ported onto upstream #101, 2026-10-06: the contract above is the
ledger's entry there (the new field `region_hi: usize` between
`push_flags` and `regions`, at `S_REGION_HI`), and the harness asserts
it; the build's `const` assertion of the same conditions passes on both
targets and quanta. The Kani re-run is pending, as for #87.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
