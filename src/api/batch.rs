//! `challoc_free_batch` (proof of concept, not verified): free many
//! pointers in one call, released straight to their slabs instead of
//! pushed through the thread cache one by one.
//!
//! The thread cache's flush groups what it releases by slab, but a flush
//! releases at most half a cache (512 slots for the small classes), so a
//! burst of frees scattered over more slabs than that (a large hash table
//! torn down in bucket order) releases one or two slots per slab per flush
//! and pays the flush's address sort for every half cache. A batch spans as
//! many slabs as the caller hands over, so it sorts once and releases each
//! slab's slots together.
//!
//! The checks are `free`'s, on every pointer:
//! - each pointer is classified with the exact test of `free`
//!   (`Shared::classify_free`: a slot start of a handed-out slab whose bit is
//!   set, or the start of an active huge run; anything else aborts);
//! - the release then clears each bit with the double-free check of a cache
//!   flush (`ClassState::release_owned`: a duplicate in the batch hits the
//!   scratch bit, an already-clear bit aborts, the owner word is checked
//!   against the descriptor);
//! - a huge run is freed by `free` itself.
//! With `double_free_scan`, `zero_on_free` or `pac_pointers` set, or during
//! a fork epoch, or for a short batch, every pointer goes through `free`
//! instead, so those configurations keep their own per-free checks
//! unchanged.
//!
//! The caller's array is reordered (sorted by address); its contents after
//! the call are unspecified.

use crate::heap::{FREE_ROUTE, Shared, existing_heap, shared_ptr};
use crate::layout::{CLASSES, NCLASSES};
use vstd::prelude::Tracked;

/// Below this many pointers a batch goes through `free` one by one: the
/// slots then stay in the thread cache for the allocations that follow.
pub const BATCH_MIN: usize = 64;

/// `free` of one pointer, returning its usable size (0 for null).
fn free_one(p: *mut u8) -> usize {
    super::libc::free_usable(
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    )
}

/// Whether the configuration changes what a free does beyond the bit test
/// and the release: the hardening keys and a fork epoch.
fn needs_free(shared: &Shared) -> bool {
    shared.free_flags() & !FREE_ROUTE != 0
}

/// Free every pointer of `ptrs` (nulls are skipped) and return the sum of
/// their usable sizes. `ptrs` is reordered.
pub fn free_batch(ptrs: &mut [*mut u8]) -> usize {
    let shared = match super::shared_if_ready() {
        Some(s) if ptrs.len() >= BATCH_MIN && !needs_free(s) => s,
        _ => return ptrs.iter().map(|&p| free_one(p)).sum(),
    };
    // Sorted as integers: an instance of `core`'s sort of its own, so the
    // flush's selection over `*mut u8` keeps its code.
    // SAFETY: `*mut u8` and `usize` have the same size and alignment, and
    // every bit pattern of one is a valid value of the other.
    unsafe { core::slice::from_raw_parts_mut(ptrs.as_mut_ptr() as *mut usize, ptrs.len()) }
        .sort_unstable();
    let mut total = 0usize;
    let mut freed = [0u32; NCLASSES];
    // The current run of slots of one class: `ptrs[start..i]`.
    let mut start = 0;
    let mut run: Option<(usize, u16)> = None;
    let mut i = 0;
    while i < ptrs.len() {
        let p = ptrs[i];
        let c = if p.is_null() {
            None
        } else {
            match shared.classify_free(p, Tracked::assume_new()) {
                Ok(ca) => Some(ca),
                Err(_) => None,
            }
        };
        let same = matches!((run, c), (Some((rc, _)), Some((cc, _))) if rc == cc);
        if !same {
            if let Some((rc, ra)) = run.take() {
                release(shared, ra, rc, &ptrs[start..i]);
            }
            match c {
                Some(ca) => {
                    run = Some(ca);
                    start = i;
                }
                // Null or a huge run: through `free`, outside any run.
                None => total += free_one(p),
            }
        }
        if let Some((cc, _)) = c {
            total += CLASSES.table()[cc].size();
            freed[cc] += 1;
        }
        i += 1;
    }
    if let Some((rc, ra)) = run {
        release(shared, ra, rc, &ptrs[start..]);
    }
    if let Some(h) = existing_heap(shared_ptr(shared)) {
        for (c, &n) in freed.iter().enumerate() {
            for _ in 0..n {
                h.count_direct_free(c);
            }
        }
    }
    total
}

/// Release a sorted run of slots of `class`, the first owned by `arena`.
fn release(shared: &Shared, arena: u16, class: usize, run: &[*mut u8]) {
    if run.is_empty() {
        return;
    }
    crate::heap::batch::release_run(shared, arena, class, run);
}

#[cfg(not(test))]
mod exports {
    use core::ffi::c_void;

    /// `challoc_free_batch(ptrs, n)`: free `ptrs[0..n]`, returning the sum
    /// of their usable sizes (`challoc_free_usable`'s figure). `ptrs` is
    /// reordered.
    ///
    /// # Safety
    ///
    /// `ptrs` points to `n` writable pointers, each null or a live block
    /// the caller gives up, as for `free`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn challoc_free_batch(ptrs: *mut *mut c_void, n: usize) -> usize {
        if n == 0 || ptrs.is_null() {
            return 0;
        }
        // SAFETY: the caller hands over `n` writable pointers at `ptrs`.
        let v = unsafe { core::slice::from_raw_parts_mut(ptrs as *mut *mut u8, n) };
        super::free_batch(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn malloc(n: usize) -> *mut u8 {
        super::super::libc::malloc(n, Tracked::assume_new(), Tracked::assume_new())
    }

    fn usable(p: *mut u8) -> usize {
        super::super::libc::malloc_usable_size(p, Tracked::assume_new())
    }

    #[test]
    fn batch_frees_every_tier_and_reports_usable_sizes() {
        let sizes = [
            8usize,
            24,
            64,
            100,
            512,
            1000,
            4096,
            20000,
            300_000,
            2 << 20,
        ];
        let mut v: Vec<*mut u8> = Vec::new();
        let mut want = 0;
        for i in 0..4000 {
            let p = malloc(sizes[i % sizes.len()]);
            assert!(!p.is_null());
            want += usable(p);
            v.push(p);
        }
        v.push(core::ptr::null_mut());
        assert_eq!(free_batch(&mut v), want);
    }

    #[test]
    fn batch_slots_are_reused() {
        // A batch's slots go back to their slabs: allocating the same
        // shape again finds memory, and a second batch frees it.
        for _ in 0..8 {
            let mut v: Vec<*mut u8> = (0..20000).map(|_| malloc(48)).collect();
            for &p in &v {
                // SAFETY: a live 48-byte block.
                unsafe { p.write_bytes(0x5a, 48) };
            }
            let n = v.len();
            assert_eq!(free_batch(&mut v), n * usable_of(48));
        }
    }

    fn usable_of(n: usize) -> usize {
        let p = malloc(n);
        let u = usable(p);
        free_one(p);
        u
    }

    #[test]
    fn short_batch_goes_through_free() {
        let mut v: Vec<*mut u8> = (0..BATCH_MIN - 1).map(|_| malloc(32)).collect();
        let n = v.len();
        assert_eq!(free_batch(&mut v), n * usable_of(32));
    }
}
