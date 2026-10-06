# `DebugSpecImpl for slab::State`

Seam S309 of `verus/SEAMS.md`: src/vspec/fmt.rs:58. Kind: spec_impl. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
impl vstd::std_specs::fmt::DebugSpecImpl for crate::slab::State {
    open spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::fmt::debug_slab_state` (`src/kani/fmt.rs`):
every variant of `slab::State` formats with `{:?}` and `{:#?}`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::fmt::debug_slab_state` (both quanta;
`--harness` takes the full path).

## Translation

- `fmt_req` is `true`: no assumption; every variant (the derived
  `Debug` writes the variant's name).

The `DisplaySpecImpl`/`DebugSpecImpl` impls are ghost
(`#[cfg(verus_only)]`). vstd's specification of `fmt` requires
`fmt_req(self, f)` and claims nothing about what is written or
returned, so the seam's content is: under `fmt_req` the call returns
(any `fmt::Result`), with no panic or undefined behaviour. The harness
calls it through `write!` with `formats` (or `formats_debug`) in
`src/kani/fmt.rs`: a writer that keeps nothing and may fail any write
(so every `?` error path runs), under the plain formatter and a padded
one (`{:>24}`; `{:#?}` for `Debug`). Kani fails the harness on any
panic.

Gap: `fmt_req` holds "for every formatter"; the harness checks two
formatter configurations (default, and a width with alignment, which
the integer and `pad`-based output honours and output written through
`write_str`/`write_char` ignores). challoc formats
these values only with `{}` (or `{:?}`) in its own messages.

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
