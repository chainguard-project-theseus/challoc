//! Tests of [`Huge::grow_run`] over the synthetic region of `tests.rs`:
//! where a run can grow, what it leaves of the extent behind it, that
//! its bytes survive and its old guard page becomes usable, and that the
//! allocator's invariants (`audit`) hold after each step.

use super::tests::{Env, page_size};
use super::*;

/// The usable bytes and pages (guard included) of the run at `p`.
fn info(env: &Env, h: &Huge, p: *mut u8) -> (usize, usize) {
    let r = h
        .lookup(env.page_of(p), Tracked::assume_new(), Tracked::assume_new())
        .expect("run descriptor");
    (r.usable, r.pages)
}

/// Grow the run at `p` to `size` bytes and audit.
fn grow(env: &Env, h: &Huge, p: *mut u8, size: usize) -> Option<(u16, usize)> {
    let r = h
        .grow_run(
            &env.meta,
            env.page_of(p),
            size,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    h.audit();
    r
}

/// A huge allocator in the mode `grow_run` supports, or a `SKIP` line.
fn huge(env: &Env, test: &str) -> Option<Huge> {
    if !crate::sys::features::probe_guard_install(page_size()) {
        crate::testutil::skip(
            test,
            "no MADV_GUARD_INSTALL: grow_run declines in the PROT_NONE guard mode",
        );
        return None;
    }
    Some(env.huge_with(true, true))
}

/// Fill `[p, p + n)` with a pattern of `seed`, and check it.
fn fill(p: *mut u8, n: usize, seed: u8) {
    for i in (0..n).step_by(509) {
        // SAFETY: the caller's live run has `n` usable bytes.
        unsafe { p.add(i).write(seed.wrapping_add(i as u8)) };
    }
}
fn check(p: *mut u8, n: usize, seed: u8) {
    for i in (0..n).step_by(509) {
        // SAFETY: as `fill`.
        assert_eq!(
            unsafe { p.add(i).read() },
            seed.wrapping_add(i as u8),
            "byte {i}"
        );
    }
}

#[test]
fn the_last_run_grows_at_the_mark_and_keeps_its_bytes() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::grow_tests::the_last_run_grows_at_the_mark_and_keeps_its_bytes",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 3 * ps, 1);
    fill(a, 3 * ps, 7);
    assert_eq!(grow(&env, &h, a, 10 * ps), Some((0, 7)));
    assert_eq!(info(&env, &h, a), (10 * ps, 11));
    check(a, 3 * ps, 7);
    // The old guard page and the new pages are the run's now.
    fill(a, 10 * ps, 9);
    check(a, 10 * ps, 9);
    let st = h.stats(Tracked::assume_new());
    assert_eq!(
        (st.runs, st.run_pages, st.next_page, st.empty, st.purged),
        (1, 11, 11, 0, 0)
    );
    // The next run starts behind the moved guard.
    let (b, _) = env.alloc(&h, ps, 1);
    assert_eq!(env.page_of(b), 11);
    assert_eq!(env.free(&h, a), 10 * ps);
    env.free(&h, b);
    let st = h.stats(Tracked::assume_new());
    assert_eq!(
        (
            st.runs,
            st.empty + st.purged,
            st.empty_pages + st.purged_pages
        ),
        (0, 1, st.next_page)
    );
}

#[test]
fn a_run_grows_into_the_free_extent_behind_it() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::grow_tests::a_run_grows_into_the_free_extent_behind_it",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 2 * ps, 1); // pages [0, 3)
    let (b, _) = env.alloc(&h, 7 * ps, 1); // pages [3, 11)
    let (c, _) = env.alloc(&h, ps, 1); // pages [11, 13)
    fill(a, 2 * ps, 3);
    env.free(&h, b);
    // Part of the extent: its tail stays free, with one descriptor.
    assert_eq!(grow(&env, &h, a, 5 * ps), Some((0, 3)));
    assert_eq!(info(&env, &h, a), (5 * ps, 6));
    let st = h.stats(Tracked::assume_new());
    assert_eq!(
        (
            st.runs,
            st.run_pages,
            st.empty + st.purged,
            st.empty_pages + st.purged_pages
        ),
        (2, 8, 1, 5)
    );
    // The rest of it, exactly: no free extent is left.
    assert_eq!(grow(&env, &h, a, 10 * ps), Some((0, 5)));
    assert_eq!(info(&env, &h, a), (10 * ps, 11));
    let st = h.stats(Tracked::assume_new());
    assert_eq!((st.runs, st.run_pages, st.empty + st.purged), (2, 13, 0));
    check(a, 2 * ps, 3);
    fill(a, 10 * ps, 5);
    check(a, 10 * ps, 5);
    // `c` is behind it now: no room, and nothing changes.
    assert_eq!(grow(&env, &h, a, 11 * ps), None);
    assert_eq!(info(&env, &h, a), (10 * ps, 11));
    check(a, 10 * ps, 5);
    env.free(&h, c);
    env.free(&h, a);
    let st = h.stats(Tracked::assume_new());
    assert_eq!(
        (
            st.runs,
            st.empty + st.purged,
            st.empty_pages + st.purged_pages
        ),
        (0, 1, st.next_page)
    );
}

