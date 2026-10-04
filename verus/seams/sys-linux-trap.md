# `sys::linux::trap`

The `trap` in `src/sys/mod.rs`, one item for both architectures
(`src/sys/mod.rs:trap` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with Linux's handling of the exception
and the platform facts of part 3 as named residual assumptions: its
aarch64 half on the Armv8.5-A model and its x86_64 half on the x86-64
model.

## 1. Contract

Entry S221 of verus/SEAMS.md as numbered at 8635213 (S225 and
`src/sys/mod.rs:1077` at 64d801a), kind external_body;
`#[inline(always)]`:

```rust
pub fn trap() -> !
    requires
        crate::sys::may_abort(),
```

The bodies Verus does not see, at 64d801a: on x86_64

```rust
core::arch::asm!("ud2", options(noreturn, nostack));
```

and on aarch64

```rust
core::arch::asm!("brk #0x1", options(noreturn, nostack));
```

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 7
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above is, up to
   whitespace, an `asm!` call in `fn trap` (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** Every `brk` in the aarch64 release build
   is `brk #0x1`, encoding `d4200020` (7 at 8635213; LLVM's own traps
   are the same instruction).
3. **Isla, aarch64.** `brk #0x1` at Non-secure EL0 in AArch64 state on
   the Sail Armv8.5-A model, the PC and vector bases symbolic. With
   HCR_EL2 and MDCR_EL2 clear (`brk1.job`) every trace takes the
   software breakpoint exception to EL1: ESR_EL1 = 0xf2000001 (EC 0x3C,
   BRK from AArch64; IL set; comment 1), ELR_EL1 = the `brk`'s own
   address, the PC at VBAR_EL1's vector for a synchronous exception from
   a lower EL using AArch64, PSTATE at EL1 on SP_EL1. With HCR_EL2 as
   for a VHE host (`brk1-vhe.job`) the same at EL2. No trace writes a
   user register or memory, and none falls through.
4. **Compiled code, x86_64.** Every `ud2` in the x86_64 release build is
   `0f 0b` (7 at 64d801a; LLVM's own traps are the same instruction),
   and there is no other `ud` instruction.
5. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), `ud2` from a symbolic state, with the model's
   `x86_fault` traced (`x86/ud2.job`), under the residual fact that the
   code is a user address (below `2^47`): one of its three traces is
   feasible, and on it the model raises its architectural fault through
   `x86_fault` (the UD2 decoder's `X86-ILLEGAL-INSTRUCTION`, the ACL2
   model's #UD), writes no register (not even rip) and no memory, and
   does not complete (the checker requires the fault, the call and no
   rip write); the other two end in the model's non-canonical
   instruction pointer error (`add_to_iptr`, a model error), infeasible
   under the assumption.

## 3. Translation

`-> !`: the function never returns. Facts 3 and 5: the instruction
never falls through. On aarch64 it enters the kernel with the
breakpoint exception and, the return address being the `brk` itself, a
handler that returns re-executes it, so control never reaches the next
instruction; Linux turns the exception into `SIGTRAP`. On x86_64 it
raises the invalid-opcode fault, which leaves rip at the `ud2` (a
fault's saved rip is the faulting instruction's), so a handler that
returns re-executes it; Linux turns #UD into `SIGILL`. Either signal's
default action ends the process (residual: the kernel's handling; a
program that catches the signal and longjmps out leaves `trap` without
returning from it). `nostack`: no memory written. The `requires
may_abort()` is the capability theorem 32 (read-only APIs never abort,
verus/THEOREMS.md) rests on: every function that can end the process
requires it, as this one, which ends it, does. It says nothing about the
machine and maps to no precondition of the instruction. Gaps, aarch64:
with MDCR_EL2.TDE set (a hypervisor debugging its guest) the exception
goes to EL2 instead, which the job does not cover; it still never falls
through in the model's traces. Gaps, x86_64: the model's fault is an
exception in the Sail model, not an exception entry (no IDT, no kernel
state is modelled); that it is #UD is read from the model's source (the
UD2 case of its two-byte opcode dispatch), the check being that the
fault goes through `x86_fault` and not through `x86_model_error`; and
as in [api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md),
part 3 (64-bit mode by assumption, the application view, the model and
the checker).

## 4. Review

The aarch64 half was written on 2026-10-03 as an evidence note and
reviewed then by a fresh-context reviewer (a Claude subagent with no
prior context of this work), told to look for translations that
overclaim; it found nothing specific to it. The x86_64 half and the
record were written on 2026-10-04 and reviewed then by a fresh-context
reviewer (a Claude subagent with no prior context of this work), told to
look for checker holes and overclaiming translations. It found no
unsound discharge; its findings are folded in: `trap` now requires that
the fault path write no rip (a rip write was accepted, and
sys-linux-trap.md relies on rip staying at the `ud2`; a control checks
it), the description of `ud2`'s infeasible paths, one encoding per
register in the TLS and `fs:[0]` census, a stricter `arch_at`, a
cross-check of the 64-bit mode assumption against the `syscall` job, the
wording of the template fact, and the seam numbers at 64d801a. A human
review is pending.

## 5. Tests

`verus/isla/run.sh` fails when either `asm!` call changes, when the
ledger entry with this record's status has another contract, when the
build has a `brk` other than `brk #0x1` or none, or a `ud` instruction
other than `0f 0b` or no `ud2`, when `brk1.job`, `brk1-vhe.job` or
`x86/ud2.job` fails, and when a negative control passes (a wrong
syndrome or `brk` claimed to fall through; `ud2` claimed to fall
through, to fault through the model-error path, or to write rip before
its fault).
