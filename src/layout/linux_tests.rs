//! Layout tests on real address space: `Regions::reserve` lays out and
//! randomises the regions and commits a slab, and `seal` locks the gaps
//! while the regions stay usable. Pure layout arithmetic belongs in
//! `tests.rs`.

use super::*;
use crate::sys::{
    self, Errno, MADV_DONTNEED, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, PROT_NONE, PROT_READ,
    PROT_WRITE,
};
use std::sync::Mutex;
use std::{eprintln, string::String};

/// One full layout is up to 53 TiB of address space; two live at once
/// do not fit in x86_64's 47-bit user space, and Rosetta traps instead of
/// returning `ENOMEM` when it cannot place a mapping. The allocator only
/// ever reserves once per process, so the tests take turns too.
static RESERVE_LOCK: Mutex<()> = Mutex::new(());

fn maps() -> String {
    std::fs::read_to_string("/proc/self/maps").unwrap()
}

/// True when `m` lies inside one `/proc/self/maps` entry with `perms`.
/// Containment rather than equality: adjacent anonymous mappings with
/// identical flags merge into one VMA, so our three reservations may show
/// up as a single line.
fn has_mapping(maps: &str, m: Mapping, perms: &str) -> bool {
    maps.lines().any(|l| {
        let mut fields = l.split(' ');
        let Some(range) = fields.next() else {
            return false;
        };
        if fields.next() != Some(perms) {
            return false;
        }
        let Some((lo, hi)) = range.split_once('-') else {
            return false;
        };
        let (Ok(lo), Ok(hi)) = (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
        else {
            return false;
        };
        lo <= m.addr && m.addr + m.len <= hi
    })
}

#[test]
fn reserve_lays_out_regions_and_commits_a_slab() {
    let _guard = RESERVE_LOCK.lock().unwrap();
    let page = sys::features::page_size();
    let r = Regions::reserve(page, true);
    eprintln!(
        "layout: lg_region={} lg_stride={} huge=2^{} meta=2^{} superblock={:#x}",
        r.lg_region,
        r.lg_stride,
        r.huge_len.trailing_zeros(),
        r.meta_len.trailing_zeros(),
        r.superblock
    );
    assert!(r.lg_region >= LG_REGION_MIN && r.lg_region <= LG_REGION_PREFERRED);
    assert!(r.lg_stride == r.lg_region + 2 || r.lg_stride == r.lg_region + 1);
    assert!(r.huge_len >= 1 << LG_HUGE_MIN && r.meta_len >= 1 << LG_META_MIN);
    assert_eq!(r.page_size(), page);

    // Everything is inside its mapping, and the mappings are PROT_NONE.
    assert!(r.superblock >= r.superblock_map.addr);
    assert!(
        r.superblock + (NCLASSES << r.lg_stride) <= r.superblock_map.addr + r.superblock_map.len
    );
    assert!(
        r.huge_base >= r.huge_map.addr
            && r.huge_base + r.huge_len <= r.huge_map.addr + r.huge_map.len
    );
    assert!(
        r.meta_base >= r.meta_map.addr
            && r.meta_base + r.meta_len <= r.meta_map.addr + r.meta_map.len
    );
    // Every region has at least `MAX_SLAB` of never-committed slack below
    // it inside its own reservation, whatever the random offsets: a
    // backward overflow off a region's first page faults in that slack
    // instead of running into whatever the kernel mapped just below.
    assert!(r.huge_base >= r.huge_map.addr + MAX_SLAB);
    assert!(r.meta_base >= r.meta_map.addr + MAX_SLAB);
    assert!(r.class_base[0] >= r.superblock_map.addr + MAX_SLAB);
    // The init check sees it, and would see a region flush with an edge.
    assert!(r.slack_ok());
    let flush: [fn(&mut Regions); 5] = [
        |r| r.huge_base = r.huge_map.addr,
        |r| r.meta_base = r.meta_map.addr + r.meta_map.len - r.meta_len,
        |r| r.class_base[0] = r.superblock_map.addr,
        |r| r.class_base[1] = r.class_base[0] + r.region_len(),
        |r| {
            r.class_base[NCLASSES - 1] =
                r.superblock_map.addr + r.superblock_map.len - r.region_len()
        },
    ];
    for (i, f) in flush.iter().enumerate() {
        let mut bad = r;
        f(&mut bad);
        assert!(!bad.slack_ok(), "flush case {i} passed slack_ok");
    }
    assert_eq!(r.superblock & (MAX_SLAB - 1), 0);
    assert_eq!(r.huge_base & (MAX_SLAB - 1), 0);
    assert_eq!(r.meta_base & (MAX_SLAB - 1), 0);
    let m = maps();
    for map in [r.superblock_map, r.huge_map, r.meta_map] {
        assert!(
            has_mapping(&m, map, "---p"),
            "missing PROT_NONE mapping {map:x?} in:\n{m}"
        );
    }

    // Classes are where the arithmetic says, and nowhere else.
    for c in 0..NCLASSES {
        let base = r.class_base[c];
        // A `MAX_SLAB` boundary plus the class's colour (none under
        // `thp:always` chunks, which another test may have configured).
        let lg_slab = CLASSES[c].lg_slab as u32;
        let colour = regions::colour(c, lg_slab, r.lg_page);
        assert!(
            base & (MAX_SLAB - 1) == colour || base & (MAX_SLAB - 1) == 0,
            "class {c}: base {base:#x}, colour {colour:#x}"
        );
        assert_eq!(base & ((1usize << lg_slab) - 1), 0);
        let start = r.superblock + (c << r.lg_stride);
        assert!(base >= start && base + r.region_len() <= start + r.stride());
        assert_eq!(
            r.classify(base),
            Kind::Slab {
                class: c as u32,
                slab: 0,
                slot: 0
            }
        );
        assert_eq!(r.classify(base + r.region_len()), Kind::Invalid);
    }

    // Committing one slab is an mprotect on the reservation; it reads
    // zero and holds what we write.
    let c = NCLASSES / 2;
    let slab = r.slab_addr(c, 3);
    let len = CLASSES[c].slab_size();
    // SAFETY: the range is inside our own PROT_NONE reservation.
    unsafe { sys::mprotect(slab, len, PROT_READ | PROT_WRITE) }.unwrap();
    let p = r.slot_addr(c, 3, 2) as *mut u64;
    // SAFETY: the slab was just committed read/write.
    unsafe {
        assert_eq!(p.read(), 0);
        p.write(0xDEAD_BEEF);
        assert_eq!(p.read(), 0xDEAD_BEEF);
    }
    assert_eq!(
        r.classify(p as usize),
        Kind::Slab {
            class: c as u32,
            slab: 3,
            slot: 2
        }
    );
    // The commit left the rest of the mapping PROT_NONE.
    let m = maps();
    assert!(
        has_mapping(&m, Mapping { addr: slab, len }, "rw-p"),
        "committed slab not rw-p in:\n{m}"
    );

    // SAFETY: nothing references the regions any more.
    unsafe { r.release() };
    let m = maps();
    assert!(!has_mapping(&m, r.superblock_map, "---p"));
}

#[test]
fn reserve_randomises_class_offsets_and_order() {
    let _guard = RESERVE_LOCK.lock().unwrap();
    let page = sys::features::page_size();
    // Two sequential reservations: offsets inside the superblock and the
    // slides of the huge and metadata bases must not repeat.
    let a = Regions::reserve(page, false);
    let a_offsets: [usize; NCLASSES] =
        core::array::from_fn(|c| a.class_base[c] - (a.superblock + (c << a.lg_stride)));
    let a_slides = (a.huge_base - a.huge_map.addr, a.meta_base - a.meta_map.addr);
    // SAFETY: nothing references the regions any more.
    unsafe { a.release() };
    let b = Regions::reserve(page, false);
    let b_offsets: [usize; NCLASSES] =
        core::array::from_fn(|c| b.class_base[c] - (b.superblock + (c << b.lg_stride)));
    let b_slides = (b.huge_base - b.huge_map.addr, b.meta_base - b.meta_map.addr);
    // SAFETY: nothing references the regions any more.
    unsafe { b.release() };
    let differing = (0..NCLASSES)
        .filter(|&c| a_offsets[c] != b_offsets[c])
        .count();
    assert!(
        differing > NCLASSES / 2,
        "only {differing} class offsets differ"
    );
    assert_ne!(a_slides, b_slides, "huge/meta slides repeated");
    for offsets in [a_offsets, b_offsets] {
        for (c, off) in offsets.iter().enumerate() {
            let colour = regions::colour(c, CLASSES[c].lg_slab as u32, a.lg_page);
            assert!(off & (MAX_SLAB - 1) == colour || off & (MAX_SLAB - 1) == 0);
        }
    }
}

/// Exit code of [`seal_child`] when the kernel (or seccomp) has no `mseal`.
const SKIPPED: i32 = 3;

/// Runs in a forked child because a sealed layout can never be released
/// and a second full layout must not stay alive in the test process.
/// Returns the number of failed checks, or `SKIPPED`.
fn seal_child(page: usize) -> i32 {
    if !sys::features::probe_mseal(page) {
        return SKIPPED;
    }
    let r = Regions::reserve(page, true);
    let before = maps().lines().count();
    let (ranges, n) = r.never_committed();
    let sealed = r.seal() as usize;
    let after = maps().lines().count();
    let mut fails = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            eprintln!("seal: FAIL {what}");
            fails += 1;
        }
    };
    eprintln!(
        "seal: {sealed} of {n} ranges sealed (max {MAX_SEALED}), /proc/self/maps lines {before} -> {after} (+{})",
        after - before
    );
    check(sealed == n, "not every never-committed range was sealed");
    // Each sealed range splits its VMA into at most two more pieces.
    check(
        after - before <= 2 * n + 2,
        "VMA count grew by more than 2 per sealed range",
    );

    // The criterion: munmap of a reserved range fails with EPERM.
    // Likewise mprotect (a commit landing in a gap) and MAP_FIXED over
    // it (a foreign mapping placed in the gap).
    for m in &ranges[..n] {
        // SAFETY: the kernel refuses every one of these; nothing changes.
        unsafe {
            check(
                sys::munmap(m.addr, m.len) == Err(Errno::EPERM),
                "munmap of a sealed range did not fail with EPERM",
            );
            check(
                sys::mprotect(m.addr, page, PROT_READ | PROT_WRITE) == Err(Errno::EPERM),
                "mprotect of a sealed range did not fail with EPERM",
            );
            check(
                sys::mmap(
                    m.addr,
                    page,
                    PROT_NONE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
                    -1,
                    0,
                ) == Err(Errno::EPERM),
                "MAP_FIXED over a sealed range did not fail with EPERM",
            );
        }
    }
    // smaps marks sealed VMAs with `sl`; the largest range is a gap.
    let biggest = ranges[..n].iter().max_by_key(|m| m.len).unwrap();
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
    let mut in_vma = false;
    let mut flagged = false;
    for l in smaps.lines() {
        if let Some((range, _)) = l.split_once(' ')
            && let Some((lo, _)) = range.split_once('-')
            && let Ok(lo) = usize::from_str_radix(lo, 16)
        {
            in_vma = lo == biggest.addr;
        }
        if in_vma && l.starts_with("VmFlags:") {
            flagged = l.split_whitespace().any(|f| f == "sl");
        }
    }
    check(
        flagged,
        "the largest sealed gap has no `sl` in smaps VmFlags",
    );

    // The regions themselves are untouched: a slab of the first and
    // last class, the first huge page and the first metadata page still
    // commit, hold data, purge and reprotect.
    let first_slab = CLASSES[0].slab_size();
    let last_slab = CLASSES[NCLASSES - 1].slab_size();
    for (addr, len) in [
        (r.slab_addr(0, 0), first_slab),
        (
            r.slab_addr(NCLASSES - 1, r.nslabs(NCLASSES - 1) - 1),
            last_slab,
        ),
        (r.huge_addr(0), page),
        (r.meta_base, page),
    ] {
        // SAFETY: inside our own unsealed reservation.
        unsafe {
            check(
                sys::mprotect(addr, len, PROT_READ | PROT_WRITE).is_ok(),
                "commit inside a region failed",
            );
            let p = addr as *mut u64;
            p.write_volatile(0x5EA1);
            check(
                sys::madvise(addr, len, MADV_DONTNEED).is_ok(),
                "purge inside a region failed",
            );
            check(p.read_volatile() == 0, "purged page did not read zero");
            check(
                sys::mprotect(addr, len, PROT_NONE).is_ok(),
                "reprotect inside a region failed",
            );
        }
    }
    fails
}

#[test]
fn seal_locks_the_gaps_and_leaves_the_regions_usable() {
    let _guard = RESERVE_LOCK.lock().unwrap();
    let page = sys::features::page_size();
    // SAFETY: the child only makes syscalls and uses this crate's own
    // allocator, which the fork handlers keep consistent; it exits
    // through `_exit` without returning into the harness.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let code = seal_child(page);
        // SAFETY: terminating the child.
        unsafe { libc::_exit(code) };
    }
    let mut status = 0;
    // SAFETY: `pid` is this process's child; `status` is a local.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFEXITED(status),
        "seal child died: status {status:#x}"
    );
    let code = libc::WEXITSTATUS(status);
    if code == SKIPPED {
        crate::testutil::skip(
            "seal_locks_the_gaps_and_leaves_the_regions_usable",
            "mseal unavailable here (a kernel before 6.10, or seccomp)",
        );
        return;
    }
    assert_eq!(code, 0, "{code} seal checks failed (see stderr)");
}
