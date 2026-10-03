//! glibc's tuning and accounting extras (design.md section 13.1):
//! `mallopt`, `mallinfo`, `mallinfo2`. `malloc_trim` is in `libc.rs` next
//! to the allocation calls, `malloc_stats` and `malloc_info` in `print.rs`.
//!
//! `mallopt` accepts every parameter and returns 1 (success): glibc's
//! knobs (`M_TRIM_THRESHOLD`, `M_MMAP_THRESHOLD`, `M_ARENA_MAX`, ...) have
//! no counterpart here and programs that set them (Ruby, PostgreSQL) treat
//! a 0 return as an error worth logging.
//!
//! `mallinfo2` is filled from the *live* figures (glibc's are), with
//! challoc's meaning for each field, chosen so that the identities glibc
//! programs compute still hold (`arena >= uordblks`, `fordblks =
//! arena - uordblks`, `hblkhd` is mmapped bytes):
//!
//! | field      | challoc                                                    |
//! |------------|------------------------------------------------------------|
//! | `arena`    | resident bytes minus `large.allocated` (slabs, dirty slabs, metadata) |
//! | `ordblks`  | free slots in slabs that are neither `Empty` nor `Purged`  |
//! | `smblks`   | 0                                                          |
//! | `hblks`    | live huge runs                                             |
//! | `hblkhd`   | `large.allocated`: bytes in allocated slots of the classes above `layout::BIN_MAX` plus usable bytes of live huge runs |
//! | `usmblks`  | 0                                                          |
//! | `fsmblks`  | 0                                                          |
//! | `uordblks` | `small.allocated`: bytes in allocated slots of the classes up to `BIN_MAX` (thread caches count, as in `stats.allocated`) |
//! | `fordblks` | `arena - uordblks`                                         |
//! | `keepcost` | dirty bytes a purge would release                          |
//!
//! So `uordblks + hblkhd` is `stats.allocated`, read live
//! ([`mallinfo2`]'s contract, `verus/THEOREMS.md`, "Exact stats"), and
//! `hblkhd` can be non-zero with `hblks` zero: the classes above `BIN_MAX`
//! are slab slots, counted with the huge runs as jemalloc counts them
//! (`large.*`), not as mapped blocks.
//!
//! `mallinfo` is the same in `int`, saturating at `INT_MAX`, which is why
//! glibc deprecated it.

use core::ffi::c_int;

use crate::layout::NCLASSES;
#[cfg(verus_only)]
use crate::lock::axiom_entry;
use crate::stats::{self, fig_add, fig_mul};
use vstd::prelude::*;

verus! {

/// glibc's `struct mallinfo` (ten `int`s).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MallInfo {
    pub arena: c_int,
    pub ordblks: c_int,
    pub smblks: c_int,
    pub hblks: c_int,
    pub hblkhd: c_int,
    pub usmblks: c_int,
    pub fsmblks: c_int,
    pub uordblks: c_int,
    pub fordblks: c_int,
    pub keepcost: c_int,
}

/// glibc's `struct mallinfo2` (ten `size_t`s).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MallInfo2 {
    pub arena: usize,
    pub ordblks: usize,
    pub smblks: usize,
    pub hblks: usize,
    pub hblkhd: usize,
    pub usmblks: usize,
    pub fsmblks: usize,
    pub uordblks: usize,
    pub fordblks: usize,
    pub keepcost: usize,
}

/// `mallopt(param, value)`: accepted and ignored.
pub fn mallopt(_param: c_int, _value: c_int) -> (r: c_int)
    ensures
        r == 1,
{
    1
}
/// `mallinfo2()`. The figures are summed and multiplied as the build
/// computes them (`stats::fig_add`, `fig_mul`): no verified invariant
/// bounds the counters. `rd` gets what [`stats::collect_all`] read and
/// summed: `uordblks + hblkhd` is that sum's `allocated`, the true sum of
/// the arenas' figures when it fits ([`stats::summed`]); `uordblks` is the
/// bins up to `BIN_MAX`, `hblkhd` the slab classes above it and the huge
/// runs, whose number is `hblks`.
pub fn mallinfo2(Tracked(rd): Tracked<&mut Ghost<(Seq<stats::ArenaStats>, Seq<int>, stats::ArenaStats)>>) -> (r: MallInfo2)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.fordblks <= r.arena,
        r.smblks == 0 && r.usmblks == 0 && r.fsmblks == 0,
        stats::summed(final(rd)@.0, final(rd)@.1, final(rd)@.2),
        r.uordblks == final(rd)@.2.small_allocated,
        r.hblkhd == final(rd)@.2.large_allocated,
        r.hblks == final(rd)@.2.large_runs,
{
    let shared = super::shared();
    proof {
        shared.lemma_wf();
    }
    let tracked mut sd = Ghost((Seq::<stats::ArenaStats>::empty(), Seq::<int>::empty()));
    let all = stats::collect_all(shared, Tracked(&mut sd), Tracked(&mut axiom_entry()));
    proof {
        *rd = Ghost((sd@.0, sd@.1, all));
    }
    let page = shared.regions.page_size();
    let arena = all.resident.saturating_sub(all.large_allocated);
    let uordblks = all.small_allocated;
    let mut free_slots: usize = 0;
    for j in 0..NCLASSES {
        let b = &all.bins[j];
        free_slots = fig_add(free_slots, fig_mul(b.curslabs, b.nregs).saturating_sub(b.curregs));
    }
    MallInfo2 {
        arena,
        ordblks: free_slots,
        smblks: 0,
        hblks: all.large_runs,
        hblkhd: all.large_allocated,
        usmblks: 0,
        fsmblks: 0,
        uordblks,
        fordblks: arena.saturating_sub(uordblks),
        keepcost: fig_mul(all.pdirty, page),
    }
}

