//! The knob and the named scenarios (module docs in `mod.rs`, "One
//! knob replays the protocol before its fix"): [`Variant`], [`CURRENT`]
//! (the protocol as shipped), the [`Scenario`] shape, and the scenarios
//! the tests in `mod.rs` run.

use super::*;

/// The protocol knob (module docs).
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    /// `claim_prefix`: a zero pick's bits are claimed in `take` under the
    /// lock. `false` replays the pre-fix order.
    pub prefix_claim: bool,
    /// `plan` honours a fork epoch's floor. `false` plans as if no epoch
    /// were open, and a refill reuses the child's shared pages.
    pub cow_floor: bool,
    /// With lazy purges ([`Scenario::lazy`]), `next_slab` takes `Purged`
    /// slabs only from the floor up (`purge::purged_is_zero` false).
    /// `false` takes them anywhere, as with eager purges, and a refill
    /// reuses a lazily purged slab whose pages the child still shares.
    pub cow_lazy_floor: bool,
}

pub const CURRENT: Variant = Variant {
    prefix_claim: true,
    cow_floor: true,
    cow_lazy_floor: true,
};

/// A scenario: the threads (arena and program) and the geometry.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub threads: Vec<(u8, Vec<Op>)>,
    /// `lg_group`: 0 for one slab per group, 1 for two.
    pub lg_group: u8,
    /// Purges are lazy (`MADV_FREE`, `lazy_band:true` under headroom):
    /// a `Purged` slab keeps its contents and, across a fork, the pages
    /// the child shares, and `purge::purged_is_zero` is false. `false`:
    /// eager (`MADV_DONTNEED`), the default.
    pub lazy: bool,
}

/// The calloc race scenario (the mechanism of
/// `class::tests::a_zero_pick_never_receives_a_slot_another_claim_used_in_between`):
/// A refills one slot from a `Fresh` slab; B refills one slot from the
/// same class, writes it and frees it. Then, so the same window opens on
/// a `Purged` slab: A frees, purges, and refills again while B does its
/// refill, write, free a second time.
pub fn calloc_race() -> Scenario {
    Scenario {
        name: "calloc race (Fresh, then Purged)",
        threads: vec![
            (
                0,
                vec![Op::Refill(1), Op::Release, Op::Purge, Op::Refill(1)],
            ),
            (
                0,
                vec![
                    Op::Refill(1),
                    Op::Write(0),
                    Op::Release,
                    Op::Refill(1),
                    Op::Write(0),
                    Op::Release,
                ],
            ),
        ],
        lg_group: 1,
        lazy: false,
    }
}

/// Two threads filling and draining both slabs: multi-slab picks, full
/// words, the `reopen` path, `Empty` and `Purged` reuse.
pub fn churn() -> Scenario {
    Scenario {
        name: "churn over two slabs",
        threads: vec![
            (
                0,
                vec![
                    Op::Refill(3),
                    Op::Write(1),
                    Op::Release,
                    Op::Purge,
                    Op::Refill(2),
                ],
            ),
            (
                0,
                vec![
                    Op::Refill(2),
                    Op::Release,
                    Op::Refill(4),
                    Op::Write(0),
                    Op::Release,
                ],
            ),
        ],
        lg_group: 1,
        lazy: false,
    }
}

/// Three threads on one arena over both slabs: multi-slab picks, a
/// purge racing releases, three programs' steps interleaved. The largest
/// state space of the suite; three threads with five operations each did
/// not finish in ten minutes, which is what [`MAX_STATES`] is for.
pub fn three_way() -> Scenario {
    Scenario {
        name: "three threads, both slabs",
        threads: vec![
            (0, vec![Op::Refill(2), Op::Write(0), Op::Release]),
            (0, vec![Op::Refill(1), Op::Release, Op::Purge]),
            (0, vec![Op::Refill(3), Op::Write(1), Op::Release]),
        ],
        lg_group: 1,
        lazy: false,
    }
}

/// A fork epoch (`crate::cow`): A fills slab 0 and writes a slot, opens an
/// epoch (its floor is above slab 0, whose pages the child now shares),
/// frees its slots and refills; B allocates, frees and purges around it,
/// so that slab 0 is partial, `Empty` and `Purged` at some point of some
/// interleaving. No refill under the floor may take a slot of slab 0
/// while it is shared, and every other invariant holds.
pub fn cow_epoch() -> Scenario {
    Scenario {
        name: "fork epoch over a shared slab",
        threads: vec![
            (
                0,
                vec![
                    Op::Refill(3),
                    Op::Write(0),
                    Op::CowBegin,
                    Op::Release,
                    Op::Refill(2),
                    Op::CowEnd,
                ],
            ),
            (
                0,
                vec![Op::Refill(1), Op::Release, Op::Purge, Op::Refill(1)],
            ),
        ],
        lg_group: 0,
        lazy: false,
    }
}

/// A fork epoch over a lazily purged slab (`lazy`): A fills slab 0, writes
/// a slot and releases it all, purges it (lazily: its pages stay, and the
/// fork child shares them), opens an epoch (the floor is above slab 0)
/// and refills, while B refills one slot. No refill under the floor may
/// take slab 0, `Purged` or not: `next_slab` takes `Purged` slabs only
/// from the floor up.
pub fn cow_epoch_lazy() -> Scenario {
    Scenario {
        name: "fork epoch over a lazily purged slab",
        threads: vec![
            (
                0,
                vec![
                    Op::Refill(4),
                    Op::Write(0),
                    Op::Release,
                    Op::Purge,
                    Op::CowBegin,
                    Op::Refill(2),
                ],
            ),
            (0, vec![Op::Refill(1)]),
        ],
        lg_group: 0,
        lazy: true,
    }
}

/// A fork epoch whose floors leave nothing to take: A fills slab 0 and
/// releases it (`Empty`, its pages shared at the fork), opens an epoch
/// (the floor is above slab 0) and asks for one slot, while B fills slab
/// 1, the last. Where B's refill comes first, nothing is left above the
/// floor, so `plan` falls back to planning without it and takes slab 0:
/// an epoch never fails an allocation the heap has room for.
pub fn cow_epoch_exhausted() -> Scenario {
    Scenario {
        name: "fork epoch with no room above the floor",
        threads: vec![
            (
                0,
                vec![Op::Refill(4), Op::Release, Op::CowBegin, Op::Refill(1)],
            ),
            (0, vec![Op::Refill(4)]),
        ],
        lg_group: 0,
        lazy: false,
    }
}
