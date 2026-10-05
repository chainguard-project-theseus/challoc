//! Purge and the decay walk over the `empty` set (design section 8).

use super::SlabTable;
use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::lock::LockOrder;
#[cfg(verus_only)]
use crate::lock::{axiom_held_below, rank_class};
use crate::purge::{Batch, Curve, Left, STEPS};
use crate::slab::State;
use vstd::prelude::*;

verus! {

/// The older of two `dirty_at` stamps where 0 means "none yet": the fold
/// of the purge walks over the `Empty` slabs they keep, feeding the gate's `oldest_empty` (0 when no slab is `Empty`).
/// An `Empty` slab's stamp is never 0 (`on_live_zero` writes
/// `now.max(1)`), so 0 is unambiguous.
pub(super) fn min_nonzero(cur: u64, t: u64) -> (m: u64)
    requires
        t != 0,
    ensures
        m == if cur == 0 || t < cur { t } else { cur },
        m != 0,
{
    debug_assert!(t != 0, "min_nonzero: an Empty slab's dirty_at is never 0");
    if cur == 0 || t < cur { t } else { cur }
}

/// Slabs counted in steps `k` and older of an age histogram.
pub(super) open spec fn older(counts: [u32; STEPS], k: int) -> int
    decreases STEPS - k,
{
    if k >= STEPS { 0 } else { counts[k] + older(counts, k + 1) }
}

/// `older` grows toward the young end and is at most `u32::MAX` per step.
proof fn lemma_older(counts: [u32; STEPS], k: int)
    requires
        0 <= k <= STEPS,
    ensures
        0 <= older(counts, k) <= (STEPS - k) * 0xffff_ffff,
        forall|j: int| k <= j <= STEPS ==> #[trigger] older(counts, j) <= older(counts, k),
    decreases STEPS - k,
{
    if k < STEPS {
        lemma_older(counts, k + 1);
    }
}

/// The cut-off step of a decay walk that must purge `need` of the slabs
/// counted in `counts` (the age histogram, older steps at higher indices):
/// the oldest step such that it and the older ones hold at least `need`, and how many of that step
/// may stay.
fn cut_step(counts: &[u32; STEPS], need: usize) -> (r: (usize, usize))
    requires
        0 < need <= older(*counts, 0),
    ensures
        r.0 < STEPS,
        r.1 < counts[r.0 as int],
        older(*counts, r.0 as int) - r.1 == need,
{
    proof {
        lemma_older(*counts, 0);
    }
    let mut cut = STEPS;
    let mut acc = 0usize;
    while acc < need
        invariant
            0 < need <= older(*counts, 0),
            0 <= older(*counts, 0) <= STEPS * 0xffff_ffff,
            forall|j: int| 0 <= j <= STEPS ==> #[trigger] older(*counts, j) <= older(*counts, 0),
            cut <= STEPS,
            acc == older(*counts, cut as int),
            cut < STEPS ==> acc - counts[cut as int] < need,
        decreases cut,
    {
        cut -= 1;
        acc += counts[cut] as usize;
    }
    (cut, acc - need)
}

/// `older` of a histogram with one more slab in step `j`: one more from
/// step `j` down.
proof fn lemma_older_bump(c: [u32; STEPS], d: [u32; STEPS], j: int, k: int)
    requires
        0 <= j < STEPS,
        0 <= k <= STEPS,
        d[j] == c[j] + 1,
        forall|i: int| 0 <= i < STEPS && i != j ==> d[i] == c[i],
    ensures
        older(d, k) == older(c, k) + if k <= j { 1int } else { 0int },
    decreases STEPS - k,
{
    if k < STEPS {
        lemma_older_bump(c, d, j, k + 1);
    }
}

/// `older` of an empty histogram.
proof fn lemma_older_zero(c: [u32; STEPS], k: int)
    requires
        0 <= k <= STEPS,
        forall|i: int| 0 <= i < STEPS ==> c[i] == 0,
    ensures
        older(c, k) == 0,
    decreases STEPS - k,
{
    if k < STEPS {
        lemma_older_zero(c, k + 1);
    }
}

/// Page groups of `p` slabs (a power of two) tile a region of `n` slabs
/// (a multiple of `p`): the group of slab `s` starts at `s & !(p - 1)`,
/// at or past any group boundary `e <= s`, and ends at a group boundary
/// inside the region.
proof fn lemma_page_group(s: usize, e: usize, n: usize, p: usize)
    requires
        p >= 1,
        p & sub(p, 1) == 0,
        e & sub(p, 1) == 0,
        n & sub(p, 1) == 0,
        e <= s,
        s < n,
    ensures
        e <= s & !sub(p, 1),
        s & !sub(p, 1) <= s,
        s < (s & !sub(p, 1)) + p,
        (s & !sub(p, 1)) + p <= n,
        add(s & !sub(p, 1), p) & sub(p, 1) == 0,
{
    assert(s & !sub(p, 1) <= s && s - (s & !sub(p, 1)) < p && (s & !sub(p, 1)) & sub(p, 1) == 0) by (bit_vector)
        requires p >= 1, p & sub(p, 1) == 0;
    assert(e <= s & !sub(p, 1)) by (bit_vector)
        requires p >= 1, p & sub(p, 1) == 0, e & sub(p, 1) == 0, e <= s;
    assert((s & !sub(p, 1)) + p <= n) by (bit_vector)
        requires p >= 1, p & sub(p, 1) == 0, n & sub(p, 1) == 0, s < n;
    assert(add(s & !sub(p, 1), p) & sub(p, 1) == 0) by (bit_vector)
        requires p >= 1, p & sub(p, 1) == 0, (s & !sub(p, 1)) + p <= n;
}

/// Bytes of a slab of `cs`'s class.
spec fn slab_bytes(cs: &ClassState) -> int {
    (1usize << class_table()[cs.spec_class() as int].lg_slab) as int
}

/// Byte `b` lies in a slab of table `t` that `states`, the ghost states
/// of an arena's class lock value, records as owned by the arena and not
/// `Active`: `Fresh`, `Empty` or `Purged`. What a class purge's [`Batch`]
/// may discard (whole-allocator theorem "Purge never discards live
/// data", `verus/THEOREMS.md`, where tying such a slab to its slots'
/// tokens is open).
pub closed spec fn idle_byte(t: SlabTable, states: Map<nat, State>, b: int) -> bool {
    let sz = (1usize << class_table()[t.spec_class() as int].lg_slab) as int;
    let s = (b - t.base) / sz;
    &&& t.base <= b
    &&& s < t.spec_nslabs()
    &&& states.contains_key(s as nat)
    &&& states[s as nat] != State::Active
}

/// Two lock values that agree on which slabs the arena owns and which of
/// them are `Active` agree on [`idle_byte`].
proof fn lemma_idle_same(t: SlabTable, s1: Map<nat, State>, s2: Map<nat, State>, b: int)
    requires
        forall|s: nat| #[trigger] s1.contains_key(s) == s2.contains_key(s),
        forall|s: nat| #[trigger] s1.contains_key(s) ==> (s1[s] == State::Active) == (s2[s] == State::Active),
    ensures
        idle_byte(t, s1, b) == idle_byte(t, s2, b),
{
    let sz = (1usize << class_table()[t.spec_class() as int].lg_slab) as int;
    let s = ((b - t.base) / sz) as nat;
    assert(s1.contains_key(s) == s2.contains_key(s));
    if s1.contains_key(s) {
        assert((s1[s] == State::Active) == (s2[s] == State::Active));
    }
}

/// What [`ClassState::walk_empty`] and its visitors keep of the lock value
/// `g` they walk: the invariant, over this state's table, arena and slabs.
pub(super) open spec fn walk_ok(cs: &ClassState, g: Inner) -> bool {
    &&& g.inv()
    &&& g.table@ == cs.spec_table()
    &&& g.arena@ == cs.arena
    &&& g.nbits() == cs.spec_nslabs()
}

/// `slab` is the lowest member of `g`'s empty set above `last`: the slab
/// [`ClassState::walk_empty`] visits after `last`. Opaque, as are the two
/// below, so a visit's query carries each as one fact.
#[verifier::opaque]
pub(super) open spec fn next_after(g: Inner, last: int, slab: int) -> bool {
    forall|x: nat| #[trigger] g.empty.map@.contains(x) && x > last ==> x >= slab
}

/// No member of `g`'s empty set lies above `last`: where the walk ends.
#[verifier::opaque]
pub(super) open spec fn none_above(g: Inner, last: int) -> bool {
    forall|x: nat| #[trigger] g.empty.map@.contains(x) ==> x <= last
}

/// Every member of `g`'s empty set lies above `last`: a purge walk that
/// purged every slab it visited.
#[verifier::opaque]
spec fn all_above(g: Inner, last: int) -> bool {
    forall|x: nat| #[trigger] g.empty.map@.contains(x) ==> x > last
}

/// A purge of `slab`, the lowest member above `last`, that took it out of
/// the empty set and added none keeps every member above the walk.
proof fn lemma_all_above(g0: Inner, g1: Inner, last: int, slab: int)
    requires
        all_above(g0, last),
        next_after(g0, last, slab),
        forall|x: nat| #[trigger] g1.empty.map@.contains(x) ==> g0.empty.map@.contains(x),
        slab >= 0,
        !g1.empty.map@.contains(slab as nat),
    ensures
        all_above(g1, slab),
{
    reveal(all_above);
    reveal(next_after);
    assert forall|x: nat| #[trigger] g1.empty.map@.contains(x) implies x > slab by {
        assert(g0.empty.map@.contains(x));
    }
}

