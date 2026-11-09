//! Class lines and slot checks: the class-line tables, the sized and
//! guard-slab slot tests, and the sized free paths.

use super::*;

/// [`super::guard_interval_for`], with the class table's permission,
/// which is ghost.
fn guard_interval_for(class: usize, page: usize, interval: u32) -> u32 {
    super::guard_interval_for(class, page, interval, Tracked::assume_new())
}

/// Every class's [`ClassLine`](super::shared::ClassLine) holds what the
/// regions and the slab table say (region base, bitmap and owner bases,
/// group size), and its watermark copy follows the table's through
/// hand-outs.
#[test]
fn class_lines_agree_with_the_tables() {
    let s = shared();
    for c in 0..NCLASSES {
        let t = s.table(c);
        let (base, bits, wm, owners, lg_group) = s.lines[c].parts();
        assert_eq!(base, s.regions.class_base[c], "class {c}");
        assert_eq!(bits, t.bits_base(), "class {c}");
        assert_eq!(owners, t.owners_base(), "class {c}");
        assert_eq!(lg_group, t.lg_group(), "class {c}");
        assert_eq!(wm, t.committed_slabs(), "class {c}");
    }
    let th = TestHeap::new(s);
    let c = class_with(64);
    let before = s.table(c).committed_slabs();
    let mut ps = std::vec::Vec::new();
    while s.table(c).committed_slabs() == before {
        ps.push(th.alloc(c));
    }
    assert_eq!(s.lines[c].parts().2, s.table(c).committed_slabs());
    for p in ps {
        th.free(p);
    }
    th.destroy();
}

/// Each class line's 64-bit `inv` inverts the odd part of the class size
/// modulo `2^64`, and its low half is the class table's 32-bit
/// `inv_odd`, the multiplier the `free` path's slot-start test reads.
#[test]
fn class_line_inverse_matches_the_class_table() {
    let s = shared();
    for c in 0..NCLASSES {
        let cls = &CLASSES[c];
        let inv = s.lines[c].inv();
        let odd = (cls.size() >> cls.tz) as u64;
        assert_eq!(odd.wrapping_mul(inv), 1, "class {c}");
        assert_eq!(inv as u32, cls.inv_odd, "class {c}");
    }
}

/// The sized fast path's one-test bound and slot-start check
/// ([`ClassLine::sized_ok`]) accepts exactly what the reference does: a
/// slot start inside the class's region ([`Regions::slot_in_class`]) in
/// a slab that is not a guard slab ([`SlabTable::is_guard_slab`]). Every
/// class, guards on and off, at every byte offset of the first slots and
/// the last ones of the region, at slot starts and their neighbours
/// across whole slabs, around the region's ends, at the region bits
/// shifted in above it (one bit and its neighbours at each position), and
/// at random 64-bit pointers and random offsets inside the region.
#[test]
fn sized_slot_agrees_with_the_reference() {
    for s in [TestShared::new().guards().build(), shared()] {
        let r = &s.regions;
        let len = r.region_len();
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        for c in 0..NCLASSES {
            let t = s.table(c);
            let cls = &CLASSES[c];
            let base = r.class_base[c];
            let check = |rel: usize| {
                let p = base.wrapping_add(rel);
                let want = r
                    .slot_in_class(c, p)
                    .is_some_and(|(slab, _)| !t.is_guard_slab(slab));
                assert_eq!(
                    s.sized_slot(p as *mut u8, Some(c)).is_some(),
                    want,
                    "class {c} offset {rel:#x}"
                );
            };
            let size = cls.size();
            for rel in (0..4 * size).chain(len - 4 * size..len + 4 * size) {
                check(rel);
            }
            for slab in [0, 1, 2, t.nslabs() / 2, t.nslabs() - 1] {
                let start = slab << cls.lg_slab;
                for k in 0..=cls.nslots() {
                    for d in [0usize, 1, 8, 16, usize::MAX] {
                        check((start + k * size).wrapping_add(d));
                    }
                }
            }
            for b in 0..usize::BITS {
                for d in [0usize, 1, usize::MAX, size, size.wrapping_neg()] {
                    check((1usize << b).wrapping_add(d));
                    check(len.wrapping_neg().wrapping_add(1usize << b).wrapping_add(d));
                }
            }
            for _ in 0..20_000 {
                let x = xorshift(&mut st) as usize;
                check(x);
                check(x % len);
                check(x % len / size * size);
            }
        }
    }
}

