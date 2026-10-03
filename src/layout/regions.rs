//! Address-space parameters (design section 2 table), [`Regions`] (the
//! reserved regions of one process and pointer classification over them)
//! and [`Regions::reserve`], the one piece that talks to the kernel.

use super::{CLASSES, LG_MAX_SLAB, MAX_SLAB, NCLASSES};

/// Address-space parameters (design section 2 table), as `log2`.
pub const LG_REGION_PREFERRED: u32 = 38;
pub const LG_REGION_MIN: u32 = 32;
pub const LG_HUGE_PREFERRED: u32 = 40;
pub const LG_HUGE_MIN: u32 = 34;
pub const LG_META_PREFERRED: u32 = 36;
pub const LG_META_MIN: u32 = 30;
/// Window within which the huge and metadata bases are slid at random, on
/// top of the kernel's own mmap randomisation.
pub const LG_SLIDE: u32 = 30;

/// One `mmap` reservation as handed back by the kernel, before alignment and
/// sliding. Kept so the never-committed slack around each region can be
/// sealed ([`Regions::never_committed`]) and the whole range accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub addr: usize,
    pub len: usize,
}

/// Upper bound on the ranges [`Regions::never_committed`] returns, and so
/// on the `mseal` calls init makes: one before class 0, one after each
/// class, two per side mapping.
pub const MAX_SEALED: usize = NCLASSES + 1 + 4;

/// What a pointer is, according to the regions alone. Slab-tier results
/// still need the slab bitmap consulted to tell live from free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Exact slot start inside a class region.
    Slab { class: u32, slab: u32, slot: u32 },
    /// Page-aligned address inside the huge region; `page` is the index from
    /// the region base. Whether it starts a run is the huge allocator's call.
    Huge { page: usize },
    /// Outside every region, in a gap, or not on a slot/page boundary.
    Invalid,
}

/// The reserved regions of one process. `repr(C)` keeps `superblock`,
/// `lg_region` and `lg_stride` at the front, on the cache line the `free`
/// fast path reads (see `Shared` in `heap/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Regions {
    /// `MAX_SLAB`-aligned start of the class superblock.
    pub superblock: usize,
    pub lg_region: u32,
    pub lg_stride: u32,
    /// `1 << lg_region`, stored on x86_64 so that the sized free's region
    /// bound (`Shared::sized_slot`, through `sized_bound`) is one compare with a memory operand:
    /// against the shift LLVM emits a load of the shift count, a copy, a
    /// shift and a test. On the line `superblock` sits on. Not on aarch64,
    /// whose `lsr` by a loaded count and `cbnz` are no longer than a load
    /// and a compare, and whose layout is kept as it was (#90 and #93:
    /// moving it measured as a placement effect there).
    #[cfg(target_arch = "x86_64")]
    region_len: usize,
    /// Start of each class region, `MAX_SLAB`-aligned, inside its stride.
    pub class_base: [usize; NCLASSES],
    pub huge_base: usize,
    pub huge_len: usize,
    pub meta_base: usize,
    pub meta_len: usize,
    pub lg_page: u32,
    pub superblock_map: Mapping,
    pub huge_map: Mapping,
    pub meta_map: Mapping,
}

impl Regions {
    /// Assemble regions from already chosen bases. `offsets[c]` is the
    /// distance of class `c`'s region from the start of its stride; every
    /// offset must be `MAX_SLAB`-aligned and below the gap, and `superblock`
    /// must be `MAX_SLAB`-aligned.
    pub const fn from_parts(
        superblock: usize,
        lg_region: u32,
        lg_stride: u32,
        offsets: &[usize; NCLASSES],
        huge: (usize, usize),
        meta: (usize, usize),
        lg_page: u32,
        maps: [Mapping; 3],
    ) -> Regions {
        assert!(lg_stride > lg_region && lg_region >= LG_MAX_SLAB);
        assert!(superblock & (MAX_SLAB - 1) == 0);
        let gap = (1usize << lg_stride) - (1usize << lg_region);
        let mut class_base = [0usize; NCLASSES];
        let mut c = 0;
        while c < NCLASSES {
            assert!(offsets[c] & (MAX_SLAB - 1) == 0 && offsets[c] < gap);
            class_base[c] = superblock + (c << lg_stride) + offsets[c];
            c += 1;
        }
        Regions {
            superblock,
            lg_region,
            lg_stride,
            #[cfg(target_arch = "x86_64")]
            region_len: 1 << lg_region,
            class_base,
            huge_base: huge.0,
            huge_len: huge.1,
            meta_base: meta.0,
            meta_len: meta.1,
            lg_page,
            superblock_map: maps[0],
            huge_map: maps[1],
            meta_map: maps[2],
        }
    }

