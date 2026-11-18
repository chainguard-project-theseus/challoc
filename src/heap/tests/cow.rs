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
