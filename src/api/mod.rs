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
//! The fork prepare handler takes every allocator lock in nesting order
//! (design section 9; the list is [`fork_prepare`]'s body): the explicit-cache
//! table lock, the arena creation lock, the process statistics lock, every
//! arena's class locks, the slab table locks, every arena's slab-set lock,
//! the huge lock, the metadata lock, the per-arena statistics locks, then
//! the background thread's state lock. That order matches the allocator's
//! own nesting (the explicit-cache table lock is held across heap creation
//! and destruction, which take the arena creation lock, class locks and the
//! metadata lock; `epoch` holds the process statistics lock across its
//! scan, which takes the class, slab table, huge, metadata and per-arena
//! statistics locks; `take_group` holds a class lock and its slab table
//! lock while it takes the arena's slab-set lock; class, slab table,
//! slab-set and huge code take the metadata lock while holding their own;
//! nothing takes a class or huge lock while holding metadata; the
//! per-arena statistics locks and the background state lock are leaves),
//! so a thread that is mid-operation when another thread
//! forks can always finish and release. Parent and child release in
//! reverse. The child also marks the background thread absent. Other
//! threads' heaps are left alone in the child, as in every other allocator.
//!
//! The prepare handler also blocks every signal on the forking thread and the
//! parent and child handlers restore the mask after their unlocks. glibc's
//! `_Fork` blocks signals only around the `clone` itself, so a handler that
//! runs between our prepare and our parent handler (glibc restores the mask
//! before the parent handlers run) would otherwise see every allocator lock
//! held by its own thread: a `fork` from that handler deadlocks in prepare,
//! and glibc's `tst-mallocfork` provokes exactly that when the child's
//! `SIGALRM` overtakes the parent's post-fork release (seen under Rosetta).

/// One entry point exported twice, under its plain name and its `je_`
/// alias (design 13.2: what a program built against a
/// `--with-jemalloc-prefix=je_` jemalloc calls), as two thin `extern "C"`
/// wrappers with the same body: stable Rust has no symbol aliases. Used in
/// the `exports` modules, which are compiled out of the test binary (see
/// `libc.rs`); `#[unsafe(no_mangle)]` exports each function under its own
/// name, so the alias is spelled out in full at the definition.
#[cfg(not(test))]
macro_rules! export_pair {
    ($($(#[$m:meta])* fn $name:ident / $je:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? $body:block)*) => {
        $(
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),*) $(-> $ret)? $body
            $(#[$m])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $je($($arg: $ty),*) $(-> $ret)? $body
        )*
    };
}

// The C++ and libc exports are compiled out of the test binary, which
// leaves the helpers only they call unused there.
#[cfg_attr(test, allow(dead_code))]
pub mod cxx;
pub mod glibc;
pub mod jemalloc;
#[cfg_attr(test, allow(dead_code))]
pub mod libc;
pub mod mallctl;
pub mod print;

#[cfg(test)]
mod c_programs;

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::config;
use crate::heap::Shared;
use crate::layout::{self, NCLASSES, Regions};
use crate::purge;
use crate::sys;
use crate::sys::features::{FeatureLine, Features};

/// The crate version as a NUL-terminated static string (`"0.1.0"`; the
/// soname's major, design 13.6, follows its major); with `challoc_features`
/// and the size-returning entry points of design 13.7 (`libc.rs`) the whole
/// challoc-specific surface, declared in `<challoc/challoc.h>`. A program
/// or test that wants to know whether challoc is the loaded allocator looks
/// this symbol up with `dlsym`; it reads and writes no allocator state and
/// does not initialise anything.
/// `mallctl("version")` stays jemalloc-shaped (`5.3.0-0-gchalloc`) for
/// parsers of that string.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_version() -> *const core::ffi::c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const core::ffi::c_char
}

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
/// The feature line for `challoc_features()` and `malloc_stats_print`.
static FEATURE_LINE: Global<FeatureLine> = Global::new();
/// Signal mask of the forking thread, saved by `fork_prepare` and restored by
/// `fork_release`. One slot suffices because it is only written and read
/// while the writer holds every allocator lock: a second thread in
/// `fork_prepare` (glibc serialises fork handlers with its atfork lock, but
/// the unit tests call them directly) waits on the first class lock before
/// it can store, and the releaser reads before its first unlock. A handler
/// cannot nest a fork inside the window because signals are blocked for its
/// duration.
static FORK_SIGMASK: Global<u64> = Global::new();

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

