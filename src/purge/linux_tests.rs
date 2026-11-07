//! Purge tests of what the kernel does with a discard: `Batch` frees every
//! range's pages (checked with `mincore`), a locked range is reported and
//! left alone, an unmapped or sealed range aborts, and refused merged
//! ranges cost few calls. Tests of the decay driver's logic belong in
//! `tests.rs`.

use super::*;
use crate::sys::{self, PROT_READ};
use crate::testutil::map_rw;

/// Resident pages of `[addr, addr + len)` via `mincore`.
fn resident(addr: usize, len: usize, page: usize) -> usize {
    let n = len / page;
    let mut vec = std::vec![0u8; n];
    // SAFETY: `vec` has one byte per page of the range; `mincore` only
    // writes it.
    let rc = unsafe { ::libc::mincore(addr as *mut _, len, vec.as_mut_ptr()) };
    assert_eq!(rc, 0);
    vec.iter().filter(|&&b| b & 1 != 0).count()
}

/// A batch discards every range and leaves the pages zero.
#[test]
fn batch_discards_every_range() {
    let _advice = super::tests::serial();
    let page = crate::sys::features::page_size();
    let len = 8 * page;
    let addr = map_rw(len);
    for p in 0..8 {
        // SAFETY: `addr..addr + len` is this test's read-write mapping.
        unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
    }
    assert_eq!(resident(addr, len, page), 8);
    let mut b = Batch::new_t();
    // Two ranges with a hole between them; the hole stays resident.
    b.push_t(addr, 3 * page, |_, _, done| assert!(done));
    b.push_t(addr + 4 * page, 4 * page, |_, _, done| assert!(done));
    b.flush_t(|_, _, done| assert!(done));
    assert_eq!(resident(addr, len, page), 1);
    // SAFETY: as above.
    assert_eq!(unsafe { *((addr + 3 * page) as *const u8) }, 0xA5);
    for p in [0usize, 1, 2, 4, 5, 6, 7] {
        assert_eq!(
            // SAFETY: as above.
            unsafe { *((addr + p * page) as *const u8) },
            0,
            "page {p} not zero"
        );
    }
    // SAFETY: the test's own mapping, unused after.
    unsafe { sys::munmap(addr, len) }.unwrap();
}

