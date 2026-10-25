//! [`Shared`], the process-wide allocator state every heap works against,
//! and the checks it performs for the fast and slow paths; the thread-exit
//! key and the thread id, clock and seed a heap reads, and the per-class
//! [`ClassLine`] the `free` fast paths read, with the sized path's
//! check.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::arena::{Arena, Arenas};
use crate::class::SlabTable;
use crate::config::Config;
use crate::huge::{Huge, RunState};
use crate::layout::{CLASSES, Kind, NCLASSES, Regions};
use crate::lock::Lock;
use crate::meta::Meta;

use super::heap_destructor;
use super::tcaches::Tcaches;
use super::{FREE_ROUTE, Heap, PUSH_PAC, PUSH_SCAN, PUSH_ZERO, invalid_free};

/// The cache GC period (design 6.4): 10 s.
pub const GC_INTERVAL_NS: u64 = 10_000_000_000;

/// Process-wide allocator state that every heap works against: the regions,
/// one [`SlabTable`] per class, arena 0, the metadata allocator, the huge
/// allocator, the arena and explicit-cache registries and the thread-exit
/// key. Built in place by [`Shared::init`] because the class states borrow
/// `meta`, `regions` and `tables`.
///
/// `repr(C)` fixes the order: the per-class lines (read by every `free`),
/// then `free_flags` and `regions`, read by every `malloc` and `free`,
/// then the line-aligned tables and arena 0's class states, so the locks
/// inside `meta` and `huge` (taken on heap creation and huge allocations)
/// never share a cache line with them.
#[repr(C)]
pub struct Shared {
    /// What the `free` and sized `free` fast paths read per class, one
    /// line each ([`ClassLine`]). At offset 0: the fast paths hold a
    /// class's line as a reference, and LLVM builds its address as
    /// `Shared + class * 64` plus the array's offset, one more `add`
    /// whenever that offset is not zero.
    pub(super) lines: [ClassLine; NCLASSES],
    /// The one byte the `free` fast path loads for its dispatch:
    /// [`push_flags`](Shared::push_flags) plus [`FREE_ROUTE`], set once a
    /// second arena exists. Until then every slab and every heap is arena
    /// 0 and the `free` fast path skips the owner-word load; with
    /// `FREE_ROUTE` in the byte, the default configuration's test against
    /// zero sends `free` to the arena test, so a single-arena `free` pays
    /// no separate load and branch for it. Relaxed: a slot of arena `a > 0`
    /// can only be freed after it was allocated, which happened after `a`
    /// was built and this bit stored, and the program itself orders that
    /// allocation before the free; so any thread freeing such a slot
    /// already sees the bit. The sized path and `malloc` read
    /// `push_flags`, which never changes after init.
    ///
    /// Right before `regions`, so it shares a cache line with the
    /// `Regions` fields that `set_slot` reads on every `free`
    /// (`superblock`, `lg_stride`): after `Regions` it would sit on its own
    /// line, and larson's working set turns every extra line into an L2
    /// hit.
    pub(super) free_flags: AtomicU8,
    /// The opt-in hardening a push applies, as [`PUSH_SCAN`]
    /// (`double_free_scan`, design 6.3 step 3: every push compares the
    /// slot against the top four cache entries and aborts on a match) and
    /// [`PUSH_ZERO`] (`zero_on_free`: the slot's class-size bytes are
    /// cleared before the push, and a huge run's usable bytes before its
    /// release), [`PUSH_PAC`] (`pac_pointers`: signed cache and heap
    /// pointers where the CPU has pointer authentication; read by the pop
    /// too). Written once by `init`; kept on the line every `free` reads,
    /// and zero in the default configuration on every host (all three
    /// options default to off), so the fast path tests one byte and takes
    /// one predicted branch.
    pub(super) push_flags: u8,
    /// `!(regions.region_len() - 1)`: the bits of an offset from a class
    /// base that put it past the class region, read by the sized check
    /// ([`ClassLine::sized_ok`]) on the line `push_flags` is on.
    pub(super) region_hi: usize,
    pub regions: Regions,
    tables: [SlabTable; NCLASSES],
    pub arena0: Arena,
    pub meta: Meta,
    pub huge: Huge,
    /// Cache GC period, [`GC_INTERVAL_NS`] (the tests shorten it).
    pub gc_interval_ns: u64,
    /// The cgroup policy's outcome (design 11, `policy.rs`): the band,
    /// the keep ratio, the pending full purge. Next to `gc_interval_ns`
    /// so the periodic tick's `immediate` load hits the line it reads
    /// anyway; written at most once per 10 ms by the sampler.
    pub policy: crate::policy::Policy,
    /// The parsed `malloc_conf` (design section 12) as `init` applied it
    /// (`narenas` resolved from the CPU count, `thp` forced to `never`
    /// under a small cgroup limit). Read back by `opt.*` and the report,
    /// and on the slow paths for the cache ladder (`cache_cap`).
    pub config: Config,
    pub(super) key: ThreadKey,
    pub arenas: Arenas,
    pub tcaches: Tcaches,
    /// Process-wide statistics as of the last `epoch` (`stats.rs`).
    pub stats: Lock<crate::stats::Snapshot>,
    /// The memory cgroup as last sampled (design 11; `sys::cgroup`),
    /// discovered after init unless init needs it (`Deferred`): read it
    /// through [`Shared::cgroup`] or [`Shared::cgroup_now`]. Cold: written
    /// by discovery and by the rate-limited sampler on slow paths, read by
    /// `stats.cgroup.*` and the reports. Last, well away from the lines the
    /// fast paths read.
    cgroup: crate::sys::cgroup::Deferred,
}

