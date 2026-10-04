# `sys::raw::syscall1` (x86_64)

The x86_64 `syscall1` in `src/sys/raw.rs` (the first seam of that
name in the file; `src/sys/raw.rs:syscall1#1` in
`verus/seams.overrides`). Tool: Isla. Status: discharged, with the
kernel's syscall ABI and Linux's user-mode processor state as named
residual assumptions (part 3). The aarch64 `syscall1` (the second) has its
own record ([sys-raw-aarch64-syscall1.md](sys-raw-aarch64-syscall1.md)).

## 1. Contract

Entry S228 of verus/SEAMS.md at 8635213 (`src/sys/raw.rs:35`, kind
external_body, `requires crate::sys::may_syscall()`, no `ensures`):

```rust
pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

The body Verus does not see, at 8635213:

```rust
asm!("syscall", inlateout("rax") nr => ret, in("rdi") a0,
     lateout("rcx") _, lateout("r11") _, options(nostack));
```

## 2. Statement

Isla (rems-project/isla e9b5d94, `isla-footprint -s --pessimistic`) on the
Sail x86-64 model translated from the ACL2 x86isa model
(rems-project/sail-x86-from-acl2 0c6c023, `model/` with its
test-generation patches) and compiled for Isla by Sail 5f46c04 with
Isla's `isla-sail` plugin, together with challoc's Isla entry
(`verus/isla/x86/`: the instruction's bytes served to the model's
instruction fetches, a model fault recorded as `isla_fault`) and
configuration (`verus/isla/x86/x86.toml`: the model's system view);
traces composed and checked by `verus/isla/check.rs` with Z3 4.16.0.
Reproduce with `verus/isla/run.sh` (setup in verus/ISLA.md). Three
facts:

1. **Template.** The `asm!` call in `fn syscall1` is the one above, up
   to whitespace (`verus/isla/templates.txt`), and the ledger entry the
   status names is the one under `#[cfg(target_arch = "x86_64")]`.
2. **Compiled code.** Every `syscall`, `sysenter` and `int` in the
   x86_64 release build is `syscall`, encoding `0f 05` (267 instances
   at 8635213).
3. **Isla.** `syscall` executed with every register symbolic except
   the model's mode flags, under the assumptions of part 3
   (`verus/isla/x86/syscall.job`): of Isla's 71 traces exactly one is
   feasible, and on it RCX is the address of the next instruction (RIP +
   2), R11 is RFLAGS (zero-extended: the model's RFLAGS is its low 32
   bits), RIP is IA32_LSTAR, RFLAGS loses the bits set in IA32_FMASK, the
   CS selector is IA32_STAR[47:32] with RPL 0 and the SS selector that
   plus 8, and their hidden descriptor parts are flat: base 0, limit
   4 GiB, CS an accessed readable 64-bit code segment (L = 1, D = 0) and
   SS an accessed writable data segment (B = 1), both DPL 0 and present,
   the other attribute bits kept. Nothing else is written:
   no other general-purpose register (RAX, RBX, RDX, RSI, RDI, RSP, RBP,
   R8-R10, R12-R15), no ZMM register, neither FS nor GS (selector, base
   or the base MSRs), no other MSR, control or segment register the
   model has. No memory is read or written, and the model raises no
   fault. Isla ran with `--pessimistic`: no assertion in the model
   can fail on any of the traces, so none is assumed to hold.

## 3. Translation

The contract has one `requires` and no `ensures`: Verus assumes only
that the call returns some `usize` and changes nothing it tracks.
`may_syscall()` is an opaque ghost capability (`true`, "a capability,
not a claim about the world", `src/sys/mod.rs`) that restricts which
verified callers may reach this stub: theorem 8 (no system call on the
fast paths, verus/THEOREMS.md) rests on every stub that enters the
kernel requiring it, as this one does (part 2). It says nothing about
the machine, so it maps to no precondition of the instruction. The kernel's effects (the
mapping `mmap` creates, the bytes `read` stores) are the contracts of
the `sys` wrappers, separate seams (FFI/syscall, tool none). What the
stub itself must do is the asm block's interface: enter the kernel with
the syscall number in rax and the argument in rdi, leave every register the block does
not declare as an output (rax) or clobber (rcx, r11) as it was, and
resume at the next instruction with the result in rax.

- **What Isla ran from.** Every register symbolic except the model's
  mode flags (`x86.toml`: the system view, in which SYSCALL is the
  SDM's rather than the application view's emulated system call; the
  model neither halted nor faulted), and the job's assumptions, each of
  them needed (without any one the job fails: a feasible trace faults,
  takes another path than SYSCALL's or breaks the RCX assertion;
  `run.sh`'s `sys-sce`, `sys-lma`, `sys-csl` and `sys-star` controls
  keep the first three and the last checked, the other two were checked
  by hand on 2026-10-03):
  IA32_EFER.SCE and IA32_EFER.LMA set and CS.L set (64-bit mode with SYSCALL enabled:
  Linux sets SCE in `arch/x86/kernel/head_64.S` and runs 64-bit tasks with
  `__USER_CS`, a long-mode code segment); the instruction at a user
  address below 2^47 - 2 (Linux's x86_64 user space ends at 2^47 - 4096
  for a mapping that does not ask for an address above 2^47 under 5-level
  paging, and the loader maps challoc's code without such a hint; the
  model has 48-bit canonical addresses only); IA32_LSTAR canonical
  (WRMSR faults on a
  non-canonical value); and IA32_STAR[47:32] + 8 below 2^15, a check the
  model makes and the processor does not (Linux: `__KERNEL_CS`, 0x10).
