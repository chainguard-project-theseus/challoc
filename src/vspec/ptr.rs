//! Raw pointer arithmetic.

use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// `p.add(count)`, left abstract: verified code only dereferences the
/// result through a `vstd::raw_ptr::PointsTo` whose `ptr()` is this value,
/// so nothing about the address itself is needed.
pub uninterp spec fn ptr_add<T>(p: *mut T, count: usize) -> *mut T;

/// `p.sub(count)`: the pointer `count` elements before `p`, with `p`'s
/// provenance and metadata.
pub open spec fn ptr_sub<T>(p: *mut T, count: usize) -> *mut T {
    vstd::raw_ptr::ptr_mut_from_data(
        vstd::raw_ptr::PtrData::<T> {
            addr: (p@.addr - count as int * vstd::layout::size_of::<T>()) as usize,
            provenance: p@.provenance,
            metadata: p@.metadata,
        },
    )
}


/// How many bytes from `addr` to the end of the allocation that a pointer
/// with provenance `prov` and address `addr` is derived from (negative
/// when there is none). Uninterpreted: verified code cannot learn it on
/// its own. The trusted function that produced a pointer states a bound on
/// it for the offsets its callers use, or [`axiom_raw_readable`] derives
/// one from a permission to the bytes, and verified code carries that
/// bound from there. Address and provenance survive pointer casts, so the
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

/// How many bytes from `addr` are valid for reads, perhaps only reads,
/// through a pointer with provenance `prov`: as [`rw_len`], for memory a
/// C caller hands over read-only (`mallctl`'s `name`, which may be in
/// `.rodata`; `vspec::cbuf::CStr::wf`). Uninterpreted; only those facts
/// bound it.
pub uninterp spec fn r_len(prov: Provenance, addr: usize) -> int;

/// `p` is valid for writes of `len` bytes.
pub open spec fn valid_writes<T>(p: *mut T, len: int) -> bool {
    0 <= len <= rw_len(p@.provenance, p@.addr)
}

