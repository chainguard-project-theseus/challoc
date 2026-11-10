//! `double_free_scan`'s per-slot state (design 6.3 step 3): one byte per
//! slot, parallel to the class bitmap (byte `slab * slab_bits + slot` of
//! the class's array), [`FREE`], [`CACHED`] or [`LIVE`]. The bitmap's bit
//! says only "not available to a refill" (live or cached); the byte tells
//! the two apart, so every free route checks at the call that the slot it
//! frees is live, whichever cache (this thread's, another thread's, the
//! foreign cache) already holds a copy and whichever route (plain, sized,
//! direct) the earlier free took.
//!
//! The transitions, each on the path that makes it:
//!
//! - a claim from the slab's free set: `FREE -> CACHED` for a refill,
//!   `FREE -> LIVE` for `alloc_direct` ([`claim`]);
//! - a cache pop that hands the slot to the program, `CACHED -> LIVE`
//!   ([`pop`]);
//! - a free, `LIVE -> CACHED` onto a cache, `LIVE -> FREE` straight to the
//!   slab ([`free`]); `CACHED` aborts as a double free, `FREE` as an
//!   invalid or double free;
//! - a flush's release, `CACHED -> FREE` ([`release`]).
//!
//! With the key off nothing here runs and nothing is mapped: [`init`] is
//! not called, no array is recorded, and every caller tests
//! `Shared::double_free_scan` first. With it on, the fast push declines
//! on the `free_flags` bit the free fast path already dispatches on, and
//! the fast pop declines because every cache is in `MODE_STATE`, whose
//! scheduled-pop mask is 0 (`CacheHeader::mask_for`); the pop reads no
//! flag. So every transition is on a slow path.
//!
//! The arrays are reserved `PROT_NONE` at init, one per class, and
//! committed with the slabs: a group's bytes when the group is handed to
//! an arena ([`commit`], before the watermark that publishes the group),
//! so a free that passes the watermark compare reads committed memory and
//! a pointer past the watermark is rejected without a read.
//!
//! The bytes are `AtomicU8`s, `Relaxed`: a byte is a memory location of
//! its own, so a neighbouring slot's store never loses this one's. A free
//! moves `LIVE` on with one compare-and-exchange, so of two frees of one
//! slot on two threads at the same instant exactly one finds it `LIVE`.
//! `tests/loom/src/slotstate.rs` models that race, with the
//! load-then-store form as its control. The other transitions are plain
//! stores by the thread that holds the slot in its cache (or claims it
//! from the slab). A free racing a claim into a cache or a release sees
//! the value before or after, and neither is `LIVE`. A free racing a pop
//! (`CACHED -> LIVE`) or `alloc_direct`'s claim (`FREE -> LIVE`) can see
//! `LIVE` and pass, which is right: the slot is the program's by then.
//!
//! Verified: every index here is in bounds and no arithmetic overflows;
//! the addresses (an array base read from an atomic, plus an offset) and
//! [`commit`]'s page rounding of them wrap, and [`init`] rounds with
//! `meta::align_up` within proved bounds. Every byte's permission lives
//! in one invariant, [`Slots`] (the one `api::init` published, which
//! [`step`] reaches through `api::axiom_published`; that it is the one is
//! that axiom's premise), which [`step`] opens around each of its
//! operations on the byte (`core`'s atomics through `vspec::byte`'s
//! wrappers), so in `vspec::byte`'s sequentially consistent model each
//! step finds the value the last step on that byte left (that `Relaxed`
//! suffices is not proved); the permissions start `FREE`, the arrays'
//! zero fill (premise, `api::axiom_start`'s [`fresh`], handed out once
//! with the other statics' permissions and published by `api::init`).
//! [`free`], [`claim`], [`pop`] and [`release`] return only when the
//! byte held the state their transition starts from, and leave the one
//! it ends in; and
//! [`theorem_double_free_window`] proves, over the code's transitions on
//! one byte, that a free left `CACHED` or `FREE` stays so until a
//! hand-out. Not verified: that a load-then-store step's store follows
//! its load with no other change to the byte (the slot's holder makes
//! it; the free's race is modelled in `tests/loom/src/slotstate.rs`);
//! that a computed address is a committed byte (trusted, [`byte_at`]);
//! and that [`commit`]'s range lies in [`init`]'s reservation (`mprotect`
//! is checked only for its call).
//! Every check here only adds an abort ([`crate::sys::abort_pending`]: it
//! fires on an invalid free, and on a valid one only if this state is
//! corrupt); none changes what the verified paths do with the bitmap or
//! the caches.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use super::Shared;
use crate::layout::{CLASSES, Class, NCLASSES, Regions};
use crate::vspec::byte::BytePerm;
#[cfg(verus_keep_ghost)]
use crate::vspec::byte::byte_id;
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::map::Map;
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked};

