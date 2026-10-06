# `u64::rotate_right`

Seam S295 of `verus/SEAMS.md`: src/vspec/core_int.rs:91. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 61768ed6 (added by the port of upstream #101, for the sized
check's `ClassLine::sized_ok`):

```rust
pub assume_specification[ u64::rotate_right ](x: u64, n: u32) -> (r: u64)
    ensures
        r == rotr64(x, n),
    opens_invariants none
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::core_int::u64_rotate_right` (`src/kani/core_int.rs`):
for every `x: u64` and `n: u32` (including `n >= 64`), `x.rotate_right(n) == spec_rotr64(x, n)`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::core_int::u64_rotate_right` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`.
- `opens_invariants none`: the call opens no Verus invariant. A `core`
  function has no ghost code and cannot open one, so there is nothing
  for Kani to check; rustc's type checking of the call is the whole
  claim.
- `ensures r == rotr64(x, n)`: the assertion; `spec_rotr64` is
  `vspec::core_int::rotr64` word for word (`x` for `n % 64 == 0`,
  else `(x >> (n % 64)) | (x << (64 - n % 64))`). The spec's casts of
  the shift counts to `u64` change no value (both are below 64), and
  in the harness both shifts are by less than 64, so neither overflows
  in the debug build Kani checks.

The inputs are `kani::any()` machine integers with no bound
other than the contract's `requires`, so the check is exhaustive; the
harness has no loop to unwind.
The spec
function is a Verus `spec fn`, erased from builds, so the harness
restates it as the exec `spec_rotr64` in `src/kani/core_int.rs`; it
needs no `u128`: the spec's one `int` operation, `64 - n % 64`, is
between 1 and 63 in the else branch, so the harness's `u32`
subtraction gives the same value, and the rest stays in `u32` and
`u64`.

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

Written 2026-10-06 at commit 61768ed6, from the `u32::rotate_right`
record (S292: the same harness shape, the same kind of seam): the
contract above is the ledger's entry there, and `spec_rotr64` was
compared with `rotr64` term by term. A fresh-context subagent
reviewed this record against the harness and the contract; its
findings are folded in.

Run at commit 61768ed6, 2026-10-06, with Kani 0.68.0 under both
quanta: `kani_proofs::core_int::u64_rotate_right` passes (0 of 35
checks failed in each).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
