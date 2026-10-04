//! Address-to-pointer casts and byte fills, which Verus does not model.

use vstd::prelude::*;

verus! {

/// The pointer `addr as *mut u8` yields: the address with the provenance
/// the cast picks up (challoc's memory comes from `mmap`, whose ranges
/// the kernel hands out as plain addresses). Left abstract; one value per
/// address, so a claim about the memory at `addr` can name it.
pub uninterp spec fn addr_ptr(addr: usize) -> *mut u8;

/// The provenance every integer-to-pointer cast picks up: an address cast
/// to a pointer names no allocation of its own, so all of them share one
/// (Rust's exposed provenance). The permissions minted for `mmap`'d
/// memory carry it (`vspec::unbacked::Unbacked::commit`).
pub uninterp spec fn cast_provenance() -> vstd::raw_ptr::Provenance;

/// `addr as *mut u8`: a pointer to `addr` (Verus supports only the other
/// direction of the cast), [`addr_ptr`], with [`cast_provenance`]. Verified
/// code compares and returns it, and passes it to the functions below
/// only under an axiom about the memory at `addr` or with a permission
/// of the same provenance.
#[verifier::external_body]
#[inline(always)]
pub fn addr_to_ptr(addr: usize) -> (p: *mut u8)
    ensures
        p as usize == addr,
        p == addr_ptr(addr),
        p@.provenance == cast_provenance(),
    opens_invariants none
{
    addr as *mut u8
}

/// `core::ptr::write_bytes`, with the safety contract of the `core`
/// documentation: `dst` is properly aligned and, unless the fill is empty,
/// valid for writes of `count * size_of::<T>()` bytes. Its effect is not
/// stated, so Verus assumes it changes no memory a ghost permission covers.
pub assume_specification<T>[ core::ptr::write_bytes::<T> ](dst: *mut T, val: u8, count: usize)
    requires
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || crate::vspec::ptr::valid_writes(dst, count as int * vstd::layout::size_of::<T>()),
;

/// `core::ptr::copy_nonoverlapping`, with the safety contract of the
/// `core` documentation: both pointers are properly aligned and, unless
/// the copy is empty, `src` is valid for reads and `dst` for writes of
/// `count * size_of::<T>()` bytes, and the two ranges do not overlap. As
/// for `write_bytes`, its effect is not stated.
pub assume_specification<T>[ core::ptr::copy_nonoverlapping::<T> ](src: *const T, dst: *mut T, count: usize)
    requires
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || {
            let len = count as int * vstd::layout::size_of::<T>();
            &&& crate::vspec::ptr::valid_reads(src, len)
            &&& crate::vspec::ptr::valid_writes(dst, len)
            &&& (src@.addr + len <= dst@.addr || dst@.addr + len <= src@.addr)
        },
;

/// `<*mut T>::is_null`: the address is zero.
pub assume_specification<T: core::marker::PointeeSized>[ <*mut T>::is_null ](p: *mut T) -> (r: bool)
    ensures
        r == (p@.addr == 0),
    opens_invariants none
    no_unwind
;

/// `<*const T>::is_null`: the address is zero.
pub assume_specification<T: core::marker::PointeeSized>[ <*const T>::is_null ](p: *const T) -> (r: bool)
    ensures
        r == (p@.addr == 0),
    opens_invariants none
    no_unwind
;

} // verus!

verus! {

// The allocator's own claims about the memory of its blocks, for the
// `write_bytes` and `copy_nonoverlapping` calls that verified code makes
// on them. The slab and huge regions have no permission model (no
// `PointsToRaw` for their bytes, unlike the metadata region's
// `vspec::unbacked::Unbacked`): no ghost permission records which slabs
// and runs are committed (the class lock's ghost `committed` set grants
// nothing, and no block token reaches it). Each axiom states, for one
// kind of block, the fact a model of the commits would prove; a live
// block's token gives its premise ([`Live::lemma_rw`](crate::heap::live::Live::lemma_rw),
// which the zero fills of fresh blocks and `realloc`'s copy use), and the
// free paths' classification gives it for the block being freed. As
// `rw_len` says, a validity fact holds when it is established: nothing
// here may be used on a block after it is freed.

/// An allocated slot of `class` is valid for writes of the class's size:
/// its slab is committed before its first slot is claimed and decommitted
/// only when it holds no allocated slot. Stated for any pointer with the
/// slot's address, whatever its provenance (`rw_len` keys on both): the
/// pointers that reach a slot are the one the allocation returned and
/// the one the program passes back to a free or `realloc`, which carry no
/// provenance verified code can check. Used for a live slot's token
/// (`Live::lemma_rw`) and by `zero_slot`, which clears a slot being
/// freed, whose bitmap bit `Shared::classify_free` checked (its callers
/// `Heap::free_slow_in` and `heap::free_direct` pass `p` on without
/// stating it). Required: `p` is a slot start of `class` (what
/// `zero_slot` requires); without it any pointer, null included, would be
/// writable.
#[verifier::external_body]
pub proof fn axiom_slot_writable(class: usize, p: *mut u8)
    requires
        class < crate::layout::NCLASSES,
        crate::layout::process_regions().is_slot(class as int, p@.addr),
    ensures
        crate::vspec::ptr::valid_writes(p, crate::layout::class_table()[class as int].size as int),
{
}

/// The `len` usable bytes of the live huge run that starts at `page` are
/// committed and valid for writes, through any pointer with the page's
/// address (as for [`axiom_slot_writable`]: `zero_huge` writes through
/// [`addr_ptr`] of it, `realloc` copies from the pointer C passed). Used
/// for a live run's token (`Live::lemma_rw`, from the run's pages before
/// its guard) and by `Shared::zero_huge`, from `Huge::usable_size` for a
/// run start that `Shared::classify_free` returned. Required: `regions`
/// are the process's, well formed and separated (so the huge region is
/// not at address 0), and the `len` bytes end inside the huge region
/// (`Huge::spec_run_usable` gives it). Not checked: that `page` starts a
/// live run (the caller's premise above).
#[verifier::external_body]
pub proof fn axiom_huge_writable(regions: crate::layout::Regions, page: usize, len: usize)
    requires
        regions == crate::layout::process_regions(),
        crate::heap::regions_ok(),
        page < regions.huge_len >> regions.lg_page,
        (page << regions.lg_page) + len <= regions.huge_len,
    ensures
        forall|p: *mut u8| p@.addr == regions.huge_base + (page << regions.lg_page)
            ==> #[trigger] crate::vspec::ptr::valid_writes(p, len as int),
{
}

} // verus!
