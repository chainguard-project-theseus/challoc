//! Freeing a huge run (design 7): [`Huge::free_run`] and the [`Merge`]
//! model of what it coalesces, the lemmas that keep `Inner::wf` across
//! the merge, the retention cap's outcome (`retain_cap`), and the
//! run-token reads [`Huge::usable_size`] and [`Huge::lookup`].

use super::*;

verus! {

/// What [`Huge::free_run`] merges into one free extent: the freed run `d`
/// (`vd`, still `Active`) and its free neighbours `pred`, which ends where
/// the run starts, and `succ`, which starts where it ends, when present.
/// `surv` holds the merged extent afterwards, as `vm`; the others retire.
#[allow(dead_code)]
ghost struct Merge {
    d: *mut HugeDesc,
    vd: HugeDesc,
    pred: *mut HugeDesc,
    has_pred: bool,
    vp: HugeDesc,
    succ: *mut HugeDesc,
    has_succ: bool,
    vs: HugeDesc,
    surv: *mut HugeDesc,
    vm: HugeDesc,
}

impl Merge {
    /// After `i` steps of the coalescing loop, `k` is a key of a neighbour
    /// it has taken out of the table.
    spec fn nb_key(self, i: int, k: int) -> bool {
        (i >= 1 && self.has_pred && is_key(self.vp, k)) || (i >= 2 && self.has_succ && is_key(self.vs, k))
    }

    /// What those neighbours added to tally `j`.
    spec fn nb_w(self, i: int, j: int) -> int {
        (if i >= 1 && self.has_pred { weight(self.vp, j) } else { 0 }) + (if i >= 2 && self.has_succ {
            weight(self.vs, j)
        } else {
            0
        })
    }

    /// One of them is `Empty`.
    spec fn nb_empty(self, i: int) -> bool {
        (i >= 1 && self.has_pred && self.vp.state == RunState::Empty as u8) || (i >= 2 && self.has_succ && self.vs.state
            == RunState::Empty as u8)
    }

    /// The merged extent's first page so far.
    spec fn nb_lo(self, i: int) -> int {
        if i >= 1 && self.has_pred { self.vp.start as int } else { self.vd.start as int }
    }

    /// One past its last page so far.
    spec fn nb_hi(self, i: int) -> int {
        if i >= 2 && self.has_succ { self.vs.start + self.vs.pages } else { self.vd.start + self.vd.pages }
    }

    /// After `i` steps of the retiring loop over `[d, pred, succ]`, `x` is
    /// one it has retired: present and not the survivor.
    spec fn gone(self, i: int, x: *mut HugeDesc) -> bool {
        ||| (i >= 1 && x == self.d && self.d != self.surv)
        ||| (i >= 2 && self.has_pred && x == self.pred && self.pred != self.surv)
        ||| (i >= 3 && self.has_succ && x == self.succ && self.succ != self.surv)
    }

    /// What those added to tally `j`.
    spec fn gone_w(self, i: int, j: int) -> int {
        (if i >= 1 && self.d != self.surv { weight(self.vd, j) } else { 0 }) + (if i >= 2 && self.has_pred && self.pred
            != self.surv {
            weight(self.vp, j)
        } else {
            0
        }) + (if i >= 3 && self.has_succ && self.succ != self.surv { weight(self.vs, j) } else { 0 })
    }

    /// `x` is one of the merged extents.
    spec fn part(self, x: *mut HugeDesc) -> bool {
        x == self.d || (self.has_pred && x == self.pred) || (self.has_succ && x == self.succ)
    }

    /// What the merged extents added to tally `j`.
    spec fn parts(self, j: int) -> int {
        weight(self.vd, j) + self.nb_w(2, j)
    }

    /// The descriptor at `x` afterwards, up to its list links.
    spec fn model(self, d0: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc) -> Option<HugeDesc> {
        if x == self.surv {
            Some(self.vm)
        } else if self.part(x) {
            None
        } else if d0.contains_key(x) {
            Some(d0[x].value())
        } else {
            None
        }
    }

    /// The table's entry for `k` afterwards.
    spec fn lookup(self, t0: Table, k: usize) -> Option<*mut HugeDesc> {
        if is_key(self.vm, k as int) {
            Some(self.surv)
        } else if is_key(self.vd, k as int) || self.nb_key(2, k as int) {
            None
        } else {
            t0.lookup(k)
        }
    }

    /// The merged extents, against the state `g0` they come from.
    spec fn parts_ok(self, g0: Inner) -> bool {
        &&& g0.descs@.contains_key(self.d)
        &&& self.vd == g0.descs@[self.d].value()
        &&& self.vd.state == RunState::Active as u8
        &&& self.has_pred == (self.pred@.addr != 0)
        &&& self.has_pred ==> g0.descs@.contains_key(self.pred) && self.vp == g0.descs@[self.pred].value() && self.vp.state
            != RunState::Active as u8 && self.vp.start + self.vp.pages == self.vd.start
        &&& self.has_succ == (self.succ@.addr != 0)
        &&& self.has_succ ==> g0.descs@.contains_key(self.succ) && self.vs == g0.descs@[self.succ].value() && self.vs.state
            != RunState::Active as u8 && self.vs.start == self.vd.start + self.vd.pages
    }

    /// The survivor is one of the merged extents and holds all of them,
    /// free.
    spec fn surv_ok(self) -> bool {
        &&& self.part(self.surv)
        &&& self.vm.start == self.nb_lo(2)
        &&& self.vm.pages == self.nb_hi(2) - self.nb_lo(2)
        &&& self.vm.state == RunState::Empty as u8 || self.vm.state == RunState::Purged as u8
    }

    /// The neighbours are every free extent touching the run: without
    /// `pred`, no free extent ends where the run starts; without `succ`,
    /// none starts where it ends.
    spec fn nbrs_ok(self, g0: Inner) -> bool {
        &&& !self.has_pred ==> none_ends_at(g0.descs@, self.vd.start as int)
        &&& !self.has_succ ==> none_starts_at(g0.descs@, self.vd.start + self.vd.pages)
    }
}

impl Inner {
    /// `self` is `g0` with the extents of `m` merged into `m.surv`.
    spec fn freed(&self, g0: Inner, m: Merge) -> bool {
        &&& m.parts_ok(g0)
        &&& m.surv_ok()
        &&& self.table.wf()
        &&& self.table.spec_count() == g0.table.spec_count() - m.parts(6) + weight(m.vm, 6)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) == m.lookup(g0.table, k)
        &&& perms_ok(self.descs@)
        &&& forall|d: *mut HugeDesc| #[trigger] self.descs@.contains_key(d) ==> desc_claimed(self.dclaims@, d, self.mid@)
        &&& self.table.spec_mid() == self.mid@
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> m.model(g0.descs@, x) is Some
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) ==> same_but_links(
            m.model(g0.descs@, x).unwrap(),
            self.descs@[x].value(),
        )
        &&& self.all@.no_duplicates()
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) <==> self.all@.contains(x)
        &&& forall|j: int| 0 <= j < 7 ==> #[trigger] tally(self.all@, self.descs@, j) == tally(g0.all@, g0.descs@, j) - m.parts(j)
            + weight(m.vm, j)
        &&& forall|j: int| 0 <= j < 6 ==> #[trigger] stat(self.stats, j) == stat(g0.stats, j) - m.parts(j) + weight(m.vm, j)
        &&& self.next_page == g0.next_page
        &&& self.stats.next_page == g0.stats.next_page
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.base == g0.base
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
        &&& self.free.own_claimed()
        &&& self.free.l1_words <= 0x1_0000_0000
        &&& elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@)
        &&& self.stats.empty == self.elist@.len()
        &&& self.stats.empty_pages == epages(self.elist@, self.descs@)
        &&& forall|x: *mut HugeDesc| #[trigger] self.descs@.contains_key(x) && self.descs@[x].value().state == RunState::Empty as u8
            ==> self.elist@.contains(x)
    }
}

/// A key of an extent is one of its pages.
proof fn lemma_key_inside(v: HugeDesc, k: int)
    requires
        v.pages >= 1,
        is_key(v, k),
    ensures
        v.start <= k < v.start + v.pages,
{
}

/// A state's weights: one extent, and its pages, in one state.
proof fn lemma_weights(v: HugeDesc)
    requires
        v.state <= 2,
    ensures
        weight(v, 0) + weight(v, 1) + weight(v, 2) == 1,
        weight(v, 3) + weight(v, 4) + weight(v, 5) == v.pages,
{
}

/// An old extent outside the merge shares no page with the merged one.
proof fn lemma_freed_apart(g0: Inner, m: Merge, y: *mut HugeDesc)
    requires
        g0.wf(),
        m.parts_ok(g0),
        m.surv_ok(),
        g0.descs@.contains_key(y),
        !m.part(y),
    ensures
        apart(g0.descs@[y].value(), m.vm),
        desc_ok(g0.descs@[y].value(), g0.next_page),
{
    let d0 = g0.descs@;
    assert(apart(d0[y].value(), m.vd));
    if m.has_pred {
        assert(apart(d0[y].value(), m.vp));
    }
    if m.has_succ {
        assert(apart(d0[y].value(), m.vs));
    }
    assert(desc_ok(d0[y].value(), g0.next_page));
}

