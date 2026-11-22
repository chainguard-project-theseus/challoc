//! Growing a live run in place (design 7): [`Huge::grow_run`] and the
//! [`Grow`] model of what it changes, with the lemmas that keep
//! `Inner::wf` across it.
//!
//! A buffer grown by appending (a CPython `str +=`, `bytearray` or
//! `BytesIO`; a Ruby `String#<<` or `IO#read`) is usually the last thing
//! its owner allocated, so the pages after it are free or past the
//! high-water mark, and a move at every step would copy the whole buffer
//! each time. `realloc` of a run to more pages ([`Huge::grow_run`]) takes
//! those pages for the run instead: from the free extent that starts right after
//! the run's guard, when it is long enough or reaches the mark, or from
//! the mark itself when the run is the last one. The run keeps its
//! descriptor, start and arena; its old guard page becomes a usable page,
//! and the guard moves to the new end. Its page count is the one a new run
//! of the requested size would have, so the sized frees' rule (design
//! 13.1) holds for a grown run as for any other.

use super::*;

verus! {

/// What [`Huge::grow_run`] changes in the huge lock's value: the live run
/// `d` (`vd`) becomes `vg`, the same run with more pages. The pages it
/// gains come from the free extent `x` (`ve`, `has_ext`) that starts where
/// the run ended, which keeps its tail `vt` (`used`) or retires, and, when
/// the run reaches past the mark `mark`, from fresh pages; `nm` is the
/// mark afterwards.
#[allow(dead_code)]
ghost struct Grow {
    d: *mut HugeDesc,
    vd: HugeDesc,
    vg: HugeDesc,
    x: *mut HugeDesc,
    has_ext: bool,
    ve: HugeDesc,
    used: bool,
    vt: HugeDesc,
    mark: usize,
    nm: usize,
}

impl Grow {
    /// Where the run ended before.
    spec fn e(self) -> int {
        self.vd.start + self.vd.pages
    }

    /// Where the run ends afterwards.
    spec fn end(self) -> int {
        self.vg.start + self.vg.pages
    }

    /// Where the extent ends.
    spec fn xe(self) -> int {
        self.ve.start + self.ve.pages
    }

    /// The descriptor at `y` afterwards, up to its list links.
    spec fn model(self, d0: Map<*mut HugeDesc, PointsTo<HugeDesc>>, y: *mut HugeDesc) -> Option<HugeDesc> {
        if y == self.d {
            Some(self.vg)
        } else if self.has_ext && y == self.x {
            if self.used { Some(self.vt) } else { None }
        } else if d0.contains_key(y) {
            Some(d0[y].value())
        } else {
            None
        }
    }

    /// The table's entry for `k` afterwards: the run keeps its one key,
    /// the extent's go, and the tail's come.
    spec fn lookup(self, t0: Table, k: usize) -> Option<*mut HugeDesc> {
        if self.used && is_key(self.vt, k as int) {
            Some(self.x)
        } else if self.has_ext && is_key(self.ve, k as int) {
            None
        } else {
            t0.lookup(k)
        }
    }

    /// What tally `k` gains.
    spec fn delta(self, k: int) -> int {
        weight(self.vg, k) - weight(self.vd, k) + (if self.has_ext { -weight(self.ve, k) } else { 0 }) + (if self.used {
            weight(self.vt, k)
        } else {
            0
        })
    }

    /// `all` afterwards: a retired extent leaves.
    spec fn all_after(self, a0: Seq<*mut HugeDesc>) -> Seq<*mut HugeDesc> {
        if self.has_ext && !self.used { a0.remove(a0.index_of(self.x)) } else { a0 }
    }

    /// The `Empty` list afterwards: a retired `Empty` extent leaves; a
    /// tail keeps the extent's place.
    spec fn elist_after(self, l0: Seq<*mut HugeDesc>) -> Seq<*mut HugeDesc> {
        if self.has_ext && !self.used && self.ve.state == RunState::Empty as u8 {
            l0.remove(l0.index_of(self.x))
        } else {
            l0
        }
    }

    /// The growth, against the state `g0` it starts from: the run, longer;
    /// the extent behind it, free, starting at the run's end and holding
    /// the new pages or reaching the mark; or no extent, the run ending at
    /// the mark; the tail, what is left of the extent.
    spec fn shape_ok(self, g0: Inner) -> bool {
        let d0 = g0.descs@;
        &&& self.mark == g0.next_page
        &&& d0.contains_key(self.d)
        &&& self.vd == d0[self.d].value()
        &&& self.vd.state == RunState::Active as u8
        &&& same_but_links(HugeDesc { pages: self.vg.pages, usable: self.vg.usable, ..self.vd }, self.vg)
        &&& self.vg.pages > self.vd.pages
        &&& run_ok(self.vg, g0.lg@)
        &&& self.nm == (if self.end() > self.mark { self.end() } else { self.mark as int })
        &&& self.nm <= g0.np@
        &&& self.has_ext == (self.x@.addr != 0)
        &&& self.has_ext ==> {
            &&& d0.contains_key(self.x)
            &&& self.ve == d0[self.x].value()
            &&& self.ve.state != RunState::Active as u8
            &&& self.ve.start == self.e()
            &&& (self.end() <= self.xe() || self.xe() == self.mark)
        }
        &&& !self.has_ext ==> self.e() == self.mark
        &&& self.used == (self.has_ext && self.end() < self.xe())
        &&& self.used ==> same_but_links(HugeDesc { start: self.end() as u32, pages: (self.xe() - self.end()) as u32, ..self.ve }, self.vt)
    }
}

impl Inner {
    /// `self` is `g0` grown by `t`, but for the run authorities.
    spec fn grew(&self, g0: Inner, t: Grow) -> bool {
        &&& t.shape_ok(g0)
        &&& self.table.wf()
        &&& self.table.spec_count() == g0.table.spec_count() + t.delta(6)
        &&& forall|k: usize| #[trigger] self.table.lookup(k) == t.lookup(g0.table, k)
        &&& perms_ok(self.descs@)
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) ==> desc_claimed(self.dclaims@, y, self.mid@)
        &&& self.table.spec_mid() == self.mid@
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) <==> t.model(g0.descs@, y) is Some
        &&& forall|y: *mut HugeDesc| #[trigger] self.descs@.contains_key(y) ==> same_but_links(
            t.model(g0.descs@, y).unwrap(),
            self.descs@[y].value(),
        )
        &&& self.all@ == t.all_after(g0.all@)
        &&& forall|k: int| 0 <= k < 6 ==> #[trigger] stat(self.stats, k) == stat(g0.stats, k) + t.delta(k)
        &&& self.next_page == t.nm
        &&& self.stats.next_page == self.next_page
        &&& self.np == g0.np
        &&& self.lg == g0.lg
        &&& self.free.map.wf()
        &&& self.free.map.in_meta()
        &&& self.free.map.spec_nbits() == self.np@
        &&& self.free.own_claimed()
        &&& self.free.l1_words <= 0x1_0000_0000
        &&& self.elist@ == t.elist_after(g0.elist@)
        &&& elist_ok(self.empty_head, self.empty_tail, self.elist@, self.descs@)
        &&& forall|q: nat| #[trigger] self.free.map@.contains(q) <==> g0.free.map@.contains(q) && !(t.e() <= q < t.end())
    }
}

