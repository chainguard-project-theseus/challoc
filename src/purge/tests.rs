use super::*;

/// The step gate: nothing `Empty` is `None` and no walk; the oldest
/// member's first step is the deadline until it passes, then one walk
/// per step; a deadline set under a longer period since shortened
/// (further away than one step) does not delay the walk; the walk's
/// republished oldest decides whether a next deadline exists.
#[test]
fn decayer_walks_once_per_step_and_follows_a_shortened_period() {
    let d = Decayer::new();
    // The walks seen, `(now, curve)`; a cell so the assertions between
    // the calls can read it.
    let walks = std::cell::RefCell::new(std::vec::Vec::new());
    let walk_to = |d: &Decayer, now: u64, decay_ns: u64, leave: u64| {
        d.decay(now, decay_ns, true, |at, curve| {
            walks.borrow_mut().push((at, *curve));
            d.set_oldest_empty(leave);
            None
        })
    };
    let walks = || walks.borrow().clone();
    // Nothing `Empty`: no deadline, no walk.
    assert_eq!(d.oldest_empty(), 0);
    assert_eq!(walk_to(&d, 1_000, 200_000, 0), None);
    assert!(walks().is_empty());
    // The first `Empty` member at 1000: the deadline is the end of its
    // first step (1000 ns steps of a 200 us period), no walk before.
    assert!(d.note_first_empty(1_000));
    assert!(!d.note_first_empty(1_500), "the oldest is already known");
    assert_eq!(d.oldest_empty(), 1_000);
    assert_eq!(walk_to(&d, 1_500, 200_000, 1_000), Some(2_000));
    assert_eq!(walk_to(&d, 1_999, 200_000, 1_000), Some(2_000));
    assert!(walks().is_empty());
    // At the step: one walk, with the curve of the period, and the
    // next deadline one step on.
    assert_eq!(walk_to(&d, 2_000, 200_000, 1_000), Some(3_000));
    assert_eq!(walks(), [(2_000, Curve::new(200_000))]);
    // Within the step: the stored deadline, no second walk.
    assert_eq!(walk_to(&d, 2_500, 200_000, 1_000), Some(3_000));
    assert_eq!(walks().len(), 1);
    // The next step: a walk again.
    assert_eq!(walk_to(&d, 3_000, 200_000, 1_000), Some(4_000));
    assert_eq!(walks().len(), 2);
    // The period is shortened to 20 us (100 ns steps): the stored
    // deadline 4000 is more than a step away from 3_050, so the walk
    // runs now instead of waiting for it, and the next deadline is a
    // short step on.
    assert_eq!(walk_to(&d, 3_050, 20_000, 1_000), Some(3_150));
    assert_eq!(walks().len(), 3);
    assert_eq!(walks()[2], (3_050, Curve::new(20_000)));
    // A walk whose republished oldest is 0 (everything purged) ends
    // the deadlines; a later `Empty` starts them again from its own
    // first step.
    assert_eq!(walk_to(&d, 3_150, 20_000, 0), None);
    assert_eq!(walks().len(), 4);
    assert_eq!(d.oldest_empty(), 0);
    assert!(d.set_oldest_empty(10_000), "from none to one");
    assert!(!d.set_oldest_empty(10_000), "already one");
    assert_eq!(walk_to(&d, 10_050, 20_000, 10_000), Some(10_100));
    assert_eq!(walks().len(), 4);
    // A zero period: every step is 0 ns, so the walk is due at once
    // and the deadline is `now` itself.
    assert_eq!(walk_to(&d, 10_060, 0, 10_000), Some(10_060));
    assert_eq!(walks().len(), 5);
    assert_eq!(walks()[4], (10_060, Curve::new(0)));
}

