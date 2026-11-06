//! Debug-only run-time check of the TLS slot premise (`cfg(challoc_tls_check)`).
//!
//! `TlsSlot::entry` assumes that a slot value above the sentinels names,
//! through the thread pointer, the heap `create_heap_for_thread` published
//! on this thread, and that the heap is as `Heap::wf` describes it. This
//! module tests that link at every slot-to-heap use (`heap_from_slot`):
//!
//! - **registered and live**: the heap address is one this module saw
//!   published on a thread (`on_publish`) and not yet handed to the
//!   thread-exit destructor (`on_exit`);
//! - **owned by this thread**: the registry records the publishing
//!   thread's kernel thread ID, process ID and `pthread_self`. A use from
//!   another thread fails. In the child of a `fork`, the child handler
//!   (`on_fork_child`) moves the forking thread's record (same
//!   `pthread_self`, which `fork` keeps) to the child's IDs and marks every
//!   other parent thread's heap orphaned; a use before that handler from
//!   the forking thread re-homes the same way;
//! - **where `heap_at`'s facts say**: the heap's `shared` field is the one
//!   it was published with (the process's `Shared` in the library), the
//!   heap lies in that `Shared`'s metadata region, and, at publication,
//!   exit and every 64th use, the part of the test checker
//!   `heap_at_tests::check` that needs no sets, which the two share
//!   (`heap_facts`; `deep`).
//!
//! The registry is a fixed open-addressing table in `.bss` (the check
//! cannot allocate) whose dead entries are recycled, so it bounds the heaps
//! live at once, not the threads a process ever runs. Release builds never set the cfg, so none of this, and
//! none of the hooks that call it, is in the shipped library.

use super::Heap;
use super::heap_facts::{self, in_meta};
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering::*};

/// 4096 entries (192 KiB of `.bss`), so at most 4096 thread heaps live at
/// once: a heap challoc reuses at the same address takes its own entry
/// back, and a new address takes over a dead or orphaned entry (`claim`).
/// The bound is the `.bss` budget: `resource.c`'s `data_threads` mode runs
/// whole processes under `RLIMIT_DATA` of 3 MiB, which a table of 3 MiB
/// exceeds; past the bound the check aborts saying so.
const LG_SLOTS: u32 = 12;
const SLOTS: usize = 1 << LG_SLOTS;
const EMPTY: u8 = 0;
const LIVE: u8 = 1;
const DEAD: u8 = 2;
/// A test heap the unit tests' `free_fast` hands `free_fast_tail` as if
/// the slot held it (`adopt`); a publication may take its entry over.
#[cfg(test)]
const ADOPTED: u8 = 3;
/// A parent thread's heap in the child of a `fork` (`on_fork_child`).
const ORPHANED: u8 = 4;
/// An entry one `claim` caller holds until it stores the next state.
const CLAIMING: u8 = 5;
/// Deep check period, in uses of one heap.
const DEEP_EVERY: u32 = 64;

struct Entry {
    addr: AtomicUsize,
    state: AtomicU8,
    tid: AtomicU32,
    pid: AtomicU32,
    me: AtomicUsize,
    shared: AtomicUsize,
    uses: AtomicU32,
}

#[allow(clippy::declare_interior_mutable_const)]
const ENTRY: Entry = Entry {
    addr: AtomicUsize::new(0),
    state: AtomicU8::new(EMPTY),
    tid: AtomicU32::new(0),
    pid: AtomicU32::new(0),
    me: AtomicUsize::new(0),
    shared: AtomicUsize::new(0),
    uses: AtomicU32::new(0),
};

static TABLE: [Entry; SLOTS] = [ENTRY; SLOTS];

/// Uses checked, publications and exits seen, fork re-homes: read by the
/// tests and the fuzz driver through `challoc_tls_check_stats`.
static USES: AtomicUsize = AtomicUsize::new(0);
static PUBLISHED: AtomicUsize = AtomicUsize::new(0);
static EXITED: AtomicUsize = AtomicUsize::new(0);
static REHOMED: AtomicUsize = AtomicUsize::new(0);
static DEEP: AtomicUsize = AtomicUsize::new(0);

// `hash` is a 64-bit Fibonacci hash; challoc's targets are 64-bit.
const _: () = assert!(usize::BITS == 64);

/// The first probe of `a` in a table of `len` entries, a power of two.
fn hash(a: usize, len: usize) -> usize {
    (a >> 6).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (usize::BITS - len.trailing_zeros())
}

