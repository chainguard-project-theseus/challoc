//! Huge allocations (design.md section 7).
//!
//! Requests above the largest medium class are served from the huge region
//! with page granularity. Every page below the high-water mark `next_page`
//! belongs to exactly one *extent*, described by a [`HugeDesc`]:
//!
//! - an `Active` run: the pages handed to the caller followed by
//!   [`GUARD_PAGES`] guard pages, or
//! - a free extent, `Empty` (dirty, still resident) or `Purged` (discarded,
//!   reads as zero), which is what a freed run becomes.
//!
//! Adjacent free extents are always coalesced, whatever their states (the
//! merge is `Empty` unless both were `Purged`), so a maximal run of free
//! pages is exactly one extent. Free pages are also mirrored in a [`Set`]
//! (a `TwoLevelBitmap` over pages) so that allocation is lowest-address
//! first fit: walk the free extents in address order, take the first that
//! fits (or the last one if it touches the high-water mark and can grow),
//! else carve fresh pages at the mark. Descriptors live in an open-addressing hash table
//! keyed by page number: a run is keyed by its start page, a free extent by
//! its first and last page so that a free can find both neighbours.
//!
//! Guards. In `guard_install` mode the guard page is committed like its
//! neighbours and gets `MADV_GUARD_INSTALL`, so runs stay in one VMA; the
//! guard is removed when the run is freed so free extents never contain
//! one. Otherwise the guard page is left (or put back) `PROT_NONE`, which
//! splits the VMA but costs nothing else, and every allocation re-commits
//! its usable range so stale `PROT_NONE` guards inside a reused extent are
//! made writable again.
//!
//! Alignment above a page is met in `find`: the start of each free extent
//! (and finally the high-water mark) is rounded up to the alignment and the
//! extent is taken when the whole run still fits below its end, or when
//! the extent touches the mark and can grow; `take` leaves the pages
//! before the aligned start and any left after the run as free extents.
//! Runs whose usable size exceeds `retain_max` (default 64 MiB) are
//! `MADV_DONTNEED`ed the moment they are freed (every run under
//! `huge_dirty_decay_ms:0`, [`Huge::set_purge_on_free`]); smaller ones
//! become `Empty` and wait for the decay walk ([`Huge::decay_with`], on arena
//! 0's period or the huge tier's own `huge_dirty_decay_ms`, design 7) or a
//! full purge ([`Huge::purge_empty`]). `Empty` extents sit in a FIFO list ordered by
//! `dirty_at`, so purging walks it from the head; an extent whose discard
//! fails (`mlock`ed pages) stays in its place, `Empty`, as does a run
//! whose discard at free time fails.
//!
//! Everything is under one futex lock: huge operations are rare compared to
//! slab operations and each already costs at least one syscall.
//!
//! Submodules: [`table`] is the open-addressing descriptor table
//! (`Table`), `tests` the allocator's tests and `linux_tests` the ones
//! that need the process's own regions.

use core::alloc::Layout;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::class::Set;
use crate::layout::Regions;
use crate::lock::{Lock, RawLock};
use crate::meta::Meta;
use crate::purge::{self, Batch, Curve, Decayer};
use crate::sys::{self, Errno};

mod table;

use table::Table;
use vstd::prelude::verus;

verus! {

/// Guard pages after every run.
pub const GUARD_PAGES: usize = 1;
/// Runs with more usable bytes than this are purged as soon as they are
/// freed (`huge_retain_max`).
pub const DEFAULT_RETAIN_MAX: usize = 64 << 20;

} // verus!

/// State of an extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RunState {
    /// Handed to the caller (usable pages plus guard).
    Active = 0,
    /// Freed, dirty, resident.
    Empty = 1,
    /// Freed and discarded (or never touched); reads as zero.
    Purged = 2,
}

impl RunState {
    /// Decode a state byte. Aborts on a value no state has: a corrupted
    /// byte must not read as `Purged`, whose pages `take` hands out as
    /// zero memory.
    #[inline]
    fn from_u8(v: u8) -> RunState {
        match RunState::try_from_u8(v) {
            Some(s) => s,
            None => bad_run_state_byte(v),
        }
    }

    /// [`from_u8`](RunState::from_u8) without the abort.
    #[inline]
    const fn try_from_u8(v: u8) -> Option<RunState> {
        match v {
            0 => Some(RunState::Active),
            1 => Some(RunState::Empty),
            2 => Some(RunState::Purged),
            _ => None,
        }
    }

    #[inline]
    pub const fn is_free(self) -> bool {
        !matches!(self, RunState::Active)
    }
}

/// Descriptor of one extent (design section 7's `HugeDesc`). Allocated from
/// the metadata region; free extents also carry the FIFO links of the
/// `Empty` list.
#[repr(C)]
pub struct HugeDesc {
    start: u32,
    pages: u32,
    arena: u16,
    state: u8,
    _pad: u8,
    /// Bytes the caller may use; 0 for a free extent.
    usable: usize,
    /// Coarse monotonic time the extent became `Empty`; 0 otherwise.
    dirty_at: u64,
    prev: *mut HugeDesc,
    next: *mut HugeDesc,
}

impl HugeDesc {
    #[inline]
    pub fn start(&self) -> usize {
        self.start as usize
    }

    #[inline]
    pub fn pages(&self) -> usize {
        self.pages as usize
    }

    /// One past the last page.
    #[inline]
    pub fn end(&self) -> usize {
        self.start() + self.pages()
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    pub fn state(&self) -> RunState {
        RunState::from_u8(self.state)
    }
}

impl HugeDesc {
    #[cfg(test)]
    const LAYOUT: Layout = Layout::new::<HugeDesc>();
}

/// What [`Huge::alloc_run`] placed: the run's start, whether it reads as
/// zero, and its pages with the guard, read from the descriptor it wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Null when nothing was placed.
    pub ptr: *mut u8,
    pub zeroed: bool,
    /// The run's pages, guard included (0 when nothing was placed).
    pub pages: usize,
}

impl Placed {
    /// Nothing placed.
    pub const NONE: Placed = Placed {
        ptr: ptr::null_mut(),
        zeroed: false,
        pages: 0,
    };
}

/// A copy of a descriptor for callers outside the lock (tests, stats).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub start: usize,
    pub pages: usize,
    pub usable: usize,
    pub arena: u16,
    pub state: RunState,
    pub dirty_at: u64,
}

