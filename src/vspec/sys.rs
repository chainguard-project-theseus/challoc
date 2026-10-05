//! Specifications for `sys`: the `core` byte, integer and slice methods its
//! parsers and syscall wrappers call, and the derived `PartialEq` of its
//! enums.

use vstd::prelude::*;

verus! {

/// `u8::is_ascii_digit`: `0`..`9`.
pub assume_specification[ u8::is_ascii_digit ](b: &u8) -> (r: bool)
    ensures r <==> 48 <= *b <= 57,
;

/// `u8::is_ascii_whitespace`: space, tab, line feed, form feed and
/// carriage return (not vertical tab).
pub assume_specification[ u8::is_ascii_whitespace ](b: &u8) -> (r: bool)
    ensures r <==> (*b == 32 || *b == 9 || *b == 10 || *b == 12 || *b == 13),
;

/// `u8::is_ascii_graphic`: `!`..`~`.
pub assume_specification[ u8::is_ascii_graphic ](b: &u8) -> (r: bool)
    ensures r <==> 33 <= *b <= 126,
;

/// `isize::wrapping_neg`: `-x`, except that `isize::MIN` is its own
/// negation.
pub assume_specification[ isize::wrapping_neg ](x: isize) -> (r: isize)
    ensures r == if x == isize::MIN { isize::MIN as int } else { -x },
;

/// The address of a slice's first element, for a syscall argument; the
/// verified code never dereferences it. Taking it leaves the slice as
/// long as it was; its contents are left unconstrained, so that a buffer
/// the kernel then writes through the address (`read`, `getrandom`, the
/// mask of `ncpus`), which `raw::syscall3` does not model, is
/// not taken to keep its old bytes.
pub assume_specification<T>[ <[T]>::as_mut_ptr ](s: &mut [T]) -> *mut T
    ensures
        final(s)@.len() == old(s)@.len(),
;

pub assume_specification<T>[ <[T]>::as_ptr ](s: &[T]) -> *const T;

/// `Result::map_or`: `f` of the `Ok` value, else `default`.
pub assume_specification<T, E, U, F: FnOnce(T) -> U>[ Result::<T, E>::map_or ](
    result: Result<T, E>,
    default: U,
    f: F,
) -> (r: U)
    requires
        result matches Ok(v) ==> f.requires((v,)),
    ensures
        result is Err ==> r == default,
        result matches Ok(v) ==> f.ensures((v,), r),
;

/// `Option::is_some_and`: `f` of the value, `false` for `None`.
pub assume_specification<T, F: FnOnce(T) -> bool>[ Option::<T>::is_some_and ](o: Option<T>, f: F) -> (r: bool)
    requires
        o matches Some(v) ==> f.requires((v,)),
    ensures
        o is None ==> !r,
        o matches Some(v) ==> f.ensures((v,), r),
;

/// `Result::unwrap_or`: the `Ok` value, else `default`.
pub assume_specification<T, E>[ Result::<T, E>::unwrap_or ](result: Result<T, E>, default: T) -> (r: T)
    ensures
        r == match result {
            Ok(v) => v,
            Err(_) => default,
        },
;

/// The word eight bytes spell in the target's byte order
/// (`usize::from_ne_bytes`), left abstract.
pub uninterp spec fn ne_word(b: Seq<u8>) -> usize;

/// `u64::is_power_of_two`.
pub assume_specification[ u64::is_power_of_two ](x: u64) -> (r: bool)
    ensures
        r <==> exists|k: nat| k < 64 && x as nat == #[trigger] vstd::arithmetic::power2::pow2(k),
;

#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExUtf8Error(core::str::Utf8Error);

/// `core::str::from_utf8`: the bytes as a `str` when they are UTF-8.
pub assume_specification[ core::str::from_utf8 ](v: &[u8]) -> Result<&str, core::str::Utf8Error>;

/// `core::str::from_utf8_unchecked`: its safety contract is that the
/// bytes are UTF-8.
pub assume_specification[ core::str::from_utf8_unchecked ](v: &[u8]) -> &str
    requires
        vstd::utf8::valid_utf8(v@),
;

/// The unit `fmt::Error`, built through its derived `Default`.
pub assume_specification[ <core::fmt::Error as core::default::Default>::default ]() -> core::fmt::Error;

/// A reference as a raw pointer, for a syscall argument.
pub assume_specification<T: core::marker::PointeeSized>[ core::ptr::from_ref ](r: &T) -> *const T;

pub assume_specification<T: core::marker::PointeeSized>[ core::ptr::from_mut ](r: &mut T) -> *mut T;

/// `Option::copied`: the referent, copied.
pub assume_specification<T: Copy>[ Option::<&T>::copied ](o: Option<&T>) -> (r: Option<T>)
    ensures
        r == match o {
            Some(v) => Some(*v),
            None => None::<T>,
        },
;

// The derived `PartialEq` of these enums compares variants (ghost only:
// ordinary builds have no `std_specs`).

#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::sys::cgroup::State {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

#[cfg(verus_only)]
impl vstd::std_specs::cmp::PartialEqSpecImpl for crate::sys::Errno {
    open spec fn obeys_eq_spec() -> bool {
        true
    }

    open spec fn eq_spec(&self, other: &Self) -> bool {
        *self == *other
    }
}

} // verus!

