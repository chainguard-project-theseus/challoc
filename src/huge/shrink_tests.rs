//! Tests of [`Huge::shrink_run`] over the synthetic region of `tests.rs`:
//! what a shrunk run keeps, what becomes of the pages it releases (freed
//! as a run's are: coalesced, purged or left for decay), when it
//! declines, and that the allocator's invariants (`audit`) hold after
//! each step.

use super::tests::{Env, page_size};
use super::*;

/// The usable bytes and pages (guard included) of the run at `p`.
fn info(env: &Env, h: &Huge, p: *mut u8) -> (usize, usize) {
    let r = h
        .lookup(env.page_of(p), Tracked::assume_new(), Tracked::assume_new())
        .expect("run descriptor");
    (r.usable, r.pages)
}

/// The state of the extent the table keys at `page`.
fn state_at(h: &Huge, page: usize) -> RunState {
    h.lookup(page, Tracked::assume_new(), Tracked::assume_new())
        .expect("extent descriptor")
        .state
}

/// Shrink the run at `p` to `size` bytes and audit; then, as
/// `Shared::shrink_huge` does once it has given up the permission to the
/// tail's bytes, free the tail `shrink_run` returns (a live run until
/// then), and audit again.
fn shrink(env: &Env, h: &Huge, p: *mut u8, size: usize) -> Option<(u16, usize)> {
    let (r, _, _) = h.shrink_run(
        &env.meta,
        env.page_of(p),
        size,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.audit();
    let (arena, k, tail) = r?;
    assert_eq!(
        state_at(h, tail),
        RunState::Active,
        "the tail before its free"
    );
    h.free_run(
        &env.meta,
        tail,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.audit();
    Some((arena, k))
}

/// A huge allocator in the mode `shrink_run` supports, or a `SKIP` line.
fn huge(env: &Env, test: &str) -> Option<Huge> {
    if !crate::sys::features::probe_guard_install(page_size()) {
        crate::testutil::skip(
            test,
            "no MADV_GUARD_INSTALL: shrink_run declines in the PROT_NONE guard mode",
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
        let b = unsafe { p.add(i).read() };
        assert_eq!(b, seed.wrapping_add(i as u8), "byte {i}");
    }
}

/// (runs, run pages, free extents, free pages, mark).
fn tally(h: &Huge) -> (usize, usize, usize, usize, usize) {
    let st = h.stats(Tracked::assume_new());
    (
        st.runs,
        st.run_pages,
        st.empty + st.purged,
        st.empty_pages + st.purged_pages,
        st.next_page,
    )
}

#[test]
fn the_last_run_shrinks_and_its_tail_is_freed() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::the_last_run_shrinks_and_its_tail_is_freed",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 10 * ps, 1); // pages [0, 11)
    fill(a, 10 * ps, 7);
    assert_eq!(shrink(&env, &h, a, 3 * ps - 5), Some((0, 7)));
    // A new run's page count for the size: the sized frees' rule.
    assert_eq!(info(&env, &h, a), (3 * ps, 4));
    check(a, 3 * ps, 7);
    // The released pages [4, 11) are one `Empty` extent, aging.
    assert_eq!(tally(&h), (1, 4, 1, 7, 11));
    assert_eq!(state_at(&h, 4), RunState::Empty);
    // The next run starts right behind the new guard, in the tail.
    let (b, _) = env.alloc(&h, ps, 1);
    assert_eq!(env.page_of(b), 4);
    env.free(&h, b);
    assert_eq!(env.free(&h, a), 3 * ps);
    assert_eq!(tally(&h), (0, 0, 1, 11, 11));
}

#[test]
fn the_released_tail_coalesces_with_the_free_extent_after_it() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::the_released_tail_coalesces_with_the_free_extent_after_it",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 10 * ps, 1); // pages [0, 11)
    let (b, _) = env.alloc(&h, 2 * ps, 1); // pages [11, 14)
    let (c, _) = env.alloc(&h, ps, 1); // pages [14, 16)
    fill(a, 10 * ps, 1);
    env.free(&h, b);
    assert_eq!(shrink(&env, &h, a, 2 * ps), Some((0, 8)));
    assert_eq!(info(&env, &h, a), (2 * ps, 3));
    check(a, 2 * ps, 1);
    // [3, 11) and [11, 14) are one extent.
    assert_eq!(tally(&h), (2, 5, 1, 11, 16));
    // A run that fills the merged extent exactly lands at the tail.
    let (d, _) = env.alloc(&h, 10 * ps, 1);
    assert_eq!(env.page_of(d), 3);
    env.free(&h, d);
    env.free(&h, c);
    env.free(&h, a);
    assert_eq!(tally(&h), (0, 0, 1, 16, 16));
}

#[test]
fn a_shrunk_run_shrinks_and_grows_again() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::a_shrunk_run_shrinks_and_grows_again",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 20 * ps, 1); // pages [0, 21)
    fill(a, 20 * ps, 4);
    assert_eq!(shrink(&env, &h, a, 12 * ps), Some((0, 8)));
    assert_eq!(shrink(&env, &h, a, 5 * ps), Some((0, 7)));
    assert_eq!(info(&env, &h, a), (5 * ps, 6));
    check(a, 5 * ps, 4);
    // The released pages are one extent at the mark: the run grows back
    // into them.
    assert_eq!(tally(&h), (1, 6, 1, 15, 21));
    assert_eq!(
        h.grow_run(
            &env.meta,
            env.page_of(a),
            9 * ps,
            Tracked::assume_new(),
            Tracked::assume_new()
        )
        .0,
        Some((0, 4))
    );
    h.audit();
    check(a, 5 * ps, 4);
    fill(a, 9 * ps, 6);
    check(a, 9 * ps, 6);
    env.free(&h, a);
}

