# `S_TCACHES`

Seam S151 of `verus/SEAMS.md`: src/heap/shared.rs:1076. Kind: external_body const. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
const S_TCACHES: usize = core::mem::offset_of!(Shared, tcaches);
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::shared::kani_layout::shared_layout` (`src/heap/shared.rs`):
`S_TCACHES == offset_of!(Shared, tcaches)`, and the field-fit conditions of
`axiom_shared_layout` at that value (see its record,
`verus/seams/heap-shared-axiom_shared_layout.md`).

Reproduce: `sh verus/kani/run.sh --harness heap::shared::kani_layout::shared_layout` (both quanta;
`--harness` takes the full path).

## Translation

An `external_body` const: Verus sees a `usize` whose value it cannot
evaluate (`offset_of!`), and assumes nothing else about it; everything
the proofs use about the value comes from `axiom_shared_layout`. The
harness asserts the value is its definition and that the axiom's
conditions hold of it. No input: the check is exact.

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
