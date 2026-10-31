//! Compaction (design 6.4). A LIFO cache hands every allocation the slot
//! that was just freed, so under a balanced alloc/free churn neither a
//! refill nor a flush runs and the slab layout stays frozen. A compacting
//! cache serves one pop in `2^COMPACT_SHIFT` (a *scheduled pop*) from a
//! run of low addresses kept at the bottom of its stack,
//! `slots[..run_end]`, descending, instead of the top: the run's lowest
//! slot goes out and the top entry takes its place. A refill's batch is
//! such a run; an exhausted run is rebuilt by claiming `cap / 4` slots
//! from the class's lowest partial slab. The displaced frees sit above the
//! run until a flush returns them, highest addresses first, so the live
//! set migrates downward and the high slabs empty and are purged.
//!
//! The schedule is gated: a cache probes its class every `PROBE_MASK + 1`
//! pops and compacts only while the class has holes worth
//! [`COMPACT_MIN_SLABS`] slabs and a [`COMPACT_MIN_RATIO`]th of its
//! capacity that ordinary refills are not consuming
//! ([`COMPACT_MAX_REFILL_SLABS`]). The run stays intact because `run_end
//! <= low_water <= len`: a pop that dips below it clips both, and a push
//! never writes below `len`.

use core::ptr;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};

use crate::layout::CLASSES;
#[cfg(verus_keep_ghost)]
use crate::layout::NCLASSES;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use crate::sys::pac;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::errno::ThreadErrno;

use super::paths::shared_of;
use super::{CacheHeader, CachePerms, Heap, Live, Shared, stack_mut};
#[cfg(verus_keep_ghost)]
use super::{
    cell_ok, hand_out, lemma_handed_ok, lemma_rebuild_moves, lemma_rebuild_tracks,
    lemma_stack_fill, lemma_stack_grow, lemma_stack_shrink, pac_mode, plain, regions_ok, slot_in,
    slot_ptr, slots_grow, stored, zero,
};
#[cfg(verus_keep_ghost)]
use crate::class::{ptr_addrs, slot_loc};
#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions, slots_in};
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
#[cfg(verus_keep_ghost)]
use crate::vspec::zero::Zeroed;
#[cfg(verus_keep_ghost)]
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::modes::tracked_swap;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::PointsTo;
use vstd::raw_ptr::{ptr_mut_ref, ptr_mut_write, ptr_ref};
#[cfg(verus_keep_ghost)]
use vstd::resource::set::GhostSingleton;

verus! {

/// Cache modes: off (the `FOREIGN` cache), probing the class for
/// fragmentation every [`PROBE_MASK`] + 1 pops, compacting.
pub const MODE_OFF: u16 = 0;
pub const MODE_PROBE: u16 = 1;
pub const MODE_COMPACT: u16 = 2;

/// Pops between two fragmentation probes of a class (minus one).
pub exec const PROBE_MASK: u32
    ensures
        PROBE_MASK == 0xfff,
{
    proof {
        assert((1u32 << 12u32) == 0x1000) by (bit_vector);
    }
    (1 << 12) - 1
}
/// A compacting cache serves every `2^COMPACT_SHIFT`th pop of a class
/// from the lowest-address cached slot instead of the top of the stack
/// (design 6.4).
pub const COMPACT_SHIFT: u32 = 3;
/// The scheduled-pop mask of a compacting cache: a pop whose count has
/// no bit of it set is served from the bottom of the run.
pub exec const COMPACT_MASK: u32
    ensures
        COMPACT_MASK == 7,
{
    proof {
        assert((1u32 << 3u32) == 8) by (bit_vector);
    }
    (1 << COMPACT_SHIFT) - 1
}

/// A class is worth compacting when its slabs hold at least this many
/// slabs' worth of holes (free slots not in any cache) and at least a
/// [`COMPACT_MIN_RATIO`]th of their capacity. Two or three slabs of holes
/// are what a producer/consumer pair keeps in flight between the
/// consumer's flushes and the producer's refills (xmalloc-test), which is
/// not fragmentation.
pub const COMPACT_MIN_SLABS: usize = 4;
/// See [`COMPACT_MIN_SLABS`]: holes must be at least capacity / 32.
pub const COMPACT_MIN_RATIO: usize = 32;
/// A class whose cache refills claimed this many slabs' worth of slots
/// between two probes of one cache (4096 pops apart) is being drained by
/// ordinary refills, which take the lowest partial slab anyway: its holes
/// are transient (xmalloc-test's producers run 0..8 slabs of holes through
/// a 150..450-slab class at thousands of refilled slots per window) and it
/// is not compacted. Only refills count (`ClassState::refilled`), not
/// compaction's own rebuild claims and not direct `MALLOCX_TCACHE_NONE`
/// claims: Valkey's active defragmenter allocates that way at hundreds of
/// slots per second, which at one slab per window would keep the frozen
/// residue of the Valkey soak from ever being compacted. The residue
/// itself sees a refill only when a class's random-size churn walks a
/// cache to empty, a few per second.
pub const COMPACT_MAX_REFILL_SLABS: usize = 2;
// Both slab counts are in 64 KiB slabs (`Class::compact_slots`), so the
// thresholds mean the same bytes for the 4 KiB-slab classes as for the rest.

/// What a compaction probe decided ([`Heap::probe_verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Ordinary refills claimed [`COMPACT_MAX_REFILL_SLABS`] slabs' worth
    /// of slots since the previous probe: the class is being drained by
    /// them and is left alone.
    Refilling,
    /// Not fragmented ([`Heap::fragmented`]): keep probing.
    Dense,
    /// Fragmented and left alone by refills: enter compact mode.
    Fragmented,
}

