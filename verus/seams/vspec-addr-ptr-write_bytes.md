# `core::ptr::write_bytes::<T>`

Seam S265 of `verus/SEAMS.md`: src/vspec/addr.rs:40. Kind: assume_spec. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub assume_specification<T>[ core::ptr::write_bytes::<T> ](dst: *mut T, val: u8, count: usize)
    requires
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || crate::vspec::ptr::valid_writes(dst, count as int * vstd::layout::size_of::<T>()),
;
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::ptr_write_bytes` (`src/kani/ptr.rs`):
on an array of 5 `u8`s and one of 4 `u64`s with arbitrary contents
and an arbitrary fill byte, `write_bytes(p, val, count)` from every
element `i` (or one past the end) and every `count` with `i + count` at
most the length is defined (Kani checks every write against the
object `p` points into, and the alignment and non-null requirements
of `core`'s debug checks) and sets exactly the `count * size_of::<T>()`
bytes from `p` to `val`, leaving every other byte of the array; and
`write_bytes(d, val, 0)` returns for an aligned pointer `d` made from
any address, null included.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_write_bytes` (both quanta;
`--harness` takes the full path).

## Translation

- `requires dst@.addr % align_of::<T>() == 0`: the pointers checked
  are array elements (aligned) and, for the empty fill, addresses
  rounded down to the alignment.
- `requires count * size_of::<T>() == 0 || valid_writes(dst, count *
  size_of::<T>())`: `valid_writes(p, len)` is `0 <= len <= rw_len(provenance,
  address)` with `rw_len` uninterpreted ("the bytes from
  `dst` may be written"); here they lie in the array `dst` is derived
  from. The empty case is checked on pointers to no object.
- No `ensures`: the call is defined and returns. Verus also takes the
  absence of an `ensures` to mean that no memory a ghost permission
  covers changes; the harness shows that the fill writes no byte
  outside its range, and that the range is not under any permission
  is what the callers' `valid_writes` premises (other seams) stand
  for.

Gap: generic over `T`; checked at a byte and a word (the proofs fill
bytes), on arrays of 5 and 4 elements, every in-range
`(i, count)`. `i` and `count` run as constants: with a symbolic count
Kani 0.68 fills one element too few of an array of 8-byte elements
(the module comment of `src/kani/ptr.rs`), which would hide a write
past the range. `write_bytes` is one fill whatever the length, so the
bound only limits the sizes. Not checked at a zero-sized `T` with a
non-zero count: Kani 0.68 reports such an empty fill through a pointer
to an array of `()` as a write to a region that is not writeable (a
model artefact; `core` writes nothing). The proofs fill bytes only
(`*mut u8`: `vspec::zero::write_zero` and the `api` fills).

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