/// Counters kept under the huge lock. Identity:
/// `run_pages + empty_pages + purged_pages == next_page`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HugeStats {
    /// Live runs.
    pub runs: usize,
    /// Pages in live runs, guards included.
    pub run_pages: usize,
    /// `Empty` extents.
    pub empty: usize,
    pub empty_pages: usize,
    /// `Purged` extents.
    pub purged: usize,
    pub purged_pages: usize,
    /// High-water mark: pages ever handed out.
    pub next_page: usize,
    /// Descriptor table capacity, for diagnostics; filled in by
    /// [`Huge::stats`] from the table, not tracked.
    pub table_cap: usize,
}

impl HugeStats {
    /// Count one extent of `pages` in `state`.
    fn add(&mut self, state: RunState, pages: usize) {
        match state {
            RunState::Active => {
                self.runs += 1;
                self.run_pages += pages;
            }
            RunState::Empty => {
                self.empty += 1;
                self.empty_pages += pages;
            }
            RunState::Purged => {
                self.purged += 1;
                self.purged_pages += pages;
            }
        }
    }

    /// Uncount one extent of `pages` in `state`.
    fn sub(&mut self, state: RunState, pages: usize) {
        // A count going below zero means the extents and the counters
        // disagree; the release profile does not check overflow, so the
        // subtractions are explicit.
        let sub = |x: usize, y: usize| {
            x.checked_sub(y)
                .unwrap_or_else(|| huge_count_underflow(state))
        };
        match state {
            RunState::Active => {
                self.runs = sub(self.runs, 1);
                self.run_pages = sub(self.run_pages, pages);
            }
            RunState::Empty => {
                self.empty = sub(self.empty, 1);
                self.empty_pages = sub(self.empty_pages, pages);
            }
            RunState::Purged => {
                self.purged = sub(self.purged, 1);
                self.purged_pages = sub(self.purged_pages, pages);
            }
        }
    }
}

#[cold]
#[inline(never)]
fn bad_run_state_byte(v: u8) -> ! {
    panic!("huge: run state byte {v:#x} is not a run state")
}

#[cold]
#[inline(never)]
fn huge_count_underflow(state: RunState) -> ! {
    panic!("huge: {state:?} extent count underflow")
}

/// What a purge walk's flushes settled: extents made `Purged` and whether
/// a discard failed.
#[derive(Default)]
struct Tally {
    purged: usize,
    failed: bool,
}

struct Inner {
    /// Free pages below `next_page`.
    free: Set,
    table: Table,
    /// High-water mark: pages at or above it were never handed out.
    next_page: usize,
    /// `Empty` extents, oldest `dirty_at` first.
    empty_head: *mut HugeDesc,
    empty_tail: *mut HugeDesc,
    stats: HugeStats,
}

// SAFETY: the raw pointers address the metadata region, which is process
// global; the `Lock` serialises every access.
unsafe impl Send for Inner {}

/// The huge allocator: one per process, shared by every arena (design
/// section 7); the arena an allocation was made through is recorded per
/// run for the per-arena statistics.
pub struct Huge {
    base: usize,
    lg_page: u32,
    npages: usize,
    guard_install: bool,
    retain_max: AtomicUsize,
    /// `huge_dirty_decay_ms:0` (design 7): a freed run is purged as it is
    /// freed, whatever its size, instead of waiting in the `Empty` list
    /// for a decay pass; set once at init (`Shared::init`).
    purge_on_free: AtomicBool,
    /// The decay step gate (`purge::Decayer`): `dirty_at` of the head of
    /// the `Empty` list (its oldest member), kept in step with the list
    /// under the lock by `sync_oldest` and read without it by the decay
    /// driver, and the next walk's deadline.
    gate: Decayer,
    inner: Lock<Inner>,
}

/// `PROT_NONE`, `PROT_READ | PROT_WRITE` and the guard advice of
/// `sys` (checked against them below).
const PROT_NONE: i32 = 0;
const PROT_RW: i32 = 3;
const MADV_GUARD_INSTALL: i32 = 102;
const MADV_GUARD_REMOVE: i32 = 103;

const _: () = assert!(
    PROT_NONE == sys::PROT_NONE
        && PROT_RW == sys::PROT_READ | sys::PROT_WRITE
        && MADV_GUARD_INSTALL == sys::MADV_GUARD_INSTALL
        && MADV_GUARD_REMOVE == sys::MADV_GUARD_REMOVE
);

fn commit(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: inside our own reservation; adding permissions to pages
    // nobody holds cannot break anything.
    unsafe { sys::mprotect(addr, len, PROT_RW) }
}

fn protect_none(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the page is a guard that no caller may touch.
    unsafe { sys::mprotect(addr, len, PROT_NONE) }
}

fn guard_install(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: as `protect_none`.
    unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
}

fn guard_remove(addr: usize, len: usize) -> Result<(), Errno> {
    // SAFETY: the run is being freed; the guard protects nothing now.
    unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
}

fn now_ns() -> u64 {
    sys::now_coarse_ns()
}

/// A block for a descriptor from `meta`; null when the metadata region is
/// exhausted. [`free_desc`] gives it back.
#[inline(always)]
fn alloc_desc(meta: &Meta) -> *mut HugeDesc {
    meta.alloc(Layout::new::<HugeDesc>()) as *mut HugeDesc
}

/// Give the block `d` from [`alloc_desc`] back to `meta`.
#[inline(always)]
fn free_desc(meta: &Meta, d: *mut HugeDesc) {
    meta.free(d as *mut u8, Layout::new::<HugeDesc>());
}

impl Inner {
    /// The descriptor at `d`.
    #[inline(always)]
    fn desc(&self, d: *mut HugeDesc) -> &HugeDesc {
        // SAFETY: `d` is a live descriptor of this allocator: in the table,
        // the `Empty` list, or just written by its holder under the lock.
        unsafe { &*d }
    }

    /// The descriptor at `d`, to write.
    #[inline(always)]
    fn desc_mut(&mut self, d: *mut HugeDesc) -> &mut HugeDesc {
        // SAFETY: as `desc`; the huge lock, which `&mut self` stands for,
        // serialises every access to it.
        unsafe { &mut *d }
    }

