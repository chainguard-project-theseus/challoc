//! The per-CPU mode (module docs in `mod.rs`, "The per-CPU mode"): the
//! micro-machines the composite `Pop` and `Push` are made of, the cache
//! commit stores, and the rseq critical sections as the kernel defines
//! them: entry with the `mm_cid` read, the speculative reads, the abort
//! a lost cid or a preemption causes, and the commit store that ends the
//! section. The commits of the four sections (pop, the refill's push-back,
//! push, the flush) are here too; `steps.rs` calls them from `step`.

use super::*;

/// The micro-machine an `(operation, sub-step)` pair is in. The four
/// operations of the thread mode are their own machines; the per-CPU
/// `Pop` and `Push` compose their sections with `Refill` and `Release`
/// through the sub-step bases (`POP_REFILL` and the others).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Micro {
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

pub(super) fn micro(op: Op, sub: u8) -> Micro {
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
pub(super) fn micro_base(sub: u8) -> u8 {
    sub - sub % 10
}

impl Cache {
    /// The commit store of `len`. Entries above it are blanked (a stale
    /// `len` stored over a longer cache loses the entries above: the
    /// lost push) and the zero prefix never exceeds `len`.
    pub(super) fn set_len(&mut self, len: u8) {
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
    /// reaches it; a dirty entry below the prefix clips it: `heap/`,
    /// `if len < zeroed { zeroed = len }`), then `len = at + 1`.
    pub(super) fn push_at(&mut self, at: u8, h: Held) {
        self.slots[at as usize] = Held { zero: false, ..h };
        if h.zero && self.zeroed == at {
            self.zeroed = at + 1;
        } else if !h.zero && at < self.zeroed {
            self.zeroed = at;
        }
        self.set_len(at + 1);
    }
}

impl Model<'_> {
    /// The arena a `Purge` runs on: the thread's; in the per-CPU mode,
    /// where a thread's arena follows its cid from pop to pop, the one
    /// the scenario names for the thread (decay is an arena's work, not
    /// a CPU's).
    pub(super) fn purge_arena(&self, th: &Thread, t: usize) -> usize {
        if self.sc.percpu {
            self.sc.threads[t].0 as usize
        } else {
            th.arena as usize
        }
    }

    /// The end of a micro-machine: the operation ends, or the composite
    /// operation moves on (`Pop`: the refill done, push the batch back;
    /// the overflow released, done; `Push`: the flush released, retry).
    pub(super) fn micro_done(&self, th: &mut Thread, op: Op) {
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
    pub(super) fn section_start(&self, sub: u8) -> u8 {
        micro_base(sub) + u8::from(self.v.section == Section::CidOutside)
    }

    /// The cid a thread has read and not yet committed on: it is between
    /// step 0 (the `mm_cid` read) and the commit of a section. The steps
    /// outside sections (the refill, the release, the program's writes)
    /// do not care which CPU runs them.
    pub(super) fn outstanding(&self, th: &Thread, t: usize) -> Option<u8> {
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
    pub(super) fn enter_section(&self, ths: &mut [Thread], t: usize, c: u8, what: &str) -> String {
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
    pub(super) fn abort(&self, th: &mut Thread, at: u8) {
        th.clear_section();
        th.sub = at;
        if self.v.section == Section::Rseq {
            th.sec_cid = 0;
        }
        self.aborts.set(self.aborts.get() + 1);
    }

    /// Step 1 of a section: the speculative reads begin; under
    /// `CidOutside` this is where the section starts.
    pub(super) fn speculate(&self, th: &mut Thread) {
        if self.v.section == Section::CidOutside {
            th.in_section = true;
        }
    }

    /// The commit store ends the section. A commit by a thread that lost
    /// its cid on the way is what the kernel's abort rules out and
    /// `strict_cid` checks; the model records the first.
    pub(super) fn commit(sh: &mut Shared, th: &mut Thread, tid: u8) -> &'static str {
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

    /// The speculative reads are done. Under `Section::Rseq` the commit
    /// is the same step: the kernel lets no other thread run on this cid
    /// until the section ends (one that does aborts this one back to
    /// before its entry), so nothing can interleave between the reads and
    /// the store that the cache could observe; the section is atomic the
    /// way a lock hold is in the thread mode (module docs). The buggy
    /// variants keep the store a step of its own, which is where a stale
    /// read commits.
    pub(super) fn after_reads(
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
    pub(super) fn pop_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
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
    pub(super) fn pushback_commit(
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
    pub(super) fn push_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
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
    /// prefix is dropped by a flush (`heap/`, `zeroed = 0`).
    pub(super) fn flush_commit(&self, sh: &mut Shared, th: &mut Thread, t: usize) -> Result<String, String> {
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
}
