//! Thread lifecycle (design 6.5): the TLS slot's sentinels, heap creation
//! on first use, the thread-exit destructor and the slot readers the entry
//! points inline.
//!
//! Re-entrant registration: the slot is per-thread ghost state, a
//! [`TlsSlot`] token. The top-level slot users make the token at entry
//! ([`TlsSlot::entry`], which lists them) and the readers run the cores [`heap_for_thread_in`] and
//! [`existing_heap_in`], which state what they do to it. While
//! [`create_heap_for_thread`] is inside `ThreadKey::set`, which may
//! re-enter the allocator, the slot holds [`TLS_CREATING`]
//! ([`reentry_benign`]); every nested entry is a core holding that token,
//! and at that value the cores return `None` and leave the token as they
//! found it: no slot write and no `Heap::create` call.

use core::ffi::c_void;
use core::ptr;

#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
use crate::sys::pac;
use crate::sys::tls::{HeapLoan, TlsCell, TlsSlot};
use crate::vspec::addr::addr_to_ptr;

use super::PUSH_PAC;
use super::paths::shared_of;
#[cfg(verus_keep_ghost)]
use super::tcaches::axiom_heap_use;
use super::tcaches::{heap_at, heap_new};
use super::{Heap, Shared};
use vstd::prelude::Tracked;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
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

/// The phase of a thread's TLS slot value: no heap yet (null), creation
/// in progress ([`TLS_CREATING`]), a heap (any value above the
/// sentinels: the heap pointer, signed under PAC), or the destructor
/// has run ([`TLS_EXITED`]).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost enum SlotPhase {
    /// Null: no heap yet.
    Empty,
    /// [`TLS_CREATING`]: a creation, or the background thread's `create`,
    /// has parked the slot.
    Creating,
    /// A heap pointer (signed under PAC).
    Live,
    /// [`TLS_EXITED`]: the thread-exit destructor has run.
    Exited,
}

/// The [`SlotPhase`] of slot value `v`.
pub open spec fn slot_phase(v: usize) -> SlotPhase {
    if v == 0 {
        SlotPhase::Empty
    } else if v == TLS_CREATING {
        SlotPhase::Creating
    } else if v == TLS_EXITED {
        SlotPhase::Exited
    } else {
        SlotPhase::Live
    }
}

/// Writing `new` to a slot that holds `old` is a step the phases allow,
/// which `sys::tls::set`, the slot's one writer, requires: `Exited` is
/// final; a new heap value is written only over `Creating` (a creation
/// publishing its heap, or the background thread's `create` putting back
/// the heap it parked), so a heap never directly replaces another heap,
/// an empty slot or `Exited`; and an empty slot is written only over an
/// empty slot or `Creating` (a creation whose heap the thread-exit key
/// could not take). Any phase may go to `Exited` (`heap_destructor`).
/// The phases do not say which heap value a write from `Creating` puts
/// in the slot (`create` puts back the value it parked, read off its
/// code).
pub open spec fn slot_step(old: usize, new: usize) -> bool {
    &&& (slot_phase(old) is Exited) ==> (slot_phase(new) is Exited)
    &&& (slot_phase(new) is Live) && new != old ==> (slot_phase(old) is Creating)
    &&& (slot_phase(new) is Empty) ==> (slot_phase(old) is Empty) || (slot_phase(old) is Creating)
}

/// `Exited` is final: the only step from [`TLS_EXITED`] writes it again.
pub proof fn lemma_exited_final(old: usize, new: usize)
    requires
        old == TLS_EXITED,
        slot_step(old, new),
    ensures
        new == TLS_EXITED,
{
}

/// Ghost: the permission to a thread heap, linear: the heap loan of the
/// thread's TLS slot token holds it while the slot holds the heap
/// ([`HeapLoan`](crate::sys::tls::HeapLoan), [`slot_ok`]), and
/// [`slot_heap_ref`] lends the heap out of the loan, for as long as the
/// loan is lent. [`addr`](HeapPerm::addr) is the heap's address and
/// [`value`](HeapPerm::value) the heap, its ghost fields with it: what the
/// heap held when the loan was last lent. Abstract rather than a
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
    /// fields and what the field writers stored in them
    /// ([`FreshHeap`](super::FreshHeap): `init_fields`, `mark_bound` and
    /// `mark_rehome`, verified), used up here, and with its
    /// home headers written
    /// `EMPTY` through `perms` ([`CachePerms::fresh`](super::CachePerms::fresh)),
    /// which the caller installs as the heap's ghost state. The heap's
    /// `others` is null and its `shared`, `arena` and `bound` are what `s`
    /// records; nothing is claimed about its other fields or its ghost
    /// fields. Trusted: `init_fields`, `mark_bound` and `mark_rehome` store
    /// the fields before the home headers through the raw permission `s`
    /// holds, which forgets the values, and the whole-`Heap` permission is
    /// abstract (above), so nothing verified joins the two.
    /// Made once per heap: `s` is linear and made once per heap by
    /// `init_fields`.
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

