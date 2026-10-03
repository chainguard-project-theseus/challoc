//! Explicit caches (design.md section 13.3).
//!
//! An explicit cache (`tcache.create`) is a [`Heap`] owned by no thread:
//! same caches, same refill and flush, no TLS. It is not thread safe, as in
//! jemalloc. `MALLOCX_TCACHE_NONE` bypasses every cache and claims or
//! releases straight against the slab bitmaps (the defragmenter relies on
//! this to land allocations in the fullest slabs).

use crate::sync::atomic::{AtomicPtr, Ordering};
use core::ptr;

use crate::lock::{Lock, LockOrder};
#[cfg(verus_only)]
use crate::lock::{rank_class, rank_tcaches};

use super::{Heap, Shared};
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};

verus! {

/// Explicit caches; ids are the 12-bit `MALLOCX_TCACHE` field minus 2.
pub const MAX_TCACHES: usize = 1024;

/// Explicit caches (`tcache.create` / `tcache.flush` / `tcache.destroy`):
/// heaps owned by no thread, looked up by id.
pub struct Tcaches {
    lock: Lock<()>,
    /// The heaps, as `*mut Heap` cast to bytes: `Heap` reaches `Shared`,
    /// which holds this table, and Verus rejects a type that contains
    /// itself through a pointer's type parameter. The casts at the load
    /// and store sites are no-ops.
    table: [AtomicPtr<u8>; MAX_TCACHES],
}

impl Tcaches {
    /// The table lock is the explicit-cache table lock of the lock order
    /// (`init` creates it so; trusted with `init`).
    #[verifier::type_invariant]
    spec fn rank_ok(self) -> bool {
        self.lock.rank() == rank_tcaches()
    }
}

} // verus!

impl Tcaches {
    /// Initialise `this` in place, empty.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns.
    pub unsafe fn init(this: *mut Tcaches) {
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            // Rank `rank_tcaches()` (the type invariant, trusted with this
            // function, which is not verified).
            ptr::write(&raw mut (*this).lock, Lock::new((), Ghost::assume_new()));
            let table = &raw mut (*this).table;
            for i in 0..MAX_TCACHES {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
            }
        }
    }
}

verus! {

/// A heap [`Heap::create`] or [`Heap::create_detached`] returned and
/// nothing has destroyed: an explicit cache's non-null table entry, or
/// the calling thread's heap at exit. Trusted: Verus has no raw-pointer
/// dereference, and such a heap is well formed ([`Heap::wf`]), as every
/// existing heap a verified path is handed: creation makes it so
/// ([`CachePerms::fresh`](super::CachePerms::fresh); `Tcaches::create`
/// takes no reference to its heap, so it drops them), every
/// writer of its caches proves it keeps `wf`, and no ghost state carries
/// that from one call to the next.
///
/// # Safety
/// `p` is such a heap, and nothing else uses it while the reference
/// lives (see [`Tcaches::get`]).
#[verifier::external_body]
#[inline(always)]
pub(super) unsafe fn heap_at<'a>(p: *mut Heap) -> (r: &'a mut Heap)
    ensures
        r.wf(),
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *p }
}

/// The heap [`Heap::create`] has just returned, as a reference, before
/// its ghost state is installed: its `others` field is null, as
/// `init_fields` stored it. Trusted: Verus has no raw-pointer
/// dereference. Nothing is claimed about its ghost fields: the caller
/// installs the permissions creation returned (`create_heap_for_thread`).
///
/// # Safety
/// `p` is a heap `Heap::create` has just returned, and nothing else uses
/// it while the reference lives.
#[verifier::external_body]
#[inline(always)]
pub(super) unsafe fn heap_new<'a>(p: *mut Heap) -> (r: &'a mut Heap)
    ensures
        r.others@.addr == 0,
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *p }
}

impl Tcaches {
    /// `tcache.create`: a new explicit cache, home arena `arena`. `None`
    /// when the table or the metadata region is full.
    // Not isolated: the search returns while the guard borrows `ord`.
    #[verifier::loop_isolation(false)]
    pub fn create(&self, shared: &Shared, arena: u16, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u16>
        requires
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_tcaches()),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let _g = self.lock.lock(Tracked(ord), Tracked(&mut under));
        // The first null entry, lowest index first.
        let mut slot = 0;
        while !self.table[slot].load(Ordering::Relaxed).is_null()
            invariant
                slot < MAX_TCACHES,
            decreases MAX_TCACHES - slot,
        {
            slot += 1;
            if slot == MAX_TCACHES {
                return None;
            }
        }
        let (h, _) = Heap::create_detached(shared, arena, Tracked(&mut under));
        if h.is_null() {
            return None;
        }
        self.table[slot].store(h as *mut u8, Ordering::Release);
        Some(slot as u16)
    }

    /// The table lock, for the fork handlers. It is the outermost
    /// allocator lock: `create` and `destroy` hold it across
    /// `Heap::create_detached` / `Heap::destroy`, which take the metadata
    /// lock, and the class, metadata and huge locks.
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

    /// The explicit cache `id`, if it exists. Not thread safe: two threads
    /// using one explicit cache at once is the caller's bug, as in
    /// jemalloc, and so is using an id after `tcache.destroy`: `destroy`
    /// frees the heap while another thread may still hold the `&mut Heap`
    /// this returns. Every `MALLOCX_TCACHE(t)` path (`mallocx`, `rallocx`,
    /// `dallocx`, `sdallocx`) inherits that rule.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self, id: u16) -> (r: Option<&mut Heap>)
        ensures
            r matches Some(h) ==> h.wf(),
            // An id no `tcache.create` hands out names no cache.
            id as usize >= MAX_TCACHES ==> r is None,
    {
        let i = id as usize;
        if i >= MAX_TCACHES {
            return None;
        }
        let p = self.table[i].load(Ordering::Acquire) as *mut Heap;
        // SAFETY: published after `Heap::create_detached`; only `destroy`
        // frees it, and using a destroyed id is the caller's bug.
        if p.is_null() {
            None
        } else {
            Some(unsafe { heap_at(p) })
        }
    }

    /// `tcache.flush`: return every cached slot of `id` to its slabs.
    /// `false` if there is no such cache. Afterwards every home and
    /// `others`-node cache of the explicit cache's heap is empty
    /// ([`Heap::flush_all`]; theorem "cache flush completeness").
    pub fn flush(&self, shared: &Shared, id: u16, Tracked(ord): Tracked<&mut LockOrder>) -> (r: bool)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            id as usize >= MAX_TCACHES ==> !r,
            *final(ord) == *old(ord),
    {
        match self.get(id) {
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

    /// `tcache.destroy`: flush and free `id`. `false` if there is no such
    /// cache.
    pub fn destroy(&self, shared: &Shared, id: u16, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
            old(ord).allows(rank_tcaches()),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let _g = self.lock.lock(Tracked(ord), Tracked(&mut under));
        let i = id as usize;
        if i >= MAX_TCACHES {
            return false;
        }
        let p = self.table[i].swap(ptr::null_mut(), Ordering::AcqRel) as *mut Heap;
        if p.is_null() {
            return false;
        }
        // SAFETY: `p` came from `create_detached` and is now unreachable
        // through the table.
        unsafe { Heap::destroy(p, shared, Tracked(&mut under)) };
        true
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