    /// Enter `d`, an initialised descriptor not in the table, into the
    /// table and the counters: a run under its start page, a free extent
    /// under its first and last page so that a free finds both
    /// neighbours. The free bitmap and the `Empty` list are the caller's.
    fn add_extent(&mut self, d: *mut HugeDesc) {
        // `d` is a live descriptor owned by this allocator.
        let (start, pages, state) = (
            self.desc(d).start(),
            self.desc(d).pages(),
            self.desc(d).state(),
        );
        self.table.insert(start, d);
        if state.is_free() && pages > 1 {
            self.table.insert(start + pages - 1, d);
        }
        self.stats.add(state, pages);
    }

    /// Undo [`add_extent`](Inner::add_extent): take `d` out of the table
    /// and the counters. `d` itself, its list links and the free bitmap
    /// are untouched, so the caller can enter it again with new bounds or
    /// [`retire`](Inner::retire) it.
    fn drop_extent(&mut self, d: *mut HugeDesc) {
        // As `add_extent`.
        let (start, pages, state) = (
            self.desc(d).start(),
            self.desc(d).pages(),
            self.desc(d).state(),
        );
        self.table.remove(start);
        if state.is_free() && pages > 1 {
            self.table.remove(start + pages - 1);
        }
        self.stats.sub(state, pages);
    }

    /// Give a dropped descriptor back to `meta`, leaving the `Empty` list
    /// first when it is a member (every `Empty` extent is; a freed run's
    /// descriptor still reads `Active` here and is in no list).
    fn retire(&mut self, meta: &Meta, d: *mut HugeDesc) {
        // `d` is live and about to be discarded.
        if self.desc(d).state() == RunState::Empty {
            self.unlink(d);
        }
        free_desc(meta, d);
    }

    /// Append to the `Empty` list; `d.dirty_at` must be the newest.
    fn link_tail(&mut self, d: *mut HugeDesc) {
        // `d` is live and not in the list.
        self.desc_mut(d).prev = self.empty_tail;
        self.desc_mut(d).next = ptr::null_mut();
        if self.empty_tail.is_null() {
            self.empty_head = d;
        } else {
            self.desc_mut(self.empty_tail).next = d;
        }
        self.empty_tail = d;
    }

    /// Insert `d` right after `after`, which is in the list.
    fn link_after(&mut self, after: *mut HugeDesc, d: *mut HugeDesc) {
        // Both are live; `after` is linked, `d` is not.
        self.desc_mut(d).prev = after;
        self.desc_mut(d).next = self.desc(after).next;
        if self.desc(after).next.is_null() {
            self.empty_tail = d;
        } else {
            self.desc_mut(self.desc(after).next).prev = d;
        }
        self.desc_mut(after).next = d;
    }

    /// Take `d` out of the `Empty` list.
    fn unlink(&mut self, d: *mut HugeDesc) {
        // `d` is live and in the list.
        if self.desc(d).prev.is_null() {
            self.empty_head = self.desc(d).next;
        } else {
            self.desc_mut(self.desc(d).prev).next = self.desc(d).next;
        }
        if self.desc(d).next.is_null() {
            self.empty_tail = self.desc(d).prev;
        } else {
            self.desc_mut(self.desc(d).next).prev = self.desc(d).prev;
        }
        self.desc_mut(d).prev = ptr::null_mut();
        self.desc_mut(d).next = ptr::null_mut();
    }

    /// The free extent starting at `page`, which must be a set free bit.
    fn extent_at(&self, page: usize) -> *mut HugeDesc {
        let d = self.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || self.desc(d).start() != page || !self.desc(d).state().is_free() {
            panic!("huge: free page {page} has no extent");
        }
        d
    }

    /// Neighbouring free extent ending exactly at `page`, if any.
    fn free_before(&self, page: usize) -> *mut HugeDesc {
        if page == 0 {
            return ptr::null_mut();
        }
        let d = self.table.get(page - 1);
        // As `extent_at`.
        if !d.is_null() && self.desc(d).state().is_free() && self.desc(d).end() == page {
            d
        } else {
            ptr::null_mut()
        }
    }

    /// Neighbouring free extent starting exactly at `page`, if any.
    fn free_after(&self, page: usize) -> *mut HugeDesc {
        let d = self.table.get(page);
        // As `extent_at`.
        if !d.is_null() && self.desc(d).state().is_free() && self.desc(d).start() == page {
            d
        } else {
            ptr::null_mut()
        }
    }
}

/// Where [`Huge::find`] put a run: its first page, its length in pages
/// (guard included) and the free extent the pages come from, null for
/// fresh pages at the high-water mark. What [`Huge::take`] records.
#[derive(Clone, Copy)]
struct Placement {
    start: usize,
    pages: usize,
    ext: *mut HugeDesc,
}

impl Placement {
    /// One past the run's last page.
    #[inline]
    fn end(&self) -> usize {
        self.start + self.pages
    }
}

impl Huge {
    /// Create the huge allocator for `regions`, reserving its free-page
    /// bitmap and descriptor table in `meta`. `guard_install` should be the
    /// probed `MADV_GUARD_INSTALL` support. `None` if the metadata region
    /// cannot hold the tables.
    pub fn new(meta: &Meta, regions: &Regions, guard_install: bool) -> Option<Huge> {
        let page = regions.page_size();
        let npages = regions.huge_pages();
        assert!(
            npages > 0 && npages <= u32::MAX as usize,
            "huge: region page count out of range"
        );
        let free = Set::new(meta, page, npages)?;
        let table = Table::new(meta, page)?;
        Some(Huge {
            base: regions.huge_base,
            lg_page: regions.lg_page,
            npages,
            guard_install,
            retain_max: AtomicUsize::new(DEFAULT_RETAIN_MAX),
            purge_on_free: AtomicBool::new(false),
            gate: Decayer::new(),
            inner: Lock::new(Inner {
                free,
                table,
                next_page: 0,
                empty_head: ptr::null_mut(),
                empty_tail: ptr::null_mut(),
                stats: HugeStats {
                    runs: 0,
                    run_pages: 0,
                    empty: 0,
                    empty_pages: 0,
                    purged: 0,
                    purged_pages: 0,
                    next_page: 0,
                    table_cap: 0,
                },
            }),
        })
    }

    /// The huge lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    #[inline]
    pub fn page_size(&self) -> usize {
        1 << self.lg_page
    }

