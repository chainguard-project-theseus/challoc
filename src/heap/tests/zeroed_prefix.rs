//! The zeroed prefix of a cache and the zero flag of a pop.

use super::*;

#[test]
fn zeroed_prefix_tracks_fresh_memory() {
    let s = shared();
    let t = TestHeap::new(s);
    // 4096 slots per slab, so refills after the first come from the
    // same, by then Active, slab.
    let c = class_with(16);
    // First refill comes from a Fresh slab: the whole batch is zero.
    // A pop of a zero entry shrinks the prefix to `len`.
    let (p1, z1) = t.alloc_zeroed(c);
    assert!(z1);
    assert_eq!(t.len(c), HALF - 1);
    assert_eq!(t.cache(c).zeroed(), HALF - 1);
    let (p2, z2) = t.alloc_zeroed(c);
    assert!(z2);
    // A free pushes dirty memory on top, just above the prefix.
    // Popping it is not zero, the entry below it still is.
    t.free(p2);
    assert_eq!(t.cache(c).zeroed(), HALF - 2);
    let (p3, z3) = t.alloc_zeroed(c);
    assert_eq!(p3, p2);
    assert!(!z3);
    let (_, z4) = t.alloc_zeroed(c);
    assert!(z4);
    // Pops lower the prefix with `len`, so a push lands just above it.
    for _ in 0..50 {
        t.alloc(c);
    }
    t.free(p1);
    assert_eq!(t.cache(c).zeroed(), t.len(c) - 1);
    // Drain the cache: the next refill is from an Active slab, so the
    // conservative answer is "not zero" for the whole batch.
    while t.len(c) > 0 {
        t.alloc(c);
    }
    let (_, z5) = t.alloc_zeroed(c);
    assert!(!z5);
    assert_eq!(t.cache(c).zeroed(), 0);
    t.destroy();
}

/// Through a random mix of pops, pushes, cache GCs and the flushes and
/// refills they cause, the zero flag is never wrong (every slot handed
/// out is written, so a dirty slot reported zero would show) and the zero
/// prefix stays at or below `low_water`, which is what lets a push leave
/// `zeroed` alone.
#[test]
fn zeroed_prefix_stays_below_low_water() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let mut live: Vec<*mut u8> = Vec::new();
    let mut r = 0x2545_F491_4F6C_DD1Du64;
    let mut zeros = 0;
    for i in 0..20_000 {
        let x = xorshift(&mut r);
        if live.is_empty() || x % 5 < 3 {
            let (p, z) = t.alloc_zeroed(c);
            if z {
                assert!(
                    // SAFETY: `p` is a live 64-byte slot.
                    (0..64).all(|k| unsafe { *p.add(k) } == 0),
                    "op {i}: a dirty slot reported zero"
                );
                zeros += 1;
            }
            // SAFETY: as above; the test owns the slot.
            unsafe { core::ptr::write_bytes(p, 0xa5, 64) };
            live.push(p);
        } else {
            let k = (x >> 8) as usize % live.len();
            t.free(live.swap_remove(k));
        }
        if x.is_multiple_of(97) {
            t.gc();
        }
        let h = t.cache(c);
        assert!(
            h.zeroed() <= h.low_water() && h.low_water() <= h.len(),
            "op {i}: zeroed {} low_water {} len {}",
            h.zeroed(),
            h.low_water(),
            h.len()
        );
    }
    assert!(
        zeros > 20,
        "fresh refills were exercised ({zeros} zero pops)"
    );
    for p in live {
        t.free(p);
    }
    t.destroy();
}

