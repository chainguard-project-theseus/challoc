//! The knobs and the named scenarios (module docs in `mod.rs`, "Two
//! knobs replay the protocol before its fixes"): [`Variant`] and
//! [`Section`], [`CURRENT`] (the protocol as shipped), the [`Scenario`]
//! shape, and the scenarios the tests in `mod.rs` run.

use super::*;

/// The protocol knobs (module docs).
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    /// Plan Phase 1.5 task 7 (c): a zero pick's bits are claimed in
    /// `take` under the lock. `false` replays the pre-fix order.
    pub prefix_claim: bool,
    /// Plan Phase 1.5 task 15: `on_live_zero` and `reopen` test the
    /// owner word under the lock before touching the arena's sets.
    pub owner_check: bool,
    /// Per-CPU mode: how the pop and push touch the cid's cache.
    pub section: Section,
    /// Check that every commit to a cache happened on the cid that picked
    /// it (the property the rseq abort provides). Off, the buggy variants
    /// run on to the corruption that follows (a slot handed out twice, a
    /// push lost) so a test can show it by name.
    pub strict_cid: bool,
    /// Per-CPU mode, challoc PR #4: a refill push-back onto a non-empty
    /// stack clips the zero prefix at the old `len` (`rseq::push_batch`).
    /// `false` replays the code before it, which left `zeroed` alone, so
    /// a dirty batch landed inside a prefix that pops had left above
    /// `len` and was reported zero.
    pub batch_clip: bool,
    /// Per-CPU mode, challoc PR #5: the pop reads `zeroed` for its zero
    /// flag inside the section, before the commit store, as the aarch64
    /// `pop_raw` always did. `false` replays the x86_64 code before it,
    /// which compared after the commit, outside the section: another
    /// thread on the cid could refill the emptied cache with a zero batch
    /// in between and the pop reported its dirty slot zero.
    pub pop_zero_in_section: bool,
}

/// The per-CPU pop and push (design 15, plan Phase 2.5 task 8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// The protocol: `mm_cid` read inside an rseq critical section, the
    /// cache index derived from that read only, one commit store at the
    /// end; a scheduler event before the commit aborts to the section's
    /// start.
    Rseq,
    /// The bug the plan's "cache index derived only from the in-section
    /// read" rules out: `mm_cid` read before the section, the section
    /// (and its abort) covering the reads and the store only.
    CidOutside,
    /// The bug the plan's "pop and push inside rseq critical sections"
    /// rules out: plain loads and stores under whatever cid was read.
    None,
}

pub const CURRENT: Variant = Variant {
    prefix_claim: true,
    owner_check: true,
    section: Section::Rseq,
    strict_cid: true,
    batch_clip: true,
    pop_zero_in_section: true,
};

/// A scenario: the threads (arena and program) and the geometry.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub threads: Vec<(u8, Vec<Op>)>,
    /// `lg_group`: 0 for one slab per group, 1 for two.
    pub lg_group: u8,
    /// The `group_handoff` key.
    pub group_handoff: bool,
    /// The per-CPU mode (module docs): caches per cid, every section
    /// entry on any cid, `Pop`/`Push` in the programs. A thread's arena
    /// then follows the cid of its pop (the refill's arena); the
    /// `(arena, program)` arena is the one its `Purge` runs on.
    pub percpu: bool,
    /// Per-CPU mode: slots a `Pop` claims when its cache is empty (the
    /// real code's `cap / 2`), one handed out and the rest pushed back.
    /// [`REFILL`] (2) pushes back one slot; 3 pushes back a zero batch of
    /// two, the least that leaves `zeroed` above a non-empty `len` after
    /// a pop, at a larger state space.
    pub refill: u8,
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
        group_handoff: false,
        percpu: false,
        refill: REFILL,
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
        group_handoff: false,
        percpu: false,
        refill: REFILL,
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
        group_handoff: false,
        percpu: false,
        refill: REFILL,
    }
}