verus! {

/// One `Shared`'s arrays: each class's region base, which names the
/// `Shared` (its regions are its own), and the class's byte array.
struct Arrays {
    class_base: [AtomicUsize; NCLASSES],
    base: [AtomicUsize; NCLASSES],
}

/// How many `Shared`s with the key on one process can have: the process's
/// own, and in the unit tests one per test that turns the key on (each
/// test builds a `Shared` over regions of its own).
const MAX_SHARED: usize = 32;

/// The arrays of every `Shared` with the key on, in [`init`] order; an
/// entry is published by its `base` stores (`Release`), the last.
exec static ARRAYS: [Arrays; MAX_SHARED] = NO_ARRAYS;

/// [`ARRAYS`]'s initial value: every entry unclaimed, all zero. Trusted:
/// Verus takes no array repeat of an element that is not `Copy`; it
/// claims nothing about the value.
#[verifier::external_body]
#[allow(clippy::declare_interior_mutable_const, clippy::large_const_arrays)]
const NO_ARRAYS: [Arrays; MAX_SHARED] = [const {
    Arrays {
        class_base: [const { AtomicUsize::new(0) }; NCLASSES],
        base: [const { AtomicUsize::new(0) }; NCLASSES],
    }
}; MAX_SHARED];
/// Entries of [`ARRAYS`] claimed so far.
exec static CLAIMED: AtomicUsize = AtomicUsize::new(0);
/// The process page, for [`commit`]'s rounding.
exec static PAGE: AtomicUsize = AtomicUsize::new(0);

/// The byte array of `class` in the `Shared` whose class region starts at
/// `class_base`, zero when that `Shared` has the key off.
// An index loop: Verus specifies no iterator over a slice of `ARRAYS`.
#[allow(clippy::needless_range_loop)]
fn array_of(class: usize, class_base: usize) -> usize
    requires
        class < NCLASSES,
{
    let n = CLAIMED.load(Ordering::Acquire).min(MAX_SHARED);
    for i in 0..n
        invariant
            n <= MAX_SHARED,
            class < NCLASSES,
    {
        let a = &ARRAYS[i];
        let base = a.base[class].load(Ordering::Acquire);
        if base != 0 && a.class_base[class].load(Ordering::Relaxed) == class_base {
            return base;
        }
    }
    0
}

/// The slot is in its slab's free set (or was never claimed).
pub const FREE: u8 = 0;
/// The slot is in a thread cache (refilled, or freed and not yet popped).
pub const CACHED: u8 = 1;
/// The slot is the program's.
pub const LIVE: u8 = 2;

/// A value a slot's state byte holds: [`FREE`], [`CACHED`] or [`LIVE`].
pub open spec fn is_state(v: u8) -> bool {
    v == FREE || v == CACHED || v == LIVE
}

/// The `(from, to)` of every [`step`] the code makes: a claim (`FREE` to
/// `CACHED` or `LIVE`), a pop (`CACHED` to `LIVE`), a free (`LIVE` to
/// `CACHED` or `FREE`), a release (`CACHED` to `FREE`), and
/// [`check_not_cached`]'s read (`NONE` to `NONE`).
pub open spec fn transition(from: u8, to: u8) -> bool {
    ||| from == FREE && (to == CACHED || to == LIVE)
    ||| from == CACHED && (to == LIVE || to == FREE)
    ||| from == LIVE && (to == CACHED || to == FREE)
    ||| from == NONE && to == NONE
}

/// What a [`step`] from `from` to `to` leaves in a byte that holds `held`.
pub open spec fn after(held: u8, from: u8, to: u8) -> u8 {
    if held == from {
        to
    } else {
        held
    }
}

/// What that [`step`] returns, short of [`PAST`].
pub open spec fn seen(held: u8, from: u8) -> u8 {
    if held == from {
        MOVED
    } else {
        held
    }
}

/// The byte after the steps `ops`, in order, from `s`.
pub open spec fn run(s: u8, ops: Seq<(u8, u8)>) -> u8
    decreases ops.len(),
{
    if ops.len() == 0 {
        s
    } else {
        after(run(s, ops.drop_last()), ops.last().0, ops.last().1)
    }
}

/// One of the steps `ops` from `s` moves the byte to `LIVE`: a pop or
/// `alloc_direct`'s claim hands the slot to the program.
pub open spec fn hands_out(s: u8, ops: Seq<(u8, u8)>) -> bool {
    exists|i: int| 0 <= i < ops.len() && #[trigger] ops[i].1 == LIVE && run(s, ops.take(i)) == ops[i].0
}

/// The double-free window of `double_free_scan`: after a free of a slot
/// returns, leaving its byte at `to` ([`free`] checks it), every sequence
/// of the code's steps on that byte that does not hand the slot out
/// leaves it `CACHED` or `FREE`, so the next free's step returns that
/// value, on which [`free`] aborts. The window ends at the slot's next
/// hand-out (a pop, or `alloc_direct`'s claim): the slot is the
/// program's again, and a free of it passes.
///
/// The steps are those of one slot's byte in the order they take effect.
/// That each finds what the last step on that slot left is [`Slots`]'
/// invariant, which [`step`] opens around each operation on the byte;
/// that the first finds `FREE` is `api::axiom_start`'s premise
/// ([`fresh`]); and
/// that a load-then-store step (a claim, pop or release) takes effect as
/// one operation, no other step changing the byte between its load and
/// its store, is a premise, not verified here (the slot's holder makes
/// it; a racing free's compare-and-exchange moves only a `LIVE` byte).
pub proof fn theorem_double_free_window(to: u8, ops: Seq<(u8, u8)>)
    requires
        to == CACHED || to == FREE,
        forall|i: int| 0 <= i < ops.len() ==> transition(#[trigger] ops[i].0, ops[i].1),
        !hands_out(to, ops),
    ensures
        run(to, ops) == CACHED || run(to, ops) == FREE,
        seen(run(to, ops), LIVE) == run(to, ops),
    decreases ops.len(),
{
    if ops.len() > 0 {
        let init = ops.drop_last();
        assert forall|i: int| 0 <= i < init.len() implies transition(#[trigger] init[i].0, init[i].1) by {
            assert(init[i] == ops[i]);
        }
        assert(!hands_out(to, init)) by {
            if hands_out(to, init) {
                let i = choose|i: int| 0 <= i < init.len() && #[trigger] init[i].1 == LIVE && run(to, init.take(i)) == init[i].0;
                assert(init.take(i) =~= ops.take(i));
                assert(init[i] == ops[i]);
            }
        }
        theorem_double_free_window(to, init);
        let k = ops.len() - 1;
        assert(ops.take(k) =~= init);
        assert(transition(ops[k].0, ops[k].1));
        assert(!(ops[k].1 == LIVE && run(to, ops.take(k)) == ops[k].0));
    }
}

/// The identity of the slot-state byte at address `at`: what
/// [`byte_at`] returns is the byte with it, and [`Slots`] holds the
/// permission to it. Premise, not checked: distinct addresses have
/// distinct identities, and none is the identity of an atomic
/// `vspec::byte::new` made (`byte_at` is given only slot-state bytes).
pub uninterp spec fn slot_byte_id(at: usize) -> int;

/// What [`Slots`]' invariant holds: a permission to the slot-state byte
/// at every address ([`slot_byte_id`]), whose value is a state.
// Ghost: named only by proof code.
#[allow(dead_code)]
struct SlotPred;

impl InvariantPredicate<(), Map<usize, BytePerm>> for SlotPred {
    open spec fn inv(k: (), m: Map<usize, BytePerm>) -> bool {
        forall|a: usize| #![trigger m.dom().contains(a)] #![trigger m[a]] m.dom().contains(a) && m[a].id() == slot_byte_id(a) && is_state(m[a].value())
    }
}

/// The namespace of [`Slots`]' invariant.
spec fn slots_ns() -> int {
    0x736c_6f74
}

/// The slot-state bytes' permissions as the process starts: one to the
/// byte at every address ([`slot_byte_id`]), each `FREE`. What
/// `api::Statics::fresh` requires of the permissions `api::axiom_start`
/// hands out, once, with the other statics' (`axiom_start`'s `ensures`
/// spells this out).
pub open spec fn fresh(m: Map<usize, BytePerm>) -> bool {
    forall|a: usize| #![trigger m.dom().contains(a)] #![trigger m[a]] m.dom().contains(a) && m[a].id() == slot_byte_id(a) && m[a].value() == FREE
}

/// The invariant that holds every slot-state byte's permission
/// ([`SlotPred`]): [`step`] opens it around each of its byte operations
/// and puts the permission back, so the next step on that byte, on any
/// thread, finds the value the last one left. `api::init` builds it
/// ([`adopt`]) from the permissions `api::axiom_start` hands out and
/// publishes it (`api::Published`); its field is private, so no code
/// outside this file opens it.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Slots {
    tracked inv: AtomicInvariant<(), Map<usize, BytePerm>, SlotPred>,
}

/// [`Slots`]' invariant over the process's slot-state bytes, each
/// `FREE` ([`fresh`]): the permissions are used up, so it is built once
/// per set of them, and `api::axiom_start` hands out one.
pub proof fn adopt(tracked m: Map<usize, BytePerm>) -> (tracked r: Slots)
    requires
        fresh(m),
{
    Slots { inv: AtomicInvariant::new((), m, slots_ns()) }
}

/// Reserve one `PROT_NONE` byte array per class, `nslabs * slab_bits`
/// bytes, and record the class bases. Called once, by `Shared::init`,
/// when `double_free_scan` is on, before any class table hands out a
/// slab. Aborts when the reservation fails (no address space).
#[cold]
#[inline(never)]
pub(super) fn init(regions: &Regions, Tracked(classes): Tracked<&'static vstd::raw_ptr::PointsTo<[Class; NCLASSES]>>)
    requires
        crate::sys::may_abort_kernel(),
        crate::sys::may_syscall(),
        regions.wf(),
        regions.separated(),
        crate::layout::frozen_table(*classes),
{
    let page = regions.page_size();
    proof {
        // The page is a power of two of at most 64 KiB.
        let lg = regions.lg_page;
        assert(page & sub(page, 1) == 0 && page <= 0x1_0000) by (bit_vector)
            requires
                page == 1usize << lg,
                12 <= lg <= 16,
        ;
    }
    PAGE.store(page, Ordering::Relaxed);
    let i = CLAIMED.fetch_add(1, Ordering::AcqRel);
    if i >= MAX_SHARED {
        crate::sys::abort_kernel(format_args!("double_free_scan: more than {MAX_SHARED} allocator instances"));
    }
    for c in 0..NCLASSES
        invariant
            crate::sys::may_abort_kernel(),
            crate::sys::may_syscall(),
            regions.wf(),
            crate::layout::frozen_table(*classes),
            i < MAX_SHARED,
            page > 0,
            page & sub(page, 1) == 0,
            page <= 0x1_0000,
    {
        let n = regions.nslabs(c, Tracked(classes));
        let bits = CLASSES.table_with(Tracked(classes))[c].slab_bits();
        proof {
            // The array fits well inside the address space: at most
            // 2^38 slabs (the region) of at most 2^13 bits each.
            crate::vspec::class::lemma_class_table_wf(c as int);
            assert(crate::layout::class_table()[c as int].wf());
            let (lr, ls) = (regions.lg_region, crate::layout::class_table()[c as int].lg_slab);
            let (lru, lsu) = (lr as usize, ls as usize);
            assert(crate::layout::LG_REGION_PREFERRED == 38);
            assert(lru <= 38);
            assert(((1usize << lru) >> lsu) == ((1usize << lr) >> ls)) by (bit_vector)
                requires
                    lru == lr as usize,
                    lsu == ls as usize,
            ;
            assert(((1usize << lru) >> lsu) <= 0x40_0000_0000usize) by (bit_vector)
                requires
                    lru <= 38,
            ;
            assert(n * bits <= 0x40_0000_0000 * 0x2000) by (nonlinear_arith)
                requires
                    n <= 0x40_0000_0000,
                    bits <= 0x2000,
            ;
        }
        let len = crate::meta::align_up(n * bits, page);
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        // SAFETY: a fresh anonymous mapping at an address the kernel picks;
        // no existing memory is touched.
        let Ok(m) = (unsafe { crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0) }) else {
            crate::sys::abort_kernel(format_args!(
                "double_free_scan: cannot reserve the slot-state table of class {c} ({len} bytes)"
            ))
        };
        ARRAYS[i].class_base[c].store(regions.class_base[c], Ordering::Relaxed);
        ARRAYS[i].base[c].store(m, Ordering::Release);
    }
}

/// Commit the state bytes of `class`'s slabs `[s, end)`, a group being
/// handed to an arena (`SlabTable::take_group`, before it publishes the
/// watermark), in the `Shared` whose class region starts at
/// `class_base`. Nothing without the key. Aborts when the commit fails.
#[cold]
#[inline(never)]
pub(crate) fn commit(
    class: usize,
    class_base: usize,
    s: usize,
    end: usize,
    Tracked(classes): Tracked<&'static vstd::raw_ptr::PointsTo<[Class; NCLASSES]>>,
)
    requires
        crate::sys::may_abort_kernel(),
        crate::sys::may_syscall(),
        class < NCLASSES,
        crate::layout::frozen_table(*classes),
{
    let base = array_of(class, class_base);
    if base == 0 {
        return;
    }
    let bits = CLASSES.table_with(Tracked(classes))[class].slab_bits();
    // The page rounding of `[base + s * bits, base + end * bits)`, by
    // mask (the page is a power of two).
    let mask = PAGE.load(Ordering::Relaxed).wrapping_sub(1);
    let lo = base.wrapping_add(s.wrapping_mul(bits)) & !mask;
    let hi = base.wrapping_add(end.wrapping_mul(bits)).wrapping_add(mask) & !mask;
    // SAFETY: `[lo, hi)` lies in the class's own reservation (`init`);
    // making it writable touches no other mapping.
    if unsafe { crate::sys::mprotect(lo, hi.wrapping_sub(lo), crate::sys::PROT_READ | crate::sys::PROT_WRITE) }.is_err() {
        crate::sys::abort_kernel(format_args!("double_free_scan: cannot commit the slot-state table of class {class}"));
    }
}

/// What [`step`] returns besides a state: it moved the byte.
pub const MOVED: u8 = 0xfe;
/// What [`step`] returns besides a state: the slot's slab is past the
/// watermark (its byte may be uncommitted and is not read).
pub const PAST: u8 = 0xff;
/// A value no byte holds: a [`step`] from it only reads.
pub const NONE: u8 = 0xfd;

/// The one transition every state change is: when the byte of slot start
/// `p` of `class` holds `from`, store `to` and return [`MOVED`]; else
/// return what it holds (or [`PAST`]). With `exchange` the test and the
/// store are one compare-and-exchange (a free: two frees of one slot on
/// two threads cannot both find it `LIVE`); without, a load and a store
/// (the claims, pops and releases, which only the thread that holds the
/// slot makes). Short of `PAST`, `obs` is the value the byte held and the
/// one it holds after: the step returns [`seen`] and leaves [`after`] of
/// the first.
fn step(shared: &Shared, class: usize, p: *mut u8, from: u8, to: u8, exchange: bool, Tracked(obs): Tracked<&mut Ghost<(u8, u8)>>) -> (r: u8)
    requires
        class < NCLASSES,
        transition(from, to),
        crate::api::ready(),
    ensures
        r != PAST ==> is_state(final(obs)@.0) && r == seen(final(obs)@.0, from) && final(obs)@.1 == after(final(obs)@.0, from, to),
{
    let Some(b) = byte(shared, class, p) else { return PAST };
    let ghost a = choose|a: usize| byte_id(b) == slot_byte_id(a);
    // The published invariant ([`Slots`]): a ghost read, no code.
    let tracked inv = &crate::api::axiom_published().slots.inv;
    let ghost mut held: u8 = 0;
    if exchange {
        let r;
        vstd::open_atomic_invariant!(inv => m => {
            let tracked mut perm = m.tracked_remove(a);
            proof {
                held = perm.value();
            }
            match crate::vspec::byte::compare_exchange(b, from, to, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut perm)) {
                Ok(_) => {
                    r = MOVED;
                },
                Err(v) => {
                    r = v;
                },
            }
            proof {
                *obs = Ghost((held, perm.value()));
                m.tracked_insert(a, perm);
            }
        });
        r
    } else {
        let v;
        vstd::open_atomic_invariant!(inv => m => {
            let tracked perm = m.tracked_borrow(a);
            proof {
                held = perm.value();
            }
            v = crate::vspec::byte::load(b, Ordering::Relaxed, Tracked(perm));
        });
        if v == from {
            vstd::open_atomic_invariant!(inv => m => {
                let tracked mut perm = m.tracked_remove(a);
                crate::vspec::byte::store(b, to, Ordering::Relaxed, Tracked(&mut perm));
                proof {
                    *obs = Ghost((held, perm.value()));
                    m.tracked_insert(a, perm);
                }
            });
            MOVED
        } else {
            proof {
                *obs = Ghost((held, held));
            }
            v
        }
    }
}

