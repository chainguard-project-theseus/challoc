use super::*;
use crate::api::jemalloc::{dallocx, mallocx};
use crate::api::libc::{free, malloc};
use crate::api::shared;
use crate::config::Config;
use crate::heap::mallocx::{MALLOCX_TCACHE_NONE, mallocx_arena, mallocx_tcache};
use std::ffi::CString;
use std::vec::Vec;

// The three entry points, with a placeholder token for the call and
// for `errno`.
fn mallctl(
    name: *const c_char,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    newp: *const c_void,
    newlen: usize,
) -> c_int {
    super::mallctl(
        name,
        oldp,
        oldlenp,
        newp,
        newlen,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    )
}

fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> c_int {
    super::mallctlnametomib(name, mibp, miblenp, Tracked::assume_new())
}

fn mallctlbymib(
    mib: *const usize,
    miblen: usize,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    newp: *const c_void,
    newlen: usize,
) -> c_int {
    super::mallctlbymib(
        mib,
        miblen,
        oldp,
        oldlenp,
        newp,
        newlen,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    )
}

/// `mallctl` by name with typed in/out values.
fn ctl_rw<T: Copy>(name: &str, old: Option<&mut T>, new: Option<&T>) -> c_int {
    let cname = CString::new(name).unwrap();
    let mut oldlen = size_of::<T>();
    let (oldp, oldlenp) = match old {
        Some(o) => (o as *mut T as *mut c_void, &mut oldlen as *mut usize),
        None => (ptr::null_mut(), ptr::null_mut()),
    };
    let (newp, newlen) = match new {
        Some(n) => (n as *const T as *const c_void, size_of::<T>()),
        None => (ptr::null(), 0),
    };
    mallctl(cname.as_ptr(), oldp, oldlenp, newp, newlen)
}

fn get<T: Copy + Default>(name: &str) -> T {
    let mut v = T::default();
    let rc = ctl_rw(name, Some(&mut v), None);
    assert_eq!(rc, 0, "mallctl({name}) failed with {rc}");
    v
}

fn set<T: Copy>(name: &str, v: T) -> c_int {
    ctl_rw::<T>(name, None, Some(&v))
}

fn call(name: &str) -> c_int {
    let cname = CString::new(name).unwrap();
    mallctl(
        cname.as_ptr(),
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null(),
        0,
    )
}

fn mib_of(name: &str) -> Vec<usize> {
    let cname = CString::new(name).unwrap();
    let mut mib = [0usize; MAX_MIB];
    let mut len = MAX_MIB;
    let rc = mallctlnametomib(cname.as_ptr(), mib.as_mut_ptr(), &mut len);
    assert_eq!(rc, 0, "mallctlnametomib({name}) = {rc}");
    mib[..len].to_vec()
}

/// The read-only subtrees: `opt`, `arenas` and `stats` are entries 4,
/// 5 and 7 of the root, and no reader under them is one of the
/// `thread.*` readers, the only ones that may abort
/// (`Reader::aborts`): theorem "Read-only APIs never abort"
/// (`theorem_mallctl_reads_never_abort`) covers every read under them.
#[test]
fn read_only_subtrees_are_roots_4_5_and_7() {
    assert_eq!(
        (ROOT[4].name, ROOT[5].name, ROOT[7].name),
        ("opt", "arenas", "stats")
    );
    fn readers(n: &Node, count: &mut usize) {
        match n.entry {
            Entry::Dir(children) => {
                for c in children {
                    readers(c, count);
                }
            }
            Entry::Read(r) => {
                assert!(
                    !matches!(
                        r,
                        Reader::ThreadAllocated
                            | Reader::ThreadAllocatedp
                            | Reader::ThreadDeallocated
                            | Reader::ThreadDeallocatedp
                    ),
                    "`{}` is read by a `thread.*` reader",
                    n.name
                );
                *count += 1;
            }
            Entry::Op(_) => {}
        }
    }
    // The readers under `opt` (one per key), `arenas` and `stats`.
    let counts = [4, 5, 7].map(|i| {
        let mut count = 0;
        readers(&ROOT[i], &mut count);
        count
    });
    assert_eq!(counts, [KEYS.len(), 8, 35]);
}

