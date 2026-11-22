//! Fork epochs (`crate::cow`; design 5, "Fork epochs") on a test
//! `Shared`. Each test runs in a fork child: an epoch's state is
//! process-wide (the cache-flush stamp reaches every heap), and the other
//! tests of this process must not see it.

use super::*;
use crate::testutil::in_fork_child;

/// The slab of `p` in class `c`'s region.
fn slab_of(s: &Shared, c: usize, p: *mut u8) -> usize {
    s.table(c).slab_index(p)
}

/// The floor of `c` in arena 0, while the epoch is in force.
fn floor0(s: &Shared, c: usize) -> usize {
    s.arena0.class(c).cow_floor()
}

#[test]
fn refills_under_an_epoch_stay_above_the_floor() {
    in_fork_child("cow floors", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let nslots = CLASSES[c].nslots();
        // Four slabs, every other slot freed: holes in every slab.
        let all: Vec<*mut u8> = (0..4 * nslots).map(|_| t.alloc(c)).collect();
        for p in all.iter().step_by(2) {
            t.free(*p);
        }
        t.flush_all();
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        let f = floor0(s, c);
        assert!(f > 0, "the floor is the commit cursor");
        assert!(
            all.iter().all(|&p| slab_of(s, c, p) < f),
            "every block of before is below the floor"
        );
        let new: Vec<*mut u8> = (0..2 * nslots).map(|_| t.alloc(c)).collect();
        for &p in &new {
            assert!(
                slab_of(s, c, p) >= f,
                "a refill under the floor took {p:?} of slab {}",
                slab_of(s, c, p)
            );
            assert!(!all.contains(&p));
        }
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
        assert_eq!(floor0(s, c), 0, "no floor after the epoch");
        assert_eq!(
            s.arena0.class(c).drain_floor(),
            f,
            "the floor became the drain floor"
        );
        // After the epoch, address-ordered first fit refills the holes.
        t.flush_all();
        let p = t.alloc(c);
        assert!(
            slab_of(s, c, p) < f,
            "after the epoch the holes below the floor are refilled first"
        );
    });
}

#[test]
fn frees_below_the_floor_stay_out_of_the_cache() {
    in_fork_child("cow deflected frees", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let old: Vec<*mut u8> = (0..64).map(|_| t.alloc(c)).collect();
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        assert!(s.free_flags() & FREE_EPOCH != 0, "the dispatch bit is set");
        for &p in &old[..32] {
            t.free(p);
        }
        let cached = t.cached_plain(c);
        for &p in &old[..32] {
            assert!(
                !cached.contains(&p),
                "{p:?} below the floor went to the cache malloc pops"
            );
        }
        assert!(
            crate::cow::stat(s, 6, Tracked::assume_new()) >= 32,
            "the deflected frees are counted"
        );
        for _ in 0..256 {
            let p = t.alloc(c);
            assert!(
                !old.contains(&p),
                "{p:?} of before the epoch handed out again"
            );
        }
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
        assert!(
            s.free_flags() & FREE_EPOCH == 0,
            "the dispatch bit is cleared"
        );
    });
}

#[test]
fn another_heaps_cache_is_flushed_at_its_next_slow_path() {
    in_fork_child("cow cache flush", 30_000, || {
        let s = shared();
        let a = TestHeap::new(s);
        let b = TestHeap::new(s);
        let c = class_with(64);
        let mine: Vec<*mut u8> = (0..8).map(|_| b.alloc(c)).collect();
        for &p in &mine {
            b.free(p);
        }
        assert!(b.len(c) >= 8);
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        // `a` began the epoch; `b`'s caches still hold slots below the
        // floor until its next slow path, here an allocation of a class it
        // has no cache for.
        let _ = a.alloc(c);
        let _ = b.alloc(class_with(128));
        assert_eq!(b.len(c), 0, "b's slow path flushed its caches");
        for _ in 0..64 {
            let p = b.alloc(c);
            assert!(
                !mine.contains(&p),
                "{p:?}, cached before the epoch, handed out during it"
            );
        }
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
    });
}

