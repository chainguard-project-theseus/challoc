# `S_REGION_HI`

Seam S139 of `verus/SEAMS.md`: src/heap/shared.rs:1054. Kind: external_body const. Category: core library.
Discharged by Kani.

## Contract

At commit caf28956 (the port of upstream #101, which added the field
`region_hi: usize` between `push_flags` and `regions`: the bits of an
offset past a class region, the sized check's region bound; checked
again at 61768ed6):

```rust
const S_REGION_HI: usize = core::mem::offset_of!(Shared, region_hi);
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::shared::kani_layout::shared_layout` (`src/heap/shared.rs`):
`S_REGION_HI == offset_of!(Shared, region_hi)`, and the field-fit conditions of
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
in a `const` assertion (`arena::fits`, right after the axiom; its
`ensures` is `field_fits`, its sum in `usize`, where an overflow fails
the build), which is the stronger check (rustc's own layout, on every
build) and the reason a layout change cannot ship unnoticed. The
assertion does not restate `S_REGION_HI == offset_of!(Shared,
region_hi)`, which holds by the const's definition. `verus/kani/run.sh`
runs the harness under both quanta, since `NCLASSES` sizes the arrays.
`Shared`'s own fields are all exec; the ghost fields inside its field
types (`ClassLine::committed_inv`, `Arena::sets_perm`) are `Tracked`,
zero-sized in builds, and the axiom is about the build's layout, which
is what the pointers the proofs place address.

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

Written at the port of upstream #101, 2026-10-06, from the
`S_PUSH_FLAGS` record (the same harness, the same kind of seam): the
contract above is the ledger's entry, and the harness asserts it
(`S_REGION_HI`, and the fit of `push_flags` before it and of
`region_hi` before `regions`); the build's `const` assertion of the
same conditions passes on both targets and quanta.

Re-run at commit 9c2de60f (the port of upstream #116 onto the CCall
tip), 2026-10-06, with Kani 0.68.0 under both quanta:
`heap::shared::kani_layout::shared_layout` passes (0 of 366 checks
failed in each).

Fresh-context subagent, 2026-10-06, at commit 61768ed6: the harness,
the build's `const` assertion and the ledger entry against this
record. It found the harness asserts what the Statement says (the
offset, and the fits of `push_flags` before `region_hi` and of
`region_hi` before `regions`, with the axiom's types and order); its
other findings (a stale "pending" line, the commits the contract and
the re-run were taken at, the harness comment's seam numbers, the
ghost-field sentence, what the `const` assertion checks) are folded
in, and a second fresh-context subagent reviewed the revision. Only
this record was revised: its siblings (`heap-shared-S_PUSH_FLAGS.md`
and the other `heap-shared-S_*` records) keep the earlier wording of
the two Translation sentences. Re-run at 61768ed6, 2026-10-06, with Kani 0.68.0 under both
quanta: it passes (0 of 366 checks failed in each).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
