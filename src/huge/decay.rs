//! The huge allocator's purge walks (design 7, 8): the full purge
//! ([`Huge::purge_empty`]) and the decay walk ([`Huge::decay_with`]) over
//! the `Empty` list, the [`Tally`] that settles their batches, and the
//! dirty-byte and statistics reads under the huge lock.

use super::*;

verus! {

/// What a purge walk's flushes settled: extents made `Purged` and whether
/// a discard failed. It settles the walk's batch ([`Settle`], through
/// [`Huge::settle`]).
// Ghost fields that only the proofs read.
#[allow(dead_code)]
struct Tally {
    purged: usize,
    failed: bool,
    /// Ghost: the run-page authority's set when the walk started; no
    /// purge changes it.
    held: Ghost<vstd::set::Set<int>>,
    /// Ghost: the `Empty` extents the walk visited.
    vis: Ghost<vstd::set::Set<*mut HugeDesc>>,
    /// Ghost: the members of the `Empty` list left to visit.
    rest: Ghost<Seq<*mut HugeDesc>>,
    /// Ghost: the walk pushes every extent it visits (a full purge).
    full: Ghost<bool>,
    /// Ghost: the length of the `Empty` list when the walk started.
    e0: Ghost<int>,
    /// Ghost: the bytes reported discarded ([`Settle::bytes`]).
    bytes: Ghost<int>,
    /// Ghost: the extents of ranges reported not discarded.
    refused: Ghost<vstd::set::Set<*mut HugeDesc>>,
}

impl Settle for Tally {
    type Owner = Huge;

    type Ctx = Inner;

    /// `(held, vis, rest, full, e0)`.
    type Conf = (vstd::set::Set<int>, vstd::set::Set<*mut HugeDesc>, Seq<*mut HugeDesc>, bool, int);

    /// The walk's invariant over the lock value `c`, the pieces `pend`
    /// pushed and not settled: the lock value is the allocator's, its page
    /// authority's set the one the walk started with; each piece pending
    /// is a visited `Empty` extent's range ([`has_piece`]); the walk has
    /// visited the list up to `rest` ([`walked`]); the extents purged and the list
    /// sum to the list at the start; an extent was purged iff a byte was
    /// reported discarded; and a full walk has every visited member of the
    /// list pending or refused.
    closed spec fn inv(&self, o: &Huge, c: Inner, pend: spec_fn((int, int)) -> bool) -> bool {
        &&& o.is_process()
        &&& c.wf()
        &&& c.spec_np() == o.npages
        &&& c.spec_lg() == o.lg_page
        &&& c.spec_base() == o.base
        &&& c.spec_mid() == o.spec_mid()
        &&& c.spec_rid() == o.spec_run_loc()
        &&& c.spec_pid() == o.spec_page_loc()
        &&& c.pgs@@ == self.held@
        &&& forall|x: (int, int)| #[trigger] pend(x) ==> has_piece(o, c, self.vis@, x)
        &&& walked(c.elist@, self.rest@, self.vis@)
        &&& self.purged + c.elist@.len() == self.e0@
        &&& self.e0@ <= u32::MAX
        &&& self.bytes@ >= 0
        &&& (self.purged > 0) == (self.bytes@ > 0)
        &&& self.full@ ==> forall|i: int|
            0 <= i < c.elist@.len() - self.rest@.len() ==> #[trigger] pend(piece_of(o, c.descs@[c.elist@[i]].value()))
                || self.refused@.contains(c.elist@[i])
    }

    closed spec fn bytes(&self) -> int {
        self.bytes@
    }

    closed spec fn conf(&self) -> (vstd::set::Set<int>, vstd::set::Set<*mut HugeDesc>, Seq<*mut HugeDesc>, bool, int) {
        (self.held@, self.vis@, self.rest@, self.full@, self.e0@)
    }

    #[inline(always)]
    fn settle(
        &mut self,
        o: &Huge,
        c: &mut Inner,
        addr: usize,
        len: usize,
        done: bool,
        piece: Ghost<int>,
        pend: Ghost<spec_fn((int, int)) -> bool>,
    ) {
        o.settle(c, addr, len, done, self, piece, pend);
    }
}

impl Tally {
    /// Nothing visited of the list `l`, of `e0` members; the page
    /// authority's set `held`, `full` for a walk that pushes every extent
    /// it visits.
    fn new(
        Ghost(held): Ghost<vstd::set::Set<int>>,
        Ghost(l): Ghost<Seq<*mut HugeDesc>>,
        Ghost(full): Ghost<bool>,
        Ghost(e0): Ghost<int>,
    ) -> (t: Tally)
        ensures
            t.purged == 0,
            t.held@ == held,
            t.vis@ == vstd::set::Set::<*mut HugeDesc>::empty(),
            t.rest@ == l,
            t.full@ == full,
            t.e0@ == e0,
            t.bytes@ == 0,
            t.refused@ == vstd::set::Set::<*mut HugeDesc>::empty(),
    {
        Tally {
            purged: 0,
            failed: false,
            held: Ghost(held),
            vis: Ghost(vstd::set::Set::empty()),
            rest: Ghost(l),
            full: Ghost(full),
            e0: Ghost(e0),
            bytes: Ghost(0),
            refused: Ghost(vstd::set::Set::empty()),
        }
    }

    /// The first member left is a live `Empty` extent, at its place in
    /// the list.
    proof fn lemma_rest_live(&self, h: &Huge, g: Inner, pend: spec_fn((int, int)) -> bool)
        requires
            self.inv(h, g, pend),
        ensures
            self.rest@.len() > 0 ==> {
                let d = self.rest@[0];
                &&& g.descs@.contains_key(d)
                &&& d@.addr != 0
                &&& g.elist@.contains(d)
                &&& g.elist@[g.elist@.len() - self.rest@.len()] == d
                &&& g.descs@[d].value().state == RunState::Empty as u8
                &&& !self.vis@.contains(d)
                &&& g.descs@[d].value().next == (if self.rest@.len() > 1 {
                    self.rest@[1]
                } else {
                    ptr::null_mut::<HugeDesc>()
                })
            },
            self.purged <= u32::MAX,
    {
        if self.rest@.len() > 0 {
            let l = g.elist@;
            let k = l.len() - self.rest@.len();
            assert(l[k] == l.subrange(k, l.len() as int)[0]);
            if self.rest@.len() > 1 {
                assert(l[k + 1] == l.subrange(k, l.len() as int)[1]);
            }
            assert(enode(l, g.descs@, k));
            assert(!self.vis@.contains(self.rest@[0]));
            assert(perms_ok(g.descs@));
        }
    }

    /// Once nothing is pending and nothing is left to visit, every
    /// `Empty` extent of a full walk was refused.
    proof fn lemma_walked_all(&self, h: &Huge, g: Inner, x: *mut HugeDesc)
        requires
            self.inv(h, g, purge::none()),
            self.full@,
            self.rest@.len() == 0,
            g.descs@.contains_key(x),
            g.descs@[x].value().state == RunState::Empty as u8,
        ensures
            self.refused@.contains(x),
    {
        assert(g.elist@.contains(x));
        let i = g.elist@.index_of(x);
        assert(0 <= i < g.elist@.len() - self.rest@.len());
        assert(!(purge::none())(piece_of(h, g.descs@[g.elist@[i]].value())));
    }

    /// The count purged is at most `u32::MAX`.
    proof fn lemma_counts(&self, h: &Huge, g: Inner)
        requires
            self.inv(h, g, purge::none()),
        ensures
            self.purged <= u32::MAX,
    {
    }
}

/// The range a huge purge pushes for extent `v` of `h`: its pages.
spec fn piece_of(h: &Huge, v: HugeDesc) -> (int, int) {
    (h.spec_base() as int + v.start * h.spec_page(), v.pages * h.spec_page())
}

/// `x` is the range of an `Empty` extent of `g` that a walk visited (in
/// `vis`): what a huge purge's batch holds pending. Opaque, so a
/// settler's invariant carries it per piece as one fact.
#[verifier::opaque]
spec fn has_piece(h: &Huge, g: Inner, vis: vstd::set::Set<*mut HugeDesc>, x: (int, int)) -> bool {
    exists|d: *mut HugeDesc|
        #[trigger] g.descs@.contains_key(d) && g.descs@[d].value().state == RunState::Empty as u8 && vis.contains(d)
            && piece_of(h, g.descs@[d].value()) == x
}

/// A walk of the `Empty` list `l` that has visited (`vis`) its members up
/// to `rest`, the members left: `l` ends with `rest`, every member before
/// is visited, and none of `rest` is.
spec fn walked(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>) -> bool {
    &&& rest.len() <= l.len()
    &&& l.subrange(l.len() - rest.len(), l.len() as int) == rest
    &&& forall|i: int| 0 <= i < l.len() - rest.len() ==> vis.contains(#[trigger] l[i])
    &&& forall|j: int| 0 <= j < rest.len() ==> !vis.contains(#[trigger] rest[j])
}

/// A visited member's removal (a settled extent's unlink) keeps the walk.
proof fn lemma_walked_remove(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>, i: int)
    requires
        walked(l, rest, vis),
        0 <= i < l.len(),
        vis.contains(l[i]),
    ensures
        walked(l.remove(i), rest, vis),
        i < l.len() - rest.len(),
{
    let k = l.len() - rest.len();
    if i >= k {
        assert(l[i] == l.subrange(k, l.len() as int)[i - k]);
    }
    let m = l.remove(i);
    assert(m.subrange(m.len() - rest.len(), m.len() as int) =~= rest) by {
        assert forall|j: int| 0 <= j < rest.len() implies m.subrange(m.len() - rest.len(), m.len() as int)[j] == rest[j] by {
            assert(m[k - 1 + j] == l[k + j]);
            assert(l[k + j] == l.subrange(k, l.len() as int)[j]);
        }
    }
    assert forall|x: int| 0 <= x < m.len() - rest.len() implies vis.contains(#[trigger] m[x]) by {
        if x < i {
            assert(m[x] == l[x]);
        } else {
            assert(m[x] == l[x + 1]);
        }
    }
}

/// Visiting the first member left keeps the walk.
proof fn lemma_walked_visit(l: Seq<*mut HugeDesc>, rest: Seq<*mut HugeDesc>, vis: vstd::set::Set<*mut HugeDesc>)
    requires
        walked(l, rest, vis),
        rest.len() > 0,
        l.no_duplicates(),
    ensures
        walked(l, rest.drop_first(), vis.insert(rest[0])),
        l[l.len() - rest.len()] == rest[0],
{
    let k = l.len() - rest.len();
    let r1 = rest.drop_first();
    assert(l[k] == l.subrange(k, l.len() as int)[0]);
    assert(l.subrange(l.len() - r1.len(), l.len() as int) =~= r1) by {
        assert forall|j: int| 0 <= j < r1.len() implies l.subrange(l.len() - r1.len(), l.len() as int)[j] == r1[j] by {
            assert(l[k + 1 + j] == l.subrange(k, l.len() as int)[j + 1]);
        }
    }
    assert forall|x: int| 0 <= x < l.len() - r1.len() implies vis.insert(rest[0]).contains(#[trigger] l[x]) by {
        if x < k {
            assert(vis.contains(l[x]));
        }
    }
    assert forall|j: int| 0 <= j < r1.len() implies !vis.insert(rest[0]).contains(#[trigger] r1[j]) by {
        assert(r1[j] == rest[j + 1]);
        assert(l[k + 1 + j] == l.subrange(k, l.len() as int)[j + 1]);
        assert(l[k] != l[k + 1 + j]);
    }
}

impl Huge {
    /// Purge every `Empty` extent whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all): `MADV_DONTNEED`, then `Purged`; an extent
    /// whose discard fails stays `Empty` in its place (the next purge
    /// retries it). Returns the number purged, at most `u32::MAX`.
    /// `left` is ghost: what the purge left (`purge::Left`).
    pub fn purge_empty(
        &self,
        cutoff: u64,
        Tracked(left): Tracked<&mut Ghost<purge::Left>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: usize)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r <= u32::MAX,
            final(left)@.bytes >= 0,
            (r > 0) == (final(left)@.bytes > 0),
            final(left)@.arenas == vstd::set::Set::<int>::empty(),
            final(left)@.classes == vstd::set::Set::<(int, int)>::empty(),
            final(left)@.slabs == ISet::<(int, int, int)>::empty(),
            final(left)@.huge,
            cutoff == u64::MAX ==> final(left)@.clean(),
    {
        proof {
            use_type_invariant(self);
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g@.lemma_elist();
        }
        // The batch may discard pages of no live run: the run authority
        // does not change while the lock is held here.
        let ghost held = g@.pgs@@;
        let ghost full = cutoff == u64::MAX;
        let mut t = Tally::new(Ghost(held), Ghost(g@.elist@), Ghost(full), Ghost(g@.elist@.len() as int));
        let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
        let mut d = g.empty_head;
        proof {
            reveal(has_piece);
            assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
            g@.lemma_head();
            assert(t.inv(self, g@, batch.pieces()));
            t.lemma_rest_live(self, g@, batch.pieces());
        }
        while !d.is_null()
            invariant
                g.spec_lock() == self.inner,
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                crate::sys::may_syscall(),
                self.is_process(),
                batch.wf(),
                forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                batch.sent() == t.bytes@,
                t.inv(self, g@, batch.pieces()),
                t.held@ == held,
                t.full@ == (cutoff == u64::MAX),
                d == (if t.rest@.len() > 0 { t.rest@[0] } else { ptr::null_mut::<HugeDesc>() }),
                t.rest@.len() > 0 ==> d@.addr != 0,
            ensures
                cutoff == u64::MAX ==> t.rest@.len() == 0,
            decreases t.rest@.len(),
        {
            proof {
                if t.rest@.len() == 0 {
                    assert(d@.addr == 0);
                }
                t.lemma_rest_live(self, g@, batch.pieces());
            }
            // SAFETY: list members are live `Empty` extents; `next` is
            // read before a flush can unlink `d`.
            let (dirty_at, next) = (g.desc(d).dirty_at, g.desc(d).next);
            if dirty_at > cutoff {
                break;
            }
            self.push_extent(&mut g, d, &mut batch, &mut t);
            proof {
                t.lemma_rest_live(self, g@, batch.pieces());
            }
            d = next;
        }
        batch.flush(&mut t, self, &mut g);
        self.sync_oldest(&g);
        proof {
            let lv = g@;
            let ext = ISet::new(
                |a: int|
                    exists|x: *mut HugeDesc| #[trigger]
                        lv.descs@.contains_key(x) && x@.addr == a && lv.descs@[x].value().state == RunState::Empty as u8,
            );
            let refused = ISet::new(
                |a: int| exists|x: *mut HugeDesc| #[trigger] t.refused@.contains(x) && x@.addr == a,
            );
            *left = Ghost(
                purge::Left {
                    bytes: batch.sent(),
                    arenas: vstd::set::Set::empty(),
                    classes: vstd::set::Set::empty(),
                    slabs: ISet::empty(),
                    huge: true,
                    extents: ext,
                    refused: ISet::empty(),
                    refused_extents: refused,
                },
            );
            t.lemma_counts(self, lv);
            if cutoff == u64::MAX {
                // Every `Empty` extent is on the list, every member of
                // which the walk visited; none is pending once the batch
                // is flushed, so each was refused.
                assert forall|a: int| #[trigger] ext.contains(a) implies refused.contains(a) by {
                    let x = choose|x: *mut HugeDesc| #[trigger]
                        lv.descs@.contains_key(x) && x@.addr == a && lv.descs@[x].value().state == RunState::Empty as u8;
                    t.lemma_walked_all(self, lv, x);
                    assert(t.refused@.contains(x));
                }
            }
        }
        proof {
            // The lock's frame: the ghost page count, shift, base and `Meta`
            // identity are the ones `new` set.
            assert(lock_frame(lock_init(self.inner), g@));
        }
        let r = t.purged;
        g.unlock();
        r
    }

    /// Push `d`, the first member of the `Empty` list a walk has left
    /// (`t.rest`), to `batch`; it stays there, `Empty`, until the flush
    /// reports it discarded ([`settle`]). The batch is flushed before the
    /// lock is released, so no allocation can take the extent between the
    /// discard and the state change. Returns its pages.
    ///
    /// [`settle`]: Huge::settle
    fn push_extent(&self, g: &mut Inner, d: *mut HugeDesc, batch: &mut Batch, t: &mut Tally) -> (r: usize)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(t).inv(self, *old(g), old(batch).pieces()),
            old(batch).wf(),
            forall|b: int| #[trigger] (old(batch).free())(b) == self.purgeable(old(t).held@, b),
            old(t).rest@.len() > 0,
            d == old(t).rest@[0],
        ensures
            final(t).inv(self, *final(g), final(batch).pieces()),
            final(batch).wf(),
            final(batch).free() == old(batch).free(),
            final(batch).sent() - old(batch).sent() == final(t).bytes() - old(t).bytes(),
            final(t).held == old(t).held,
            final(t).vis@ == old(t).vis@.insert(d),
            final(t).rest@ == old(t).rest@.drop_first(),
            final(t).full == old(t).full,
            r == old(g).descs@[d].value().pages,
    {
        let ghost d0 = g.descs@;
        let ghost l0 = g.elist@;
        let ghost t0 = *t;
        proof {
            use_type_invariant(self);
            self.lemma_page();
            t.lemma_rest_live(self, *g, batch.pieces());
            let v = d0[d].value();
            let p = self.spec_page();
            assert(desc_ok(v, g.next_page));
            self.lemma_pages((v.start + v.pages) as usize);
            self.lemma_pages(v.start as usize);
            assert((v.start + v.pages) * p == v.start * p + v.pages * p) by (nonlinear_arith);
        }
        // SAFETY: list members are live `Empty` extents.
        let (start, pages) = (g.desc(d).start(), g.desc(d).pages());
        let ghost new = piece_of(self, d0[d].value());
        // The extent's pages are no live run's: the page authority holds
        // the `Active` extents' pages, and those are apart from `d`.
        proof {
            let v = d0[d].value();
            let p = self.spec_page();
            let base = self.base as int;
            lemma_run_auths(*g);
            lemma_active(d0);
            assert forall|b: int| base + v.start * p <= b < base + v.start * p + v.pages * p
                implies #[trigger] (batch.free())(b) by {
                crate::purge::lemma_unit_of(base, p, v.start as int, v.pages as int, b);
                let k = (b - base) / p;
                if g.pgs@@.contains(k) {
                    let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
                    assert(x != d);
                    assert(apart(v, d0[x].value()));
                }
                assert(self.purgeable(g.pgs@@, b));
            }
            // Page-aligned extent pages inside the huge region.
            self.lemma_purge_range(v.start as int, v.pages as int);
            // Apart from the extents pending, each another visited one.
            reveal(has_piece);
            assert forall|x: (int, int)| #[trigger] (batch.pieces())(x) implies x.0 + x.1 <= new.0 || new.0 + new.1
                <= x.0 by {
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g.descs@.contains_key(y) && g.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g.descs@[y].value()) == x;
                assert(y != d);
                let w = d0[y].value();
                assert(apart(v, w));
                if v.start + v.pages <= w.start {
                    assert(v.start * p + v.pages * p <= w.start * p) by (nonlinear_arith)
                        requires v.start + v.pages <= w.start, p > 0;
                } else {
                    assert(w.start * p + w.pages * p <= v.start * p) by (nonlinear_arith)
                        requires w.start + w.pages <= v.start, p > 0;
                }
            }
            lemma_walked_visit(l0, t0.rest@, t0.vis@);
            assert(l0.no_duplicates());
        }
        t.vis = Ghost(t.vis@.insert(d));
        t.rest = Ghost(t.rest@.drop_first());
        proof {
            reveal(has_piece);
            let pend = purge::either(batch.pieces(), purge::one(new));
            assert forall|x: (int, int)| #[trigger] pend(x) implies has_piece(self, *g, t.vis@, x) by {
                if x == new {
                    assert(g.descs@.contains_key(d));
                } else {
                    let y = choose|y: *mut HugeDesc|
                        #[trigger] g.descs@.contains_key(y) && g.descs@[y].value().state == RunState::Empty as u8
                            && t0.vis@.contains(y) && piece_of(self, g.descs@[y].value()) == x;
                    assert(t.vis@.contains(y));
                }
            }
            if t.full@ {
                let l = g.elist@;
                let k = l.len() - t0.rest@.len();
                assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] pend(
                    piece_of(self, g.descs@[l[i]].value()),
                ) || t.refused@.contains(l[i]) by {
                    if i == k {
                        assert(l[i] == d);
                    }
                }
            }
            assert(t.inv(self, *g, pend));
        }
        batch.push(self.addr(start), pages * self.page_size(), t, self, g);
        pages
    }

    /// Make `d`, a member of the `Empty` list, `Purged`: out of the list,
    /// with no age, its pages counted as purged. Returns its pages.
    #[inline(always)]
    fn purge_desc(&self, g: &mut Inner, d: *mut HugeDesc) -> (r: usize)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            old(g).wf(),
            old(g).elist@.contains(d),
        ensures
            final(g).wf(),
            final(g).spec_np() == old(g).spec_np(),
            final(g).spec_lg() == old(g).spec_lg(),
            final(g).spec_base() == old(g).spec_base(),
            final(g).spec_mid() == old(g).spec_mid(),
            final(g).spec_rid() == old(g).spec_rid(),
            final(g).next_page == old(g).next_page,
            final(g).pgs == old(g).pgs,
            r == old(g).descs@[d].value().pages,
            final(g).elist@ == old(g).elist@.remove(old(g).elist@.index_of(d)),
            final(g).descs@.dom() == old(g).descs@.dom(),
            forall|x: *mut HugeDesc| #[trigger] old(g).descs@.contains_key(x) ==> final(g).descs@[x].value().pages
                == old(g).descs@[x].value().pages && final(g).descs@[x].value().start == old(g).descs@[x].value().start
                && (x != d ==> final(g).descs@[x].value().state == old(g).descs@[x].value().state),
            final(g).descs@[d].value().state == RunState::Purged as u8,
    {
        let ghost j = g.elist@.index_of(d);
        let ghost d0 = g.descs@;
        let ghost l0 = g.elist@;
        proof {
            g.lemma_elist();
            assert(enode(l0, d0, j));
        }
        // SAFETY: list members are live `Empty` extents.
        let pages = g.desc(d).pages();
        g.unlink(d);
        let ghost d1 = g.descs@;
        g.desc_mut(d).state = RunState::Purged as u8;
        g.desc_mut(d).dirty_at = 0;
        proof {
            // `d` is on the list, so the list's count and pages hold it.
            lemma_epages_remove(l0, d0, j);
            lemma_epages_nonneg(l0.remove(j), d0);
            assert(g.stats == old(g).stats);
        }
        g.stats.sub(RunState::Empty, pages);
        g.stats.add(RunState::Purged, pages);
        proof {
            let l1 = g.elist@;
            assert(!l1.contains(d)) by {
                assert(l0.no_duplicates());
                if l1.contains(d) {
                    let i = l1.index_of(d);
                    if i < j {
                        assert(l1[i] == l0[i]);
                    } else {
                        assert(l1[i] == l0[i + 1]);
                    }
                }
            }
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] enode(l1, g.descs@, i) by {
                assert(enode(l1, d1, i));
                assert(l1[i] != d);
            }
            assert(g.descs@.dom() =~= d0.dom());
            lemma_epages_remove(l0, d0, j);
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] g.descs@[l1[i]].value().pages
                == d0[l1[i]].value().pages by {
                assert(same_but_links(d0[l1[i]].value(), d1[l1[i]].value())) by {
                    assert(enode(l1, d1, i));
                }
            }
            lemma_epages_frame(l1, d0, g.descs@);
            assert forall|x: *mut HugeDesc| #[trigger] g.descs@.contains_key(x) implies desc_ok(
                g.descs@[x].value(),
                g.next_page,
            ) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies g.descs@[x].value().pages
                == d0[x].value().pages by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            // The extents are where they were; `d` changed state only.
            let d2 = g.descs@;
            // The run authorities are untouched: `d` was `Empty`, and the
            // `Active` extents are as they were.
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state == RunState::Active as u8 implies {
                &&& d2.contains_key(x)
                &&& d2[x].value().state == RunState::Active as u8
                &&& d2[x].value().start == d0[x].value().start
                &&& d2[x].value().pages == d0[x].value().pages
            } by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state == RunState::Active as u8 implies {
                &&& d0.contains_key(x)
                &&& d0[x].value().state == RunState::Active as u8
            } by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            lemma_active_frame(d0, d2);
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) implies keyed(g.table, d2[x].value(), x) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|k: usize| #[trigger] g.table.lookup(k) is Some implies is_key(
                d2[g.table.lookup(k).unwrap()].value(),
                k as int,
            ) by {
                let x = g.table.lookup(k).unwrap();
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
            assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
                #[trigger] d2.contains_key(x1) && #[trigger] d2.contains_key(x2) && x1 != x2 implies apart(
                    d2[x1].value(),
                    d2[x2].value(),
                ) by {
                assert(same_but_links(d0[x1].value(), d1[x1].value()));
                assert(same_but_links(d0[x2].value(), d1[x2].value()));
            }
            // Free extents stay free (`d` turns from `Empty` to `Purged`),
            // where they were.
            assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
                #[trigger] d2.contains_key(x1) && #[trigger] d2.contains_key(x2) && d2[x1].value().state != RunState::Active as u8
                    && d2[x2].value().state != RunState::Active as u8 implies d2[x1].value().start + d2[x1].value().pages
                    != d2[x2].value().start by {
                assert(same_but_links(d0[x1].value(), d1[x1].value()));
                assert(same_but_links(d0[x2].value(), d1[x2].value()));
                assert(d0.contains_key(x1) && d0.contains_key(x2));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state != RunState::Active as u8
                implies marked(g.free.map.view(), d2[x].value()) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(d0.contains_key(x));
            }
            assert forall|q: nat| #[trigger] g.free.map.view().contains(q) implies exists|x: *mut HugeDesc| #[trigger] in_free(
                d2,
                x,
                q as int,
            ) by {
                let x = choose|x: *mut HugeDesc| #[trigger] in_free(d0, x, q as int);
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(in_free(d2, x, q as int));
            }
            assert forall|x: *mut HugeDesc| #[trigger] d2.contains_key(x) && d2[x].value().state == RunState::Empty as u8
                implies l1.contains(x) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(l0.contains(x));
                let i = l0.index_of(x);
                assert(i != j);
                if i < j {
                    assert(l1[i] == x);
                } else {
                    assert(l1[i - 1] == x);
                }
            }
            let all = g.all@;
            assert(all.contains(d));
            assert forall|k: int| 0 <= k < 7 implies #[trigger] tally(all, d2, k) == tally(all, d0, k) - weight(d0[d].value(), k)
                + weight(d2[d].value(), k) by {
                assert forall|i: int| 0 <= i < all.len() implies #[trigger] weight(d1[all[i]].value(), k) == weight(
                    d0[all[i]].value(),
                    k,
                ) by {
                    assert(d0.contains_key(all[i]));
                    assert(same_but_links(d0[all[i]].value(), d1[all[i]].value()));
                }
                assert(same_but_links(d0[d].value(), d1[d].value()));
                lemma_tally_frame(all, d0, d1, k);
                lemma_tally_update(all, d1, d2, d, k);
            }
            assert(tally(all, d2, 6) == tally(all, d0, 6));
            assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g.stats, k) == tally(all, d2, k) by {
                assert(tally(all, d2, k) == tally(all, d0, k) - weight(d0[d].value(), k) + weight(d2[d].value(), k));
                assert(stat(old(g).stats, k) == tally(all, d0, k));
            }
        }
        proof {
            assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) implies g.descs@[x].value().pages
                == d0[x].value().pages && g.descs@[x].value().start == d0[x].value().start && (x != d
                ==> g.descs@[x].value().state == d0[x].value().state) by {
                assert(same_but_links(d0[x].value(), d1[x].value()));
            }
        }
        pages
    }

    /// The flush's report on `[addr, addr + len)`, extents pushed by
    /// [`push_extent`] (under the huge lock since, so still in the `Empty`
    /// list): discarded, each leaves the list and becomes `Purged`; not,
    /// they stay as they are. Free extents never touch, so a range is one
    /// extent: the check that it starts an `Empty` extent never fails.
    ///
    /// [`push_extent`]: Huge::push_extent
    #[allow(clippy::too_many_arguments)]
    fn settle(
        &self,
        g: &mut Inner,
        addr: usize,
        len: usize,
        done: bool,
        t: &mut Tally,
        Ghost(piece): Ghost<int>,
        Ghost(pend): Ghost<spec_fn((int, int)) -> bool>,
    )
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            old(t).inv(self, *old(g), pend),
            0 < piece <= len,
            len as int % piece == 0,
            addr + len <= usize::MAX,
            forall|x: (int, int)| #[trigger] purge::in_run(addr as int, len as int, piece, x) ==> pend(x),
        ensures
            final(t).inv(self, *final(g), purge::minus(pend, addr as int, len as int, piece)),
            final(t).conf() == old(t).conf(),
            final(t).bytes() == old(t).bytes() + if done {
                len as int
            } else {
                0
            },
    {
        let ghost g0 = *g;
        let ghost t0 = *t;
        let ghost p = self.spec_page();
        let ghost base = self.base as int;
        proof {
            use_type_invariant(self);
            self.lemma_page();
            reveal(has_piece);
            vstd::arithmetic::div_mod::lemma_small_mod(0, piece as nat);
            assert(purge::in_run(addr as int, len as int, piece, (addr as int, piece)));
            assert(pend((addr as int, piece)));
        }
        // The extent pushed for the range's first piece.
        let ghost dd = choose|y: *mut HugeDesc|
            #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == (addr as int, piece);
        let ghost v = g0.descs@[dd].value();
        proof {
            assert(desc_ok(v, g0.next_page));
            // One piece: a second would be another `Empty` extent starting
            // where this one ends, and free extents never touch.
            if piece < len {
                vstd::arithmetic::div_mod::lemma_mod_self_0(piece);
                assert(purge::in_run(addr as int, len as int, piece, (addr + piece, piece)));
                assert(pend((addr + piece, piece)));
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == (addr + piece, piece);
                let w = g0.descs@[y].value();
                assert(w.start * p == v.start * p + v.pages * p);
                assert(w.start == v.start + v.pages) by (nonlinear_arith)
                    requires w.start * p == v.start * p + v.pages * p, p > 0;
                g0.theorem_coalesced(dd, y);
            }
            assert(len == piece);
            assert(g0.elist@.contains(dd));
            assert(keyed(g0.table, v, dd));
        }
        if !done {
            t.failed = true;
            proof {
                t.refused@ = t.refused@.insert(dd);
                let rest = purge::minus(pend, addr as int, len as int, piece);
                assert forall|x: (int, int)| #[trigger] rest(x) implies has_piece(self, *g, t.vis@, x) by {
                    assert(pend(x));
                }
                if t.full@ {
                    let l = g.elist@;
                    assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] rest(
                        piece_of(self, g.descs@[l[i]].value()),
                    ) || t.refused@.contains(l[i]) by {
                        assert(pend(piece_of(self, g.descs@[l[i]].value())) || t0.refused@.contains(l[i]));
                        if l[i] != dd && purge::in_run(addr as int, len as int, piece, piece_of(self, g.descs@[l[i]].value())) {
                            purge::lemma_run_one(addr as int, len as int, piece_of(self, g.descs@[l[i]].value()));
                            assert(enode(l, g.descs@, i));
                            let w = g.descs@[l[i]].value();
                            assert(w.start * p == v.start * p);
                            assert(w.start == v.start) by (nonlinear_arith)
                                requires w.start * p == v.start * p, p > 0;
                            assert(apart(v, w));
                        }
                    }
                }
                assert(t.inv(self, *g, rest));
            }
            return;
        }
        let mut page = (addr - self.base) >> self.lg_page;
        proof {
            vstd::bits::lemma_usize_shr_is_div((addr - self.base) as usize, self.lg_page as usize);
            vstd::bits::lemma_usize_shr_is_div(len, self.lg_page as usize);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(v.start as int, p);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(v.pages as int, p);
            assert(p * v.start == v.start * p && p * v.pages == v.pages * p) by (nonlinear_arith);
            assert(page == v.start);
            assert(len >> self.lg_page == v.pages);
            assert(v.start + v.pages <= g0.next_page <= u32::MAX);
        }
        let end = page + (len >> self.lg_page);
        let ghost mut once = false;
        while page < end
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                end == v.start + v.pages,
                v == g0.descs@[dd].value(),
                v.pages >= 1,
                g0.wf(),
                g0.descs@.contains_key(dd),
                g0.elist@.contains(dd),
                keyed(g0.table, v, dd),
                v.state == RunState::Empty as u8,
                t0.purged + g0.elist@.len() == t0.e0@,
                t0.e0@ <= u32::MAX,
                !once ==> page == v.start && *g == g0 && *t == t0,
                once ==> page == end,
                once ==> {
                    &&& g.wf()
                    &&& g.spec_np() == g0.spec_np()
                    &&& g.spec_lg() == g0.spec_lg()
                    &&& g.spec_base() == g0.spec_base()
                    &&& g.spec_mid() == g0.spec_mid()
                    &&& g.spec_rid() == g0.spec_rid()
                    &&& g.pgs == g0.pgs
                    &&& g.elist@ == g0.elist@.remove(g0.elist@.index_of(dd))
                    &&& g.descs@.dom() == g0.descs@.dom()
                    &&& forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) ==> g.descs@[x].value().pages
                        == g0.descs@[x].value().pages && g.descs@[x].value().start == g0.descs@[x].value().start
                        && (x != dd ==> g.descs@[x].value().state == g0.descs@[x].value().state)
                    &&& g.descs@[dd].value().state == RunState::Purged as u8
                    &&& t.purged == t0.purged + 1
                    &&& t.conf() == t0.conf()
                    &&& t.bytes == t0.bytes
                    &&& t.refused == t0.refused
                },
            decreases end - page,
        {
            let d = g.table.get(page);
            // SAFETY: table entries are live descriptors.
            if d.is_null() || g.desc(d).start() != page || g.desc(d).state() != RunState::Empty {
                proof {
                    assert(false);
                }
                sys::abort_fmt(format_args!("huge: purged range {addr:#x}+{len:#x} is not Empty extents"));
            }
            proof {
                assert(d == dd);
                assert(t.purged + g.elist@.len() == t.e0@);
                g.lemma_elist();
            }
            let pages = self.purge_desc(g, d);
            t.purged += 1;
            page += pages;
            proof {
                once = true;
            }
        }
        t.bytes = Ghost(t.bytes@ + len);
        proof {
            assert(once);
            let l0 = g0.elist@;
            let i0 = l0.index_of(dd);
            lemma_walked_remove(l0, t0.rest@, t0.vis@, i0);
            let rest = purge::minus(pend, addr as int, len as int, piece);
            assert forall|x: (int, int)| #[trigger] rest(x) implies has_piece(self, *g, t.vis@, x) by {
                assert(pend(x));
                let y = choose|y: *mut HugeDesc|
                    #[trigger] g0.descs@.contains_key(y) && g0.descs@[y].value().state == RunState::Empty as u8
                        && t0.vis@.contains(y) && piece_of(self, g0.descs@[y].value()) == x;
                purge::lemma_run_one(addr as int, len as int, x);
                assert(y != dd);
                assert(g.descs@.contains_key(y));
            }
            if t.full@ {
                let l = g.elist@;
                assert forall|i: int| 0 <= i < l.len() - t.rest@.len() implies #[trigger] rest(
                    piece_of(self, g.descs@[l[i]].value()),
                ) || t.refused@.contains(l[i]) by {
                    let i1 = if i < i0 { i } else { i + 1 };
                    assert(l[i] == l0[i1]);
                    assert(pend(piece_of(self, g0.descs@[l0[i1]].value())) || t0.refused@.contains(l0[i1]));
                    assert(l0.no_duplicates());
                    assert(l[i] != dd);
                    assert(enode(l0, g0.descs@, i1));
                    let w = g0.descs@[l[i]].value();
                    purge::lemma_run_one(addr as int, len as int, piece_of(self, w));
                    if piece_of(self, w) == (addr as int, len as int) {
                        assert(w.start * p == v.start * p);
                        assert(w.start == v.start) by (nonlinear_arith)
                            requires w.start * p == v.start * p, p > 0;
                        assert(apart(v, w));
                    }
                }
            }
            assert(t.inv(self, *g, rest));
        }
    }

    /// Decay walk (design 8), as `ClassState::purge_curve` in
    /// pages: the curve's allowance at `now` is the sum over the `Empty`
    /// extents of their pages times the weight of their age, rounded up,
    /// and the excess over it is purged oldest extent first, whole extents
    /// only and never below the allowance (an extent larger than the
    /// remaining excess is skipped for younger ones that fit), so a lone
    /// extent lasts until its weight is zero, the end of the window, as
    /// before. A failed discard leaves its extent `Empty` and backs the
    /// gate off (`Decayer::back_off`). Returns the number of extents
    /// purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            use_type_invariant(self);
            self.lemma_page();
            g@.lemma_elist();
        }
        let ghost l = g@.elist@;
        let ghost len = l.len() as int;
        let ghost mut k: int = 0;
        let mut total = 0usize;
        let mut weighted = 0u64;
        let mut d = g.empty_head;
        proof {
            assert(l.subrange(0, len) =~= l);
            g@.lemma_head();
        }
        while !d.is_null()
            invariant
                g.spec_lock() == self.inner,
                g@.wf(),
                l == g@.elist@,
                len == l.len(),
                0 <= k <= len,
                d == (if k < len { l[k] } else { ptr::null_mut::<HugeDesc>() }),
                total + epages(l.subrange(k, len), g@.descs@) == epages(l, g@.descs@),
                epages(l, g@.descs@) <= g@.next_page <= u32::MAX,
                forall|i: int| 0 <= i < len ==> #[trigger] g@.descs@.contains_key(l[i]),
                weighted <= total * 0xFFFF_FFFFu64,
            decreases len - k,
        {
            proof {
                if k >= len {
                    assert(d@.addr == 0);
                }
                assert(enode(l, g@.descs@, k));
                lemma_epages_step(l, g@.descs@, k);
                lemma_epages_nonneg(l.subrange(k + 1, len), g@.descs@);
            }
            // List members are live `Empty` extents.
            let (pages, dirty_at, next) = (g.desc(d).pages(), g.desc(d).dirty_at, g.desc(d).next);
            total += pages;
            proof {
                assert forall|w: u64| w <= 0xFFFF_FFFFu64 implies #[trigger] (pages as u64 * w) <= pages * 0xFFFF_FFFFu64 by {
                    assert(pages as u64 * w <= pages * 0xFFFF_FFFFu64) by (nonlinear_arith)
                        requires
                            w <= 0xFFFF_FFFFu64,
                    ;
                }
                assert(total * 0xFFFF_FFFFu64 <= 0xFFFF_FFFFu64 * 0xFFFF_FFFFu64) by (nonlinear_arith)
                    requires
                        total <= 0xFFFF_FFFFu64,
                ;
                assert(total * 0xFFFF_FFFFu64 == (total - pages) * 0xFFFF_FFFFu64 + pages * 0xFFFF_FFFFu64) by (nonlinear_arith);
            }
            weighted += pages as u64 * Curve::weight(curve.step(now, dirty_at));
            d = next;
            proof {
                k = k + 1;
            }
        }
        // The cgroup policy's retention cap (design 11), as in the class
        // walk: keep at most this walk's share of the cap.
        let allow = Curve::capped_allowance(weighted, total, keep16);
        let mut excess = total.saturating_sub(allow);
        // Under the cap an extent larger than the excess goes whole: free
        // neighbours coalesce, so what a program frees in pieces is often
        // one extent, and skipping it would keep the retention over the
        // cap for good (measured: 50 freed 8 MiB runs, one 400 MiB
        // extent, nothing purged in 3 s). The curve alone keeps the rule.
        let capped = keep16 < crate::policy::KEEP_ALL;
        // As in `purge_empty`: pages of no live run.
        let ghost held = g@.pgs@@;
        let mut t = Tally::new(Ghost(held), Ghost(g@.elist@), Ghost(false), Ghost(g@.elist@.len() as int));
        if excess > 0 {
            let mut batch = Batch::new(Ghost(|b: int| self.purgeable(held, b)));
            let mut d = g.empty_head;
            proof {
                reveal(has_piece);
                g@.lemma_head();
                g@.lemma_elist();
                assert(g@.elist@.subrange(0, g@.elist@.len() as int) =~= g@.elist@);
                assert(t.inv(self, g@, batch.pieces()));
            }
            while !d.is_null() && excess > 0
                invariant
                    g.spec_lock() == self.inner,
                    crate::sys::may_abort() || crate::sys::valid_aborts(),
                    crate::sys::may_syscall(),
                    self.is_process(),
                    batch.wf(),
                    forall|b: int| #[trigger] (batch.free())(b) == self.purgeable(held, b),
                    t.inv(self, g@, batch.pieces()),
                    t.held@ == held,
                    !t.full@,
                    d == (if t.rest@.len() > 0 { t.rest@[0] } else { ptr::null_mut::<HugeDesc>() }),
                decreases t.rest@.len(),
            {
                proof {
                    if t.rest@.len() == 0 {
                        assert(d@.addr == 0);
                    }
                    t.lemma_rest_live(self, g@, batch.pieces());
                }
                // SAFETY: as above; `next` is read before a flush can
                // unlink `d`.
                let (pages, next) = (g.desc(d).pages(), g.desc(d).next);
                if pages <= excess || capped {
                    let pushed = self.push_extent(&mut g, d, &mut batch, &mut t);
                    excess = excess.saturating_sub(pushed);
                } else {
                    proof {
                        reveal(has_piece);
                        lemma_walked_visit(g@.elist@, t.rest@, t.vis@);
                        let pend = batch.pieces();
                        let vis0 = t.vis@;
                        t.vis@ = t.vis@.insert(d);
                        t.rest@ = t.rest@.drop_first();
                        assert forall|x: (int, int)| #[trigger] pend(x) implies has_piece(self, g@, t.vis@, x) by {
                            assert(has_piece(self, g@, vis0, x));
                            let y = choose|y: *mut HugeDesc|
                                #[trigger] g@.descs@.contains_key(y) && g@.descs@[y].value().state == RunState::Empty as u8
                                    && vis0.contains(y) && piece_of(self, g@.descs@[y].value()) == x;
                            assert(t.vis@.contains(y));
                        }
                        assert(t.inv(self, g@, pend));
                    }
                }
                proof {
                    t.lemma_rest_live(self, g@, batch.pieces());
                }
                d = next;
            }
            batch.flush(&mut t, self, &mut g);
        }
        if t.failed {
            self.gate.back_off(now);
        }
        self.sync_oldest(&g);
        proof {
            // The lock's frame: the ghost page count, shift, base and `Meta`
            // identity are the ones `new` set.
            assert(lock_frame(lock_init(self.inner), g@));
        }
        let r = t.purged;
        g.unlock();
        r
    }

    /// [`decay_with`](Huge::decay_with) keeping everything the curve
    /// allows.
    #[cfg(test)]
    pub fn decay(&self, now: u64, decay_ns: u64, Tracked(ord): Tracked<&mut LockOrder>) -> Option<u64>
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
    {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL, Tracked(ord))
    }

    /// Decay step (design 8), as `ClassState::decay_with`: the
    /// gate lets `purge_curve` run over the list at most once per step of the
    /// curve, with the cgroup policy's keep ratio (design 11), and returns
    /// the next deadline, `None` when nothing is `Empty`. Lock-free unless
    /// a walk is due.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u64>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            self.is_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r matches Some(t) ==> t >= now,
            exists|oldest: u64, retry: u64, next: u64, after: u64, retry2: u64|
                r == #[trigger] crate::purge::Decayer::spec_decay(now, decay_ns, oldest, retry, next, after, retry2),
    {
        match self.gate.due(now, decay_ns) {
            Due::Done(r) => r,
            Due::Walk(curve, step, seen) => {
                self.purge_curve(now, &curve, keep16, Tracked(ord));
                self.gate.finish(now, step, Ghost(decay_ns), seen)
            },
        }
    }

    /// `dirty_at` of the oldest `Empty` extent as the decay driver sees it
    /// (0 when none).
    #[cfg(test)]
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Bytes in `Empty` extents (one lock hold), for the cgroup policy's
    /// retention figure: at most the region.
    pub fn empty_bytes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r <= crate::meta::va_limit(),
    {
        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            assert(g@.stats.empty_pages <= g@.next_page <= self.npages);
            self.lemma_pages(g@.stats.empty_pages as usize);
        }
        let r = g.stats.empty_pages << self.lg_page;
        g.unlock();
        r
    }

    /// The extent counters, from one lock hold. The free extents' pages
    /// are among the region's, at most `u32::MAX`.
    pub fn stats(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: HugeStats)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            r.empty_pages + r.purged_pages <= u32::MAX,
            *final(ord) == *old(ord),
    {
        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        proof {
            g@.table.lemma_wf_storage();
        }
        let r = HugeStats { table_cap: g.table.cap(), ..g.stats };
        g.unlock();
        r
    }
}

} // verus!
