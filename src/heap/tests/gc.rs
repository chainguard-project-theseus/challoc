//! The cache GC: low-water release, the defrag and periodic intervals.

use super::*;

#[test]
fn gc_releases_unused_entries_by_low_water() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(48);
    // A refill starts low water at the batch length (the run needs
    // `run_end <= low_water`); the pop that follows lowers it.
    t.alloc(c);
    assert_eq!(t.len(c), HALF - 1);
    assert_eq!(t.cache(c).low_water(), HALF - 1);
    assert_eq!(t.cache(c).run().len(), HALF - 1);
    // Pops lower it; pushes do not raise it.
    for _ in 0..27 {
        t.alloc(c);
    }
    let len = HALF - 28;
    assert_eq!(t.cache(c).low_water(), len);
    let extra: Vec<*mut u8> = (0..10).map(|_| t.alloc(c)).collect();
    let lw = len - 10;
    assert_eq!(t.cache(c).low_water(), lw);
    for &p in &extra {
        t.free(p);
    }
    assert_eq!(t.len(c), len);
    assert_eq!(t.cache(c).low_water(), lw);
    // GC releases three quarters of the low water.
    let live_before = live(s, c);
    t.gc();
    let flushed = lw - lw / 4;
    assert_eq!(t.len(c), len - flushed);
    assert_eq!(live(s, c), live_before - flushed);
    assert_eq!(t.cache(c).low_water(), len - flushed);
    // Untouched since, so the next GC shrinks it again.
    let len = t.len(c);
    t.gc();
    assert_eq!(t.len(c), len - (len - len / 4));
    // A cache that was drained and refilled and then left untouched
    // gives three quarters of the batch back at the next GC.
    while t.len(c) > 0 {
        t.alloc(c);
    }
    t.alloc(c);
    assert_eq!(t.cache(c).low_water(), HALF - 1);
    let len = t.len(c);
    t.gc();
    assert_eq!(t.len(c), len - (len - len / 4));
    assert_eq!(t.cache(c).low_water(), t.len(c));
    assert_eq!(t.cache(c).run().len(), 0);
    t.destroy();
}

/// Design 13.5: `gc_for_defrag_at` is `gc` once the last GC (periodic or
/// not) is `DEFRAG_GC_INTERVAL_NS` old, restarts the clock when it runs,
/// and does nothing before that.
#[test]
fn gc_for_defrag_runs_the_gc_once_its_interval_has_passed() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(48);
    let other = class_with(160);
    // Refills: both caches hold a batch, low water at its length.
    t.alloc(c);
    t.alloc(other);
    let (len, lw) = (t.len(c), t.cache(c).low_water());
    let (olen, olw) = (t.len(other), t.cache(other).low_water());
    assert!(lw > 0 && olw > 0);
    let at = now_ns();
    // The last GC is recent: nothing moves.
    t.heap().set_last_gc(at - DEFRAG_GC_INTERVAL_NS + 1);
    t.heap().gc_for_defrag_at(s, at, Tracked::assume_new());
    assert_eq!((t.len(c), t.len(other)), (len, olen));
    // The interval has passed: exactly `gc`, three quarters of every
    // cache's low water back to the slabs, and the clock restarts.
    t.heap().set_last_gc(at - DEFRAG_GC_INTERVAL_NS);
    let live_before = live(s, c);
    t.heap().gc_for_defrag_at(s, at, Tracked::assume_new());
    assert_eq!(t.len(c), len - (lw - lw / 4));
    assert_eq!(t.len(other), olen - (olw - olw / 4));
    assert_eq!(live(s, c), live_before - (lw - lw / 4));
    assert_eq!(t.cache(c).low_water(), t.len(c));
    assert_eq!(t.heap().last_gc(), at);
    // Straight after, the interval has not passed again.
    let len = t.len(c);
    t.heap()
        .gc_for_defrag_at(s, at + DEFRAG_GC_INTERVAL_NS - 1, Tracked::assume_new());
    assert_eq!(t.len(c), len);
    t.destroy();
}