/// An old extent other than the run and the extent shares no page with
/// the grown run or the tail: it is apart from both and ends by the mark.
proof fn lemma_grew_apart(g0: Inner, t: Grow, y: *mut HugeDesc)
    requires
        g0.wf(),
        t.shape_ok(g0),
        g0.descs@.contains_key(y),
        y != t.d,
        !(t.has_ext && y == t.x),
    ensures
        apart(g0.descs@[y].value(), t.vg),
        t.used ==> apart(g0.descs@[y].value(), t.vt),
{
    let v = g0.descs@[y].value();
    assert(desc_ok(v, g0.next_page));
    assert(apart(v, t.vd));
    if t.has_ext {
        assert(apart(v, t.ve));
    }
}

/// The extents after a growth: inside the new mark, disjoint, each under
/// exactly its keys.
proof fn lemma_grew_extents(g0: Inner, g1: Inner, t: Grow)
    requires
        g0.wf(),
        g1.grew(g0, t),
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
    if t.has_ext {
        assert(desc_ok(t.ve, g0.next_page));
        assert(keyed(t0, t.ve, t.x));
        assert(t.x != t.d);
    }
    assert(t.model(d0, t.d) == Some(t.vg));
    assert(d1.contains_key(t.d));
    if t.used {
        assert(t.model(d0, t.x) == Some(t.vt));
        assert(d1.contains_key(t.x));
    }
    // Which descriptors are old.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && y != t.d && !(t.has_ext && y == t.x) implies d0.contains_key(y)
        && same_but_links(d0[y].value(), d1[y].value()) by {
        assert(t.model(d0, y) is Some);
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) implies desc_ok(d1[y].value(), g1.next_page) && run_ok(
        d1[y].value(),
        g1.lg@,
    ) by {
        if y != t.d && !(t.has_ext && y == t.x) {
            assert(desc_ok(d0[y].value(), g0.next_page));
            assert(run_ok(d0[y].value(), g0.lg@));
        } else {
            assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        }
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) implies keyed(g1.table, d1[y].value(), y) by {
        let v = d1[y].value();
        let k1 = v.start as usize;
        let k2 = (v.start + v.pages - 1) as usize;
        assert(g1.table.lookup(k1) == t.lookup(t0, k1));
        assert(g1.table.lookup(k2) == t.lookup(t0, k2));
        if y == t.d {
            assert(same_but_links(t.vg, v));
            if t.has_ext {
                assert(!is_key(t.ve, k1 as int));
            }
            if t.used {
                assert(!is_key(t.vt, k1 as int));
            }
        } else if t.has_ext && y == t.x {
            assert(same_but_links(t.vt, v));
        } else {
            lemma_grew_apart(g0, t, y);
            if t.has_ext {
                assert(apart(d0[y].value(), t.ve));
            }
            assert(keyed(t0, d0[y].value(), y));
            assert(same_but_links(d0[y].value(), v));
        }
    }
    assert forall|k: usize| #[trigger] g1.table.lookup(k) is Some implies d1.contains_key(g1.table.lookup(k).unwrap()) && is_key(
        d1[g1.table.lookup(k).unwrap()].value(),
        k as int,
    ) by {
        assert(g1.table.lookup(k) == t.lookup(t0, k));
        let y = g1.table.lookup(k).unwrap();
        if t.used && is_key(t.vt, k as int) {
            assert(y == t.x);
            assert(same_but_links(t.vt, d1[y].value()));
        } else {
            assert(t0.lookup(k) == Some(y));
            assert(d0.contains_key(y) && is_key(d0[y].value(), k as int));
            if t.has_ext {
                assert(y != t.x);
            }
            assert(t.model(d0, y) is Some);
            assert(d1.contains_key(y));
            assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        }
    }
    assert forall|y1: *mut HugeDesc, y2: *mut HugeDesc|
        #[trigger] d1.contains_key(y1) && #[trigger] d1.contains_key(y2) && y1 != y2 implies apart(d1[y1].value(), d1[y2].value()) by {
        let new1 = y1 == t.d || (t.has_ext && y1 == t.x);
        let new2 = y2 == t.d || (t.has_ext && y2 == t.x);
        assert(same_but_links(t.model(d0, y1).unwrap(), d1[y1].value()));
        assert(same_but_links(t.model(d0, y2).unwrap(), d1[y2].value()));
        if !new1 && !new2 {
            assert(apart(d0[y1].value(), d0[y2].value()));
        } else if !new1 {
            lemma_grew_apart(g0, t, y1);
        } else if !new2 {
            lemma_grew_apart(g0, t, y2);
        }
    }
}

