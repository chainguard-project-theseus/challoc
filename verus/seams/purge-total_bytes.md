# `total_bytes`

Seam S189 of `verus/SEAMS.md`: src/purge.rs:712. Kind: external_body. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
fn total_bytes(ranges: &[(usize, usize)]) -> (bytes: u64)
    requires
        lens(ranges@, ranges.len() as int) <= u64::MAX,
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `purge::kani_total::total_bytes_returns` (`src/purge.rs`):
for every list of up to 4 ranges whose lengths sum to at most
`u64::MAX`, `total_bytes` returns (their sum) without a panic.

Reproduce: `sh verus/kani/run.sh --harness purge::kani_total::total_bytes_returns` (both quanta;
`--harness` takes the full path).

## Translation

- `requires lens(ranges@, ranges.len()) <= u64::MAX`: `kani::assume`
  on the sum in `u128`, where it is exact (`lens` is the sum of the
  `.1` fields of the prefix, here the whole slice).
- No `ensures`: the call returns.
- Gap: bounded to 4 ranges; the body is one `sum` over an iterator,
  the same step per range, and its only failure is the overflow the
  `requires` excludes.

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