#[test]
fn epochs_nest_and_cow_epoch_false_refuses_them() {
    in_fork_child("cow nesting", 30_000, || {
        let s = shared();
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        assert_eq!(crate::cow::stat(s, 1, Tracked::assume_new()), 2);
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
        assert!(crate::cow::in_force(), "the outer epoch still holds");
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
        assert!(!crate::cow::in_force());
        assert_eq!(
            crate::cow::end(s, Tracked::assume_new()),
            crate::cow::EINVAL,
            "no epoch open"
        );
        assert_eq!(
            crate::cow::stat(s, 0, Tracked::assume_new()),
            1,
            "one outermost epoch"
        );
        let off = TestShared::with(Config {
            cow_epoch: false,
            ..Config::DEFAULT
        })
        .build();
        assert_eq!(
            crate::cow::begin(off, Tracked::assume_new()),
            crate::cow::ENOTSUP
        );
    });
}

/// Begins and ends of several threads at once: every begin that returns 0
/// finds its floors and the dispatch bit in force until its end, the
/// depth never loses a count (two ends at depth 2 both storing 1), and a
/// last end never clears the state of an epoch a concurrent begin opened.
/// A begin refused with `EBUSY` opened nothing and is not ended.
#[test]
fn concurrent_begins_and_ends_keep_the_depth_and_the_floors() {
    in_fork_child("cow concurrent begin and end", 60_000, || {
        let s = shared();
        let threads = 4;
        let rounds = 2000;
        let opened = std::sync::atomic::AtomicU64::new(0);
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| {
                    for _ in 0..rounds {
                        match crate::cow::begin(s, Tracked::assume_new()) {
                            0 => {
                                opened.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                assert!(
                                    crate::cow::in_force(),
                                    "a begin returned with no floors in force"
                                );
                                assert!(
                                    s.free_flags() & FREE_EPOCH != 0,
                                    "a begin returned with the dispatch bit clear"
                                );
                                std::thread::yield_now();
                                assert!(
                                    crate::cow::in_force(),
                                    "another thread's end cleared an open epoch"
                                );
                                assert_eq!(
                                    crate::cow::end(s, Tracked::assume_new()),
                                    0,
                                    "a matched end"
                                );
                            }
                            e => assert_eq!(e, crate::cow::EBUSY),
                        }
                    }
                });
            }
        });
        assert!(opened.into_inner() > 0);
        assert_eq!(
            crate::cow::stat(s, 1, Tracked::assume_new()),
            0,
            "every count went back"
        );
        assert!(!crate::cow::open());
        assert!(!crate::cow::in_force());
        assert!(s.free_flags() & FREE_EPOCH == 0);
    });
}

/// An end whose compare-and-exchanges all lose to other threads' begins
/// and ends (`cow::leave`'s exhausted exit, forced with no tries) counts
/// nothing and says so with `EAGAIN`, and the retry closes the epoch.
#[test]
fn an_end_that_runs_out_of_tries_is_retryable() {
    in_fork_child("cow end retry", 30_000, || {
        let s = shared();
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        assert_eq!(
            crate::cow::end_within(s, 0, Tracked::assume_new()),
            crate::cow::EAGAIN
        );
        assert_eq!(
            crate::cow::stat(s, 1, Tracked::assume_new()),
            1,
            "the end did not count"
        );
        assert!(crate::cow::in_force() && s.free_flags() & FREE_EPOCH != 0);
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0, "the retry");
        assert!(!crate::cow::open() && !crate::cow::in_force());
        assert!(s.free_flags() & FREE_EPOCH == 0);
    });
}

/// A fork child starts with no epoch open, also when it was forked while
/// another thread's outermost transition was under way (`BUSY`), which
/// nothing in the child would clear (`cow::fork_child`).
#[test]
fn a_fork_child_starts_with_no_epoch_open() {
    in_fork_child("cow fork child", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let _keep: Vec<*mut u8> = (0..64).map(|_| t.alloc(c)).collect();
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        crate::cow::test_set_busy();
        assert!(crate::cow::in_force() && s.free_flags() & FREE_EPOCH != 0);
        crate::cow::fork_child(s);
        assert!(!crate::cow::open(), "the child's depth is 0, BUSY clear");
        assert!(!crate::cow::in_force(), "the parent's floors do not hold");
        assert_eq!(floor0(s, c), 0);
        assert!(
            s.free_flags() & FREE_EPOCH == 0,
            "frees take the fast path again"
        );
        assert_eq!(crate::cow::flush_after(), 0);
        // The child's own epochs open and close.
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
        assert!(!crate::cow::open());
    });
}