#[test]
fn a_run_grows_through_a_trailing_extent_past_the_mark() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::grow_tests::a_run_grows_through_a_trailing_extent_past_the_mark",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, ps, 1); // pages [0, 2)
    let (b, _) = env.alloc(&h, 2 * ps, 1); // pages [2, 5)
    env.free(&h, b);
    // The free extent [2, 5) is too short but touches the mark.
    assert_eq!(grow(&env, &h, a, 8 * ps), Some((0, 7)));
    assert_eq!(info(&env, &h, a), (8 * ps, 9));
    let st = h.stats(Tracked::assume_new());
    assert_eq!(
        (st.runs, st.run_pages, st.next_page, st.empty + st.purged),
        (1, 9, 9, 0)
    );
    fill(a, 8 * ps, 1);
    check(a, 8 * ps, 1);
    env.free(&h, a);
}

#[test]
fn a_run_does_not_grow_into_a_live_run_or_a_short_extent() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::grow_tests::a_run_does_not_grow_into_a_live_run_or_a_short_extent",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, ps, 1); // [0, 2)
    let (b, _) = env.alloc(&h, ps, 1); // [2, 4)
    let (c, _) = env.alloc(&h, ps, 1); // [4, 6)
    let before = h.stats(Tracked::assume_new());
    assert_eq!(grow(&env, &h, a, 2 * ps), None);
    env.free(&h, b);
    // Two free pages behind `a`, three wanted, and `c` is in the way.
    assert_eq!(grow(&env, &h, a, 4 * ps), None);
    // The same size or a smaller one is no growth.
    assert_eq!(grow(&env, &h, a, ps), None);
    assert_eq!(info(&env, &h, a), (ps, 2));
    assert_eq!(h.stats(Tracked::assume_new()).next_page, before.next_page);
    // Two pages fit.
    assert_eq!(grow(&env, &h, a, 3 * ps), Some((0, 2)));
    // Past the end of the region.
    assert_eq!(grow(&env, &h, c, usize::MAX / 2), None);
    env.free(&h, a);
    env.free(&h, c);
}

#[test]
fn the_other_guard_modes_decline() {
    let env = Env::new();
    let h = env.huge_with(false, true);
    let ps = page_size();
    let (a, _) = env.alloc(&h, ps, 1);
    assert_eq!(grow(&env, &h, a, 4 * ps), None);
    assert_eq!(info(&env, &h, a), (ps, 2));
    env.free(&h, a);
}

#[test]
fn guards_off_grows_without_markers() {
    let env = Env::new();
    let h = env.huge_with(false, false);
    let ps = page_size();
    let (a, _) = env.alloc(&h, ps, 1);
    fill(a, ps, 2);
    assert_eq!(grow(&env, &h, a, 6 * ps), Some((0, 5)));
    check(a, ps, 2);
    fill(a, 6 * ps, 4);
    check(a, 6 * ps, 4);
    env.free(&h, a);
}

#[test]
fn a_run_grows_by_one_page() {
    let env = Env::new();
    let Some(h) = huge(&env, "huge::grow_tests::a_run_grows_by_one_page") else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 3 * ps, 1); // pages [0, 4)
    let (b, _) = env.alloc(&h, ps, 1); // pages [4, 6)
    let (c, _) = env.alloc(&h, ps, 1); // pages [6, 8)
    fill(a, 3 * ps, 6);
    env.free(&h, b);
    // One page at a time, as an exact-size growth asks: the old guard
    // page, then the next.
    assert_eq!(grow(&env, &h, a, 3 * ps + 1), Some((0, 1)));
    assert_eq!(info(&env, &h, a), (4 * ps, 5));
    assert_eq!(grow(&env, &h, a, 5 * ps), Some((0, 1)));
    assert_eq!(info(&env, &h, a), (5 * ps, 6));
    // The extent is gone; `c` is behind the run.
    assert_eq!(grow(&env, &h, a, 5 * ps + 1), None);
    check(a, 3 * ps, 6);
    fill(a, 5 * ps, 8);
    check(a, 5 * ps, 8);
    env.free(&h, a);
    env.free(&h, c);
}

#[test]
fn a_refused_marker_declines() {
    let env = Env::new();
    let Some(h) = huge(&env, "huge::grow_tests::a_refused_marker_declines") else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, ps, 1);
    // A `PROT_NONE` guard may lie below the mark: the move splits it off.
    h.mixed_guards.store(true, Ordering::Relaxed);
    assert_eq!(grow(&env, &h, a, 4 * ps), None);
    assert_eq!(info(&env, &h, a), (ps, 2));
    env.free(&h, a);
}

/// `Live::take_run` lends a run token out while its block's permission
/// stays held (`heap/live.rs`, `axiom_hand_out`'s discipline): its one
/// caller is `Shared::grow_huge`, which hands the token to `grow_run` and
/// puts back what comes back with `Live::put_run`, its one caller too.
#[test]
fn take_run_and_put_run_only_in_grow_huge() {
    let mut files = std::vec::Vec::new();
    super::tests::src_files(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        "",
        &mut files,
    );
    // Split, so this test's own source does not match.
    let needles = [[".take", "_run()"].concat(), [".put", "_run("].concat()];
    let mut calls = [0, 0];
    for (rel, text) in &files {
        let mut in_grow_huge = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("pub(crate) fn grow_huge(") {
                in_grow_huge = true;
            } else if in_grow_huge && line == "    }" {
                in_grow_huge = false;
            }
            if t.starts_with("//") {
                continue;
            }
            for (i, n) in needles.iter().enumerate() {
                if t.contains(n.as_str()) {
                    assert!(
                        rel == "heap/shared.rs" && in_grow_huge,
                        "{rel}: got = {t}, want = a call in Shared::grow_huge only"
                    );
                    calls[i] += 1;
                }
            }
        }
    }
    assert_eq!(calls, [1, 1], "take_run and put_run calls in grow_huge");
}
