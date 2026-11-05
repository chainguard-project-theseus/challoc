//! The raw-pointer functions of `core` that `vspec/addr.rs`,
//! `vspec/ptr.rs`, `vspec/config.rs` and `vspec/sys.rs` specify, the
//! proof `vspec::ptr::axiom_ptr_add_addr`, the axioms `axiom_plain_*`,
//! `axiom_array_elem` and `axiom_array_view`, and the trusted wrappers
//! `vspec::addr::addr_to_ptr`, `vspec::ptr::ref_ptr`,
//! `vspec::zero::zero_raw` and `vspec::slice::array_get_unchecked`
//! (`verus/ledger/src/vspec/`), checked against their contracts. The
//! private wrapper `vspec::user::copy_raw` has its harness in its own
//! file, `vspec::user::kani_copy` (`copy_raw_copies` between two buffers,
//! `copy_raw_copies_within` inside one), checked as `zero_raw` is here.
//!
//! The memory functions (`add`, `write_bytes`, `copy_nonoverlapping`,
//! `zero_raw`) are checked on arrays: the contracts' `in_alloc`,
//! `valid_writes` and `valid_reads` are uninterpreted Verus predicates
//! ("the range lies in an allocation that may be read or written"), and
//! `zero_raw`'s permission is ghost; each is read here as "the range
//! lies inside the array", the one allocation the pointer is derived
//! from. Kani fails a harness on any access outside the object a pointer
//! is derived from, so a passing harness shows the call defined on every
//! such range; the assertions then show what it did. Kani 0.68 does not
//! check alignment on a typed read or write (`ptr::read` and `ptr::write`
//! on a misaligned pointer verify): only `copy_nonoverlapping` and
//! `write_bytes` carry their own checks for a null or misaligned pointer.
//! The harnesses of the typed reads and writes (`read_volatile`, the
//! unaligned pair) state what `core` requires of the pointer with
//! `kani::mem`'s predicates (`-Z mem-predicates`): `can_dereference`
//! holds when the pointer is non-null, aligned and in bounds and the bytes
//! are a valid value; `can_read_unaligned` and `can_write_unaligned` drop
//! the alignment (and, for a write, the value).
//!
//! The lengths of the fills and copies are constants (loops over every
//! value), not symbolic: with a symbolic count, Kani 0.68 (CBMC 6.11)
//! writes one element too few into an array of 8-byte elements
//! (`write_bytes(p, 0, n)` with `n` assumed 1 leaves `p[0]` as it was;
//! `copy_nonoverlapping` likewise), which would hide a write past the
//! range.

use crate::api::mallctl::Utilization;
use core::mem::size_of;

/// `T`'s `is_null` (both pointer kinds) on a pointer to `x` and on
/// pointers made from every address: `true` exactly at address 0.
fn is_null_on<T>(x: &mut T) {
    let p: *mut T = x;
    assert!(!p.is_null());
    assert!(!(p as *const T).is_null());
    let a: usize = kani::any();
    let q = core::ptr::without_provenance_mut::<T>(a);
    assert_eq!(q.is_null(), a == 0);
    assert_eq!((q as *const T).is_null(), a == 0);
    let e = core::ptr::with_exposed_provenance_mut::<T>(a);
    assert_eq!(e.is_null(), a == 0);
    assert_eq!((e as *const T).is_null(), a == 0);
}

/// `<*mut T>::is_null` and `<*const T>::is_null`: `r == (p@.addr
/// == 0)`, at sized types (a byte, a word, a zero-sized type) on pointers
/// to an object and on pointers from every address, and at unsized types
/// (a slice, a `str`, a trait object, whose `is_null` looks at the data
/// address only) on pointers to an object, on pointers made from every
/// address with every length (slice, `str`) and on null ones (trait
/// objects).
#[kani::proof]
fn ptr_is_null() {
    is_null_on(&mut kani::any::<u8>());
    is_null_on(&mut kani::any::<u64>());
    is_null_on(&mut ());
    let mut a: [u8; 3] = kani::any();
    let s: *mut [u8] = &mut a[..];
    assert!(!s.is_null() && !(s as *const [u8]).is_null());
    let addr: usize = kani::any();
    let len: usize = kani::any();
    let z: *mut [u8] =
        core::ptr::slice_from_raw_parts_mut(core::ptr::without_provenance_mut(addr), len);
    assert_eq!(z.is_null(), addr == 0);
    assert_eq!((z as *const [u8]).is_null(), addr == 0);
    let t: *const str = "kani";
    assert!(!t.is_null());
    let d: *const dyn core::fmt::Debug = &a;
    assert!(!d.is_null());
    let dm: *mut dyn core::fmt::Debug = &mut a;
    assert!(!dm.is_null());
    let ts = core::ptr::slice_from_raw_parts(core::ptr::without_provenance::<u8>(addr), len)
        as *const str;
    assert_eq!(ts.is_null(), addr == 0);
    let tm = core::ptr::slice_from_raw_parts_mut(core::ptr::without_provenance_mut::<u8>(addr), len)
        as *mut str;
    assert_eq!(tm.is_null(), addr == 0);
    let dn = core::ptr::null::<[u8; 1]>() as *const dyn core::fmt::Debug;
    assert!(dn.is_null());
    let dmn = core::ptr::null_mut::<[u8; 1]>() as *mut dyn core::fmt::Debug;
    assert!(dmn.is_null());
}

