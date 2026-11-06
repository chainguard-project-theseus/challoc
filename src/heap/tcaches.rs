//! Explicit caches (design 13.3).
//!
//! An explicit cache (`tcache.create`) is a [`Heap`] owned by no thread:
//! same caches, same refill and flush, no TLS. It is not thread safe, as in
//! jemalloc. `MALLOCX_TCACHE_NONE` bypasses every cache and claims or
//! releases straight against the slab bitmaps (the defragmenter relies on
//! this to land allocations in the fullest slabs).

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::arena::{Held, elem_at, field_at, fits};
#[cfg(verus_keep_ghost)]
use crate::arena::{
    axiom_array_layout, elems_at, elems_init, elems_uninit, field_fits, field_ptr,
    lemma_field_cast, split_array, storage_ok, take_field,
};
use crate::lock::{Lock, LockOrder};
#[cfg(verus_keep_ghost)]
use crate::lock::{rank_class, rank_tcaches};
use crate::vspec::entry::EntryPerm;
#[cfg(verus_keep_ghost)]
use crate::vspec::entry::entry_id;
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{axiom_raw_readable, in_alloc, ptr_add};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
#[cfg(verus_keep_ghost)]
use vstd::layout::size_of;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::PointsToRaw;
use vstd::raw_ptr::{PointsTo, ptr_mut_write};
use vstd::resource::Loc;
use vstd::resource::set::{GhostPersistentSingleton, GhostSetAuth};

#[cfg(verus_keep_ghost)]
use super::thread::HeapPerm;
use super::thread::slot_heap_ref;
use super::{FreshHeap, Heap, Shared};
use crate::sys::tls::HeapLoan;
use crate::vspec::ctl::Change;

verus! {

/// Explicit caches; ids are the 12-bit `MALLOCX_TCACHE` field minus 2.
pub const MAX_TCACHES: usize = 1024;

/// Explicit caches (`tcache.create` / `tcache.flush` / `tcache.destroy`):
/// heaps owned by no thread, looked up by id. `repr(C)`, for
/// [`axiom_tcaches_layout`]: [`Tcaches::init`] writes the table entry by
/// entry. The ghost `inv` at the end has no bytes.
#[repr(C)]
pub struct Tcaches {
    /// The heaps, as `*mut Heap` cast to bytes: `Heap` reaches `Shared`,
    /// which holds this table, and Verus rejects a type that contains
    /// itself through a pointer's type parameter. The casts at the load
    /// and store sites are no-ops.
    table: [AtomicPtr<u8>; MAX_TCACHES],
    lock: Lock<()>,
    /// Ghost (no bytes): the table's invariant ([`TablePred`]), which
    /// holds each entry's permission.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    inv: Tracked<TableInv>,
}

/// What the table's invariant ([`TablePred`]) holds: each entry's
/// permission ([`EntryPerm`], by index) and its generation, a counter
/// that [`Tcaches::create`]'s store always advances, to the next odd
/// value, and [`Tcaches::destroy`]'s swap of a non-null entry advances by
/// one: even while the entry is null, odd while it holds a cache. And the
/// authority of the generations' lower bounds (`bounds`, a set of
/// `(entry, generation)` pairs, each at most its entry's generation), with
/// the persistent token of each pair (`tokens`), which
/// [`lower_bound`] hands out copies of.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct TableVal {
    pub tracked entries: Map<int, EntryPerm<u8>>,
    pub ghost gens: Map<int, nat>,
    pub tracked bounds: GhostSetAuth<(int, nat)>,
    pub tracked tokens: Map<(int, nat), Bound>,
}

/// A lower bound on an entry's generation: the pair `(entry, generation)`
/// is in the table's `bounds` set ([`TableVal`]), so the entry's
/// generation is at least that, now and from then on: a token is
/// persistent, and the table's invariant ([`TablePred`]) keeps every pair
/// at most its entry's generation.
pub type Bound = GhostPersistentSingleton<(int, nat)>;

/// What the table's invariant is about: each entry's atomic, by index
/// ([`entry_id`]), and the location of the generations' lower bounds.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct TableKey {
    pub entries: Seq<int>,
    pub bounds: Loc,
}

/// Table entry `i` in the table's invariant: its permission is for the
/// atomic the key names (`k.entries[i]`, its [`entry_id`]), and it is
/// null exactly when its generation is even.
pub open spec fn entry_ok(k: TableKey, v: TableVal, i: int) -> bool {
    &&& v.entries.contains_key(i)
    &&& v.entries[i].id() == k.entries[i]
    &&& v.gens.contains_key(i)
    &&& (v.entries[i].value()@.addr == 0 <==> v.gens[i] % 2 == 0)
}

/// Lower bound `p` in the table's invariant: an entry of the table, at
/// most that entry's generation, with its persistent token.
pub open spec fn bound_ok(k: TableKey, v: TableVal, p: (int, nat)) -> bool {
    &&& 0 <= p.0 < MAX_TCACHES
    &&& v.gens.contains_key(p.0)
    &&& p.1 <= v.gens[p.0]
    &&& v.tokens.contains_key(p)
    &&& v.tokens[p].id() == k.bounds
    &&& v.tokens[p]@ == p
}

/// The table's invariant, about the entries' atomics and the bounds'
/// location (the key): every entry is [`entry_ok`] and has a lower bound
/// at its generation once it has moved ([`has_bound`]), and every lower
/// bound is [`bound_ok`]. So no write moves a generation back: the bound
/// at the old generation would exceed it.
pub struct TablePred;

impl InvariantPredicate<TableKey, TableVal> for TablePred {
    open spec fn inv(k: TableKey, v: TableVal) -> bool {
        &&& inv_but(k, v, -1)
    }
}

/// Entry `i` has moved from generation 0 only to a generation with a lower
/// bound in `bounds`.
pub open spec fn has_bound(v: TableVal, i: int) -> bool {
    v.gens[i] != 0 ==> v.bounds@.contains((i, v.gens[i]))
}

/// [`TablePred`]'s invariant, but for [`has_bound`] at entry `skip`: what
/// a write of entry `skip` leaves until it records the generation it
/// moved the entry to ([`lower_bound`]).
pub open spec fn inv_but(k: TableKey, v: TableVal, skip: int) -> bool {
    &&& k.entries.len() == MAX_TCACHES
    &&& forall|i: int| 0 <= i < MAX_TCACHES ==> #[trigger] entry_ok(k, v, i)
    &&& forall|i: int| 0 <= i < MAX_TCACHES && i != skip ==> #[trigger] has_bound(v, i)
    &&& v.bounds.id() == k.bounds
    &&& forall|p: (int, nat)| #[trigger] v.bounds@.contains(p) ==> bound_ok(k, v, p)
}

/// The table's invariant.
pub type TableInv = AtomicInvariant<TableKey, TableVal, TablePred>;

