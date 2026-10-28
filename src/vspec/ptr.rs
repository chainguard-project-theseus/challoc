//! Raw pointer arithmetic.

use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// `p.add(count)`, left abstract: verified code only dereferences the
/// result through a `vstd::raw_ptr::PointsTo` whose `ptr()` is this value,
/// so nothing about the address itself is needed.
pub uninterp spec fn ptr_add<T>(p: *mut T, count: usize) -> *mut T;

/// How many bytes from `addr` to the end of the allocation that a pointer
/// with provenance `prov` and address `addr` is derived from (negative
/// when there is none). Uninterpreted: verified code cannot learn it on
/// its own. The trusted function that produced a pointer states a bound on
/// it for the offsets its callers use, or [`axiom_raw_valid`] derives one
/// from a permission to the bytes, and verified code carries that bound
/// from there. Address and provenance survive pointer casts, so the
/// bound does too.
pub uninterp spec fn alloc_len(prov: Provenance, addr: usize) -> int;

/// The `len` bytes from `p` lie inside the allocation `p` is derived from
/// (the range may end one past its last byte): the bounds condition of
/// `<*mut T>::add`.
pub open spec fn in_alloc<T>(p: *mut T, len: int) -> bool {
    0 <= len <= alloc_len(p@.provenance, p@.addr)
}

/// How many bytes from `addr` are valid for reads and writes through a
/// pointer with provenance `prov`: they lie inside one live allocation
/// that pointer is derived from, and the memory may be read and written.
/// Uninterpreted, as [`alloc_len`]. The model has no memory state: a
/// bound holds at the point it is established (a free or a decommit ends
/// it), and verified code uses each one right away.
pub uninterp spec fn rw_len(prov: Provenance, addr: usize) -> int;

