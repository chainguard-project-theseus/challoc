//! Refill and flush: the batches between a cache and its slabs. The
//! refill claims a batch into an empty cache; the flush releases a
//! cache's highest addresses grouped by slab ([`group_by_slab`]) and
//! aborts on a double free that straddles the split.

use core::ptr;

use crate::arena::Arena;
use crate::layout::CLASSES;
use crate::sys::pac;

use super::shared::seed;
use super::{CacheHeader, FOREIGN, Heap, Shared};

impl Heap {
    /// The batch claim behind a refill or a run rebuild: `cs.claim`, or
    /// with `slot_randomize` `cs.claim_random` with this heap's xorshift
    /// (the scan starts at a random bitmap word and the batch is shuffled;
    /// design section 4). The key is read from the configuration here, on
    /// the slow path, so the fast paths carry nothing for it.
    #[inline]
    pub(super) fn claim(
        &mut self,
        shared: &Shared,
        cs: &crate::class::ClassState,
        out: &mut [*mut u8],
    ) -> crate::class::Claim {
        if shared.config.slot_randomize {
            let rng = self.rng();
            cs.claim_random(&shared.meta, out, rng)
        } else {
            cs.claim(&shared.meta, out)
        }
    }

    /// This heap's xorshift state for `slot_randomize`, seeded on first use
    /// from `getrandom` (mixed with the heap's address and thread id so two
    /// heaps seeded in the same instant differ even if the kernel returned
    /// the same bytes, and never zero, which xorshift cannot leave).
    #[cold]
    #[inline(never)]
    pub(super) fn rng(&mut self) -> &mut u64 {
        if self.rng == 0 {
            let mut seed = seed();
            seed ^= (self as *const Heap as u64).rotate_left(17) ^ ((self.tid as u64) << 32);
            if seed == 0 {
                seed = 0x9E37_79B9_7F4A_7C15;
            }
            self.rng = seed;
        }
        &mut self.rng
    }

    /// Claim up to `cap / 2` slots of `class` in `arena` into an empty
    /// cache, lowest address on top so allocations stay address-ordered;
    /// the batch is the cache's run. Returns the new length.
    ///
    /// `low_water` restarts at the batch length rather than zero: the run
    /// needs `run_end <= low_water`, and a GC that then finds the batch
    /// untouched releases three quarters of it, which is what the GC is
    /// for (design 6.4).
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and is empty:
    /// callers arrive from a fast path that fell through on `len == 0`,
    /// from `ensure_cache` on a fresh header, or after re-reading `len`
    /// following a `tick`. The `assert_eq!` below is the abort on a heap
    /// inconsistency, not the contract.
    pub(super) unsafe fn refill(
        &mut self,
        shared: &Shared,
        arena: &Arena,
        cache: *mut CacheHeader,
        class: usize,
    ) -> u32 {
        // SAFETY: the cache is empty and has `cap` slots of storage.
        unsafe {
            assert_eq!((*cache).len, 0, "heap: refill of a cache that is not empty");
            let want = ((*cache).cap / 2).max(1) as usize;
            let out = core::slice::from_raw_parts_mut((*cache).slots(), want);
            let claim = self.claim(shared, arena.class(class), out);
            arena.class(class).note_refill(claim.n);
            out[..claim.n].reverse();
            if shared.pac() {
                CacheHeader::sign_all((*cache).slots(), &mut out[..claim.n]);
            }
            (*cache).len = claim.n as u32;
            (*cache).moved = (*cache).moved.wrapping_add(claim.n as u64);
            (*cache).low_water = claim.n as u32;
            (*cache).run_end = claim.n as u32;
            (*cache).zeroed = if claim.zeroed { claim.n as u32 } else { 0 };
            claim.n as u32
        }
    }

