# `<*const T>::cast::<U>`

Seam S287 of `verus/SEAMS.md`: src/vspec/config.rs:20. Kind: assume_spec. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T: core::marker::PointeeSized, U>[ <*const T>::cast::<U> ](p: *const T) -> (r: *const U)
    ensures
        r@.addr == p@.addr,
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::ptr_cast` (`src/kani/ptr.rs`):
`cast` keeps the address: from `[u64; 2]` to `u8`, `u64` and `()`,
from `[u64]` and `str` (wide pointers) to `u8`, on pointers to objects,
and from `u32` to `u64` and `u8` on pointers made from every address.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_cast` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`.
- `ensures r@.addr == p@.addr`: the assertions `p.cast::<U>() as
  usize == p as usize` (for a wide `p`, its data address).

Gap: generic over `T: PointeeSized` and `U`; checked at the pairs
above. `cast` is a pointer-to-pointer `as` cast, which keeps the data
address whatever the types (dropping the metadata of a wide pointer).

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