/// The demand hold: a walk that returns a deadline past the next step
/// defers the walks to it; the hold is void under another period, and
/// ignored (and not recorded) when the caller says no holds (a retention
/// cap, or the policy turning the floor off); a deadline no later than
/// the next step is no hold.
/// `last_walk` is the previous walk's time while the walk runs.
#[test]
fn decayer_holds_after_a_floor_walk() {
    let d = Decayer::new();
    let walks = std::cell::Cell::new(0);
    let walk_hold = |now: u64, decay_ns: u64, hold_ok: bool, hold: Option<u64>, last: u64| {
        d.decay(now, decay_ns, hold_ok, |_, _| {
            walks.set(walks.get() + 1);
            assert_eq!(d.last_walk(), last, "the walk sees the previous walk's time");
            hold
        })
    };
    assert!(d.note_first_empty(1_000));
    // Steps of 1000 ns; the walk at 2000 holds until 50_000.
    assert_eq!(walk_hold(2_000, 200_000, true, Some(50_000), 0), Some(50_000));
    assert_eq!(walks.get(), 1);
    assert_eq!(d.last_walk(), 2_000);
    // Steps pass, no walk: the deadline is the hold.
    assert_eq!(walk_hold(3_000, 200_000, true, None, 2_000), Some(50_000));
    assert_eq!(walk_hold(49_999, 200_000, true, None, 2_000), Some(50_000));
    assert_eq!(walks.get(), 1);
    // No holds (a retention cap, the floor turned off): the hold set
    // under the floor is ignored, walks at step cadence.
    assert_eq!(walk_hold(10_000, 200_000, false, None, 2_000), Some(11_000));
    assert_eq!(walks.get(), 2);
    // A walk's hold is not recorded under no holds either.
    assert_eq!(walk_hold(11_000, 200_000, false, Some(60_000), 10_000), Some(12_000));
    assert_eq!(walks.get(), 3);
    assert_eq!(walk_hold(12_000, 200_000, true, None, 11_000), Some(13_000));
    assert_eq!(walks.get(), 4);
    // A new hold, then the period changes: the hold is void and the
    // shortened-period rule walks at once.
    assert_eq!(walk_hold(13_000, 200_000, true, Some(40_000), 12_000), Some(40_000));
    assert_eq!(walk_hold(14_000, 200_000, true, None, 13_000), Some(40_000));
    assert_eq!(walks.get(), 5);
    assert_eq!(walk_hold(14_000, 20_000, true, None, 13_000), Some(14_100));
    assert_eq!(walks.get(), 6);
    // A hold no later than the next step is no hold.
    assert_eq!(walk_hold(14_100, 20_000, true, Some(14_150), 14_000), Some(14_200));
    assert_eq!(walks.get(), 7);
    // At the hold's deadline the walk runs again; nothing left `Empty`
    // ends the deadlines whatever the walk returned.
    assert_eq!(walk_hold(15_000, 200_000, true, Some(90_000), 14_100), Some(90_000));
    assert_eq!(walk_hold(90_000, 200_000, true, None, 15_000), Some(91_000));
    d.set_oldest_empty(0);
    assert_eq!(walk_hold(95_000, 200_000, true, Some(99_000), 90_000), None);
    assert_eq!(walks.get(), 9);
}

/// Pages the bookkeeping test may point a batch at: a real mapping, since
/// the flush really discards.
fn scratch(pages: usize, page: usize) -> usize {
    use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    unsafe { sys::mmap(0, pages * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap()
}

/// The page the batch tests work in: the kernel's (the flush really
/// discards, so the ranges must be page-aligned).
fn page_size() -> usize {
    crate::sys::features::page_size()
}

#[test]
fn batch_merges_adjacent_ranges() {
    let _serial = serial();
    let page = page_size();
    let base = scratch(8, page);
    let mut b = Batch::new();
    b.push(base, page);
    b.push(base + page, page);
    b.push(base + 2 * page, 2 * page);
    assert_eq!(b.len(), 1);
    assert_eq!(b.ranges[0], (base, 4 * page));
    // A gap starts a new range; so does a range that is not the next one.
    b.push(base + 5 * page, page);
    assert_eq!(b.len(), 2);
    b.push(base + 4 * page, page);
    assert_eq!(b.len(), 3);
    b.flush();
    assert!(b.is_empty());
}

#[test]
fn batch_fills_and_flushes() {
    let _serial = serial();
    let (_, eager0) = batches();
    let page = page_size();
    let base = scratch(2 * MAX_BATCH + 4, page);
    let mut b = Batch::new();
    assert!(b.is_empty());
    // Every other page, so nothing merges.
    for i in 0..MAX_BATCH {
        assert!(!b.is_full());
        b.push(base + 2 * page * i, page);
    }
    assert!(b.is_full());
    assert_eq!(b.len(), MAX_BATCH);
    // The 65th push flushes the first 64 first.
    b.push(base + 2 * page * MAX_BATCH, 2 * page);
    assert_eq!(b.len(), 1);
    b.flush();
    assert!(b.is_empty());
    b.flush();
    assert!(b.is_empty());
    // At least the two eager batches of this test were counted (the
    // default advice; other tests flush their own batches concurrently
    // in this process), none lazy.
    assert!(batches().1 >= eager0 + 2);
    assert!(purged_is_zero());
}

/// The default (no limit) discards with `MADV_DONTNEED`, so purged
/// memory is zero. The headroom band is exercised end to end by
/// `tests/c/cgroup_lazy.c`; issuing a lazy batch here would make the flag sticky for the class
/// and huge tests that read it.
#[test]
fn purged_memory_is_zero_by_default() {
    assert!(purged_is_zero());
    assert_eq!(advice(), Advice::Eager);
    assert_eq!(advice_reason(), "no headroom known");
}

/// The advice's process-wide wiring: the
/// policy's headroom mirror, the `lazy_band` key and an explicit purge
/// in flight, read without any batch being flushed. Serialised with
/// [`ADVICE_TEST`] against the batch tests, which flush.
#[test]
fn advice_follows_headroom_band_key_and_explicit_purges() {
    let _serial = serial();
    assert_eq!(advice(), Advice::Eager);
    set_headroom(true, false);
    assert_eq!(advice(), Advice::Lazy);
    assert_eq!(advice_reason(), "under 70% of the limit");
    // The diagnostics name the flag the next flush would issue.
    assert_eq!(advice_name(), "MADV_FREE");
    {
        let _e = Explicit::enter();
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "explicit purge");
        {
            let _e2 = Explicit::enter();
            assert_eq!(advice(), Advice::Eager);
        }
        assert_eq!(advice(), Advice::Eager);
    }
    assert_eq!(advice(), Advice::Lazy);
    configure_advice(false);
    assert_eq!(advice(), Advice::Eager);
    assert_eq!(advice_reason(), "lazy_band:false");
    configure_advice(true);
    assert_eq!(advice(), Advice::Lazy);
    set_headroom(false, true);
    assert_eq!(advice(), Advice::Eager);
    assert_eq!(advice_reason(), "returning the excess over the cap");
    set_headroom(false, false);
    assert_eq!(advice(), Advice::Eager);
    assert_eq!(advice_reason(), "no headroom known");
    // Nothing was flushed: the sticky flag is untouched, so the eager
    // advice reads plain (no "some lazy batches issued").
    assert!(purged_is_zero());
    assert_eq!(advice_name(), "MADV_DONTNEED");
    assert_eq!(batches(), (0, EAGER_BATCHES.load(Ordering::Relaxed)));
}

/// The batch tests flush (for real); the advice test flips
/// the headroom mirror; one at a time.
pub(super) static ADVICE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`ADVICE_TEST`]; a panic in one holder must not fail the rest.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    ADVICE_TEST.lock().unwrap_or_else(|e| e.into_inner())
}