/// `p` is valid for reads of `len` bytes: inside the read-write bound, or
/// the read-only one ([`r_len`]). Read validity does not give write
/// validity: the bound it holds by may be the read-only one.
pub open spec fn valid_reads<T>(p: *const T, len: int) -> bool {
    0 <= len && (len <= rw_len(p@.provenance, p@.addr) || len <= r_len(p@.provenance, p@.addr))
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
/// Uninterpreted; [`axiom_plain_u64`], [`axiom_plain_u8`],
/// [`axiom_plain_usize`], [`axiom_plain_isize`], [`axiom_plain_u32`] and
/// [`axiom_plain_ptr`] state it for the types
/// verified code reads that way (volatile and unaligned reads).
pub uninterp spec fn plain<T>() -> bool;

/// [`plain`] for `u64`: every initialised eight bytes are a `u64`, which
/// holds no ghost state. Trusted: the `core` documentation of the integer
/// types (no invalid bit patterns).
pub axiom fn axiom_plain_u64()
    ensures
        plain::<u64>(),
;

/// [`plain`] for `u8`: every initialised byte is a `u8`. Trusted: the
/// `core` documentation of the integer types (no invalid bit patterns).
pub axiom fn axiom_plain_u8()
    ensures
        plain::<u8>(),
;

/// [`plain`] for `usize`: every initialised `size_of::<usize>()` bytes are
/// a `usize`. Trusted: the `core` documentation of the integer types (no
/// invalid bit patterns).
pub axiom fn axiom_plain_usize()
    ensures
        plain::<usize>(),
;

/// [`plain`] for `isize`: every initialised `size_of::<isize>()` bytes are
/// an `isize`. Trusted: the `core` documentation of the integer types (no
/// invalid bit patterns).
pub axiom fn axiom_plain_isize()
    ensures
        plain::<isize>(),
;

/// [`plain`] for `u32` (C's `unsigned`): every initialised four bytes are
/// a `u32`. Trusted: the `core` documentation of the integer types (no
/// invalid bit patterns).
pub axiom fn axiom_plain_u32()
    ensures
        plain::<u32>(),
;

/// [`plain`] for `*const T` (sized `T`): every initialised pointer-sized
/// bytes are a `*const T`. Trusted: the `core` documentation of raw
/// pointers (any address is a valid raw pointer value). The value carries
/// no provenance the reader may use: a pointer read from C bytes is good
/// for comparing addresses, not for access.
pub axiom fn axiom_plain_ptr<T>()
    ensures
        plain::<*const T>(),
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

/// `core::ptr::write_bytes`, with the safety contract of the `core`
/// documentation: `dst` is non-null and properly aligned and, unless the
/// fill is empty, valid for writes of `count * size_of::<T>()` bytes. Its
/// effect is not stated, so Verus assumes it changes no memory a ghost
/// permission covers: as for the unaligned writes below, unless the fill
/// is empty, the bytes must be apart from challoc's memory
/// (`vspec::cbuf::c_apart`), where verified code holds its permissions,
/// and from the slab and huge regions (`vspec::cbuf::blocks_apart`), where
/// the user blocks it hands out are, and writes through their provenance
/// must be open (`vspec::cbuf::c_writes`). Verified code fills its own
/// memory through `vspec::zero::zero_raw` and `vspec::user::zero_user`,
/// which take the permission `&mut`. `T` is not zero-sized: every caller
/// fills bytes, and the contract claims nothing about a zero-sized fill.
pub assume_specification<T>[ core::ptr::write_bytes::<T> ](dst: *mut T, val: u8, count: usize)
    requires
        vstd::layout::size_of::<T>() > 0,
        dst@.addr != 0,
        dst@.addr as int % vstd::layout::align_of::<T>() as int == 0,
        count as int * vstd::layout::size_of::<T>() == 0 || {
            &&& valid_writes(dst, count as int * vstd::layout::size_of::<T>())
            &&& crate::vspec::cbuf::c_apart(dst@.addr as int, count as int * vstd::layout::size_of::<T>())
            &&& crate::vspec::cbuf::blocks_apart(dst@.addr as int, count as int * vstd::layout::size_of::<T>())
            &&& crate::vspec::cbuf::c_writes(dst@.provenance)
        },
;

/// A permission to the `len` bytes from `p`, of `p`'s provenance, makes
/// them valid for reads through `p` and, when there are some, puts them
/// inside the allocation `p` is derived from ([`in_alloc`]). It gives no
/// write validity, so a view of the permission that may be stale (a
/// borrow of a `vspec::zero::ZeroRaw` or `vspec::user::UserMem` whose
/// bytes another holder writes) cannot be used to write. Trusted: what a
/// `PointsToRaw` stands for. The permissions verified code holds start as
/// committed memory (`vspec::unbacked::Unbacked::commit`) or as a lazily
/// committed table (`Unbacked::into_lazy`) and are split and joined from
/// there, all inside one mapping (the metadata region, the only
/// `Unbacked` token's source), or as a slot's or a huge run's usable
/// bytes (`heap::live`'s `axiom_hand_out`: a slot of a committed slab,
/// inside the one class superblock mapping, or pages `Huge::prepare`
/// committed, inside the one huge-region mapping), or as a committed
/// `Fresh` slab's slots (`class::axiom_fresh_slab_mem`, inside the class
/// superblock mapping), so one permission's range never spans two
/// allocations. Each of those mappings is read-write, so the holder of
/// the permission itself, taken `&mut`, may also write the bytes (through
/// a typed `PointsTo` split from it, `vspec::zero::zero_raw` or
/// `vspec::user`'s `copy_raw`, which trust this); this axiom states only
/// the read half. A table's bytes are valid only once its owner has
/// committed their page, the premise of `into_lazy`, which a caller of
/// this axiom keeps too.
#[verifier::external_body]
pub proof fn axiom_raw_readable(tracked raw: &vstd::raw_ptr::PointsToRaw, p: *mut u8, len: int)
    requires
        raw.contains_range(p@.addr as int, len),
        raw.provenance() == p@.provenance,
        0 <= len,
    ensures
        valid_reads(p as *const u8, len),
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

/// `<*mut T>::sub`, with the safety contract of the `core` documentation:
/// the byte offset `count * size_of::<T>()` fits in an `isize` and is at
/// most `p`'s address, and, when it is not zero, the whole range from
/// the result to `p` is inside the allocation `p` points into
/// ([`in_alloc`] of the result). Trusted: the `core` documentation of
/// `sub` (an offset of `count * size_of::<T>()` bytes down, keeping `p`'s
/// provenance and metadata), which Verus does not model for raw pointers.
pub assume_specification<T>[ <*mut T>::sub ](p: *mut T, count: usize) -> (r: *mut T)
    requires
        count as int * vstd::layout::size_of::<T>() <= isize::MAX,
        count as int * vstd::layout::size_of::<T>() <= p@.addr,
        count as int * vstd::layout::size_of::<T>() == 0 || in_alloc(ptr_sub(p, count), count as int * vstd::layout::size_of::<T>()),
    ensures
        r == ptr_sub(p, count),
    opens_invariants none
;

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

/// Element `i` of the `[T; N]` at `a`: `size_of::<T>()` bytes apart from
/// `a`'s address, with its provenance.
pub open spec fn array_elem_ptr<T, const N: usize>(a: *mut [T; N], i: int) -> *mut T {
    vstd::raw_ptr::ptr_mut_from_data(
        vstd::raw_ptr::PtrData::<T> {
            addr: (a@.addr + i * vstd::layout::size_of::<T>()) as usize,
            provenance: a@.provenance,
            metadata: (),
        },
    )
}

/// A shared view of the `[T; N]` at `a` lent by its elements' permissions:
/// `perms` holds an initialised permission to each element
/// ([`array_elem_ptr`]), so it holds the array, whose value is theirs, for
/// as long as they stay borrowed. Trusted: the `core` layout of arrays
/// (`N` elements `size_of::<T>()` apart from the array's address, aligned
/// as `T`) and the meaning of a permission (one to an array owns its
/// elements' bytes), neither of which vstd states (vstd joins no typed
/// permissions; [`axiom_array_elem`] is the other direction, for frozen
/// arrays). Shared only: nothing is written through either side while the
/// borrow lives, and no permission is duplicated that could be written.
/// The array has an element (element 0's permission gives it the non-null,
/// `T`-aligned address an array of `T` needs) and does not wrap the
/// address space (so its elements are the permissions' objects). Used only
/// at `[CacheHeader; NCLASSES]`, in `heap::others::node_view`, which
/// `array_view_tests` pins and `heap::others::kani_view::caches_array_view`
/// checks.
pub axiom fn axiom_array_view<'a, T, const N: usize>(tracked perms: &'a Map<*mut T, vstd::raw_ptr::PointsTo<T>>, a: *mut [T; N]) -> (tracked r: &'a vstd::raw_ptr::PointsTo<[T; N]>)
    requires
        0 < N,
        0 < vstd::layout::size_of::<T>(),
        a@.addr + N * vstd::layout::size_of::<T>() <= usize::MAX,
        forall|i: int| #![trigger array_elem_ptr(a, i)] 0 <= i < N ==> perms.contains_key(array_elem_ptr(a, i)),
        forall|i: int| #![trigger array_elem_ptr(a, i)] 0 <= i < N ==> perms[array_elem_ptr(a, i)].ptr() == array_elem_ptr(a, i),
        forall|i: int| #![trigger array_elem_ptr(a, i)] 0 <= i < N ==> perms[array_elem_ptr(a, i)].is_init(),
    ensures
        r.ptr() == a,
        r.is_init(),
        forall|i: int| 0 <= i < N ==> #[trigger] r.value()[i] == perms[array_elem_ptr(a, i)].value(),
;

} // verus!