/// A range `madvise` refuses (an `mlock`ed page: `EINVAL` for both
/// advices) is reported not discarded and keeps its contents, counts in
/// `failed_discards` and not in the discarded bytes, and the batch's
/// other ranges still go; a batch with nothing discarded is not counted.
/// In a fork child: the lazy flush makes `purged_is_zero` false for the
/// process, and the counters move only by this test there.
#[test]
fn a_locked_range_is_reported_and_left_alone() {
    crate::testutil::in_fork_child("purge locked range", 30_000, || {
        super::tests::forget_other_threads_explicit_purges();
        let page = crate::sys::features::page_size();
        let len = 4 * page;
        let addr = map_rw(len);
        let locked = addr + 2 * page;
        if !crate::testutil::mlock_or_skip(
            "a_locked_range_is_reported_and_left_alone",
            locked,
            page,
        ) {
            return;
        }
        for lazy in [false, true] {
            set_headroom(lazy, false);
            assert_eq!(advice() == Advice::Lazy, lazy);
            for p in 0..4 {
                // SAFETY: page `p < 4` of the test's own read-write mapping.
                unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
            }
            let failed0 = failed_discards();
            let bytes0 = discarded_bytes();
            let batches0 = batches();
            let mut settled = std::vec::Vec::new();
            let mut b = Batch::new_t();
            b.push_t(addr, page, |_, _, _| unreachable!());
            b.push_t(locked, page, |_, _, _| unreachable!());
            b.flush_t(|a, l, done| settled.push((a, l, done)));
            assert_eq!(settled, [(addr, page, true), (locked, page, false)]);
            // SAFETY: `locked` is a mapped, locked page of that mapping.
            assert_eq!(unsafe { *(locked as *const u8) }, 0xA5);
            if !lazy {
                // SAFETY: `addr` is a mapped page of that mapping.
                assert_eq!(unsafe { *(addr as *const u8) }, 0);
            }
            assert_eq!(failed_discards(), (failed0.0 + 1, failed0.1 + page as u64));
            let (b1, d1) = (batches(), discarded_bytes());
            if lazy {
                assert_eq!((b1.0, d1.0), (batches0.0 + 1, bytes0.0 + page as u64));
                assert_eq!((b1.1, d1.1), (batches0.1, bytes0.1));
            } else {
                assert_eq!((b1.1, d1.1), (batches0.1 + 1, bytes0.1 + page as u64));
                assert_eq!((b1.0, d1.0), (batches0.0, bytes0.0));
            }
            // Only the locked range: nothing discarded, no batch counted.
            settled.clear();
            b.push_t(locked, page, |_, _, _| unreachable!());
            b.flush_t(|a, l, done| settled.push((a, l, done)));
            assert_eq!(settled, [(locked, page, false)]);
            assert_eq!((batches(), discarded_bytes()), (b1, d1));
            assert_eq!(
                failed_discards(),
                (failed0.0 + 2, failed0.1 + 2 * page as u64)
            );
            assert!(!discard_one(locked, page, Ghost::assume_new()));
            assert!(discard_one(addr + 3 * page, page, Ghost::assume_new()));
            // Four one-page pieces merged: refused whole, then issued
            // piece by piece, only the locked one failing.
            let failed1 = failed_discards();
            settled.clear();
            for p in 0..4 {
                b.push_t(addr + p * page, page, |_, _, _| unreachable!());
            }
            assert_eq!(b.len(), 1);
            b.flush_t(|a, l, done| settled.push((a, l, done)));
            let want: std::vec::Vec<_> = (0..4).map(|p| (addr + p * page, page, p != 2)).collect();
            assert_eq!(settled, want);
            assert_eq!(failed_discards(), (failed1.0 + 1, failed1.1 + page as u64));
        }
        set_headroom(false, false);
        // SAFETY: `addr..addr + len` is the test's own mapping, unused after.
        unsafe { sys::munmap(addr, len) }.unwrap();
    });
}

/// A range that is not mapped (`ENOMEM`: a reservation of ours is gone)
/// is a heap inconsistency, not a refusal: the flush aborts. In a fork
/// child, so no other test maps the hole between the `munmap` and the
/// discard.
#[test]
fn an_unmapped_range_aborts() {
    crate::testutil::in_fork_child("purge unmapped range", 30_000, || {
        super::tests::forget_other_threads_explicit_purges();
        set_headroom(false, false);
        let page = crate::sys::features::page_size();
        let addr = map_rw(page);
        // SAFETY: the page just mapped, which nothing else uses.
        unsafe { sys::munmap(addr, page) }.unwrap();
        let r = std::panic::catch_unwind(|| discard_one(addr, page, Ghost::assume_new()));
        let msg = *r.unwrap_err().downcast::<std::string::String>().unwrap();
        assert!(msg.contains("purge: madvise("), "{msg}");
        assert!(msg.ends_with("ENOMEM"), "{msg}");
    });
}

