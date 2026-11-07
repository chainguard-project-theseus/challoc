//! Slab pages (docs/proposals/slab-pages.md, section 3.2): which slots of a
//! slab overlap which of its pages, and which slots of one bitmap word an
//! open-page mask leaves open. Pure geometry: no state, no effects. A slot
//! is open when every page it overlaps is open; a slot that straddles a
//! page boundary (48, 80, 96, ..., 448 bytes on 4 KiB pages) needs both.

use vstd::prelude::*;

verus! {

/// The most pages a slab with an open-page mask has: the mask is a `u16`.
pub const MAX_MASK_PAGES: usize = 16;

/// Page `p` and slot `s` of a slab whose slots are `size` bytes and whose
/// pages are `pg` bytes share a byte: slot `s` is bytes `[s * size, (s + 1)
/// * size)`, page `p` is bytes `[p * pg, (p + 1) * pg)`.
pub open spec fn overlaps(size: int, pg: int, p: int, s: int) -> bool {
    s * size < (p + 1) * pg && p * pg < (s + 1) * size
}

/// Bit `p` of the open-page mask is set.
pub open spec fn page_open(mask: u16, p: int) -> bool {
    0 <= p < 16 && (mask >> (p as u16)) & 1u16 == 1u16
}

/// Slot `s` is open under `mask`: every one of the slab's `pages` pages
/// that it overlaps is open.
pub open spec fn slot_open(size: int, pg: int, pages: int, mask: u16, s: int) -> bool {
    forall|p: int| 0 <= p < pages && #[trigger] overlaps(size, pg, p, s) ==> page_open(mask, p)
}

/// Bit `i` of `x` is set.
pub open spec fn bit(x: u64, i: int) -> bool {
    0 <= i < 64 && (x >> (i as u64)) & 1u64 == 1u64
}

/// `x / d <= s` exactly when `x < (s + 1) * d`, for `x >= 0`, `d > 0`.
proof fn lemma_div_le_iff(x: int, d: int, s: int)
    requires
        x >= 0,
        d > 0,
        s >= 0,
    ensures
        (x / d <= s) <==> (x < (s + 1) * d),
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x, d);
    let q = x / d;
    let r = x % d;
    vstd::arithmetic::div_mod::lemma_mod_bound(x, d);
    if q <= s {
        assert(x < (s + 1) * d) by (nonlinear_arith)
            requires
                x == d * q + r,
                0 <= r < d,
                q <= s,
                d > 0;
    }
    if x < (s + 1) * d {
        assert(q <= s) by (nonlinear_arith)
            requires
                x == d * q + r,
                0 <= r < d,
                x < (s + 1) * d,
                d > 0,
                q >= 0;
    }
}

/// `s <= y / d` exactly when `s * d <= y`, for `y >= 0`, `d > 0`.
proof fn lemma_le_div_iff(y: int, d: int, s: int)
    requires
        y >= 0,
        d > 0,
        s >= 0,
    ensures
        (s <= y / d) <==> (s * d <= y),
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y, d);
    let q = y / d;
    let r = y % d;
    vstd::arithmetic::div_mod::lemma_mod_bound(y, d);
    if s <= q {
        assert(s * d <= y) by (nonlinear_arith)
            requires
                y == d * q + r,
                0 <= r < d,
                s <= q,
                d > 0,
                s >= 0;
    }
    if s * d <= y {
        assert(s <= q) by (nonlinear_arith)
            requires
                y == d * q + r,
                0 <= r < d,
                s * d <= y,
                d > 0,
                q >= 0,
                s >= 0;
    }
}