/// A purge walk's count `n` and its batch `b` agree: it purged a slab iff
/// it pushed a byte (`sent() + total()`, never negative). Opaque, for
/// [`Purger::inv`].
#[verifier::opaque]
spec fn counted(n: usize, b: Batch) -> bool {
    &&& b.sent() + b.total() >= 0
    &&& (n > 0) == (b.sent() + b.total() > 0)
}

/// A purge of `k >= 1` slabs that pushed `d > 0` bytes keeps [`counted`].
proof fn lemma_counted_step(n: usize, k: usize, b0: Batch, b1: Batch, d: int)
    requires
        counted(n, b0),
        k >= 1,
        n + k <= usize::MAX,
        d > 0,
        b1.sent() + b1.total() == b0.sent() + b0.total() + d,
    ensures
        counted((n + k) as usize, b1),
{
    reveal(counted);
}

/// A walk that ended (`none_above`) with every member above it
/// (`all_above`) leaves the empty set empty.
proof fn lemma_walked_empty(g: Inner, last: int)
    requires
        none_above(g, last),
        all_above(g, last),
    ensures
        g.empty.map@ == vstd::set::Set::<nat>::empty(),
{
    reveal(none_above);
    reveal(all_above);
    assert(g.empty.map@ =~= vstd::set::Set::<nat>::empty());
}

/// A visitor of [`ClassState::walk_empty`]: the state of a walk (a
/// histogram, a count of slabs purged and their batch) as a struct, so
/// the walk's writes to it are visible to Verus, which does not model a
/// closure's writes to its captures.
pub(super) trait EmptyVisitor {
    /// The visitor's own invariant, over the lock value it walks.
    spec fn inv(&self, cs: &ClassState, g: Inner) -> bool;

    /// The last slab visited (-1 before the first); the walk visits in
    /// increasing order.
    spec fn last(&self) -> int;

    /// Whether the walk purges every slab it visits; no visit changes it.
    spec fn full(&self) -> bool;

    /// Visit `slab` of the empty set (stamped `dirty_at`) under the class
    /// lock (`g`), keeping the lock value's invariant; a purge takes slabs
    /// out of the empty set. `slab` is the lowest member above the last
    /// slab visited.
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).inv(cs, *old(g)),
            old(self).last() < slab,
            walk_ok(cs, *old(g)),
            old(g).empty.map@.contains(slab as nat),
            next_after(*old(g), old(self).last(), slab as int),
            dirty_at != 0,
        ensures
            final(self).inv(cs, *final(g)),
            final(self).last() == slab,
            final(self).full() == old(self).full(),
            walk_ok(cs, *final(g)),
    ;
}

