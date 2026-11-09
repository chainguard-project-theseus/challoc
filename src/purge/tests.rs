//! Purge tests of the decay driver's logic: the `Decayer`'s steps and
//! back-off, `Batch` merging and flushing, and the cgroup advice, checked
//! against their own state. A test of what the kernel does with a discard
//! (residency, locked, unmapped or sealed ranges) belongs in
//! `linux_tests.rs`.

use super::*;
use crate::testutil::{map_rw, page_size};

/// The owners' decay step (`ClassState::decay_with`, `Huge::decay_with`)
/// with `walk` for the owner's walk.
fn decay(d: &Decayer, now: u64, decay_ns: u64, walk: impl FnOnce(u64, &Curve)) -> Option<u64> {
    match d.due(now, decay_ns) {
        Due::Done(r) => r,
        Due::Walk(curve, step, seen) => {
            walk(now, &curve);
            d.finish(now, step, Ghost::assume_new(), seen)
        }
    }
}

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
        decay(d, now, decay_ns, |at, curve| {
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

/// A walk that backs the gate off (a failed discard) holds the next walk
/// off for `DISCARD_RETRY_NS` whatever the period, a zero one included,
/// and reports the back-off's end as the deadline; after it the walks
/// run again.
#[test]
fn decayer_backs_off_after_a_failed_discard() {
    let d = Decayer::new();
    let walks = core::cell::Cell::new(0);
    let walk = |now: u64, fail: bool| {
        decay(&d, now, 0, |at, _| {
            walks.set(walks.get() + 1);
            if fail {
                d.back_off(at);
            }
        })
    };
    assert!(d.note_first_empty(1_000));
    // A zero period walks at every call.
    assert_eq!(walk(1_000, false), Some(1_000));
    assert_eq!(walk(1_001, false), Some(1_001));
    assert_eq!(walks.get(), 2);
    let retry = 1_002 + DISCARD_RETRY_NS;
    assert_eq!(walk(1_002, true), Some(retry));
    assert_eq!(walks.get(), 3);
    assert_eq!(walk(1_003, false), Some(retry));
    assert_eq!(walk(retry - 1, false), Some(retry));
    assert_eq!(walks.get(), 3);
    assert_eq!(walk(retry, false), Some(retry));
    assert_eq!(walk(retry + 1, false), Some(retry + 1));
    assert_eq!(walks.get(), 5);
}

#[test]
fn batch_merges_adjacent_ranges() {
    let _serial = serial();
    let page = page_size();
    let base = map_rw(8 * page);
    let mut b = Batch::new_t();
    b.push_t(base, page, |_, _, done| assert!(done));
    b.push_t(base + page, page, |_, _, done| assert!(done));
    b.push_t(base + 2 * page, page, |_, _, done| assert!(done));
    assert_eq!(b.len(), 1);
    assert_eq!(b.ranges[0], (base, 3 * page, page));
    // A range of another length starts a new range, so does a gap, and
    // so does a range that is not the next one.
    b.push_t(base + 3 * page, 2 * page, |_, _, done| assert!(done));
    assert_eq!(b.len(), 2);
    assert_eq!(b.ranges[1], (base + 3 * page, 2 * page, 2 * page));
    b.push_t(base + 6 * page, page, |_, _, done| assert!(done));
    assert_eq!(b.len(), 3);
    b.push_t(base + 5 * page, page, |_, _, done| assert!(done));
    assert_eq!(b.len(), 4);
    b.flush_t(|_, _, done| assert!(done));
    assert!(b.is_empty());
}

#[test]
fn batch_fills_and_flushes() {
    let _serial = serial();
    let (_, eager0) = batches();
    let page = page_size();
    let base = map_rw((2 * MAX_BATCH + 4) * page);
    let mut b = Batch::new_t();
    assert!(b.is_empty());
    // Every other page, so nothing merges.
    for i in 0..MAX_BATCH {
        assert!(!b.is_full());
        b.push_t(base + 2 * page * i, page, |_, _, done| assert!(done));
    }
    assert!(b.is_full());
    assert_eq!(b.len(), MAX_BATCH);
    // The 65th push flushes the first 64 first, reporting each.
    let mut settled = std::vec::Vec::new();
    b.push_t(base + 2 * page * MAX_BATCH, 2 * page, |addr, len, done| {
        settled.push((addr, len, done))
    });
    assert_eq!(b.len(), 1);
    let want: std::vec::Vec<_> = (0..MAX_BATCH)
        .map(|i| (base + 2 * page * i, page, true))
        .collect();
    assert_eq!(settled, want);
    b.flush_t(|_, _, done| assert!(done));
    assert!(b.is_empty());
    b.flush_t(|_, _, done| assert!(done));
    assert!(b.is_empty());
    // At least the two eager batches of this test were counted (the
    // default advice; other tests flush their own batches concurrently
    // in this process), none lazy.
    assert!(batches().1 >= eager0 + 2);
    assert!(purged_is_zero());
}

/// For the advice tests, in a fork child: an explicit purge another
/// thread had in flight at the fork never ends there (that thread is not
/// in the child), so its count is dropped; nothing else is reset.
pub(super) fn forget_other_threads_explicit_purges() {
    EXPLICIT.store(0, Ordering::Relaxed);
}

/// The default (no limit) discards with `MADV_DONTNEED`, so purged
/// memory is zero. The headroom band is exercised end to end by
/// `tests/c/cgroup_lazy.c`; issuing a lazy batch here would make the flag
/// sticky for the class and huge tests that read it. In a fork child: in
/// this process an explicit purge of another test reads as "explicit
/// purge".
#[test]
fn purged_memory_is_zero_by_default() {
    crate::testutil::in_fork_child("purge default advice", 30_000, || {
        forget_other_threads_explicit_purges();
        assert!(purged_is_zero());
        assert_eq!(advice(), Advice::Eager);
        assert_eq!(advice_reason(), "no headroom known");
    });
}

/// The advice's process-wide wiring: the
/// policy's headroom mirror, the `lazy_band` key and an explicit purge
/// in flight, read without any batch being flushed. In a fork child: in
/// this process any test's policy sample resets the mirror
/// (`policy::evaluate` without a limit) between the write and the read,
/// and a concurrent flush under the flipped mirror would issue a lazy
/// batch and make [`purged_is_zero`] false for the class and huge
/// tests.
#[test]
fn advice_follows_headroom_band_key_and_explicit_purges() {
    crate::testutil::in_fork_child("purge advice", 30_000, || {
        forget_other_threads_explicit_purges();
        assert_eq!(advice(), Advice::Eager);
        // The headroom band's lazy advice is an opt-in (`lazy_band:true`).
        configure_advice(true);
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
    });
}

/// The batch tests flush (for real), one at a time; the advice test,
/// which flips the headroom mirror, runs in a fork child instead.
pub(super) static BATCH_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`BATCH_SERIAL`]; a panic in one holder must not fail the rest.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    BATCH_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// `api::axiom_fork_resumed_state` takes the `Lock<State>` its handler
/// passes (`purge::background::state`'s `&STATE`; the test
/// `api::tests::fork_hold_is_resumed_once` pins the argument) to be the
/// lock `fork_prepare` held, which is sound only while `STATE` is the one
/// `Lock<State>` the program builds. Only `background.rs` (with the modules inside it)
/// can build a `State`, its fields being private (the compiler checks
/// that), and no other file gets one from it: `State` has no `Clone`,
/// `Copy` or `Default`, and no function returns one (`-> State`,
/// `-> (r: State)`). `background.rs` builds one `State` (one struct
/// literal, no `ptr::read` copy) and one lock (one `Lock::new`, no
/// `Lock::<..>` path), `STATE`, at that rank. The premise is private to
/// `api` (declared at the top level of `api/mod.rs`, without `pub`). It
/// is trusted, not proved, and these source counts, with the argument
/// pin above, are its only guard: if a count changes, re-audit the
/// premise's soundness before updating the expected numbers.
/// Not seen: a `State` inside a returned tuple or `Option`, and a
/// trait impl written by a macro.
#[test]
fn state_lock_is_the_one_lock_of_its_type() {
    let src = include_str!("background.rs");
    let code = || src.lines().filter(|l| !l.trim_start().starts_with("//"));
    let count = |s: &str| code().filter(|l| l.contains(s)).count();
    assert_eq!(count("static STATE: Lock<State>"), 1);
    let premise = std::format!("axiom fn {}(", "axiom_fork_resumed_state");
    assert_eq!(
        include_str!("../api/mod.rs")
            .lines()
            .filter(|l| l.contains(&premise))
            .map(|l| l.starts_with(&premise))
            .collect::<std::vec::Vec<_>>(),
        std::vec![true],
        "the premise is declared once, private, at the module's top level"
    );
    assert_eq!(count("Lock::new("), 1, "STATE's initialiser");
    assert_eq!(count("Lock::<"), 0, "no other path to the constructor");
    assert_eq!(
        count("State {"),
        3,
        "the struct, STATE's initial value and the LockSpec impl"
    );
    assert_eq!(count("State{"), 0, "a literal has a space before its brace");
    assert_eq!(count("ptr::read"), 0, "no bitwise copy of a value");
    assert_eq!(count("Ghost(rank_state())"), 1);
    // `State` derives nothing: its attributes and doc stand right above it.
    let lines: std::vec::Vec<&str> = src.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == "pub(crate) struct State {")
        .expect("State's declaration");
    for l in lines[..at].iter().rev().map(|l| l.trim()) {
        if !(l.starts_with("#[") || l.starts_with("//")) {
            break;
        }
        assert!(!l.contains("derive"), "State derives a trait: {l}");
    }
    // No impl in src/ gives this `State` `Clone`, `Copy` or `Default`,
    // and no function returns one: in `background.rs` and in each file
    // that imports it (`use ..background::{.., State, ..}`) by its name,
    // elsewhere by its path (`background::State`). Other modules have
    // types of their own named `State`.
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let word = |l: &str, p: &str| {
        l.match_indices(p)
            .any(|(i, _)| !l[..i].ends_with(ident) && !l[i + p.len()..].starts_with(ident))
    };
    // Whether the path `t` is `ty`, or ends in it when `ty` is a path.
    let is =
        |t: &str, ty: &str| t == ty || (ty.contains("::") && t.ends_with(&std::format!("::{ty}")));
    // Whether `l` names `Tr for ty`, the head of an impl.
    let impl_of = |l: &str, tr: &str, ty: &str| {
        let p = std::format!("{tr} for ");
        l.match_indices(p.as_str()).any(|(i, _)| {
            let t = &l[i + p.len()..];
            is(t.split(|c: char| !ident(c) && c != ':').next().unwrap(), ty)
        })
    };
    // Whether `l` returns `ty` (`-> ty` or `-> (r: ty)`).
    let returns = |l: &str, ty: &str| {
        l.match_indices("-> ").any(|(i, _)| {
            let r = &l[i + 3..];
            let r = match r.strip_prefix('(') {
                Some(r) => r.split_once(": ").map_or("", |(_, t)| t),
                None => r,
            };
            is(r.split(|c: char| !ident(c) && c != ':').next().unwrap(), ty)
        })
    };
    let mut files = std::vec::Vec::new();
    crate::testutil::walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let name = std::format!("{}{}", "Sta", "te");
    let path = std::format!("background::{name}");
    let mut scanned = 0;
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let code: std::vec::Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        // Each `use` statement, joined up to its `;`.
        let mut uses: std::vec::Vec<std::string::String> = std::vec::Vec::new();
        let mut open = false;
        for l in &code {
            let t = l.trim();
            if !open && (t.starts_with("use ") || t.contains(" use ")) {
                uses.push(std::string::String::new());
                open = true;
            }
            if open {
                uses.last_mut().unwrap().push_str(t);
                open = !t.contains(';');
            }
        }
        let imports = f.ends_with("purge/background.rs")
            || uses
                .iter()
                .any(|u| u.contains("background::") && word(u, &name));
        let mut tys = std::vec![path.as_str()];
        if imports {
            tys.push(name.as_str());
            scanned += 1;
        }
        for l in &code {
            for ty in &tys {
                for tr in ["Clone", "Copy", "Default"] {
                    assert!(
                        !impl_of(l, tr, ty),
                        "{}: `{tr} for {ty}`: {}",
                        f.display(),
                        l.trim()
                    );
                }
                assert!(
                    !returns(l, ty),
                    "{}: returns a `{ty}`: {}",
                    f.display(),
                    l.trim()
                );
            }
        }
    }
    assert!(scanned >= 2, "background.rs and the files importing State");
}
