//! The thread cache: allocation order, refills and flushes, the
//! double-free scan on the push, the fast paths, the direct and
//! cross-thread paths, and huge runs through the heap.

use super::*;

/// Class 0's state hands out the pointer to the first class table, the one
/// `Shared::tables` returns (`ClassState::tables`).
#[test]
fn class_zero_state_points_at_the_class_tables() {
    let s = shared();
    let (tables, _) = s.arena0.classes[0].tables();
    assert_eq!(tables.cast_const(), s.tables().as_ptr());
}

#[test]
fn heap_starts_with_the_sentinel_and_allocates_caches_lazily() {
    let s = shared();
    let before = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated;
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
    let meta_used = s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated - before;
    assert!(meta_used >= core::mem::size_of::<Heap>() + 8 * CAP);
    assert!(meta_used <= 4096 + 8 * CAP, "{meta_used}");
    t.free(p);
    t.destroy();
    assert_eq!(
        s.meta.stats(vstd::prelude::Tracked::assume_new()).allocated,
        before
    );
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
    assert_eq!(
        t.heap().stats(),
        ThreadStats {
            allocated: (n as u64 + 2) * 16,
            deallocated: 2 * 16
        }
    );
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
    // The heap was the arena's last: its destruction purges the `Empty`
    // slabs it just flushed (design 13.3).
    t.destroy();
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: cs.committed_slabs(),
            purged: cs.committed_slabs(),
            ..Default::default()
        }
    );
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
    assert!(
        msg.contains(&std::format!("double free of {:#x} (class {c}", p as usize)),
        "{msg}"
    );
    msg
}

/// With `double_free_scan` a second free of a cached slot aborts at the
/// call however deep the first copy sits, and nothing is stored.
#[test]
fn double_free_scan_catches_a_cached_copy_at_any_depth() {
    let s = TestShared::with(Config {
        narenas: 1,
        double_free_scan: true,
        ..Config::DEFAULT
    })
    .build();
    assert!(s.double_free_scan());
    let t = TestHeap::new(s);
    let c = class_with(64);
    for between in [0usize, 1, 3, 4, 5, 64] {
        let p: Vec<*mut u8> = (0..between + 2).map(|_| t.alloc(c)).collect();
        let before = t.cache(c).len();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.free(p[0]);
            for &q in &p[1..=between] {
                t.free(q);
            }
            t.free(p[0]);
        }));
        let m = panic_message(r.expect_err("a cached double free went through"));
        assert!(
            m.contains(&std::format!(
                "double free of {:#x}: the slot is already in a thread cache",
                p[0] as usize
            )),
            "{between}: {m}"
        );
        // The abort came before the second copy was stored.
        assert_eq!(t.cache(c).len(), before + between + 1);
        t.free(p[between + 1]);
        t.flush_all();
    }
}

/// The fast paths carry no call: with `double_free_scan` the fast push
/// and the fast pop decline (`false`, `None`; nothing stored, nothing
/// counted) and the slow paths move the slot's state, so a double free
/// that reaches the slow path aborts by name; a due scheduled pop makes
/// `pop_fast` decline the same way and `Heap::alloc` serves it.
#[test]
fn fast_paths_decline_under_the_state_and_the_scheduled_pop() {
    let s = TestShared::with(Config {
        narenas: 1,
        double_free_scan: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    assert!(flags & PUSH_STATE != 0);
    let p = t.alloc(c);
    let q = t.alloc(c);
    t.free(p);
    let len = t.cache(c).len();
    // Every fast push declines, a cached slot's and a live one's.
    assert!(!t.heap().push_fast(c, p, flags, Tracked::assume_new()));
    assert!(!t.heap().push_fast(c, q, flags, Tracked::assume_new()));
    assert_eq!(t.cache(c).len(), len, "nothing was stored");
    // So does every fast pop.
    assert!(t.heap().pop_fast(c, flags, Tracked::assume_new()).is_none());
    assert_eq!(t.cache(c).len(), len, "nothing was popped");
    // The slow path a decline reaches aborts on the cached copy.
    let m = panic_message(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.free(p)))
            .expect_err("the slow path let the double free through"),
    );
    assert!(
        m.contains(&std::format!(
            "double free of {:#x}: the slot is already in a thread cache",
            p as usize
        )),
        "{m}"
    );
    assert_eq!(t.cache(c).len(), len, "the abort came before any store");
    // A live slot frees and comes back through the slow paths.
    t.free(q);
    assert_eq!(t.cache(c).len(), len + 1);
    assert_eq!(t.alloc(c), q);

    // The scheduled pop: force the schedule due and pop through the fast
    // path alone, then through `alloc`.
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(64);
    let flags = s.push_flags();
    let a = t.alloc(c);
    t.free(a);
    // `allocs + 1` lands on the mask. The write goes through the heap's
    // `&mut` (`Heap::cache` hands out a shared reference), and each read
    // after it takes a fresh reference.
    let k = &mut t.heap().caches[c];
    k.allocs = k.compact_mask as u64;
    let allocs = k.allocs;
    let len = t.cache(c).len();
    assert!(
        t.heap().pop_fast(c, flags, Tracked::assume_new()).is_none(),
        "a due schedule declines the fast pop"
    );
    assert_eq!(
        (t.cache(c).allocs, t.cache(c).len()),
        (allocs, len),
        "nothing counted, nothing popped"
    );
    let b = t.alloc(c);
    assert!(!b.is_null());
    assert_eq!(
        t.cache(c).allocs,
        allocs.wrapping_add(1),
        "the slow path counted the one pop"
    );
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
    assert!(
        msg.contains("both copies cached, one at the flush boundary"),
        "{msg}"
    );
}

