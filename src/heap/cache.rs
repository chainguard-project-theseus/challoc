//! The cache header and its stack: the pop and push primitives every
//! path shares, and the first-use setup that gives a header its capacity
//! and stack ([`Heap::ensure_cache`]).

use core::alloc::Layout;
use core::ptr;

use crate::layout::NCLASSES;
use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::rank_meta;
use crate::sys::pac;
use crate::vspec::addr::addr_to_ptr;

#[cfg(verus_only)]
use super::{
    ArenaCaches, axiom_fresh_stack, cell_ok, hand_out, lemma_handed_ok, lemma_plain_stored,
    lemma_stack_push, lemma_stack_shrink, node_hdr, pac_mode, popped, regions_ok, slot_in,
    slot_ptr, slot_start, slot_token, stack_ok,
};
use super::{CachePerms, FOREIGN, Heap, Live, MIN_CAP, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, Shared};
use super::{MODE_OFF, MODE_PROBE};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::layout::process_regions;
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, int, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_ref, ptr_mut_write, ptr_ref};
use vstd::resource::set::GhostSingleton;

verus! {

/// inline in the heap ([`Heap::caches`]) or in an [`ArenaCaches`](super::ArenaCaches) node,
/// pointing at its stack of `cap` slot pointers, a metadata object of its
/// own. The fields the fast paths read (`len`, `cap`, `zeroed`, `slots`,
/// `allocs`, `compact_mask`, `low_water`) come first; a header whose `cap`
/// is zero has never been used (`slots` is null and never dereferenced,
/// because `len == cap == 0` sends both fast paths to the slow path).
///
/// The byte counters `thread.allocated` and `thread.deallocated` are not
/// kept per operation. Every pop bumps `allocs` on the header line the pop
/// writes anyway, and the number of pushes follows from the stack
/// arithmetic `len = (frees + refilled) - (allocs + flushed)`, with
/// `moved = refilled - flushed` maintained on the slow paths; see
/// [`Heap::stats`].
#[repr(C, align(64))]
pub struct CacheHeader {
    pub(super) len: u32,
    pub(super) cap: u32,
    /// Smallest `len` since the last GC (design 6.4).
    pub(super) low_water: u32,
    /// `slots[..zeroed]` are known to read as zero. `zeroed <= low_water`:
    /// a pop that takes `len` below `low_water` lowers `zeroed` with it
    /// ([`Heap::pop_top`]), so a push, which stores at `len >= zeroed`,
    /// never touches it.
    pub(super) zeroed: u32,
    /// The stack: a metadata object of `cap` slot pointers
    /// ([`CacheHeader::slots_layout`]; null while `cap == 0`).
    pub(super) slots: *mut *mut u8,
    /// Pops served from this cache since it was created.
    pub(super) allocs: u64,
    /// A pop whose `allocs` count has no bit of this mask set is a
    /// scheduled pop: [`PROBE_MASK`](super::PROBE_MASK),
    /// [`COMPACT_MASK`](super::COMPACT_MASK), or `u32::MAX` when
    /// compaction is off.
    pub(super) compact_mask: u32,
    /// `slots[..run_end]` is the run of low addresses, descending (the
    /// lowest at `run_end - 1`); `run_end <= low_water`.
    pub(super) run_end: u32,
    /// Slots moved in by refills minus slots moved out by flushes
    /// (wrapping; the true value is `len - frees + allocs`).
    pub(super) moved: u64,
    /// The class's refill counter (low bits) at the previous probe: a class
    /// that ordinary refills are draining is not compacted.
    pub(super) probe_refilled: u32,
    /// The arena this cache belongs to (a rebuild's claim needs it).
    pub(super) arena: u16,
    /// [`MODE_OFF`], [`MODE_PROBE`] or [`MODE_COMPACT`](super::MODE_COMPACT).
    pub(super) mode: u16,
    /// Consecutive flushes whose slot array was not already in address
    /// order (wrapping); zeroed by a flush that found it sorted. Drives the
    /// kept half's sort in [`Heap::flush`].
    pub(super) unsorted_flushes: u8,
}

/// Size of a [`CacheHeader`]: one line, so a class's header never shares
/// a line with another class's.
pub exec const CACHE_HEADER: usize
    ensures
        CACHE_HEADER == 64,
{
    core::mem::size_of::<CacheHeader>()
}

// Verus checks the size and alignment `CACHE_HEADER` names.
global layout CacheHeader is size == 64, align == 64;

/// The size and alignment `global layout` gives [`CacheHeader`], for
/// modules that do not see the declaration.
pub(super) proof fn lemma_header_layout()
    ensures
        vstd::layout::size_of::<CacheHeader>() == 64,
        vstd::layout::align_of::<CacheHeader>() == 64,
{
}

impl CacheHeader {
    /// The header invariant: the run lies inside the entries not popped
    /// since the last GC, which lie inside the stack, which lies inside
    /// its capacity (`run_end <= low_water <= len <= cap`), and the zero
    /// prefix lies inside the entries not popped since the last GC
    /// (`zeroed <= low_water`, see [`CacheHeader::zeroed`]).
    pub(super) open spec fn inv(&self) -> bool {
        &&& self.run_end <= self.low_water <= self.len <= self.cap
        &&& self.zeroed <= self.low_water
    }
}

} // verus!

