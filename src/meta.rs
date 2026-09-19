//! Metadata region allocator (design.md sections 2, 6.2 and 17).
//!
//! Everything the allocator has to remember lives in the metadata region
//! reserved by `layout::Regions`: slab descriptor arrays, the huge page
//! bitmap and descriptor table, thread heaps and their class caches, the
//! background thread's stack. None of it may come from `malloc`, so this is
//! the one module that carves that region up.
//!
//! Two kinds of request come from the two ends of the region:
//!
//! - **Reservations** ([`Meta::reserve`]) are large, fixed-address, never
//!   freed arrays such as a class's descriptor table. They are carved
//!   downward from the top of the region and handed back *uncommitted*; the
//!   owner calls [`Meta::commit`] on the pages it starts to use, so a class
//!   with three live slabs commits one page of descriptors, not the whole
//!   table.
//! - **Objects** ([`Meta::alloc`] / [`Meta::free`]) are small: `Heap`,
//!   cache slot stacks, bitmap words. They come from a bump pointer growing
//!   upward from the base, with the region committed ahead of the bump in
//!   [`COMMIT_CHUNK`] steps, and freed objects go on one of [`NBINS`]
//!   size-classed free lists for reuse.
//!
//! Committing is `mprotect(PROT_READ | PROT_WRITE)` on the `PROT_NONE`
//! reservation. The kernel supplies zero pages on first touch, so committed
//! but untouched metadata costs address space and overcommit charge, never
//! RSS. Nothing is ever uncommitted or returned to the kernel.
//!
//! All state sits behind one [`Lock`], the "metadata lock" of design
//! section 9. None of this is on the `malloc`/`free` fast path.

use core::alloc::Layout;
use core::ptr;

use crate::lock::{Lock, RawLock};
use crate::sys::Errno;

/// Alignment of every object; the smallest bin is one quantum of 16.
pub const MIN_ALIGN: usize = 16;
/// Objects of 128 bytes or more are line aligned and line sized so heaps and
/// caches of different threads never share a line. 128, not 64: Apple
/// silicon has 128-byte lines and Intel's adjacent-line prefetcher pairs
/// 64-byte lines (the same unit as `lock::PAD_ALIGN`).
pub const CACHE_LINE: usize = 128;
const _: () = assert!(CACHE_LINE == crate::lock::PAD_ALIGN);
/// Largest alignment [`Meta::alloc`] accepts. Anything needing more (a
/// thread stack) is a reservation.
pub const MAX_ALIGN: usize = CACHE_LINE;
/// Largest object [`Meta::alloc`] serves. Bigger things are reservations.
pub const LG_MAX_BINNED: u32 = 20;
pub const MAX_BINNED: usize = 1 << LG_MAX_BINNED;
/// Bins: 16, 32, 48, 64, 128, 256, then `1.5 * 2^(k-1)` and `2^k` for `k`
/// in `9..=LG_MAX_BINNED`. Every bin from 128 up is a multiple of the cache
/// line (which is why there is no 96 or 192), and consecutive bins differ by
/// at most 2x.
pub const NBINS: usize = 6 + 2 * (LG_MAX_BINNED as usize - 8);
/// Granularity of bump-area commits. Only address space and overcommit
/// charge, not RSS, so a coarse step keeps `mprotect` calls rare.
pub const COMMIT_CHUNK: usize = 64 * 1024;

/// Bin sizes, ascending.
pub static BINS: [u32; NBINS] = build_bins();

const fn build_bins() -> [u32; NBINS] {
    let mut b = [0u32; NBINS];
    b[0] = 16;
    b[1] = 32;
    b[2] = 48;
    b[3] = 64;
    b[4] = 128;
    b[5] = 256;
    let mut i = 6;
    let mut lg = 9;
    while lg <= LG_MAX_BINNED {
        b[i] = 3 << (lg - 2);
        b[i + 1] = 1 << lg;
        i += 2;
        lg += 1;
    }
    assert!(i == NBINS);
    b
}

/// Smallest bin whose size is at least `size`; `None` above `MAX_BINNED`.
#[inline]
pub const fn bin_of(size: usize) -> Option<usize> {
    if size <= 48 {
        return Some(if size == 0 { 0 } else { (size - 1) / 16 });
    }
    if size > MAX_BINNED {
        return None;
    }
    if size <= 64 {
        return Some(3);
    }
    if size <= 128 {
        return Some(4);
    }
    if size <= 256 {
        return Some(5);
    }
    // `size` is in `(2^lg, 2^(lg+1)]` with `lg >= 8`; bin `6 + 2 * (lg - 8)`
    // holds `1.5 * 2^lg` and the next holds `2^(lg+1)`.
    let lg = (usize::BITS - 1 - (size - 1).leading_zeros()) as usize;
    let base = 6 + 2 * (lg - 8);
    if size <= (3 << (lg - 1)) {
        Some(base)
    } else {
        Some(base + 1)
    }
}

