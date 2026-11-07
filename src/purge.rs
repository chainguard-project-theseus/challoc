//! Decay and purge (design 8) and the background thread (8.1).
//!
//! - [`Batch`], the discard primitive. Callers collect up to [`MAX_BATCH`]
//!   ranges, merging adjacent ones, and flush them with a loop of
//!   `madvise`. The advice is decided at each flush ([`advice`], design 8
//!   and 11): `MADV_FREE` under the cgroup policy's headroom band
//!   ([`set_headroom`]), `MADV_DONTNEED` everywhere else and always for an
//!   explicit purge ([`purge_all`], [`purge_arena`]). The flush reports
//!   each range to the caller as discarded or not, and the caller changes
//!   state only for the discarded ones: a range `madvise` refuses (`EINVAL`
//!   on `mlock`ed pages) stays `Empty`, dirty and never assumed zero, and
//!   is counted in [`failed_discards`] instead of [`discarded_bytes`]. The
//!   next explicit purge retries it; a decay walk that hit one backs its
//!   owner off for [`DISCARD_RETRY_NS`] ([`Decayer::back_off`]). Any
//!   other errno (`ENOMEM`: a reservation of ours is gone; `EPERM`: a
//!   sealed range) is a heap inconsistency and aborts.
//! - The decay curve ([`Curve`], `purge/curve.rs`) and its driver
//!   ([`decay`]): jemalloc's decay shape, a smootherstep from one to zero
//!   over the `dirty_decay_ms` window. Each `(arena, class)` and the huge
//!   allocator purge their oldest `Empty` slabs (extents) until what is left
//!   is within the curve's allowance, at most once per step; [`decay`]
//!   visits them all and reports the earliest next deadline. Each class
//!   keeps the `dirty_at` of its oldest `Empty` slab and its next deadline
//!   in atomics, so the visit takes no lock unless a walk is due.
//!   [`on_tick`] is the slow path's hook (design 6.4).
//! - The background thread ([`background`], `purge/background.rs`): created
//!   on the first periodic tick after it was requested, or by default on
//!   the first one that finds enough dirty memory waiting (the lazy start,
//!   [`background::auto_tick`]), never from init, on
//!   a stack from the metadata region with a guard page below it and every
//!   signal blocked. It loops over `decay` and sleeps on a futex until the
//!   next deadline; a class whose first slab goes `Empty` wakes it. While it
//!   exists the slow path does no purging of its own. `pthread_create`
//!   allocates, so the creating thread parks its TLS slot on the
//!   `TLS_CREATING` sentinel and that allocation is served straight from the
//!   slabs (design 6.5).
//!
//! `MADV_FREE`d memory keeps its contents until the kernel reclaims it, so
//! [`purged_is_zero`] is true until the first lazy batch of the process and
//! false from then on: a refill no longer reports a `Purged` slab's batch
//! zero after that. That report is bookkeeping (`check_zero` reads it); no
//! zeroing entry point trusts it for a slab slot, and a huge run reusing a
//! `Purged` extent is discarded again before it is reported zero
//! (`Huge::rediscard`).
//!
//! The curve's tests sit in `purge/curve.rs`, the other pure tests and the
//! `serial` lock the class tests share in `purge/tests.rs`, and the ones that
//! need a kernel in `purge/linux_tests.rs`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arena::Arena;
use crate::heap::Shared;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
pub use crate::policy::Advice;
use crate::sys;
use vstd::prelude::*;

mod curve;
#[cfg(test)]
mod linux_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use curve::{Curve, NEVER, STEPS, decay_ns_from_ms, ms_of};

verus! {

/// Ranges per batch (design 8).
pub const MAX_BATCH: usize = 64;

/// How long a decay walk whose discard failed holds its owner's next walk
/// off ([`Decayer::back_off`]): the memory is most likely locked, and a
/// zero or short period would otherwise retry it at every tick.
pub const DISCARD_RETRY_NS: u64 = 1_000_000_000;

/// `lazy_band` (default false): `MADV_FREE` under the headroom band, an
/// opt-in (docs/tuning.md, section 1).
exec static LAZY_BAND: AtomicBool = AtomicBool::new(false);
/// The policy's last evaluation put usage under 70% of a known limit with
/// no eager return of the excess over the cap in progress
/// (`policy::evaluate` mirrors `Policy::headroom` here).
exec static HEADROOM: AtomicBool = AtomicBool::new(false);
/// The policy's last evaluation found an eager return of the excess over
/// the cap in progress (`Policy::eager_excess`; the reason line only).
exec static EAGER_EXCESS: AtomicBool = AtomicBool::new(false);
/// Explicit purges in flight ([`purge_all`], [`purge_arena`]): eager.
exec static EXPLICIT: AtomicU32 = AtomicU32::new(0);
/// A lazy batch has been issued in this process: `Purged` memory is no
/// longer known to be zero (the module doc). Sticky.
exec static LAZY_EVER: AtomicBool = AtomicBool::new(false);
/// Batches that discarded anything, and bytes discarded, with each advice
/// (the report, `stats.cgroup.*`).
exec static LAZY_BATCHES: AtomicU64 = AtomicU64::new(0);
exec static EAGER_BATCHES: AtomicU64 = AtomicU64::new(0);
exec static LAZY_BYTES: AtomicU64 = AtomicU64::new(0);
exec static EAGER_BYTES: AtomicU64 = AtomicU64::new(0);
/// Pieces and bytes whose discard failed (the report).
exec static FAILED_PIECES: AtomicU64 = AtomicU64::new(0);
exec static FAILED_BYTES: AtomicU64 = AtomicU64::new(0);
/// `check_zero`: the debug mode that reads every slot or page handed out
/// as "known zero" and aborts on a dirty word.
exec static CHECK_ZERO: AtomicBool = AtomicBool::new(false);

/// The `lazy_band` key of the two-advice purge. Called once at init,
/// before anything can be purged, and by the tests.
pub fn configure_advice(lazy_band: bool) {
    LAZY_BAND.store(lazy_band, Ordering::Relaxed);
}

/// The policy's mirror: `low`, usage is under 70% of a known limit
/// (design 11's `Low` band) and no eager return of the excess over the cap
/// is in progress (`Policy::headroom`); `eager_excess`, such a return is
/// in progress (the reason line). Written where the policy is evaluated,
/// read at each flush.
pub fn set_headroom(low: bool, eager_excess: bool) {
    HEADROOM.store(low, Ordering::Relaxed);
    EAGER_EXCESS.store(eager_excess, Ordering::Relaxed);
}

/// The advice the next batch flush would issue (the module doc).
#[inline]
pub fn advice() -> Advice {
    crate::policy::advice(
        HEADROOM.load(Ordering::Relaxed),
        LAZY_BAND.load(Ordering::Relaxed),
        EXPLICIT.load(Ordering::Relaxed) != 0,
    )
}

/// Why [`advice`] is what it is, for the report:
/// `under 70% of the limit`, `explicit purge`, `lazy_band:false` (under
/// 70% of the limit with the lazy band off, the default),
/// `returning the excess over the cap` (an eager return in progress,
/// `Policy::eager_excess`), or `no headroom known` (no limit, or 70% and
/// above).
#[cold]
pub fn advice_reason() -> &'static str {
    if EXPLICIT.load(Ordering::Relaxed) != 0 {
        "explicit purge"
    } else if HEADROOM.load(Ordering::Relaxed) {
        if LAZY_BAND.load(Ordering::Relaxed) { "under 70% of the limit" } else { "lazy_band:false" }
    } else if EAGER_EXCESS.load(Ordering::Relaxed) {
        "returning the excess over the cap"
    } else {
        "no headroom known"
    }
}

/// `(lazy batches, eager batches)` that discarded anything so far.
pub fn batches() -> (u64, u64) {
    (
        LAZY_BATCHES.load(Ordering::Relaxed),
        EAGER_BATCHES.load(Ordering::Relaxed),
    )
}

/// `(lazy bytes, eager bytes)` discarded so far.
pub fn discarded_bytes() -> (u64, u64) {
    (
        LAZY_BYTES.load(Ordering::Relaxed),
        EAGER_BYTES.load(Ordering::Relaxed),
    )
}

/// `(pieces, bytes)` whose discard failed so far: the ranges as pushed (a
/// page group, an extent) that stayed dirty (the module doc), the middle
/// pieces of a refused merged range left without a call included
/// ([`Batch`]). A piece retried and refused again counts again.
pub fn failed_discards() -> (u64, u64) {
    (
        FAILED_PIECES.load(Ordering::Relaxed),
        FAILED_BYTES.load(Ordering::Relaxed),
    )
}

/// An explicit purge in progress: every flush until the guard drops is
/// eager (the caller asked for RSS to drop), whatever the band.
struct Explicit;

impl Explicit {
    fn enter() -> Explicit {
        EXPLICIT.fetch_add(1, Ordering::Relaxed);
        Explicit
    }
}

// Trusted: Verus wants a destructor that opens no invariant and cannot
// unwind; the body is one atomic subtract.
impl Drop for Explicit {
    #[verifier::external_body]
    fn drop(&mut self)
        opens_invariants none
        no_unwind
    {
        EXPLICIT.fetch_sub(1, Ordering::Relaxed);
    }
}

} // verus!

verus! {

/// Turn the `check_zero` debug mode on or off (init, from the config key;
/// the tests, directly).
pub fn set_check_zero(on: bool) {
    CHECK_ZERO.store(on, Ordering::Relaxed);
}

/// Whether the `check_zero` debug mode is on: `ClassState::claim` and
/// `Huge::alloc` then read the first and last word of every slot or page
/// they report as zero and abort on a non-zero one. Off by default; the
/// callers test it only on the paths that already hold a class or huge
/// lock, so the release fast paths never see it.
///
/// Its `true` grants those reads the capability to read user memory
/// (`vspec::user::may_read_user`). The only other verified code that reads
/// a user block is `vspec::user::copy_user`, realloc's copy of a moved
/// block, whose cores grant the capability themselves.
#[inline]
pub fn check_zero() -> (r: bool)
    ensures
        r ==> crate::vspec::user::may_read_user(),
{
    proof {
        crate::vspec::user::grant_user_reads();
    }
    CHECK_ZERO.load(Ordering::Relaxed)
}

/// The advice in force for diagnostics, e.g. `MADV_DONTNEED`; it reads
/// `MADV_DONTNEED (some lazy batches issued)` once any lazy batch went
/// out, since that is what makes `Purged` memory unknown-zero.
#[cold]
pub fn advice_name() -> &'static str {
    match (advice(), purged_is_zero()) {
        (Advice::Eager, false) => "MADV_DONTNEED (some lazy batches issued)",
        (a, _) => a.madv_name(),
    }
}

/// Whether memory in the `Purged` state is known to read as zero: until
/// the first `MADV_FREE` batch of the process (the module doc), which
/// leaves the old contents in place until the kernel gets around to
/// reclaiming the pages; false from then on, for good.
#[inline]
pub fn purged_is_zero() -> bool {
    !LAZY_EVER.load(Ordering::Relaxed)
}

/// Ghost: the record of one read of [`purged_is_zero`] that returned
/// true, which the claim that hands out a `Purged` slab's slots as zero
/// uses up (`class::axiom_discarded_slots_zero`). Not `Copy`, and its
/// field is private to this module: verified code gets one only from
/// [`purged_zero`].
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct PurgedZero {
    ghost _read: (),
}

/// [`purged_is_zero`], with the witness of a true read (`Some` exactly
/// when it returns true). The witness records only that this read
/// returned true; what that means for a `Purged` slab's memory is
/// `class::axiom_discarded_slots_zero`'s premise.
///
/// Read only in `class::ClassState::take`, in the lock hold that claims
/// the slots the witness is for. Checked by
/// `crate::tests::trusted_mint_sites_are_pinned`, which pins its call sites to
/// the one named here: a new one is named here, with its premise, and
/// added to that test's list in the same change.
#[inline]
pub fn purged_zero() -> (r: (bool, Tracked<Option<PurgedZero>>))
    ensures
        r.0 <==> r.1@ is Some,
{
    let zero = purged_is_zero();
    let tracked w = if zero { Some(PurgedZero { _read: () }) } else { None };
    (zero, Tracked(w))
}

/// Ghost: a set of bytes by address, such as those a [`Batch`] may
/// discard ([`Batch::new`]).
pub type Bytes = spec_fn(int) -> bool;

/// Ghost: a set of `(addr, len)` pieces, such as those a flush has yet
/// to settle ([`Settle::settle`]).
pub type Pieces = spec_fn((int, int)) -> bool;