/// The entry of `table` recording `h`, if any. Entries never go back to
/// empty, so a probe sequence stays unbroken when `claim` recycles one.
fn find(table: &[Entry], h: usize) -> Option<&Entry> {
    let mut i = hash(h, table.len());
    for _ in 0..table.len() {
        let e = &table[i];
        let a = e.addr.load(Acquire);
        if a == h {
            return Some(e);
        }
        if a == 0 {
            return None;
        }
        i = (i + 1) & (table.len() - 1);
    }
    None
}

/// The entry of `table` for `h`, `CLAIMING` and so this caller's until it
/// stores a state: `h`'s own entry, else the first entry on its probe
/// sequence that is empty, dead or orphaned, taken over for `h`. `h`'s own
/// entry is returned as it is when it is `LIVE` (a second publication,
/// which the caller reports). `None` when every entry is in use.
///
/// Every taker moves an entry to `CLAIMING` with one compare-exchange
/// from the state it saw, so a thread re-publishing a block at its old
/// address and a thread recycling that address's dead entry for another
/// never both get it; an entry another taker holds is waited for.
fn claim(table: &[Entry], h: usize) -> Option<&Entry> {
    'retry: loop {
        if let Some(e) = find(table, h) {
            let st = e.state.load(Acquire);
            // Recycled for another address after `find` saw it, or held by
            // another taker: look again.
            if e.addr.load(Acquire) != h || st == CLAIMING || st == EMPTY {
                core::hint::spin_loop();
                continue 'retry;
            }
            if st == LIVE {
                return Some(e);
            }
            if e.state
                .compare_exchange(st, CLAIMING, AcqRel, Acquire)
                .is_err()
            {
                continue 'retry;
            }
            if e.addr.load(Acquire) != h {
                e.state.store(st, Release);
                continue 'retry;
            }
            return Some(e);
        }
        let mut i = hash(h, table.len());
        for _ in 0..table.len() {
            let e = &table[i];
            if e.addr.compare_exchange(0, h, AcqRel, Acquire).is_ok() {
                e.state.store(CLAIMING, Release);
                return Some(e);
            }
            let st = e.state.load(Acquire);
            if (st == DEAD || st == ORPHANED)
                && e.state
                    .compare_exchange(st, CLAIMING, AcqRel, Acquire)
                    .is_ok()
            {
                e.addr.store(h, Release);
                return Some(e);
            }
            i = (i + 1) & (table.len() - 1);
        }
        return None;
    }
}

/// The entry of `table` (the registry, [`TABLE`]) for `h`, claimed if
/// need be.
fn find_or_claim(table: &[Entry], h: usize) -> &Entry {
    match claim(table, h) {
        Some(e) => e,
        None => crate::abort!(
            "tls check: heap {:#x} on thread {}: the registry is full: {} thread heaps are live at once (LG_SLOTS in heap/tlscheck.rs)",
            h,
            crate::sys::gettid(),
            table.len()
        ),
    }
}

#[cold]
#[inline(never)]
fn fail(h: usize, what: &str) -> ! {
    crate::abort!(
        "tls check: heap {:#x} on thread {}: {}",
        h,
        crate::sys::gettid(),
        what
    )
}

#[cfg(not(test))]
fn process_shared() -> Option<usize> {
    Some(crate::api::shared_ptr() as usize)
}

// The unit tests run thread heaps on many test `Shared`s at once
// (`TestShared`), none of them the process's, so there is no one `Shared`
// to compare with: the unit tests check only that the field is the one
// published and that the heap lies in that `Shared`'s metadata, but where
// a test names one for its own thread (`tests::PROCESS_SHARED`). The
// `api::` tests run the C programs on the library built without
// `cfg(test)`, where the comparison is made.
#[cfg(test)]
fn process_shared() -> Option<usize> {
    tests::PROCESS_SHARED.with(|c| c.get())
}

/// The heap `h` just published in this thread's slot.
pub(crate) fn on_publish(h: *mut Heap) {
    let a = h as usize;
    let e = find_or_claim(&TABLE, a);
    // An adopted test heap's entry (`adopt`) is not a live thread heap.
    if e.state.load(Acquire) == LIVE {
        fail(
            a,
            "published while the registry has it live (two threads' heap, or reused before its exit)",
        );
    }
    // SAFETY: just created by this thread, which alone uses it.
    let heap = unsafe { &*h };
    e.tid.store(crate::sys::gettid() as u32, Relaxed);
    e.pid.store(crate::sys::getpid() as u32, Relaxed);
    e.me.store(crate::sys::pthread_self(), Relaxed);
    e.shared.store(heap.shared as usize, Relaxed);
    e.uses.store(0, Relaxed);
    e.state.store(LIVE, Release);
    PUBLISHED.fetch_add(1, Relaxed);
    if let Some(s) = process_shared()
        && heap.shared as usize != s
    {
        fail(
            a,
            "published with a shared field that is not the process's Shared",
        );
    }
    deep(a, heap, "at publication");
}