/// A pair that the split keeps together stays cached (both copies, the
/// bit set) and is caught by the release that flushes it, not by the
/// boundary compare.
#[test]
fn a_cached_pair_kept_by_the_split_is_caught_when_released() {
    let msg = cached_pair_flush_message(|rest| rest / 2);
    assert!(
        msg.contains("double free of") && !msg.contains("boundary"),
        "{msg}"
    );
}

/// A flush of the whole cache (`n == len`, `rest_len == 0`): a stack in
/// address order takes the sorted branch (no grouping pass; the
/// `FOREIGN` cache's flush in xmalloc-test), a scrambled one the
/// unsorted branch; the kept-half sort has nothing to do, the straddling
/// compare has no kept maximum to match, and the header comes out empty
/// with its run, zero prefix and low water reset. `flush_all` is such a
/// flush of every class.
#[test]
fn a_whole_cache_flush_keeps_nothing_and_skips_grouping_when_sorted() {
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
    assert_eq!(
        (k.len(), k.run().len(), k.zeroed(), k.low_water()),
        (0, 0, 0, 0)
    );
    assert_eq!(
        k.unsorted_flushes, 0,
        "a sorted whole-cache flush takes the sorted branch"
    );
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
    assert_eq!(
        (k.len(), k.run().len(), k.zeroed(), k.low_water()),
        (0, 0, 0, 0)
    );
    assert_eq!(k.unsorted_flushes, 1);
    assert_eq!(live(s, c), 0);
    t.destroy();
}

/// A sorted whole-cache flush over many slabs (a class with few slots
/// per slab) is released slab by slab without the grouping pass, and a
/// duplicate in it (adjacent once sorted) still aborts at the release,
/// on the scratch bit, as it does on the unsorted branch.
#[test]
fn a_sorted_whole_cache_flush_spans_slabs_and_catches_a_duplicate() {
    let s = shared();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cap = default_cap(c) as usize;
    let mut ps: Vec<*mut u8> = (0..4 * cap).map(|_| t.alloc(c)).collect();
    t.flush_all();
    ps.sort_unstable();
    let slabs = |v: &[*mut u8]| {
        let mut n: Vec<usize> = v.iter().map(|&p| s.arena0.classes[c].locate(p).0).collect();
        n.dedup();
        n.len()
    };
    let batch = &ps[..cap - 1];
    assert!(slabs(batch) > 1, "the batch spans several slabs");
    for &p in batch {
        t.free(p);
    }
    assert!(t.cached_plain(c).is_sorted());
    let before = live(s, c);
    t.flush_all();
    assert_eq!(t.cache(c).unsorted_flushes, 0);
    assert_eq!(live(s, c), before - (cap - 1));
    // A duplicate: the stack stays sorted (the copies are adjacent).
    let dup = ps[cap + 1];
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        t.free(ps[cap]);
        t.free(dup);
        t.free(dup);
        t.free(ps[cap + 2]);
        assert!(t.cached_plain(c).is_sorted());
        t.flush_all();
    }));
    let msg = panic_message(r.expect_err("a sorted whole flush let a duplicate through"));
    assert!(
        msg.contains(&std::format!(
            "double free of {:#x} (class {c}",
            dup as usize
        )),
        "{msg}"
    );
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
    assert_eq!(
        kept,
        cached[..HALF].to_vec(),
        "the kept half is the lowest addresses, content intact"
    );
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
            v.push(
                (s.regions.slab_addr(c, sl) + ((k * 37) % cls.nslots()) * cls.size()) as *mut u8,
            );
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
    assert_eq!(
        seen,
        slabs.to_vec(),
        "one contiguous group per slab, ascending"
    );
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
    let mut wide = std::vec![
        at(1000, 3),
        at(0, 8),
        at(300, 1),
        at(0, 2),
        at(1000, 1),
        at(300, 5)
    ];
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
    let mut v: Vec<*mut u8> = (0..1000)
        .map(|_| (xorshift(&mut st) % 1_000_000) as *mut u8)
        .collect();
    let mut expect = v.clone();
    let key = |p: *mut u8| (p as usize) & 0xff;
    flag_sort(&mut v, 256, key);
    assert!(
        v.windows(2).all(|w| key(w[0]) <= key(w[1])),
        "buckets ascend"
    );
    v.sort_unstable();
    expect.sort_unstable();
    assert_eq!(v, expect);
}

