//! Slab descriptors (design.md section 4).
//!
//! Every slab of every class has one [`SlabDesc`] in the metadata region.
//! Descriptors for class `c` form a dense array indexed by slab number with
//! a per-class stride ([`desc_stride`]): a fixed 16-byte header followed by
//! the slab's allocation bitmap, rounded up to a cache-line multiple so
//! neighbouring descriptors never share a line.
//!
//! The header is `repr(C)` so that the offsets are fixed and the bitmap can
//! be addressed by pointer arithmetic. Descriptors live in memory that is
//! never moved or freed, so accessors hand out references with the caller's
//! lifetime and no `Drop` is involved.
//!
//! A set bitmap bit means "not available to a refill": the slot is live in
//! the program or sitting in some thread's cache. `live` counts set bits
//! plus in-flight claim reservations (see `class.rs`); it is what drives the
//! `Active -> Empty` transition. `dirty_at` is the coarse monotonic time the
//! slab became `Empty`, or 0 in any other state.

use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::bitmap::AtomicBitmap;
use crate::layout::{CLASSES, Class, NCLASSES};

/// Size of the fixed descriptor header in bytes; the bitmap starts here.
pub const HEADER: usize = 16;
/// Descriptor arrays and strides are cache-line aligned.
pub const DESC_ALIGN: usize = 64;

/// Lifecycle of a committed slab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Committed, zero from the kernel, no slot ever claimed.
    Fresh = 0,
    /// At least one slot claimed since the slab was last `Fresh`, `Empty`
    /// or `Purged`.
    Active = 1,
    /// `live` reached zero; the pages are dirty and still resident.
    Empty = 2,
    /// `MADV_DONTNEED` applied; the pages read zero and are not resident.
    Purged = 3,
}

impl State {
    #[inline]
    const fn from_u8(v: u8) -> State {
        match v {
            0 => State::Fresh,
            1 => State::Active,
            2 => State::Empty,
            _ => State::Purged,
        }
    }

    /// Memory of a slab in this state is known to read as zero.
    #[inline]
    pub const fn is_zero(self) -> bool {
        matches!(self, State::Fresh | State::Purged)
    }
}

/// Descriptor header. The bitmap words follow immediately at [`HEADER`].
#[repr(C, align(16))]
pub struct SlabDesc {
    state: AtomicU8,
    _pad: u8,
    arena: AtomicU16,
    live: AtomicU32,
    dirty_at: AtomicU64,
}

const _: () = assert!(core::mem::size_of::<SlabDesc>() == HEADER);

#[inline]
const fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

const fn build_strides() -> [u32; NCLASSES] {
    let mut out = [0u32; NCLASSES];
    let mut c = 0;
    while c < NCLASSES {
        out[c] = align_up(HEADER + 8 * CLASSES[c].bitmap_words(), DESC_ALIGN) as u32;
        c += 1;
    }
    out
}

/// Per-class descriptor stride in bytes: header plus bitmap, rounded up to
/// a cache-line multiple.
pub static DESC_STRIDE: [u32; NCLASSES] = build_strides();

/// Descriptor stride for `class` in bytes.
#[inline]
pub fn desc_stride(class: usize) -> usize {
    DESC_STRIDE[class] as usize
}

impl SlabDesc {
    /// Descriptor of slab `slab` in the array for `class` starting at
    /// `base`.
    ///
    /// # Safety
    /// `base` must be the start of a descriptor array for `class` with at
    /// least `slab + 1` committed entries.
    #[inline]
    pub unsafe fn at<'a>(base: usize, class: usize, slab: usize) -> &'a SlabDesc {
        debug_assert!(base % DESC_ALIGN == 0);
        // SAFETY: the caller guarantees the entry is inside a committed array;
        // descriptors are plain atomics valid for any bit pattern.
        unsafe { &*((base + slab * desc_stride(class)) as *const SlabDesc) }
    }

    /// The slab's allocation bitmap, `class.bitmap_words()` words long.
    #[inline]
    pub fn bitmap(&self, class: &Class) -> &AtomicBitmap {
        let words = class.bitmap_words();
        // SAFETY: the descriptor stride reserves `words` `AtomicU64`s right
        // after the header, 8-aligned because the header is 16 bytes.
        let slice = unsafe {
            core::slice::from_raw_parts(
                (self as *const SlabDesc as usize + HEADER) as *const AtomicU64,
                words,
            )
        };
        AtomicBitmap::from_words(slice)
    }