/// The owner and liveness test of [`on_use`] and [`on_exit`].
fn owned(a: usize, what: &str) -> &'static Entry {
    let Some(e) = find(&TABLE, a) else {
        fail(
            a,
            match what {
                "use" => "the slot names a heap no thread published",
                _ => "the exit key names a heap no thread published",
            },
        )
    };
    owns(e, a);
    e
}

/// The rest of [`owned`]'s test, of `e`, the entry `find` found for `a`:
/// live, still `a`'s, and this thread's (re-homed in a `fork` child).
fn owns(e: &Entry, a: usize) {
    match e.state.load(Acquire) {
        LIVE => {}
        #[cfg(test)]
        ADOPTED => {}
        DEAD => fail(a, "the slot names a heap its thread's exit has destroyed"),
        ORPHANED => fail(
            a,
            "the slot names a heap of a parent thread the fork did not copy",
        ),
        _ => fail(a, "the slot names an unregistered heap"),
    }
    // A dead or orphaned entry `claim` handed to another address after
    // `find` found it.
    if e.addr.load(Acquire) != a {
        fail(a, "the slot names a destroyed or orphaned heap");
    }
    let tid = crate::sys::gettid() as u32;
    if e.tid.load(Relaxed) != tid {
        let pid = crate::sys::getpid() as u32;
        if e.pid.load(Relaxed) != pid && e.me.load(Relaxed) == crate::sys::pthread_self() {
            // The forking thread in the child of a `fork`: `pthread_self`
            // and the TLS block are the parent thread's, the IDs new.
            e.tid.store(tid, Relaxed);
            e.pid.store(pid, Relaxed);
            REHOMED.fetch_add(1, Relaxed);
        } else {
            crate::abort!(
                "tls check: heap {:#x} on thread {} of process {} (pthread {:#x}): the slot names another thread's heap: published by thread {} of process {} (pthread {:#x}), state {}",
                a,
                tid,
                pid,
                crate::sys::pthread_self(),
                e.tid.load(Relaxed),
                e.pid.load(Relaxed),
                e.me.load(Relaxed),
                e.state.load(Relaxed)
            );
        }
    }
}

/// A slot-to-heap use: `h` is what `heap_from_slot` is about to borrow.
pub(crate) fn on_use(h: usize) {
    let e = owned(h, "use");
    USES.fetch_add(1, Relaxed);
    // SAFETY: registered live and owned by this thread (checked above), so
    // the block is a heap this thread alone uses.
    let heap = unsafe { &*(h as *const Heap) };
    if heap.shared as usize != e.shared.load(Relaxed) {
        fail(h, "the heap's shared field changed since publication");
    }
    // SAFETY: as above; the shared field is the one published, a live `Shared`.
    let s = unsafe { &*heap.shared };
    if !in_meta(s, h, core::mem::size_of::<Heap>()) || !h.is_multiple_of(64) {
        fail(h, "the heap is not 64-aligned metadata");
    }
    if e.uses.fetch_add(1, Relaxed) % DEEP_EVERY == DEEP_EVERY - 1 {
        deep(h, heap, "at a use");
    }
}

/// The thread-exit destructor is about to destroy `h`.
pub(crate) fn on_exit(h: *mut Heap) {
    let a = h as usize;
    let e = owned(a, "exit");
    // SAFETY: live and this thread's (checked above).
    deep(a, unsafe { &*h }, "at thread exit");
    e.state.store(DEAD, Release);
    EXITED.fetch_add(1, Relaxed);
}

