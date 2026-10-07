# Seam audit: memory (raw pointer, UnsafeCell/volatile/transmute, atomics)

An adversarial audit of the 93 seams of `verus/SEAMS.md` in the
categories raw pointer (internal), UnsafeCell/volatile/transmute and
atomics primitive, at base 8635213. Numbers (`S<n>`) and lines are the
ledger's at that base; the ledger is regenerated with this audit, so
later numbers differ. For each seam: can its contract prove `false`
(instantiated at equal pointers, a second call, empty and maximal sizes)?
Does the `ensures` hold of the body for every input meeting the
`requires`, in both quanta and on aarch64 and x86_64? Is it still needed?
Is it stronger than its callers need?

Since package P7 (READY+HEAPPERM) S26 `shared_ptr` has an `ensures`,
and S132 `shared_of`, S158 `heap_new`, S159 `fast_flags`, S160
`heap_from_slot` and S175 `class_entry` verify (`verus/THEOREMS.md`,
common premises); the rows below are as audited at 8635213.

Verdicts: **sound** (no derivation of `false`, the contract holds of the
body; notes say what its truth rests on where that is an unchecked
`# Safety` duty), **fixed** (the contract was false or unsound and is
corrected here), **weakened** (over-strong, made weaker here),
**removed** (now verified), **unsound-open** (unsound, the fix needs a
design decision; see the details).

Totals: 76 sound, 10 fixed, 1 weakened, 5 removed, 1 unsound-open.
Two derivations of `false` were machine-checked (`from_raw`,
`Lock::get_unlocked`); one more is sketched (`axiom_arena_assemble`).

After the audit, the unsound-open S61 was fixed, and the three minting
seams of "Outside this slice" were made checkable (`Shared::begin`,
`sets::axiom_table_bytes`, now removed) or left as a premise with the
reason (`api::axiom_fork_resumed_state`): see `invariants.md`,
"One-shot premises", which also machine-checks the S61 derivation.

