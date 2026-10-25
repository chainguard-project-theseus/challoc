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

/// The empty table [`Tcaches::init`] writes, every entry null.
#[allow(
    clippy::declare_interior_mutable_const,
    reason = "only moved into a new value, never shared"
)]
const EMPTY_TABLE: [AtomicPtr<u8>; MAX_TCACHES] =
    [const { AtomicPtr::new(ptr::null_mut()) }; MAX_TCACHES];

/// A heap [`Heap::create`] or [`Heap::create_detached`] returned and
/// nothing has destroyed, as a reference: an explicit cache's non-null
/// table entry, or the heap `Heap::destroy` is handed.
///
/// # Safety
/// `p` is such a heap, and nothing else uses it while the reference
/// lives (see [`Tcaches::get`]).
#[inline(always)]
pub(super) unsafe fn heap_at<'a>(p: *mut Heap) -> &'a mut Heap {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *p }
}

impl Tcaches {
    /// Initialise `this` in place, empty: one write of the whole value,
    /// whose table entries are null.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns.
    pub unsafe fn init(this: *mut Tcaches) {
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            ptr::write(
                this,
                Tcaches {
                    lock: Lock::new(()),
                    table: EMPTY_TABLE,
                },
            )
        };
    }

    /// The table lock, for the fork handlers. It is the outermost
    /// allocator lock: `create` and `destroy` hold it across
    /// `Heap::create_detached` / `Heap::destroy`, which take the arena
    /// creation, class and metadata locks.
    pub fn raw_lock(&self) -> &crate::lock::RawLock {
        self.lock.raw()
    }

    /// `tcache.create`: a new explicit cache, home arena `arena`. `None`
    /// when the table or the metadata region is full.
    pub fn create(&self, shared: &Shared, arena: u16) -> Option<u16> {
        let _g = self.lock.lock();
        // The first null entry, lowest index first.
        let mut slot = 0;
        while !self.table[slot].load(Ordering::Relaxed).is_null() {
            slot += 1;
            if slot == MAX_TCACHES {
                return None;
            }
        }
        let h = Heap::create_detached(shared, arena);
        if h.is_null() {
            return None;
        }
        self.table[slot].store(h as *mut u8, Ordering::Release);
        Some(slot as u16)
    }

    /// The explicit cache `id`, if it exists. Not thread safe: two threads
    /// using one explicit cache at once is the caller's bug, as in
    /// jemalloc, and so is using an id after `tcache.destroy`: `destroy`
    /// frees the heap while another thread may still hold the `&mut Heap`
    /// this returns. Every `MALLOCX_TCACHE(t)` path (`mallocx`, `rallocx`,
    /// `dallocx`, `sdallocx`) inherits that rule.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self, id: u16) -> Option<&mut Heap> {
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
    pub fn flush(&self, shared: &Shared, id: u16) -> bool {
        match self.get(id) {
            Some(h) => {
                h.flush_all(shared);
                true
            }
            None => false,
        }
    }

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
        // The non-null entries.
        let mut n = 0;
        for i in 0..MAX_TCACHES {
            if !self.table[i].load(Ordering::Relaxed).is_null() {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
