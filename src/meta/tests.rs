use super::*;
use std::vec;
use std::vec::Vec;

use crate::testutil::{Reservation, page_size, xorshift};

/// The object side's commit step: [`COMMIT_CHUNK`] rounded up to the
/// page, so 16 pages under 4 KiB pages and one under 64 KiB pages.
fn chunk() -> usize {
    align_up(COMMIT_CHUNK, page_size())
}

/// `Meta::free` without the claim (the tests hold none: they free
/// what `Meta::alloc` returned, or deliberately bad pointers).
fn free(m: &Meta, p: *mut u8, l: Layout) {
    m.free(p, l, Tracked::assume_new(), Tracked::assume_new());
}

/// `Meta::reserve` without the claim to the pages.
fn reserve(m: &Meta, len: usize, align: usize) -> Option<usize> {
    m.reserve(len, align, Tracked::assume_new(), Tracked::assume_new())
}

/// `Meta::reserve_table`'s address, without the claim to the pages.
fn reserve_table(m: &Meta, len: usize, align: usize) -> Option<usize> {
    m.reserve_table(len, align, Tracked::assume_new(), Tracked::assume_new())
        .0
}

/// `Meta::commit` without the claim to the pages.
fn commit(m: &Meta, addr: usize, len: usize) -> Result<(), Errno> {
    m.commit(
        addr,
        len,
        Ghost::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    )
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn bins_are_well_formed() {
    assert_eq!(NBINS, 30);
    assert_eq!(BINS[0], 16);
    assert_eq!(BINS[NBINS - 1] as usize, MAX_BINNED);
    for w in BINS.windows(2) {
        assert!(w[0] < w[1]);
    }
    for (i, &b) in BINS.iter().enumerate() {
        let b = b as usize;
        assert_eq!(
            b % bin_align(i),
            0,
            "bin {i} ({b}) not a multiple of its alignment"
        );
        // Power of two or 1.5x a power of two; from 128 up, line sized.
        assert!(b.is_power_of_two() || (b.is_multiple_of(3) && (b / 3).is_power_of_two()));
        if i >= 4 {
            assert_eq!(b % CACHE_LINE, 0);
        } else {
            assert_eq!(bin_align(i), MIN_ALIGN);
        }
    }
    // Internal waste is bounded: consecutive bins grow by at most 2x.
    for w in BINS.windows(2) {
        assert!(w[1] <= 2 * w[0]);
    }
}

#[test]
fn bin_of_matches_linear_scan() {
    for size in 0..=MAX_BINNED + 1 {
        let expect = BINS.iter().position(|&b| b as usize >= size);
        assert_eq!(bin_of(size), expect, "size {size}");
    }
    assert_eq!(bin_of(usize::MAX), None);
    assert_eq!(bin_of(MAX_BINNED), Some(NBINS - 1));
    for size in [
        1usize, 16, 17, 32, 33, 48, 49, 64, 65, 128, 129, 256, 257, 384, 385, 512,
    ] {
        let b = bin_of(size).unwrap();
        assert!(BINS[b] as usize >= size);
        assert!(b == 0 || (BINS[b - 1] as usize) < size);
    }
}

#[test]
fn bin_for_promotes_on_alignment() {
    assert_eq!(bin_for(layout(8, 8)), Some(0));
    assert_eq!(bin_for(layout(8, 16)), Some(0));
    assert_eq!(bin_for(layout(8, 32)), Some(4));
    assert_eq!(bin_for(layout(40, 64)), Some(4));
    assert_eq!(bin_for(layout(64, 128)), Some(4));
    assert_eq!(bin_for(layout(100, 64)), Some(4));
    assert_eq!(bin_for(layout(129, 8)), Some(5));
    assert_eq!(BINS[5], 256);
    assert_eq!(bin_for(layout(257, 8)), Some(6));
    assert_eq!(BINS[6], 384);
    assert_eq!(bin_for(layout(MAX_BINNED + 1, 8)), None);
}

#[test]
fn fresh_region_stats() {
    let b = Reservation::new(1 << 20);
    let m = b.meta();
    assert_eq!(m.base(), b.base);
    assert_eq!(m.top(), b.base + b.len);
    assert_eq!(
        m.stats(Tracked::assume_new()),
        MetaStats {
            len: 1 << 20,
            ..MetaStats::default()
        }
    );
}

#[test]
fn first_objects_come_from_zero_committed_memory() {
    let b = Reservation::new(4 << 20);
    let m = b.meta();
    let l = layout(200, 8);
    let p = m.alloc(l, Tracked::assume_new());
    assert!(!p.is_null());
    assert_eq!(p as usize, b.base, "first object sits at the base");
    assert_eq!(p as usize % CACHE_LINE, 0);
    let s = m.stats(Tracked::assume_new());
    assert_eq!(s.committed, chunk(), "one chunk committed");
    assert_eq!(s.allocated, 256, "200 rounds up to the 256 bin");
    assert_eq!(s.bumped, 256);
    // Writable and zero.
    // SAFETY: `p` is a live 200-byte metadata object, unaliased.
    let slice = unsafe { core::slice::from_raw_parts_mut(p, 200) };
    assert!(slice.iter().all(|&x| x == 0));
    slice.fill(0xA5);
    // Small objects pack at 16 and do not disturb the chunk.
    let q = m.alloc(layout(1, 1), Tracked::assume_new());
    let r = m.alloc(layout(16, 16), Tracked::assume_new());
    assert_eq!(q as usize, b.base + 256);
    assert_eq!(r as usize, b.base + 272);
    assert_eq!(m.stats(Tracked::assume_new()).committed, chunk());
    // Crossing the chunk boundary commits the next one.
    let big = m.alloc(layout(MAX_BINNED, 64), Tracked::assume_new());
    assert!(!big.is_null());
    assert_eq!(
        m.stats(Tracked::assume_new()).committed,
        align_up(MAX_BINNED + 512, chunk())
    );
    // SAFETY: `big` is a live `MAX_BINNED`-byte object, unaliased.
    unsafe { core::slice::from_raw_parts_mut(big, MAX_BINNED) }.fill(1);
}

#[test]
fn free_list_is_lifo_and_per_bin() {
    let b = Reservation::new(1 << 20);
    let m = b.meta();
    let l = layout(100, 8);
    let a = m.alloc(l, Tracked::assume_new());
    let c = m.alloc(l, Tracked::assume_new());
    let d = m.alloc(layout(1000, 8), Tracked::assume_new());
    let bumped = m.stats(Tracked::assume_new()).bumped;
    free(&m, a, l);
    free(&m, c, l);
    assert_eq!(m.stats(Tracked::assume_new()).allocated, 1024);
    // Same bin: last freed comes back first, nothing new is bumped.
    assert_eq!(m.alloc(l, Tracked::assume_new()), c);
    assert_eq!(m.alloc(l, Tracked::assume_new()), a);
    assert_eq!(m.stats(Tracked::assume_new()).bumped, bumped);
    // Different bin: not served from that list.
    free(&m, a, l);
    let e = m.alloc(layout(200, 8), Tracked::assume_new());
    assert_ne!(e, a);
    assert!(m.stats(Tracked::assume_new()).bumped > bumped);
    free(&m, d, layout(1000, 8));
    assert_eq!(m.alloc(layout(1000, 8), Tracked::assume_new()), d);
    // alloc_zeroed scrubs a reused block but leaves fresh ones alone.
    // SAFETY: `d` is live in the 1024-byte bin (a 1000-byte request).
    unsafe { core::slice::from_raw_parts_mut(d, 1024) }.fill(0xFF);
    free(&m, d, layout(1000, 8));
    let z = m.alloc_zeroed(layout(1000, 8), Tracked::assume_new()).0;
    assert_eq!(z, d);
    assert!(
        // SAFETY: `z` is live in the 1024-byte bin.
        unsafe { core::slice::from_raw_parts(z, 1024) }
            .iter()
            .all(|&x| x == 0)
    );
    let fresh = m.alloc_zeroed(layout(5000, 8), Tracked::assume_new()).0;
    assert!(
        // SAFETY: `fresh` is a live 5000-byte object.
        unsafe { core::slice::from_raw_parts(fresh, 5000) }
            .iter()
            .all(|&x| x == 0)
    );
}

#[test]
fn alignment_is_honoured() {
    let b = Reservation::new(1 << 20);
    let m = b.meta();
    for align in [1usize, 2, 4, 8, 16, 32, 64, 128] {
        for size in [1usize, 15, 16, 17, 48, 49, 64, 65, 96, 300, 4097] {
            let l = layout(size, align);
            let p = m.alloc(l, Tracked::assume_new());
            assert!(!p.is_null());
            assert_eq!(p as usize % align, 0, "size {size} align {align}");
            assert_eq!(p as usize % bin_align(bin_for(l).unwrap()), 0);
            if size > 64 || align > 16 {
                assert_eq!(p as usize % CACHE_LINE, 0);
            }
        }
    }
}

#[test]
fn exhaustion_returns_null_and_recovers() {
    let b = Reservation::new(16 * page_size());
    let m = b.meta();
    let l = layout(1024, 8);
    let mut got = Vec::new();
    loop {
        let p = m.alloc(l, Tracked::assume_new());
        if p.is_null() {
            break;
        }
        got.push(p);
    }
    assert_eq!(
        got.len(),
        16 * page_size() / 1024,
        "16 pages hold exactly that many 1 KiB objects"
    );
    assert_eq!(m.stats(Tracked::assume_new()).committed, 16 * page_size());
    assert_eq!(m.stats(Tracked::assume_new()).bumped, 16 * page_size());
    assert!(m.alloc(layout(1, 1), Tracked::assume_new()).is_null());
    assert_eq!(
        reserve(&m, page_size(), page_size()),
        None,
        "no room left for a reservation"
    );
    // Freeing one makes exactly one more available.
    free(&m, got.pop().unwrap(), l);
    assert!(!m.alloc(l, Tracked::assume_new()).is_null());
    assert!(m.alloc(l, Tracked::assume_new()).is_null());
    // Distinct pointers throughout.
    got.sort_unstable();
    got.dedup();
    assert_eq!(got.len(), 16 * page_size() / 1024 - 1);
}

#[test]
fn reservations_come_from_the_top() {
    let b = Reservation::new(64 * page_size());
    let m = b.meta();
    // The eager carve below is the host policy's choice (off under a
    // finite `RLIMIT_DATA`); this test pins it on.
    m.set_eager_for_test(true);
    let end = b.base + b.len;
    let r1 = reserve(&m, 3 * page_size(), page_size()).unwrap();
    assert_eq!(r1, end - 3 * page_size());
    assert_eq!(m.top(), r1);
    // Sub-page lengths round up; large alignment rounds the start down.
    let r2 = reserve(&m, 10, 8 * page_size()).unwrap();
    assert_eq!(r2 % (8 * page_size()), 0);
    assert!(r2 + page_size() <= r1);
    assert_eq!(m.top(), r2);
    let s = m.stats(Tracked::assume_new());
    assert_eq!(s.reserved, end - r2);
    assert_eq!(s.committed, 0, "reservations are uncommitted");
    // Commit part of r1, write into it.
    commit(&m, r1 + page_size(), page_size()).unwrap();
    assert_eq!(m.stats(Tracked::assume_new()).committed, page_size());
    // SAFETY: that page of `r1` was just committed and nothing else uses it.
    unsafe { core::slice::from_raw_parts_mut((r1 + page_size()) as *mut u8, page_size()) }.fill(7);
    // A small reserve_committed is zero and writable, and (eager) sits
    // on the object side, not below r2.
    let r3 = m
        .reserve_committed(2 * page_size(), page_size(), Tracked::assume_new())
        .unwrap();
    assert_eq!(
        r3, b.base,
        "a small committed reservation is carved from the object side"
    );
    assert_eq!(m.top(), r2);
    // SAFETY: `r3` is a fresh committed reservation of two pages.
    let sl = unsafe { core::slice::from_raw_parts_mut(r3 as *mut u8, 2 * page_size()) };
    assert!(sl.iter().all(|&x| x == 0));
    sl.fill(9);
    let s = m.stats(Tracked::assume_new());
    assert_eq!(
        s.committed,
        page_size() + align_up(2 * page_size(), chunk()),
        "r1's page, r3's two and the chunk's slack around r3"
    );
    assert_eq!(s.eager, 2 * page_size());
    assert_eq!(s.bumped, 2 * page_size());
    // Objects and reservations never overlap: fill the rest with
    // objects and check the boundary.
    let mut n = 0;
    while !m
        .alloc(layout(page_size(), 8), Tracked::assume_new())
        .is_null()
    {
        n += 1;
    }
    assert_eq!(m.stats(Tracked::assume_new()).bumped, m.top() - b.base);
    assert_eq!(n, (m.top() - b.base) / page_size() - 2);
    assert_eq!(reserve(&m, page_size(), page_size()), None);
    assert_eq!(reserve_table(&m, page_size(), page_size()), None);
    // Object side writable exactly up to the reservation boundary;
    // committed counts r1's page, r3 and the objects' chunks.
    let s = m.stats(Tracked::assume_new());
    assert_eq!(s.writable, page_size() + (m.top() - b.base));
    assert_eq!(s.committed, page_size() + (m.top() - b.base));
}

/// Reservations larger than `EAGER_MAX`, or any table once eager
/// carving is off, come from the top uncommitted; `commit` then really
/// makes pages writable and counts them.
#[test]
fn large_and_forced_tables_stay_on_the_reservation_side() {
    let b = Reservation::new(EAGER_MAX + (EAGER_MAX >> 1) + 64 * page_size());
    let m = b.meta();
    let end = b.base + b.len;
    let big = reserve_table(&m, EAGER_MAX + page_size(), page_size()).unwrap();
    assert_eq!(
        big,
        end - EAGER_MAX - page_size(),
        "larger than EAGER_MAX: from the top"
    );
    assert_eq!(m.stats(Tracked::assume_new()).eager, 0);
    assert_eq!(
        m.stats(Tracked::assume_new()).reserved,
        EAGER_MAX + page_size()
    );
    commit(&m, big, page_size()).unwrap();
    let s = m.stats(Tracked::assume_new());
    assert_eq!((s.committed, s.writable), (page_size(), page_size()));
    m.set_eager_for_test(false);
    let small = reserve_table(&m, page_size(), page_size()).unwrap();
    assert_eq!(small, big - page_size(), "eager off: from the top too");
    assert_eq!(m.stats(Tracked::assume_new()).eager, 0);
    let c = m
        .reserve_committed(3 * page_size(), page_size(), Tracked::assume_new())
        .unwrap();
    assert_eq!(c, small - 3 * page_size());
    assert_eq!(m.stats(Tracked::assume_new()).committed, 4 * page_size());
    m.set_eager_for_test(true);
    let e = reserve_table(&m, page_size(), page_size()).unwrap();
    assert_eq!(e, b.base, "eager on again: from the base");
    assert_eq!(m.stats(Tracked::assume_new()).eager, page_size());
}

/// Eager tables, objects and more tables interleave on the object side
/// and stay one contiguous writable range: no hole, no page committed
/// twice, every table aligned as asked, and `commit` on such a table
/// only counts.
#[test]
fn eager_tables_pack_on_the_object_side() {
    let b = Reservation::new(16 << 20);
    let m = b.meta();
    m.set_eager_for_test(true);
    let o1 = m.alloc(layout(200, 8), Tracked::assume_new());
    assert_eq!(o1 as usize, b.base);
    let t1 = reserve_table(&m, 3 * page_size(), page_size()).unwrap();
    assert_eq!(
        t1,
        b.base + page_size(),
        "page aligned, right after the objects' page"
    );
    let t2 = reserve_table(&m, 10, 8 * page_size()).unwrap();
    assert_eq!(t2 % (8 * page_size()), 0);
    assert!(t2 >= t1 + 3 * page_size());
    let o2 = m.alloc(layout(1000, 8), Tracked::assume_new());
    assert_eq!(
        o2 as usize,
        t2 + page_size(),
        "objects resume right after the table"
    );
    let t3 = reserve_table(&m, EAGER_MAX, page_size()).unwrap();
    assert!(t3 >= o2 as usize + 1024);
    let before = m.stats(Tracked::assume_new());
    assert_eq!(before.eager, 3 * page_size() + page_size() + EAGER_MAX);
    assert_eq!(before.bumped, t3 + EAGER_MAX - b.base);
    assert!(before.writable >= before.bumped && before.writable.is_multiple_of(page_size()));
    assert!(before.committed <= before.writable);
    // The owner's commits: writable does not move, committed counts.
    commit(&m, t1, page_size()).unwrap();
    commit(&m, t3 + EAGER_MAX - page_size(), page_size()).unwrap();
    let after = m.stats(Tracked::assume_new());
    assert_eq!(after.writable, before.writable);
    assert_eq!(after.committed, before.committed + 2 * page_size());
    // Every byte of every table is writable and zero.
    for (t, len) in [(t1, 3 * page_size()), (t2, page_size()), (t3, EAGER_MAX)] {
        // SAFETY: each table is a live reservation of `len` bytes, committed
        // (eager or just above), and nothing else uses it.
        let sl = unsafe { core::slice::from_raw_parts_mut(t as *mut u8, len) };
        assert!(sl.iter().all(|&x| x == 0), "table at {t:#x} not zero");
        sl[0] = 1;
        sl[len - 1] = 1;
    }
    // Identities.
    let s = m.stats(Tracked::assume_new());
    assert!(s.allocated + s.eager <= s.bumped && s.bumped <= s.writable && s.writable <= s.len);
    assert!(s.bumped + s.reserved <= s.len);
}

/// An eager table that would reach the reservations is refused (and
/// the reservation side, already up against the chunk frontier, has no
/// room either); a smaller one still fits, then nothing fits either
/// way, and the failure leaves the allocator working for what does.
#[test]
fn eager_carve_stops_at_the_reservations() {
    let b = Reservation::new(16 * page_size());
    let m = b.meta();
    m.set_eager_for_test(true);
    let top = reserve(&m, 4 * page_size(), page_size()).unwrap();
    assert_eq!(top, b.base + 12 * page_size());
    // 10 pages fit on the object side (12 free); the chunk commit
    // stops at the reservations (under 4 KiB pages; a 64 KiB-page
    // chunk is one page, so the frontier stops at the table's end).
    let t1 = reserve_table(&m, 10 * page_size(), page_size()).unwrap();
    assert_eq!(t1, b.base);
    assert_eq!(
        m.stats(Tracked::assume_new()).writable,
        align_up(10 * page_size(), chunk()).min(12 * page_size())
    );
    // 3 more do not: not eagerly (past `top`), not as a reservation
    // (the frontier is at `top`).
    assert_eq!(reserve_table(&m, 3 * page_size(), page_size()), None);
    assert_eq!(
        m.reserve_committed(3 * page_size(), page_size(), Tracked::assume_new()),
        None
    );
    assert_eq!(m.top(), top, "a failed table reserves nothing");
    // 2 do, eagerly; then nothing, while an object in the free list
    // still does.
    let t2 = reserve_table(&m, 2 * page_size(), page_size()).unwrap();
    assert_eq!(t2, b.base + 10 * page_size());
    assert_eq!(reserve_table(&m, page_size(), page_size()), None);
    let l = layout(16, 8);
    assert!(m.alloc(l, Tracked::assume_new()).is_null());
    let s = m.stats(Tracked::assume_new());
    assert_eq!(
        (s.bumped, s.eager, s.reserved, s.writable),
        (
            12 * page_size(),
            12 * page_size(),
            4 * page_size(),
            12 * page_size()
        )
    );
    assert_eq!(s.committed, 0, "nothing asked for yet");
    commit(&m, t1, 10 * page_size()).unwrap();
    commit(&m, t2, 2 * page_size()).unwrap();
    assert_eq!(m.stats(Tracked::assume_new()).committed, 12 * page_size());
}

/// The host policy: eager carving is on under the default `thp:never`
/// on a kernel that honours `MAP_NORESERVE`, off under
/// `vm.overcommit_memory=2` and under a finite `RLIMIT_DATA`; the
/// probe reads the sysctl and the limit.
#[test]
fn eager_policy_follows_overcommit_mode_and_data_limit() {
    let mode = std::fs::read_to_string("/proc/sys/vm/overcommit_memory").unwrap();
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a live `rlimit` the call writes.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_DATA, &mut lim) }, 0);
    let limited = lim.rlim_cur != libc::RLIM_INFINITY;
    assert_eq!(crate::sys::data_limited(), limited);
    let expect = mode.trim() != "2" && !limited && crate::thp::is_never();
    assert_eq!(
        eager_policy(),
        expect,
        "overcommit_memory {mode:?}, RLIMIT_DATA {}",
        lim.rlim_cur
    );
    let b = Reservation::new(1 << 20);
    let m = b.meta();
    assert_eq!(m.eager_allowed(), expect);
}