| seam | name | verdict | reason |
|---|---|---|---|
| S24 | `Global::get` | sound | No `ensures`; Verus assumes only `T`'s type invariants, which every stored value (`FeatureLine`'s `len < CAP` even zeroed) has. (Superseded 2026-10-05: removed or verified, through the permissions `api::axiom_statics` and `STATE`'s invariant hand out.) |
| S25 | `shared_ref` | sound | Ensures the closed, value-level `Shared::wf`, which `Shared::init` proves of `shared_at(storage)`; no field `wf` reads is written after init. (Superseded 2026-10-05: verified from `STATE`'s invariant.) |
| S26 | `shared_ptr` | sound | No `ensures`. (An `ensures p == Shared::storage()` would let `fast_flags` require its pointer; see S159.) |
| S28 | `shared_storage_ptr` | sound | `Shared::storage()` is one uninterpreted constant; the body returns the static's address. |
| S29 | `publish_features` | sound | No contract; writes before the `READY` release store, `init_slow` aborts on re-entry. (Superseded 2026-10-05: removed or verified, through the permissions `api::axiom_statics` and `STATE`'s invariant hand out.) |
| S35 | `save_sigmask` | sound | No contract. Removable by an `AtomicU64` static (an exec change; proposed, not made). |
| S36 | `saved_sigmask` | sound | No contract; as S35. |
| S47 | `Arena::axiom_array_join` | sound | Consumes the element permissions, which cover exactly the array's bytes (arrays have no inter-element padding); `N > 0` blocks a zero-sized array at any address. |
| S57 | `axiom_arena_assemble` | fixed | Returned a `PointsTo<Arena>` over all the struct's bytes while consuming only the fields': a caller keeping a padding byte's permission held two for one byte. Now consumes the padding (`pad_covers`). |
| S58 | `Arenas::init` | sound | Claims only `ptr` and `is_init`; every field is written. |
| S59 | `arena_at` | sound | The result is a fresh reference not tied to `p`, so two ghost indices cannot meet. Its `spec_index() == index` is true only for the registry entry `index` (the `# Safety` contract, a protocol invariant nothing checks). |
| S60 | `from_raw` | fixed | Minted the view's word permissions from nothing: two calls on one pointer prove `false` (machine-checked). Now takes the words' `PointsToRaw` and requires 8-byte alignment. |
| S61 | `from_raw_chunked` | unsound-open, fixed after the audit | The same minting as S60, with no permission available at its caller (`Set::in_table` over a shared `&SetTable`): two calls prove `false`. Fixed after the audit: it takes the permission to its rows' words, which `SetTable::new` carves once per slot (`invariants.md`, "One-shot premises"). |
| S71 | `ClassState::table` | sound | The clauses follow from the type invariant and `table_ptr`; the table's lifetime rests on the safe `ClassState::new`'s doc (proposal: store `&'static SlabTable`, removing S71 and S73). |
| S73 | `table_ptr` | sound | The pointer is otherwise unconstrained; nothing reads a table's address, so no two tables meet. |
| S78 | `SetTable::set_table_ptr` | sound | No `ensures`. |
| S79 | `Set::ensure` | sound | The body touches neither `map` nor `l1_words`. Over-strong: no caller uses `final(self).l1_words == old(self).l1_words` (could be dropped). |
| S87 | `axiom_init_descs` | sound | `SlabDesc` (atomics, `repr(C)`, no padding, no enum) is valid for every bit pattern; consumes the uninitialised permissions. "Initialised" covers pages not yet committed: the commit-before-read premise of `sets::axiom_table_bytes`. |
| S88 | `axiom_init_owners` | sound | As S87 (`AtomicU16`). |
| S89 | `axiom_init_words` | sound | As S87; `word_addr` of distinct opaque values at distinct addresses is satisfiable, and no `AtomicU64` value equality is provable. |
| S90 | `words_ref` | sound | The `requires` is `from_raw_parts`'s; `'a` is the permissions' borrow. |
| S91 | `SlabTable::watermark` | fixed | `core`'s load panics for `Release`/`AcqRel`, which the contract allowed; now required away. The bound holds: the only writers store 0 and a proved group end. |
| S92 | `SlabTable::owner_unchecked` | removed | Now verified: `requires slab < spec_nslabs()`, which `Shared::set_slot` now ensures. |
| S94 | `desc_ptr_addr` | sound | Empty contract; the Kani record covers it (the harness only shows that the cast returns, which is all the contract needs). |
| S95 | `read_word` | sound | Nothing claimed about the value. (Its caller's comment "slots are at least 16 bytes" is false under quantum8; harmless, both reads are of word 0.) |
| S108 | `Heap::home_ptr_at` | sound | True while a heap's ghost `home_hdrs` is where it lives: no verified code moves or swaps a `Heap` (by inspection). |
| S109 | `Heap::cached_recently` | sound | No `ensures`; reads `0..len` under the permissions. Removable by a `ptr_ref` loop (exec change; proposed). |
| S111 | `addr_mix` | sound | No `ensures`. |
| S114 | `stack_mut` | sound | The slice's lifetime is the permission map's mutable borrow; final values flow back into the permissions. |
| S119 | `arena_volatile` | sound | No `ensures`. |
| S120 | `init_fields` | sound | Consumes the 128-byte head permission (and drops the block's `MetaClaim`, which only weakens what the caller holds); every field it writes is in it (both quanta). |
| S122 | `mark_bound` | sound | No contract; runs before any reference to the heap exists. Removable by passing `bound` to `init_fields` (exec change; proposed). |
| S123 | `Heap::destroy` | fixed | The body calls `flush_all`, which requires `shared.wf()`; the contract did not. Now required (and by `Tcaches::destroy`). |
| S124 | `node_link` | removed | Now verified: stores through permissions split from the node's first-line permission, and returns the `arena` and `next` ones (`others::NodeHead`), which the heap keeps (`Heap::heads`). |
| S125 | `node_caches_mut` | weakened | Handed out `&'a mut` headers whose permissions `h` holds, so a write through it left them stale. Its one use reads; now `node_caches_detached`, a shared reference. |
| S126 | `node_set_arena` | removed | Now verified: stores through the permission to the node's `arena` (`Heap::heads`). |
| S128 | `Heap::node_next` | removed | Now verified: reads through the permission to `next` that `list_ok` (`others::heads_ok`) ties to the list. |
| S129 | `Heap::node_arena` | removed | Now verified: reads through the permission to `arena`, whose value `others::heads_ok` makes the ghost key. |
| S130 | `Heap::node_caches` | sound | No claim; lifetime tied to `&self`. |
| S132 | `shared_of` | sound | `wf` for the `Shared` a heap was created on; the tests' extra `Shared`s break only `wf`'s process-global conjuncts (documented). |
| S153 | `axiom_shared_built` | sound | Consumes its permissions and returns none, so padding does not matter; a second call needs a second set of field permissions, which only `Shared::begin` (S155, allocator invariant, not this slice) could mint (see "Outside this slice"). |
| S154 | `axiom_shared_regions` | sound | The permission is frozen behind `&'static`; two calls give the same value. |
| S157 | `heap_at` | sound | Two calls alias, but the results' ghost state is not tied to `p`, so nothing relates them; an `ensures` tying `r` to `p` would prove `false`. |
| S158 | `heap_new` | sound | True at its one call (`init_fields` stored null; nothing writes it before). |
| S159 | `fast_flags` | sound | The result is not linked to any known `push_flags`, so no `false`. Its claim is about the process's `Shared` for any pointer (proposal: `requires shared == Shared::storage()`, which needs S26's `ensures` and reaches every fast path). |
| S160 | `heap_from_slot` | fixed | With the wrong `pac` it authenticates (or not) and claims `wf` of garbage; now `requires pac == pac_mode()`, which both callers prove. |
| S162 | `Huge::check_zero` | sound | No `ensures` (missing `requires` for `addr`'s bounds is a debug-only memory-safety gap). |
| S164 | `axiom_zero_entry` | sound | Consumes the uninitialised permission and a 16-byte zero token; all-zero is an empty entry (null `desc`). |
| S165 | `ClassTable` | sound | Opaque type with one private constructor. |
| S166 | `ClassTable::new` | sound | No contract. |
| S167 | `ClassTable::table` | sound | One spec constant over memory that `adopt_page_geometry` rewrites once at init; no verified read of the geometry precedes it, and `wf` and the sizes hold before and after. The `wf`/size clauses repeat S263. |
| S175 | `class_entry` | sound | As S167; its callers need `Regions`, built after the call. |
| S178 | `Lock::get_unlocked` | fixed | Took its token by shared reference: two calls gave two `&mut T` to one value, each with the invariant's ghost state, which proves `false` (machine-checked). The token is now borrowed mutably for the result's lifetime, and the token-less `RawLock::unlock` (a second token by release and re-take) is private. |
| S179 | `LockGuard::deref` | sound | The view is uninterpreted and private; two guards of one lock at once needed the token-less release that S178's fix made private. |
| S180 | `LockGuard::deref_mut` | sound | As S179. |
| S181 | `LockGuard::drop` | sound | No contract. (Its futex wake is a syscall under no `may_syscall`; no fast path drops a guard.) |
| S184 | `Policy::position` | sound | True: only `new` (0) and the verified `apply` write it. Removable by a clamp (exec change; proposed). |
| S186 | `Explicit::drop` | sound | No contract. |
| S194 | `shared_arg` | sound | `wf` for any argument: true for its one caller, the thread `spawn` starts with the process's `Shared` (a C-program contract on `pthread_create`). |
| S196 | `state` | sound | One static. |
| S197 | `read_global` | sound | No `ensures`; instantiated at `isize` and `u32`, which have no type invariant. |
| S198 | `read_cpu_id` | sound | No `ensures`; the caller checks the area size. |
| S201 | `SlabDesc::at` | sound | No `ensures`. Since verified through its `PointsTo` permission, no longer a seam. |
| S202 | `SlabDesc::sub_live` | sound | No `ensures`. Removable (vstd specifies `fetch_sub`) if the formatted `assert!` became a `check`. |
| S244 | `load_lg_slab_min` | sound | True: initial values (255, 21, 255) are `wf` and the one writer requires `Decision::wf`. |
| S245 | `load_lg_pmd` | sound | As S244. |
| S246 | `load_lg_meta` | sound | As S244. |
| S247 | `addr_to_ptr` | sound | `addr_ptr` and `cast_provenance` are uninterpreted, one value per address; the ensures pins the address, that value and the shared cast provenance (which `Set::new` uses to match `from_raw`'s permission). |
| S248 | `core::ptr::write_bytes::<T>` | sound | `core`'s safety contract. Its effect is unstated (see details). |
| S249 | `core::ptr::copy_nonoverlapping::<T>` | sound | `core`'s safety contract, overlap included. |
| S250 | `<*mut T>::is_null` | sound | The data address is zero (wide pointers too). |
| S251 | `<*const T>::is_null` | sound | As S250. |
| S256 | `Atomic::<*mut T>::load` | fixed | Panics for `Release`/`AcqRel`; now required away. |
| S257 | `Atomic::<*mut T>::store` | sound | Already requires away `Acquire`/`AcqRel`. |
| S258 | `Atomic::<*mut T>::new` | sound | No contract. |
| S259 | `Atomic::<*mut T>::swap` | sound | Every ordering is valid; any result. |
| S261 | `load_bool` | fixed | Panics for `Release`/`AcqRel`; now required away. |
| S265 | `crate::layout::adopt_page_geometry` | sound | No claim; S167's constant rests on its single pre-publication call. (Superseded 2026-10-05: removed or verified, through the permissions `api::axiom_statics` and `STATE`'s invariant hand out.) |
| S267 | `<*const T>::cast::<U>` | sound | The address is kept. |
| S301 | `core::cell::UnsafeCell<T>` | sound | Opaque. |
| S302 | `core::cell::UnsafeCell::<T>::new` | sound | No claim. |
| S310 | `core::sync::atomic::compiler_fence` | sound | Panics only for `Relaxed`, which it requires away. |
| S312 | `Atomic::<u32>::as_ptr` | sound | No claim. |
| S321 | `<*mut T>::add` | sound | `core`'s contract; the result is abstract (`ptr_add`). |
| S322 | `axiom_raw_valid` | sound | Only lower bounds on the uninterpreted lengths, so consistent; true of a permission's bytes. With `len == 0` (any empty permission) it gives `alloc_len >= 0` and `rw_len >= 0` at every address, against `alloc_len`'s "negative when there is none"; harmless while no axiom bounds them above (see the model notes). |
| S352 | `core::ptr::from_ref` | sound | No claim. |
| S353 | `core::ptr::from_mut` | sound | No claim. |
| S360 | `WordPerm` | sound | Unique per word only because `table::axiom_fresh_words` mints one set per table (outside this slice). |
| S361 | `fetch_or` | sound | Every ordering is valid; the value follows the permission. |
| S362 | `store` | fixed | Panics for `Acquire`/`AcqRel`; now required away. |
| S363 | `load` | fixed | Panics for `Release`/`AcqRel`; now required away. |
| S364 | `fetch_and` | sound | As S361. |
| S365 | `write_zero` | sound | `write_bytes(p, 0, n)` makes the bytes zero; the token is not a permission. |

## Details

### S178 `Lock::get_unlocked`: two `&mut` from one token (fixed)

The token came in as `Tracked<&LockHeld>`, so nothing stopped a second
call while the first reference lived. Verus treats the two `&mut T` as
two independent values, each satisfying `lock_inv` and `lock_frame`, and
`lock_frame` pins the ghost identities. For `meta::Inner` this proves
`false`; the scratch below verified (`cargo verus focus --
--verify-module meta`) with the old signature:

```rust
fn scratch_get_unlocked_twice(m: &Meta)
    requires crate::sys::may_syscall(),
{
    let t = m.inner.raw().lock_held();
    let a = unsafe { m.inner.get_unlocked(Tracked(t.borrow())) };
    let b = unsafe { m.inner.get_unlocked(Tracked(t.borrow())) };
    proof {
        broadcast use crate::vspec::meta::axiom_meta_lock_inv,
            crate::vspec::meta::axiom_meta_lock_frame, vstd::raw_ptr::group_raw_ptr_axioms;
        // A key no live object can have: `live_wf` puts every key's block
        // below the bump pointer.
        let k = vstd::raw_ptr::ptr_mut_from_data::<FreeNode>(vstd::raw_ptr::PtrData {
            addr: usize::MAX, provenance: vstd::raw_ptr::Provenance::null(), metadata: () });
        assert(a.live_wf() && b.live_wf());
        assert(a.live_id() == b.live_id());   // lock_frame
        let tracked mut pa = a.live.borrow_mut().insert(k, 0);
        let tracked pb = b.live.borrow_mut().insert(k, 0);
        pa.disjoint(&pb);                     // one key, one location
        assert(false);
    }
}
```

Fix: `get_unlocked<'a>(&'a self, Tracked(held): Tracked<&'a mut LockHeld>)
-> &'a mut T`, ensuring `*final(held) == *old(held)`. The same scratch
(with `t.borrow_mut()`) is now rejected by the borrow checker (E0499,
both rustc's and Verus's lifetime check), so the two references cannot
coexist. Callers: `purge::background::fork_child` now takes the token
mutably, and `api::fork_child` passes `&mut state`. Ghost only.

That alone left a second route (found by the fresh-context review, and
machine-checked there): `RawLock::unlock` was `pub` and took no token,
so `lock_held`, `raw().unlock()`, `lock_held` again gave two tokens for
one lock on one thread, hence two `get_unlocked` references, and the
same derivation of `false`; `lock()`, `raw().unlock()`, `lock()` gave
two guards the same way. The token-less release is now the private
`RawLock::release`: outside `lock.rs` a lock is released only by
`unlock_held` (which consumes the token) or a guard's drop. The loom
models, which drive the bare word, get a `cfg(loom)` `unlock`. No
caller outside `lock.rs` used it. A second `lock_held` (or `lock()`)
while the first hold lives deadlocks at run time; Verus, which assumes
`lock_slow` returns (no `decreases`), can reason past it, but only on a
path that never runs (partial correctness).

One token source remains outside this slice: `api::axiom_fork_resumed_state`
(S31) mints a `LockHeld` with no `requires`, so two calls give two
tokens (see "Outside this slice").

What remains is the discipline every `Lock` holder already has
(`lock_inv`'s doc): a holder that moves a tracked permission out of the
value and keeps it past its hold could meet the next holder's copy.
That is `axiom_acquired`'s assumption (S177, not this slice), shared by
`Lock::lock`, not something this seam adds.

### S60 `from_raw`, S61 `from_raw_chunked`: minted word permissions

Both bodies build the view's permissions with `Tracked::assume_new()`
and `fresh` (`valid`, `ok`) promises an initialised `PointsTo<u64>` at
`ptr_add(ptr, k)` for every word. Nothing is consumed, so two calls on
one pointer give two permissions for one word. Verified (`--verify-module
bitmap`) at the base:

```rust
fn scratch_from_raw_twice(p: *mut u64) {
    let mut a = unsafe { TwoLevelBitmap::from_raw(64, p) };
    let mut b = unsafe { TwoLevelBitmap::from_raw(64, p) };
    proof {
        broadcast use vstd::layout::layout_of_primitives;
        assert(a.ok(a.l1o(0)) && b.ok(b.l1o(0)));
        crate::vspec::ptr::axiom_ptr_add_addr(a.ptr, 0);
        crate::vspec::ptr::axiom_ptr_add_addr(b.ptr, 0);
        let tracked mut x = a.perms.borrow_mut().tracked_remove(0);
        let tracked y = b.perms.borrow_mut().tracked_remove(0);
        x.is_disjoint(&y);
        assert(false);
    }
}
```

The `# Safety` contract ("must not be accessed by anything else while
the set exists") is the missing `requires`, and Verus does not check it.

S60 fixed: `from_raw` takes `Tracked<PointsToRaw>` covering exactly the
view's `8 * words_needed(nbits)` bytes, of `ptr`'s provenance, so a
second view over the same words needs a second permission. Its one
verified caller, `Set::new`, now reserves with `Lazy::reserve_raw` (the
same `Meta::reserve_table` call, plus the table's permission) instead of
`Lazy::reserve`. The conversion of those bytes into zero `u64`s stays
trusted (true of a fresh reservation). The permission comes from
`sets::axiom_table_bytes`, which itself mints (outside this slice: see
below), but that axiom is private with one call; `from_raw` was
`pub(crate)`. It also requires `ptr` 8-byte aligned (found by the
review, machine-checked there): vstd's `PointsTo::is_aligned` holds of
every typed permission, so a permission to misaligned bytes turned into
`PointsTo<u64>`s proves `false`. `Set::new` shows it from the table's
page alignment, with a new `requires page >= 8` that its one caller
(`Huge::new`, page at least `MIN_ALIGN`) meets.

S61 open: `Set::in_table` builds each class's three chunked views over
an arena's shared `SetTable` from `&SetTable`, so it has no permission
to give. A fix means `SetTable::new` reserving its group tables with
`reserve_raw`, splitting each group's permission into per-slot row sets
(rows `slot + r * stride`), and handing each slot's set to the one
`in_table` call for it (through `Arena::init` and `ClassState::new`, or a
per-slot token kept in the `SetTable` and taken once). That threads
ghost state through three files other lanes are changing, so it is left
for the coordinator; until then `in_table`'s `requires` could at least
document the once-per-slot duty. `from_raw_chunked` also lacks
`from_raw`'s alignment requirement; its fix should add it.

Fixed after the audit, along those lines: `SetTable::new` reserves
each group with `reserve_raw` and splits the permission into every
class's three slots, keyed `(class, kind)`, which `Arena::init` hands
to `ClassState::new` and that to `Set::in_table`; `from_raw_chunked`
takes the slot's bytes (`chunked_bytes`) and requires 8-byte
alignment. The two-calls derivation above, run against the old
contract, verifies `false`; against the new one it fails at the second
call (`invariants.md`, "One-shot premises").

### S57 `axiom_arena_assemble`: padding (fixed)

The axiom returned a permission to all `size_of::<Arena>()` bytes but
consumed only the field permissions; `Arena::split` dropped the rest
(`_raw`). `index: u16` is followed by `decay_ns: AtomicU64`, so
`axiom_arena_layout` (with `AtomicU64`'s alignment 8) forces a byte `g`
that no field covers (`A_INDEX + 2` if below `A_DECAY_NS`, else
`A_INDEX - 1`). Sketch, not machine-checked (it needs a copy of
`Arena::init`'s writes, including real `ClassState`s): split the
uninitialised `Arena` permission as `split` does but keep the leftover
raw; initialise the fields; assemble; `into_typed::<u8>(g)` on the
leftover and `is_disjoint` against the assembled `PointsTo<Arena>`.

Fix (ghost only): `take_field` now also ensures the exact domain of what
it leaves; `Arena::split` returns that leftover (the padding) with
`pad_covers` (every byte of the `Arena` is the leftover's or a field's);
`axiom_arena_assemble` requires and consumes it. The assembled
permission is now exactly the bytes it was given.

Precision: of its `ensures`, its one caller (`Arena::init`) uses `ptr`,
`is_init` and `index`; the other field equalities are true and unused,
and could be dropped.

### Atomics orderings (S91, S256, S261, S362, S363: fixed)

`core` panics for a load with `Release` or `AcqRel` and for a store with
`Acquire` or `AcqRel`. `AtomicPtr::store` and `compiler_fence` already
required their panicking orderings away; `AtomicPtr::load`, `load_bool`,
`vspec::word::load` and `store`, and `SlabTable::watermark` (which takes
the ordering from its caller) did not, so verified code could pass one
and keep its "no panic" claim. Each now requires them away; every
caller passes a constant ordering that meets it. vstd's own `core`
atomic specifications (`std_specs/atomic.rs`) have the same gap for the
integer atomics and `compare_exchange`'s failure ordering; they are
vstd's, not challoc seams.

### S160 `heap_from_slot`, S123 `Heap::destroy` (fixed)

`heap_from_slot` authenticated against the thread pointer exactly when
`pac`, and claimed `wf` of the result. With a `pac` that is not the
process's mode it reads a garbage or still-signed address. Now
`requires pac == pac_mode()`; `heap_for_thread_in` has it from
`Shared::pac` under `wf`, `existing_heap_in` from `fast_flags`.

`Heap::destroy` flushes with `flush_all`, which requires `shared.wf()`;
the trusted contract required only `may_syscall`. Now it requires
`shared.wf()`, as does `Tcaches::destroy`, its caller (whose caller,
`mallctl`'s `tcache.destroy`, has it).

### S125 `node_caches_mut` (weakened)

It returned `&'a mut [CacheHeader; NCLASSES]`, any `'a`, to headers
whose `PointsTo`s heap `h` holds: a write through it would change
memory under those permissions without Verus seeing it. Its one use
(`Heap::set_tcache_enabled`) passes it to `fold_counters`, which takes
`&`. It is now `node_caches_detached`, returning `&'a`; the machine code
is the same field address.

### S92 `SlabTable::owner_unchecked` (removed)

A trusted wrapper whose only job was hiding `owner_at`'s `requires slab
< spec_nslabs()`. `Shared::set_slot` tested `is_handed_out` (which
ensures that bound) but did not say so; it now ensures it, and
`owner_unchecked` is verified with that `requires`.

### Model notes (sound verdicts with an assumption worth knowing)

- **`valid_writes`/`in_alloc` are timeless.** `rw_len` and `alloc_len`
  have no memory state, so a bound established from a permission
  (`axiom_raw_valid`) or an allocator axiom outlives the permission. And
  `write_bytes`/`copy_nonoverlapping` (S248, S249, and `write_zero`)
  state no effect, so Verus assumes they change no memory a ghost
  permission covers. Together: verified code could write through a
  bound it kept after giving the bytes' permission away, or zero bytes
  under a `PointsTo` it holds, and keep believing the old value. No
  derivation of `false` was found (no axiom combines a `Zeroed` token
  with an initialised permission), and every current call uses its bound
  at once on memory no permission covers, as `vspec/ptr.rs` documents.
  A sound version would make these functions take the bytes' permission.
- **Truth under `# Safety`.** `arena_at` (S59), `heap_at` (S157),
  `heap_new` (S158), `shared_of` (S132), `shared_arg` (S194),
  `fast_flags` (S159) and `ClassState::table` (S71) have no `requires`
  that ties their claim to the pointer; the claim is true for the
  pointers their callers pass by the `# Safety` contract. None relates
  two results, so none proves `false`; adding an `ensures` that relates
  the result to the pointer (for the heaps: `home_hdrs` from `p`) would.
- **`class_table()` across `adopt_page_geometry`** (S165-S167, S175,
  S265): one constant names the table before and after the init-time
  rewrite. No verified code reads the geometry fields before it, and the
  rewrite keeps every entry `wf` with its size, so the constant is the
  table's value at every verified read.
- **Sequential consistency.** `vspec::word::load` returns the permission's
  value for a `Relaxed` load; the proofs assume SC (`vspec/word.rs`,
  `verus/WEAKMEM.md`).

### Outside this slice (reported here, changed after the audit)

The S60 pattern, a trusted function or axiom that mints ownership with
no `requires` making it once-only, recurs in three seams of other
categories, each provable to `false` by a second call:

- `Shared::begin` (S155, allocator invariant): `Tracked::assume_new()`
  of a `PointsTo<Shared>`; two calls on one pointer give two
  permissions (`is_disjoint`), and two `Shared::init`s would give
  `axiom_shared_built` two values for `shared_at(p)`.
- `sets::axiom_table_bytes` (S77): a `PointsToRaw` for any `base` and
  `len`, no `requires` (private, one call, `Lazy::reserve_raw`, which
  `Set::new` now also calls).
- `api::axiom_fork_resumed_state` (S31, C-program contract): a
  `LockHeld` from nothing; two calls give two tokens for the state lock,
  so two `get_unlocked` references (the S178 derivation).

After the audit, `Shared::begin` takes a one-shot token minted once per
process and `axiom_table_bytes` is gone (`Meta::reserve_table` returns
the table's permission); `axiom_fork_resumed_state` stays a premise,
because ghost state does not cross the fork handlers' C calls
(`invariants.md`, "One-shot premises").

### Kani record

S94 `desc_ptr_addr` is the slice's one discharged seam. Its harness
asserts the body's own expression, so it shows only that the call
returns, which is all an empty contract claims; the record's translation
says so. Its contract is unchanged here.