/// Up to [`MAX_BATCH`] page ranges to discard together.
///
/// Invariant the callers uphold (the reason [`Batch::push`] and
/// [`Batch::flush`] are safe functions over an `madvise` that drops page
/// contents): every range pushed is a page-aligned part of one of this
/// process's own reservations whose pages are *free* (an `Empty` slab, a
/// freed huge run: reachable through no live pointer) and stay so until
/// the flush returns. The callers (`ClassState::purge_empty` and
/// `purge_curve`, `Huge::free_run` through [`discard_one`], `purge_empty`
/// and `purge_curve`) push under the class or huge lock that keeps the
/// ranges free and flush before releasing it; the batch never outlives
/// that hold.
///
/// The memory stays in its free state (`Empty`) until the flush reports
/// it discarded: `push` and `flush` take the caller's settler
/// ([`Settle::settle`]`(addr, len, discarded)`), which the flush (a full
/// batch flushes inside `push`) calls for every range it issued, and the
/// caller makes the memory `Purged` only when `discarded` is true. Only
/// ranges pushed with the same length merge (a walk's page groups); a
/// merged range the kernel refuses is issued again piece by piece, as
/// pushed, and each piece is reported on its own, so a locked page keeps
/// only its own piece dirty. A piece `madvise` refused part of is
/// reported not discarded, and the bytes a refused call did discard are
/// not counted.
///
/// The re-issue starts with the first and the last piece; when both are
/// refused too, the middle ones are reported refused without a call.
/// Under `mlockall` every range is refused, and a merged range of `n`
/// pieces then costs 3 calls instead of `n + 1` (64 MiB of 4 KiB groups
/// would be ~16k `EINVAL`s per walk, under the class lock). The price is
/// a range locked at both ends and not in the middle: its unlocked middle
/// stays dirty, which is always safe (only not returned yet), until an
/// end is unlocked or reused, or a later batch merges the run another
/// way. One locked page in an otherwise unlocked run, the common case,
/// still keeps only its own piece dirty.
///
/// The verified half of that invariant: the maker of a batch fixes `free`,
/// the bytes it may discard ([`Batch::new`]), and [`Batch::push`]
/// takes only ranges of such bytes, so every byte a flush hands to
/// `madvise` is one ([`discard`]). What a maker's `free` means (pages no
/// live huge run holds, slabs a class records as idle) is the maker's to
/// prove. Every range pushed is also [`well_formed`]
/// ([`theorem_purge_well_formed`]): non-empty, page-aligned, and in the
/// huge region or a class region outside its guard slabs, and apart
/// from every range the batch holds; [`Batch::wf`] keeps that of the
/// ranges held, of each piece of a merged one, and of those handed to
/// [`discard`] ([`Batch::discarded`]). Every piece pushed is settled once,
/// by the flush that issues it or reports it refused ([`Settle`]: a settler's invariant is stated over the
/// pieces pushed and not yet settled, [`Batch::pieces`]).
pub struct Batch {
    /// `(addr, len, piece)`: `len` is a multiple of `piece`, the length
    /// of each range merged into it.
    ranges: [(usize, usize, usize); MAX_BATCH],
    n: usize,
    // The ghost fields below are zero-sized: they change neither the
    // offsets above nor the machine code.
    /// Ghost: the bytes the batch may discard.
    #[allow(dead_code)]
    free: Ghost<Bytes>,
    /// Ghost: the bytes `madvise` took and the flushes reported
    /// discarded so far ([`Batch::sent`]).
    #[allow(dead_code)]
    sent: Ghost<int>,
    /// Ghost: the ranges handed to [`discard`] so far, in call order.
    #[allow(dead_code)]
    gone: Ghost<Seq<(usize, usize)>>,
}

/// The process's page size (`Regions::lg_page`).
pub open spec fn page_bytes() -> int {
    (1usize << crate::layout::process_regions().lg_page) as int
}

/// Byte `b` lies in the process's huge region.
pub open spec fn huge_byte(b: int) -> bool {
    let r = crate::layout::process_regions();
    r.huge_base <= b < r.huge_base + r.huge_len
}

/// Byte `b` lies in one of the slabs of class `c`'s region of the
/// process, and that slab is not a guard slab
/// ([`guard_slab`](crate::layout::guard_slab)).
pub open spec fn class_byte(c: int, b: int) -> bool {
    let r = crate::layout::process_regions();
    let sz = (1usize << crate::layout::class_table()[c].lg_slab) as int;
    &&& r.class_base[c] <= b < r.class_base[c] + r.spec_nslabs(c) * sz
    &&& !crate::layout::guard_slab(c, (b - r.class_base[c]) / sz)
}

/// Byte `b` is one a purge may hand to `madvise`: in the huge region, or
/// in a slab of a class region that is not a guard slab. Never metadata
/// ([`theorem_purge_well_formed`]).
pub open spec fn discardable(b: int) -> bool {
    ||| huge_byte(b)
    ||| exists|c: int| 0 <= c < crate::layout::NCLASSES && #[trigger] class_byte(c, b)
}

/// `[addr, addr + len)` is a range a purge may discard: non-empty, both
/// ends on a page boundary, and every byte [`discardable`]. Opaque, so a
/// batch's invariant carries it per range as one fact
/// ([`lemma_well_formed`] unfolds it).
#[verifier::opaque]
pub open spec fn well_formed(addr: int, len: int) -> bool {
    &&& page_bytes() > 0
    &&& len > 0
    &&& addr % page_bytes() == 0
    &&& len % page_bytes() == 0
    &&& forall|b: int| addr <= b < addr + len ==> #[trigger] discardable(b)
}

/// [`well_formed`] unfolded.
proof fn lemma_well_formed(addr: int, len: int)
    ensures
        well_formed(addr, len) == {
            &&& page_bytes() > 0
            &&& len > 0
            &&& addr % page_bytes() == 0
            &&& len % page_bytes() == 0
            &&& forall|b: int| addr <= b < addr + len ==> #[trigger] discardable(b)
        },
{
    reveal(well_formed);
}

/// Two adjacent well-formed ranges make one ([`Batch::push`]'s merge).
proof fn lemma_well_formed_join(a: int, l1: int, l2: int)
    requires
        well_formed(a, l1),
        well_formed(a + l1, l2),
    ensures
        well_formed(a, l1 + l2),
{
    reveal(well_formed);
    let p = page_bytes();
    vstd::arithmetic::div_mod::lemma_add_mod_noop(l1, l2, p);
    vstd::arithmetic::div_mod::lemma_small_mod(0, p as nat);
    assert forall|b: int| a <= b < a + (l1 + l2) implies #[trigger] discardable(b) by {
        if b < a + l1 {
            assert(a <= b < a + l1);
        } else {
            assert(a + l1 <= b < a + l1 + l2);
        }
    }
}

/// Slab `s` of class `c`'s region lies inside stride `c` of the class
/// superblock, which lies inside the superblock.
proof fn lemma_slab_in_stride(c: int, s: int)
    requires
        crate::heap::regions_ok(),
        0 <= c < crate::layout::NCLASSES,
        0 <= s < crate::layout::process_regions().spec_nslabs(c),
    ensures
        ({
            let r = crate::layout::process_regions();
            let stride = (1usize << r.lg_stride) as int;
            let (lo, sz) = r.slab_span(c, s);
            &&& sz > 0
            &&& r.superblock + c * stride <= lo
            &&& lo + sz <= r.superblock + (c + 1) * stride
            &&& 0 <= c * stride
            &&& (c + 1) * stride <= crate::layout::NCLASSES * stride
        }),
{
    let r = crate::layout::process_regions();
    let stride = (1usize << r.lg_stride) as int;
    let (lo, sz) = r.slab_span(c, s);
    crate::vspec::class::lemma_class_table_wf(c);
    let lg = crate::layout::class_table()[c].lg_slab;
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
    vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
    let lr = r.lg_region;
    let ru = 1usize << lr;
    let nslabs = ru >> lg;
    let lgu = lg as usize;
    assert(nslabs == ru >> lgu) by (bit_vector)
        requires nslabs == ru >> lg, lgu == lg as usize;
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(nslabs, lgu, ru);
    assert(s * sz + sz <= nslabs * sz && 0 <= s * sz) by (nonlinear_arith)
        requires 0 <= s < nslabs, sz >= 0;
    assert(0 <= c * stride && (c + 1) * stride <= crate::layout::NCLASSES * stride) by (nonlinear_arith)
        requires 0 <= c < crate::layout::NCLASSES, stride >= 0;
}

/// Purge batches are well formed: a range [`discard`] is handed ([`well_formed`],
/// which [`Batch::wf`] keeps of every range a batch holds or handed over)
/// is non-empty and starts and ends on a page boundary, and each of its
/// bytes `b` lies in the huge region or in a class region ([`discardable`]),
/// never in the metadata region, and in no guard slab `s` of any class
/// `c`.
pub proof fn theorem_purge_well_formed(addr: int, len: int, b: int, c: int, s: int)
    requires
        crate::heap::regions_ok(),
        well_formed(addr, len),
        addr <= b < addr + len,
    ensures
        len > 0,
        page_bytes() > 0,
        addr % page_bytes() == 0,
        (addr + len) % page_bytes() == 0,
        discardable(b),
        !crate::layout::process_regions().in_meta(b, 1),
        0 <= c < crate::layout::NCLASSES && 0 <= s < crate::layout::process_regions().spec_nslabs(c)
            && crate::layout::guard_slab(c, s) ==> !(crate::layout::process_regions().slab_span(c, s).0 <= b
            < crate::layout::process_regions().slab_span(c, s).0 + crate::layout::process_regions().slab_span(c, s).1),
{
    reveal(well_formed);
    let r = crate::layout::process_regions();
    let p = page_bytes();
    vstd::arithmetic::div_mod::lemma_add_mod_noop(addr, len, p);
    vstd::arithmetic::div_mod::lemma_small_mod(0, p as nat);
    assert(discardable(b));
    let stride = (1usize << r.lg_stride) as int;
    let guard = 0 <= c < crate::layout::NCLASSES && 0 <= s < r.spec_nslabs(c) && crate::layout::guard_slab(c, s);
    if guard {
        lemma_slab_in_stride(c, s);
    }
    if !huge_byte(b) {
        let c2 = choose|c2: int| 0 <= c2 < crate::layout::NCLASSES && #[trigger] class_byte(c2, b);
        let sz = (1usize << crate::layout::class_table()[c2].lg_slab) as int;
        let base = r.class_base[c2] as int;
        crate::vspec::class::lemma_class_table_wf(c2);
        let lg = crate::layout::class_table()[c2].lg_slab;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        let s2 = (b - base) / sz;
        assert(0 <= s2 < r.spec_nslabs(c2)) by {
            lemma_unit_of(base, sz, 0, r.spec_nslabs(c2), b);
        }
        lemma_slab_in_stride(c2, s2);
        assert(base + s2 * sz <= b < base + s2 * sz + sz) by {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b - base, sz);
            vstd::arithmetic::div_mod::lemma_mod_bound(b - base, sz);
            assert(sz * s2 == s2 * sz) by (nonlinear_arith);
        }
        if guard {
            let (lo, ssz) = r.slab_span(c, s);
            if c2 == c && lo <= b < lo + ssz {
                assert((s + 1) * sz == s * sz + sz) by (nonlinear_arith);
                lemma_unit_of(base, sz, s, 1, b);
            } else if c2 < c {
                assert((c2 + 1) * stride <= c * stride) by (nonlinear_arith)
                    requires c2 + 1 <= c, stride >= 0;
            } else if c < c2 {
                assert((c + 1) * stride <= c2 * stride) by (nonlinear_arith)
                    requires c + 1 <= c2, stride >= 0;
            }
        }
    }
}

/// Every byte of `[addr, addr + len)` is one `free` admits.
pub open spec fn frees(free: spec_fn(int) -> bool, addr: int, len: int) -> bool {
    forall|b: int| addr <= b < addr + len ==> #[trigger] free(b)
}

/// Total length of the first `k` of `ranges`.
pub open spec fn lens(ranges: Seq<(usize, usize, usize)>, k: int) -> int
    decreases k,
{
    if k <= 0 { 0 } else { lens(ranges, k - 1) + ranges[k - 1].1 }
}

/// `lens` of a prefix depends only on that prefix, and never falls as
/// the prefix grows.
proof fn lemma_lens(a: Seq<(usize, usize, usize)>, b: Seq<(usize, usize, usize)>, k: int)
    requires
        0 <= k <= a.len(),
        k <= b.len(),
        forall|i: int| 0 <= i < k ==> a[i] == b[i],
    ensures
        lens(a, k) == lens(b, k),
        0 <= lens(a, k),
        forall|j: int| 0 <= j <= k ==> lens(a, j) <= lens(a, k),
    decreases k,
{
    if k > 0 {
        lemma_lens(a, b, k - 1);
    }
}

/// Ranges `0..k` of `r` pairwise share no byte.
pub open spec fn disjoint_upto(r: Seq<(usize, usize, usize)>, k: int) -> bool {
    forall|i: int, j: int|
        #![trigger r[i], r[j]]
        0 <= i < k && 0 <= j < k && i != j ==> r[i].0 + r[i].1 <= r[j].0 || r[j].0 + r[j].1 <= r[i].0
}

/// The index of a range of `0..k` of `r` that ends last.
spec fn last_end(r: Seq<(usize, usize, usize)>, k: int) -> int
    decreases k,
{
    if k <= 1 {
        0
    } else {
        let j = last_end(r, k - 1);
        if r[k - 1].0 + r[k - 1].1 > r[j].0 + r[j].1 {
            k - 1
        } else {
            j
        }
    }
}

proof fn lemma_last_end(r: Seq<(usize, usize, usize)>, k: int)
    requires
        1 <= k <= r.len(),
    ensures
        0 <= last_end(r, k) < k,
        forall|i: int| 0 <= i < k ==> (#[trigger] r[i]).0 + r[i].1 <= r[last_end(r, k)].0 + r[last_end(r, k)].1,
    decreases k,
{
    if k > 1 {
        lemma_last_end(r, k - 1);
    }
}

/// Removing range `j` takes its length off `lens`.
proof fn lemma_lens_remove(r: Seq<(usize, usize, usize)>, j: int)
    requires
        0 <= j < r.len(),
    ensures
        lens(r.remove(j), r.len() - 1) + r[j].1 == lens(r, r.len() as int),
    decreases r.len(),
{
    let n = r.len() as int;
    let s = r.remove(j);
    if j == n - 1 {
        lemma_lens(s, r, n - 1);
    } else {
        let p = r.subrange(0, n - 1);
        lemma_lens_remove(p, j);
        assert(p.remove(j) =~= s.subrange(0, n - 2));
        lemma_lens(p.remove(j), s, n - 2);
        lemma_lens(p, r, n - 1);
        assert(s[n - 2] == r[n - 1]);
    }
}

/// Pairwise disjoint non-empty ranges that end at or below `m` sum to at
/// most `m`.
proof fn lemma_disjoint_total(r: Seq<(usize, usize, usize)>, m: int)
    requires
        m >= 0,
        disjoint_upto(r, r.len() as int),
        forall|i: int| 0 <= i < r.len() ==> #[trigger] r[i].1 > 0 && r[i].0 + r[i].1 <= m,
    ensures
        lens(r, r.len() as int) <= m,
    decreases r.len(),
{
    let n = r.len() as int;
    if n == 0 {
        assert(lens(r, 0) == 0);
        return;
    }
    lemma_last_end(r, n);
    let j = last_end(r, n);
    let s = r.remove(j);
    lemma_lens_remove(r, j);
    // Every other range ends at or below the start of range `j`.
    assert forall|i: int| 0 <= i < s.len() implies #[trigger] s[i].1 > 0 && s[i].0 + s[i].1 <= r[j].0 by {
        let k = if i < j { i } else { i + 1 };
        assert(s[i] == r[k]);
        assert(r[k].1 > 0);
        assert(r[k].0 + r[k].1 <= r[j].0 || r[j].0 + r[j].1 <= r[k].0);
    }
    assert forall|a: int, b: int|
        #![trigger s[a], s[b]]
        0 <= a < s.len() && 0 <= b < s.len() && a != b implies s[a].0 + s[a].1 <= s[b].0 || s[b].0 + s[b].1 <= s[a].0 by {
        let ka = if a < j { a } else { a + 1 };
        let kb = if b < j { b } else { b + 1 };
        assert(s[a] == r[ka] && s[b] == r[kb]);
        assert(r[ka].0 + r[ka].1 <= r[kb].0 || r[kb].0 + r[kb].1 <= r[ka].0);
    }
    lemma_disjoint_total(s, r[j].0 as int);
}