    /// Pages in the region.
    #[cfg(test)]
    pub fn npages(&self) -> usize {
        self.npages
    }

    /// Usable-byte threshold above which freed runs are purged at once.
    pub fn retain_max(&self) -> usize {
        self.retain_max.load(Ordering::Relaxed)
    }

    pub fn set_retain_max(&self, bytes: usize) {
        self.retain_max.store(bytes, Ordering::Relaxed);
    }

    /// Whether a freed run is purged as it is freed (`huge_dirty_decay_ms:0`).
    #[inline]
    pub fn purge_on_free(&self) -> bool {
        self.purge_on_free.load(Ordering::Relaxed)
    }

    pub fn set_purge_on_free(&self, on: bool) {
        self.purge_on_free.store(on, Ordering::Relaxed);
    }

    #[inline]
    fn addr(&self, page: usize) -> usize {
        self.base + (page << self.lg_page)
    }

    /// Usable pages for a request, or `None` if it cannot be represented.
    #[inline]
    pub fn usable_pages(&self, size: usize) -> Option<usize> {
        let n = size.checked_add(self.page_size() - 1)? >> self.lg_page;
        Some(n.max(1))
    }

    /// Usable bytes `alloc(size, ..)` would give: `nallocx` for huge sizes.
    #[inline]
    pub fn usable_for(&self, size: usize) -> Option<usize> {
        let n = self.usable_pages(size);
        Some(n? << self.lg_page)
    }

    /// Pages a run for `size` bytes at `align` takes, its guard included, or
    /// `None` when no placement in the region can hold it: the size cannot
    /// be represented, the run is longer than the region, or `align` is
    /// larger than the region. [`Huge::alloc_run`] and [`Huge::placeable`]
    /// share these checks.
    fn run_pages(&self, size: usize, align: usize) -> Option<usize> {
        let n = self.usable_pages(size)?;
        let need = n.checked_add(GUARD_PAGES)?;
        if need > self.npages || align > (self.npages << self.lg_page) {
            return None;
        }
        Some(need)
    }

    /// Whether [`Huge::alloc_run`] can place a run for `size` bytes at
    /// `align` in an empty region: [`Huge::run_pages`] holds, and the run
    /// fits from the first `align`-aligned page. Aligned starts repeat every
    /// `align` bytes, so if the first one leaves no room, none does.
    pub fn placeable(&self, size: usize, align: usize) -> bool {
        // `run_pages(size, align).is_some_and(|need| ...)`, spelled as a
        // `match`.
        match self.run_pages(size, align) {
            Some(need) => {
                let s = self.align_page(0, align);
                s + need <= self.npages
            }
            None => false,
        }
    }

    /// First page at or after `page` whose address is a multiple of
    /// `align` (a power of two).
    #[inline]
    fn align_page(&self, page: usize, align: usize) -> usize {
        if align <= self.page_size() {
            return page;
        }
        let a = self.addr(page);
        let aligned = (a + align - 1) & !(align - 1);
        (aligned - self.base) >> self.lg_page
    }

    /// [`Huge::alloc_run`] as the pointer and the zero flag alone.
    #[cfg(test)]
    pub fn alloc(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let r = self.alloc_run(meta, size, align, arena);
        (r.ptr, r.zeroed)
    }

