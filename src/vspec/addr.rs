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

// The allocator's own claims about the blocks it hands out, for the
// `write_bytes` and `copy_nonoverlapping` calls that verified code makes
// on them. The verified allocation paths return addresses, not
// permissions: a cache pop returns whatever word the stack held, so no
// verified function can ensure these today. Each axiom stands for an
// ensures its producer would carry under a model of the slab and huge
// memory (the vstd `PointsTo` hand-out of `verus/TODO.md`, Tier 3), and
// names that producer. The requires state what the verifier can check;
// the rest (that the pointer is the block the doc names) is on the
// caller, and each axiom has one kind of call site. As `rw_len` says, a
// validity fact holds when it is established: nothing here may be used
// on a block after it is freed.

/// A non-null block that the allocation paths just returned is valid for
/// writes of `len` bytes, where `len` is at most its usable size (the
/// request `n * m` for `calloc`; the class size or the huge run's usable
/// bytes for `calloc_usable` and jemalloc's `alloc_with`). The producers
/// that would ensure it: the slab paths under `heap::alloc_zeroed` and
/// `arena::alloc` (cache pops, refills, direct claims) and
/// `Huge::alloc_run` under `Heap::alloc_huge`, `Shared::alloc_huge` and
/// `arena::alloc_huge`.
#[verifier::external_body]
pub proof fn axiom_fresh_block(p: *mut u8, len: usize)
    requires
        p@.addr != 0,
    ensures
        crate::vspec::ptr::valid_writes(p, len as int),
{
}

/// A `realloc` move: `p` is a live block of `old` usable bytes (the
/// classification `old_size_or_keep` or `realloc_old` made), and `q` a
/// non-null block the allocation paths just returned for at least `len`
/// bytes. `p` is valid for reads of `old` bytes, `q` for writes of `len`,
/// and the two do not overlap (a fresh block is never a live one). The
/// producers that would ensure it: the classification, for `p`; `malloc`,
/// `malloc_usable` and jemalloc's `alloc_with`, for `q` and for the
/// disjointness.
#[verifier::external_body]
pub proof fn axiom_moved_block(p: *mut u8, old: usize, q: *mut u8, len: usize)
    requires
        p@.addr != 0,
        q@.addr != 0,
    ensures
        crate::vspec::ptr::valid_reads(p as *const u8, old as int),
        crate::vspec::ptr::valid_writes(q, len as int),
        p@.addr + old <= q@.addr || q@.addr + len <= p@.addr,
{
}

/// An allocated slot of `class` is valid for writes of the class's size:
/// its slab is committed before its first slot is claimed and decommitted
/// only when it holds no allocated slot. `zero_slot` clears a slot being
/// freed. The producer that would ensure it: `Shared::classify_free`,
/// which checks the slot's bitmap bit and gives `p` to every `zero_slot`
/// caller (`Heap::free_slow_in`, `Heap::percpu_free_slow` and
/// `heap::free_direct`, which pass it on without stating it).
#[verifier::external_body]
pub proof fn axiom_slot_writable(class: usize, p: *mut u8)
    requires
        class < crate::layout::NCLASSES,
    ensures
        crate::vspec::ptr::valid_writes(p, crate::layout::class_table()[class as int].size as int),
{
}

/// The `len` usable bytes of the live huge run that starts at `page` are
/// committed and valid for writes through `addr_ptr` of the page's
/// address. The producer that would ensure it: `Huge::usable_size` for a
/// run start that `Shared::classify_free` returned.
#[verifier::external_body]
pub proof fn axiom_huge_writable(regions: crate::layout::Regions, page: usize, len: usize)
    requires
        regions.wf(),
        page < regions.huge_len >> regions.lg_page,
    ensures
        crate::vspec::ptr::valid_writes(addr_ptr((regions.huge_base + (page << regions.lg_page)) as usize), len as int),
{
}

} // verus!