    /// Release the `n` highest-address entries of a cache to their slabs
    /// (in `arena`, which owns them) in one batch grouped by slab and keep
    /// the rest. Selecting by address sends the holes to the high slabs,
    /// which is what lets them drain (design 6.4). One `sort_unstable` of
    /// the whole slot array per flush would be 8..9% of glibc-simple and
    /// 5% of the Python json row under tiny-object churn on both VMs, the
    /// largest single term wherever challoc trailed mimalloc or tcmalloc
    /// by more than 10%, so the ordering work is the least that keeps the release grouped:
    ///
    /// - A slot array already in address order (a cache pushed in the order
    ///   a producer's refill batch was handed out, xmalloc-test's consumer
    ///   side, where even a sort's O(n) presorted path is about 1.25k
    ///   cycles per flush) is split directly: `is_sorted` is one pass that
    ///   stops at the first inversion, the released half is sorted and so
    ///   grouped, and the kept half stays sorted for the next flush.
    /// - Otherwise `select_nth_unstable` on the address (O(len), in place)
    ///   puts the `n` highest at the top, and [`group_by_slab`] groups them
    ///   by slab (a counting pass over the slab numbers, O(n); nothing when
    ///   they are one slab); `release_owned` needs nothing more.
    /// - The kept half is left as the selection leaves it, except on the
    ///   first two consecutive unsorted flushes of a cache and every 32nd
    ///   after that (`unsorted_flushes`), when it is sorted: a cache pushed
    ///   in order that met one unsorted flush (its first fill, a producer
    ///   moving to a lower slab) gets back onto the sorted path, where a
    ///   scrambled kept half would have kept it on the selection forever
    ///   (xmalloc-test +16% that way; a full sort re-establishes the order
    ///   at every flush), while a cache pushed in arbitrary order pays two
    ///   sorts of half the array and then one in 32 (the unconditional
    ///   kept sort cost glibc-simple and Python their whole gain). The run
    ///   is dropped in every case and rebuilt by the scheduled pop (design
    ///   6.4); the zero prefix is given up.
    ///
    /// Candidates rejected, all in the results file: a selection followed
    /// by a sort of the released half (the selection scrambles it, so that
    /// is a full sort: xmalloc +51%); an in-place bucket pass over the
    /// whole array by address (about 10 cycles per entry, a full sort's
    /// cost on random input and ten times its cost on sorted input: xmalloc
    /// +49%); the selection alone (xmalloc +21%, no self-healing).
    ///
    /// Double frees. `release_owned` aborts on a duplicate inside the
    /// released batch (a slab's slots are contiguous after the bucket pass
    /// and land on one scratch bitmap, so the second copy hits a set bit; a
    /// duplicate in two groups of one slab hits a clear bit in the class
    /// bitmap). A pair kept together stays cached and consistent (both
    /// copies in the cache, the bit set) until a flush releases it. The
    /// pair that straddles the split would have one copy released, its bit
    /// cleared, and the other still cached to be handed out over a slot a
    /// refill can claim again: after the split every kept entry is at most
    /// `kept_max = slots[rest_len - 1]` and every released entry at least
    /// that, so a straddling pair is exactly a released entry equal to
    /// `kept_max`, and the lowest released address (found by the bucket
    /// pass's first sweep, or `slots[rest_len]` on the sorted path) is
    /// compared with it. One compare, the same strength as the sorted
    /// form's neighbour compare at the split; a pass over the kept half was
    /// measured at 4.9% / 7.3% of xmalloc-test and is not done.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and `n <= len`.
    pub(super) unsafe fn flush(
        &mut self,
        shared: &Shared,
        arena: u16,
        cache: *mut CacheHeader,
        class: usize,
        n: usize,
    ) {
        if n == 0 {
            return;
        }
        // SAFETY: `n <= len <= cap` entries exist.
        unsafe {
            let pac = shared.pac();
            let slots = (*cache).slots();
            // With signed entries the selection, the sortedness test and
            // the kept sort order by the address bits alone (`!pac::mask()`
            // masks the PAC field, one `and` per compare); only the
            // released half is authenticated, once, before the release,
            // and the kept half stays as it is. Authenticating the whole
            // stack and re-signing the kept half was measured first:
            // xmalloc-test -15.8% on the C4A (the consumer's flushes are
            // the sorted path, 1.25k cycles each, and the two passes
            // doubled them).
            // The keyed forms only when signed: `sort_unstable_by_key` and
            // friends take a comparator closure and lose the primitive
            // pointer sort's specialisations, which costs every flush even
            // when the key is the identity. The mask itself only when
            // signed: `pac::mask` is an `xpacd`, UNDEFINED on a CPU without
            // the extension (`SIGILL` on Neoverse N1, the Altra), and no
            // PAC instruction may run unless the probe said yes.
            let addr_mask = if pac { !pac::mask() } else { usize::MAX };
            let key = |e: &*mut u8| (*e as usize) & addr_mask;
            let all = (*cache).entries();
            let rest_len = all.len() - n;
            let mut kept_max = ptr::null_mut();
            let sorted = rest_len > 0
                && if pac {
                    all.is_sorted_by_key(key)
                } else {
                    all.is_sorted()
                };
            // The counting split did the grouping and the straddle compare.
            let mut split = false;
            if sorted {
                // Already in address order: the split is direct, the
                // released half is sorted and so grouped, and the kept half
                // stays sorted for the next flush.
                kept_max = *all.get_unchecked(rest_len - 1);
                (*cache).unsorted_flushes = 0;
            } else {
                if rest_len > 0 {
                    let scratch = self.flush_scratch(shared, (*cache).cap);
                    if !scratch.is_null() {
                        let mut split_by = CountingSplit::new(
                            scratch,
                            all.len(),
                            shared.regions.class_base[class],
                            CLASSES[class].lg_slab as u32,
                            pac,
                            addr_mask,
                        );
                        let dup = split_by.split(all, rest_len);
                        if !dup.is_null() {
                            straddling_double_free(dup, class);
                        }
                        split = true;
                    }
                    if !split {
                        // `all[..rest_len - 1] <= all[rest_len - 1] <= all[rest_len..]`.
                        if pac {
                            all.select_nth_unstable_by_key(rest_len - 1, key);
                        } else {
                            all.select_nth_unstable(rest_len - 1);
                        }
                        kept_max = *all.get_unchecked(rest_len - 1);
                    }
                }
                let k = (*cache).unsorted_flushes;
                (*cache).unsorted_flushes = k.wrapping_add(1);
                if rest_len > 1 && (k < 2 || k % 32 == 0) {
                    if pac {
                        all.get_unchecked_mut(..rest_len).sort_unstable_by_key(key);
                    } else {
                        all.get_unchecked_mut(..rest_len).sort_unstable();
                    }
                }
            }
            let high = all.get_unchecked_mut(rest_len..);
            if pac {
                // The release needs plain pointers. A corrupted entry
                // faults here (FEAT_FPAC) or in `release_owned`.
                CacheHeader::auth_all(slots, high);
            }
            if !split {
                let low = if sorted {
                    *high.get_unchecked(0)
                } else {
                    group_by_slab(shared, class, high)
                };
                let kept_max_plain = if pac && rest_len > 0 {
                    pac::auth(kept_max as usize, slots as usize) as *mut u8
                } else {
                    kept_max
                };
                if low == kept_max_plain {
                    straddling_double_free(low, class);
                }
            }
            // The `FOREIGN` cache's batch starts wherever its first slab
            // belongs; `release_routed` follows the owner words from there.
            let owner = if arena == FOREIGN {
                shared.arena_of_slot(class, *high.get_unchecked(0))
            } else {
                arena
            };
            shared.release_routed(owner, class, high);
            (*cache).len = rest_len as u32;
            (*cache).moved = (*cache).moved.wrapping_sub(n as u64);
            (*cache).zeroed = 0;
            (*cache).run_end = 0;
            (*cache).low_water = (*cache).low_water.min(rest_len as u32);
        }
    }
}

