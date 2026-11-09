//! Frees and cache states that abort: foreign, misaligned, unallocated
//! and stale pointers, and the four cache run invariants.

use super::*;

#[test]
#[should_panic(expected = "not an allocation")]
fn free_of_a_pointer_outside_every_region_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let x = 0u64;
    t.free(&x as *const u64 as *mut u8);
}

#[test]
#[should_panic(expected = "not an allocation")]
fn free_of_a_misaligned_pointer_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let p = t.alloc(class_with(64));
    // SAFETY: 8 bytes into a live 64-byte slot.
    t.free(unsafe { p.add(8) });
}

#[test]
#[should_panic(expected = "invalid or double free")]
fn free_of_a_never_allocated_slot_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(16);
    let p = t.alloc(c);
    // A slot of slab 0 above the first refill's HALF is committed but
    // was never claimed (the 16-byte slab has 1024 slots).
    // SAFETY: still inside slab 0: slot 712 of its 1024.
    t.free(unsafe { p.add((HALF + 200) * 16) });
}

#[test]
#[should_panic(expected = "was never allocated")]
fn free_into_an_uncommitted_slab_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _ = t.alloc(c);
    t.free(s.regions.slab_addr(c, 5) as *mut u8);
}

/// The `free` fast path's one bound is the watermark compare
/// (`ClassLine::set_slab`): a slot start in a slab never handed
/// out, up to the region's last slab, and the slot-aligned addresses just
/// past the region and below its base (in the gap of the stride) all
/// leave for the slow path, in every class.
#[test]
fn the_watermark_compare_alone_bounds_the_region() {
    let s = shared();
    let t = TestHeap::new(s);
    let h = t.heap();
    for c in 0..NCLASSES {
        let committed = s.arena0.class(c).committed_slabs();
        let last = s.regions.nslabs(c, Tracked::assume_new()) - 1;
        let base = s.regions.slab_addr(c, 0);
        let len = s.regions.region_len();
        let mut ps = std::vec::Vec::new();
        for slab in [committed, committed + 1, last / 2, last] {
            ps.push(s.regions.slab_addr(c, slab));
        }
        // The gap after the region, and the stride's start when the
        // region's slide leaves a gap before it (the offset wraps).
        let stride_start = s.regions.superblock + c * s.regions.stride();
        assert!(base + len <= stride_start + s.regions.stride());
        if base + len < stride_start + s.regions.stride() {
            ps.push(base + len);
        }
        if stride_start < base {
            ps.push(stride_start);
        }
        for p in ps {
            let p = p as *mut u8;
            assert_eq!(s.checked_slot_any(p), None, "class {c} {p:?}");
            assert!(!free_fast(s, h, p, s.free_flags()), "class {c} {p:?}");
        }
    }
    t.destroy();
}

/// A slab past the watermark, the region's last, aborts by name from the
/// slow path.
#[test]
#[should_panic(expected = "was never allocated")]
fn free_into_the_last_slab_of_a_region_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _ = t.alloc(c);
    t.free(
        s.regions
            .slab_addr(c, s.regions.nslabs(c, Tracked::assume_new()) - 1) as *mut u8,
    );
}

#[test]
#[should_panic(expected = "invalid or double free")]
fn stale_double_free_aborts_at_the_check() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    t.free(p);
    t.flush_all();
    t.free(p);
}

#[test]
#[should_panic(expected = "double free of")]
fn cached_double_free_aborts_at_the_flush() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    t.free(p);
    // The bit is still set while `p` is cached, so the check passes
    // and the duplicate is caught when the batch is released.
    t.free(p);
    t.flush_all();
}

#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn free_of_an_unallocated_huge_pointer_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    t.free(s.regions.huge_base as *mut u8);
}

// The four cache invariants are release assertions, not
// `debug_assert!`s: each test below builds the violating state by hand
// and expects the abort.

#[test]
#[should_panic(expected = "cache run invariant violated")]
fn a_scheduled_pop_aborts_when_the_run_reaches_past_the_length() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    let len = h.cache(c).unwrap().len() as u32;
    assert!(len > 1);
    let k = h.cache_ptr(c);
    // SAFETY: `k` is the live cache header of the test's heap; the run past
    // the length is the deliberate violation, which the release assertion
    // catches (the expected panic).
    unsafe {
        (*k).mode = MODE_COMPACT;
        (*k).run_end = len + 1;
        h.pop_compact_run(
            k,
            c,
            len,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
    }
}

#[test]
#[should_panic(expected = "refill of a cache that is not empty")]
fn a_refill_of_a_cache_that_is_not_empty_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    assert!(h.cache(c).unwrap().len() > 0);
    let a = s.arena(0, Tracked::assume_new()).unwrap().0;
    let k = h.cache_ptr(c);
    // SAFETY: `k` is the heap's real cache for `(a, c)`; that it is not
    // empty is the deliberate violation the release assertion catches.
    unsafe { h.refill(s, a, Tracked::assume_new(), k, c, Tracked::assume_new()) };
}

#[test]
#[should_panic(expected = "freeing a cache that was not flushed")]
fn freeing_caches_that_were_not_flushed_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    assert!(h.cache(c).unwrap().len() > 0);
    Heap::free_caches(
        s,
        &mut h.caches,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
}

#[test]
#[should_panic(expected = "others node requested for the home arena")]
fn an_others_node_for_the_home_arena_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let h = t.heap();
    let home = h.arena;
    assert!(
        h.other_caches(s, home, false, Tracked::assume_new())
            .is_null()
    );
    h.other_caches(s, home, true, Tracked::assume_new());
}
