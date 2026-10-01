//! The ghost contents model for zeroed memory: a [`Zeroed`] token says a
//! range of bytes reads as zero. `calloc`, `mallocx(MALLOCX_ZERO)` and
//! their internals return one for the memory they hand out
//! (`verus/TODO.md`, "Zeroed memory").

use vstd::prelude::*;

verus! {

/// Ghost token: the `len` bytes at address `addr` read as zero.
///
/// A token is made only where the bytes are known to be zero: by the
/// `write_bytes` of [`write_zero`], from the kernel's contract for fresh
/// and discarded anonymous memory (`vspec::sys::axiom_fresh_anon_zero`,
/// `vspec::sys::axiom_discarded_zero`, called by `huge::zero_run`), by the
/// trusted `ClassState::claim_with` for the slots of a zeroed claim, and
/// by splitting and joining tokens (the bytes of a zero range are zero).
/// A token is not a permission to access the bytes (that is
/// `vspec::ptr::valid_writes`), and nothing ties it to the writes the
/// program or the allocator make: it stays true because the allocator
/// hands the bytes to no one while it holds the token, and the only
/// writes verified code makes to such bytes are zero fills and
/// `rallocx`'s copy into a new block's prefix, which `rallocx_zeroed`
/// splits off first. A token means nothing once its memory is handed to
/// the program: the entry points' postconditions state it at the moment
/// of return.
pub tracked struct Zeroed {
    ghost addr: int,
    ghost len: int,
}

impl Zeroed {
    /// First byte.
    pub closed spec fn addr(&self) -> int {
        self.addr
    }

    /// Bytes covered.
    pub closed spec fn len(&self) -> int {
        self.len
    }

    /// The token covers at least `n` bytes from `p`.
    pub open spec fn covers(&self, p: *mut u8, n: int) -> bool {
        &&& self.addr() == p@.addr
        &&& n <= self.len()
    }

    /// The empty range at `addr` reads as zero: there is nothing to read.
    pub proof fn empty(addr: int) -> (tracked z: Zeroed)
        ensures
            z.addr() == addr,
            z.len() == 0,
    {
        Zeroed { addr, len: 0 }
    }

    /// A zero range is its first `k` bytes and the rest, both zero.
    pub proof fn split(tracked self, k: int) -> (tracked r: (Zeroed, Zeroed))
        requires
            0 <= k <= self.len(),
        ensures
            r.0.addr() == self.addr(),
            r.0.len() == k,
            r.1.addr() == self.addr() + k,
            r.1.len() == self.len() - k,
    {
        (Zeroed { addr: self.addr, len: k }, Zeroed { addr: self.addr + k, len: self.len - k })
    }

    /// Two adjacent zero ranges are one.
    pub proof fn join(tracked self, tracked next: Zeroed) -> (tracked z: Zeroed)
        requires
            self.len() >= 0,
            next.len() >= 0,
            self.addr() + self.len() == next.addr(),
        ensures
            z.addr() == self.addr(),
            z.len() == self.len() + next.len(),
    {
        Zeroed { addr: self.addr, len: self.len + next.len }
    }
}

/// `write_bytes(p, 0, n)`, returning the token for what it wrote. Trusted
/// for the one fact `write_bytes`' specification does not state
/// (`vspec::addr`): the `n` bytes at `p` read as zero afterwards.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn write_zero(p: *mut u8, n: usize) -> (z: Tracked<Zeroed>)
    requires
        n == 0 || crate::vspec::ptr::valid_writes(p, n as int),
    ensures
        z@.addr() == p@.addr,
        z@.len() == n,
{
    // SAFETY: the caller's, as for `write_bytes`.
    unsafe { core::ptr::write_bytes(p, 0, n) };
    Tracked::assume_new()
}

} // verus!
