//! The steps (module docs in `mod.rs`): which step a thread takes next,
//! whether it is enabled, and its effect on the state, one method per
//! operation of `class/` (`Refill` is `ClassState::claim`, `Release` is
//! `release_owned` and `dec_live`, `Purge` is `purge_empty`; the line each
//! step mirrors is quoted at the step); then the invariants checked at
//! every state and the depth-first exploration itself. A step returns
//! `Err` on an abort of the real code (a heap inconsistency the protocol
//! detects itself). The sub-steps are the variants of
//! [`class::protocol`](crate::class::protocol), matched exhaustively: a
//! step the code gains that the model lacks does not compile.

use super::*;
use crate::class::protocol::{Purge, Refill, Release};

impl Model<'_> {
    pub(super) fn program(&self, t: usize) -> &[Op] {
        &self.sc.threads[t].1
    }

    pub(super) fn finished(&self, th: &Thread, t: usize) -> bool {
        th.pc as usize >= self.program(t).len()
    }

    /// Whether thread `t`'s next step can run now.
    pub(super) fn enabled(&self, n: &Node, t: usize) -> bool {
        let th = &n.threads[t];
        if self.finished(th, t) {
            return false;
        }
        let sh = &n.shared;
        let a = th.arena as usize;
        let s = th.sub;
        match self.program(t)[th.pc as usize] {
            // Lock acquisitions: `self.inner.lock()` blocks while held.
            Op::Refill(_) if Refill::at(s) == Refill::Lock => sh.lock[a].is_none(),
            Op::Purge if Purge::at(s) == Purge::Lock => sh.lock[a].is_none(),
            Op::Release
                if matches!(Release::at(s), Release::LockLiveZero | Release::LockReopen) =>
            {
                sh.lock[th.rel_arena as usize].is_none()
            }
            // `finish`'s bit loop: `claim_from` finds nothing while every
            // word is full and rescans (`MAX_EMPTY_PASSES`); the model
            // waits for a clear bit instead of spinning.
            Op::Refill(_) if Refill::at(s) == Refill::Finish => {
                let p = th.picks[th.cur as usize];
                p.zero && self.v.prefix_claim || sh.slabs[p.slab as usize].bits != FULL
            }
            _ => true,
        }
    }

    /// Run thread `t`'s next step. `Ok(description)`.
    pub(super) fn step(&self, n: &mut Node, t: usize) -> Result<String, String> {
        let op = self.program(t)[n.threads[t].pc as usize];
        let sub = n.threads[t].sub;
        let sh = &mut n.shared;
        let th = &mut n.threads[t];
        let tid = t as u8;
        match op {
            Op::Refill(want) => self.step_refill(sh, th, tid, want, sub),
            Op::Write(i) => {
                // The program writes a slot it was given; a refill that
                // came up short (the class exhausted by the other
                // threads) gave fewer, and then there is nothing to write.
                th.done();
                let n = th.nheld;
                if i >= n {
                    return Ok(format!(
                        "write: the program has {n} slots, none at index {i}"
                    ));
                }
                let h = th.held[i as usize];
                sh.slabs[h.slab as usize].mem |= 1 << h.slot;
                th.held[i as usize].zero = false;
                Ok(format!("write slot {} of slab {}", h.slot, h.slab))
            }
            Op::Release => self.step_release(sh, th, tid, sub),
            Op::Purge => self.step_purge(sh, th, tid, sub),
        }
    }

    /// `ClassState::purge_empty`: the steps of `Purge`.
    pub(super) fn step_purge(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        sub: u8,
    ) -> Result<String, String> {
        match Purge::at(sub) {
            Purge::Lock => {
                // purge_empty: `let mut g = self.inner.lock();`
                let a = th.arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = Purge::PurgeLocked as u8;
                Ok(format!("purge: lock arena {a}"))
            }
            Purge::PurgeLocked => {
                // purge_empty under the lock: `walk_empty` asserts
                // `desc.state() == State::Empty && desc.live() == 0` for
                // every slab of the empty set, then `purge_group`:
                // `d.set_state(State::Purged); d.set_dirty_at(0);
                // g.empty_take(s); g.purged_add(s);` and the discard
                // (`batch.flush()` before the lock is dropped), after
                // which the memory reads zero.
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
        }
    }

    /// `ClassState::claim`: the steps of `Refill`.
    pub(super) fn step_refill(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        want: u8,
        sub: u8,
    ) -> Result<String, String> {
        let a = th.arena as usize;
        match Refill::at(sub) {
            Refill::Lock => {
                // plan: `let mut g = self.inner.lock();`
                sh.lock[a] = Some(tid);
                th.sub = Refill::PlanLocked as u8;
                Ok(format!("lock arena {a}"))
            }
            Refill::PlanLocked => {
                // plan under the lock: the partial walk, then
                // `next_slab` (empty, purged, the group cursor, a fresh
                // group), `take` per slab.
                // `self.plan(meta, out.len() - got, &mut picks)`
                let desc = self.plan_locked(sh, th, want - th.got)?;
                th.sub = Refill::Unlock as u8;
                Ok(desc)
            }
            Refill::Unlock => {
                // `plan` returns: the lock hold ends.
                sh.lock[a] = None;
                th.cur = 0;
                if th.np == 0 {
                    th.done();
                    return Ok(String::from("unlock; nothing to commit, claim returns 0"));
                }
                th.sub = Refill::Finish as u8;
                Ok(String::from("unlock"))
            }
            Refill::Finish => {
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
                            th.sub = Refill::Lock as u8;
                            return Ok(desc + "; short, plan again");
                        }
                        th.done();
                    }
                }
                Ok(desc)
            }
        }
    }

    /// `ClassState::release` through `Heap::release_routed`: the steps of
    /// `Release`. Each slab is released through the
    /// arena its owner word names; a run of one owner ends with that
    /// arena's `reopen`.
    pub(super) fn step_release(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        sub: u8,
    ) -> Result<String, String> {
        match Release::at(sub) {
            Release::Group => {
                // release_owned: the batch grouped by slab, lowest slab
                // first (`Heap::flush`'s bucket pass).
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
                th.sub = Release::FetchAnd as u8;
                Ok(format!("release: slabs {todo:#04b}"))
            }
            Release::FetchAnd => {
                // release_routed: `self.owner(a).class(class).release_owned(..)`
                // with `a` the owner word's arena (`arena_of_slot`), then
                // release_word for the slab's one word:
                // `let (prev, was_full) = self.table().bits(slab).release(word, mask);`
                // `if prev & mask != mask { double_free(...) }`
                // preceded by `release_owned`'s owner compare (the
                // descriptor's arena against the word) and `Fresh` check.
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
                th.sub = Release::SubLive as u8;
                Ok(format!(
                    "release: fetch_and !{mask:#06b} on slab {slab} of arena {owner} (bits {prev:#06b} -> {:#06b}){}",
                    s.bits,
                    if prev == FULL { ", was full" } else { "" }
                ))
            }
            Release::SubLive => {
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
                th.sub = if th.rel_zero {
                    Release::LockLiveZero
                } else {
                    Release::NextSlab
                } as u8;
                Ok(format!(
                    "release: sub_live({}) on slab {} ({prev} -> {}){}",
                    th.rel_count,
                    th.rel_slab,
                    s.live,
                    if th.rel_zero { ", reached zero" } else { "" }
                ))
            }
            Release::LockLiveZero => {
                // on_live_zero: `let mut g = self.inner.lock();`
                let a = th.rel_arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = Release::OnLiveZero as u8;
                Ok(format!("on_live_zero: lock arena {a}"))
            }
            Release::OnLiveZero => {
                // on_live_zero under the lock:
                // `if self.table().owner(slab) != self.arena { return; }`
                // `if desc.live() == 0 && desc.state() == State::Active {`
                //   `desc.set_state(State::Empty); desc.set_dirty_at(now);`
                //   `g.clear_partial(slab); g.empty_add(slab);`
                let a = th.rel_arena as usize;
                let slab = th.rel_slab;
                let desc;
                if sh.owner(self.sc, slab) != th.rel_arena {
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
                th.sub = Release::NextSlab as u8;
                Ok(desc + "; unlock")
            }
            Release::NextSlab => {
                // The next slab of the batch while it is the same owner's
                // (`release_owned` runs on); else `self.reopen(&mut reopen)`
                // for the run just done, then `release_routed` moves to
                // the next owner.
                let next_owner = lowest_set(th.rel_todo).map(|s| sh.owner(self.sc, s));
                if th.was_full != 0 && next_owner != Some(th.rel_arena) {
                    th.sub = Release::LockReopen as u8;
                    return Ok(format!(
                        "release: run of arena {} done, reopen pending",
                        th.rel_arena
                    ));
                }
                if th.rel_todo != 0 {
                    th.sub = Release::FetchAnd as u8;
                    return Ok(String::from("release: next slab"));
                }
                th.done();
                Ok(String::from("release: batch done"))
            }
            Release::LockReopen => {
                // reopen: `let mut g = self.inner.lock();`
                let a = th.rel_arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = Release::Reopen as u8;
                Ok(format!("reopen: lock arena {a}"))
            }
            Release::Reopen => {
                // reopen under the lock, per slab:
                // `if self.desc(slab).state() == State::Active && self.table().owner(slab) == self.arena { g.set_partial(slab); }`
                let a = th.rel_arena as usize;
                let mut desc = String::from("reopen:");
                for slab in 0..NSLABS as u8 {
                    if th.was_full & (1 << slab) == 0 {
                        continue;
                    }
                    let s = &sh.slabs[slab as usize];
                    let ours = sh.owner(self.sc, slab) == th.rel_arena;
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
                    th.sub = Release::FetchAnd as u8;
                    return Ok(desc + " unlock; next owner's slabs");
                }
                th.done();
                Ok(desc + " unlock")
            }
        }
    }

    /// `plan` and `take`, one lock hold. Up to `MAX_PICKS` picks.
    pub(super) fn plan_locked(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        want: u8,
    ) -> Result<String, String> {
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
    pub(super) fn take(
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
    /// the next slab of the current group (committing it), else the next
    /// fresh group of the class. `None` when the class is out of slabs.
    pub(super) fn next_slab(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
    ) -> Result<Option<(u8, String)>, String> {
        let a = th.arena as usize;
        // `if let Some(s) = g.empty.map.lowest_set() { g.empty.map.clear(s); ... return Some(s); }`
        if let Some(s) = lowest_set(sh.empty[a]) {
            sh.empty[a] &= !(1 << s);
            return Ok(Some((
                s,
                format!("next_slab: Empty slab {s} out of the empty set"),
            )));
        }
        // `if let Some(s) = g.purged.map.lowest_set() { g.purged.map.clear(s); ... return Some(s); }`
        if let Some(s) = lowest_set(sh.purged[a]) {
            sh.purged[a] &= !(1 << s);
            return Ok(Some((
                s,
                format!("next_slab: Purged slab {s} out of the purged set"),
            )));
        }
        // `if g.group_next >= g.group_end {`
        if sh.group_next[a] >= sh.group_end[a] {
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
                if slab.state != St::Fresh || slab.committed || slab.live != 0 || slab.bits != 0 {
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
        // `let s = g.group_next; let usable = if table.starts_run(s) { table.commit_run(s, g.group_end)? } else { true }; g.group_next = s + 1;`
        let s = sh.group_next[a];
        sh.group_next[a] = s + 1;
        sh.slabs[s as usize].committed = true;
        Ok(Some((
            s,
            format!("next_slab: commit fresh slab {s} of arena {a}'s group"),
        )))
    }

    /// The invariants, at every state.
    pub(super) fn check(&self, n: &Node) -> Result<(), String> {
        let sh = &n.shared;
        // Slots handed to two holders at once; a held slot's bit is set; a
        // slot reported zero and not yet written reads zero (the calloc
        // race).
        let mut holders = [[u8::MAX; NSLOTS as usize]; NSLABS];
        let mut place = |slab: u8, slot: u8, h: u8| -> Result<(), String> {
            let cell = &mut holders[slab as usize][slot as usize];
            if *cell != u8::MAX {
                return Err(format!(
                    "slot {slot} of slab {slab} is held by thread {} and thread {h} at once",
                    *cell
                ));
            }
            *cell = h;
            Ok(())
        };
        for (t, th) in n.threads.iter().enumerate() {
            for h in &th.held[..th.nheld as usize] {
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
        for (i, s) in sh.slabs.iter().enumerate() {
            // Every set bit has exactly one holder, once the prefix bits a
            // zero pick claimed under the lock reach their thread in
            // `finish`: a bit with no holder is a slot leaked.
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
                    "slab {i}: bits {:#06b} set but {held} holders + {prefix} prefix bits in flight (a slot leaked, or claimed by nobody)",
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
        Ok(())
    }

    pub(super) fn explore(&mut self, n: Node) -> Result<(), (String, Trace)> {
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
