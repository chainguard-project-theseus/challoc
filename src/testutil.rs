//! Test fixtures shared across modules: [`TestShared`], the one builder of
//! a [`Shared`] over a synthetic layout, a deterministic [`xorshift`], and
//! [`global_state`], the guard of the tests that write or assert the
//! process-wide allocator's settings.

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
    gc_interval_ns: Option<u64>,
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
            gc_interval_ns: None,
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

    /// Replace the GC interval `Shared::init` stores
    /// (`Shared::gc_interval_ns`).
    pub(crate) fn gc_interval_ns(mut self, ns: u64) -> TestShared {
        self.gc_interval_ns = Some(ns);
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
        // One raw pointer for the init, the field edit and the final
        // reference: `init` keeps pointers derived from it (the arena
        // registries), which a reborrow through a `&mut` in between would
        // invalidate (Miri, `verus/MIRI.md`).
        let b: *mut Shared = Box::into_raw(Box::new(MaybeUninit::<Shared>::uninit())).cast();
        assert!(unsafe {
            Shared::init(
                b,
                regions,
                vstd::prelude::Tracked::assume_new(),
                self.config,
                self.guards,
                self.pac,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        });
        if let Some(ns) = self.gc_interval_ns {
            // SAFETY: initialised above; nothing else references it yet.
            unsafe { (*b).gc_interval_ns = ns };
        }
        // SAFETY: initialised above and leaked.
        unsafe { &*b }
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

/// The tests of the process-wide allocator (`api::shared()`) that write a
/// process-wide setting (decay, the background thread request, the stats
/// epoch, the fork handlers' lock state) or assert one take turns on this
/// guard. libtest runs the tests of one process in parallel, and without it
/// one test's write lands between another's write and its assertion (#69).
/// Tests that only allocate and free need not take it: none of the guarded
/// assertions depends on another thread not allocating, and the one that
/// would (every lock free after the fork handlers) runs in a fork child.
static GLOBAL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`GLOBAL_STATE`]; a panic in one holder must not fail the rest.
pub(crate) fn global_state() -> std::sync::MutexGuard<'static, ()> {
    GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` in a child of a real `fork` and assert that it returned. The
/// child has one thread, so a check of process-wide state there cannot be
/// raced by the other tests of this process, and what `f` writes (a
/// sticky flag, a lock state) never reaches them (#69). A panic in `f`
/// exits the child with status 1 (its message goes to stderr) instead of
/// unwinding into a copy of the test harness; a child still running after
/// `timeout_ms` is killed and fails the test. The test binary's own
/// allocator is glibc's, which `fork` leaves usable in the child, so `f`
/// may allocate and use `assert!`.
pub(crate) fn in_fork_child(what: &str, timeout_ms: u64, f: impl FnOnce()) {
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "{what}: fork failed");
    if pid == 0 {
        let r = std::panic::catch_unwind(core::panic::AssertUnwindSafe(f));
        unsafe { libc::_exit(if r.is_ok() { 0 } else { 1 }) };
    }
    let mut status = 0;
    let mut waited = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert!(r >= 0, "{what}: waitpid failed");
        if r == pid {
            break;
        }
        if waited >= timeout_ms {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
            panic!("{what}: fork child still running after {timeout_ms} ms");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        waited += 10;
    }
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "{what}: fork child failed (status {status:#x}; its panic is on stderr)"
    );
}
