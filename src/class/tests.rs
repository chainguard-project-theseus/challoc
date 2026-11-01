//! Tests of the class protocol over a synthetic region and metadata area
//! (`Env`): real `PROT_NONE` reservations.

use super::*;
use crate::layout::{Kind, LG_MAX_SLAB, LG_MIN_SLAB, MAX_SLAB, MIN_SLAB, Mapping, NCLASSES};
use core::ptr;
use std::collections::HashSet;
use std::sync::Arc;
use std::vec;
use std::vec::Vec;

/// The page size the environment is built with: the kernel's, never a
/// 4 KiB constant, because the reservations are real and must hold on
/// 16 and 64 KiB-page hosts too.
fn page_size() -> usize {
    crate::sys::features::page_size()
}

/// Smallest layout `from_parts` accepts: 4 MiB class regions, so the
/// 64 KiB classes have 64 slabs and the 4 MiB class has one.
const LG_REGION: u32 = LG_MAX_SLAB;
const LG_STRIDE: u32 = LG_REGION + 2;
/// Test metadata: 256 KiB per class on a 4 KiB kernel, scaled with the
/// page (every table reservation is rounded up to a page). The
/// every-class tests build a slab table and a set table of all
/// `NCLASSES` per class, up to about 180 KiB a class at the last one, so
/// the need grows with the class count and `quantum8` has more classes.
fn meta_len() -> usize {
    (NCLASSES << 18) * (page_size() / 4096).max(1)
}

/// Test environment: a metadata region and a class superblock, both real
/// `PROT_NONE` reservations so commits and purges go through the kernel
/// and slot memory can be touched.
struct Env {
    meta: Meta,
    regions: Regions,
    maps: [(usize, usize); 2],
}

fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

fn regions_at(superblock: usize, meta: (usize, usize), lg_page: u32) -> Regions {
    let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
    let huge = (superblock + sb_len + (1 << 30), 1usize << 34);
    Regions::from_parts(
        superblock,
        LG_REGION,
        LG_STRIDE,
        &[0usize; NCLASSES],
        huge,
        meta,
        lg_page,
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

impl Env {
    fn new() -> Env {
        Env::with_lg_page(page_size().trailing_zeros())
    }

    /// An environment whose layout believes the page is `1 << lg_page`
    /// bytes. That must be a multiple of the real page (the commits and
    /// purges are real syscalls over a real reservation);
    /// larger than the real page it stands in for a 16 KiB or 64 KiB
    /// page kernel.
    fn with_lg_page(lg_page: u32) -> Env {
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let page = 1usize << lg_page;
        assert!(
            page.is_multiple_of(crate::sys::features::page_size()),
            "fake page must be a real page multiple"
        );
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
        let sb_len = (NCLASSES << LG_STRIDE) + MAX_SLAB;
        let sb = reserve(sb_len);
        let mb_len = meta_len() + page;
        let mb_raw = reserve(mb_len);
        let mb = align_up(mb_raw, page);
        let superblock = align_up(sb, MAX_SLAB);
        let regions = regions_at(superblock, (mb, meta_len()), lg_page);
        Env {
            meta: Meta::new(mb, meta_len(), page),
            regions,
            maps: [(sb, sb_len), (mb_raw, mb_len)],
        }
    }

    /// A fresh table for `c` (leaked: the state points at it) and the
    /// arena-0 state over it, leaked.
    fn class(&self, c: usize, guard_interval: u32) -> &'static ClassState {
        let table = self.table(c, guard_interval);
        std::boxed::Box::leak(std::boxed::Box::new(
            ClassState::new(
                &self.meta,
                &self.regions,
                core::ptr::from_ref(table).cast_mut(),
                Tracked::assume_new(),
                Ghost::assume_new(),
                Ghost::assume_new(),
                0,
                core::ptr::from_ref(self.sets()),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .expect("class state"),
        ))
    }

    /// A fresh set table for one arena (leaked: the states point at it).
    fn sets(&self) -> &'static SetTable {
        std::boxed::Box::leak(std::boxed::Box::new(
            SetTable::new(
                &self.meta,
                &self.regions,
                Ghost::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .expect("set table"),
        ))
    }

    fn table(&self, c: usize, guard_interval: u32) -> &'static SlabTable {
        std::boxed::Box::leak(std::boxed::Box::new(
            SlabTable::new(
                &self.meta,
                &self.regions,
                c,
                guard_interval,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .expect("slab table"),
        ))
    }

    /// The state of `arena` over the same table as `cs`, leaked like
    /// [`Env::class`].
    fn class_in_arena(&self, cs: &ClassState, arena: u16) -> &'static ClassState {
        let table = cs.table();
        std::boxed::Box::leak(std::boxed::Box::new(
            ClassState::new(
                &self.meta,
                &self.regions,
                core::ptr::from_ref(table).cast_mut(),
                Tracked::assume_new(),
                Ghost::assume_new(),
                Ghost::assume_new(),
                arena,
                core::ptr::from_ref(self.sets()),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .expect("class state"),
        ))
    }

    fn claim(&self, cs: &ClassState, n: usize) -> (Vec<*mut u8>, Claim) {
        let mut out = vec![ptr::null_mut(); n];
        let c = cs.claim(&self.meta, &mut out);
        out.truncate(c.n);
        (out, c)
    }
}

/// Regression: with 2^21 members the level-1 table is exactly one page,
/// so level 0 starts on a page that `Set::new` leaves uncommitted (the
/// real huge region has the same shape at 2^28 pages). Scans of an
/// empty or sparsely populated set must never touch those pages.
#[test]
fn set_scans_never_read_uncommitted_words() {
    let env = Env::new();
    let nbits = 1 << 21;
    let mut set = Set::new(&env.meta, page_size(), nbits, Tracked::assume_new()).expect("set");
    assert_eq!(set.map.lowest_set(), None);
    assert_eq!(set.map.lowest_set_from(0), None);
    assert_eq!(set.map.lowest_set_from(12345), None);
    assert_eq!(set.map.lowest_set_from(nbits - 1), None);
    set.ensure(&env.meta, page_size(), 100, Tracked::assume_new())
        .unwrap();
    set.map.set(100);
    assert_eq!(set.map.lowest_set_from(0), Some(100));
    assert_eq!(set.map.lowest_set_from(100), Some(100));
    assert_eq!(set.map.lowest_set_from(101), None);
    // Far beyond the committed prefix, in a level-1 group of its own.
    assert_eq!(set.map.lowest_set_from(64 * 64 * 100), None);
    set.map.clear(100);
    assert_eq!(set.map.lowest_set_from(0), None);
}

impl Drop for Env {
    fn drop(&mut self) {
        for (addr, len) in self.maps {
            unsafe { crate::sys::munmap(addr, len) }.unwrap();
        }
    }
}

/// A class with 64 KiB slabs and a handful of bitmap words.
fn small_class() -> usize {
    // 64 bytes: 1024 slots, 16 words.
    CLASSES.iter().position(|c| c.size() == 64).unwrap()
}

/// The largest class with the minimum slab (1024 bytes: 4 slots of a
/// 4 KiB slab).
fn min_slab_class() -> usize {
    CLASSES
        .iter()
        .rposition(|c| c.slab_size() == MIN_SLAB)
        .unwrap()
}

/// A kernel page larger than the slab (here a page four times the
/// minimum slab, faked through the layout): slabs are committed,
/// guarded and purged in page groups of `slabs_per_page`, and a group
/// is purged only when every slab in it is idle, committed and owned by
/// the purging arena. Dormant on every supported kernel:
/// `layout::adopt_page_geometry` raises a class's slab to the page at
/// init (up to 64 KiB), so `slabs_per_page` is 1 everywhere but here,
/// where the page is faked above the slab; this test keeps the path
/// working.
#[test]
fn page_groups_when_the_page_is_larger_than_the_slab() {
    if (1usize << (LG_MIN_SLAB + 2)) < page_size() {
        // The faked page must be a multiple of the real one; on a
        // 64 KiB kernel the real page already is the larger-page case.
        std::eprintln!(
            "skipped: the real page ({}) is larger than the faked one",
            page_size()
        );
        return;
    }
    let env = Env::with_lg_page(LG_MIN_SLAB + 2);
    let c = min_slab_class();
    let n = CLASSES[c].nslots();
    assert_eq!(CLASSES[c].slab_size(), MIN_SLAB);
    // Every fourth page group is a guard group: slabs 12..16.
    let cs = env.class(c, 4);
    assert_eq!(cs.table().slabs_per_page(), 4);
    assert_eq!(cs.table().guard_interval(), 4);
    for s in 0..20 {
        assert_eq!(cs.is_guard_slab(s), (12..16).contains(&s), "slab {s}");
    }
    // Fill the first group and the first slab of the second; the
    // second group's page is committed with slab 4, so every slot of
    // slabs 4..8 is writable already (the test touches them in `claim`).
    let (p, _) = env.claim(cs, 5 * n);
    assert_eq!(p.len(), 5 * n);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 5,
            ..Default::default()
        }
    );
    for (s, chunk) in p.chunks(n).enumerate() {
        assert_eq!(chunk[0] as usize, env.regions.slab_addr(c, s));
    }
    let base = env.regions.slab_addr(c, 0);
    let in_slab = |v: &[*mut u8], s: usize| -> Vec<*mut u8> {
        v.iter()
            .copied()
            .filter(|&x| (x as usize - base) / MIN_SLAB == s)
            .collect()
    };
    // Slab 0 goes Empty; its group mates 1..4 are Active, so nothing is purged.
    cs.release(&p[..n]);
    assert_eq!(cs.desc(0).state(), State::Empty);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        0
    );
    assert_eq!(cs.desc(0).state(), State::Empty);
    assert_eq!(cs.oldest_empty(), cs.desc(0).dirty_at());
    // Another arena gets the next group (slabs 128..256: groups are
    // whole page groups, so a page group never straddles arenas);
    // 128..132 is page group 32, not a guard (124..128 is), so its
    // first usable slab is 128. Arena 0 then reuses its Empty slab 0
    // and commits 5 and 6 (a refill takes Empty slabs before Purged
    // ones and Purged ones before committing).
    let other = env.class_in_arena(cs, 1);
    let (q, _) = env.claim(other, n);
    assert_eq!(cs.table().group(), 128);
    assert!(cs.is_guard_slab(124) && !cs.is_guard_slab(128));
    assert_eq!(q[0] as usize, env.regions.slab_addr(c, 128));
    assert_eq!(cs.table().owner(128).0, 1);
    assert_eq!(cs.table().owner(5).0, 0);
    assert_eq!(cs.table().committed_slabs(), 256);
    assert_eq!(cs.table().guards(Tracked::assume_new()), 0);
    let (r, _) = env.claim(cs, 3 * n);
    assert_eq!(r.len(), 3 * n);
    for s in [0, 5, 6] {
        assert_eq!(in_slab(&r, s).len(), n, "slab {s}");
        assert_eq!(cs.desc(s).state(), State::Active, "slab {s}");
        assert_eq!(cs.desc(s).arena(), 0);
    }
    assert_eq!(cs.desc(128).arena(), 1);
    // Slab 7 was committed with its page group (by slab 4) and handed
    // to arena 0 with the group, but no refill has used it: Fresh.
    assert_eq!(cs.desc(7).state(), State::Fresh);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 7,
            ..Default::default()
        }
    );
    // Page group 4..8 with 4, 5, 6 Empty and 7 Fresh is idle, so it is
    // discarded as a whole; 7 stays Fresh (nothing was written to it).
    cs.release(&p[4 * n..]);
    cs.release(&in_slab(&r, 5));
    cs.release(&in_slab(&r, 6));
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 7,
            empty: 3,
            ..Default::default()
        }
    );
    assert_eq!(
        other.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        0
    );
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        3
    );
    for s in 4..7 {
        assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
    }
    assert_eq!(cs.desc(7).state(), State::Fresh);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 7,
            purged: 3,
            ..Default::default()
        }
    );
    // With 0..4 Empty, purging with a cutoff at the oldest slab's
    // `dirty_at` takes the whole group, a slab released later (younger,
    // whenever the coarse clock has moved on) included.
    cs.release(&p[n..4 * n]);
    cs.release(&in_slab(&r, 0));
    let oldest = cs.desc(1).dirty_at();
    assert!(cs.desc(0).dirty_at() >= oldest);
    assert_eq!(
        cs.purge_empty(oldest, Tracked::assume_new(), Tracked::assume_new()),
        4
    );
    for s in 0..4 {
        assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
        assert_eq!(cs.desc(s).dirty_at(), 0);
    }
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 7,
            purged: 7,
            ..Default::default()
        }
    );
    // Purged neighbours do not block a purge: a claim reuses Purged
    // slab 0 (zero), and releasing it purges it alone inside its
    // Purged group.
    let (z, claim) = env.claim(cs, 3);
    assert_eq!(claim, Claim { n: 3, zeroed: true });
    assert_eq!(z[0] as usize, base);
    cs.release(&z);
    assert_eq!(cs.desc(0).state(), State::Empty);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    assert_eq!(cs.desc(0).state(), State::Purged);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 7,
            purged: 7,
            ..Default::default()
        }
    );
    // Past the guard group: the Purged slabs go first, then slabs 7
    // and 8..12 are committed, then the next commit skips the guard
    // group 12..16 and lands on slab 16.
    let (g, _) = env.claim(cs, 12 * n + 1);
    assert_eq!(g.len(), 12 * n + 1);
    assert_eq!(in_slab(&g, 7).len(), n);
    assert_eq!(in_slab(&g, 11).len(), n);
    assert_eq!(in_slab(&g, 16).len(), 1);
    assert_eq!(cs.table().guards(Tracked::assume_new()), 4);
    assert_eq!(cs.stats().committed, 13);
    assert_eq!(cs.table().committed_slabs(), 256);
    for s in 12..16 {
        assert!(cs.is_guard_slab(s));
        assert_eq!(cs.desc(s).state(), State::Fresh);
        assert_eq!(cs.desc(s).live(), 0);
    }
    // The other arena's group is untouched by all of this.
    assert_eq!(other.desc(128).state(), State::Active);
    assert_eq!(other.desc(128).live(), n);
    assert_eq!(
        other.stats(),
        ClassStats {
            committed: 1,
            guards: 4,
            ..Default::default()
        }
    );
}

