# `layout::regions::entry_barrier` (x86_64)

The x86_64 `entry_barrier` in `src/layout/regions.rs` (the item is
under `#[cfg(target_arch = "x86_64")]`; aarch64 has no barrier and no
such item). Tool: Isla. Status: evidence only: the template is empty, so
there is no instruction for Isla to trace; what is checked is the
template pin.

## 1. Contract

The ledger's entry (`verus/SEAMS.md`, `entry_barrier`), at the commit
that split it out of `class_entry` (package P7, READY+HEAPPERM), kind
external_body; `#[inline(always)]`:

```rust
fn entry_barrier(p: *const super::Class) -> (r: *const super::Class)
    ensures
        r == p,
    opens_invariants none
```

The body Verus does not see:

```rust
let mut p = p;
// SAFETY: an empty instruction sequence; `p` comes out unchanged.
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags));
}
p
```

Its one caller, `class_entry`, is verified: it reads the class table's
entry through the permission to it frozen at `READY`
(`vspec::ptr::axiom_array_elem` of `api::Published::classes`), at the
pointer this returns. Before the split the barrier was inside
`class_entry`'s trusted body, with no record.

## 2. Statement

`verus/isla/run.sh`, section 1: the `asm!` call above is, up to
whitespace, an `asm!` call in `fn entry_barrier`
(`verus/isla/templates.txt`). Its template is `/* {0} */`: an assembler
comment naming the operand, which assembles to no instruction. Nothing
else runs: Isla traces instructions, and the block has none (the effect
the hint has is on register allocation, which the codegen gate, not
this record, compares with upstream).

## 3. Translation

- `inout(reg) p`: one general register holds `p` on entry to the block
  and is read back as `p` on exit (the Rust reference, "Inline
  assembly", `inout`: the same register for input and output). With no
  instruction in between, the value read back is the value written, so
  `p` is unchanged.
- `options(pure, nomem, nostack, preserves_flags)`: promises about the
  block (no side effect, no memory access, no stack use, no flag write)
  that an empty instruction sequence keeps trivially.
- `ensures r == p`: the pointer comes back as it went in, address and
  provenance. The provenance is the operand's: an `asm!` block's
  pointer output may carry any provenance its inputs carry, and the one
  input is `p`, with the bits unchanged; the same reading as
  `api::shared_base`'s record.

Gaps: the assembler's treatment of `/* */` as a comment (GNU as and
LLVM's integrated assembler both do, for x86_64), and rustc's operand
semantics, are read from their documentation, not checked.

## 4. Review

Written with package P7. A fresh-context review of the package (a Claude
subagent with no prior context of this work) read this record with the
change; a human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (any instruction
added to the template, or another operand or option), until this record
is revisited, and when the ledger entry with this record's status has
another contract.
