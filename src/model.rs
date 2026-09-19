//! An exhaustive interleaving model of the slab protocol (design.md
//! section 17, the "model-checked protocol" row; plan Phase 2.5 task 13).
//! Test-only: `cargo test --features std model::`.
//!
//! What is modelled. One class, [`NSLABS`] slabs of [`NSLOTS`] slots each
//! (the bits of a slab fit one word, as every real slab's do), one or two
//! arenas, two or three threads, and the shared state the protocol of
//! `class.rs` runs on: per slab the state byte, the `live` count, the
//! slot bitmap word, the descriptor's arena field and the slot memory
//! (one bit per slot: zero or dirty); per arena the class lock and the
//! `partial`, `empty` and `purged` sets and the group cursor; the owner
//! word per group and the class-wide next fresh group. Every thread runs
//! a short program of operations, each split into the atomic steps of the
//! real code in the real order (the line of `class.rs` each step mirrors
//! is quoted at the step), and [`explore`] enumerates every interleaving
//! of those steps depth first, memoising on the whole state, and checks
//! the invariants of [`check`] at every state. A violation returns the
//! interleaving that reached it.
//!
//! Operations: `Refill(n)` is `ClassState::claim` (`plan` under the lock:
//! the slab selection, `take`, the zero flag and, with the fix of plan
//! Phase 1.5 task 7 (c), `claim_prefix`; then `finish` outside the lock:
//! the `fetch_or` bit loop of an `Active` pick, one atomic per word);
//! `Write(i)` dirties the `i`th slot the thread holds; `Release` gives
//! every held slot back (`release_owned`: `fetch_and` per slab, then
//! `dec_live`, whose `sub_live` reaching zero runs `on_live_zero` under
//! the lock, then `reopen` under the lock for the slabs whose word was
//! full); `Purge` is `purge_empty` (every `Empty` slab of the arena to
//! `Purged` and its memory to zero, under the lock). The group handoff
//! (`adopt_idle_group`/`transfer_group`) is part of `Refill`'s locked
//! step when the scenario turns `group_handoff` on, as in `next_slab`.
//!
//! Abstractions, stated: a lock hold that only reads and writes
//! lock-protected state is one atomic step (the sets, the cursors, the
//! state byte, which is written under the lock only); the lock-free
//! atomics (`fetch_or`, `fetch_and`, `sub_live`, the memory write) are
//! one step each and interleave with everything, including another
//! thread's lock hold, as they do in the real code; `add_live` and the
//! zero pick's prefix `fetch_or`s run inside `take`'s lock hold and are
//! folded into that step (a concurrent `sub_live` between the `live()`
//! read and the `fetch_add` changes nothing the code relies on, and the
//! prefix bits of a `Fresh`/`Purged` slab have no contender). Guard slabs,
//! page groups larger than a slab (`spp == 1`), `slot_randomize`, the
//! thread cache above `claim`/`release` and the decay curve are not
//! modelled: the model is the protocol of the sets, the bits, `live` and
//! the state byte, which is what the calloc race and the ownership
//! check were about.
//!
//! Two knobs replay the protocol before its fixes ([`Variant`]):
//! `prefix_claim: false` is the pre-7 (c) order (the zero flag decided in
//! `plan`, the bits claimed in `finish`), and the model finds the calloc
//! race (a slot reported zero whose memory another thread dirtied in the
//! window) and prints the interleaving; `owner_check: false` drops the
//! owner test of `on_live_zero` and `reopen` (plan Phase 1.5 task 15),
//! and with `group_handoff` on the model finds a slab of one arena in
//! another arena's sets. With both on, every scenario passes.
//!
//! A change to the protocol in `class.rs` is a change to a step here.
//!
//! The per-CPU mode (design section 15, plan Phase 2.5 task 8; the
//! scenarios `percpu_*`). With [`Scenario::percpu`] the caches are owned
//! by a concurrency id, not a thread: [`NCACHES`] caches, cache `c` bound
//! to arena `c`, each a small array plus `len` and the `zeroed` prefix of
//! design 6.3. `Pop` and `Push(i)` are rseq critical sections as the
//! kernel defines them: the thread reads `mm_cid` and picks the cache,
//! reads `len` and the top entry (speculative locals), then one commit
//! store. The scheduler is modelled by what the caches can observe of
//! it: the `mm_cid` a thread reads at a section's entry is the
//! exploration's choice (any cid: the kernel may have moved the thread
//! anywhere since its last section), and since running threads of one
//! process never share an `mm_cid`, a thread that still has an
//! outstanding read of that cid was descheduled in between: under
//! [`Section::Rseq`] the kernel sends it to the abort handler (the
//! section's start: the locals discarded, `mm_cid` re-read, possibly a
//! new cid), and a thread inside a section may also be preempted
//! spontaneously (a signal, a migration; the one explicit scheduler step)
//! with the same abort. So with more threads than cids the caches are
//! shared over time, a thread that popped on cid 0 may push on cid 1,
//! and a commit under `Rseq` is always by the thread that holds the cid
//! that picked the cache. The kernel-owned fields are read-only in the
//! model: the threads read `mm_cid`, nothing in the protocol writes it.
//! [`Section::None`] is the buggy variant that does the same loads and
//! stores with no section (a thread descheduled between its `mm_cid`
//! read and its store goes on with the stale cid and stale locals), and
//! [`Section::CidOutside`] reads `mm_cid` before the section (the abort
//! handler restarts with the stale index): both hand a slot to two
//! threads or lose a push, and the model prints the interleaving. An empty cache
//! leaves the section and refills through the existing `Refill` steps
//! (the class lock of the cid's arena), then pushes the batch back
//! through a section on whatever cid the thread is on then; a full cache
//! leaves the section, pops half through a section into the thread's
//! staging and releases it through the existing `Release` steps, routed
//! slab by slab to the owner arena (`Heap::release_routed`), then retries.
//! The invariants gain: no slot in two caches or in a cache and owned by
//! a thread, every set bit has exactly one holder (a cache, a thread's
//! owned slot or its staging: a lost push is a bit with no holder), the
//! `zeroed` prefix reads zero, and (`Variant::strict_cid`) a cache is
//! only ever mutated by a section whose cid matched at commit.

use std::cell::Cell;
use std::collections::HashSet;
use std::string::String;
use std::time::Instant;
use std::vec::Vec;
use std::{eprintln, format, vec};

/// Slabs in the model. Two groups of one slab (the small classes' real
/// geometry, `slabs_per_line == 1`) when [`Scenario::lg_group`] is 0, one
/// group of two when it is 1.
pub const NSLABS: usize = 2;
/// Slots per slab; the slab's bits are the low `NSLOTS` bits of one word.
pub const NSLOTS: u8 = 4;
const FULL: u8 = (1 << NSLOTS) - 1;
const NARENAS: usize = 2;
const MAX_THREADS: usize = 3;
const MAX_HELD: usize = 4;
const MAX_PICKS: usize = 2;
const MAX_OPS: usize = 6;
/// States one exploration may visit before it gives up: a scenario past
/// this is too large for the 30 s budget of the host suite and must be
/// cut, not waited for.
const MAX_STATES: usize = 5_000_000;
/// Per-cid caches in the per-CPU mode; cache `c` is bound to arena `c`
/// (design 15: arenas per CPU when the mode is on), so `NCACHES ==
/// NARENAS`.
pub const NCACHES: usize = NARENAS;
/// Entries per cache. Small: the refill's batch is the real code's
/// `cap / 2`, here [`REFILL`] slots so that a refill has one slot to push
/// back into the cache and one to hand out.
pub const CACHE_CAP: u8 = 2;
/// Slots a `Pop` claims when its cache is empty.
const REFILL: u8 = 2;
/// Slots the flush of a full cache pops out (`cap / 2`).
const FLUSH_HALF: u8 = CACHE_CAP / 2;
/// Slots a thread's program owns at once in the per-CPU mode.
const MAX_OWNED: usize = 2;
/// Sub-step bases of the composite per-CPU operations: `Pop` is the
/// section (0..3), then `Refill`'s steps (10..), then the batch push
/// section (20..23), then `Release` of the overflow (30..); `Push` is the
/// section (0..3), then the flush section (10..13), then `Release` of
/// the flushed slots (20..) and back to 0.
const POP_REFILL: u8 = 10;
const POP_PUSHBACK: u8 = 20;
const POP_RELEASE: u8 = 30;
const PUSH_FLUSH: u8 = 10;
const PUSH_RELEASE: u8 = 20;

/// `slab::State`, as the model sees it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum St {
    Fresh,
    Active,
    Empty,
    Purged,
}

impl St {
    /// `State::is_zero`.
    fn is_zero(self) -> bool {
        matches!(self, St::Fresh | St::Purged)
    }
}

/// One slab: the descriptor fields the protocol reads and writes, its
/// bitmap word and its memory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Slab {
    state: St,
    live: u8,
    bits: u8,
    /// Bit `k` set: slot `k` holds non-zero data.
    mem: u8,
    /// `SlabDesc::arena`.
    arena: u8,
    /// `commit_slab` ran (the slab is below its arena's cursor).
    committed: bool,
}

/// One per-cid cache (`CacheHeader`, design 6.2): the stack, `len`, and
/// the zero prefix `slots[..zeroed]`. Entries above `len` are kept
/// blank so equal caches hash equal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
struct Cache {
    slots: [Held; CACHE_CAP as usize],
    len: u8,
    zeroed: u8,
}