/// The extents after a free: inside the mark, disjoint, each under exactly
/// its keys.
proof fn lemma_freed_extents(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
    ensures
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) ==> desc_ok(g1.descs@[d].value(), g1.next_page),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(d) ==> g1.descs@.contains_key(d),
        forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) ==> keyed(g1.table, g1.descs@[d].value(), d),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(d) ==> is_key(g1.descs@[d].value(), k as int),
        forall|d1: *mut HugeDesc, d2: *mut HugeDesc|
            #[trigger] g1.descs@.contains_key(d1) && #[trigger] g1.descs@.contains_key(d2) && d1 != d2 ==> apart(
                g1.descs@[d1].value(),
                g1.descs@[d2].value(),
            ),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let t0 = g0.table;
    assert(desc_ok(m.vd, g0.next_page));
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
    }
    assert(m.model(d0, m.surv) == Some(m.vm));
    assert(d1.contains_key(m.surv));
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != m.surv implies d0.contains_key(x) && !m.part(x)
        && same_but_links(d0[x].value(), d1[x].value()) by {
        assert(m.model(d0, x) is Some);
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies desc_ok(d1[x].value(), g1.next_page) by {
        if x != m.surv {
            lemma_freed_apart(g0, m, x);
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies keyed(g1.table, d1[x].value(), x) by {
        let v = d1[x].value();
        let k1 = v.start as usize;
        let k2 = (v.start + v.pages - 1) as usize;
        assert(g1.table.lookup(k1) == m.lookup(t0, k1));
        assert(g1.table.lookup(k2) == m.lookup(t0, k2));
        if x != m.surv {
            lemma_freed_apart(g0, m, x);
            assert(apart(d0[x].value(), m.vd));
            if m.has_pred {
                assert(apart(d0[x].value(), m.vp));
            }
            if m.has_succ {
                assert(apart(d0[x].value(), m.vs));
            }
            assert(keyed(t0, d0[x].value(), x));
        }
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies d1.contains_key(g1.table.lookup(k).unwrap()) && is_key(
        d1[g1.table.lookup(k).unwrap()].value(),
        k as int,
    ) by {
        assert(g1.table.lookup(k) == m.lookup(t0, k));
        let y = g1.table.lookup(k).unwrap();
        if !is_key(m.vm, k as int) {
            assert(t0.lookup(k) == Some(y));
            assert(d0.contains_key(y) && is_key(d0[y].value(), k as int));
            assert(!m.part(y));
            assert(y != m.surv);
            assert(m.model(d0, y) == Some(d0[y].value()));
            assert(d1.contains_key(y));
        }
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && x1 != x2 implies apart(d1[x1].value(), d1[x2].value()) by {
        if x1 != m.surv && x2 != m.surv {
            assert(apart(d0[x1].value(), d0[x2].value()));
        } else if x1 != m.surv {
            lemma_freed_apart(g0, m, x1);
        } else {
            lemma_freed_apart(g0, m, x2);
        }
    }
}

/// A free makes the run's descriptor free (merged into the survivor, or
/// retired): the run authorities lose its extent and its pages, which no
/// other `Active` descriptor has.
proof fn lemma_freed_active(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
    ensures
        active_runs(g1.descs@) == active_runs(g0.descs@).remove((m.vd.start as int, m.vd.pages as int)),
        active_pages(g1.descs@) == active_pages(g0.descs@).difference(
            vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages),
        ),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    lemma_active(d0);
    lemma_active(d1);
    let ed = (m.vd.start as int, m.vd.pages as int);
    let range = vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages);
    assert(desc_ok(m.vd, g0.next_page));
    // An `Active` descriptor afterwards is an old one other than the run.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Active as u8 implies {
        &&& d0.contains_key(x)
        &&& x != m.d
        &&& same_but_links(d0[x].value(), d1[x].value())
        &&& apart(d0[x].value(), m.vd)
    } by {
        assert(same_but_links(m.model(d0, x).unwrap(), d1[x].value()));
    }
    // An old `Active` descriptor other than the run stays.
    assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state == RunState::Active as u8 && x != m.d implies {
        &&& d1.contains_key(x)
        &&& same_but_links(d0[x].value(), d1[x].value())
        &&& apart(d0[x].value(), m.vd)
    } by {
        assert(!m.part(x));
        assert(x != m.surv);
        assert(m.model(d0, x) == Some(d0[x].value()));
        assert(d1.contains_key(x) <==> m.model(d0, x) is Some);
        assert(d1.contains_key(x));
        assert(same_but_links(m.model(d0, x).unwrap(), d1[x].value()));
    }
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d0).remove(ed).contains(e) by {
        if active_runs(d1).contains(e) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_at(d1[x].value(), e);
            assert(d0.contains_key(x) && run_at(d0[x].value(), e));
        }
        if active_runs(d0).contains(e) && e != ed {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_at(d0[x].value(), e);
            assert(x != m.d);
            assert(d1.contains_key(x) && run_at(d1[x].value(), e));
        }
    }
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d0).difference(range).contains(k) by {
        if active_pages(d1).contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && run_page(d1[x].value(), k);
            assert(d0.contains_key(x) && run_page(d0[x].value(), k));
        }
        if active_pages(d0).contains(k) && !range.contains(k) {
            let x = choose|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && run_page(d0[x].value(), k);
            assert(x != m.d);
            assert(d1.contains_key(x) && run_page(d1[x].value(), k));
        }
    }
    assert(active_runs(d1) =~= active_runs(d0).remove(ed));
    assert(active_pages(d1) =~= active_pages(d0).difference(range));
}

/// The free-page set after a free, against `f0` before: exactly what was
/// free, and every page of the freed run `v`.
spec fn freed_free(f0: vstd::set::Set<nat>, f1: vstd::set::Set<nat>, v: HugeDesc) -> bool {
    forall|q: nat| #[trigger] f1.contains(q) <==> f0.contains(q) || (v.start <= q < v.start + v.pages)
}

/// A free keeps the free extents coalesced and marked free: the merged
/// extent starts at `pred`'s start, which no other free extent touched,
/// or at the run's, which none did when `free_before` found none, and
/// ends likewise; its pages are `pred`'s, `succ`'s (free before) and the
/// run's (added to the set). The set holds nothing else: a page free
/// before is in `pred`, `succ` (both now the merged extent) or an
/// untouched free extent.
proof fn lemma_freed_coalesced(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map.view()),
        free_exact(g1.descs@, g1.free.map.view()),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let f0 = g0.free.map.view();
    let f1 = g1.free.map.view();
    lemma_parts_distinct(g0, m);
    assert(m.model(d0, m.surv) == Some(m.vm));
    assert(d1.contains_key(m.surv));
    assert(same_but_links(m.vm, d1[m.surv].value()));
    // Which descriptors are old.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && x != m.surv implies d0.contains_key(x) && !m.part(x)
        && same_but_links(d0[x].value(), d1[x].value()) by {
        assert(m.model(d0, x) is Some);
    }
    if m.has_pred {
        assert(marked(f0, m.vp));
    }
    if m.has_succ {
        assert(marked(f0, m.vs));
    }
    assert forall|x1: *mut HugeDesc, x2: *mut HugeDesc|
        #[trigger] d1.contains_key(x1) && #[trigger] d1.contains_key(x2) && d1[x1].value().state != RunState::Active as u8
            && d1[x2].value().state != RunState::Active as u8 implies d1[x1].value().start + d1[x1].value().pages
            != d1[x2].value().start by {
        if x1 != m.surv && x2 != m.surv {
            assert(d0.contains_key(x1) && d0.contains_key(x2));
        } else if x1 != m.surv {
            if m.has_pred {
                assert(d0.contains_key(x1) && d0.contains_key(m.pred));
            } else {
                assert(d0.contains_key(x1));
            }
        } else if x2 != m.surv {
            if m.has_succ {
                assert(d0.contains_key(m.succ) && d0.contains_key(x2));
            } else {
                assert(d0.contains_key(x2));
            }
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state != RunState::Active as u8 implies marked(
        f1,
        d1[x].value(),
    ) by {
        let v = d1[x].value();
        if x != m.surv {
            assert(d0.contains_key(x));
            assert(marked(f0, d0[x].value()));
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                assert(f0.contains(q as nat));
            }
        } else {
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                if m.has_pred && q < m.vd.start {
                    assert(f0.contains(q as nat));
                }
                if m.has_succ && q >= m.vd.start + m.vd.pages {
                    assert(f0.contains(q as nat));
                }
            }
        }
    }
    assert forall|q: nat| #[trigger] f1.contains(q) implies exists|x: *mut HugeDesc| #[trigger] in_free(d1, x, q as int) by {
        if f0.contains(q) {
            let x = choose|x: *mut HugeDesc| #[trigger] in_free(d0, x, q as int);
            if m.part(x) {
                assert(x != m.d);
                assert(in_free(d1, m.surv, q as int));
            } else {
                assert(m.model(d0, x) == Some(d0[x].value()));
                assert(d1.contains_key(x));
                assert(same_but_links(d0[x].value(), d1[x].value()));
                assert(in_free(d1, x, q as int));
            }
        } else {
            assert(in_free(d1, m.surv, q as int));
        }
    }
}

/// A free keeps `Inner::wf`.
proof fn lemma_freed_wf(g0: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.freed(g0, m),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
    ensures
        g1.wf(),
{
    lemma_freed_extents(g0, g1, m);
    lemma_freed_coalesced(g0, g1, m);
    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == tally(g1.all@, g1.descs@, k) by {
        assert(stat(g0.stats, k) == tally(g0.all@, g0.descs@, k));
        assert(tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) - m.parts(k) + weight(m.vm, k));
    }
    assert(g1.table.spec_count() == tally(g1.all@, g1.descs@, 6)) by {
        assert(tally(g1.all@, g1.descs@, 6) == tally(g0.all@, g0.descs@, 6) - m.parts(6) + weight(m.vm, 6));
    }
    // The pages sum to the mark: the merged extent has the parts' pages.
    assert(desc_ok(m.vd, g0.next_page));
    lemma_weights(m.vd);
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
        lemma_weights(m.vp);
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
        lemma_weights(m.vs);
    }
    lemma_weights(m.vm);
    assert(m.parts(3) + m.parts(4) + m.parts(5) == weight(m.vm, 3) + weight(m.vm, 4) + weight(m.vm, 5));
    assert(stat(g1.stats, 3) + stat(g1.stats, 4) + stat(g1.stats, 5) == stat(g0.stats, 3) + stat(g0.stats, 4) + stat(g0.stats, 5));
    // Every extent has a page, so the counts are at most the pages.
    let a1 = g1.all@;
    let d1 = g1.descs@;
    assert forall|i: int| 0 <= i < a1.len() implies (#[trigger] d1[a1[i]].value()).pages >= 1 && d1[a1[i]].value().state <= 2 by {
        assert(a1.contains(a1[i]));
        assert(d1.contains_key(a1[i]));
        assert(desc_ok(d1[a1[i]].value(), g1.next_page));
    }
    lemma_tally_counts(a1, d1, 0);
    lemma_tally_counts(a1, d1, 1);
    lemma_tally_counts(a1, d1, 2);
    assert(stat(g1.stats, 0) + stat(g1.stats, 1) + stat(g1.stats, 2) <= g1.next_page);
}

