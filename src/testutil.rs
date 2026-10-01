//! Test fixtures shared across modules: [`TestShared`], the one builder of
//! a [`Shared`] over a synthetic layout, and a deterministic [`xorshift`].

use core::mem::MaybeUninit;
use std::boxed::Box;

use crate::config::Config;
use crate::heap::Shared;
use crate::layout::{LG_MAX_SLAB, MAX_SLAB, Mapping, NCLASSES, Regions};
use crate::sys::pac;

const LG_REGION: u32 = LG_MAX_SLAB;
const LG_STRIDE: u32 = LG_REGION + 2;
const META_LEN: usize = 16 << 20;
/// 64 MiB huge region, reserved for real.
const HUGE_LEN: usize = 64 << 20;

/// The page size the synthetic layout is built with: the kernel's (the
/// reservations are real).
pub(crate) fn page_size() -> usize {
    crate::sys::features::page_size()
}

pub(crate) fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

fn regions_at(superblock: usize, meta: (usize, usize), huge_base: usize) -> Regions {
    let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
    let huge = (huge_base, HUGE_LEN);
    Regions::from_parts(
        superblock,
        LG_REGION,
        LG_STRIDE,
        &[0usize; NCLASSES],
        huge,
        meta,
        page_size().trailing_zeros(),
        [
            Mapping {
                addr: superblock,
                len: sb_len,
            },
            Mapping {
                addr: huge.0,
                len: huge.1,
            },
            Mapping {
                addr: meta.0,
                len: meta.1,
            },
        ],
    )
}

/// A `Shared` over a synthetic 4 MiB-per-class layout. The regions are
/// real `PROT_NONE` reservations so slot memory can be touched. The
/// result is leaked on purpose: the test thread's own heap is torn down
/// by the key destructor after the test body has returned.
pub(crate) struct TestShared {
    config: Config,
    guards: bool,
    pac: bool,
}

impl TestShared {
    /// The heap tests' configuration: one automatic arena, so every heap
    /// of a test shares arena 0 (the default would spread concurrently
    /// live heaps over `4 * ncpus`), and `pac_pointers` on, so the signed
    /// path runs on a host with pointer authentication and the plain path
    /// elsewhere.
    pub(crate) fn new() -> TestShared {
        TestShared::with(Config {
            narenas: 1,
            pac_pointers: true,
            ..Config::DEFAULT
        })
    }

    /// `config` as given. No guard slabs, the PAC probe's real answer.
    pub(crate) fn with(config: Config) -> TestShared {
        TestShared {
            config,
            guards: false,
            pac: pac::available(),
        }
    }

    /// Guard slabs at the configured interval (`guard_install` as on a
    /// kernel with guard regions).
    pub(crate) fn guards(mut self) -> TestShared {
        self.guards = true;
        self
    }

    /// Stand in for the PAC probe: `false` on a host that has the
    /// extension builds the fallback (plain pointers) that CPUs without it
    /// take.
    pub(crate) fn pac(mut self, available: bool) -> TestShared {
        self.pac = available;
        self
    }

    pub(crate) fn build(self) -> &'static Shared {
        let (sb, mb, hb) = {
            use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
            let reserve = |len: usize| -> usize {
                unsafe {
                    crate::sys::mmap(
                        0,
                        len,
                        PROT_NONE,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                        -1,
                        0,
                    )
                }
                .expect("reserve")
            };
            (
                reserve((NCLASSES << LG_STRIDE) + MAX_SLAB),
                reserve(META_LEN),
                reserve(HUGE_LEN),
            )
        };
        let regions = regions_at(
            align_up(sb, MAX_SLAB),
            (align_up(mb, page_size()), META_LEN),
            hb,
        );
        // One raw pointer for `init` and the returned `&Shared`, as in
        // `api::init`: `init` keeps pointers derived from it.
        let p = Box::into_raw(Box::new(MaybeUninit::<Shared>::uninit())).cast::<Shared>();
        assert!(unsafe {
            Shared::init(
                p,
                regions,
                vstd::prelude::Tracked::assume_new(),
                self.config,
                self.guards,
                self.pac,
            )
        });
        unsafe { &*p }
    }
}

/// `TestShared::new().build()`.
pub(crate) fn shared() -> &'static Shared {
    TestShared::new().build()
}

/// One step of Marsaglia's xorshift64; `state` must not be zero.
pub(crate) fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}