/// Alignment every object of bin `bin` has: 16 up to the 64-byte bin, a
/// full line from 128 up.
#[inline]
pub const fn bin_align(bin: usize) -> usize {
    if bin < 4 { MIN_ALIGN } else { CACHE_LINE }
}

/// Bin serving a `Layout`: the size's bin, promoted to a cache-line bin when
/// the alignment asks for more than 16. `None` if the size is too big.
#[inline]
pub const fn bin_for(layout: Layout) -> Option<usize> {
    match bin_of(layout.size()) {
        Some(b) if layout.align() > MIN_ALIGN && b < 4 => Some(4),
        other => other,
    }
}

/// Counters for `stats.metadata` and diagnostics. Identities:
/// `allocated <= bumped <= committed_bump <= top - base`,
/// `committed = committed_bump + committed_reserved`, and
/// `bumped + reserved <= len`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaStats {
    /// Length of the whole region.
    pub len: usize,
    /// Bytes readable and writable: bump chunks plus explicit commits.
    pub committed: usize,
    /// Bytes in live objects, each rounded to its bin.
    pub allocated: usize,
    /// Bytes the bump pointer has passed, including padding and freed objects.
    pub bumped: usize,
    /// Bytes handed out by `reserve`, committed or not.
    pub reserved: usize,
}

struct FreeNode {
    next: *mut FreeNode,
}

struct Inner {
    base: usize,
    len: usize,
    page: usize,
    /// Next byte for objects. `base <= bump <= committed <= top`.
    bump: usize,
    /// Bump area is writable below here; always page aligned.
    committed: usize,
    /// Reservations occupy `[top, base + len)`; always page aligned.
    top: usize,
    free: [*mut FreeNode; NBINS],
    stats: MetaStats,
}

// SAFETY: the raw pointers are addresses inside the region, meaningful in
// any thread; the `Lock` serialises every access.
unsafe impl Send for Inner {}

/// The metadata allocator for one region. One per process, in a `static`,
/// unusable until [`Meta::init`].
pub struct Meta {
    inner: Lock<Inner>,
}

#[inline]
const fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

#[cfg(target_os = "linux")]
fn commit_range(addr: usize, len: usize) -> Result<(), Errno> {
    use crate::sys::{PROT_READ, PROT_WRITE};
    // SAFETY: the range is inside our own PROT_NONE reservation and nothing
    // is mapped over it; adding permissions cannot invalidate anything.
    unsafe { crate::sys::mprotect(addr, len, PROT_READ | PROT_WRITE) }
}

/// Host unit tests back the region with ordinary writable memory.
#[cfg(not(target_os = "linux"))]
fn commit_range(_addr: usize, _len: usize) -> Result<(), Errno> {
    Ok(())
}

impl Inner {
    const fn empty() -> Inner {
        Inner {
            base: 0,
            len: 0,
            page: 0,
            bump: 0,
            committed: 0,
            top: 0,
            free: [ptr::null_mut(); NBINS],
            stats: MetaStats {
                len: 0,
                committed: 0,
                allocated: 0,
                bumped: 0,
                reserved: 0,
            },
        }
    }

    const fn new(base: usize, len: usize, page: usize) -> Inner {
        assert!(
            page.is_power_of_two() && page >= MIN_ALIGN,
            "meta: bad page size"
        );
        assert!(
            base % page == 0 && len % page == 0 && len > 0,
            "meta: region not page aligned"
        );
        assert!(base.checked_add(len).is_some(), "meta: region wraps");
        Inner {
            base,
            len,
            page,
            bump: base,
            committed: base,
            top: base + len,
            free: [ptr::null_mut(); NBINS],
            stats: MetaStats {
                len,
                committed: 0,
                allocated: 0,
                bumped: 0,
                reserved: 0,
            },
        }
    }

    /// Make the bump area writable through `end`, in `COMMIT_CHUNK` steps,
    /// never past `top`.
    fn commit_bump_to(&mut self, end: usize) -> Result<(), Errno> {
        debug_assert!(end <= self.top);
        // Chunks are measured from the base: the region is only page
        // aligned, so rounding the absolute address would make the first
        // chunk short and every later one misaligned against the counter.
        let step = align_up(COMMIT_CHUNK, self.page);
        let want = (self.base + align_up(end - self.base, step)).min(self.top);
        if want > self.committed {
            commit_range(self.committed, want - self.committed)?;
            self.stats.committed += want - self.committed;
            self.committed = want;
        }
        Ok(())
    }

