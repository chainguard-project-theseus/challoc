# The seams Isla checks

Each `src/` heading below is one seam of the ledger (`verus/ledger/`)
whose overrides line gives it tool Islaris/Isla and cites this file:
status `discharged (Isla: verus/isla/seams.md)` when the runs cover the
whole contract (verus/SEAMS.md, "Status"), and `discharged on premises
(Isla: verus/isla/seams.md; premises: ...)` when part of it rests on
platform premises the runs do not reach, which the overrides line names,
and `trusted (partly checked, Isla: verus/isla/seams.md; not checked:
...)` when part of it also rests on a premise of Verus's own model.
"Platform premises" below states each fact about the build, the
loader, Linux and the CPU that the runs assume or that a contract rests
on, once, with its source. Each seam's `Premises:` line names the
premises its runs take as their bounds, and its overrides line names,
after `premises:` or `not checked:`, the premises its contract needs
beyond its runs and those bounds, by label and in label order. Each
heading gives its file, its name as the ledger prints it, and the
architecture of the item (`both` for one item on both architectures:
one whose body holds an `asm!` call per architecture, or one of
`sys::pac`'s, whose aarch64 arm is the `asm!` call and whose x86_64 arm
is the identity). Its `Jobs:` line names the run.sh jobs that trace the
seam, as ARCH/JOB (`aarch64/tls-load` for every
`tls-load-xA-xB-xC.job`): at least one per architecture the heading
names, each of which must run and pass when its architecture is in
`ISLA_TARGETS` (or, for a `both` seam whose arm on one architecture has
no `asm!` call, `ARCH/none` for that arm, as `sys::pac`'s x86_64 arm).
The ```rust block under it is the contract Verus assumes, copied from
the ledger entry;
`verus/isla/run.sh` fails when the ledger's contract, the item's
architecture or the set of seams citing this file differs from it, or a
seam names no job, or a job of a target it runs that did not run or
failed, and
`sh verus/seams.sh --check`, which needs no Isla tool, fails when an
`asm!` call in a seam's item differs from `templates.txt`: a changed
template, an added one (a new `hvc`), or a trapping or system-register
instruction in a string literal there. The other sections say
what the runs establish against those contracts and what stays assumed
(the residual). [ISLA.md](../ISLA.md) describes the setup, the harness
and the checker.

The four register-pinning hints with an empty template (`/* {0} */`:
`api::shared_base`, `class::table::word_index`,
`layout::regions::entry_barrier`, `heap::shared::ClassLine::guard_word`)
assemble to no instruction, so Isla has nothing to trace. `make
emptyasm` (`verus/emptyasm/run.sh`) checks that instead: their ledger
statuses cite it as `emptyasm: <fn> in <file>`.

## Platform premises

The runs start from a stated state and trace the instructions to their
end, a return or an exception. P1 to P13 describe that state: the code
the runs read, and what the build, the loader, Linux and the CPU have
set up before the code runs. They are the bounds of the runs, as a Kani
harness's are the ones its comment states (verus/SEAMS.md, "Status"),
and a seam's `Premises:` line names the ones its jobs assume. P8 is the
one no job assumes: the runs leave GOT entries' contents symbolic, so a
contract that rests on P8 is at most discharged on premises. K1 to K3
are what the kernel does after a trace ends or between the process's
calls, which no run reaches, so a contract that rests on one of them is
at most discharged on premises (SEAMS.md: "a trace that ends at an
exception the kernel handles ... checks nothing the kernel does after
it"). A seam is `discharged (Isla: ...)` only when its contract rests on
nothing but its runs and the premises its `Premises:` line names. A seam
is `discharged on premises` when its contract rests, beyond those, only
on P and K premises, which its overrides line names after `premises:`,
by label and in label order. A contract that also rests on a premise of
Verus's own model (the last paragraph of this section) is partly
checked: its overrides line names, after `not checked:`, the P and K
premises, then the model's. run.sh does not read the `Premises:` lines;
the jobs' `assume` lines and their comments are where the premises meet
the runs. The Linux source paths below are those of Linux v7.4-rc1;
older kernels may reach the same outcome through other functions.

- **P1, the build.** The code is the release static library's, read
  before the final link, with each register assignment the runners find
  there (one job per assignment). The shipped `cdylib` is a separate
  fat-LTO build that may assign other registers at some sites, and a
  static link of `libchalloc.a` into an executable relaxes the TLS
  sequences to local-exec and may relax a GOT load to an address
  computation; the runners read neither, so a `discharged` status here
  is about the static library's code. Source: `run-aarch64.sh` section
  2 and `run-x86_64.sh` section 9; [ISLA.md](../ISLA.md), "Not covered".
- **P2, user memory.** The code, the GOT, the stack, the TLS block and
  the return area are present, accessible user pages when the
  instruction runs, below `2^48` on aarch64 and below `0x7ffffffff000`
  (`2^47` less one page) on x86_64, the bounds the jobs assume; a load
  or store there reads or writes those bytes. The aarch64 runs have the
  MMU off and the x86_64 runs other than `syscall` use the model's
  application view (linear addresses are the memory's, no page walk), so
  no run takes a translation fault. Source: Linux
  `Documentation/arch/arm64/memory.rst` (the 48-bit user VA; on a 52-bit
  kernel `mmap` returns an address above `2^48` only for a hint above
  it) and `Documentation/arch/x86/x86_64/mm.rst` (user space below
  `2^47` with 4-level paging; with 5-level paging, `mmap` returns an
  address above it only for a hint above it, `5level-paging.rst`).
  Linux may fault a page in on first touch or after swapping it out and
  then reruns the same instruction; that is the flat-memory premise the
  Verus and Kani results rest on too, and no trace ends at such a fault,
  so it is a bound here, not a K premise.
- **P3, the aarch64 start state.** Non-secure EL0 in AArch64 state,
  with a kernel at EL1 or a VHE kernel at EL2, the PC and the vector
  bases symbolic. Source: Linux `Documentation/arch/arm64/booting.rst`
  (the kernel is entered at EL2 or non-secure EL1).
- **P4, x86_64 64-bit mode.** Linux runs the task in 64-bit mode
  (IA32_EFER.LMA and the code segment's L flag set). The runs other
  than `syscall` take it as the function assumption `n64_bit_modep` =
  true, applied through the `reset_registers` call run.sh patches into
  the model's entry (the patched model gives `syscall` the unpatched
  model's traces, and the `syscall` job still passes with the
  assumption in place). Source: Intel SDM Vol. 3A, sections 2.2.1
  (IA32_EFER) and 3.4.5 (the L flag).
- **P5, Linux's SYSCALL setup on x86_64.** EFER.SCE set, IA32_STAR[47:32]
  `0x10` (`__KERNEL_CS`, below `0x7ff8`), a canonical IA32_LSTAR, and
  the code below `2^47`, as the `syscall` job assumes; the model lacks
  the x87, MXCSR, AVX-512 mask, PKRU and CET state, so the run says
  nothing about them. A kernel with FRED enabled enters through FRED's
  ring-3 entry point instead of LSTAR, which the model does not have.
  Source: Linux `arch/x86/kernel/head_64.S` (EFER.SCE) and
  `arch/x86/kernel/cpu/common.c`, `syscall_init` and `idt_syscall_init`
  (STAR, LSTAR, the FRED case).
- **P6, the x86_64 stack.** At a function's entry the stack pointer is
  8-byte aligned (`rsp + 8` is a multiple of 16) and a user address,
  and the word on top of the stack is the return address, a user
  address. Source: System V AMD64 psABI, section 3.2.2 ("The Stack
  Frame").
- **P7, GOT entries mapped.** The loader maps each GOT entry, 8 bytes
  and 8-byte aligned, at a user address before any of challoc's code
  runs, and the relocations against it resolve as the ABI defines them
  (the jobs' `assume` lines). Source: ELF gABI, "Global Offset Table";
  Arm's ELF for the Arm 64-bit Architecture (AAELF64), its relocation
  chapter; System V AMD64 psABI, section 4.4 ("Relocation").
- **P8, GOT entry contents.** The loader fills a symbol's GOT entry
  (`R_AARCH64_GLOB_DAT`, `R_X86_64_GLOB_DAT`) with the address of the
  definition that symbol resolution picks (the program's, when it
  interposes), or with 0 for an unresolved weak reference, and the
  entry of `challoc_tls`'s initial-exec reference
  (`R_AARCH64_TLS_TPREL`, formerly `R_AARCH64_TLS_TPREL64`;
  `R_X86_64_TPOFF64`) with the slot's offset from the thread pointer.
  Source: ELF gABI, "Symbol Table" (an unresolved weak symbol has value
  zero) and "Global Offset Table"; AAELF64, its dynamic and
  thread-local storage relocations; Ulrich Drepper, "ELF Handling For
  Thread-Local Storage", the initial-exec model.
- **P9, the TLS slot's address.** The slot, the thread pointer plus the
  offset its GOT entry holds, is an 8-byte aligned user address (`.p2align
  3` in `tls.rs`'s `.tbss`, which the TLS layout honours). Source:
  Drepper, the initial-exec model and the TLS block's alignment.
- **P10, the thread pointer.** TPIDR_EL0 holds the thread's thread
  pointer (TLS variant I). On x86_64 the FS base is the thread pointer,
  an 8-byte aligned user address, and the word at `%fs:0` is the TCB's
  pointer to itself (TLS variant II). Source: Drepper, the two TLS data
  structure variants; AAELF64, its thread-local storage section.
- **P11, the return area.** `rseq::symbols` gets its return-area pointer
  in x0 on aarch64 and rdi on x86_64, to an 8-byte aligned 24-byte area
  at a user address that its caller, built by the same rustc, provides.
  Source: the compiled code (`run-aarch64.sh` section 7 and
  `run-x86_64.sh` section 13 hold the function to its exact
  instructions); Rust's own ABI is unspecified.
- **P12, the keys enabled above EL1.** Firmware sets SCR_EL3.API and
  SCR_EL3.APK; HCR_EL2.API and APK are set by what runs at EL2 for a
  kernel entered at EL1, and by Linux itself (`HCR_HOST_NVHE_FLAGS`) for
  a kernel entered at EL2 that runs nVHE; so `pacda`, `autda` and
  `xpacd` at EL0 do not trap to EL3 or EL2 (the configurations pin
  SCR_EL3 = `0x30401` and HCR_EL2). Source: Linux
  `Documentation/arch/arm64/booting.rst`, "System registers" ("SCR_EL3.APK
  (bit 16) must be initialised to 0b1", "SCR_EL3.API (bit 17) must be
  initialised to 0b1", and HCR_EL2's bits 40 and 41 for a kernel entered
  at EL1); Arm ARM (DDI 0487), the SCR_EL3 and HCR_EL2 register
  descriptions.
- **P13, the PAC CPU.** The CPU is one of the two kinds the PAC runs
  model (Pointer authentication, below): Armv8.5-A with FEAT_PAuth, or
  an Armv9.0-A CPU with FEAT_PAuth2, FEAT_FPAC and FEAT_FPACCOMBINE and
  FEAT_CONSTPACFIELD off or on, with Linux's TCR for a user VA of 36,
  39, 42, 47, 48 or 52 bits; and `user_va_limit()` is at most `2^` the
  VA width that TCR configures (`user_va_limit`'s doc: `2^VA_BITS`,
  capped; Verus knows only `axiom_user_va_limit`'s `2^48`). Not covered:
  FEAT_EPAC without FEAT_PAuth2, FEAT_PAuth2 without FEAT_FPAC, and
  52-bit VA with 4K or 16K pages (FEAT_LPA2). Source:
  [ISLA.md](../ISLA.md), "The harness" (the PAC configurations) and
  "Not covered"; `src/sys/pac.rs`, `user_va_limit`; Arm ARM (DDI 0487),
  the pointer authentication features and the `ComputePAC`, `AddPACDA`
  and `AuthDA` pseudocode.
- **K1, the kernel's side of a system call.** For a call that returns
  (challoc's `exit_group`, and its `tgkill` of itself with `SIGABRT`, do
  not), Linux returns to the instruction after `svc #0` or `syscall`
  with the result in x0 or rax and every other user register as it
  was, but rcx and r11 on x86_64 and, on arm64, the SVE state (the
  Z-register bits above 128, P0-P15 and FFR); and the call has the effects
  its manual page defines. challoc makes no `execve`, `clone` or
  `rt_sigreturn` call. Source: `syscall(2)`, "Architecture calling
  conventions"; Intel SDM Vol. 2B, SYSCALL (rcx and r11); Linux
  `Documentation/arch/arm64/sve.rst`, "System call behaviour".
- **K2, no resume past a trap.** For `brk #0x1` at EL0 Linux delivers
  `SIGTRAP` (`arch/arm64/kernel/debug-monitors.c`, `do_el0_brk64`: `#1`
  is not the uprobe immediate), for `ud2` `SIGILL`
  (`arch/x86/kernel/traps.c`, `handle_invalid_op`) and for an `autda`
  that takes the FPAC fault `SIGILL` (`arch/arm64/kernel/traps.c`,
  `do_el0_fpac`). It resumes the thread at the faulting instruction or
  not at all, never past it: the default action of both signals ends
  the process (`signal(7)`), and only a handler the program installs
  could move the PC on.
- **K3, a fixed data key A and EnDA.** Linux assigns the process its
  keys at `exec`, shared by all its threads and kept across `fork`;
  `prctl(PR_PAC_RESET_KEYS)` reinitialises the calling thread's keys,
  and `prctl(PR_PAC_SET_ENABLED_KEYS)` enables or disables keys for the
  calling task. challoc calls neither, and the program must not
  (`spec_sign`'s doc). Source: Linux
  `Documentation/arch/arm64/pointer-authentication.rst`, "Basic support"
  and "Enabling and disabling keys"; `prctl(2)`, `PR_PAC_RESET_KEYS`.

Two TLS facts are premises of Verus's own model, not of the platform:
the slot premise (`src/sys/tls.rs`, `TlsSlot`'s "Premise of the
model") and the cell recording the calling thread's TPIDR_EL0
(`TlsCell::tp`, which the trusted `TlsSlot::entry` hands out).

## The GOT functions

`get_new_handler_addr`, `throw_bad_alloc_addr`, `malloc_conf_addr`,
`je_malloc_conf_addr` and `purge::background::single_threaded_addr`
(`#[inline(never)]`) each load one symbol's GOT entry. On aarch64 each compiled function is exactly `adrp x0, :got:SYM; ldr x0, [x0,
:got_lo12:SYM]; ret` (`got-fn.job.in`); on x86_64 exactly `mov rax, [rip
+ SYM@GOTPCREL]; ret` (`x86/got-fn.job`). Under the relocations'
definitions each reads exactly the GOT entry, returns it in the result
register, writes nothing else and returns to the caller. The aarch64
jobs assume P2 and P7 (code and GOT entry at user addresses, the entry
8-byte aligned); the x86_64 jobs also assume P4 and P6 (64-bit mode and
the stack). The contracts claim nothing about
the address beyond the uninterpreted `is_throw_bad_alloc` and
`is_get_new_handler`, which only `throw_bad_alloc_addr` and
`get_new_handler_addr` ensure. Each predicate says the address is that
of the symbol's definition (`std::__throw_bad_alloc`'s,
`std::get_new_handler`'s), or null, which is P8 itself, so each of the
two is `discharged on premises (Isla: verus/isla/seams.md; premises:
P8 ...)`. The other two have no `ensures`: their contracts claim only
that the call returns, writing nothing Verus tracks, which the runs
show within the premises their `Premises:` lines name, so each is
`discharged (Isla: verus/isla/seams.md)`.

## `src/api/cxx.rs` `get_new_handler_addr` (both)

Jobs: `aarch64/got-fn-get_new_handler_addr` `x86_64/got-fn-get_new_handler_addr`

Premises: P1, P2, P3, P4, P6, P7

```rust
fn get_new_handler_addr() -> (r: *const c_void)
    ensures
        is_get_new_handler(r),
```

## `src/api/cxx.rs` `throw_bad_alloc_addr` (both)

Jobs: `aarch64/got-fn-throw_bad_alloc_addr` `x86_64/got-fn-throw_bad_alloc_addr`

Premises: P1, P2, P3, P4, P6, P7

```rust
fn throw_bad_alloc_addr() -> (r: *const c_void)
    ensures
        is_throw_bad_alloc(r),
```

## `src/config.rs` `malloc_conf_addr` (both)

Jobs: `aarch64/got-fn-malloc_conf_addr` `x86_64/got-fn-malloc_conf_addr`

Premises: P1, P2, P3, P4, P6, P7

```rust
fn malloc_conf_addr() -> *const c_void
```

## `src/config.rs` `je_malloc_conf_addr` (both)

Jobs: `aarch64/got-fn-je_malloc_conf_addr` `x86_64/got-fn-je_malloc_conf_addr`

Premises: P1, P2, P3, P4, P6, P7

```rust
fn je_malloc_conf_addr() -> *const c_void
```

## `src/purge/background.rs` `single_threaded_addr` (both)

Jobs: `aarch64/got-fn-single_threaded_addr` `x86_64/got-fn-single_threaded_addr`

Premises: P1, P2, P3, P4, P6, P7

```rust
fn single_threaded_addr() -> *const u8
```

## The rseq symbols and the thread pointer

`rseq::symbols` (`#[inline(never)]`) is the three GOT loads followed by
the compiler's stores of the tuple through the return-area pointer (x0
on aarch64, rdi on x86_64) and the return, each required exactly
(`rseq-symbols.job.in`, `x86/rseq-symbols.job`): the three entries are
read in order, their values stored to the return area's three words,
nothing else written. Its jobs assume what the GOT functions' do, and
P11 (the return area).

`rseq::thread_pointer` (`#[inline(always)]`): every `mrs xN, tpidr_el0`
in the aarch64 code, its own and the TLS sequences' third instruction,
writes only xN, with TPIDR_EL0, and touches no memory or flags
(`mrs-tp.job.in`, one job per register); every `mov R, qword ptr fs:[0]`
in the x86_64 code reads the 8 bytes at the FS base into R and writes
nothing else (`x86/fs0.job.in`). The x86_64 job assumes P2 (the code at
a user address), P4 and P10 (the FS base an 8-byte aligned user
address).

Neither has an `ensures`: their contracts claim only that the call
returns, writing nothing Verus tracks, which the runs show within the
premises their `Premises:` lines name, so both are `discharged (Isla:
verus/isla/seams.md)`. What the thread pointer's value means (P10) is a
fact `thread_pointer`'s contract does not claim.

## `src/rseq.rs` `symbols` (both)

Jobs: `aarch64/rseq-symbols` `x86_64/rseq-symbols`

Premises: P1, P2, P3, P4, P6, P7, P11

```rust
fn symbols() -> (*const isize, *const u32, *const u32)
```

## `src/rseq.rs` `thread_pointer` (both)

Jobs: `aarch64/mrs-tp` `x86_64/fs0`

Premises: P1, P2, P3, P4, P10

```rust
pub fn thread_pointer() -> usize
```

## The trap

`sys::trap`: every `brk` in the aarch64 code is `brk #0x1` (LLVM's own
traps are the same instruction); it takes the breakpoint exception (EC
0x3C) to EL1, or to EL2 under VHE, with the `brk` itself as the return
address, writes no user register or memory and never falls through
(`brk1.job`, `brk1-vhe.job`). Every `ud2` in the x86_64 code is `0f 0b`;
it raises the model's architectural fault (`x86_fault`, #UD), writes
nothing (rip included) and never completes (`x86/ud2.job`). The contract
(`-> !` under `may_abort()`) needs exactly that the call does not
return. The x86_64 job assumes P2 (the code at a user address) and P4.
The trace ends at the exception, and resuming at the return address
runs the trap again, so the thread gets past it only if the kernel (or
a signal handler it runs) moves the PC on, which is K2, the kernel's
side, which no run reaches. So `trap` is `discharged on premises (Isla:
verus/isla/seams.md; premises: K2 ...)`.

## `src/sys/mod.rs` `trap` (both)

Jobs: `aarch64/brk1` `aarch64/brk1-vhe` `x86_64/ud2`

Premises: P1, P2, P3, P4

```rust
pub fn trap() -> !
    requires
        crate::sys::may_abort(),
```

## Pointer authentication

`pac::sign`, `auth` and `strip` are `pacda`, `autda` and `xpacd` on
aarch64 and the identity on x86_64, where no verified caller can
establish `pac_available()` (`available()` returns false there, and its
`ensures` is the only fact about it), so the x86_64 bodies are
unreachable. On aarch64, every
PAC-family instruction in the code is `pacda Xd, Xn` or `autda Xd, Xn`
(d and n distinct, neither 31) or `xpacd Xd`. Run in 36 configurations
(12 of the Armv8.5-A model with FEAT_PAuth; 24 of the Armv9.4-A model as
an Armv9.0-A CPU with FEAT_PAuth2, FEAT_FPAC and FEAT_FPACCOMBINE, with
FEAT_CONSTPACFIELD off and on: Neoverse V2's pointer authentication),
each with a kernel at EL1 or a VHE kernel at EL2 and Linux's TCR for a
user VA of 36, 39, 42, 47, 48 or 52 bits, SCTLR (EnDA), keys, pointers
and modifiers symbolic and ComputePAC uninterpreted:

- every site's instruction falls through writing only its destination,
  no memory and no flags (`pac-footprint.job.in`), except that with
  FEAT_FPAC an `autda` may instead end at the FPAC fault having written
  nothing;
- against the canonical `pacda x0, x1` (`autda x0, x1`, `xpacd x0`) run
  from an independent state, the same pointer, modifier, data key A and
  EnDA give the same result, and for `autda` with FEAT_FPAC the same
  fault or none (`pac-det.job.in`, `pac-strip-det.job.in`): `spec_sign`
  and `spec_auth` are each one function of `(p, modifier)` once the key,
  EnDA and the configuration are fixed for the process;
- `pacda x0, x1; autda x0, x1` gives back every `p` below the VA limit,
  with EnDA set or clear, and never takes the FPAC fault
  (`pac-rt.job.in`): `axiom_auth_sign`'s aarch64 half, and `auth`'s
  second `ensures`, since `user_va_limit()` is at most the VA width the
  kernel configures (P13; `user_va_limit`'s doc: `2^VA_BITS`, capped);
- what each model computes, in terms of ComputePAC, for user pointers
  (`pac-spec-*.job.in`, `pac-enda-off.job.in`), and the FPAC exception
  entry: EC 0x1C, the `autda` as return address, the kernel's vector, no
  user register, other system register or memory written
  (`pac-fault.job.in`), so where `autda` faults it does not return.

`ComputePAC` being a function of its arguments is spot-checked on eight
concrete inputs in the v8.5 model and in the v9.4 one with QARMA5 and
QARMA3 (`pac-pure.job.in`). The configurations are P12 (the keys
firmware enables) and P13 (the two kinds of CPU, and the VA sizes).
`axiom_auth_sign`'s x86_64 half needs no tool: `spec_sign` and
`spec_auth` are unconstrained there and the identity satisfies it.

`spec_sign` and `spec_auth` are one function per process only while
the key and EnDA stay as they were, K3, and an `autda` that faults
returns only if the kernel resumes past it, K2: both are the kernel's
side, which no run reaches. `sign`'s `ensures` rests on K3, and
`auth`'s on K2 and K3, so `sign` is `discharged on premises (Isla:
verus/isla/seams.md; premises: K3 ...)` and `auth` the same with K2
and K3. `strip` has no `ensures`, and `axiom_auth_sign` relates the two
spec functions for one key, which every configuration checks with the
key symbolic and EnDA set or clear; neither rests on anything but its
runs and the premises its `Premises:` line names, so both are `discharged (Isla:
verus/isla/seams.md)`.

## `src/sys/pac.rs` `sign` (both)

Jobs: `aarch64/pac-pacda` `aarch64/pac-pacda-det` `aarch64/pac-pacda-spec` `aarch64/pac-pacda-off` `aarch64/pac-pure` `x86_64/none`

Premises: P1, P3, P12, P13

```rust
pub fn sign(p: usize, modifier: usize) -> (r: usize)
    requires
        pac_available(),
    ensures
        r == spec_sign(p, modifier),
    opens_invariants none
```

## `src/sys/pac.rs` `auth` (both)

Jobs: `aarch64/pac-autda` `aarch64/pac-autda-det` `aarch64/pac-autda-spec` `aarch64/pac-autda-off` `aarch64/pac-autda-fault` `aarch64/pac-rt` `x86_64/none`

Premises: P1, P3, P12, P13

```rust
pub fn auth(p: usize, modifier: usize) -> (r: usize)
    requires
        pac_available(),
        super::may_abort_corrupt() || signed(p, modifier),
    ensures
        r == spec_auth(p, modifier),
        forall|q: usize| q < user_va_limit() && p == #[trigger] spec_sign(q, modifier) ==> r == q,
    opens_invariants none
```

## `src/sys/pac.rs` `strip` (both)

Jobs: `aarch64/pac-xpacd` `aarch64/pac-xpacd-det` `x86_64/none`

Premises: P1, P3, P12, P13

```rust
pub fn strip(p: usize) -> usize
    requires
        pac_available(),
```

## `src/sys/pac.rs` `axiom_auth_sign` (both)

Jobs: `aarch64/pac-rt` `x86_64/none`

Premises: P3, P12, P13

```rust
pub broadcast axiom fn axiom_auth_sign(p: usize, modifier: usize)
    requires
        p < user_va_limit(),
    ensures
        #[trigger] spec_auth(spec_sign(p, modifier), modifier) == p,
;
```

## The TLS slot

`tls::get`, `get_tp` and `set` read or write the `challoc_tls` slot
through its initial-exec GOT entry. Every relocation against
`challoc_tls` in the code sits in one of the templates' shapes. aarch64:
`adrp A, :gottprel:challoc_tls; ldr A, [A, :gottprel_lo12:...]; mrs B,
tpidr_el0` then `ldr C, [B, A]` (`tls-load.job.in`, the load of `get`
and `get_tp`) or `str C, [B, A]` (`tls-store.job.in`), one job per
register assignment, with the relocated immediates symbolic. x86_64:
`mov R, [rip + challoc_tls@GOTTPOFF]` then `mov R, fs:[R]`
(`x86/tls-load.job.in`) or `mov fs:[R], V` (`x86/tls-store.job.in`).
Each reads exactly the GOT entry and then reads, or writes the value
to, the 8 bytes at the thread pointer plus the offset it holds, writes
only its outputs and no flags, and falls through; `get_tp`'s second
output is TPIDR_EL0.

What Isla shows is which memory is read and written, not that it holds
the cell's value: `r == slot.value()` and `set`'s `set_to` rest on the
ghost slot model's premise (`src/sys/tls.rs`, `TlsSlot`'s "Premise of
the model": a token is not read after the slot has changed other than
through it), which holds only if these sequences are the only accesses
to the slot, and on P8 (the GOT entry holds the slot's offset from the
thread pointer); `r.1 == slot.tp()` rests on the cell recording the
calling thread's TPIDR_EL0 (`TlsCell::tp`), which the trusted
`TlsSlot::entry` hands out. The slot premise and `TlsCell::tp` are
premises of Verus's model, not of the platform, and the runs check
neither, nor P8. So `get`, `get_tp` and `set` are each `trusted (partly
checked, Isla: verus/isla/seams.md; not checked: ...)`, their overrides
lines naming P8, the slot premise and, for `get_tp`, `TlsCell::tp`.
Their jobs assume P2, P7 and P9 (code, GOT entry and slot at user
addresses, the GOT entry and the slot 8-byte aligned) and, on x86_64,
P4.

On x86_64, `get_tp` has no asm (it pairs `tls::get` with 0), so its
seam is aarch64 only. Besides the jobs, run.sh holds every relocation
against `challoc_tls` in the x86_64 code to one of two shapes with no
model (`run-x86_64.sh` section 11): `movq (%rip), %R` with
`R_X86_64_GOTTPOFF`, then `movq %fs:(%R), %R` (`tls::get`'s load) or
`movq %V, %fs:(%R)` (`tls::set`'s store).

## `src/sys/tls.rs` `get` (both)

Jobs: `aarch64/tls-load` `x86_64/tls-load`

Premises: P1, P2, P3, P4, P7, P9

```rust
pub fn get(Tracked(slot): Tracked<&TlsCell>) -> (r: *mut c_void)
    ensures
        r as usize == slot.value(),
    opens_invariants none
```

## `src/sys/tls.rs` `get_tp` (aarch64)

Jobs: `aarch64/tls-load`

Premises: P1, P2, P3, P7, P9

```rust
pub fn get_tp(Tracked(slot): Tracked<&TlsCell>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        r.1 == slot.tp(),
    opens_invariants none
```

## `src/sys/tls.rs` `set` (both)

Jobs: `aarch64/tls-store` `x86_64/tls-store`

Premises: P1, P2, P3, P4, P7, P9

```rust
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsCell>)
    requires
        crate::heap::slot_step(old(slot).value(), value as usize),
    ensures
        final(slot).set_to(*old(slot), value as usize),
```

`requires slot_step(..)` is a condition on the verified callers (the
slot's phase order); it puts no condition on the instruction.

## The system calls

`raw::syscall0` to `syscall6` are one item per architecture (`mod imp`
under each `cfg(target_arch)`): `#1` is x86_64, `#2` aarch64. Their
`requires may_syscall()` is a ghost capability that restricts which
verified callers reach them; it puts no condition on the instruction.
`syscall3`'s `returned3` is an uninterpreted predicate only it ensures,
which the kernel-fact axioms in `sys` give meaning.

x86_64: every `syscall`, `sysenter` and `int` in the code is `syscall`
(`0f 05`). Traced on the x86 model from a symbolic state in 64-bit mode
with SYSCALL enabled (`x86/syscall.job`): RCX = RIP + 2, R11 = RFLAGS,
RIP = IA32_LSTAR, RFLAGS masked by IA32_FMASK, CS and SS from IA32_STAR
with flat 64-bit code and data descriptors, no other register the model
has and no memory written, no model fault. The job assumes P4 and P5
(Linux's SYSCALL setup). The trace ends at the kernel's entry point
(LSTAR), so the kernel's side of the call, K1, is not checked: that
`sysret` comes back to the next instruction with the result in rax and
every user register but rcx and r11 as it was, and that the call has
the effects the Linux x86_64 syscall ABI defines. Each `syscallN#1` is
`discharged on premises (Isla: verus/isla/seams.md; premises: K1
...)`, as each `syscallN#2` below.
The x86_64 check behind these statuses runs in CI only after landing
(`isla-main`, which gates nothing), not in the presubmit; see
`verus/ISLA.md`, "The x86_64 model".

aarch64: every `svc`, `hvc` and `smc` in the code is `svc #0`. At EL0
for a kernel at EL1 (`svc0.job`) and at EL2 under VHE (`svc0-vhe.job`)
it takes the SVC exception with syndrome EC 0x15, IL set, imm16 0, the
next instruction as return address and the lower-EL AArch64 vector, and
writes no user register (X0-X30, SP_EL0, the SIMD/FP registers, FPCR,
FPSR, TPIDR_EL0) and no memory. The trace ends at the exception vector, so the
kernel's side of the call, K1, is not checked: that it returns to the next
instruction, leaves the result in x0 and every other user register as
it was, and that the call has the effects the Linux arm64 syscall ABI
defines (none of `execve`, `clone` or `rt_sigreturn` among challoc's).
Nothing in the runs relates `syscall3`'s `r` to the call, so
`returned3` and the kernel-fact axioms built on it rest on the kernel
alone, on both architectures. Each `syscallN#2` is therefore `discharged
on premises (Isla: verus/isla/seams.md; premises: K1 (the kernel's
side of the call))`.

## `src/sys/raw.rs` `syscall0#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall0(nr: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall1#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall2#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall3#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
    requires
        crate::sys::may_syscall(),
    ensures
        super::returned3(nr, a0, a1, a2, r),
```

## `src/sys/raw.rs` `syscall4#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall5#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

```rust
pub unsafe fn syscall5(
    nr: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall6#1` (x86_64)

Jobs: `x86_64/syscall`

Premises: P1, P4, P5

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

## `src/sys/raw.rs` `syscall0#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall0(nr: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall1#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall2#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall3#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
    requires
        crate::sys::may_syscall(),
    ensures
        super::returned3(nr, a0, a1, a2, r),
```

## `src/sys/raw.rs` `syscall4#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall5#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

```rust
pub unsafe fn syscall5(
    nr: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall6#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

Premises: P1, P3

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

