# `sys::pac::axiom_auth_sign`

The broadcast axiom `axiom_auth_sign` in `src/sys/pac.rs`
(`axiom_auth_sign` in `verus/seams.overrides`). The aarch64 half is
checked with Isla; the x86_64 half needs no tool (part 3).
Tool: Isla. Status: discharged, with the named residual assumptions of
`sign` and `auth` ([sys-pac-sign.md](sys-pac-sign.md),
[sys-pac-auth.md](sys-pac-auth.md), part 3: among them that the CPU's
pointer authentication is one of the two kinds checked, Armv8.3-v8.5
FEAT_PAuth or FEAT_PAuth2 with FEAT_FPAC as on the C4A reference
machine), and two of its own (part 3): the kernel's user VA is one of
the six sizes checked, and `user_va_limit()` is at most the VA width the
kernel configures, which rests on `axiom_user_va_limit` (S229, trusted).

## 1. Contract

Entry S220 of verus/SEAMS.md at 8635213 (`src/sys/pac.rs:175`, kind
axiom):

```rust
pub broadcast axiom fn axiom_auth_sign(p: usize, modifier: usize)
    requires
        p < user_va_limit(),
    ensures
        #[trigger] spec_auth(spec_sign(p, modifier), modifier) == p,
;
```

`spec_sign` and `spec_auth` are what `sign` and `auth` return (their
records); `user_va_limit()` is "the end of the user addresses the kernel
hands out to a process that asks for no more: `2^VA_BITS` of the
kernel, capped by its default map window", uninterpreted.

## 2. Statement

Isla (rems-project/isla e9b5d94) on the Sail Armv8.5-A and Armv9.4-A
models of `sign`'s record (part 2: `armv8p5.ir` and `armv9p4.ir.gz` from
isla-snapshots d8b3101), traces composed and checked by
`verus/isla/check.rs` with Z3 4.16.0; `verus/isla/run.sh`, section 6
(setup in verus/ISLA.md). In each of the 36 configurations of `sign`'s
record (part 2, fact 3: a kernel at EL1 or VHE at EL2, and Linux's
translation control for a user VA of 36, 39, 42, 47, 48 or 52 bits, in
the Armv8.5-A model and in the Armv9.4-A one with FEAT_PAuth2 and
FEAT_FPAC, with FEAT_CONSTPACFIELD off and on; the key, SCTLR with its
EnDA bit, the pointer and the modifier symbolic; `ComputePAC` an
uninterpreted function), the two instructions

```
pacda x0, x1
autda x0, x1
```

run in sequence (`pac-rt.job.in`) from any state in which x0 holds a
pointer `p` whose bits from VA_BITS up are zero (`p < 2^VA_BITS`): every
feasible path ends with x0 = `p` and falls through both instructions.
In the Armv9.4-A configurations `autda` is traced with its FPAC fault
(`--stop-at AArch64_PACFailException`) and the job takes it as
`fallthrough`, so every path on which it faults is shown infeasible:
the round trip never faults. That holds whether EnDA is set (both
instructions use the PAC) or clear (both are the identity). Without the
bound on `p` the job fails in both models (negative controls `pac-rt-va`
and `pac9-rt-va`): a pointer with a bit in the PAC field does not come
back (v8.5) or makes `autda` fault (v9.4).