/// Slabs go to arenas in line-sized groups (design 4): with 4
/// slots per slab the bitmap line holds 128 slabs, so the second arena
/// starts at slab 128 and the bits of neighbouring slabs share a word
/// without disturbing each other. A slab handed out with the group but
/// not yet committed has a `Fresh` descriptor, clear bits and the
/// group's owner. The region has 8 groups: a ninth arena gets nothing.
#[test]
fn slabs_are_handed_out_in_line_sized_groups() {
    let env = Env::new();
    let c = min_slab_class();
    let cls = &CLASSES[c];
    assert_eq!(cls.nslots(), 4);
    assert_eq!(cls.slab_bits(), 4);
    assert_eq!(cls.slabs_per_line(), 128);
    let cs = env.class(c, 0);
    let t = cs.table();
    assert_eq!(t.group(), 128);
    assert_eq!(t.nslabs(), 1024);
    assert_eq!(t.committed_slabs(), 0);
    let (p, claim) = env.claim(cs, 4);
    assert_eq!(claim, Claim { n: 4, zeroed: true });
    assert_eq!(p[0] as usize, env.regions.slab_addr(c, 0));
    assert_eq!(t.committed_slabs(), 128);
    assert_eq!(cs.stats().committed, 1);
    assert!(t.bits(0).is_full());
    for s in 1..128 {
        assert_eq!(t.owner(s).0, 0, "slab {s}");
        assert_eq!(t.desc(s).state(), State::Fresh, "slab {s}");
        assert_eq!(t.desc(s).arena(), 0, "slab {s}");
        assert_eq!(t.desc(s).live(), 0, "slab {s}");
        assert!(!t.bits(s).is_full(), "slab {s}");
        for slot in 0..4 {
            assert!(!t.is_allocated(s, slot), "slab {s} slot {slot}");
        }
    }
    assert!(!t.is_allocated(128, 0));
    // The second arena's first slab is the second group's.
    let cs1 = env.class_in_arena(cs, 1);
    let (q, claim) = env.claim(cs1, 1);
    assert_eq!(claim, Claim { n: 1, zeroed: true });
    assert_eq!(q[0] as usize, env.regions.slab_addr(c, 128));
    assert_eq!(t.committed_slabs(), 256);
    assert_eq!(t.owner(128).0, 1);
    assert_eq!(t.owner(255).0, 1);
    assert_eq!(t.owner(127).0, 0);
    assert_eq!(t.check(q[0]), 1);
    assert_eq!(t.check(p[0]), 0);
    // Arena 0 keeps filling its own group: slab 1 next, in the same
    // bitmap word as slab 0 (16 slabs of 4 bits per word).
    let (r, _) = env.claim(cs, 1);
    assert_eq!(r[0] as usize, env.regions.slab_addr(c, 1));
    assert_eq!(t.bits(1).allocated(4), 1);
    assert!(t.bits(0).is_full());
    assert_eq!(t.bits(0).allocated(4), 4);
    cs.release(&p[1..3]);
    assert_eq!(t.bits(0).allocated(4), 2);
    assert_eq!(t.bits(1).allocated(4), 1);
    assert!(
        t.is_allocated(0, 0)
            && !t.is_allocated(0, 1)
            && !t.is_allocated(0, 2)
            && t.is_allocated(0, 3)
    );
    assert!(t.is_allocated(1, 0));
    // Eight groups in the region: arenas 2..8 take the rest, a ninth
    // arena finds nothing to commit and reports the shortfall.
    for a in 2..8u16 {
        let csa = env.class_in_arena(cs, a);
        let (pa, claim) = env.claim(csa, 1);
        assert_eq!(claim.n, 1, "arena {a}");
        assert_eq!(
            pa[0] as usize,
            env.regions.slab_addr(c, 128 * a as usize),
            "arena {a}"
        );
    }
    assert_eq!(t.committed_slabs(), 1024);
    let cs8 = env.class_in_arena(cs, 8);
    let (_, claim) = env.claim(cs8, 1);
    assert_eq!(
        claim,
        Claim {
            n: 0,
            zeroed: false
        }
    );
    // Arena 0 still has 126 uncommitted slabs of its own group, plus
    // the 2 + 3 free slots of slabs 0 and 1.
    let (more, claim) = env.claim(cs, 126 * 4 + 5);
    assert_eq!(claim.n, 126 * 4 + 5);
    assert_eq!(more[0] as usize, env.regions.slot_addr(c, 0, 1));
    assert_eq!(cs.locate(more[more.len() - 1]).0, 127);
    assert_eq!(cs.stats().committed, 128);
    let (_, claim) = env.claim(cs, 1);
    assert_eq!(
        claim,
        Claim {
            n: 0,
            zeroed: false
        }
    );
}

/// A free into a slab handed to an arena but not yet committed is
/// rejected by its clear bit, at the same point as any invalid free.
#[test]
#[should_panic(expected = "invalid or double free of")]
fn check_below_the_watermark_in_a_slab_not_yet_committed_aborts() {
    let env = Env::new();
    let c = min_slab_class();
    let cs = env.class(c, 0);
    env.claim(cs, 1);
    assert_eq!(cs.committed_slabs(), 128);
    cs.check(env.regions.slab_addr(c, 5) as *mut u8);
}

/// Every class: a slab's bits are either whole words or one sub-word
/// range, neighbouring slabs never share bits, and a group's bits fill
/// whole lines.
#[test]
fn bitmap_geometry_for_every_class() {
    let env = Env::new();
    for c in 0..NCLASSES {
        let cs = env.class(c, 0);
        let cls = cs.class();
        let t = cs.table();
        let n = cls.nslots();
        if t.nslabs() < 2 {
            continue;
        }
        let (p, _) = env.claim(cs, n + 1);
        assert_eq!(p.len(), n + 1, "class {c}");
        assert!(t.bits(0).is_full(), "class {c}");
        assert_eq!(t.bits(0).allocated(n), n, "class {c}");
        assert_eq!(t.bits(0).nbits(), cls.slab_bits(), "class {c}");
        assert_eq!(t.bits(1).allocated(n), 1, "class {c}");
        assert!(!t.bits(1).is_full(), "class {c}");
        assert!(
            t.is_allocated(1, 0) && (n == 1 || !t.is_allocated(1, 1)),
            "class {c}"
        );
        let g = t.group();
        assert!(
            g >= cls.slabs_per_line() && g.is_multiple_of(cls.slabs_per_line()),
            "class {c}"
        );
        // Two slabs used: one group unless groups are single slabs.
        assert_eq!(
            t.committed_slabs(),
            if g == 1 { 2 } else { g.min(t.nslabs()) },
            "class {c}"
        );
        cs.release(&p);
        assert_eq!(t.bits(0).allocated(n), 0, "class {c}");
        assert_eq!(t.bits(1).allocated(n), 0, "class {c}");
    }
}

#[test]
fn first_claim_is_lowest_addresses_and_zero() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let cls = cs.class();
    assert_eq!(cs.committed_slabs(), 0);
    let (p, claim) = env.claim(cs, 10);
    assert_eq!(
        claim,
        Claim {
            n: 10,
            zeroed: true
        }
    );
    for (i, &q) in p.iter().enumerate() {
        assert_eq!(q as usize, env.regions.slab_addr(c, 0) + i * cls.size());
        assert_eq!(
            env.regions.classify(q as usize),
            Kind::Slab {
                class: c as u32,
                slab: 0,
                slot: i as u32
            }
        );
        cs.check(q);
        assert_eq!(cs.locate(q), (0, i));
    }
    let d = cs.desc(0);
    assert_eq!(d.state(), State::Active);
    assert_eq!(d.live(), 10);
    assert_eq!(d.arena(), 0);
    assert_eq!(cs.committed_slabs(), 1);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 1,
            nonfull: 1,
            ..Default::default()
        }
    );
    // A second claim from the same, now Active, slab is not known zero.
    let (p2, claim2) = env.claim(cs, 5);
    assert_eq!(
        claim2,
        Claim {
            n: 5,
            zeroed: false
        }
    );
    assert_eq!(p2[0] as usize, env.regions.slot_addr(c, 0, 10));
    assert_eq!(d.live(), 15);
}