impl Heap {
    /// This heap's flush scratch buffer ([`scratch_layout`]) with room for
    /// `cap` entries, allocated (or grown) on first need; null when the
    /// metadata region cannot provide one, and the flush then selects in
    /// place.
    #[inline]
    fn flush_scratch(&mut self, shared: &Shared, cap: u32) -> *mut u8 {
        if self.scratch_cap < cap {
            self.grow_scratch(shared, cap);
        }
        self.scratch
    }

    #[cold]
    #[inline(never)]
    fn grow_scratch(&mut self, shared: &Shared, cap: u32) {
        let p = shared.meta.alloc(scratch_layout(cap));
        if p.is_null() {
            return;
        }
        self.free_scratch(shared);
        self.scratch = p;
        self.scratch_cap = cap;
    }

    /// Free the flush scratch buffer, if any.
    pub(super) fn free_scratch(&mut self, shared: &Shared) {
        if self.scratch_cap != 0 {
            shared
                .meta
                .free(self.scratch, scratch_layout(self.scratch_cap));
            self.scratch = ptr::null_mut();
            self.scratch_cap = 0;
        }
    }
}

/// log2 of the buckets of one [`CountingSplit`] pass.
const LG_SPLIT_BUCKETS: u32 = 8;
const SPLIT_BUCKETS: usize = 1 << LG_SPLIT_BUCKETS;
/// Independent counter tables of a [`CountingSplit`] pass: consecutive
/// entries count into different tables, so a run of entries in one bucket
/// (a cache pushed in address order) is not one chain of increments
/// through a single counter in memory.
const SPLIT_LANES: usize = 4;
type SplitCounts = [[u32; SPLIT_BUCKETS]; SPLIT_LANES];
/// Slices this short are sorted outright.
const SPLIT_SORT_BELOW: usize = 24;