/// Piece `x` (start, length) is one of the pieces `p` bytes long that
/// `[a, a + l)` was merged from: what [`Batch::push`] merges and a
/// refused flush re-issues one by one.
pub open spec fn in_run(a: int, l: int, p: int, x: (int, int)) -> bool {
    &&& x.1 == p
    &&& a <= x.0 < a + l
    &&& (x.0 - a) % p == 0
}

/// `x` is a piece of one of ranges `lo..hi` of `r` (`(addr, len,
/// piece)`).
pub open spec fn pieces_of(r: Seq<(usize, usize, usize)>, lo: int, hi: int, x: (int, int)) -> bool {
    exists|i: int| lo <= i < hi && #[trigger] in_run(r[i].0 as int, r[i].1 as int, r[i].2 as int, x)
}

/// The pieces of `pend` but those of `[a, a + l)` cut every `p` bytes:
/// what a settled range leaves.
pub open spec fn minus(pend: spec_fn((int, int)) -> bool, a: int, l: int, p: int) -> spec_fn((int, int)) -> bool {
    |x: (int, int)| pend(x) && !in_run(a, l, p, x)
}

/// The pieces of `f` and of `g`.
pub open spec fn either(f: spec_fn((int, int)) -> bool, g: spec_fn((int, int)) -> bool) -> spec_fn((int, int)) -> bool {
    |x: (int, int)| f(x) || g(x)
}

/// The one piece `y`.
pub open spec fn one(y: (int, int)) -> spec_fn((int, int)) -> bool {
    |x: (int, int)| x == y
}

/// No piece.
pub open spec fn none() -> spec_fn((int, int)) -> bool {
    |x: (int, int)| false
}

/// The pieces of ranges `i..n` of `r` and of `keep`: what a flush has
/// left to settle before range `i`.
pub open spec fn from(r: Seq<(usize, usize, usize)>, i: int, n: int, keep: spec_fn((int, int)) -> bool) -> spec_fn((int, int)) -> bool {
    |x: (int, int)| pieces_of(r, i, n, x) || keep(x)
}

/// The pieces of `[at, e)` cut every `p` bytes and of `rest`: what a
/// flush re-issuing a refused range piece by piece has left at `at`.
pub open spec fn at_on(at: int, e: int, p: int, rest: spec_fn((int, int)) -> bool) -> spec_fn((int, int)) -> bool {
    |x: (int, int)| in_run(at, e - at, p, x) || rest(x)
}

/// Every piece of `[a, a + l)` cut every `p` bytes is [`well_formed`].
pub open spec fn runs_wf(a: int, l: int, p: int) -> bool {
    forall|x: (int, int)| #[trigger] in_run(a, l, p, x) ==> well_formed(x.0, x.1)
}

/// A range of one piece has that piece only.
pub proof fn lemma_run_one(a: int, l: int, x: (int, int))
    requires
        l > 0,
    ensures
        in_run(a, l, l, x) <==> x == (a, l),
{
    if a <= x.0 < a + l {
        vstd::arithmetic::div_mod::lemma_small_mod((x.0 - a) as nat, l as nat);
    }
    vstd::arithmetic::div_mod::lemma_small_mod(0, l as nat);
}

/// The pieces of `[a, a + l)` but the first are those of `[a + p, a +
/// l)`.
proof fn lemma_run_step(a: int, l: int, p: int, x: (int, int))
    requires
        p > 0,
    ensures
        (in_run(a, l, p, x) && x != (a, p)) <==> in_run(a + p, l - p, p, x),
{
    let d = x.0 - a;
    vstd::arithmetic::div_mod::lemma_mod_sub_multiples_vanish(d, p);
    assert(d - p == -p + d);
    if in_run(a, l, p, x) && x != (a, p) {
        if d < p {
            vstd::arithmetic::div_mod::lemma_small_mod(d as nat, p as nat);
        }
    }
}

/// The pieces of `[a, a + l + p)` are those of `[a, a + l)` and the one
/// at `a + l`.
proof fn lemma_run_merge(a: int, l: int, p: int, x: (int, int))
    requires
        p > 0,
        l >= 0,
        l % p == 0,
    ensures
        in_run(a, l + p, p, x) <==> in_run(a, l, p, x) || x == (a + l, p),
{
    let d = x.0 - a;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, p);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(l, p);
    if in_run(a, l + p, p, x) && !in_run(a, l, p, x) {
        let (q1, q2) = (d / p, l / p);
        assert(d == p * q1 && l == p * q2);
        assert(l <= d < l + p);
        assert(q1 == q2) by (nonlinear_arith)
            requires d == p * q1, l == p * q2, l <= d < l + p, p > 0;
    }
}

/// A piece of `[a, a + l)` ends inside it.
proof fn lemma_run_inside(a: int, l: int, p: int, x: (int, int))
    requires
        p > 0,
        l % p == 0,
        in_run(a, l, p, x),
    ensures
        a <= x.0,
        x.0 + p <= a + l,
{
    let d = x.0 - a;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, p);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(l, p);
    let (q1, q2) = (d / p, l / p);
    assert(d == p * q1 && l == p * q2);
    assert(q1 < q2) by (nonlinear_arith)
        requires d == p * q1, l == p * q2, d < l, p > 0;
    assert(p * (q1 + 1) <= p * q2) by (nonlinear_arith)
        requires q1 + 1 <= q2, p > 0;
    assert(p * (q1 + 1) == p * q1 + p) by (nonlinear_arith);
}

/// Every byte of `[a, a + l)` lies in one of its pieces.
proof fn lemma_byte_piece(a: int, l: int, p: int, b: int) -> (x: (int, int))
    requires
        p > 0,
        a <= b < a + l,
    ensures
        in_run(a, l, p, x),
        x.0 <= b < x.0 + p,
{
    let d = b - a;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, p);
    vstd::arithmetic::div_mod::lemma_mod_bound(d, p);
    let q = d / p;
    let x = (a + p * q, p);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(q, p);
    assert(p * q == q * p) by (nonlinear_arith);
    assert(0 <= p * q) by (nonlinear_arith)
        requires q >= 0, p > 0;
    x
}

/// How a batch's ranges are settled ([`Batch::flush`]): the caller's
/// record of the memory a batch discards, whose state changes only once a
/// flush reports a range discarded ([`Batch`]). Owned by the caller
/// (`Self`, its tally), it works on the caller's state `c` (the lock
/// value), with its owner `o` (the class or the huge allocator) for
/// context. Its invariant is over the pieces pushed and not yet settled
/// (`pend`, [`Batch::pieces`]): each flush settles each piece once, alone
/// or as part of its merged range.
pub trait Settle {
    type Owner;

    type Ctx;

    /// The settler's configuration, which settling does not change.
    type Conf;

    /// The settler's invariant over its owner, the caller's state and the
    /// pieces pushed and not yet settled.
    spec fn inv(&self, o: &Self::Owner, c: Self::Ctx, pend: spec_fn((int, int)) -> bool) -> bool;

    /// The bytes reported discarded so far.
    spec fn bytes(&self) -> int;

    /// The settler's configuration: fixed by its maker, kept by every
    /// settle.
    spec fn conf(&self) -> Self::Conf;

    /// The flush's report on `[addr, addr + len)`, made of the pieces
    /// `piece` bytes long pushed and not yet settled: `done`, `madvise`
    /// took the range ([`discard_taken`]); otherwise it refused the range
    /// or one holding it ([`discard_refused`]).
    #[allow(clippy::too_many_arguments)]
    fn settle(
        &mut self,
        o: &Self::Owner,
        c: &mut Self::Ctx,
        addr: usize,
        len: usize,
        done: bool,
        piece: Ghost<int>,
        pend: Ghost<Pieces>,
    )
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(self).inv(o, *old(c), pend@),
            0 < piece@ <= len,
            len as int % piece@ == 0,
            addr + len <= usize::MAX,
            forall|x: (int, int)| #[trigger] in_run(addr as int, len as int, piece@, x) ==> (pend@)(x),
            done ==> discard_taken(addr as int, len as int),
            !done ==> discard_refused(addr as int, len as int),
        ensures
            final(self).inv(o, *final(c), minus(pend@, addr as int, len as int, piece@)),
            final(self).conf() == old(self).conf(),
            final(self).bytes() == old(self).bytes() + if done {
                len as int
            } else {
                0
            },
    ;
}

/// [`discard_one`]'s settler: whether the one range was discarded.
struct One {
    discarded: bool,
    /// Ghost: the bytes reported discarded.
    #[allow(dead_code)]
    bytes: Ghost<int>,
    /// Ghost: the one range pushed, `(addr, len)`.
    #[allow(dead_code)]
    at: Ghost<(int, int)>,
}

impl Settle for One {
    type Owner = ();

    type Ctx = ();

    /// The range pushed.
    type Conf = (int, int);

    /// The range is pending and alone, or settled, and then `discarded`
    /// is the kernel's answer on it.
    closed spec fn inv(&self, o: &(), c: (), pend: spec_fn((int, int)) -> bool) -> bool {
        ||| pend(self.at@) && forall|x: (int, int)| #[trigger] pend(x) ==> x == self.at@
        ||| (forall|x: (int, int)| !#[trigger] pend(x)) && (self.discarded ==> discard_taken(self.at@.0, self.at@.1))
            && (!self.discarded ==> discard_refused(self.at@.0, self.at@.1))
    }

    closed spec fn bytes(&self) -> int {
        self.bytes@
    }

    closed spec fn conf(&self) -> (int, int) {
        self.at@
    }

    // Only `done` is kept; `len` is read by ghost code alone.
    #[inline(always)]
    fn settle(
        &mut self,
        _o: &(),
        _c: &mut (),
        _addr: usize,
        #[allow(unused_variables)] len: usize,
        done: bool,
        _piece: Ghost<int>,
        _pend: Ghost<Pieces>,
    ) {
        proof {
            // The pending range is the one pushed: its first piece is
            // it, and it has no second.
            let (a, p) = (_addr as int, _piece@);
            vstd::arithmetic::div_mod::lemma_small_mod(0, p as nat);
            assert(in_run(a, len as int, p, (a, p)));
            if p < len {
                vstd::arithmetic::div_mod::lemma_mod_self_0(p);
                assert(in_run(a, len as int, p, (a + p, p)));
            }
            assert forall|x: (int, int)| !#[trigger] (minus(_pend@, a, len as int, p))(x) by {
                if (_pend@)(x) {
                    assert(x == self.at@);
                }
            }
        }
        self.discarded = done;
        self.bytes = Ghost(self.bytes@ + if done {
            len as int
        } else {
            0
        });
    }
}

/// Every byte of units (slabs, pages) `first..first + k` of a range at
/// `base` of units of `sz` bytes lies in one of them: its index
/// `(b - base) / sz`.
pub(crate) proof fn lemma_unit_of(base: int, sz: int, first: int, k: int, b: int)
    requires
        sz > 0,
        0 <= first,
        base + first * sz <= b < base + (first + k) * sz,
    ensures
        first <= (b - base) / sz < first + k,
{
    let d = b - base;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, sz);
    let q = d / sz;
    let r = d % sz;
    assert(0 <= r < sz) by {
        vstd::arithmetic::div_mod::lemma_mod_bound(d, sz);
    }
    assert(first <= q) by {
        if q < first {
            assert(q * sz + sz <= first * sz) by (nonlinear_arith)
                requires q + 1 <= first, sz > 0;
            assert(sz * q == q * sz) by (nonlinear_arith);
        }
    }
    assert(q < first + k) by {
        if q >= first + k {
            assert((first + k) * sz <= q * sz) by (nonlinear_arith)
                requires first + k <= q, sz > 0;
            assert(sz * q == q * sz) by (nonlinear_arith);
        }
    }
}

/// What [`Batch::wf`] keeps of each range held, over `free`.
pub open spec fn range_ok(r: (usize, usize, usize), free: spec_fn(int) -> bool) -> bool {
    &&& r.0 + r.1 <= usize::MAX
    &&& 0 < r.2 <= r.1
    &&& r.1 % r.2 == 0
    &&& frees(free, r.0 as int, r.1 as int)
    &&& well_formed(r.0 as int, r.1 as int)
    &&& runs_wf(r.0 as int, r.1 as int, r.2 as int)
}

/// A piece of a range [`range_ok`] is free and well formed, and inside
/// the range.
proof fn lemma_piece_ok(a: int, l: int, p: int, x: (int, int), free: spec_fn(int) -> bool)
    requires
        p > 0,
        l % p == 0,
        frees(free, a, l),
        runs_wf(a, l, p),
        in_run(a, l, p, x),
    ensures
        frees(free, x.0, x.1),
        well_formed(x.0, x.1),
        a <= x.0,
        x.0 + x.1 <= a + l,
{
    lemma_run_inside(a, l, p, x);
}

