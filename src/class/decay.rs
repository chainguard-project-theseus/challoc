//! Purge and the decay walk over the `empty` set (design 8).

use super::SlabTable;
use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use crate::purge::{Batch, Curve, Due, Left, STEPS, Settle};
use crate::slab::{DescPerm, State};
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
/// may discard. Tying such a slab to its slots' tokens is left to the
/// proofs of the free paths.
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
/// `g` they walk: the invariant, over this state's table, arena, slabs and
/// owned-slab record.
pub(super) open spec fn walk_ok(cs: &ClassState, g: Inner) -> bool {
    &&& g.inv()
    &&& g.table@ == cs.spec_table()
    &&& g.arena@ == cs.arena
    &&& g.nbits() == cs.spec_nslabs()
    &&& g.oauth@.id() == cs.spec_oloc()
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

/// The page group of `spp` slabs from slab `f` of `cs`'s class, as a
/// purge range: what [`ClassState::push_group`] pushes.
spec fn group_piece(cs: &ClassState, f: int, spp: int) -> (int, int) {
    (cs.spec_table().base + f * slab_bytes(cs), spp * slab_bytes(cs))
}

/// `x` is a page group a purge walk of `cs` (page groups of `spp` slabs,
/// pushed below slab `end`) pushed and has not settled, in the lock value
/// `g`: its first slab `f` is a group boundary, the group ends at or
/// below `end`, every slab of it is owned and not `Active`, and one is
/// in the empty set (the slab whose visit pushed it). Opaque, so a
/// settler's invariant carries it per piece as one fact.
#[verifier::opaque]
spec fn pend_ok(cs: &ClassState, g: Inner, spp: int, end: int, x: (int, int)) -> bool {
    let f = (x.0 - cs.spec_table().base) / slab_bytes(cs);
    &&& x == group_piece(cs, f, spp)
    &&& 0 <= f
    &&& f % spp == 0
    &&& f + spp <= end
    &&& forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)] f <= s < f + spp ==> g.owns(s) && g.st(s) != State::Active
    &&& exists|s: nat| f <= s < f + spp && #[trigger] g.empty.map@.contains(s)
}

/// The page group of `spp` slabs at slab `fy` is a piece of the run of
/// groups from `f0` (`cnt` slabs) exactly when it lies in the run:
/// groups start at multiples of `spp`.
proof fn lemma_group_run(base: int, sz: int, spp: int, f0: int, cnt: int, fy: int)
    requires
        sz > 0,
        spp > 0,
        f0 % spp == 0,
        fy % spp == 0,
    ensures
        crate::purge::in_run(base + f0 * sz, cnt * sz, spp * sz, (base + fy * sz, spp * sz)) <==> f0 <= fy < f0
            + cnt,
{
    assert(base + f0 * sz <= base + fy * sz <==> f0 <= fy) by (nonlinear_arith)
        requires sz > 0;
    assert(base + fy * sz < base + f0 * sz + cnt * sz <==> fy < f0 + cnt) by (nonlinear_arith)
        requires sz > 0;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(f0, spp);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(fy, spp);
    let m = fy / spp - f0 / spp;
    assert(base + fy * sz - (base + f0 * sz) == m * (spp * sz)) by (nonlinear_arith)
        requires
            f0 == spp * (f0 / spp),
            fy == spp * (fy / spp),
            m == fy / spp - f0 / spp,
    ;
    assert(spp * sz > 0) by (nonlinear_arith)
        requires spp > 0, sz > 0;
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(m, spp * sz);
}

/// Of two multiples of `m`, `a` above `b - m` is at least `b`.
proof fn lemma_mult_order(a: int, b: int, m: int)
    requires
        m > 0,
        a % m == 0,
        b % m == 0,
        a > b - m,
    ensures
        a >= b,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(a, m);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b, m);
    let (qa, qb) = (a / m, b / m);
    assert(qa >= qb) by (nonlinear_arith)
        requires a == m * qa, b == m * qb, a > b - m, m > 0;
    assert(a >= b) by (nonlinear_arith)
        requires a == m * qa, b == m * qb, qa >= qb, m > 0;
}

/// The first slab of the page group of `spp` slabs that holds slab `s`,
/// counted from a group boundary `f0`.
spec fn group_of(f0: int, spp: int, s: int) -> int {
    f0 + ((s - f0) / spp) * spp
}