    /// Initialise the descriptor of a freshly committed slab: `Fresh`, no
    /// live slots, bitmap clear apart from the padding bits above `nslots`.
    pub fn init(&self, class: &Class, arena: u16) {
        self.state.store(State::Fresh as u8, Ordering::Relaxed);
        self.arena.store(arena, Ordering::Relaxed);
        self.live.store(0, Ordering::Relaxed);
        self.dirty_at.store(0, Ordering::Relaxed);
        self.bitmap(class).init(class.nslots());
    }

    #[inline]
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// Only ever called with the class lock held.
    #[inline]
    pub fn set_state(&self, s: State) {
        self.state.store(s as u8, Ordering::Relaxed);
    }

    #[inline]
    pub fn arena(&self) -> u16 {
        self.arena.load(Ordering::Relaxed)
    }

    /// Acquire: a releaser clears bits and then subtracts, so a reader that
    /// sees the smaller count also sees the cleared bits. `class.rs` relies
    /// on that to treat `nslots - live` as a lower bound on claimable slots.
    #[inline]
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Acquire) as usize
    }

    /// Add `n` to `live`, returning the previous value.
    #[inline]
    pub fn add_live(&self, n: usize) -> usize {
        self.live.fetch_add(n as u32, Ordering::AcqRel) as usize
    }

    /// Subtract `n` from `live`, returning the previous value. Aborts if
    /// that would go negative: more slots released than were ever claimed.
    #[inline]
    pub fn sub_live(&self, n: usize) -> usize {
        let prev = self.live.fetch_sub(n as u32, Ordering::AcqRel) as usize;
        assert!(prev >= n, "slab: live count underflow ({prev} - {n})");
        prev
    }

    #[inline]
    pub fn dirty_at(&self) -> u64 {
        self.dirty_at.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_dirty_at(&self, ns: u64) {
        self.dirty_at.store(ns, Ordering::Relaxed);
    }

    /// Free slots according to `live` (design 13.4 `nfree`). Transiently
    /// under-reports while a claim reservation is in flight.
    #[inline]
    pub fn nfree(&self, class: &Class) -> usize {
        class.nslots().saturating_sub(self.live())
    }

    /// Whether `slot` is currently marked allocated (or cached).
    #[inline]
    pub fn is_allocated(&self, class: &Class, slot: usize) -> bool {
        debug_assert!(slot < class.nslots());
        self.bitmap(class).is_set(slot)
    }

    /// The `free` check of design section 4: one load, no write. Aborts
    /// with the pointer and its location if the slot is not allocated, which
    /// is an invalid free or a stale double free.
    #[inline]
    pub fn check(&self, class: &Class, slot: usize, p: usize, class_idx: usize, slab: usize) {
        if !self.is_allocated(class, slot) {
            invalid_free(p, class_idx, slab, slot);
        }
    }
}

