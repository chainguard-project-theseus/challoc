use super::tests::{Env, guard_mode, page_size};
use super::*;
use std::vec::Vec;

fn fill(p: *mut u8, len: usize, v: u8) {
    unsafe { ptr::write_bytes(p, v, len) };
}

fn all(p: *mut u8, len: usize, v: u8) -> bool {
    let s = unsafe { core::slice::from_raw_parts(p, len) };
    s.iter().all(|&b| b == v)
}

/// `check_zero` on a healthy huge heap: fresh
/// and purged runs read zero, dirty `Empty` reuse is not checked (it is
/// not reported zero), and the flag stays on for the rest of the process.
#[test]
fn check_zero_passes_fresh_and_purged_runs() {
    purge::set_check_zero(true);
    assert!(purge::check_zero());
    let env = Env::new();
    let h = env.huge();
    let size = 6 * page_size();
    let (p, zero) = env.alloc(&h, size, 1);
    assert!(zero);
    fill(p, size, 0xAB);
    env.free(&h, p);
    let (q, zero) = env.alloc(&h, size, 1);
    assert_eq!(q, p);
    assert!(!zero);
    env.free(&h, q);
    assert_eq!(h.purge_empty(u64::MAX), 1);
    let (r, zero) = env.alloc(&h, size, 1);
    assert_eq!(r, p);
    assert!(zero && all(r, size, 0));
    env.free(&h, r);
}

/// `check_zero` fires on a purged run whose last usable page was
/// dirtied behind the allocator's back, naming the `Purged` pages.
#[test]
#[should_panic(expected = "was handed out as zero from Purged pages but the word at")]
fn check_zero_aborts_on_a_dirty_purged_run() {
    purge::set_check_zero(true);
    let env = Env::new();
    let h = env.huge();
    let size = 3 * page_size();
    let (p, _) = env.alloc(&h, size, 1);
    env.free(&h, p);
    assert_eq!(h.purge_empty(u64::MAX), 1);
    unsafe { ptr::write_volatile(p.add(size - 8) as *mut u64, 0xDEAD_BEEF) };
    let _ = env.alloc(&h, size, 1);
}

#[test]
fn memory_is_writable_and_the_zero_flag_is_honest() {
    let env = Env::new();
    let h = env.huge();
    let size = 10 * page_size();
    let (p, zero) = env.alloc(&h, size, 1);
    assert!(zero && all(p, size, 0));
    fill(p, size, 0xAB);
    env.free(&h, p);
    // Reused while Empty: dirty, and the flag says so.
    let (q, zero) = env.alloc(&h, size, 1);
    assert_eq!(q, p);
    assert!(!zero);
    assert!(all(q, size, 0xAB));
    env.free(&h, q);
    assert_eq!(h.purge_empty(u64::MAX), 1);
    let (r, zero) = env.alloc(&h, size, 1);
    assert_eq!(r, p);
    assert!(zero && all(r, size, 0));
    env.free(&h, r);
    // Above retain_max the purge is immediate.
    h.set_retain_max(size - 1);
    let (s, _) = env.alloc(&h, size, 1);
    fill(s, size, 0xCD);
    env.free(&h, s);
    assert_eq!(h.lookup(env.page_of(s)).unwrap().state, RunState::Purged);
    let (t, zero) = env.alloc(&h, size, 1);
    assert!(zero && all(t, size, 0));
    env.free(&h, t);
}

#[test]
fn stale_guards_inside_a_reused_extent_are_writable_and_purgeable() {
    let env = Env::new();
    let h = env.huge();
    let mut ps = Vec::new();
    for _ in 0..4 {
        let (p, _) = env.alloc(&h, 3 * page_size(), 1);
        fill(p, 3 * page_size(), 0x11);
        ps.push(p);
    }
    for p in &ps {
        env.free(&h, *p);
    }
    assert_eq!(h.stats().empty, 1);
    // One extent of 16 pages with three old guard positions inside:
    // purging it must succeed whatever their protection is.
    assert_eq!(h.purge_empty(u64::MAX), 1);
    let (big, zero) = env.alloc(&h, 15 * page_size(), 1);
    assert_eq!(big, ps[0]);
    assert!(zero && all(big, 15 * page_size(), 0));
    fill(big, 15 * page_size(), 0x22);
    assert!(all(big, 15 * page_size(), 0x22));
    env.free(&h, big);
    // And an Empty extent with stale guards can be re-split.
    let (a, zero) = env.alloc(&h, 5 * page_size(), 1);
    assert!(!zero);
    fill(a, 5 * page_size(), 0x33);
    let (b, _) = env.alloc(&h, 2 * page_size(), 1);
    fill(b, 2 * page_size(), 0x44);
    assert!(all(a, 5 * page_size(), 0x33));
    env.free(&h, a);
    env.free(&h, b);
}