/// What a flush has left before range `i` is range `i`'s pieces and what
/// it leaves after; range `i`'s pieces are in neither ranges `i + 1..n`
/// nor `keep`: the ranges share no byte, and `keep` holds no piece of a
/// range.
proof fn lemma_pend_split(r: Seq<(usize, usize, usize)>, n: int, i: int, keep: spec_fn((int, int)) -> bool)
    requires
        0 <= i < n <= r.len(),
        disjoint_upto(r, n),
        forall|j: int| 0 <= j < n ==> 0 < (#[trigger] r[j]).2 && r[j].1 % r[j].2 == 0,
        forall|x: (int, int)| #[trigger] keep(x) ==> !pieces_of(r, 0, n, x),
    ensures
        from(r, i, n, keep) =~= at_on(r[i].0 as int, r[i].0 + r[i].1, r[i].2 as int, from(r, i + 1, n, keep)),
        minus(from(r, i, n, keep), r[i].0 as int, r[i].1 as int, r[i].2 as int) =~= from(r, i + 1, n, keep),
        forall|x: (int, int)| #[trigger] in_run(r[i].0 as int, r[i].1 as int, r[i].2 as int, x) ==> !(from(
            r,
            i + 1,
            n,
            keep,
        ))(x) && (from(r, i, n, keep))(x),
{
    assert forall|x: (int, int)| #[trigger] in_run(r[i].0 as int, r[i].1 as int, r[i].2 as int, x) implies !(from(
        r,
        i + 1,
        n,
        keep,
    ))(x) && (from(r, i, n, keep))(x) by {
        assert(pieces_of(r, 0, n, x));
        assert(pieces_of(r, i, n, x));
        if pieces_of(r, i + 1, n, x) {
            let j = choose|j: int| i + 1 <= j < n && #[trigger] in_run(r[j].0 as int, r[j].1 as int, r[j].2 as int, x);
            assert(0 < r[j].2 && r[j].1 % r[j].2 == 0);
            lemma_run_inside(r[i].0 as int, r[i].1 as int, r[i].2 as int, x);
            lemma_run_inside(r[j].0 as int, r[j].1 as int, r[j].2 as int, x);
            assert(r[i].0 + r[i].1 <= r[j].0 || r[j].0 + r[j].1 <= r[i].0);
        }
    }
    assert forall|x: (int, int)| #[trigger] pieces_of(r, i, n, x) implies in_run(r[i].0 as int, r[i].1 as int, r[i].2 as int, x)
        || pieces_of(r, i + 1, n, x) by {
        let j = choose|j: int| i <= j < n && #[trigger] in_run(r[j].0 as int, r[j].1 as int, r[j].2 as int, x);
        if j != i {
            assert(i + 1 <= j < n);
        }
    }
    assert forall|x: (int, int)| pieces_of(r, i + 1, n, x) implies pieces_of(r, i, n, x) by {
        let j = choose|j: int| i + 1 <= j < n && #[trigger] in_run(r[j].0 as int, r[j].1 as int, r[j].2 as int, x);
        assert(i <= j < n);
    }
    assert forall|x: (int, int)| #[trigger] (from(r, i, n, keep))(x) == (at_on(
        r[i].0 as int,
        r[i].0 + r[i].1,
        r[i].2 as int,
        from(r, i + 1, n, keep),
    ))(x) by {
        assert(r[i].0 + r[i].1 - r[i].0 == r[i].1);
    }
}

/// Once the piece at `at` of what a refused range has left is settled,
/// what is left starts at `at + p`; that piece is in what was left.
proof fn lemma_pend_step(at: int, e: int, p: int, rest: spec_fn((int, int)) -> bool)
    requires
        p > 0,
        at + p <= e,
        !rest((at, p)),
    ensures
        minus(at_on(at, e, p, rest), at, p, p) =~= at_on(at + p, e, p, rest),
        forall|x: (int, int)| #[trigger] in_run(at, p, p, x) ==> (at_on(at, e, p, rest))(x),
{
    assert forall|x: (int, int)| #[trigger] in_run(at, p, p, x) implies (at_on(at, e, p, rest))(x) by {
        lemma_run_one(at, p, x);
        vstd::arithmetic::div_mod::lemma_small_mod(0, p as nat);
    }
    assert forall|x: (int, int)| #[trigger] (minus(at_on(at, e, p, rest), at, p, p))(x) == (at_on(at + p, e, p, rest))(
        x,
    ) by {
        lemma_run_one(at, p, x);
        lemma_run_step(at, e - at, p, x);
        assert(e - at - p == e - (at + p));
    }
}

impl Batch {
    /// At most [`MAX_BATCH`] ranges, none of which ends past the address
    /// space and no two of which share a byte, each of bytes `free`
    /// admits, [`well_formed`], and a whole number of its pieces, each
    /// well formed too ([`range_ok`]); every range handed to [`discard`]
    /// was well formed.
    pub closed spec fn wf(&self) -> bool {
        &&& self.n <= MAX_BATCH
        &&& forall|i: int| 0 <= i < self.n ==> #[trigger] range_ok(self.ranges@[i], self.free@)
        &&& disjoint_upto(self.ranges@, self.n as int)
        &&& forall|i: int| #![auto] 0 <= i < self.gone@.len() ==> well_formed(self.gone@[i].0 as int, self.gone@[i].1 as int)
    }

    /// The bytes the batch may discard, fixed by its maker
    /// ([`Batch::new`]).
    pub closed spec fn free(&self) -> spec_fn(int) -> bool {
        self.free@
    }

    /// Bytes `madvise` took and a flush reported discarded
    /// ([`Settle::settle`] with `done`) since [`Batch::new`]: what the
    /// batch's flushes discarded. A flush adds to it what its settler's
    /// [`Settle::bytes`] gains.
    pub closed spec fn sent(&self) -> int {
        self.sent@
    }

    /// The pieces pushed and not yet settled: each of a range held,
    /// cut every `piece` bytes.
    pub closed spec fn pieces(&self) -> spec_fn((int, int)) -> bool {
        |x: (int, int)| pieces_of(self.ranges@, 0, self.n as int, x)
    }

    /// `[addr, addr + len)` shares no byte with a piece the batch holds.
    pub open spec fn apart(&self, addr: int, len: int) -> bool {
        forall|x: (int, int)| #[trigger] (self.pieces())(x) ==> x.0 + x.1 <= addr || addr + len <= x.0
    }

    /// The ranges handed to [`discard`] since [`Batch::new`], in call
    /// order: each flush appends its calls, a refused range's pieces
    /// included. Every one is [`well_formed`] ([`Batch::wf`]).
    pub closed spec fn discarded(&self) -> Seq<(usize, usize)> {
        self.gone@
    }

    /// Ranges held.
    pub closed spec fn count(&self) -> nat {
        self.n as nat
    }

    /// An empty batch that may discard the bytes `free` admits.
    pub const fn new(Ghost(free): Ghost<Bytes>) -> (b: Batch)
        ensures
            b.wf(),
            b.count() == 0,
            b.free() == free,
            b.sent() == 0,
            b.discarded() == Seq::<(usize, usize)>::empty(),
            b.pieces() == none(),
    {
        let b = Batch {
            ranges: [(0, 0, 0); MAX_BATCH],
            n: 0,
            free: Ghost(free),
            sent: Ghost(0),
            gone: Ghost(Seq::empty()),
        };
        proof {
            assert(b.pieces() =~= none());
        }
        b
    }

    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> (n: usize)
        ensures
            n == self.count(),
    {
        self.n
    }

    #[inline]
    #[cfg(test)]
    pub fn is_empty(&self) -> (e: bool)
        ensures
            e == (self.count() == 0),
    {
        self.n == 0
    }

    #[inline]
    pub fn is_full(&self) -> (f: bool)
        ensures
            f == (self.count() == MAX_BATCH),
    {
        self.n == MAX_BATCH
    }

