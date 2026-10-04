# `node_link`

Seam S125 of `verus/SEAMS.md`: src/heap/others.rs:110. Kind: external_body. Category: raw pointer (internal).
Discharged by RefinedRust.

## Contract

At commit 0c9cd77 (unchanged since 8d0621a):

```rust
unsafe fn node_link(node: *mut ArenaCaches, arena: u16, pad: u8, next: &*mut ArenaCaches, Tracked(head): Tracked<PointsToRaw>)
    requires
        head.is_range(node@.addr as int, 64),
        head.provenance() == node@.provenance,
        node@.addr % 64 == 0,
```

No `ensures`; `head` is used up. The body:

```rust
unsafe {
    ptr::write(&raw mut (*node).arena, arena);
    ptr::write(&raw mut (*node).pad, pad);
    ptr::write(&raw mut (*node).next, *next);
}
```

## Statement

RefinedRust (refinedrust-dev 5846daa, frontend patched with
`verus/refinedrust/frontend.patch`; Rocq 9.2.0, Iris 4a6add1) proves,
with the specification on the function

```rust
#[cfg_attr(refinedrust, rr::params("l" : "loc", "a" : "Z", "p" : "Z", "nx" : "loc"))]
#[cfg_attr(refinedrust, rr::args("l", "a", "p", "nx", "()"))]
#[cfg_attr(refinedrust, rr::requires(#type "l" : "ArenaCachesHead_rfn" @ "ArenaCachesHead_uninit"))]
```

the frontend's lemma for that specification, named in the line below.

RefinedRust proof: `heap_others_node_link_proof` (verus/refinedrust/proofs/proof_heap_others_node_link.v)

The lemma states that the function, called with `node = l`, `arena = a`,
`pad = p`, `next` a shared reference (live for the call) to the pointer
`nx`, and the unit `head`, and with ownership of the 16 bytes at `l` as
an `ArenaCachesHead` (`verus/refinedrust/theories/heads.v`: a `repr(C)`
struct of `arena: u16`, `pad: u8`, `next: *mut _`, the first three
fields of `ArenaCaches`) whose fields hold any bytes (`uninit`), returns
without unwinding (no postcondition: the ownership is used up), given
the stdlib's specification of `core::ptr::write`. The stdlib's proof
of `ptr::write` is checked with it (`verus/refinedrust/callees/`).
`Print Assumptions` names only `ProofIrrelevance.proof_irrelevance` and
`FunctionalExtensionality.functional_extensionality_dep`.

The proof types each field place `(*node).f` with the lemma
`typed_place_member_prefix` (`verus/refinedrust/theories/struct_prefix.v`,
proved here): projecting field `f` of `ArenaCaches` is the same Radium
expression as projecting it in `ArenaCachesHead`, because a `repr(C)`
struct whose fields are the first fields of another has the same offset
for each of them (`struct_prefix_offset`, proved for any two such
structs from the definition of RefinedRust's C layout algorithm). The
rest is RefinedRust's automation.

Reproduce: `sh verus/refinedrust/run.sh` (both quanta).

## Translation

- `requires head.is_range(node@.addr, 64)`, `head.provenance() ==
  node@.provenance`, `node@.addr % 64 == 0`: Verus's permission for the
  64 bytes at `node`, of any contents. RefinedRust's precondition owns
  the first 16 of them (`ArenaCachesHead` has size 16 and alignment 8
  under the C layout, which `node`'s 64-byte alignment meets) as
  `uninit`, which holds of any bytes. The precondition is implied by the
  contract's: owning 64 bytes owns any 16 at their start.
- `next: &*mut ArenaCaches`: a shared reference, so RefinedRust takes
  the pointer it refers to as shared ownership, separate from the owned
  head. The contract does not say that `*next` lies outside the 64
  bytes; a `next` into the line the function writes through `node`
  would make the Rust call undefined (a write through a raw pointer to
  memory a live `&` refers to), which is the Rust aliasing model
  RefinedRust assumes of every caller. The one caller passes
  `&self.others`, a field of the `Heap`, not of the node.
- `head` (`Tracked<PointsToRaw>`) is zero-sized outside Verus; the
  patched frontend translates it as unit.
- No `ensures`: nothing to translate. RefinedRust proves that the body
  is memory safe given only the head, does not unwind, and leaves the
  rest of the 64 bytes and every other location untouched (it owns
  nothing else). The values stored (`arena`, `pad`, `nx`) are not part
  of the specification; the caller's claim about them (the key in
  `Heap::keys`) stays trusted elsewhere (`Heap::node_arena`, S130).
- Gaps:
  - Verus's permission (`PointsToRaw`: a range and a provenance) is
    read as RefinedRust's exclusive ownership of those bytes in its own
    memory model (an allocation in bounds, owned by the caller, the
    frame carrying back to Verus's other permissions). The two logics
    are not linked by a proof; this correspondence is assumed.
  - Layout: RefinedRust's `ArenaCaches` has the offsets of rustc's for
    the three fields written (0, 2, 8: `repr(C)`, checked against
    `offset_of!` on rustc 1.95); RefinedRust ignores `repr(align(64))`
    on `CacheHeader`, so its `caches` field starts at 16, not 64, which
    the proof never touches (the precondition does not own it).
  - The MIR checked is nightly-2026-08-28's (rustc 1.100, unoptimised,
    overflow checks on), translated from the code `rustc` 1.95/1.98
    builds; the frontend's translation of MIR to Radium, the patch, and
    the composition with `ptr::write`'s proof through its specification
    (RefinedRust's adequacy, not instantiated for this crate) are
    trusted.
  - Iris's weakest precondition is partial correctness: termination is
    not part of the statement. The body is straight-line code.
  - x86_64 only (the frontend's target); both quanta.

## Review

Translation reviewed on 2026-10-04 by a fresh-context reviewer (a
Claude subagent with no prior context of this work), told to look for
translations that overclaim. It found this translation sound apart from
wording (`mark_bound`: rustc's layout is not one of RefinedRust's
layout algorithms, now stated precisely) and gaps in `run.sh`, now
closed: the `rr::` attributes quoted in the Statement are pinned to
the source, the proof's statement is checked (`statements.v`), and the
axiom check reads every line of `Print Assumptions`. A human review is
pending.

## Tests

`verus/refinedrust/run.sh` fails when this contract changes (the record
quotes it), when the `rr::` attributes on the function differ from the
ones quoted above, when the body or the struct fields the specification
owns change so that the proof no longer checks, when the proof's
statement is not the frontend's lemma, and when the proof rests on any
other axiom.
