# `class::table::word_index`

The `word_index` in `src/class/table.rs`, one item for both
architectures (the `asm!` is under `#[cfg(target_arch = "aarch64")]`
inside its body). Tool: Isla. Status: evidence only: the aarch64
template is empty, so there is no instruction for Isla to trace; what
is checked is the template pin.

## 1. Contract

The ledger's entry (`verus/SEAMS.md`, `word_index`), at the port of
upstream #77 onto `lane`, kind external_body; `#[inline(always)]`:

```rust
fn word_index(index: usize) -> (r: usize)
    ensures
        r == index,
    opens_invariants none
```

The body Verus does not see:

```rust
#[allow(unused_mut)]
let mut index = index;
#[cfg(target_arch = "aarch64")]
// SAFETY: an empty template; `index` comes out unchanged.
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags));
}
index
```

Upstream #77 writes the same `asm!` inline in
`SlabTable::is_set_unchecked`; here it is this function, which
`is_set_unchecked` calls on `bit / 64` (inlined, the same machine
code), so that Verus sees only the contract.

## 2. Statement

`verus/isla/run.sh`, section 1: the `asm!` call above is, up to
whitespace, an `asm!` call in `fn word_index`
(`verus/isla/templates.txt`). Its template is `/* {0} */`: an assembler
comment naming the operand, which assembles to no instruction. Nothing
else runs: Isla traces instructions, and the block has none (its effect
is on instruction selection: the word index stays `bit >> 6` and the
load scales it, which the codegen gate, not this record, compares with
upstream).

## 3. Translation

- `inout(reg) index`: one general register holds `index` on entry to
  the block and is read back as `index` on exit (the Rust reference,
  "Inline assembly", `inout`). With no instruction in between, the
  value read back is the value written: `r == index`, on aarch64 as
  off it (where the block is compiled out and the body is `index`).
- `options(pure, nomem, nostack, preserves_flags)`: promises an empty
  instruction sequence keeps trivially.

Gaps: as in [api-shared_base.md](api-shared_base.md), the assembler's
comment syntax and rustc's operand semantics are read from their
documentation, not checked.

## 4. Review

Written on 2026-10-06 with the port of upstream #77. Reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), with the whole port: it found the contract true of the body and
nothing to fix in this record. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (any
instruction added to the template, or another operand or option),
until this record is revisited, and when the ledger entry with this
record's status has another contract.