    /// Allocate `size` bytes aligned to `align` (a power of two; anything
    /// up to a page is free). Returns the pointer, whether the memory is
    /// known to read as zero and the pages of the run as its descriptor
    /// records them (the guard included; what [`Huge::free_run`] will
    /// report as `pages`, so the caller's per-arena page count never
    /// drifts from what the free subtracts); [`Placed::NONE`] when the
    /// region, the metadata region or the kernel refuses.
    #[inline]
    pub fn alloc_run(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> Placed {
        assert!(
            align.is_power_of_two(),
            "huge: alignment not a power of two"
        );
        self.alloc_aligned(meta, size, align, arena)
    }

    /// [`Huge::alloc_run`] for an `align` already known to be a power of
    /// two.
    #[inline(always)]
    fn alloc_aligned(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> Placed {
        let Some(need) = self.run_pages(size, align) else {
            return Placed::NONE;
        };
        let usable_pages = need - GUARD_PAGES;
        let page = self.page_size();
        let mut g = self.inner.lock();
        let Some((s, ext)) = self.find(&g, need, align) else {
            return Placed::NONE;
        };
        let end = s + need;

        // Everything fallible happens before any state changes.
        let run = alloc_desc(meta);
        if run.is_null() {
            return Placed::NONE;
        }
        let spare = alloc_desc(meta);
        if spare.is_null() {
            free_desc(meta, run);
            return Placed::NONE;
        }
        let ok = g.table.ensure(meta, page, 3).is_ok()
            && (end <= g.next_page || g.free.ensure(meta, page, end - 1).is_ok())
            && self.prepare(s, usable_pages, g.next_page).is_ok();
        if !ok {
            free_desc(meta, run);
            free_desc(meta, spare);
            return Placed::NONE;
        }

        // `check_zero` needs the extent's state before `take` may free it.
        let prior = if purge::check_zero() {
            Some(self.prior_state(s, ext))
        } else {
            None
        };
        let zeroed = self.take(
            &mut g,
            meta,
            Placement {
                start: s,
                pages: need,
                ext,
            },
            run,
            spare,
            arena,
        );
        // `take` wrote the run's descriptor; the lock is still held.
        let pages = g.desc(run).pages();
        self.sync_oldest(&g);
        if let (true, Some(prior)) = (zeroed, prior) {
            self.check_zero(s, usable_pages, prior, arena);
        }
        Placed {
            ptr: crate::vspec::addr::addr_to_ptr(self.addr(s)),
            zeroed,
            pages,
        }
    }

    /// Where the pages of a run at `s` come from, for `check_zero`: the
    /// free extent's state, or "fresh" for never-touched pages at or past
    /// the mark (which is also what an aligned start beyond `ext` gets).
    #[cold]
    fn prior_state(&self, s: usize, ext: *mut HugeDesc) -> &'static str {
        if ext.is_null() {
            return "fresh";
        }
        // SAFETY: `ext` is a live free extent found under the lock the
        // caller holds.
        let (st, e_end) = unsafe { ((*ext).state(), (*ext).end()) };
        if s >= e_end {
            return "fresh";
        }
        match st {
            RunState::Purged => "Purged",
            RunState::Empty => "Empty",
            // `find` returns free extents only (`extent_at` checks).
            RunState::Active => unreachable!(),
        }
    }

    /// The `check_zero` debug mode: the first and
    /// last word of every usable page of a run reported zero must read
    /// zero. Aborts with the run, the pages' prior state, the arena and the
    /// purge advice. Faults every page in, so debug only.
    #[cold]
    #[inline(never)]
    fn check_zero(&self, s: usize, usable_pages: usize, prior: &'static str, arena: u16) {
        let page = self.page_size();
        let base = self.addr(s);
        for i in 0..usable_pages {
            for &off in &[0, page - 8] {
                let a = base + i * page + off;
                // SAFETY: a usable page of the run this call just took;
                // `prepare` committed it.
                let word = unsafe { self.read_run_word(a) };
                if word != 0 {
                    dirty_zero_run(base, usable_pages, s, prior, arena, a, word);
                }
            }
        }
    }

    /// The word at `a`, read once, for [`Huge::check_zero`]. Nothing is
    /// claimed about the value.
    ///
    /// # Safety
    /// `a` is an aligned word of a committed usable page of a run.
    #[inline(always)]
    unsafe fn read_run_word(&self, a: usize) -> u64 {
        let q = crate::vspec::addr::addr_to_ptr(a);
        // SAFETY: the caller's.
        unsafe { ptr::read_volatile(q as *const u64) }
    }

    /// Refresh `oldest_empty` from the list head. Returns `true` when the
    /// list went from empty to non-empty, which is when a background
    /// thread sleeping without a deadline needs waking.
    fn sync_oldest(&self, g: &Inner) -> bool {
        // The head, if any, is a live `Empty` extent.
        let new = if g.empty_head.is_null() {
            0
        } else {
            g.desc(g.empty_head).dirty_at
        };
        self.gate.set_oldest_empty(new)
    }

    /// Lowest-address first fit for `need` pages at `align`. Returns the
    /// start page and the free extent it comes from (`None` for fresh pages
    /// at the high-water mark). The extent may end below `s + need` only if
    /// it touches the mark.
    fn find(&self, g: &Inner, need: usize, align: usize) -> Option<(usize, *mut HugeDesc)> {
        let mut from = 0;
        while let Some(a) = g.free.map.lowest_set_from(from) {
            let e = g.extent_at(a);
            // `extent_at` checked `e` is live.
            let e_end = g.desc(e).end();
            let s = self.align_page(a, align);
            if s + need <= e_end {
                return Some((s, e));
            }
            if e_end == g.next_page {
                return if s + need <= self.npages {
                    Some((s, e))
                } else {
                    None
                };
            }
            from = e_end;
        }
        let s = self.align_page(g.next_page, align);
        if s + need <= self.npages {
            Some((s, ptr::null_mut()))
        } else {
            None
        }
    }

    /// Kernel side of an allocation of `usable` pages at `s`: commit and
    /// place the guard. Pages below `watermark` are committed already
    /// (apart from `PROT_NONE` guards in fallback mode).
    ///
    /// In guard-install mode every page below the mark stays committed, so
    /// one `mprotect` over `[watermark, end)` is all a run past the mark
    /// needs. That range deliberately starts at the mark, not at `s`: when
    /// alignment puts `s` above the mark, `take` records `[watermark, s)`
    /// as a `Purged` free extent (or folds it into the extent that touched
    /// the mark), and the run later placed in that hole must find its pages
    /// committed. Committing only `[s, end)` here left the hole `PROT_NONE`
    /// and the first write into it faulted; the fallback mode never had
    /// the problem because it commits `[s, s + usable)` on every call.
    fn prepare(&self, s: usize, usable: usize, watermark: usize) -> Result<(), Errno> {
        let page = self.page_size();
        let guard = s + usable;
        let end = guard + GUARD_PAGES;
        let r = if self.guard_install {
            if watermark < end {
                commit(self.addr(watermark), (end - watermark) * page)?;
            }
            guard_install(self.addr(guard), GUARD_PAGES * page)
        } else {
            commit(self.addr(s), usable * page)?;
            if guard < watermark {
                protect_none(self.addr(guard), GUARD_PAGES * page)?;
            }
            Ok(())
        };
        // Design 7: `MADV_HUGEPAGE` on runs of a PMD or more under
        // `thp:always` (`thp.rs`; `usize::MAX` otherwise, so nothing is
        // advised). A refusal is counted, not an error: the run works
        // with small pages.
        if r.is_ok() && usable * page >= crate::thp::huge_min() {
            crate::thp::advise_huge(self.addr(s), usable * page);
        }
        r
    }

    /// Record the run `p` taken from its extent (or fresh pages), splitting
    /// the extent around it. Infallible. Returns the zero flag.
    fn take(
        &self,
        g: &mut Inner,
        meta: &Meta,
        p: Placement,
        run: *mut HugeDesc,
        spare: *mut HugeDesc,
        arena: u16,
    ) -> bool {
        let Placement {
            start: s,
            pages: need,
            ext,
        } = p;
        let end = p.end();
        let old_mark = g.next_page;
        let mut spare_used = false;
        let mut zeroed = true;
        if ext.is_null() {
            if s > old_mark {
                // Alignment left a hole at the mark: a clean free extent.
                init_free(spare, old_mark, s - old_mark, RunState::Purged, 0);
                g.free.map.set_range(old_mark, s - old_mark);
                g.add_extent(spare);
                spare_used = true;
            }
        } else {
            // `ext` is a live free extent found under this lock.
            let (a, e_end, st, dirty_at) = (
                g.desc(ext).start(),
                g.desc(ext).end(),
                g.desc(ext).state(),
                g.desc(ext).dirty_at,
            );
            // `Purged` pages are zero unless a lazy purge discarded them
            // with `MADV_FREE`; pages at or past the mark are untouched.
            zeroed = (st == RunState::Purged && purge::purged_is_zero()) || s >= e_end;
            g.drop_extent(ext);
            let taken_lo = s.max(a);
            let taken_hi = end.min(e_end);
            if taken_lo < taken_hi {
                g.free.map.clear_range(taken_lo, taken_hi - taken_lo);
            }
            let tail = e_end.saturating_sub(end);
            if tail > 0 {
                // The tail inherits the state and age of `ext` and, when
                // `Empty`, its place in the FIFO (same `dirty_at`).
                init_free(spare, end, tail, st, dirty_at);
                if st == RunState::Empty {
                    g.link_after(ext, spare);
                }
                g.add_extent(spare);
                spare_used = true;
            }
            if s > a {
                // Head survives (and, when `s >= e_end`, absorbs the hole
                // between the old extent and the aligned start).
                if s > e_end {
                    g.free.map.set_range(e_end, s - e_end);
                }
                g.desc_mut(ext).pages = (s - a) as u32;
                g.add_extent(ext);
            } else {
                g.retire(meta, ext);
            }
        }
        if end > old_mark {
            g.next_page = end;
        }
        // SAFETY: `run` is a fresh metadata block for a `HugeDesc`.
        unsafe {
            ptr::write(
                run,
                HugeDesc {
                    start: s as u32,
                    pages: need as u32,
                    arena,
                    state: RunState::Active as u8,
                    _pad: 0,
                    usable: (need - GUARD_PAGES) << self.lg_page,
                    dirty_at: 0,
                    prev: ptr::null_mut(),
                    next: ptr::null_mut(),
                },
            );
        }
        g.add_extent(run);
        g.stats.next_page = g.next_page;
        if !spare_used {
            free_desc(meta, spare);
        }
        zeroed
    }

    /// [`Huge::free_run`] returning only the usable bytes released.
    #[cfg(test)]
    pub fn free(&self, meta: &Meta, page: usize) -> usize {
        self.free_run(meta, page).usable
    }

    /// Free the run starting at `page` (from `Regions::classify`). Aborts
    /// unless it is the start of a live run. Returns the run as it was
    /// (`start`, `pages` with the guard, `usable`, `arena`), for the
    /// caller's per-arena counters.
    // The coalescing loops are spelled out (see below), not `while let`.
    #[allow(clippy::while_let_loop)]
    pub fn free_run(&self, meta: &Meta, page: usize) -> RunInfo {
        let page_size = self.page_size();
        let mut g = self.inner.lock();
        let d = g.table.get(page);
        if d.is_null() {
            not_a_run(self.addr(page));
        }
        // Table entries are live descriptors.
        let (start, n, state, usable, arena) = (
            g.desc(d).start(),
            g.desc(d).pages(),
            g.desc(d).state(),
            g.desc(d).usable,
            g.desc(d).arena(),
        );
        if state != RunState::Active {
            already_free(self.addr(page));
        }
        if start != page {
            not_a_run(self.addr(page));
        }
        let addr = self.addr(page);
        if self.guard_install {
            if let Err(e) = guard_remove(
                addr + (n - GUARD_PAGES) * page_size,
                GUARD_PAGES * page_size,
            ) {
                panic!("huge: madvise(MADV_GUARD_REMOVE) of {addr:#x} failed: {e}");
            }
        }
        let now = now_ns().max(1);
        // Purged at once: above `huge_retain_max`, or under a zero huge
        // decay period (`huge_dirty_decay_ms:0`). A run the discard fails
        // on is kept `Empty` for decay or the next explicit purge.
        let cap = self.retain_max();
        let purge = usable > cap || self.purge_on_free();
        let st = if purge && purge::discard_one(addr, n * page_size) {
            RunState::Purged
        } else {
            RunState::Empty
        };
        // Best effort: a fuller table still works, only slower.
        let _ = g.table.ensure(meta, page_size, 2);
        g.drop_extent(d);

        // Coalesce with free neighbours into one extent `[lo, hi)`. The
        // survivor is the `Empty` participant with the oldest `dirty_at`
        // (it keeps its place in the FIFO list), else `d`; the other
        // descriptors go back to `meta`. A neighbour whose discards are
        // refused (locked pages) absorbs the run all the same, which then
        // stays dirty with it until the unlock or a reuse (design 8).
        let pred = g.free_before(page);
        let succ = g.free_after(page + n);
        let mut lo = page;
        let mut hi = page + n;
        let mut merged = st;
        let mut survivor = d;
        // `for nb in [pred, succ]`, spelled out: Verus rejects `continue`
        // in a `for` loop, and the spelled-out loop keeps the machine code
        // of the `for`.
        let mut iter = [pred, succ].into_iter();
        loop {
            let nb = match iter.next() {
                Some(nb) => nb,
                None => {
                    break;
                }
            };
            if nb.is_null() {
                continue;
            }
            // Neighbours are live free extents.
            let (ns, ne, nst, ndirty) = (
                g.desc(nb).start(),
                g.desc(nb).end(),
                g.desc(nb).state(),
                g.desc(nb).dirty_at,
            );
            g.drop_extent(nb);
            lo = lo.min(ns);
            hi = hi.max(ne);
            if nst == RunState::Empty {
                merged = RunState::Empty;
                // A survivor other than `d` is a live `Empty` neighbour.
                if survivor == d || ndirty < g.desc(survivor).dirty_at {
                    survivor = nb;
                }
            }
        }
        // `for x in [d, pred, succ]`, spelled out as above.
        let mut iter = [d, pred, succ].into_iter();
        loop {
            let x = match iter.next() {
                Some(x) => x,
                None => {
                    break;
                }
            };
            if !x.is_null() && x != survivor {
                g.retire(meta, x);
            }
        }
        // `survivor` is live: `d` (still the run's descriptor) or an
        // `Empty` neighbour that keeps its age and list position.
        g.desc_mut(survivor).start = lo as u32;
        g.desc_mut(survivor).pages = (hi - lo) as u32;
        g.desc_mut(survivor).state = merged as u8;
        g.desc_mut(survivor).usable = 0;
        if merged == RunState::Purged {
            g.desc_mut(survivor).dirty_at = 0;
        } else if survivor == d {
            g.desc_mut(survivor).dirty_at = now;
            g.link_tail(d);
        }
        g.free.map.set_range(page, n);
        g.add_extent(survivor);
        let first = self.sync_oldest(&g);
        drop(g);
        if first {
            purge::note_empty();
        }
        RunInfo {
            start,
            pages: n,
            usable,
            arena,
            state: RunState::Active,
            dirty_at: 0,
        }
    }

    /// Usable bytes of the live run starting at `page`. Aborts otherwise.
    pub fn usable_size(&self, page: usize) -> usize {
        let g = self.inner.lock();
        let d = g.table.get(page);
        // Table entries are live descriptors.
        if d.is_null() || g.desc(d).start() != page {
            not_a_run(self.addr(page));
        }
        if g.desc(d).state() != RunState::Active {
            already_free(self.addr(page));
        }
        g.desc(d).usable
    }

    /// Descriptor of the extent starting or ending at `page`, if any.
    pub fn lookup(&self, page: usize) -> Option<RunInfo> {
        let g = self.inner.lock();
        let d = g.table.get(page);
        if d.is_null() {
            return None;
        }
        // Table entries are live descriptors.
        Some(RunInfo {
            start: g.desc(d).start(),
            pages: g.desc(d).pages(),
            usable: g.desc(d).usable,
            arena: g.desc(d).arena,
            state: g.desc(d).state(),
            dirty_at: g.desc(d).dirty_at,
        })
    }

    /// Purge every `Empty` extent whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all): `MADV_DONTNEED`, then `Purged`; an extent
    /// whose discard fails stays `Empty` in its place (the next purge
    /// retries it). Returns the number purged.
    pub fn purge_empty(&self, cutoff: u64) -> usize {
        let mut g = self.inner.lock();
        let mut t = Tally::default();
        let mut batch = Batch::new();
        let mut d = g.empty_head;
        while !d.is_null() {
            // List members are live `Empty` extents; `next` is read before
            // a flush can unlink `d`.
            let (dirty_at, next) = (g.desc(d).dirty_at, g.desc(d).next);
            if dirty_at > cutoff {
                break;
            }
            self.push_extent(&mut g, d, &mut batch, &mut t);
            d = next;
        }
        batch.flush(|addr, len, done| self.settle(&mut g, addr, len, done, &mut t));
        self.sync_oldest(&g);
        t.purged
    }

    /// Push `d`, a member of the `Empty` list, to `batch`; it stays there,
    /// `Empty`, until the flush reports it discarded ([`settle`]). The
    /// batch is flushed before the lock is released, so no allocation can
    /// take the extent between the discard and the state change. Returns
    /// its pages.
    ///
    /// [`settle`]: Huge::settle
    fn push_extent(
        &self,
        g: &mut Inner,
        d: *mut HugeDesc,
        batch: &mut Batch,
        t: &mut Tally,
    ) -> usize {
        // List members are live `Empty` extents.
        let (start, pages) = (g.desc(d).start(), g.desc(d).pages());
        batch.push(
            self.addr(start),
            pages * self.page_size(),
            |addr, len, done| self.settle(g, addr, len, done, t),
        );
        pages
    }

    /// Make `d`, a member of the `Empty` list, `Purged`: out of the list,
    /// with no age, its pages counted as purged. Returns its pages.
    #[inline(always)]
    fn purge_desc(&self, g: &mut Inner, d: *mut HugeDesc) -> usize {
        // List members are live `Empty` extents.
        let pages = g.desc(d).pages();
        g.unlink(d);
        g.desc_mut(d).state = RunState::Purged as u8;
        g.desc_mut(d).dirty_at = 0;
        g.stats.sub(RunState::Empty, pages);
        g.stats.add(RunState::Purged, pages);
        pages
    }

    /// The flush's report on `[addr, addr + len)`, extents pushed by
    /// [`push_extent`] (under the huge lock since, so still in the `Empty`
    /// list): discarded, each leaves the list and becomes `Purged`; not,
    /// they stay as they are.
    ///
    /// [`push_extent`]: Huge::push_extent
    fn settle(&self, g: &mut Inner, addr: usize, len: usize, done: bool, t: &mut Tally) {
        if !done {
            t.failed = true;
            return;
        }
        let mut page = (addr - self.base) >> self.lg_page;
        let end = page + (len >> self.lg_page);
        while page < end {
            let d = g.table.get(page);
            // Table entries are live descriptors.
            if d.is_null() || g.desc(d).start() != page || g.desc(d).state() != RunState::Empty {
                panic!("huge: purged range {addr:#x}+{len:#x} is not Empty extents");
            }
            let pages = self.purge_desc(g, d);
            t.purged += 1;
            page += pages;
        }
    }

    /// Decay walk (design section 8), as `ClassState::purge_curve` in
    /// pages: the curve's allowance at `now` is the sum over the `Empty`
    /// extents of their pages times the weight of their age, rounded up,
    /// and the excess over it is purged oldest extent first, whole extents
    /// only and never below the allowance (an extent larger than the
    /// remaining excess is skipped for younger ones that fit), so a lone
    /// extent lasts until its weight is zero, the end of the window, as
    /// before. A failed discard leaves its extent `Empty` and backs the
    /// gate off (`Decayer::back_off`). Returns the number of extents
    /// purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32) -> usize {
        let mut g = self.inner.lock();
        let mut total = 0usize;
        let mut weighted = 0u64;
        let mut d = g.empty_head;
        while !d.is_null() {
            // List members are live `Empty` extents.
            let (pages, dirty_at, next) = (g.desc(d).pages(), g.desc(d).dirty_at, g.desc(d).next);
            total += pages;
            weighted += pages as u64 * Curve::weight(curve.step(now, dirty_at));
            d = next;
        }
        // The cgroup policy's retention cap (design 11), as in the class
        // walk: keep at most this walk's share of the cap.
        let mut allow = Curve::allowance(weighted);
        if keep16 < crate::policy::KEEP_ALL {
            allow = allow.min(crate::policy::mul16(total as u64, keep16) as usize);
        }
        let mut excess = total.saturating_sub(allow);
        // Under the cap an extent larger than the excess goes whole: free
        // neighbours coalesce, so what a program frees in pieces is often
        // one extent, and skipping it would keep the retention over the
        // cap for good (measured: 50 freed 8 MiB runs, one 400 MiB
        // extent, nothing purged in 3 s). The curve alone keeps the rule.
        let capped = keep16 < crate::policy::KEEP_ALL;
        let mut t = Tally::default();
        if excess > 0 {
            let mut batch = Batch::new();
            let mut d = g.empty_head;
            while !d.is_null() && excess > 0 {
                // As above; `next` is read before a flush can unlink `d`.
                let (pages, next) = (g.desc(d).pages(), g.desc(d).next);
                if pages <= excess || capped {
                    let pushed = self.push_extent(&mut g, d, &mut batch, &mut t);
                    excess = excess.saturating_sub(pushed);
                }
                d = next;
            }
            batch.flush(|addr, len, done| self.settle(&mut g, addr, len, done, &mut t));
        }
        if t.failed {
            self.gate.back_off(now);
        }
        self.sync_oldest(&g);
        t.purged
    }