/// Everything the `free` fast path reads for one class, in one 64-byte
/// line: the slot geometry (`Regions::class_base[c]` and `CLASSES[c]`'s
/// slot-start test), the bitmap and owner array of [`SlabTable`] and a
/// copy of its hand-out watermark. `[ClassLine; NCLASSES]` has a
/// power-of-two stride equal to the heap's cache headers', so the class
/// index scaled once addresses both, and every field is one load at an
/// immediate offset from it; before, `free` indexed three arrays of
/// different strides (`class_base`, the 32-byte `CLASSES`, the 384-byte
/// tables, one `lea`/`shl` or `madd` each) and read three lines. The 52
/// lines are consecutive, so they fill distinct L1 sets.
///
/// The copy of the watermark is stored after the table's own, under its
/// commit lock ([`SlabTable::mirror_watermark`]), so it is never ahead of
/// it: a slab below the copy has its bitmap words and owner word, and the
/// relaxed load is enough for the reason [`SlabTable::desc_if_committed`]
/// gives. A copy that lags the table only sends a `free` into a slab
/// just handed out to the slow path, which repeats the check against the
/// table itself.
#[repr(C, align(64))]
pub struct ClassLine {
    class_base: usize,
    bits_base: usize,
    committed: AtomicUsize,
    owners_base: usize,
    slab_bits: u16,
    lg_slab: u8,
    lg_group: u8,
    /// Right before `inv`, so that `free` loads it and `inv`'s low half
    /// with one `ldp` on aarch64.
    slab_mask: u32,
    /// The inverse modulo `2^64` of the odd part of the class size, the
    /// multiplier of the sized check ([`ClassLine::sized_ok`]); its low
    /// half is [`Class::inv_odd`], the multiplier of the `free` path's
    /// 32-bit slot-start test.
    ///
    /// [`Class::inv_odd`]: crate::layout::Class::inv_odd
    inv: u64,
    /// Slots per slab, as a `u64` so that the sized check compares its
    /// 64-bit quotient against the field in memory; the `free` path reads
    /// the low half.
    nslots: u64,
    /// The last eight bytes are the sized check's guard word
    /// ([`ClassLine::guard_word`]), one load: `guard_mask` is
    /// [`SlabTable::guard_mask`] over page groups of `2^lg_guard` bytes,
    /// `tz` the slot-start test's rotate.
    guard_mask: u32,
    tz: u8,
    /// `log2` of a page group in bytes: `lg_slab` plus
    /// `log2(slabs_per_page)`.
    lg_guard: u8,
    /// Zero: initialised, so that the guard word's load reads no padding.
    _pad: [u8; 2],
}

const _: () = assert!(core::mem::size_of::<ClassLine>() == crate::layout::CLASS_STRIDE);
const _: () = assert!(core::mem::offset_of!(Shared, lines) == 0);

impl ClassLine {
    fn of(regions: &Regions, table: &SlabTable, class: usize) -> ClassLine {
        let cls = &CLASSES[class];
        ClassLine {
            class_base: regions.class_base[class],
            bits_base: table.bits_base(),
            committed: AtomicUsize::new(0),
            owners_base: table.owners_base(),
            inv: inverse_of_odd_part_u64(cls.size()),
            nslots: cls.nslots as u64,
            slab_mask: cls.slab_mask,
            guard_mask: table.guard_mask(),
            slab_bits: cls.slab_bits,
            tz: cls.tz,
            lg_slab: cls.lg_slab,
            lg_group: table.lg_group(),
            lg_guard: cls.lg_slab + table.slabs_per_page().trailing_zeros() as u8,
            _pad: [0; 2],
        }
    }

    /// Tests: the line's fields as `(class_base, bits_base, watermark copy,
    /// owners_base, lg_group)`.
    #[cfg(test)]
    pub(super) fn parts(&self) -> (usize, usize, usize, usize, u8) {
        (
            self.class_base,
            self.bits_base,
            self.committed.load(Ordering::Relaxed),
            self.owners_base,
            self.lg_group,
        )
    }

    /// The slot-start test ([`Class::slot_of_offset`]) on `rel`, the
    /// offset from the region base: the slot number, or `None` for an
    /// address that is not a slot start of its slab.
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn slot_start(&self, rel: usize) -> Option<u32> {
        let slot = ((rel as u32) & self.slab_mask)
            .wrapping_mul(self.inv as u32)
            .rotate_right(self.tz as u32);
        if slot < self.nslots as u32 {
            Some(slot)
        } else {
            None
        }
    }

    /// The sized free's check on this class's line: whether `p`, at `rel`
    /// from the region base, is inside the region and a slot start in a
    /// slab that is not a guard slab. `region_hi` is
    /// [`Shared::region_hi`], the offset bits at and above the region
    /// length.
    ///
    /// The region bound and the slot-start test are one 64-bit test: `x`
    /// keeps the in-slab offset `rel & slab_mask` plus the bits of `rel`
    /// past the region, and `q = (x * inv).rotate_right(tz)` is below
    /// `nslots` exactly when `x` is a multiple of the size and `x / size <
    /// nslots` (the exact-division test of [`Class::slot_of_offset`], on 64
    /// bits). An offset inside the region leaves `x` the in-slab offset, so
    /// that is the slot-start test; an offset past it leaves `x >= 2^32`,
    /// more than `nslots * size` (a slab is at most `MAX_SLAB`), so `q` is
    /// at least `nslots` whether or not `x` is a multiple. The guard test
    /// is [`ClassLine::is_guard`] on the guard word. The whole check
    /// accepts exactly what the reference form does
    /// ([`Regions::slot_in_class`] and [`SlabTable::is_guard_slab`];
    /// `sized_slot_agrees_with_the_reference`).
    ///
    /// The shape is about loads. Each one waits on the size (the class
    /// indexes the line) or the pointer, and in a program whose frees take
    /// both from a cache miss (a Valkey `SET` reads the old value's pointer
    /// and size from the key's entry) they all wait in the load queue and
    /// the scheduler until the miss returns. On Zen 4 (Genoa) that
    /// capacity, more than the instruction count, sets how far the core
    /// runs ahead to the next miss: `valkey1` is 8% faster with 13 such
    /// loads in `sdallocx` than with 16 (#46). So the bound has no load of
    /// its own, and the guard mask and both shift counts are one load.
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn sized_ok(&self, rel: usize, region_hi: usize) -> bool {
        let gw = self.guard_word();
        let x = (rel & (self.slab_mask as usize | region_hi)) as u64;
        // `rotate_right` takes its count modulo 64: `tz`, the low byte.
        let q = x.wrapping_mul(self.inv).rotate_right((gw >> 32) as u32);
        q < self.nslots && !Self::is_guard(rel, gw)
    }

