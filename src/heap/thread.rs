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

#[cfg(verus_only)]
use crate::lock::axiom_entry;
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

/// Ghost: the permission to a thread heap, linear: the thread's TLS slot
/// token holds it while the slot holds the heap
/// ([`TlsSlot`](crate::sys::tls::TlsSlot), [`slot_ok`]), and
/// [`slot_heap_ref`] lends the heap out of the token, for as long as the
/// token is lent. [`addr`](HeapPerm::addr) is the heap's address and
/// [`value`](HeapPerm::value) the heap, its ghost fields with it: what the
/// heap held when the token was last lent. Abstract rather than a
/// `vstd` `PointsTo<Heap>`: a heap's ghost fields own the permissions to
/// its own home headers ([`super::CachePerms`]), so a `PointsTo` to the
/// whole `Heap` would own those bytes a second time, and `ptr_mut_read`
/// with `PointsTo::is_disjoint` would prove `false` from the pair.
/// Made once per thread heap, by [`super::tcaches::heap_new`]
/// ([`HeapPerm::assemble`]), and, since no ghost state survives from one
/// allocator call to the next, minted again with each token
/// `TlsSlot::entry` makes: it is linear within a token, not across
/// tokens. So nothing may state a separation or uniqueness fact of it
/// (two permissions with one `addr()` are not contradictory here): such
/// an axiom would let two top-level users' tokens prove `false`.
#[verifier::external_body]
pub tracked struct HeapPerm {
    _no_copy: (),
}

impl HeapPerm {
    /// The heap's address.
    pub uninterp spec fn addr(&self) -> usize;

    /// The heap.
    pub uninterp spec fn value(&self) -> Heap;

    /// The permission to the heap [`Heap::create`] has just made at
    /// `s.heap()`, from the token holding the permission to its plain
    /// fields and what the trusted writers stored in them
    /// ([`FreshHeap`](super::FreshHeap): `init_fields`, `mark_bound`,
    /// `mark_rehome`), used up here, and with its home headers written
    /// `EMPTY` through `perms` ([`CachePerms::fresh`](super::CachePerms::fresh)),
    /// which the caller installs as the heap's ghost state. The heap's
    /// `others` is null and its `arena` and `bound` are what `s` records;
    /// nothing is claimed about its other fields or its ghost fields.
    /// Trusted: the field stores before the home headers are `&raw mut`
    /// projections Verus does not support, so no verified permission
    /// covers the whole `Heap`; this is the claim `tcaches::heap_new`'s
    /// body made before it was verified over it. Made once per heap: `s`
    /// is linear and made once per heap by `init_fields`.
    pub(super) axiom fn assemble(tracked s: super::FreshHeap, tracked perms: &super::CachePerms) -> (tracked r: HeapPerm)
        requires
            super::heap_head(s.heap(), s.head()),
            perms.fresh(s.heap()),
        ensures
            r.addr() == s.heap()@.addr,
            r.value().others@.addr == 0,
            r.value().arena as int == s.arena(),
            r.value().bound == s.bound(),
            r.value().shared == s.shared(),
    ;
}

/// The heap a slot value `v` names, for thread pointer `tp`: `v`
/// authenticated with `tp` when heap pointers are signed
/// ([`super::pac_mode`]), `v` itself otherwise ([`heap_from_slot`]).
pub open spec fn slot_heap(v: usize, tp: usize) -> usize {
    if super::pac_mode() {
        crate::sys::pac::spec_auth(v, tp)
    } else {
        v
    }
}

/// What a thread's slot token holds: above the sentinels, the permission
/// to the heap the value names ([`slot_heap`]), which is not null and
/// is well formed (so the allocator is published, [`crate::api::ready`]);
/// at or below them (null, [`TLS_CREATING`], [`TLS_EXITED`]), no heap
/// permission; and the thread pointer is as `get_tp` returns it
/// ([`tp_ok`](crate::sys::tls::tp_ok)). [`create_heap_for_thread`]
/// publishes a heap with its permission in the token; `TlsSlot::entry`
/// assumes this of the token it makes.
pub open spec fn slot_ok(s: TlsSlot) -> bool {
    &&& s.value() > TLS_EXITED ==> (s.heap() matches Some(hp) && hp.addr() == slot_heap(s.value(), s.tp())
        && hp.addr() != 0 && hp.value().wf())
    &&& s.value() <= TLS_EXITED ==> s.heap() is None
    &&& crate::sys::tls::tp_ok(s.tp())
}