    /// `dirty_at` of the oldest `Empty` extent as the decay driver sees it
    /// (0 when none).
    #[cfg(test)]
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// [`decay_with`](Huge::decay_with) keeping everything the curve
    /// allows.
    #[cfg(test)]
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL)
    }

    /// Decay step (design section 8), as `ClassState::decay_with`: the
    /// gate runs `purge_curve` over the list at most once per step of the
    /// curve, with the cgroup policy's keep ratio (design 11), and returns
    /// the next deadline, `None` when nothing is `Empty`. Lock-free unless
    /// a walk is due.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64> {
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16);
        })
    }

    /// Bytes in `Empty` extents (one lock hold), for the cgroup policy's
    /// retention figure.
    pub fn empty_bytes(&self) -> usize {
        let g = self.inner.lock();
        g.stats.empty_pages << self.lg_page
    }

    pub fn stats(&self) -> HugeStats {
        let g = self.inner.lock();
        HugeStats {
            table_cap: g.table.cap(),
            ..g.stats
        }
    }

    /// Oldest `Empty` extent's `dirty_at`, read under the lock (the decay
    /// driver reads the copy in [`Huge::oldest_empty`] instead).
    #[cfg(test)]
    pub fn oldest_dirty_at(&self) -> Option<u64> {
        let g = self.inner.lock();
        if g.empty_head.is_null() {
            None
        } else {
            // SAFETY: list members are live.
            Some(unsafe { (*g.empty_head).dirty_at })
        }
    }

    /// Walk every structure and check the invariants; for tests.
    #[cfg(test)]
    fn audit(&self) {
        let g = self.inner.lock();
        let st = g.stats;
        assert_eq!(
            st.run_pages + st.empty_pages + st.purged_pages,
            st.next_page,
            "{st:?}"
        );
        assert_eq!(st.next_page, g.next_page);
        let mut p = 0;
        let mut counts = HugeStats::default();
        let mut prev_free = false;
        let mut keys = 0;
        while p < g.next_page {
            let d = g.table.get(p);
            assert!(!d.is_null(), "page {p} has no extent");
            let (start, pages, state) = unsafe { ((*d).start(), (*d).pages(), (*d).state()) };
            assert_eq!(start, p, "extent at {p} starts at {start}");
            assert!(pages > 0);
            keys += 1;
            counts.add(state, pages);
            match state {
                RunState::Active => {
                    assert_eq!(
                        unsafe { (*d).usable },
                        (pages - GUARD_PAGES) << self.lg_page
                    );
                    for q in p..p + pages {
                        assert!(!g.free.map.is_set(q), "page {q} of run {p} marked free");
                    }
                    prev_free = false;
                }
                _ => {
                    assert!(!prev_free, "adjacent free extents at {p}");
                    for q in p..p + pages {
                        assert!(
                            g.free.map.is_set(q),
                            "page {q} of free extent {p} not marked free"
                        );
                    }
                    if pages > 1 {
                        assert_eq!(
                            g.table.get(p + pages - 1),
                            d,
                            "last-page key of {p} missing"
                        );
                        keys += 1;
                    }
                    prev_free = true;
                }
            }
            p += pages;
        }
        assert_eq!(p, g.next_page);
        assert_eq!((counts.runs, counts.run_pages), (st.runs, st.run_pages));
        assert_eq!(
            (counts.empty, counts.empty_pages),
            (st.empty, st.empty_pages)
        );
        assert_eq!(
            (counts.purged, counts.purged_pages),
            (st.purged, st.purged_pages)
        );
        assert_eq!(keys, g.table.count, "table holds stale keys");
        assert_eq!(g.free.map.lowest_set_from(g.next_page), None);
        // The Empty list holds exactly the Empty extents, oldest first.
        let mut n = 0;
        let mut d = g.empty_head;
        let mut last_dirty = 0;
        let mut prev = ptr::null_mut();
        while !d.is_null() {
            unsafe {
                assert_eq!((*d).state(), RunState::Empty);
                assert!((*d).dirty_at >= last_dirty, "empty list out of order");
                assert_eq!((*d).prev, prev);
                last_dirty = (*d).dirty_at;
                prev = d;
                d = (*d).next;
            }
        }
        assert_eq!(prev, g.empty_tail);
        d = g.empty_head;
        while !d.is_null() {
            n += 1;
            d = unsafe { (*d).next };
        }
        assert_eq!(n, st.empty);
    }
}