/// A free of slot start `p` of `class`, on every route (the cached
/// free's slow path, which the plain, foreign and sized frees all take
/// with the key on, and the direct free): `LIVE` becomes `to` (`CACHED`
/// onto a cache, `FREE` straight to the slab), with one
/// compare-and-exchange. Aborts on anything else: `CACHED` is a second
/// free of a slot a cache still holds (any thread's); `FREE`, or a slab
/// past the watermark, is an invalid or stale double free. Verified (the
/// assertion at its end): it returns only when the byte held `LIVE` and
/// now holds `to`.
#[cold]
#[inline(never)]
pub(super) fn free(shared: &Shared, class: usize, p: *mut u8, to: u8)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        to == CACHED || to == FREE,
        crate::api::ready(),
{
    let tracked mut obs = Ghost((0u8, 0u8));
    let r = step(shared, class, p, LIVE, to, true, Tracked(&mut obs));
    if r == PAST {
        crate::sys::abort_pending(format_args!("invalid free of {:#x}: class {}: its slab was never allocated", p as usize, class))
    } else if r == CACHED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: the slot is already in a thread cache", p as usize))
    } else if r != MOVED {
        crate::sys::abort_pending(format_args!("invalid or double free of {:#x} (class {}): slot is not allocated", p as usize, class))
    }
    proof {
        assert(obs@.0 == LIVE && obs@.1 == to);
    }
}