- **The model.** The ACL2 x86isa model, machine-translated to Sail (the
  translator and the snapshot are research artifacts, validated by
  testing). Its SYSCALL follows the SDM's pseudocode for the registers
  above, but it has no x87/MMX state, MXCSR, AVX-512 mask registers,
  PKRU, XCR0 or CET shadow-stack state, so fact 3 says nothing about
  them; the SDM's SYSCALL writes none of them that user code can see
  (with supervisor shadow stacks it saves SSP to IA32_PL3_SSP, an MSR).
  The entry serves instruction fetches from the opcode instead of memory
  through paging, so instruction-fetch faults are not modelled (the
  kernel resolves them and the instruction restarts).
- **Which code.** Fact 2 reads the x86_64 static library built with the
  release profile (fat LTO, one codegen unit) from the same sources, so
  it covers the code before the final link. The shipped shared library
  is a separate LTO run: `syscall` has no operand fields, so every
  `syscall` there is the instruction fact 3 traced, but that it holds no
  other kernel entry (`sysenter`, `int`) is not checked until `run.sh`
  reads the linked library (verus/ISLA.md, plan).
- **The operands.** Facts 1 and 2 say the compiled stub is `syscall`
  with the bindings above; that rustc and LLVM place `nr` in rax and
  the arguments in their named registers before the instruction and read
  `ret` from rax after it is the compiler's inline-assembly semantics,
  trusted here as for every Verus result.
- **Returns some `usize`, nothing else changes.** Fact 3: the
  instruction writes no register but RCX, R11, RFLAGS and the kernel's
  CS and SS, no memory, and enters the kernel at IA32_LSTAR with the
  return address in RCX and the user RFLAGS in R11. The rest is the
  residual assumption, **the Linux x86_64 syscall ABI** for the calls
  challoc makes (`mmap`, `munmap`, `mprotect`, `madvise`, `mseal`,
  `prctl` (`PR_SET_NAME`, `PR_SET_VMA`), `futex`, `read`, `write`,
  `openat`, `close`, `clock_gettime`, `getrandom`, `getpid`,
  `gettid`, `sched_getaffinity`, `rt_sigaction`, `rt_sigprocmask`,
  `tgkill`, `exit_group`; none of `execve`, `clone` or
  `rt_sigreturn`, which change registers by design;
  `arch/x86/entry/entry_64.S`, the System V x86-64 psABI, appendix
  A.2): the kernel takes the number from rax and the arguments from rdi,
  rsi, rdx, r10, r8 and r9, and either never returns (`exit_group`, a
  fatal signal) or returns, by SYSRET or IRET, to RCX with RFLAGS from
  R11, the result in rax, and every other general-purpose register, RSP,
  the FS and GS bases and the x87, SSE and AVX state as they were. Under
  that assumption the block's declared interface (rax in and out, rcx and
  r11 clobbered, `nostack`, memory not `nomem`, flags not preserved)
  is exact, and DF is clear on return as it was on entry (RFLAGS comes
  back from R11). `nostack` holds because `syscall` writes no memory
  (fact 3), the kernel runs on its own stack, and a signal delivered on
  return lands below the 128-byte red zone under RSP
  (`arch/x86/kernel/signal.c`), so nothing the compiler keeps there is
  overwritten.
- **Gaps.** The model is a translation of a formal model, not the CPU;
  Isla runs one instruction from a symbolic state, with no interrupts or
  debug events modelled; the kernel ABI above is assumed, not checked;
  `check.rs` composes the trace and is part of the trusted base.

## 4. Review

Written on 2026-10-03 by the x86 Isla lane, and reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), told to look for checker holes and overclaiming translations. Its
findings, nothing serious: the negative controls (a `sys-star` control;
each control to
fail with its own reason), the CS and SS descriptor parts asserted
rather than only allowed, x86 jobs required to assert `final.rip`, the
source of EFER.SCE (`head_64.S`, not `syscall_init`), dated trace
counts and a "Which code" paragraph. Those fixes had not reached the
lane's pushed commit; they were redone when the lane was merged onto
64d801a on 2026-10-04, and the merged harness and these records were
reviewed again by a fresh-context reviewer told to look for checker
holes and overclaiming translations. Its findings are folded in: a
field write with a plain value now updates that field (it left a stale
`final.R.F`, latent: the real traces write whole structs), the
placeholder check sees hyphenated names, the abnormal-path report
names its cause (the `sys-sce` and `sys-star` controls now match "a
model fault"), an assert naming a value its combination leaves
undefined fails instead of stopping the checker (so the LMA and CS.L
assumptions got controls), the census counts `int3`/`int1`, the
discharge counts in verus/ISLA.md, and the `may_syscall` paragraph now
says theorem 8 rests on it. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (fact 1), when
the ledger entry with this record's status has another contract or is
not the x86_64 item (fact 1), when the x86_64 build emits a `syscall`,
`sysenter` or `int` other than `0f 05`, or no `syscall` at all (fact
2), when Isla's trace of `syscall` no longer meets `syscall.job` (fact
3), and when a negative control passes (the checker accepting a false
property: the return address one byte off, R11 or the SS selector
written but not listed, a memory read, RFLAGS claimed preserved,
SS's attributes without B, the model's fault paths without EFER.SCE
or without the STAR bound, the job without EFER.LMA or CS.L; and a job
asserting nothing about `final.rip`
is refused). `sh verus/seams.sh --check` fails
when verus/SEAMS.md is stale, so a changed contract changes the ledger
and then fails this record's check. No CI job runs `run.sh` yet: it
needs the Isla toolchain and the x86 model (verus/ISLA.md).