/// `<*const T>::cast::<U>`: no `ensures`; it returns, and (beyond the
/// contract) keeps the address, from sized and unsized pointees to bytes,
/// words and a zero-sized type, on pointers to an object and from every
/// address.
#[kani::proof]
fn ptr_cast() {
    let x: [u64; 2] = kani::any();
    let p: *const [u64; 2] = &x;
    assert_eq!(p.cast::<u8>() as usize, p as usize);
    assert_eq!(p.cast::<u64>() as usize, p as usize);
    assert_eq!(p.cast::<()>() as usize, p as usize);
    let s: *const [u64] = &x[..];
    assert_eq!(s.cast::<u8>() as usize, s as *const u64 as usize);
    let t: *const str = "kani";
    assert_eq!(t.cast::<u8>() as usize, t as *const u8 as usize);
    let a: usize = kani::any();
    let q = core::ptr::without_provenance::<u32>(a);
    assert_eq!(q.cast::<u64>() as usize, a);
    assert_eq!(q.cast::<u8>() as usize, a);
}

/// `<*const T>::wrapping_add`: no `ensures`; it returns, with no failed
/// check, for every address and every count, at the two pointee types
/// `config::linux::env_var` walks `environ` with (`c_char` and
/// `*const c_char`), on pointers with no provenance and on a pointer to
/// an object. Beyond the contract, a count that stays inside the object
/// moves the address by that many elements. (Kani keeps a pointer as an
/// object and an offset, so it cannot show the address of a step that
/// leaves the object; the contract claims none.)
#[kani::proof]
fn ptr_wrapping_add() {
    use core::ffi::c_char;
    let a: usize = kani::any();
    let n: usize = kani::any();
    let _ = core::ptr::without_provenance::<c_char>(a).wrapping_add(n);
    let _ = core::ptr::without_provenance::<*const c_char>(a).wrapping_add(n);
    let x: [*const c_char; 2] = [core::ptr::null(); 2];
    let r: *const *const c_char = &x[0];
    let _ = r.wrapping_add(n);
    let k: usize = kani::any();
    kani::assume(k <= 2);
    assert!(r.wrapping_add(k) as usize == r as usize + core::mem::size_of::<*const c_char>() * k);
    let s: [c_char; 3] = [0; 3];
    let t: *const c_char = &s[0];
    assert!(t.wrapping_add(k) as usize == t as usize + k);
}

/// `core::ptr::from_ref` and `core::ptr::from_mut`: no
/// `ensures`; they return, the referent's address, for sized and
/// unsized referents.
#[kani::proof]
fn ptr_from_ref_mut() {
    let mut x: u64 = kani::any();
    let a = &x as *const u64 as usize;
    assert_eq!(core::ptr::from_ref(&x) as usize, a);
    assert_eq!(core::ptr::from_mut(&mut x) as usize, a);
    let mut s: [u8; 3] = kani::any();
    let b = s.as_ptr() as usize;
    assert_eq!(core::ptr::from_ref::<[u8]>(&s[..]) as *const u8 as usize, b);
    assert_eq!(
        core::ptr::from_mut::<[u8]>(&mut s[..]) as *mut u8 as usize,
        b
    );
    let _ = core::ptr::from_ref::<str>("kani");
    let _ = core::ptr::from_ref(&());
}

/// `<*mut T>::add` on an array of `N` `T`s: from element `i` (`i <= N`),
/// every `count` with `i + count <= N` (the range from `p` to the result
/// lies in the allocation: the contract's `in_alloc`, with the byte
/// offset below `isize::MAX`, as an array's always is) gives the pointer
/// `count * size_of::<T>()` bytes on, of `p`'s allocation (a read
/// through it is a read of element `i + count`, which Kani accepts only
/// through a pointer derived from the array). And `add(0)` on any
/// pointer, even null or dangling, gives the pointer back (the contract's
/// empty-offset case, which needs no allocation).
fn ptr_add_on<T: Copy + PartialEq + kani::Arbitrary, const N: usize>() {
    let mut a: [T; N] = kani::any();
    let i: usize = kani::any();
    let count: usize = kani::any();
    kani::assume(i <= N && count <= N - i);
    // SAFETY: `i <= N`: inside the array or one past it.
    let p = unsafe { a.as_mut_ptr().add(i) };
    // SAFETY: the contract's precondition, `i + count <= N`.
    let r = unsafe { p.add(count) };
    assert_eq!(r as usize, p as usize + count * size_of::<T>());
    if i + count < N {
        // SAFETY: element `i + count` of `a`.
        assert!(unsafe { *r } == a[i + count]);
    }
    let d = core::ptr::without_provenance_mut::<T>(kani::any());
    // SAFETY: a zero offset, which `core` allows on any pointer.
    assert_eq!(unsafe { d.add(0) }, d);
}

