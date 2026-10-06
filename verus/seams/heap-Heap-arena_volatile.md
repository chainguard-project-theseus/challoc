# `Heap::arena_volatile`

Seam S119 of `verus/SEAMS.md`: src/heap/mod.rs:4603. Kind: external_body. Category: UnsafeCell/volatile/transmute.
Discharged by RefinedRust.

## Contract

At commit 0c9cd77 (unchanged since 8d0621a):

```rust
pub(super) fn arena_volatile(&self) -> (r: u16)
    ensures
        r == self.spec_arena(),
    opens_invariants none
```

`spec_arena` is `self.arena` (a closed spec fn of `Heap`). The body:

```rust
unsafe { ptr::read_volatile(&raw const self.arena) }
```

## Statement

RefinedRust (refinedrust-dev 5846daa, frontend patched with
`verus/refinedrust/frontend.patch`; Rocq 9.2.0, Iris 4a6add1) proves,
with the specification on the function

```rust
#[cfg_attr(refinedrust, rr::params("l", "sh" : "loc", "tid" : "Z", "ar" : "Z"))]
#[cfg_attr(refinedrust, rr::args("l" @ "alias_ref_t"))]
#[cfg_attr(refinedrust, rr::requires(#iris "l ◁ₗ[π, Shared ulft_1] #(-[#sh; #tid; #ar]) @ ◁ HeapArenaHead_ty"))]
#[cfg_attr(refinedrust, rr::returns("ar"))]
```

the frontend's lemma for that specification, named in the line below.

RefinedRust proof: `heap_Heap_arena_volatile_proof` (verus/refinedrust/proofs/proof_heap_Heap_arena_volatile.v)

The lemma states that the function, called with `self` the address `l` and
shared ownership, for the lifetime `ulft_1` of `&self` (which outlives
the call), of a `HeapArenaHead` at `l` holding `sh`, `tid`, `ar`,
returns `ar` and does not unwind. `HeapArenaHead`
(`verus/refinedrust/theories/heads.v`) is a `repr(C)` struct of `Heap`'s
first three fields, `shared: *const Shared`, `tid: u32`, `arena: u16`
(offsets 0, 8, 12; 16 bytes with padding). The lemma takes the
stdlib's specification of `core::ptr::read_volatile`;
`verus/refinedrust/callees/callees.v` checks the stdlib's proofs of
`read_volatile` (which calls `ptr::read`) and of `ptr::read` (Radium's
typed copy). `verus/refinedrust/proofs/statements.v` checks that the
proof has exactly the lemma's statement, and `Print Assumptions` of
each of the three proofs names only `ProofIrrelevance.proof_irrelevance`
and `FunctionalExtensionality.functional_extensionality_dep`.

The proof applies two lemmas proved here. `type_raw_shr`
(`verus/refinedrust/theories/raw_shr.v`) types `&raw const self.arena`,
which RefinedRust has no rule for (only `&raw mut`), as a shared borrow
at `self`'s lifetime whose result is used as a raw pointer. The two are
the same Radium expression, `SkipE (SkipE e)`. `typed_place_member_prefix`
(`verus/refinedrust/theories/struct_prefix.v`) types the projection of
`Heap`'s `arena` through `HeapArenaHead`, which has the same offset for
it (`struct_prefix_offset`, proved for any `repr(C)` struct and a
`repr(C)` struct of its first fields from RefinedRust's C layout
algorithm). Everything else is RefinedRust's automation.

Reproduce: `sh verus/refinedrust/run.sh` (both quanta).

## Translation

- No `requires`. RefinedRust's precondition is shared ownership, for
  `&self`'s lifetime, of the heap's first 16 bytes as `HeapArenaHead`:
  bytes 0..8 any pointer value, 8..12 an initialised `u32`, 12..14 an
  initialised `u16`, 14..16 padding (any bytes). Verus's `&self` is a
  live `&Heap`, and under rustc's layout `Heap`'s `shared`, `tid` and
  `arena` sit at 0, 8 and 12 (`repr(C)`; checked with `offset_of!` on
  rustc 1.95) and hold valid values of their types (Rust's validity of
  `&Heap`), which no one writes while the `&Heap` lives (no interior
  mutability in them). Reading a Rust shared reference as RefinedRust
  shared ownership is RefinedRust's own premise for safe Rust callers.
  The precondition owns nothing past byte 16, so the difference between
  RefinedRust's layout of the rest of `Heap` and rustc's (RefinedRust
  ignores `repr(align(64))` on `CacheHeader`) does not enter it.
- `ensures r == self.spec_arena()`: RefinedRust's return clause, the
  value returned is the `arena` field's, `ar`. Exact. `unwind False`:
  the call does not panic.
- `opens_invariants none`: the body has no ghost code and opens no
  Verus invariant; nothing to translate.
- Gaps:
  - Volatility is not modelled: RefinedRust's stdlib proves
    `read_volatile` as a plain `ptr::read`. The contract says nothing
    about volatility (why the body uses it is codegen, see the doc
    comment).
  - The MIR checked is nightly-2026-08-28's (rustc 1.100, unoptimised,
    overflow checks on), translated from the code `rustc` 1.95/1.98
    builds; the frontend's translation of MIR to Radium, the patch
    (named-constant array lengths, array refinements, Verus's
    `Ghost`/`Tracked` as unit: zero-sized, like unit), and the
    composition of the three proofs through function specifications
    (RefinedRust's adequacy, not instantiated for this crate) are
    trusted.
  - Iris's weakest precondition is partial correctness: termination is
    not part of the statement. The body is straight-line code.
  - x86_64 only (the frontend's target); both quanta.

## Review

Translation reviewed on 2026-10-04 by a fresh-context reviewer (a
Claude subagent with no prior context of this work), told to look for
translations that overclaim. It found that the first version took
shared ownership of the whole `Heap` in RefinedRust's layout, whose
`CacheHeader` array (no `align(64)`) overlaps real padding and pointer
bytes, so no real heap met it. The specification now shares only the
first 16 bytes. Its other findings (the `rr::` attributes pinned by
`run.sh`, proof statements checked, stricter axiom parsing) are folded
into `run.sh`. A human review is pending.

## Tests

`verus/refinedrust/run.sh` fails when this contract changes (the record
quotes it), when the `rr::` attributes on the function differ from the
ones quoted above, when the body or `Heap`'s first three fields change
so that the proof no longer checks, when the proof's statement is not
the frontend's lemma, and when the proof rests on any other axiom.