/// The merged extents are three different descriptors, the neighbours
/// free and on either side of the run.
proof fn lemma_parts_distinct(g0: Inner, m: Merge)
    requires
        g0.wf(),
        m.parts_ok(g0),
    ensures
        m.has_pred ==> m.pred != m.d && m.vp.pages >= 1,
        m.has_succ ==> m.succ != m.d && m.vs.pages >= 1,
        m.has_pred && m.has_succ ==> m.pred != m.succ,
        m.vd.pages >= 1,
{
    assert(desc_ok(m.vd, g0.next_page));
    if m.has_pred {
        assert(desc_ok(m.vp, g0.next_page));
    }
    if m.has_succ {
        assert(desc_ok(m.vs, g0.next_page));
    }
}

/// The merged extents are counted together: the counters hold `d`, `pred`
/// and `succ` at once (three distinct live descriptors), so `free_run`
/// can take each out with [`Inner::drop_extent`].
proof fn lemma_parts_counted(g0: Inner, m: Merge, k: int)
    requires
        g0.wf(),
        m.parts_ok(g0),
        0 <= k < 6,
    ensures
        weight(m.vd, k) + m.nb_w(2, k) <= stat(g0.stats, k),
{
    lemma_parts_distinct(g0, m);
    let ds = g0.descs@;
    let l0 = g0.all@;
    assert(stat(g0.stats, k) == tally(l0, ds, k));
    assert(l0.contains(m.d));
    let i0 = l0.index_of(m.d);
    lemma_tally_remove(l0, ds, i0, k);
    let l1 = l0.remove(i0);
    // A member other than the one removed is still a member.
    assert forall|x: *mut HugeDesc| l0.contains(x) && x != m.d implies #[trigger] l1.contains(x) by {
        let j = l0.index_of(x);
        if j < i0 {
            assert(l1[j] == x);
        } else {
            assert(j != i0);
            assert(l1[j - 1] == x);
        }
    }
    let l2 = if m.has_pred {
        assert(l0.contains(m.pred));
        assert(l1.contains(m.pred));
        let i1 = l1.index_of(m.pred);
        lemma_tally_remove(l1, ds, i1, k);
        let l2 = l1.remove(i1);
        assert forall|x: *mut HugeDesc| l1.contains(x) && x != m.pred implies #[trigger] l2.contains(x) by {
            let j = l1.index_of(x);
            if j < i1 {
                assert(l2[j] == x);
            } else {
                assert(j != i1);
                assert(l2[j - 1] == x);
            }
        }
        l2
    } else {
        l1
    };
    assert(tally(l2, ds, k) == stat(g0.stats, k) - weight(m.vd, k) - (if m.has_pred { weight(m.vp, k) } else { 0 }));
    if m.has_succ {
        assert(l0.contains(m.succ));
        assert(l1.contains(m.succ));
        assert(l2.contains(m.succ));
        let i2 = l2.index_of(m.succ);
        lemma_tally_remove(l2, ds, i2, k);
        lemma_tally_nonneg(l2.remove(i2), ds, k);
    } else {
        lemma_tally_nonneg(l2, ds, k);
    }
}

/// Step `i` of the retiring loop may retire `x`, the `i`th of
/// `[d, pred, succ]`, when it is present and not the survivor.
proof fn lemma_retire_ok(g0: Inner, g: Inner, m: Merge, i: int, x: *mut HugeDesc)
    requires
        g0.wf(),
        m.parts_ok(g0),
        0 <= i < 3,
        x == seq![m.d, m.pred, m.succ][i],
        x@.addr != 0,
        x != m.surv,
        forall|y: *mut HugeDesc| #[trigger] g.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i, y),
        forall|y: *mut HugeDesc| #[trigger] g.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), g.descs@[y].value()),
        forall|y: *mut HugeDesc| #[trigger] g.all@.contains(y) <==> g.descs@.contains_key(y),
        forall|y: *mut HugeDesc| #[trigger] g.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i, y),
    ensures
        g.descs@.contains_key(x),
        g.all@.contains(x),
        g.descs@[x].value().state == RunState::Empty as u8 ==> g.elist@.contains(x),
        forall|y: *mut HugeDesc| #[trigger] m.gone(i + 1, y) <==> m.gone(i, y) || y == x,
        forall|j: int| #[trigger] m.gone_w(i + 1, j) == m.gone_w(i, j) + weight(g0.descs@[x].value(), j),
{
    lemma_parts_distinct(g0, m);
    assert(seq![m.d, m.pred, m.succ][0] == m.d);
    assert(seq![m.d, m.pred, m.succ][1] == m.pred);
    assert(seq![m.d, m.pred, m.succ][2] == m.succ);
    assert(i == 1 ==> m.has_pred);
    assert(i == 2 ==> m.has_succ);
    assert(g0.descs@.contains_key(x));
    assert(!m.gone(i, x));
    let v = g0.descs@[x].value();
    assert(g.descs@.contains_key(x));
    assert(same_but_links(v, g.descs@[x].value()));
    if v.state == RunState::Empty as u8 {
        assert(g0.elist@.contains(x));
    }
}