/// `<*mut T>::add`: `r == ptr_add(p, count)`, and `axiom_ptr_add_addr`,
/// what `ptr_add` is under `in_alloc`: the address `count * size_of::<T>()`
/// bytes on, with `p`'s provenance (a read through it is a read of the
/// array) and metadata (`()`, `T` being sized). At a byte, a word, a
/// padded element and a zero-sized type (for which every `count` is an
/// empty offset). `<*const T>::add`, whose contract is that of
/// `<*mut T>::add` through a cast, gives the same pointer as the `*mut`
/// one at every in-bounds `count`.
#[kani::proof]
fn ptr_add() {
    ptr_add_on::<u8, 5>();
    ptr_add_on::<u64, 5>();
    ptr_add_on::<(u64, u8), 3>();
    let mut z = [(); 4];
    let p = z.as_mut_ptr();
    let count: usize = kani::any();
    // SAFETY: a zero-sized `T`: every offset is empty.
    let r = unsafe { p.add(count) };
    assert_eq!(r, p);
    let mut a: [u64; 5] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= 5);
    let m = a.as_mut_ptr();
    // SAFETY: `n <= 5`: inside the array or one past it, for both.
    let (rm, rc) = unsafe { (m.add(n), (m as *const u64).add(n)) };
    assert_eq!(rc, rm as *const u64);
}

/// The bytes of `T`s.
fn bytes_of<T>(a: &[T]) -> &[u8] {
    // SAFETY: the `T`s of these harnesses (integers, `bool`, pointers and
    // `Utilization`, `repr(C)` with six word-sized fields) have no padding.
    unsafe { core::slice::from_raw_parts(a.as_ptr().cast::<u8>(), size_of::<T>() * a.len()) }
}

/// An aligned pointer to `T` at an arbitrary address (null included),
/// derived from no object.
fn any_aligned<T>() -> *mut T {
    let m: usize = kani::any();
    core::ptr::without_provenance_mut(m & !(core::mem::align_of::<T>() - 1))
}

/// `write_bytes` on an array of `N` `T`s with arbitrary contents and an
/// arbitrary fill byte: from every element `i <= N`, every `count` with
/// `i + count <= N` (the contract's `valid_writes`; the pointer is
/// aligned, as an array element is) fills exactly the `count` elements'
/// bytes with `val` and leaves every other byte. With `count == 0` it
/// also returns on a dangling non-null aligned pointer (the contract's
/// empty case requires a non-null aligned pointer only).
fn write_bytes_on<T: Copy + kani::Arbitrary, const N: usize>() {
    for i in 0..=N {
        for count in 0..=N - i {
            let mut a: [T; N] = kani::any();
            let old = a;
            let val: u8 = kani::any();
            // SAFETY: `i <= N`: inside the array or one past it.
            let p = unsafe { a.as_mut_ptr().add(i) };
            // SAFETY: the contract's precondition.
            unsafe { core::ptr::write_bytes(p, val, count) };
            let (b, o) = (bytes_of(&a), bytes_of(&old));
            for k in 0..size_of::<T>() * N {
                let inside = k >= i * size_of::<T>() && k < (i + count) * size_of::<T>();
                assert_eq!(b[k], if inside { val } else { o[k] });
            }
        }
    }
    let d = any_aligned::<T>();
    kani::assume(!d.is_null());
    // SAFETY: a non-null aligned pointer and an empty fill.
    unsafe { core::ptr::write_bytes(d, kani::any(), 0) };
}

/// `core::ptr::write_bytes::<T>`: no `ensures`; under its `requires`
/// it is defined and returns (and writes what `core` documents), at a
/// byte and a word. The contract's `c_apart`, `blocks_apart` and
/// `c_writes` conjuncts say whose memory it is, not whether the call is
/// defined, and are not checked here. The contract requires a `T` that is
/// not zero-sized, so it says nothing of a zero-sized fill (which Kani
/// 0.68 rejects at any nonzero count as a write to a region that is not
/// writeable).
#[kani::proof]
#[kani::unwind(41)]
fn ptr_write_bytes() {
    write_bytes_on::<u8, 5>();
    write_bytes_on::<u64, 4>();
}

