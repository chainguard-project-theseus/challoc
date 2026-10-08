# `Arena::axiom_array_layout`

Seam S51 of `verus/SEAMS.md`: src/arena.rs:344. Kind: axiom. Category: core library.
Discharged by Kani.

## Contract

As changed by the library-and-control seam audit on base 8635213
(`verus/audit/library-control.md`; Kani re-run below), from the
contract checked at commit 7d4e997 (the record was written at
ae82d5b):

```rust
pub(crate) axiom fn axiom_array_layout<T, const N: usize>()
    requires
        N * size_of::<T>() <= isize::MAX,
    ensures
        size_of::<[T; N]>() == N * size_of::<T>(),
        align_of::<[T; N]>() == align_of::<T>(),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `arena::kani_layout::array_layout_instances, heap::shared::kani_layout::shared_array_layout` (`src/arena.rs, src/heap/shared.rs`):
for `[ClassState; NCLASSES]` (`arena::kani_layout::array_layout_instances`)
and `[ClassLine; NCLASSES]` (since the port of upstream #87, in place
of `[GuardTest; NCLASSES]`), `[SlabTable; NCLASSES]`
(`heap::shared::kani_layout::shared_array_layout`), the three arrays
the proofs split with `split_array`, and for `u8` (lengths 0 and 7),
`u64` (1 and 7), `(u64, u8)` (a padded element), `()` (zero-sized),
`[u16; 3]` and `AtomicU64` (7 each):
`size_of::<[T; N]>() == N * size_of::<T>()` (in `u128`) and
`align_of::<[T; N]>() == align_of::<T>()`.

Reproduce: `sh verus/kani/run.sh --harness arena::kani_layout::array_layout_instances --harness heap::shared::kani_layout::shared_array_layout` (both quanta;
`--harness` takes the full path).

## Translation

- `requires N * size_of::<T>() <= isize::MAX`: every checked instance
  meets it (the array exists); the harness need not assume it.
- `ensures size_of::<[T; N]>() == N * size_of::<T>()` and
  `align_of::<[T; N]>() == align_of::<T>()`: the two assertions of
  `array_layout::<T, N>`.
- Gap: the axiom is generic over every `T` and `N`; Kani checks the
  instantiations the proofs use (`split_array`'s three callers,
  `grep -rn split_array src`) and a spread of others. For all `T` and
  `N` it is the Rust reference's statement of array layout (an array
  is `N` elements laid out with stride `size_of::<T>()`, which is a
  multiple of the alignment) for every array that has a layout, which
  the `requires` names: an array of more than `isize::MAX` bytes has
  none, and without the `requires` the axiom proved `false` with vstd's
  `layout_for_type_is_valid` at `[u64; usize::MAX]`. The three callers'
  instances get the bound from `axiom_arena_layout` and
  `axiom_shared_layout`.
- The instances are checked by Kani for x86_64 only; the three
  `split_array` instances are also checked on every target and
  quantum by build-time assertions (in `arena.rs` for `ClassState` and
  in `heap/shared.rs` for `ClassLine` (since #87; `GuardTest` before) and
  `SlabTable`, added by the seam
  audit `verus/audit/invariants.md`). `verus/kani/run.sh` runs the
  harnesses under both quanta, since `NCLASSES` is the length of the
  three arrays.

Common to every Kani record: Kani checks the MIR of a debug build
(overflow checks and debug assertions on) for x86_64, with CBMC's
bit-precise semantics for the integer operations and Kani's own models
of the `core` intrinsics it lowers. The shipped library is a release
build: it drops the overflow checks and debug assertions (where a
harness shows they never fire under the contract, the two builds
agree), and it is also built for aarch64, whose layouts and pointer
width match x86_64's for every type here (both are LP64 targets;
the layout records name the one check that runs per target). The
`core` Kani checks is its own nightly's (2026-08-21); the shipped
library is built with rustc 1.98, whose `core` functions here are the
same unless they changed between the two, which a rerun with a newer
Kani would show. Policy for every Kani record: a trusted body must
return under its `requires` without a panic, except an abort seam
(whose contract is that it does not return) and the overflow aborts
a contract's comment declares (`fig_*`, `stats_allocated`); a panic
in challoc aborts the process (`panic = "abort"`), so it never yields
a wrong result, but the proofs' "no panic" claim rests on this policy.

## Review

Fresh-context subagent, 2026-10-02: the harness against the contract
and this translation; its findings are folded in.

Reconciled onto commit 7d4e997, 2026-10-03: the contract above
compared with the ledger's entry there, and the harness re-run under
both quanta with the same Kani (it passes). A fresh-context subagent
reviewed the reconciliation.

Ported onto upstream #87, 2026-10-06: the contract is unchanged; the
`shared_array_layout` harness now checks `[ClassLine; NCLASSES]` in
place of `[GuardTest; NCLASSES]` (the build's `const` assertion of the
same, on both targets and quanta, passes).

Re-run at the port's last code commit (upstream #116 onto the CCall
tip), 2026-10-06, with Kani 0.68.0 under both quanta:
`arena::kani_layout::array_layout_instances` and
`heap::shared::kani_layout::shared_array_layout` pass (0 of 247 and 0
of 58 checks failed in each), which also settles the audit's pending
re-run (above).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet. Every build checks the
three `split_array` instances (the assertions above).