    /// Add `[addr, addr + len)` (free, per the type's invariant), flushing
    /// first, with the settler `s`, if the batch is full. The settler's
    /// invariant must already count the range as pushed.
    pub fn push<S: Settle>(&mut self, addr: usize, len: usize, s: &mut S, o: &S::Owner, c: &mut S::Ctx)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(self).wf(),
            addr + len <= usize::MAX,
            frees(old(self).free(), addr as int, len as int),
            well_formed(addr as int, len as int),
            old(self).apart(addr as int, len as int),
            old(s).inv(o, *old(c), either(old(self).pieces(), one((addr as int, len as int)))),
        ensures
            final(self).wf(),
            final(self).free() == old(self).free(),
            final(s).inv(o, *final(c), final(self).pieces()),
            final(s).conf() == old(s).conf(),
            (final(self).pieces())((addr as int, len as int)),
            forall|x: (int, int)| #[trigger] (final(self).pieces())(x) ==> x == (addr as int, len as int) || (old(
                self,
            ).pieces())(x),
            final(self).sent() - old(self).sent() == final(s).bytes() - old(s).bytes(),
            final(self).discarded().len() >= old(self).discarded().len(),
            forall|i: int| 0 <= i < old(self).discarded().len() ==> #[trigger] final(self).discarded()[i]
                == old(self).discarded()[i],
    {
        proof {
            lemma_well_formed(addr as int, len as int);
        }
        let ghost new = (addr as int, len as int);
        // Adjacent ranges of one length pushed in address order (a run of
        // empty 4 KiB slabs) go out as one.
        if self.n != 0 {
            let (last, last_len, piece) = self.ranges[self.n - 1];
            proof {
                assert(range_ok(self.ranges@[self.n - 1], self.free@));
            }
            if last + last_len == addr && len == piece {
                let ghost r0 = self.ranges@;
                let ghost n = self.n as int;
                let ghost k = n - 1;
                proof {
                    assert(r0[k] == (last, last_len, piece));
                    assert(range_ok(r0[k], self.free@));
                    lemma_well_formed_join(last as int, last_len as int, len as int);
                    vstd::arithmetic::div_mod::lemma_mod_add_multiples_vanish(last_len as int, len as int);
                    assert forall|j: int| 0 <= j < n implies 0 < (#[trigger] r0[j]).2 <= r0[j].1 by {
                        assert(range_ok(r0[j], self.free@));
                    }
                    assert forall|x: (int, int)| #[trigger] pieces_of(r0, 0, n, x) implies x.0 + x.1 <= addr || addr
                        + len <= x.0 by {
                        assert((self.pieces())(x));
                    }
                }
                self.ranges[self.n - 1].1 = last_len + len;
                proof {
                    let r1 = self.ranges@;
                    assert(r1[k] == (last, (last_len + len) as usize, piece));
                    assert forall|x: (int, int)| #[trigger] in_run(last as int, (last_len + len) as int, len as int, x)
                        implies well_formed(x.0, x.1) by {
                        lemma_run_merge(last as int, last_len as int, len as int, x);
                    }
                    assert(range_ok(r1[k], self.free@));
                    assert forall|i: int| 0 <= i < n implies #[trigger] range_ok(r1[i], self.free@) by {
                        if i != k {
                            assert(r0[i] == r1[i]);
                            assert(range_ok(r0[i], self.free@));
                        }
                    }
                    assert forall|i: int, j: int|
                        #![trigger r1[i], r1[j]]
                        0 <= i < n && 0 <= j < n && i != j implies r1[i].0 + r1[i].1 <= r1[j].0 || r1[j].0 + r1[j].1
                            <= r1[i].0 by {
                        if i == k {
                            lemma_apart_range(r0, n, addr as int, len as int, j);
                            assert(r0[k].0 + r0[k].1 <= r0[j].0 || r0[j].0 + r0[j].1 <= r0[k].0);
                            assert(r0[j] == r1[j]);
                        } else if j == k {
                            lemma_apart_range(r0, n, addr as int, len as int, i);
                            assert(r0[k].0 + r0[k].1 <= r0[i].0 || r0[i].0 + r0[i].1 <= r0[k].0);
                            assert(r0[i] == r1[i]);
                        } else {
                            assert(r0[i] == r1[i] && r0[j] == r1[j]);
                        }
                    }
                    assert forall|x: (int, int)| #[trigger] (self.pieces())(x) == (either(
                        old(self).pieces(),
                        one(new),
                    ))(x) by {
                        lemma_run_merge(last as int, last_len as int, len as int, x);
                        if (self.pieces())(x) {
                            let j = choose|j: int| 0 <= j < n && #[trigger] in_run(
                                r1[j].0 as int,
                                r1[j].1 as int,
                                r1[j].2 as int,
                                x,
                            );
                            if j != k {
                                assert(r0[j] == r1[j]);
                                assert(pieces_of(r0, 0, n, x));
                            } else if x != new {
                                assert(in_run(r0[k].0 as int, r0[k].1 as int, r0[k].2 as int, x));
                                assert(pieces_of(r0, 0, n, x));
                            }
                        }
                        if (old(self).pieces())(x) {
                            let j = choose|j: int| 0 <= j < n && #[trigger] in_run(
                                r0[j].0 as int,
                                r0[j].1 as int,
                                r0[j].2 as int,
                                x,
                            );
                            if j != k {
                                assert(r0[j] == r1[j]);
                            }
                            assert(in_run(r1[j].0 as int, r1[j].1 as int, r1[j].2 as int, x));
                            assert(pieces_of(r1, 0, n, x));
                        }
                        if x == new {
                            assert(in_run(r1[k].0 as int, r1[k].1 as int, r1[k].2 as int, x));
                            assert(pieces_of(r1, 0, n, x));
                        }
                    }
                    assert(self.pieces() =~= either(old(self).pieces(), one(new)));
                }
                return;
            }
        }
        let ghost flushed = self.count() == MAX_BATCH;
        if self.is_full() {
            proof {
                assert forall|x: (int, int)| #[trigger] (one(new))(x) implies !(self.pieces())(x) by {
                    if (self.pieces())(x) {
                        assert(x.0 + x.1 <= addr || addr + len <= x.0);
                    }
                }
            }
            self.flush_in(s, o, c, Ghost(one(new)));
        }
        let ghost r0 = self.ranges@;
        let ghost n0 = self.n as int;
        proof {
            if flushed {
                assert(either(none(), one(new)) =~= one(new));
                assert(n0 == 0);
            } else {
                assert(r0 == old(self).ranges@ && n0 == old(self).n);
            }
            assert forall|x: (int, int)| #[trigger] pieces_of(r0, 0, n0, x) implies x.0 + x.1 <= addr || addr + len
                <= x.0 by {
                assert((old(self).pieces())(x));
            }
            assert forall|j: int| 0 <= j < n0 implies 0 < (#[trigger] r0[j]).2 <= r0[j].1 by {
                assert(range_ok(r0[j], self.free@));
            }
        }
        self.ranges[self.n] = (addr, len, len);
        self.n += 1;
        proof {
            let r1 = self.ranges@;
            let n = self.n as int;
            assert(r1[n0] == (addr, len, len));
            assert forall|x: (int, int)| #[trigger] in_run(addr as int, len as int, len as int, x) implies well_formed(
                x.0,
                x.1,
            ) by {
                lemma_run_one(addr as int, len as int, x);
            }
            vstd::arithmetic::div_mod::lemma_mod_self_0(len as int);
            assert forall|i: int| 0 <= i < n implies #[trigger] range_ok(r1[i], self.free@) by {
                if i != n0 {
                    assert(r0[i] == r1[i]);
                    assert(range_ok(r0[i], self.free@));
                }
            }
            assert forall|i: int, j: int|
                #![trigger r1[i], r1[j]]
                0 <= i < n && 0 <= j < n && i != j implies r1[i].0 + r1[i].1 <= r1[j].0 || r1[j].0 + r1[j].1 <= r1[i].0 by {
                if i == n0 {
                    lemma_apart_range(r0, n0, addr as int, len as int, j);
                    assert(r0[j] == r1[j]);
                } else if j == n0 {
                    lemma_apart_range(r0, n0, addr as int, len as int, i);
                    assert(r0[i] == r1[i]);
                } else {
                    assert(r0[i] == r1[i] && r0[j] == r1[j]);
                }
            }
            assert forall|x: (int, int)| #[trigger] (self.pieces())(x) == (pieces_of(r0, 0, n0, x) || x == new) by {
                lemma_run_one(addr as int, len as int, x);
                if (self.pieces())(x) {
                    let j = choose|j: int| 0 <= j < n && #[trigger] in_run(r1[j].0 as int, r1[j].1 as int, r1[j].2 as int, x);
                    if j != n0 {
                        assert(r0[j] == r1[j]);
                        assert(pieces_of(r0, 0, n0, x));
                    }
                }
                if pieces_of(r0, 0, n0, x) {
                    let j = choose|j: int| 0 <= j < n0 && #[trigger] in_run(r0[j].0 as int, r0[j].1 as int, r0[j].2 as int, x);
                    assert(r0[j] == r1[j]);
                    assert(pieces_of(r1, 0, n, x));
                }
                if x == new {
                    assert(in_run(r1[n0].0 as int, r1[n0].1 as int, r1[n0].2 as int, x));
                    assert(pieces_of(r1, 0, n, x));
                }
            }
            if flushed {
                assert(self.pieces() =~= one(new));
            } else {
                assert(self.pieces() =~= either(old(self).pieces(), one(new)));
            }
        }
    }

    /// Discard every range with `madvise`, report each to the settler
    /// `s` (a refused merged range piece by piece, the type's doc), then
    /// empty the batch. The advice is [`advice`] at this moment; a batch
    /// that discarded anything is counted, and so are the bytes discarded
    /// ([`discarded_bytes`]; a refused piece counts in
    /// [`failed_discards`]). A lazy flush makes `Purged` memory
    /// unknown-zero for the rest of the process (the module doc), which is
    /// published before the discard so a claim of one of these ranges
    /// after the caller's lock drops sees it. The ranges are free (the
    /// type's invariant), which is what makes the discard sound.
    #[inline(always)]
    pub fn flush<S: Settle>(&mut self, s: &mut S, o: &S::Owner, c: &mut S::Ctx)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(self).wf(),
            old(s).inv(o, *old(c), old(self).pieces()),
        ensures
            final(self).wf(),
            final(self).count() == 0,
            final(self).free() == old(self).free(),
            final(s).inv(o, *final(c), none()),
            final(s).conf() == old(s).conf(),
            final(self).pieces() == none(),
            final(self).sent() - old(self).sent() == final(s).bytes() - old(s).bytes(),
            final(self).discarded().len() >= old(self).discarded().len(),
            forall|i: int| 0 <= i < old(self).discarded().len() ==> #[trigger] final(self).discarded()[i]
                == old(self).discarded()[i],
    {
        proof {
            assert(either(self.pieces(), none()) =~= self.pieces());
        }
        self.flush_in(s, o, c, Ghost(none()));
    }

    /// [`Batch::flush`], with `keep`, pieces of the settler's invariant
    /// that the batch does not hold, left as they are: the flush inside a
    /// [`Batch::push`] leaves the range being pushed.
    // Not isolated: the loops see the function's context.
    #[verifier::loop_isolation(false)]
    fn flush_in<S: Settle>(
        &mut self,
        s: &mut S,
        o: &S::Owner,
        c: &mut S::Ctx,
        Ghost(keep): Ghost<Pieces>,
    )
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(self).wf(),
            old(s).inv(o, *old(c), either(old(self).pieces(), keep)),
            forall|x: (int, int)| #[trigger] keep(x) ==> !(old(self).pieces())(x),
        ensures
            final(self).wf(),
            final(self).count() == 0,
            final(self).free() == old(self).free(),
            final(s).inv(o, *final(c), keep),
            final(s).conf() == old(s).conf(),
            final(self).pieces() == none(),
            final(self).sent() - old(self).sent() == final(s).bytes() - old(s).bytes(),
            final(self).discarded().len() >= old(self).discarded().len(),
            forall|i: int| 0 <= i < old(self).discarded().len() ==> #[trigger] final(self).discarded()[i]
                == old(self).discarded()[i],
    {
        let ghost r = self.ranges@;
        let ghost n = self.n as int;
        let ghost free = self.free@;
        let ghost sent0 = self.sent@;
        let ghost bytes0 = s.bytes();
        let ghost conf0 = s.conf();
        let ghost gone0 = self.gone@;
        proof {
            assert forall|k: int| 0 <= k < n implies #[trigger] range_ok(r[k], free) by {
                assert(r[k] == self.ranges[k]);
            }
            let p = r.subrange(0, n);
            assert(disjoint_upto(p, n)) by {
                assert forall|i: int, j: int|
                    #![trigger p[i], p[j]]
                    0 <= i < n && 0 <= j < n && i != j implies p[i].0 + p[i].1 <= p[j].0 || p[j].0 + p[j].1 <= p[i].0 by {
                    assert(p[i] == r[i] && p[j] == r[j]);
                }
            }
            assert forall|i: int| 0 <= i < p.len() implies #[trigger] p[i].1 > 0 && p[i].0 + p[i].1 <= usize::MAX by {
                assert(p[i] == r[i]);
                assert(range_ok(r[i], free));
            }
            lemma_disjoint_total(p, usize::MAX as int);
            lemma_lens(p, r, n);
            assert forall|x: (int, int)| #[trigger] keep(x) implies !pieces_of(r, 0, n, x) by {
                assert(!(old(self).pieces())(x));
            }
            assert(either(old(self).pieces(), keep) =~= from(r, 0, n, keep));
        }
        if self.n != 0 {
            let advice = advice();
            if let Advice::Lazy = advice {
                LAZY_EVER.store(true, Ordering::Relaxed);
            }
            let mut bytes = 0u64;
            // The ranges by index and each range's pieces by address
            // (`for` over a slice and a `step_by` range, spelled out:
            // Verus does not specify those iterators).
            let mut i = 0;
            while i < self.n
                invariant
                    crate::sys::may_abort() || crate::sys::valid_aborts(),
                    crate::sys::may_syscall(),
                    self.ranges@ == r,
                    self.n == n,
                    self.free@ == free,
                    0 <= i <= n,
                    n <= MAX_BATCH,
                    forall|k: int| 0 <= k < n ==> #[trigger] range_ok(r[k], free),
                    disjoint_upto(r, n),
                    forall|x: (int, int)| #[trigger] keep(x) ==> !pieces_of(r, 0, n, x),
                    s.inv(o, *c, from(r, i as int, n, keep)),
                    bytes as int == self.sent@ - sent0,
                    s.bytes() - bytes0 == self.sent@ - sent0,
                    s.conf() == conf0,
                    bytes as int <= lens(r, i as int),
                    lens(r, n) <= usize::MAX,
                    self.gone@.len() >= gone0.len(),
                    forall|k: int| 0 <= k < gone0.len() ==> #[trigger] self.gone@[k] == gone0[k],
                    forall|k: int| #![auto] 0 <= k < self.gone@.len() ==> well_formed(self.gone@[k].0 as int, self.gone@[k].1 as int),
                decreases n - i,
            {
                let (addr, len, piece) = self.ranges[i];
                let ghost rest = from(r, i + 1, n, keep);
                proof {
                    assert(r[i as int] == (addr, len, piece));
                    assert(range_ok(r[i as int], free));
                    assert forall|k: int| 0 <= k < n implies 0 < (#[trigger] r[k]).2 && r[k].1 % r[k].2 == 0 by {
                        assert(range_ok(r[k], free));
                    }
                    lemma_pend_split(r, n, i as int, keep);
                    lemma_lens(r, r, n);
                    assert(lens(r, i + 1) == lens(r, i as int) + len);
                }
                let whole = discard(addr, len, advice, Ghost(free));
                self.gone = Ghost(self.gone@.push((addr, len)));
                if whole {
                    bytes += len as u64;
                    self.sent = Ghost(self.sent@ + len);
                    s.settle(
                        o,
                        c,
                        addr,
                        len,
                        true,
                        Ghost(piece as int),
                        Ghost(from(r, i as int, n, keep)),
                    );
                    i += 1;
                    continue;
                }
                // A range of one piece is not issued twice; when its first
                // and last pieces are refused as well, neither are the
                // middle ones (the type's doc).
                let last = addr + len - piece;
                proof {
                    vstd::arithmetic::div_mod::lemma_small_mod(0, piece as nat);
                    assert(in_run(addr as int, len as int, piece as int, (addr as int, piece as int)));
                    vstd::arithmetic::div_mod::lemma_mod_sub_multiples_vanish(len as int, piece as int);
                    assert(in_run(addr as int, len as int, piece as int, (last as int, piece as int)));
                    lemma_piece_ok(addr as int, len as int, piece as int, (addr as int, piece as int), free);
                    lemma_piece_ok(addr as int, len as int, piece as int, (last as int, piece as int), free);
                }
                let first_done = if piece < len {
                    let d = discard(addr, piece, advice, Ghost(free));
                    self.gone = Ghost(self.gone@.push((addr, piece)));
                    d
                } else {
                    false
                };
                let last_done = if piece < len {
                    let d = discard(last, piece, advice, Ghost(free));
                    self.gone = Ghost(self.gone@.push((last, piece)));
                    d
                } else {
                    false
                };
                let middle = first_done || last_done;
                let mut at = addr;
                proof {
                    assert(r[i as int].0 + r[i as int].1 == (addr + len) as int);
                }
                while at < addr + len
                    invariant
                        crate::sys::may_abort() || crate::sys::valid_aborts(),
                        crate::sys::may_syscall(),
                        self.ranges@ == r,
                        self.n == n,
                        self.free@ == free,
                        0 <= i < n,
                        r[i as int] == (addr, len, piece),
                        range_ok(r[i as int], free),
                        forall|x: (int, int)| #[trigger] in_run(addr as int, len as int, piece as int, x) ==> !rest(x),
                        rest == from(r, i + 1, n, keep),
                        last == addr + len - piece,
                        addr <= at <= addr + len,
                        (at - addr) % (piece as int) == 0,
                        s.inv(o, *c, at_on(at as int, (addr + len) as int, piece as int, rest)),
                        bytes as int == self.sent@ - sent0,
                        s.bytes() - bytes0 == self.sent@ - sent0,
                        s.conf() == conf0,
                        bytes as int <= lens(r, i as int) + (at - addr),
                        lens(r, i as int) + len <= lens(r, n),
                        lens(r, n) <= usize::MAX,
                        self.gone@.len() >= gone0.len(),
                        forall|k: int| 0 <= k < gone0.len() ==> #[trigger] self.gone@[k] == gone0[k],
                        forall|k: int| #![auto] 0 <= k < self.gone@.len() ==> well_formed(self.gone@[k].0 as int, self.gone@[k].1 as int),
                    decreases addr + len - at,
                {
                    proof {
                        assert(in_run(addr as int, len as int, piece as int, (at as int, piece as int)));
                        lemma_piece_ok(addr as int, len as int, piece as int, (at as int, piece as int), free);
                        lemma_pend_step(at as int, (addr + len) as int, piece as int, rest);
                        vstd::arithmetic::div_mod::lemma_mod_self_0(piece as int);
                    }
                    let done = if at == addr {
                        first_done
                    } else if at == last {
                        last_done
                    } else if middle {
                        let d = discard(at, piece, advice, Ghost(free));
                        self.gone = Ghost(self.gone@.push((at, piece)));
                        d
                    } else {
                        false
                    };
                    let ghost pend = at_on(at as int, (addr + len) as int, piece as int, rest);
                    proof {
                        // The whole range was refused, so each of its
                        // pieces was.
                        lemma_run_inside(addr as int, len as int, piece as int, (at as int, piece as int));
                        lemma_refused_inside(addr as int, len as int, at as int, piece as int);
                    }
                    if done {
                        bytes += piece as u64;
                        self.sent = Ghost(self.sent@ + piece);
                        s.settle(o, c, at, piece, true, Ghost(piece as int), Ghost(pend));
                    } else {
                        FAILED_PIECES.fetch_add(1, Ordering::Relaxed);
                        FAILED_BYTES.fetch_add(piece as u64, Ordering::Relaxed);
                        s.settle(o, c, at, piece, false, Ghost(piece as int), Ghost(pend));
                    }
                    proof {
                        vstd::arithmetic::div_mod::lemma_mod_add_multiples_vanish((at - addr) as int, piece as int);
                    }
                    at += piece;
                }
                proof {
                    assert(at_on(at as int, (addr + len) as int, piece as int, rest) =~= rest);
                }
                i += 1;
            }
            if bytes != 0 {
                let (batches, total) = match advice {
                    Advice::Lazy => (&LAZY_BATCHES, &LAZY_BYTES),
                    Advice::Eager => (&EAGER_BATCHES, &EAGER_BYTES),
                };
                batches.fetch_add(1, Ordering::Relaxed);
                total.fetch_add(bytes, Ordering::Relaxed);
            }
            self.n = 0;
            proof {
                assert(from(r, n, n, keep) =~= keep);
                assert(self.pieces() =~= none());
            }
        } else {
            proof {
                assert(from(r, 0, n, keep) =~= keep);
                assert(self.pieces() =~= none());
            }
        }
    }
}