/// Step `i` of the retiring loop retired `x` (`gr` before, `g1` after):
/// its invariant holds for step `i + 1`.
proof fn lemma_retired(g0: Inner, gr: Inner, g1: Inner, m: Merge, i: int, x: *mut HugeDesc)
    requires
        g0.wf(),
        gr.descs@.contains_key(x),
        gr.all@.contains(x),
        gr.descs@[x].value().state == RunState::Empty as u8 ==> gr.elist@.contains(x),
        forall|y: *mut HugeDesc| #[trigger] m.gone(i + 1, y) <==> m.gone(i, y) || y == x,
        forall|j: int| #[trigger] m.gone_w(i + 1, j) == m.gone_w(i, j) + weight(g0.descs@[x].value(), j),
        // The loop invariant at step `i`.
        forall|y: *mut HugeDesc| #[trigger] gr.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i, y),
        forall|y: *mut HugeDesc| #[trigger] gr.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), gr.descs@[y].value()),
        gr.all@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] gr.all@.contains(y) <==> gr.descs@.contains_key(y),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(gr.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(i, j),
        gr.elist@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] gr.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i, y),
        gr.elist@.len() == g0.elist@.len() - m.gone_w(i, 1),
        epages(gr.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(i, 4),
        // What `retire` did.
        g1.descs@.dom() == gr.descs@.dom().remove(x),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> same_but_links(gr.descs@[y].value(), g1.descs@[y].value()),
        g1.elist@ == if gr.descs@[x].value().state == RunState::Empty as u8 {
            gr.elist@.remove(gr.elist@.index_of(x))
        } else {
            gr.elist@
        },
        g1.all@ == gr.all@.remove(gr.all@.index_of(x)),
    ensures
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> g0.descs@.contains_key(y) && !m.gone(i + 1, y),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> same_but_links(g0.descs@[y].value(), g1.descs@[y].value()),
        g1.all@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] g1.all@.contains(y) <==> g1.descs@.contains_key(y),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(g1.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(i + 1, j),
        forall|y: *mut HugeDesc| #[trigger] g1.elist@.contains(y) <==> g0.elist@.contains(y) && !m.gone(i + 1, y),
        g1.elist@.len() == g0.elist@.len() - m.gone_w(i + 1, 1),
        epages(g1.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(i + 1, 4),
{
    let d0 = g0.descs@;
    let v = d0[x].value();
    assert(same_but_links(v, gr.descs@[x].value()));
    assert forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> d0.contains_key(y) && !m.gone(i + 1, y) by {
        assert(g1.descs@.contains_key(y) <==> g1.descs@.dom().contains(y));
    }
    lemma_drop(gr.all@, x);
    let ja = gr.all@.index_of(x);
    assert forall|j: int| 0 <= j < 7 implies #[trigger] tally(g1.all@, d0, j) == tally(g0.all@, d0, j) - m.gone_w(i + 1, j) by {
        lemma_tally_remove(gr.all@, d0, ja, j);
    }
    if v.state == RunState::Empty as u8 {
        lemma_drop(gr.elist@, x);
        lemma_epages_remove(gr.elist@, d0, gr.elist@.index_of(x));
    } else {
        assert(!g0.elist@.contains(x)) by {
            if g0.elist@.contains(x) {
                assert(enode(g0.elist@, d0, g0.elist@.index_of(x)));
            }
        }
    }
    assert(m.gone_w(i + 1, 1) == m.gone_w(i, 1) + weight(v, 1));
    assert(m.gone_w(i + 1, 4) == m.gone_w(i, 4) + weight(v, 4));
}

/// No extent outside the merge has a key inside the merged extent.
proof fn lemma_merged_room(g0: Inner, m: Merge, key: usize)
    requires
        g0.wf(),
        m.parts_ok(g0),
        m.nb_lo(2) <= key < m.nb_hi(2),
    ensures
        g0.table.lookup(key) is Some ==> is_key(m.vd, key as int) || m.nb_key(2, key as int),
{
    lemma_parts_distinct(g0, m);
    if g0.table.lookup(key) is Some {
        let y = g0.table.lookup(key).unwrap();
        let v = g0.descs@[y].value();
        assert(g0.descs@.contains_key(y) && is_key(v, key as int));
        assert(desc_ok(v, g0.next_page));
        if y != m.d {
            assert(apart(v, m.vd));
        }
        if m.has_pred && y != m.pred {
            assert(apart(v, m.vp));
        }
        if m.has_succ && y != m.succ {
            assert(apart(v, m.vs));
        }
    }
}

/// The end of a free, from three states: `gr` after the retiring loop,
/// whose table and counters are those before the free without the merged
/// extents; `gl` after the survivor's rewrite as `vm` (and, when it is
/// the run's descriptor turned `Empty`, its append to the `Empty` list);
/// `g1` after its entry into the table, the counters and the free-page
/// set. `g1` is `g0` with the extents of `m` merged.
proof fn lemma_free_end(g0: Inner, gr: Inner, gl: Inner, g1: Inner, m: Merge)
    requires
        g0.wf(),
        g1.stats.next_page == g0.stats.next_page,
        m.parts_ok(g0),
        m.surv_ok(),
        m.nbrs_ok(g0),
        freed_free(g0.free.map.view(), g1.free.map.view(), m.vd),
        m.surv != m.d ==> g0.descs@[m.surv].value().state == RunState::Empty as u8 && m.vm.state == RunState::Empty as u8,
        // `gr`: the merged extents are out of the table and the counters,
        // and the ones other than the survivor are retired.
        gr.table.wf(),
        forall|k: usize| #[trigger] gr.table.lookup(k) == if is_key(m.vd, k as int) || m.nb_key(2, k as int) {
            None
        } else {
            g0.table.lookup(k)
        },
        gr.table.spec_count() == g0.table.spec_count() - m.parts(6),
        forall|j: int| 0 <= j < 6 ==> #[trigger] stat(gr.stats, j) == stat(g0.stats, j) - m.parts(j),
        forall|x: *mut HugeDesc| #[trigger] gr.descs@.contains_key(x) <==> g0.descs@.contains_key(x) && !m.gone(3, x),
        forall|x: *mut HugeDesc| #[trigger] gr.descs@.contains_key(x) ==> same_but_links(g0.descs@[x].value(), gr.descs@[x].value()),
        gr.all@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] gr.all@.contains(x) <==> gr.descs@.contains_key(x),
        forall|j: int| 0 <= j < 7 ==> #[trigger] tally(gr.all@, g0.descs@, j) == tally(g0.all@, g0.descs@, j) - m.gone_w(3, j),
        gr.elist@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] gr.elist@.contains(x) <==> g0.elist@.contains(x) && !m.gone(3, x),
        gr.elist@.len() == g0.elist@.len() - m.gone_w(3, 1),
        epages(gr.elist@, g0.descs@) == epages(g0.elist@, g0.descs@) - m.gone_w(3, 4),
        gr.next_page == g0.next_page,
        gr.np == g0.np,
        gr.lg == g0.lg,
        gr.base == g0.base,
        gr.mid == g0.mid,
        forall|x: *mut HugeDesc| #[trigger] gr.descs@.contains_key(x) ==> desc_claimed(gr.dclaims@, x, g0.mid@),
        // `gl`: the survivor rewritten.
        gl.table == gr.table,
        gl.stats == gr.stats,
        gl.all == gr.all,
        gl.next_page == gr.next_page,
        gl.np == gr.np,
        gl.lg == gr.lg,
        gl.base == gr.base,
        gl.mid == gr.mid,
        gl.dclaims == gr.dclaims,
        perms_ok(gl.descs@),
        gl.descs@.dom() == gr.descs@.dom(),
        forall|x: *mut HugeDesc| #[trigger] gl.descs@.contains_key(x) && x != m.surv ==> same_but_links(
            gr.descs@[x].value(),
            gl.descs@[x].value(),
        ),
        gl.descs@[m.surv].value() == m.vm,
        gl.elist@ == if m.surv == m.d && m.vm.state == RunState::Empty as u8 {
            gr.elist@.push(m.d)
        } else {
            gr.elist@
        },
        elist_ok(gl.empty_head, gl.empty_tail, gl.elist@, gl.descs@),
        // `g1`: the survivor entered.
        g1 == (Inner { table: g1.table, stats: g1.stats, free: g1.free, ..gl }),
        g1.table.wf(),
        g1.table.spec_mid() == g0.mid@,
        g1.table.spec_count() == gl.table.spec_count() + weight(m.vm, 6),
        forall|k: usize| #[trigger] g1.table.lookup(k) == if is_key(m.vm, k as int) {
            Some(m.surv)
        } else {
            gl.table.lookup(k)
        },
        forall|j: int| 0 <= j < 6 ==> #[trigger] stat(g1.stats, j) == stat(gl.stats, j) + weight(m.vm, j),
        g1.free.map.wf(),
        g1.free.map.in_meta(),
        g1.free.map.spec_nbits() == g1.np@,
        g1.free.own_claimed(),
        g1.free.l1_words <= 0x1_0000_0000,
        // The run authorities without the run's token.
        g1.runs@@ == active_runs(g0.descs@).remove((m.vd.start as int, m.vd.pages as int)),
        g1.pgs@@ == active_pages(g0.descs@).difference(vstd::set_lib::set_int_range(m.vd.start as int, m.vd.start + m.vd.pages)),
    ensures
        g1.wf(),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let l0 = g0.elist@;
    let lr = gr.elist@;
    let ar = gr.all@;
    let s = m.surv;
    let v0 = d0[s].value();
    lemma_parts_distinct(g0, m);
    assert(d0.contains_key(s) && !m.gone(3, s));
    assert(gr.descs@.contains_key(s));
    // The survivor's old weight is what the retired ones leave out.
    assert forall|j: int| #[trigger] m.gone_w(3, j) == m.parts(j) - weight(v0, j) by {}
    // The descriptors afterwards.
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) <==> m.model(d0, x) is Some by {
        assert(d1.contains_key(x) <==> gr.descs@.dom().contains(x));
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) implies same_but_links(m.model(d0, x).unwrap(), d1[x].value()) by {
        if x != s {
            assert(same_but_links(d0[x].value(), gr.descs@[x].value()));
        }
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) <==> g1.all@.contains(x) by {
        assert(d1.contains_key(x) <==> gr.descs@.dom().contains(x));
    }
    // The tallies: the survivor's weight changed, the others' did not.
    assert forall|j: int| 0 <= j < 7 implies #[trigger] tally(g1.all@, d1, j) == tally(g0.all@, d0, j) - m.parts(j) + weight(
        m.vm,
        j,
    ) by {
        assert forall|i: int| 0 <= i < ar.len() && ar[i] != s implies #[trigger] weight(d1[ar[i]].value(), j) == weight(
            d0[ar[i]].value(),
            j,
        ) by {
            assert(ar.contains(ar[i]));
            assert(same_but_links(d0[ar[i]].value(), gr.descs@[ar[i]].value()));
        }
        lemma_tally_update(ar, d0, d1, s, j);
    }
    // The table.
    assert forall|k: usize| #[trigger] g1.table.lookup(k) == m.lookup(g0.table, k) by {}
    // The `Empty` list: its length and pages are the counters, and it has
    // every `Empty` extent.
    assert(stat(g1.stats, 1) == stat(g0.stats, 1) - m.parts(1) + weight(m.vm, 1));
    assert(stat(g1.stats, 4) == stat(g0.stats, 4) - m.parts(4) + weight(m.vm, 4));
    assert forall|i: int| 0 <= i < lr.len() && lr[i] != s implies #[trigger] d1[lr[i]].value().pages == d0[lr[i]].value().pages by {
        assert(lr.contains(lr[i]));
        assert(l0.contains(lr[i]));
        assert(enode(l0, d0, l0.index_of(lr[i])));
        assert(gr.descs@.contains_key(lr[i]));
        assert(d1.contains_key(lr[i]));
        assert(same_but_links(d0[lr[i]].value(), gr.descs@[lr[i]].value()));
        assert(same_but_links(gr.descs@[lr[i]].value(), d1[lr[i]].value()));
    }
    if s == m.d {
        assert(!lr.contains(s)) by {
            if l0.contains(s) {
                assert(enode(l0, d0, l0.index_of(s)));
            }
        }
        lemma_epages_frame(lr, d0, d1);
        if m.vm.state == RunState::Empty as u8 {
            lemma_epages_insert(lr, d1, lr.len() as int, s);
            assert(lr.insert(lr.len() as int, s) =~= lr.push(s));
        }
    } else {
        assert(l0.contains(s));
        assert(lr.contains(s));
        lemma_epages_update(lr, d0, d1, s);
    }
    assert forall|x: *mut HugeDesc| #[trigger] d1.contains_key(x) && d1[x].value().state == RunState::Empty as u8 implies g1.elist@.contains(
        x,
    ) by {
        if x != s {
            assert(same_but_links(d0[x].value(), gr.descs@[x].value()));
            assert(l0.contains(x));
            assert(lr.contains(x));
        } else if s == m.d {
            assert(g1.elist@ == lr.push(s));
            assert(g1.elist@[lr.len() as int] == s);
        } else {
            assert(l0.contains(s));
            assert(lr.contains(s));
        }
        if g1.elist@ != lr && lr.contains(x) {
            assert(g1.elist@[lr.index_of(x)] == x);
        }
    }
    assert(g1.freed(g0, m));
    lemma_freed_active(g0, g1, m);
    lemma_freed_wf(g0, g1, m);
}