    /// `guard_mask | tz << 32 | lg_guard << 40` (the padding above is
    /// zero), from one 8-byte load.
    #[inline(always)]
    fn guard_word(&self) -> u64 {
        // SAFETY: the eight initialised bytes from `guard_mask` to the end
        // of `self`, 8-aligned (asserted below).
        #[allow(unused_mut)]
        let mut gw = unsafe {
            ptr::read(
                (self as *const ClassLine as *const u8)
                    .add(core::mem::offset_of!(ClassLine, guard_mask))
                    as *const u64,
            )
        };
        // SAFETY: an empty template; `gw` comes out unchanged. Without it
        // LLVM splits the load back into a byte load per shift count.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) gw, options(pure, nomem, nostack, preserves_flags))
        };
        gw
    }

    /// [`SlabTable::is_guard_slab`] on the offset `rel` from the region
    /// base, from a guard word: `rel >> lg_guard` is the page group
    /// (`slab >> lg_slabs_per_page`), a guard when `(group + 1) &
    /// guard_mask == 0`. With guards off the mask is all ones and only
    /// group `u32::MAX` would match, which is past every region; an offset
    /// past the region is rejected by the bound in [`ClassLine::sized_ok`]
    /// whatever this says. `guard_test_agrees_with_the_tables` checks it
    /// against the table for every class.
    #[inline(always)]
    fn is_guard(rel: usize, gw: u64) -> bool {
        // `wrapping_shr` takes its count modulo 64: `lg_guard`.
        (rel.wrapping_shr((gw >> 40) as u32) as u32).wrapping_add(1) & (gw as u32) == 0
    }

    /// Tests: the sized check's 64-bit multiplier.
    #[cfg(test)]
    pub(super) fn inv(&self) -> u64 {
        self.inv
    }

    /// Tests: [`ClassLine::is_guard`] on this line.
    #[cfg(test)]
    pub(super) fn is_guard_at(&self, rel: usize) -> bool {
        Self::is_guard(rel, self.guard_word())
    }

    /// The `free` check on this class's line: `Some(slab)` when `p` is a
    /// slot start in a slab below the watermark whose bit is set, `None`
    /// otherwise. The slot-start test is [`Class::slot_of_offset`]; the
    /// watermark compare is also the region bound (an address past the
    /// region, in the gap after it or below its base yields a slab number
    /// of at least `nslabs`, which no watermark reaches).
    ///
    /// [`Class::slot_of_offset`]: crate::layout::Class::slot_of_offset
    #[inline(always)]
    fn set_slab(&self, p: usize) -> Option<usize> {
        // On aarch64 every field is read up front, before the first
        // branch: read after it, LLVM rebuilt the line's address (one more
        // `add`), and up front the two compares fold into a `ccmp`. On
        // x86_64 that order costs callee-saved registers (`push` and `pop`
        // of `rbx`, and `r14`), so there each is read just before its use.
        #[cfg(target_arch = "aarch64")]
        let (lg_slab, slab_bits, bits_base, committed) = self.bit_fields();
        let rel = p.wrapping_sub(self.class_base);
        let slot = self.slot_start(rel)?;
        #[cfg(not(target_arch = "aarch64"))]
        let lg_slab = self.lg_slab;
        let slab = rel >> lg_slab;
        #[cfg(not(target_arch = "aarch64"))]
        let committed = self.committed.load(Ordering::Relaxed);
        if slab >= committed {
            return None;
        }
        #[cfg(not(target_arch = "aarch64"))]
        let (slab_bits, bits_base) = (self.slab_bits, self.bits_base);
        let bit = slab * slab_bits as usize + slot as usize;
        #[allow(unused_mut)]
        let mut index = bit / 64;
        // As in `SlabTable::is_set_unchecked`: on aarch64 `bit >> 6` and a
        // scaled `ldr`, not LLVM's `(bit >> 3) & !7`.
        #[cfg(target_arch = "aarch64")]
        // SAFETY: an empty template; `index` comes out unchanged.
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) index, options(pure, nomem, nostack, preserves_flags))
        };
        let word = (bits_base + index * 8) as *const AtomicU64;
        // SAFETY: `slab` is below a value the watermark held and `slot`
        // below `nslots`, so the word is in the committed bitmap.
        if (unsafe { (*word).load(Ordering::Relaxed) } >> (bit % 64)) & 1 == 0 {
            return None;
        }
        Some(slab)
    }

    /// What [`set_slab`](ClassLine::set_slab) reads past the slot-start
    /// test: `lg_slab`, `slab_bits`, `bits_base` and the watermark copy.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn bit_fields(&self) -> (u8, u16, usize, usize) {
        (
            self.lg_slab,
            self.slab_bits,
            self.bits_base,
            self.committed.load(Ordering::Relaxed),
        )
    }

    /// The arena that owns `slab`'s group.
    ///
    /// # Safety
    /// `slab` is one [`set_slab`](ClassLine::set_slab) returned.
    #[inline(always)]
    unsafe fn owner(&self, slab: usize) -> u16 {
        let word = (self.owners_base + (slab >> self.lg_group) * 2) as *const AtomicU16;
        // SAFETY: as `SlabTable::owner_at`.
        unsafe { (*word).load(Ordering::Relaxed) }
    }
}

const _: () = assert!(
    core::mem::offset_of!(Shared, free_flags) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, superblock))
            / 128
);
const _: () = assert!(
    core::mem::offset_of!(Shared, free_flags) / 128
        == (core::mem::offset_of!(Shared, regions) + core::mem::offset_of!(Regions, lg_stride))
            / 128
);

/// Makes the next [`ThreadKey::set`] on the thread with this kernel id
/// fail as `pthread_setspecific` would, for tests.
#[cfg(test)]
pub static FAIL_SETSPECIFIC: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The `pthread_key` whose destructor tears down a thread's heap.
pub(super) struct ThreadKey {
    key: libc::pthread_key_t,
    valid: bool,
}

