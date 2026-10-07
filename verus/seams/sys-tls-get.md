# `sys::tls::get`

The `get` in `src/sys/tls.rs`, one item for both architectures
(`src/sys/tls.rs:get` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the ELF TLS and loader facts, the
platform facts of part 3 and the TLS slot model's premise (theorem 15)
as named residual assumptions: its aarch64 half on the Armv8.5-A model
(the load jobs of [sys-tls-get_tp.md](sys-tls-get_tp.md)) and its
x86_64 half on the x86-64 model. Isla shows which memory the code
reads, not that the token equals it (part 3).

## 1. Contract

Entry S238 of verus/SEAMS.md as numbered at 8635213 (S242 and
`src/sys/tls.rs:125` at 64d801a), kind external_body;
`#[inline(always)]`:

```rust
pub fn get(Tracked(slot): Tracked<&TlsSlot>) -> (r: *mut c_void)
    ensures
        r as usize == slot.value(),
    opens_invariants none
```

The bodies Verus does not see, at 64d801a: on x86_64

```rust
asm!(
    "mov {out}, qword ptr [rip + challoc_tls@GOTTPOFF]",
    "mov {out}, qword ptr fs:[{out}]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

and on aarch64 `get_tp`'s asm block with the thread pointer discarded
(`tp = out(reg) _`).

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 5
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls (the one above and the
   aarch64 one) is, up to whitespace, an `asm!` call in `fn get`
   (`verus/isla/templates.txt`).
2. **aarch64.** `get` and `get_tp` compile to the same four
   instructions, and every `challoc_tls` load site in the aarch64 build
   is checked by the TLS load jobs of
   [sys-tls-get_tp.md](sys-tls-get_tp.md) (part 2, facts 2 and 3; 70
   sites, 12 register assignments at 8635213): the sequence reads the
   GOT entry and then the calling thread's slot, returns the slot's 8
   bytes, writes only its three registers, no memory and no flags, and
   falls through.
3. **Compiled code, x86_64.** Every `R_X86_64_GOTTPOFF` relocation in
   the x86_64 release build is against `challoc_tls` and sits on the
   displacement of a `mov R, qword ptr [rip]` whose next instruction is
   the load `mov R, qword ptr fs:[R]` or the store of `set`
   ([sys-tls-set.md](sys-tls-set.md)), and no other instruction mentions
   `challoc_tls`: 68 load sites in 10 registers at 64d801a (rax, rbx,
   rcx, rdx, rsi, rdi, rbp, r8, r13, r14; a base of rbp or r13 takes a
   zero 8-bit displacement), one encoding per register, each checked.
4. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), for each register R the two instructions in
   sequence (`x86/tls-load.job.in`) from a symbolic state with the
   displacement a symbolic opcode field, under the relocation's
   definition (the displacement is the GOT entry `got` minus the next
   instruction's address, within 32 signed bits) and the residual facts
   that `got` is 8-byte aligned, the code and `got` are user addresses
   (below `2^47`), and the slot, the FS base (IA32_FS_BASE, the model's
   `msrs.1`) plus the offset `got` holds, is a user address and 8-byte
   aligned: exactly four combinations of traces are feasible, and on
   each the model raises no fault, reads the 8 bytes at `got` and then
   the 8 bytes at the slot, a byte at a time, R ends holding the slot's
   8 bytes (little-endian), nothing else is written (no other register,
   no flag, no memory), and the pair falls through (rip + 7, then the
   second instruction's length).

## 3. Translation

The contract has no `requires`, and its `ensures` says the result is
the token's value: Verus assumes that, and that the call changes
nothing it tracks (the token is shared, `opens_invariants none`). What
the block must do is return the slot's contents and honour its declared
interface: write only its outputs (`out`; on aarch64 also `off` and
`tp`), write no memory (`readonly`) and in particular not the stack
(`nostack`), leave the flags alone (`preserves_flags`), fall through,
and not fault. Facts 2 and 4 show that on each architecture.

- **`ensures r as usize == slot.value()`.** As in
  [sys-tls-get_tp.md](sys-tls-get_tp.md), part 3: the result is the 8
  bytes at the calling thread's `challoc_tls`; that those bytes are
  `slot.value()` is the ghost model's premise (verus/THEOREMS.md,
  theorem 15, premises; `src/sys/tls.rs`), with `set`'s contract
  ([sys-tls-set.md](sys-tls-set.md)): Isla shows which memory is read,
  not that the token equals it.
- **Residual assumptions, x86_64** (verus/ISLA.md, "The x86_64 mixed
  seams"): ELF TLS, initial-exec model (the x86-64 psABI and Drepper,
  "ELF Handling For Thread-Local Storage"): the linker resolves
  `challoc_tls@GOTTPOFF` to an 8-byte aligned GOT entry the loader fills
  with the slot's offset from the thread pointer, and the FS base is
  that thread pointer (the C library sets IA32_FS_BASE for each
  thread); the slot is 8-byte aligned (`.p2align 3` in tls.rs's
  `.tbss`, which the TLS layout honours); the processor is in 64-bit
  mode (by function assumption, as in
  [api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md),
  part 3); the application view (the GOT and the TLS block are mapped);
  user addresses below `2^47`. Residual assumptions on aarch64: as
  `get_tp`'s record.
- **Gaps.** The code read is the static libraries'; a static link of
  `libchalloc.a` into an executable lets the linker relax the
  initial-exec sequence to local-exec (on x86_64 a `mov R, imm32` of the
  offset in place of the GOT load; on aarch64 `movz`/`movk`), code this
  record does not cover (verus/ISLA.md, plan). The model and the
  checker, as for the x86_64 `syscall` records.

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
wording of the template fact, and the seam numbers at 64d801a. A human
review is pending.

## 5. Tests

`verus/isla/run.sh` fails when either `asm!` call changes, when the
ledger entry with this record's status has another contract, on
everything `get_tp`'s record lists for the aarch64 load sites, when an
x86_64 `GOTTPOFF` relocation or `challoc_tls` reference sits outside the
load and store shapes, a register has two encodings, or no load site
remains, when a `tls-load` job fails, and when a negative control
passes (on x86_64, the load without the relocation's definition).