impl CacheHeader {
    /// Whether the cache is currently compacting its class.
    #[cfg(test)]
    pub fn compacting(&self) -> bool {
        self.mode == MODE_COMPACT
    }

    /// This header with `mode` and `compact_mask` replaced: what
    /// [`CacheHeader::set_mode`] leaves.
    pub(super) open spec fn with_mode(self, mode: u16, compact_mask: u32) -> CacheHeader {
        CacheHeader { mode, compact_mask, ..self }
    }

    /// The scheduled-pop mask `mode` implies: [`PROBE_MASK`] while
    /// probing, [`COMPACT_MASK`] while compacting, `u32::MAX` when off (one pop in 2^32 then reaches
    /// `pop_compact`, which pops the top). The one home of the
    /// mode-to-mask rule; [`CacheHeader::set_mode`] applies it.
    pub(super) fn mask_for(mode: u16) -> (r: u32)
        ensures
            mode == MODE_PROBE ==> r == 0xfff,
            mode == MODE_COMPACT ==> r == 7,
            mode != MODE_PROBE && mode != MODE_COMPACT ==> r == u32::MAX,
    {
        match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => COMPACT_MASK,
            _ => u32::MAX,
        }
    }

    /// Enter `mode` and derive the scheduled-pop mask from it
    /// ([`CacheHeader::mask_for`]), so the two fields never disagree.
    pub(super) fn set_mode(&mut self, mode: u16)
        ensures
            *final(self) == old(self).with_mode(mode, final(self).compact_mask),
            mode == MODE_PROBE ==> final(self).compact_mask == 0xfff,
            mode == MODE_COMPACT ==> final(self).compact_mask == 7,
            mode != MODE_PROBE && mode != MODE_COMPACT ==> final(self).compact_mask == u32::MAX,
    {
        self.mode = mode;
        self.compact_mask = Self::mask_for(mode);
    }
}

