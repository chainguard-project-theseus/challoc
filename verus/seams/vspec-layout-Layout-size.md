# `Layout::size`

Seam S303 of `verus/SEAMS.md`: src/vspec/layout.rs:31. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub assume_specification[ Layout::size ](l: &Layout) -> (r: usize)
    ensures
        r == layout_size(*l),
        layout_ok(*l),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::misc::layout_from_size_align, kani_proofs::misc::layout_new_instances` (`src/kani/misc.rs`):
for every layout `from_size_align` accepts (`layout_from_size_align`,
every `size` and `align`) and for `Layout::new::<T>()` at the types
of `layout_new_instances`, `l.size()` is the layout's size and the
layout is `layout_ok`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::misc::layout_from_size_align --harness kani_proofs::misc::layout_new_instances` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`.
- `ensures r == layout_size(*l)`: true by the reading below (the
  method is the function).
- `ensures layout_ok(*l)`: asserted for every layout the harnesses
  build.
- Gap: `layout_ok` is claimed of every `Layout` value, and Kani checks
  the layouts built by `from_size_align` (all of them) and `new` (the
  instantiations listed). Other constructors (`from_size_align_unchecked`,
  `array`, `extend`) keep the same invariant by `core`'s safety
  contract (it is `Layout`'s type invariant: a power-of-two alignment
  and a size that rounds up within `isize::MAX`); challoc builds
  layouts only through `from_size_align` and `new`
  (`grep -rn 'Layout::' src`).

`layout_size` and `layout_align` are uninterpreted spec functions;
the contracts that name them (`Layout::size`, `Layout::align`,
`Layout::from_size_align`, `Layout::new`) are consistent when they
are read as the methods `Layout::size` and `Layout::align`, and the
harnesses check every contract under that one reading. `layout_ok`
then reads: `align` is `pow2(k)` for some `k < 64`
(`is_power_of_two`, itself checked against that spec in
`verus/seams/vspec-core_int-usize-is_power_of_two.md`), and
`size + align - 1 <= isize::MAX` (in `u128`).

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