/// The heap whose permission the slot token holds, at `h`, as a
/// reference, for as long as the token is lent. Trusted: Verus has no
/// raw-pointer dereference, and the permission is abstract
/// ([`HeapPerm`]). The reference is the heap the permission describes,
/// and the permission describes what it holds when the reference ends.
///
/// # Safety
/// `h` points to the heap whose permission `tls` holds (the
/// precondition).
#[verifier::external_body]
#[inline(always)]
pub(super) unsafe fn slot_heap_ref<'a>(h: *mut Heap, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: &'a mut Heap)
    requires
        old(tls).heap() matches Some(hp) && hp.addr() == h@.addr,
    ensures
        *r == old(tls).heap()->Some_0.value(),
        final(tls).value() == old(tls).value(),
        final(tls).creates() == old(tls).creates(),
        final(tls).tp() == old(tls).tp(),
        final(tls).heap() matches Some(hp) && hp.addr() == h@.addr && hp.value() == *final(r),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *h }
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
    proof {
        crate::sys::grant_syscalls();
        crate::sys::grant_aborts();
    }
    let h = p as *mut Heap;
    let tracked tls = TlsSlot::entry();
    proof {
        // The heap is about to be freed: its permission leaves the token
        // (`Heap::destroy`, trusted, does not take it).
        let tracked _heap = tls.take_heap();
    }
    crate::sys::tls::set(addr_to_ptr(TLS_EXITED) as *mut c_void, Tracked(tls));
    // SAFETY: the pointer was registered by `heap_for_thread` for this thread.
    unsafe {
        let shared = shared_of(heap_at(h).shared);
        // Premise (lock order): a TLS destructor runs at thread exit, outside
        // any allocator call, so this thread holds no lock.
        Heap::destroy(h, shared, Tracked(&mut axiom_entry()));
    }
}