impl Heap {
    /// The scheduled pop (design 6.4). Probing: run the periodic work, ask
    /// the class whether it is fragmented and being left alone by ordinary
    /// refills and, if not, pop the top as usual. Compacting: serve
    /// the lowest entry of the run, `slots[run_end - 1]`, moving the top
    /// entry into its place, so this allocation lands in the lowest slab
    /// with room instead of reusing the slot that was just freed; an
    /// exhausted run is rebuilt first ([`Heap::rebuild_run`]), which also
    /// re-checks the class and drops back to probing when it is compact.
    ///
    /// # Safety
    /// As for [`Heap::pop`]; `allocs` has already been counted. The
    /// probing branch calls `tick`, after which `len` is stale (the GC may
    /// have flushed the cache), so it reloads `len` and refills an emptied
    /// cache before popping.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn pop_compact(
        &mut self,
        cache: *mut CacheHeader,
        class: usize,
        len: u32,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    ) -> (r: (*mut u8, bool))
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
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        let tracked mut spare = Map::<int, GhostSingleton<int>>::tracked_empty();
        // SAFETY: guaranteed by the caller.
        unsafe {
            let pac = shared_of(self.shared).pac();
            match self.hdr_ref(cache).mode {
                MODE_COMPACT => {}
                MODE_PROBE => {
                    let shared: &Shared = shared_of(self.shared);
                    // The probe is also a heartbeat for the periodic work: a
                    // balanced churn on a dense class takes no other slow
                    // path, and without it no GC or decay would ever run on
                    // a thread without a background thread.
                    // Premise (lock order): as in `Heap::rebuild_run`, the scheduled
                    // pops' callers hold no allocator lock at the call.
                    let tracked mut ord = axiom_entry();
                    self.tick::<true>(shared, Tracked(&mut ord), Tracked(errno));
                    proof {
                        Heap::lemma_keeps_poppable(*old(self), *self, class as int, cache);
                    }
                    let mut len = self.hdr_ref(cache).len;
                    if len == 0 {
                        // The GC emptied the cache.
                        let Some((a, w)) = shared.arena(self.hdr_ref(cache).arena, Tracked(&mut ord)) else {
                            return (ptr::null_mut(), false);
                        };
                        let ghost s = *self;
                        len = self.refill(shared, a, w, cache, class, Tracked(&mut ord));
                        proof {
                            Heap::lemma_keeps_poppable(s, *self, class as int, cache);
                        }
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    let (refilled, verdict) =
                        Self::probe_verdict(shared, self.hdr_ref(cache).arena, class, self.hdr_ref(cache).probe_refilled, Tracked(&mut ord));
                    let ghost h0 = *self;
                    let h = self.hdr_mut(cache);
                    h.probe_refilled = refilled;
                    proof {
                        Heap::lemma_set(h0, *self, cache, *h);
                        Heap::lemma_poppable_same(h0, *self, class as int, cache);
                    }
                    match verdict {
                        Verdict::Refilling | Verdict::Dense => {
                            let r = Self::pop_top(cache, len, pac, Tracked(self.perms.borrow_mut()), Tracked(&mut spare));
                            proof {
                                hand_out(&mut spare, blk, class as int, r.0);
                            }
                            return r;
                        }
                        Verdict::Fragmented => {}
                    }
                    let ghost h1 = *self;
                    let h = self.hdr_mut(cache);
                    h.set_mode(MODE_COMPACT);
                    proof {
                        Heap::lemma_set(h1, *self, cache, *h);
                        Heap::lemma_poppable_same(h1, *self, class as int, cache);
                    }
                    return self.pop_compact_run(cache, class, len, Tracked(blk), Tracked(errno));
                }
                _ => {
                    let r = Self::pop_top(cache, len, pac, Tracked(self.perms.borrow_mut()), Tracked(&mut spare));
                    proof {
                        hand_out(&mut spare, blk, class as int, r.0);
                    }
                    return r;
                },
            }
            self.pop_compact_run(cache, class, len, Tracked(blk), Tracked(errno))
        }
    }

    /// The compacting half of [`Heap::pop_compact`]. A cache that is not
    /// (or, after its rebuild, no longer) in compact mode pops the top.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    #[verifier::spinoff_prover]
    #[verifier::rlimit(30)]
    pub(super) unsafe fn pop_compact_run(
        &mut self,
        cache: *mut CacheHeader,
        class: usize,
        len: u32,
        Tracked(blk): Tracked<&mut Live>,
        Tracked(errno): Tracked<&mut ThreadErrno>,
    ) -> (r: (*mut u8, bool))
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
        ensures
            final(self).keeps(*old(self)),
            r.0@.addr != 0 ==> process_regions().is_slot(class as int, r.0@.addr),
            r.0@.addr != 0 && r.1 ==> final(self).handed_ok(r.0, class as int),
            r.0@.addr != 0 ==> final(blk).slot_block(r.0, class as int),
    {
        broadcast use lemma_handed_ok;
        broadcast use vstd::layout::layout_of_references_and_pointers_for_sized_types;
        let tracked mut spare = Map::<int, GhostSingleton<int>>::tracked_empty();
        // SAFETY: guaranteed by the caller.
        unsafe {
            let pac = shared_of(self.shared).pac();
            match self.hdr_ref(cache).mode {
                MODE_COMPACT => {}
                _ => {
                    let r = Self::pop_top(cache, len, pac, Tracked(self.perms.borrow_mut()), Tracked(&mut spare));
                    proof {
                        hand_out(&mut spare, blk, class as int, r.0);
                    }
                    return r;
                },
            }
            let mut len = len;
            if self.hdr_ref(cache).run_end == 0 {
                self.rebuild_run(cache, class, Tracked(errno));
                len = self.hdr_ref(cache).len;
                if self.hdr_ref(cache).run_end == 0 {
                    if len == 0 {
                        // The rebuild's tick ran the cache GC, which
                        // flushes every entry of a cache with `low_water
                        // == len < 4`, and the rebuild then claimed
                        // nothing (the class is no longer fragmented, or
                        // the claim came back empty). Refill as the probe
                        // branch does; popping here would wrap `len - 1`.
                        let shared: &Shared = shared_of(self.shared);
                        // Premise (lock order): as in `pop_compact`.
                        let tracked mut ord = axiom_entry();
                        let Some((a, w)) = shared.arena(self.hdr_ref(cache).arena, Tracked(&mut ord)) else {
                            return (ptr::null_mut(), false);
                        };
                        len = self.refill(shared, a, w, cache, class, Tracked(&mut ord));
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    let r = Self::pop_top(cache, len, pac, Tracked(self.perms.borrow_mut()), Tracked(&mut spare));
                    proof {
                        hand_out(&mut spare, blk, class as int, r.0);
                    }
                    return r;
                }
            }
            let ghost old_perms = self.perms@;
            proof {
                assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
            }
            let tracked perms = self.perms.borrow_mut();
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            // The run invariant `run_end <= low_water <= len` is what keeps
            // this pop from serving a stale pointer out of `slots[run_end -
            // 1]`, and `zeroed <= low_water` what keeps the zero prefix
            // inside the stack; a violation is a heap inconsistency and
            // aborts in release.
            assert!(
                h.run_end <= h.low_water
                    && h.low_water <= len
                    && h.zeroed <= h.low_water,
                "heap: cache run invariant violated (run_end or zeroed > low_water, or low_water > len)"
            );
            let top = len - 1;
            let at = h.run_end - 1;
            let slots = h.slots();
            proof {
                assert(slot_ptr(slots, at as int) == crate::vspec::ptr::ptr_add(slots, at as usize));
                assert(slot_ptr(slots, top as int) == crate::vspec::ptr::ptr_add(slots, top as usize));
                assert(slot_in(slots, at as int) && slot_in(slots, top as int));
                assert(vstd::layout::size_of::<*mut u8>() == 8);
                let sz = vstd::layout::size_of::<*mut u8>() as int;
                assert((at as usize) as int * sz <= isize::MAX) by (nonlinear_arith)
                    requires
                        sz == 8,
                        at <= u32::MAX;
                assert((top as usize) as int * sz <= isize::MAX) by (nonlinear_arith)
                    requires
                        sz == 8,
                        top <= u32::MAX;
                assert(crate::vspec::ptr::in_alloc(slots, (at as usize) as int * vstd::layout::size_of::<*mut u8>()));
            }
            let p = *ptr_ref(slots.add(at as usize), Tracked(perms.slots.tracked_borrow(slot_ptr(slots, at as int))));
            // Entries move within the stack signed (the modifier is the
            // stack's base, not the entry's position); only the one leaving
            // is authenticated.
            let p = if pac {
                addr_to_ptr(pac::auth(p as usize, slots as usize))
            } else {
                p
            };
            let ghost s0 = perms.slots;
            let ghost q = slot_ptr(slots, at as int);
            let t = *ptr_ref(slots.add(top as usize), Tracked(perms.slots.tracked_borrow(slot_ptr(slots, top as int))));
            ptr_mut_write(slots.add(at as usize), Tracked(perms.slots.tracked_borrow_mut(q)), t);
            proof {
                CachePerms::lemma_write_grows(s0, perms.slots, q);
                lemma_stack_grow(old_perms.hdr(cache), s0, perms.slots);
            }
            h.len = top;
            h.run_end = at;
            if top < h.low_water {
                h.low_water = top;
            }
            // `at` now holds the old top entry: the prefix keeps `at` only
            // when that entry was in it too, and never reaches past `top`.
            let zeroed = h.zeroed;
            let zero = at < zeroed;
            if zero {
                h.zeroed = if top < zeroed { top } else { at };
            }
            proof {
                lemma_stack_shrink(old_perms.hdr(cache), *h, perms.slots);
                CachePerms::lemma_update(old_perms, *perms, cache);
                assert(old_perms.ent(cache, at as int) == q);
                assert(old_perms.ent(cache, top as int) == slot_ptr(slots, top as int));
                assert(perms.slots == s0.insert(q, perms.slots[q]));
                CachePerms::lemma_tracks_put(old_perms, *perms, cache, at as int);
                // The served entry's zero token is handed out; the top
                // entry's moves with it to `at` when both are in the prefix.
                let ghost mid = *perms;
                let ghost z0 = old_perms.hdr(cache).zeroed;
                let qt = slot_ptr(slots, top as int);
                assert(old_perms.owner[q] == (cache, at as int));
                assert(old_perms.owner[qt] == (cache, top as int));
                if zero {
                    old_perms.lemma_zero_get(cache, at as int);
                    let tracked t = perms.zero.tracked_remove(q);
                    perms.handed = Some(t);
                    if top < z0 && top != at {
                        old_perms.lemma_zero_get(cache, top as int);
                        let tracked u = perms.zero.tracked_remove(qt);
                        perms.zero.tracked_insert(q, u);
                    }
                }
                assert forall|i: int| #[trigger] CachePerms::in_prefix(perms.hdr(cache), i) implies perms.zero_at(cache, i) by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    if i != at {
                        old_perms.lemma_zero_get(cache, i);
                        assert(old_perms.owner[old_perms.ent(cache, i)] == (cache, i));
                    }
                }
                CachePerms::lemma_zero_update(old_perms, *perms, cache);
                // The served slot's slot token leaves with it, as the
                // block's token; the top entry's moves with its slot to
                // `at`.
                let ghost before_t = *perms;
                let tracked served = CachePerms::lemma_tokened_compact(&mut perms.toks, old_perms, before_t, cache, at as int, top as int);
                assert(*perms == (CachePerms { toks: perms.toks, ..before_t }));
                CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
                CachePerms::lemma_zero_same(before_t, *perms);
                CachePerms::lemma_same_but_tokens(mid, *perms, old_perms);
                assert(served@ == p@.addr as int);
                blk.set(Live::from_slot(served, class as int, p));
            }
            (p, zero)
        }
    }

    /// Rebuild the run for a scheduled pop that found it exhausted: run the
    /// periodic work (under a balanced churn this is the only slow path a
    /// class ever takes), drop back to probing if the class is no longer
    /// fragmented, otherwise flush the cache down to `cap / 2` if it has no
    /// room for the block, claim a block of `cap / 4` slots from the class's
    /// lowest partial slab and swap it into the bottom of the stack,
    /// lowest address on top of the block. No sort: the block is claimed in
    /// address order and the entries it displaces are LIFO entries whose
    /// order does not matter.
    ///
    /// The block is claimed into a slice over the stack and moved with
    /// slice operations ([`stack_mut`](super::stack_mut)). The stores keep
    /// [`CacheHeader::inv`]: the block of `n <= cap - len` entries makes
    /// `len + n <= cap`, `run_end = n`, `low_water = max(low_water, n)` and
    /// `zeroed <= n`.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `class`, in compact mode.
    #[cold]
    #[inline(never)]
    #[verifier::rlimit(50)]
    // Own prover: in the module's shared Z3 context the proof goes past
    // its rlimit under quantum8.
    #[verifier::spinoff_prover]
    pub(super) unsafe fn rebuild_run(
        &mut self,
        cache: *mut CacheHeader,
        class: usize,
        Tracked(errno): Tracked<&mut crate::vspec::errno::ThreadErrno>,
    )
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
        ensures
            final(self).keeps(*old(self)),
    {
        // SAFETY: `self.shared` is the process's `Shared` and it is ready
        // (`Heap::wf`), as `shared_of` requires; the unbounded lifetime lets
        // `self` be borrowed mutably below.
        let shared: &Shared = unsafe { shared_of(self.shared) };
        // Premise (lock order): the scheduled pops run from `Heap::pop`, whose
        // callers (`alloc_in`, `alloc_other`, `alloc_slow_in` after `refill`,
        // and `alloc_slow`'s declined scheduled pop) hold no allocator lock at
        // the call, so this rebuild holds none either.
        let tracked mut ord = axiom_entry();
        self.tick::<true>(shared, Tracked(&mut ord), Tracked(errno));
        let ghost h0 = *self;
        // SAFETY: guaranteed by the caller; `len <= cap` entries exist and
        // `cap - len` slots of storage follow them.
        unsafe {
            let arena = self.hdr_ref(cache).arena;
            if !Self::fragmented(shared, arena, class, Tracked(&mut ord)) {
                shared.owner(arena).0.class(class).note_compact_mode(false);
                let h = self.hdr_mut(cache);
                h.set_mode(MODE_PROBE);
                proof {
                    Heap::lemma_set(h0, *self, cache, *h);
                    Heap::lemma_keeps_trans(*old(self), h0, *self);
                }
                return;
            }
            let cap = self.hdr_ref(cache).cap as usize;
            let half = (cap / 2).max(1);
            let block = (cap / 4).max(1);
            let len = self.hdr_ref(cache).len as usize;
            if len + block > cap && len > half {
                self.flush(shared, arena, cache, class, len - half, Tracked(&mut ord));
            }
            let ghost h1 = *self;
            proof {
                Heap::lemma_keeps_trans(*old(self), h0, h1);
            }
            let len = self.hdr_ref(cache).len as usize;
            let want = block.min(cap - len);
            if want == 0 {
                return;
            }
            let ghost p1 = self.perms@;
            proof {
                assert(cell_ok(cache, p1.hdrs[cache], p1.slots));
            }
            let slots = self.hdr_ref(cache).slots;
            // The stack's permissions move out of the heap while the slices
            // borrow them, so that `claim` can take `self`.
            let tracked mut sl = Map::tracked_empty();
            proof {
                tracked_swap(&mut self.perms.borrow_mut().slots, &mut sl);
            }
            let out = stack_mut(slots, len, want, Tracked(&mut sl));
            let tracked mut toks = crate::class::SlotToks::empty();
            let (oa, ow) = shared.owner(arena);
            let (claim, Tracked(mut ztok)) = self.claim(shared, oa.class(class), out, Tracked(&mut toks), ow, Tracked(&mut ord));
            let ghost claimed = out@;
            let n = claim.n;
            if n == 0 {
                proof {
                    tracked_swap(&mut self.perms.borrow_mut().slots, &mut sl);
                    CachePerms::lemma_rebuild_none(p1, self.perms@, cache, len as int, want as int);
                    CachePerms::lemma_claimed_frame(p1, self.perms@);
                    Heap::lemma_keeps_trans(*old(self), h1, *self);
                }
                return;
            }
            shared.owner(arena).0.class(class).note_compact(n);
            out[..n].reverse();
            let ghost reversed = out@;
            if shared.pac() {
                CacheHeader::sign_all(slots, &mut out[..n]);
            }
            let ghost s2 = sl;
            let all = stack_mut(slots, 0, len + n, Tracked(&mut sl));
            let ghost all0 = all@;
            if len >= n {
                // [old..][block] -> [block][old[n..]][old[..n]]
                let (bottom, rest) = all.split_at_mut(n);
                bottom.swap_with_slice(&mut rest[len - n..]);
                proof {
                    assert(forall|i: int| 0 <= i < len + n ==> #[trigger] all@[i] == if i < n { all0[len + i] } else if i < len { all0[i] } else { all0[i - len] });
                }
            } else {
                // The block is bigger than what was there: rotate it down.
                all.rotate_left(len);
                proof {
                    assert(forall|i: int| 0 <= i < len + n ==> #[trigger] all@[i] == if i < n { all0[len + i] } else { all0[i - n] });
                }
            }
            proof {
                tracked_swap(&mut self.perms.borrow_mut().slots, &mut sl);
            }
            let tracked perms = self.perms.borrow_mut();
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            let new_len = (len + n) as u32;
            h.len = new_len;
            h.moved = h.moved.wrapping_add(n as u64);
            h.run_end = n as u32;
            h.low_water = h.low_water.max(n as u32);
            h.zeroed = if claim.zeroed { n as u32 } else { 0 };
            proof {
                let nn = n as int;
                assert forall|k: int| 0 <= k < nn implies (#[trigger] out@[k])@.addr == stored(claimed[nn - 1 - k]@.addr, slots) by {
                    assert(reversed[k] == claimed.subrange(0, nn).reverse()[k]);
                    assert(claimed.subrange(0, nn).reverse()[k] == claimed[nn - 1 - k]);
                    if !pac_mode() {
                        assert(out@ == reversed);
                    }
                }
                assert(perms.hdrs =~= p1.hdrs.insert(cache, perms.hdrs[cache]));
                assert(crate::class::claim_zero_ok(claim, claimed, ztok, class as int));
                assert(class_table()[p1.cls[cache]].size == class_table()[class as int].size);
                if claim.zeroed {
                    // One quantifier per conjunct, each instantiating
                    // `claim_zero_ok` through its trigger term, so neither depends
                    // on which conjunct Z3 splits on.
                    assert forall|k: int| 0 <= k < nn implies #[trigger] ztok.contains_key(k) by {
                        assert(0 <= k < claim.n);
                        assert(ztok.contains_key(k));
                    }
                    assert forall|k: int| 0 <= k < nn implies #[trigger] ztok[k].covers(claimed[k], class_table()[p1.cls[cache]].size as int) by {
                        assert(0 <= k < claim.n);
                        assert(ztok.contains_key(k));
                        assert(ztok[k].covers(claimed[k], class_table()[class as int].size as int));
                    }
                }
                assert(p1.claimed());
                CachePerms::lemma_rebuild_some(perms, ztok, &mut toks, p1, s2, cache, len as int, want as int, nn, claim.zeroed, claimed, out@, all0, all@);
                Heap::lemma_keeps_trans(*old(self), h1, *self);
            }
        }
    }

    /// Whether `class` in `arena` is worth compacting (design 6.4).
    pub(super) fn fragmented(shared: &Shared, arena: u16, class: usize, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        let cs = shared.owner(arena).0.class(class);
        let (holes, capacity) = cs.holes(Tracked(ord));
        holes >= COMPACT_MIN_SLABS * CLASSES.table()[class].compact_slots()
            && holes * COMPACT_MIN_RATIO >= capacity
    }

    /// The compaction probe's gate (design 6.4) for the scheduled pops
    /// ([`Heap::pop_compact`]): the class's refill counter now (the caller stores it as the
    /// header's `probe_refilled`) and the verdict, given the counter at
    /// the previous probe. Records the class's `note_gate_refill` and
    /// `note_compact_mode(true)` statistics as it decides.
    pub(super) fn probe_verdict(
        shared: &Shared,
        arena: u16,
        class: usize,
        prev_refilled: u32,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (u32, Verdict)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        let cs = shared.owner(arena).0.class(class);
        let refilled = cs.refilled() as u32;
        let delta = refilled.wrapping_sub(prev_refilled) as usize;
        let verdict = if delta >= COMPACT_MAX_REFILL_SLABS * CLASSES.table()[class].compact_slots() {
            if Self::fragmented(shared, arena, class, Tracked(ord)) {
                cs.note_gate_refill();
            }
            Verdict::Refilling
        } else if !Self::fragmented(shared, arena, class, Tracked(ord)) {
            Verdict::Dense
        } else {
            cs.note_compact_mode(true);
            Verdict::Fragmented
        };
        (refilled, verdict)
    }
}

