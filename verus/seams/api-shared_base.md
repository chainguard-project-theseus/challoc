# `api::shared_base`

The `shared_base` in `src/api/mod.rs`, one item for both architectures
(the `asm!` is under `#[cfg(target_arch = "aarch64")]` inside its body).
Tool: Isla. Status: evidence only: the aarch64 template is empty, so
there is no instruction for Isla to trace; what is checked is the
template pin, and the projection of the static stays assumed.

## 1. Contract

The ledger's entry (`verus/SEAMS.md`, `shared_base`), at the port of
upstream #77 onto `lane`, with the `ensures` strengthened from the
address to the pointer by package P7 (READY+HEAPPERM), kind
external_body; `#[inline(always)]`:

```rust
pub fn shared_base() -> (r: *const Shared)
    ensures
        r == Shared::storage() as *const Shared,
    opens_invariants none
```

The body Verus does not see:

```rust
#[allow(unused_mut)]
let mut p = shared_ptr();
#[cfg(target_arch = "aarch64")]
// SAFETY: an empty template; the pointer is unchanged.
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags));
}
p
```

## 2. Statement

`verus/isla/run.sh`, section 1: the `asm!` call above is, up to
whitespace, an `asm!` call in `fn shared_base`
(`verus/isla/templates.txt`). Its template is `/* {0} */`: an assembler
comment naming the operand, which assembles to no instruction. Nothing
else runs: Isla traces instructions, and the block has none (the
release build's `free`, `free_sized` and `challoc_free_usable` contain
no instruction from it; the effect the hint has is on register
allocation, which the codegen gate, not this record, compares with
upstream).

## 3. Translation

- `inout(reg) p`: one general register holds `p` on entry to the block
  and is read back as `p` on exit (the Rust reference, "Inline
  assembly", `inout`: the same register for input and output). With no
  instruction in between, the value read back is the value written, so
  `p` is unchanged, on aarch64 as off it (where the block is compiled
  out).
- `options(pure, nomem, nostack, preserves_flags)`: promises about the
  block (no side effect, no memory access, no stack use, no flag
  write) that an empty instruction sequence keeps trivially.
- `ensures r == Shared::storage() as *const Shared`: `p` is
  `shared_ptr()`, `(&raw const SHARED.0).cast::<Shared>()`, a pointer to
  the static `SHARED`'s cell, which is the pointer `Shared::storage()`
  names (`shared_storage_ptr` projects the same cell, through `get`):
  the same address, and the provenance of the same static, the one
  allocation both projections are derived from. That projection is the
  residual: the same raw-pointer fact as `shared_ptr`'s and
  `shared_storage_ptr`'s, trusted there too (no tool). Through the
  empty block the pointer keeps its provenance as well as its bits: an
  `asm!` block's pointer output may carry the provenance its inputs
  carry, and the one input is `p`. Since package P7 the provenance is
  claimed, because `shared_deref` now reads through the permission
  frozen at `READY` (`vstd::raw_ptr::ptr_ref`), which needs the pointer
  the permission is for, not just its address.

Gaps: the assembler's treatment of `/* */` as a comment (GNU as and
LLVM's integrated assembler both do, for AArch64), and rustc's
operand semantics, are read from their documentation, not checked.

## 4. Review

Written on 2026-10-06 with the port of upstream #77. Reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), with the whole port: it found the contract true of the body
(for `shared_base`: `&raw const SHARED.0` and `SHARED.0.get()` are one
address, `UnsafeCell` being `repr(transparent)`) and nothing to fix in
this record. Package P7 (READY+HEAPPERM) strengthened the `ensures` to
the pointer; its fresh-context review read this record with the change.
A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (any
instruction added to the template, or another operand or option),
until this record is revisited, and when the ledger entry with this
record's status has another contract.
