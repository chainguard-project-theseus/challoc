# `debug_scheduled`

Seam S126 of `verus/SEAMS.md`: src/heap/paths.rs:1090. Kind: external_body. Category: panic/abort.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
fn debug_scheduled(allocs: u64, mask: u32)
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `heap::paths::kani_aborts::debug_scheduled_returns_only_if_clear, heap::paths::kani_aborts::debug_scheduled_returns_if_clear` (`src/heap/paths.rs`):
for every `allocs: u64` and `mask: u32`, `debug_scheduled(allocs, mask)`
returns exactly when `(allocs as u32) & mask == 0`: it returns then
(`debug_scheduled_returns_if_clear`), and otherwise panics on every
path (`debug_scheduled_returns_only_if_clear`).

Reproduce: `sh verus/kani/run.sh --harness heap::paths::kani_aborts::debug_scheduled_returns_only_if_clear --harness heap::paths::kani_aborts::debug_scheduled_returns_if_clear` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`, no `ensures` ("nothing is claimed"): a return in
  any state meets the contract, and a non-return is an abort.
- Gap: the body is a `debug_assert!`, so this describes builds with
  debug assertions (Kani's, the tests'); the shipped library compiles it
  out and always returns, which the contract allows too.

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

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
