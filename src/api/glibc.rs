//! glibc's tuning and accounting extras (design 13.1):
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
//! arena - uordblks`) whenever the arenas' `mapped` sum fits. `hblkhd`,
//! glibc's mmapped bytes, holds the large allocated bytes (jemalloc's
//! `large.allocated`), and `arena` the rest of resident memory.
//!
//! | field      | challoc                                                                  |
//! |------------|--------------------------------------------------------------------------|
//! | `arena`    | resident bytes minus `hblkhd`                                            |
//! | `ordblks`  | free slots in slabs that are neither `Empty` nor `Purged`                |
//! | `smblks`   | 0                                                                        |
//! | `hblks`    | live huge runs                                                           |
//! | `hblkhd`   | bytes in allocated slots above `BIN_MAX` plus usable bytes of huge runs  |
//! | `usmblks`  | 0                                                                        |
//! | `fsmblks`  | 0                                                                        |
//! | `uordblks` | bytes in allocated slots up to `BIN_MAX`, thread-cached ones included    |
//! | `fordblks` | `arena - uordblks`                                                       |
//! | `keepcost` | dirty bytes a purge would release                                        |
//!
//! So `uordblks + hblkhd` is `stats.allocated`, read live, when the sum
//! fits ([`mallinfo2`]'s contract), and
//! `hblkhd` can be non-zero with `hblks` zero: the classes above `BIN_MAX`
//! are slab slots, counted with the huge runs as jemalloc counts them
//! (`large.*`), not as mapped blocks.
//!
//! `mallinfo` is the same in `int`, saturating at `INT_MAX`, which is why
//! glibc deprecated it.

use core::ffi::c_int;

use crate::layout::NCLASSES;
#[cfg(verus_keep_ghost)]
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

/// What [`mallinfo2`] read: [`stats::collect_all`]'s per-arena reads, their
/// huge pages, and their sum; and whether it collected them at all (it
/// does unless no metadata object is left for its scratch records).
pub type Reads = Ghost<(Seq<stats::ArenaStats>, Seq<int>, stats::ArenaStats, bool)>;

/// A `mallinfo2` of zeros, for a call with nothing collected.
pub(crate) fn zero_info() -> (r: MallInfo2)
    ensures
        r == zero_info_spec(),
{
    MallInfo2 {
        arena: 0,
        ordblks: 0,
        smblks: 0,
        hblks: 0,
        hblkhd: 0,
        usmblks: 0,
        fsmblks: 0,
        uordblks: 0,
        fordblks: 0,
        keepcost: 0,
    }
}

