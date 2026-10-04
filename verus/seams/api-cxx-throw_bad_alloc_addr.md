# `api::cxx::throw_bad_alloc_addr`

The `throw_bad_alloc_addr` in `src/api/cxx.rs`, one item for both architectures
(`src/api/cxx.rs:throw_bad_alloc_addr` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the loader's GOT entry and the
platform facts of part 3 as named residual assumptions: its aarch64 half
on the Armv8.5-A model and its x86_64 half on the x86-64 model.

## 1. Contract

Entry S1 of verus/SEAMS.md as numbered at 8635213 (S1 and
`src/api/cxx.rs:77` at 64d801a), kind external_body, no `requires`, no
`ensures`; `#[inline(never)]`:

```rust
fn throw_bad_alloc_addr() -> *const c_void
```

The bodies Verus does not see, at 64d801a: on x86_64

```rust
asm!(
    ".weak _ZSt17__throw_bad_allocv",
    "mov {out}, qword ptr [rip + _ZSt17__throw_bad_allocv@GOTPCREL]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

and on aarch64

```rust
asm!(
    ".weak _ZSt17__throw_bad_allocv",
    "adrp {out}, :got:_ZSt17__throw_bad_allocv",
    "ldr {out}, [{out}, :got_lo12:_ZSt17__throw_bad_allocv]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 7
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above is, up to
   whitespace, an `asm!` call in `fn throw_bad_alloc_addr` (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** The function compiles to exactly `adrp
   x0` with `R_AARCH64_ADR_GOT_PAGE _ZSt17__throw_bad_allocv`, `ldr x0, [x0]` with
   `R_AARCH64_LD64_GOT_LO12_NC _ZSt17__throw_bad_allocv`, and `ret`.
3. **Isla, aarch64.** On the Sail Armv8.5-A model (`armv8p5.ir`,
   isla-snapshots d8b3101), the three instructions in sequence
   (`got-fn.job.in`, one job for this function and the two others of
   the same shape), at Non-secure EL0 with the PC symbolic and the two
   relocated immediates symbolic opcode fields, under the relocations'
   definitions (ADRP's immediate `Page(got) - Page(P) >> 12` within 21
   signed bits, the LDR's `got[11:3]`, `got` the symbol's GOT entry)
   and the residual facts that `got` is 8-byte aligned and code and GOT
   lie below `2^48`: every feasible path reads memory once, at `got`,
   returns its contents in x0, writes no other register, no memory and
   no flag, and ends at the address in x30 (the return).
4. **Compiled code, x86_64.** The function compiles to exactly `mov
   rax, qword ptr [rip]` (`48 8b 05`) with `R_X86_64_GOTPCREL
   _ZSt17__throw_bad_allocv-0x4` on its displacement, and `ret` (`c3`).
5. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), the two instructions in sequence
   (`x86/got-fn.job`, one job for the three functions) from a symbolic
   state with the displacement a symbolic opcode field, under the
   relocation's definition (the displacement is `got` minus the next
   instruction's address, within 32 signed bits) and the residual facts
   that `got` is 8-byte aligned, code, GOT and stack are user addresses
   (below `2^47`), the stack pointer is 8-byte aligned and the return
   address on top of the stack is a user address: of the 741 trace
   combinations exactly four are feasible, and on each the model raises
   no fault, reads memory 16 times, a byte at a time, the 8 bytes at
   `got` and then the 8 at rsp; rax ends holding the first 8
   (little-endian), rip the second 8 and rsp is 8 higher; nothing else
   is written: no other register, no flag (`rflags`), no memory.

## 3. Translation

The contract has no `ensures`: Verus assumes only that the call returns
some pointer and changes nothing it tracks. Facts 3 and 5 are the
declared interface and more, on both architectures: only `out` written
(x0 or rax, the result register), `readonly` and `nostack` (no memory
write), `preserves_flags`, and a return to the caller (x30, or the
address the caller's `call` pushed, with rsp popped past it, the
x86-64 psABI's return). The value returned is the GOT entry's, which
the loader fills with the symbol's address (or 0 for an unresolved weak symbol: the doc's "or null"), a residual (System
V gABI; the AArch64 ELF ABI and the x86-64 psABI for the relocations)
nothing in the contract relies on.

Residual assumptions and gaps, aarch64: as in
[sys-tls-get_tp.md](sys-tls-get_tp.md), the model's MMU is off and the
code read is the static library's, and the configuration has TCR_EL1 =
0, so `ret` uses all 64 bits of x30 (under Linux's TBI0 it would ignore
a tag in the top byte, which return addresses do not carry).

Residual assumptions and gaps, x86_64 (verus/ISLA.md, "The x86_64
mixed seams"): the processor is in 64-bit mode (IA32_EFER.LMA and CS.L
set, as Linux runs 64-bit tasks; the model's `n64_bit_modep` answers
true by assumption instead of reading them); the application view, in
which linear addresses are the memory's (Linux maps the GOT and the
stack; a page fault is the kernel's, and the instruction restarts);
user addresses below `2^47`; the return address is the caller's (the
psABI). The model and the checker are as for the x86_64 `syscall`
records ([sys-raw-x86_64-syscall0.md](sys-raw-x86_64-syscall0.md),
part 3): a machine translation of ACL2 x86isa with no x87, MXCSR, mask
or CET state, and `check.rs` in the trusted base. The code read is the
static library's (fat LTO, one codegen unit); the function being
`#[inline(never)]`, the shared library has it as one function too.

## 4. Review

The aarch64 half was written on 2026-10-03 as an evidence note and
reviewed then by a fresh-context reviewer (a Claude subagent with no
prior context of this work), told to look for translations that
overclaim; its finding on these jobs is folded in: `fallthrough` proves
that each instruction continues at its own PC + 4, which the relocation
assumptions (P the ADRP's own address) and the final `ret` rely on. The
x86_64 half and the
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
compiled function is not exactly its instructions with their
relocations on either architecture, when `got-fn.job` (aarch64) or
`x86/got-fn.job` fails, and when a negative control passes: on aarch64
the load one GOT entry off and `ret` claimed to fall through; on x86_64
the load one entry off, `ret` claimed to fall through, and the job
without its 64-bit mode assumption named.
