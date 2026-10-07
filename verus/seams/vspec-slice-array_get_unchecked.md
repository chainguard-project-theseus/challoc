# `array_get_unchecked`

Seam S382 of `verus/SEAMS.md`: src/vspec/slice.rs:235. Kind: external_body. Category: core library.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub unsafe fn array_get_unchecked<T, const N: usize>(a: &[T; N], i: usize) -> (r: &T)
    requires
        i < N,
    ensures
        *r == a[i as int],
    opens_invariants none
    no_unwind
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::array_get_unchecked` (`src/kani/ptr.rs`):
for arrays of 1, 2 and 6 `u64`s and of 3 `(u64, u8)`s with arbitrary
contents and every index `i < N`, `array_get_unchecked(&a, i)` is
defined and returns a reference to `a[i]` (the same address, so the
same value).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::array_get_unchecked` (both quanta;
`--harness` takes the full path).

## Translation

- `requires i < N`: the harness's assumption.
- `ensures *r == a[i]`: the assertions (value and address).
- `opens_invariants none`, `no_unwind`: it returns without a panic on
  every input checked.

Gap: generic over `T` and `N`; checked at a word and a padded pair, at
four lengths. The body is `a.get_unchecked(i)`, one offset whatever
`N`.

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

Fresh-context subagent, 2026-10-04: the harness against the contract
and this translation, looking for claims the harness does not support;
its findings are folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