/// `p` is valid for writes of `len` bytes.
pub open spec fn valid_writes<T>(p: *mut T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// `p` is valid for reads of `len` bytes: inside the read-write bound.
/// Closed, so that read validity (from [`lemma_writes_read`]) does not
/// unfold into write validity outside this module.
pub closed spec fn valid_reads<T>(p: *const T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// Bytes valid for writes are valid for reads.
pub proof fn lemma_writes_read(p: *mut u8, len: int)
    requires
        valid_writes(p, len),
    ensures
        valid_reads(p as *const u8, len),
{
}

/// Read validity is of the address and provenance, so it carries over a
/// cast to a pointer to `T`.
pub proof fn lemma_reads_cast<T>(p: *mut u8, len: int)
    requires
        valid_reads(p as *const u8, len),
    ensures
        valid_reads(p as *const T, len),
{
}

/// Every initialised value of `T`'s size is a `T` with no ghost content:
/// a volatile read of a `T` makes a valid value out of any initialised
/// bytes.
/// Uninterpreted; [`axiom_plain_u64`] states it for `u64`, the only type
/// verified code reads that way.
pub uninterp spec fn plain<T>() -> bool;

/// [`plain`] for `u64`: every initialised eight bytes are a `u64`, which
/// holds no ghost state. Trusted: the `core` documentation of the integer
/// types (no invalid bit patterns).
pub axiom fn axiom_plain_u64()
    ensures
        plain::<u64>(),
;

/// `core::ptr::read_volatile`, with the safety contract of the `core`
/// documentation: `src` is valid for reads of `size_of::<T>()` bytes and
/// aligned for `T`, and the bytes are a valid `T`, which [`plain`] asserts
/// for `T` (only `u64`). The bytes are initialised: the callers read a
/// slab slot or a huge run, anonymous memory the kernel fills (zero, or
/// what the program wrote). The value read is not stated.
pub assume_specification<T>[ core::ptr::read_volatile::<T> ](src: *const T) -> T
    requires
        plain::<T>(),
        valid_reads(src, vstd::layout::size_of::<T>() as int),
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
    opens_invariants none
;

/// `core::ptr::write_bytes` filled the `len` bytes at `p` with zero. Only
/// `write_bytes`' specification states it; `vspec::zero::axiom_wrote_zero`
/// turns it into the token that the bytes read as zero.
pub uninterp spec fn wrote_zero(p: *mut u8, len: int) -> bool;

/// `core::ptr::write_bytes`, with the safety contract of the `core`
/// documentation: `dst` is non-null and properly aligned and, unless the
/// fill is empty, valid for writes of `count * size_of::<T>()` bytes. Its
/// effect is not stated, so Verus assumes it changes no memory a ghost
/// permission covers, except that a zero fill is recorded ([`wrote_zero`]).
pub assume_specification<T>[ core::ptr::write_bytes::<T> ](dst: *mut T, val: u8, count: usize)
    requires
        dst@.addr != 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || valid_writes(dst, count as int * vstd::layout::size_of::<T>()),
    ensures
        val == 0 ==> wrote_zero(dst as *mut u8, count as int * vstd::layout::size_of::<T>()),
;

/// A permission to the `len` bytes from `p`, of `p`'s provenance, makes
/// them valid for writes through `p` and, when there are some, puts them
/// inside the allocation `p` is derived from ([`in_alloc`]). Trusted: what a `PointsToRaw`
/// stands for. The permissions verified code holds start as committed memory
/// (`vspec::unbacked::Unbacked::commit`) or as a lazily committed table
/// (`Unbacked::into_lazy`) and are split and joined from there, all inside
/// one mapping (the metadata region, the only `Unbacked` token's source),
/// or as a slot's or a huge run's usable bytes (`heap::live`'s
/// `axiom_hand_out`: a slot of a committed slab, inside the one class
/// superblock mapping, or pages `Huge::prepare` committed, inside the one
/// huge-region mapping), or as a committed `Fresh` slab's slots
/// (`class::axiom_fresh_slab_mem`, inside the class superblock mapping),
/// so one permission's range never spans two allocations. A table's bytes
/// are writable only once its owner has committed their page, the premise
/// of `into_lazy`, which a caller of this axiom keeps too.
#[verifier::external_body]
pub proof fn axiom_raw_valid(tracked raw: &vstd::raw_ptr::PointsToRaw, p: *mut u8, len: int)
    requires
        raw.contains_range(p@.addr as int, len),
        raw.provenance() == p@.provenance,
        0 <= len,
    ensures
        valid_writes(p, len),
        len > 0 ==> in_alloc(p, len),
{
}

/// The address [`ptr_add`] stands for: `count` elements past `p`, when
/// that stays inside the allocation `p` points into (the only case in
/// which `<*mut T>::add` is defined, and so the only one verified code
/// reaches), and the address does not wrap (the caller shows it: no
/// allocation does, but `alloc_len` is not tied to the address space).
/// Trusted: the `core` documentation of `add` (an offset of
/// `count * size_of::<T>()` bytes, keeping `p`'s provenance and
/// metadata), which Verus does not model for raw pointers. Not a
/// broadcast: the proofs that place memory call it.
#[verifier::external_body]
pub proof fn axiom_ptr_add_addr<T>(p: *mut T, count: usize)
    requires
        in_alloc(p, count as int * vstd::layout::size_of::<T>()),
        p@.addr + count as int * vstd::layout::size_of::<T>() <= usize::MAX,
    ensures
        ptr_add(p, count)@.addr == p@.addr + count as int * vstd::layout::size_of::<T>(),
        ptr_add(p, count)@.provenance == p@.provenance,
        ptr_add(p, count)@.metadata == p@.metadata,
{
}

/// `<*mut T>::add`, with the safety contract of the `core` documentation:
/// the byte offset `count * size_of::<T>()` fits in an `isize`, and, when
/// it is not zero, the whole range from `p` to the result is inside the
/// allocation `p` points into ([`in_alloc`]).
pub assume_specification<T>[ <*mut T>::add ](p: *mut T, count: usize) -> (r: *mut T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(p, count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_add(p, count),
    opens_invariants none
;

/// `<*const T>::add`, with the safety contract of `<*mut T>::add`: the
/// result is [`ptr_add`] of the pointer as a `*mut T`, back as a
/// `*const T` (`axiom_ptr_add_addr` gives its address).
pub assume_specification<T>[ <*const T>::add ](p: *const T, count: usize) -> (r: *const T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(p as *mut T, count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_add(p as *mut T, count) as *const T,
    opens_invariants none
;

/// Element `i` of a frozen array: a `'static` shared permission to a
/// `[T; N]` gives one to each of its elements, holding that element, at
/// [`ptr_add`] of the array's address as a `*mut T`; and the array's first
/// `i` elements lie inside the allocation it is in, in fewer than
/// `isize::MAX` bytes, so `<*mut T>::add(i)` is defined on that pointer.
/// Trusted: the `core` layout of arrays (`N` elements `size_of::<T>()`
/// apart, from the array's address; a Rust object is at most `isize::MAX`
/// bytes) and the meaning of a permission (one to an array owns its
/// elements' bytes), neither of which vstd states. Shared and `'static`
/// only, so nothing is written through either permission.
pub axiom fn axiom_array_elem<T, const N: usize>(tracked a: &'static vstd::raw_ptr::PointsTo<[T; N]>, i: usize) -> (tracked r: &'static vstd::raw_ptr::PointsTo<T>)
    requires
        i < N,
        a.is_init(),
    ensures
        in_alloc(a.ptr() as *mut T, i as int * vstd::layout::size_of::<T>()),
        i as int * vstd::layout::size_of::<T>() <= isize::MAX,
        r.ptr() == ptr_add(a.ptr() as *mut T, i),
        r.is_init(),
        r.value() == a.value()[i as int],
;

} // verus!