    #[inline]
    pub const fn region_len(&self) -> usize {
        1 << self.lg_region
    }

    /// [`region_len`](Regions::region_len) for the sized free's region
    /// bound (`Shared::sized_slot`): the stored copy on x86_64, the shift
    /// elsewhere. The other users keep the shift, so only the sized fast
    /// path's code changes.
    #[inline(always)]
    pub const fn sized_bound(&self) -> usize {
        #[cfg(target_arch = "x86_64")]
        return self.region_len;
        #[cfg(not(target_arch = "x86_64"))]
        return 1 << self.lg_region;
    }

    #[cfg(test)]
    pub const fn stride(&self) -> usize {
        1 << self.lg_stride
    }

    #[cfg(test)]
    pub const fn gap(&self) -> usize {
        self.stride() - self.region_len()
    }

    #[inline]
    pub const fn page_size(&self) -> usize {
        1 << self.lg_page
    }

    /// Slabs that fit in a class region.
    #[inline]
    pub fn nslabs(&self, class: usize) -> usize {
        self.region_len() >> CLASSES[class].lg_slab
    }

    /// Pages in the huge region.
    #[inline]
    pub const fn huge_pages(&self) -> usize {
        self.huge_len >> self.lg_page
    }

    #[inline]
    pub fn slab_addr(&self, class: usize, slab: usize) -> usize {
        self.class_base[class] + (slab << CLASSES[class].lg_slab)
    }

    #[inline]
    #[cfg(test)]
    pub fn slot_addr(&self, class: usize, slab: usize, slot: usize) -> usize {
        self.slab_addr(class, slab) + slot * CLASSES[class].size as usize
    }

    #[inline]
    pub const fn huge_addr(&self, page: usize) -> usize {
        self.huge_base + (page << self.lg_page)
    }

    /// The parts of the three mappings that no code ever commits, purges
    /// or reprotects, in address order per mapping: the slack before class
    /// 0, the gap between each class region and the next (one contiguous
    /// range: the tail of stride `c` plus the head of stride `c + 1`), the
    /// slack after the last class, and the alignment-plus-slide slack on
    /// both sides of the huge and metadata regions. Empty ranges are
    /// omitted. These are what `mseal` seals at init (design 10 step 5,
    /// section 14): the class regions, the huge region and the metadata
    /// region themselves are committed by `mprotect` and purged by
    /// `madvise` for the life of the process, so they cannot be sealed.
    ///
    /// Returns the ranges and how many are filled in. At most
    /// [`MAX_SEALED`]: `NCLASSES + 1` around the classes and two per side
    /// mapping.
    pub fn never_committed(&self) -> ([Mapping; MAX_SEALED], usize) {
        let mut out = [Mapping { addr: 0, len: 0 }; MAX_SEALED];
        let mut n = 0;
        let mut push = |lo: usize, hi: usize| {
            if hi > lo {
                out[n] = Mapping {
                    addr: lo,
                    len: hi - lo,
                };
                n += 1;
            }
        };
        let mut start = self.superblock_map.addr;
        for c in 0..NCLASSES {
            push(start, self.class_base[c]);
            start = self.class_base[c] + self.region_len();
        }
        push(start, self.superblock_map.addr + self.superblock_map.len);
        for (m, base, len) in [
            (self.huge_map, self.huge_base, self.huge_len),
            (self.meta_map, self.meta_base, self.meta_len),
        ] {
            push(m.addr, base);
            push(base + len, m.addr + m.len);
        }
        (out, n)
    }

    /// Slab-slot lookup of design section 2, the whole of the `free` fast
    /// path's classification: `Some((class, slab, slot))` when `p` is
    /// exactly a slot start inside a class region, `None` for anything else
    /// (a gap, a mid-slot address, the huge or metadata region, or outside
    /// every reservation). Two subtractions and a shift find the class; a
    /// multiply, a rotate and a compare validate the slot
    /// ([`super::Class::slot_of_offset`]). Never aborts: the caller's
    /// slow path decides what `None` means.
    #[inline(always)]
    pub fn slab_slot(&self, p: usize) -> Option<(usize, usize, usize)> {
        let c = p.wrapping_sub(self.superblock) >> self.lg_stride;
        if c >= NCLASSES {
            return None;
        }
        // SAFETY: `c < NCLASSES` was just checked; both arrays have
        // `NCLASSES` entries.
        let cls = unsafe { class_entry(c) };
        let rel = p.wrapping_sub(unsafe { *self.class_base.get_unchecked(c) });
        if rel >= self.region_len() {
            return None;
        }
        let slab = rel >> cls.lg_slab;
        let offset = rel & cls.slab_mask();
        let (slot, valid) = cls.slot_of_offset(offset);
        if !valid {
            return None;
        }
        Some((c, slab, slot))
    }

