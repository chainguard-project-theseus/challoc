//! The set-free part of the test checker of what `tcaches::heap_at`
//! trusts (`r.wf()` and `r.home_at(p)` of the heap at `p`), shared by the
//! unit tests' `heap_at_tests::check` and the debug TLS check's
//! `tlscheck::deep` (`cfg(challoc_tls_check)`).
//!
//! [`set_free`] and [`homed_ok`] test every conjunct of `check` that needs
//! no allocation, in `check`'s order, and report the first broken one as a
//! [`Broken`] variant. `check` adds the set-based conjuncts (a node met
//! twice and a header of two nodes, through `seen`; two stacks overlapping
//! and a slot cached twice) and the zero prefix between the two.
//! `heap_at_tests::checker_transcribes_heap_wf_conjunct_by_conjunct` ties
//! [`Broken`] to `Heap::wf`, and `checker_fails_each_corrupted_fact` runs a
//! mutant for each variant through `check` (but the ones its module names),
//! so both cover the conjuncts `deep` tests too.
//!
//! No `std`: the TLS check runs inside the library, which cannot allocate
//! while it checks.

use super::{ArenaCaches, CacheHeader, FOREIGN, Heap, Shared};
use crate::layout::{Kind, NCLASSES};

/// A conjunct of a spec `heap_at_tests::check` transcribes that the heap
/// does not meet. Header, node and slot fields are addresses. The TLS
/// check's builds construct only the variants [`set_free`] and
/// [`homed_ok`] return.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Broken {
    /// `Heap::wf`: `self.shared == Shared::storage() as *const Shared`.
    Shared { got: usize, want: usize },
    /// `Heap::home_at`: home header `class` is not `home_hdr(p, class)`,
    /// `p + 128 + 64 * class`.
    HomeAt { class: usize },
    /// `Heap::home_at`: `p + size_of::<Heap>()` wraps.
    HomeWraps,
    /// `Heap::list_ok`: the `next` chain from `others` comes back to a
    /// node (the nodes are distinct).
    ListRepeats { node: usize },
    /// `others::block_ok`'s claim: the node is not in the metadata region.
    NodeOutsideMeta { node: usize },
    /// `node_placed`: `node + 64 + 64 * NCLASSES` wraps.
    NodeWraps { node: usize },
    /// `Heap::home_ok`: a header of the node is home header `class`.
    NodeOnHome { node: usize, class: usize },
    /// `owns_node` with `hdr_class` a function: two nodes share a header.
    HeaderTwice { hdr: usize },
    /// `others::block_ok`: `pad < 16`.
    PadRange { node: usize, pad: u8 },
    /// `others::block_ok`: `64 * pad <= n@.addr`.
    PadBelowZero { node: usize, pad: u8 },
    /// `others::block_ok`'s claim (`held(.., node_block(n, pad),
    /// block_size(), 64)`): the block is not 64-aligned metadata.
    BlockMisplaced { node: usize },
    /// Not a `Heap::wf` conjunct: a node's key is an arena index
    /// (`< MAX_ARENAS`) or `FOREIGN`.
    KeyRange { node: usize, key: u16 },
    /// `Heap::list_ok`: `k[i] != self.arena`.
    KeyIsHome { node: usize, key: u16 },
    /// `Heap::list_ok`: the keys are pairwise distinct.
    KeyRepeats { node: usize, key: u16 },
    /// `CacheHeader::inv`: `run_end <= low_water <= len <= cap`.
    HeaderOrder { hdr: usize },
    /// `CacheHeader::inv`: `cap <= MAX_NSLOTS`.
    HeaderCap { hdr: usize },
    /// `CacheHeader::inv`: `zeroed <= low_water`.
    HeaderZeroed { hdr: usize },
    /// `cell_in_meta`: the header is not 64 bytes of metadata.
    HeaderOutsideMeta { hdr: usize },
    /// `cell_in_meta`: the stack is not `8 * cap` bytes of metadata.
    StackOutsideMeta { hdr: usize },
    /// `stack_claimed`'s claim (`held(.., 8 * cap, 16)`): the stack is not
    /// 16-aligned.
    StackMisaligned { hdr: usize },
    /// `slot_entry`: `e@.addr == stored(plain(e, base), base)`.
    EntryNotStored { hdr: usize, i: usize },
    /// `slot_entry`: `process_regions().is_slot(c, plain(e, base))` for
    /// the header's class `c`.
    EntryNotSlot { hdr: usize, i: usize },
    /// `CachePerms::tracks`: an entry of one stack is an entry of another
    /// (`owner` maps it to one header).
    StacksOverlap { a: usize, b: usize },
    /// `CachePerms::tokened`: a slot is cached twice (each cached entry
    /// holds its slot's token, and there is one token per slot).
    SlotTwice { slot: usize },
    /// `CachePerms::zero_ok`: entry `i` of the zero prefix does not read
    /// as zero for its class's size.
    NotZero { hdr: usize, i: usize },
    /// `Heap::homed_ok`: `self.rehome ==> self.bound`.
    RehomeUnbound,
    /// `Heap::homed_ok`: a counted heap holds its home arena's ticket, so
    /// the arena's bound count is at least one.
    BoundUncounted { arena: u16 },
}

