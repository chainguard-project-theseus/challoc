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
//! table lock, the arena creation lock, every arena's class locks and
//! slab-set lock, the slab table locks, the huge lock, the metadata lock,
//! the process and per-arena statistics locks, then the background
//! thread's state lock. That order matches the allocator's own nesting (the
//! explicit-cache table lock is held across heap creation and destruction,
//! which take the arena creation lock, class locks and the metadata lock;
//! class, slab table and huge code take the metadata lock while holding
//! their own; nothing takes a class or huge lock while holding metadata;
//! the statistics locks and the background state lock are leaves, only
//! ever held alone), so a thread that is mid-operation when another thread
//! forks can always finish and release. Parent and child release in reverse. The
//! child also marks the background thread absent and drops a `process_madvise`
//! pidfd that now names the parent. Other threads' heaps are left alone in
//! the child, as in every other allocator.
//!
//! The prepare handler also blocks every signal on the forking thread and the
//! parent and child handlers restore the mask after their unlocks. glibc's
//! `_Fork` blocks signals only around the `clone` itself, so a handler that
//! runs between our prepare and our parent handler (glibc restores the mask
//! before the parent handlers run) would otherwise see every allocator lock
//! held by its own thread: a `fork` from that handler deadlocks in prepare,
//! and glibc's `tst-mallocfork` provokes exactly that when the child's
//! `SIGALRM` overtakes the parent's post-fork release (seen under Rosetta).

pub mod cxx;
pub mod glibc;
pub mod jemalloc;
pub mod libc;
pub mod mallctl;
pub mod print;

#[cfg(test)]
mod ctests;

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::config;
use crate::heap::Shared;
use crate::layout::{self, NCLASSES, Regions};
use crate::purge;
use crate::sys;
use crate::sys::features::{FeatureLine, Features};

/// The one challoc-specific export (design 16): the crate version as a
/// NUL-terminated static string (`"0.1.0"`; the soname's major, design
/// 13.6, follows its major). A program or test that wants to know whether
/// challoc is the loaded allocator looks this symbol up with `dlsym`; it
/// reads and writes no allocator state and does not initialise anything.
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

/// The feature line (`Features`'s `Display`, built at init). Initialises
/// the allocator if needed.
pub fn feature_line() -> &'static str {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURE_LINE.get() }.as_str()
}