impl ClassState {
    /// Whether the page group of `slab` (`spp` slabs, `spp > 1`) can be
    /// discarded: every slab in it is committed, owned by this arena, and
    /// `Fresh`, `Empty` or `Purged`. Called under this arena's class lock
    /// (`g`), which is what pins those states for this arena's slabs; a
    /// slab of another arena, or one not yet handed out by
    /// `next_fresh_slab`, could be claimed under a lock this thread does
    /// not hold, so it vetoes the purge.
    ///
    /// The owner is read from the descriptor here where the release path
    /// reads the owner word: they agree because a page group never
    /// straddles a hand-out group (`SlabTable::new` asserts `group %
    /// slabs_per_page == 0`), so every slab here is under one owner word,
    /// and `take_group` writes that word and every descriptor's arena
    /// once, as it hands the group out, and nothing rewrites either
    /// (`table::slab_owner`).
    ///
    /// Trusted: that agreement is what the `ensures` rests on (the ghost
    /// `g` is the value of the class lock the caller holds, whose owner
    /// words say which slabs this arena owns), and, for the states, the
    /// agreement of an owned slab's descriptor with `g` while the lock is
    /// held, which [`ClassState::state_locked`] proves through the
    /// descriptor permissions `g` holds (`Inner::descs`) and this body,
    /// with `g` a ghost copy, reads without them; the iterator and
    /// closure stay as written.
    #[verifier::external_body]
    fn page_group_is_idle(&self, Ghost(g): Ghost<Inner>, slab: usize, spp: usize) -> (r: bool)
        requires
            crate::sys::may_abort(),
            spp > 1,
            slab + spp <= usize::MAX,
            g.arena@ == self.arena,
            g.table@ == self.spec_table(),
        ensures
            r ==> forall|s: nat| (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp
                ==> #[trigger] g.owns(s) && g.st(s) != State::Active,
    {
        let first = slab & !(spp - 1);
        let committed = self.table().committed_slabs();
        (first..first + spp).all(|s| {
            if s >= committed {
                return false;
            }
            let d = self.desc(s);
            d.arena() == self.arena
                && matches!(d.state(), State::Fresh | State::Empty | State::Purged)
        })
    }

    /// `dirty_at` of the oldest `Empty` slab as the decay driver sees it
    /// (0 when none; see the gate).
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold):
    /// at most the class region, which ends below `2^57`.
    pub fn empty_bytes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            r <= 0x200_0000_0000_0000,
    {
        broadcast use {super::axiom_class_lock, super::axiom_class_lock_inv, super::lemma_inv_counts};
        proof {
            use_type_invariant(self);
            super::lemma_slots_fit(super::table_at(self.table));
            let n = self.spec_nslabs() as int;
            let p = (1usize << class_table()[self.spec_class() as int].lg_slab) as int;
            assert forall|e: int| 0 <= e <= n implies #[trigger] (e * p) <= 0x200_0000_0000_0000 by {
                assert(e * p <= n * p) by (nonlinear_arith)
                    requires 0 <= e <= n, p >= 0;
            }
        }
        self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare())).stats.empty * self.class().slab_size()
    }
}

impl ClassState {
    /// Walk this arena's empty set in address order under the class lock
    /// (`g`): `v.visit(self, g, slab, dirty_at)` for every slab in it,
    /// after the check that the slab is what the set says (`Empty`, `live
    /// == 0`; anything else is a heap inconsistency and aborts). The
    /// visitor may take slabs out of the set, including ones ahead of the
    /// walk (a purge takes the whole page group), and the walk does not
    /// visit those. The visitor keeps the invariant; the walk ends because
    /// every visit moves past the slab it visited. It ends with no member
    /// of the set above the last slab visited.
    pub(super) fn walk_empty<V: EmptyVisitor>(&self, g: &mut Inner, v: &mut V)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            walk_ok(self, *old(g)),
            old(v).inv(self, *old(g)),
            old(v).last() == -1,
        ensures
            walk_ok(self, *final(g)),
            final(v).inv(self, *final(g)),
            final(v).full() == old(v).full(),
            none_above(*final(g), final(v).last()),
    {
        let ghost f0 = v.full();
        let mut next = g.empty.map.lowest_set();
        proof {
            reveal(next_after);
            reveal(none_above);
        }
        while let Some(slab) = next
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                walk_ok(self, *g),
                v.inv(self, *g),
                v.full() == f0,
                next matches Some(x) ==> v.last() < x && g.empty.map@.contains(x as nat),
                next matches Some(x) ==> next_after(*g, v.last(), x as int),
                next is None ==> none_above(*g, v.last()),
            ensures
                none_above(*g, v.last()),
            decreases
                match next {
                    Some(x) => self.spec_nslabs() - x,
                    None => 0,
                },
        {
            proof {
                assert(g.slab_ok(slab as nat));
            }
            let desc = self.desc(slab);
            if !(desc.state() == State::Empty && desc.live() == 0) {
                crate::sys::abort_fmt(format_args!("class {}: slab {slab} in the empty set is not empty", self.class));
            }
            let dirty_at = self.dirty_locked(g, desc, Ghost(slab));
            v.visit(self, g, slab, dirty_at);
            next = g.empty.map.lowest_set_from(slab + 1);
            proof {
                reveal(next_after);
                reveal(none_above);
            }
        }
    }
}

/// The purge half of both purge walks: the page groups purged, their
/// discard batch and the oldest stamp among the slabs kept.
///
/// The batch bound `purge_group` needs: the walk visits in increasing
/// order, and a purge takes every member of the empty set in its page
/// group, so each page group pushed starts at or past the end of the
/// last one (`end`) and the batch holds at most the bytes of the slabs
/// below `end`, which is inside the class region, below `2^57`.
///
/// The batch may discard the bytes of the slabs idle in `states`, the
/// ghost states of the lock value the walk started from ([`idle_byte`]);
/// a purge only moves `Empty` slabs to `Purged`, so those are the slabs
/// idle in the lock value at every flush.
struct Purger {
    spp: usize,
    n: usize,
    batch: Batch,
    oldest: u64,
    /// Ghost: the end of the last page group pushed (0 before the first).
    end: Ghost<usize>,
    /// Ghost: the last slab visited (-1 before the first).
    last: Ghost<int>,
    /// Ghost: the slab states of the lock value the walk started from.
    #[allow(dead_code)]
    states: Ghost<Map<nat, State>>,
}