/// [`group_of`] is a group boundary, at or below `s` and within a group of
/// it.
proof fn lemma_group_of(f0: int, spp: int, s: int)
    requires
        spp > 0,
        f0 % spp == 0,
        f0 <= s,
    ensures
        group_of(f0, spp, s) % spp == 0,
        f0 <= group_of(f0, spp, s) <= s < group_of(f0, spp, s) + spp,
{
    let d = s - f0;
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(d, spp);
    vstd::arithmetic::div_mod::lemma_mod_bound(d, spp);
    let q = d / spp;
    assert(q >= 0) by {
        vstd::arithmetic::div_mod::lemma_div_pos_is_pos(d, spp);
    }
    assert(q * spp >= 0) by (nonlinear_arith)
        requires q >= 0, spp > 0;
    assert(spp * q == q * spp) by (nonlinear_arith);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(q, spp);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(f0, spp);
    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(f0 / spp + q, spp);
    assert(f0 + q * spp == (f0 / spp + q) * spp) by (nonlinear_arith)
        requires f0 == spp * (f0 / spp);
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
            crate::sys::may_abort() || crate::sys::valid_aborts(),
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
    /// (`g`, the value the caller holds, and `dperms`, its descriptor
    /// permissions), which is what pins those states for this arena's
    /// slabs; a slab of another arena, or one not yet handed out, could be
    /// claimed under a lock this thread does not hold, so it would veto
    /// the purge.
    ///
    /// Neither occurs for the real lock value: `slab` is owned (the caller
    /// found it in the empty set), and so is every slab of its page group,
    /// since a page group never straddles a hand-out group (`SlabTable::new`
    /// asserts `group % slabs_per_page == 0`) and an arena owns whole
    /// hand-out groups ([`whole_groups`]), all below the watermark. The
    /// watermark and arena checks (`take_group` writes every descriptor's
    /// arena as it hands the group out) stay as run-time defences against
    /// corrupted metadata, which the proof does not need; each state is
    /// read through its descriptor's permission, as
    /// [`ClassState::state_locked`] reads it.
    ///
    /// [`whole_groups`]: super::whole_groups
    // `(first..first + spp).all(..)` spelled out as its desugaring, a
    // `next` loop with early returns: Verus takes no closure here. The
    // machine code is unchanged.
    #[allow(clippy::while_let_loop, clippy::match_like_matches_macro)]
    fn page_group_is_idle(
        &self,
        Ghost(g): Ghost<Inner>,
        Tracked(dperms): Tracked<&Map<nat, DescPerm>>,
        slab: usize,
        spp: usize,
    ) -> (r: bool)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            spp > 1,
            slab + spp <= usize::MAX,
            g.arena@ == self.arena,
            g.table@ == self.spec_table(),
            g.base(),
            g.owns(slab as nat),
            slab < self.spec_nslabs(),
            spp == self.spec_table().spec_slabs_per_page(),
            *dperms == g.dperms@,
        ensures
            r ==> forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)] (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp
                ==> g.owns(s) && g.st(s) != State::Active,
    {
        proof {
            use_type_invariant(self);
        }
        let first = slab & !(spp - 1);
        proof {
            lemma_page_group_owned(g, self.spec_table(), slab, spp);
        }
        let committed = self.table().committed_slabs();
        let mut slabs = first..first + spp;
        loop
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                first == slab & !((spp - 1) as usize),
                first <= slabs.start <= slabs.end,
                slabs.end == first + spp,
                g.base(),
                g.table@ == self.spec_table(),
                *dperms == g.dperms@,
                forall|s: nat| first <= s < first + spp ==> #[trigger] g.owns(s),
                forall|s: nat| first <= s < slabs.start ==> #[trigger] g.st(s) != State::Active,
            ensures
                forall|s: nat| first <= s < first + spp ==> #[trigger] g.st(s) != State::Active,
            decreases slabs.end - slabs.start,
        {
            let s = match slabs.next() {
                Some(s) => s,
                None => break,
            };
            if s >= committed {
                return false;
            }
            let d = self.desc(s);
            proof {
                assert(g.owns(s as nat));
                g.lemma_desc(s as nat);
            }
            // `matches!` spelled out: `verus!` does not reach into the
            // macro's arguments, so the ghost and tracked arguments there
            // would not compile.
            if !(d.arena() == self.arena && match d.state_held(Ghost(s as nat), Tracked(dperms)) {
                State::Fresh | State::Empty | State::Purged => true,
                _ => false,
            }) {
                return false;
            }
        }
        true
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
        broadcast use super::lemma_inv_counts;
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
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let r = g.stats.empty * self.class().slab_size();
        g.unlock();
        r
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
            crate::sys::may_abort() || crate::sys::valid_aborts(),
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
                crate::sys::may_abort() || crate::sys::valid_aborts(),
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
                // A pending check (`sys::may_abort_pending`) on a free's
                // tick: the state is read raw from the descriptor, not
                // through `state_locked`, and no verified invariant relates
                // the slab's live count to the sets.
                crate::sys::abort_pending(format_args!("class {}: slab {slab} in the empty set is not empty", self.class));
            }
            let dirty_at = self.dirty_locked(g, desc, slab);
            v.visit(self, g, slab, dirty_at);
            next = g.empty.map.lowest_set_from(slab + 1);
            proof {
                reveal(next_after);
                reveal(none_above);
            }
        }
    }
}

/// What a purge walk's flushes settled: slabs made `Purged`, the oldest
/// `dirty_at` among the survivors (`min_nonzero`), and whether a discard
/// failed. It settles the walk's batch ([`Settle`], through
/// [`ClassState::settle`]); its ghost half carries the walk's invariant
/// over the lock value, which the flushes inside the walk's pushes
/// change.
// Ghost fields that only the proofs read.
#[allow(dead_code)]
struct Tally {
    purged: usize,
    oldest: u64,
    failed: bool,
    /// Ghost: the page group size.
    spp: Ghost<usize>,
    /// Ghost: the slab states of the lock value the walk started from.
    states: Ghost<Map<nat, State>>,
    /// Ghost: the end of the last page group pushed (0 before the first).
    end: Ghost<usize>,
    /// Ghost: the last slab visited (-1 before the first).
    last: Ghost<int>,
    /// Ghost: no member of the empty set lies above it (the slab count
    /// until a full walk ends, its last slab after).
    top: Ghost<int>,
    /// Ghost: the walk pushes every slab it visits (a full purge with one
    /// slab per page, [`EmptyVisitor::full`]).
    full: Ghost<bool>,
    /// Ghost: the empty count of the lock value the walk started from.
    e0: Ghost<int>,
    /// Ghost: the bytes reported discarded.
    bytes: Ghost<int>,
    /// Ghost: the slabs `Empty` in a range reported not discarded.
    refused: Ghost<Set<nat>>,
}

impl Settle for Tally {
    type Owner = ClassState;

    type Ctx = Inner;

    /// `(spp, states, end, last, full, e0, top)`.
    type Conf = (usize, Map<nat, State>, usize, int, bool, int, int);

