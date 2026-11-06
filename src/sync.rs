//! The atomics the `vspec` wrappers use: `core`'s, or loom's under
//! `cfg(loom)`, which only the loom crate (`tests/loom`) sets when it
//! compiles those wrapper files as its own modules (`verus/WEAKMEM.md`,
//! "loom"). A re-export is the same item, so the release build and the
//! Verus specifications see `core`'s unchanged.

pub(crate) mod atomic {
    #[cfg(not(loom))]
    pub(crate) use core::sync::atomic::*;
    #[cfg(loom)]
    pub(crate) use loom::sync::atomic::*;
}