/// A cache of `c` in compact mode with a non-empty run and at least one
/// entry above it, from a fragmented residue (the live slots, which the
/// caller frees). The schedule of the next pop is not set.
fn compacting_cache_with_run(t: &TestHeap, c: usize, seed: u64) -> Vec<*mut u8> {
    let mut live_ps = residue(t, c, 30, 560, seed);
    assert!(Heap::fragmented(t.s, 0, c, Tracked::assume_new()));
    live_ps.push(t.alloc(c));
    // Scheduled pops until the cache is compacting and a rebuild has
    // given it a run (the first probe only records the refill count).
    for _ in 0..16 {
        if t.cache(c).compacting() && !t.cache(c).run().is_empty() {
            break;
        }
        let k = t.heap().cache_ptr(c);
        // SAFETY: `k` is the live cache header of the test's heap.
        unsafe { (*k).allocs = (*k).compact_mask as u64 };
        live_ps.push(t.alloc(c));
    }
    assert!(
        t.cache(c).compacting() && !t.cache(c).run().is_empty(),
        "no compacting cache with a run"
    );
    // An entry above the run, so the scheduled pop moves a different top
    // into the run's slot.
    if t.len(c) == t.cache(c).run().len() {
        t.free(live_ps.pop().unwrap());
    }
    assert!(t.len(c) > t.cache(c).run().len());
    live_ps
}

/// The compacting pop's zero flag against the slots' memory. A scheduled
/// pop serves `slots[run_end - 1]` and moves the top entry into its
/// place, so the prefix may keep that position only if the old top was in
/// it too. Three prefixes: the whole stack (the moved entry is zero, the
/// prefix ends at the new `len`), up to the run entry alone (the moved
/// entry is dirty, the prefix ends below it) and below the run entry (not
/// zero, the prefix unchanged). The prefix's slots are cleared and every
/// other cached slot is filled, then the scheduled pop and every pop
/// after it, scheduled or not, must report zero exactly when the slot
/// handed out reads as zero.
#[test]
fn compacting_pop_zero_flag_matches_memory() {
    let s = shared();
    let c = class_with(224);
    let size = CLASSES[c].size();
    for case in 0..3u64 {
        let t = TestHeap::new(s);
        let live_ps = compacting_cache_with_run(&t, c, 0x2e70_c0de + case);
        let entries = t.cached_plain(c);
        let len = entries.len() as u32;
        let run_end = t.cache(c).run().len() as u32;
        let at = run_end - 1;
        let zeroed = match case {
            0 => len,
            1 => run_end,
            _ => at,
        };
        for (i, &q) in entries.iter().enumerate() {
            // SAFETY: `q` is a cached `size`-byte slot of the test's heap, unused.
            unsafe { core::ptr::write_bytes(q, if (i as u32) < zeroed { 0 } else { 0xa5 }, size) };
        }
        let h = t.heap();
        h.slow_ticks = 0;
        let k = h.cache_ptr(c);
        // SAFETY: `k` is the live cache header of the test's heap.
        unsafe {
            (*k).low_water = len;
            (*k).zeroed = zeroed;
            (*k).allocs = (*k).compact_mask as u64;
        }
        let (p, z) = t.alloc_zeroed(c);
        assert_eq!(
            p, entries[at as usize],
            "case {case}: the scheduled pop serves the run's last entry"
        );
        assert_eq!(z, case < 2, "case {case}: zero flag of the run entry");
        let want = match case {
            0 => len - 1,
            1 => at,
            _ => at,
        };
        assert_eq!(
            t.cache(c).zeroed(),
            want as usize,
            "case {case}: the prefix after the pop"
        );
        let mut out = std::vec![p];
        let mut zeros = z as usize;
        while t.len(c) > 0 {
            let (q, zq) = t.alloc_zeroed(c);
            // SAFETY: `q` is a live `size`-byte slot.
            let clean = (0..size).all(|b| unsafe { *q.add(b) } == 0);
            assert!(
                !zq || clean,
                "case {case}: a dirty slot {q:?} reported zero"
            );
            if zq {
                zeros += 1;
            }
            out.push(q);
        }
        // Every slot the prefix still covered after the pop came out zero.
        assert_eq!(zeros, z as usize + want as usize, "case {case}: zero pops");
        for q in out.into_iter().chain(live_ps) {
            t.free(q);
        }
        t.destroy();
    }
}

