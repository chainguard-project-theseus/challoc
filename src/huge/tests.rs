use super::*;
use crate::layout::{Kind, LG_MAX_SLAB, MAX_SLAB, Mapping, NCLASSES};
use crate::meta;
use std::sync::Arc;
use std::vec;
use std::vec::Vec;

/// The page size the environment is built with: the kernel's (the
/// reservations are real, so a 64 KiB-page kernel needs 64 KiB runs and
/// guards).
pub(super) fn page_size() -> usize {
    crate::sys::features::page_size()
}

const LG_REGION: u32 = LG_MAX_SLAB;
const LG_STRIDE: u32 = LG_REGION + 2;
const META_LEN: usize = 16 << 20;
/// A huge region of 16384 pages (64 MiB on a 4 KiB kernel, 1 GiB on a
/// 64 KiB one; a `PROT_NONE` reservation either way), small enough to
/// exhaust and the same page arithmetic on every kernel.
const HUGE_PAGES_N: usize = 16384;
fn huge_len() -> usize {
    HUGE_PAGES_N * page_size()
}
fn huge_pages() -> usize {
    HUGE_PAGES_N
}

/// Test environment: a metadata region and a huge region, both real
/// `PROT_NONE` reservations so commits, guards and purges go through the
/// kernel and run memory can be touched.
pub(super) struct Env {
    pub meta: Meta,
    pub regions: Regions,
    maps: [(usize, usize); 2],
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
        page_size().trailing_zeros(),
        [
            Mapping {
                addr: superblock,
                len: sb_len,
            },
            Mapping {
                addr: huge.0,
                len: huge.1,
            },
            Mapping {
                addr: meta.0,
                len: meta.1,
            },
        ],
    )
}

impl Env {
    pub fn new() -> Env {
        Env::with_meta_len(META_LEN)
    }

    pub fn with_meta_len(meta_len: usize) -> Env {
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let reserve = |len: usize| -> usize {
            unsafe {
                crate::sys::mmap(
                    0,
                    len,
                    PROT_NONE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                    -1,
                    0,
                )
            }
            .expect("reserve")
        };
        // `mmap` only promises page alignment; the alignment tests
        // assume a `MAX_SLAB`-aligned huge base like the real layout.
        let hraw = reserve(huge_len() + crate::layout::MAX_SLAB);
        let hb = (hraw + crate::layout::MAX_SLAB - 1) & !(crate::layout::MAX_SLAB - 1);
        let mb = reserve(meta_len);
        let regions = regions_at((hb, huge_len()), (mb, meta_len));
        Env {
            meta: Meta::new(mb, meta_len, page_size()),
            regions,
            maps: [(hraw, huge_len() + crate::layout::MAX_SLAB), (mb, meta_len)],
        }
    }

    pub fn huge(&self) -> Huge {
        Huge::new(&self.meta, &self.regions, guard_mode()).expect("huge allocator")
    }

    /// [`Huge::alloc`] plus an audit and a check that the run's pages
    /// are committed, by reading the first and last usable byte (a
    /// `PROT_NONE` page would fault). The guard page is never touched.
    pub fn alloc(&self, h: &Huge, size: usize, align: usize) -> (*mut u8, bool) {
        let r = h.alloc(&self.meta, size, align, 0);
        h.audit();
        if !r.0.is_null() {
            let info = h.lookup(self.page_of(r.0)).expect("run descriptor");
            assert!(info.usable >= size.max(1));
            unsafe {
                core::ptr::read_volatile(r.0);
                core::ptr::read_volatile(r.0.add(info.usable - 1));
            }
        }
        r
    }

