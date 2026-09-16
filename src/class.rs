//! Per-class state, slab selection, commit, and the claim/release protocol
//! (design.md sections 4 and 5).
//!
//! One [`ClassState`] exists per `(arena, class)`. It owns the class's
//! descriptor array and three [`TwoLevelBitmap`]s over slab numbers:
//! `partial` (slabs believed to have a claimable slot), `empty` (`live == 0`,
//! dirty, not yet purged) and `purged` (`live == 0`, pages discarded). All
//! three plus the commit watermark are behind one futex lock that is taken
//! only on slab transitions: selecting a slab for a refill, a slab filling
//! up or emptying, a word going from full to non-full, purge, commit.
//! Claims and releases against a slab's bitmap happen outside the lock.
//!
//! Protocol. A refill takes the lock, picks the lowest partial slab (else
//! the lowest empty, else the lowest purged, else commits the next slab),
//! moves it to `Active`, *reserves* the wanted count in the descriptor's
//! `live` counter, and releases the lock. It then claims bits, hands out
//! what it got and gives back the unused reservation. Reserving before
//! claiming means `live` is never zero while a claim is in flight, so a
//! purge (which requires `live == 0` under the lock) can never discard
//! memory a thread is about to hand out, and a slab observed `Fresh` or
//! `Purged` at selection is guaranteed zero for the thread that selected it.
//!
//! A release clears bits with `fetch_and` and then subtracts from `live`.
//! If a word was full before the clear the releaser sets the partial bit
//! under the lock; if `live` reaches zero the releaser makes the slab
//! `Empty` under the lock. A refill that finds a partial slab full re-checks
//! `is_full` under the lock before clearing its partial bit, so a concurrent
//! release either is visible to that check or sets the bit afterwards.

use crate::bitmap::TwoLevelBitmap;
use crate::layout::{CLASSES, Class, Regions};
use crate::lock::{Lock, PAD_ALIGN, Padded, RawLock};
use crate::meta::Meta;
use crate::slab::{DESC_ALIGN, SlabDesc, State, desc_stride};
use crate::sys::Errno;

/// Counters kept under the class lock. Slab counts only; `curregs` and
/// byte totals are Phase 1 (`stats.rs`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassStats {
    /// Slabs committed for allocation (excludes guard slabs).
    pub committed: usize,
    /// Guard slabs committed (never handed out).
    pub guards: usize,
    /// Slabs currently `Empty`.
    pub empty: usize,
    /// Slabs currently `Purged`.
    pub purged: usize,
    /// Slabs in the partial set (jemalloc's `nonfull_slabs`).
    pub nonfull: usize,
}

/// Result of one [`ClassState::claim`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// Slots written to the output; fewer than asked means the region or
    /// the metadata region is exhausted or a commit failed.
    pub n: usize,
    /// Every returned slot came from a slab that was `Fresh` or `Purged`
    /// when this thread selected it, so its memory reads as zero.
    pub zeroed: bool,
}

/// A never-freed table in the metadata region, committed page by page as
/// the high-water mark grows. Shared with `huge.rs`.
pub(crate) struct Lazy {
    pub(crate) base: usize,
    len: usize,
    committed: usize,
}

impl Lazy {
    pub(crate) fn reserve(meta: &Meta, len: usize, align: usize) -> Option<Lazy> {
        let base = meta.reserve(len, align)?;
        Some(Lazy { base, len, committed: 0 })
    }

    /// Make the first `upto` bytes committed.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, upto: usize) -> Result<(), Errno> {
        let want = ((upto + page - 1) & !(page - 1)).min((self.len + page - 1) & !(page - 1));
        if want > self.committed {
            meta.commit(self.base + self.committed, want - self.committed)?;
            self.committed = want;
        }
        Ok(())
    }
}

/// A slab-number (or huge-page) set: a [`TwoLevelBitmap`] over a lazily
/// committed table. Shared with `huge.rs`.
pub(crate) struct Set {
    pub(crate) map: TwoLevelBitmap,
    table: Lazy,
    l1_words: usize,
}

impl Set {
    pub(crate) fn new(meta: &Meta, page: usize, nslabs: usize) -> Option<Set> {
        let l0_words = nslabs.div_ceil(64);
        let l1_words = l0_words.div_ceil(64);
        let mut table = Lazy::reserve(meta, 8 * TwoLevelBitmap::words_needed(nslabs), page)?;
        // Level 1 is small and touched by every operation: commit it now.
        table.ensure(meta, page, 8 * l1_words).ok()?;
        // SAFETY: the words come from a fresh, zero, never-freed reservation.
        let map = unsafe { TwoLevelBitmap::from_raw(nslabs, table.base as *mut u64) };
        Some(Set { map, table, l1_words })
    }

    /// Commit the level-0 words that hold every bit up to and including
    /// `slab`.
    pub(crate) fn ensure(&mut self, meta: &Meta, page: usize, slab: usize) -> Result<(), Errno> {
        self.table.ensure(meta, page, 8 * (self.l1_words + slab / 64 + 1))
    }
}

struct Inner {
    partial: Set,
    empty: Set,
    purged: Set,
    descs: Lazy,
    /// Next slab number to commit (`high_water + 1` in the design).
    next_slab: usize,
    stats: ClassStats,
}

// SAFETY: `TwoLevelBitmap` holds a raw pointer into the metadata region,
// which is process-global and only ever accessed through this lock.
unsafe impl Send for Inner {}

