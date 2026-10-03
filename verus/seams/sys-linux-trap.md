# `sys::linux::trap` (aarch64 half)

Evidence note, not a discharge: entry S221 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla below; its x86_64 half
(`ud2`) is not, as there is no x86_64 Sail model for Isla (verus/ISLA.md,
plan item 1). When it is, this note becomes the seam's record.

## 1. Contract

Entry S221 of verus/SEAMS.md at 8635213 (`src/sys/mod.rs:1029`, kind
external_body, no `requires`, no `ensures`; `#[inline(always)]`):

```rust
pub fn trap() -> !
```

The aarch64 body Verus does not see, at 8635213:

```rust
core::arch::asm!("brk #0x1", options(noreturn, nostack));
```

## 2. Statement (aarch64)

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A model, checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, section 7.

1. **Template.** The aarch64 `asm!` call in `fn trap` is the one above
   (`verus/isla/templates.txt`).
2. **Compiled code.** Every `brk` in the aarch64 release build is
   `brk #0x1`, encoding `d4200020` (7 at 8635213; LLVM's own traps are
   the same instruction).
3. **Isla.** `brk #0x1` at Non-secure EL0 in AArch64 state, the PC and
   vector bases symbolic. With HCR_EL2 and MDCR_EL2 clear (`brk1.job`)
   every trace takes the software breakpoint exception to EL1: ESR_EL1 =
   0xf2000001 (EC 0x3C, BRK from AArch64; IL set; comment 1), ELR_EL1 =
   the `brk`'s own address, the PC at VBAR_EL1's vector for a
   synchronous exception from a lower EL using AArch64, PSTATE at EL1 on
   SP_EL1. With HCR_EL2 as for a VHE host (`brk1-vhe.job`) the same at
   EL2. No trace writes a user register or memory, and none falls
   through.

## 3. Translation (aarch64 half)

`-> !`: the function never returns. Fact 3: the instruction never falls
through; it enters the kernel with the breakpoint exception and, the
return address being the `brk` itself, a handler that returns
re-executes it, so control never reaches the next instruction. Linux
turns the exception into `SIGTRAP`, whose default action ends the
process (residual: the kernel's handling; a program that catches
`SIGTRAP` and longjmps out leaves `trap` without returning from it).
`nostack`: no memory written. With MDCR_EL2.TDE set (a hypervisor
debugging its guest) the exception goes to EL2 instead, which the job
does not cover; it still never falls through in the model's traces.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. It found nothing specific to this note. A
human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the aarch64 `asm!` call changes, when the
build has a `brk` other than `brk #0x1` or none, when `brk1.job` or
`brk1-vhe.job` fails, and when their negative controls (a wrong
syndrome; `brk` claimed to fall through) pass.