/// The guard page after a run faults, in both guard modes.
#[test]
fn guard_page_faults() {
    let env = Env::new();
    let h = env.huge();
    let (p, _) = env.alloc(&h, 2 * page_size(), 1);
    let (q, _) = env.alloc(&h, page_size(), 1);
    // The guard is the page right before `q`.
    assert_eq!(q as usize, p as usize + 3 * page_size());
    let guard = p as usize + 2 * page_size();
    if guard_mode() {
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let merged = maps.lines().any(|l| {
            let range = l.split_whitespace().next().unwrap();
            let (a, b) = range.split_once('-').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let b = usize::from_str_radix(b, 16).unwrap();
            a <= p as usize && q as usize + page_size() <= b
        });
        assert!(merged, "guard page split the VMA in guard_install mode");
    }
    for (what, addr) in [
        ("guard", guard),
        ("first page past the mark", q as usize + 2 * page_size()),
    ] {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                core::ptr::write_volatile(addr as *mut u8, 1);
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFSIGNALED(status),
            "{what}: child did not fault: status {status:#x}"
        );
        assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV, "{what}");
    }
    // Freeing removes the guard: the page is reusable as run memory.
    env.free(&h, p);
    env.free(&h, q);
    let (r, _) = env.alloc(&h, 4 * page_size(), 1);
    assert_eq!(r, p);
    fill(r, 4 * page_size(), 0x55);
    assert!(all(r, 4 * page_size(), 0x55));
    env.free(&h, r);
}

#[test]
fn aligned_runs_are_usable_end_to_end() {
    let env = Env::new();
    let h = env.huge();
    let mut held = Vec::new();
    for lg in 13..=21 {
        let align = 1usize << lg;
        let size = 3 * page_size() + 100;
        let (p, zero) = env.alloc(&h, size, align);
        assert!(!p.is_null() && (p as usize).is_multiple_of(align));
        assert!(zero && all(p, 4 * page_size(), 0));
        fill(p, 4 * page_size(), lg as u8);
        held.push((p, lg as u8));
    }
    for &(p, v) in &held {
        assert!(all(p, 4 * page_size(), v));
    }
    for (p, _) in held {
        env.free(&h, p);
    }
    assert_eq!(h.stats().runs, 0);
}

/// Alignment holes: in guard-install mode `prepare` must commit the
/// alignment hole `[mark, s)` that `take` records as a free extent, not
/// only `[max(s, mark), end)`, or the hole stays `PROT_NONE` and the
/// first run refilled into it faults. Both hole
/// shapes: a hole between the mark and an aligned run on fresh pages
/// (`ext` null in `take`), and a hole folded into an `Empty` extent that
/// touched the mark (`s > e_end`). Every filler is written end to end.
#[test]
fn alignment_holes_are_committed_and_writable() {
    let env = Env::new();
    let h = env.huge();
    let mut held = Vec::new();
    // Shape 1: fresh pages above the mark.
    let (a, _) = env.alloc(&h, page_size(), 1);
    fill(a, page_size(), 1);
    let (b, _) = env.alloc(&h, page_size(), 1 << 20);
    fill(b, page_size(), 2);
    let b_page = env.page_of(b);
    let hole = h
        .lookup(env.page_of(a) + 1 + GUARD_PAGES)
        .expect("hole extent");
    assert_eq!(
        (hole.state, hole.start + hole.pages),
        (RunState::Purged, b_page)
    );
    let mut fillers = 0;
    loop {
        let (p, zero) = env.alloc(&h, page_size(), 1);
        if env.page_of(p) > b_page {
            env.free(&h, p);
            break;
        }
        assert!(zero && all(p, page_size(), 0));
        fill(p, page_size(), 0x33);
        held.push(p);
        fillers += 1;
    }
    assert!(
        fillers >= 2,
        "hole below the aligned run took {fillers} fillers"
    );
    // Shape 2: an Empty extent touching the mark absorbs the hole.
    let (c, _) = env.alloc(&h, 3 * page_size(), 1);
    let c_page = env.page_of(c);
    fill(c, 3 * page_size(), 0xC0);
    env.free(&h, c);
    assert_eq!(h.lookup(c_page).unwrap().state, RunState::Empty);
    let c_ext = h.lookup(c_page).unwrap();
    assert_eq!(c_ext.start + c_ext.pages, h.stats().next_page);
    let (d, zero) = env.alloc(&h, page_size(), 2 << 20);
    assert!(zero && all(d, page_size(), 0));
    fill(d, page_size(), 4);
    let d_page = env.page_of(d);
    let head = h.lookup(c_page).expect("head extent");
    assert_eq!(
        (head.state, head.start + head.pages),
        (RunState::Empty, d_page),
        "head did not absorb the hole"
    );
    let mut fillers = 0;
    loop {
        let (p, _) = env.alloc(&h, page_size(), 1);
        if env.page_of(p) > d_page {
            env.free(&h, p);
            break;
        }
        fill(p, page_size(), 0x44);
        held.push(p);
        fillers += 1;
    }
    assert!(
        fillers >= 2,
        "hole below the second aligned run took {fillers} fillers"
    );
    for &p in &held {
        let v = unsafe { *p };
        assert!(all(p, page_size(), v));
    }
    assert!(all(a, page_size(), 1) && all(b, page_size(), 2) && all(d, page_size(), 4));
    for p in held.into_iter().chain([a, b, d]) {
        env.free(&h, p);
    }
    assert_eq!(h.stats().runs, 0);
}

