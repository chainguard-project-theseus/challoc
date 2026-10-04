# `sys::pac::auth`

`auth` in `src/sys/pac.rs` (`src/sys/pac.rs:auth` in
`verus/seams.overrides`), one item for both architectures.
Tool: Isla. Status: discharged, with the same named residual assumptions
as `sign` ([sys-pac-sign.md](sys-pac-sign.md), part 3): the fixed key and
configuration, the trap settings, `ComputePAC` a function of its
arguments, and the CPU's pointer authentication one of the two kinds
checked (Armv8.3-v8.5 FEAT_PAuth, or FEAT_PAuth2 with FEAT_FPAC as on
the C4A reference machine); with FEAT_FPAC, also Linux's handling of the
FPAC exception (part 3). The aarch64 body is `autda`; the x86_64 body is
plain Rust and is never reached.

## 1. Contract

Entry S225 of verus/SEAMS.md at 8635213 (`src/sys/pac.rs:226`, kind
external_body):

```rust
pub fn auth(p: usize, modifier: usize) -> (r: usize)
    requires
        pac_available(),
    ensures
        r == spec_auth(p, modifier),
    opens_invariants none
```

`spec_auth` is uninterpreted: "what `auth` returns for `p` and
`modifier`". The body Verus does not see, at 8635213:

```rust
#[cfg(target_arch = "aarch64")]
{
    let mut v = p;
    core::arch::asm!(
        ".arch_extension pauth",
        "autda {v}, {m}",
        v = inout(reg) v,
        m = in(reg) modifier,
        options(nomem, nostack, preserves_flags),
    );
    v
}
#[cfg(not(target_arch = "aarch64"))]
{
    let _ = modifier;
    p
}
```

(plus a test-only counter, compiled out of the library).

## 2. Statement

As for `sign` ([sys-pac-sign.md](sys-pac-sign.md), part 2: the same
tools and models, `run.sh` section 6, the same 36 configurations and the
abstract `ComputePAC`), for `autda`:

1. **Template.** The `asm!` call in `fn auth` is the one above, up to
   whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** As in `sign`'s fact 2: 75 `autda Xd, Xn` sites at
   8635213, in 17 register assignments, d and n distinct and neither 31.