impl CachePerms {
    /// [`Heap::rebuild_run`] with an empty claim: the claim wrote entries
    /// `len..len + want` of `p`'s stack through a slice ([`stack_mut`]'s
    /// frame, from `o` to `new`), above `len`, and nothing else changed,
    /// so every invariant still holds.
    proof fn lemma_rebuild_none(o: CachePerms, new: CachePerms, p: *mut CacheHeader, len: int, want: int)
        requires
            o.wf(),
            o.tracks(),
            o.zero_ok(),
            o.tokened(),
            o.hdrs.contains_key(p),
            len == o.hdr(p).len,
            len + want <= o.hdr(p).cap,
            new == (CachePerms { slots: new.slots, ..o }),
            slots_grow(o.slots, new.slots),
            forall|i: int| len <= i < len + want ==> #[trigger] new.slots[slot_ptr(o.hdr(p).slots, i)].is_init(),
            forall|q: *mut *mut u8|
                o.slots.contains_key(q) && (forall|i: int| len <= i < len + want ==> q != #[trigger] slot_ptr(o.hdr(p).slots, i))
                    ==> #[trigger] new.slots[q] == o.slots[q],
        ensures
            new.keeps(o),
            new.follows(o),
            new.zero_ok(),
            new.tokened(),
    {
        let hd = o.hdr(p);
        assert(cell_ok(p, o.hdrs[p], o.slots));
        lemma_stack_fill(hd, hd, o.slots, new.slots, 0, len);
        assert(new.hdrs =~= o.hdrs.insert(p, new.hdrs[p]));
        CachePerms::lemma_update(o, new, p);
        CachePerms::lemma_slice_frame(o, o.slots, new.slots, p, len, want);
        assert forall|i: int| 0 <= i < new.hdr(p).len implies #[trigger] new.holds(new.cls[p], new.ent(p, i), new.hdr(p).slots) by {
            assert(new.ent(p, i) == o.ent(p, i));
            assert(o.owner[o.ent(p, i)] == (p, i));
        }
        CachePerms::lemma_tracks_stack(o, new, p);
        assert forall|i: int| #[trigger] CachePerms::in_prefix(new.hdr(p), i) implies new.zero_at(p, i) by {
            o.lemma_zero_get(p, i);
            assert(new.ent(p, i) == o.ent(p, i));
        }
        CachePerms::lemma_zero_update_ent(o, new, p);
        assert forall|p2: *mut CacheHeader, j: int|
            o.hdrs.contains_key(p2) && p2 != p && 0 <= j < o.hdr(p2).cap implies {
                let q = #[trigger] o.ent(p2, j);
                &&& new.slots[q] == o.slots[q]
                &&& (o.toks.contains_key(q) ==> new.toks.contains_key(q) && new.toks[q] == o.toks[q])
            } by {
        }
        assert forall|i: int| 0 <= i < new.hdr(p).len implies #[trigger] new.tok_at(p, i) by {
            o.lemma_tok_get(p, i);
            assert(new.ent(p, i) == o.ent(p, i));
        }
        CachePerms::lemma_tok_update(o, new, p);
    }

    /// [`Heap::rebuild_run`] with a claim of `n > 0` slots: the claim
    /// (`claimed`, its tokens `ctoks` and its zero tokens `z`) went into
    /// entries `len..len + want` of `p`'s stack through a first slice
    /// (`o.slots` to `mid`, the block reversed and signed into `outf`),
    /// the moves of a second slice over entries `0..len + n` (`all0` to
    /// `allf`, `mid` to `new.slots`) put it at the bottom, and the header
    /// took `len + n` entries, a run of `n` and a zero prefix of the block
    /// at most. The claim's zero tokens become the prefix's, and the old
    /// entries' slot tokens and the claim's go back under the entries
    /// holding their slots.
    #[verifier::spinoff_prover]
    proof fn lemma_rebuild_some(
        tracked perms: &mut CachePerms,
        tracked z: Map<int, Zeroed>,
        tracked ctoks: &mut crate::class::SlotToks,
        o: CachePerms,
        mid: Map<*mut *mut u8, PointsTo<*mut u8>>,
        p: *mut CacheHeader,
        len: int,
        want: int,
        n: int,
        zeroed: bool,
        claimed: Seq<*mut u8>,
        outf: Seq<*mut u8>,
        all0: Seq<*mut u8>,
        allf: Seq<*mut u8>,
    )
        requires
            o.wf(),
            o.tracks(),
            o.zero_ok(),
            o.tokened(),
            o.hdrs.contains_key(p),
            regions_ok(),
            len == o.hdr(p).len,
            0 < n <= want,
            len + want <= o.hdr(p).cap,
            old(perms).hdrs == o.hdrs.insert(p, old(perms).hdrs[p]),
            old(perms).cls == o.cls,
            old(perms).owner == o.owner,
            old(perms).toks == o.toks,
            old(perms).zero == o.zero,
            old(perms).blocks == o.blocks,
            old(perms).hdr(p).slots == o.hdr(p).slots,
            old(perms).hdr(p).cap == o.hdr(p).cap,
            old(perms).hdr(p).len == len + n,
            old(perms).hdrs[p].ptr() == p,
            old(perms).hdrs[p].is_init(),
            old(perms).hdr(p).inv(),
            old(perms).hdr(p).zeroed <= (if zeroed { n } else { 0 }),
            claimed.len() == want,
            slots_in(claimed, o.cls[p], 0, n),
            crate::class::keyed(old(ctoks).toks, ptr_addrs(claimed.subrange(0, n)), slot_loc(o.cls[p]), 0, n),
            zeroed ==> forall|k: int| #![trigger z.contains_key(k)] #![trigger z[k]] 0 <= k < n ==> z.contains_key(k) && z[k].covers(claimed[k], class_table()[o.cls[p]].size as int),
            outf.len() == want,
            forall|k: int| 0 <= k < n ==> (#[trigger] outf[k])@.addr == stored(claimed[n - 1 - k]@.addr, o.hdr(p).slots),
            // The first slice (`len..len + want`, from `o.slots` to `mid`).
            slots_grow(o.slots, mid),
            forall|q: *mut *mut u8|
                o.slots.contains_key(q) && (forall|i: int| len <= i < len + want ==> q != #[trigger] slot_ptr(o.hdr(p).slots, i))
                    ==> #[trigger] mid[q] == o.slots[q],
            forall|i: int| len <= i < len + want ==> #[trigger] mid[slot_ptr(o.hdr(p).slots, i)].is_init(),
            forall|i: int| len <= i < len + want ==> #[trigger] mid[slot_ptr(o.hdr(p).slots, i)].value() == outf[i - len],
            // The second (`0..len + n`, from `mid` to `old(perms).slots`).
            all0.len() == len + n,
            allf.len() == len + n,
            slots_grow(mid, old(perms).slots),
            forall|i: int| 0 <= i < len + n ==> #[trigger] old(perms).slots[slot_ptr(o.hdr(p).slots, i)].is_init(),
            forall|i: int|
                0 <= i < len + n && mid[slot_ptr(o.hdr(p).slots, i)].is_init()
                    ==> all0[i] == #[trigger] mid[slot_ptr(o.hdr(p).slots, i)].value(),
            forall|i: int| 0 <= i < len + n ==> #[trigger] old(perms).slots[slot_ptr(o.hdr(p).slots, i)].value() == allf[i],
            forall|q: *mut *mut u8|
                mid.contains_key(q) && (forall|i: int| 0 <= i < len + n ==> q != #[trigger] slot_ptr(o.hdr(p).slots, i))
                    ==> #[trigger] old(perms).slots[q] == mid[q],
            len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else if i < len { all0[i] } else { all0[i - len] },
            len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else { all0[i - n] },
        ensures
            final(perms).keeps(o),
            o.claimed() ==> final(perms).claimed(),
            final(perms).follows(o),
            final(perms).zero_ok(),
            final(perms).tokened(),
    {
        let slots = o.hdr(p).slots;
        let c = o.cls[p];
        assert(cell_ok(p, o.hdrs[p], o.slots));
        lemma_rebuild_moves(all0, allf, len, n);
        assert forall|q: *mut *mut u8| #[trigger] o.slots.contains_key(q) implies {
            &&& perms.slots.contains_key(q)
            &&& perms.slots[q].ptr() == o.slots[q].ptr()
            &&& (o.slots[q].is_init() ==> perms.slots[q].is_init())
        } by {
            assert(mid.contains_key(q));
        }
        lemma_stack_fill(o.hdr(p), perms.hdr(p), o.slots, perms.slots, 0, len + n);
        CachePerms::lemma_update(o, *perms, p);
        lemma_rebuild_tracks(o, mid, *perms, p, len, want, n, claimed, outf, all0, allf);
        // The claim's tokens, reversed with its slots and moved to the
        // bottom with them, are the new zero prefix.
        CachePerms::lemma_slice_frame(o, o.slots, mid, p, len, want);
        CachePerms::lemma_slice_frame(o, mid, perms.slots, p, 0, len + n);
        let ghost mid2 = *perms;
        zero::lemma_rebuild_entries(perms.slots, mid, slots, len, want, n, c, claimed, outf, all0, allf);
        assert forall|i: int| 0 <= i < n implies #[trigger] plain(perms.slots[perms.ent(p, i)].value(), perms.hdr(p).slots) == claimed[n - 1 - i]@.addr by {
            assert(perms.ent(p, i) == slot_ptr(slots, i));
        }
        CachePerms::lemma_install_claim(perms, z, o, p, n, zeroed, claimed);
        CachePerms::lemma_same_but_tokens(mid2, *perms, o);
        // The slot tokens: the old entries' and the claim's, by address, go
        // back under the entries the moves left them in.
        let ghost before_t = *perms;
        CachePerms::lemma_rebuild_step(perms, ctoks, o, mid, p, len, want, n, claimed, outf, all0, allf);
        assert(*perms == (CachePerms { toks: perms.toks, ..before_t }));
        CachePerms::lemma_zero_same(before_t, *perms);
        CachePerms::lemma_same_but_tokens(before_t, *perms, o);
        if o.claimed() {
            CachePerms::lemma_claimed_hdr(o, *perms, p);
        }
    }
}

} // verus!