/// `copy_nonoverlapping` between two arrays of `N` `T`s and within one,
/// with arbitrary contents: from every `i` to every `j`, every `count`
/// with both ranges inside their array (the contract's `valid_reads`
/// and `valid_writes`) and, within one array, apart (the contract's
/// non-overlap), copies exactly the `count` elements and leaves every
/// other element. With `count == 0` it also returns on null or dangling
/// aligned pointers, equal or not.
fn copy_nonoverlapping_on<T: Copy + PartialEq + kani::Arbitrary, const N: usize>() {
    for i in 0..=N {
        for j in 0..=N {
            for count in 0..=N - if i > j { i } else { j } {
                let src: [T; N] = kani::any();
                let mut dst: [T; N] = kani::any();
                let old = dst;
                // SAFETY: the contract's precondition: two arrays never overlap.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src.as_ptr().add(i),
                        dst.as_mut_ptr().add(j),
                        count,
                    )
                };
                for k in 0..N {
                    let want = if k >= j && k < j + count {
                        src[i + k - j]
                    } else {
                        old[k]
                    };
                    assert!(dst[k] == want);
                }
                if i + count <= j || j + count <= i {
                    let mut one: [T; N] = kani::any();
                    let before = one;
                    let base = one.as_mut_ptr();
                    // SAFETY: the contract's precondition, the two ranges apart.
                    unsafe {
                        core::ptr::copy_nonoverlapping(base.add(i) as *const T, base.add(j), count)
                    };
                    for k in 0..N {
                        let want = if k >= j && k < j + count {
                            before[i + k - j]
                        } else {
                            before[k]
                        };
                        assert!(one[k] == want);
                    }
                }
            }
        }
    }
    let s = any_aligned::<T>() as *const T;
    let d = if kani::any() {
        s as *mut T
    } else {
        any_aligned::<T>()
    };
    // SAFETY: aligned pointers and an empty copy.
    unsafe { core::ptr::copy_nonoverlapping(s, d, 0) };
}

/// `core::ptr::copy_nonoverlapping::<T>`: no `ensures`; under its
/// `requires` it is defined and returns (and copies what `core`
/// documents), at a byte and a word, and at a zero-sized type with every
/// count. As for `write_bytes`, the `c_apart` and `c_writes` conjuncts
/// are not checked here.
#[kani::proof]
#[kani::unwind(7)]
fn ptr_copy_nonoverlapping() {
    copy_nonoverlapping_on::<u8, 5>();
    copy_nonoverlapping_on::<u64, 4>();
    let mut z = [(); 4];
    let zp = z.as_mut_ptr();
    // SAFETY: a zero-sized `T`: every copy is empty.
    unsafe { core::ptr::copy_nonoverlapping(zp as *const (), zp, kani::any()) };
}

/// `zero_raw`: under `p@.addr != 0` and a permission to the `n` bytes at
/// `p` (here: the `n` bytes from offset `i` lie in an 8-byte buffer, every
/// `i` and `n`; the permission is ghost, so the harness passes the erased
/// one), it returns, and afterwards the `n` bytes at `p` read zero and no
/// other byte changed: the fact its `Zeroed` token (`addr() == p@.addr`,
/// `len() == n`) stands for. With `n == 0` it also returns on a dangling
/// non-null pointer.
#[kani::proof]
#[kani::unwind(10)]
fn zero_raw_fills() {
    use crate::vspec::zero::zero_raw;
    use vstd::prelude::Tracked;
    for i in 0..=8 {
        for n in 0..=8 - i {
            let mut a: [u8; 8] = kani::any();
            let old = a;
            // SAFETY: the contract's precondition: the range lies in `a`.
            let _ = unsafe { zero_raw(a.as_mut_ptr().add(i), n, Tracked::assume_new()) };
            for k in 0..8 {
                assert_eq!(a[k], if k >= i && k < i + n { 0 } else { old[k] });
            }
        }
    }
    let m: usize = kani::any();
    kani::assume(m != 0);
    let d = core::ptr::without_provenance_mut(m);
    // SAFETY: `n == 0` on a non-null pointer: an empty fill.
    let _ = unsafe { zero_raw(d, 0, Tracked::assume_new()) };
}

/// `array_get_unchecked`: under `i < N`, a reference to `a[i]`, for
/// every array of 1, 2 or 6 words or of 3 padded pairs and every index
/// in it.
#[kani::proof]
fn array_get_unchecked() {
    fn on<T: Copy + PartialEq + kani::Arbitrary, const N: usize>() {
        let a: [T; N] = kani::any();
        let i: usize = kani::any();
        kani::assume(i < N);
        // SAFETY: the contract's precondition, `i < N`.
        let r = unsafe { crate::vspec::slice::array_get_unchecked(&a, i) };
        assert!(*r == a[i]);
        assert!(core::ptr::eq(r, &a[i]));
    }
    on::<u64, 1>();
    on::<u64, 2>();
    on::<u64, 6>();
    on::<(u64, u8), 3>();
}

