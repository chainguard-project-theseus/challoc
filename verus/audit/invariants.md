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

After the audit, the one-shot premises of S77, S84, S85, S86, S115,
S135, S153, S154, S155 and S161 were made checkable; S31 and S113
remain premises ("One-shot premises", before "Open").

Seam numbers are the ledger's at base 8635213. Removing S118 shifts every
later number down by one in the regenerated ledger: S121 `assign_home`
is now S120, S155 `Shared::begin` is S154, and so on (in the ledger of
that commit; later changes renumber it again).

Every derivation of `false` below was run with Verus (`cargo verus
focus`, quantum16) as a scratch proof in the seam's module, and the
scratch removed afterwards. None reaches `false` through a call the
code makes.

## Per seam

| seam | item | verdict | reason |
|---|---|---|---|
| S18 | `heap_set_arena` (api/mallctl.rs) | sound | ensures nothing; its body's `Heap::set_arena` needs `home < MAX_ARENAS`, which `Heap::wf` does not carry (see Open) |
| S31 | `axiom_fork_resumed_state` (api/mod.rs) | sound (premise) | one call per fork handler; a second call gives a second token (double unlock), no `false` (no lemma separates two `LockHeld`s); still a premise after the one-shot work ("One-shot premises") |
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
| S67 | `ClassState::state_locked` (class/mod.rs) | sound (premise) | every state write goes through `set_state_locked` or `SlabDesc::init` under the lock; since verified through the descriptor permissions (`slab::DescPerm`), no longer a seam |
| S68 | `ClassState::set_state_locked` (class/mod.rs) | sound (premise) | frame lists every `Inner` field but the one it changes; since verified through the descriptor permissions (`slab::DescPerm`), no longer a seam |
| S69 | `ClassState::dirty_locked` (class/mod.rs) | sound (premise) | as S67; since verified through the descriptor permissions (`slab::DescPerm`), no longer a seam |
| S70 | `ClassState::set_dirty_locked` (class/mod.rs) | sound (premise) | as S68; since verified through the descriptor permissions (`slab::DescPerm`), no longer a seam |
| S75 | `axiom_fresh_slots_zero` (class/mod.rs) | fixed | `n` was unbounded: a range past the slab; now `n <= nslots` |
| S76 | `axiom_discarded_slots_zero` (class/mod.rs) | fixed | as S75; the `purged_is_zero` premise is still the call site's exec test |
| S77 | `axiom_table_bytes` (class/sets.rs) | fixed | at `base == 0` proved `false` (`into_typed` then `is_nonnull`); now requires `0 < base` (`Meta::reserve_table` ensures it); still a premise that `(base, len)` is a fresh `reserve_table` result: a second call, or a range over another permission's bytes, proves `false` (`is_disjoint`); closed after the audit ("One-shot premises") |
| S80 | `axiom_set_tables_lock_inv` (class/sets.rs) | sound | `lock_inv` is `true` for its type; the only axiom for it |
| S81 | `axiom_table_lock_inv` (class/table.rs) | sound | initial value verified; `take_group` asserts `inv`; `commit_run`'s and `commit_slab`'s `guards` bound is the documented premise |
| S82 | `axiom_table_lock_frame` (class/table.rs) | sound | nothing writes the geometry or `mloc` |
| S83 | `load_owner` (class/table.rs) | sound (premise) | every caller gates on the watermark before the read |
| S84 | `store_owner` (class/table.rs) | sound (premise) | one call, in `take_group`; `slab_owner` has no time, so two stores of one group prove `false`; closed after the audit ("One-shot premises") |
| S85 | `axiom_slot_loc` (class/table.rs) | sound (premise) | one call per `SlabTable::new`; two calls proved `false`; closed after the audit ("One-shot premises") |
| S86 | `axiom_fresh_words` (class/table.rs) | fixed | one call at `base = usize::MAX` proved `false` (a `usize` `addr()` equal to `usize::MAX + 8`); now requires the words end below `usize::MAX`; a second call stays a premise (no `false`, but a stale duplicate would let `word::load` claim a wrong value); closed after the audit ("One-shot premises") |
| S87 | `axiom_init_descs` (class/table.rs) | sound | consumes the uninitialised permissions; a descriptor of atomics is valid for every bit pattern |
| S88 | `axiom_init_owners` (class/table.rs) | sound | as S87 |
| S89 | `axiom_init_words` (class/table.rs) | sound | as S87; `word_addr` is an opaque function of a value |
| S113 | `axiom_c_claim` (heap/live.rs) | sound (premise) | the C contract; two claims of one slot start proved `false` (`theorem_apart`), as its doc says; still a premise after the one-shot work ("One-shot premises") |
| S115 | `axiom_fresh_stack` (heap/mod.rs) | sound (premise) | one call, on a block `Meta::alloc` just returned; `slots` aliasing any permission or live stack the caller holds proves `false`; closed after the audit ("One-shot premises") |
| S118 | `Heap::axiom_list_bound` (heap/mod.rs) | removed | proved: the list's keys are pairwise distinct `u16`s (`Heap::lemma_list_bound`) |
| S121 | `assign_home` (heap/mod.rs) | sound | `Arenas::assign` returns 0 or an index below `auto <= MAX_ARENAS` |
| S135 | `axiom_process_shared` (heap/shared.rs) | sound (premise) | one `Shared` per process; depends on S155 being called once; closed after the audit ("One-shot premises") |
| S152 | `axiom_shared_layout` (heap/shared.rs) | sound | Kani record and the build-time `fits` assertion cover every clause |
| S153 | `axiom_shared_built` (heap/shared.rs) | sound (premise) | consumes the field permissions; a second use needs a second `begin`; closed after the audit ("One-shot premises") |
| S154 | `axiom_shared_regions` (heap/shared.rs) | sound (premise) | a `'static` permission freezes the field; as S153; closed after the audit ("One-shot premises") |
| S155 | `Shared::begin` (heap/shared.rs) | fixed | no `requires`: at null proved `false` (`is_nonnull`); now requires `this == shared_storage()`; closed after the audit ("One-shot premises") |
| S156 | `init_tcaches` (heap/shared.rs) | sound | `Tcaches::init` writes both fields |
| S161 | `axiom_run_locs` (huge/mod.rs) | sound (premise) | one call per `Huge::new`; two calls prove `false`; closed after the audit ("One-shot premises") |
| S164 | `axiom_zero_entry` (huge/table.rs) | sound (premise) | an all-zero `Entry` is unoccupied; its `Zeroed` is not tied to the permission (see Details) |
| S168 | `axiom_process_page_at_most_64k` (layout/regions.rs) | sound (premise) | true on Linux x86_64 and aarch64 (4, 16, 64 KiB pages); the code admits up to 4 MiB (`Regions::wf`, `reserve`), so the platform fact is the premise |
| S170 | `Reserved::is_apart_from_static` (layout/regions.rs) | fixed | false for an empty reservation inside the static; now requires `len() > 0` |
| S171 | `Reserved::is_disjoint` (layout/regions.rs) | fixed | false for an empty reservation inside another; now requires both non-empty |
| S172 | `Reserved::into_unbacked` (layout/regions.rs) | sound | consumes the token |
| S177 | `axiom_acquired` (lock.rs) | sound (premise) | private, called on the guard just built; `deref_mut` havocs the guard, so a changed guard cannot meet its `requires` |
| S182 | `Meta::commit` (meta.rs) | sound (premise) | callers trusted to pass an aligned, uncommitted range inside a reservation or eager table (see Open) |
| S183 | `Meta::free` (meta.rs) | sound (premise) | callers trusted to pass a live block with its layout; null or an oversized layout aborts (see Open); closed after the audit: verified, its callers give back the `MetaClaim` the allocation handed out |
| S188 | `axiom_state_lock_inv` (purge/background.rs) | sound | initial value meets it; `start` (both holds), `main`'s exit and `fork_child` assert it; since upstream #128 it also has `exited ==> !present` |
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

