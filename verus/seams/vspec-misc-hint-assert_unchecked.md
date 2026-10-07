# `core::hint::assert_unchecked`

Seam S339 of `verus/SEAMS.md`: src/vspec/misc.rs:89. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 61768ed6 (added by the port of upstream #101):

```rust
pub assume_specification[ core::hint::assert_unchecked ](cond: bool)
    requires
        cond,
    opens_invariants none
    no_unwind
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::misc::hint_assert_unchecked` (`src/kani/misc.rs`):
for every `cond: bool` with `cond` true (`kani::assume(cond)`),
`core::hint::assert_unchecked(cond)` returns without a panic and with
no undefined behaviour Kani detects.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::misc::hint_assert_unchecked` (both quanta;
`--harness` takes the full path).

## Translation

- `requires cond`: the harness's `kani::assume(cond)`. It is also
  `core`'s safety contract for the function: with `cond` false the
  call is undefined behaviour, which the debug build's precondition
  check (`ub_checks`) turns into an abort and the release build into
  an `assume` the optimiser may use. The `requires` is what makes
  every verified call defined; the harness shows the call is harmless
  when it holds.
- `opens_invariants none`: the call opens no Verus invariant. A
  `core` function has no ghost code and cannot open one, so there is
  nothing for Kani to check; rustc's type checking of the call is the
  whole claim.
- `no_unwind`: the call returns (no panic) under the `requires`: the
  debug build's precondition check does not fire. In Kani's terms, of
  the harness's 4 checks, 3 are on that check's panic path (the
  message's formatting, unreachable; `Location::caller` and the call
  to `rust_begin_unwind`, constructs Kani does not support: each is an
  `assert(false)`, so its SUCCESS means it is not reached) and none is
  reached; the 4th is
  Kani's assertion that the `assume` intrinsic's condition holds,
  which passes.
- No `ensures`: nothing about the call is claimed beyond its
  returning. (Its release effect, the optimiser's `assume(cond)`, is
  sound exactly because the `requires` holds.)

The input is a `kani::any()` `bool` constrained only by the
contract's `requires`, so the check is exhaustive (it is the one
value `true`); the harness has no loop to unwind.

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

Written 2026-10-06 at commit 61768ed6: the contract above is the
ledger's entry there, and the harness was compared with each clause.
A fresh-context subagent reviewed this record against the harness and
the contract; its findings are folded in.

Run at commit 61768ed6, 2026-10-06, with Kani 0.68.0 under both
quanta: `kani_proofs::misc::hint_assert_unchecked` passes (0 of 4
checks failed in each, 1 unreachable).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