/// With the run `d` out of the table `t`, every free extent is still
/// under its keys there, and has a page or more.
proof fn lemma_free_keyed(g0: Inner, t: Table, d: *mut HugeDesc)
    requires
        g0.wf(),
        g0.descs@.contains_key(d),
        g0.descs@[d].value().state == RunState::Active as u8,
        forall|k: usize| #[trigger] t.lookup(k) == if is_key(g0.descs@[d].value(), k as int) {
            None
        } else {
            g0.table.lookup(k)
        },
    ensures
        forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) && g0.descs@[x].value().state != RunState::Active as u8 ==> keyed(
            t,
            g0.descs@[x].value(),
            x,
        ) && g0.descs@[x].value().pages >= 1,
{
    let d0 = g0.descs@;
    let vd = d0[d].value();
    assert(desc_ok(vd, g0.next_page));
    assert forall|x: *mut HugeDesc| #[trigger] d0.contains_key(x) && d0[x].value().state != RunState::Active as u8 implies keyed(
        t,
        d0[x].value(),
        x,
    ) && d0[x].value().pages >= 1 by {
        let v = d0[x].value();
        assert(desc_ok(v, g0.next_page));
        assert(apart(v, vd));
        assert(keyed(g0.table, v, x));
        let k2 = (v.start + v.pages - 1) as usize;
        assert(t.lookup(v.start as usize) == g0.table.lookup(v.start as usize));
        assert(t.lookup(k2) == g0.table.lookup(k2));
    }
}

/// What [`Huge::free_run`] reads from `wf` before it changes anything.
proof fn lemma_free_start(g0: Inner, page: usize)
    requires
        g0.wf(),
    ensures
        g0.lookups_live(),
        g0.next_page <= g0.np@ <= u32::MAX,
        g0.table.lookup(page) matches Some(d) ==> is_key(g0.descs@[d].value(), page as int),
        forall|x: *mut HugeDesc| #[trigger] g0.descs@.contains_key(x) ==> desc_ok(g0.descs@[x].value(), g0.next_page) && keyed(
            g0.table,
            g0.descs@[x].value(),
            x,
        ),
        g0.table.spec_count() <= 2 * g0.next_page,
        g0.stats.run_pages + g0.stats.empty_pages + g0.stats.purged_pages == g0.next_page,
        g0.stats.runs + g0.stats.empty + g0.stats.purged <= g0.next_page,
        elist_ok(g0.empty_head, g0.empty_tail, g0.elist@, g0.descs@),
        g0.all@.no_duplicates(),
        forall|x: *mut HugeDesc| #[trigger] g0.all@.contains(x) <==> g0.descs@.contains_key(x),
        g0.free.map.wf(),
        g0.free.map.spec_nbits() == g0.np@,
{
    let d0 = g0.descs@;
    assert forall|i: int| 0 <= i < g0.all@.len() implies (#[trigger] d0[g0.all@[i]].value()).pages >= 1 && d0[g0.all@[i]].value().state
        <= 2 by {
        assert(g0.all@.contains(g0.all@[i]));
        assert(d0.contains_key(g0.all@[i]));
        assert(desc_ok(d0[g0.all@[i]].value(), g0.next_page));
    }
    lemma_tally_counts(g0.all@, d0, 0);
    assert(stat(g0.stats, 0) == tally(g0.all@, d0, 0));
    assert(stat(g0.stats, 1) == tally(g0.all@, d0, 1));
    assert(stat(g0.stats, 2) == tally(g0.all@, d0, 2));
}

/// Removing `y` from a list without duplicates removes exactly `y`.
proof fn lemma_drop(l: Seq<*mut HugeDesc>, y: *mut HugeDesc)
    requires
        l.no_duplicates(),
        l.contains(y),
    ensures
        l.remove(l.index_of(y)).no_duplicates(),
        l.remove(l.index_of(y)).len() == l.len() - 1,
        forall|x: *mut HugeDesc| #[trigger] l.remove(l.index_of(y)).contains(x) <==> l.contains(x) && x != y,
{
    let j = l.index_of(y);
    let r = l.remove(j);
    assert forall|x: *mut HugeDesc| #[trigger] r.contains(x) <==> l.contains(x) && x != y by {
        if r.contains(x) {
            let i = r.index_of(x);
            let i0 = if i < j { i } else { i + 1 };
            assert(l[i0] == x);
            assert(x != y) by {
                assert(l.no_duplicates());
            }
        }
        if l.contains(x) && x != y {
            let i0 = l.index_of(x);
            assert(i0 != j);
            let i = if i0 < j { i0 } else { i0 - 1 };
            assert(r[i] == x);
        }
    }
    assert forall|i1: int, i2: int| 0 <= i1 < r.len() && 0 <= i2 < r.len() && i1 != i2 implies r[i1] != r[i2] by {
        let j1 = if i1 < j { i1 } else { i1 + 1 };
        let j2 = if i2 < j { i2 } else { i2 + 1 };
        assert(r[i1] == l[j1] && r[i2] == l[j2]);
    }
}

/// The huge retain cap (`huge_retain_max`), at the huge lock's
/// release by [`Huge::free_run`]: `v1` is the value it releases, `v0`
/// the one it locked, `[page, page + n)` the freed run (guard included),
/// `usable` its usable bytes, `cap` the `retain_max` it read, `pof`
/// `free_run`'s binding of what its `purge_on_free` read returned (read
/// only at or below the cap, false when unread), `now` its clock reading,
/// `discarded` whether `purge::discard_one` of the run's pages returned
/// true, and `[at, at + len)` those pages' bytes.
///
/// The first conjunct checks `free_run`'s cap comparison: the run is
/// discarded only when `usable > cap` or when the flag read returned
/// true. The values of `cap` and the flag are unconstrained, because
/// `retain_max` and `purge_on_free` are plain atomics with no spec view.
/// The next two say what the kernel answered (timelessly: some such
/// `madvise` was answered so, `purge::discard_taken` and
/// `purge::discard_refused`): discarded, a discard advice on the run's
/// bytes was answered with success; not discarded when the run is above
/// the cap or the flag read returned true, one on a range holding them
/// was answered with an error. So a run above the cap is discarded
/// unless some discard advice on its bytes was refused. Discarded, the
/// run's pages lie in a free extent that is `Purged` with no age, or
/// `Empty` with the age of an `Empty` extent of `v0` touching the run. Otherwise, the extent is
/// `Empty`, aged `now` or such a touching extent's age.
spec fn retain_cap(
    v0: Inner,
    v1: Inner,
    page: int,
    n: int,
    usable: int,
    cap: int,
    pof: bool,
    now: u64,
    discarded: bool,
    at: int,
    len: int,
) -> bool {
    &&& discarded ==> (usable > cap || pof)
    &&& discarded ==> crate::purge::discard_taken(at, len)
    &&& (usable > cap || pof) && !discarded ==> crate::purge::discard_refused(at, len)
    &&& exists|x: *mut HugeDesc| #[trigger] v1.descs@.contains_key(x) && {
        let e = v1.descs@[x].value();
        &&& e.start <= page && page + n <= e.start + e.pages
        &&& if discarded {
            (e.state == RunState::Purged as u8 && e.dirty_at == 0) || (e.state == RunState::Empty as u8 && empty_nbr_at(
                v0,
                page,
                n,
                e.dirty_at,
            ))
        } else {
            e.state == RunState::Empty as u8 && (e.dirty_at == now || empty_nbr_at(v0, page, n, e.dirty_at))
        }
    }
}

/// `v0` has an `Empty` extent aged `t` that ends where `[page, page + n)`
/// starts or starts where it ends.
spec fn empty_nbr_at(v0: Inner, page: int, n: int, t: u64) -> bool {
    exists|y: *mut HugeDesc| #[trigger] v0.descs@.contains_key(y) && {
        let e = v0.descs@[y].value();
        &&& e.state == RunState::Empty as u8
        &&& e.dirty_at == t
        &&& (e.start + e.pages == page || e.start == page + n)
    }
}

/// `retain_cap` from what [`Huge::free_run`] knows of the extent `x`
/// holding the freed run: when `kept`, `x` is an `Empty` neighbour in
/// `v0` that kept its age; otherwise `x` is the run's own descriptor,
/// `Purged` with no age when the run was discarded, else `Empty` aged
/// `now`.
proof fn lemma_retain_cap(
    v0: Inner,
    v1: Inner,
    x: *mut HugeDesc,
    kept: bool,
    page: int,
    n: int,
    usable: int,
    cap: int,
    pof: bool,
    now: u64,
    discarded: bool,
    at: int,
    len: int,
)
    requires
        v1.descs@.contains_key(x),
        v1.descs@[x].value().start <= page,
        page + n <= v1.descs@[x].value().start + v1.descs@[x].value().pages,
        discarded ==> (usable > cap || pof),
        discarded ==> crate::purge::discard_taken(at, len),
        (usable > cap || pof) && !discarded ==> crate::purge::discard_refused(at, len),
        kept ==> {
            let e = v0.descs@[x].value();
            &&& v0.descs@.contains_key(x)
            &&& e.state == RunState::Empty as u8
            &&& e.dirty_at == v1.descs@[x].value().dirty_at
            &&& e.start + e.pages == page || e.start == page + n
            &&& v1.descs@[x].value().state == RunState::Empty as u8
        },
        !kept && discarded ==> v1.descs@[x].value().state == RunState::Purged as u8
            && v1.descs@[x].value().dirty_at == 0,
        !kept && !discarded ==> v1.descs@[x].value().state == RunState::Empty as u8
            && v1.descs@[x].value().dirty_at == now,
    ensures
        retain_cap(v0, v1, page, n, usable, cap, pof, now, discarded, at, len),
{
    if kept {
        assert(empty_nbr_at(v0, page, n, v1.descs@[x].value().dirty_at));
    }
}