/// Namespace of the table's invariant ([`TablePred`]).
pub open spec fn tcaches_ns() -> int {
    0x7463_6163
}

/// A lower-bound token for entry `i` at its current generation, from the
/// table's invariant value `v`, which may lack only that entry's
/// [`has_bound`]: a copy of the one `v` keeps, or a new one it then keeps.
/// Nothing else in `v` changes, and `v` meets the whole invariant.
proof fn lower_bound(k: TableKey, tracked v: &mut TableVal, i: int) -> (tracked b: Bound)
    requires
        inv_but(k, *old(v), i),
        0 <= i < MAX_TCACHES,
    ensures
        TablePred::inv(k, *final(v)),
        final(v).entries == old(v).entries,
        final(v).gens == old(v).gens,
        b.id() == k.bounds,
        b@ == (i, old(v).gens[i]),
{
    assert(entry_ok(k, *v, i));
    let p = (i, v.gens[i]);
    if v.bounds@.contains(p) {
        assert(bound_ok(k, *v, p));
        assert forall|j: int| 0 <= j < MAX_TCACHES && j != -1 implies #[trigger] has_bound(*v, j) by {}
        v.tokens.tracked_borrow(p).duplicate()
    } else {
        let ghost v0 = *v;
        let tracked s = v.bounds.insert(p);
        let tracked t = s.persist();
        let tracked b = t.duplicate();
        v.tokens.tracked_insert(p, t);
        assert forall|q: (int, nat)| #[trigger] v.bounds@.contains(q) implies bound_ok(k, *v, q) by {
            if q != p {
                assert(v0.bounds@.contains(q));
                assert(bound_ok(k, v0, q));
            }
        }
        assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] entry_ok(k, *v, j) by {
            assert(entry_ok(k, v0, j));
        }
        assert forall|j: int| 0 <= j < MAX_TCACHES && j != -1 implies #[trigger] has_bound(*v, j) by {
            if j != i {
                assert(has_bound(v0, j));
            }
        }
        b
    }
}

impl Tcaches {
    /// The table lock is the explicit-cache table lock of the lock order
    /// (`init` creates it so), and `inv` is the table's invariant, over
    /// this table's entries, in [`tcaches_ns`].
    #[verifier::type_invariant]
    spec fn type_inv(self) -> bool {
        &&& self.lock.rank() == rank_tcaches()
        &&& self.inv@.namespace() == tcaches_ns()
        &&& self.inv@.constant().entries.len() == MAX_TCACHES
        &&& forall|i: int| 0 <= i < MAX_TCACHES ==> #[trigger] self.inv@.constant().entries[i] == entry_id(&self.table[i])
    }
}

// Byte offsets of `Tcaches`'s fields: `offset_of!`, which Verus cannot
// evaluate, so their values are opaque to it; `axiom_tcaches_layout`
// states what verified code relies on, and the build checks it.
#[verifier::external_body]
const T_TABLE: usize = core::mem::offset_of!(Tcaches, table);
#[verifier::external_body]
const T_LOCK: usize = core::mem::offset_of!(Tcaches, lock);
#[verifier::external_body]
const T_INV: usize = core::mem::offset_of!(Tcaches, inv);

/// `Tcaches`'s layout (`repr(C)`): each field at its offset, aligned, and
/// ending by the next one's offset, the last (the zero-sized ghost `inv`)
/// by the size, and a table entry not empty. Trusted, and checked by the
/// build: the `const` assertion after this block evaluates the same
/// conditions with `fits`.
axiom fn axiom_tcaches_layout()
    ensures
        field_fits::<Tcaches, [AtomicPtr<u8>; MAX_TCACHES]>(T_TABLE, T_LOCK as int),
        0 < size_of::<AtomicPtr<u8>>(),
        MAX_TCACHES * size_of::<AtomicPtr<u8>>() <= isize::MAX,
        // Over several lines, as in `arena::axiom_arena_layout`.
        field_fits::<
            Tcaches,
            Lock<()>,
        >(T_LOCK, T_INV as int),
        field_fits::<Tcaches, Tracked<TableInv>>(T_INV, size_of::<Tcaches>() as int),
        size_of::<Tcaches>() <= isize::MAX,
;

} // verus!

// `axiom_tcaches_layout`, checked. `Tcaches` is `repr(C)`, which fixes the
// fields in declaration order (the table, `lock`, then the zero-sized
// ghost), so this holds on every compiler.
const _: () = assert!(
    fits::<Tcaches, [AtomicPtr<u8>; MAX_TCACHES]>(T_TABLE, T_LOCK)
        && core::mem::size_of::<AtomicPtr<u8>>() > 0
        && MAX_TCACHES as u128 * core::mem::size_of::<AtomicPtr<u8>>() as u128
            <= isize::MAX as u128
        && fits::<Tcaches, Lock<()>>(T_LOCK, T_INV)
        && fits::<Tcaches, Tracked<TableInv>>(T_INV, core::mem::size_of::<Tcaches>())
        && core::mem::size_of::<Tcaches>() <= isize::MAX as usize
);

// `arena::axiom_array_layout` at `Tcaches::split`'s instance, checked.
const _: () = assert!(
    core::mem::size_of::<[AtomicPtr<u8>; MAX_TCACHES]>()
        == MAX_TCACHES * core::mem::size_of::<AtomicPtr<u8>>()
        && core::mem::align_of::<[AtomicPtr<u8>; MAX_TCACHES]>()
            == core::mem::align_of::<AtomicPtr<u8>>()
);

