//! The `MALLOCX_TCACHE` paths of the C API, on the process's allocator,
//! with `heap::heap_at_tests`'s checker on the explicit cache's heap after
//! every call: each of these calls reaches `heap::tcaches::heap_at`
//! through `Tcaches::get` (`mallocx` small and huge, `dallocx`,
//! `sdallocx`, `rallocx`), `tcache.flush` and `tcache.destroy`.

use super::jemalloc::{dallocx, mallocx, rallocx, sdallocx};
use super::shared;
use crate::heap::FOREIGN;
use crate::heap::heap_at_tests::{assert_tcache_ok, tcache_create};
use crate::heap::mallocx::{mallocx_arena, mallocx_tcache};
use libc::c_int;
use std::vec::Vec;
use vstd::prelude::Tracked;

fn tcache_flags(t: u16) -> c_int {
    mallocx_tcache(t) as c_int
}

/// An explicit cache through `mallocx`, `dallocx`, `sdallocx` and
/// `rallocx` with `MALLOCX_TCACHE`, from its home arena and with
/// `MALLOCX_ARENA` (an `others` node), a `dallocx` of another arena's slot
/// (the FOREIGN node), a huge `mallocx`, then `tcache.flush` (every cache
/// drained) and `tcache.destroy`.
#[test]
fn mallocx_tcache_paths_pass_the_heap_checker() {
    // `tcache.create` hands out the lowest free id: under the guard, no
    // test that checks an id it destroyed names no cache runs meanwhile
    // (`api::mallctl`'s `thread_and_tcache_keys`).
    let _serial = crate::testutil::global_state();
    let s = shared();
    let lua = s
        .arenas
        .create(s, Tracked::assume_new(), Tracked::assume_new())
        .expect("arenas.create");
    let t = tcache_create(s, 0);
    assert_tcache_ok(s, t, "tcache.create");
    let f = tcache_flags(t);
    let fa = f | mallocx_arena(lua) as c_int;
    let mut live = Vec::new();
    for size in [8usize, 16, 100, 1000, 4096, 30_000] {
        let p = mallocx(size, f, Tracked::assume_new(), Tracked::assume_new());
        assert!(!p.is_null());
        assert_tcache_ok(s, t, "mallocx");
        let q = mallocx(size, fa, Tracked::assume_new(), Tracked::assume_new());
        assert!(!q.is_null());
        assert_tcache_ok(s, t, "mallocx with MALLOCX_ARENA");
        let r = rallocx(
            p,
            size * 3,
            f,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!r.is_null());
        assert_tcache_ok(s, t, "rallocx");
        dallocx(
            r,
            f,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert_tcache_ok(s, t, "dallocx");
        let e = mallocx(size, f, Tracked::assume_new(), Tracked::assume_new());
        sdallocx(
            e,
            size,
            f,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert_tcache_ok(s, t, "sdallocx");
        live.push((q, size));
    }
    // A slot of the Lua arena from the thread's own cache, freed through
    // `t` (homed on arena 0): onto `t`'s FOREIGN node.
    let o = mallocx(
        64,
        mallocx_arena(lua) as c_int,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    dallocx(
        o,
        f,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_tcache_ok(s, t, "dallocx of another arena's slot");
    let huge = mallocx(3 << 20, f, Tracked::assume_new(), Tracked::assume_new());
    assert!(!huge.is_null());
    assert_tcache_ok(s, t, "a huge mallocx");
    dallocx(
        huge,
        f,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert_tcache_ok(s, t, "a huge dallocx");
    for (q, size) in live {
        sdallocx(
            q,
            size,
            fa,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert_tcache_ok(s, t, "sdallocx with MALLOCX_ARENA");
    }
    let h = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    for a in [lua, FOREIGN] {
        assert!(
            (0..crate::layout::NCLASSES).any(|c| h.cache_in(a, c).is_some_and(|k| k.len() > 0)),
            "arena {a}: no cached slot"
        );
    }
    assert!(s.tcaches.flush(s, t, Tracked::assume_new()));
    assert_tcache_ok(s, t, "tcache.flush");
    let h = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    for a in [0, lua, FOREIGN] {
        assert!(
            (0..crate::layout::NCLASSES).all(|c| h.cache_in(a, c).is_none_or(|k| k.len() == 0))
        );
    }
    assert!(
        s.tcaches
            .destroy(s, t, Tracked::assume_new(), Tracked::assume_new())
    );
    // No `get` of `t` here: other `api::` tests create explicit caches on
    // this `Shared` in parallel, and `tcache.create` hands out the lowest
    // free id.
}

/// A fork child uses the explicit cache its parent created through the C
/// API's `MALLOCX_TCACHE` calls (the C contract: one thread at a time,
/// and the child has its own copy): the checker passes there before and
/// after them, after `tcache.flush` and until `tcache.destroy`, and in the
/// parent afterwards.
#[test]
fn fork_child_uses_the_parents_explicit_cache_through_mallocx() {
    // As in `mallocx_tcache_paths_pass_the_heap_checker`.
    let _serial = crate::testutil::global_state();
    let s = shared();
    let t = tcache_create(s, 0);
    let f = tcache_flags(t);
    let kept: Vec<usize> = (0..8)
        .map(|_| mallocx(200, f, Tracked::assume_new(), Tracked::assume_new()) as usize)
        .collect();
    for &p in &kept[..4] {
        dallocx(
            p as *mut u8,
            f,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
    }
    assert_tcache_ok(s, t, "the parent's dallocx");
    let kept = kept[4..].to_vec();
    let child_kept = kept.clone();
    crate::testutil::in_fork_child("MALLOCX_TCACHE in a fork child", 20_000, move || {
        assert_tcache_ok(s, t, "fork");
        let ps: Vec<*mut u8> = (0..32)
            .map(|_| mallocx(64, f, Tracked::assume_new(), Tracked::assume_new()))
            .collect();
        assert_tcache_ok(s, t, "the child's mallocx");
        for p in ps {
            sdallocx(
                p,
                64,
                f,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
        }
        for &p in &child_kept {
            dallocx(
                p as *mut u8,
                f,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
        }
        assert_tcache_ok(s, t, "the child's frees");
        assert!(s.tcaches.flush(s, t, Tracked::assume_new()));
        assert_tcache_ok(s, t, "the child's tcache.flush");
        assert!(
            s.tcaches
                .destroy(s, t, Tracked::assume_new(), Tracked::assume_new())
        );
        assert!(s.tcaches.get(t, Tracked::assume_new()).is_none());
    });
    assert_tcache_ok(s, t, "the child's exit");
    for &p in &kept {
        dallocx(
            p as *mut u8,
            f,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
    }
    assert_tcache_ok(s, t, "the parent's last dallocx");
    assert!(
        s.tcaches
            .destroy(s, t, Tracked::assume_new(), Tracked::assume_new())
    );
}
