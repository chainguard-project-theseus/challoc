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

/// vstd's permissioned atomics, which the ported `vspec` wrappers hold:
/// vstd's own types, or under `cfg(loom)` stand-ins with the same methods
/// over loom's atomics, so the loom models run the wrappers' bodies. vstd's
/// operations are `SeqCst`, and so are the stand-ins'. Loom builds are
/// never verified, so the stand-ins carry no specifications; the ghost
/// permission they take is erased.
pub(crate) mod patomic {
    #[cfg(not(loom))]
    pub(crate) use vstd::atomic::{PAtomicU8, PAtomicU32, PermissionU8, PermissionU32};

    #[cfg(loom)]
    pub(crate) use loom_patomic::*;

    #[cfg(loom)]
    mod loom_patomic {
        use loom::sync::atomic::{AtomicU8, AtomicU32, Ordering};
        use vstd::prelude::Tracked;

        /// The erased permission of a [`PAtomicU32`].
        pub(crate) struct PermissionU32;

        /// `vstd::atomic::PAtomicU32` over loom's `AtomicU32`.
        pub(crate) struct PAtomicU32 {
            ato: AtomicU32,
        }

        impl PAtomicU32 {
            pub(crate) fn new(v: u32) -> (Self, Tracked<PermissionU32>) {
                (
                    PAtomicU32 {
                        ato: AtomicU32::new(v),
                    },
                    Tracked::assume_new(),
                )
            }

            pub(crate) fn load(&self, _perm: Tracked<&PermissionU32>) -> u32 {
                self.ato.load(Ordering::SeqCst)
            }

            pub(crate) fn store(&self, _perm: Tracked<&mut PermissionU32>, v: u32) {
                self.ato.store(v, Ordering::SeqCst)
            }

            pub(crate) fn swap(&self, _perm: Tracked<&mut PermissionU32>, v: u32) -> u32 {
                self.ato.swap(v, Ordering::SeqCst)
            }

            pub(crate) fn compare_exchange(
                &self,
                _perm: Tracked<&mut PermissionU32>,
                current: u32,
                new: u32,
            ) -> Result<u32, u32> {
                self.ato
                    .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
            }

            pub(crate) fn fetch_add_wrapping(
                &self,
                _perm: Tracked<&mut PermissionU32>,
                v: u32,
            ) -> u32 {
                self.ato.fetch_add(v, Ordering::SeqCst)
            }

            pub(crate) fn fetch_sub_wrapping(
                &self,
                _perm: Tracked<&mut PermissionU32>,
                v: u32,
            ) -> u32 {
                self.ato.fetch_sub(v, Ordering::SeqCst)
            }
        }

        /// The erased permission of a [`PAtomicU8`].
        pub(crate) struct PermissionU8;

        /// `vstd::atomic::PAtomicU8` over loom's `AtomicU8`.
        pub(crate) struct PAtomicU8 {
            ato: AtomicU8,
        }

        impl PAtomicU8 {
            pub(crate) fn new(v: u8) -> (Self, Tracked<PermissionU8>) {
                (
                    PAtomicU8 {
                        ato: AtomicU8::new(v),
                    },
                    Tracked::assume_new(),
                )
            }

            pub(crate) fn load(&self, _perm: Tracked<&PermissionU8>) -> u8 {
                self.ato.load(Ordering::SeqCst)
            }

            pub(crate) fn fetch_or(&self, _perm: Tracked<&mut PermissionU8>, v: u8) -> u8 {
                self.ato.fetch_or(v, Ordering::SeqCst)
            }
        }
    }
}
