//! Raw pointer arithmetic, and the validity of raw memory for the `core`
//! functions that take raw pointers.

use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::raw_ptr::Provenance;

verus! {

/// `p.add(count)`, left abstract: verified code only dereferences the
/// result through a `vstd::raw_ptr::PointsTo` whose `ptr()` is this value,
/// so nothing about the address itself is needed.
pub uninterp spec fn ptr_add<T>(p: *mut T, count: usize) -> *mut T;

/// How many bytes from `addr` to the end of the allocation that a pointer
/// with provenance `prov` and address `addr` is derived from (negative
/// when there is none). Uninterpreted: verified code cannot learn it on
/// its own. The trusted function that produced a pointer, or an axiom
/// about the reservation it came from, states a bound on it for the
/// offsets its callers use, and verified code carries that bound from
/// there. Address and provenance survive pointer casts, so the bound does
/// too.
pub uninterp spec fn alloc_len(prov: Provenance, addr: usize) -> int;

/// How many bytes from `addr` are valid for reads and writes through a
/// pointer with provenance `prov`: they lie inside one live allocation
/// that pointer is derived from, and the memory may be read and written.
/// Uninterpreted, as [`alloc_len`]. challoc's own memory is all readable
/// and writable, so one bound serves reads and writes. The model has no
/// memory state: a bound holds at the point it is established (a free or
/// a decommit ends it), and verified code uses each one right away.
pub uninterp spec fn rw_len(prov: Provenance, addr: usize) -> int;

/// How many bytes from `addr` are valid for reads, perhaps only reads,
/// through a pointer with provenance `prov`: as [`rw_len`], for memory a
/// C caller hands over read-only (`mallctl`'s `name` and `newp`, which may
/// be in `.rodata`; `vspec::cbuf::c_readable`). Uninterpreted; only those
/// facts bound it.
pub uninterp spec fn r_len(prov: Provenance, addr: usize) -> int;

/// The `len` bytes from `p` lie inside the allocation `p` is derived from
/// (the range may end one past its last byte): the bounds condition of
/// `<*mut T>::add`.
pub open spec fn in_alloc<T>(p: *mut T, len: int) -> bool {
    0 <= len <= alloc_len(p@.provenance, p@.addr)
}

/// `p` is valid for writes of `len` bytes.
pub open spec fn valid_writes<T>(p: *mut T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// `p` is valid for reads of `len` bytes: by the read-write bound, or by
/// the read-only one ([`r_len`]).
pub open spec fn valid_reads<T>(p: *const T, len: int) -> bool {
    0 <= len && (len <= rw_len(p@.provenance, p@.addr) || len <= r_len(p@.provenance, p@.addr))
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

} // verus!

verus! {

// The unaligned reads and writes of `mallctl`'s value transfer, through
// the C caller's buffers (`vspec::cbuf::c_buffer`). Each has the safety
// contract of the `core` documentation: the pointer is valid for reads or
// writes of `size_of::<T>()` bytes, with no alignment needed. As for
// `copy_nonoverlapping` (`vspec::addr`), the effect is not stated, so
// Verus assumes a write changes no memory a ghost permission covers: the
// writes require the bytes apart from the metadata mapping and the
// `Shared` static (`vspec::cbuf::c_apart`), where verified code holds its
// permissions, and the other holders (a block's `UserMem`, the frozen
// init-once statics, lock values, the fork slot, `errno`) are memory no
// contract-keeping C argument names while they are held (the `cbuf`
// module doc), so the writes are for the C caller's buffers only; and the
// writes through the pointer's provenance open (`vspec::cbuf::c_writes`:
// the call's token was taken `&mut` and mints no read permission from
// then on; a caller's buffer is writable only under it, `c_buffer`), so
// no permission the token minted is held across one. The value a read
// returns is not stated either. A read also needs the bytes to be a valid
// `T`, which no precondition here can say: the `T`s read (integers and
// raw pointers, by `mallctl`'s `load` and `query_ptr`, the only verified
// callers) are valid for any initialised bytes.

/// `core::ptr::read_unaligned`.
pub assume_specification<T>[ core::ptr::read_unaligned::<T> ](src: *const T) -> T
    requires
        valid_reads(src, vstd::layout::size_of::<T>() as int),
    opens_invariants none
;

/// `<*const T>::read_unaligned`.
pub assume_specification<T>[ <*const T>::read_unaligned ](src: *const T) -> T
    requires
        valid_reads(src, vstd::layout::size_of::<T>() as int),
    opens_invariants none
;

/// Every initialised value of `T`'s size is a `T` with no ghost content:
/// a volatile read of a `T` makes a valid value out of any bytes.
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
/// aligned for `T`, and the bytes are a valid `T`, which the spec asks of
/// every value of the type ([`plain`]: only `u64`). The value read is not
/// stated.
pub assume_specification<T>[ core::ptr::read_volatile::<T> ](src: *const T) -> T
    requires
        plain::<T>(),
        valid_reads(src, vstd::layout::size_of::<T>() as int),
        src@.addr as int % vstd::layout::align_of::<T>() as int == 0,
    opens_invariants none
;

/// `core::ptr::write_unaligned`.
pub assume_specification<T>[ core::ptr::write_unaligned::<T> ](dst: *mut T, src: T)
    requires
        valid_writes(dst, vstd::layout::size_of::<T>() as int),
        crate::vspec::cbuf::c_apart(dst@.addr as int, vstd::layout::size_of::<T>() as int),
        crate::vspec::cbuf::c_writes(dst@.provenance),
    opens_invariants none
;

/// `<*mut T>::write_unaligned`.
pub assume_specification<T>[ <*mut T>::write_unaligned ](dst: *mut T, val: T)
    requires
        valid_writes(dst, vstd::layout::size_of::<T>() as int),
        crate::vspec::cbuf::c_apart(dst@.addr as int, vstd::layout::size_of::<T>() as int),
        crate::vspec::cbuf::c_writes(dst@.provenance),
    opens_invariants none
;

} // verus!

verus! {

/// A permission to the `len` bytes from `p`, of `p`'s provenance, makes
/// them valid for reads and writes through `p` and puts them inside the
/// allocation `p` is derived from ([`in_alloc`]). Trusted: what a
/// `PointsToRaw` stands for. The permissions verified code holds start
/// as committed memory (`vspec::unbacked::Unbacked::commit`) and are split
/// and joined from there, all inside one mapping (the metadata
/// region, the only `Unbacked` token's source), so one permission's range
/// never spans two allocations; `Meta::free` takes back the permissions
/// its callers hold (with their `MetaClaim`s), except that `Heap::destroy`'s
/// trusted body mints the ones it gives back.
#[verifier::external_body]
pub proof fn axiom_raw_valid(tracked raw: &vstd::raw_ptr::PointsToRaw, p: *mut u8, len: int)
    requires
        raw.contains_range(p@.addr as int, len),
        raw.provenance() == p@.provenance,
        0 <= len,
    ensures
        valid_writes(p, len),
        valid_reads(p as *const u8, len),
        in_alloc(p, len),
{
}

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

/// `ptr::from_ref(v)`: the address of the `T` behind a reference, valid
/// for reads of its `size_of::<T>()` bytes, which do not wrap the address
/// space. Trusted: a reference points to a live, initialised `T` (the
/// `core::ptr` documentation's "Safety"), which Verus does not relate to
/// an address. Nothing is claimed about which memory it is, nor that
/// its padding bytes are initialised (they are copied as bytes only:
/// `mallctl`'s `copy_prefix`, the one caller). `T: Copy` keeps out
/// `UnsafeCell`s and atomics, whose bytes another thread may write.
#[verifier::external_body]
#[inline(always)]
pub fn ref_ptr<T: Copy>(v: &T) -> (r: *const T)
    ensures
        valid_reads(r, vstd::layout::size_of::<T>() as int),
        r@.addr + vstd::layout::size_of::<T>() <= usize::MAX,
{
    core::ptr::from_ref(v)
}

} // verus!

verus! {

/// Two permissions own no byte in common: a byte in both would give two
/// `PointsTo<u8>` for one address, which `PointsTo::is_disjoint` rules
/// out. Takes and returns both, unchanged.
pub proof fn lemma_raw_disjoint(
    tracked a: vstd::raw_ptr::PointsToRaw,
    tracked b: vstd::raw_ptr::PointsToRaw,
) -> (tracked r: (vstd::raw_ptr::PointsToRaw, vstd::raw_ptr::PointsToRaw))
    requires
        forall|x: int| #[trigger] a.dom().contains(x) ==> 0 <= x <= usize::MAX,
    ensures
        a.dom().disjoint(b.dom()),
        r == (a, b),
        r.0.dom() == a.dom(),
        r.0.provenance() == a.provenance(),
        r.1.dom() == b.dom(),
        r.1.provenance() == b.provenance(),
{
    if exists|x: int| a.dom().contains(x) && b.dom().contains(x) {
        let x = choose|x: int| a.dom().contains(x) && b.dom().contains(x);
        let tracked (ax, _arest) = a.split(set![x]);
        let tracked (bx, _brest) = b.split(set![x]);
        broadcast use {vstd::layout::align_of_u8, vstd::raw_ptr::axiom_ptr_mut_from_data};
        assert(ax.dom() =~= Set::range(x, x + 1));
        assert(bx.dom() =~= Set::range(x, x + 1));
        let tracked mut pa = ax.into_typed::<u8>(x as usize);
        let tracked pb = bx.into_typed::<u8>(x as usize);
        pa.is_disjoint(&pb);
        assert(false);
        (pa.into_raw(), pb.into_raw())
    } else {
        assert(a.dom().disjoint(b.dom()));
        (a, b)
    }
}

} // verus!
