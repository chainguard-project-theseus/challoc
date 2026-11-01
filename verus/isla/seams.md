# The seams Isla checks

Each `src/` heading below is one seam of the ledger (`verus/ledger/`)
whose overrides line gives it tool Islaris/Isla and cites this file:
status `discharged (Isla: verus/isla/seams.md)` when the runs cover the
whole contract (verus/SEAMS.md, "Status"), and `trusted (partly checked,
Isla: verus/isla/seams.md; not checked: ...)` when part of it rests on
something the runs do not reach, which the overrides line names. Each
heading gives its file, its name as the ledger prints it, and the
architecture of the item (`both` for one item whose body holds an `asm!`
call per architecture). Its `Jobs:` line names the run.sh jobs that
trace the seam, as ARCH/JOB (`aarch64/tls-load` for every
`tls-load-xA-xB-xC.job`): at least one per architecture the heading
names, each of which must run and pass when its architecture is in
`ISLA_TARGETS` (or, for a `both` seam whose arm on one architecture has
no `asm!` call, `ARCH/none` for that arm). The
```rust block under it is the contract Verus assumes, copied from the
ledger entry;
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

The other inline-assembly seams stay trusted, with status `trusted` and
no check cited: the x86_64 system calls (`syscall0#1` to `syscall6#1`),
`sys::trap`, `tls::get` and `tls::set`, the GOT functions of `api::cxx`
and `config`, `rseq::symbols` and `rseq::thread_pointer`, and
`sys::pac`'s `sign`, `auth`, `strip` and `axiom_auth_sign`. So do the
four register-pinning hints with an empty template (`/* {0} */`:
`api::shared_base`, `class::table::word_index`,
`layout::regions::entry_barrier`, `heap::shared::ClassLine::guard_word`),
which assemble to no instruction, so Isla has nothing to trace.

Every result below is about the release static library's code, read
before the final link, and its register assignments: run.sh builds a job
for each assignment it finds there. The shipped `cdylib` is a separate
fat-LTO build that may assign other registers at some sites, and a
static link of `libchalloc.a` into an executable relaxes the TLS
sequences to local-exec; both are residuals of every seam below until
run.sh reads them too ([ISLA.md](../ISLA.md), "Not covered").

The aarch64 runs start at Non-secure EL0 in AArch64 state with the PC
and the vector bases symbolic.

## The TLS slot

`tls::get_tp` reads the `challoc_tls` slot through its initial-exec GOT
entry. Every relocation against `challoc_tls` in the aarch64 code sits
in one of two shapes: `adrp A, :gottprel:challoc_tls; ldr A, [A,
:gottprel_lo12:...]; mrs B, tpidr_el0` then `ldr C, [B, A]` (the load,
`get_tp`'s and `tls::get`'s) or `str C, [B, A]` (`tls::set`'s store).
Each load site gets a job (`tls-load.job.in`), one per register
assignment, with the relocated immediates symbolic: it reads exactly the
GOT entry and then the 8 bytes at the thread pointer plus the offset it
holds, writes only its outputs and no flags, and falls through;
`get_tp`'s second output is TPIDR_EL0.

What Isla shows is which memory is read, not that it holds the cell's
value, so `get_tp` is partly checked: `r.0 == slot.value()` rests on the ghost slot model's premise
(`src/sys/tls.rs`, the comment before `get`) that only these sequences
touch the slot, and `r.1 == slot.tp()` on the cell recording the
calling thread's TPIDR_EL0 (`TlsCell::tp`), which the trusted
`TlsSlot::entry` hands out. Other residuals: the ELF initial-exec TLS
model and the loader (the GOT entry holds the slot's offset from the
thread pointer), user addresses below `2^48`, the slot's 8-byte
alignment. Its overrides line names the three facts not checked: the
GOT entry's offset, the slot's value and the cell's tp.

## `src/sys/tls.rs` `get_tp` (aarch64)

Jobs: `aarch64/tls-load`

```rust
pub fn get_tp(Tracked(slot): Tracked<&TlsCell>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        r.1 == slot.tp(),
    opens_invariants none
```

## The system calls

`raw::syscall0` to `syscall6` are one item per architecture (`mod imp`
under each `cfg(target_arch)`): `#1` is x86_64, `#2` aarch64. Their
`requires may_syscall()` is a ghost capability that restricts which
verified callers reach them; it puts no condition on the instruction.
`syscall3`'s `returned3` is an uninterpreted predicate only it ensures,
which the kernel-fact axioms in `sys` give meaning.

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
alone. Each `syscallN#2` is therefore `trusted (partly checked, Isla:
verus/isla/seams.md; not checked: the kernel's side of the call)`.

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

