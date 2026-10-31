//! The usable-size model: what the size-returning entry points state
//! about the sizes they report. [`usable_is`] is the usable size of a
//! block an allocation of known size returned, [`usable_of`] what a query
//! reads for a live block, and a [`Stored`] token names what an entry
//! point wrote through the caller's `size_t *usable`.

use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
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
    shared.lemma_wf_slow();
    shared.lemma_huge_pages();
    shared.lemma_huge_run_usable(p@.addr, n);
    assert forall|c: int| #[trigger] process_regions().is_slot(c, p@.addr) implies false by {
        crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
    }
}

/// The process's page `pg` (`1 << lg_page`) is a positive power of two,
/// shifting `x` down by `lg_page` divides it by `pg`, and the huge
/// region's whole pages are at most its length.
pub proof fn lemma_page_shift(x: usize)
    requires
        crate::heap::regions_ok(),
    ensures
        ({
            let r = process_regions();
            let pg = (1usize << r.lg_page) as int;
            &&& pg == vstd::arithmetic::power2::pow2(r.lg_page as nat)
            &&& pg > 0
            &&& (x >> r.lg_page) as int == x as int / pg
            &&& ((r.huge_len >> r.lg_page) as int) * pg <= r.huge_len
        }),
{
    let r = process_regions();
    let lg32 = r.lg_page;
    let lg = lg32 as usize;
    let len = r.huge_len;
    let q = x >> lg32;
    let np = len >> lg32;
    let one = 1usize << lg32;
    assert(q == x >> lg && np == len >> lg && one == 1usize << lg) by (bit_vector)
        requires
            q == x >> lg32,
            np == len >> lg32,
            one == 1usize << lg32,
            lg == lg32 as usize,
            lg32 < 64,
    ;
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    vstd::bits::lemma_usize_shr_is_div(x, lg);
    vstd::bits::lemma_usize_shr_is_div(len, lg);
    let pg = one as int;
    vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(len as int, pg);
}

/// The usable bytes of a run lie in the huge region: `n` bytes that can
/// be the usable size of a run at the huge page start `p` ([`run_bytes`],
/// what [`Huge::usable_size`](crate::huge::Huge::usable_size) reads from
/// the run's descriptor) are `n` bytes of the region from `p` on, so any
/// `size` up to `n` is a huge block of `size` bytes.
pub proof fn lemma_run_bytes_block(p: usize, n: int, size: int)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(p, 1),
        run_bytes(p, n),
        size <= n,
    ensures
        process_regions().is_huge_block(p, if size < 1 { 1 } else { size }),
{
    let r = process_regions();
    let rel = (p - r.huge_base) as usize;
    let len = r.huge_len;
    let page = rel >> r.lg_page;
    let np = len >> r.lg_page;
    let pg = (1usize << r.lg_page) as int;
    lemma_page_shift(rel);
    // The page index times the page is `p`'s offset (a page start), and
    // the region's whole pages are at most its length.
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(rel as int, pg);
    assert(page * pg == rel) by (nonlinear_arith)
        requires
            page as int == rel as int / pg,
            rel as int == pg * (rel as int / pg) + (rel as int) % pg,
            (rel as int) % pg == 0,
    ;
    assert(np * pg <= len);
    assert(0 <= crate::huge::GUARD_PAGES * pg) by (nonlinear_arith)
        requires
            pg > 0,
    ;
    assert(p + n <= r.huge_base + len);
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
/// makes through it, so `*ptr == val` when it returns.
///
/// A token is made only from the permission of a store through `ptr`
/// ([`Stored::stored`]; the permission is a `vspec::cbuf::COut`, which
/// only `libc`'s `axiom_c_out` makes, and `libc::store_usable` is the one
/// writer of a `COut<usize>`); it is not a permission to access `*ptr`, and nothing ties it
/// to later writes. The entry points return the token of their last
/// store, and their postconditions state its value at the moment of
/// return.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
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

    /// The token of the store of `val` through `ptr`, from the permission
    /// it was made with (`out`, the C caller's out-pointer's), which now
    /// holds `val`; for a null `ptr`, the token of no store.
    pub proof fn stored(tracked out: &crate::vspec::cbuf::COut<usize>, ptr: *mut usize, val: usize) -> (tracked s: Stored)
        requires
            ptr@.addr == 0 || (out.opt() matches Some(q) && q.ptr() == ptr && q.is_init() && q.value() == val),
        ensures
            s.ptr() == ptr,
            s.val() == val,
    {
        Stored { ptr, val }
    }
}

} // verus!