verus! {

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
// `T`: the reads require [`plain`], as `read_volatile` does, which only
// the `axiom_plain_*` axioms give, for types valid for any initialised
// bytes. That the bytes are initialised, and not changed by another thread
// during the call, is premise C2 of `verus/kani/premises.md` for the three
// verified reads (`load`, `query_ptr` and `mib_in` in `api::mallctl`), each
// of the C program's `newp` or MIB buffer
// (`api::mallctl::tests::c_buffer_reads_are_listed`).

/// `core::ptr::read_unaligned`.
pub assume_specification<T>[ core::ptr::read_unaligned::<T> ](src: *const T) -> T
    requires
        plain::<T>(),
        valid_reads(src, vstd::layout::size_of::<T>() as int),
    opens_invariants none
;

/// `<*const T>::read_unaligned`.
pub assume_specification<T>[ <*const T>::read_unaligned ](src: *const T) -> T
    requires
        plain::<T>(),
        valid_reads(src, vstd::layout::size_of::<T>() as int),
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
    if exists|x: int| #[trigger] a.dom().contains(x) && b.dom().contains(x) {
        let x = choose|x: int| #[trigger] a.dom().contains(x) && b.dom().contains(x);
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

/// The instance of `axiom_array_view` the proofs use, against the Kani
/// harness that checks it (`verus/ledger/src/vspec/ptr.rs.md`).
#[cfg(test)]
mod array_view_tests {
    use crate::testutil::grant_sites;
    use std::vec::Vec;

    /// `axiom_array_view` is generic in `T` and `N`, and the harness
    /// `heap::others::kani_view::caches_array_view` checks it only at
    /// `[CacheHeader; NCLASSES]`. Outside comments, the name appears on
    /// two lines only: its definition here, and one call, with no
    /// turbofish, spelled as below, in `heap::others::node_view`, as that
    /// function's last expression, so its result is what `node_view`
    /// returns, a `&'a PointsTo<[CacheHeader; NCLASSES]>` by its
    /// signature, which fixes `T` and `N` (no line between that signature
    /// and the call is a `}` at column 0, so the call is in its body); the
    /// argument `a` is bound once in the function, to `node_arr(o)`. Any
    /// other line naming it (a
    /// second call, a generic use, a turbofish, a `use` with or without a
    /// rename) fails the test.
    #[test]
    fn array_view_only_at_the_checked_instance() {
        // Spelled in parts, so that this test's lines match nothing.
        let name = concat!("axiom_array", "_view");
        let def = concat!("pub axiom fn axiom_array", "_view<'a, T, const N: usize>(");
        let call = concat!("crate::vspec::ptr::axiom_array", "_view(&perms.hdrs, a)");
        let view = "proof fn node_view<'a>(";
        let ret = "-> (tracked r: &'a PointsTo<[CacheHeader; NCLASSES]>)";
        let mut defs = 0;
        let mut calls = Vec::new();
        for (sig, at) in grant_sites(name) {
            let text = at
                .split_once(": `")
                .map_or("", |(_, t)| t.strip_suffix('`').unwrap_or(t));
            if at.starts_with("vspec/ptr.rs:") && text.starts_with(def) {
                defs += 1;
            } else if at.starts_with("heap/others.rs:") && sig.starts_with(view) && text == call {
                calls.push(at);
            } else {
                panic!(
                    "{at}: got = `{name}` in `{sig}`, want = only its definition and the one \
                     call in `node_view`, spelled `{call}`"
                );
            }
        }
        assert_eq!(defs, 1, "got = {defs} definitions of `{name}`, want = 1");
        assert_eq!(calls.len(), 1, "got = calls {calls:?}, want = 1");

        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/heap/others.rs"),
        )
        .unwrap_or_else(|e| panic!("heap/others.rs: got = {e}, want = its text"));
        let lines: Vec<&str> = text.lines().collect();
        let line: usize = calls[0]
            .split(':')
            .nth(1)
            .and_then(|l| l.parse().ok())
            .unwrap_or_else(|| panic!("{}: got = no line number, want = one", calls[0]));
        let start = (0..line)
            .rev()
            .find(|&i| lines[i].starts_with(view))
            .unwrap_or_else(|| panic!("heap/others.rs: got = no `{view}` above, want = one"));
        assert!(
            lines[start].ends_with(ret),
            "node_view: got = `{}`, want = a signature ending `{ret}`",
            lines[start]
        );
        assert_eq!(
            lines.get(line).copied(),
            Some("}"),
            "{}: got = no `}}` after the call, want = the call ends `node_view`",
            calls[0]
        );
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let body = &lines[start + 1..line - 1];
        assert!(
            !body.contains(&"}"),
            "node_view: got = an item closing between its signature and the call, want = none"
        );
        let binds: Vec<&str> = body
            .iter()
            .map(|l| l.trim())
            .filter(|l| {
                l.strip_prefix("let ").is_some_and(|l| {
                    l.split_once('=')
                        .map_or(l, |(p, _)| p)
                        .split(|c: char| !ident(c))
                        .any(|w| w == "a")
                })
            })
            .collect();
        assert_eq!(
            binds,
            ["let a = node_arr(o);"],
            "node_view: got = bindings {binds:?} of `a`, want = one, to `node_arr(o)`"
        );
        let count = |want: &str| lines.iter().filter(|l| **l == want).count();
        let arr =
            "pub open spec fn node_arr(n: *mut ArenaCaches) -> *mut [CacheHeader; NCLASSES] {";
        let got = count(arr);
        assert_eq!(
            got, 1,
            "heap/others.rs: got = {got} lines `{arr}`, want = 1"
        );
        let harness = "    fn caches_array_view() {";
        let got = count(harness);
        assert_eq!(
            got, 1,
            "heap/others.rs: got = {got} lines `{harness}`, want = 1"
        );
    }
}