/// Whether `[a, a + len)` is in `s`'s metadata region
/// (`Regions::in_meta(a, len)`: the region is one range, so its first and
/// last byte are).
pub(crate) fn in_meta(s: &Shared, a: usize, len: usize) -> bool {
    len > 0
        && a.checked_add(len - 1)
            .is_some_and(|end| s.regions.in_meta(a) && s.regions.in_meta(end))
}

/// The address stack entry `e` stands for (`plain`): `pac::strip` of it
/// with pointer authentication, which is `pac::auth` of an entry signed
/// as `stored` signs it ([`Broken::EntryNotStored`]), and never faults on
/// one that is not, where `pac::auth` would on FEAT_FPAC hosts.
pub(crate) fn plain(e: *mut u8, pac: bool) -> usize {
    if pac {
        crate::sys::pac::strip(e as usize)
    } else {
        e as usize
    }
}

/// A header `h` at address `at` caching `class`: an element of the home
/// `caches` or of a node's.
pub(crate) struct Hdr<'a> {
    pub(crate) at: usize,
    pub(crate) class: usize,
    pub(crate) h: &'a CacheHeader,
}

/// What [`set_free`] hands its caller to test with sets as the walk meets
/// it: a node of the `others` list, before anything else of it is tested,
/// and each of a node's headers, after it is found not to be a home
/// header. The TLS check's builds pass a `seen` that reads neither.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Seen {
    Node(usize),
    Hdr(usize),
}

/// The headers of `h`, the heap at `p`: the home headers by class, then
/// each `others` node's by class, in list order, until `f` fails. The
/// list must be one [`set_free`] passed, which bounds it.
///
/// # Safety
/// As for [`set_free`].
pub(crate) unsafe fn headers<'a>(
    h: &'a Heap,
    mut f: impl FnMut(Hdr<'a>) -> Result<(), Broken>,
) -> Result<(), Broken> {
    for c in 0..NCLASSES {
        f(Hdr {
            at: &h.caches[c] as *const CacheHeader as usize,
            class: c,
            h: &h.caches[c],
        })?;
    }
    let mut o = h.others as *const ArenaCaches;
    while !o.is_null() {
        // SAFETY: a node of the list `set_free` walked; the caller keeps
        // the heap and its nodes live.
        let n = unsafe { &*o };
        for c in 0..NCLASSES {
            f(Hdr {
                at: o as usize + 64 + 64 * c,
                class: c,
                h: &n.caches[c],
            })?;
        }
        o = n.next;
    }
    Ok(())
}

