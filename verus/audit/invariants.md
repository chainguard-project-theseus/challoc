# Seam audit: allocator invariants, axioms and proof seams, the audited seams still trusted

*An adversarial audit of the Verus seams of kind `axiom` or
`external_body proof` (any category) and of category allocator invariant
(any kind), made during [#138](https://github.com/wolfi-dev/challoc/pull/138). There it covered 69 seams: 28
sound, 28 sound (premise), 11 fixed, 1 weakened, 1 removed and 0
unsound-open.*

*This file lists the 36 of those seams that are still seams in the
ledger. The others are verified or no longer exist, so their rows are
left out: among them `axiom_raw_valid`, gone, and `axiom_process_shared`,
which `lemma_process_shared` (`heap/shared.rs`) now proves from the
opaque definitions of `process_regions` and `pac_mode`. Seams
of this scope that the audit did not cover, such as
`axiom_set_tables_lock_frame` and `axiom_snapshot_lock_frame`, are not
listed. Each row names the seam by its item and its ledger file,
`verus/ledger/src/<file>.md`, which gives the seam's contract, category,
tool and status.*

The verdicts are the audit's, from #138; none was re-audited at
`26f535a9`. A reason may describe the contract as it stands at
`26f535a9`, including a premise closed after the audit, but its verdict
was not judged again.

For each seam the audit asks four questions. Can it prove `false`, through
edge cases, a second call, arbitrary spec arguments or vstd's permission
lemmas? Is its `ensures` true of the body or of the world for every input
that meets its `requires`? Is it still needed? Is it stronger than its
callers need? Each derivation of `false` this file shows was run with
Verus (`cargo verus focus`, quantum 16) as a scratch proof in the seam's
module, at the audit.

Verdicts:

- **sound**: no derivation of `false` was found, and the contract is
  true for every input that meets its `requires`.
- **sound (premise)**: true at the code's own calls, under a premise
  that the seam's doc states and Verus cannot check, such as the seam
  being called once, from one place, in a stated state. A second call,
  or a call with arguments the code never passes, would prove `false`
  or an untrue fact.
- **fixed**: the audit found the contract false or unsound and corrected
  it; the reason gives the corrected contract.

Remaining seam work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).

Totals for the 36 seams listed: 16 sound, 13 sound (premise), 7 fixed.

## Per seam

