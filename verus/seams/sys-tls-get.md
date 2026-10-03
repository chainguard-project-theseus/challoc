# `sys::tls::get` (aarch64 half)

Evidence note, not a discharge: entry S241 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla, as part of `get_tp`'s
record; its x86_64 half (`mov out, [rip + challoc_tls@GOTTPOFF]; mov
out, fs:[out]`) is not, as there is no x86_64 Sail model for Isla
(verus/ISLA.md, plan item 1). When it is, this note becomes the seam's
record.

## 1. Contract

Entry S241 of verus/SEAMS.md at 8635213 (`src/sys/tls.rs:125`, kind
external_body; `#[inline(always)]`):

```rust
pub fn get(Tracked(slot): Tracked<&TlsSlot>) -> (r: *mut c_void)
    ensures
        r as usize == slot.value(),
    opens_invariants none
```

The aarch64 body is `get_tp`'s asm block with the thread pointer
discarded (`tp = out(reg) _`), checked verbatim against
`verus/isla/templates.txt`.

## 2. Statement (aarch64)

`get` and `get_tp` compile to the same four instructions, and every
`challoc_tls` load site in the aarch64 build is checked by the TLS load
jobs of [sys-tls-get_tp.md](sys-tls-get_tp.md) (part 2, facts 2 and 3;
70 sites, 12 register assignments at 8635213): the sequence reads the
GOT entry and then the calling thread's slot, returns the slot's 8
bytes, writes only its three registers, no memory and no flags, and
falls through.

## 3. Translation (aarch64 half)

As `get_tp`'s record, part 3: the `ensures` follows from the read being
of the calling thread's slot and the ghost model's premise that the
token's value is the slot's contents (verus/THEOREMS.md, theorem 15),
with the ELF TLS and loader residuals and the same gaps.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. It found nothing specific to this note. A
human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when `get`'s aarch64 `asm!` call changes, and
on everything `get_tp`'s record lists for the load sites.
