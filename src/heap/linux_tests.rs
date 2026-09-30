use super::*;
use crate::config::Config;
use crate::layout::class_of;
use crate::tls;
use core::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::vec::Vec;

use super::tests::{TestHeap, live, shared_build, shared_percpu, shared_with_guards_mode, xorshift};
use std::collections::HashSet;

/// One automatic arena (the tests below reason about `arena0`), no
/// guard slabs, the thread-cache mode, the PAC probe's real answer.
fn shared() -> &'static Shared {
    shared_pac(pac::available())
}

/// `shared()` with the PAC probe's answer given by the caller
/// (`shared_build`'s `pac`).
fn shared_pac(pac: bool) -> &'static Shared {
    shared_build(Config { narenas: 1, pac_pointers: true, ..Config::DEFAULT }, false, false, pac)
}

fn fill(p: *mut u8, n: usize, v: u8) {
    unsafe { core::ptr::write_bytes(p, v, n) }
}

fn check(p: *mut u8, n: usize, v: u8) {
    let s = unsafe { core::slice::from_raw_parts(p, n) };
    assert!(s.iter().all(|&b| b == v), "memory at {:#x} is not all {v:#x}", p as usize);
}

fn is_zero(p: *mut u8, n: usize) -> bool {
    unsafe { core::slice::from_raw_parts(p, n) }.iter().all(|&b| b == 0)
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
        assert_eq!(unsafe { &*hp }.stats().deallocated, 2 * CLASSES[c].size() as u64);
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
                assert!(is_zero(r, size), "{:#x} flagged zero but is not", r as usize);
                zeros += 1;
            }
            fill(r, size, 0x11);
        }
        let refill = default_cap(c) as usize / 2;
        assert!(zeros >= refill - 1 && zeros < 3000, "{zeros} zero-flagged of 3000");
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
    assert_eq!(s.arena0.classes[c].desc(0).state(), crate::slab::State::Active);
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
        assert_eq!(unsafe { libc::pthread_setspecific(key, core::ptr::null()) }, 0);
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