impl ThreadKey {
    pub fn create() -> ThreadKey {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: `key` is a valid out-pointer; the destructor is a plain
        // `extern "C"` function that only touches allocator state.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(heap_destructor)) };
        ThreadKey {
            key,
            valid: rc == 0,
        }
    }

    #[cfg(test)]
    pub fn valid(&self) -> bool {
        self.valid
    }

    #[cfg(test)]
    pub fn key(&self) -> Option<libc::pthread_key_t> {
        if self.valid { Some(self.key) } else { None }
    }

    /// Register `heap` for destruction when the calling thread exits.
    /// `false` if `pthread_setspecific` fails, which for a live key
    /// happens only past glibc's first 32 keys, when it cannot allocate
    /// the second-level block; `true` without registering when the key
    /// does not exist.
    pub fn set(&self, heap: *mut Heap) -> bool {
        if !self.valid {
            return true;
        }
        #[cfg(test)]
        if FAIL_SETSPECIFIC
            .compare_exchange(tid(), 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return false;
        }
        // SAFETY: the key was created by `create`; glibc stores the
        // pointer without dereferencing it.
        unsafe { libc::pthread_setspecific(self.key, heap as *const c_void) == 0 }
    }
}

pub(super) fn tid() -> u32 {
    crate::sys::gettid() as u32
}

pub(super) fn ncpus() -> u32 {
    crate::sys::ncpus()
}

pub(super) fn now_ns() -> u64 {
    crate::sys::now_coarse_ns()
}

/// Eight random bytes from `getrandom(2)` for `slot_randomize`; on a
/// kernel that refuses (not seen on any supported one) the clock, so
/// the heap is still seeded with something that varies.
pub(super) fn seed() -> u64 {
    let mut b = [0u8; 8];
    match crate::sys::getrandom_exact(&mut b) {
        Ok(()) => u64::from_ne_bytes(b),
        Err(_) => crate::sys::now_coarse_ns(),
    }
}

/// Guard slabs are placed once per `interval` slabs of 64 KiB, so a class
/// with smaller slabs (`layout::LG_MIN_SLAB`) gets one guard per
/// `interval * (64 KiB / slab)` slabs (in page groups when the page is
/// larger than the slab, see `SlabTable::is_guard_slab`): the same one
/// guard page per `interval * 64 KiB` of address space and the same
/// `madvise` count per byte committed for every class. 0 stays 0. The
/// ratio is a power of two (slab sizes and pages are), so a power-of-two
/// `interval` (what `config.rs` guarantees) stays one, as the mask test
/// needs.
pub(crate) fn guard_interval_for(class: usize, page: usize, interval: u32) -> u32 {
    let unit = (1usize << crate::layout::LG_GUARD_UNIT).max(page);
    let group = CLASSES[class].slab_size().max(page);
    if group >= unit {
        interval
    } else {
        interval.saturating_mul((unit / group) as u32)
    }
}

impl Shared {
    /// Initialise `this` in place for already reserved `regions`: the
    /// metadata allocator over the metadata reservation, one slab table
    /// per class with `guard_interval` (0 when the kernel has no guard
    /// regions), arena 0, the huge allocator (`guard_install` is the probed
    /// `MADV_GUARD_INSTALL` support), the registries and the thread-exit
    /// key. Returns `false` if the metadata region cannot hold the tables,
    /// leaving `this` unusable.
    ///
    /// # Safety
    /// `this` must point to writable storage for a `Shared` that nothing
    /// else reads until this returns `true`, and the regions must be
    /// reserved (`PROT_NONE` mappings that this process owns).
    /// The `Shared` must never move afterwards: arena 0's class states
    /// point at the tables and the registry points at arena 0.
    ///
    /// `config` supplies the guard slab interval, the huge-run guards, the
    /// cache GC period, the cache capacities, the decay period and the
    /// number of automatic arenas (`narenas:0`, the default, is resolved
    /// here from the CPU count and stored, so `opt.narenas` reports the
    /// number in use); `guard_install` says whether the kernel
    /// has `MADV_GUARD_INSTALL`, without which no guard is placed at all
    /// (the `PROT_NONE` fallback would split every class region into one
    /// VMA per guard, see `class/`).
    pub unsafe fn init(
        this: *mut Shared,
        regions: Regions,
        config: Config,
        guard_install: bool,
        pac: bool,
    ) -> bool {
        let mut config = config;
        config.narenas = config.effective_narenas(ncpus());
        let guard_interval = if guard_install {
            config.effective_guard_slab_interval()
        } else {
            0
        };
        let guard_install = guard_install && config.guards;
        let decay_ns = crate::purge::decay_ns_from_ms(config.dirty_decay_ms);
        // SAFETY: the caller guarantees `this` is writable and unaliased.
        unsafe {
            let meta = &raw mut (*this).meta;
            ptr::write(
                meta,
                Meta::new(regions.meta_base, regions.meta_len, regions.page_size()),
            );
            ptr::write(&raw mut (*this).regions, regions);
            let meta = &*meta;
            let push_flags = (if config.double_free_scan {
                PUSH_SCAN
            } else {
                0
            }) | (if config.zero_on_free { PUSH_ZERO } else { 0 })
                | (if pac && config.pac_pointers {
                    PUSH_PAC
                } else {
                    0
                });
            ptr::write(&raw mut (*this).push_flags, push_flags);
            ptr::write(&raw mut (*this).region_hi, !(regions.region_len() - 1));
            ptr::write(&raw mut (*this).free_flags, AtomicU8::new(push_flags));
            let regions = &*(&raw const (*this).regions);
            let tables = &raw mut (*this).tables;
            for c in 0..NCLASSES {
                match SlabTable::new(
                    meta,
                    regions,
                    c,
                    guard_interval_for(c, regions.page_size(), guard_interval),
                ) {
                    Some(t) => ptr::write(&raw mut (*tables)[c], t),
                    None => return false,
                }
            }
            let lines = &raw mut (*this).lines;
            for c in 0..NCLASSES {
                ptr::write(
                    &raw mut (*lines)[c],
                    ClassLine::of(regions, &(*tables)[c], c),
                );
                // The `Shared` never moves (this function's contract).
                (*tables)[c].mirror_watermark(&raw const (*lines)[c].committed);
            }
            let arena0 = &raw mut (*this).arena0;
            if !Arena::init(arena0, meta, regions, &*tables, 0, decay_ns) {
                return false;
            }
            match Huge::new(meta, regions, guard_install) {
                Some(h) => {
                    h.set_retain_max(config.huge_retain_max);
                    // `huge_dirty_decay_ms:0`: a freed run is purged at
                    // once rather than at the next decay pass (design 7).
                    h.set_purge_on_free(config.huge_dirty_decay_ms == Some(0));
                    ptr::write(&raw mut (*this).huge, h)
                }
                None => return false,
            }
            ptr::write(&raw mut (*this).gc_interval_ns, GC_INTERVAL_NS);
            Arenas::init(&raw mut (*this).arenas, arena0, config.narenas);
            Tcaches::init(&raw mut (*this).tcaches);
            // Design 11: discovery waits for the first tick `DEFER_NS` into
            // the process's life (or the first read of the figures), so a
            // short-lived process never reads the files; `thp:always` needs
            // the limit now, and `cgroup:false` reads nothing either way.
            use crate::sys::cgroup::{Cgroup, Deferred};
            ptr::write(
                &raw mut (*this).cgroup,
                if config.cgroup && config.thp == crate::config::Thp::Always {
                    Deferred::now(Cgroup::discover(true))
                } else if config.cgroup {
                    Deferred::later(crate::sys::now_coarse_ns())
                } else {
                    Deferred::now(Cgroup::discover(false))
                },
            );
            // Design 11: under an effective limit below 1 GiB a configured
            // `thp:always` is forced to `never` in the stored configuration
            // (`auto` is left alone; the report says so); the policy's
            // `high` baseline is the count at init. A deferred discovery
            // leaves no limit, no rises and the `Unlimited` band for now.
            let cgroup = (*(&raw const (*this).cgroup)).get();
            let limit = cgroup.map_or(crate::sys::cgroup::UNLIMITED, |c| c.limit().min(c.high()));
            let rises = cgroup.map_or(0, |c| c.rises().0);
            let band = cgroup.map_or(crate::policy::Band::Unlimited, crate::policy::cgroup_band);
            let thp_forced = crate::thp::forced_by_limit(config.thp, limit);
            if thp_forced {
                config.thp = crate::config::Thp::Never;
            }
            ptr::write(
                &raw mut (*this).policy,
                crate::policy::Policy::new(rises, thp_forced),
            );
            // Applied once on the discovery sample, so the band and the cap
            // are published before the first 10 ms window closes (nothing
            // is `Empty` yet: retention 0).
            (*this).policy.apply(band, limit, 0, rises);
            ptr::write(&raw mut (*this).config, config);
            ptr::write(&raw mut (*this).key, ThreadKey::create());
            ptr::write(
                &raw mut (*this).stats,
                Lock::new(crate::stats::Snapshot::EMPTY),
            );
        }
        true
    }

