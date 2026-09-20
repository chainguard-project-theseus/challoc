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
        match micro(self.program(t)[th.pc as usize], th.sub) {
            // Lock acquisitions: `self.inner.lock()` blocks while held.
            Micro::Refill(_, s) if Refill::at(s) == Refill::Lock => sh.lock[a].is_none(),
            Micro::Purge(s) if Purge::at(s) == Purge::Lock => sh.lock[self.purge_arena(th, t)].is_none(),
            Micro::Release(s) if matches!(Release::at(s), Release::LockLiveZero | Release::LockReopen) => {
                sh.lock[th.rel_arena as usize].is_none()
            }
            // `finish`'s bit loop: `claim_from` finds nothing while every
            // word is full and rescans (`MAX_EMPTY_PASSES`); the model
            // waits for a clear bit instead of spinning.
            Micro::Refill(_, s) if Refill::at(s) == Refill::Finish => {
                let p = th.picks[th.cur as usize];
                p.zero && self.v.prefix_claim || sh.slabs[p.slab as usize].bits != FULL
            }
            _ => true,
        }
    }

    /// How many ways thread `t`'s next step can go: [`NCACHES`] at a
    /// section entry in the per-CPU mode (the cid the kernel put the
    /// thread on), one otherwise.
    pub(super) fn choices(&self, n: &Node, t: usize) -> u8 {
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
    pub(super) fn step(&self, n: &mut Node, t: usize, choice: u8) -> Result<String, String> {
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
            Micro::Purge(sub) => self.step_purge(sh, th, tid, t, sub),
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

    /// `ClassState::purge_empty`: the steps of `Purge`.
    pub(super) fn step_purge(&self, sh: &mut Shared, th: &mut Thread, tid: u8, t: usize, sub: u8) -> Result<String, String> {
        match Purge::at(sub) {
            Purge::Lock => {
                // purge_empty: `let mut g = self.inner.lock();`
                let a = self.purge_arena(th, t);
                th.arena = a as u8;
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

    /// `ClassState::claim`: the steps of `Refill`, at sub-step base
    /// `micro_base(th.sub)` (0 for `Op::Refill`, `POP_REFILL` inside a
    /// `Pop`).
    pub(super) fn step_refill(
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
        match Refill::at(sub) {
            Refill::Lock => {
                // plan_locked: `let mut g = self.inner.lock();`
                sh.lock[a] = Some(tid);
                th.sub = base + Refill::PlanLocked as u8;
                Ok(format!("lock arena {a}"))
            }
            Refill::PlanLocked => {
                // plan_locked under the lock: the partial walk, then
                // `next_slab` (empty, purged, the group cursor, the
                // handoff, a fresh group), `take` per slab.
                // `self.plan(meta, out.len() - got, &mut picks)`
                let desc = self.plan_locked(sh, th, want - th.got)?;
                th.sub = base + Refill::Unlock as u8;
                Ok(desc)
            }
            Refill::Unlock => {
                // `plan` returns: the lock hold ends.
                sh.lock[a] = None;
                th.cur = 0;
                if th.np == 0 {
                    self.micro_done(th, op);
                    return Ok(String::from("unlock; nothing to commit, claim returns 0"));
                }
                th.sub = base + Refill::Finish as u8;
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
                            th.sub = base + Refill::Lock as u8;
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
    pub(super) fn step_release(
        &self,
        sh: &mut Shared,
        th: &mut Thread,
        tid: u8,
        op: Op,
        sub: u8,
    ) -> Result<String, String> {
        let base = micro_base(th.sub);
        match Release::at(sub) {
            Release::Group => {
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
                th.sub = base + Release::FetchAnd as u8;
                Ok(format!("release: slabs {todo:#04b}"))
            }
            Release::FetchAnd => {
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
                th.sub = base + Release::SubLive as u8;
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
                th.sub = base + if th.rel_zero { Release::LockLiveZero } else { Release::NextSlab } as u8;
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
                th.sub = base + Release::OnLiveZero as u8;
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
                th.sub = base + Release::NextSlab as u8;
                Ok(desc + "; unlock")
            }
            Release::NextSlab => {
                // The next slab of the batch while it is the same owner's
                // (`release_owned` runs on); else `self.reopen(&mut reopen)`
                // for the run just done, then `release_routed` moves to
                // the next owner.
                let next_owner = lowest_set(th.rel_todo).map(|s| sh.owner(self.sc, s));
                if th.was_full != 0 && next_owner != Some(th.rel_arena) {
                    th.sub = base + Release::LockReopen as u8;
                    return Ok(format!(
                        "release: run of arena {} done, reopen pending",
                        th.rel_arena
                    ));
                }
                if th.rel_todo != 0 {
                    th.sub = base + Release::FetchAnd as u8;
                    return Ok(String::from("release: next slab"));
                }
                self.micro_done(th, op);
                Ok(String::from("release: batch done"))
            }
            Release::LockReopen => {
                // reopen: `let mut g = self.inner.lock();`
                let a = th.rel_arena as usize;
                sh.lock[a] = Some(tid);
                th.sub = base + Release::Reopen as u8;
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
                    th.sub = base + Release::FetchAnd as u8;
                    return Ok(desc + " unlock; next owner's slabs");
                }
                self.micro_done(th, op);
                Ok(desc + " unlock")
            }
        }
    }

    /// `plan_locked` and `take`, one lock hold. Up to `MAX_PICKS` picks.
    pub(super) fn plan_locked(&self, sh: &mut Shared, th: &mut Thread, want: u8) -> Result<String, String> {
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
    /// the next slab of the current group (committing it), else a group
    /// adopted from another arena (with `group_handoff`), else the next
    /// fresh group of the class. `None` when the class is out of slabs.
    pub(super) fn next_slab(&self, sh: &mut Shared, th: &mut Thread) -> Result<Option<(u8, String)>, String> {
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
    pub(super) fn adopt_idle_group(&self, sh: &mut Shared, th: &mut Thread) -> Result<Option<String>, String> {
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
    pub(super) fn check(&self, n: &Node) -> Result<(), String> {
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