/// The guard test the sized fast path reads ([`ClassLine::is_guard_at`])
/// agrees with [`SlabTable::is_guard_slab`] for every class over the
/// first slabs of the region and around its end, at the slab's first and
/// last byte, with guards on and off.
#[test]
fn guard_test_agrees_with_the_tables() {
    for s in [TestShared::new().guards().build(), shared()] {
        for c in 0..NCLASSES {
            let t = s.table(c);
            let line = &s.lines[c];
            let lg_slab = CLASSES[c].lg_slab;
            let n = t.nslabs();
            for slab in (0..1024.min(n)).chain(n.saturating_sub(64)..n) {
                for rel in [slab << lg_slab, ((slab + 1) << lg_slab) - 1] {
                    assert_eq!(
                        line.is_guard_at(rel),
                        t.is_guard_slab(slab),
                        "class {c} slab {slab} offset {rel:#x}"
                    );
                }
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
    let small = (0..NCLASSES)
        .find(|&c| CLASSES[c].slab_size() < unit)
        .expect("a class with slabs below the guard unit");
    let large = (0..NCLASSES)
        .find(|&c| CLASSES[c].slab_size() >= unit)
        .expect("a class with slabs at or above the guard unit");
    for page in [4096usize, 16 << 10, 64 << 10] {
        let unit = unit.max(page);
        for interval in [1u32, 4, 16, 64] {
            for c in 0..NCLASSES {
                let group = CLASSES[c].slab_size().max(page);
                let r = guard_interval_for(c, page, interval);
                assert_eq!(guard_interval_for(c, page, 0), 0, "class {c} page {page}");
                assert!(
                    r.is_power_of_two(),
                    "class {c} page {page} interval {interval}: {r}"
                );
                if group >= unit {
                    assert_eq!(r, interval, "class {c} page {page}");
                } else {
                    assert_eq!(
                        r as usize * group,
                        interval as usize * unit,
                        "class {c} page {page}"
                    );
                }
            }
        }
    }
    assert_eq!(
        guard_interval_for(small, 4096, 4) as usize,
        4 * unit / CLASSES[small].slab_size().max(4096)
    );
    assert!(
        guard_interval_for(small, 4096, 4) > 4,
        "the scaling branch was taken"
    );
    assert_eq!(guard_interval_for(large, 4096, 4), 4);
    assert_eq!(
        guard_interval_for(small, 64 << 10, 4),
        4,
        "a 64 KiB page makes every group the unit"
    );
    assert_eq!(
        guard_interval_for(small, 4096, u32::MAX),
        u32::MAX,
        "saturates"
    );
}

/// The sized fast path's check rejects a slot
/// start inside a guard slab (arithmetic on a dense per-class pair), so
/// the exact slow path aborts instead of the guard memory being cached.
/// It does not read the watermark: a slot start in a slab past it
/// passes here and aborts at the flush (the next test).
#[test]
fn sized_slot_rejects_guard_slabs() {
    let s = TestShared::new().guards().build();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let cs = s.arena0.class(c);
    let n = CLASSES[c].nslots();
    let interval = guard_interval_for(
        c,
        s.regions.page_size(),
        Config::DEFAULT.guard_slab_interval,
    ) as usize;
    let all: Vec<*mut u8> = (0..(interval + 2) * n).map(|_| t.alloc(c)).collect();
    let committed = cs.committed_slabs();
    let guard = (0..committed)
        .find(|&i| cs.is_guard_slab(i))
        .expect("a guard slab was passed");
    let body =
        s.regions.slab_addr(c, guard) + s.regions.page_size().min(CLASSES[c].slab_size() / 2);
    assert_eq!(s.sized_slot(all[0], Some(c)), Some(c));
    assert_eq!(
        s.sized_slot(body as *mut u8, Some(c)),
        None,
        "guard slab {guard}"
    );
    assert_eq!(
        s.sized_slot(s.regions.slab_addr(c, committed) as *mut u8, Some(c)),
        Some(c),
        "deferred to the flush"
    );
    assert_eq!(s.sized_slot(all[0], Some(c + 1)), None, "wrong hint");
    assert_eq!(s.sized_slot(all[0], None), None, "huge hint");
    assert_eq!(
        s.sized_slot((all[0] as usize + 8) as *mut u8, Some(c)),
        None,
        "mid-slot"
    );
    assert_eq!(
        s.sized_slot((s.regions.class_base[c] - 64) as *mut u8, Some(c)),
        None,
        "below the region"
    );
    assert_eq!(
        s.sized_slot(
            (s.regions.class_base[c] + s.regions.region_len()) as *mut u8,
            Some(c)
        ),
        None,
        "past the region"
    );
    for p in all {
        t.free(p);
    }
    t.destroy();
}

/// The watermark check the sized fast path does not make is the
/// flush's: a sized free of a slot start in a slab no
/// arena has been handed is cached and aborts by name when the cache
/// flushes it.
#[test]
#[should_panic(expected = "was never allocated")]
fn sized_free_past_the_watermark_aborts_at_the_flush() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let p = t.alloc(c);
    let committed = s.arena0.class(c).committed_slabs();
    let beyond = s.regions.slab_addr(c, committed + 1) as *mut u8;
    assert!(
        free_sized_fast(
            s,
            t.heap(),
            beyond,
            Some(c),
            s.push_flags(),
            Tracked::assume_new()
        ),
        "the fast path took it"
    );
    t.free(p);
    t.flush_all();
}

/// With `exact_sized_free` the sized fast path takes nothing: every
/// class's check declines a slot start it accepts by default, and a live
/// slot's sized free is left to the size check and the exact `free`.
#[test]
fn exact_sized_free_declines_every_sized_fast_free() {
    let s = TestShared::with(Config {
        narenas: 1,
        exact_sized_free: true,
        ..Config::DEFAULT
    })
    .build();
    let d = shared();
    for c in 0..NCLASSES {
        let first = |s: &Shared| s.regions.slab_addr(c, 0) as *mut u8;
        assert_eq!(
            d.sized_slot(first(d), Some(c)),
            Some(c),
            "class {c} default"
        );
        assert_eq!(s.sized_slot(first(s), Some(c)), None, "class {c}");
    }
    let t = TestHeap::new(s);
    for size in [64, 256] {
        let c = class_with(size);
        let p = t.alloc(c);
        assert!(
            !free_sized_fast(
                s,
                t.heap(),
                p,
                Some(c),
                s.push_flags(),
                Tracked::assume_new()
            ),
            "class {c}"
        );
        s.check_size_hint(
            p,
            size,
            Some(c),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        t.free(p);
    }
    t.destroy();
}

/// `checked_slot_any` is `checked_slot`'s bit test without the arena
/// test: they agree on every pointer of the home arena and on anything
/// that is not a live slot, and differ only for a live slot of another
/// arena, which the thread push declines.
#[test]
fn checked_slot_any_is_checked_slot_without_the_arena_test() {
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
    assert!(s.multi_arena());
    assert_eq!(
        (s.checked_slot(p0, h), s.checked_slot_any(p0)),
        (Some(c), Some(c))
    );
    assert_eq!(
        (s.checked_slot(p1, h), s.checked_slot_any(p1)),
        (None, Some(c))
    );
    // Not a slot start, a never-claimed slot, a pointer outside every region.
    // SAFETY: 8 bytes into the live 64-byte slot `p0`.
    let mid = unsafe { p0.add(8) };
    assert_eq!(
        (s.checked_slot(mid, h), s.checked_slot_any(mid)),
        (None, None)
    );
    // SAFETY: inside `p0`'s class region (an unclaimed slot of its slab).
    let unclaimed = unsafe { p0.add((HALF + 200) * 64) };
    assert_eq!(
        (s.checked_slot(unclaimed, h), s.checked_slot_any(unclaimed)),
        (None, None)
    );
    let outside = 0x10usize as *mut u8;
    assert_eq!(
        (s.checked_slot(outside, h), s.checked_slot_any(outside)),
        (None, None)
    );
    // A freed slot's bit clears at the flush; until then it is cached and set.
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
        p1,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.checked_slot_any(p0), Some(c));
    h.flush_all(s, Tracked::assume_new());
    assert_eq!(
        (
            s.checked_slot(p0, h),
            s.checked_slot_any(p0),
            s.checked_slot_any(p1)
        ),
        (None, None, None)
    );
}