/// The allocator state's address, with no readiness check and no
/// reference formed: what the fast paths hand `heap::existing_heap`, which
/// reads one byte through it only after the thread's TLS slot has proved
/// init complete.
#[inline(always)]
pub fn shared_ptr() -> *const Shared {
    (&raw const SHARED.0).cast::<Shared>()
}

/// [`shared_ptr`] for the free paths. On aarch64 the address goes through
/// an empty `asm!`, so LLVM keeps it in one register and every field the
/// free path reads (the push flags, the region geometry, the class
/// tables, the arena flag) is one `ldr` at an immediate offset from it.
/// Without that each field is its own `adrp` (and `add`) of `SHARED +
/// offset`: LLVM folds the offset into the relocation, so the page
/// addresses of two fields never CSE, and `free` paid five of them.
/// x86_64 addresses statics `rip`-relative at no cost, so there the
/// address stays a constant. Like [`shared_ptr`], no reference is formed.
#[inline(always)]
pub fn shared_base() -> *const Shared {
    #[allow(unused_mut)]
    let mut p = shared_ptr();
    #[cfg(target_arch = "aarch64")]
    // SAFETY: an empty template; the pointer is unchanged.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) p, options(pure, nomem, nostack, preserves_flags));
    }
    p
}

/// The probed kernel features. Initialises the allocator if needed.
#[cfg(test)]
pub fn features() -> &'static Features {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURES.get() }
}

/// The feature line (`Features`'s `Display`, built at init). Initialises
/// the allocator if needed.
pub fn feature_line() -> &'static str {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURE_LINE.get() }.as_str()
}

/// `challoc_features()`: the feature report as one NUL-terminated line of
/// `key=value` pairs (design.md sections 16 and 17): the kernel probes
/// (`page_size`, `guard_install`, `mseal`, `anon_name`, ...) then `pac`.
/// The string is static, built once at init without allocating, and
/// identical to the `Kernel and CPU features:` line of
/// `malloc_stats_print`. Initialises the allocator if needed.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_features() -> *const core::ffi::c_char {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURE_LINE.get() }.as_ptr()
}

/// Whether init has completed (for the tests; the entry points use
/// [`shared_if_ready`]).
#[cfg(test)]
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
                sys::abort("allocation during initialisation (re-entrant init)");
            }
            while STATE.load(Ordering::Acquire) != READY {
                core::hint::spin_loop();
            }
        }
    }
    // SAFETY: `READY` observed (or published by this thread).
    unsafe { SHARED.get() }
}

/// Never-committed ranges of the reservations sealed at init (design 10
/// step 5); 0 when the kernel has no `mseal` or seccomp filters it.
static SEALED: AtomicU32 = AtomicU32::new(0);

/// How many ranges init sealed with `mseal` (for `malloc_stats_print` and
/// tests). Initialises the allocator if needed.
pub fn sealed_ranges() -> u32 {
    let _ = shared();
    SEALED.load(Ordering::Relaxed)
}

/// Design section 10 steps 1 to 8 (step 7, the cgroup discovery, happens
/// inside `Shared::init`). Configuration (step 2) is read before the
/// regions are reserved so that nothing it may abort on has been mapped
/// yet; it reads only the weak symbols, one file and `environ`.
fn init() {
    let features = Features::probe();
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(features.page_size.trailing_zeros()) };
    let config = config::load();
    crate::thp::configure(config.thp, &features);
    purge::configure_advice(config.lazy_band);
    purge::set_check_zero(config.check_zero);
    // The unit-test process shares this allocator between tests that
    // assert on the thread's presence; the lazy start is tested on its own
    // (`purge::tests`, the C programs).
    purge::background::configure(if cfg!(test) {
        config.background_thread.or(Some(false))
    } else {
        config.background_thread
    });
    let regions = Regions::reserve(features.page_size, features.anon_name);
    if features.mseal {
        SEALED.store(regions.seal(), Ordering::Relaxed);
    }
    // SAFETY: the statics are written once here, before `READY` is
    // published, and `regions` are live reservations this process owns.
    unsafe {
        let ok = Shared::init(
            // The cell's own pointer, not `as_mut_ptr` through a `&mut`:
            // `init` keeps pointers derived from it, and writes through
            // the readers' `&Shared` would invalidate ones derived from a
            // `&mut`.
            SHARED.0.get().cast::<Shared>(),
            regions,
            config,
            features.guard_install,
            features.pac,
        );
        if !ok {
            sys::abort("metadata region too small for the allocator tables");
        }
        // Design 11: `Shared::init` forced the stored `thp` to `never`
        // under a cgroup limit below 1 GiB; the advice follows.
        if (*SHARED.0.get()).assume_init_ref().policy.thp_forced() {
            crate::thp::force_never();
        }
        (*FEATURE_LINE.0.get()).write(FeatureLine::build(&features));
        (*FEATURES.0.get()).write(features);
    }
    // SAFETY: the three handlers are plain `extern "C"` functions that only
    // touch allocator locks.
    let rc =
        unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) };
    if rc != 0 {
        sys::abort_fmt(format_args!("pthread_atfork failed: {}", sys::Errno(rc)));
    }
}

