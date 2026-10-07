# `addr_to_ptr`

Seam S262 of `verus/SEAMS.md`: src/vspec/addr.rs:26. Kind: external_body. Category: raw pointer (internal).
Discharged by Kani, except the provenance clause (Translation), which
stays an assumption.

## Contract

At commit f079fe76:

```rust
pub fn addr_to_ptr(addr: usize) -> (p: *mut u8)
    ensures
        p as usize == addr,
        p == addr_ptr(addr),
        p@.provenance == cast_provenance(),
    opens_invariants none
```

## Statement

Kani 0.68.0 (CBMC 6.11.0, nightly-2026-08-21) proves the harness `kani_proofs::ptr::addr_to_ptr_is_the_cast` (`src/kani/ptr.rs`):
for every `addr: usize` (symbolic), `addr_to_ptr(addr)` returns,
without a panic, a pointer `p` with `p as usize == addr` and
`p.addr() == addr`, equal (`==`) to `addr as *mut u8`, and a second
call returns a pointer equal to `p`.

Reproduce: `sh verus/kani/run.sh --harness kani_proofs::ptr::addr_to_ptr_is_the_cast` (both quanta;
`--harness` takes the full path).

## Translation

- No `requires`: the harness covers every address. The body is the
  cast alone, so it never panics and opens no invariant (`opens_invariants
  none`).
- `ensures p as usize == addr`: asserted.
- `ensures p == addr_ptr(addr)`: `addr_ptr` is an uninterpreted spec
  function, read as the cast: `addr_ptr(a)` is the pointer `a as *mut
  u8` yields, the pointer with address `a`, provenance
  `cast_provenance()` and no metadata. The harness shows the body is
  that cast and returns one pointer per address (pointer `==` compares
  addresses; the provenance is the next clause's).
- `ensures p@.provenance == cast_provenance()`: not checked here; it
  stays an assumption, this record's residual. It is the reading of
  `cast_provenance` the contract states: an integer-to-pointer cast
  (`as`, Rust's exposed-provenance cast) names no allocation of its own,
  and Verus gives every such cast one abstract provenance, the one the
  permissions minted for `mmap`'d memory carry
  (`vspec::unbacked::Unbacked::commit` and `into_lazy`, seams of their
  own, trusted). The assumption is that a pointer cast from an address
  in a mapping challoc made may access that mapping through those
  permissions, as Rust's exposed-provenance rules are understood to
  allow for memory a system call mapped (the cast may pick that
  allocation's provenance); nothing here checks it. This record
  discharges the body (the cast, its address, its determinism); the
  provenance clause is not discharged.

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

Written on 2026-10-05 against commit f079fe76 (the contract above is
the ledger's entry there), and the harness run under both quanta with
the Kani above (it passes). Fresh-context subagent, 2026-10-05: the
harness against the contract and this translation; its findings are
folded in.

## Tests

`sh verus/kani/run.sh` (`docs/testing.md`, "Kani") runs the
harness under both quanta, after checking that the contract above
is still the ledger's entry for this seam: a body that breaks the
contract fails the harness, and a changed contract fails that
check until this record and its harness are revisited. The script
is run by hand (it needs Kani's own toolchain,
`verus/kani/setup.sh`); no CI job runs it yet.
