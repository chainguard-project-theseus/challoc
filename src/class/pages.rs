//! Slab pages (design 5, `docs/proposals/slab-pages.md`): the geometry of
//! a paged class's slab as pages, and its open-page masks.
//!
//! A paged class ([`SlabTable::paged`](super::SlabTable::paged)) keeps one
//! `u16` per slab, bit `p` set when page `p` of the slab is open. A slot is
//! open when every page it overlaps is open; claims take open slots only.
//! A slot that straddles a page boundary (48, 80, 96, ... bytes) overlaps
//! two pages. The slots overlapping page `p` are [`Pages::page_slots`].

use crate::bitmap::word_mask;
use crate::layout::Class;
use vstd::prelude::*;

verus! {

/// Most pages a paged slab has: a 64 KiB slab on 4 KiB pages.
pub const MAX_PAGES: usize = 16;

/// A paged class's slab as pages: slot `size`, `nslots` slots, and
/// `npages` pages of `2^lg_page` bytes.
#[derive(Clone, Copy)]
pub struct Pages {
    pub size: usize,
    pub nslots: usize,
    pub lg_page: u32,
    pub npages: usize,
}

impl Pages {
    /// What the methods need: at most [`MAX_PAGES`] pages of at most
    /// 64 KiB, a slot no larger than the slab, and the slots inside it.
    pub open spec fn wf(self) -> bool {
        &&& 1 <= self.npages <= MAX_PAGES
        &&& self.lg_page <= 16
        &&& 0 < self.size <= 0x4_0000
        &&& self.nslots * self.size <= self.npages * (1usize << self.lg_page)
        &&& self.nslots <= 0x1_0000
    }

    /// The geometry of `cls`'s slab on pages of `2^lg_page` bytes, for a
    /// paged class: a slab of at most 64 KiB, larger than the page.
    pub fn new(cls: &Class, lg_page: u32) -> (r: Pages)
        requires
            cls.wf(),
            12 <= lg_page,
            lg_page < cls.lg_slab as u32,
            cls.lg_slab <= 16,
        ensures
            r.wf(),
            r.size == cls.size,
            r.nslots == cls.nslots,
    {
        let lg_slab = cls.lg_slab;
        let k = (lg_slab as u32 - lg_page) as usize;
        let npages = 1usize << k;
        let ghost ls = lg_slab as usize;
        let ghost lp = lg_page as usize;
        proof {
            assert(1 <= npages <= 16 && npages * (1usize << lp) == (1usize << ls)) by (bit_vector)
                requires npages == 1usize << k, k + lp == ls, ls <= 16, 1 <= k <= 4;
            assert((1usize << ls) == (1usize << lg_slab) && (1usize << lp) == (1usize << lg_page)) by (bit_vector)
                requires ls == lg_slab as usize, lp == lg_page as usize;
            assert((1usize << lg_slab) <= 0x1_0000) by (bit_vector)
                requires lg_slab <= 16;
            assert(cls.nslots <= 0x1_0000) by (nonlinear_arith)
                requires cls.nslots as int * cls.size as int <= 0x1_0000, cls.size >= 1;
        }
        Pages { size: cls.size as usize, nslots: cls.nslots as usize, lg_page, npages }
    }

    /// Every page open.
    #[inline]
    pub fn full(&self) -> (r: u16)
        requires
            self.wf(),
    {
        let n = self.npages as u32;
        proof {
            assert(1u32 <= (1u32 << n) <= 0x1_0000) by (bit_vector)
                requires 1 <= n <= 16;
        }
        ((1u32 << n) - 1) as u16
    }

    /// The slots overlapping page `p`: `lo..hi`, empty past the last
    /// slot.
    #[inline]
    pub fn page_slots(&self, p: usize) -> (r: (usize, usize))
        requires
            self.wf(),
            p < MAX_PAGES,
        ensures
            r.0 <= r.1 <= self.nslots,
    {
        debug_assert!(p < MAX_PAGES);
        let lg_page = self.lg_page;
        let page = 1usize << lg_page;
        proof {
            assert(page <= 0x1_0000) by (bit_vector)
                requires lg_page <= 16, page == 1usize << lg_page;
            assert((p + 1) * page <= 16 * 0x1_0000) by (nonlinear_arith)
                requires p < 16, page <= 0x1_0000;
            assert(p * page <= (p + 1) * page) by (nonlinear_arith);
        }
        let lo = p * page / self.size;
        // `div_ceil`, spelled out: Verus has no specification of it.
        #[allow(clippy::manual_div_ceil)]
        let hi = ((p + 1) * page + self.size - 1) / self.size;
        proof {
            vstd::arithmetic::div_mod::lemma_div_is_ordered(p * page, (p + 1) * page + self.size - 1, self.size as int);
        }
        let hi = if hi < self.nslots { hi } else { self.nslots };
        let lo = if lo < hi { lo } else { hi };
        (lo, hi)
    }

    /// The number of low pages `open` has open in a row, and whether those
    /// are all it has open (the mask is a prefix: every mask a claim makes
    /// opens the lowest closed pages first).
    #[inline]
    pub fn prefix(&self, open: u16) -> (r: (usize, bool))
        requires
            self.wf(),
        ensures
            r.0 <= self.npages,
    {
        let m = open & self.full();
        let mut k = 0;
        while k < self.npages && (m >> k as u16) & 1 == 1
            invariant
                self.wf(),
                k <= self.npages,
            decreases self.npages - k,
        {
            k += 1;
        }
        let only = k >= 16 || (m >> k as u16) == 0;
        (k, only)
    }

    /// The slots wholly inside the first `k` pages.
    #[inline]
    fn prefix_slots(&self, k: usize) -> (r: usize)
        requires
            self.wf(),
            k <= self.npages,
        ensures
            r <= self.nslots,
    {
        let lg_page = self.lg_page;
        proof {
            assert(k <= 16 && lg_page <= 16 ==> (k << lg_page) <= 0x10_0000) by (bit_vector);
        }
        let n = (k << lg_page) / self.size;
        if n < self.nslots { n } else { self.nslots }
    }

    /// Slots that overlap a page `open` has closed (bits `npages` and up
    /// are ignored): a slot is closed when any page it overlaps is. One
    /// division for a prefix mask, two per closed page otherwise.
    pub fn closed_count(&self, open: u16) -> (r: usize)
        requires
            self.wf(),
        ensures
            r <= self.nslots,
    {
        let (k, only) = self.prefix(open);
        if only {
            return self.nslots - self.prefix_slots(k);
        }
        let mut n = 0;
        // The end of the last closed range counted: ranges of ascending
        // pages ascend, and two adjacent pages' share at most the slot
        // straddling their boundary.
        let mut end = 0;
        let mut p = 0;
        while p < self.npages
            invariant
                self.wf(),
                p <= self.npages,
                n <= end <= self.nslots,
            decreases self.npages - p,
        {
            if (open >> p as u16) & 1 == 0 {
                let (lo, hi) = self.page_slots(p);
                let lo = if lo < end { end } else { lo };
                if lo < hi {
                    n += hi - lo;
                    end = hi;
                }
            }
            p += 1;
        }
        n
    }

    /// Slots on open pages only: a claim's ceiling.
    #[inline]
    pub fn open_count(&self, open: u16) -> (r: usize)
        requires
            self.wf(),
        ensures
            r <= self.nslots,
    {
        self.nslots - self.closed_count(open)
    }

    /// The open slots of bitmap word `w` of the slab under `open` (slab
    /// coordinates, bit `b` slot `64 * w + b`): every bit but those of
    /// slots overlapping a closed page. Bits past the last slot are left
    /// set; the bitmap's padding bits are set, so no claim wins them. Only
    /// the pages the word's slots overlap are looked at (one to eight),
    /// two divisions for each that is closed.
    pub fn open_slots(&self, open: u16, w: usize) -> (r: u64)
        requires
            self.wf(),
            w < 0x1000,
    {
        let lo_w = w * 64;
        let hi_w = lo_w + 64;
        if lo_w >= self.nslots {
            return !0;
        }
        proof {
            assert(lo_w * self.size <= hi_w * self.size <= 0x1_0040 * 0x4_0000) by (nonlinear_arith)
                requires lo_w <= hi_w, hi_w <= 0x1_0040, self.size <= 0x4_0000;
            assert(hi_w * self.size >= 1) by (nonlinear_arith)
                requires hi_w >= 1, self.size >= 1;
        }
        let lg_page = self.lg_page;
        let first = (lo_w * self.size) >> lg_page;
        let last = (hi_w * self.size - 1) >> lg_page;
        let mut p = first;
        let mut allow = !0u64;
        while p <= last && p < self.npages
            invariant
                self.wf(),
                lo_w == w * 64,
                hi_w == lo_w + 64,
                w < 0x1000,
            decreases self.npages - p,
        {
            if (open >> p as u16) & 1 == 0 {
                let (lo, hi) = self.page_slots(p);
                let a = if lo > lo_w { lo } else { lo_w };
                let b = if hi < hi_w { hi } else { hi_w };
                if a < b {
                    allow &= !word_mask(a - lo_w, b - lo_w);
                }
            }
            p += 1;
        }
        allow
    }

    /// Bitmap words `lo..hi` cover every word that holds an open slot
    /// under `open` (and the slots of open pages whose other pages are
    /// closed): empty when no page is open. Where a randomised claim starts
    /// (`slot_randomize`), so that it starts on an open slot.
    pub fn open_words(&self, open: u16) -> (r: (usize, usize))
        requires
            self.wf(),
        ensures
            r.0 <= r.1,
            r.1 * 64 <= self.nslots + 63,
    {
        let mut lo = self.nslots;
        let mut hi = 0;
        let mut p = 0;
        while p < self.npages
            invariant
                self.wf(),
                p <= self.npages,
                hi <= self.nslots,
            decreases self.npages - p,
        {
            if (open >> p as u16) & 1 == 1 {
                let (a, b) = self.page_slots(p);
                if a < lo {
                    lo = a;
                }
                if b > hi {
                    hi = b;
                }
            }
            p += 1;
        }
        if lo >= hi {
            return (0, 0);
        }
        // `div_ceil`, spelled out: Verus has no specification of it.
        #[allow(clippy::manual_div_ceil)]
        let hi_w = (hi + 63) / 64;
        (lo / 64, hi_w)
    }

    /// The mask that opens `open`'s lowest closed pages, in order, until
    /// the open slots number at least `need` or every page is open. For a
    /// prefix mask, the pages that cover `need` slots from slot 0 (one
    /// shift); otherwise page by page.
    pub fn open_for(&self, open: u16, need: usize) -> (r: u16)
        requires
            self.wf(),
    {
        let full = self.full();
        let (k, only) = self.prefix(open);
        if only {
            let lg_page = self.lg_page;
            let want = if need > self.nslots {
                self.npages
            } else {
                proof {
                    assert(need * self.size <= 0x1_0000 * 0x4_0000) by (nonlinear_arith)
                        requires need <= self.nslots, self.nslots <= 0x1_0000, self.size <= 0x4_0000;
                    assert(1 <= (1usize << lg_page) <= 0x1_0000) by (bit_vector)
                        requires lg_page <= 16;
                }
                let pages = (need * self.size + (1usize << lg_page) - 1) >> lg_page;
                if pages > self.npages { self.npages } else { pages }
            };
            let kk = if want > k { want } else { k };
            proof {
                assert(forall|n: u32| n <= 16 ==> (1u32 << n) - 1 <= 0xffff) by (bit_vector);
                assert(forall|n: u32| n <= 16 ==> (1u32 << n) >= 1) by (bit_vector);
            }
            return ((1u32 << kk as u32) - 1) as u16;
        }
        let mut m = open & full;
        let mut p = 0;
        while p < self.npages
            invariant
                self.wf(),
                p <= self.npages,
            decreases self.npages - p,
        {
            if self.open_count(m) >= need {
                return m;
            }
            m |= 1u16 << p as u16;
            p += 1;
        }
        m
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LG_MIN_SLAB, LG_SMALL_SLAB, classes_for_page};
    use std::format;
    use std::string::String;
    use std::vec::Vec;

    /// The pages slot `s` overlaps, slot by slot.
    fn pages_of(pg: &Pages, s: usize) -> core::ops::RangeInclusive<usize> {
        let page = 1usize << pg.lg_page;
        (s * pg.size / page)..=(((s + 1) * pg.size - 1) / page)
    }

    /// Slot `s` is open under `m`: every page it overlaps is.
    fn slot_open(pg: &Pages, m: u16, s: usize) -> bool {
        pages_of(pg, s).all(|p| m >> p & 1 == 1)
    }

    /// The masks a geometry is checked under: every mask for up to eight
    /// pages; for more, every prefix, every mask with one page closed or
    /// one open, and a spread of others.
    fn masks(npages: usize) -> Vec<u16> {
        let full = ((1u32 << npages) - 1) as u16;
        if npages <= 8 {
            return (0..=full).collect();
        }
        let mut v = Vec::new();
        for p in 0..=npages {
            v.push(((1u32 << p) - 1) as u16);
        }
        for p in 0..npages {
            v.push(full & !(1 << p));
            v.push(1 << p);
        }
        let mut x = 0x9e37_79b9u32;
        for _ in 0..256 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push(x as u16 & full);
        }
        v
    }

    /// Slab pages (design 5): for every class a page size makes paged
    /// (`SlabTable::pages`), every slot is in exactly the ranges of the
    /// pages it overlaps, and the closed and open counts, the per-word
    /// open slots and the pages a claim opens agree with a slot-by-slot
    /// computation.
    #[test]
    fn page_ranges_and_masks_match_the_slots() {
        let mut checked = 0;
        for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
            for (c, cls) in classes_for_page(lg_page).iter().enumerate() {
                if cls.lg_slab as u32 <= lg_page || cls.lg_slab > 16 {
                    continue;
                }
                let pg = Pages::new(cls, lg_page);
                let ctx = |what: &str| -> String {
                    format!("class {c} ({} B), pages of 2^{lg_page}: {what}", cls.size)
                };
                assert_eq!(pg.npages << lg_page, cls.slab_size(), "{}", ctx("npages"));
                assert!(pg.npages <= MAX_PAGES);
                // Every slot is in exactly the ranges of the pages it
                // overlaps, and each range is contiguous.
                for p in 0..pg.npages {
                    let (lo, hi) = pg.page_slots(p);
                    for s in 0..pg.nslots {
                        assert_eq!(
                            (lo..hi).contains(&s),
                            pages_of(&pg, s).contains(&p),
                            "{}",
                            ctx(&format!("slot {s}, page {p}"))
                        );
                    }
                }
                assert_eq!(pg.full(), ((1u32 << pg.npages) - 1) as u16);
                for m in masks(pg.npages) {
                    let open = (0..pg.nslots).filter(|&s| slot_open(&pg, m, s)).count();
                    assert_eq!(
                        pg.closed_count(m),
                        pg.nslots - open,
                        "{}",
                        ctx(&format!("closed, mask {m:#x}"))
                    );
                    assert_eq!(
                        pg.open_count(m),
                        open,
                        "{}",
                        ctx(&format!("open, mask {m:#x}"))
                    );
                    for w in 0..pg.nslots.div_ceil(64) {
                        let allow = pg.open_slots(m, w);
                        for b in 0..64 {
                            let s = w * 64 + b;
                            let want = s >= pg.nslots || slot_open(&pg, m, s);
                            assert_eq!(
                                allow >> b & 1 == 1,
                                want,
                                "{}",
                                ctx(&format!("word {w} bit {b}, mask {m:#x}"))
                            );
                        }
                    }
                    // The words holding an open slot lie in `open_words`.
                    let (lo, hi) = pg.open_words(m);
                    let words: Vec<usize> = (0..pg.nslots.div_ceil(64))
                        .filter(|&w| {
                            (w * 64..(w * 64 + 64).min(pg.nslots)).any(|s| slot_open(&pg, m, s))
                        })
                        .collect();
                    if let (Some(&a), Some(&b)) = (words.first(), words.last()) {
                        assert!(
                            lo <= a && b < hi,
                            "{}",
                            ctx(&format!("open_words, mask {m:#x}"))
                        );
                    }
                    for need in [
                        0,
                        1,
                        open,
                        open + 1,
                        pg.nslots / 2,
                        pg.nslots,
                        pg.nslots + 1,
                    ] {
                        let r = pg.open_for(m, need);
                        assert_eq!(
                            r & m,
                            m & pg.full(),
                            "{}",
                            ctx("open_for keeps the open pages")
                        );
                        assert!(
                            pg.open_count(r) >= need || r == pg.full(),
                            "{}",
                            ctx("open_for covers")
                        );
                        // Lowest closed pages first, and no more than needed.
                        let added = r & !m;
                        if added != 0 {
                            let top = 15 - added.leading_zeros() as usize;
                            for p in 0..top {
                                assert!(
                                    r >> p & 1 == 1,
                                    "{}",
                                    ctx(&format!("open_for skipped page {p}, mask {m:#x}"))
                                );
                            }
                            assert!(
                                pg.open_count(r & !(1 << top)) < need,
                                "{}",
                                ctx("open_for opened too much")
                            );
                        } else {
                            assert!(
                                open >= need || m & pg.full() == pg.full(),
                                "{}",
                                ctx("open_for opened nothing")
                            );
                        }
                    }
                }
                checked += 1;
            }
        }
        // The small classes on 4 KiB and 16 KiB pages.
        assert!(checked >= 2 * 19, "only {checked} geometries");
    }
}
