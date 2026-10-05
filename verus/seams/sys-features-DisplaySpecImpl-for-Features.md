# `DisplaySpecImpl for Features`

Seam S216 of `verus/SEAMS.md`: src/sys/features.rs:200. Kind: spec_impl. Category: core library.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
impl vstd::std_specs::fmt::DisplaySpecImpl for Features {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `sys::features::kani_features::display_features` (`src/sys/features.rs`):
`Features`' `fmt` returns without a panic or undefined behaviour,
under the plain formatter and a padded one and whatever the writer
returns, for every `Features` value (every page size and shift, every
probe flag, every THP setting or none, every PMD size and size set,
every rseq probe), with `Value`'s `fmt` replaced by a stub that
returns `Ok` or `Err` at Kani's choice and writes nothing.

Reproduce: `sh verus/kani/run.sh --harness sys::features::kani_features::display_features` (both quanta;
`--harness` takes the full path).

## Translation

- `fmt_req` is `true`: `fmt` may be called on any `Features` with any
  formatter, and Verus assumes the call returns without a panic: the
  harness, for every value.

The stub: `Value`'s `Display` is a seam of its own (S220, discharged
by Kani's `kani_proofs::fmt::display_feature_value`, which shows that
its `fmt` returns, `Ok` or `Err`, without a panic, for every value and
under the same formatters). The stub returns exactly the results that
harness allows, so the check covers everything else `fmt` runs (the
word pairs, the iterator chain, `write_words`, the keys and
separators) on the real code, and `Value`'s formatting by S220's
result.

Why the stub covers the real `fmt`: `write_words` formats each value
with `write!(f, "{key}={value}")`, which gives `Value`'s `fmt` a
formatter with default options over the same writer, the `{}` case of
S220's harness. The writer (`kani_proofs::fmt::Sink`) keeps no state
and picks each result independently, so what the real `fmt` wrote
before returning changes nothing later; only its result matters, and
the stub returns every result S220 allows. (S220's harness builds
`Value::Word` from two words, while `Features` passes the THP setting
names and `none`; `Word`'s `fmt` is one `write_str` of the word,
whatever it is.)

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
