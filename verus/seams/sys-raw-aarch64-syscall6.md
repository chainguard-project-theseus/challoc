# `sys::raw::syscall6` (aarch64)

The aarch64 `syscall6` in `src/sys/raw.rs` (the second seam of that
name in the file; `src/sys/raw.rs:syscall6#2` in
`verus/seams.overrides`). Tool: Isla. Status: discharged, with the
kernel's syscall ABI as a named residual assumption (part 3). The x86_64
`syscall6` (the first) stays trusted: there is no x86_64 Sail model
for Isla (verus/ISLA.md).

## 1. Contract

Entry S240 of verus/SEAMS.md at 8635213 (`src/sys/raw.rs:248`, kind
external_body, one `requires`, the system-call capability of theorem 8
(`verus/THEOREMS.md`), no `ensures`; at d865271, where the record was
first checked, the same with no `requires`):

```rust
pub unsafe fn syscall6(
    nr: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> usize
    requires
        crate::sys::may_syscall(),
```

The body Verus does not see, at d865271:

```rust
asm!("svc 0", in("x8") nr, inlateout("x0") a0 => ret, in("x1") a1,
     in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, options(nostack));
```

## 2. Statement

Isla (rems-project/isla e9b5d94, `isla-footprint -s`) on the Sail
Armv8.5-A model (`armv8p5.ir`, isla-snapshots d8b3101), traces composed
and checked by `verus/isla/check.rs` with Z3 4.16.0. Reproduce with
`verus/isla/run.sh` (setup in verus/ISLA.md). Three facts:

1. **Template.** The `asm!` call in `fn syscall6` is the one above, up
   to whitespace (`verus/isla/templates.txt`), and the ledger entry the
   status names is the one under `#[cfg(target_arch = "aarch64")]`.
2. **Compiled code.** Every `svc`, `hvc` and `smc` in the aarch64
   release build is `svc #0`, encoding `d4000001` (266 instances at
   d865271).
3. **Isla.** `svc #0` executed at Non-secure EL0 in AArch64 state, with
   the PC and the vector bases symbolic. With HCR_EL2 clear
   (`verus/isla/svc0.job`, four traces, all feasible) every trace takes
   the SVC exception to EL1: `ESR_EL1 = 0x56000000` (EC 0x15, SVC from
   AArch64; IL set; imm16 0), `ELR_EL1 = PC + 4`, `PC =
   VBAR_EL1[63:11] : 0x400` (the vector for a synchronous exception from
   a lower EL using AArch64), and PSTATE ends at EL1 on SP_EL1. With
   HCR_EL2 as Linux sets it for a VHE host (E2H, TGE, RW;
   `svc0-vhe.job`, eight traces) the same holds at EL2, with ESR_EL2,
   ELR_EL2 and VBAR_EL2. No trace writes X0-X30, SP_EL0, the SIMD/FP
   registers, FPCR, FPSR or TPIDR_EL0 (the model has no SVE), and none
   reads or writes memory.

## 3. Translation

The contract has no `ensures`: Verus assumes only that
the call returns some `usize` and changes nothing it tracks. The kernel's
effects (the mapping `mmap` creates, the bytes `read` stores) are the
contracts of the `sys` wrappers, separate seams (FFI/syscall, tool none),
or, where a verified wrapper passes the address of a Rust local for the
kernel to write (`clock_gettime`, `ncpus`, `block_all_signals`), the
`&mut` borrow that made the address: `ptr::from_mut` and
`<[T]>::as_mut_ptr` state nothing of their referent afterwards, so Verus
treats it as overwritten (verus/audit/c-boundary.md).
What the stub itself must do is the asm block's interface: enter the
kernel with the syscall number in x8 and the arguments in x0-x5, leave
every register the block does not declare as an output as it was, and
resume at the next instruction with the result in x0.

- **The `requires`.** `may_syscall()` is a capability with no meaning
  (an opaque `true`): it restricts which verified callers may reach the
  stub and asks nothing of the machine state, so it neither adds to nor
  takes from what Isla must show. What Isla ran from is EL0 in AArch64 state,
  Non-secure, with the PC and the vector bases symbolic and these pinned:
  PSTATE.{EL,nRW}, SCR_EL3 = 0x401 (NS, RW), HCR_EL2 = 0 (a kernel at
  EL1) or E2H, TGE and RW (a VHE kernel at EL2), and the configuration's
  defaults for the rest (`isla/configs/armv8p5.toml`: SCTLR_EL1 =
  0x04000000, so the MMU is off; MDSCR_EL1 = 0, so no single-step;
  TCR_ELx = 0; every EL AArch64). Linux user space runs in one of the
  two pinned configurations. Gap: the defaults stand in for the kernel's
  own settings, and that the SVC's routing, syndrome and vector do not
  depend on them is read from the architecture, not checked; the MMU
  setting is irrelevant here, as `svc` accesses no memory.
