# `mark_bound`

Seam S125 of `verus/SEAMS.md`: src/heap/mod.rs:6391. Kind: external_body. Category: raw pointer (internal).
Discharged by RefinedRust.

## Contract

At commit 0c9cd77:

```rust
unsafe fn mark_bound(h: *mut Heap, Tracked(s): Tracked<&mut FreshHeap>)
    requires
        old(s).heap() == h,
        heap_head(h, old(s).head()),
    ensures
        final(s).heap() == h,
        final(s).head() == old(s).head(),
        final(s).arena() == old(s).arena(),
        final(s).bound(),
```

`heap_head(h, head)` is `head.is_range(h@.addr, 128)`, `head.provenance()
== h@.provenance`, `h@.addr != 0` and `h@.addr % 64 == 0`. The body:

```rust
unsafe { (*h).bound = true };
```

## Statement

RefinedRust (refinedrust-dev 5846daa, frontend patched with
`verus/refinedrust/frontend.patch`; Rocq 9.2.0, Iris 4a6add1) proves,
with the specification on the function

```rust
#[cfg_attr(refinedrust, rr::params("l" : "loc", "sh", "tid", "ar", "st", "lg", "un", "ot", "sn", "te" : "val", "b" : "bool"))]
#[cfg_attr(refinedrust, rr::args("l", "()"))]
#[cfg_attr(refinedrust, rr::requires(#type "l" : "HeapHead_fresh_rfn sh tid ar st lg un ot sn te b" @ "HeapHead_fresh heap_ThreadStats_st"))]
#[cfg_attr(refinedrust, rr::ensures(#type "l" : "HeapHead_fresh_rfn sh tid ar st lg un ot sn te true" @ "HeapHead_fresh heap_ThreadStats_st"))]
```

the frontend's lemma for that specification, named in the line below.

RefinedRust proof: `heap_mark_bound_proof` (verus/refinedrust/proofs/proof_heap_mark_bound.v)

The lemma states that the function, called with `h = l` and the unit
`s`, and with ownership of a `HeapHead` at `l`
(`verus/refinedrust/theories/heads.v`: a `repr(C)` struct of `Heap`'s
fields from `shared` to `bound`, its first ten) whose `bound` holds the
`bool` `b` and whose other nine fields hold the bytes `sh`, `tid`,
`ar`, `st`, `lg`, `un`, `ot`, `sn`, `te` (`value_t false`: any bytes of
the field's layout, initialised or not, named by their value), returns
without unwinding and gives back ownership of the same head with those
nine fields holding the same bytes and `bound` holding `true`. `Print
Assumptions` names only `ProofIrrelevance.proof_irrelevance` and
`FunctionalExtensionality.functional_extensionality_dep`.

The proof types the field place `(*h).bound` through the head
(`typed_place_member_prefix` and `struct_prefix_offset`,
`verus/refinedrust/theories/struct_prefix.v`, as for `node_link`); the
store of `true` into the head's `bool` field is RefinedRust's own rule,
and the rest is RefinedRust's automation.

Reproduce: `sh verus/refinedrust/run.sh` (both quanta).

## Translation

The contract speaks of the fresh heap through the linear token
`FreshHeap` (S121, `none (assumption)`): `s.heap()` is the heap,
`s.head()` the permission to its 128 bytes of plain fields, and, by the
token's trusted meaning ("what the trusted writers stored in them"),
`s.arena()` and `s.bound()` are what `init_fields` and `mark_bound`
stored in the heap's `arena` and `bound` fields. `init_fields` stores
them (it ensures `s.arena()` is the arena it stored and `!s.bound()`),
and `tcaches::heap_new` reads them back (`r.arena as int == s.arena()`,
`r.bound == s.bound()`). The translation uses that meaning to read
`s.heap()` and `s.head()` as the heap at `l` and the permission to its
head, `ar` as the bytes of `s.arena()` and `b` as `s.bound()`.

It also uses a premise the token's documentation does not state:
that the fields still hold what was stored when `mark_bound` runs,
because nothing else writes the head's first ten fields between
`init_fields` and `heap_new`. By inspection: the token is the only
holder of the head's permission, `create_detached` writes the rest of
the block through the other permission (`rest`), and `create` calls
`mark_bound` right after `create_detached` returns. Since the port of
upstream #103, `create` then calls `mark_rehome` (S124, discharged by
RefinedRust, `heap-mark_rehome.md`), which stores `true` into `rehome`
alone: a field after `rng`, at byte 88 under rustc's layout, outside
`HeapHead` (whose fields end at byte 74, below), so it leaves the bytes
this proof is about as they were (its proof gives back every other
field before `caches` with its bytes).