    /// The sized free's classification (design 6.3): the size hint names
    /// `class`, so the region is known and the
    /// pointer is only checked against it, `Some((slab, slot))` when `p` is
    /// exactly a slot start inside class `class`'s region, `None` otherwise
    /// (outside the region, which covers every other class's region, the
    /// gaps, the huge and metadata regions; or a mid-slot address). One
    /// subtraction and a compare replace [`slab_slot`]'s superblock
    /// subtraction, stride shift, bound check and class compare. `class`
    /// must be below `NCLASSES` (what `class_of`/`class_for` return).
    ///
    /// The fast path (`heap::Shared::sized_slot`) makes the same test from
    /// its class line; this form is the reference the tests hold it to.
    ///
    /// [`slab_slot`]: Regions::slab_slot
    #[cfg(test)]
    pub fn slot_in_class(&self, class: usize, p: usize) -> Option<(usize, usize)> {
        debug_assert!(class < NCLASSES);
        // SAFETY: the caller's `class` is an index `class_of` produced.
        let cls = unsafe { class_entry(class) };
        let rel = p.wrapping_sub(unsafe { *self.class_base.get_unchecked(class) });
        if rel >= self.region_len() {
            return None;
        }
        let slab = rel >> cls.lg_slab;
        let (slot, valid) = cls.slot_of_offset(rel & cls.slab_mask());
        if !valid {
            return None;
        }
        Some((slab, slot))
    }

    /// Whether `p` falls in the class superblock (a class region or a gap).
    #[inline]
    fn in_superblock(&self, p: usize) -> bool {
        p.wrapping_sub(self.superblock) >> self.lg_stride < NCLASSES
    }

    /// Classify a pointer (design section 2): [`slab_slot`] for the class
    /// regions, then the huge region.
    ///
    /// [`slab_slot`]: Regions::slab_slot
    #[inline]
    pub fn classify(&self, p: usize) -> Kind {
        if let Some((class, slab, slot)) = self.slab_slot(p) {
            return Kind::Slab {
                class: class as u32,
                slab: slab as u32,
                slot: slot as u32,
            };
        }
        if self.in_superblock(p) {
            return Kind::Invalid;
        }
        let rel = p.wrapping_sub(self.huge_base);
        if rel < self.huge_len && rel & (self.page_size() - 1) == 0 {
            return Kind::Huge {
                page: rel >> self.lg_page,
            };
        }
        Kind::Invalid
    }

    /// True when `p` is anywhere inside the metadata reservation.
    #[cfg(test)]
    pub fn in_meta(&self, p: usize) -> bool {
        p.wrapping_sub(self.meta_base) < self.meta_len
    }
}

mod linux {
    use super::{
        LG_HUGE_MIN, LG_HUGE_PREFERRED, LG_MAX_SLAB, LG_META_MIN, LG_META_PREFERRED, LG_REGION_MIN,
        LG_REGION_PREFERRED, LG_SLIDE, MAX_SLAB, Mapping, NCLASSES, Regions,
    };
    use crate::sys::{self, Errno, MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};

    /// Reserve address space with `PROT_NONE`, `MAP_NORESERVE`.
    fn try_reserve(len: usize) -> Result<usize, Errno> {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        unsafe {
            sys::mmap(
                0,
                len,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                -1,
                0,
            )
        }
    }

    /// Reserve `2^lg + extra` bytes, halving `lg` down to `lg_min` on any
    /// failure. Returns the mapping and the `lg` that fit.
    fn reserve_shrinking(what: &str, mut lg: u32, lg_min: u32, extra: usize) -> (Mapping, u32) {
        loop {
            let len = (1usize << lg) + extra;
            match try_reserve(len) {
                Ok(addr) => return (Mapping { addr, len }, lg),
                Err(e) if lg > lg_min => {
                    let _ = e;
                    lg -= 1;
                }
                Err(e) => sys::abort_fmt(format_args!(
                    "cannot reserve {what} region of 2^{lg} bytes: {e}"
                )),
            }
        }
    }