/// A process' worth of `Empty` 1 KiB slabs in arena 0 of `s`: eight
/// slabs' worth of slots allocated and freed through a heap and flushed.
/// Returns the class and how many slabs went `Empty`.
fn empty_slabs(s: &'static Shared) -> (usize, usize) {
    use crate::heap::tests::{TestHeap, class_with};
    let c = class_with(1024);
    let t = TestHeap::new(s);
    let n = 8 * crate::layout::CLASSES[c].nslots();
    let p: std::vec::Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
    for &q in &p {
        t.free(q);
    }
    t.flush_all();
    let empty = s.arena0.classes[c].stats().empty;
    assert!(empty >= 4, "{empty} empty slabs");
    (c, empty)
}

/// `arena.<i>.decay` with the window on (`decay_arena`): the arena's walks
/// keep the same demand floor as the decay driver's pass, over the same
/// trackers; with the window off, or the policy's verdict no, the same
/// call purges down to the curve.
#[test]
fn arena_decay_keeps_the_drivers_demand_floor() {
    use crate::config::Config;
    use crate::testutil::TestShared;
    let _serial = serial();
    const S: u64 = 1_000_000_000;
    let build = |window_ms: u32| {
        let s = TestShared::with(Config { narenas: 1, dirty_decay_ms: 1000, hpa_peak_demand_window_ms: window_ms, ..Config::DEFAULT })
            .build();
        s.policy.apply(crate::policy::Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, s.cgroup.rises().0);
        s
    };
    for (window_ms, apply) in [(60_000u32, true), (0, true), (60_000, false)] {
        let s = build(window_ms);
        let (c, n) = empty_slabs(s);
        let cs = &s.arena0.classes[c];
        let t0 = cs.oldest_empty();
        // Two earlier occurrences of this level, as two earlier bursts
        // of this size would have left them; the walk samples the third.
        let w = Window::from_ms(60_000).unwrap();
        if window_ms != 0 {
            let tr = &arena_demand(s, 0).expect("carved").classes[c];
            for _ in 0..2 {
                tr.sample(w.epoch(t0), n);
                tr.sample(w.epoch(t0), 0);
            }
        }
        if !apply {
            let l = 512u64 << 20;
            s.policy.apply(crate::policy::band(l, l * 8 / 10), l, 0, s.cgroup.rises().0);
            assert!(!s.policy.demand_ok());
        }
        // Past the 1 s curve, mid-epoch (a hold ends at the next epoch
        // boundary): only the floor keeps anything.
        let at = ((t0 + 2 * S) / w.epoch_ns() + 1) * w.epoch_ns() + w.epoch_ns() / 2;
        let next = decay_arena(s, &s.arena0, at);
        let kept = window_ms != 0 && apply;
        let st = cs.stats();
        if kept {
            assert_eq!((st.empty, st.purged), (n, 0), "kept by the floor");
            assert!(next.is_some_and(|d| d > at + S / 200), "a hold: {next:?} at {at}");
            assert_eq!(demand_retained(s, &s.arena0), n * crate::layout::CLASSES[c].slab_size());
            // The driver's pass agrees, and honours the same hold.
            assert_eq!(decay(s, at + S / 100), next);
            assert_eq!(cs.stats().empty, n);
            // Once the window has passed the level, the same call purges.
            let late = at + 60 * S + w.epoch_ns();
            assert_eq!(decay_arena(s, &s.arena0, late), None);
            assert_eq!(cs.stats().empty, 0);
        } else {
            assert_eq!((st.empty, next), (0, None), "window {window_ms} ms, apply {apply}: purged to the curve");
            assert_eq!(demand_retained(s, &s.arena0), 0);
        }
    }
}