#[test]
fn reserve_too_large_fails_cleanly() {
    let b = Reservation::new(8 * page_size());
    let m = b.meta();
    assert_eq!(reserve(&m, 9 * page_size(), page_size()), None);
    assert_eq!(reserve(&m, usize::MAX, page_size()), None);
    assert_eq!(reserve(&m, page_size(), 1 << 40), None);
    assert_eq!(m.stats(Tracked::assume_new()).reserved, 0);
    assert_eq!(m.top(), b.base + b.len);
    assert_eq!(reserve(&m, 8 * page_size(), page_size()), Some(b.base));
    assert_eq!(
        m.alloc(layout(1, 1), Tracked::assume_new()),
        ptr::null_mut()
    );
}

#[test]
#[should_panic(expected = "never allocated here")]
fn free_of_foreign_pointer_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let _ = m.alloc(layout(64, 8), Tracked::assume_new());
    let mut foreign = [0u8; 128];
    free(
        &m,
        align_up(foreign.as_mut_ptr() as usize, 64) as *mut u8,
        layout(64, 8),
    );
}

#[test]
#[should_panic(expected = "never allocated here")]
fn free_of_misaligned_pointer_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let p = m.alloc(layout(64, 8), Tracked::assume_new());
    // SAFETY: 8 bytes into the live 64-byte object `p`.
    free(&m, unsafe { p.add(8) }, layout(64, 8));
}