| seam | ledger file | verdict | reason |
|---|---|---|---|
| `axiom_fork_resumed_state` | `api/mod.rs` | sound (premise) | One call per fork handler. A second call gives a second token for the state lock (a double unlock); no lemma separates two `LockHeld`s, so no `false`. See "Premises". |
| `axiom_fork_resumed` | `api/mod.rs` | fixed | Requires `state.held() == state_lock()`, so a token of another lock cannot become the state lock's hold; the one caller, `fork_release`, has it. Rests on the same handoff premise as `axiom_fork_resumed_state`. |
| `axiom_array_layout` | `arena.rs` | sound | Requires `N * size_of::<T>() <= isize::MAX`, which every array with a layout meets (`library-control.md`). |
| `axiom_arena_layout` | `arena.rs` | sound | The build's `const` assertion covers every clause, on every target and quantum. |
| `axiom_arena_assemble` | `arena.rs` | sound (premise) | Consumes the field permissions and the padding (`pad_covers`), so the assembled permission covers exactly the bytes it is given (`memory.md`). |
| `axiom_class_lock_inv` | `class/mod.rs` | sound | One `lock_inv` axiom for the class `Inner`; it rests on the lock premise below. |
| `axiom_class_lock` | `class/mod.rs` | sound | One `lock_frame` axiom: the slab count, `arena`, `table` and the owned-slab record's id (`oauth`). Nothing writes the first three, and only `take_group` writes the record. |
| `axiom_fresh_slots_zero` | `class/mod.rs` | fixed | Requires `n <= nslots`, a `Fresh` state ticket for the slab, and the held slot tokens of the slots it covers, so the zero claim stays inside the slab. |
| `axiom_discarded_slots_zero` | `class/mod.rs` | fixed | As `axiom_fresh_slots_zero`, for a `Purged` slab, with the `PurgedZero` witness of the call site's run-time test (`purged_is_zero`). |
| `axiom_table_lock_inv` | `class/table.rs` | sound | The initial value is verified and `take_group` asserts `inv`. `commit_run` and `commit_slab` keep every conjunct but `guards <= next_slab`, which rests on the premise the axiom's doc states: each slab below `next_slab` is reached once, by the cursor of the one arena its group went to. |
| `axiom_table_lock_frame` | `class/table.rs` | sound | Nothing writes the geometry, the marker authority (`mloc`), the descriptor array or the watermark's ghost location. |
| `store_owner` | `class/table.rs` | sound (premise) | Uses up the owner-store markers of the group's slabs (`owner_marks`), so a second store of one group does not verify. That holds while every holder of the commit lock restores `TableInner::inv` before releasing it (see "Premises"). |
| `axiom_slot_loc` | `class/table.rs` | sound (premise) | Takes the class's `SlotLoc(c)` one-shot token, so it runs once per class. |
| `axiom_fresh_words` | `class/table.rs` | fixed | Requires the words to end below `usize::MAX` and takes the class's `FreshWords(c)` token. Nothing ties `base` to the class's bitmap; a call at another address would give `WordPerm`s that no lemma separates from other permissions, so no `false`. |
| `axiom_c_claim` | `heap/live.rs` | sound (premise) | The C contract: two claims of one slot start prove `false` through `theorem_apart`, as its doc says. Every C free is its own entry, with no ghost state from the allocation that made the block. |
| `axiom_shared_layout` | `heap/shared.rs` | sound | The build's `const` assertion covers every clause. |
| `axiom_shared_built` | `heap/shared.rs` | sound (premise) | Consumes the field permissions; a second use needs a second `Shared::begin`, which does not verify. |
| `axiom_shared_regions` | `heap/shared.rs` | sound (premise) | A `'static` permission freezes the field; as `axiom_shared_built`. |
| `Shared::begin` | `heap/shared.rs` | fixed | Requires `this == shared_storage()` and takes the `Shared` step's `Once` token, so it mints one permission per process. Without the `requires`, a call at null proves `false` through `is_nonnull`. |
| `axiom_run_locs` | `huge/mod.rs` | sound (premise) | Takes the `Huge` step's one-shot token. |
| `axiom_zero_entry` | `huge/table.rs` | sound (premise) | An all-zero `Entry` is unoccupied. Its `ZeroRaw` is a snapshot, not tied to the permission (see "Premises"). |
| `Reserved::is_apart_from_static` | `layout/regions.rs` | fixed | Requires a non-empty reservation; an empty one inside the static would make the claim false. |
| `Reserved::is_disjoint` | `layout/regions.rs` | fixed | Requires both reservations non-empty, for the same reason. |
| `Reserved::into_unbacked` | `layout/regions.rs` | sound | Consumes the token. |
| `axiom_acquired` | `lock.rs` | sound (premise) | Private, called on the guard just built; `deref_mut` havocs the guard, so a changed guard cannot meet its `requires` (see "Premises"). |
| `axiom_state_lock_inv` | `purge/background.rs` | sound | The initial value meets it; `start` (both holds), `main`'s exit and `fork_child` assert it, including `exited ==> !present`. |
| `axiom_user_va_limit` | `sys/pac.rs` | sound | Linux's default map window is at most 2^48 on both targets. |
| `axiom_auth_sign` | `sys/pac.rs` | sound | True for every address below `user_va_limit()` and every modifier, and with the keys disabled. |
| `axiom_arena_stats_lock_inv` | `vspec/arena.rs` | sound | One axiom for `ArenaStats`; `empty` and `store_stats` keep `identities`. |
| `axiom_class_table_wf` | `vspec/class.rs` | sound | Sizes and `wf` verified for every page geometry, both quanta. |
| `axiom_huge_lock_inv` | `vspec/huge.rs` | sound | One axiom; `Huge::new` meets `Lock::new`'s `requires`. |
| `axiom_huge_lock_frame` | `vspec/huge.rs` | sound | Reflexive; nothing writes the page count, the page shift or the `Meta` identity. |
| `axiom_ptr_add_addr` | `vspec/ptr.rs` | sound | True: `add` is `wrapping_add` where it is defined (see "Premises" for the harness's reach). |
| `axiom_fresh_anon_zero` | `huge/mod.rs` | sound (premise) | Mints a `Zeroed` for any range; its call sites (`huge/mod.rs`) pass a run's freshly mapped pages. |
| `axiom_discarded_zero` | `huge/mod.rs` | sound (premise) | As `axiom_fresh_anon_zero`, for pages just discarded with `MADV_DONTNEED`. |
| `Unbacked::commit` | `vspec/unbacked.rs` | sound (premise) | Called after a successful `mprotect` (`committed_range`); non-empty tokens come only from reservations, which start above 0. |

## Derivations of `false` the contracts rule out

Each of these derivations proves `false` against a weaker contract and
fails ("precondition not satisfied", or rustc's E0382 for a moved
one-shot token) against main's.

- **`Shared::begin` at null.** A `begin` with no `requires` mints a
  `PointsTo<Shared>` at any `this`:

  ```rust
  let Tracked(perm) = begin_old(core::ptr::null_mut());
  proof { perm.is_nonnull(); assert(false); }   // verifies
  ```

  A misaligned `this` does the same through `is_aligned`, and two calls
  on one storage through `is_disjoint`. That second-call route would
  also feed `axiom_shared_built` and
  `axiom_shared_regions`: a second permission to the `regions` field
  could be frozen with another value. `requires this ==
  shared_storage()` closes the first two; the `Shared` step's `Once`
  token closes the third.
- **`axiom_fresh_words` at `usize::MAX`.** `WordPerm::addr()` is an
  uninterpreted `usize` function, so it lies in the `usize` range. With
  no bound on `base`:

  ```rust
  let tracked r = fresh_words_old(usize::MAX, 2);
  assert(r.contains_key(1));                 // ensures false: verifies
  ```

  Here `r[1].addr()` is a `usize` equal to `usize::MAX + 8`. The
  `requires base + 8 * n <= usize::MAX` rules this out; `SlabTable::new`
  proves it from the bitmap reservation's end.
- **`axiom_fork_resumed` for any token.** An `ensures`
  `state.held() == state_lock()` for any `state` token would let a token
  of any lock become the state lock's. The contract requires it instead.
- **The class zero axioms past the slab.** With `n` unbounded, a caller
  that holds the next slab's slot tokens would get a zero claim running
  past the slab. `n <= nslots` rules it out.
- **`Reserved` with an empty token.** `disjoint(a, 0, b, blen)` is false
  when `b < a < b + blen`. A zero-length reservation is impossible
  (`mmap` of 0 bytes fails), and the non-empty `requires` say so.
- **The one-shot seams called twice.** `Shared::begin`,
  `axiom_slot_loc`, `axiom_fresh_words` and `axiom_run_locs` each take a
  linear `Once` token (`vspec/once.rs`), and `store_owner` uses up its
  group's markers. A second call with the same token fails with E0382; a
  second token for the same step cannot be taken from the process's map
  ("precondition not satisfied"). The tokens are the initial state of
  `STATE`'s invariant, and the verified `api::claim_init` takes them out
  on the one winning exchange from `UNINIT`.

## Premises

These seams are sound under a premise worth naming.

- **Fork handoff** (`axiom_fork_resumed_state`, `axiom_fork_resumed`).
  The hold is taken in `fork_prepare` and released in `fork_parent` or
  `fork_child`, separate C calls. Ghost state does not cross them, so any
  token the release handlers consume is minted at their entry under the
  premise that each handler runs once per fork, on the thread whose
  prepare handler returned.
- **The C contract** (`axiom_c_claim`). The C program hands each block
  back once; two claims of one pointer prove `false`.
- **Lock holders restore the invariant** (`axiom_acquired`, and every
  `*_lock_inv` axiom). A holder that ends with `LockGuard::unlock` is
  checked, since the unlock requires `lock_inv`. A guard that drops
  instead is not checked: a verified holder ends with the value
  satisfying the invariant, through the postcondition of the last call
  that changes the value or an explicit `assert`, unless the type's
  owner names a premise that holder rests on (`lock.rs`, `lock_inv`).
  `store_owner`'s one store per slab rests on it: a holder that took the
  owner-store markers and released without moving the watermark would
  verify, and the next holder would see the markers still there (no
  `false` follows, since two holds share only `lock_frame`).
  `take_group`, the one holder that takes them, asserts the invariant
  before its release.
- **`axiom_acquired`'s view.** `LockGuard`'s view is an uninterpreted
  function of a struct whose only field is the lock reference, so every
  acquisition of one lock has the same spec value. Callers cannot see
  `g.lock == self`; if `Lock::lock` ensured it, two acquisitions' values
  would be provably equal.
- **Zero tokens are snapshots** (`axiom_zero_entry`,
  `axiom_fresh_anon_zero`, `axiom_discarded_zero`, `Unbacked::commit`).
  Writes do not consume a `Zeroed` or `ZeroRaw`, and
  `axiom_fresh_anon_zero` and `axiom_discarded_zero` mint one for any
  range. With vstd's `PointsTo::leak_contents`, an initialised, occupied
  entry can be made uninitialised, and with a zero token for its bytes
  `axiom_zero_entry` then calls it empty. That is no `false`, but an
  untrue fact about memory. The one call of `axiom_zero_entry`
  (`huge/table.rs`, building the table's storage) passes bytes straight
  from the reservation's `ZeroRaw`.
- **`Unbacked::commit`'s commit-before-read.** Verus does not model page
  protection, so that committed pages are read only after the
  `mprotect` is a premise.
- **`axiom_ptr_add_addr`'s harness.** The Kani harness checks `u8`,
  `u64` and `(u64, u8)` on stack arrays. It does not check a zero-sized
  `T`, `cast_provenance` (mmap) pointers or one past the end. The
  contract is true for those as well: `add` is `wrapping_add` where it
  is defined, and the metadata clause is trivial for `T: Sized`. The
  `in_alloc` requirement is stronger than truth needs.