/// Initialise a metadata block as a free extent.
fn init_free(d: *mut HugeDesc, start: usize, pages: usize, state: RunState, dirty_at: u64) {
    debug_assert!(state.is_free());
    // SAFETY: `d` is a fresh metadata block for a `HugeDesc`.
    unsafe {
        ptr::write(
            d,
            HugeDesc {
                start: start as u32,
                pages: pages as u32,
                arena: 0,
                state: state as u8,
                _pad: 0,
                usable: 0,
                dirty_at,
                prev: ptr::null_mut(),
                next: ptr::null_mut(),
            },
        );
    }
}

#[cold]
#[inline(never)]
fn not_a_run(p: usize) -> ! {
    panic!("invalid free of {p:#x}: not the start of a huge allocation")
}

#[cold]
#[inline(never)]
fn already_free(p: usize) -> ! {
    panic!("double free of {p:#x}: huge allocation already freed")
}

/// The `check_zero` abort: a huge run handed out as zero has a dirty word.
#[cold]
#[inline(never)]
fn dirty_zero_run(
    run: usize,
    pages: usize,
    page: usize,
    prior: &str,
    arena: u16,
    at: usize,
    word: u64,
) -> ! {
    let advice = purge::advice_name();
    panic!(
        "check_zero: huge run {run:#x} ({pages} pages from page {page}, arena {arena}) was handed out as zero from {prior} pages but the word at {at:#x} reads {word:#x} (purge advice {advice})"
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;
