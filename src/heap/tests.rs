use super::*;
use super::flush::flag_sort;
use crate::class::ClassStats;
use crate::layout::{Kind, LG_MAX_SLAB, MAX_SLAB, Mapping, Regions, class_of};
use crate::slab::State;
use std::boxed::Box;
use std::collections::HashSet;
use std::mem::MaybeUninit;
use std::vec::Vec;

/// The page size the synthetic layout is built with: the kernel's on
/// Linux (the reservations are real), 4 KiB on the host.
#[cfg(target_os = "linux")]
pub(crate) fn page_size() -> usize {
    crate::sys::features::page_size()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn page_size() -> usize {
    4096
}

/// Default capacity of a cache for a class up to 128 bytes, and the
/// refill/flush batch that goes with it.
const CAP: usize = MAX_CAP as usize;
const HALF: usize = CAP / 2;
const LG_REGION: u32 = LG_MAX_SLAB;
const LG_STRIDE: u32 = LG_REGION + 2;
const META_LEN: usize = 16 << 20;

fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

/// 64 MiB huge region; on Linux `shared()` reserves it for real.
const HUGE_LEN: usize = 64 << 20;

fn regions_at(superblock: usize, meta: (usize, usize), huge_base: usize) -> Regions {
    let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
    let huge = (huge_base, HUGE_LEN);
    Regions::from_parts(
        superblock,
        LG_REGION,
        LG_STRIDE,
        &[0usize; NCLASSES],
        huge,
        meta,
        page_size().trailing_zeros(),
        [
            Mapping { addr: superblock, len: sb_len },
            Mapping { addr: huge.0, len: huge.1 },
            Mapping { addr: meta.0, len: meta.1 },
        ],
    )
}

/// A `Shared` over a synthetic 4 MiB-per-class layout. Leaked on
/// purpose: on Linux the test thread's own heap is torn down by the
/// key destructor after the test body has returned. On Linux the
/// regions are real `PROT_NONE` reservations so slot memory can be
/// touched; elsewhere the metadata is a `Vec` and slot addresses are
/// never dereferenced.
/// One automatic arena, so every heap of a test shares arena 0 (the
/// default would spread concurrently live heaps over `4 * ncpus`).
/// `pac_pointers` on: the signed path runs in every heap test on a
/// host with pointer authentication, and is the plain path elsewhere.
pub(crate) fn shared() -> &'static Shared {
    shared_with(Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT })
}

pub(crate) fn shared_with(config: Config) -> &'static Shared {
    shared_with_guards(config, false)
}

/// `huge_retain_max` from `malloc_conf` reaches the huge allocator.
#[test]
fn huge_retain_max_key_reaches_the_huge_allocator() {
    assert_eq!(shared().huge.retain_max(), crate::huge::DEFAULT_RETAIN_MAX);
    let s = shared_with(Config { huge_retain_max: 3 << 20, ..Config::DEFAULT });
    assert_eq!(s.huge.retain_max(), 3 << 20);
    let s = shared_with(Config { huge_retain_max: 0, ..Config::DEFAULT });
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
        s.free_huge(page_of(s, p));
        s.huge.oldest_empty()
    };
    let size = 300 * 1024;
    const S: u64 = 1_000_000_000;

    // Unset: arena 0's period, live.
    let s = shared_with(Config { narenas: 1, ..Config::DEFAULT });
    assert_eq!(s.config.huge_dirty_decay_ms, None);
    assert_eq!(s.huge_decay_ns(), s.decay_ns());
    assert_eq!(s.huge_decay_ns(), 10 * S);
    s.arena0.set_decay_ms(3000);
    assert_eq!(s.huge_decay_ns(), 3 * S, "follows arena 0");
    s.arena0.set_decay_ms(-1);
    assert_eq!(s.huge_decay_ns(), crate::purge::NEVER);
    s.arena0.set_decay_ms(10_000);
    // A freed run is still `Empty` two seconds in on the 10 s window.
    let p = s.alloc_huge(size, 16, 0).0;
    assert!(!p.is_null());
    let t0 = free_at(s, p);
    assert!(t0 > 0);
    assert_eq!(s.huge.stats().empty, 1);
    assert!(crate::purge::decay(s, t0 + 2 * S).is_some());
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (1, 0));
    assert_eq!(crate::purge::decay(s, t0 + 10 * S), None, "the window ends: purged");
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (0, 1));

    // Set to 2 s: the huge extent is gone at 2 s while arena 0 keeps
    // its 10 s (and moving arena 0 no longer moves the huge period).
    let s = shared_with(Config { narenas: 1, huge_dirty_decay_ms: Some(2000), ..Config::DEFAULT });
    assert_eq!(s.huge_decay_ns(), 2 * S);
    assert_eq!(s.decay_ns(), 10 * S);
    s.arena0.set_decay_ms(500);
    assert_eq!((s.huge_decay_ns(), s.decay_ns()), (2 * S, S / 2), "its own period");
    s.arena0.set_decay_ms(10_000);
    let p = s.alloc_huge(size, 16, 0).0;
    let t0 = free_at(s, p);
    assert_eq!(s.huge.lookup(page_of(s, p)).unwrap().state, RunState::Empty, "retained: under huge_retain_max");
    // One step in (10 ms of a 2 s window): a walk that purges nothing.
    assert_eq!(crate::purge::decay(s, t0 + 2 * S / 200), Some(t0 + 2 * 2 * S / 200));
    assert_eq!(s.huge.stats().empty, 1);
    assert_eq!(crate::purge::decay(s, t0 + 2 * S), None, "the 2 s window ends: purged");
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (0, 1));
    assert_eq!(s.huge.lookup(page_of(s, p)).unwrap().state, RunState::Purged);

    // Set to 0: purged as it is freed, no `Empty` extent ever.
    let s = shared_with(Config { narenas: 1, huge_dirty_decay_ms: Some(0), ..Config::DEFAULT });
    assert!(s.huge.purge_on_free());
    assert_eq!(s.huge_decay_ns(), 0);
    let p = s.alloc_huge(size, 16, 0).0;
    s.free_huge(page_of(s, p));
    assert_eq!(s.huge.oldest_empty(), 0);
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (0, 1));
    assert_eq!(s.huge.lookup(page_of(s, p)).unwrap().state, RunState::Purged);
    assert_eq!(crate::purge::decay(s, u64::MAX / 2), None);

    // Set to -1: never, while arena 0 still decays.
    let s = shared_with(Config { narenas: 1, huge_dirty_decay_ms: Some(-1), ..Config::DEFAULT });
    assert!(!s.huge.purge_on_free());
    assert_eq!(s.huge_decay_ns(), crate::purge::NEVER);
    assert_eq!(s.decay_ns(), 10 * S);
    let p = s.alloc_huge(size, 16, 0).0;
    let t0 = free_at(s, p);
    assert_eq!(crate::purge::decay(s, t0 + 1000 * S), None);
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (1, 0), "never purged by decay");
    assert_eq!(crate::purge::purge_all(s), 1, "malloc_trim still purges it");
    assert_eq!((s.huge.stats().empty, s.huge.stats().purged), (0, 1));
}

/// `shared()` with guard slabs at the default interval (`guard_install`
/// as on a kernel with guard regions; a real reservation is needed, so
/// Linux only).
#[cfg(target_os = "linux")]
pub(crate) fn shared_guarded() -> &'static Shared {
    shared_with_guards(Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT }, true)
}

fn shared_with_guards(config: Config, guard_install: bool) -> &'static Shared {
    shared_with_guards_mode(config, guard_install, false)
}

/// A `Shared` with the per-CPU mode allowed (on where the host's rseq
/// has `mm_cid`; the tests check `percpu.on()` and skip otherwise).
#[cfg(target_os = "linux")]
pub(crate) fn shared_percpu() -> &'static Shared {
    shared_with_guards_mode(
        Config { narenas: 4, percpu_arena: crate::config::PercpuArena::Percpu, ..Config::DEFAULT },
        true,
        true,
    )
}

/// [`shared_build`] with the PAC probe's real answer.
pub(crate) fn shared_with_guards_mode(config: Config, guard_install: bool, percpu: bool) -> &'static Shared {
    shared_build(config, guard_install, percpu, pac::available())
}

/// The builder every `Shared` of the heap tests goes through. `pac`
/// stands in for the PAC probe: `false` on a host that has the
/// extension builds the fallback (plain pointers) the Altra and every
/// other Neoverse N1 take.
pub(crate) fn shared_build(config: Config, guard_install: bool, percpu: bool, pac: bool) -> &'static Shared {
    #[cfg(target_os = "linux")]
    let (sb, mb, hb) = {
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let reserve = |len: usize| -> usize {
            unsafe {
                crate::sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0)
            }
            .expect("reserve")
        };
        (reserve((NCLASSES << LG_STRIDE) + MAX_SLAB), reserve(META_LEN), reserve(HUGE_LEN))
    };
    #[cfg(not(target_os = "linux"))]
    let (sb, mb, hb) = {
        let mut v: Vec<u8> = Vec::new();
        v.resize(META_LEN + page_size(), 0);
        let buf: &'static mut [u8] = Box::leak(v.into_boxed_slice());
        (0x1000_0000_0000usize, buf.as_ptr() as usize, 0x2000_0000_1000usize)
    };
    let regions = regions_at(
        align_up(sb, MAX_SLAB),
        (align_up(mb, page_size()), META_LEN),
        hb,
    );
    let b: &'static mut MaybeUninit<Shared> = Box::leak(Box::new(MaybeUninit::uninit()));
    assert!(unsafe { Shared::init(b.as_mut_ptr(), regions, config, guard_install, pac, percpu) });
    unsafe { b.assume_init_ref() }
}

/// The message of a caught panic (`catch_unwind`'s payload is a
/// `String` from `panic!` with arguments, a `&str` without).
pub(crate) fn panic_message(e: Box<dyn std::any::Any + Send>) -> std::string::String {
    e.downcast_ref::<std::string::String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|m| std::string::String::from(*m)))
        .expect("a message")
}

pub(crate) fn class_with(size: usize) -> usize {
    CLASSES.iter().position(|c| c.size() == size).unwrap()
}

pub(crate) struct TestHeap {
    h: *mut Heap,
    s: &'static Shared,
}

impl TestHeap {
    pub(crate) fn new(s: &'static Shared) -> TestHeap {
        let h = Heap::create(s);
        assert!(!h.is_null());
        TestHeap { h, s }
    }

    pub(crate) fn heap(&self) -> &mut Heap {
        unsafe { &mut *self.h }
    }

    pub(crate) fn alloc(&self, c: usize) -> *mut u8 {
        let p = self.heap().alloc(self.s, c);
        assert!(!p.is_null());
        p
    }

    pub(crate) fn alloc_zeroed(&self, c: usize) -> (*mut u8, bool) {
        let (p, z) = self.heap().alloc_zeroed(self.s, c);
        assert!(!p.is_null());
        (p, z)
    }

    pub(crate) fn free(&self, p: *mut u8) {
        self.heap().free(self.s, p)
    }

    pub(crate) fn alloc_huge(&self, size: usize, align: usize) -> (*mut u8, bool) {
        self.heap().alloc_huge(self.s, size, align)
    }

    pub(crate) fn stats(&self) -> ThreadStats {
        self.heap().stats()
    }

    fn gc(&self) {
        self.heap().gc(self.s)
    }

    pub(crate) fn flush_all(&self) {
        self.heap().flush_all(self.s)
    }

    fn ticks(&self) -> u32 {
        self.heap().slow_ticks()
    }

    fn cache(&self, c: usize) -> &CacheHeader {
        self.heap().cache(c).expect("cache exists")
    }

    /// The entries of `c`'s cache as plain pointers (authenticated
    /// where the stack is signed), bottom first.
    fn cached_plain(&self, c: usize) -> Vec<*mut u8> {
        let k = self.cache(c);
        k.cached().iter().map(|&e| k.plain(e, self.s.pac())).collect()
    }

    /// The run of `c`'s cache as plain pointers.
    fn run_plain(&self, c: usize) -> Vec<*mut u8> {
        let k = self.cache(c);
        k.run().iter().map(|&e| k.plain(e, self.s.pac())).collect()
    }

    fn len(&self, c: usize) -> usize {
        self.cache(c).len()
    }

    pub(crate) fn destroy(self) {
        unsafe { Heap::destroy(self.h, self.s) }
    }
}

/// Sum of `live` over every committed slab of a class.
pub(crate) fn live(s: &Shared, c: usize) -> usize {
    let cs = &s.arena0.classes[c];
    (0..cs.committed_slabs()).filter(|&i| !cs.is_guard_slab(i)).map(|i| cs.desc(i).live()).sum()
}