/// `mallinfo()`: [`mallinfo2`] saturated into `int`s.
pub fn mallinfo() -> MallInfo
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let tracked mut rd: Ghost<(Seq<stats::ArenaStats>, Seq<int>, stats::ArenaStats)> = Ghost(arbitrary());
    let m = mallinfo2(Tracked(&mut rd));
    let clamp = |v: usize| v.min(c_int::MAX as usize) as c_int;
    MallInfo {
        arena: clamp(m.arena),
        ordblks: clamp(m.ordblks),
        smblks: clamp(m.smblks),
        hblks: clamp(m.hblks),
        hblkhd: clamp(m.hblkhd),
        usmblks: clamp(m.usmblks),
        fsmblks: clamp(m.fsmblks),
        uordblks: clamp(m.uordblks),
        fordblks: clamp(m.fordblks),
        keepcost: clamp(m.keepcost),
    }
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::c_int;
    use vstd::prelude::*;

    verus! {

    #[unsafe(no_mangle)]
    pub extern "C" fn mallopt(param: c_int, value: c_int) -> c_int {
        super::mallopt(param, value)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn mallinfo() -> super::MallInfo {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        super::mallinfo()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn mallinfo2() -> super::MallInfo2 {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let tracked mut rd: Ghost<(Seq<crate::stats::ArenaStats>, Seq<int>, crate::stats::ArenaStats)> = Ghost(arbitrary());
        super::mallinfo2(Tracked(&mut rd))
    }

    } // verus!
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::libc::free;
    use crate::api::libc::plain::malloc;
    use crate::layout::MEDIUM_MAX;
    use std::vec::Vec;

    #[test]
    fn mallopt_accepts_everything() {
        for param in [-1, -2, -3, -4, -6, -7, -8, 0, 1, 99] {
            assert_eq!(mallopt(param, 0), 1);
            assert_eq!(mallopt(param, 1 << 20), 1);
        }
    }

    #[test]
    fn mallinfo_reflects_live_allocations_and_keeps_glibcs_identities() {
        assert_eq!(
            core::mem::size_of::<MallInfo>(),
            10 * core::mem::size_of::<c_int>()
        );
        assert_eq!(
            core::mem::size_of::<MallInfo2>(),
            10 * core::mem::size_of::<usize>()
        );
        // Other tests allocate concurrently, so only our own live blocks
        // give bounds; the identities must hold for any single collection.
        let small: Vec<*mut u8> = (0..200).map(|_| malloc(900)).collect();
        let huge: Vec<*mut u8> = (0..3).map(|_| malloc(MEDIUM_MAX + 1)).collect();
        let m = mallinfo2(Tracked::assume_new());
        let size = crate::layout::class_size_of(900).unwrap();
        assert!(m.uordblks >= 200 * size, "{m:?}");
        assert!(m.hblks >= 3, "{m:?}");
        assert!(m.hblkhd >= 3 * (MEDIUM_MAX + 1), "{m:?}");
        assert!(m.arena >= m.uordblks, "{m:?}");
        assert_eq!(m.fordblks, m.arena - m.uordblks);
        assert_eq!(m.smblks + m.usmblks + m.fsmblks, 0);
        assert_eq!(m.keepcost % super::super::shared().regions.page_size(), 0);
        let i = mallinfo();
        assert!(
            i.hblks >= 3 && i.arena >= i.uordblks && i.fordblks == i.arena - i.uordblks,
            "{i:?}"
        );
        assert!(i.uordblks as usize >= 200 * size);
        for p in small.into_iter().chain(huge) {
            free(p);
        }
    }
}