/// `mallinfo2()`. `rd` gets what [`stats::collect_all`] read and summed:
/// `uordblks` and `hblkhd` are that sum's `small_allocated` and
/// `large_allocated`, whose total is the true sum of the arenas'
/// `allocated` when it fits ([`stats::summed`]), and `hblks` its huge
/// runs. `arena` is the sum's `resident` less its `large_allocated`, and
/// `fordblks` is `arena` less `uordblks`, each 0 where it would be
/// negative. When the arenas' `mapped` sum fits, `arena >= uordblks` and
/// `fordblks == arena - uordblks` (the identities glibc programs
/// compute). The reads come from arenas of ascending registry IDs
/// ([`stats::ids_ascending`]). This contract leaves out
/// `ordblks` and `keepcost`: their figure arithmetic (`stats::fig_add`,
/// `fig_mul`) is exact only when it fits and wraps in the shipped library.
/// The sum and each arena's figures (about 7 KiB each with their bins)
/// are collected into a [`stats::Scratch`]'s two records, not onto the
/// caller's stack; with no object for them every figure is 0 and `rd`'s
/// flag is clear.
/// `theorem_read_only_never_aborts` restates its calls that carry a
/// contract; `api::restated::read_only_restatements_match_their_bodies`
/// checks that the two make the same calls.
pub fn mallinfo2(Tracked(rd): Tracked<&mut Reads>) -> (r: MallInfo2)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r.smblks == 0 && r.usmblks == 0 && r.fsmblks == 0,
        !final(rd)@.3 ==> r == zero_info_spec(),
        final(rd)@.3 ==> {
            &&& r.arena == if final(rd)@.2.resident >= final(rd)@.2.large_allocated { final(rd)@.2.resident - final(rd)@.2.large_allocated } else { 0 }
            &&& r.fordblks == if r.arena >= r.uordblks { r.arena - r.uordblks } else { 0 }
            &&& r.fordblks <= r.arena
            &&& stats::sum_mapped(final(rd)@.0) <= usize::MAX ==> r.arena >= r.uordblks && r.fordblks == r.arena - r.uordblks
            &&& stats::summed(final(rd)@.0, final(rd)@.1, final(rd)@.2)
            &&& stats::ids_ascending(final(rd)@.0)
            &&& forall|j: int| 0 <= j < final(rd)@.0.len() ==> ((#[trigger] final(rd)@.0[j]).index as usize) < crate::arena::MAX_ARENAS
            &&& r.uordblks == final(rd)@.2.small_allocated
            &&& r.hblkhd == final(rd)@.2.large_allocated
            &&& r.hblks == final(rd)@.2.large_runs
        },
{
    let shared = super::shared();
    proof {
        shared.lemma_wf();
    }
    // Premise (lock order): `mallinfo2`'s callers (the C exports and the
    // tests) hold no allocator lock.
    let tracked mut ord = axiom_entry();
    let Some(mut sc) = stats::Scratch::new(&shared.meta, Tracked(&mut ord)) else {
        proof {
            *rd = Ghost((rd@.0, rd@.1, rd@.2, false));
        }
        return zero_info();
    };
    let tracked mut sd = Ghost((Seq::<stats::ArenaStats>::empty(), Seq::<int>::empty()));
    let pair = sc.record();
    stats::collect_all(shared, &mut pair.a, &mut pair.b, Tracked(&mut sd), Tracked(&mut ord));
    let all = &sc.get().a;
    proof {
        *rd = Ghost((sd@.0, sd@.1, *all, true));
    }
    let page = shared.regions.page_size();
    let arena = all.resident.saturating_sub(all.large_allocated);
    let uordblks = all.small_allocated;
    // The free slots of every class, summed with an index loop and the
    // figure arithmetic (`stats::fig_add`, `fig_mul`): `+` and `*` as the
    // build computes them.
    let mut free_slots: usize = 0;
    for j in 0..NCLASSES {
        let b = &all.bins[j];
        free_slots = fig_add(
            free_slots,
            fig_mul(b.curslabs, b.nregs).saturating_sub(b.curregs),
        );
    }
    let r = MallInfo2 {
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
    };
    sc.free(&shared.meta, Tracked(&mut ord));
    r
}

/// [`zero_info`]'s value.
pub open spec fn zero_info_spec() -> MallInfo2 {
    MallInfo2 {
        arena: 0,
        ordblks: 0,
        smblks: 0,
        hblks: 0,
        hblkhd: 0,
        usmblks: 0,
        fsmblks: 0,
        uordblks: 0,
        fordblks: 0,
        keepcost: 0,
    }
}

/// `mallinfo()`: [`mallinfo2`] saturated into `int`s.
pub fn mallinfo() -> MallInfo
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let tracked mut rd: Reads = Ghost(arbitrary());
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
        let tracked mut rd: super::Reads = Ghost(arbitrary());
        super::mallinfo2(Tracked(&mut rd))
    }

    } // verus!
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::libc::{free, malloc};
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
        let small: Vec<*mut u8> = (0..200)
            .map(|_| {
                malloc(
                    900,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                )
            })
            .collect();
        let huge: Vec<*mut u8> = (0..3)
            .map(|_| {
                malloc(
                    MEDIUM_MAX + 1,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                )
            })
            .collect();
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
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
}