pub(crate) fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
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
        assert!(cap >= MIN_CAP && cap <= MAX_CAP);
        // A cache may hold more than one slab's worth (a 64 KiB slab of
        // 128-byte slots has 512); refills and flushes span slabs.
        let l = CacheHeader::slots_layout(cap);
        assert_eq!(l.size(), 8 * cap as usize);
        assert_eq!(l.align(), 16);
    }
    // One line per header, headers dense in the heap, the small fields
    // on the two lines before them (review P3).
    assert_eq!(core::mem::size_of::<CacheHeader>(), 64);
    assert_eq!(core::mem::align_of::<CacheHeader>(), 64);
    assert_eq!(core::mem::offset_of!(Heap, caches), 128);
    assert_eq!(core::mem::size_of::<Heap>(), 128 + 64 * NCLASSES, "heap is {} bytes", core::mem::size_of::<Heap>());
    assert_eq!(core::mem::size_of::<ArenaCaches>(), 64 + 64 * NCLASSES);
}

#[test]
fn heap_starts_with_the_sentinel_and_allocates_caches_lazily() {
    let s = shared();
    let before = s.meta.stats().allocated;
    let t = TestHeap::new(s);
    for c in 0..NCLASSES {
        assert!(t.heap().cache(c).is_none());
    }
    assert_eq!(t.heap().stats(), ThreadStats::default());
    assert_eq!(t.heap().arena(), 0);
    let c = class_with(64);
    let p = t.alloc(c);
    let cache = t.cache(c);
    assert_eq!(cache.cap(), CAP);
    // A refill claims half the capacity and the pop takes the lowest.
    assert_eq!(cache.len(), HALF - 1);
    assert_eq!(p as usize, s.regions.slab_addr(c, 0));
    assert!(t.heap().cache(c + 1).is_none());
    assert_eq!(t.heap().stats().allocated, 64);
    assert_eq!(t.heap().slow_ticks(), 1);
    // Only the heap and one stack came from the metadata region (both
    // rounded up to a metadata bin).
    let meta_used = s.meta.stats().allocated - before;
    assert!(meta_used >= core::mem::size_of::<Heap>() + 8 * CAP);
    assert!(meta_used <= 4096 + 8 * CAP, "{meta_used}");
    t.free(p);
    t.destroy();
    assert_eq!(s.meta.stats().allocated, before);
    assert_eq!(live(s, c), 0);
}

#[test]
fn allocations_are_address_ordered_and_lifo_after_free() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(16);
    let size = 16;
    let base = s.regions.slab_addr(c, 0);
    let n = 2 * HALF + 44;
    let mut ps = Vec::new();
    for i in 0..n {
        let p = t.alloc(c);
        assert_eq!(p as usize, base + i * size, "allocation {i}");
        ps.push(p);
    }
    // Three refills of HALF (the third partially consumed).
    assert_eq!(t.heap().slow_ticks(), 3);
    assert_eq!(t.cache(c).len(), 3 * HALF - n);
    assert_eq!(live(s, c), 3 * HALF);
    // Frees push, the most recent free is reused first.
    t.free(ps[10]);
    t.free(ps[20]);
    assert_eq!(t.alloc(c), ps[20]);
    assert_eq!(t.alloc(c), ps[10]);
    assert_eq!(t.heap().stats(), ThreadStats { allocated: (n as u64 + 2) * 16, deallocated: 2 * 16 });
    t.destroy();
    assert_eq!(live(s, c), n);
}

#[test]
fn full_cache_flushes_the_highest_half_sorted_and_slabs_empty() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let cs = &s.arena0.classes[c];
    let n = 3000;
    let ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
    assert_eq!(live(s, c), align_up(n, HALF));
    // Free in a scrambled order so flushes see unsorted batches.
    let mut order: Vec<usize> = (0..n).collect();
    let mut st = 0x1234_5678u64;
    for i in (1..n).rev() {
        let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    let mut flushes = 0;
    for (k, &i) in order.iter().enumerate() {
        let before = t.cache(c).len();
        t.free(ps[i]);
        let after = t.cache(c).len();
        if after != before + 1 {
            // A flush released the highest HALF and pushed one.
            assert_eq!(before, CAP);
            assert_eq!(after, HALF + 1);
            flushes += 1;
        }
        assert!(t.cache(c).len() <= CAP, "free {k}");
    }
    assert!(flushes >= 2, "{flushes} flushes");
    // Nothing is live in the program, so `live` is exactly what is cached
    // (the never-handed-out tail of the last refill was flushed too).
    assert_eq!(live(s, c), t.cache(c).len());
    // Everything back: the cache empties, slabs go Empty.
    t.flush_all();
    assert_eq!(t.cache(c).len(), 0);
    assert_eq!(live(s, c), 0);
    for i in 0..cs.committed_slabs() {
        assert_eq!(cs.desc(i).state(), State::Empty, "slab {i}");
    }
    let st = cs.stats();
    assert_eq!(st.empty, cs.committed_slabs());
    assert_eq!(st.nonfull, 0);
    // Allocating again reuses the lowest slab, now Empty -> Active.
    let p = t.alloc(c);
    assert_eq!(p as usize, s.regions.slab_addr(c, 0));
    assert_eq!(cs.desc(0).state(), State::Active);
    t.free(p);
    t.destroy();
    assert_eq!(cs.stats(), ClassStats { committed: cs.committed_slabs(), empty: cs.committed_slabs(), ..Default::default() });
}

/// Fill the class-64 cache with a double free
/// and exactly the right number of other slots, so that after the
/// flush's sort the pair sits at `lower` and `lower + 1`; the push that
/// overflows the cache triggers the flush. Returns the flush's panic
/// message, or the message of a later flush of the whole cache when
/// the first one let the pair through.
fn cached_pair_flush_message(lower_of: impl Fn(usize) -> usize) -> std::string::String {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let cap = default_cap(c) as usize;
    let mut all: Vec<*mut u8> = (0..4 * cap).map(|_| t.alloc(c)).collect();
    t.flush_all();
    all.sort_unstable();
    let mid = 2 * cap;
    let rest = cap - cap / 2;
    let lower = lower_of(rest);
    let higher = cap - 2 - lower;
    let p = all[mid];
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        t.free(p);
        t.free(p);
        for i in 1..=lower {
            t.free(all[mid - i]);
        }
        for i in 1..=higher {
            t.free(all[mid + i]);
        }
        assert_eq!(t.cache(c).len(), cap);
        t.free(all[0]);
        // A pair kept together is released, and caught, by the next
        // flush of the whole cache.
        t.flush_all();
    }));
    let msg = panic_message(r.expect_err("the flushes let a cached double free through"));
    assert!(msg.contains(&std::format!("double free of {:#x} (class {c}", p as usize)), "{msg}");
    msg
}

/// With `double_free_scan` a second free of a
/// slot that is one of the top four cache entries aborts at the push;
/// deeper than that it is left to the flush as before.
#[test]
fn double_free_scan_compares_the_top_four_entries() {
    let s = shared_with(Config { narenas: 1, double_free_scan: true, ..Config::DEFAULT });
    assert!(s.double_free_scan());
    let t = TestHeap::new(s);
    let c = class_with(64);
    for between in 0..=4usize {
        let p: Vec<*mut u8> = (0..6).map(|_| t.alloc(c)).collect();
        let before = t.cache(c).len();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.free(p[0]);
            for &q in &p[1..=between] {
                t.free(q);
            }
            t.free(p[0]);
        }));
        if between < 4 {
            let m = panic_message(r.expect_err("the push let a double free through"));
            assert!(m.contains(&std::format!("double free of {:#x}: the slot is already in the thread cache", p[0] as usize)), "{between}: {m}");
            // The abort came before the second copy was stored.
            assert_eq!(t.cache(c).len(), before + between + 1);
            for &q in &p[between + 1..] {
                t.free(q);
            }
            t.flush_all();
        } else {
            r.expect("the fifth entry down is not compared");
            // Both copies are cached; the flush catches them.
            let m = panic_message(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.flush_all())).expect_err("flush"));
            assert!(m.contains("double free of"), "{m}");
        }
    }
}

/// The fast paths carry no call (plan Phase 2.5 task 4c's budget, the
/// sized-path lane 2026-09-21): a `double_free_scan` hit makes
/// `push_fast` decline (`false`, nothing stored) instead of aborting from
/// the inlined body, and the slow path that a decline reaches
/// (`Heap::free`) aborts by name; a due scheduled pop makes `pop_fast`
/// decline (`None`, `allocs` not counted) and `Heap::alloc` serves it.
#[test]
fn fast_paths_decline_the_scan_hit_and_the_scheduled_pop() {
    let s = shared_with(Config { narenas: 1, double_free_scan: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    assert!(flags & PUSH_SCAN != 0);
    let p = t.alloc(c);
    let q = t.alloc(c);
    t.free(p);
    let len = t.cache(c).len();
    // The fast push of `p` again: a hit among the top entries, declined.
    assert!(!t.heap().push_fast(c, p, flags), "a scan hit declines the fast push");
    assert_eq!(t.cache(c).len(), len, "nothing was stored");
    // A slot that is not cached pushes as before.
    assert!(t.heap().push_fast(c, q, flags));
    assert_eq!(t.cache(c).len(), len + 1);
    // The slow path a decline reaches aborts with the scan's message.
    let m = panic_message(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.free(p))).expect_err("the slow path let the double free through"),
    );
    assert!(m.contains(&std::format!("double free of {:#x}: the slot is already in the thread cache", p as usize)), "{m}");
    assert_eq!(t.cache(c).len(), len + 1, "the abort came before any store");

    // The scheduled pop: force the schedule due and pop through the fast
    // path alone, then through `alloc`.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    let a = t.alloc(c);
    t.free(a);
    let k = t.heap().cache(c).unwrap() as *const CacheHeader as *mut CacheHeader;
    // SAFETY: the test owns the heap; `allocs + 1` lands on the mask.
    unsafe { (*k).allocs = (*k).compact_mask as u64 };
    let allocs = unsafe { (*k).allocs };
    let len = t.cache(c).len();
    assert!(t.heap().pop_fast(c, flags).is_none(), "a due schedule declines the fast pop");
    assert_eq!((unsafe { (*k).allocs }, t.cache(c).len()), (allocs, len), "nothing counted, nothing popped");
    let b = t.alloc(c);
    assert!(!b.is_null());
    assert_eq!(unsafe { (*k).allocs }, allocs.wrapping_add(1), "the slow path counted the one pop");
    assert_eq!(t.cache(c).len(), len - 1);
    t.free(b);
}

#[test]
fn double_free_scan_off_by_default_leaves_the_push_alone() {
    let s = shared();
    assert_eq!(s.double_free_scan(), Config::DEFAULT.double_free_scan);
}

#[test]
fn flush_aborts_on_a_cached_pair_straddling_the_split() {
    let msg = cached_pair_flush_message(|rest| rest - 1);
    assert!(msg.contains("both copies cached, one at the flush boundary"), "{msg}");
}

/// A pair that the split keeps together stays cached (both copies, the
/// bit set) and is caught by the release that flushes it, not by the
/// boundary compare.
#[test]
fn a_cached_pair_kept_by_the_split_is_caught_when_released() {
    let msg = cached_pair_flush_message(|rest| rest / 2);
    assert!(msg.contains("double free of") && !msg.contains("boundary"), "{msg}");
}

/// A flush of the whole cache (`n == len`, `rest_len == 0`): the sorted
/// split is not taken even when the stack is in address order (there
/// is no kept half to keep sorted), the kept-half sort has nothing to
/// do, the straddling compare has no kept maximum to match, and the
/// header comes out empty with its run, zero prefix and low water
/// reset. `flush_all` is such a flush of every class.
#[test]
fn a_whole_cache_flush_keeps_nothing_and_takes_the_unsorted_branch() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    // Drain the first refill so the cache is empty, then push ten
    // slots in ascending address order: a sorted stack.
    let mut ps: Vec<*mut u8> = (0..HALF).map(|_| t.alloc(c)).collect();
    assert_eq!(t.len(c), 0);
    ps.sort_unstable();
    for &p in &ps[..10] {
        t.free(p);
    }
    assert_eq!(t.len(c), 10);
    assert!(t.cached_plain(c).is_sorted());
    assert_eq!(t.cache(c).unsorted_flushes, 0);
    t.flush_all();
    let k = t.cache(c);
    assert_eq!((k.len(), k.run().len(), k.zeroed(), k.low_water()), (0, 0, 0, 0));
    assert_eq!(k.unsorted_flushes, 1, "a whole-cache flush never takes the sorted split");
    assert_eq!(live(s, c), HALF - 10);
    // The same with a scrambled stack: evens first, then odds.
    for (i, &p) in ps[10..].iter().enumerate() {
        if i % 2 == 0 {
            t.free(p);
        }
    }
    for (i, &p) in ps[10..].iter().enumerate() {
        if i % 2 == 1 {
            t.free(p);
        }
    }
    assert_eq!(t.len(c), HALF - 10);
    assert!(!t.cached_plain(c).is_sorted());
    t.flush_all();
    let k = t.cache(c);
    assert_eq!((k.len(), k.run().len(), k.zeroed(), k.low_water()), (0, 0, 0, 0));
    assert_eq!(k.unsorted_flushes, 2);
    assert_eq!(live(s, c), 0);
    t.destroy();
}

