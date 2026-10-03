# `config::linux::je_malloc_conf_addr` (aarch64 half)

Evidence note, not a discharge: entry S104 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla below; its x86_64 half
(`mov out, qword ptr [rip + je_malloc_conf@GOTPCREL]`) is not, as there is no
x86_64 Sail model for Isla (verus/ISLA.md, plan item 1). When it is, this
note becomes the seam's record (SEAMS.md, "Recording a non-Verus
result").

## 1. Contract

Entry S104 of verus/SEAMS.md at 8635213 (`src/config.rs:1497`, kind external_body, no
`requires`, no `ensures`; `#[inline(never)]`):

```rust
fn je_malloc_conf_addr() -> *const c_void
```

The aarch64 body Verus does not see, at 8635213:

```rust
asm!(
    ".weak je_malloc_conf",
    "adrp {out}, :got:je_malloc_conf",
    "ldr {out}, [{out}, :got_lo12:je_malloc_conf]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

## 2. Statement (aarch64)

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A model
(`armv8p5.ir`, isla-snapshots d8b3101), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, section 7
(setup in verus/ISLA.md).

1. **Template.** The aarch64 `asm!` call in `fn je_malloc_conf_addr` is the one above,
   up to whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** The function compiles to exactly `adrp x0` with
   `R_AARCH64_ADR_GOT_PAGE je_malloc_conf`, `ldr x0, [x0]` with
   `R_AARCH64_LD64_GOT_LO12_NC je_malloc_conf`, and `ret`.
3. **Isla.** The three instructions in sequence (`got-fn.job.in`, one job
   for this function and the two others of the same shape), at
   Non-secure EL0 with the PC symbolic and the two relocated immediates
   symbolic opcode fields, under the relocations' definitions (ADRP's
   immediate `Page(got) - Page(P) >> 12` within 21 signed bits, the
   LDR's `got[11:3]`, `got` the symbol's GOT entry) and the residual
   facts that `got` is 8-byte aligned and code and GOT lie below `2^48`:
   every feasible path reads memory once, at `got`, returns its contents
   in x0, writes no other register, no memory and no flag, and ends at
   the address in x30 (the return).

## 3. Translation (aarch64 half)

The contract has no `ensures`: Verus assumes only that the call returns
some pointer and changes nothing it tracks. Fact 3 is the declared
interface and more: only `out` written (x0, the result register),
`readonly` and `nostack` (no memory write), `preserves_flags`, and a
return to the caller. The value returned is the GOT entry's, which the
loader fills with the symbol's address (the program's definition
interposing; 0 for an unresolved weak symbol: the doc's "or null"), a
residual (System V gABI, AArch64 ELF ABI) nothing in the contract relies
on. Gaps as in [sys-tls-get_tp.md](sys-tls-get_tp.md): the model's MMU
is off, the code read is the static library's, and the configuration
has TCR_EL1 = 0, so `ret` uses all 64 bits of x30 (under Linux's TBI0 it
would ignore a tag in the top byte, which return addresses do not
carry).

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its finding on these jobs is folded in:
`fallthrough` now proves that each instruction continues at its own PC
+ 4, which the relocation assumptions (P the ADRP's own address) and
the final `ret` rely on. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the aarch64 `asm!` call changes, when the
compiled function is not exactly the three instructions with the two
relocations, when `got-fn.job` fails, and when its negative controls
(the load one GOT entry off; `ret` claimed to fall through) pass.