/// `flag_sort` takes a batch as long as the largest cache
/// (`MAX_NSLOTS`, 65536 entries): a whole flush of a full cache that
/// spans 2 to 256 slabs. The bucket counts sum to the batch length, so
/// the cursors must hold 65536 itself.
#[test]
fn flag_sort_groups_a_batch_as_long_as_the_largest_cache() {
    let len = crate::config::MAX_NSLOTS as usize;
    let mut st = 0x0123_4567_89ab_cdefu64;
    for nb in [2usize, 3, 256] {
        let mut v: Vec<*mut u8> = (0..len)
            .map(|_| (xorshift(&mut st) % (1 << 40)) as *mut u8)
            .collect();
        let mut expect = v.clone();
        let key = |p: *mut u8| (p as usize) % nb;
        flag_sort(&mut v, nb, key);
        assert!(
            v.windows(2).all(|w| key(w[0]) <= key(w[1])),
            "nb {nb}: buckets ascend"
        );
        v.sort_unstable();
        expect.sort_unstable();
        assert_eq!(v, expect, "nb {nb}: a permutation of the input");
    }
}

/// A flush of a whole 65536-slot cache (`tcache_nslots_small_max` at
/// its bound, `Heap::flush_all` here, which `thread.tcache.flush`,
/// `tcache.flush` and thread exit take) hands `group_by_slab` all 65536
/// entries; scrambled and spread over a few 4096-slot slabs, they go
/// through `flag_sort` and every one is released.
#[test]
fn a_whole_flush_of_a_65536_slot_cache_releases_every_slot() {
    let cap = crate::config::MAX_NSLOTS as usize;
    let s = TestShared::with(Config {
        narenas: 1,
        lg_tcache_nslots_mul: 16,
        tcache_nslots_small_max: cap as u32,
        ..Config::DEFAULT
    })
    .build();
    let c = class_with(16);
    assert_eq!(s.config.cap_for(0, 0, c) as usize, cap);
    let t = TestHeap::new(s);
    let mut ps: Vec<*mut u8> = (0..2 * cap).map(|_| t.alloc(c)).collect();
    let mut st = 0xfeed_face_0bad_f00du64;
    for i in (1..ps.len()).rev() {
        let j = (xorshift(&mut st) % (i as u64 + 1)) as usize;
        ps.swap(i, j);
    }
    let (cached, kept) = ps.split_at(cap - t.len(c));
    for &p in cached {
        t.free(p);
    }
    assert_eq!(t.len(c), cap);
    assert!(!t.cached_plain(c).is_sorted());
    let flushes = t.cache(c).unsorted_flushes;
    t.flush_all();
    assert_eq!(t.cache(c).unsorted_flushes, flushes + 1);
    assert_eq!(t.len(c), 0);
    assert_eq!(live(s, c), kept.len());
    for &p in kept {
        t.free(p);
    }
    t.flush_all();
    assert_eq!(live(s, c), 0);
    t.destroy();
}

