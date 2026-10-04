# `write_zero`

Seam S417 of `verus/SEAMS.md`: src/vspec/zero.rs:455. Kind: external_body. Category: raw pointer (internal).
Discharged by Kani.

## Contract

At commit 0c9cd77:

```rust
pub unsafe fn write_zero(p: *mut u8, n: usize) -> (z: Tracked<Zeroed>)
    requires
        n == 0 || crate::vspec::ptr::valid_writes(p, n as int),
    ensures
        z@.addr() == p@.addr,
        z@.len() == n,
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::write_zero` (`src/kani/ptr.rs`):
on an 8-byte buffer with arbitrary contents, from every offset `i` and
every `n` with `i + n <= 8`, `write_zero(p, n)` is defined and returns,
and afterwards the `n` bytes from `p` are zero and every other byte of
the buffer is unchanged; with `n == 0` it also returns on a pointer
made from any address, null included.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::write_zero` (both quanta;
`--harness` takes the full path).

## Translation

- `requires n == 0 || valid_writes(p, n)`: `valid_writes` is
  bounded by the uninterpreted `rw_len` ("the `n` bytes from `p` may be
  written"); here they lie
  in the buffer. The `n == 0` case is checked on pointers to no object.
- `ensures z@.addr() == p@.addr, z@.len() == n`: the token is ghost
  (minted by `Tracked::assume_new()` in the body, erased from builds),
  so these two equalities hold by construction; what the token means,
  and what the body is trusted for, is that the `n` bytes at `p` read
  as zero afterwards (`vspec::zero`'s `Zeroed`), which is the
  harness's assertion. The bytes stay zero only until something writes
  them; that no write happens while the token is held is the token
  protocol's premise (`Zeroed`'s comment), not this seam's.

Gap: checked on an 8-byte buffer, every in-range `(i, n)`, run as
constants (see the module comment of `src/kani/ptr.rs`). The body is
one `write_bytes` whatever the length.

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
