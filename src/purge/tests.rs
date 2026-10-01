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
        d.decay(now, decay_ns, |at, curve| {
            walks.borrow_mut().push((at, *curve));
            d.set_oldest_empty(leave);
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

/// The page size and the `MADV_POPULATE_WRITE` probe of the kernel, as
/// `api::init` reads them (the 5.10 and 64 KiB-page matrix cells found a
/// 4 KiB constant and a forced populate word).
fn prefault_env() -> (usize, bool) {
    let page = crate::sys::features::page_size();
    (page, crate::sys::features::probe_populate_write(page))
}

/// The prefault pass's plumbing, at the kernel's page size and in the
/// populate direction the real probe reports: `populate` counts the
/// bytes; configured as the probe says it never falls back (the advice
/// works, or the touch is the populate); told the advice works where the
/// probe says it does not (before 5.14), one fallback, the bytes still
/// counted and the touch done, so a zero buffer stays zero either way;
/// `note_commit` counts a wake only while the pass is enabled;
/// `configure` is what `enabled` and the populate word report.
#[test]
fn prefault_populate_and_wakes_are_counted() {
    let _serial = serial();
    let (page, usable) = prefault_env();
    std::println!(
        "prefault test: page_size={page} populate_write={} (probed)",
        if usable { "yes" } else { "no" }
    );
    let buf = std::vec![0u8; 4 * page + page];
    let addr = (buf.as_ptr() as usize + page - 1) & !(page - 1);
    prefault::reset_for_test(false, false);
    assert!(!prefault::enabled() && !prefault::uses_populate_write());
    prefault::note_commit();
    assert_eq!(prefault::stats().wakes, 0, "off: no wake");
    prefault::populate(addr, 4 * page, page);
    let s = prefault::stats();
    assert_eq!((s.bytes, s.fallbacks), (4 * page as u64, 0), "the touch is the populate, not a fallback");
    assert!(buf.iter().all(|&b| b == 0));
    // The probe's direction, as `api::init` configures the pass.
    prefault::reset_for_test(true, usable);
    assert!(prefault::enabled() && prefault::uses_populate_write() == usable);
    prefault::note_commit();
    prefault::note_commit();
    // At least this test's two wakes (other tests' fresh commits wake
    // too while the pass is on here).
    assert!(prefault::stats().wakes >= 2);
    prefault::populate(addr, 2 * page, page);
    let s = prefault::stats();
    assert_eq!((s.bytes, s.fallbacks), (2 * page as u64, 0), "the probe's direction never falls back");
    assert!(buf.iter().all(|&b| b == 0));
    // The fallback contract: the populate word set where the kernel
    // lacks the advice (or the address is not page-aligned), the
    // `madvise` fails, one fallback is counted, the bytes are counted
    // and the touch populates; where the probe says yes this is the
    // probe's direction again.
    prefault::reset_for_test(true, true);
    prefault::populate(addr, page, page);
    let s = prefault::stats();
    let expected = if usable { 0 } else { 1 };
    assert_eq!((s.bytes, s.fallbacks), (page as u64, expected), "forced populate_write against the probe");
    assert!(buf.iter().all(|&b| b == 0));
    std::println!(
        "prefault test: probed direction fallbacks=0, forced populate_write fallbacks={} (expected {expected})",
        s.fallbacks
    );
    prefault::reset_for_test(false, false);
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
    assert_eq!(backend_name().1, "MADV_FREE");
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
    configure_advice(false, false);
    assert_eq!(advice(), Advice::Eager);
    assert_eq!(advice_reason(), "lazy_band:false");
    assert!(!lazy_uses_process_madvise());
    configure_advice(false, true);
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
    assert_eq!(backend_name().1, "MADV_DONTNEED");
    assert_eq!(batches(), (0, EAGER_BATCHES.load(Ordering::Relaxed)));
}

/// The batch tests flush (for real); the advice test flips
/// the headroom mirror; one at a time.
pub(super) static ADVICE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`ADVICE_TEST`]; a panic in one holder must not fail the rest.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    ADVICE_TEST.lock().unwrap_or_else(|e| e.into_inner())
}
