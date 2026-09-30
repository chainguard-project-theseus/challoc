//! The modelled state (module docs in `mod.rs`): the slab, the shared
//! class and arena state, the operations, and the per-thread machine with
//! its locals; plus the bit helpers. Everything is `pub(super)`: the steps
//! in `steps.rs` read and write these fields directly, as the real code
//! does its own.

use super::*;

/// `slab::State`, as the model sees it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum St {
    Fresh,
    Active,
    Empty,
    Purged,
}

impl St {
    /// `State::is_zero`.
    pub(super) fn is_zero(self) -> bool {
        matches!(self, St::Fresh | St::Purged)
    }
}

/// One slab: the descriptor fields the protocol reads and writes, its
/// bitmap word and its memory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct Slab {
    pub(super) state: St,
    pub(super) live: u8,
    pub(super) bits: u8,
    /// Bit `k` set: slot `k` holds non-zero data.
    pub(super) mem: u8,
    /// `SlabDesc::arena`.
    pub(super) arena: u8,
    /// `commit_slab` ran (the slab is below its arena's cursor).
    pub(super) committed: bool,
}

/// The shared state: what every thread's steps read and write.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct Shared {
    /// The class lock of each arena: the thread holding it, or none.
    pub(super) lock: [Option<u8>; NARENAS],
    pub(super) slabs: [Slab; NSLABS],
    /// Slab bitmasks: `Inner::partial`, `empty`, `purged` per arena.
    pub(super) partial: [u8; NARENAS],
    pub(super) empty: [u8; NARENAS],
    pub(super) purged: [u8; NARENAS],
    /// `SlabTable::owner`: the arena of each group (`u16` per group in
    /// the real table; `NARENAS` here marks a group never handed out,
    /// which the real table encodes as arena 0 with a `Fresh` cross-check).
    pub(super) owner: [u8; NSLABS],
    /// `Inner::group_next`, `group_end` per arena.
    pub(super) group_next: [u8; NARENAS],
    pub(super) group_end: [u8; NARENAS],
    /// `SlabTable::inner.next_group`.
    pub(super) next_group: u8,
}

/// A program step of one thread.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// `ClassState::claim` of `n` slots.
    Refill(u8),
    /// Write to the `i`th slot the thread holds (the program using it).
    Write(u8),
    /// `ClassState::release` of every held slot, grouped by slab in
    /// address order (`Heap::flush`'s bucket pass).
    Release,
    /// `ClassState::purge_empty(u64::MAX)` on the thread's arena.
    Purge,
}

/// A slot a thread holds: slab, slot, and whether the refill reported it
/// zero (cleared by a write).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(super) struct Held {
    pub(super) slab: u8,
    pub(super) slot: u8,
    pub(super) zero: bool,
}

/// `class::Pick` plus the bits `finish` has claimed so far.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(super) struct Pick {
    pub(super) slab: u8,
    pub(super) n: u8,
    pub(super) zero: bool,
    pub(super) claimed: u8,
}

/// One thread: its arena, its program counter and the locals of the
/// operation in flight.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct Thread {
    pub(super) arena: u8,
    /// Index into the program.
    pub(super) pc: u8,
    /// Step within the current operation.
    pub(super) sub: u8,
    pub(super) held: [Held; MAX_HELD],
    pub(super) nheld: u8,
    pub(super) picks: [Pick; MAX_PICKS],
    pub(super) np: u8,
    /// `finish`: the pick being claimed.
    pub(super) cur: u8,
    /// `claim_with`: slots obtained so far (`got`); a refill re-plans
    /// while `got < out.len()` until a plan returns no picks.
    pub(super) got: u8,
    /// `Release`: slabs still to release (bitmask), the slab in flight,
    /// its count (between the `fetch_and` and the `sub_live`), whether
    /// `sub_live` reached zero, and the slabs whose word was full.
    pub(super) rel_todo: u8,
    pub(super) rel_slab: u8,
    pub(super) rel_count: u8,
    pub(super) rel_zero: bool,
    pub(super) was_full: u8,
    /// The `fetch_and` of `rel_slab` is done and `sub_live` is not:
    /// `live` is over by `rel_count` (the accounting the invariant uses).
    pub(super) dec_pending: bool,
    /// `Release`: the arena the slab in flight is released through
    /// (`release_routed`: the owner word's arena), whose lock
    /// `on_live_zero` and `reopen` take.
    pub(super) rel_arena: u8,
}