#[test]
#[should_panic(expected = "never allocated here")]
fn free_beyond_bump_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let p = m.alloc(layout(64, 8), Tracked::assume_new());
    // SAFETY: one past the end of the live 64-byte object `p`.
    free(&m, unsafe { p.add(64) }, layout(64, 8));
}

#[test]
#[should_panic(expected = "object too large")]
fn oversized_object_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let _ = m.alloc(layout(MAX_BINNED + 1, 8), Tracked::assume_new());
}

#[test]
#[should_panic(expected = "alignment too large")]
fn overaligned_object_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let _ = m.alloc(layout(64, 256), Tracked::assume_new());
}

#[test]
#[should_panic(expected = "commit outside")]
fn commit_outside_reservations_aborts() {
    // Several pages, so the one-page reservation leaves room below it.
    let b = Reservation::new(4 * page_size());
    let m = b.meta();
    let _ = reserve(&m, page_size(), page_size());
    let _ = commit(&m, b.base, page_size());
}

/// Random alloc/free against a model: no overlap, alignment, contents
/// survive, stats match, and freed space is reused rather than bumped.
#[test]
fn matches_model() {
    let b = Reservation::new(32 << 20);
    let m = b.meta();
    let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
    let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
    let mut expect_allocated = 0usize;
    for _ in 0..20_000 {
        let r = xorshift(&mut s);
        if live.is_empty() || (live.len() < 400 && r.is_multiple_of(2)) {
            let size = match xorshift(&mut s) % 10 {
                0 => (xorshift(&mut s) as usize) % (64 * 1024) + 1,
                1..=3 => (xorshift(&mut s) as usize) % 4096 + 1,
                _ => (xorshift(&mut s) as usize) % 256,
            };
            let align = 1usize << (xorshift(&mut s) % 7);
            let l = layout(size, align);
            let p = m.alloc(l, Tracked::assume_new());
            assert!(!p.is_null());
            let bin = BINS[bin_for(l).unwrap()] as usize;
            assert_eq!(p as usize % align, 0);
            assert!(
                b.base <= p as usize && p as usize + bin <= b.base + b.len,
                "object outside the region"
            );
            for (q, ql, _) in &live {
                let qb = BINS[bin_for(*ql).unwrap()] as usize;
                let (a0, a1) = (p as usize, p as usize + bin);
                let (b0, b1) = (*q as usize, *q as usize + qb);
                assert!(a1 <= b0 || b1 <= a0, "overlap");
            }
            let fill = (xorshift(&mut s) as u8) | 1;
            // SAFETY: `p` is a live object of at least `size` bytes, unaliased.
            unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
            live.push((p, l, fill));
            expect_allocated += bin;
        } else {
            let i = (xorshift(&mut s) as usize) % live.len();
            let (p, l, fill) = live.swap_remove(i);
            // SAFETY: `p` is live with `l.size()` bytes, filled at allocation.
            let slice = unsafe { core::slice::from_raw_parts(p, l.size()) };
            assert!(slice.iter().all(|&x| x == fill), "contents clobbered");
            free(&m, p, l);
            expect_allocated -= BINS[bin_for(l).unwrap()] as usize;
        }
        let st = m.stats(Tracked::assume_new());
        assert_eq!(st.allocated, expect_allocated);
        assert!(st.allocated <= st.bumped && st.bumped <= st.committed);
        assert!(st.committed <= st.len && st.committed.is_multiple_of(page_size()));
    }
    // Free everything, then the same sequence of layouts reuses memory.
    let layouts: Vec<Layout> = live.iter().map(|(_, l, _)| *l).collect();
    for (p, l, _) in live.drain(..) {
        free(&m, p, l);
    }
    assert_eq!(m.stats(Tracked::assume_new()).allocated, 0);
    let bumped = m.stats(Tracked::assume_new()).bumped;
    for l in &layouts {
        assert!(!m.alloc(*l, Tracked::assume_new()).is_null());
    }
    assert_eq!(
        m.stats(Tracked::assume_new()).bumped,
        bumped,
        "everything came from the free lists"
    );
}

