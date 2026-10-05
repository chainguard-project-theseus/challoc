# `Config::set_listed`

Seam S100 of `verus/SEAMS.md`: src/config.rs:350. Kind: external_body. Category: function pointer.
Discharged by Kani.

## Contract

At commit 7d4e997 plus the `requires` this record's commit adds (the
ledger has this block from that commit on; the record was written at
ae82d5b):

```rust
fn set_listed<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Option<Result<(), Issue<'a>>>)
    requires
        old(self).wf(),
    ensures
        r is None <==> !listed(key@),
        r is None ==> *final(self) == *old(self),
        old(self).wf() ==> final(self).wf(),
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `config::kani_config::set_listed_contract` (`src/config.rs`):
for every `Config` with `wf()` (every field arbitrary), every
key that is a `TABLE` entry's name or any byte string of up to 4 bytes,
and every value of up to 8 bytes: `set_listed` returns `None` exactly
when `!listed(key)`, leaves the config unchanged when it returns
`None`, and leaves it `wf()`.

Reproduce: `sh verus/kani/run.sh --harness config::kani_config::set_listed_contract` (both quanta;
`--harness` takes the full path).

## Translation

- `requires old(self).wf()`: `kani::assume(wf(&old))`. This lane
  added the `requires` (with `Config::set`, `apply_pair`, `parse`
  and `parse_from` passing it on; Verus checks every caller). Kani
  found the case: when `guard_slab_interval`'s value does not parse,
  the field keeps its old value and the parser rounds it up with
  `u32::next_power_of_two`, which overflows (a panic in the tests'
  build, a wrap to 0 in the shipped one) for an old value above `2^31`
  that is not a power of two. A well-formed config never holds one, and
  every config the program builds is well formed (`Config::DEFAULT` is,
  and every step keeps it), so nothing changed at run time; the
  `requires` makes the body panic-free under its contract (the policy
  below).
- `ensures r is None <==> !listed(key@)`: the first assertion.
- `ensures r is None ==> *final(self) == *old(self)`: the second, with
  the derived `PartialEq` of `Config` (field-wise `==` of integers,
  bools, fieldless enums and an `Option<i64>`, i.e. structural
  equality).
- `ensures old(self).wf() ==> final(self).wf()`: the third;
  `kani_config::wf` is `Config::wf` with `is_pow2` read as
  `is_power_of_two` (the two agree: `is_pow2(x)` is
  `exists k < 64, x == pow2(k)`, checked in
  `verus/seams/vspec-core_int-usize-is_power_of_two.md`).
- Gap: values up to 8 bytes. That reaches past every bound a `wf`
  field is clamped to (`0x` and six hex digits is 16 Mi, above
  `MAX_GUARD_SLAB_INTERVAL`; six decimal digits pass `MAX_NSLOTS`),
  so every clamp, rounding and error branch of the parsers runs; a
  longer value differs only in the digits `parse_int` (verified by
  Verus, `r == parse_int_spec(value@)`) folds in. Keys: every entry,
  plus arbitrary short keys for the unlisted case.

`listed` is an uninterpreted spec function that three seams name
(`Config::set_listed`, `Config::get`, `KEYS`); they are consistent
when it is read as "some `TABLE` entry has this name", and every
harness checks its contract under that one reading, which
`kani_config::listed` spells as a loop over `TABLE` (independently of
`Key::find`, which the bodies use).

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
