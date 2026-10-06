# `core::ptr::copy_nonoverlapping::<T>`

Seam S259 of `verus/SEAMS.md`: src/vspec/addr.rs:51. Kind: assume_spec. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T>[ core::ptr::copy_nonoverlapping::<T> ](src: *const T, dst: *mut T, count: usize)
    requires
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || {
            let len = count as int * vstd::layout::size_of::<T>();
            &&& crate::vspec::ptr::valid_reads(src, len)
            &&& crate::vspec::ptr::valid_writes(dst, len)
            &&& (src@.addr + len <= dst@.addr || dst@.addr + len <= src@.addr)
        },
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::ptr_copy_nonoverlapping` (`src/kani/ptr.rs`):
on arrays of 5 `u8`s and of 4 `u64`s with arbitrary contents, from
every element `i` to every element `j` (or one past the end) and every
`count` with both ranges inside their array: a copy between two
arrays, and (when the two ranges are apart) a copy within one array,
is defined (Kani checks every read and write against the objects the
pointers point into, and `core`'s debug checks of alignment, non-null
and non-overlap) and copies exactly the `count` elements, leaving every
other element of the destination; and `copy_nonoverlapping(s, d, 0)`
returns for aligned pointers made from any addresses, null and equal
ones included.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_copy_nonoverlapping` (both quanta;
`--harness` takes the full path).

## Translation

- `requires` both addresses aligned: array elements are; the empty
  copy uses addresses rounded down to the alignment.
- `requires count * size_of::<T>() == 0 || (valid_reads(src, len) &&
  valid_writes(dst, len) && the ranges apart)`: `valid_reads` and
  `valid_writes` bound the length by the uninterpreted `rw_len` of the
  pointer's provenance and address ("the bytes may be read / written");
  here the ranges lie in the arrays the pointers are derived from, and
  the non-overlap is the harness's `i + count <= j || j + count <= i`
  (or two distinct arrays). The empty case is checked on pointers to
  no object.
- No `ensures`: the call is defined and returns. As for `write_bytes`
  (S256), the harness also shows that nothing outside the destination
  range is written.

Gap: generic over `T`; checked at a byte and a word, and at a zero-sized
type (every count, an empty copy), on arrays of up to
5 elements, every in-range `(i, j, count)`, run as constants (with a
symbolic count Kani 0.68 copies one element too few into an array of
8-byte elements; the module comment of `src/kani/ptr.rs`).
`copy_nonoverlapping` is one copy whatever the length, so the bound
only limits the sizes.

Provenance: every non-empty range is checked on pointers derived from
a local array, which Kani tracks as one object. The allocator's
pointers are integer-to-pointer casts of mapped addresses
(`vspec::addr::addr_to_ptr`, S255, discharged by Kani except its
provenance claim, which stays an assumption); Kani has no model of
those mappings. The record covers what
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
