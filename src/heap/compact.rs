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

use super::{CacheHeader, Heap, Shared, stack_mut};

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

} // verus!

impl CacheHeader {
    /// Whether the cache is currently compacting its class.
    #[cfg(test)]
    pub fn compacting(&self) -> bool {
        self.mode == MODE_COMPACT
    }
}

verus! {

impl CacheHeader {
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

} // verus!

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
    ) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let pac = (*self.shared).pac();
            match self.hdr_ref(cache).mode {
                MODE_COMPACT => {}
                MODE_PROBE => {
                    let shared: &Shared = &*self.shared;
                    // The probe is also a heartbeat for the periodic work: a
                    // balanced churn on a dense class takes no other slow
                    // path, and without it no GC or decay would ever run on
                    // a thread without a background thread.
                    self.tick::<true>(shared);
                    let mut len = self.hdr_ref(cache).len;
                    if len == 0 {
                        // The GC emptied the cache.
                        let Some((a, _)) =
                            shared.arena(self.hdr_ref(cache).arena, Tracked::assume_new())
                        else {
                            return (ptr::null_mut(), false);
                        };
                        len = self.refill(
                            shared,
                            a,
                            Tracked::assume_new(),
                            cache,
                            class,
                            Tracked::assume_new(),
                        );
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    let (refilled, verdict) = Self::probe_verdict(
                        shared,
                        self.hdr_ref(cache).arena,
                        class,
                        self.hdr_ref(cache).probe_refilled,
                        Tracked::assume_new(),
                    );
                    let h = self.hdr_mut(cache);
                    h.probe_refilled = refilled;
                    match verdict {
                        Verdict::Refilling | Verdict::Dense => {
                            return Self::pop_top(
                                cache,
                                len,
                                pac,
                                Tracked::assume_new(),
                                Tracked::assume_new(),
                            );
                        }
                        Verdict::Fragmented => {}
                    }
                    let h = self.hdr_mut(cache);
                    h.set_mode(MODE_COMPACT);
                    return self.pop_compact_run(cache, class, len);
                }
                _ => {
                    return Self::pop_top(
                        cache,
                        len,
                        pac,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    );
                }
            }
            self.pop_compact_run(cache, class, len)
        }
    }

    /// The compacting half of [`Heap::pop_compact`]. A cache that is not
    /// (or, after its rebuild, no longer) in compact mode pops the top.
    ///
    /// # Safety
    /// As for [`Heap::pop`].
    pub(super) unsafe fn pop_compact_run(
        &mut self,
        cache: *mut CacheHeader,
        class: usize,
        len: u32,
    ) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let pac = (*self.shared).pac();
            match self.hdr_ref(cache).mode {
                MODE_COMPACT => {}
                _ => {
                    return Self::pop_top(
                        cache,
                        len,
                        pac,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    );
                }
            }
            let mut len = len;
            if self.hdr_ref(cache).run_end == 0 {
                self.rebuild_run(cache, class);
                len = self.hdr_ref(cache).len;
                if self.hdr_ref(cache).run_end == 0 {
                    if len == 0 {
                        // The rebuild's tick ran the cache GC, which
                        // flushes every entry of a cache with `low_water
                        // == len < 4`, and the rebuild then claimed
                        // nothing (the class is no longer fragmented, or
                        // the claim came back empty). Refill as the probe
                        // branch does; popping here would wrap `len - 1`.
                        let shared: &Shared = &*self.shared;
                        let Some((a, _)) =
                            shared.arena(self.hdr_ref(cache).arena, Tracked::assume_new())
                        else {
                            return (ptr::null_mut(), false);
                        };
                        len = self.refill(
                            shared,
                            a,
                            Tracked::assume_new(),
                            cache,
                            class,
                            Tracked::assume_new(),
                        );
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    return Self::pop_top(
                        cache,
                        len,
                        pac,
                        Tracked::assume_new(),
                        Tracked::assume_new(),
                    );
                }
            }
            let h = self.hdr_mut(cache);
            // The run invariant `run_end <= low_water <= len` is what keeps
            // this pop from serving a stale pointer out of `slots[run_end -
            // 1]`, and `zeroed <= low_water` what keeps the zero prefix
            // inside the stack; a violation is a heap inconsistency and
            // aborts in release.
            assert!(
                h.run_end <= h.low_water && h.low_water <= len && h.zeroed <= h.low_water,
                "heap: cache run invariant violated (run_end or zeroed > low_water, or low_water > len)"
            );
            let top = len - 1;
            let at = h.run_end - 1;
            let slots = h.slots();
            let p = *slots.add(at as usize);
            // Entries move within the stack signed (the modifier is the
            // stack's base, not the entry's position); only the one leaving
            // is authenticated.
            let p = if pac {
                addr_to_ptr(pac::auth(p as usize, slots as usize))
            } else {
                p
            };
            let t = *slots.add(top as usize);
            *slots.add(at as usize) = t;
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
    /// # Safety
    /// `cache` is this heap's real cache for `class`, in compact mode.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn rebuild_run(&mut self, cache: *mut CacheHeader, class: usize) {
        // SAFETY: a heap never outlives the `Shared` it was created on; the
        // unbounded lifetime lets `self` be borrowed mutably below.
        let shared: &Shared = unsafe { &*self.shared };
        self.tick::<true>(shared);
        // SAFETY: guaranteed by the caller; `len <= cap` entries exist and
        // `cap - len` slots of storage follow them.
        unsafe {
            let arena = self.hdr_ref(cache).arena;
            if !Self::fragmented(shared, arena, class, Tracked::assume_new()) {
                shared.owner(arena).0.class(class).note_compact_mode(false);
                let h = self.hdr_mut(cache);
                h.set_mode(MODE_PROBE);
                return;
            }
            let cap = self.hdr_ref(cache).cap as usize;
            let half = (cap / 2).max(1);
            let block = (cap / 4).max(1);
            let len = self.hdr_ref(cache).len as usize;
            if len + block > cap && len > half {
                self.flush(
                    shared,
                    arena,
                    cache,
                    class,
                    len - half,
                    Tracked::assume_new(),
                );
            }
            let len = self.hdr_ref(cache).len as usize;
            let want = block.min(cap - len);
            if want == 0 {
                return;
            }
            let slots = self.hdr_ref(cache).slots;
            let out = stack_mut(slots, len, want, Tracked::assume_new());
            let (claim, _) = self.claim(
                shared,
                shared.owner(arena).0.class(class),
                out,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            let n = claim.n;
            if n == 0 {
                return;
            }
            shared.owner(arena).0.class(class).note_compact(n);
            out[..n].reverse();
            if shared.pac() {
                CacheHeader::sign_all(slots, &mut out[..n]);
            }
            let all = stack_mut(slots, 0, len + n, Tracked::assume_new());
            if len >= n {
                // [old..][block] -> [block][old[n..]][old[..n]]
                let (bottom, rest) = all.split_at_mut(n);
                bottom.swap_with_slice(&mut rest[len - n..]);
            } else {
                // The block is bigger than what was there: rotate it down.
                all.rotate_left(len);
            }
            let h = self.hdr_mut(cache);
            let new_len = (len + n) as u32;
            h.len = new_len;
            h.moved = h.moved.wrapping_add(n as u64);
            h.run_end = n as u32;
            h.low_water = h.low_water.max(n as u32);
            h.zeroed = if claim.zeroed { n as u32 } else { 0 };
        }
    }
}

verus! {

impl Heap {
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

} // verus!