/// The group handoff (plan Phase 1.5 task 15): arena 0's two threads use
/// slab 0 and free it; arena 1, with nothing of its own, adopts the idle
/// group and uses the slab while arena 0's `on_live_zero` for it may
/// still be pending.
pub fn handoff() -> Scenario {
    Scenario {
        name: "group handoff",
        threads: vec![
            (0, vec![Op::Refill(1), Op::Release]),
            (0, vec![Op::Refill(1), Op::Release]),
            (1, vec![Op::Refill(1), Op::Release]),
        ],
        lg_group: 0,
        group_handoff: true,
        percpu: false,
        refill: REFILL,
    }
}

/// The per-CPU sharing scenario (design 15, plan Phase 2.5 task 8):
/// three threads on two cids, so the caches are shared over time and
/// every section entry may find another thread's read of its cid
/// outstanding. The first pop on a cid refills from the cid's arena and
/// pushes the rest of the batch back into whatever cache the thread is
/// on then; the first thread writes its slot (the zero prefix's clip) and
/// pushes it, onto a full cache when the others' refills got there first
/// (the flush). Three threads with two pushes each is 2 M states, past
/// the budget; two of the three only pop.
pub fn percpu_share() -> Scenario {
    Scenario {
        name: "per-CPU: three threads on two cids",
        threads: vec![
            (0, vec![Op::Pop, Op::Write(0), Op::Push(0)]),
            (0, vec![Op::Pop]),
            (0, vec![Op::Pop]),
        ],
        lg_group: 0,
        group_handoff: false,
        percpu: true,
        refill: REFILL,
    }
}

/// A thread that pops twice, writes, and pushes both slots later: the
/// kernel is free to move it between the pops and the pushes, so a slot
/// of arena 0 popped from cache 0 is pushed into cache 1
/// (`Outcome::cross_pushes` counts it), a full cache flushes, and the
/// flush releases slots of arena 0 through cache 1 (`release_routed`),
/// while a second thread pops on either cid.
pub fn percpu_migrate() -> Scenario {
    Scenario {
        name: "per-CPU: pop on one cid, push on another",
        threads: vec![
            (
                0,
                vec![Op::Pop, Op::Pop, Op::Write(1), Op::Push(1), Op::Push(0)],
            ),
            (0, vec![Op::Pop]),
        ],
        lg_group: 0,
        group_handoff: false,
        percpu: true,
        refill: REFILL,
    }
}

/// The purge under the mode: a thread pops both slots of its refill,
/// frees them directly (`Release` of the owned slots,
/// `MALLOCX_TCACHE_NONE`), which empties the slab when both pops were on
/// the same cid, purges arena 0, and pops again so the refill's zero pick
/// from the `Purged` slab reaches a cache's zero prefix while the other
/// thread pops (the zero slot, on either cid), writes and pushes (the
/// clip).
pub fn percpu_purge() -> Scenario {
    Scenario {
        name: "per-CPU: purge and the zero prefix",
        threads: vec![
            (0, vec![Op::Pop, Op::Pop, Op::Release, Op::Purge, Op::Pop]),
            (0, vec![Op::Pop, Op::Write(0), Op::Push(0)]),
        ],
        lg_group: 0,
        group_handoff: false,
        percpu: true,
        refill: REFILL,
    }
}

/// Two refills racing on one cid (challoc PR #4): with batches of three,
/// a refill pushes a zero batch of two onto an empty cache and a pop
/// leaves `zeroed` above `len`; the other thread's refill, whose empty
/// pop came first, claims a slot another refill dirtied and freed, and
/// its push-back lands on that non-empty cache (the same cid, or
/// migrated onto it). The push-back must clip the prefix at the old
/// `len`, or the dirty entry sits inside it and a pop reports it zero.
pub fn percpu_refill_race() -> Scenario {
    Scenario {
        name: "per-CPU: two refills racing on one cid",
        threads: vec![
            (0, vec![Op::Pop, Op::Pop, Op::Release, Op::Pop]),
            (0, vec![Op::Pop, Op::Write(0), Op::Release]),
        ],
        lg_group: 0,
        group_handoff: false,
        percpu: true,
        refill: 3,
    }
}