    /// The walk's invariant over the lock value `c`, the pieces `pend`
    /// pushed and not settled: the lock value is the class's and agrees
    /// with the walk's start on ownership and `Active`-ness (a purge only
    /// moves `Empty` to `Purged`); each piece pending is a page group
    /// below `end`, idle, with a slab in the empty set ([`pend_ok`]); the
    /// slabs purged and the empty count sum to the empty count at the
    /// start; a slab was purged iff a byte was reported discarded; and a
    /// full walk has every member of the empty set up to `last` pending or
    /// refused.
    closed spec fn inv(&self, o: &ClassState, c: Inner, pend: spec_fn((int, int)) -> bool) -> bool {
        let spp = self.spp@;
        &&& walk_ok(o, c)
        &&& spp == o.spec_table().spec_slabs_per_page()
        &&& spp >= 1
        &&& spp & sub(spp, 1) == 0
        &&& o.spec_nslabs() & sub(spp, 1) == 0
        &&& self.end@ & sub(spp, 1) == 0
        &&& self.end@ <= o.spec_nslabs()
        &&& -1 <= self.last@ < o.spec_nslabs()
        &&& forall|s: nat| #[trigger] c.states@.contains_key(s) == self.states@.contains_key(s)
        &&& forall|s: nat| #[trigger] c.states@.contains_key(s)
            ==> (c.st(s) == State::Active) == (self.states@[s] == State::Active)
        &&& forall|x: (int, int)| #[trigger] pend(x) ==> pend_ok(o, c, spp as int, self.end@ as int, x)
        &&& forall|x: nat| #[trigger] c.empty.map@.contains(x) ==> x <= self.top@
        &&& self.purged + c.stats.empty == self.e0@
        &&& self.e0@ <= o.spec_nslabs()
        &&& self.bytes@ >= 0
        &&& (self.purged > 0) == (self.bytes@ > 0)
        &&& self.full@ ==> spp == 1 && self.end@ == self.last@ + 1
        &&& self.full@ ==> forall|x: nat| #[trigger] c.empty.map@.contains(x) && x <= self.last@ ==> pend(
            group_piece(o, x as int, 1),
        ) || self.refused@.contains(x)
    }

    closed spec fn bytes(&self) -> int {
        self.bytes@
    }

    closed spec fn conf(&self) -> (usize, Map<nat, State>, usize, int, bool, int, int) {
        (self.spp@, self.states@, self.end@, self.last@, self.full@, self.e0@, self.top@)
    }

    #[inline(always)]
    fn settle(
        &mut self,
        o: &ClassState,
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

/// The purge half of both purge walks: the page groups pushed, their
/// discard batch and its settler, the tally.
///
/// The batch may discard the bytes of the slabs idle in the ghost states
/// of the lock value the walk started from ([`idle_byte`]); a purge only
/// moves `Empty` slabs to `Purged`, so those are the slabs idle in the
/// lock value at every flush.
struct Purger {
    spp: usize,
    /// Slabs below `pushed` are in a page group already in the batch.
    pushed: usize,
    batch: Batch,
    t: Tally,
}

impl Purger {
    /// Nothing pushed yet over page groups of `spp` slabs of table `t`,
    /// whose arena's lock value records `states` and `e0` empty slabs;
    /// `full` for a walk that pushes every slab it visits.
    #[inline(always)]
    fn new(
        spp: usize,
        Ghost(t): Ghost<SlabTable>,
        Ghost(states): Ghost<Map<nat, State>>,
        Ghost(full): Ghost<bool>,
        Ghost(e0): Ghost<int>,
        Ghost(top): Ghost<int>,
    ) -> (p: Purger)
        ensures
            p.spp == spp,
            p.pushed == 0,
            p.batch.wf(),
            p.batch.pieces() == crate::purge::none(),
            p.batch.sent() == 0,
            forall|b: int| (p.batch.free())(b) == idle_byte(t, states, b),
            p.t.purged == 0,
            p.t.oldest == 0,
            p.t.spp@ == spp,
            p.t.states@ == states,
            p.t.end@ == 0,
            p.t.last@ == -1,
            p.t.full@ == full,
            p.t.e0@ == e0,
            p.t.top@ == top,
            p.t.bytes@ == 0,
    {
        Purger {
            spp,
            pushed: 0,
            batch: Batch::new(Ghost(|b: int| idle_byte(t, states, b))),
            t: Tally {
                purged: 0,
                oldest: 0,
                failed: false,
                spp: Ghost(spp),
                states: Ghost(states),
                end: Ghost(0),
                last: Ghost(-1),
                full: Ghost(full),
                e0: Ghost(e0),
                top: Ghost(top),
                bytes: Ghost(0),
                refused: Ghost(Set::empty()),
            },
        }
    }

    /// The tally's invariant over the batch's pieces, with the batch's
    /// predicate and byte count the tally's, and `pushed` its `end`.
    spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        &&& self.spp == self.t.spp@
        &&& self.pushed == self.t.end@
        &&& self.batch.wf()
        &&& forall|b: int| #[trigger] (self.batch.free())(b) == idle_byte(cs.spec_table(), self.t.states@, b)
        &&& self.batch.sent() == self.t.bytes@
        &&& self.t.inv(cs, g, self.batch.pieces())
    }

    /// `slab` is in a page group already pushed: the walk moves past it.
    #[inline(always)]
    #[allow(unused_variables)]
    fn skip(&mut self, cs: &ClassState, Ghost(g): Ghost<Inner>, Ghost(slab): Ghost<int>)
        requires
            old(self).inv(cs, g),
            old(self).t.last@ < slab < old(self).pushed,
        ensures
            final(self).inv(cs, g),
            final(self).t.last@ == slab,
            final(self).t.full == old(self).t.full,
    {
        self.t.last = Ghost(slab);
    }

    /// Visit `slab` (stamped `dirty_at`), at or past the page groups
    /// pushed: push its page group when `purge` holds and the group is
    /// idle, else keep it and fold its stamp into `oldest`. A full walk
    /// (`purge` always, one slab per page) pushes `slab` itself.
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64, purge: bool)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(self).inv(cs, *old(g)),
            old(self).t.last@ < slab,
            old(self).pushed <= slab,
            walk_ok(cs, *old(g)),
            old(g).empty.map@.contains(slab as nat),
            next_after(*old(g), old(self).t.last@, slab as int),
            dirty_at != 0,
            old(self).t.full@ ==> purge,
        ensures
            final(self).inv(cs, *final(g)),
            final(self).t.last@ == slab,
            final(self).t.full == old(self).t.full,
            walk_ok(cs, *final(g)),
    {
        let spp = self.spp;
        proof {
            assert(g.slab_ok(slab as nat));
            lemma_page_group(slab, self.pushed, cs.spec_nslabs(), spp);
            assert(usize::MAX >> 12 == 0xf_ffff_ffff_ffffusize) by (bit_vector);
            assert(slab + spp <= usize::MAX);
        }
        if purge && (spp == 1 || cs.page_group_is_idle(Ghost(*g), Tracked(g.dperms.borrow()), slab, spp)) {
            proof {
                let first = slab & !sub(spp, 1);
                if spp == 1 {
                    assert(first == slab) by (bit_vector)
                        requires spp == 1usize, first == slab & !sub(spp, 1);
                }
                // Every slab of the group is idle: the one slab of a page
                // is in the empty set, and `page_group_is_idle` says so of
                // a group of several.
                assert forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)] first <= s < first + spp
                    implies g.owns(s) && g.st(s) != State::Active by {
                    if spp == 1 {
                        assert(s == slab && g.slab_ok(slab as nat));
                    } else {
                        assert(first == slab & !((spp - 1) as usize));
                        assert(g.owns(s));
                        assert(g.st(s) != State::Active);
                    }
                }
                assert(sub(spp, 1) == (spp - 1) as usize);
                assert(first == slab & !((spp - 1) as usize));
                // In the callee's terms, by a lemma whose query carries
                // none of this walk's context.
                lemma_group_idle(*g, slab, spp, first);
            }
            self.pushed = cs.push_group(g, slab, spp, &mut self.batch, &mut self.t);
        } else {
            self.t.oldest = min_nonzero(self.t.oldest, dirty_at);
            self.t.last = Ghost(slab as int);
        }
    }
}

