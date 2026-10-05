# `api::cxx::get_new_handler_addr`

The `get_new_handler_addr` in `src/api/cxx.rs`, one item for both architectures
(`src/api/cxx.rs:get_new_handler_addr` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the loader's GOT entry and the
platform facts of part 3 as named residual assumptions: its aarch64 half
on the Armv8.5-A model and its x86_64 half on the x86-64 model.

The same shape and the same two jobs as
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md), for
the symbol `_ZSt15get_new_handlerv` (`std::get_new_handler()`).

## 1. Contract

Entry S1 of verus/SEAMS.md as numbered at f079fe76 (`src/api/cxx.rs:67`),
kind external_body, no `requires`, no `ensures`; `#[inline(never)]`:

```rust
fn get_new_handler_addr() -> *const c_void
```

The bodies Verus does not see, at f079fe76: on x86_64

```rust
asm!(
    ".weak _ZSt15get_new_handlerv",
    "mov {out}, qword ptr [rip + _ZSt15get_new_handlerv@GOTPCREL]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

and on aarch64

```rust
asm!(
    ".weak _ZSt15get_new_handlerv",
    "adrp {out}, :got:_ZSt15get_new_handlerv",
    "ldr {out}, [{out}, :got_lo12:_ZSt15get_new_handlerv]",
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 7
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above is, up to
   whitespace, an `asm!` call in `fn get_new_handler_addr`
   (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** The function compiles to exactly `adrp
   x0` with `R_AARCH64_ADR_GOT_PAGE _ZSt15get_new_handlerv`, `ldr x0,
   [x0]` with `R_AARCH64_LD64_GOT_LO12_NC _ZSt15get_new_handlerv`, and
   `ret`.
3. **Isla, aarch64.** `got-fn.job.in`, the one job for this function and
   the three others of the same shape (`throw_bad_alloc_addr`,
   `malloc_conf_addr`, `je_malloc_conf_addr`): on the Sail Armv8.5-A
   model (`armv8p5.ir`, isla-snapshots d8b3101), the three instructions
   in sequence, at Non-secure EL0 with the PC symbolic and the two
   relocated immediates symbolic opcode fields, under the relocations'
   definitions (ADRP's immediate `Page(got) - Page(P) >> 12` within 21
   signed bits, the LDR's `got[11:3]`, `got` the symbol's GOT entry) and
   the residual facts that `got` is 8-byte aligned and code and GOT lie
   below `2^48`: every feasible path reads memory once, at `got`,
   returns its contents in x0, writes no other register, no memory and
   no flag, and ends at the address in x30 (the return).
4. **Compiled code, x86_64.** The function compiles to exactly `mov
   rax, qword ptr [rip]` (`48 8b 05`) with `R_X86_64_GOTPCREL
   _ZSt15get_new_handlerv-0x4` on its displacement, and `ret` (`c3`).
5. **Isla, x86_64.** `x86/got-fn.job`, the one job for the four
   functions: on the Sail x86-64 model (sail-x86-from-acl2 0c6c023 with
   challoc's entry, `verus/isla/x86/`) in its application view, the
   processor in 64-bit mode (the function assumption of run.sh's section
   12), the two instructions in sequence from a symbolic state with the
   displacement a symbolic opcode field, under the relocation's
   definition (the displacement is `got` minus the next instruction's
   address, within 32 signed bits) and the residual facts that `got` is
   8-byte aligned, code, GOT and stack are user addresses (below
   `2^47`), the stack pointer is 8-byte aligned and the return address
   on top of the stack is a user address: of the 741 trace combinations
   exactly four are feasible, and on each the model raises no fault,
   reads memory 16 times, a byte at a time, the 8 bytes at `got` and
   then the 8 at rsp; rax ends holding the first 8 (little-endian), rip
   the second 8 and rsp is 8 higher; nothing else is written: no other
   register, no flag (`rflags`), no memory.

The jobs name no symbol: the relocated fields are symbolic and `got` is
any address meeting the residual facts, so facts 3 and 5 are those of
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md)
part 2, and facts 2 and 4 tie them to this function's own code and
relocations.

## 3. Translation

The contract has no `ensures`: Verus assumes only that the call returns
some pointer and changes nothing it tracks. Facts 3 and 5 are the
declared interface and more, on both architectures: only `out` written
(x0 or rax, the result register), `readonly` and `nostack` (no memory
write), `preserves_flags`, and a return to the caller (x30, or the
address the caller's `call` pushed, with rsp popped past it, the
x86-64 psABI's return). The value returned is the GOT entry's, which
the loader fills with the symbol's address (or 0 for an unresolved weak
symbol: the doc's "or null when no libstdc++ is loaded"), a residual
(System V gABI; the AArch64 ELF ABI and the x86-64 psABI for the
relocations) nothing in the contract relies on. That a non-null value
is `std::get_new_handler`'s address is what `call_get_new_handler`
(S5, trusted) relies on; it is the loader's fact and outside this
contract.

Residual assumptions and gaps: those of
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md)
part 3, unchanged. aarch64: the model's MMU is off and the code read is
the static library's, and the configuration has TCR_EL1 = 0, so `ret`
uses all 64 bits of x30. x86_64: the processor is in 64-bit mode (the
model's `n64_bit_modep` answers true by assumption); the application
view, in which linear addresses are the memory's; user addresses below
`2^47`; the return address is the caller's (the psABI); a machine
translation of ACL2 x86isa with no x87, MXCSR, mask or CET state, and
`check.rs` in the trusted base. The code read is the static library's
(fat LTO, one codegen unit); the function being `#[inline(never)]`, the
shared library has it as one function too.

## 4. Review

Written on 2026-10-05 (the seam was the only inline-asm seam without a
record), by adding the function to the shape checks of run.sh's
sections 7 and 12 and its two templates to `templates.txt`; the jobs
and their negative controls are unchanged. Checked then on the
libraries built from this tree with rustc 1.95.0 (run.sh's default
toolchain): the templates, the record's ledger entry, the compiled
shape on both architectures, and `got-fn.job` and `x86/got-fn.job`
(run.sh with its pointer authentication section and that section's
negative controls left out: the Armv9.4-A model was not on the machine;
they check other seams). Reviewed then by a
fresh-context reviewer (a Claude subagent with no prior context of this
work), told to look for overclaiming translations; its findings are
folded in. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when either `asm!` call changes, when the
ledger entry with this record's status has another contract, when the
compiled function is not exactly its instructions with their
relocations on either architecture, when `got-fn.job` (aarch64) or
`x86/got-fn.job` fails, and when a negative control of those jobs
passes (the load one GOT entry off and `ret` claimed to fall through, on
both architectures; on x86_64 also the job without its 64-bit mode
assumption named).