/// State of one `(arena, class)` pair.
///
/// Layout matters: the fields before `inner` are written once by `new` (or,
/// for `committed_slabs`, only when a slab is committed) and read by every
/// `free`, while `inner` holds the class lock and the state every refill and
/// flush writes. `repr(C)` keeps the read-mostly fields first and
/// [`Padded`] starts `inner` on its own line, so a lock acquisition on one
/// core does not evict the descriptor base and commit watermark from the
/// others. The alignment makes `[ClassState; NCLASSES]` line-granular too.
///
/// The stride of that array must not be a multiple of 512 bytes. The
/// natural size (128 bytes of read-mostly fields plus 384 for the padded
/// lock) is exactly 512, and with 4 KiB pages that puts the hot line of
/// every class on the same handful of L1 sets: on Apple silicon (128-byte
/// lines, sets indexed by page-offset bits) single-threaded `alloc-test`
/// slowed from 2.13 s to 2.52 s. `_tail` makes the stride 640 = 5 lines,
/// which visits every set. See `bench/results/2026-09-16-aarch64-*.md`.
#[repr(C, align(128))]
pub struct ClassState {
    class: u16,
    arena: u16,
    /// Every `guard_interval`-th slab is a guard slab; 0 disables guards.
    guard_interval: u32,
    lg_page: u32,
    /// Start of this class's region.
    base: usize,
    /// Number of slabs in the region.
    nslabs: usize,
    /// Start of the descriptor array.
    desc_base: usize,
    /// Slabs with a committed descriptor, readable without the lock so the
    /// `free` check can reject pointers into never-committed slabs cleanly.
    committed_slabs: core::sync::atomic::AtomicUsize,
    inner: Padded<Lock<Inner>>,
    _tail: [u8; PAD_ALIGN],
}

const _: () = assert!(core::mem::offset_of!(ClassState, inner) == PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, committed_slabs) + 8 <= PAD_ALIGN);
const _: () = assert!(core::mem::size_of::<ClassState>() % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>() % 512 != 0);

#[cfg(target_os = "linux")]
mod os {
    use crate::sys::{self, Errno, MADV_DONTNEED, MADV_GUARD_INSTALL, PROT_NONE, PROT_READ, PROT_WRITE};

    pub fn commit(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
    }

    pub fn decommit(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::mprotect(addr, len, PROT_NONE) }
    }

    pub fn guard(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
    }

    pub fn purge(addr: usize, len: usize) -> Result<(), Errno> {
        unsafe { sys::madvise(addr, len, MADV_DONTNEED) }
    }

    pub fn now_ns() -> u64 {
        sys::now_coarse_ns()
    }
}

/// Off Linux nothing maps memory; host tests exercise the state machine
/// over addresses that are never dereferenced.
#[cfg(not(target_os = "linux"))]
mod os {
    use crate::sys::Errno;
    use core::sync::atomic::{AtomicU64, Ordering};

