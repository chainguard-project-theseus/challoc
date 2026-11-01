# The seams Isla checks

Each `src/` heading below is one seam of the ledger (`verus/ledger/`)
whose overrides line gives it tool Islaris/Isla and cites this file:
status `discharged (Isla: verus/isla/seams.md)` when the runs cover the
whole contract (verus/SEAMS.md, "Status"), and `trusted (partly checked,
Isla: verus/isla/seams.md; not checked: ...)` when part of it rests on
something the runs do not reach, which the overrides line names. For
the GOT functions, the rseq symbols and thread pointer, the trap and the
TLS slot, that list names every assumption of the seam's jobs other
than the relocations' definitions, and the facts its contract rests on,
in one order: the aarch64 jobs' assumptions, then the x86_64 jobs'
(with the 64-bit mode and application view of their runs), then the
loader's, the kernel's or the C library's side. Each
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
assemble to no instruction, so Isla has nothing to trace; they stay
trusted.

Every result below is about the release static library's code, read
before the final link, and its register assignments: run.sh builds a job
for each assignment it finds there. The shipped `cdylib` is a separate
fat-LTO build that may assign other registers at some sites, and a
static link of `libchalloc.a` into an executable relaxes the TLS
sequences to local-exec, and the linker may relax a GOT load to an
address computation; all three are residuals of every seam below until
run.sh reads them too ([ISLA.md](../ISLA.md), "Not covered").

The aarch64 runs start at Non-secure EL0 in AArch64 state with the PC
and the vector bases symbolic. The x86_64 `syscall` run starts from a
symbolic state in the model's system view. The other x86_64 runs (the
GOT loads, the TLS accesses, `mov fs:[0]` and `ud2`) add three things,
each a residual of the seams below that have an x86_64 half: the
processor is in 64-bit mode by function assumption (`n64_bit_modep`
answers true, as Linux runs 64-bit tasks; the `syscall` job still passes
with the assumption in place), the model's application view (linear
addresses are the memory's, no page walk: Linux maps the GOT, the stack
and the TLS block, and a page fault is the kernel's), and a call of
Isla's `reset_registers` hook patched into the entry, through which the
assumption applies (the patched model gives `syscall` the unpatched
model's traces).

## The GOT functions

`get_new_handler_addr`, `throw_bad_alloc_addr`, `malloc_conf_addr` and
`je_malloc_conf_addr` (`#[inline(never)]`) each load one symbol's GOT
entry. On aarch64 each compiled function is exactly `adrp x0, :got:SYM; ldr x0, [x0,
:got_lo12:SYM]; ret` (`got-fn.job.in`); on x86_64 exactly `mov rax, [rip
+ SYM@GOTPCREL]; ret` (`x86/got-fn.job`). Under the relocations'
definitions each reads exactly the GOT entry, returns it in the result
register, writes nothing else and returns to the caller. Residual: the
ELF loader fills the entry with the symbol's address (the program's
definition interposing), or 0 for an unresolved weak symbol; GOT entries
are 8-byte aligned; code and GOT are user addresses; on x86_64 the stack
is a user address, the stack pointer 8-byte aligned and the return
address on top of the stack a user address. The
contracts claim nothing about the address beyond the uninterpreted
`is_throw_bad_alloc`, which only this function ensures. That predicate
says the address is `std::__throw_bad_alloc`'s, or null, which only the
loader makes true, so `throw_bad_alloc_addr` is `trusted (partly checked,
Isla: verus/isla/seams.md; not checked: ...)`, its overrides line naming
that fill and the residuals above. The other three have no
`ensures`: their contracts claim only that the call returns, writing
nothing Verus tracks, which the runs show given the residuals above.
Those residuals are platform facts the runs assume (the loader's GOT
mapping and alignment, user addresses, the x86_64 stack, and on x86_64
the 64-bit mode and application view), so, as `strip` and
`axiom_auth_sign` are, each is `trusted (partly checked, Isla:
verus/isla/seams.md; not checked: ...)` with its overrides line naming
them.

## `src/api/cxx.rs` `get_new_handler_addr` (both)

Jobs: `aarch64/got-fn-get_new_handler_addr` `x86_64/got-fn-get_new_handler_addr`

```rust
fn get_new_handler_addr() -> *const c_void
```

## `src/api/cxx.rs` `throw_bad_alloc_addr` (both)

Jobs: `aarch64/got-fn-throw_bad_alloc_addr` `x86_64/got-fn-throw_bad_alloc_addr`

```rust
fn throw_bad_alloc_addr() -> (r: *const c_void)
    ensures
        is_throw_bad_alloc(r),
```

## `src/config.rs` `malloc_conf_addr` (both)

Jobs: `aarch64/got-fn-malloc_conf_addr` `x86_64/got-fn-malloc_conf_addr`

```rust
fn malloc_conf_addr() -> *const c_void
```

## `src/config.rs` `je_malloc_conf_addr` (both)

Jobs: `aarch64/got-fn-je_malloc_conf_addr` `x86_64/got-fn-je_malloc_conf_addr`

```rust
fn je_malloc_conf_addr() -> *const c_void
```