/// `purge_empty`'s visitor: purge every slab stamped at or before `cutoff`.
/// With `cutoff == u64::MAX` and one slab per page it pushes every slab
/// it visits, so every member of the empty set at or below the last is
/// pending or refused.
struct PurgeOlder {
    cutoff: u64,
    p: Purger,
}

impl EmptyVisitor for PurgeOlder {
    closed spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        &&& self.p.inv(cs, g)
        &&& self.p.t.full@ == self.full()
    }

    closed spec fn last(&self) -> int {
        self.p.t.last@
    }

    closed spec fn full(&self) -> bool {
        self.cutoff == u64::MAX && self.p.spp == 1
    }

    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        if slab < self.p.pushed {
            self.p.skip(cs, Ghost(*g), Ghost(slab as int));
            return;
        }
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
    /// claim can hand out a slab between the discard and its state change;
    /// a slab whose discard fails stays `Empty` (the next purge retries
    /// it). Recomputes `oldest_empty` from the survivors. Returns the
    /// number purged, at most the slab count: each purged slab left the
    /// empty set.
    ///
    /// `left` gets the bytes discarded, nonzero iff a slab was purged, and
    /// the slabs `Empty` in the lock value released: with `cutoff ==
    /// u64::MAX`, none unless the class has several slabs per page or
    /// `madvise` refused the slab's range ([`Left::clean`]).
    pub fn purge_empty(
        &self,
        cutoff: u64,
        Tracked(left): Tracked<&mut Ghost<Left>>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: usize)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
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
        broadcast use super::lemma_inv_counts;
        proof {
            use_type_invariant(self);
        }
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let ghost full = cutoff == u64::MAX && spp == 1;
        let mut v = PurgeOlder {
            cutoff,
            p: Purger::new(
                spp,
                Ghost(self.spec_table()),
                Ghost(g@.states@),
                Ghost(full),
                Ghost(g@.stats.empty as int),
                Ghost(self.spec_nslabs() as int),
            ),
        };
        proof {
            assert(0usize & sub(spp, 1) == 0) by (bit_vector);
            assert forall|x: nat| #[trigger] g@.empty.map@.contains(x) implies x <= self.spec_nslabs() by {
                assert(g@.slab_ok(x));
            }
            reveal(pend_ok);
            assert(v.p.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut v);
        proof {
            reveal(none_above);
        }
        v.p.t.top = Ghost(v.p.t.last@);
        proof {
            assert(v.p.inv(self, g@));
        }
        v.p.batch.flush(&mut v.p.t, self, &mut g);
        self.gate.set_oldest_empty(v.p.t.oldest);
        proof {
            let (a, c) = (self.spec_arena() as int, self.spec_class() as int);
            let lv = g@;
            let t = v.p.t;
            // The slabs the released value records as owned and `Empty`:
            // by the seam's invariant, its empty set (`Inner::slab_core`).
            let slabs = ISet::new(
                |x: (int, int, int)|
                    x.0 == a && x.1 == c && x.2 >= 0 && lv.owns(x.2 as nat) && lv.st(x.2 as nat) == State::Empty,
            );
            let refused = ISet::new(|x: (int, int, int)| x.0 == a && x.1 == c && x.2 >= 0 && t.refused@.contains(x.2 as nat));
            *left = Ghost(
                Left {
                    bytes: v.p.batch.sent(),
                    arenas: Set::empty(),
                    classes: set![(a, c)],
                    slabs,
                    huge: false,
                    extents: ISet::empty(),
                    refused,
                    refused_extents: ISet::empty(),
                },
            );
            if cutoff == u64::MAX {
                if spp == 1 {
                    // Every member is at or below the last slab visited,
                    // and none is pending once the batch is flushed.
                    reveal(none_above);
                    assert(t.full@);
                    assert(t.inv(self, lv, crate::purge::none()));
                    assert forall|x: (int, int, int)| #[trigger] slabs.contains(x) implies refused.contains(x) by {
                        let s = x.2 as nat;
                        assert(lv.slab_ok(s));
                        assert(lv.empty.map@.contains(s));
                        assert(s <= t.top@);
                        assert(!(crate::purge::none())(group_piece(self, s as int, 1)));
                        assert(t.refused@.contains(s));
                    }
                } else {
                    let x = crate::layout::guard_geom(c).0;
                    if x == 0 {
                        assert(1usize << x == 1usize) by (bit_vector)
                            requires x == 0u8;
                    }
                }
            }
        }
        let r = v.p.t.purged;
        g.unlock();
        r
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
/// and not active: [`ClassState::push_group`]'s precondition. Opaque, so
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