/// The slots of page `p`: `[first, end)`, exactly those slots below
/// `nslots` that overlap page `p` (`overlaps`). `pg` is the page size and
/// `size` the slot size. `overlaps` is symmetric in its two units, so the
/// same call with the roles swapped, `page_slots(pg, pages, size, s)`,
/// gives the pages slot `s` overlaps. Every product stays below 2^32.
pub fn page_slots(size: usize, nslots: usize, pg: usize, p: usize) -> (r: (usize, usize))
    requires
        1 <= size <= 0x10000,
        1 <= pg <= 0x10000,
        p < 0x10000,
        nslots <= 0x10000,
    ensures
        r.0 <= r.1 <= nslots,
        forall|s: int| (r.0 <= s < r.1) <==> (0 <= s < nslots && #[trigger] overlaps(size as int, pg as int, p as int, s)),
{
    proof {
        assert((p + 1) * pg <= 0x10000 * 0x10000) by (nonlinear_arith)
            requires
                p < 0x10000,
                pg <= 0x10000;
        assert(p * pg <= (p + 1) * pg) by (nonlinear_arith);
        assert((p + 1) * pg >= 1) by (nonlinear_arith)
            requires
                pg >= 1;
    }
    let lo_byte = p * pg;
    let hi_byte = (p + 1) * pg - 1;
    let first = lo_byte / size;
    let last_plus = hi_byte / size + 1;
    let end = if last_plus < nslots { last_plus } else { nslots };
    proof {
        assert forall|s: int| (first <= s < end) <==> (0 <= s < nslots && #[trigger] overlaps(size as int, pg as int, p as int, s)) by {
            if s >= 0 {
                lemma_div_le_iff(lo_byte as int, size as int, s);
                lemma_le_div_iff(hi_byte as int, size as int, s);
                assert((p as int + 1) * pg as int == p as int * pg as int + pg as int) by (nonlinear_arith);
                assert((s + 1) * size as int == s * size as int + size as int) by (nonlinear_arith);
            }
        }
    }
    if first > end { (end, end) } else { (first, end) }
}

/// Bits `[lo, hi)` of a `u64`.
fn range_mask(lo: u64, hi: u64) -> (r: u64)
    requires
        lo <= hi <= 64,
    ensures
        forall|i: int| 0 <= i < 64 ==> (#[trigger] bit(r, i) <==> lo <= i < hi),
{
    let r = if hi == lo {
        0u64
    } else if hi - lo == 64 {
        !0u64
    } else {
        let n = hi - lo;
        assert(1u64 << n >= 1u64) by (bit_vector)
            requires
                n < 64;
        ((1u64 << n) - 1) << lo
    };
    proof {
        assert forall|i: int| 0 <= i < 64 implies (#[trigger] bit(r, i) <==> lo <= i < hi) by {
            let iu = i as u64;
            if hi == lo {
                assert((0u64 >> iu) & 1u64 == 0u64) by (bit_vector);
            } else if hi - lo == 64 {
                assert((!0u64 >> iu) & 1u64 == 1u64) by (bit_vector) requires iu < 64;
            } else {
                let n = (hi - lo) as u64;
                assert((((((1u64 << n) - 1u64) as u64) << lo) >> iu) & 1u64 == (if lo <= iu && iu < lo + n { 1u64 } else { 0u64 })) by (bit_vector)
                    requires
                        n < 64,
                        n >= 1,
                        lo + n <= 64,
                        iu < 64;
            }
        }
    }
    r
}

/// The open slots of bitmap word `w` of a slab: bit `i` is set exactly
/// when slot `64 * w + i` is below `nslots` and every one of the slab's
/// `pages` pages it overlaps is open in `mask` (`slot_open`).
pub fn open_slots_word(size: usize, nslots: usize, pg: usize, pages: usize, mask: u16, w: usize) -> (r: u64)
    requires
        1 <= size <= 0x10000,
        4096 <= pg <= 0x10000,
        pages <= MAX_MASK_PAGES,
        nslots <= 0x10000,
        w < 1024,
    ensures
        forall|i: int| 0 <= i < 64 ==> (#[trigger] bit(r, i) <==> (64 * w + i < nslots && slot_open(size as int, pg as int, pages as int, mask, 64 * w + i))),
{
    let base = 64 * w;
    let valid: u64 = if nslots <= base { 0 } else if nslots - base >= 64 { 64 } else { (nslots - base) as u64 };
    let mut acc = range_mask(0, valid);
    let mut p: usize = 0;
    while p < pages
        invariant
            1 <= size <= 0x10000,
            4096 <= pg <= 0x10000,
            pages <= MAX_MASK_PAGES,
            nslots <= 0x10000,
            base == 64 * w,
            w < 1024,
            valid as int == if nslots <= base { 0int } else if nslots - base >= 64 { 64int } else { nslots - base },
            p <= pages,
            forall|i: int| 0 <= i < 64 ==> (#[trigger] bit(acc, i) <==> (i < valid && (forall|q: int| 0 <= q < p && #[trigger] overlaps(size as int, pg as int, q, base + i) ==> page_open(mask, q)))),
        decreases pages - p,
    {
        let open = (mask >> (p as u16)) & 1 == 1;
        let ghost old = acc;
        let ghost (gfirst, gend) = (0usize, 0usize);
        let m = if open {
            0u64
        } else {
            let (first, end) = page_slots(size, nslots, pg, p);
            proof {
                gfirst = first;
                gend = end;
            }
            // The page's slots inside this word, as word bit positions.
            let lo = if first <= base { 0 } else if first - base >= 64 { 64 } else { first - base };
            let hi = if end <= base { 0 } else if end - base >= 64 { 64 } else { end - base };
            let lo = if lo > hi { hi } else { lo };
            let m = range_mask(lo as u64, hi as u64);
            proof {
                assert forall|i: int| 0 <= i < 64 implies (#[trigger] bit(m, i) <==> (first <= base + i < end)) by {
                    assert(bit(m, i) <==> (lo <= i < hi));
                }
            }
            m
        };
        acc &= !m;
        proof {
            assert(page_open(mask, p as int) <==> open);
            assert forall|i: int| 0 <= i < 64 implies (#[trigger] bit(acc, i) <==> (bit(old, i) && !bit(m, i))) by {
                let iu = i as u64;
                assert((((old & !m) >> iu) & 1u64 == 1u64) == (((old >> iu) & 1u64 == 1u64) && !((m >> iu) & 1u64 == 1u64))) by (bit_vector)
                    requires
                        iu < 64;
            }
            assert forall|i: int| 0 <= i < 64 implies (#[trigger] bit(acc, i) <==> (i < valid && (forall|q: int| 0 <= q < p + 1 && #[trigger] overlaps(size as int, pg as int, q, base + i) ==> page_open(mask, q)))) by {
                assert(bit(old, i) <==> (i < valid && (forall|q: int| 0 <= q < p && #[trigger] overlaps(size as int, pg as int, q, base + i) ==> page_open(mask, q))));
                if open {
                    let iu = i as u64;
                    assert((0u64 >> iu) & 1u64 == 0u64) by (bit_vector);
                    assert(!bit(m, i));
                } else {
                    assert(bit(m, i) <==> (gfirst <= base + i < gend));
                    assert((gfirst <= base + i < gend) <==> (0 <= base + i < nslots && overlaps(size as int, pg as int, p as int, base + i)));
                }
            }
        }
        p += 1;
    }
    acc
}

/// Slot `s` is open under `mask` (`slot_open`): every page it overlaps
/// has its bit set. A slot no larger than a page overlaps at most two.
// The exhaustive test's reference for `OpenView::closed_slots`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn slot_is_open(size: usize, pg: usize, pages: usize, mask: u16, s: usize) -> (r: bool)
    requires
        1 <= size <= 0x10000,
        4096 <= pg <= 0x10000,
        pages <= MAX_MASK_PAGES,
        s < 0x10000,
    ensures
        r == slot_open(size as int, pg as int, pages as int, mask, s as int),
{
    let (a, b) = page_slots(pg, pages, size, s);
    proof {
        assert forall|q: int| #[trigger] overlaps(size as int, pg as int, q, s as int) == overlaps(pg as int, size as int, s as int, q) by {}
    }
    let mut q = a;
    while q < b
        invariant
            a <= q <= b <= pages <= MAX_MASK_PAGES,
            forall|t: int| (a <= t < b) <==> (0 <= t < pages && #[trigger] overlaps(size as int, pg as int, t, s as int)),
            forall|t: int| a <= t < q ==> page_open(mask, t),
        decreases b - q,
    {
        if (mask >> (q as u16)) & 1 != 1 {
            assert(!page_open(mask, q as int));
            assert(overlaps(size as int, pg as int, q as int, s as int));
            return false;
        }
        q += 1;
    }
    true
}

/// The slots below `n` that `mask` closes: those not `slot_open`.
pub open spec fn closed_below(size: int, pg: int, pages: int, mask: u16, n: int) -> nat
    decreases n,
{
    if n <= 0 {
        0
    } else {
        closed_below(size, pg, pages, mask, n - 1) + if slot_open(size, pg, pages, mask, n - 1) { 0nat } else { 1nat }
    }
}

proof fn lemma_closed_below_bound(size: int, pg: int, pages: int, mask: u16, n: int)
    requires
        n >= 0,
    ensures
        closed_below(size, pg, pages, mask, n) <= n,
    decreases n,
{
    if n > 0 {
        lemma_closed_below_bound(size, pg, pages, mask, n - 1);
    }
}

/// Closing pages never reopens a slot: when every page open under `m2`
/// is open under `m1`, `m2` closes at least the slots `m1` does.
pub proof fn lemma_closed_below_mono(size: int, pg: int, pages: int, m1: u16, m2: u16, n: int)
    requires
        forall|q: int| #[trigger] page_open(m2, q) ==> page_open(m1, q),
    ensures
        closed_below(size, pg, pages, m1, n) <= closed_below(size, pg, pages, m2, n),
    decreases n,
{
    if n > 0 {
        lemma_closed_below_mono(size, pg, pages, m1, m2, n - 1);
        if slot_open(size, pg, pages, m2, n - 1) {
            assert(slot_open(size, pg, pages, m1, n - 1));
        }
    }
}

/// How many of the slab's `nslots` slots `mask` closes (`closed_below`):
/// the slots of the closed pages, each straddling slot counted once.
// The exhaustive test's reference for `OpenView::closed_slots`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn closed_slot_count(size: usize, nslots: usize, pg: usize, pages: usize, mask: u16) -> (r: usize)
    requires
        1 <= size <= 0x10000,
        4096 <= pg <= 0x10000,
        pages <= MAX_MASK_PAGES,
        nslots <= 0x10000,
    ensures
        r == closed_below(size as int, pg as int, pages as int, mask, nslots as int),
        r <= nslots,
{
    let mut n: usize = 0;
    let mut s: usize = 0;
    while s < nslots
        invariant
            1 <= size <= 0x10000,
            4096 <= pg <= 0x10000,
            pages <= MAX_MASK_PAGES,
            s <= nslots <= 0x10000,
            n == closed_below(size as int, pg as int, pages as int, mask, s as int),
        decreases nslots - s,
    {
        proof {
            lemma_closed_below_bound(size as int, pg as int, pages as int, mask, s as int);
        }
        if !slot_is_open(size, pg, pages, mask, s) {
            n += 1;
        }
        s += 1;
    }
    proof {
        lemma_closed_below_bound(size as int, pg as int, pages as int, mask, nslots as int);
    }
    n
}

/// Every slot below `n` is open when no page is closed.
pub proof fn lemma_closed_below_all_open(size: int, pg: int, pages: int, n: int)
    requires
        pages <= 16,
    ensures
        closed_below(size, pg, pages, !0u16, n) == 0,
    decreases n,
{
    if n > 0 {
        lemma_closed_below_all_open(size, pg, pages, n - 1);
        assert forall|p: int| 0 <= p < pages && #[trigger] overlaps(size, pg, p, n - 1) implies page_open(!0u16, p) by {
            let pu = p as u16;
            assert((!0u16 >> pu) & 1u16 == 1u16) by (bit_vector)
                requires pu < 16;
        }
    }
}

/// A slab's page geometry and closed-page mask, as a claim or the class
/// lock holder sees them: `closed` bit `p` set is page `p` closed (the
/// complement of `slot_open`'s mask). `pages == 0` is a slab without page
/// state (a class whose descriptor has no `SlabPages`), every slot open.
#[derive(Clone, Copy)]
pub struct OpenView {
    pub size: usize,
    pub nslots: usize,
    pub pg: usize,
    pub pages: usize,
    pub closed: u16,
}

impl OpenView {
    /// No page state: every slot open.
    pub const fn none() -> (r: OpenView)
        ensures
            !r.paged(),
    {
        OpenView { size: 0, nslots: 0, pg: 0, pages: 0, closed: 0 }
    }

    /// The geometry the page functions accept: a slab of two to 16 pages.
    pub open spec fn paged(&self) -> bool {
        &&& 1 <= self.size <= 0x10000
        &&& 4096 <= self.pg <= 0x10000
        &&& 2 <= self.pages <= MAX_MASK_PAGES
        &&& self.nslots <= 0x10000
    }

    /// [`paged`](OpenView::paged), at run time.
    pub fn is_paged(&self) -> (r: bool)
        ensures
            r == self.paged(),
    {
        1 <= self.size && self.size <= 0x10000 && 4096 <= self.pg && self.pg <= 0x10000 && 2 <= self.pages
            && self.pages <= MAX_MASK_PAGES && self.nslots <= 0x10000
    }

    /// Slot `s` is open: every page it overlaps is open.
    pub open spec fn open(&self, s: int) -> bool {
        slot_open(self.size as int, self.pg as int, self.pages as int, !self.closed, s)
    }

    /// The same view with another closed mask.
    pub fn with_closed(&self, closed: u16) -> (r: OpenView)
        ensures
            r == (OpenView { closed, ..*self }),
    {
        OpenView { closed, ..*self }
    }

    /// The bits of word `w` of the slab (slab coordinates: slot `64 * w +
    /// i`) a claim may take: for a paged view, bit `i` of a slot below
    /// `nslots` is set exactly when the slot is open; every other bit is
    /// set, and so is every bit of a view without page state.
    pub fn word(&self, w: usize) -> (r: u64)
        ensures
            self.paged() ==> forall|i: int| 0 <= i < 64 && 64 * w + i < self.nslots ==> (#[trigger] bit(r, i) <==> self.open(64 * w + i)),
            !self.paged() ==> r == !0u64,
    {
        let paged = 1 <= self.size && self.size <= 0x10000 && 4096 <= self.pg && self.pg <= 0x10000 && 2 <= self.pages
            && self.pages <= MAX_MASK_PAGES && self.nslots <= 0x10000;
        if !paged || self.closed == 0 || w >= 1024 {
            proof {
                if paged {
                    assert forall|i: int| 0 <= i < 64 && 64 * w + i < self.nslots implies (#[trigger] bit(!0u64, i) <==> self.open(64 * w + i)) by {
                        let iu = i as u64;
                        assert((!0u64 >> iu) & 1u64 == 1u64) by (bit_vector)
                            requires iu < 64;
                        if self.closed == 0 {
                            let c = self.closed;
                            assert(!c == !0u16) by (bit_vector)
                                requires c == 0u16;
                            assert forall|p: int| 0 <= p < self.pages && #[trigger] overlaps(self.size as int, self.pg as int, p, 64 * w + i)
                                implies page_open(!self.closed, p) by {
                                let pu = p as u16;
                                assert((!0u16 >> pu) & 1u16 == 1u16) by (bit_vector)
                                    requires pu < 16;
                            }
                        } else {
                            // w >= 1024: no slot of the slab is in the word.
                            assert(64 * w + i >= 0x10000);
                        }
                    }
                }
            }
            return !0u64;
        }
        open_slots_word(self.size, self.nslots, self.pg, self.pages, !self.closed, w)
    }

    /// The slots of page `p` ([`page_slots`]), for a paged view.
    pub fn page_slots_of(&self, p: usize) -> (r: (usize, usize))
        requires
            self.paged(),
            p < self.pages,
        ensures
            r.0 <= r.1 <= self.nslots,
            forall|s: int| (r.0 <= s < r.1) <==> (0 <= s < self.nslots && #[trigger] overlaps(self.size as int, self.pg as int, p as int, s)),
    {
        page_slots(self.size, self.nslots, self.pg, p)
    }

    /// The bits of word `w` (slab coordinates) whose slots overlap page
    /// `p`: what the walk reads to find the page closable, and probes
    /// before it discards the page.
    pub fn page_word(&self, p: usize, w: usize) -> (r: u64)
        requires
            self.paged(),
            p < self.pages,
            w < 1024,
        ensures
            forall|i: int| 0 <= i < 64 ==> (#[trigger] bit(r, i) <==> (64 * w + i < self.nslots
                && overlaps(self.size as int, self.pg as int, p as int, 64 * w + i))),
    {
        let (first, end) = page_slots(self.size, self.nslots, self.pg, p);
        let base = 64 * w;
        let lo = if first <= base { 0 } else if first - base >= 64 { 64 } else { first - base };
        let hi = if end <= base { 0 } else if end - base >= 64 { 64 } else { end - base };
        let lo = if lo > hi { hi } else { lo };
        let m = range_mask(lo as u64, hi as u64);
        proof {
            assert forall|i: int| 0 <= i < 64 implies (#[trigger] bit(m, i) <==> (64 * w + i < self.nslots
                && overlaps(self.size as int, self.pg as int, p as int, 64 * w + i))) by {
                assert(bit(m, i) <==> (lo <= i < hi));
                assert((first <= base + i < end) <==> (0 <= base + i < self.nslots && overlaps(self.size as int, self.pg as int, p as int, base + i)));
            }
        }
        m
    }

    /// The slots the closed pages hide: a straddling slot counts once
    /// (exactly [`closed_below`], which `closed_slot_count` proves and the
    /// exhaustive test checks this against). 0 for a view without page
    /// state. The union of the closed pages' slot ranges, which come in
    /// slot order, so at most 16 steps: a refill computes it.
    pub fn closed_slots(&self) -> (r: usize)
        ensures
            !self.paged() ==> r == 0,
            r <= self.nslots,
    {
        if !self.is_paged() || self.closed == 0 {
            return 0;
        }
        let mut total: usize = 0;
        let mut covered: usize = 0;
        let mut p: usize = 0;
        while p < self.pages
            invariant
                self.paged(),
                p <= self.pages,
                total <= covered <= self.nslots,
            decreases self.pages - p,
        {
            if (self.closed >> (p as u16)) & 1 == 1 {
                let (first, end) = self.page_slots_of(p);
                let lo = if first > covered { first } else { covered };
                if end > lo {
                    total += end - lo;
                    covered = end;
                }
            }
            p += 1;
        }
        total
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LG_MIN_SLAB, LG_SMALL_SLAB, classes_for_page};
    use std::vec;
    use std::vec::Vec;

    /// Brute force: slot `s` is open when no closed page shares a byte
    /// with it, page and slot as byte ranges.
    fn open_ref(size: usize, pg: usize, pages: usize, mask: u16, s: usize) -> bool {
        (0..pages).all(|p| {
            let overlap = s * size < (p + 1) * pg && p * pg < (s + 1) * size;
            !overlap || mask >> p & 1 == 1
        })
    }

    fn masks(pages: usize, seed: &mut u64) -> Vec<u16> {
        let full = ((1u32 << pages) - 1) as u16;
        if pages <= 6 {
            return (0..=full).collect();
        }
        let mut v = vec![0, full];
        for p in 0..pages {
            v.push(1 << p);
            v.push(full & !(1 << p));
        }
        for _ in 0..64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            v.push(*seed as u16 & full);
        }
        v
    }

    /// Every class of every page size whose slab has at most 16 pages,
    /// under every mask (six pages or fewer) or the all, none, single
    /// and random masks: `page_slots`, `open_slots_word`, `slot_is_open`
    /// and `closed_slot_count` agree with the byte-range reference.
    #[test]
    fn page_geometry_matches_brute_force() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut checked = 0usize;
        for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
            let pg = 1usize << lg_page;
            for cls in classes_for_page(lg_page).iter() {
                let (size, nslots) = (cls.size(), cls.nslots());
                let pages = cls.slab_size() / pg;
                if pages > MAX_MASK_PAGES {
                    continue;
                }
                for p in 0..pages {
                    let (first, end) = page_slots(size, nslots, pg, p);
                    let want: Vec<usize> = (0..nslots)
                        .filter(|&s| s * size < (p + 1) * pg && p * pg < (s + 1) * size)
                        .collect();
                    assert_eq!(
                        (first..end).collect::<Vec<_>>(),
                        want,
                        "{size} B page {p} of {pg}"
                    );
                }
                let words = nslots.div_ceil(64);
                for mask in masks(pages, &mut seed) {
                    let mut closed = 0;
                    for s in 0..nslots {
                        let open = open_ref(size, pg, pages, mask, s);
                        assert_eq!(
                            slot_is_open(size, pg, pages, mask, s),
                            open,
                            "{size} B slot {s} mask {mask:#x}"
                        );
                        closed += usize::from(!open);
                    }
                    assert_eq!(
                        closed_slot_count(size, nslots, pg, pages, mask),
                        closed,
                        "{size} B mask {mask:#x}"
                    );
                    let view = OpenView {
                        size,
                        nslots,
                        pg,
                        pages,
                        closed: !mask & ((1u32 << pages) - 1) as u16,
                    };
                    if pages >= 2 {
                        assert_eq!(
                            view.closed_slots(),
                            closed,
                            "{size} B mask {mask:#x}: the range union"
                        );
                    }
                    for w in 0..words + 1 {
                        let want = (0..64)
                            .filter(|&i| {
                                64 * w + i < nslots && open_ref(size, pg, pages, mask, 64 * w + i)
                            })
                            .fold(0u64, |a, i| a | 1 << i);
                        assert_eq!(
                            open_slots_word(size, nslots, pg, pages, mask, w),
                            want,
                            "{size} B word {w} mask {mask:#x}"
                        );
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 1000, "{checked}");
    }
}
