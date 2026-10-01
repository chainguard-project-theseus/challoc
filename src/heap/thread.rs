//! Thread lifecycle (design 6.5): the TLS slot's sentinels, heap creation
//! on first use, the thread-exit destructor and the slot readers the entry
//! points inline.

use core::ffi::c_void;
use core::ptr;

use crate::sys::pac;

use super::PUSH_PAC;
use super::{Heap, Shared};

/// TLS slot value while this thread's heap is being created and
/// `pthread_setspecific` may re-enter the allocator.
pub const TLS_CREATING: usize = 1;
/// TLS slot value after this thread's heap destructor has run.
pub const TLS_EXITED: usize = 2;

/// Destructor of the thread-exit key: mark the thread as exited, then flush
/// and free its heap. The sentinel goes first so that anything allocating
/// on this thread from here on (a signal handler landing in the middle of
/// `Heap::destroy`, another key's destructor afterwards) is served without
/// a heap instead of reaching the one being freed through `existing_heap`
///
/// # Safety
/// `p` is the heap pointer registered with `pthread_setspecific` by
/// `create_heap_for_thread`, called on the thread that owns it; after this
/// returns the heap is freed and the TLS slot holds `TLS_EXITED`, so it
/// must not be reached through TLS again (the sentinel is what stops
/// `existing_heap` from handing it out).
pub unsafe extern "C" fn heap_destructor(p: *mut c_void) {
    let h = p as *mut Heap;
    crate::tls::set(TLS_EXITED as *mut c_void);
    // SAFETY: the pointer was registered by `heap_for_thread` for this thread.
    unsafe {
        let shared = &*(*h).shared;
        Heap::destroy(h, shared);
    }
}

/// The calling thread's heap, created on first use. `None` when the thread
/// must be served without a cache: heap creation is in progress on it
/// (`pthread_setspecific` re-entered the allocator), its destructor has run,
/// or the metadata region is exhausted.
///
/// The `&mut Heap` is exclusive by construction, not by the type system:
/// only the owning thread reaches its heap through TLS, and every entry
/// point takes the reference once and passes it down, never holding two
/// (a `Shared` method called with one live must not call back into
/// `heap_for_thread` or [`existing_heap`]). The reference is not really
/// `'static` either: the thread-exit destructor frees the heap, so it must
/// not be stored beyond the call.
#[inline(always)]
pub fn heap_for_thread(shared: &Shared) -> Option<&mut Heap> {
    let (t, tp) = crate::tls::get_tp();
    let t = t as usize;
    if t > TLS_EXITED {
        // Anything above the sentinels is this thread's heap pointer,
        // signed with the thread pointer when `PUSH_PAC` is set (the
        // sentinels are never signed, so the compare comes first; a signed
        // pointer keeps its low 48 bits and stays above them).
        // SAFETY: `t` is this thread's slot value above the sentinels.
        return Some(unsafe { heap_from_slot(t, tp, shared.pac()) });
    }
    create_heap_for_thread(shared, t)
}

/// The slot value for heap `h` on the calling thread: `h` signed with the
/// thread pointer when `shared.pac()`, else `h` itself.
fn tls_value_for(shared: &Shared, h: *mut Heap) -> *mut c_void {
    if shared.pac() {
        let (_, tp) = crate::tls::get_tp();
        pac::sign(h as usize, tp) as *mut c_void
    } else {
        h as *mut c_void
    }
}

#[cold]
#[inline(never)]
fn create_heap_for_thread(shared: &Shared, slot: usize) -> Option<&mut Heap> {
    if slot != 0 {
        return None;
    }
    let h = Heap::create(shared);
    if h.is_null() {
        return None;
    }
    crate::tls::set(TLS_CREATING as *mut c_void);
    shared.key.set(h);
    crate::tls::set(tls_value_for(shared, h));
    // SAFETY: freshly created and owned by this thread.
    Some(unsafe { &mut *h })
}

/// The calling thread's heap if it already exists, with no attempt to
/// create one. The fast paths use this so that everything unusual is one
/// tail call away. `Some` also proves the allocator is initialised on this
/// thread: heaps are only created by [`heap_for_thread`] under `shared()`,
/// so the entry points skip the init-state load (an acquire that would
/// otherwise head every dependent-load chain). Same aliasing and lifetime
/// rule as [`heap_for_thread`]: one live reference per thread, never
/// stored.
///
/// `shared` is the process's `Shared` as a raw pointer (`api::shared_ptr`,
/// or a `&Shared`), read only after the slot proved a heap exists: its
/// `push_flags` byte says whether the slot value is signed
/// ([`PUSH_PAC`]), and a thread that created a heap did so under
/// `shared()`, after init wrote that byte.
#[inline(always)]
pub fn existing_heap(shared: *const Shared) -> Option<&'static mut Heap> {
    let (t, tp) = tls_heap_slot()?;
    // SAFETY: a real slot value proves this thread observed `READY`, so
    // `push_flags` was written before.
    let flags = unsafe { fast_flags(shared) };
    // SAFETY: `t` is this thread's slot value above the sentinels.
    Some(unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0) })
}

/// The calling thread's TLS slot value and thread pointer when the slot
/// holds a heap pointer (plain or signed), `None` on null or a sentinel.
/// (Branching once here into two copies of the whole fast path was tried:
/// LLVM merged the copies' tails back together and gave `malloc` a stack
/// frame it did not have, so the entry points take [`existing_heap`] and
/// the `autda`s sit behind their own predicted `tbz`s.)
#[inline(always)]
pub fn tls_heap_slot() -> Option<(usize, usize)> {
    let (t, tp) = crate::tls::get_tp();
    let t = t as usize;
    if t > TLS_EXITED { Some((t, tp)) } else { None }
}

/// `Shared::push_flags` through a raw pointer (`api::shared_ptr`).
///
/// # Safety
/// The caller must have observed `READY` (a heap pointer in its TLS slot
/// is the witness: heaps are created under `shared()`, after init wrote
/// the byte).
#[inline(always)]
pub unsafe fn fast_flags(shared: *const Shared) -> u8 {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::read(&raw const (*shared).push_flags) }
}

/// The heap a TLS slot value names: authenticated against the thread
/// pointer when `pac` ([`PUSH_PAC`]), the value itself otherwise.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`] on this thread, and `pac` is the
/// process's `PUSH_PAC`.
#[inline(always)]
pub unsafe fn heap_from_slot(t: usize, tp: usize, pac: bool) -> &'static mut Heap {
    let h = if pac { pac::auth(t, tp) } else { t };
    // SAFETY: guaranteed by the caller; the hint spares the null test the
    // compiler would otherwise add for the `Option` niche, `t` being above
    // the sentinels and `autda` never producing zero from it.
    unsafe {
        core::hint::assert_unchecked(h != 0);
        &mut *(h as *mut Heap)
    }
}
