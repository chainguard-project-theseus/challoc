//! Shrinking a live run in place (design 7): [`Huge::shrink_run`] and the
//! [`Split`] model of what it changes, with the lemmas that keep
//! `Inner::wf` across it.
//!
//! A buffer trimmed to its final length (Ruby's `IO#read` and
//! `String#resize`, CPython's `bytes` and `bytearray` once a read or a
//! join is done) is reallocated to fewer pages. A move would copy every
//! byte it keeps; [`Huge::shrink_run`] keeps the run where it is
//! instead. It splits the run in two under the huge lock: the run keeps
//! its descriptor and start with the page count a new run of the
//! requested size has (so the sized frees' rule holds), its new guard
//! goes on its new last page, and the pages after it become a second run
//! with a descriptor of its own, which still ends with the old guard.
//! Then, once the caller (`Shared::shrink_huge`) has given up the
//! permission to its bytes, that tail is freed by [`Huge::free_run`], as
//! any run is: its
//! guard marker removed, coalesced with a free extent after it, purged at
//! once above `huge_retain_max` or under `huge_dirty_decay_ms:0`, else
//! `Empty` and aging in the decay list.

use super::*;

verus! {

/// What [`Huge::shrink_books`] changes in the huge lock's value: the live
/// run `d` (`vd`) becomes `vs`, the same run with fewer pages, and the
/// pages it gave up become the new run `y` (`vt`), right after it.
#[allow(dead_code)]
ghost struct Split {
    d: *mut HugeDesc,
    vd: HugeDesc,
    vs: HugeDesc,
    y: *mut HugeDesc,
    vt: HugeDesc,
}

impl Split {
    /// The descriptor at `x` afterwards, up to its list links.
    spec fn model(self, d0: Map<*mut HugeDesc, PointsTo<HugeDesc>>, x: *mut HugeDesc) -> Option<HugeDesc> {
        if x == self.d {
            Some(self.vs)
        } else if x == self.y {
            Some(self.vt)
        } else if d0.contains_key(x) {
            Some(d0[x].value())
        } else {
            None
        }
    }

    /// The table's entry for `k` afterwards: the run keeps its one key,
    /// and the tail's comes.
    spec fn lookup(self, t0: Table, k: usize) -> Option<*mut HugeDesc> {
        if k == self.vt.start {
            Some(self.y)
        } else {
            t0.lookup(k)
        }
    }

    /// What tally `k` gains.
    spec fn delta(self, k: int) -> int {
        weight(self.vs, k) - weight(self.vd, k) + weight(self.vt, k)
    }

    /// The split, against the state `g0` it starts from: the run, shorter,
    /// and the tail, a run of the pages it gave up, at a descriptor that
    /// is not live.
    spec fn shape_ok(self, g0: Inner) -> bool {
        let d0 = g0.descs@;
        &&& d0.contains_key(self.d)
        &&& self.vd == d0[self.d].value()
        &&& self.vd.state == RunState::Active as u8
        &&& !d0.contains_key(self.y)
        &&& self.y@.addr != 0
        &&& same_but_links(HugeDesc { pages: self.vs.pages, usable: self.vs.usable, ..self.vd }, self.vs)
        &&& run_ok(self.vs, g0.lg@)
        &&& self.vt.state == RunState::Active as u8
        &&& self.vt.start == self.vd.start + self.vs.pages
        &&& self.vt.pages == self.vd.pages - self.vs.pages
        &&& run_ok(self.vt, g0.lg@)
    }
}

impl Inner {
    /// `self` is `g0` split by `t`, but for the run authorities.
    spec fn split(&self, g0: Inner, t: Split) -> bool {
        &&& t.shape_ok(g0)
        &&& self.table.wf()
        &&& self.table.spec_count() == g0.table.spec_count() + t.delta(6)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) == t.lookup(g0.table, k)
        &&& perms_ok(self.descs@)
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) ==> desc_claimed(self.dclaims@, y, self.mid@)
        &&& self.table.spec_mid() == self.mid@
        &&& self.mid == g0.mid
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) <==> t.model(g0.descs@, y) is Some
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) ==> same_but_links(
            t.model(g0.descs@, y).unwrap(),
            self.descs@[y].value(),
        )
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) && y != t.d && y != t.y ==> self.descs@[y]
            == g0.descs@[y]
        &&& self.all@ == g0.all@.push(t.y)
        &&& forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == stat(g0.stats, k) + t.delta(k)
        &&& self.next_page == g0.next_page
        &&& self.stats.next_page == self.next_page
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.base == g0.base
        &&& self.free == g0.free
        &&& self.elist@ == g0.elist@
        &&& self.empty_head == g0.empty_head
        &&& self.empty_tail == g0.empty_tail
    }
}

/// An old extent other than the run shares no page with the shortened
/// run or the tail: both lie inside the run's old pages.
proof fn lemma_split_apart(g0: Inner, t: Split, x: *mut HugeDesc)
    requires
        g0.wf(),
        t.shape_ok(g0),
        g0.descs@.contains_key(x),
        x != t.d,
    ensures
        apart(g0.descs@[x].value(), t.vd),
        apart(g0.descs@[x].value(), t.vs),
        apart(g0.descs@[x].value(), t.vt),
        x != t.y,
{
    assert(apart(g0.descs@[x].value(), t.vd));
}