/// Eight threads share one allocator; each checks its own objects' fill
/// patterns so any handout of the same block twice shows up.
#[test]
fn threads_get_disjoint_objects() {
    let b = Reservation::new(16 << 20);
    let m = b.meta();
    std::thread::scope(|sc| {
        for t in 0..8u64 {
            let m = &m;
            sc.spawn(move || {
                let mut s = 0x1234_5678 + t * 0x9E37_79B9;
                let mut live: Vec<(*mut u8, Layout)> = Vec::new();
                let fill = t as u8 + 1;
                for _ in 0..5_000 {
                    if live.len() < 64 && !xorshift(&mut s).is_multiple_of(3) {
                        let size = (xorshift(&mut s) as usize) % 2048 + 1;
                        let l = layout(size, 1 << (xorshift(&mut s) % 7));
                        let p = m.alloc(l, Tracked::assume_new());
                        assert!(!p.is_null());
                        // SAFETY: `p` is a live object of `size` bytes, owned by this thread.
                        unsafe { core::slice::from_raw_parts_mut(p, size) }.fill(fill);
                        live.push((p, l));
                    } else if let Some(i) = live.len().checked_sub(1) {
                        let (p, l) = live.swap_remove((xorshift(&mut s) as usize) % (i + 1));
                        // SAFETY: `p` is live with `l.size()` bytes, filled at allocation.
                        let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                        assert!(sl.iter().all(|&x| x == fill), "thread {t} block clobbered");
                        free(m, p, l);
                    }
                }
                for (p, l) in live {
                    // SAFETY: as above.
                    let sl = unsafe { core::slice::from_raw_parts(p, l.size()) };
                    assert!(sl.iter().all(|&x| x == fill));
                    free(m, p, l);
                }
            });
        }
    });
    assert_eq!(m.stats(Tracked::assume_new()).allocated, 0);
    assert!(!m.raw_lock().is_locked());
}