/// `PUSH_PAC` follows the probe and the key,
/// and the key is off by default (the test builders turn it on).
#[test]
fn pac_flag_follows_the_probe_and_the_key() {
    let s = shared();
    assert_eq!(s.pac(), pac::available(), "pac_pointers:true is the probe's answer");
    assert_eq!(s.push_flags() & PUSH_PAC != 0, s.pac());
    assert!(!Config::DEFAULT.pac_pointers, "opt-in");
    let off = crate::heap::tests::shared_with(Config { narenas: 1, ..Config::DEFAULT });
    assert!(!off.pac(), "the default configuration signs nothing");
    assert_eq!(off.push_flags(), 0);
    #[cfg(target_arch = "x86_64")]
    assert!(!s.pac());
    std::eprintln!("pac: available={} on={} fpac={}", pac::available(), s.pac(), pac::fpac());
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
    for (forced_off, s) in [(true, shared_pac(false)), (false, shared())] {
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
                std::eprintln!("pac: forced_off={forced_off} executed {ops} PAC instructions, expected {}", if expect_some { "some" } else { "none" });
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
        std::eprintln!("pac: forced_off={forced_off} available={} on={}: no unexpected PAC instruction", pac::available(), s.pac());
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
        assert_eq!(&plain[plain.len() - 8..], &ps[..], "the last eight pushes, bottom first");
        for (&e, &p) in stored.iter().zip(&plain) {
            assert_eq!(pac::strip(e as usize), p as usize, "entry strips to its slot");
            assert!(p as usize >> 48 == 0, "plain pointers are canonical");
        }
        // Every entry carries a PAC; with a 7-bit PAC the chance that
        // all eight are zero is 2^-56.
        assert!(stored[stored.len() - 8..].iter().zip(&plain[plain.len() - 8..]).any(|(&e, &p)| e != p));
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
                pac::sign((p as usize) ^ b, slots as usize) != top ^ b && pac::sign((hp as usize) ^ b, tp) != raw ^ b
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
            assert!(libc::WIFSIGNALED(status), "{target}: the child survived a corrupted pointer: status {status:#x}");
            let sig = libc::WTERMSIG(status);
            if pac::fpac() {
                assert_eq!(sig, libc::SIGILL, "{target}: FEAT_FPAC faults at the autda");
            } else {
                assert_eq!(sig, libc::SIGSEGV, "{target}: the non-canonical pointer faults on use");
            }
            std::eprintln!("pac: corrupted {target} -> signal {sig} (fpac={})", pac::fpac());
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
    let classes: Vec<usize> = [16usize, 64, 320, 2048].iter().map(|&n| class_of(n).unwrap()).collect();
    let mut handles = Vec::new();
    for t in 0..8u64 {
        let classes = classes.clone();
        handles.push(std::thread::spawn(move || {
            let mut st = 0x1234_5678_9abc_def0u64 ^ (t + 1);
            let mut held: Vec<(*mut u8, usize)> = Vec::new();
            for _ in 0..50_000 {
                st ^= st << 13;
                st ^= st >> 7;
                st ^= st << 17;
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

/// `live` over the class's whole table, whichever arenas own the slabs
/// (the per-CPU mode binds ids to arenas, so several are in play).
#[cfg(target_os = "linux")]
fn live_all(s: &Shared, c: usize) -> usize {
    let t = s.table(c);
    (0..t.committed_slabs()).filter(|&i| !t.is_guard_slab(i)).map(|i| t.desc(i).live()).sum()
}

/// The per-CPU mode follows the probe and the key, both
/// ways. On a host whose glibc registered `mm_cid` (the reference
/// VMs) the allowed `Shared` is on; on one without (the Mac's Docker
/// VM, `__rseq_size` 20) it is off with the probe's verdict; the
/// default test builders never turn it on, `percpu_arena:off` never
/// does, and `double_free_scan` excludes it.
#[cfg(target_os = "linux")]
#[test]
fn percpu_mode_follows_the_probe_and_the_key() {
    use crate::config::PercpuArena;
    let probe = rseq::probe();
    let s = shared_percpu();
    std::eprintln!(
        "percpu: probe {} (size {}, offset {}), state {}, ids {}",
        probe.verdict(),
        probe.size,
        probe.offset,
        s.percpu.state().name(),
        s.percpu.ids()
    );
    assert_eq!(s.percpu_on(), probe.mm_cid_usable() && rseq::possible_cpus().is_some());
    assert_eq!(s.percpu_on(), s.percpu.state() == PercpuState::On);
    assert_eq!(s.push_flags() & !PUSH_PAC, 0, "the mode has no bit in the flag byte the fast paths load");
    assert!(s.percpu.forced(), "the builder asks for percpu");
    if s.percpu_on() {
        assert_eq!(s.percpu.ids(), rseq::possible_cpus().unwrap());
    } else {
        assert_eq!(s.percpu.state(), PercpuState::OffProbe);
    }
    assert!(!shared().percpu_on(), "the default builders keep thread caches");
    assert_eq!(shared().percpu.state(), PercpuState::OffAuto, "auto is the opt-in's default");
    let auto = shared_with_guards_mode(Config { narenas: 1, ..Config::DEFAULT }, false, true);
    assert!(!auto.percpu_on(), "auto keeps thread caches pending the default decision");
    assert_eq!(auto.percpu.state(), PercpuState::OffAuto);
    assert!(!auto.percpu.forced());
    let off = shared_with_guards_mode(Config { percpu_arena: PercpuArena::Off, ..Config::DEFAULT }, false, true);
    assert!(!off.percpu_on());
    assert_eq!(off.percpu.state(), PercpuState::OffKey);
    let denied =
        shared_with_guards_mode(Config { percpu_arena: PercpuArena::Percpu, ..Config::DEFAULT }, false, false);
    assert!(!denied.percpu_on(), "not allowed by the builder");
    assert_eq!(denied.percpu.state(), PercpuState::OffProbe);
    let scan = shared_with_guards_mode(
        Config { percpu_arena: PercpuArena::Percpu, double_free_scan: true, ..Config::DEFAULT },
        false,
        true,
    );
    assert!(!scan.percpu_on());
    if probe.mm_cid_usable() {
        assert_eq!(scan.percpu.state(), PercpuState::OffScan);
    }
}

/// Signed pointers under the per-CPU mode: a batch pushed onto a
/// non-empty per-CPU stack is signed with the stack base like every
/// other entry, so the pops that follow authenticate it. Signing with
/// the write position (`slots + len * 8`) instead is the base only for
/// an empty stack; a free on the same CPU between a refill's empty pop
/// and its push leaves `len > 0`, and the next pop's `autda` then
/// faults under FEAT_FPAC (seen in `tests/c/percpu` and `tls_threads`
/// under `pac_pointers:true,percpu_arena:percpu`). Here the
/// sections are driven directly on a fresh `Shared` of the mode: one
/// entry, then a batch of three, then everything taken out and
/// compared with what went in (a mis-signed entry is a `SIGILL` with
/// FPAC and a non-canonical pointer without it). Without PAC the
/// same sequence checks the stack order alone; skipped where the mode
/// is off.
#[cfg(target_os = "linux")]
#[test]
fn percpu_batch_onto_a_non_empty_stack_authenticates() {
    // The mode with the key: `PUSH_PAC` needs `pac_pointers:true` and
    // the probe (`shared_percpu` leaves the key off).
    let s = shared_with_guards_mode(
        Config { narenas: 4, percpu_arena: crate::config::PercpuArena::Percpu, pac_pointers: true, ..Config::DEFAULT },
        true,
        true,
    );
    if !s.percpu_on() {
        std::eprintln!("percpu: off on this host ({}), skipping", s.percpu.probe().verdict());
        return;
    }
    std::thread::spawn(move || {
        let t = TestHeap::new(s);
        let c = class_of(64).unwrap();
        let pac = s.pac();
        let area = t.heap().rseq_area;
        let tbl = s.percpu.table(c);
        let cap = Heap::percpu_cap(s, c);
        // Install the running id's header (a refill does), then start
        // from an empty stack, whatever id this thread is on now.
        let p0 = t.alloc(c);
        let mut buf = std::vec![ptr::null_mut::<u8>(); cap as usize];
        let taken = unsafe { rseq::take_all(area, tbl, &mut buf, pac) }.expect("id in range");
        let cid = unsafe { rseq::mm_cid(area) };
        if !t.heap().percpu_current_has(s, c, false) {
            // Migrated to an id without a header: install it here.
            assert!(t.heap().percpu_ensure(s, c, cap, cid, false));
        }
        // Fake entries: never dereferenced, only signed, stored,
        // authenticated and compared.
        let fakes: Vec<*mut u8> = (1..=4u8).map(|i| (0x0000_5a5a_0000_0000usize + (i as usize) * 64) as *mut u8).collect();
        assert!(matches!(unsafe { rseq::push(area, tbl, fakes[0], pac) }, rseq::Push::Pushed));
        let mut batch = [fakes[1], fakes[2], fakes[3]];
        let pushed = unsafe { rseq::push_batch(area, tbl, &mut batch, false, pac) }.expect("id in range");
        let mut out = std::vec![ptr::null_mut::<u8>(); cap as usize];
        let n = unsafe { rseq::take_all(area, tbl, &mut out, pac) }.expect("id in range");
        let got = &out[..n];
        // Everything that went in came back plain (a migration between
        // the sections leaves some on another id's stack: then only
        // what came back is compared, and the rest is not lost, it is
        // signed under that id's base and pops there).
        let mut seen = 0;
        for &f in &fakes[..1 + pushed] {
            if got.contains(&f) {
                seen += 1;
            }
        }
        assert!(got.iter().all(|&e| fakes.contains(&e) || !e.is_null()), "a corrupted entry came back");
        std::eprintln!(
            "percpu batch: pac={pac} cid {cid}: {taken} entries taken first, 1 + {pushed} pushed, {n} taken, {seen} of them the fakes ({} authenticated)",
            if pac { "pacda/autda" } else { "plain" }
        );
        assert!(seen >= 1, "not one fake entry came back through take_all");
        if n == 1 + pushed {
            assert_eq!(got, &fakes[..1 + pushed], "stack order bottom to top");
        }
        // Put the real entries back where a refill would want them and
        // leave the fakes out of the heap for good.
        let real: Vec<*mut u8> = buf[..taken].iter().copied().filter(|e| !e.is_null()).collect();
        if !real.is_empty() {
            let mut real = real;
            let k = unsafe { rseq::push_batch(area, tbl, &mut real, false, pac) }.expect("id in range");
            if k < real.len() {
                s.release_routed(t.heap().arena, c, &real[k..]);
            }
        }
        t.free(p0);
        t.flush_all();
        t.destroy();
    })
    .join()
    .unwrap();
}

/// With the mode on, a thread heap's plain allocations go through the
/// running CPU's caches, not the heap's own: the refill installs the
/// id's header, the thread's byte counters still move by every pop and
/// push, `calloc`'s zero prefix survives the refill, and every slot
/// out of the bitmaps is either live in the program or in a CPU cache.
#[cfg(target_os = "linux")]
#[test]
fn percpu_caches_serve_plain_allocations() {
    let s = shared_percpu();
    if !s.percpu_on() {
        std::eprintln!("percpu: off on this host ({}), skipping", s.percpu.probe().verdict());
        return;
    }
    std::thread::spawn(move || {
        let t = TestHeap::new(s);
        assert!(t.heap().percpu(), "a thread heap on an automatic arena takes the per-CPU paths");
        let c = class_of(64).unwrap();
        let before = t.stats();
        let (p0, zero) = t.alloc_zeroed(c);
        assert!(zero, "a fresh slab's batch is a zero prefix");
        let ps: Vec<*mut u8> = (0..7).map(|_| t.alloc(c)).collect();
        assert!(t.heap().cache(c).is_none(), "the thread's own cache stays untouched");
        let (headers, cached) = s.percpu.snapshot();
        assert!(headers >= 1);
        let half = (Config::DEFAULT.cache_cap(c) / 2) as usize;
        assert_eq!(cached + 8, live_all(s, c), "cached + live in the program = slots out of the bitmaps");
        assert!(live_all(s, c) <= 2 * half, "at most two refills happened (one per CPU the thread ran on)");
        let mid = t.stats();
        assert_eq!(mid.allocated - before.allocated, 8 * 64, "thread.allocated counts per-CPU pops");
        for &p in ps.iter().chain([&p0]) {
            unsafe { *p = 7 };
            t.free(p);
        }
        let after = t.stats();
        assert_eq!(after.deallocated - mid.deallocated, 8 * 64, "thread.deallocated counts per-CPU pushes");
        let (_, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c));
        // A slot freed and popped again is not zero.
        let (q, zero) = t.alloc_zeroed(c);
        assert!(!zero || unsafe { *q } == 0, "the zero flag never lies");
        t.free(q);
        t.destroy();
    })
    .join()
    .unwrap();
}

/// The per-CPU flush: more frees than a cache holds release the
/// highest-address half to the slabs and keep the lowest, the total
/// stays conserved, and `flush_all` drains the running CPU's caches.
#[cfg(target_os = "linux")]
#[test]
fn percpu_flush_releases_the_high_half_and_drain_empties() {
    let s = shared_percpu();
    if !s.percpu_on() {
        return;
    }
    std::thread::spawn(move || {
        let t = TestHeap::new(s);
        let c = class_of(64).unwrap();
        let cap = Config::DEFAULT.cache_cap(c) as usize;
        let n = 2 * cap + 100;
        let mut ps: Vec<*mut u8> = (0..n).map(|_| t.alloc(c)).collect();
        let mut set: HashSet<usize> = ps.iter().map(|&p| p as usize).collect();
        assert_eq!(set.len(), n, "distinct slots");
        for &p in &ps {
            t.free(p);
        }
        let (_, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c), "every slot is cached or released");
        assert!(cached <= cap * s.percpu.ids() as usize);
        // Allocate again: the cache hands back what it kept, lowest first
        // after a sorted flush, and the rest come from the slabs' low
        // addresses; nothing is handed out twice.
        ps.clear();
        set.clear();
        for _ in 0..n {
            let p = t.alloc(c);
            assert!(set.insert(p as usize), "slot handed out twice");
            ps.push(p);
        }
        for &p in &ps {
            t.free(p);
        }
        t.flush_all();
        // The running CPU's caches are empty now; another CPU's may
        // hold what the thread pushed while it ran there.
        let cid = unsafe { rseq::mm_cid(rseq::area(s.percpu.probe().offset)) };
        let h = s.percpu.header(cid, c).unwrap();
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*h).len) }, 0);
        let (_, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c));
        t.destroy();
    })
    .join()
    .unwrap();
}