## One-shot premises (closed after the audit)

The "one call" seams above, and the memory audit's S61, S155 and S77
(`memory.md`, "Outside this slice"), are now checked rather than
premised: each either uses up a linear tracked token minted once, or
takes the permission its owner already tracks, so a second call no
longer type-checks or verifies. Numbers are this audit's (base
8635213); the regenerated ledger numbers them differently. All changes
are ghost-only except where noted, and x86_64 machine code is
identical to the base (f4b9fed) on every function.

**The root token.** `vspec::once` defines `Once`, a linear token for
one step (`Step::Shared`, `Huge`, `SlotLoc(c)`, `FreshWords(c)`), with
a private field and no constructor. `api::claim_init` (new,
`external_body`) is `init_slow`'s compare-exchange of `STATE` from
`UNINIT`, moved out unchanged, and hands the winner the process's map
of tokens, one per step (`once::all_steps`). Its premise: nothing else
leaves `UNINIT`, so the exchange succeeds once per process; that is a
fact about one atomic and its one writer, not about where a ghost call
appears. `init` and `Shared::init` pass the map on; `Shared::init`
takes each step's token where the step runs (`once::take`).
(Superseded 2026-10-05: `claim_init` is verified; the tokens are
`api::axiom_state`'s initial state, and `claim_init` takes them out of
`STATE`'s invariant on the winning exchange, `state_claim`.)

| seam | before | after | double call now |
|---|---|---|---|
| S61 `from_raw_chunked` | unsound-open: minted the view's word permissions from nothing; two calls on one pointer prove `false` (machine-checked now as a control, below) | takes `words: PointsToRaw` with `dom() == chunked_bytes(ptr, rows, stride)` (eight bytes per row) and requires 8-byte alignment. `SetTable::new` reserves each group table with `Lazy::reserve_raw` (was `Lazy::reserve`), and `split_sets` carves each group's permission into its slots' bytes (`slot_bytes`, every row's word of the slot), keyed `(class, kind)`: classes of one group have distinct positions (`pos_apart`, proved in the first pass), so no two slots meet. `Arena::init` threads the map, `ClassState::new` takes its class's three and `Set::in_table` takes one; `lemma_slot_bytes` proves a slot's bytes are the chunked view's. `SetTable::new` and `Arena::init` now require `regions.separated()` (a page of at least 4 KiB, so the groups start on a word); both callers have it | E0382 (use of moved value) with one permission; "precondition not satisfied" at `from_raw_chunked` with any other (an empty one), at the second `tracked_remove` of one slot from `SetTable::new`'s map, and at `in_table` given another slot's bytes |
| S155 `Shared::begin` | premise: a second call gives a second `PointsTo<Shared>` (proves `false` by `is_disjoint`; machine-checked as a control) | takes the `Shared` step's `Once` | E0382 with one token; "precondition not satisfied" at the second `once::take(Step::Shared)`, and at `begin` given another step's token |
| S153 `axiom_shared_built`, S154 `axiom_shared_regions` | premise: a second use needs a second `begin` | unchanged contracts; a second `begin` no longer verifies (S155), so their inputs exist once | as S155 |
| S135 `axiom_process_shared` | premise: depends on S155 being called once | unchanged; that dependency is now checked | as S155 |
| S85 `axiom_slot_loc` | premise: two calls for one class prove `false` | takes the class's `SlotLoc(c)` token; also fixes `mark_loc(c)`, the class's marker authority (for S84) | E0382 with one token; "precondition not satisfied" at the second `take(SlotLoc(c))`, and at the axiom given class `c + 1`'s token |
| S86 `axiom_fresh_words` | premise: a second call gives a stale duplicate | takes the class's `FreshWords(c)` token | E0382 with one token; a second token of `c` cannot be taken (as for S85's `SlotLoc(c)`) |
| S161 `axiom_run_locs` | premise: two calls prove `false` | takes the `Huge` step's token | E0382 with one token; "precondition not satisfied" at the second `take(Step::Huge)` |
| S84 `store_owner` | premise: `slab_owner` has no time, so two stores of one group with two arenas prove `false` (machine-checked as a control) | uses up the owner-store markers (`owner_mark(x) = -1 - x`) of the group's slabs: `SlabTable::new` mints one per slab in its marker authority, which `axiom_slot_loc` fixes as `mark_loc(c)`; the commit lock holds those of the slabs from `next_slab` on (`TableInner::inv`, new conjunct); `take_group` splits off the group's before the store. Keyed per slab, not per group, so a made-up ghost `SlabTable` with another group size cannot cover a stored slab again. `SlabTable`'s type invariant gains `mark_auth.id() == mark_loc(class)`. The markers live in the commit lock's value, so this rests on the lock premise that a holder restores `TableInner::inv` before it releases (below) | E0382 with one marker set; "precondition not satisfied" at the second store given a fresh authority's markers |
| S77 `axiom_table_bytes` | premise: a second call, or a range over another permission's bytes, proves `false` | removed. `Meta::reserve_table` now returns the table bytes' permission it tracks: an eager table's from `Inner::handed` (where `reserve_eager` left it, previously dropped), a reservation's from the `Unbacked` token `Inner::reserve` carved (previously dropped), through the new `Unbacked::into_lazy`, which uses the token up and keeps only the old premise that pages are committed before they are read. Tests call `reserve_table_addr` | E0382 at a second `into_lazy` of one token; no other source mints the bytes' permission |
| S115 `axiom_fresh_stack` | premise: a second call, or `slots` aliasing a held permission, proves `false` | takes the block's `PointsToRaw` from `Meta::alloc_raw` (`ensure_cache` calls `alloc_raw` directly; `Meta::alloc` is `alloc_raw(..).0`, `inline(always)`, so the code is the same) | E0382 with one permission; "precondition not satisfied" with an empty one |
| S31 `axiom_fork_resumed_state` | premise | unchanged: still a premise | — |
| S113 `axiom_c_claim` | premise (the C contract) | unchanged: still a premise | — |