/// `addr_to_ptr`, no `requires`, `ensures p as usize == addr, p ==
/// addr_ptr(addr), p@.provenance == cast_provenance()`: for every address
/// it returns, the pointer's address is `addr`, and it is the cast
/// `addr as *mut u8` (`addr_ptr(addr)` read as that cast), the same
/// pointer on every call. The provenance clause is not checked here (it
/// stays an assumption).
#[kani::proof]
fn addr_to_ptr_is_the_cast() {
    use crate::vspec::addr::addr_to_ptr;
    let addr: usize = kani::any();
    let p = addr_to_ptr(addr);
    assert_eq!(p as usize, addr);
    assert_eq!(p.addr(), addr);
    assert_eq!(p, addr as *mut u8);
    assert_eq!(addr_to_ptr(addr), p);
}

/// `ref_ptr`, no `requires`, `ensures valid_reads(r, size_of::<T>())`
/// and `r@.addr + size_of::<T>() <= usize::MAX`: for a `T` behind a
/// reference (a local, and an element and a field inside a larger
/// object), it returns the referent's address; adding `size_of::<T>()`
/// to it does not wrap; and the `size_of::<T>()` bytes from it read,
/// each byte (as `MaybeUninit<u8>`, so padding is allowed) and all at
/// once into a `MaybeUninit<T>` (`copy_nonoverlapping`, as `mallctl`'s
/// `copy_prefix` reads them), giving back the value. At `Copy` types of
/// one, four and eight bytes, a padded pair, an odd-sized array and a
/// zero-sized type.
#[kani::proof]
#[kani::unwind(17)]
fn ptr_ref_ptr() {
    use crate::vspec::ptr::ref_ptr;
    use core::mem::MaybeUninit;
    fn reads<T: Copy + PartialEq>(v: &T) {
        let r = ref_ptr(v);
        assert_eq!(r as usize, v as *const T as usize);
        assert!((r as usize).checked_add(size_of::<T>()).is_some());
        let b = r.cast::<MaybeUninit<u8>>();
        for i in 0..size_of::<T>() {
            // SAFETY: the contract's `valid_reads`, byte `i` of the range.
            let _ = unsafe { b.add(i).read() };
        }
        let mut c = MaybeUninit::<T>::uninit();
        // SAFETY: the contract's `valid_reads`, the whole range, into a
        // local of the same size.
        unsafe {
            core::ptr::copy_nonoverlapping(
                r as *const u8,
                c.as_mut_ptr() as *mut u8,
                size_of::<T>(),
            )
        };
        // SAFETY: every byte of a `T` was copied from one.
        assert!(unsafe { c.assume_init() } == *v);
        // SAFETY: as above.
        assert!(unsafe { r.read() } == *v);
    }
    fn on<T: Copy + PartialEq + kani::Arbitrary>() {
        let x: T = kani::any();
        reads(&x);
        let a: [T; 3] = kani::any();
        reads(&a[1]);
        let p: (u8, T, u16) = kani::any();
        reads(&p.1);
    }
    on::<u8>();
    on::<bool>();
    on::<u32>();
    on::<u64>();
    on::<usize>();
    on::<(u8, u64)>();
    on::<[u8; 3]>();
    on::<()>();
}

/// The plain types `axiom_plain_*` name (`u8`, `u32`, `u64`, `usize`,
/// `isize`), each built from its native-order bytes.
trait Plain: Copy + PartialEq {
    fn from_bytes(b: &[u8]) -> Self;
}

macro_rules! plain_int {
    ($($t:ty),*) => {$(
        impl Plain for $t {
            fn from_bytes(b: &[u8]) -> Self {
                let mut w = [0u8; size_of::<$t>()];
                w.copy_from_slice(b);
                <$t>::from_ne_bytes(w)
            }
        }
    )*};
}

plain_int!(u8, u32, u64, usize, isize);

/// Every `size_of::<T>()` bytes, whatever their values, are a valid `T`:
/// `kani::mem::can_read_unaligned` checks the value's validity (it fails
/// at `bool`, whose invalid bit patterns these bytes reach:
/// `plain_rejects_bool`).
fn plain_on<T>() {
    let b: [u8; 8] = kani::any();
    assert!(size_of::<T>() <= 8);
    assert!(kani::mem::can_read_unaligned(b.as_ptr() as *const T));
}

