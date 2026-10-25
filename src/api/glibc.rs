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
//! | `arena`    | resident bytes minus huge runs (slabs, dirty slabs, metadata) |
//! | `ordblks`  | free slots in slabs that are neither `Empty` nor `Purged`  |
//! | `smblks`   | 0                                                          |
//! | `hblks`    | live huge runs                                             |
//! | `hblkhd`   | usable bytes of live huge runs                             |
//! | `usmblks`  | 0                                                          |
//! | `fsmblks`  | 0                                                          |
//! | `uordblks` | bytes in allocated slots (thread caches count, as in `stats.allocated`) |
//! | `fordblks` | `arena - uordblks`                                         |
//! | `keepcost` | dirty bytes a purge would release                          |
//!
//! `mallinfo` is the same in `int`, saturating at `INT_MAX`, which is why
//! glibc deprecated it.

use core::ffi::c_int;

use crate::layout::NCLASSES;
use crate::stats::{self, fig_add, fig_mul};

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
pub fn mallopt(_param: c_int, _value: c_int) -> c_int {
    1
}

/// `mallinfo2()`.
pub fn mallinfo2() -> MallInfo2 {
    let shared = super::shared();
    let all = stats::collect_all(shared);
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
pub fn mallinfo() -> MallInfo {
    let m = mallinfo2();
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

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::c_int;

    #[unsafe(no_mangle)]
    pub extern "C" fn mallopt(param: c_int, value: c_int) -> c_int {
        super::mallopt(param, value)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn mallinfo() -> super::MallInfo {
        super::mallinfo()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn mallinfo2() -> super::MallInfo2 {
        super::mallinfo2()
    }
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
        let small: Vec<*mut u8> = (0..200).map(|_| malloc(900)).collect();
        let huge: Vec<*mut u8> = (0..3).map(|_| malloc(MEDIUM_MAX + 1)).collect();
        let m = mallinfo2();
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
