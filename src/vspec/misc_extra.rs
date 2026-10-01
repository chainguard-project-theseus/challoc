//! Specifications for the futex and lock-word callers (`lock.rs`,
//! `purge/background.rs`).

use vstd::prelude::*;

verus! {

/// `AtomicU32::as_ptr`: the address of the atomic's value, for a futex
/// syscall argument. Nothing is claimed about it; the verified callers
/// only pass it to the kernel and never dereference it.
pub assume_specification[ core::sync::atomic::Atomic::<u32>::as_ptr ](a: &core::sync::atomic::Atomic<u32>) -> *mut u32;

} // verus!