/// `axiom_plain_u8`, `axiom_plain_u32`, `axiom_plain_u64`,
/// `axiom_plain_usize`, `axiom_plain_isize` and `axiom_plain_ptr`,
/// `ensures plain::<T>()`, read as `plain`'s doc states it ("every
/// initialised value of `T`'s size is a `T`"): every byte pattern of
/// `size_of::<T>()` bytes is a valid value of the type. `axiom_plain_ptr`
/// at `*const T` for a byte, a word, `c_void` (the proofs' one
/// instance) and a zero-sized `T`: a thin pointer's validity does not
/// depend on `T`. That the value carries no provenance the reader may use
/// is a restriction on the proofs, not a fact to check.
#[kani::proof]
fn plain_bytes_are_values() {
    plain_on::<u8>();
    plain_on::<u32>();
    plain_on::<u64>();
    plain_on::<usize>();
    plain_on::<isize>();
    plain_on::<*const u8>();
    plain_on::<*const u64>();
    plain_on::<*const core::ffi::c_void>();
    plain_on::<*const ()>();
}

/// The control for `plain_bytes_are_values`: at `bool`, which has invalid
/// bit patterns, `can_read_unaligned` fails on some byte, so the check
/// there tests validity and not only bounds.
#[kani::proof]
#[kani::should_panic]
fn plain_rejects_bool() {
    plain_on::<bool>();
}

/// `read_volatile` at `T` on a 16-byte buffer of two words: from every
/// offset `i` aligned for `T` with `i + size_of::<T>() <= 16` (the
/// contract's `valid_reads` read as "the range lies inside the buffer",
/// and its alignment conjunct; `plain::<T>()` holds at these `T`s), the
/// pointer is what `core` requires (`can_dereference`: non-null, aligned,
/// in bounds, a valid `T`), and the call returns the `T` those bytes make
/// (`is`: the value is that of the bytes from `i`).
fn read_volatile_on<T: Copy>(is: fn(T, &[u8]) -> bool) {
    let a: [u64; 2] = kani::any();
    let b = bytes_of(&a);
    let mut i = 0;
    while i + size_of::<T>() <= 16 {
        // SAFETY: `i <= 16`: inside the buffer or one past it.
        let p = unsafe { b.as_ptr().add(i) } as *const T;
        assert!(kani::mem::can_dereference(p));
        // SAFETY: the contract's precondition, checked above.
        let v = unsafe { core::ptr::read_volatile(p) };
        assert!(is(v, &b[i..i + size_of::<T>()]));
        i += core::mem::align_of::<T>();
    }
}

/// `core::ptr::read_volatile::<T>`: no `ensures`; under the `requires`
/// (`plain::<T>()`, the `size_of::<T>()` bytes at `src` valid for reads,
/// `src` aligned for `T`) the call is defined and returns, at the types
/// `axiom_plain_*` give `plain` for (the integers, and `*const u8` for the
/// pointers; verified code reads `u64`), from every aligned offset of a
/// 16-byte buffer; and (beyond the contract, which states no value) it
/// returns the value of those bytes.
#[kani::proof]
#[kani::unwind(18)]
fn ptr_read_volatile() {
    read_volatile_on::<u8>(|v, b| v == u8::from_bytes(b));
    read_volatile_on::<u32>(|v, b| v == u32::from_bytes(b));
    read_volatile_on::<u64>(|v, b| v == u64::from_bytes(b));
    read_volatile_on::<usize>(|v, b| v == usize::from_bytes(b));
    read_volatile_on::<isize>(|v, b| v == isize::from_bytes(b));
    read_volatile_on::<*const u8>(|v, b| v as usize == usize::from_bytes(b));
}

/// `N` arbitrary bytes, read at every offset `i` with `i + size_of::<T>()
/// <= N` (the contract's `valid_reads(src, size_of::<T>())`, read as "the
/// range lies inside the buffer"; `plain::<T>()` holds at these `T`s), so
/// at every alignment: the pointer is what `core` requires
/// (`can_read_unaligned`: in bounds, a valid `T`, no alignment), both
/// `core::ptr::read_unaligned` and `<*const T>::read_unaligned` return,
/// and (beyond the contract, which states no value) give the `T` those
/// bytes make.
fn read_unaligned_on<T: Plain, const N: usize>() {
    let a: [u8; N] = kani::any();
    for i in 0..=N - size_of::<T>() {
        // SAFETY: `i <= N`: inside the buffer or one past it.
        let p = unsafe { a.as_ptr().add(i) } as *const T;
        assert!(kani::mem::can_read_unaligned(p));
        let want = T::from_bytes(&a[i..i + size_of::<T>()]);
        // SAFETY: the contract's precondition, checked above.
        assert!(unsafe { core::ptr::read_unaligned(p) } == want);
        // SAFETY: as above.
        assert!(unsafe { p.read_unaligned() } == want);
    }
}