/// The dense [`GuardTest`] the sized fast path reads agrees with
/// [`SlabTable::is_guard_slab`] for every class over the first slabs of
/// the region and around its end, with guards on and off.
#[cfg(target_os = "linux")]
#[test]
fn guard_test_agrees_with_the_tables() {
    for s in [shared_guarded(), shared()] {
        for c in 0..NCLASSES {
            let t = s.table(c);
            let g = s.guard_test[c];
            assert_eq!(g, GuardTest::of(t));
            let n = t.nslabs();
            for slab in (0..1024.min(n)).chain(n.saturating_sub(64)..n) {
                assert_eq!(g.is_guard(slab), t.is_guard_slab(slab), "class {c} slab {slab}");
            }
        }
    }
}

/// `guard_interval_for` alone: a class whose slab (or page, when that
/// is larger) is at least the 64 KiB guard unit keeps the configured
/// interval; a smaller one multiplies it so that every class places
/// one guard per `interval * unit` of address space; 0 stays 0, a
/// power of two stays one, and the product saturates instead of
/// wrapping.
#[test]
fn guard_interval_for_places_one_guard_per_unit_of_address_space() {
    let unit = 1usize << crate::layout::LG_GUARD_UNIT;
    let small = (0..NCLASSES).find(|&c| CLASSES[c].slab_size() < unit).expect("a class with slabs below the guard unit");
    let large = (0..NCLASSES).find(|&c| CLASSES[c].slab_size() >= unit).expect("a class with slabs at or above the guard unit");
    for page in [4096usize, 16 << 10, 64 << 10] {
        let unit = unit.max(page);
        for interval in [1u32, 4, 16, 64] {
            for c in 0..NCLASSES {
                let group = CLASSES[c].slab_size().max(page);
                let r = guard_interval_for(c, page, interval);
                assert_eq!(guard_interval_for(c, page, 0), 0, "class {c} page {page}");
                assert!(r.is_power_of_two(), "class {c} page {page} interval {interval}: {r}");
                if group >= unit {
                    assert_eq!(r, interval, "class {c} page {page}");
                } else {
                    assert_eq!(r as usize * group, interval as usize * unit, "class {c} page {page}");
                }
            }
        }
    }
    assert_eq!(guard_interval_for(small, 4096, 4) as usize, 4 * unit / CLASSES[small].slab_size().max(4096));
    assert!(guard_interval_for(small, 4096, 4) > 4, "the scaling branch was taken");
    assert_eq!(guard_interval_for(large, 4096, 4), 4);
    assert_eq!(guard_interval_for(small, 64 << 10, 4), 4, "a 64 KiB page makes every group the unit");
    assert_eq!(guard_interval_for(small, 4096, u32::MAX), u32::MAX, "saturates");
}

/// The sized fast path's check rejects a slot
/// start inside a guard slab (arithmetic on a dense per-class pair), so
/// the exact slow path aborts instead of the guard memory being cached.
/// It does not read the watermark: a slot start in a slab past it
/// passes here and aborts at the flush (the next test).
#[cfg(target_os = "linux")]
#[test]
fn sized_slot_rejects_guard_slabs() {
    let s = shared_guarded();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let cs = s.arena0.class(c);
    let n = CLASSES[c].nslots();
    let interval = guard_interval_for(c, s.regions.page_size(), Config::DEFAULT.guard_slab_interval) as usize;
    let all: Vec<*mut u8> = (0..(interval + 2) * n).map(|_| t.alloc(c)).collect();
    let committed = cs.committed_slabs();
    let guard = (0..committed).find(|&i| cs.is_guard_slab(i)).expect("a guard slab was passed");
    let body = s.regions.slab_addr(c, guard) + s.regions.page_size().min(CLASSES[c].slab_size() / 2);
    assert_eq!(s.sized_slot(all[0], Some(c)), Some(c));
    assert_eq!(s.sized_slot(body as *mut u8, Some(c)), None, "guard slab {guard}");
    assert_eq!(s.sized_slot(s.regions.slab_addr(c, committed) as *mut u8, Some(c)), Some(c), "deferred to the flush");
    assert_eq!(s.sized_slot(all[0], Some(c + 1)), None, "wrong hint");
    assert_eq!(s.sized_slot(all[0], None), None, "huge hint");
    assert_eq!(s.sized_slot((all[0] as usize + 8) as *mut u8, Some(c)), None, "mid-slot");
    assert_eq!(s.sized_slot((s.regions.class_base[c] - 64) as *mut u8, Some(c)), None, "below the region");
    assert_eq!(s.sized_slot((s.regions.class_base[c] + s.regions.region_len()) as *mut u8, Some(c)), None, "past the region");
    for p in all {
        t.free(p);
    }
    t.destroy();
}

/// The watermark check the sized fast path does not make is the
/// flush's: a sized free of a slot start in a slab no
/// arena has been handed is cached and aborts by name when the cache
/// flushes it.
#[cfg(target_os = "linux")]
#[test]
#[should_panic(expected = "was never allocated")]
fn sized_free_past_the_watermark_aborts_at_the_flush() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    let committed = s.arena0.class(c).committed_slabs();
    let beyond = s.regions.slab_addr(c, committed + 1) as *mut u8;
    assert!(free_sized_fast(s, t.heap(), beyond, Some(c), s.push_flags()), "the fast path took it");
    t.free(p);
    t.flush_all();
}

#[test]
fn flush_releases_the_highest_addresses_and_keeps_the_lowest() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    // Slots spread over several slabs, freed in a scrambled order so
    // the cache holds an unsorted mix of low and high addresses.
    let n = 4 * CAP;
    let ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
    let mut order: Vec<usize> = (0..n).collect();
    let mut st = 0x9e37_79b9u64;
    for i in (1..n).rev() {
        let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    let mut cached: Vec<*mut u8> = order[..CAP].iter().map(|&i| ps[i]).collect();
    for &q in &cached {
        t.free(q);
    }
    assert_eq!(t.len(c), CAP);
    let live_before = live(s, c);
    // Overflow by one: the HALF highest addresses go back to their
    // slabs, the HALF lowest stay (in whatever order the selection
    // left them; the run is dropped, design 6.4) under the new push.
    let extra = ps[order[CAP]];
    t.free(extra);
    assert_eq!(t.len(c), HALF + 1);
    assert_eq!(live(s, c), live_before - HALF);
    cached.sort_unstable();
    assert_eq!(t.alloc(c), extra);
    let mut kept: Vec<*mut u8> = (0..HALF).map(|_| t.alloc(c)).collect();
    kept.sort_unstable();
    assert_eq!(kept, cached[..HALF].to_vec(), "the kept half is the lowest addresses, content intact");
    assert_eq!(t.len(c), 0);
    // The released half is free in the slabs: allocating it again
    // is the only way to get those addresses back, and a refill takes
    // the lowest partial slab first, which is where they now are
    // (everything below them is live in the program).
    let got: Vec<*mut u8> = (0..HALF).map(|_| t.alloc(c)).collect();
    let mut got_sorted = got.clone();
    got_sorted.sort_unstable();
    assert_eq!(got_sorted, cached[HALF..].to_vec());
    t.destroy();
}