#[test]
fn direct_paths_bypass_the_cache() {
    let s = shared();
    let c = class_with(256);
    let (p, zero, _) = s.alloc_direct(
        &s.arena0,
        Tracked::assume_new(),
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(!p.is_null() && zero);
    assert_eq!(p as usize, s.regions.slab_addr(c, 0));
    assert_eq!(live(s, c), 1);
    let (q, zero, _) = s.alloc_direct(
        &s.arena0,
        Tracked::assume_new(),
        c,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(q as usize, p as usize + 256);
    assert!(!zero);
    s.free_direct(0, c, p, Tracked::assume_new(), Tracked::assume_new());
    assert_eq!(live(s, c), 1);
    s.free_direct(0, c, q, Tracked::assume_new(), Tracked::assume_new());
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
    assert_eq!(
        b.heap().stats(),
        ThreadStats {
            allocated: 0,
            deallocated: 200 * 64
        }
    );
    // B reuses what it freed, lowest address first after its flushes.
    let q = b.alloc(c);
    assert_eq!(q, ps[199]);
    b.free(q);
    a.destroy();
    b.destroy();
    assert_eq!(live(s, c), 0);
}

#[test]
fn huge_runs_go_through_the_heap_without_caches() {
    let s = shared();
    let t = TestHeap::new(s);
    let size = 300 * 1024;
    let (p, zero) = t.alloc_huge(size, 16);
    assert!(!p.is_null() && zero);
    assert_eq!(s.regions.classify(p as usize), Kind::Huge { page: 0 });
    assert_eq!(
        s.huge
            .usable_size(0, Tracked::assume_new(), Tracked::assume_new()),
        s.huge.usable_for(size).unwrap()
    );
    assert_eq!(t.stats().allocated, s.huge.usable_for(size).unwrap() as u64);
    let (q, _) = t.alloc_huge(size, 1 << 20);
    assert_eq!(q as usize % (1 << 20), 0);
    // Frees route by region: a huge free never touches a cache.
    t.free(p);
    assert_eq!(
        t.stats().deallocated,
        s.huge.usable_for(size).unwrap() as u64
    );
    assert!((0..NCLASSES).all(|c| t.heap().cache(c).is_none()));
    assert_eq!(s.huge.stats(Tracked::assume_new()).runs, 1);
    // The heap-less path frees huge runs too.
    s.free_huge(
        match s.regions.classify(q as usize) {
            Kind::Huge { page } => page,
            other => panic!("{other:?}"),
        },
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_eq!(s.huge.stats(Tracked::assume_new()).runs, 0);
    assert!(
        s.alloc_huge(
            usize::MAX,
            1,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new()
        )
        .0
        .is_null()
    );
}

/// The pages `Shared::alloc_huge` adds
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
    let info = s
        .huge
        .lookup(page_of(p), Tracked::assume_new(), Tracked::assume_new())
        .unwrap();
    assert_eq!(info.pages, s.huge.usable_pages(size).unwrap() + GUARD_PAGES);
    assert_eq!(s.arena0.large(), (1, info.pages));
    // An aligned run: the hole alignment leaves in front of it is a free
    // extent, not the run's pages.
    let (q, _) = t.alloc_huge(size, 1 << 20);
    let qinfo = s
        .huge
        .lookup(page_of(q), Tracked::assume_new(), Tracked::assume_new())
        .unwrap();
    assert_eq!(qinfo.pages, info.pages);
    assert_eq!(s.arena0.large(), (2, 2 * info.pages));
    // `alloc_run` reports the descriptor's pages, and the free the same.
    let r = s
        .huge
        .alloc_run(
            &s.meta,
            5 * page_size() + 1,
            1,
            0,
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0;
    assert!(!r.ptr.is_null());
    assert_eq!(r.pages, 6 + GUARD_PAGES);
    assert_eq!(
        r.pages,
        s.huge
            .lookup(page_of(r.ptr), Tracked::assume_new(), Tracked::assume_new())
            .unwrap()
            .pages
    );
    assert_eq!(
        s.huge
            .free_run(
                &s.meta,
                page_of(r.ptr),
                Tracked::assume_new(),
                Tracked::assume_new()
            )
            .pages,
        r.pages
    );
    t.free(p);
    assert_eq!(s.arena0.large(), (1, info.pages));
    t.free(q);
    assert_eq!(s.arena0.large(), (0, 0));
    assert_eq!(
        s.huge
            .alloc_run(
                &s.meta,
                usize::MAX,
                1,
                0,
                false,
                Tracked::assume_new(),
                Tracked::assume_new()
            )
            .0,
        crate::huge::Placed::NONE
    );
    t.destroy();
}
