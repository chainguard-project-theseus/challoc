# `<&'a mut [T] as core::iter::IntoIterator>::into_iter`

Seam S360 of `verus/SEAMS.md`: src/vspec/slice.rs:43. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub assume_specification<'a, T>[ <&'a mut [T] as core::iter::IntoIterator>::into_iter ](s: &'a mut [T]) -> (iter: core::slice::IterMut<'a, T>)
    ensures
        vstd::std_specs::iter::IteratorSpec::remaining(&iter).len() == old(s)@.len() == final(s)@.len(),
        forall|i: int| #![trigger vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]]
            0 <= i < old(s)@.len() ==> *(vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]) == old(s)@[i],
        forall|i: int|
            #![trigger vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]]
            #![trigger final(s)@[i]]
            0 <= i < old(s)@.len() ==> *final(vstd::std_specs::iter::IteratorSpec::remaining(&iter)[i]) == final(s)@[i],
        vstd::std_specs::iter::IteratorSpec::obeys_prophetic_iter_laws(&iter),
        vstd::std_specs::iter::IteratorSpec::will_return_none(&iter),
        vstd::std_specs::iter::IteratorSpec::decrease(&iter) is Some,
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::slice::slice_iter_mut` (`src/kani/slice.rs`):
for every slice of up to 6 words, iterating `(&mut s[..n]).into_iter()`
yields `n` references, in order, each to the old value of its element,
then `None` (and again `None`); writing a new value through each
leaves the slice holding exactly the values written.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::slice::slice_iter_mut` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`.
- `ensures remaining(iter).len() == old(s)@.len() == final(s)@.len()`:
  `it.len() == n` at the start, `n` items, and the slice's length is
  fixed.
- `ensures *(remaining(iter)[i]) == old(s)@[i]`: each yielded reference
  reads the old element `i`.
- `ensures *final(remaining(iter)[i]) == final(s)@[i]`: what is written
  through reference `i` is the slice's final element `i` (the harness
  writes arbitrary values through every reference and compares).
- `obeys_prophetic_iter_laws`, `will_return_none`, `decrease is Some`:
  `next` hands out `remaining` in order and then `None` for good,
  which the loop and the two final `next` calls show; the decrease is
  the remaining length, which the bound shows finite.
- Gap: the prophetic laws are vstd's encoding; the harness checks the
  behaviour they describe for one full iteration, not every partial
  interleaving of `next` with other uses.

Gap: generic over `T`; Kani checks `u64` elements. The proofs use
these methods at `*mut u8`, `usize` and `(usize, usize)`, plain
`Copy` words, and the methods only move (or compare) elements, so the
type enters only through its size. Bounded: slices up to 6 elements,
every length and every content; the method runs one loop whatever the
length (no length-dependent branch in its `core` implementation),
which the bound covers.

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