/// The `rw-p` ranges of `/proc/self/maps` inside `[base, base + len)`,
/// clipped to it (a committed range at either end merges with an
/// adjacent `rw-p` anonymous mapping of another test); everything else
/// in the region must be `---p`.
fn writable_ranges(base: usize, len: usize) -> Vec<(usize, usize)> {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let mut rw: Vec<(usize, usize)> = Vec::new();
    for line in maps.lines() {
        let mut it = line.split_whitespace();
        let range = it.next().unwrap();
        let perms = it.next().unwrap();
        let (lo, hi) = range.split_once('-').unwrap();
        let lo = usize::from_str_radix(lo, 16).unwrap();
        let hi = usize::from_str_radix(hi, 16).unwrap();
        if hi <= base || lo >= base + len {
            continue;
        }
        if perms.starts_with("rw") {
            rw.push((lo.max(base), hi.min(base + len)));
        } else {
            assert!(
                perms.starts_with("---"),
                "unexpected perms {perms} at {range}"
            );
        }
    }
    rw.sort_unstable();
    rw
}

/// A run of eager carves makes nothing writable (no system call) until
/// the first commit of one of them, which makes the object side
/// writable through all of them at once; an object allocated after
/// further carves does the same for those. The counters are the ones
/// an immediate commit would give.
#[test]
fn eager_carves_defer_their_commit_to_first_use() {
    let b = Reservation::new(64 << 20);
    let m = b.meta();
    m.set_eager_for_test(true);
    let mut tables = Vec::new();
    for i in 0..20usize {
        let len = (1 + i % 5) * page_size() + 100;
        tables.push((reserve_table(&m, len, page_size()).unwrap(), len));
    }
    assert_eq!(writable_ranges(b.base, b.len), vec![], "carves alone");
    let s = m.stats(Tracked::assume_new());
    let (last, last_len) = tables[19];
    assert_eq!(s.writable, align_up(last + last_len - b.base, chunk()));
    assert_eq!(s.committed, s.writable - s.eager, "the chunk slack only");
    commit(&m, tables[3].0, page_size()).unwrap();
    assert_eq!(
        writable_ranges(b.base, b.len),
        vec![(b.base, b.base + s.writable)],
        "one commit covers every table carved so far"
    );
    assert_eq!(
        m.stats(Tracked::assume_new()).committed,
        s.committed + page_size()
    );
    assert_eq!(m.stats(Tracked::assume_new()).writable, s.writable);
    for &(t, len) in &tables {
        // SAFETY: the last byte of a carved table, inside the commit asserted
        // above.
        unsafe { core::ptr::write_volatile((t + len - 1) as *mut u8, 1) };
    }
    // More carves, then an object past them: one commit again.
    let t = reserve_table(&m, EAGER_MAX, page_size()).unwrap();
    let before = m.stats(Tracked::assume_new());
    assert_eq!(writable_ranges(b.base, b.len).len(), 1);
    assert!(writable_ranges(b.base, b.len)[0].1 < t + EAGER_MAX);
    let o = m.alloc(layout(1000, 8), Tracked::assume_new());
    assert_eq!(o as usize, t + EAGER_MAX, "the object follows the table");
    let after = m.stats(Tracked::assume_new());
    assert_eq!(
        writable_ranges(b.base, b.len),
        vec![(b.base, b.base + after.writable)]
    );
    assert_eq!(
        after.committed - before.committed,
        after.writable - before.writable,
        "only a new chunk past the frontier counts"
    );
    // SAFETY: `t` is inside the writable frontier the object's allocation
    // moved past it (asserted above).
    unsafe { core::ptr::write_volatile(t as *mut u8, 1) };
}