#[cold]
#[inline(never)]
fn invalid_free(p: usize, class: usize, slab: usize, slot: usize) -> ! {
    panic!(
        "invalid or double free of {p:#x} (class {class} slab {slab} slot {slot}): slot is not allocated"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A descriptor array for one class backed by a `Vec`, aligned to a
    /// cache line.
    struct Descs {
        buf: Vec<u8>,
        base: usize,
        class: usize,
    }

    impl Descs {
        fn new(class: usize, n: usize) -> Descs {
            let buf = vec![0u8; n * desc_stride(class) + DESC_ALIGN];
            let base = align_up(buf.as_ptr() as usize, DESC_ALIGN);
            Descs { buf, base, class }
        }

        fn at(&self, slab: usize) -> &SlabDesc {
            assert!((slab + 1) * desc_stride(self.class) <= self.buf.len() - DESC_ALIGN);
            unsafe { SlabDesc::at(self.base, self.class, slab) }
        }
    }

    #[test]
    fn header_layout_is_fixed() {
        assert_eq!(core::mem::size_of::<SlabDesc>(), 16);
        assert_eq!(core::mem::align_of::<SlabDesc>(), 16);
        assert_eq!(core::mem::offset_of!(SlabDesc, state), 0);
        assert_eq!(core::mem::offset_of!(SlabDesc, arena), 2);
        assert_eq!(core::mem::offset_of!(SlabDesc, live), 4);
        assert_eq!(core::mem::offset_of!(SlabDesc, dirty_at), 8);
    }

    #[test]
    fn strides_cover_the_bitmap_and_are_cache_line_multiples() {
        for (c, cls) in CLASSES.iter().enumerate() {
            let s = desc_stride(c);
            assert_eq!(s % DESC_ALIGN, 0, "class {c}");
            assert!(s >= HEADER + 8 * cls.bitmap_words(), "class {c}");
            assert!(s < HEADER + 8 * cls.bitmap_words() + DESC_ALIGN, "class {c}");
        }
        // The smallest class has the most slots and the widest descriptor.
        assert_eq!(desc_stride(0), align_up(HEADER + 8 * CLASSES[0].bitmap_words(), 64));
        // Classes with at most 256 slots fit in one line.
        for (c, cls) in CLASSES.iter().enumerate() {
            if cls.nslots() <= (DESC_ALIGN - HEADER) * 8 {
                assert_eq!(desc_stride(c), DESC_ALIGN, "class {c}");
            }
        }
    }

    #[test]
    fn state_round_trips() {
        for s in [State::Fresh, State::Active, State::Empty, State::Purged] {
            assert_eq!(State::from_u8(s as u8), s);
        }
        assert!(State::Fresh.is_zero());
        assert!(State::Purged.is_zero());
        assert!(!State::Active.is_zero());
        assert!(!State::Empty.is_zero());
    }

    #[test]
    fn init_sets_header_and_padding_bits() {
        for (c, cls) in CLASSES.iter().enumerate() {
            let d = Descs::new(c, 3);
            for slab in 0..3 {
                let desc = d.at(slab);
                desc.init(cls, 7);
                assert_eq!(desc.state(), State::Fresh);
                assert_eq!(desc.arena(), 7);
                assert_eq!(desc.live(), 0);
                assert_eq!(desc.dirty_at(), 0);
                assert_eq!(desc.nfree(cls), cls.nslots());
                let bm = desc.bitmap(cls);
                assert_eq!(bm.words(), cls.bitmap_words());
                for slot in 0..cls.nslots() {
                    assert!(!desc.is_allocated(cls, slot));
                }
                let pad = cls.bitmap_words() * 64 - cls.nslots();
                let set: u32 = (0..bm.words()).map(|w| bm.load(w).count_ones()).sum();
                assert_eq!(set as usize, pad, "class {c}: padding bits");
                assert!(!bm.is_full());
            }
        }
    }

    #[test]
    fn neighbouring_descriptors_do_not_overlap() {
        let c = 0;
        let cls = &CLASSES[c];
        let d = Descs::new(c, 4);
        for slab in 0..4 {
            d.at(slab).init(cls, slab as u16);
        }
        // Claim everything in slab 1 and check slabs 0 and 2 are untouched.
        let bm = d.at(1).bitmap(cls);
        let mut got = 0;
        while let Some((_, bits)) = bm.claim(64) {
            got += bits.count_ones() as usize;
        }
        assert_eq!(got, cls.nslots());
        assert!(bm.is_full());
        for other in [0usize, 2, 3] {
            let desc = d.at(other);
            assert_eq!(desc.arena(), other as u16);
            assert_eq!(desc.state(), State::Fresh);
            assert!(!desc.bitmap(cls).is_full());
            for slot in 0..cls.nslots() {
                assert!(!desc.is_allocated(cls, slot));
            }
        }
    }

    #[test]
    fn live_counter_and_state() {
        let cls = &CLASSES[3];
        let d = Descs::new(3, 1);
        let desc = d.at(0);
        desc.init(cls, 0);
        assert_eq!(desc.add_live(5), 0);
        assert_eq!(desc.add_live(2), 5);
        assert_eq!(desc.live(), 7);
        assert_eq!(desc.nfree(cls), cls.nslots() - 7);
        assert_eq!(desc.sub_live(7), 7);
        assert_eq!(desc.live(), 0);
        desc.set_state(State::Empty);
        desc.set_dirty_at(42);
        assert_eq!(desc.state(), State::Empty);
        assert_eq!(desc.dirty_at(), 42);
    }

    #[test]
    #[should_panic(expected = "live count underflow")]
    fn live_underflow_aborts() {
        let cls = &CLASSES[3];
        let d = Descs::new(3, 1);
        let desc = d.at(0);
        desc.init(cls, 0);
        desc.add_live(1);
        desc.sub_live(2);
    }

    #[test]
    fn check_passes_for_allocated_slots() {
        let cls = &CLASSES[5];
        let d = Descs::new(5, 1);
        let desc = d.at(0);
        desc.init(cls, 0);
        let (w, bits) = desc.bitmap(cls).claim(3).unwrap();
        assert_eq!((w, bits), (0, 0b111));
        for slot in 0..3 {
            desc.check(cls, slot, 0x1000 + slot * cls.size(), 5, 0);
        }
    }

    #[test]
    #[should_panic(expected = "invalid or double free of 0xdead0 (class 5 slab 0 slot 3)")]
    fn check_aborts_for_free_slots() {
        let cls = &CLASSES[5];
        let d = Descs::new(5, 1);
        let desc = d.at(0);
        desc.init(cls, 0);
        desc.bitmap(cls).claim(3).unwrap();
        desc.check(cls, 3, 0xdead0, 5, 0);
    }
}
