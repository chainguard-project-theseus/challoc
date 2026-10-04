# `<[T; N] as core::iter::IntoIterator>::into_iter`

Seam S382 of `verus/SEAMS.md`: src/vspec/slice.rs:204. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub assume_specification<T, const N: usize>[ <[T; N] as core::iter::IntoIterator>::into_iter ](a: [T; N]) -> (r: <[
    T;
    N
] as core::iter::IntoIterator>::IntoIter)
    ensures
        array_iter_rest(r) == a@,
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::slice::array_into_iter` (`src/kani/slice.rs`):
for every `a: [u64; 3]`, `a.into_iter()` starts with `a` as its rest,
each `next` hands out the front element and drops it from the rest,
and once empty `next` returns `None` and the rest stays empty; for
`[u64; 0]`, `next` is `None` at once.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::slice::array_into_iter` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`.
- `ensures array_iter_rest(r) == a@`: `it.as_slice() == a`
  (`array_iter_rest` is the iterator's remaining elements, which
  `as_slice` returns).

Gap: `T = u64`, `N` in 0 and 3; the iterator moves elements without
looking at them, and its state is the alive range `as_slice` reads.

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