/// The flush groups its released half by slab
/// with a counting pass instead of sorting. Fake slot addresses (never
/// dereferenced) from five slabs inside a 256-slab window, scrambled,
/// come out slab-contiguous in ascending slab order as a permutation
/// of the input, and the return value is the lowest address.
#[test]
fn group_by_slab_groups_a_scrambled_batch() {
    let s = shared();
    let c = class_with(64);
    let cls = &CLASSES[c];
    let slabs = [3usize, 5, 7, 9, 200];
    let mut v: Vec<*mut u8> = Vec::new();
    for (i, &sl) in slabs.iter().enumerate() {
        for k in 0..(5 + 7 * i) {
            v.push((s.regions.slab_addr(c, sl) + ((k * 37) % cls.nslots()) * cls.size()) as *mut u8);
        }
    }
    let mut st = 0x1234_5678_9abc_def1u64;
    for i in (1..v.len()).rev() {
        let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    let mut expect = v.clone();
    expect.sort_unstable();
    let low = group_by_slab(s, c, &mut v);
    assert_eq!(low, expect[0]);
    let slab_of = |p: *mut u8| (p as usize - s.regions.class_base[c]) >> cls.lg_slab;
    let mut seen: Vec<usize> = Vec::new();
    for &p in &v {
        let sl = slab_of(p);
        if seen.last() != Some(&sl) {
            assert!(!seen.contains(&sl), "slab {sl} appears in two groups");
            seen.push(sl);
        }
    }
    assert_eq!(seen, slabs.to_vec(), "one contiguous group per slab, ascending");
    let mut got = v.clone();
    got.sort_unstable();
    assert_eq!(got, expect, "a permutation of the input");
}

/// A batch from one slab needs no grouping and is left as it is; a
/// batch spanning more than 256 slabs takes the sort fallback and comes
/// out fully sorted. Both return the lowest address.
#[test]
fn group_by_slab_single_slab_and_wide_span() {
    let s = shared();
    let c = class_with(64);
    let cls = &CLASSES[c];
    let at = |sl: usize, k: usize| (s.regions.slab_addr(c, sl) + k * cls.size()) as *mut u8;
    let mut one = std::vec![at(4, 9), at(4, 2), at(4, 700), at(4, 0), at(4, 33)];
    let before = one.clone();
    assert_eq!(group_by_slab(s, c, &mut one), at(4, 0));
    assert_eq!(one, before, "one slab: untouched");
    let mut wide = std::vec![at(1000, 3), at(0, 8), at(300, 1), at(0, 2), at(1000, 1), at(300, 5)];
    let mut expect = wide.clone();
    expect.sort_unstable();
    assert_eq!(group_by_slab(s, c, &mut wide), at(0, 2));
    assert_eq!(wide, expect, "wider than GROUP_BUCKETS slabs: sorted");
}

/// The in-place counting sort behind the bucket pass: 1000 entries
/// with random 8-bit keys come out in non-decreasing key order as a
/// permutation of the input.
#[test]
fn flag_sort_permutes_into_ascending_buckets() {
    let mut st = 0xdead_beef_cafe_f00du64;
    let mut v: Vec<*mut u8> = (0..1000).map(|_| (xorshift(&mut st) % 1_000_000) as *mut u8).collect();
    let mut expect = v.clone();
    let key = |p: *mut u8| (p as usize) & 0xff;
    flag_sort(&mut v, 256, key);
    assert!(v.windows(2).all(|w| key(w[0]) <= key(w[1])), "buckets ascend");
    v.sort_unstable();
    expect.sort_unstable();
    assert_eq!(v, expect);
}

#[test]
fn zeroed_prefix_tracks_fresh_memory() {
    let s = shared();
    let t = TestHeap::new(s);
    // 4096 slots per slab, so refills after the first come from the
    // same, by then Active, slab.
    let c = class_with(16);
    // First refill comes from a Fresh slab: the whole batch is zero.
    // Pops do not shrink the prefix (the popped index is above `len`).
    let (p1, z1) = t.alloc_zeroed(c);
    assert!(z1);
    assert_eq!(t.len(c), HALF - 1);
    assert_eq!(t.cache(c).zeroed(), HALF);
    let (p2, z2) = t.alloc_zeroed(c);
    assert!(z2);
    // A free pushes dirty memory on top: the prefix ends below it.
    // Popping it is not zero, the entry below it still is.
    t.free(p2);
    assert_eq!(t.cache(c).zeroed(), HALF - 2);
    let (p3, z3) = t.alloc_zeroed(c);
    assert_eq!(p3, p2);
    assert!(!z3);
    let (_, z4) = t.alloc_zeroed(c);
    assert!(z4);
    // Pushing below the prefix cuts it there.
    for _ in 0..50 {
        t.alloc(c);
    }
    t.free(p1);
    assert_eq!(t.cache(c).zeroed(), t.len(c) - 1);
    // Drain the cache: the next refill is from an Active slab, so the
    // conservative answer is "not zero" for the whole batch.
    while t.len(c) > 0 {
        t.alloc(c);
    }
    let (_, z5) = t.alloc_zeroed(c);
    assert!(!z5);
    assert_eq!(t.cache(c).zeroed(), 0);
    t.destroy();
}

#[test]
fn flush_drops_the_zeroed_prefix() {
    let s = shared();
    let t = TestHeap::new(s);
    let u = TestHeap::new(s);
    let c = class_with(32);
    // Zero prefix of HALF - 28 with 28 dirty pushes above it.
    let ps: Vec<*mut u8> = (0..28).map(|_| t.alloc(c)).collect();
    assert_eq!(t.len(c), HALF - 28);
    for &p in &ps {
        t.free(p);
    }
    assert_eq!(t.len(c), HALF);
    assert_eq!(t.cache(c).zeroed(), HALF - 28);
    // Fill the rest of the cache with pointers another heap allocated
    // (dirty by definition), then overflow it by one.
    let qs: Vec<*mut u8> = (0..=HALF).map(|_| u.alloc(c)).collect();
    for &q in &qs[..HALF] {
        t.free(q);
    }
    assert_eq!(t.len(c), CAP);
    assert_eq!(t.cache(c).zeroed(), HALF - 28);
    let live_before = live(s, c);
    t.free(qs[HALF]);
    // HALF entries went back to the slab and the rest were reordered:
    // the prefix is gone.
    assert_eq!(t.len(c), HALF + 1);
    assert_eq!(t.cache(c).zeroed(), 0);
    assert_eq!(live(s, c), live_before - HALF);
    t.destroy();
    u.destroy();
    assert_eq!(live(s, c), 0);
}

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

#[test]
fn periodic_work_runs_gc_on_the_interval() {
    // Every slow path is a tick and every 64th tick checks the clock.
    // With a zero interval that means GC on ticks 64, 128, ...; with the
    // default 10 s it means no GC in a test.
    let c = class_with(16);
    let d = class_with(48);
    for interval in [0u64, Config::DEFAULT.gc_interval_ns()] {
        let s = shared();
        // SAFETY: nothing else references `s` yet; the field is plain data.
        unsafe { (*(s as *const Shared as *mut Shared)).gc_interval_ns = interval };
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

#[test]
fn direct_paths_bypass_the_cache() {
    let s = shared();
    let c = class_with(256);
    let (p, zero) = s.alloc_direct(&s.arena0, c);
    assert!(!p.is_null() && zero);
    assert_eq!(p as usize, s.regions.slab_addr(c, 0));
    assert_eq!(live(s, c), 1);
    let (q, zero) = s.alloc_direct(&s.arena0, c);
    assert_eq!(q as usize, p as usize + 256);
    assert!(!zero);
    s.free_direct(0, c, p);
    assert_eq!(live(s, c), 1);
    s.free_direct(0, c, q);
    assert_eq!(live(s, c), 0);
    assert_eq!(s.arena0.classes[c].desc(0).state(), State::Empty);
}

#[test]
fn cross_thread_frees_land_in_the_freeing_heap() {
    let s = shared();
    let a = TestHeap::new(s);
    let b = TestHeap::new(s);
    let c = class_with(64);
    let ps: Vec<*mut u8> = (0..200).map(|_| a.alloc(c)).collect();
    for &p in &ps {
        b.free(p);
    }
    // 200 pushes fit in B's cache of CAP; A did one refill of HALF.
    assert_eq!(b.cache(c).len(), 200);
    assert_eq!(a.cache(c).len(), HALF - 200);
    assert_eq!(b.heap().stats(), ThreadStats { allocated: 0, deallocated: 200 * 64 });
    // B reuses what it freed, lowest address first after its flushes.
    let q = b.alloc(c);
    assert_eq!(q, ps[199]);
    b.free(q);
    a.destroy();
    b.destroy();
    assert_eq!(live(s, c), 0);
}

/// Fill `slabs` slabs of `c` on `t`, free a random `frac` of the blocks
/// and return the survivors: the post-delete residue of the churn
/// probe, with the holes in the slabs (the cache is flushed).
/// Slots the churn models fill: 40 slabs of the 64-slot 1 KiB class the
/// models were written for (`bench/probes/churn.c`), 640 of today's
/// 4-slot slabs.
const MODEL_SLOTS: usize = 2560;

fn residue(t: &TestHeap, c: usize, slabs: usize, frac_per_mille: u64, seed: u64) -> Vec<*mut u8> {
    let nslots = CLASSES[c].nslots();
    let mut st = seed;
    let mut keep = Vec::new();
    for p in (0..slabs * nslots).map(|_| t.alloc(c)).collect::<Vec<_>>() {
        if xorshift(&mut st) % 1000 < frac_per_mille { t.free(p) } else { keep.push(p) }
    }
    t.flush_all();
    keep
}

/// Slabs of `c` with at least one live slot.
fn occupied(s: &Shared, c: usize) -> usize {
    let cs = &s.arena0.classes[c];
    (0..cs.committed_slabs()).filter(|&i| !cs.is_guard_slab(i) && cs.desc(i).live() > 0).count()
}

/// Pop one slot of `c` and check the rule against the cache state
/// before the pop: while compacting, a scheduled pop returns the run's
/// lowest entry (its last) or, when the run was exhausted, the lowest
/// slot of the block a rebuild claimed; otherwise every pop returns the
/// top of the stack. Nothing is checked when the cache was empty (a
/// refill happened). The run is descending afterwards.
fn pop_checked(t: &TestHeap, c: usize) -> *mut u8 {
    let (before, run, allocs, compacting) = match t.heap().cache(c) {
        Some(k) => (t.cached_plain(c), t.run_plain(c), k.allocs(), k.compacting()),
        None => (Vec::new(), Vec::new(), 0, false),
    };
    let scheduled = compacting && (allocs + 1) % (1 << Config::DEFAULT.tcache_compact_shift) == 0;
    let probe = !compacting && (allocs + 1) % (PROBE_MASK as u64 + 1) == 0;
    let p = t.alloc(c);
    if !before.is_empty() && !probe {
        if scheduled {
            if let Some(&low) = run.last() {
                assert_eq!(p, low, "scheduled pop {} is not the run's lowest entry", allocs + 1);
            } else {
                // A rebuild: the block came from the lowest partial slab
                // and its lowest slot was served.
                assert!(t.run_plain(c).iter().all(|&q| q > p), "rebuild pop {} is not the block minimum", allocs + 1);
            }
        } else if t.cache(c).compacting() || !compacting {
            assert_eq!(p, *before.last().unwrap(), "pop {} is not the top", allocs + 1);
        }
    }
    check_run(t, c);
    p
}

/// The run invariant: descending, distinct, a prefix of the cache no
/// longer than the low-water mark.
fn check_run(t: &TestHeap, c: usize) {
    let k = t.cache(c);
    let run = t.run_plain(c);
    assert!(run.windows(2).all(|w| w[0] > w[1]), "run not descending: {run:?}");
    assert!(run.len() <= k.low_water() && k.low_water() <= k.len());
    assert_eq!(run, &t.cached_plain(c)[..run.len()]);
}

/// The compaction wrap: a scheduled
/// pop finds a compact-mode cache holding one entry with `low_water ==
/// len == 1`, an exhausted run and the GC due. `rebuild_run`'s tick
/// runs the GC, which flushes the whole cache (`lw - lw / 4 == lw`
/// below 4); the class is dense by then, so the rebuild drops the cache
/// back to probing and claims nothing. `pop_compact_run` must then
/// refill (or give up) rather than pop the empty cache, where `len - 1`
/// wrapped and read `slots[0xFFFF_FFFF]`. Every step but the schedule
/// (`allocs`, `slow_ticks`, `last_gc`) goes through the public
/// operations.
#[test]
fn a_scheduled_pop_whose_gc_empties_the_cache_refills_instead_of_wrapping() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(224);
    let mut live_ps = residue(&t, c, 30, 560, 0x0c0f_fee5);
    assert!(Heap::fragmented(s, 0, c));
    // Refill once, then probe until the cache switches to compacting
    // (the first probe only records the class's refill count).
    live_ps.push(t.alloc(c));
    for _ in 0..4 {
        if t.cache(c).compacting() {
            break;
        }
        let k = t.heap().cache_ptr(c);
        unsafe { (*k).allocs = (*k).compact_mask as u64 };
        live_ps.push(t.alloc(c));
    }
    assert!(t.cache(c).compacting());
    // One entry, `low_water == len == 1`, no run, still compacting.
    t.flush_all();
    assert!(t.cache(c).compacting());
    let p0 = live_ps.pop().unwrap();
    t.free(p0);
    t.gc();
    assert_eq!(t.len(c), 1);
    assert_eq!(t.cache(c).low_water(), 1);
    assert!(t.cache(c).run().is_empty());
    // Make the class dense: every other live slot goes back to its slab
    // through another heap, so `p0`'s slab is the only one that counts
    // and a single slab is never fragmented.
    let b = TestHeap::new(s);
    for &p in &live_ps {
        b.free(p);
    }
    b.destroy();
    assert!(!Heap::fragmented(s, 0, c));
    assert_eq!(live(s, c), 1);
    // The next pop is scheduled, its tick is the periodic one and the
    // GC interval has elapsed (on the host `now_ns` is a counter, so the
    // interval is subtracted rather than assumed).
    let h = t.heap();
    h.slow_ticks = PERIODIC_EVERY - 1;
    h.last_gc = os::now_ns().wrapping_sub(s.gc_interval_ns);
    let k = h.cache_ptr(c);
    unsafe { (*k).allocs = (*k).compact_mask as u64 };
    let ticks = t.ticks();
    let p = t.alloc(c);
    assert_eq!(t.ticks(), ticks + 1);
    assert!(!p.is_null());
    let (cls, arena) = s.classify_free(p).ok().expect("a slot of the class");
    assert_eq!((cls, arena), (c, 0));
    // The GC flushed `p0`, the rebuild dropped back to probing, and the
    // refill served the pop: half a cache minus the one popped.
    assert!(!t.cache(c).compacting());
    assert_eq!(t.len(c), t.cache(c).cap() / 2 - 1);
    assert_eq!(t.cache(c).low_water(), t.len(c));
    check_run(&t, c);
    t.free(p);
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn a_dense_class_is_probed_but_never_compacted() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    // A larson-like churn over a dense class: allocations and frees
    // balance, the holes stay in the cache, the class has none.
    let mut ps: Vec<*mut u8> = (0..2000).map(|_| pop_checked(&t, c)).collect();
    let ticks = t.ticks();
    let mut st = 0xabcd_ef01u64;
    for _ in 0..40_000 {
        let i = (xorshift(&mut st) % ps.len() as u64) as usize;
        t.free(ps[i]);
        ps[i] = pop_checked(&t, c);
    }
    let pops = t.cache(c).allocs();
    assert!(pops > 4 * (PROBE_MASK as u64 + 1), "{pops} pops");
    assert!(!t.cache(c).compacting());
    // No refill, no flush, no rebuild: the only slow paths were the
    // probes' heartbeats, one per 4096 pops.
    assert_eq!(t.ticks() as u64, ticks as u64 + pops / (PROBE_MASK as u64 + 1) - 2000 / (PROBE_MASK as u64 + 1));
    assert!(!Heap::fragmented(s, 0, c));
    for p in ps {
        t.free(p);
    }
    t.destroy();
}

#[test]
fn scheduled_pops_take_the_run_minimum_and_rebuild_from_the_lowest_slab() {
    let s = shared();
    let t = TestHeap::new(s);
    // 292 slots per slab, cap 512, block 128.
    let c = class_with(224);
    let cs = &s.arena0.classes[c];
    let mut live_ps = residue(&t, c, 30, 560, 0x5eed_1234);
    assert!(Heap::fragmented(s, 0, c));
    assert_eq!(t.len(c), 0);
    // The first probe switches the cache to compacting; from then on
    // every eighth pop is served from the run.
    let mut st = 0x1357_9bdfu64;
    let mut switched_at = None;
    let mut rebuilds = 0;
    for step in 0..30_000 {
        if xorshift(&mut st) & 1 == 0 && live_ps.len() > 2000 {
            let i = (xorshift(&mut st) % live_ps.len() as u64) as usize;
            t.free(live_ps.swap_remove(i));
            check_run(&t, c);
        } else {
            let run_before = t.heap().cache(c).map_or(0, |k| k.run().len());
            let ticks = t.ticks();
            live_ps.push(pop_checked(&t, c));
            if run_before == 0 && t.ticks() > ticks && t.cache(c).compacting() {
                rebuilds += 1;
                // The block is the run, claimed lowest partial slab
                // first: no slab below the highest it touched still has
                // room (the lowest partial slab left is at or above it).
                let run = t.run_plain(c);
                let top_slab = run.iter().map(|&q| cs.locate(q).0).max().unwrap();
                // (`plan` clears a filled slab's partial bit lazily, so
                // the lowest partial slab may be a full one.)
                let nslots = CLASSES[c].nslots();
                assert!(
                    cs.lowest_partial().is_none_or(|l| l >= top_slab || cs.desc(l).live() == nslots),
                    "block from slab {top_slab} above the lowest partial slab {:?}",
                    cs.lowest_partial()
                );
            }
        }
        if switched_at.is_none() && t.cache(c).compacting() {
            switched_at = Some(step);
        }
        if step % 1009 == 0 {
            assert_eq!(live(s, c), live_ps.len() + t.len(c), "step {step}");
        }
    }
    assert!(switched_at.is_some(), "never switched to compacting");
    assert!(rebuilds > 0, "no rebuild observed");
    assert_eq!(live(s, c), live_ps.len() + t.len(c));
    for p in live_ps {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn churn_model_empties_the_high_slabs_and_then_stops_compacting() {
    // The model of `bench/probes/churn.c`: fill `MODEL_SLOTS` slots of
    // the 1 KiB class (cap 128), free a random 56%, then run a
    // balanced free-one/allocate-one churn. Without scheduled pops
    // nothing moves (every allocation gets back the slot just freed);
    // with them the live set migrates into the lowest slabs, the high
    // slabs empty, and once the class is dense the cache goes back to
    // probing.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
    let before = occupied(s, c);
    // A random 56% deletion empties a 64-slot slab never and a 4-slot
    // slab about one time in ten.
    assert!(before * 20 >= 17 * (MODEL_SLOTS / nslots), "{before} occupied slabs after the delete");
    assert!(Heap::fragmented(s, 0, c));
    let ideal = keep.len().div_ceil(nslots);
    let mut st = 0x2468_ace0u64;
    let mut was_compacting = false;
    // With shift 8 one pop in 256 migrates a slot; the live set of about
    // 1100 slots in the high half needs a few hundred thousand steps.
    for _ in 0..400_000 {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        t.free(keep[i]);
        keep[i] = t.alloc(c);
        was_compacting |= t.cache(c).compacting();
    }
    assert!(was_compacting);
    let after = occupied(s, c);
    // Live data plus the holes the gate leaves (under COMPACT_MIN_SLABS
    // 64 KiB slabs' worth, i.e. 64 of the 4 KiB slabs) plus what the
    // cache holds (cap = 128 slots, 32 slabs) plus one partial slab.
    let bound = ideal + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
    assert!(after <= bound, "{after} occupied slabs after the churn, ideal {ideal}, bound {bound}, before {before}");
    assert!(!Heap::fragmented(s, 0, c));
    assert!(!t.cache(c).compacting(), "still compacting a dense class");
    assert_eq!(live(s, c), keep.len() + t.len(c));
    for p in keep {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn direct_claims_do_not_block_compaction_but_cache_refills_do() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x0bad_cafe);
    assert!(Heap::fragmented(s, 0, c));
    // A balanced churn with a defragmenter-like stream of direct
    // (MALLOCX_TCACHE_NONE) claims at 3 slabs' worth per probe window
    // (over the gate, were it counting `claimed`; light enough not to
    // fill the residue's holes by itself): `claimed` races ahead,
    // `refilled` does not, compaction engages and slabs empty.
    let mut st = 0x1111_2222u64;
    let mut direct: *mut u8 = ptr::null_mut();
    let before_claimed = cs.claimed();
    let before = occupied(s, c);
    let mut was_compacting = false;
    let windows = 72;
    for step in 0..windows * (PROBE_MASK as usize + 1) {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        t.free(keep[i]);
        keep[i] = t.alloc(c);
        was_compacting |= t.cache(c).compacting();
        if step % 32 == 0 {
            // Like the defragmenter: a direct copy, the old one freed.
            if !direct.is_null() {
                s.free_direct(0, c, direct);
            }
            let (p, _) = s.alloc_direct(&s.arena0, c);
            assert!(!p.is_null());
            direct = p;
        }
    }
    assert!(cs.claimed() - before_claimed >= windows as u64 * COMPACT_MAX_REFILL_SLABS as u64 * CLASSES[c].compact_slots() as u64);
    assert!(was_compacting, "direct claims blocked compaction");
    // The churn model's bound: live data plus the holes the gate
    // leaves plus what the cache holds plus one partial slab.
    let bound = keep.len().div_ceil(nslots) + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
    assert!(occupied(s, c) <= bound, "little compacted: {} of {before} slabs still occupied, bound {bound}", occupied(s, c));
    s.free_direct(0, c, direct);
    for p in keep {
        t.free(p);
    }
    t.destroy();
    // A residue under a growing allocation stream (the 224-byte class,
    // 292 slots per slab, so two probe windows fit the synthetic
    // region): every 256 pops refill 256 slots, 4096 per window, far
    // over the gate; the class is being drained the ordinary way and
    // is left alone.
    let c = class_with(224);
    let cs = &s.arena0.classes[c];
    let t = TestHeap::new(s);
    let mut keep = residue(&t, c, 30, 560, 0x3333_4444);
    assert!(Heap::fragmented(s, 0, c));
    let before_refilled = cs.refilled();
    for _ in 0..2 * (PROBE_MASK as usize + 1) + 64 {
        keep.push(t.alloc(c));
    }
    assert!(cs.refilled() - before_refilled >= 2 * 4096 - 256);
    assert!(!t.cache(c).compacting(), "a refilling class was compacted");
    for p in keep {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn random_walk_churn_model_compacts_too() {
    // Valkey's delete-phase pattern: frees and allocations of one class
    // arrive in random order (memtier DEL:SET over random keys), so the
    // live count and the cache length random-walk instead of the strict
    // alternation of `churn_model_...`. The bound is the same.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x5151_a1a1_7777_0f0f);
    let target = keep.len();
    let before = occupied(s, c);
    let mut st = 0x1357_9bdfu64;
    let mut was_compacting = false;
    for _ in 0..400_000 {
        let r = xorshift(&mut st);
        let go_free = if keep.len() > target + 100 { true } else if keep.len() + 100 < target { false } else { r & 1 == 0 };
        if go_free {
            let i = ((r >> 8) % keep.len() as u64) as usize;
            t.free(keep.swap_remove(i));
        } else {
            keep.push(t.alloc(c));
        }
        was_compacting |= t.cache(c).compacting();
    }
    assert!(was_compacting);
    let after = occupied(s, c);
    let bound = keep.len().div_ceil(nslots) + COMPACT_MIN_SLABS * CLASSES[c].compact_slots() / nslots + default_cap(c) as usize / nslots + 1;
    assert!(after <= bound, "{after} occupied slabs after the random walk, bound {bound}, before {before}");
    for p in keep {
        t.free(p);
    }
    t.destroy();
}

#[test]
fn compaction_off_keeps_the_lifo_reuse_in_place() {
    let s = shared_with(Config { tcache_compact_shift: 0, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x1234_5678);
    let before = occupied(s, c);
    let mut st = 0x8765_4321u64;
    for _ in 0..50_000 {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        t.free(keep[i]);
        let p = t.alloc(c);
        assert_eq!(p, keep[i], "LIFO reuse in place");
    }
    assert_eq!(occupied(s, c), before);
    assert!(!t.cache(c).compacting());
    for p in keep {
        t.free(p);
    }
    t.destroy();
}

#[test]
fn randomised_model_check() {
    let s = shared();
    let heaps: Vec<TestHeap> = (0..3).map(|_| TestHeap::new(s)).collect();
    let classes = [class_with(16), class_with(48), class_with(64), class_with(256), class_with(4096)];
    let mut model: HashSet<usize> = HashSet::new();
    let mut order: Vec<(usize, usize)> = Vec::new();
    let mut st = 0xdead_beef_cafe_f00du64;
    for step in 0..60_000 {
        let r = xorshift(&mut st);
        let t = &heaps[(r >> 8) as usize % heaps.len()];
        if model.len() < 2000 && (r & 3 != 0 || model.is_empty()) {
            let ci = (r >> 16) as usize % classes.len();
            let c = classes[ci];
            let p = t.alloc(c) as usize;
            assert!(model.insert(p), "step {step}: duplicate live pointer {p:#x}");
            assert_eq!(class_of(CLASSES[c].size()), Some(c));
            assert!(matches!(s.regions.classify(p), Kind::Slab { class, .. } if class as usize == c));
            order.push((p, c));
        } else {
            let i = (r >> 20) as usize % order.len();
            let (p, _) = order.swap_remove(i);
            assert!(model.remove(&p));
            t.free(p as *mut u8);
        }
        if step % 5000 == 0 {
            // Invariant: live == program-live + cached, per class.
            for &c in &classes {
                let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
                let cached: usize = heaps.iter().map(|h| h.heap().cache(c).map_or(0, |k| k.len())).sum();
                assert_eq!(live(s, c), prog + cached, "step {step} class {c}");
            }
        }
    }
    for t in heaps {
        t.destroy();
    }
    for &c in &classes {
        let prog = order.iter().filter(|&&(_, cc)| cc == c).count();
        assert_eq!(live(s, c), prog);
    }
}

#[test]
#[should_panic(expected = "not an allocation")]
fn free_of_a_pointer_outside_every_region_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let x = 0u64;
    t.free(&x as *const u64 as *mut u8);
}

#[test]
#[should_panic(expected = "not an allocation")]
fn free_of_a_misaligned_pointer_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let p = t.alloc(class_with(64));
    t.free(unsafe { p.add(8) });
}

#[test]
#[should_panic(expected = "invalid or double free")]
fn free_of_a_never_allocated_slot_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(16);
    let p = t.alloc(c);
    // A slot of slab 0 above the first refill's HALF is committed but
    // was never claimed (the 16-byte slab has 1024 slots).
    t.free(unsafe { p.add((HALF + 200) * 16) });
}

#[test]
#[should_panic(expected = "was never allocated")]
fn free_into_an_uncommitted_slab_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _ = t.alloc(c);
    t.free(s.regions.slab_addr(c, 5) as *mut u8);
}

#[test]
#[should_panic(expected = "invalid or double free")]
fn stale_double_free_aborts_at_the_check() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    t.free(p);
    t.flush_all();
    t.free(p);
}

#[test]
#[should_panic(expected = "double free of")]
fn cached_double_free_aborts_at_the_flush() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    t.free(p);
    // The bit is still set while `p` is cached, so the check passes
    // and the duplicate is caught when the batch is released.
    t.free(p);
    t.flush_all();
}

#[test]
#[should_panic(expected = "not the start of a huge allocation")]
fn free_of_an_unallocated_huge_pointer_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    t.free(s.regions.huge_base as *mut u8);
}