/// `challoc_features()`: the feature report as one NUL-terminated line of
/// `key=value` pairs (design.md sections 16 and 17): the kernel probes
/// (`page_size`, `guard_install`, `mseal`, `process_madvise`, `anon_name`)
/// then the CPU (`mte`, `pac`, `lse`, `lse2`, `bti`, `sve` on arm64; `lam`,
/// `uai`, `la57`, `la57_enabled`, `cldemote`, `waitpkg`, `erms`, `fsrm`, `avx512f`
/// on x86_64; `cacheline`, `l1d`, `l2` on both). The string is static, built
/// once at init without allocating, and identical to the `Kernel and CPU
/// features:` line of `malloc_stats_print`. Initialises the allocator if
/// needed.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_features() -> *const core::ffi::c_char {
    let _ = shared();
    // SAFETY: `shared()` returned, so `READY` has been observed.
    unsafe { FEATURE_LINE.get() }.as_ptr()
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
    let mut features = Features::probe();
    // The slab geometry of the classes whose table slab is below the page
    // (design 3, page-sized slabs): written once here, before any other
    // thread can see the allocator, never again.
    // SAFETY: `init` runs once, under `INITIALISING`, before `READY`.
    unsafe { layout::adopt_page_geometry(features.page_size.trailing_zeros()) };
    let config = config::load();
    // `config::load` ran `slice::configure` on this thread: the feature
    // line's `slice_ext=` word can now say `on` (design 17, "Time slice
    // extension").
    features.note_slice_ext();
    // The `MADV_COLLAPSE` probe populates a PMD of memory, so only a
    // configuration that asks for huge pages pays for it (design 10).
    if config.thp != config::Thp::Never {
        features.probe_collapse();
    }
    crate::thp::configure(config.thp, &features);
    // `thp_process:advised` (design 17, `PR_THP_DISABLE_EXCEPT_ADVISED`):
    // process-wide, so before any region exists; a refused call is
    // reported, not retried.
    crate::thp::configure_process(config.thp_process(), &features);
    purge::configure(features.process_madvise, config.lazy_free);
    purge::configure_advice(features.process_madvise_free, config.lazy_band);
    purge::set_check_zero(config.check_zero);
    crate::class::set_group_handoff(config.group_handoff);
    // The prefault pass (design 8.2) runs on the background thread, so
    // `prefault_active` is the one gate (the key without the thread is
    // stored and inert).
    purge::prefault::configure(config.prefault_active(), features.populate_write);
    purge::background::request(config.background_thread);
    let regions = Regions::reserve(features.page_size, features.anon_name);
    if features.mseal {
        SEALED.store(regions.seal(), Ordering::Relaxed);
    }
    // SAFETY: the statics are written once here, before `READY` is
    // published, and `regions` are live reservations this process owns.
    unsafe {
        let pac = matches!(features.cpu.isa, crate::sys::cpu::Isa::Arm(a) if a.pac) && crate::sys::pac::available();
        let ok = Shared::init((*SHARED.0.get()).as_mut_ptr(), regions, config, features.guard_install, pac, true);
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
    let rc = unsafe { ::libc::pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child)) };
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
/// arena creation lock, class locks and the metadata lock; the statistics
/// locks are leaves (`stats.rs` takes each with nothing else held) and
/// the background thread's state lock comes last. The test
/// `fork_handlers_take_and_release_every_lock` derives the list of locks
/// from the source tree, so a new `Lock` fails it until it is added here.
extern "C" fn fork_prepare() {
    let mask = sys::block_all_signals();
    let s = shared();
    s.tcaches.raw_lock().lock();
    s.arenas.raw_lock().lock();
    for a in s.arenas.iter() {
        for c in 0..NCLASSES {
            a.classes[c].raw_lock().lock();
        }
        a.sets().raw_lock().lock();
    }
    for c in 0..NCLASSES {
        s.tables()[c].raw_lock().lock();
    }
    s.huge.raw_lock().lock();
    s.meta.raw_lock().lock();
    s.stats.raw().lock();
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
    s.stats.raw().unlock();
    s.meta.raw_lock().unlock();
    s.huge.raw_lock().unlock();
    for c in (0..NCLASSES).rev() {
        s.tables()[c].raw_lock().unlock();
    }
    for i in (0..n).rev() {
        if let Some(a) = s.arenas.get_existing(i as u16) {
            a.sets().raw_lock().unlock();
            for c in (0..NCLASSES).rev() {
                a.classes[c].raw_lock().unlock();
            }
        }
    }
    s.arenas.raw_lock().unlock();
    s.tcaches.raw_lock().unlock();
    sys::set_signal_mask(mask);
}

extern "C" fn fork_parent() {
    fork_release();
}