/// The shared state: what every thread's steps read and write.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Shared {
    /// The class lock of each arena: the thread holding it, or none.
    lock: [Option<u8>; NARENAS],
    slabs: [Slab; NSLABS],
    /// The per-cid caches (per-CPU mode; blank otherwise).
    caches: [Cache; NCACHES],
    /// A section committed to a cache after its thread had lost the cid
    /// that picked it (another thread's section ran on that cid between
    /// the `mm_cid` read and the commit): (thread, cache). The kernel's
    /// abort makes this impossible under `Section::Rseq`; the buggy
    /// variants set it.
    stale_commit: Option<(u8, u8)>,
    /// Slab bitmasks: `Inner::partial`, `empty`, `purged` per arena.
    partial: [u8; NARENAS],
    empty: [u8; NARENAS],
    purged: [u8; NARENAS],
    /// `SlabTable::owner`: the arena of each group (`u16` per group in
    /// the real table; `NARENAS` here marks a group never handed out,
    /// which the real table encodes as arena 0 with a `Fresh` cross-check).
    owner: [u8; NSLABS],
    /// `Inner::group_next`, `group_end` per arena.
    group_next: [u8; NARENAS],
    group_end: [u8; NARENAS],
    /// `SlabTable::inner.next_group`.
    next_group: u8,
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
    /// `ClassState::purge_empty(u64::MAX)` on the thread's arena (the
    /// cid's arena in the per-CPU mode).
    Purge,
    /// Per-CPU mode: `malloc` through the cid's cache (design 6.3 step 3
    /// and 4 inside an rseq section), the refill when it is empty. The
    /// slot goes to the thread's owned slots.
    Pop,
    /// Per-CPU mode: `free` of the `i`th owned slot through the cid's
    /// cache (design 6.3 step 4 inside an rseq section), the flush of half
    /// the cache when it is full.
    Push(u8),
    /// Never in a program: the scheduler's step in a trace (a thread taken
    /// off its cid, or put on one).
    Sched,
}

/// A slot a thread holds: slab, slot, and whether the refill (or the
/// cache's zero prefix) reported it zero (cleared by a write).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
struct Held {
    slab: u8,
    slot: u8,
    zero: bool,
}

/// `class::Pick` plus the bits `finish` has claimed so far.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
struct Pick {
    slab: u8,
    n: u8,
    zero: bool,
    claimed: u8,
}

/// One thread: its arena, its program counter and the locals of the
/// operation in flight.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Thread {
    arena: u8,
    /// Index into the program.
    pc: u8,
    /// Step within the current operation.
    sub: u8,
    held: [Held; MAX_HELD],
    nheld: u8,
    picks: [Pick; MAX_PICKS],
    np: u8,
    /// `finish`: the pick being claimed.
    cur: u8,
    /// `claim_with`: slots obtained so far (`got`); a refill re-plans
    /// while `got < out.len()` until a plan returns no picks.
    got: u8,
    /// `Release`: slabs still to release (bitmask), the slab in flight,
    /// its count (between the `fetch_and` and the `sub_live`), whether
    /// `sub_live` reached zero, and the slabs whose word was full.
    rel_todo: u8,
    rel_slab: u8,
    rel_count: u8,
    rel_zero: bool,
    was_full: u8,
    /// The `fetch_and` of `rel_slab` is done and `sub_live` is not:
    /// `live` is over by `rel_count` (the accounting the invariant uses).
    dec_pending: bool,
    /// `Release`: the arena the slab in flight is released through
    /// (`release_routed`: the owner word's arena), whose lock
    /// `on_live_zero` and `reopen` take.
    rel_arena: u8,
    /// Per-CPU mode: the thread lost the cid it read at the section's
    /// start (another thread's section ran on it, so this thread was
    /// descheduled in between) and was not aborted: its commit is stale.
    stale: bool,
    /// Per-CPU mode: the slots the program owns (a `Pop`'s result until
    /// its `Push`); `held` is then the staging of a refill's batch or a
    /// flush's slots on their way to `Release`.
    owned: [Held; MAX_OWNED],
    nowned: u8,
    /// The section in flight: the cid read at its start (the cache
    /// index), whether the thread is inside it (a reschedule aborts it),
    /// and the speculative reads (`len` and the top entry).
    sec_cid: u8,
    in_section: bool,
    spec_len: u8,
    spec_ent: Held,
}

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
}

/// The whole model state: the memoisation key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Node {
    shared: Shared,
    threads: Vec<Thread>,
}

/// A step taken: thread, operation, sub-step, and what it did.
#[derive(Clone, Debug)]
pub struct Trace(pub Vec<(u8, Op, u8, String)>);

/// What [`explore`] found.
#[derive(Debug)]
pub struct Outcome {
    pub states: usize,
    pub violation: Option<(String, Trace)>,
    /// Per-CPU mode: commits of a `Push` of a slot of one arena into
    /// another cid's cache (the thread migrated between its pop and its
    /// push; the flush routes the slot back), and sections the scheduler
    /// aborted; counted over the exploration so a test can show a
    /// scenario exercised them.
    pub cross_pushes: usize,
    pub aborts: usize,
}

struct Model<'a> {
    sc: &'a Scenario,
    v: Variant,
    seen: HashSet<Node>,
    trace: Vec<(u8, Op, u8, String)>,
    cross_pushes: Cell<usize>,
    aborts: Cell<usize>,
}

fn popcount(x: u8) -> u8 {
    x.count_ones() as u8
}

fn lowest_set(mask: u8) -> Option<u8> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros() as u8)
    }
}

/// `bitmap::lowest_n_bits(!cur & mask, n)`.
fn lowest_n_clear(bits: u8, n: u8) -> u8 {
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
    fn group_of(&self, sc: &Scenario, slab: u8) -> usize {
        (slab >> sc.lg_group) as usize
    }

    fn group_range(&self, sc: &Scenario, group: usize) -> (u8, u8) {
        let start = (group << sc.lg_group) as u8;
        let end = (start + (1 << sc.lg_group)).min(NSLABS as u8);
        (start, end)
    }

    /// `SlabTable::owner(slab)`.
    fn owner(&self, sc: &Scenario, slab: u8) -> u8 {
        self.owner[self.group_of(sc, slab)]
    }
}

impl Thread {
    fn new(arena: u8) -> Thread {
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
            stale: false,
            owned: [Held::default(); MAX_OWNED],
            nowned: 0,
            sec_cid: 0,
            in_section: false,
            spec_len: 0,
            spec_ent: Held::default(),
        }
    }

    /// Per-CPU mode: the program takes ownership of a popped slot.
    fn own(&mut self, h: Held) {
        assert!(
            (self.nowned as usize) < MAX_OWNED,
            "model: a program owns more than MAX_OWNED slots"
        );
        self.owned[self.nowned as usize] = h;
        self.nowned += 1;
    }

    /// Per-CPU mode: the program gives up its `i`th owned slot.
    fn take_owned(&mut self, i: u8) -> Held {
        let h = self.owned[i as usize];
        for k in (i as usize + 1)..self.nowned as usize {
            self.owned[k - 1] = self.owned[k];
        }
        self.nowned -= 1;
        self.owned[self.nowned as usize] = Held::default();
        h
    }

    /// Drop every staged slot (`held`), returning them in order.
    fn drain_held(&mut self) -> Vec<Held> {
        let out = self.held[..self.nheld as usize].to_vec();
        self.held = [Held::default(); MAX_HELD];
        self.nheld = 0;
        out
    }

    fn hold(&mut self, h: Held) {
        assert!(
            (self.nheld as usize) < MAX_HELD,
            "model: a thread holds more than MAX_HELD slots"
        );
        self.held[self.nheld as usize] = h;
        self.nheld += 1;
    }

    /// Drop every held slot of `slab`, returning how many.
    fn drop_slab(&mut self, slab: u8) -> u8 {
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
    fn done(&mut self) {
        self.pc += 1;
        self.sub = 0;
        self.clear_locals();
    }

    /// The locals of a finished machine are dead: reset them so that
    /// states that differ only in dead locals are one state (the
    /// memoisation key is the whole state).
    fn clear_locals(&mut self) {
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
        self.clear_section();
        self.sec_cid = 0;
        self.stale = false;
    }

    /// The speculative reads of a section are dead once it committed or
    /// aborted.
    fn clear_section(&mut self) {
        self.in_section = false;
        self.spec_len = 0;
        self.spec_ent = Held::default();
    }
}

/// The micro-machine an `(operation, sub-step)` pair is in. The four
/// operations of the thread mode are their own machines; the per-CPU
/// `Pop` and `Push` compose their sections with `Refill` and `Release`
/// through the sub-step bases (`POP_REFILL` and the others).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Micro {
    Refill(u8, u8),
    Write(u8),
    Release(u8),
    Purge(u8),
    /// The pop section: entry and `mm_cid`, the speculative reads, the
    /// commit.
    Pop(u8),
    /// The section that pushes a refill's batch back into the cid's cache.
    PushBack(u8),
    /// The push section of owned slot `i`.
    Push(u8, u8),
    /// The section that pops half of a full cache into the staging.
    Flush(u8),
}

fn micro(op: Op, sub: u8) -> Micro {
    match op {
        Op::Refill(want) => Micro::Refill(want, sub),
        Op::Write(i) => Micro::Write(i),
        Op::Release => Micro::Release(sub),
        Op::Purge => Micro::Purge(sub),
        Op::Pop => {
            if sub < POP_REFILL {
                Micro::Pop(sub)
            } else if sub < POP_PUSHBACK {
                Micro::Refill(REFILL, sub - POP_REFILL)
            } else if sub < POP_RELEASE {
                Micro::PushBack(sub - POP_PUSHBACK)
            } else {
                Micro::Release(sub - POP_RELEASE)
            }
        }
        Op::Push(i) => {
            if sub < PUSH_FLUSH {
                Micro::Push(i, sub)
            } else if sub < PUSH_RELEASE {
                Micro::Flush(sub - PUSH_FLUSH)
            } else {
                Micro::Release(sub - PUSH_RELEASE)
            }
        }
        Op::Sched => unreachable!("model: the scheduler is not a program step"),
    }
}

/// The sub-step base (0, 10, 20 or 30) of the micro-machine `sub` is in.
fn micro_base(sub: u8) -> u8 {
    sub - sub % 10
}

impl Cache {
    /// The commit store of `len`. Entries above it are blanked (a stale
    /// `len` stored over a longer cache loses the entries above: the
    /// lost push) and the zero prefix never exceeds `len`.
    fn set_len(&mut self, len: u8) {
        self.len = len;
        for k in len as usize..CACHE_CAP as usize {
            self.slots[k] = Held::default();
        }
        if self.zeroed > len {
            self.zeroed = len;
        }
    }