S34 `count_held` needed nothing more: `ForkHeld::wf` already carries
`0 <= n <= u32::MAX` and `count_held` requires it (fixed by this audit).

Exec changes, all off the hot set and all leaving the x86_64 code of
every function identical to the base: `init_slow`'s compare-exchange
moved into the `inline(always)` `claim_init`; `SetTable::new` calls
`Lazy::reserve_raw` instead of `Lazy::reserve` (removed: no other
caller), and `reserve_raw` is now `#[inline(always)]` (without it LLVM
outlined `reserve_raw` and inlined `reserve_table` into it, changing
`init_slow`, `Arena::init` and `reserve_table`); `reserve_raw` and
`Meta::reserve_table` return the permission (a zero-sized `Tracked`);
the eager path of `reserve_table` reborrows its guard to take
`handed`; `ensure_cache` calls `alloc_raw`. `Meta::alloc` is now used
only by tests (`allow(dead_code)` outside them).

Scratch proofs (run with `cargo verus focus`, quantum16, then removed;
nothing of them is left in the tree):

- Linearity, one batch: `from_raw_chunked` twice on one `words`;
  `Shared::begin` twice on one token; `Unbacked::into_lazy` twice;
  `axiom_fresh_stack` twice on one `raw`; `axiom_slot_loc`,
  `axiom_fresh_words`, `axiom_run_locs` twice on one token;
  `store_owner` twice on one marker set. rustc rejected all eight:
  "error[E0382]: use of moved value" (`w`, `t`, `u`, `raw`, `once` (3),
  `m`).