/// With lazy purges (`lazy_band:true` under headroom: a `MADV_FREE`
/// batch, after which `purge::purged_is_zero` is false), a `Purged` slab
/// below the floor may still hold pages the fork child shares, so a
/// refill under the floor takes none: every slot comes from at or above
/// the floor (`next_slab`'s lazy branch; `model::` covers the fallback
/// when nothing is left above the floor).
#[test]
fn lazily_purged_slabs_below_the_floor_stay_unused() {
    in_fork_child("cow lazy purges", 30_000, || {
        crate::purge::tests::forget_other_threads_explicit_purges();
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let nslots = CLASSES[c].nslots();
        // Three slabs: the first two released whole, the third kept.
        let all: Vec<*mut u8> = (0..3 * nslots).map(|_| t.alloc(c)).collect();
        let purged: Vec<usize> = all[..2 * nslots]
            .iter()
            .map(|&p| slab_of(s, c, p))
            .collect();
        for &p in &all[..2 * nslots] {
            t.free(p);
        }
        t.flush_all();
        crate::purge::configure_advice(true);
        crate::purge::set_headroom(true, false);
        assert_eq!(crate::purge::advice(), crate::purge::Advice::Lazy);
        let cs = s.arena0.class(c);
        assert!(cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()) >= 2);
        crate::purge::set_headroom(false, false);
        assert!(
            !crate::purge::purged_is_zero(),
            "the purge was a MADV_FREE batch"
        );
        assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
        let f = floor0(s, c);
        assert!(
            purged.iter().all(|&b| b < f),
            "the purged slabs are below the floor"
        );
        let new: Vec<*mut u8> = (0..2 * nslots).map(|_| t.alloc(c)).collect();
        for &p in &new {
            assert!(
                slab_of(s, c, p) >= f,
                "a refill under the floor took {p:?} of lazily purged slab {}",
                slab_of(s, c, p)
            );
        }
        assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
    });
}

/// An epoch over four half-empty slabs of class `c` (holes below the
/// floor) whose writes fill `slabs` slabs above it, ended, with every
/// cache flushed so that `live` counts only the blocks in use. Returns
/// the floor and the blocks written during the epoch.
fn drain_setup(s: &Shared, t: &TestHeap, c: usize, slabs: usize) -> (usize, Vec<*mut u8>) {
    let nslots = CLASSES[c].nslots();
    let all: Vec<*mut u8> = (0..4 * nslots).map(|_| t.alloc(c)).collect();
    for p in all.iter().step_by(2) {
        t.free(*p);
    }
    t.flush_all();
    assert_eq!(crate::cow::begin(s, Tracked::assume_new()), 0);
    let f = floor0(s, c);
    let new: Vec<*mut u8> = (0..slabs * nslots).map(|_| t.alloc(c)).collect();
    assert!(new.iter().all(|&p| slab_of(s, c, p) >= f));
    assert_eq!(crate::cow::end(s, Tracked::assume_new()), 0);
    t.flush_all();
    assert_eq!(
        s.arena0.class(c).drain_floor(),
        f,
        "the floor became the drain floor"
    );
    (f, new)
}

/// `challoc_cow_stat(CHALLOC_COW_DRAINING)`.
fn draining(s: &Shared) -> u64 {
    crate::cow::stat(s, 8, Tracked::assume_new())
}

