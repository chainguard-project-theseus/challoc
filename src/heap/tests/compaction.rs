//! Compaction: the probe's gate, scheduled pops, the churn models and
//! the randomised model check.

use super::*;

/// Slabs of `c` with at least one live slot.
fn occupied(s: &Shared, c: usize) -> usize {
    let cs = &s.arena0.classes[c];
    (0..cs.committed_slabs())
        .filter(|&i| !cs.is_guard_slab(i) && cs.desc(i).live() > 0)
        .count()
}

/// Pop one slot of `c` and check the rule against the cache state
/// before the pop: while compacting, a scheduled pop returns the run's
/// lowest entry (its last) or, when the run was exhausted, the lowest
/// slot of the block a rebuild claimed; otherwise every pop returns the
/// top of the stack. Nothing is checked when the cache was empty (a
/// refill happened). The run is descending afterwards.
fn pop_checked(t: &TestHeap, c: usize) -> *mut u8 {
    let (before, run, allocs, compacting) =
        match t.heap().cache(c).map(|k| (k.allocs(), k.compacting())) {
            Some((allocs, compacting)) => (t.cached_plain(c), t.run_plain(c), allocs, compacting),
            None => (Vec::new(), Vec::new(), 0, false),
        };
    let scheduled = compacting && (allocs + 1) % (1 << COMPACT_SHIFT) == 0;
    let probe = !compacting && (allocs + 1) % (PROBE_MASK as u64 + 1) == 0;
    let p = t.alloc(c);
    if !before.is_empty() && !probe {
        if scheduled {
            if let Some(&low) = run.last() {
                assert_eq!(
                    p,
                    low,
                    "scheduled pop {} is not the run's lowest entry",
                    allocs + 1
                );
            } else {
                // A rebuild: the block came from the lowest partial slab
                // and its lowest slot was served.
                assert!(
                    t.run_plain(c).iter().all(|&q| q > p),
                    "rebuild pop {} is not the block minimum",
                    allocs + 1
                );
            }
        } else if t.cache(c).compacting() || !compacting {
            assert_eq!(
                p,
                *before.last().unwrap(),
                "pop {} is not the top",
                allocs + 1
            );
        }
    }
    check_run(t, c);
    p
}

/// The run invariant: descending, distinct, a prefix of the cache no
/// longer than the low-water mark.
fn check_run(t: &TestHeap, c: usize) {
    let run = t.run_plain(c);
    let k = t.cache(c);
    assert!(
        run.windows(2).all(|w| w[0] > w[1]),
        "run not descending: {run:?}"
    );
    assert!(run.len() <= k.low_water() && k.low_water() <= k.len());
    assert_eq!(run, &t.cached_plain(c)[..run.len()]);
}