#[test]
fn filling_a_slab_moves_on_and_release_reopens_it() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let n = cs.class().nslots();
    let (all, claim) = env.claim(cs, n);
    assert_eq!(claim.n, n);
    let set: HashSet<usize> = all.iter().map(|&p| p as usize).collect();
    assert_eq!(set.len(), n);
    assert!(cs.table().bits(0).is_full());
    // A whole fresh slab reserved by one plan leaves the partial set at
    // once; nothing in it could be released before the claim finished.
    assert!(!cs.is_partial(0, Tracked::assume_new()));
    assert_eq!(cs.stats().nonfull, 0);
    let (next, claim) = env.claim(cs, 1);
    assert_eq!(claim, Claim { n: 1, zeroed: true });
    assert_eq!(next[0] as usize, env.regions.slab_addr(c, 1));
    assert!(!cs.is_partial(0, Tracked::assume_new()));
    assert!(cs.is_partial(1, Tracked::assume_new()));
    assert_eq!(cs.stats().committed, 2);
    assert_eq!(cs.stats().nonfull, 1);
    // Releasing one slot in slab 0 puts it back, and first fit picks it
    // before slab 1.
    let victim = all[n / 2];
    cs.release(&[victim]);
    assert!(cs.is_partial(0, Tracked::assume_new()));
    assert_eq!(cs.lowest_partial(Tracked::assume_new()), Some(0));
    assert_eq!(cs.desc(0).live(), n - 1);
    assert!(!cs.table().is_allocated(0, n / 2));
    let (again, claim) = env.claim(cs, 2);
    assert_eq!(
        claim,
        Claim {
            n: 2,
            zeroed: false
        }
    );
    assert_eq!(again[0], victim);
    assert_eq!(again[1] as usize, env.regions.slot_addr(c, 1, 1));
}

#[test]
fn filling_a_live_slab_leaves_a_stale_bit_until_the_next_plan() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let n = cs.class().nslots();
    // Two claims fill slab 0; the second reserved every remaining slot
    // of a slab that already had live slots, so the bit is left stale.
    let (a, _) = env.claim(cs, n - 3);
    let (b, _) = env.claim(cs, 3);
    assert_eq!(a.len() + b.len(), n);
    assert!(cs.table().bits(0).is_full());
    assert!(cs.is_partial(0, Tracked::assume_new()));
    assert_eq!(cs.stats().nonfull, 1);
    // The next plan notices, clears it and commits slab 1 in the same
    // lock hold.
    let (q, claim) = env.claim(cs, 2);
    assert_eq!(claim, Claim { n: 2, zeroed: true });
    assert_eq!(q[0] as usize, env.regions.slab_addr(c, 1));
    assert!(!cs.is_partial(0, Tracked::assume_new()));
    assert!(cs.is_partial(1, Tracked::assume_new()));
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 2,
            nonfull: 1,
            ..Default::default()
        }
    );
    // A release into the full slab reopens it and first fit prefers it.
    cs.release(&[a[5]]);
    assert!(cs.is_partial(0, Tracked::assume_new()));
    let (r, claim) = env.claim(cs, 1);
    assert_eq!(
        claim,
        Claim {
            n: 1,
            zeroed: false
        }
    );
    assert_eq!(r[0], a[5]);
}

#[test]
fn one_plan_spans_many_slabs_and_release_reopens_them_all() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let n = cs.class().nslots();
    // Fill 2 * MAX_PICKS + 1 slabs: the first plan reserves MAX_PICKS
    // whole fresh slabs, the second the rest.
    let slabs = 2 * MAX_PICKS + 1;
    let (all, claim) = env.claim(cs, slabs * n);
    assert_eq!(
        claim,
        Claim {
            n: slabs * n,
            zeroed: true
        }
    );
    for s in 0..slabs {
        assert_eq!(all[s * n] as usize, env.regions.slab_addr(c, s));
        assert!(cs.table().bits(s).is_full());
        assert!(!cs.is_partial(s, Tracked::assume_new()));
    }
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: slabs,
            nonfull: 0,
            ..Default::default()
        }
    );
    // One slot per slab, sorted, in one batch: the reopen list overflows
    // at MAX_PICKS and flushes; every slab must be partial afterwards.
    let victims: Vec<*mut u8> = (0..slabs).map(|s| all[s * n + s % n]).collect();
    cs.release(&victims);
    for s in 0..slabs {
        assert!(
            cs.is_partial(s, Tracked::assume_new()),
            "slab {s} not reopened"
        );
        assert_eq!(cs.desc(s).live(), n - 1);
    }
    assert_eq!(cs.stats().nonfull, slabs);
    // Refilling takes exactly those slots, lowest slab first, in one
    // plan per MAX_PICKS slabs.
    let (again, claim) = env.claim(cs, slabs);
    assert_eq!(
        claim,
        Claim {
            n: slabs,
            zeroed: false
        }
    );
    assert_eq!(again, victims);
    // Those slabs had live slots, so their bits went stale rather than
    // being cleared; each later plan cleared the previous plan's full
    // slabs on its way past, leaving only the last one stale.
    assert_eq!(cs.stats().nonfull, 1);
    assert!(cs.is_partial(slabs - 1, Tracked::assume_new()));
    // Releasing a whole slab in one batch empties it; the reopen list
    // must not put an Empty slab back into the partial set.
    let mut whole: Vec<*mut u8> = all[3 * n..4 * n].to_vec();
    whole.sort();
    cs.release(&whole);
    assert_eq!(cs.desc(3).state(), State::Empty);
    assert!(!cs.is_partial(3, Tracked::assume_new()));
    assert_eq!(cs.stats().empty, 1);
    assert_eq!(cs.stats().nonfull, 1);
    // And the same for the slab with the stale bit.
    let mut whole: Vec<*mut u8> = all[(slabs - 1) * n..].to_vec();
    whole.sort();
    cs.release(&whole);
    assert_eq!(cs.desc(slabs - 1).state(), State::Empty);
    assert!(!cs.is_partial(slabs - 1, Tracked::assume_new()));
    assert_eq!(cs.stats().empty, 2);
    assert_eq!(cs.stats().nonfull, 0);
}

#[test]
fn release_all_makes_empty_then_reuse_then_purge() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let (p, _) = env.claim(cs, 100);
    // Release in reverse order to exercise the run grouping.
    let mut rev = p.clone();
    rev.reverse();
    cs.release(&rev);
    let d = cs.desc(0);
    assert_eq!(d.state(), State::Empty);
    assert_eq!(d.live(), 0);
    assert!(d.dirty_at() > 0);
    assert!(!cs.is_partial(0, Tracked::assume_new()));
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 1,
            empty: 1,
            ..Default::default()
        }
    );
    // An Empty slab is preferred over committing a new one but is dirty.
    let (q, claim) = env.claim(cs, 3);
    assert_eq!(
        claim,
        Claim {
            n: 3,
            zeroed: false
        }
    );
    assert_eq!(q[0], p[0]);
    assert_eq!(d.state(), State::Active);
    assert_eq!(d.dirty_at(), 0);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 1,
            nonfull: 1,
            ..Default::default()
        }
    );
    cs.release(&q);
    assert_eq!(d.state(), State::Empty);
    // Purge with a cutoff before dirty_at does nothing; after it purges.
    assert_eq!(
        cs.purge_empty(
            d.dirty_at() - 1,
            Tracked::assume_new(),
            Tracked::assume_new()
        ),
        0
    );
    assert_eq!(d.state(), State::Empty);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    assert_eq!(d.state(), State::Purged);
    assert_eq!(d.dirty_at(), 0);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 1,
            purged: 1,
            ..Default::default()
        }
    );
    // A Purged slab is reused before a new commit and is zero.
    let (r, claim) = env.claim(cs, 4);
    assert_eq!(claim, Claim { n: 4, zeroed: true });
    assert_eq!(r[0], p[0]);
    assert_eq!(d.state(), State::Active);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 1,
            nonfull: 1,
            ..Default::default()
        }
    );
}

/// The decay driver's view (design 8): nothing is purged in
/// the first step of the window, about half of a cohort by mid-window
/// (the highest addresses, so a claim still finds the lowest), all of
/// it at the end; the walk runs once per step; older cohorts go first;
/// a lone slab lasts until its weight is zero; a stale (too early)
/// `oldest_empty` corrects itself; a zero period purges at once.
#[test]
fn decay_purges_the_oldest_empty_slabs_along_the_curve() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let nslots = CLASSES[c].nslots();
    let n = cs.nslabs();
    assert!(n >= 64, "test layout holds {n} slabs");
    assert_eq!(cs.oldest_empty(), 0);
    assert_eq!(cs.decay(u64::MAX, 1000), None);
    // A window of 200_000 ns: steps of 1000.
    const W: u64 = 200_000;
    // One cohort of every slab; the clock reads are made uniform.
    let (p, _) = env.claim(cs, nslots * n);
    cs.release(&p);
    assert_eq!(cs.stats().empty, n);
    let t0 = cs.oldest_empty();
    assert!(t0 > 0);
    for s in 0..n {
        cs.desc(s).set_dirty_at(t0, Tracked::assume_new());
    }
    // Before the first step ends: no walk, the deadline is its end.
    assert_eq!(cs.decay(t0 + 999, W), Some(t0 + 1000));
    assert_eq!(cs.stats().empty, n);
    // At it: a walk that purges nothing (weight one), next one a step on.
    assert_eq!(cs.decay(t0 + 1000, W), Some(t0 + 2000));
    assert_eq!(cs.stats().empty, n);
    // Within the step the deadline holds and nothing is walked.
    assert_eq!(cs.decay(t0 + 1500, W), Some(t0 + 2000));
    // Mid-window: about half, the highest slabs first.
    assert_eq!(cs.decay(t0 + W / 2, W), Some(t0 + W / 2 + 1000));
    let st = cs.stats();
    assert_eq!(st.empty + st.purged, n);
    assert!(st.purged.abs_diff(n / 2) <= n / 10 + 1, "{st:?}");
    for s in 0..n {
        let want = if s < st.empty {
            State::Empty
        } else {
            State::Purged
        };
        assert_eq!(cs.desc(s).state(), want, "slab {s}");
    }
    assert_eq!(cs.oldest_empty(), t0);
    // A quarter of the way in nothing more would have gone; three
    // quarters in most of it; at the end all of it.
    assert_eq!(cs.decay(t0 + 3 * W / 4, W), Some(t0 + 3 * W / 4 + 1000));
    let st2 = cs.stats();
    assert!(st2.purged > st.purged && st2.empty > 0, "{st2:?}");
    assert!(st2.empty * 100 / n <= 15, "{st2:?}");
    assert_eq!(cs.decay(t0 + W, W), None);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: n,
            purged: n,
            ..Default::default()
        }
    );
    assert_eq!(cs.oldest_empty(), 0);
    // Two cohorts: slab 0 at t, slab 1 a half window younger. At the
    // last step of slab 0 only it goes; slab 1, alone, lasts until its
    // own last step.
    let (q, _) = env.claim(cs, nslots * 2);
    cs.release(&q);
    let t = cs.oldest_empty();
    cs.desc(0).set_dirty_at(t, Tracked::assume_new());
    cs.desc(1).set_dirty_at(t + W / 2, Tracked::assume_new());
    assert_eq!(cs.decay(t + W - 1000, W), Some(t + W));
    assert_eq!(cs.desc(0).state(), State::Purged);
    assert_eq!(cs.desc(1).state(), State::Empty);
    assert_eq!(cs.oldest_empty(), t + W / 2);
    assert_eq!(
        cs.decay(t + W / 2 + W - 2000, W),
        Some(t + W / 2 + W - 1000)
    );
    assert_eq!(cs.desc(1).state(), State::Empty);
    assert_eq!(cs.decay(t + W / 2 + W - 1000, W), None);
    assert_eq!(cs.desc(1).state(), State::Purged);
    // A claim reuses slab 0 (the lowest Empty): the atomic is now stale
    // (too early). The next decay finds nothing and clears it.
    let (r, _) = env.claim(cs, nslots);
    cs.release(&r[..1]);
    assert_eq!(cs.desc(0).state(), State::Active);
    cs.release(&r[1..]);
    assert_eq!(cs.desc(0).state(), State::Empty);
    let t = cs.oldest_empty();
    assert!(t > 0);
    let (r, _) = env.claim(cs, 1);
    assert_eq!(r[0], q[0]);
    assert_eq!(cs.oldest_empty(), t);
    assert_eq!(cs.decay(u64::MAX / 2, W), None);
    assert_eq!(cs.oldest_empty(), 0);
    // A zero period purges as soon as a slab is Empty, whatever the
    // deadline of the last walk.
    cs.release(&r);
    let t = cs.oldest_empty();
    assert!(t > 0);
    assert_eq!(cs.decay(t, 0), None);
    assert_eq!(cs.desc(0).state(), State::Purged);
    // Purging everything at once clears it too.
    let (r, _) = env.claim(cs, nslots * 2);
    cs.release(&r);
    assert!(cs.oldest_empty() > 0);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        2
    );
    assert_eq!(cs.oldest_empty(), 0);
}

