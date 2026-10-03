# `rseq::thread_pointer` (aarch64 half)

Evidence note, not a discharge: entry S200 of verus/SEAMS.md stays
trusted. Its aarch64 half is checked with Isla below; its x86_64 half
(`mov tp, qword ptr fs:[0]`) is not, as there is no x86_64 Sail model
for Isla (verus/ISLA.md, plan item 1). When it is, this note becomes the
seam's record.

## 1. Contract

Entry S200 of verus/SEAMS.md at 8635213 (`src/rseq.rs:205`, kind
external_body, no `requires`, no `ensures`; `#[inline(always)]`):

```rust
pub fn thread_pointer() -> usize
```

The aarch64 body Verus does not see, at 8635213:

```rust
asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
```

## 2. Statement (aarch64)

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A model, checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, section 7.

1. **Template.** The aarch64 `asm!` call in `fn thread_pointer` is the
   one above (`verus/isla/templates.txt`).
2. **Compiled code.** Every `mrs` of TPIDR_EL0 in the aarch64 release
   build (this function's inlined sites and the TLS sequences' third
   instruction: 76 at 8635213) writes x8, x9, x10 or x11, and each of
   the four is checked.
3. **Isla.** `mrs xN, tpidr_el0` at Non-secure EL0 (`mrs-tp.job.in`, one
   job per register): one trace; it writes only xN, with TPIDR_EL0,
   reads and writes no memory, writes no flag and falls through.

## 3. Translation (aarch64 half)

The contract has no `ensures`: nothing is claimed about the value. Fact
3 is the declared interface (`nomem`, `nostack`, `preserves_flags`, one
output) and the doc's claim that the value is TPIDR_EL0. That TPIDR_EL0
holds the thread pointer the C library set (the rseq area is at it plus
`__rseq_offset`) is the residual, as in
[sys-tls-get_tp.md](sys-tls-get_tp.md). Gap: the code read is the static
library's; an inlined site in the shared library may use another
register.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. It found nothing specific to this note. A
human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the aarch64 `asm!` call changes, when an
`mrs` of TPIDR_EL0 in the build writes a register no job covers or none
exists, when an `mrs-tp` job fails, and when its negative control (the
`mrs` claimed to write nothing) passes.