/// The compaction wrap: a scheduled
/// pop finds a compact-mode cache holding one entry with `low_water ==
/// len == 1`, an exhausted run and the GC due. `rebuild_run`'s tick
/// runs the GC, which flushes the whole cache (`lw - lw / 4 == lw`
/// below 4); the class is dense by then, so the rebuild drops the cache
/// back to probing and claims nothing. `pop_compact_run` must then
/// refill (or give up) rather than pop the empty cache, where `len - 1`
/// wrapped and read `slots[0xFFFF_FFFF]`. Every step but the schedule
/// (`allocs`, `slow_ticks`, `last_gc`) goes through the public
/// operations.
#[test]
fn a_scheduled_pop_whose_gc_empties_the_cache_refills_instead_of_wrapping() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(224);
    let mut live_ps = residue(&t, c, 30, 560, 0x0c0f_fee5);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    // Refill once, then probe until the cache switches to compacting
    // (the first probe only records the class's refill count).
    live_ps.push(t.alloc(c));
    for _ in 0..4 {
        if t.cache(c).compacting() {
            break;
        }
        let k = t.heap().cache_ptr(c);
        // SAFETY: `k` is the live cache header of the test's heap.
        unsafe { (*k).allocs = (*k).compact_mask as u64 };
        live_ps.push(t.alloc(c));
    }
    assert!(t.cache(c).compacting());
    // One entry, `low_water == len == 1`, no run, still compacting.
    t.flush_all();
    assert!(t.cache(c).compacting());
    let p0 = live_ps.pop().unwrap();
    t.free(p0);
    t.gc();
    assert_eq!(t.len(c), 1);
    assert_eq!(t.cache(c).low_water(), 1);
    assert!(t.cache(c).run().is_empty());
    // Make the class dense: every other live slot goes back to its slab
    // through another heap, so `p0`'s slab is the only one that counts
    // and a single slab is never fragmented.
    let b = TestHeap::new(s);
    for &p in &live_ps {
        b.free(p);
    }
    b.destroy();
    assert!(!Heap::fragmented(s, 0, c, Tracked::assume_new()));
    assert_eq!(live(s, c), 1);
    // The next pop is scheduled, its tick is the periodic one and the
    // GC interval has elapsed (the interval is subtracted from the
    // clock rather than assumed).
    let h = t.heap();
    h.slow_ticks = PERIODIC_EVERY - 1;
    h.last_gc = now_ns().wrapping_sub(s.gc_interval_ns);
    let k = h.cache_ptr(c);
    // SAFETY: `k` is the live cache header of the test's heap.
    unsafe { (*k).allocs = (*k).compact_mask as u64 };
    let ticks = t.ticks();
    let p = t.alloc(c);
    assert_eq!(t.ticks(), ticks + 1);
    assert!(!p.is_null());
    let (cls, arena) = s
        .classify_free(p, Tracked::assume_new())
        .expect("a slot of the class");
    assert_eq!((cls, arena), (c, 0));
    // The GC flushed `p0`, the rebuild dropped back to probing, and the
    // refill served the pop: half a cache minus the one popped.
    assert!(!t.cache(c).compacting());
    assert_eq!(t.len(c), t.cache(c).cap() / 2 - 1);
    assert_eq!(t.cache(c).low_water(), t.len(c));
    check_run(&t, c);
    t.free(p);
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn a_dense_class_is_probed_but_never_compacted() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    // A larson-like churn over a dense class: allocations and frees
    // balance, the holes stay in the cache, the class has none.
    let mut ps: Vec<*mut u8> = (0..2000).map(|_| pop_checked(&t, c)).collect();
    let ticks = t.ticks();
    let mut st = 0xabcd_ef01u64;
    for _ in 0..40_000 {
        let i = (xorshift(&mut st) % ps.len() as u64) as usize;
        t.free(ps[i]);
        ps[i] = pop_checked(&t, c);
    }
    let pops = t.cache(c).allocs();
    assert!(pops > 4 * (PROBE_MASK as u64 + 1), "{pops} pops");
    assert!(!t.cache(c).compacting());
    // No refill, no flush, no rebuild: the only slow paths were the
    // probes' heartbeats, one per 4096 pops.
    assert_eq!(
        t.ticks() as u64,
        ticks as u64 + pops / (PROBE_MASK as u64 + 1) - 2000 / (PROBE_MASK as u64 + 1)
    );
    assert!(!Heap::fragmented(s, 0, c, Tracked::assume_new()));
    for p in ps {
        t.free(p);
    }
    t.destroy();
}

