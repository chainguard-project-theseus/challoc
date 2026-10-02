//! The usable-size model: what the size-returning entry points state
//! about the sizes they report (`verus/THEOREMS.md`, "Interface
//! contract", the U column). [`usable_is`] is the usable size of a block
//! an allocation of known size returned, [`usable_of`] what a query reads
//! for a live block, and a [`Stored`] token names what an entry point
//! wrote through the caller's `size_t *usable`.

use vstd::prelude::*;

#[cfg(verus_only)]
use crate::layout::{class_table, process_regions};

verus! {

/// `n` is the usable size of the block `p` that an allocation of `size`
/// bytes returned: its class's size for a slot of a class
/// ([`Regions::is_slot`](crate::layout::Regions::is_slot), one class per
/// slot: [`lemma_slot_unique`](crate::vspec::zero::lemma_slot_unique)),
/// and [`huge_usable`](crate::vspec::zero::huge_usable)`(size)`, `size`
/// rounded up to whole pages, for a page start of the huge region. These
/// are the usable ends [`covers_usable`](crate::vspec::zero::covers_usable)
/// names.
pub open spec fn usable_is(p: *mut u8, size: int, n: int) -> bool {
    &&& forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) ==> n == class_table()[c].size
    &&& process_regions().is_huge_block(p@.addr, 1) ==> n == crate::vspec::zero::huge_usable(size)
}

/// `n` is the usable size a query reads for the live block `p`, whose
/// request size it does not know: its class's size for a slot of a class,
/// and for a page start of the huge region whole pages that fit as a run
/// at `p` ([`run_bytes`]), the shape of what the run's descriptor
/// records. That the descriptor records
/// [`huge_usable`](crate::vspec::zero::huge_usable) of the request is
/// `vspec::huge::axiom_owned_run`, for a caller that holds the run it
/// allocated.
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

/// The usable size of a slot of class `c` is the class's size, whatever
/// the request.
pub proof fn lemma_usable_slot(p: *mut u8, c: int, size: int)
    requires
        crate::heap::regions_ok(),
        process_regions().is_slot(c, p@.addr),
    ensures
        usable_is(p, size, class_table()[c].size as int),
        usable_of(p, class_table()[c].size as int),
{
    assert forall|c2: int| #[trigger] process_regions().is_slot(c2, p@.addr) implies c2 == c by {
        crate::vspec::zero::lemma_slot_unique(c, c2, p@.addr);
    }
    crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
}

/// The usable size of a huge page start allocated for `size` bytes is
/// `huge_usable(size)`: no slot is a huge page start.
pub proof fn lemma_usable_huge(p: *mut u8, size: int)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(p@.addr, 1),
    ensures
        usable_is(p, size, crate::vspec::zero::huge_usable(size)),
{
    assert forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) implies false by {
        crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
    }
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
    shared.lemma_huge_run_usable(p@.addr, n);
    assert forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) implies false by {
        crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
    }
}

/// Size consistency (`verus/THEOREMS.md`, the U column): a query of a
/// slot that an allocation of `size` bytes returned reads the usable size
/// the allocation stated. With `nallocx`'s and `mallocx`'s postconditions
/// (`jemalloc::nalloc_usable` in both), `sallocx(mallocx(size, flags))`
/// and `malloc_usable_size` of it equal `nallocx(size, flags)` for every
/// slab-class result. For a huge run the query states the descriptor's
/// bounds ([`run_bytes`]); its equality to the request's page-rounded
/// size is `vspec::huge::axiom_owned_run`, which the allocating entry
/// points apply at their own allocation.
pub proof fn theorem_usable_consistent(p: *mut u8, size: int, n: int, r: int)
    requires
        crate::heap::slot_start(p),
        usable_is(p, size, n),
        usable_of(p, r),
    ensures
        r == n,
{
    let c = choose|c: int| #[trigger] process_regions().is_slot(c, p@.addr);
    assert(process_regions().is_slot(c, p@.addr));
}

/// Ghost token: an entry point stored `val` through the caller's
/// out-pointer `ptr` (and nothing, when `ptr` is null), the last store it
/// makes through it, so `*ptr == val` when it returns.
///
/// A token is made only by `libc::store_usable`, from the store it
/// performs; it is not a permission to access `*ptr`, and nothing ties it
/// to later writes. The entry points return the token of their last
/// store, and their postconditions state its value at the moment of
/// return.
pub tracked struct Stored {
    ghost ptr: *mut usize,
    ghost val: usize,
}

impl Stored {
    /// The out-pointer.
    pub closed spec fn ptr(&self) -> *mut usize {
        self.ptr
    }

    /// The value stored through it.
    pub closed spec fn val(&self) -> usize {
        self.val
    }
}

} // verus!
