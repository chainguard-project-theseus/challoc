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
    pub(super) fn claim(&mut self, shared: &Shared, cs: &crate::class::ClassState, out: &mut [*mut u8]) -> crate::class::Claim {
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
    pub(super) unsafe fn refill(&mut self, shared: &Shared, arena: &Arena, cache: *mut CacheHeader, class: usize) -> u32 {
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
    /// by more than 10% (`bench/results/2026-09-19-*-gce-flush-sort.md`),
    /// so the ordering work is the least that keeps the release grouped:
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
    /// measured at 4.9% / 7.3% of xmalloc-test and is not done
    /// (`bench/results/2026-09-18-*-gce-detection-gaps.md`).
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and `n <= len`.
    pub(super) unsafe fn flush(&mut self, shared: &Shared, arena: u16, cache: *mut CacheHeader, class: usize, n: usize) {
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
            // doubled them; `bench/results/2026-09-19-lane-a-pac-pointers.md`).
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
            let kept_max;
            let sorted = rest_len > 0 && if pac { all.is_sorted_by_key(key) } else { all.is_sorted() };
            if sorted {
                // Already in address order: the split is direct, the
                // released half is sorted and so grouped, and the kept half
                // stays sorted for the next flush.
                kept_max = *all.get_unchecked(rest_len - 1);
                (*cache).unsorted_flushes = 0;
            } else {
                kept_max = if rest_len > 0 {
                    // `all[..rest_len - 1] <= all[rest_len - 1] <= all[rest_len..]`.
                    if pac {
                        all.select_nth_unstable_by_key(rest_len - 1, key);
                    } else {
                        all.select_nth_unstable(rest_len - 1);
                    }
                    *all.get_unchecked(rest_len - 1)
                } else {
                    ptr::null_mut()
                };
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
            let low = if sorted { *high.get_unchecked(0) } else { group_by_slab(shared, class, high) };
            let kept_max_plain = if pac && rest_len > 0 { pac::auth(kept_max as usize, slots as usize) as *mut u8 } else { kept_max };
            if low == kept_max_plain {
                straddling_double_free(low, class);
            }
            // The `FOREIGN` cache's batch starts wherever its first slab
            // belongs; `release_routed` follows the owner words from there.
            let owner = if arena == FOREIGN { shared.arena_of_slot(class, *high.get_unchecked(0)) } else { arena };
            shared.release_routed(owner, class, high);
            (*cache).len = rest_len as u32;
            (*cache).moved = (*cache).moved.wrapping_sub(n as u64);
            (*cache).zeroed = 0;
            (*cache).run_end = 0;
            (*cache).low_water = (*cache).low_water.min(rest_len as u32);
        }
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
    panic!("double free of {:#x} (class {class}): both copies cached, one at the flush boundary", p as usize)
}