#[test]
fn a_shrink_declines_without_two_pages_to_release() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::a_shrink_declines_without_two_pages_to_release",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 4 * ps, 1); // pages [0, 5)
    fill(a, 4 * ps, 2);
    let before = tally(&h);
    // The same page count, more pages, and one page fewer (the old
    // guard alone would be released): nothing changes.
    assert_eq!(shrink(&env, &h, a, 4 * ps - 1), None);
    assert_eq!(shrink(&env, &h, a, 5 * ps), None);
    assert_eq!(shrink(&env, &h, a, 3 * ps), None);
    assert_eq!(info(&env, &h, a), (4 * ps, 5));
    assert_eq!(tally(&h), before);
    // Two pages fewer releases two.
    assert_eq!(shrink(&env, &h, a, 2 * ps), Some((0, 2)));
    assert_eq!(info(&env, &h, a), (2 * ps, 3));
    check(a, 2 * ps, 2);
    // A size past the region rounds to no page count.
    assert_eq!(shrink(&env, &h, a, usize::MAX), None);
    env.free(&h, a);
}

#[test]
fn the_fallback_guard_mode_and_mixed_guards_decline() {
    let env = Env::new();
    let h = env.huge_with(false, true);
    let ps = page_size();
    let (a, _) = env.alloc(&h, 8 * ps, 1);
    assert_eq!(shrink(&env, &h, a, 2 * ps), None);
    assert_eq!(info(&env, &h, a), (8 * ps, 9));
    env.free(&h, a);
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::the_fallback_guard_mode_and_mixed_guards_decline",
    ) else {
        return;
    };
    let (a, _) = env.alloc(&h, 8 * ps, 1);
    // A `PROT_NONE` guard may lie below the mark: the move splits it off.
    h.mixed_guards.store(true, Ordering::Relaxed);
    assert_eq!(shrink(&env, &h, a, 2 * ps), None);
    assert_eq!(info(&env, &h, a), (8 * ps, 9));
    env.free(&h, a);
}

#[test]
fn guards_off_shrinks_without_markers() {
    let env = Env::new();
    let h = env.huge_with(false, false);
    let ps = page_size();
    let (a, _) = env.alloc(&h, 8 * ps, 1);
    fill(a, 8 * ps, 3);
    assert_eq!(shrink(&env, &h, a, 3 * ps), Some((0, 5)));
    assert_eq!(info(&env, &h, a), (3 * ps, 4));
    check(a, 3 * ps, 3);
    // Without a guard the page after the run is read-write: the next
    // run's.
    let (b, _) = env.alloc(&h, ps, 1);
    assert_eq!(env.page_of(b), 4);
    fill(b, ps, 9);
    check(a, 3 * ps, 3);
    env.free(&h, a);
    env.free(&h, b);
}

