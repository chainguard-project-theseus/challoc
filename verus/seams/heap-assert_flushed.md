# `assert_flushed`

Seam S117 of `verus/SEAMS.md`: src/heap/mod.rs:1881. Kind: external_body. Category: panic/abort.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b), plus the `requires` theorem 31 ("Read-only APIs never abort")
added after it:

```rust
fn assert_flushed(len: &u32)
    requires
        crate::sys::may_abort() || *len == 0,
    ensures
        *len == 0,
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::kani_aborts::assert_flushed_returns_only_on_zero, heap::kani_aborts::asserts_return_on_zero` (`src/heap/mod.rs`):
for every `len: u32`, `assert_flushed(&len)` returns only if
`len == 0`, and returns on 0 (`asserts_return_on_zero`).

Reproduce: `sh verus/kani/run.sh --harness heap::kani_aborts::assert_flushed_returns_only_on_zero --harness heap::kani_aborts::asserts_return_on_zero` (both quanta;
`--harness` takes the full path).

## Translation

- `requires` is a ghost capability (`may_abort*`, opaque, no exec meaning). The harness covers every input, so a stronger `requires` keeps the `ensures` discharged.
- `ensures *len == 0`: every return has `len == 0`, by the
  `should_panic` harness.

A panic in challoc aborts (`panic = "abort"`, and the panic handler
raises `SIGABRT`), so a call that panics never returns. The harness
uses `#[kani::should_panic]`, which Kani passes when some path panics
and every failed check is a panic, and ends the paths on which the call
must not return with `must_not_return()` (`src/kani/mod.rs`):
reaching it is an "unreachable code" failure, which is not a panic, so
the harness fails if the call returned on any such input (a failed
`kani::assert` or `panic!` there would count as the expected panic).
The panic message is not checked (Kani replaces runtime-formatted
messages with a placeholder). That `must_not_return()` does catch a
return was checked with a mutant in a scratch crate under the same Kani
and nightly: a function that returns on some must-not-return inputs
fails such a harness ("unreachable code"), while the panicking original
passes.

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

Theorem 31, 2026-10-03: the ledger's contract gained the `requires`
above; the contract block and the translation were updated to it, and
the harness was not re-run (it covers every input).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