/// The calling thread's heap, created on first use. `None` when the thread
/// must be served without a cache: heap creation is in progress on it
/// (`pthread_setspecific` re-entered the allocator), its destructor has run,
/// the metadata region is exhausted, or the thread-exit key cannot take
/// the heap.
///
/// The `&mut Heap` is borrowed from the thread's slot token, which holds
/// the heap's permission ([`HeapPerm`]) and is lent for as long as the
/// reference lives; that it is the only one rests on the slot premise
/// (`TlsSlot::entry`): only the owning thread reaches its heap through
/// TLS, and every entry point takes the reference once and passes it
/// down, never holding two (a `Shared` method called with one live must
/// not call back into `heap_for_thread` or [`existing_heap`]). The
/// reference is not really `'static` either: the thread-exit destructor
/// frees the heap, so it must not be stored beyond the call.
#[inline(always)]
pub fn heap_for_thread(shared: &Shared) -> (r: Option<&mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        r matches Some(h) ==> h.wf(),
{
    let tracked tls = TlsSlot::entry();
    heap_for_thread_in(shared, Tracked(tls))
}

/// [`heap_for_thread`] on the thread's slot token `tls`. At a sentinel
/// ([`reentry_benign`]: creation in progress, or the destructor has run)
/// `None`, with the token unchanged: the slot is not written and no heap
/// is created. Above the sentinels the published heap, borrowed from the
/// token, which is unchanged but for what the heap holds
/// ([`TlsSlot::lent`](crate::sys::tls::TlsSlot::lent)).
/// On an empty slot one `Heap::create` call, and the slot stays empty
/// unless a heap is returned.
#[inline(always)]
pub fn heap_for_thread_in<'a>(shared: &'a Shared, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        slot_ok(*old(tls)),
    ensures
        r matches Some(h) ==> h.wf(),
        reentry_benign(old(tls).value()) ==> r is None && *final(tls) == *old(tls),
        old(tls).value() > TLS_EXITED ==> r is Some && final(tls).lent(*old(tls)),
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
        return Some(unsafe { heap_from_slot(t, tp, shared.pac(), Tracked(tls)) });
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
/// progress, after the exit destructor ran, when the metadata region is
/// exhausted, or when the thread-exit key cannot take the heap (which is
/// then destroyed and the slot emptied again). The new heap is well
/// formed ([`Heap::wf`]): its ghost state is the permissions
/// `Heap::create` returns, which own its home headers, all empty
/// ([`CachePerms::fresh`](super::CachePerms::fresh)), an empty
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
fn create_heap_for_thread<'a>(shared: &'a Shared, slot: usize, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        slot == old(tls).value(),
        slot_ok(*old(tls)),
    ensures
        r matches Some(h) ==> h.wf(),
        slot != 0 ==> r is None && *final(tls) == *old(tls),
        slot == 0 ==> final(tls).creates() == old(tls).creates() + 1,
        slot == 0 && r is None ==> final(tls).value() == 0,
{
    if slot != 0 {
        return None;
    }
    // Premise (lock order): reached from the first allocation of a thread,
    // on its slow path, with no lock held.
    let tracked mut ord = axiom_entry();
    let (h, Tracked(perms), Tracked(fresh)) = Heap::create(shared, Tracked(tls), Tracked(&mut ord));
    if h.is_null() {
        return None;
    }
    crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(tls));
    if !shared.key.set(h, Tracked(tls)) {
        // Thread exit could not free the heap, so it goes now and this
        // call is served without one; the next call tries again, as
        // after a failed `Heap::create`.
        // SAFETY: created above and never published.
        unsafe { Heap::destroy(h, shared, Tracked(&mut ord)) };
        crate::sys::tls::set(ptr::null_mut(), Tracked(tls));
        return None;
    }
    crate::sys::tls::set(tls_value_for(shared, h, Tracked(&*tls)), Tracked(tls));
    let tracked (homed, stored) = fresh.tracked_unwrap();
    // SAFETY: freshly created and owned by this thread.
    let heap = unsafe { heap_new(h, Tracked(stored), Tracked(&perms), Tracked(tls)) };
    proof {
        shared.lemma_wf();
        shared.lemma_wf_ready();
        heap.perms = Tracked(perms);
        heap.homed = Tracked(Some(homed));
        heap.list = Ghost(Seq::empty());
        heap.keys = Ghost(Seq::empty());
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
/// `shared` is the process's `Shared` as a raw pointer, at its storage
/// (the precondition: `api::shared_ptr`, `api::shared_base`, or
/// [`shared_ptr`](super::paths::shared_ptr) of a well-formed `&Shared`),
/// read only after the slot proved a heap exists: its `push_flags` byte
/// says whether the slot value is signed ([`PUSH_PAC`]), and a thread
/// that created a heap did so under a `&Shared`, after init wrote that
/// byte.
#[inline(always)]
pub fn existing_heap(shared: *const Shared) -> (r: Option<&'static mut Heap>)
    requires
        shared == Shared::storage() as *const Shared,
    ensures
        r matches Some(h) ==> h.wf(),
        r is Some ==> crate::api::ready(),
    opens_invariants none
{
    let tracked tls = TlsSlot::entry();
    existing_heap_in(shared, Tracked(tls))
}

/// [`existing_heap`] on the thread's slot token `tls`: it never writes
/// the slot or creates a heap. `None` exactly when the slot is empty or
/// holds a sentinel, with the token unchanged; otherwise the heap,
/// borrowed from the token, which is unchanged but for what the heap
/// holds ([`TlsSlot::lent`](crate::sys::tls::TlsSlot::lent)).
#[inline(always)]
pub fn existing_heap_in<'a>(shared: *const Shared, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        slot_ok(*old(tls)),
        shared == Shared::storage() as *const Shared,
    ensures
        r matches Some(h) ==> h.wf(),
        r is None <==> old(tls).value() <= TLS_EXITED,
        r is None ==> *final(tls) == *old(tls),
        r is Some ==> final(tls).lent(*old(tls)),
        r is Some ==> crate::api::ready(),
    opens_invariants none
{
    let Some((t, tp)) = tls_heap_slot(Tracked(&*tls)) else { return None };
    // SAFETY: a real slot value proves this thread observed `READY`, so
    // `push_flags` was written before.
    let flags = unsafe { fast_flags(shared) };
    // SAFETY: `t` is this thread's slot value above the sentinels.
    Some(unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0, Tracked(tls)) })
}

/// The calling thread's TLS slot value and thread pointer when the slot
/// holds a heap pointer (plain or signed), `None` on null or a sentinel.
/// A heap pointer there means the allocator is published
/// ([`crate::api::ready`]): the heap the token holds is well formed
/// ([`slot_ok`]). (Branching once here into two copies of the whole fast
/// path was tried: LLVM merged the copies' tails back together and gave
/// `malloc` a stack frame it did not have, so the entry points take
/// [`existing_heap`] and the `autda`s sit behind their own predicted
/// `tbz`s.)
#[inline(always)]
pub fn tls_heap_slot(Tracked(tls): Tracked<&TlsSlot>) -> (r: Option<(usize, usize)>)
    ensures
        r is None <==> tls.value() <= TLS_EXITED,
        r matches Some((t, tp)) ==> t == tls.value() && t > TLS_EXITED && (slot_ok(*tls) ==> tp == tls.tp()),
        r is Some && slot_ok(*tls) ==> crate::api::ready(),
    opens_invariants none
{
    let (t, tp) = crate::sys::tls::get_tp(Tracked(tls));
    let t = t as usize;
    proof {
        if t > TLS_EXITED && slot_ok(*tls) {
            tls.heap()->Some_0.value().lemma_wf_regions();
        }
    }
    if t > TLS_EXITED { Some((t, tp)) } else { None }
}

/// `Shared::push_flags` through a raw pointer (`api::shared_ptr`), through
/// the permission frozen at `READY`, as `api::shared_deref` reads it.
///
/// # Safety
/// The caller must have observed `READY` ([`crate::api::ready`], the
/// precondition; a heap pointer in its TLS slot is the witness: heaps are
/// created under a `&Shared`, after init wrote the byte), and `shared` is
/// the process's `Shared`, at its storage.
#[inline(always)]
pub unsafe fn fast_flags(shared: *const Shared) -> (r: u8)
    requires
        crate::api::ready(),
        shared == Shared::storage() as *const Shared,
    ensures
        (r & PUSH_PAC != 0) == super::pac_mode(),
        (r & PUSH_PAC != 0) ==> crate::sys::pac::pac_available(),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { crate::api::shared_deref(shared) }.push_flags()
}

