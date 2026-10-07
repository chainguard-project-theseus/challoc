# `core::sync::atomic::Atomic::<*mut T>::load`

Seam S266 of `verus/SEAMS.md`: src/vspec/arena.rs:13. Kind: assume_spec. Category: core library.
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T>[ core::sync::atomic::Atomic::<*mut T>::load ](
    a: &core::sync::atomic::Atomic<*mut T>,
    order: core::sync::atomic::Ordering,
) -> *mut T
    requires
        !(order is Release),
        !(order is AcqRel),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::atomic::atomic_ptr` (`src/kani/atomic.rs`):
for a pointer to a local or made from any address (null included),
`AtomicPtr::new` returns, `load` with every ordering but `Release`
and `AcqRel` returns (the stored pointer), `store` with every
ordering but `Acquire` and `AcqRel` returns, and `swap` with every
ordering returns (the previous pointer).

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::atomic::atomic_ptr` (both quanta;
`--harness` takes the full path).

## Translation

- `requires !(order is Release) && !(order is AcqRel)`: the harness takes every other ordering (`Relaxed`, `Acquire`, `SeqCst`).
- No `ensures` ("any pointer"): the call returns. That the two excluded orderings are the ones `core` panics on is shown by `kani_proofs::atomic::atomic_ptr_bad_orders`, not part of the contract.

Gap: generic over `T`; checked at `u64`. `AtomicPtr<T>` stores the
pointer as a word whatever `T` is. Kani runs one thread: the harness
shows each call defined and returning on the orderings the contract
allows; the values other threads could make a load return are what the
contracts leave open ("any pointer"), and no harness constrains them.

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

Fresh-context subagent, 2026-10-04: the harness against the contract
and this translation, looking for claims the harness does not support;
its findings are folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