/// The whole model state: the memoisation key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct Node {
    pub(super) shared: Shared,
    pub(super) threads: Vec<Thread>,
}

pub(super) fn popcount(x: u8) -> u8 {
    x.count_ones() as u8
}

pub(super) fn lowest_set(mask: u8) -> Option<u8> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros() as u8)
    }
}

/// `bitmap::lowest_n_bits(!cur & mask, n)`.
pub(super) fn lowest_n_clear(bits: u8, n: u8) -> u8 {
    let mut free = !bits & FULL;
    let mut out = 0;
    for _ in 0..n {
        let Some(b) = lowest_set(free) else { break };
        out |= 1 << b;
        free &= !(1 << b);
    }
    out
}

impl Shared {
    pub(super) fn group_of(&self, sc: &Scenario, slab: u8) -> usize {
        (slab >> sc.lg_group) as usize
    }

    pub(super) fn group_range(&self, sc: &Scenario, group: usize) -> (u8, u8) {
        let start = (group << sc.lg_group) as u8;
        let end = (start + (1 << sc.lg_group)).min(NSLABS as u8);
        (start, end)
    }

    /// `SlabTable::owner(slab)`.
    pub(super) fn owner(&self, sc: &Scenario, slab: u8) -> u8 {
        self.owner[self.group_of(sc, slab)]
    }
}

impl Thread {
    pub(super) fn new(arena: u8) -> Thread {
        Thread {
            arena,
            pc: 0,
            sub: 0,
            held: [Held::default(); MAX_HELD],
            nheld: 0,
            picks: [Pick::default(); MAX_PICKS],
            np: 0,
            cur: 0,
            got: 0,
            rel_todo: 0,
            rel_slab: 0,
            rel_count: 0,
            rel_zero: false,
            was_full: 0,
            dec_pending: false,
            rel_arena: arena,
        }
    }

    pub(super) fn hold(&mut self, h: Held) {
        assert!(
            (self.nheld as usize) < MAX_HELD,
            "model: a thread holds more than MAX_HELD slots"
        );
        self.held[self.nheld as usize] = h;
        self.nheld += 1;
    }

    /// Drop every held slot of `slab`, returning how many.
    pub(super) fn drop_slab(&mut self, slab: u8) -> u8 {
        let mut kept = [Held::default(); MAX_HELD];
        let mut nk = 0;
        let mut dropped = 0;
        for h in &self.held[..self.nheld as usize] {
            if h.slab == slab {
                dropped += 1;
            } else {
                kept[nk] = *h;
                nk += 1;
            }
        }
        self.held = kept;
        self.nheld = nk as u8;
        dropped
    }

    /// The end of an operation.
    pub(super) fn done(&mut self) {
        self.pc += 1;
        self.sub = 0;
        self.clear_locals();
    }

    /// The locals of a finished machine are dead: reset them so that
    /// states that differ only in dead locals are one state (the
    /// memoisation key is the whole state).
    pub(super) fn clear_locals(&mut self) {
        self.np = 0;
        self.got = 0;
        self.cur = 0;
        self.picks = [Pick::default(); MAX_PICKS];
        self.rel_todo = 0;
        self.rel_slab = 0;
        self.rel_count = 0;
        self.rel_zero = false;
        self.was_full = 0;
        self.dec_pending = false;
        self.rel_arena = 0;
    }
}