impl Huge {
    /// Free the run starting at `page` (from `Regions::classify`). Aborts
    /// unless it is the start of a live run. Returns the run as it was
    /// (`start`, `pages` with the guard, `usable`, `arena`), for the
    /// caller's per-arena counters.
    ///
    /// Proved to keep `Inner::wf`: the run and its free neighbours leave
    /// the table and the counters, one of them (the survivor) comes back
    /// as the merged free extent, which covers exactly their pages, and
    /// the others retire; the `Empty` list loses the retired `Empty`
    /// neighbours and gains the run's descriptor when it survives as an
    /// `Empty` extent.
    ///
    /// `wf` is hidden here (the lemmas read it). The body checks in its own
    /// solver instance, with the free-bitmap proof at the
    /// lock release; the rlimit leaves headroom for the cost moving with
    /// unrelated items elsewhere in the crate.
    #[verifier::rlimit(120)]
    #[verifier::spinoff_prover]
    #[verifier::loop_isolation(false)]
    // The coalescing loops are spelled out (see below), not `while let`.
    #[allow(clippy::while_let_loop)]
    pub fn free_run(&self, meta: &Meta, page: usize, Tracked(tok): Tracked<RunToken>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: RunInfo)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            self.is_process(),
            meta.is_process(),
            meta.live_id() == self.spec_mid(),
            tok.wf(),
            tok.start() == page,
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r.pages <= u32::MAX,
            r.pages == tok.npages(),
    {
        hide(Inner::wf);
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let page_size = self.page_size();
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        let ghost g0 = g@;
        let ghost d0 = g0.descs@;
        let ghost t0 = g0.table;
        proof {
            lemma_free_start(g0, page);
        }
        let d = g.table.get(page);
        let _gi: &Inner = &g;
        proof {
            // The token's run is the `Active` descriptor the table maps
            // `page` to (as in `lookup`): none of the three checks below
            // can fail.
            let tracked t = &tok;
            self.lemma_process_runs();
            self.lemma_run_ids(g@);
            theorem_run_counted(_gi, t);
            lemma_run_auths(g@);
            lemma_active(g@.descs@);
            let x = choose|x: *mut HugeDesc|
                #![trigger g@.descs@.contains_key(x)]
                #![trigger run_at(g@.descs@[x].value(), (t.start(), t.npages()))]
                g@.descs@.contains_key(x) && run_at(g@.descs@[x].value(), (t.start(), t.npages()));
            assert(keyed(g@.table, g@.descs@[x].value(), x));
            assert(d == x);
            assert(state_of(g@.descs@[x].value().state) == RunState::Active);
        }
        if d.is_null() {
            proof {
                // The table maps `page` to the token's run. (Unreachable,
                // so the address `self.addr(page)` of an unvalidated page is
                // never computed.)
                assert(false);
            }
            not_a_run(self.addr(page));
        }
        let ghost vd = d0[d].value();
        proof {
            assert(d0.contains_key(d));
            assert(is_key(vd, page as int));
            assert(desc_ok(vd, g0.next_page));
            lemma_key_inside(vd, page as int);
        }
        // Table entries are live descriptors.
        let (start, n, state, usable, arena) =
            (g.desc(d).start(), g.desc(d).pages(), g.desc(d).state(), g.desc(d).usable, g.desc(d).arena());
        if state != RunState::Active {
            proof {
                assert(false);
            }
            already_free(self.addr(page));
        }
        if start != page {
            proof {
                assert(false);
            }
            not_a_run(self.addr(page));
        }
        // The run's token: its extent is this run's (one `Active` extent
        // starts at `page`), and it leaves the run authorities.
        let ghost tpages = tok.pages@;
        let _gm: &mut Inner = &mut g;
        proof {
            lemma_run_auths(g0);
            self.lemma_run_ids(g0);
            tok.ext.agree(_gm.runs.borrow());
            lemma_token_extent(g0, d, tok.start(), tok.npages());
            assert(n == tok.npages());
            let tracked RunToken { ext, pages } = tok;
            _gm.runs.borrow_mut().delete_singleton(ext);
            _gm.pgs.borrow_mut().delete(pages);
        }
        let addr = self.addr(page);
        proof {
            let p = pow2(self.lg_page as nat);
            self.lemma_pages((page + n) as usize);
            assert((page + n) * p == page * p + (n - 1) * p + p) by (nonlinear_arith)
                requires
                    n >= 1,
            ;
            assert(n * p == (n - 1) * p + p) by (nonlinear_arith);
        }
        let guard_addr = addr + (n - GUARD_PAGES) * page_size;
        proof {
            // The freed run's guard page is apart from every other live
            // run's guard (`spares_guards`), and the run's token is back
            // (`held_by`): the descriptors are still `g0`'s, and the
            // token's extent left the run authority above.
            lemma_freed_guard_apart(*self, g0, d);
            assert(g@.descs@ == g0.descs@);
            assert(!g@.runs@@.contains((vd.start as int, vd.pages as int)));
            let p = pow2(self.lg_page as nat) as int;
            assert(guard_addr == self.base + (page + n - GUARD_PAGES) * p) by (nonlinear_arith)
                requires
                    guard_addr == addr + (n - GUARD_PAGES) * page_size,
                    addr == self.base + page * p,
                    page_size == p,
                    n >= GUARD_PAGES;
        }
        // Nested, not a `let` chain: Verus does not take `let` chains.
        #[allow(clippy::collapsible_if)]
        if self.guard_install {
            // Not a resource failure: `MADV_GUARD_REMOVE` takes locked
            // VMAs and allocates nothing, and a `PROT_NONE` guard
            // (`mixed_guards`) has no marker to remove, so on our own
            // anonymous mapping it fails only when the program unmapped
            // (`ENOMEM`) or sealed (`EPERM`) part of the huge region: an
            // inconsistency, as for the purge (design 14.1).
            if let Err(e) = guard_remove(guard_addr, GUARD_PAGES * page_size, Ghost(*self), g.perm(), Ghost(d)) {
                sys::abort_kernel(format_args!("huge: madvise(MADV_GUARD_REMOVE) of {addr:#x} failed: {e}"));
            }
        }
        if self.guards && (!self.guard_install || self.mixed_guards.load(Ordering::Relaxed)) {
            // A `PROT_NONE` guard (fallback mode, or a refused marker)
            // splits the VMA; made read-write again it merges back into
            // the free extent, so the VMAs the huge tier holds stay
            // bounded by its live runs (design 7). Best effort: the
            // change only merges VMAs, and if it fails anyway the page
            // stays `PROT_NONE`, `mixed_guards` is set, and from then on
            // every allocation commits its own range (`prepare`), which
            // undoes it when the page is reused.
            if commit(guard_addr, GUARD_PAGES * page_size, Ghost(*self), g.perm(), Ghost(d)).is_err() {
                self.mixed_guards.store(true, Ordering::Relaxed);
            }
        }
        let now = sys::now_coarse_ns().max(1);
        // Purged at once: above `huge_retain_max`, or under a zero huge
        // decay period (`huge_dirty_decay_ms:0`). A run the discard fails
        // on is kept `Empty` for decay or the next explicit purge.
        // The flag is read only at or below the cap; `pof` is false when
        // it is not read.
        let cap = self.retain_max();
        let over = usable > cap;
        let pof = !over && self.purge_on_free();
        let purge = over || pof;
        // The run's pages left the run authority with its token above:
        // they are no live run's.
        let ghost held = g@.pgs@@;
        let ghost free = |b: int| self.purgeable(held, b);
        proof {
            let p = self.spec_page();
            let base = self.base as int;
            assert(page + n <= g0.next_page <= g0.np@ == self.npages);
            assert forall|b: int| addr <= b < addr + n * page_size implies #[trigger] free(b) by {
                crate::purge::lemma_unit_of(base, p, page as int, n as int, b);
                assert(tpages.contains((b - base) / p));
            }
            // Page-aligned run pages inside the huge region.
            self.lemma_purge_range(page as int, n as int);
        }
        let st = if purge && purge::discard_one(addr, n * page_size, Ghost(free)) {
            RunState::Purged
        } else {
            RunState::Empty
        };
        // Ghost: whether the run was discarded here.
        let ghost discarded = st == RunState::Purged;
        // Best effort: a fuller table still works, only slower.
        proof {
            g0.lemma_mid();
        }
        let _ = g.table.ensure(meta, page_size, 2, Tracked(&mut under));
        proof {
            // The run is counted.
            lemma_can_sub(g0, d);
        }
        g.drop_extent(d, Ghost(Seq::empty()));
        let ghost gb = g@;