const _: () = assert!(CACHE_HEADER == 64 && CACHE_HEADER == crate::layout::CLASS_STRIDE);
const _: () = assert!(core::mem::offset_of!(CacheHeader, slots) < 64 - 8);

verus! {

impl CacheHeader {
    /// A header that has never been used: both fast paths fall through to
    /// the slow path, which gives it a capacity and a stack.
    pub(super) exec const EMPTY: CacheHeader
        ensures
            Self::EMPTY.len == 0,
            Self::EMPTY.cap == 0,
            Self::EMPTY.low_water == 0,
            Self::EMPTY.zeroed == 0,
            Self::EMPTY.run_end == 0,
    {
        CacheHeader {
            len: 0,
            cap: 0,
            low_water: 0,
            zeroed: 0,
            slots: ptr::null_mut(),
            allocs: 0,
            compact_mask: u32::MAX,
            run_end: 0,
            moved: 0,
            probe_refilled: 0,
            arena: 0,
            mode: MODE_OFF,
            unsorted_flushes: 0,
        }
    }

    /// Metadata layout of a stack of `cap` slots.
    pub const fn slots_layout(cap: u32) -> (r: Layout)
        ensures
            crate::vspec::layout::layout_size(r) == 8 * cap,
            crate::vspec::layout::layout_align(r) == 16,
    {
        proof {
            assert(16usize & sub(16usize, 1) == 0) by (bit_vector);
        }
        match Layout::from_size_align(8 * cap as usize, 16) {
            Ok(l) => l,
            Err(_) => unreachable!(),
        }
    }

    #[inline(always)]
    pub(super) fn slots(&mut self) -> (r: *mut *mut u8)
        ensures
            r == old(self).slots,
            *final(self) == *old(self),
        opens_invariants none
    {
        self.slots
    }

    /// Sign every entry of `entries` (plain pointers) in place with the
    /// stack base `slots` as the modifier ([`PUSH_PAC`]).
    #[inline]
    pub(super) fn sign_all(slots: *mut *mut u8, entries: &mut [*mut u8])
        requires
            pac::pac_available(),
        ensures
            final(entries)@.len() == old(entries)@.len(),
            forall|i: int| 0 <= i < old(entries)@.len() ==>
                (#[trigger] final(entries)@[i])@.addr == pac::spec_sign(old(entries)@[i]@.addr, slots@.addr),
    {
        let m = slots as usize;
        let ghost e0 = entries@;
        for e in it: entries
            invariant
                m == slots@.addr,
                pac::pac_available(),
                it.seq().len() == e0.len(),
                forall|i: int| 0 <= i < it.seq().len() ==> *(#[trigger] it.seq()[i]) == e0[i],
                forall|i: int| 0 <= i < it.index() ==> (*final(#[trigger] it.seq()[i]))@.addr == pac::spec_sign(e0[i]@.addr, m),
        {
            *e = addr_to_ptr(pac::sign(*e as usize, m));
        }
    }

    /// Authenticate every entry of `entries` (signed) in place.
    #[inline]
    pub(super) fn auth_all(slots: *mut *mut u8, entries: &mut [*mut u8])
        requires
            pac::pac_available(),
        ensures
            final(entries)@.len() == old(entries)@.len(),
            forall|i: int| 0 <= i < old(entries)@.len() ==>
                (#[trigger] final(entries)@[i])@.addr == pac::spec_auth(old(entries)@[i]@.addr, slots@.addr),
    {
        let m = slots as usize;
        let ghost e0 = entries@;
        for e in it: entries
            invariant
                m == slots@.addr,
                pac::pac_available(),
                it.seq().len() == e0.len(),
                forall|i: int| 0 <= i < it.seq().len() ==> *(#[trigger] it.seq()[i]) == e0[i],
                forall|i: int| 0 <= i < it.index() ==> (*final(#[trigger] it.seq()[i]))@.addr == pac::spec_auth(e0[i]@.addr, m),
        {
            *e = addr_to_ptr(pac::auth(*e as usize, m));
        }
    }
}

} // verus!

impl CacheHeader {
    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    #[cfg(test)]
    pub fn cap(&self) -> usize {
        self.cap as usize
    }

    #[inline]
    #[cfg(test)]
    pub fn low_water(&self) -> usize {
        self.low_water as usize
    }

    /// Number of entries at the bottom of the stack known to be zero.
    #[inline]
    #[cfg(test)]
    pub fn zeroed(&self) -> usize {
        self.zeroed as usize
    }

    /// The run of low addresses, `slots[..run_end]`, descending, as
    /// stored: signed when `Shared::pac()` ([`CacheHeader::plain`]).
    #[cfg(test)]
    pub fn run(&self) -> &[*mut u8] {
        debug_assert!(self.run_end <= self.low_water && self.low_water <= self.len);
        if self.run_end == 0 {
            return &[];
        }
        // SAFETY: `run_end <= len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.run_end as usize) }
    }

    /// Cached pointers, bottom of the stack first, as stored: signed when
    /// `Shared::pac()` ([`CacheHeader::plain`]).
    #[cfg(test)]
    pub fn cached(&self) -> &[*mut u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len <= cap` pointers exist at `slots`.
        unsafe { core::slice::from_raw_parts(self.slots, self.len as usize) }
    }

    /// Pops served from this cache since it was created.
    #[cfg(test)]
    pub fn allocs(&self) -> u64 {
        self.allocs
    }

    /// The plain pointer a stored entry names: authenticated when `pac`
    /// (`Shared::pac()`), the entry itself otherwise. For tests reading
    /// [`CacheHeader::cached`] or [`CacheHeader::run`], which return the
    /// entries as stored.
    #[cfg(test)]
    pub fn plain(&self, e: *mut u8, pac: bool) -> *mut u8 {
        if pac {
            pac::auth(e as usize, self.slots as usize) as *mut u8
        } else {
            e
        }
    }
}

verus! {

impl Heap {
    /// The home header for `class`: `heap + 128 + 64 * class`, no load.
    /// Trusted: `&raw mut` and the array-to-element cast, which compute
    /// the address creation wrote the header at ([`home_hdr`](super::home_hdr)
    /// of the heap's address), as the heap's record has it
    /// ([`Heap::home_ptr`]). `create_heap_for_thread` makes the record; for
    /// a heap `tcaches::heap_at` or `thread::heap_from_slot` hands out it
    /// is assumed with `wf`. It holds of a heap at the address it was
    /// created at: verified code never moves, swaps or builds a `Heap`.
    #[inline(always)]
    #[verifier::external_body]
    fn home_ptr_at(&mut self, class: usize) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
        ensures
            *final(self) == *old(self),
            r == old(self).home_ptr(class as int),
        opens_invariants none
    {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { (&raw mut self.caches).cast::<CacheHeader>().add(class) }
    }

    /// The home header for `class` ([`Heap::home_ok`]): owned, caching
    /// `class`, and no header of a node on the `others` list.
    #[inline(always)]
    pub(super) fn cache_ptr(&mut self, class: usize) -> (r: *mut CacheHeader)
        requires
            class < NCLASSES,
            old(self).wf(),
        ensures
            *final(self) == *old(self),
            old(self).owns(r),
            old(self).hdr_class(r) == class,
            forall|n: *mut ArenaCaches, k: int| old(self).in_list(n) && 0 <= k < NCLASSES ==> r != #[trigger] node_hdr(n, k),
            r == old(self).home_ptr(class as int),
        opens_invariants none
    {
        let r = self.home_ptr_at(class);
        proof {
            assert forall|n: *mut ArenaCaches, k: int| old(self).in_list(n) && 0 <= k < NCLASSES implies r != #[trigger] node_hdr(n, k) by {
                let i = choose|i: int| 0 <= i < self.list@.len() && self.list@[i] == n;
                assert(self.home_hdrs@[class as int] != node_hdr(self.list@[i], k));
            }
        }
        r
    }

    /// Pop the top entry of a non-empty cache. Touches only the header
    /// line and the slot: `allocs` is the only counter, `low_water` is
    /// written only when it actually drops (and the run clipped with it),
    /// and the class size is never looked up. Every `compact_mask + 1`
    /// pops go to [`Heap::pop_compact`] instead.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `class`, one a pop may take
    /// from ([`Heap::poppable`]: the home arena's or an arena's node's,
    /// never the [`FOREIGN`] node's), and `len` is its
    /// current non-zero length. `len` is only a copy: the scheduled pop
    /// runs `tick`, whose GC may flush this cache, and reloads it. `pac`
    /// is `Shared::pac()`: whether the entry is stored signed.
    #[inline(always)]
    pub(super) unsafe fn pop(&mut self, cache: *mut CacheHeader, class: usize, len: u32, pac: bool, Tracked(blk): Tracked<&mut Live>) -> (r: (*mut u8, bool))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
            old(self).poppable(class as int, cache),
            len == old(self).hdr(cache).len,
            len > 0,
            pac ==> pac::pac_available(),
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 && pac == pac_mode() ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 && pac == pac_mode() ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 && pac == pac_mode() ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        let ghost old_perms = self.perms@;
        let tracked mut spare = Map::<int, GhostSingleton<int>>::tracked_empty();
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        let h = ptr_mut_ref(cache, Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)));
        let allocs = h.allocs.wrapping_add(1);
        h.allocs = allocs;
        if ((allocs as u32) & h.compact_mask) == 0 {
            proof {
                CachePerms::lemma_update(old_perms, self.perms@, cache);
                CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
                CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
                Heap::lemma_poppable_same(*old(self), *self, class as int, cache);
            }
            // SAFETY: guaranteed by the caller.
            return unsafe { self.pop_compact(cache, class, len, Tracked(blk)) };
        }
        proof {
            CachePerms::lemma_update(old_perms, self.perms@, cache);
            CachePerms::lemma_zero_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_tracks_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_tokened_hdr(old_perms, self.perms@, cache);
            CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
        }
        // SAFETY: guaranteed by the caller.
        let r = unsafe { Self::pop_top(cache, len, pac, Tracked(self.perms.borrow_mut()), Tracked(&mut spare)) };
        proof {
            if pac == pac_mode() {
                hand_out(&mut spare, blk, class as int, r.0);
            }
        }
        r
    }

    /// The ordinary pop of the top entry, `allocs` already counted. With
    /// `pac` the entry is authenticated against the stack's base address
    /// (`autda`; [`CacheHeader::plain`]): a corrupted entry faults here on
    /// FEAT_FPAC, or comes out non-canonical and faults on its first use.
    /// The entry is known zero only if it is below `zeroed`, which is at
    /// most `low_water`, so the zero test and the prefix's shrink live in
    /// the branch that lowers `low_water`.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    #[inline(always)]
    pub(super) unsafe fn pop_top(
        cache: *mut CacheHeader,
        len: u32,
        pac: bool,
        Tracked(perms): Tracked<&mut CachePerms>,
        Tracked(out): Tracked<&mut Map<int, GhostSingleton<int>>>,
    ) -> (r: (*mut u8, bool))
        requires
            old(perms).wf(),
            old(perms).tracks(),
            old(perms).zero_ok(),
            old(perms).hdrs.contains_key(cache),
            len == old(perms).hdr(cache).len,
            len > 0,
            pac ==> pac::pac_available(),
        ensures
            final(perms).keeps(*old(perms)),
            old(perms).claimed() ==> final(perms).claimed(),
            final(perms).follows(*old(perms)),
            final(perms).zero_ok(),
            r.1 && pac == pac_mode() ==> CachePerms::handed_ok(final(perms).handed, r.0, old(perms).cls[cache]),
            pac == pac_mode() ==> process_regions().is_slot(old(perms).cls[cache], r.0@.addr),
            old(perms).tokened() ==> popped(*old(perms), *final(perms), *final(out), cache, pac == pac_mode(), r.0),
        opens_invariants none
    {
        broadcast use vstd::layout::layout_of_references_and_pointers_for_sized_types;

        let ghost old_perms = *perms;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // SAFETY: guaranteed by the caller.
        unsafe {
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            let len = len - 1;
            h.len = len;
            let mut zero = false;
            if len < h.low_water {
                h.low_water = len;
                if len < h.run_end {
                    h.run_end = len;
                }
                if len < h.zeroed {
                    h.zeroed = len;
                    zero = true;
                }
            }
            let slots = h.slots();
            proof {
                assert(zero == (len < old_perms.hdr(cache).zeroed));
                assert(slot_ptr(slots, len as int) == crate::vspec::ptr::ptr_add(slots, len as usize));
                assert(slots == old_perms.hdr(cache).slots);
                assert(slot_in(slots, len as int));
                assert(vstd::layout::size_of::<*mut u8>() == 8);
                let sz = vstd::layout::size_of::<*mut u8>() as int;
                assert((len as usize) as int * sz <= isize::MAX) by (nonlinear_arith)
                    requires
                        sz == 8,
                        len <= u32::MAX;
            }
            let p = *ptr_ref(slots.add(len as usize), Tracked(perms.slots.tracked_borrow(slot_ptr(slots, len as int))));
            let p = if pac { addr_to_ptr(pac::auth(p as usize, slots as usize)) } else { p };
            proof {
                lemma_stack_shrink(old_perms.hdr(cache), *h, perms.slots);
                CachePerms::lemma_update(old_perms, *perms, cache);
                CachePerms::lemma_tracks_hdr(old_perms, *perms, cache);
                assert(old_perms.ent(cache, len as int) == slot_ptr(slots, len as int));
            }
            proof {
                if old_perms.tokened() {
                    let before = *perms;
                    let tracked t = perms.toks.tracked_remove(slot_ptr(slots, len as int));
                    out.tracked_insert(0, t);
                    CachePerms::lemma_toks_only(before, *perms, old_perms);
                    CachePerms::lemma_tokened_pop(old_perms, *perms, cache);
                }
                let ghost mid = *perms;
                if zero {
                    old_perms.lemma_zero_get(cache, len as int);
                    let tracked t = perms.zero.tracked_remove(slot_ptr(slots, len as int));
                    perms.handed = Some(t);
                }
                assert forall|i: int| #[trigger] CachePerms::in_prefix(perms.hdr(cache), i) implies perms.zero_at(cache, i) by {
                    old_perms.lemma_zero_get(cache, i);
                    assert(old_perms.owner[old_perms.ent(cache, i)] == (cache, i));
                    assert(old_perms.owner[old_perms.ent(cache, len as int)] == (cache, len as int));
                }
                CachePerms::lemma_zero_update(old_perms, *perms, cache);
                CachePerms::lemma_same_but_tokens(mid, *perms, old_perms);
                if old_perms.claimed() {
                    CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
                }
            }
            (p, zero)
        }
    }


    /// Push a checked slot of `class` (home arena) onto its cache.
    #[inline(always)]
    pub(super) fn push(&mut self, shared: &Shared, class: usize, p: *mut u8, Tracked(blk): Tracked<Live>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            process_regions().is_slot(class as int, p@.addr),
            blk.slot_block(p, class as int),
            shared.wf(),
            class < NCLASSES,
            old(self).wf(),
        ensures
            final(self).keeps(*old(self)),
    {
        // The slot's token goes onto the cache; the permission to its bytes
        // is dropped with it (`Live::take_slot`), and a fresh one comes
        // back with the token when the push declines.
        let tracked mut blk = blk;
        let ghost orig = blk;
        let tracked mut tk = Map::<int, GhostSingleton<int>>::tracked_empty();
        proof {
            shared.lemma_regions_ok();
            blk.lemma_mem();
            let tracked tok = blk.take_slot(class as int);
            tk.tracked_insert(0, tok);
        }
        if !self.push_fast(class, p, shared.push_flags(), Tracked(&mut tk)) {
            proof {
                blk.put_slot(tk.tracked_remove(0), orig, p, class as int);
            }
            self.free_slow(shared, class, p, Tracked(blk));
        }
    }

    /// The cache push alone: `false` when the cache is full (or still the
    /// sentinel) and the caller must take a slow path. No counter moves:
    /// the push count is implied by `len`, `allocs` and `moved`. `zeroed` is
    /// never above the pushed position (see [`CacheHeader::zeroed`]).
    /// `flags` is `Shared::push_flags`.
    ///
    /// `tk[0]` is the pushed slot's token: it goes onto the stack with the
    /// slot ([`Heap::tokened`]; `verus/TODO.md`, "Uniqueness: slot tokens"),
    /// or stays in `tk` for the slow path when the push declines.
    #[inline(always)]
    pub fn push_fast(&mut self, class: usize, p: *mut u8, flags: u8, Tracked(tk): Tracked<&mut Map<int, GhostSingleton<int>>>) -> (r: bool)
        requires
            (flags & PUSH_PAC != 0) == pac_mode(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
            process_regions().is_slot(class as int, p@.addr),
            class < NCLASSES,
            old(self).wf(),
            slot_token(*old(tk), class as int, true, p),
        ensures
            flags & PUSH_ZERO != 0 ==> !r,
            final(self).keeps(*old(self)),
            !r ==> *final(tk) == *old(tk),
        opens_invariants none
    {
        let cache = self.cache_ptr(class);
        // SAFETY: this thread's own header, unused (`len == cap == 0`, so
        // nothing is written) or with a stack of `cap` slots.
        unsafe { Self::push_cache_flags(cache, p, flags, Tracked(self.perms.borrow_mut()), Tracked(tk)) }
    }

    /// [`Heap::push_cache`] behind the opt-in hardening: with `flags` zero
    /// (the default on every host: `pac_pointers` defaults to false, so
    /// even a PAC host's byte is 0) it is the plain push after one byte
    /// test. On aarch64 the signed-only push (`pac_pointers:true` on a
    /// host with PAC, `flags == PUSH_PAC`) is tested first, one compare
    /// ahead of the default's; off aarch64 `PUSH_PAC` is never set and
    /// that compare is compiled out, so the zero test is the whole
    /// dispatch. With
    /// `zero_on_free` it declines (`false`), so the caller takes the cold
    /// slow path it already has for a full cache, and that path
    /// ([`Heap::free_slow_in`]) clears the slot and pushes it: the clear
    /// must not be a call from here, since a call inside the inlined fast
    /// path costs every default `free` a callee-saved register spill in
    /// its prologue (measured: larson1 +1.9% on aarch64 with the `memset`
    /// call inlined here, the `sdallocx` frame 0x20 -> 0x50 with `x19`
    /// saved). With `double_free_scan` alone it is the scanning push.
    ///
    /// # Safety
    /// As for [`Heap::push_cache`].
    #[inline(always)]
    pub(super) unsafe fn push_cache_flags(
        cache: *mut CacheHeader,
        p: *mut u8,
        flags: u8,
        Tracked(perms): Tracked<&mut CachePerms>,
        Tracked(tk): Tracked<&mut Map<int, GhostSingleton<int>>>,
    ) -> (r: bool)
        requires
            old(perms).wf(),
            old(perms).tracks(),
            old(perms).zero_ok(),
            old(perms).hdrs.contains_key(cache),
            (flags & PUSH_PAC != 0) == pac_mode(),
            (flags & PUSH_PAC != 0) ==> pac::pac_available(),
            regions_ok(),
            process_regions().is_slot(old(perms).cls[cache], p@.addr),
            slot_start(p),
            old(tk).contains_key(0) ==> slot_token(*old(tk), old(perms).cls[cache], true, p),
        ensures
            flags & PUSH_ZERO != 0 ==> !r,
            final(perms).keeps(*old(perms)),
            old(perms).claimed() ==> final(perms).claimed(),
            final(perms).follows(*old(perms)),
            !r ==> *final(tk) == *old(tk),
            old(perms).tokened() && old(tk).contains_key(0) ==> final(perms).tokened(),
            final(perms).zero_ok(),
        opens_invariants none
    {
        proof {
            assert((flags == 0 || flags == 4) ==> flags & 2 == 0) by (bit_vector);
            assert(flags == 4 ==> flags & 4 != 0) by (bit_vector);
            assert(flags == 0 ==> flags & 4 == 0) by (bit_vector);
        }
        // One byte compare each for the two common configurations: signed
        // pointers alone (opt-in `pac_pointers:true` on an arm64 host with
        // PAC; never set elsewhere, so the compare is compiled out) and no
        // hardening (the default on every host, PAC or not).
        if cfg!(target_arch = "aarch64") && flags == PUSH_PAC {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, true, Tracked(perms), Tracked(tk)) };
        }
        if flags == 0 {
            // SAFETY: guaranteed by the caller.
            return unsafe { Self::push_cache(cache, p, false, false, Tracked(perms), Tracked(tk)) };
        }
        if flags & PUSH_ZERO != 0 {
            proof {
                CachePerms::lemma_keeps_refl(*perms);
                CachePerms::lemma_tracks_same(*perms);
            }
            return false;
        }
        // SAFETY: guaranteed by the caller.
        unsafe { Self::push_cache(cache, p, flags & PUSH_SCAN != 0, flags & PUSH_PAC != 0, Tracked(perms), Tracked(tk)) }
    }

    /// `double_free_scan`'s compare (design 6.3 step 3): whether `e`, the
    /// entry as it would be stored (signed under `pac`), is one of the top
    /// four of the `len` entries in `slots`. Shared by the fast push, which
    /// declines on a hit, and by [`Heap::free_slow_in`], which aborts on
    /// one; no call in the body.
    ///
    /// # Safety
    /// `slots` has `len` entries (`perms` owns them, written).
    ///
    /// Trusted: the short-cache case is an iterator.
    #[inline(always)]
    #[verifier::external_body]
    pub(super) unsafe fn cached_recently(slots: *mut *mut u8, len: u32, e: *mut u8, Tracked(perms): Tracked<&Map<*mut *mut u8, PointsTo<*mut u8>>>) -> bool
        requires
            forall|i: int| 0 <= i < len ==> {
                let q = #[trigger] slot_ptr(slots, i);
                perms.contains_key(q) && perms[q].ptr() == q && perms[q].is_init()
            },
        opens_invariants none
    {
        if len == 0 {
            return false;
        }
        // SAFETY: guaranteed by the caller.
        unsafe {
            let top = slots.add(len as usize);
            if len >= 4 {
                (*top.sub(1) == e) | (*top.sub(2) == e) | (*top.sub(3) == e) | (*top.sub(4) == e)
            } else {
                (1..=len as usize).any(|i| *top.sub(i) == e)
            }
        }
    }

    /// Push onto `cache` unless it is full. With `scan`, compare `p`
    /// against the top four entries first (design 6.3 step 3): a match is
    /// a double free whose first copy is still
    /// cached, which the bit test cannot see, and the push declines
    /// (`false`) without storing the second copy; the caller's slow path
    /// ([`Heap::free_slow_in`]) repeats the compare and aborts by name. Not
    /// an abort from here: `cached_twice` would be the one call inside the
    /// inlined push, and a call in the body costs every `free` and
    /// `sdallocx` a frame (7 instructions per call on x86_64, the
    /// `stp`/`ldp` pair on aarch64). Four unconditional compares from the
    /// line the push writes to; a cache holding fewer than four takes the
    /// loop.
    ///
    /// With `pac` the entry is stored signed with the stack's base address
    /// as the modifier (`pacda`, [`PUSH_PAC`]); the scan compares signed
    /// against signed, which is the same test (one key, one modifier, so
    /// equal addresses give equal words).
    ///
    /// # Safety
    /// `cache` is a header owned by this heap, unused or with a stack.
    #[inline(always)]
    pub(super) unsafe fn push_cache(
        cache: *mut CacheHeader,
        p: *mut u8,
        scan: bool,
        pac: bool,
        Tracked(perms): Tracked<&mut CachePerms>,
        Tracked(tk): Tracked<&mut Map<int, GhostSingleton<int>>>,
    ) -> (r: bool)
        requires
            old(perms).wf(),
            old(perms).tracks(),
            old(perms).zero_ok(),
            old(perms).hdrs.contains_key(cache),
            pac == pac_mode(),
            pac ==> pac::pac_available(),
            regions_ok(),
            process_regions().is_slot(old(perms).cls[cache], p@.addr),
            slot_start(p),
            old(tk).contains_key(0) ==> slot_token(*old(tk), old(perms).cls[cache], true, p),
        ensures
            final(perms).keeps(*old(perms)),
            old(perms).claimed() ==> final(perms).claimed(),
            final(perms).follows(*old(perms)),
            !scan && old(perms).hdr(cache).len < old(perms).hdr(cache).cap ==> r,
            r ==> final(perms).hdr(cache).len == old(perms).hdr(cache).len + 1,
            old(perms).tokened() && old(tk).contains_key(0) ==> final(perms).tokened(),
            !r ==> *final(tk) == *old(tk),
            final(perms).zero_ok(),
        opens_invariants none
    {
        broadcast use vstd::layout::layout_of_references_and_pointers_for_sized_types;

        let ghost old_perms = *perms;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // SAFETY: guaranteed by the caller.
        unsafe {
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            let len = h.len;
            if len == h.cap {
                proof {
                    CachePerms::lemma_update(old_perms, *perms, cache);
                    CachePerms::lemma_tracks_hdr(old_perms, *perms, cache);
                    if old_perms.tokened() {
                        CachePerms::lemma_tokened_hdr(old_perms, *perms, cache);
                    }
                    if old_perms.claimed() {
                        CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
                    }
                    CachePerms::lemma_zero_hdr(old_perms, *perms, cache);
                }
                return false;
            }
            let slots = h.slots();
            let e = if pac { addr_to_ptr(pac::sign(p as usize, slots as usize)) } else { p };
            if scan && Self::cached_recently(slots, len, e, Tracked(&perms.slots)) {
                proof {
                    CachePerms::lemma_update(old_perms, *perms, cache);
                    CachePerms::lemma_tracks_hdr(old_perms, *perms, cache);
                    if old_perms.tokened() {
                        CachePerms::lemma_tokened_hdr(old_perms, *perms, cache);
                    }
                    if old_perms.claimed() {
                        CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
                    }
                    CachePerms::lemma_zero_hdr(old_perms, *perms, cache);
                }
                return false;
            }
            proof {
                assert(slot_ptr(slots, len as int) == crate::vspec::ptr::ptr_add(slots, len as usize));
                assert(perms.slots.contains_key(slot_ptr(slots, len as int)));
                assert(vstd::layout::size_of::<*mut u8>() == 8);
                assert(slot_in(slots, len as int));
                let sz = vstd::layout::size_of::<*mut u8>() as int;
                assert((len as usize) as int * sz <= isize::MAX) by (nonlinear_arith)
                    requires
                        sz == 8,
                        len <= u32::MAX;
            }
            let ghost q = slot_ptr(slots, len as int);
            let ghost s0 = perms.slots;
            ptr_mut_write(slots.add(len as usize), Tracked(perms.slots.tracked_borrow_mut(q)), e);
            proof {
                CachePerms::lemma_write_grows(s0, perms.slots, q);
            }
            h.len = len + 1;
            proof {
                lemma_stack_push(old_perms.hdr(cache), *h, s0, perms.slots);
                lemma_plain_stored(e, p, old_perms.cls[cache], slots);
                CachePerms::lemma_update(old_perms, *perms, cache);
                assert(old_perms.ent(cache, len as int) == q);
                assert(perms.slots == s0.insert(q, perms.slots[q]));
                CachePerms::lemma_tracks_put(old_perms, *perms, cache, len as int);
                assert forall|i: int| #[trigger] CachePerms::in_prefix(perms.hdr(cache), i) implies perms.zero_at(cache, i) by {
                    old_perms.lemma_zero_get(cache, i);
                    assert(old_perms.owner[old_perms.ent(cache, i)] == (cache, i));
                }
                CachePerms::lemma_zero_update(old_perms, *perms, cache);
            }
            proof {
                if old_perms.tokened() && tk.contains_key(0) {
                    let before = *perms;
                    let tracked t = tk.tracked_remove(0);
                    perms.toks.tracked_insert(q, t);
                    CachePerms::lemma_toks_only(before, *perms, old_perms);
                    CachePerms::lemma_tokened_push(old_perms, *perms, cache, t);
                }
                if old_perms.claimed() {
                    CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
                }
            }
        }
        true
    }

    /// The usable cache for `(arena, class)`, giving the header its
    /// capacity and stack on first use. Null if the metadata region is
    /// exhausted or caches are off (`tcache:false`, or
    /// `thread.tcache.enabled` false).
    pub(super) fn ensure_cache(
        &mut self,
        shared: &Shared,
        arena: u16,
        class: usize,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: *mut CacheHeader)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(ord).allows(rank_meta()),
            old(self).wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            r@.addr != 0 ==> final(self).owns(r) && final(self).hdr(r).cap != 0 && final(self).hdr_class(r) == class,
            r@.addr != 0 ==> final(self).routes(arena, class as int, r),
            // Caches off (`tcache:false`, or `thread.tcache.enabled` false):
            // no cache, and the heap is left as it was.
            !shared.config.tcache || !old(self).tcache_on() ==> r@.addr == 0 && *final(self) == *old(self),
    {
        proof {
            shared.lemma_wf();
            shared.lemma_meta();
            CachePerms::lemma_keeps_refl(self.perms@);
        }
        // The capacity first: a class that is never cached for this arena
        // must not create an `others` node. This is the one place the
        // placement rule is consulted on the allocation and free paths.
        let cap = shared.config.cap_for(self.arena, arena, class);
        if cap == 0 || !self.tcache_enabled {
            return ptr::null_mut();
        }
        // The FOREIGN cache is never popped, so its capacity only sets
        // the release batch: half the ladder, released whole when full
        // (`free_slow_in`), is the same batch as a home cache's half
        // flush for a residue that averages a quarter of the ladder
        // instead of three quarters (design 6.3 step 4).
        let cap = if arena == FOREIGN {
            (cap / 2).max(MIN_CAP)
        } else {
            cap
        };
        proof {
            assert(0 < cap <= 65536);
            assert(crate::meta::MAX_BINNED == 0x10_0000usize) by (compute_only);
        }
        // The arena before the node: `arena` came from an owner word on
        // the free path, and a word naming an arena that was never built
        // aborts here (`Shared::owner`) before a node is created for it.
        let refilled = if arena == FOREIGN {
            0
        } else {
            shared.owner(arena).class(class).refilled() as u32
        };
        let cache = self.cache_slot(shared, arena, class, Tracked(ord));
        if cache.is_null() {
            return ptr::null_mut();
        }
        if self.hdr_ref(cache).cap != 0 {
            return cache;
        }
        let (slots, Tracked(blk)) = shared.meta.alloc_raw(CacheHeader::slots_layout(cap), Tracked(ord));
        let tracked (raw, mc) = blk;
        if slots.is_null() {
            return ptr::null_mut();
        }
        let ghost h0 = *self;
        proof {
            axiom_fresh_stack(self.perms.borrow_mut(), cache, slots as *mut *mut u8, cap, raw);
            CachePerms::lemma_claimed_frame(h0.perms@, self.perms@);
            // The stack's claim joins the heap's, by the stack's base.
            shared.lemma_meta();
            assert(((slots as *mut *mut u8) as *mut u8)@ == slots@);
            self.perms.borrow_mut().tracked_put_claim(slots as *mut *mut u8, cap as int, mc);
        }
        let ghost pc = self.perms@;
        // `cache` is this heap's own header (still `EMPTY`) and `slots`
        // has room for `cap` pointers, exclusively ours.
        ptr_mut_write(
            cache,
            Tracked(self.perms.borrow_mut().hdrs.tracked_borrow_mut(cache)),
            CacheHeader {
                len: 0,
                cap,
                low_water: 0,
                zeroed: 0,
                slots: slots as *mut *mut u8,
                allocs: 0,
                compact_mask: u32::MAX,
                run_end: 0,
                moved: 0,
                probe_refilled: refilled,
                arena,
                mode: MODE_OFF,
                unsorted_flushes: 0,
            },
        );
        // The `FOREIGN` cache is never popped, so it never probes.
        let mode = if arena == FOREIGN {
            MODE_OFF
        } else {
            MODE_PROBE
        };
        let ghost h1 = *self;
        proof {
            let hp = self.perms@.hdrs[cache];
            assert(stack_ok(hp.value(), self.perms@.slots));
            CachePerms::lemma_update(h0.perms@, self.perms@, cache);
            CachePerms::lemma_tracks_frame(h0.perms@, self.perms@, cache);
            CachePerms::lemma_zero_update(h0.perms@, self.perms@, cache);
            // The fresh header caches nothing yet; every other entry keeps
            // its value and token.
            assert forall|p2: *mut CacheHeader, j: int|
                h0.perms@.hdrs.contains_key(p2) && p2 != cache && 0 <= j < h0.perms@.hdr(p2).cap implies {
                    let q = #[trigger] h0.perms@.ent(p2, j);
                    &&& self.perms@.slots[q] == h0.perms@.slots[q]
                    &&& (h0.perms@.toks.contains_key(q) ==> self.perms@.toks.contains_key(q) && self.perms@.toks[q] == h0.perms@.toks[q])
                } by {
                assert(cell_ok(p2, h0.perms@.hdrs[p2], h0.perms@.slots));
                assert(h0.perms@.slots.contains_key(h0.perms@.ent(p2, j)));
            }
            CachePerms::lemma_tok_update(h0.perms@, self.perms@, cache);
            CachePerms::lemma_claimed_new(pc, self.perms@, cache);
        }
        let h = self.hdr_mut(cache);
        h.set_mode(mode);
        proof {
            Heap::lemma_set(h1, *self, cache, *h);
            Heap::lemma_keeps_trans(*old(self), h0, h1);
            Heap::lemma_keeps_trans(*old(self), h1, *self);
            Heap::lemma_keeps_trans(h0, h1, *self);
            Heap::lemma_keeps_routes(h0, *self, arena, class as int, cache);
        }
        cache
    }
}

} // verus!