    pub fn commit(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn decommit(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn guard(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn purge(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }
}

impl Inner {
    fn set_partial(&mut self, slab: usize) {
        if !self.partial.map.is_set(slab) {
            self.partial.map.set(slab);
            self.stats.nonfull += 1;
        }
    }

    fn clear_partial(&mut self, slab: usize) {
        if self.partial.map.is_set(slab) {
            self.partial.map.clear(slab);
            self.stats.nonfull -= 1;
        }
    }
}

impl ClassState {
    /// Create the state for `class` in `arena`. Reserves the descriptor
    /// array and the three slab sets in `meta`; returns `None` if the
    /// metadata region cannot hold them. `guard_interval` of `N > 0` makes
    /// every `N`-th slab a guard slab; callers must pass 0 when
    /// `MADV_GUARD_INSTALL` is unavailable, since a `PROT_NONE` slab would
    /// split the region into one VMA per guard (design: Phase 2 task 1).
    pub fn new(
        meta: &Meta,
        regions: &Regions,
        class: usize,
        arena: u16,
        guard_interval: u32,
    ) -> Option<ClassState> {
        assert!(class < CLASSES.len(), "class: index out of range");
        assert!(guard_interval != 1, "class: a guard interval of 1 leaves no slabs to allocate");
        let page = regions.page_size();
        let nslabs = regions.nslabs(class);
        let descs = Lazy::reserve(meta, nslabs * desc_stride(class), DESC_ALIGN.max(page))?;
        let partial = Set::new(meta, page, nslabs)?;
        let empty = Set::new(meta, page, nslabs)?;
        let purged = Set::new(meta, page, nslabs)?;
        Some(ClassState {
            class: class as u16,
            arena,
            guard_interval,
            lg_page: regions.lg_page,
            base: regions.class_base[class],
            nslabs,
            desc_base: descs.base,
            committed_slabs: core::sync::atomic::AtomicUsize::new(0),
            inner: Padded(Lock::new(Inner {
                partial,
                empty,
                purged,
                descs,
                next_slab: 0,
                stats: ClassStats::default(),
            })),
            _tail: [0; PAD_ALIGN],
        })
    }

    #[inline]
    pub fn class_index(&self) -> usize {
        self.class as usize
    }

    #[inline]
    pub fn class(&self) -> &'static Class {
        &CLASSES[self.class as usize]
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    pub fn nslabs(&self) -> usize {
        self.nslabs
    }

    /// The class lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    /// Whether `slab` is reserved as a guard slab.
    #[inline]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        self.guard_interval != 0 && (slab + 1) % self.guard_interval as usize == 0
    }

    /// Number of slabs (including guard slabs) whose descriptors exist.
    #[inline]
    pub fn committed_slabs(&self) -> usize {
        self.committed_slabs.load(core::sync::atomic::Ordering::Acquire)
    }

    /// Descriptor of a committed slab. Aborts for a slab that was never
    /// committed rather than faulting on its `PROT_NONE` descriptor page.
    #[inline]
    pub fn desc(&self, slab: usize) -> &SlabDesc {
        if slab >= self.committed_slabs() {
            never_committed(self.class as usize, slab);
        }
        // SAFETY: descriptors up to `committed_slabs` are committed and
        // initialised (or zero, for guard slabs) before the counter moves.
        unsafe { SlabDesc::at(self.desc_base, self.class as usize, slab) }
    }

    #[inline]
    fn slab_addr(&self, slab: usize) -> usize {
        self.base + (slab << self.class().lg_slab)
    }

    #[inline]
    fn slot_ptr(&self, slab: usize, slot: usize) -> *mut u8 {
        (self.slab_addr(slab) + slot * self.class().size()) as *mut u8
    }

    /// Slab and slot of a pointer that must be a slot start in this class's
    /// region. Aborts otherwise.
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (usize, usize) {
        let cls = self.class();
        let rel = (p as usize).wrapping_sub(self.base);
        if rel >= self.nslabs << cls.lg_slab {
            invalid_pointer(p as usize, self.class as usize, "outside the class region");
        }
        let (slot, valid) = cls.slot_of_offset(rel & (cls.slab_size() - 1));
        if !valid {
            invalid_pointer(p as usize, self.class as usize, "not a slot start");
        }
        (rel >> cls.lg_slab, slot)
    }

    /// The `free` check: `p` must be a slot start whose bit is set.
    #[inline]
    pub fn check(&self, p: *mut u8) {
        let (slab, slot) = self.locate(p);
        self.desc(slab).check(self.class(), slot, p as usize, self.class as usize, slab);
    }

    /// Claim up to `out.len()` slots, lowest address first, writing slot
    /// pointers to `out[..n]`. Loops over slabs until satisfied or until
    /// nothing more can be committed.
    pub fn claim(&self, meta: &Meta, out: &mut [*mut u8]) -> Claim {
        let cls = self.class();
        let mut got = 0;
        let mut zeroed = true;
        while got < out.len() {
            let want = (out.len() - got).min(cls.nslots());
            let Some((slab, zero)) = self.select(meta, want) else { break };
            let desc = self.desc(slab);
            let bm = desc.bitmap(cls);
            let words = bm.words();
            let mut k = 0;
            let mut word = 0;
            while k < want {
                let Some((w, bits)) = bm.claim_from(word, want - k) else { break };
                let mut m = bits;
                while m != 0 {
                    let b = m.trailing_zeros() as usize;
                    m &= m - 1;
                    out[got + k] = self.slot_ptr(slab, w * 64 + b);
                    k += 1;
                }
                word = if w + 1 == words { 0 } else { w + 1 };
            }
            if k < want {
                // Return the unused reservation, then the slab is full (or
                // was emptied under us): fix up its set membership.
                self.unreserve(slab, want - k);
                self.on_full(slab);
            }
            if k > 0 && !zero {
                zeroed = false;
            }
            got += k;
        }
        Claim { n: got, zeroed: got > 0 && zeroed }
    }

    /// Pick a slab for a refill and reserve `want` in its `live` counter.
    /// Returns the slab and whether its memory is known zero.
    fn select(&self, meta: &Meta, want: usize) -> Option<(usize, bool)> {
        let mut g = self.inner.lock();
        let slab = if let Some(s) = g.partial.map.lowest_set() {
            s
        } else if let Some(s) = g.empty.map.lowest_set() {
            g.empty.map.clear(s);
            g.stats.empty -= 1;
            g.set_partial(s);
            s
        } else if let Some(s) = g.purged.map.lowest_set() {
            g.purged.map.clear(s);
            g.stats.purged -= 1;
            g.set_partial(s);
            s
        } else {
            let s = self.commit_next(&mut g, meta)?;
            g.set_partial(s);
            s
        };
        let desc = self.desc(slab);
        let state = desc.state();
        if state != State::Active {
            desc.set_state(State::Active);
            desc.set_dirty_at(0);
        }
        desc.add_live(want);
        Some((slab, state.is_zero()))
    }

    /// Commit the next non-guard slab (and any guard slabs before it) and
    /// initialise its descriptor. `None` when the region or the metadata
    /// region is exhausted or `mprotect` fails.
    fn commit_next(&self, g: &mut Inner, meta: &Meta) -> Option<usize> {
        let cls = self.class();
        let page = 1usize << self.lg_page;
        loop {
            let s = g.next_slab;
            if s >= self.nslabs {
                return None;
            }
            g.descs.ensure(meta, page, (s + 1) * desc_stride(self.class as usize)).ok()?;
            g.partial.ensure(meta, page, s).ok()?;
            g.empty.ensure(meta, page, s).ok()?;
            g.purged.ensure(meta, page, s).ok()?;
            let addr = self.slab_addr(s);
            let len = cls.slab_size();
            os::commit(addr, len).ok()?;
            if self.is_guard_slab(s) {
                // Read-write so the VMA merges with its neighbours, with a
                // guard region on the first page so a stray write past the
                // previous slab faults. If the install fails, fall back to
                // leaving the whole slab `PROT_NONE`: a hard guard at the
                // cost of a VMA split.
                if os::guard(addr, page).is_err() && os::decommit(addr, len).is_err() {
                    panic!("class {}: cannot re-protect guard slab {s}", self.class);
                }
                g.next_slab = s + 1;
                g.stats.guards += 1;
                self.committed_slabs.store(s + 1, core::sync::atomic::Ordering::Release);
                continue;
            }
            // SAFETY: the descriptor page for `s` was committed above.
            unsafe { SlabDesc::at(self.desc_base, self.class as usize, s) }.init(cls, self.arena);
            g.next_slab = s + 1;
            g.stats.committed += 1;
            self.committed_slabs.store(s + 1, core::sync::atomic::Ordering::Release);
            return Some(s);
        }
    }

    /// Give back an unused claim reservation.
    fn unreserve(&self, slab: usize, n: usize) {
        if n > 0 && self.desc(slab).sub_live(n) == n {
            self.on_live_zero(slab);
        }
    }

    /// A refill observed `slab` full: drop it from the partial set if it
    /// still is full under the lock.
    fn on_full(&self, slab: usize) {
        let mut g = self.inner.lock();
        let desc = self.desc(slab);
        if desc.state() == State::Active && desc.bitmap(self.class()).is_full() {
            g.clear_partial(slab);
        }
    }

    /// `live` reached zero: make the slab `Empty` unless a refill has
    /// already reserved it again.
    fn on_live_zero(&self, slab: usize) {
        let mut g = self.inner.lock();
        let desc = self.desc(slab);
        if desc.live() == 0 && desc.state() == State::Active {
            desc.set_state(State::Empty);
            desc.set_dirty_at(os::now_ns().max(1));
            g.clear_partial(slab);
            g.empty.map.set(slab);
            g.stats.empty += 1;
        }
    }

    /// Release a batch of slot pointers. Consecutive pointers in the same
    /// bitmap word share one `fetch_and`, so a batch sorted by address costs
    /// about one atomic per 64 slots. Aborts on a pointer that is not an
    /// allocated slot of this class.
    pub fn release(&self, ptrs: &[*mut u8]) {
        let mut cur_slab = usize::MAX;
        let mut cur_word = 0usize;
        let mut mask = 0u64;
        let mut count = 0usize;
        for &p in ptrs {
            let (slab, slot) = self.locate(p);
            let word = slot / 64;
            if slab != cur_slab || word != cur_word {
                if mask != 0 {
                    self.release_word(cur_slab, cur_word, mask);
                }
                if slab != cur_slab && count != 0 {
                    self.dec_live(cur_slab, count);
                    count = 0;
                }
                cur_slab = slab;
                cur_word = word;
                mask = 0;
            }
            let bit = 1u64 << (slot % 64);
            if mask & bit != 0 {
                double_free(p as usize, self.class as usize, slab, slot);
            }
            mask |= bit;
            count += 1;
        }
        if mask != 0 {
            self.release_word(cur_slab, cur_word, mask);
        }
        if count != 0 {
            self.dec_live(cur_slab, count);
        }
    }

    /// Release the slots of `mask` in `word` of `slab`. The `live` update
    /// is separate so a batch can do one per slab.
    fn release_word(&self, slab: usize, word: usize, mask: u64) {
        let desc = self.desc(slab);
        let prev = desc.bitmap(self.class()).release(word, mask);
        if prev & mask != mask {
            let bad = (mask & !prev).trailing_zeros() as usize;
            let slot = word * 64 + bad;
            double_free(self.slot_ptr(slab, slot) as usize, self.class as usize, slab, slot);
        }
        if prev == !0 {
            let mut g = self.inner.lock();
            g.set_partial(slab);
        }
    }

    fn dec_live(&self, slab: usize, n: usize) {
        if self.desc(slab).sub_live(n) == n {
            self.on_live_zero(slab);
        }
    }

    /// Purge every `Empty` slab whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all of them): `MADV_DONTNEED`, then `Purged`. Returns
    /// the number purged. Phase 1 adds batching and the decay driver.
    pub fn purge_empty(&self, cutoff: u64) -> usize {
        let cls = self.class();
        let mut g = self.inner.lock();
        let mut n = 0;
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next {
            let desc = self.desc(slab);
            assert!(
                desc.state() == State::Empty && desc.live() == 0,
                "class {}: slab {slab} in the empty set is not empty",
                self.class
            );
            if desc.dirty_at() <= cutoff {
                if let Err(e) = os::purge(self.slab_addr(slab), cls.slab_size()) {
                    panic!("class {}: madvise(MADV_DONTNEED) of slab {slab} failed: {e}", self.class);
                }
                desc.set_state(State::Purged);
                desc.set_dirty_at(0);
                g.empty.map.clear(slab);
                g.purged.map.set(slab);
                g.stats.empty -= 1;
                g.stats.purged += 1;
                n += 1;
            }
            next = g.empty.map.lowest_set_from(slab + 1);
        }
        n
    }

    pub fn stats(&self) -> ClassStats {
        self.inner.lock().stats
    }

    /// Lowest slab in the partial set, if any (for tests and stats).
    pub fn lowest_partial(&self) -> Option<usize> {
        self.inner.lock().partial.map.lowest_set()
    }

    /// Whether `slab` is in the partial set.
    pub fn is_partial(&self, slab: usize) -> bool {
        self.inner.lock().partial.map.is_set(slab)
    }
}

#[cold]
#[inline(never)]
fn never_committed(class: usize, slab: usize) -> ! {
    panic!("invalid free: class {class} slab {slab} was never allocated")
}

#[cold]
#[inline(never)]
fn invalid_pointer(p: usize, class: usize, why: &str) -> ! {
    panic!("invalid free of {p:#x} (class {class}): {why}")
}

#[cold]
#[inline(never)]
fn double_free(p: usize, class: usize, slab: usize, slot: usize) -> ! {
    panic!("double free of {p:#x} (class {class} slab {slab} slot {slot})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Kind, LG_MAX_SLAB, MAX_SLAB, Mapping, NCLASSES};
    use core::ptr;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::vec;
    use std::vec::Vec;

    const PAGE: usize = 4096;
    /// Smallest layout `from_parts` accepts: 4 MiB class regions, so the
    /// 64 KiB classes have 64 slabs and the 4 MiB class has one.
    const LG_REGION: u32 = LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    const META_LEN: usize = 8 << 20;

    /// Test environment: a metadata region and a class superblock. On Linux
    /// both are real `PROT_NONE` reservations so commits and purges go
    /// through the kernel and slot memory can be touched; elsewhere the
    /// metadata is a `Vec` and slot addresses are never dereferenced.
    struct Env {
        meta: Meta,
        regions: Regions,
        #[cfg(target_os = "linux")]
        maps: [(usize, usize); 2],
        #[cfg(not(target_os = "linux"))]
        _buf: Vec<u8>,
    }

    fn align_up(x: usize, a: usize) -> usize {
        (x + a - 1) & !(a - 1)
    }

    fn regions_at(superblock: usize, meta: (usize, usize)) -> Regions {
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
        let huge = (superblock + sb_len + (1 << 30), 1usize << 34);
        Regions::from_parts(
            superblock,
            LG_REGION,
            LG_STRIDE,
            &[0usize; NCLASSES],
            huge,
            meta,
            PAGE.trailing_zeros(),
            [
                Mapping { addr: superblock, len: sb_len },
                Mapping { addr: huge.0, len: huge.1 },
                Mapping { addr: meta.0, len: meta.1 },
            ],
        )
    }

    impl Env {
        #[cfg(target_os = "linux")]
        fn new() -> Env {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let reserve = |len: usize| -> usize {
                unsafe {
                    crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
                }
                .expect("reserve")
            };
            let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
            let sb = reserve(sb_len);
            let mb = reserve(META_LEN);
            let superblock = align_up(sb, MAX_SLAB);
            let regions = regions_at(superblock, (mb, META_LEN));
            Env { meta: Meta::new(mb, META_LEN, PAGE), regions, maps: [(sb, sb_len), (mb, META_LEN)] }
        }

        #[cfg(not(target_os = "linux"))]
        fn new() -> Env {
            let buf = vec![0u8; META_LEN + PAGE];
            let mb = align_up(buf.as_ptr() as usize, PAGE);
            let regions = regions_at(0x1000_0000_0000, (mb, META_LEN));
            Env { meta: Meta::new(mb, META_LEN, PAGE), regions, _buf: buf }
        }

        fn class(&self, c: usize, guard_interval: u32) -> ClassState {
            ClassState::new(&self.meta, &self.regions, c, 0, guard_interval).expect("class state")
        }

        fn claim(&self, cs: &ClassState, n: usize) -> (Vec<*mut u8>, Claim) {
            let mut out = vec![ptr::null_mut(); n];
            let c = cs.claim(&self.meta, &mut out);
            out.truncate(c.n);
            (out, c)
        }
    }

    /// Regression: with 2^21 members the level-1 table is exactly one page,
    /// so level 0 starts on a page that `Set::new` leaves uncommitted (the
    /// real huge region has the same shape at 2^28 pages). Scans of an
    /// empty or sparsely populated set must never touch those pages.
    #[cfg(target_os = "linux")]
    #[test]
    fn set_scans_never_read_uncommitted_words() {
        let env = Env::new();
        let nbits = 1 << 21;
        let mut set = Set::new(&env.meta, PAGE, nbits).expect("set");
        assert_eq!(set.map.lowest_set(), None);
        assert_eq!(set.map.lowest_set_from(0), None);
        assert_eq!(set.map.lowest_set_from(12345), None);
        assert_eq!(set.map.lowest_set_from(nbits - 1), None);
        set.ensure(&env.meta, PAGE, 100).unwrap();
        set.map.set(100);
        assert_eq!(set.map.lowest_set_from(0), Some(100));
        assert_eq!(set.map.lowest_set_from(100), Some(100));
        assert_eq!(set.map.lowest_set_from(101), None);
        // Far beyond the committed prefix, in a level-1 group of its own.
        assert_eq!(set.map.lowest_set_from(64 * 64 * 100), None);
        set.map.clear(100);
        assert_eq!(set.map.lowest_set_from(0), None);
    }

    #[cfg(target_os = "linux")]
    impl Drop for Env {
        fn drop(&mut self) {
            for (addr, len) in self.maps {
                unsafe { crate::sys::munmap(addr, len) }.unwrap();
            }
        }
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// A class with 64 KiB slabs and a handful of bitmap words.
    fn small_class() -> usize {
        // 64 bytes: 1024 slots, 16 words (quantum16) or 1024 slots (quantum8).
        CLASSES.iter().position(|c| c.size() == 64).unwrap()
    }

    #[test]
    fn first_claim_is_lowest_addresses_and_zero() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let cls = cs.class();
        assert_eq!(cs.committed_slabs(), 0);
        let (p, claim) = env.claim(&cs, 10);
        assert_eq!(claim, Claim { n: 10, zeroed: true });
        for (i, &q) in p.iter().enumerate() {
            assert_eq!(q as usize, env.regions.slab_addr(c, 0) + i * cls.size());
            assert_eq!(
                env.regions.classify(q as usize),
                Kind::Slab { class: c as u32, slab: 0, slot: i as u32 }
            );
            cs.check(q);
            assert_eq!(cs.locate(q), (0, i));
        }
        let d = cs.desc(0);
        assert_eq!(d.state(), State::Active);
        assert_eq!(d.live(), 10);
        assert_eq!(d.arena(), 0);
        assert_eq!(cs.committed_slabs(), 1);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
        // A second claim from the same, now Active, slab is not known zero.
        let (p2, claim2) = env.claim(&cs, 5);
        assert_eq!(claim2, Claim { n: 5, zeroed: false });
        assert_eq!(p2[0] as usize, env.regions.slot_addr(c, 0, 10));
        assert_eq!(d.live(), 15);
    }

    #[test]
    fn filling_a_slab_moves_on_and_release_reopens_it() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        let (all, claim) = env.claim(&cs, n);
        assert_eq!(claim.n, n);
        let set: HashSet<usize> = all.iter().map(|&p| p as usize).collect();
        assert_eq!(set.len(), n);
        assert!(cs.desc(0).bitmap(cs.class()).is_full());
        // Still in partial until a refill notices.
        assert!(cs.is_partial(0));
        let (next, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 1, zeroed: true });
        assert_eq!(next[0] as usize, env.regions.slab_addr(c, 1));
        assert!(!cs.is_partial(0));
        assert!(cs.is_partial(1));
        assert_eq!(cs.stats().committed, 2);
        assert_eq!(cs.stats().nonfull, 1);
        // Releasing one slot in slab 0 puts it back, and first fit picks it
        // before slab 1.
        let victim = all[n / 2];
        cs.release(&[victim]);
        assert!(cs.is_partial(0));
        assert_eq!(cs.lowest_partial(), Some(0));
        assert_eq!(cs.desc(0).live(), n - 1);
        assert!(!cs.desc(0).is_allocated(cs.class(), n / 2));
        let (again, claim) = env.claim(&cs, 2);
        assert_eq!(claim, Claim { n: 2, zeroed: false });
        assert_eq!(again[0], victim);
        assert_eq!(again[1] as usize, env.regions.slot_addr(c, 1, 1));
    }

    #[test]
    fn release_all_makes_empty_then_reuse_then_purge() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let (p, _) = env.claim(&cs, 100);
        // Release in reverse order to exercise the run grouping.
        let mut rev = p.clone();
        rev.reverse();
        cs.release(&rev);
        let d = cs.desc(0);
        assert_eq!(d.state(), State::Empty);
        assert_eq!(d.live(), 0);
        assert!(d.dirty_at() > 0);
        assert!(!cs.is_partial(0));
        assert_eq!(cs.stats(), ClassStats { committed: 1, empty: 1, ..Default::default() });
        // An Empty slab is preferred over committing a new one but is dirty.
        let (q, claim) = env.claim(&cs, 3);
        assert_eq!(claim, Claim { n: 3, zeroed: false });
        assert_eq!(q[0], p[0]);
        assert_eq!(d.state(), State::Active);
        assert_eq!(d.dirty_at(), 0);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
        cs.release(&q);
        assert_eq!(d.state(), State::Empty);
        // Purge with a cutoff before dirty_at does nothing; after it purges.
        assert_eq!(cs.purge_empty(d.dirty_at() - 1), 0);
        assert_eq!(d.state(), State::Empty);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        assert_eq!(d.state(), State::Purged);
        assert_eq!(d.dirty_at(), 0);
        assert_eq!(cs.stats(), ClassStats { committed: 1, purged: 1, ..Default::default() });
        // A Purged slab is reused before a new commit and is zero.
        let (r, claim) = env.claim(&cs, 4);
        assert_eq!(claim, Claim { n: 4, zeroed: true });
        assert_eq!(r[0], p[0]);
        assert_eq!(d.state(), State::Active);
        assert_eq!(cs.stats(), ClassStats { committed: 1, nonfull: 1, ..Default::default() });
    }

