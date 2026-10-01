//! Explicit caches (design.md section 13.3).
//!
//! An explicit cache (`tcache.create`) is a [`Heap`] owned by no thread:
//! same caches, same refill and flush, no TLS. It is not thread safe, as in
//! jemalloc. `MALLOCX_TCACHE_NONE` bypasses every cache and claims or
//! releases straight against the slab bitmaps (the defragmenter relies on
//! this to land allocations in the fullest slabs).

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::lock::Lock;

use super::{Heap, Shared};
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;

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
            ptr::write(&raw mut (*this).lock, Lock::new(()));
            let table = &raw mut (*this).table;
            for i in 0..MAX_TCACHES {
                ptr::write(&raw mut (*table)[i], AtomicPtr::new(ptr::null_mut()));
            }
        }
    }

    /// `tcache.create`: a new explicit cache, home arena `arena`. `None`
    /// when the table or the metadata region is full.
    pub fn create(&self, shared: &Shared, arena: u16) -> Option<u16> {
        let _g = self.lock.lock();
        let slot = self
            .table
            .iter()
            .position(|p| p.load(Ordering::Relaxed).is_null())?;
        let h = Heap::create_detached(shared, arena);
        if h.is_null() {
            return None;
        }
        self.table[slot].store(h as *mut u8, Ordering::Release);
        Some(slot as u16)
    }
}

verus! {

/// The explicit cache a non-null table entry points at. Trusted: Verus
/// has no raw-pointer dereference, and the heap `Tcaches::create`
/// published is well formed ([`Heap::wf`]), as every heap a verified path
/// is handed.
///
/// # Safety
/// `p` is a non-null entry of `Tcaches::table`, and nothing else uses the
/// heap while the reference lives (see [`Tcaches::get`]).
#[verifier::external_body]
#[inline(always)]
unsafe fn heap_at<'a>(p: *mut Heap) -> (r: &'a mut Heap)
    ensures
        r.wf(),
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *p }
}

impl Tcaches {
    /// The table lock, for the fork handlers. It is the outermost
    /// allocator lock: `create` and `destroy` hold it across
    /// `Heap::create_detached` / `Heap::destroy`, which take the arena
    /// creation, class and metadata locks.
    pub fn raw_lock(&self) -> &crate::lock::RawLock {
        self.lock.raw()
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
    /// `false` if there is no such cache.
    pub fn flush(&self, shared: &Shared, id: u16) -> bool
        requires
            shared.wf(),
    {
        match self.get(id) {
            Some(h) => {
                h.flush_all(shared);
                true
            }
            None => false,
        }
    }
}

} // verus!

impl Tcaches {
    /// `tcache.destroy`: flush and free `id`. `false` if there is no such
    /// cache.
    pub fn destroy(&self, shared: &Shared, id: u16) -> bool {
        let _g = self.lock.lock();
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
        unsafe { Heap::destroy(p, shared) };
        true
    }

    /// Number of live explicit caches (for tests and stats).
    pub fn len(&self) -> usize {
        self.table
            .iter()
            .filter(|p| !p.load(Ordering::Relaxed).is_null())
            .count()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