- **The operands.** Facts 1 and 2 say the compiled stub is `svc #0`
  with the bindings above; that rustc and LLVM place `nr` in x8 and the
  arguments in their named registers before the instruction and read
  `ret` from x0 after it is the compiler's inline-assembly semantics,
  trusted here as for every Verus result.
- **Returns some `usize`, nothing else changes.** Fact 3: the instruction
  writes no user register and no memory and hands control to the kernel
  with the return address `PC + 4`. The rest is the residual assumption,
  **the Linux arm64 syscall ABI** for the calls challoc makes (`mmap`,
  `munmap`, `mprotect`, `madvise`, `mseal`, `prctl` (`PR_SET_NAME`,
  `PR_SET_VMA`), `futex`, `read`, `write`, `openat`, `close`,
  `clock_gettime`, `getrandom`, `getpid`, `gettid`, `sched_getaffinity`,
  `rt_sigaction`, `rt_sigprocmask`, `tgkill`, `exit_group`; none of
  `execve`, `clone` or `rt_sigreturn`, which change registers by design;
  `arch/arm64/kernel/entry.S`, `syscall.c`,
  `Documentation/arch/arm64/sve.rst`): the kernel takes the number from
  x8 and the arguments from x0-x5, and either never returns
  (`exit_group`, a fatal signal) or returns to `ELR_EL1` (`ELR_EL2`
  under VHE) with the result in x0 and x1-x30, SP_EL0, PSTATE.NZCV,
  FPCR, FPSR, TPIDR_EL0 and the low 128 bits of V0-V31 as they were. It
  discards SVE state above those 128 bits; no value lives there, since
  challoc's build targets `armv8-a` without SVE. Under that assumption
  the block's declared interface (output x0, no clobbers, `nostack`,
  memory not `nomem`) is exact. `nostack` holds because `svc` itself
  writes no memory (fact 3) and the kernel writes the user stack only to
  deliver a signal, which can interrupt any instruction; aarch64 has no
  red zone, so no live data sits below sp.
- **Gaps.** The model is the ASL-derived Armv8.5-A snapshot, not the
  CPU; Isla runs one instruction from a symbolic state, with no
  interrupts or debug events modelled; the kernel ABI above is assumed,
  not checked.

## 4. Review

Translation reviewed on 2026-10-02 by a fresh-context reviewer (a Claude
subagent with no prior context of this work). Its findings on this
record are folded in: the pinned system-register values listed as the
precondition instead of "any state", FPCR, FPSR and TPIDR_EL0 added to
the ABI's preserved state and the ABI scoped to challoc's syscalls, the
VHE job given Linux's HCR_EL2 value, the target EL asserted, the
architecture of the ledger entry checked, and two soundness holes in
`check.rs` (an inexpressible write could be read back as the initial
value; writes through a field accessor were not checked) fixed, with
negative controls for both. A human review is pending.

On 2026-10-03 the C and kernel boundary audit (verus/audit/c-boundary.md)
found part 1 quoting the contract of d865271, which the record check of
`verus/isla/run.sh` (part 1 of its checks) rejected against the ledger
at 8635213, and revised parts 1 and 3 for the current contract. The asm
block is unchanged, so the Isla facts stand; `run.sh` has not been
rerun since (no Isla toolchain in that environment), and the revision
awaits the same fresh-context and human review as the rest.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (fact 1), when
the ledger entry with this record's status has another contract or is
not the aarch64 item (fact 1), when the aarch64 build emits an `svc`,
`hvc` or `smc` other than `svc #0`, or none at all (fact 2), when
Isla's trace of `svc #0` no longer meets `svc0.job` and
`svc0-vhe.job` (fact 3), and when a negative control passes (the checker
accepting a false property). `sh verus/seams.sh --check` fails when
verus/SEAMS.md is stale, so a changed contract changes the ledger and
then fails this record's check. No CI job runs `run.sh` yet: it needs
the Isla toolchain (verus/ISLA.md).