/// The extents after a split: inside the mark, disjoint, each under
/// exactly its keys.
proof fn lemma_split_extents(g0: Inner, g1: Inner, t: Split)
    requires
        g0.wf(),
        g1.split(g0, t),
    ensures
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> desc_ok(g1.descs@[y].value(), g1.next_page),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> run_ok(g1.descs@[y].value(), g1.lg@),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(y) ==> g1.descs@.contains_key(y),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> keyed(g1.table, g1.descs@[y].value(), y),
        forall|k: usize| #[trigger] g1.table.lookup(k) matches Some(y) ==> is_key(g1.descs@[y].value(), k as int),
        forall|y1: *mut HugeDesc, y2: *mut HugeDesc|
            #[trigger] g1.descs@.contains_key(y1) && #[trigger] g1.descs@.contains_key(y2) && y1 != y2 ==> apart(
                g1.descs@[y1].value(),
                g1.descs@[y2].value(),
            ),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let t0 = g0.table;
    assert(desc_ok(t.vd, g0.next_page));
    assert(keyed(t0, t.vd, t.d));
    assert(t.model(d0, t.d) == Some(t.vs));
    assert(d1.contains_key(t.d) && same_but_links(t.vs, d1[t.d].value()));
    assert(t.y != t.d);
    assert(t.model(d0, t.y) == Some(t.vt));
    assert(t.model(d0, t.y) is Some);
    assert(d1.contains_key(t.y));
    assert(same_but_links(t.vt, d1[t.y].value()));
    // Which descriptors are old.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && y != t.d && y != t.y implies d0.contains_key(y)
        && same_but_links(d0[y].value(), d1[y].value()) by {
        assert(t.model(d0, y) is Some);
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) implies desc_ok(d1[y].value(), g1.next_page) && run_ok(
        d1[y].value(),
        g1.lg@,
    ) by {
        if y != t.d && y != t.y {
            assert(desc_ok(d0[y].value(), g0.next_page));
            assert(run_ok(d0[y].value(), g0.lg@));
        }
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) implies keyed(g1.table, d1[y].value(), y) by {
        let v = d1[y].value();
        let k1 = v.start as usize;
        let k2 = (v.start + v.pages - 1) as usize;
        assert(g1.table.lookup(k1) == t.lookup(t0, k1));
        assert(g1.table.lookup(k2) == t.lookup(t0, k2));
        if y == t.d {
            assert(k1 != t.vt.start);
        } else if y == t.y {
        } else {
            lemma_split_apart(g0, t, y);
            assert(keyed(t0, d0[y].value(), y));
            assert(desc_ok(d0[y].value(), g0.next_page));
        }
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies d1.contains_key(g1.table.lookup(k).unwrap()) && is_key(
        d1[g1.table.lookup(k).unwrap()].value(),
        k as int,
    ) by {
        assert(g1.table.lookup(k) == t.lookup(t0, k));
        let y = g1.table.lookup(k).unwrap();
        if k != t.vt.start {
            assert(t0.lookup(k) == Some(y));
            assert(d0.contains_key(y) && is_key(d0[y].value(), k as int));
            assert(y != t.y);
            assert(t.model(d0, y) is Some);
            assert(d1.contains_key(y));
        }
    }
    assert forall|y1: *mut HugeDesc, y2: *mut HugeDesc|
        #[trigger] d1.contains_key(y1) && #[trigger] d1.contains_key(y2) && y1 != y2 implies apart(d1[y1].value(), d1[y2].value()) by {
        let new1 = y1 == t.d || y1 == t.y;
        let new2 = y2 == t.d || y2 == t.y;
        if !new1 && !new2 {
            assert(apart(d0[y1].value(), d0[y2].value()));
        } else if !new1 {
            lemma_split_apart(g0, t, y1);
        } else if !new2 {
            lemma_split_apart(g0, t, y2);
        }
    }
}

/// The tallies after a split gain `delta`; `all` stays a list of the live
/// descriptors.
#[verifier::spinoff_prover]
proof fn lemma_split_tally(g0: Inner, g1: Inner, t: Split, k: int)
    requires
        g0.wf(),
        g1.split(g0, t),
    ensures
        tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) + t.delta(k),
        g1.all@.no_duplicates(),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> g1.all@.contains(y),
{
    reveal(Seq::no_duplicates);
    reveal(Seq::index_of);
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let a0 = g0.all@;
    assert(forall|y: *mut HugeDesc| #[trigger] a0.contains(y) <==> d0.contains_key(y));
    assert(a0.contains(t.d));
    assert(!a0.contains(t.y));
    assert(t.model(d0, t.d) == Some(t.vs));
    assert(d1.contains_key(t.d) && same_but_links(t.vs, d1[t.d].value()));
    assert(t.model(d0, t.y) == Some(t.vt));
    assert(t.model(d0, t.y) is Some);
    assert(d1.contains_key(t.y));
    assert(same_but_links(t.vt, d1[t.y].value()));
    // Every old entry but the run keeps its weight.
    assert forall|i: int| 0 <= i < a0.len() && a0[i] != t.d implies #[trigger] weight(d1[a0[i]].value(), k) == weight(
        d0[a0[i]].value(),
        k,
    ) by {
        assert(a0.contains(a0[i]));
        assert(d0.contains_key(a0[i]));
        assert(a0[i] != t.y);
        assert(t.model(d0, a0[i]) == Some(d0[a0[i]].value()));
        assert(d1.contains_key(a0[i]));
    }
    lemma_tally_update(a0, d0, d1, t.d, k);
    lemma_tally_push(a0, d1, t.y, k);
    assert(weight(d1[t.d].value(), k) == weight(t.vs, k));
    assert(weight(d1[t.y].value(), k) == weight(t.vt, k));
    assert(g1.all@.no_duplicates()) by {
        let a1 = g1.all@;
        assert forall|i1: int, i2: int| 0 <= i1 < a1.len() && 0 <= i2 < a1.len() && i1 != i2 implies a1[i1] != a1[i2] by {
            if i1 < a0.len() && i2 < a0.len() {
                assert(a1[i1] == a0[i1] && a1[i2] == a0[i2]);
            } else if i1 < a0.len() {
                assert(a1[i1] == a0[i1] && a0.contains(a0[i1]));
            } else if i2 < a0.len() {
                assert(a1[i2] == a0[i2] && a0.contains(a0[i2]));
            }
        }
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) <==> g1.all@.contains(y) by {
        assert(d1.contains_key(y) <==> t.model(d0, y) is Some);
        if g1.all@.contains(y) && y != t.y {
            let i = g1.all@.index_of(y);
            assert(i < a0.len());
            assert(a0[i] == y);
        }
        if a0.contains(y) {
            let i = a0.index_of(y);
            assert(g1.all@[i] == y);
        }
        if y == t.y {
            assert(g1.all@[a0.len() as int] == y);
        }
    }
}

