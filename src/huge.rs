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
//! pages is exactly one extent. Free pages are also mirrored in a
//! [`TwoLevelBitmap`] so that allocation is lowest-address first fit: walk
//! the free extents in address order, take the first that fits (or the last
//! one if it touches the high-water mark and can grow), else carve fresh
//! pages at the mark. Descriptors live in an open-addressing hash table
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
//! Alignment above a page is met by searching for `need + align_pages - 1`
//! pages and taking the aligned run inside; the pages before and after stay
//! free extents. Runs whose usable size exceeds `retain_max` (default
//! 64 MiB) are `MADV_DONTNEED`ed the moment they are freed; smaller ones
//! become `Empty` and wait for [`Huge::purge_empty`], which the decay driver
//! (Phase 1) calls with a `dirty_at` cutoff. `Empty` extents sit in a FIFO
//! list ordered by `dirty_at`, so purging is a pop from the head.
//!
//! Everything is under one futex lock: huge operations are rare compared to
//! slab operations and each already costs at least one syscall.

use core::alloc::Layout;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::class::Set;
use crate::layout::Regions;
use crate::lock::{Lock, RawLock};
use crate::meta::{self, Meta};
use crate::purge::{self, Batch};
use crate::sys::Errno;

/// Guard pages after every run.
pub const GUARD_PAGES: usize = 1;
/// Runs with more usable bytes than this are purged as soon as they are
/// freed (`huge_retain_max`).
pub const DEFAULT_RETAIN_MAX: usize = 64 << 20;
/// `log2` of the initial descriptor table capacity.
const LG_INITIAL_TABLE: u32 = 10;

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
    #[inline]
    const fn from_u8(v: u8) -> RunState {
        match v {
            0 => RunState::Active,
            1 => RunState::Empty,
            _ => RunState::Purged,
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
    const LAYOUT: Layout = Layout::new::<HugeDesc>();

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
    pub fn usable(&self) -> usize {
        self.usable
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    pub fn state(&self) -> RunState {
        RunState::from_u8(self.state)
    }

    #[inline]
    pub fn dirty_at(&self) -> u64 {
        self.dirty_at
    }
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
    /// Descriptor table capacity, for diagnostics.
    pub table_cap: usize,
}

impl HugeStats {
    fn add_free(&mut self, state: RunState, pages: usize) {
        match state {
            RunState::Empty => {
                self.empty += 1;
                self.empty_pages += pages;
            }
            RunState::Purged => {
                self.purged += 1;
                self.purged_pages += pages;
            }
            RunState::Active => unreachable!(),
        }
    }

    fn sub_free(&mut self, state: RunState, pages: usize) {
        match state {
            RunState::Empty => {
                self.empty -= 1;
                self.empty_pages -= pages;
            }
            RunState::Purged => {
                self.purged -= 1;
                self.purged_pages -= pages;
            }
            RunState::Active => unreachable!(),
        }
    }
}

/// One hash table slot: the key it was inserted under and the descriptor.
/// A null descriptor means empty.
#[derive(Clone, Copy)]
struct Entry {
    key: usize,
    desc: *mut HugeDesc,
}

/// Open-addressing hash table with linear probing and backward-shift
/// deletion. Load is kept at or below one half by [`Table::ensure`].
struct Table {
    entries: *mut Entry,
    lg_cap: u32,
    count: usize,
    /// Storage came from `Meta::alloc` (and can be freed) rather than a
    /// reservation.
    binned: bool,
}

impl Table {
    fn storage(meta: &Meta, page: usize, lg_cap: u32) -> Option<(*mut Entry, bool)> {
        let bytes = core::mem::size_of::<Entry>() << lg_cap;
        if bytes <= meta::MAX_BINNED {
            let layout = Layout::from_size_align(bytes, 16).ok()?;
            let p = meta.alloc_zeroed(layout);
            if p.is_null() { None } else { Some((p as *mut Entry, true)) }
        } else {
            let p = meta.reserve_committed(bytes, page)?;
            Some((p as *mut Entry, false))
        }
    }

    fn new(meta: &Meta, page: usize) -> Option<Table> {
        let (entries, binned) = Table::storage(meta, page, LG_INITIAL_TABLE)?;
        Some(Table { entries, lg_cap: LG_INITIAL_TABLE, count: 0, binned })
    }

    #[inline]
    fn cap(&self) -> usize {
        1 << self.lg_cap
    }

    #[inline]
    fn hash(&self, key: usize) -> usize {
        ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - self.lg_cap)) as usize
    }

    #[inline]
    fn slot(&self, i: usize) -> &mut Entry {
        debug_assert!(i < self.cap());
        // SAFETY: `i < cap` and the storage is `cap` entries that only this
        // table (behind the huge lock) touches.
        unsafe { &mut *self.entries.add(i) }
    }

    /// Slot index holding `key`, if present.
    fn find(&self, key: usize) -> Option<usize> {
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        loop {
            let e = self.slot(i);
            if e.desc.is_null() {
                return None;
            }
            if e.key == key {
                return Some(i);
            }
            i = (i + 1) & mask;
        }
    }

    fn get(&self, key: usize) -> *mut HugeDesc {
        match self.find(key) {
            Some(i) => self.slot(i).desc,
            None => ptr::null_mut(),
        }
    }

    /// Insert `key`, which must not be present. The table must not be full.
    fn insert(&mut self, key: usize, desc: *mut HugeDesc) {
        assert!(self.count < self.cap(), "huge: descriptor table full");
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        loop {
            let e = self.slot(i);
            if e.desc.is_null() {
                *e = Entry { key, desc };
                self.count += 1;
                return;
            }
            debug_assert!(e.key != key, "huge: key inserted twice");
            i = (i + 1) & mask;
        }
    }

    /// Remove `key`, which must be present.
    fn remove(&mut self, key: usize) {
        let Some(mut i) = self.find(key) else {
            panic!("huge: descriptor key {key} missing from the table");
        };
        let mask = self.cap() - 1;
        // Backward shift: pull later entries of the same probe chain into
        // the hole unless their ideal slot lies past it.
        let mut j = i;
        loop {
            j = (j + 1) & mask;
            let e = *self.slot(j);
            if e.desc.is_null() {
                break;
            }
            let ideal = self.hash(e.key);
            let stays = if i <= j { i < ideal && ideal <= j } else { i < ideal || ideal <= j };
            if !stays {
                *self.slot(i) = e;
                i = j;
            }
        }
        *self.slot(i) = Entry { key: 0, desc: ptr::null_mut() };
        self.count -= 1;
    }

    /// Make room for `extra` more keys at a load of one half or less. On
    /// failure the table is unchanged and still works, only fuller.
    fn ensure(&mut self, meta: &Meta, page: usize, extra: usize) -> Result<(), ()> {
        if (self.count + extra) * 2 <= self.cap() {
            return Ok(());
        }
        let mut lg = self.lg_cap + 1;
        while (self.count + extra) * 2 > (1usize << lg) {
            lg += 1;
        }
        let (entries, binned) = Table::storage(meta, page, lg).ok_or(())?;
        let old = Table { entries: self.entries, lg_cap: self.lg_cap, count: self.count, binned: self.binned };
        *self = Table { entries, lg_cap: lg, count: 0, binned };
        for i in 0..old.cap() {
            let e = *old.slot(i);
            if !e.desc.is_null() {
                self.insert(e.key, e.desc);
            }
        }
        if old.binned {
            let bytes = core::mem::size_of::<Entry>() << old.lg_cap;
            meta.free(old.entries as *mut u8, Layout::from_size_align(bytes, 16).unwrap());
        }
        // A reserved table cannot be given back; it is leaked (at most the
        // size of the final table in total).
        Ok(())
    }
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

/// The huge allocator: one per process (per arena in Phase 1).
pub struct Huge {
    base: usize,
    lg_page: u32,
    npages: usize,
    guard_install: bool,
    retain_max: AtomicUsize,
    /// `dirty_at` of the head of the `Empty` list (its oldest member), 0
    /// when the list is empty. Kept in step with the list under the lock
    /// by `sync_oldest` and read without it by the decay driver.
    oldest_empty: AtomicU64,
    inner: Lock<Inner>,
}

#[cfg(target_os = "linux")]
mod os {
    use crate::sys::{
        self, Errno, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE, PROT_NONE, PROT_READ, PROT_WRITE,
    };

    pub fn commit(addr: usize, len: usize) -> Result<(), Errno> {
        // SAFETY: inside our own reservation; adding permissions to pages
        // nobody holds cannot break anything.
        unsafe { sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
    }

    pub fn protect_none(addr: usize, len: usize) -> Result<(), Errno> {
        // SAFETY: the page is a guard that no caller may touch.
        unsafe { sys::mprotect(addr, len, PROT_NONE) }
    }

    pub fn guard_install(addr: usize, len: usize) -> Result<(), Errno> {
        // SAFETY: as `protect_none`.
        unsafe { sys::madvise(addr, len, MADV_GUARD_INSTALL) }
    }

    pub fn guard_remove(addr: usize, len: usize) -> Result<(), Errno> {
        // SAFETY: the run is being freed; the guard protects nothing now.
        unsafe { sys::madvise(addr, len, MADV_GUARD_REMOVE) }
    }

    pub fn now_ns() -> u64 {
        sys::now_coarse_ns()
    }
}

/// Off Linux nothing maps memory; host tests exercise the extent logic
/// over addresses that are never dereferenced.
#[cfg(not(target_os = "linux"))]
mod os {
    use crate::sys::Errno;
    use core::sync::atomic::{AtomicU64, Ordering};

    pub fn commit(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn protect_none(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn guard_install(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    pub fn guard_remove(_addr: usize, _len: usize) -> Result<(), Errno> {
        Ok(())
    }

    static TICK: AtomicU64 = AtomicU64::new(0);

    pub fn now_ns() -> u64 {
        TICK.fetch_add(1, Ordering::Relaxed) + 1
    }
}

impl Inner {
    fn insert_keys(&mut self, d: *mut HugeDesc) {
        // SAFETY: `d` is a live descriptor owned by this allocator.
        let (start, pages) = unsafe { ((*d).start(), (*d).pages()) };
        self.table.insert(start, d);
        if pages > 1 {
            self.table.insert(start + pages - 1, d);
        }
    }

    fn remove_keys(&mut self, d: *mut HugeDesc) {
        // SAFETY: as `insert_keys`.
        let (start, pages) = unsafe { ((*d).start(), (*d).pages()) };
        self.table.remove(start);
        if pages > 1 {
            self.table.remove(start + pages - 1);
        }
    }

    /// Append to the `Empty` list; `d.dirty_at` must be the newest.
    fn link_tail(&mut self, d: *mut HugeDesc) {
        // SAFETY: `d` is live and not in the list.
        unsafe {
            (*d).prev = self.empty_tail;
            (*d).next = ptr::null_mut();
            if self.empty_tail.is_null() {
                self.empty_head = d;
            } else {
                (*self.empty_tail).next = d;
            }
            self.empty_tail = d;
        }
    }

    /// Insert `d` right after `after`, which is in the list.
    fn link_after(&mut self, after: *mut HugeDesc, d: *mut HugeDesc) {
        // SAFETY: both are live; `after` is linked, `d` is not.
        unsafe {
            (*d).prev = after;
            (*d).next = (*after).next;
            if (*after).next.is_null() {
                self.empty_tail = d;
            } else {
                (*(*after).next).prev = d;
            }
            (*after).next = d;
        }
    }

    fn unlink(&mut self, d: *mut HugeDesc) {
        // SAFETY: `d` is live and in the list.
        unsafe {
            if (*d).prev.is_null() {
                self.empty_head = (*d).next;
            } else {
                (*(*d).prev).next = (*d).next;
            }
            if (*d).next.is_null() {
                self.empty_tail = (*d).prev;
            } else {
                (*(*d).next).prev = (*d).prev;
            }
            (*d).prev = ptr::null_mut();
            (*d).next = ptr::null_mut();
        }
    }

    /// The free extent starting at `page`, which must be a set free bit.
    fn extent_at(&self, page: usize) -> *mut HugeDesc {
        let d = self.table.get(page);
        // SAFETY: table entries are live descriptors.
        if d.is_null() || unsafe { (*d).start() != page || !(*d).state().is_free() } {
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
        // SAFETY: as `extent_at`.
        if !d.is_null() && unsafe { (*d).state().is_free() && (*d).end() == page } {
            d
        } else {
            ptr::null_mut()
        }
    }

    /// Neighbouring free extent starting exactly at `page`, if any.
    fn free_after(&self, page: usize) -> *mut HugeDesc {
        let d = self.table.get(page);
        // SAFETY: as `extent_at`.
        if !d.is_null() && unsafe { (*d).state().is_free() && (*d).start() == page } {
            d
        } else {
            ptr::null_mut()
        }
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
        assert!(npages > 0 && npages <= u32::MAX as usize, "huge: region page count out of range");
        let free = Set::new(meta, page, npages)?;
        let table = Table::new(meta, page)?;
        let table_cap = table.cap();
        Some(Huge {
            base: regions.huge_base,
            lg_page: regions.lg_page,
            npages,
            guard_install,
            retain_max: AtomicUsize::new(DEFAULT_RETAIN_MAX),
            oldest_empty: AtomicU64::new(0),
            inner: Lock::new(Inner {
                free,
                table,
                next_page: 0,
                empty_head: ptr::null_mut(),
                empty_tail: ptr::null_mut(),
                stats: HugeStats { table_cap, ..HugeStats::default() },
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
    #[inline]
    pub fn npages(&self) -> usize {
        self.npages
    }

    #[inline]
    pub fn guard_install(&self) -> bool {
        self.guard_install
    }

    /// Usable-byte threshold above which freed runs are purged at once.
    pub fn retain_max(&self) -> usize {
        self.retain_max.load(Ordering::Relaxed)
    }

    pub fn set_retain_max(&self, bytes: usize) {
        self.retain_max.store(bytes, Ordering::Relaxed);
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
        Some(self.usable_pages(size)? << self.lg_page)
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

    /// Allocate `size` bytes aligned to `align` (a power of two; anything
    /// up to a page is free). Returns the pointer and whether the memory is
    /// known to read as zero, or null when the region, the metadata region
    /// or the kernel refuses.
    pub fn alloc(&self, meta: &Meta, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        assert!(align.is_power_of_two(), "huge: alignment not a power of two");
        let Some(usable_pages) = self.usable_pages(size) else { return (ptr::null_mut(), false) };
        let Some(need) = usable_pages.checked_add(GUARD_PAGES) else { return (ptr::null_mut(), false) };
        if need > self.npages || align > (self.npages << self.lg_page) {
            return (ptr::null_mut(), false);
        }
        let page = self.page_size();
        let mut g = self.inner.lock();
        let Some((s, ext)) = self.find(&g, need, align) else { return (ptr::null_mut(), false) };
        let end = s + need;

        // Everything fallible happens before any state changes.
        let run = meta.alloc(HugeDesc::LAYOUT) as *mut HugeDesc;
        if run.is_null() {
            return (ptr::null_mut(), false);
        }
        let spare = meta.alloc(HugeDesc::LAYOUT) as *mut HugeDesc;
        if spare.is_null() {
            meta.free(run as *mut u8, HugeDesc::LAYOUT);
            return (ptr::null_mut(), false);
        }
        let ok = g.table.ensure(meta, page, 3).is_ok()
            && (end <= g.next_page || g.free.ensure(meta, page, end - 1).is_ok())
            && self.prepare(s, usable_pages, g.next_page).is_ok();
        if !ok {
            meta.free(run as *mut u8, HugeDesc::LAYOUT);
            meta.free(spare as *mut u8, HugeDesc::LAYOUT);
            return (ptr::null_mut(), false);
        }

        let zeroed = self.take(&mut g, meta, s, need, ext, run, spare, arena);
        self.sync_oldest(&g);
        (self.addr(s) as *mut u8, zeroed)
    }

    /// Refresh `oldest_empty` from the list head. Returns `true` when the
    /// list went from empty to non-empty, which is when a background
    /// thread sleeping without a deadline needs waking.
    fn sync_oldest(&self, g: &Inner) -> bool {
        // SAFETY: the head, if any, is a live `Empty` extent.
        let new = if g.empty_head.is_null() { 0 } else { unsafe { (*g.empty_head).dirty_at } };
        let old = self.oldest_empty.swap(new, Ordering::Relaxed);
        old == 0 && new != 0
    }

    /// Lowest-address first fit for `need` pages at `align`. Returns the
    /// start page and the free extent it comes from (`None` for fresh pages
    /// at the high-water mark). The extent may end below `s + need` only if
    /// it touches the mark.
    fn find(&self, g: &Inner, need: usize, align: usize) -> Option<(usize, *mut HugeDesc)> {
        let mut from = 0;
        while let Some(a) = g.free.map.lowest_set_from(from) {
            let e = g.extent_at(a);
            // SAFETY: `extent_at` checked `e` is live.
            let e_end = unsafe { (*e).end() };
            let s = self.align_page(a, align);
            if s + need <= e_end {
                return Some((s, e));
            }
            if e_end == g.next_page {
                return if s + need <= self.npages { Some((s, e)) } else { None };
            }
            from = e_end;
        }
        let s = self.align_page(g.next_page, align);
        if s + need <= self.npages { Some((s, ptr::null_mut())) } else { None }
    }

    /// Kernel side of an allocation of `usable` pages at `s`: commit and
    /// place the guard. Pages below `watermark` are committed already
    /// (apart from `PROT_NONE` guards in fallback mode).
    fn prepare(&self, s: usize, usable: usize, watermark: usize) -> Result<(), Errno> {
        let page = self.page_size();
        let guard = s + usable;
        let end = guard + GUARD_PAGES;
        if self.guard_install {
            let from = s.max(watermark);
            if from < end {
                os::commit(self.addr(from), (end - from) * page)?;
            }
            os::guard_install(self.addr(guard), GUARD_PAGES * page)
        } else {
            os::commit(self.addr(s), usable * page)?;
            if guard < watermark {
                os::protect_none(self.addr(guard), GUARD_PAGES * page)?;
            }
            Ok(())
        }
    }

    /// Record the run `[s, s + need)` taken from `ext` (or fresh pages),
    /// splitting the extent around it. Infallible. Returns the zero flag.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &self,
        g: &mut Inner,
        meta: &Meta,
        s: usize,
        need: usize,
        ext: *mut HugeDesc,
        run: *mut HugeDesc,
        spare: *mut HugeDesc,
        arena: u16,
    ) -> bool {
        let end = s + need;
        let old_mark = g.next_page;
        let mut spare_used = false;
        let mut zeroed = true;
        if ext.is_null() {
            if s > old_mark {
                // Alignment left a hole at the mark: a clean free extent.
                init_free(spare, old_mark, s - old_mark, RunState::Purged, 0);
                g.insert_keys(spare);
                g.free.map.set_range(old_mark, s - old_mark);
                g.stats.add_free(RunState::Purged, s - old_mark);
                spare_used = true;
            }
        } else {
            // SAFETY: `ext` is a live free extent found under this lock.
            let (a, e_end, st, dirty_at) = unsafe { ((*ext).start(), (*ext).end(), (*ext).state(), (*ext).dirty_at) };
            // `Purged` pages are zero unless `lazy_free` discarded them
            // with `MADV_FREE`; pages at or past the mark are untouched.
            zeroed = (st == RunState::Purged && purge::purged_is_zero()) || s >= e_end;
            g.remove_keys(ext);
            g.stats.sub_free(st, e_end - a);
            let taken_lo = s.max(a);
            let taken_hi = end.min(e_end);
            if taken_lo < taken_hi {
                g.free.map.clear_range(taken_lo, taken_hi - taken_lo);
            }
            let tail = e_end.saturating_sub(end);
            if tail > 0 {
                init_free(spare, end, tail, st, dirty_at);
                g.insert_keys(spare);
                g.stats.add_free(st, tail);
                if st == RunState::Empty {
                    g.link_after(ext, spare);
                }
                spare_used = true;
            }
            if s > a {
                // Head survives (and, when `s >= e_end`, absorbs the hole
                // between the old extent and the aligned start).
                if s > e_end {
                    g.free.map.set_range(e_end, s - e_end);
                }
                // SAFETY: as above.
                unsafe { (*ext).pages = (s - a) as u32 };
                g.insert_keys(ext);
                g.stats.add_free(st, s - a);
            } else {
                if st == RunState::Empty {
                    g.unlink(ext);
                }
                meta.free(ext as *mut u8, HugeDesc::LAYOUT);
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
        g.table.insert(s, run);
        g.stats.runs += 1;
        g.stats.run_pages += need;
        g.stats.next_page = g.next_page;
        g.stats.table_cap = g.table.cap();
        if !spare_used {
            meta.free(spare as *mut u8, HugeDesc::LAYOUT);
        }
        zeroed
    }

    /// Free the run starting at `page` (from `Regions::classify`). Aborts
    /// unless it is the start of a live run. Returns the usable bytes
    /// released, for the caller's counters.
    pub fn free(&self, meta: &Meta, page: usize) -> usize {
        self.free_run(meta, page).usable
    }

    /// [`Huge::free`] returning the run as it was (`start`, `pages` with
    /// the guard, `usable`, `arena`), for the per-arena counters.
    pub fn free_run(&self, meta: &Meta, page: usize) -> RunInfo {
        let page_size = self.page_size();
        let mut g = self.inner.lock();
        let d = g.table.get(page);
        if d.is_null() {
            not_a_run(self.addr(page));
        }
        // SAFETY: table entries are live descriptors.
        let (start, n, state, usable, arena) =
            unsafe { ((*d).start(), (*d).pages(), (*d).state(), (*d).usable, (*d).arena()) };
        if state != RunState::Active {
            already_free(self.addr(page));
        }
        if start != page {
            not_a_run(self.addr(page));
        }
        let addr = self.addr(page);
        if self.guard_install {
            if let Err(e) = os::guard_remove(addr + (n - GUARD_PAGES) * page_size, GUARD_PAGES * page_size) {
                panic!("huge: madvise(MADV_GUARD_REMOVE) of {addr:#x} failed: {e}");
            }
        }
        let now = os::now_ns().max(1);
        let st = if usable > self.retain_max() {
            let mut batch = Batch::new();
            batch.push(addr, n * page_size);
            batch.flush();
            RunState::Purged
        } else {
            RunState::Empty
        };
        // Best effort: a fuller table still works, only slower.
        let _ = g.table.ensure(meta, page_size, 2);

        g.table.remove(page);
        g.stats.runs -= 1;
        g.stats.run_pages -= n;
        // SAFETY: `d` is ours; it is now a free extent candidate.
        unsafe {
            (*d).state = st as u8;
            (*d).usable = 0;
            (*d).dirty_at = if st == RunState::Empty { now } else { 0 };
        }

        // Coalesce with free neighbours. The survivor is the `Empty`
        // participant with the oldest `dirty_at` (it keeps its place in
        // the FIFO list), else `d`.
        let pred = g.free_before(page);
        let succ = g.free_after(page + n);
        let mut lo = page;
        let mut hi = page + n;
        let mut merged = st;
        let mut survivor = d;
        for nb in [pred, succ] {
            if nb.is_null() {
                continue;
            }
            // SAFETY: neighbours are live free extents.
            let (ns, ne, nst, ndirty) = unsafe { ((*nb).start(), (*nb).end(), (*nb).state(), (*nb).dirty_at) };
            g.remove_keys(nb);
            g.stats.sub_free(nst, ne - ns);
            lo = lo.min(ns);
            hi = hi.max(ne);
            if nst == RunState::Empty {
                merged = RunState::Empty;
                // SAFETY: `survivor` is live.
                let sdirty = unsafe { if (*survivor).state() == RunState::Empty { (*survivor).dirty_at } else { u64::MAX } };
                if survivor == d || ndirty < sdirty {
                    survivor = nb;
                }
            }
        }
        for x in [d, pred, succ] {
            if x.is_null() || x == survivor {
                continue;
            }
            // SAFETY: `x` is live and about to be discarded.
            if x != d && unsafe { (*x).state() } == RunState::Empty {
                g.unlink(x);
            }
            meta.free(x as *mut u8, HugeDesc::LAYOUT);
        }
        // SAFETY: `survivor` is live.
        unsafe {
            (*survivor).start = lo as u32;
            (*survivor).pages = (hi - lo) as u32;
            (*survivor).state = merged as u8;
            (*survivor).usable = 0;
            if merged == RunState::Purged {
                (*survivor).dirty_at = 0;
            } else if survivor == d {
                (*survivor).dirty_at = now;
                g.link_tail(d);
            }
        }
        g.insert_keys(survivor);
        g.free.map.set_range(page, n);
        g.stats.add_free(merged, hi - lo);
        g.stats.table_cap = g.table.cap();
        let first = self.sync_oldest(&g);
        drop(g);
        if first {
            purge::note_empty();
        }
        RunInfo { start, pages: n, usable, arena, state: RunState::Active, dirty_at: 0 }
    }

    /// Usable bytes of the live run starting at `page`. Aborts otherwise.
    pub fn usable_size(&self, page: usize) -> usize {
        let g = self.inner.lock();
        let d = g.table.get(page);
        // SAFETY: table entries are live descriptors.
        if d.is_null() || unsafe { (*d).start() != page } {
            not_a_run(self.addr(page));
        }
        // SAFETY: as above.
        unsafe {
            if (*d).state() != RunState::Active {
                already_free(self.addr(page));
            }
            (*d).usable
        }
    }

    /// Descriptor of the extent starting or ending at `page`, if any.
    pub fn lookup(&self, page: usize) -> Option<RunInfo> {
        let g = self.inner.lock();
        let d = g.table.get(page);
        if d.is_null() {
            return None;
        }
        // SAFETY: table entries are live descriptors.
        unsafe {
            Some(RunInfo {
                start: (*d).start(),
                pages: (*d).pages(),
                usable: (*d).usable,
                arena: (*d).arena,
                state: (*d).state(),
                dirty_at: (*d).dirty_at,
            })
        }
    }

    /// Purge every `Empty` extent whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all): `MADV_DONTNEED`, then `Purged`. Returns the
    /// number purged.
    pub fn purge_empty(&self, cutoff: u64) -> usize {
        let page = self.page_size();
        let mut g = self.inner.lock();
        let mut n = 0;
        let mut batch = Batch::new();
        loop {
            let d = g.empty_head;
            if d.is_null() {
                break;
            }
            // SAFETY: list members are live `Empty` extents.
            let (start, pages, dirty_at) = unsafe { ((*d).start(), (*d).pages(), (*d).dirty_at) };
            if dirty_at > cutoff {
                break;
            }
            // Discarded in batches before the lock is released, so no
            // allocation can take the extent between the state change and
            // the discard.
            batch.push(self.addr(start), pages * page);
            g.unlink(d);
            // SAFETY: as above.
            unsafe {
                (*d).state = RunState::Purged as u8;
                (*d).dirty_at = 0;
            }
            g.stats.sub_free(RunState::Empty, pages);
            g.stats.add_free(RunState::Purged, pages);
            n += 1;
        }
        batch.flush();
        self.sync_oldest(&g);
        n
    }

    /// `dirty_at` of the oldest `Empty` extent as the decay driver sees it
    /// (0 when none).
    pub fn oldest_empty(&self) -> u64 {
        self.oldest_empty.load(Ordering::Relaxed)
    }

    /// Decay step (design section 8), as `ClassState::decay`: purge every
    /// extent `Empty` for `decay_ns` or longer at `now` and return the next
    /// deadline, `None` when nothing is `Empty`. Lock-free unless due.
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        let oldest = self.oldest_empty();
        if oldest == 0 {
            return None;
        }
        let deadline = oldest.saturating_add(decay_ns);
        if now < deadline {
            return Some(deadline);
        }
        self.purge_empty(now.saturating_sub(decay_ns));
        let oldest = self.oldest_empty();
        if oldest == 0 { None } else { Some(oldest.saturating_add(decay_ns)) }
    }

    pub fn stats(&self) -> HugeStats {
        self.inner.lock().stats
    }

    /// Oldest `Empty` extent's `dirty_at`, for the decay driver.
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
        assert_eq!(st.run_pages + st.empty_pages + st.purged_pages, st.next_page, "{st:?}");
        assert_eq!(st.next_page, g.next_page);
        let mut p = 0;
        let mut runs = 0;
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
            match state {
                RunState::Active => {
                    runs += 1;
                    counts.run_pages += pages;
                    assert_eq!(unsafe { (*d).usable }, (pages - GUARD_PAGES) << self.lg_page);
                    for q in p..p + pages {
                        assert!(!g.free.map.is_set(q), "page {q} of run {p} marked free");
                    }
                    prev_free = false;
                }
                free => {
                    assert!(!prev_free, "adjacent free extents at {p}");
                    counts.add_free(free, pages);
                    for q in p..p + pages {
                        assert!(g.free.map.is_set(q), "page {q} of free extent {p} not marked free");
                    }
                    if pages > 1 {
                        assert_eq!(g.table.get(p + pages - 1), d, "last-page key of {p} missing");
                        keys += 1;
                    }
                    prev_free = true;
                }
            }
            p += pages;
        }
        assert_eq!(p, g.next_page);
        assert_eq!(runs, st.runs);
        assert_eq!(counts.run_pages, st.run_pages);
        assert_eq!((counts.empty, counts.empty_pages), (st.empty, st.empty_pages));
        assert_eq!((counts.purged, counts.purged_pages), (st.purged, st.purged_pages));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Kind, LG_MAX_SLAB, MAX_SLAB, Mapping, NCLASSES};
    use std::sync::Arc;
    use std::vec;
    use std::vec::Vec;

    const PAGE: usize = 4096;
    const LG_REGION: u32 = LG_MAX_SLAB;
    const LG_STRIDE: u32 = LG_REGION + 2;
    const META_LEN: usize = 16 << 20;
    /// 64 MiB huge region: 16384 pages, small enough to exhaust.
    const HUGE_LEN: usize = 64 << 20;
    const HUGE_PAGES: usize = HUGE_LEN / PAGE;

    /// Test environment: a metadata region and a huge region. On Linux both
    /// are real `PROT_NONE` reservations so commits, guards and purges go
    /// through the kernel and run memory can be touched; elsewhere the
    /// metadata is a `Vec` and run addresses are never dereferenced.
    pub(super) struct Env {
        pub meta: Meta,
        pub regions: Regions,
        #[cfg(target_os = "linux")]
        maps: [(usize, usize); 2],
        #[cfg(not(target_os = "linux"))]
        _buf: Vec<u8>,
    }

    #[cfg(not(target_os = "linux"))]
    fn align_up(x: usize, a: usize) -> usize {
        (x + a - 1) & !(a - 1)
    }

    fn regions_at(huge: (usize, usize), meta: (usize, usize)) -> Regions {
        let superblock = 0x1000_0000_0000usize;
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
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
        pub fn new() -> Env {
            Env::with_meta_len(META_LEN)
        }

        #[cfg(target_os = "linux")]
        pub fn with_meta_len(meta_len: usize) -> Env {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let reserve = |len: usize| -> usize {
                unsafe {
                    crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
                }
                .expect("reserve")
            };
            // `mmap` only promises page alignment; the alignment tests
            // assume a `MAX_SLAB`-aligned huge base like the real layout.
            let hraw = reserve(HUGE_LEN + crate::layout::MAX_SLAB);
            let hb = (hraw + crate::layout::MAX_SLAB - 1) & !(crate::layout::MAX_SLAB - 1);
            let mb = reserve(meta_len);
            let regions = regions_at((hb, HUGE_LEN), (mb, meta_len));
            Env {
                meta: Meta::new(mb, meta_len, PAGE),
                regions,
                maps: [(hraw, HUGE_LEN + crate::layout::MAX_SLAB), (mb, meta_len)],
            }
        }

        #[cfg(not(target_os = "linux"))]
        pub fn new() -> Env {
            Env::with_meta_len(META_LEN)
        }

        #[cfg(not(target_os = "linux"))]
        pub fn with_meta_len(meta_len: usize) -> Env {
            let buf = vec![0u8; meta_len + PAGE];
            let mb = align_up(buf.as_ptr() as usize, PAGE);
            // A deliberately non-4 MiB-aligned base, like a real slid one.
            let regions = regions_at((0x2000_0000_1000, HUGE_LEN), (mb, meta_len));
            Env { meta: Meta::new(mb, meta_len, PAGE), regions, _buf: buf }
        }

        pub fn huge(&self) -> Huge {
            Huge::new(&self.meta, &self.regions, guard_mode()).expect("huge allocator")
        }

        pub fn alloc(&self, h: &Huge, size: usize, align: usize) -> (*mut u8, bool) {
            let r = h.alloc(&self.meta, size, align, 0);
            h.audit();
            r
        }

        pub fn free(&self, h: &Huge, p: *mut u8) -> usize {
            let page = match self.regions.classify(p as usize) {
                Kind::Huge { page } => page,
                other => panic!("{p:?} classified as {other:?}"),
            };
            let n = h.free(&self.meta, page);
            h.audit();
            n
        }

        pub fn page_of(&self, p: *mut u8) -> usize {
            match self.regions.classify(p as usize) {
                Kind::Huge { page } => page,
                other => panic!("{p:?} classified as {other:?}"),
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for Env {
        fn drop(&mut self) {
            for (addr, len) in self.maps {
                unsafe { crate::sys::munmap(addr, len) }.unwrap();
            }
        }
    }

    /// Guard mode under test: the probed kernel support on Linux, the
    /// install path off Linux (nothing maps memory there anyway).
    #[cfg(target_os = "linux")]
    pub(super) fn guard_mode() -> bool {
        crate::sys::features::probe_guard_install(PAGE)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn guard_mode() -> bool {
        true
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn desc_layout_fits_a_small_bin() {
        assert_eq!(core::mem::size_of::<HugeDesc>(), 48);
        assert_eq!(meta::bin_for(HugeDesc::LAYOUT), meta::bin_of(48));
    }

    #[test]
    fn first_run_is_at_the_base_and_zero() {
        let env = Env::new();
        let h = env.huge();
        assert_eq!(h.npages(), HUGE_PAGES);
        assert_eq!(h.usable_for(1), Some(PAGE));
        assert_eq!(h.usable_for(PAGE + 1), Some(2 * PAGE));
        assert_eq!(h.usable_for(usize::MAX), None);
        let (p, zero) = env.alloc(&h, 300 * 1024, 16);
        assert_eq!(p as usize, env.regions.huge_base);
        assert!(zero);
        assert_eq!(env.regions.classify(p as usize), Kind::Huge { page: 0 });
        let pages = (300usize * 1024).div_ceil(PAGE);
        let info = h.lookup(0).unwrap();
        assert_eq!(
            info,
            RunInfo { start: 0, pages: pages + GUARD_PAGES, usable: pages * PAGE, arena: 0, state: RunState::Active, dirty_at: 0 }
        );
        assert_eq!(h.usable_size(0), pages * PAGE);
        assert_eq!(h.lookup(pages), None, "guard page is not keyed");
        let st = h.stats();
        assert_eq!(st.runs, 1);
        assert_eq!(st.run_pages, pages + GUARD_PAGES);
        assert_eq!(st.next_page, pages + GUARD_PAGES);
        // The next run follows the guard.
        let (q, zero) = env.alloc(&h, 1, 1);
        assert!(zero);
        assert_eq!(env.page_of(q), pages + GUARD_PAGES);
        assert_eq!(h.usable_size(env.page_of(q)), PAGE);
        assert_eq!(env.free(&h, p), pages * PAGE);
        assert_eq!(env.free(&h, q), PAGE);
    }

    #[test]
    fn free_and_reuse_prefers_the_lowest_address() {
        let env = Env::new();
        let h = env.huge();
        let (a, _) = env.alloc(&h, 4 * PAGE, 1);
        let (b, _) = env.alloc(&h, 4 * PAGE, 1);
        let (c, _) = env.alloc(&h, 4 * PAGE, 1);
        assert_eq!(env.page_of(b), 5);
        assert_eq!(env.page_of(c), 10);
        env.free(&h, b);
        let info = h.lookup(5).unwrap();
        assert_eq!(info.state, RunState::Empty);
        assert_eq!((info.start, info.pages), (5, 5));
        assert!(info.dirty_at > 0);
        assert_eq!(h.lookup(9).unwrap().start, 5, "last page keyed");
        assert_eq!(h.stats().empty, 1);
        // A smaller run reuses the hole from its start and is not zero.
        let (d, zero) = env.alloc(&h, 2 * PAGE, 1);
        assert_eq!(env.page_of(d), 5);
        assert!(!zero);
        let rest = h.lookup(8).unwrap();
        assert_eq!((rest.start, rest.pages, rest.state), (8, 2, RunState::Empty));
        // Too big for the remaining hole: goes to the mark.
        let (e, zero) = env.alloc(&h, 3 * PAGE, 1);
        assert_eq!(env.page_of(e), 15);
        assert!(zero);
        // Purge makes the hole zero again.
        assert_eq!(h.purge_empty(u64::MAX), 1);
        assert_eq!(h.lookup(8).unwrap().state, RunState::Purged);
        let (f, zero) = env.alloc(&h, PAGE, 1);
        assert_eq!(env.page_of(f), 8);
        assert!(zero);
        assert_eq!(h.stats().purged, 0);
        for p in [a, c, d, e, f] {
            env.free(&h, p);
        }
        let st = h.stats();
        assert_eq!((st.runs, st.empty, st.empty_pages), (0, 1, st.next_page));
    }

    #[test]
    fn a_run_can_grow_past_the_mark_from_a_trailing_extent() {
        let env = Env::new();
        let h = env.huge();
        let (a, _) = env.alloc(&h, PAGE, 1);
        let (b, _) = env.alloc(&h, PAGE, 1);
        env.free(&h, b);
        // The Empty extent [2, 4) touches the mark; a 6-page run starts in
        // it and extends the mark. Dirty memory means not zero.
        let (c, zero) = env.alloc(&h, 5 * PAGE, 1);
        assert_eq!(env.page_of(c), 2);
        assert!(!zero);
        assert_eq!(h.stats().next_page, 8);
        assert_eq!(h.stats().empty, 0);
        env.free(&h, c);
        env.free(&h, a);
        assert_eq!(h.stats().empty, 1);
        assert_eq!(h.lookup(0).unwrap().pages, 8);
    }

    #[test]
    fn alignment_over_allocates_and_frees_head_and_tail() {
        let env = Env::new();
        let h = env.huge();
        let (a, _) = env.alloc(&h, PAGE, 1);
        let mut held = vec![a];
        for lg in 13..=22 {
            let align = 1usize << lg;
            let (p, zero) = env.alloc(&h, 3 * PAGE, align);
            assert!(!p.is_null());
            assert_eq!(p as usize % align, 0, "align {align}");
            assert!(zero);
            let page = env.page_of(p);
            let info = h.lookup(page).unwrap();
            assert_eq!((info.pages, info.usable, info.state), (3 + GUARD_PAGES, 3 * PAGE, RunState::Active));
            // The page before is either an unkeyed guard page or the last
            // page of a clean alignment hole.
            if let Some(before) = h.lookup(page - 1) {
                assert_eq!(before.state, RunState::Purged);
            }
            held.push(p);
        }
        let st = h.stats();
        assert_eq!(st.runs, held.len());
        assert!(st.purged > 0, "alignment holes should be Purged extents");
        // Aligned allocation inside an existing hole: free everything but
        // the first and the largest, then ask for 8 KiB alignment.
        let last = held.pop().unwrap();
        for p in held.drain(1..) {
            env.free(&h, p);
        }
        let (q, _) = env.alloc(&h, PAGE, 2 * PAGE);
        assert_eq!(q as usize % (2 * PAGE), 0);
        assert!(env.page_of(q) < env.page_of(last));
        env.free(&h, q);
        env.free(&h, last);
        env.free(&h, held[0]);
        assert_eq!(h.stats().runs, 0);
    }

    #[test]
    fn alignment_at_the_mark_leaves_a_clean_hole() {
        let env = Env::new();
        let h = env.huge();
        let (a, _) = env.alloc(&h, PAGE, 1);
        let (b, zero) = env.alloc(&h, PAGE, 1 << 20);
        assert!(zero);
        assert_eq!(b as usize % (1 << 20), 0);
        let hole = h.lookup(2).unwrap();
        assert_eq!(hole.state, RunState::Purged);
        assert_eq!(hole.end(), env.page_of(b));
        // The hole is reused first, and is zero.
        let (c, zero) = env.alloc(&h, PAGE, 1);
        assert_eq!(env.page_of(c), 2);
        assert!(zero);
        env.free(&h, a);
        env.free(&h, b);
        env.free(&h, c);
    }

    impl RunInfo {
        fn end(&self) -> usize {
            self.start + self.pages
        }
    }

    #[test]
    fn neighbours_coalesce_and_keep_the_oldest_dirty_time() {
        let env = Env::new();
        let h = env.huge();
        let (a, _) = env.alloc(&h, PAGE, 1);
        let (b, _) = env.alloc(&h, PAGE, 1);
        let (c, _) = env.alloc(&h, PAGE, 1);
        let (d, _) = env.alloc(&h, PAGE, 1);
        env.free(&h, a);
        let ta = h.lookup(0).unwrap().dirty_at;
        env.free(&h, c);
        assert_eq!(h.stats().empty, 2);
        env.free(&h, b);
        let st = h.stats();
        assert_eq!((st.empty, st.empty_pages), (1, 6));
        let m = h.lookup(0).unwrap();
        assert_eq!((m.start, m.pages, m.dirty_at), (0, 6, ta));
        assert_eq!(h.lookup(5).unwrap().start, 0);
        assert_eq!(h.lookup(2), None);
        assert_eq!(h.lookup(4), None);
        env.free(&h, d);
        assert_eq!(h.stats().empty, 1);
        assert_eq!(h.lookup(0).unwrap().pages, 8);
        assert_eq!(h.oldest_dirty_at(), Some(ta));
    }

    #[test]
    fn purged_and_empty_merge_to_empty() {
        let env = Env::new();
        let h = env.huge();
        h.set_retain_max(2 * PAGE);
        let (a, _) = env.alloc(&h, PAGE, 1);
        let (b, _) = env.alloc(&h, 3 * PAGE, 1);
        let (c, _) = env.alloc(&h, PAGE, 1);
        env.free(&h, b);
        assert_eq!(h.lookup(2).unwrap().state, RunState::Purged, "above retain_max: purged at once");
        assert_eq!(h.oldest_dirty_at(), None);
        env.free(&h, a);
        let m = h.lookup(0).unwrap();
        assert_eq!((m.pages, m.state), (6, RunState::Empty));
        assert_eq!(h.stats().purged, 0);
        env.free(&h, c);
        assert_eq!(h.lookup(0).unwrap().pages, 8);
        assert_eq!(h.purge_empty(u64::MAX), 1);
        assert_eq!(h.lookup(0).unwrap().state, RunState::Purged);
        assert_eq!(h.stats().empty, 0);
        let (p, zero) = env.alloc(&h, 8 * PAGE, 1);
        assert!(zero);
        assert_eq!(env.page_of(p), 0);
        assert_eq!(h.stats().next_page, 9);
    }

    /// The decay driver's view of the huge allocator: `oldest_empty`
    /// follows the list head through frees, purges and reuse.
    #[test]
    fn decay_deadlines_follow_the_empty_list_head() {
        let env = Env::new();
        let h = env.huge();
        assert_eq!(h.oldest_empty(), 0);
        assert_eq!(h.decay(u64::MAX, 1000), None);
        let mut ps = Vec::new();
        for _ in 0..4 {
            ps.push(env.alloc(&h, PAGE, 1).0);
        }
        // Free runs 0 and 2 (no coalescing) with a gap in time.
        env.free(&h, ps[0]);
        let t0 = h.oldest_dirty_at().unwrap();
        assert_eq!(h.oldest_empty(), t0);
        std::thread::sleep(core::time::Duration::from_millis(30));
        env.free(&h, ps[2]);
        let t2 = h.lookup(env.page_of(ps[2])).unwrap().dirty_at;
        assert!(t2 > t0);
        assert_eq!(h.oldest_empty(), t0);
        assert_eq!(h.decay(t0 + 999, 1000), Some(t0 + 1000));
        assert_eq!(h.stats().empty, 2);
        assert_eq!(h.decay(t0 + 1000, 1000), Some(t2 + 1000));
        assert_eq!(h.stats().empty, 1);
        assert_eq!(h.stats().purged, 1);
        assert_eq!(h.oldest_empty(), t2);
        // Reusing the last Empty extent (lowest fit is run 0's pages, now
        // Purged; ask for both so run 2's pages go too) clears the head.
        let (a, _) = env.alloc(&h, PAGE, 1);
        assert_eq!(a, ps[0]);
        assert_eq!(h.oldest_empty(), t2);
        let (b, zeroed) = env.alloc(&h, PAGE, 1);
        assert_eq!(b, ps[2]);
        assert!(!zeroed);
        assert_eq!(h.oldest_empty(), 0);
        assert_eq!(h.decay(u64::MAX / 2, 1000), None);
        // Everything freed and purged at once.
        for &p in &[a, b, ps[1], ps[3]] {
            env.free(&h, p);
        }
        assert!(h.oldest_empty() > 0);
        assert!(h.purge_empty(u64::MAX) >= 1);
        assert_eq!(h.oldest_empty(), 0);
        assert_eq!(h.stats().empty, 0);
        h.audit();
    }

    #[test]
    fn purge_respects_the_cutoff_in_fifo_order() {
        let env = Env::new();
        let h = env.huge();
        let mut ps = Vec::new();
        for _ in 0..6 {
            ps.push(env.alloc(&h, PAGE, 1).0);
        }
        // Free every other run so nothing coalesces: three Empty extents
        // with increasing dirty_at.
        let mut times = Vec::new();
        for &p in ps.iter().step_by(2) {
            // The coarse clock ticks every few milliseconds on Linux; off
            // Linux it is a counter and the sleep is just short.
            std::thread::sleep(core::time::Duration::from_millis(30));
            env.free(&h, p);
            times.push(h.lookup(env.page_of(p)).unwrap().dirty_at);
        }
        assert!(times[0] < times[1] && times[1] < times[2]);
        assert_eq!(h.oldest_dirty_at(), Some(times[0]));
        assert_eq!(h.purge_empty(times[0] - 1), 0);
        assert_eq!(h.purge_empty(times[1]), 2);
        h.audit();
        assert_eq!(h.oldest_dirty_at(), Some(times[2]));
        assert_eq!(h.stats().purged, 2);
        assert_eq!(h.purge_empty(u64::MAX), 1);
        assert_eq!(h.stats(), HugeStats { runs: 3, run_pages: 6, purged: 3, purged_pages: 6, next_page: 12, table_cap: 1024, ..HugeStats::default() });
        for &p in ps.iter().skip(1).step_by(2) {
            env.free(&h, p);
        }
        assert_eq!(h.stats().empty, 1);
    }

    #[test]
    fn exhaustion_returns_null_and_recovers() {
        let env = Env::new();
        let h = env.huge();
        let mut held = Vec::new();
        loop {
            let (p, _) = env.alloc(&h, 1 << 20, 1);
            if p.is_null() {
                break;
            }
            held.push(p);
        }
        // 64 MiB of 1 MiB + guard runs: 63 fit, the 64th would need 257 pages.
        assert_eq!(held.len(), HUGE_PAGES / 257);
        let st = h.stats();
        assert!(st.next_page <= HUGE_PAGES);
        // Something small still fits in the leftover.
        let (s, _) = env.alloc(&h, PAGE, 1);
        assert!(!s.is_null());
        held.push(s);
        for p in held.drain(..) {
            env.free(&h, p);
        }
        let st = h.stats();
        assert_eq!((st.runs, st.empty), (0, 1));
        // The whole region minus a guard is one run now.
        let (big, zero) = env.alloc(&h, HUGE_LEN - PAGE, 1);
        assert_eq!(env.page_of(big), 0);
        assert!(!zero);
        assert!(env.alloc(&h, 1, 1).0.is_null());
        env.free(&h, big);
        assert!(env.alloc(&h, HUGE_LEN, 1).0.is_null(), "no room for the guard");
        assert!(env.alloc(&h, PAGE, 2 * HUGE_LEN).0.is_null(), "alignment beyond the region");
    }

    #[test]
    fn table_grows_and_lookups_survive_backward_shift() {
        let env = Env::new();
        let h = env.huge();
        // 3000 one-page runs: 6000 pages, 3000 start keys, well past the
        // initial 1024 slots at half load.
        let mut runs = Vec::new();
        for _ in 0..3000 {
            let (p, _) = h.alloc(&env.meta, 1, 1, 7);
            assert!(!p.is_null());
            runs.push(env.page_of(p));
        }
        h.audit();
        let st = h.stats();
        assert!(st.table_cap >= 8192, "table did not grow: {st:?}");
        for (i, &page) in runs.iter().enumerate() {
            assert_eq!(page, 2 * i);
            let info = h.lookup(page).unwrap();
            assert_eq!((info.pages, info.arena, info.state), (2, 7, RunState::Active));
        }
        // Free every third run: many removals with shifts, and every
        // remaining key must still resolve.
        for (i, &page) in runs.iter().enumerate() {
            if i % 3 == 0 {
                h.free(&env.meta, page);
            }
        }
        h.audit();
        for (i, &page) in runs.iter().enumerate() {
            let info = h.lookup(page).unwrap();
            if i % 3 == 0 {
                assert_eq!(info.state, RunState::Empty);
            } else {
                assert_eq!(info.state, RunState::Active);
                assert_eq!(h.usable_size(page), PAGE);
            }
        }
        for (i, &page) in runs.iter().enumerate() {
            if i % 3 != 0 {
                h.free(&env.meta, page);
            }
        }
        h.audit();
        assert_eq!(h.stats().empty, 1);
        let ms = env.meta.stats();
        assert!(ms.allocated < h.stats().table_cap * 16 + 4096, "descriptors leaked: {ms:?}");
    }

    #[test]
    fn metadata_exhaustion_is_reported_not_fatal() {
        // 64 KiB of metadata: the bitmap and table fit, descriptors run out.
        let env = Env::with_meta_len(192 * 1024);
        let Some(h) = Huge::new(&env.meta, &env.regions, guard_mode()) else {
            return;
        };
        let mut held = Vec::new();
        loop {
            let (p, _) = env.alloc(&h, PAGE, 1);
            if p.is_null() {
                break;
            }
            held.push(p);
        }
        assert!(!held.is_empty());
        // Frees still work and hand descriptors back.
        for p in held {
            env.free(&h, p);
        }
        let (p, _) = env.alloc(&h, PAGE, 1);
        assert!(!p.is_null());
        // The tables of a region that does not fit at all fail cleanly.
        let tiny = Env::with_meta_len(3 * PAGE);
        assert!(Huge::new(&tiny.meta, &tiny.regions, guard_mode()).is_none());
    }

    /// Random allocations, frees and purges against an interval model.
    #[test]
    fn randomised_model_check() {
        let env = Env::new();
        let h = env.huge();
        h.set_retain_max(40 * PAGE);
        let mut rng = 0x1234_5678_9ABC_DEF0u64;
        // (page, usable pages, pattern)
        let mut live: Vec<(usize, usize, u64)> = Vec::new();
        let mut zero_flags = 0;
        for step in 0..4000 {
            let r = xorshift(&mut rng);
            match r % 10 {
                0..=4 => {
                    let size = match xorshift(&mut rng) % 4 {
                        0 => (xorshift(&mut rng) as usize % (4 * PAGE)) + 1,
                        1 => (xorshift(&mut rng) as usize % (16 * PAGE)) + 1,
                        2 => (xorshift(&mut rng) as usize % (100 * PAGE)) + 1,
                        _ => PAGE * ((xorshift(&mut rng) as usize % 8) + 1),
                    };
                    let align = if xorshift(&mut rng) % 3 == 0 { 1usize << (13 + xorshift(&mut rng) % 9) } else { 1 };
                    let (p, zero) = env.alloc(&h, size, align);
                    if p.is_null() {
                        // Only acceptable when the region is really tight.
                        assert!(h.stats().next_page + size / PAGE + 2 > HUGE_PAGES || align > PAGE, "step {step}");
                        continue;
                    }
                    assert_eq!(p as usize % align, 0);
                    let page = env.page_of(p);
                    let n = size.div_ceil(PAGE);
                    assert_eq!(h.usable_size(page), n * PAGE);
                    for &(q, m, _) in &live {
                        assert!(page + n + GUARD_PAGES <= q || q + m + GUARD_PAGES <= page, "overlap at step {step}");
                    }
                    if zero {
                        zero_flags += 1;
                    }
                    live.push((page, n, r));
                }
                5..=8 if !live.is_empty() => {
                    let i = xorshift(&mut rng) as usize % live.len();
                    let (page, n, _) = live.swap_remove(i);
                    assert_eq!(h.free(&env.meta, page), n * PAGE);
                    h.audit();
                }
                9 => {
                    let cutoff = if xorshift(&mut rng) % 2 == 0 { u64::MAX } else { os::now_ns().saturating_sub(5) };
                    h.purge_empty(cutoff);
                    h.audit();
                }
                _ => {}
            }
            let st = h.stats();
            assert_eq!(st.runs, live.len());
            assert_eq!(st.run_pages, live.iter().map(|&(_, n, _)| n + GUARD_PAGES).sum::<usize>());
        }
        assert!(zero_flags > 0);
        for (page, _, _) in live.drain(..) {
            h.free(&env.meta, page);
        }
        h.audit();
        let st = h.stats();
        assert_eq!(st.runs, 0);
        assert_eq!(st.empty + st.purged, 1);
        assert_eq!(h.purge_empty(u64::MAX), st.empty);
        assert_eq!(h.stats().purged_pages, st.next_page);
    }

    /// Eight threads allocate and free concurrently; no run is ever held
    /// by two threads and the counters settle.
    #[test]
    fn concurrent_runs_are_disjoint() {
        let env = Arc::new(Env::new());
        let h = Arc::new(env.huge());
        let threads = 8;
        let mut handles = Vec::new();
        for t in 0..threads {
            let env = Arc::clone(&env);
            let h = Arc::clone(&h);
            handles.push(std::thread::spawn(move || {
                let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
                let mut held: Vec<(usize, usize)> = Vec::new();
                let mut got = 0;
                for _ in 0..400 {
                    let size = (xorshift(&mut rng) as usize % (8 * PAGE)) + 1;
                    let (p, _) = h.alloc(&env.meta, size, 1, t as u16);
                    if !p.is_null() {
                        let page = env.page_of(p);
                        assert_eq!(h.lookup(page).unwrap().arena, t as u16);
                        held.push((page, size.div_ceil(PAGE)));
                        got += 1;
                    }
                    if xorshift(&mut rng) % 3 == 0 && !held.is_empty() {
                        let i = xorshift(&mut rng) as usize % held.len();
                        let (page, n) = held.swap_remove(i);
                        assert_eq!(h.free(&env.meta, page), n * PAGE);
                    }
                }
                for (page, _) in held {
                    h.free(&env.meta, page);
                }
                got
            }));
        }
        let mut total = 0;
        for hd in handles {
            total += hd.join().unwrap();
        }
        assert!(total > 0);
        h.audit();
        let st = h.stats();
        assert_eq!(st.runs, 0);
        assert_eq!(st.empty + st.purged, 1);
    }

    #[test]
    #[should_panic(expected = "not the start of a huge allocation")]
    fn free_of_an_interior_page_aborts() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 4 * PAGE, 1);
        h.free(&env.meta, env.page_of(p) + 1);
    }

    #[test]
    #[should_panic(expected = "not the start of a huge allocation")]
    fn free_of_a_guard_page_aborts() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 4 * PAGE, 1);
        h.free(&env.meta, env.page_of(p) + 4);
    }

    #[test]
    #[should_panic(expected = "not the start of a huge allocation")]
    fn free_beyond_the_mark_aborts() {
        let env = Env::new();
        let h = env.huge();
        let _ = env.alloc(&h, 4 * PAGE, 1);
        h.free(&env.meta, 100);
    }

    #[test]
    #[should_panic(expected = "huge allocation already freed")]
    fn double_free_aborts() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 4 * PAGE, 1);
        env.free(&h, p);
        h.free(&env.meta, env.page_of(p));
    }

    #[test]
    #[should_panic(expected = "huge allocation already freed")]
    fn free_of_a_free_extents_last_page_aborts() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 4 * PAGE, 1);
        let (_q, _) = env.alloc(&h, PAGE, 1);
        env.free(&h, p);
        h.free(&env.meta, env.page_of(p) + 4);
    }

    #[test]
    #[should_panic(expected = "not the start of a huge allocation")]
    fn usable_size_of_a_bad_pointer_aborts() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 4 * PAGE, 1);
        h.usable_size(env.page_of(p) + 2);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::tests::{Env, guard_mode};
    use super::*;
    use std::vec::Vec;

    const PAGE: usize = 4096;

    fn fill(p: *mut u8, len: usize, v: u8) {
        unsafe { ptr::write_bytes(p, v, len) };
    }

    fn all(p: *mut u8, len: usize, v: u8) -> bool {
        let s = unsafe { core::slice::from_raw_parts(p, len) };
        s.iter().all(|&b| b == v)
    }

    #[test]
    fn memory_is_writable_and_the_zero_flag_is_honest() {
        let env = Env::new();
        let h = env.huge();
        let size = 10 * PAGE;
        let (p, zero) = env.alloc(&h, size, 1);
        assert!(zero && all(p, size, 0));
        fill(p, size, 0xAB);
        env.free(&h, p);
        // Reused while Empty: dirty, and the flag says so.
        let (q, zero) = env.alloc(&h, size, 1);
        assert_eq!(q, p);
        assert!(!zero);
        assert!(all(q, size, 0xAB));
        env.free(&h, q);
        assert_eq!(h.purge_empty(u64::MAX), 1);
        let (r, zero) = env.alloc(&h, size, 1);
        assert_eq!(r, p);
        assert!(zero && all(r, size, 0));
        env.free(&h, r);
        // Above retain_max the purge is immediate.
        h.set_retain_max(size - 1);
        let (s, _) = env.alloc(&h, size, 1);
        fill(s, size, 0xCD);
        env.free(&h, s);
        assert_eq!(h.lookup(env.page_of(s)).unwrap().state, RunState::Purged);
        let (t, zero) = env.alloc(&h, size, 1);
        assert!(zero && all(t, size, 0));
        env.free(&h, t);
    }

    #[test]
    fn stale_guards_inside_a_reused_extent_are_writable_and_purgeable() {
        let env = Env::new();
        let h = env.huge();
        let mut ps = Vec::new();
        for _ in 0..4 {
            let (p, _) = env.alloc(&h, 3 * PAGE, 1);
            fill(p, 3 * PAGE, 0x11);
            ps.push(p);
        }
        for p in &ps {
            env.free(&h, *p);
        }
        assert_eq!(h.stats().empty, 1);
        // One extent of 16 pages with three old guard positions inside:
        // purging it must succeed whatever their protection is.
        assert_eq!(h.purge_empty(u64::MAX), 1);
        let (big, zero) = env.alloc(&h, 15 * PAGE, 1);
        assert_eq!(big, ps[0]);
        assert!(zero && all(big, 15 * PAGE, 0));
        fill(big, 15 * PAGE, 0x22);
        assert!(all(big, 15 * PAGE, 0x22));
        env.free(&h, big);
        // And an Empty extent with stale guards can be re-split.
        let (a, zero) = env.alloc(&h, 5 * PAGE, 1);
        assert!(!zero);
        fill(a, 5 * PAGE, 0x33);
        let (b, _) = env.alloc(&h, 2 * PAGE, 1);
        fill(b, 2 * PAGE, 0x44);
        assert!(all(a, 5 * PAGE, 0x33));
        env.free(&h, a);
        env.free(&h, b);
    }

    /// The guard page after a run faults, in both guard modes.
    #[test]
    fn guard_page_faults() {
        let env = Env::new();
        let h = env.huge();
        let (p, _) = env.alloc(&h, 2 * PAGE, 1);
        let (q, _) = env.alloc(&h, PAGE, 1);
        // The guard is the page right before `q`.
        assert_eq!(q as usize, p as usize + 3 * PAGE);
        let guard = p as usize + 2 * PAGE;
        if guard_mode() {
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            let merged = maps.lines().any(|l| {
                let range = l.split_whitespace().next().unwrap();
                let (a, b) = range.split_once('-').unwrap();
                let a = usize::from_str_radix(a, 16).unwrap();
                let b = usize::from_str_radix(b, 16).unwrap();
                a <= p as usize && q as usize + PAGE <= b
            });
            assert!(merged, "guard page split the VMA in guard_install mode");
        }
        for (what, addr) in [("guard", guard), ("first page past the mark", q as usize + 2 * PAGE)] {
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                unsafe {
                    core::ptr::write_volatile(addr as *mut u8, 1);
                    libc::_exit(0);
                }
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(libc::WIFSIGNALED(status), "{what}: child did not fault: status {status:#x}");
            assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV, "{what}");
        }
        // Freeing removes the guard: the page is reusable as run memory.
        env.free(&h, p);
        env.free(&h, q);
        let (r, _) = env.alloc(&h, 4 * PAGE, 1);
        assert_eq!(r, p);
        fill(r, 4 * PAGE, 0x55);
        assert!(all(r, 4 * PAGE, 0x55));
        env.free(&h, r);
    }

    #[test]
    fn aligned_runs_are_usable_end_to_end() {
        let env = Env::new();
        let h = env.huge();
        let mut held = Vec::new();
        for lg in 13..=21 {
            let align = 1usize << lg;
            let size = 3 * PAGE + 100;
            let (p, zero) = env.alloc(&h, size, align);
            assert!(!p.is_null() && p as usize % align == 0);
            assert!(zero && all(p, 4 * PAGE, 0));
            fill(p, 4 * PAGE, lg as u8);
            held.push((p, lg as u8));
        }
        for &(p, v) in &held {
            assert!(all(p, 4 * PAGE, v));
        }
        for (p, _) in held {
            env.free(&h, p);
        }
        assert_eq!(h.stats().runs, 0);
    }

    #[test]
    fn concurrent_threads_see_their_own_patterns() {
        use std::sync::Arc;
        let env = Arc::new(Env::new());
        let h = Arc::new(env.huge());
        let mut handles = Vec::new();
        for t in 0..8u8 {
            let env = Arc::clone(&env);
            let h = Arc::clone(&h);
            handles.push(std::thread::spawn(move || {
                let mut held: Vec<(*mut u8, usize)> = Vec::new();
                let mut rng = 0xA5A5_5A5A_1234_5678u64 ^ (t as u64 + 1);
                for i in 0..300 {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let size = (rng as usize % (6 * PAGE)) + 1;
                    let (p, zero) = h.alloc(&env.meta, size, 1, 0);
                    if p.is_null() {
                        continue;
                    }
                    if zero {
                        assert!(all(p, size, 0));
                    }
                    fill(p, size, t.wrapping_add(i as u8));
                    held.push((p, size));
                    if held.len() > 12 {
                        let (q, n) = held.remove((rng >> 32) as usize % held.len());
                        let v = unsafe { *q };
                        assert!(all(q, n, v), "thread {t} saw another thread's writes");
                        h.free(&env.meta, env.page_of(q));
                    }
                }
                for (q, n) in held {
                    let v = unsafe { *q };
                    assert!(all(q, n, v));
                    h.free(&env.meta, env.page_of(q));
                }
            }));
        }
        for hd in handles {
            hd.join().unwrap();
        }
        assert_eq!(h.stats().runs, 0);
    }
}