        // Coalesce with free neighbours into one extent `[lo, hi)`. The
        // survivor is the `Empty` participant with the oldest `dirty_at`
        // (it keeps its place in the FIFO list), else `d`; the other
        // descriptors go back to `meta`. A neighbour whose discards are
        // refused (locked pages) absorbs the run all the same, which then
        // stays dirty with it until the unlock or a reuse (design 8).
        let pred = g.free_before(page);
        let succ = g.free_after(page + n);
        let ghost m = Merge {
            d,
            vd,
            pred,
            has_pred: pred@.addr != 0,
            vp: d0[pred].value(),
            succ,
            has_succ: succ@.addr != 0,
            vs: d0[succ].value(),
            surv: d,
            vm: vd,
        };
        proof {
            assert(m.parts_ok(g0));
            // The neighbours are every free extent touching the run: each
            // is still under its keys once the run left the table.
            lemma_free_keyed(g0, gb.table, d);
            assert(m.nbrs_ok(g0));
            if m.has_pred {
                assert(desc_ok(m.vp, g0.next_page));
                assert(keyed(t0, m.vp, pred));
            }
            if m.has_succ {
                assert(desc_ok(m.vs, g0.next_page));
                assert(keyed(t0, m.vs, succ));
            }
            // The counters still hold both neighbours.
            assert forall|j: int| 0 <= j < 6 implies #[trigger] stat(gb.stats, j) >= m.nb_w(2, j) - m.nb_w(0, j) by {
                lemma_parts_counted(g0, m, j);
            }
        }
        let mut lo = page;
        let mut hi = page + n;
        let mut merged = st;
        let mut survivor = d;
        // `for nb in [pred, succ]`, spelled out: Verus rejects `continue`
        // in a `for` loop, and the spelled-out loop keeps the machine code
        // of the `for`.
        let mut iter = [pred, succ].into_iter();
        let ghost mut k: int = 0;
        loop
            invariant
                g.spec_lock() == self.inner,
                0 <= k <= 2,
                array_iter_rest(iter) == seq![pred, succ].skip(k),
                g@ == (Inner { table: g@.table, stats: g@.stats, ..gb }),
                g@.table.spec_mid() == gb.table.spec_mid(),
                g@.table.wf(),
                forall|key: usize| #[trigger] g@.table.lookup(key) == if m.nb_key(k, key as int) {
                    None
                } else {
                    gb.table.lookup(key)
                },
                g@.table.spec_count() == gb.table.spec_count() - m.nb_w(k, 6),
                forall|j: int| 0 <= j < 6 ==> #[trigger] stat(g@.stats, j) == stat(gb.stats, j) - m.nb_w(k, j),
                forall|j: int| 0 <= j < 6 ==> #[trigger] stat(gb.stats, j) >= m.nb_w(2, j) - m.nb_w(0, j),
                g@.stats.next_page == gb.stats.next_page,
                lo == m.nb_lo(k),
                hi == m.nb_hi(k),
                merged == (if m.nb_empty(k) { RunState::Empty } else { st }),
                survivor == d <==> !m.nb_empty(k),
                survivor != d ==> (k >= 1 && m.has_pred && survivor == pred && m.vp.state == RunState::Empty as u8) || (k
                    >= 2 && m.has_succ && survivor == succ && m.vs.state == RunState::Empty as u8),
                k >= 1 && m.has_pred ==> pred@.addr != d@.addr,
                k >= 2 && m.has_succ ==> succ@.addr != d@.addr && (m.has_pred ==> succ@.addr != pred@.addr),
            decreases 2 - k,
        {
            let nb = match iter.next() {
                Some(nb) => nb,
                None => {
                    proof {
                        assert(seq![pred, succ].skip(k).len() == 2 - k);
                    }
                    break;
                },
            };
            proof {
                assert(seq![pred, succ].skip(k)[0] == seq![pred, succ][k]);
                k = k + 1;
            }
            if nb.is_null() {
                continue;
            }
            proof {
                assert(k == 1 ==> nb == pred);
                assert(k == 2 ==> nb == succ);
                let v = d0[nb].value();
                assert(g@.table.lookup(v.start as usize) == Some(nb));
                assert(v.pages > 1 ==> g@.table.lookup((v.start + v.pages - 1) as usize) == Some(nb));
                // The counters hold `nb`: what is left of them holds the
                // neighbours not yet taken out.
                let sk = v.state as int;
                assert(v == if k == 1 { m.vp } else { m.vs });
                assert(desc_ok(v, g0.next_page));
                assert(sk == 1 || sk == 2);
                assert(weight(v, sk) == 1 && weight(v, sk + 3) == v.pages);
                assert(stat(gb.stats, sk) >= m.nb_w(2, sk) - m.nb_w(0, sk));
                assert(stat(gb.stats, sk + 3) >= m.nb_w(2, sk + 3) - m.nb_w(0, sk + 3));
                assert(stat(g@.stats, sk) == stat(gb.stats, sk) - m.nb_w(k - 1, sk));
                assert(stat(g@.stats, sk + 3) == stat(gb.stats, sk + 3) - m.nb_w(k - 1, sk + 3));
                assert(stat(g@.stats, sk) >= 1 && stat(g@.stats, sk + 3) >= v.pages);
                assert(g@.stats.can_sub(state_of(v.state), v.pages as usize));
            }
            // Neighbours are live free extents.
            let (ns, ne, nst, ndirty) = (g.desc(nb).start(), g.desc(nb).end(), g.desc(nb).state(), g.desc(nb).dirty_at);
            let ghost others = seq![d, pred];
            g.drop_extent(nb, Ghost(others));
            proof {
                assert(others[0] == d);
                assert(others[1] == pred);
            }
            lo = lo.min(ns);
            hi = hi.max(ne);
            if nst == RunState::Empty {
                merged = RunState::Empty;
                // A survivor other than `d` is a live `Empty` neighbour.
                if survivor == d || ndirty < g.desc(survivor).dirty_at {
                    survivor = nb;
                }
            }
        }
        let ghost ge = g@;
        let ghost m2 = Merge { surv: survivor, ..m };
        // `for x in [d, pred, succ]`, spelled out as above.
        let mut iter = [d, pred, succ].into_iter();
        let ghost mut k2: int = 0;
        loop
            invariant
                g.spec_lock() == self.inner,
                under.allows(rank_meta()),
                0 <= k2 <= 3,
                array_iter_rest(iter) == seq![d, pred, succ].skip(k2),
                g@.mid == g0.mid,
                meta.live_id() == g0.mid@,
                forall|x: *mut HugeDesc| #[trigger] g@.descs@.contains_key(x) ==> desc_claimed(g@.dclaims@, x, g0.mid@),
                perms_ok(g@.descs@),
                elist_ok(g@.empty_head, g@.empty_tail, g@.elist@, g@.descs@),
                forall|x: *mut HugeDesc| #[trigger] g@.descs@.contains_key(x) <==> d0.contains_key(x) && !m2.gone(k2, x),
                forall|x: *mut HugeDesc| #[trigger] g@.descs@.contains_key(x) ==> same_but_links(d0[x].value(), g@.descs@[x].value()),
                g@.all@.no_duplicates(),
                forall|x: *mut HugeDesc| #[trigger] g@.all@.contains(x) <==> g@.descs@.contains_key(x),
                forall|j: int| 0 <= j < 7 ==> #[trigger] tally(g@.all@, d0, j) == tally(g0.all@, d0, j) - m2.gone_w(k2, j),
                forall|x: *mut HugeDesc| #[trigger] g@.elist@.contains(x) <==> g0.elist@.contains(x) && !m2.gone(k2, x),
                g@.elist@.len() == g0.elist@.len() - m2.gone_w(k2, 1),
                epages(g@.elist@, d0) == epages(g0.elist@, d0) - m2.gone_w(k2, 4),
                g@.free == ge.free,
                g@.table == ge.table,
                g@.next_page == ge.next_page,
                g@.stats == ge.stats,
                g@.np == ge.np,
                g@.lg == ge.lg,
                g@.base == ge.base,
                g@.runs == ge.runs,
                g@.pgs == ge.pgs,
            decreases 3 - k2,
        {
            let x = match iter.next() {
                Some(x) => x,
                None => {
                    proof {
                        assert(seq![d, pred, succ].skip(k2).len() == 3 - k2);
                    }
                    break;
                },
            };
            proof {
                assert(seq![d, pred, succ].skip(k2)[0] == seq![d, pred, succ][k2]);
            }
            if !x.is_null() && x != survivor {
                proof {
                    lemma_retire_ok(g0, g@, m2, k2, x);
                }
                let ghost gr = g@;
                g.retire(meta, x, Tracked(&mut under));
                proof {
                    lemma_retired(g0, gr, g@, m2, k2, x);
                }
            }
            proof {
                if !(x@.addr != 0 && x != survivor) {
                    lemma_parts_distinct(g0, m2);
                    assert(seq![d, pred, succ][0] == d);
                    assert(seq![d, pred, succ][1] == pred);
                    assert(seq![d, pred, succ][2] == succ);
                    assert forall|y: *mut HugeDesc| #[trigger] m2.gone(k2 + 1, y) <==> m2.gone(k2, y) by {}
                    assert forall|j: int| #[trigger] m2.gone_w(k2 + 1, j) == m2.gone_w(k2, j) by {}
                }
            }
            proof {
                k2 = k2 + 1;
            }
        }
        let ghost gr = g@;
        proof {
            lemma_parts_distinct(g0, m2);
            assert forall|x: *mut HugeDesc| #[trigger] m2.gone(3, x) <==> m2.part(x) && x != survivor by {}
            assert(gr.descs@.contains_key(survivor));
            assert(desc_ok(vd, g0.next_page));
            if m.has_pred {
                assert(desc_ok(m.vp, g0.next_page));
            }
            if m.has_succ {
                assert(desc_ok(m.vs, g0.next_page));
            }
            assert(lo < hi <= g0.next_page);
            assert(!gr.elist@.contains(d)) by {
                if g0.elist@.contains(d) {
                    assert(enode(g0.elist@, d0, g0.elist@.index_of(d)));
                }
            }
        }
        // `survivor` is live: `d` (still the run's descriptor) or an
        // `Empty` neighbour that keeps its age and list position.
        g.desc_mut(survivor).start = lo as u32;
        g.desc_mut(survivor).pages = (hi - lo) as u32;
        g.desc_mut(survivor).state = merged as u8;
        g.desc_mut(survivor).usable = 0;
        let ghost gw = g@;
        proof {
            assert(gw.descs@ =~= gr.descs@.insert(survivor, gw.descs@[survivor]));
            assert(lo < u32::MAX);
            assert(same_but_links(gw.descs@[survivor].value(), HugeDesc {
                start: lo as u32,
                pages: (hi - lo) as u32,
                state: merged as u8,
                usable: 0,
                ..gr.descs@[survivor].value()
            }));
            assert(gw.descs@[survivor].value().prev == gr.descs@[survivor].value().prev);
            assert(gw.descs@[survivor].value().next == gr.descs@[survivor].value().next);
            assert(gw.descs@[survivor].value().dirty_at == gr.descs@[survivor].value().dirty_at);
            assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, gw.descs@, i) by {
                assert(enode(gr.elist@, gr.descs@, i));
            }
            assert(same_but_links(d0[survivor].value(), gr.descs@[survivor].value()));
        }
        if merged == RunState::Purged {
            g.desc_mut(survivor).dirty_at = 0;
            proof {
                assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, g@.descs@, i) by {
                    assert(enode(gr.elist@, gw.descs@, i));
                }
            }
        } else if survivor == d {
            g.desc_mut(survivor).dirty_at = now;
            proof {
                assert forall|i: int| 0 <= i < gr.elist@.len() implies #[trigger] enode(gr.elist@, g@.descs@, i) by {
                    assert(enode(gr.elist@, gw.descs@, i));
                }
            }
            g.link_tail(d);
        }
        let ghost gl = g@;
        proof {
            let vw = gw.descs@[survivor].value();
            let vl = gl.descs@[survivor].value();
            assert(gl.descs@.contains_key(survivor));
            assert(gl.descs@.dom() =~= gr.descs@.dom());
            assert(vl.start == vw.start && vl.pages == vw.pages && vl.state == vw.state && vl.usable == vw.usable);
            assert(lo == m2.nb_lo(2) && hi == m2.nb_hi(2));
            assert forall|key: usize| lo <= key < hi implies #[trigger] gl.table.lookup(key) is None by {
                lemma_merged_room(g0, m2, key);
            }
            let v = gl.descs@[survivor].value();
            assert(v.start == lo && v.pages == hi - lo);
            assert(stat(gl.stats, 1) <= stat(g0.stats, 1) && stat(gl.stats, 2) <= stat(g0.stats, 2));
            assert(stat(gl.stats, 4) <= stat(g0.stats, 4) && stat(gl.stats, 5) <= stat(g0.stats, 5));
        }
        g.free.map.set_range(page, n);
        g.add_extent(survivor);
        proof {
            let m3 = Merge { vm: g@.descs@[survivor].value(), ..m2 };
            assert(m3.surv_ok());
            assert forall|key: usize| #[trigger] gr.table.lookup(key) == if is_key(vd, key as int) || m3.nb_key(2, key as int) {
                None
            } else {
                t0.lookup(key)
            } by {}
            assert(g0.free.map.in_meta() && g0.free.own_claimed() && g0.free.l1_words <= 0x1_0000_0000) by {
                reveal(Inner::wf);
            }
            assert(gl.free.map.place() == g0.free.map.place());
            assert(gl.free == g0.free);
            assert(freed_free(g0.free.map.view(), g@.free.map.view(), vd));
            assert(m3.nbrs_ok(g0));
            lemma_free_end(g0, gr, gl, g@, m3);
            // The lock's frame: the ghost page count, shift, base and `Meta`
            // identity are the ones `new` set.
            assert(lock_frame(lock_init(self.inner), g@));
            // The retain cap holds of the value the lock is released
            // with.
            assert(g@.descs@.contains_key(survivor));
            lemma_retain_cap(
                g0,
                g@,
                survivor,
                survivor != d,
                page as int,
                n as int,
                usable as int,
                cap as int,
                pof,
                now,
                discarded,
                addr as int,
                (n * page_size) as int,
            );
        }
        let first = self.sync_oldest(&g);
        g.unlock();
        if first {
            purge::note_empty();
        }
        RunInfo { start, pages: n, usable, arena, state: RunState::Active, dirty_at: 0 }
    }
}

