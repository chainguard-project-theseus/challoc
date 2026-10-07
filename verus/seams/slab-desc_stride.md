# `desc_stride`

Seam S205 of `verus/SEAMS.md`: src/slab.rs:264. Kind: external_body. Category: core library.
Discharged by Kani.

## Contract

At commit 82ca40d7:

```rust
pub fn desc_stride(class: usize) -> (r: usize)
    requires class < NCLASSES,
    ensures r <= u32::MAX, r == stride_of(class as int),
```

The body, at that commit: `DESC_STRIDE[class] as usize`, a read of the
`exec static DESC_STRIDE: [u32; NCLASSES]` (built by the `const fn`
`build_strides`), widened.

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `slab::kani_desc_stride::desc_stride_reads_the_table` (`src/slab.rs`):
for every `class < NCLASSES`, `desc_stride(class)` returns without a
panic, its result is at most `u32::MAX`, equals
`DESC_STRIDE[class] as usize`, and a second call returns the same.

Reproduce: `sh verus/kani/run.sh --harness slab::kani_desc_stride::desc_stride_reads_the_table` (both quanta;
`--harness` takes the full path).

## Translation

- `requires class < NCLASSES`: `kani::assume`; the index is then in
  bounds (the harness would fail on the panic otherwise).
- `ensures r <= u32::MAX`: asserted.
- `ensures r == stride_of(class as int)`: `stride_of` is an
  uninterpreted spec function, "the stride `desc_stride` reads for
  class `c`", with no axiom about it; the `ensures` says only that the
  result is a function of `class`, the same on every call. The harness
  shows the result is `DESC_STRIDE[class]` widened, and `DESC_STRIDE` is
  an immutable `static` of plain `u32`s (no `static mut`, no interior
  mutability), so its entries are fixed for the life of the process and
  `stride_of(c)` is `DESC_STRIDE[c] as usize`. The two-call assertion
  checks the same within one run.
- No bound: `class` ranges over every value below `NCLASSES`, under
  both quanta (`NCLASSES` and the table differ between them). The
  table's values are the compiled `const` evaluation's, the one the
  release build uses.

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

Written on 2026-10-05; reviewed then by a fresh-context subagent (a
Claude subagent with no prior context of this work), told to look for
overclaiming translations.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