/// The tallies after a growth gain `delta`; `all` stays a list of the
/// live descriptors.
#[verifier::spinoff_prover]
proof fn lemma_grew_tally(g0: Inner, g1: Inner, t: Grow, k: int)
    requires
        g0.wf(),
        g1.grew(g0, t),
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
    // The run first: `dm` is `d0` with the run's new value.
    let dm = d0.insert(t.d, d1[t.d]);
    assert(t.model(d0, t.d) == Some(t.vg));
    assert(d1.contains_key(t.d));
    assert(same_but_links(t.vg, d1[t.d].value()));
    assert(weight(dm[t.d].value(), k) == weight(t.vg, k));
    lemma_tally_update(a0, d0, dm, t.d, k);
    // Every other old entry but the extent keeps its weight.
    assert forall|i: int| 0 <= i < a0.len() && a0[i] != t.d && !(t.has_ext && a0[i] == t.x) implies #[trigger] weight(
        d1[a0[i]].value(),
        k,
    ) == weight(dm[a0[i]].value(), k) by {
        assert(a0.contains(a0[i]));
        assert(d0.contains_key(a0[i]));
        assert(t.model(d0, a0[i]) == Some(d0[a0[i]].value()));
        assert(d1.contains_key(a0[i]));
        assert(same_but_links(d0[a0[i]].value(), d1[a0[i]].value()));
    }
    if t.has_ext && !t.used {
        let j = a0.index_of(t.x);
        assert(a0.contains(t.x));
        let b = a0.remove(j);
        assert(a0[j] == t.x);
        assert forall|i: int| 0 <= i < b.len() implies #[trigger] weight(d1[b[i]].value(), k) == weight(dm[b[i]].value(), k) by {
            if i < j {
                assert(b[i] == a0[i]);
                assert(a0[i] != a0[j]);
            } else {
                assert(b[i] == a0[i + 1]);
                assert(a0[i + 1] != a0[j]);
            }
            if b[i] == t.d {
                assert(weight(d1[t.d].value(), k) == weight(dm[t.d].value(), k));
            }
        }
        lemma_tally_frame(b, dm, d1, k);
        lemma_tally_remove(a0, dm, j, k);
        assert(dm[t.x].value() == t.ve);
        assert(b.no_duplicates()) by {
            assert forall|i1: int, i2: int| 0 <= i1 < b.len() && 0 <= i2 < b.len() && i1 != i2 implies b[i1] != b[i2] by {
                let j1 = if i1 < j { i1 } else { i1 + 1 };
                let j2 = if i2 < j { i2 } else { i2 + 1 };
                assert(b[i1] == a0[j1] && b[i2] == a0[j2]);
            }
        }
        assert forall|y: *mut HugeDesc| #[trigger] b.contains(y) <==> a0.contains(y) && y != t.x by {
            if b.contains(y) {
                let i = b.index_of(y);
                let i0 = if i < j { i } else { i + 1 };
                assert(a0[i0] == y);
            }
            if a0.contains(y) && y != t.x {
                let i0 = a0.index_of(y);
                let i = if i0 < j { i0 } else { i0 - 1 };
                assert(b[i] == y);
            }
        }
        assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) <==> g1.all@.contains(y) by {
            assert(d1.contains_key(y) <==> t.model(d0, y) is Some);
        }
    } else {
        if t.used {
            assert(a0.contains(t.x));
            assert(t.model(d0, t.x) == Some(t.vt));
            assert(d1.contains_key(t.x));
            assert(same_but_links(t.vt, d1[t.x].value()));
            assert(dm[t.x].value() == t.ve);
            assert forall|i: int| 0 <= i < a0.len() && a0[i] != t.x implies #[trigger] weight(d1[a0[i]].value(), k) == weight(
                dm[a0[i]].value(),
                k,
            ) by {
                if a0[i] == t.d {
                    assert(weight(d1[t.d].value(), k) == weight(dm[t.d].value(), k));
                }
            }
            lemma_tally_update(a0, dm, d1, t.x, k);
        } else {
            assert forall|i: int| 0 <= i < a0.len() implies #[trigger] weight(d1[a0[i]].value(), k) == weight(dm[a0[i]].value(), k) by {
                if a0[i] == t.d {
                    assert(weight(d1[t.d].value(), k) == weight(dm[t.d].value(), k));
                }
            }
            lemma_tally_frame(a0, dm, d1, k);
        }
        assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) <==> g1.all@.contains(y) by {
            assert(d1.contains_key(y) <==> t.model(d0, y) is Some);
        }
    }
}

/// The `Empty` list after a growth: its length and pages are the
/// counters, and it holds every `Empty` extent.
#[verifier::spinoff_prover]
proof fn lemma_grew_elist(g0: Inner, g1: Inner, t: Grow)
    requires
        g0.wf(),
        g1.grew(g0, t),
    ensures
        g1.stats.empty == g1.elist@.len(),
        g1.stats.empty_pages == epages(g1.elist@, g1.descs@),
        forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) && g1.descs@[y].value().state == RunState::Empty as u8
            ==> g1.elist@.contains(y),
{
    reveal(Seq::no_duplicates);
    reveal(Seq::index_of);
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let l0 = g0.elist@;
    assert(stat(g1.stats, 1) == stat(g0.stats, 1) + t.delta(1));
    assert(stat(g1.stats, 4) == stat(g0.stats, 4) + t.delta(4));
    assert forall|i: int| #![trigger d0.contains_key(l0[i])] #![trigger d0[l0[i]]] 0 <= i < l0.len() implies d0.contains_key(l0[i])
        && d0[l0[i]].value().state == RunState::Empty as u8 by {
        assert(enode(l0, d0, i));
    }
    // The old list's members other than the extent keep their pages; the
    // run is not one.
    assert forall|i: int| 0 <= i < l0.len() && !(t.has_ext && l0[i] == t.x) implies #[trigger] d1[l0[i]].value().pages
        == d0[l0[i]].value().pages && d1.contains_key(l0[i]) by {
        assert(d0.contains_key(l0[i]));
        assert(l0[i] != t.d);
        assert(t.model(d0, l0[i]) == Some(d0[l0[i]].value()));
        assert(d1.contains_key(l0[i]));
        assert(same_but_links(d0[l0[i]].value(), d1[l0[i]].value()));
    }
    if t.has_ext && t.ve.state == RunState::Empty as u8 {
        assert(l0.contains(t.x));
        let j = l0.index_of(t.x);
        if t.used {
            assert(t.model(d0, t.x) == Some(t.vt));
            assert(d1.contains_key(t.x));
            assert(same_but_links(t.vt, d1[t.x].value()));
            lemma_epages_update(l0, d0, d1, t.x);
        } else {
            let l1 = l0.remove(j);
            assert forall|i: int| 0 <= i < l1.len() implies #[trigger] d1[l1[i]].value().pages == d0[l1[i]].value().pages by {
                if i < j {
                    assert(l1[i] == l0[i]);
                } else {
                    assert(l1[i] == l0[i + 1]);
                }
            }
            lemma_epages_frame(l1, d0, d1);
            lemma_epages_remove(l0, d0, j);
        }
        assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state == RunState::Empty as u8 implies g1.elist@.contains(
            y,
        ) by {
            assert(t.model(d0, y) is Some);
            assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
            if y != t.d && y != t.x {
                assert(d0.contains_key(y) && d0[y].value().state == RunState::Empty as u8);
                assert(l0.contains(y));
                if !t.used {
                    let i = l0.index_of(y);
                    assert(i != j);
                    if i < j {
                        assert(g1.elist@[i] == y);
                    } else {
                        assert(g1.elist@[i - 1] == y);
                    }
                }
            }
        }
    } else {
        assert forall|i: int| 0 <= i < l0.len() implies #[trigger] d1[l0[i]].value().pages == d0[l0[i]].value().pages by {
            assert(d0.contains_key(l0[i]));
            if t.has_ext && l0[i] == t.x {
                assert(d0[l0[i]].value().state == RunState::Empty as u8);
            }
        }
        lemma_epages_frame(l0, d0, d1);
        assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state == RunState::Empty as u8 implies g1.elist@.contains(
            y,
        ) by {
            assert(t.model(d0, y) is Some);
            assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        }
    }
}