/// The child's `fork` handler: only the forking thread runs here, so every
/// entry live in the parent process is either that thread's heap (same
/// `pthread_self`), re-homed now, or a heap of a thread the fork did not
/// copy, which no child thread may reach again (a child thread that gets a
/// dead parent thread's TCB, and so its `pthread_self`, has a fresh slot).
/// An entry a parent thread was taking (`claim`) when the process forked
/// is orphaned too, so `claim` can recycle it in the child.
pub(crate) fn on_fork_child() {
    let pid = crate::sys::getpid() as u32;
    let tid = crate::sys::gettid() as u32;
    let me = crate::sys::pthread_self();
    for e in &TABLE {
        let st = e.state.load(Acquire);
        if st == CLAIMING || (st == EMPTY && e.addr.load(Acquire) != 0) {
            e.state.store(ORPHANED, Release);
            continue;
        }
        if st != LIVE || e.pid.load(Relaxed) == pid {
            continue;
        }
        if e.me.load(Relaxed) == me {
            e.tid.store(tid, Relaxed);
            e.pid.store(pid, Relaxed);
            REHOMED.fetch_add(1, Relaxed);
        } else {
            e.state.store(ORPHANED, Release);
        }
    }
}

/// The unit tests' `free_fast` runs `free_fast_tail` with a test heap as
/// the slot value, a heap no thread published: record it as this thread's
/// (`ADOPTED`, which `claim` recycles only for the same address). A heap
/// the registry has live is left alone.
#[cfg(test)]
pub(crate) fn adopt(h: *mut Heap) {
    let a = h as usize;
    let e = find_or_claim(&TABLE, a);
    if e.state.load(Acquire) == LIVE {
        return;
    }
    // SAFETY: the caller's live test heap.
    e.shared.store(unsafe { (*h).shared } as usize, Relaxed);
    e.tid.store(crate::sys::gettid() as u32, Relaxed);
    e.pid.store(crate::sys::getpid() as u32, Relaxed);
    e.me.store(crate::sys::pthread_self(), Relaxed);
    e.state.store(ADOPTED, Release);
}

/// `heap_facts::no_sets` of `heap`, the heap at `h`, on its own `Shared`:
/// the conjuncts of `Heap::wf` and `Heap::home_at` that
/// `heap_at_tests::check` tests and that need no allocation, through the
/// same code, so the checker's pin to `Heap::wf` and its mutants
/// (`checker_fails_each_corrupted_fact`, which runs `no_sets` on each
/// mutant but the set-based ones) cover them. The `others` walk ends on a cycle at a
/// repeated key. Not tested here: the set-based ones (two nodes sharing a
/// header, stacks overlapping, a slot cached twice) and the zero prefix.
fn deep(h: usize, heap: &Heap, when: &str) {
    DEEP.fetch_add(1, Relaxed);
    #[cfg(test)]
    tests::DEEP_HERE.with(|c| c.set(c.get() + 1));
    // SAFETY: the published shared field, a live `Shared`.
    let s = unsafe { &*heap.shared };
    // SAFETY: this thread's live heap (the caller's), which nothing else
    // uses while the check runs.
    if let Err(b) = unsafe { heap_facts::no_sets(heap, h as *const Heap, s) } {
        crate::abort!(
            "tls check: heap {:#x} on thread {} {}: heap_at fact broken: {:?}",
            h,
            crate::sys::gettid(),
            when,
            b
        );
    }
}

