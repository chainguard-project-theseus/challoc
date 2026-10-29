//! The usable size an entry point reports through a caller's out-pointer
//! ([`Stored`]). Only the type is here; its specification lands with the
//! proofs of the `*_usable` entry points.

use vstd::prelude::*;

verus! {

/// Ghost token: an entry point stored `val` through the caller's
/// out-pointer `ptr` (and nothing, when `ptr` is null), the last store it
/// makes through it.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct Stored {
    ghost ptr: *mut usize,
    ghost val: usize,
}

} // verus!
