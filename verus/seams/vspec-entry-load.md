# `load` (`vspec/entry.rs`)

Seam S308 of `verus/SEAMS.md`: src/vspec/entry.rs:93. Kind: external_body. Category: atomics primitive.
Discharged by loom and the C11 coherence rules (part 3).

## Contract

At the commit that adds it (package P8, ARENAS):

```rust
pub fn load<T>(a: &AtomicPtr<T>, ordering: Ordering, Tracked(perm): Tracked<&EntryPerm<T>>) -> (r: *mut T)
    requires
        perm.id() == entry_id(a),
        !(ordering is Release),
        !(ordering is AcqRel),
    ensures
        r == perm.value(),
    opens_invariants none
    no_unwind
```

The body: `a.load(ordering)`. Its call sites, each `Acquire`, each a
table entry of the arena registry inside its invariant
(`arena::NarenasPred`): `Arenas::get`, `Arenas::materialise` (under the
creation lock) and `Arenas::get_frozen` (the fork handlers, under the
creation lock). `Arenas::assign` keeps a plain `core` load. Package
TSLOT added `Arenas::get_existing`, outside the creation lock as
`get` is: the same `Acquire` load, so the same statement covers it,
and the trusted `arena::arena_at` it read through is gone.

## Statement

As `vspec-entry-store.md`: C11 coherence for one atomic pointer (part
1), and the loom model `src/loom_tests/narenas.rs`, which runs this
wrapper (`Arenas::entry`, `Acquire`) for every scan and every check
under the creation lock (part 2). Exhaustive:

Models: `loom_narenas_create_publishes`, `loom_narenas_relaxed_store_is_unpublished`, `loom_narenas_relaxed_entry_races`

   - `loom_narenas_create_publishes`: every non-null entry a scan loads
     is an arena whose fields it reads with no race, and a thread holding
     the creation lock loads every explicit arena below the count as
     built (the frozen table).
   - Controls: those of `vspec-entry-store.md`; in
     `loom_narenas_relaxed_entry_races` the `Acquire` loads of the probe
     find the entry without its `Release` store and loom reports the
     race on the arena's fields.

## Translation

- **`requires perm.id() == entry_id(a)`**: the permission is for this
  entry.
- **`requires !(ordering is Release)`, `!(ordering is AcqRel)`**: the
  two orderings for which `core`'s `load` panics; the call sites pass
  `Acquire`.
- **`ensures r == perm.value()`**: `perm.value()` is the value of the
  last write to the entry in its modification order (as in
  `vspec-entry-store.md`); part 1 places the load right after the write
  it reads. Under the creation lock (`materialise`, `get_frozen`) that
  is the last write, every write being made under the lock and ordered
  by its edge; outside it (`get`, `get_existing`) a load may read an
  older write, which for an entry is null (an entry is written at most
  once after `Arenas::init`, null to an arena): `get` then takes its
  slow path (`materialise`), which reloads under the lock for an
  automatic index and returns `None` for an explicit one, as the plain
  load before this wrapper did; `get_existing` returns `None`, as its
  plain load did, and its `ensures` says nothing of `None`. The SC
  proof's claim, that a non-null result is the arena the invariant
  names, is what the callers rely on, and a stale null never yields a
  wrong arena.
- **`opens_invariants none`**: read from the source.
- **Gaps.** Those of `vspec-entry-store.md`.

## Review

Fresh-context subagent, 2026-10-07 (package P8): the wrappers against
`count.rs`'s, the model and this translation. Its findings are folded
in: the original control relaxed only the count's store, so nothing
showed the entry's `Release` store has teeth; the probe and
`loom_narenas_relaxed_entry_races` were added, and the run.sh name check
now also covers writes through `elem_at(table, ..)`.

## Tests

`sh verus/loom/run.sh`; see `vspec-count-store.md`.