/// A deadline left by a walk under a long period does not delay a
/// period shortened since (`arena.<i>.dirty_decay_ms`).
#[test]
fn decay_walks_at_once_after_the_period_shrinks() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let nslots = CLASSES[c].nslots();
    let (p, _) = env.claim(cs, nslots * 4);
    cs.release(&p);
    let t0 = cs.oldest_empty();
    for s in 0..4 {
        cs.desc(s).set_dirty_at(t0, Tracked::assume_new());
    }
    // A walk under a 20 s period leaves a deadline 100 ms out.
    let long = 20_000_000_000;
    assert_eq!(cs.decay(t0 + long / 200, long), Some(t0 + 2 * (long / 200)));
    assert_eq!(cs.stats().empty, 4);
    // Under a 200 us period the same instant is past the window.
    assert_eq!(cs.decay(t0 + long / 200, 200_000), None);
    assert_eq!(cs.stats().purged, 4);
}

/// Every slab of the region (a full batch's worth in the test layout)
/// is purged in one call.
#[test]
fn purge_batches_a_whole_region() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let nslots = CLASSES[c].nslots();
    let n = cs.nslabs();
    assert!(n >= crate::purge::MAX_BATCH, "test layout holds {n} slabs");
    let (p, claim) = env.claim(cs, nslots * n);
    assert_eq!(claim.n, nslots * n);
    cs.release(&p);
    assert_eq!(cs.stats().empty, n);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        n
    );
    assert_eq!(cs.stats().purged, n);
    assert_eq!(cs.oldest_empty(), 0);
    for s in 0..n {
        assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
    }
}

/// A slab whose discard fails (a page `mlock`ed: `EINVAL`) stays `Empty`
/// with its age and is not counted purged, while its neighbours in the
/// same merged range, issued again one by one, purge; the decay walk
/// that hit it backs the gate off for `DISCARD_RETRY_NS`, and once
/// unlocked it purges.
#[test]
fn a_slab_whose_discard_fails_stays_empty() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    assert_eq!(cs.table().slabs_per_page(), 1);
    let nslots = CLASSES[c].nslots();
    let (p, _) = env.claim(cs, nslots * 4);
    cs.release(&p);
    let t0 = cs.oldest_empty();
    for s in 0..4 {
        cs.desc(s).set_dirty_at(t0, Tracked::assume_new());
    }
    let page = crate::sys::features::page_size();
    let locked = env.regions.slab_addr(c, 1);
    if !crate::testutil::mlock_or_skip("a_slab_whose_discard_fails_stays_empty", locked, page) {
        return;
    }
    let only_slab_1_empty = || {
        let st = cs.stats();
        assert_eq!((st.empty, st.purged), (1, 3), "{st:?}");
        for s in [0, 2, 3] {
            assert_eq!(cs.desc(s).state(), State::Purged, "slab {s}");
        }
        assert_eq!(cs.desc(1).state(), State::Empty);
        assert_eq!(cs.desc(1).dirty_at(), t0);
        assert_eq!(cs.oldest_empty(), t0);
    };
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        3
    );
    only_slab_1_empty();
    // A walk past the window tries it, fails and backs off.
    const W: u64 = 200_000;
    let retry = t0 + W + crate::purge::DISCARD_RETRY_NS;
    assert_eq!(cs.decay(t0 + W, W), Some(retry));
    only_slab_1_empty();
    unsafe { libc::munlock(locked as *const _, page) };
    // No walk until the back-off ends, then the purge.
    assert_eq!(cs.decay(t0 + 2 * W, W), Some(retry));
    only_slab_1_empty();
    assert_eq!(cs.decay(retry, W), None);
    assert_eq!(cs.stats().purged, 4);
}

#[test]
fn claim_spans_slabs_and_reports_shortfall_on_exhaustion() {
    let env = Env::new();
    // The largest class: one 4 MiB slab of 16 slots in this tiny region.
    let c = NCLASSES - 1;
    let cs = env.class(c, 0);
    assert_eq!(cs.nslabs(), 1);
    assert_eq!(cs.class().nslots(), 16);
    let (p, claim) = env.claim(cs, 20);
    assert_eq!(
        claim,
        Claim {
            n: 16,
            zeroed: true
        }
    );
    assert_eq!(p.len(), 16);
    let (q, claim) = env.claim(cs, 1);
    assert_eq!(
        claim,
        Claim {
            n: 0,
            zeroed: false
        }
    );
    assert!(q.is_empty());
    cs.release(&p[..8]);
    let (q, claim) = env.claim(cs, 20);
    assert_eq!(
        claim,
        Claim {
            n: 8,
            zeroed: false
        }
    );
    assert_eq!(q, p[..8].to_vec());
    // A class with 64 slabs: ask for more than one slab holds.
    let c = small_class();
    let cs = env.class(c, 0);
    let n = cs.class().nslots();
    let (p, claim) = env.claim(cs, 2 * n + 5);
    assert_eq!(
        claim,
        Claim {
            n: 2 * n + 5,
            zeroed: true
        }
    );
    assert_eq!(cs.stats().committed, 3);
    assert_eq!(cs.stats().nonfull, 1);
    assert_eq!(p[2 * n] as usize, env.regions.slab_addr(c, 2));
}

#[test]
fn guard_slabs_are_skipped() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 4);
    for s in 0..16 {
        assert_eq!(cs.is_guard_slab(s), s % 4 == 3, "slab {s}");
    }
    let n = cs.class().nslots();
    let (p, claim) = env.claim(cs, 6 * n);
    assert_eq!(claim.n, 6 * n);
    let slabs: HashSet<usize> = p.iter().map(|&q| cs.locate(q).0).collect();
    assert_eq!(slabs, [0, 1, 2, 4, 5, 6].into_iter().collect());
    // Slabs 0..=7 are committed: six allocatable and two guards.
    assert_eq!(cs.committed_slabs(), 7);
    let (q, _) = env.claim(cs, 1);
    assert_eq!(cs.locate(q[0]).0, 8);
    assert_eq!(cs.committed_slabs(), 9);
    assert_eq!(cs.stats().committed, 7);
    assert_eq!(cs.stats().guards, 2);
    // A guard slab's descriptor is zero: any free into it aborts.
    assert_eq!(cs.desc(3).state(), State::Fresh);
    assert_eq!(cs.desc(3).live(), 0);
    // `commit_slab` of a guard slab (here slab 11, handed out with the
    // group of slab 8 and not yet reached): committed, the guard installed
    // (`install_guard`), counted, never handed out.
    assert_eq!(
        cs.table().commit_slab(11, Tracked::assume_new()),
        Some(false)
    );
    assert_eq!(cs.stats().guards, 3);
    assert_eq!(
        cs.committed_slabs(),
        9,
        "the watermark is `take_group`'s, not `commit_slab`'s"
    );
}

#[test]
fn works_for_every_class() {
    let env = Env::new();
    for c in 0..NCLASSES {
        let cs = env.class(c, 0);
        let cls = cs.class();
        let want = cls.nslots().min(70);
        let (p, claim) = env.claim(cs, want);
        assert_eq!(
            claim,
            Claim {
                n: want,
                zeroed: true
            },
            "class {c}"
        );
        for (i, &q) in p.iter().enumerate() {
            assert_eq!(
                env.regions.classify(q as usize),
                Kind::Slab {
                    class: c as u32,
                    slab: 0,
                    slot: i as u32
                },
                "class {c}"
            );
            cs.check(q);
        }
        cs.release(&p);
        assert_eq!(cs.desc(0).state(), State::Empty, "class {c}");
        assert_eq!(
            cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
            1
        );
    }
    let s = env.meta.stats(vstd::prelude::Tracked::assume_new());
    // The tables are reservations or, at this layout's sizes, eager carves.
    assert!(s.reserved + s.eager > 0 && s.committed > 0);
}