/// The heap a TLS slot value names: authenticated against the thread
/// pointer when `pac` ([`PUSH_PAC`]), the value itself otherwise
/// ([`slot_heap`]), borrowed from the slot token `tls`, which holds its
/// permission ([`slot_ok`]) and is lent for as long as the reference
/// lives.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`] on this thread, and `pac` is the
/// process's `PUSH_PAC` (the preconditions).
#[inline(always)]
pub unsafe fn heap_from_slot<'a>(t: usize, tp: usize, pac: bool, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: &'a mut Heap)
    requires
        pac == super::pac_mode(),
        pac ==> crate::sys::pac::pac_available(),
        slot_ok(*old(tls)),
        t == old(tls).value(),
        t > TLS_EXITED,
        tp == old(tls).tp(),
    ensures
        r.wf(),
        final(tls).lent(*old(tls)),
    opens_invariants none
{
    let h = if pac { pac::auth(t, tp) } else { t };
    // SAFETY: guaranteed by the caller; the hint spares the null test the
    // compiler would otherwise add for the `Option` niche, `t` being above
    // the sentinels and `autda` never producing zero from it.
    unsafe {
        core::hint::assert_unchecked(h != 0);
        slot_heap_ref(addr_to_ptr(h) as *mut Heap, Tracked(tls))
    }
}

} // verus!
