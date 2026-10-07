//! The `Option`, `Result`, `Layout`, `str`, `CStr` and `fmt::Error`
//! methods of `core` that `vspec/heap.rs`, `vspec/misc.rs`,
//! `vspec/sys.rs`, `vspec/layout.rs` and `vspec/config.rs` specify,
//! checked against their `assume_specification`s.
//!
//! The closure-taking methods are checked with a closure that records
//! the argument it was called with and returns an arbitrary function of
//! it: `f.ensures((v,), r)` becomes "`f` was called once, with `v`, and
//! `r` is what it returned", and `f.requires` holding only on the stated
//! inputs becomes "`f` was not called otherwise".

use core::alloc::Layout;
use core::cell::Cell;

/// An arbitrary `Option<u64>`.
fn any_option() -> Option<u64> {
    if kani::any() { Some(kani::any()) } else { None }
}

/// An arbitrary `Result<u64, u32>`.
fn any_result() -> Result<u64, u32> {
    if kani::any() {
        Ok(kani::any())
    } else {
        Err(kani::any())
    }
}

/// S317 `Option::<T>::or_else`: `option` itself when it is `Some` (and `f` not
/// called), else what `f()` returned.
#[kani::proof]
fn option_or_else() {
    let o = any_option();
    let out = any_option();
    let calls = Cell::new(0u32);
    let r = o.or_else(|| {
        calls.set(calls.get() + 1);
        out
    });
    match o {
        Some(_) => {
            assert_eq!(r, o);
            assert_eq!(calls.get(), 0);
        }
        None => {
            assert_eq!(r, out);
            assert_eq!(calls.get(), 1);
        }
    }
}

/// S333 `Option::<T>::map_or`: `default` for `None` (and `f` not called),
/// `f(v)` for `Some(v)`.
#[kani::proof]
fn option_map_or() {
    let o = any_option();
    let default: u32 = kani::any();
    let (m, c): (u64, u64) = (kani::any(), kani::any());
    let arg = Cell::new(None);
    let r = o.map_or(default, |v| {
        assert!(arg.get().is_none());
        arg.set(Some(v));
        (v.wrapping_mul(m) ^ c) as u32
    });
    match o {
        None => {
            assert_eq!(r, default);
            assert!(arg.get().is_none());
        }
        Some(v) => {
            assert_eq!(arg.get(), Some(v));
            assert_eq!(r, (v.wrapping_mul(m) ^ c) as u32);
        }
    }
}

/// S392 `Result::<T, E>::map_or`: `default` for `Err` (and `f` not called),
/// `f(v)` for `Ok(v)`.
#[kani::proof]
fn result_map_or() {
    let res = any_result();
    let default: u32 = kani::any();
    let (m, c): (u64, u64) = (kani::any(), kani::any());
    let arg = Cell::new(None);
    let r = res.map_or(default, |v| {
        assert!(arg.get().is_none());
        arg.set(Some(v));
        (v.wrapping_mul(m) ^ c) as u32
    });
    match res {
        Err(_) => {
            assert_eq!(r, default);
            assert!(arg.get().is_none());
        }
        Ok(v) => {
            assert_eq!(arg.get(), Some(v));
            assert_eq!(r, (v.wrapping_mul(m) ^ c) as u32);
        }
    }
}

/// S393 `Option::<T>::is_some_and`: `false` for `None` (and `f` not called),
/// `f(v)` for `Some(v)`.
#[kani::proof]
fn option_is_some_and() {
    let o = any_option();
    let (m, c): (u64, u64) = (kani::any(), kani::any());
    let arg = Cell::new(None);
    let r = o.is_some_and(|v| {
        assert!(arg.get().is_none());
        arg.set(Some(v));
        v.wrapping_mul(m) < c
    });
    match o {
        None => {
            assert!(!r);
            assert!(arg.get().is_none());
        }
        Some(v) => {
            assert_eq!(arg.get(), Some(v));
            assert_eq!(r, v.wrapping_mul(m) < c);
        }
    }
}

/// S394 `Result::<T, E>::unwrap_or`: the `Ok` value, else `default`.
#[kani::proof]
fn result_unwrap_or() {
    let res = any_result();
    let default: u64 = kani::any();
    let r = res.unwrap_or(default);
    match res {
        Ok(v) => assert_eq!(r, v),
        Err(_) => assert_eq!(r, default),
    }
}