- `requires old(s).heap() == h`: `h = l`.
- `requires heap_head(h, old(s).head())`: Verus's permission for the
  128 bytes at `h`, `h` non-null and 64-aligned. RefinedRust's
  precondition owns the first `size_of(HeapHead)` of them: nine fields
  of any bytes, and `bound` holding a valid `bool` (`b`, which is
  `old(s).bound()` by the token's meaning and the premise above;
  `init_fields` stored `false`). It is the only part of the
  precondition that asks more of the bytes than the permission does,
  and it asks it of a field the token records. (With `bound` as any
  bytes, `value_t false BoolSynType`, RefinedRust's automation does not
  type the store: that type's syntactic type is `UntypedSynType
  BoolSynType`, `bool_t`'s is `BoolSynType`, and RefinedRust's update
  of a struct field to a new type asks the two syntactic types to be
  equal.) Under rustc's layout `HeapHead`'s fields end at byte 74 and
  it is 80 bytes with alignment 8 (`arena` at 12, `others` at 48,
  `tcache_enabled` at 72, `bound` at 73, `rng` at 80, `ThreadStats` 16
  bytes with alignment 8; checked with `const` assertions in a scratch
  build of 0c9cd77 on rustc 1.95, both quanta), inside the 128 and
  aligned by `h`'s 64-byte alignment, so the precondition is implied by
  the contract's together with the token's meaning and the premise
  above. RefinedRust's statement holds for every layout algorithm
  meeting its axioms, which leave `repr(Rust)` structs such as
  `ThreadStats` abstract and fix `repr(C)` structs to the C algorithm.
  rustc's layout is not such an algorithm as a whole (RefinedRust's
  `repr(C)` ignores `align(64)`, so its `Heap` differs from rustc's
  after `rng`), but the algorithm that lays out `ThreadStats` as rustc
  does (16 bytes, align 8) gives `HeapHead` and the offsets of its
  fields in `Heap` exactly rustc's values, which are all the statement
  depends on.
- `ensures final(s).heap() == h`: the token is about the same heap.
- `ensures final(s).head() == old(s).head()`: the permission, a range
  and a provenance with no contents, is unchanged. RefinedRust's
  postcondition gives back ownership of the same head, and the body
  owns nothing else, so the remaining 48 bytes and every other location
  are untouched. The head's padding (bytes 14..16, 20..24 and 74..80)
  is owned but not stated unchanged; it is padding in `Heap` too.
- `ensures final(s).arena() == old(s).arena()`: `arena` holds the same
  bytes `ar`, so the same `u16`.
- `ensures final(s).bound()`: `bound` holds `true`.
- Beyond the contract: the other eight head fields hold the same bytes,
  so the body does not change `others`. `tcaches::heap_new`'s trusted
  `r.others@.addr == 0` needs that, and it rests as well on
  `init_fields` storing null (trusted) and on the premise above.
- `s` (`Tracked<&mut FreshHeap>`) is zero-sized outside Verus; the
  patched frontend translates it as unit.
- Gaps:
  - The token's meaning (S121) and the no-other-writer premise above
    are what this rests on; the proof shows the body keeps them, not
    that they hold.
  - Verus's permission (`PointsToRaw`: a range and a provenance) is
    read as RefinedRust's exclusive ownership of those bytes in its
    own memory model (an allocation in bounds, owned by the caller,
    the frame carrying back to Verus's other permissions). The two
    logics are not linked by a proof; this correspondence is assumed,
    as for every RefinedRust record.
  - Layout: the offsets RefinedRust computes for the head are those of
    `Heap`'s own fields in its C layout, equal to rustc's under the
    premise above. The crate asserts only `offset_of!(Heap, caches) ==
    128`; the offsets above were checked once, not on every build. A
    change to the fields up to `bound` makes the proof fail
    (`struct_prefix_offset` asks `HeapHead_sls`'s fields to be `Heap`'s
    first ones); a change to `ThreadStats`' size or alignment does not.
    RefinedRust ignores `repr(align(64))` on `CacheHeader`, which only
    moves fields after `rng`, which the proof never touches.
  - The MIR checked is nightly-2026-08-28's (rustc 1.100, unoptimised,
    overflow checks on), translated from the code `rustc` 1.95/1.98
    builds; the frontend's translation of MIR to Radium and the patch
    are trusted.
  - Iris's weakest precondition is partial correctness: termination is
    not part of the statement. The body is a single store.
  - x86_64 only (the frontend's target); both quanta.

## Review

The first version of this record (at 8d0621a, against the contract
`heap_head(h, *old(head))` / `*final(head) == *old(head)`) was reviewed
on 2026-10-04 by a fresh-context reviewer (a Claude subagent with no
prior context of this work), told to look for translations that
overclaim; its findings (wording about rustc's layout, and `run.sh`
gaps) were fixed then. The re-stated specification against the
`FreshHeap` contract was reviewed on 2026-10-04 by another fresh-context
reviewer with the same brief. It found the reliance on the token an
honest correspondence and the layout offsets right, and found that the
first re-statement left the other eight head fields as `uninit`, so it
did not show the body leaves `others` alone, which `heap_new`'s trusted
claim needs: the specification now names and returns their bytes
(`value_t false`); and that the record should say why the head is
still what the token records at `mark_bound` and why `bound` must hold
a `bool` beforehand: both are stated above. A third fresh-context
reviewer, with the same brief, found the translation sound and asked
for the reason `bound` must hold a `bool` to be stated in terms of
syntactic types, for the no-other-writer premise to be named apart
from the token's documented meaning, for the gap between Verus's
permissions and RefinedRust's ownership and the unasserted layout to
be named, and for the padding to be mentioned; all are above. A human
review is pending.

## Tests

`verus/refinedrust/run.sh` fails when this contract changes (the record
quotes it), when the `rr::` attributes on the function differ from the
ones quoted above, when the body or the struct fields the specification
owns change so that the proof no longer checks, when the proof's
statement is not the frontend's lemma, and when the proof rests on any
other axiom. Checked by hand on 2026-10-04: an `ensures` that keeps
`bound` as `b`, one that gives `arena` the bytes of `tid`, and one that
gives `others` the bytes of `shared` each leave the proof with an
unprovable goal (`true = b`, `ar = tid`, `ot = sh`).