/// A claim of slot `p` of `class` from its slab's free set: `FREE`
/// becomes `to` (`CACHED` for a refill, `LIVE` for `alloc_direct`).
/// Anything else means the bitmap handed out a slot this state still
/// holds, which only corruption explains.
#[cold]
#[inline(never)]
pub(super) fn claim(shared: &Shared, class: usize, p: *mut u8, to: u8)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        to == CACHED || to == LIVE,
        crate::api::ready(),
{
    let tracked mut obs = Ghost((0u8, 0u8));
    if step(shared, class, p, FREE, to, false, Tracked(&mut obs)) != MOVED {
        crate::sys::abort_pending(format_args!("slot state of {:#x} (class {}) is not free at its claim", p as usize, class))
    }
    proof {
        assert(obs@.0 == FREE && obs@.1 == to);
    }
}

/// A pop of slot `p` of `class` that hands it to the program: `CACHED`
/// becomes `LIVE`. Anything else is a cached entry whose slot was freed
/// to its slab or handed out since it was cached.
#[cold]
#[inline(never)]
pub(super) fn pop(shared: &Shared, class: usize, p: *mut u8)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        crate::api::ready(),
{
    let tracked mut obs = Ghost((0u8, 0u8));
    if step(shared, class, p, CACHED, LIVE, false, Tracked(&mut obs)) != MOVED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: a cached copy of a slot that is not cached", p as usize))
    }
    proof {
        assert(obs@.0 == CACHED && obs@.1 == LIVE);
    }
}