## The rseq symbols and the thread pointer

`rseq::symbols` (`#[inline(never)]`) is the three GOT loads followed by
the compiler's stores of the tuple through the return-area pointer (x0
on aarch64, rdi on x86_64) and the return, each required exactly
(`rseq-symbols.job.in`, `x86/rseq-symbols.job`): the three entries are
read in order, their values stored to the return area's three words,
nothing else written. Residual: as for the GOT functions, plus the Rust
ABI's return area (an 8-byte aligned 24-byte tuple at a user address).

`rseq::thread_pointer` (`#[inline(always)]`): every `mrs xN, tpidr_el0`
in the aarch64 code, its own and the TLS sequences' third instruction,
writes only xN, with TPIDR_EL0, and touches no memory or flags
(`mrs-tp.job.in`, one job per register); every `mov R, qword ptr fs:[0]`
in the x86_64 code reads the 8 bytes at the FS base into R and writes
nothing else (`x86/fs0.job.in`). Residual: the C library's thread
pointer (TPIDR_EL0; the TCB's self pointer at `%fs:0`); on x86_64, the
code is a user address and the FS base an 8-byte aligned user address.

Neither has an `ensures`: their contracts claim only that the call
returns, writing nothing Verus tracks, which the runs show given the
residuals above. Those rest on platform facts the runs assume (the
loader's GOT, the return area, user addresses for the code, the x86_64
stack and the FS base and, on x86_64, the 64-bit mode and application
view),
so both are `trusted (partly checked, Isla:
verus/isla/seams.md; not checked: ...)`, their overrides lines naming
them.

## `src/rseq.rs` `symbols` (both)

Jobs: `aarch64/rseq-symbols` `x86_64/rseq-symbols`

```rust
fn symbols() -> (*const isize, *const u32, *const u32)
```

## `src/rseq.rs` `thread_pointer` (both)

Jobs: `aarch64/mrs-tp` `x86_64/fs0`

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
return. Residual: Linux delivers `SIGTRAP` or `SIGILL`, which kills the
process; on x86_64, the code is a user address, with the 64-bit mode
and application view above. The trace ends at the exception, and resuming at the
return address runs the trap again, so the thread gets past it only if
the kernel (or a signal handler it runs) moves the PC on; that is the
kernel's side, which no run reaches, so `trap` is `trusted (partly
checked, Isla: verus/isla/seams.md; not checked: ...)`, its overrides
line naming that and the x86_64 residuals.

## `src/sys/mod.rs` `trap` (both)

Jobs: `aarch64/brk1` `aarch64/brk1-vhe` `x86_64/ud2`

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
  kernel configures (its definition: `2^VA_BITS`, capped);
- what each model computes, in terms of ComputePAC, for user pointers
  (`pac-spec-*.job.in`, `pac-enda-off.job.in`), and the FPAC exception
  entry: EC 0x1C, the `autda` as return address, the kernel's vector, no
  user register, other system register or memory written
  (`pac-fault.job.in`), so where `autda` faults it does not return.

`ComputePAC` being a function of its arguments is spot-checked on eight
concrete inputs in the v8.5 model and in the v9.4 one with QARMA5 and
QARMA3 (`pac-pure.job.in`). Residual: the process's key and EnDA stay
fixed (the `prctl` calls `spec_sign`'s doc names break it), firmware
sets SCR_EL3.API and APK, Linux delivers `SIGILL` on the FPAC fault and
never resumes past the `autda`, and the CPU is one of the two kinds
checked; not checked are FEAT_EPAC without FEAT_PAuth2, FEAT_PAuth2
without FEAT_FPAC and 52-bit VA with 4K or 16K pages (FEAT_LPA2).
`axiom_auth_sign`'s x86_64 half needs no tool: `spec_sign` and
`spec_auth` are unconstrained there and the identity satisfies it.

The key and EnDA staying fixed, and the kernel never resuming past a
faulting `autda`, are the kernel's side, which no run reaches; `sign`'s and
`auth`'s `ensures` rest on them, so both are `trusted (partly checked,
Isla: verus/isla/seams.md; not checked: ...)` with those facts named.
`strip` has no `ensures` and `axiom_auth_sign` relates the two spec
functions for one key, which every configuration checks with the key
symbolic; both still rest on the platform residuals above (firmware's
SCR_EL3.API and APK, a CPU of a kind not checked), so both are `trusted
(partly checked, ...)` with those named, as `get_tp` is.

## `src/sys/pac.rs` `sign` (both)

Jobs: `aarch64/pac-pacda` `aarch64/pac-pacda-det` `aarch64/pac-pacda-spec` `aarch64/pac-pacda-off` `aarch64/pac-pure` `x86_64/none`

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

```rust
pub fn strip(p: usize) -> usize
    requires
        pac_available(),