#[test]
fn huge_runs_go_through_the_heap_without_caches() {
    let s = shared();
    let t = TestHeap::new(s);
    let size = 300 * 1024;
    let (p, zero) = t.alloc_huge(size, 16);
    assert!(!p.is_null() && zero);
    assert_eq!(s.regions.classify(p as usize), Kind::Huge { page: 0 });
    assert_eq!(s.huge.usable_size(0), s.huge.usable_for(size).unwrap());
    assert_eq!(t.stats().allocated, s.huge.usable_for(size).unwrap() as u64);
    let (q, _) = t.alloc_huge(size, 1 << 20);
    assert_eq!(q as usize % (1 << 20), 0);
    // Frees route by region: a huge free never touches a cache.
    t.free(p);
    assert_eq!(t.stats().deallocated, s.huge.usable_for(size).unwrap() as u64);
    assert!((0..NCLASSES).all(|c| t.heap().cache(c).is_none()));
    assert_eq!(s.huge.stats().runs, 1);
    // The heap-less path frees huge runs too.
    s.free_huge(match s.regions.classify(q as usize) {
        Kind::Huge { page } => page,
        other => panic!("{other:?}"),
    });
    assert_eq!(s.huge.stats().runs, 0);
    assert!(s.alloc_huge(usize::MAX, 1, 0).0.is_null());
}
/// Design 11: the periodic tick's GC interval is the configured one
/// until the policy's immediate band, where it is 0; the effective
/// decay period follows the band for arena 0 and the huge allocator;
/// `-1` stays never. The host has no cgroup, so the policy is driven
/// through `apply` here.
#[test]
fn cgroup_policy_drives_gc_interval_and_decay_period() {
    use crate::policy::{Band, KEEP_ALL, band, effective_decay_ns};
    let s = shared();
    // At init the outcome is the discovery sample's: no limit on the
    // host and under Docker's default profile, a band under
    // `--memory=512m`.
    let at_init = band(s.cgroup.limit().min(s.cgroup.high()), s.cgroup.current());
    assert_eq!(s.policy.band(), at_init);
    assert_eq!(s.gc_interval(), if s.policy.immediate() { 0 } else { s.gc_interval_ns });
    assert_eq!(s.policy.effective_decay_ns(s.decay_ns()), effective_decay_ns(s.decay_ns(), at_init));
    assert_eq!(crate::policy::empty_bytes(s), 0);
    let l = 512u64 << 20;
    s.policy.apply(band(l, l * 95 / 100), l, 0, 0);
    assert_eq!(s.gc_interval(), 0);
    assert_eq!(s.policy.effective_decay_ns(s.decay_ns()), 0);
    assert_eq!(s.policy.effective_decay_ns(crate::purge::NEVER), crate::purge::NEVER);
    s.policy.apply(band(l, l * 8 / 10), l, 0, 0);
    assert_eq!(s.gc_interval(), s.gc_interval_ns);
    let eff = s.policy.effective_decay_ns(s.decay_ns());
    assert!(eff < s.decay_ns() && eff > 1_000_000_000, "{eff}");
    // A decay pass with a pending full purge consumes it.
    s.policy.apply(band(l, l * 8 / 10), l, 0, 1);
    assert!(s.policy.full_purge_pending());
    let _ = crate::purge::decay(s, os::now_ns());
    assert!(!s.policy.full_purge_pending());
    assert_eq!(s.policy.full_purges(), 1);
    s.policy.apply(Band::Unlimited, crate::sys::cgroup::UNLIMITED, 0, 1);
    assert_eq!(s.policy.keep16(), KEEP_ALL);
    assert_eq!(s.gc_interval(), s.gc_interval_ns);
}