/// A flush's release of cached slot `p` of `class` to its slab: `CACHED`
/// becomes `FREE`. Anything else is a cached copy of a slot that is live
/// or free, which only corruption explains.
#[cold]
#[inline(never)]
pub(super) fn release(shared: &Shared, class: usize, p: *mut u8)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        crate::api::ready(),
{
    let tracked mut obs = Ghost((0u8, 0u8));
    if step(shared, class, p, CACHED, FREE, false, Tracked(&mut obs)) != MOVED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: a cached copy released while the slot is not cached", p as usize))
    }
    proof {
        assert(obs@.0 == CACHED && obs@.1 == FREE);
    }
}

/// [`claim`] of the first `n` of `ps`, each to `CACHED`: a refill's or a
/// run rebuild's batch (`Heap::claim`).
#[cold]
#[inline(never)]
pub(super) fn claim_all(shared: &Shared, class: usize, ps: &[*mut u8], n: usize)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        n <= ps@.len(),
        crate::api::ready(),
{
    let mut i = 0;
    while i < n
        invariant
            crate::sys::may_abort_pending(),
            class < NCLASSES,
            crate::api::ready(),
            n <= ps@.len(),
            i <= n,
        decreases n - i,
    {
        claim(shared, class, ps[i], CACHED);
        i += 1;
    }
}

