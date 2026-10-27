use super::*;
use crate::config::Config;
use crate::layout::class_of;
use crate::sys::pac;
use crate::sys::tls;
use core::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::vec::Vec;

use super::tests::live;
use crate::testutil::{TestShared, shared, xorshift};

fn fill(p: *mut u8, n: usize, v: u8) {
    unsafe { core::ptr::write_bytes(p, v, n) }
}

fn check(p: *mut u8, n: usize, v: u8) {
    let s = unsafe { core::slice::from_raw_parts(p, n) };
    assert!(
        s.iter().all(|&b| b == v),
        "memory at {:#x} is not all {v:#x}",
        p as usize
    );
}

fn is_zero(p: *mut u8, n: usize) -> bool {
    unsafe { core::slice::from_raw_parts(p, n) }
        .iter()
        .all(|&b| b == 0)
}

#[test]
fn heap_is_created_on_first_use_through_tls() {
    let s = shared();
    assert!(s.has_thread_key());
    std::thread::spawn(move || {
        assert!(tls::get().is_null());
        let c = class_of(100).unwrap();
        let p = alloc(s, c);
        assert!(!p.is_null());
        let raw = tls::get() as usize;
        assert!(raw > TLS_EXITED);
        // The slot holds the heap pointer, signed with the thread
        // pointer when PAC is on (`existing_heap` authenticates it).
        let hp = existing_heap(s).expect("heap") as *mut Heap;
        let h = hp as usize;
        if s.pac() {
            assert_eq!(pac::strip(raw), h);
            assert_eq!(pac::auth(raw, tls::get_tp().1), h);
        } else {
            assert_eq!(raw, h);
        }
        let heap = unsafe { &*hp };
        assert_eq!(heap.tid(), crate::sys::gettid() as u32);
        assert!(core::ptr::eq(heap.shared(), s));
        assert_eq!(heap.stats().allocated, CLASSES[c].size() as u64);
        // Same heap on the next call.
        let q = alloc(s, c);
        assert_eq!(tls::get() as usize, raw);
        assert_ne!(p, q);
        fill(p, CLASSES[c].size(), 0xa5);
        fill(q, CLASSES[c].size(), 0x5a);
        check(p, CLASSES[c].size(), 0xa5);
        free(s, p);
        free(s, q);
        assert_eq!(
            unsafe { &*hp }.stats().deallocated,
            2 * CLASSES[c].size() as u64
        );
    })
    .join()
    .unwrap();
}

