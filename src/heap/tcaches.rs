//! Explicit caches (design.md section 13.3).
//!
//! An explicit cache (`tcache.create`) is a [`Heap`] owned by no thread:
//! same caches, same refill and flush, no TLS. It is not thread safe, as in
//! jemalloc. `MALLOCX_TCACHE_NONE` bypasses every cache and claims or
//! releases straight against the slab bitmaps (the defragmenter relies on
//! this to land allocations in the fullest slabs).

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::lock::{Lock, LockOrder};
#[cfg(verus_keep_ghost)]
use crate::lock::{rank_class, rank_tcaches};
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, verus};
use vstd::raw_ptr::{PointsTo, ptr_mut_write};

#[cfg(verus_keep_ghost)]
use super::thread::HeapPerm;
use super::thread::slot_heap_ref;
use super::{FreshHeap, Heap, Shared};
use crate::sys::tls::HeapLoan;

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
    /// (`init` creates it so).
    #[verifier::type_invariant]
    spec fn rank_ok(self) -> bool {
        self.lock.rank() == rank_tcaches()
    }
}

/// Assumption (`lock.rs`): the explicit-cache table lock guards no value
/// (`()`), so its invariant is trivially true. Nothing writes the value.
pub broadcast axiom fn axiom_tcaches_lock_inv(v: ())
    ensures
        #[trigger] crate::lock::lock_inv(v),
;

/// The empty table [`Tcaches::init`] writes, every entry null. Trusted
/// only as a value of its type: Verus has no array fill of a type that is
/// not `Copy`, and nothing is claimed about the entries.
#[verifier::external_body]
#[allow(clippy::declare_interior_mutable_const, reason = "only moved into a new value, never shared")]
const EMPTY_TABLE: [AtomicPtr<u8>; MAX_TCACHES] = [const { AtomicPtr::new(ptr::null_mut()) }; MAX_TCACHES];

impl Tcaches {
    /// Initialise `this` in place, empty, through the permission `perm` to
    /// its storage, handed back initialised: one write of the whole value,
    /// whose lock is created with rank `rank_tcaches()` (the type
    /// invariant) and whose table entries are null.
    ///
    /// # Safety
    /// `this` must point to writable storage that nothing else reads until
    /// this returns.
    pub unsafe fn init(this: *mut Tcaches, Tracked(perm): Tracked<&mut PointsTo<Tcaches>>)
        requires
            old(perm).ptr() == this,
            old(perm).is_uninit(),
        ensures
            final(perm).ptr() == this,
            final(perm).is_init(),
    {
        broadcast use axiom_tcaches_lock_inv;

        // Through the permission to the storage, which the caller makes
        // writable and unaliased.
        ptr_mut_write(
            this,
            Tracked(perm),
            Tcaches {
                lock: Lock::new((), Ghost(rank_tcaches())),
                table: EMPTY_TABLE,
            },
        );
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
}

/// Ghost: the right to hold one reference to an existing heap, which
/// [`heap_at`] (and through it [`Tcaches::get`]) lends for as long as the
/// reference lives, so one token gives one reference at a time.
/// `Heap::destroy` takes the token by value and keeps it, so a destroyed
/// heap cannot be reached again with the token that destroyed it. Its
/// field is private, so outside this module only [`axiom_heap_use`]
/// makes one.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct HeapUse {
    ghost unit: (),
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
/// (the heap it could not register).
pub axiom fn axiom_heap_use() -> (tracked u: HeapUse);

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
/// address space. No memory precondition is stated: no caller can show
/// one. `Tcaches::get` has a pointer loaded from its table, the thread
/// key's destructor the one the C library hands it, and `Heap::destroy`
/// the one either of them hands on; the permissions creation returned
/// are dropped, and carrying them from one call to the next would need a
/// ghost registry of live explicit caches, which callers use without a
/// lock (the C caller's contract).
///
/// # Safety
/// `p` is such a heap, and nothing else uses it while the reference
/// lives (see [`Tcaches::get`]).
#[verifier::external_body]
#[inline(always)]
pub(super) unsafe fn heap_at(p: *mut Heap, Tracked(_u): Tracked<&mut HeapUse>) -> (r: &mut Heap)
    ensures
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
/// `mark_bound` stored them). Nothing is claimed about the heap's ghost
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
        perms.fresh(p),
        old(loan).heap() is None,
    ensures
        r.others@.addr == 0,
        r.arena as int == s.arena(),
        r.bound == s.bound(),
        r.shared == s.shared(),
        final(loan).heap() matches Some(hp) && hp.addr() == p@.addr && hp.value() == *final(r),
{
    let tracked hp = HeapPerm::assemble(s, perms);
    proof {
        loan.put(hp);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { slot_heap_ref(p, Tracked(loan)) }
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
        let (h, _, _) = Heap::create_detached(shared, arena, Tracked(&mut under));
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
    /// `dallocx`, `sdallocx`) inherits that rule. The reference borrows
    /// the caller's token `u` ([`HeapUse`]) for as long as it lives.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn get<'a>(&'a self, id: u16, Tracked(u): Tracked<&'a mut HeapUse>) -> (r: Option<&'a mut Heap>)
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
        // Premise (`axiom_heap_use`): `p` is now unreachable through the
        // table, and `tcache.destroy`'s caller uses the cache on no other
        // thread.
        let tracked u = axiom_heap_use();
        // SAFETY: `p` came from `create_detached` and is now unreachable
        // through the table.
        unsafe { Heap::destroy(p, shared, Tracked(u), Tracked(&mut under)) };
        true
    }
}

} // verus!

#[cfg(test)]
mod tests {
    /// `axiom_heap_use` is called once in each of the 8 functions its doc
    /// lists, and nowhere else: each call rests on that doc's premise for
    /// its own caller, so a new call needs a premise of its own.
    #[test]
    fn heap_use_axiom_only_at_listed_mint_sites() {
        // Spelled in parts, so that this test's own lines match nothing.
        let name = std::format!("{}{}", "axiom_", "heap_use");
        let call = std::format!("{name}(");
        let sites: std::vec::Vec<_> = crate::sys::tests::grant_sites(&name)
            .into_iter()
            .filter(|(_, at)| at.contains(call.as_str()) && !at.contains("axiom fn "))
            .collect();
        let callers = [
            ("heap/mallocx.rs:", "pub fn alloc("),
            ("heap/mallocx.rs:", "pub fn alloc_huge("),
            ("heap/mallocx.rs:", "pub fn free("),
            ("api/jemalloc.rs:", "fn sdallocx_routed("),
            ("heap/tcaches.rs:", "pub fn flush("),
            ("heap/tcaches.rs:", "pub fn destroy("),
            (
                "heap/thread.rs:",
                "pub unsafe extern \"C\" fn heap_destructor(",
            ),
            ("heap/thread.rs:", "fn create_heap_for_thread<"),
        ];
        for (sig, at) in &sites {
            assert!(
                callers
                    .iter()
                    .any(|(file, f)| at.starts_with(file) && sig.starts_with(f)),
                "{at}: a heap-use token made in `{sig}`"
            );
        }
        for (file, f) in callers {
            let n = sites
                .iter()
                .filter(|(sig, at)| at.starts_with(file) && sig.starts_with(f))
                .count();
            assert_eq!(n, 1, "{file} `{f}`: got = {n} tokens, want = 1");
        }
    }
}
