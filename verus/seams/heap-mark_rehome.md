# `mark_rehome`

Seam S123 of `verus/SEAMS.md`: src/heap/mod.rs:6484. Kind: external_body. Category: raw pointer (internal).
Discharged by RefinedRust.

## Contract

At commit c704a90e:

```rust
unsafe fn mark_rehome(h: *mut Heap, Tracked(s): Tracked<&mut FreshHeap>)
    requires
        old(s).heap() == h,
        heap_head(h, old(s).head()),
    ensures
        *final(s) == *old(s),
```

`heap_head(h, head)` is `head.is_range(h@.addr, 128)`, `head.provenance()
== h@.provenance`, `h@.addr != 0` and `h@.addr % 64 == 0`. The body:

```rust
unsafe { (*h).rehome = true };
```

Its one caller is `Heap::create`, right after `mark_bound` on the same
fresh heap and token.

## Statement

RefinedRust (refinedrust-dev 5846daa, frontend patched with
`verus/refinedrust/frontend.patch`; Rocq 9.2.0, Iris 4a6add1) proves,
with the specification on the function

```rust
#[cfg_attr(refinedrust, rr::params("l" : "loc", "sh", "tid", "ar", "st", "lg", "un", "ot", "sn", "te", "bd", "rg", "rf" : "val", "rh" : "bool"))]
#[cfg_attr(refinedrust, rr::args("l", "()"))]
#[cfg_attr(refinedrust, rr::requires(#type "l" : "HeapRehomeHead_fresh_rfn sh tid ar st lg un ot sn te bd rg rh rf" @ "HeapRehomeHead_fresh heap_ThreadStats_st"))]
#[cfg_attr(refinedrust, rr::ensures(#type "l" : "HeapRehomeHead_fresh_rfn sh tid ar st lg un ot sn te bd rg true rf" @ "HeapRehomeHead_fresh heap_ThreadStats_st"))]
```

the frontend's lemma for that specification, named in the line below.

RefinedRust proof: `heap_mark_rehome_proof` (verus/refinedrust/proofs/proof_heap_mark_rehome.v)

The lemma states that the function, called with `h = l` and the unit
`s`, and with ownership of a `HeapRehomeHead` at `l`
(`verus/refinedrust/theories/heads.v`: a `repr(C)` struct of `Heap`'s
fields from `shared` to `rehome_from`, its first thirteen, all those
before `caches`: `mark_bound`'s `HeapHead`, then `rng`, `rehome` and
`rehome_from`) whose `rehome` holds the `bool` `rh` and whose other
twelve fields hold the bytes `sh`, `tid`, `ar`, `st`, `lg`, `un`, `ot`,
`sn`, `te`, `bd`, `rg`, `rf` (`value_t false`: any bytes of the field's
layout, initialised or not, named by their value), returns without
unwinding and gives back ownership of the same head with those twelve
fields holding the same bytes and `rehome` holding `true`. `Print
Assumptions` names only `ProofIrrelevance.proof_irrelevance` and
`FunctionalExtensionality.functional_extensionality_dep`.

The proof is `mark_bound`'s with the longer head: it types the field
place `(*h).rehome` through the head (`typed_place_member_prefix` and
`struct_prefix_offset`, `verus/refinedrust/theories/struct_prefix.v`);
the store of `true` into the head's `bool` field is RefinedRust's own
rule, and the rest is RefinedRust's automation. `mark_bound`'s
specification, head (`HeapHead`) and proof are unchanged and still
check.

Reproduce: `sh verus/refinedrust/run.sh` (both quanta).

## Translation

The contract speaks of the fresh heap through the linear token
`FreshHeap` (S122, `none (assumption)`), as `mark_bound`'s does
(`heap-mark_bound.md`, whose reading of the token this record shares):
`s.heap()` is the heap, `s.head()` the permission to its 128 bytes of
plain fields, and, by the token's trusted meaning, `s.arena()` and
`s.bound()` are what `init_fields` and `mark_bound` stored in the
heap's `arena` and `bound` fields. The translation reads `s.heap()` and
`s.head()` as the heap at `l` and the permission to its head, `ar` as
the bytes of `s.arena()` and `bd` as those of `s.bound()`.

It uses the premise `mark_bound`'s record states, extended to the
fields after `bound`: nothing else writes the heap's first thirteen
fields between `init_fields` and `tcaches::heap_new`. By inspection:
the token is the only holder of the head's permission,
`create_detached` writes the rest of the block through the other
permission (`rest`), and `create` calls `mark_bound` and then
`mark_rehome` right after `create_detached` returns, with only ghost
code between. `init_fields` stores `false` into `rehome` (and `0` into
`rng`, `FOREIGN` into `rehome_from`), and `mark_bound`'s body touches
only `HeapHead`: its proof owns nothing else, and `rng`, `rehome` and
`rehome_from` lie outside `HeapHead` (whose fields end at byte 74; it
is padded to 80).