/// The `Empty` list after a split: the same list, its length and pages
/// the counters, holding every `Empty` extent (neither run is one).
#[verifier::spinoff_prover]
proof fn lemma_split_elist(g0: Inner, g1: Inner, t: Split)
    requires
        g0.wf(),
        g1.split(g0, t),
    ensures
        elist_ok(g1.empty_head, g1.empty_tail, g1.elist@, g1.descs@),
        g1.stats.empty == g1.elist@.len(),
        g1.stats.empty_pages == epages(g1.elist@, g1.descs@),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) && g1.descs@[y].value().state == RunState::Empty as u8
            ==> g1.elist@.contains(y),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let l0 = g0.elist@;
    assert(stat(g1.stats, 1) == stat(g0.stats, 1) + t.delta(1));
    assert(stat(g1.stats, 4) == stat(g0.stats, 4) + t.delta(4));
    assert(t.model(d0, t.d) == Some(t.vs));
    assert(t.model(d0, t.y) == Some(t.vt));
    // The list's members are `Empty`, so neither run, and keep their
    // descriptors.
    assert forall|i: int| 0 <= i < l0.len() implies d1.contains_key(l0[i]) && #[trigger] d1[l0[i]] == d0[l0[i]] by {
        assert(enode(l0, d0, i));
        assert(l0[i] != t.d);
        assert(l0[i] != t.y);
        assert(t.model(d0, l0[i]) is Some);
        assert(d1.contains_key(l0[i]));
    }
    assert forall|i: int| 0 <= i < l0.len() implies #[trigger] enode(l0, d1, i) by {
        assert(enode(l0, d0, i));
        assert(d1.contains_key(l0[i]));
        assert(d1[l0[i]] == d0[l0[i]]);
    }
    assert forall|i: int| 0 <= i < l0.len() implies #[trigger] d1[l0[i]].value().pages == d0[l0[i]].value().pages by {
        assert(d1[l0[i]] == d0[l0[i]]);
    }
    lemma_epages_frame(l0, d0, d1);
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state == RunState::Empty as u8 implies g1.elist@.contains(
        y,
    ) by {
        assert(t.model(d0, y) is Some);
        assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        assert(y != t.d && y != t.y);
        assert(d0.contains_key(y) && d0[y].value().state == RunState::Empty as u8);
    }
}