    pub fn free(&self, h: &Huge, p: *mut u8) -> usize {
        let page = match self.regions.classify(p as usize) {
            Kind::Huge { page } => page,
            other => panic!("{p:?} classified as {other:?}"),
        };
        let n = h.free_run(&self.meta, page).usable;
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

impl Drop for Env {
    fn drop(&mut self) {
        for (addr, len) in self.maps {
            unsafe { crate::sys::munmap(addr, len) }.unwrap();
        }
    }
}

/// Guard mode under test: the probed kernel support.
pub(super) fn guard_mode() -> bool {
    crate::sys::features::probe_guard_install(page_size())
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
    assert_eq!(h.npages(), huge_pages());
    assert_eq!(h.usable_for(1), Some(page_size()));
    assert_eq!(h.usable_for(page_size() + 1), Some(2 * page_size()));
    assert_eq!(h.usable_for(usize::MAX), None);
    let (p, zero) = env.alloc(&h, 300 * 1024, 16);
    assert_eq!(p as usize, env.regions.huge_base);
    assert!(zero);
    assert_eq!(env.regions.classify(p as usize), Kind::Huge { page: 0 });
    let pages = (300usize * 1024).div_ceil(page_size());
    let info = h.lookup(0).unwrap();
    assert_eq!(
        info,
        RunInfo {
            start: 0,
            pages: pages + GUARD_PAGES,
            usable: pages * page_size(),
            arena: 0,
            state: RunState::Active,
            dirty_at: 0
        }
    );
    assert_eq!(h.usable_size(0), pages * page_size());
    assert_eq!(h.lookup(pages), None, "guard page is not keyed");
    let st = h.stats();
    assert_eq!(st.runs, 1);
    assert_eq!(st.run_pages, pages + GUARD_PAGES);
    assert_eq!(st.next_page, pages + GUARD_PAGES);
    // The next run follows the guard.
    let (q, zero) = env.alloc(&h, 1, 1);
    assert!(zero);
    assert_eq!(env.page_of(q), pages + GUARD_PAGES);
    assert_eq!(h.usable_size(env.page_of(q)), page_size());
    assert_eq!(env.free(&h, p), pages * page_size());
    assert_eq!(env.free(&h, q), page_size());
}

#[test]
fn free_and_reuse_prefers_the_lowest_address() {
    let env = Env::new();
    let h = env.huge();
    let (a, _) = env.alloc(&h, 4 * page_size(), 1);
    let (b, _) = env.alloc(&h, 4 * page_size(), 1);
    let (c, _) = env.alloc(&h, 4 * page_size(), 1);
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
    let (d, zero) = env.alloc(&h, 2 * page_size(), 1);
    assert_eq!(env.page_of(d), 5);
    assert!(!zero);
    let rest = h.lookup(8).unwrap();
    assert_eq!(
        (rest.start, rest.pages, rest.state),
        (8, 2, RunState::Empty)
    );
    // Too big for the remaining hole: goes to the mark.
    let (e, zero) = env.alloc(&h, 3 * page_size(), 1);
    assert_eq!(env.page_of(e), 15);
    assert!(zero);
    // Purge makes the hole zero again.
    assert_eq!(h.purge_empty(u64::MAX), 1);
    assert_eq!(h.lookup(8).unwrap().state, RunState::Purged);
    let (f, zero) = env.alloc(&h, page_size(), 1);
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
    let (a, _) = env.alloc(&h, page_size(), 1);
    let (b, _) = env.alloc(&h, page_size(), 1);
    env.free(&h, b);
    // The Empty extent [2, 4) touches the mark; a 6-page run starts in
    // it and extends the mark. Dirty memory means not zero.
    let (c, zero) = env.alloc(&h, 5 * page_size(), 1);
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
    let (a, _) = env.alloc(&h, page_size(), 1);
    let mut held = vec![a];
    for lg in 13..=22 {
        let align = 1usize << lg;
        let (p, zero) = env.alloc(&h, 3 * page_size(), align);
        assert!(!p.is_null());
        assert_eq!(p as usize % align, 0, "align {align}");
        assert!(zero);
        let page = env.page_of(p);
        let info = h.lookup(page).unwrap();
        assert_eq!(
            (info.pages, info.usable, info.state),
            (3 + GUARD_PAGES, 3 * page_size(), RunState::Active)
        );
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
    let (q, _) = env.alloc(&h, page_size(), 2 * page_size());
    assert_eq!(q as usize % (2 * page_size()), 0);
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
    let (a, _) = env.alloc(&h, page_size(), 1);
    let (b, zero) = env.alloc(&h, page_size(), 1 << 20);
    assert!(zero);
    assert_eq!(b as usize % (1 << 20), 0);
    let hole = h.lookup(2).unwrap();
    assert_eq!(hole.state, RunState::Purged);
    assert_eq!(hole.end(), env.page_of(b));
    // The hole is reused first, and is zero.
    let (c, zero) = env.alloc(&h, page_size(), 1);
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
    let (a, _) = env.alloc(&h, page_size(), 1);
    let (b, _) = env.alloc(&h, page_size(), 1);
    let (c, _) = env.alloc(&h, page_size(), 1);
    let (d, _) = env.alloc(&h, page_size(), 1);
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
    h.set_retain_max(2 * page_size());
    let (a, _) = env.alloc(&h, page_size(), 1);
    let (b, _) = env.alloc(&h, 3 * page_size(), 1);
    let (c, _) = env.alloc(&h, page_size(), 1);
    env.free(&h, b);
    assert_eq!(
        h.lookup(2).unwrap().state,
        RunState::Purged,
        "above retain_max: purged at once"
    );
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
    let (p, zero) = env.alloc(&h, 8 * page_size(), 1);
    assert!(zero);
    assert_eq!(env.page_of(p), 0);
    assert_eq!(h.stats().next_page, 9);
}

/// Design 11: under the policy's keep ratio the walk purges the oldest
/// extents whole until within `total * keep16`, an extent larger than
/// the excess included (free neighbours coalesce, so a program's
/// pieces are one extent); along the curve alone (`KEEP_ALL`) such an
/// extent is skipped as before.
#[test]
fn decay_with_a_keep_ratio_purges_coalesced_extents() {
    use crate::policy::{FRAC_ONE, KEEP_ALL};
    let env = Env::new();
    let h = env.huge();
    const W: u64 = 200_000;
    let a = env.alloc(&h, 4 * page_size(), 1).0;
    let b = env.alloc(&h, 4 * page_size(), 1).0;
    env.free(&h, a);
    env.free(&h, b);
    // One coalesced extent.
    assert_eq!(h.stats().empty, 1);
    let total = h.stats().empty_pages;
    assert!(total >= 8, "{total}");
    let t0 = h.oldest_dirty_at().unwrap();
    // Uncapped, one step in: nothing goes (the curve keeps it all).
    assert_eq!(h.decay_with(t0 + 1000, W, KEEP_ALL), Some(t0 + 2000));
    assert_eq!(h.stats().empty, 1);
    // Capped at an eighth, a step later: the extent is larger than
    // the excess and goes whole.
    assert_eq!(h.empty_bytes(), total << h.lg_page);
    assert_eq!(h.decay_with(t0 + 2000, W, FRAC_ONE / 8), None);
    assert_eq!((h.stats().empty, h.stats().purged), (0, 1));
    assert_eq!(h.empty_bytes(), 0);
}

/// The decay driver's view of the huge allocator (design section 8):
/// the curve in pages over the `Empty` list, oldest extent first; a
/// lone extent lasts until its weight is zero; `oldest_empty` follows
/// the head through frees, purges and reuse.
#[test]
fn decay_purges_the_oldest_extents_along_the_curve() {
    let env = Env::new();
    let h = env.huge();
    assert_eq!(h.oldest_empty(), 0);
    assert_eq!(h.decay(u64::MAX, 1000), None);
    const W: u64 = 200_000;
    let mut ps = Vec::new();
    for _ in 0..4 {
        ps.push(env.alloc(&h, page_size(), 1).0);
    }
    // Free runs 0 and 2 (no coalescing): one cohort of two extents,
    // the clock reads made uniform.
    env.free(&h, ps[0]);
    env.free(&h, ps[2]);
    let t0 = h.oldest_dirty_at().unwrap();
    assert_eq!(h.oldest_empty(), t0);
    {
        let g = h.inner.lock();
        let mut d = g.empty_head;
        while !d.is_null() {
            unsafe {
                (*d).dirty_at = t0;
                d = (*d).next;
            }
        }
    }
    let pages = h.stats().empty_pages;
    assert_eq!(h.stats().empty, 2);
    // Before the first step ends: no walk. At it: a walk purging
    // nothing. Mid-window: half the pages, the head (run 0) first.
    assert_eq!(h.decay(t0 + 999, W), Some(t0 + 1000));
    assert_eq!(h.decay(t0 + 1000, W), Some(t0 + 2000));
    assert_eq!(h.stats().empty, 2);
    assert_eq!(h.decay(t0 + 1500, W), Some(t0 + 2000));
    assert_eq!(h.decay(t0 + W / 2, W), Some(t0 + W / 2 + 1000));
    assert_eq!((h.stats().empty, h.stats().purged), (1, 1));
    assert_eq!(h.stats().purged_pages, pages / 2);
    assert_eq!(
        h.lookup(env.page_of(ps[0])).unwrap().state,
        RunState::Purged
    );
    assert_eq!(h.lookup(env.page_of(ps[2])).unwrap().state, RunState::Empty);
    assert_eq!(h.oldest_empty(), t0);
    // The survivor, alone, lasts until the last step of the window.
    assert_eq!(h.decay(t0 + W - 2000, W), Some(t0 + W - 1000));
    assert_eq!(h.stats().empty, 1);
    assert_eq!(h.decay(t0 + W - 1000, W), None);
    assert_eq!((h.stats().empty, h.stats().purged), (0, 2));
    assert_eq!(h.oldest_empty(), 0);
    // Reuse keeps the atomic in step (`alloc` syncs it). Freeing run 1
    // merges it with its Purged neighbours into one Empty extent of
    // three runs.
    env.free(&h, ps[1]);
    let t1 = h.oldest_dirty_at().unwrap();
    assert_eq!(h.oldest_empty(), t1);
    assert_eq!(h.stats().empty, 1);
    let (a, _) = env.alloc(&h, page_size(), 1);
    assert_eq!(a, ps[0]);
    let (b, zeroed) = env.alloc(&h, page_size(), 1);
    assert_eq!(b, ps[1]);
    assert!(!zeroed);
    assert_eq!(h.oldest_empty(), t1);
    let (c, _) = env.alloc(&h, page_size(), 1);
    assert_eq!(c, ps[2]);
    assert_eq!(h.stats().empty, 0);
    assert_eq!(h.oldest_empty(), 0);
    assert_eq!(h.decay(u64::MAX / 2, W), None);
    // A zero period purges at once; everything freed and purged at
    // once clears the head too.
    env.free(&h, a);
    let t = h.oldest_empty();
    assert!(t > 0);
    assert_eq!(h.decay(t, 0), None);
    assert_eq!(h.lookup(env.page_of(a)).unwrap().state, RunState::Purged);
    for &p in &[b, c, ps[3]] {
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
        ps.push(env.alloc(&h, page_size(), 1).0);
    }
    // Free every other run so nothing coalesces: three Empty extents
    // with increasing dirty_at.
    let mut times = Vec::new();
    for &p in ps.iter().step_by(2) {
        // The coarse clock ticks every few milliseconds.
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
    assert_eq!(
        h.stats(),
        HugeStats {
            runs: 3,
            run_pages: 6,
            purged: 3,
            purged_pages: 6,
            next_page: 12,
            table_cap: 1024,
            ..HugeStats::default()
        }
    );
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
    // 1 MiB + guard runs (257 pages of 4 KiB, 17 of 64 KiB): the region
    // holds the whole quotient and the remainder cannot take another.
    assert_eq!(
        held.len(),
        huge_pages() / ((1 << 20) / page_size() + GUARD_PAGES)
    );
    let st = h.stats();
    assert!(st.next_page <= huge_pages());
    // Something small still fits in the leftover.
    let (s, _) = env.alloc(&h, page_size(), 1);
    assert!(!s.is_null());
    held.push(s);
    for p in held.drain(..) {
        env.free(&h, p);
    }
    let st = h.stats();
    assert_eq!((st.runs, st.empty), (0, 1));
    // The whole region minus a guard is one run now.
    let (big, zero) = env.alloc(&h, huge_len() - page_size(), 1);
    assert_eq!(env.page_of(big), 0);
    assert!(!zero);
    assert!(env.alloc(&h, 1, 1).0.is_null());
    env.free(&h, big);
    assert!(
        env.alloc(&h, huge_len(), 1).0.is_null(),
        "no room for the guard"
    );
    assert!(
        env.alloc(&h, page_size(), 2 * huge_len()).0.is_null(),
        "alignment beyond the region"
    );
}

/// `malloc(0)`-style requests that reach the huge allocator (a zero
/// size with an alignment above the largest slab class) get one usable
/// page, as `usable_pages` promises.
#[test]
fn a_zero_byte_request_gets_one_page() {
    let env = Env::new();
    let h = env.huge();
    assert_eq!(h.usable_pages(0), Some(1));
    assert_eq!(h.usable_for(0), Some(page_size()));
    let (p, zero) = env.alloc(&h, 0, 1);
    assert!(!p.is_null() && zero);
    let page = env.page_of(p);
    let info = h.lookup(page).expect("run descriptor");
    assert_eq!(
        (info.pages, info.usable, info.state),
        (1 + GUARD_PAGES, page_size(), RunState::Active)
    );
    assert_eq!(h.usable_size(page), page_size());
    assert_eq!(env.free(&h, p), page_size());
    assert_eq!(h.stats().runs, 0);
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
        assert_eq!(
            (info.pages, info.arena, info.state),
            (2, 7, RunState::Active)
        );
    }
    // Free every third run: many removals with shifts, and every
    // remaining key must still resolve.
    for (i, &page) in runs.iter().enumerate() {
        if i % 3 == 0 {
            h.free_run(&env.meta, page);
        }
    }
    h.audit();
    for (i, &page) in runs.iter().enumerate() {
        let info = h.lookup(page).unwrap();
        if i % 3 == 0 {
            assert_eq!(info.state, RunState::Empty);
        } else {
            assert_eq!(info.state, RunState::Active);
            assert_eq!(h.usable_size(page), page_size());
        }
    }
    for (i, &page) in runs.iter().enumerate() {
        if i % 3 != 0 {
            h.free_run(&env.meta, page);
        }
    }
    h.audit();
    assert_eq!(h.stats().empty, 1);
    let ms = env.meta.stats();
    assert!(
        ms.allocated < h.stats().table_cap * 16 + 4096,
        "descriptors leaked: {ms:?}"
    );
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
        let (p, _) = env.alloc(&h, page_size(), 1);
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
    let (p, _) = env.alloc(&h, page_size(), 1);
    assert!(!p.is_null());
    // The tables of a region that does not fit at all fail cleanly: two
    // page-rounded reservations never fit in one page, whatever its size.
    let tiny = Env::with_meta_len(page_size());
    assert!(Huge::new(&tiny.meta, &tiny.regions, guard_mode()).is_none());
}

/// Random allocations, frees and purges against an interval model.
#[test]
fn randomised_model_check() {
    let env = Env::new();
    let h = env.huge();
    h.set_retain_max(40 * page_size());
    let mut rng = 0x1234_5678_9ABC_DEF0u64;
    // (page, usable pages, pattern)
    let mut live: Vec<(usize, usize, u64)> = Vec::new();
    let mut zero_flags = 0;
    for step in 0..4000 {
        let r = xorshift(&mut rng);
        match r % 10 {
            0..=4 => {
                let size = match xorshift(&mut rng) % 4 {
                    0 => (xorshift(&mut rng) as usize % (4 * page_size())) + 1,
                    1 => (xorshift(&mut rng) as usize % (16 * page_size())) + 1,
                    2 => (xorshift(&mut rng) as usize % (100 * page_size())) + 1,
                    _ => page_size() * ((xorshift(&mut rng) as usize % 8) + 1),
                };
                let align = if xorshift(&mut rng) % 3 == 0 {
                    1usize << (13 + xorshift(&mut rng) % 9)
                } else {
                    1
                };
                let (p, zero) = env.alloc(&h, size, align);
                if p.is_null() {
                    // Only acceptable when the region is really tight.
                    assert!(
                        h.stats().next_page + size / page_size() + 2 > huge_pages()
                            || align > page_size(),
                        "step {step}"
                    );
                    continue;
                }
                assert_eq!(p as usize % align, 0);
                let page = env.page_of(p);
                let n = size.div_ceil(page_size());
                assert_eq!(h.usable_size(page), n * page_size());
                for &(q, m, _) in &live {
                    assert!(
                        page + n + GUARD_PAGES <= q || q + m + GUARD_PAGES <= page,
                        "overlap at step {step}"
                    );
                }
                if zero {
                    zero_flags += 1;
                }
                live.push((page, n, r));
            }
            5..=8 if !live.is_empty() => {
                let i = xorshift(&mut rng) as usize % live.len();
                let (page, n, _) = live.swap_remove(i);
                assert_eq!(h.free(&env.meta, page), n * page_size());
                h.audit();
            }
            9 => {
                let cutoff = if xorshift(&mut rng) % 2 == 0 {
                    u64::MAX
                } else {
                    now_ns().saturating_sub(5)
                };
                h.purge_empty(cutoff);
                h.audit();
            }
            _ => {}
        }
        let st = h.stats();
        assert_eq!(st.runs, live.len());
        assert_eq!(
            st.run_pages,
            live.iter().map(|&(_, n, _)| n + GUARD_PAGES).sum::<usize>()
        );
    }
    assert!(zero_flags > 0);
    for (page, _, _) in live.drain(..) {
        h.free_run(&env.meta, page);
    }
    h.audit();
    let st = h.stats();
    assert_eq!(st.runs, 0);
    assert_eq!(st.empty + st.purged, 1);
    assert_eq!(h.purge_empty(u64::MAX), st.empty);
    assert_eq!(h.stats().purged_pages, st.next_page);
}

/// A run is keyed by its start page only, a free extent by its first
/// and last page (`Inner::add_extent`), which is what `free_before` and
/// `free_after` rely on; `drop_extent` takes the same keys out again and
/// the counters follow the state. `table_cap` is read from the table.
#[test]
fn runs_have_one_key_and_free_extents_two() {
    let env = Env::new();
    let h = env.huge();
    assert_eq!(h.stats().table_cap, h.inner.lock().table.cap());
    let (p, _) = env.alloc(&h, 3 * page_size(), 1);
    let page = env.page_of(p);
    let run = h.lookup(page).expect("run keyed by its start");
    assert_eq!((run.pages, run.state), (3 + GUARD_PAGES, RunState::Active));
    assert_eq!(h.lookup(page + 1), None);
    assert_eq!(
        h.lookup(page + run.pages - 1),
        None,
        "a run has no last-page key"
    );
    let st = h.stats();
    assert_eq!(
        (st.runs, st.run_pages, st.empty + st.purged),
        (1, run.pages, 0)
    );
    env.free(&h, p);
    let ext = h.lookup(page).expect("free extent keyed by its first page");
    assert_eq!(h.lookup(page + ext.pages - 1), Some(ext), "and by its last");
    assert_eq!(h.lookup(page + 1), None);
    let st = h.stats();
    assert_eq!((st.runs, st.run_pages, st.empty + st.purged), (0, 0, 1));
    // Taking the whole extent drops both keys and counts it out.
    let (q, _) = env.alloc(&h, 3 * page_size(), 1);
    assert_eq!(q, p);
    assert_eq!(h.lookup(page + ext.pages - 1), None);
    let st = h.stats();
    assert_eq!((st.runs, st.empty + st.purged), (1, 0));
}

/// `prior_state` names where a run's pages come from for `check_zero`:
/// fresh at or past the mark (or past the extent's end), else the
/// extent's state.
#[test]
fn prior_state_names_the_source_of_the_pages() {
    let env = Env::new();
    let h = env.huge();
    assert_eq!(
        h.prior_state(
            0,
            core::ptr::null_mut(),
            vstd::prelude::Tracked::assume_new()
        ),
        "fresh"
    );
    let (p, _) = env.alloc(&h, page_size(), 1);
    let (q, _) = env.alloc(&h, page_size(), 1);
    env.free(&h, p);
    let ext = h.inner.lock().table.get(0);
    assert_eq!(
        h.lookup(0).map(|i| (i.pages, i.state)),
        Some((1 + GUARD_PAGES, RunState::Empty))
    );
    assert_eq!(
        h.prior_state(0, ext, vstd::prelude::Tracked::assume_new()),
        "Empty"
    );
    assert_eq!(
        h.prior_state(1 + GUARD_PAGES, ext, vstd::prelude::Tracked::assume_new()),
        "fresh",
        "pages past the extent's end"
    );
    h.purge_empty(u64::MAX);
    assert_eq!(
        h.prior_state(0, ext, vstd::prelude::Tracked::assume_new()),
        "Purged"
    );
    env.free(&h, q);
    h.audit();
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
                let size = (xorshift(&mut rng) as usize % (8 * page_size())) + 1;
                let (p, _) = h.alloc(&env.meta, size, 1, t as u16);
                if !p.is_null() {
                    let page = env.page_of(p);
                    assert_eq!(h.lookup(page).unwrap().arena, t as u16);
                    held.push((page, size.div_ceil(page_size())));
                    got += 1;
                }
                if xorshift(&mut rng) % 3 == 0 && !held.is_empty() {
                    let i = xorshift(&mut rng) as usize % held.len();
                    let (page, n) = held.swap_remove(i);
                    assert_eq!(h.free(&env.meta, page), n * page_size());
                }
            }
            for (page, _) in held {
                h.free_run(&env.meta, page);
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
    let (p, _) = env.alloc(&h, 4 * page_size(), 1);
    h.free(&env.meta, env.page_of(p) + 1);
}

#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn free_of_a_guard_page_aborts() {
    let env = Env::new();
    let h = env.huge();
    let (p, _) = env.alloc(&h, 4 * page_size(), 1);
    h.free(&env.meta, env.page_of(p) + 4);
}

#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn free_beyond_the_mark_aborts() {
    let env = Env::new();
    let h = env.huge();
    let _ = env.alloc(&h, 4 * page_size(), 1);
    h.free(&env.meta, 100);
}

#[test]
#[should_panic(expected = "huge allocation already freed")]
fn double_free_aborts() {
    let env = Env::new();
    let h = env.huge();
    let (p, _) = env.alloc(&h, 4 * page_size(), 1);
    env.free(&h, p);
    h.free(&env.meta, env.page_of(p));
}

#[test]
#[should_panic(expected = "huge allocation already freed")]
fn free_of_a_free_extents_last_page_aborts() {
    let env = Env::new();
    let h = env.huge();
    let (p, _) = env.alloc(&h, 4 * page_size(), 1);
    let (_q, _) = env.alloc(&h, page_size(), 1);
    env.free(&h, p);
    h.free(&env.meta, env.page_of(p) + 4);
}

#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn usable_size_of_a_bad_pointer_aborts() {
    let env = Env::new();
    let h = env.huge();
    let (p, _) = env.alloc(&h, 4 * page_size(), 1);
    h.usable_size(env.page_of(p) + 2);
}

