# `<*mut T>::add`

Seam S338 of `verus/SEAMS.md`: src/vspec/ptr.rs:84. Kind: assume_spec. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T>[ <*mut T>::add ](p: *mut T, count: usize) -> (r: *mut T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(p, count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_add(p, count),
    opens_invariants none
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::ptr_add` (`src/kani/ptr.rs`):
on arrays of 5 `u8`s, 5 `u64`s and 3 `(u64, u8)`s (a padded element),
from every element `i` (or one past the end) and every `count` with
`i + count` at most the length, `p.add(count)` is defined (Kani checks
that the result stays inside, or one past, the object `p` points into)
and is `count * size_of::<T>()` bytes on from `p`; when it is an
element, a read through it is that element (Kani accepts the read only
through a pointer derived from the array: the provenance is `p`'s).
`add(0)` on a pointer made from any address (null and dangling
included) returns the pointer unchanged, and on a zero-sized `T`
`add(count)` returns `p` for every `count`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_add` (both quanta;
`--harness` takes the full path).

## Translation

- `requires count * size_of::<T>() <= isize::MAX` and, unless that
  offset is 0, `in_alloc(p, count * size_of::<T>())`: `in_alloc` is
  `0 <= len <= alloc_len(provenance, address)` with `alloc_len`
  uninterpreted ("the range from `p` lies in `p`'s allocation"),
  here the array `p` was derived from; the empty-offset case is the
  `add(0)` and zero-sized checks, on pointers to no allocation.
- `ensures r == ptr_add(p, count)`: `ptr_add` is uninterpreted; what
  the proofs know of it is `axiom_ptr_add_addr` (S329, discharged by
  Kani the same way): the address `count * size_of::<T>()` bytes on,
  `p`'s provenance and metadata. The harness checks that address and
  that provenance (the read through `r`); `T: Sized`, so the metadata
  is `()`. `add` is a pure function of `p` and `count`, so one value
  per pair, as an uninterpreted function has.
- `opens_invariants none`: it returns without a panic on every input
  checked.

Gap: generic over `T`; checked at a byte, a word, a padded pair and a
zero-sized type, on arrays of up to 5 elements, every in-range
`(i, count)`. `add` is one offset computation whatever the length, so
longer arrays only scale the numbers.

Provenance: every non-empty range is checked on pointers derived from
a local array, which Kani tracks as one object. The allocator's
pointers are integer-to-pointer casts of mapped addresses
(`vspec::addr::addr_to_ptr`, S252, whose provenance claim stays
trusted); Kani has no model of those mappings. The record covers what
the function does on a range that is valid; that the allocator's
ranges are valid is what the premises behind `rw_len` and `alloc_len`
(other seams) state.

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