/// Design 6.4 under the mode: a fragmented class's scheduled pops are
/// served from the id's *run*, a header of the second per-CPU table
/// filled by one block claim of `cap / 4` slots from the lowest partial
/// slab, not by a locked one-slot claim per scheduled pop. The test
/// leaves a residue of holes through the CPU caches, forces scheduled
/// pops by setting the header's `allocs` to the schedule (as the
/// thread-mode tests do), watches the class enter compact mode, and
/// then checks that the run header is installed with one block, that
/// the next `block - 1` scheduled pops come out of it without another
/// claim, that each lands in the lowest slab with room (below every
/// kept allocation), that conservation holds with the run counted, and
/// that `flush_all` drains the run as well as the cache. One automatic
/// arena, so the holes are in the arena every id binds to whatever id
/// the thread ran under; the header the test schedules is re-derived
/// from the current id before every scheduled pop, and a thread whose
/// id changes in the middle (the test harness runs more threads than
/// CPUs, where the kernel shares ids) says so and stops rather than
/// asserting on another id's header.
#[cfg(target_os = "linux")]
#[test]
fn percpu_scheduled_pops_come_from_a_run_claimed_a_block_at_a_time() {
    let s = shared_with_guards_mode(
        Config { narenas: 1, percpu_arena: crate::config::PercpuArena::Percpu, ..Config::DEFAULT },
        true,
        true,
    );
    if !s.percpu_on() {
        return;
    }
    std::thread::spawn(move || {
        let t = TestHeap::new(s);
        // 292 slots per slab, cap 512, block 128 (as the thread test).
        let c = super::tests::class_with(224);
        let cap = Config::DEFAULT.cache_cap(c) as usize;
        let block = cap / 4;
        let area = rseq::area(s.percpu.probe().offset);
        let cid = unsafe { rseq::mm_cid(area) };
        let arena = Percpu::arena_of(s, cid);
        assert_eq!(arena, 0, "one automatic arena");
        let cs = s.owner(arena).class(c);
        // A residue: 30 slabs' worth allocated, 56% freed at random,
        // the caches flushed, so the class has holes in most slabs and
        // the kept allocations are spread over all of them.
        let nslots = CLASSES[c].nslots();
        let mut st = 0x5eed_1234u64;
        let mut keep: Vec<*mut u8> = Vec::new();
        for p in (0..30 * nslots).map(|_| t.alloc(c)).collect::<Vec<_>>() {
            if xorshift(&mut st) % 1000 < 560 { t.free(p) } else { keep.push(p) }
        }
        t.flush_all();
        if unsafe { rseq::mm_cid(area) } != cid {
            std::eprintln!("percpu: the thread changed id during the residue, skipping");
            for p in keep { t.free(p) }
            t.destroy();
            return;
        }
        assert!(Heap::fragmented(s, arena, c), "the residue is a fragmented class");
        let h = s.percpu.header(cid, c).unwrap() as *mut CacheHeader;
        let rh = s.percpu.run_header(cid, c).unwrap();
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).cap) }, 0, "no run before compaction");
        // Refill the cache once (the pops below must be scheduled pops,
        // not refills), then quieten the refill gate: the probe compares
        // the class's refill counter with the header's snapshot.
        let p0 = t.alloc(c);
        unsafe {
            ptr::write_volatile(&raw mut (*h).probe_refilled, cs.refilled() as u32);
            assert_eq!(ptr::read_volatile(&raw const (*h).mode), MODE_PROBE);
        }
        // Schedule the next pop on the current id's header; `false`
        // when the thread's id is no longer `cid`.
        let schedule = |h: *mut CacheHeader| unsafe {
            if rseq::mm_cid(area) != cid {
                return false;
            }
            let m = ptr::read_volatile(&raw const (*h).compact_mask) as u64;
            ptr::write_volatile(&raw mut (*h).allocs, m);
            true
        };
        let moved = |what: &str| {
            std::eprintln!("percpu: the thread changed id {what}, stopping the test here");
        };
        let (compacted0, on0, _, _) = cs.compaction_stats();
        // The probe: enters compact mode, and the same scheduled pop is
        // served from a freshly built run.
        if !schedule(h) {
            moved("before the probe");
            t.free(p0);
            for p in keep { t.free(p) }
            t.destroy();
            return;
        }
        let p1 = t.alloc(c);
        if unsafe { rseq::mm_cid(area) } != cid {
            moved("at the probe");
            t.free(p0);
            t.free(p1);
            for p in keep { t.free(p) }
            t.destroy();
            return;
        }
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*h).mode) }, MODE_COMPACT, "the probe found the residue");
        let (compacted1, on1, _, _) = cs.compaction_stats();
        assert_eq!(on1, on0 + 1);
        assert_eq!(compacted1 - compacted0, block as u64, "one block claim of cap / 4");
        assert_ne!(unsafe { ptr::read_volatile(&raw const (*rh).cap) }, 0, "the run header is installed");
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).cap) } as usize, block);
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) } as usize, block - 1, "one served, the rest waits");
        // Where the run's slots land: the block is claimed from the
        // lowest partial slab at the time of the rebuild. The refill
        // that served `p0` took `cap / 2` of the lowest holes first, so
        // the block starts above those; it is still in the low half of
        // the residue's slabs, below the median kept slot, which is
        // what moves the live set downward.
        let slab_of = |p: *mut u8| s.regions.slab_slot(p as usize).unwrap().1;
        let mut kept_slabs: Vec<usize> = keep.iter().map(|&p| slab_of(p)).collect();
        kept_slabs.sort_unstable();
        let median_kept_slab = kept_slabs[kept_slabs.len() / 2];
        let min_kept_slab = median_kept_slab;
        assert!(
            slab_of(p1) < median_kept_slab,
            "the run's slot (slab {}) is in the low half of the residue (median kept slab {median_kept_slab})",
            slab_of(p1)
        );
        // The rest of the block, one per scheduled pop, no claim.
        let mut from_run = std::vec![p1];
        let mut stable = true;
        for _ in 1..block {
            if !schedule(h) {
                stable = false;
                break;
            }
            let p = t.alloc(c);
            from_run.push(p);
            if unsafe { rseq::mm_cid(area) } != cid {
                stable = false;
                break;
            }
            assert!(slab_of(p) < min_kept_slab, "every run slot is in the low half");
        }
        let mut p2 = None;
        if stable {
            assert_eq!(cs.compaction_stats().0, compacted1, "no claim while the run lasts");
            assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) }, 0, "the run is exhausted");
            let (_, cached) = s.percpu.snapshot();
            assert_eq!(cached + keep.len() + from_run.len() + 1, live_all(s, c), "cached (runs included) + live = out of the bitmaps");
            // The next scheduled pop rebuilds: a second block.
            if schedule(h) {
                let q = t.alloc(c);
                p2 = Some(q);
                if unsafe { rseq::mm_cid(area) } == cid {
                    assert_eq!(cs.compaction_stats().0 - compacted1, block as u64, "the rebuild claims the next block");
                    assert!(slab_of(q) <= min_kept_slab);
                    assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) } as usize, block - 1);
                } else {
                    moved("at the rebuild");
                }
            } else {
                moved("before the rebuild");
            }
        } else {
            moved("while the run was being consumed");
        }
        // The cache GC (`percpu_gc`) keeps the run of a class that is
        // still compacting and returns the run of one that stopped:
        // the second loop of the GC reads the cache header's mode.
        if unsafe { rseq::mm_cid(area) } == cid {
            let rh_len = unsafe { ptr::read_volatile(&raw const (*rh).len) };
            if rh_len != 0 && unsafe { ptr::read_volatile(&raw const (*h).mode) } == MODE_COMPACT {
                t.heap().gc(s);
                if unsafe { rseq::mm_cid(area) } == cid {
                    assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) }, rh_len, "a compacting class keeps its run through the GC");
                    unsafe {
                        ptr::write_volatile(&raw mut (*h).mode, MODE_PROBE);
                        ptr::write_volatile(&raw mut (*h).compact_mask, PROBE_MASK);
                    }
                    t.heap().gc(s);
                    if unsafe { rseq::mm_cid(area) } == cid {
                        assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) }, 0, "the GC returns the run of a class that stopped compacting");
                    } else {
                        moved("at the second GC");
                    }
                } else {
                    moved("at the first GC");
                }
            } else {
                std::eprintln!("percpu: no run to hand the GC (len {rh_len}), skipping the GC check");
            }
        }
        // The drain empties the run too (of the id the thread runs on
        // now, whose headers are re-derived).
        t.flush_all();
        let now = unsafe { rseq::mm_cid(area) };
        let (h, rh) = (s.percpu.header(now, c).unwrap(), s.percpu.run_header(now, c).unwrap());
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*rh).len) }, 0, "flush_all drains the run");
        assert_eq!(unsafe { ptr::read_volatile(&raw const (*h).len) }, 0);
        for p in keep.into_iter().chain(from_run).chain([p0]).chain(p2) {
            t.free(p);
        }
        t.flush_all();
        let (_, cached) = s.percpu.snapshot();
        assert_eq!(cached, live_all(s, c), "everything is cached or released at the end");
        t.destroy();
    })
    .join()
    .unwrap();
}