verus! {

/// The permissions to the fields of a `Tcaches` under construction but
/// its table, which is a map of its own, entry by entry
/// ([`Tcaches::split`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
tracked struct TcachesParts {
    tracked lock: PointsTo<Lock<()>>,
    tracked inv: PointsTo<Tracked<TableInv>>,
}

impl TcachesParts {
    /// Each permission is to its field of the `Tcaches` at `p`, which is
    /// [`storage_ok`], and the table is inside it.
    spec fn at(&self, p: *mut Tcaches) -> bool {
        &&& storage_ok(p)
        &&& self.lock.ptr() == field_ptr::<Tcaches, Lock<()>>(p, T_LOCK)
        &&& self.inv.ptr() == field_ptr::<Tcaches, Tracked<TableInv>>(p, T_INV)
        &&& in_alloc(field_ptr::<Tcaches, AtomicPtr<u8>>(p, T_TABLE), MAX_TCACHES as int * size_of::<AtomicPtr<u8>>())
        &&& MAX_TCACHES * size_of::<AtomicPtr<u8>>() <= isize::MAX
    }
}

/// The table entry permissions of the `Tcaches` at `p`, entry by entry.
spec fn table_at(m: Map<int, PointsTo<AtomicPtr<u8>>>, p: *mut Tcaches) -> bool {
    elems_at(m, field_ptr::<Tcaches, AtomicPtr<u8>>(p, T_TABLE), 0, MAX_TCACHES as int)
}

/// Byte `x` is in the field of type `T` at offset `off` of the `Tcaches`
/// at `p`.
spec fn in_tcaches_field<T>(x: int, p: *mut Tcaches, off: usize) -> bool {
    p@.addr + off <= x < p@.addr + off + size_of::<T>()
}

/// Every byte of the `Tcaches` at `p` is `pad`'s or a field's: what
/// [`Tcaches::split`] keeps of the storage once it has taken the fields
/// (the padding), which [`axiom_tcaches_assemble`] takes back.
spec fn tcaches_pad_covers(pad: PointsToRaw, p: *mut Tcaches) -> bool {
    forall|x: int|
        p@.addr <= x < p@.addr + size_of::<Tcaches>() ==> #[trigger] pad.dom().contains(x)
            || in_tcaches_field::<[AtomicPtr<u8>; MAX_TCACHES]>(x, p, T_TABLE)
            || in_tcaches_field::<Lock<()>>(x, p, T_LOCK)
            || in_tcaches_field::<Tracked<TableInv>>(x, p, T_INV)
}

impl Tcaches {
    /// The permissions to the fields of the uninitialised `Tcaches`
    /// `perm` is to: `arena::take_field` for each field at the offsets
    /// [`axiom_tcaches_layout`] gives, and the table split by
    /// `arena::split_array`; and the bytes left over, the padding.
    proof fn split(tracked perm: PointsTo<Tcaches>) -> (tracked r: (
        TcachesParts,
        Map<int, PointsTo<AtomicPtr<u8>>>,
        PointsToRaw,
    ))
        requires
            perm.is_uninit(),
            perm.ptr()@.addr + size_of::<Tcaches>() <= usize::MAX,
        ensures
            r.0.at(perm.ptr()),
            r.0.lock.is_uninit(),
            r.0.inv.is_uninit(),
            table_at(r.1, perm.ptr()),
            elems_uninit(r.1, 0, MAX_TCACHES as int),
            tcaches_pad_covers(r.2, perm.ptr()),
            r.2.provenance() == perm.ptr()@.provenance,
    {
        broadcast use vstd::raw_ptr::group_raw_ptr_axioms, vstd::layout::layout_of_primitives;

        axiom_tcaches_layout();
        axiom_array_layout::<AtomicPtr<u8>, MAX_TCACHES>();
        assert(0 < MAX_TCACHES * size_of::<AtomicPtr<u8>>()) by (nonlinear_arith)
            requires
                0 < MAX_TCACHES,
                0 < size_of::<AtomicPtr<u8>>(),
        ;
        let p = perm.ptr();
        perm.is_nonnull();
        perm.is_aligned();
        let tracked raw = perm.into_raw();
        axiom_raw_readable(&raw, p as *mut u8, size_of::<Tcaches>() as int);
        let tracked (table, raw) = take_field::<Tcaches, [AtomicPtr<u8>; MAX_TCACHES]>(raw, p, T_TABLE, T_LOCK as int);
        let tracked (lock, raw) = take_field::<Tcaches, Lock<()>>(raw, p, T_LOCK, T_INV as int);
        let tracked (inv, pad) = take_field::<Tcaches, Tracked<TableInv>>(raw, p, T_INV, size_of::<Tcaches>() as int);
        let tracked table = split_array(table);
        lemma_field_cast::<Tcaches, [AtomicPtr<u8>; MAX_TCACHES], AtomicPtr<u8>>(p, T_TABLE);
        (TcachesParts { lock, inv }, table, pad)
    }
}

/// The written fields of the `Tcaches` at `p` (`Tcaches::split`'s parts
/// and table entries, every one initialised), as the `Tcaches`: the value
/// of each field is what its permission holds. Trusted, as
/// `arena::axiom_arenas_assemble` is for `Arenas`: vstd has no way to
/// join typed permissions; the permissions cover every field at its
/// offset, and `pad` every byte between and around them
/// ([`tcaches_pad_covers`]), so their bytes are the `Tcaches`'s. The type
/// invariant (`Tcaches::type_inv`) is a precondition, so the value is one
/// a `Tcaches` may hold.
axiom fn axiom_tcaches_assemble(
    tracked parts: TcachesParts,
    tracked table: Map<int, PointsTo<AtomicPtr<u8>>>,
    tracked pad: PointsToRaw,
    p: *mut Tcaches,
) -> (tracked t: PointsTo<Tcaches>)
    requires
        parts.at(p),
        table_at(table, p),
        tcaches_pad_covers(pad, p),
        pad.provenance() == p@.provenance,
        parts.lock.is_init(),
        parts.inv.is_init(),
        elems_init(table, 0, MAX_TCACHES as int),
        parts.lock.value().rank() == rank_tcaches(),
        parts.inv.value()@.namespace() == tcaches_ns(),
        parts.inv.value()@.constant().entries.len() == MAX_TCACHES,
        forall|i: int| 0 <= i < MAX_TCACHES ==> #[trigger] parts.inv.value()@.constant().entries[i] == entry_id(&table[i].value()),
    ensures
        t.ptr() == p,
        t.is_init(),
        t.value().lock == parts.lock.value(),
        forall|i: int| 0 <= i < MAX_TCACHES ==> #[trigger] t.value().table[i] == table[i].value(),
        t.value().inv == parts.inv.value(),
;

impl Tcaches {
    /// Initialise `this` in place, empty, through the permission to its
    /// storage in `slot`, handed back initialised: the lock is created
    /// with rank `rank_tcaches()` (the type invariant), each table entry
    /// is made null by `vspec::entry::new`, and the ghost `inv` is the
    /// table's invariant ([`TablePred`], in [`tcaches_ns`]), holding the
    /// permission `new` minted for each entry, at generation 0.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns.
    #[verifier::loop_isolation(false)]
    pub unsafe fn init(this: *mut Tcaches, Tracked(slot): Tracked<&mut Held<Tcaches>>)
        requires
            old(slot).perm matches Some(p) && p.ptr() == this && p.is_uninit(),
            this@.addr + size_of::<Tcaches>() <= usize::MAX,
        ensures
            final(slot).perm matches Some(p) && p.ptr() == this && p.is_init(),
    {
        let tracked (mut parts, mut table, pad) = Tcaches::split(slot.perm.tracked_take());
        // Ghost: each table entry's permission, as `put_entry` returns it
        // from `vspec::entry::new`.
        let tracked mut entries: Map<int, EntryPerm<u8>> = Map::tracked_empty();
        // SAFETY: the caller guarantees `this` is writable and unaliased;
        // every write goes through the permission to its field.
        unsafe {
            for i in 0..MAX_TCACHES
                invariant
                    table_at(table, this),
                    elems_init(table, 0, i as int),
                    forall|j: int| 0 <= j < i ==> #[trigger] entries.contains_key(j),
                    forall|j: int| 0 <= j < i ==> #[trigger] entries[j].id() == entry_id(&table[j].value()),
                    forall|j: int| 0 <= j < i ==> (#[trigger] entries[j]).value()@.addr == 0,
            {
                let ghost (t0, e0) = (table, entries);
                proof {
                    assert(table.contains_key(i as int));
                }
                let tracked t = table.tracked_borrow_mut(i as int);
                let Tracked(e) = Tcaches::put_entry(this, i, Tracked(t));
                proof {
                    entries.tracked_insert(i as int, e);
                    assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] table.contains_key(j) by {
                        assert(t0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < MAX_TCACHES implies (#[trigger] table[j]).ptr()
                        == ptr_add(field_ptr::<Tcaches, AtomicPtr<u8>>(this, T_TABLE), j as usize) by {
                        assert(t0.contains_key(j));
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies (#[trigger] table[j]).is_init() by {
                        if j < i {
                            assert(t0[j] == table[j]);
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies #[trigger] entries.contains_key(j) by {
                        if j < i {
                            assert(e0.contains_key(j));
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies #[trigger] entries[j].id() == entry_id(&table[j].value()) by {
                        if j < i {
                            assert(e0[j] == entries[j]);
                            assert(t0[j] == table[j]);
                        }
                    }
                    assert forall|j: int| 0 <= j < i + 1 implies (#[trigger] entries[j]).value()@.addr == 0 by {
                        if j < i {
                            assert(e0[j] == entries[j]);
                        }
                    }
                }
            }
            // The table's invariant: every entry null, at generation 0, and
            // no lower bound yet.
            let ghost ents = entries;
            let tracked (bounds, _) = GhostSetAuth::<(int, nat)>::new(Set::empty());
            let ghost key = TableKey { entries: Seq::new(MAX_TCACHES as nat, |j: int| ents[j].id()), bounds: bounds.id() };
            let tracked val = TableVal {
                entries,
                gens: Map::new(Set::range(0, MAX_TCACHES as int), |j: int| 0nat),
                bounds,
                tokens: Map::tracked_empty(),
            };
            proof {
                assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] entry_ok(key, val, j) by {
                    assert(ents.contains_key(j));
                }
                assert forall|j: int| 0 <= j < MAX_TCACHES && j != -1 implies #[trigger] has_bound(val, j) by {}
            }
            let tracked inv = TableInv::new(key, val, tcaches_ns());
            let inv: Tracked<TableInv> = Tracked(inv);
            proof {
                axiom_tcaches_layout();
            }
            ptr_mut_write(field_at(this, T_LOCK), Tracked(&mut parts.lock), Lock::new((), Ghost(rank_tcaches())));
            ptr_mut_write(field_at(this, T_INV), Tracked(&mut parts.inv), inv);
            proof {
                assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] parts.inv.value()@.constant().entries[j]
                    == entry_id(&table[j].value()) by {
                    assert(ents.contains_key(j));
                }
                slot.perm = Some(axiom_tcaches_assemble(parts, table, pad, this));
            }
        }
    }

    /// Entry `i` of the table of the `Tcaches` at `this`, made null
    /// through the permission `t` to it; the entry's permission.
    ///
    /// # Safety
    /// `this` is `init`'s storage and `i < MAX_TCACHES`.
    #[inline(always)]
    unsafe fn put_entry(this: *mut Tcaches, i: usize, Tracked(t): Tracked<&mut PointsTo<AtomicPtr<u8>>>) -> (r: Tracked<EntryPerm<u8>>)
        requires
            storage_ok(this),
            i < MAX_TCACHES,
            old(t).ptr() == ptr_add(field_ptr::<Tcaches, AtomicPtr<u8>>(this, T_TABLE), i),
            in_alloc(field_ptr::<Tcaches, AtomicPtr<u8>>(this, T_TABLE), MAX_TCACHES as int * size_of::<AtomicPtr<u8>>()),
            MAX_TCACHES * size_of::<AtomicPtr<u8>>() <= isize::MAX,
        ensures
            final(t).ptr() == old(t).ptr(),
            final(t).is_init(),
            r@.id() == entry_id(&final(t).value()),
            r@.value()@.addr == 0,
    {
        proof {
            axiom_tcaches_layout();
        }
        // SAFETY: element `i` of the field, through its permission.
        unsafe {
            let table: *mut AtomicPtr<u8> = field_at(this, T_TABLE);
            let (e, Tracked(eperm)) = crate::vspec::entry::new(ptr::null_mut());
            ptr_mut_write(elem_at(table, i, MAX_TCACHES), Tracked(t), e);
            Tracked(eperm)
        }
    }

    /// The table lock, for the fork handlers. It is the outermost
    /// allocator lock: `create` and `destroy` hold it across
    /// `Heap::create_detached` / `Heap::destroy`, which take the arena
    /// creation, class and metadata locks.
    pub fn raw_lock(&self) -> (r: &crate::lock::RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_tcaches(),
    {
        proof {
            use_type_invariant(self);
        }
        self.lock.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> crate::lock::RawLock {
        self.lock.spec_raw()
    }

    /// The identity ([`entry_id`]) of table entry `i`'s atomic.
    pub closed spec fn spec_entry(&self, i: int) -> int {
        entry_id(&self.table[i])
    }

    /// The location of the generations' lower bounds ([`Bound`]).
    pub closed spec fn spec_bounds(&self) -> Loc {
        self.inv@.constant().bounds
    }
}

/// Ghost record of a table entry as one access left it or read it: the
/// entry's atomic ([`entry_id`], which names the table), its index, its
/// generation ([`TableVal`]), the address it held, and for a read, the
/// lower bound on the generation the read checked against the table's
/// invariant (`floor`, from the reader's [`HeapUse::watch`]).
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub ghost struct EntrySeen {
    pub atomic: int,
    pub index: int,
    pub generation: nat,
    pub addr: usize,
    pub floor: Option<nat>,
}

/// A record the table's invariant agrees with ([`entry_ok`],
/// [`bound_ok`]): the entry was null exactly when its generation was
/// even, and its generation was at least the floor checked.
pub open spec fn seen_ok(s: EntrySeen) -> bool {
    &&& (s.addr == 0 <==> s.generation % 2 == 0)
    &&& (s.floor matches Some(f) ==> f <= s.generation)
}

/// Ghost: the right to hold one reference to an existing heap, which
/// [`heap_at`] (and through it [`Tcaches::get`]) lends for as long as the
/// reference lives, so one token gives one reference at a time.
/// `Heap::destroy` takes the token by value and keeps it, so a destroyed
/// heap cannot be reached again with the token that destroyed it. It also
/// keeps the record of the last table entry [`Tcaches::get`] read with it
/// ([`HeapUse::read`]), and a lower-bound token to check that read against
/// ([`HeapUse::watch`], set by [`HeapUse::watch_from`]). Its fields are
/// private, so outside this module only [`axiom_heap_use`] makes one.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct HeapUse {
    ghost unit: (),
    ghost read: Option<EntrySeen>,
    tracked watch: Option<Bound>,
}

impl HeapUse {
    /// A read record is one [`Tcaches::get`] made inside the table's
    /// invariant ([`seen_ok`]).
    #[verifier::type_invariant]
    spec fn type_inv(self) -> bool {
        self.read matches Some(s) ==> seen_ok(s)
    }

    /// The record of the last table entry [`Tcaches::get`] read with this
    /// token, if any.
    pub closed spec fn read(&self) -> Option<EntrySeen> {
        self.read
    }

    /// The lower bound this token carries for [`Tcaches::get`] to check,
    /// if any: `(entry, generation)`.
    pub closed spec fn watch(&self) -> Option<(int, nat)> {
        match self.watch {
            Some(b) => Some(b@),
            None => None,
        }
    }

    /// The location of the bounds that lower bound is in.
    pub closed spec fn watch_loc(&self) -> Loc {
        self.watch->Some_0.id()
    }

    /// Carry a copy of the lower-bound token `w` holds (a
    /// [`Tcaches::destroy`]'s), so the next [`Tcaches::get`] checks its
    /// read against it.
    pub proof fn watch_from(tracked &mut self, tracked w: &TableWrite)
        requires
            w.last() is Some,
        ensures
            final(self).read() == old(self).read(),
            final(self).watch() == Some((w.last()->Some_0.index, w.last()->Some_0.generation)),
            final(self).watch_loc() == w.bound_loc(),
    {
        use_type_invariant(w);
        use_type_invariant(&*self);
        let tracked b = w.bound.tracked_borrow().duplicate();
        *self = HeapUse { unit: (), read: self.read, watch: Some(b) };
    }
}

/// Ghost: the record of the table write a [`Tcaches::destroy`] made with
/// it, if any ([`TableWrite::last`]): the entry it set to null, at the
/// generation it left, and the lower-bound token for that generation
/// ([`Bound`]). Its fields are private: outside this module only
/// [`TableWrite::new`] makes one, holding none.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct TableWrite {
    ghost last: Option<EntrySeen>,
    tracked bound: Option<Bound>,
}

impl TableWrite {
    /// A record is one [`Tcaches::destroy`] made inside the table's
    /// invariant: a null entry at an even generation, no floor, with the
    /// lower-bound token for that entry and generation.
    #[verifier::type_invariant]
    spec fn type_inv(self) -> bool {
        self.last matches Some(s) ==> {
            &&& s.addr == 0
            &&& seen_ok(s)
            &&& s.floor is None
            &&& self.bound matches Some(b) && b@ == (s.index, s.generation)
        }
    }

    /// The record of the last table write made with this token, if any.
    pub closed spec fn last(&self) -> Option<EntrySeen> {
        self.last
    }

    /// The location of the bounds its lower-bound token is in.
    pub closed spec fn bound_loc(&self) -> Loc {
        self.bound->Some_0.id()
    }

    /// A token holding no record.
    pub proof fn new() -> (tracked w: TableWrite)
        ensures
            w.last() is None,
    {
        TableWrite { last: None, bound: None }
    }
}

/// Premise (the C caller's contract): the calling thread uses the heap it
/// is about to reach through [`heap_at`] with no other reference to it
/// live, here or on another thread: an explicit cache is used by one
/// thread at a time and not after `tcache.destroy` (the contract of
/// [`Tcaches::get`]), a destroyed cache is unreachable through the table,
/// a thread's heap reaches its key destructor once, at thread exit, and
/// a heap created and never published has no other reference.
/// One token per call that reaches heaps that way, made where it starts,
/// each before any reference it lends: `heap::mallocx`'s `alloc`,
/// `alloc_huge` and `free` (the `MALLOCX_TCACHE` paths), `api::jemalloc`'s
/// `sdallocx_routed`, [`Tcaches::flush`], [`Tcaches::destroy`],
/// `heap::thread::heap_destructor` and `heap::thread::create_heap_for_thread`
/// (the heap it could not register). The token it makes holds no read
/// record and no lower bound ([`HeapUse::read`] and [`HeapUse::watch`]
/// are `None`).
///
/// Checked by `heap::tcaches::tests::heap_use_axiom_only_at_listed_mint_sites`.
pub axiom fn axiom_heap_use() -> (tracked u: HeapUse)
    ensures
        u.read() is None,
        u.watch() is None,
;

/// A heap [`Heap::create`] or [`Heap::create_detached`] returned and
/// nothing has destroyed: an explicit cache's non-null table entry, or
/// the heap `Heap::destroy` is handed. Trusted: Verus has no raw-pointer
/// dereference, and such a heap is well formed ([`Heap::wf`]), as every
/// existing heap a verified path is handed: creation makes it so
/// ([`CachePerms::fresh`](super::CachePerms::fresh); `Tcaches::create`
/// takes no reference to its heap, so it drops them), every writer of
/// its caches proves it keeps `wf`, and no ghost state carries that from
/// one call to the next. `Heap::wf` includes `api::ready()` and that the
/// heap's `shared` is the process's `Shared`, so this asserts both: a
/// heap is created under a well-formed `&Shared` (`init_fields`). That
/// includes the heap's arena ticket ([`Heap::homed_ok`]): an explicit
/// cache is not counted and holds none; a thread heap holds the one its
/// creator installed and `set_arena` exchanged. The reference borrows
/// `u` ([`HeapUse`]) for as long as it lives, so two of them cannot be
/// live from one token, and two tokens rest on [`axiom_heap_use`]'s
/// premise: that is what keeps a ticket from being used twice.
/// It also includes the permissions to the `others` nodes' `arena`,
/// `pad` and `next` fields and the rest of their blocks with the blocks'
/// claims (`Heap::heads`). And the heap's home headers are the ones
/// creation wrote at `p` (`Heap::home_at`): `p` is the pointer creation
/// returned (the table stores it, and the C library hands back the value
/// registered with it), so it has the address and the provenance the
/// heap's home headers were recorded with; `Heap::destroy` gives the
/// heap's block back from it, a metadata block, which does not wrap the
/// address space. `Heap::home_at` also includes the rest of that block
/// the heap keeps (`Heap::head`): the permission to the 128 bytes before
/// the home headers and the block's claim. This asserts them as it
/// asserts the home headers' permissions: creation returns them (a thread
/// heap's go into its ghost state at [`heap_new`]; `Tcaches::create`
/// drops them with the cache permissions), and `Heap::destroy`, the one
/// place that takes them out, frees the block with them, so the block
/// goes back once per heap on the same premise as the ticket. No
/// memory precondition is stated: no caller can show
/// one. `Tcaches::get` has a pointer loaded from its table, the thread
/// key's destructor the one the C library hands it, and `Heap::destroy`
/// the one either of them hands on; the permissions creation returned
/// are dropped, and carrying them from one call to the next would need a
/// ghost registry of live explicit caches, which callers use without a
/// lock (the C caller's contract).
///
/// # Safety
/// `p` is such a heap, and nothing else uses it while the reference
/// lives (see [`Tcaches::get`]). The token is left as it was: the body
/// never names it.
#[verifier::external_body]
#[inline(always)]
pub(super) unsafe fn heap_at(p: *mut Heap, Tracked(_u): Tracked<&mut HeapUse>) -> (r: &mut Heap)
    ensures
        *final(_u) == *old(_u),
        r.wf(),
        r.home_at(p),
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *p }
}

/// The heap [`Heap::create`] has just returned, as a reference, before
/// its ghost state is installed, borrowed from the thread's heap loan
/// `loan`, which takes its permission ([`HeapPerm`], made here by
/// [`HeapPerm::assemble`]) and is lent for as long as the reference
/// lives: its `others` field is null, as `init_fields` stored it, and its
/// `shared`, `arena` and `bound` are what `s` records (`init_fields` and
/// `mark_bound` stored them). The permission to the heap's plain fields
/// and the block's claim, which `s` holds, go into the heap's ghost state
/// (`Heap::head`, [`FreshHeap::into_block`]), for `Heap::destroy` to free
/// the block with. Nothing else is claimed about the heap's ghost
/// fields: the caller installs the permissions and the ticket creation
/// returned (`create_heap_for_thread`), through the reference, so the
/// loan's permission describes the heap with them once the reference
/// ends.
///
/// # Safety
/// `p` is a heap `Heap::create` has just returned, and nothing else uses
/// it while the reference lives.
#[inline(always)]
pub(super) unsafe fn heap_new<'a>(
    p: *mut Heap,
    Tracked(s): Tracked<FreshHeap>,
    Tracked(perms): Tracked<&super::CachePerms>,
    Tracked(loan): Tracked<&'a mut HeapLoan>,
) -> (r: &'a mut Heap)
    requires
        s.heap() == p,
        super::heap_head(p, s.head()),
        super::heap_claim(p, s.claim()),
        perms.fresh(p),
        old(loan).heap() is None,
    ensures
        r.others@.addr == 0,
        r.arena as int == s.arena(),
        r.bound == s.bound(),
        r.shared == s.shared(),
        r.head@ == (s.head(), s.claim()),
        final(loan).heap() matches Some(hp) && hp.addr() == p@.addr && hp.value() == *final(r),
{
    let tracked hp = HeapPerm::assemble(&s, perms);
    proof {
        loan.put(hp);
    }
    // SAFETY: guaranteed by the caller.
    let r = unsafe { slot_heap_ref(p, Tracked(loan)) };
    proof {
        r.head = Tracked(s.into_block());
    }
    r
}

impl Tcaches {
    /// `tcache.create`: a new explicit cache, home arena `arena`. `None`
    /// when the table or the metadata region is full. The caller's record
    /// `fx` gets [`Change::Created`] when a cache is created. The store
    /// that publishes the cache moves its entry to an odd generation
    /// ([`TableVal`]): the next odd generation, which skips one when the
    /// table's invariant held a non-null pointer there (the search reads
    /// the table outside it).
    // Not isolated: the search returns while the guard borrows `ord`.
    #[verifier::loop_isolation(false)]
    pub fn create(&self, shared: &Shared, arena: u16, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u16>)
        requires
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_tcaches()),
        ensures
            r is Some ==> final(fx)@ == Change::Created,
            r is None ==> *final(fx) == *old(fx),
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let g = self.lock.lock(Tracked(ord), Tracked(&mut under));
        // The first null entry, lowest index first.
        let mut slot = 0;
        while !self.table[slot].load(Ordering::Relaxed).is_null()
            invariant
                slot < MAX_TCACHES,
            decreases MAX_TCACHES - slot,
        {
            slot += 1;
            if slot == MAX_TCACHES {
                g.unlock();
                return None;
            }
        }
        let (h, _, _) = Heap::create_detached(shared, arena, Tracked(&mut under));
        if h.is_null() {
            g.unlock();
            return None;
        }
        let hp = h as *mut u8;
        proof {
            assert(hp@.addr != 0);
        }
        // Inside the table's invariant ([`TablePred`]): the entry's
        // permission takes the pointer, at an odd generation.
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            proof {
                assert(entry_ok(self.inv@.constant(), v, slot as int));
            }
            let ghost v0 = v;
            let ghost was = v.entries[slot as int].value()@.addr;
            let ghost g = v.gens[slot as int];
            crate::vspec::entry::store(&self.table[slot], hp, Ordering::Release, Tracked(v.entries.tracked_borrow_mut(slot as int)));
            proof {
                v.gens = v.gens.insert(slot as int, if was == 0 { g + 1 } else { g + 2 });
                assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] entry_ok(self.inv@.constant(), v, j) by {
                    assert(entry_ok(self.inv@.constant(), v0, j));
                    if j != slot as int {
                        assert(v.entries[j] == v0.entries[j]);
                    }
                }
                assert forall|q: (int, nat)| #[trigger] v.bounds@.contains(q) implies bound_ok(self.inv@.constant(), v, q) by {
                    assert(bound_ok(self.inv@.constant(), v0, q));
                }
                assert forall|j: int| 0 <= j < MAX_TCACHES && j != slot as int implies #[trigger] has_bound(v, j) by {
                    assert(has_bound(v0, j));
                }
                // Record the generation the store moved the entry to.
                let tracked _ = lower_bound(self.inv@.constant(), &mut v, slot as int);
            }
        });
        proof {
            *fx = Ghost(Change::Created);
        }
        g.unlock();
        Some(slot as u16)
    }

    /// The explicit cache `id`, if it exists. Not thread safe: two threads
    /// using one explicit cache at once is the caller's bug, as in
    /// jemalloc, and so is using an id after `tcache.destroy`: `destroy`
    /// frees the heap while another thread may still hold the `&mut Heap`
    /// this returns. Every `MALLOCX_TCACHE(t)` path (`mallocx`, `rallocx`,
    /// `dallocx`, `sdallocx`) inherits that rule. The reference borrows
    /// the caller's token `u` ([`HeapUse`]) for as long as it lives, and
    /// `u` keeps the record of the entry read ([`HeapUse::read`]): the
    /// load, inside the table's invariant ([`TablePred`]), reads the
    /// entry's permission's value, with the entry's generation, and a
    /// cache is returned only for a non-null entry. When `u` carries a
    /// lower bound for this table's entry `id` ([`HeapUse::watch`]), the
    /// read checks it against the invariant: the record's `floor` is that
    /// bound, at most the generation read.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn get<'a>(&'a self, id: u16, Tracked(u): Tracked<&'a mut HeapUse>) -> (r: Option<&'a mut Heap>)
        ensures
            r matches Some(h) ==> h.wf(),
            // An id no `tcache.create` hands out names no cache.
            id as usize >= MAX_TCACHES ==> r is None,
            id as usize >= MAX_TCACHES ==> *final(u) == *old(u),
            (id as usize) < MAX_TCACHES ==> (final(u).read() matches Some(s) && s.index == id as int && s.atomic == self.spec_entry(id as int)
                && (r is Some <==> s.addr != 0)),
            (id as usize) < MAX_TCACHES ==> final(u).watch() == old(u).watch(),
            (id as usize) < MAX_TCACHES && (old(u).watch() matches Some(b) && b.0 == id as int && old(u).watch_loc() == self.spec_bounds())
                ==> final(u).read()->Some_0.floor == Some(old(u).watch()->Some_0.1),
    {
        let i = id as usize;
        if i >= MAX_TCACHES {
            return None;
        }
        proof {
            use_type_invariant(&*self);
            use_type_invariant(&*u);
        }
        // Ghost: whether `u`'s lower bound is for this table's entry `i`.
        let ghost watching = u.watch is Some && u.watch->Some_0.id() == self.inv@.constant().bounds
            && u.watch->Some_0@.0 == i as int;
        let p: *mut u8;
        let ghost mut g: nat = 0;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            proof {
                assert(entry_ok(self.inv@.constant(), v, i as int));
                if watching {
                    u.watch.tracked_borrow().agree(&v.bounds);
                    assert(bound_ok(self.inv@.constant(), v, u.watch->Some_0@));
                }
            }
            p = crate::vspec::entry::load(&self.table[i], Ordering::Acquire, Tracked(v.entries.tracked_borrow(i as int)));
            proof {
                g = v.gens[i as int];
            }
        });
        proof {
            let floor = if watching { Some(u.watch->Some_0@.1) } else { None };
            let tracked watch = if u.watch is Some { Some(u.watch.tracked_borrow().duplicate()) } else { None };
            *u = HeapUse {
                unit: (),
                read: Some(EntrySeen { atomic: self.inv@.constant().entries[i as int], index: i as int, generation: g, addr: p@.addr, floor }),
                watch,
            };
        }
        let p = p as *mut Heap;
        if p.is_null() {
            None
        } else {
            // SAFETY: published after `Heap::create_detached`; only `destroy`
            // frees it, and using a destroyed id is the caller's bug. `u`
            // keeps other users off the heap while the reference lives.
            Some(unsafe { heap_at(p, Tracked(u)) })
        }
    }

    /// `tcache.flush`: return every cached slot of `id` to its slabs.
    /// `false` if there is no such cache. Afterwards every home and
    /// `others`-node cache of the explicit cache's heap is empty
    /// ([`Heap::flush_all`]). Its aborts are `flush_all`'s, so it takes
    /// `flush_all`'s capability; the table lookup does not abort.
    pub fn flush(&self, shared: &Shared, id: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: bool)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            id as usize >= MAX_TCACHES ==> !r,
            *final(ord) == *old(ord),
    {
        // Premise (`axiom_heap_use`): `tcache.flush`'s caller uses the
        // cache on no other thread, and this call holds no other reference.
        let tracked mut u = axiom_heap_use();
        match self.get(id, Tracked(&mut u)) {
            Some(h) => {
                h.flush_all(shared, Tracked(ord));
                proof {
                    assert(h.drained());
                }
                true
            }
            None => false,
        }
    }

    /// Number of live explicit caches (for tests and stats).
    pub fn len(&self) -> usize {
        // The non-null entries.
        let mut n = 0;
        for i in 0..MAX_TCACHES
            invariant
                n <= i,
        {
            if !self.table[i].load(Ordering::Relaxed).is_null() {
                n += 1;
            }
        }
        n
    }
}

} // verus!

