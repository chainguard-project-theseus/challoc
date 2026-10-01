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

use crate::layout::CLASSES;
use crate::sys::pac;

use super::{CacheHeader, Heap, Shared};

/// Cache modes: off (the `FOREIGN` cache), probing the class for
/// fragmentation every [`PROBE_MASK`] + 1 pops, compacting.
pub const MODE_OFF: u16 = 0;
pub const MODE_PROBE: u16 = 1;
pub const MODE_COMPACT: u16 = 2;

/// Pops between two fragmentation probes of a class (minus one).
pub const PROBE_MASK: u32 = (1 << 12) - 1;
/// A compacting cache serves every `2^COMPACT_SHIFT`th pop of a class
/// from the lowest-address cached slot instead of the top of the stack
/// (design 6.4).
pub const COMPACT_SHIFT: u32 = 3;
/// The scheduled-pop mask of a compacting cache: a pop whose count has
/// no bit of it set is served from the bottom of the run.
pub const COMPACT_MASK: u32 = (1 << COMPACT_SHIFT) - 1;
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

    /// The scheduled-pop mask `mode` implies: [`PROBE_MASK`] while
    /// probing, [`COMPACT_MASK`] while compacting, `u32::MAX` when off (one pop in 2^32 then reaches
    /// `pop_compact`, which pops the top). The one home of the
    /// mode-to-mask rule; [`CacheHeader::set_mode`] applies it.
    pub(super) fn mask_for(mode: u16) -> u32 {
        match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => COMPACT_MASK,
            _ => u32::MAX,
        }
    }

    /// Enter `mode` and derive the scheduled-pop mask from it
    /// ([`CacheHeader::mask_for`]), so the two fields never disagree.
    pub(super) fn set_mode(&mut self, mode: u16) {
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
    ) -> (*mut u8, bool) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            let pac = (*self.shared).pac();
            match (*cache).mode {
                MODE_COMPACT => {}
                MODE_PROBE => {
                    let shared: &Shared = &*self.shared;
                    // The probe is also a heartbeat for the periodic work: a
                    // balanced churn on a dense class takes no other slow
                    // path, and without it no GC or decay would ever run on
                    // a thread without a background thread.
                    self.tick(shared);
                    let mut len = (*cache).len;
                    if len == 0 {
                        // The GC emptied the cache.
                        let Some(a) = shared.arena((*cache).arena) else {
                            return (ptr::null_mut(), false);
                        };
                        len = self.refill(shared, a, cache, class);
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    let (refilled, verdict) =
                        Self::probe_verdict(shared, (*cache).arena, class, (*cache).probe_refilled);
                    (*cache).probe_refilled = refilled;
                    match verdict {
                        Verdict::Refilling | Verdict::Dense => {
                            return Self::pop_top(cache, len, pac);
                        }
                        Verdict::Fragmented => {}
                    }
                    (*cache).set_mode(MODE_COMPACT);
                    return self.pop_compact_run(cache, class, len);
                }
                _ => return Self::pop_top(cache, len, pac),
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
            match (*cache).mode {
                MODE_COMPACT => {}
                _ => return Self::pop_top(cache, len, pac),
            }
            let mut len = len;
            if (*cache).run_end == 0 {
                self.rebuild_run(cache, class);
                len = (*cache).len;
                if (*cache).run_end == 0 {
                    if len == 0 {
                        // The rebuild's tick ran the cache GC, which
                        // flushes every entry of a cache with `low_water
                        // == len < 4`, and the rebuild then claimed
                        // nothing (the class is no longer fragmented, or
                        // the claim came back empty). Refill as the probe
                        // branch does; popping here would wrap `len - 1`.
                        let shared: &Shared = &*self.shared;
                        let Some(a) = shared.arena((*cache).arena) else {
                            return (ptr::null_mut(), false);
                        };
                        len = self.refill(shared, a, cache, class);
                        if len == 0 {
                            return (ptr::null_mut(), false);
                        }
                    }
                    return Self::pop_top(cache, len, pac);
                }
            }
            // The run invariant `run_end <= low_water <= len` is what keeps
            // this pop from serving a stale pointer out of `slots[run_end -
            // 1]`, and `zeroed <= low_water` what keeps the zero prefix
            // inside the stack; a violation is a heap inconsistency and
            // aborts in release.
            assert!(
                (*cache).run_end <= (*cache).low_water && (*cache).low_water <= len && (*cache).zeroed <= (*cache).low_water,
                "heap: cache run invariant violated (run_end or zeroed > low_water, or low_water > len)"
            );
            let top = len - 1;
            let at = (*cache).run_end - 1;
            let slots = (*cache).slots();
            let p = *slots.add(at as usize);
            // Entries move within the stack signed (the modifier is the
            // stack's base, not the entry's position); only the one leaving
            // is authenticated.
            let p = if pac {
                pac::auth(p as usize, slots as usize) as *mut u8
            } else {
                p
            };
            *slots.add(at as usize) = *slots.add(top as usize);
            (*cache).len = top;
            (*cache).run_end = at;
            if top < (*cache).low_water {
                (*cache).low_water = top;
            }
            // `at` now holds the old top entry: the prefix keeps `at` only
            // when that entry was in it too, and never reaches past `top`.
            let zeroed = (*cache).zeroed;
            let zero = at < zeroed;
            if zero {
                (*cache).zeroed = if top < zeroed { top } else { at };
            }
            (p, zero)
        }
    }

    /// Whether `class` in `arena` is worth compacting (design 6.4).
    pub(super) fn fragmented(shared: &Shared, arena: u16, class: usize) -> bool {
        let cs = shared.owner(arena).class(class);
        let (holes, capacity) = cs.holes();
        holes >= COMPACT_MIN_SLABS * CLASSES[class].compact_slots()
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
    ) -> (u32, Verdict) {
        let cs = shared.owner(arena).class(class);
        let refilled = cs.refilled() as u32;
        let delta = refilled.wrapping_sub(prev_refilled) as usize;
        let verdict = if delta >= COMPACT_MAX_REFILL_SLABS * CLASSES[class].compact_slots() {
            if Self::fragmented(shared, arena, class) {
                cs.note_gate_refill();
            }
            Verdict::Refilling
        } else if !Self::fragmented(shared, arena, class) {
            Verdict::Dense
        } else {
            cs.note_compact_mode(true);
            Verdict::Fragmented
        };
        (refilled, verdict)
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
        self.tick(shared);
        // SAFETY: guaranteed by the caller; `len <= cap` entries exist and
        // `cap - len` slots of storage follow them.
        unsafe {
            let arena = (*cache).arena;
            if !Self::fragmented(shared, arena, class) {
                shared.owner(arena).class(class).note_compact_mode(false);
                (*cache).set_mode(MODE_PROBE);
                return;
            }
            let cap = (*cache).cap as usize;
            let half = (cap / 2).max(1);
            let block = (cap / 4).max(1);
            let len = (*cache).len as usize;
            if len + block > cap && len > half {
                self.flush(shared, arena, cache, class, len - half);
            }
            let len = (*cache).len as usize;
            let want = block.min(cap - len);
            if want == 0 {
                return;
            }
            let slots = (*cache).slots();
            let out = core::slice::from_raw_parts_mut(slots.add(len), want);
            let claim = self.claim(shared, shared.owner(arena).class(class), out);
            let n = claim.n;
            if n == 0 {
                return;
            }
            shared.owner(arena).class(class).note_compact(n);
            out[..n].reverse();
            if shared.pac() {
                CacheHeader::sign_all(slots, &mut out[..n]);
            }
            let all = core::slice::from_raw_parts_mut(slots, len + n);
            if len >= n {
                // [old..][block] -> [block][old[n..]][old[..n]]
                let (bottom, rest) = all.split_at_mut(n);
                bottom.swap_with_slice(&mut rest[len - n..]);
            } else {
                // The block is bigger than what was there: rotate it down.
                all.rotate_left(len);
            }
            let new_len = (len + n) as u32;
            (*cache).len = new_len;
            (*cache).moved = (*cache).moved.wrapping_add(n as u64);
            (*cache).run_end = n as u32;
            (*cache).low_water = (*cache).low_water.max(n as u32);
            (*cache).zeroed = if claim.zeroed { n as u32 } else { 0 };
        }
    }
}