verus! {

// The kernel's contract for the contents of anonymous memory, which the
// zero claims of `calloc` and `mallocx(MALLOCX_ZERO)` rest on
// (`vspec::zero`). Each axiom states a kernel fact about a range and
// leaves the premise that the range is in the state it names to its call
// sites. The slab forms of these facts, `class::axiom_fresh_slots_zero`
// and `class::axiom_discarded_slots_zero`, require the part of that
// premise the caller's ghost state proves; the rest is what the
// allocator's state records (a slab's or an extent's state under
// its lock, the huge region's high-water mark, the process-wide
// `purge::purged_is_zero` flag) right before the call, and what that
// record means is the allocator protocol each axiom's doc names. The
// metadata region does not use them: `Meta` takes its zero tokens from
// `vspec::unbacked::Unbacked::commit`, for bytes its ghost state proves
// never handed out.

/// `mmap(2)`: a private anonymous mapping reads as zero until it is first
/// written. Changing its protection (`mprotect`), populating it
/// (`MADV_POPULATE_WRITE`), advising it (`MADV_HUGEPAGE`,
/// `MADV_COLLAPSE`) and guard markers (`MADV_GUARD_INSTALL`/`REMOVE`)
/// leave the contents alone.
///
/// Premise, the caller's and trusted: the `len` bytes at `addr` lie in the
/// huge region (`Regions::reserve`) and nothing has written them since it
/// was mapped, or written anything but zero. Call site: `huge::zero_run`,
/// from `Huge::take` under the huge lock, for pages at or past the huge
/// region's high-water mark, which only `take` raises and which no run
/// has covered.
pub(crate) axiom fn axiom_fresh_anon_zero(addr: int, len: int) -> (tracked z: crate::vspec::zero::Zeroed)
    requires
        0 <= len,
    ensures
        z.addr() == addr,
        z.len() == len,
;

/// `madvise(2)`: after `MADV_DONTNEED` on private anonymous memory,
/// reads return zero-fill-on-demand pages (`MADV_FREE` gives no such
/// guarantee: the old contents stay until the kernel reclaims the page).
///
/// Premise, the caller's and trusted: the `len` bytes at `addr` were
/// discarded with `MADV_DONTNEED`, or never written but with zeros, and
/// nothing has written them since. Call site: `huge::zero_run`, from
/// `Huge::take` under the huge lock, for a `Purged` extent while
/// `purge::purged_is_zero` reads true: no batch of the process has used
/// `MADV_FREE` (`purge::Batch::flush` clears the flag before it issues
/// one), `Purged` is set only in the lock hold that flushes the extent's
/// discard, and a `Purged` extent has no allocated run.
pub(crate) axiom fn axiom_discarded_zero(addr: int, len: int) -> (tracked z: crate::vspec::zero::Zeroed)
    requires
        0 <= len,
    ensures
        z.addr() == addr,
        z.len() == len,
;

} // verus!