/// [`release`] of every slot of `ps`: a flush's released batch.
#[cold]
#[inline(never)]
pub(super) fn release_all(shared: &Shared, class: usize, ps: &[*mut u8])
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        crate::api::ready(),
{
    let mut i = 0;
    while i < ps.len()
        invariant
            crate::sys::may_abort_pending(),
            class < NCLASSES,
            crate::api::ready(),
            i <= ps@.len(),
        decreases ps@.len() - i,
    {
        release(shared, class, ps[i]);
        i += 1;
    }
}

/// Abort when slot `p` of `class` sits in a cache, freed: what
/// `malloc_usable_size`, `realloc` and `sallocx` reject with the key on
/// (their bit test passes for a cached slot).
#[cold]
#[inline(never)]
pub(super) fn check_not_cached(shared: &Shared, class: usize, p: *mut u8)
    requires
        crate::sys::may_abort_pending(),
        class < NCLASSES,
        crate::api::ready(),
{
    let tracked mut obs = Ghost((0u8, 0u8));
    // A step from a value no byte holds stores nothing: a read.
    if step(shared, class, p, NONE, NONE, false, Tracked(&mut obs)) == CACHED {
        crate::sys::abort_pending(format_args!("use of {:#x} after free: the slot is in a thread cache", p as usize))
    }
}