/// What a thread's slot token holds, as its cell `cell` and heap loan
/// `loan`: above the sentinels, the loan holds the permission to the heap
/// the cell's value names ([`slot_heap`]), which is not null and
/// is well formed (so the allocator is published, [`crate::api::ready`]);
/// at or below them (null, [`TLS_CREATING`], [`TLS_EXITED`]), no heap
/// permission; and the thread pointer is as `get_tp` returns it
/// ([`tp_ok`](crate::sys::tls::tp_ok)). [`create_heap_for_thread`]
/// publishes a heap with its permission in the loan; `TlsSlot::entry`
/// assumes this of the token it makes.
pub open spec fn slot_ok(cell: TlsCell, loan: HeapLoan) -> bool {
    &&& cell.value() > TLS_EXITED ==> (loan.heap() matches Some(hp) && hp.addr() == slot_heap(cell.value(), cell.tp())
        && hp.addr() != 0 && hp.value().wf())
    &&& cell.value() <= TLS_EXITED ==> loan.heap() is None
    &&& crate::sys::tls::tp_ok(cell.tp())
}

/// The heap whose permission the heap loan holds, at `h`, as a
/// reference, for as long as the loan is lent. Trusted: Verus has no
/// raw-pointer dereference, and the permission is abstract
/// ([`HeapPerm`]). The reference is the heap the permission describes,
/// and the permission describes what it holds when the reference ends.
///
/// # Safety
/// `h` points to the heap whose permission the loan `loan` holds (the
/// precondition).
#[verifier::external_body]
#[inline(always)]
#[allow(clippy::needless_lifetimes)]
pub(super) unsafe fn slot_heap_ref<'a>(h: *mut Heap, Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: &'a mut Heap)
    requires
        old(loan).heap() matches Some(hp) && hp.addr() == h@.addr,
    ensures
        *r == old(loan).heap()->Some_0.value(),
        final(loan).heap() matches Some(hp) && hp.addr() == h@.addr && hp.value() == *final(r),
    opens_invariants none
{
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *h }
}

/// Destructor of the thread-exit key: mark the thread as exited, then flush
/// and free its heap. The sentinel goes first so that anything allocating
/// on this thread from here on (a signal handler landing in the middle of
/// `Heap::destroy`, another key's destructor afterwards) is served without
/// a heap instead of reaching the one being freed through `existing_heap`.
/// Every slot write requires [`slot_step`], under which `Exited` is
/// final ([`lemma_exited_final`]), and at `Exited` the slot readers hand
/// out no heap and create none (`theorem_exit_no_rebuild`); that later
/// entries see `Exited` is `TlsSlot::entry`'s slot premise.
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
    if h.is_null() {
        // POSIX runs a key's destructor only for a non-null value.
        return;
    }
    let tracked tls = TlsSlot::entry();
    proof {
        // The heap is about to be freed: its permission leaves the token
        // (`Heap::destroy` gives the heap's block back from the heap's own
        // ghost state, `heap::axiom_heap_block`, not from it).
        let tracked _heap = tls.loan.take();
    }
    crate::sys::tls::set(addr_to_ptr(TLS_EXITED) as *mut c_void, Tracked(&mut tls.cell));
    proof {
        // The slot is `Exited`, with no heap permission in the token:
        // `Heap::destroy` below takes neither part of it.
        assert(tls.value() == TLS_EXITED && tls.loan.heap() is None && slot_ok(tls.cell, tls.loan));
    }
    // Premise (`axiom_heap_use`): a thread's heap reaches its key
    // destructor once, at thread exit, and nothing else uses it then.
    let tracked mut u = axiom_heap_use();
    #[cfg(challoc_tls_check)]
    super::tlscheck::on_exit(h);
    // SAFETY: the pointer was registered by `create_heap_for_thread` for
    // this thread.
    unsafe {
        let shared = shared_of(heap_at(h, Tracked(&mut u)).shared);
        // Premise (lock order): a TLS destructor runs at thread exit, outside
        // any allocator call, so this thread holds no lock.
        Heap::destroy(h, shared, Tracked(u), Tracked(&mut axiom_entry()));
    }
}