    /// A push at index `at` (the speculative `len`): the entry, then the
    /// zero prefix rule of design 6.3 (a zero entry extends a prefix that
    /// reaches it; a dirty entry below the prefix clips it: `heap.rs`,
    /// `if len < zeroed { zeroed = len }`), then `len = at + 1`.
    fn push_at(&mut self, at: u8, h: Held) {
        self.slots[at as usize] = Held { zero: false, ..h };
        if h.zero && self.zeroed == at {
            self.zeroed = at + 1;
        } else if !h.zero && at < self.zeroed {
            self.zeroed = at;
        }
        self.set_len(at + 1);
    }
}

// The step a thread would take next, whether it is enabled, and its
// effect. A step returns `Err` on an abort of the real code (a heap
// inconsistency the protocol detects itself).
impl Model<'_> {
    fn program(&self, t: usize) -> &[Op] {
        &self.sc.threads[t].1
    }

    fn finished(&self, th: &Thread, t: usize) -> bool {
        th.pc as usize >= self.program(t).len()
    }

    /// Whether thread `t`'s next step can run now.
    fn enabled(&self, n: &Node, t: usize) -> bool {
        let th = &n.threads[t];
        if self.finished(th, t) {
            return false;
        }
        let sh = &n.shared;
        let a = th.arena as usize;
        match micro(self.program(t)[th.pc as usize], th.sub) {
            // Lock acquisitions: `self.inner.lock()` blocks while held.
            Micro::Refill(_, 0) => sh.lock[a].is_none(),
            Micro::Purge(0) => sh.lock[self.purge_arena(th, t)].is_none(),
            Micro::Release(3) | Micro::Release(5) => sh.lock[th.rel_arena as usize].is_none(),
            // `finish`'s bit loop: `claim_from` finds nothing while every
            // word is full and rescans (`MAX_EMPTY_PASSES`); the model
            // waits for a clear bit instead of spinning.
            Micro::Refill(_, 3) => {
                let p = th.picks[th.cur as usize];
                p.zero && self.v.prefix_claim || sh.slabs[p.slab as usize].bits != FULL
            }
            _ => true,
        }
    }

    /// The arena a `Purge` runs on: the thread's; in the per-CPU mode,
    /// where a thread's arena follows its cid from pop to pop, the one
    /// the scenario names for the thread (decay is an arena's work, not
    /// a CPU's).
    fn purge_arena(&self, th: &Thread, t: usize) -> usize {
        if self.sc.percpu {
            self.sc.threads[t].0 as usize
        } else {
            th.arena as usize
        }
    }

    /// The end of a micro-machine: the operation ends, or the composite
    /// operation moves on (`Pop`: the refill done, push the batch back;
    /// the overflow released, done; `Push`: the flush released, retry).
    fn micro_done(&self, th: &mut Thread, op: Op) {
        match op {
            Op::Pop if th.sub >= POP_REFILL && th.sub < POP_PUSHBACK => {
                th.sub = POP_PUSHBACK;
                th.clear_locals();
            }
            Op::Push(_) if th.sub >= PUSH_RELEASE => {
                th.sub = 0;
                th.clear_locals();
            }
            _ => th.done(),
        }
    }

    /// The abort handler's target: the section's first step. Under
    /// `Section::Rseq` that re-reads `mm_cid`; under `CidOutside` the
    /// section starts after the read, so the stale index stays.
    fn section_start(&self, sub: u8) -> u8 {
        micro_base(sub) + u8::from(self.v.section == Section::CidOutside)
    }

    /// The cid a thread has read and not yet committed on: it is between
    /// step 0 (the `mm_cid` read) and the commit of a section. The steps
    /// outside sections (the refill, the release, the program's writes)
    /// do not care which CPU runs them.
    fn outstanding(&self, th: &Thread, t: usize) -> Option<u8> {
        if self.finished(th, t) {
            return None;
        }
        match micro(self.program(t)[th.pc as usize], th.sub) {
            Micro::Pop(s) | Micro::PushBack(s) | Micro::Push(_, s) | Micro::Flush(s) if s >= 1 => {
                Some(th.sec_cid)
            }
            _ => None,
        }
    }

    /// Step 0 of every section, thread `t` on cid `c` (the exploration's
    /// choice: any cid the kernel could have put the thread on): `mm_cid`
    /// read and the cache picked. Under `Rseq` the read is the first
    /// instruction of the section. Running threads of one process never
    /// share an `mm_cid`, so another thread with an outstanding read of
    /// `c` was descheduled in between: inside its section the kernel
    /// sends it to the abort handler at its next instruction (under
    /// `CidOutside` the handler keeps the index read before the section,
    /// so it is stale); outside one (`None`, or `CidOutside` before the
    /// section starts) it goes on with a stale cid.
    fn enter_section(&self, ths: &mut [Thread], t: usize, c: u8, what: &str) -> String {
        let mut taken = String::new();
        for x in 0..ths.len() {
            if x == t || self.outstanding(&ths[x], x) != Some(c) {
                continue;
            }
            let xt = &mut ths[x];
            if xt.in_section {
                let at = self.section_start(xt.sub);
                self.abort(xt, at);
                xt.stale = self.v.section == Section::CidOutside;
                self.aborts.set(self.aborts.get() + 1);
                taken += &format!("; T{x} was descheduled inside its section on cid {c}: rseq abort, retry at [{at}]");
            } else {
                xt.stale = true;
                taken += &format!(
                    "; T{x} was descheduled with cid {c} read, no section: it goes on stale"
                );
            }
        }
        let th = &mut ths[t];
        th.sec_cid = c;
        th.stale = false;
        th.in_section = self.v.section == Section::Rseq;
        th.sub += 1;
        format!(
            "{what}: mm_cid = {c}, cache {c}{}{taken}",
            match self.v.section {
                Section::Rseq => " (rseq section entered)",
                Section::CidOutside => " (read before the section)",
                Section::None => " (no section)",
            }
        )
    }

    /// The kernel's abort of `th`'s section: back to `at` (the section's
    /// start) with the speculative reads dropped; under `Rseq` the cid is
    /// re-read there, so it is dropped too (the state is the one before
    /// the entry), under `CidOutside` the stale index stays.
    fn abort(&self, th: &mut Thread, at: u8) {
        th.clear_section();
        th.sub = at;
        if self.v.section == Section::Rseq {
            th.sec_cid = 0;
        }
        self.aborts.set(self.aborts.get() + 1);
    }

    /// Step 1 of a section: the speculative reads begin; under
    /// `CidOutside` this is where the section starts.
    fn speculate(&self, th: &mut Thread) {
        if self.v.section == Section::CidOutside {
            th.in_section = true;
        }
    }

    /// The commit store ends the section. A commit by a thread that lost
    /// its cid on the way is what the kernel's abort rules out and
    /// `strict_cid` checks; the model records the first.
    fn commit(sh: &mut Shared, th: &mut Thread, tid: u8) -> &'static str {
        th.in_section = false;
        if th.stale {
            th.stale = false;
            if sh.stale_commit.is_none() {
                sh.stale_commit = Some((tid, th.sec_cid));
            }
            "; STALE: the thread lost this cid before the commit"
        } else {
            ""
        }
    }

    /// How many ways thread `t`'s next step can go: [`NCACHES`] at a
    /// section entry in the per-CPU mode (the cid the kernel put the
    /// thread on), one otherwise.
    fn choices(&self, n: &Node, t: usize) -> u8 {
        let th = &n.threads[t];
        if !self.sc.percpu {
            return 1;
        }
        match micro(self.program(t)[th.pc as usize], th.sub) {
            Micro::Pop(0) | Micro::PushBack(0) | Micro::Push(_, 0) | Micro::Flush(0) => {
                NCACHES as u8
            }
            _ => 1,
        }
    }

    /// Run thread `t`'s next step, the `choice`th way. `Ok(description)`.
    fn step(&self, n: &mut Node, t: usize, choice: u8) -> Result<String, String> {
        let op = self.program(t)[n.threads[t].pc as usize];
        let sub = n.threads[t].sub;
        let (sh, ths) = (&mut n.shared, &mut n.threads);
        let th = &mut ths[t];
        let tid = t as u8;
        match micro(op, sub) {
            Micro::Refill(want, sub) => self.step_refill(sh, th, tid, op, want, sub),
            Micro::Write(i) => {
                // The program writes a slot it was given; a refill that
                // came up short (the class exhausted by the other
                // threads) gave fewer, and then there is nothing to write.
                th.done();
                let percpu = self.sc.percpu;
                let n = if percpu { th.nowned } else { th.nheld };
                if i >= n {
                    return Ok(format!(
                        "write: the program has {n} slots, none at index {i}"
                    ));
                }
                let list = if percpu {
                    &mut th.owned[..]
                } else {
                    &mut th.held[..]
                };
                let h = list[i as usize];
                sh.slabs[h.slab as usize].mem |= 1 << h.slot;
                list[i as usize].zero = false;
                Ok(format!("write slot {} of slab {}", h.slot, h.slab))
            }
            Micro::Release(sub) => self.step_release(sh, th, tid, op, sub),
            Micro::Purge(0) => {
                // purge_empty: `let mut g = self.inner.lock();`
                let a = self.purge_arena(th, t);
                th.arena = a as u8;
                sh.lock[a] = Some(tid);
                th.sub = 1;
                Ok(format!("purge: lock arena {a}"))
            }
            Micro::Purge(_) => {
                // purge_empty under the lock: for every slab of the
                // empty set, `assert!(desc.state() == State::Empty &&
                // desc.live() == 0)`, then `purge_group`:
                // `d.set_state(State::Purged); d.set_dirty_at(0);
                // g.empty.map.clear(s); g.purged.map.set(s);` and the
                // discard (`batch.flush()` before the lock is dropped),
                // after which the memory reads zero.
                let a = th.arena as usize;
                let mut desc = String::from("purge:");
                let mut set = sh.empty[a];
                while let Some(slab) = lowest_set(set) {
                    set &= !(1 << slab);
                    let s = &mut sh.slabs[slab as usize];
                    if s.state != St::Empty || s.live != 0 {
                        return Err(format!(
                            "purge_empty: slab {slab} in the empty set is {:?} with live {}",
                            s.state, s.live
                        ));
                    }
                    s.state = St::Purged;
                    s.mem = 0;
                    sh.empty[a] &= !(1 << slab);
                    sh.purged[a] |= 1 << slab;
                    desc += &format!(" slab {slab} -> Purged, memory zero;");
                }
                if sh.empty[a] == 0 && desc == "purge:" {
                    desc += " nothing Empty;";
                }
                sh.lock[a] = None;
                th.done();
                Ok(desc + " unlock")
            }
            Micro::Pop(0) => {
                // design 6.3 `malloc` steps 2 and 3 in the per-CPU mode:
                // the section opens, `mm_cid` is read, the cache picked.
                Ok(self.enter_section(ths, t, choice, "pop"))
            }
            Micro::Pop(1) => {
                self.speculate(th);
                let c = &sh.caches[th.sec_cid as usize];
                if c.len == 0 {
                    // `if len == 0`, slow path: leave the section and
                    // refill from the cid's arena (design 15: arenas per
                    // CPU) through `ClassState::claim`.
                    th.in_section = false;
                    th.arena = th.sec_cid;
                    th.sub = POP_REFILL;
                    return Ok(format!(
                        "pop: cache {} empty, leave the section, refill from arena {}",
                        th.sec_cid, th.arena
                    ));
                }
                // The speculative reads: `len` and `slots[len - 1]`, and
                // the zero flag from the prefix (`len < zeroed`).
                th.spec_len = c.len;
                let top = c.slots[(c.len - 1) as usize];
                th.spec_ent = Held {
                    zero: c.len - 1 < c.zeroed,
                    ..top
                };
                let d = format!(
                    "pop: cache {} len {}, top slab {} slot {}{} (speculative)",
                    th.sec_cid,
                    c.len,
                    top.slab,
                    top.slot,
                    if th.spec_ent.zero {
                        " in the zero prefix"
                    } else {
                        ""
                    }
                );
                self.after_reads(sh, th, t, d, Self::pop_commit)
            }
            Micro::Pop(_) => self.pop_commit(sh, th, t),
            Micro::PushBack(0) => {
                // The refill's batch: one slot is the pop's result, the
                // rest goes into the cache of the cid the thread is on
                // now, through a section.
                if th.nheld == 0 {
                    th.done();
                    return Ok(String::from(
                        "refill push-back: the class is out of slabs, the pop returns null",
                    ));
                }
                Ok(self.enter_section(ths, t, choice, "refill push-back"))
            }
            Micro::PushBack(1) => {
                self.speculate(th);
                th.spec_len = sh.caches[th.sec_cid as usize].len;
                let d = format!(
                    "refill push-back: cache {} len {} (speculative)",
                    th.sec_cid, th.spec_len
                );
                self.after_reads(sh, th, t, d, Self::pushback_commit)
            }
            Micro::PushBack(_) => self.pushback_commit(sh, th, t),
            Micro::Push(i, 0) => {
                // design 6.3 `free` step 4 in the per-CPU mode.
                if i >= th.nowned {
                    th.done();
                    return Ok(format!(
                        "push: the program owns {} slots, none at index {i}",
                        th.nowned
                    ));
                }
                Ok(self.enter_section(ths, t, choice, "push"))
            }
            Micro::Push(_, 1) => {
                self.speculate(th);
                let c = &sh.caches[th.sec_cid as usize];
                if c.len == CACHE_CAP {
                    // `if len == cap`, slow path (flush half): leave the
                    // section.
                    th.in_section = false;
                    th.sub = PUSH_FLUSH;
                    return Ok(format!(
                        "push: cache {} full, leave the section, flush",
                        th.sec_cid
                    ));
                }
                th.spec_len = c.len;
                let d = format!("push: cache {} len {} (speculative)", th.sec_cid, c.len);
                self.after_reads(sh, th, t, d, Self::push_commit)
            }
            Micro::Push(_, _) => self.push_commit(sh, th, t),
            Micro::Flush(0) => {
                // `Heap::flush` in the per-CPU mode: half the cache leaves
                // it through a section, then `release_routed`.
                Ok(self.enter_section(ths, t, choice, "flush"))
            }
            Micro::Flush(1) => {
                self.speculate(th);
                let c = &sh.caches[th.sec_cid as usize];
                if c.len == 0 {
                    // Migrated onto an empty cache since the push saw a
                    // full one: nothing to flush, the push retries.
                    th.in_section = false;
                    th.sub = 0;
                    return Ok(format!(
                        "flush: cache {} is empty now, retry the push",
                        th.sec_cid
                    ));
                }
                th.spec_len = c.len;
                let d = format!("flush: cache {} len {} (speculative)", th.sec_cid, c.len);
                self.after_reads(sh, th, t, d, Self::flush_commit)
            }
            Micro::Flush(_) => self.flush_commit(sh, th, t),
        }
    }

    /// The speculative reads are done. Under `Section::Rseq` the commit
    /// is the same step: the kernel lets no other thread run on this cid
    /// until the section ends (one that does aborts this one back to
    /// before its entry), so nothing can interleave between the reads and
    /// the store that the cache could observe; the section is atomic the
    /// way a lock hold is in the thread mode (module docs). The buggy
    /// variants keep the store a step of its own, which is where a stale
    /// read commits.
    fn after_reads(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        t: usize,
        d: String,
        commit: fn(&Self, &mut Shared, &mut Thread, usize) -> Result<String, String>,
    ) -> Result<String, String> {
        if self.v.section == Section::Rseq {
            let d2 = commit(self, sh, th, t)?;
            Ok(format!("{d}; {d2}"))
        } else {
            th.sub += 1;
            Ok(d)
        }
    }

    /// `p = slots[--len]`: the one commit store of a pop.
    fn pop_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
        let stale = Self::commit(sh, th, t as u8);
        let ent = th.spec_ent;
        let c = &mut sh.caches[th.sec_cid as usize];
        let before = c.len;
        c.set_len(th.spec_len - 1);
        let d = format!(
            "pop: commit len {before} -> {} on cache {}, slab {} slot {} owned{}{stale}",
            c.len,
            th.sec_cid,
            ent.slab,
            ent.slot,
            if ent.zero { " (reported zero)" } else { "" }
        );
        th.own(ent);
        th.done();
        Ok(d)
    }

    /// The refill push-back's commit: `slots[len..len + k] = batch; len +=
    /// k`. What the cache has no room for (the thread migrated onto a
    /// fuller cache) stays staged and is released.
    fn pushback_commit(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        t: usize,
    ) -> Result<String, String> {
        let stale = Self::commit(sh, th, t as u8);
        let batch = th.drain_held();
        th.own(batch[0]);
        let c = &mut sh.caches[th.sec_cid as usize];
        let before = c.len;
        let room = CACHE_CAP - th.spec_len;
        let mut at = th.spec_len;
        let mut pushed = 0u8;
        for h in &batch[1..] {
            if pushed < room {
                c.push_at(at, *h);
                at += 1;
                pushed += 1;
            } else {
                th.hold(*h);
            }
        }
        let mut desc = format!(
            "refill push-back: commit slab {} slot {} owned, {pushed} into cache {} (len {before} -> {}, zeroed {}){stale}",
            batch[0].slab, batch[0].slot, th.sec_cid, c.len, c.zeroed
        );
        if th.nheld != 0 {
            th.sub = POP_RELEASE;
            desc += &format!("; {} overflow to release", th.nheld);
        } else {
            th.done();
        }
        Ok(desc)
    }

    /// `slots[len++] = p`: the one commit store of a push. A pushed slot
    /// is dirty as far as the prefix knows.
    fn push_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
        let Op::Push(i) = self.program(t)[th.pc as usize] else {
            unreachable!("model: push_commit outside a Push")
        };
        let stale = Self::commit(sh, th, t as u8);
        let h = th.take_owned(i);
        let home = sh.owner(self.sc, h.slab);
        let cross = if home != th.sec_cid {
            // The thread migrated between the pop and the push: the
            // slot's arena is not this cache's, and the flush will route
            // it back (`release_routed`).
            self.cross_pushes.set(self.cross_pushes.get() + 1);
            format!(" (a slot of arena {home})")
        } else {
            String::new()
        };
        let c = &mut sh.caches[th.sec_cid as usize];
        let before = c.len;
        c.push_at(th.spec_len, Held { zero: false, ..h });
        let d = format!(
            "push: commit slots[{}] = slab {} slot {} on cache {} (len {before} -> {}){cross}{stale}",
            th.spec_len, h.slab, h.slot, th.sec_cid, c.len
        );
        th.done();
        Ok(d)
    }

    /// The flush's commit: `len -= k`, the `k` entries staged; the zero
    /// prefix is dropped by a flush (`heap.rs`, `zeroed = 0`).
    fn flush_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
        let stale = Self::commit(sh, th, t as u8);
        let k = th.spec_len.min(FLUSH_HALF);
        let new_len = th.spec_len - k;
        let c = &mut sh.caches[th.sec_cid as usize];
        for j in new_len..th.spec_len {
            th.hold(Held {
                zero: false,
                ..c.slots[j as usize]
            });
        }
        let before = c.len;
        c.set_len(new_len);
        c.zeroed = 0;
        th.sub = PUSH_RELEASE;
        Ok(format!(
            "flush: commit {k} entries out of cache {} (len {before} -> {new_len}), zero prefix dropped, release{stale}",
            th.sec_cid
        ))
    }

    /// `ClassState::claim`: the steps of `Refill`, at sub-step base
    /// `micro_base(th.sub)` (0 for `Op::Refill`, `POP_REFILL` inside a
    /// `Pop`).
    fn step_refill(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        op: Op,
        want: u8,
        sub: u8,
    ) -> Result<String, String> {
        let base = micro_base(th.sub);
        let a = th.arena as usize;
        match sub {
            0 => {
                // plan_locked: `let mut g = self.inner.lock();`
                sh.lock[a] = Some(tid);
                th.sub = base + 1;
                Ok(format!("lock arena {a}"))
            }
            1 => {
                // plan_locked under the lock: the partial walk, then
                // `next_slab` (empty, purged, the group cursor, the
                // handoff, a fresh group), `take` per slab.
                // `self.plan(meta, out.len() - got, &mut picks)`
                let desc = self.plan_locked(sh, th, want - th.got)?;
                th.sub = base + 2;
                Ok(desc)
            }
            2 => {
                // `plan` returns: the lock hold ends.
                sh.lock[a] = None;
                th.cur = 0;
                if th.np == 0 {
                    self.micro_done(th, op);
                    return Ok(String::from("unlock; nothing to commit, claim returns 0"));
                }
                th.sub = base + 3;
                Ok(String::from("unlock"))
            }
            _ => {
                // finish: one pick at a time.
                let p = th.picks[th.cur as usize];
                let s = &mut sh.slabs[p.slab as usize];
                let desc;
                if p.zero && self.v.prefix_claim {
                    // `if pick.zero { for k in 0..want { out[got + k] =
                    // self.slot_ptr(slab, k); } ... continue; }`
                    for k in 0..p.n {
                        th.hold(Held {
                            slab: p.slab,
                            slot: k,
                            zero: true,
                        });
                    }
                    th.picks[th.cur as usize].claimed = p.n;
                    desc = format!(
                        "finish: zero pick, slots 0..{} of slab {} handed out",
                        p.n, p.slab
                    );
                } else {
                    // `let Some((w, bits)) = bm.claim_from(word, want - k)`:
                    // `claim_word`: `want = lowest_n_bits(!cur & mask, n)`;
                    // `prev = w.fetch_or(want)`; `(want & !prev)`.
                    let wantbits = lowest_n_clear(s.bits, p.n - p.claimed);
                    assert!(wantbits != 0, "model: finish stepped with no clear bit");
                    let prev = s.bits;
                    s.bits |= wantbits;
                    let got = wantbits & !prev;
                    for k in 0..NSLOTS {
                        if got & (1 << k) != 0 {
                            // Pre-fix: the slot is handed out under the
                            // stale zero flag of `plan`.
                            th.hold(Held {
                                slab: p.slab,
                                slot: k,
                                zero: p.zero,
                            });
                        }
                    }
                    th.picks[th.cur as usize].claimed += popcount(got);
                    desc = format!(
                        "finish: fetch_or {got:#06b} on slab {} (bits {prev:#06b} -> {:#06b}){}",
                        p.slab,
                        s.bits,
                        if p.zero { ", pick flagged zero" } else { "" }
                    );
                }
                if th.picks[th.cur as usize].claimed == p.n {
                    th.got += p.n;
                    th.cur += 1;
                    if th.cur == th.np {
                        // claim_with: `while got < out.len() { let np =
                        // self.plan(..); if np == 0 { break; } got +=
                        // self.finish(..); }`: short, so plan again.
                        th.np = 0;
                        if th.got < want {
                            th.sub = base;
                            return Ok(desc + "; short, plan again");
                        }
                        self.micro_done(th, op);
                    }
                }
                Ok(desc)
            }
        }
    }

    /// `ClassState::release` through `Heap::release_routed`: the steps of
    /// `Release`, at sub-step base `micro_base(th.sub)` (0 for
    /// `Op::Release`, `POP_RELEASE` for a refill's overflow,
    /// `PUSH_RELEASE` for a flush). Each slab is released through the
    /// arena its owner word names; a run of one owner ends with that
    /// arena's `reopen`.
    fn step_release(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        op: Op,
        sub: u8,
    ) -> Result<String, String> {
        let base = micro_base(th.sub);
        match sub {
            0 => {
                // In the per-CPU mode a program's `Release` is the direct
                // free (`MALLOCX_TCACHE_NONE`): its owned slots are the
                // batch. Then release_owned: the batch grouped by slab,
                // lowest slab first (`Heap::flush`'s bucket pass).
                if self.sc.percpu && op == Op::Release {
                    for k in 0..th.nowned as usize {
                        th.hold(th.owned[k]);
                    }
                    th.owned = [Held::default(); MAX_OWNED];
                    th.nowned = 0;
                }
                let mut todo = 0u8;
                for h in &th.held[..th.nheld as usize] {
                    todo |= 1 << h.slab;
                }
                if todo == 0 {
                    self.micro_done(th, op);
                    return Ok(String::from("release: nothing held"));
                }
                th.rel_todo = todo;
                th.was_full = 0;
                th.sub = base + 1;
                Ok(format!("release: slabs {todo:#04b}"))
            }
            1 => {
                // release_routed: `self.owner(a).class(class).release_owned(..)`
                // with `a` the owner word's arena (`arena_of_slot`), then
                // release_word for the slab's one word:
                // `let (prev, was_full) = self.table().bits(slab).release(word, mask);`
                // `if prev & mask != mask { double_free(...) }`
                // preceded by `release_owned`'s `Fresh` check.
                let slab = lowest_set(th.rel_todo).unwrap();
                th.rel_todo &= !(1 << slab);
                let owner = sh.owner(self.sc, slab);
                if owner == NARENAS as u8 {
                    return Err(format!(
                        "SlabTable::owner: slab {slab} was never handed out (released by thread {tid})"
                    ));
                }
                th.rel_arena = owner;
                let mut mask = 0u8;
                for h in &th.held[..th.nheld as usize] {
                    if h.slab == slab {
                        mask |= 1 << h.slot;
                    }
                }
                let s = &mut sh.slabs[slab as usize];
                if s.state == St::Fresh {
                    return Err(format!("release_into_fresh_slab: slab {slab}"));
                }
                let prev = s.bits;
                if prev & mask != mask {
                    return Err(format!(
                        "double_free: slab {slab} mask {mask:#06b} bits {prev:#06b}"
                    ));
                }
                s.bits &= !mask;
                if prev == FULL {
                    th.was_full |= 1 << slab;
                }
                let count = th.drop_slab(slab);
                th.rel_slab = slab;
                th.rel_count = count;
                th.dec_pending = true;
                th.sub = base + 2;
                Ok(format!(
                    "release: fetch_and !{mask:#06b} on slab {slab} of arena {owner} (bits {prev:#06b} -> {:#06b}){}",
                    s.bits,
                    if prev == FULL { ", was full" } else { "" }
                ))
            }
            2 => {
                // dec_live: `if self.desc(slab).sub_live(n) == n { self.on_live_zero(slab); }`
                let s = &mut sh.slabs[th.rel_slab as usize];
                let prev = s.live;
                if prev < th.rel_count {
                    return Err(format!(
                        "sub_live underflow on slab {}: {prev} - {}",
                        th.rel_slab, th.rel_count
                    ));
                }
                s.live -= th.rel_count;
                th.dec_pending = false;
                th.rel_zero = prev == th.rel_count;
                th.sub = base + if th.rel_zero { 3 } else { 6 };
                Ok(format!(
                    "release: sub_live({}) on slab {} ({prev} -> {}){}",
                    th.rel_count,
                    th.rel_slab,
                    s.live,
                    if th.rel_zero { ", reached zero" } else { "" }
                ))
            }
            3 => {
                // on_live_zero: `let mut g = self.inner.lock();`
                let a = th.rel_arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = base + 4;
                Ok(format!("on_live_zero: lock arena {a}"))
            }
            4 => {
                // on_live_zero under the lock:
                // `if self.table().owner(slab) != self.arena { return; }`
                // `if desc.live() == 0 && desc.state() == State::Active {`
                //   `desc.set_state(State::Empty); desc.set_dirty_at(now);`
                //   `g.clear_partial(slab); g.empty.map.set(slab); g.stats.empty += 1;`
                let a = th.rel_arena as usize;
                let slab = th.rel_slab;
                let desc;
                if self.v.owner_check && sh.owner(self.sc, slab) != th.rel_arena {
                    desc = format!(
                        "on_live_zero: slab {slab} now owned by arena {}, skip",
                        sh.owner(self.sc, slab)
                    );
                } else {
                    let s = &mut sh.slabs[slab as usize];
                    if s.live == 0 && s.state == St::Active {
                        s.state = St::Empty;
                        sh.partial[a] &= !(1 << slab);
                        sh.empty[a] |= 1 << slab;
                        desc = format!(
                            "on_live_zero: slab {slab} -> Empty, into arena {a}'s empty set"
                        );
                    } else {
                        desc = format!(
                            "on_live_zero: slab {slab} is {:?} with live {}, nothing",
                            s.state, s.live
                        );
                    }
                }
                sh.lock[a] = None;
                th.sub = base + 6;
                Ok(desc + "; unlock")
            }
            5 => {
                // reopen: `let mut g = self.inner.lock();`
                let a = th.rel_arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = base + 7;
                Ok(format!("reopen: lock arena {a}"))
            }
            6 => {
                // The next slab of the batch while it is the same owner's
                // (`release_owned` runs on); else `self.reopen(&mut reopen)`
                // for the run just done, then `release_routed` moves to
                // the next owner.
                let next_owner = lowest_set(th.rel_todo).map(|s| sh.owner(self.sc, s));
                if th.was_full != 0 && next_owner != Some(th.rel_arena) {
                    th.sub = base + 5;
                    return Ok(format!(
                        "release: run of arena {} done, reopen pending",
                        th.rel_arena
                    ));
                }
                if th.rel_todo != 0 {
                    th.sub = base + 1;
                    return Ok(String::from("release: next slab"));
                }
                self.micro_done(th, op);
                Ok(String::from("release: batch done"))
            }
            _ => {
                // reopen under the lock, per slab:
                // `if self.desc(slab).state() == State::Active && self.table().owner(slab) == self.arena { g.set_partial(slab); }`
                let a = th.rel_arena as usize;
                let mut desc = String::from("reopen:");
                for slab in 0..NSLABS as u8 {
                    if th.was_full & (1 << slab) == 0 {
                        continue;
                    }
                    let s = &sh.slabs[slab as usize];
                    let ours = !self.v.owner_check || sh.owner(self.sc, slab) == th.rel_arena;
                    if s.state == St::Active && ours {
                        sh.partial[a] |= 1 << slab;
                        desc += &format!(" slab {slab} back in partial;");
                    } else {
                        desc += &format!(
                            " slab {slab} is {:?}{}, left;",
                            s.state,
                            if ours { "" } else { " (other arena)" }
                        );
                    }
                }
                th.was_full = 0;
                sh.lock[a] = None;
                if th.rel_todo != 0 {
                    th.sub = base + 1;
                    return Ok(desc + " unlock; next owner's slabs");
                }
                self.micro_done(th, op);
                Ok(desc + " unlock")
            }
        }
    }

    /// `plan_locked` and `take`, one lock hold. Up to `MAX_PICKS` picks.
    fn plan_locked(&self, sh: &mut Shared, th: &mut Thread, want: u8) -> Result<String, String> {
        let a = th.arena as usize;
        let mut remaining = want;
        th.np = 0;
        let mut desc = String::from("plan:");
        // `let mut next = g.partial.map.lowest_set(); while let Some(s) = next {`
        let mut set = sh.partial[a];
        while let Some(s) = lowest_set(set) {
            set &= !(1 << s);
            if remaining == 0 || th.np as usize == MAX_PICKS {
                break;
            }
            let slab = &sh.slabs[s as usize];
            // `if desc.live() >= cls.nslots() { if desc.state() == State::Active
            // && self.table().bits(s).is_full() { g.clear_partial(s); } } else { take }`
            if slab.live >= NSLOTS {
                if slab.state == St::Active && slab.bits == FULL {
                    sh.partial[a] &= !(1 << s);
                    desc += &format!(" slab {s} full, out of partial;");
                }
            } else {
                desc += &self.take(sh, th, s, &mut remaining)?;
            }
        }
        // `while remaining > 0 && np < MAX_PICKS { let s = match self.next_slab(..) ...; self.take(..) }`
        while remaining > 0 && (th.np as usize) < MAX_PICKS {
            let Some((s, how)) = self.next_slab(sh, th)? else {
                desc += " nothing more to commit;";
                break;
            };
            desc += &format!(" {how};");
            desc += &self.take(sh, th, s, &mut remaining)?;
        }
        Ok(desc)
    }

    /// `take`: reserve in `live`, fix the state and partial bit, record the
    /// pick; `claim_prefix` for a zero pick with the fix on.
    fn take(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        s: u8,
        remaining: &mut u8,
    ) -> Result<String, String> {
        let a = th.arena as usize;
        let slab = &mut sh.slabs[s as usize];
        // `debug_assert_eq!(self.table().owner(slab), self.arena)`
        if sh.owner[(s >> self.sc.lg_group) as usize] != th.arena || slab.arena != th.arena {
            return Err(format!("take: slab {s} is not arena {}'s", th.arena));
        }
        // `let live = desc.live(); ... let avail = nslots - live; let n = avail.min(*remaining);`
        let live = slab.live;
        if live > NSLOTS {
            return Err(format!("take: slab {s} live {live} above nslots"));
        }
        let avail = NSLOTS - live;
        let n = avail.min(*remaining);
        // `let state = desc.state(); if state != State::Active { desc.set_state(State::Active); desc.set_dirty_at(0); }`
        let state = slab.state;
        if state != St::Active {
            slab.state = St::Active;
        }
        // `desc.add_live(n);`
        slab.live += n;
        // `if live == 0 && n == avail { g.clear_partial(slab); } else { g.set_partial(slab); }`
        if live == 0 && n == avail {
            sh.partial[a] &= !(1 << s);
        } else {
            sh.partial[a] |= 1 << s;
        }
        // `let zero = state.is_zero() && (state != State::Purged || purge::purged_is_zero());`
        // (`MADV_DONTNEED` here, so a `Purged` slab is zero.)
        let zero = state.is_zero();
        let mut d = format!(
            " take slab {s} ({state:?}, live {live} -> {}, n {n}{})",
            slab.live,
            if zero { ", zero pick" } else { "" }
        );
        if zero && self.v.prefix_claim {
            // claim_prefix: `let got = bm.claim_word(w, want); if got != expect { zero_slab_had_bits_set(..) }`
            let expect = lowest_n_clear(0, n);
            let prev = slab.bits;
            let got = expect & !prev;
            if got != expect {
                return Err(format!(
                    "zero_slab_had_bits_set: slab {s} {state:?} live {live} bits {prev:#06b}"
                ));
            }
            slab.bits |= expect;
            d += &format!(", prefix {expect:#06b} claimed under the lock");
        }
        d += ";";
        th.picks[th.np as usize] = Pick {
            slab: s,
            n,
            zero,
            claimed: 0,
        };
        th.np += 1;
        *remaining -= n;
        Ok(d)
    }

    /// `next_slab`: the lowest `Empty`, else the lowest `Purged`, else
    /// the next slab of the current group (committing it), else a group
    /// adopted from another arena (with `group_handoff`), else the next
    /// fresh group of the class. `None` when the class is out of slabs.
    fn next_slab(&self, sh: &mut Shared, th: &mut Thread) -> Result<Option<(u8, String)>, String> {
        let a = th.arena as usize;
        let mut pre = String::new();
        loop {
            // `if let Some(s) = g.empty.map.lowest_set() { g.empty.map.clear(s); ... return Some(s); }`
            if let Some(s) = lowest_set(sh.empty[a]) {
                sh.empty[a] &= !(1 << s);
                return Ok(Some((
                    s,
                    format!("{pre}next_slab: Empty slab {s} out of the empty set"),
                )));
            }
            // `if let Some(s) = g.purged.map.lowest_set() { g.purged.map.clear(s); ... return Some(s); }`
            if let Some(s) = lowest_set(sh.purged[a]) {
                sh.purged[a] &= !(1 << s);
                return Ok(Some((
                    s,
                    format!("{pre}next_slab: Purged slab {s} out of the purged set"),
                )));
            }
            // `if g.group_next >= g.group_end {`
            if sh.group_next[a] >= sh.group_end[a] {
                // `if self.adopt_idle_group(g, meta, wake) { continue; }`
                if self.sc.group_handoff {
                    if let Some(d) = self.adopt_idle_group(sh, th)? {
                        // The adopted slabs are in this arena's sets now.
                        pre += &d;
                        pre += "; ";
                        continue;
                    }
                }
                // `let (start, end) = table.take_group(meta, self.arena, ..)?;`
                let group = sh.next_group as usize;
                if group >= (NSLABS >> self.sc.lg_group) {
                    return Ok(None);
                }
                let (start, end) = sh.group_range(self.sc, group);
                // take_group: the owner word and every descriptor's arena
                // are written together after the cross-check that both are
                // untouched.
                if sh.owner[group] != NARENAS as u8 {
                    return Err(format!(
                        "take_group: group {group} already owned by arena {}",
                        sh.owner[group]
                    ));
                }
                for s in start..end {
                    let slab = &mut sh.slabs[s as usize];
                    if slab.state != St::Fresh || slab.committed || slab.live != 0 || slab.bits != 0
                    {
                        return Err(format!(
                            "take_group: slab {s} of a fresh group is not Fresh"
                        ));
                    }
                    slab.arena = th.arena;
                }
                sh.owner[group] = th.arena;
                sh.next_group += 1;
                sh.group_next[a] = start;
                sh.group_end[a] = end;
            }
            // `let s = g.group_next; let usable = table.commit_slab(s)?; g.group_next = s + 1;`
            let s = sh.group_next[a];
            sh.group_next[a] = s + 1;
            sh.slabs[s as usize].committed = true;
            return Ok(Some((
                s,
                format!("{pre}next_slab: commit fresh slab {s} of arena {a}'s group"),
            )));
        }
    }

    /// `adopt_idle_group` and `transfer_group`, with this arena's lock
    /// held: `try_lock` each other arena (skip a held one), find its
    /// lowest idle group, verify and move it.
    fn adopt_idle_group(&self, sh: &mut Shared, th: &mut Thread) -> Result<Option<String>, String> {
        let a = th.arena as usize;
        for d in 0..NARENAS {
            if d == a {
                continue;
            }
            // `let Some(mut dg) = donor.inner.try_lock() else { continue; };`
            if sh.lock[d].is_some() {
                continue;
            }
            // next_idle_group: walk the donor's empty and purged sets by
            // group; a group is idle when every committed slab of it is
            // in them (`cursor` is the donor's `group_next` for its
            // current group, the group end otherwise).
            let ngroups = NSLABS >> self.sc.lg_group;
            for group in 0..ngroups {
                let (start, end) = sh.group_range(self.sc, group);
                if sh.owner[group] != d as u8 {
                    continue;
                }
                let cursor = if sh.group_end[d] == end {
                    sh.group_next[d]
                } else {
                    end
                };
                let in_sets = |s: u8| (sh.empty[d] | sh.purged[d]) & (1 << s) != 0;
                if !(start..end).any(in_sets) || !(start..cursor).all(in_sets) {
                    continue;
                }
                // transfer_group, under both locks. `owner_word == donor`,
                // then per slab: arena field, state against the donor's
                // sets, `live == 0`, no partial bit, not in the taker's
                // sets; abort (`idle_group_in_use`) otherwise.
                for s in start..end {
                    let slab = &sh.slabs[s as usize];
                    if slab.arena != d as u8 {
                        return Err(format!(
                            "transfer_group: slab {s} descriptor arena {} is not the donor {d}",
                            slab.arena
                        ));
                    }
                    let ok = match slab.state {
                        St::Empty => sh.empty[d] & (1 << s) != 0,
                        St::Purged => sh.purged[d] & (1 << s) != 0,
                        St::Fresh => s >= cursor && !in_sets(s),
                        St::Active => false,
                    };
                    let taker_has = (sh.partial[a] | sh.empty[a] | sh.purged[a]) & (1 << s) != 0;
                    if !ok || slab.live != 0 || sh.partial[d] & (1 << s) != 0 || taker_has {
                        return Err(format!(
                            "idle_group_in_use: slab {s} {:?} live {} (in donor sets {ok})",
                            slab.state, slab.live
                        ));
                    }
                }
                // The move: sets, `d.set_arena(self.arena)`, the owner
                // word, the cursor when it was the donor's current group.
                let mut moved = String::from("adopt: group ");
                moved += &format!("{group} from arena {d}:");
                for s in start..end {
                    let bit = 1 << s;
                    match sh.slabs[s as usize].state {
                        St::Empty => {
                            sh.empty[d] &= !bit;
                            sh.empty[a] |= bit;
                        }
                        St::Purged => {
                            sh.purged[d] &= !bit;
                            sh.purged[a] |= bit;
                        }
                        _ => {}
                    }
                    sh.slabs[s as usize].arena = th.arena;
                    moved += &format!(" slab {s} ({:?})", sh.slabs[s as usize].state);
                }
                sh.owner[group] = th.arena;
                if sh.group_end[d] == end {
                    sh.group_next[a] = sh.group_next[d];
                    sh.group_end[a] = end;
                    sh.group_next[d] = end;
                    sh.group_end[d] = end;
                }
                return Ok(Some(moved));
            }
        }
        Ok(None)
    }

    /// The invariants, at every state.
    fn check(&self, n: &Node) -> Result<(), String> {
        let sh = &n.shared;
        // Slots handed to two holders at once (a thread's staged or owned
        // slots, a cache's entries); a held slot's bit is set; a slot
        // reported zero and not yet written reads zero (the calloc race,
        // through a refill or through a cache's zero prefix).
        const CACHE_TAG: u8 = 100;
        fn who(h: u8) -> String {
            if h >= CACHE_TAG {
                format!("cache {}", h - CACHE_TAG)
            } else {
                format!("thread {h}")
            }
        }
        let mut holders = [[u8::MAX; NSLOTS as usize]; NSLABS];
        let mut place = |slab: u8, slot: u8, h: u8| -> Result<(), String> {
            let cell = &mut holders[slab as usize][slot as usize];
            if *cell != u8::MAX {
                return Err(format!(
                    "slot {slot} of slab {slab} is held by {} and {} at once",
                    who(*cell),
                    who(h)
                ));
            }
            *cell = h;
            Ok(())
        };
        for (t, th) in n.threads.iter().enumerate() {
            let owned = &th.owned[..th.nowned as usize];
            for h in th.held[..th.nheld as usize].iter().chain(owned) {
                place(h.slab, h.slot, t as u8)?;
                let s = &sh.slabs[h.slab as usize];
                if s.bits & (1 << h.slot) == 0 {
                    return Err(format!(
                        "thread {t} holds slot {} of slab {} whose bit is clear",
                        h.slot, h.slab
                    ));
                }
                if h.zero && s.mem & (1 << h.slot) != 0 {
                    return Err(format!(
                        "thread {t} holds slot {} of slab {} reported zero, and its memory is dirty (the calloc race)",
                        h.slot, h.slab
                    ));
                }
            }
        }
        for (c, cache) in sh.caches.iter().enumerate() {
            assert!(
                cache.len <= CACHE_CAP && cache.zeroed <= cache.len,
                "model: cache {c} len {} zeroed {}",
                cache.len,
                cache.zeroed
            );
            for (j, e) in cache.slots[..cache.len as usize].iter().enumerate() {
                place(e.slab, e.slot, CACHE_TAG + c as u8)?;
                let s = &sh.slabs[e.slab as usize];
                if s.bits & (1 << e.slot) == 0 {
                    return Err(format!(
                        "cache {c} holds slot {} of slab {} whose bit is clear",
                        e.slot, e.slab
                    ));
                }
                if (j as u8) < cache.zeroed && s.mem & (1 << e.slot) != 0 {
                    return Err(format!(
                        "cache {c} entry {j} (slot {} of slab {}) is in the zero prefix and its memory is dirty (the calloc race through the cache)",
                        e.slot, e.slab
                    ));
                }
            }
        }
        if self.sc.percpu {
            // The model's own bookkeeping: a section entry on a cid aborts
            // the section already on it, so at most one thread is inside
            // an unaborted section per cid.
            for (t, th) in n.threads.iter().enumerate() {
                if th.in_section {
                    assert!(
                        !n.threads[..t]
                            .iter()
                            .any(|o| o.in_section && o.sec_cid == th.sec_cid),
                        "model: two threads inside sections on cid {}",
                        th.sec_cid
                    );
                }
            }
        }
        for (i, s) in sh.slabs.iter().enumerate() {
            // Every set bit has exactly one holder, once the prefix bits a
            // zero pick claimed under the lock reach their thread in
            // `finish`: a bit with no holder is a slot lost (a push
            // overwritten by a stale store, or leaked).
            let held = holders[i].iter().filter(|&&h| h != u8::MAX).count() as u8;
            let mut prefix = 0u8;
            for th in &n.threads {
                for p in &th.picks[..th.np as usize] {
                    if p.slab as usize == i && p.zero && self.v.prefix_claim {
                        prefix += p.n - p.claimed;
                    }
                }
            }
            if popcount(s.bits) != held + prefix {
                return Err(format!(
                    "slab {i}: bits {:#06b} set but {held} holders + {prefix} prefix bits in flight (a slot lost, or claimed by nobody)",
                    s.bits
                ));
            }
            // `live` equals set bits plus in-flight reservations (picks
            // whose bits `finish` has not claimed yet) plus releases whose
            // `fetch_and` is done and whose `sub_live` is not.
            let mut reserved = 0u8;
            let mut pending = 0u8;
            for th in &n.threads {
                for p in &th.picks[..th.np as usize] {
                    if p.slab as usize == i && !(p.zero && self.v.prefix_claim) {
                        reserved += p.n - p.claimed;
                    }
                }
                if th.dec_pending && th.rel_slab as usize == i {
                    pending += th.rel_count;
                }
            }
            if s.live != popcount(s.bits) + reserved + pending {
                return Err(format!(
                    "slab {i}: live {} != {} set bits + {reserved} reserved + {pending} pending releases",
                    s.live,
                    popcount(s.bits)
                ));
            }
            if s.live > NSLOTS {
                return Err(format!("slab {i}: live {} above nslots", s.live));
            }
            let owner = sh.owner(self.sc, i as u8);
            let in_partial = (0..NARENAS)
                .filter(|&a| sh.partial[a] & (1 << i) != 0)
                .count();
            let in_empty: Vec<usize> = (0..NARENAS)
                .filter(|&a| sh.empty[a] & (1 << i) != 0)
                .collect();
            let in_purged: Vec<usize> = (0..NARENAS)
                .filter(|&a| sh.purged[a] & (1 << i) != 0)
                .collect();
            if in_partial + in_empty.len() + in_purged.len() > 1 {
                return Err(format!("slab {i} is in more than one set"));
            }
            if in_partial + in_empty.len() + in_purged.len() == 1 && owner == NARENAS as u8 {
                return Err(format!(
                    "slab {i} is in a set but its group was never handed out"
                ));
            }
            for &a in in_empty.iter().chain(in_purged.iter()) {
                if a as u8 != owner {
                    return Err(format!(
                        "slab {i} is in arena {a}'s sets but owned by arena {owner}"
                    ));
                }
            }
            if in_partial == 1
                && (s.state != St::Active || sh.partial[owner as usize] & (1 << i) == 0)
            {
                return Err(format!(
                    "slab {i} is in a partial set but {:?} or not its owner's",
                    s.state
                ));
            }
            match s.state {
                St::Empty => {
                    if in_empty.len() != 1 || s.live != 0 || s.bits != 0 {
                        return Err(format!(
                            "slab {i} is Empty with live {} bits {:#06b} in {} empty sets",
                            s.live,
                            s.bits,
                            in_empty.len()
                        ));
                    }
                }
                St::Purged => {
                    if in_purged.len() != 1 || s.live != 0 || s.bits != 0 || s.mem != 0 {
                        return Err(format!(
                            "slab {i} is Purged with live {} bits {:#06b} mem {:#06b} in {} purged sets",
                            s.live,
                            s.bits,
                            s.mem,
                            in_purged.len()
                        ));
                    }
                }
                St::Fresh => {
                    if s.live != 0
                        || s.bits != 0
                        || s.mem != 0
                        || !in_empty.is_empty()
                        || !in_purged.is_empty()
                    {
                        return Err(format!(
                            "slab {i} is Fresh with live {} bits {:#06b} mem {:#06b}",
                            s.live, s.bits, s.mem
                        ));
                    }
                }
                St::Active => {
                    if !in_empty.is_empty() || !in_purged.is_empty() {
                        return Err(format!("slab {i} is Active and in an empty or purged set"));
                    }
                }
            }
            if owner != NARENAS as u8 && s.arena != owner {
                return Err(format!(
                    "slab {i}: descriptor arena {} disagrees with the owner word {owner}",
                    s.arena
                ));
            }
        }
        // A cache is only ever mutated by a section whose cid matched at
        // commit (last, so that a corruption in the same state is named
        // first).
        if self.v.strict_cid {
            if let Some((t, c)) = sh.stale_commit {
                return Err(format!(
                    "cache {c} was mutated by thread {t}'s section after the thread lost cid {c} (the index came from a stale mm_cid read)"
                ));
            }
        }
        Ok(())
    }

    fn explore(&mut self, n: Node) -> Result<(), (String, Trace)> {
        if !self.seen.insert(n.clone()) {
            return Ok(());
        }
        assert!(
            self.seen.len() <= MAX_STATES,
            "model: {} states and counting: the scenario is too large for the host budget",
            self.seen.len()
        );
        if let Err(msg) = self.check(&n) {
            return Err((msg, Trace(self.trace.clone())));
        }
        let mut any = false;
        for t in 0..n.threads.len() {
            if !self.enabled(&n, t) {
                continue;
            }
            any = true;
            for choice in 0..self.choices(&n, t) {
                let mut next = n.clone();
                let op = self.program(t)[next.threads[t].pc as usize];
                let sub = next.threads[t].sub;
                let desc = match self.step(&mut next, t, choice) {
                    Ok(d) => d,
                    Err(abort) => {
                        self.trace
                            .push((t as u8, op, sub, format!("ABORT {abort}")));
                        return Err((
                            format!("the protocol aborted: {abort}"),
                            Trace(self.trace.clone()),
                        ));
                    }
                };
                self.trace.push((t as u8, op, sub, desc));
                let r = self.explore(next);
                self.trace.pop();
                r?;
            }
        }
        // Per-CPU mode: a thread inside a section may be preempted at any
        // instruction (a signal, a migration, a time slice) whether or
        // not another thread then runs on its cid; the kernel finds the
        // IP in `[start_ip, post_commit_ip)` and returns to user space at
        // `abort_ip`, the section's start. (A thread outside a section
        // loses nothing by a preemption the caches can observe, except its
        // cid to another thread's section, which `enter_section` models.)
        if self.sc.percpu {
            for t in 0..n.threads.len() {
                if !n.threads[t].in_section {
                    continue;
                }
                let mut next = n.clone();
                let nt = &mut next.threads[t];
                let at = self.section_start(nt.sub);
                let desc = format!(
                    "scheduler: T{t} preempted inside its section on cid {}: rseq abort, retry at [{at}]",
                    nt.sec_cid
                );
                self.abort(nt, at);
                self.trace.push((t as u8, Op::Sched, 0, desc));
                let r = self.explore(next);
                self.trace.pop();
                r?;
            }
        }
        if !any {
            let unfinished: Vec<usize> = (0..n.threads.len())
                .filter(|&t| !self.finished(&n.threads[t], t))
                .collect();
            if !unfinished.is_empty() {
                return Err((
                    format!(
                        "deadlock: threads {unfinished:?} have no enabled step (a reservation no release can meet, or a lock never dropped)"
                    ),
                    Trace(self.trace.clone()),
                ));
            }
        }
        Ok(())
    }
}