/// The flush scratch buffer of a heap: the counter tables, then `cap`
/// entries for a counting sort's output. Kept off the stack, which a
/// `free` on a small signal stack cannot spare.
pub(super) const fn scratch_layout(cap: u32) -> core::alloc::Layout {
    let size = core::mem::size_of::<SplitCounts>() + 8 * cap as usize;
    match core::alloc::Layout::from_size_align(size, 16) {
        Ok(l) => l,
        Err(_) => panic!("scratch layout"),
    }
}

/// The flush's split by a most-significant-digit counting sort on the
/// address. [`CountingSplit::split`] moves the highest addresses of a
/// slice to its top grouped by slab and the rest below them, and finds a
/// slot cached on both sides of the split.
///
/// One pass over a slice: a sweep finds its lowest and highest address
/// (relative to the class base, PAC field masked); the bucket width is the
/// smallest power of two that fits that span in [`SPLIT_BUCKETS`]; a
/// counting sweep, a prefix sum and a scatter into the scratch buffer order
/// the slice by bucket, and it is copied back. Then only two kinds of
/// bucket need more: the one holding the split, which gets a pass of its
/// own, and, while the bucket width is wider than a slab, every released
/// bucket holding more than one entry, which gets a pass that only groups.
/// A bucket of the width of a slab or less lies in one slab. Every pass
/// narrows the span at least `2^(LG_SPLIT_BUCKETS - 1)`-fold, so the
/// recursion is a handful of levels deep; slices shorter than
/// [`SPLIT_SORT_BELOW`] are sorted instead.
pub(super) struct CountingSplit<'a> {
    counts: &'a mut SplitCounts,
    scratch: &'a mut [*mut u8],
    base: usize,
    lg_slab: u32,
    pac: bool,
    mask: usize,
}

impl<'a> CountingSplit<'a> {
    /// A split over the scratch buffer `buf` ([`scratch_layout`] of at
    /// least `len` entries) for one class.
    ///
    /// # Safety
    /// `buf` is a live, exclusively owned scratch buffer of at least `len`
    /// entries.
    pub(super) unsafe fn new(
        buf: *mut u8,
        len: usize,
        base: usize,
        lg_slab: u32,
        pac: bool,
        mask: usize,
    ) -> Self {
        // SAFETY: the caller's buffer: the counter tables, then `len`
        // entries, 16-aligned.
        unsafe {
            CountingSplit {
                counts: &mut *(buf as *mut SplitCounts),
                scratch: core::slice::from_raw_parts_mut(
                    buf.add(core::mem::size_of::<SplitCounts>()) as *mut *mut u8,
                    len,
                ),
                base,
                lg_slab,
                pac,
                mask,
            }
        }
    }

    #[inline(always)]
    fn key(&self, p: *mut u8) -> usize {
        ((p as usize) & self.mask).wrapping_sub(self.base)
    }