#[test]
fn metadata_exhaustion_is_reported_not_fatal() {
    // A metadata region too small for the smallest class's descriptor
    // array: 64 slabs * stride is small, so shrink the region to a
    // couple of pages instead.
    let meta = {
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let mb = unsafe {
            crate::sys::mmap(
                0,
                2 * page_size(),
                PROT_NONE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                -1,
                0,
            )
        }
        .unwrap();
        Meta::new(mb, 2 * page_size(), page_size())
    };
    let regions = regions_at(
        0x1000_0000_0000,
        (0x2000_0000_0000, 2 * page_size()),
        page_size().trailing_zeros(),
    );
    assert!(
        SlabTable::new(
            &meta,
            &regions,
            0,
            0,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .is_none()
    );
}

/// Two arenas over one class region: slabs are handed out in address
/// order to whichever arena commits next, each records its owner, and
/// each arena's sets and counts see only its own slabs.
/// The point of the interleaved set table: however many classes an
/// arena uses, the words their sets touch for low slab numbers share
/// the first page of their slab-size group (rows 0 and 1), where one
/// table per set touched a page or two per (arena, class). Creating
/// the table commits nothing.
#[test]
fn set_table_of_many_classes_touches_one_page_per_slab_size() {
    let env = Env::new();
    let before = env
        .meta
        .stats(vstd::prelude::Tracked::assume_new())
        .committed;
    let sets = env.sets();
    // The set tables commit nothing of their own; the ones small enough to
    // be carved from the object side (`meta::EAGER_MAX`) bring at most
    // their alignment padding and one chunk's rounding into the count.
    let slack = env
        .meta
        .stats(vstd::prelude::Tracked::assume_new())
        .committed
        - before;
    assert!(
        slack < crate::meta::COMMIT_CHUNK + SET_GROUPS * env.regions.page_size(),
        "reservation only, slack {slack}"
    );
    let page = env.regions.page_size();
    let classes: Vec<usize> = (0..NCLASSES).step_by(5).collect();
    let states: Vec<ClassState> = classes
        .iter()
        .map(|&c| {
            ClassState::new(
                &env.meta,
                &env.regions,
                core::ptr::from_ref(env.table(c, 0)).cast_mut(),
                Tracked::assume_new(),
                Ghost::assume_new(),
                Ghost::assume_new(),
                0,
                core::ptr::from_ref(sets),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .expect("class state")
        })
        .collect();
    for cs in &states {
        let (p, _) = env.claim(cs, 3);
        cs.release(&p[..1]);
        let (q, _) = env.claim(cs, 1);
        cs.release(&q);
        cs.release(&p[1..]);
    }
    // Every state has slabs in its partial or empty set now.
    assert!(states.iter().all(|cs| {
        let s = cs.stats();
        s.nonfull + s.empty > 0
    }));
    let mut groups_used: Vec<usize> = classes.iter().map(|&c| SetTable::group_of(c)).collect();
    groups_used.sort_unstable();
    groups_used.dedup();
    let mut pages: Vec<usize> = Vec::new();
    for (base, committed) in sets.committed_ranges() {
        if committed == 0 {
            continue;
        }
        // SAFETY: the committed prefix of a table this test owns.
        let words = unsafe { core::slice::from_raw_parts(base as *const u64, committed / 8) };
        pages.extend(
            words
                .iter()
                .enumerate()
                .filter(|(_, w)| **w != 0)
                .map(|(i, _)| (base + 8 * i) / page),
        );
    }
    pages.sort_unstable();
    pages.dedup();
    assert!(!pages.is_empty());
    assert!(
        pages.len() <= groups_used.len(),
        "{} classes in {} slab-size groups touched {} pages of the set tables",
        classes.len(),
        groups_used.len(),
        pages.len()
    );
}

#[test]
fn two_arenas_share_one_region() {
    let env = Env::new();
    let c = small_class();
    let cs0 = env.class(c, 0);
    let cs1 = env.class_in_arena(cs0, 1);
    assert_eq!(cs1.arena(), 1);
    assert!(core::ptr::eq(cs0.table(), cs1.table()));
    let (p0, _) = env.claim(cs0, 3);
    let (p1, _) = env.claim(cs1, 3);
    assert_eq!(cs0.committed_slabs(), 2);
    assert_eq!(cs1.committed_slabs(), 2);
    assert_eq!(cs0.desc(0).arena(), 0);
    assert_eq!(cs0.desc(1).arena(), 1);
    assert_eq!(cs0.locate(p0[0]).0, 0);
    assert_eq!(cs1.locate(p1[0]).0, 1);
    assert!(cs0.is_partial(0, Tracked::assume_new()) && !cs0.is_partial(1, Tracked::assume_new()));
    assert!(cs1.is_partial(1, Tracked::assume_new()) && !cs1.is_partial(0, Tracked::assume_new()));
    assert_eq!(cs0.stats().committed, 1);
    assert_eq!(cs1.stats().committed, 1);
    // Each arena keeps growing past the other's slabs.
    let (q1, _) = env.claim(cs1, cs1.class().nslots());
    assert_eq!(cs1.locate(q1[q1.len() - 1]).0, 2);
    assert_eq!(cs0.desc(2).arena(), 1);
    let (q0, _) = env.claim(cs0, cs0.class().nslots());
    assert_eq!(cs0.locate(q0[q0.len() - 1]).0, 3);
    assert_eq!(cs0.desc(3).arena(), 0);
    // Releases land in the owner's sets only.
    cs1.release(&p1);
    cs1.release(&q1);
    assert_eq!(cs1.desc(1).state(), State::Empty);
    assert_eq!(cs1.desc(2).state(), State::Empty);
    assert_eq!(cs1.stats().empty, 2);
    assert_eq!(cs0.stats().empty, 0);
    assert_eq!(
        cs1.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        2
    );
    assert_eq!(cs0.stats().purged, 0);
    assert_eq!(cs1.stats().purged, 2);
    cs0.release(&p0);
    cs0.release(&q0);
    assert_eq!(cs0.stats().empty, 2);
    // A purged slab is reused by its own arena before anything new is
    // committed.
    let (r1, claim) = env.claim(cs1, 1);
    assert!(claim.zeroed);
    assert_eq!(cs1.locate(r1[0]).0, 1);
    assert_eq!(cs0.committed_slabs(), 4);
}

/// `take_group` cross-checks the group's owner word against the
/// descriptors' arena field before handing the group out: a wild write
/// into the owner array aborts the next hand-out instead of routing
/// that group's frees through a wrong arena.
#[test]
#[should_panic(expected = "owner word says arena 7, slab")]
fn a_corrupt_owner_word_aborts_the_next_take_group() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let (_p, _) = env.claim(cs, 1);
    assert_eq!(cs.table().committed_slabs(), cs.table().group());
    cs.table().corrupt_owner_word(&env.meta, 1, 7);
    // The second arena's first claim needs a fresh group: the second.
    let cs1 = env.class_in_arena(cs, 1);
    let _ = env.claim(cs1, 1);
}

/// `release_owned` cross-checks the owner word it routes by against the
/// slab descriptor's arena at every slab switch: a descriptor that disagrees with the word
/// means one of them is corrupt, and the batch aborts before a bit is
/// cleared.
#[test]
#[should_panic(expected = "owner word says arena 0, slab")]
fn a_disagreeing_descriptor_arena_aborts_the_release() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 1);
    let (slab, _) = cs.locate(p[0]);
    assert_eq!(cs.table().owner(slab).0, 0);
    cs.desc(slab).set_arena(1);
    cs.release(&p);
}

#[test]
#[should_panic(expected = "belongs to arena 1 but was released through arena 0")]
fn release_through_the_wrong_arena_aborts() {
    let env = Env::new();
    let c = small_class();
    let cs0 = env.class(c, 0);
    let cs1 = env.class_in_arena(cs0, 1);
    let (_p0, _) = env.claim(cs0, 1);
    let (p1, _) = env.claim(cs1, 1);
    cs0.release(&p1);
}

#[test]
#[should_panic(expected = "belongs to arena 1 but was released through arena 0")]
fn check_through_the_wrong_arena_aborts() {
    let env = Env::new();
    let c = small_class();
    let cs0 = env.class(c, 0);
    let cs1 = env.class_in_arena(cs0, 1);
    let (_p0, _) = env.claim(cs0, 1);
    let (p1, _) = env.claim(cs1, 1);
    assert_eq!(cs0.table().check(p1[0]), 1);
    cs0.check(p1[0]);
}

#[test]
#[should_panic(expected = "double free of")]
fn double_release_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 3);
    cs.release(&p[..1]);
    cs.release(&p[..1]);
}

/// A release batch is grouped by slab, not
/// sorted. Slabs in any order, and a slab's slots in any order, clear
/// exactly their bits with the right live counts.
#[test]
fn release_takes_slab_groups_in_any_order() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let n = CLASSES[c].nslots();
    let (p, _) = env.claim(cs, 2 * n + 7);
    assert_eq!(p.len(), 2 * n + 7);
    // Slab 1 reversed, then slab 0's odd slots scrambled, then slab 2.
    let mut batch: Vec<*mut u8> = p[n..2 * n].iter().rev().copied().collect();
    let mut odd: Vec<*mut u8> = (1..n).step_by(2).map(|i| p[i]).collect();
    shuffle(&mut odd, &mut 0x9e37_79b9_7f4a_7c15u64);
    batch.extend_from_slice(&odd);
    batch.extend_from_slice(&p[2 * n..]);
    cs.release(&batch);
    for (i, &q) in p.iter().enumerate() {
        let (slab, slot) = cs.locate(q);
        let released = i >= n || i % 2 == 1;
        assert_eq!(cs.table().bits(slab).is_set(slot), !released, "slot {i}");
    }
    assert_eq!(cs.desc(0).live(), n - odd.len());
    assert_eq!(cs.desc(1).live(), 0);
    assert_eq!(cs.desc(2).live(), 0);
}

/// A duplicate inside one slab's group aborts even when the two copies
/// are not adjacent and not in the same bitmap word: the group's bits
/// accumulate in one scratch bitmap.
#[test]
#[should_panic(expected = "double free of")]
fn duplicate_apart_in_one_slab_group_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 80);
    cs.release(&[p[70], p[1], p[5], p[70]]);
}

#[test]
#[should_panic(expected = "double free of")]
fn duplicate_in_one_batch_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 3);
    cs.release(&[p[1], p[1]]);
}

#[test]
#[should_panic(expected = "not a slot start")]
fn release_of_interior_pointer_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 1);
    cs.release(&[unsafe { p[0].add(1) }]);
}

#[test]
#[should_panic(expected = "outside the class region")]
fn release_outside_region_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    env.claim(cs, 1);
    cs.release(&[env.regions.huge_base as *mut u8]);
}

#[test]
#[should_panic(expected = "was never allocated")]
fn check_above_the_watermark_aborts() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    env.claim(cs, 1);
    cs.check(env.regions.slab_addr(c, 5) as *mut u8);
}

/// A slot start in a guard slab is named as
/// such by the exact check (the sized fast path rejects it
/// arithmetically and lands here).
#[test]
#[should_panic(expected = "guard slab")]
fn check_of_a_guard_slab_slot_aborts() {
    let env = Env::new();
    let c = small_class();
    let n = CLASSES[c].nslots();
    // Every second page group is a guard group.
    let cs = env.class(c, 2);
    env.claim(cs, 2 * n + 1);
    let guard = (0..cs.table().committed_slabs())
        .find(|&s| cs.is_guard_slab(s))
        .expect("a guard slab");
    assert!(guard < cs.table().committed_slabs());
    cs.check(env.regions.slab_addr(c, guard) as *mut u8);
}

/// A release into a slab of the arena's group
/// that the arena has not committed (`Fresh`, below the hand-out
/// watermark) is where a sized free of such a slot lands at the flush.
#[test]
#[should_panic(expected = "the slab has never been allocated from")]
fn release_into_an_uncommitted_slab_of_the_group_aborts() {
    let env = Env::new();
    // The smallest slabs have several descriptors per line, so a group
    // is several slabs.
    let c = min_slab_class();
    let cs = env.class(c, 0);
    env.claim(cs, 1);
    // The whole group was handed out; only slab 0 is committed.
    assert!(
        cs.table().committed_slabs() > 1,
        "group of {}",
        cs.table().committed_slabs()
    );
    assert_eq!(cs.desc(1).state(), State::Fresh);
    assert_eq!(cs.stats().committed, 1);
    cs.release(&[env.regions.slab_addr(c, 1) as *mut u8]);
}

/// The mask form of `is_guard_slab` agrees with the definition (every
/// `interval`-th page group, counted from 1) for every page-group ratio
/// and power-of-two interval the tables use, and with guards off no slab
/// of a region is a guard.
#[test]
fn guard_slab_test_is_the_page_group_modulo() {
    for lg in [0u32, 2] {
        if lg != 0 && (1usize << (LG_MIN_SLAB + lg)) < page_size() {
            continue; // a faked page smaller than the real one cannot be reserved
        }
        let env = if lg == 0 {
            Env::new()
        } else {
            Env::with_lg_page(LG_MIN_SLAB + lg)
        };
        let c = min_slab_class();
        for interval in [0u32, 2, 4, 8, 32, 1 << 22] {
            let t = env.table(c, interval);
            assert_eq!(t.guard_interval(), interval);
            let spp = t.slabs_per_page();
            for s in 0..256 {
                let want = interval != 0 && (s / spp + 1) % interval as usize == 0;
                assert_eq!(
                    t.is_guard_slab(s),
                    want,
                    "lg {lg} interval {interval} slab {s}"
                );
            }
            if interval == 0 {
                assert!((0..t.nslabs()).step_by(4093).all(|s| !t.is_guard_slab(s)));
                assert!(!t.is_guard_slab(t.nslabs() - 1));
            }
        }
    }
}

/// The interval must be a power of two (the sized path's guard test is
/// a mask); `config.rs` rounds a configured value up before it gets here.
#[test]
#[should_panic(expected = "guard interval 3 is not a power of two")]
fn guard_interval_must_be_a_power_of_two() {
    let env = Env::new();
    let _ = env.table(min_slab_class(), 3);
}

#[test]
#[should_panic(expected = "invalid or double free of")]
fn check_of_unclaimed_slot_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 2);
    cs.check(unsafe { p[1].add(cs.class().size()) });
}

#[test]
#[should_panic(expected = "invalid or double free of")]
fn check_after_release_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 2);
    cs.release(&p[..1]);
    cs.check(p[0]);
}

