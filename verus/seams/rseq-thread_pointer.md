# `rseq::thread_pointer`

The `thread_pointer` in `src/rseq.rs`, one item for both architectures
(`src/rseq.rs:thread_pointer` in `verus/seams.overrides`).
Tool: Isla. Status: discharged, with the thread pointer's setup and the
platform facts of part 3 as named residual assumptions: its aarch64 half
on the Armv8.5-A model and its x86_64 half on the x86-64 model.

## 1. Contract

Entry S200 of verus/SEAMS.md as numbered at 8635213 (S203 and
`src/rseq.rs:220` at 64d801a), kind external_body, no `requires`, no
`ensures`; `#[inline(always)]`:

```rust
pub fn thread_pointer() -> usize
```

The bodies Verus does not see, at 64d801a: on x86_64

```rust
asm!("mov {tp}, qword ptr fs:[0]", tp = out(reg) tp, options(nostack, readonly, preserves_flags));
```

and on aarch64

```rust
asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
```

## 2. Statement

Isla (rems-project/isla e9b5d94), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, sections 7
(aarch64) and 12 (x86_64); setup in verus/ISLA.md.

1. **Templates.** Each of the two `asm!` calls above is, up to
   whitespace, an `asm!` call in `fn thread_pointer`
   (`verus/isla/templates.txt`).
2. **Compiled code, aarch64.** Every `mrs` of TPIDR_EL0 in the aarch64
   release build (this function's inlined sites and the TLS sequences'
   third instruction: 76 at 8635213) writes x8, x9, x10 or x11, and each
   of the four is checked.
3. **Isla, aarch64.** `mrs xN, tpidr_el0` at Non-secure EL0 on the Sail
   Armv8.5-A model (`mrs-tp.job.in`, one job per register): one trace;
   it writes only xN, with TPIDR_EL0, reads and writes no memory, writes
   no flag and falls through.
4. **Compiled code, x86_64.** Every load from `%fs:0` in the x86_64
   release build (`mov R, qword ptr fs:[0]`, this function's inlined
   sites: one, into rax, at 64d801a) is checked, one job per register
   (one encoding each), and every other `%fs` access in the build is one
   of the TLS slot sites ([sys-tls-get.md](sys-tls-get.md)).
5. **Isla, x86_64.** On the Sail x86-64 model (sail-x86-from-acl2
   0c6c023 with challoc's entry, `verus/isla/x86/`) in its application
   view, the processor in 64-bit mode (the function assumption of
   run.sh's section 12), `mov R, qword ptr fs:[0]` from a symbolic state
   (`x86/fs0.job.in`), under the residual facts that the code and the FS
   base are user addresses (below `2^47`) and the FS base is 8-byte
   aligned: four of its 44 traces are feasible, and on each the model
   raises no fault, reads the 8 bytes at the FS base (IA32_FS_BASE, the
   model's `msrs.1`) into R (little-endian), writes no other register,
   no flag and no memory, and falls through (rip + 9).

## 3. Translation

The contract has no `ensures`: nothing is claimed about the value.
Facts 3 and 5 are the declared interface (one output; `nomem` on
aarch64, `readonly` on x86_64, where the one read is the TCB word;
`nostack`, `preserves_flags`) and the doc's claim of what the value is:
TPIDR_EL0 on aarch64, the 8 bytes at `%fs:0` on x86_64. That these hold
the thread pointer the C library set up (TPIDR_EL0; on x86_64 the TCB's
self pointer at offset 0 of the TCB, the x86-64 TLS ABI; the rseq area
is at it plus `__rseq_offset`) is the residual, as in
[sys-tls-get_tp.md](sys-tls-get_tp.md). Gaps: the code read is the
static library's, and an inlined site in the shared library may use
another register; on x86_64 as in
[api-cxx-throw_bad_alloc_addr.md](api-cxx-throw_bad_alloc_addr.md),
part 3 (64-bit mode by assumption, the application view, user
addresses, the model and the checker).

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
ledger entry with this record's status has another contract, when an
`mrs` of TPIDR_EL0 writes a register no job covers or none exists, when
a load of `%fs:0` is missing, has two encodings for one register, or an
`%fs` access is neither such a load nor a TLS site, when an `mrs-tp` or
`fs0` job fails, and when a negative control passes (the `mrs` claimed
to write nothing; the x86_64 load claimed to read at the GS base).
