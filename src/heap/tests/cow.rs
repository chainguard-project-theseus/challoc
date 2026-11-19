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
