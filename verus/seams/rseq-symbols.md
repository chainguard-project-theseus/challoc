# `rseq::symbols`

The `symbols` in `src/rseq.rs`, one item for both architectures
(`src/rseq.rs:symbols` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the loader's GOT entries, the
return-area convention and the platform facts of part 3 as named
residual assumptions: its aarch64 half on the Armv8.5-A model and its
x86_64 half on the x86-64 model.

## 1. Contract

Entry S196 of verus/SEAMS.md as numbered at 8635213 (S199 and
`src/rseq.rs:177` at 64d801a), kind external_body, no `requires`, no
`ensures`; `#[inline(never)]`:

```rust
fn symbols() -> (*const isize, *const u32, *const u32)
```

The bodies Verus does not see, at 64d801a: the `.weak` directives for
the three symbols, then on x86_64

```rust
asm!(
    "mov {so}, qword ptr [rip + __rseq_offset@GOTPCREL]",
    "mov {ss}, qword ptr [rip + __rseq_size@GOTPCREL]",
    "mov {sf}, qword ptr [rip + __rseq_flags@GOTPCREL]",
    so = out(reg) so,
    ss = out(reg) ss,
    sf = out(reg) sf,
    options(nostack, readonly, preserves_flags),
);
```

and on aarch64

```rust
asm!(
    "adrp {so}, :got:__rseq_offset",
    "ldr {so}, [{so}, :got_lo12:__rseq_offset]",
    "adrp {ss}, :got:__rseq_size",
    "ldr {ss}, [{ss}, :got_lo12:__rseq_size]",
    "adrp {sf}, :got:__rseq_flags",
    "ldr {sf}, [{sf}, :got_lo12:__rseq_flags]",
    so = out(reg) so,
    ss = out(reg) ss,
    sf = out(reg) sf,
    options(nostack, readonly, preserves_flags),
);
```

and the tuple `(so, ss, sf)` returned.

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 7
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above, with its `.weak`
   lines, is, up to whitespace, an `asm!` call in `fn symbols`
   (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** The function compiles to exactly: the
   three GOT pairs into x8, x9 and x10, each `adrp` with
   `R_AARCH64_ADR_GOT_PAGE` and each `ldr` with
   `R_AARCH64_LD64_GOT_LO12_NC` of its symbol, then `stp x8, x9, [x0]`,
   `str x10, [x0, #16]` and `ret`: the compiler returns the 24-byte
   tuple through a pointer the caller passes in x0.
3. **Isla, aarch64.** On the Sail Armv8.5-A model (`armv8p5.ir`,
   isla-snapshots d8b3101), the nine instructions in sequence
   (`rseq-symbols.job.in`), under the relocations' definitions for each
   pair (P the `adrp`'s own address) and the residual facts that the
   GOT entries are 8-byte aligned, code, GOT entries and the return area
   lie below `2^48` and the return area is 8-byte aligned: every
   feasible path reads memory three times, at the three GOT entries in
   order, writes memory three times, the three values read to the
   return area's words at x0, x0 + 8 and x0 + 16, writes no register but
   x8-x10 and no flag, and ends at the address in x30.
4. **Compiled code, x86_64.** The function compiles to exactly: `mov
   rax`, `mov rcx` and `mov rdx, qword ptr [rip]` (`48 8b 05`, `48 8b
   0d`, `48 8b 15`), each with `R_X86_64_GOTPCREL` of its symbol, then
   `mov [rdi], rax`, `mov [rdi + 8], rcx`, `mov [rdi + 16], rdx` and
   `ret`: the compiler returns the tuple through a pointer the caller
   passes in rdi.
5. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), the seven instructions in sequence
   (`x86/rseq-symbols.job`) from a symbolic state with the three
   displacements symbolic opcode fields, under the relocation's
   definition for each (the displacement is the GOT entry minus the
   next instruction's address, within 32 signed bits) and the residual
   facts that the GOT entries are 8-byte aligned, code, GOT and stack
   are user addresses (below `2^47`), the return area is 8-byte aligned
   and lies below `2^47`, the stack pointer is 8-byte aligned and the
   return address on top of the stack is a user address: exactly four
   combinations of traces are feasible (of 5.1e10, all others cut at an
   infeasible prefix), and on each the model raises no fault, reads the
   three GOT entries' 8 bytes each in order, writes the three values to
   the return area's three words (24 bytes, a byte at a time), reads
   the 8 bytes at rsp into rip, ends with rsp 8 higher, and writes no
   register but rax, rcx, rdx (and rsp, rip) and no flag.

## 3. Translation

The contract has no `ensures`. Facts 3 and 5 show the asm block's
interface (only its three outputs written, no memory written by it, no
flags) and the compiler's code around it: the result stored where the
caller asked and nothing else written, x8-x10 (AAPCS64) and rax, rcx,
rdx (the x86-64 psABI) being caller-saved scratch registers. The
residuals: the loader fills each GOT entry with the symbol's address or
0 (weak, unresolved: the doc's "null when they do not resolve"), and
the return-area convention (x0 or rdi, 8-byte aligned) is the Rust ABI
the compiler chose for this call, which caller and callee share. Gaps,
aarch64: as in [sys-tls-get_tp.md](sys-tls-get_tp.md) (MMU off, the
static library's code) and in
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md)
(`ret` with TBI off). Residual assumptions and gaps, x86_64: as in
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md),
part 3 (64-bit mode by assumption, the application view, user
addresses, the caller's return address, the model and the checker).

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
relocations on either architecture, when `rseq-symbols.job` or
`x86/rseq-symbols.job` fails, and when a negative control passes (the
second value stored as the first, on each architecture).
