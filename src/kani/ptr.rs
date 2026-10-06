//! The raw-pointer functions of `core` that `vspec/addr.rs`,
//! `vspec/ptr.rs`, `vspec/config.rs` and `vspec/sys.rs` specify, and the
//! trusted wrappers `vspec::zero::write_zero` and
//! `vspec::slice::array_get_unchecked`, checked against their contracts.
//!
//! The memory functions (`add`, `write_bytes`, `copy_nonoverlapping`,
//! `write_zero`) are checked on arrays: the contracts' `in_alloc`,
//! `valid_writes` and `valid_reads` are uninterpreted Verus predicates
//! ("the range lies in an allocation that may be read or written"),
//! read here as "the range lies inside the array", the one allocation
//! the pointer is derived from. Kani fails a harness on any access
//! outside the object a pointer is derived from and on a misaligned or
//! null pointer where `core`'s safety contract forbids one, so a passing
//! harness shows the call defined on every such range; the assertions
//! then show what it did.
//!
//! The lengths of the fills and copies are constants (loops over every
//! value), not symbolic: with a symbolic count, Kani 0.68 (CBMC 6.11)
//! writes one element too few into an array of 8-byte elements
//! (`write_bytes(p, 0, n)` with `n` assumed 1 leaves `p[0]` as it was;
//! `copy_nonoverlapping` likewise), which would hide a write past the
//! range.

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

/// S259 `<*mut T>::is_null` and S260 `<*const T>::is_null`: `r == (p@.addr
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

/// S279 `<*const T>::cast::<U>`: `r@.addr == p@.addr`, from sized and
/// unsized pointees to bytes, words and a zero-sized type, on pointers
/// to an object and from every address.
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

/// S390 `core::ptr::from_ref` and S391 `core::ptr::from_mut`: no
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

/// S348 `<*mut T>::add`: `r == ptr_add(p, count)`, `ptr_add` being the
/// uninterpreted function that `axiom_ptr_add_addr` (S338) describes:
/// the address `count * size_of::<T>()` bytes on, with `p`'s provenance
/// and metadata. At a byte, a word, a padded element and a zero-sized
/// type (for which every `count` is an empty offset).
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
}

/// The bytes of `T`s.
fn bytes_of<T>(a: &[T]) -> &[u8] {
    // SAFETY: the plain-integer `T`s of these harnesses have no padding.
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
/// also returns on a null or dangling aligned pointer (the contract's
/// empty case requires the alignment only).
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
    // SAFETY: an aligned pointer and an empty fill.
    unsafe { core::ptr::write_bytes(any_aligned::<T>(), kani::any(), 0) };
}

/// S257 `core::ptr::write_bytes::<T>`: no `ensures`; under its `requires`
/// it is defined and returns (and writes what `core` documents), at a
/// byte and a word. (Not at a zero-sized type: Kani 0.68 reports an
/// empty fill through a pointer to an array of `()` as a write to a
/// region that is not writeable.)
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

/// S258 `core::ptr::copy_nonoverlapping::<T>`: no `ensures`; under its
/// `requires` it is defined and returns (and copies what `core`
/// documents), at a byte and a word, and at a zero-sized type with every
/// count.
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

/// S409 `write_zero`: under `n == 0 || valid_writes(p, n)` (here: the
/// `n` bytes from offset `i` lie in an 8-byte buffer, every `i` and `n`,
/// or `n == 0` and `p` any pointer), it returns, and afterwards the `n`
/// bytes at `p` read zero and no other byte changed: the fact its
/// `Zeroed` token (`addr() == p@.addr`, `len() == n`) stands for.
#[kani::proof]
#[kani::unwind(10)]
fn write_zero() {
    for i in 0..=8 {
        for n in 0..=8 - i {
            let mut a: [u8; 8] = kani::any();
            let old = a;
            // SAFETY: the contract's precondition: the range lies in `a`.
            let _ = unsafe { crate::vspec::zero::write_zero(a.as_mut_ptr().add(i), n) };
            for k in 0..8 {
                assert_eq!(a[k], if k >= i && k < i + n { 0 } else { old[k] });
            }
        }
    }
    let m: usize = kani::any();
    // SAFETY: `n == 0`: an empty fill, which any (byte-aligned) pointer allows.
    let _ = unsafe { crate::vspec::zero::write_zero(core::ptr::without_provenance_mut(m), 0) };
}

/// S373 `array_get_unchecked`: under `i < N`, a reference to `a[i]`, for
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

/// S256 `addr_to_ptr`, no `requires`, `ensures p as usize == addr, p ==
/// addr_ptr(addr), p@.provenance == cast_provenance()`: for every address
/// it returns, the pointer's address is `addr`, and it is the cast
/// `addr as *mut u8` (`addr_ptr(addr)` read as that cast), the same
/// pointer on every call. The provenance clause is not checked here (it
/// stays an assumption; see the record).
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
