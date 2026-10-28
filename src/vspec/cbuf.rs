//! The C caller's buffers: what the C contract of an entry point hands
//! challoc for one call through a pointer argument (a string, a byte
//! buffer and its length, an array of `size_t`s and its length), as one
//! linear ghost token per call, [`CCall`].
//!
//! The trust sits at the C boundary: the export wrapper of each C symbol
//! is to mint the token once from its own parameters and hand it to the
//! Rust entry point, which threads it, borrowed, to the code that reads
//! the caller's buffers and begins the writes into them (`api/mallctl.rs`,
//! `api/print.rs`).
//! Only the types are here; the contracts of the arguments, the read
//! permissions the token lends and the mints land with the proofs that
//! use them. Until then every caller outside `verus!` passes a
//! placeholder token.

use vstd::prelude::*;
use vstd::raw_ptr::Provenance;

verus! {

/// The C entry point a call came through, with the buffer and length
/// arguments C passed it: ghost values of the entry point's own
/// parameters, as its export wrapper names them ([`CCall`]).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost enum CEntry {
    /// `mallctl(name, oldp, oldlenp, newp, newlen)`.
    Mallctl { name: *const u8, oldp: *mut u8, oldlenp: *const usize, newp: *mut u8, newlen: usize },
    /// `mallctlnametomib(name, mibp, miblenp)`.
    NameToMib { name: *const u8, mibp: *mut usize, miblenp: *const usize },
    /// `mallctlbymib(mib, miblen, oldp, oldlenp, newp, newlen)`.
    ByMib { mib: *const usize, miblen: usize, oldp: *mut u8, oldlenp: *const usize, newp: *mut u8, newlen: usize },
    /// `malloc_stats_print(write_cb, cbopaque, opts)`.
    StatsPrint { opts: *const u8 },
}

/// Ghost view of a C caller's string at one call: its bytes up to and
/// including the NUL (`bytes`), at `addr` with provenance `prov`. A
/// [`CCall`] holds the one of its call.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct CStr {
    pub addr: usize,
    pub prov: Provenance,
    pub bytes: Seq<u8>,
}

/// The token of one call from C through a C entry point: the entry point
/// and its arguments (`entry`), a view of its string argument (`string`),
/// the values its `size_t *` arguments point at (`lens`), and whether a
/// write through the call's buffers, or a call back into C, has begun
/// (`written`). Linear: not `Copy`, and no constructor here.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct CCall {
    ghost entry: CEntry,
    ghost string: CStr,
    ghost lens: spec_fn(*const usize) -> usize,
    ghost written: bool,
}

} // verus!