/// A pair drains while its blocks above the floor fill more than one
/// slab and holes are open below, and stops once what is left above fits
/// in one slab (at most that slab to give back): the figure a server
/// reads to force its defragmenter on ends when the work does, not at the
/// window's end. It does not come back.
#[test]
fn a_pair_stops_draining_when_its_blocks_above_the_floor_fit_in_one_slab() {
    in_fork_child("cow drain ends with one slab", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, new) = drain_setup(s, &t, c, 3);
        assert_eq!(draining(s), 1, "three slabs above the floor, holes below");
        assert_eq!(draining(s), 1, "and again: nothing changed");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        // Free the blocks of every slab above the floor but the highest.
        let top = new.iter().map(|&p| slab_of(s, c, p)).max().unwrap();
        let mut kept = 0;
        for &p in &new {
            if slab_of(s, c, p) == top {
                kept += 1;
            } else {
                t.free(p);
            }
        }
        t.flush_all();
        assert!(kept > 0);
        assert_eq!(
            draining(s),
            0,
            "the {kept} blocks left above fit in one slab"
        );
        assert_eq!(s.arena0.class(c).drain_floor(), 0, "the pair's drain ended");
        // More blocks of the class do not bring the drain back.
        let more: Vec<*mut u8> = (0..2 * CLASSES[c].nslots()).map(|_| t.alloc(c)).collect();
        assert_eq!(draining(s), 0, "an ended drain stays ended");
        for p in more {
            t.free(p);
        }
    });
}

/// A query of a block below the floor (refused, no move): what a
/// defragmenter that passes over the heap asks without finding anything
/// to move, recorded as the queries of the drain (`cow::DRAIN_ASKS`).
fn ask_below(s: &Shared, c: usize, f: usize) {
    assert!(!crate::cow::drain_move(s.arena0.class(c), f - 1, Tracked::assume_new()).0);
}

/// A pair whose blocks above the floor fill exactly two slabs still
/// drains (`DRAIN_MIN_SLABS` is two: moving them gives back a slab).
#[test]
fn a_pair_with_blocks_in_two_slabs_above_the_floor_drains() {
    in_fork_child("cow drain two slabs", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, new) = drain_setup(s, &t, c, 2);
        let slabs: std::collections::BTreeSet<usize> =
            new.iter().map(|&p| slab_of(s, c, p)).collect();
        assert_eq!(slabs.len(), 2, "the epoch's blocks fill two slabs");
        assert_eq!(draining(s), 1, "two slabs above the floor, holes below");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
    });
}

/// A pair keeps draining while a defragmenter is asking for moves, and
/// all drains end once its queries have gone on for `DRAIN_STALL_S`
/// without asking for one (the defragmenter found nothing more to move),
/// well before the window closes.
#[test]
fn the_drain_ends_when_no_move_is_asked_for_in_a_stall() {
    in_fork_child("cow drain stall", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, new) = drain_setup(s, &t, c, 3);
        let cs = s.arena0.class(c);
        assert_eq!(draining(s), 1);
        // A query asks for a move: progress, so a stall's age is no stall.
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        let slab = slab_of(s, c, new[0]);
        assert!(
            crate::cow::drain_move(cs, slab, Tracked::assume_new()).0,
            "a block above the floor moves"
        );
        assert_eq!(draining(s), 1, "a move was asked for: still draining");
        assert_eq!(cs.drain_floor(), f);
        // Queries over half a stall, none for a move: still draining.
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S / 2, 0);
        assert_eq!(draining(s), 1, "half a stall of queries without a move");
        // Queries over a stall without a move end it, three slabs above
        // or not, and close the window.
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S / 2, 0);
        assert_eq!(
            draining(s),
            0,
            "a stall of queries without a move ends the drain"
        );
        assert_eq!(cs.drain_floor(), 0);
        assert!(!crate::cow::draining(), "and closes the window");
        assert!(
            !crate::cow::drain_move(cs, slab, Tracked::assume_new()).0,
            "and the queries stop asking for moves"
        );
        assert_eq!(draining(s), 0);
    });
}

/// The stall clock starts at the first read of figure 8, not at the
/// epoch's end: a program that turns its defragmenter on (and so first
/// reads the figure) long after the save still gets its drain, and the
/// stall then counts from that read.
#[test]
fn the_stall_clock_starts_at_the_first_read() {
    in_fork_child("cow drain first read", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, _new) = drain_setup(s, &t, c, 3);
        // However long before the first read: no progress to age yet, and
        // queries before it do not count.
        ask_below(s, c, f);
        crate::cow::test_age_drain_asks(10 * crate::cow::DRAIN_STALL_S, 0);
        crate::cow::test_age_drain_progress(10 * crate::cow::DRAIN_STALL_S);
        assert_eq!(draining(s), 1, "the first read, late, finds the drain");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        assert_eq!(
            draining(s),
            0,
            "a stall of queries after the first read ends it"
        );
    });
}