    /// Reserve the class superblock: `NCLASSES` strides plus alignment slack.
    /// Prefers a stride of 4x the region (gap 3x). Shrinks the region down to
    /// the minimum, then as a last resort drops the stride to 2x the region
    /// (gap 1x, below the design's 1.25x preference but still a full gap).
    fn reserve_superblock() -> (Mapping, u32, u32) {
        let mut lg_region = LG_REGION_PREFERRED;
        let mut lg_stride = lg_region + 2;
        loop {
            let len = (NCLASSES << lg_stride) + MAX_SLAB;
            match try_reserve(len) {
                Ok(addr) => return (Mapping { addr, len }, lg_region, lg_stride),
                Err(_) if lg_region > LG_REGION_MIN => {
                    lg_region -= 1;
                    lg_stride = lg_region + 2;
                }
                Err(_) if lg_stride == lg_region + 2 => lg_stride = lg_region + 1,
                Err(e) => sys::abort_fmt(format_args!(
                    "cannot reserve class superblock of {NCLASSES} x 2^{lg_stride} bytes: {e}"
                )),
            }
        }
    }

    const SLIDE_STEPS: usize = (1usize << LG_SLIDE) >> LG_MAX_SLAB;

    fn name_mapping(m: Mapping, name: &'static [u8]) {
        // SAFETY: `m` is a live mapping we own; the name is NUL-terminated.
        let _ = unsafe { sys::set_vma_anon_name(m.addr, m.len, name) };
    }

    impl Regions {
        /// Probe and reserve every region (design section 10 step 2). Never
        /// allocates. Aborts if even the minimum layout cannot be reserved.
        /// `anon_name` should be the probed `PR_SET_VMA_ANON_NAME` support.
        pub fn reserve(page_size: usize, anon_name: bool) -> Regions {
            assert!(page_size.is_power_of_two() && page_size <= MAX_SLAB);
            let lg_page = page_size.trailing_zeros();

            // One 64-bit random per class offset, one each for the two
            // slides, one for the reservation order.
            let mut rnd = [0u8; 8 * (NCLASSES + 3)];
            if let Err(e) = sys::getrandom_exact(&mut rnd) {
                sys::abort_fmt(format_args!("getrandom failed: {e}"));
            }
            let word = |i: usize| -> u64 {
                let mut b = [0u8; 8];
                b.copy_from_slice(&rnd[8 * i..8 * i + 8]);
                u64::from_ne_bytes(b)
            };

            // Reserve the three mappings in a random order so their relative
            // placement is not fixed across processes.
            let order: [u8; 3] = match word(NCLASSES + 2) % 6 {
                0 => [0, 1, 2],
                1 => [0, 2, 1],
                2 => [1, 0, 2],
                3 => [1, 2, 0],
                4 => [2, 0, 1],
                _ => [2, 1, 0],
            };
            let slide = 1usize << LG_SLIDE;
            let mut sb = None;
            let mut huge = None;
            let mut meta = None;
            for role in order {
                match role {
                    0 => sb = Some(reserve_superblock()),
                    1 => {
                        huge = Some(reserve_shrinking(
                            "huge",
                            LG_HUGE_PREFERRED,
                            LG_HUGE_MIN,
                            slide + MAX_SLAB,
                        ))
                    }
                    _ => {
                        meta = Some(reserve_shrinking(
                            "metadata",
                            LG_META_PREFERRED,
                            LG_META_MIN,
                            slide + MAX_SLAB,
                        ))
                    }
                }
            }
            let (sb_map, lg_region, lg_stride) = sb.unwrap();
            let (huge_map, lg_huge) = huge.unwrap();
            let (meta_map, lg_meta) = meta.unwrap();

            let superblock = (sb_map.addr + MAX_SLAB - 1) & !(MAX_SLAB - 1);
            let gap = (1usize << lg_stride) - (1usize << lg_region);
            let mut offsets = [0usize; NCLASSES];
            for (c, off) in offsets.iter_mut().enumerate() {
                *off = (word(c) as usize % (gap >> LG_MAX_SLAB)) << LG_MAX_SLAB;
            }
            // `mmap` only promises page alignment, so each mapping carries
            // `MAX_SLAB` of slack and its base is aligned up before the slide.
            let align_base = |m: Mapping| (m.addr + MAX_SLAB - 1) & !(MAX_SLAB - 1);
            let huge_base =
                align_base(huge_map) + ((word(NCLASSES) as usize % SLIDE_STEPS) << LG_MAX_SLAB);
            let meta_base =
                align_base(meta_map) + ((word(NCLASSES + 1) as usize % SLIDE_STEPS) << LG_MAX_SLAB);

            if anon_name {
                name_mapping(sb_map, b"challoc:classes\0");
                name_mapping(huge_map, b"challoc:huge\0");
                name_mapping(meta_map, b"challoc:meta\0");
            }
            // `thp:never` on a host whose global mode is `always` (`thp.rs`):
            // the class and metadata reservations are `MADV_NOHUGEPAGE`d
            // before anything in them is committed; the huge region follows
            // the host's policy (`thp::Reservation`), so `thp.rs` skips it
            // by kind.
            use crate::thp::Reservation;
            for (kind, m) in [
                (Reservation::Classes, sb_map),
                (Reservation::Huge, huge_map),
                (Reservation::Metadata, meta_map),
            ] {
                crate::thp::advise_reservation(kind, m.addr, m.len);
            }

            Regions::from_parts(
                superblock,
                lg_region,
                lg_stride,
                &offsets,
                (huge_base, 1usize << lg_huge),
                (meta_base, 1usize << lg_meta),
                lg_page,
                [sb_map, huge_map, meta_map],
            )
        }