No property of the PAC algorithm is used: both instructions apply the
same uninterpreted `ComputePAC` to the same arguments (the pointer with
its PAC field cleared to bit 55's copies, the modifier, the key). In the
Armv8.5-A model `pacda` writes the PAC into the cleared field and
`autda` compares it there; with FEAT_PAuth2 `pacda` XORs it into the
field and `autda` XORs it out again (`sign`'s fact 5, `auth`'s fact 4),
and FEAT_FPAC's check that the field is then all copies of bit 55
passes.

## 3. Translation

- **The aarch64 half.** By the `sign` and `auth` records, under their
  residual (the key, EnDA and the configuration fixed for the process),
  `spec_sign` is the function `pacda x0, x1` computes and `spec_auth`
  the one `autda x0, x1` computes, at every site. The sequence above
  composes exactly those two functions with the same modifier and the
  same key, so it shows `spec_auth(spec_sign(p, m), m) = p` for every
  `p < 2^VA_BITS`, where `VA_BITS` is the width the job's TCR sets
  (`64 - T0SZ`); with FEAT_FPAC, where `spec_auth` is the value of the
  `autda` that returns, it also shows that this `autda` returns.
  **`requires p < user_va_limit()`**: the job's bound
  needs `user_va_limit() <= 2^(64 - T0SZ)`, the residual **limit within
  the configured VA**. `user_va_limit`'s doc defines it as `2^VA_BITS`
  of the kernel capped by its default map window, and `VA_BITS` there is
  the build option, which can exceed what the kernel configures: a
  kernel built for 52 bits on a CPU without 52-bit VA support runs with
  a 48-bit TCR. What closes that case is `axiom_user_va_limit` (S229,
  trusted): the limit is at most `2^48`, at most the configured width in
  every configuration here except the 36-, 39-, 42- and 47-bit ones,
  where the build option is the configured width (Linux falls back
  only from 52 bits). The residual **user VA size**: the kernel's user
  VA is one of the six sizes, with the granule listed (Linux's arm64
  configurations except 52-bit VA with 4K or 16K pages, which needs
  FEAT_LPA2 and is not among the configurations).
- **The x86_64 half.** On x86_64 `spec_sign` and `spec_auth` have no
  meaning tied to execution: `sign` and `auth` require
  `pac_available()`, which no x86_64 process meets (their records), and
  no other trusted fact mentions the two functions. So the axiom only
  has to be consistent there, and it is: the identity, which is what the
  x86_64 bodies of `sign` and `auth` return, satisfies it. No tool is
  needed; this is a reading of the source (`grep -rn
  'spec_sign\|spec_auth' src` at 8635213: only `pac.rs`'s definitions,
  this axiom and verified code use them).
- **Gaps.** Those of `sign` and `auth`: `ComputePAC` taken to be a
  function of its arguments (read and spot-checked); one TCR value per
  VA size; the static library's code; the CPU kinds not checked
  (FEAT_EPAC alone, FEAT_PAuth2 without FEAT_FPAC).

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its findings on this record are folded in:
the precondition's bound shown to rest on axiom_user_va_limit (S229)
where the kernel's build option exceeds the configured VA, named in the
status line; FEAT_PAuth2 named as a residual; and those on sign's
record and run.sh apply here.

This change (the Armv9.4-A model, FEAT_PAuth2 and FEAT_FPAC) was
reviewed on 2026-10-04 by another fresh-context reviewer, told to look
for overclaiming translations and checker holes. Its findings are folded
in: under `pc-step` an instruction the model aborts as UNDEFINED (which
its footprint function swallows) looked like a fall-through, so each
instruction now names the register it writes when it completes
(control `pac9-udf`); an exception path could write any system register
(now an allow-list); a `stop` path could write memory and a `fault`
instruction need not be last (both enforced); the v9.4 CPU had features
of Armv8.6 and later on from the model's initialisers (now pinned off);
`HCR_HOST_NVHE_FLAGS` includes ATA, and C4A's kernel is a guest at EL1;
the serialized model was compared on one trace (now four). A human
review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the round-trip job fails in any
configuration of either model (and with it everything `sign`'s and
`auth`'s records list), and when a negative control without the VA
bound passes. `sh verus/seams.sh --check` fails when verus/SEAMS.md is
stale, so a changed axiom changes the ledger and then fails this
record's contract check. The unit test
`sys::pac::tests::sign_auth_round_trip` checks the round trip on an
aarch64 host with PAuth, and the identity on x86_64. No CI job runs
`run.sh` yet (verus/ISLA.md).
