# `sys::tls::get_tp` (aarch64)

The aarch64 `get_tp` in `src/sys/tls.rs` (`src/sys/tls.rs:get_tp` in
`verus/seams.overrides`; the x86_64 `get_tp` is verified and is not a
seam). Tool: Isla. Status: discharged, with the ELF TLS and loader facts
as named residual assumptions, and with its `ensures` (the value read is
the token's) resting on the ghost slot model's premise, which `set`
(S256, discharged by Isla, sys-tls-set.md) also carries: Isla shows which memory the read
reads, not that the token equals it (part 3).

## 1. Contract

Entry S242 of verus/SEAMS.md at 8635213 (`src/sys/tls.rs:182`; S263
since package P7, READY+HEAPPERM, which added the second `ensures`), kind
external_body, no `requires`, two `ensures`):

```rust
pub fn get_tp(Tracked(slot): Tracked<&TlsSlot>) -> (r: (*mut c_void, usize))
    ensures
        r.0 as usize == slot.value(),
        r.1 == slot.tp(),
    opens_invariants none
```

`TlsSlot` (`src/sys/tls.rs`) is the calling thread's slot as ghost
state: `value()` is what the slot holds (verus/THEOREMS.md, theorem 15,
"Re-entrant heap registration"). The body Verus does not see, at
8635213 (unchanged since d865271, where this record was first written
against a contract with no `ensures`):

```rust
asm!(
    "adrp {off}, :gottprel:challoc_tls",
    "ldr {off}, [{off}, :gottprel_lo12:challoc_tls]",
    "mrs {tp}, tpidr_el0",
    "ldr {out}, [{tp}, {off}]",
    off = out(reg) _,
    tp = out(reg) tp,
    out = out(reg) out,
    options(nostack, readonly, preserves_flags),
);
```

`challoc_tls` is the 8-byte `.tbss` object `tls.rs` defines with
`global_asm!` (`.p2align 3`, hidden).

## 2. Statement

Isla (rems-project/isla e9b5d94, `isla-footprint -s`) on the Sail
Armv8.5-A model (`armv8p5.ir`, isla-snapshots d8b3101), traces composed
and checked by `verus/isla/check.rs` with Z3 4.16.0. Reproduce with
`verus/isla/run.sh` (setup in verus/ISLA.md). Three facts:

1. **Template.** The `asm!` call in `fn get_tp` is the one above, up to
   whitespace (`verus/isla/templates.txt`), and the ledger entry the
   status names is the one under `#[cfg(target_arch = "aarch64")]`.
2. **Compiled code.** In the aarch64 release build every relocation
   against `challoc_tls` belongs to one of two shapes: the load
   `adrp A; ldr A, [A]; mrs B, tpidr_el0; ldr C, [B, A]`, with
   `R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21` on the `adrp` and
   `R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC` on the first `ldr` (`get` and
   `get_tp`, which compile to the same four instructions: 70 sites, 12
   register assignments at 8635213), or the same with a final `str`
   (`set`, 5 sites, checked as evidence for `set`, not here:
   [sys-tls-set.md](sys-tls-set.md)). A, B and C are distinct.
3. **Isla.** For each of the 12 assignments, the four instructions run
   in sequence at Non-secure EL0 in AArch64 state with the PC symbolic
   and the two relocated immediates symbolic opcode fields
   (`verus/isla/tls-load.job.in`; one job per assignment). The model
   gives 1, 13, 1 and 13 traces; of the 169 combinations exactly one is
   feasible under the assumptions below, and in it:
   - the first load reads the 8 bytes at `got`, the GOT entry the two
     relocations name; the second reads the 8 bytes at
     `TPIDR_EL0 + off`, `off` being the value the first one read;
   - afterwards C holds the second value, B holds `TPIDR_EL0`, A holds
     `off`, and the PC is the first instruction's plus 16 (no branch);
   - no register besides A, B, C and the PC is written, no memory is
     written, and PSTATE.{N,Z,C,V} are not written.

   Every other combination (a data abort in either load) is infeasible.
   The assumptions, from the AArch64 ELF ABI's definitions of the two
   relocations: the `adrp` immediate is `(Page(got) - Page(P)) >> 12` for
   `P` its own address, within the 21 signed bits the linker checks; the
   `ldr` immediate is `got[11:3]`. And residual facts: `got` is 8-byte
   aligned; `P`, `got` and `TPIDR_EL0 + off` are below `2^48`, and
   `TPIDR_EL0 + off` is 8-byte aligned.

## 3. Translation

The contract has no `requires`, and its `ensures` says the first
component is the token's value: Verus assumes that, and that the call
changes nothing it tracks (the token is shared, `opens_invariants
none`). What the block must do is return the slot's contents as the
first component and honour its declared interface: write only its three outputs
(`off`, `tp`, `out`), write no memory (`readonly`) and in particular
not the stack (`nostack`; the two options are independent, and the
memory check discharges both), leave the condition flags alone
(`preserves_flags`), fall through to the next instruction, and not
fault.