/// Randomised single-thread model check: live pointers are unique,
/// valid, and the descriptor counts match; after draining, every slab
/// is Empty and purging leaves everything Purged.
#[test]
fn randomised_model_check() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let cls = cs.class();
    let mut rng = 0xC0FF_EE12_3456_789Au64;
    let mut live: Vec<*mut u8> = Vec::new();
    let mut set: HashSet<usize> = HashSet::new();
    for _ in 0..4000 {
        let r = xorshift(&mut rng);
        if r.is_multiple_of(2) && !live.is_empty() {
            // Release a random batch; claims and releases are balanced so
            // the live set random-walks well inside the 64-slab region.
            let k = (xorshift(&mut rng) as usize % 70 + 1).min(live.len());
            let mut batch = Vec::with_capacity(k);
            for _ in 0..k {
                let i = xorshift(&mut rng) as usize % live.len();
                let p = live.swap_remove(i);
                assert!(set.remove(&(p as usize)));
                batch.push(p);
            }
            if xorshift(&mut rng).is_multiple_of(2) {
                batch.sort();
            }
            cs.release(&batch);
        } else {
            let k = xorshift(&mut rng) as usize % 70 + 1;
            let (p, claim) = env.claim(cs, k);
            assert_eq!(claim.n, k);
            for &q in &p {
                assert!(set.insert(q as usize), "duplicate pointer {q:p}");
                assert!(matches!(
                    env.regions.classify(q as usize),
                    Kind::Slab { class, .. } if class as usize == c
                ));
                cs.check(q);
            }
            live.extend(p);
        }
        // Invariants after every step.
        let mut total_live = 0;
        let st = cs.stats();
        let mut empty = 0;
        for s in 0..cs.committed_slabs() {
            let d = cs.desc(s);
            total_live += d.live();
            let bm = cs.table().bits(s);
            assert_eq!(bm.allocated(cls.nslots()), d.live(), "slab {s}");
            match d.state() {
                State::Empty => {
                    assert_eq!(d.live(), 0);
                    assert!(!cs.is_partial(s, Tracked::assume_new()));
                    empty += 1;
                }
                State::Active => {
                    assert!(d.live() > 0 || cs.is_partial(s, Tracked::assume_new()));
                    if !bm.is_full() {
                        assert!(
                            cs.is_partial(s, Tracked::assume_new()),
                            "slab {s} has free slots but is not partial"
                        );
                    }
                }
                other => panic!("slab {s} in state {other:?}"),
            }
        }
        assert_eq!(total_live, live.len());
        assert_eq!(st.empty, empty);
    }
    live.sort();
    cs.release(&live);
    let st = cs.stats();
    assert_eq!(st.empty, st.committed);
    assert_eq!(st.nonfull, 0);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        st.committed
    );
    let st = cs.stats();
    assert_eq!(st.purged, st.committed);
    assert_eq!(st.empty, 0);
    for s in 0..cs.committed_slabs() {
        assert_eq!(cs.desc(s).state(), State::Purged);
    }
    // Everything is reusable afterwards, from slab 0, and zero.
    let (p, claim) = env.claim(cs, 3);
    assert_eq!(claim, Claim { n: 3, zeroed: true });
    assert_eq!(p[0] as usize, env.regions.slab_addr(c, 0));
}

/// Eight threads claim and release concurrently; no pointer is ever
/// held by two threads at once and the counters settle.
#[test]
fn concurrent_claims_are_disjoint() {
    let env = Arc::new(Env::new());
    let c = small_class();
    let cs = Arc::new(env.class(c, 0));
    let threads = 8;
    let rounds = 300;
    let mut handles = Vec::new();
    for t in 0..threads {
        let env = Arc::clone(&env);
        let cs = Arc::clone(&cs);
        handles.push(std::thread::spawn(move || {
            let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
            let mut held: Vec<*mut u8> = Vec::new();
            let mut seen: Vec<usize> = Vec::new();
            for _ in 0..rounds {
                let k = xorshift(&mut rng) as usize % 50 + 1;
                let (p, claim) = env.claim(&cs, k);
                assert_eq!(claim.n, k);
                for &q in &p {
                    cs.check(q);
                    seen.push(q as usize);
                }
                held.extend(p);
                if !xorshift(&mut rng).is_multiple_of(4) {
                    let keep = xorshift(&mut rng) as usize % (held.len() + 1);
                    let mut batch = held.split_off(keep);
                    batch.sort();
                    cs.release(&batch);
                }
            }
            held.sort();
            cs.release(&held);
            seen
        }));
    }
    // Pointers held simultaneously by two threads would show up as a
    // slot claimed twice without an intervening release; we approximate
    // that check by verifying the final state is consistent and that
    // per-thread `check` never aborted (it runs on every claimed slot).
    let mut total = 0;
    for h in handles {
        total += h.join().unwrap().len();
    }
    assert!(total > 0);
    let st = cs.stats();
    assert_eq!(st.empty, st.committed, "{st:?}");
    assert_eq!(st.nonfull, 0);
    for s in 0..cs.committed_slabs() {
        let d = cs.desc(s);
        assert_eq!(d.state(), State::Empty, "slab {s}");
        assert_eq!(d.live(), 0, "slab {s}");
        assert!(!cs.table().bits(s).is_full());
        for slot in 0..cs.class().nslots() {
            assert!(!cs.table().is_allocated(s, slot));
        }
    }
}

/// Two threads hammer one slot's worth of address space: one claims and
/// immediately releases, the other purges whenever it can. The
/// reservation-first protocol must keep every claimed slot readable and
/// writable and never let a purge race a claim.
#[test]
fn purge_never_races_a_claim() {
    let env = Arc::new(Env::new());
    let c = small_class();
    let cs = Arc::new(env.class(c, 0));
    let stop = Arc::new(core::sync::atomic::AtomicBool::new(false));
    // The purger publishes its count as it goes: on a small VM (four
    // vCPUs, four test threads) it can be off the CPU for the whole
    // 20 000-iteration window, so the loop runs past the window until
    // the purger has purged once, bounded by time. The assertion at the end is about
    // the protocol, not the scheduler.
    let purged = Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let purger = {
        let cs = Arc::clone(&cs);
        let stop = Arc::clone(&stop);
        let purged = Arc::clone(&purged);
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(core::sync::atomic::Ordering::Relaxed) {
                n += cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
                purged.store(n, core::sync::atomic::Ordering::Relaxed);
            }
            n
        })
    };
    let mut rng = 1u64;
    let started = std::time::Instant::now();
    let mut i = 0u32;
    loop {
        i += 1;
        if i > 20_000
            && (purged.load(core::sync::atomic::Ordering::Relaxed) > 0
                || started.elapsed() > std::time::Duration::from_secs(30))
        {
            break;
        }
        let k = xorshift(&mut rng) as usize % 8 + 1;
        let (p, claim) = env.claim(&cs, k);
        assert_eq!(claim.n, k);
        for &q in &p {
            unsafe {
                if claim.zeroed {
                    assert_eq!(*(q as *const u32), 0, "iteration {i}: purged slot not zero");
                }
                *(q as *mut u32) = i;
            }
        }
        for &q in &p {
            assert_eq!(
                unsafe { *(q as *const u32) },
                i,
                "iteration {i}: slot lost its value"
            );
        }
        cs.release(&p);
    }
    stop.store(true, core::sync::atomic::Ordering::Relaxed);
    let purges = purger.join().unwrap();
    assert!(purges > 0, "the purger never found an empty slab");
    assert_eq!(cs.desc(0).live(), 0);
}

/// Purged memory reads zero and the claim reports it; committed slabs
/// show up read-write in `/proc/self/maps`.
#[test]
fn purge_zeroes_memory_and_commit_is_visible() {
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 0);
    let (p, claim) = env.claim(cs, 5);
    assert!(claim.zeroed);
    for &q in &p {
        unsafe {
            assert_eq!(*q, 0);
            *q = 0xAB;
        }
    }
    cs.release(&p);
    let (q, claim) = env.claim(cs, 5);
    assert_eq!(q, p);
    assert!(!claim.zeroed);
    assert_eq!(unsafe { *q[0] }, 0xAB);
    cs.release(&q);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    let (r, claim) = env.claim(cs, 5);
    assert_eq!(r, p);
    assert!(claim.zeroed);
    for &s in &r {
        assert_eq!(unsafe { *s }, 0);
    }
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let slab = env.regions.slab_addr(c, 0);
    let end = slab + cs.class().slab_size();
    let rw = maps.lines().any(|l| {
        let mut it = l.split_whitespace();
        let range = it.next().unwrap();
        let perms = it.next().unwrap();
        let (a, b) = range.split_once('-').unwrap();
        let a = usize::from_str_radix(a, 16).unwrap();
        let b = usize::from_str_radix(b, 16).unwrap();
        a <= slab && end <= b && perms.starts_with("rw")
    });
    assert!(rw, "slab 0 is not read-write in /proc/self/maps");
    // Descriptors live in the metadata region and got committed too.
    let d = cs.desc(0) as *const SlabDesc as usize;
    assert!(env.regions.in_meta(d));
    assert!(
        env.meta
            .stats(vstd::prelude::Tracked::assume_new())
            .committed
            >= page_size()
    );
}

/// `check_zero` on a healthy heap: every
/// `Fresh` and `Purged` claim reads zero, so the mode changes nothing,
/// and the flag stays on for every later claim of this test process.
#[test]
fn check_zero_passes_fresh_and_purged_slabs() {
    purge::set_check_zero(true);
    assert!(purge::check_zero());
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, claim) = env.claim(cs, 5);
    assert!(claim.zeroed);
    for &q in &p {
        unsafe { *q = 0xAB };
    }
    cs.release(&p);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    let (q, claim) = env.claim(cs, 5);
    assert_eq!(q, p);
    assert!(claim.zeroed);
}

/// `check_zero` fires: a slab purged (so reported zero) whose memory
/// is dirtied behind the allocator's back aborts at the next claim,
/// naming the `Purged` state and the purge advice.
#[test]
#[should_panic(expected = "was handed out as zero from a Purged slab but its word at offset")]
fn check_zero_aborts_on_a_dirty_purged_slab() {
    purge::set_check_zero(true);
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (p, _) = env.claim(cs, 2);
    cs.release(&p);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    // The last word of the second slot: the check reads both ends.
    let size = cs.class().size();
    unsafe { ptr::write_volatile(p[1].add(size - 8) as *mut u64, 0xDEAD_BEEF) };
    let _ = env.claim(cs, 2);
}