```

## `src/sys/pac.rs` `axiom_auth_sign` (both)

Jobs: `aarch64/pac-rt` `x86_64/none`

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
to the slot, and on the loader's initial-exec TLS (the GOT entry holds
the slot's offset from the thread pointer); `r.1 == slot.tp()` rests on
the cell recording the calling thread's TPIDR_EL0 (`TlsCell::tp`), which
the trusted `TlsSlot::entry` hands out. So `get`, `get_tp` and `set` are
each `trusted (partly checked, Isla: verus/isla/seams.md; not checked:
...)`, their overrides lines naming those facts and the other
residuals: code, GOT entry and slot at user addresses below `2^48`
(`2^47` on x86_64), the GOT entry's and the slot's 8-byte alignment,
and on x86_64 the 64-bit mode and application view.

On x86_64, `get_tp` has no asm (it pairs `tls::get` with 0), so its
seam is aarch64 only. Besides the jobs, run.sh holds every relocation
against `challoc_tls` in the x86_64 code to one of two shapes with no
model (`run-x86_64.sh` section 11): `movq (%rip), %R` with
`R_X86_64_GOTTPOFF`, then `movq %fs:(%R), %R` (`tls::get`'s load) or
`movq %V, %fs:(%R)` (`tls::set`'s store).

## `src/sys/tls.rs` `get` (both)

Jobs: `aarch64/tls-load` `x86_64/tls-load`

```rust
pub fn get(Tracked(slot): Tracked<&TlsCell>) -> (r: *mut c_void)
    ensures
        r as usize == slot.value(),
    opens_invariants none
```

## `src/sys/tls.rs` `get_tp` (aarch64)

Jobs: `aarch64/tls-load`

```rust
pub fn get_tp(Tracked(slot): Tracked<&TlsCell>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        r.1 == slot.tp(),
    opens_invariants none
```

## `src/sys/tls.rs` `set` (both)

Jobs: `aarch64/tls-store` `x86_64/tls-store`

```rust
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsCell>)
    ensures
        final(slot).set_to(*old(slot), value as usize),
```

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
has and no memory written, no model fault. Residual: Linux's user-mode
state (EFER.SCE, 64-bit mode, code below `2^47`, a canonical LSTAR,
IA32_STAR[47:32] below `0x7ff8`, Linux: `0x10`); the
model lacks x87, MXCSR, AVX-512 mask, PKRU and CET state. The trace ends
at the kernel's entry point (LSTAR), so the kernel's side of the call is
not checked: that `sysret` comes back to the next instruction with the
result in rax and every user register but rcx and r11 as it was, and
that the call has the effects the Linux x86_64 syscall ABI defines.
Each `syscallN#1` is `trusted (partly checked, Isla: verus/isla/seams.md;
not checked: the kernel's side of the call)`, as each `syscallN#2` below.
The x86_64 check behind these statuses runs in CI only after landing
(`isla-main`, which gates nothing), not in the presubmit; see
`verus/ISLA.md`, "The x86_64 model".

aarch64: every `svc`, `hvc` and `smc` in the code is `svc #0`. At EL0
for a kernel at EL1 (`svc0.job`) and at EL2 under VHE (`svc0-vhe.job`)
it takes the SVC exception with syndrome EC 0x15, IL set, imm16 0, the
next instruction as return address and the lower-EL AArch64 vector, and
writes no user register (X0-X30, SP_EL0, the SIMD/FP registers, FPCR,
FPSR, TPIDR_EL0) and no memory. The trace ends at the exception vector, so the
kernel's side of the call is not checked: that it returns to the next
instruction, leaves the result in x0 and every other user register as
it was, and that the call has the effects the Linux arm64 syscall ABI
defines (none of `execve`, `clone` or `rt_sigreturn` among challoc's).
Nothing in the runs relates `syscall3`'s `r` to the call, so
`returned3` and the kernel-fact axioms built on it rest on the kernel
alone, on both architectures. Each `syscallN#2` is therefore `trusted
(partly checked, Isla: verus/isla/seams.md; not checked: the kernel's
side of the call)`.

## `src/sys/raw.rs` `syscall0#1` (x86_64)

Jobs: `x86_64/syscall`

```rust
pub unsafe fn syscall0(nr: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall1#1` (x86_64)

Jobs: `x86_64/syscall`

```rust
pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall2#1` (x86_64)

Jobs: `x86_64/syscall`

```rust
pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall3#1` (x86_64)

Jobs: `x86_64/syscall`

```rust
pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
    requires
        crate::sys::may_syscall(),
    ensures
        super::returned3(nr, a0, a1, a2, r),
```

## `src/sys/raw.rs` `syscall4#1` (x86_64)

Jobs: `x86_64/syscall`

```rust
pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall5#1` (x86_64)

Jobs: `x86_64/syscall`

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

```rust
pub unsafe fn syscall0(nr: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall1#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

```rust
pub unsafe fn syscall1(nr: usize, a0: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall2#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

```rust
pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall3#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

```rust
pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> (r: usize)
    requires
        crate::sys::may_syscall(),
    ensures
        super::returned3(nr, a0, a1, a2, r),
```

## `src/sys/raw.rs` `syscall4#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

```rust
pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize
    requires
        crate::sys::may_syscall(),
```

## `src/sys/raw.rs` `syscall5#2` (aarch64)

Jobs: `aarch64/svc0` `aarch64/svc0-vhe`

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