#[test]
fn scheduled_pops_take_the_run_minimum_and_rebuild_from_the_lowest_slab() {
    let s = shared();
    let t = TestHeap::new(s);
    // 292 slots per slab, cap 512, block 128.
    let c = class_with(224);
    let cs = &s.arena0.classes[c];
    let mut live_ps = residue(&t, c, 30, 560, 0x5eed_1234);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    assert_eq!(t.len(c), 0);
    // The first probe switches the cache to compacting; from then on
    // every eighth pop is served from the run.
    let mut st = 0x1357_9bdfu64;
    let mut switched_at = None;
    let mut rebuilds = 0;
    for step in 0..30_000 {
        if xorshift(&mut st) & 1 == 0 && live_ps.len() > 2000 {
            let i = (xorshift(&mut st) % live_ps.len() as u64) as usize;
            t.free(live_ps.swap_remove(i));
            check_run(&t, c);
        } else {
            let run_before = t.heap().cache(c).map_or(0, |k| k.run().len());
            let ticks = t.ticks();
            live_ps.push(pop_checked(&t, c));
            if run_before == 0 && t.ticks() > ticks && t.cache(c).compacting() {
                rebuilds += 1;
                // The block is the run, claimed lowest partial slab
                // first: no slab below the highest it touched still has
                // room (the lowest partial slab left is at or above it).
                let run = t.run_plain(c);
                let top_slab = run.iter().map(|&q| cs.locate(q).0).max().unwrap();
                // (`plan` clears a filled slab's partial bit lazily, so
                // the lowest partial slab may be a full one.)
                let nslots = CLASSES[c].nslots();
                assert!(
                    cs.lowest_partial(vstd::prelude::Tracked::assume_new())
                        .is_none_or(|l| l >= top_slab || cs.desc(l).live() == nslots),
                    "block from slab {top_slab} above the lowest partial slab {:?}",
                    cs.lowest_partial(vstd::prelude::Tracked::assume_new())
                );
            }
        }
        if switched_at.is_none() && t.cache(c).compacting() {
            switched_at = Some(step);
        }
        if step % 1009 == 0 {
            assert_eq!(live(s, c), live_ps.len() + t.len(c), "step {step}");
        }
    }
    assert!(switched_at.is_some(), "never switched to compacting");
    assert!(rebuilds > 0, "no rebuild observed");
    assert_eq!(live(s, c), live_ps.len() + t.len(c));
    for p in live_ps {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn churn_model_empties_the_high_slabs_and_then_stops_compacting() {
    // The model of `bench/probes/churn.c`: fill `MODEL_SLOTS` slots of
    // the 1 KiB class (cap 128), free a random 56%, then run a
    // balanced free-one/allocate-one churn. Without scheduled pops
    // nothing moves (every allocation gets back the slot just freed);
    // with them the live set migrates into the lowest slabs, the high
    // slabs empty, and once the class is dense the cache goes back to
    // probing.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
    let before = occupied(s, c);
    // A random 56% deletion empties a 64-slot slab never and a 4-slot
    // slab about one time in ten.
    assert!(
        before * 20 >= 17 * (MODEL_SLOTS / nslots),
        "{before} occupied slabs after the delete"
    );
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    let ideal = keep.len().div_ceil(nslots);
    let mut st = 0x2468_ace0u64;
    let mut was_compacting = false;
    // With shift 8 one pop in 256 migrates a slot; the live set of about
    // 1100 slots in the high half needs a few hundred thousand steps.
    for _ in 0..400_000 {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        t.free(keep[i]);
        keep[i] = t.alloc(c);
        was_compacting |= t.cache(c).compacting();
    }
    assert!(was_compacting);
    let after = occupied(s, c);
    // Live data plus the holes the gate leaves (under COMPACT_MIN_SLABS
    // 64 KiB slabs' worth, i.e. 64 of the 4 KiB slabs) plus what the
    // cache holds (cap = 128 slots, 32 slabs) plus one partial slab.
    let bound = ideal
        + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots
        + default_cap(c) as usize / nslots
        + 1;
    assert!(
        after <= bound,
        "{after} occupied slabs after the churn, ideal {ideal}, bound {bound}, before {before}"
    );
    assert!(!Heap::fragmented(s, 0, c, Tracked::assume_new()));
    assert!(!t.cache(c).compacting(), "still compacting a dense class");
    assert_eq!(live(s, c), keep.len() + t.len(c));
    for p in keep {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn direct_claims_do_not_block_compaction_but_cache_refills_do() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x0bad_cafe);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    // A balanced churn with a defragmenter-like stream of direct
    // (MALLOCX_TCACHE_NONE) claims at 3 slabs' worth per probe window
    // (over the gate, were it counting `claimed`; light enough not to
    // fill the residue's holes by itself): `claimed` races ahead,
    // `refilled` does not, compaction engages and slabs empty.
    let mut st = 0x1111_2222u64;
    let mut direct: *mut u8 = ptr::null_mut();
    let before_claimed = cs.claimed();
    let before = occupied(s, c);
    let mut was_compacting = false;
    let windows = 72;
    for step in 0..windows * (PROBE_MASK as usize + 1) {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        t.free(keep[i]);
        keep[i] = t.alloc(c);
        was_compacting |= t.cache(c).compacting();
        if step % 32 == 0 {
            // Like the defragmenter: a direct copy, the old one freed.
            if !direct.is_null() {
                s.free_direct(0, c, direct, Tracked::assume_new(), Tracked::assume_new());
            }
            let (p, _, _) = s.alloc_direct(
                &s.arena0,
                Tracked::assume_new(),
                c,
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
            assert!(!p.is_null());
            direct = p;
        }
    }
    assert!(
        cs.claimed() - before_claimed
            >= windows as u64 * COMPACT_MAX_REFILL_SLABS as u64 * CLASSES[c].compact_slots() as u64
    );
    assert!(was_compacting, "direct claims blocked compaction");
    // The churn model's bound: live data plus the holes the gate
    // leaves plus what the cache holds plus one partial slab.
    let bound = keep.len().div_ceil(nslots)
        + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots
        + default_cap(c) as usize / nslots
        + 1;
    assert!(
        occupied(s, c) <= bound,
        "little compacted: {} of {before} slabs still occupied, bound {bound}",
        occupied(s, c)
    );
    s.free_direct(0, c, direct, Tracked::assume_new(), Tracked::assume_new());
    for p in keep {
        t.free(p);
    }
    t.destroy();
    // A residue under a growing allocation stream (the 224-byte class,
    // 292 slots per slab, so two probe windows fit the synthetic
    // region): every 256 pops refill 256 slots, 4096 per window, far
    // over the gate; the class is being drained the ordinary way and
    // is left alone.
    let c = class_with(224);
    let cs = &s.arena0.classes[c];
    let t = TestHeap::new(s);
    let mut keep = residue(&t, c, 30, 560, 0x3333_4444);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    let before_refilled = cs.refilled();
    for _ in 0..2 * (PROBE_MASK as usize + 1) + 64 {
        keep.push(t.alloc(c));
    }
    assert!(cs.refilled() - before_refilled >= 2 * 4096 - 256);
    assert!(!t.cache(c).compacting(), "a refilling class was compacted");
    for p in keep {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn random_walk_churn_model_compacts_too() {
    // Valkey's delete-phase pattern: frees and allocations of one class
    // arrive in random order (memtier DEL:SET over random keys), so the
    // live count and the cache length random-walk instead of the strict
    // alternation of `churn_model_...`. The bound is the same.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(
        &t,
        c,
        MODEL_SLOTS / CLASSES[c].nslots(),
        560,
        0x5151_a1a1_7777_0f0f,
    );
    let target = keep.len();
    let before = occupied(s, c);
    let mut st = 0x1357_9bdfu64;
    let mut was_compacting = false;
    for _ in 0..400_000 {
        let r = xorshift(&mut st);
        let go_free = if keep.len() > target + 100 {
            true
        } else if keep.len() + 100 < target {
            false
        } else {
            r & 1 == 0
        };
        if go_free {
            let i = ((r >> 8) % keep.len() as u64) as usize;
            t.free(keep.swap_remove(i));
        } else {
            keep.push(t.alloc(c));
        }
        was_compacting |= t.cache(c).compacting();
    }
    assert!(was_compacting);
    let after = occupied(s, c);
    let bound = keep.len().div_ceil(nslots)
        + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots
        + default_cap(c) as usize / nslots
        + 1;
    assert!(
        after <= bound,
        "{after} occupied slabs after the random walk, bound {bound}, before {before}"
    );
    for p in keep {
        t.free(p);
    }
    t.destroy();
}

#[test]
fn randomised_model_check() {
    let s = shared();
    let heaps: Vec<TestHeap> = (0..3).map(|_| TestHeap::new(s)).collect();
    let classes = [
        class_with(16),
        class_with(48),
        class_with(64),
        class_with(256),
        class_with(4096),
    ];
    let mut model: HashSet<usize> = HashSet::new();
    let mut order: Vec<(usize, usize)> = Vec::new();
    let mut st = 0xdead_beef_cafe_f00du64;
    for step in 0..60_000 {
        let r = xorshift(&mut st);
        let t = &heaps[(r >> 8) as usize % heaps.len()];
        if model.len() < 2000 && (r & 3 != 0 || model.is_empty()) {
            let ci = (r >> 16) as usize % classes.len();
            let c = classes[ci];
            let p = t.alloc(c) as usize;
            assert!(
                model.insert(p),
                "step {step}: duplicate live pointer {p:#x}"
            );
            assert_eq!(class_of(CLASSES[c].size()), Some(c));
            assert!(
                matches!(s.regions.classify(p), Kind::Slab { class, .. } if class as usize == c)
            );
            order.push((p, c));
        } else {
            let i = (r >> 20) as usize % order.len();
            let (p, _) = order.swap_remove(i);
            assert!(model.remove(&p));
            t.free(p as *mut u8);
        }
        if step % 5000 == 0 {
            // Invariant: live == program-live + cached, per class.
            for &c in &classes {
                let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
                let cached: usize = heaps
                    .iter()
                    .map(|h| h.heap().cache(c).map_or(0, |k| k.len()))
                    .sum();
                assert_eq!(live(s, c), prog + cached, "step {step} class {c}");
            }
        }
    }
    for t in heaps {
        t.destroy();
    }
    for &c in &classes {
        let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
        assert_eq!(live(s, c), prog);
    }
}

/// The compaction probe's gate ([`Heap::probe_verdict`]): dense classes keep probing, a
/// fragmented class enters compaction (`compact_on` counted) unless
/// ordinary refills claimed the gate's worth of slabs since the previous
/// probe (`gate_refill` counted). The refill counter it returns is the
/// class's current one.
#[test]
fn probe_verdict_applies_the_gate_and_records_it() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let ps: Vec<*mut u8> = (0..4).map(|_| t.alloc(c)).collect();
    assert!(!Heap::fragmented(s, 0, c, Tracked::assume_new()));
    let now = cs.refilled() as u32;
    assert_eq!(
        Heap::probe_verdict(s, 0, c, now, Tracked::assume_new()),
        (now, Verdict::Dense)
    );
    for p in &ps {
        t.free(*p);
    }
    let keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x0bad_cafe);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
    let now = cs.refilled() as u32;
    let before = cs.compaction_stats();
    assert_eq!(
        Heap::probe_verdict(s, 0, c, now, Tracked::assume_new()),
        (now, Verdict::Fragmented)
    );
    let after = cs.compaction_stats();
    assert_eq!(
        (after.1 - before.1, after.3 - before.3),
        (1, 0),
        "(compact_on, gate_refill)"
    );
    let gate = (COMPACT_MAX_REFILL_SLABS * CLASSES[c].compact_slots()) as u32;
    let before = cs.compaction_stats();
    assert_eq!(
        Heap::probe_verdict(s, 0, c, now.wrapping_sub(gate), Tracked::assume_new()),
        (now, Verdict::Refilling)
    );
    let after = cs.compaction_stats();
    assert_eq!(
        (after.1 - before.1, after.3 - before.3),
        (0, 1),
        "(compact_on, gate_refill)"
    );
    assert_eq!(
        Heap::probe_verdict(s, 0, c, now.wrapping_sub(gate - 1), Tracked::assume_new()).1,
        Verdict::Fragmented,
        "one refill short of the gate"
    );
    for p in keep {
        t.free(p);
    }
    t.destroy();
}

/// The mode-to-mask rule has one home ([`CacheHeader::mask_for`]), and
/// `set_mode` applies it for every mode.
#[test]
fn set_mode_derives_the_mask() {
    for mode in [MODE_OFF, MODE_PROBE, MODE_COMPACT] {
        let mut a = CacheHeader::EMPTY;
        a.set_mode(mode);
        let want = match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => COMPACT_MASK,
            _ => u32::MAX,
        };
        assert_eq!(a.compact_mask, want, "mode {mode}");
    }
}