/// Many threads, more than CPUs, allocate and free at random through
/// the per-CPU caches (migrations and aborted sections included): no
/// slot is live in two threads at once, and at the end every slot out
/// of the bitmaps is in a cache.
#[cfg(target_os = "linux")]
#[test]
fn percpu_threads_conserve_slots_under_migration() {
    let s = shared_percpu();
    if !s.percpu_on() {
        return;
    }
    let c = class_of(64).unwrap();
    // More threads than possible CPUs: on a 6.19+ kernel that is what
    // leaves the per-task id mode (a thread otherwise keeps its own id
    // across migrations, and no header is ever shared), and on every
    // kernel it is what makes the abort path run.
    let nthreads = 2 * (rseq::possible_cpus().unwrap_or(1) as usize).max(2);
    let aborts0 = rseq::aborts();
    let handles: Vec<_> = (0..nthreads)
        .map(|i| {
            std::thread::spawn(move || {
                let t = TestHeap::new(s);
                let tag = (i as u64 + 1) * 0x0101_0101_0101_0101;
                let mut mine: Vec<*mut u8> = Vec::new();
                let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (i as u64 + 1);
                for _ in 0..20_000 {
                    let r = xorshift(&mut rng);
                    if mine.len() < 300 && (r & 1 == 0 || mine.is_empty()) {
                        let p = t.alloc(c);
                        unsafe { (p as *mut u64).write(tag) };
                        mine.push(p);
                    } else {
                        let k = (r as usize >> 1) % mine.len();
                        let p = mine.swap_remove(k);
                        assert_eq!(unsafe { (p as *mut u64).read() }, tag, "slot {p:?} was handed to another thread");
                        t.free(p);
                    }
                }
                for p in mine {
                    assert_eq!(unsafe { (p as *mut u64).read() }, tag);
                    t.free(p);
                }
                t.destroy();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let (headers, cached) = s.percpu.snapshot();
    assert_eq!(cached, live_all(s, c), "nothing leaked and nothing is counted twice");
    let aborts = rseq::aborts() - aborts0;
    // (A section is aborted only when a preemption or a signal lands
    // inside its few instructions; in this short run that is a
    // handful at most, and `percpu_sections_abort_and_retry_under_signals`
    // is the test that forces it.)
    std::eprintln!(
        "percpu: {nthreads} threads, {headers} headers installed, {cached} slots cached at the end, {aborts} sections aborted and retried"
    );
}

/// The abort path, forced: a worker pops and pushes through the CPU
/// caches while another thread signals it as fast as it can (a signal
/// delivered inside a section moves the thread to the abort handler
/// before the handler frame is built, on every kernel with rseq and on
/// both `mm_cid` implementations), so sections are aborted and retried
/// by the hundreds; every block the worker frees still carries its tag
/// and the slots are conserved.
///
/// The witness is exact, not inferred: the handler reads the
/// interrupted program counter from its `ucontext`, which the kernel
/// fills after the rseq fixup, so a signal that landed inside a section
/// shows `abort_ip`, recognisable by the signature word (`rseq::SIG`)
/// that precedes every abort handler. That count belongs to this
/// worker and this signal alone (the process-wide `rseq::aborts()`
/// also moves for other tests' threads and for plain preemptions).
/// Whether a signal lands inside a section is up to the host: the
/// sections are a few instructions and a worker that does not run
/// (a shared-core guest with the rest of the suite beside it, the
/// k68-amd64 cell of `bench/results/2026-09-20-phase-2.5-acceptance.md`
/// 1b.2 (c)) coalesces every signal into one. So the signaller keeps
/// the 400 ms stress window, extends it only while no abort has been
/// witnessed, and past `ABORT_CAP` prints a skip line naming the
/// host's shape instead of passing or failing on luck.
#[cfg(target_os = "linux")]
#[test]
fn percpu_sections_abort_and_retry_under_signals() {
    use std::os::unix::thread::JoinHandleExt;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};
    /// The stress window on any host; the retries this collects are the
    /// test's coverage of the retry paths.
    const ABORT_WINDOW: Duration = Duration::from_millis(400);
    /// The most the window stretches while no abort has been witnessed.
    const ABORT_CAP: Duration = Duration::from_secs(4);
    /// Signals whose handler ran on the worker.
    static DELIVERED: AtomicU64 = AtomicU64::new(0);
    /// Of those, the ones that found the worker inside a section: the
    /// kernel had already moved it to an abort handler.
    static IN_SECTION: AtomicU64 = AtomicU64::new(0);
    let s = shared_percpu();
    if !s.percpu_on() {
        return;
    }
    extern "C" fn on_signal(_: libc::c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        DELIVERED.fetch_add(1, Ordering::Relaxed);
        // SAFETY: an `SA_SIGINFO` handler's third argument is the
        // `ucontext_t` the kernel built for this delivery.
        let uc = unsafe { &*(ctx as *const libc::ucontext_t) };
        #[cfg(target_arch = "x86_64")]
        let ip = uc.uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
        #[cfg(target_arch = "aarch64")]
        let ip = uc.uc_mcontext.pc as usize;
        // The interrupted program counter is inside a mapped text
        // segment of this binary or a library it loaded (the worker
        // never runs vDSO code), and every such mapping starts with an
        // ELF header or a preceding segment, so the four bytes before
        // it are readable; they are the signature exactly when the
        // kernel moved the thread to a section's `abort_ip`.
        // SAFETY: as argued; a plain unaligned load from text.
        if ip >= 4 && unsafe { ptr::read_unaligned((ip - 4) as *const u32) } == rseq::SIG {
            IN_SECTION.fetch_add(1, Ordering::Relaxed);
        }
    }
    // SAFETY: installing a handler for SIGUSR1, which nothing else in
    // the test binary uses.
    unsafe {
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut sa.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, ptr::null_mut()), 0);
    }
    let c = class_of(64).unwrap();
    let aborts0 = rseq::aborts();
    let stop = Arc::new(AtomicUsize::new(0));
    let worker = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let t = TestHeap::new(s);
            let tag = 0xA5A5_0000_0000_0001u64;
            let mut mine: Vec<*mut u8> = Vec::new();
            let mut rng = 0x1234_5678_9ABC_DEF1u64;
            let mut ops = 0u64;
            while stop.load(Ordering::Relaxed) == 0 {
                for _ in 0..256 {
                    let r = xorshift(&mut rng);
                    if mine.len() < 200 && (r & 1 == 0 || mine.is_empty()) {
                        let p = t.alloc(c);
                        unsafe { (p as *mut u64).write(tag) };
                        mine.push(p);
                    } else {
                        let k = (r as usize >> 1) % mine.len();
                        let p = mine.swap_remove(k);
                        assert_eq!(unsafe { (p as *mut u64).read() }, tag);
                        t.free(p);
                    }
                    ops += 1;
                }
            }
            for p in mine {
                t.free(p);
            }
            t.destroy();
            ops
        })
    };
    let target = worker.as_pthread_t();
    // The worker's CPU clock: how long it actually ran, against the
    // wall time of the window (the difference is the host's share).
    let mut clock: libc::clockid_t = 0;
    // SAFETY: `target` is alive until it is joined below.
    let clock = if unsafe { libc::pthread_getcpuclockid(target, &mut clock) } == 0 { Some(clock) } else { None };
    let t0 = Instant::now();
    let mut signals = 0u64;
    loop {
        // SAFETY: the worker is alive until `stop` is set below.
        unsafe { libc::pthread_kill(target, libc::SIGUSR1) };
        signals += 1;
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        let elapsed = t0.elapsed();
        if elapsed >= ABORT_WINDOW && (IN_SECTION.load(Ordering::Relaxed) > 0 || elapsed >= ABORT_CAP) {
            break;
        }
    }
    let elapsed = t0.elapsed();
    let worker_ms = clock.and_then(|clk| {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: `ts` is a valid out-pointer; the clock names the live worker.
        (unsafe { libc::clock_gettime(clk, &mut ts) } == 0).then(|| ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000)
    });
    stop.store(1, Ordering::Relaxed);
    let ops = worker.join().unwrap();
    let delivered = DELIVERED.load(Ordering::Relaxed);
    let in_section = IN_SECTION.load(Ordering::Relaxed);
    let aborts = rseq::aborts() - aborts0;
    let shape = {
        let allowed = {
            // SAFETY: a zeroed `cpu_set_t` is a valid empty set the call fills.
            let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
            if unsafe { libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set) } == 0 {
                (0..libc::CPU_SETSIZE as usize).filter(|&i| unsafe { libc::CPU_ISSET(i, &set) }).count()
            } else {
                0
            }
        };
        let usable = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
        let possible = rseq::possible_cpus().unwrap_or(0);
        let worker_ms = worker_ms.map(|ms| std::format!("{ms}")).unwrap_or_else(|| "?".into());
        std::format!(
            "{signals} signals ({delivered} delivered) over {} ms on {allowed} allowed CPUs ({usable} usable, {possible} possible), the worker ran {worker_ms} ms for {ops} operations",
            elapsed.as_millis()
        )
    };
    let (_, cached) = s.percpu.snapshot();
    assert_eq!(cached, live_all(s, c), "slots conserved across the retries");
    if in_section == 0 {
        std::eprintln!("percpu: skip: no abort in {shape}: cannot prove the retry path here ({aborts} sections aborted process-wide meanwhile)");
        return;
    }
    std::eprintln!("percpu: {in_section} sections aborted by the signal and retried, {aborts} process-wide, under {shape}");
    // The abort handler of every section counts itself (94fa9aa) before
    // jumping back: at least one ran. Not `>= in_section`: a signal that
    // finds the worker already at an abort handler's entry (moved there
    // by an earlier preemption or signal, its increment not yet run)
    // is counted once more than the handler runs (716 against 712 in
    // one `make test` on the C4), and the aarch64 increment can lose a
    // count to another thread's.
    assert!(aborts >= 1, "{in_section} signals landed at an abort handler but rseq::aborts() did not move");
}