/// The registry: arena 0 is `arena0`, automatic arenas are 0 alone by
/// default, explicit ones come from `create` in order and are built in
/// the metadata region with their own class states over the shared
/// tables; unknown indices are `None`.
#[test]
fn arena_registry_creates_and_looks_up() {
    let s = shared();
    assert_eq!(s.arenas.count(), 1);
    assert_eq!(s.arenas.auto(), 1);
    assert!(!s.multi_arena());
    assert!(core::ptr::eq(s.arena(0).unwrap(), &s.arena0));
    assert!(s.arena(1).is_none());
    assert!(s.arena(4095).is_none());
    assert!(s.arenas.get_existing(1).is_none());
    for _ in 0..8 {
        assert_eq!(s.arenas.assign(), 0);
    }
    let a1 = s.arenas.create(s).unwrap();
    let a2 = s.arenas.create(s).unwrap();
    assert_eq!((a1, a2), (1, 2));
    assert_eq!(s.arenas.count(), 3);
    assert!(s.multi_arena());
    let a = s.arena(1).unwrap();
    assert_eq!(a.index(), 1);
    assert!(s.regions.in_meta(a as *const Arena as usize));
    assert!(core::ptr::eq(a.class(3).table(), s.table(3)));
    assert_eq!(a.class(3).arena(), 1);
    assert_eq!(a.decay_ns(), s.decay_ns());
    assert!(s.arena(3).is_none());
    let seen: Vec<u16> = s.arenas.iter().map(|a| a.index()).collect();
    assert_eq!(seen, [0, 1, 2]);
    // Still assigning 0: created arenas are explicit, not automatic.
    assert_eq!(s.arenas.assign(), 0);
    a.set_decay_ms(-1);
    assert_eq!(a.decay_ns(), crate::purge::NEVER);
    assert_ne!(s.decay_ns(), crate::purge::NEVER);
    s.set_decay_ms(5);
    assert_eq!(a.decay_ns(), 5_000_000);
    assert_eq!(s.decay_ns(), 5_000_000);
}

/// `narenas:3`: a new heap is bound to the automatic arena with the
/// fewest bound heaps (ties: a built one first, then the lowest index),
/// `destroy` releases the binding, and arenas are built on first touch.
#[test]
fn automatic_arenas_are_assigned_least_loaded_and_built_lazily() {
    let s = shared_with(Config { narenas: 3, ..Config::DEFAULT });
    assert_eq!(s.arenas.count(), 3);
    assert_eq!(s.arenas.auto(), 3);
    assert!(s.arenas.get_existing(1).is_none());
    // Six live heaps: two per arena, lowest index first.
    let mut heaps: Vec<TestHeap> = (0..6).map(|_| TestHeap::new(s)).collect();
    let homes: Vec<u16> = heaps.iter().map(|t| t.heap().arena()).collect();
    assert_eq!(homes, [0, 1, 2, 0, 1, 2]);
    assert_eq!((0..3).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(), [2, 2, 2]);
    assert!(s.arenas.get_existing(1).is_none(), "assignment alone must not build");
    assert!(!s.multi_arena());
    // Destroying a heap frees its binding; the next heap takes the
    // vacated arena.
    heaps.remove(1).destroy();
    assert_eq!(s.arenas.bound(1), 1);
    let t = TestHeap::new(s);
    assert_eq!(t.heap().arena(), 1);
    assert_eq!(s.arenas.bound(1), 2);
    // Explicit arenas and explicit caches take no part in it.
    assert!(s.arenas.create(s).is_some_and(|i| i == 3));
    assert_eq!(s.arenas.bound(3), 0);
    let before = s.arenas.bound(0);
    let tc = s.tcaches.create(s, 0).unwrap();
    assert_eq!(s.arenas.bound(0), before);
    assert!(s.tcaches.destroy(s, tc));
    assert_eq!(s.arenas.bound(0), before);
    // First touch builds arena 1.
    let c = class_with(64);
    let p = t.alloc(c);
    assert!(s.arenas.get_existing(1).is_some());
    assert!(s.multi_arena());
    assert_eq!(s.arena0.classes[c].desc(0).arena(), 1);
    assert_eq!(s.classify_free(p), Ok((c, 1)));
    t.heap().free(s, p);
    assert_eq!(t.heap().cache(c).unwrap().len(), HALF);
    // `thread.arena` moves the binding with the heap.
    t.heap().set_arena(s, 3);
    assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 1));
    t.destroy();
    assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 0));
    for h in heaps {
        h.destroy();
    }
    assert_eq!((0..4).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(), [0, 0, 0, 0]);
}

/// Thread churn under a multi-arena default: a lone heap stays on
/// arena 0 and never sets `multi_arena` (a single-threaded program
/// never pays the `free` routing); a second heap that comes and goes
/// lands on arena 1 every time, so only the arenas that were ever
/// alive at once get built; a built idle arena beats an unbuilt one.
#[test]
fn heap_churn_reuses_the_vacated_arena() {
    let s = shared_with(Config { narenas: 8, ..Config::DEFAULT });
    let main = TestHeap::new(s);
    assert_eq!(main.heap().arena(), 0);
    let c = class_with(64);
    let p = main.alloc(c);
    assert!(!s.multi_arena(), "one heap: arena 0 alone");
    for _ in 0..20 {
        let w = TestHeap::new(s);
        assert_eq!(w.heap().arena(), 1);
        let q = w.alloc(c);
        w.free(q);
        w.destroy();
    }
    assert!(s.multi_arena());
    assert!(s.arenas.get_existing(1).is_some());
    assert!((2..8).all(|i| s.arenas.get_existing(i).is_none()), "only arenas 0 and 1 were ever alive at once");
    assert_eq!((s.arenas.bound(0), s.arenas.bound(1)), (1, 0));
    // Two workers alive at once: the second takes a third arena.
    let w1 = TestHeap::new(s);
    let w2 = TestHeap::new(s);
    assert_eq!((w1.heap().arena(), w2.heap().arena()), (1, 2));
    w1.destroy();
    // Arena 1 (built, idle) is taken before arena 2 would be shared.
    let w3 = TestHeap::new(s);
    assert_eq!(w3.heap().arena(), 1);
    w2.destroy();
    // Arenas 2..8 are all idle and unbuilt: lowest index.
    let w4 = TestHeap::new(s);
    assert_eq!(w4.heap().arena(), 2);
    w3.destroy();
    w4.destroy();
    main.free(p);
    main.destroy();
    assert!((0..8).all(|i| s.arenas.bound(i) == 0));
}

/// `narenas:0` (the default) is resolved at init from the CPU count
/// and stored, so `opt.narenas` and `arenas.auto` agree on it.
#[test]
fn default_narenas_is_resolved_at_init() {
    let s = shared_with(Config::DEFAULT);
    let expect = Config::DEFAULT.effective_narenas(os::ncpus());
    assert!((4..=crate::config::MAX_AUTO_ARENAS).contains(&expect));
    assert_eq!(s.config.narenas, expect);
    assert_eq!(s.arenas.auto(), expect);
    assert_eq!(s.arenas.count(), expect);
    let s1 = shared_with(Config { narenas: 1, ..Config::DEFAULT });
    assert_eq!((s1.config.narenas, s1.arenas.auto()), (1, 1));
}

/// A heap keeps one cache per (arena, class): allocations from another
/// arena come from and go back to that arena's slabs through a cache on
/// the `others` list, plain `free` routes by the slab's owner, and the
/// fast path refuses another arena's slot.
#[test]
fn caches_are_keyed_by_arena() {
    let s = shared();
    let a1 = s.arenas.create(s).unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    assert_eq!(h.arena(), 0);
    let c = class_with(64);
    let p0 = t.alloc(c);
    let (p1, zero) = h.alloc_in(s, a1, c);
    assert!(!p1.is_null() && zero);
    assert_eq!(s.classify_free(p0), Ok((c, 0)));
    assert_eq!(s.classify_free(p1), Ok((c, a1)));
    assert_ne!(s.arena0.classes[c].locate(p0).0, s.arena0.classes[c].locate(p1).0);
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF - 1);
    // One refill spans `HALF / nslots` slabs (one 1024-slot slab at 64 bytes).
    let per_refill = HALF.div_ceil(CLASSES[c].nslots());
    assert_eq!(s.arena(a1).unwrap().classes[c].stats().committed, per_refill);
    assert_eq!(s.arena0.classes[c].stats().committed, per_refill);
    // Non-existent arena: null, nothing touched.
    assert!(h.alloc_in(s, 77, c).0.is_null());
    // The fast path takes only the home arena's slots.
    assert!(free_fast(s, h, p0, s.push_flags()));
    assert!(!free_fast(s, h, p1, s.push_flags()));
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    h.free(s, p1);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF);
    // The next allocation from arena 1 is the LIFO top of its cache.
    assert_eq!(h.alloc_in(s, a1, c).0, p1);
    h.free(s, p1);
    // Byte counters cover both arenas.
    let st = h.stats();
    assert_eq!(st.allocated, 3 * 64);
    assert_eq!(st.deallocated, 3 * 64);
    // Flushing returns everything to the right owner.
    h.flush_all(s);
    assert_eq!(h.cache(c).unwrap().len(), 0);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), 0);
    assert_eq!(s.arena0.classes[c].stats().empty, per_refill);
    assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, per_refill);
    // Direct paths against an explicit arena.
    let a = s.arena(a1).unwrap();
    let (d, _) = s.alloc_direct(a, c);
    assert_eq!(s.classify_free(d), Ok((c, a1)));
    assert_eq!(a.classes[c].desc(s.arena0.classes[c].locate(d).0).live(), 1);
    s.free_direct(a1, c, d);
    assert_eq!(a.classes[c].stats().empty, per_refill);
    // Destroying the heap frees the other-arena caches too.
    let before = s.meta.stats().allocated;
    unsafe { Heap::destroy(t.h, s) };
    assert!(s.meta.stats().allocated < before);
    core::mem::forget(t);
}

/// A shared class (above `tcache_max`) comes from arena 0 on the
/// default path whatever the home arena, but `MALLOCX_ARENA(a)`
/// keeps `a`, home or not (design 13.3), and a heap creates no
/// `others` node for a class it never caches there (`Config::cap_for`).
#[test]
fn explicit_arena_keeps_a_shared_class_in_its_arena() {
    let s = shared();
    let a1 = s.arenas.create(s).unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    let c = class_with(64 * 1024);
    assert!(s.config.shared_class(c));
    assert_eq!(s.config.cap_for(0, 0, c), 0);
    assert_eq!(s.config.cap_for(0, a1, c), 0);
    h.set_arena(s, a1);
    // Default path: arena 0.
    let (d, _) = h.alloc_zeroed(s, c);
    assert_eq!(s.classify_free(d), Ok((c, 0)));
    // The home arena named explicitly: stays home.
    let (p, _) = h.alloc_in(s, a1, c);
    assert_eq!(s.classify_free(p), Ok((c, a1)));
    assert_eq!(s.arena(a1).unwrap().classes[c].stats().committed, 1);
    // A foreign arena named explicitly, through a heap that caches
    // nothing of that class there: no node, no cache, right arena.
    let before = s.meta.stats().allocated;
    let (q, _) = h.alloc_in(s, 0, c);
    assert_eq!(s.classify_free(q), Ok((c, 0)));
    assert_eq!(s.meta.stats().allocated, before, "no others node for an uncached class");
    assert!(h.cache_in(0, c).is_none());
    assert!(h.cache(c).is_none());
    let st = h.stats();
    assert_eq!(st.allocated, 3 * 64 * 1024);
    h.free(s, d);
    h.free(s, p);
    h.free(s, q);
    assert_eq!(s.meta.stats().allocated, before);
    assert_eq!(h.stats().deallocated, 3 * 64 * 1024);
    assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, 1);
    assert_eq!(s.arena0.classes[c].stats().empty, 1);
}

/// `thread.arena` with no metadata for an `others` node drops the old
/// home's caches (`drop_home_caches`); their counters are folded into
/// `uncached` first, so `thread.allocated` and `thread.deallocated`
/// do not move and keep counting afterwards.
#[test]
fn dropping_the_home_caches_keeps_the_thread_counters() {
    let s = shared();
    let t = TestHeap::new(s);
    let h = t.heap();
    let c = class_with(64);
    let p = t.alloc(c);
    let q = t.alloc(c);
    h.free(s, p);
    let before = h.stats();
    assert_eq!((before.allocated, before.deallocated), (128, 64));
    let meta_before = s.meta.stats().allocated;
    h.drop_home_caches(s);
    assert!(h.cache(c).is_none());
    assert!(s.meta.stats().allocated < meta_before, "the stack was freed");
    let after = h.stats();
    assert_eq!((after.allocated, after.deallocated), (before.allocated, before.deallocated));
    h.free(s, q);
    let again = h.stats();
    assert_eq!((again.allocated, again.deallocated), (128, 128));
    assert!(h.cache(c).is_some(), "the slow path created a new cache");
}