impl Tcaches {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

verus! {

impl Tcaches {
    /// `tcache.destroy`: flush and free `id`. `false` if there is no such
    /// cache. For an id below [`MAX_TCACHES`], the swap that unpublishes
    /// the cache, before the heap is freed, sets the entry's permission to
    /// null inside the table's invariant ([`TablePred`]), moving a
    /// non-null entry to the next generation, an even one, and `w` gets
    /// the record of it: the entry, null, at that generation, with the
    /// lower-bound token for it ([`Bound`]).
    pub fn destroy(&self, shared: &Shared, id: u16, Tracked(w): Tracked<&mut TableWrite>, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_tcaches()),
        ensures
            *final(ord) == *old(ord),
            id as usize >= MAX_TCACHES ==> final(w).last() == old(w).last(),
            (id as usize) < MAX_TCACHES ==> (final(w).last() matches Some(s) && s.index == id as int && s.atomic == self.spec_entry(id as int)),
            (id as usize) < MAX_TCACHES ==> final(w).bound_loc() == self.spec_bounds(),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let guard = self.lock.lock(Tracked(ord), Tracked(&mut under));
        let i = id as usize;
        if i >= MAX_TCACHES {
            guard.unlock();
            return false;
        }
        let null = ptr::null_mut();
        let p: *mut u8;
        let ghost mut g: nat = 0;
        let tracked mut bound: Option<Bound> = None;
        vstd::open_atomic_invariant!(self.inv.borrow() => v => {
            proof {
                assert(entry_ok(self.inv@.constant(), v, i as int));
            }
            let ghost v0 = v;
            let ghost was = v.gens[i as int];
            p = crate::vspec::entry::swap(&self.table[i], null, Ordering::AcqRel, Tracked(v.entries.tracked_borrow_mut(i as int)));
            proof {
                g = if p@.addr == 0 { was } else { was + 1 };
                v.gens = v.gens.insert(i as int, g);
                assert forall|j: int| 0 <= j < MAX_TCACHES implies #[trigger] entry_ok(self.inv@.constant(), v, j) by {
                    assert(entry_ok(self.inv@.constant(), v0, j));
                    if j != i as int {
                        assert(v.entries[j] == v0.entries[j]);
                    }
                }
                assert forall|q: (int, nat)| #[trigger] v.bounds@.contains(q) implies bound_ok(self.inv@.constant(), v, q) by {
                    assert(bound_ok(self.inv@.constant(), v0, q));
                }
                assert forall|j: int| 0 <= j < MAX_TCACHES && j != i as int implies #[trigger] has_bound(v, j) by {
                    assert(has_bound(v0, j));
                }
                bound = Some(lower_bound(self.inv@.constant(), &mut v, i as int));
            }
        });
        proof {
            *w = TableWrite {
                last: Some(EntrySeen { atomic: self.inv@.constant().entries[i as int], index: i as int, generation: g, addr: 0, floor: None }),
                bound,
            };
        }
        let p = p as *mut Heap;
        if p.is_null() {
            guard.unlock();
            return false;
        }
        // Premise (`axiom_heap_use`): `p` is now unreachable through the
        // table, and `tcache.destroy`'s caller uses the cache on no other
        // thread.
        let tracked u = axiom_heap_use();
        // SAFETY: `p` came from `create_detached` and is now unreachable
        // through the table.
        unsafe { Heap::destroy(p, shared, Tracked(u), Tracked(&mut under)) };
        guard.unlock();
        true
    }
}