impl Purger {
    /// Nothing purged yet over page groups of `spp` slabs of table `t`,
    /// whose arena's lock value records `states`.
    #[inline(always)]
    fn new(spp: usize, Ghost(t): Ghost<SlabTable>, Ghost(states): Ghost<Map<nat, State>>) -> (p: Purger)
        ensures
            p.spp == spp,
            p.n == 0,
            p.batch.wf(),
            p.batch.total() == 0,
            p.oldest == 0,
            p.end@ == 0,
            p.last@ == -1,
            p.states@ == states,
            forall|b: int| (p.batch.free())(b) == idle_byte(t, states, b),
            p.batch.sent() == 0,
            counted(p.n, p.batch),
    {
        proof {
            reveal(counted);
        }
        Purger {
            spp,
            n: 0,
            batch: Batch::new(Ghost(|b: int| idle_byte(t, states, b))),
            oldest: 0,
            end: Ghost(0),
            last: Ghost(-1),
            states: Ghost(states),
        }
    }

    /// `spp` is `cs`'s page group size; see the type for the rest.
    spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        &&& self.spp == cs.spec_table().spec_slabs_per_page()
        &&& self.spp >= 1
        &&& self.spp & sub(self.spp, 1) == 0
        &&& cs.spec_nslabs() & sub(self.spp, 1) == 0
        &&& self.end@ & sub(self.spp, 1) == 0
        &&& self.end@ <= cs.spec_nslabs()
        &&& self.n <= self.end@
        &&& self.batch.wf()
        &&& self.batch.total() <= self.end@ as int * slab_bytes(cs)
        &&& -1 <= self.last@ < cs.spec_nslabs()
        &&& forall|x: nat| #[trigger] g.empty.map@.contains(x) && x > self.last@ ==> x >= self.end@
        &&& forall|b: int| #[trigger] (self.batch.free())(b) == idle_byte(cs.spec_table(), self.states@, b)
        &&& forall|s: nat| #[trigger] g.states@.contains_key(s) == self.states@.contains_key(s)
        &&& forall|s: nat| #[trigger] g.states@.contains_key(s)
            ==> (g.st(s) == State::Active) == (self.states@[s] == State::Active)
        &&& counted(self.n, self.batch)
    }

    /// Visit `slab` (stamped `dirty_at`): purge its page group when
    /// `purge` holds and the group is idle, else keep it and fold its
    /// stamp into `oldest`. With one slab per page, `purge` always purges
    /// `slab`: a walk that purged every slab before it still has no
    /// member of the empty set at or below it.
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64, purge: bool)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(self).inv(cs, *old(g)),
            old(self).last@ < slab,
            walk_ok(cs, *old(g)),
            old(g).empty.map@.contains(slab as nat),
            dirty_at != 0,
        ensures
            final(self).inv(cs, *final(g)),
            final(self).last@ == slab,
            walk_ok(cs, *final(g)),
            old(self).spp == 1 && purge && all_above(*old(g), old(self).last@) && next_after(
                *old(g),
                old(self).last@,
                slab as int,
            ) ==> all_above(*final(g), slab as int),
    {
        let spp = self.spp;
        let ghost last0 = self.last@;
        let ghost first = slab & !sub(spp, 1);
        proof {
            assert(g.slab_ok(slab as nat));
            lemma_page_group(slab, self.end@, cs.spec_nslabs(), spp);
            assert(sub(spp, 1) == (spp - 1) as usize);
            assert(usize::MAX >> 12 == 0xf_ffff_ffff_ffffusize) by (bit_vector);
            assert(slab + spp <= usize::MAX);
        }
        if purge && (spp == 1 || cs.page_group_is_idle(Ghost(*g), slab, spp)) {
            let ghost g0 = *g;
            proof {
                if spp == 1 {
                    assert(first == slab);
                }
                use_type_invariant(cs);
                lemma_run_fits(cs.spec_table(), 0, self.end@);
                assert(self.end@ as int * slab_bytes(cs) <= 0x200_0000_0000_0000);
                assert forall|b: int| idle_byte(cs.spec_table(), g.states@, b)
                    implies #[trigger] (self.batch.free())(b) by {
                    lemma_idle_same(cs.spec_table(), g.states@, self.states@, b);
                }
                // Every slab of the group is idle: the one slab of a page
                // is in the empty set, and `page_group_is_idle` says so of
                // a group of several.
                assert forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)] first <= s < first + spp
                    implies g.owns(s) && g.st(s) != State::Active by {
                    if spp == 1 {
                        assert(first == slab && s == slab && g.slab_ok(slab as nat));
                    } else {
                        assert(first == slab & !((spp - 1) as usize));
                        assert(g.owns(s));
                        assert(g.st(s) != State::Active);
                    }
                }
                assert(first == slab & !((spp - 1) as usize));
                // In the callee's terms, by a lemma whose query carries
                // none of this walk's context.
                lemma_group_idle(*g, slab, spp, first);
            }
            let ghost n0 = self.n;
            let ghost b0 = self.batch;
            let k = cs.purge_group(g, slab, spp, &mut self.batch);
            proof {
                assert(self.end@ as int * slab_bytes(cs) <= first as int * slab_bytes(cs)) by (nonlinear_arith)
                    requires self.end@ <= first, slab_bytes(cs) >= 0;
                assert(first as int * slab_bytes(cs) + spp as int * slab_bytes(cs) == (first + spp) as int * slab_bytes(cs))
                    by (nonlinear_arith);
                // `slab` is in its own group, and was `Empty`.
                assert(k >= 1);
                lemma_slab_bytes_pos(cs.spec_table());
                assert(slab_bytes(cs) > 0);
                assert(spp as int * slab_bytes(cs) > 0) by (nonlinear_arith)
                    requires spp >= 1, slab_bytes(cs) > 0;
                lemma_counted_step(n0, k, b0, self.batch, spp as int * slab_bytes(cs));
                if spp == 1 && all_above(g0, last0) && next_after(g0, last0, slab as int) {
                    assert(!g.empty.map@.contains(slab as nat));
                    lemma_all_above(g0, *g, last0, slab as int);
                }
            }
            self.n += k;
            self.end = Ghost((first + spp) as usize);
            proof {
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) && x > slab implies x >= self.end@ by {
                    assert(g0.empty.map@.contains(x));
                }
                assert forall|s: nat| #[trigger] g.states@.contains_key(s)
                    implies (g.st(s) == State::Active) == (self.states@[s] == State::Active) by {
                    assert(g0.states@.contains_key(s));
                }
            }
        } else {
            self.oldest = min_nonzero(self.oldest, dirty_at);
        }
        self.last = Ghost(slab as int);
    }
}

