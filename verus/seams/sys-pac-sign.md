# `sys::pac::sign`

`sign` in `src/sys/pac.rs` (`src/sys/pac.rs:sign` in
`verus/seams.overrides`), one item for both architectures.
Tool: Isla. Status: discharged, with these named residual assumptions
(part 3): the process's key and PAC configuration are fixed; firmware
and kernel leave the PAC instructions untrapped; the model's
`ComputePAC` is a function of its arguments (read from its source and
spot-checked); and the CPU's pointer authentication is one of the two
kinds checked: FEAT_PAuth as Armv8.3-v8.5 has it (the Armv8.5-A model),
or FEAT_PAuth with FEAT_PAuth2 and FEAT_FPAC (the Armv9.4-A model run as
an Armv9.0-A CPU, as Neoverse V2 in the C4A reference machine is; Apple
M2 and later too). CPUs with FEAT_EPAC alone, or FEAT_PAuth2 without
FEAT_FPAC, are not checked. The aarch64 body is `pacda`; the x86_64 body
is plain Rust and is never reached.

## 1. Contract

Entry S221 of verus/SEAMS.md at 8635213 (`src/sys/pac.rs:188`, kind
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

Isla (rems-project/isla e9b5d94, `isla-footprint -s`) on two Sail models
from isla-snapshots d8b3101: the Armv8.5-A model (`armv8p5.ir`) and the
Armv9.4-A model (`armv9p4.ir.gz`, last changed in isla-snapshots
9591cbb, built from sail-arm's `wip-rebase-v9.4` branch at 111345b;
sha256 of the `.gz`
d8c547eefd125a8bd01a827d2733fc9eddbc415839e8d278310e527c7721a3db).
Traces are composed and checked by `verus/isla/check.rs` with Z3 4.16.0.
Reproduce with `verus/isla/run.sh` (setup in verus/ISLA.md), section 6.
Six facts:

1. **Template.** The `asm!` call in `fn sign` is the one above, up to
   whitespace (`verus/isla/templates.txt`).
2. **Compiled code.** Every PAC-family instruction in the aarch64 release
   build (any `pac*`, `aut*`, `xpac*`, `reta*`, `bra*`, `blra*`,
   `ldra*`, `ereta*`) is `pacda Xd, Xn` or `autda Xd, Xn` with d and n
   distinct general registers (neither 31), or `xpacd Xd` (d not 31). At
   8635213: 45 `pacda` sites in 15 register assignments, 75 `autda` in
   17, one `xpacd`.
3. **Isla, the configurations.** Each instruction runs from a symbolic
   state at Non-secure EL0 in AArch64 state with these pinned: SCR_EL3 =
   0x30401 (NS, RW, API, APK); either a kernel at EL1 with HCR_EL2 =
   0x0100030080000000 (RW, API, APK, ATA: Linux's
   `HCR_HOST_NVHE_FLAGS`) or a VHE kernel with HCR_EL2 =
   0x0000000488000000 (E2H, TGE, RW); and the translation control
   register EL0 uses (TCR_EL1, or TCR_EL2 under VHE) set as Linux sets
   it for one of its user VA sizes: 36 bits (16K pages), 39 (4K), 42
   (64K), 47 (16K), 48 (4K) or 52 (64K), each with TBI0 on. SCTLR_EL1
   (SCTLR_EL2 under VHE), whose EnDA bit enables the key, is symbolic,
   as are the pointer, the modifier and the key registers
   APDAKeyHi_EL1/APDAKeyLo_EL1. Those twelve, in each of three CPU
   settings, 36 configurations:
   - **Armv8.5-A** (`armv8p5.ir`): the model's v8.2-v8.5 features on,
     FEAT_PAuth among them; no FEAT_EPAC, FEAT_PAuth2 or FEAT_FPAC.
   - **Armv9.4-A as Armv9.0-A** (`armv9p4.ir`, configurations `p2-*`):
     the architecture version flags v8.2-v8.5 and v9.0 on (v8.6-v8.9 and
     v9.1-v9.4 off); FEAT_PAuth, FEAT_PAuth2, FEAT_FPAC and
     FEAT_FPACCOMBINE on, FEAT_EPAC and FEAT_CONSTPACFIELD off, the
     QARMA5 algorithm (FEAT_PACQARMA5); and every other feature flag of
     Armv8.6 and later that the model's register initialisers would turn
     on pinned off (103 of them, among them FEAT_GCS, FEAT_S1PIE,
     FEAT_THE, FEAT_TCR2, FEAT_D128, FEAT_LVA3 and FEAT_LPA2: `run.sh`'s
     `pac9_later`). The features up to Armv9.0 are as the model's
     initialisers and Isla's `armv9p4.toml` leave them (the file turns
     FEAT_BTI and FEAT_MTE off; Neoverse V2 has BTI, which concerns
     branches only). The model's footprint function
     `isla_footprint_no_init` (the default one runs the model's reset
     first) starts from the model's register initialisers, the
     configuration's defaults and `[registers.reset]` values and the
     pinned registers, with EDSCR in Non-debug state (no external
     debugger has halted the core), and ends before the model's PC
     update, which the checker applies (`pc-step`: the PC advances by 4
     unless the instruction set `__BranchTaken`, as the model's
     `__InstructionExecute` does). The footprint function also swallows
     an UNDEFINED abort without a trace event, so under `pc-step` every
     instruction names the register it writes when it completes, and a
     path that writes nothing fails (`pac9-udf`: `udf #0`).
   - **The same with FEAT_CONSTPACFIELD** (`p2c-*`): the PAC field's
     size taken from bit 55 whatever the TBI settings (ID_AA64ISAR2_EL1.
     PAC_frac = 1), so either value of that field is covered.
   The PAC algorithm, the model's `ComputePAC` (four arguments in the
   v8.5 model, `(data, modifier, key0, key1)`; five in the v9.4 one, the
   last `isgeneric`, false here), is left abstract (`isla-footprint
   --abstract ComputePAC`): the checker treats it as an uninterpreted
   function, so every result below holds for any function in its place,
   QARMA5, QARMA3 or an implementation's own. The v9.4 traces come from
   Isla's serialized form of the model (`isla-preprocess`); four of them
   (one per configuration file, and an FPAC exception entry) are
   compared with the text model's and are the same.
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
     (`pac-det-mod`, `pac9-det-mod`) or either half of the key
     (`pac-det-keylo`, `pac-det-keyhi`, and the `pac9-` ones) makes this
     job fail, and so does equating SCTLR bit 26 in place of EnDA in the
     `autda` job (`pac-det-enda`, `pac9-det-enda`): the result depends
     on each.