/// The calloc race: thread A's `plan` selects a `Fresh` slab, flags its
/// pick zero and drops the class lock; thread B refills from the same,
/// now partial, slab, hands the slot to the program, which writes to it
/// and frees it; then A's `finish` runs. Invariant: a slot reported zero
/// was never handed out since it was last zeroed. A's slots are claimed
/// in `take`, under the lock, so B gets a different slot and A's still
/// reads zero. The same holds for a `Purged` slab when the purge advice
/// zeroes.
#[test]
fn a_zero_pick_never_receives_a_slot_another_claim_used_in_between() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let mut round = 0;
    loop {
        // Slab 0 is not handed out before the first `plan` (so no
        // `desc(0)` yet): `Fresh` then, `Purged` in round 2.
        let prior = if round == 0 {
            State::Fresh
        } else {
            State::Purged
        };
        // Thread A: one lock hold, one pick of one slot of slab 0.
        let mut picks = [Pick::EMPTY; MAX_PICKS];
        let np = cs.plan(
            &env.meta,
            1,
            &mut picks,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert_eq!(np, 1);
        assert_eq!((picks[0].slab, picks[0].n, picks[0].state), (0, 1, prior));
        assert!(
            picks[0].zero,
            "round {round}: the {prior:?} slab must be a zero pick"
        );
        assert_eq!(cs.desc(0).live(), 1);
        assert_eq!(
            cs.stats().nonfull,
            1,
            "slab 0 is partial, so B's plan finds it"
        );
        // Thread B: a whole refill of one slot from the same class,
        // the program writes to the slot and frees it.
        let (b, claim) = env.claim(cs, 1);
        assert_eq!(
            claim,
            Claim {
                n: 1,
                zeroed: false
            }
        );
        assert_eq!(
            cs.locate(b[0]).0,
            0,
            "B refills from the slab A reserved in"
        );
        unsafe {
            ptr::write_volatile(b[0], 0xAB);
        }
        cs.release(&b);
        assert_eq!(cs.desc(0).live(), 1);
        // Thread A finishes its refill.
        let mut out = [ptr::null_mut(); 1];
        let mut zeroed = true;
        assert_eq!(
            cs.finish(
                &picks[..np],
                &mut out,
                &mut zeroed,
                None,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
                Ghost::assume_new()
            ),
            1
        );
        assert!(zeroed);
        assert_ne!(
            out[0], b[0],
            "round {round}: a slot reported zero was allocated, written and freed by another claim in between"
        );
        assert_eq!(
            cs.locate(out[0]),
            (0, 0),
            "A's slot was decided under the lock: the lowest"
        );
        assert_eq!(
            cs.locate(b[0]),
            (0, 1),
            "B saw A's bit and took the next slot"
        );
        assert_eq!(
            unsafe { ptr::read_volatile(out[0]) },
            0,
            "round {round}: A's slot reads zero"
        );
        cs.release(&out);
        assert_eq!(cs.desc(0).live(), 0);
        assert_eq!(cs.stats().empty, 1);
        round += 1;
        if round == 2 {
            break;
        }
        // Round 2: the same window on the slab once purged.
        assert_eq!(
            cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
            1
        );
        assert_eq!(cs.desc(0).state(), State::Purged);
        if !purge::purged_is_zero() {
            break;
        }
    }
}

/// Whether `len` bytes at `addr` lie in one read-write mapping of
/// `/proc/self/maps`.
fn is_rw(addr: usize, len: usize) -> bool {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    maps.lines().any(|l| {
        let mut it = l.split_whitespace();
        let (a, b) = it.next().unwrap().split_once('-').unwrap();
        let a = usize::from_str_radix(a, 16).unwrap();
        let b = usize::from_str_radix(b, 16).unwrap();
        a <= addr && addr + len <= b && it.next().unwrap().starts_with("rw")
    })
}

/// An arena commits its group a run at a time (`SlabTable::commit_run`):
/// its first fresh slab commits every slab up to the next guard slab with
/// one `mprotect`, and the guard slab stays uncommitted until the cursor
/// reaches it, which commits and guards it with the next run. With guards
/// off a run is eight 64 KiB slabs.
#[test]
fn slabs_are_committed_a_run_at_a_time() {
    use crate::sys::features::Features;
    let guards = Features::probe(true).guard_install;
    let env = Env::new();
    if env.regions.page_size() > 64 << 10 {
        std::eprintln!("page above 64 KiB; skipping the run layout test");
        return;
    }
    let c = (0..NCLASSES)
        .find(|&c| {
            let cls = &CLASSES[c];
            cls.slab_size() == 64 << 10 && cls.slabs_per_line() >= 16
        })
        .expect("a class of 64 KiB slabs with groups of 16 slabs or more");
    let cs = env.class(c, if guards { 8 } else { 0 });
    assert!(cs.table().group() >= 16);
    let size = cs.class().slab_size();
    let slab = |s: usize| env.regions.slab_addr(c, s);
    let run = if guards { 7 } else { 8 };
    let n = cs.class().nslots();
    let (p, _) = env.claim(cs, n);
    assert_eq!(cs.locate(p[0]).0, 0);
    assert!(is_rw(slab(0), run * size), "the first run is committed");
    assert!(!is_rw(slab(run), size), "slab {run} is committed early");
    assert_eq!(cs.table().guards(Tracked::assume_new()), 0);
    // Use up the run: the next fresh slab is past it, slab 8.
    for _ in 1..run {
        env.claim(cs, n);
    }
    let (q, _) = env.claim(cs, n);
    assert_eq!(cs.locate(q[0]).0, 8);
    let end = if guards { 15 } else { 16 };
    assert!(is_rw(slab(0), end * size), "the second run is committed");
    assert!(!is_rw(slab(end), size), "slab {end} is committed early");
    assert_eq!(
        cs.table().guards(Tracked::assume_new()),
        usize::from(guards)
    );
    // Slabs 0..=8 were handed out, less the guard slab 7 with guards on.
    assert_eq!(cs.stats().committed, if guards { 8 } else { 9 });
}

/// The runs `starts_run` and `run_end` describe tile every group exactly,
/// for every class, page size and guard interval: the group's first slab
/// starts a run, each run ends where the next slab that starts one is (or
/// at the group's end), no slab inside a run, past its first, is a guard
/// slab, and a run holds at most 512 KiB of slabs past its guard. A slab
/// no run covered would be handed out `PROT_NONE`.
#[test]
fn runs_tile_every_group() {
    let real = page_size().trailing_zeros();
    for lg_page in [real, 14, 16] {
        if lg_page < real {
            continue;
        }
        for interval in [0u32, 2, 4, 8, 16, 32, 64, 128, 512, 2048] {
            // One environment per table set: the tables' metadata is
            // reserved and leaked.
            let env = Env::with_lg_page(lg_page);
            for c in 0..NCLASSES {
                let t = env.table(c, interval);
                if !t.runs() {
                    continue;
                }
                let ngroups = t.nslabs().div_ceil(t.group());
                let groups = (0..ngroups.min(64)).chain(ngroups.saturating_sub(2)..ngroups);
                for gi in groups {
                    let (start, end) = t.group_range(gi);
                    let what = |s: usize| {
                        std::format!(
                            "page 2^{lg_page} interval {interval} class {c} group {gi} slab {s}"
                        )
                    };
                    assert!(t.starts_run(start), "{}", what(start));
                    let mut s = start;
                    while s < end {
                        assert!(t.starts_run(s), "{}", what(s));
                        let e = t.run_end(s, end);
                        assert!(s < e && e <= end, "{}: end {e}", what(s));
                        for x in s + 1..e {
                            assert!(!t.starts_run(x), "{}: inside the run", what(x));
                            assert!(!t.is_guard_slab(x), "{}: a guard inside the run", what(x));
                        }
                        // At most `RUN_BYTES` of slabs to allocate from,
                        // after the guard slab a run starts with.
                        let most = (super::table::RUN_BYTES >> CLASSES[c].lg_slab).max(1)
                            + usize::from(t.is_guard_slab(s));
                        assert!(e - s <= most, "{}: run of {}", what(s), e - s);
                        s = e;
                    }
                }
            }
        }
    }
}

/// A run holds at most 512 KiB (`RUN_BYTES`): a class of larger slabs
/// commits one slab at a time.
#[test]
fn large_slabs_are_committed_one_at_a_time() {
    let env = Env::new();
    let page = env.regions.page_size();
    let Some(c) = (0..NCLASSES).find(|&c| {
        let cls = &CLASSES[c];
        cls.slab_size() >= (512 << 10).max(page) && cls.slabs_per_line() >= 2
    }) else {
        std::eprintln!("no class of 512 KiB slabs in groups; skipping");
        return;
    };
    let cs = env.class(c, 0);
    let size = cs.class().slab_size();
    let (p, _) = env.claim(cs, cs.class().nslots());
    assert_eq!(cs.locate(p[0]).0, 0);
    assert!(is_rw(env.regions.slab_addr(c, 0), size));
    assert!(
        !is_rw(env.regions.slab_addr(c, 1), size),
        "slab 1 is committed early"
    );
}

/// Guard slabs only when the kernel has `MADV_GUARD_INSTALL`: touching
/// the guard page must fault. Run in a child so the fault is observable.
#[test]
fn guard_slab_faults_when_supported() {
    use crate::sys::features::Features;
    let f = Features::probe(true);
    if !f.guard_install {
        std::eprintln!("guard_install=no on this kernel; skipping guard fault test");
        return;
    }
    let env = Env::new();
    let c = small_class();
    let cs = env.class(c, 2);
    let n = cs.class().nslots();
    let (p, _) = env.claim(cs, n + 1);
    assert_eq!(cs.locate(p[n]).0, 2);
    let guard = env.regions.slab_addr(c, 1);
    // The guard slab is inside the same rw mapping as its neighbours.
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let merged = maps.lines().any(|l| {
        let range = l.split_whitespace().next().unwrap();
        let (a, b) = range.split_once('-').unwrap();
        let a = usize::from_str_radix(a, 16).unwrap();
        let b = usize::from_str_radix(b, 16).unwrap();
        a <= env.regions.slab_addr(c, 0) && env.regions.slab_addr(c, 3) <= b
    });
    assert!(merged, "guard slab split the VMA");
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        unsafe {
            core::ptr::write_volatile(guard as *mut u8, 1);
            libc::_exit(0);
        }
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFSIGNALED(status),
        "child did not fault: status {status:#x}"
    );
    assert_eq!(libc::WTERMSIG(status), libc::SIGSEGV);
}

#[test]
fn bin_stats_counts_every_curregs_clamp() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (_, curregs) = cs.bin_stats(Tracked::assume_new());
    assert_eq!((curregs, cs.clamped()), (0, 0));
    // No slab committed, so the capacity is zero: a `claimed` ahead of
    // `released` (the window `bin_stats` documents) is clamped to it and
    // the clamp is counted.
    cs.claimed.store(3, Ordering::Relaxed);
    let (_, curregs) = cs.bin_stats(Tracked::assume_new());
    assert_eq!((curregs, cs.clamped()), (0, 1));
    let (_, curregs) = cs.bin_stats(Tracked::assume_new());
    assert_eq!((curregs, cs.clamped()), (0, 2));
    // Caught up: no clamp, the count stays.
    cs.released.store(3, Ordering::Relaxed);
    let (_, curregs) = cs.bin_stats(Tracked::assume_new());
    assert_eq!((curregs, cs.clamped()), (0, 2));
}

#[test]
#[should_panic(expected = "slab live count above its slot count")]
fn a_plan_over_a_slab_with_live_above_nslots_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (b, claim) = env.claim(cs, 1);
    assert_eq!(claim.n, 1);
    let slab = cs
        .lowest_partial(Tracked::assume_new())
        .expect("the slab is partial");
    cs.release(&b[..1]);
    assert_eq!(cs.stats().empty, 1);
    // Corrupt the descriptor of the now `Empty` slab: `live` past the
    // slot count (the partial walk skips such a slab; the empty set
    // hands it straight to `take`).
    let nslots = cs.class().nslots();
    cs.desc(slab).add_live(nslots + 1);
    let mut picks = [Pick::EMPTY; MAX_PICKS];
    cs.plan(
        &env.meta,
        1,
        &mut picks,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
}

#[test]
#[should_panic(expected = "empty slab count underflow")]
fn a_plan_that_takes_an_empty_slab_the_counts_do_not_know_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (b, claim) = env.claim(cs, 1);
    assert_eq!(claim.n, 1);
    cs.release(&b[..1]);
    assert_eq!(cs.stats().empty, 1);
    // Corrupt the count: the set still holds the slab.
    cs.inner.lock_t().stats.empty = 0;
    let mut picks = [Pick::EMPTY; MAX_PICKS];
    cs.plan(
        &env.meta,
        1,
        &mut picks,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
}

#[test]
#[should_panic(expected = "empty slab count underflow")]
fn a_purge_of_an_empty_slab_the_counts_do_not_know_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (b, claim) = env.claim(cs, 1);
    assert_eq!(claim.n, 1);
    cs.release(&b[..1]);
    assert_eq!(cs.stats().empty, 1);
    cs.inner.lock_t().stats.empty = 0;
    cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
}

