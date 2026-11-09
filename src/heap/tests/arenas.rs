//! Arenas: the registry, failed builds, automatic assignment and
//! rehoming, explicit arenas and caches, and the foreign-free caches.

use super::*;

/// `free_flags` is `push_flags` until a second arena exists, then
/// `push_flags | FREE_ROUTE`; `push_flags`, which the sized path and
/// `malloc` read, never carries the routing bit.
#[test]
fn free_flags_gain_the_route_bit_with_a_second_arena() {
    let s = shared();
    assert_eq!(s.free_flags(), s.push_flags());
    assert!(!s.multi_arena());
    let _ = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    assert!(s.multi_arena());
    assert_eq!(s.free_flags(), s.push_flags() | FREE_ROUTE);
    assert_eq!(s.push_flags() & FREE_ROUTE, 0);
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
    assert!(core::ptr::eq(
        s.arena(0, Tracked::assume_new()).unwrap().0,
        &s.arena0
    ));
    assert!(s.arena(1, Tracked::assume_new()).is_none());
    assert!(s.arena(4095, Tracked::assume_new()).is_none());
    assert!(s.arenas.get_existing(1).is_none());
    for _ in 0..8 {
        assert_eq!(s.arenas.assign(vstd::prelude::Tracked::assume_new()).0, 0);
    }
    let a1 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let a2 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    assert_eq!((a1, a2), (1, 2));
    assert_eq!(s.arenas.count(), 3);
    assert!(s.multi_arena());
    let a = s.arena(1, Tracked::assume_new()).unwrap().0;
    assert_eq!(a.index(), 1);
    assert!(s.regions.in_meta(a as *const Arena as usize));
    assert!(core::ptr::eq(a.class(3).table(), s.table(3)));
    assert_eq!(a.class(3).arena(), 1);
    assert_eq!(a.decay_ns(), s.decay_ns());
    assert!(s.arena(3, Tracked::assume_new()).is_none());
    let seen: Vec<u16> = s.arenas.iter().map(|a| a.index()).collect();
    assert_eq!(seen, [0, 1, 2]);
    // Still assigning 0: created arenas are explicit, not automatic.
    assert_eq!(s.arenas.assign(vstd::prelude::Tracked::assume_new()).0, 0);
    a.set_decay_ms(-1, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(a.decay_ns(), crate::purge::NEVER);
    assert_ne!(s.decay_ns(), crate::purge::NEVER);
    s.set_decay_ms(5, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(a.decay_ns(), 5_000_000);
    assert_eq!(s.decay_ns(), 5_000_000);
}

/// The tests that arm [`crate::arena::FAIL_INIT`] take turns on this
/// guard. It holds one metadata address, so without it one test's store
/// replaces another's and that test's build does not fail.
static FAIL_INIT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`FAIL_INIT_SERIAL`]; a panic in one holder must not fail the rest.
fn fail_init_serial() -> std::sync::MutexGuard<'static, ()> {
    FAIL_INIT_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A build whose `Arena::init` fails keeps its storage for the next
/// build: the reservation is never freed, so every retry under metadata
/// pressure (each slow path of a heap homed on an unbuilt arena, each
/// `arenas.create`) would otherwise strand another `Arena`.
#[test]
fn failed_arena_build_leaves_its_storage_to_the_next() {
    use core::sync::atomic::Ordering::Relaxed;
    let _serial = fail_init_serial();
    let s = shared();
    // On the reservation side the arena's storage is the lowest
    // reservation right after its build, so `top` names it.
    s.meta.set_eager_for_test(false);
    crate::arena::FAIL_INIT.store(&s.meta as *const _ as usize, Relaxed);
    assert!(
        s.arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_none()
    );
    assert_eq!(crate::arena::FAIL_INIT.load(Relaxed), 0, "init never ran");
    let stranded = s.meta.top();
    assert_eq!(s.arenas.count(), 1);
    let i = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    assert_eq!(i, 1);
    assert_eq!(
        s.arena(i, Tracked::assume_new()).unwrap().0 as *const Arena as usize,
        stranded,
        "the build after a failed one reuses its storage"
    );
}

/// An `Arena::init` that fails because the region cannot hold the slab
/// set tables returns the `SetTable` object it allocated for them.
#[test]
fn failed_arena_init_frees_its_set_table_object() {
    let s = shared();
    s.meta.set_eager_for_test(false);
    let st = s.meta.stats(vstd::prelude::Tracked::assume_new());
    if st.reserved != 0 {
        // Init put tables on the reservation side (no eager tables on
        // this host), so `writable` no longer locates the object side's
        // frontier.
        std::eprintln!(
            "SKIP failed_arena_init_frees_its_set_table_object: eager tables off on this host"
        );
        return;
    }
    // Leave room for the arena's storage and one page: not enough for
    // the set tables (two groups or more, a page each at least).
    let arena = align_up(core::mem::size_of::<Arena>(), page_size());
    let gap = s.meta.top() - (s.meta.base() + st.writable);
    s.meta
        .reserve(
            gap - arena - page_size(),
            page_size(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let allocated = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
    assert!(
        s.arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_none()
    );
    assert_eq!(
        s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated,
        allocated
    );
}

/// `narenas:3`: a new heap is bound to the automatic arena with the
/// fewest bound heaps (ties: a built one first, then the lowest index),
/// `destroy` releases the binding, and arenas are built on first touch.
#[test]
fn automatic_arenas_are_assigned_least_loaded_and_built_lazily() {
    let s = TestShared::with(Config {
        narenas: 3,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.arenas.count(), 3);
    assert_eq!(s.arenas.auto(), 3);
    assert!(s.arenas.get_existing(1).is_none());
    // Six live heaps: two per arena, lowest index first.
    let mut heaps: Vec<TestHeap> = (0..6).map(|_| TestHeap::new(s)).collect();
    let homes: Vec<u16> = heaps.iter().map(|t| t.heap().arena()).collect();
    assert_eq!(homes, [0, 1, 2, 0, 1, 2]);
    assert_eq!(
        (0..3).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(),
        [2, 2, 2]
    );
    assert!(
        s.arenas.get_existing(1).is_none(),
        "assignment alone must not build"
    );
    assert!(!s.multi_arena());
    // Destroying a heap frees its binding; the next heap takes the
    // vacated arena.
    heaps.remove(1).destroy();
    assert_eq!(s.arenas.bound(1), 1);
    let t = TestHeap::new(s);
    assert_eq!(t.heap().arena(), 1);
    assert_eq!(s.arenas.bound(1), 2);
    // Explicit arenas and explicit caches take no part in it.
    assert!(
        s.arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .is_some_and(|i| i == 3)
    );
    assert_eq!(s.arenas.bound(3), 0);
    let before = s.arenas.bound(0);
    let tc = s
        .tcaches
        .create(s, 0, Tracked::assume_new(), Tracked::assume_new())
        .unwrap();
    assert_eq!(s.arenas.bound(0), before);
    assert!(
        s.tcaches
            .destroy(s, tc, Tracked::assume_new(), Tracked::assume_new())
    );
    assert_eq!(s.arenas.bound(0), before);
    // First touch builds arena 1.
    let c = class_with(64);
    let p = t.alloc(c);
    assert!(s.arenas.get_existing(1).is_some());
    assert!(s.multi_arena());
    assert_eq!(s.arena0.classes[c].desc(0).arena(), 1);
    assert_eq!(s.classify_free(p, Tracked::assume_new()), Ok((c, 1)));
    t.heap().free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(t.heap().cache(c).unwrap().len(), HALF);
    // `thread.arena` moves the binding with the heap.
    t.heap().set_arena(
        s,
        3,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 1));
    t.destroy();
    assert_eq!((s.arenas.bound(1), s.arenas.bound(3)), (1, 0));
    for h in heaps {
        h.destroy();
    }
    assert_eq!(
        (0..4).map(|i| s.arenas.bound(i)).collect::<Vec<_>>(),
        [0, 0, 0, 0]
    );
}

/// Thread churn under a multi-arena default: a lone heap stays on
/// arena 0 and never sets `multi_arena` (a single-threaded program
/// never pays the `free` routing); a second heap that comes and goes
/// lands on arena 1 every time, so only the arenas that were ever
/// alive at once get built; a built idle arena beats an unbuilt one.
#[test]
fn heap_churn_reuses_the_vacated_arena() {
    let s = TestShared::with(Config {
        narenas: 8,
        ..Config::DEFAULT
    })
    .build();
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
    assert!(
        (2..8).all(|i| s.arenas.get_existing(i).is_none()),
        "only arenas 0 and 1 were ever alive at once"
    );
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

/// A successor thread (larson's shape): started while the thread whose
/// data it takes over still holds its arena, so homed elsewhere; once
/// that arena is idle, the second free of the inherited data that takes
/// the `FOREIGN` slow path (the first of another class) moves its home
/// there (`Heap::try_rehome`): the home and `FOREIGN` caches are flushed
/// through the ordinary flush, the headers keep their stacks, the binding
/// moves, and the freed slot takes the home push. A free that takes the
/// `FOREIGN` fast push is not looked at. At most once per heap.
#[test]
fn a_young_heap_moves_onto_the_idle_arena_it_frees_into() {
    let s = TestShared::with(Config {
        narenas: 8,
        ..Config::DEFAULT
    })
    .build();
    let main = TestHeap::new(s);
    let c = class_with(64);
    let w1 = TestHeap::new(s);
    assert_eq!(w1.heap().arena(), 1);
    let inherited: Vec<*mut u8> = (0..8).map(|_| w1.alloc(c)).collect();
    let c2 = class_with(128);
    let x = w1.alloc(c2);
    // Started before w1 exits: arena 1 is held, so w2 lands on 2.
    let w2 = TestHeap::new(s);
    assert_eq!(w2.heap().arena(), 2);
    let own = w2.alloc(c);
    assert_eq!(s.classify_free(own, Tracked::assume_new()), Ok((c, 2)));
    // A foreign free while arena 1 is held: onto FOREIGN, no move.
    w2.free(inherited[0]);
    assert_eq!(w2.heap().arena(), 2);
    assert_eq!(w2.heap().cache_in(FOREIGN, c).unwrap().len(), 1);
    assert!(w2.heap().rehome);
    w1.destroy();
    assert_eq!(s.arenas.bound(1), 0);
    let home_len = w2.cache(c).len();
    assert!(home_len > 0);
    // Arena 1 is idle now, but a FOREIGN fast push is not looked at.
    w2.free(inherited[1]);
    assert_eq!(w2.heap().arena(), 2);
    assert_eq!(w2.heap().cache_in(FOREIGN, c).unwrap().len(), 2);
    // The first free of another class takes the slow path: w2 moves.
    w2.free(x);
    assert_eq!(w2.heap().arena(), 1);
    assert_eq!((s.arenas.bound(1), s.arenas.bound(2)), (1, 0));
    assert!(!w2.heap().rehome);
    // Both caches were flushed (arena 2's refill went back to its slab);
    // the slot that triggered the move is the one home entry.
    assert_eq!(w2.heap().cache_in(FOREIGN, c).unwrap().len(), 0);
    assert_eq!(w2.cached_plain(c2), [x]);
    assert_eq!(w2.cache(c).len(), 0);
    assert_eq!(
        s.arena(2, Tracked::assume_new()).unwrap().0.classes[c].curregs(),
        1,
        "only `own`"
    );
    // The rest of the inherited data takes the fast path now.
    for &p in &inherited[2..] {
        assert!(free_fast(s, w2.heap(), p, s.free_flags()));
    }
    assert_eq!(w2.cache(c).len(), inherited.len() - 2);
    // A pop serves an inherited slot.
    assert_eq!(
        s.classify_free(w2.alloc(c), Tracked::assume_new()),
        Ok((c, 1))
    );
    // One move per heap: arena 2 is idle, but `own` goes onto FOREIGN.
    w2.free(own);
    assert_eq!(w2.heap().arena(), 1);
    assert_eq!(w2.heap().cache_in(FOREIGN, c).unwrap().len(), 1);
    // Byte counters survived the move.
    let st = w2.stats();
    assert_eq!(st.allocated, 2 * 64);
    assert_eq!(st.deallocated, 9 * 64 + 128);
    w2.destroy();
    main.destroy();
    assert!((0..8).all(|i| s.arenas.bound(i) == 0));
}

/// The move never stacks a second heap on an arena, never leaves a home
/// `thread.arena` chose, and never targets an explicit arena.
#[test]
fn rehoming_only_takes_an_idle_automatic_arena() {
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    let main = TestHeap::new(s);
    let c = class_with(64);
    // Held arena: no move, the window counts down.
    let w1 = TestHeap::new(s);
    let p = w1.alloc(c);
    let w2 = TestHeap::new(s);
    w2.free(p);
    assert_eq!(w2.heap().arena(), 2);
    // Two candidates for one idle arena: the first wins, the second
    // finds it bound again (each free is the first of its class, so each
    // takes the FOREIGN slow path and is looked at).
    let c2 = class_with(128);
    let q1 = w1.alloc(c2);
    let q2 = w1.alloc(c);
    let q3 = w1.alloc(c2);
    let w3 = TestHeap::new(s);
    assert_eq!(w3.heap().arena(), 3);
    w3.free(q2);
    w1.destroy();
    w2.free(q1);
    w3.free(q3);
    assert_eq!((w2.heap().arena(), w3.heap().arena()), (1, 3));
    assert_eq!(s.arenas.bound(1), 1);
    assert!(w3.heap().rehome);
    // `thread.arena` pins the home.
    w3.heap().set_arena(
        s,
        0,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!w3.heap().rehome);
    let r = w2.alloc(c);
    w2.destroy();
    assert_eq!(s.arenas.bound(1), 0);
    w3.free(r);
    assert_eq!(w3.heap().arena(), 0);
    assert_eq!(s.arenas.bound(1), 0);
    // An explicit arena is never a target.
    let x = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    assert!(x as u32 >= s.arenas.auto());
    let w4 = TestHeap::new(s);
    let (e1, _) = w4.heap().alloc_in(
        s,
        x,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let (e2, _) = w4.heap().alloc_in(
        s,
        x,
        c2,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!e1.is_null() && !e2.is_null());
    let w5 = TestHeap::new(s);
    let home = w5.heap().arena();
    w5.free(e1);
    w5.free(e2);
    assert_eq!(w5.heap().arena(), home);
    assert_eq!(s.arenas.bound(x), 0);
    w5.destroy();
    w4.destroy();
    w3.destroy();
    main.destroy();
}

/// The move needs two cross-arena frees in a row into the same arena: a
/// young heap freeing slots of several idle arenas in turn (mstress's
/// transfer slots) stays where it is until two in a row come from one.
#[test]
fn rehoming_needs_two_frees_in_a_row_into_one_arena() {
    let s = TestShared::with(Config {
        narenas: 8,
        ..Config::DEFAULT
    })
    .build();
    let main = TestHeap::new(s);
    let w1 = TestHeap::new(s);
    let w2 = TestHeap::new(s);
    // One class per free, so each takes the FOREIGN slow path.
    let a: Vec<*mut u8> = [16, 48].iter().map(|&n| w1.alloc(class_with(n))).collect();
    let b: Vec<*mut u8> = [32, 64, 80]
        .iter()
        .map(|&n| w2.alloc(class_with(n)))
        .collect();
    let w3 = TestHeap::new(s);
    assert_eq!(w3.heap().arena(), 3);
    w1.destroy();
    w2.destroy();
    for (&x, &y) in a.iter().zip(&b[..2]) {
        w3.free(x);
        w3.free(y);
    }
    assert_eq!(w3.heap().arena(), 3);
    assert!(w3.heap().rehome);
    w3.free(b[2]);
    assert_eq!(w3.heap().arena(), 2);
    assert_eq!((s.arenas.bound(2), s.arenas.bound(3)), (1, 0));
    w3.destroy();
    main.destroy();
}

/// A heap that has made [`REHOME_TICKS`] slow-path entries has data of
/// its own at home: it stays there even when the arena it frees into is
/// idle (mstress's workers), and the first such free closes the window.
#[test]
fn a_heap_past_the_young_limit_never_moves() {
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    let main = TestHeap::new(s);
    let w1 = TestHeap::new(s);
    let p = w1.alloc(class_with(64));
    let w2 = TestHeap::new(s);
    // One refill per class: one slow-path entry each.
    let own: Vec<*mut u8> = (0..REHOME_TICKS as usize)
        .map(|i| w2.alloc(class_with(16 * (i + 1))))
        .collect();
    assert!(w2.heap().slow_ticks() >= REHOME_TICKS);
    w1.destroy();
    assert_eq!(s.arenas.bound(1), 0);
    w2.free(p);
    assert_eq!(w2.heap().arena(), 2);
    assert_eq!(s.arenas.bound(1), 0);
    assert!(!w2.heap().rehome);
    for q in own {
        w2.free(q);
    }
    w2.destroy();
    main.destroy();
}

/// A slot that went onto `FOREIGN` before the move is back in its bitmap
/// after it, so freeing it again aborts as any stale double free does:
/// nothing stays cached across the move.
#[test]
#[should_panic(expected = "invalid or double free")]
fn a_double_free_across_the_move_aborts() {
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    let _main = TestHeap::new(s);
    let c = class_with(64);
    let w1 = TestHeap::new(s);
    let (p, q) = (w1.alloc(c), w1.alloc(class_with(128)));
    let w2 = TestHeap::new(s);
    w2.free(p);
    w1.destroy();
    w2.free(q);
    assert_eq!(w2.heap().arena(), 1);
    w2.free(p);
}

/// The double free itself can be the free that moves the heap: with
/// `double_free_scan` the second free of a slot on the `FOREIGN` cache
/// takes the slow path (every push declines), which is where a young heap
/// would move; under the key it does not move, and the state check finds
/// the first copy cached and aborts by name.
#[test]
#[should_panic(expected = "the slot is already in a thread cache")]
fn a_double_free_that_would_move_the_heap_aborts_with_the_state() {
    let s = TestShared::with(Config {
        narenas: 4,
        double_free_scan: true,
        ..Config::DEFAULT
    })
    .build();
    let _main = TestHeap::new(s);
    let c = class_with(64);
    let w1 = TestHeap::new(s);
    let p = w1.alloc(c);
    let w2 = TestHeap::new(s);
    w1.destroy();
    assert_eq!(s.arenas.bound(1), 0);
    w2.free(p);
    assert_eq!(w2.heap().arena(), 2);
    assert!(w2.heap().rehome);
    w2.free(p);
}

/// The same in the default configuration: a full `FOREIGN` cache sends
/// the second free of a slot it holds to the slow path, and that free
/// moves the heap, whose flush releases the first copy. The bit test runs
/// again after the move, so the second copy aborts instead of taking the
/// home push (where `malloc` would pop it while a refill could hand the
/// released slot out again).
#[test]
#[should_panic(expected = "invalid or double free")]
fn a_double_free_that_moves_the_heap_aborts_at_the_bit_test() {
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    let _main = TestHeap::new(s);
    let c = class_with(64);
    let w1 = TestHeap::new(s);
    let slots: Vec<*mut u8> = (0..4096).map(|_| w1.alloc(c)).collect();
    let w2 = TestHeap::new(s);
    w1.destroy();
    let p = slots[0];
    w2.free(p);
    let cap = w2.heap().cache_in(FOREIGN, c).unwrap().cap();
    assert!(cap < slots.len());
    for &q in &slots[1..cap] {
        w2.free(q);
    }
    assert_eq!(w2.heap().cache_in(FOREIGN, c).unwrap().len(), cap);
    assert_eq!(w2.heap().arena(), 2);
    w2.free(p);
}

/// Explicit allocations from another arena come from a cache on the
/// `others` list keyed by that arena; a plain `free` of another arena's
/// slot goes onto the one `FOREIGN` cache for its class (the fast path
/// declines it), which nothing pops from and whose flush routes each slot
/// to the arena its slab belongs to, so per-arena counts stay exact
/// (design 6.3 step 4, 13.3), rather than one cache per arena a thread
/// frees into.
#[test]
// `forget` marks `t` consumed: `Heap::destroy` freed its heap.
#[allow(clippy::forget_non_drop)]
fn foreign_frees_share_one_flush_only_cache_per_class() {
    let s = shared();
    let a1 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let a2 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let t = TestHeap::new(s);
    assert_eq!(t.heap().arena(), 0);
    let c = class_with(64);
    let p0 = t.alloc(c);
    let h = t.heap();
    let (p1, zero) = h.alloc_in(
        s,
        a1,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p1.is_null() && zero);
    let (p2, _) = h.alloc_in(
        s,
        a2,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.classify_free(p0, Tracked::assume_new()), Ok((c, 0)));
    assert_eq!(s.classify_free(p1, Tracked::assume_new()), Ok((c, a1)));
    assert_eq!(s.classify_free(p2, Tracked::assume_new()), Ok((c, a2)));
    assert_ne!(
        s.arena0.classes[c].locate(p0).0,
        s.arena0.classes[c].locate(p1).0
    );
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(a2, c).unwrap().len(), HALF - 1);
    // One refill spans `HALF / nslots` slabs (one 1024-slot slab at 64 bytes).
    let per_refill = HALF.div_ceil(CLASSES[c].nslots());
    assert_eq!(
        s.arena(a1, Tracked::assume_new()).unwrap().0.classes[c]
            .stats()
            .committed,
        per_refill
    );
    assert_eq!(s.arena0.classes[c].stats().committed, per_refill);
    // Non-existent arena: null, nothing touched.
    assert!(
        h.alloc_in(
            s,
            77,
            c,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new()
        )
        .0
        .is_null()
    );
    // The fast path takes only the home arena's slots; the others go onto
    // the one FOREIGN cache, whatever their arena, never onto the arena's
    // own node.
    assert!(free_fast(s, h, p0, s.free_flags()));
    assert!(!free_fast(s, h, p1, s.free_flags()));
    assert!(!free_fast(s, h, p2, s.free_flags()));
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert!(h.cache_in(FOREIGN, c).is_none());
    h.free(
        s,
        p1,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        p2,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 2);
    assert_eq!(
        h.cache_in(FOREIGN, c).unwrap().cap(),
        HALF,
        "half the ladder"
    );
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(a2, c).unwrap().len(), HALF - 1);
    assert!(!h.cache_in(FOREIGN, c).unwrap().compacting());
    // Nothing pops from it: a default allocation is the home's LIFO top,
    // an explicit one the arena node's.
    assert_eq!(t.alloc(c), p0);
    let h = t.heap();
    let (p3, _) = h.alloc_in(
        s,
        a1,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_ne!(p3, p1);
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 2);
    // Byte counters cover every cache.
    let st = h.stats();
    assert_eq!(st.allocated, 5 * 64);
    assert_eq!(st.deallocated, 3 * 64);
    // Flushing returns everything to the right owner: every slot of the
    // three arenas is back in its bitmap and the refills' slabs are Empty.
    h.free(
        s,
        p0,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        p3,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 3);
    h.flush_all(s, Tracked::assume_new());
    assert_eq!(h.cache(c).unwrap().len(), 0);
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 0);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), 0);
    assert_eq!(h.cache_in(a2, c).unwrap().len(), 0);
    for (a, cs) in [
        (0, &s.arena0.classes[c]),
        (
            a1,
            &s.arena(a1, Tracked::assume_new()).unwrap().0.classes[c],
        ),
        (
            a2,
            &s.arena(a2, Tracked::assume_new()).unwrap().0.classes[c],
        ),
    ] {
        assert_eq!(cs.stats().empty, per_refill, "arena {a}");
        assert_eq!(cs.curregs(), 0, "arena {a}");
    }
    // Direct paths against an explicit arena.
    let a = s.arena(a1, Tracked::assume_new()).unwrap().0;
    let (d, _, _) = s.alloc_direct(
        a,
        Tracked::assume_new(),
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.classify_free(d, Tracked::assume_new()), Ok((c, a1)));
    assert_eq!(a.classes[c].desc(s.arena0.classes[c].locate(d).0).live(), 1);
    s.free_direct(a1, c, d, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(a.classes[c].stats().empty, per_refill);
    // Destroying the heap frees the nodes too.
    let before = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
    // SAFETY: `t.h` came from `Heap::create` on `s`; `t` is forgotten below,
    // so nothing uses the heap again.
    unsafe { Heap::destroy(t.h, s, Tracked::assume_new(), Tracked::assume_new()) };
    assert!(s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated < before);
    core::mem::forget(t);
}

/// The last thread heap homed on an arena leaves: `Heap::destroy` purges
/// the arena's `Empty` slabs at once (jemalloc's `tcache_destroy` rule,
/// design 13.3), most of them the caches it just flushed, at most once
/// per decay period of the arena; an arena that still has a heap is left
/// to the decay.
#[test]
fn destroying_the_last_heap_of_an_arena_purges_it() {
    let s = TestShared::with(Config {
        narenas: 2,
        ..Config::DEFAULT
    })
    .build();
    let a = TestHeap::new(s);
    let b = TestHeap::new(s);
    let b2 = TestHeap::new(s);
    assert_eq!(
        (a.heap().arena(), b.heap().arena(), b2.heap().arena()),
        (0, 1, 0)
    );
    let c = class_with(64);
    let pa = a.alloc(c);
    let pb = b.alloc(c);
    a.heap().free(
        s,
        pa,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    b.heap().free(
        s,
        pb,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let per_refill = HALF.div_ceil(CLASSES[c].nslots());
    // Arena 1 loses its only heap: flushed and purged.
    b.destroy();
    let s1 = s.arena(1, Tracked::assume_new()).unwrap().0.classes[c].stats();
    assert_eq!((s1.empty, s1.purged), (0, per_refill));
    // A second last-heap exit on arena 1 within the decay period does
    // not purge again (thread churn); after the period, or with the
    // stamp cleared, it does; with decay off, never.
    let b3 = TestHeap::new(s);
    assert_eq!(b3.heap().arena(), 1);
    let pb3 = b3.alloc(c);
    b3.heap().free(
        s,
        pb3,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    b3.destroy();
    let s1 = s.arena(1, Tracked::assume_new()).unwrap().0.classes[c].stats();
    assert_eq!(
        s1.empty, per_refill,
        "within the period: flushed, not purged"
    );
    let a1 = s.arena(1, Tracked::assume_new()).unwrap();
    assert!(!a1.0.take_exit_purge(now_ns()));
    assert!(a1.0.take_exit_purge(now_ns() + a1.0.decay_ns()));
    a1.0.set_decay_ms(-1, Tracked::assume_new(), Tracked::assume_new());
    assert!(!a1.0.take_exit_purge(now_ns() + 2 * 10_000_000_000));
    a1.0.set_decay_ms(10_000, Tracked::assume_new(), Tracked::assume_new());
    // Arena 0 keeps `b2`: flushed, still `Empty`.
    a.destroy();
    let s0 = s.arena0.classes[c].stats();
    assert_eq!((s0.empty, s0.purged), (per_refill, 0));
    b2.destroy();
    let s0 = s.arena0.classes[c].stats();
    assert_eq!((s0.empty, s0.purged), (0, per_refill));
}

/// A shared class (above `tcache_max`) comes from arena 0 on the
/// default path whatever the home arena, but `MALLOCX_ARENA(a)`
/// keeps `a`, home or not (design 13.3), and a heap creates no
/// `others` node for a class it never caches there (`Config::cap_for`).
#[test]
fn explicit_arena_keeps_a_shared_class_in_its_arena() {
    let s = shared();
    let a1 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    let c = class_with(64 * 1024);
    assert!(s.config.shared_class(c));
    assert_eq!(s.config.cap_for(0, 0, c), 0);
    assert_eq!(s.config.cap_for(0, a1, c), 0);
    h.set_arena(
        s,
        a1,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    // Default path: arena 0.
    let (d, _) = h.alloc_zeroed(
        s,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.classify_free(d, Tracked::assume_new()), Ok((c, 0)));
    // The home arena named explicitly: stays home.
    let (p, _) = h.alloc_in(
        s,
        a1,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.classify_free(p, Tracked::assume_new()), Ok((c, a1)));
    assert_eq!(
        s.arena(a1, Tracked::assume_new()).unwrap().0.classes[c]
            .stats()
            .committed,
        1
    );
    // A foreign arena named explicitly, through a heap that caches
    // nothing of that class there: no node, no cache, right arena.
    let before = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
    let (q, _) = h.alloc_in(
        s,
        0,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.classify_free(q, Tracked::assume_new()), Ok((c, 0)));
    assert_eq!(
        s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated,
        before,
        "no others node for an uncached class"
    );
    assert!(h.cache_in(0, c).is_none());
    assert!(h.cache(c).is_none());
    let st = h.stats();
    assert_eq!(st.allocated, 3 * 64 * 1024);
    h.free(
        s,
        d,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        q,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(
        s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated,
        before
    );
    assert_eq!(h.stats().deallocated, 3 * 64 * 1024);
    assert_eq!(
        s.arena(a1, Tracked::assume_new()).unwrap().0.classes[c]
            .stats()
            .empty,
        1
    );
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
    let c = class_with(64);
    let p = t.alloc(c);
    let q = t.alloc(c);
    let h = t.heap();
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let before = h.stats();
    assert_eq!((before.allocated, before.deallocated), (128, 64));
    let meta_before = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
    h.drop_home_caches(s, Tracked::assume_new());
    assert!(h.cache(c).is_none());
    assert!(
        s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated < meta_before,
        "the stack was freed"
    );
    let after = h.stats();
    assert_eq!(
        (after.allocated, after.deallocated),
        (before.allocated, before.deallocated)
    );
    h.free(
        s,
        q,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let again = h.stats();
    assert_eq!((again.allocated, again.deallocated), (128, 128));
    assert!(h.cache(c).is_some(), "the slow path created a new cache");
}

/// `thread.arena`: the home arena changes without flushing; the old
/// home's caches move to the list and back.
#[test]
fn set_arena_swaps_cache_arrays() {
    let s = shared();
    let a1 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p0 = t.alloc(c);
    let h = t.heap();
    let (p1, _) = h.alloc_in(
        s,
        a1,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.set_arena(
        s,
        a1,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.arena(), a1);
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(0, c).unwrap().len(), HALF - 1);
    // Plain allocation now comes from arena 1's cache; a plain free of an
    // arena-0 slot is foreign now and goes onto the FOREIGN cache, not
    // onto arena 0's node.
    let q = t.alloc(c);
    let h = t.heap();
    assert_eq!(s.classify_free(q, Tracked::assume_new()), Ok((c, a1)));
    assert!(free_fast(s, h, p1, s.free_flags()));
    assert!(!free_fast(s, h, p0, s.free_flags()));
    h.free(
        s,
        p0,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        q,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert_eq!(h.cache_in(0, c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 1);
    h.set_arena(
        s,
        0,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    assert_eq!(h.cache_in(a1, c).unwrap().len(), HALF);
    h.flush_all(s, Tracked::assume_new());
    let per_refill = HALF.div_ceil(CLASSES[c].nslots());
    assert_eq!(s.arena0.classes[c].stats().empty, per_refill);
    assert_eq!(
        s.arena(a1, Tracked::assume_new()).unwrap().0.classes[c]
            .stats()
            .empty,
        per_refill
    );
}

/// Explicit caches are heaps owned by no thread with their own ids.
#[test]
fn explicit_caches_create_flush_destroy() {
    let s = shared();
    assert!(s.tcaches.is_empty());
    let t0 = s
        .tcaches
        .create(s, 0, Tracked::assume_new(), Tracked::assume_new())
        .unwrap();
    let t1 = s
        .tcaches
        .create(s, 0, Tracked::assume_new(), Tracked::assume_new())
        .unwrap();
    assert_eq!((t0, t1), (0, 1));
    assert_eq!(s.tcaches.len(), 2);
    assert!(s.tcaches.get(2, Tracked::assume_new()).is_none());
    assert!(s.tcaches.get(u16::MAX, Tracked::assume_new()).is_none());
    let c = class_with(64);
    let h = s.tcaches.get(t0, Tracked::assume_new()).unwrap();
    let (p, _) = h.alloc_in(
        s,
        0,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p.is_null());
    assert_eq!(h.cache(c).unwrap().len(), HALF - 1);
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.cache(c).unwrap().len(), HALF);
    assert!(s.tcaches.flush(s, t0, Tracked::assume_new()));
    assert_eq!(
        s.tcaches
            .get(t0, Tracked::assume_new())
            .unwrap()
            .cache(c)
            .unwrap()
            .len(),
        0
    );
    assert!(!s.tcaches.flush(s, 9, Tracked::assume_new()));
    assert!(
        s.tcaches
            .destroy(s, t0, Tracked::assume_new(), Tracked::assume_new())
    );
    assert!(
        !s.tcaches
            .destroy(s, t0, Tracked::assume_new(), Tracked::assume_new())
    );
    assert!(s.tcaches.get(t0, Tracked::assume_new()).is_none());
    assert_eq!(s.tcaches.len(), 1);
    // The slot is reused.
    assert_eq!(
        s.tcaches
            .create(s, 0, Tracked::assume_new(), Tracked::assume_new()),
        Some(t0)
    );
}

/// The cold walks of the `others` list ([`Heap::nodes`]) see every node,
/// the most recently created first, and a walk may free the node it was
/// handed (`destroy` does).
#[test]
// `forget` marks `t` consumed: `Heap::destroy` freed its heap.
#[allow(clippy::forget_non_drop)]
fn others_nodes_iterate_newest_first_and_survive_freeing() {
    let s = shared();
    let a1 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let a2 = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let t = TestHeap::new(s);
    let h = t.heap();
    assert_eq!(Heap::nodes(h.others).count(), 0);
    let c = class_with(64);
    let (p1, _) = h.alloc_in(
        s,
        a1,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let (p2, _) = h.alloc_in(
        s,
        a2,
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p1.is_null() && !p2.is_null());
    let arenas: Vec<u16> = Heap::nodes(h.others)
        // SAFETY: `nodes` yields the heap's live nodes.
        .map(|o| unsafe { (*o).arena })
        .collect();
    assert_eq!(arenas, [a2, a1]);
    assert!(h.cache_in(a1, c).is_some() && h.cache_in(a2, c).is_some());
    assert!(
        h.cache_in(7, c).is_none(),
        "an arena never touched has no node"
    );
    assert_eq!(
        h.stats().allocated,
        2 * 64,
        "the byte counters fold every node"
    );
    // The plain frees of the two arenas' slots create the FOREIGN node,
    // the newest.
    h.free(
        s,
        p1,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.free(
        s,
        p2,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(h.stats().deallocated, 2 * 64);
    let arenas: Vec<u16> = Heap::nodes(h.others)
        // SAFETY: as above.
        .map(|o| unsafe { (*o).arena })
        .collect();
    assert_eq!(arenas, [FOREIGN, a2, a1]);
    assert_eq!(h.cache_in(FOREIGN, c).unwrap().len(), 2);
    // Turning the caches off walks the list, flushes and frees every
    // node's caches (the nodes stay) and folds their counters.
    h.set_tcache_enabled(
        s,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(Heap::nodes(h.others).count(), 3);
    assert!(
        h.cache_in(a1, c).is_none()
            && h.cache_in(a2, c).is_none()
            && h.cache_in(FOREIGN, c).is_none()
    );
    assert_eq!(
        h.stats(),
        ThreadStats {
            allocated: 2 * 64,
            deallocated: 2 * 64
        }
    );
    // `destroy` frees each node as the walk hands it over.
    let before = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
    // SAFETY: `t.h` came from `Heap::create` on `s`; `t` is forgotten below,
    // so nothing uses the heap again.
    unsafe { Heap::destroy(t.h, s, Tracked::assume_new(), Tracked::assume_new()) };
    assert!(s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated < before);
    core::mem::forget(t);
}

/// An owner word naming an arena that
/// was never built is corruption. The cross-arena free goes onto the
/// FOREIGN cache; its flush, which routes the slot by that word, aborts by
/// name (`Shared::owner` does not build arenas) instead of quietly
/// building the automatic arena and releasing into it.
#[test]
#[should_panic(expected = "arena 5 does not exist")]
fn a_free_routed_to_an_unbuilt_arena_aborts() {
    let s = TestShared::with(Config {
        narenas: 8,
        pac_pointers: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    assert_eq!(t.heap().arena(), 0);
    let c = class_with(64);
    let p = t.alloc(c);
    let h = t.heap();
    let Kind::Slab { slab, .. } = s.regions.classify(p as usize) else {
        panic!("not a slab slot")
    };
    let table = s.table(c);
    assert!(s.arenas.get_existing(5).is_none());
    table.corrupt_owner_word(&s.meta, slab as usize / table.group(), 5);
    assert_eq!(s.classify_free(p, Tracked::assume_new()), Ok((c, 5)));
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    h.flush_all(s, Tracked::assume_new());
}

/// A heap homed on an automatic arena nothing has built yet
/// (`Arenas::assign` does not build) may start with a huge allocation:
/// `Shared::alloc_huge` builds the arena, and the run's free finds it as
/// the owner.
#[test]
fn a_huge_first_allocation_builds_the_home_arena() {
    let s = TestShared::with(Config {
        narenas: 4,
        pac_pointers: true,
        ..Config::DEFAULT
    })
    .build();
    let (hp, _, _) = Heap::create_detached(s, 2, Tracked::assume_new());
    assert!(!hp.is_null());
    // SAFETY: `hp` is a live heap from `create_detached`, used only here.
    let h = unsafe { &mut *hp };
    assert!(s.arenas.get_existing(2).is_none());
    let (p, _, _) = h.alloc_huge(
        s,
        300 * 1024,
        16,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p.is_null());
    let a2 = s
        .arenas
        .get_existing(2)
        .expect("built by the huge allocation");
    assert_eq!(a2.large().0, 1);
    assert_eq!(
        s.huge
            .lookup(0, Tracked::assume_new(), Tracked::assume_new())
            .unwrap()
            .arena,
        2
    );
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(a2.large(), (0, 0));
    // SAFETY: `hp` came from `Heap::create_detached` on `s` (as for
    // `tcache.destroy`) and is not used again.
    unsafe { Heap::destroy(hp, s, Tracked::assume_new(), Tracked::assume_new()) };
}

/// A heap whose home arena cannot be built (the metadata region refuses
/// it: a resource failure, design 14.1) is served by arena 0, huge and
/// small, without an abort, and the next allocation after the pressure
/// lifts builds the home arena (nothing remembers the failure).
#[test]
fn an_unbuildable_home_arena_falls_back_to_arena_0_then_recovers() {
    use core::sync::atomic::Ordering::Relaxed;
    let _serial = fail_init_serial();
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    let (hp, _, _) = Heap::create_detached(s, 2, Tracked::assume_new());
    assert!(!hp.is_null());
    // SAFETY: `hp` is a live heap from `create_detached`, used only here.
    let h = unsafe { &mut *hp };
    let runs0 = s.arena0.large().0;
    crate::arena::FAIL_INIT.store(&s.meta as *const _ as usize, Relaxed);
    let (p, _, _) = h.alloc_huge(
        s,
        300 * 1024,
        16,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p.is_null(), "served by arena 0");
    assert_eq!(
        crate::arena::FAIL_INIT.load(Relaxed),
        0,
        "the build ran and failed"
    );
    assert!(s.arenas.get_existing(2).is_none());
    assert_eq!(s.arena0.large().0, runs0 + 1);
    h.free(
        s,
        p,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    // The pressure lifted: the next allocation builds arena 2.
    let (q, _, _) = h.alloc_huge(
        s,
        300 * 1024,
        16,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!q.is_null());
    assert!(s.arenas.get_existing(2).is_some(), "built on retry");
    h.free(
        s,
        q,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    // SAFETY: `hp` came from `Heap::create_detached` on `s` and is not
    // used again.
    unsafe { Heap::destroy(hp, s, Tracked::assume_new(), Tracked::assume_new()) };
}

/// The small-class slow path of a heap whose home arena cannot be built
/// serves the block uncached from arena 0, and the next slow path, with
/// the pressure gone, builds the home arena and serves from it.
#[test]
fn an_unbuildable_home_arena_serves_small_blocks_from_arena_0_then_recovers() {
    use core::sync::atomic::Ordering::Relaxed;
    let _serial = fail_init_serial();
    let s = TestShared::with(Config {
        narenas: 4,
        ..Config::DEFAULT
    })
    .build();
    // The main thread's heap holds arena 0, so the next lands on 1.
    let _main = TestHeap::new(s);
    let w = TestHeap::new(s);
    assert_eq!(w.heap().arena(), 1);
    assert!(
        s.arenas.get_existing(1).is_none(),
        "not built until the first refill"
    );
    let c = class_with(64);
    crate::arena::FAIL_INIT.store(&s.meta as *const _ as usize, Relaxed);
    let p = w.alloc(c);
    assert_eq!(
        crate::arena::FAIL_INIT.load(Relaxed),
        0,
        "the build ran and failed"
    );
    assert!(s.arenas.get_existing(1).is_none());
    assert_eq!(
        s.classify_free(p, Tracked::assume_new()),
        Ok((c, 0)),
        "from arena 0"
    );
    w.free(p);
    let q = w.alloc(c);
    assert!(s.arenas.get_existing(1).is_some(), "built on retry");
    assert_eq!(s.classify_free(q, Tracked::assume_new()), Ok((c, 1)));
    w.free(q);
}

/// The huge path on an index no arena exists for leaves the run to
/// arena 0 instead of aborting (the `mallocx` callers check an explicit
/// index first).
#[test]
fn a_huge_allocation_in_an_arena_that_does_not_exist_goes_to_arena_0() {
    let s = shared();
    let (p, _, _) = s.alloc_huge(
        300 * 1024,
        16,
        77,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p.is_null());
    let page = match s.regions.classify(p as usize) {
        Kind::Huge { page } => page,
        other => panic!("{other:?}"),
    };
    s.free_huge(page, Tracked::assume_new(), Tracked::assume_new());
}