/// Block signals, then take every allocator lock so the child inherits a
/// consistent heap and no signal handler on this thread can re-enter the
/// allocator (or fork again) while the locks are held.
///
/// The order is the nesting order (design section 9): the explicit-cache
/// table lock is held across heap creation and destruction, which take the
/// arena creation lock, class locks and the metadata lock; the process
/// statistics lock is held across `epoch`'s scan, which takes the class,
/// slab table, huge, metadata and per-arena statistics locks
/// (`stats.rs`); `take_group` holds a class lock and its slab table lock
/// while it takes the arena's slab-set lock, so every slab table lock
/// comes before every slab-set lock; the per-arena statistics locks are
/// leaves and the background thread's state lock comes last. The test
/// `fork_handlers_take_and_release_every_lock` derives the list of locks
/// from the source tree, so a new `Lock` fails it until it is added here.
extern "C" fn fork_prepare() {
    let mask = sys::block_all_signals();
    let s = shared();
    s.tcaches.raw_lock().lock();
    s.arenas.raw_lock().lock();
    s.stats.raw().lock();
    for a in s.arenas.iter() {
        for c in 0..NCLASSES {
            a.classes[c].raw_lock().lock();
        }
    }
    for c in 0..NCLASSES {
        s.tables()[c].raw_lock().lock();
    }
    for a in s.arenas.iter() {
        a.sets().raw_lock().lock();
    }
    s.huge.raw_lock().lock();
    s.meta.raw_lock().lock();
    for a in s.arenas.iter() {
        a.stats_raw_lock().lock();
    }
    purge::background::fork_lock();
    // SAFETY: every allocator lock is held, so no other forking thread can
    // touch the slot until `fork_release` has read it (see `FORK_SIGMASK`).
    unsafe { (*FORK_SIGMASK.0.get()).write(mask) };
}

/// Release the locks taken by `fork_prepare`, in reverse order, then let
/// signals through again.
fn fork_release() {
    let s = shared();
    // SAFETY: written by the `fork_prepare` that paired with this release,
    // and still ours because every lock is held until the unlocks below.
    let mask = unsafe { (*FORK_SIGMASK.0.get()).assume_init() };
    purge::background::fork_unlock();
    // Arenas cannot appear while the creation lock is held, so this is the
    // same set `fork_prepare` locked, in reverse.
    let n = s.arenas.count() as usize;
    for i in (0..n).rev() {
        if let Some(a) = s.arenas.get_existing(i as u16) {
            a.stats_raw_lock().unlock();
        }
    }
    s.meta.raw_lock().unlock();
    s.huge.raw_lock().unlock();
    for i in (0..n).rev() {
        if let Some(a) = s.arenas.get_existing(i as u16) {
            a.sets().raw_lock().unlock();
        }
    }
    for c in (0..NCLASSES).rev() {
        s.tables()[c].raw_lock().unlock();
    }
    for i in (0..n).rev() {
        if let Some(a) = s.arenas.get_existing(i as u16) {
            for c in (0..NCLASSES).rev() {
                a.classes[c].raw_lock().unlock();
            }
        }
    }
    s.stats.raw().unlock();
    s.arenas.raw_lock().unlock();
    s.tcaches.raw_lock().unlock();
    sys::set_signal_mask(mask);
}

extern "C" fn fork_parent() {
    fork_release();
}