/// A destroyed explicit cache is unreachable until `tcache.create` hands
/// its id out again. [`Tcaches::destroy`] of an id below [`MAX_TCACHES`]
/// sets the entry to null inside the table's invariant ([`TablePred`]),
/// before the heap is freed, and leaves in `w` its record (`cleared`): the
/// entry's atomic, null, at an even generation ([`TableVal`]), with a
/// lower-bound token for that generation ([`Bound`]). The caller hands a
/// copy of the token to `u` ([`HeapUse::watch_from`]), and a
/// [`Tcaches::get`] of `id` with `u` loads the entry inside the same
/// invariant: its record in `u` (`read`) holds the entry's atomic, the
/// address the entry's permission holds at the load and the entry's
/// generation, checked against the token (`floor`), and `found` is its
/// `r is Some`. `Tcaches::destroy_then_get` checks that those calls, in
/// that order, meet these premises.
///
/// Then the generation get read is at least the one destroy left (the
/// table's invariant keeps every lower bound at most its entry's
/// generation, and every moved entry's generation has one, so no write
/// moves a generation back), and if get found a cache, it read a later
/// generation. So a get that reads the generation destroy left finds no
/// cache: it does not reach the freed heap. The theorem does not cover the
/// `MALLOCX_TCACHE(id)` paths: they make their tokens with `axiom_heap_use`,
/// which carries no lower bound, so their ending in `heap::mallocx`'s
/// `invalid_tcache` stays that axiom's premise. That
/// a later, odd generation means a `create` of `id` rests on reading this
/// module, not on a specification: an entry turns odd only when a non-null
/// pointer is stored in it, and the table's only such store is
/// [`Tcaches::create`]'s, at the id it returns (`vspec::entry`'s list of
/// writes).
///
/// The proof model is sequentially consistent (`vspec::entry`): both
/// records come from the table's invariant, so neither the orderings of
/// the accesses nor a history of the table's writes are premises here.
/// Using an explicit cache concurrently with its `tcache.destroy` stays
/// the caller's bug, as in jemalloc ([`Tcaches::get`]). Trusted:
/// `vspec::entry`'s wrapped atomic operations and its identity rules, and
/// the layout ([`axiom_tcaches_layout`]) and assembly
/// ([`axiom_tcaches_assemble`]) [`Tcaches::init`] builds the table's
/// invariant with.
pub proof fn theorem_tcache_destroyed_none(id: u16, tracked w: &TableWrite, tracked u: &HeapUse, found: bool)
    requires
        (id as usize) < MAX_TCACHES,
        w.last() matches Some(cleared) && cleared.index == id as int,
        u.read() matches Some(read) && read.index == id as int && (found <==> read.addr != 0)
            && read.floor == Some(w.last()->Some_0.generation) && read.atomic == w.last()->Some_0.atomic,
    ensures
        u.read()->Some_0.generation >= w.last()->Some_0.generation,
        found ==> u.read()->Some_0.generation > w.last()->Some_0.generation,
        u.read()->Some_0.generation == w.last()->Some_0.generation ==> !found,
{
    use_type_invariant(w);
    use_type_invariant(u);
}