/// The page group of owned slab `slab` (`spp` slabs from `slab & !(spp -
/// 1)`) is owned: page and hand-out groups are powers of two, a page
/// group no larger than a hand-out group (`SlabTable::inv`), so the page
/// group lies in `slab`'s hand-out group, which the arena owns whole
/// ([`whole_groups`](super::whole_groups)).
proof fn lemma_page_group_owned(g: Inner, t: SlabTable, slab: usize, spp: usize)
    requires
        g.base(),
        g.table@ == t,
        t.inv(),
        g.owns(slab as nat),
        slab < t.spec_nslabs(),
        spp == t.spec_slabs_per_page(),
    ensures
        (slab & !((spp - 1) as usize)) <= slab,
        (slab & !((spp - 1) as usize)) + spp <= t.spec_nslabs(),
        forall|s: nat| (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp ==> #[trigger] g.owns(s),
{
    broadcast use {super::table::lemma_spec_nslabs, super::table::lemma_spec_slabs_per_page};
    reveal(SlabTable::page_groups_ok);
    let (lgs, lg, n, gr) = (t.lg_slabs_per_page, t.lg_group, t.nslabs, t.group as usize);
    assert(spp == 1usize << lgs && gr == 1usize << lg && spp <= gr && spp >= 1);
    assert(lgs <= lg) by (bit_vector)
        requires spp == 1usize << lgs, gr == 1usize << lg, spp <= gr, lgs <= 26, lg < 64;
    let first = slab & !sub(spp, 1);
    assert(first <= slab && first + spp <= n) by (bit_vector)
        requires spp == 1usize << lgs, lgs <= 26, n & sub(spp, 1) == 0, slab < n, first == slab & !sub(spp, 1);
    assert forall|s: nat| first <= s < first + spp implies #[trigger] g.owns(s) by {
        let su = s as usize;
        assert(su >> lg == slab >> lg) by (bit_vector)
            requires spp == 1usize << lgs, lgs <= lg, lg < 64, lgs <= 26, first == slab & !sub(spp, 1), first <= su, su < first + spp, first + spp <= n, n <= 0x400_0000;
        assert((su >> lg) << lg <= su && su < ((su >> lg) << lg) + gr) by (bit_vector)
            requires gr == 1usize << lg, lg < 64, su < 0x400_0000;
        super::lemma_whole_group(g.states@.dom(), t, slab as nat, s);
    }
}

