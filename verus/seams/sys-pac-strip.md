# `sys::pac::strip`

`strip` in `src/sys/pac.rs` (`src/sys/pac.rs:strip` in
`verus/seams.overrides`), one item for both architectures.
Tool: Isla. Status: discharged, with the trap-settings and fixed
configuration residuals of `sign` ([sys-pac-sign.md](sys-pac-sign.md),
part 3). The aarch64 body is `xpacd`; the x86_64 body is plain Rust and
is never reached.

## 1. Contract

Entry S226 of verus/SEAMS.md at 8635213 (`src/sys/pac.rs:264`, kind
external_body):

```rust
pub fn strip(p: usize) -> usize
    requires
        pac_available(),
```

The body Verus does not see, at 8635213:

```rust
#[cfg(target_arch = "aarch64")]
{
    let mut v = p;
    core::arch::asm!(
        ".arch_extension pauth",
        "xpacd {v}",
        v = inout(reg) v,
        options(pure, nomem, nostack, preserves_flags),
    );
    v
}
#[cfg(not(target_arch = "aarch64"))]
{
    p
}
```

(plus a test-only counter, compiled out of the library).

## 2. Statement

As for `sign` ([sys-pac-sign.md](sys-pac-sign.md), part 2: the same
tools, `run.sh` section 6, the same twelve configurations), for
`xpacd`:

1. **Template.** The `asm!` call in `fn strip` is the one above, up to
   whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** As in `sign`'s fact 2: one `xpacd Xd` site at
   8635213 (in `mask`), d not 31.
3. **Isla.** In each configuration, for every `xpacd` assignment at a
   site and for the canonical `xpacd x0`: every path falls through,
   writes only Xd (with the PC and the model's bookkeeping), reads and
   writes no memory and writes no flag (`pac-footprint.job.in`); and
   against `xpacd x0` from an independent state, the same pointer gives
   the same result (`pac-strip-det.job.in`). The instruction reads no
   key and no SCTLR.

## 3. Translation

- **`requires pac_available()`.** As for `sign`: FEAT_PAuth on aarch64
  (`xpacd` is UNDEFINED without it), with the trap-settings residual; no
  x86_64 process meets it.
- **No `ensures`.** Verus assumes only that the call returns some
  `usize`. Fact 3 shows the rest of what the declared interface needs:
  only `v` written, no memory, no flags, a fall-through, and (for
  `pure`) a result that is a function of the operand in a fixed
  configuration.
- **Gaps.** As for `sign` (one TCR value per VA size, the static
  library's code; `xpacd` computes no PAC, so neither `ComputePAC` nor
  the key enters, and FEAT_PAuth2 does not change it); the value `xpacd`
  returns (the pointer with the PAC field replaced by bit 55's copies)
  is not asserted, as nothing relies on it: `mask` uses it, and `mask`
  has no `ensures`.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. It found nothing specific to this record;
its findings on sign's record and run.sh apply here. A human review is
pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes, when the ledger
entry with this record's status has another contract, when the build
emits a PAC-family instruction outside the three shapes or no `xpacd`,
and when any `xpacd` job fails in any configuration. `sh verus/seams.sh
--check` fails when verus/SEAMS.md is stale. No CI job runs `run.sh` yet
(verus/ISLA.md).