/// A pointer count whose triples overflow `usize` must not pass the
/// length check: `n * 24` wrapping to the caller's 8-byte `oldlen`
/// would have the loop read `n` pointers and write `3 * n` words.
#[test]
fn batch_query_rejects_a_count_whose_triples_overflow() {
    let mib = mib_of("experimental.utilization.batch_query");
    let p = malloc(
        16,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let ptrs = [p as *const c_void];
    let mut out = [0usize; 1];
    let mut outlen = size_of::<usize>();
    let n = usize::MAX / (3 * size_of::<usize>()) + 1;
    assert_eq!(
        n.wrapping_mul(3 * size_of::<usize>()),
        outlen,
        "the product wraps to the out length"
    );
    let newlen = n * size_of::<*const c_void>();
    let rc = mallctlbymib(
        mib.as_ptr(),
        mib.len(),
        out.as_mut_ptr() as *mut c_void,
        &mut outlen,
        ptrs.as_ptr() as *const c_void,
        newlen,
    );
    assert_eq!(rc, EINVAL);
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

fn get_mib<T: Copy + Default>(mib: &[usize]) -> T {
    let mut v = T::default();
    let mut len = size_of::<T>();
    let rc = mallctlbymib(
        mib.as_ptr(),
        mib.len(),
        &mut v as *mut T as *mut c_void,
        &mut len,
        ptr::null(),
        0,
    );
    assert_eq!(rc, 0, "mallctlbymib({mib:?}) = {rc}");
    v
}

/// Design 13.5: an `epoch` write runs the calling thread's cache GC
/// first when one is due, so the refreshed figures do not count slots
/// that only sit in its caches; a read, and a write inside the
/// interval, run none. Its own thread, so the heap is the test's.
#[test]
fn an_epoch_write_runs_the_callers_cache_gc_when_due() {
    // It writes `epoch`, which the tests that read frozen figures
    // serialize on.
    let _serial = crate::testutil::global_state();
    std::thread::spawn(|| {
        let heap = || {
            crate::heap::existing_heap(crate::heap::shared_ptr(shared()))
                .expect("this thread's heap")
        };
        // A batch through the thread's cache, all but one back in.
        let ptrs: Vec<*mut u8> = (0..64)
            .map(|_| mallocx(100, 0, Tracked::assume_new(), Tracked::assume_new()))
            .collect();
        for p in &ptrs[1..] {
            dallocx(
                *p,
                0,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            );
        }
        let c = crate::layout::class_of(100).unwrap();
        let len = || heap().cache(c).map_or(0, |k| k.len());
        // A read refreshes nothing and runs no GC.
        heap().set_last_gc(0);
        let mut e: u64 = 0;
        assert_eq!(ctl_rw::<u64>("epoch", Some(&mut e), None), 0);
        assert_eq!(heap().last_gc(), 0, "a read runs no GC");
        // A write with the last GC long ago runs it (restarting the low
        // water), and an interval later the next returns three quarters
        // of what sat unused since.
        epoch();
        assert_ne!(heap().last_gc(), 0, "the write ran the GC");
        let after_first = len();
        assert!(after_first > 0);
        let at = heap().last_gc();
        epoch();
        assert_eq!(heap().last_gc(), at, "not again within the interval");
        assert_eq!(len(), after_first);
        heap().set_last_gc(0);
        epoch();
        assert_eq!(len(), after_first - (after_first - after_first / 4));
        dallocx(
            ptrs[0],
            0,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
    })
    .join()
    .unwrap();
}

fn epoch() -> u64 {
    let mut e: u64 = 0;
    let one: u64 = 1;
    assert_eq!(ctl_rw("epoch", Some(&mut e), Some(&one)), 0);
    e
}

/// An `epoch` write arms the slab-pages walks (`purge::note_epoch`), so a
/// consumer's next read counts the free pages they release as released:
/// the write makes a call, and afterwards every class of arena 0 is
/// within its hint interval (armed by this write or by one less than
/// `HINT_NS` before it).
#[test]
fn an_epoch_write_arms_the_slab_pages_walks() {
    let calls = crate::purge::NOTE_EPOCH_CALLS.load(core::sync::atomic::Ordering::Relaxed);
    let before = crate::sys::now_coarse_ns();
    epoch();
    assert!(
        crate::purge::NOTE_EPOCH_CALLS.load(core::sync::atomic::Ordering::Relaxed) > calls,
        "the write called note_epoch"
    );
    let s = shared();
    let a = s.arenas.get_existing(0).unwrap();
    for c in 0..crate::layout::NCLASSES {
        assert!(
            a.class(c).hint_due_for_tests() > before,
            "class {c} not armed"
        );
    }
}

/// Each `epoch` write stores a new number and reads that number back,
/// so concurrent writers all read back different numbers. The four
/// writers start every write together, on a private allocator so no
/// other test's epochs interleave.
#[test]
fn concurrent_epoch_writes_read_back_their_own_numbers() {
    use std::sync::Barrier;
    let s = crate::testutil::shared();
    let mut mib = [0usize; MAX_MIB];
    let (len, node) = name_to_mib(b"epoch", &mut mib).unwrap();
    let write = || {
        let mut e: u64 = 0;
        let mut elen = size_of::<u64>();
        let one: u64 = 1;
        let io = Io {
            oldp: &mut e as *mut u64 as *mut c_void,
            oldlenp: &mut elen,
            newp: &one as *const u64 as *const c_void,
            newlen: size_of::<u64>(),
        };
        // Checked after the join: a failed write must not leave the
        // other writers waiting at the barrier.
        ctl(
            s,
            Tracked::assume_new(),
            node,
            &mib[..len],
            &io,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .map(|()| e)
    };
    let go = Barrier::new(4);
    let seen: Vec<Result<u64, c_int>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                sc.spawn(|| {
                    (0..2000)
                        .map(|_| {
                            go.wait();
                            write()
                        })
                        .collect::<Vec<Result<u64, c_int>>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let seen: Vec<u64> = seen.into_iter().map(|r| r.expect("epoch write")).collect();
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        (sorted.len(), sorted.first(), sorted.last()),
        (seen.len(), Some(&1), Some(&(seen.len() as u64))),
        "(distinct, lowest, highest) epoch numbers read back"
    );
}

/// `arena.0.decay` walks the huge extents on the period `purge::decay`
/// gives them, as `arena.<MALLCTL_ARENAS_ALL>.decay` does: the huge
/// tier's own (`huge_dirty_decay_ms`, design 7) rather than arena 0's,
/// and both halves scaled by the cgroup policy's band (design 11).
#[test]
fn arena_decay_uses_the_periods_of_the_decay_driver() {
    use crate::policy::Band;
    let decay = |s: &Shared, i: u16| {
        let mut mib = [0usize; MAX_MIB];
        let name = std::format!("arena.{i}.decay");
        let (len, node) = name_to_mib(name.as_bytes(), &mut mib).unwrap();
        let io = Io {
            oldp: ptr::null_mut(),
            oldlenp: ptr::null_mut(),
            newp: ptr::null(),
            newlen: 0,
        };
        assert_eq!(
            ctl(
                s,
                Tracked::assume_new(),
                node,
                &mib[..len],
                &io,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new()
            ),
            Ok(()),
            "{name}"
        );
    };
    let free_huge_run = |s: &Shared| {
        let p = s
            .alloc_huge(
                300 * 1024,
                16,
                0,
                false,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
            .0;
        match s.regions.classify(p as usize) {
            Kind::Huge { page } => s.free_huge(
                page,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            ),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            (
                s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
                s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
            ),
            (1, 0)
        );
    };

    // `huge_dirty_decay_ms:-1` never purges a huge run by decay, even
    // with arena 0 purging at once.
    let s = crate::testutil::TestShared::with(Config {
        narenas: 1,
        dirty_decay_ms: 0,
        huge_dirty_decay_ms: Some(-1),
        ..Config::DEFAULT
    })
    .build();
    free_huge_run(s);
    decay(s, 0);
    assert_eq!(
        (
            s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
            s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
        ),
        (1, 0),
        "huge_dirty_decay_ms:-1: retained"
    );

    // Above 90% of a cgroup limit every period is 0: a slab and a huge
    // run freed a moment ago both go, though both windows are 10 s.
    let s = crate::testutil::TestShared::with(Config {
        narenas: 1,
        ..Config::DEFAULT
    })
    .build();
    let class = CLASSES.len() - 1;
    let p = s
        .alloc_direct(
            &s.arena0,
            vstd::prelude::Tracked::assume_new(),
            class,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .0;
    assert!(!p.is_null());
    s.free_direct(
        0,
        class,
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(s.arena0.class(class).stats().empty, 1);
    free_huge_run(s);
    s.policy.apply(Band::Immediate, 512 << 20, 0, 0);
    // Only arena 0's call walks the huge extents.
    let a = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    decay(s, a);
    assert_eq!(
        (
            s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
            s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
        ),
        (1, 0),
        "arena.{a}.decay leaves the huge run"
    );
    decay(s, 0);
    let slabs = s.arena0.class(class).stats();
    assert_eq!(
        (slabs.empty, slabs.purged),
        (0, 1),
        "immediate band: the slab is purged"
    );
    assert_eq!(
        (
            s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
            s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
        ),
        (0, 1),
        "immediate band: the huge run is purged"
    );
}

fn cstring_at(p: *const c_char) -> std::string::String {
    // SAFETY: the callers pass a NUL-terminated string from `mallctl`.
    unsafe { core::ffi::CStr::from_ptr(p) }
        .to_str()
        .unwrap()
        .into()
}

/// Every name in the tree (numeric components as `0`), directories
/// included, in tree order.
fn all_names() -> Vec<std::string::String> {
    fn walk(nodes: &'static [Node], prefix: &str, out: &mut Vec<std::string::String>) {
        for n in nodes {
            let name = if n.name == NUMERIC { "0" } else { n.name };
            let full = if prefix.is_empty() {
                name.into()
            } else {
                std::format!("{prefix}.{name}")
            };
            out.push(full.clone());
            walk(n.children(), &full, out);
        }
    }
    let mut out = Vec::new();
    walk(&ROOT, "", &mut out);
    out
}

/// Every key's name resolves to a MIB that leads back to the same key,
/// and a directory is never a key.
#[test]
fn every_name_round_trips_through_its_mib() {
    let names = all_names();
    assert!(names.len() > 100, "{names:?}");
    for name in &names {
        let mib = mib_of(name);
        let node = mib_to_node(&mib).unwrap();
        let last = name.rsplit('.').next().unwrap();
        assert!(
            node.name == last || (node.name == NUMERIC && last == "0"),
            "{name}: {mib:?}"
        );
        match node.entry {
            Entry::Dir(_) => assert_eq!(call(name), ENOENT, "{name}"),
            _ => assert_eq!(mib_to_key(&mib), Ok(node.name), "{name}"),
        }
    }
}

#[test]
fn names_resolve_to_positional_mibs_and_back() {
    // Reads `stats.cgroup.decay_ms` against arena 0's decay.
    let _serial = crate::testutil::global_state();
    assert_eq!(mib_of("version"), [0]);
    assert_eq!(mib_of("epoch"), [1]);
    // Numeric components are stored as the number, at the position
    // Valkey overwrites.
    assert_eq!(mib_of("arenas.bin.7.size"), [5, 5, 7, 0]);
    assert_eq!(mib_of("arena.4096.purge"), [6, 4096, 0]);
    assert_eq!(
        mib_of("stats.arenas.3.bins.51.nonfull_slabs"),
        [7, 6, 3, 8, 51, 2]
    );
    assert_eq!(mib_of("stats.arenas.0.small.allocated"), [7, 6, 0, 3, 0]);
    assert_eq!(mib_of("experimental.utilization.batch_query"), [10, 0, 1]);
    for (i, key) in KEYS.iter().enumerate() {
        assert_eq!(mib_of(&std::format!("opt.{key}")), [4, i]);
    }
    // Interior nodes resolve (jemalloc allows it) but are not keys.
    assert_eq!(mib_of("arenas"), [5]);
    assert_eq!(mib_to_key(&[5]), Err(ENOENT));
    assert_eq!(call("arenas"), ENOENT);
    assert_eq!(mib_to_key(&[5, 5, 7, 0]), Ok("size"));
    assert_eq!(mib_to_key(&[7, 6, 3, 8, 51, 2]), Ok("nonfull_slabs"));
    assert_eq!(
        mib_to_key(&[7, 6, 3, 4, 51, 2]),
        Err(ENOENT),
        "large has one child"
    );
    assert_eq!(mib_to_key(&[99]), Err(ENOENT));
    assert_eq!(mib_to_key(&[5, 5, 7, 9]), Err(ENOENT));
    assert_eq!(mib_to_key(&[]), Err(ENOENT));
    // Unknown, malformed, and prof.* names.
    // `stats.cgroup.*` (design 11): the names resolve; a read is
    // `ENOENT` only where no cgroup2 is tracked.
    assert_eq!(mib_of("stats.cgroup.limit"), [7, 7, 0]);
    assert_eq!(mib_to_key(&[7, 7, 2]), Ok("current"));
    // The policy's keys (design 11, `policy.rs`) follow, in table order.
    assert_eq!(mib_of("stats.cgroup.decay_ms"), [7, 7, 3]);
    assert_eq!(mib_of("stats.cgroup.full_purges"), [7, 7, 6]);
    assert_eq!(mib_to_key(&[7, 7, 4]), Ok("retention_cap"));
    assert_eq!(mib_to_key(&[7, 7, 5]), Ok("empty_bytes"));
    // The two-advice purge's keys (design 8) follow and read
    // with or without a tracked cgroup: eager by default (no limit on
    // the host, none in the default container), nothing lazy issued.
    assert_eq!(mib_of("stats.cgroup.purge_advice"), [7, 7, 7]);
    assert_eq!(mib_of("stats.cgroup.lazy_purges"), [7, 7, 8]);
    assert_eq!(mib_of("stats.cgroup.eager_purges"), [7, 7, 9]);
    assert_eq!(mib_to_key(&[7, 7, 10]), Err(ENOENT));
    let advice: usize = get("stats.cgroup.purge_advice");
    assert_eq!(advice, 0, "eager without a limit");
    let lazy: usize = get("stats.cgroup.lazy_purges");
    assert_eq!(lazy, 0);
    let _eager: usize = get("stats.cgroup.eager_purges");
    assert_eq!(set("stats.cgroup.purge_advice", 1usize), EPERM);
    let tracked = shared()
        .cgroup_now(vstd::prelude::Tracked::assume_new())
        .tracked();
    for key in [
        "stats.cgroup.limit",
        "stats.cgroup.high",
        "stats.cgroup.current",
        "stats.cgroup.decay_ms",
        "stats.cgroup.retention_cap",
        "stats.cgroup.empty_bytes",
        "stats.cgroup.full_purges",
    ] {
        assert_eq!(
            call(key),
            if tracked { 0 } else { ENOENT },
            "{key} tracked={tracked}"
        );
        assert_eq!(
            set(key, 1usize),
            if tracked { EPERM } else { ENOENT },
            "{key}"
        );
    }
    if tracked {
        let limit: usize = get("stats.cgroup.limit");
        let high: usize = get("stats.cgroup.high");
        let current: usize = get("stats.cgroup.current");
        std::eprintln!("stats.cgroup: limit {limit} high {high} current {current}");
        assert_eq!(
            limit,
            shared()
                .cgroup_now(vstd::prelude::Tracked::assume_new())
                .limit() as usize
        );
        assert_eq!(
            high,
            shared()
                .cgroup_now(vstd::prelude::Tracked::assume_new())
                .high() as usize
        );
        assert!(limit == usize::MAX || limit > 0);
        // The policy's keys: the defaults without a limit (the period
        // as configured, no cap), the cap arithmetic with one.
        let decay_ms: usize = get("stats.cgroup.decay_ms");
        let cap: usize = get("stats.cgroup.retention_cap");
        let empty: usize = get("stats.cgroup.empty_bytes");
        let full: usize = get("stats.cgroup.full_purges");
        std::eprintln!(
            "stats.cgroup: decay_ms {decay_ms} retention_cap {cap} empty_bytes {empty} full_purges {full}"
        );
        let effective = limit.min(high);
        if effective == usize::MAX {
            assert_eq!(decay_ms, (shared().decay_ns() / 1_000_000) as usize);
            assert_eq!(cap, usize::MAX);
        } else {
            // The sampler may not have run in this process yet: the cap
            // is either unpublished or the limit's twentieth.
            assert!(
                cap == usize::MAX || cap == effective / 20,
                "cap {cap} for limit {effective}"
            );
        }
        assert_eq!(full, 0);
    }
    // `stats.cgroup` itself is interior: it resolves, and is not a key.
    assert_eq!(mib_of("stats.cgroup"), [7, 7]);
    assert_eq!(call("stats.cgroup"), ENOENT);
    for bad in [
        "nope",
        "arenas.bin.x.size",
        "arenas.bin..size",
        "arenas.bin.7x.size",
        "arenas.bin.18446744073709551616.size",
        "prof.active",
        "prof.dump",
        "stats.cgroup.nope",
        "version.more",
        "",
    ] {
        let c = CString::new(bad).unwrap();
        let mut mib = [0usize; MAX_MIB];
        let mut len = MAX_MIB;
        assert_eq!(
            mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len),
            ENOENT,
            "{bad}"
        );
        assert_eq!(call(bad), ENOENT, "{bad}");
    }
    assert_eq!(
        mallctl(
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            0
        ),
        EINVAL
    );
    // Too small a MIB buffer.
    let c = CString::new("arenas.bin.7.size").unwrap();
    let mut mib = [0usize; 2];
    let mut len = 2;
    assert_eq!(
        mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len),
        EINVAL
    );
    assert_eq!(
        mallctlbymib(
            ptr::null(),
            1,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            0
        ),
        EINVAL
    );
    assert_eq!(
        mallctlbymib(
            mib.as_ptr(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            0
        ),
        EINVAL
    );
}

#[test]
fn read_write_rules_match_jemalloc() {
    // Advances the stats epoch.
    let _serial = crate::testutil::global_state();
    // Reading with the wrong size copies the prefix and fails EINVAL.
    let mut short: u32 = 0;
    let c = CString::new("arenas.page").unwrap();
    let mut len = 4usize;
    let rc = mallctl(
        c.as_ptr(),
        &mut short as *mut u32 as *mut c_void,
        &mut len,
        ptr::null(),
        0,
    );
    assert_eq!(rc, EINVAL);
    assert_eq!(
        short as usize,
        shared().regions.page_size(),
        "the low half was still copied"
    );
    // Writing a read-only key is EPERM; a wrong newlen on a writable key
    // is EINVAL; a write-only key refuses a read.
    assert_eq!(set("version", 1u64), EPERM);
    assert_eq!(set("stats.allocated", 1usize), EPERM);
    assert_eq!(set("arenas.nbins", 1u32), EPERM);
    assert_eq!(set("epoch", 1u32), EINVAL);
    assert_eq!(set("arena.0.dirty_decay_ms", 5i32), EINVAL);
    let mut out: c_uint = 0;
    assert_eq!(ctl_rw("tcache.flush", Some(&mut out), None), EPERM);
    assert_eq!(set("tcache.flush", 4000 as c_uint), EFAULT);
    assert_eq!(call("tcache.flush"), EINVAL, "write-only key needs a value");
    // Void keys take neither.
    assert_eq!(set("arena.0.purge", 1u32), EPERM);
    let mut v: u32 = 0;
    assert_eq!(ctl_rw("thread.tcache.flush", Some(&mut v), None), EPERM);
    // Null out-pointers mean "do not read".
    assert_eq!(call("version"), 0);
    assert_eq!(call("epoch"), 0);
}

#[test]
fn static_and_opt_keys_report_the_build_and_configuration() {
    // Asserts arena 0's decay, writes `background_thread`.
    let _serial = crate::testutil::global_state();
    let v: *const c_char = get("version");
    assert_eq!(cstring_at(v), VERSION_STR);
    assert!(cstring_at(v).starts_with("5.3.0-0-g"));
    let s: *const c_char = get("config.malloc_conf");
    assert_eq!(cstring_at(s), "");
    assert!(!get::<bool>("config.debug"));
    assert!(get::<bool>("config.stats"));
    assert_eq!(get::<c_uint>("arenas.nbins") as usize, NBINS);
    assert_eq!(
        get::<usize>(&std::format!("arenas.bin.{}.size", NBINS - 1)),
        crate::layout::BIN_MAX
    );
    assert_eq!(get::<c_uint>("arenas.nlextents"), 0);
    assert_eq!(get::<usize>("arenas.quantum"), QUANTUM);
    assert_eq!(get::<usize>("arenas.page"), shared().regions.page_size());
    assert!(get::<c_uint>("arenas.narenas") >= 1);
    assert_eq!(get::<usize>("max_background_threads"), 1);
    assert_eq!(set("max_background_threads", 0usize), EINVAL);
    assert_eq!(set("max_background_threads", 4usize), 0);
    for j in 0..NBINS {
        assert_eq!(
            get::<usize>(&std::format!("arenas.bin.{j}.size")),
            CLASSES[j].size()
        );
        assert_eq!(
            get::<u32>(&std::format!("arenas.bin.{j}.nregs")) as usize,
            CLASSES[j].nslots()
        );
        assert_eq!(
            get::<usize>(&std::format!("arenas.bin.{j}.slab_size")),
            CLASSES[j].slab_size()
        );
    }
    assert_eq!(call(&std::format!("arenas.bin.{NBINS}.size")), ENOENT);
    // Every opt key with its jemalloc type, against the live Config:
    // `ssize_t` for the signed keys, `size_t` for the byte counts
    // (`Value::Size`), `unsigned` for the other counts.
    let cfg: &Config = &shared().config;
    for key in KEYS {
        let name = std::format!("opt.{key}");
        match cfg.get(key.as_bytes()).unwrap() {
            Value::Bool(b) => assert_eq!(get::<bool>(&name), b, "{name}"),
            Value::Int(i) => assert_eq!(get::<isize>(&name), i as isize, "{name}"),
            Value::Uint(u) => assert_eq!(get::<c_uint>(&name), u as c_uint, "{name}"),
            Value::Size(n) => assert_eq!(get::<usize>(&name), n, "{name}"),
            Value::Str(s) => assert_eq!(
                cstring_at(get::<*const c_char>(&name)),
                s.cstr().to_str().unwrap(),
                "{name}"
            ),
        }
        assert_eq!(set(&name, 0u8), EPERM, "{name} must be read-only");
    }
    for key in ["tcache_max", "huge_retain_max"] {
        assert!(
            matches!(cfg.get(key.as_bytes()), Some(Value::Size(_))),
            "opt.{key} is a byte count of the table"
        );
    }
    assert_eq!(
        call("opt.junk"),
        ENOENT,
        "accepted-and-ignored conf keys are not readable"
    );
    assert_eq!(
        call("opt.stats_print"),
        ENOENT,
        "the inert key without a field is not readable"
    );
    // The two byte-count options are `size_t`, as jemalloc's
    // `opt.tcache_max` is: an `unsigned` buffer is the wrong size and
    // gets the low half plus `EINVAL`, as for any size_t key.
    assert_eq!(get::<usize>("opt.tcache_max"), cfg.tcache_max);
    let mut half: c_uint = 0;
    assert_eq!(ctl_rw("opt.tcache_max", Some(&mut half), None), EINVAL);
    assert_eq!(
        half as usize,
        cfg.tcache_max & u32::MAX as usize,
        "the low half was still copied"
    );
    assert_eq!(
        get::<c_uint>("opt.narenas"),
        cfg.narenas as c_uint,
        "a count stays unsigned"
    );
    assert_eq!(
        get::<isize>("opt.dirty_decay_ms"),
        cfg.dirty_decay_ms as isize,
        "a period stays ssize_t"
    );
    assert_eq!(
        get::<isize>("opt.huge_dirty_decay_ms"),
        cfg.huge_decay_ms() as isize,
        "unset: the slab period"
    );
    // `opt.huge_retain_max` reports the value the huge allocator uses.
    assert_eq!(
        get::<usize>("opt.huge_retain_max"),
        shared().huge.retain_max()
    );
    assert_eq!(
        get::<usize>("opt.huge_retain_max"),
        crate::huge::DEFAULT_RETAIN_MAX
    );
    assert_eq!(
        get::<isize>("arena.0.dirty_decay_ms"),
        cfg.dirty_decay_ms as isize
    );
    // A background thread is neither requested by default nor running.
    let _ = get::<bool>("background_thread");
    assert_eq!(set("background_thread", false), 0);
}

#[test]
fn stats_are_frozen_between_epochs_and_arenas_all_sums() {
    // Asserts that only its own `epoch` moves the epoch.
    let _serial = crate::testutil::global_state();
    let s = shared();
    // Other tests of this binary allocate and free concurrently, and
    // since `tcache_max` a freed 40 KiB block leaves `allocated` at
    // once instead of lingering in a cache, so the growth check retries
    // until a snapshot pair is not swamped by a neighbour's frees.
    let mut live: Vec<*mut u8> = Vec::new();
    let mut a1 = 0usize;
    for attempt in 0..20 {
        // Slots in this thread's cache (a previous attempt's frees)
        // still count as allocated; the blocks must come from the
        // slabs or `allocated` cannot grow.
        assert_eq!(call("thread.tcache.flush"), 0);
        let e0 = epoch();
        let a0: usize = get("stats.allocated");
        live = (0..300)
            .map(|_| {
                malloc(
                    200,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                )
            })
            .collect();
        assert_eq!(get::<usize>("stats.allocated"), a0, "no epoch, no change");
        let e1 = epoch();
        assert_eq!(e1, e0 + 1);
        assert_eq!(get::<u64>("epoch"), e1);
        a1 = get("stats.allocated");
        if a1 >= a0 + 300 * 200 {
            break;
        }
        assert!(attempt < 19, "allocated {a0} -> {a1} on every attempt");
        for p in live.drain(..) {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
    let active: usize = get("stats.active");
    let resident: usize = get("stats.resident");
    let mapped: usize = get("stats.mapped");
    let retained: usize = get("stats.retained");
    let metadata: usize = get("stats.metadata");
    assert!(resident >= active && active >= a1);
    assert_eq!(mapped, resident + retained);
    assert!(metadata > 0);
    // Per-arena figures sum to the totals through MALLCTL_ARENAS_ALL,
    // read by name and through a MIB with the index patched in.
    let mib = mib_of("stats.arenas.0.resident");
    let mut sum = 0usize;
    for i in 0..s.arenas.count() as usize {
        if s.arenas.get_existing(i as u16).is_none() {
            continue;
        }
        let mut m = mib.clone();
        m[2] = i;
        sum += get_mib::<usize>(&m);
    }
    assert_eq!(sum, resident);
    assert_eq!(get::<usize>("stats.arenas.4096.resident"), resident);
    assert_eq!(get::<usize>("stats.arenas.4096.mapped"), mapped);
    assert_eq!(
        get::<usize>("stats.arenas.4096.pactive") * s.regions.page_size(),
        active
    );
    assert_eq!(get::<usize>("stats.arenas.4096.pmuzzy"), 0);
    let small: usize = get("stats.arenas.4096.small.allocated");
    let large: usize = get("stats.arenas.4096.large.allocated");
    assert_eq!(small + large, a1);
    assert!(get::<usize>("stats.arenas.4096.small.resident") >= small);
    assert_eq!(call("stats.arenas.4000.pactive"), ENOENT);
    assert_eq!(call("stats.arenas.5000.pactive"), ENOENT);
    // Bin figures: the class of the 300 live blocks has them.
    let c = crate::layout::class_of(200).unwrap();
    let bin_mib = mib_of("stats.arenas.0.bins.0.curregs");
    let mut m = bin_mib.clone();
    m[2] = ARENAS_ALL as usize;
    m[4] = c;
    assert!(get_mib::<usize>(&m) >= 300);
    let curslabs: usize = get(&std::format!("stats.arenas.4096.bins.{c}.curslabs"));
    assert!(curslabs >= 1);
    assert!(get::<usize>(&std::format!("stats.arenas.4096.bins.{c}.nonfull_slabs")) <= curslabs);
    assert_eq!(
        call(&std::format!("stats.arenas.0.bins.{NBINS}.curregs")),
        ENOENT
    );
    for p in live {
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
}

/// `MALLCTL_ARENAS_DESTROYED` (4097, `ARENAS_DESTROYED`): the
/// statistics of the arenas destroyed so far are all zero, by name and
/// through a MIB with the index patched in as Valkey does, bins
/// included; the index is still no arena for `arena.4097.*`, and 4098
/// is nothing at all.
#[test]
fn destroyed_arenas_index_reads_zeros_and_is_no_arena() {
    for key in [
        "pactive",
        "pdirty",
        "pmuzzy",
        "small.allocated",
        "small.resident",
        "large.allocated",
        "resident",
        "mapped",
        "retained",
        "bins.0.curregs",
        "bins.0.curslabs",
        "bins.0.nonfull_slabs",
    ] {
        let name = std::format!("stats.arenas.4097.{key}");
        assert_eq!(get::<usize>(&name), 0, "{name}");
        assert_eq!(set(&name, 1usize), EPERM, "{name} is read-only");
        assert_eq!(
            call(&std::format!("stats.arenas.4098.{key}")),
            ENOENT,
            "{key}: 4098 is nothing"
        );
    }
    let c = crate::layout::class_of(200).unwrap();
    let mut m = mib_of("stats.arenas.0.bins.0.curregs");
    m[2] = ARENAS_DESTROYED;
    m[4] = c;
    assert_eq!(get_mib::<usize>(&m), 0);
    assert_eq!(
        call(&std::format!("stats.arenas.4097.bins.{NBINS}.curregs")),
        ENOENT,
        "past arenas.nbins"
    );
    for key in ["purge", "decay", "destroy", "dirty_decay_ms"] {
        assert_eq!(
            call(&std::format!("arena.4097.{key}")),
            ENOENT,
            "arena.4097.{key}"
        );
    }
    assert_eq!(set("arena.4097.dirty_decay_ms", 5isize), ENOENT);
    assert_eq!(set("thread.arena", ARENAS_DESTROYED as c_uint), EFAULT);
    assert_eq!(
        ARENAS_DESTROYED,
        ARENAS_ALL as usize + 1,
        "the headers' value"
    );
}

#[test]
fn arena_keys_create_lookup_purge_decay_and_destroy() {
    // Writes every arena's decay through `arena.4096`.
    let _serial = crate::testutil::global_state();
    let s = shared();
    let a: c_uint = get("arenas.create");
    assert!(a >= 1);
    assert!(s.arenas.get_existing(a as u16).is_some());
    assert!(get::<c_uint>("arenas.narenas") > a);
    let p = mallocx(
        500,
        mallocx_arena(a as u16) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    let h = mallocx(
        2 << 20,
        mallocx_arena(a as u16) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // arenas.lookup finds both tiers; garbage is EFAULT, not an abort.
    for q in [p, h] {
        let mut out: c_uint = 0;
        let cname = CString::new("arenas.lookup").unwrap();
        let mut len = size_of::<c_uint>();
        let qp: *const c_void = q as *const c_void;
        let rc = mallctl(
            cname.as_ptr(),
            &mut out as *mut c_uint as *mut c_void,
            &mut len,
            &qp as *const *const c_void as *const c_void,
            size_of::<*const c_void>(),
        );
        assert_eq!(rc, 0);
        assert_eq!(out, a);
    }
    let stack = 0u64;
    let sp: *const c_void = &stack as *const u64 as *const c_void;
    let mut out: c_uint = 0;
    assert_eq!(
        ctl_rw::<c_uint>("arenas.lookup", Some(&mut out), None),
        EINVAL,
        "lookup needs a pointer"
    );
    let cname = CString::new("arenas.lookup").unwrap();
    let mut len = size_of::<c_uint>();
    assert_eq!(
        mallctl(
            cname.as_ptr(),
            &mut out as *mut c_uint as *mut c_void,
            &mut len,
            &sp as *const *const c_void as *const c_void,
            size_of::<*const c_void>()
        ),
        EFAULT
    );
    // Decay period per arena, and for all.
    assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), 250isize), 0);
    assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 250);
    assert_eq!(
        s.arena(a as u16, vstd::prelude::Tracked::assume_new())
            .unwrap()
            .0
            .decay_ns(),
        250_000_000
    );
    assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), -1isize), 0);
    assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), -1);
    assert_eq!(
        set(&std::format!("arena.{a}.dirty_decay_ms"), -2isize),
        EFAULT
    );
    let default = get::<isize>("arena.0.dirty_decay_ms");
    assert_eq!(set("arena.4096.dirty_decay_ms", 777isize), 0);
    assert_eq!(get::<isize>("arena.0.dirty_decay_ms"), 777);
    assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 777);
    assert_eq!(set("arena.4096.dirty_decay_ms", default), 0);
    assert_eq!(get::<isize>("arena.4096.dirty_decay_ms"), default);
    // Free everything in the arena, then purge: its Empty slab goes.
    dallocx(
        p,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    dallocx(
        h,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let arena = s
        .arena(a as u16, vstd::prelude::Tracked::assume_new())
        .unwrap();
    let c = crate::layout::class_of(500).unwrap();
    // The slot sits in this thread's cache for arena `a` until the
    // purge flushes the caller.
    assert_eq!(arena.0.classes[c].stats().empty, 0);
    assert_eq!(call(&std::format!("arena.{a}.purge")), 0);
    assert_eq!(arena.0.classes[c].stats().empty, 0);
    // The first refill spanned `cap / 2` slots' worth of slabs (one
    // 128-slot slab at 512 bytes); all of them were flushed and purged.
    let refill_slabs =
        (crate::heap::default_cap(c) as usize / 2).div_ceil(crate::layout::CLASSES[c].nslots());
    assert_eq!(
        arena.0.classes[c].stats().purged,
        refill_slabs,
        "the flushed slabs were purged"
    );
    assert_eq!(call("arena.4096.purge"), 0);
    // jemalloc's legacy "all arenas" index, which Valkey's MEMORY PURGE uses.
    let narenas: c_uint = get("arenas.narenas");
    assert_eq!(call(&std::format!("arena.{narenas}.purge")), 0);
    assert_eq!(call(&std::format!("arena.{narenas}.decay")), 0);
    // The first unused index is ENOENT at this instant. Another api
    // test's `arenas.create` can land between the count read and the
    // call (the api tests share one process; on the 7.2 Chainguard OS
    // host `arena.{narenas+1}.purge` once answered 0, the moved count's
    // new all-arenas alias), so the pair is retried until the count did
    // not move across it; the far range below no concurrent test reaches.
    let mut stable = false;
    for _ in 0..8 {
        let n: c_uint = get("arenas.narenas");
        let rc = call(&std::format!("arena.{}.purge", n + 1));
        if get::<c_uint>("arenas.narenas") == n {
            assert_eq!(rc, ENOENT, "arena.{}.purge with {n} arenas", n + 1);
            stable = true;
            break;
        }
    }
    assert!(
        stable,
        "arenas.narenas moved across eight consecutive probes"
    );
    assert_eq!(call("arena.4000.purge"), ENOENT);
    assert_eq!(call("arena.5000.purge"), ENOENT);
    assert_eq!(call("arena.0.decay"), 0);
    assert_eq!(call(&std::format!("arena.{a}.decay")), 0);
    assert_eq!(call("arena.4096.decay"), 0);
    assert_eq!(call("arena.4000.decay"), ENOENT);
    // destroy: `EFAULT` for arena 0 and for `MALLCTL_ARENAS_ALL`,
    // `ENOENT` for an index that was never created. For an existing
    // arena challoc's `arena.<i>.destroy` is a flush of the caller's
    // caches plus a purge of that arena, nothing more: the arena index
    // stays registered, `arenas.narenas` does not move, and a later
    // `MALLOCX_ARENA(i)` allocation is served from it (jemalloc would
    // retire the index and, with live allocations, fail). Pinned here
    // so a change of these semantics has to change this test.
    assert_eq!(call("arena.0.destroy"), EFAULT);
    assert_eq!(call("arena.4096.destroy"), EFAULT);
    assert_eq!(call("arena.4000.destroy"), ENOENT);
    let narenas_before: c_uint = get("arenas.narenas");
    assert_eq!(call(&std::format!("arena.{a}.destroy")), 0);
    assert!(
        s.arenas.get_existing(a as u16).is_some(),
        "destroy keeps the arena registered"
    );
    assert!(
        get::<c_uint>("arenas.narenas") >= narenas_before,
        "destroy retires no index"
    );
    let again = mallocx(
        500,
        mallocx_arena(a as u16) as c_int,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(
        !again.is_null(),
        "the destroyed arena still serves MALLOCX_ARENA allocations"
    );
    assert_eq!(
        s.classify_free(again, vstd::prelude::Tracked::assume_new())
            .map(|x| x.1),
        Ok(a as u16)
    );
    assert_eq!(
        call(&std::format!("arena.{a}.destroy")),
        0,
        "destroy is repeatable"
    );
    dallocx(
        again,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
}

#[test]
fn thread_and_tcache_keys() {
    // Its `tcache.destroy` checks that the id names no cache after;
    // `api::heap_at_tests` creates explicit caches under this guard.
    let _serial = crate::testutil::global_state();
    let s = shared();
    let h = heap::heap_for_thread(s, Tracked::assume_new()).unwrap();
    let before = h.stats();
    let a0: u64 = get("thread.allocated");
    let d0: u64 = get("thread.deallocated");
    assert_eq!(a0, before.allocated);
    assert_eq!(d0, before.deallocated);
    let ap: *const u64 = get("thread.allocatedp");
    let dp: *const u64 = get("thread.deallocatedp");
    assert!(!ap.is_null() && !dp.is_null());
    // SAFETY: `thread.allocatedp` points at this thread's counter, live
    // while the thread runs.
    assert_eq!(unsafe { *ap }, a0);
    let p = malloc(
        1000,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let size = crate::layout::class_size_of(1000).unwrap() as u64;
    // The pointer is stale until a counter is read again.
    // SAFETY: as above.
    assert_eq!(unsafe { *ap }, a0);
    assert_eq!(get::<u64>("thread.allocated"), a0 + size);
    // SAFETY: as above.
    assert_eq!(unsafe { *ap }, a0 + size, "materialised by the read");
    free(
        p,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(get::<u64>("thread.deallocated"), d0 + size);
    // SAFETY: as above, for `thread.deallocatedp`.
    assert_eq!(unsafe { *dp }, d0 + size);
    // thread.tcache.flush empties this thread's caches.
    let many: Vec<*mut u8> = (0..64)
        .map(|_| {
            malloc(
                48,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        })
        .collect();
    for q in many {
        free(
            q,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    let c = crate::layout::class_of(48).unwrap();
    let h = heap::heap_for_thread(s, Tracked::assume_new()).unwrap();
    assert!(h.cache(c).map_or(0, |cc| cc.len()) >= 64);
    assert_eq!(call("thread.tcache.flush"), 0);
    assert_eq!(h.cache(c).map_or(0, |cc| cc.len()), 0);
    // thread.tcache.enabled: off frees the caches and serves directly;
    // counters keep counting; on brings caches back.
    assert!(get::<bool>("thread.tcache.enabled"));
    let a_before: u64 = get("thread.allocated");
    assert_eq!(set("thread.tcache.enabled", false), 0);
    assert!(!get::<bool>("thread.tcache.enabled"));
    let h = heap::heap_for_thread(s, Tracked::assume_new()).unwrap();
    assert!(h.cache(c).is_none(), "caches were freed");
    assert_eq!(get::<u64>("thread.allocated"), a_before);
    let q = malloc(
        48,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(h.cache(c).is_none(), "no cache is created while disabled");
    assert_eq!(get::<u64>("thread.allocated"), a_before + 48);
    free(
        q,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(h.cache(c).is_none());
    assert_eq!(set("thread.tcache.enabled", true), 0);
    let q = malloc(
        48,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(h.cache(c).is_some(), "caches come back");
    free(
        q,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // thread.arena moves the home arena.
    let home: c_uint = get("thread.arena");
    assert_eq!(home as u16, h.arena());
    let a: c_uint = get("arenas.create");
    assert_eq!(set("thread.arena", a), 0);
    assert_eq!(get::<c_uint>("thread.arena"), a);
    // Plain malloc comes from that arena through the thread's own caches.
    let q = malloc(
        200,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        s.classify_free(q, vstd::prelude::Tracked::assume_new())
            .map(|x| x.1),
        Ok(a as u16)
    );
    free(
        q,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(set("thread.arena", 4000 as c_uint), EFAULT);
    assert_eq!(set("thread.arena", 5000 as c_uint), EFAULT);
    assert_eq!(set("thread.arena", home), 0);
    assert_eq!(get::<c_uint>("thread.arena"), home);
    // Explicit caches.
    let t: c_uint = get("tcache.create");
    let flags = mallocx_tcache(t as u16) as c_int;
    let e = mallocx(
        200,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let c = crate::layout::class_of(200).unwrap();
    dallocx(
        e,
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let th = s.tcaches.get(t as u16, Tracked::assume_new()).unwrap();
    assert!(th.cache_in(th.arena(), c).unwrap().len() >= 1);
    assert_eq!(set("tcache.flush", t), 0);
    assert_eq!(th.cache_in(th.arena(), c).unwrap().len(), 0);
    assert_eq!(set("tcache.destroy", t), 0);
    assert!(s.tcaches.get(t as u16, Tracked::assume_new()).is_none());
    assert_eq!(set("tcache.destroy", t), EFAULT);
    assert_eq!(set("tcache.flush", t), EFAULT);
}

#[test]
fn utilization_queries_answer_valkeys_defragmenter() {
    let s = shared();
    let c = crate::layout::class_of(100).unwrap();
    let cls = &CLASSES[c];
    // A private arena keeps the slabs, and so the nfree counts this
    // test compares, its own: in arena 0 or a home arena, another
    // test's allocation of this class can take free slots between
    // the batch query and the read-back.
    let first = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .expect("arenas.create");
    let flags = (mallocx_arena(first) | MALLOCX_TCACHE_NONE) as c_int;
    let ptrs: Vec<*mut u8> = (0..500)
        .map(|_| {
            mallocx(
                100,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        })
        .collect();
    assert_eq!(
        owner_of(s, ptrs[0] as *const c_void, Tracked::assume_new()),
        Some(first)
    );
    // Verbose query on one pointer.
    let cname = CString::new("experimental.utilization.query").unwrap();
    let mut u = Utilization::ZERO;
    let mut len = size_of::<Utilization>();
    let p0: *const c_void = ptrs[0] as *const c_void;
    let rc = mallctl(
        cname.as_ptr(),
        &mut u as *mut Utilization as *mut c_void,
        &mut len,
        &p0 as *const *const c_void as *const c_void,
        size_of::<*const c_void>(),
    );
    assert_eq!(rc, 0);
    // Slab pages (docs/proposals/slab-pages.md, 3.6): the slab's
    // regions are its open slots, opened on demand by the 500 claims.
    let open = |p: *const c_void| {
        let t = s.table(c);
        let (slab, _) = t.locate(p as *mut u8);
        let closed = t
            .pages(slab)
            .map_or(0, |g| t.page_view().with_closed(g.closed()).closed_slots());
        cls.nslots() - closed
    };
    assert_eq!(u.nregs, open(p0));
    assert!(u.nregs <= cls.nslots());
    assert_eq!(
        u.size,
        u.nregs * cls.size(),
        "region bytes, so size / nregs is the class size"
    );
    assert!(u.nfree < u.nregs);
    assert!(u.bin_nregs >= u.nregs);
    assert!(u.bin_nfree <= u.bin_nregs);
    assert!(
        u.bin_nregs - u.bin_nfree >= 500,
        "at least our 500 slots are out"
    );
    if !u.slabcur_addr.is_null() {
        assert!(
            matches!(s.regions.classify(u.slabcur_addr as usize), Kind::Slab { class, slot: 0, .. } if class as usize == c)
        );
    }
    assert_eq!(size_of::<Utilization>(), 6 * size_of::<usize>());
    // Batch query as Valkey does it: nametomib, then bymib with the
    // pointer array in and 3-tuples out.
    let mib = mib_of("experimental.utilization.batch_query");
    let mut out = std::vec![0usize; 3 * ptrs.len()];
    let mut outlen = out.len() * size_of::<usize>();
    let rc = mallctlbymib(
        mib.as_ptr(),
        mib.len(),
        out.as_mut_ptr() as *mut c_void,
        &mut outlen,
        ptrs.as_ptr() as *const c_void,
        ptrs.len() * size_of::<*mut u8>(),
    );
    assert_eq!(rc, 0);
    for (i, p) in ptrs.iter().enumerate() {
        let (nfree, nregs, size) = (out[3 * i], out[3 * i + 1], out[3 * i + 2]);
        let n = open(*p as *const c_void);
        assert_eq!(nregs, n, "ptr {i}");
        assert_eq!(size, n * cls.size(), "ptr {i}");
        let (slab, _) = s.table(c).locate(*p);
        assert_eq!(
            nfree,
            s.table(c)
                .desc(slab)
                .nfree(cls)
                .saturating_sub(cls.nslots() - n),
            "ptr {i}"
        );
    }
    // Size mismatches are EINVAL; the pointer array is required.
    let mut short = outlen - size_of::<usize>();
    assert_eq!(
        mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            out.as_mut_ptr() as *mut c_void,
            &mut short,
            ptrs.as_ptr() as *const c_void,
            ptrs.len() * size_of::<*mut u8>()
        ),
        EINVAL
    );
    assert_eq!(
        mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            out.as_mut_ptr() as *mut c_void,
            &mut outlen,
            ptr::null(),
            0
        ),
        EINVAL
    );
    // A huge run is one region of its own, and so is a slot of a class
    // above BIN_MAX (reported like jemalloc's large extents); a
    // non-allocation is zeros.
    let h = mallocx(
        3 << 20,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let m = mallocx(
        100 << 10,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let cm = crate::layout::class_of(100 << 10).unwrap();
    assert!((NBINS..crate::layout::NCLASSES).contains(&cm));
    let stack = 0u64;
    let four = [
        h as *const c_void,
        &stack as *const u64 as *const c_void,
        ptrs[1] as *const c_void,
        m as *const c_void,
    ];
    let mut out4 = [0usize; 12];
    let mut len4 = out4.len() * size_of::<usize>();
    assert_eq!(
        mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            out4.as_mut_ptr() as *mut c_void,
            &mut len4,
            four.as_ptr() as *const c_void,
            four.len() * size_of::<*const c_void>()
        ),
        0
    );
    assert_eq!(&out4[..3], &[0, 1, s.huge.usable_for(3 << 20).unwrap()]);
    assert_eq!(&out4[3..6], &[0, 0, 0]);
    assert_eq!(out4[7], open(ptrs[1] as *const c_void));
    assert_eq!(&out4[9..12], &[0, 1, CLASSES[cm].size()]);
    let um = utilization(s, m as *const c_void, true, Tracked::assume_new());
    assert_eq!(
        (um.nfree, um.nregs, um.size, um.bin_nfree, um.bin_nregs),
        (0, 1, CLASSES[cm].size(), 0, 0)
    );
    assert!(um.slabcur_addr.is_null());
    // A class above tcache_max comes from arena 0 on the default path
    // whatever the thread's arena (design 13.3); a cached class from
    // the thread's own arena.
    assert_eq!(
        owner_of(s, m as *const c_void, Tracked::assume_new()),
        Some(0)
    );
    let home = crate::heap::existing_heap(s).unwrap().arena();
    let mid = mallocx(
        8 << 10,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(crate::layout::class_of(8 << 10).unwrap() >= NBINS);
    assert_eq!(
        owner_of(s, mid as *const c_void, Tracked::assume_new()),
        Some(home)
    );
    dallocx(
        mid,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    dallocx(
        m,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    dallocx(
        h,
        0,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // A freed slot (direct release, so the bit is clear) is zeros too.
    let gone = ptrs[2] as *const c_void;
    dallocx(
        ptrs[2],
        flags,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert_eq!(
        utilization(s, gone, true, Tracked::assume_new()),
        Utilization::ZERO
    );
    assert_eq!(owner_of(s, gone, Tracked::assume_new()), None);
    assert_eq!(
        owner_of(s, ptrs[3] as *const c_void, Tracked::assume_new()),
        Some(first)
    );
    for (i, p) in ptrs.into_iter().enumerate() {
        if i != 2 {
            dallocx(
                p,
                flags,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
}

/// The mallctl write/read contract: an error leaves allocator state
/// unchanged, and with both `oldp` and `newp` the old value is read
/// before the new one is applied (as jemalloc 5.3.0 reads first in
/// `arena_i_decay_ms_ctl_impl` and `thread_arena_ctl`, captures the old
/// value in `thread_tcache_enabled_ctl`, and checks `*oldlenp` with
/// `VERIFY_READ` in `arenas_create_ctl` and `tcache_create_ctl`).
fn ctl_raw<T: Copy, U: Copy>(name: &str, old: &mut T, oldlen: usize, new: Option<&U>) -> c_int {
    let cname = CString::new(name).unwrap();
    let mut len = oldlen;
    let (newp, newlen) = match new {
        Some(n) => (n as *const U as *const c_void, size_of::<U>()),
        None => (ptr::null(), 0),
    };
    mallctl(
        cname.as_ptr(),
        old as *mut T as *mut c_void,
        &mut len,
        newp,
        newlen,
    )
}

#[test]
fn contract_dirty_decay_ms_error_leaves_period_unchanged() {
    // `arena.4096.dirty_decay_ms` writes from other tests set every
    // arena's period.
    let _serial = crate::testutil::global_state();
    let a: c_uint = get("arenas.create");
    let key = std::format!("arena.{a}.dirty_decay_ms");
    assert_eq!(set(&key, 250isize), 0);
    // Wrong `*oldlenp` with a valid new value: EINVAL, so nothing
    // may change.
    let mut old = 0u32;
    let rc = ctl_raw::<u32, isize>(&key, &mut old, size_of::<u32>(), Some(&313isize));
    assert_eq!(rc, EINVAL);
    assert_eq!(get::<isize>(&key), 250, "an EINVAL read applied the write");
}

#[test]
fn contract_dirty_decay_ms_reads_the_old_period() {
    let _serial = crate::testutil::global_state();
    let a: c_uint = get("arenas.create");
    let key = std::format!("arena.{a}.dirty_decay_ms");
    assert_eq!(set(&key, 250isize), 0);
    let mut old = 0isize;
    let rc = ctl_raw::<isize, isize>(&key, &mut old, size_of::<isize>(), Some(&313isize));
    assert_eq!(rc, 0);
    assert_eq!(old, 250, "oldp got the new period, not the old one");
}

#[test]
fn contract_arenas_create_error_creates_nothing() {
    // A sentinel no arena index has: a create that went ahead would
    // copy the low half of its new index over it.
    let mut old = u16::MAX;
    let rc = ctl_raw::<u16, u8>("arenas.create", &mut old, size_of::<u16>(), None);
    assert_eq!(rc, EINVAL);
    assert_eq!(
        old,
        u16::MAX,
        "an EINVAL arenas.create created an arena and copied its index"
    );
}

#[test]
fn contract_tcache_create_error_creates_nothing() {
    // A sentinel no tcache ID has, as for `arenas.create`.
    let mut old = u16::MAX;
    let rc = ctl_raw::<u16, u8>("tcache.create", &mut old, size_of::<u16>(), None);
    assert_eq!(rc, EINVAL);
    assert_eq!(
        old,
        u16::MAX,
        "an EINVAL tcache.create created a tcache and copied its ID"
    );
}

#[test]
fn contract_thread_tcache_enabled_reads_the_old_value() {
    assert!(get::<bool>("thread.tcache.enabled"));
    let mut old = true;
    let rc = ctl_raw::<bool, bool>(
        "thread.tcache.enabled",
        &mut old,
        size_of::<bool>(),
        Some(&false),
    );
    assert_eq!(rc, 0);
    assert_eq!(set("thread.tcache.enabled", true), 0);
    assert!(old, "oldp got the new value, not the old one");
}

#[test]
fn contract_thread_arena_reads_the_old_value() {
    let home: c_uint = get("thread.arena");
    let a: c_uint = get("arenas.create");
    let mut old: c_uint = c_uint::MAX;
    let rc = ctl_raw::<c_uint, c_uint>("thread.arena", &mut old, size_of::<c_uint>(), Some(&a));
    assert_eq!(rc, 0);
    assert_eq!(set("thread.arena", home), 0);
    assert_eq!(old, home, "oldp got the new arena, not the old one");
}

#[test]
fn contract_background_thread_error_leaves_request_unchanged() {
    let _serial = crate::testutil::global_state();
    let before = purge::background::requested();
    // A wrong `*oldlenp` with a valid new value: the read fails before
    // the request is made, so no thread is started either.
    let mut old = 0u32;
    let rc = ctl_raw::<u32, bool>("background_thread", &mut old, size_of::<u32>(), Some(&true));
    assert_eq!(rc, EINVAL);
    assert_eq!(
        purge::background::requested(),
        before,
        "an EINVAL read made the request"
    );
}

#[test]
fn background_thread_enable_racing_a_start_in_progress_succeeds() {
    use core::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    let _serial = crate::testutil::global_state();
    assert!(!purge::background::present());
    // A second enable, from another thread, while the first one's
    // `start` is between creating the thread and publishing it: that
    // `start` finds the creation in progress, and the enable returns
    // 0, as jemalloc's does once the thread runs.
    let racing = Arc::new(AtomicI32::new(-1));
    let racing_rc = Arc::clone(&racing);
    *purge::background::BEFORE_PUBLISH.lock().unwrap() = Some(std::boxed::Box::new(move || {
        // No panics here: unwinding out of `start` would leave the
        // state marked as creating for every later test.
        let rc = std::thread::spawn(|| set("background_thread", true))
            .join()
            .unwrap_or(-2);
        racing_rc.store(rc, Ordering::SeqCst);
    }));
    let rc = set("background_thread", true);
    // Another test's allocation tick may have become the creator and
    // run the hook; wait for it before taking the hook back.
    for _ in 0..400 {
        if racing.load(Ordering::SeqCst) != -1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    purge::background::BEFORE_PUBLISH.lock().unwrap().take();
    let present = purge::background::present();
    assert_eq!(set("background_thread", false), 0);
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
    assert_eq!(rc, 0, "the enable that created the thread");
    assert_eq!(
        racing.load(Ordering::SeqCst),
        0,
        "the enable racing the creation (-1: the hook did not run)"
    );
    assert!(present, "background thread did not start");
    assert!(stopped, "background thread did not stop");
}

#[test]
fn background_thread_enable_whose_thread_exits_before_it_publishes_fails() {
    use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};
    use std::sync::Arc;
    let _serial = crate::testutil::global_state();
    // A disable, through `mallctl` on another thread, while the
    // enable's `start` is between creating the thread and publishing
    // it, and the thread exits first: when that `start` returns
    // false, no thread is present or being created, so the enable
    // fails with EAGAIN. Another test's allocation tick may create a
    // thread in the same round (before the enable's `start`, so that
    // it runs the hook, or after the hook's disable, from a request it
    // read before it); such a round is cleaned up and run again, so
    // the checked round's only creation is the enable's.
    let mut checked = None;
    for _ in 0..20 {
        assert!(!purge::background::present());
        // 0: the hook did not run, 1: it ran on this thread and the
        // thread exited, 2: it ran here and the thread did not exit,
        // 3: it ran on another thread.
        let hook = Arc::new(AtomicU8::new(0));
        let hook_seen = Arc::clone(&hook);
        // `EXITS` once the hook saw the thread exit.
        let exited_at = Arc::new(AtomicU32::new(0));
        let exited_at_seen = Arc::clone(&exited_at);
        let me = std::thread::current().id();
        let exits = purge::background::EXITS.load(Ordering::SeqCst);
        *purge::background::BEFORE_PUBLISH.lock().unwrap() =
            Some(std::boxed::Box::new(move || {
                // No panics here: unwinding out of `start` would leave
                // the state marked as creating for every later test.
                if std::thread::current().id() != me {
                    hook_seen.store(3, Ordering::SeqCst);
                    return;
                }
                let _ = std::thread::spawn(|| set("background_thread", false)).join();
                let mut seen = 2;
                for _ in 0..1000 {
                    let now = purge::background::EXITS.load(Ordering::SeqCst);
                    if now != exits {
                        exited_at_seen.store(now, Ordering::SeqCst);
                        seen = 1;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                hook_seen.store(seen, Ordering::SeqCst);
            }));
        let rc = set("background_thread", true);
        let present = purge::background::present();
        // Still installed: no creation reached the hook (another
        // creator is still before it, or the creation failed).
        let unused = purge::background::BEFORE_PUBLISH
            .lock()
            .unwrap()
            .take()
            .is_some();
        // Taken: wait for the creator that took it to report.
        if !unused {
            for _ in 0..1200 {
                if hook.load(Ordering::SeqCst) != 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let ran = hook.load(Ordering::SeqCst);
        // Leave no thread, and no creation in progress, for the next
        // round or test.
        assert_eq!(set("background_thread", false), 0);
        for _ in 0..400 {
            if !purge::background::present_or_creating(Tracked::assume_new()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let stopped = !purge::background::present_or_creating(Tracked::assume_new());
        if stopped {
            purge::background::join_exited();
        }
        // Another creation in this round made a thread that exited
        // after the hook's.
        let other = ran == 1
            && purge::background::EXITS.load(Ordering::SeqCst) != exited_at.load(Ordering::SeqCst);
        // A round that did not stop is checked: running another
        // would start from a thread left over.
        if !stopped || (!unused && ran != 3 && !other) {
            checked = Some((ran, rc, present, stopped));
            break;
        }
    }
    let (ran, rc, present, stopped) = checked.expect("no round had the enable as the only creator");
    assert_eq!(
        ran, 1,
        "hook: 0 did not run, 2 the thread did not exit before publication"
    );
    assert!(!present, "start published a thread that had already exited");
    assert_eq!(
        rc, EAGAIN,
        "the enable whose thread exited before its start published it"
    );
    assert!(stopped, "background thread did not stop");
}

/// The verified reads that rest on premise C2 of
/// `verus/kani/premises.md` (the C program's name, `newp` and MIB
/// bytes are initialised and unchanged by any other thread during the
/// call). In verified code (inside `verus!`, in a function that is not
/// `external_body`, `external`, `cfg(test)` or `cfg(kani)`, in the
/// files `testutil::shipped_sources` returns), every use of
/// `from_raw_parts` and `read_unaligned` is in one of the four helpers
/// listed, as many times as listed; each helper is used only in the
/// functions listed: `name_bytes` in `cstr`, which `mallctl` and
/// `mallctlnametomib` call on their name; `load` in `Io::write` and
/// `query_ptr` in `batch_query`, on the call's `newp`; `mib_in` in
/// `mallctlbymib` (and the theorems that restate those entry points'
/// bodies); and the helpers keep the contracts that tie what they read
/// to those buffers (`c_readable` of the caller's buffer, and for
/// `name_bytes` the call's string, with the slice borrowed from the
/// call's token). The helpers are private to `mallctl.rs`, so their uses
/// are looked for there only: every mention of the name but a method
/// (`.name`), a `fn` item, or a path to another module's item
/// (`m::name` but for `self::`, `super::` or `mallctl::`). Known
/// escapes: a rename through a `use .. as` outside `verus!` (its later
/// uses are not seen), a use that a `macro_rules!` defined outside
/// `verus!` expands to, and a kept clause moved or weakened inside its
/// helper's item (`CONTRACTS` matches text from the `fn` line to the
/// body's `{`). `verus/README.md`'s guard-test row records the mutants
/// this test kills.
#[test]
fn c_buffer_reads_are_listed() {
    use crate::testutil::{fn_item, shipped_sources};
    const READS: [&str; 2] = ["from_raw_parts", "read_unaligned"];
    const HELPERS: [&str; 5] = ["name_bytes", "cstr", "load", "query_ptr", "mib_in"];
    const HERE: &str = "api/mallctl.rs";
    // (the function in `mallctl.rs`, the name it uses, uses).
    const SITES: [(&str, &str, usize); 12] = [
        ("name_bytes", "from_raw_parts", 1),
        ("load", "read_unaligned", 1),
        ("query_ptr", "read_unaligned", 1),
        ("mib_in", "read_unaligned", 1),
        ("cstr", "name_bytes", 1),
        ("mallctl", "cstr", 1),
        ("mallctlnametomib", "cstr", 1),
        ("theorem_mallctl_reads_never_abort", "cstr", 1),
        ("write", "load", 1),
        ("batch_query", "query_ptr", 1),
        ("mallctlbymib", "mib_in", 1),
        ("theorem_mallctlbymib_reads_never_abort", "mib_in", 1),
    ];
    // (helper, a clause of its item, from its `fn` line to its body's
    // `{`, that ties its read to the C buffer).
    const CONTRACTS: [(&str, &str); 5] = [
        (
            "name_bytes",
            "Tracked(call): Tracked<&CCall>) -> (r: &[u8])",
        ),
        ("name_bytes", "call.reads_str(p),"),
        ("load", "c_readable(newp as *mut u8, newlen as int),"),
        ("query_ptr", "c_readable(ptrs as *mut u8, newlen as int),"),
        (
            "mib_in",
            "c_readable(mib as *mut usize, miblen as int * size_of::<usize>()),",
        ),
    ];
    // The name a `fn` item line declares.
    let fn_name = |sig: &str| -> std::string::String {
        let rest = sig.split_once("fn ").map_or("", |(_, r)| r);
        rest.split(['(', '<']).next().unwrap().into()
    };
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut found = Vec::new();
    let mut items: Vec<(std::string::String, std::string::String)> = Vec::new();
    for (rel, text) in shipped_sources() {
        let mut depth = 0usize;
        let mut attrs: Vec<&str> = Vec::new();
        let mut sig = "";
        let mut verified = false;
        // The item of the helper whose `fn` line was seen last, until
        // its body's `{`.
        let mut item: Option<(std::string::String, std::string::String)> = None;
        for (i, line) in text.lines().enumerate() {
            let mut t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if t.ends_with("verus! {") {
                depth += 1;
                // Code before the block's first `fn` counts as verified.
                (sig, verified) = ("", true);
            } else if t == "} // verus!" {
                depth = depth
                    .checked_sub(1)
                    .unwrap_or_else(|| panic!("{rel}:{}: a `verus!` close with none open", i + 1));
            }
            // Attributes, on lines of their own or before an item on
            // the same line.
            while t.starts_with("#[") {
                let Some(end) = t.find(']') else { break };
                attrs.push(&t[..=end]);
                t = t[end + 1..].trim_start();
            }
            if t.is_empty() {
                continue;
            }
            if fn_item(t) {
                sig = t;
                verified = !attrs.iter().any(|a| {
                    a.contains("verifier::external_body")
                        || a.contains("verifier::external]")
                        || a.contains("cfg(test)")
                        || a.contains("cfg(kani)")
                });
                let f = fn_name(sig);
                if rel == HERE && HELPERS.contains(&f.as_str()) {
                    item = Some((f, std::string::String::new()));
                }
            }
            attrs.clear();
            if let Some((f, s)) = &mut item {
                s.push_str(t);
                s.push('\n');
                if t == "{" || t.ends_with(" {") {
                    items.push((core::mem::take(f), core::mem::take(s)));
                    item = None;
                }
            }
            if depth == 0 || !verified || t.contains("assume_specification") {
                continue;
            }
            let names = READS.iter().map(|n| (*n, false));
            let helpers = HELPERS.iter().filter(|_| rel == HERE).map(|n| (*n, true));
            for (name, helper) in names.chain(helpers) {
                for (k, _) in t.match_indices(name) {
                    let (before, after) = (&t[..k], &t[k + name.len()..]);
                    if before.ends_with(ident) || after.starts_with(ident) {
                        continue;
                    }
                    let other_path = before.ends_with("::")
                        && !["self::", "super::", "mallctl::"]
                            .iter()
                            .any(|p| before.ends_with(p));
                    if helper && (before.ends_with('.') || before.ends_with("fn ") || other_path) {
                        continue;
                    }
                    found.push((
                        rel.clone(),
                        fn_name(sig),
                        name,
                        std::format!("{rel}:{}: `{t}`", i + 1),
                    ));
                }
            }
        }
        assert_eq!(depth, 0, "{rel}: a `verus!` block left open");
    }
    for (rel, f, name, at) in &found {
        assert!(
            rel == HERE && SITES.iter().any(|(s, n, _)| f == s && name == n),
            "{at}: `{name}` in `{f}`, which is not a listed site. A verified read \
             through it rests on premise C2 only for the C program's name, `newp` \
             and MIB: argue the new site in verus/kani/premises.md and add it \
             here, in one change."
        );
    }
    for (s, n, want) in SITES {
        let got = found
            .iter()
            .filter(|(_, f, name, _)| f == s && *name == n)
            .count();
        assert_eq!(
            got, want,
            "{HERE} `{s}`: got = {got} uses of `{n}`, want = {want}"
        );
    }
    for (f, clause) in CONTRACTS {
        let got: Vec<_> = items.iter().filter(|(g, _)| g == f).collect();
        assert_eq!(
            got.len(),
            1,
            "{HERE} `{f}`: got = {} items, want = 1",
            got.len()
        );
        assert!(
            got[0].1.contains(clause),
            "{HERE} `{f}`: got = item\n{}\nwant = one with `{clause}`, which ties its read to \
             the C buffer premise C2 covers",
            got[0].1
        );
    }
}