/// Fill the metadata region of `s`: every page reserved, then every
/// block of the trackers' bin taken, so the next carve gets null.
fn exhaust_meta(s: &Shared) {
    let page = s.regions.page_size();
    let mut len = 16usize << 20;
    while len >= page {
        while s.meta.reserve(len, page).is_some() {}
        len /= 2;
    }
    let l = core::alloc::Layout::new::<ArenaDemand>();
    while !s.meta.alloc(l).is_null() {}
    let l = core::alloc::Layout::new::<demand::DemandTable>();
    while !s.meta.alloc(l).is_null() {}
}

/// A failed tracker carve (the metadata region full): counted once, the
/// arena decays without a floor on today's curve, nothing is reported as
/// demand-retained, and a later pass does not retry or count it again.
/// The table failing and one arena's trackers failing are both covered;
/// the arenas carved before the failure keep theirs.
#[test]
fn a_failed_tracker_carve_decays_without_a_floor() {
    use crate::config::Config;
    use crate::testutil::TestShared;
    let _serial = serial();
    const S: u64 = 1_000_000_000;
    let conf = Config { narenas: 1, dirty_decay_ms: 1000, hpa_peak_demand_window_ms: 60_000, ..Config::DEFAULT };

    // The table itself: nothing carved before the region filled up.
    let s = TestShared::with(conf).build();
    s.policy.apply(crate::policy::Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, s.cgroup.rises().0);
    let (c, n) = empty_slabs(s);
    exhaust_meta(s);
    let before = demand_carve_failures();
    let t0 = s.arena0.classes[c].oldest_empty();
    assert!(decay(s, t0 + S / 100).is_some(), "a curve deadline");
    assert_eq!(demand_carve_failures(), before + 1);
    assert_eq!(s.demand.load(Ordering::Relaxed), TABLE_FAILED);
    assert!(existing_arena_demand(s, 0).is_err());
    assert_eq!(demand_retained(s, &s.arena0), 0);
    assert!(s.arena0.classes[c].stats().empty <= n);
    // Past the curve everything goes: no floor.
    assert_eq!(decay(s, t0 + 2 * S), None);
    assert_eq!(s.arena0.classes[c].stats().empty, 0);
    assert_eq!(demand_carve_failures(), before + 1, "counted once");

    // One arena's trackers: arena 0 carved, arena 1 created after.
    let s = TestShared::with(conf).build();
    s.policy.apply(crate::policy::Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, s.cgroup.rises().0);
    let _ = decay(s, crate::sys::now_coarse_ns());
    assert!(existing_arena_demand(s, 0).is_ok(), "arena 0 carved");
    let one = s.arenas.create(s).expect("arena 1");
    exhaust_meta(s);
    let before = demand_carve_failures();
    let _ = decay(s, crate::sys::now_coarse_ns());
    assert_eq!(demand_carve_failures(), before + 1);
    assert!(matches!(existing_arena_demand(s, one), Err(false)), "arena 1 marked failed");
    assert!(existing_arena_demand(s, 0).is_ok(), "arena 0 keeps its trackers");
    let _ = decay(s, crate::sys::now_coarse_ns());
    assert_eq!(demand_carve_failures(), before + 1, "not retried");
    assert_eq!(demand_retained(s, s.arenas.get_existing(one).unwrap()), 0);
}