/// `purge_empty`'s visitor: purge every slab stamped at or before `cutoff`.
/// With `cutoff == u64::MAX` and one slab per page it purges every slab
/// it visits, so no member of the empty set is left at or below the last.
struct PurgeOlder {
    cutoff: u64,
    p: Purger,
}

impl EmptyVisitor for PurgeOlder {
    closed spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        &&& self.p.inv(cs, g)
        &&& self.full() ==> all_above(g, self.p.last@)
    }

    closed spec fn last(&self) -> int {
        self.p.last@
    }

    closed spec fn full(&self) -> bool {
        self.cutoff == u64::MAX && self.p.spp == 1
    }

    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        let purge = dirty_at <= self.cutoff;
        self.p.visit(cs, g, slab, dirty_at, purge);
    }
}

} // verus!

verus! {

impl ClassState {
    /// Purge every `Empty` slab whose `dirty_at` is at or before `cutoff`
    /// (`u64::MAX` for all of them) and make it `Purged`. The discards go
    /// out in batches of [`purge::MAX_BATCH`] under the class lock, so no
    /// claim can hand out a slab between its state change and the discard.
    /// Recomputes `oldest_empty` from the survivors. Returns the number
    /// purged, at most the slab count: each purged slab is below the end
    /// of the last page group pushed, inside the class region.
    ///
    /// `left` gets the bytes discarded, nonzero iff a slab was purged, and
    /// the slabs `Empty` in the lock value released: with `cutoff ==
    /// u64::MAX`, none unless the class has several slabs per page
    /// ([`Left::clean`]).
    pub fn purge_empty(
        &self,
        cutoff: u64,
        Tracked(left): Tracked<&mut Ghost<Left>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            r <= self.spec_nslabs(),
            final(left)@.bytes >= 0,
            (r > 0) == (final(left)@.bytes > 0),
            final(left)@.arenas == Set::<int>::empty(),
            final(left)@.classes == set![(self.spec_arena() as int, self.spec_class() as int)],
            forall|t: (int, int, int)| #[trigger] final(left)@.slabs.contains(t)
                ==> t.0 == self.spec_arena() && t.1 == self.spec_class(),
            !final(left)@.huge,
            final(left)@.extents == ISet::<int>::empty(),
            cutoff == u64::MAX ==> final(left)@.clean(),
    {
        broadcast use {super::axiom_class_lock, super::axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let mut v = PurgeOlder { cutoff, p: Purger::new(spp, Ghost(self.spec_table()), Ghost(g@.states@)) };
        proof {
            assert(0usize & sub(spp, 1) == 0) by (bit_vector);
            reveal(all_above);
            assert(v.p.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush();
        self.gate.set_oldest_empty(v.p.oldest);
        proof {
            reveal(counted);
            let (a, c) = (self.spec_arena() as int, self.spec_class() as int);
            let lv = g@;
            // The slabs the released value records as owned and `Empty`:
            // by the seam's invariant, its empty set (`Inner::slab_core`).
            let slabs = ISet::new(
                |t: (int, int, int)|
                    t.0 == a && t.1 == c && t.2 >= 0 && lv.owns(t.2 as nat) && lv.st(t.2 as nat) == State::Empty,
            );
            *left = Ghost(
                Left {
                    bytes: v.p.batch.sent(),
                    arenas: Set::empty(),
                    classes: set![(a, c)],
                    slabs,
                    huge: false,
                    extents: ISet::empty(),
                },
            );
            if cutoff == u64::MAX {
                if spp == 1 {
                    lemma_walked_empty(lv, v.p.last@);
                    assert forall|t: (int, int, int)| !slabs.contains(t) by {
                        if t.2 >= 0 {
                            assert(lv.slab_ok(t.2 as nat));
                        }
                    }
                    assert(slabs =~= ISet::<(int, int, int)>::empty());
                } else {
                    let x = crate::layout::guard_geom(c).0;
                    if x == 0 {
                        assert(1usize << x == 1usize) by (bit_vector)
                            requires x == 0u8;
                    }
                }
            }
        }
        v.p.n
    }
}

} // verus!

verus! {

/// A run of `k` slabs from `first` lies inside the class region, which
/// ends at or below `2^57`.
proof fn lemma_run_fits(t: SlabTable, first: usize, k: usize)
    requires
        t.inv(),
        first + k <= t.spec_nslabs(),
    ensures
        t.base as int + (first as int + k as int) * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int)
            <= 0x200_0000_0000_0000,
{
    broadcast use {super::table::lemma_spec_nslabs, super::table::lemma_spec_class};
    let c = class_table()[t.spec_class() as int];
    let lg = c.lg_slab as usize;
    let n = t.spec_nslabs();
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    let m57 = ((1usize << 57usize) - t.base) as usize;
    assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m57);
    let l8 = c.lg_slab;
    assert((1usize << l8) == (1usize << lg)) by (bit_vector)
        requires lg == l8 as usize;
    let p = (1usize << lg) as int;
    assert(n as int * p <= m57);
    assert((first as int + k as int) * p <= n as int * p) by (nonlinear_arith)
        requires first + k <= n, p >= 0;
}

/// A slab of a well-formed table has at least one byte.
proof fn lemma_slab_bytes_pos(t: SlabTable)
    requires
        t.inv(),
    ensures
        (1usize << class_table()[t.spec_class() as int].lg_slab) as int > 0,
{
    broadcast use {super::table::lemma_spec_class, crate::vspec::class::axiom_class_table_wf};
    let c = class_table()[t.spec_class() as int];
    let l8 = c.lg_slab;
    let lg = l8 as usize;
    assert(c.wf());
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
    assert((1usize << l8) == (1usize << lg)) by (bit_vector)
        requires lg == l8 as usize;
}

/// Every slab of the page group of `slab` (`spp` slabs) is this arena's
/// and not active: [`ClassState::purge_group`]'s precondition. Opaque, so
/// the walk that establishes it ([`Purger::visit`]) hands it over as one
/// fact, not as a quantifier the solver must match under that walk's
/// context.
// Each conjunct is a trigger: a query that needs only the `st` conjunct
// may have no `owns` term to match.
#[verifier::opaque]
spec fn group_idle(g: Inner, slab: usize, spp: usize) -> bool {
    forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)]
        (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp
        ==> g.owns(s) && g.st(s) != State::Active
}

/// [`group_idle`] from the same fact stated over the group's first slab.
proof fn lemma_group_idle(g: Inner, slab: usize, spp: usize, first: usize)
    requires
        first == slab & !((spp - 1) as usize),
        forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)]
            first <= s < first + spp ==> g.owns(s) && g.st(s) != State::Active,
    ensures
        group_idle(g, slab, spp),
{
    reveal(group_idle);
}