- `requires old(s).heap() == h`: `h = l`.
- `requires heap_head(h, old(s).head())`: Verus's permission for the
  128 bytes at `h`, `h` non-null and 64-aligned. RefinedRust's
  precondition owns the first `size_of(HeapRehomeHead)` of them: twelve
  fields of any bytes, and `rehome` holding a valid `bool` (`rh`; by the
  premise above, the `false` `init_fields` stored). That is the only
  part of the precondition that asks more of the bytes than the
  permission does, for the reason `mark_bound`'s record gives
  (RefinedRust's automation types the store into a struct field only
  when the field's syntactic type is the stored type's). Under rustc's
  layout `HeapRehomeHead`'s fields end at byte 92 and it is 96 bytes
  with alignment 8 (`HeapHead`'s offsets, then `rng` at 80, `rehome` at
  88 and `rehome_from` at 90, as in `Heap`, whose `caches` start at
  128; checked with `const` assertions on `offset_of!(Heap, ..)` for
  every field through `rehome_from` and for `caches`, and on
  `ThreadStats`' size 16 and alignment 8, in a scratch build of
  c704a90e on rustc 1.95, both quanta, a deliberately wrong offset
  failing that build), inside the 128 and aligned by `h`'s 64-byte
  alignment, so the precondition is implied by the contract's together
  with the token's meaning and the premise above. The layout argument
  of `mark_bound`'s record (RefinedRust's statement holds for every
  layout algorithm meeting its axioms; the one that lays out
  `ThreadStats` as rustc does gives this head and its offsets in `Heap`
  exactly rustc's values) carries over: `rng` (`u64`), `rehome`
  (`bool`) and `rehome_from` (`u16`) have fixed layouts.
- `ensures *final(s) == *old(s)`: the token is ghost and the body does
  not touch it (`s` is erased); what the equality asks of the memory is
  that the token's meaning still holds afterwards. RefinedRust's
  postcondition gives back ownership of the same head (the same `heap()`
  and `head()`: the permission, a range and a provenance with no
  contents, is unchanged, and the body owns nothing else, so the
  remaining 32 bytes, 96..128, and every other location are untouched),
  with `arena` holding the same bytes `ar` (the same `u16`, `s.arena()`)
  and `bound` the same bytes `bd` (`s.bound()`): what the token records
  is still what the fields hold. The head's padding (bytes 14..16,
  20..24, 74..80, 89..90 and 92..96) is owned but not stated unchanged;
  it is padding in `Heap` too.
- Beyond the contract: `rehome` holds `true`, the body's purpose, which
  the contract does not state (Verus does not track a fresh heap's
  `rehome`: `heap_new` claims nothing about it, and `Heap::homed_ok`'s
  `rehome ==> bound` holds whatever it holds, because the heap is
  bound); and the ten fields besides `rehome`, `arena` and `bound` hold
  the same bytes, so the body does not change `others` (`heap_new`'s
  trusted `r.others@.addr == 0`), `rng` or `rehome_from`.
- `s` (`Tracked<&mut FreshHeap>`) is zero-sized outside Verus; the
  patched frontend translates it as unit.
- Gaps:
  - The token's meaning (S122) and the no-other-writer premise above
    are what this rests on; the proof shows the body keeps them, not
    that they hold.
  - Verus's permission (`PointsToRaw`: a range and a provenance) is
    read as RefinedRust's exclusive ownership of those bytes in its own
    memory model, as for every RefinedRust record; the two logics are
    not linked by a proof.
  - Layout: the crate asserts only `offset_of!(Heap, caches) == 128`;
    the offsets above were checked once, not on every build. A change
    to the fields through `rehome_from` makes the proof fail
    (`struct_prefix_offset` asks `HeapRehomeHead_sls`'s fields to be
    `Heap`'s first ones); a field inserted after `rehome_from` does
    not, and the head would then no longer cover every field before
    `caches`; nor does a change to `ThreadStats`' size or alignment.
  - The MIR checked is nightly-2026-08-28's (rustc 1.100, unoptimised,
    overflow checks on), translated from the code `rustc` 1.95/1.98
    builds; the frontend's translation of MIR to Radium and the patch
    are trusted.
  - Iris's weakest precondition is partial correctness: termination is
    not part of the statement. The body is a single store.
  - x86_64 only (the frontend's target); both quanta.

## Review

Written at c704a90e, 2026-10-06, after `mark_bound`'s record. A
fresh-context reviewer (a Claude subagent with no prior context of this
work), told to look for translations that overclaim, reviewed the
specification, the head, the proof and this translation. It found that
the first head (fields through `rehome`, padded to 96 bytes) owned
`rehome_from` as padding without stating it unchanged while the record
called it untouched: the head now ends with `rehome_from`, stated
unchanged. A human review is pending.

## Tests

`verus/refinedrust/run.sh` fails when this contract changes (the record
quotes it), when the `rr::` attributes on the function differ from the
ones quoted above, when the body or the struct fields the specification
owns change so that the proof no longer checks, when the proof's
statement is not the frontend's lemma, and when the proof rests on any
other axiom. Checked by hand on 2026-10-07 (quantum16): an `ensures`
that keeps `rehome` as `rh`, one that gives `bound` the bytes of
`tcache_enabled`, and one that gives `rehome_from` the bytes of
`last_gc` each leave the proof with an unprovable goal (`true = rh`,
`bd = te`, `rf = lg`).
