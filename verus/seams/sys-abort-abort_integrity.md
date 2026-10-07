# `abort_integrity`

Seam S210 of `verus/SEAMS.md`: src/sys/abort.rs:175. Kind: external_body. Category: panic/abort.
Discharged by Kani (the body); the capability relabel it makes is theorem
31's policy (Translation), which Kani cannot check.

## Contract

At commit f079fe76:

```rust
pub fn abort_integrity(args: fmt::Arguments<'_>) -> !
    requires
        crate::sys::may_abort_corrupt(),
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::abort::abort_integrity_reaches_abort_fmt` (`src/kani/abort.rs`):
for a message formatting a symbolic `usize` (the body passes `args` on
unread, so the message is immaterial),
`abort_integrity(args)` reaches `abort_fmt(args)` with no failed check
on the way; `abort_fmt` is stubbed (`kani::stub`) by a function that
panics, since its body ends in raw syscalls (inline assembly, which
Kani does not run).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::abort::abort_integrity_reaches_abort_fmt` (both quanta;
`--harness` takes the full path).

## Translation

- `-> !` (the whole `ensures`): the call never returns. The body's type
  says so already (rustc rejects a `-> !` body that can return), and
  the harness shows it ends in `abort_fmt` on every path with nothing
  failing before; `abort_fmt` is verified by Verus with its own `-> !`
  (it writes the message and raises `SIGABRT`, `sys/abort.rs`).
- `requires may_abort_corrupt()`: what is trusted here is not the body
  but the relabel. `abort_fmt` requires `may_abort()`; this wrapper
  requires only `may_abort_corrupt()` (`may_abort() ||
  integrity_aborts()`), so a caller that holds only `integrity_aborts`
  can abort through it. Both are opaque capabilities with no run-time
  meaning (both are `true`); the relabel is sound for theorem 31
  ("Read-only APIs never abort", verus/THEOREMS.md) because that theorem
  excepts exactly the integrity aborts: `api::tests::abort_grants_only_at_roots`
  lists the callers of `abort_integrity`, and THEOREMS.md says what
  state each one tests. Kani cannot check that; it is the policy the
  theorem states, named here as this record's residual.

The harness is `#[kani::should_panic]`, which Kani passes when some
path panics and every failed check is a panic (as in
`verus/seams/vspec-check-check.md`); the stub's panic is the only
panic on the path, and if the stub were not applied the real
`abort_fmt`'s inline assembly would fail the harness as an unsupported
construct, not a panic. The panic message is not checked (Kani replaces
runtime-formatted messages with a placeholder).

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

Written on 2026-10-05 against commit f079fe76 (the contract above is
the ledger's entry there), and the harness run under both quanta with
the Kani above (it passes). Fresh-context subagent, 2026-10-05: the
harness against the contract and this translation; its findings are
folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
