# `ClassLine::guard_word`

Seam S130 of `verus/SEAMS.md`: src/heap/shared.rs:518. Kind: external_body. Category: inline asm.
Evidence: Kani settles the load (on a pinned copy); the x86_64 `asm!` block, an empty template, is pinned for Isla but has no instruction to trace, so its effect (none) is argued, as for `word_index`'s record (Translation, "Status").

## Contract

At commit 4f75a5b7 (added by the port of upstream #101):

```rust
fn guard_word(&self) -> (r: u64)
    requires
        self._pad[0] == 0 && self._pad[1] == 0,
    ensures
        r == guard_word_of(self.guard_mask, self.tz, self.lg_guard),
    opens_invariants none
```

with `guard_word_of(guard_mask, tz, lg_guard) = guard_mask as u64 |
(tz as u64) << 32 | (lg_guard as u64) << 40` (an open spec fn,
heap/shared.rs). The body Verus does not see, `#[inline(always)]`:

```rust
#[allow(unused_mut)]
let mut gw = unsafe {
    ptr::read(
        (self as *const ClassLine as *const u8)
            .add(core::mem::offset_of!(ClassLine, guard_mask))
            as *const u64,
    )
};
#[cfg(target_arch = "x86_64")]
unsafe {
    core::arch::asm!("/* {0} */", inout(reg) gw, options(pure, nomem, nostack, preserves_flags))
};
gw
```

It is on the `free` path (the sized check, `ClassLine::sized_ok`, and
the guard test, `ClassLine::is_guard_at`, each passing the word to
`is_guard`).

## Statement

Two parts, one per half of the body.

1. **The load: Kani.** Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21)
   proves the harness `heap::shared::kani_guard_word::guard_word_is_the_three_fields`
   (`src/heap/shared.rs`). It runs `ClassLine::guard_word_load`, a
   copy of the body without the `asm!` block (Kani has no model of
   inline assembly: the real body fails a harness with "InlineAsm is
   not currently supported"). The copy and the original are pinned
   line by line in `verus/kani/pins.txt`, which `verus/kani/run.sh`
   requires in place, consecutive and in order (blank and `//` lines
   aside): the original's signature, contract, load, `asm!` block and
   return, then the copy's load and return. The harness checks, under
   both quanta, that `guard_mask` is at an 8-aligned offset whose 8
   bytes end the struct, that `tz`, `lg_guard` and `_pad` sit at
   offsets 4, 5 and 6 from it, and that the target is little-endian;
   and, on a `ClassLine` whose integer fields are any value
   (`kani::any()`) but `_pad`, which is zero (the atomic `committed`
   and the zero-sized ghost field stay zeroed, outside the 8 bytes
   read), that the load returns
   `guard_mask as u64 | (tz as u64) << 32 | (lg_guard as u64) << 40`,
   with no undefined behaviour Kani detects (the read is inside the
   object and aligned).

   Reproduce: `sh verus/kani/run.sh --harness heap::shared::kani_guard_word::guard_word_is_the_three_fields`
   (both quanta; `--harness` takes the full path).

   A control (run by hand, not kept): with `_pad` any value instead of
   zero, the harness fails (the returned word has the padding's bits).

2. **The `asm!` block: the template pin.** `verus/isla/run.sh`,
   section 1: the `asm!` call above is, up to whitespace, an `asm!`
   call in `fn guard_word` (`verus/isla/templates.txt`, the line added
   with this record; the extractor now ends a call at its matching
   parenthesis, since this block, unlike `shared_base`'s and
   `word_index`'s, has no `;` of its own). Its template is `/* {0} */`:
   an assembler comment naming the operand, which assembles to no
   instruction. Nothing else runs; Isla traces instructions and the
   block has none (on aarch64 it is compiled out).

## Translation

- **`requires self._pad[0] == 0 && self._pad[1] == 0`.** The harness
  sets `_pad` to zero, as `ClassLine::new` does; the contract's
  `guard_word_of` has no padding term, so the requirement is what
  makes the top 16 bits zero (the control shows they are the
  padding's otherwise). All 8 bytes are initialised (four fields), so
  the `u64` read reads no uninitialised byte.
- **`ensures r == guard_word_of(...)`.** By part 1, the load of the
  last 8 bytes is that value: `guard_mask`'s 4 bytes are the low 32
  bits, `tz` bits 32..40, `lg_guard` bits 40..48, `_pad` the top 16
  (zero), little-endian. The layout is `#[repr(C, align(64))]`'s, the
  same on both targets (LP64, little-endian: the `const` asserts after
  `ClassLine`'s impl check offsets 56..62 and the endianness at every
  build, on both); Kani checks it for x86_64 and the asserts for each
  target built. By part 2, on x86_64 the block leaves `gw` unchanged:
  `inout(reg) gw` puts `gw` in one general register on entry and reads
  the same register back as `gw` on exit (the Rust reference, "Inline
  assembly", `inout`), and no instruction runs in between, so the
  value read back is the value written. `options(pure, nomem, nostack,
  preserves_flags)` are promises (no side effect, no memory access, no
  stack use, no flag write) that an empty instruction sequence keeps
  trivially. On aarch64 there is no block.
- **`opens_invariants none`.** No ghost code in the body (read from
  the source).
- **Returns.** No panic site: `ptr::read`, `add` and the block cannot
  panic; the harness reaches the end for every input.
- **The copy.** The harness runs the copy, not the original, so the
  claim for the load rests on the pin (the two read the same lines,
  verbatim, which `run.sh` checks) and on `guard_word_load` being a
  method of `ClassLine` with the same receiver, so `self` names the
  same thing.

**Status: evidence.** The assembler's treatment of `/* */` as a
comment (GNU as and LLVM's integrated assembler both do, on x86_64),
and rustc's `inout` operand semantics, are read from their
documentation, not checked: the residual that keeps
`class-table-word_index.md` (an empty aarch64 template and nothing
else) at evidence, and `api-shared_base.md` with it. The load half is
settled; the record becomes "discharged" with those two if empty
template pins are accepted as discharging (an open question for the
maintainers, not settled in this repository). The block's effect on
register allocation (why it is there: without it LLVM splits the load
into a byte load per shift count) is the codegen gate's to compare,
not this record's.

Common to every Kani record: Kani checks the MIR of a debug build
(overflow checks and debug assertions on) for x86_64, with CBMC's
bit-precise semantics for the integer operations and Kani's own models
of the `core` intrinsics it lowers. The shipped library is a release
build: it drops the overflow checks and debug assertions (where a
harness shows they never fire under the contract, the two builds
agree), and it is also built for aarch64, whose layouts and pointer
width match x86_64's for every type here (both are LP64 targets;
the layout records name the one check that runs per target). The
`core` Kani checks is its own nightly's (2026-08-21); the shipped
library is built with rustc 1.98, whose `core` functions here are the
same unless they changed between the two, which a rerun with a newer
Kani would show. Policy for every Kani record: a trusted body must
return under its `requires` without a panic, except an abort seam
(whose contract is that it does not return) and the overflow aborts
a contract's comment declares (`fig_*`, `stats_allocated`); a panic
in challoc aborts the process (`panic = "abort"`), so it never yields
a wrong result, but the proofs' "no panic" claim rests on this policy.

## Review

Written 2026-10-07 at commit 4f75a5b7 (GUARDWORD): the contract above
is the ledger's entry there, and the harness and the pins were
compared with each clause. A fresh-context subagent reviewed this
record against the harness, the pins, the template line and the
contract; it judged the status "evidence" by the standard of
`word_index`'s record (first written as "discharged"), and its other
findings are folded in.

Run at commit 4f75a5b7, 2026-10-07, with Kani 0.68.0 under both
quanta: `heap::shared::kani_guard_word::guard_word_is_the_three_fields`
passes (0 of 111 checks failed in each, 2 unreachable). `verus/isla/run.sh`
section 1 passes (39 templates, the 28 Isla records); its later
sections (the instruction traces) need the Armv9.4-A snapshot
(`armv9p4.ir`), which this lane's image lacks, and trace no
instruction of this block (it has none).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") checks that the
contract above is still the ledger's entry for this seam and that the
pinned lines of the original and the copy are in place
(`verus/kani/pins.txt`), then runs the harness under both quanta: a
copy that breaks the contract fails the harness, and a changed
contract or a changed body (either half) fails those checks until
this record, the copy and the harness are revisited.
`sh verus/isla/run.sh` fails when the `asm!` call changes (any
instruction added to the template, or another operand or option).
The scripts are run by hand; no CI job runs them yet.