5. **Isla, what `pacda x0, x1` computes** (`pac-spec-sign85.job.in`,
   `pac-spec-sign94.job.in`, `pac-enda-off.job.in`), in each
   configuration of its model, for a user pointer `p` (bit 55 clear, the
   TBI0 half) and a VA width `B` (64 - T0SZ): with EnDA clear the result
   is `p`; with EnDA set, let `E` be `p` with bits 54:B replaced by
   copies of bit 55 (here 0) and `P = ComputePAC(E, modifier, key A high,
   key A low)`. The bits 63:55 and B-1:0 of the result are `p`'s, and
   bits 54:B are
   - in the Armv8.5-A model, `P`'s bits 54:B, with bit 54 inverted when
     `p`'s bits 55:B are not all equal (the PAC *replaces* the field);
   - with FEAT_PAuth2, `p`'s bits 54:B XOR `P`'s (the PAC is *XORed
     into* the field, whatever it held).
   The controls `pac-spec-flip` (no inversion) and `pac9-spec-xor` (OR
   for XOR) fail.
6. **Isla, the PAC algorithm.** With `ComputePAC` unfolded, `pacda x0,
   x1` on eight concrete inputs (pointer, modifier, key; kernel at EL1,
   48-bit VA, EnDA set; `pac-pure.job.in`), in the v8.5 model and in the
   v9.4 one with QARMA5 and with QARMA3 (24 checks): the model's
   undefined values and the initial contents of `RC`, ComputePAC's
   round-constant array, are the only symbols left, and two copies of
   each trace, each with its own, give the same result, which differs
   from the pointer.

## 3. Translation

- **`requires pac_available()`.** On aarch64 it means the CPU implements
  FEAT_PAuth: the configurations above. That the instruction is not
  trapped is the residual **trap settings**: firmware sets
  SCR_EL3.{API,APK} on a CPU with PAuth (Trusted Firmware-A does), and
  Linux runs either at EL1 with `HCR_HOST_NVHE_FLAGS` in HCR_EL2 or at
  EL2 with VHE; under a hypervisor, the hypervisor either clears the
  trap or takes it and resumes the instruction with it cleared (KVM
  enables PAuth for a guest lazily that way), which the guest does not
  see. C4A is such a guest: its kernel runs at EL1 under the
  hypervisor's HCR_EL2, not `HCR_HOST_NVHE_FLAGS`, and the EL1
  configurations stand for it because, of HCR_EL2, only API (with APK
  for the key registers), TGE and E2H bear on whether these
  instructions trap and where the FPAC exception goes (the model's
  `AddPACDA`, `AuthDA` and `AArch64_PACFailException`; read from the
  source). On x86_64 no CPU implements FEAT_PAuth, so `pac_available()`
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
  to be a function of its arguments, the residual **PAC algorithm as a
  function**: read from each model's source, `ComputePAC` and its
  callees (in v8.5 `PACSub`, `PACInvSub`, `PACMult`, `PACCellShuffle`,
  `PACCellInvShuffle`, `TweakShuffle`, `TweakInvShuffle`, `RotCell`,
  `TweakCellRot`, `TweakCellInvRot`; in v9.4 `ComputePACQARMA` with
  those and QARMA3's `PACSub1`, behind a dispatch on the configuration's
  algorithm features) read no architectural register; `RC`, a model
  register, is written with constants before it is read; locals start
  undefined and are filled before use. Fact 6 checks the last two on
  eight inputs per model and algorithm. Only the substitutions (`PACSub`,
  `PACInvSub`, `PACSub1`) branch on the data (a case per nibble over its
  16 values, with a default); every other function runs the same
  assignments on every input, so for them fact 6 covers all inputs, and
  for the substitutions the completeness of the cases is read from the
  source.