/// On Linux the committed part of the region must show up as exactly one
/// `rw-p` mapping per committed range, and nothing else in the region
/// may be writable: the object side with a small table inside it is one
/// range, a page committed in a large reservation another.
#[test]
fn maps_show_commits() {
    let b = Reservation::new(64 << 20);
    let m = b.meta();
    m.set_eager_for_test(true);
    for _ in 0..10 {
        assert!(!m.alloc(layout(1000, 8), Tracked::assume_new()).is_null());
    }
    let small = m
        .reserve_committed(3 * page_size(), page_size(), Tracked::assume_new())
        .unwrap();
    assert_eq!(
        small,
        b.base + align_up(10 * 1024, page_size()),
        "page aligned, after the ten 1 KiB objects"
    );
    let big = reserve_table(&m, EAGER_MAX + page_size(), page_size()).unwrap();
    commit(&m, big + page_size(), page_size()).unwrap();
    // The object side's frontier: the chunks through the small table's
    // end (one chunk under 4 KiB pages, four pages under 64 KiB pages).
    let front = align_up(small + 3 * page_size() - b.base, chunk());
    let st = m.stats(Tracked::assume_new());
    assert_eq!(
        st.committed,
        front + page_size(),
        "the objects' chunks with the table in them, plus big's page"
    );
    assert_eq!(st.writable, front + page_size());
    assert_eq!(
        writable_ranges(b.base, b.len),
        vec![
            (b.base, b.base + front),
            (big + page_size(), big + 2 * page_size())
        ],
        "writable ranges in the region"
    );
    // The small table's pages are writable, the large table's
    // uncommitted pages are not (a write there faults: checked in a
    // child so the test survives).
    // SAFETY: `small` is a committed table of three pages.
    unsafe { core::ptr::write_volatile(small as *mut u8, 1) };
    // SAFETY: the test binary is multithreaded; the child takes no lock: it
    // writes one byte and leaves through `_exit`.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        // SAFETY: `big`'s first page is reserved but uncommitted: the write
        // faults by design (the parent expects `SIGSEGV`), in the child only.
        unsafe {
            core::ptr::write_volatile(big as *mut u8, 1);
            libc::_exit(0);
        }
    }
    let mut status = 0;
    // SAFETY: `pid` is this process's child; `status` is a local.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSEGV,
        "status {status:#x}"
    );
}

