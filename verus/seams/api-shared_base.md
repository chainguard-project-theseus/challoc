# `api::shared_base` (aarch64)

The aarch64 `shared_base` in `src/api/mod.rs`, under
`#[cfg(target_arch = "aarch64")]`. The x86_64 one, under
`#[cfg(target_arch = "x86_64")]`, has no `asm!`: its body is
`shared_storage_ptr() as *const Shared`, and Verus verifies it against
the same contract (since P7 the pointer, address and provenance), from
`shared_storage_ptr`'s ensures (S33, trusted, no tool).
Tool: Isla. Status: discharged, by the template pin of
`verus/isla/run.sh` and the empty-block check of
`verus/emptyasm/run.sh` (no instruction for Isla to trace), given
`shared_ptr`'s contract (S29, its own seam, trusted, no tool) and with
rustc's `inout` operand semantics as the residual assumption every
compiled item shares.

## 1. Contract

The ledger's entry (`verus/SEAMS.md`, `shared_base`), at the split of
the item per architecture (package ASMHINT), with the `ensures`
strengthened earlier from the address to the pointer by package P7
(READY+HEAPPERM; before both, at the port of upstream #77 onto `lane`,
one item for both), kind external_body; `#[inline(always)]`:

```rust
pub fn shared_base() -> (r: *const Shared)
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
```

The body Verus does not see:

```rust
let mut p = shared_ptr();
// SAFETY: an empty template; the pointer is unchanged.
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags));
}
p
```

## 2. Statement

Two checks, as for [class-table-word_index.md](class-table-word_index.md)
(its part 2 describes both in full).

1. **The template pin.** `verus/isla/run.sh`, section 1: the `asm!`
   call above is, up to whitespace, an `asm!` call in `fn shared_base`
   (`verus/isla/templates.txt`); and this record's contract is the
   ledger entry's, for the item under `#[cfg(target_arch = "aarch64")]`.
2. **The block is empty in the shipped code.** `verus/emptyasm/run.sh`
   (`verus/emptyasm/blocks.txt`, the line for `shared_base`), with
   rustc 1.95.0 (LLVM 22), in its three steps: the object built beside
   the assembly is the shipped build's (byte-identical); in the
   assembly, every inline-asm block comes from an `asm!(` line, only
   listed blocks hold a comment, and every instance of this block's
   line (`src/api/mod.rs`, the `asm!` in `fn shared_base`) in the
   aarch64 code is a comment and nothing else, with at least one there
   and none in the x86_64 code; and the call, compiled alone into an
   aarch64 object beside the same function without it, adds no byte.

   At commit 2e78fb21: 13 instances on aarch64 (inlined into `free`,
   `free_sized` and the other free paths), each `// x<n>` alone; none on
   x86_64.

   Reproduce: `sh verus/emptyasm/run.sh`; the negative test
   `sh verus/emptyasm/run.sh -n` makes the template `nop /* {0} */` on a
   copy of the tree and must see the check fail for this block.

## 3. Translation

- **No instruction.** By part 2 the block adds no byte to the shipped
  aarch64 code (the reading of the check is
  [class-table-word_index.md](class-table-word_index.md)'s, part 3).
- **`inout(reg) p`.** One general register holds `p` on entry to the
  block and is read back as `p` on exit (the Rust reference, "Inline
  assembly", `inout`: the same register for input and output). With no
  instruction in between, the value read back is the value written, so
  `p` is unchanged, bits and provenance: an `asm!` block's pointer
  output may carry the provenance its inputs carry, and the one input
  is `p`. rustc's lowering of `inout` is read from its documentation:
  the residual every compiled item shares.
- **`options(pure, nomem, nostack, preserves_flags)`.** Promises about
  the block (no side effect, no memory access, no stack use, no flag
  write) that an empty instruction sequence keeps.
- **`ensures r == Shared::storage() as *const Shared`.** `p` is
  `shared_ptr()`, whose contract (S29) is this same `ensures`; the
  block returns it unchanged. `shared_ptr`'s body,
  `(&raw const SHARED.0).cast::<Shared>()`, is a pointer to the static
  `SHARED`'s cell, the pointer `Shared::storage()` names
  (`shared_storage_ptr` projects the same cell, through `get`): that
  projection is S29's own trusted fact (no tool), not this record's.
  Since package P7 the provenance is claimed, because `shared_deref`
  reads through the permission frozen at `READY`
  (`vstd::raw_ptr::ptr_ref`), which needs the pointer the permission is
  for, not just its address.
- **`opens_invariants none`.** No ghost code in the body; `shared_ptr`
  is declared `opens_invariants none` too.

The block's effect on register allocation (why it is there: the free
paths keep `SHARED`'s address in one register and read each field at an
immediate offset) is the codegen gate's to compare with upstream, not
this record's.

Gaps: as in [class-table-word_index.md](class-table-word_index.md) (the
build checked is the default-feature one with rustc 1.95.0; the tool
column says Isla because the check runs inside `verus/isla/run.sh`).

## 4. Review

Written on 2026-10-06 with the port of upstream #77. Reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), with the whole port: it found the contract true of the body
(for `shared_base`: `&raw const SHARED.0` and `SHARED.0.get()` are one
address, `UnsafeCell` being `repr(transparent)`) and nothing to fix in
this record. Package P7 (READY+HEAPPERM) strengthened the `ensures` to
the pointer; its fresh-context review read this record with the change.
Revised on 2026-10-07 for the split per architecture (package ASMHINT),
reviewed then by a fresh-context reviewer: it found the contract true
of the aarch64 body, nothing newly trusted, and doc fixes only, folded
in. Revised on 2026-10-08 (package EMPTYASM): the assembler's comment
syntax, argued until then, is checked by `verus/emptyasm/run.sh`, and
the status goes from evidence to discharged, the projection of the
static now named as `shared_ptr`'s (S29) rather than this record's
residual; reviewed by a fresh-context reviewer with the checker,
whose findings are folded in. A human review is pending.

## 5. Tests

`verus/isla/run.sh`, section 1, fails when the `asm!` call changes (any
instruction added to the template, or another operand or option), when
the ledger entry with this record's status has another contract, and
when that entry's item is not under the `cfg` of this record's title
(`aarch64`); it then runs `verus/emptyasm/run.sh`, which fails when an
instance of the block holds anything but a comment, when the block has
no instance in the aarch64 code or one in the x86_64 code, or when the
object built beside the assembly is not the shipped build's, or when
the block, compiled alone into an object, adds an instruction. Neither
runs in CI; both are run by hand (`verus/ISLA.md`, and
`docs/testing.md`, "Empty asm blocks").