/// A growth leaves the run the one changed `Active` descriptor: the run
/// authorities lose its old extent and pages and gain its new ones,
/// which no other `Active` descriptor holds.
proof fn lemma_grew_active(g0: Inner, g1: Inner, t: Grow)
    requires
        g0.wf(),
        g1.grew(g0, t),
    ensures
        ({
            let (old_e, new_e) = ((t.vd.start as int, t.vd.pages as int), (t.vg.start as int, t.vg.pages as int));
            let (old_p, new_p) = (
                vstd::set_lib::set_int_range(t.vd.start as int, t.e()),
                vstd::set_lib::set_int_range(t.vg.start as int, t.end()),
            );
            &&& active_runs(g0.descs@).contains(old_e)
            &&& !active_runs(g0.descs@).remove(old_e).contains(new_e)
            &&& active_runs(g1.descs@) == active_runs(g0.descs@).remove(old_e).insert(new_e)
            &&& active_pages(g0.descs@).difference(old_p).disjoint(new_p)
            &&& active_pages(g1.descs@) == active_pages(g0.descs@).difference(old_p).union(new_p)
        }),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    lemma_active(d0);
    lemma_active(d1);
    let old_e = (t.vd.start as int, t.vd.pages as int);
    let new_e = (t.vg.start as int, t.vg.pages as int);
    let old_p = vstd::set_lib::set_int_range(t.vd.start as int, t.e());
    let new_p = vstd::set_lib::set_int_range(t.vg.start as int, t.end());
    assert(desc_ok(t.vd, g0.next_page));
    assert(run_at(d0[t.d].value(), old_e));
    assert(d1.contains_key(t.d) && same_but_links(t.vg, d1[t.d].value())) by {
        assert(t.model(d0, t.d) == Some(t.vg));
    }
    // An old `Active` extent other than the run keeps its value and is
    // apart from the grown run.
    assert forall|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && d0[y].value().state == RunState::Active as u8 && y != t.d implies {
        &&& !(t.has_ext && y == t.x)
        &&& d1.contains_key(y)
        &&& same_but_links(d0[y].value(), d1[y].value())
        &&& apart(d0[y].value(), t.vg)
        &&& apart(d0[y].value(), t.vd)
    } by {
        assert(t.model(d0, y) == Some(d0[y].value()));
        assert(d1.contains_key(y));
        assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
        lemma_grew_apart(g0, t, y);
        assert(apart(d0[y].value(), t.vd));
    }
    // A new `Active` extent is the run or an old one.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state == RunState::Active as u8 && y != t.d implies d0.contains_key(
        y,
    ) && same_but_links(d0[y].value(), d1[y].value()) by {
        assert(same_but_links(t.model(d0, y).unwrap(), d1[y].value()));
    }
    assert(!active_runs(d0).remove(old_e).contains(new_e)) by {
        if active_runs(d0).remove(old_e).contains(new_e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_at(d0[y].value(), new_e);
            if y == t.d {
                assert(t.vg.pages != t.vd.pages);
            } else {
                assert(apart(d0[y].value(), t.vg));
            }
        }
    }
    assert forall|e: (int, int)| active_runs(d1).contains(e) <==> active_runs(d0).remove(old_e).insert(new_e).contains(e) by {
        if active_runs(d1).contains(e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && run_at(d1[y].value(), e);
            if y != t.d {
                assert(d0.contains_key(y) && run_at(d0[y].value(), e));
                assert(apart(d0[y].value(), t.vd));
            }
        }
        if active_runs(d0).remove(old_e).contains(e) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_at(d0[y].value(), e);
            assert(y != t.d);
            assert(d1.contains_key(y) && run_at(d1[y].value(), e));
        }
        if e == new_e {
            assert(run_at(d1[t.d].value(), e));
        }
    }
    assert(active_runs(d1) =~= active_runs(d0).remove(old_e).insert(new_e));
    assert forall|k: int| active_pages(d0).difference(old_p).contains(k) implies !new_p.contains(k) by {
        let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_page(d0[y].value(), k);
        if y == t.d {
            assert(old_p.contains(k));
        } else {
            assert(apart(d0[y].value(), t.vg));
        }
    }
    assert(active_pages(d0).difference(old_p).disjoint(new_p));
    assert forall|k: int| active_pages(d1).contains(k) <==> active_pages(d0).difference(old_p).union(new_p).contains(k) by {
        if active_pages(d1).contains(k) {
            let y = choose|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && run_page(d1[y].value(), k);
            if y != t.d {
                assert(d0.contains_key(y) && run_page(d0[y].value(), k));
                assert(apart(d0[y].value(), t.vd));
            }
        }
        if active_pages(d0).difference(old_p).contains(k) {
            let y = choose|y: *mut HugeDesc| #[trigger] d0.contains_key(y) && run_page(d0[y].value(), k);
            assert(y != t.d);
            assert(d1.contains_key(y) && run_page(d1[y].value(), k));
        }
        if new_p.contains(k) {
            assert(run_page(d1[t.d].value(), k));
        }
    }
    assert(active_pages(d1) =~= active_pages(d0).difference(old_p).union(new_p));
}

