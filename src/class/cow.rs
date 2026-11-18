//! A fork epoch's per-`(arena, class)` floors (design 5, "Fork epochs";
//! `crate::cow`).
//!
//! While a fork child shares the parent's pages, a write of the parent to
//! one of them copies the page. The allocator's share of those writes is
//! the program's allocations, which a refill serves from the lowest slab
//! with a free slot: at fork time that is a slab the child shares. An
//! epoch's floor is this arena's commit cursor at the fork, so every slab
//! of the arena below it may hold pages the child shares, and none at or
//! above it does (a slab's pages are untouched until a refill hands it
//! out, address-ordered from the cursor). While the floor is in force a
//! refill takes slots only at or above it, or from a `Purged` slab (an
//! eager discard left no page to share): `ClassState::plan` and
//! `next_slab`.

use super::ClassState;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use core::sync::atomic::Ordering;
use vstd::prelude::*;

verus! {

/// The most slabs [`ClassState::live_at_or_above`] reads.
pub const MAX_DRAIN_WALK: usize = 65536;

impl ClassState {
    /// The floor a refill honours: this pair's floor while an epoch is in
    /// force ([`crate::cow::in_force`]), else 0 (none).
    #[inline(always)]
    pub fn cow_floor(&self) -> usize {
        if crate::cow::in_force() {
            self.cow_floor.load(Ordering::Relaxed) as usize
        } else {
            0
        }
    }

    /// Set this pair's floor to the arena's commit cursor, under the class
    /// lock, so that a refill in flight either finished before or sees it:
    /// the start of an epoch (`cow::begin`, before the fork). `ord` is the
    /// caller's lock-order token (ghost) for the class lock.
    pub fn cow_mark(&self, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        self.cow_floor.store(g.group_next as u64, Ordering::Relaxed);
        g.unlock();
    }

    /// The end of an epoch (`cow::end`): the floor goes, and becomes this
    /// pair's drain floor ([`ClassState::drain_floor`]) when the arena's
    /// cursor moved past it, i.e. a refill took a slab above it and blocks
    /// of the epoch may sit there. `ord` is the caller's lock-order token
    /// (ghost) for the class lock.
    pub fn cow_settle(&self, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let f = self.cow_floor.load(Ordering::Relaxed);
        self.cow_floor.store(0, Ordering::Relaxed);
        if f != 0 {
            let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
            if g.group_next as u64 != f {
                self.drain_floor.store(f, Ordering::Relaxed);
            }
            g.unlock();
        }
    }

    /// Whether slot `p` of this pair's class lies in a slab below the
    /// floor in force ([`ClassState::cow_floor`]): a free of it is kept
    /// out of the cache `malloc` pops (`Heap::free`). Slow paths only.
    pub fn cow_below(&self, p: *mut u8) -> bool {
        let f = self.cow_floor();
        f != 0 && self.table().slab_index(p) < f
    }

    /// The last epoch's floor while blocks above it are to drain back
    /// below it (`cow::drain_move`), else 0.
    #[inline(always)]
    pub fn drain_floor(&self) -> usize {
        self.drain_floor.load(Ordering::Relaxed) as usize
    }

    /// Whether a committed slab of this pair's arena at or above `f`
    /// holds a live slot, read without the lock from the descriptors (the
    /// drain's check that blocks of the epoch are left to move). Walks at
    /// most [`MAX_DRAIN_WALK`] slabs and answers true past them.
    pub fn live_at_or_above(&self, f: usize) -> bool {
        let n = self.committed_slabs();
        let t = self.table();
        let mut s = f;
        let mut walked: usize = 0;
        while s < n
            invariant
                n <= self.spec_nslabs(),
                *t == self.spec_table(),
            decreases n - s,
        {
            if walked >= MAX_DRAIN_WALK {
                return true;
            }
            walked += 1;
            if let Some(d) = t.desc_if_committed(s) {
                if d.live() > 0 && d.arena_unheld(self.desc_inv(s)) == self.arena {
                    return true;
                }
            }
            s += 1;
        }
        false
    }

    /// The drain of this pair is over: no slab below its drain floor has
    /// a free slot left to move a block into, or no block above it.
    pub fn drain_done(&self) {
        self.drain_floor.store(0, Ordering::Relaxed);
    }
}

} // verus!
