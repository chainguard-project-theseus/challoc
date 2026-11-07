use super::*;
use crate::heap::mallocx::{
    MALLOCX_TCACHE_NONE, MALLOCX_ZERO, Tcache, mallocx_arena, mallocx_lg_align, mallocx_tcache,
};
use crate::layout::{Kind, MEDIUM_MAX, class_size_of};
use std::thread;
use std::vec::Vec;

const SIZES: &[usize] = &[
    0,
    1,
    16,
    17,
    100,
    1000,
    4096,
    65_536,
    MEDIUM_MAX,
    MEDIUM_MAX + 1,
    1 << 20,
    5 << 20,
];

fn shared() -> &'static Shared {
    super::super::shared()
}

#[test]
fn mallocx_honours_size_alignment_and_zero_and_matches_nallocx() {
    for &size in SIZES {
        for la in [0u32, 4, 5, 6, 8, 12, 16, 20] {
            let flags = mallocx_lg_align(la) as c_int;
            let p = mallocx(size, flags, vstd::prelude::Tracked::assume_new());
            assert!(!p.is_null(), "mallocx({size}, lg_align {la})");
            assert_eq!(
                p as usize % (1usize << la),
                0,
                "mallocx({size}, lg_align {la}) = {p:p}"
            );
            let usable = sallocx(p, flags);
            assert!(usable >= size);
            assert_eq!(
                usable,
                nallocx(size, flags),
                "nallocx disagrees for size {size} lg_align {la}"
            );
            assert_eq!(usable, crate::api::libc::malloc_usable_size(p));
            // Every class is a multiple of the quantum, so up to it the
            // alignment leaves the class alone.
            if (1usize << la) <= crate::QUANTUM {
                match class_size_of(size) {
                    Some(s) => assert_eq!(usable, s),
                    None => assert_eq!(usable, shared().huge.usable_for(size).unwrap()),
                }
            }
            // Dirty it, free it, and take the same shape zeroed.
            // SAFETY: `p` is live with `usable` bytes (`sallocx`).
            unsafe { ptr::write_bytes(p, 0xAB, usable) };
            dallocx(
                p,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            let z = mallocx(
                size,
                flags | MALLOCX_ZERO as c_int,
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!z.is_null());
            let n = sallocx(z, 0);
            for i in 0..n {
                assert_eq!(
                    // SAFETY: `i < sallocx(z)`, inside the live allocation `z`.
                    unsafe { *z.add(i) },
                    0,
                    "mallocx ZERO size {size} lg_align {la} byte {i}"
                );
            }
            sdallocx(
                z,
                size,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
    // Alignment above the medium tier is a huge run even for one byte.
    let p = mallocx(
        1,
        mallocx_lg_align(19) as c_int,
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(shared().regions.classify(p as usize) != Kind::Invalid);
    assert!(matches!(
        shared().regions.classify(p as usize),
        Kind::Huge { .. }
    ));
    assert_eq!(p as usize % (1 << 19), 0);
    dallocx(
        p,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(nallocx(usize::MAX, 0), 0);
    assert_eq!(nallocx(0, 0), class_size_of(0).unwrap());
    assert!(mallocx(usize::MAX / 2, 0, vstd::prelude::Tracked::assume_new()).is_null());
}

/// `nallocx` is 0 exactly when `mallocx` rejects the size or alignment:
/// a request larger than the huge region, or an alignment no page of it
/// satisfies, fails every time, so `nallocx` must say so; a large
/// aligned run that fits still gets its size.
#[test]
fn nallocx_is_zero_when_mallocx_rejects() {
    for (size, flags) in [
        (usize::MAX / 2, 0),
        (1usize << 60, 0),
        (1, mallocx_lg_align(62) as c_int),
        (1, mallocx_lg_align(63) as c_int),
    ] {
        assert!(
            mallocx(size, flags, vstd::prelude::Tracked::assume_new()).is_null(),
            "mallocx({size:#x}, {flags:#x})"
        );
        assert_eq!(nallocx(size, flags), 0, "nallocx({size:#x}, {flags:#x})");
    }
    let flags = mallocx_lg_align(24) as c_int;
    let n = nallocx(1 << 20, flags);
    assert_eq!(n, 1 << 20);
    let p = mallocx(1 << 20, flags, vstd::prelude::Tracked::assume_new());
    assert!(!p.is_null());
    assert_eq!(p as usize % (1 << 24), 0);
    dallocx(
        p,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

#[test]
fn rallocx_keeps_moves_zeroes_and_preserves_content() {
    // Same class keeps the pointer; a class change moves and copies.
    let p = rallocx(
        ptr::null_mut(),
        10,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    for i in 0..10 {
        // SAFETY: `p` is a live 10-byte allocation.
        unsafe { *p.add(i) = i as u8 };
    }
    assert_eq!(
        rallocx(
            p,
            16,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        p
    );
    let q = rallocx(
        p,
        100,
        MALLOCX_ZERO as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_ne!(q, p);
    let n = sallocx(q, 0);
    for i in 0..n {
        let want = if i < 10 { i as u8 } else { 0 };
        // SAFETY: `i < sallocx(q)`, inside the live allocation `q`.
        assert_eq!(unsafe { *q.add(i) }, want, "byte {i} after rallocx ZERO");
    }
    // Alignment forces a move even within the same size.
    let r = rallocx(
        q,
        100,
        mallocx_lg_align(12) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(r as usize % 4096, 0);
    // SAFETY: `r` is live with at least 100 bytes.
    assert_eq!(unsafe { *r.add(9) }, 9);
    // Into and out of the huge tier.
    let h = rallocx(
        r,
        MEDIUM_MAX + 1,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(matches!(
        shared().regions.classify(h as usize),
        Kind::Huge { .. }
    ));
    // SAFETY: `h` is a live huge allocation (more than 10 bytes).
    assert_eq!(unsafe { *h.add(9) }, 9);
    let page = shared().regions.page_size();
    assert_eq!(
        rallocx(
            h,
            MEDIUM_MAX + page,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        ),
        h,
        "same page count keeps the run"
    );
    let h2 = rallocx(
        h,
        MEDIUM_MAX + page + 1,
        MALLOCX_ZERO as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_ne!(h2, h);
    // SAFETY: `h2` is a live huge allocation.
    assert_eq!(unsafe { *h2.add(9) }, 9);
    let n = sallocx(h2, 0);
    for i in (MEDIUM_MAX + page)..n {
        // SAFETY: `i < sallocx(h2)`, inside the live allocation `h2`.
        assert_eq!(unsafe { *h2.add(i) }, 0, "huge rallocx ZERO tail byte {i}");
    }
    let s = rallocx(
        h2,
        24,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(sallocx(s, 0), class_size_of(24).unwrap());
    // SAFETY: `s` is live with 24 bytes.
    assert_eq!(unsafe { *s.add(9) }, 9);
    // xallocx never moves and reports the current usable size, with
    // or without `extra` (jemalloc may grow into `size + extra`;
    // challoc never grows in place, so `extra` changes nothing).
    assert_eq!(xallocx(s, 1 << 20, 0, 0), class_size_of(24).unwrap());
    assert_eq!(xallocx(s, 1, 0, 0), class_size_of(24).unwrap());
    assert_eq!(xallocx(s, 1, 100, 0), class_size_of(24).unwrap());
    assert_eq!(xallocx(s, 24, 1 << 20, 0), class_size_of(24).unwrap());
    assert_eq!(
        xallocx(s, 1 << 20, 1 << 20, MALLOCX_ZERO as c_int),
        class_size_of(24).unwrap()
    );
    // SAFETY: as above.
    assert_eq!(unsafe { *s.add(9) }, 9, "xallocx touched nothing");
    // A failed rallocx leaves the block intact.
    assert!(
        rallocx(
            s,
            usize::MAX / 2,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    // SAFETY: as above: the failed `rallocx` left `s` live.
    assert_eq!(unsafe { *s.add(9) }, 9);
    dallocx(
        s,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    dallocx(
        ptr::null_mut(),
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

/// `MALLOCX_TCACHE_NONE` bypasses every cache, sampled (the proofs'
/// `Heap::caches_same`): direct allocations and frees through
/// `mallocx`, `rallocx`, `dallocx` and `sdallocx` leave every cache of
/// the thread's heap (home, another arena's, the foreign one) and of an
/// explicit cache as they were, entries, runs, capacities and pop
/// counts included.
#[test]
fn tcache_none_leaves_every_cache_alone() {
    use crate::heap::{FOREIGN, Heap, existing_heap};
    use crate::layout::NCLASSES;
    let s = shared();
    let lua = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("arenas.create");
    let t = s
        .tcaches
        .create(
            s,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("tcache.create");
    let sizes = [16usize, 100, 300, 1000, 4096];
    // Give the caches something to hold: the home caches, the Lua
    // arena's, the foreign one (a plain free of a Lua slot) and the
    // explicit cache's.
    let mut kept = Vec::new();
    for &size in &sizes {
        for flags in [0, mallocx_arena(lua), mallocx_tcache(t)] {
            let ps: Vec<*mut u8> = (0..4)
                .map(|_| mallocx(size, flags as c_int, vstd::prelude::Tracked::assume_new()))
                .collect();
            dallocx(
                ps[0],
                flags as c_int,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            crate::api::libc::free(
                ps[1],
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            kept.push((ps[2], flags));
            kept.push((ps[3], flags));
        }
    }
    let snap = |h: &Heap| {
        let mut v = Vec::new();
        for a in [h.arena(), 0, lua, FOREIGN] {
            for c in 0..NCLASSES {
                if let Some(cc) = h.cache_in(a, c) {
                    v.push((
                        a,
                        c,
                        cc.cap(),
                        cc.allocs(),
                        cc.cached().to_vec(),
                        cc.run().to_vec(),
                    ));
                }
            }
        }
        v
    };
    let before = (
        snap(existing_heap(s).unwrap()),
        snap(s.tcaches.get(t, Tracked::assume_new()).unwrap()),
    );
    for a in [lua, FOREIGN] {
        assert!(before.0.iter().any(|e| e.0 == a && !e.4.is_empty()), "{a}");
    }
    assert!(before.0.iter().any(|e| !e.4.is_empty()));
    assert!(before.1.iter().any(|e| !e.4.is_empty()));
    let none = MALLOCX_TCACHE_NONE;
    for &size in &sizes {
        for flags in [
            none,
            none | mallocx_arena(lua),
            none | MALLOCX_ZERO,
            none | mallocx_lg_align(6),
        ] {
            let flags = flags as c_int;
            let p = mallocx(size, flags, vstd::prelude::Tracked::assume_new());
            assert!(!p.is_null());
            let q = rallocx(
                p,
                size * 2,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!q.is_null());
            let r = mallocx(size, flags, vstd::prelude::Tracked::assume_new());
            assert!(!r.is_null());
            dallocx(
                q,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            sdallocx(
                r,
                size,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
    let after = (
        snap(existing_heap(s).unwrap()),
        snap(s.tcaches.get(t, Tracked::assume_new()).unwrap()),
    );
    assert_eq!(after, before, "a MALLOCX_TCACHE_NONE call touched a cache");
    for (p, flags) in kept {
        dallocx(
            p,
            flags as c_int,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    assert!(s.tcaches.destroy(
        s,
        t,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new()
    ));
}

#[test]
fn mallocx_routes_arenas_and_caches() {
    let s = shared();
    let lua = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("arenas.create");
    let c = crate::layout::class_of(300).unwrap();
    // Default cache, explicit arena.
    let p = mallocx(
        300,
        mallocx_arena(lua) as c_int,
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        s.classify_free(p, vstd::prelude::Tracked::assume_new()),
        Ok((c, lua))
    );
    // rallocx within the class keeps it in the Lua arena; across
    // classes with the arena flag lands there too.
    assert_eq!(
        rallocx(
            p,
            320,
            mallocx_arena(lua) as c_int,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        ),
        p
    );
    let q = rallocx(
        p,
        3000,
        mallocx_arena(lua) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        s.classify_free(q, vstd::prelude::Tracked::assume_new())
            .map(|x| x.1),
        Ok(lua)
    );
    dallocx(
        q,
        mallocx_arena(lua) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // No cache: the live count moves at once.
    let flags = (mallocx_arena(lua) | MALLOCX_TCACHE_NONE) as c_int;
    let d = mallocx(300, flags, vstd::prelude::Tracked::assume_new());
    let (slab, _) = s.table(c).locate(d);
    let desc = s.table(c).desc(slab);
    assert_eq!(desc.arena(), lua);
    let live = desc.live();
    dallocx(
        d,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(desc.live(), live - 1);
    // Explicit cache.
    let t = s
        .tcaches
        .create(
            s,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("tcache.create");
    let flags = mallocx_tcache(t) as c_int;
    let e = mallocx(300, flags, vstd::prelude::Tracked::assume_new());
    let th = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    let before = th.cache_in(0, c).map_or(0, |cc| cc.len());
    dallocx(
        e,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(th.cache_in(0, c).unwrap().len(), before + 1);
    assert!(s.tcaches.destroy(
        s,
        t,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new()
    ));
    // Huge runs record the arena.
    let h = mallocx(
        2 << 20,
        mallocx_arena(lua) as c_int,
        vstd::prelude::Tracked::assume_new(),
    );
    let Err(page) = s.classify_free(h, vstd::prelude::Tracked::assume_new()) else {
        panic!("not huge")
    };
    assert_eq!(
        s.huge
            .lookup(
                page,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .unwrap()
            .arena,
        lua
    );
    dallocx(
        h,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // Nonexistent arena or explicit cache: null, not an abort.
    assert!(
        mallocx(
            300,
            mallocx_arena(4000) as c_int,
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert!(
        mallocx(
            2 << 20,
            mallocx_arena(4000) as c_int,
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    let _ = Tcache::Default;
}

/// Arenas and explicit caches against the real allocator (design 13.3):
/// `MALLOCX_ARENA` allocations come from the named arena and plain
/// `free` puts them on the freeing thread's one foreign cache for the
/// class, whichever thread that is (the flush routes them home; the
/// per-arena cache on the `others` list sees explicit-arena
/// allocations only); `MALLOCX_TCACHE_NONE` claims and
/// releases against the slab bitmaps at once; an explicit cache holds
/// its own slots until flushed.
#[test]
fn arenas_route_mallocx_allocations_and_plain_frees() {
    use crate::api::libc as api_libc;
    use crate::heap;
    use crate::layout::class_of;
    let s = shared();
    let lua = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("arenas.create");
    assert!(s.multi_arena());
    let c = class_of(200).unwrap();
    let size = crate::layout::CLASSES[c].size();
    let f = Flags::decode(mallocx_arena(lua));
    assert_eq!((f.arena, f.tcache), (Some(lua), Tcache::Default));

    // Default cache, explicit arena: through this thread's heap.
    let (p, _, _) = mallocx::alloc(
        s,
        c,
        f.arena,
        f.tcache,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    // SAFETY: `p` is a live slot of `size` bytes.
    unsafe { core::ptr::write_bytes(p, 0xa5, size) };
    assert_eq!(
        s.classify_free(p, vstd::prelude::Tracked::assume_new()),
        Ok((c, lua))
    );
    let h = heap::existing_heap(s).unwrap();
    // This thread's automatic arena: whichever `Arenas::assign` gave
    // the test thread (the default is `min(4 * ncpus, 64)` arenas).
    let home = h.arena();
    assert_ne!(home, lua);
    // A plain `free` of another arena's slot pushes onto the FOREIGN
    // cache; the thread's per-arena caches see only explicit-arena
    // traffic.
    let cached_in = |h: &heap::Heap| h.cache_in(lua, c).map_or(0, |cc| cc.len());
    let cached_foreign = |h: &heap::Heap| h.cache_in(heap::FOREIGN, c).map_or(0, |cc| cc.len());
    let before = cached_in(h);
    let before_foreign = cached_foreign(h);
    api_libc::free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(cached_in(h), before);
    assert_eq!(cached_foreign(h), before_foreign + 1);
    // realloc within the class keeps the pointer and the arena.
    let (p, _, _) = mallocx::alloc(
        s,
        c,
        Some(lua),
        Tcache::Default,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // (Written again: this is a fresh slot of the Lua arena's cache,
    // the one freed above went to the FOREIGN cache.)
    // SAFETY: as above.
    unsafe { core::ptr::write_bytes(p, 0xa5, size) };
    assert_eq!(
        api_libc::realloc(
            p,
            size,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        p
    );
    let q = api_libc::realloc(
        p,
        size * 4,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_ne!(q, p);
    // SAFETY: `q` is a live allocation of `size * 4` bytes.
    assert_eq!(unsafe { *q }, 0xa5);
    // Across classes, plain realloc lands in the caller's home arena.
    assert_eq!(
        s.classify_free(q, vstd::prelude::Tracked::assume_new())
            .map(|x| x.1),
        Ok(home)
    );
    api_libc::free(
        q,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );

    // Direct path: live count moves at once, no cache involved.
    let f = Flags::decode(mallocx_arena(lua) | MALLOCX_TCACHE_NONE);
    assert_eq!(f.tcache, Tcache::None);
    let (d, _, _) = mallocx::alloc(
        s,
        c,
        f.arena,
        f.tcache,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let (slab, _) = s.table(c).locate(d);
    let desc = s.table(c).desc(slab);
    assert_eq!(desc.arena(), lua);
    let live = desc.live();
    let cached = cached_in(h);
    mallocx::free(
        s,
        d,
        Tcache::None,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(desc.live(), live - 1);
    assert_eq!(cached_in(h), cached);
    // A non-existent arena is null, not an abort.
    assert!(
        mallocx::alloc(
            s,
            c,
            Some(4000),
            Tcache::Default,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .0
        .is_null()
    );
    assert!(
        mallocx::alloc(
            s,
            c,
            Some(4000),
            Tcache::None,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .0
        .is_null()
    );
    assert!(
        mallocx::alloc_huge(
            s,
            1 << 20,
            1,
            Some(4000),
            Tcache::Default,
            false,
            vstd::prelude::Tracked::assume_new()
        )
        .0
        .is_null()
    );

    // Huge runs record the arena too.
    let (hp, _, _) = mallocx::alloc_huge(
        s,
        3 << 20,
        1,
        Some(lua),
        Tcache::Default,
        false,
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!hp.is_null());
    let Err(page) = s.classify_free(hp, vstd::prelude::Tracked::assume_new()) else {
        panic!("not huge")
    };
    assert_eq!(
        s.huge
            .lookup(
                page,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            )
            .unwrap()
            .arena,
        lua
    );
    api_libc::free(
        hp,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );

    // Explicit cache.
    let t = s
        .tcaches
        .create(
            s,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("tcache.create");
    let f = Flags::decode(mallocx_tcache(t) | mallocx_arena(lua));
    assert_eq!(f.tcache, Tcache::Explicit(t));
    let (e, _, _) = mallocx::alloc(
        s,
        c,
        f.arena,
        f.tcache,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        s.classify_free(e, vstd::prelude::Tracked::assume_new()),
        Ok((c, lua))
    );
    let th = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    let before = th.cache_in(lua, c).unwrap().len();
    let before_foreign = th.cache_in(heap::FOREIGN, c).map_or(0, |cc| cc.len());
    // The free through the explicit cache (homed on arena 0) lands on
    // its FOREIGN cache, as any plain free of another arena's slot.
    mallocx::free(
        s,
        e,
        Tcache::Explicit(t),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(th.cache_in(lua, c).unwrap().len(), before);
    assert_eq!(
        th.cache_in(heap::FOREIGN, c).unwrap().len(),
        before_foreign + 1
    );
    assert!(s.tcaches.flush(s, t, vstd::prelude::Tracked::assume_new()));
    assert_eq!(th.cache_in(lua, c).unwrap().len(), 0);
    assert_eq!(th.cache_in(heap::FOREIGN, c).unwrap().len(), 0);
    assert!(s.tcaches.destroy(
        s,
        t,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new()
    ));

    // Cross-thread: another thread allocates from the Lua arena and its
    // own home arena; this thread frees both with plain `free`, both
    // land on this thread's FOREIGN cache, and the flush routes each
    // slot back to its own arena.
    let (other, ptrs) = thread::spawn(move || {
        let s = shared();
        let mut v = Vec::new();
        for i in 0..64 {
            let a = if i % 2 == 0 { Some(lua) } else { None };
            let (p, _, _) = mallocx::alloc(
                s,
                c,
                a,
                Tcache::Default,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!p.is_null());
            // SAFETY: `p` is a live, non-null slot of at least one byte.
            unsafe { *p = i as u8 };
            v.push(p as usize);
        }
        (heap::existing_heap(s).unwrap().arena(), v)
    })
    .join()
    .unwrap();
    // The other thread was bound to an idle automatic arena, not ours.
    assert_ne!(other, home);
    assert_ne!(other, lua);
    let h = heap::existing_heap(s).unwrap();
    // Pin the home (a `thread.arena` write): this thread's heap is
    // young and the other thread's arena is idle now, so its first
    // free of a slot there would otherwise move it onto that arena
    // (`Heap::try_rehome`, covered in heap/tests.rs).
    h.set_arena(
        s,
        home,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    h.flush_all(s, vstd::prelude::Tracked::assume_new());
    for (i, &p) in ptrs.iter().enumerate() {
        let p = p as *mut u8;
        // SAFETY: the other thread's slots are still allocated; nothing freed
        // them.
        assert_eq!(unsafe { *p }, i as u8);
        let (class, arena) = s
            .classify_free(p, vstd::prelude::Tracked::assume_new())
            .unwrap();
        assert_eq!(class, c);
        if i % 2 == 0 {
            assert_eq!(arena, lua);
        } else {
            assert_eq!(arena, other);
        }
        api_libc::free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    assert_eq!(
        cached_in(h),
        0,
        "plain frees never reach the thread's per-arena caches"
    );
    assert!(
        h.cache_in(other, c).is_none(),
        "a plain free creates no arena node"
    );
    assert!(
        h.cache(c).is_none_or(|cache| cache.len() == 0),
        "nothing landed in the home cache"
    );
    assert_eq!(
        cached_foreign(h),
        64,
        "both arenas' slots on the one FOREIGN cache"
    );
    h.flush_all(s, vstd::prelude::Tracked::assume_new());
    // Per-arena purge covers the Lua arena's slabs only.
    let lua_arena = s.arena(lua, vstd::prelude::Tracked::assume_new()).unwrap();
    let empty_before = lua_arena.0.classes[c].stats().empty;
    assert!(
        empty_before >= 1,
        "the Lua arena's slab should be Empty after the flush"
    );
    let purged = crate::purge::purge_arena(
        s,
        lua,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    )
    .unwrap();
    assert!(purged >= empty_before);
    assert_eq!(lua_arena.0.classes[c].stats().empty, 0);
    assert!(
        crate::purge::purge_arena(
            s,
            4000,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_none()
    );
}

#[test]
fn malloc_write_goes_to_the_hook_when_set() {
    let _serial = crate::testutil::global_state();
    use std::sync::Mutex;
    static GOT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static LENS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
        assert!(opaque.is_null());
        // SAFETY: the hook is handed a NUL-terminated message.
        let s = unsafe { core::ffi::CStr::from_ptr(s) };
        GOT.lock().unwrap().extend_from_slice(s.to_bytes());
        LENS.lock().unwrap().push(s.to_bytes().len());
    }
    set_message_hook_for_tests(Some(hook));
    let text: Vec<u8> = (0..2000u32).map(|i| b'a' + (i % 26) as u8).collect();
    malloc_write(&text, Tracked::assume_new());
    // jemalloc's 511-byte chunks, the remainder last.
    assert_eq!(*LENS.lock().unwrap(), [511, 511, 511, 467]);
    // Empty input calls the hook zero times.
    malloc_write(b"", Tracked::assume_new());
    assert_eq!(LENS.lock().unwrap().len(), 4);
    set_message_hook_for_tests(None);
    assert_eq!(
        *GOT.lock().unwrap(),
        text,
        "chunks must reassemble to the original"
    );
    assert!(message_hook().is_none());
    // Without a hook it goes to stderr and must not crash.
    malloc_write(b"challoc: malloc_write test line\n", Tracked::assume_new());
}
/// Design 13.5: hint calls run the calling thread's cache GC at most
/// once per `DEFRAG_GC_INTERVAL_NS`, reading the clock once every
/// `PERIODIC_EVERY` calls, so a GC runs within that many calls once one
/// is due, and releases idle cache entries. A free through no cache
/// runs none. Its own thread, so the heap and its caches are the
/// test's.
#[test]
fn get_defrag_hint_runs_the_callers_cache_gc() {
    thread::spawn(|| {
        // Fetched afresh each time: the hint takes the same heap.
        let heap = || {
            crate::heap::existing_heap(crate::heap::shared_ptr(shared()))
                .expect("this thread's heap")
        };
        // Through the thread's caches, in two classes: a batch out,
        // all but one back in.
        let mut kept = Vec::new();
        for size in [100usize, 300] {
            let ptrs: Vec<*mut u8> = (0..64)
                .map(|_| mallocx(size, 0, vstd::prelude::Tracked::assume_new()))
                .collect();
            assert!(ptrs.iter().all(|p| !p.is_null()));
            kept.push(ptrs[0]);
            for p in &ptrs[1..] {
                dallocx(
                    *p,
                    0,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
        }
        let lens = || -> Vec<usize> {
            (0..crate::layout::NCLASSES)
                .map(|c| heap().cache(c).map_or(0, |k| k.len()))
                .collect()
        };
        let every = crate::heap::PERIODIC_EVERY as usize;
        // A free through no cache runs no step.
        heap().set_last_gc(0);
        let copy = mallocx(
            100,
            MALLOCX_TCACHE_NONE as c_int,
            vstd::prelude::Tracked::assume_new(),
        );
        dallocx(
            copy,
            MALLOCX_TCACHE_NONE as c_int,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(heap().last_gc(), 0, "a free runs no step");
        // A last GC long ago (the monotonic clock is past 100 ms): a GC
        // runs within `PERIODIC_EVERY` calls.
        let first = lens();
        heap().set_last_gc(0);
        let mut calls = 0;
        while heap().last_gc() == 0 {
            assert!(calls < every, "no GC within {every} calls");
            get_defrag_hint(kept[0]);
            calls += 1;
        }
        // It restarted the low water; the next one, an interval later,
        // gives back three quarters of what sat unused since.
        let after_first = lens();
        heap().set_last_gc(0);
        for _ in 0..every {
            get_defrag_hint(kept[0]);
        }
        let after_second = lens();
        let c = crate::layout::class_of(100).unwrap();
        assert!(first[c] > 0);
        assert_eq!(
            after_second[c],
            after_first[c] - (after_first[c] - after_first[c] / 4)
        );
        // Further calls run a GC only once another is due.
        let at = heap().last_gc();
        for _ in 0..4 * every {
            get_defrag_hint(kept[0]);
        }
        let at2 = heap().last_gc();
        assert!(
            at2 == at || at2.wrapping_sub(at) >= crate::heap::DEFRAG_GC_INTERVAL_NS,
            "GCs {at} and {at2} closer than the interval"
        );
        for p in kept {
            dallocx(
                p,
                0,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    })
    .join()
    .unwrap();
}

/// Design 13.5: `get_defrag_hint` answers from the utilization data
/// with jemalloc's rule. A private arena keeps the bin figures this
/// test's own.
#[test]
fn get_defrag_hint_follows_the_utilization_data() {
    use crate::api::mallctl::utilization;
    let s = shared();
    let a = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("arenas.create");
    let flags = (mallocx_arena(a) | MALLOCX_TCACHE_NONE) as c_int;
    let c = crate::layout::class_of(100).unwrap();
    let cls = &CLASSES[c];
    let n = 20 * cls.nslots();
    let ptrs: Vec<*mut u8> = (0..n)
        .map(|_| mallocx(100, flags, vstd::prelude::Tracked::assume_new()))
        .collect();
    assert!(ptrs.iter().all(|p| !p.is_null()));
    // Every other slot of the first half goes back: those slabs are
    // half full, the second half's stay full.
    for p in ptrs[..n / 2].iter().step_by(2) {
        dallocx(
            *p,
            flags,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    let live: Vec<*mut u8> = ptrs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i >= n / 2 || i % 2 == 1)
        .map(|(_, p)| *p)
        .collect();
    let t = s.table(c);
    let cs = s
        .arena(a, vstd::prelude::Tracked::assume_new())
        .unwrap()
        .0
        .class(c);
    let mut moved = 0;
    for &p in &live {
        let hint = get_defrag_hint(p);
        let u = utilization(s, p as *const c_void, true);
        assert_eq!(u.nregs, cls.nslots());
        // The reference: jemalloc's rule over the same figures.
        let (slab, _) = t.locate(p);
        let slabcur = cs.lowest_partial(vstd::prelude::Tracked::assume_new());
        let mut curslabs = u.bin_nregs / u.nregs;
        let mut curregs = u.bin_nregs - u.bin_nfree;
        if let Some(sc) = slabcur {
            curregs -= t.desc(sc).live();
            curslabs -= 1;
        }
        let want =
            u.nfree > 0 && slabcur != Some(slab) && (u.nregs - u.nfree) * curslabs <= curregs;
        assert_eq!(
            hint, want as c_int,
            "ptr {p:p}: nfree {} of {}, bin {}/{}, slabcur {slabcur:?} slab {slab}",
            u.nfree, u.nregs, curregs, curslabs
        );
        if hint == 1 {
            assert!(u.nfree > 0, "a full slab is never worth moving out of");
            moved += 1;
        } else if u.nfree == 0 {
            // Full: 0 whatever the average.
        }
    }
    // The half-full slabs (all but the one a claim takes from next)
    // are below the average; the full ones are never hinted.
    assert!(
        moved >= cls.nslots() / 2 * 8,
        "{moved} hints for {} half-full slots",
        n / 4
    );
    assert!(
        live[n / 4..]
            .iter()
            .all(|p| get_defrag_hint(*p) == 0
                || utilization(s, *p as *const c_void, false).nfree > 0)
    );
    // Not a hint: a freed slot, a huge run, null, a stack address.
    assert_eq!(get_defrag_hint(ptrs[0]), 0, "freed");
    let h = mallocx(3 << 20, flags, vstd::prelude::Tracked::assume_new());
    assert_eq!(get_defrag_hint(h), 0, "huge");
    assert_eq!(get_defrag_hint(ptr::null()), 0, "null");
    let stack = 0u64;
    assert_eq!(
        get_defrag_hint(&stack as *const u64 as *const u8),
        0,
        "stack"
    );
    dallocx(
        h,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // A class above BIN_MAX answers like a bin (the utilization queries
    // report it as one region; the hint has no geometry contract): two
    // full slabs of 16 KiB slots plus one slab holding a single slot,
    // whose block is below the average and worth moving unless it sits
    // in the slab a claim takes from next.
    let cm = crate::layout::class_of(16 << 10).unwrap();
    assert!(cm >= crate::layout::NBINS);
    let clm = &CLASSES[cm];
    let big: Vec<*mut u8> = (0..2 * clm.nslots() + 1)
        .map(|_| mallocx(16 << 10, flags, vstd::prelude::Tracked::assume_new()))
        .collect();
    assert!(big.iter().all(|p| !p.is_null()));
    let um = utilization(s, big[0] as *const c_void, true);
    assert_eq!(
        (um.nregs, um.nfree),
        (1, 0),
        "the utilization query keeps jemalloc's large-extent shape"
    );
    let tm = s.table(cm);
    let csm = s
        .arena(a, vstd::prelude::Tracked::assume_new())
        .unwrap()
        .0
        .class(cm);
    let (lone_slab, _) = tm.locate(*big.last().unwrap());
    assert_eq!(tm.desc(lone_slab).live(), 1);
    let lone_hint = get_defrag_hint(*big.last().unwrap());
    assert_eq!(
        lone_hint,
        (csm.lowest_partial(vstd::prelude::Tracked::assume_new()) != Some(lone_slab)) as c_int,
        "the lone block moves unless its slab is the current one"
    );
    for p in &big[..2 * clm.nslots()] {
        assert_eq!(
            get_defrag_hint(*p),
            0,
            "a full slab is never worth moving out of"
        );
    }
    for p in big {
        dallocx(
            p,
            flags,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    for p in live {
        dallocx(
            p,
            flags,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
}

/// A sized free of a huge run accepts any size whose page count is the
/// run's: every size with the run's `nallocx`, the small size of a run
/// an alignment made huge, and the size `realloc` last kept.
#[test]
fn sdallocx_of_a_huge_run_takes_any_size_in_its_last_page() {
    let size = (1 << 20) + 1;
    let usable = nallocx(size, 0);
    for hint in [size, usable - 1, usable] {
        let p = mallocx(size, 0, vstd::prelude::Tracked::assume_new());
        assert!(!p.is_null());
        assert_eq!(nallocx(hint, 0), usable);
        sdallocx(
            p,
            hint,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    // A run an alignment made huge takes the small size it was asked
    // for, with or without the alignment flag.
    let flags = mallocx_lg_align(20) as c_int;
    let p = mallocx(64, flags, vstd::prelude::Tracked::assume_new());
    assert!(!p.is_null());
    sdallocx(
        p,
        64,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let p = crate::api::libc::aligned_alloc(1 << 20, 64, vstd::prelude::Tracked::assume_new());
    assert!(!p.is_null());
    crate::api::libc::free_aligned_sized(
        p,
        1 << 20,
        64,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // A run `realloc` kept takes the size the caller last asked for.
    let p = mallocx(1 << 20, 0, vstd::prelude::Tracked::assume_new());
    let q = crate::api::libc::realloc(
        p,
        (1 << 20) - 100,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(q, p);
    crate::api::libc::free_sized(
        q,
        (1 << 20) - 100,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}
