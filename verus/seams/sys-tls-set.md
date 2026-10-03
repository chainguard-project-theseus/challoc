# `sys::tls::set` (aarch64 half)

Evidence note, not a discharge: entry S243 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla below; its x86_64 half
(`mov off, [rip + challoc_tls@GOTTPOFF]; mov fs:[off], val`) is not, as
there is no x86_64 Sail model for Isla (verus/ISLA.md, plan item 1).
When it is, this note becomes the seam's record.

## 1. Contract

Entry S243 of verus/SEAMS.md at 8635213 (`src/sys/tls.rs:208`, kind
external_body; `#[inline(always)]`):

```rust
pub fn set(value: *mut c_void, Tracked(slot): Tracked<&mut TlsSlot>)
    ensures
        final(slot).value() == value as usize,
        final(slot).creates() == old(slot).creates(),
```

The aarch64 body Verus does not see, at 8635213:

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

## 2. Statement (aarch64)

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A model, checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 5
and 7.

1. **Template.** The aarch64 `asm!` call in `fn set` is the one above
   (`verus/isla/templates.txt`).
2. **Compiled code.** Every `challoc_tls` relocation sits in the load or
   the store shape ([sys-tls-get_tp.md](sys-tls-get_tp.md), fact 2); the
   store sites (5 at 8635213, 2 register assignments) are `adrp A; ldr
   A, [A]; mrs B, tpidr_el0; str C, [B, A]` with the two GOT
   relocations and A, B, C distinct.
3. **Isla.** For each assignment (`tls-store.job.in`), under the
   assumptions of the load jobs (the relocations' definitions, the GOT
   entry aligned, addresses below `2^48`, the slot 8-byte aligned):
   exactly one feasible combination of traces, in which the sequence
   reads memory once, at the GOT entry, writes memory once, C's value,
   8 bytes, at TPIDR_EL0 plus the offset read, writes no register but A
   (the offset) and B (TPIDR_EL0), no flag, and falls through.

## 3. Translation (aarch64 half)

`ensures final(slot).value() == value as usize`: fact 3 shows the
calling thread's slot ends holding `value`'s bits and nothing else in
memory changes; that the token's value is then the slot's contents is
the ghost model's premise ([sys-tls-get_tp.md](sys-tls-get_tp.md), part
3; verus/THEOREMS.md, theorem 15), here as the definition of what `set`
does to the token. `final(slot).creates() == old(slot).creates()` is
about ghost state only: the asm block has no ghost effect. The declared
interface holds: the outputs `off` and `tp` are the only registers
written, `nostack` (the one memory write is the slot, not the stack),
`preserves_flags`. Residuals and gaps as in `get_tp`'s record (ELF TLS
initial-exec, the loader, the thread pointer; MMU off; the static
library's code).

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. It found nothing specific to this note. A
human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the aarch64 `asm!` call changes, when a
`challoc_tls` relocation sits outside the two shapes or no store site
remains, when a `tls-store` job fails, and when its negative controls
(the offset stored in place of the value; the store claimed to write
nothing) pass.