    #[test]
    fn claim_spans_slabs_and_reports_shortfall_on_exhaustion() {
        let env = Env::new();
        // The largest class: one 4 MiB slab of 16 slots in this tiny region.
        let c = NCLASSES - 1;
        let cs = env.class(c, 0);
        assert_eq!(cs.nslabs(), 1);
        assert_eq!(cs.class().nslots(), 16);
        let (p, claim) = env.claim(&cs, 20);
        assert_eq!(claim, Claim { n: 16, zeroed: true });
        assert_eq!(p.len(), 16);
        let (q, claim) = env.claim(&cs, 1);
        assert_eq!(claim, Claim { n: 0, zeroed: false });
        assert!(q.is_empty());
        cs.release(&p[..8]);
        let (q, claim) = env.claim(&cs, 20);
        assert_eq!(claim, Claim { n: 8, zeroed: false });
        assert_eq!(q, p[..8].to_vec());
        // A class with 64 slabs: ask for more than one slab holds.
        let c = small_class();
        let cs = env.class(c, 0);
        let n = cs.class().nslots();
        let (p, claim) = env.claim(&cs, 2 * n + 5);
        assert_eq!(claim, Claim { n: 2 * n + 5, zeroed: true });
        assert_eq!(cs.stats().committed, 3);
        assert_eq!(cs.stats().nonfull, 1);
        assert_eq!(p[2 * n] as usize, env.regions.slab_addr(c, 2));
    }