/// The state byte of slot start `p` of `class`, after the watermark
/// compare: `None` for a slot of a slab no arena has been handed (whose
/// byte may be uncommitted). Called by [`step`] only.
fn byte(shared: &Shared, class: usize, p: *mut u8) -> (r: Option<&'static AtomicU8>)
    requires
        class < NCLASSES,
        crate::api::ready(),
    ensures
        r matches Some(b) ==> exists|a: usize| byte_id(b) == slot_byte_id(a),
{
    let cls = &CLASSES.table()[class];
    proof {
        crate::vspec::class::lemma_class_table_wf(class as int);
        assert(cls.wf());
    }
    let class_base = shared.regions.class_base[class];
    let rel = (p as usize).wrapping_sub(class_base);
    let slab = rel >> cls.lg_slab;
    if slab >= shared.tables[class].committed_slabs() {
        return None;
    }
    let base = array_of(class, class_base);
    if base == 0 {
        // Not this `Shared`'s key: nothing is tracked (unreachable, every
        // caller tests the key first).
        return None;
    }
    let slot = (rel & (cls.slab_size() - 1)) / cls.size as usize;
    let at = base.wrapping_add(slab.wrapping_mul(cls.slab_bits())).wrapping_add(slot);
    // SAFETY: the byte of a slab below the watermark, which `commit` made
    // writable before the watermark was published.
    Some(unsafe { byte_at(at) })
}

/// The `AtomicU8` at address `at`.
///
/// # Safety
/// `at` is a byte of a slot-state array that [`commit`] made writable;
/// the arrays live for the process.
///
/// Trusted (raw pointer): Verus has no integer-to-reference cast, and
/// the contract above is the protocol of [`init`], [`commit`] and the
/// watermark, not a verified fact. It claims only the byte's identity:
/// the byte at `at`, whose permission [`Slots`] holds under
/// [`slot_byte_id`]`(at)`; nothing about its value.
#[verifier::external_body]
#[inline(always)]
unsafe fn byte_at(at: usize) -> (r: &'static AtomicU8)
    ensures
        byte_id(r) == slot_byte_id(at),
{
    // SAFETY: the caller's contract; an `AtomicU8` has no alignment
    // requirement.
    unsafe { &*(at as *const AtomicU8) }
}

} // verus!

/// The Kani harness of [`NO_ARRAYS`], next to the private type.
#[cfg(kani)]
mod kani_arrays {
    use super::{MAX_SHARED, NO_ARRAYS};
    use crate::layout::NCLASSES;
    use core::sync::atomic::Ordering::Relaxed;

    /// `NO_ARRAYS`: no `ensures`; the compiler evaluates the constant,
    /// and (beyond the contract, which claims nothing) the harness checks
    /// that every entry is unclaimed, all zero: both words of every class
    /// of every entry load 0.
    #[kani::proof]
    fn no_arrays_are_zero() {
        let a = NO_ARRAYS;
        let (e, c): (usize, usize) = (kani::any(), kani::any());
        kani::assume(e < MAX_SHARED && c < NCLASSES);
        assert_eq!(a[e].class_base[c].load(Relaxed), 0);
        assert_eq!(a[e].base[c].load(Relaxed), 0);
    }
}

/// The Kani harness of [`byte_at`], next to the private function.
#[cfg(kani)]
mod kani_byte_at {
    use super::byte_at;
    use core::sync::atomic::AtomicU8;
    use core::sync::atomic::Ordering::Relaxed;

    /// `byte_at`: its `ensures` names a ghost identity, which Kani does
    /// not see; with `at` a byte of the harness's own
    /// 4-byte array, standing in for the `# Safety` contract's slot-state
    /// array, its address cast to an integer as `byte` computes it, the
    /// call is defined, the reference is that byte (`core::ptr::eq`;
    /// `kani::mem::can_dereference`: non-null, in bounds, a valid
    /// `AtomicU8`), and a store through it is the byte's. Not checked:
    /// the `# Safety` contract itself.
    #[kani::proof]
    fn byte_at_is_the_byte() {
        let bytes: [AtomicU8; 4] = [const { AtomicU8::new(0) }; 4];
        let i: usize = kani::any();
        kani::assume(i < bytes.len());
        // SAFETY: `bytes[i]` is a byte of the harness's array, which
        // outlives every use of the reference.
        let b = unsafe { byte_at(bytes.as_ptr() as usize + i) };
        assert!(core::ptr::eq(b, &bytes[i]));
        assert!(kani::mem::can_dereference(b as *const AtomicU8));
        let v: u8 = kani::any();
        b.store(v, Relaxed);
        assert_eq!(bytes[i].load(Relaxed), v);
    }
}
