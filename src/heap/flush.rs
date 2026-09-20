//! The flush's grouping pass: order a released batch by slab so
//! `release_owned` sees each slab's slots contiguous, and the
//! straddling double-free abort.

use crate::layout::CLASSES;

use super::Shared;

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
