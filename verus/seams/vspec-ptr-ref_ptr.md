# `ref_ptr` (`vspec/ptr.rs`)

Seam S367 of `verus/SEAMS.md`: src/vspec/ptr.rs:248. Kind: external_body. Category: raw pointer (internal).
Discharged by Kani and `core`'s rules for references (Translation).

## Contract

At commit 4f75a5b7 (added by ceiling2's LOCALPTR package, for
`mallctl`'s `copy_prefix`):

```rust
pub fn ref_ptr<T: Copy>(v: &T) -> (r: *const T)
    ensures
        valid_reads(r, vstd::layout::size_of::<T>() as int),
        r@.addr + vstd::layout::size_of::<T>() <= usize::MAX,
```

The body Verus does not see, `#[inline(always)]`:
`core::ptr::from_ref(v)`. Its one caller is `mallctl`'s `copy_prefix`,
which casts the result to `*const u8` and, when the `n <=
size_of::<T>()` bytes from it do not overlap the caller's buffer,
copies them there with `ptr::copy_nonoverlapping`.

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::ptr_ref_ptr` (`src/kani/ptr.rs`):
for a `T` behind a reference, in three places (a local, the middle
element of a three-element array, and the middle field of a
`(u8, T, u16)`), with every value of `T` (`kani::any()`), `ref_ptr`
returns the referent's address; adding `size_of::<T>()` to that address
does not overflow (`checked_add` is `Some`); each of the
`size_of::<T>()` bytes from the result reads as a `MaybeUninit<u8>`;
`copy_nonoverlapping` of the `size_of::<T>()` bytes into a
`MaybeUninit<T>` gives back the value; and so does `r.read()`. The
types: `u8`, `bool`, `u32`, `u64`, `usize`, the padded pair
`(u8, u64)`, `[u8; 3]` and `()`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::ptr_ref_ptr` (both quanta;
`--harness` takes the full path).

A control (run by hand, not kept): with the byte loop running to
`size_of::<T>()` inclusive, one byte past the range, the harness fails
with "dereference failure: pointer outside object bounds".

## Translation

- **`ensures valid_reads(r, size_of::<T>())`.** `valid_reads` bounds the
  length by the uninterpreted `rw_len` or `r_len` of the pointer's
  provenance and address ("the bytes may be read"; `vspec/ptr.rs`).
  Here: the `size_of::<T>()` bytes from `r` lie inside the object the
  reference points into, so a read of them (bytewise, or as the one
  copy `copy_prefix` makes) is defined. Kani fails a harness on an
  access outside the object a pointer is derived from, and the
  harness reads every byte of the range, through the result, at a
  referent that is a whole object and at two inside larger objects.
  The fact behind it is `core`'s, for every `T`: a `&T` is
  dereferenceable for `size_of::<T>()` bytes (the reference's validity
  invariant, the `core::ptr` documentation's "Safety"), and
  `from_ref` keeps the reference's address and provenance (it is the
  coercion `r as *const T`). Only reads are claimed: the disjunction
  does not let a proof pick `rw_len`, so it gives no writable range.
  The bytes are not claimed initialised: padding (the pair's) is read
  as `MaybeUninit<u8>` and copied, as `copy_prefix` copies it, never
  read as an integer. Nor is it claimed that the bytes stay unchanged:
  `T: Copy` keeps out `UnsafeCell`s and atomics (`Copy` is not
  implemented for them), so no other thread writes them while the
  shared borrow lives, and the contract says nothing of their values
  (`copy_prefix` copies them, whatever they are).
- **`ensures r@.addr + size_of::<T>() <= usize::MAX`.** The harness
  checks the sum does not overflow. The fact behind it is `core`'s: the
  bytes of a reference's referent lie in one allocation, and an
  allocation never wraps around the edge of the address space (the
  `core::ptr` documentation, "Allocation"); Kani's object model keeps
  each object below the top of the address space likewise. (The
  contract's `<=` is the non-wrapping of the range's end, which may
  itself be `usize::MAX`: the bound is on the end, not a byte.)
- **No `requires`; returns.** `from_ref` is a coercion, which no `T`
  can make panic; the harness reaches the end for every input.
- **Nothing is claimed about which memory.** The `ensures` does not
  relate `r` to `v` (no permission, no value). The harness checks the
  address is the referent's, beyond the contract.

Gap: generic over `T: Copy`; checked at the eight types above. The
body is the one coercion whatever `T` is, and the claim is the
reference's own validity, so the instances only limit the sizes and
layouts seen. A zero-sized `T` (`()`) gives an empty range, valid at
any non-null aligned address.

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

Written 2026-10-07 at commit 4f75a5b7 (REFPTR): the contract above is
the ledger's entry there, and the harness was compared with each
clause. A fresh-context subagent reviewed this record against the
harness and the contract; its findings are folded in.

Run at commit 4f75a5b7, 2026-10-07, with Kani 0.68.0 under both
quanta: `kani_proofs::ptr::ptr_ref_ptr` passes (0 of 848 checks failed in each; 9 unreachable, all branches of `core` and Kani models that the inputs do not take, such as a zero-sized `offset_from`, and one array bound check).

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