impl Tcaches {
    /// Verification only: [`theorem_tcache_destroyed_none`]'s premises are
    /// what [`Tcaches::destroy`], [`HeapUse::watch_from`] and
    /// [`Tcaches::get`] leave, called in that order for one id with the
    /// caller's token `u`; `r` is whether get found a cache, which it does
    /// only at a later generation than the one destroy left.
    #[cfg(verus_keep_ghost)]
    fn destroy_then_get(&self, shared: &Shared, id: u16, Tracked(u): Tracked<&mut HeapUse>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: bool)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_tcaches()),
            (id as usize) < MAX_TCACHES,
        ensures
            r ==> final(u).read()->Some_0.generation > final(u).read()->Some_0.floor->Some_0,
    {
        let tracked mut w = TableWrite::new();
        self.destroy(shared, id, Tracked(&mut w), Tracked(ord));
        proof {
            u.watch_from(&w);
        }
        let found = self.get(id, Tracked(&mut *u)).is_some();
        proof {
            theorem_tcache_destroyed_none(id, &w, &*u, found);
        }
        found
    }
}

} // verus!

verus! {

/// A lock that guards no value (`()`): the explicit-cache table lock, and
/// any other `Lock<()>`. Its invariant and frame are `true`. Nothing
/// writes the value.
///
/// The table lock's holders, `Tcaches::create` and `Tcaches::destroy`,
/// hold it only to serialise their table updates, and end in
/// `LockGuard::unlock` (`api::tests::guards_end_in_unlock`).
/// `api::tests::lock_holders_match_inv_docs` checks that the functions
/// that take the guard are the holders named here.
impl crate::lock::LockSpec for () {
    open spec fn lock_spec_inv(self) -> bool {
        true
    }

    open spec fn lock_spec_frame(init: Self, v: Self) -> bool {
        true
    }
}

/// The frame holds of every value with itself, as `LockSpec` requires of
/// a defined frame (`Lock::new` does not check it, and
/// `lock::axiom_acquired` claims it at the first acquisition).
proof fn lemma_unit_lock_frame_reflexive(v: ())
    ensures
        crate::lock::lock_frame(v, v),
{
}

} // verus!