    /// Order `v` so that `v[..keep]` are its `keep` lowest addresses and
    /// `v[keep..]` the rest grouped by slab in ascending order. Returns the
    /// address of a slot found on both sides of the split, or null.
    pub(super) fn split(&mut self, v: &mut [*mut u8], keep: usize) -> *mut u8 {
        let len = v.len();
        debug_assert!(keep <= len && len <= self.scratch.len());
        if len < SPLIT_SORT_BELOW {
            if self.pac {
                let mask = self.mask;
                v.sort_unstable_by_key(|&p| (p as usize) & mask);
            } else {
                v.sort_unstable();
            }
            if keep > 0 && keep < len && self.key(v[keep - 1]) == self.key(v[keep]) {
                return self.key(v[keep]).wrapping_add(self.base) as *mut u8;
            }
            return ptr::null_mut();
        }
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for &p in v.iter() {
            let a = self.key(p);
            lo = lo.min(a);
            hi = hi.max(a);
        }
        if lo == hi {
            // One address `len` times: kept and released both hold it
            // unless the split takes none or all.
            if keep > 0 && keep < len {
                return lo.wrapping_add(self.base) as *mut u8;
            }
            return ptr::null_mut();
        }
        if keep == 0 && lo >> self.lg_slab == hi >> self.lg_slab {
            // Only grouping asked, and one slab already is one group.
            return ptr::null_mut();
        }
        let mut sh = (usize::BITS - (hi - lo).leading_zeros()).saturating_sub(LG_SPLIT_BUCKETS);
        if (hi >> sh) - (lo >> sh) >= SPLIT_BUCKETS {
            sh += 1;
        }
        let b0 = lo >> sh;
        let nb = (hi >> sh) - b0 + 1;
        let (base, mask) = (self.base, self.mask);
        // `& (SPLIT_BUCKETS - 1)` is the identity (every bucket is below
        // `nb`) and drops the bounds checks.
        let bucket = move |p: *mut u8| {
            ((((p as usize) & mask).wrapping_sub(base) >> sh) - b0) & (SPLIT_BUCKETS - 1)
        };
        for t in self.counts.iter_mut() {
            t[..nb].fill(0);
        }
        let mut quads = v.chunks_exact(SPLIT_LANES);
        for q in &mut quads {
            for (l, &p) in q.iter().enumerate() {
                self.counts[l][bucket(p)] += 1;
            }
        }
        for &p in quads.remainder() {
            self.counts[0][bucket(p)] += 1;
        }
        // The bucket holding the split, `s`, with `lo_s` entries below
        // it and `count_s` in it: `lo_s <= keep < lo_s + count_s`, or
        // none when `keep` is `len`.
        let total = |c: &SplitCounts, b: usize| -> usize {
            (c[0][b] + c[1][b] + c[2][b] + c[3][b]) as usize
        };
        let (mut s, mut lo_s, mut count_s) = (nb, len, 0);
        if keep < len {
            s = nb - 1;
            loop {
                count_s = total(self.counts, s);
                lo_s -= count_s;
                if lo_s <= keep {
                    break;
                }
                s -= 1;
            }
        }
        let mut acc = 0u32;
        for b in 0..nb {
            for t in self.counts.iter_mut() {
                let c = t[b];
                t[b] = acc;
                acc += c;
            }
        }
        let mut quads = v.chunks_exact(SPLIT_LANES);
        for q in &mut quads {
            for (l, &p) in q.iter().enumerate() {
                let b = bucket(p);
                let i = self.counts[l][b];
                self.counts[l][b] = i + 1;
                // SAFETY: the prefix sums place every entry below `len`.
                unsafe { *self.scratch.get_unchecked_mut(i as usize) = p };
            }
        }
        for &p in quads.remainder() {
            let b = bucket(p);
            let i = self.counts[0][b];
            self.counts[0][b] = i + 1;
            // SAFETY: as above.
            unsafe { *self.scratch.get_unchecked_mut(i as usize) = p };
        }
        v.copy_from_slice(&self.scratch[..len]);
        // The counters are free again: the passes below use them.
        if s < nb && keep > lo_s {
            let dup = self.split(&mut v[lo_s..lo_s + count_s], keep - lo_s);
            if !dup.is_null() {
                return dup;
            }
        } else {
            // `keep` is on a bucket boundary: the split bucket, if any,
            // is released whole.
            lo_s = keep;
            count_s = 0;
        }
        if sh > self.lg_slab {
            // Buckets wider than a slab: group each released one.
            let mut i = lo_s + count_s;
            while i < len {
                let b = bucket(v[i]);
                let mut j = i + 1;
                while j < len && bucket(v[j]) == b {
                    j += 1;
                }
                if j - i > 1 {
                    self.split(&mut v[i..j], 0);
                }
                i = j;
            }
        }
        ptr::null_mut()
    }
}