#[test]
fn run_state_round_trips_and_rejects_other_bytes() {
    for s in [RunState::Active, RunState::Empty, RunState::Purged] {
        assert_eq!(RunState::from_u8(s as u8), s);
        assert_eq!(RunState::try_from_u8(s as u8), Some(s));
    }
    for v in 3..=255u8 {
        assert_eq!(RunState::try_from_u8(v), None, "{v}");
    }
}

#[test]
#[should_panic(expected = "is not a run state")]
fn a_run_state_byte_no_state_has_aborts() {
    let _ = RunState::from_u8(3);
}

#[test]
#[should_panic(expected = "Empty extent count underflow")]
fn huge_stats_abort_on_a_free_count_going_below_zero() {
    let mut s = HugeStats::default();
    s.sub(RunState::Empty, 1);
}

/// The run counters go through the same checked path as the free ones
/// since `drop_extent` uncounts a freed run through `HugeStats::sub`.
#[test]
#[should_panic(expected = "Active extent count underflow")]
fn huge_stats_abort_on_a_run_count_going_below_zero() {
    let mut s = HugeStats::default();
    s.add(RunState::Active, 3);
    assert_eq!((s.runs, s.run_pages), (1, 3));
    s.sub(RunState::Active, 3);
    assert_eq!(s, HugeStats::default());
    s.sub(RunState::Active, 1);
}
