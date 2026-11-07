//! Configuration keys that reach the heap: `huge_retain_max`,
//! `huge_dirty_decay_ms`, the cache caps, `narenas` and the cgroup
//! policy's periods.

use super::*;

/// `huge_retain_max` from `malloc_conf` reaches the huge allocator.
#[test]
fn huge_retain_max_key_reaches_the_huge_allocator() {
    assert_eq!(shared().huge.retain_max(), crate::huge::DEFAULT_RETAIN_MAX);
    let s = TestShared::with(Config {
        huge_retain_max: 3 << 20,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.huge.retain_max(), 3 << 20);
    let s = TestShared::with(Config {
        huge_retain_max: 0,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.huge.retain_max(), 0);
}

/// `huge_dirty_decay_ms` (design 7): unset, the huge tier decays on
/// arena 0's live period (the default, and `arena.0.dirty_decay_ms`
/// moves both); set, `purge::decay` walks the huge extents on the huge
/// period alone, a freed run under `huge_retain_max` going by its own
/// window while the slab period stays what `dirty_decay_ms` says; `0`
/// purges a freed run as it is freed; `-1` never purges one.
#[test]
fn huge_dirty_decay_ms_gives_the_huge_tier_its_own_period() {
    use crate::huge::RunState;
    let page_of = |s: &Shared, p: *mut u8| match s.regions.classify(p as usize) {
        Kind::Huge { page } => page,
        other => panic!("{other:?}"),
    };
    // A run's `dirty_at` after its free, from the huge allocator's gate.
    let free_at = |s: &Shared, p: *mut u8| {
        s.free_huge(page_of(s, p), Tracked::assume_new(), Tracked::assume_new());
        s.huge.oldest_empty()
    };
    let size = 300 * 1024;
    const S: u64 = 1_000_000_000;

    // Unset: arena 0's period, live.
    let s = TestShared::with(Config {
        narenas: 1,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.config.huge_dirty_decay_ms, None);
    assert_eq!(s.huge_decay_ns(), s.decay_ns());
    assert_eq!(s.huge_decay_ns(), 10 * S);
    s.arena0
        .set_decay_ms(3000, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(s.huge_decay_ns(), 3 * S, "follows arena 0");
    s.arena0
        .set_decay_ms(-1, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(s.huge_decay_ns(), crate::purge::NEVER);
    s.arena0
        .set_decay_ms(10_000, Tracked::assume_new(), Tracked::assume_new());
    // A freed run is still `Empty` two seconds in on the 10 s window.
    let p = s
        .alloc_huge(
            size,
            16,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    assert!(!p.is_null());
    let t0 = free_at(s, p);
    assert!(t0 > 0);
    assert_eq!(s.huge.stats(Tracked::assume_new()).empty, 1);
    assert!(crate::purge::decay(s, t0 + 2 * S, Tracked::assume_new()).is_some());
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (1, 0)
    );
    assert_eq!(
        crate::purge::decay(s, t0 + 10 * S, Tracked::assume_new()),
        None,
        "the window ends: purged"
    );
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (0, 1)
    );

    // Set to 2 s: the huge extent is gone at 2 s while arena 0 keeps
    // its 10 s (and moving arena 0 no longer moves the huge period).
    let s = TestShared::with(Config {
        narenas: 1,
        huge_dirty_decay_ms: Some(2000),
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.huge_decay_ns(), 2 * S);
    assert_eq!(s.decay_ns(), 10 * S);
    s.arena0
        .set_decay_ms(500, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(
        (s.huge_decay_ns(), s.decay_ns()),
        (2 * S, S / 2),
        "its own period"
    );
    s.arena0
        .set_decay_ms(10_000, Tracked::assume_new(), Tracked::assume_new());
    let p = s
        .alloc_huge(
            size,
            16,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    let t0 = free_at(s, p);
    assert_eq!(
        s.huge
            .lookup(page_of(s, p), Tracked::assume_new(), Tracked::assume_new())
            .unwrap()
            .state,
        RunState::Empty,
        "retained: under huge_retain_max"
    );
    // One step in (10 ms of a 2 s window): a walk that purges nothing.
    assert_eq!(
        crate::purge::decay(s, t0 + 2 * S / 200, Tracked::assume_new()),
        Some(t0 + 2 * 2 * S / 200)
    );
    assert_eq!(s.huge.stats(Tracked::assume_new()).empty, 1);
    assert_eq!(
        crate::purge::decay(s, t0 + 2 * S, Tracked::assume_new()),
        None,
        "the 2 s window ends: purged"
    );
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (0, 1)
    );
    assert_eq!(
        s.huge
            .lookup(page_of(s, p), Tracked::assume_new(), Tracked::assume_new())
            .unwrap()
            .state,
        RunState::Purged
    );

    // Set to 0: purged as it is freed, no `Empty` extent ever.
    let s = TestShared::with(Config {
        narenas: 1,
        huge_dirty_decay_ms: Some(0),
        ..Config::DEFAULT
    })
    .build();
    assert!(s.huge.purge_on_free());
    assert_eq!(s.huge_decay_ns(), 0);
    let p = s
        .alloc_huge(
            size,
            16,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    s.free_huge(page_of(s, p), Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(s.huge.oldest_empty(), 0);
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (0, 1)
    );
    assert_eq!(
        s.huge
            .lookup(page_of(s, p), Tracked::assume_new(), Tracked::assume_new())
            .unwrap()
            .state,
        RunState::Purged
    );
    assert_eq!(
        crate::purge::decay(s, u64::MAX / 2, Tracked::assume_new()),
        None
    );

    // Set to -1: never, while arena 0 still decays.
    let s = TestShared::with(Config {
        narenas: 1,
        huge_dirty_decay_ms: Some(-1),
        ..Config::DEFAULT
    })
    .build();
    assert!(!s.huge.purge_on_free());
    assert_eq!(s.huge_decay_ns(), crate::purge::NEVER);
    assert_eq!(s.decay_ns(), 10 * S);
    let p = s
        .alloc_huge(
            size,
            16,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    let t0 = free_at(s, p);
    assert_eq!(
        crate::purge::decay(s, t0 + 1000 * S, Tracked::assume_new()),
        None
    );
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (1, 0),
        "never purged by decay"
    );
    assert_eq!(
        crate::purge::purge_all(s, Tracked::assume_new(), Tracked::assume_new()),
        1,
        "malloc_trim still purges it"
    );
    assert_eq!(
        (
            s.huge.stats(Tracked::assume_new()).empty,
            s.huge.stats(Tracked::assume_new()).purged
        ),
        (0, 1)
    );
}

#[test]
fn default_caps_follow_the_design_table() {
    for (c, cls) in CLASSES.iter().enumerate() {
        let cap = default_cap(c);
        let expect = match cls.size() {
            0..=128 => 1024,
            129..=256 => 512,
            257..=512 => 256,
            513..=1024 => 128,
            1025..=2048 => 64,
            2049..=4096 => 32,
            4097..=8192 => 16,
            _ => 8,
        };
        assert_eq!(cap, expect, "class {c} size {}", cls.size());
        assert!((MIN_CAP..=MAX_CAP).contains(&cap));
        // A cache may hold more than one slab's worth (a 64 KiB slab of
        // 128-byte slots has 512); refills and flushes span slabs.
        let l = CacheHeader::slots_layout(cap);
        assert_eq!(l.size(), 8 * cap as usize);
        assert_eq!(l.align(), 16);
    }
    // One line per header, headers dense in the heap, the small fields
    // on the two lines before them.
    assert_eq!(core::mem::size_of::<CacheHeader>(), 64);
    assert_eq!(core::mem::align_of::<CacheHeader>(), 64);
    assert_eq!(core::mem::offset_of!(Heap, caches), 128);
    assert_eq!(
        core::mem::size_of::<Heap>(),
        128 + 64 * NCLASSES,
        "heap is {} bytes",
        core::mem::size_of::<Heap>()
    );
    assert_eq!(core::mem::size_of::<ArenaCaches>(), 64 + 64 * NCLASSES);
}

/// Design 11: the periodic tick's GC interval is the configured one
/// until the policy's immediate band, where it is 0; the effective
/// decay period follows the band for arena 0 and the huge allocator;
/// `-1` stays never. A test cannot choose its cgroup's limit, so the
/// policy is driven through `apply` here.
#[test]
fn cgroup_policy_drives_gc_interval_and_decay_period() {
    use crate::policy::{Band, KEEP_ALL, band, cgroup_band, effective_decay_ns};
    let s = shared();
    // At init the outcome is the discovery sample's: no limit on the
    // host and under Docker's default profile, a band under
    // `--memory=512m`.
    let at_init = cgroup_band(s.cgroup_now(Tracked::assume_new()));
    assert_eq!(s.policy.band(), at_init);
    assert_eq!(
        s.gc_interval(),
        if s.policy.immediate() {
            0
        } else {
            s.gc_interval_ns
        }
    );
    assert_eq!(
        s.policy.effective_decay_ns(s.decay_ns()),
        effective_decay_ns(s.decay_ns(), at_init)
    );
    assert_eq!(crate::policy::empty_bytes(s, Tracked::assume_new()), 0);
    let l = 512u64 << 20;
    s.policy.apply(band(l, l * 95 / 100), l, 0, 0);
    assert_eq!(s.gc_interval(), 0);
    assert_eq!(s.policy.effective_decay_ns(s.decay_ns()), 0);
    assert_eq!(
        s.policy.effective_decay_ns(crate::purge::NEVER),
        crate::purge::NEVER
    );
    s.policy.apply(band(l, l * 8 / 10), l, 0, 0);
    assert_eq!(s.gc_interval(), s.gc_interval_ns);
    let eff = s.policy.effective_decay_ns(s.decay_ns());
    assert!(eff < s.decay_ns() && eff > 1_000_000_000, "{eff}");
    // A decay pass with a pending full purge consumes it.
    s.policy.apply(band(l, l * 8 / 10), l, 0, 1);
    assert!(s.policy.full_purge_pending());
    let _ = crate::purge::decay(s, now_ns(), Tracked::assume_new());
    assert!(!s.policy.full_purge_pending());
    assert_eq!(s.policy.full_purges(), 1);
    s.policy
        .apply(Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, 1);
    assert_eq!(s.policy.keep16(), KEEP_ALL);
    assert_eq!(s.gc_interval(), s.gc_interval_ns);
}

/// `narenas:0` (the default) is resolved at init from the CPU count
/// and stored, so `opt.narenas` and `arenas.auto` agree on it.
#[test]
fn default_narenas_is_resolved_at_init() {
    let s = TestShared::with(Config::DEFAULT).build();
    let expect = Config::DEFAULT.effective_narenas(ncpus());
    assert!((4..=crate::config::MAX_AUTO_ARENAS).contains(&expect));
    assert_eq!(s.config.narenas, expect);
    assert_eq!(s.arenas.auto(), expect);
    assert_eq!(s.arenas.count(), expect);
    let s1 = TestShared::with(Config {
        narenas: 1,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!((s1.config.narenas, s1.arenas.auto()), (1, 1));
}