/// The calling thread's heap, created on first use. `None` when the thread
/// must be served without a cache: heap creation is in progress on it
/// (`pthread_setspecific` re-entered the allocator), its destructor has run,
/// the metadata region is exhausted, or the thread-exit key cannot take
/// the heap.
///
/// The `&mut Heap` is borrowed from the heap loan of the thread's slot
/// token, which holds the heap's permission ([`HeapPerm`]) and is lent for as long as the
/// reference lives; that it is the only one rests on the slot premise
/// (`TlsSlot::entry`): only the owning thread reaches its heap through
/// TLS, and every entry point takes the reference once and passes it
/// down, never holding two (a `Shared` method called with one live must
/// not call back into `heap_for_thread` or [`existing_heap`]). The
/// reference is not really `'static` either: the thread-exit destructor
/// frees the heap, so it must not be stored beyond the call.
#[inline(always)]
// The heap is per thread, reached through `Shared`; see the doc above.
#[allow(clippy::mut_from_ref)]
pub fn heap_for_thread(shared: &Shared) -> (r: Option<&mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        r matches Some(h) ==> h.wf(),
{
    let tracked tls = TlsSlot::entry();
    heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan))
}

/// [`heap_for_thread`] on the thread's slot cell `cell` and heap loan
/// `loan`. At a sentinel
/// ([`reentry_benign`]: creation in progress, or the destructor has run)
/// `None`, with the cell and loan unchanged: the slot is not written and
/// no heap is created. Above the sentinels the published heap, borrowed
/// from the loan, which is unchanged but for what the heap holds
/// ([`HeapLoan::lent`](crate::sys::tls::HeapLoan::lent)), and the cell
/// unchanged.
/// On an empty slot one `Heap::create` call, and the slot stays empty
/// unless a heap is returned.
#[inline(always)]
pub fn heap_for_thread_in<'a>(shared: &'a Shared, Tracked(cell): Tracked<&mut TlsCell>, Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: Option<&'a mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        slot_ok(*old(cell), *old(loan)),
    ensures
        r matches Some(h) ==> h.wf(),
        reentry_benign(old(cell).value()) ==> r is None && *final(cell) == *old(cell) && *final(loan) == *old(loan),
        old(cell).value() > TLS_EXITED ==> r is Some && *final(cell) == *old(cell) && final(loan).lent(*old(loan)),
        old(cell).value() == 0 ==> final(cell).creates() == old(cell).creates() + 1,
        old(cell).value() == 0 && r is None ==> final(cell).value() == 0,
{
    let (t, tp) = crate::sys::tls::get_tp(Tracked(&*cell));
    let t = t as usize;
    if t > TLS_EXITED {
        // Anything above the sentinels is this thread's heap pointer,
        // signed with the thread pointer when `PUSH_PAC` is set (the
        // sentinels are never signed, so the compare comes first; a signed
        // pointer keeps its low 48 bits and stays above them).
        // SAFETY: `t` is this thread's slot value above the sentinels.
        return Some(unsafe { heap_from_slot(t, tp, shared.pac(), Tracked(&*cell), Tracked(loan)) });
    }
    create_heap_for_thread(shared, t, Tracked(cell), Tracked(loan))
}