/// The page group of `slab` (`spp` slabs from `first`) in table `t`,
/// where `slab` is not a guard slab, is a range a purge may discard
/// ([`well_formed`](crate::purge::well_formed)): the region's base and a
/// page group's bytes are page multiples (`groups_paged`), so the group
/// starts and ends on a page boundary, and its slabs lie in the class
/// region and share `slab`'s page group, so none is a guard slab.
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
    /// Push the whole page group of `slab` (one slab when the slab is at
    /// least a page) to `batch`; once it is discarded ([`settle`]) every
    /// `Empty` slab in it becomes `Purged`, a younger one included, since
    /// nothing in the group is in use. Under the class lock; the caller
    /// checked the group is idle. Returns the slab after the group.
    ///
    /// The preconditions are the walks' (`Purger::visit`): the page group
    /// is this arena's and idle, and `slab`, in the empty set, is no guard
    /// slab, nor is any slab of its group, which is a well-formed purge
    /// range (`lemma_group_well_formed`); `slab` is past the groups
    /// already pushed, so the group is apart from the batch's pieces.
    ///
    /// [`settle`]: ClassState::settle
    fn push_group(&self, g: &mut Inner, slab: usize, spp: usize, batch: &mut Batch, t: &mut Tally) -> (r: usize)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(t).inv(self, *old(g), old(batch).pieces()),
            old(batch).wf(),
            forall|b: int| #[trigger] (old(batch).free())(b) == idle_byte(self.spec_table(), old(t).states@, b),
            spp == old(t).spp@,
            old(t).end@ <= slab,
            old(t).last@ < slab,
            (slab & !((spp - 1) as usize)) + spp <= self.spec_nslabs(),
            group_idle(*old(g), slab, spp),
            old(g).empty.map@.contains(slab as nat),
            next_after(*old(g), old(t).last@, slab as int),
        ensures
            final(t).inv(self, *final(g), final(batch).pieces()),
            final(batch).wf(),
            final(batch).free() == old(batch).free(),
            final(batch).sent() - old(batch).sent() == final(t).bytes() - old(t).bytes(),
            final(t).spp == old(t).spp,
            final(t).states == old(t).states,
            final(t).full == old(t).full,
            final(t).last@ == slab,
            final(t).end@ == r,
            r == (slab & !((spp - 1) as usize)) + spp,
    {
        proof {
            use_type_invariant(self);
            reveal(group_idle);
        }
        let first = slab & !(spp - 1);
        let ghost sz = slab_bytes(self);
        let ghost t0 = *t;
        let ghost new = group_piece(self, first as int, spp as int);
        proof {
            let tb = self.spec_table();
            lemma_page_group(slab, t0.end@, self.spec_nslabs(), spp);
            assert(sub(spp, 1) == (spp - 1) as usize);
            lemma_run_fits(tb, first, spp);
            lemma_slab_bytes_pos(tb);
            assert((first as int + spp as int) * sz == first as int * sz + spp as int * sz) by (nonlinear_arith);
            assert(first as int * sz >= 0 && spp as int * sz >= 0) by (nonlinear_arith)
                requires sz >= 0;
            assert(sz * spp == spp as int * sz) by (nonlinear_arith);
            // Every byte of the group is in one of its slabs, each idle.
            assert forall|b: int| tb.base + first as int * sz <= b < tb.base + first as int * sz + spp as int * sz
                implies #[trigger] (batch.free())(b) by {
                crate::purge::lemma_unit_of(tb.base as int, sz, first as int, spp as int, b);
                let s = (b - tb.base) / sz;
                assert(g.owns(s as nat) && g.st(s as nat) != State::Active);
                assert(idle_byte(tb, g.states@, b));
                lemma_idle_same(tb, g.states@, t0.states@, b);
            }
            // The group is a well-formed purge range: `slab`, in the
            // empty set, is no guard slab (`Inner::slab_core`).
            assert(g.slab_ok(slab as nat));
            lemma_group_well_formed(tb, slab, spp, first);
            // Apart from the groups pushed before, all below `end`.
            reveal(pend_ok);
            assert forall|x: (int, int)| #[trigger] (batch.pieces())(x) implies x.0 + x.1 <= new.0 || new.0 + new.1
                <= x.0 by {
                assert(pend_ok(self, *g, spp as int, t0.end@ as int, x));
                let f = (x.0 - tb.base) / sz;
                assert(f + spp <= t0.end@ <= first);
                assert(x.0 + x.1 == tb.base + (f + spp) * sz) by (nonlinear_arith)
                    requires x.0 == tb.base + f * sz, x.1 == spp * sz;
                assert((f + spp) * sz <= first * sz) by (nonlinear_arith)
                    requires f + spp <= first, sz >= 0;
            }
            // The group is a piece the walk may leave pending: its first
            // slab, and `slab` in the empty set.
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(first as int, sz);
            assert(sz * first == first as int * sz) by (nonlinear_arith);
            assert(first % spp == 0) by (bit_vector)
                requires spp >= 1, spp & sub(spp, 1) == 0, first == slab & !sub(spp, 1);
            assert(pend_ok(self, *g, spp as int, (first + spp) as int, new));
        }
        t.end = Ghost((first + spp) as usize);
        t.last = Ghost(slab as int);
        proof {
            reveal(pend_ok);
            let pend = crate::purge::either(batch.pieces(), crate::purge::one(new));
            assert forall|x: (int, int)| #[trigger] pend(x) implies pend_ok(self, *g, spp as int, t.end@ as int, x) by {
                if x != new {
                    assert(pend_ok(self, *g, spp as int, t0.end@ as int, x));
                }
            }
            if t.full@ {
                assert(spp == 1);
                assert(first == slab) by (bit_vector)
                    requires spp == 1usize, first == slab & !sub(spp, 1);
                reveal(next_after);
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) && x <= t.last@ implies pend(
                    group_piece(self, x as int, 1),
                ) || t.refused@.contains(x) by {
                    if x < slab {
                        assert(x <= t0.last@);
                    }
                }
            }
            assert(t.inv(self, *g, pend));
        }
        batch.push(self.slab_addr(first), self.class().slab_size() * spp, t, self, g);
        first + spp
    }

    /// The flush's report on `[addr, addr + len)`, page groups pushed by
    /// [`push_group`] (under the class lock since): discarded, every
    /// `Empty` slab in it becomes `Purged`; not, they stay `Empty` and
    /// count among the survivors for `oldest_empty`.
    ///
    /// [`push_group`]: ClassState::push_group
    // The loop carries a Verus invariant.
    #[allow(clippy::while_let_loop, clippy::too_many_arguments)]
    #[verifier::rlimit(40)]
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
            forall|x: (int, int)| #[trigger] crate::purge::in_run(addr as int, len as int, piece, x) ==> pend(x),
        ensures
            final(t).inv(self, *final(g), crate::purge::minus(pend, addr as int, len as int, piece)),
            final(t).conf() == old(t).conf(),
            final(t).bytes() == old(t).bytes() + if done {
                len as int
            } else {
                0
            },
    {
        let ghost t0 = *t;
        let ghost g1 = *g;
        let ghost tb = self.spec_table();
        let ghost sz = slab_bytes(self);
        let ghost spp = t0.spp@ as int;
        proof {
            use_type_invariant(self);
            reveal(pend_ok);
            lemma_slab_bytes_pos(tb);
            vstd::arithmetic::div_mod::lemma_small_mod(0, piece as nat);
            assert(crate::purge::in_run(addr as int, len as int, piece, (addr as int, piece)));
            assert(pend_ok(self, g1, spp, t0.end@ as int, (addr as int, piece)));
        }
        // The run's first group: `addr` is its first slab's, `len` the
        // run's whole groups.
        let ghost f0 = (addr - tb.base) / sz;
        let ghost k = len as int / piece;
        let ghost cnt = k * spp;
        proof {
            assert(addr == tb.base + f0 * sz && piece == spp * sz);
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(len as int, piece);
            assert(len == piece * k);
            assert(len == cnt * sz) by (nonlinear_arith)
                requires len == piece * k, piece == spp * sz, cnt == k * spp;
            assert(cnt >= spp) by (nonlinear_arith)
                requires len == piece * k, piece == spp * sz, cnt == k * spp, len >= piece, spp >= 1, sz > 0;
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(f0, sz);
            vstd::arithmetic::div_mod::lemma_div_multiples_vanish(cnt, sz);
            assert(sz * f0 == f0 * sz && sz * cnt == cnt * sz) by (nonlinear_arith);
            assert(cnt % spp == 0) by {
                vstd::arithmetic::div_mod::lemma_mod_multiples_basic(k, spp);
            }
            // Every slab of the run is in one of its groups, each pending.
            assert(f0 % spp == 0);
            lemma_run_idle(self, g1, t0, pend, addr as int, len as int, piece, f0, cnt, (f0 + cnt - 1) as nat);
        }
        let slab_size = self.class().slab_size();
        let first = (addr - self.slab_addr(0)) / slab_size;
        t.failed |= !done;
        proof {
            assert(first == f0);
            assert(len / slab_size == cnt);
        }
        // `for s in first..first + len / slab_size`, spelled out: Verus
        // rejects `continue` in a `for` loop.
        let mut slabs = first..first + len / slab_size;
        loop
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                g.inv(),
                g.nbits() == g1.nbits(),
                g.table == g1.table,
                g.arena == g1.arena,
                g.oauth@.id() == g1.oauth@.id(),
                g.table@ == tb,
                tb == self.spec_table(),
                slabs.end == f0 + cnt,
                f0 <= slabs.start <= slabs.end,
                f0 + cnt <= self.spec_nslabs(),
                t0.inv(self, g1, pend),
                sz > 0,
                sz == slab_bytes(self),
                addr == tb.base + f0 * sz,
                piece == spp * sz,
                spp == t0.spp@ as int,
                len == cnt * sz,
                f0 % spp == 0,
                forall|x: (int, int)| #[trigger] crate::purge::in_run(addr as int, len as int, piece, x) ==> pend(x),
                forall|s: nat| #[trigger] g.states@.contains_key(s) == g1.states@.contains_key(s),
                forall|s: nat| #[trigger] g1.states@.contains_key(s)
                    ==> (g.st(s) == State::Active) == (g1.st(s) == State::Active),
                forall|x: nat| #[trigger] g.empty.map@.contains(x) ==> g1.empty.map@.contains(x),
                forall|x: nat| !(f0 <= x < slabs.start) ==> (#[trigger] g.empty.map@.contains(x) == g1.empty.map@.contains(x)),
                done ==> forall|x: nat| f0 <= x < slabs.start ==> !(#[trigger] g.empty.map@.contains(x)),
                !done ==> *g == g1,
                !done ==> forall|x: nat| f0 <= x < slabs.start && #[trigger] g1.empty.map@.contains(x) ==> t.refused@.contains(x),
                forall|x: nat| #[trigger] t0.refused@.contains(x) ==> t.refused@.contains(x),
                t.purged + g.stats.empty == t0.e0@,
                t.purged >= t0.purged,
                t0.e0@ <= self.spec_nslabs(),
                !done ==> t.purged == t0.purged,
                done && t.purged == t0.purged ==> forall|x: nat| f0 <= x < slabs.start ==> !(#[trigger] g1.empty.map@.contains(x)),
                t.spp == t0.spp,
                t.states == t0.states,
                t.end == t0.end,
                t.last == t0.last,
                t.full == t0.full,
                t.e0 == t0.e0,
                t.bytes == t0.bytes,
            ensures
                slabs.start >= slabs.end,
            decreases slabs.end - slabs.start,
        {
            let s = match slabs.next() {
                Some(s) => s,
                None => break,
            };
            let d = self.desc(s);
            proof {
                lemma_run_idle(self, g1, t0, pend, addr as int, len as int, piece, f0, cnt, s as nat);
                assert(g1.owns(s as nat));
                assert(g.owns(s as nat));
                assert(g.slab_ok(s as nat));
            }
            if self.state_locked(&*g, d, s) != State::Empty {
                continue;
            }
            if !done {
                t.oldest = min_nonzero(t.oldest, self.dirty_locked(&*g, d, s));
                proof {
                    t.refused@ = t.refused@.insert(s as nat);
                }
                continue;
            }
            let ghost g0 = *g;
            proof {
                assert(g0.slab_ok(s as nat));
                super::lemma_set_len(g0.purged, g0.nbits(), s as nat);
                // The empty count is the empty set's size (`Inner::base`),
                // which holds `s`: the decrement below cannot underflow.
                assert(g0.empty.map@.contains(s as nat));
                vstd::set::lemma_set_contains_len(g0.empty.map@, s as nat);
            }
            self.set_state_locked(g, d, s, State::Purged);
            self.set_dirty_locked(g, d, s, 0);
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
                .unwrap_or_else(|| -> (r: usize)
                    requires
                        false,
                    { slab_count_underflow("empty") });
            g.stats.purged += 1;
            t.purged += 1;
            proof {
                assert forall|x: nat| #[trigger] g.slab_ok(x) by {
                    assert(g0.slab_ok(x));
                }
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) implies g1.empty.map@.contains(x) by {
                    assert(g0.empty.map@.contains(x));
                }
                assert forall|x: nat| #[trigger] g1.states@.contains_key(x)
                    implies (g.st(x) == State::Active) == (g1.st(x) == State::Active) by {
                    assert(g0.states@.contains_key(x));
                }
            }
        }
        if done {
            t.bytes = Ghost(t.bytes@ + len);
        }
        proof {
            let rest = crate::purge::minus(pend, addr as int, len as int, piece);
            // A piece left pending is a group outside the run, whose slabs
            // the loop did not touch.
            assert forall|y: (int, int)| #[trigger] rest(y) implies pend_ok(self, *g, spp, t.end@ as int, y) by {
                assert(pend(y));
                assert(pend_ok(self, g1, spp, t0.end@ as int, y));
                let fy = (y.0 - tb.base) / sz;
                lemma_group_run(tb.base as int, sz, spp, f0, cnt, fy);
                assert(!(f0 <= fy < f0 + cnt));
                let w = choose|w: nat| fy <= w < fy + spp && #[trigger] g1.empty.map@.contains(w);
                if f0 <= w < f0 + cnt {
                    assert(f0 % spp == 0);
                    if fy < f0 {
                        lemma_mult_order(fy, f0, spp);
                    }
                }
                assert(g.empty.map@.contains(w));
                assert forall|s: nat| #![trigger g.owns(s)] #![trigger g.st(s)] fy <= s < fy + spp implies g.owns(s)
                    && g.st(s) != State::Active by {
                    assert(g1.owns(s) && g1.st(s) != State::Active);
                }
            }
            if done {
                // The run's first group had a slab in the empty set: the
                // loop purged it.
                let w = choose|w: nat| f0 <= w < f0 + spp && #[trigger] g1.empty.map@.contains(w);
                assert(f0 <= w < f0 + cnt);
            }
            if t.full@ {
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) && x <= t.last@ implies rest(
                    group_piece(self, x as int, 1),
                ) || t.refused@.contains(x) by {
                    assert(g1.empty.map@.contains(x));
                    lemma_group_run(tb.base as int, sz, 1, f0, cnt, x as int);
                    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(x as int, 1);
                    vstd::arithmetic::div_mod::lemma_mod_multiples_basic(f0, 1);
                    if !(f0 <= x < f0 + cnt) {
                        assert(pend(group_piece(self, x as int, 1)) || t0.refused@.contains(x));
                    }
                }
            }
            assert(t.inv(self, *g, rest));
        }
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
        self.p.inv(cs, g) && !self.p.t.full@
    }

    closed spec fn last(&self) -> int {
        self.p.t.last@
    }

    closed spec fn full(&self) -> bool {
        false
    }

    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64) {
        if slab < self.p.pushed {
            self.p.skip(cs, Ghost(*g), Ghost(slab as int));
            return;
        }
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
    /// Decay walk (design 8): purge the oldest `Empty` slabs until
    /// the number left is within the curve's allowance at `now`, the sum
    /// over the `Empty` slabs of the weight of their age, rounded up. Slabs
    /// are purged oldest step first; within the cut-off step the highest
    /// addresses go, so the lowest, which a claim takes first, stay. The
    /// discards go out in batches under the class lock, as `purge_empty`;
    /// a failed one leaves its slabs `Empty` and backs the gate off
    /// (`Decayer::back_off`). Recomputes `oldest_empty` from the
    /// survivors. Returns the number purged.
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32, Tracked(ord): Tracked<&mut LockOrder>) -> usize
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use super::lemma_inv_counts;
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
            g.unlock();
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
            p: Purger::new(spp, Ghost(self.spec_table()), Ghost(g@.states@), Ghost(false), Ghost(g@.stats.empty as int), Ghost(self.spec_nslabs() as int)),
        };
        proof {
            assert(0usize & sub(spp, 1) == 0) by (bit_vector);
            assert forall|x: nat| #[trigger] g@.empty.map@.contains(x) implies x <= self.spec_nslabs() by {
                assert(g@.slab_ok(x));
            }
            reveal(pend_ok);
            assert(v.p.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush(&mut v.p.t, self, &mut g);
        if v.p.t.failed {
            self.gate.back_off(now);
        }
        self.gate.set_oldest_empty(v.p.t.oldest);
        let r = v.p.t.purged;
        g.unlock();
        r
    }

    /// Decay step (design 8): the gate lets `purge_curve` run over
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
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
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
}

} // verus!