/// A split replaces the run's extent in the extents' authority by the two
/// runs', and keeps the pages it holds: the two runs hold exactly the
/// run's pages.
proof fn lemma_split_active(g0: Inner, g1: Inner, t: Split)
    requires
        g0.wf(),
        g1.split(g0, t),
    ensures
        ({
            let (old_e, s_e, t_e) = (
                (t.vd.start as int, t.vd.pages as int),
                (t.vs.start as int, t.vs.pages as int),
                (t.vt.start as int, t.vt.pages as int),
            );
            &&& active_runs(g0.descs@).contains(old_e)
            &&& !active_runs(g0.descs@).remove(old_e).contains(s_e)
            &&& !active_runs(g0.descs@).remove(old_e).insert(s_e).contains(t_e)
            &&& active_runs(g1.descs@) == active_runs(g0.descs@).remove(old_e).insert(s_e).insert(t_e)
            &&& active_pages(g1.descs@) == active_pages(g0.descs@)
        }),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    lemma_active(d0);
    lemma_active(d1);
    let old_e = (t.vd.start as int, t.vd.pages as int);
    let s_e = (t.vs.start as int, t.vs.pages as int);
    let t_e = (t.vt.start as int, t.vt.pages as int);
    assert(desc_ok(t.vd, g0.next_page));
    assert(run_at(d0[t.d].value(), old_e));
    assert(t.model(d0, t.d) == Some(t.vs));
    assert(d1.contains_key(t.d) && same_but_links(t.vs, d1[t.d].value()));
    assert(t.model(d0, t.y) == Some(t.vt));
    assert(t.model(d0, t.y) is Some);
    assert(d1.contains_key(t.y));
    assert(same_but_links(t.vt, d1[t.y].value()));
    // An old descriptor other than the run keeps its value and is apart
    // from both runs.
    assert forall|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && y != t.d implies {
        &&& y != t.y
        &&& d1.contains_key(y)
        &&& same_but_links(d0[y].value(), d1[y].value())
        &&& apart(d0[y].value(), t.vd)
        &&& apart(d0[y].value(), t.vs)
        &&& apart(d0[y].value(), t.vt)
    } by {
        lemma_split_apart(g0, t, y);
        assert(t.model(d0, y) == Some(d0[y].value()));
    }
    // A new descriptor is one of the runs or an old one.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && y != t.d && y != t.y implies d0.contains_key(y)
        && same_but_links(d0[y].value(), d1[y].value()) by {
        assert(t.model(d0, y) is Some);
    }
    assert(!active_runs(d0).remove(old_e).contains(s_e)) by {
        if active_runs(d0).remove(old_e).contains(s_e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_at(d0[y].value(), s_e);
            if y == t.d {
                assert(t.vs.pages != t.vd.pages);
            } else {
                assert(apart(d0[y].value(), t.vd));
            }
        }
    }
    assert(!active_runs(d0).remove(old_e).insert(s_e).contains(t_e)) by {
        if active_runs(d0).remove(old_e).contains(t_e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_at(d0[y].value(), t_e);
            if y == t.d {
                assert(t.vt.start != t.vd.start);
            } else {
                assert(apart(d0[y].value(), t.vd));
            }
        }
        assert(s_e != t_e);
    }
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d0).remove(old_e).insert(s_e).insert(t_e).contains(e) by {
        if active_runs(d1).contains(e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && run_at(d1[y].value(), e);
            if y != t.d && y != t.y {
                assert(d0.contains_key(y) && run_at(d0[y].value(), e));
                assert(apart(d0[y].value(), t.vd));
            }
        }
        if active_runs(d0).remove(old_e).contains(e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_at(d0[y].value(), e);
            assert(y != t.d);
            assert(d1.contains_key(y) && run_at(d1[y].value(), e));
        }
        if e == s_e {
            assert(run_at(d1[t.d].value(), e));
        }
        if e == t_e {
            assert(run_at(d1[t.y].value(), e));
        }
    }
    assert(active_runs(d1) =~= active_runs(d0).remove(old_e).insert(s_e).insert(t_e));
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d0).contains(k) by {
        if active_pages(d1).contains(k) {
            let y = choose|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && run_page(d1[y].value(), k);
            if y == t.d || y == t.y {
                assert(run_page(d0[t.d].value(), k));
            } else {
                assert(d0.contains_key(y) && run_page(d0[y].value(), k));
            }
        }
        if active_pages(d0).contains(k) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_page(d0[y].value(), k);
            if y == t.d {
                if k < t.vt.start {
                    assert(run_page(d1[t.d].value(), k));
                } else {
                    assert(run_page(d1[t.y].value(), k));
                }
            } else {
                assert(d1.contains_key(y) && run_page(d1[y].value(), k));
            }
        }
    }
    assert(active_pages(d1) =~= active_pages(d0));
}

/// A split keeps the free extents as they were: both runs are `Active`,
/// and every free extent keeps its descriptor and its pages in the
/// unchanged free-page set.
proof fn lemma_split_coalesced(g0: Inner, g1: Inner, t: Split)
    requires
        g0.wf(),
        g1.split(g0, t),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map@),
        free_exact(g1.descs@, g1.free.map@),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let f = g0.free.map@;
    assert(t.model(d0, t.d) == Some(t.vs));
    assert(t.model(d0, t.y) == Some(t.vt));
    // A free extent afterwards is an old one, with its old value.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state != RunState::Active as u8 implies d0.contains_key(
        y,
    ) && d1[y] == d0[y] by {
        assert(t.model(d0, y) is Some);
        assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        assert(y != t.d && y != t.y);
    }
    assert forall|y1: *mut HugeDesc, y2: *mut HugeDesc|
        #[trigger] d1.contains_key(y1) && #[trigger] d1.contains_key(y2) && d1[y1].value().state != RunState::Active as u8
            && d1[y2].value().state != RunState::Active as u8 implies d1[y1].value().start + d1[y1].value().pages
        != d1[y2].value().start by {
        assert(d0.contains_key(y1) && d0.contains_key(y2));
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state != RunState::Active as u8 implies marked(
        f,
        d1[y].value(),
    ) by {
        assert(d0.contains_key(y));
        assert(marked(f, d0[y].value()));
    }
    assert forall|q: nat| #[trigger] f.contains(q) implies exists|x: *mut HugeDesc| #[trigger] in_free(d1, x, q as int) by {
        let x = choose|x: *mut HugeDesc| #[trigger] in_free(d0, x, q as int);
        assert(x != t.d);
        assert(x != t.y);
        assert(t.model(d0, x) == Some(d0[x].value()));
        assert(d1.contains_key(x));
        assert(d1[x] == d0[x]);
        assert(in_free(d1, x, q as int));
    }
}

