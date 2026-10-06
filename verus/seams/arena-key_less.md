# `key_less`

Seam S60 of `verus/SEAMS.md`: src/arena.rs:1699. Kind: external_body. Category: core library.
Discharged by Kani.

## Contract

At commit f079fe76:

```rust
fn key_less(a: (u32, bool), b: (u32, bool)) -> (r: bool)
    ensures
        r == key_lt(a, b),
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `arena::kani_key_less::key_less_is_key_lt` (`src/arena.rs`, module `kani_key_less`):
for every two `u32` counts `a.0`, `b.0` (symbolic) and each of the four
pairs of `bool`s `(a.1, b.1)` (as literals), `key_less(a, b)` returns,
without a panic, `key(a) < key(b)` for `key(k) = 2 * k.0 + k.1` (in
`u64`, the `bool` as 0 or 1). The harness matches the two results by
branching on the body's (`if r { want } else { !want }`), with no
`bool` comparison, and covers (`kani::cover!`) both `a.0 == b.0`, where
the `bool`s decide, and `a.0 < b.0`.

Reproduce: `sh verus/kani/run.sh --harness arena::kani_key_less::key_less_is_key_lt` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`: the harness covers every input (2^64 counts, symbolic,
  times the four `bool` pairs, enumerated).
- `ensures r == key_lt(a, b)`: `key_lt(a, b)` is `a.0 < b.0 || (a.0 ==
  b.0 && !a.1 && b.1)` (`src/arena.rs`), which is `2 * a.0 + a.1 < 2 *
  b.0 + b.1` over the integers: the counts differ by at least 1, so a
  smaller count gives a smaller key whatever the `bool`s, and equal
  counts compare the `bool`s as 0 and 1, `false < true`. The harness
  asserts the body's result is that integer comparison.

**Why the `bool`s are literals, and the expected value integers.** Kani
0.68 (CBMC 6.11) leaves `<` on two symbolic `bool`s unconstrained: in a
scratch crate under the same Kani, with `p` and `q` from `kani::any()`
pinned by `assume` to each of the four pairs of values, `kani::cover!`
reaches both `p < q` and `!(p < q)`, and assertions of the correct
value fail, also with `bool`s made from a symbolic integer (`x & 1 ==
1`); the same comparisons on literal `bool`s (through a call, as here,
or `black_box`) are right. A harness with symbolic `bool`s would
therefore reject the correct body, and one whose expected value
compared `bool`s could accept a wrong one. With the `bool`s literals
the body's comparison is evaluated correctly, and the expected value
compares `u64`s, which Kani models bit-precisely. The same scratch
crate checked that the harness is not vacuous: bodies that order `true`
before `false`, that use `<=`, or that ignore the `bool` each fail it,
and the real body (`a < b`) passes. The gap: that Kani evaluates `<` on
literal `bool`s correctly is shown only by those scratch-crate runs (the
literal comparisons give the right values, and the mutants fail), not
by a model of the lowering.

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
