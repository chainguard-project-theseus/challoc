//! Specifications of the `core` functions the verified code calls that
//! vstd does not specify (`verus/README.md`, "What counts as verified"),
//! and trusted wrappers for operations Verus does not model. Each is a
//! seam (`verus/SEAMS.md`): Verus takes it on trust, so each states only
//! what the `core` documentation promises. Ordinary builds compile the
//! wrappers; the specifications are ghost and vanish there.
//! [`div_ceil`] and [`div_ceil_u64`] are not seams: they repeat `core`'s
//! `div_ceil` bodies, and Verus checks them.

use vstd::prelude::*;

pub(crate) mod addr;
pub(crate) mod atom;
pub(crate) mod byte;
pub(crate) mod cbuf;
pub(crate) mod check;
pub(crate) mod count;
pub(crate) mod ctl;
pub(crate) mod entry;
pub(crate) mod errno;
pub(crate) mod flags;
// Specifications only: Verus alone compiles these.
#[cfg(verus_keep_ghost)]
pub(crate) mod arena;
#[cfg(verus_keep_ghost)]
pub(crate) mod class;
#[cfg(verus_keep_ghost)]
mod config;
#[cfg(verus_keep_ghost)]
pub(crate) mod core_int;
#[cfg(verus_keep_ghost)]
pub(crate) mod fmt;
#[cfg(verus_keep_ghost)]
pub(crate) mod huge;
#[cfg(verus_keep_ghost)]
pub(crate) mod layout;
#[cfg(verus_keep_ghost)]
pub(crate) mod meta;
#[cfg(verus_keep_ghost)]
mod misc;
#[cfg(verus_keep_ghost)]
pub(crate) mod panic;
#[cfg(verus_keep_ghost)]
pub(crate) mod sys;

pub(crate) mod once;
pub(crate) mod print;
pub(crate) mod ptr;
pub(crate) mod slice;
pub(crate) mod unbacked;
pub(crate) mod usable;
pub(crate) mod user;
pub(crate) mod word;
pub(crate) mod zero;

verus! {

// challoc runs on x86_64 and aarch64 only (its TLS, rseq and syscall asm),
// where `usize` is 8 bytes; Verus checks the size when it compiles the
// crate. `Curve::allowance` casts a `u64` to `usize`.
global size_of usize == 8;

/// `usize::div_ceil`, with `core`'s body: the quotient rounded up.
/// Verified, so the verified code calls this in place of `usize::div_ceil`,
/// which vstd does not specify. `#[track_caller]`, as on `core`'s, reports a
/// division by zero at the caller.
#[inline(always)]
#[track_caller]
pub(crate) const fn div_ceil(x: usize, d: usize) -> (r: usize)
    requires
        d > 0,
    ensures
        r as int == (x as int + d as int - 1) / d as int,
{
    proof {
        lemma_div_ceil(x as int, d as int);
    }
    let q = x / d;
    let r = x % d;
    if r > 0 { q + 1 } else { q }
}

/// `u64::div_ceil`, with `core`'s body, as [`div_ceil`].
#[inline(always)]
#[track_caller]
pub(crate) const fn div_ceil_u64(x: u64, d: u64) -> (r: u64)
    requires
        d > 0,
    ensures
        r == (x + d - 1) / d as int,
{
    proof {
        lemma_div_ceil(x as int, d as int);
    }
    let q = x / d;
    let r = x % d;
    if r > 0 { q + 1 } else { q }
}

/// `x / d`, plus one when `d` leaves a remainder, is `(x + d - 1) / d`;
/// the plus one stays at or below `x`, so it cannot overflow.
proof fn lemma_div_ceil(x: int, d: int)
    requires
        0 <= x,
        0 < d,
    ensures
        x % d > 0 ==> x / d < x && x / d + 1 == (x + d - 1) / d,
        x % d == 0 ==> x / d == (x + d - 1) / d,
{
    let (q, r) = (x / d, x % d);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x, d);
    vstd::arithmetic::div_mod::lemma_mod_pos_bound(x, d);
    vstd::arithmetic::div_mod::lemma_div_pos_is_pos(x, d);
    if r > 0 {
        assert(q < x && x + d - 1 == (q + 1) * d + (r - 1)) by (nonlinear_arith)
            requires
                x == d * q + r,
                0 < r < d,
                0 <= q,
        ;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(x + d - 1, d, q + 1, r - 1);
    } else {
        assert(x + d - 1 == q * d + (d - 1)) by (nonlinear_arith)
            requires
                x == d * q + r,
                r == 0,
        ;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(x + d - 1, d, q, d - 1);
    }
}

} // verus!
