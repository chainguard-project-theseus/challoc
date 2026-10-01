use super::*;
use std::vec::Vec;

/// Deterministic xorshift for synthetic offsets.
fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn synthetic(lg_region: u32, lg_stride: u32) -> Regions {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let gap = (1usize << lg_stride) - (1usize << lg_region);
    let mut offsets = [0usize; NCLASSES];
    for o in offsets.iter_mut() {
        *o = (xorshift(&mut s) as usize % (gap >> LG_MAX_SLAB)) << LG_MAX_SLAB;
    }
    let superblock = 0x1000_0000_0000usize;
    let sb_len = NCLASSES << lg_stride;
    let huge = (superblock + sb_len + (1 << 30), 1usize << 34);
    let meta = (huge.0 + huge.1 + (1 << 30), 1usize << 30);
    Regions::from_parts(
        superblock,
        lg_region,
        lg_stride,
        &offsets,
        huge,
        meta,
        12,
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

/// The never-committed ranges are page aligned, disjoint from every
/// region, and together with the regions cover the three mappings
/// exactly; there are at most `MAX_SEALED` of them.
#[test]
fn never_committed_is_the_complement_of_the_regions() {
    for (lg_region, lg_stride) in [(38, 40), (32, 34), (32, 33)] {
        let r = synthetic(lg_region, lg_stride);
        let (ranges, n) = r.never_committed();
        assert!(n <= MAX_SEALED && n >= NCLASSES, "{n} ranges");
        // Regions, in address order, with the mapping each belongs to.
        let mut regions: Vec<(usize, usize)> = (0..NCLASSES)
            .map(|c| (r.class_base[c], r.class_base[c] + r.region_len()))
            .collect();
        regions.push((r.huge_base, r.huge_base + r.huge_len));
        regions.push((r.meta_base, r.meta_base + r.meta_len));
        let page = r.page_size();
        let mut covered = 0usize;
        for m in &ranges[..n] {
            assert!(
                m.len > 0 && m.addr % page == 0 && m.len % page == 0,
                "{m:x?}"
            );
            let (lo, hi) = (m.addr, m.addr + m.len);
            for &(a, b) in &regions {
                assert!(
                    hi <= a || lo >= b,
                    "range {m:x?} overlaps region {a:#x}-{b:#x}"
                );
            }
            assert!(
                [r.superblock_map, r.huge_map, r.meta_map]
                    .iter()
                    .any(|map| map.addr <= lo && hi <= map.addr + map.len),
                "range {m:x?} outside every mapping"
            );
            covered += m.len;
        }
        for &(a, b) in &regions {
            covered += b - a;
        }
        let total = r.superblock_map.len + r.huge_map.len + r.meta_map.len;
        assert_eq!(
            covered, total,
            "ranges and regions do not tile the mappings"
        );
    }
}

#[test]
fn class_count_matches_quantum() {
    if QUANTUM == 16 {
        assert_eq!(NCLASSES, 52);
    } else {
        assert_eq!(NCLASSES, 56);
    }
    assert_eq!(CLASSES.len(), NCLASSES);
}

#[test]
fn class_sizes_follow_the_design() {
    let expected_head: &[usize] = if QUANTUM == 16 {
        &[
            16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512,
        ]
    } else {
        &[
            8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256,
        ]
    };
    for (i, &want) in expected_head.iter().enumerate() {
        assert_eq!(CLASSES[i].size(), want, "class {i}");
    }
    assert_eq!(CLASSES[0].size(), QUANTUM);
    assert_eq!(CLASSES[NCLASSES - 1].size(), MEDIUM_MAX);
    for w in CLASSES.windows(2) {
        assert!(w[0].size < w[1].size, "sizes must be strictly monotonic");
    }
    for c in CLASSES.iter() {
        assert_eq!(c.size() % QUANTUM, 0);
    }
}

#[test]
fn slab_geometry() {
    for (i, c) in CLASSES.iter().enumerate() {
        let slab = c.slab_size();
        assert!(slab.is_power_of_two());
        assert!(
            (MIN_SLAB..=MAX_SLAB).contains(&slab),
            "class {i} slab {slab}"
        );
        assert_eq!(c.nslots(), slab / c.size());
        assert!(c.nslots() * c.size() <= slab);
        // Slab bits: at least nslots, whole words or one sub-word
        // range, a line-sized group of slabs spanning at most
        // 1 << LG_GROUP_SPAN bytes, and no more padding than needed.
        let b = c.slab_bits();
        assert!(b >= c.nslots(), "class {i}");
        assert!(
            b.is_power_of_two() || b % LINE_BITS == 0,
            "class {i}: slab_bits {b}"
        );
        assert!(
            b >= 64 || 64 % b == 0,
            "class {i}: sub-word slab_bits {b} must divide a word"
        );
        assert_eq!(c.bitmap_words(), b.div_ceil(64));
        let g = c.slabs_per_line();
        assert!(
            g.is_power_of_two() && (g == 1 || g * b == LINE_BITS),
            "class {i}"
        );
        assert!(
            g << c.lg_slab <= 1 << LG_GROUP_SPAN || g == 1,
            "class {i}: group spans {} bytes",
            g << c.lg_slab
        );
        assert_eq!(b, slab_bits_for(c.nslots(), c.lg_slab as u32));
        let cap = (1usize << LG_GROUP_SPAN >> c.lg_slab).max(1);
        if g == cap.min(LINE_BITS / c.nslots().next_power_of_two().min(LINE_BITS)) && g < cap {
            // Not raised for the span cap: no more padding than needed.
            if b <= LINE_BITS {
                assert!(
                    b / 2 < c.nslots(),
                    "class {i}: slab_bits {b} over-padded for {} slots",
                    c.nslots()
                );
            } else {
                assert!(b - LINE_BITS < c.nslots(), "class {i}");
            }
        }
        if c.size() < SMALL_SLAB_BELOW {
            assert_eq!(
                slab,
                1 << LG_SMALL_SLAB,
                "class {i}: slab below {SMALL_SLAB_BELOW}"
            );
        } else if c.size() <= SMALL_MAX {
            // The smallest slab from MIN_SLAB up with waste under 1/32.
            assert!(slab <= 1 << LG_SMALL_SLAB, "class {i}");
            assert!(
                slab - c.nslots() * c.size() <= slab / SLAB_WASTE_DIV,
                "class {i} wastes too much"
            );
            if slab > MIN_SLAB {
                let half = slab / 2;
                assert!(
                    half % c.size() > half / SLAB_WASTE_DIV,
                    "class {i}: a {half}-byte slab would do"
                );
            }
        } else {
            assert_eq!(
                slab,
                (16 * c.size())
                    .next_power_of_two()
                    .max(1 << LG_MEDIUM_MIN_SLAB)
            );
        }
        assert!(c.nslots() >= 4, "class {i} has {} slots", c.nslots());
        match c.tier() {
            Tier::Small => {
                assert!(c.size() <= SMALL_MAX);
                assert!(c.nslots() <= 8192);
            }
            Tier::Medium => {
                assert!(c.size() > SMALL_MAX);
                assert!(c.nslots() >= 16);
            }
        }
        // The thresholds' unit is a 64 KiB slab's worth of slots.
        assert_eq!(c.compact_slots(), slab.max(1 << LG_COMPACT_UNIT) / c.size());
        // The slot-start test's constants.
        assert_eq!(c.slab_mask(), slab - 1, "class {i}");
        assert_eq!(c.tz as u32, c.size().trailing_zeros(), "class {i}");
        assert_eq!(((c.size() >> c.tz) as u32).wrapping_mul(c.inv_odd), 1, "class {i}");
    }
}

/// The slot-start check by a fixed-point reciprocal and a multiply back:
/// `slot = offset * ceil(2^32 / size) >> 32`, valid when `slot < nslots`
/// and `slot * size == offset`. The reference [`Class::slot_of_offset`]
/// is held to.
fn reciprocal_slot_of_offset(cls: &Class, offset: usize) -> (usize, bool) {
    let recip = (1u64 << 32).div_ceil(cls.size() as u64);
    let slot = ((offset as u64 * recip) >> 32) as usize;
    (slot, slot < cls.nslots() && slot * cls.size() == offset)
}

/// Design 3: `slot_of_offset`'s multiply-and-rotate test accepts exactly
/// the offsets the reciprocal check and the definition (`size` divides
/// the offset, the quotient is below `nslots`) accept, with the same slot,
/// for every byte offset of a slab of every class in the table of every
/// supported page size (4, 8, 16, 32 and 64 KiB; the table above 64 KiB
/// is the 64 KiB one).
#[test]
fn slot_of_offset_is_exactly_the_reciprocal_check() {
    let mut seen: Vec<Class> = Vec::new();
    let mut offsets = 0usize;
    for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
        for (c, cls) in classes_for_page(lg_page).iter().enumerate() {
            if seen.contains(cls) {
                continue;
            }
            seen.push(*cls);
            let size = cls.size();
            let mut accepted = 0;
            for off in 0..cls.slab_size() {
                let (slot, valid) = cls.slot_of_offset(off);
                let (rslot, rvalid) = reciprocal_slot_of_offset(cls, off);
                if valid != rvalid || valid != (off % size == 0 && off / size < cls.nslots()) {
                    panic!("class {c} ({size} B, slab 2^{}) offset {off}: new {valid}, reciprocal {rvalid}", cls.lg_slab);
                }
                if valid {
                    assert_eq!((slot, rslot), (off / size, off / size), "class {c} offset {off}");
                    accepted += 1;
                }
            }
            assert_eq!(accepted, cls.nslots(), "class {c}");
            offsets += cls.slab_size();
        }
    }
    assert!(seen.len() > NCLASSES, "the raised classes were covered too");
    assert!(offsets > 1 << 24);
}

/// Design 3: the geometry a kernel with pages
/// above 4 KiB adopts. Every class's slab is at least the page (up to
/// 64 KiB), only the 512..1024-byte classes change, and for those the
/// slot arithmetic and its constants, the bitmap and descriptor
/// invariants and the compaction unit hold as at 4 KiB.
#[test]
fn page_sized_geometry_for_large_pages() {
    assert_eq!(classes_for_page(LG_MIN_SLAB), BASE_CLASSES);
    assert_eq!(
        classes_for_page(LG_MIN_SLAB - 1),
        BASE_CLASSES,
        "a smaller page changes nothing"
    );
    assert_eq!(
        classes_for_page(LG_MAX_SLAB),
        classes_for_page(LG_SMALL_SLAB),
        "capped at 64 KiB"
    );
    for lg_page in LG_MIN_SLAB..=LG_SMALL_SLAB {
        let t = classes_for_page(lg_page);
        let mut changed = 0;
        for (c, (cls, base)) in t.iter().zip(BASE_CLASSES.iter()).enumerate() {
            assert_eq!((cls.size, cls.inv_odd, cls.tz), (base.size, base.inv_odd, base.tz), "class {c}");
            assert_eq!(cls.slab_mask(), cls.slab_size() - 1, "class {c}");
            assert_eq!(cls.lg_slab as u32, (base.lg_slab as u32).max(lg_page), "class {c} at 2^{lg_page}");
            assert_eq!(cls.lg_slab as u32, lg_slab_at(cls.size(), lg_page));
            assert!(
                cls.slab_size() >= 1 << lg_page,
                "class {c}: slab below the page"
            );
            if cls == base {
                continue;
            }
            changed += 1;
            assert!(
                cls.size() >= SMALL_SLAB_BELOW && cls.size() <= SMALL_MAX,
                "class {c}: only the small-slab classes move"
            );
            assert_eq!(cls.nslots(), cls.slab_size() / cls.size());
            assert!(cls.nslots() >= 4);
            assert!(
                cls.slab_size() % cls.size() <= cls.slab_size() / SLAB_WASTE_DIV,
                "class {c}: waste"
            );
            // Slot arithmetic round-trips for every slot and rejects
            // every other quantum-aligned offset.
            for slot in 0..cls.nslots() {
                assert_eq!(
                    cls.slot_of_offset(slot * cls.size()),
                    (slot, true),
                    "class {c} slot {slot}"
                );
            }
            for off in (0..cls.slab_size()).step_by(QUANTUM) {
                let (slot, valid) = cls.slot_of_offset(off);
                assert_eq!(
                    valid,
                    off % cls.size() == 0 && off / cls.size() < cls.nslots(),
                    "class {c} offset {off}"
                );
                if valid {
                    assert_eq!(slot, off / cls.size());
                }
            }
            // Bitmap layout (design 4): whole words or one sub-word
            // range, a line holds whole slabs, a line's slabs span at
            // most `LG_GROUP_SPAN`, and the release scratch bitmap holds
            // a slab.
            let b = cls.slab_bits();
            assert!(b >= cls.nslots(), "class {c}");
            assert!(
                (b.is_power_of_two() && b <= LINE_BITS) || b % LINE_BITS == 0,
                "class {c}: slab_bits {b}"
            );
            if b < LINE_BITS {
                assert_eq!(cls.slabs_per_line() * b, LINE_BITS, "class {c}");
            } else {
                assert_eq!(cls.slabs_per_line(), 1);
            }
            assert!(
                cls.slabs_per_line() == 1
                    || cls.slabs_per_line() * cls.slab_size() <= 1 << LG_GROUP_SPAN,
                "class {c}"
            );
            assert!(cls.bitmap_words() <= MAX_SLAB_WORDS);
            // Compaction thresholds count 64 KiB of slots (heap/).
            assert_eq!(
                cls.compact_slots(),
                (1usize << LG_COMPACT_UNIT).max(cls.slab_size()) / cls.size()
            );
            if cls.lg_slab as u32 >= LG_COMPACT_UNIT {
                assert_eq!(cls.compact_slots(), cls.nslots());
            }
        }
        let expect = BASE_CLASSES
            .iter()
            .filter(|c| (c.lg_slab as u32) < lg_page)
            .count();
        assert_eq!(changed, expect, "at 2^{lg_page}");
    }
    // The 64 KiB table by the numbers (jemalloc's bins at LG_PAGE 16
    // have the same regs for 512 and 1024), and the 16 KiB one.
    let at = |lg_page: u32, size: u32| -> Class {
        *classes_for_page(lg_page)
            .iter()
            .find(|c| c.size == size)
            .unwrap()
    };
    for (size, nslots) in [(512, 128), (640, 102), (768, 85), (896, 73), (1024, 64)] {
        let c = at(LG_SMALL_SLAB, size);
        assert_eq!(
            (c.lg_slab as u32, c.nslots),
            (LG_SMALL_SLAB, nslots),
            "{size} at 64 KiB"
        );
        // 128 bits per slab for the first four (four slabs per bitmap
        // line), 64 for the last (eight per line).
        assert_eq!(
            c.slab_bits as u32,
            nslots.next_power_of_two(),
            "{size} at 64 KiB"
        );
        assert_eq!(c.slabs_per_line(), if size == 1024 { 8 } else { 4 });
    }
    assert!(
        classes_for_page(LG_SMALL_SLAB)
            .iter()
            .all(|c| c.lg_slab as u32 >= LG_SMALL_SLAB)
    );
    for (size, nslots) in [(512, 32), (640, 25), (768, 21), (896, 18), (1024, 16)] {
        let c = at(14, size);
        assert_eq!(
            (c.lg_slab as u32, c.nslots),
            (14, nslots),
            "{size} at 16 KiB"
        );
    }
    assert!(classes_for_page(14).iter().all(|c| c.lg_slab as u32 >= 14));
}

/// `adopt_page_geometry`'s writer, on a copy: it writes exactly the
/// raised classes' geometry and nothing else, and reports the count.
#[test]
fn apply_page_geometry_writes_only_the_raised_classes() {
    let mut t = BASE_CLASSES;
    assert_eq!(unsafe { apply_page_geometry(&mut t, LG_MIN_SLAB) }, 0);
    assert_eq!(t, BASE_CLASSES);
    let n = unsafe { apply_page_geometry(&mut t, LG_SMALL_SLAB) };
    assert_eq!(t, classes_for_page(LG_SMALL_SLAB));
    assert_eq!(n, 5, "512, 640, 768, 896 and 1024");
    let mut t = BASE_CLASSES;
    assert_eq!(
        unsafe { apply_page_geometry(&mut t, 14) },
        3,
        "512, 896 and 1024; 640 and 768 already have 16 KiB slabs"
    );
    assert_eq!(t, classes_for_page(14));
    // The host test process never adopts a page: the live table is the
    // compile-time one.
    assert_eq!(&*CLASSES, &BASE_CLASSES);
    assert_eq!(page_sized_classes(), None);
}

#[test]
fn small_slab_classes_are_the_pinned_range() {
    // (size, slab, slots, compaction unit in slots): the 512..1024 classes
    // get 4..16 KiB slabs (the smallest wasting under 1/32), their
    // neighbours keep 64 KiB.
    for (size, slab, nslots, unit) in [
        (448, 65536, 146, 146),
        (512, 4096, 8, 128),
        (640, 16384, 25, 102),
        (768, 16384, 21, 85),
        (896, 8192, 9, 73),
        (1024, 4096, 4, 64),
        (1280, 65536, 51, 51),
        (4096, 65536, 16, 16),
        (8192, 131072, 16, 16),
    ] {
        let c = &CLASSES[class_of(size).unwrap()];
        assert_eq!(c.size(), size);
        assert_eq!(
            (c.slab_size(), c.nslots(), c.compact_slots()),
            (slab, nslots, unit),
            "size {size}"
        );
    }
    assert_eq!(lg_slab_for(64), 16);
    assert_eq!(lg_slab_for(SMALL_SLAB_BELOW - 1), LG_SMALL_SLAB);
}

#[test]
fn every_request_maps_to_the_tightest_class() {
    for n in 0..=MEDIUM_MAX {
        let c = class_of(n).unwrap_or_else(|| panic!("no class for {n}"));
        assert_eq!(Some(c), class_of_slow(n), "request {n}");
        let sz = CLASSES[c].size();
        assert!(sz >= n, "request {n} got class {c} of {sz}");
        if c > 0 {
            assert!(
                CLASSES[c - 1].size() < n,
                "request {n} should fit class {}",
                c - 1
            );
        }
        assert_eq!(class_size_of(n), Some(sz));
    }
    assert_eq!(class_of(0), Some(0));
    assert_eq!(class_of(MEDIUM_MAX + 1), None);
    assert_eq!(class_of(usize::MAX), None);
    assert_eq!(class_size_of(1 << 40), None);
}

#[test]
fn from_parts_layout_invariants() {
    let r = synthetic(32, 34);
    assert_eq!(r.region_len(), 1 << 32);
    assert_eq!(r.stride(), 1 << 34);
    assert_eq!(r.gap(), (1 << 34) - (1 << 32));
    for c in 0..NCLASSES {
        let base = r.class_base[c];
        assert_eq!(base & (MAX_SLAB - 1), 0);
        let start = r.superblock + c * r.stride();
        assert!(base >= start && base + r.region_len() <= start + r.stride());
        assert_eq!(r.nslabs(c), r.region_len() / CLASSES[c].slab_size());
        assert_eq!(r.slab_addr(c, 0), base);
        assert_eq!(r.slab_addr(c, 3), base + 3 * CLASSES[c].slab_size());
        assert_eq!(
            r.slot_addr(c, 3, 2),
            r.slab_addr(c, 3) + 2 * CLASSES[c].size()
        );
    }
    assert_eq!(r.huge_pages(), r.huge_len / 4096);
    assert_eq!(r.huge_addr(5), r.huge_base + 5 * 4096);
}

#[test]
fn slot_arithmetic_round_trips_for_every_slot() {
    let r = synthetic(32, 34);
    for c in 0..NCLASSES {
        let cls = &CLASSES[c];
        let last = r.nslabs(c) - 1;
        for slab in [0usize, 1, 7, last / 2, last] {
            for slot in 0..cls.nslots() {
                let p = r.slot_addr(c, slab, slot);
                assert_eq!(
                    r.classify(p),
                    Kind::Slab {
                        class: c as u32,
                        slab: slab as u32,
                        slot: slot as u32
                    },
                    "class {c} slab {slab} slot {slot}"
                );
            }
        }
    }
}

#[test]
fn every_in_slab_offset_classifies_exactly() {
    let r = synthetic(32, 34);
    for c in 0..NCLASSES {
        let cls = &CLASSES[c];
        let last = r.nslabs(c) - 1;
        for slab in [0usize, last] {
            let base = r.slab_addr(c, slab);
            for offset in 0..cls.slab_size() {
                let want = if offset % cls.size() == 0 && offset / cls.size() < cls.nslots() {
                    Kind::Slab {
                        class: c as u32,
                        slab: slab as u32,
                        slot: (offset / cls.size()) as u32,
                    }
                } else {
                    Kind::Invalid
                };
                assert_eq!(
                    r.classify(base + offset),
                    want,
                    "class {c} slab {slab} offset {offset}"
                );
            }
        }
    }
}

/// The sized free's `slot_in_class` agrees with `slab_slot` for every
/// class, at both ends of the region and at every in-slab offset, and
/// answers `None` for a pointer anywhere but in that class's region.
#[test]
fn slot_in_class_agrees_with_slab_slot() {
    let r = synthetic(32, 34);
    for c in 0..NCLASSES {
        let cls = &CLASSES[c];
        let last = r.nslabs(c) - 1;
        for slab in [0usize, last] {
            let base = r.slab_addr(c, slab);
            for offset in 0..cls.slab_size() {
                let p = base + offset;
                let want = r.slab_slot(p).map(|(cc, s, slot)| {
                    assert_eq!(cc, c, "slab_slot names another class at offset {offset}");
                    (s, slot)
                });
                assert_eq!(
                    r.slot_in_class(c, p),
                    want,
                    "class {c} slab {slab} offset {offset}"
                );
                if offset % cls.size() == 0 && offset / cls.size() < cls.nslots() {
                    assert_eq!(want, Some((slab, offset / cls.size())));
                }
            }
        }
        // Outside the region: the bytes on either side of it, another
        // class's slot start, the huge and metadata regions, both ends
        // of the address space.
        let other = (c + 1) % NCLASSES;
        for p in [
            r.class_base[c] - 1,
            r.class_base[c] + r.region_len(),
            r.slot_addr(other, 0, 0),
            r.huge_base,
            r.meta_base,
            0,
            usize::MAX,
        ] {
            assert_eq!(r.slot_in_class(c, p), None, "class {c} pointer {p:#x}");
        }
    }
}

#[test]
fn misaligned_and_out_of_region_pointers_are_invalid() {
    let r = synthetic(32, 34);
    for c in 0..NCLASSES {
        let base = r.class_base[c];
        let end = base + r.region_len();
        let stride_start = r.superblock + c * r.stride();
        assert_eq!(r.classify(base - 1), Kind::Invalid, "byte before class {c}");
        assert_eq!(r.classify(base - MAX_SLAB), Kind::Invalid);
        assert_eq!(
            r.classify(end),
            Kind::Invalid,
            "first gap byte after class {c}"
        );
        assert_eq!(r.classify(end + 12345), Kind::Invalid);
        assert_eq!(r.classify(stride_start + r.stride() - 1), Kind::Invalid);
        if stride_start != base {
            assert_eq!(r.classify(stride_start), Kind::Invalid);
        }
        for k in 1..QUANTUM {
            assert_eq!(r.classify(base + k), Kind::Invalid, "class {c} +{k}");
        }
    }
    assert_eq!(r.classify(0), Kind::Invalid);
    assert_eq!(r.classify(usize::MAX), Kind::Invalid);
    assert_eq!(r.classify(r.superblock - 1), Kind::Invalid);
    assert_eq!(
        r.classify(r.superblock + NCLASSES * r.stride()),
        Kind::Invalid
    );
    assert_eq!(
        r.classify(r.superblock + (NCLASSES + 3) * r.stride()),
        Kind::Invalid
    );
}

#[test]
fn huge_and_meta_classification() {
    let r = synthetic(32, 34);
    let page = r.page_size();
    assert_eq!(r.classify(r.huge_base), Kind::Huge { page: 0 });
    assert_eq!(r.classify(r.huge_base + 17 * page), Kind::Huge { page: 17 });
    assert_eq!(
        r.classify(r.huge_addr(r.huge_pages() - 1)),
        Kind::Huge {
            page: r.huge_pages() - 1
        }
    );
    assert_eq!(r.classify(r.huge_base + 1), Kind::Invalid);
    assert_eq!(r.classify(r.huge_base + page - 1), Kind::Invalid);
    assert_eq!(r.classify(r.huge_base + page + 8), Kind::Invalid);
    assert_eq!(r.classify(r.huge_base - page), Kind::Invalid);
    assert_eq!(r.classify(r.huge_base + r.huge_len), Kind::Invalid);
    assert!(r.in_meta(r.meta_base));
    assert!(r.in_meta(r.meta_base + r.meta_len - 1));
    assert!(!r.in_meta(r.meta_base + r.meta_len));
    assert!(!r.in_meta(r.meta_base - 1));
    assert_eq!(r.classify(r.meta_base), Kind::Invalid);
    assert_eq!(r.classify(r.meta_base + page), Kind::Invalid);
}

#[test]
fn classes_do_not_overlap_each_other_or_huge() {
    let r = synthetic(32, 34);
    let mut ranges: Vec<(usize, usize)> = (0..NCLASSES)
        .map(|c| (r.class_base[c], r.class_base[c] + r.region_len()))
        .collect();
    ranges.push((r.huge_base, r.huge_base + r.huge_len));
    ranges.push((r.meta_base, r.meta_base + r.meta_len));
    ranges.sort();
    for w in ranges.windows(2) {
        assert!(w[0].1 <= w[1].0, "{:?} overlaps {:?}", w[0], w[1]);
    }
}

#[test]
fn minimal_stride_layout_still_classifies() {
    // The last-resort layout: 4 GiB regions at a 2x stride.
    let r = synthetic(32, 33);
    for c in [0usize, NCLASSES / 2, NCLASSES - 1] {
        let cls = &CLASSES[c];
        let last = r.nslabs(c) - 1;
        for slab in [0usize, last] {
            for slot in [0usize, 1, cls.nslots() - 1] {
                let p = r.slot_addr(c, slab, slot);
                assert_eq!(
                    r.classify(p),
                    Kind::Slab {
                        class: c as u32,
                        slab: slab as u32,
                        slot: slot as u32
                    }
                );
            }
        }
        assert_eq!(r.classify(r.class_base[c] + r.region_len()), Kind::Invalid);
    }
}