#[test]
fn calloc_flag_is_honest_about_memory() {
    let s = shared();
    std::thread::spawn(move || {
        let c = class_of(200).unwrap();
        let size = CLASSES[c].size();
        let (p, zero) = alloc_zeroed(s, c);
        assert!(zero && is_zero(p, size));
        fill(p, size, 0xff);
        free(s, p);
        let (q, zero) = alloc_zeroed(s, c);
        assert_eq!(p, q);
        assert!(!zero && !is_zero(q, size));
        // Everything the flag calls zero really is. The flag is
        // conservative: only a refill batch that came entirely from a
        // `Fresh` or `Purged` slab is flagged, so here that is the rest
        // of the very first batch (the first slab's later batches come
        // from an `Active` slab, and the batch that crosses into the
        // next fresh slab is mixed).
        let mut zeros = 0;
        for _ in 0..3000 {
            let (r, zero) = alloc_zeroed(s, c);
            assert!(!r.is_null());
            if zero {
                assert!(
                    is_zero(r, size),
                    "{:#x} flagged zero but is not",
                    r as usize
                );
                zeros += 1;
            }
            fill(r, size, 0x11);
        }
        let refill = default_cap(c) as usize / 2;
        assert!(
            zeros >= refill - 1 && zeros < 3000,
            "{zeros} zero-flagged of 3000"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn thread_exit_flushes_and_frees_the_heap() {
    let s = shared();
    let c = class_of(64).unwrap();
    let meta_before = s.meta.stats().allocated;
    let leaked = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for t in 0..16u8 {
        let leaked = leaked.clone();
        handles.push(std::thread::spawn(move || {
            let mut ps = Vec::new();
            for i in 0..500 {
                let p = alloc(s, c);
                assert!(!p.is_null());
                fill(p, 64, t);
                ps.push(p);
                if i % 3 == 0 {
                    free(s, ps.swap_remove(i / 3 % ps.len()));
                }
            }
            for &p in &ps {
                check(p, 64, t);
            }
            // Exit with `ps` live and a non-empty cache; the live ones
            // stay allocated, the cached ones must be released.
            leaked.fetch_add(ps.len(), Ordering::Relaxed);
            assert!(tls::get() as usize > TLS_EXITED);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(live(s, c), leaked.load(Ordering::Relaxed));
    // Heaps and caches went back to the metadata allocator.
    assert_eq!(s.meta.stats().allocated, meta_before);
    assert_eq!(
        s.arena0.classes[c].desc(0).state(),
        crate::slab::State::Active
    );
}

#[test]
fn creating_and_exited_sentinels_are_served_directly() {
    let s = shared();
    std::thread::spawn(move || {
        let c = class_of(32).unwrap();
        let meta_before = s.meta.stats().allocated;
        // Inside heap creation: no heap, served from the slab bitmap.
        tls::set(TLS_CREATING as *mut c_void);
        let p = alloc(s, c);
        assert!(!p.is_null());
        assert_eq!(tls::get() as usize, TLS_CREATING);
        assert_eq!(s.meta.stats().allocated, meta_before);
        assert_eq!(live(s, c), 1);
        free(s, p);
        assert_eq!(live(s, c), 0);
        // Real heap.
        tls::set(core::ptr::null_mut());
        let p = alloc(s, c);
        let raw = tls::get();
        assert!(raw as usize > TLS_EXITED);
        // The key holds the plain pointer, the slot the signed one.
        let h = existing_heap(s).unwrap() as *mut Heap as *mut c_void;
        if s.pac() {
            assert_eq!(pac::strip(raw as usize), h as usize);
        } else {
            assert_eq!(raw, h);
        }
        assert!(s.meta.stats().allocated > meta_before);
        free(s, p);
        // Run the destructor by hand (glibc clears the key's value
        // before calling it, so do the same to avoid a second run at
        // thread exit): heap gone, slot says exited, and later calls go
        // direct without creating another heap.
        let key = s.thread_key().expect("key");
        assert_eq!(unsafe { libc::pthread_getspecific(key) }, h);
        assert_eq!(
            unsafe { libc::pthread_setspecific(key, core::ptr::null()) },
            0
        );
        unsafe { heap_destructor(h) };
        assert_eq!(tls::get() as usize, TLS_EXITED);
        assert_eq!(s.meta.stats().allocated, meta_before);
        assert_eq!(live(s, c), 0);
        // The slab went `Empty` when everything was released, so its
        // memory is dirty and the flag must say so.
        let (q, zero) = alloc_zeroed(s, c);
        assert!(!q.is_null());
        assert!(!zero || is_zero(q, 32));
        assert_eq!(tls::get() as usize, TLS_EXITED);
        assert_eq!(s.meta.stats().allocated, meta_before);
        free(s, q);
        assert_eq!(live(s, c), 0);
        // The key holds no value now, so thread exit runs no destructor.
    })
    .join()
    .unwrap();
}

/// A thread whose heap the thread-exit key cannot take
/// (`pthread_setspecific` fails: past its first 32 keys glibc allocates
/// a second-level block, and that allocation can fail) is served
/// without a cache: its first call, a `free` here, which has no error to
/// return, releases straight to the slab, the heap goes back to the
/// metadata allocator, and the next call tries again.
#[test]
fn key_registration_failure_is_served_directly() {
    let s = shared();
    let c = class_of(48).unwrap();
    let p = s.alloc_direct(&s.arena0, c).0 as usize;
    assert_ne!(p, 0);
    std::thread::spawn(move || {
        let meta_before = s.meta.stats().allocated;
        let bound_before = s.arenas.bound(0);
        assert!(tls::get().is_null());
        FAIL_SETSPECIFIC.store(crate::sys::gettid() as u32, Ordering::Relaxed);
        free(s, p as *mut u8);
        assert_eq!(FAIL_SETSPECIFIC.load(Ordering::Relaxed), 0, "set never ran");
        assert!(tls::get().is_null(), "no heap, and none half-registered");
        assert_eq!(live(s, c), 0);
        assert_eq!(s.meta.stats().allocated, meta_before);
        assert_eq!(s.arenas.bound(0), bound_before);
        // The next call registers a heap as usual.
        let q = alloc(s, c);
        assert!(!q.is_null());
        assert!(tls::get() as usize > TLS_EXITED);
        let h = existing_heap(s).unwrap() as *mut Heap as *mut c_void;
        let key = s.thread_key().expect("key");
        assert_eq!(unsafe { libc::pthread_getspecific(key) }, h);
        free(s, q);
    })
    .join()
    .unwrap();
}

/// `PUSH_PAC` follows the probe and the key,
/// and the key is off by default (the test builders turn it on).
#[test]
// The constant check pins the opt-in default.
#[allow(clippy::assertions_on_constants)]
fn pac_flag_follows_the_probe_and_the_key() {
    let s = shared();
    assert_eq!(
        s.pac(),
        pac::available(),
        "pac_pointers:true is the probe's answer"
    );
    assert_eq!(s.push_flags() & PUSH_PAC != 0, s.pac());
    assert!(!Config::DEFAULT.pac_pointers, "opt-in");
    let off = TestShared::with(Config {
        narenas: 1,
        ..Config::DEFAULT
    })
    .build();
    assert!(!off.pac(), "the default configuration signs nothing");
    assert_eq!(off.push_flags(), 0);
    #[cfg(target_arch = "x86_64")]
    assert!(!s.pac());
    std::eprintln!(
        "pac: available={} on={} fpac={}",
        pac::available(),
        s.pac(),
        pac::fpac()
    );
}

/// The fallback direction (a `Heap::flush` that ran `pac::mask`'s
/// `xpacd` whatever the probe said died with `SIGILL` on the Altra, a
/// Neoverse N1 without the extension). With `pac_pointers:true` and the
/// probe answering no, a heap's pop, push, refill, flush and the TLS
/// heap pointer execute no PAC instruction at all, counted by
/// `sys::pac::test_ops` in a forked child (the only thread there, so
/// the count is this heap's alone); with the probe's real answer the
/// same work executes some where the extension exists and none where
/// it does not, and the child lives either way. Both directions run
/// on every host: the forced-off one is the Neoverse N1 proof on a
/// CPU that has PAC, the probe one is the real fallback on a CPU that
/// lacks it.
#[test]
fn heap_off_pac_executes_no_pac_instruction() {
    for (forced_off, s) in [
        (true, TestShared::new().pac(false).build()),
        (false, shared()),
    ] {
        assert_eq!(s.pac(), !forced_off && pac::available());
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let before = pac::test_ops();
            let c = class_of(48).unwrap();
            let cap = default_cap(c) as usize;
            let mut v: Vec<*mut u8> = Vec::with_capacity(cap + 8);
            // Two full stacks' worth: refills, a full cache and its
            // flush (the sorted and the unsorted branch), then a GC-free
            // drain through the heap's own flush of every class.
            for _ in 0..cap + 8 {
                v.push(alloc(s, c));
            }
            for &p in v.iter().rev() {
                free(s, p);
            }
            for _ in 0..cap {
                let q = alloc(s, c);
                free(s, q);
            }
            let h = existing_heap(s).expect("a heap") as *mut Heap;
            unsafe { (*h).flush_arena(s, 0, ptr::null_mut()) };
            let ops = pac::test_ops() - before;
            let expect_some = !forced_off && pac::available();
            if (ops == 0) != !expect_some {
                std::eprintln!(
                    "pac: forced_off={forced_off} executed {ops} PAC instructions, expected {}",
                    if expect_some { "some" } else { "none" }
                );
                unsafe { libc::_exit(1) };
            }
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "forced_off={forced_off}: child status {status:#x} (a signal here is the Altra SIGILL)"
        );
        std::eprintln!(
            "pac: forced_off={forced_off} available={} on={}: no unexpected PAC instruction",
            pac::available(),
            s.pac()
        );
    }
}

/// With pointer authentication on, every entry
/// of a cache's stack is stored signed (strips to the slot it names,
/// authenticates against the stack's base) and pops return the plain
/// pointers; the TLS slot holds the heap pointer signed with the thread
/// pointer. Skipped where the CPU has no PAC.
#[test]
fn cache_entries_and_the_tls_heap_pointer_are_signed() {
    let s = shared();
    if !s.pac() {
        std::eprintln!("pac: off on this host, skipping");
        return;
    }
    std::thread::spawn(move || {
        let c = class_of(64).unwrap();
        let ps: Vec<*mut u8> = (0..8).map(|_| alloc(s, c)).collect();
        for &p in &ps {
            free(s, p);
        }
        let hp = existing_heap(s).unwrap() as *mut Heap;
        let k = unsafe { &*hp }.cache(c).unwrap();
        let stored = k.cached();
        assert!(stored.len() >= 8);
        let plain: Vec<*mut u8> = stored.iter().map(|&e| k.plain(e, true)).collect();
        assert_eq!(
            &plain[plain.len() - 8..],
            &ps[..],
            "the last eight pushes, bottom first"
        );
        for (&e, &p) in stored.iter().zip(&plain) {
            assert_eq!(
                pac::strip(e as usize),
                p as usize,
                "entry strips to its slot"
            );
            assert!(p as usize >> 48 == 0, "plain pointers are canonical");
        }
        // Every entry carries a PAC; with a 7-bit PAC the chance that
        // all eight are zero is 2^-56.
        assert!(
            stored[stored.len() - 8..]
                .iter()
                .zip(&plain[plain.len() - 8..])
                .any(|(&e, &p)| e != p)
        );
        // The TLS slot: signed heap pointer, thread pointer as modifier.
        let (raw, tp) = tls::get_tp();
        assert_eq!(pac::strip(raw as usize), hp as usize);
        assert_eq!(pac::auth(raw as usize, tp), hp as usize);
        // Pops authenticate and hand the plain slots back, LIFO.
        for &p in ps.iter().rev() {
            assert_eq!(alloc(s, c), p);
        }
        for &p in &ps {
            free(s, p);
        }
    })
    .join()
    .unwrap();
}

/// The signed pointers' acceptance test: a corrupted cache entry
/// (an address bit flipped by a write into the metadata region) never
/// comes back from `malloc` as a usable pointer, and a corrupted heap
/// pointer in the TLS slot never reaches a heap. In a forked child:
/// on FEAT_FPAC the `autda` faults (`SIGILL`); on the original PAuth
/// the pointer comes out non-canonical and its first dereference
/// faults (`SIGSEGV`). The flipped bit is chosen so the forged
/// address's PAC differs (a 7-bit PAC matches by chance one time in
/// 128). Skipped where the CPU has no PAC.
#[test]
fn corrupted_cache_entry_or_heap_pointer_faults() {
    let s = shared();
    if !s.pac() {
        std::eprintln!("pac: off on this host, skipping");
        return;
    }
    std::thread::spawn(move || {
        let c = class_of(48).unwrap();
        let p = alloc(s, c);
        free(s, p);
        let hp = existing_heap(s).unwrap() as *mut Heap;
        let cache = unsafe { &mut *hp }.cache_ptr(c);
        let (slots, len) = unsafe { ((*cache).slots, (*cache).len as usize) };
        assert!(len >= 1);
        let top = unsafe { *slots.add(len - 1) } as usize;
        assert_eq!(pac::auth(top, slots as usize), p as usize);
        let (raw, tp) = tls::get_tp();
        let raw = raw as usize;
        // A bit whose flip changes the PAC of both pointers.
        let bit = (12..40)
            .map(|b| 1usize << b)
            .find(|&b| {
                pac::sign((p as usize) ^ b, slots as usize) != top ^ b
                    && pac::sign((hp as usize) ^ b, tp) != raw ^ b
            })
            .expect("a bit that changes both PACs");
        for target in ["entry", "tls"] {
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                unsafe {
                    if target == "entry" {
                        let e = slots.add(len - 1);
                        *e = ((*e as usize) ^ bit) as *mut u8;
                    } else {
                        tls::set((raw ^ bit) as *mut c_void);
                    }
                    let q = alloc(s, c);
                    core::ptr::write_volatile(q, 1);
                    libc::_exit(3);
                }
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(
                libc::WIFSIGNALED(status),
                "{target}: the child survived a corrupted pointer: status {status:#x}"
            );
            let sig = libc::WTERMSIG(status);
            if pac::fpac() {
                assert_eq!(sig, libc::SIGILL, "{target}: FEAT_FPAC faults at the autda");
            } else {
                assert_eq!(
                    sig,
                    libc::SIGSEGV,
                    "{target}: the non-canonical pointer faults on use"
                );
            }
            std::eprintln!(
                "pac: corrupted {target} -> signal {sig} (fpac={})",
                pac::fpac()
            );
        }
        // The parent's cache is intact.
        assert_eq!(alloc(s, c), p);
        free(s, p);
    })
    .join()
    .unwrap();
}

#[test]
fn producer_consumer_across_threads_with_patterns() {
    let s = shared();
    let c = class_of(96).unwrap();
    let size = CLASSES[c].size();
    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(64);
    let producer = std::thread::spawn(move || {
        for i in 0..20_000usize {
            let p = alloc(s, c);
            assert!(!p.is_null());
            fill(p, size, (i & 0xff) as u8);
            tx.send(p as usize).unwrap();
        }
    });
    let consumer = std::thread::spawn(move || {
        let mut n = 0usize;
        while let Ok(p) = rx.recv() {
            check(p as *mut u8, size, (n & 0xff) as u8);
            free(s, p as *mut u8);
            n += 1;
        }
        n
    });
    producer.join().unwrap();
    assert_eq!(consumer.join().unwrap(), 20_000);
    assert_eq!(live(s, c), 0);
}

#[test]
fn eight_threads_hammer_disjoint_pointers() {
    let s = shared();
    let classes: Vec<usize> = [16usize, 64, 320, 2048]
        .iter()
        .map(|&n| class_of(n).unwrap())
        .collect();
    let mut handles = Vec::new();
    for t in 0..8u64 {
        let classes = classes.clone();
        handles.push(std::thread::spawn(move || {
            let mut st = 0x1234_5678_9abc_def0u64 ^ (t + 1);
            let mut held: Vec<(*mut u8, usize)> = Vec::new();
            for _ in 0..50_000 {
                xorshift(&mut st);
                if held.len() < 400 && (st & 3 != 0 || held.is_empty()) {
                    let c = classes[(st >> 8) as usize % classes.len()];
                    let p = alloc(s, c);
                    assert!(!p.is_null());
                    let size = CLASSES[c].size();
                    // Fresh from the kernel or zeroed by whoever freed it.
                    check(p, size, 0);
                    fill(p, size, t as u8 + 1);
                    held.push((p, size));
                } else {
                    let i = (st >> 12) as usize % held.len();
                    let (p, size) = held.swap_remove(i);
                    check(p, size, t as u8 + 1);
                    fill(p, size, 0);
                    free(s, p);
                }
            }
            for (p, size) in held {
                check(p, size, t as u8 + 1);
                fill(p, size, 0);
                free(s, p);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    for &c in &classes {
        assert_eq!(live(s, c), 0, "class {c}");
    }
}

const CHANGED_ARENA_ENV: &str = "CHALLOC_HEAP_TEST_CHANGED_ARENA";

/// What the `SIGSEGV` handler of `changed_arena_child` needs: the
/// write-protected range, the table and group whose owner word it
/// rewrites, and whether it has run.
static FLIP_LO: AtomicUsize = AtomicUsize::new(0);
static FLIP_HI: AtomicUsize = AtomicUsize::new(0);
static FLIP_SHARED: AtomicUsize = AtomicUsize::new(0);
static FLIP_CLASS: AtomicUsize = AtomicUsize::new(0);
static FLIP_GROUP: AtomicUsize = AtomicUsize::new(0);
static FLIP_DONE: AtomicUsize = AtomicUsize::new(0);

/// `[base, base + len)`: the pages covering `[lo, hi)`.
fn page_span(lo: usize, hi: usize) -> (usize, usize) {
    let page = crate::testutil::page_size();
    let base = lo & !(page - 1);
    (base, crate::testutil::align_up(hi, page) - base)
}

/// The owner word rewritten between `release_owned`'s load and
/// `arena_of_slot`'s. `release_owned` of arena 1 releases the arena-1
/// slot, stops at the arena-0 slot, and its one write to the
/// `ClassState` after the stop is the `released` add. With the state's
/// pages read-only that add faults; the handler writes arena 1 into the
/// stopped slab's owner word, restores the pages and returns, the add
/// is retried, and `release_routed` then reads arena 1 for the slab
/// arena 1 just refused. Runs only in the fresh process
/// `changed_arena_during_a_flush_aborts` starts, because the handler is
/// process-wide.
#[test]
fn changed_arena_child() {
    if std::env::var(CHANGED_ARENA_ENV).is_err() {
        return;
    }
    extern "C" fn on_segv(_: i32, info: *mut libc::siginfo_t, _: *mut c_void) {
        let addr = unsafe { (*info).si_addr() } as usize;
        let (lo, hi) = (
            FLIP_LO.load(Ordering::Relaxed),
            FLIP_HI.load(Ordering::Relaxed),
        );
        if FLIP_DONE.swap(1, Ordering::Relaxed) != 0 || addr < lo || addr >= hi {
            unsafe { libc::_exit(3) };
        }
        let s = unsafe { &*(FLIP_SHARED.load(Ordering::Relaxed) as *const Shared) };
        let class = FLIP_CLASS.load(Ordering::Relaxed);
        s.table(class)
            .corrupt_owner_word(&s.meta, FLIP_GROUP.load(Ordering::Relaxed), 1);
        let (base, len) = page_span(lo, hi);
        if unsafe { libc::mprotect(base as *mut c_void, len, libc::PROT_READ | libc::PROT_WRITE) }
            != 0
        {
            unsafe { libc::_exit(4) };
        }
    }
    let s = TestShared::with(Config {
        narenas: 1,
        ..Config::DEFAULT
    })
    .build();
    assert_eq!(
        s.arenas.create(s, vstd::prelude::Tracked::assume_new()),
        Some(1)
    );
    let c = class_of(64).unwrap();
    let (a0, a1) = (s.arena(0).unwrap(), s.arena(1).unwrap());
    // Two slots per slab: neither release empties its slab or reopens
    // it, so the only `ClassState` write is the `released` add.
    let (p1, _) = s.alloc_direct(a1, c);
    let (q1, _) = s.alloc_direct(a1, c);
    let (p0, _) = s.alloc_direct(a0, c);
    let (q0, _) = s.alloc_direct(a0, c);
    assert!(!p0.is_null() && !q0.is_null() && !p1.is_null() && !q1.is_null());
    let t = s.table(c);
    let (slab0, _) = t.locate(p0);
    assert_eq!((t.owner(t.locate(p1).0), t.owner(slab0)), (1, 0));
    let cs = a1.class(c) as *const crate::class::ClassState as usize;
    let hi = cs + core::mem::size_of::<crate::class::ClassState>();
    FLIP_LO.store(cs, Ordering::Relaxed);
    FLIP_HI.store(hi, Ordering::Relaxed);
    FLIP_SHARED.store(s as *const Shared as usize, Ordering::Relaxed);
    FLIP_CLASS.store(c, Ordering::Relaxed);
    FLIP_GROUP.store(slab0 / t.group(), Ordering::Relaxed);
    let (base, len) = page_span(cs, hi);
    unsafe {
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = on_segv as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        assert_eq!(libc::sigaction(libc::SIGSEGV, &sa, ptr::null_mut()), 0);
        assert_eq!(libc::mprotect(base as *mut c_void, len, libc::PROT_READ), 0);
    }
    s.release_routed(1, c, &[p1, p0]);
    std::eprintln!(
        "release_routed returned (handler ran: {})",
        FLIP_DONE.load(Ordering::Relaxed)
    );
    unsafe { libc::_exit(5) };
}

/// `Shared::release_routed` aborts when `release_owned` stops at a slab
/// whose owner word, read again, names the arena that just refused it:
/// the word changed under the flush.
#[test]
fn changed_arena_during_a_flush_aborts() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "heap::linux_tests::changed_arena_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHANGED_ARENA_ENV, "1")
        .output()
        .unwrap();
    let err = std::string::String::from_utf8_lossy(&out.stderr);
    let msg = std::format!(
        "heap: slab of class {} changed arena 1 during a flush",
        class_of(64).unwrap()
    );
    assert!(
        !out.status.success() && err.contains(&msg),
        "status {:?}, stderr: {err}",
        out.status
    );
}