/// S402 `Option::<&T>::copied`: `Some(*v)` for `Some(v)`, else `None`.
#[kani::proof]
fn option_copied() {
    let x: u64 = kani::any();
    let o = if kani::any() { Some(&x) } else { None };
    let r = o.copied();
    match o {
        Some(v) => assert_eq!(r, Some(*v)),
        None => assert_eq!(r, None),
    }
}

/// S338 `Option::<T>::as_deref_mut`: no `ensures`; it returns without a panic
/// (here: `None` for `None`, else a reborrow of the contents, through
/// which a write lands in the option).
#[kani::proof]
fn option_as_deref_mut() {
    let mut x: [u64; 1] = kani::any();
    let v: u64 = kani::any();
    let some: bool = kani::any();
    let mut o: Option<&mut [u64; 1]> = if some { Some(&mut x) } else { None };
    match o.as_deref_mut() {
        Some(r) => {
            assert!(some);
            r[0] = v;
        }
        None => assert!(!some),
    }
    if some {
        assert_eq!(x[0], v);
    }
}

/// `vspec::layout::layout_ok` of a layout, reading `layout_size` and
/// `layout_align` as `Layout::size` and `Layout::align`: the alignment is
/// `pow2(k)` for a `k < 64`, and `size + align - 1 <= isize::MAX` in
/// `int`.
fn layout_ok(l: &Layout) -> bool {
    l.align().is_power_of_two() && l.size() as u128 + l.align() as u128 - 1 <= isize::MAX as u128
}

/// S324 `Layout::from_size_align`, S322 `Layout::size` and S327
/// `Layout::align`: `Ok` exactly when `align > 0`, `align & (align - 1)
/// == 0` and `size <= isize::MAX - (align - 1)` (in `int`), and then
/// `size()` and `align()` give the arguments back and the layout is
/// `layout_ok`, for every `size` and `align`.
#[kani::proof]
fn layout_from_size_align() {
    let size: usize = kani::any();
    let align: usize = kani::any();
    let r = Layout::from_size_align(size, align);
    let ok = align > 0
        && align & align.wrapping_sub(1) == 0
        && size as i128 <= isize::MAX as i128 - (align as i128 - 1);
    assert_eq!(r.is_ok(), ok);
    if let Ok(l) = r {
        assert_eq!(l.size(), size);
        assert_eq!(l.align(), align);
        assert!(layout_ok(&l));
    }
}

/// `Layout::new::<T>()` gives `size_of::<T>()` and `align_of::<T>()`, and
/// is `layout_ok`.
fn layout_new<T>() {
    let l = Layout::new::<T>();
    assert_eq!(l.size(), core::mem::size_of::<T>());
    assert_eq!(l.align(), core::mem::align_of::<T>());
    assert!(layout_ok(&l));
}

/// S325 `Layout::new::<T>`, with S326 and S327 on its result, at the
/// types the proofs build layouts of (`SetTable`, `Arena`, `Heap`,
/// `HugeDesc`: `grep -rn Layout::new src`) and
/// at a zero-sized and an over-aligned type.
#[kani::proof]
fn layout_new_instances() {
    layout_new::<u8>();
    layout_new::<u64>();
    layout_new::<()>();
    layout_new::<[u64; 0]>();
    layout_new::<crate::class::SetTable>();
    layout_new::<crate::arena::Arena>();
    layout_new::<crate::heap::Heap>();
    layout_new::<crate::huge::HugeDesc>();
    #[repr(align(4096))]
    struct Page([u8; 4096]);
    layout_new::<Page>();
}

/// S334 `str::as_ptr`: no `ensures`; it returns, the address of the first
/// byte.
#[kani::proof]
fn str_as_ptr() {
    let s = "challoc";
    assert!(core::ptr::eq(s.as_ptr(), &s.as_bytes()[0]));
    let e = "";
    let _ = e.as_ptr();
}

/// S283 `core::ffi::CStr::as_ptr`: no `ensures`; it returns, the address of the
/// first byte, for every C string up to 3 bytes before its NUL (built at
/// run time: Kani 0.68 has no C string literals).
#[kani::proof]
#[kani::unwind(6)]
fn cstr_as_ptr() {
    let a: [u8; 4] = kani::any();
    let n: usize = kani::any();
    kani::assume(n < 4 && a[n] == 0 && a[..n].iter().all(|&b| b != 0));
    let s = core::ffi::CStr::from_bytes_with_nul(&a[..=n]).unwrap();
    assert!(core::ptr::eq(s.as_ptr() as *const u8, &a[0]));
}