/// `core::ptr::read_unaligned::<T>` and `<*const T>::read_unaligned`: no
/// `ensures`; under the `requires` (`plain::<T>()` and the `size_of::<T>()`
/// bytes at `src` valid for reads, no alignment) the call is defined and
/// returns, at every plain type `axiom_plain_*` names (the pointers at
/// `*const u8` and `*const c_void`, `mallctl`'s), from every offset of a
/// 12-byte buffer, so aligned and misaligned alike.
#[kani::proof]
#[kani::unwind(14)]
fn ptr_read_unaligned() {
    read_unaligned_on::<u8, 12>();
    read_unaligned_on::<u32, 12>();
    read_unaligned_on::<u64, 12>();
    read_unaligned_on::<usize, 12>();
    read_unaligned_on::<isize, 12>();
    read_ptr_unaligned_on::<u8>();
    read_ptr_unaligned_on::<core::ffi::c_void>();
}

/// `read_unaligned_on` at `*const T`, whose value is compared by address.
fn read_ptr_unaligned_on<T>() {
    let a: [u8; 12] = kani::any();
    for i in 0..=12 - size_of::<*const T>() {
        // SAFETY: as in `read_unaligned_on`.
        let p = unsafe { a.as_ptr().add(i) } as *const *const T;
        assert!(kani::mem::can_read_unaligned(p));
        let want = usize::from_bytes(&a[i..i + size_of::<usize>()]);
        // SAFETY: the contract's precondition.
        let r = unsafe { core::ptr::read_unaligned(p) };
        assert_eq!(r as usize, want);
        // SAFETY: as above.
        assert_eq!(unsafe { p.read_unaligned() } as usize, want);
    }
}

/// `N` arbitrary bytes and an arbitrary `T`, written at every offset `i`
/// with `i + size_of::<T>() <= N` (the contract's `valid_writes(dst,
/// size_of::<T>())`; `c_apart` and `c_writes` say whose memory it is, not
/// whether the call is defined, and are not checked here): the pointer is
/// what `core` requires (`can_write_unaligned`: in bounds, no alignment),
/// both `core::ptr::write_unaligned` and `<*mut T>::write_unaligned`
/// return, and (beyond the contract, which states no effect) write `v`'s
/// bytes there and leave every other byte.
fn write_unaligned_on<T: Copy, const N: usize>(any: fn() -> T) {
    for i in 0..=N - size_of::<T>() {
        for method in [false, true] {
            let mut a: [u8; N] = kani::any();
            let old = a;
            let v: T = any();
            // SAFETY: `i <= N`: inside the buffer or one past it.
            let p = unsafe { a.as_mut_ptr().add(i) } as *mut T;
            assert!(kani::mem::can_write_unaligned(p));
            if method {
                // SAFETY: the contract's precondition, checked above.
                unsafe { p.write_unaligned(v) };
            } else {
                // SAFETY: as above.
                unsafe { core::ptr::write_unaligned(p, v) };
            }
            let vb = bytes_of(core::slice::from_ref(&v));
            for k in 0..N {
                let want = if k >= i && k < i + size_of::<T>() {
                    vb[k - i]
                } else {
                    old[k]
                };
                assert_eq!(a[k], want);
            }
        }
    }
}

/// `core::ptr::write_unaligned::<T>` and `<*mut T>::write_unaligned`: no
/// `ensures`; under the `requires` the call is defined and returns, at
/// the word-sized types `mallctl` writes (`bool`, `u32`, `u64`, `usize`,
/// `isize`, pointers), from every offset of a 12-byte buffer, and at
/// `mallctl`'s 48-byte `Utilization`, from every offset of a 56-byte
/// buffer (every misalignment of its words).
#[kani::proof]
#[kani::unwind(57)]
fn ptr_write_unaligned() {
    write_unaligned_on::<bool, 12>(kani::any);
    write_unaligned_on::<u32, 12>(kani::any);
    write_unaligned_on::<u64, 12>(kani::any);
    write_unaligned_on::<usize, 12>(kani::any);
    write_unaligned_on::<isize, 12>(kani::any);
    write_unaligned_on::<*mut u8, 12>(|| core::ptr::without_provenance_mut(kani::any()));
    write_unaligned_on::<*const u64, 12>(|| core::ptr::without_provenance(kani::any()));
    write_unaligned_on::<Utilization, 56>(|| Utilization {
        slabcur_addr: core::ptr::without_provenance_mut(kani::any()),
        nfree: kani::any(),
        nregs: kani::any(),
        size: kani::any(),
        bin_nfree: kani::any(),
        bin_nregs: kani::any(),
    });
}