/// A split keeps `Inner::wf`, once the run authorities hold the two runs'
/// extents and the pages the run held.
proof fn lemma_split_wf(g0: Inner, g1: Inner, t: Split)
    requires
        g0.wf(),
        g1.split(g0, t),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
    ensures
        g1.wf(),
{
    lemma_split_extents(g0, g1, t);
    lemma_split_elist(g0, g1, t);
    lemma_split_coalesced(g0, g1, t);
    assert forall|k: int| 0 <= k < 7 implies #[trigger] tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) + t.delta(k) by {
        lemma_split_tally(g0, g1, t, k);
    }
    // Once more outside the `forall`, for what it says about `all`.
    lemma_split_tally(g0, g1, t, 0);
    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == tally(g1.all@, g1.descs@, k) by {
        assert(stat(g1.stats, k) == stat(g0.stats, k) + t.delta(k));
        assert(stat(g0.stats, k) == tally(g0.all@, g0.descs@, k));
    }
    assert(g1.table.spec_count() == tally(g1.all@, g1.descs@, 6));
    // The pages sum to the mark: the two runs hold the run's pages.
    assert(t.delta(3) + t.delta(4) + t.delta(5) == 0);
    // Every extent has a page, so the counts are at most the pages.
    let a1 = g1.all@;
    let d1 = g1.descs@;
    assert forall|i: int| 0 <= i < a1.len() implies (#[trigger] d1[a1[i]].value()).pages >= 1 && d1[a1[i]].value().state <= 2 by {
        assert(d1.contains_key(a1[i]));
        assert(desc_ok(d1[a1[i]].value(), g1.next_page));
    }
    lemma_tally_counts(a1, d1, 0);
    lemma_tally_counts(a1, d1, 1);
    lemma_tally_counts(a1, d1, 2);
    assert(stat(g1.stats, 3) + stat(g1.stats, 4) + stat(g1.stats, 5) == stat(g0.stats, 3) + stat(g0.stats, 4) + stat(g0.stats, 5)
        + t.delta(3) + t.delta(4) + t.delta(5));
    assert(g0.stats.run_pages + g0.stats.empty_pages + g0.stats.purged_pages == g0.next_page);
    assert(g1.stats.run_pages + g1.stats.empty_pages + g1.stats.purged_pages == g1.next_page);
    assert(stat(g1.stats, 0) == tally(a1, d1, 0) && stat(g1.stats, 3) == tally(a1, d1, 3));
    assert(stat(g1.stats, 1) == tally(a1, d1, 1) && stat(g1.stats, 4) == tally(a1, d1, 4));
    assert(stat(g1.stats, 2) == tally(a1, d1, 2) && stat(g1.stats, 5) == tally(a1, d1, 5));
    assert(stat(g1.stats, 0) + stat(g1.stats, 1) + stat(g1.stats, 2) <= g1.next_page);
    assert(g1.next_page <= g1.np@ <= u32::MAX);
    assert(forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> desc_claimed(g1.dclaims@, y, g1.mid@));
    assert(g1.table.spec_mid() == g1.mid@);
    assert(g1.mark_committed());
}

/// `wf` survives a change of the table's storage only (`Table::ensure`).
proof fn lemma_wf_table(g0: Inner, g1: Inner)
    requires
        g0.wf(),
        g1 == (Inner { table: g1.table, ..g0 }),
        g1.table.wf(),
        g1.table.spec_count() == g0.table.spec_count(),
        g1.table.spec_mid() == g0.table.spec_mid(),
        forall|k: usize| #[trigger] g1.table.lookup(k) == g0.table.lookup(k),
    ensures
        g1.wf(),
{
    assert forall|d: *mut HugeDesc| #[trigger] g1.descs@.contains_key(d) implies keyed(g1.table, g1.descs@[d].value(), d) by {
        assert(keyed(g0.table, g0.descs@[d].value(), d));
        let v = g0.descs@[d].value();
        assert(g1.table.lookup(v.start as usize) == g0.table.lookup(v.start as usize));
        assert(g1.table.lookup((v.start + v.pages - 1) as usize) == g0.table.lookup((v.start + v.pages - 1) as usize));
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies g1.descs@.contains_key(g1.table.lookup(k).unwrap())
        && is_key(g1.descs@[g1.table.lookup(k).unwrap()].value(), k as int) by {
        assert(g1.table.lookup(k) == g0.table.lookup(k));
    }
}