    fn alloc(&mut self, bin: usize) -> *mut u8 {
        if self.len == 0 {
            return ptr::null_mut();
        }
        let size = BINS[bin] as usize;
        let head = self.free[bin];
        if !head.is_null() {
            // SAFETY: `head` came from `free`, so it is a live block of at
            // least `size` bytes inside the bump area holding a `FreeNode`.
            self.free[bin] = unsafe { (*head).next };
            self.stats.allocated += size;
            return head as *mut u8;
        }
        let start = align_up(self.bump, bin_align(bin));
        let Some(end) = start.checked_add(size) else {
            return ptr::null_mut();
        };
        if end > self.top {
            return ptr::null_mut();
        }
        if end > self.committed && self.commit_bump_to(end).is_err() {
            return ptr::null_mut();
        }
        self.bump = end;
        self.stats.bumped = end - self.base;
        self.stats.allocated += size;
        start as *mut u8
    }

    fn free(&mut self, p: *mut u8, bin: usize) {
        let addr = p as usize;
        let size = BINS[bin] as usize;
        assert!(
            addr >= self.base && addr + size <= self.bump && addr % bin_align(bin) == 0,
            "meta: free of a pointer that was never allocated here"
        );
        let node = p as *mut FreeNode;
        // The one double free that costs nothing to see: the block is the
        // list's head. A block deeper in the list is not looked for (that
        // would walk the list on every free).
        assert!(
            self.free[bin] != node,
            "meta: double free of the free list head"
        );
        // SAFETY: the block is `size >= 16` bytes of committed memory that
        // the caller has given up, aligned to at least 16.
        unsafe { (*node).next = self.free[bin] };
        self.free[bin] = node;
        self.stats.allocated -= size;
    }

    fn reserve(&mut self, len: usize, align: usize) -> Option<usize> {
        assert!(
            align.is_power_of_two(),
            "meta: reserve alignment not a power of two"
        );
        if self.len == 0 || len > self.top - self.committed {
            return None;
        }
        let align = align.max(self.page);
        let len = align_up(len.max(1), self.page);
        let start = self.top.checked_sub(len)? & !(align - 1);
        if start < self.committed {
            return None;
        }
        self.stats.reserved += self.top - start;
        self.top = start;
        Some(start)
    }

    fn commit(&mut self, addr: usize, len: usize) -> Result<(), Errno> {
        assert!(
            addr % self.page == 0 && len % self.page == 0 && len > 0,
            "meta: commit range not page aligned"
        );
        assert!(
            addr >= self.top && addr + len <= self.base + self.len,
            "meta: commit outside the reserved area"
        );
        commit_range(addr, len)?;
        self.stats.committed += len;
        Ok(())
    }
}

impl Default for Meta {
    fn default() -> Self {
        Self::empty()
    }
}

impl Meta {
    /// An allocator with no region. Every request fails until [`Meta::init`].
    pub const fn empty() -> Meta {
        Meta {
            inner: Lock::new(Inner::empty()),
        }
    }

    /// An allocator over `[base, base + len)`, both page multiples.
    pub const fn new(base: usize, len: usize, page: usize) -> Meta {
        Meta {
            inner: Lock::new(Inner::new(base, len, page)),
        }
    }

    /// Point an [`empty`](Meta::empty) allocator at its region. Called once
    /// from init, before any other thread can reach the allocator.
    ///
    /// # Safety
    /// `[base, base + len)` must be a `PROT_NONE` reservation owned by the
    /// caller that nothing else will ever map, protect or touch.
    pub unsafe fn init(&self, base: usize, len: usize, page: usize) {
        let mut g = self.inner.lock();
        assert!(g.len == 0, "meta: initialised twice");
        *g = Inner::new(base, len, page);
    }