/// Heaps off the mode inside a per-CPU process take the thread paths:
/// an explicit cache, a thread with `thread.tcache.enabled:false`
/// (served directly), and a thread moved to an explicit arena (its own
/// caches on that arena).
#[cfg(target_os = "linux")]
#[test]
fn percpu_heaps_off_the_mode_take_the_thread_paths() {
    let s = shared_percpu();
    if !s.percpu_on() {
        return;
    }
    std::thread::spawn(move || {
        let c = class_of(64).unwrap();
        // Explicit cache.
        let e = Heap::create_detached(s, 0);
        let eh = unsafe { &mut *e };
        assert!(!eh.percpu());
        let p = eh.alloc(s, c);
        assert!(eh.cache(c).is_some(), "an explicit cache uses its own stacks");
        eh.free(s, p);
        unsafe { Heap::destroy(e, s) };
        // Thread caches off: direct.
        let t = TestHeap::new(s);
        assert!(t.heap().percpu());
        t.heap().set_tcache_enabled(s, false);
        assert!(!t.heap().percpu());
        let live0 = live_all(s, c);
        let (_, cached0) = s.percpu.snapshot();
        let p = t.alloc(c);
        assert_eq!(live_all(s, c), live0 + 1, "claimed straight from a slab");
        assert_eq!(s.percpu.snapshot().1, cached0, "no CPU cache touched");
        t.free(p);
        assert_eq!(live_all(s, c), live0);
        t.heap().set_tcache_enabled(s, true);
        assert!(t.heap().percpu());
        // An explicit arena: the thread's own caches on it.
        let a = s.arenas.create(s).expect("arenas.create");
        t.heap().set_arena(s, a);
        assert!(!t.heap().percpu());
        let p = t.alloc(c);
        assert_eq!(s.classify_free(p), Ok((c, a)));
        assert!(t.heap().cache(c).is_some());
        t.free(p);
        assert!(t.heap().cache(c).unwrap().len() >= 1);
        t.destroy();
    })
    .join()
    .unwrap();
}