/// `<*mut T>::sub` on an array of `N` `T`s: from element `i` (`i <= N`),
/// every `count <= i` (the range from the result to `p` lies in the
/// allocation: the contract's `in_alloc` of `ptr_sub(p, count)`, and the
/// byte offset at most `p`'s address and below `isize::MAX`, as an
/// array's always is) gives the pointer `count * size_of::<T>()` bytes
/// back, of `p`'s allocation (a read through it is a read of element
/// `i - count`, which Kani accepts only through a pointer derived from
/// the array). And `sub(0)` on any pointer, even null or dangling, gives
/// the pointer back (the contract's empty-offset case).
fn ptr_sub_on<T: Copy + PartialEq, const N: usize>(any: fn() -> T) {
    let mut a: [T; N] = [any(); N];
    for e in a.iter_mut() {
        *e = any();
    }
    let i: usize = kani::any();
    let count: usize = kani::any();
    kani::assume(i <= N && count <= i);
    // SAFETY: `i <= N`: inside the array or one past it.
    let p = unsafe { a.as_mut_ptr().add(i) };
    // SAFETY: the contract's precondition, `count <= i`.
    let r = unsafe { p.sub(count) };
    assert_eq!(r as usize, p as usize - count * size_of::<T>());
    if i - count < N {
        // SAFETY: element `i - count` of `a`.
        assert!(unsafe { *r } == a[i - count]);
    }
    let d = core::ptr::without_provenance_mut::<T>(kani::any());
    // SAFETY: a zero offset, which `core` allows on any pointer.
    assert_eq!(unsafe { d.sub(0) }, d);
}

/// `<*mut T>::sub`: `r == ptr_sub(p, count)`, the address
/// `count * size_of::<T>()` bytes back, with `p`'s provenance (a read
/// through it is a read of the array) and metadata (`()`, `T` being
/// sized). At a byte and a `*mut u8` (verified code's two instances: a
/// block's bytes, a cache stack's entries), a word, a padded element and a
/// zero-sized type (for which every `count` is an empty offset).
#[kani::proof]
#[kani::unwind(7)]
fn ptr_sub() {
    ptr_sub_on::<u8, 5>(kani::any);
    ptr_sub_on::<*mut u8, 5>(|| core::ptr::without_provenance_mut(kani::any()));
    ptr_sub_on::<u64, 5>(kani::any);
    ptr_sub_on::<(u64, u8), 3>(kani::any);
    let mut z = [(); 4];
    // SAFETY: one past the end of `z`.
    let p = unsafe { z.as_mut_ptr().add(4) };
    let count: usize = kani::any();
    // SAFETY: a zero-sized `T`: every offset is empty.
    let r = unsafe { p.sub(count) };
    assert_eq!(r, p);
}

/// The array `a` at its own address: element `i`'s pointer, `i *
/// size_of::<T>()` bytes from the array's address and derived from it,
/// is in bounds (Kani fails an `add` past the array) and aligned, holds
/// `a[i]`, and is `&a[i]`, for every `i < N`; the bytes of the
/// whole array (`N * size_of::<T>()`, below `isize::MAX`) from that
/// address read as the array whose elements those are.
fn array_layout_on<T: Copy + PartialEq, const N: usize>(a: [T; N]) {
    let base: *const [T; N] = &a;
    assert!(N * size_of::<T>() <= isize::MAX as usize);
    assert!(base as usize + N * size_of::<T>() >= base as usize);
    let e0 = base as *const T;
    for i in 0..N {
        // SAFETY: `i < N`: inside the array.
        let e = unsafe { e0.add(i) };
        assert_eq!(e as usize, base as usize + i * size_of::<T>());
        assert!(kani::mem::can_dereference(e));
        // SAFETY: checked above.
        assert!(unsafe { *e } == a[i]);
        assert!(core::ptr::eq(e, &a[i]));
    }
    assert!(kani::mem::can_dereference(base));
    // SAFETY: checked above.
    let whole = unsafe { *base };
    for i in 0..N {
        // SAFETY: `i < N`, as above.
        assert!(whole[i] == unsafe { *e0.add(i) });
    }
}

/// `axiom_array_elem` and `axiom_array_view`, the `core` layout of
/// arrays they rest on: `N` elements `size_of::<T>()` apart from the
/// array's address, aligned as `T`, each holding its element, the whole
/// at most `isize::MAX` bytes and not wrapping the address space. At
/// sampled `T` and `N` only: a byte, a word, a padded pair, and the
/// class table `[Class; NCLASSES]` at `BASE_CLASSES` (the instance
/// `layout::regions`' `class_entry` takes `axiom_array_elem` at; the
/// other proof instance, `heap::others`' `[CacheHeader; NCLASSES]`, has
/// its own harness, `heap::others::kani_view::caches_array_view`). The
/// permissions themselves are ghost.
#[kani::proof]
#[kani::unwind(70)]
fn array_layout_elems() {
    array_layout_on::<u8, 3>(kani::any());
    array_layout_on::<u64, 4>(kani::any());
    array_layout_on::<(u64, u8), 3>(kani::any());
    array_layout_on(crate::layout::BASE_CLASSES);
}