/// Reads of figure 8 alone end no drain: a reader that runs no
/// defragmenter (`challoc_cow_stats`, polled a stall apart), and a
/// defragmenter held up between two reads (its queries stopped a stall
/// before the read) or between two queries (one before the hold and one
/// after it), have made no pass over the heap. Queries across a whole
/// stall after the hold end the drain.
#[test]
fn a_reader_without_queries_ends_no_drain() {
    in_fork_child("cow drain reads only", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, _new) = drain_setup(s, &t, c, 3);
        assert_eq!(draining(s), 1);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        assert_eq!(draining(s), 1, "two reads a stall apart, no query between");
        crate::cow::test_age_drain_progress(10 * crate::cow::DRAIN_STALL_S);
        assert_eq!(draining(s), 1, "nor ten stalls apart");
        // Queries that stopped a stall ago: the defragmenter was held.
        ask_below(s, c, f);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, crate::cow::DRAIN_STALL_S);
        assert_eq!(draining(s), 1, "queries a stall ago, none since");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        // The held defragmenter resumes: its first query starts the span
        // again, so the hold does not count as looking.
        ask_below(s, c, f);
        assert_eq!(draining(s), 1, "a query before the hold and one after it");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        // Queries across the whole stall after that end it.
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        assert_eq!(draining(s), 0, "a stall of queries after the hold");
        assert_eq!(s.arena0.class(c).drain_floor(), 0);
    });
}

/// A stall judged before a whole begin and end in another thread, and
/// acted on after it, ends nothing: the end counted itself
/// (`DRAIN_ENDS`), and the drains are the new epoch's. That holds too
/// when a reader held across the end stores the progress mark the stall
/// was judged by (`DRAIN_SEEN`) again. A stall with no end between ends
/// them.
#[test]
fn a_stall_judged_before_an_epoch_end_ends_nothing_after_it() {
    in_fork_child("cow drain stall across end", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, _new) = drain_setup(s, &t, c, 3);
        assert_eq!(draining(s), 1);
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        assert_eq!(
            crate::cow::test_stall_across_end(s, true, false),
            (true, false),
            "an end between the verdict and the clear"
        );
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        assert_eq!(draining(s), 1, "the next read starts the stall clock again");
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        assert_eq!(
            crate::cow::test_stall_across_end(s, true, true),
            (true, false),
            "an end between, and the judged mark stored again after it"
        );
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        assert_eq!(draining(s), 1, "the end's count, not the mark, decides");
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        assert_eq!(
            crate::cow::test_stall_across_end(s, false, false),
            (true, true),
            "no end between"
        );
        assert_eq!(s.arena0.class(c).drain_floor(), 0);
        assert_eq!(draining(s), 0);
    });
}

/// A stall ends no drain while an epoch is open or an outermost begin or
/// end is under way (`BUSY`): an end may be setting the next epoch's
/// drain floors, which a stall of the last epoch's drain must not clear.
/// The first read after the transition ends them.
#[test]
fn a_stall_waits_out_an_epoch_transition() {
    in_fork_child("cow drain stall busy", 30_000, || {
        let s = shared();
        let t = TestHeap::new(s);
        let c = class_with(64);
        let (f, _new) = drain_setup(s, &t, c, 3);
        assert_eq!(draining(s), 1);
        ask_below(s, c, f);
        crate::cow::test_age_drain_progress(crate::cow::DRAIN_STALL_S);
        crate::cow::test_age_drain_asks(crate::cow::DRAIN_STALL_S, 0);
        crate::cow::test_set_busy();
        assert_eq!(draining(s), 1, "a stall during a transition ends nothing");
        assert_eq!(s.arena0.class(c).drain_floor(), f);
        // The transition is over (the fork child's reset clears BUSY).
        crate::cow::fork_child(s);
        assert_eq!(draining(s), 0, "the stall ends the drain after it");
        assert_eq!(s.arena0.class(c).drain_floor(), 0);
    });
}