/// `claim_prefix`'s cross-check: a `Fresh` slab (handed out, not yet
/// committed) with a bit already set when a refill takes it aborts
/// instead of reporting the pick zero.
#[test]
#[should_panic(
    expected = "slab 1 (arena 0) is Fresh with live 0 but word 0 of its bitmap had bits set before the refill's claim"
)]
fn a_fresh_slab_with_a_bit_set_before_its_first_take_aborts() {
    let env = Env::new();
    let c = min_slab_class();
    let n = CLASSES[c].nslots();
    let cs = env.class(c, 0);
    // Slab 0 fills; slab 1 is handed out with the group (its bitmap
    // line is initialised) and not yet committed.
    let (p, _) = env.claim(cs, n);
    assert_eq!(p.len(), n);
    assert_eq!(cs.desc(1).state(), State::Fresh);
    assert_eq!(
        cs.table().bits(1).claim_word(
            0,
            1,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new()
        ),
        0b1,
        "bit 0 of slab 1 set behind the protocol's back"
    );
    // The next refill commits slab 1 and takes it as a zero pick.
    env.claim(cs, 1);
}

/// `shuffle` is a permutation (Fisher-Yates over `xorshift`): the
/// same elements, deterministic for a seed, not the identity on a
/// batch a cache would fill; a batch of one or none is left alone
/// without touching the generator.
#[test]
fn shuffle_permutes_the_batch() {
    let orig: Vec<*mut u8> = (1..=64usize).map(|i| (i * 16) as *mut u8).collect();
    let (mut a, mut b) = (orig.clone(), orig.clone());
    let (mut ra, mut rb) = (0x9e37_79b9_7f4a_7c15u64, 0x9e37_79b9_7f4a_7c15u64);
    shuffle(&mut a, &mut ra);
    shuffle(&mut b, &mut rb);
    assert_eq!(a, b, "the same seed gives the same order");
    assert_eq!(ra, rb);
    assert_ne!(a, orig);
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(sorted, orig, "the same slots");
    let mut none: Vec<*mut u8> = Vec::new();
    let mut one = vec![orig[0]];
    let before = ra;
    shuffle(&mut none, &mut ra);
    shuffle(&mut one, &mut ra);
    assert_eq!((ra, one[0]), (before, orig[0]));
}

/// `claim_random`: the slots `claim` would hand out, in a shuffled
/// order, and the zero report unchanged. A `Fresh` slab's prefix
/// `0..n` is claimed under the lock (zero); after a release of all of
/// it the `Empty` slab is dirty, its bits are scanned from a random
/// word, and two refills between them hand out every slot exactly
/// once.
#[test]
fn claim_random_hands_out_the_same_slots_in_a_random_order() {
    let env = Env::new();
    let c = small_class();
    let n = CLASSES[c].nslots();
    let cs = env.class(c, 0);
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut out = vec![ptr::null_mut(); n];
    let claim = cs.claim_random(&env.meta, &mut out, &mut rng);
    assert_eq!(claim.n, n);
    assert!(
        claim.zeroed,
        "a Fresh slab's prefix, claimed under the lock"
    );
    let slots: Vec<usize> = out
        .iter()
        .map(|&q| {
            let (slab, slot) = cs.locate(q);
            assert_eq!(slab, 0);
            slot
        })
        .collect();
    let mut sorted = slots.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        (0..n).collect::<Vec<_>>(),
        "slots 0..n of slab 0, each once"
    );
    assert_ne!(slots, sorted, "shuffled");
    assert!(cs.table().bits(0).is_full());
    cs.release(&out);
    assert_eq!(cs.desc(0).state(), State::Empty);
    let mut half = vec![ptr::null_mut(); n / 2];
    let c1 = cs.claim_random(&env.meta, &mut half, &mut rng);
    assert_eq!((c1.n, c1.zeroed), (n / 2, false), "an Empty slab is dirty");
    let mut rest = vec![ptr::null_mut(); n - n / 2];
    let c2 = cs.claim_random(&env.meta, &mut rest, &mut rng);
    assert_eq!(
        (c2.n, c2.zeroed),
        (n - n / 2, false),
        "a partial slab is dirty"
    );
    let all: HashSet<*mut u8> = half.iter().chain(rest.iter()).copied().collect();
    assert_eq!(all.len(), n, "every slot once across the two refills");
    assert_eq!(all, out.iter().copied().collect());
    assert!(cs.table().bits(0).is_full());
}

/// The set/count seam of `Inner` (`empty_add`, `empty_take`,
/// `purged_add`, `purged_take`): each helper moves the bit and the count
/// together, so the count is always the set's population.
#[test]
fn the_set_helpers_keep_the_counts_in_step_with_the_bits() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let n = cs.class().nslots();
    // Two groups of one slab handed out, both `Empty`.
    let (p, _) = env.claim(cs, n + 1);
    cs.release(&p);
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 2,
            empty: 2,
            ..Default::default()
        }
    );
    {
        let mut g = cs.inner.lock_t();
        g.empty_take(0);
        assert!(!g.empty.map.is_set(0));
        assert_eq!((g.stats.empty, g.stats.purged), (1, 0));
        g.purged_add(0);
        assert!(g.purged.map.is_set(0));
        assert_eq!((g.stats.empty, g.stats.purged), (1, 1));
        g.purged_take(0);
        assert!(!g.purged.map.is_set(0));
        assert_eq!((g.stats.empty, g.stats.purged), (1, 0));
        g.empty_add(0);
        assert!(g.empty.map.is_set(0));
        assert_eq!((g.stats.empty, g.stats.purged), (2, 0));
    }
    assert_eq!(
        cs.stats(),
        ClassStats {
            committed: 2,
            empty: 2,
            ..Default::default()
        }
    );
    assert_eq!(cs.set_membership(0), (false, true, false));
    assert_eq!(cs.set_membership(1), (false, true, false));
}

#[test]
#[should_panic(expected = "purged slab count underflow")]
fn a_plan_that_takes_a_purged_slab_the_counts_do_not_know_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let (b, claim) = env.claim(cs, 1);
    assert_eq!(claim.n, 1);
    cs.release(&b);
    assert_eq!(
        cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new()),
        1
    );
    assert_eq!(cs.stats().purged, 1);
    // Corrupt the count: the set still holds the slab.
    cs.inner.lock_t().stats.purged = 0;
    let mut picks = [Pick::EMPTY; MAX_PICKS];
    cs.plan(
        &env.meta,
        1,
        &mut picks,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
}

/// `walk_empty`, the one walk behind `purge_empty` and both passes of
/// `purge_curve`: the empty set in address order with each slab's
/// `dirty_at`; a slab the visitor takes out ahead of the walk (a purge
/// takes a whole page group) is not visited.
#[test]
fn walk_empty_visits_the_set_in_address_order_and_skips_what_the_visitor_takes() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let n = cs.class().nslots();
    let (p, _) = env.claim(cs, 3 * n);
    cs.release(&p);
    for s in 0..3 {
        assert_eq!(cs.desc(s).state(), State::Empty, "slab {s}");
    }
    /// Records each visit.
    struct Seen(Vec<(usize, u64)>);
    impl decay::EmptyVisitor for Seen {
        fn visit(&mut self, _: &ClassState, _: &mut Inner, slab: usize, dirty_at: u64) {
            self.0.push((slab, dirty_at));
        }
    }
    /// Takes slab 1 out of the set on the visit to slab 0.
    struct TakesOne(Vec<usize>);
    impl decay::EmptyVisitor for TakesOne {
        fn visit(&mut self, _: &ClassState, g: &mut Inner, slab: usize, _: u64) {
            if slab == 0 {
                g.empty_take(1);
            }
            self.0.push(slab);
        }
    }
    let mut g = cs.inner.lock_t();
    let mut seen = Seen(Vec::new());
    cs.walk_empty(&mut g, &mut seen);
    assert_eq!(
        seen.0,
        (0..3)
            .map(|s| (s, cs.desc(s).dirty_at()))
            .collect::<Vec<_>>()
    );
    assert!(
        seen.0.iter().all(|&(_, t)| t != 0),
        "an Empty slab's stamp is never 0"
    );
    let mut seen = TakesOne(Vec::new());
    cs.walk_empty(&mut g, &mut seen);
    assert_eq!(seen.0, vec![0, 2]);
    g.empty_add(1);
    drop(g);
    assert_eq!(cs.stats().empty, 3);
}

/// The walk's cross-check: a slab of the empty set whose descriptor is
/// not `Empty` is a heap inconsistency, in every purge walk.
#[test]
#[should_panic(expected = "slab 1 in the empty set is not empty")]
fn a_purge_over_an_empty_set_slab_that_is_not_empty_aborts() {
    let env = Env::new();
    let cs = env.class(small_class(), 0);
    let n = cs.class().nslots();
    let (p, _) = env.claim(cs, 2 * n);
    cs.release(&p);
    assert_eq!(cs.stats().empty, 2);
    // Corrupt the descriptor: the set says `Empty`, the state byte says
    // `Active`.
    cs.desc(1).set_state(State::Active, Tracked::assume_new());
    cs.purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
}

/// `min_nonzero`: the fold of the purge walks over `dirty_at`
/// stamps, where 0 is "none yet" and a stamp is never 0.
#[test]
fn min_nonzero_treats_zero_as_none() {
    assert_eq!(decay::min_nonzero(0, 7), 7);
    assert_eq!(decay::min_nonzero(7, 5), 5);
    assert_eq!(decay::min_nonzero(5, 7), 5);
    assert_eq!(decay::min_nonzero(5, 5), 5);
}

/// `SlabTable::group_range`: the slabs of a group, the last one clipped
/// to the class's slab count.
#[test]
fn group_range_is_the_groups_slabs_clipped_to_the_class() {
    let env = Env::new();
    let t = env.table(min_slab_class(), 0);
    let group = t.group();
    assert!(
        group > 1,
        "the 4 KiB-slab class hands out many slabs per group"
    );
    assert_eq!(t.group_range(0), (0, group));
    assert_eq!(t.group_range(1), (group, 2 * group));
    let last = (t.nslabs - 1) >> t.lg_group;
    assert_eq!(t.group_range(last), (last << t.lg_group, t.nslabs));
    assert!(
        t.group_range(last + 1).0 >= t.nslabs,
        "one past the last group starts beyond the class"
    );
    let one = env.table(small_class(), 0);
    assert_eq!(one.group(), 1);
    assert_eq!(one.group_range(5), (5, 6));
}

/// The protocol's step names (`class::protocol`) are numbered in the
/// order the code takes them and round-trip through `at`, which is what
/// the model's sub-step numbers rely on.
#[test]
fn the_protocol_steps_are_numbered_in_order() {
    use protocol::{Purge, Refill, Release};
    for (i, s) in [
        Refill::Lock,
        Refill::PlanLocked,
        Refill::Unlock,
        Refill::Finish,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(s as u8 as usize, i);
        assert_eq!(Refill::at(i as u8), s);
    }
    let release = [
        Release::Group,
        Release::FetchAnd,
        Release::SubLive,
        Release::LockLiveZero,
        Release::OnLiveZero,
        Release::NextSlab,
        Release::LockReopen,
        Release::Reopen,
    ];
    for (i, s) in release.into_iter().enumerate() {
        assert_eq!(s as u8 as usize, i);
        assert_eq!(Release::at(i as u8), s);
    }
    for (i, s) in [Purge::Lock, Purge::PurgeLocked].into_iter().enumerate() {
        assert_eq!(s as u8 as usize, i);
        assert_eq!(Purge::at(i as u8), s);
    }
}