/// Any errno but `EINVAL` (the refusal of locked pages) aborts:
/// here `EPERM`, `MADV_DONTNEED` on a sealed read-only range (Linux 6.10
/// and later; a skip without `mseal`). In a fork child: the sealed page
/// can never be unmapped.
#[test]
fn a_sealed_range_aborts() {
    crate::testutil::in_fork_child("purge sealed range", 30_000, || {
        super::tests::forget_other_threads_explicit_purges();
        set_headroom(false, false);
        let page = crate::sys::features::page_size();
        let addr = map_rw(page);
        // SAFETY: `addr` is that read-write page.
        unsafe { *(addr as *mut u8) = 0xA5 };
        // SAFETY: the test's own page; only its protection changes.
        unsafe { sys::mprotect(addr, page, PROT_READ) }.unwrap();
        // SAFETY: as above; sealing a page this child owns affects nothing
        // else.
        if let Err(e) = unsafe { sys::mseal(addr, page, 0) } {
            crate::testutil::skip("a_sealed_range_aborts", &std::format!("mseal failed: {e}"));
            return;
        }
        let r = std::panic::catch_unwind(|| discard_one(addr, page, Ghost::assume_new()));
        let msg = *r.unwrap_err().downcast::<std::string::String>().unwrap();
        assert!(msg.contains("purge: madvise("), "{msg}");
        assert!(msg.ends_with("EPERM"), "{msg}");
        // SAFETY: the sealed page stays mapped and readable.
        assert_eq!(unsafe { *(addr as *const u8) }, 0xA5);
    });
}

/// The `madvise` calls a refused merged range of 16 one-page pieces costs
/// with `locked` pages `mlock`ed, and which pieces it reports discarded.
fn merged_flush(addr: usize, page: usize, locked: &[usize]) -> (u64, std::vec::Vec<bool>) {
    const PIECES: usize = 16;
    for &p in locked {
        if !crate::testutil::mlock_or_skip(
            "refused_merged_ranges_cost_few_calls",
            addr + p * page,
            page,
        ) {
            return (0, std::vec::Vec::new());
        }
    }
    let mut b = Batch::new_t();
    for p in 0..PIECES {
        b.push_t(addr + p * page, page, |_, _, _| unreachable!());
    }
    assert_eq!(b.len(), 1);
    let mut done = std::vec::Vec::new();
    let calls0 = DISCARD_CALLS.load(Ordering::Relaxed);
    b.flush_t(|a, l, d| {
        // In address order, a range reported whole marking each piece.
        assert_eq!(a, addr + done.len() * page);
        done.extend((0..l / page).map(|_| d));
    });
    let calls = DISCARD_CALLS.load(Ordering::Relaxed) - calls0;
    assert_eq!(
        // SAFETY: unlocking the caller's mapping; no memory is touched.
        unsafe { ::libc::munlock(addr as *const _, PIECES * page) },
        0
    );
    (calls, done)
}

/// A refused merged range is issued again piece by piece, starting with
/// its ends: all locked (`mlockall`), it costs 3 calls, not one per
/// piece; one locked page keeps only its own piece dirty; locked at both
/// ends, the middle is left dirty uncalled (the trade-off, `Batch`'s
/// doc). In a fork child: the call counter moves only by this test.
#[test]
fn refused_merged_ranges_cost_few_calls() {
    crate::testutil::in_fork_child("purge merged refusals", 30_000, || {
        super::tests::forget_other_threads_explicit_purges();
        set_headroom(false, false);
        let page = crate::sys::features::page_size();
        let addr = map_rw(16 * page);
        let only = |locked: &[usize]| {
            (0..16)
                .map(|p| !locked.contains(&p))
                .collect::<std::vec::Vec<_>>()
        };
        let all: std::vec::Vec<usize> = (0..16).collect();
        let (calls, done) = merged_flush(addr, page, &all);
        if done.is_empty() {
            return;
        }
        assert_eq!((calls, done), (3, only(&all)));
        for locked in [&[7usize][..], &[0], &[15]] {
            assert_eq!(merged_flush(addr, page, locked), (17, only(locked)));
        }
        assert_eq!(merged_flush(addr, page, &[0, 15]), (3, only(&all)));
        assert_eq!(merged_flush(addr, page, &[]), (1, only(&[])));
        // SAFETY: the test's own 16-page mapping, unused after.
        unsafe { sys::munmap(addr, 16 * page) }.unwrap();
    });
}
