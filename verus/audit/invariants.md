# Seam audit: allocator invariants, axioms and proof seams

An adversarial audit of the seams in `verus/SEAMS.md` of kind `axiom`
or `external_body proof` (any category) and of category `allocator
invariant` (any kind): 69 seams at base 8635213. For each one: can it
prove `false` (edge cases, a second call, arbitrary spec arguments,
vstd's permission lemmas), is its `ensures` true of the body or of the
world for every input meeting its `requires`, is it still needed, and
is it stronger than its callers need.

Verdicts:

- **sound**: no derivation of `false` found, and the contract is true
  for every input meeting its `requires`.
- **sound (premise)**: true at the code's own calls, under a premise
  the seam's doc states and Verus cannot check: the seam is called
  once, from one place, in a stated state. A second call, or a call
  with arguments the code never passes, would prove `false` or an
  untrue fact. Where the audit derived `false` that way with Verus,
  the details say so ("Derivations").
- **fixed**: a `requires` was missing or the contract was false at
  some input meeting it; corrected, callers re-proved.
- **weakened**: the `ensures` claimed more than any caller uses; cut
  to what callers use.
- **removed**: proved; no longer a seam.
- **unsound-open**: false at some reachable input, not fixed here.
  None in this slice.

Totals: 69 seams. sound 28, sound (premise) 28, fixed 11, weakened 1,
removed 1, unsound-open 0.

Seam numbers are the ledger's at base 8635213. Removing S118 shifts every
later number down by one in the regenerated ledger: S121 `assign_home`
is now S120, S155 `Shared::begin` is S154, and so on.

Every derivation of `false` below was run with Verus (`cargo verus
focus`, quantum16) as a scratch proof in the seam's module, and the
scratch removed afterwards. None reaches `false` through a call the
code makes.

## Per seam

| seam | item | verdict | reason |
|---|---|---|---|
| S18 | `heap_set_arena` (api/mallctl.rs) | sound | ensures nothing; its body's `Heap::set_arena` needs `home < MAX_ARENAS`, which `Heap::wf` does not carry (see Open) |
| S31 | `axiom_fork_resumed_state` (api/mod.rs) | sound (premise) | one call per fork handler; a second call gives a second token (double unlock), no `false` (no lemma separates two `LockHeld`s) |
| S32 | `axiom_fork_resumed` (api/mod.rs) | fixed | ensured `state.held() == state_lock()` for any `state`; now required |
| S33 | `arena_held` (api/mod.rs) | sound (premise) | frozen arena table under the creation lock; `i as u16` exact because the count never exceeds `MAX_ARENAS` (S34's fix bounds `n` to `u32`) |
| S34 | `count_held` (api/mod.rs) | fixed | `r: u32` with `r == held.n()` proved `false` for a bundle with `n` out of `u32`; `n` now in `ForkHeld::wf` and required |
| S39 | `axiom_unlocked` (api/print.rs) | sound | `Unlocked` has no spec content; premise (no allocator lock held at the print entry points) holds |
| S46 | `Arena::axiom_array_layout` (arena.rs) | sound | true for every `T`, `N` rustc accepts (vstd bounds `size_of` only through the exec `layout_for_type_is_valid`, which only compilable types reach); build-time check of the three `split_array` instances added |
| S47 | `Arena::axiom_array_join` (arena.rs) | sound | consumes the element permissions; arrays have no inter-element padding |
| S56 | `axiom_arena_layout` (arena.rs) | sound | Kani record and the build-time `fits` assertion (every target and quantum) cover every clause |
| S57 | `axiom_arena_assemble` (arena.rs) | sound (premise) | consumes the field permissions but mints the padding bytes too, which `split` dropped (`_raw`) |
| S62 | `ClassState::page_group_is_idle` (class/decay.rs) | sound (premise) | true for the real `g` (the guard's value); a made-up ghost `g` gets the `ensures` for free; stale doc (`transfer_group`) corrected |
| S63 | `axiom_class_lock_inv` (class/mod.rs) | sound | one `lock_inv` axiom for class `Inner`; every verified holder restores `inv` |
| S64 | `axiom_class_lock` (class/mod.rs) | sound | one `lock_frame` axiom; nothing writes `nbits`, `arena`, `table` |
| S66 | `ClassState::owner_locked` (class/mod.rs) | weakened | claimed `(r == arena) == owns(slab)`; callers use only `r == arena ==> owns`; the converse is provable and no longer trusted |
| S67 | `ClassState::state_locked` (class/mod.rs) | sound (premise) | every state write goes through `set_state_locked` or `SlabDesc::init` under the lock |
| S68 | `ClassState::set_state_locked` (class/mod.rs) | sound (premise) | frame lists every `Inner` field but the one it changes |
| S69 | `ClassState::dirty_locked` (class/mod.rs) | sound (premise) | as S67 |
| S70 | `ClassState::set_dirty_locked` (class/mod.rs) | sound (premise) | as S68 |
| S75 | `axiom_fresh_slots_zero` (class/mod.rs) | fixed | `n` was unbounded: a range past the slab; now `n <= nslots` |
| S76 | `axiom_discarded_slots_zero` (class/mod.rs) | fixed | as S75; the `purged_is_zero` premise is still the call site's exec test |
| S77 | `axiom_table_bytes` (class/sets.rs) | fixed | at `base == 0` proved `false` (`into_typed` then `is_nonnull`); now requires `0 < base` (`Meta::reserve_table` ensures it); still a premise that `(base, len)` is a fresh `reserve_table` result: a second call, or a range over another permission's bytes, proves `false` (`is_disjoint`) |
| S80 | `axiom_set_tables_lock_inv` (class/sets.rs) | sound | `lock_inv` is `true` for its type; the only axiom for it |
| S81 | `axiom_table_lock_inv` (class/table.rs) | sound | initial value verified; `take_group` asserts `inv`; `commit_slab`'s `guards` bound is the documented premise |
| S82 | `axiom_table_lock_frame` (class/table.rs) | sound | nothing writes the geometry or `mloc` |
| S83 | `load_owner` (class/table.rs) | sound (premise) | every caller gates on the watermark before the read |
| S84 | `store_owner` (class/table.rs) | sound (premise) | one call, in `take_group`; `slab_owner` has no time, so two stores of one group prove `false` |
| S85 | `axiom_slot_loc` (class/table.rs) | sound (premise) | one call per `SlabTable::new`; two calls proved `false` |
| S86 | `axiom_fresh_words` (class/table.rs) | fixed | one call at `base = usize::MAX` proved `false` (a `usize` `addr()` equal to `usize::MAX + 8`); now requires the words end below `usize::MAX`; a second call stays a premise (no `false`, but a stale duplicate would let `word::load` claim a wrong value) |
| S87 | `axiom_init_descs` (class/table.rs) | sound | consumes the uninitialised permissions; a descriptor of atomics is valid for every bit pattern |
| S88 | `axiom_init_owners` (class/table.rs) | sound | as S87 |
| S89 | `axiom_init_words` (class/table.rs) | sound | as S87; `word_addr` is an opaque function of a value |
| S113 | `axiom_c_claim` (heap/live.rs) | sound (premise) | the C contract; two claims of one slot start proved `false` (`theorem_apart`), as its doc says |
| S115 | `axiom_fresh_stack` (heap/mod.rs) | sound (premise) | one call, on a block `Meta::alloc` just returned; `slots` aliasing any permission or live stack the caller holds proves `false` |
| S118 | `Heap::axiom_list_bound` (heap/mod.rs) | removed | proved: the list's keys are pairwise distinct `u16`s (`Heap::lemma_list_bound`) |
| S121 | `assign_home` (heap/mod.rs) | sound | `Arenas::assign` returns 0 or an index below `auto <= MAX_ARENAS` |
| S135 | `axiom_process_shared` (heap/shared.rs) | sound (premise) | one `Shared` per process; depends on S155 being called once |
| S152 | `axiom_shared_layout` (heap/shared.rs) | sound | Kani record and the build-time `fits` assertion cover every clause |
| S153 | `axiom_shared_built` (heap/shared.rs) | sound (premise) | consumes the field permissions; a second use needs a second `begin` |
| S154 | `axiom_shared_regions` (heap/shared.rs) | sound (premise) | a `'static` permission freezes the field; as S153 |
| S155 | `Shared::begin` (heap/shared.rs) | fixed | no `requires`: at null proved `false` (`is_nonnull`); now requires `this == shared_storage()` |
| S156 | `init_tcaches` (heap/shared.rs) | sound | `Tcaches::init` writes both fields |
| S161 | `axiom_run_locs` (huge/mod.rs) | sound (premise) | one call per `Huge::new`; two calls prove `false` |
| S164 | `axiom_zero_entry` (huge/table.rs) | sound (premise) | an all-zero `Entry` is unoccupied; its `Zeroed` is not tied to the permission (see Details) |
| S168 | `axiom_process_page_at_most_64k` (layout/regions.rs) | sound (premise) | true on Linux x86_64 and aarch64 (4, 16, 64 KiB pages); the code admits up to 4 MiB (`Regions::wf`, `reserve`), so the platform fact is the premise |
| S170 | `Reserved::is_apart_from_static` (layout/regions.rs) | fixed | false for an empty reservation inside the static; now requires `len() > 0` |
| S171 | `Reserved::is_disjoint` (layout/regions.rs) | fixed | false for an empty reservation inside another; now requires both non-empty |
| S172 | `Reserved::into_unbacked` (layout/regions.rs) | sound | consumes the token |
| S177 | `axiom_acquired` (lock.rs) | sound (premise) | private, called on the guard just built; `deref_mut` havocs the guard, so a changed guard cannot meet its `requires` |
| S182 | `Meta::commit` (meta.rs) | sound (premise) | callers trusted to pass an aligned, uncommitted range inside a reservation or eager table (see Open) |
| S183 | `Meta::free` (meta.rs) | sound (premise) | callers trusted to pass a live block with its layout; null or an oversized layout aborts (see Open) |
| S188 | `axiom_state_lock_inv` (purge/background.rs) | sound | initial value meets it; `start` and `fork_child` assert it |
| S206 | `axiom_snapshot_lock_inv` (stats.rs) | sound | `Snapshot::EMPTY` meets it; `collect_into` asserts it |
| S222 | `axiom_user_va_limit` (sys/pac.rs) | sound | Linux's default map window is at most 2^48 on both targets |
| S223 | `axiom_auth_sign` (sys/pac.rs) | sound | true for every address below `user_va_limit()` (at most 2^48) and every modifier, and with the keys disabled |
| S252 | `axiom_fresh_block` (vspec/addr.rs) | sound (premise) | any non-null `p` and any `len`; callers pass a fresh block and at most its usable size (see Open) |
| S253 | `axiom_moved_block` (vspec/addr.rs) | sound (premise) | the earlier fix holds: `p == q` only with `old == 0` or `len == 0`, which yields nothing S252 does not |
| S254 | `axiom_slot_writable` (vspec/addr.rs) | fixed | any `p`, null included, was writable; now requires `is_slot(class, p)` (its one caller has it) |
| S255 | `axiom_huge_writable` (vspec/addr.rs) | fixed | any `len`; now requires the bytes end inside the huge region (proved at `zero_huge` from `spec_run_usable`) |
| S260 | `axiom_arena_stats_lock_inv` (vspec/arena.rs) | sound | one axiom for `ArenaStats`; `empty` and `store_stats` keep `identities` |
| S263 | `axiom_class_table_wf` (vspec/class.rs) | sound | sizes and `wf` verified for every page geometry, both quanta |
| S293 | `axiom_huge_lock_inv` (vspec/huge.rs) | sound | one axiom; `Huge::new` passes `Lock::new`'s `requires` |
| S294 | `axiom_huge_lock_frame` (vspec/huge.rs) | sound | `==>` form, reflexive; nothing writes `np`, `lg` |
| S303 | `axiom_meta_lock_inv` (vspec/meta.rs) | sound | as S293; its truth also rests on S182 and S183's premises |
| S304 | `axiom_meta_lock_frame` (vspec/meta.rs) | sound | as S294 |
| S320 | `axiom_ptr_add_addr` (vspec/ptr.rs) | sound | true (`add` is `wrapping_add` where defined); Kani gaps in Details |
| S322 | `axiom_raw_valid` (vspec/ptr.rs) | sound (premise) | validity outlives the borrowed permission (see Details) |
| S357 | `axiom_fresh_anon_zero` (vspec/sys.rs) | sound (premise) | mints a `Zeroed` for any range; one call site (`huge::zero_run`) |
| S358 | `axiom_discarded_zero` (vspec/sys.rs) | sound (premise) | as S357 |
| S359 | `Unbacked::commit` (vspec/unbacked.rs) | sound (premise) | called after a successful `mprotect`; non-empty tokens come only from reservations, which start above 0 |

## Details

### Fixed

**S155 `Shared::begin`.** It had no `requires` and minted a
`PointsTo<Shared>` at any `this`. Derivation (run with Verus, in
`heap::shared`, with the old contract):

```rust
let Tracked(perm) = begin_old(core::ptr::null_mut());
proof { perm.is_nonnull(); assert(false); }   // verifies
```

A misaligned `this` does the same through `is_aligned`, and two calls on
one storage through `is_disjoint`. That second-call route also feeds
S135, S153 and S154: a second permission to the `regions` field could be
frozen with another value. Fix: `requires this == shared_storage()`.
Its one caller, `Shared::init`, already requires that. The second-call
premise stays (`api::init` calls `init` once, under `INITIALISING`).
`init` now needs `#[verifier::rlimit(20)]`.

**S34 `count_held` (and S33).** `ensures r as int == held.n()` with
`r: u32`, and nothing bounded `n`. Inside `api` a bundle can be built
with `n == -1` (the fields are private to `api`, not to the
constructor), and the call then proves `false`. Fix: `ForkHeld::wf` now
carries `0 <= n <= u32::MAX`, and `count_held` requires it.
`fork_prepare` builds `n` from a `u32`; `axiom_fork_resumed` restates
`wf`. S33's `i as u16` is exact only when `n <= 65536`. That holds in
reality because `Arenas::create` caps the count at `MAX_ARENAS`, but
nothing in the spec says so; this stays its premise.

**S32 `axiom_fork_resumed`.** The `ensures` included
`state.held() == state_lock()` (through `fork_holds`) for any `state`
token, so a token of any lock "became" the state lock. Fix: required.
The one caller, `fork_release`, requires it already.

**S77 `axiom_table_bytes`.** Derivation (run with Verus, in
`class::sets`):

```rust
let tracked r = axiom_table_bytes(0, 8);
let tracked (a, _b) = r.split(Set::range(0, 1));
let tracked p = a.into_typed::<u8>(0);
p.is_nonnull();                            // ensures false: verifies
```

Fix: `requires 0 < base`. `Meta::reserve_table` now ensures `0 < s`
(from `Inner::wf`'s `base > 0`, with no proof hints), and
`Lazy::reserve_raw` passes that on. A second call for one table stays
the stated premise (two permissions to one byte also prove `false`
through `is_disjoint`).

**S86 `axiom_fresh_words`.** It had no `requires`, and it ensured
`r[j].addr() == base + 8 * j`, where `WordPerm::addr()` is an
uninterpreted `usize` function and so lies in the `usize` range.
Derivation (run with Verus, in `class::table`, with the old contract):

```rust
let tracked r = fresh_words_old(usize::MAX, 2);
assert(r.contains_key(1));                 // ensures false: verifies
```

Here `r[1].addr()` is a `usize` equal to `usize::MAX + 8`. Fix:
`requires base + 8 * n <= usize::MAX`. `SlabTable::new` proves it in
one `nonlinear_arith` step from the bitmap reservation's end (below
2^57). Proved inline, it pushed the function's type-invariant check
over under quantum8. The same call with the new contract fails with
"precondition not satisfied".

**S254 `axiom_slot_writable`.** Any `p`, null included, was "valid for
writes" of the class size. So `axiom_slot_writable(0, null)` followed
by `write_zero(null, size)` verified. Fix:
`requires process_regions().is_slot(class, p@.addr)`, which is
`zero_slot`'s own `requires`. The scratch call at null now fails with
"precondition not satisfied".

**S255 `axiom_huge_writable`.** Any `len`. Fix: `requires (page <<
lg_page) + len <= huge_len`. `Shared::zero_huge` proves it from
`Huge::usable_size`'s `spec_run_usable` and the page arithmetic.

**S75, S76 (class zero axioms).** `n` was unbounded, so a caller
holding the next slab's slot tokens got a zero claim running past the
slab. Fix: `requires n <= t.spec_slots().nslots`. `take` proves it from
its pick's `n <= nslots`.

**S170, S171 (`Reserved`).** `disjoint(a, 0, b, blen)` is false when
`b < a < b + blen`. A zero-length token is impossible (`mmap` of 0
bytes fails), but `try_reserve`'s contract allows one. Fix: both
require non-empty tokens, which every caller has.

### Weakened

**S66 `ClassState::owner_locked`.** It claimed `(r == arena) ==
owns(slab)`. Both callers (`reopen`, `on_live_zero`) use only `r ==
arena ==> owns(slab)`, the direction that is really assumed. The
converse is provable (`Inner::base`) and is no longer claimed on trust.

### Removed

**S118 `Heap::axiom_list_bound`.** It is now `Heap::lemma_list_bound`,
proved: `list_ok` makes the keys pairwise distinct `u16`s, so the list
has at most 2^16 nodes (`others::lemma_distinct_u16s`, by
`unique_seq_to_set` and `lemma_len_subset`). The pigeonhole proof sits
in `heap/others.rs`. With its closure in `heap/mod.rs`,
`Heap::lemma_gc_step` stopped verifying (a context-sensitivity of that
proof).

### Derivations of `false` through a call the code does not make

All of these ran with Verus as scratch proofs (each `ensures false`,
verified) and were then removed. Each seam's doc already names its
premise:

- S113 `axiom_c_claim`: two claims of one slot start, then
  `theorem_apart`.
- S85 `axiom_slot_loc(0, l, 0, 0); axiom_slot_loc(0, l, 1, 0);` (the
  guard geometry `(0, 0) == (1, 0)`).
- S115 `axiom_fresh_stack(perms, hdr, live_stack, 1)`, where
  `live_stack` is the stack of an owned header with `cap > 0`: the new
  entry's owner must be both `(hdr, 0)` and `(p, 0)`. More generally
  (by reading, not run), any `slots` that aliases a permission the
  caller holds proves `false` through `is_disjoint`, for example
  `slots = hdr as *mut *mut u8`. The premise is that `slots` is a fresh
  `Meta::alloc` block.
- S77 `axiom_table_bytes` after its fix (by reading, not run): one call
  over bytes that another permission covers, such as
  `axiom_table_bytes(1, usize::MAX)`, proves `false` through
  `is_disjoint`. The premise is a fresh `reserve_table` result.
- S84 `store_owner` (by reading, not run): two stores of one group with
  two arenas. `slab_owner` has no time parameter.
- S161 `axiom_run_locs` (by reading, not run): two calls with two
  authorities' ids, then the authorities' disjointness.

These are the "one call" axioms. Making them checkable would mean
consuming a one-shot tracked token minted once per process (for
example by `Shared::begin`), which is a design change (Open).

### Kept as premises worth naming

**S164 `axiom_zero_entry`, with S357, S358 and S359.** A `Zeroed` token
is a snapshot: writes do not consume it, and the axioms mint one for
any range. With vstd's `PointsTo::leak_contents`, an initialised,
occupied entry can be made uninitialised, and with a `Zeroed` for its
bytes `axiom_zero_entry` then calls it empty. That is no `false`, but
an untrue fact about memory. Today only `entry_slots` calls it, with
tokens straight from `commit` or `alloc_zeroed`.

**S322 `axiom_raw_valid`.** It borrows the permission, but its
`valid_writes` outlives the borrow, so a write after the permission is
given back (`Meta::free`) would verify. `rw_len` and `alloc_len` have
no upper bound anywhere, so this cannot reach `false`.

**S168 `axiom_process_page_at_most_64k`.** `Regions::wf` allows
`lg_page` up to 22, and `Regions::reserve` accepts any power-of-two
page up to `MAX_SLAB`. So nothing in the code keeps the process page at
64 KiB or below: only the platform does (Linux pages are 4 KiB on
x86_64, and 4, 16 or 64 KiB on aarch64). Making the axiom provable
would mean `reserve` aborting above 64 KiB, which is an exec change, and
`Regions::wf` carrying `lg_page <= 16`.

**S57 `axiom_arena_assemble`.** The assembled `PointsTo<Arena>` covers
the padding bytes, and nothing consumes them: `Arena::split` drops the
remainder (`_raw`). A proof that kept the remainder could hold two
permissions to one byte.

**S177 `axiom_acquired`.** `LockGuard`'s view is an uninterpreted
function of a struct whose only field is the lock reference, so every
acquisition of one lock has the same spec value. Callers cannot see
`g.lock == self` today. If `Lock::lock` ever ensured it, two
acquisitions' values would be provably equal. A `ghost v: T` field as
the view would remove the hazard.

**S320 `axiom_ptr_add_addr` (Kani record).** The harness checks `u8`,
`u64` and `(u64, u8)` on stack arrays. It does not check a zero-sized
`T`, `cast_provenance` (mmap) pointers, or one-past-the-end. The
contract is still true for those: `add` is `wrapping_add` where
defined, and the metadata clause is trivial for `T: Sized`. The
`in_alloc` requirement is stronger than truth needs.

**S46 `Arena::axiom_array_layout` (Kani record).** The axiom is generic
and Kani checks instances on x86_64. Build-time assertions (`arena.rs`,
`heap/shared.rs`) now check the three `split_array` instances on every
target and quantum. The record's Translation and Tests sections
say so; the contract is unchanged, so the record check still passes.

## Open (decisions for the coordinator)

- One-shot premises (S84, S85, S113, S115, S153, S154, S155's second
  call, S161, S77's second call, S86). These could be made checkable
  by threading a tracked one-shot token from `Shared::begin`, or by
  having `Meta` return the permissions it already tracks (S77, S86:
  `handed` and the `Unbacked` token `reserve_table` drops; S115: the
  `PointsToRaw` `Meta::alloc_raw` already returns).
- S252 `axiom_fresh_block` should require
  `process_regions().user_block(p, len)`. calloc's caller meets it from
  `fits`. The usable-size callers (`calloc_usable`, jemalloc's
  `alloc_with`) would need the huge run's end, which `usable_is` does
  not carry.
- S182 and S183 (`Meta::commit`, `Meta::free`): their bodies pass
  `assume_new` claims to verified functions whose `requires` (alignment,
  `len > 0`, `p != 0`, `layout_size <= MAX_BINNED`) the outer
  `requires` does not state. Null or an oversized layout aborts rather
  than corrupting memory, but double commits or a mismatched free break
  the metadata lock's invariant silently. Lifting the checkable part
  into the `requires` needs `Lazy::inv` to carry page alignment and
  `Layout` facts at the callers.
- S18 `heap_set_arena` could go once `Heap::wf` carries
  `arena < MAX_ARENAS`.
- S121 `assign_home` could go if `Arenas::assign` looped to
  `min(auto, MAX_ARENAS)`. That is an exec change, left out here.
- S62 `page_group_is_idle` and S75/S76 take the lock value as a ghost
  argument, so a made-up value meets their `requires`. Taking `&Inner`
  (the guard's value) would tie them to the lock.
- Adjacent, not this slice: `ClassTable::table()` ensures
  `*r == class_table()` with no time, while `adopt_page_geometry`
  mutates the table once in `api::init`. No verified read precedes that
  write today.