/// The child has one thread; the locks were held by the forking thread, so
/// releasing them is exactly right. Before that, with everything still
/// locked, the background thread is marked absent (design 8.1) and the
/// discard backend drops a pidfd that names the parent, which the feature
/// record reflects.
extern "C" fn fork_child() {
    purge::background::fork_child();
    // The parent's PSI trigger descriptor was closed by `fork_child` above;
    // the child's cgroup record says so until a thread of its own arms one.
    shared().cgroup.set_psi_unavailable(0);
    let backend = purge::fork_child();
    // SAFETY: the child is single threaded and every lock is held, so
    // nothing reads the features while they change.
    unsafe {
        let features = (*FEATURES.0.get()).assume_init_mut();
        features.process_madvise = backend;
        (*FEATURE_LINE.0.get()).write(FeatureLine::build(features));
    }
    fork_release();
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
        assert!(s.contains(" cacheline="), "{s}");
        assert!(!s.contains('\n'));
        for kv in s.split(' ') {
            let (k, v) = kv.split_once('=').unwrap_or_else(|| panic!("`{kv}` in {s}"));
            assert!(!k.is_empty() && !v.is_empty(), "`{kv}` in {s}");
        }
    }

    /// The two handler tests drive the process-wide locks directly and the
    /// harness runs tests in parallel, so they take turns.
    static FORK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Every `Lock<..>` (or bare `RawLock`) field and static in `src/`, as
    /// `file:Owner.field` (`file:static.NAME` for statics), read from the
    /// source tree so the fork handlers cannot miss a lock added later.
    /// `lock.rs` defines the types and is skipped; so is each file's
    /// `mod tests`.
    fn lock_inventory() -> Vec<String> {
        use std::string::ToString;
        fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<String>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            entries.sort();
            for path in entries {
                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                let rel = if rel.is_empty() { name.clone() } else { std::format!("{rel}/{name}") };
                if path.is_dir() {
                    walk(&path, &rel, out);
                    continue;
                }
                if !name.ends_with(".rs") || rel == "lock.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let mut owner = String::new();
                for line in text.lines() {
                    let t = line.trim();
                    if t == "mod tests {" {
                        break;
                    }
                    if t.starts_with("//") || t.starts_with("use ") || t.contains(" fn ") || t.starts_with("fn ") {
                        continue;
                    }
                    let decl = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
                    if let Some(rest) = decl.strip_prefix("struct ") {
                        owner = rest.split(['<', '{', '(', ' ', ';']).next().unwrap().to_string();
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
        }
        let mut out = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), "", &mut out);
        out.sort();
        out
    }

    /// The locks `fork_prepare` takes, keyed as in [`lock_inventory`].
    /// Adding a lock to the crate means adding it here *and* to the
    /// handlers; `assert_locks` refuses a key it has no assertion for.
    const FORK_LOCKS: &[&str] = &[
        "arena.rs:Arena.stats",
        "arena.rs:Arenas.lock",
        "arena.rs:Tcaches.lock",
        "class.rs:ClassState.inner",
        "class.rs:SetTable.tables",
        "class.rs:SlabTable.inner",
        "heap.rs:Shared.stats",
        "huge.rs:Huge.inner",
        "meta.rs:Meta.inner",
        "purge.rs:static.STATE",
    ];

    /// Assert that every lock in [`FORK_LOCKS`] is held (`locked`) or free
    /// (`!locked`), for the arenas in `arenas`.
    fn assert_locks(s: &Shared, arenas: &[u16], locked: bool) {
        let state = |held: bool, what: &str| {
            assert_eq!(held, locked, "{what} {}", if locked { "not locked" } else { "still locked" });
        };
        for key in FORK_LOCKS {
            match *key {
                "arena.rs:Arena.stats" => {
                    for &a in arenas {
                        state(s.arena(a).unwrap().stats_raw_lock().is_locked(), &std::format!("arena {a} stats"));
                    }
                }
                "arena.rs:Arenas.lock" => state(s.arenas.raw_lock().is_locked(), "arena registry"),
                "arena.rs:Tcaches.lock" => state(s.tcaches.raw_lock().is_locked(), "tcache table"),
                "class.rs:ClassState.inner" => {
                    for &a in arenas {
                        for c in 0..NCLASSES {
                            state(s.arena(a).unwrap().classes[c].raw_lock().is_locked(), &std::format!("arena {a} class {c}"));
                        }
                    }
                }
                "class.rs:SetTable.tables" => {
                    for &a in arenas {
                        state(s.arena(a).unwrap().sets().raw_lock().is_locked(), &std::format!("arena {a} sets"));
                    }
                }
                "class.rs:SlabTable.inner" => {
                    for c in 0..NCLASSES {
                        state(s.tables()[c].raw_lock().is_locked(), &std::format!("table {c}"));
                    }
                }
                "heap.rs:Shared.stats" => state(s.stats.raw().is_locked(), "process stats"),
                "huge.rs:Huge.inner" => state(s.huge.raw_lock().is_locked(), "huge"),
                "meta.rs:Meta.inner" => state(s.meta.raw_lock().is_locked(), "meta"),
                "purge.rs:static.STATE" => state(purge::background::is_locked(), "background state"),
                other => panic!("no fork-handler assertion for lock {other}"),
            }
        }
    }

    /// The set of locks the handlers cover is exactly the set of locks in
    /// the crate (design 9).
    #[test]
    fn fork_lock_list_matches_the_source_tree() {
        let found = lock_inventory();
        let mut want: Vec<String> = FORK_LOCKS.iter().map(|k| std::string::ToString::to_string(k)).collect();
        want.sort();
        assert_eq!(
            found, want,
            "locks in src/ differ from the fork handlers' list: add every new lock to \
             fork_prepare/fork_release in nesting order, to FORK_LOCKS and to assert_locks"
        );
    }

    #[test]
    fn fork_handlers_take_and_release_every_lock() {
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        // A second arena so the handlers have more than arena 0 to cover.
        let extra = s.arenas.create(s).unwrap();
        let arenas = [0, extra];
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
    }

    /// The background thread starts on request, is visible under
    /// `/proc/self/task` with its name, purges without any slow-path
    /// tick, and is gone from the bookkeeping after a fork child handler.
    #[test]
    fn background_thread_starts_purges_and_is_absent_after_fork() {
        use std::string::ToString;
        fn tasks() -> std::vec::Vec<std::string::String> {
            let mut names = std::vec::Vec::new();
            for e in std::fs::read_dir("/proc/self/task").unwrap() {
                let p = e.unwrap().path().join("comm");
                names.push(std::fs::read_to_string(p).unwrap_or_default().trim().to_string());
            }
            names
        }
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        assert!(!tasks().iter().any(|n| n == "challoc-bg"));
        purge::background::request(true);
        assert!(purge::background::start(s), "background thread did not start");
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
        assert!(purged, "background thread did not purge the freed run: {:?}", s.huge.stats());
        s.set_decay_ms((old_ns / 1_000_000) as i64);

        // The child handler forgets the thread and the request. The thread
        // itself keeps running in this (parent) process, so withdraw the
        // request again to kick it: it exits at that wake-up. The handler
        // dropped its record (a real child has no such thread), so this
        // process joins it by hand to keep glibc's view of the stack
        // consistent with the next `start`.
        let thread = purge::background::thread().expect("a thread record");
        fork_prepare();
        fork_child();
        assert!(!purge::background::present());
        assert!(!purge::background::requested());
        assert!(!purge::background::is_locked());
        assert!(purge::background::thread().is_none());
        purge::background::request(false);
        let mut gone = false;
        for _ in 0..400 {
            if !tasks().iter().any(|n| n == "challoc-bg") {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(gone, "background thread still running: {:?}", tasks());
        assert_eq!(unsafe { ::libc::pthread_join(thread, core::ptr::null_mut()) }, 0);
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
        let _serial = FORK_TEST.lock().unwrap();
        let s = shared();
        assert!(!purge::background::present());
        purge::background::request(true);
        assert!(purge::background::start(s), "background thread did not start");
        let stack = purge::background::stack();
        assert_ne!(stack, 0);
        for cycle in 0..8 {
            purge::background::request(false);
            wait_absent();
            purge::background::request(true);
            assert!(purge::background::start(s), "restart {cycle} failed");
            assert!(purge::background::present());
            assert_eq!(purge::background::stack(), stack, "restart {cycle} took a new stack");
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
            let rc = unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
            assert_eq!(rc, 0);
            // The kernel mask is the first 8 bytes of glibc's 128-byte set.
            unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
        }
        fn blockable(mask: u64) -> u64 {
            const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
            mask & !KILL_STOP
        }
        let _serial = FORK_TEST.lock().unwrap();
        let all = blockable(!0);
        // Start from a distinctive mask so "restored" is not "left blocked".
        let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        let mut before: ::libc::sigset_t = unsafe { core::mem::zeroed() };
        unsafe {
            ::libc::sigemptyset(&mut want);
            ::libc::sigaddset(&mut want, ::libc::SIGUSR2);
            ::libc::sigaddset(&mut want, ::libc::SIGWINCH);
            assert_eq!(::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, &mut before), 0);
        }
        let start = current_mask();
        assert_eq!(start, (1 << (::libc::SIGUSR2 - 1)) | (1 << (::libc::SIGWINCH - 1)));

        fork_prepare();
        assert_eq!(blockable(current_mask()), all, "prepare did not block every signal");
        fork_parent();
        assert_eq!(current_mask(), start, "parent handler did not restore the mask");

        fork_prepare();
        assert_eq!(blockable(current_mask()), all);
        fork_child();
        assert_eq!(current_mask(), start, "child handler did not restore the mask");

        unsafe { ::libc::pthread_sigmask(::libc::SIG_SETMASK, &before, core::ptr::null_mut()) };
    }

    /// Arenas and explicit caches against the real allocator (design 13.3):
    /// `MALLOCX_ARENA` allocations come from the named arena and plain
    /// `free` returns them to a cache keyed by that arena, on the freeing
    /// thread whichever thread that is; `MALLOCX_TCACHE_NONE` claims and
    /// releases against the slab bitmaps at once; an explicit cache holds
    /// its own slots until flushed.
    #[test]
    fn arenas_route_mallocx_allocations_and_plain_frees() {
        use crate::api::libc as api_libc;
        use crate::arena::{self, Flags, MALLOCX_TCACHE_NONE, Tcache, mallocx_arena, mallocx_tcache};
        use crate::heap;
        use crate::layout::class_of;
        let s = shared();
        let lua = s.arenas.create(s).expect("arenas.create");
        assert!(s.multi_arena());
        let c = class_of(200).unwrap();
        let size = crate::layout::CLASSES[c].size();
        let f = Flags::decode(mallocx_arena(lua));
        assert_eq!((f.arena, f.tcache), (Some(lua), Tcache::Default));

        // Default cache, explicit arena: through this thread's heap.
        let (p, _) = arena::alloc(s, c, f.arena, f.tcache);
        assert!(!p.is_null());
        unsafe { core::ptr::write_bytes(p, 0xa5, size) };
        assert_eq!(s.classify_free(p), Ok((c, lua)));
        let h = heap::existing_heap(s).unwrap();
        // This thread's automatic arena: whichever `Arenas::assign` gave
        // the test thread (the default is `min(4 * ncpus, 64)` arenas).
        let home = h.arena();
        assert_ne!(home, lua);
        // In the per-CPU mode (design 15) a plain `free` pushes onto the
        // running CPU's cache whatever the slot's arena; the thread's
        // per-arena caches see only explicit-arena traffic.
        let percpu = s.percpu_on();
        let cached_in = |h: &heap::Heap| h.cache_in(lua, c).map_or(0, |cc| cc.len());
        let before = cached_in(h);
        api_libc::free(p);
        if percpu {
            assert_eq!(cached_in(h), before);
        } else {
            assert_eq!(cached_in(h), before + 1);
        }
        // realloc within the class keeps the pointer and the arena.
        let (p, _) = arena::alloc(s, c, Some(lua), Tcache::Default);
        // (Written again: without the per-CPU mode this is the slot freed
        // above, LIFO, and still holds 0xa5; with it that slot went to the
        // CPU cache and this one is fresh.)
        unsafe { core::ptr::write_bytes(p, 0xa5, size) };
        assert_eq!(api_libc::realloc(p, size), p);
        let q = api_libc::realloc(p, size * 4);
        assert_ne!(q, p);
        assert_eq!(unsafe { *q }, 0xa5);
        // Across classes, plain realloc lands in the caller's home arena
        // (in the per-CPU mode: in the running CPU's, an automatic one).
        if percpu {
            assert!((s.classify_free(q).map(|x| x.1).unwrap() as u32) < s.arenas.auto());
        } else {
            assert_eq!(s.classify_free(q).map(|x| x.1), Ok(home));
        }
        api_libc::free(q);

        // Direct path: live count moves at once, no cache involved.
        let f = Flags::decode(mallocx_arena(lua) | MALLOCX_TCACHE_NONE);
        assert_eq!(f.tcache, Tcache::None);
        let (d, _) = arena::alloc(s, c, f.arena, f.tcache);
        let (slab, _) = s.table(c).locate(d);
        let desc = s.table(c).desc(slab);
        assert_eq!(desc.arena(), lua);
        let live = desc.live();
        let cached = cached_in(h);
        arena::free(s, d, Tcache::None);
        assert_eq!(desc.live(), live - 1);
        assert_eq!(cached_in(h), cached);
        // A non-existent arena is null, not an abort.
        assert!(arena::alloc(s, c, Some(4000), Tcache::Default).0.is_null());
        assert!(arena::alloc(s, c, Some(4000), Tcache::None).0.is_null());
        assert!(arena::alloc_huge(s, 1 << 20, 1, Some(4000), Tcache::Default).0.is_null());

        // Huge runs record the arena too.
        let (hp, _) = arena::alloc_huge(s, 3 << 20, 1, Some(lua), Tcache::Default);
        assert!(!hp.is_null());
        let Err(page) = s.classify_free(hp) else { panic!("not huge") };
        assert_eq!(s.huge.lookup(page).unwrap().arena, lua);
        api_libc::free(hp);

        // Explicit cache.
        let t = s.tcaches.create(s, 0).expect("tcache.create");
        let f = Flags::decode(mallocx_tcache(t) | mallocx_arena(lua));
        assert_eq!(f.tcache, Tcache::Explicit(t));
        let (e, _) = arena::alloc(s, c, f.arena, f.tcache);
        assert_eq!(s.classify_free(e), Ok((c, lua)));
        let th = s.tcaches.get(t).unwrap();
        let before = th.cache_in(lua, c).unwrap().len();
        arena::free(s, e, Tcache::Explicit(t));
        assert_eq!(th.cache_in(lua, c).unwrap().len(), before + 1);
        assert!(s.tcaches.flush(s, t));
        assert_eq!(th.cache_in(lua, c).unwrap().len(), 0);
        assert!(s.tcaches.destroy(s, t));

        // Cross-thread: another thread allocates from the Lua arena and its
        // own home arena; this thread frees both with plain `free`, and each
        // lands in the cache of its own arena.
        let (other, ptrs) = thread::spawn(move || {
            let s = shared();
            let mut v = Vec::new();
            for i in 0..64 {
                let a = if i % 2 == 0 { Some(lua) } else { None };
                let (p, _) = arena::alloc(s, c, a, Tcache::Default);
                assert!(!p.is_null());
                unsafe { *p = i as u8 };
                v.push(p as usize);
            }
            (heap::existing_heap(s).unwrap().arena(), v)
        })
        .join()
        .unwrap();
        // The other thread was bound to an idle automatic arena, not ours.
        assert_ne!(other, home);
        assert_ne!(other, lua);
        let h = heap::existing_heap(s).unwrap();
        h.flush_all(s);
        for (i, &p) in ptrs.iter().enumerate() {
            let p = p as *mut u8;
            assert_eq!(unsafe { *p }, i as u8);
            let (class, arena) = s.classify_free(p).unwrap();
            assert_eq!(class, c);
            if i % 2 == 0 {
                assert_eq!(arena, lua);
            } else if !percpu {
                assert_eq!(arena, other);
            }
            // (In the per-CPU mode a plain allocation is whatever the
            // running CPU's cache held, this test's own freed Lua slots
            // included; the arena is not the thread's.)
            api_libc::free(p);
        }
        if percpu {
            assert_eq!(cached_in(h), 0, "plain frees never reach the thread's per-arena caches");
            assert_eq!(h.cache_in(other, c).map_or(0, |cc| cc.len()), 0);
        } else {
            assert_eq!(h.cache_in(lua, c).unwrap().len(), 32);
            assert_eq!(h.cache_in(other, c).unwrap().len(), 32);
        }
        assert!(h.cache(c).is_none_or(|cache| cache.len() == 0), "nothing landed in the home cache");
        h.flush_all(s);
        // Per-arena purge covers the Lua arena's slabs only.
        let lua_arena = s.arena(lua).unwrap();
        let empty_before = lua_arena.classes[c].stats().empty;
        assert!(empty_before >= 1, "the Lua arena's slab should be Empty after the flush");
        let purged = purge::purge_arena(s, lua).unwrap();
        assert!(purged >= empty_before);
        assert_eq!(lua_arena.classes[c].stats().empty, 0);
        assert!(purge::purge_arena(s, 4000).is_none());
    }
}
