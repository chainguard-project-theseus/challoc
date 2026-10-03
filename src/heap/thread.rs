//! Thread lifecycle (design 6.5): the TLS slot's sentinels, heap creation
//! on first use, the thread-exit destructor and the slot readers the entry
//! points inline.
//!
//! Re-entrant registration (`verus/THEOREMS.md`): the slot is per-thread
//! ghost state, a [`TlsSlot`] token. The public readers make the token at
//! entry ([`TlsSlot::entry`]) and run the cores [`heap_for_thread_in`] and
//! [`existing_heap_in`], which state what they do to it. While
//! [`create_heap_for_thread`] is inside `ThreadKey::set`, which may
//! re-enter the allocator, the slot holds [`TLS_CREATING`]
//! ([`reentry_benign`]); every nested entry is a core holding that token,
//! and at that value the cores return `None` and leave the token as they
//! found it: no slot write and no `Heap::create` call.

use core::ffi::c_void;
use core::ptr;

use crate::sys::pac;
use crate::sys::tls::TlsSlot;
use crate::vspec::addr::addr_to_ptr;

use super::PUSH_PAC;
use super::paths::shared_of;
use super::tcaches::{heap_at, heap_new};
use super::{Heap, Shared};
use vstd::prelude::Tracked;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;

verus! {

/// TLS slot value while this thread's heap is being created and
/// `pthread_setspecific` may re-enter the allocator.
pub const TLS_CREATING: usize = 1;
/// TLS slot value after this thread's heap destructor has run.
pub const TLS_EXITED: usize = 2;

/// A slot value at which an allocator entry on the thread is served
/// without a heap and leaves the slot alone: one of the two sentinels.
/// A call that may re-enter the allocator on this thread
/// (`ThreadKey::set`) requires it of the slot.
pub open spec fn reentry_benign(v: usize) -> bool {
    v == TLS_CREATING || v == TLS_EXITED
}

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
    let tracked mut tls = TlsSlot::entry();
    crate::sys::tls::set(addr_to_ptr(TLS_EXITED) as *mut c_void, Tracked(&mut tls));
    // SAFETY: the pointer was registered by `heap_for_thread` for this thread.
    unsafe {
        let shared = shared_of(heap_at(h).shared);
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
pub fn heap_for_thread(shared: &Shared) -> (r: Option<&mut Heap>)
    requires
        shared.wf(),
    ensures
        r matches Some(h) ==> h.wf(),
{
    let tracked mut tls = TlsSlot::entry();
    heap_for_thread_in(shared, Tracked(&mut tls))
}

/// [`heap_for_thread`] on the thread's slot token `tls`. At a sentinel
/// ([`reentry_benign`]: creation in progress, or the destructor has run)
/// `None`, with the token unchanged: the slot is not written and no heap
/// is created. Above the sentinels the published heap, token unchanged.
/// On an empty slot one `Heap::create` call, and the slot stays empty
/// unless a heap is returned.
#[inline(always)]
pub fn heap_for_thread_in<'a>(shared: &'a Shared, Tracked(tls): Tracked<&mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        shared.wf(),
    ensures
        r matches Some(h) ==> h.wf(),
        reentry_benign(old(tls).value()) ==> r is None && *final(tls) == *old(tls),
        old(tls).value() > TLS_EXITED ==> r is Some && *final(tls) == *old(tls),
        old(tls).value() == 0 ==> final(tls).creates() == old(tls).creates() + 1,
        old(tls).value() == 0 && r is None ==> final(tls).value() == 0,
{
    let (t, tp) = crate::sys::tls::get_tp(Tracked(&*tls));
    let t = t as usize;
    if t > TLS_EXITED {
        // Anything above the sentinels is this thread's heap pointer,
        // signed with the thread pointer when `PUSH_PAC` is set (the
        // sentinels are never signed, so the compare comes first; a signed
        // pointer keeps its low 48 bits and stays above them).
        // SAFETY: `t` is this thread's slot value above the sentinels.
        return Some(unsafe { heap_from_slot(t, tp, shared.pac()) });
    }
    create_heap_for_thread(shared, t, Tracked(tls))
}

/// The slot value for heap `h` on the calling thread: `h` signed with the
/// thread pointer when `shared.pac()`, else `h` itself.
fn tls_value_for(shared: &Shared, h: *mut Heap, Tracked(tls): Tracked<&TlsSlot>) -> *mut c_void
    requires
        shared.wf(),
{
    if shared.pac() {
        let (_, tp) = crate::sys::tls::get_tp(Tracked(tls));
        addr_to_ptr(pac::sign(h as usize, tp)) as *mut c_void
    } else {
        h as *mut c_void
    }
}

/// Create, register and publish the calling thread's heap (`slot` is its
/// TLS value, 0 when it has none yet); `None` while a creation is in
/// progress, after the exit destructor ran, or when the metadata region is
/// exhausted. The new heap is well formed ([`Heap::wf`]): its ghost state
/// is the permissions `Heap::create` returns, which own its home headers,
/// all empty ([`CachePerms::fresh`](super::CachePerms::fresh)), an empty
/// `others` list, and the record of where the home headers are
/// (`Heap::home_hdrs`, [`home_hdr`](super::home_hdr) of the new heap).
///
/// The slot is parked on [`TLS_CREATING`] across `ThreadKey::set`, the
/// one call here that may re-enter the allocator, which requires it
/// ([`reentry_benign`]). `Heap::create` runs on the empty slot: it makes
/// no call that can allocate (`verus/THEOREMS.md` states what would
/// follow if it did).
#[cold]
#[inline(never)]
fn create_heap_for_thread<'a>(shared: &'a Shared, slot: usize, Tracked(tls): Tracked<&mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        shared.wf(),
        slot == old(tls).value(),
    ensures
        r matches Some(h) ==> h.wf(),
        slot != 0 ==> r is None && *final(tls) == *old(tls),
        slot == 0 ==> final(tls).creates() == old(tls).creates() + 1,
        slot == 0 && r is None ==> final(tls).value() == 0,
{
    if slot != 0 {
        return None;
    }
    let (h, Tracked(perms)) = Heap::create(shared, Tracked(tls));
    if h.is_null() {
        return None;
    }
    crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(tls));
    shared.key.set(h, Tracked(tls));
    crate::sys::tls::set(tls_value_for(shared, h, Tracked(&*tls)), Tracked(tls));
    // SAFETY: freshly created and owned by this thread.
    let heap = unsafe { heap_new(h) };
    proof {
        shared.lemma_wf();
        heap.perms = Tracked(perms);
        heap.list = Ghost(Seq::empty());
        heap.home_hdrs = Ghost(Seq::new(crate::layout::NCLASSES as nat, |c: int| super::home_hdr(h, c)));
        heap.lemma_fresh_wf(h);
    }
    Some(heap)
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
pub fn existing_heap(shared: *const Shared) -> (r: Option<&'static mut Heap>)
    ensures
        r matches Some(h) ==> h.wf(),
    opens_invariants none
{
    let tracked tls = TlsSlot::entry();
    existing_heap_in(shared, Tracked(&tls))
}

/// [`existing_heap`] on the thread's slot token `tls`, which it only
/// reads: it never writes the slot or creates a heap. `None` exactly when
/// the slot is empty or holds a sentinel.
#[inline(always)]
pub fn existing_heap_in(shared: *const Shared, Tracked(tls): Tracked<&TlsSlot>) -> (r: Option<&'static mut Heap>)
    ensures
        r matches Some(h) ==> h.wf(),
        r is None <==> tls.value() <= TLS_EXITED,
    opens_invariants none
{
    let Some((t, tp)) = tls_heap_slot(Tracked(tls)) else { return None };
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
pub fn tls_heap_slot(Tracked(tls): Tracked<&TlsSlot>) -> (r: Option<(usize, usize)>)
    ensures
        r is None <==> tls.value() <= TLS_EXITED,
        r matches Some((t, _)) ==> t == tls.value() && t > TLS_EXITED,
    opens_invariants none
{
    let (t, tp) = crate::sys::tls::get_tp(Tracked(tls));
    let t = t as usize;
    if t > TLS_EXITED { Some((t, tp)) } else { None }
}

/// `Shared::push_flags` through a raw pointer (`api::shared_ptr`).
///
/// # Safety
/// The caller must have observed `READY` (a heap pointer in its TLS slot
/// is the witness: heaps are created under `shared()`, after init wrote
/// the byte).
///
/// Trusted: a field read through a raw pointer. The pointer is the
/// process's `Shared` (every caller passes `api::shared_ptr` or the
/// published reference), whose `PUSH_PAC` bit is [`super::pac_mode`]. It
/// is the well-formed `Shared` `init` built (`Shared::wf`, the claim
/// `api::shared_ref` makes), so the bit is set only when
/// `sys::pac::pac_available`.
#[inline(always)]
#[verifier::external_body]
pub unsafe fn fast_flags(shared: *const Shared) -> (r: u8)
    ensures
        (r & PUSH_PAC != 0) == super::pac_mode(),
        (r & PUSH_PAC != 0) ==> crate::sys::pac::pac_available(),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::read(&raw const (*shared).push_flags) }
}

/// The heap a TLS slot value names: authenticated against the thread
/// pointer when `pac` ([`PUSH_PAC`]), the value itself otherwise.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`] on this thread, and `pac` is the
/// process's `PUSH_PAC`.
///
/// Trusted: the integer-to-reference cast. A slot value above the
/// sentinels is a heap `create_heap_for_thread` published for this
/// thread, and every heap a verified path is handed is well formed
/// ([`Heap::wf`]).
#[inline(always)]
#[verifier::external_body]
pub unsafe fn heap_from_slot(t: usize, tp: usize, pac: bool) -> (r: &'static mut Heap)
    requires
        pac ==> crate::sys::pac::pac_available(),
    ensures
        r.wf(),
    opens_invariants none
{
    let h = if pac { pac::auth(t, tp) } else { t };
    // SAFETY: guaranteed by the caller; the hint spares the null test the
    // compiler would otherwise add for the `Option` niche, `t` being above
    // the sentinels and `autda` never producing zero from it.
    unsafe {
        core::hint::assert_unchecked(h != 0);
        &mut *(h as *mut Heap)
    }
}

} // verus!
