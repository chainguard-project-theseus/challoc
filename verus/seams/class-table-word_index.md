# `class::table::word_index` (aarch64)

The aarch64 `word_index` in `src/class/table.rs`, under
`#[cfg(target_arch = "aarch64")]`. The x86_64 one, under
`#[cfg(target_arch = "x86_64")]`, has no `asm!`: its body is `index`,
and Verus verifies it against the same contract.
Tool: Isla. Status: discharged, by the template pin of
`verus/isla/run.sh` and the empty-block check of
`verus/emptyasm/run.sh` (no instruction for Isla to trace), with
rustc's `inout` operand semantics as the residual assumption every
compiled item shares.

## 1. Contract

The ledger's entry (`verus/SEAMS.md`, `word_index`), at the split of
the item per architecture (before, at the port of upstream #87, one
item for both, `pub(crate)` since; at the port of upstream #77 onto
`lane`, private), kind external_body; `#[inline(always)]`:

```rust
pub(crate) fn word_index(index: usize) -> (r: usize)
    ensures
        r == index,
    opens_invariants none
```

The body Verus does not see:

```rust
let mut index = index;
// SAFETY: an empty template; `index` comes out unchanged.
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags));
}
index
```

Upstream #77 writes the same `asm!` inline in
`SlabTable::is_set_unchecked`; here it is this function, which
`is_set_unchecked` calls on `bit / 64` (inlined, the same machine
code), so that Verus sees only the contract. Upstream #87 writes it
inline a second time, in `heap::ClassLine::set_slab` (the `free` fast
path's bit test on the class line); there too it is this function,
`pub(crate)` since (re-exported as `class::word_index`), and the machine
code is upstream's.

## 2. Statement

Two checks.

1. **The template pin.** `verus/isla/run.sh`, section 1: the `asm!`
   call above is, up to whitespace, an `asm!` call in `fn word_index`
   (`verus/isla/templates.txt`); and this record's contract is the
   ledger entry's, for the item under `#[cfg(target_arch = "aarch64")]`.
2. **The block is empty in the code.** `verus/emptyasm/run.sh`
   (`verus/emptyasm/blocks.txt`, the line for `word_index`), with
   rustc 1.95.0 (LLVM 22, the toolchain the codegen gate and the perf
   budget pin), for aarch64 and x86_64, in three steps:
   - **The module.** It builds the library as `cargo build --release`
     does (default features), once with `--emit=asm,obj` and once with
     `-C save-temps`, and checks that the two objects of the one
     fat-LTO codegen unit are byte-identical: adding the assembly
     output leaves the shipped object as it is, and the assembly is
     printed from that object's module. (rustc prints it from a copy of
     the module, after the same passes; reassembling it, done once by
     hand on x86_64, gives the same instructions as the object except
     the order of a few in some functions and the encoding of some
     alignment `nop`s, so it is not a byte-exact listing of the object,
     and step 3 checks the object path itself. The save-temps build's
     `libchalloc.so` is byte-identical to a plain
     `cargo build --release`'s, compared once by hand on x86_64.)
   - **Every instance.** In that assembly LLVM prints each inline-asm
     block between `//APP` and `//NO_APP` (aarch64; `#APP`/`#NO_APP` on
     x86_64), after its assembler parser has read the template with the
     operand's register substituted, and the `.loc` in force (reset at
     each function's start) names the block's source line. Every block
     must come from a line holding an `asm!(` call, and only a listed
     block may hold a comment, so this block, whose printed text always
     holds one, cannot be attributed to another line; `word_index`'s
     line (the `asm!` at `src/class/table.rs`, found in
     `fn word_index`) must have at least one instance in the aarch64
     code and none in the x86_64 code, and each instance must hold
     nothing but comment lines.
   - **The object path.** It compiles the `asm!` call, verbatim from
     `fn word_index`, into a function that returns its operand
     (`--emit=obj`, opt-level 3, for aarch64), beside the same function
     without the block, and checks that the two disassemble to the same
     instructions, byte for byte (`ret`, the operand already in `x0`):
     the integrated assembler writes nothing for the template into an
     object.

   At commit 2e78fb21: 27 instances on aarch64 (inlined into the `free`
   paths), each `// x<n>` alone, the template's comment with the
   operand's register; none on x86_64.

   Reproduce: `sh verus/emptyasm/run.sh` (both targets, four builds),
   and `sh verus/emptyasm/run.sh -n`, the negative test: on a copy of
   the tree whose listed templates read `nop /* {0} */`, the check must
   fail for every listed block in both the instance and the object-path
   steps (it does: each instance holds a `nop`, and each probe gains
   one).

## 3. Translation

- **No instruction.** For each of the 27 places the shipped aarch64
  code inlines this block, LLVM's assembler parser, given the template
  with the register it was allocated, found only a comment (step 2,
  `/* x9 */` reprinted as `// x9` shows the text went through the
  parser), and the same parser, writing an object, writes no byte for
  the template (step 3). Inline-asm expansion depends on the template
  and the operands' registers only, not on the surrounding schedule,
  so the copy of the module the assembly is printed from expands each
  block as the object's module does. So the block adds no byte to the
  code. The negative test shows the check sees one `nop`.
- **`inout(reg) index`, `ensures r == index`.** One general register
  holds `index` on entry to the block and is read back as `index` on
  exit (the Rust reference, "Inline assembly", `inout`: the same
  register for input and output; the printed comment names that one
  register). No instruction runs in between, so the value read back is
  the value written: `r == index`. That rustc lowers `inout` to a tied
  operand is read from its documentation, as every compiled item's
  meaning rests on rustc's: the residual, and the same trust the Verus
  proofs place in the compiler for all verified code.
- **`options(pure, nomem, nostack, preserves_flags)`.** Promises (no
  side effect, no memory access, no stack use, no flag write) an empty
  instruction sequence keeps.
- **`opens_invariants none`.** No ghost code in the body.
- **x86_64.** The item is `index` itself, verified, and not this
  record's; the check confirms that no instance of this block is in
  the x86_64 code.

Gaps: the check runs on the build it makes, with rustc 1.95.0 and the
default features (quantum16). The `quantum8` build and other compilers
(the distribution package's rustc 1.98, say) are not checked by
default; `EMPTYASM_CARGO_FLAGS='--no-default-features --features
quantum8'` and `RUSTUP_TOOLCHAIN` run it for them. The ledger's tool
column says Isla because the check runs inside `verus/isla/run.sh`, with
the template pin; Isla itself traces nothing of this block.

The block's effect on instruction selection (why it is there: the word
index stays `bit >> 6` and the load scales it) is the codegen gate's to
compare with upstream, not this record's.

## 4. Review

Written on 2026-10-06 with the port of upstream #77. Reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), with the whole port: it found the contract true of the body and
nothing to fix in this record. Revised on 2026-10-07 for the split per
architecture (package ASMHINT), reviewed then by a fresh-context
reviewer: it found the contract true of the aarch64 body, nothing newly
trusted, and doc fixes only, folded in. Revised on 2026-10-08 (package
EMPTYASM): the assembler's comment syntax, argued until then, is
checked by `verus/emptyasm/run.sh`, and the status goes from evidence
to discharged; reviewed by a fresh-context reviewer with the checker,
whose findings are folded in (the assembly is printed from a copy of
the module, not a byte-exact listing of the object, hence the
object-path step; attribution reset at each function; the gaps above).
A human review is pending.

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
