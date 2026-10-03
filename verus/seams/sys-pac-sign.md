# `sys::pac::sign`

`sign` in `src/sys/pac.rs` (`src/sys/pac.rs:sign` in
`verus/seams.overrides`), one item for both architectures.
Tool: Isla. Status: discharged, with these named residual assumptions
(part 3): the process's key and PAC configuration are fixed; firmware
and kernel leave the PAC instructions untrapped; on CPUs with FEAT_FPAC
(and so FEAT_PAuth2; Armv8.6 features the model lacks, and present on
Neoverse V2, the C4A reference machine, and on Apple M2 and later, as
the module doc says), `pacda` is still a function of the same inputs,
as the architecture says but nothing here checks; and the model's
`ComputePAC` is a function of its arguments (read from its source and
spot-checked). The aarch64 body is `pacda`; the x86_64 body is plain
Rust and is never reached.

## 1. Contract

Entry S224 of verus/SEAMS.md at 8635213 (`src/sys/pac.rs:188`, kind
external_body):

```rust
pub fn sign(p: usize, modifier: usize) -> (r: usize)
    requires
        pac_available(),
    ensures
        r == spec_sign(p, modifier),
    opens_invariants none
```

`pac_available()` and `spec_sign` are uninterpreted. `pac_available()`
means every CPU the process runs on implements FEAT_PAuth (its doc);
`spec_sign(p, modifier)` is "what `sign` returns for `p` and `modifier`:
one value per pair, because every thread signs with the data key A it
got at `exec`", with the `prctl` caveats its doc names. The body Verus
does not see, at 8635213:

```rust
#[cfg(target_arch = "aarch64")]
{
    let mut v = p;
    core::arch::asm!(
        ".arch_extension pauth",
        "pacda {v}, {m}",
        v = inout(reg) v,
        m = in(reg) modifier,
        options(pure, nomem, nostack, preserves_flags),
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

Isla (rems-project/isla e9b5d94, `isla-footprint -s`) on the Sail
Armv8.5-A model (`armv8p5.ir`, isla-snapshots d8b3101), traces composed
and checked by `verus/isla/check.rs` with Z3 4.16.0. Reproduce with
`verus/isla/run.sh` (setup in verus/ISLA.md), section 6. Five facts:

1. **Template.** The `asm!` call in `fn sign` is the one above, up to
   whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** Every PAC-family instruction in the aarch64 release
   build (any `pac*`, `aut*`, `xpac*`, `reta*`, `bra*`, `blra*`,
   `ldra*`, `ereta*`) is `pacda Xd, Xn` or `autda Xd, Xn` with d and n
   distinct general registers (neither 31), or `xpacd Xd` (d not 31). At
   8635213: 45 `pacda` sites in 15 register assignments, 75 `autda` in
   17, one `xpacd`.
3. **Isla, the configurations.** Each instruction runs from a symbolic
   state at Non-secure EL0 in AArch64 state with the model's v8.2-v8.5
   features on (FEAT_PAuth among them) and these pinned: SCR_EL3 =
   0x30401 (NS, RW, API, APK); either a kernel at EL1 with HCR_EL2 =
   0x0000030080000000 (RW, API, APK: Linux's `HCR_HOST_NVHE_FLAGS`) or a
   VHE kernel with HCR_EL2 = 0x0000000488000000 (E2H, TGE, RW); and the
   translation control register EL0 uses (TCR_EL1, or TCR_EL2 under VHE)
   set as Linux sets it for one of its user VA sizes: 36 bits (16K
   pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) or 52 (64K), each with
   TBI0 on. Twelve configurations. SCTLR_EL1 (SCTLR_EL2 under VHE),
   whose EnDA bit enables the key, is symbolic, as are the pointer, the
   modifier and the key registers APDAKeyHi_EL1/APDAKeyLo_EL1. The PAC
   algorithm, the model's `ComputePAC(data, modifier, key0, key1)`, is
   left abstract (`isla-footprint --abstract ComputePAC`): the checker
   treats it as an uninterpreted function, so every result below holds
   for any function in its place, QARMA or an implementation's own.
4. **Isla, the results.** In each configuration:
   - for every `pacda` assignment at a site (`pac-footprint.job.in`):
     every path falls through to the next instruction, writes only Xd
     (with the PC and the model's own bookkeeping), reads and writes no
     memory and writes no flag;
   - for every such assignment against the canonical `pacda x0, x1`
     (`pac-det.job.in`, two copies of the machine from independent
     states): when x0 and Xd hold the same pointer, x1 and Xn the same
     modifier, and the two states have the same data key A and the same
     EnDA bit, the two results are equal. Leaving out the modifier
     (`pac-det-mod`) or either half of the key (`pac-det-keylo`,
     `pac-det-keyhi`) makes this job fail, and so does equating SCTLR
     bit 26 in place of EnDA in the `autda` job (`pac-det-enda`): the
     result depends on each.
5. **Isla, the PAC algorithm.** With `ComputePAC` unfolded, `pacda x0,
   x1` on eight concrete inputs (pointer, modifier, key; kernel at EL1,
   48-bit VA, EnDA set; `pac-pure.job.in`): the model's undefined values
   and the initial contents of `RC`, ComputePAC's round-constant array,
   are the only symbols left, and two copies of each trace, each with
   its own, give the same result, which differs from the pointer.

## 3. Translation

- **`requires pac_available()`.** On aarch64 it means the CPU implements
  FEAT_PAuth: the configurations above. That the instruction is not
  trapped is the residual **trap settings**: firmware sets
  SCR_EL3.{API,APK} on a CPU with PAuth (Trusted Firmware-A does), and
  Linux runs either at EL1 with `HCR_HOST_NVHE_FLAGS` in HCR_EL2 or at
  EL2 with VHE; under a hypervisor, the hypervisor either clears the
  trap or takes it and resumes the instruction with it cleared (KVM
  enables PAuth for a guest lazily that way), which the guest does not
  see. On x86_64 no CPU implements FEAT_PAuth, so `pac_available()`
  holds of no x86_64 process and no call meets the precondition: the
  x86_64 body is unreachable from verified code. (It returns `p`, which
  is a function of `(p, modifier)` too.)
- **`ensures r == spec_sign(p, modifier)`.** The contract asks that what
  `sign` returns is one function of `(p, modifier)` for the whole
  process, the same at every call site and in every thread. Fact 4: at
  every site, whatever registers the compiler chose, the result is
  `F(p, modifier, K, E)` for the one function `F` that `pacda x0, x1`
  computes in that configuration, `K` the data key A and `E` the EnDA
  bit. The residual **fixed key and configuration**: `K`, `E` and the
  configuration of fact 3 are the same in every thread of the process
  and never change while it runs. Linux gives each process fresh keys
  at `exec`, copies them to every thread at `clone`, and changes them
  only through `prctl(PR_PAC_RESET_KEYS)` and `E` only through
  `prctl(PR_PAC_SET_ENABLED_KEYS)`, which `spec_sign`'s doc already
  names as trusted (challoc never calls either); TCR and HCR are the
  kernel's, fixed at boot. Under it, `spec_sign(p, m) = F(p, m, K, E)`
  satisfies the contract. When EnDA is clear `F` is the identity, still
  one function. This needs `ComputePAC`, which the jobs leave abstract,
  to be a function of its four arguments, the residual **PAC algorithm
  as a function**: read from the model's source, `ComputePAC` and its
  callees (`PACSub`, `PACInvSub`, `PACMult`, `PACCellShuffle`,
  `PACCellInvShuffle`, `TweakShuffle`, `TweakInvShuffle`, `RotCell`,
  `TweakCellRot`, `TweakCellInvRot`) read no architectural register;
  `RC`, a model register, is written with constants before it is read;
  locals start undefined and are filled before use. Fact 5 checks the
  last two on eight inputs. Only `PACSub` and `PACInvSub` branch on the
  data (a case per nibble over its 16 values, with a default); every
  other function runs the same assignments on every input, so for them
  fact 5 covers all inputs, and for the two substitutions the
  completeness of the cases is read from the source.
- **The interface.** Fact 4's footprint is the declared one: only `v`
  written, no memory (`nomem`; and `nostack`, which the absence of any
  memory write also gives), no flags (`preserves_flags`), a
  fall-through. `pure` lets the compiler merge or
  drop calls with equal operands, which the result's being a function
  of the operands under the residual above makes sound. That the
  compiler binds `v` and `m` to the Xd and Xn of each site is its
  inline-assembly semantics, trusted as for every Verus result; fact 2
  checks the result at every site.
- **`opens_invariants none`.** The body has no ghost code and opens no
  invariant.
- **FEAT_PAuth2 and FEAT_FPAC** (a residual, not only a gap: the
  reference aarch64 machine has them). The model is the ASL-derived
  Armv8.5-A snapshot, without FEAT_PAuth2, FEAT_FPAC or
  FEAT_CONSTPACFIELD (Armv8.6 and later). With FEAT_PAuth2 `pacda`
  combines the PAC with the pointer by XOR instead of replacing the
  field; the architecture defines that as again a function of the same
  pointer, modifier, key and configuration, so the argument above
  carries over, but that is read from the architecture, not checked
  (verus/ISLA.md, plan item 2: the v9.4 snapshot has both).
- **Gaps.** One TCR value per VA size stands for the kernel's (the
  other TCR fields are Linux's defaults; 52-bit VA with 4K or 16K pages,
  FEAT_LPA2 in Armv8.7, is not modelled). The code read is the static
  library's (verus/ISLA.md, plan item 3): the shipped shared library may
  use register assignments not in the 15, which no job then covers.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its findings on this record are folded in:
FEAT_PAuth2 and FEAT_FPAC, present on the reference machine, named as a
residual in the status line instead of a gap; the purity of ComputePAC
extended to its callees and spot-checked on eight inputs (fact 5); the
negative controls described as run; the nomem/nostack wording
corrected. Its finding on run.sh, that a trace left by a killed earlier
run could be reused, is fixed (traces are written whole and made in
every run). A human review is pending.

## 5. Tests

`verus/isla/run.sh` fails when the `asm!` call changes (fact 1), when
the ledger entry with this record's status has another contract (fact
1), when the aarch64 build emits a PAC-family instruction other than the
three shapes above or none of `pacda` (fact 2), when any footprint or
determinism job fails in any configuration (fact 4), when a purity spot
check fails (fact 5), and when a negative control passes (among them
determinism without the modifier or either half of the key or with
another SCTLR bit for EnDA, a job whose abstract function is not named,
and a spot check claiming the PAC left the pointer unchanged). `sh
verus/seams.sh --check` fails when verus/SEAMS.md is stale. The unit test
`sys::pac::tests::sign_auth_round_trip` exercises the instructions on an
aarch64 host with PAuth. No CI job runs `run.sh` yet: it needs the Isla
toolchain (verus/ISLA.md).