/// Run `sc` under `v`: every interleaving, every state checked. The
/// exploration is depth first and recursive, one frame per step of the
/// path (a `Node` clone each), and with the scheduler as an actor a path
/// runs tens of thousands of steps deep, past the 2 MiB stack of a test
/// thread: it runs on its own thread with a 1 GiB stack (reserved, not
/// touched).
pub fn explore(sc: &Scenario, v: Variant) -> Outcome {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, || explore_here(sc, v))
            .expect("model: spawn the exploration thread")
            .join()
            .expect("model: the exploration thread panicked")
    })
}

fn explore_here(sc: &Scenario, v: Variant) -> Outcome {
    assert!(sc.threads.len() <= MAX_THREADS);
    for (_, p) in &sc.threads {
        assert!(p.len() <= MAX_OPS);
    }
    let shared = Shared {
        lock: [None; NARENAS],
        slabs: [Slab {
            state: St::Fresh,
            live: 0,
            bits: 0,
            mem: 0,
            arena: 0,
            committed: false,
        }; NSLABS],
        partial: [0; NARENAS],
        empty: [0; NARENAS],
        purged: [0; NARENAS],
        owner: [NARENAS as u8; NSLABS],
        group_next: [0; NARENAS],
        group_end: [0; NARENAS],
        next_group: 0,
        caches: [Cache::default(); NCACHES],
        stale_commit: None,
    };
    let threads = sc.threads.iter().map(|(a, _)| Thread::new(*a)).collect();
    let mut m = Model {
        sc,
        v,
        seen: HashSet::new(),
        trace: Vec::new(),
        cross_pushes: Cell::new(0),
        aborts: Cell::new(0),
    };
    let violation = m.explore(Node { shared, threads }).err();
    Outcome {
        states: m.seen.len(),
        violation,
        cross_pushes: m.cross_pushes.get(),
        aborts: m.aborts.get(),
    }
}