/// A range apart from every piece of ranges `0..n` of `r` is apart from
/// each of those ranges: a byte of a range is in one of its pieces.
proof fn lemma_apart_range(r: Seq<(usize, usize, usize)>, n: int, addr: int, len: int, i: int)
    requires
        0 <= i < n <= r.len(),
        forall|j: int| 0 <= j < n ==> 0 < (#[trigger] r[j]).2 <= r[j].1,
        forall|x: (int, int)| #[trigger] pieces_of(r, 0, n, x) ==> x.0 + x.1 <= addr || addr + len <= x.0,
        len > 0,
    ensures
        r[i].0 + r[i].1 <= addr || addr + len <= r[i].0,
{
    let (a, l, p) = (r[i].0 as int, r[i].1 as int, r[i].2 as int);
    if !(a + l <= addr || addr + len <= a) {
        assert(0 < r[i].2 <= r[i].1);
        let b = if addr < a { a } else { addr };
        let x = lemma_byte_piece(a, l, p, b);
        assert(pieces_of(r, 0, n, x));
    }
}

/// Discard `[addr, addr + len)` alone, free per the [`Batch`] invariant
/// (bytes `free` admits), as a batch of one: whether it was discarded
/// (`Huge::free_run`'s purge at free time). True: the kernel took the
/// discard advice on the range ([`discard_taken`]); false: it answered one
/// on a range holding it with an error ([`discard_refused`]).
pub fn discard_one(addr: usize, len: usize, Ghost(free): Ghost<Bytes>) -> (r: bool)
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        addr + len <= usize::MAX,
        frees(free, addr as int, len as int),
        well_formed(addr as int, len as int),
    ensures
        r ==> discard_taken(addr as int, len as int),
        !r ==> discard_refused(addr as int, len as int),
{
    let mut discarded = One { discarded: false, bytes: Ghost(0), at: Ghost((addr as int, len as int)) };
    let mut batch = Batch::new(Ghost(free));
    proof {
        let at = (addr as int, len as int);
        assert forall|x: (int, int)| #[trigger] (either(batch.pieces(), one(at)))(x) implies x == at by {}
        assert((either(batch.pieces(), one(at)))(at));
    }
    batch.push(addr, len, &mut discarded, &(), &mut ());
    batch.flush(&mut discarded, &(), &mut ());
    proof {
        assert(discarded.at@ == (addr as int, len as int));
        assert(!(none())(discarded.at@));
    }
    discarded.discarded
}

/// What [`Decayer::due`] found at `now`.
pub enum Due {
    /// No walk is due: the owner's next deadline, `None` when nothing is
    /// `Empty`.
    Done(Option<u64>),
    /// The owner's walk is due, along this curve, whose step is the
    /// `u64`; then [`Decayer::finish`], with the load values `due` saw.
    Walk(Curve, u64, Ghost<(u64, u64, u64)>),
}

/// The step gate of design 8, one per `ClassState` and one in
/// the huge allocator: `dirty_at` of the oldest `Empty` member (0 when
/// none is known), written under the owner's lock and read without it by
/// the decay driver, and the deadline of the owner's next walk.
/// [`Decayer::due`] lets the owner's walk run at most once per step of the
/// curve, and not before the back-off a failed discard set has passed;
/// the walks themselves (`ClassState::purge_curve`, `Huge::purge_curve`)
/// differ and stay with their owners.
pub struct Decayer {
    /// `dirty_at` of the oldest `Empty` member, 0 when none. It may lag
    /// behind a claim that reused the oldest member, in which case it is
    /// too early rather than too late; the next walk recomputes it.
    oldest_empty: AtomicU64,
    /// When the next walk is due; 0 before the first.
    next_walk: AtomicU64,
    /// No walk before this ([`Decayer::back_off`]); 0 until a walk's
    /// discard fails.
    retry_at: AtomicU64,
}

impl Decayer {
    pub const fn new() -> Decayer {
        Decayer {
            oldest_empty: AtomicU64::new(0),
            next_walk: AtomicU64::new(0),
            retry_at: AtomicU64::new(0),
        }
    }

    /// A walk at `now` could not discard some of what it chose (the
    /// module doc): hold the owner's walks off for [`DISCARD_RETRY_NS`],
    /// whatever the period, so memory `madvise` keeps refusing is retried
    /// once a second rather than at every tick. Called by the walk, under
    /// the owner's lock.
    pub fn back_off(&self, now: u64) {
        self.retry_at.store(now.saturating_add(DISCARD_RETRY_NS), Ordering::Relaxed);
    }

    /// `dirty_at` of the oldest `Empty` member as the driver sees it (0
    /// when none).
    #[inline]
    pub fn oldest_empty(&self) -> u64 {
        self.oldest_empty.load(Ordering::Relaxed)
    }

    /// Publish the oldest `dirty_at` (0 for none) after a walk or a list
    /// change. Returns `true` when the owner went from no `Empty` member
    /// to one, which is when a background thread sleeping without a
    /// deadline needs waking ([`note_empty`]).
    pub fn set_oldest_empty(&self, dirty_at: u64) -> bool {
        let old = self.oldest_empty.swap(dirty_at, Ordering::Relaxed);
        old == 0 && dirty_at != 0
    }

    /// A member went `Empty` at `now`: record it as the oldest if none is
    /// known. Returns `true` when it is the first (see
    /// [`set_oldest_empty`]).
    ///
    /// [`set_oldest_empty`]: Decayer::set_oldest_empty
    pub fn note_first_empty(&self, now: u64) -> bool {
        if self.oldest_empty.load(Ordering::Relaxed) != 0 {
            return false;
        }
        self.oldest_empty.store(now, Ordering::Relaxed);
        true
    }

    /// Decay step, first half: once the oldest `Empty` member is a step
    /// of the curve old, a walk is due ([`Due::Walk`]) at most once per
    /// step and not during a back-off; the owner then runs its walk (which
    /// purges along the curve and republishes the oldest `dirty_at`) and
    /// calls [`finish`](Decayer::finish). Otherwise [`Due::Done`] holds the
    /// owner's next deadline (the end of the oldest member's first step, of
    /// the back-off, or of the current step), `None` when nothing is
    /// `Empty`. Lock-free. The walk runs between the halves, in the
    /// owner's own `decay_with`, which holds the owner's lock-order
    /// token. Together the two halves
    /// return [`spec_decay`] of the values their loads saw; the `ensures`
    /// state only that some load values give it, which is what
    /// [`theorem_decay_deadline`] bounds.
    ///
    /// [`spec_decay`]: Decayer::spec_decay
    /// [`theorem_decay_deadline`]: Decayer::theorem_decay_deadline
    pub fn due(&self, now: u64, decay_ns: u64) -> (r: Due)
        ensures
            match r {
                Due::Done(d) => {
                    &&& d matches Some(t) ==> t >= now
                    &&& exists|oldest: u64, retry: u64, next: u64, after: u64, retry2: u64|
                        d == #[trigger] Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2)
                },
                Due::Walk(curve, step, seen) => {
                    &&& Decayer::spec_walks(now, decay_ns, seen@.0, seen@.1, seen@.2)
                    &&& curve.period() == decay_ns
                    &&& step == Decayer::spec_step(decay_ns)
                },
            },
    {
        let oldest = self.oldest_empty();
        if oldest == 0 {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, 0, 0, 0, 0) == None::<u64>);
            }
            return Due::Done(None);
        }
        let curve = Curve::new(decay_ns);
        let step = curve.step_ns();
        let first = oldest.saturating_add(step);
        if now < first {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, 0, 0, 0, 0) == Some(first));
            }
            return Due::Done(Some(first));
        }
        let retry = self.retry_at.load(Ordering::Relaxed);
        if now < retry {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, retry, 0, 0, 0) == Some(retry));
            }
            return Due::Done(Some(retry));
        }
        // A deadline further away than one step was set under a longer
        // period since shortened: walk now.
        let next = self.next_walk.load(Ordering::Relaxed);
        if now < next && next - now <= step {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, oldest, retry, next, 0, 0) == Some(next));
            }
            return Due::Done(Some(next));
        }
        Due::Walk(curve, step, Ghost((oldest, retry, next)))
    }

    /// Decay step, second half, after the owner's walk that
    /// [`due`](Decayer::due) found due at `now` (its `step` and the load
    /// values it saw, `seen`): store the next walk's deadline and return
    /// the owner's next deadline, `None` when the walk left nothing
    /// `Empty`.
    pub fn finish(&self, now: u64, step: u64, Ghost(decay_ns): Ghost<u64>, Ghost(seen): Ghost<(u64, u64, u64)>) -> (r: Option<u64>)
        requires
            Decayer::spec_walks(now, decay_ns, seen.0, seen.1, seen.2),
            step == Decayer::spec_step(decay_ns),
        ensures
            r matches Some(t) ==> t >= now,
            exists|after: u64, retry2: u64|
                r == #[trigger] Decayer::spec_decay(now, decay_ns, seen.0, seen.1, seen.2, after, retry2),
    {
        let next = now.saturating_add(step);
        self.next_walk.store(next, Ordering::Relaxed);
        let after = self.oldest_empty();
        if after == 0 {
            proof {
                assert(Decayer::spec_decay(now, decay_ns, seen.0, seen.1, seen.2, after, 0) == None::<u64>);
            }
            None
        } else {
            let retry2 = self.retry_at.load(Ordering::Relaxed);
            let late = next.max(retry2);
            proof {
                assert(Decayer::spec_decay(now, decay_ns, seen.0, seen.1, seen.2, after, retry2) == Some(late));
            }
            Some(late)
        }
    }

    /// `a + b` saturated at `u64::MAX` (`u64::saturating_add`).
    pub open spec fn sat_add(a: u64, b: u64) -> u64 {
        if a + b > u64::MAX { u64::MAX } else { (a + b) as u64 }
    }

    /// One step of the curve for `decay_ns` ([`Curve::step_ns`]).
    pub open spec fn spec_step(decay_ns: u64) -> u64 {
        decay_ns / STEPS as u64
    }

    /// `due(now, decay_ns)` finds the walk due: something is `Empty` on
    /// entry (`oldest`, the first `oldest_empty` load), its first step is
    /// over, no back-off is in force (`retry`, the first `retry_at` load),
    /// and the stored deadline (`next`, the `next_walk` load) is not
    /// within one step after `now`: it has passed, or it was set under a
    /// longer period.
    pub open spec fn spec_walks(now: u64, decay_ns: u64, oldest: u64, retry: u64, next: u64) -> bool {
        let step = Decayer::spec_step(decay_ns);
        &&& oldest != 0
        &&& now >= Decayer::sat_add(oldest, step)
        &&& now >= retry
        &&& !(now < next && next - now <= step)
    }

    /// What `due(now, decay_ns)`, and after the walk `finish`, return when
    /// their loads see `oldest` (`oldest_empty` on entry), `retry`
    /// (`retry_at`), `next` (`next_walk`), `after` (`oldest_empty` after
    /// the walk, as the walk republished it or a concurrent writer left
    /// it) and `retry2` (`retry_at` after the walk, as a failed discard in
    /// it set it). A load the calls do not make does not matter.
    pub closed spec fn spec_decay(
        now: u64,
        decay_ns: u64,
        oldest: u64,
        retry: u64,
        next: u64,
        after: u64,
        retry2: u64,
    ) -> Option<u64> {
        let step = Decayer::spec_step(decay_ns);
        if oldest == 0 {
            None
        } else if now < Decayer::sat_add(oldest, step) {
            Some(Decayer::sat_add(oldest, step))
        } else if now < retry {
            Some(retry)
        } else if now < next && next - now <= step {
            Some(next)
        } else if after == 0 {
            None
        } else if retry2 > Decayer::sat_add(now, step) {
            Some(retry2)
        } else {
            Some(Decayer::sat_add(now, step))
        }
    }

    /// Decay deadline soundness: whatever its loads see,
    /// [`due`](Decayer::due) and [`finish`](Decayer::finish) return
    /// `None` exactly when nothing is `Empty` on entry or the walk ran
    /// and the `oldest_empty` load after it reads 0; a deadline is no earlier than `now`, and either a
    /// back-off deadline (a value read from `retry_at`, which
    /// [`back_off`](Decayer::back_off) sets [`DISCARD_RETRY_NS`] past the
    /// walk that failed to discard) or at most one step of the period it
    /// was called with past `max(oldest, now)`. When the oldest `Empty`
    /// member is not stamped after `now`, such a deadline is at most one
    /// step of the current period past `now`: a `next_walk` more than one
    /// step away (set under a longer period since shortened:
    /// `arena.<i>.dirty_decay_ms`, a pressure band) is never returned. A
    /// deadline equal to `now` comes only from a zero step or saturation,
    /// and only after a walk.
    pub proof fn theorem_decay_deadline(
        now: u64,
        decay_ns: u64,
        oldest: u64,
        retry: u64,
        next: u64,
        after: u64,
        retry2: u64,
    )
        ensures
            ({
                let r = Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2);
                let step = Decayer::spec_step(decay_ns);
                &&& r is None <==> oldest == 0 || (Decayer::spec_walks(now, decay_ns, oldest, retry, next)
                    && after == 0)
                &&& r matches Some(t) ==> now <= t
                &&& r matches Some(t) ==> t == retry || t == retry2 || t <= (if oldest <= now {
                    now
                } else {
                    oldest
                }) + step
                &&& r matches Some(t) ==> (oldest <= now ==> t == retry || t == retry2 || t <= now + step)
                &&& r matches Some(t) ==> (t == now ==> Decayer::spec_walks(now, decay_ns, oldest, retry, next)
                    && (step == 0 || now == u64::MAX))
            }),
    {
    }

    /// Decay deadline soundness under pressure (with
    /// `policy::theorem_pressure_never_lengthens_decay`): at one limit and
    /// one configured period `base`, the bound on the gate's deadline at
    /// usage `c2` is no later than at a lower usage `c1`, whatever the
    /// loads see, back-off deadlines aside. `p1` and `p2` are the
    /// effective periods for `c1` and `c2`
    /// (`policy::spec_effective_decay_ns`).
    pub proof fn theorem_decay_deadline_under_pressure(
        base: u64,
        limit: u64,
        c1: u64,
        c2: u64,
        p1: u64,
        p2: u64,
        now: u64,
        oldest: u64,
        retry: u64,
        next: u64,
        after: u64,
        retry2: u64,
    )
        requires
            c1 <= c2,
            p1 == crate::policy::spec_effective_decay_ns(base, crate::policy::spec_band(limit, c1)),
            p2 == crate::policy::spec_effective_decay_ns(base, crate::policy::spec_band(limit, c2)),
        ensures
            Decayer::spec_step(p2) <= Decayer::spec_step(p1),
            Decayer::spec_decay(now, p2, oldest, retry, next, after, retry2) matches Some(t) ==> t == retry || t
                == retry2 || t <= (if oldest <= now { now } else { oldest }) + Decayer::spec_step(p1),
    {
        crate::policy::theorem_pressure_never_lengthens_decay(base, limit, c1, c2);
        vstd::arithmetic::div_mod::lemma_div_is_ordered(p2 as int, p1 as int, STEPS as int);
        Decayer::theorem_decay_deadline(now, p2, oldest, retry, next, after, retry2);
    }
}

