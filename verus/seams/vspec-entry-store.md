# `store` (`vspec/entry.rs`)

Seam S307 of `verus/SEAMS.md`: src/vspec/entry.rs:74. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At the commit that adds it (package P8, ARENAS):

```rust
pub fn store<T>(a: &AtomicPtr<T>, v: *mut T, ordering: Ordering, Tracked(perm): Tracked<&mut EntryPerm<T>>)
    requires
        old(perm).id() == entry_id(a),
        !(ordering is Acquire),
        !(ordering is AcqRel),
    ensures
        final(perm).id() == old(perm).id(),
        final(perm).value() == v,
    opens_invariants none
    no_unwind
```

The body: `a.store(v, ordering)`. Its call sites, each a table entry
of the arena registry (`Arenas::table`): `Arenas::put_arena0`
(`Release`, entry 0 gets arena 0 while `Arenas::init` builds the
registry, before it is published) and `Arenas::build` (`Release`, under
the creation lock, inside the registry's invariant `arena::NarenasPred`:
a null entry gets the arena `Arena::init` just built).

## Statement

The same two parts as `vspec-count-store.md`, for one atomic pointer
rather than one counter.

1. **C11, per atomic.** C++20's coherence for one atomic object (the
   statement in `vspec-count-store.md`, part 1, holds for any atomic
   type): the accesses to one table entry can be put in one total order,
   consistent with happens-before, in which every read reads the last
   write before it. `core`'s `AtomicPtr::store` panics for `Acquire` and
   `AcqRel` and is otherwise one atomic store.
2. **loom.** `src/loom_tests/narenas.rs` runs the real wrappers of
   `vspec/entry.rs` (`store`, `load`, compiled over loom's `AtomicPtr`)
   beside those of `vspec/count.rs`, with the orderings `arena.rs`
   passes: `create` (copied from `Arenas::create` and `build`) writes the
   new arena's fields, then stores its entry with `store(.., Release)`
   under the creation lock, and the scan (`Arenas::iter`, the loads of
   `get`, `get_existing` and `get_frozen`) loads every entry below the
   count `Acquire` and reads the fields of each non-null one; before it,
   the probe (`Arenas::get` for an index known without the count, as a
   `MALLOCX_ARENA` index is) loads every entry `Acquire` and reads the
   fields of each non-null one. The
   configuration is `vspec-count-store.md`'s (two creators, one scanner,
   `MAX_ARENAS` 3, arena 0 built from the start). Exhaustive:

Models: `loom_narenas_create_publishes`, `loom_narenas_relaxed_store_is_unpublished`, `loom_narenas_relaxed_entry_races`

   - `loom_narenas_create_publishes`: a probe and a scan read every published
     arena's fields with no race (the entry's `Release` store orders
     `build`'s writes before a reader's `Acquire` load of it), a thread
     holding the creation lock finds every explicit arena below the
     count built, and the checks of `vspec-count-store.md`.
   - Controls: `loom_narenas_relaxed_entry_races` (the entry stored
     `Relaxed`) fails: the probe (`Arenas::get` for an index known without
     the count) reads the arena's fields racing with `build`'s writes,
     which loom reports ("Causality violation"), so the entry's `Release`
     store is the edge the readers need; `loom_narenas_relaxed_store_is_unpublished`
     (the count stored `Relaxed`) shows the scan's built check has teeth.

## Translation

- **`requires old(perm).id() == entry_id(a)`**: the permission is for
  this entry; part 1 is about that atomic alone.
- **`requires !(ordering is Acquire)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `store` panics; the call sites pass
  `Release`.
- **`ensures`.** Read `perm.value()` as the value of the last write to
  the entry in its modification order (every write of an entry goes
  through this wrapper: `put_arena0` and `build`; `Arenas::init` makes
  each entry with the verified `vspec::entry::new` before the registry
  is published). A store is a write: after it the value is `v` until the
  next write. Every write after `init` is under the creation lock, whose
  release/acquire edge orders the stores in happens-before and so in
  modification order; and `build` stores only into an entry the
  snapshot has no arena at (Verus proves it), so each entry is written
  at most once after `init`.
- **`opens_invariants none`**: read from the source.
- **What the SC proof carries across locations.** `NarenasPred` relates
  the count, every entry and ghost state (the creation lock's snapshot,
  the frozen arena permissions): each non-null entry is a built arena at
  its index, and the snapshot names exactly the non-null entries. Under
  SC, the invariant holds at every step. Under C11 the facts a reader
  uses are per location or carried by happens-before: a reader that
  loads a non-null entry dereferences the arena, whose initialisation
  (`Arena::init`, plain writes through its permission) must happen
  before the read; the `Release` store and the reader's `Acquire` load
  give that edge, which the model checks (loom reports the race
  otherwise). The snapshot is ghost; the fork handlers' reads under the
  creation lock (`api::count_held`, `api::arena_held`) read locations
  last written by an earlier holder of that lock, ordered by its edge,
  which the model checks in `create`.
- **Gaps.** Those of `vspec-count-store.md`: one scanner, two creators,
  `MAX_ARENAS` 3, `build` cannot fail, the lazy build of automatic
  arenas (`Arenas::materialise`) and the fork handlers' reads are not
  modelled; the arena's fields are one loom `UnsafeCell`; the `Arenas`
  steps are copies (pinned, `verus/loom/pins.txt`). The loom model's
  common part (memory model, exploration, futex stand-in, copies) is the
  one stated in `vspec-count-store.md`.

## Review

Fresh-context subagent, 2026-10-07 (package P8): the wrappers against
`count.rs`'s, the model and this translation. Its findings are folded
in: the original control relaxed only the count's store, so nothing
showed the entry's `Release` store has teeth; the probe and
`loom_narenas_relaxed_entry_races` were added, and the run.sh name check
now also covers writes through `elem_at(table, ..)`.

## Tests

`sh verus/loom/run.sh` (`docs/testing.md`, "loom model checks"): the
record checks of its first part, then every model; see
`vspec-count-store.md`.