    /// The memory cgroup record, `None` until discovery has run
    /// (`sys::cgroup::Deferred`). For the samplers and the policy.
    #[inline]
    pub fn cgroup(&self) -> Option<&crate::sys::cgroup::Cgroup> {
        self.cgroup.get()
    }

    /// The memory cgroup record, discovering it first if that has not
    /// happened: what a reader of the figures (`stats.cgroup.*`, the
    /// report) uses.
    pub fn cgroup_now(&self) -> &crate::sys::cgroup::Cgroup {
        self.discover_cgroup(true);
        self.cgroup
            .get()
            .unwrap_or_else(|| crate::sys::abort("cgroup: discovery did not publish"))
    }

    /// Discover the cgroup if the deferral has run out at `now` (the
    /// periodic tick, the background thread): `true` when this call did,
    /// and the policy has been evaluated on the first figures.
    pub fn discover_cgroup_if_due(&self, now: u64) -> bool {
        self.cgroup.due(now) && self.discover_cgroup(false)
    }

    /// Discover the cgroup in `fork`'s prepare handler, so the child
    /// inherits the record; without waiting for a discovery another
    /// thread (or this one, under a signal handler) has under way, which
    /// the child then starts over (`Deferred::fork_child`).
    pub fn discover_cgroup_for_fork(&self) {
        self.discover_cgroup(false);
    }

    /// Run the deferred discovery (`Deferred::discover`): `true` when this
    /// call ran it. The `memory.events` `high` count found is the policy's
    /// baseline, set before the record is published (as at init: a
    /// container that already had `high` events does not start with a
    /// full purge, whichever thread samples first); then the band from
    /// the first sample.
    fn discover_cgroup(&self, wait: bool) -> bool {
        let ran = self.cgroup.discover(wait, |cg| {
            self.policy.rebase_high_rises(cg.rises().0);
        });
        if ran {
            crate::policy::evaluate(self);
        }
        ran
    }

    /// The child of a `fork`: see `sys::cgroup::Deferred::fork_child`.
    pub fn cgroup_fork_child(&self) {
        self.cgroup.fork_child();
    }

    /// The per-class slab tables (shared by every arena).
    #[inline(always)]
    pub fn tables(&self) -> &[SlabTable; NCLASSES] {
        &self.tables
    }

