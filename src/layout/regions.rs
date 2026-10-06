//! Address-space parameters (design section 2 table), [`Regions`] (the
//! reserved regions of one process and pointer classification over them)
//! and [`Regions::reserve_owning`], the one piece that talks to the kernel.

#[cfg(verus_only)]
use super::class_table;
use super::{CLASSES, LG_MAX_SLAB, MAX_SLAB, NCLASSES};
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;

verus! {

broadcast use {vstd::slice::group_slice_axioms, vstd::array::group_array_axioms};

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

/// `[a, a + alen)` and `[b, b + blen)` do not overlap.
pub open spec fn disjoint(a: int, alen: int, b: int, blen: int) -> bool {
    a + alen <= b || b + blen <= a
}

/// The first byte of the process's `Shared` (`api`'s `SHARED` static,
/// [`Shared::storage`](crate::heap::Shared::storage)): the lock values of
/// `Meta`, the huge allocator and the class tables, arena 0 and the
/// statistics. It lives in the writable data of the object challoc is
/// linked into, which the loader maps before any allocator code runs.
pub open spec fn shared_static_addr() -> int {
    crate::heap::Shared::storage()@.addr as int
}

/// The bytes of the process's `Shared` ([`shared_static_addr`]). Closed, so
/// that the layout of `Shared` stays out of the proofs that mention
/// `Regions::separated` but not the static
/// ([`lemma_shared_static_len`] unfolds it).
pub closed spec fn shared_static_len() -> int {
    vstd::layout::size_of::<crate::heap::Shared>() as int
}

/// [`shared_static_len`] is the size of a `Shared`.
pub proof fn lemma_shared_static_len()
    ensures
        shared_static_len() == vstd::layout::size_of::<crate::heap::Shared>() as int,
{
}

/// The process's regions: the `regions` of the one `Shared` a process
/// publishes (`Shared::wf` ties them together). The allocation paths that hold no `Shared`
/// (a cache pop, a slab table's slot address) state their results
/// against it. The unit tests build several `Shared`s over different
/// regions; the assumption is about the shipped library.
pub uninterp spec fn process_regions() -> Regions;

/// Slab `slab` is a guard slab of a class table whose slabs per page is
/// `1 << lg` and whose guard mask is `mask` (`SlabTable::is_guard_slab`):
/// every `mask + 1`-th page group, counted from 1. With guards off the
/// mask is all ones and only page group `u32::MAX` would match, which no
/// region has.
#[verifier::opaque]
pub open spec fn guard_of(lg: u8, mask: u32, slab: usize) -> bool {
    let pg = (slab >> lg) as u32;
    (if pg == u32::MAX { 0u32 } else { (pg + 1) as u32 }) & mask == 0
}

/// The guard geometry of the process's class `c` table: its
/// `lg_slabs_per_page` and `guard_mask`. Uninterpreted; the table fixes
/// it (`class::table::axiom_slot_loc`, the one-table-per-class
/// assumption the slot tokens' authority already rests on).
pub uninterp spec fn guard_geom(c: int) -> (u8, u32);

/// Slab `slab` of class `c`'s region is one of the process's guard slabs
/// (`SlabTable::is_guard_slab` of the process's class `c` table): never
/// committed past its first page and never handed out.
pub open spec fn guard_slab(c: int, slab: int) -> bool {
    guard_of(guard_geom(c).0, guard_geom(c).1, slab as usize)
}

/// The process's page is at most 64 KiB: Linux on x86_64 uses 4 KiB pages
/// and on aarch64 4, 16 or 64 KiB. Trusted: a kernel fact the code relies on
/// without checking it (the background thread's stack keeps one page as its
/// guard out of 256 KiB).
pub axiom fn axiom_process_page_at_most_64k()
    ensures
        process_regions().lg_page <= 16;

/// Entries `lo..hi` of `s` are slot starts of class `c` in the process's
/// regions.
pub open spec fn slots_in(s: Seq<*mut u8>, c: int, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> process_regions().is_slot(c, (#[trigger] s[i])@.addr)
}

/// Entries `lo..hi` of `s` are bit addresses of class `c`
/// ([`Regions::is_bit_slot`]) in the process's regions.
pub open spec fn bit_slots_in(s: Seq<*mut u8>, c: int, lo: int, hi: int) -> bool {
    forall|i: int| lo <= i < hi ==> process_regions().is_bit_slot(c, (#[trigger] s[i])@.addr)
}

/// What a huge allocation of `size` bytes at `align` (a power of two)
/// returns: a page start in the process's huge region with `size` bytes
/// (at least one) of the region from it, a multiple of `align`.
pub open spec fn huge_block(p: usize, size: usize, align: usize) -> bool {
    &&& process_regions().is_huge_block(p, if size == 0 { 1 } else { size as int })
    &&& (p as int) % (align as int) == 0
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
    /// What [`from_parts`] establishes and every classification relies on:
    /// shift amounts in range, and every class region, the huge and
    /// metadata regions and the three mappings below the top of the
    /// address space.
    ///
    /// [`from_parts`]: Regions::from_parts
    pub open spec fn wf(self) -> bool {
        &&& LG_MAX_SLAB <= self.lg_region < self.lg_stride < 64
        &&& self.lg_region <= LG_REGION_PREFERRED
        &&& self.lg_page < 64
        &&& forall|c: int| 0 <= c < NCLASSES ==>
            #[trigger] self.class_base[c] + (1usize << self.lg_region) <= usize::MAX
        &&& self.huge_base + self.huge_len <= usize::MAX
        &&& self.meta_base + self.meta_len <= usize::MAX
        &&& self.superblock_map.addr + self.superblock_map.len <= usize::MAX
        &&& self.huge_map.addr + self.huge_map.len <= usize::MAX
        &&& self.meta_map.addr + self.meta_map.len <= usize::MAX
    }

    /// The layout facts region separation rests on, on top of
    /// [`wf`](Regions::wf): every class base is `MAX_SLAB`-aligned and
    /// class `c`'s region lies inside stride `c` of the superblock (so the
    /// class regions are pairwise disjoint and each is followed by its
    /// gap), the strides starting above address 0 and ending below `2^48`;
    /// every class base and the huge base are above address 0, so no slot
    /// and no huge page start is null ([`Regions::lemma_block_nonzero`]);
    /// the huge base is `MAX_SLAB`-aligned and the page is between 4 KiB
    /// and `MAX_SLAB`; and the superblock's strides, the huge region and
    /// the metadata region are pairwise disjoint, and none holds a byte of
    /// the process's `Shared` static ([`Regions::outside`]). [`from_parts`]
    /// proves it from its arguments; the disjointness comes from its caller
    /// ([`from_parts`]'s preconditions say what).
    ///
    /// [`from_parts`]: Regions::from_parts
    pub open spec fn separated(self) -> bool {
        let stride = (1usize << self.lg_stride) as int;
        let region = (1usize << self.lg_region) as int;
        &&& 12 <= self.lg_page <= LG_MAX_SLAB
        &&& 0 < self.superblock
        &&& self.superblock + NCLASSES * stride <= 0x1_0000_0000_0000
        &&& self.superblock + NCLASSES * stride <= crate::sys::pac::user_va_limit()
        &&& forall|c: int| 0 <= c < NCLASSES ==> {
            &&& (#[trigger] self.class_base[c] as int) % (MAX_SLAB as int) == 0
            &&& 0 < self.class_base[c]
            &&& self.superblock + c * stride <= self.class_base[c]
            &&& self.class_base[c] + region <= self.superblock + (c + 1) * stride
        }
        &&& 0 < self.huge_base
        &&& (self.huge_base as int) % (MAX_SLAB as int) == 0
        &&& disjoint(self.superblock as int, NCLASSES * stride, self.huge_base as int, self.huge_len as int)
        &&& disjoint(self.superblock as int, NCLASSES * stride, self.meta_base as int, self.meta_len as int)
        &&& disjoint(self.huge_base as int, self.huge_len as int, self.meta_base as int, self.meta_len as int)
        &&& self.outside(shared_static_addr(), shared_static_len())
    }

    /// `[a, a + len)` shares no byte with the class superblock's strides,
    /// the huge region or the metadata region. [`separated`] says so of
    /// the process's `Shared` static ([`shared_static_addr`]): the three
    /// reservations are `mmap`s that the kernel places apart from the
    /// loaded object's data (`linux::Reserved::is_apart_from_static`,
    /// trusted).
    ///
    /// [`separated`]: Regions::separated
    pub open spec fn outside(self, a: int, len: int) -> bool {
        let stride = (1usize << self.lg_stride) as int;
        &&& disjoint(self.superblock as int, NCLASSES * stride, a, len)
        &&& disjoint(self.huge_base as int, self.huge_len as int, a, len)
        &&& disjoint(self.meta_base as int, self.meta_len as int, a, len)
    }

    /// The regions inside the three reservations they were carved from:
    /// the superblock's strides inside `superblock_map`, the huge region
    /// inside `huge_map`, the metadata region inside `meta_map`; the three
    /// mappings pairwise disjoint and none holding a byte of the process's
    /// `Shared` static. [`from_parts`] proves it from its preconditions,
    /// which [`Regions::reserve_owning`] proves from the kernel's `mmap`
    /// contract (`linux::Reserved`, trusted).
    ///
    /// [`from_parts`]: Regions::from_parts
    pub open spec fn mapped(self) -> bool {
        let stride = (1usize << self.lg_stride) as int;
        let (s, h, m) = (self.superblock_map, self.huge_map, self.meta_map);
        &&& s.addr <= self.superblock
        &&& self.superblock + NCLASSES * stride <= s.addr + s.len
        &&& h.addr <= self.huge_base
        &&& self.huge_base + self.huge_len <= h.addr + h.len
        &&& m.addr <= self.meta_base
        &&& self.meta_base + self.meta_len <= m.addr + m.len
        &&& disjoint(s.addr as int, s.len as int, h.addr as int, h.len as int)
        &&& disjoint(s.addr as int, s.len as int, m.addr as int, m.len as int)
        &&& disjoint(h.addr as int, h.len as int, m.addr as int, m.len as int)
        &&& disjoint(s.addr as int, s.len as int, shared_static_addr(), shared_static_len())
        &&& disjoint(h.addr as int, h.len as int, shared_static_addr(), shared_static_len())
        &&& disjoint(m.addr as int, m.len as int, shared_static_addr(), shared_static_len())
    }

    /// `[a, a + len)` lies inside one of the three reservations.
    pub open spec fn in_mapping(self, a: int, len: int) -> bool {
        let (s, h, m) = (self.superblock_map, self.huge_map, self.meta_map);
        ||| s.addr <= a && a + len <= s.addr + s.len
        ||| h.addr <= a && a + len <= h.addr + h.len
        ||| m.addr <= a && a + len <= m.addr + m.len
    }

    /// A range `seal` may hand `mseal`: non-empty, inside one of challoc's
    /// reservations ([`Regions::in_mapping`]), and sharing no byte with any
    /// class region, the huge region, the metadata region or the process's
    /// `Shared` static. Opaque, so that the quantifier over the classes
    /// stays out of the queries that only carry it;
    /// [`Regions::lemma_seal_target`] unfolds it.
    #[verifier::opaque]
    pub open spec fn seal_target(self, m: Mapping) -> bool {
        let region = (1usize << self.lg_region) as int;
        &&& m.len > 0
        &&& self.in_mapping(m.addr as int, m.len as int)
        &&& forall|c: int| 0 <= c < NCLASSES ==>
            disjoint(#[trigger] self.class_base[c] as int, region, m.addr as int, m.len as int)
        &&& disjoint(self.huge_base as int, self.huge_len as int, m.addr as int, m.len as int)
        &&& disjoint(self.meta_base as int, self.meta_len as int, m.addr as int, m.len as int)
        &&& disjoint(m.addr as int, m.len as int, shared_static_addr(), shared_static_len())
    }

    /// [`Regions::seal_target`], unfolded.
    pub proof fn lemma_seal_target(self, m: Mapping)
        requires
            self.seal_target(m),
        ensures
            m.len > 0,
            self.in_mapping(m.addr as int, m.len as int),
            forall|c: int| 0 <= c < NCLASSES ==>
                disjoint(#[trigger] self.class_base[c] as int, (1usize << self.lg_region) as int, m.addr as int, m.len as int),
            disjoint(self.huge_base as int, self.huge_len as int, m.addr as int, m.len as int),
            disjoint(self.meta_base as int, self.meta_len as int, m.addr as int, m.len as int),
            disjoint(m.addr as int, m.len as int, shared_static_addr(), shared_static_len()),
    {
        reveal(Regions::seal_target);
    }

    /// Class `j`'s region ends at or before class `k`'s starts, for `j <
    /// k`: each lies inside its own stride.
    pub proof fn lemma_class_before(self, j: int, k: int)
        requires
            self.separated(),
            0 <= j < k < NCLASSES,
        ensures
            self.class_base[j] + (1usize << self.lg_region) <= self.class_base[k],
    {
        let stride = (1usize << self.lg_stride) as int;
        assert(self.superblock + j * stride <= self.class_base[j]);
        assert(self.superblock + k * stride <= self.class_base[k]);
        assert((j + 1) * stride <= k * stride) by (nonlinear_arith)
            requires
                j + 1 <= k,
                stride >= 0,
        ;
    }

    /// Class `c`'s region lies inside the superblock's strides.
    pub proof fn lemma_class_in_strides(self, c: int)
        requires
            self.separated(),
            0 <= c < NCLASSES,
        ensures
            self.superblock <= self.class_base[c],
            self.class_base[c] + (1usize << self.lg_region) <= self.superblock + NCLASSES * ((1usize << self.lg_stride) as int),
    {
        let stride = (1usize << self.lg_stride) as int;
        assert(self.superblock + c * stride <= self.class_base[c]);
        assert(self.class_base[c] + (1usize << self.lg_region) <= self.superblock + (c + 1) * stride);
        assert(0 <= c * stride && (c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
            requires
                0 <= c < NCLASSES,
                stride >= 0,
        ;
    }

    /// What `Shared::init` needs of its regions beyond [`wf`] and
    /// [`separated`]: a metadata region `Meta::owning` accepts (page
    /// multiples, non-empty, at a non-zero address below the metadata
    /// limit, a page of at least `MIN_ALIGN`) and a huge region `Huge::new`
    /// accepts (1 to `u32::MAX` pages, below the limit).
    /// [`Regions::reserve_owning`] proves it.
    ///
    /// [`wf`]: Regions::wf
    /// [`separated`]: Regions::separated
    pub open spec fn ready(self) -> bool {
        let page = 1usize << self.lg_page;
        &&& page >= crate::meta::MIN_ALIGN
        &&& self.meta_base > 0
        &&& self.meta_base % page == 0
        &&& self.meta_len % page == 0
        &&& self.meta_len > 0
        &&& self.meta_base + self.meta_len <= crate::meta::va_limit()
        &&& 0 < self.huge_len >> self.lg_page <= u32::MAX
        &&& self.huge_base + self.huge_len <= crate::meta::va_limit()
    }

    /// Slabs in class `c`'s region ([`Regions::nslabs`]).
    pub open spec fn spec_nslabs(self, c: int) -> int {
        ((1usize << self.lg_region) >> class_table()[c].lg_slab) as int
    }

    /// Address of slot `slot` of slab `slab` in class `c`'s region
    /// ([`Regions::slot_addr`]).
    pub open spec fn slot_at(self, c: int, slab: int, slot: int) -> int {
        self.class_base[c] + slab * ((1usize << class_table()[c].lg_slab) as int) + slot * (class_table()[c].size as int)
    }

    /// `p` is a slot start of class `c`: slot `slot < nslots` of slab
    /// `slab` of the class region, for some slab and slot, guard slabs
    /// included. What the pointer arithmetic of the free paths checks
    /// ([`Regions::slab_slot`], [`Regions::slot_in_class`]).
    pub open spec fn is_slot_addr(self, c: int, p: usize) -> bool {
        &&& 0 <= c < NCLASSES
        &&& exists|slab: int, slot: int|
            0 <= slab < self.spec_nslabs(c) && 0 <= slot < class_table()[c].nslots
                && p as int == #[trigger] self.slot_at(c, slab, slot)
    }

    /// The slab of class `c`'s region that address `p` falls in.
    #[verifier::opaque]
    pub open spec fn slab_index(self, c: int, p: usize) -> int {
        (p - self.class_base[c]) / ((1usize << class_table()[c].lg_slab) as int)
    }

    /// `p` is a slot start of class `c` ([`Regions::is_slot_addr`]) in a
    /// slab that is not one of the process's guard slabs ([`guard_slab`]).
    /// What the slab paths hand out, and what a free that returns acted
    /// on: a guard slab's slots are never handed out (whole-allocator
    /// theorem 7, `verus/THEOREMS.md`).
    pub open spec fn is_slot(self, c: int, p: usize) -> bool {
        &&& self.is_slot_addr(c, p)
        &&& !guard_slab(c, self.slab_index(c, p))
    }

    /// The slab of slot `slot` of slab `slab` is `slab`.
    pub proof fn lemma_slab_index(self, c: int, slab: int, slot: int, p: usize)
        requires
            0 <= c < NCLASSES,
            0 <= slot < class_table()[c].nslots,
            p as int == self.slot_at(c, slab, slot),
        ensures
            self.slab_index(c, p) == slab,
    {
        reveal(Regions::slab_index);
        crate::vspec::class::axiom_class_table_wf(c);
        let cls = class_table()[c];
        let lg = cls.lg_slab;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        let s = (1usize << lg) as int;
        assert(0 <= slot * (cls.size as int) < s) by (nonlinear_arith)
            requires
                0 <= slot < cls.nslots,
                (cls.nslots as int) * (cls.size as int) <= s,
                cls.size > 0,
        ;
        assert(self.slot_at(c, slab, slot) - self.class_base[c] == slab * s + slot * (cls.size as int));
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(
            slab * s + slot * (cls.size as int), s, slab, slot * (cls.size as int));
    }

    /// Slot `slot` of slab `slab` of class `c`, in the region and not in a
    /// guard slab, is a slot start ([`Regions::is_slot`]).
    pub proof fn lemma_is_slot(self, c: int, slab: int, slot: int, p: usize)
        requires
            0 <= c < NCLASSES,
            0 <= slab < self.spec_nslabs(c),
            0 <= slot < class_table()[c].nslots,
            p as int == self.slot_at(c, slab, slot),
            !guard_slab(c, slab),
        ensures
            self.is_slot(c, p),
            self.is_slot_addr(c, p),
    {
        self.lemma_slab_index(c, slab, slot, p);
    }

    /// A class region has at most `2^26` slabs (`2^38` bytes of slabs of at
    /// least 4 KiB).
    pub proof fn lemma_nslabs_bound(self, c: int)
        requires
            self.wf(),
            0 <= c < NCLASSES,
        ensures
            self.spec_nslabs(c) <= 0x400_0000,
    {
        crate::vspec::class::axiom_class_table_wf(c);
        let (lr, ls) = (self.lg_region, class_table()[c].lg_slab);
        assert(LG_REGION_PREFERRED == 38 && super::LG_MIN_SLAB == 12);
        assert((1usize << lr) >> ls <= 0x400_0000) by (bit_vector)
            requires lr <= 38, 12 <= ls;
    }

    /// [`Regions::is_slot`] with the slot bound relaxed to the bitmap's
    /// `slab_bits`, padding included: the address of bit `slot` of slab
    /// `slab`, which is what a claim computes for a bit it won.
    pub open spec fn is_bit_slot(self, c: int, p: usize) -> bool {
        &&& 0 <= c < NCLASSES
        &&& exists|slab: int, slot: int|
            0 <= slab < self.spec_nslabs(c) && 0 <= slot < class_table()[c].slab_bits
                && p as int == #[trigger] self.slot_at(c, slab, slot)
    }

    /// `p` is a page start in the huge region with `len > 0` bytes of the
    /// region from `p` on. What the huge paths hand out.
    pub open spec fn is_huge_block(self, p: usize, len: int) -> bool {
        &&& 0 < len
        &&& self.huge_base <= p
        &&& p + len <= self.huge_base + self.huge_len
        &&& (p - self.huge_base) % ((1usize << self.lg_page) as int) == 0
    }

    /// `p` is a block an allocation of `size` bytes may return: a slot
    /// start of a class at least `size` bytes large, or a page start in
    /// the huge region with `size` bytes of the region from it. Region
    /// separation ([`Regions::lemma_user_block_separated`]) and fit (the
    /// usable size is at least `size`) follow.
    pub open spec fn user_block(self, p: usize, size: int) -> bool {
        ||| exists|c: int| #[trigger] self.is_slot(c, p) && size <= class_table()[c].size
        ||| self.is_huge_block(p, if size < 1 { 1 } else { size })
    }

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
    ) -> (r: Regions)
        requires
            lg_stride > lg_region >= LG_MAX_SLAB,
            lg_stride <= 57,
            superblock & ((MAX_SLAB - 1) as usize) == 0,
            forall|c: int| 0 <= c < NCLASSES ==>
                (#[trigger] offsets[c]) & ((MAX_SLAB - 1) as usize) == 0
                && offsets[c] < (1usize << lg_stride) - (1usize << lg_region),
            superblock + (NCLASSES << lg_stride) <= usize::MAX,
            lg_region <= LG_REGION_PREFERRED,
            lg_page < 64,
            huge.0 + huge.1 <= usize::MAX,
            meta.0 + meta.1 <= usize::MAX,
            forall|i: int| 0 <= i < 3 ==> #[trigger] maps[i].addr + maps[i].len <= usize::MAX,
            // What `reserve` proves for `separated`: each region lies inside
            // its own mapping, the huge base is aligned like the class
            // bases, and the page is between 4 KiB (`reserve`'s caller
            // passes the kernel's page) and `MAX_SLAB` (`reserve` asserts
            // the bound). The mappings are three `mmap`s without
            // `MAP_FIXED`, which the kernel places apart from each other
            // (`linux::Reserved::is_disjoint`, trusted).
            maps[0].addr <= superblock,
            superblock + (NCLASSES << lg_stride) <= maps[0].addr + maps[0].len,
            // The class superblock is user space, above address 0 (the
            // kernel places no mapping there) and below
            // `sys::pac::user_va_limit` (at most `2^48`).
            0 < superblock,
            superblock + (NCLASSES << lg_stride) <= crate::sys::pac::user_va_limit(),
            maps[1].addr <= huge.0,
            // The huge region is user space too, above address 0.
            0 < huge.0,
            huge.0 + huge.1 <= maps[1].addr + maps[1].len,
            maps[2].addr <= meta.0,
            meta.0 + meta.1 <= maps[2].addr + maps[2].len,
            disjoint(maps[0].addr as int, maps[0].len as int, maps[1].addr as int, maps[1].len as int),
            disjoint(maps[0].addr as int, maps[0].len as int, maps[2].addr as int, maps[2].len as int),
            disjoint(maps[1].addr as int, maps[1].len as int, maps[2].addr as int, maps[2].len as int),
            // The process's `Shared` static is in none of them
            // (`linux::Reserved::is_apart_from_static`, trusted).
            forall|i: int| 0 <= i < 3 ==> disjoint(#[trigger] maps[i].addr as int, maps[i].len as int, shared_static_addr(), shared_static_len()),
            huge.0 & ((MAX_SLAB - 1) as usize) == 0,
            12 <= lg_page <= LG_MAX_SLAB,
        ensures
            r.wf(),
            r.separated(),
            r.mapped(),
            r.lg_region == lg_region && r.lg_stride == lg_stride && r.superblock == superblock,
            r.huge_base == huge.0 && r.huge_len == huge.1,
            r.meta_base == meta.0 && r.meta_len == meta.1,
            r.lg_page == lg_page,
            forall|c: int| 0 <= c < NCLASSES ==>
                #[trigger] r.class_base[c] == superblock + ((c as usize) << lg_stride) + offsets[c],
    {
        proof {
            crate::sys::pac::axiom_user_va_limit();
            assert((1usize << lg_stride) > (1usize << lg_region)) by (bit_vector)
                requires lg_region < lg_stride < 64;
            assert(MAX_SLAB == 0x40_0000) by (bit_vector);
        }
        assert!(lg_stride > lg_region && lg_region >= LG_MAX_SLAB);
        assert!(superblock & (MAX_SLAB - 1) == 0);
        let gap = (1usize << lg_stride) - (1usize << lg_region);
        let mut class_base = [0usize; NCLASSES];
        let mut c = 0;
        while c < NCLASSES
            invariant
                0 <= c <= NCLASSES,
                gap == (1usize << lg_stride) - (1usize << lg_region),
                lg_region < lg_stride <= 57,
                MAX_SLAB == 0x40_0000,
                superblock + (NCLASSES << lg_stride) <= usize::MAX,
                forall|j: int| 0 <= j < NCLASSES ==>
                    (#[trigger] offsets[j]) & ((MAX_SLAB - 1) as usize) == 0 && offsets[j] < gap,
                forall|j: int| 0 <= j < c ==>
                    #[trigger] class_base[j] == superblock + ((j as usize) << lg_stride) + offsets[j],
            decreases NCLASSES - c,
        {
            assert!(offsets[c] & (MAX_SLAB - 1) == 0 && offsets[c] < gap);
            proof {
                let n = NCLASSES;
                assert((c << lg_stride) + gap <= (n << lg_stride)) by (bit_vector)
                    requires c < n, n <= 64, lg_region < lg_stride <= 57,
                        gap == (1usize << lg_stride) - (1usize << lg_region);
            }
            class_base[c] = superblock + (c << lg_stride) + offsets[c];
            c += 1;
        }
        proof {
            assert forall|j: int| 0 <= j < NCLASSES implies #[trigger] class_base[j] + (1usize << lg_region) <= usize::MAX by {
                let (n, jj, o, sb) = (NCLASSES, j as usize, offsets[j], superblock);
                assert(sb + (jj << lg_stride) + o + (1usize << lg_region) <= usize::MAX) by (bit_vector)
                    requires jj < n, n <= 64, lg_region < lg_stride <= 57, o < (1usize << lg_stride) - (1usize << lg_region),
                        sb + (n << lg_stride) <= usize::MAX;
            }
            let stride = (1usize << lg_stride) as int;
            let region = (1usize << lg_region) as int;
            let m = MAX_SLAB as int;
            lemma_shl_mul(NCLASSES, lg_stride);
            lemma_mask_mod(superblock);
            lemma_mask_mod(huge.0);
            lemma_pow2_multiple(lg_stride);
            assert forall|j: int| 0 <= j < NCLASSES implies {
                &&& (#[trigger] class_base[j] as int) % m == 0
                &&& 0 < class_base[j]
                &&& superblock + j * stride <= class_base[j]
                &&& class_base[j] + region <= superblock + (j + 1) * stride
            } by {
                let jj = j as usize;
                vstd::arithmetic::mul::lemma_mul_nonnegative(j, stride);
                lemma_shl_mul(jj, lg_stride);
                lemma_mask_mod(offsets[j]);
                lemma_mod_lin3(superblock as int, j, stride, 1, offsets[j] as int, m);
                assert((j + 1) * stride == j * stride + stride) by (nonlinear_arith);
            }
            // Each region lies inside its mapping, which the static is not in.
            assert(disjoint(maps[0].addr as int, maps[0].len as int, shared_static_addr(), shared_static_len()));
            assert(disjoint(maps[1].addr as int, maps[1].len as int, shared_static_addr(), shared_static_len()));
            assert(disjoint(maps[2].addr as int, maps[2].len as int, shared_static_addr(), shared_static_len()));
        }
        Regions {
            superblock,
            lg_region,
            lg_stride,
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
    pub const fn region_len(&self) -> (r: usize)
        requires self.wf(),
        ensures r == 1usize << self.lg_region,
        opens_invariants none
    {
        1 << self.lg_region
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
    pub const fn page_size(&self) -> (r: usize)
        requires self.wf(),
        ensures r == 1usize << self.lg_page, r >= 1,
    {
        proof {
            let lg = self.lg_page;
            assert((1usize << lg) >= 1) by (bit_vector) requires lg < 64;
        }
        1 << self.lg_page
    }

    /// Slabs that fit in a class region.
    #[inline]
    pub fn nslabs(&self, class: usize) -> (r: usize)
        requires self.wf(), class < NCLASSES,
        ensures r == (1usize << self.lg_region) >> class_table()[class as int].lg_slab,
    {
        let table = CLASSES.table();
        proof {
            assert(table[class as int].wf());
        }
        self.region_len() >> table[class].lg_slab
    }

    /// Pages in the huge region.
    #[inline]
    pub const fn huge_pages(&self) -> (r: usize)
        requires self.wf(),
        ensures r == self.huge_len >> self.lg_page,
    {
        self.huge_len >> self.lg_page
    }

    #[inline]
    pub fn slab_addr(&self, class: usize, slab: usize) -> (r: usize)
        requires
            self.wf(),
            class < NCLASSES,
            slab < (1usize << self.lg_region) >> class_table()[class as int].lg_slab,
        ensures r == self.class_base[class as int] + (slab << class_table()[class as int].lg_slab),
    {
        let table = CLASSES.table();
        proof {
            assert(table[class as int].wf());
        }
        let lg = table[class].lg_slab;
        proof {
            let lr = self.lg_region;
            let rl = 1usize << lr;
            assert(self.class_base[class as int] + rl <= usize::MAX);
            assert((slab << lg) < rl) by (bit_vector)
                requires slab < rl >> lg, lg <= 22, rl == 1usize << lr, lr < 64;
        }
        self.class_base[class] + (slab << lg)
    }

    #[inline]
    #[cfg(test)]
    pub fn slot_addr(&self, class: usize, slab: usize, slot: usize) -> usize {
        self.slab_addr(class, slab) + slot * CLASSES[class].size as usize
    }

    #[inline]
    pub const fn huge_addr(&self, page: usize) -> (r: usize)
        requires self.wf(), page < self.huge_len >> self.lg_page,
        ensures r == self.huge_base + (page << self.lg_page),
    {
        proof {
            let (len, lg) = (self.huge_len, self.lg_page);
            assert((page << lg) < len) by (bit_vector)
                requires page < len >> lg, lg < 64;
        }
        self.huge_base + (page << self.lg_page)
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
    pub fn slab_slot(&self, p: usize) -> (r: Option<(usize, usize, usize)>)
        requires self.wf(),
        ensures
            r matches Some((c, slab, slot)) ==> {
                let cls = class_table()[c as int];
                let rel = p.wrapping_sub(self.class_base[c as int]);
                &&& c < NCLASSES
                &&& cls.wf()
                &&& rel < (1usize << self.lg_region)
                &&& slab == rel >> cls.lg_slab
                &&& slot < cls.nslots
                &&& rel & (((1usize << cls.lg_slab) - 1) as usize) == slot * cls.size
                &&& self.is_slot_addr(c as int, p)
                &&& p as int == self.slot_at(c as int, slab as int, slot as int)
                &&& (slab as int) < self.spec_nslabs(c as int)
            },
            // Exact with separated regions: every slot start is found, in
            // its class.
            self.separated() ==> forall|c: int| #[trigger] self.is_slot_addr(c, p) ==> (r matches Some((c2, _, _)) && c2 as int == c),
        opens_invariants none
    {
        proof {
            if self.separated() {
                assert forall|c: int| #[trigger] self.is_slot_addr(c, p) implies {
                    &&& (p.wrapping_sub(self.superblock) >> self.lg_stride) as int == c
                    &&& p.wrapping_sub(self.class_base[c]) < (1usize << self.lg_region)
                } by {
                    self.lemma_slot_addr_found(c, p);
                }
            }
        }
        let c = p.wrapping_sub(self.superblock) >> self.lg_stride;
        if c >= NCLASSES {
            return None;
        }
        // SAFETY: `c < NCLASSES` was just checked; both arrays have
        // `NCLASSES` entries.
        let cls = unsafe { class_entry(c) };
        proof {
            assert(cls.wf());
        }
        let rel = p.wrapping_sub(unsafe { *crate::vspec::slice::array_get_unchecked(&self.class_base, c) });
        if rel >= self.region_len() {
            return None;
        }
        let slab = rel >> cls.lg_slab;
        let offset = rel & cls.slab_mask();
        proof {
            let lg = cls.lg_slab;
            let m = cls.slab_mask as usize;
            assert(m == ((1usize << lg) - 1) as usize);
            assert(MAX_SLAB == 0x40_0000) by (bit_vector);
            assert((rel & m) < (1usize << lg)) by (bit_vector)
                requires lg <= 22, m == ((1usize << lg) - 1) as usize;
            assert(self.class_base@[c as int] == self.class_base[c as int]);
        }
        let (slot, valid) = cls.slot_of_offset(offset);
        proof {
            if self.separated() && self.is_slot_addr(c as int, p) {
                self.lemma_slot_addr_found(c as int, p);
            }
        }
        if !valid {
            return None;
        }
        proof {
            self.lemma_slot_of(c as int, p, slot);
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
    fn in_superblock(&self, p: usize) -> (r: bool)
        requires self.wf(),
        ensures
            // With the strides below `2^48`, a hit lies in a stride.
            r && self.separated() ==> self.superblock <= p && p < self.superblock + NCLASSES * ((1usize << self.lg_stride) as int),
    {
        proof {
            if self.separated() {
                let (sb, ls) = (self.superblock, self.lg_stride);
                let n = NCLASSES;
                let lim = n << ls;
                assert(n * ((1usize << ls) as int) <= 0x1_0000_0000_0000);
                vstd::arithmetic::power2::lemma_pow2_pos(ls as nat);
                vstd::bits::lemma_usize_pow2_no_overflow(ls as nat);
                vstd::bits::lemma_usize_shl_is_mul(1, ls as usize);
                assert(n * vstd::arithmetic::power2::pow2(ls as nat) <= usize::MAX);
                vstd::bits::lemma_usize_shl_is_mul(n, ls as usize);
                assert(lim as int == n * ((1usize << ls) as int));
                vstd::bits::lemma_usize_shr_is_div(lim, ls as usize);
                vstd::arithmetic::div_mod::lemma_div_multiples_vanish(n as int, vstd::arithmetic::power2::pow2(ls as nat) as int);
                assert(lim >> ls == n);
                let d = p.wrapping_sub(sb);
                assert(d == (if p >= sb { (p - sb) as usize } else { (p - sb + 0x1_0000_0000_0000_0000) as usize }));
                assert(d >> ls < n ==> d < lim) by (bit_vector)
                    requires
                        lim == n << ls,
                        lim >> ls == n,
                        ls < 64,
                ;
            }
        }
        p.wrapping_sub(self.superblock) >> self.lg_stride < NCLASSES
    }

    /// Classify a pointer (design section 2): [`slab_slot`] for the class
    /// regions, then the huge region.
    ///
    /// [`slab_slot`]: Regions::slab_slot
    #[inline]
    pub fn classify(&self, p: usize) -> (r: Kind)
        requires self.wf(),
        ensures
            r matches Kind::Slab { class, slab, slot } ==>
                class < NCLASSES && slot < class_table()[class as int].nslots && self.is_slot_addr(class as int, p)
                    && p as int == self.slot_at(class as int, slab as int, slot as int) && (slab as int) < self.spec_nslabs(class as int),
            r matches Kind::Huge { page } ==> page <= self.huge_len >> self.lg_page,
            // Exact: a page start of the huge region, `page` its index.
            r matches Kind::Huge { page } ==> self.is_huge_block(p, 1) && page == ((p - self.huge_base) as usize) >> self.lg_page,
            // And every page start of the huge region is `Huge`.
            self.separated() && self.is_huge_block(p, 1) ==> r matches Kind::Huge { .. },
            // Exact with separated regions: a slot start is `Slab`, of its
            // class ([`Regions::slab_slot`]).
            self.separated() ==> forall|c: int| #[trigger] self.is_slot_addr(c, p) ==> (r matches Kind::Slab { class, .. } && class as int == c),
    {
        if let Some((class, slab, slot)) = self.slab_slot(p) {
            proof {
                let rel = p.wrapping_sub(self.class_base[class as int]);
                let lr = self.lg_region;
                let lg = class_table()[class as int].lg_slab;
                assert(rel >> lg < 0x1_0000_0000usize) by (bit_vector)
                    requires rel < (1usize << lr), lr <= 38, 12 <= lg;
                assert(MAX_SLAB == 0x40_0000) by (bit_vector);
                // A slot lies in its class's stride, apart from the huge
                // region.
                if self.separated() && self.is_huge_block(p, 1) {
                    let c = class as int;
                    let stride = (1usize << self.lg_stride) as int;
                    self.lemma_slot_in_region(c, p);
                    assert(self.superblock + c * stride <= self.class_base[c]);
                    assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
                        requires
                            c < NCLASSES,
                            stride >= 0,
                    ;
                }
            }
            return Kind::Slab { class: class as u32, slab: slab as u32, slot: slot as u32 };
        }
        if self.in_superblock(p) {
            return Kind::Invalid;
        }
        let rel = p.wrapping_sub(self.huge_base);
        if rel < self.huge_len && rel & (self.page_size() - 1) == 0 {
            proof {
                let (len, lg) = (self.huge_len, self.lg_page);
                assert(rel >> lg <= len >> lg) by (bit_vector)
                    requires rel < len, lg < 64;
                let base = self.huge_base;
                assert(base <= p && rel == p - base);
                assert(rel % (1usize << lg) == 0) by (bit_vector)
                    requires rel & sub(1usize << lg, 1) == 0, lg < 64;
            }
            return Kind::Huge { page: rel >> self.lg_page };
        }
        proof {
            // A page start of the huge region passes the test above.
            if self.separated() && self.is_huge_block(p, 1) {
                let lg = self.lg_page;
                assert(rel == p - self.huge_base);
                assert(rel % (1usize << lg) == 0);
                assert(rel & sub(1usize << lg, 1) == 0) by (bit_vector)
                    requires rel % (1usize << lg) == 0, lg < 64;
            }
        }
        Kind::Invalid
    }

    /// True when `p` is anywhere inside the metadata reservation.
    #[cfg(test)]
    pub fn in_meta(&self, p: usize) -> bool {
        p.wrapping_sub(self.meta_base) < self.meta_len
    }
}

/// `x << s` is `x * 2^s` for the multipliers and shifts of the layout.
pub proof fn lemma_shl_mul(x: usize, s: u32)
    requires
        x <= 64,
        s <= 57,
    ensures
        (x << s) as int == x * ((1usize << s) as int),
        (1usize << s) as int == pow2(s as nat),
{
    vstd::arithmetic::power2::lemma2_to64();
    vstd::arithmetic::power2::lemma2_to64_rest();
    if s < 57 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(s as nat, 57);
    }
    let p = pow2(s as nat) as int;
    assert(x * p <= 64 * 0x200_0000_0000_0000) by (nonlinear_arith)
        requires
            x <= 64,
            0 <= p <= 0x200_0000_0000_0000,
    ;
    vstd::bits::lemma_usize_shl_is_mul(x, s as usize);
    vstd::bits::lemma_usize_shl_is_mul(1, s as usize);
}

/// A `MAX_SLAB`-aligned address, as a remainder.
pub proof fn lemma_mask_mod(x: usize)
    ensures
        x & ((MAX_SLAB - 1) as usize) == 0 ==> (x as int) % (MAX_SLAB as int) == 0,
{
    assert(MAX_SLAB == 0x40_0000) by (bit_vector);
    assert(x & 0x3f_ffffusize == 0 ==> x % 0x40_0000usize == 0) by (bit_vector);
}

/// `2^s` is a multiple of `MAX_SLAB` for `s` at least `LG_MAX_SLAB`.
proof fn lemma_pow2_multiple(s: u32)
    requires
        LG_MAX_SLAB <= s <= 57,
    ensures
        ((1usize << s) as int) % (MAX_SLAB as int) == 0,
{
    lemma_shl_mul(1, s);
    vstd::arithmetic::power2::lemma2_to64();
    assert(MAX_SLAB == 0x40_0000) by (bit_vector);
    vstd::arithmetic::power2::lemma_pow2_adds(22, (s - 22) as nat);
    assert(pow2(22) * pow2((s - 22) as nat) == pow2((s - 22) as nat) * pow2(22)) by (nonlinear_arith);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(pow2((s - 22) as nat) as int, pow2(22) as int);
}

/// A sum of multiples of `m` is one.
pub proof fn lemma_mod_lin3(a: int, x: int, s: int, y: int, t: int, m: int)
    requires
        m > 0,
        a % m == 0,
        s % m == 0,
        t % m == 0,
    ensures
        (a + x * s + y * t) % m == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(a, m);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(s, m);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(t, m);
    let q = a / m + x * (s / m) + y * (t / m);
    assert(a + x * s + y * t == q * m) by (nonlinear_arith)
        requires
            a == m * (a / m),
            s == m * (s / m),
            t == m * (t / m),
            q == a / m + x * (s / m) + y * (t / m),
    ;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(q, m);
}

/// A power of two that divides `x > 0` is at most `x`, and divides every
/// power of two at least `x`.
pub proof fn lemma_pow2_divides_pow2(align: usize, x: int, l: nat)
    requires
        crate::meta::is_pow2(align),
        x > 0,
        x % (align as int) == 0,
        x <= pow2(l),
    ensures
        (pow2(l) as int) % (align as int) == 0,
{
    let k = choose|k: nat| k < 64 && align as nat == #[trigger] pow2(k);
    vstd::arithmetic::power2::lemma_pow2_pos(k);
    if x < align {
        vstd::arithmetic::div_mod::lemma_small_mod(x as nat, align as nat);
    }
    if k > l {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(l, k);
    }
    vstd::arithmetic::power2::lemma_pow2_adds(k, (l - k) as nat);
    assert(pow2(k) * pow2((l - k) as nat) == pow2((l - k) as nat) * pow2(k)) by (nonlinear_arith);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(pow2((l - k) as nat) as int, pow2(k) as int);
}

/// Divisibility is transitive: a multiple of `big`, itself a multiple of
/// `m`, is a multiple of `m`.
pub proof fn lemma_aligned_down(b: int, big: int, m: int)
    requires
        m > 0,
        big > 0,
        b % big == 0,
        big % m == 0,
    ensures
        b % m == 0,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b, big);
    assert(b == 0 + (b / big) * big + 0 * 0) by (nonlinear_arith)
        requires
            b == big * (b / big) + b % big,
            b % big == 0,
    ;
    lemma_mod_lin3(0, b / big, big, 0, 0, m);
}

impl Regions {
    /// What [`Regions::slab_slot`] and [`Regions::slot_in_class`] check
    /// (the offset from the class base is inside the region, and its part
    /// inside the slab is a whole number of slots below `nslots`) makes
    /// `p` a slot start, guard slabs included, at slot `slot` of slab
    /// `rel >> lg_slab`.
    pub proof fn lemma_slot_of(self, c: int, p: usize, slot: usize)
        requires
            self.wf(),
            0 <= c < NCLASSES,
            p.wrapping_sub(self.class_base[c]) < (1usize << self.lg_region),
            slot < class_table()[c].nslots,
            p.wrapping_sub(self.class_base[c]) & (((1usize << class_table()[c].lg_slab) - 1) as usize)
                == slot * class_table()[c].size,
        ensures
            self.is_slot_addr(c, p),
            p as int == self.slot_at(c, (p.wrapping_sub(self.class_base[c]) >> class_table()[c].lg_slab) as int, slot as int),
            (p.wrapping_sub(self.class_base[c]) >> class_table()[c].lg_slab) < self.spec_nslabs(c),
    {
        crate::vspec::class::axiom_class_table_wf(c);
        let b = self.class_base[c];
        let rel = p.wrapping_sub(b);
        let lg = class_table()[c].lg_slab;
        let lr = self.lg_region;
        let rl = 1usize << lr;
        assert(b + rl <= usize::MAX);
        assert(rel == p - b);
        let slab = rel >> lg;
        let mask = ((1usize << lg) - 1) as usize;
        assert(rel == ((rel >> lg) << lg) + (rel & mask) && slab < rl >> lg && slab <= usize::MAX >> lg) by (bit_vector)
            requires
                lg <= 22,
                22 <= lr < 64,
                rl == 1usize << lr,
                rel < rl,
                slab == rel >> lg,
                mask == ((1usize << lg) - 1) as usize,
        ;
        let lgu = lg as usize;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(slab, lgu, usize::MAX);
        assert(slab <= usize::MAX >> lgu) by (bit_vector)
            requires
                slab <= usize::MAX >> lg,
                lgu == lg as usize,
        ;
        vstd::bits::lemma_usize_shl_is_mul(slab, lgu);
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lgu);
        assert((slab << lg) == (slab << lgu)) by (bit_vector)
            requires
                lgu == lg as usize,
        ;
        assert(p as int == self.slot_at(c, slab as int, slot as int));
    }

    /// The converse of [`Regions::lemma_slot_of`]: a slot start of class
    /// `c`, guard slabs included, passes every test of
    /// [`Regions::slab_slot`]. Its stride of the superblock is `c`'s (the
    /// class regions lie in their strides), its offset from the class base
    /// is inside the region, and its part inside the slab is a whole
    /// number of slots below `nslots`. So `slab_slot` finds every slot
    /// start, and its class.
    pub proof fn lemma_slot_addr_found(self, c: int, p: usize)
        requires
            self.wf(),
            self.separated(),
            self.is_slot_addr(c, p),
        ensures
            (p.wrapping_sub(self.superblock) >> self.lg_stride) as int == c,
            p.wrapping_sub(self.class_base[c]) < (1usize << self.lg_region),
            ({
                let cls = class_table()[c];
                let off = p.wrapping_sub(self.class_base[c]) & (cls.slab_mask as usize);
                &&& off % (cls.size as usize) == 0
                &&& off / (cls.size as usize) < cls.nslots
            }),
    {
        let (slab, slot) = choose|slab: int, slot: int|
            0 <= slab < self.spec_nslabs(c) && 0 <= slot < class_table()[c].nslots
                && p as int == #[trigger] self.slot_at(c, slab, slot);
        crate::vspec::class::axiom_class_table_wf(c);
        let cls = class_table()[c];
        let lg = cls.lg_slab;
        let lgu = lg as usize;
        let lr = self.lg_region;
        let rl = 1usize << lr;
        let nu = rl >> lg;
        assert(nu == rl >> lgu) by (bit_vector)
            requires
                nu == rl >> lg,
                lgu == lg as usize,
        ;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lgu);
        let su = 1usize << lg;
        assert(su == (1usize << lgu) && su > 0) by (bit_vector)
            requires
                lgu == lg as usize,
                lg <= 22,
                su == 1usize << lg,
        ;
        let s = su as int;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(nu, lgu, rl);
        let z = cls.size as int;
        assert(slot * z + z <= s) by (nonlinear_arith)
            requires
                0 <= slot < cls.nslots,
                (cls.nslots as int) * z <= s,
                z >= 0,
        ;
        assert(slab * s + s <= nu * s) by (nonlinear_arith)
            requires
                0 <= slab < nu,
                s >= 0,
        ;
        assert(0 <= slot * z) by (nonlinear_arith)
            requires
                0 <= slot,
                z >= 0,
        ;
        assert(0 <= slab * s) by (nonlinear_arith)
            requires
                0 <= slab,
                s >= 0,
        ;
        // Inside the region, so inside stride `c`.
        let b = self.class_base[c];
        let rel = p.wrapping_sub(b);
        assert(b <= p && rel as int == slab * s + slot * z);
        assert(rel < rl);
        let lst = self.lg_stride;
        let ls = lst as usize;
        let stu = 1usize << lst;
        assert(stu == (1usize << ls)) by (bit_vector)
            requires
                ls == lst as usize,
                stu == 1usize << lst,
        ;
        let stride = stu as int;
        let sb = self.superblock;
        assert(sb + c * stride <= b);
        assert(b + rl <= sb + (c + 1) * stride);
        let d = p.wrapping_sub(sb);
        assert(d as int == p - sb);
        assert(c * stride <= d && d < c * stride + stride) by (nonlinear_arith)
            requires
                sb + c * stride <= b,
                b <= p,
                p < b + rl,
                b + rl <= sb + (c + 1) * stride,
                d as int == p - sb,
        ;
        vstd::bits::lemma_usize_pow2_no_overflow(ls as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, ls);
        vstd::bits::lemma_usize_shr_is_div(d, ls);
        assert(d >> lst == d >> ls) by (bit_vector)
            requires
                ls == lst as usize,
        ;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(d as int, stride, c, d - c * stride);
        // The part inside the slab: `rel % s`, the mask's low bits.
        let m = cls.slab_mask as usize;
        assert(m == ((1usize << lg) - 1) as usize);
        let q = rel >> lg;
        assert(rel == ((rel >> lg) << lg) + (rel & m) && (rel & m) < (1usize << lg) && q <= usize::MAX >> lg) by (bit_vector)
            requires
                lg <= 22,
                m == ((1usize << lg) - 1) as usize,
                q == rel >> lg,
        ;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(q, lgu, usize::MAX);
        assert(q <= usize::MAX >> lgu) by (bit_vector)
            requires
                q <= usize::MAX >> lg,
                lgu == lg as usize,
        ;
        vstd::bits::lemma_usize_shl_is_mul(q, lgu);
        assert((q << lg) == (q << lgu)) by (bit_vector)
            requires
                lgu == lg as usize,
        ;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(rel as int, s, q as int, (rel & m) as int);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(rel as int, s, slab, slot * z);
        // A whole number of slots.
        assert(z > 0);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(slot * z, z, slot, 0);
    }

    /// No block at address 0: with separated regions every slot start of
    /// a class and every page start of the huge region is non-zero, as
    /// each lies at or above its region's base, which
    /// [`separated`](Regions::separated) puts above address 0. So a null
    /// pointer is never a block an allocation can return.
    pub proof fn lemma_block_nonzero(self, c: int, p: usize, len: int, size: int)
        requires
            self.wf(),
            self.separated(),
        ensures
            self.is_slot(c, p) ==> p != 0,
            self.is_huge_block(p, len) ==> p != 0,
            self.user_block(p, size) ==> p != 0,
    {
        if self.is_slot(c, p) {
            self.lemma_slot_in_region(c, p);
        }
        if self.user_block(p, size) && !self.is_huge_block(p, if size < 1 { 1 } else { size }) {
            let c2 = choose|c2: int| #[trigger] self.is_slot(c2, p) && size <= class_table()[c2].size;
            self.lemma_slot_in_region(c2, p);
        }
    }

    /// A slot of class `c` lies, whole, in class `c`'s region.
    pub proof fn lemma_slot_in_region(self, c: int, p: usize)
        requires
            self.wf(),
            self.is_slot_addr(c, p),
        ensures
            self.class_base[c] <= p,
            p + class_table()[c].size <= self.class_base[c] + (1usize << self.lg_region),
    {
        let (slab, slot) = choose|slab: int, slot: int|
            0 <= slab < self.spec_nslabs(c) && 0 <= slot < class_table()[c].nslots
                && p as int == #[trigger] self.slot_at(c, slab, slot);
        crate::vspec::class::axiom_class_table_wf(c);
        let cls = class_table()[c];
        let lg = cls.lg_slab;
        let lr = self.lg_region;
        let rl = 1usize << lr;
        let nu = rl >> lg;
        let lgu = lg as usize;
        assert(nu == rl >> lgu) by (bit_vector)
            requires
                nu == rl >> lg,
                lgu == lg as usize,
        ;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lgu);
        let s = (1usize << lg) as int;
        vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(nu, lgu, rl);
        assert(nu * s <= rl);
        assert(slot * (cls.size as int) + cls.size as int <= s) by (nonlinear_arith)
            requires
                0 <= slot < cls.nslots,
                (cls.nslots as int) * (cls.size as int) <= s,
                cls.size >= 0,
        ;
        assert(slab * s + s <= nu * s) by (nonlinear_arith)
            requires
                0 <= slab < nu,
                s >= 0,
        ;
    }

    /// A slot start lies below `sys::pac::user_va_limit`: inside
    /// its class region, inside its stride of the superblock, which
    /// [`separated`](Regions::separated) puts below it. What lets a slot
    /// pointer round-trip through `pac::sign` and `pac::auth`.
    pub proof fn lemma_slot_below_user_va(self, c: int, p: usize)
        requires
            self.wf(),
            self.separated(),
            self.is_slot(c, p),
        ensures
            (p as int) < crate::sys::pac::user_va_limit(),
    {
        let stride = (1usize << self.lg_stride) as int;
        self.lemma_slot_in_region(c, p);
        crate::vspec::class::axiom_class_table_wf(c);
        assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
            requires
                c < NCLASSES,
                stride >= 0,
        ;
    }

    /// Region separation for one block: a [`user_block`] lies, whole (its
    /// `size` bytes, at least one), in a class region or in the huge region
    /// (never in a gap between class regions),
    /// and neither its start nor any of its `size` bytes is in the
    /// metadata region.
    ///
    /// [`user_block`]: Regions::user_block
    pub proof fn lemma_user_block_separated(self, p: usize, size: int)
        requires
            self.wf(),
            self.separated(),
            self.user_block(p, size),
            size >= 0,
        ensures
            (exists|c: int|
                0 <= c < NCLASSES && #[trigger] self.class_base[c] <= p && p + (if size < 1 { 1 } else { size }) <= self.class_base[c] + (1usize << self.lg_region))
                || (self.huge_base <= p && p + (if size < 1 { 1 } else { size }) <= self.huge_base + self.huge_len),
            !(self.meta_base <= p && p < self.meta_base + self.meta_len),
            p + size <= self.meta_base || self.meta_base + self.meta_len <= p,
    {
        let stride = (1usize << self.lg_stride) as int;
        if exists|c: int| #[trigger] self.is_slot(c, p) && size <= class_table()[c].size {
            let c = choose|c: int| #[trigger] self.is_slot(c, p) && size <= class_table()[c].size;
            self.lemma_slot_in_region(c, p);
            crate::vspec::class::axiom_class_table_wf(c);
            assert(self.superblock + c * stride <= self.class_base[c]);
            assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
                requires
                    c < NCLASSES,
                    stride >= 0,
            ;
            assert(0 <= c * stride) by (nonlinear_arith)
                requires
                    c >= 0,
                    stride >= 0,
            ;
        }
    }

    /// Slab `slab` of class `c`'s region: its `2^lg_slab` bytes.
    pub open spec fn slab_span(self, c: int, slab: int) -> (int, int) {
        let s = (1usize << class_table()[c].lg_slab) as int;
        (self.class_base[c] + slab * s, s)
    }

    /// Guard pages are never handed out, for one block: no byte of a
    /// [`user_block`] (its `size` bytes, at least one) lies in a guard
    /// slab ([`guard_slab`]) of any class. A slot lies inside its own slab,
    /// which [`is_slot`] puts outside the guard slabs, and inside its class
    /// region, which is apart from every other class region and from the
    /// huge region ([`separated`]); a huge block lies in the huge region.
    ///
    /// [`user_block`]: Regions::user_block
    /// [`is_slot`]: Regions::is_slot
    /// [`separated`]: Regions::separated
    pub proof fn lemma_user_block_not_guard(self, p: usize, size: int, c: int, slab: int)
        requires
            self.wf(),
            self.separated(),
            self.user_block(p, size),
            size >= 0,
            0 <= c < NCLASSES,
            0 <= slab < self.spec_nslabs(c),
            guard_slab(c, slab),
        ensures
            disjoint(p as int, if size < 1 { 1 } else { size }, self.slab_span(c, slab).0, self.slab_span(c, slab).1),
    {
        let n = if size < 1 { 1 } else { size };
        let stride = (1usize << self.lg_stride) as int;
        let region = (1usize << self.lg_region) as int;
        let (lo, s) = self.slab_span(c, slab);
        crate::vspec::class::axiom_class_table_wf(c);
        let lg = class_table()[c].lg_slab;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        // The guard slab lies in class `c`'s region.
        let lr = self.lg_region;
        assert(0 <= slab * s && slab * s + s <= region) by {
            let ru = 1usize << lr;
            let nslabs = ru >> lg;
            let lgu = lg as usize;
            assert(nslabs == ru >> lgu) by (bit_vector)
                requires nslabs == ru >> lg, lgu == lg as usize;
            vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(nslabs, lgu, ru);
            assert(nslabs * s <= ru);
            assert(slab * s + s <= nslabs * s && 0 <= slab * s) by (nonlinear_arith)
                requires 0 <= slab < nslabs, s >= 0;
        }
        assert(self.superblock + c * stride <= lo && lo + s <= self.superblock + (c + 1) * stride);
        assert(0 <= c * stride && (c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
            requires 0 <= c < NCLASSES, stride >= 0;
        if exists|c2: int| #[trigger] self.is_slot(c2, p) && size <= class_table()[c2].size {
            let c2 = choose|c2: int| #[trigger] self.is_slot(c2, p) && size <= class_table()[c2].size;
            let (slab2, slot2) = choose|slab2: int, slot2: int|
                0 <= slab2 < self.spec_nslabs(c2) && 0 <= slot2 < class_table()[c2].nslots
                    && p as int == #[trigger] self.slot_at(c2, slab2, slot2);
            crate::vspec::class::axiom_class_table_wf(c2);
            let cls2 = class_table()[c2];
            vstd::bits::lemma_usize_pow2_no_overflow(cls2.lg_slab as nat);
            vstd::bits::lemma_usize_shl_is_mul(1, cls2.lg_slab as usize);
            let s2 = (1usize << cls2.lg_slab) as int;
            // The slot lies inside its slab.
            assert(0 <= slot2 * (cls2.size as int) && slot2 * (cls2.size as int) + cls2.size as int <= s2) by (nonlinear_arith)
                requires
                    0 <= slot2 < cls2.nslots,
                    (cls2.nslots as int) * (cls2.size as int) <= s2,
                    cls2.size > 0,
            ;
            assert(n <= cls2.size);
            let lo2 = self.class_base[c2] + slab2 * s2;
            assert(lo2 <= p && p + n <= lo2 + s2);
            if c2 == c {
                self.lemma_slab_index(c, slab2, slot2, p);
                assert(slab2 != slab);
                if slab < slab2 {
                    assert(slab * s + s <= slab2 * s) by (nonlinear_arith)
                        requires slab + 1 <= slab2, s >= 0;
                } else {
                    assert(slab2 * s + s <= slab * s) by (nonlinear_arith)
                        requires slab2 + 1 <= slab, s >= 0;
                }
            } else {
                self.lemma_slot_in_region(c2, p);
                assert(self.superblock + c2 * stride <= self.class_base[c2]);
                assert(self.class_base[c2] + region <= self.superblock + (c2 + 1) * stride);
                if c2 < c {
                    assert((c2 + 1) * stride <= c * stride) by (nonlinear_arith)
                        requires c2 + 1 <= c, stride >= 0;
                } else {
                    assert((c + 1) * stride <= c2 * stride) by (nonlinear_arith)
                        requires c + 1 <= c2, stride >= 0;
                }
            }
        } else {
            // A huge block lies in the huge region, apart from the
            // superblock.
            assert(self.is_huge_block(p, n));
        }
    }

    /// `[a, a + len)` lies in the metadata region: where the metadata
    /// allocator (`Meta`) hands out every object and table, and so where
    /// the allocator's descriptors, bitmaps, owner words, heaps, cache
    /// stacks and tables live (whole-allocator theorem 4, `verus/TODO.md`).
    pub open spec fn in_meta(self, a: int, len: int) -> bool {
        &&& self.meta_base <= a
        &&& a + len <= self.meta_base + self.meta_len
    }

    /// A block an allocation may return ([`Regions::user_block`]) and
    /// metadata ([`Regions::in_meta`]) share no byte: the block lies in a
    /// class region or the huge region, and both are disjoint from the
    /// metadata region (`separated`).
    pub proof fn lemma_user_block_apart(self, p: usize, size: int, a: int, len: int)
        requires
            self.wf(),
            self.separated(),
            self.user_block(p, size),
            size >= 0,
            len >= 0,
            self.in_meta(a, len),
        ensures
            disjoint(p as int, if size < 1 { 1 } else { size }, a, len),
    {
        let stride = (1usize << self.lg_stride) as int;
        let n = if size < 1 { 1 } else { size };
        self.lemma_user_block_separated(p, size);
        if self.huge_base <= p && p + n <= self.huge_base + self.huge_len {
            assert(disjoint(self.huge_base as int, self.huge_len as int, self.meta_base as int, self.meta_len as int));
        } else {
            let c = choose|c: int|
                0 <= c < NCLASSES && #[trigger] self.class_base[c] <= p && p + n <= self.class_base[c] + (1usize << self.lg_region);
            assert(self.superblock + c * stride <= self.class_base[c]);
            assert(self.class_base[c] + (1usize << self.lg_region) <= self.superblock + (c + 1) * stride);
            assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
                requires
                    c < NCLASSES,
                    stride >= 0,
            ;
            assert(0 <= c * stride) by (nonlinear_arith)
                requires
                    c >= 0,
                    stride >= 0,
            ;
            assert(disjoint(self.superblock as int, NCLASSES * stride, self.meta_base as int, self.meta_len as int));
        }
    }

    /// A block an allocation may return ([`Regions::user_block`]) shares no
    /// byte with a range [`Regions::outside`] the regions, such as the
    /// process's `Shared` static ([`Regions::separated`]): the block lies in
    /// a class region, inside its stride of the superblock, or in the huge
    /// region.
    pub proof fn lemma_user_block_outside(self, p: usize, size: int, a: int, len: int)
        requires
            self.wf(),
            self.separated(),
            self.user_block(p, size),
            size >= 0,
            self.outside(a, len),
        ensures
            disjoint(p as int, if size < 1 { 1 } else { size }, a, len),
    {
        let stride = (1usize << self.lg_stride) as int;
        let n = if size < 1 { 1 } else { size };
        self.lemma_user_block_separated(p, size);
        if !(self.huge_base <= p && p + n <= self.huge_base + self.huge_len) {
            let c = choose|c: int|
                0 <= c < NCLASSES && #[trigger] self.class_base[c] <= p && p + n <= self.class_base[c] + (1usize << self.lg_region);
            assert(self.superblock + c * stride <= self.class_base[c]);
            assert(self.class_base[c] + (1usize << self.lg_region) <= self.superblock + (c + 1) * stride);
            assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
                requires
                    c < NCLASSES,
                    stride >= 0,
            ;
            assert(0 <= c * stride) by (nonlinear_arith)
                requires
                    c >= 0,
                    stride >= 0,
            ;
        }
    }

    /// Alignment of a slot: a slot start of a class whose size is a
    /// multiple of the power of two `align` is itself a multiple of it
    /// (class bases are `MAX_SLAB`-aligned and slabs are powers of two no
    /// smaller than their slots).
    pub proof fn lemma_slot_aligned(self, c: int, p: usize, align: usize)
        requires
            self.separated(),
            self.is_slot(c, p),
            crate::meta::is_pow2(align),
            (class_table()[c].size as int) % (align as int) == 0,
        ensures
            (p as int) % (align as int) == 0,
    {
        self.lemma_slot_aligned_in(c, p, align);
    }

    /// [`Regions::lemma_slot_aligned`] for any slot start of class `c`,
    /// guard slabs included, from the alignment of the class's base alone
    /// (which a class's table keeps, `SlabTable::inv`).
    pub proof fn lemma_slot_aligned_in(self, c: int, p: usize, align: usize)
        requires
            (self.class_base[c] as int) % (MAX_SLAB as int) == 0,
            self.is_slot_addr(c, p),
            crate::meta::is_pow2(align),
            (class_table()[c].size as int) % (align as int) == 0,
        ensures
            (p as int) % (align as int) == 0,
    {
        let (slab, slot) = choose|slab: int, slot: int|
            0 <= slab < self.spec_nslabs(c) && 0 <= slot < class_table()[c].nslots
                && p as int == #[trigger] self.slot_at(c, slab, slot);
        crate::vspec::class::axiom_class_table_wf(c);
        let cls = class_table()[c];
        let lg = cls.lg_slab;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg as usize);
        let s = (1usize << lg) as int;
        assert(cls.size as int <= s) by (nonlinear_arith)
            requires
                cls.nslots >= 1,
                (cls.nslots as int) * (cls.size as int) <= s,
                cls.size >= 0,
        ;
        lemma_pow2_divides_pow2(align, cls.size as int, lg as nat);
        vstd::arithmetic::power2::lemma2_to64();
        assert(MAX_SLAB == 0x40_0000) by (bit_vector);
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(lg as nat, 23);
        lemma_pow2_divides_pow2(align, cls.size as int, 22);
        let m = align as int;
        assert(m > 0) by {
            let k = choose|k: nat| k < 64 && align as nat == #[trigger] pow2(k);
            vstd::arithmetic::power2::lemma_pow2_pos(k);
        }
        // The base is a multiple of `MAX_SLAB`, which is one of `align`.
        let b = self.class_base[c] as int;
        lemma_aligned_down(b, MAX_SLAB as int, m);
        lemma_mod_lin3(b, slab, s, slot, cls.size as int, m);
    }
}

} // verus!

verus! {

/// Append `lo..hi` to `out[..*n]` when it is not empty.
#[inline(always)]
fn push_range(out: &mut [Mapping; MAX_SEALED], n: &mut usize, lo: usize, hi: usize)
    requires
        *old(n) < MAX_SEALED,
    ensures
        *final(n) == if hi > lo { *old(n) + 1 } else { *old(n) as int },
        hi > lo ==> final(out)[*old(n) as int] == (Mapping { addr: lo, len: (hi - lo) as usize }),
        forall|i: int| 0 <= i < *old(n) ==> final(out)[i] == old(out)[i],
{
    if hi > lo {
        out[*n] = Mapping { addr: lo, len: hi - lo };
        *n += 1;
    }
}

impl Regions {
    /// `lo..hi`, non-empty and inside the superblock's mapping, is a seal
    /// target when it starts at or after the end of every class region
    /// below `k` and ends at or before the start of class `k`'s (after
    /// every class when `k == NCLASSES`).
    proof fn lemma_superblock_gap(self, lo: usize, hi: usize, k: int)
        requires
            self.separated(),
            self.mapped(),
            self.superblock_map.addr <= lo < hi,
            hi <= self.superblock_map.addr + self.superblock_map.len,
            0 <= k <= NCLASSES,
            k < NCLASSES ==> hi <= self.class_base[k],
            forall|j: int| 0 <= j < k ==> #[trigger] self.class_base[j] + (1usize << self.lg_region) <= lo,
        ensures
            self.seal_target(Mapping { addr: lo, len: (hi - lo) as usize }),
    {
        reveal(Regions::seal_target);
        let region = (1usize << self.lg_region) as int;
        assert forall|c: int| 0 <= c < NCLASSES implies
            disjoint(#[trigger] self.class_base[c] as int, region, lo as int, hi - lo) by {
            if c > k {
                self.lemma_class_before(k, c);
            }
        }
    }

    /// `lo..hi`, non-empty and inside the huge region's mapping but outside
    /// the region, is a seal target.
    proof fn lemma_huge_slack(self, lo: usize, hi: usize)
        requires
            self.separated(),
            self.mapped(),
            self.huge_map.addr <= lo < hi,
            hi <= self.huge_map.addr + self.huge_map.len,
            hi <= self.huge_base || self.huge_base + self.huge_len <= lo,
        ensures
            self.seal_target(Mapping { addr: lo, len: (hi - lo) as usize }),
    {
        reveal(Regions::seal_target);
        let region = (1usize << self.lg_region) as int;
        assert forall|c: int| 0 <= c < NCLASSES implies
            disjoint(#[trigger] self.class_base[c] as int, region, lo as int, hi - lo) by {
            self.lemma_class_in_strides(c);
        }
    }

    /// `lo..hi`, non-empty and inside the metadata region's mapping but
    /// outside the region, is a seal target.
    proof fn lemma_meta_slack(self, lo: usize, hi: usize)
        requires
            self.separated(),
            self.mapped(),
            self.meta_map.addr <= lo < hi,
            hi <= self.meta_map.addr + self.meta_map.len,
            hi <= self.meta_base || self.meta_base + self.meta_len <= lo,
        ensures
            self.seal_target(Mapping { addr: lo, len: (hi - lo) as usize }),
    {
        reveal(Regions::seal_target);
        let region = (1usize << self.lg_region) as int;
        assert forall|c: int| 0 <= c < NCLASSES implies
            disjoint(#[trigger] self.class_base[c] as int, region, lo as int, hi - lo) by {
            self.lemma_class_in_strides(c);
        }
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
    /// mapping. Of separated regions inside their mappings
    /// ([`Regions::separated`], [`Regions::mapped`]), every range returned
    /// is a [`Regions::seal_target`]: non-empty, inside a reservation and
    /// apart from every region and the `Shared` static.
    pub fn never_committed(&self) -> (r: ([Mapping; MAX_SEALED], usize))
        requires
            self.wf(),
        ensures
            r.1 <= MAX_SEALED,
            self.separated() && self.mapped() ==> forall|i: int| 0 <= i < r.1 ==> self.seal_target(#[trigger] r.0[i]),
    {
        let ghost ok = self.separated() && self.mapped();
        let ghost stride = (1usize << self.lg_stride) as int;
        let mut out = [Mapping { addr: 0, len: 0 }; MAX_SEALED];
        let mut n = 0;
        let mut start = self.superblock_map.addr;
        for c in 0..NCLASSES
            invariant
                self.wf(),
                n <= c,
                ok == (self.separated() && self.mapped()),
                stride == (1usize << self.lg_stride) as int,
                ok ==> self.superblock_map.addr <= start,
                ok ==> start <= self.superblock + c * stride,
                ok ==> forall|j: int| 0 <= j < c ==> #[trigger] self.class_base[j] + (1usize << self.lg_region) <= start,
                ok ==> forall|i: int| 0 <= i < n ==> self.seal_target(#[trigger] out[i]),
        {
            proof {
                if ok {
                    assert(self.superblock + c * stride <= self.class_base[c as int]);
                    if start < self.class_base[c as int] {
                        self.lemma_class_in_strides(c as int);
                        self.lemma_superblock_gap(start, self.class_base[c as int], c as int);
                    }
                }
            }
            push_range(&mut out, &mut n, start, self.class_base[c]);
            start = self.class_base[c] + self.region_len();
            proof {
                if ok {
                    let cc = c as int;
                    assert(self.class_base[cc] + (1usize << self.lg_region) <= self.superblock + (cc + 1) * stride);
                    assert forall|j: int| 0 <= j < cc + 1 implies #[trigger] self.class_base[j] + (1usize << self.lg_region) <= start by {
                        if j < cc {
                            self.lemma_class_before(j, cc);
                        }
                    }
                }
            }
        }
        proof {
            if ok {
                vstd::arithmetic::mul::lemma_mul_nonnegative(NCLASSES as int, stride);
                if start < self.superblock_map.addr + self.superblock_map.len {
                    self.lemma_superblock_gap(start, (self.superblock_map.addr + self.superblock_map.len) as usize, NCLASSES as int);
                }
            }
        }
        push_range(&mut out, &mut n, start, self.superblock_map.addr + self.superblock_map.len);
        proof {
            if ok && self.huge_map.addr < self.huge_base {
                self.lemma_huge_slack(self.huge_map.addr, self.huge_base);
            }
        }
        push_range(&mut out, &mut n, self.huge_map.addr, self.huge_base);
        proof {
            if ok && self.huge_base + self.huge_len < self.huge_map.addr + self.huge_map.len {
                self.lemma_huge_slack((self.huge_base + self.huge_len) as usize, (self.huge_map.addr + self.huge_map.len) as usize);
            }
        }
        push_range(&mut out, &mut n, self.huge_base + self.huge_len, self.huge_map.addr + self.huge_map.len);
        proof {
            if ok && self.meta_map.addr < self.meta_base {
                self.lemma_meta_slack(self.meta_map.addr, self.meta_base);
            }
        }
        push_range(&mut out, &mut n, self.meta_map.addr, self.meta_base);
        proof {
            if ok && self.meta_base + self.meta_len < self.meta_map.addr + self.meta_map.len {
                self.lemma_meta_slack((self.meta_base + self.meta_len) as usize, (self.meta_map.addr + self.meta_map.len) as usize);
            }
        }
        push_range(&mut out, &mut n, self.meta_base + self.meta_len, self.meta_map.addr + self.meta_map.len);
        (out, n)
    }
}

} // verus!
mod linux {
    use super::{
        LG_HUGE_MIN, LG_HUGE_PREFERRED, LG_MAX_SLAB, LG_META_MIN, LG_META_PREFERRED, LG_REGION_MIN,
        LG_REGION_PREFERRED, LG_SLIDE, MAX_SLAB, Mapping, NCLASSES, Regions,
    };
    #[cfg(verus_only)]
    use super::{MAX_SEALED, disjoint, shared_static_addr, shared_static_len};
    #[cfg(verus_only)]
    use crate::sys::pac::user_va_limit;
    use crate::sys::{self, Errno, MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
    use crate::vspec::unbacked::Unbacked;
    #[cfg(verus_only)]
    use vstd::arithmetic::power2::pow2;
    use vstd::prelude::*;

    verus! {

    /// Ghost ownership of one live reservation, `len()` bytes from
    /// `addr()`, that [`try_reserve`] obtained from the kernel. Tokens are
    /// never duplicated, so two of them are two live mappings, which the
    /// kernel keeps apart ([`Reserved::is_disjoint`]). The reservations
    /// are never unmapped (`Regions::release` is test-only), so a token
    /// stays true for the life of the process.
    #[verifier::external_body]
    pub tracked struct Reserved {
        _no_copy: (),
    }

    impl Reserved {
        pub uninterp spec fn addr(&self) -> usize;

        pub uninterp spec fn len(&self) -> usize;

        /// Trusted (the kernel's `mmap` contract): a live reservation does
        /// not overlap the process's `Shared` static
        /// ([`shared_static_addr`]). The static is in the writable data of
        /// the object challoc is linked into, which the loader mapped before
        /// any allocator code ran and which stays mapped for the life of the
        /// process; `mmap` without `MAP_FIXED` never places a mapping over a
        /// live one.
        #[verifier::external_body]
        pub proof fn is_apart_from_static(tracked &self)
            requires
                self.len() > 0,
            ensures
                disjoint(self.addr() as int, self.len() as int, shared_static_addr(), shared_static_len()),
        {
            unimplemented!()
        }

        /// Trusted (the kernel's `mmap` contract): two live mappings do not
        /// overlap. `mmap` without `MAP_FIXED` never places a mapping over
        /// a live one, and nothing unmaps a reservation. Both are
        /// non-empty: [`disjoint`] of an empty range inside another is
        /// false (`mmap` of 0 bytes fails, but [`try_reserve`]'s contract
        /// does not say so), and likewise for
        /// [`Reserved::is_apart_from_static`].
        #[verifier::external_body]
        pub proof fn is_disjoint(tracked &mut self, tracked other: &Self)
            requires
                old(self).len() > 0,
                other.len() > 0,
            ensures
                *final(self) == *old(self),
                disjoint(old(self).addr() as int, old(self).len() as int, other.addr() as int, other.len() as int),
        {
            unimplemented!()
        }

        /// The reservation as uncommitted address space this process owns,
        /// for the one owner that commits it piece by piece (`Meta`, for the
        /// metadata region). Trusted (the kernel's `mmap` contract): the range
        /// is this process's alone. The token is consumed, so the range is
        /// handed out once. Verified code has no other source of an
        /// `Unbacked` token (`vspec::unbacked` keeps its fields private;
        /// the test-only `Meta::new` assumes one).
        #[verifier::external_body]
        pub proof fn into_unbacked(tracked self) -> (tracked u: Unbacked)
            ensures
                u.addr() == self.addr(),
                u.len() == self.len(),
        {
            unimplemented!()
        }
    }

    /// Reserve address space with `PROT_NONE`, `MAP_NORESERVE`: on
    /// success, the address and the token for the `len` bytes there.
    ///
    /// Trusted (the kernel's `mmap` contract): a successful anonymous
    /// mapping without `MAP_FIXED` is a new mapping of `len` bytes (the
    /// token, apart from every other live one) that ends in user space,
    /// below `sys::pac::user_va_limit` (the kernel's default map window,
    /// at most `2^48`), and never at address 0: the kernel places a
    /// mapping without `MAP_FIXED` at or above `max(PAGE_SIZE,
    /// mmap_min_addr)`.
    #[verifier::external_body]
    fn try_reserve(len: usize) -> (r: Result<(usize, Tracked<Reserved>), Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r matches Ok((a, t)) ==> t@.addr() == a && t@.len() == len && 0 < a && a + len <= user_va_limit(),
    {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        match unsafe { sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0) } {
            Ok(addr) => Ok((addr, Tracked::assume_new())),
            Err(e) => Err(e),
        }
    }

    /// A reservation [`try_reserve`] made: `m` with its token, below
    /// `sys::pac::user_va_limit`.
    pub open spec fn reserved(m: Mapping, t: Reserved) -> bool {
        &&& t.addr() == m.addr
        &&& t.len() == m.len
        &&& m.addr > 0
        &&& m.addr + m.len <= user_va_limit()
    }

    /// The abort of [`reserve_shrinking`] when even `2^lg_min` bytes
    /// cannot be reserved.
    #[cold]
    #[inline(never)]
    fn cannot_reserve(what: &str, lg: u32, e: Errno) -> !
        requires
            crate::sys::may_abort(),
    {
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        sys::abort_fmt(format_args!("cannot reserve {what} region of 2^{lg} bytes: {e}"))
    }

    /// Reserve `2^lg + extra` bytes, halving `lg` down to `lg_min` on any
    /// failure. Returns the mapping and the `lg` that fit.
    fn reserve_shrinking(what: &str, mut lg: u32, lg_min: u32, extra: usize) -> (r: (Mapping, u32, Tracked<Reserved>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            lg_min <= lg <= 40,
            extra <= 0x8000_0000,
        ensures
            lg_min <= r.1 <= 40,
            r.0.len == (1usize << r.1) + extra,
            reserved(r.0, r.2@),
    {
        let ghost lg0 = lg;
        loop
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                lg_min <= lg <= lg0 <= 40,
                extra <= 0x8000_0000,
            decreases lg,
        {
            proof {
                assert((1usize << lg) <= 0x100_0000_0000) by (bit_vector)
                    requires lg <= 40;
            }
            let len = (1usize << lg) + extra;
            match try_reserve(len) {
                Ok((addr, t)) => return (Mapping { addr, len }, lg, t),
                Err(e) if lg > lg_min => {
                    let _ = e;
                    lg -= 1;
                }
                Err(e) => cannot_reserve(what, lg, e),
            }
        }
    }

    /// The abort of [`reserve_superblock`] when even the smallest layout
    /// cannot be reserved.
    #[cold]
    #[inline(never)]
    fn cannot_reserve_superblock(lg_stride: u32, e: Errno) -> !
        requires
            crate::sys::may_abort(),
    {
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        sys::abort_fmt(format_args!(
            "cannot reserve class superblock of {NCLASSES} x 2^{lg_stride} bytes: {e}"
        ))
    }

    /// Reserve the class superblock: `NCLASSES` strides plus alignment slack.
    /// Prefers a stride of 4x the region (gap 3x). Shrinks the region down to
    /// the minimum, then as a last resort drops the stride to 2x the region
    /// (gap 1x, below the design's 1.25x preference but still a full gap).
    fn reserve_superblock() -> (r: (Mapping, u32, u32, Tracked<Reserved>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
        ensures
            LG_REGION_MIN <= r.1 <= LG_REGION_PREFERRED,
            r.1 < r.2 <= r.1 + 2,
            r.0.len == (NCLASSES << r.2) + MAX_SLAB,
            reserved(r.0, r.3@),
    {
        let mut lg_region = LG_REGION_PREFERRED;
        let mut lg_stride = lg_region + 2;
        loop
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                LG_REGION_MIN <= lg_region <= LG_REGION_PREFERRED,
                lg_region < lg_stride <= lg_region + 2,
            decreases lg_region + lg_stride,
        {
            proof {
                let n = NCLASSES;
                assert(n <= 64);
                assert((n << lg_stride) + 0x40_0000 <= 0x100_0000_0000_0000) by (bit_vector)
                    requires n <= 64, lg_stride <= 40;
                assert(MAX_SLAB == 0x40_0000) by (bit_vector);
            }
            let len = (NCLASSES << lg_stride) + MAX_SLAB;
            match try_reserve(len) {
                Ok((addr, t)) => return (Mapping { addr, len }, lg_region, lg_stride, t),
                Err(_) if lg_region > LG_REGION_MIN => {
                    lg_region -= 1;
                    lg_stride = lg_region + 2;
                }
                Err(_) if lg_stride == lg_region + 2 => lg_stride = lg_region + 1,
                Err(e) => cannot_reserve_superblock(lg_stride, e),
            }
        }
    }

    const SLIDE_STEPS: usize = (1usize << LG_SLIDE) >> LG_MAX_SLAB;

    /// The reservations' VMA names, NUL-terminated (spelled as bytes so
    /// the verifier sees the NUL `set_vma_anon_name` requires).
    const CLASSES_NAME: [u8; 16] = [b'c', b'h', b'a', b'l', b'l', b'o', b'c', b':', b'c', b'l', b'a', b's', b's', b'e', b's', 0];
    const HUGE_NAME: [u8; 13] = [b'c', b'h', b'a', b'l', b'l', b'o', b'c', b':', b'h', b'u', b'g', b'e', 0];
    const META_NAME: [u8; 13] = [b'c', b'h', b'a', b'l', b'l', b'o', b'c', b':', b'm', b'e', b't', b'a', 0];

    fn name_mapping(m: Mapping, name: &'static [u8])
        requires
            crate::sys::may_syscall(),
            name@.len() > 0,
            name@.last() == 0,
    {
        // SAFETY: `m` is a live mapping we own; the name is NUL-terminated.
        let _ = unsafe { sys::set_vma_anon_name(m.addr, m.len, name) };
    }

    /// `addr` rounded up to a multiple of `MAX_SLAB`: `mmap` only promises
    /// page alignment, so each mapping carries `MAX_SLAB` of slack for it.
    #[inline(always)]
    fn align_base(addr: usize) -> (r: usize)
        requires
            addr + MAX_SLAB <= usize::MAX,
        ensures
            addr <= r < addr + MAX_SLAB,
            r & ((MAX_SLAB - 1) as usize) == 0,
    {
        proof {
            assert(MAX_SLAB == 0x40_0000) by (bit_vector);
            let r = ((addr + 0x3f_ffff) as usize) & !0x3f_ffffusize;
            assert(addr <= r < addr + 0x40_0000 && r & 0x3f_ffff == 0) by (bit_vector)
                requires addr + 0x40_0000 <= usize::MAX, r == ((addr + 0x3f_ffff) as usize) & !0x3f_ffffusize;
        }
        (addr + MAX_SLAB - 1) & !(MAX_SLAB - 1)
    }

    /// Two multiples of `MAX_SLAB` add to one.
    proof fn lemma_add_aligned(a: usize, b: usize)
        requires
            a + b <= usize::MAX,
            a & 0x3f_ffff == 0,
            b & 0x3f_ffff == 0,
        ensures
            ((a + b) as usize) & 0x3f_ffff == 0,
    {
        assert(((a + b) as usize) & 0x3f_ffff == 0) by (bit_vector)
            requires a + b <= usize::MAX, a & 0x3f_ffff == 0, b & 0x3f_ffff == 0;
    }

    /// A multiple of `MAX_SLAB` below `steps` of them, picked by `w`:
    /// `w % steps` slabs.
    #[inline(always)]
    fn slab_multiple(w: usize, steps: usize) -> (r: usize)
        requires
            0 < steps <= 0x4_0000,
        ensures
            r & 0x3f_ffff == 0,
            r + 0x40_0000 <= steps << 22,
    {
        let m = w % steps;
        proof {
            assert((m << 22) & 0x3f_ffff == 0 && (m << 22) + 0x40_0000 <= steps << 22) by (bit_vector)
                requires m < steps, steps <= 0x4_0000;
        }
        m << LG_MAX_SLAB
    }

    /// Word `i` of the random bytes (`8 * i` on, the target's byte order).
    /// Trusted: `copy_from_slice` and `from_ne_bytes`; any word will do.
    #[verifier::external_body]
    #[inline(always)]
    fn word(rnd: &[u8; 8 * (NCLASSES + 3)], i: usize) -> u64
        requires
            i < NCLASSES + 3,
    {
        let mut b = [0u8; 8];
        b.copy_from_slice(&rnd[8 * i..8 * i + 8]);
        u64::from_ne_bytes(b)
    }

    /// The abort of [`Regions::reserve_owning`] when `getrandom` fails.
    #[cold]
    #[inline(never)]
    fn getrandom_failed(e: Errno) -> !
        requires
            crate::sys::may_abort(),
    {
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        sys::abort_fmt(format_args!("getrandom failed: {e}"))
    }

    /// `thp:never` on a host whose global mode is `always` (`thp.rs`):
    /// the class and metadata reservations are `MADV_NOHUGEPAGE`d before
    /// anything in them is committed; the huge region follows the host's
    /// policy (`thp::Reservation`), so `thp.rs` skips it by kind.
    #[inline(always)]
    fn advise_reservations(sb_map: Mapping, huge_map: Mapping, meta_map: Mapping)
        requires
            crate::sys::may_syscall(),
    {
        use crate::thp::{Reservation, advise_reservation};
        advise_reservation(Reservation::Classes, sb_map.addr, sb_map.len);
        advise_reservation(Reservation::Huge, huge_map.addr, huge_map.len);
        advise_reservation(Reservation::Metadata, meta_map.addr, meta_map.len);
    }

    /// The abort of [`Regions::reserve_owning`] for a page size that is not a
    /// power of two from 4 KiB to `MAX_SLAB`, with the message of the
    /// `assert!` it replaced.
    #[cold]
    #[inline(never)]
    fn bad_page_size() -> !
        requires
            crate::sys::may_abort(),
    {
        sys::abort_fmt(format_args!(
            "assertion failed: page_size.is_power_of_two() && (0x1000..=MAX_SLAB).contains(&page_size)"
        ))
    }

    /// `1 << k` has `k` trailing zeros.
    proof fn lemma_tz_pow2(x: usize, k: nat)
        requires
            k < 64,
            x as nat == pow2(k),
        ensures
            vstd::std_specs::bits::u64_trailing_zeros(x as u64) == k,
            x == 1usize << (k as usize),
    {
        broadcast use vstd::std_specs::bits::axiom_u64_trailing_zeros;
        vstd::bits::lemma_usize_pow2_no_overflow(k);
        vstd::bits::lemma_usize_shl_is_mul(1, k as usize);
        let kk = k as u64;
        let y = x as u64;
        let t = vstd::std_specs::bits::u64_trailing_zeros(y) as u64;
        assert(y == 1u64 << kk) by (bit_vector)
            requires x == 1usize << kk, y == x as u64, kk < 64;
        if t < kk {
            assert((y >> t) & 1u64 == 0u64) by (bit_vector)
                requires y == 1u64 << kk, t < kk, kk < 64;
        } else if t > kk {
            assert((y >> kk) & 1u64 == 1u64) by (bit_vector)
                requires y == 1u64 << kk, kk < 64;
        }
    }

    impl Regions {
        /// [`Regions::reserve_owning`] without the metadata region's address
        /// space, for the tests.
        #[cfg(test)]
        #[inline(always)]
        pub fn reserve(page_size: usize, anon_name: bool) -> (r: Regions)
            ensures
                r.wf(),
                r.separated(),
                r.ready(),
                r.mapped(),
        {
            Regions::reserve_owning(page_size, anon_name).0
        }

        /// Probe and reserve every region (design section 10 step 2). Never
        /// allocates. Aborts if even the minimum layout cannot be reserved.
        /// `anon_name` should be the probed `PR_SET_VMA_ANON_NAME` support.
        /// The result is well formed, separated and inside its three
        /// reservations ([`Regions::wf`], [`Regions::separated`],
        /// [`Regions::mapped`]), from the kernel's `mmap` contract
        /// ([`try_reserve`]), and ready for `Shared::init` ([`Regions::ready`]),
        /// with the metadata region's uncommitted address space, for
        /// `Meta::owning`, which commits it as it hands it out.
        /// The page size must be the kernel's (`api::init` passes the probed
        /// one): a power of two from 4 KiB, the smallest page of any Linux
        /// target, to `MAX_SLAB`; anything else aborts.
        // The index carries a Verus invariant.
        #[allow(clippy::needless_range_loop)]
        pub fn reserve_owning(page_size: usize, anon_name: bool) -> (res: (Regions, Tracked<Unbacked>))
            requires
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
            ensures
                res.0.wf(),
                res.0.separated(),
                res.0.ready(),
                res.0.mapped(),
                res.1@.addr() == res.0.meta_base,
                res.1@.len() == res.0.meta_len,
        {
            if !(page_size.is_power_of_two() && (0x1000..=MAX_SLAB).contains(&page_size)) {
                bad_page_size();
            }
            let lg_page = page_size.trailing_zeros();
            proof {
                crate::sys::pac::axiom_user_va_limit();
                let k = choose|k: nat| k < 64 && page_size as nat == #[trigger] pow2(k);
                lemma_tz_pow2(page_size, k);
                let lg = lg_page;
                assert(MAX_SLAB == 0x40_0000) by (bit_vector);
                assert(12 <= lg <= 22) by (bit_vector)
                    requires page_size == 1usize << lg, 0x1000 <= page_size <= 0x40_0000, lg < 64;
            }

            // One 64-bit random per class offset, one each for the two
            // slides, one for the reservation order.
            let mut rnd = [0u8; 8 * (NCLASSES + 3)];
            if let Err(e) = sys::getrandom_exact(&mut rnd) {
                getrandom_failed(e);
            }

            // Reserve the three mappings in a random order so their relative
            // placement is not fixed across processes.
            let order: [u8; 3] = match word(&rnd, NCLASSES + 2) % 6 {
                0 => [0, 1, 2],
                1 => [0, 2, 1],
                2 => [1, 0, 2],
                3 => [1, 2, 0],
                4 => [2, 0, 1],
                _ => [2, 1, 0],
            };
            let slide = 1usize << LG_SLIDE;
            let mut sb: Option<(Mapping, u32, u32, Tracked<Reserved>)> = None;
            let mut huge: Option<(Mapping, u32, Tracked<Reserved>)> = None;
            let mut meta: Option<(Mapping, u32, Tracked<Reserved>)> = None;
            proof {
                assert(slide == 0x4000_0000) by (bit_vector)
                    requires slide == 1usize << 30u32;
                assert(MAX_SLAB == 0x40_0000) by (bit_vector);
            }
            let mut i = 0;
            while i < 3
                invariant
                    crate::sys::may_abort(),
                    crate::sys::may_syscall(),
                    0 <= i <= 3,
                    slide == 0x4000_0000,
                    MAX_SLAB == 0x40_0000,
                    forall|j: int| 0 <= j < i ==> {
                        &&& (order[j] == 0 ==> sb is Some)
                        &&& (order[j] == 1 ==> huge is Some)
                        &&& (order[j] != 0 && order[j] != 1 ==> meta is Some)
                    },
                    sb matches Some(s) ==> {
                        &&& LG_REGION_MIN <= s.1 <= LG_REGION_PREFERRED
                        &&& s.1 < s.2 <= s.1 + 2
                        &&& s.0.len == (NCLASSES << s.2) + MAX_SLAB
                        &&& reserved(s.0, s.3@)
                    },
                    huge matches Some(h) ==> {
                        &&& LG_HUGE_MIN <= h.1 <= 40
                        &&& h.0.len == (1usize << h.1) + slide + MAX_SLAB
                        &&& reserved(h.0, h.2@)
                    },
                    meta matches Some(m) ==> {
                        &&& LG_META_MIN <= m.1 <= 40
                        &&& m.0.len == (1usize << m.1) + slide + MAX_SLAB
                        &&& reserved(m.0, m.2@)
                    },
                decreases 3 - i,
            {
                match order[i] {
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
                i += 1;
            }
            let (sb_map, lg_region, lg_stride, _sb_tok) = sb.unwrap();
            let (huge_map, lg_huge, _huge_tok) = huge.unwrap();
            let (meta_map, lg_meta, _meta_tok) = meta.unwrap();
            proof {
                assert(lg_region < lg_stride <= lg_region + 2);
                assert(LG_REGION_MIN <= lg_region <= LG_REGION_PREFERRED);
                assert(SLIDE_STEPS == 256) by (bit_vector)
                    requires SLIDE_STEPS == (1usize << 30u32) >> 22u32;
                let tracked mut a = _sb_tok.get();
                let tracked mut b = _huge_tok.get();
                a.is_disjoint(&b);
                a.is_disjoint(_meta_tok.borrow());
                b.is_disjoint(_meta_tok.borrow());
                a.is_apart_from_static();
                b.is_apart_from_static();
                _meta_tok.borrow().is_apart_from_static();
            }

            let superblock = align_base(sb_map.addr);
            proof {
                let (lr, ls) = (lg_region, lg_stride);
                assert((1usize << ls) > (1usize << lr)) by (bit_vector)
                    requires 32 <= lr < ls <= 40;
            }
            let gap = (1usize << lg_stride) - (1usize << lg_region);
            proof {
                let (lr, ls) = (lg_region, lg_stride);
                assert(gap >> 22 > 0 && gap & 0x3f_ffff == 0 && gap <= 0x100_0000_0000 && (gap >> 22) <= 0x4_0000) by (bit_vector)
                    requires gap == (1usize << ls) - (1usize << lr), 32 <= lr < ls <= 40;
            }
            let mut offsets = [0usize; NCLASSES];
            for c in 0..NCLASSES
                invariant
                    gap >> 22 > 0,
                    gap & 0x3f_ffff == 0,
                    gap <= 0x100_0000_0000,
                    (gap >> 22) <= 0x4_0000,
                    gap == (1usize << lg_stride) - (1usize << lg_region),
                    forall|j: int| 0 <= j < c ==> (#[trigger] offsets[j]) & 0x3f_ffff == 0 && offsets[j] < gap,
            {
                offsets[c] = slab_multiple(word(&rnd, c) as usize, gap >> LG_MAX_SLAB);
                proof {
                    let (o, g) = (offsets[c as int], gap);
                    assert(o < g) by (bit_vector)
                        requires o + 0x40_0000 <= (g >> 22) << 22, g & 0x3f_ffff == 0;
                }
            }
            let huge_align = align_base(huge_map.addr);
            let huge_slide = slab_multiple(word(&rnd, NCLASSES) as usize, SLIDE_STEPS);
            let meta_align = align_base(meta_map.addr);
            let meta_slide = slab_multiple(word(&rnd, NCLASSES + 1) as usize, SLIDE_STEPS);
            proof {
                let n = NCLASSES;
                let ls = lg_stride;
                assert((n << ls) <= 0x100_0000_0000_0000) by (bit_vector)
                    requires n <= 64, ls <= 40;
                assert(SLIDE_STEPS << 22 == 0x4000_0000) by (bit_vector)
                    requires SLIDE_STEPS == 256;
                let (lh, lm) = (lg_huge, lg_meta);
                assert((1usize << lh) <= 0x100_0000_0000 && (1usize << lm) <= 0x100_0000_0000) by (bit_vector)
                    requires lh <= 40, lm <= 40;
                lemma_add_aligned(huge_align, huge_slide);
                lemma_add_aligned(meta_align, meta_slide);
            }
            let huge_base = huge_align + huge_slide;
            let meta_base = meta_align + meta_slide;

            if anon_name {
                name_mapping(sb_map, &CLASSES_NAME);
                name_mapping(huge_map, &HUGE_NAME);
                name_mapping(meta_map, &META_NAME);
            }
            advise_reservations(sb_map, huge_map, meta_map);
            proof {
                // `Regions::ready`: the page, the metadata base and the huge
                // region's page count.
                let (mb, lp, lm, lh) = (meta_base, lg_page, lg_meta, lg_huge);
                assert(mb % (1usize << lp) == 0 && (1usize << lm) % (1usize << lp) == 0 && (1usize << lm) > 0 && (1usize << lp) >= 16
                    && ((1usize << lh) >> lp) > 0 && ((1usize << lh) >> lp) <= 0xffff_ffff) by (bit_vector)
                    requires mb & 0x3f_ffff == 0, 12 <= lp <= 22, 30 <= lm <= 40, 34 <= lh <= 40;
            }

            let r = Regions::from_parts(
                superblock,
                lg_region,
                lg_stride,
                &offsets,
                (huge_base, 1usize << lg_huge),
                (meta_base, 1usize << lg_meta),
                lg_page,
                [sb_map, huge_map, meta_map],
            );
            // The metadata region's address space, for `Meta`, which commits
            // it as it hands it out.
            let tracked meta_space = _meta_tok.get().into_unbacked().split(meta_base - meta_map.addr).1.split((1usize << lg_meta) as int).0;
            (r, Tracked(meta_space))
        }
    }

    } // verus!

    verus! {

    impl Regions {
        /// Seal every range [`Regions::never_committed`] lists (design 10
        /// step 5): `mseal` forbids `munmap`, `mremap`, `mprotect` and
        /// `MAP_FIXED` over them for the rest of the process, so nothing can
        /// land a mapping in the gap between two class regions or unmap
        /// part of a reservation. Only for kernels whose probe said
        /// `mseal=yes`; each call splits the enclosing VMA, so the VMA count
        /// grows by at most `2 * MAX_SEALED`. Every range it hands `mseal`
        /// is a [`Regions::seal_target`] (inside a reservation, apart from
        /// every region and the `Shared` static), from the regions being
        /// separated and inside their mappings, which
        /// [`Regions::reserve_owning`] ensures. Returns the number of ranges
        /// sealed. A refusal after a successful probe (`ENOMEM` at the VMA
        /// limit, for instance) stops the sweep and leaves the rest
        /// unsealed rather than aborting: sealing is a hardening extra, not
        /// a heap invariant, and the count is reported by
        /// `malloc_stats_print`.
        pub fn seal(&self) -> u32
            requires
                crate::sys::may_syscall(),
                self.wf(),
                self.separated(),
                self.mapped(),
        {
            let (ranges, n) = self.never_committed();
            let mut sealed: u32 = 0;
            for m in it: &ranges[..n]
                invariant
                    crate::sys::may_syscall(),
                    sealed as int <= it.index(),
                    it.seq().len() == n,
                    n <= MAX_SEALED,
                    forall|i: int| 0 <= i < it.seq().len() ==> self.seal_target(*#[trigger] it.seq()[i]),
            {
                // Whole-allocator theorem "Seal targets stay inside
                // challoc's reservations" (`verus/THEOREMS.md`): what
                // `mseal` gets is inside a reservation and apart from
                // every region and the `Shared` static.
                assert(self.seal_target(*m));
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
    }

    } // verus!

    impl Regions {
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

    /// Kani check of `word` (record under `verus/seams/`).
    #[cfg(kani)]
    mod kani_word {
        use super::{NCLASSES, word};

        /// S176 `word`, `requires i < NCLASSES + 3`, no `ensures`: under
        /// it the call returns, for every byte string and index (and the
        /// word is bytes `8 * i..8 * i + 8` in native order).
        #[kani::proof]
        fn word_returns() {
            let rnd: [u8; 8 * (NCLASSES + 3)] = kani::any();
            let i: usize = kani::any();
            kani::assume(i < NCLASSES + 3);
            let w = word(&rnd, i);
            let mut b = [0u8; 8];
            b.copy_from_slice(&rnd[8 * i..8 * i + 8]);
            assert_eq!(w, u64::from_ne_bytes(b));
        }
    }
}

verus! {

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
///
/// Trusted, like [`super::ClassTable::table`]: the entry is `CLASSES[c]`,
/// well formed and sized [`super::spec_class_size`].
// PORT-RESTORE: new upstream accessor (asm barrier); trusted entry read, same claim as ClassTable::table.
#[verifier::external_body]
#[inline(always)]
unsafe fn class_entry(c: usize) -> (r: &'static super::Class)
    requires
        c < NCLASSES,
    ensures
        *r == class_table()[c as int],
        r.wf(),
        r.size == super::spec_class_size(c as int),
    opens_invariants none
{
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

} // verus!
