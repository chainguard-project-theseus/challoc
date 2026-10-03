# `sys::pac::auth`

`auth` in `src/sys/pac.rs` (`src/sys/pac.rs:auth` in
`verus/seams.overrides`), one item for both architectures.
Tool: Isla. Status: discharged, with the same named residual assumptions
as `sign` ([sys-pac-sign.md](sys-pac-sign.md), part 3): the fixed key and
configuration, the trap settings, `ComputePAC` a function of its
arguments, and on CPUs with FEAT_FPAC and FEAT_PAuth2 (the C4A reference
machine among them; not in the model) the architecture's word that a
successful `autda` is still a function of the same inputs. The aarch64
body is `autda`; the x86_64 body is plain Rust and is never reached.

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
tools, `run.sh` section 6, the same twelve configurations and the
abstract `ComputePAC`), for `autda`:

1. **Template.** The `asm!` call in `fn auth` is the one above, up to
   whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** As in `sign`'s fact 2: 75 `autda Xd, Xn` sites at
   8635213, in 17 register assignments, d and n distinct and neither 31.
3. **Isla.** In each configuration, for every `autda` assignment at a
   site: every path falls through, writes only Xd (with the PC and the
   model's bookkeeping), reads and writes no memory and writes no flag
   (`pac-footprint.job.in`); and against the canonical `autda x0, x1`
   from an independent state, the same pointer, modifier, data key A
   and EnDA bit give the same result (`pac-det.job.in`; the negative
   control `pac-det-enda` runs on `autda`). Both the paths where the PAC
   matches and those where it does not (the result then carries the
   error code in the PAC field) are among the traces: neither faults in
   this model.

## 3. Translation

- **`requires pac_available()`.** As for `sign`: FEAT_PAuth on aarch64,
  with the trap-settings residual; no x86_64 process meets it, so the
  x86_64 body (which returns `p`) is unreachable from verified code.
- **`ensures r == spec_auth(p, modifier)`.** As for `sign`: fact 3 shows
  every site computes `G(p, modifier, K, E)` for the one `G` that `autda
  x0, x1` computes, and under the residual that the key `K`, the EnDA
  bit `E` and the configuration are fixed for the process, `spec_auth(p,
  m) = G(p, m, K, E)`. That covers a failed authentication too: the
  value with the error code is `G`'s value for that pair.
- **The interface.** Only `v` written, no memory, no flags, a
  fall-through: the declared `nomem`, `nostack`, `preserves_flags`. The
  block is not `pure`, so the compiler keeps every call; the result
  being a function of the operands would allow `pure` too.
- **`opens_invariants none`.** No ghost code.
- **FEAT_FPAC and FEAT_PAuth2** (a residual, as for `sign`). With
  FEAT_FPAC (Armv8.6; not in the model) a failed `autda` faults instead
  of returning (the kernel sends `SIGILL`), which ends the process unless
  a handler intervenes, and is not a return, so it does not contradict
  the contract; that a successful `autda` is a function of the same
  inputs there (with FEAT_PAuth2's XOR) is read from the architecture,
  not checked.
- **Gaps.** As for `sign`: one TCR value per VA size, the static
  library's code.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its findings on this record are folded in:
FEAT_FPAC and FEAT_PAuth2 named as a residual in the status line; and
those on sign's record and run.sh apply here. A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes, when the ledger
entry with this record's status has another contract, when the build
emits a PAC-family instruction outside the three shapes or no `autda`,
when any `autda` footprint or determinism job fails in any
configuration, and when a negative control passes. `sh verus/seams.sh
--check` fails when verus/SEAMS.md is stale. The unit tests
`sys::pac::tests::sign_auth_round_trip` and `mismatched_modifier_faults`
exercise `autda` on an aarch64 host with PAuth. No CI job runs `run.sh`
yet (verus/ISLA.md).
