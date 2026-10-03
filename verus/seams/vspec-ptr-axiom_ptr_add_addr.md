# `axiom_ptr_add_addr`

Seam S320 of `verus/SEAMS.md`: src/vspec/ptr.rs:61. Kind: external_body proof. Category: core library.
Discharged by Kani.

## Contract

At commit 7d4e997 (checked again there; the record was written at
ae82d5b):

```rust
pub proof fn axiom_ptr_add_addr<T>(p: *mut T, count: usize)
    requires
        in_alloc(p, count as int * vstd::layout::size_of::<T>()),
        p@.addr + count as int * vstd::layout::size_of::<T>() <= usize::MAX,
    ensures
        ptr_add(p, count)@.addr == p@.addr + count as int * vstd::layout::size_of::<T>(),
        ptr_add(p, count)@.provenance == p@.provenance,
        ptr_add(p, count)@.metadata == p@.metadata,
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::misc::ptr_add_addr_instances` (`src/kani/misc.rs`):
for `T` in `u8` (5 elements), `u64` (5) and `(u64, u8)` (3), every
element index `i` and count with `i + count <= N`, `p = base.add(i)`
and `q = p.add(count)` satisfy `q as usize == p as usize + count * size_of::<T>()`,
and when `i + count < N` a read through `q` is element `i + count`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::misc::ptr_add_addr_instances` (both quanta;
`--harness` takes the full path).

## Translation

- `requires in_alloc(p, count * size_of::<T>())`: `p` and the range
  of `count` elements after it lie in one array (the allocation; one
  past its end allowed).
- `requires p@.addr + count * size_of::<T>() <= usize::MAX`: holds for
  any range inside an allocation, as the arrays are.
- `ensures ptr_add(p, count)@.addr == p@.addr + count * size_of::<T>()`:
  the first assertion, `ptr_add(p, count)` read as `p.add(count)`
  (the reading `<*mut T>::add`'s own specification gives it).
- `ensures ptr_add(p, count)@.provenance == p@.provenance`: the read
  through `q`; Kani's memory model rejects a read through a pointer
  outside the object its provenance names, so a successful read of the
  right element shows `q` keeps `p`'s. Gap: at one past the end
  there is nothing to read, and provenance is only checked where a read
  is possible.
- `ensures ptr_add(p, count)@.metadata == p@.metadata`: `add` needs
  `T: Sized`, whose pointer metadata is `()` on both sides.
- Gap: generic over `T`; Kani checks three element types, and the
  proofs place memory of many more. `add` computes the address from
  `size_of::<T>()` alone, so the type enters only through that size.

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