    #[test]
    fn guard_slabs_are_skipped() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 4);
        for s in 0..16 {
            assert_eq!(cs.is_guard_slab(s), s % 4 == 3, "slab {s}");
        }
        let n = cs.class().nslots();
        let (p, claim) = env.claim(&cs, 6 * n);
        assert_eq!(claim.n, 6 * n);
        let slabs: HashSet<usize> = p.iter().map(|&q| cs.locate(q).0).collect();
        assert_eq!(slabs, [0, 1, 2, 4, 5, 6].into_iter().collect());
        // Slabs 0..=7 are committed: six allocatable and two guards.
        assert_eq!(cs.committed_slabs(), 7);
        let (q, _) = env.claim(&cs, 1);
        assert_eq!(cs.locate(q[0]).0, 8);
        assert_eq!(cs.committed_slabs(), 9);
        assert_eq!(cs.stats().committed, 7);
        assert_eq!(cs.stats().guards, 2);
        // A guard slab's descriptor is zero: any free into it aborts.
        assert_eq!(cs.desc(3).state(), State::Fresh);
        assert_eq!(cs.desc(3).live(), 0);
    }

    #[test]
    fn works_for_every_class() {
        let env = Env::new();
        for c in 0..NCLASSES {
            let cs = env.class(c, 0);
            let cls = cs.class();
            let want = cls.nslots().min(70);
            let (p, claim) = env.claim(&cs, want);
            assert_eq!(claim, Claim { n: want, zeroed: true }, "class {c}");
            for (i, &q) in p.iter().enumerate() {
                assert_eq!(
                    env.regions.classify(q as usize),
                    Kind::Slab { class: c as u32, slab: 0, slot: i as u32 },
                    "class {c}"
                );
                cs.check(q);
            }
            cs.release(&p);
            assert_eq!(cs.desc(0).state(), State::Empty, "class {c}");
            assert_eq!(cs.purge_empty(u64::MAX), 1);
        }
        let s = env.meta.stats();
        assert!(s.reserved > 0 && s.committed > 0);
    }

    #[test]
    fn metadata_exhaustion_is_reported_not_fatal() {
        // A metadata region too small for the smallest class's descriptor
        // array: 64 slabs * stride is small, so shrink the region to a
        // couple of pages instead.
        #[cfg(not(target_os = "linux"))]
        let (meta, _buf) = {
            let buf = vec![0u8; 3 * PAGE];
            let mb = align_up(buf.as_ptr() as usize, PAGE);
            (Meta::new(mb, 2 * PAGE, PAGE), buf)
        };
        #[cfg(target_os = "linux")]
        let meta = {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let mb = unsafe {
                crate::sys::mmap(0, 2 * PAGE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
            }
            .unwrap();
            Meta::new(mb, 2 * PAGE, PAGE)
        };
        let regions = regions_at(0x1000_0000_0000, (0x2000_0000_0000, 2 * PAGE));
        assert!(ClassState::new(&meta, &regions, 0, 0, 0).is_none());
    }

    #[test]
    #[should_panic(expected = "double free of")]
    fn double_release_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 3);
        cs.release(&p[..1]);
        cs.release(&p[..1]);
    }

    #[test]
    #[should_panic(expected = "double free of")]
    fn duplicate_in_one_batch_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 3);
        cs.release(&[p[1], p[1]]);
    }

    #[test]
    #[should_panic(expected = "not a slot start")]
    fn release_of_interior_pointer_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 1);
        cs.release(&[unsafe { p[0].add(1) }]);
    }

    #[test]
    #[should_panic(expected = "outside the class region")]
    fn release_outside_region_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        env.claim(&cs, 1);
        cs.release(&[env.regions.huge_base as *mut u8]);
    }

    #[test]
    #[should_panic(expected = "was never allocated")]
    fn check_of_uncommitted_slab_aborts() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        env.claim(&cs, 1);
        cs.check(env.regions.slab_addr(c, 5) as *mut u8);
    }

    #[test]
    #[should_panic(expected = "invalid or double free of")]
    fn check_of_unclaimed_slot_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 2);
        cs.check(unsafe { p[1].add(cs.class().size()) });
    }

    #[test]
    #[should_panic(expected = "invalid or double free of")]
    fn check_after_release_aborts() {
        let env = Env::new();
        let cs = env.class(small_class(), 0);
        let (p, _) = env.claim(&cs, 2);
        cs.release(&p[..1]);
        cs.check(p[0]);
    }

    /// Randomised single-thread model check: live pointers are unique,
    /// valid, and the descriptor counts match; after draining, every slab
    /// is Empty and purging leaves everything Purged.
    #[test]
    fn randomised_model_check() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let cls = cs.class();
        let mut rng = 0xC0FF_EE12_3456_789Au64;
        let mut live: Vec<*mut u8> = Vec::new();
        let mut set: HashSet<usize> = HashSet::new();
        for _ in 0..4000 {
            let r = xorshift(&mut rng);
            if r % 2 == 0 && !live.is_empty() {
                // Release a random batch; claims and releases are balanced so
                // the live set random-walks well inside the 64-slab region.
                let k = (xorshift(&mut rng) as usize % 70 + 1).min(live.len());
                let mut batch = Vec::with_capacity(k);
                for _ in 0..k {
                    let i = xorshift(&mut rng) as usize % live.len();
                    let p = live.swap_remove(i);
                    assert!(set.remove(&(p as usize)));
                    batch.push(p);
                }
                if xorshift(&mut rng) % 2 == 0 {
                    batch.sort();
                }
                cs.release(&batch);
            } else {
                let k = xorshift(&mut rng) as usize % 70 + 1;
                let (p, claim) = env.claim(&cs, k);
                assert_eq!(claim.n, k);
                for &q in &p {
                    assert!(set.insert(q as usize), "duplicate pointer {q:p}");
                    assert!(matches!(
                        env.regions.classify(q as usize),
                        Kind::Slab { class, .. } if class as usize == c
                    ));
                    cs.check(q);
                }
                live.extend(p);
            }
            // Invariants after every step.
            let mut total_live = 0;
            let st = cs.stats();
            let mut empty = 0;
            for s in 0..cs.committed_slabs() {
                let d = cs.desc(s);
                total_live += d.live();
                let bm = d.bitmap(cls);
                let set_bits: usize = (0..bm.words()).map(|w| bm.load(w).count_ones() as usize).sum();
                assert_eq!(set_bits - (bm.words() * 64 - cls.nslots()), d.live(), "slab {s}");
                match d.state() {
                    State::Empty => {
                        assert_eq!(d.live(), 0);
                        assert!(!cs.is_partial(s));
                        empty += 1;
                    }
                    State::Active => {
                        assert!(d.live() > 0 || cs.is_partial(s));
                        if !bm.is_full() {
                            assert!(cs.is_partial(s), "slab {s} has free slots but is not partial");
                        }
                    }
                    other => panic!("slab {s} in state {other:?}"),
                }
            }
            assert_eq!(total_live, live.len());
            assert_eq!(st.empty, empty);
        }
        live.sort();
        cs.release(&live);
        let st = cs.stats();
        assert_eq!(st.empty, st.committed);
        assert_eq!(st.nonfull, 0);
        assert_eq!(cs.purge_empty(u64::MAX), st.committed);
        let st = cs.stats();
        assert_eq!(st.purged, st.committed);
        assert_eq!(st.empty, 0);
        for s in 0..cs.committed_slabs() {
            assert_eq!(cs.desc(s).state(), State::Purged);
        }
        // Everything is reusable afterwards, from slab 0, and zero.
        let (p, claim) = env.claim(&cs, 3);
        assert_eq!(claim, Claim { n: 3, zeroed: true });
        assert_eq!(p[0] as usize, env.regions.slab_addr(c, 0));
    }

    /// Eight threads claim and release concurrently; no pointer is ever
    /// held by two threads at once and the counters settle.
    #[test]
    fn concurrent_claims_are_disjoint() {
        let env = Arc::new(Env::new());
        let c = small_class();
        let cs = Arc::new(env.class(c, 0));
        let threads = 8;
        let rounds = 300;
        let mut handles = Vec::new();
        for t in 0..threads {
            let env = Arc::clone(&env);
            let cs = Arc::clone(&cs);
            handles.push(std::thread::spawn(move || {
                let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
                let mut held: Vec<*mut u8> = Vec::new();
                let mut seen: Vec<usize> = Vec::new();
                for _ in 0..rounds {
                    let k = xorshift(&mut rng) as usize % 50 + 1;
                    let (p, claim) = env.claim(&cs, k);
                    assert_eq!(claim.n, k);
                    for &q in &p {
                        cs.check(q);
                        seen.push(q as usize);
                    }
                    held.extend(p);
                    if xorshift(&mut rng) % 4 != 0 {
                        let keep = xorshift(&mut rng) as usize % (held.len() + 1);
                        let mut batch = held.split_off(keep);
                        batch.sort();
                        cs.release(&batch);
                    }
                }
                held.sort();
                cs.release(&held);
                seen
            }));
        }
        // Pointers held simultaneously by two threads would show up as a
        // slot claimed twice without an intervening release; we approximate
        // that check by verifying the final state is consistent and that
        // per-thread `check` never aborted (it runs on every claimed slot).
        let mut total = 0;
        for h in handles {
            total += h.join().unwrap().len();
        }
        assert!(total > 0);
        let st = cs.stats();
        assert_eq!(st.empty, st.committed, "{st:?}");
        assert_eq!(st.nonfull, 0);
        for s in 0..cs.committed_slabs() {
            let d = cs.desc(s);
            assert_eq!(d.state(), State::Empty, "slab {s}");
            assert_eq!(d.live(), 0, "slab {s}");
            assert!(!d.bitmap(cs.class()).is_full());
            for slot in 0..cs.class().nslots() {
                assert!(!d.is_allocated(cs.class(), slot));
            }
        }
    }

    /// Two threads hammer one slot's worth of address space: one claims and
    /// immediately releases, the other purges whenever it can. The
    /// reservation-first protocol must keep every claimed slot readable and
    /// writable and never let a purge race a claim.
    #[cfg(target_os = "linux")]
    #[test]
    fn purge_never_races_a_claim() {
        let env = Arc::new(Env::new());
        let c = small_class();
        let cs = Arc::new(env.class(c, 0));
        let stop = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let purger = {
            let cs = Arc::clone(&cs);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(core::sync::atomic::Ordering::Relaxed) {
                    n += cs.purge_empty(u64::MAX);
                }
                n
            })
        };
        let mut rng = 1u64;
        for i in 0..20_000u32 {
            let k = xorshift(&mut rng) as usize % 8 + 1;
            let (p, claim) = env.claim(&cs, k);
            assert_eq!(claim.n, k);
            for &q in &p {
                unsafe {
                    if claim.zeroed {
                        assert_eq!(*(q as *const u32), 0, "iteration {i}: purged slot not zero");
                    }
                    *(q as *mut u32) = i;
                }
            }
            for &q in &p {
                assert_eq!(unsafe { *(q as *const u32) }, i, "iteration {i}: slot lost its value");
            }
            cs.release(&p);
        }
        stop.store(true, core::sync::atomic::Ordering::Relaxed);
        let purges = purger.join().unwrap();
        assert!(purges > 0, "the purger never found an empty slab");
        assert_eq!(cs.desc(0).live(), 0);
    }

    /// Purged memory reads zero and the claim reports it; committed slabs
    /// show up read-write in `/proc/self/maps`.
    #[cfg(target_os = "linux")]
    #[test]
    fn purge_zeroes_memory_and_commit_is_visible() {
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 0);
        let (p, claim) = env.claim(&cs, 5);
        assert!(claim.zeroed);
        for &q in &p {
            unsafe {
                assert_eq!(*q, 0);
                *q = 0xAB;
            }
        }
        cs.release(&p);
        let (q, claim) = env.claim(&cs, 5);
        assert_eq!(q, p);
        assert!(!claim.zeroed);
        assert_eq!(unsafe { *q[0] }, 0xAB);
        cs.release(&q);
        assert_eq!(cs.purge_empty(u64::MAX), 1);
        let (r, claim) = env.claim(&cs, 5);
        assert_eq!(r, p);
        assert!(claim.zeroed);
        for &s in &r {
            assert_eq!(unsafe { *s }, 0);
        }
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let slab = env.regions.slab_addr(c, 0);
        let end = slab + cs.class().slab_size();
        let rw = maps.lines().any(|l| {
            let mut it = l.split_whitespace();
            let range = it.next().unwrap();
            let perms = it.next().unwrap();
            let (a, b) = range.split_once('-').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let b = usize::from_str_radix(b, 16).unwrap();
            a <= slab && end <= b && perms.starts_with("rw")
        });
        assert!(rw, "slab 0 is not read-write in /proc/self/maps");
        // Descriptors live in the metadata region and got committed too.
        let d = cs.desc(0) as *const SlabDesc as usize;
        assert!(env.regions.in_meta(d));
        assert!(env.meta.stats().committed >= PAGE);
    }

    /// Guard slabs only when the kernel has `MADV_GUARD_INSTALL`: touching
    /// the guard page must fault. Run in a child so the fault is observable.
    #[cfg(target_os = "linux")]
    #[test]
    fn guard_slab_faults_when_supported() {
        use crate::sys::features::Features;
        let f = Features::probe();
        if !f.guard_install {
            std::eprintln!("guard_install=no on this kernel; skipping guard fault test");
            return;
        }
        let env = Env::new();
        let c = small_class();
        let cs = env.class(c, 2);
        let n = cs.class().nslots();
        let (p, _) = env.claim(&cs, n + 1);
        assert_eq!(cs.locate(p[n]).0, 2);
        let guard = env.regions.slab_addr(c, 1);
        // The guard slab is inside the same rw mapping as its neighbours.
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let merged = maps.lines().any(|l| {
            let range = l.split_whitespace().next().unwrap();
            let (a, b) = range.split_once('-').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let b = usize::from_str_radix(b, 16).unwrap();
            a <= env.regions.slab_addr(c, 0) && env.regions.slab_addr(c, 3) <= b
        });
        assert!(merged, "guard slab split the VMA");
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                core::ptr::write_volatile(guard as *mut u8, 1);
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status), "child did not fault: status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV);
    }
}