#[cfg(test)]
impl Tcaches {
    /// The pointer entry `id` holds (null for no cache), as
    /// [`Tcaches::get`] loads it and hands it to [`heap_at`].
    pub(crate) fn entry(&self, id: u16) -> *mut Heap {
        self.table[id as usize].load(Ordering::Acquire) as *mut Heap
    }
}

#[cfg(test)]
mod tests {
    /// `axiom_heap_use` is called once in each of the 8 functions its doc
    /// lists, and nowhere else: each call rests on that doc's premise for
    /// its own caller, so a new call needs a premise of its own.
    #[test]
    fn heap_use_axiom_only_at_listed_mint_sites() {
        crate::testutil::assert_only_at(
            concat!("axiom_", "heap_use("),
            &[
                ("heap/mallocx.rs:", "pub fn alloc(", 1),
                ("heap/mallocx.rs:", "pub fn alloc_huge(", 1),
                ("heap/mallocx.rs:", "pub fn free(", 1),
                ("api/jemalloc.rs:", "fn sdallocx_routed(", 1),
                ("heap/tcaches.rs:", "pub fn flush(", 1),
                ("heap/tcaches.rs:", "pub fn destroy(", 1),
                (
                    "heap/thread.rs:",
                    "pub unsafe extern \"C\" fn heap_destructor(",
                    1,
                ),
                ("heap/thread.rs:", "fn create_heap_for_thread<", 1),
            ],
        );
    }
}