verus! {

/// Every slab of a run a class settle is handed (`cnt` slabs from `f0`,
/// whole page groups pending in the tally `t`) is owned, not `Active` and
/// below the slab count: it lies in one of the run's groups, each a piece
/// pending ([`pend_ok`]).
proof fn lemma_run_idle(
    cs: &ClassState,
    g: Inner,
    t: Tally,
    pend: spec_fn((int, int)) -> bool,
    addr: int,
    len: int,
    piece: int,
    f0: int,
    cnt: int,
    s: nat,
)
    requires
        t.inv(cs, g, pend),
        f0 <= s < f0 + cnt,
        slab_bytes(cs) > 0,
        addr == cs.spec_table().base + f0 * slab_bytes(cs),
        piece == (t.spp@ as int) * slab_bytes(cs),
        len == cnt * slab_bytes(cs),
        f0 % (t.spp@ as int) == 0,
        forall|x: (int, int)| #[trigger] crate::purge::in_run(addr, len, piece, x) ==> pend(x),
    ensures
        g.owns(s),
        g.st(s) != State::Active,
        s < cs.spec_nslabs(),
{
    reveal(pend_ok);
    let spp = t.spp@ as int;
    let sz = slab_bytes(cs);
    let tb = cs.spec_table();
    let f = group_of(f0, spp, s as int);
    lemma_group_of(f0, spp, s as int);
    lemma_group_run(tb.base as int, sz, spp, f0, cnt, f);
    let y = group_piece(cs, f, spp);
    assert(pend(y));
    assert(pend_ok(cs, g, spp, t.end@ as int, y));
    vstd::arithmetic::div_mod::lemma_div_multiples_vanish(f, sz);
    assert(sz * f == f * sz) by (nonlinear_arith);
    assert((y.0 - tb.base) / sz == f);
}

} // verus!