/// `thread.arena`: the home arena changes without flushing; the old
/// home's caches move to the list and back.
#[test]
fn set_arena_swaps_cache_arrays() {
    let s = shared();
    let a1 = s.arenas.create(s).unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    let c = class_with(64);
    let p0 = t.alloc(c);
    let (p1, _) = h.alloc_in(s, a1, c);
    h.set_arena(s, a1);
    assert_eq!(h.arena(), a1);
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(0, c).unwrap().len(), HALF - 1);
    // Plain allocation now comes from arena 1's cache.
    let q = t.alloc(c);
    assert_eq!(s.classify_free(q), Ok((c, a1)));
    assert!(free_fast(s, h, p1, s.push_flags()));
    assert!(!free_fast(s, h, p0, s.push_flags()));
    h.free(s, p0);
    h.free(s, q);
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert_eq!(h.cache_in(0, c).unwrap().len(), HALF);
    h.set_arena(s, 0);
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF);
    h.flush_all(s);
    let per_refill = HALF.div_ceil(CLASSES[c].nslots());
    assert_eq!(s.arena0.classes[c].stats().empty, per_refill);
    assert_eq!(s.arena(a1).unwrap().classes[c].stats().empty, per_refill);
}

/// Explicit caches are heaps owned by no thread with their own ids.
#[test]
fn explicit_caches_create_flush_destroy() {
    let s = shared();
    assert!(s.tcaches.is_empty());
    let t0 = s.tcaches.create(s, 0).unwrap();
    let t1 = s.tcaches.create(s, 0).unwrap();
    assert_eq!((t0, t1), (0, 1));
    assert_eq!(s.tcaches.len(), 2);
    assert!(s.tcaches.get(2).is_none());
    assert!(s.tcaches.get(u16::MAX).is_none());
    let c = class_with(64);
    let h = s.tcaches.get(t0).unwrap();
    let (p, _) = h.alloc_in(s, 0, c);
    assert!(!p.is_null());
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    h.free(s, p);
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert!(s.tcaches.flush(s, t0));
    assert_eq!(s.tcaches.get(t0).unwrap().cache(c).unwrap().len(), 0);
    assert!(!s.tcaches.flush(s, 9));
    assert!(s.tcaches.destroy(s, t0));
    assert!(!s.tcaches.destroy(s, t0));
    assert!(s.tcaches.get(t0).is_none());
    assert_eq!(s.tcaches.len(), 1);
    // The slot is reused.
    assert_eq!(s.tcaches.create(s, 0), Some(t0));
}

// The opt-in `zero_on_free` and `slot_randomize` keys.

/// The two push flags are one byte on `Shared`'s first line; the
/// default configuration leaves it zero.
#[test]
fn push_flags_encode_zero_on_free_and_double_free_scan() {
    // `PUSH_PAC` follows the host; the two opt-in bits are what this
    // test is about.
    let pac = if pac::available() { PUSH_PAC } else { 0 };
    assert_eq!(shared().push_flags(), pac, "the test builder asks for pac_pointers");
    assert!(!shared().zero_on_free());
    let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
    assert_eq!(s.push_flags(), PUSH_ZERO, "pac_pointers is off by default");
    assert!(s.zero_on_free() && !s.double_free_scan());
    let s = shared_with(Config { narenas: 1, zero_on_free: true, double_free_scan: true, pac_pointers: true, ..Config::DEFAULT });
    assert_eq!(s.push_flags(), PUSH_ZERO | PUSH_SCAN | pac);
    assert!(s.zero_on_free() && s.double_free_scan());
    assert_eq!(core::mem::offset_of!(Shared, push_flags) / 128, core::mem::offset_of!(Shared, multi_arena) / 128);
}

/// Slot memory is real on Linux only (`shared_with_guards`).
#[cfg(target_os = "linux")]
fn bytes_of(p: *mut u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| unsafe { core::ptr::read_volatile(p.add(i)) }).collect()
}

/// With the key a freed slot reads as zero the moment `free` returns,
/// through the plain path (cached), the sized path and the direct
/// release; without it the slot keeps its bytes (the default path does
/// not clear).
#[cfg(target_os = "linux")]
#[test]
fn zero_on_free_clears_the_slot_on_the_plain_sized_and_direct_paths() {
    let c = class_with(64);
    let size = CLASSES[c].size();
    let dirty = |t: &TestHeap| {
        let p = t.alloc(c);
        unsafe { ptr::write_bytes(p, 0xA5, size) };
        p
    };
    // Default: the bytes survive the free (LIFO reuse hands them back).
    let s = shared();
    let t = TestHeap::new(s);
    let p = dirty(&t);
    t.free(p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0xA5), "the default path cleared a slot");
    assert_eq!(t.alloc(c), p);
    t.free(p);
    t.flush_all();
    t.destroy();

    let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    // Plain free: cleared, then the same slot comes back zero without
    // `calloc`'s memset.
    let p = dirty(&t);
    t.free(p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0), "plain free left bytes");
    assert_eq!(t.alloc(c), p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0));
    // Sized path (`sdallocx`): the size hint's class, no bit test. With
    // the key the fast push declines and `free_sized_slow` takes the
    // exact route, which clears the slot in `free_slow_in`.
    unsafe { ptr::write_bytes(p, 0xA5, size) };
    assert!(!free_sized_fast(s, t.heap(), p, Some(c), s.push_flags()), "the fast push accepted a slot under zero_on_free");
    assert!(bytes_of(p, size).iter().all(|&b| b == 0xA5), "the declined push touched the slot");
    s.check_size_hint(p, size, Some(c));
    t.free(p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0), "sized free left bytes");
    assert_eq!(t.alloc(c), p);
    // A full cache: the slot goes through `free_slow` after the clear.
    let cap = t.cache(c).cap() as usize;
    let ps: Vec<*mut u8> = (0..cap).map(|_| dirty(&t)).collect();
    for &q in &ps {
        t.free(q);
    }
    for &q in &ps {
        assert!(bytes_of(q, size).iter().all(|&b| b == 0), "a flushed or cached slot kept bytes");
    }
    // Direct release (`MALLOCX_TCACHE_NONE`), with no cache involved.
    unsafe { ptr::write_bytes(p, 0xA5, size) };
    super::free_direct(s, p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0), "direct free left bytes");
    t.flush_all();
    t.destroy();
}

/// A huge run's usable bytes are cleared before the release, on the
/// thread path and the direct path; the run is retained (`Empty`) so
/// the memory is still there to read.
#[cfg(target_os = "linux")]
#[test]
fn zero_on_free_clears_a_huge_run() {
    let s = shared_with(Config { narenas: 1, zero_on_free: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let n = 1 << 20;
    for direct in [false, true] {
        let (p, _) = t.alloc_huge(n, page_size());
        assert!(!p.is_null());
        let usable = s.huge.usable_for(n).unwrap();
        assert!(usable >= n);
        unsafe { ptr::write_bytes(p, 0xA5, usable) };
        if direct { super::free_direct(s, p) } else { t.free(p) }
        let b = bytes_of(p, usable);
        assert!(b.iter().all(|&x| x == 0), "huge free (direct {direct}) left bytes: first non-zero at {:?}", b.iter().position(|&x| x != 0));
    }
    // Default: untouched.
    let s = shared();
    let t2 = TestHeap::new(s);
    let (p, _) = t2.alloc_huge(n, page_size());
    unsafe { ptr::write_bytes(p, 0xA5, n) };
    t2.free(p);
    assert!(bytes_of(p, n).iter().all(|&x| x == 0xA5), "the default huge free cleared the run");
    t2.destroy();
    t.destroy();
}

/// Heap creation makes no `getrandom` call: the state stays 0 until
/// the first randomised claim, and never under the default.
#[test]
fn slot_randomize_seeds_the_heap_lazily() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    assert_eq!(t.heap().rng, 0);
    let p = t.alloc(c);
    assert_eq!(t.heap().rng, 0, "the default refill touched the rng");
    t.free(p);
    t.flush_all();
    t.destroy();
    let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    assert_eq!(t.heap().rng, 0, "heap creation seeded the rng");
    let p = t.alloc(c);
    let seed = t.heap().rng;
    assert_ne!(seed, 0, "the first randomised refill did not seed");
    let q = t.alloc(c);
    assert_eq!(t.heap().rng, seed, "a pop advanced the rng");
    t.free(p);
    t.free(q);
    t.flush_all();
    t.destroy();
}

/// Under the key a refill's batch is the same slot set an ordered
/// claim would take from the slab (`plan` and the prefix claim are
/// untouched, so `Claim::zeroed` is too) in a non-address order; the
/// second batch from the now `Active` slab (the bit loop from a random
/// word) is the rest of the slab, shuffled as well. The default hands
/// the slots out by ascending address.
#[test]
fn slot_randomize_shuffles_the_batch_and_keeps_the_slot_set() {
    let c = class_with(64);
    let size = CLASSES[c].size();
    let nslots = CLASSES[c].nslots();
    assert_eq!(nslots, 1024);
    let batch = HALF;
    let contiguous = |ps: &[*mut u8]| {
        let mut v: Vec<usize> = ps.iter().map(|&p| p as usize).collect();
        v.sort_unstable();
        v.windows(2).all(|w| w[1] - w[0] == size)
    };
    let ascending = |ps: &[*mut u8]| ps.windows(2).all(|w| w[0] < w[1]);

    let s = shared();
    let t = TestHeap::new(s);
    let first: Vec<*mut u8> = (0..batch).map(|_| t.alloc(c)).collect();
    assert!(ascending(&first), "the default order is not ascending");
    assert!(contiguous(&first));
    for &p in &first {
        t.free(p);
    }
    t.flush_all();
    t.destroy();

    let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let cs = &s.arena0.classes[c];
    let (first_z, zeroed): (Vec<*mut u8>, Vec<bool>) = (0..batch).map(|_| t.heap().alloc_zeroed(s, c)).unzip();
    assert!(zeroed.iter().all(|&z| z), "the first batch from a Fresh slab is zero whatever its order");
    assert!(!ascending(&first_z), "randomised batch is address-ordered");
    assert!(!first_z.windows(2).all(|w| w[0] > w[1]), "randomised batch is reverse-ordered");
    assert!(contiguous(&first_z), "the first batch is not the slab's first {batch} slots");
    let slab = cs.locate(first_z[0]).0;
    assert!(first_z.iter().all(|&p| cs.locate(p).0 == slab));
    let second: Vec<*mut u8> = (0..batch).map(|_| t.alloc(c)).collect();
    assert!(!ascending(&second) && contiguous(&second), "the second batch (bit loop) is ordered or not one run of slots");
    assert!(second.iter().all(|&p| cs.locate(p).0 == slab));
    let mut all: Vec<usize> = first_z.iter().chain(&second).map(|&p| p as usize).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), nslots, "the two batches are not the whole slab");
    assert_eq!(live(s, c), nslots);
    for p in first_z.into_iter().chain(second) {
        t.free(p);
    }
    t.flush_all();
    t.destroy();
    assert_eq!(live(s, c), 0);
}

/// The churn model with randomisation on: the cache still switches to
/// compacting and rebuilds its run (the run is a shuffled block, so the
/// scheduled pop serves a random slot of the lowest slab rather than
/// its lowest, the documented trade), every slot handed out is a valid,
/// distinct slot of the class and the live accounting holds.
#[test]
fn slot_randomize_keeps_the_compacting_run_valid() {
    let s = shared_with(Config { narenas: 1, slot_randomize: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
    assert!(Heap::fragmented(s, 0, c));
    let mut held: HashSet<usize> = keep.iter().map(|&p| p as usize).collect();
    let mut st = 0x2468_ace0u64;
    let mut was_compacting = false;
    let mut rebuilt = false;
    for _ in 0..150_000 {
        let i = (xorshift(&mut st) % keep.len() as u64) as usize;
        assert!(held.remove(&(keep[i] as usize)));
        t.free(keep[i]);
        let run_before = t.heap().cache(c).map_or(0, |k| k.run().len());
        let p = t.alloc(c);
        let k = t.cache(c);
        was_compacting |= k.compacting();
        rebuilt |= k.compacting() && run_before == 0 && !k.run().is_empty();
        // A valid slot: the class's arithmetic locates it (aborts
        // otherwise) and nobody else holds it.
        let (slab, slot) = cs.locate(p);
        assert!(slab < cs.committed_slabs() && slot < nslots);
        assert!(held.insert(p as usize), "slot handed out twice");
        assert!(k.run().len() <= k.low_water() && k.low_water() <= k.len());
        keep[i] = p;
    }
    assert!(was_compacting, "never compacting");
    assert!(rebuilt, "no rebuild observed");
    assert_eq!(live(s, c), keep.len() + t.len(c));
    for p in keep {
        t.free(p);
    }
    t.destroy();
    assert_eq!(live(s, c), 0);
}

// The four cache invariants are release assertions, not
// `debug_assert!`s: each test below builds the violating state by hand
// and expects the abort.

#[test]
#[should_panic(expected = "cache run invariant violated")]
fn a_scheduled_pop_aborts_when_the_run_reaches_past_the_length() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    let len = h.cache(c).unwrap().len() as u32;
    assert!(len > 1);
    let k = h.cache_ptr(c);
    unsafe {
        (*k).mode = MODE_COMPACT;
        (*k).run_end = len + 1;
        h.pop_compact_run(k, c, len);
    }
}

#[test]
#[should_panic(expected = "refill of a cache that is not empty")]
fn a_refill_of_a_cache_that_is_not_empty_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    assert!(h.cache(c).unwrap().len() > 0);
    let a = s.arena(0).unwrap();
    let k = h.cache_ptr(c);
    unsafe { h.refill(s, a, k, c) };
}

