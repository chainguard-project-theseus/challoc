//! The knob and the named scenarios (module docs in `mod.rs`, "One
//! knob replays the protocol before its fix"): [`Variant`], [`CURRENT`]
//! (the protocol as shipped), the [`Scenario`] shape, and the scenarios
//! the tests in `mod.rs` run.

use super::*;

/// The protocol knob (module docs).
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    /// Plan Phase 1.5 task 7 (c): a zero pick's bits are claimed in
    /// `take` under the lock. `false` replays the pre-fix order.
    pub prefix_claim: bool,
}

pub const CURRENT: Variant = Variant {
    prefix_claim: true,
};

/// A scenario: the threads (arena and program) and the geometry.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub threads: Vec<(u8, Vec<Op>)>,
    /// `lg_group`: 0 for one slab per group, 1 for two.
    pub lg_group: u8,
}

/// The calloc race scenario (plan Phase 1.5 task 7, the campaign's
/// mechanism and `class::tests::a_zero_pick_never_receives_a_slot_another_claim_used_in_between`):
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
    }
}