/// The child has one thread; the locks were held by the forking thread, so
/// releasing them is exactly right. Before that, with everything still
/// locked, the background thread is marked absent (design 8.1).
extern "C" fn fork_child() {
    purge::background::fork_child();
    fork_release();
    // The dirty bytes this child inherited, the lazy start's baseline: read
    // after the release (the sum takes the class and huge locks), while
    // the child is still single-threaded, so nothing has moved them.
    purge::background::set_fork_base(crate::policy::empty_bytes(shared()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
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

    /// `challoc_features()` is the `Display` of the probe, NUL-terminated,
    /// static, and the same bytes on every call.
    #[test]
    fn challoc_features_is_the_probe_line() {
        let p = challoc_features();
        assert!(!p.is_null());
        assert_eq!(p, challoc_features());
        let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap();
        assert_eq!(s, std::format!("{}", features()));
        assert_eq!(s, feature_line());
        assert!(s.starts_with("page_size="), "{s}");
        assert!(s.ends_with(" pac=yes") || s.ends_with(" pac=no"), "{s}");
        assert!(!s.contains('\n'));
        for kv in s.split(' ') {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| panic!("`{kv}` in {s}"));
            assert!(!k.is_empty() && !v.is_empty(), "`{kv}` in {s}");
        }
    }

    /// Every `Lock<..>` (or bare `RawLock`) field and static in `src/`, as
    /// `file:Owner.field` (`file:static.NAME` for statics), read from the
    /// source tree so the fork handlers cannot miss a lock added later.
    /// `lock.rs` defines the types and is skipped; so are the modules
    /// declared under `#[cfg(test)]` and each file's `mod tests`.
    fn lock_inventory() -> Vec<String> {
        use std::string::ToString;
        /// Every `.rs` file under `dir` as (path relative to `src/`, text).
        fn sources(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                let rel = if rel.is_empty() {
                    name
                } else {
                    std::format!("{rel}/{name}")
                };
                if path.is_dir() {
                    sources(&path, &rel, out);
                } else if rel.ends_with(".rs") {
                    out.push((rel, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        /// The module paths (relative to `src/`, without `.rs`) that `rel`
        /// declares as `#[cfg(test)] mod name;`.
        fn test_modules(rel: &str, text: &str, out: &mut Vec<String>) {
            let stem = rel.strip_suffix(".rs").unwrap();
            let dir = match stem.rsplit_once('/') {
                Some((d, "mod")) => d.to_string(),
                None if stem == "lib" || stem == "mod" => String::new(),
                _ => stem.to_string(),
            };
            let mut cfg_test = false;
            for line in text.lines() {
                let t = line.trim();
                let decl = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                if let (true, Some(name)) = (
                    cfg_test,
                    decl.strip_prefix("mod ").and_then(|m| m.strip_suffix(';')),
                ) {
                    out.push(if dir.is_empty() {
                        name.to_string()
                    } else {
                        std::format!("{dir}/{name}")
                    });
                }
                cfg_test = t == "#[cfg(test)]";
            }
        }
        let mut files = Vec::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            "",
            &mut files,
        );
        let mut test_only = Vec::new();
        for (rel, text) in &files {
            test_modules(rel, text, &mut test_only);
        }
        let mut out = Vec::new();
        for (rel, text) in &files {
            let stem = rel.strip_suffix(".rs").unwrap();
            if rel == "lock.rs"
                || test_only
                    .iter()
                    .any(|m| stem == m || rel.starts_with(&std::format!("{m}/")))
            {
                continue;
            }
            let mut owner = String::new();
            for line in text.lines() {
                let t = line.trim();
                if t == "mod tests {" {
                    break;
                }
                if t.starts_with("//")
                    || t.starts_with("use ")
                    || t.contains(" fn ")
                    || t.starts_with("fn ")
                {
                    continue;
                }
                let decl = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                if let Some(rest) = decl.strip_prefix("struct ") {
                    owner = rest
                        .split(['<', '{', '(', ' ', ';'])
                        .next()
                        .unwrap()
                        .to_string();
                    continue;
                }
                let is_lock = |ty: &str| ty.contains("Lock<") || ty.contains("RawLock");
                if let Some(rest) = decl.strip_prefix("static ") {
                    if let Some((name, ty)) = rest.split_once(':') {
                        if is_lock(ty) {
                            out.push(std::format!("{rel}:static.{}", name.trim()));
                        }
                    }
                    continue;
                }
                if let Some((name, ty)) = decl.split_once(':') {
                    let name = name.trim();
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        && is_lock(ty)
                        && !ty.contains("->")
                    {
                        out.push(std::format!("{rel}:{owner}.{name}"));
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// The locks `fork_prepare` takes, keyed as in [`lock_inventory`].
    /// Adding a lock to the crate means adding it here *and* to the
    /// handlers; `assert_locks` refuses a key it has no assertion for.
    const FORK_LOCKS: &[&str] = &[
        "arena.rs:Arena.stats",
        "arena.rs:Arenas.lock",
        "heap/tcaches.rs:Tcaches.lock",
        "class/mod.rs:ClassState.inner",
        "class/sets.rs:SetTable.tables",
        "class/table.rs:SlabTable.inner",
        "heap/shared.rs:Shared.stats",
        "huge/mod.rs:Huge.inner",
        "meta.rs:Meta.inner",
        "purge/background.rs:static.STATE",
    ];

    /// Assert that every lock in [`FORK_LOCKS`] is held (`locked`) or free
    /// (`!locked`), for the arenas in `arenas`.
    fn assert_locks(s: &Shared, arenas: &[u16], locked: bool) {
        let state = |held: bool, what: &str| {
            assert_eq!(
                held,
                locked,
                "{what} {}",
                if locked { "not locked" } else { "still locked" }
            );
        };
        for key in FORK_LOCKS {
            match *key {
                "arena.rs:Arena.stats" => {
                    for &a in arenas {
                        state(
                            s.arena(a).unwrap().stats_raw_lock().is_locked(),
                            &std::format!("arena {a} stats"),
                        );
                    }
                }
                "arena.rs:Arenas.lock" => state(s.arenas.raw_lock().is_locked(), "arena registry"),
                "heap/tcaches.rs:Tcaches.lock" => {
                    state(s.tcaches.raw_lock().is_locked(), "tcache table")
                }
                "class/mod.rs:ClassState.inner" => {
                    for &a in arenas {
                        for c in 0..NCLASSES {
                            state(
                                s.arena(a).unwrap().classes[c].raw_lock().is_locked(),
                                &std::format!("arena {a} class {c}"),
                            );
                        }
                    }
                }
                "class/sets.rs:SetTable.tables" => {
                    for &a in arenas {
                        state(
                            s.arena(a).unwrap().sets().raw_lock().is_locked(),
                            &std::format!("arena {a} sets"),
                        );
                    }
                }
                "class/table.rs:SlabTable.inner" => {
                    for c in 0..NCLASSES {
                        state(
                            s.tables()[c].raw_lock().is_locked(),
                            &std::format!("table {c}"),
                        );
                    }
                }
                "heap/shared.rs:Shared.stats" => state(s.stats.raw().is_locked(), "process stats"),
                "huge/mod.rs:Huge.inner" => state(s.huge.raw_lock().is_locked(), "huge"),
                "meta.rs:Meta.inner" => state(s.meta.raw_lock().is_locked(), "meta"),
                "purge/background.rs:static.STATE" => {
                    state(purge::background::is_locked(), "background state")
                }
                other => panic!("no fork-handler assertion for lock {other}"),
            }
        }
    }

    /// The set of locks the handlers cover is exactly the set of locks in
    /// the crate (design 9).
    #[test]
    fn fork_lock_list_matches_the_source_tree() {
        let found = lock_inventory();
        let mut want: Vec<String> = FORK_LOCKS
            .iter()
            .map(|k| std::string::ToString::to_string(k))
            .collect();
        want.sort();
        assert_eq!(
            found, want,
            "locks in src/ differ from the fork handlers' list: add every new lock to \
             fork_prepare/fork_release in nesting order, to FORK_LOCKS and to assert_locks"
        );
    }

    /// The handlers take every lock and release every lock, on both fork
    /// sides. The checks run in a fork child: in this process another test
    /// can take a heap lock (a huge allocation, a refill) the moment the
    /// handlers release it, so "every lock free" is only exact where no
    /// other thread runs (#69). The real `fork` runs the handlers through
    /// `pthread_atfork`, so the child starts from `fork_child`'s release.
    #[test]
    fn fork_handlers_take_and_release_every_lock() {
        let _serial = crate::testutil::global_state();
        let s = shared();
        // A second arena so the handlers have more than arena 0 to cover.
        let extra = s.arenas.create(s).unwrap();
        let arenas = [0, extra];
        crate::testutil::in_fork_child("fork handlers", 60_000, || {
            assert_locks(s, &arenas, false);
            assert!(!purge::background::present());
            assert!(!purge::background::requested());
            fork_prepare();
            assert_locks(s, &arenas, true);
            fork_parent();
            assert_locks(s, &arenas, false);
            fork_prepare();
            assert_locks(s, &arenas, true);
            fork_child();
            assert_locks(s, &arenas, false);
            assert!(!purge::background::present());
            assert!(!purge::background::requested());
        });
    }

    /// The background thread starts on request, is visible under
    /// `/proc/self/task` with its name, purges without any slow-path
    /// tick, is gone from the bookkeeping in a fork child while the
    /// parent's keeps running, and stops when the request is withdrawn.
    #[test]
    fn background_thread_starts_purges_and_is_absent_after_fork() {
        use std::string::ToString;
        fn tasks() -> std::vec::Vec<std::string::String> {
            let mut names = std::vec::Vec::new();
            for e in std::fs::read_dir("/proc/self/task").unwrap() {
                let p = e.unwrap().path().join("comm");
                names.push(
                    std::fs::read_to_string(p)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                );
            }
            names
        }
        let _serial = crate::testutil::global_state();
        let s = shared();
        assert!(!tasks().iter().any(|n| n == "challoc-bg"));
        purge::background::request(true);
        assert!(
            purge::background::start(s),
            "background thread did not start"
        );
        assert!(purge::background::present());
        assert!(purge::background::start(s), "second start is idempotent");
        // The name is set by the thread itself; give it a moment.
        let mut seen = false;
        for _ in 0..200 {
            if tasks().iter().any(|n| n == "challoc-bg") {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(seen, "no task named challoc-bg: {:?}", tasks());

        // A 1 ms decay: free a huge run, wait, and the thread purges it
        // with nobody else touching the allocator.
        let old_ns = s.decay_ns();
        s.set_decay_ms(1);
        let (p, _) = s.alloc_huge(8 << 20, 4096, 0);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
        s.free_huge(page);
        // No check that the run is still `Empty` here: with a 1 ms decay
        // the thread can purge it before this thread runs again (seen on
        // a loaded Docker VM); the loop below is the assertion.
        let mut purged = false;
        for _ in 0..400 {
            if s.huge.oldest_empty() == 0 && s.huge.stats().empty == 0 {
                purged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            purged,
            "background thread did not purge the freed run: {:?}",
            s.huge.stats()
        );
        s.set_decay_ms((old_ns / 1_000_000) as i64);

        // A real fork: the child handler forgets the thread and the
        // request in the child, and leaves this process's thread alone.
        // The child checks with atomics and the state lock only and
        // answers through its exit status.
        assert!(purge::background::thread().is_some(), "no thread record");
        let pid = unsafe { ::libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let bad = [
                purge::background::present(),
                purge::background::requested(),
                purge::background::is_locked(),
                purge::background::thread().is_some(),
            ];
            let code = bad
                .iter()
                .enumerate()
                .fold(0, |m, (i, &b)| m | ((b as i32) << i));
            unsafe { ::libc::_exit(code) };
        }
        let mut status = 0;
        assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            ::libc::WIFEXITED(status),
            "fork child did not exit: status {status:#x}"
        );
        assert_eq!(
            ::libc::WEXITSTATUS(status),
            0,
            "fork child still sees (bit 0) present, (1) requested, (2) the state lock held, (3) a thread record"
        );
        assert!(
            purge::background::present() && purge::background::requested(),
            "the fork stopped the parent's thread"
        );

        // Withdraw the request: the kick wakes the thread and it exits
        // there; the exit is asynchronous, so wait for it, bounded, and
        // join it.
        purge::background::request(false);
        let mut stopped = false;
        for _ in 0..400 {
            if !purge::background::present() {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            stopped,
            "background thread did not stop at the kick: {:?}",
            tasks()
        );
        assert!(purge::background::join_exited(), "no exited thread to join");
        assert!(purge::background::thread().is_none());
        // Joined, the task leaves /proc/self/task as the kernel reaps it.
        let mut gone = false;
        for _ in 0..400 {
            if !tasks().iter().any(|n| n == "challoc-bg") {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            gone,
            "background thread still running after the join: {:?}",
            tasks()
        );
    }

    /// The lazy start's rule (design 8.1): the burst threshold starts at
    /// the first look; the floor only once every look for the hold has
    /// seen it; a look under the floor ends the stretch.
    #[test]
    fn lazy_start_rule() {
        use core::sync::atomic::AtomicU64;
        use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS, auto_due};
        let since = AtomicU64::new(0);
        let t = 5_000_000_000u64;
        assert!(auto_due(AUTO_BURST, t, &since), "burst at the first look");
        assert!(!auto_due(AUTO_FLOOR - 1, t, &since));
        assert!(
            !auto_due(AUTO_FLOOR - 1, t + 10 * AUTO_HOLD_NS, &since),
            "under the floor never"
        );
        assert!(
            !auto_due(AUTO_FLOOR, t, &since),
            "the floor starts a stretch"
        );
        assert!(!auto_due(AUTO_BURST - 1, t + AUTO_HOLD_NS / 2, &since));
        assert!(
            auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since),
            "held for the hold"
        );
        // A dip under the floor restarts the stretch.
        since.store(0, core::sync::atomic::Ordering::Relaxed);
        assert!(!auto_due(AUTO_FLOOR, t, &since));
        assert!(!auto_due(AUTO_FLOOR / 2, t + AUTO_HOLD_NS / 2, &since));
        assert!(!auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since));
        assert!(!auto_due(
            AUTO_FLOOR,
            t + AUTO_HOLD_NS + AUTO_HOLD_NS / 2,
            &since
        ));
        assert!(auto_due(AUTO_FLOOR, t + 2 * AUTO_HOLD_NS, &since));
    }

    /// The lazy start end to end on one allocator, on injected tick
    /// times: nothing dirty starts nothing; 8 MiB dirty (over the floor,
    /// under the burst threshold) starts nothing at the first look or
    /// half a hold later; once the hold has passed a free's tick only
    /// requests the thread and the next allocation's tick starts it;
    /// the start leaves the lazy start armed; an explicit `request`
    /// disarms it.
    #[test]
    fn lazy_start_waits_for_dirty_memory() {
        use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS};
        let _serial = crate::testutil::global_state();
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        // A window long enough that nothing decays between the looks.
        let old_ns = s.decay_ns();
        s.set_decay_ms(1_000_000);
        assert!((8u64 << 20) >= AUTO_FLOOR && (8u64 << 20) < AUTO_BURST);
        purge::background::set_auto(Some(s));
        let t0 = crate::sys::now_coarse_ns();
        purge::on_tick(s, t0, true);
        purge::on_tick(s, t0 + 2 * AUTO_HOLD_NS, true);
        let clean = purge::background::present();
        let (p, _) = s.alloc_huge(8 << 20, 4096, 0);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        s.free_huge((p as usize - s.regions.huge_base) >> s.regions.lg_page);
        let t1 = t0 + 3 * AUTO_HOLD_NS;
        purge::on_tick(s, t1, true);
        let first = purge::background::present();
        purge::on_tick(s, t1 + AUTO_HOLD_NS / 2, true);
        let half = purge::background::present();
        // A free's tick past the hold requests the thread but never
        // creates it (glibc frees under locks `pthread_create` takes);
        // the next allocation's tick does.
        purge::on_tick(s, t1 + AUTO_HOLD_NS + 1, false);
        let freed = purge::background::present();
        let pending = purge::background::requested();
        purge::on_tick(s, t1 + AUTO_HOLD_NS + 2, true);
        let held = purge::background::present();
        let armed = purge::background::auto();
        purge::background::request(false);
        let disarmed = !purge::background::auto();
        for _ in 0..400 {
            if !purge::background::present() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let stopped = !purge::background::present();
        if stopped {
            purge::background::join_exited();
        }
        purge::background::set_auto(None);
        s.huge.purge_empty(u64::MAX);
        s.set_decay_ms((old_ns / 1_000_000) as i64);

        assert!(!clean, "nothing dirty started the thread");
        assert!(!first, "8 MiB started the thread at the first look");
        assert!(!half, "8 MiB started the thread before the hold");
        assert!(!freed, "a free's tick created the thread");
        assert!(pending, "a free's tick past the hold did not request it");
        assert!(held, "8 MiB held past the hold did not start the thread");
        assert!(armed, "a successful start disarmed the lazy start");
        assert!(disarmed, "request(false) left the lazy start armed");
        assert!(stopped, "background thread did not stop");
    }

    /// A first `Empty` that lands while `start` is publishing the thread
    /// is purged: the thread has already scanned and gone to sleep with
    /// no deadline, and the free's `wake` does nothing because the
    /// thread is not yet present, so `start` must kick it once it is.
    /// Run alone, it fails without that kick; in the full suite another
    /// test's first `Empty` can wake the thread instead and hide it.
    #[test]
    fn background_thread_purges_an_empty_from_its_startup() {
        use core::sync::atomic::{AtomicU8, Ordering};
        use std::sync::Arc;
        let _serial = crate::testutil::global_state();
        // A private allocator: the process-wide one is shared with the
        // tests that do not take `global_state`, whose huge frees could
        // leave a run `Empty` before the hook's.
        let s = crate::testutil::shared();
        assert!(!purge::background::present());
        let old_ns = s.decay_ns();
        s.set_decay_ms(1);
        let (p, _) = s.alloc_huge(8 << 20, 4096, 0);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
        let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
        // Nothing else `Empty` in the huge allocator, so the free in the
        // hook is its first.
        s.huge.purge_empty(u64::MAX);
        // 0: the hook did not run; 1: the thread never slept without a
        // deadline; 2: the free was not the first `Empty`; 3: ready.
        let hook_state = Arc::new(AtomicU8::new(0));
        let hook_seen = Arc::clone(&hook_state);
        let waits = purge::background::UNTIMED_WAITS.load(Ordering::SeqCst);
        *purge::background::BEFORE_PUBLISH.lock().unwrap() =
            Some(std::boxed::Box::new(move || {
                // No panics here: unwinding out of `start` would leave the
                // state marked as creating for every later test.
                let mut asleep = false;
                for _ in 0..1000 {
                    if purge::background::UNTIMED_WAITS.load(Ordering::SeqCst) != waits {
                        asleep = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if !asleep {
                    hook_seen.store(1, Ordering::SeqCst);
                    return;
                }
                let none_before = s.huge.oldest_empty() == 0;
                s.free_huge(page);
                let first = none_before && s.huge.oldest_empty() != 0;
                hook_seen.store(if first { 3 } else { 2 }, Ordering::SeqCst);
            }));
        purge::background::request(true);
        let started = purge::background::start(s);
        let mut purged = false;
        if started && hook_state.load(Ordering::SeqCst) == 3 {
            for _ in 0..400 {
                if s.huge.oldest_empty() == 0 && s.huge.stats().empty == 0 {
                    purged = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // Clean up before asserting, so a failure here leaves no thread
        // or hook behind for the other background tests.
        purge::background::BEFORE_PUBLISH.lock().unwrap().take();
        if hook_state.load(Ordering::SeqCst) < 2 {
            s.free_huge(page);
        }
        s.set_decay_ms((old_ns / 1_000_000) as i64);
        purge::background::request(false);
        for _ in 0..400 {
            if !purge::background::present() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let stopped = !purge::background::present();
        if stopped {
            purge::background::join_exited();
        }
        assert!(started, "background thread did not start");
        assert_eq!(
            hook_state.load(Ordering::SeqCst),
            3,
            "hook: 0 did not run, 1 the thread never slept without a deadline, \
             2 the free was not the first Empty"
        );
        assert!(
            purged,
            "background thread slept through a first Empty from its startup: {:?}",
            s.huge.stats()
        );
        assert!(stopped, "background thread did not stop");
    }

    /// One stack for the life of the process (design 8.1): a
    /// stop/start cycle joins the exited thread and hands the same
    /// reservation to the next one, so toggling `background_thread` does
    /// not grow the metadata region.
    #[test]
    fn background_stack_is_reused_across_restarts() {
        fn wait_absent() {
            for _ in 0..400 {
                if !purge::background::present() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("background thread did not stop");
        }
        let _serial = crate::testutil::global_state();
        let s = shared();
        assert!(!purge::background::present());
        purge::background::request(true);
        assert!(
            purge::background::start(s),
            "background thread did not start"
        );
        let stack = purge::background::stack();
        assert_ne!(stack, 0);
        for cycle in 0..8 {
            purge::background::request(false);
            wait_absent();
            purge::background::request(true);
            assert!(purge::background::start(s), "restart {cycle} failed");
            assert!(purge::background::present());
            assert_eq!(
                purge::background::stack(),
                stack,
                "restart {cycle} took a new stack"
            );
        }
        purge::background::request(false);
        wait_absent();
        assert!(purge::background::join_exited());
        assert!(purge::background::thread().is_none());
    }

    /// The handlers block every signal while the locks are held and restore
    /// the caller's mask (whatever it was) afterwards, on both fork sides.
    #[test]
    fn fork_handlers_block_signals_while_locked() {
        fn current_mask() -> u64 {
            let mut set: ::libc::sigset_t = unsafe { core::mem::zeroed() };
            let rc =
                unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
            assert_eq!(rc, 0);
            // The kernel mask is the first 8 bytes of glibc's 128-byte set.
            unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
        }
        fn blockable(mask: u64) -> u64 {
            const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
            mask & !KILL_STOP
        }
        let _serial = crate::testutil::global_state();
        let all = blockable(!0);
        // Start from a distinctive mask so "restored" is not "left blocked".
        let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        let mut before: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        unsafe {
            ::libc::sigemptyset(&mut want);
            ::libc::sigaddset(&mut want, ::libc::SIGUSR2);
            ::libc::sigaddset(&mut want, ::libc::SIGWINCH);
            assert_eq!(
                ::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, &mut before),
                0
            );
        }
        let start = current_mask();
        assert_eq!(
            start,
            (1 << (::libc::SIGUSR2 - 1)) | (1 << (::libc::SIGWINCH - 1))
        );

        fork_prepare();
        assert_eq!(
            blockable(current_mask()),
            all,
            "prepare did not block every signal"
        );
        fork_parent();
        assert_eq!(
            current_mask(),
            start,
            "parent handler did not restore the mask"
        );

        fork_prepare();
        assert_eq!(blockable(current_mask()), all);
        fork_child();
        assert_eq!(
            current_mask(),
            start,
            "child handler did not restore the mask"
        );

        unsafe { ::libc::pthread_sigmask(::libc::SIG_SETMASK, &before, core::ptr::null_mut()) };
    }
}