#[test]
#[should_panic(expected = "freeing a cache that was not flushed")]
fn freeing_caches_that_were_not_flushed_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let _p = t.alloc(c);
    let h = t.heap();
    assert!(h.cache(c).unwrap().len() > 0);
    Heap::free_caches(s, &mut h.caches);
}

#[test]
#[should_panic(expected = "others node requested for the home arena")]
fn an_others_node_for_the_home_arena_aborts() {
    let s = shared();
    let t = TestHeap::new(s);
    let h = t.heap();
    let home = h.arena;
    assert!(h.other_caches(s, home, false).is_null());
    h.other_caches(s, home, true);
}

/// The cold walks of the `others` list ([`Heap::nodes`]) see every node,
/// the most recently created first, and a walk may free the node it was
/// handed (`destroy` does).
#[test]
fn others_nodes_iterate_newest_first_and_survive_freeing() {
    let s = shared();
    let a1 = s.arenas.create(s).unwrap();
    let a2 = s.arenas.create(s).unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    assert_eq!(Heap::nodes(h.others).count(), 0);
    let c = class_with(64);
    let (p1, _) = h.alloc_in(s, a1, c);
    let (p2, _) = h.alloc_in(s, a2, c);
    assert!(!p1.is_null() && !p2.is_null());
    let arenas: Vec<u16> = Heap::nodes(h.others).map(|o| unsafe { (*o).arena }).collect();
    assert_eq!(arenas, [a2, a1]);
    assert!(h.cache_in(a1, c).is_some() && h.cache_in(a2, c).is_some());
    assert!(h.cache_in(7, c).is_none(), "an arena never touched has no node");
    assert_eq!(h.stats().allocated, 2 * 64, "the byte counters fold every node");
    h.free(s, p1);
    h.free(s, p2);
    assert_eq!(h.stats().deallocated, 2 * 64);
    // Turning the caches off walks the list, flushes and frees every
    // node's caches (the nodes stay) and folds their counters.
    h.set_tcache_enabled(s, false);
    assert_eq!(Heap::nodes(h.others).count(), 2);
    assert!(h.cache_in(a1, c).is_none() && h.cache_in(a2, c).is_none());
    assert_eq!(h.stats(), ThreadStats { allocated: 2 * 64, deallocated: 2 * 64 });
    // `destroy` frees each node as the walk hands it over.
    let before = s.meta.stats().allocated;
    unsafe { Heap::destroy(t.h, s) };
    assert!(s.meta.stats().allocated < before);
    core::mem::forget(t);
}

/// The compaction probe's gate ([`Heap::probe_verdict`], shared by the
/// thread and per-CPU scheduled pops): dense classes keep probing, a
/// fragmented class enters compaction (`compact_on` counted) unless
/// ordinary refills claimed the gate's worth of slabs since the previous
/// probe (`gate_refill` counted). The refill counter it returns is the
/// class's current one.
#[test]
fn probe_verdict_applies_the_gate_and_records_it() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let ps: Vec<*mut u8> = (0..4).map(|_| t.alloc(c)).collect();
    assert!(!Heap::fragmented(s, 0, c));
    let now = cs.refilled() as u32;
    assert_eq!(Heap::probe_verdict(s, 0, c, now), (now, Verdict::Dense));
    for p in &ps {
        t.free(*p);
    }
    let keep = residue(&t, c, MODEL_SLOTS / CLASSES[c].nslots(), 560, 0x0bad_cafe);
    assert!(Heap::fragmented(s, 0, c));
    let now = cs.refilled() as u32;
    let before = cs.compaction_stats();
    assert_eq!(Heap::probe_verdict(s, 0, c, now), (now, Verdict::Fragmented));
    let after = cs.compaction_stats();
    assert_eq!((after.1 - before.1, after.3 - before.3), (1, 0), "(compact_on, gate_refill)");
    let gate = (COMPACT_MAX_REFILL_SLABS * CLASSES[c].compact_slots()) as u32;
    let before = cs.compaction_stats();
    assert_eq!(Heap::probe_verdict(s, 0, c, now.wrapping_sub(gate)), (now, Verdict::Refilling));
    let after = cs.compaction_stats();
    assert_eq!((after.1 - before.1, after.3 - before.3), (0, 1), "(compact_on, gate_refill)");
    assert_eq!(Heap::probe_verdict(s, 0, c, now.wrapping_sub(gate - 1)).1, Verdict::Fragmented, "one refill short of the gate");
    for p in keep {
        t.free(p);
    }
    t.destroy();
}

/// The mode-to-mask rule has one home ([`CacheHeader::mask_for`]): the
/// per-CPU volatile store and the thread store agree for every mode.
#[test]
fn set_mode_volatile_derives_the_same_mask_as_set_mode() {
    let config = Config { tcache_compact_shift: 5, ..Config::DEFAULT };
    for mode in [MODE_OFF, MODE_PROBE, MODE_COMPACT] {
        let mut a = CacheHeader::EMPTY;
        let mut b = CacheHeader::EMPTY;
        a.set_mode(mode, &config);
        unsafe { CacheHeader::set_mode_volatile(&mut b, mode, &config) };
        assert_eq!((a.mode, a.compact_mask), (b.mode, b.compact_mask));
        let want = match mode {
            MODE_PROBE => PROBE_MASK,
            MODE_COMPACT => config.compact_mask(),
            _ => u32::MAX,
        };
        assert_eq!(a.compact_mask, want, "mode {mode}");
    }
}

/// Item 30 of the 2026-09-18 review: the pages `Shared::alloc_huge` adds
/// to the arena are the ones the run's descriptor records
/// (`Huge::alloc_run`), which is what `free_huge` subtracts, so the
/// arena's `(runs, pages)` returns to zero and `stats.rs` subtracts the
/// guards exactly.
#[test]
fn huge_run_pages_are_counted_from_the_descriptor() {
    use crate::huge::GUARD_PAGES;
    let s = shared();
    let t = TestHeap::new(s);
    let page_of = |p: *mut u8| match s.regions.classify(p as usize) {
        Kind::Huge { page } => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(s.arena0.large(), (0, 0));
    let size = 300 * 1024;
    let (p, _) = t.alloc_huge(size, 16);
    let info = s.huge.lookup(page_of(p)).unwrap();
    assert_eq!(info.pages, s.huge.usable_pages(size).unwrap() + GUARD_PAGES);
    assert_eq!(s.arena0.large(), (1, info.pages));
    // An aligned run: the hole alignment leaves in front of it is a free
    // extent, not the run's pages.
    let (q, _) = t.alloc_huge(size, 1 << 20);
    let qinfo = s.huge.lookup(page_of(q)).unwrap();
    assert_eq!(qinfo.pages, info.pages);
    assert_eq!(s.arena0.large(), (2, 2 * info.pages));
    // `alloc_run` reports the descriptor's pages, and the free the same.
    let r = s.huge.alloc_run(&s.meta, 5 * page_size() + 1, 1, 0);
    assert!(!r.ptr.is_null());
    assert_eq!(r.pages, 6 + GUARD_PAGES);
    assert_eq!(r.pages, s.huge.lookup(page_of(r.ptr)).unwrap().pages);
    assert_eq!(s.huge.free_run(&s.meta, page_of(r.ptr)).pages, r.pages);
    t.free(p);
    assert_eq!(s.arena0.large(), (1, info.pages));
    t.free(q);
    assert_eq!(s.arena0.large(), (0, 0));
    assert_eq!(s.huge.alloc_run(&s.meta, usize::MAX, 1, 0), crate::huge::Placed::NONE);
    t.destroy();
}

/// Item 16 of the 2026-09-18 review: an owner word naming an arena that
/// was never built is corruption. The cross-arena free aborts by name
/// (`Shared::owner` does not build arenas) instead of quietly building
/// the automatic arena and caching the slot for it.
#[test]
#[should_panic(expected = "arena 5 does not exist")]
fn a_free_routed_to_an_unbuilt_arena_aborts() {
    let s = shared_with(Config { narenas: 8, pac_pointers: true, ..Config::DEFAULT });
    let t = TestHeap::new(s);
    let h = t.heap();
    assert_eq!(h.arena(), 0);
    let c = class_with(64);
    let p = t.alloc(c);
    let Kind::Slab { slab, .. } = s.regions.classify(p as usize) else { panic!("not a slab slot") };
    let table = s.table(c);
    assert!(s.arenas.get_existing(5).is_none());
    table.corrupt_owner_word(&s.meta, slab as usize / table.group(), 5);
    assert_eq!(s.classify_free(p), Ok((c, 5)));
    h.free(s, p);
}

/// A heap homed on an automatic arena nothing has built yet
/// (`Arenas::assign` does not build) may start with a huge allocation:
/// `Shared::alloc_huge` builds the arena, and the run's free finds it as
/// the owner.
#[test]
fn a_huge_first_allocation_builds_the_home_arena() {
    let s = shared_with(Config { narenas: 4, pac_pointers: true, ..Config::DEFAULT });
    let hp = Heap::create_detached(s, 2);
    assert!(!hp.is_null());
    let h = unsafe { &mut *hp };
    assert!(s.arenas.get_existing(2).is_none());
    let (p, _) = h.alloc_huge(s, 300 * 1024, 16);
    assert!(!p.is_null());
    let a2 = s.arenas.get_existing(2).expect("built by the huge allocation");
    assert_eq!(a2.large().0, 1);
    assert_eq!(s.huge.lookup(0).unwrap().arena, 2);
    h.free(s, p);
    assert_eq!(a2.large(), (0, 0));
    unsafe { Heap::destroy(hp, s) };
}

#[test]
#[should_panic(expected = "arena 77 does not exist")]
fn a_huge_allocation_in_an_arena_that_does_not_exist_aborts() {
    shared().alloc_huge(300 * 1024, 16, 77);
}

/// `pop_fast` is the one body behind `alloc_fast`, `alloc_zeroed` and
/// the per-CPU off-mode arm: same slot, same zero flag as the cache
/// header says, `None` from an empty cache.
#[test]
fn pop_fast_pops_the_top_with_the_zero_flag() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    let h = t.heap();
    assert_eq!(h.pop_fast(c, flags), None, "no cache yet");
    assert_eq!(h.alloc_fast(c, flags), None);
    let first = t.alloc(c);
    // The refill batch is a fresh slab: the zero prefix covers it all.
    let len = t.len(c);
    let top = t.cached_plain(c)[len - 1];
    assert_eq!(h.pop_fast(c, flags), Some((top, true)));
    assert_eq!(t.len(c), len - 1);
    let top = t.cached_plain(c)[len - 2];
    assert_eq!(h.alloc_fast(c, flags), Some(top));
    // A slot pushed back is no longer known zero (the push clips the
    // prefix), and the flag says so.
    t.free(first);
    assert_eq!(h.pop_fast(c, flags), Some((first, false)));
    assert_eq!(t.alloc_zeroed(c).1, true, "below the pushed entry the prefix still holds");
    t.flush_all();
    assert_eq!(h.pop_fast(c, flags), None, "flushed");
    t.destroy();
}

/// `checked_slot_any` is `checked_slot`'s bit test without the arena
/// test: they agree on every pointer of the home arena and on anything
/// that is not a live slot, and differ only for a live slot of another
/// arena, which the per-CPU push takes and the thread push declines.
#[test]
fn checked_slot_any_is_checked_slot_without_the_arena_test() {
    let s = shared();
    let a1 = s.arenas.create(s).unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    let c = class_with(64);
    let p0 = t.alloc(c);
    let (p1, _) = h.alloc_in(s, a1, c);
    assert!(s.multi_arena());
    assert_eq!((s.checked_slot(p0, h), s.checked_slot_any(p0)), (Some(c), Some(c)));
    assert_eq!((s.checked_slot(p1, h), s.checked_slot_any(p1)), (None, Some(c)));
    // Not a slot start, a never-claimed slot, a pointer outside every region.
    let mid = unsafe { p0.add(8) };
    assert_eq!((s.checked_slot(mid, h), s.checked_slot_any(mid)), (None, None));
    let unclaimed = unsafe { p0.add((HALF + 200) * 64) };
    assert_eq!((s.checked_slot(unclaimed, h), s.checked_slot_any(unclaimed)), (None, None));
    let outside = 0x10usize as *mut u8;
    assert_eq!((s.checked_slot(outside, h), s.checked_slot_any(outside)), (None, None));
    // A freed slot's bit clears at the flush; until then it is cached and set.
    h.free(s, p0);
    h.free(s, p1);
    assert_eq!(s.checked_slot_any(p0), Some(c));
    h.flush_all(s);
    assert_eq!((s.checked_slot(p0, h), s.checked_slot_any(p0), s.checked_slot_any(p1)), (None, None, None));
}