/// The tail is freed as a run is: purged at once when its usable bytes
/// are above `huge_retain_max`, or under `huge_dirty_decay_ms:0`.
#[test]
fn the_tail_is_purged_as_a_freed_run_would_be() {
    let env = Env::new();
    let Some(h) = huge(
        &env,
        "huge::shrink_tests::the_tail_is_purged_as_a_freed_run_would_be",
    ) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 20 * ps, 1); // pages [0, 21)
    let (b, _) = env.alloc(&h, 20 * ps, 1); // pages [21, 42)
    // Above the cap: a tail of 17 pages, 16 usable, is purged.
    h.set_retain_max(8 * ps);
    assert_eq!(shrink(&env, &h, a, 3 * ps), Some((0, 17)));
    assert_eq!(state_at(&h, 4), RunState::Purged);
    // At or below it, `Empty`, unless the decay period is zero.
    assert_eq!(shrink(&env, &h, b, 15 * ps), Some((0, 5)));
    assert_eq!(state_at(&h, 37), RunState::Empty);
    h.set_purge_on_free(true);
    assert_eq!(shrink(&env, &h, b, 12 * ps), Some((0, 3)));
    // It merged into the `Empty` extent after it, which keeps its state.
    assert_eq!(state_at(&h, 34), RunState::Empty);
    h.set_purge_on_free(false);
    env.free(&h, a);
    env.free(&h, b);
}

/// A shrink of a run a racing free already freed aborts, as the free its
/// move would end in does (`free_run`'s checks).
#[test]
#[should_panic(expected = "huge allocation already freed")]
fn a_shrink_of_a_freed_run_aborts() {
    let env = Env::new();
    let h = env.huge_with(false, false);
    let ps = page_size();
    let (a, _) = env.alloc(&h, 8 * ps, 1);
    let (_b, _) = env.alloc(&h, ps, 1);
    env.free(&h, a);
    shrink(&env, &h, a, 2 * ps);
}

/// The same, when the freed run merged into the free extent before it.
#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn a_shrink_of_a_run_merged_into_a_free_extent_aborts() {
    let env = Env::new();
    let h = env.huge_with(false, false);
    let ps = page_size();
    let (a, _) = env.alloc(&h, 2 * ps, 1);
    let (b, _) = env.alloc(&h, 8 * ps, 1);
    let (_c, _) = env.alloc(&h, ps, 1);
    env.free(&h, a);
    env.free(&h, b);
    shrink(&env, &h, b, 2 * ps);
}

/// The kernel refuses the new guard's marker (`EINVAL` on `mlock`ed
/// pages): the shrink declines before any bookkeeping, and the run keeps
/// its size and bytes.
#[test]
fn a_marker_the_kernel_refuses_declines() {
    let env = Env::new();
    let test = "huge::shrink_tests::a_marker_the_kernel_refuses_declines";
    let Some(h) = huge(&env, test) else {
        return;
    };
    let ps = page_size();
    let (a, _) = env.alloc(&h, 8 * ps, 1); // pages [0, 9)
    fill(a, 8 * ps, 5);
    if !crate::testutil::mlock_or_skip(test, a as usize, 8 * ps) {
        return;
    }
    let before = tally(&h);
    assert_eq!(shrink(&env, &h, a, 2 * ps), None);
    assert_eq!(info(&env, &h, a), (8 * ps, 9));
    assert_eq!(tally(&h), before);
    check(a, 8 * ps, 5);
    // SAFETY: unlocking the test's own live run; no memory is touched.
    unsafe { libc::munlock(a as *const _, 8 * ps) };
    assert_eq!(shrink(&env, &h, a, 2 * ps), Some((0, 6)));
    check(a, 2 * ps, 5);
    env.free(&h, a);
}

/// Fork epochs (design 5): a shrink while the floors hold goes through,
/// and its tail is freed as any run freed in the epoch is, an `Empty`
/// extent whose pages the child may share, so a growth under the floors
/// declines to take them back.
#[test]
fn under_the_floors_a_run_shrinks_and_does_not_grow_back() {
    crate::testutil::in_fork_child("shrink under the floors", 30_000, move || {
        let env = Env::new();
        let Some(h) = huge(
            &env,
            "huge::shrink_tests::under_the_floors_a_run_shrinks_and_does_not_grow_back",
        ) else {
            return;
        };
        let ps = page_size();
        let (a, _) = env.alloc(&h, 10 * ps, 1); // pages [0, 11)
        let (c, _) = env.alloc(&h, ps, 1); // pages [11, 13)
        fill(a, 10 * ps, 8);
        crate::cow::test_set_in_force(true);
        assert_eq!(shrink(&env, &h, a, 3 * ps), Some((0, 7)));
        assert_eq!(state_at(&h, 4), RunState::Empty);
        assert_eq!(
            h.grow_run(
                &env.meta,
                env.page_of(a),
                6 * ps,
                Tracked::assume_new(),
                Tracked::assume_new()
            )
            .0,
            None
        );
        check(a, 3 * ps, 8);
        crate::cow::test_set_in_force(false);
        env.free(&h, a);
        env.free(&h, c);
    });
}
