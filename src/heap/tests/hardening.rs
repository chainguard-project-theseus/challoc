//! The opt-in hardening keys: `zero_on_free` and `slot_randomize`.

use super::*;

// The opt-in `zero_on_free` and `slot_randomize` keys.

/// The two push flags are one byte on `Shared`'s first line; the
/// default configuration leaves it zero.
#[test]
fn push_flags_encode_zero_on_free_and_double_free_scan() {
    // `PUSH_PAC` follows the host; the two opt-in bits are what this
    // test is about.
    let pac = if pac::available() { PUSH_PAC } else { 0 };
    assert_eq!(
        shared().push_flags(),
        pac,
        "the test builder asks for pac_pointers"
    );
    assert!(!shared().zero_on_free());
    let s = TestShared::with(Config {
        narenas: 1,
        zero_on_free: true,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.push_flags(), PUSH_ZERO, "pac_pointers is off by default");
    assert!(s.zero_on_free() && !s.double_free_scan());
    let s = TestShared::with(Config {
        narenas: 1,
        zero_on_free: true,
        double_free_scan: true,
        pac_pointers: true,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(s.push_flags(), PUSH_ZERO | PUSH_STATE | pac);
    assert!(s.zero_on_free() && s.double_free_scan());
    assert_eq!(
        core::mem::offset_of!(Shared, push_flags) / 128,
        core::mem::offset_of!(Shared, free_flags) / 128
    );
}

/// Slot memory is real (`TestShared`).
fn bytes_of(p: *mut u8, n: usize) -> Vec<u8> {
    (0..n)
        // SAFETY: the callers pass `n` bytes that stay mapped after a free
        // (`TestShared` slot memory, or a huge run the allocator retains).
        .map(|i| unsafe { core::ptr::read_volatile(p.add(i)) })
        .collect()
}

/// With the key a freed slot reads as zero the moment `free` returns,
/// through the plain path (cached), the sized path and the direct
/// release; without it the slot keeps its bytes (the default path does
/// not clear).
#[test]
fn zero_on_free_clears_the_slot_on_the_plain_sized_and_direct_paths() {
    let c = class_with(64);
    let size = CLASSES[c].size();
    let dirty = |t: &TestHeap| {
        let p = t.alloc(c);
        // SAFETY: `p` is a live slot of `size` bytes.
        unsafe { ptr::write_bytes(p, 0xA5, size) };
        p
    };
    // Default: the bytes survive the free (LIFO reuse hands them back).
    let s = shared();
    let t = TestHeap::new(s);
    let p = dirty(&t);
    t.free(p);
    assert!(
        bytes_of(p, size).iter().all(|&b| b == 0xA5),
        "the default path cleared a slot"
    );
    assert_eq!(t.alloc(c), p);
    t.free(p);
    t.flush_all();
    t.destroy();

    let s = TestShared::with(Config {
        narenas: 1,
        zero_on_free: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    // Plain free: cleared, then the same slot comes back zero without
    // `calloc`'s memset.
    let p = dirty(&t);
    t.free(p);
    assert!(
        bytes_of(p, size).iter().all(|&b| b == 0),
        "plain free left bytes"
    );
    assert_eq!(t.alloc(c), p);
    assert!(bytes_of(p, size).iter().all(|&b| b == 0));
    // Sized path (`sdallocx`): the size hint's class, no bit test. With
    // the key the fast push declines and `free_sized_slow` takes the
    // exact route, which clears the slot in `free_slow_in`.
    // SAFETY: `p` is a live slot of `size` bytes again (the `alloc` above).
    unsafe { ptr::write_bytes(p, 0xA5, size) };
    assert!(
        !free_sized_fast(
            s,
            t.heap(),
            p,
            Some(c),
            s.push_flags(),
            Tracked::assume_new()
        ),
        "the fast push accepted a slot under zero_on_free"
    );
    assert!(
        bytes_of(p, size).iter().all(|&b| b == 0xA5),
        "the declined push touched the slot"
    );
    s.check_size_hint(
        p,
        size,
        Some(c),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    t.free(p);
    assert!(
        bytes_of(p, size).iter().all(|&b| b == 0),
        "sized free left bytes"
    );
    assert_eq!(t.alloc(c), p);
    // A full cache: the slot goes through `free_slow` after the clear.
    let cap = t.cache(c).cap();
    let ps: Vec<*mut u8> = (0..cap).map(|_| dirty(&t)).collect();
    for &q in &ps {
        t.free(q);
    }
    for &q in &ps {
        assert!(
            bytes_of(q, size).iter().all(|&b| b == 0),
            "a flushed or cached slot kept bytes"
        );
    }
    // Direct release (`MALLOCX_TCACHE_NONE`), with no cache involved.
    // SAFETY: `p` is live again (the `alloc` above), `size` bytes.
    unsafe { ptr::write_bytes(p, 0xA5, size) };
    super::free_direct(s, p, Tracked::assume_new(), Tracked::assume_new());
    assert!(
        bytes_of(p, size).iter().all(|&b| b == 0),
        "direct free left bytes"
    );
    t.flush_all();
    t.destroy();
}

/// A huge run's usable bytes are cleared before the release, on the
/// thread path and the direct path; the run is retained (`Empty`) so
/// the memory is still there to read.
#[test]
fn zero_on_free_clears_a_huge_run() {
    let s = TestShared::with(Config {
        narenas: 1,
        zero_on_free: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    let n = 1 << 20;
    for direct in [false, true] {
        let (p, _) = t.alloc_huge(n, page_size());
        assert!(!p.is_null());
        let usable = s.huge.usable_for(n).unwrap();
        assert!(usable >= n);
        // SAFETY: `usable` is the huge allocation's usable size.
        unsafe { ptr::write_bytes(p, 0xA5, usable) };
        if direct {
            super::free_direct(s, p, Tracked::assume_new(), Tracked::assume_new())
        } else {
            t.free(p)
        }
        let b = bytes_of(p, usable);
        assert!(
            b.iter().all(|&x| x == 0),
            "huge free (direct {direct}) left bytes: first non-zero at {:?}",
            b.iter().position(|&x| x != 0)
        );
    }
    // Default: untouched.
    let s = shared();
    let t2 = TestHeap::new(s);
    let (p, _) = t2.alloc_huge(n, page_size());
    // SAFETY: `p` is a live huge allocation of at least `n` bytes.
    unsafe { ptr::write_bytes(p, 0xA5, n) };
    t2.free(p);
    assert!(
        bytes_of(p, n).iter().all(|&x| x == 0xA5),
        "the default huge free cleared the run"
    );
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
    let s = TestShared::with(Config {
        narenas: 1,
        slot_randomize: true,
        ..Config::DEFAULT
    })
    .build();
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

    let s = TestShared::with(Config {
        narenas: 1,
        slot_randomize: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    let cs = &s.arena0.classes[c];
    let (first_z, zeroed): (Vec<*mut u8>, Vec<bool>) = (0..batch)
        .map(|_| {
            t.heap().alloc_zeroed(
                s,
                c,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
        })
        .unzip();
    assert!(
        zeroed.iter().all(|&z| z),
        "the first batch from a Fresh slab is zero whatever its order"
    );
    assert!(!ascending(&first_z), "randomised batch is address-ordered");
    assert!(
        !first_z.windows(2).all(|w| w[0] > w[1]),
        "randomised batch is reverse-ordered"
    );
    assert!(
        contiguous(&first_z),
        "the first batch is not the slab's first {batch} slots"
    );
    let slab = cs.locate(first_z[0]).0;
    assert!(first_z.iter().all(|&p| cs.locate(p).0 == slab));
    let second: Vec<*mut u8> = (0..batch).map(|_| t.alloc(c)).collect();
    assert!(
        !ascending(&second) && contiguous(&second),
        "the second batch (bit loop) is ordered or not one run of slots"
    );
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
    let s = TestShared::with(Config {
        narenas: 1,
        slot_randomize: true,
        ..Config::DEFAULT
    })
    .build();
    let t = TestHeap::new(s);
    let c = class_with(1024);
    let cs = &s.arena0.classes[c];
    let nslots = CLASSES[c].nslots();
    let mut keep = residue(&t, c, MODEL_SLOTS / nslots, 560, 0x9e37_79b9_7f4a_7c15);
    assert!(Heap::fragmented(s, 0, c, Tracked::assume_new()));
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
