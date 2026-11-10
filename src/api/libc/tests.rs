use super::*;
use crate::layout::{MEDIUM_MAX, class_size_of};
use std::process::Command;
use std::string::String;
use std::vec::Vec;

fn errno() -> c_int {
    // SAFETY: `__errno_location` returns this thread's `errno`.
    unsafe { *::libc::__errno_location() }
}

fn fill(p: *mut u8, n: usize, seed: u8) {
    for i in 0..n {
        // SAFETY: the callers pass `n` writable bytes at `p`.
        unsafe { *p.add(i) = seed.wrapping_add(i as u8) };
    }
}

fn check(p: *mut u8, n: usize, seed: u8) {
    for i in 0..n {
        // SAFETY: the callers pass `n` readable bytes at `p`.
        assert_eq!(unsafe { *p.add(i) }, seed.wrapping_add(i as u8), "byte {i}");
    }
}

#[test]
fn malloc_free_round_trip_every_tier() {
    for &n in &[
        0usize,
        1,
        7,
        16,
        17,
        100,
        1024,
        1025,
        4096,
        65536,
        MEDIUM_MAX,
        MEDIUM_MAX + 1,
        1 << 20,
        10 << 20,
    ] {
        let p = malloc(
            n,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null(), "malloc({n})");
        assert_eq!(p as usize % QUANTUM, 0);
        let usable = malloc_usable_size(p, vstd::prelude::Tracked::assume_new());
        assert!(usable >= n, "usable {usable} < {n}");
        match class_size_of(n) {
            Some(s) => assert_eq!(usable, s),
            None => assert_eq!(
                usable,
                super::super::shared(Tracked::assume_new())
                    .huge
                    .usable_for(n)
                    .unwrap()
            ),
        }
        fill(p, usable, 3);
        check(p, usable, 3);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    assert_eq!(
        malloc_usable_size(ptr::null_mut(), vstd::prelude::Tracked::assume_new()),
        0
    );
    free(
        ptr::null_mut(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

#[test]
fn malloc_zero_returns_distinct_pointers() {
    let a = malloc(
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let b = malloc(
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!a.is_null() && !b.is_null() && a != b);
    free(
        a,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    free(
        b,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

#[test]
fn calloc_zeroes_reused_slots_and_huge_runs() {
    for &n in &[24usize, 1000, 5000, MEDIUM_MAX + 1, 3 << 20] {
        // Dirty a block, free it, then calloc the same size: the reused
        // memory must read as zero.
        let p = malloc(
            n,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        fill(p, n, 0xAB);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let q = calloc(
            1,
            n,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!q.is_null());
        for i in 0..n {
            // SAFETY: `q` is a live `n`-byte allocation.
            assert_eq!(unsafe { *q.add(i) }, 0, "calloc({n}) byte {i} not zero");
        }
        free(
            q,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        calloc(
            usize::MAX,
            2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
}

#[test]
fn realloc_moves_and_preserves_content() {
    // Grow through the tiers and back down, checking content each step.
    let sizes = [
        10usize,
        100,
        1000,
        10_000,
        100_000,
        MEDIUM_MAX + 1,
        2 << 20,
        100_000,
        1000,
        10,
    ];
    let mut p = malloc(
        sizes[0],
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    fill(p, sizes[0], 1);
    let mut prev = sizes[0];
    for &n in &sizes[1..] {
        p = realloc(
            p,
            n,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null(), "realloc to {n}");
        check(p, prev.min(n), 1);
        fill(p, n, 1);
        prev = n;
    }
    // Same class: same pointer.
    let q = realloc(
        p,
        prev,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(q, p);
    let q = realloc(
        p,
        prev + 1,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(q, p, "10 -> 11 stays in the 16-byte class");
    assert!(
        realloc(
            p,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null(),
        "realloc(p, 0) frees and returns null"
    );

    // Huge: same number of pages keeps the pointer.
    let page = super::super::shared(Tracked::assume_new())
        .regions
        .page_size();
    let h = realloc(
        ptr::null_mut(),
        MEDIUM_MAX + 1,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        realloc(
            h,
            MEDIUM_MAX + page,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        h
    );
    let h2 = realloc(
        h,
        MEDIUM_MAX + page + 1,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_ne!(h2, h);
    free(
        h2,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );

    assert!(
        reallocarray(
            ptr::null_mut(),
            usize::MAX,
            2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
    let r = reallocarray(
        ptr::null_mut(),
        8,
        8,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(malloc_usable_size(r, vstd::prelude::Tracked::assume_new()) >= 64);
    free(
        r,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

/// `realloc(p, malloc_usable_size(p))` keeps `p` for every slot class
/// and for runs above `MEDIUM_MAX` (`keeps`, `lemma_keeps_usable`),
/// and a slot is kept for the smallest and the largest size of its
/// class.
#[test]
fn realloc_to_usable_size_keeps() {
    for c in 0..crate::layout::NCLASSES {
        let s = class_size(c);
        let p = malloc(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(
            malloc_usable_size(p, vstd::prelude::Tracked::assume_new()),
            s
        );
        assert_eq!(
            realloc(
                p,
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            p,
            "class {c} ({s} bytes)"
        );
        let below = if c == 0 { 1 } else { class_size(c - 1) + 1 };
        assert_eq!(
            realloc(
                p,
                below,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            p,
            "class {c} from {below} bytes"
        );
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    let page = super::super::shared(Tracked::assume_new())
        .regions
        .page_size();
    for n in [MEDIUM_MAX + 1, MEDIUM_MAX + page + 1, 3 << 20] {
        let p = malloc(
            n,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let u = malloc_usable_size(p, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            realloc(
                p,
                u,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            p,
            "run of {n} bytes"
        );
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
}

#[test]
fn aligned_allocations_honour_every_power_of_two() {
    let mut align = QUANTUM;
    while align <= 4 << 20 {
        for &size in &[
            1usize,
            align - 1,
            align,
            align + 1,
            3 * align,
            MEDIUM_MAX + 1,
        ] {
            let p = aligned_alloc(
                align,
                size,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
            assert!(!p.is_null(), "aligned_alloc({align}, {size})");
            assert_eq!(
                p as usize % align,
                0,
                "aligned_alloc({align}, {size}) = {p:p}"
            );
            assert!(malloc_usable_size(p, vstd::prelude::Tracked::assume_new()) >= size);
            fill(p, size, 9);
            check(p, size, 9);
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );

            let mut out = ptr::null_mut();
            assert_eq!(
                posix_memalign(
                    &mut out,
                    align,
                    size,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new()
                ),
                0
            );
            assert_eq!(out as usize % align, 0);
            free(
                out,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        align <<= 1;
    }
}

#[test]
fn aligned_allocation_picks_the_smallest_fitting_class() {
    // 48-byte request at 32-byte alignment: class 48 slots are only
    // 16-aligned, so class 64 is used.
    let p = aligned_alloc(
        32,
        48,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        malloc_usable_size(p, vstd::prelude::Tracked::assume_new()),
        64
    );
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // Alignment already implied by the quantum: plain class.
    let p = aligned_alloc(
        QUANTUM,
        48,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        malloc_usable_size(p, vstd::prelude::Tracked::assume_new()),
        class_size_of(48).unwrap()
    );
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // 4 KiB alignment for 5 KiB: the 6 KiB class (5120 + 1024) is not a
    // multiple of 4 KiB; 8 KiB is.
    let p = aligned_alloc(
        4096,
        5 * 1024,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        malloc_usable_size(p, vstd::prelude::Tracked::assume_new()),
        8192
    );
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

#[test]
fn bad_alignment_is_einval_and_exhaustion_is_enomem() {
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        aligned_alloc(
            0,
            16,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), EINVAL);
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        aligned_alloc(
            24,
            16,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), EINVAL);

    let mut out = ptr::null_mut::<u8>();
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert_eq!(
        posix_memalign(
            &mut out,
            3,
            16,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        EINVAL
    );
    assert_eq!(
        posix_memalign(
            &mut out,
            4,
            16,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        EINVAL,
        "below sizeof(void *)"
    );
    assert_eq!(errno(), 0, "posix_memalign leaves errno alone");
    assert!(out.is_null());

    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        malloc(
            usize::MAX / 2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        realloc(
            ptr::null_mut(),
            usize::MAX / 2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert_eq!(
        posix_memalign(
            &mut out,
            16,
            usize::MAX / 2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        ENOMEM
    );
    set_errno(0, vstd::prelude::Tracked::assume_new());
    assert!(
        pvalloc(
            usize::MAX,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
}

#[test]
fn valloc_and_pvalloc_are_page_aligned() {
    let page = super::super::shared(Tracked::assume_new())
        .regions
        .page_size();
    let p = valloc(
        10,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(p as usize % page, 0);
    assert!(malloc_usable_size(p, vstd::prelude::Tracked::assume_new()) >= 10);
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let p = pvalloc(
        page + 1,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(p as usize % page, 0);
    assert!(malloc_usable_size(p, vstd::prelude::Tracked::assume_new()) >= 2 * page);
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let p = pvalloc(
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(malloc_usable_size(p, vstd::prelude::Tracked::assume_new()) >= page);
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

/// Design 13.7: the size-returning entry points answer exactly what
/// `malloc_usable_size` does, on every tier, and free what they made.
#[test]
fn usable_entry_points_agree_with_malloc_usable_size_on_every_tier() {
    for &n in &[
        0usize,
        1,
        7,
        16,
        17,
        100,
        1024,
        1025,
        4096,
        65536,
        MEDIUM_MAX,
        MEDIUM_MAX + 1,
        1 << 20,
        10 << 20,
    ] {
        let mut u = usize::MAX;
        let p = malloc_usable(
            n,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null(), "malloc_usable({n})");
        assert_eq!(p as usize % QUANTUM, 0);
        assert_eq!(
            u,
            malloc_usable_size(p, vstd::prelude::Tracked::assume_new()),
            "malloc_usable({n})"
        );
        assert!(u >= n);
        match class_size_of(n) {
            Some(s) => assert_eq!(u, s),
            None => assert_eq!(
                u,
                super::super::shared(Tracked::assume_new())
                    .huge
                    .usable_for(n)
                    .unwrap()
            ),
        }
        fill(p, u, 3);
        check(p, u, 3);
        assert_eq!(
            free_usable(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            u,
            "free_usable({n})"
        );

        // calloc_usable: the whole usable size reads as zero, even on a
        // slot or run that was dirtied and freed a moment ago.
        let d = malloc(
            n.max(1),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        fill(
            d,
            malloc_usable_size(d, vstd::prelude::Tracked::assume_new()),
            0xAB,
        );
        free(
            d,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let mut u = usize::MAX;
        let q = calloc_usable(
            1,
            n,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!q.is_null(), "calloc_usable({n})");
        assert_eq!(
            u,
            malloc_usable_size(q, vstd::prelude::Tracked::assume_new())
        );
        for i in 0..u {
            assert_eq!(
                // SAFETY: `i < u`, the usable size of the live `q`.
                unsafe { *q.add(i) },
                0,
                "calloc_usable({n}) byte {i} of {u} not zero"
            );
        }
        fill(q, u, 4);

        // realloc_usable: in place within the class (same pointer,
        // same size; `realloc_usable(q, 0)` would free, so 0 asks for
        // 1, still class 0), a move across tiers keeps the content.
        let mut u2 = usize::MAX;
        let r = realloc_usable(
            q,
            n.max(1),
            &mut u2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(
            r, q,
            "realloc_usable({n}) to the same size keeps the pointer"
        );
        assert_eq!(u2, u);
        let grown = 2 * n + 4097;
        let mut u3 = usize::MAX;
        let g = realloc_usable(
            r,
            grown,
            &mut u3,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!g.is_null());
        assert_eq!(
            u3,
            malloc_usable_size(g, vstd::prelude::Tracked::assume_new())
        );
        assert!(u3 >= grown);
        check(g, u.min(grown), 4);
        assert_eq!(
            free_usable(
                g,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            u3
        );
    }
}

/// The null and failure cases of design 13.7: a null `usable` makes the
/// three plain `malloc`/`calloc`/`realloc`; `free_usable(NULL)` is 0;
/// `realloc_usable(NULL, n)` allocates and `realloc_usable(p, 0)` frees
/// and reports 0; a failed allocation returns null with `errno =
/// ENOMEM` and `*usable = 0`, and a failed `realloc_usable` leaves the
/// block intact.
#[test]
fn usable_entry_points_null_and_failure_cases() {
    assert_eq!(
        free_usable(
            ptr::null_mut(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        0
    );
    let p = malloc_usable(
        100,
        ptr::null_mut(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    let q = calloc_usable(
        2,
        50,
        ptr::null_mut(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!q.is_null() && is_zero(q, 100));
    let r = realloc_usable(
        q,
        5000,
        ptr::null_mut(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!r.is_null());
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    free(
        r,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );

    let mut u = usize::MAX;
    let p = realloc_usable(
        ptr::null_mut(),
        300,
        &mut u,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    assert_eq!(
        u,
        malloc_usable_size(p, vstd::prelude::Tracked::assume_new())
    );
    fill(p, 300, 8);
    set_errno(0, vstd::prelude::Tracked::assume_new());
    let mut u2 = usize::MAX;
    assert!(
        realloc_usable(
            p,
            usize::MAX / 2,
            &mut u2,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!(errno(), ENOMEM);
    assert_eq!(u2, 0);
    check(p, 300, 8);
    let mut u3 = usize::MAX;
    assert!(
        realloc_usable(
            p,
            0,
            &mut u3,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null(),
        "realloc_usable(p, 0) frees and returns null"
    );
    assert_eq!(u3, 0);

    set_errno(0, vstd::prelude::Tracked::assume_new());
    let mut u = usize::MAX;
    assert!(
        malloc_usable(
            usize::MAX / 2,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null()
    );
    assert_eq!((errno(), u), (ENOMEM, 0));
    set_errno(0, vstd::prelude::Tracked::assume_new());
    let mut u = usize::MAX;
    assert!(
        calloc_usable(
            usize::MAX,
            2,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null(),
        "overflow"
    );
    assert_eq!((errno(), u), (ENOMEM, 0));
    set_errno(0, vstd::prelude::Tracked::assume_new());
    let mut u = usize::MAX;
    assert!(
        calloc_usable(
            1,
            usize::MAX / 2,
            &mut u,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        )
        .is_null(),
        "exhaustion"
    );
    assert_eq!((errno(), u), (ENOMEM, 0));

    // Aligned blocks report their class, not the request.
    let a = aligned_alloc(
        4096,
        5 * 1024,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        free_usable(
            a,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        8192
    );
    // Off the fast path: a full cache and a huge run.
    let many: Vec<*mut u8> = (0..3000)
        .map(|_| {
            malloc(
                64,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        })
        .collect();
    for p in many {
        assert_eq!(
            free_usable(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new()
            ),
            64
        );
    }
    let h = malloc(
        3 << 20,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        free_usable(
            h,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new()
        ),
        malloc_usable_size_of_huge(3 << 20)
    );
}

fn malloc_usable_size_of_huge(n: usize) -> usize {
    super::super::shared(Tracked::assume_new())
        .huge
        .usable_for(n)
        .unwrap()
}

fn is_zero(p: *mut u8, n: usize) -> bool {
    // SAFETY: the callers pass `n` readable bytes at `p`.
    (0..n).all(|i| unsafe { *p.add(i) } == 0)
}

#[test]
fn free_preserves_errno() {
    let p = malloc(
        100,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let h = malloc(
        1 << 20,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    set_errno(1234, vstd::prelude::Tracked::assume_new());
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    free(
        h,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // Overflow a cache so the flush path runs too.
    let many: Vec<*mut u8> = (0..1000)
        .map(|_| {
            malloc(
                64,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        })
        .collect();
    set_errno(1234, vstd::prelude::Tracked::assume_new());
    for p in many {
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    assert_eq!(errno(), 1234);
}

/// `malloc_trim` answers 1 when it purged something and 0 when there
/// was nothing to purge: a freed huge run is `Empty` until a purge
/// takes it, so the call right after its free says 1, and a call with
/// nothing `Empty` anywhere says 0. Other tests of this binary free and
/// purge concurrently, so each answer is looked for over a few
/// attempts rather than asserted of one call.
#[test]
fn malloc_trim_reports_whether_anything_was_purged() {
    // A background thread at a 1 ms decay would purge the run first.
    let _serial = crate::testutil::global_state();
    let purged = (0..8).any(|_| {
        let h = malloc(
            4 << 20,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!h.is_null());
        // SAFETY: `h` is a live 4 MiB allocation.
        unsafe { ptr::write_bytes(h, 1, 4 << 20) };
        free(
            h,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        malloc_trim(
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        ) == 1
    });
    assert!(purged, "malloc_trim never reported the freed run purged");
    let quiet = (0..32).any(|_| {
        malloc_trim(
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        ) == 0
    });
    assert!(quiet, "malloc_trim never found nothing to purge");
}

#[test]
fn cross_thread_free_and_thread_exit_with_full_cache() {
    let blocks: Vec<usize> = (0..4096)
        .map(|i| {
            malloc(
                32 + (i % 8) * 16,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            ) as usize
        })
        .collect();
    let handles: Vec<_> = blocks
        .chunks(512)
        .map(|chunk| {
            let chunk: Vec<usize> = chunk.to_vec();
            std::thread::spawn(move || {
                for &p in &chunk {
                    free(
                        p as *mut u8,
                        vstd::prelude::Tracked::assume_new(),
                        vstd::prelude::Tracked::assume_new(),
                        vstd::prelude::Tracked::assume_new(),
                    );
                }
                // Leave with plenty in this thread's caches.
                let mine: Vec<*mut u8> = (0..300)
                    .map(|_| {
                        malloc(
                            48,
                            vstd::prelude::Tracked::assume_new(),
                            vstd::prelude::Tracked::assume_new(),
                        )
                    })
                    .collect();
                for p in mine {
                    free(
                        p,
                        vstd::prelude::Tracked::assume_new(),
                        vstd::prelude::Tracked::assume_new(),
                        vstd::prelude::Tracked::assume_new(),
                    );
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn shared_object_exports_every_libc_entry_point() {
    let so = crate::api::c_programs::release_so();
    let out = Command::new("readelf")
        .args(["--dyn-syms", "-W", &so])
        .output()
        .expect("readelf");
    assert!(out.status.success());
    let syms = String::from_utf8(out.stdout).unwrap();
    for name in [
        "malloc",
        "free",
        "calloc",
        "realloc",
        "reallocarray",
        "aligned_alloc",
        "memalign",
        "posix_memalign",
        "valloc",
        "pvalloc",
        "free_sized",
        "free_aligned_sized",
        "malloc_usable_size",
    ] {
        assert!(
            syms.lines().any(|l| l.contains(" FUNC ")
                && l.contains(" GLOBAL ")
                && l.ends_with(&std::format!(" {name}"))),
            "{name} not exported as a global function:\n{syms}"
        );
    }
}

/// The pin of `malloc` and `free` (the `global_asm!`s of `exports`)
/// holds in the shipped library: in `nm -D` each entry sits at
/// `MALLOC_OFF` / `FREE_OFF` mod 64, 0/0 on x86_64 and 16/28 on
/// aarch64. The pin depends on the release profile (fat LTO, one
/// codegen unit) keeping each entry in its own section, so a profile
/// change can silently undo it; this test catches that.
#[test]
fn malloc_and_free_entries_are_pinned() {
    let so = crate::api::c_programs::release_so();
    let out = Command::new("nm")
        .args(["-D", "--defined-only", &so])
        .output()
        .expect("nm");
    assert!(out.status.success(), "nm -D {so} failed");
    let syms = String::from_utf8(out.stdout).unwrap();
    for (name, want) in [("malloc", super::MALLOC_OFF), ("free", super::FREE_OFF)] {
        let addr = syms
            .lines()
            .find_map(|l| {
                let mut f = l.split_whitespace();
                let (a, _, n) = (f.next()?, f.next()?, f.next()?);
                (n.split('@').next() == Some(name))
                    .then(|| usize::from_str_radix(a, 16).ok())
                    .flatten()
            })
            .unwrap_or_else(|| panic!("{name} not in nm -D of {so}:\n{syms}"));
        std::eprintln!("entry {name} at {addr:#x}: {} mod 64", addr % 64);
        assert_eq!(
            addr % 64,
            want,
            "{name} at {addr:#x} is not at {want} mod 64 in {so}: the entry pin \
             (src/api/libc.rs `exports`) no longer holds"
        );
    }
}