/// The page group of `slab` (`spp` slabs from `first`) in table `t`,
/// where `slab` is not a guard slab, is a range a purge may discard
/// ([`well_formed`](crate::purge::well_formed)): the region's base and a
/// page group's bytes are page multiples (`groups_paged`), so the group
/// starts and ends on a page boundary, and its slabs lie in the class
/// region and share `slab`'s page group, so none is a guard slab
/// (whole-allocator theorem "Purge batches are well formed").
proof fn lemma_group_well_formed(t: SlabTable, slab: usize, spp: usize, first: usize)
    requires
        t.inv(),
        spp == t.spec_slabs_per_page(),
        first == slab & !((spp - 1) as usize),
        first + spp <= t.spec_nslabs(),
        !t.spec_is_guard(slab),
    ensures
        crate::purge::well_formed(
            t.base as int + first as int * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int),
            spp as int * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int),
        ),
{
    broadcast use {super::table::lemma_spec_nslabs, super::table::lemma_spec_class, super::table::lemma_spec_slabs_per_page};
    reveal(super::table::groups_paged);
    reveal(SlabTable::page_groups_ok);
    reveal(crate::layout::guard_of);
    reveal(crate::purge::well_formed);
    let c = t.class as int;
    let r = crate::layout::process_regions();
    let sz = (1usize << class_table()[c].lg_slab) as int;
    let page = crate::purge::page_bytes();
    let base = t.base as int;
    lemma_slab_bytes_pos(t);
    let lgs = t.lg_slabs_per_page;
    assert(spp == 1usize << lgs);
    // `first` is a multiple of `spp`, so its byte offset is a multiple of
    // the group's bytes, a page multiple.
    let m = sub(spp, 1);
    assert(first % spp == 0) by (bit_vector)
        requires spp == 1usize << lgs, lgs <= 26, m == sub(spp, 1), first == slab & !m;
    let q = first as int / spp as int;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(first as int, spp as int);
    let g = spp as int * sz;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(g, page);
    let k = g / page;
    assert(first as int * sz == (q * k) * page) by (nonlinear_arith)
        requires
            first as int == spp as int * q,
            g == spp as int * sz,
            g == page * k,
    ;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(q * k, page);
    vstd::arithmetic::div_mod::lemma_add_mod_noop(base, first as int * sz, page);
    vstd::arithmetic::div_mod::lemma_small_mod(0, page as nat);
    assert(g > 0) by (nonlinear_arith)
        requires spp >= 1, sz > 0, g == spp as int * sz;
    // Every byte lies in a slab of the group, in the region and not a
    // guard slab.
    assert(first as int * sz + g == (first + spp) as int * sz) by (nonlinear_arith)
        requires g == spp as int * sz;
    assert((first + spp) as int * sz <= t.nslabs as int * sz) by (nonlinear_arith)
        requires first + spp <= t.nslabs, sz > 0;
    assert forall|b: int| base + first as int * sz <= b < base + first as int * sz + g
        implies #[trigger] crate::purge::discardable(b) by {
        crate::purge::lemma_unit_of(base, sz, first as int, spp as int, b);
        let s = (b - base) / sz;
        let x = s as usize;
        assert(x >> lgs == slab >> lgs) by (bit_vector)
            requires spp == 1usize << lgs, lgs <= 26, m == sub(spp, 1), first == slab & !m, first <= x,
                x < first + spp;
        t.lemma_guard_slab(slab as int);
        assert(!crate::layout::guard_slab(c, s));
        assert(crate::purge::class_byte(c, b));
    }
}