- Second source, one batch, each ending in the audit's derivation of
  `false`: `from_raw_chunked` on an empty permission (then
  `is_disjoint` of the two views' word 0); `Set::in_table` twice from
  one map entry, and on another slot's bytes; `Shared::begin` with a
  second `take(Step::Shared)` (then `split` both and `is_disjoint` of
  the two `push_flags` permissions), and with the `Huge` token;
  `axiom_slot_loc` with a second `take(SlotLoc(c))`, and with
  `SlotLoc(c + 1)`; `store_owner` with a `GhostSetAuth::new`'s markers
  (then `slab_owner == 1` and `== 2`); `axiom_fresh_stack` on an empty
  permission; `axiom_run_locs` with a second `take(Step::Huge)`. Each
  failed with "precondition not satisfied" at the second call (or the
  second `take`), and nowhere else.
- Controls, against copies of the old contracts in the same modules:
  `from_raw_chunked` twice, `Shared::begin` twice and `store_owner`
  twice with arenas 1 and 2 each verified `assert(false)`; with the new
  contracts the same bodies fail only at the second call.

Still premises, and why:

- S31 `axiom_fork_resumed_state` (and `axiom_fork_resumed`): the hold
  is taken in `fork_prepare` and released in `fork_parent` or
  `fork_child`, separate C calls; ghost state does not cross them, so
  any token the release handlers consume would be minted at their entry
  under the same "once per handler" premise. Making it checkable needs
  the handoff to go through something real (the saved signal mask's
  static is the only state the handlers share), an exec change.
- S113 `axiom_c_claim`: the C program's contract; every C free is its
  own entry with no ghost state from the allocation that made the
  block.
- S86's `base`: the token makes the call once per class, but nothing
  ties `base` to the class's bitmap (the words' bytes are also carved
  as `PointsTo<AtomicU64>` by `axiom_init_words`, by design); a call at
  another address would give `WordPerm`s that no lemma separates from
  other permissions, so no `false`, as before.
