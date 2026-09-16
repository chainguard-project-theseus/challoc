//! Process-wide allocator state and its lazy initialisation (design.md
//! section 10), plus the fork handlers (section 9). The entry points in the
//! submodules reach the allocator through [`shared`].
//!
//! Init is triggered by the first call to any entry point and serialised
//! with an atomic state machine `Uninit -> Initialising -> Ready`. A second
//! thread arriving during `Initialising` spins; the initialising thread
//! re-entering the allocator (something in init allocated) aborts with a
//! message, because serving it would need the state that is being built.
//! ld.so may call `malloc` before constructors run, so nothing here relies
//! on `.init_array`.
//!
//! Init never allocates. It probes the kernel, reserves the regions, builds
//! the [`Shared`] in place inside a static, creates the thread-exit key
//! (inside `Shared::init`) and registers the fork handlers. glibc's
//! `pthread_key_create` and `pthread_atfork` do not allocate for the first
//! keys and handlers, which is all we need.
//!
//! The fork prepare handler takes every class lock, then the huge lock, then
//! the metadata lock. That order matches the allocator's own nesting (class
//! and huge code take the metadata lock while holding their own; nothing
//! takes a class or huge lock while holding metadata), so a thread that is
//! mid-operation when another thread forks can always finish and release.
//! Parent and child release in reverse. Other threads' heaps are left alone
//! in the child, as in every other allocator.

pub mod libc;

#[cfg(test)]
mod ctests;

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::heap::Shared;
use crate::layout::{NCLASSES, Regions};
use crate::sys;
use crate::sys::features::Features;

/// Default `guard_slab_interval` (design section 12): one guard slab per 8.
pub const DEFAULT_GUARD_SLAB_INTERVAL: u32 = 8;

const UNINIT: u32 = 0;
const INITIALISING: u32 = 1;
const READY: u32 = 2;

static STATE: AtomicU32 = AtomicU32::new(UNINIT);
/// Thread id of the initialising thread while the state is `INITIALISING`,
/// so a re-entrant call from it can be told apart from a concurrent one.
static INIT_TID: AtomicU32 = AtomicU32::new(0);

/// Storage for a value built in place at init and immutable afterwards.
struct Global<T>(UnsafeCell<MaybeUninit<T>>);

// SAFETY: written exactly once, by the initialising thread before it
// publishes `READY` with a release store; every reader loads the state with
// acquire first and then only takes shared references.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new() -> Self {
        Global(UnsafeCell::new(MaybeUninit::uninit()))
    }

    /// # Safety
    /// The value must have been initialised and `READY` observed with an
    /// acquire load on this thread.
    #[inline(always)]
    unsafe fn get(&self) -> &T {
        // SAFETY: guaranteed by the caller.
        unsafe { (*self.0.get()).assume_init_ref() }
    }
}

static SHARED: Global<Shared> = Global::new();
static FEATURES: Global<Features> = Global::new();

/// The process's allocator state, initialising it on first use.
#[inline(always)]
pub fn shared() -> &'static Shared {
    if STATE.load(Ordering::Acquire) == READY {
        // SAFETY: `READY` was published after init with a release store.
        return unsafe { SHARED.get() };
    }
    init_slow()
}

/// The allocator state if it is ready, `None` before init has completed.
/// `free` uses this: a pointer arriving before any allocation cannot be
/// ours and must not trigger init.
#[inline(always)]
pub fn shared_if_ready() -> Option<&'static Shared> {
    if STATE.load(Ordering::Acquire) == READY {
        // SAFETY: as in `shared`.
        Some(unsafe { SHARED.get() })
    } else {
        None
    }
}

/// The allocator state without the readiness check.
///
/// # Safety
/// The calling thread must itself have observed `READY` earlier (through
/// [`shared`] or [`shared_if_ready`]). The fast paths use the thread's heap
/// pointer as that witness: a heap is only created under `shared()`.
#[inline(always)]
pub unsafe fn shared_unchecked() -> &'static Shared {
    // SAFETY: guaranteed by the caller.
    unsafe { SHARED.get() }
}

/// The probed kernel features. Initialises the allocator if needed.
pub fn features() -> &'static Features {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURES.get() }
}