impl ClassState {
    /// Discard the whole page group of `slab` (one slab when the slab is
    /// at least a page) and make every `Empty` slab in it `Purged`, a
    /// younger one included, since nothing in the group is in use. Under
    /// the class lock. Returns the number of slabs purged; afterwards no
    /// slab of the group is in the empty set.
    ///
    /// The preconditions are the walks' (`Purger::visit`): the page group
    /// is this arena's (with one slab per page, `slab` is in this arena's
    /// empty set; with more, `page_group_is_idle` read every slab's owner
    /// word; either way `slab` is in the empty set, so not a guard slab, and
    /// neither is any slab of its group, which is a well-formed purge range,
    /// `lemma_group_well_formed`), and the batch holds at most the bytes of
    /// the class region below the group, which ends below `2^57`.
    // The loop carries a Verus invariant.
    #[allow(clippy::while_let_loop)]
    fn purge_group(&self, g: &mut Inner, slab: usize, spp: usize, batch: &mut Batch) -> (n: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(g).inv(),
            old(g).table@ == self.spec_table(),
            old(g).nbits() == self.spec_nslabs(),
            spp >= 1,
            (slab & !((spp - 1) as usize)) + spp <= self.spec_nslabs(),
            group_idle(*old(g), slab, spp),
            old(g).empty.map@.contains(slab as nat),
            spp == self.spec_table().spec_slabs_per_page(),
            old(batch).wf(),
            old(batch).total() <= 0x200_0000_0000_0000,
            forall|b: int| idle_byte(self.spec_table(), old(g).states@, b) ==> #[trigger] (old(batch).free())(b),
        ensures
            final(g).inv(),
            final(g).nbits() == old(g).nbits(),
            final(g).table == old(g).table,
            final(g).arena == old(g).arena,
            forall|x: nat| #[trigger] final(g).empty.map@.contains(x) ==> old(g).empty.map@.contains(x),
            forall|x: nat| (slab & !((spp - 1) as usize)) <= x < (slab & !((spp - 1) as usize)) + spp
                ==> !(#[trigger] final(g).empty.map@.contains(x)),
            final(batch).wf(),
            final(batch).total() <= old(batch).total() + spp as int * slab_bytes(self),
            final(batch).free() == old(batch).free(),
            final(batch).sent() + final(batch).total() == old(batch).sent() + old(batch).total() + spp as int
                * slab_bytes(self),
            forall|s: nat| #[trigger] final(g).states@.contains_key(s) == old(g).states@.contains_key(s),
            forall|s: nat| #[trigger] old(g).states@.contains_key(s)
                ==> (final(g).st(s) == State::Active) == (old(g).st(s) == State::Active),
            n <= spp,
            old(g).empty.map@.contains(slab as nat) && (slab & !((spp - 1) as usize)) <= slab
                < (slab & !((spp - 1) as usize)) + spp ==> n >= 1,
    {
        proof {
            use_type_invariant(self);
            reveal(group_idle);
        }
        let first = slab & !(spp - 1);
        let ghost sz = (1usize << class_table()[self.spec_class() as int].lg_slab) as int;
        proof {
            lemma_run_fits(self.spec_table(), first, spp);
            assert((first as int + spp as int) * sz == first as int * sz + spp as int * sz) by (nonlinear_arith);
            assert(first as int * sz >= 0 && spp as int * sz >= 0) by (nonlinear_arith)
                requires sz >= 0;
            assert(sz * spp == spp as int * sz) by (nonlinear_arith);
            assert(self.spec_table().base as int + first as int * sz + spp as int * sz <= 0x200_0000_0000_0000);
            // Every byte of the group is in one of its slabs, each idle.
            lemma_slab_bytes_pos(self.spec_table());
            let t = self.spec_table();
            assert forall|b: int| t.base + first as int * sz <= b < t.base + first as int * sz + spp as int * sz
                implies #[trigger] (batch.free())(b) by {
                crate::purge::lemma_unit_of(t.base as int, sz, first as int, spp as int, b);
                let s = (b - t.base) / sz;
                assert(old(g).owns(s as nat) && old(g).st(s as nat) != State::Active);
                assert(idle_byte(t, old(g).states@, b));
            }
            // The group is a well-formed purge range: `slab`, in the
            // empty set, is no guard slab (`Inner::slab_core`).
            assert(old(g).slab_ok(slab as nat));
            lemma_group_well_formed(t, slab, spp, first);
        }
        batch.push(self.slab_addr(first), self.class().slab_size() * spp);
        proof {
            assert(sz * spp == spp as int * sz) by (nonlinear_arith);
        }
        let ghost g1 = *g;
        let mut n = 0;
        // `for s in first..first + spp`, spelled out: Verus rejects
        // `continue` in a `for` loop.
        let mut slabs = first..first + spp;
        loop
            invariant
                crate::sys::may_abort(),
                g.inv(),
                g.nbits() == self.spec_nslabs(),
                g.table@ == self.spec_table(),
                g.table == g1.table,
                g.arena == g1.arena,
                slabs.end == first + spp,

                first <= slabs.start <= slabs.end,
                first + spp <= self.spec_nslabs(),
                n <= slabs.start - first,
                forall|s: nat| first <= s < first + spp ==> #[trigger] g.owns(s),
                forall|x: nat| #[trigger] g.empty.map@.contains(x) ==> g1.empty.map@.contains(x),
                forall|x: nat| first <= x < slabs.start ==> !(#[trigger] g.empty.map@.contains(x)),
                // `slab`, in the empty set, is purged when the loop reaches it.
                g1.empty.map@.contains(slab as nat) && first <= slab < first + spp && slabs.start <= slab
                    ==> g.empty.map@.contains(slab as nat),
                g1.empty.map@.contains(slab as nat) && first <= slab < first + spp && slabs.start > slab ==> n >= 1,
                forall|x: nat| #[trigger] g.states@.contains_key(x) == g1.states@.contains_key(x),
                forall|x: nat| #[trigger] g1.states@.contains_key(x)
                    ==> (g.st(x) == State::Active) == (g1.st(x) == State::Active),
            ensures
                slabs.start >= slabs.end,
            decreases slabs.end - slabs.start,
        {
            let s = match slabs.next() {
                Some(s) => s,
                None => break,
            };
            let d = self.desc(s);
            if self.state_locked(&*g, d, Ghost(s)) != State::Empty {
                proof {
                    assert(g.owns(s as nat));
                    assert(g.slab_ok(s as nat));
                }
                continue;
            }
            let ghost g0 = *g;
            proof {
                assert(g0.owns(s as nat));
                assert(g0.slab_ok(s as nat));
                super::lemma_set_len(g0.purged, g0.nbits(), s as nat);
            }
            self.set_state_locked(g, d, Ghost(s), State::Purged);
            self.set_dirty_locked(g, d, Ghost(s), 0);
            // Spelled out, not `empty_take` and `purged_add`: paired that
            // way the two stores reorder, and the release build of
            // unrelated code then changes shape (how it addresses two
            // merged statics; `scripts/codegen-diff.sh`).
            g.empty.map.clear(s);
            g.purged.map.set(s);
            g.stats.empty = g
                .stats
                .empty
                .checked_sub(1)
                .unwrap_or_else(|| slab_count_underflow("empty"));
            g.stats.purged += 1;
            n += 1;
            proof {
                assert forall|x: nat| #[trigger] g.slab_ok(x) by {
                    assert(g0.slab_ok(x));
                }
                assert forall|x: nat| first <= x < first + spp implies #[trigger] g.owns(x) by {
                    assert(g0.owns(x));
                }
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) implies g1.empty.map@.contains(x) by {
                    assert(g0.empty.map@.contains(x));
                }
                assert forall|x: nat| first <= x < slabs.start implies !(#[trigger] g.empty.map@.contains(x)) by {
                    if x != s {
                        assert(!g0.empty.map@.contains(x));
                    }
                }
                assert forall|x: nat| #[trigger] g1.states@.contains_key(x)
                    implies (g.st(x) == State::Active) == (g1.st(x) == State::Active) by {
                    assert(g0.states@.contains_key(x));
                }
            }
        }
        n
    }
}

