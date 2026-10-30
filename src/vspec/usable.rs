//! The usable size a query reports for a live block ([`usable_of`]), and
//! the usable size an entry point reports through a caller's out-pointer
//! ([`Stored`], whose specification lands with the proofs of the
//! `*_usable` entry points).

use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use crate::layout::{class_table, process_regions};

verus! {

/// `n` is the usable size a query reads for the live block `p`, whose
/// request size it does not know: its class's size for a slot of a class,
/// and for a page start of the huge region whole pages that fit as a run
/// at `p` ([`run_bytes`]), the shape of what the run's descriptor
/// records.
pub open spec fn usable_of(p: *mut u8, n: int) -> bool {
    &&& forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> n == class_table()[c].size
    &&& process_regions().is_huge_block(p@.addr, 1) ==> run_bytes(p@.addr, n)
}

/// `n` bytes can be the usable size of a run at the huge page start `p`:
/// whole pages, at least one, with the run's guard after them inside the
/// huge region (`Huge::spec_run_usable` of the process's huge allocator,
/// whose page and page count are the regions': `Shared::wf`).
pub open spec fn run_bytes(p: usize, n: int) -> bool {
    let r = process_regions();
    let pg = (1usize << r.lg_page) as int;
    let page = ((p - r.huge_base) as usize) >> r.lg_page;
    &&& n >= pg
    &&& n % pg == 0
    &&& page * pg + n + crate::huge::GUARD_PAGES * pg <= ((r.huge_len >> r.lg_page) as int) * pg
}

/// The usable size of a slot of class `c` is the class's size.
pub proof fn lemma_usable_slot(p: *mut u8, c: int)
    requires
        crate::heap::regions_ok(),
        process_regions().is_slot(c, p@.addr),
    ensures
        usable_of(p, class_table()[c].size as int),
{
    assert forall|c2: int| #[trigger] process_regions().is_slot(c2, p@.addr) implies c2 == c by {
        crate::vspec::zero::lemma_slot_unique(c, c2, p@.addr);
    }
    crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
}

/// What [`Huge::usable_size`](crate::huge::Huge::usable_size) reads for a
/// huge page start is the usable size [`usable_of`] states.
pub proof fn lemma_usable_run(shared: &crate::heap::Shared, p: *mut u8, n: usize)
    requires
        shared.wf(),
        process_regions().is_huge_block(p@.addr, 1),
        shared.huge_run_usable(p@.addr, n),
    ensures
        usable_of(p, n as int),
{
    shared.lemma_wf();
    shared.lemma_wf_slow();
    shared.lemma_huge_pages();
    shared.lemma_huge_run_usable(p@.addr, n);
    assert forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) implies false by {
        crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
    }
}

/// A query's usable size is positive: a slot's is its class's size, at
/// least the quantum, and a huge page start's at least one page.
pub proof fn lemma_usable_of_pos(p: *mut u8, n: int)
    requires
        crate::heap::regions_ok(),
        crate::heap::free_target(p),
        usable_of(p, n),
    ensures
        0 < n,
{
    let r = process_regions();
    if crate::heap::slot_start(p) {
        let c = choose|c: int| #[trigger] r.is_slot(c, p@.addr);
        crate::vspec::class::axiom_class_table_wf(c);
    } else {
        let lg = r.lg_page;
        assert((1usize << lg) > 0) by (bit_vector)
            requires
                lg < 64,
        ;
    }
}

/// Ghost token: an entry point stored `val` through the caller's
/// out-pointer `ptr` (and nothing, when `ptr` is null), the last store it
/// makes through it.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct Stored {
    ghost ptr: *mut usize,
    ghost val: usize,
}

} // verus!