#[test]
fn concurrent_threads_see_their_own_patterns() {
    use std::sync::Arc;
    let env = Arc::new(Env::new());
    let h = Arc::new(env.huge());
    let mut handles = Vec::new();
    for t in 0..8u8 {
        let env = Arc::clone(&env);
        let h = Arc::clone(&h);
        handles.push(std::thread::spawn(move || {
            let mut held: Vec<(*mut u8, usize)> = Vec::new();
            let mut rng = 0xA5A5_5A5A_1234_5678u64 ^ (t as u64 + 1);
            for i in 0..300 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let size = (rng as usize % (6 * page_size())) + 1;
                let (p, zero) = h.alloc(&env.meta, size, 1, 0);
                if p.is_null() {
                    continue;
                }
                if zero {
                    assert!(all(p, size, 0));
                }
                fill(p, size, t.wrapping_add(i as u8));
                held.push((p, size));
                if held.len() > 12 {
                    let (q, n) = held.remove((rng >> 32) as usize % held.len());
                    let v = unsafe { *q };
                    assert!(all(q, n, v), "thread {t} saw another thread's writes");
                    h.free(&env.meta, env.page_of(q));
                }
            }
            for (q, n) in held {
                let v = unsafe { *q };
                assert!(all(q, n, v));
                h.free(&env.meta, env.page_of(q));
            }
        }));
    }
    for hd in handles {
        hd.join().unwrap();
    }
    assert_eq!(h.stats().runs, 0);
}

/// Extents whose discard fails (`mlock`ed: `EINVAL`) stay `Empty` in
/// their place with their contents: after the purge at free time, a full
/// purge and a decay walk (which backs the gate off for
/// `DISCARD_RETRY_NS`); a reuse is not reported zero, and once unlocked
/// they purge.
#[test]
fn extents_whose_discard_fails_stay_empty() {
    let env = Env::new();
    let h = env.huge();
    let size = 2 * page_size();
    // `q` keeps `p` and `r` from coalescing.
    let (p, _) = env.alloc(&h, size, 1);
    let (q, _) = env.alloc(&h, size, 1);
    let (r, _) = env.alloc(&h, size, 1);
    fill(p, size, 0x5A);
    fill(r, size, 0x5A);
    let test = "extents_whose_discard_fails_stay_empty";
    if !crate::testutil::mlock_or_skip(test, p as usize, size) {
        return;
    }
    if !crate::testutil::mlock_or_skip(test, r as usize, size) {
        unsafe { libc::munlock(p as *const _, size) };
        return;
    }
    env.free(&h, p);
    h.set_purge_on_free(true);
    env.free(&h, r);
    h.set_purge_on_free(false);
    let check_empty = || {
        let st = h.stats();
        assert_eq!((st.empty, st.purged), (2, 0), "{st:?}");
        for x in [p, r] {
            assert_eq!(h.lookup(env.page_of(x)).unwrap().state, RunState::Empty);
            assert!(all(x, size, 0x5A));
        }
    };
    check_empty();
    assert_eq!(h.purge_empty(u64::MAX), 0);
    check_empty();
    const W: u64 = 200_000;
    let t0 = h.oldest_dirty_at().unwrap();
    let retry = t0 + W + purge::DISCARD_RETRY_NS;
    assert_eq!(h.decay(t0 + W, W), Some(retry));
    check_empty();
    unsafe {
        libc::munlock(p as *const _, size);
        libc::munlock(r as *const _, size);
    }
    assert_eq!(h.decay(t0 + 2 * W, W), Some(retry));
    check_empty();
    // The oldest `Empty` extent is reused, dirty.
    let (p2, zero) = env.alloc(&h, size, 1);
    assert_eq!(p2, p);
    assert!(!zero);
    env.free(&h, p2);
    assert_eq!(h.purge_empty(u64::MAX), 2);
    assert_eq!(h.stats().empty, 0);
    assert!(all(p, size, 0) && all(r, size, 0));
    env.free(&h, q);
}
