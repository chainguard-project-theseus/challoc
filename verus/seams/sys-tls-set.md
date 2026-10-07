# `sys::tls::set`

The `set` in `src/sys/tls.rs`, one item for both architectures
(`src/sys/tls.rs:set` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the ELF TLS and loader facts, the
platform facts of part 3 and the TLS slot model's premise (theorem 15)
as named residual assumptions: its aarch64 half on the Armv8.5-A model
and its x86_64 half on the x86-64 model. Isla shows which memory the
code writes, not what the token holds (part 3).

## 1. Contract

Entry S243 of verus/SEAMS.md as numbered at 8635213 (S247 and
`src/sys/tls.rs:208` at 64d801a; S264 since package P7, READY+HEAPPERM,
which restated the `ensures` as `TlsSlot::set_to`), kind external_body;
`#[inline(always)]`:

```rust
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsSlot>)
    ensures
        final(slot).set_to(*old(slot), value as usize),
```

The bodies Verus does not see, at 64d801a: on x86_64

```rust
asm!(
    "mov {off}, qword ptr [rip + challoc_tls@GOTTPOFF]",
    "mov qword ptr fs:[{off}], {val}",
    off = out(reg) _,
    val = in(reg) value,
    options(nostack, preserves_flags),
);
```

and on aarch64

```rust
asm!(
    "adrp {off}, :gottprel:challoc_tls",
    "ldr {off}, [{off}, :gottprel_lo12:challoc_tls]",
    "mrs {tp}, tpidr_el0",
    "str {val}, [{tp}, {off}]",
    off = out(reg) _,
    tp = out(reg) _,
    val = in(reg) value,
    options(nostack, preserves_flags),
);
```

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 5
and 7 (aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above is, up to
   whitespace, an `asm!` call in `fn set` (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** Every `challoc_tls` relocation sits in
   the load or the store shape ([sys-tls-get_tp.md](sys-tls-get_tp.md),
   fact 2); the store sites (5 at 8635213, 2 register assignments) are
   `adrp A; ldr A, [A]; mrs B, tpidr_el0; str C, [B, A]` with the two
   GOT relocations and A, B, C distinct.
3. **Isla, aarch64.** On the Sail Armv8.5-A model, for each assignment
   (`tls-store.job.in`), under the assumptions of the load jobs (the
   relocations' definitions, the GOT entry aligned, addresses below
   `2^48`, the slot 8-byte aligned): exactly one feasible combination
   of traces, in which the sequence reads memory once, at the GOT entry,
   writes memory once, C's value, 8 bytes, at TPIDR_EL0 plus the offset
   read, writes no register but A (the offset) and B (TPIDR_EL0), no
   flag, and falls through.
4. **Compiled code, x86_64.** The x86_64 TLS sites are as in
   [sys-tls-get.md](sys-tls-get.md), fact 3; the store sites are `mov
   A, qword ptr [rip]` with `R_X86_64_GOTTPOFF challoc_tls-0x4`, then
   `mov qword ptr fs:[A], V`: 5 at 64d801a, in 3 register assignments
   (A, V = rax, rbp; rcx, rax; rdx, rcx), each checked.
5. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), for each assignment the two instructions in
   sequence (`x86/tls-store.job.in`), under the assumptions of the load
   jobs ([sys-tls-get.md](sys-tls-get.md), fact 4): exactly four
   combinations of traces are feasible, and on each the model raises no
   fault, reads the 8 bytes at the GOT entry, writes V's 8 bytes to the
   slot (the FS base plus the offset read; a byte at a time,
   little-endian) and no other memory, writes no register but A (the
   offset) and no flag, and the pair falls through.

## 3. Translation

`ensures final(slot).set_to(*old(slot), value as usize)`, that is
(`TlsSlot::set_to`, an open spec) `final(slot).value() == value as
usize` and the token's `creates()`, `tp()` and `heap()` as they were:
facts 3 and 5 show the calling thread's slot ends holding `value`'s
bits and nothing else in memory changes; that the token's value is
then the slot's contents is the ghost model's premise
([sys-tls-get_tp.md](sys-tls-get_tp.md), part 3; verus/THEOREMS.md,
theorem 15), here as the definition of what `set` does to the token.
The other three are about ghost state only (the `Heap::create` count,
the thread pointer the token records, and the heap permission it
holds): the asm block has no ghost effect, and the thread pointer
register is not written (fact 3 on aarch64, where `tp` is an output the
block reads `tpidr_el0` into and does not write back; on x86_64 the
block names no thread-pointer register). The
declared interface holds: the outputs (`off`, and `tp` on aarch64) are
the only registers written, `nostack` (the one memory write is the
slot, not the stack), `preserves_flags`. Residuals and gaps as in
[sys-tls-get.md](sys-tls-get.md) (ELF TLS initial-exec, the loader, the
thread pointer; on aarch64 the MMU off; on x86_64 64-bit mode by
assumption and the application view; the static libraries' code).

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
wording of the template fact, and the seam numbers at 64d801a.
Package P7 (READY+HEAPPERM) restated the `ensures` over the token's new
ghost fields (`tp`, `heap`); its fresh-context review read this record
with the change. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when either `asm!` call changes, when the
ledger entry with this record's status has another contract, when a
`challoc_tls` relocation sits outside the two shapes or no store site
remains on either architecture, when a `tls-store` job fails, and when
a negative control passes (the offset stored in place of the value, on
each architecture; the aarch64 store claimed to write nothing).
