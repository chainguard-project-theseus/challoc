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
//! Trusted (every function is `external_body`): the bytes are state no
//! verified invariant constrains, and every check here only adds an abort
//! ([`crate::sys::abort_pending`]: it fires on an invalid free, and on a
//! valid one only if this state is corrupt); none changes what the
//! verified paths do with the bitmap or the caches.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use super::Shared;
use crate::layout::{CLASSES, NCLASSES, Regions};
use vstd::prelude::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

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
static ARRAYS: [Arrays; MAX_SHARED] = [const {
    Arrays {
        class_base: [const { AtomicUsize::new(0) }; NCLASSES],
        base: [const { AtomicUsize::new(0) }; NCLASSES],
    }
}; MAX_SHARED];
/// Entries of [`ARRAYS`] claimed so far.
static CLAIMED: AtomicUsize = AtomicUsize::new(0);
/// The process page, for [`commit`]'s rounding.
static PAGE: AtomicUsize = AtomicUsize::new(0);

/// The byte array of `class` in the `Shared` whose class region starts at
/// `class_base`, zero when that `Shared` has the key off.
fn array_of(class: usize, class_base: usize) -> usize {
    let n = CLAIMED.load(Ordering::Acquire).min(MAX_SHARED);
    for a in &ARRAYS[..n] {
        let base = a.base[class].load(Ordering::Acquire);
        if base != 0 && a.class_base[class].load(Ordering::Relaxed) == class_base {
            return base;
        }
    }
    0
}

verus! {

/// The slot is in its slab's free set (or was never claimed).
pub const FREE: u8 = 0;
/// The slot is in a thread cache (refilled, or freed and not yet popped).
pub const CACHED: u8 = 1;
/// The slot is the program's.
pub const LIVE: u8 = 2;

/// Reserve one `PROT_NONE` byte array per class, `nslabs * slab_bits`
/// bytes, and record the class bases. Called once, by `Shared::init`,
/// when `double_free_scan` is on, before any class table hands out a
/// slab. Aborts when the reservation fails (no address space).
#[verifier::external_body]
#[cold]
#[inline(never)]
pub(super) fn init(regions: &Regions)
    requires
        crate::sys::may_abort_kernel(),
        crate::sys::may_syscall(),
{
    let page = regions.page_size();
    PAGE.store(page, Ordering::Relaxed);
    let i = CLAIMED.fetch_add(1, Ordering::AcqRel);
    if i >= MAX_SHARED {
        crate::sys::abort_kernel(format_args!("double_free_scan: more than {MAX_SHARED} allocator instances"));
    }
    for c in 0..NCLASSES {
        let len = (regions.nslabs(c) * CLASSES.table()[c].slab_bits()).next_multiple_of(page);
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
#[verifier::external_body]
#[cold]
#[inline(never)]
pub(crate) fn commit(class: usize, class_base: usize, s: usize, end: usize)
    requires
        crate::sys::may_abort_kernel(),
        crate::sys::may_syscall(),
{
    let base = array_of(class, class_base);
    if base == 0 {
        return;
    }
    let bits = CLASSES.table()[class].slab_bits();
    let page = PAGE.load(Ordering::Relaxed);
    let lo = (base + s * bits) & !(page - 1);
    let hi = (base + end * bits).next_multiple_of(page);
    // SAFETY: `[lo, hi)` lies in the class's own reservation (`init`);
    // making it writable touches no other mapping.
    if unsafe { crate::sys::mprotect(lo, hi - lo, crate::sys::PROT_READ | crate::sys::PROT_WRITE) }.is_err() {
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
/// slot makes).
#[verifier::external_body]
fn step(shared: &Shared, class: usize, p: *mut u8, from: u8, to: u8, exchange: bool) -> (r: u8)
    ensures
        r == MOVED || r == PAST || r < 3,
{
    let Some(b) = byte(shared, class, p) else { return PAST };
    if exchange {
        match b.compare_exchange(from, to, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => MOVED,
            Err(v) => v,
        }
    } else {
        let v = b.load(Ordering::Relaxed);
        if v == from {
            b.store(to, Ordering::Relaxed);
            MOVED
        } else {
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
/// past the watermark, is an invalid or stale double free.
#[cold]
#[inline(never)]
pub(super) fn free(shared: &Shared, class: usize, p: *mut u8, to: u8)
    requires
        crate::sys::may_abort_pending(),
{
    let r = step(shared, class, p, LIVE, to, true);
    if r == PAST {
        crate::sys::abort_pending(format_args!("invalid free of {:#x}: class {}: its slab was never allocated", p as usize, class))
    } else if r == CACHED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: the slot is already in a thread cache", p as usize))
    } else if r != MOVED {
        crate::sys::abort_pending(format_args!("invalid or double free of {:#x} (class {}): slot is not allocated", p as usize, class))
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
{
    if step(shared, class, p, FREE, to, false) != MOVED {
        crate::sys::abort_pending(format_args!("slot state of {:#x} (class {}) is not free at its claim", p as usize, class))
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
{
    if step(shared, class, p, CACHED, LIVE, false) != MOVED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: a cached copy of a slot that is not cached", p as usize))
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
{
    if step(shared, class, p, CACHED, FREE, false) != MOVED {
        crate::sys::abort_pending(format_args!("double free of {:#x}: a cached copy released while the slot is not cached", p as usize))
    }
}

/// [`claim`] of the first `n` of `ps`, each to `CACHED`: a refill's or a
/// run rebuild's batch (`Heap::claim`).
#[cold]
#[inline(never)]
pub(super) fn claim_all(shared: &Shared, class: usize, ps: &[*mut u8], n: usize)
    requires
        crate::sys::may_abort_pending(),
        n <= ps@.len(),
{
    let mut i = 0;
    while i < n
        invariant
            crate::sys::may_abort_pending(),
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
{
    let mut i = 0;
    while i < ps.len()
        invariant
            crate::sys::may_abort_pending(),
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
{
    // A step from a value no byte holds stores nothing: a read.
    if step(shared, class, p, NONE, NONE, false) == CACHED {
        crate::sys::abort_pending(format_args!("use of {:#x} after free: the slot is in a thread cache", p as usize))
    }
}

} // verus!

/// The state byte of slot start `p` of `class`, after the watermark
/// compare: `None` for a slot of a slab no arena has been handed (whose
/// byte may be uncommitted). Called by the trusted [`step`]
/// only.
fn byte(shared: &Shared, class: usize, p: *mut u8) -> Option<&'static AtomicU8> {
    let cls = &CLASSES.table()[class];
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
    let at = base + slab * cls.slab_bits() + slot;
    // SAFETY: the byte of a slab below the watermark, which `commit` made
    // writable before the watermark was published; an `AtomicU8` has no
    // alignment requirement, and the array lives for the process.
    Some(unsafe { &*(at as *const AtomicU8) })
}