    /// The lock, for the fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> &RawLock {
        self.inner.raw()
    }

    /// Allocate an object. Contents are unspecified: fresh memory is zero,
    /// reused memory holds whatever its last owner left. Null when the region
    /// is exhausted or the kernel refuses to commit.
    ///
    /// Aborts for a layout this allocator does not serve (`size >
    /// MAX_BINNED` or `align > MAX_ALIGN`): that is a bug, not a resource
    /// failure.
    pub fn alloc(&self, layout: Layout) -> *mut u8 {
        assert!(
            layout.align() <= MAX_ALIGN,
            "meta: alignment too large for an object"
        );
        let Some(bin) = bin_for(layout) else {
            panic!("meta: object too large")
        };
        self.inner.lock().alloc(bin)
    }

    /// [`alloc`](Meta::alloc), then zero the object if it was reused.
    pub fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        assert!(
            layout.align() <= MAX_ALIGN,
            "meta: alignment too large for an object"
        );
        let Some(bin) = bin_for(layout) else {
            panic!("meta: object too large")
        };
        let mut g = self.inner.lock();
        let before = g.bump;
        let p = g.alloc(bin);
        if !p.is_null() && (p as usize) < before {
            // Reused: below the old bump pointer. Fresh memory is above it
            // and has never been written.
            // SAFETY: `p` is a live block of `BINS[bin]` bytes we just took.
            unsafe { ptr::write_bytes(p, 0, BINS[bin] as usize) };
        }
        p
    }

    /// Return an object allocated with the same `layout`.
    pub fn free(&self, p: *mut u8, layout: Layout) {
        assert!(!p.is_null(), "meta: free of null");
        let Some(bin) = bin_for(layout) else {
            panic!("meta: free with an impossible layout")
        };
        self.inner.lock().free(p, bin);
    }

    /// Carve `len` bytes (rounded up to pages) aligned to `align` (at least a
    /// page) from the top of the region. Uncommitted: the owner calls
    /// [`commit`](Meta::commit) on the pages it uses. Never freed. `None`
    /// when it would collide with the object area.
    pub fn reserve(&self, len: usize, align: usize) -> Option<usize> {
        self.inner.lock().reserve(len, align)
    }

    /// Make `[addr, addr + len)`, page aligned and inside a reservation,
    /// readable and writable. Each page should be committed once; the
    /// committed-bytes counter trusts the caller on that.
    pub fn commit(&self, addr: usize, len: usize) -> Result<(), Errno> {
        self.inner.lock().commit(addr, len)
    }

    /// [`reserve`](Meta::reserve) and commit the whole range. Zero on
    /// return. `None` on either failure, leaving nothing reserved.
    pub fn reserve_committed(&self, len: usize, align: usize) -> Option<usize> {
        let mut g = self.inner.lock();
        let old_top = g.top;
        let old_reserved = g.stats.reserved;
        let start = g.reserve(len, align)?;
        if g.commit(start, old_top - start).is_err() {
            g.top = old_top;
            g.stats.reserved = old_reserved;
            return None;
        }
        Some(start)
    }

    /// Whether `p` lies anywhere in the region.
    #[inline]
    pub fn contains(&self, p: usize) -> bool {
        let g = self.inner.lock();
        p.wrapping_sub(g.base) < g.len
    }

    /// Whether the region has been set.
    pub fn is_initialised(&self) -> bool {
        self.inner.lock().len != 0
    }

    pub fn stats(&self) -> MetaStats {
        self.inner.lock().stats
    }

    /// Start of the region.
    pub fn base(&self) -> usize {
        self.inner.lock().base
    }

    /// Lowest reserved address; the object area may grow up to here.
    pub fn top(&self) -> usize {
        self.inner.lock().top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// The page size the environment is built with: the kernel's on Linux
    /// (the reservations are real; the 64 KiB-page matrix cell found the
    /// 4 KiB constant), 4 KiB in the synthetic host environment.
    #[cfg(target_os = "linux")]
    fn page_size() -> usize {
        crate::sys::features::page_size()
    }

    #[cfg(not(target_os = "linux"))]
    fn page_size() -> usize {
        4096
    }

    /// A region for tests: a real `PROT_NONE` reservation on Linux so that
    /// commit goes through `mprotect`; plain zeroed memory elsewhere.
    struct Backing {
        base: usize,
        len: usize,
        #[cfg(not(target_os = "linux"))]
        _buf: Vec<u8>,
    }

    impl Backing {
        #[cfg(target_os = "linux")]
        fn new(len: usize) -> Backing {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let base = unsafe {
                crate::sys::mmap(
                    0,
                    len,
                    PROT_NONE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                    -1,
                    0,
                )
            }
            .expect("reserve");
            assert_eq!(base % page_size(), 0);
            Backing { base, len }
        }

        #[cfg(not(target_os = "linux"))]
        fn new(len: usize) -> Backing {
            let buf = vec![0u8; len + page_size()];
            let base = align_up(buf.as_ptr() as usize, page_size());
            Backing {
                base,
                len,
                _buf: buf,
            }
        }

        fn meta(&self) -> Meta {
            Meta::new(self.base, self.len, page_size())
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for Backing {
        fn drop(&mut self) {
            unsafe { crate::sys::munmap(self.base, self.len) }.unwrap();
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

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn bins_are_well_formed() {
        assert_eq!(NBINS, 30);
        assert_eq!(BINS[0], 16);
        assert_eq!(BINS[NBINS - 1] as usize, MAX_BINNED);
        for w in BINS.windows(2) {
            assert!(w[0] < w[1]);
        }
        for (i, &b) in BINS.iter().enumerate() {
            let b = b as usize;
            assert_eq!(
                b % bin_align(i),
                0,
                "bin {i} ({b}) not a multiple of its alignment"
            );
            // Power of two or 1.5x a power of two; from 128 up, line sized.
            assert!(b.is_power_of_two() || (b % 3 == 0 && (b / 3).is_power_of_two()));
            if i >= 4 {
                assert_eq!(b % CACHE_LINE, 0);
            } else {
                assert_eq!(bin_align(i), MIN_ALIGN);
            }
        }
        // Internal waste is bounded: consecutive bins grow by at most 2x.
        for w in BINS.windows(2) {
            assert!(w[1] <= 2 * w[0]);
        }
    }

    #[test]
    fn bin_of_matches_linear_scan() {
        for size in 0..=MAX_BINNED + 1 {
            let expect = BINS.iter().position(|&b| b as usize >= size);
            assert_eq!(bin_of(size), expect, "size {size}");
        }
        assert_eq!(bin_of(usize::MAX), None);
        assert_eq!(bin_of(MAX_BINNED), Some(NBINS - 1));
        for size in [
            1usize, 16, 17, 32, 33, 48, 49, 64, 65, 128, 129, 256, 257, 384, 385, 512,
        ] {
            let b = bin_of(size).unwrap();
            assert!(BINS[b] as usize >= size);
            assert!(b == 0 || (BINS[b - 1] as usize) < size);
        }
    }

    #[test]
    fn bin_for_promotes_on_alignment() {
        assert_eq!(bin_for(layout(8, 8)), Some(0));
        assert_eq!(bin_for(layout(8, 16)), Some(0));
        assert_eq!(bin_for(layout(8, 32)), Some(4));
        assert_eq!(bin_for(layout(40, 64)), Some(4));
        assert_eq!(bin_for(layout(64, 128)), Some(4));
        assert_eq!(bin_for(layout(100, 64)), Some(4));
        assert_eq!(bin_for(layout(129, 8)), Some(5));
        assert_eq!(BINS[5], 256);
        assert_eq!(bin_for(layout(257, 8)), Some(6));
        assert_eq!(BINS[6], 384);
        assert_eq!(bin_for(layout(MAX_BINNED + 1, 8)), None);
    }

    #[test]
    fn fresh_region_stats() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        assert!(m.is_initialised());
        assert_eq!(m.base(), b.base);
        assert_eq!(m.top(), b.base + b.len);
        assert_eq!(
            m.stats(),
            MetaStats {
                len: 1 << 20,
                ..MetaStats::default()
            }
        );
        assert!(m.contains(b.base));
        assert!(m.contains(b.base + b.len - 1));
        assert!(!m.contains(b.base + b.len));
        assert!(!m.contains(b.base.wrapping_sub(1)));
    }

    #[test]
    fn empty_then_init() {
        let b = Backing::new(1 << 16);
        let m = Meta::empty();
        assert!(!m.is_initialised());
        assert!(m.alloc(layout(64, 8)).is_null());
        assert_eq!(m.reserve(page_size(), page_size()), None);
        unsafe { m.init(b.base, b.len, page_size()) };
        assert!(m.is_initialised());
        let p = m.alloc(layout(64, 8));
        assert!(!p.is_null());
        assert!(m.contains(p as usize));
    }

    #[test]
    fn first_objects_come_from_zero_committed_memory() {
        let b = Backing::new(4 << 20);
        let m = b.meta();
        let l = layout(200, 8);
        let p = m.alloc(l);
        assert!(!p.is_null());
        assert_eq!(p as usize, b.base, "first object sits at the base");
        assert_eq!(p as usize % CACHE_LINE, 0);
        let s = m.stats();
        assert_eq!(s.committed, COMMIT_CHUNK, "one chunk committed");
        assert_eq!(s.allocated, 256, "200 rounds up to the 256 bin");
        assert_eq!(s.bumped, 256);
        // Writable and zero.
        let slice = unsafe { core::slice::from_raw_parts_mut(p, 200) };
        assert!(slice.iter().all(|&x| x == 0));
        slice.fill(0xA5);
        // Small objects pack at 16 and do not disturb the chunk.
        let q = m.alloc(layout(1, 1));
        let r = m.alloc(layout(16, 16));
        assert_eq!(q as usize, b.base + 256);
        assert_eq!(r as usize, b.base + 272);
        assert_eq!(m.stats().committed, COMMIT_CHUNK);
        // Crossing the chunk boundary commits the next one.
        let big = m.alloc(layout(MAX_BINNED, 64));
        assert!(!big.is_null());
        assert_eq!(
            m.stats().committed,
            align_up(MAX_BINNED + 512, COMMIT_CHUNK)
        );
        unsafe { core::slice::from_raw_parts_mut(big, MAX_BINNED) }.fill(1);
    }

    #[test]
    fn free_list_is_lifo_and_per_bin() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        let l = layout(100, 8);
        let a = m.alloc(l);
        let c = m.alloc(l);
        let d = m.alloc(layout(1000, 8));
        let bumped = m.stats().bumped;
        m.free(a, l);
        m.free(c, l);
        assert_eq!(m.stats().allocated, 1024);
        // Same bin: last freed comes back first, nothing new is bumped.
        assert_eq!(m.alloc(l), c);
        assert_eq!(m.alloc(l), a);
        assert_eq!(m.stats().bumped, bumped);
        // Different bin: not served from that list.
        m.free(a, l);
        let e = m.alloc(layout(200, 8));
        assert_ne!(e, a);
        assert!(m.stats().bumped > bumped);
        m.free(d, layout(1000, 8));
        assert_eq!(m.alloc(layout(1000, 8)), d);
        // alloc_zeroed scrubs a reused block but leaves fresh ones alone.
        unsafe { core::slice::from_raw_parts_mut(d, 1024) }.fill(0xFF);
        m.free(d, layout(1000, 8));
        let z = m.alloc_zeroed(layout(1000, 8));
        assert_eq!(z, d);
        assert!(
            unsafe { core::slice::from_raw_parts(z, 1024) }
                .iter()
                .all(|&x| x == 0)
        );
        let fresh = m.alloc_zeroed(layout(5000, 8));
        assert!(
            unsafe { core::slice::from_raw_parts(fresh, 5000) }
                .iter()
                .all(|&x| x == 0)
        );
    }

    #[test]
    fn alignment_is_honoured() {
        let b = Backing::new(1 << 20);
        let m = b.meta();
        for align in [1usize, 2, 4, 8, 16, 32, 64, 128] {
            for size in [1usize, 15, 16, 17, 48, 49, 64, 65, 96, 300, 4097] {
                let l = layout(size, align);
                let p = m.alloc(l);
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0, "size {size} align {align}");
                assert_eq!(p as usize % bin_align(bin_for(l).unwrap()), 0);
                if size > 64 || align > 16 {
                    assert_eq!(p as usize % CACHE_LINE, 0);
                }
            }
        }
    }

    #[test]
    fn exhaustion_returns_null_and_recovers() {
        let b = Backing::new(16 * page_size());
        let m = b.meta();
        let l = layout(1024, 8);
        let mut got = Vec::new();
        loop {
            let p = m.alloc(l);
            if p.is_null() {
                break;
            }
            got.push(p);
        }
        assert_eq!(
            got.len(),
            16 * page_size() / 1024,
            "16 pages hold exactly that many 1 KiB objects"
        );
        assert_eq!(m.stats().committed, 16 * page_size());
        assert_eq!(m.stats().bumped, 16 * page_size());
        assert!(m.alloc(layout(1, 1)).is_null());
        assert_eq!(
            m.reserve(page_size(), page_size()),
            None,
            "no room left for a reservation"
        );
        // Freeing one makes exactly one more available.
        m.free(got.pop().unwrap(), l);
        assert!(!m.alloc(l).is_null());
        assert!(m.alloc(l).is_null());
        // Distinct pointers throughout.
        got.sort_unstable();
        got.dedup();
        assert_eq!(got.len(), 16 * page_size() / 1024 - 1);
    }

    #[test]
    fn reservations_come_from_the_top() {
        let b = Backing::new(64 * page_size());
        let m = b.meta();
        let end = b.base + b.len;
        let r1 = m.reserve(3 * page_size(), page_size()).unwrap();
        assert_eq!(r1, end - 3 * page_size());
        assert_eq!(m.top(), r1);
        // Sub-page lengths round up; large alignment rounds the start down.
        let r2 = m.reserve(10, 8 * page_size()).unwrap();
        assert_eq!(r2 % (8 * page_size()), 0);
        assert!(r2 + page_size() <= r1);
        assert_eq!(m.top(), r2);
        let s = m.stats();
        assert_eq!(s.reserved, end - r2);
        assert_eq!(s.committed, 0, "reservations are uncommitted");
        // Commit part of r1, write into it.
        m.commit(r1 + page_size(), page_size()).unwrap();
        assert_eq!(m.stats().committed, page_size());
        unsafe { core::slice::from_raw_parts_mut((r1 + page_size()) as *mut u8, page_size()) }
            .fill(7);
        // reserve_committed is zero and writable.
        let r3 = m.reserve_committed(2 * page_size(), page_size()).unwrap();
        assert!(r3 + 2 * page_size() <= r2);
        let sl = unsafe { core::slice::from_raw_parts_mut(r3 as *mut u8, 2 * page_size()) };
        assert!(sl.iter().all(|&x| x == 0));
        sl.fill(9);
        assert_eq!(m.stats().committed, 3 * page_size());
        // Objects and reservations never overlap: fill the rest with
        // objects and check the boundary.
        let mut n = 0;
        while !m.alloc(layout(page_size(), 8)).is_null() {
            n += 1;
        }
        assert_eq!(m.stats().bumped, m.top() - b.base);
        assert_eq!(n, (m.top() - b.base) / page_size());
        assert_eq!(m.reserve(page_size(), page_size()), None);
        // Object area is committed exactly up to the reservation boundary.
        assert_eq!(m.stats().committed, 3 * page_size() + (m.top() - b.base));
    }

    #[test]
    fn reserve_too_large_fails_cleanly() {
        let b = Backing::new(8 * page_size());
        let m = b.meta();
        assert_eq!(m.reserve(9 * page_size(), page_size()), None);
        assert_eq!(m.reserve(usize::MAX, page_size()), None);
        assert_eq!(m.reserve(page_size(), 1 << 40), None);
        assert_eq!(m.stats().reserved, 0);
        assert_eq!(m.top(), b.base + b.len);
        assert_eq!(m.reserve(8 * page_size(), page_size()), Some(b.base));
        assert_eq!(m.alloc(layout(1, 1)), ptr::null_mut());
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_of_foreign_pointer_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(64, 8));
        let mut foreign = [0u8; 128];
        m.free(
            align_up(foreign.as_mut_ptr() as usize, 64) as *mut u8,
            layout(64, 8),
        );
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_of_misaligned_pointer_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        m.free(unsafe { p.add(8) }, layout(64, 8));
    }

    #[test]
    #[should_panic(expected = "never allocated here")]
    fn free_beyond_bump_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        m.free(unsafe { p.add(64) }, layout(64, 8));
    }

    #[test]
    #[should_panic(expected = "object too large")]
    fn oversized_object_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(MAX_BINNED + 1, 8));
    }

    #[test]
    #[should_panic(expected = "alignment too large")]
    fn overaligned_object_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let _ = m.alloc(layout(64, 256));
    }

    #[test]
    #[should_panic(expected = "commit outside")]
    fn commit_outside_reservations_aborts() {
        // Several pages, so the one-page reservation leaves room below it.
        let b = Backing::new(4 * page_size());
        let m = b.meta();
        let _ = m.reserve(page_size(), page_size());
        let _ = m.commit(b.base, page_size());
    }

    /// Random alloc/free against a model: no overlap, alignment, contents
    /// survive, stats match, and freed space is reused rather than bumped.
    #[test]
    fn matches_model() {
        let b = Backing::new(32 << 20);
        let m = b.meta();
        let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
        let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
        let mut expect_allocated = 0usize;
        for _ in 0..20_000 {
            let r = xorshift(&mut s);
            if live.is_empty() || (live.len() < 400 && r % 2 == 0) {
                let size = match xorshift(&mut s) % 10 {
                    0 => (xorshift(&mut s) as usize) % (64 * 1024) + 1,
                    1..=3 => (xorshift(&mut s) as usize) % 4096 + 1,
                    _ => (xorshift(&mut s) as usize) % 256,
                };
                let align = 1usize << (xorshift(&mut s) % 7);
                let l = layout(size, align);
                let p = m.alloc(l);
                assert!(!p.is_null());
                let bin = BINS[bin_for(l).unwrap()] as usize;
                assert_eq!(p as usize % align, 0);
                assert!(m.contains(p as usize) && m.contains(p as usize + bin - 1));
                for (q, ql, _) in &live {
                    let qb = BINS[bin_for(*ql).unwrap()] as usize;
                    let (a0, a1) = (p as usize, p as usize + bin);
                    let (b0, b1) = (*q as usize, *q as usize + qb);
                    assert!(a1 <= b0 || b1 <= a0, "overlap");
                }
                let fill = (xorshift(&mut s) as u8) | 1;
                unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
                live.push((p, l, fill));
                expect_allocated += bin;
            } else {
                let i = (xorshift(&mut s) as usize) % live.len();
                let (p, l, fill) = live.swap_remove(i);
                let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
                assert!(slice.iter().all(|&x| x == fill), "contents clobbered");
                m.free(p, l);
                expect_allocated -= BINS[bin_for(l).unwrap()] as usize;
            }
            let st = m.stats();
            assert_eq!(st.allocated, expect_allocated);
            assert!(st.allocated <= st.bumped && st.bumped <= st.committed);
            assert!(st.committed <= st.len && st.committed % page_size() == 0);
        }
        // Free everything, then the same sequence of layouts reuses memory.
        let layouts: Vec<Layout> = live.iter().map(|(_, l, _)| *l).collect();
        for (p, l, _) in live.drain(..) {
            m.free(p, l);
        }
        assert_eq!(m.stats().allocated, 0);
        let bumped = m.stats().bumped;
        for l in &layouts {
            assert!(!m.alloc(*l).is_null());
        }
        assert_eq!(
            m.stats().bumped,
            bumped,
            "everything came from the free lists"
        );
    }

    /// Eight threads share one allocator; each checks its own objects' fill
    /// patterns so any handout of the same block twice shows up.
    #[test]
    fn threads_get_disjoint_objects() {
        let b = Backing::new(16 << 20);
        let m = b.meta();
        std::thread::scope(|sc| {
            for t in 0..8u64 {
                let m = &m;
                sc.spawn(move || {
                    let mut s = 0x1234_5678 + t * 0x9E37_79B9;
                    let mut live: Vec<(*mut u8, Layout)> = Vec::new();
                    let fill = t as u8 + 1;
                    for _ in 0..5_000 {
                        if live.len() < 64 && xorshift(&mut s) % 3 != 0 {
                            let size = (xorshift(&mut s) as usize) % 2048 + 1;
                            let l = layout(size, 1 << (xorshift(&mut s) % 7));
                            let p = m.alloc(l);
                            assert!(!p.is_null());
                            unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
                            live.push((p, l));
                        } else if let Some(i) = live.len().checked_sub(1) {
                            let (p, l) = live.swap_remove((xorshift(&mut s) as usize) % (i + 1));
                            let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                            assert!(sl.iter().all(|&x| x == fill), "thread {t} block clobbered");
                            m.free(p, l);
                        }
                    }
                    for (p, l) in live {
                        let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                        assert!(sl.iter().all(|&x| x == fill));
                        m.free(p, l);
                    }
                });
            }
        });
        assert_eq!(m.stats().allocated, 0);
        assert!(!m.raw_lock().is_locked());
    }

    /// On Linux the committed part of the region must show up as exactly one
    /// `rw-p` mapping per committed range, and nothing else in the region
    /// may be writable.
    #[cfg(target_os = "linux")]
    #[test]
    fn maps_show_commits() {
        let b = Backing::new(64 << 20);
        let m = b.meta();
        for _ in 0..10 {
            assert!(!m.alloc(layout(1000, 8)).is_null());
        }
        let r = m.reserve_committed(3 * page_size(), page_size()).unwrap();
        let st = m.stats();
        assert_eq!(st.committed, COMMIT_CHUNK + 3 * page_size());

        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let mut rw: Vec<(usize, usize)> = Vec::new();
        for line in maps.lines() {
            let mut it = line.split_whitespace();
            let range = it.next().unwrap();
            let perms = it.next().unwrap();
            let (lo, hi) = range.split_once('-').unwrap();
            let lo = usize::from_str_radix(lo, 16).unwrap();
            let hi = usize::from_str_radix(hi, 16).unwrap();
            if hi <= b.base || lo >= b.base + b.len {
                continue;
            }
            if perms.starts_with("rw") {
                // Clip to the region: a committed range at either end merges
                // with an adjacent `rw-p` anonymous mapping of another test.
                rw.push((lo.max(b.base), hi.min(b.base + b.len)));
            } else {
                assert!(
                    perms.starts_with("---"),
                    "unexpected perms {perms} at {range}"
                );
            }
        }
        rw.sort_unstable();
        assert_eq!(
            rw,
            vec![(b.base, b.base + COMMIT_CHUNK), (r, r + 3 * page_size())],
            "writable ranges in the region"
        );
    }

    #[test]
    #[should_panic(expected = "double free of the free list head")]
    fn a_double_free_of_the_list_head_aborts() {
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        assert!(!p.is_null());
        m.free(p, layout(64, 8));
        m.free(p, layout(64, 8));
    }

    #[test]
    fn a_free_below_the_list_head_is_accepted() {
        // Documents the limit of the check: only the head is compared.
        let b = Backing::new(1 << 16);
        let m = b.meta();
        let p = m.alloc(layout(64, 8));
        let q = m.alloc(layout(64, 8));
        m.free(p, layout(64, 8));
        m.free(q, layout(64, 8));
        assert_eq!(m.alloc(layout(64, 8)), q);
        assert_eq!(m.alloc(layout(64, 8)), p);
    }
}
