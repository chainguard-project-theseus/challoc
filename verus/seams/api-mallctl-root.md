# `root`

Seam S12 of `verus/SEAMS.md`: src/api/mallctl.rs:979. Kind: external_body. Category: function pointer.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
fn root() -> (r: &'static [Node])
    ensures
        r@ == root_nodes(),
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `api::mallctl::kani_root::root_returns` (`src/api/mallctl.rs`):
`root()` returns `&ROOT[..]` (the same address and length), and a
second call returns the same slice.

Reproduce: `sh verus/kani/run.sh --harness api::mallctl::kani_root::root_returns` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`. No input.
- `ensures r@ == root_nodes()` (added since ae82d5b): `root_nodes` is
  an uninterpreted `spec fn`, so the claim is that every call returns
  the same sequence of nodes. Every call returns `&ROOT[..]` (both
  assertions), and `ROOT` is an immutable `static` whose `Node`s hold
  only `&'static str`s, `&'static` tables and function pointers (no
  interior mutability), so its contents never change: interpreting
  `root_nodes()` as `ROOT@` makes the `ensures` hold. Reading
  the table runs none of its readers (the function pointers the seam's
  category names); they are separate seams (`call`), not this one's.

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