/// A growth keeps the free extents coalesced and marked free: the tail
/// starts at the grown run's end and ends where the extent did; its pages,
/// and the other free extents' (apart from the run), are in the free-page
/// set, and nothing else is.
proof fn lemma_grew_coalesced(g0: Inner, g1: Inner, t: Grow)
    requires
        g0.wf(),
        g1.grew(g0, t),
    ensures
        coalesced(g1.descs@),
        free_marked(g1.descs@, g1.free.map@),
        free_exact(g1.descs@, g1.free.map@),
{
    let d0 = g0.descs@;
    let d1 = g1.descs@;
    let f0 = g0.free.map@;
    let f1 = g1.free.map@;
    assert(desc_ok(t.vd, g0.next_page));
    assert(d1.contains_key(t.d) && same_but_links(t.vg, d1[t.d].value())) by {
        assert(t.model(d0, t.d) == Some(t.vg));
    }
    if t.has_ext {
        assert(desc_ok(t.ve, g0.next_page));
        assert(marked(f0, t.ve));
    }
    if t.used {
        assert(t.model(d0, t.x) == Some(t.vt));
        assert(d1.contains_key(t.x) && same_but_links(t.vt, d1[t.x].value()));
    }
    // Which descriptors are old.
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && y != t.d && !(t.has_ext && y == t.x) implies d0.contains_key(y)
        && same_but_links(d0[y].value(), d1[y].value()) by {
        assert(t.model(d0, y) is Some);
    }
    assert forall|y1: *mut HugeDesc, y2: *mut HugeDesc|
        #[trigger] d1.contains_key(y1) && #[trigger] d1.contains_key(y2) && d1[y1].value().state != RunState::Active as u8
            && d1[y2].value().state != RunState::Active as u8 implies d1[y1].value().start + d1[y1].value().pages
        != d1[y2].value().start by {
        let old1 = y1 != t.d && !(t.has_ext && y1 == t.x);
        let old2 = y2 != t.d && !(t.has_ext && y2 == t.x);
        if old1 {
            assert(desc_ok(d0[y1].value(), g0.next_page));
            lemma_grew_apart(g0, t, y1);
            if t.has_ext {
                assert(apart(d0[y1].value(), t.ve));
            }
        }
        if old2 {
            assert(desc_ok(d0[y2].value(), g0.next_page));
            if t.has_ext {
                assert(apart(d0[y2].value(), t.ve));
            }
        }
        if old1 && old2 {
            assert(d0.contains_key(y1) && d0.contains_key(y2));
        } else if old1 && t.has_ext && y2 == t.x {
            // The tail starts at the grown run's end, which no free extent
            // reaches: a page before it is the run's or the extent's.
            assert(d0.contains_key(y1));
        } else if t.has_ext && y1 == t.x && old2 {
            // The tail ends where the extent did.
            assert(d0.contains_key(t.x) && d0.contains_key(y2));
        }
    }
    assert forall|y: *mut HugeDesc| #[trigger] d1.contains_key(y) && d1[y].value().state != RunState::Active as u8 implies marked(
        f1,
        d1[y].value(),
    ) by {
        let v = d1[y].value();
        if y != t.d && !(t.has_ext && y == t.x) {
            assert(d0.contains_key(y));
            assert(marked(f0, d0[y].value()));
            lemma_grew_apart(g0, t, y);
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                assert(f0.contains(q as nat));
            }
        } else {
            assert(y == t.x && t.used);
            assert forall|q: int| v.start <= q < v.start + v.pages implies #[trigger] f1.contains(q as nat) by {
                assert(f0.contains(q as nat));
            }
        }
    }
    assert forall|q: nat| #[trigger] f1.contains(q) implies exists|y: *mut HugeDesc| #[trigger] in_free(d1, y, q as int) by {
        assert(f0.contains(q) && !(t.e() <= q < t.end()));
        let y = choose|y: *mut HugeDesc| #[trigger] in_free(d0, y, q as int);
        if t.has_ext && y == t.x {
            assert(in_free(d1, t.x, q as int));
        } else {
            assert(y != t.d);
            assert(t.model(d0, y) == Some(d0[y].value()));
            assert(d1.contains_key(y));
            assert(same_but_links(d0[y].value(), d1[y].value()));
            assert(in_free(d1, y, q as int));
        }
    }
}

/// A growth keeps `Inner::wf`, once the run authorities hold the grown
/// run's extent and pages and the pages below the new mark are committed.
proof fn lemma_grew_wf(g0: Inner, g1: Inner, t: Grow)
    requires
        g0.wf(),
        g1.grew(g0, t),
        g1.runs@@ == active_runs(g1.descs@),
        g1.pgs@@ == active_pages(g1.descs@),
        g1.mark_committed(),
    ensures
        g1.wf(),
{
    lemma_grew_extents(g0, g1, t);
    lemma_grew_elist(g0, g1, t);
    lemma_grew_coalesced(g0, g1, t);
    assert forall|k: int| 0 <= k < 7 implies #[trigger] tally(g1.all@, g1.descs@, k) == tally(g0.all@, g0.descs@, k) + t.delta(k) by {
        lemma_grew_tally(g0, g1, t, k);
    }
    // Once more outside the `forall`, for what it says about `all`.
    lemma_grew_tally(g0, g1, t, 0);
    assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == tally(g1.all@, g1.descs@, k) by {
        assert(stat(g1.stats, k) == stat(g0.stats, k) + t.delta(k));
        assert(stat(g0.stats, k) == tally(g0.all@, g0.descs@, k));
    }
    assert(g1.table.spec_count() == tally(g1.all@, g1.descs@, 6));
    // The pages sum to the mark: the run gains what the extent loses, and
    // the pages past the old mark.
    assert(desc_ok(t.vd, g0.next_page));
    if t.has_ext {
        assert(desc_ok(t.ve, g0.next_page));
    }
    assert(t.delta(3) + t.delta(4) + t.delta(5) == t.nm - t.mark);
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
    assert(stat(g1.stats, 0) + stat(g1.stats, 1) + stat(g1.stats, 2) <= g1.next_page);
    assert(stat(g1.stats, 3) + stat(g1.stats, 4) + stat(g1.stats, 5) == stat(g0.stats, 3) + stat(g0.stats, 4) + stat(g0.stats, 5)
        + t.delta(3) + t.delta(4) + t.delta(5));
    assert(g1.stats.run_pages + g1.stats.empty_pages + g1.stats.purged_pages == g1.next_page);
    assert(g1.next_page <= g1.np@ <= u32::MAX);
    assert(g1.stats.empty == g1.elist@.len());
    assert(forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) ==> desc_claimed(g1.dclaims@, y, g1.mid@));
    assert(g1.table.spec_mid() == g1.mid@);
    assert(g1.mark_committed());
}

/// The pages below the mark are committed (`wf`'s, for a caller that
/// hides it).
proof fn lemma_mark_committed(g: Inner)
    requires
        g.wf(),
    ensures
        g.mark_committed(),
{
}

/// `wf` survives a change of the free-page set's storage only
/// (`Set::ensure`).
proof fn lemma_wf_store(g0: Inner, g1: Inner)
    requires
        g0.wf(),
        g1 == (Inner { free: g1.free, ..g0 }),
        g1.free.map == g0.free.map,
        g1.free.l1_words == g0.free.l1_words,
        g1.free.own_claimed(),
    ensures
        g1.wf(),
{
}

