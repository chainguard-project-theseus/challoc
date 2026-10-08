# `layout::regions::entry_barrier` (x86_64)

The x86_64 `entry_barrier` in `src/layout/regions.rs` (the item is
under `#[cfg(target_arch = "x86_64")]`; aarch64 has no barrier and no
such item).
Tool: Isla. Status: discharged, by the template pin of
`verus/isla/run.sh` and the empty-block check of
`verus/emptyasm/run.sh` (no instruction for Isla to trace), with
rustc's `inout` operand semantics as the residual assumption every
compiled item shares.

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

Two checks, as for [class-table-word_index.md](class-table-word_index.md)
(its part 2 describes both in full).

1. **The template pin.** `verus/isla/run.sh`, section 1: the `asm!`
   call above is, up to whitespace, an `asm!` call in
   `fn entry_barrier` (`verus/isla/templates.txt`); and this record's
   contract is the ledger entry's, for the item under
   `#[cfg(target_arch = "x86_64")]`.
2. **The block is empty in the shipped code.** `verus/emptyasm/run.sh`
   (`verus/emptyasm/blocks.txt`, the line for `entry_barrier`), with
   rustc 1.95.0 (LLVM 22), in its three steps: the object built beside
   the assembly is the shipped build's (byte-identical); in the
   assembly, every inline-asm block comes from an `asm!(` line, only
   listed blocks hold a comment, and every instance of this block's
   line (`src/layout/regions.rs`, the `asm!` in `fn entry_barrier`) in
   the x86_64 code is a comment and nothing else (`#APP`/`#NO_APP`
   bracket it), with at least one there and none in the aarch64 code;
   and the call, compiled alone into an x86_64 object beside the same
   function without it, adds no byte.

   At commit 2e78fb21: 20 instances on x86_64 (`class_entry` inlined
   into the size-class paths), each a comment alone naming the
   operand's register (`# rdi`, `# r8`, ...), the template as printed;
   none on aarch64.

   Reproduce: `sh verus/emptyasm/run.sh`; the negative test
   `sh verus/emptyasm/run.sh -n` makes the template `nop /* {0} */` on a
   copy of the tree and must see the check fail for this block.

## 3. Translation

- **No instruction.** By part 2 the block adds no byte to the shipped
  x86_64 code (the reading of the check is
  [class-table-word_index.md](class-table-word_index.md)'s, part 3:
  LLVM's assembler parser read the template, `/* r8 */` reprinted as
  `# r8`, and found no instruction or directive in it).
- **`inout(reg) p`.** One general register holds `p` on entry to the
  block and is read back as `p` on exit (the Rust reference, "Inline
  assembly", `inout`: the same register for input and output). With no
  instruction in between, the value read back is the value written, so
  `p` is unchanged. rustc's lowering of `inout` is read from its
  documentation: the residual every compiled item shares.
- **`options(pure, nomem, nostack, preserves_flags)`.** Promises about
  the block (no side effect, no memory access, no stack use, no flag
  write) that an empty instruction sequence keeps.
- **`ensures r == p`.** The pointer comes back as it went in, address
  and provenance. The provenance is the operand's: an `asm!` block's
  pointer output may carry any provenance its inputs carry, and the one
  input is `p`, with the bits unchanged; the same reading as
  `api::shared_base`'s record.
- **`opens_invariants none`.** No ghost code in the body.

Gaps: as in [class-table-word_index.md](class-table-word_index.md) (the
build checked is the default-feature one with rustc 1.95.0; the tool
column says Isla because the check runs inside `verus/isla/run.sh`).

## 4. Review

Written with package P7. A fresh-context review of the package (a Claude
subagent with no prior context of this work) read this record with the
change. Revised on 2026-10-08 (package EMPTYASM): the assembler's
comment syntax, argued until then, is checked by
`verus/emptyasm/run.sh`, and the status goes from evidence to
discharged; reviewed by a fresh-context reviewer with the checker,
whose findings are folded in. A human review is pending.

## 5. Tests

`verus/isla/run.sh`, section 1, fails when the `asm!` call changes (any
instruction added to the template, or another operand or option), when
the ledger entry with this record's status has another contract, and
when that entry's item is not under the `cfg` of this record's title
(`x86_64`); it then runs `verus/emptyasm/run.sh`, which fails when an
instance of the block holds anything but a comment, when the block has
no instance in the x86_64 code or one in the aarch64 code, or when the
object built beside the assembly is not the shipped build's, or when
the block, compiled alone into an object, adds an instruction. Neither
runs in CI; both are run by hand (`verus/ISLA.md`, and
`docs/testing.md`, "Empty asm blocks").