impl Huge {
    /// [`Huge::shrink_run`]'s bookkeeping, once the kernel's side is done
    /// and the run's token has left the run authorities: the run `d`
    /// (`n` pages from `page`) gets the page count `need` a new run of
    /// its new size has, and the pages after it become a new run at `y`,
    /// a fresh descriptor (from `alloc_desc`) of the same arena. Infallible:
    /// the table has room for the tail's key. A function of its own so
    /// that each proof stays within its rlimit.
    #[verifier::spinoff_prover]
    #[allow(clippy::too_many_arguments)]
    fn shrink_books(
        &self,
        g: &mut Inner,
        page: usize,
        n: usize,
        need: usize,
        usable: usize,
        d: *mut HugeDesc,
        y: *mut HugeDesc,
        Tracked(ydesc): Tracked<(PointsTo<HugeDesc>, crate::meta::MetaClaim)>,
        Ghost(gb): Ghost<Inner>,
    ) -> (r: Ghost<Split>)
        requires
            gb.wf(),
            *old(g) == (Inner { runs: old(g).runs, pgs: old(g).pgs, ..gb }),
            gb.lg@ == self.lg_page,
            gb.np@ == self.npages,
            usable >= 1,
            need == usable + GUARD_PAGES,
            need + GUARD_PAGES < n,
            gb.descs@.contains_key(d),
            gb.descs@[d].value().start == page,
            gb.descs@[d].value().pages == n,
            gb.descs@[d].value().state == RunState::Active as u8,
            y@.addr != 0,
            ydesc.0.ptr() == y,
            desc_claim_for(ydesc.1, y, gb.mid@),
            desc_in_meta(y),
            (gb.table.spec_count() + 1) * 2 <= gb.table.spec_cap(),
        ensures
            final(g).split(gb, r@),
            final(g).runs == old(g).runs,
            final(g).pgs == old(g).pgs,
            r@.d == d,
            r@.y == y,
            r@.vd == gb.descs@[d].value(),
            r@.vs.pages == need,
            r@.vt.start == page + need,
            r@.vt.pages == n - need,
    {
        hide(Inner::wf);
        hide(Seq::no_duplicates);
        proof {
            use_type_invariant(self);
            self.lemma_page();
            gb.lemma_wf_for_take();
            // A descriptor's size, which `is_disjoint` needs nonzero below:
            // the `global layout` fact does not reach a spun-off prover.
            gb.lemma_in_meta(y, 0);
        }
        let ghost d0 = gb.descs@;
        let ghost vd = d0[d].value();
        let ghost t0 = gb.table;
        let ghost st0 = gb.stats;
        let ghost a0 = gb.all@;
        let ghost l0 = gb.elist@;
        let tracked (mut yp, yc) = ydesc;
        proof {
            // `y` comes with its permission, so it is no live descriptor.
            if g.descs@.contains_key(y) {
                yp.is_disjoint(g.descs.borrow().tracked_borrow(y));
            }
            assert(!d0.contains_key(y));
            assert(desc_ok(vd, gb.next_page));
            assert(run_ok(vd, gb.lg@));
            lemma_can_sub(gb, d);
            lemma_tally_nonneg(a0, d0, 6);
            assert(keyed(t0, vd, d));
            assert(page + n <= gb.next_page <= gb.np@ <= u32::MAX);
            assert(st0.run_pages + st0.empty_pages + st0.purged_pages == gb.next_page);
            assert(st0.runs + st0.empty + st0.purged <= gb.next_page);
            assert(stat(st0, 3) == tally(a0, d0, 3));
            assert(stat(st0, 0) == tally(a0, d0, 0));
            // Nothing is keyed at the tail's first page: it is inside the
            // run, which is keyed at its start alone.
            assert(t0.lookup((page + need) as usize) is None) by {
                if t0.lookup((page + need) as usize) is Some {
                    let x = t0.lookup((page + need) as usize).unwrap();
                    assert(is_key(d0[x].value(), (page + need) as int));
                    assert(desc_ok(d0[x].value(), gb.next_page));
                    if x != d {
                        assert(apart(d0[x].value(), vd));
                    }
                }
            }
            self.lemma_pages(usable);
            self.lemma_pages((n - need - GUARD_PAGES) as usize);
        }
        let arena = g.desc(d).arena();
        g.drop_extent(d, Ghost(Seq::empty()));
        let ghost s1 = g.stats;
        {
            let dd = g.desc_mut(d);
            dd.pages = need as u32;
            dd.usable = usable << self.lg_page;
        }
        let ghost vs = g.descs@[d].value();
        let ghost ds = g.descs@;
        proof {
            assert(same_but_links(HugeDesc { pages: need as u32, usable: (usable * pow2(self.lg_page as nat)) as usize, ..vd }, vs));
            assert(run_ok(vs, gb.lg@));
            assert(g.table.lookup(page) is None);
            assert(stat(s1, 0) == stat(st0, 0) - weight(vd, 0));
            assert(stat(s1, 3) == stat(st0, 3) - weight(vd, 3));
            assert(s1.runs == st0.runs - 1 && s1.run_pages == st0.run_pages - n);
            assert(g.stats.can_add(RunState::Active, need));
        }
        g.add_extent(d);
        let ghost s2 = g.stats;
        let ghost t2 = g.table;
        let tail = n - need;
        // `y` is a fresh metadata block for a `HugeDesc`.
        ptr_mut_write(
            y,
            Tracked(&mut yp),
            HugeDesc {
                start: (page + need) as u32,
                pages: tail as u32,
                arena,
                state: RunState::Active as u8,
                _pad: 0,
                usable: (tail - GUARD_PAGES) << self.lg_page,
                dirty_at: 0,
                prev: ptr::null_mut(),
                next: ptr::null_mut(),
            },
        );
        let ghost vt = yp.value();
        let ghost dm = g.descs@;
        proof {
            assert(run_ok(vt, gb.lg@));
            g.descs.borrow_mut().tracked_insert(y, yp);
            g.dclaims.borrow_mut().tracked_insert(y, yc);
            g.all@ = g.all@.push(y);
            assert(g.table.lookup((page + need) as usize) is None);
            assert(stat(s2, 0) == stat(s1, 0) + weight(vs, 0));
            assert(stat(s2, 3) == stat(s1, 3) + weight(vs, 3));
            assert(s2.runs == st0.runs && s2.run_pages == st0.run_pages - n + need);
            assert(g.stats.can_add(RunState::Active, tail));
            assert(g.table.spec_count() + 1 < g.table.spec_cap());
            assert(perms_ok(g.descs@));
        }
        g.add_extent(y);
        let ghost g1 = *g;
        let ghost t = Split { d, vd, vs, y, vt };
        proof {
            assert(t.shape_ok(gb));
            assert(g1.split(gb, t)) by {
                assert forall|k: usize| #[trigger] g1.table.lookup(k) == t.lookup(t0, k) by {
                    assert(t2.lookup(k) == (if k == page { Some(d) } else { t0.lookup(k) }));
                }
                assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == stat(st0, k) + t.delta(k) by {
                    assert(stat(s1, k) == stat(st0, k) - weight(vd, k));
                    assert(stat(s2, k) == stat(s1, k) + weight(vs, k));
                    assert(stat(g1.stats, k) == stat(s2, k) + weight(vt, k));
                }
                assert(g1.table.spec_count() == t0.spec_count() + t.delta(6));
                assert forall|x: *mut HugeDesc| #[trigger] g1.descs@.contains_key(x) <==> t.model(d0, x) is Some by {}
                assert forall|x: *mut HugeDesc| #[trigger] g1.descs@.contains_key(x) implies same_but_links(
                    t.model(d0, x).unwrap(),
                    g1.descs@[x].value(),
                ) by {}
                assert forall|x: *mut HugeDesc| #[trigger] g1.descs@.contains_key(x) && x != d && x != y implies g1.descs@[x] == d0[x] by {}
                assert forall|x: *mut HugeDesc| #[trigger] g1.descs@.contains_key(x) implies desc_claimed(g1.dclaims@, x, g1.mid@) by {}
            }
        }
        Ghost(t)
    }

    /// Shrink the live run that starts at `page`, whose token is `tok`, in
    /// place to hold `size` bytes: [`Huge::usable_for`]`(size)` usable
    /// bytes, what a new run of that size has, followed by the guard.
    /// `Some((arena, k))`, the run's arena and the `k` pages it released,
    /// with the token of the shrunk run; `None`, with `tok`, when it does
    /// not shrink here and nothing changed: the size needs as many pages
    /// as the run has, or more, or releases a single page (the old guard
    /// alone, which can be no run of its own); the guards are `PROT_NONE`
    /// pages (the fallback mode, or a refused marker, `mixed_guards`),
    /// whose new guard would split the run's VMA, as the move's placement
    /// does anyway; or the descriptor block, the table's room or the new
    /// guard's marker could not be had (resource failures, which the
    /// move meets or not on its own).
    ///
    /// Under the huge lock, after `free_run`'s live-run checks (the token
    /// is the C caller's claim, which a racing free can make stale): the
    /// fallible steps first (a descriptor for the tail, room in the table
    /// for its key and for its free, the new guard's marker on the run's
    /// new last page), then the run's token leaves the run authorities,
    /// [`Huge::shrink_books`] splits the run in two, and the two runs'
    /// tokens enter them. The lock is released, and the tail, a live run
    /// whose token only this call holds, is returned with its first page,
    /// `Some((arena, k, tail))`, not freed: the caller gives up the
    /// permission to the tail's bytes first (`Live::put_shrunk_run`) and
    /// then frees it with [`Huge::free_run`], so no other thread can be
    /// handed those pages while a permission to them is still held. That
    /// free is any run's: its guard (the run's old one) loses its marker,
    /// it coalesces with a free extent after it, and it is purged or left
    /// `Empty` for decay as any freed run is. Nothing is reported zero.
    ///
    /// Fork epochs (design 5): the tail's free is a huge free during the
    /// epoch, which the floors do not steer either; the child keeps its
    /// copy of the pages, as it does of any run freed in the epoch.
    ///
    /// Proved to keep `Inner::wf` ([`Split`], `lemma_split_wf`); the
    /// tokens it returns are the shrunk run's and the tail's.
    #[verifier::spinoff_prover]
    #[allow(clippy::type_complexity)]
    pub fn shrink_run(
        &self,
        meta: &Meta,
        page: usize,
        size: usize,
        Tracked(tok): Tracked<RunToken>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (Option<(u16, usize, usize)>, Tracked<RunToken>, Tracked<Option<RunToken>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            self.is_process(),
            meta.is_process(),
            meta.live_id() == self.spec_mid(),
            tok.wf(),
            tok.start() == page,
            old(ord).allows(rank_huge()),
        ensures
            *final(ord) == *old(ord),
            r.1@.wf(),
            r.1@.start() == page,
            r.0 is None ==> r.1@ == tok,
            r.0 matches Some((_, k, tp)) ==> {
                &&& 2 <= k < 0x1_0000_0000
                &&& r.2@ is Some
                &&& r.2@.unwrap().wf()
                &&& r.2@.unwrap().start() == tp
                &&& tp == r.1@.start() + r.1@.npages()
                &&& r.1@.npages() + k == tok.npages()
                &&& r.1@.start() + r.1@.npages() <= self.spec_npages()
                &&& (r.1@.npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size)
                &&& size + self.spec_page() - 1 <= usize::MAX
                &&& size <= self.spec_usable(size)
            },
    {
        hide(Inner::wf);
        hide(Seq::no_duplicates);
        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        // A `PROT_NONE` guard would split the run's VMA: the move's
        // placement does that.
        if self.guards && !self.guard_install {
            return (None, Tracked(tok), Tracked(None));
        }
        let Some(usable) = self.usable_pages(size) else { return (None, Tracked(tok), Tracked(None)) };
        if usable >= self.npages {
            return (None, Tracked(tok), Tracked(None));
        }
        let need = usable + GUARD_PAGES;
        let ps = self.page_size();
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        let ghost g0 = g@;
        let ghost d0 = g0.descs@;
        let _gi: &Inner = &g;
        proof {
            self.lemma_process_runs();
            self.lemma_run_ids(g0);
            g0.lemma_wf_for_take();
            // The token's run is live: the run authority holds its
            // extent, so an `Active` descriptor starts at `page`, keyed
            // there.
            let tracked t = &tok;
            theorem_run_counted(_gi, t);
            super::free::lemma_live_keyed(g0, page);
        }
        let d = g.table.get(page);
        // `free_run`'s checks: a freed run aborts here as its move's free
        // would.
        self.grow_live_checks(_gi, page, d);
        proof {
            let x = choose|x: *mut HugeDesc|
                #![trigger g0.descs@.contains_key(x)]
                g0.descs@.contains_key(x) && run_at(g0.descs@[x].value(), (page as int, g0.descs@[x].value().pages as int))
                    && g0.descs@[x].value().state == RunState::Active as u8 && keyed(g0.table, g0.descs@[x].value(), x);
            assert(d == x);
            lemma_token_extent(g0, d, page as int, tok.npages());
            assert(desc_ok(d0[d].value(), g0.next_page));
            assert(states_ok(d0));
            assert(g0.lookups_live());
        }
        // `d` is the token's run, a live descriptor.
        let n = g.desc(d).pages();
        let arena = g.desc(d).arena();
        // A `PROT_NONE` guard below the mark (a refused marker) is set
        // only under this lock, so this read is stable while it is held.
        if need + GUARD_PAGES >= n || (self.guards && self.mixed_guards.load(Ordering::Relaxed)) {
            proof {
                assert(lock_frame(lock_init(self.inner), g@));
            }
            g.unlock();
            return (None, Tracked(tok), Tracked(None));
        }
        let ghost vd = d0[d].value();
        proof {
            assert(vd.start == page && vd.pages == n && vd.state == RunState::Active as u8);
            assert(page + n <= g0.next_page <= self.npages);
        }
        // Everything fallible happens before any bookkeeping: the tail's
        // descriptor, room in the table for its key and, as an
        // allocation reserves it, one more key per live run for the
        // frees (design 14.1), and the new guard's marker.
        let (y, _y_opt) = alloc_desc(meta, Tracked(&mut under));
        if y.is_null() {
            proof {
                assert(lock_frame(lock_init(self.inner), g@));
            }
            g.unlock();
            return (None, Tracked(tok), Tracked(None));
        }
        let ydesc = Tracked(_y_opt.get().tracked_unwrap());
        proof {
            g0.lemma_mid();
            // The table holds at most two keys per extent, a page or more
            // each, so its count is far below `Table::ensure`'s bound.
            assert forall|i: int| 0 <= i < g0.all@.len() implies (#[trigger] g0.descs@[g0.all@[i]].value()).pages >= 1
                && g0.descs@[g0.all@[i]].value().state <= 2 by {
                assert(g0.all@.contains(g0.all@[i]));
                assert(g0.descs@.contains_key(g0.all@[i]));
            }
            lemma_tally_counts(g0.all@, g0.descs@, 0);
            assert(stat(g0.stats, 0) == tally(g0.all@, g0.descs@, 0));
            assert(stat(g0.stats, 1) == tally(g0.all@, g0.descs@, 1));
            assert(stat(g0.stats, 2) == tally(g0.all@, g0.descs@, 2));
        }
        let runs = g.stats.runs;
        let extra = if g.table.keys() <= 0xFFFF_FFFF && runs <= 0xFFFF_FFFF { 4 + runs } else { 3 };
        if g.table.ensure(meta, ps, extra, Tracked(&mut under)).is_err() {
            free_desc(meta, y, Tracked(desc_raw(ydesc.get())), Tracked(&mut under));
            proof {
                assert(lock_frame(lock_init(self.inner), g@));
            }
            g.unlock();
            return (None, Tracked(tok), Tracked(None));
        }
        let ghost gb = g@;
        proof {
            lemma_wf_table(g0, gb);
            assert(gb.descs@ == d0);
        }
        if self.guards {
            proof {
                self.lemma_pages((page + need - GUARD_PAGES) as usize);
            }
            if guard_install(self.addr(page + need - GUARD_PAGES), GUARD_PAGES * ps).is_err() {
                free_desc(meta, y, Tracked(desc_raw(ydesc.get())), Tracked(&mut under));
                proof {
                    assert(lock_frame(lock_init(self.inner), g@));
                }
                g.unlock();
                return (None, Tracked(tok), Tracked(None));
            }
        }
        // The run's token leaves the run authorities, its pages split
        // between the two runs: from here on both are the lock's alone.
        let _gm: &mut Inner = &mut g;
        let tracked RunToken { ext, pages: mut kept_pages } = tok;
        let tracked tail_pages = kept_pages.split(vstd::set_lib::set_int_range((page + need) as int, (page + n) as int));
        proof {
            _gm.runs.borrow_mut().delete_singleton(ext);
        }
        // The bookkeeping.
        let _gm: &mut Inner = &mut g;
        let _ts = self.shrink_books(_gm, page, n, need, usable, d, y, ydesc, Ghost(gb));
        let ghost t = _ts@;
        let ghost g1 = g@;
        proof {
            lemma_split_active(gb, g1, t);
            lemma_run_auths(gb);
        }
        // The two runs' extents enter the run authorities; their pages
        // never left.
        let _gm: &mut Inner = &mut g;
        let tracked ext_s = _gm.runs.borrow_mut().insert((page as int, need as int));
        let tracked ext_t = _gm.runs.borrow_mut().insert(((page + need) as int, (n - need) as int));
        proof {
            assert(kept_pages@ =~= vstd::set_lib::set_int_range(page as int, (page + need) as int));
        }
        let tracked kept = RunToken { ext: ext_s, pages: kept_pages };
        let tracked tail = RunToken { ext: ext_t, pages: tail_pages };
        proof {
            assert(g@ == (Inner { runs: g@.runs, pgs: g@.pgs, ..g1 }));
            assert(g@.split(gb, t));
            lemma_split_wf(gb, g@, t);
            assert(lock_frame(lock_init(self.inner), g@));
            self.lemma_run_ids(g@);
        }
        g.unlock();
        // The tail is a live run whose token only this call holds. The
        // caller frees it, after it has given up the permission to the
        // tail's bytes: freed here, its pages could be another thread's
        // run while the caller's permission still covers them.
        (Some((arena, n - need, page + need)), Tracked(kept), Tracked(Some(tail)))
    }
}

} // verus!