impl Huge {
    /// Grow the live run that starts at `page`, whose token is `tok`, in
    /// place to hold `size` bytes: [`Huge::usable_for`]`(size)` usable
    /// bytes, what a new run of that size has, followed by the guard.
    /// `Some((arena, k))`, the run's arena and the `k` pages it gained,
    /// with the token of the grown run; `None`, with `tok`, when it does
    /// not grow here and nothing changed: the size needs no more pages
    /// (or more than the region has), the pages after the run are another
    /// run's, a free extent too short to hold the growth that ends below
    /// the mark, or past the region's end; the guards are `PROT_NONE`
    /// pages (the fallback mode, or a refused marker, `mixed_guards`),
    /// which the move splits off instead; a fork epoch is in force and the
    /// extent's pages may be shared with the child (below); or the kernel
    /// refused a commit or the new guard's marker.
    ///
    /// The kernel's side first, before any bookkeeping: pages past the
    /// mark are committed, the new guard gets its marker; then the run's
    /// token leaves the run authorities and its old guard's marker is
    /// removed, which fails only when the program unmapped or sealed part
    /// of the region (an inconsistency: abort, as `free_run` does). The
    /// bookkeeping is infallible: the extent loses the pages the run
    /// takes (it keeps its descriptor, state, age and place in the `Empty`
    /// list for what is left, or retires), the mark moves past the run
    /// when it reaches past it, and the run's descriptor gets its new page
    /// count. No table key is added, so the table needs no room.
    ///
    /// Nothing is reported zero: the pages the run takes from a free
    /// extent may have been written (design 7, `rediscard`), and the old
    /// guard page and the fresh pages are not worth tracking, since
    /// `realloc` does not zero.
    ///
    /// Fork epochs (design 5): while the floors are in force
    /// (`cow::in_force`), the run grows into a free extent only when it is
    /// `Purged` and every purge so far was `MADV_DONTNEED`
    /// (`purge::purged_is_zero`), so the child holds the only copy of its
    /// pages, or past the mark, pages the child never had; an `Empty`
    /// extent (a run freed during the epoch, whose pages the child shares,
    /// or one a purge left resident) is declined, as a slab refill under a
    /// floor declines a slab below it. The move that follows is not
    /// steered either (design 5, "Scope"), so declining costs nothing that
    /// was not paid before.
    ///
    /// Proved to keep `Inner::wf` ([`Grow`], `lemma_grew_wf`), and the
    /// token it returns is the grown run's: its extent and pages replace
    /// the old token's in the run authorities.
    #[verifier::spinoff_prover]
    #[verifier::rlimit(20)]
    #[allow(clippy::type_complexity)]
    pub fn grow_run(
        &self,
        meta: &Meta,
        page: usize,
        size: usize,
        Tracked(tok): Tracked<RunToken>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (Option<(u16, usize)>, Tracked<RunToken>))
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
            r.0 matches Some((_, k)) ==> {
                &&& 1 <= k < 0x1_0000_0000
                &&& r.1@.npages() == tok.npages() + k
                &&& r.1@.start() + r.1@.npages() <= self.spec_npages()
                &&& (r.1@.npages() - GUARD_PAGES) * self.spec_page() == self.spec_usable(size)
                &&& size + self.spec_page() - 1 <= usize::MAX
                &&& size <= self.spec_usable(size)
            },
    {
        hide(Inner::wf);
        hide(Seq::no_duplicates);
        hide(Seq::index_of);
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        proof {
            use_type_invariant(self);
            self.lemma_page();
        }
        // A `PROT_NONE` guard would have to be split off the grown run's
        // pages again: the move does that.
        if self.guards && !self.guard_install {
            return (None, Tracked(tok));
        }
        let Some(usable) = self.usable_pages(size) else { return (None, Tracked(tok)) };
        if usable >= self.npages {
            return (None, Tracked(tok));
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
        let (n, arena) = (g.desc(d).pages(), g.desc(d).arena());
        let e = page + n;
        let mark = g.next_page;
        // A `PROT_NONE` guard below the mark (a refused marker) is set
        // only under this lock, so this read is stable while it is held.
        if need <= n || need > self.npages - page || (self.guards && self.mixed_guards.load(Ordering::Relaxed)) {
            proof {
                assert(lock_frame(lock_init(self.inner), g@));
            }
            g.unlock();
            return (None, Tracked(tok));
        }
        let end = page + need;
        let x = if e < mark { g.free_after(e) } else { ptr::null_mut() };
        if e < mark {
            if x.is_null() {
                proof {
                    assert(lock_frame(lock_init(self.inner), g@));
                }
                g.unlock();
                return (None, Tracked(tok));
            }
            // `x` is a live free extent starting at `e`.
            let (xe, xst) = (g.desc(x).end(), g.desc(x).state());
            let shared_pages = crate::cow::in_force() && !(xst == RunState::Purged && purge::purged_is_zero());
            if (xe < end && xe != mark) || shared_pages {
                proof {
                    assert(lock_frame(lock_init(self.inner), g@));
                }
                g.unlock();
                return (None, Tracked(tok));
            }
        }
        let ghost has_ext = x@.addr != 0;
        let ghost vd = d0[d].value();
        let ghost ve = d0[x].value();
        proof {
            assert(vd.start == page && vd.pages == n && vd.state == RunState::Active as u8);
            assert(e <= mark);
            if has_ext {
                assert(desc_ok(ve, g0.next_page));
                assert(ve.start == e && ve.state != RunState::Active as u8);
            } else {
                assert(e == mark);
            }
        }
        // Everything fallible happens before any bookkeeping: the free-page
        // set's storage for the pages up to the new end (as `alloc_aligned`
        // ensures it, so the run's free can mark them), the commit of the
        // pages past the mark, and the new guard's marker.
        if end > mark && g.free.ensure(meta, ps, end - 1, Tracked(&mut under)).is_err() {
            proof {
                lemma_wf_store(g0, g@);
                assert(lock_frame(lock_init(self.inner), g@));
            }
            g.unlock();
            return (None, Tracked(tok));
        }
        let ghost gb = g@;
        proof {
            lemma_wf_store(g0, gb);
            assert(gb.descs@ == d0);
        }
        if end > mark {
            proof {
                // Every live run ends by the mark, so the pages past it
                // spare every guard.
                assert forall|y: *mut HugeDesc|
                    #[trigger] gb.descs@.contains_key(y) && y != vstd::raw_ptr::ptr_null_mut::<HugeDesc>() && gb.descs@[y].value().state
                        == RunState::Active as u8 implies {
                        let v = gb.descs@[y].value();
                        v.start + v.pages <= mark || end <= v.start + v.pages - GUARD_PAGES
                    } by {
                    assert(desc_ok(gb.descs@[y].value(), g0.next_page));
                }
                assert(!gb.descs@.contains_key(vstd::raw_ptr::ptr_null_mut::<HugeDesc>())) by {
                    if gb.descs@.contains_key(vstd::raw_ptr::ptr_null_mut::<HugeDesc>()) {
                        assert(perms_ok(gb.descs@));
                    }
                }
                lemma_pages_spare_guards(*self, gb, vstd::raw_ptr::ptr_null_mut(), mark as int, end as int);
                self.lemma_pages(mark);
                self.lemma_pages(end);
                self.lemma_pages((end - mark) as usize);
            }
            if commit(self.addr(mark), (end - mark) * ps, Ghost(*self), g.perm(), Ghost(ptr::null_mut())).is_err() {
                proof {
                    assert(lock_frame(lock_init(self.inner), g@));
                }
                g.unlock();
                return (None, Tracked(tok));
            }
            proof {
                self.lemma_mark_grows(mark as int, mark as int, end as int);
            }
        }
        proof {
            // The pages below the mark afterwards are committed: those
            // below the old mark were, and a run past it committed the
            // rest.
            lemma_mark_committed(gb);
            assert(lock_frame(lock_init(self.inner), gb));
            assert(gb.base@ == self.base && gb.lg@ == self.lg_page);
            assert(crate::vspec::unbacked::committed_range(
                self.base as int,
                (if end > mark { end as int } else { mark as int }) * pow2(self.lg_page as nat),
            ));
        }
        if self.guards {
            proof {
                self.lemma_pages((end - GUARD_PAGES) as usize);
            }
            if guard_install(self.addr(end - GUARD_PAGES), GUARD_PAGES * ps).is_err() {
                proof {
                    assert(lock_frame(lock_init(self.inner), g@));
                }
                g.unlock();
                return (None, Tracked(tok));
            }
        }
        // The run's token leaves the run authorities: from here on the run
        // is the lock's alone, and its old guard may be released.
        let _gm: &mut Inner = &mut g;
        proof {
            let tracked RunToken { ext, pages } = tok;
            _gm.runs.borrow_mut().delete_singleton(ext);
            _gm.pgs.borrow_mut().delete(pages);
        }
        let ghost gr = g@;
        if self.guards {
            proof {
                lemma_freed_guard_apart(*self, gb, d);
                self.lemma_pages((e - GUARD_PAGES) as usize);
                assert(gr.descs@ == gb.descs@);
                assert(!gr.runs@@.contains((vd.start as int, vd.pages as int)));
            }
            // Not a resource failure (`free_run`): the program unmapped or
            // sealed part of the huge region.
            if let Err(err) = guard_remove(self.addr(e - GUARD_PAGES), GUARD_PAGES * ps, Ghost(*self), g.perm(), Ghost(d)) {
                sys::abort_kernel(format_args!("huge: madvise(MADV_GUARD_REMOVE) of {:#x} failed: {err}", self.addr(page)));
            }
        }
        // The bookkeeping.
        let ghost t0 = gb.table;
        let ghost st0 = gb.stats;
        let ghost mut vt = ve;
        let ghost mut used = false;
        proof {
            lemma_can_sub(gb, d);
            if has_ext {
                lemma_can_sub(gb, x);
                assert(keyed(t0, ve, x));
                assert(apart(vd, ve));
            }
            lemma_tally_nonneg(gb.all@, d0, 6);
            assert(keyed(t0, vd, d));
        }
        proof {
            assert(st0.run_pages + st0.empty_pages + st0.purged_pages == mark);
            assert(st0.runs + st0.empty + st0.purged <= mark);
            assert(mark <= gb.np@ <= u32::MAX);
        }
        if !x.is_null() {
            let xe = g.desc(x).end();
            g.drop_extent(x, Ghost(Seq::empty()));
            proof {
                assert(ve.state == RunState::Empty as u8 || ve.state == RunState::Purged as u8);
                assert(stat(g@.stats, 1) == stat(st0, 1) - weight(ve, 1));
                assert(stat(g@.stats, 2) == stat(st0, 2) - weight(ve, 2));
                assert(stat(g@.stats, 4) == stat(st0, 4) - weight(ve, 4));
                assert(stat(g@.stats, 5) == stat(st0, 5) - weight(ve, 5));
            }
            let hi = if end < xe { end } else { xe };
            g.free.map.clear_range(e, hi - e);
            if xe > end {
                // What is left keeps the descriptor, its state, its age
                // and its place in the `Empty` list.
                let ghost dm = g@.descs@;
                {
                    let xd = g.desc_mut(x);
                    xd.start = end as u32;
                    xd.pages = (xe - end) as u32;
                }
                proof {
                    used = true;
                    vt = HugeDesc { start: end as u32, pages: (xe - end) as u32, ..ve };
                    assert(same_but_links(vt, g@.descs@[x].value()));
                    assert(forall|i: int| 0 <= i < g@.elist@.len() ==> #[trigger] enode(g@.elist@, g@.descs@, i) == enode(g@.elist@, dm, i));
                    // Nothing is keyed inside the old extent now.
                    assert(g@.table.lookup(end) is None) by {
                        if t0.lookup(end) is Some {
                            let y = t0.lookup(end).unwrap();
                            assert(is_key(d0[y].value(), end as int));
                            assert(desc_ok(d0[y].value(), g0.next_page));
                            if y != x {
                                assert(apart(d0[y].value(), ve));
                            }
                        }
                    }
                    assert(g@.table.lookup((xe - 1) as usize) is None) by {
                        if t0.lookup((xe - 1) as usize) is Some {
                            let y = t0.lookup((xe - 1) as usize).unwrap();
                            assert(is_key(d0[y].value(), xe - 1));
                            assert(desc_ok(d0[y].value(), g0.next_page));
                            if y != x {
                                assert(apart(d0[y].value(), ve));
                            }
                        }
                    }
                    assert(weight(vt, 6) <= weight(ve, 6));
                    assert(g@.stats.can_add(state_of(vt.state), vt.pages as usize));
                    assert(perms_ok(g@.descs@));
                }
                g.add_extent(x);
            } else {
                proof {
                    assert(gb.all@.contains(x));
                    if ve.state == RunState::Empty as u8 {
                        assert(gb.elist@.contains(x));
                    }
                }
                g.retire(meta, x, Tracked(&mut under));
            }
        }
        if end > mark {
            g.next_page = end;
        }
        let ghost dr = g@.descs@;
        let ghost s1 = g@.stats;
        let ghost tx = g@.table;
        proof {
            assert(dr.contains_key(d) && same_but_links(vd, dr[d].value()));
            assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(s1, k) == stat(st0, k) - (if has_ext { weight(ve, k) } else { 0 }) + (if used {
                weight(vt, k)
            } else {
                0
            }) by {}
            assert(tx.spec_count() == t0.spec_count() - (if has_ext { weight(ve, 6) } else { 0 }) + (if used { weight(vt, 6) } else { 0 }));
            assert forall|k: usize| #[trigger] tx.lookup(k) == (if used && is_key(vt, k as int) {
                Some(x)
            } else if has_ext && is_key(ve, k as int) {
                None
            } else {
                t0.lookup(k)
            }) by {}
            assert(tx.lookup(page) == Some(d));
            assert(stat(s1, 0) == stat(st0, 0) && stat(s1, 3) == stat(st0, 3));
            assert(s1.runs == st0.runs && s1.run_pages == st0.run_pages);
        }
        g.drop_extent(d, Ghost(Seq::empty()));
        let ghost s2 = g@.stats;
        proof {
            assert(stat(s2, 0) == stat(s1, 0) - weight(dr[d].value(), 0));
            assert(stat(s2, 3) == stat(s1, 3) - weight(dr[d].value(), 3));
            assert(s2.runs == st0.runs - 1 && s2.run_pages == st0.run_pages - n);
        }
        proof {
            self.lemma_pages(usable);
        }
        {
            let dd = g.desc_mut(d);
            dd.pages = need as u32;
            dd.usable = usable << self.lg_page;
        }
        let ghost vg = g@.descs@[d].value();
        proof {
            assert(same_but_links(HugeDesc { pages: need as u32, usable: (usable * pow2(self.lg_page as nat)) as usize, ..vd }, vg));
            assert(run_ok(vg, gb.lg@));
            assert(forall|i: int| 0 <= i < g@.elist@.len() ==> #[trigger] enode(g@.elist@, g@.descs@, i) == enode(g@.elist@, dr, i));
            assert(g@.table.lookup(page) is None);
            assert(g@.stats.can_add(RunState::Active, need));
        }
        g.add_extent(d);
        let ghost s3 = g@.stats;
        proof {
            assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(s3, k) == stat(s2, k) + weight(vg, k) by {}
        }
        g.stats.next_page = g.next_page;
        let ghost g1 = g@;
        proof {
            assert(g1.stats == (HugeStats { next_page: g1.stats.next_page, ..s3 }));
            assert(forall|k: int| 0 <= k < 6 ==> #[trigger] stat(g1.stats, k) == stat(s3, k));
            let t = Grow { d, vd, vg, x, has_ext, ve, used, vt, mark, nm: g1.next_page };
            assert(t.shape_ok(gb));
            assert(g1.grew(gb, t)) by {
                assert(weight(dr[d].value(), 6) == weight(vd, 6));
                assert forall|k: usize| #[trigger] g1.table.lookup(k) == t.lookup(t0, k) by {
                    assert(tx.lookup(k) == (if used && is_key(vt, k as int) {
                        Some(x)
                    } else if has_ext && is_key(ve, k as int) {
                        None
                    } else {
                        t0.lookup(k)
                    }));
                    if k == page {
                        assert(has_ext ==> !is_key(ve, k as int));
                        assert(used ==> !is_key(vt, k as int));
                    }
                }
                assert(g1.table.spec_count() == t0.spec_count() + t.delta(6));
                assert forall|k: int| 0 <= k < 6 implies #[trigger] stat(g1.stats, k) == stat(st0, k) + t.delta(k) by {
                    assert(stat(s2, k) == stat(s1, k) - weight(dr[d].value(), k));
                    assert(weight(dr[d].value(), k) == weight(vd, k));
                    assert(stat(g1.stats, k) == stat(s2, k) + weight(vg, k));
                    assert(stat(s1, k) == stat(st0, k) - (if has_ext { weight(ve, k) } else { 0 }) + (if used { weight(vt, k) } else { 0 }));
                }
                assert forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) <==> t.model(d0, y) is Some by {}
                assert forall|y: *mut HugeDesc| #[trigger] g1.descs@.contains_key(y) implies same_but_links(
                    t.model(d0, y).unwrap(),
                    g1.descs@[y].value(),
                ) by {}
                assert(g1.all@ == t.all_after(gb.all@));
                assert(g1.elist@ == t.elist_after(gb.elist@));
                assert forall|q: nat| #[trigger] g1.free.map@.contains(q) <==> gb.free.map@.contains(q) && !(t.e() <= q < t.end()) by {
                    if has_ext && t.e() <= q < t.end() && !(q < ve.start + ve.pages) {
                        // Past the extent, which then ends at the mark: no
                        // free page is there.
                        gb.lemma_free_below();
                    }
                    if !has_ext && t.e() <= q < t.end() {
                        gb.lemma_free_below();
                    }
                }
            }
            lemma_grew_active(gb, g1, t);
        }
        // The grown run's token: its extent and pages enter the run
        // authorities.
        let _gm: &mut Inner = &mut g;
        let tracked ext = _gm.runs.borrow_mut().insert((page as int, need as int));
        let tracked pages = _gm.pgs.borrow_mut().insert_set(vstd::set_lib::set_int_range(page as int, page + need));
        let tracked grown = RunToken { ext, pages };
        proof {
            assert(g@ == (Inner { runs: g@.runs, pgs: g@.pgs, ..g1 }));
            let t = Grow { d, vd, vg, x, has_ext, ve, used, vt, mark, nm: g1.next_page };
            assert(g@.grew(gb, t));
            assert(g@.base == gb.base && g@.lg == gb.lg && g@.next_page == t.nm);
            assert(g@.mark_committed());
            lemma_grew_wf(gb, g@, t);
            assert(lock_frame(lock_init(self.inner), g@));
        }
        self.sync_oldest(&g);
        // Design 7: `MADV_HUGEPAGE` on runs of a PMD or more under
        // `thp:always`, as `prepare` advises a new run.
        proof {
            self.lemma_pages(usable);
        }
        if usable * ps >= crate::thp::huge_min() {
            crate::thp::advise_huge(self.addr(page), usable * ps);
        }
        g.unlock();
        (Some((arena, need - n)), Tracked(grown))
    }
}

} // verus!