/// Whether init has completed (for tests and diagnostics).
pub fn is_ready() -> bool {
    STATE.load(Ordering::Acquire) == READY
}

#[cold]
#[inline(never)]
fn init_slow() -> &'static Shared {
    match STATE.compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire) {
        Ok(_) => {
            INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);
            init();
            STATE.store(READY, Ordering::Release);
        }
        Err(READY) => {}
        Err(_) => {
            // The `INIT_TID` store happens before any allocation the init
            // thread could make, so a matching tid here is re-entry.
            if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {
                sys::abort("challoc: allocation during initialisation (re-entrant init)");
            }
            while STATE.load(Ordering::Acquire) != READY {
                core::hint::spin_loop();
            }
        }
    }
    // SAFETY: `READY` observed (or published by this thread).
    unsafe { SHARED.get() }
}

/// Design section 10 steps 1 to 6 (configuration, sealing and cgroups join
/// in Phases 1 and 2).
fn init() {
    let features = Features::probe();
    let regions = Regions::reserve(features.page_size, features.anon_name);
    // Guard slabs need `MADV_GUARD_INSTALL`; the `PROT_NONE` fallback would
    // split every class region into one VMA per guard (class.rs).
    let guard_interval = if features.guard_install { DEFAULT_GUARD_SLAB_INTERVAL } else { 0 };
    // SAFETY: the statics are written once here, before `READY` is
    // published, and `regions` are live reservations this process owns.
    unsafe {
        let ok = Shared::init(
            (*SHARED.0.get()).as_mut_ptr(),
            regions,
            guard_interval,
            features.guard_install,
        );
        if !ok {
            sys::abort("challoc: metadata region too small for the allocator tables");
        }
        (*FEATURES.0.get()).write(features);
    }
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    let rc = unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) };
    if rc != 0 {
        sys::abort_fmt(format_args!("challoc: pthread_atfork failed: {}", sys::Errno(rc)));
    }
}

/// Take every allocator lock so the child inherits a consistent heap.
extern "C" fn fork_prepare() {
    let s = shared();
    for c in 0..NCLASSES {
        s.classes[c].raw_lock().lock();
    }
    s.huge.raw_lock().lock();
    s.meta.raw_lock().lock();
}

/// Release the locks taken by `fork_prepare`, in reverse order.
fn fork_release() {
    let s = shared();
    s.meta.raw_lock().unlock();
    s.huge.raw_lock().unlock();
    for c in (0..NCLASSES).rev() {
        s.classes[c].raw_lock().unlock();
    }
}

extern "C" fn fork_parent() {
    fork_release();
}

/// The child has one thread; the locks were held by the forking thread, so
/// releasing them is exactly right. Phase 1 also marks the background
/// thread absent here.
extern "C" fn fork_child() {
    fork_release();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::vec::Vec;

    #[test]
    fn init_is_idempotent_and_concurrent() {
        let handles: Vec<_> = (0..16)
            .map(|_| thread::spawn(|| shared() as *const Shared as usize))
            .collect();
        let first = shared() as *const Shared as usize;
        for h in handles {
            assert_eq!(h.join().unwrap(), first);
        }
        assert!(is_ready());
        assert!(shared_if_ready().is_some());
        assert!(shared().has_thread_key());
        let f = features();
        assert_eq!(f.page_size, shared().regions.page_size());
    }

    #[test]
    fn fork_handlers_take_and_release_every_lock() {
        let s = shared();
        fork_prepare();
        for c in 0..NCLASSES {
            assert!(s.classes[c].raw_lock().is_locked(), "class {c} not locked");
        }
        assert!(s.huge.raw_lock().is_locked());
        assert!(s.meta.raw_lock().is_locked());
        fork_parent();
        for c in 0..NCLASSES {
            assert!(!s.classes[c].raw_lock().is_locked(), "class {c} still locked");
        }
        assert!(!s.huge.raw_lock().is_locked());
        assert!(!s.meta.raw_lock().is_locked());
        fork_prepare();
        fork_child();
        assert!(!s.meta.raw_lock().is_locked());
    }
}