/// What an explicit purge left (ghost), for the purges' contracts: each
/// purge is to set it from the lock values it releases, just before each
/// release, so that to a caller it is only a value those contracts
/// constrain.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct Left {
    /// Bytes `madvise` took and a flush reported discarded
    /// ([`Batch::sent`]), every batch summed.
    pub bytes: int,
    /// Arenas whose every class was purged.
    pub arenas: Set<int>,
    /// `(arena, class)` of every class lock the purge took and released.
    pub classes: Set<(int, int)>,
    /// `(arena, class, slab)` of every slab `Empty` in a class lock value
    /// the purge released.
    pub slabs: ISet<(int, int, int)>,
    /// Whether the purge took and released the huge lock.
    pub huge: bool,
    /// The addresses of the descriptors of the extents `Empty` in the huge
    /// lock value the purge released (none when `huge` is false).
    pub extents: ISet<int>,
    /// `(arena, class, slab)` of every slab of a range `madvise` refused in
    /// the purge (a flush reported it not discarded), `Empty` when the
    /// refusal was reported: memory the program locked (`mlock`).
    pub refused: ISet<(int, int, int)>,
    /// The addresses of the descriptors of the extents of a range
    /// `madvise` refused in the purge, `Empty` when the refusal was
    /// reported.
    pub refused_extents: ISet<int>,
}

/// Class `c`'s slabs are smaller than the process's page, so a page holds
/// several (`SlabTable::slabs_per_page`, the class's guard geometry): only
/// for a page above 64 KiB, which no supported kernel has
/// (`layout::lg_slab_at`).
pub open spec fn paged(c: int) -> bool {
    crate::layout::guard_geom(c).0 != 0
}

impl Left {
    /// Nothing purged.
    pub open spec fn none() -> Left {
        Left {
            bytes: 0,
            arenas: Set::empty(),
            classes: Set::empty(),
            slabs: ISet::empty(),
            huge: false,
            extents: ISet::empty(),
            refused: ISet::empty(),
            refused_extents: ISet::empty(),
        }
    }

    /// Two purges' records together.
    pub open spec fn join(self, o: Left) -> Left {
        Left {
            bytes: self.bytes + o.bytes,
            arenas: self.arenas + o.arenas,
            classes: self.classes + o.classes,
            slabs: self.slabs + o.slabs,
            huge: self.huge || o.huge,
            extents: self.extents + o.extents,
            refused: self.refused + o.refused,
            refused_extents: self.refused_extents + o.refused_extents,
        }
    }

    /// An explicit purge's guarantee: no slab was left `Empty` in a class
    /// lock value released, except in a class with several slabs per page
    /// (whose page group another slab can keep from being discarded) or in
    /// a range `madvise` refused, and no huge extent was left `Empty`
    /// except in a range `madvise` refused; the byte count is not
    /// negative.
    pub open spec fn clean(self) -> bool {
        &&& forall|t: (int, int, int)| #[trigger] self.slabs.contains(t) ==> paged(t.1) || self.refused.contains(t)
        &&& forall|a: int| #[trigger] self.extents.contains(a) ==> self.refused_extents.contains(a)
        &&& self.bytes >= 0
    }

    /// The classes purged are every class of the arenas purged.
    pub open spec fn covers(self) -> bool {
        forall|a: int, c: int| #[trigger] self.classes.contains((a, c))
            <==> self.arenas.contains(a) && 0 <= c < crate::layout::NCLASSES
    }

    /// Nothing purged is clean and covers.
    pub proof fn lemma_none()
        ensures
            Left::none().clean(),
            Left::none().covers(),
    {
    }

    /// Two clean records join to a clean one, two that cover to one that
    /// covers.
    pub proof fn lemma_join(a: Left, b: Left)
        ensures
            a.clean() && b.clean() ==> a.join(b).clean(),
            a.covers() && b.covers() ==> a.join(b).covers(),
    {
        let j = a.join(b);
        if a.clean() && b.clean() {
            assert forall|x: int| #[trigger] j.extents.contains(x) implies j.refused_extents.contains(x) by {
                if a.extents.contains(x) {
                } else {
                    assert(b.extents.contains(x));
                }
            }
            assert forall|t: (int, int, int)| #[trigger] j.slabs.contains(t) implies paged(t.1) || j.refused.contains(t) by {
                if a.slabs.contains(t) {
                } else {
                    assert(b.slabs.contains(t));
                }
            }
        }
        if a.covers() && b.covers() {
            assert forall|x: int, c: int| #[trigger] j.classes.contains((x, c))
                <==> j.arenas.contains(x) && 0 <= c < crate::layout::NCLASSES by {
                assert(a.classes.contains((x, c)) <==> a.arenas.contains(x) && 0 <= c < crate::layout::NCLASSES);
                assert(b.classes.contains((x, c)) <==> b.arenas.contains(x) && 0 <= c < crate::layout::NCLASSES);
            }
        }
    }
}

} // verus!

verus! {

/// Run every decay walk that is due at `now` (coarse monotonic
/// nanoseconds) and return the earliest future deadline, `None` when
/// nothing is `Empty` or decay is off. Lock-free for a class with nothing
/// due.
///
/// Never inlined, so `Heap::tick` (through `on_tick`) and
/// `background::main` keep a call here rather than a copy of the loop.
#[inline(never)]
pub fn decay(shared: &Shared, now: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    // Design 11: a `memory.events` `high` rise owes one full purge, run
    // before the walks so they see the survivors (none, normally).
    let policy = &shared.policy;
    if policy.take_full_purge() {
        let tracked mut left = Ghost(Left::none());
        purge_all(shared, Tracked(&mut left), Tracked(ord));
    }
    let keep16 = policy.keep16();
    let mut next: Option<u64> = None;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            shared.wf(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            count <= crate::arena::MAX_ARENAS,
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            proof {
                shared.lemma_arena_tables(*arena);
            }
            next = earliest(next, decay_one(shared, arena, now, keep16, Tracked(ord)));
        }
    }
    next = earliest(next, decay_huge(shared, now, keep16, Tracked(ord)));
    next
}

/// `arena.<i>.decay`: the decay walks of one arena (plus, for arena 0,
/// the huge extents, which belong to no single arena's sets) that are due
/// at `now`, on the periods and keep ratio [`decay`] gives them. `None`
/// if the arena does not exist.
pub fn decay_arena(shared: &Shared, index: u16, now: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<()>
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let arena = shared.arenas.get_existing(index)?;
    let keep16 = shared.policy.keep16();
    proof {
        shared.lemma_arena_tables(*arena);
    }
    decay_one(shared, arena, now, keep16, Tracked(ord));
    if index == 0 {
        decay_huge(shared, now, keep16, Tracked(ord));
    }
    Some(())
}

/// One arena's walks on its own period, scaled by the policy's band.
fn decay_one(shared: &Shared, arena: &Arena, now: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
    requires
        arena.tables_process(),
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    arena.decay_with(
        now,
        shared.policy.effective_decay_ns(arena.decay_ns()),
        keep16,
        Tracked(ord),
    )
}

/// The huge allocator's walk. It is shared by every arena: its own period
/// when `huge_dirty_decay_ms` is set, else arena 0's (design 7), scaled by
/// the policy's band.
fn decay_huge(shared: &Shared, now: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    let decay_ns = shared.policy.effective_decay_ns(shared.huge_decay_ns());
    if decay_ns == NEVER {
        return None;
    }
    proof {
        shared.lemma_huge_process();
    }
    shared.huge.decay_with(now, decay_ns, keep16, Tracked(ord))
}

/// The earlier of two optional deadlines; either one when the other is
/// `None`.
#[inline(always)]
fn earliest(next: Option<u64>, d: Option<u64>) -> (r: Option<u64>)
    ensures
        d is None ==> r == next,
        d matches Some(x) ==> r == Some(match next {
            Some(n) => if n < x { n } else { x },
            None => x,
        }),
{
    match d {
        Some(d) => Some(next.map_or(d, |n: u64| -> (m: u64)
            ensures
                m == if n < d { n } else { d },
            { n.min(d) })),
        None => next,
    }
}
/// Arena indices to visit: [`crate::arena::Arenas::count`], which never
/// exceeds the table (`create` stops at [`MAX_ARENAS`]; `count` is
/// verified to ensure it, so the clamp is redundant and stays only to
/// keep the machine code).
///
/// [`MAX_ARENAS`]: crate::arena::MAX_ARENAS
#[inline]
fn arena_count(shared: &Shared) -> (n: usize)
    ensures
        n <= crate::arena::MAX_ARENAS,
{
    (shared.arenas.count() as usize).min(crate::arena::MAX_ARENAS)
}