/// `purge_curve`'s first pass: the age histogram of the empty set (steps
/// of `curve` at `now`), its size, its weighted sum and its oldest stamp.
struct Histogram<'a> {
    now: u64,
    curve: &'a Curve,
    counts: [u32; STEPS],
    total: usize,
    weighted: u64,
    oldest: u64,
    /// Ghost: the last slab visited (-1 before the first).
    last: Ghost<int>,
}

impl<'a> EmptyVisitor for Histogram<'a> {
    /// `total` counts the slabs visited, at most one per slab up to
    /// `last`, and is the histogram's sum; each visit adds at most
    /// `u32::MAX` to `weighted`.
    closed spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        &&& self.total == older(self.counts, 0)
        &&& self.total <= self.last@ + 1
        &&& -1 <= self.last@ < cs.spec_nslabs()
        &&& cs.spec_nslabs() <= 0x400_0000
        &&& forall|k: int| 0 <= k < STEPS ==> #[trigger] self.counts[k] <= self.total
        &&& self.weighted <= self.total * 0xffff_ffff
    }

    closed spec fn last(&self) -> int {
        self.last@
    }

    closed spec fn full(&self) -> bool {
        false
    }

    #[allow(unused_variables)]
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        let step = self.curve.step(self.now, dirty_at);
        let ghost c0 = self.counts;
        self.counts[step] += 1;
        proof {
            lemma_older_bump(c0, self.counts, step as int, 0);
        }
        self.total += 1;
        self.weighted += Curve::weight(step);
        self.oldest = min_nonzero(self.oldest, dirty_at);
        self.last = Ghost(slab as int);
    }
}

/// `purge_curve`'s second pass: purge every slab in a step older than
/// `cut`, and all but the first `keep` of step `cut`.
struct PurgeCurve<'a> {
    now: u64,
    curve: &'a Curve,
    cut: usize,
    keep: usize,
    p: Purger,
}

impl<'a> EmptyVisitor for PurgeCurve<'a> {
    closed spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        self.p.inv(cs, g)
    }

    closed spec fn last(&self) -> int {
        self.p.last@
    }

    closed spec fn full(&self) -> bool {
        false
    }

    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        let step = self.curve.step(self.now, dirty_at);
        let purge = step > self.cut
            || (step == self.cut && {
                if self.keep > 0 {
                    self.keep -= 1;
                    false
                } else {
                    true
                }
            });
        self.p.visit(cs, g, slab, dirty_at, purge);
    }
}

} // verus!

verus! {

impl ClassState {
    /// Decay walk (design section 8): purge the oldest `Empty` slabs until
    /// the number left is within the curve's allowance at `now`, the sum
    /// over the `Empty` slabs of the weight of their age, rounded up. Slabs
    /// are purged oldest step first; within the cut-off step the highest
    /// addresses go, so the lowest, which a claim takes first, stay. The
    /// discards go out in batches under the class lock, as `purge_empty`.
    /// Recomputes `oldest_empty` from the survivors. Returns the number
    /// purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use {super::axiom_class_lock, super::axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        // First pass: the age histogram of the empty set and its allowance.
        let mut h = Histogram { now, curve, counts: [0u32; STEPS], total: 0, weighted: 0, oldest: 0, last: Ghost(-1) };
        proof {
            lemma_older_zero(h.counts, 0);
            broadcast use super::table::lemma_spec_nslabs;
            assert(self.spec_nslabs() <= 0x400_0000);
            assert(forall|k: int| 0 <= k < STEPS ==> #[trigger] h.counts[k] <= h.total);
            assert(h.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut h);
        // The cgroup policy's retention cap (design 11): this walk's share
        // of the process-wide cap, on top of the curve.
        let mut allow = Curve::allowance(h.weighted);
        if keep16 < crate::policy::KEEP_ALL {
            proof {
                crate::policy::frac_one_value();
            }
            allow = allow.min(crate::policy::mul16(h.total as u64, keep16) as usize);
        }
        if h.total <= allow {
            self.gate.set_oldest_empty(h.oldest);
            return 0;
        }
        // The cut-off step: every slab in an older step goes and all but
        // the first `keep` of this one.
        let need = h.total - allow;
        let (cut, keep) = cut_step(&h.counts, need);
        // Second pass: purge.
        let mut v = PurgeCurve {
            now,
            curve,
            cut,
            keep,
            p: Purger::new(spp, Ghost(self.spec_table()), Ghost(g@.states@)),
        };
        proof {
            assert(0usize & sub(spp, 1) == 0) by (bit_vector);
            assert(v.p.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush();
        self.gate.set_oldest_empty(v.p.oldest);
        v.p.n
    }

    /// Decay step (design section 8): the gate runs `purge_curve` over
    /// the empty set at most once per step of the curve and returns the
    /// class's next deadline, `None` when nothing is `Empty`. Takes the
    /// lock only when a walk is due.
    #[cfg(test)]
    pub fn decay(&self, now: u64, decay_ns: u64) -> Option<u64> {
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL, Tracked::assume_new())
    }

    /// [`decay`](ClassState::decay) with the cgroup policy's keep ratio
    /// (design 11): the walk keeps at most `keep16 / 65536` of its `Empty`
    /// slabs on top of the curve's allowance.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u64>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            r matches Some(t) ==> t >= now,
            exists|oldest: u64, next: u64, after: u64|
                r == #[trigger] crate::purge::Decayer::spec_decay(now, decay_ns, oldest, next, after),
    {
        self.gate.decay(now, decay_ns, |now, curve| {
            // Premise (lock order): the closure cannot carry the caller's
            // token; `ord`, which `decay_with` requires, bounds the same
            // locks (this thread's, unchanged since the call).
            let tracked mut ord = axiom_held_below(rank_class(0, 0));
            self.purge_curve(now, curve, keep16, Tracked(&mut ord));
        })
    }
}

} // verus!