/// The counters, for the C tests: uses, publications, exits, re-homes,
/// deep checks. Exported only in check builds.
#[unsafe(no_mangle)]
pub extern "C" fn challoc_tls_check_stats(out: *mut usize) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller passes five writable words.
    unsafe {
        *out = USES.load(Relaxed);
        *out.add(1) = PUBLISHED.load(Relaxed);
        *out.add(2) = EXITED.load(Relaxed);
        *out.add(3) = REHOMED.load(Relaxed);
        *out.add(4) = DEEP.load(Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::Shared;
    use crate::layout::class_of;
    use crate::sys::tls;
    use crate::testutil::{TestShared, in_fork_child};
    use core::cell::Cell;
    use core::ffi::c_void;
    use std::string::String;
    use vstd::prelude::Tracked;

    std::thread_local! {
        /// The `Shared` `on_publish` holds this thread's heaps to
        /// (`process_shared`): none unless a test sets one.
        pub(super) static PROCESS_SHARED: Cell<Option<usize>> = const { Cell::new(None) };
        /// The `deep` checks this thread ran.
        pub(super) static DEEP_HERE: Cell<usize> = const { Cell::new(0) };
    }

    fn s() -> &'static Shared {
        TestShared::new().pac(false).build()
    }

    /// Allocate and free once, so the thread has a published heap; its
    /// slot value.
    fn warm(s: &Shared) -> usize {
        let c = class_of(64).unwrap();
        let p = super::super::alloc(s, c, Tracked::assume_new(), Tracked::assume_new());
        super::super::free(s, p, Tracked::assume_new(), Tracked::assume_new());
        let v = tls::get(Tracked::assume_new()) as usize;
        assert!(
            v > super::super::TLS_EXITED,
            "the slot holds a heap: {v:#x}"
        );
        v
    }

    /// Run `existing_heap` with the slot set to `v`, put `back` back, and
    /// return the check's message, or `None` when it passed.
    fn use_with(s: &'static Shared, v: usize, back: usize) -> Option<String> {
        tls::set(v as *mut c_void, Tracked::assume_new());
        let m = caught(|| {
            let _ = super::super::existing_heap(s);
        });
        tls::set(back as *mut c_void, Tracked::assume_new());
        m
    }

    /// Run `f`; the check's message if it failed, else `None`.
    fn caught(f: impl FnOnce()) -> Option<String> {
        std::panic::catch_unwind(core::panic::AssertUnwindSafe(f))
            .err()
            .map(|e| {
                e.downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| String::from("?"))
            })
    }

    /// A copy of this thread's heap `v` in a 64-aligned block of the
    /// test's own, outside every metadata region, and the block's layout.
    fn copy_outside_meta(v: usize) -> (*mut Heap, std::alloc::Layout) {
        let l = std::alloc::Layout::from_size_align(core::mem::size_of::<Heap>(), 64).unwrap();
        // SAFETY: `l` has a nonzero size.
        let p = unsafe { std::alloc::alloc(l) }.cast::<Heap>();
        assert!(!p.is_null());
        // SAFETY: `v` is this thread's live heap, and `p` a fresh block of
        // its size.
        unsafe { core::ptr::copy_nonoverlapping(v as *const u8, p.cast::<u8>(), l.size()) };
        (p, l)
    }

    /// Free a [`copy_outside_meta`] block after retiring its registry
    /// entry.
    fn drop_copy(p: *mut Heap, l: std::alloc::Layout) {
        find(&TABLE, p as usize)
            .expect("the copy is registered")
            .state
            .store(DEAD, Release);
        // SAFETY: allocated by `copy_outside_meta` with `l`.
        unsafe { std::alloc::dealloc(p.cast::<u8>(), l) };
    }

    #[test]
    fn own_heap_passes() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let e = find(&TABLE, v).expect("the heap is registered");
            let u0 = e.uses.load(Relaxed);
            let d0 = DEEP_HERE.with(Cell::get);
            for _ in 0..200 {
                assert_eq!(use_with(s, v, v), None);
            }
            // `on_use` runs `deep` at every 64th use of a heap: 200 uses
            // of this one, on this thread, ran it at least three times.
            assert!(e.uses.load(Relaxed) >= u0 + 200, "200 uses counted");
            let d = DEEP_HERE.with(Cell::get) - d0;
            assert!(d >= 3, "deep checks: got = {d}, want >= 3");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn corrupted_slot_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            for bad in [v + 64, v ^ (1 << 20), v - 4096, 0x1000] {
                let m = use_with(s, bad, v).expect("a corrupted slot passed the check");
                assert!(m.contains("no thread published"), "{bad:#x}: {m}");
            }
        })
        .join()
        .unwrap();
    }

    /// A use of a heap whose `shared` field is no longer the one it was
    /// published with fails; the restored field passes.
    #[test]
    fn changed_shared_field_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let h = v as *mut Heap;
            // SAFETY: this thread's heap, which nothing else uses while the
            // test changes and restores its field.
            let saved = unsafe { (*h).shared };
            // SAFETY: as above.
            unsafe { (*h).shared = saved.wrapping_byte_add(64) };
            let m = use_with(s, v, v);
            // SAFETY: as above.
            unsafe { (*h).shared = saved };
            let m = m.expect("a changed shared field passed the check");
            assert!(m.contains("shared field changed"), "{m}");
            assert_eq!(use_with(s, v, v), None);
        })
        .join()
        .unwrap();
    }

    /// A slot naming a registered heap, with its published `shared`
    /// field, in a block outside that `Shared`'s metadata fails.
    #[test]
    fn heap_outside_metadata_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let (p, l) = copy_outside_meta(v);
            adopt(p);
            let m =
                use_with(s, p as usize, v).expect("a heap outside the metadata passed the check");
            assert!(m.contains("not 64-aligned metadata"), "{m}");
            drop_copy(p, l);
        })
        .join()
        .unwrap();
    }

    /// An entry for `a` in `state`, published by this thread, outside the
    /// registry (a recycled or orphaned entry of `TABLE` could be claimed
    /// by a parallel test).
    fn entry_of_this_thread(a: usize, state: u8) -> Entry {
        let e = ENTRY;
        e.addr.store(a, Relaxed);
        e.state.store(state, Relaxed);
        e.tid.store(crate::sys::gettid() as u32, Relaxed);
        e.pid.store(crate::sys::getpid() as u32, Relaxed);
        e.me.store(crate::sys::pthread_self(), Relaxed);
        e
    }

    /// An orphaned entry (as `on_fork_child` leaves a parent thread's heap
    /// the fork did not copy) fails the owner test, which a live one of
    /// the same thread passes.
    #[test]
    fn orphaned_entry_is_caught() {
        let a = 0x7f00_0000_0040;
        let e = entry_of_this_thread(a, ORPHANED);
        let m = caught(|| owns(&e, a)).expect("an orphaned entry passed the check");
        assert!(m.contains("a parent thread the fork did not copy"), "{m}");
        e.state.store(LIVE, Relaxed);
        assert_eq!(caught(|| owns(&e, a)), None);
    }

    /// An entry `find` found for an address that `claim` has since handed
    /// to another one fails, though it is live and this thread's.
    #[test]
    fn recycled_entry_is_caught() {
        let a = 0x7f00_0000_0040;
        let e = entry_of_this_thread(a + 64, LIVE);
        let m = caught(|| owns(&e, a)).expect("a recycled entry passed the check");
        assert!(m.contains("a destroyed or orphaned heap"), "{m}");
        e.addr.store(a, Relaxed);
        assert_eq!(caught(|| owns(&e, a)), None);
    }

    /// A new address on a registry whose every entry is live fails,
    /// naming the bound.
    #[test]
    fn full_registry_is_caught() {
        let t = [ENTRY; 64];
        for i in 0..64 {
            let e = claim(&t, 0x7f00_0000_0000 + 4096 * i).expect("a free entry");
            e.state.store(LIVE, Relaxed);
        }
        let m = caught(|| {
            find_or_claim(&t, 0x7e00_0000_0000);
        })
        .expect("a full registry took a new address");
        assert!(m.contains("the registry is full: 64 thread heaps"), "{m}");
    }

    /// A second publication of a heap the registry has live fails.
    #[test]
    fn second_publication_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let m = caught(|| on_publish(v as *mut Heap))
                .expect("a second publication passed the check");
            assert!(
                m.contains("published while the registry has it live"),
                "{m}"
            );
            assert_eq!(use_with(s, v, v), None);
        })
        .join()
        .unwrap();
    }

    /// With a process `Shared` set for the thread (as the library build
    /// always has), a publication on that `Shared` passes and one on
    /// another fails. The second publishes a copy of the heap, so the
    /// thread's own entry stays live.
    #[test]
    fn publication_on_another_shared_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let mine = s as *const Shared as usize;
            PROCESS_SHARED.with(|c| c.set(Some(mine)));
            let v = warm(s);
            let (p, l) = copy_outside_meta(v);
            PROCESS_SHARED.with(|c| c.set(Some(mine + 64)));
            let m = caught(|| on_publish(p));
            PROCESS_SHARED.with(|c| c.set(None));
            let m = m.expect("a publication on another Shared passed the check");
            assert!(m.contains("not the process's Shared"), "{m}");
            drop_copy(p, l);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn another_threads_heap_is_caught() {
        let s = s();
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let a = std::thread::spawn(move || {
            tx.send(warm(s)).unwrap();
            done_rx.recv().unwrap();
        });
        let other = rx.recv().unwrap();
        std::thread::spawn(move || {
            let mine = warm(s);
            assert_ne!(mine, other);
            let m = use_with(s, other, mine).expect("another thread's heap passed the check");
            assert!(m.contains("another thread's heap"), "{m}");
        })
        .join()
        .unwrap();
        done_tx.send(()).unwrap();
        a.join().unwrap();
    }

    #[test]
    fn destroyed_heap_is_caught() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let key = s.thread_key().unwrap();
            // SAFETY: the allocator's live thread key; cleared as glibc
            // clears it before the destructor.
            let kv = unsafe { libc::pthread_getspecific(key) };
            assert_eq!(kv as usize, v);
            assert_eq!(
                unsafe { libc::pthread_setspecific(key, core::ptr::null()) },
                0
            );
            // SAFETY: this thread's heap, which neither the key nor the
            // slot hands out again but through the test below.
            unsafe { super::super::heap_destructor(kv) };
            let m = use_with(s, v, super::super::TLS_EXITED)
                .expect("a destroyed heap passed the check");
            // A parallel test may have recycled the dead entry already.
            assert!(
                m.contains("destroyed") || m.contains("no thread published"),
                "{m}"
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn fork_child_re_homes_the_forking_threads_heap() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            // Read before the fork: when the process's atfork handlers are
            // registered, the child handler re-homes before the first use.
            let r0 = REHOMED.load(Relaxed);
            in_fork_child("tlscheck fork", 10_000, move || {
                assert_eq!(use_with(s, v, v), None);
                let r1 = REHOMED.load(Relaxed);
                assert!(r1 > r0, "the child re-homed the heap");
                assert_eq!(use_with(s, v, v), None);
                assert_eq!(REHOMED.load(Relaxed), r1, "re-homed once");
                warm(s);
            });
        })
        .join()
        .unwrap();
    }

    /// In a fork child, after the child handler, a parent thread's heap
    /// the fork did not copy is orphaned (the state `owned` fails on), and
    /// the forking thread's own heap still passes. The child checks the
    /// entry rather than fail a use: that panic would run the panic hook,
    /// whose locks another test thread may have held at the `fork`.
    #[test]
    fn fork_child_orphans_the_other_threads_heaps() {
        let s = s();
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let a = std::thread::spawn(move || {
            tx.send(warm(s)).unwrap();
            done_rx.recv().unwrap();
        });
        let other = rx.recv().unwrap();
        std::thread::spawn(move || {
            let mine = warm(s);
            in_fork_child("tlscheck orphan", 10_000, move || {
                on_fork_child();
                assert_eq!(use_with(s, mine, mine), None);
                let e = find(&TABLE, other).expect("the other thread's heap is registered");
                assert_eq!(e.state.load(Relaxed), ORPHANED);
            });
        })
        .join()
        .unwrap();
        done_tx.send(()).unwrap();
        a.join().unwrap();
    }

    /// A full table of live entries refuses a new address; a dead and an
    /// orphaned entry are taken over, and every other address is still
    /// found along its probe sequence after the takeovers.
    #[test]
    fn claim_recycles_dead_and_orphaned_entries() {
        let t = [ENTRY; 64];
        let addrs: std::vec::Vec<usize> = (1..=64).map(|i| 0x7f00_0000_0000 + 4096 * i).collect();
        for &a in &addrs {
            let e = claim(&t, a).expect("a free entry");
            assert_eq!(e.addr.load(Relaxed), a);
            e.state.store(LIVE, Relaxed);
        }
        for &a in &addrs {
            let e = claim(&t, a).expect("its own entry");
            assert!(core::ptr::eq(e, find(&t, a).unwrap()));
        }
        let new = 0x7e00_0000_0000;
        assert!(claim(&t, new).is_none(), "a full table took a new address");
        find(&t, addrs[7]).unwrap().state.store(DEAD, Relaxed);
        find(&t, addrs[40]).unwrap().state.store(ORPHANED, Relaxed);
        let e = claim(&t, new).expect("a dead or orphaned entry");
        assert_eq!(e.state.load(Relaxed), CLAIMING);
        e.state.store(LIVE, Relaxed);
        let e2 = claim(&t, new + 4096).expect("the other recycled entry");
        assert_eq!(e2.state.load(Relaxed), CLAIMING);
        assert!(find(&t, addrs[7]).is_none() && find(&t, addrs[40]).is_none());
        for (i, &a) in addrs.iter().enumerate() {
            if i != 7 && i != 40 {
                assert_eq!(find(&t, a).unwrap().addr.load(Relaxed), a);
            }
        }
        assert!(core::ptr::eq(find(&t, new).unwrap(), e));
        assert!(core::ptr::eq(find(&t, new + 4096).unwrap(), e2));
    }

    /// Threads publish, use and retire addresses of their own on a small
    /// table, so `claim` recycles entries while other threads take their
    /// own addresses back: no taker gets an entry another one holds, and
    /// every live address is found with its owner.
    #[test]
    fn claim_is_exclusive_under_recycling() {
        static T: [Entry; 64] = [ENTRY; 64];
        let hs: std::vec::Vec<_> = (0..8u32)
            .map(|me| {
                std::thread::spawn(move || {
                    let mut x = u64::from(me) * 0x9e37_79b9 + 1;
                    for _ in 0..20_000 {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        let a = 0x7f00_0000_0000 + 64 * (me as usize * 4 + (x % 4) as usize);
                        let e = claim(&T, a).expect("a free entry");
                        assert_eq!(e.state.load(Acquire), CLAIMING, "{a:#x}");
                        e.tid.store(me, Relaxed);
                        e.state.store(LIVE, Release);
                        for _ in 0..4 {
                            let f = find(&T, a).expect("a live address is found");
                            assert!(core::ptr::eq(e, f) && f.tid.load(Relaxed) == me);
                            assert_eq!(f.state.load(Acquire), LIVE);
                        }
                        e.state.store(DEAD, Release);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
    }

    /// `deep` aborts, naming the `heap_facts::Broken` variant, on a broken
    /// `CacheHeader::inv` (`heap_facts::set_free`) and on `rehome` without
    /// `bound` (`heap_facts::homed_ok`); the heap is restored after each.
    #[test]
    fn deep_catches_a_broken_heap() {
        let s = s();
        std::thread::spawn(move || {
            let v = warm(s);
            let h = v as *mut Heap;
            let run = move |what: &str| {
                // SAFETY: this thread's live heap, read only by the check.
                let m = caught(|| deep(v, unsafe { &*h }, "in a test"))
                    .expect("a broken heap passed the deep check");
                assert!(m.contains(what), "{m}");
            };
            // SAFETY: this thread's heap, which nothing else uses while the
            // test breaks and restores one field at a time.
            unsafe {
                let k = &mut (*h).caches[0];
                let saved = k.run_end;
                k.run_end = k.low_water + 1;
                run("HeaderOrder");
                (*h).caches[0].run_end = saved;
                let (rehome, bound) = ((*h).rehome, (*h).bound);
                (*h).rehome = true;
                (*h).bound = false;
                run("RehomeUnbound");
                (*h).rehome = rehome;
                (*h).bound = bound;
            }
            deep(v, unsafe { &*h }, "after the restore");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn many_short_lived_threads_pass() {
        let s = s();
        let e0 = EXITED.load(Relaxed);
        for _ in 0..50 {
            let hs: std::vec::Vec<_> = (0..40)
                .map(|_| {
                    std::thread::spawn(move || {
                        warm(s);
                    })
                })
                .collect();
            for h in hs {
                h.join().unwrap();
            }
        }
        assert!(EXITED.load(Relaxed) - e0 >= 2000);
    }

    /// A corrupted signed slot on a PAC host: with FEAT_FPAC the `autda`
    /// in `heap_from_slot` faults (`SIGILL`) before the check runs; on the
    /// original PAuth the pointer comes out poisoned and the check catches
    /// it (`SIGABRT`: a real abort, since a panic in a fork child can hang
    /// on a lock of the panic hook). Skipped where the CPU has no PAC.
    #[test]
    fn corrupted_signed_slot_faults_or_is_caught() {
        let s = crate::testutil::shared();
        if !s.pac() {
            std::eprintln!("tlscheck pac: off on this host, skipping");
            return;
        }
        std::thread::spawn(move || {
            let v = warm(s);
            let (raw, tp) = tls::get_tp(Tracked::assume_new());
            assert_eq!(raw as usize, v);
            let hp = crate::sys::pac::auth(v, tp);
            let bit = (12..40)
                .map(|b| 1usize << b)
                .find(|&b| crate::sys::pac::sign(hp ^ b, tp) != v ^ b)
                .expect("a bit that changes the PAC");
            // SAFETY: the child only corrupts its own slot, reads it and exits.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                tls::set((v ^ bit) as *mut c_void, Tracked::assume_new());
                crate::sys::test_real_aborts();
                let _ = super::super::existing_heap(s);
                // SAFETY: `_exit` has no preconditions.
                unsafe { libc::_exit(0) };
            }
            let mut st = 0;
            // SAFETY: our child; `st` is a local.
            assert_eq!(unsafe { libc::waitpid(pid, &mut st, 0) }, pid);
            if crate::sys::pac::fpac() {
                assert!(
                    libc::WIFSIGNALED(st) && libc::WTERMSIG(st) == libc::SIGILL,
                    "status {st:#x}"
                );
            } else {
                assert!(
                    (libc::WIFSIGNALED(st) && libc::WTERMSIG(st) == libc::SIGABRT)
                        || (libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 134),
                    "status {st:#x}"
                );
            }
            std::eprintln!(
                "tlscheck pac: corrupted slot -> status {st:#x} (fpac={})",
                crate::sys::pac::fpac()
            );
        })
        .join()
        .unwrap();
    }
}
