//! The `Option`, `Result`, `Layout`, `str`, `CStr`, `Formatter` and
//! `hint` functions of `core` that `vspec/misc.rs`, `vspec/sys.rs`,
//! `vspec/layout.rs`, `vspec/config.rs` and `vspec/fmt.rs` specify
//! (`verus/ledger/src/vspec/`), checked against their contracts.
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

/// `Option::<T>::map_or`: `default` for `None` (and `f` not called),
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

/// `Result::<T, E>::map_or`: `default` for `Err` (and `f` not called),
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

/// `Result::<T, E>::unwrap_or`: the `Ok` value, else `default`.
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

/// `Option::<T>::as_deref_mut`: no `ensures`; it returns without a panic
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

/// `Layout::from_size_align`, `Layout::size` and `Layout::align`: `Ok`
/// exactly when `align > 0`, `align & (align - 1) == 0` and
/// `size <= isize::MAX - (align - 1)` (in `int`), and then `size()` and
/// `align()` give the arguments back and the layout is `layout_ok`, for
/// every `size` and `align`.
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

/// `Layout::new::<T>`, with `Layout::size` and `Layout::align` on its
/// result, at the types the proofs build layouts of (`SetTable`, `Arena`,
/// `Heap`, `HugeDesc`) and at a zero-sized and an over-aligned type.
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

/// `core::ffi::CStr::as_ptr`: no `ensures`; it returns, the address of the
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

/// `core::str::from_utf8`: no `ensures`; it returns without a panic
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

/// Writes its string through `Formatter::write_str`.
struct WriteStr<'a>(&'a str);

impl core::fmt::Display for WriteStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

/// `core::fmt::Formatter::<'a>::write_str`: no `requires`, no `ensures`;
/// it returns, for every string up to 4 bytes, under the plain and a
/// padded formatter and whatever the writer returns.
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

/// `core::fmt::Formatter::<'a>::write_fmt`: no `requires`, no `ensures`;
/// it returns, for the arguments above with every string up to 4 bytes
/// and every number, whatever the writer returns. (`formats` also runs it
/// under a padded formatter, which `write_fmt` ignores: each argument
/// gets its placeholder's options.) The slowest harness, about 21
/// minutes on 8 vCPUs.
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

/// `core::hint::assert_unchecked`: under its `requires` (`cond`) the
/// call returns.
#[kani::proof]
fn hint_assert_unchecked() {
    let cond: bool = kani::any();
    kani::assume(cond);
    // SAFETY: `cond` holds.
    unsafe { core::hint::assert_unchecked(cond) };
}