/// Buckets of [`group_by_slab`]'s counting pass: a batch whose slab
/// numbers span fewer than this many slabs is grouped in one in-place
/// pass; a batch spread wider is sorted instead.
const GROUP_BUCKETS: usize = 256;

/// Group a flush's released batch by slab, in place, and return its lowest
/// address. `release_owned` needs a slab's slots contiguous (one owner
/// check, one descriptor update and one bitmap pass per slab) and nothing
/// else, so the pass is a counting sort on the slab number, `(p - base) >>
/// lg_slab` (design 3's geometry): one sweep finds the lowest and highest
/// slab number (and the batch's minimum address); a batch from one slab is
/// done; one spanning fewer than [`GROUP_BUCKETS`] slabs is permuted by
/// [`flag_sort`] on `slab - lowest`; anything wider, a cache holding slots
/// of more than 256 slabs' span, falls back to `sort_unstable` on the
/// address. Every path is O(n) but the last, a full sort of half the
/// entries.
pub(super) fn group_by_slab(shared: &Shared, class: usize, high: &mut [*mut u8]) -> *mut u8 {
    let base = shared.regions.class_base[class];
    let lg = CLASSES[class].lg_slab as u32;
    let key = |p: *mut u8| (p as usize).wrapping_sub(base) >> lg;
    let mut low = high[0];
    let (mut kmin, mut kmax) = (usize::MAX, 0usize);
    for &p in high.iter() {
        let k = key(p);
        kmin = kmin.min(k);
        kmax = kmax.max(k);
        if p < low {
            low = p;
        }
    }
    if kmin == kmax {
        return low;
    }
    let span = kmax - kmin + 1;
    if span <= GROUP_BUCKETS {
        flag_sort(high, span, |p| key(p) - kmin);
    } else {
        high.sort_unstable();
    }
    low
}

/// American flag sort: permute `v` in place so that entries with equal
/// `key` (below `nb`, at most [`GROUP_BUCKETS`]) are contiguous, buckets
/// in ascending key order. One counting sweep, a prefix sum, and one
/// cycle-following sweep that moves every entry once: O(len + nb), no
/// scratch beyond the bucket cursors.
pub(super) fn flag_sort(v: &mut [*mut u8], nb: usize, key: impl Fn(*mut u8) -> usize) {
    debug_assert!(nb <= GROUP_BUCKETS && v.len() <= u16::MAX as usize);
    let mut next = [0u16; GROUP_BUCKETS];
    let mut end = [0u16; GROUP_BUCKETS];
    for &p in v.iter() {
        end[key(p)] += 1;
    }
    let mut acc = 0u16;
    for b in 0..nb {
        next[b] = acc;
        acc += end[b];
        end[b] = acc;
    }
    for b in 0..nb {
        while next[b] < end[b] {
            let i = next[b] as usize;
            let mut p = v[i];
            let mut kb = key(p);
            while kb != b {
                let j = next[kb] as usize;
                next[kb] += 1;
                core::mem::swap(&mut p, &mut v[j]);
                kb = key(p);
            }
            v[i] = p;
            next[b] += 1;
        }
    }
}

/// Two copies of one slot in a cache at flush time, one on each side of
/// the split (`Heap::flush`): the flush would have released one and kept
/// the other.
#[cold]
#[inline(never)]
pub(super) fn straddling_double_free(p: *mut u8, class: usize) -> ! {
    panic!(
        "double free of {:#x} (class {class}): both copies cached, one at the flush boundary",
        p as usize
    )
}