3. **Isla, Armv8.5-A** (the twelve configurations without FEAT_PAuth2).
   For every `autda` assignment at a site: every path falls through,
   writes only Xd (with the PC and the model's bookkeeping), reads and
   writes no memory and writes no flag (`pac-footprint.job.in`); and
   against the canonical `autda x0, x1` from an independent state, the
   same pointer, modifier, data key A and EnDA bit give the same result
   (`pac-det.job.in`; the negative control `pac-det-enda` runs on
   `autda`). Both the paths where the PAC matches and those where it
   does not are among the traces, and neither faults: for a user
   pointer `p` (bit 55 clear) with EnDA set, the result is `p` with its
   PAC field (bits 54:B, B = 64 - T0SZ) cleared when the field equals
   ComputePAC's of that cleared pointer, and otherwise the cleared
   pointer with bits 54:53 set to the error code 0b01
   (`pac-spec-auth85.job.in`; `pac-spec-ec` claims 0b00 and fails); with
   EnDA clear it is `p` (`pac-enda-off.job.in`).
4. **Isla, Armv9.4-A with FEAT_PAuth2 and FEAT_FPAC** (the 24 `p2-*` and
   `p2c-*` configurations; `autda` traced with `--stop-at
   AArch64_PACFailException`, so a path that fails authentication ends
   at the call of the model's FPAC exception function, the `stop` kind
   of `check.rs`). For every `autda` assignment at a site:
   - every path either falls through, writing only Xd, or ends at
     `AArch64_PACFailException(0b10)` (syndrome: a data key, key A)
     having written no register (other than the model's bookkeeping)
     and no memory; neither reads memory or writes a flag
     (`pac-footprint.job.in` with the `stop` line); both kinds of path
     are feasible (a job with a `stop` line needs one that stops;
     `pac9-nofault`, `pac9-nostop` and `pac9-stop-key` fail);
   - against the canonical `autda x0, x1` from an independent state, the
     same pointer, modifier, data key A and EnDA bit give the same
     outcome: both fault or neither, and when neither does, the same
     result (`pac-det.job.in` with `assert (= a.stop.1 b.stop.1)`;
     `pac9-det-mod-auth`, `pac9-det-enda` fail).
   What it computes (`pac-spec-auth94.job.in`): for a user pointer `p`
   with EnDA set, with `O` = `p` with its PAC field cleared to copies of
   bit 55 and `X` = `p`'s bits 54:B XOR those of ComputePAC(O, modifier,
   key), it faults exactly when `X` is not all copies of bit 55, and
   otherwise returns `O`; with EnDA clear it returns `p` and never
   faults (`pac-enda-off.job.in`, `autda` as `fallthrough`).
5. **Isla, the FPAC exception** (`pac-fault.job.in`, in each `p2-*` and
   `p2c-*` configuration, on `autda x0, x1` traced without `--stop-at`:
   51 paths with the kernel at EL1, 147 under VHE): every path falls
   through writing only x0, or takes an exception that writes no user
   register (x0 included), no memory and no system register but the
   entry's own (ESR, ELR, SPSR, FAR, PFAR, HPFAR, PSTATE), with ESR_ELx
   = 0x72000002 (EC 0x1C, FPAC; IL; ISS 0b10), ELR_ELx = the `autda`'s
   own address, PSTATE at EL1 (EL2 under VHE) on SP_ELx, and the PC at
   that EL's vector for a synchronous exception from a lower EL using
   AArch64 (VBAR_ELx + 0x400, bits 55:0); the controls `pac9-fault-esr`,
   `pac9-fault-elr` and `pac9-fault-el` fail. The exception entry (the
   model's `AArch64_PACFailException(syndrome)` and the
   `AArch64_TakeException` it calls) depends on the syndrome and on
   system state, not on which registers the `autda` names, so the entry
   at a site is this one: read from the source, and seen in these traces,
   which read no general register but the operands x0 and x1, once each
   (not checked by a job). At the sites, fact 4 checks the path up to the
   call.

## 3. Translation

- **`requires pac_available()`.** As for `sign`: FEAT_PAuth on aarch64,
  with the trap-settings residual; no x86_64 process meets it, so the
  x86_64 body (which returns `p`) is unreachable from verified code.
- **`ensures r == spec_auth(p, modifier)`.** As for `sign`: facts 3 and
  4 show every site computes `G(p, modifier, K, E)` for the one `G` that
  `autda x0, x1` computes in the configuration, and under the residual
  that the key `K`, the EnDA bit `E` and the configuration are fixed
  for the process, `spec_auth(p, m) = G(p, m, K, E)`. On Armv8.3-v8.5
  that covers a failed authentication too: the value with the error code
  is `G`'s value for that pair. With FEAT_FPAC (fact 4) `G` is partial:
  where authentication fails the instruction does not complete but
  takes the FPAC exception (fact 5) with nothing written, and whether it
  does is itself a function of the same inputs. A call that does not
  return makes no claim about a result, so the `ensures` holds of every
  call that returns; `spec_auth` is uninterpreted, and its values where
  `G` faults are never observed.
- **The FPAC exception.** The residual **FPAC handling**: Linux handles
  the exception from EL0 (`do_el0_fpac`) by sending `SIGILL` to the
  thread, so the process ends unless a handler intervenes; a handler that
  returns without changing the registers re-runs the `autda` (fact 5:
  the return address is the instruction itself), which faults again. The
  module doc of `src/sys/pac.rs` and docs/design.md already describe
  this as a crash at the `autda`.
- **What FEAT_PAuth2 and FEAT_FPAC change** (facts 3 and 4): a failed
  authentication faults at the `autda` instead of returning a pointer
  with an error code that faults at its first use; a successful one
  returns the same value in both models (the pointer with its PAC field
  cleared). The authentication itself XORs the PAC out of the field
  (PAuth2) instead of comparing it, which is how the round trip of
  `axiom_auth_sign` holds in both models.
- **The interface.** Only `v` written, no memory, no flags, a
  fall-through when it returns: the declared `nomem`, `nostack`,
  `preserves_flags`. The block is not `pure`, so the compiler keeps
  every call (and with FEAT_FPAC, every fault); the result being a
  function of the operands would allow `pure` too.
- **`opens_invariants none`.** No ghost code.
- **Gaps.** As for `sign`: one TCR value per VA size, the static
  library's code, the CPU kinds not checked (FEAT_EPAC alone, FEAT_PAuth2
  without FEAT_FPAC), the formulas stated for user pointers only. The
  FPAC exception entry is checked on the canonical `autda x0, x1`; at the
  sites, the path up to the call of `AArch64_PACFailException` is.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its findings on this record are folded in:
FEAT_FPAC and FEAT_PAuth2 named as a residual in the status line; and
those on sign's record and run.sh apply here.

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

`verus/isla/run.sh` fails when the `asm!` call changes, when the ledger
entry with this record's status has another contract, when the build
emits a PAC-family instruction outside the three shapes or no `autda`,
when any `autda` footprint, determinism, formula or FPAC exception job
fails in any configuration, and when a negative control passes. `sh
verus/seams.sh --check` fails when verus/SEAMS.md is stale. The unit tests
`sys::pac::tests::sign_auth_round_trip` and `mismatched_modifier_faults`
exercise `autda` on an aarch64 host with PAuth. No CI job runs `run.sh`
yet (verus/ISLA.md).