- **No `requires`.** Isla ran from EL0 in AArch64 state, Non-secure,
  with the PC symbolic, PSTATE.{EL,nRW} and SCR_EL3 = 0x401 pinned,
  HCR_EL2 clear and the configuration's defaults for the rest (the MMU
  off among them; see the gaps), under the assumptions in part 2. The
  instructions behave the same at EL0 whether the kernel runs at EL1 or
  at EL2, which is read from the architecture, not checked. The
  assumptions are the trusted part:
  - *ELF TLS, initial-exec model* (AArch64 ELF ABI, "Thread-local
    storage"; Drepper, "ELF Handling For Thread-Local Storage"): the
    linker resolves the relocation pair to an 8-byte aligned GOT entry,
    and the loader fills it with `challoc_tls`'s offset from the thread
    pointer in the static TLS block. The library is loaded at startup
    (initial-exec TLS needs that), so the block exists in every thread.
  - *Linux and glibc*: TPIDR_EL0 holds the thread pointer the C library
    set for the calling thread; the code, the GOT entry and the slot are
    mapped and readable user addresses below `2^48` (Linux's default map
    window, as `axiom_user_va_limit` assumes for `sys::pac`); the slot is
    8-byte aligned because its section is (`.p2align 3`) and the TLS
    layout honours section alignment.
- **`ensures r.0 as usize == slot.value()`.** Fact 3: `out`, returned
  as `r.0`, is the 8 bytes at the calling thread's `challoc_tls` (the
  pointer-to-`usize` cast is the bits as they are). That those bytes are
  `slot.value()` is assumed, not shown: it is the ghost model's premise,
  which no trace of machine code can establish. In Verus,
  `TlsSlot::entry` (an axiom since package P7) returns a token whose
  value is unconstrained but for what `heap::thread::slot_ok` says of
  the ghost state that goes with it, so nothing in the verified code
  ties it to the slot; the premise is that a token stands for the calling
  thread's slot, that `set` (S256, discharged by Isla) makes the written value the
  token's, and that no token is read after the slot changed other than
  through it (verus/THEOREMS.md, theorem 15, premises; `src/sys/tls.rs`).
  So the `ensures` is this premise plus `set`'s trusted contract; what
  this record adds is only that the read really is of the calling
  thread's own `challoc_tls` and of nothing else.
- **`ensures r.1 == slot.tp()`.** Fact 3: `tp`, returned as `r.1`, is
  B, which holds TPIDR_EL0, the calling thread's thread pointer. That
  this is the token's `tp()` is again the ghost model's premise:
  `TlsSlot::entry` (S253, trusted) makes the calling thread's token,
  recording its thread pointer, which the kernel keeps fixed for the
  thread's life (glibc sets TPIDR_EL0 once, at thread creation), and no
  accessor changes it (`set`'s contract keeps `tp()`). Since package
  P7 Verus relies on it: a signed slot value authenticates, with this
  modifier, to the heap whose permission the token holds
  (`heap::thread::slot_ok`, `heap_from_slot`).
- **The interface.** Fact 3's footprint is exactly the declared one:
  writes to A, B, C and the PC only, no memory write, no flag write, a
  fall-through PC. That the compiler binds `off`, `tp` and `out` to the
  distinct registers A, B and C of each site, and reads `tp` and `out`
  from them, is the compiler's inline-assembly semantics, trusted as for
  every Verus result; fact 2 checks the result on every site.
- **Gaps.** The model runs with the MMU off: each access goes to the
  physical address equal to its virtual address and is treated as Device
  memory, where an unaligned access faults (hence the alignment fact for
  the slot; Linux's Normal memory would not fault). That the MMU maps
  the two addresses to the GOT entry and the slot is the loader and
  kernel residual above. The model is the ASL-derived Armv8.5-A snapshot;
  Isla runs from a symbolic state with no interrupts, debug events or
  concurrent writers (the slot is the calling thread's own).
- **Which code.** Fact 2 reads the aarch64 static library built with the
  release profile (fat LTO, one codegen unit) from the same sources, so
  it covers the code before the final link. Two links change it. The
  shipped shared library is a separate LTO run and could allocate other
  registers: a site there with an assignment outside the 12 is not
  covered until `run.sh` reads the linked library. And a static link of
  `libchalloc.a` into an executable lets the linker relax the
  initial-exec sequence to local-exec (`movz`/`movk` of the offset with
  `R_AARCH64_TLSLE_MOVW_TPREL_G1`/`G0_NC` in place of the GOT load), code
  this record does not cover at all; the relaxed shape needs its own job
  (verus/ISLA.md, plan). The record covers the shared library's form,
  where the GOT load stays.

## 4. Review

Translation reviewed on 2026-10-02 by a fresh-context reviewer (a Claude
subagent with no prior context of this work). Its findings on this
record are folded in: the static link's relaxation to local-exec named
as code the record does not cover, the precondition's pinned values
listed, the `readonly`/`nostack` wording corrected, every
`challoc_tls` relocation (an orphan `LO12_NC` included) required to sit
in a checked shape, the architecture of the ledger entry checked, and
two soundness holes in `check.rs` (an inexpressible write could be read
back as the initial value; writes through a field accessor were not
checked) fixed, with negative controls for both. Updated on 2026-10-03
to the contract at 8635213 (the `TlsSlot` token and its `ensures`,
added after d865271), and
reviewed again that day by a fresh-context reviewer (a Claude subagent
with no prior context of this work), told to look for translations that
overclaim. Its finding on this record is folded in: the `ensures` is
stated as resting on the ghost slot premise and `set`'s trusted contract
(S256), both named in the status line, and `TlsSlot::entry`'s
`arbitrary()` value is described as what it is. Package P7
(READY+HEAPPERM) added `r.1 == slot.tp()`; its fresh-context review
read this record with the change. A human review is
pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (fact 1), when
the ledger entry with this record's status has another contract or is
not the aarch64 item (fact 1), when a `challoc_tls` relocation in the
aarch64 build sits outside the load and store shapes or no load site
remains (fact 2), when any job for a load site fails (fact 3), and when
a negative control passes (among them this job with the GOT address off
by one entry, without the `ldr` relocation's constraint, and without
the slot's alignment). `sh verus/seams.sh --check` fails when
verus/SEAMS.md is stale. No CI job runs `run.sh` yet: it needs the Isla
toolchain (verus/ISLA.md).