#[test]
#[should_panic(expected = "double free of the free list head")]
fn a_double_free_of_the_list_head_aborts() {
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let p = m.alloc(layout(64, 8), Tracked::assume_new());
    assert!(!p.is_null());
    free(&m, p, layout(64, 8));
    free(&m, p, layout(64, 8));
}

#[test]
fn a_free_below_the_list_head_is_accepted() {
    // Documents the limit of the check: only the head is compared.
    let b = Reservation::new(1 << 16);
    let m = b.meta();
    let p = m.alloc(layout(64, 8), Tracked::assume_new());
    let q = m.alloc(layout(64, 8), Tracked::assume_new());
    free(&m, p, layout(64, 8));
    free(&m, q, layout(64, 8));
    assert_eq!(m.alloc(layout(64, 8), Tracked::assume_new()), q);
    assert_eq!(m.alloc(layout(64, 8), Tracked::assume_new()), p);
}

/// Many small tables with objects between them are one `rw-p` mapping
/// from the base to the chunk frontier, and a forked child sees the same
/// one mapping: what a PostgreSQL backend inherits (the module doc).
#[test]
fn eager_tables_are_one_mapping_in_parent_and_child() {
    let b = Reservation::new(64 << 20);
    let m = b.meta();
    m.set_eager_for_test(true);
    let mut tables = Vec::new();
    for i in 0..40usize {
        assert!(
            !m.alloc(layout(64 + i * 16, 8), Tracked::assume_new())
                .is_null()
        );
        let len = (1 + i % 7) * page_size() + (i % 3) * 100;
        let t = reserve_table(
            &m,
            len,
            if i % 4 == 0 {
                16 * page_size()
            } else {
                page_size()
            },
        )
        .unwrap();
        commit(&m, t, page_size()).unwrap();
        // SAFETY: the first page of table `t` was just committed.
        unsafe { core::ptr::write_volatile(t as *mut u64, i as u64 + 1) };
        tables.push((t, len));
    }
    let s = m.stats(Tracked::assume_new());
    assert_eq!(s.reserved, 0, "nothing on the reservation side");
    assert_eq!(
        s.eager,
        tables
            .iter()
            .map(|&(_, l)| align_up(l, page_size()))
            .sum::<usize>()
    );
    let expect = vec![(b.base, b.base + s.writable)];
    assert_eq!(
        writable_ranges(b.base, b.len),
        expect,
        "one writable range for 40 tables and 40 objects"
    );
    // No hole: every byte from the base to the frontier is writable
    // (read back through a volatile touch of each page).
    for off in (0..s.writable).step_by(page_size()) {
        unsafe { core::ptr::read_volatile((b.base + off) as *const u8) };
    }
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let ok = writable_ranges(b.base, b.len) == expect
            && tables.iter().enumerate().all(
                |(i, &(t, _))| unsafe { core::ptr::read_volatile(t as *const u64) } == i as u64 + 1,
            );
        unsafe { libc::_exit(if ok { 0 } else { 1 }) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "child saw a different mapping"
    );
}
