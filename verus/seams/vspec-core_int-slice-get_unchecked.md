# `<[T]>::get_unchecked::<I>`

Seam S291 of `verus/SEAMS.md`: src/vspec/core_int.rs:85. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub assume_specification<T, I>[ <[T]>::get_unchecked::<I> ](slice: &[T], i: I) -> (b: &<I as core::slice::SliceIndex<[T]>>::Output)
    where I: core::slice::SliceIndex<[T]>
    requires call_requires(<I as core::slice::SliceIndex<[T]>>::index, (i, slice)),
    ensures call_ensures(<I as core::slice::SliceIndex<[T]>>::index, (i, slice), b),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::slice::slice_get_unchecked` (`src/kani/slice.rs`):
for every slice of up to 6 words, every `i < len` and every
`k <= len`: `s.get_unchecked(i)` is `&s[i]`, and
`s.get_unchecked(..k)`, `s.get_unchecked(k..)` are `&s[..k]`,
`&s[k..]` (the same address and length), with no undefined behaviour.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::slice::slice_get_unchecked` (both quanta;
`--harness` takes the full path).

## Translation

- `requires call_requires(I::index, (i, slice))`: `index`'s
  precondition is that the index is in bounds (it panics otherwise):
  `kani::assume(i < len)`, `kani::assume(k <= len)`.
- `ensures call_ensures(I::index, (i, slice), b)`: `b` is what
  `index` returns, which the harness compares by address and length
  (`ptr::eq`) with the checked index.
- Gap: generic over `I: SliceIndex`; the proofs call `get_unchecked`
  with a `usize` only (`grep -rn 'get_unchecked(' src`); Kani checks
  that and also `RangeTo<usize>` and `RangeFrom<usize>`. Kani's
  undefined-behaviour checks cover the unchecked access, including
  `core`'s debug precondition check of it.
- Gap: generic over `T`; Kani checks `u64` elements, while the proofs
  index slices of `ClassState`, `SlabTable`, `GuardTest`, the class
  table's `Class` and `usize` (`class_base`), and `*mut u8`. The
  access is pointer arithmetic by `size_of::<T>()` and reads no element,
  so the type enters only through that size. Bounded: slices up to 6
  elements; there is no loop, so the length matters only for the bounds,
  which the assumptions cover.

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