/// Purge every `Empty` slab of every arena and every huge extent
/// regardless of age, eagerly (`malloc_trim(0)`, the `high`-rise full
/// purge: the memory is wanted back now). Returns how many were purged.
///
/// `left` gets what was purged: the bytes discarded, nonzero iff anything
/// was purged, the arenas visited, every class of each, and the huge
/// allocator, with no slab or extent `Empty` in the lock value each purge
/// released, except in a class with several slabs per page or in a range
/// `madvise` refused ([`Left::clean`]).
pub fn purge_all(shared: &Shared, Tracked(left): Tracked<&mut Ghost<Left>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
    requires
        crate::sys::may_abort() || crate::sys::valid_aborts(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
        (r > 0) == (final(left)@.bytes > 0),
        forall|i: int| #[trigger] final(left)@.arenas.contains(i) ==> 0 <= i < crate::arena::MAX_ARENAS,
        final(left)@.covers(),
        final(left)@.huge,
        final(left)@.clean(),
{
    let _explicit = Explicit::enter();
    proof {
        *left = Ghost(Left::none());
        Left::lemma_none();
    }
    let mut n: usize = 0;
    let count = arena_count(shared);
    for i in 0..count
        invariant
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            shared.wf(),
            ord.allows(rank_class(0, 0)),
            *ord == *old(ord),
            count <= crate::arena::MAX_ARENAS,
            n <= i * 0x8_0000_0000_0000,
            (n > 0) == (left@.bytes > 0),
            forall|a: int| #[trigger] left@.arenas.contains(a) ==> 0 <= a < i,
            left@.covers(),
            !left@.huge,
            left@.clean(),
    {
        if let Some(arena) = shared.arenas.get_existing(i as u16) {
            let tracked mut la = Ghost(Left::none());
            // At most `NCLASSES * 2^45` slabs an arena.
            proof {
                shared.lemma_arena_tables(*arena);
            }
            n += arena.purge_all(Tracked(&mut la), Tracked(ord));
            proof {
                let l0 = left@;
                Left::lemma_join(l0, la@);
                *left = Ghost(l0.join(la@));
                assert forall|a: int| #[trigger] left@.arenas.contains(a) implies 0 <= a < i + 1 by {
                    if !l0.arenas.contains(a) {
                        assert(la@.arenas.contains(a));
                    }
                }
            }
        }
    }
    let tracked mut lh = Ghost(Left::none());
    proof {
        shared.lemma_huge_process();
    }
    let h = shared.huge.purge_empty(u64::MAX, Tracked(&mut lh), Tracked(ord));
    proof {
        let l0 = left@;
        Left::lemma_join(l0, lh@);
        *left = Ghost(l0.join(lh@));
        assert(left@.arenas =~= l0.arenas);
        assert(left@.classes =~= l0.classes);
    }
    n + h
}

/// `arena.<i>.purge`: purge every `Empty` slab of one arena regardless of
/// age (plus, for arena 0, the huge extents, which belong to no single
/// arena's sets), eagerly. `None` if the arena does not exist.
///
/// `left` gets what [`purge_all`] says of its purges: here every class of
/// arena `index` and, for arena 0, the huge allocator.
pub fn purge_arena(
    shared: &Shared,
    index: u16,
    Tracked(left): Tracked<&mut Ghost<Left>>,
    Tracked(ord): Tracked<&mut LockOrder>,
) -> (r: Option<usize>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
        r is None ==> final(left)@ == Left::none(),
        r matches Some(n) ==> (n > 0) == (final(left)@.bytes > 0),
        r is Some ==> final(left)@.arenas == set![index as int],
        final(left)@.huge == (r is Some && index == 0),
        final(left)@.covers(),
        final(left)@.clean(),
{
    let _explicit = Explicit::enter();
    proof {
        *left = Ghost(Left::none());
        Left::lemma_none();
    }
    let arena = shared.arenas.get_existing(index)?;
    proof {
        shared.lemma_arena_tables(*arena);
    }
    let mut n = arena.purge_all(Tracked(&mut *left), Tracked(ord));
    if index == 0 {
        let tracked mut lh = Ghost(Left::none());
        proof {
            shared.lemma_huge_process();
        }
        n += shared.huge.purge_empty(u64::MAX, Tracked(&mut lh), Tracked(ord));
        proof {
            let l0 = left@;
            Left::lemma_join(l0, lh@);
            *left = Ghost(l0.join(lh@));
            assert(left@.arenas =~= l0.arenas);
        }
    }
    Some(n)
}

/// Periodic-work hook (design 6.4): start the background thread if one was
/// requested and does not exist, otherwise run decay here unless the
/// thread is doing it, and let the lazy start look at what decay left.
/// `alloc` is whether the tick is an allocation's: a `free`'s tick never
/// creates (or joins) a thread, because glibc frees with its stack cache
/// lock held (`pthread_join`'s TLS teardown, the stack cache trim) and
/// `pthread_create` takes that lock again. Such a tick purges in the
/// foreground and leaves the start to the next allocation's tick.
/// So it needs only `valid_aborts`: the thread start, which may abort,
/// runs only on an allocation's tick, and decay reaches only resource
/// failures and pending checks.
pub fn on_tick(shared: &Shared, now: u64, alloc: bool, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut crate::vspec::errno::ThreadErrno>, Tracked(cell): Tracked<Option<&mut crate::sys::tls::TlsCell>>)
    requires
        alloc ==> cell is Some,
        crate::sys::may_abort() || (crate::sys::valid_aborts() && !alloc),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        cell matches Some(c) ==> final(c).kept(*c),
        final(errno).val() == old(errno).val(),
        *final(ord) == *old(ord),
{
    sample(shared, now, Tracked(ord));
    if background::present() {
        return;
    }
    let tracked mut cell = cell;
    if alloc && background::requested() && background::start_requested(shared, Tracked(ord), Tracked(&mut *errno), Tracked(crate::sys::tls::TlsCell::lend(&mut cell))) {
        return;
    }
    if decay(shared, now, Tracked(ord)).is_some() {
        // Dirty memory is still waiting: the lazy start's look.
        background::auto_tick(shared, now, alloc, Tracked(ord), Tracked(&mut *errno), Tracked(cell));
    }
}

/// Design 11: `memory.current` and `memory.events`, at most once per 10 ms
/// of the coarse clock, and the policy evaluated on the sample that was
/// taken; nothing at all when the cgroup is untracked or the window has
/// not passed. Slow paths only (the periodic tick, the background thread).
fn sample(shared: &Shared, now: u64, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    if shared.discover_cgroup_if_due(now, Tracked(ord)) {
        return;
    }
    // `shared.cgroup().is_some_and(|cg| cg.sample(now))`, spelled out:
    // Verus does not specify `Option::is_some_and`.
    let sampled = match shared.cgroup() {
        Some(cg) => cg.sample(now),
        None => false,
    };
    if sampled {
        crate::policy::evaluate(shared, Tracked(ord));
    }
}

/// A class or the huge allocator went from no `Empty` slab to one: wake a
/// background thread that may be sleeping without a deadline.
#[inline]
pub fn note_empty()
    requires
        crate::sys::may_syscall(),
{
    background::wake();
}

/// A decay period changed (`arena.<i>.dirty_decay_ms`, `malloc_conf` at
/// run time): wake a background thread whose deadline was computed with
/// the old period, so a shorter period takes effect now rather than at
/// the next wake-up.
pub fn note_decay_changed()
    requires
        crate::sys::may_syscall(),
{
    background::wake();
}

/// The kernel answered a discard advice (`MADV_DONTNEED` or `MADV_FREE`)
/// on `[addr, addr + len)` with success (`sys::madvise_ok`): what
/// [`discard`] returning true says. Timeless: it says that some such call
/// was answered so, not that the last one was.
pub closed spec fn discard_taken(addr: int, len: int) -> bool {
    &&& 0 <= addr <= usize::MAX
    &&& 0 <= len <= usize::MAX
    &&& exists|a: i32|
        (a == crate::sys::MADV_DONTNEED || a == crate::sys::MADV_FREE) && #[trigger] crate::sys::madvise_ok(
            addr as usize,
            len as usize,
            a,
        )
}

/// The kernel answered a discard advice on a range holding
/// `[addr, addr + len)` with success ([`discard_taken`] of that range):
/// what a class purge records of each slab it makes `Purged`. Timeless,
/// as [`discard_taken`] is: it says that some such call was answered so,
/// not which one or when.
pub closed spec fn discard_covers(addr: int, len: int) -> bool {
    exists|a: int, l: int| a <= addr && addr + len <= a + l && #[trigger] discard_taken(a, l)
}

/// A range inside one [`discard_taken`] is [`discard_covers`].
pub proof fn lemma_taken_covers(a: int, l: int, addr: int, len: int)
    requires
        discard_taken(a, l),
        a <= addr,
        addr + len <= a + l,
    ensures
        discard_covers(addr, len),
{
}

/// A range inside one [`discard_covers`] is [`discard_covers`].
pub proof fn lemma_covers_inside(addr: int, len: int, at: int, l: int)
    requires
        discard_covers(addr, len),
        addr <= at,
        at + l <= addr + len,
    ensures
        discard_covers(at, l),
{
    let (a0, l0) = choose|a0: int, l0: int| a0 <= addr && addr + len <= a0 + l0 && #[trigger] discard_taken(a0, l0);
    assert(a0 <= at && at + l <= a0 + l0 && discard_taken(a0, l0));
}

/// The kernel answered a discard advice on a range holding
/// `[addr, addr + len)` with an error (`sys::madvise_err`): what a
/// [`discard`] returning false says of the range, and of each of its
/// pieces. A refused call may still have discarded some of the pages
/// (the doc of [`discard`]). Timeless, as [`discard_taken`] is: a range
/// can be both, and an earlier refusal keeps it refused.
pub closed spec fn discard_refused(addr: int, len: int) -> bool {
    exists|a: usize, l: usize, adv: i32|
        a <= addr && addr + len <= a + l && (adv == crate::sys::MADV_DONTNEED || adv == crate::sys::MADV_FREE)
            && #[trigger] crate::sys::madvise_err(a, l, adv)
}

/// An `madvise` success with a discard advice is [`discard_taken`] of its
/// range.
broadcast proof fn lemma_madvise_taken(addr: usize, len: usize, a: i32)
    ensures
        #[trigger] crate::sys::madvise_ok(addr, len, a) && (a == crate::sys::MADV_DONTNEED || a == crate::sys::MADV_FREE)
            ==> discard_taken(addr as int, len as int),
{
    if crate::sys::madvise_ok(addr, len, a) {
        assert(crate::sys::madvise_ok((addr as int) as usize, (len as int) as usize, a));
    }
}

/// An `madvise` error with a discard advice is [`discard_refused`] of its
/// range.
broadcast proof fn lemma_madvise_refused(addr: usize, len: usize, a: i32)
    ensures
        #[trigger] crate::sys::madvise_err(addr, len, a) && (a == crate::sys::MADV_DONTNEED || a == crate::sys::MADV_FREE)
            ==> discard_refused(addr as int, len as int),
{
    if crate::sys::madvise_err(addr, len, a) {
        assert(addr <= addr as int && addr + len <= addr + len as int);
    }
}

/// A refusal of a range is one of each range inside it.
proof fn lemma_refused_inside(addr: int, len: int, at: int, l: int)
    requires
        discard_refused(addr, len),
        addr <= at,
        at + l <= addr + len,
    ensures
        discard_refused(at, l),
{
    let (a, k, adv) = choose|a: usize, k: usize, adv: i32|
        a <= addr && addr + len <= a + k && (adv == crate::sys::MADV_DONTNEED || adv == crate::sys::MADV_FREE)
            && #[trigger] crate::sys::madvise_err(a, k, adv);
    assert(crate::sys::madvise_err(a, k, adv));
}

/// Issue `advice` on `[addr, addr + len)`, which is free (the [`Batch`]
/// invariant; this is the one place the batch's ranges reach the
/// kernel): every byte of it is one the batch's `free` admits. It is
/// also [`well_formed`]: non-empty, page-aligned, and in the huge region
/// or a class region outside its guard slabs, so never metadata
/// ([`theorem_purge_well_formed`]); the page-alignment check never fails.
/// Whether the kernel took it: on a refusal (`EINVAL` for a
/// range with `mlock`ed pages, under `mlockall` every range) the pages
/// keep their contents, or some of them do (Linux applies the advice to
/// the VMAs before the one it refuses). Every supported kernel has
/// `MADV_FREE` (design 17), so `EINVAL` is never an unknown advice. Any
/// other errno aborts: `ENOMEM` (part
/// of the range not mapped) means one of our reservations is gone,
/// `EPERM` that the program sealed our free memory, and anything else is
/// unexplained.
// `%` rather than `is_multiple_of`: the purge proofs reason about `%`.
#[allow(clippy::manual_is_multiple_of)]
fn discard(addr: usize, len: usize, advice: Advice, Ghost(free): Ghost<Bytes>) -> (r: bool)
    requires
        crate::sys::may_abort_kernel(),
        crate::sys::may_syscall(),
        frees(free, addr as int, len as int),
        well_formed(addr as int, len as int),
    ensures
        r ==> discard_taken(addr as int, len as int),
        !r ==> discard_refused(addr as int, len as int),
{
    broadcast use crate::vspec::fmt::lemma_errno_fmt_req;
    broadcast use lemma_madvise_taken, lemma_madvise_refused;

    // The range must be whole pages (checked against the page size here):
    // the check never fails, by the precondition, so only the `madvise`
    // failure aborts.
    let page = sys::features::page_size();
    proof {
        lemma_well_formed(addr as int, len as int);
    }
    if !(addr % page == 0 && len % page == 0 && len != 0) {
        sys::abort_fmt(format_args!("purge: discard of {addr:#x}+{len:#x} is not page-aligned"));
    }
    #[cfg(test)]
    DISCARD_CALLS.fetch_add(1, Ordering::Relaxed);
    let advice = advice.madv();
    // SAFETY: the range is a page-aligned part of our own mappings that
    // the caller holds free (the `Batch` invariant).
    match unsafe { sys::madvise(addr, len, advice) } {
        Ok(()) => true,
        Err(e) if e.0 == sys::Errno::EINVAL.0 => false,
        Err(e) => sys::abort_kernel(format_args!("purge: madvise({advice}) of {addr:#x}+{len:#x} failed: {e}")),
    }
}

} // verus!

/// `madvise` calls [`discard`] made (the tests' syscall counts).
#[cfg(test)]
static DISCARD_CALLS: AtomicU64 = AtomicU64::new(0);

verus! {

/// A test's settler: a closure over each report, as the tests write
/// them, with a ghost count of the bytes reported discarded.
#[cfg(test)]
struct FnSettle<F: FnMut(usize, usize, bool)>(F, Ghost<int>);

#[cfg(test)]
impl<F: FnMut(usize, usize, bool)> Settle for FnSettle<F> {
    type Owner = ();

    type Ctx = ();

    type Conf = ();

    closed spec fn inv(&self, o: &(), c: (), pend: spec_fn((int, int)) -> bool) -> bool {
        true
    }

    closed spec fn bytes(&self) -> int {
        self.1@
    }

    closed spec fn conf(&self) -> () {
        ()
    }

    fn settle(
        &mut self,
        _o: &(),
        _c: &mut (),
        addr: usize,
        len: usize,
        done: bool,
        _piece: Ghost<int>,
        _pend: Ghost<Pieces>,
    ) {
        (self.0)(addr, len, done);
        self.1 = Ghost(self.1@ + if done {
            len as int
        } else {
            0
        });
    }
}

} // verus!

/// Tests: a batch with no ghost predicate (the tests hold no ghost
/// state), pushed and flushed with a closure as the settler.
#[cfg(test)]
impl Batch {
    fn new_t() -> Batch {
        Batch::new(Ghost::assume_new())
    }

    fn push_t(&mut self, addr: usize, len: usize, settle: impl FnMut(usize, usize, bool)) {
        self.push(
            addr,
            len,
            &mut FnSettle(settle, Ghost::assume_new()),
            &(),
            &mut (),
        )
    }

    fn flush_t(&mut self, settle: impl FnMut(usize, usize, bool)) {
        self.flush(&mut FnSettle(settle, Ghost::assume_new()), &(), &mut ())
    }
}

/// The background thread (design 8.1).
pub mod background;

/// Kani harness for `Explicit::drop`, this file's seam that Kani can run
/// (`verus/ledger/src/purge.rs.md`).
#[cfg(kani)]
mod kani_explicit {
    use super::{EXPLICIT, Explicit};
    use core::sync::atomic::Ordering::Relaxed;

    /// `Explicit::drop`: no `requires`, no `ensures`, `no_unwind` and
    /// `opens_invariants none` (which Kani does not check): it
    /// returns without a panic for every count `EXPLICIT` holds (one less
    /// afterwards, wrapping at zero as an atomic subtraction does), both
    /// when a guard from `Explicit::enter` drops and when the count was
    /// changed in between.
    #[kani::proof]
    fn explicit_drop_returns() {
        let c: u32 = kani::any();
        EXPLICIT.store(c, Relaxed);
        let g = Explicit::enter();
        assert_eq!(EXPLICIT.load(Relaxed), c.wrapping_add(1));
        let d: u32 = kani::any();
        EXPLICIT.store(d, Relaxed);
        drop(g);
        assert_eq!(EXPLICIT.load(Relaxed), d.wrapping_sub(1));
    }
}