impl Trace {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, (t, op, sub, desc)) in self.0.iter().enumerate() {
            out += &format!("  {i:>2}. T{t} {op:?}[{sub}]: {desc}\n");
        }
        out
    }
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
    }
}

fn run(sc: &Scenario, v: Variant) -> Outcome {
    let start = Instant::now();
    let out = explore(sc, v);
    eprintln!(
        "model: {} under {v:?}: {} states in {:.2} s, {}",
        sc.name,
        out.states,
        start.elapsed().as_secs_f64(),
        match &out.violation {
            None => String::from("no violation"),
            Some((m, tr)) => format!("VIOLATION: {m}\n{}", tr.render()),
        }
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pre_fix_order_has_the_calloc_race_and_the_model_prints_it() {
        let out = run(
            &calloc_race(),
            Variant {
                prefix_claim: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("the pre-fix protocol must show the race");
        assert!(
            msg.contains("reported zero") && msg.contains("dirty"),
            "{msg}"
        );
        // The interleaving is the campaign's mechanism: A's plan flags a
        // zero pick and drops the lock; B claims the slot, writes it and
        // releases it; A's bit loop then takes the same slot.
        let steps: Vec<&(u8, Op, u8, String)> = tr.0.iter().collect();
        let a_plan = steps
            .iter()
            .position(|s| s.0 == 0 && s.3.contains("zero pick"))
            .expect("A plans a zero pick");
        let b_write = steps
            .iter()
            .position(|s| s.0 == 1 && matches!(s.1, Op::Write(_)))
            .expect("B writes");
        let b_release = steps
            .iter()
            .position(|s| s.0 == 1 && s.3.starts_with("release: fetch_and"))
            .expect("B releases");
        let a_claim = steps
            .iter()
            .rposition(|s| s.0 == 0 && s.3.contains("pick flagged zero"))
            .expect("A claims under the stale flag");
        assert!(
            a_plan < b_write && b_write < b_release && b_release < a_claim,
            "{}",
            tr.render()
        );
    }

    #[test]
    fn the_current_order_passes_the_calloc_race_scenario() {
        let out = run(&calloc_race(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(out.states > 100);
    }

    #[test]
    fn the_current_order_passes_the_churn_scenario() {
        let out = run(&churn(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
    }

    #[test]
    fn the_current_order_passes_the_three_way_scenario() {
        let start = Instant::now();
        let out = run(&three_way(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(
            start.elapsed().as_secs() < 30,
            "the model must stay under 30 s on the host"
        );
    }

    #[test]
    fn the_pre_fix_order_has_the_race_on_a_purged_slab_too() {
        // The same window after a purge: A's second refill picks the
        // `Purged` slab. Drop the `Fresh` round from the programs so the
        // violation the model finds is the `Purged` one.
        let sc = Scenario {
            name: "calloc race on a Purged slab",
            threads: vec![
                (
                    0,
                    vec![
                        Op::Refill(1),
                        Op::Write(0),
                        Op::Release,
                        Op::Purge,
                        Op::Refill(1),
                    ],
                ),
                (0, vec![Op::Refill(1), Op::Write(0), Op::Release]),
            ],
            lg_group: 1,
            group_handoff: false,
            percpu: false,
        };
        let out = run(
            &sc,
            Variant {
                prefix_claim: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("the pre-fix protocol must show the race on a Purged slab");
        assert!(msg.contains("the calloc race"), "{msg}");
        assert!(
            tr.0.iter()
                .any(|s| s.3.contains("Purged slab 0 out of the purged set")),
            "{}",
            tr.render()
        );
        let out = run(&sc, CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
    }

    #[test]
    fn the_handoff_without_the_owner_check_puts_a_foreign_slab_in_an_arenas_sets() {
        let out = run(
            &handoff(),
            Variant {
                owner_check: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("without the owner check the handoff scenario must fail");
        assert!(
            msg.contains("owned by arena")
                || msg.contains("more than one set")
                || msg.contains("held by threads"),
            "{msg}\n{}",
            tr.render()
        );
        assert!(
            tr.0.iter().any(|s| s.3.contains("adopt: group")),
            "{}",
            tr.render()
        );
    }

    #[test]
    fn the_current_order_passes_the_handoff_scenario() {
        let out = run(&handoff(), CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(out.states > 100);
    }

    #[test]
    fn lowest_n_clear_takes_the_lowest_clear_bits() {
        assert_eq!(lowest_n_clear(0b0000, 1), 0b0001);
        assert_eq!(lowest_n_clear(0b0101, 2), 0b1010);
        assert_eq!(lowest_n_clear(0b1111, 1), 0);
        assert_eq!(lowest_n_clear(0b0001, 4), 0b1110);
    }

    /// The per-CPU protocol as designed (`CURRENT`: `Section::Rseq`)
    /// passes `sc` inside the host budget.
    fn percpu_passes(sc: &Scenario) -> Outcome {
        let start = Instant::now();
        let out = run(sc, CURRENT);
        assert!(
            out.violation.is_none(),
            "{}",
            out.violation.unwrap().1.render()
        );
        assert!(
            start.elapsed().as_secs() < 30,
            "the model must stay under 30 s on the host"
        );
        out
    }

    /// A violation the buggy per-CPU variants are expected to produce:
    /// the stale commit itself, a slot handed to two holders, a slot lost
    /// (a push overwritten), or a dirty slot reported zero.
    fn is_percpu_corruption(msg: &str) -> bool {
        msg.contains("stale mm_cid")
            || msg.contains("at once")
            || msg.contains("a slot lost")
            || msg.contains("calloc race")
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_share_scenario() {
        let out = percpu_passes(&percpu_share());
        assert!(out.states > 100);
        assert!(out.aborts > 0, "the scheduler never aborted a section");
        assert!(
            out.cross_pushes > 0,
            "no slot was pushed into a cache other than the one it came from"
        );
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_migrate_scenario() {
        let out = percpu_passes(&percpu_migrate());
        assert!(out.cross_pushes > 0);
    }

    #[test]
    fn the_rseq_protocol_passes_the_per_cpu_purge_scenario() {
        percpu_passes(&percpu_purge());
    }

    #[test]
    fn without_the_section_a_slot_is_handed_to_two_threads_or_a_push_is_lost() {
        // With the cid check off the model runs past the stale commit to
        // the corruption it causes and names it.
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::None,
                strict_cid: false,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("plain loads and stores under a stale cid must corrupt a cache");
        assert!(
            msg.contains("at once") || msg.contains("a slot lost") || msg.contains("calloc race"),
            "{msg}\n{}",
            tr.render()
        );
        // The mechanism: a thread taken off its cid between a speculative
        // read and its commit, and no abort.
        assert!(
            tr.0.iter().any(|s| s.3.contains("goes on stale")),
            "{}",
            tr.render()
        );
        assert!(
            !tr.0.iter().any(|s| s.3.contains("rseq abort")),
            "{}",
            tr.render()
        );
        // With the check on, the stale commit is named as such unless the
        // corruption is in the same state.
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::None,
                ..CURRENT
            },
        );
        let (msg, tr) = out.violation.expect("the stale commit must be found");
        assert!(is_percpu_corruption(&msg), "{msg}\n{}", tr.render());
    }

    #[test]
    fn the_cid_read_before_the_section_is_the_same_bug() {
        let out = run(
            &percpu_share(),
            Variant {
                section: Section::CidOutside,
                ..CURRENT
            },
        );
        let (msg, tr) = out
            .violation
            .expect("a cache index from an mm_cid read outside the section must fail");
        assert!(is_percpu_corruption(&msg), "{msg}\n{}", tr.render());
        // The section did abort where it was preempted; the index it
        // kept is what went stale.
        assert!(
            tr.0.iter().any(|s| s.3.contains("read before the section")),
            "{}",
            tr.render()
        );
    }
}
