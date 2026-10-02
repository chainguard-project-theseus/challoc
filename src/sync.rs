//! The atomics and the spin hint the crate uses: `core`'s, or loom's under
//! `cfg(loom)` for the model-checking tests (`src/loom_tests/`,
//! `verus/WEAKMEM.md`). A re-export is the same item, so the release build
//! and the Verus specifications see `core`'s unchanged.

pub(crate) mod atomic {
    #[cfg(not(loom))]
    pub(crate) use core::sync::atomic::*;
    #[cfg(loom)]
    pub(crate) use loom::sync::atomic::*;
}

pub(crate) mod hint {
    #[cfg(not(loom))]
    pub(crate) use core::hint::spin_loop;
    // loom's yields to the other modelled threads, so a spin makes progress.
    #[cfg(loom)]
    pub(crate) use loom::hint::spin_loop;
}