impl Huge {
    /// `r` is the usable size of a run starting at `page`: whole pages, at
    /// least one, with the run's guard after them inside the region.
    pub open spec fn spec_run_usable(&self, page: usize, r: usize) -> bool {
        &&& r >= self.spec_page()
        &&& (r as int) % self.spec_page() == 0
        &&& page * self.spec_page() + r + GUARD_PAGES * self.spec_page() <= self.spec_npages() * self.spec_page()
    }

    /// Usable bytes of the live run starting at `page`. Aborts otherwise.
    /// The result is the bytes of the run's usable pages, as its descriptor
    /// counts them (`Huge::take` writes `usable` from the pages it commits):
    /// the usable pages of the run token `tok` holds, if it is this run's.
    /// With that token neither abort can fire, so the caller needs only
    /// `valid_aborts`.
    pub fn usable_size(&self, page: usize, Tracked(tok): Tracked<&Option<RunToken>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && holds_run(*tok, page as int)),
            crate::sys::may_syscall(),
            page <= self.spec_npages(),
            self.runs_process(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            page < self.spec_npages(),
            self.spec_run_usable(page, r),
            tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page ==> r == (tok.unwrap().npages() - GUARD_PAGES) * self.spec_page(),
    {
        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let d = g.table.get(page);
        let _gi: &Inner = &g;
        proof {
            self.lemma_run_ids(g@);
        }
        proof {
            // As in `lookup`: the token's run is the `Active` descriptor
            // the table maps `page` to.
            if holds_run(*tok, page as int) {
                let tracked t = tok.tracked_borrow();
                theorem_run_counted(_gi, t);
                lemma_run_auths(g@);
                lemma_active(g@.descs@);
                let x = choose|x: *mut HugeDesc|
                    #![trigger g@.descs@.contains_key(x)]
                    #![trigger run_at(g@.descs@[x].value(), (t.start(), t.npages()))]
                    g@.descs@.contains_key(x) && run_at(g@.descs@[x].value(), (t.start(), t.npages()));
                assert(keyed(g@.table, g@.descs@[x].value(), x));
                assert(d == x);
                assert(state_of(g@.descs@[x].value().state) == RunState::Active);
            }
        }
        // Table entries are live descriptors.
        if d.is_null() || g.desc(d).start() != page {
            proof {
                assert(!holds_run(*tok, page as int));
            }
            not_a_run(self.addr(page));
        }
        if g.desc(d).state() != RunState::Active {
            proof {
                assert(!holds_run(*tok, page as int));
            }
            already_free(self.addr(page));
        }
        proof {
            // A live descriptor's pages lie below the mark.
            assert(g@.descs@.contains_key(d));
            let v = g@.descs@[d].value();
            assert(desc_ok(v, g@.next_page));
            assert(v.start == page && g@.next_page <= g@.np@);
            // An `Active` descriptor's usable pages and guard are its pages.
            assert(run_ok(v, g@.lg@));
            // A token of this run has its extent ([`lemma_token_extent`]).
            if tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page {
                let tracked t = tok.tracked_borrow();
                t.ext.agree(_gi.runs.borrow());
                lemma_token_extent(g@, d, page as int, t.npages());
            }
            let p = self.spec_page();
            assert(v.usable >= p && (v.usable as int) % p == 0) by {
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(v.pages - GUARD_PAGES, p);
                assert((v.pages - GUARD_PAGES) * p >= 1 * p) by (nonlinear_arith)
                    requires
                        v.pages - GUARD_PAGES >= 1,
                        p > 0,
                ;
            }
            assert(page * p + v.usable + GUARD_PAGES * p <= self.npages * p) by (nonlinear_arith)
                requires
                    v.usable == (v.pages - GUARD_PAGES) * p,
                    page + v.pages <= self.npages,
                    p > 0,
            ;
        }
        let r = g.desc(d).usable;
        g.unlock();
        r
    }

    /// Descriptor of the extent starting or ending at `page`, if any. With
    /// `tok` the token of a live run that starts at `page`, that run's
    /// descriptor: `Active`, starting at `page`, its usable bytes the
    /// token's usable pages (as [`Huge::usable_size`]).
    pub fn lookup(&self, page: usize, Tracked(tok): Tracked<&Option<RunToken>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<RunInfo>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            self.runs_process() && tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page ==> (r matches Some(i) && i.start == page && i.state == RunState::Active && i.usable == (tok.unwrap().npages() - GUARD_PAGES) * self.spec_page()),
    {
        proof {
            use_type_invariant(self);
        }
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let d = g.table.get(page);
        let _gi: &Inner = &g;
        proof {
            // A token of a run starting at `page` names an `Active`
            // descriptor that starts there, which the table maps `page` to.
            if self.runs_process() && tok is Some && tok.unwrap().wf() && tok.unwrap().start() == page {
                let tracked t = tok.tracked_borrow();
                self.lemma_run_ids(g@);
                theorem_run_counted(_gi, t);
                lemma_run_auths(g@);
                lemma_active(g@.descs@);
                let x = choose|x: *mut HugeDesc|
                    #![trigger g@.descs@.contains_key(x)]
                    #![trigger run_at(g@.descs@[x].value(), (t.start(), t.npages()))]
                    g@.descs@.contains_key(x) && run_at(g@.descs@[x].value(), (t.start(), t.npages()));
                assert(keyed(g@.table, g@.descs@[x].value(), x));
                assert(d == x);
                assert(run_ok(g@.descs@[x].value(), g@.lg@));
                let v = g@.descs@[x].value();
                assert(v.state == RunState::Active as u8 && v.start == page && v.pages == t.npages());
                assert(state_of(v.state) == RunState::Active);
                assert(g@.lg@ == self.lg_page);
                assert(v.usable == (t.npages() - GUARD_PAGES) * self.spec_page());
            }
        }
        // Table entries are live descriptors.
        let r = if d.is_null() {
            None
        } else {
            Some(RunInfo {
                start: g.desc(d).start(),
                pages: g.desc(d).pages(),
                usable: g.desc(d).usable,
                arena: g.desc(d).arena,
                state: g.desc(d).state(),
                dirty_at: g.desc(d).dirty_at,
            })
        };
        g.unlock();
        r
    }
}

} // verus!