    /// Slab table for `class`, without the bounds check the fast paths do
    /// not need (`class` comes from `class_of`).
    #[inline(always)]
    pub fn table(&self, class: usize) -> &SlabTable {
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of` only yields indices below `NCLASSES`.
        unsafe { self.tables.get_unchecked(class) }
    }

    /// Arena `index` if it exists (automatic arenas are built on first
    /// touch; see `Arenas::get`).
    #[inline]
    pub fn arena(&self, index: u16) -> Option<&Arena> {
        if index == 0 {
            return Some(&self.arena0);
        }
        self.arenas.get(self, index)
    }

    /// Arena `index`, which must exist: the index came from a slab or run
    /// descriptor, an owner word or a cache header, so it was built before
    /// that memory was handed out. Never builds one: an index no arena was
    /// built for is a corrupt word (design 6.3 step 4), and aborts here
    /// instead of quietly materialising an automatic arena to release the
    /// slot through.
    #[inline]
    pub(super) fn owner(&self, index: u16) -> &Arena {
        if index == 0 {
            return &self.arena0;
        }
        match self.arenas.get_existing(index) {
            Some(a) => a,
            None => no_such_arena(index),
        }
    }

    /// Whether more than one arena exists (see the field).
    #[cfg(test)]
    pub fn multi_arena(&self) -> bool {
        self.free_flags() & FREE_ROUTE != 0
    }

    /// The `free` fast path's dispatch byte (see the field): `push_flags`,
    /// plus [`FREE_ROUTE`] once a second arena exists.
    #[inline(always)]
    pub fn free_flags(&self) -> u8 {
        self.free_flags.load(Ordering::Relaxed)
    }

    /// The flag byte both fast paths load: [`PUSH_SCAN`], [`PUSH_ZERO`]
    /// and [`PUSH_PAC`]; 0 in the default configuration, PAC host or not.
    #[inline(always)]
    pub fn push_flags(&self) -> u8 {
        self.push_flags
    }

    /// Whether cache and heap pointers are signed ([`PUSH_PAC`]).
    #[inline(always)]
    pub fn pac(&self) -> bool {
        self.push_flags & PUSH_PAC != 0
    }

    /// Whether pushes compare against the top four cache entries
    /// (`double_free_scan`).
    #[inline(always)]
    pub fn double_free_scan(&self) -> bool {
        self.push_flags & PUSH_SCAN != 0
    }

    /// Whether a freed block is cleared before it is cached or released
    /// (`zero_on_free`).
    #[inline(always)]
    pub fn zero_on_free(&self) -> bool {
        self.push_flags & PUSH_ZERO != 0
    }

    /// A second arena is being built (`Arenas::build`).
    pub fn set_multi_arena(&self) {
        self.free_flags.fetch_or(FREE_ROUTE, Ordering::Relaxed);
    }

    /// The cache GC period the periodic tick uses: `gc_interval_ns`,
    /// or 0 (GC every tick) while the cgroup policy is in its immediate
    /// band (design 11).
    #[inline]
    pub fn gc_interval(&self) -> u64 {
        if self.policy.immediate() {
            0
        } else {
            self.gc_interval_ns
        }
    }

    /// The decay period new arenas start with (`dirty_decay_ms`).
    pub fn default_decay_ns(&self) -> u64 {
        crate::purge::decay_ns_from_ms(self.config.dirty_decay_ms)
    }

    /// Arena 0's decay period in nanoseconds (`purge::NEVER` when purging
    /// is off), which the huge allocator shares unless
    /// `huge_dirty_decay_ms` gives it its own ([`Shared::huge_decay_ns`]).
    #[inline]
    pub fn decay_ns(&self) -> u64 {
        self.arena0.decay_ns()
    }

    /// The huge allocator's decay period in nanoseconds (design 7): its
    /// own when `huge_dirty_decay_ms` is set (fixed at init; `-1` is
    /// `purge::NEVER`), else arena 0's live period ([`Shared::decay_ns`],
    /// which `arena.0.dirty_decay_ms` moves). The cgroup policy scales
    /// whichever it is (`purge::decay`).
    pub fn huge_decay_ns(&self) -> u64 {
        match self.config.huge_dirty_decay_ms {
            Some(ms) => crate::purge::decay_ns_from_ms(ms),
            None => self.decay_ns(),
        }
    }

    /// Change the decay period of every existing arena; `-1` turns purging
    /// off. Takes effect at the next periodic tick, or at once for a
    /// background thread (each arena setter wakes it).
    /// `arena.<i>.dirty_decay_ms` goes through `Arena::set_decay_ms` instead.
    pub fn set_decay_ms(&self, ms: i64) {
        for a in self.arenas.iter() {
            a.set_decay_ms(ms);
        }
    }

    /// Whether the thread-exit key exists (heaps of exiting threads are
    /// reclaimed). False only if `pthread_key_create` failed at init.
    #[cfg(test)]
    pub fn has_thread_key(&self) -> bool {
        self.key.valid()
    }

    /// The thread-exit key, for diagnostics and tests.
    #[cfg(test)]
    pub fn thread_key(&self) -> Option<libc::pthread_key_t> {
        self.key.key()
    }

    /// Claim one slot of `class` in `arena` straight from the slab bitmaps,
    /// bypassing every cache. Null when the region is exhausted. The flag
    /// says the memory is known zero.
    pub fn alloc_direct(&self, arena: &Arena, class: usize) -> (*mut u8, bool) {
        let mut out = [ptr::null_mut()];
        let c = arena.class(class).claim(&self.meta, &mut out);
        if c.n == 0 {
            (ptr::null_mut(), false)
        } else {
            (out[0], c.zeroed)
        }
    }

    /// Release one slot straight to its slab bitmap through its owning
    /// arena. Aborts unless `p` is an allocated slot of `class` in `arena`.
    pub fn free_direct(&self, arena: u16, class: usize, p: *mut u8) {
        self.owner(arena).class(class).release(&[p]);
    }

    /// Classify `p` for `free`: `Ok((class, arena))` for an allocated slab
    /// slot (the bitmap check happens here), `Err(page)` for a page-aligned
    /// pointer into the huge region (the huge allocator checks it). Aborts
    /// on anything else.
    #[inline(always)]
    pub fn classify_free(&self, p: *mut u8) -> Result<(usize, u16), usize> {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, slab, slot } => {
                let class = class as usize;
                let arena =
                    self.table(class)
                        .check_located(slab as usize, slot as usize, p as usize);
                Ok((class, arena))
            }
            Kind::Huge { page } => Err(page),
            Kind::Invalid => invalid_free(p as usize),
        }
    }

    /// The sized free's check (`sdallocx`, `free_sized`; design 6.3): `p`
    /// must be a slot start in a non-guard slab of the class the size hint
    /// names, `hint` (`class_for(size, align)`, `None` for a huge size).
    /// Arithmetic on the pointer and the class's [`ClassLine`]: the hint
    /// names the region, so the pointer is checked against that region
    /// alone (no superblock classification and no class compare; the
    /// reference form is [`Regions::slot_in_class`]), as one
    /// [`ClassLine::sized_ok`]: the region bound folded into the
    /// slot-start test, and the guard test. Neither the slab
    /// descriptor nor the class table line is touched: the bit test that
    /// makes plain `free` exact is left to the flush, and so is the
    /// watermark. A slot start in a slab no arena has been handed, or in
    /// a handed-out slab its arena has not committed (the cursor sits
    /// under the class lock), is caught at the flush, which aborts by name
    /// (`SlabTable::owner` on a slab past the watermark, `release_owned`
    /// on a `Fresh` one); until then the slot can be popped by `malloc`,
    /// and a program that writes to it faults on the reserved `PROT_NONE`
    /// page instead of aborting by name (the trade design 6.3 records).
    /// `None` for anything else; the slow path repeats the checks with
    /// aborts ([`check_size_hint`], then [`classify_free`]). The arena is
    /// not read either (it is on the descriptor line): the home cache
    /// takes the slot and the flush routes it ([`release_routed`]).
    ///
    /// [`check_size_hint`]: Shared::check_size_hint
    /// [`classify_free`]: Shared::classify_free
    /// [`release_routed`]: Shared::release_routed
    #[inline(always)]
    pub(super) fn sized_slot(&self, p: *mut u8, hint: Option<usize>) -> Option<usize> {
        let class = hint?;
        debug_assert!(class < NCLASSES);
        // SAFETY: `class_of`/`class_for` only produce `class < NCLASSES`.
        let line = unsafe { self.lines.get_unchecked(class) };
        let rel = (p as usize).wrapping_sub(line.class_base);
        if !line.sized_ok(rel, self.region_hi) {
            return None;
        }
        Some(class)
    }

    /// The sized free's abort: a slab pointer whose class is not the one
    /// the size hint names (`sdallocx`'s contract is `nallocx(size, flags)
    /// == sallocx(p)`), or the start of an active huge run whose page count
    /// is not the one `size` rounds to (any size whose page count is the
    /// run's passes, which also lets `free_aligned_sized` name the small
    /// size of a run an alignment made huge). Pointers that are not
    /// allocations pass here and are judged by the exact free that follows.
    #[cold]
    #[inline(never)]
    pub fn check_size_hint(&self, p: *mut u8, size: usize, hint: Option<usize>) {
        match self.regions.classify(p as usize) {
            Kind::Slab { class, .. } => {
                if hint != Some(class as usize) {
                    panic!(
                        "sized free of {:#x}: size hint {} does not match the allocation (class {}, {} bytes)",
                        p as usize,
                        size,
                        class,
                        CLASSES[class as usize].size()
                    );
                }
            }
            Kind::Huge { page } => {
                let Some(run) = self.huge.lookup(page) else {
                    return;
                };
                if run.start == page
                    && run.state == RunState::Active
                    && self.huge.usable_for(size) != Some(run.usable)
                {
                    panic!(
                        "sized free of {:#x}: size hint {} does not match the allocation (huge run, {} bytes)",
                        p as usize, size, run.usable
                    );
                }
            }
            _ => {}
        }
    }

    /// Owning arena of the handed-out slab holding the slot `p` of `class`.
    /// Aborts for anything that is not a slot start in such a slab.
    pub(super) fn arena_of_slot(&self, class: usize, p: *mut u8) -> u16 {
        let t = self.table(class);
        let (slab, _) = t.locate(p);
        t.owner(slab)
    }

    /// Release a cache's sorted batch of `class` slots, starting with
    /// `arena`. A home cache's batch is normally one [`release_owned`]
    /// call with no extra work per pointer; the sized path's home cache
    /// and the one foreign cache per class (`heap::FOREIGN`, design 6.3
    /// step 4) hold slots of any arena, and each run of slabs of another
    /// arena is released through that arena (one owner-word load per
    /// switch, a word `release_owned` reads anyway).
    ///
    /// [`release_owned`]: crate::class::ClassState::release_owned
    pub fn release_routed(&self, arena: u16, class: usize, ptrs: &[*mut u8]) {
        let mut done = 0;
        let mut a = arena;
        loop {
            done += self.owner(a).class(class).release_owned(&ptrs[done..]);
            if done == ptrs.len() {
                return;
            }
            let next = self.arena_of_slot(class, ptrs[done]);
            if next == a {
                // `release_owned` stopped at a slab it says is not ours while
                // the descriptor says it is: the descriptor changed under us.
                panic!("heap: slab of class {class} changed arena {a} during a flush");
            }
            a = next;
        }
    }

    /// The `free` fast path's check: `Some(class)` when `p` is a slot start
    /// in a handed-out slab whose bit is set and whose arena is `home` (the
    /// arena load is skipped while only arena 0 exists), `None` for
    /// anything else. It never aborts; [`classify_free`] on the slow path
    /// repeats the checks and does. Every read is bounds-free: the
    /// class index is checked against `NCLASSES` and every per-class field
    /// comes from its [`ClassLine`], the bitmap word from a
    /// slot already known to be below `nslots` in a slab below the
    /// watermark, whose compare is the region bound too (no separate
    /// `region_len` test). The fast path itself
    /// (`heap::free_fast_class`) makes the same two tests from
    /// [`set_slot`] and [`home_owns`], the arena test selected by the
    /// `free_flags` byte it loads anyway; this form serves the tests.
    ///
    /// The two loads that depend on the pointer are from dense structures
    /// (design section 4): the class bitmap word (one bit per slot, so a
    /// 550 MiB heap of 1 KiB values is 70 KB of bitmap and stays in L2)
    /// and, with several arenas, the group owner word (one `u16` per
    /// line-sized group of slabs). Neither touches the slab descriptor
    /// (one 64-byte line per slab, a cache miss per free on a large heap),
    /// and both are lines only the owning arena's refills and flushes
    /// write, because slabs are handed to arenas in groups that own whole
    /// lines.
    ///
    /// The geometry comes from [`Regions::slab_slot`] (`class_base[c]`,
    /// `lg_region`, `CLASSES[c]`), not from a copy in `tables[c]`: those
    /// arrays are dense (about a dozen lines for all 52 classes, so L1
    /// resident under any workload) and `class_base[c]` is one indexed
    /// load, whereas the per-class table line is one of 52 and reached
    /// through a multiply. With the geometry on the table line the chain
    /// class -> `madd` -> table load -> slab -> bitmap word would be the
    /// whole of `free`'s critical path; here the table's `bits_base`,
    /// `slab_bits` and `committed_slabs` loads run alongside the slot
    /// arithmetic and are needed only for the word address.
    ///
    /// [`classify_free`]: Shared::classify_free
    /// [`Regions::slab_slot`]: crate::layout::Regions::slab_slot
    /// [`set_slot`]: Shared::set_slot
    /// [`home_owns`]: Shared::home_owns
    #[cfg(test)]
    pub(super) fn checked_slot(&self, p: *mut u8, h: &Heap) -> Option<usize> {
        let (class, slab) = self.set_slot(p)?;
        if self.multi_arena() && !self.home_owns(class, slab, h) {
            return None;
        }
        Some(class)
    }

    /// The arena half of [`checked_slot`]: whether the group owner word of
    /// `slab`, a slab holding a slot whose bit [`set_slot`] found set, names
    /// `h`'s home arena. Read only with several arenas ([`FREE_ROUTE`]).
    ///
    /// [`checked_slot`]: Shared::checked_slot
    /// [`set_slot`]: Shared::set_slot
    #[inline(always)]
    pub(super) fn home_owns(&self, class: usize, slab: usize, h: &Heap) -> bool {
        // The home arena is read on the multi-arena branch only. A plain
        // load is hoisted above the flag test (the heap is known valid)
        // and then every single-arena `free` touches the heap's header
        // line, which nothing else on the fast paths reads; a volatile
        // read stays here.
        // SAFETY: `h` is a live heap and `arena` a plain field.
        let home = unsafe { ptr::read_volatile(&raw const h.arena) };
        // SAFETY: the caller's `class` and `slab` came from `set_slot`.
        unsafe { self.lines.get_unchecked(class).owner(slab) == home }
    }

    /// [`checked_slot`] without the arena test: the bit test alone, so a
    /// test can pin down what [`checked_slot`]'s two halves each decide.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[cfg(test)]
    pub(super) fn checked_slot_any(&self, p: *mut u8) -> Option<usize> {
        self.set_slot(p).map(|(class, _)| class)
    }

    /// The bit test the two `free` fast-path checks share: `Some((class,
    /// slab))` when `p` is a slot start in a handed-out slab of `class`
    /// whose bit is set (what [`checked_slot`]'s arena test then reads),
    /// `None` for anything else; never aborts. The class index, not a
    /// reference to its [`ClassLine`]: a reference made LLVM materialise
    /// the line's address (one more `add`) ahead of loads that address it
    /// from the base and the scaled index anyway.
    ///
    /// [`checked_slot`]: Shared::checked_slot
    #[inline(always)]
    pub(super) fn set_slot(&self, p: *mut u8) -> Option<(usize, usize)> {
        let p = p as usize;
        let class = p.wrapping_sub(self.regions.superblock) >> self.regions.lg_stride;
        if class >= NCLASSES {
            return None;
        }
        // SAFETY: `class < NCLASSES` was just checked.
        let line = unsafe { self.lines.get_unchecked(class) };
        let slab = line.set_slab(p)?;
        Some((class, slab))
    }

    /// Allocate a huge run of `size` bytes aligned to `align` in `arena`:
    /// an automatic arena not built yet (a thread whose first allocation
    /// is huge) is built here, an index that does not exist aborts. Null
    /// on exhaustion; the flag says the memory is known zero.
    pub fn alloc_huge(&self, size: usize, align: usize, arena: u16) -> (*mut u8, bool) {
        let Some(a) = self.arena(arena) else {
            no_such_arena(arena)
        };
        let r = self.huge.alloc_run(&self.meta, size, align, arena);
        if !r.ptr.is_null() {
            // The pages the run's descriptor records, which is what
            // `free_huge` will subtract.
            a.note_huge_alloc(r.pages);
        }
        (r.ptr, r.zeroed)
    }

    /// `zero_on_free` for a huge run: clear the usable bytes of the live
    /// run starting at `page` (aborts unless it is one) before its release.
    /// Out of line: it takes the huge lock for the size and then writes
    /// the whole run.
    #[cold]
    #[inline(never)]
    pub fn zero_huge(&self, page: usize) {
        let n = self.huge.usable_size(page);
        // SAFETY: a live run's usable bytes are committed, writable memory
        // this allocator handed out and the caller is freeing.
        unsafe { ptr::write_bytes(self.regions.huge_addr(page) as *mut u8, 0, n) };
    }

    /// Free the huge run starting at `page`. Aborts unless it is a live
    /// run start. Returns the usable bytes released.
    pub fn free_huge(&self, page: usize) -> usize {
        let run = self.huge.free_run(&self.meta, page);
        self.owner(run.arena).note_huge_free(run.pages);
        run.usable
    }
}

#[cold]
#[inline(never)]
fn no_such_arena(index: u16) -> ! {
    panic!("arena {index} does not exist")
}

/// The inverse modulo `2^64` of the odd part of `size` ([`ClassLine`]'s
/// `inv`): Newton's iteration `x = x * (2 - d * x)` doubles the correct
/// low bits from the 3 that `x = d` has for odd `d`, so five steps give
/// 96.
const fn inverse_of_odd_part_u64(size: usize) -> u64 {
    let d = (size as u64) >> size.trailing_zeros();
    let mut x = d;
    let mut i = 0;
    while i < 5 {
        x = x.wrapping_mul(2u64.wrapping_sub(d.wrapping_mul(x)));
        i += 1;
    }
    x
}

// The guard word: `guard_mask`, `tz`, `lg_guard` and the zero padding are
// the line's last eight bytes, 8-aligned.
const _: () = assert!(
    core::mem::offset_of!(ClassLine, guard_mask) == 56
        && core::mem::offset_of!(ClassLine, tz) == 60
        && core::mem::offset_of!(ClassLine, lg_guard) == 61
        && core::mem::offset_of!(ClassLine, _pad) == 62
);