/// The conjuncts of what `heap_at(p)` ensures that need no sets, of `h`,
/// the heap at `p`, created on `s`: `shared`, `Heap::home_at`, the
/// `others` walk (nodes in metadata, not wrapping, not on a home header,
/// pads, blocks, keys in range, not the home arena and distinct), every
/// header's `cell_ok` (`CacheHeader::inv`, `cell_in_meta`, stack
/// alignment), and every cached entry a slot start of its header's class
/// stored as a push stores it. The first broken one, in that order, or
/// the first error of `seen`, which the walk calls with each node and
/// node header ([`Seen`]).
///
/// The walk ends on a cycle without `seen`: a node met twice repeats its
/// key ([`Broken::KeyRepeats`]), and the keys are in range first.
///
/// # Safety
/// `h` is a live heap that nothing else uses while the check runs; every
/// `others` node the check reaches is read, so a corrupted list must stay
/// inside mapped memory.
pub(crate) unsafe fn set_free(
    h: &Heap,
    p: *const Heap,
    s: &Shared,
    mut seen: impl FnMut(Seen) -> Result<(), Broken>,
) -> Result<(), Broken> {
    // `Heap::wf`: `self.shared == Shared::storage() as *const Shared`.
    let want = s as *const Shared;
    if h.shared != want {
        return Err(Broken::Shared {
            got: h.shared as usize,
            want: want as usize,
        });
    }
    // `Heap::home_at`: the home headers are `home_hdr(p, c)`, and the
    // heap's block does not wrap.
    let base = p as usize;
    if base.checked_add(core::mem::size_of::<Heap>()).is_none() {
        return Err(Broken::HomeWraps);
    }
    for c in 0..NCLASSES {
        let at = &h.caches[c] as *const CacheHeader as usize;
        if at != base + 128 + 64 * c {
            return Err(Broken::HomeAt { class: c });
        }
    }
    let home = base + 128..base + 128 + 64 * NCLASSES;
    // `Heap::list_ok`: walk `others` along `next`; `l` is the walk's
    // nodes and `k` their `arena` fields, kept as a bitmap of the
    // `MAX_ARENAS` arenas and `FOREIGN`.
    let mut keys = [0u64; (crate::arena::MAX_ARENAS + 1).div_ceil(64)];
    let mut o = h.others as *const ArenaCaches;
    while !o.is_null() {
        let node = o as usize;
        seen(Seen::Node(node))?;
        if !in_meta(s, node, core::mem::size_of::<ArenaCaches>()) {
            return Err(Broken::NodeOutsideMeta { node });
        }
        // `node_placed`: `node@.addr + 64 + 64 * NCLASSES <= usize::MAX`.
        if node.checked_add(64 + 64 * NCLASSES).is_none() {
            return Err(Broken::NodeWraps { node });
        }
        // `Heap::home_ok`: no home header is `node_hdr(node, k)`; and,
        // each header having one class, no two nodes share one (`seen`).
        for k in 0..NCLASSES {
            let at = node + 64 + 64 * k;
            if home.contains(&at) && (at - home.start).is_multiple_of(64) {
                return Err(Broken::NodeOnHome {
                    node,
                    class: (at - home.start) / 64,
                });
            }
            seen(Seen::Hdr(at))?;
        }
        // SAFETY: the node lies in the metadata region (checked above);
        // the caller keeps the heap and its nodes live.
        let n = unsafe { &*o };
        // `others::block_ok`: `pad < 16`, `64 * pad <= n@.addr`, and the
        // block (`node_block(n, pad)`, `block_size()` bytes) is the
        // claim's: 64-aligned metadata.
        let pad = n.pad;
        if pad >= 16 {
            return Err(Broken::PadRange { node, pad });
        }
        if 64 * pad as usize > node {
            return Err(Broken::PadBelowZero { node, pad });
        }
        let block = node - 64 * pad as usize;
        let block_size = core::mem::size_of::<ArenaCaches>() + 64 * 15;
        if !block.is_multiple_of(64) || !in_meta(s, block, block_size) {
            return Err(Broken::BlockMisplaced { node });
        }
        // `Heap::list_ok`'s keys: an arena or `FOREIGN` (not a conjunct),
        // not the home arena, pairwise distinct.
        let key = n.arena;
        if key != FOREIGN && key as usize >= crate::arena::MAX_ARENAS {
            return Err(Broken::KeyRange { node, key });
        }
        if key == h.arena {
            return Err(Broken::KeyIsHome { node, key });
        }
        let bit = if key == FOREIGN {
            crate::arena::MAX_ARENAS
        } else {
            key as usize
        };
        if keys[bit / 64] & (1 << (bit % 64)) != 0 {
            return Err(Broken::KeyRepeats { node, key });
        }
        keys[bit / 64] |= 1 << (bit % 64);
        o = n.next;
    }
    // `cell_ok` of every header: `CacheHeader::inv`, `cell_in_meta`, and
    // `stack_claimed`'s alignment.
    // SAFETY: the caller's, and the list passed the walk above.
    unsafe {
        headers(h, |d| {
            let k = d.h;
            if !(k.run_end <= k.low_water && k.low_water <= k.len && k.len <= k.cap) {
                return Err(Broken::HeaderOrder { hdr: d.at });
            }
            if k.cap > crate::config::MAX_NSLOTS {
                return Err(Broken::HeaderCap { hdr: d.at });
            }
            if k.zeroed > k.low_water {
                return Err(Broken::HeaderZeroed { hdr: d.at });
            }
            if !in_meta(s, d.at, 64) {
                return Err(Broken::HeaderOutsideMeta { hdr: d.at });
            }
            if k.cap != 0 {
                if !in_meta(s, k.slots as usize, 8 * k.cap as usize) {
                    return Err(Broken::StackOutsideMeta { hdr: d.at });
                }
                if !(k.slots as usize).is_multiple_of(16) {
                    return Err(Broken::StackMisaligned { hdr: d.at });
                }
            }
            Ok(())
        })?;
    }
    // `CachePerms::tracks`: each of the first `len` entries holds a slot
    // start of the header's class, stored as a push stores it
    // (`slot_entry`).
    let pac = s.pac();
    // SAFETY: as above.
    unsafe {
        headers(h, |d| {
            let k = d.h;
            for (i, &e) in k.cached().iter().enumerate() {
                let plain = plain(e, pac);
                let stored = if pac {
                    crate::sys::pac::sign(plain, k.slots as usize)
                } else {
                    plain
                };
                if stored != e as usize {
                    return Err(Broken::EntryNotStored { hdr: d.at, i });
                }
                let slot = match s.regions.classify(plain) {
                    Kind::Slab { class, slab, .. } => {
                        class as usize == d.class
                            && !s.arena0.classes[d.class].is_guard_slab(slab as usize)
                    }
                    _ => false,
                };
                if !slot {
                    return Err(Broken::EntryNotSlot { hdr: d.at, i });
                }
            }
            Ok(())
        })
    }
}

/// [`set_free`], with no sets (a cycle ends at a repeated key), then
/// [`homed_ok`]: what the debug TLS check's `deep` tests.
///
/// # Safety
/// As for [`set_free`].
pub(crate) unsafe fn no_sets(h: &Heap, p: *const Heap, s: &Shared) -> Result<(), Broken> {
    // SAFETY: the caller's.
    unsafe { set_free(h, p, s, |_| Ok(())) }.and_then(|()| homed_ok(h, s))
}

/// `Heap::homed_ok` of `h`, created on `s`: `rehome ==> bound`, and a
/// counted heap's ticket is counted in its home arena.
pub(crate) fn homed_ok(h: &Heap, s: &Shared) -> Result<(), Broken> {
    if h.rehome && !h.bound {
        return Err(Broken::RehomeUnbound);
    }
    if h.bound && ((h.arena as usize) >= crate::arena::MAX_ARENAS || s.arenas.bound(h.arena) == 0) {
        return Err(Broken::BoundUncounted { arena: h.arena });
    }
    Ok(())
}