        /// Seal every range [`Regions::never_committed`] lists (design 10
        /// step 5): `mseal` forbids `munmap`, `mremap`, `mprotect` and
        /// `MAP_FIXED` over them for the rest of the process, so nothing can
        /// land a mapping in the gap between two class regions or unmap
        /// part of a reservation. Only for kernels whose probe said
        /// `mseal=yes`; each call splits the enclosing VMA, so the VMA count
        /// grows by at most `2 * MAX_SEALED`. Returns the number of ranges
        /// sealed. A refusal after a successful probe (`ENOMEM` at the VMA
        /// limit, for instance) stops the sweep and leaves the rest
        /// unsealed rather than aborting: sealing is a hardening extra, not
        /// a heap invariant, and the count is reported by
        /// `malloc_stats_print`.
        pub fn seal(&self) -> u32 {
            let (ranges, n) = self.never_committed();
            let mut sealed = 0;
            for m in &ranges[..n] {
                // SAFETY: the range is inside a reservation this process
                // owns and, by construction, is never committed, purged,
                // reprotected or unmapped (`release` is test-only and is
                // never called on sealed regions).
                if unsafe { sys::mseal(m.addr, m.len, 0) }.is_err() {
                    break;
                }
                sealed += 1;
            }
            sealed
        }

        /// Give the reservations back. The allocator never does this; it
        /// exists so tests can reserve repeatedly in one process. Must not
        /// be called after [`seal`](Regions::seal): the kernel refuses and
        /// this aborts.
        ///
        /// # Safety
        /// Nothing may reference the regions afterwards.
        #[cfg(test)]
        pub unsafe fn release(&self) {
            for m in [self.superblock_map, self.huge_map, self.meta_map] {
                // SAFETY: forwarded from the caller.
                if let Err(e) = unsafe { sys::munmap(m.addr, m.len) } {
                    sys::abort_fmt(format_args!("munmap of reservation failed: {e}"));
                }
            }
        }
    }
}

/// `&CLASSES[c]` for the fast paths' classification, `c < NCLASSES`.
///
/// On x86_64 the entry's address goes through an empty `asm!` so that it
/// is one register: left alone, LLVM keeps the table base and the scaled
/// index in two registers for the slot test's loads (`(idx, base, 1)`
/// addressing; the table is RIP-relative, so it cannot be a displacement)
/// and adds them again for `lg_slab`, and the extra register costs `free`
/// and `sdallocx` a callee-saved push and pop (2 instructions each per
/// call, measured). aarch64 folds the address into one shifted add on its
/// own, and the barrier there costs `free` an instruction.
///
/// # Safety
/// `c < NCLASSES`.
#[inline(always)]
unsafe fn class_entry(c: usize) -> &'static super::Class {
    // SAFETY: `c < NCLASSES` (the caller's contract).
    let p = unsafe { CLASSES.as_ptr().add(c) };
    #[cfg(target_arch = "x86_64")]
    let p = {
        let mut p = p;
        // SAFETY: an empty instruction sequence; `p` comes out unchanged.
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags))
        };
        p
    };
    // SAFETY: `p` points into the table, which is never written after init.
    unsafe { &*p }
}