- **The CPU kind.** The C4A reference machine (Neoverse V2, Armv9.0-A)
  implements FEAT_PAuth2 and FEAT_FPAC (`src/sys/pac.rs`'s module doc;
  `pac::fpac` reads it from the ID registers in the tests). Facts 3-5
  cover it under the `p2-*` and `p2c-*` configurations, for either
  value of ID_AA64ISAR2_EL1.PAC_frac and any algorithm (abstract).
  Whether FEAT_FPACCOMBINE is on makes no difference here: it changes
  only the combined authenticate-and-branch or -load instructions
  (`retaa`, `ldraa` and the like; the model's `Auth` faults when
  `HaveFPACCombined() || HaveFPAC() && !is_combined`, so with FEAT_FPAC
  a non-combined `autda` faults either way), none of which challoc
  emits (fact 2). The Armv8.5-A configurations cover the CPUs with
  FEAT_PAuth alone (Armv8.3-v8.5 pointer authentication). Not checked:
  FEAT_EPAC without FEAT_PAuth2 (it zeroes the PAC of a non-canonical
  pointer) and FEAT_PAuth2 without FEAT_FPAC (a failed `autda` returns
  the pointer with the XOR applied); the architecture defines both as
  functions of the same inputs, so the argument above would carry over,
  but that is read, not checked. The v9.4 model is Arm's ASL for v9.4
  translated to Sail on a work-in-progress branch: what the checks show
  is that model's behaviour, as the v8.5 results are the v8.5 model's.
  The model's CPU is Neoverse V2's in its pointer authentication, not in
  everything: Neoverse V2 implements some Armv8.6 features that are
  pinned off here (data-processing ones, such as FEAT_BF16 and
  FEAT_I8MM), and that none of them bears on these instructions or the
  FPAC exception entry is read, not checked.
- **What FEAT_PAuth2 changes** (fact 5): `pacda` XORs the PAC into the
  pointer's PAC field instead of replacing it, so signing a pointer
  whose field is not all copies of bit 55 no longer marks it (v8.5
  inverted bit 54); for the pointers challoc signs (user addresses, the
  field clear) the two give the same field. Either way the result is a
  function of the pointer, modifier, key and configuration, which is all
  the contract needs.
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
- **Gaps.** One TCR value per VA size stands for the kernel's (the
  other TCR fields are Linux's defaults; 52-bit VA with 4K or 16K pages,
  FEAT_LPA2, is not among the configurations). The code read is the
  static library's (verus/ISLA.md, plan item 2): the shipped shared
  library may use register assignments not in the 15, which no job then
  covers. Fact 5 describes user pointers only (bit 55 clear); for the
  others (bit 55 set, the TBI1 half, which Linux leaves without
  top-byte-ignore) facts 4 and 6 hold as for every pointer, but the
  formula is not stated.

## 4. Review

Translation reviewed on 2026-10-03 by a fresh-context reviewer (a Claude
subagent with no prior context of this work), told to look for
translations that overclaim. Its findings on this record are folded in:
FEAT_PAuth2 and FEAT_FPAC, present on the reference machine, named as a
residual in the status line instead of a gap; the purity of ComputePAC
extended to its callees and spot-checked on eight inputs (fact 6); the
negative controls described as run; the nomem/nostack wording
corrected. Its finding on run.sh, that a trace left by a killed earlier
run could be reused, is fixed (traces are written whole and made in
every run).

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

`verus/isla/run.sh` fails when the `asm!` call changes (fact 1), when
the ledger entry with this record's status has another contract (fact
1), when the aarch64 build emits a PAC-family instruction other than the
three shapes above or none of `pacda` (fact 2), when a v9.4
configuration file does not set the features of fact 3 exactly once,
when a serialized-model trace differs from the text model's, when any
footprint, determinism or `pacda` formula job fails in any
configuration (facts 4 and 5), when a purity spot check fails (fact 6),
and when a negative control passes (among them determinism without the
modifier or either half of the key or with another SCTLR bit for EnDA,
in both models, a job whose abstract function is not named, the two
formula controls of fact 5, a v9.4 job without the PC update, `udf #0`
claimed to complete, and a spot check claiming the PAC left the pointer
unchanged). `sh verus/seams.sh --check` fails when verus/SEAMS.md is
stale. The unit test `sys::pac::tests::sign_auth_round_trip` exercises
the instructions on an aarch64 host with PAuth. No CI job runs `run.sh`
yet: it needs the Isla toolchain (verus/ISLA.md).