/// Design 13.5: `gc_if_due` (a statistics refresh) runs `gc` when the last
/// GC is `DEFRAG_GC_INTERVAL_NS` old and nothing when it is recent.
#[test]
fn gc_if_due_runs_the_gc_only_once_its_interval_has_passed() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(48);
    t.alloc(c);
    let (len, lw) = (t.len(c), t.cache(c).low_water());
    assert!(lw > 0);
    // Recent: nothing.
    t.heap().set_last_gc(now_ns());
    t.heap().gc_if_due(s, Tracked::assume_new());
    assert_eq!(t.len(c), len);
    // Long ago (the monotonic clock is past 100 ms): exactly `gc`.
    t.heap().set_last_gc(0);
    t.heap().gc_if_due(s, Tracked::assume_new());
    assert_eq!(t.len(c), len - (lw - lw / 4));
    assert_ne!(t.heap().last_gc(), 0);
    t.destroy();
}

/// Design 13.5: `gc_for_defrag` reads the clock once every
/// `PERIODIC_EVERY` calls (the clock is a system call), counting in
/// `hint_ticks`: with the last GC long past, the calls before the count
/// reaches a multiple of `PERIODIC_EVERY` run nothing, the one that
/// reaches it runs a step.
#[test]
fn gc_for_defrag_reads_the_clock_once_every_periodic_every_calls() {
    let s = shared();
    let t = TestHeap::new(s);
    t.heap().set_last_gc(0);
    let hints = t.heap().hint_ticks();
    let quiet = PERIODIC_EVERY - 1 - hints % PERIODIC_EVERY;
    for _ in 0..quiet {
        t.heap().gc_for_defrag(s, Tracked::assume_new());
        assert_eq!(t.heap().last_gc(), 0, "no clock read before the count");
    }
    t.heap().gc_for_defrag(s, Tracked::assume_new());
    assert_ne!(t.heap().last_gc(), 0, "the counted call reads the clock");
    assert_eq!(t.heap().hint_ticks(), hints + quiet + 1);
    t.destroy();
}

/// Design 13.5: hints count in `hint_ticks`, not in the slow path's
/// `slow_ticks`, so a defragmenter's calls neither take the periodic
/// tick's turns (its `purge::on_tick` work) nor run down `REHOME_TICKS`:
/// after many hints the next slow-path tick that reaches a multiple of
/// `PERIODIC_EVERY` is the same one as without them.
#[test]
fn gc_for_defrag_leaves_the_periodic_ticks_count_alone() {
    let s = shared();
    let t = TestHeap::new(s);
    let ticks = t.ticks();
    let hints = t.heap().hint_ticks();
    for _ in 0..3 * PERIODIC_EVERY + 5 {
        t.heap().gc_for_defrag(s, Tracked::assume_new());
    }
    assert_eq!(t.ticks(), ticks, "hints do not advance slow_ticks");
    assert_eq!(t.heap().hint_ticks(), hints + 3 * PERIODIC_EVERY + 5);
    t.destroy();
}

#[test]
fn periodic_work_runs_gc_on_the_interval() {
    // Every slow path is a tick and every 64th tick checks the clock.
    // With a zero interval that means GC on ticks 64, 128, ...; with the
    // default 10 s it means no GC in a test.
    let c = class_with(16);
    let d = class_with(48);
    for interval in [0u64, super::shared::GC_INTERVAL_NS] {
        let s = TestShared::new().gc_interval_ns(interval).build();
        let t = TestHeap::new(s);
        // Prime class d: one refill, HALF - 1 cached, low water HALF - 1.
        t.alloc(d);
        assert_eq!(t.ticks(), 1);
        // Drive the tick counter with class c refills.
        let mut ps = Vec::new();
        while t.ticks() < 64 {
            ps.push(t.alloc(c));
        }
        assert_eq!(t.ticks(), 64);
        // GC 1 released three quarters of d's untouched batch if it ran.
        let lw1 = HALF - 1;
        let flushed1 = if interval == 0 { lw1 - lw1 / 4 } else { 0 };
        assert_eq!(t.len(d), lw1 - flushed1);
        assert_eq!(t.cache(d).low_water(), lw1 - flushed1);
        for _ in 0..27 {
            t.alloc(d);
        }
        let lw = lw1 - flushed1 - 27;
        assert_eq!(t.len(d), lw);
        while t.ticks() < 128 {
            ps.push(t.alloc(c));
        }
        // GC 2 released three quarters of d's low water if it ran.
        let live_d = live(s, d);
        let flushed = if interval == 0 { lw - lw / 4 } else { 0 };
        assert_eq!(t.len(d), lw - flushed);
        assert_eq!(t.cache(d).low_water(), lw - flushed);
        assert_eq!(live_d, HALF - flushed1 - flushed);
        t.destroy();
        assert_eq!(live(s, d), 28);
        assert_eq!(live(s, c), ps.len());
    }
}