- S77's replacement `Unbacked::into_lazy`: the commit-before-read
  premise (Verus does not model page protection), as before.
- `claim_init`: the root mint, trusted for the one compare-exchange.
  (Superseded 2026-10-05: `claim_init` is verified; the tokens are
  `api::axiom_state`'s initial state.)
- S84's "once per slab" holds only while every holder of a class's
  commit lock restores `TableInner::inv` before releasing it
  (`axiom_table_lock_inv`): `LockGuard::drop` requires nothing, so a
  holder that took the owner-store markers and released without moving
  the watermark would verify, and the next holder would be told the
  markers are still there (the fresh-context review machine-checked such
  a holder; no `false` follows, since two holds share only
  `lock_frame`). `take_group`, the one holder that takes them, asserts
  the invariant before the release. A verified release that requires
  `lock_inv` would close it; it changes `Lock`'s interface everywhere.

## Open (decisions for the coordinator)

- One-shot premises (S84, S85, S113, S115, S153, S154, S155's second
  call, S161, S77's second call, S86): done after the audit, except
  S113 and S31 ("One-shot premises" above).
- S252 `axiom_fresh_block` should require
  `process_regions().user_block(p, len)`. calloc's caller meets it from
  `fits`. The usable-size callers (`calloc_usable`, jemalloc's
  `alloc_with`) would need the huge run's end, which `usable_is` does
  not carry. Closed after the audit: S252 to S255 are removed. Each live
  block's `Live` token holds the permission to its usable bytes
  (`vspec::user::UserMem`), which the fills and `realloc`'s copy take.
  New trusted items with it: `heap::live::axiom_hand_out` (the private
  hand-out mint, used up with a bare slot or run token),
  `vspec::user::axiom_zeroed_bytes` (a `Zeroed` token's bytes read zero;
  a stale token gives a false byte state, not `false`), `axiom_cast`
  (a permission's provenance, in place) and `copy_user`
  (`copy_nonoverlapping` with its byte effect). S113 `axiom_c_claim`'s
  `claims` now also carries the block's permission and provenance; two
  claims of one pointer still prove `false`, as before.
- S182 and S183 (`Meta::commit`, `Meta::free`): their bodies pass
  `assume_new` claims to verified functions whose `requires` (alignment,
  `len > 0`, `p != 0`, `layout_size <= MAX_BINNED`) the outer
  `requires` does not state. Null or an oversized layout aborts rather
  than corrupting memory, but double commits or a mismatched free break
  the metadata lock's invariant silently. Lifting the checkable part
  into the `requires` needs `Lazy::inv` to carry page alignment and
  `Layout` facts at the callers. Closed after the audit for S183:
  `Meta::free` is verified; it requires the `MetaClaim` (the live-object
  record and the bin slack) `alloc_raw` or `alloc_zeroed` returned with
  the block, which its callers carry (`CachePerms::claimed`, the huge
  allocator's `dclaims` and its table's `claim`), so a double or
  mismatched free cannot be verified; `Heap::destroy`'s trusted body
  passes `Tracked::assume_new()` for the claims of what it frees.
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
