# `core::fmt::Formatter::<'a>::write_fmt`

Seam S298 of `verus/SEAMS.md`: src/vspec/fmt.rs:26. Kind: assume_spec. Category: function pointer.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<'a>[ core::fmt::Formatter::<'a>::write_fmt ](f: &mut core::fmt::Formatter<'a>, args: core::fmt::Arguments<'_>) -> core::fmt::Result;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::misc::formatter_write_fmt` (`src/kani/misc.rs`):
for every string `s` of up to 4 bytes and every `n: u64`, and whatever
the writer returns, these calls return: `f.write_fmt(format_args!(
"challoc"))` (a literal alone), a `write_fmt` of a literal of over 128
bytes between `{}` placeholders of `s` and `n`, and
`f.write_fmt(format_args!("<{}|{:>6}|{:#x}>", s, n, n))` (short
literals, a default placeholder and two with options).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::misc::formatter_write_fmt` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`, no `ensures`: the contract claims only that the call
  returns (with any `fmt::Result`), which the harness shows for every
  writer outcome.

Gap: some shapes of arguments. `write_fmt` is `fmt::write` over the
arguments: a literal-only template is written in one go (the
`as_str` path); otherwise a loop over the template writes each literal
piece (short and long pieces are encoded differently, and both are
run) and calls each argument's own `fmt`, with the default options
(`{}`) or with its placeholder's options (`{:>6}`, `{:#x}`), stopping
at the first error. Not run: widths or precisions taken from
arguments and explicit argument positions, which no format string in
`src` uses. The arguments' own `fmt`s are seams of their own (a
`DisplaySpecImpl` whose `fmt_req` Verus checks where the arguments are
built); what this record adds is that `write_fmt`'s walk over the
template returns. `kani_proofs::fmt::formats` also runs the harness
under a padded formatter, which changes nothing: `write_fmt` gives
each argument its placeholder's options, not the caller's.

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
