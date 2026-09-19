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

/// The shared state: what every thread's steps read and write.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Shared {
    /// The class lock of each arena: the thread holding it, or none.
    lock: [Option<u8>; NARENAS],
    slabs: [Slab; NSLABS],
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
    /// `ClassState::purge_empty(u64::MAX)` on the thread's arena.
    Purge,
}

/// A slot a thread holds: slab, slot, and whether the refill reported it
/// zero (cleared by a write).
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
}

pub const CURRENT: Variant = Variant {
    prefix_claim: true,
    owner_check: true,
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
}

struct Model<'a> {
    sc: &'a Scenario,
    v: Variant,
    seen: HashSet<Node>,
    trace: Vec<(u8, Op, u8, String)>,
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
        }
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
        self.np = 0;
        self.got = 0;
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
        match (self.program(t)[th.pc as usize], th.sub) {
            // Lock acquisitions: `self.inner.lock()` blocks while held.
            (Op::Refill(_), 0) | (Op::Purge, 0) => sh.lock[a].is_none(),
            (Op::Release, 3) | (Op::Release, 5) => sh.lock[a].is_none(),
            // `finish`'s bit loop: `claim_from` finds nothing while every
            // word is full and rescans (`MAX_EMPTY_PASSES`); the model
            // waits for a clear bit instead of spinning.
            (Op::Refill(_), 3) => {
                let p = th.picks[th.cur as usize];
                p.zero && self.v.prefix_claim || sh.slabs[p.slab as usize].bits != FULL
            }
            _ => true,
        }
    }

    /// Run thread `t`'s next step. `Ok(description)`.
    fn step(&self, n: &mut Node, t: usize) -> Result<String, String> {
        let op = self.program(t)[n.threads[t].pc as usize];
        let sub = n.threads[t].sub;
        let (sh, ths) = (&mut n.shared, &mut n.threads);
        let th = &mut ths[t];
        let a = th.arena as usize;
        let tid = t as u8;
        match op {
            Op::Refill(want) => match sub {
                0 => {
                    // plan_locked: `let mut g = self.inner.lock();`
                    sh.lock[a] = Some(tid);
                    th.sub = 1;
                    Ok(format!("lock arena {a}"))
                }
                1 => {
                    // plan_locked under the lock: the partial walk, then
                    // `next_slab` (empty, purged, the group cursor, the
                    // handoff, a fresh group), `take` per slab.
                    // `self.plan(meta, out.len() - got, &mut picks)`
                    let desc = self.plan_locked(sh, th, want - th.got)?;
                    th.sub = 2;
                    Ok(desc)
                }
                2 => {
                    // `plan` returns: the lock hold ends.
                    sh.lock[a] = None;
                    th.cur = 0;
                    if th.np == 0 {
                        th.done();
                        return Ok(String::from("unlock; nothing to commit, claim returns 0"));
                    }
                    th.sub = 3;
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
                                th.sub = 0;
                                return Ok(desc + "; short, plan again");
                            }
                            th.done();
                        }
                    }
                    Ok(desc)
                }
            },
            Op::Write(i) => {
                // The program writes a slot it was given; a refill that
                // came up short (the class exhausted by the other
                // threads) gave fewer, and then there is nothing to write.
                th.done();
                if i >= th.nheld {
                    return Ok(format!(
                        "write: the refill gave {} slots, none at index {i}",
                        th.nheld
                    ));
                }
                let h = th.held[i as usize];
                sh.slabs[h.slab as usize].mem |= 1 << h.slot;
                th.held[i as usize].zero = false;
                Ok(format!("write slot {} of slab {}", h.slot, h.slab))
            }
            Op::Release => match sub {
                0 => {
                    // release_owned: the batch grouped by slab, lowest
                    // slab first (`Heap::flush`'s bucket pass).
                    let mut todo = 0u8;
                    for h in &th.held[..th.nheld as usize] {
                        todo |= 1 << h.slab;
                    }
                    if todo == 0 {
                        th.done();
                        return Ok(String::from("release: nothing held"));
                    }
                    th.rel_todo = todo;
                    th.was_full = 0;
                    th.sub = 1;
                    Ok(format!("release: slabs {todo:#04b}"))
                }
                1 => {
                    // release_word for the slab's one word:
                    // `let (prev, was_full) = self.table().bits(slab).release(word, mask);`
                    // `if prev & mask != mask { double_free(...) }`
                    // preceded by `release_owned`'s owner and `Fresh` checks.
                    let slab = lowest_set(th.rel_todo).unwrap();
                    th.rel_todo &= !(1 << slab);
                    if sh.owner(self.sc, slab) != th.arena {
                        return Err(format!(
                            "release_owned: slab {slab} is owned by arena {}, releasing arena {}",
                            sh.owner(self.sc, slab),
                            th.arena
                        ));
                    }
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
                    th.sub = 2;
                    Ok(format!(
                        "release: fetch_and !{mask:#06b} on slab {slab} (bits {prev:#06b} -> {:#06b}){}",
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
                    th.sub = if th.rel_zero { 3 } else { 6 };
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
                    sh.lock[a] = Some(tid);
                    th.sub = 4;
                    Ok(format!("on_live_zero: lock arena {a}"))
                }
                4 => {
                    // on_live_zero under the lock:
                    // `if self.table().owner(slab) != self.arena { return; }`
                    // `if desc.live() == 0 && desc.state() == State::Active {`
                    //   `desc.set_state(State::Empty); desc.set_dirty_at(now);`
                    //   `g.clear_partial(slab); g.empty.map.set(slab); g.stats.empty += 1;`
                    let slab = th.rel_slab;
                    let desc;
                    if self.v.owner_check && sh.owner(self.sc, slab) != th.arena {
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
                    th.sub = 6;
                    Ok(desc + "; unlock")
                }
                5 => {
                    // reopen: `let mut g = self.inner.lock();`
                    sh.lock[a] = Some(tid);
                    th.sub = 7;
                    Ok(format!("reopen: lock arena {a}"))
                }
                6 => {
                    // The next slab of the batch, else `self.reopen(&mut reopen)`.
                    if th.rel_todo != 0 {
                        th.sub = 1;
                        return Ok(String::from("release: next slab"));
                    }
                    if th.was_full != 0 {
                        th.sub = 5;
                        return Ok(String::from("release: batch done, reopen pending"));
                    }
                    th.done();
                    Ok(String::from("release: batch done"))
                }
                _ => {
                    // reopen under the lock, per slab:
                    // `if self.desc(slab).state() == State::Active && self.table().owner(slab) == self.arena { g.set_partial(slab); }`
                    let mut desc = String::from("reopen:");
                    for slab in 0..NSLABS as u8 {
                        if th.was_full & (1 << slab) == 0 {
                            continue;
                        }
                        let s = &sh.slabs[slab as usize];
                        let ours = !self.v.owner_check || sh.owner(self.sc, slab) == th.arena;
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
                    th.done();
                    Ok(desc + " unlock")
                }
            },
            Op::Purge => match sub {
                0 => {
                    // purge_empty: `let mut g = self.inner.lock();`
                    sh.lock[a] = Some(tid);
                    th.sub = 1;
                    Ok(format!("purge: lock arena {a}"))
                }
                _ => {
                    // purge_empty under the lock: for every slab of the
                    // empty set, `assert!(desc.state() == State::Empty &&
                    // desc.live() == 0)`, then `purge_group`:
                    // `d.set_state(State::Purged); d.set_dirty_at(0);
                    // g.empty.map.clear(s); g.purged.map.set(s);` and the
                    // discard (`batch.flush()` before the lock is dropped),
                    // after which the memory reads zero.
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
            },
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
        // Slots handed to two claimers at once; a held slot's bit is set;
        // a slot reported zero and not yet written reads zero.
        let mut holders = [[u8::MAX; NSLOTS as usize]; NSLABS];
        for (t, th) in n.threads.iter().enumerate() {
            for h in &th.held[..th.nheld as usize] {
                let cell = &mut holders[h.slab as usize][h.slot as usize];
                if *cell != u8::MAX {
                    return Err(format!(
                        "slot {} of slab {} is held by threads {} and {t} at once",
                        h.slot, h.slab, *cell
                    ));
                }
                *cell = t as u8;
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
        for (i, s) in sh.slabs.iter().enumerate() {
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
            let mut next = n.clone();
            let op = self.program(t)[next.threads[t].pc as usize];
            let sub = next.threads[t].sub;
            let desc = match self.step(&mut next, t) {
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

/// Run `sc` under `v`: every interleaving, every state checked.
pub fn explore(sc: &Scenario, v: Variant) -> Outcome {
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
    };
    let threads = sc.threads.iter().map(|(a, _)| Thread::new(*a)).collect();
    let mut m = Model {
        sc,
        v,
        seen: HashSet::new(),
        trace: Vec::new(),
    };
    let violation = m.explore(Node { shared, threads }).err();
    Outcome {
        states: m.seen.len(),
        violation,
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
                owner_check: true,
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
        };
        let out = run(
            &sc,
            Variant {
                prefix_claim: false,
                owner_check: true,
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
                prefix_claim: true,
                owner_check: false,
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
}