/// `zeroed <= low_water` is part of the run invariant the compacting pop
/// checks: a zero prefix reaching above the low-water mark is a heap
/// inconsistency and aborts (a panic under test) before any entry moves.
#[test]
#[should_panic(
    expected = "heap: cache run invariant violated (run_end or zeroed > low_water, or low_water > len)"
)]
fn compacting_pop_aborts_on_a_zeroed_prefix_above_low_water() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(224);
    let _live = compacting_cache_with_run(&t, c, 0x10a7_e12d);
    let len = t.len(c) as u32;
    let run_end = t.cache(c).run().len() as u32;
    let h = t.heap();
    h.slow_ticks = 0;
    let k = h.cache_ptr(c);
    // SAFETY: `k` is the live cache header of the test's heap.
    unsafe {
        // Every other clause holds: `run_end <= low_water < len`.
        (*k).low_water = run_end;
        assert!(run_end < len);
        (*k).zeroed = run_end + 1;
        (*k).allocs = (*k).compact_mask as u64;
    }
    t.alloc_zeroed(c);
}

#[test]
fn flush_drops_the_zeroed_prefix() {
    let s = shared();
    let t = TestHeap::new(s);
    let u = TestHeap::new(s);
    let c = class_with(32);
    // Zero prefix of HALF - 28 with 28 dirty pushes above it.
    let ps: Vec<*mut u8> = (0..28).map(|_| t.alloc(c)).collect();
    assert_eq!(t.len(c), HALF - 28);
    for &p in &ps {
        t.free(p);
    }
    assert_eq!(t.len(c), HALF);
    assert_eq!(t.cache(c).zeroed(), HALF - 28);
    // Fill the rest of the cache with pointers another heap allocated
    // (dirty by definition), then overflow it by one.
    let qs: Vec<*mut u8> = (0..=HALF).map(|_| u.alloc(c)).collect();
    for &q in &qs[..HALF] {
        t.free(q);
    }
    assert_eq!(t.len(c), CAP);
    assert_eq!(t.cache(c).zeroed(), HALF - 28);
    let live_before = live(s, c);
    t.free(qs[HALF]);
    // HALF entries went back to the slab and the rest were reordered:
    // the prefix is gone.
    assert_eq!(t.len(c), HALF + 1);
    assert_eq!(t.cache(c).zeroed(), 0);
    assert_eq!(live(s, c), live_before - HALF);
    t.destroy();
    u.destroy();
    assert_eq!(live(s, c), 0);
}

/// `pop_fast` is the one body behind `alloc_fast` and `alloc_zeroed`:
/// same slot, same zero flag as the cache
/// header says, `None` from an empty cache.
#[test]
fn pop_fast_pops_the_top_with_the_zero_flag() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    assert_eq!(
        t.heap().pop_fast(c, flags, Tracked::assume_new()),
        None,
        "no cache yet"
    );
    assert_eq!(t.heap().alloc_fast(c, flags, Tracked::assume_new()), None);
    let first = t.alloc(c);
    // The refill batch is a fresh slab: the zero prefix covers it all.
    let len = t.len(c);
    let top = t.cached_plain(c)[len - 1];
    assert_eq!(
        t.heap().pop_fast(c, flags, Tracked::assume_new()),
        Some((top, true))
    );
    assert_eq!(t.len(c), len - 1);
    let top = t.cached_plain(c)[len - 2];
    assert_eq!(
        t.heap().alloc_fast(c, flags, Tracked::assume_new()),
        Some(top)
    );
    // A slot pushed back is no longer known zero (the push clips the
    // prefix), and the flag says so.
    t.free(first);
    assert_eq!(
        t.heap().pop_fast(c, flags, Tracked::assume_new()),
        Some((first, false))
    );
    assert!(
        t.alloc_zeroed(c).1,
        "below the pushed entry the prefix still holds"
    );
    t.flush_all();
    assert_eq!(
        t.heap().pop_fast(c, flags, Tracked::assume_new()),
        None,
        "flushed"
    );
    t.destroy();
}