/// The slot value for heap `h` on the calling thread: `h` signed with the
/// thread pointer when `shared.pac()`, else `h` itself.
fn tls_value_for(shared: &Shared, h: *mut Heap, Tracked(cell): Tracked<&TlsCell>) -> *mut c_void
    requires
        shared.wf(),
{
    if shared.pac() {
        let (_, tp) = crate::sys::tls::get_tp(Tracked(cell));
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
/// no call that can allocate (one that did would re-enter on an empty
/// slot and create a second heap).
#[cold]
#[inline(never)]
fn create_heap_for_thread<'a>(shared: &'a Shared, slot: usize, Tracked(cell): Tracked<&mut TlsCell>, Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: Option<&'a mut Heap>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        slot == old(cell).value(),
        slot_ok(*old(cell), *old(loan)),
    ensures
        r matches Some(h) ==> h.wf(),
        slot != 0 ==> r is None && *final(cell) == *old(cell) && *final(loan) == *old(loan),
        slot == 0 ==> final(cell).creates() == old(cell).creates() + 1,
        slot == 0 && r is None ==> final(cell).value() == 0,
{
    if slot != 0 {
        return None;
    }
    // Premise (lock order): reached from the first allocation of a thread,
    // on its slow path, with no lock held.
    let tracked mut ord = axiom_entry();
    let (h, Tracked(perms), Tracked(fresh)) = Heap::create(shared, Tracked(&mut ord));
    proof {
        cell.note_create();
    }
    if h.is_null() {
        return None;
    }
    crate::sys::tls::set(addr_to_ptr(TLS_CREATING) as *mut c_void, Tracked(&mut *cell));
    if !shared.key.set(h, Tracked(&mut *cell)) {
        // Thread exit could not free the heap, so it goes now and this
        // call is served without one; the next call tries again, as
        // after a failed `Heap::create`.
        // What creation returned is dropped here, never installed:
        // `Heap::destroy` gets the heap back through `tcaches::heap_at`,
        // whose premise is the ghost state creation left, and frees it.
        proof {
            let tracked _created = (perms, fresh);
        }
        // Premise (`axiom_heap_use`): created above and never published,
        // so nothing else holds a reference to it.
        let tracked u = axiom_heap_use();
        // SAFETY: created above and never published.
        unsafe { Heap::destroy(h, shared, Tracked(u), Tracked(&mut ord)) };
        crate::sys::tls::set(ptr::null_mut(), Tracked(&mut *cell));
        return None;
    }
    // Registered while the slot still holds `TLS_CREATING`, so a signal
    // handler that allocates in between never finds the heap unregistered.
    #[cfg(challoc_tls_check)]
    super::tlscheck::on_publish(h);
    crate::sys::tls::set(tls_value_for(shared, h, Tracked(&*cell)), Tracked(&mut *cell));
    let tracked (homed, stored) = fresh.tracked_unwrap();
    // SAFETY: freshly created and owned by this thread.
    let heap = unsafe { heap_new(h, Tracked(stored), Tracked(&perms), Tracked(loan)) };
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
/// [`shared_ptr`](super::paths::shared_ptr), the static's in the library),
/// read only after the slot proved a heap exists: its `push_flags` byte
/// says whether the slot value is signed ([`PUSH_PAC`]), and a thread
/// that created a heap did so under a `&Shared`, after init wrote that
/// byte.
#[inline(always)]
pub fn existing_heap(shared: *const Shared) -> (r: Option<&'static mut Heap>)
    requires
        crate::sys::may_abort_corrupt(),
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
#[allow(clippy::needless_lifetimes)]
// Verus does not take `?` here: the `let`-`else` is the same branch.
#[allow(clippy::question_mark)]
pub fn existing_heap_in<'a>(shared: *const Shared, Tracked(tls): Tracked<&'a mut TlsSlot>) -> (r: Option<&'a mut Heap>)
    requires
        crate::sys::may_abort_corrupt(),
        slot_ok(old(tls).cell, old(tls).loan),
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
    Some(unsafe { heap_from_slot(t, tp, flags & PUSH_PAC != 0, Tracked(&tls.cell), Tracked(&mut tls.loan)) })
}

/// Thread exit never reuses a destroyed heap: at a token whose
/// slot is [`TLS_EXITED`], as `heap_destructor` leaves it before
/// `Heap::destroy` frees the heap, [`existing_heap_in`] (the fast paths'
/// reader) and [`heap_for_thread_in`] (the slow paths' reader, the one
/// that creates heaps) both return `None` and leave the token as it was:
/// no heap is handed out, the slot is not written, and no `Heap::create`
/// call is made (the cell's count is unchanged). And every value a
/// slot write may store over it, the step `sys::tls::set` requires
/// ([`slot_step`]), is `TLS_EXITED` again ([`lemma_exited_final`]).
/// Verus checks each write and each reader; that every later entry on
/// the thread sees `TLS_EXITED`, so that its allocations and frees are
/// served without the thread's heap, also needs each later entry's token
/// to hold the slot as the last write left it: `TlsSlot::entry`'s slot
/// premise, the link from one call to the next that Verus does not check.
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such function.
#[cfg(verus_keep_ghost)]
fn theorem_exit_no_rebuild(shared: &Shared, sp: *const Shared, Tracked(tls): Tracked<&mut TlsSlot>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        sp == Shared::storage() as *const Shared,
        slot_ok(old(tls).cell, old(tls).loan),
        old(tls).value() == TLS_EXITED,
    ensures
        *final(tls) == *old(tls),
        forall|v: usize| slot_step(old(tls).value(), v) ==> v == TLS_EXITED,
{
    assert forall|v: usize| slot_step(old(tls).value(), v) implies v == TLS_EXITED by {
        lemma_exited_final(old(tls).value(), v);
    }
    let fast = existing_heap_in(sp, Tracked(&mut *tls));
    assert(fast is None);
    let slow = heap_for_thread_in(shared, Tracked(&mut tls.cell), Tracked(&mut tls.loan));
    assert(slow is None);
}

/// The calling thread's TLS slot value and thread pointer when the slot
/// holds a heap pointer (plain or signed), `None` on null or a sentinel.
/// A heap pointer there means the allocator is published
/// ([`crate::api::ready`]): the heap the token holds is well formed
/// ([`slot_ok`]). The entry points take [`existing_heap`] and the
/// `autda`s sit behind their own predicted `tbz`s rather than branching
/// once into two copies of the fast path, which LLVM merges back
/// together at the cost of a stack frame in `malloc`.
#[inline(always)]
pub fn tls_heap_slot(Tracked(tls): Tracked<&TlsSlot>) -> (r: Option<(usize, usize)>)
    ensures
        r is None <==> tls.value() <= TLS_EXITED,
        r matches Some((t, tp)) ==> t == tls.value() && t > TLS_EXITED && (slot_ok(tls.cell, tls.loan) ==> tp == tls.tp()),
        r is Some && slot_ok(tls.cell, tls.loan) ==> crate::api::ready(),
    opens_invariants none
{
    let (t, tp) = crate::sys::tls::get_tp(Tracked(&tls.cell));
    let t = t as usize;
    proof {
        if t > TLS_EXITED && slot_ok(tls.cell, tls.loan) {
            tls.loan.heap()->Some_0.value().lemma_wf_regions();
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
    let s = unsafe { crate::api::shared_deref(shared) };
    s.push_flags()
}

/// The heap a TLS slot value names: authenticated against the thread
/// pointer when `pac` ([`PUSH_PAC`]), the value itself otherwise
/// ([`slot_heap`]), borrowed from the heap loan `loan`, which holds its
/// permission ([`slot_ok`] of it with the slot cell `cell`) and is lent for as long as the reference
/// lives. With `pac`, a corrupt slot value faults in `pac::auth`
/// (FEAT_FPAC), an integrity abort, so the caller holds
/// `sys::may_abort_corrupt`, as [`existing_heap`] and
/// [`heap_for_thread`] do.
///
/// # Safety
/// `(t, tp)` from [`tls_heap_slot`] on this thread, and `pac` is the
/// process's `PUSH_PAC` (the preconditions).
#[inline(always)]
#[allow(clippy::needless_lifetimes)]
pub unsafe fn heap_from_slot<'a>(t: usize, tp: usize, pac: bool, Tracked(cell): Tracked<&TlsCell>, Tracked(loan): Tracked<&'a mut HeapLoan>) -> (r: &'a mut Heap)
    requires
        crate::sys::may_abort_corrupt(),
        pac == super::pac_mode(),
        pac ==> crate::sys::pac::pac_available(),
        slot_ok(*cell, *old(loan)),
        t == cell.value(),
        t > TLS_EXITED,
        tp == cell.tp(),
    ensures
        r.wf(),
        final(loan).lent(*old(loan)),
    opens_invariants none
{
    let h = if pac { pac::auth(t, tp) } else { t };
    #[cfg(challoc_tls_check)]
    super::tlscheck::on_use(h);
    // SAFETY: guaranteed by the caller; the hint spares the null test the
    // compiler would otherwise add for the `Option` niche, `t` being above
    // the sentinels and `autda` never producing zero from it.
    unsafe {
        core::hint::assert_unchecked(h != 0);
        slot_heap_ref(addr_to_ptr(h) as *mut Heap, Tracked(loan))
    }
}

} // verus!
