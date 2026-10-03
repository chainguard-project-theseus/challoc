# `rseq::symbols` (aarch64 half)

Evidence note, not a discharge: entry S199 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla below; its x86_64 half
(three `mov reg, qword ptr [rip + sym@GOTPCREL]`) is not, as there is no
x86_64 Sail model for Isla (verus/ISLA.md, plan item 1). When it is, this
note becomes the seam's record.

## 1. Contract

Entry S199 of verus/SEAMS.md at 8635213 (`src/rseq.rs:162`, kind
external_body, no `requires`, no `ensures`; `#[inline(never)]`):

```rust
fn symbols() -> (*const isize, *const u32, *const u32)
```

The aarch64 body Verus does not see, at 8635213: `.weak` directives for
the three symbols, then

```rust
asm!(
    "adrp {so}, :got:__rseq_offset",
    "ldr {so}, [{so}, :got_lo12:__rseq_offset]",
    "adrp {ss}, :got:__rseq_size",
    "ldr {ss}, [{ss}, :got_lo12:__rseq_size]",
    "adrp {sf}, :got:__rseq_flags",
    "ldr {sf}, [{sf}, :got_lo12:__rseq_flags]",
    so = out(reg) so,
    ss = out(reg) ss,
    sf = out(reg) sf,
    options(nostack, readonly, preserves_flags),
);
```

and the tuple `(so, ss, sf)` returned.

## 2. Statement (aarch64)

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A model
(`armv8p5.ir`, isla-snapshots d8b3101), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, section 7.

1. **Template.** The aarch64 `asm!` call in `fn symbols` is the one
   above (with its `.weak` lines), up to whitespace
   (`verus/isla/templates.txt`).
2. **Compiled code.** The function compiles to exactly: the three GOT
   pairs into x8, x9 and x10, each `adrp` with `R_AARCH64_ADR_GOT_PAGE`
   and each `ldr` with `R_AARCH64_LD64_GOT_LO12_NC` of its symbol, then
   `stp x8, x9, [x0]`, `str x10, [x0, #16]` and `ret`: the compiler
   returns the 24-byte tuple through a pointer the caller passes in x0.
3. **Isla.** The nine instructions in sequence (`rseq-symbols.job.in`;
   856830 combinations of traces, all but two cut at an infeasible
   prefix), under the relocations' definitions for each pair (P the
   `adrp`'s own address) and the residual facts that the GOT entries are
   8-byte aligned, code, GOT entries and the return area lie below
   `2^48` and the return area is 8-byte aligned: every feasible path
   reads memory three times, at the three GOT entries in order, writes
   memory three times, the three values read to the return area's words
   at x0, x0 + 8 and x0 + 16, writes no register but x8-x10 and no flag,
   and ends at the address in x30.

## 3. Translation (aarch64 half)

The contract has no `ensures`. Fact 3 shows the asm block's interface
(only its three outputs written, no memory written, no flags) and the
compiler's code around it: the result stored where the caller asked and
nothing else written, x8-x10 being caller-saved scratch registers under
AAPCS64. The residuals: the loader fills each GOT entry with the
symbol's address or 0 (weak, unresolved), and the return-area convention
(x0, 8-byte aligned) is the Rust ABI the compiler chose for this call,
which caller and callee share. Gaps as in
[sys-tls-get_tp.md](sys-tls-get_tp.md) (MMU off, the static library's
code) and in [api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md)
(`ret` with TBI off).

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its finding on these jobs is folded in:
`fallthrough` now proves that each instruction continues at its own PC
+ 4, which the relocation assumptions (P the ADRP's own address) and
the final `ret` rely on. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the aarch64 `asm!` call changes, when the
compiled function is not exactly the nine instructions with their
relocations, when `rseq-symbols.job` fails, and when its negative control
(the second value stored as the first) passes.
