# `Result::<T, E>::map_or`

Seam S350 of `verus/SEAMS.md`: src/vspec/sys.rs:33. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub assume_specification<T, E, U, F: FnOnce(T) -> U>[ Result::<T, E>::map_or ](
    result: Result<T, E>,
    default: U,
    f: F,
) -> (r: U)
    requires
        result matches Ok(v) ==> f.requires((v,)),
    ensures
        result is Err ==> r == default,
        result matches Ok(v) ==> f.ensures((v,), r),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::misc::result_map_or` (`src/kani/misc.rs`):
for every `result: Result<u64, u32>` and `default: u32`,
`result.map_or(default, f)` is `default` (and `f` not called) for
`Err`, and `f(v)` (with `f` called once, on `v`) for `Ok(v)`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::misc::result_map_or` (both quanta;
`--harness` takes the full path).

## Translation

- `requires result matches Ok(v) ==> f.requires((v,))`: the harness's
  `f` accepts every call.
- `ensures result is Err ==> r == default` and
  `result matches Ok(v) ==> f.ensures((v,), r)`: the two branches.

The closure is checked through a closure that records its argument
and returns a function of it from the family `v.wrapping_mul(m) ^ c`
(`m`, `c` arbitrary; for `is_some_and`, `v.wrapping_mul(m) < c`): Verus's `f.ensures((v,), r)` for a
deterministic `f` is `r == f(v)`, and `f.requires` holding only on
the stated inputs means `f` is not called on others, so the harness
asserts both that `f` ran once, on `v`, with `r` its result, and
that it did not run otherwise. Gap: the methods are generic over `T`,
`U` and `F`; Kani checks `T = u64` and one closure shape. The
methods move their arguments without looking at them, so the type does
not change the control flow.

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

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