/// S397 `core::str::from_utf8`: no `ensures`; it returns without a panic
/// for every byte string up to 4 bytes (one UTF-8 sequence's longest).
#[kani::proof]
#[kani::unwind(6)]
fn str_from_utf8() {
    let a: [u8; 4] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= 4);
    if let Ok(s) = core::str::from_utf8(&a[..n]) {
        assert_eq!(s.as_bytes(), &a[..n]);
    }
}

/// S399 `<core::fmt::Error as core::default::Default>::default`: no `ensures`; it returns.
#[kani::proof]
fn fmt_error_default() {
    let e = <core::fmt::Error as Default>::default();
    assert_eq!(e, core::fmt::Error);
}

/// Writes its string through `Formatter::write_str`.
struct WriteStr<'a>(&'a str);

impl core::fmt::Display for WriteStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

/// S311 `core::fmt::Formatter::<'a>::write_str`: no `requires`, no `ensures`; it returns,
/// for every string up to 4 bytes, under the plain and a padded formatter
/// and whatever the writer returns.
#[kani::proof]
#[kani::unwind(6)]
fn formatter_write_str() {
    let a: [u8; 4] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= 4);
    if let Ok(s) = core::str::from_utf8(&a[..n]) {
        super::fmt::formats(&WriteStr(s));
    }
}

/// `axiom_ptr_add_addr::<T>` on arrays of `N` `T`s: for `p` at element
/// `i` and `i + count <= N` (the range lies in the array, `in_alloc`; the
/// sum cannot wrap, an array being an allocation), `p.add(count)` is
/// `count * size_of::<T>()` bytes on, and a read through it is a read of
/// element `i + count` of the same array (Kani rejects a read through a
/// pointer outside the object it was derived from, so the provenance is
/// `p`'s). `T: Sized`, so the metadata is `()` on both sides.
fn ptr_add_addr<T: Copy + PartialEq + kani::Arbitrary, const N: usize>() {
    let mut a: [T; N] = kani::any();
    let i: usize = kani::any();
    let count: usize = kani::any();
    kani::assume(i <= N && count <= N - i);
    let base = a.as_mut_ptr();
    // SAFETY: `i <= N`: inside the array or one past it.
    let p = unsafe { base.add(i) };
    // SAFETY: `i + count <= N`, as above.
    let q = unsafe { p.add(count) };
    assert_eq!(q as usize, p as usize + count * core::mem::size_of::<T>());
    if i + count < N {
        // SAFETY: element `i + count` of `a`.
        assert!(unsafe { *q } == a[i + count]);
    }
}

/// S354 `axiom_ptr_add_addr`, at a byte, a word and a padded element type.
#[kani::proof]
fn ptr_add_addr_instances() {
    ptr_add_addr::<u8, 5>();
    ptr_add_addr::<u64, 5>();
    ptr_add_addr::<(u64, u8), 3>();
}

/// Writes through `Formatter::write_fmt`: a literal alone (`fmt::write`'s
/// path for arguments that are one static string), a literal of 128
/// bytes and more between placeholders (its long-piece path), and
/// placeholders with and without options.
struct WriteFmt<'a>(&'a str, u64);

impl core::fmt::Display for WriteFmt<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("challoc"))?;
        f.write_fmt(format_args!(
            "{}: a piece of more than one hundred and twenty-eight bytes, \
             which fmt::write passes to the writer by its own path, the one \
             it takes for every literal of that length or longer: {}",
            self.0, self.1
        ))?;
        f.write_fmt(format_args!("<{}|{:>6}|{:#x}>", self.0, self.1, self.1))
    }
}

/// S312 `core::fmt::Formatter::<'a>::write_fmt`: no `requires`, no `ensures`; it returns,
/// for the arguments above with every string up to 4 bytes and every
/// number, whatever the writer returns. (`formats` also runs it under a
/// padded formatter, which `write_fmt` ignores: each argument gets its
/// placeholder's options.)
#[kani::proof]
#[kani::unwind(22)]
fn formatter_write_fmt() {
    let a: [u8; 4] = kani::any();
    let n: usize = kani::any();
    kani::assume(n <= 4);
    if let Ok(s) = core::str::from_utf8(&a[..n]) {
        super::fmt::formats(&WriteFmt(s, kani::any()));
    }
}

/// S337 `core::hint::assert_unchecked` (since the port of upstream #101):
/// under its `requires` (`cond`) the call returns; passes (Kani 0.68.0).
#[kani::proof]
fn hint_assert_unchecked() {
    let cond: bool = kani::any();
    kani::assume(cond);
    // SAFETY: `cond` holds.
    unsafe { core::hint::assert_unchecked(cond) };
}
