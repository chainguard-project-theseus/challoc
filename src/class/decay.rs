//! Purge and the decay walk over the `empty` set (design section 8).

#[cfg(verus_only)]
use super::SlabTable;
use super::abort::slab_count_underflow;
use super::{ClassState, Inner};
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::purge::{Batch, Curve, STEPS};
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

/// What [`ClassState::walk_empty`] and its visitors keep of the lock value
/// `g` they walk: the invariant, over this state's table, arena and slabs.
pub(super) open spec fn walk_ok(cs: &ClassState, g: Inner) -> bool {
    &&& g.inv()
    &&& g.table@ == cs.spec_table()
    &&& g.arena@ == cs.arena
    &&& g.nbits() == cs.spec_nslabs()
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

    /// Visit `slab` of the empty set (stamped `dirty_at`) under the class
    /// lock (`g`), keeping the lock value's invariant; a purge takes slabs
    /// out of the empty set.
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64)
        requires
            old(self).inv(cs, *old(g)),
            old(self).last() < slab,
            walk_ok(cs, *old(g)),
            old(g).empty.map@.contains(slab as nat),
            dirty_at != 0,
        ensures
            final(self).inv(cs, *final(g)),
            final(self).last() == slab,
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
    /// The owner is read from the group's owner word, as on the release
    /// path ([`owner_locked`]), which says exactly whether the slab is
    /// this arena's. Its descriptor's arena agrees for a non-guard slab
    /// (`take_group` writes both together and `transfer_group` rewrites
    /// the word and every descriptor's arena in one hold of both class
    /// locks; a page group never straddles a hand-out group, `SlabTable::new`
    /// asserts `group % slabs_per_page == 0`, so one word covers it), and a
    /// page group holding an `Empty` slab has no guard slab (guards are
    /// whole page groups).
    ///
    /// [`owner_locked`]: ClassState::owner_locked
    fn page_group_is_idle(&self, g: &Inner, slab: usize, spp: usize) -> (r: bool)
        requires
            spp > 1,
            slab + spp <= usize::MAX,
            g.arena@ == self.arena,
            g.table@ == self.spec_table(),
        ensures
            r ==> forall|s: nat| (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp
                ==> #[trigger] g.owns(s),
    {
        let first = slab & !(spp - 1);
        proof {
            let m = (spp - 1) as usize;
            assert(slab & !m <= slab) by (bit_vector);
        }
        let committed = self.table().committed_slabs();
        let r = (first..first + spp).all(|s: usize| -> (ok: bool)
            requires
                g.arena@ == self.arena,
                g.table@ == self.spec_table(),
            ensures
                ok ==> g.owns(s as nat),
        {
            if s >= committed {
                return false;
            }
            let d = self.desc(s);
            self.owner_locked(g, s) == self.arena && matches!(d.state(), State::Fresh | State::Empty | State::Purged)
        });
        proof {
            if r {
                assert forall|s: nat| first <= s < first + spp implies #[trigger] g.owns(s) by {
                    let i = s - first;
                    let it = first..(first + spp) as usize;
                    assert(vstd::std_specs::iter::IteratorSpec::remaining(&it)[i] == s);
                }
            }
        }
        r
    }

    /// `dirty_at` of the oldest `Empty` slab as the decay driver sees it
    /// (0 when none; see the gate).
    pub fn oldest_empty(&self) -> u64 {
        self.gate.oldest_empty()
    }

    /// Bytes of this arena's `Empty` slabs of the class (one lock hold):
    /// at most the class region, which ends below `2^57`.
    pub fn empty_bytes(&self) -> (r: usize)
        ensures
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
        self.inner.lock().stats.empty * self.class().slab_size()
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
    /// every visit moves past the slab it visited.
    pub(super) fn walk_empty<V: EmptyVisitor>(&self, g: &mut Inner, v: &mut V)
        requires
            walk_ok(self, *old(g)),
            old(v).inv(self, *old(g)),
            old(v).last() == -1,
        ensures
            walk_ok(self, *final(g)),
            final(v).inv(self, *final(g)),
    {
        let mut next = g.empty.map.lowest_set();
        while let Some(slab) = next
            invariant
                walk_ok(self, *g),
                v.inv(self, *g),
                next matches Some(x) ==> v.last() < x && g.empty.map@.contains(x as nat),
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
                #[cfg(verus_only)]
                not_empty(self.class, slab);
                #[cfg(not(verus_only))]
                panic!("class {}: slab {slab} in the empty set is not empty", self.class);
            }
            let dirty_at = self.dirty_locked(g, desc, Ghost(slab));
            v.visit(self, g, slab, dirty_at);
            next = g.empty.map.lowest_set_from(slab + 1);
        }
    }
}

/// `walk_empty`'s abort: a slab of the empty set whose descriptor says
/// it is not empty (`live` is not modelled, so the check stays).
///
/// Verification only (`verus_only`): Verus sees this call where the build
/// keeps the `panic!` in place, because the inlined helper changed the
/// code of the purge walks `walk_empty` is inlined into
/// (`Arena::purge_all`, `Arena::decay_with`) on both architectures.
/// Trusted: the `-> !` is the whole specification, and the `panic!` it
/// stands for never returns; the two copies of the message must be kept
/// in step.
#[cfg(verus_only)]
#[verifier::external_body]
fn not_empty(class: u16, slab: usize) -> ! {
    panic!("class {}: slab {slab} in the empty set is not empty", class)
}

/// The purge half of both purge walks: the page groups purged, their
/// discard batch and the oldest stamp among the slabs kept.
///
/// The batch bound `purge_group` needs: the walk visits in increasing
/// order, and a purge takes every member of the empty set in its page
/// group, so each page group pushed starts at or past the end of the
/// last one (`end`) and the batch holds at most the bytes of the slabs
/// below `end`, which is inside the class region, below `2^57`.
struct Purger {
    spp: usize,
    n: usize,
    batch: Batch,
    oldest: u64,
    /// Ghost: the end of the last page group pushed (0 before the first).
    end: Ghost<usize>,
    /// Ghost: the last slab visited (-1 before the first).
    last: Ghost<int>,
}

impl Purger {
    /// Nothing purged yet over page groups of `spp` slabs.
    #[inline(always)]
    fn new(spp: usize) -> (p: Purger)
        ensures
            p.spp == spp,
            p.n == 0,
            p.batch.wf(),
            p.batch.total() == 0,
            p.oldest == 0,
            p.end@ == 0,
            p.last@ == -1,
    {
        Purger { spp, n: 0, batch: Batch::new(), oldest: 0, end: Ghost(0), last: Ghost(-1) }
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
    }

    /// Visit `slab` (stamped `dirty_at`): purge its page group when
    /// `purge` holds and the group is idle, else keep it and fold its
    /// stamp into `oldest`.
    #[inline(always)]
    fn visit(&mut self, cs: &ClassState, g: &mut Inner, slab: usize, dirty_at: u64, purge: bool)
        requires
            old(self).inv(cs, *old(g)),
            old(self).last@ < slab,
            walk_ok(cs, *old(g)),
            old(g).empty.map@.contains(slab as nat),
            dirty_at != 0,
        ensures
            final(self).inv(cs, *final(g)),
            final(self).last@ == slab,
            walk_ok(cs, *final(g)),
    {
        let spp = self.spp;
        let ghost first = slab & !sub(spp, 1);
        proof {
            assert(g.slab_ok(slab as nat));
            lemma_page_group(slab, self.end@, cs.spec_nslabs(), spp);
            assert(sub(spp, 1) == (spp - 1) as usize);
            assert(usize::MAX >> 12 == 0xf_ffff_ffff_ffffusize) by (bit_vector);
            assert(slab + spp <= usize::MAX);
        }
        if purge && (spp == 1 || cs.page_group_is_idle(&*g, slab, spp)) {
            let ghost g0 = *g;
            proof {
                if spp == 1 {
                    assert(first == slab);
                }
                use_type_invariant(cs);
                lemma_run_fits(cs.spec_table(), 0, self.end@);
                assert(self.end@ as int * slab_bytes(cs) <= 0x200_0000_0000_0000);
            }
            let k = cs.purge_group(g, slab, spp, &mut self.batch);
            proof {
                assert(self.end@ as int * slab_bytes(cs) <= first as int * slab_bytes(cs)) by (nonlinear_arith)
                    requires self.end@ <= first, slab_bytes(cs) >= 0;
                assert(first as int * slab_bytes(cs) + spp as int * slab_bytes(cs) == (first + spp) as int * slab_bytes(cs))
                    by (nonlinear_arith);
            }
            self.n += k;
            self.end = Ghost((first + spp) as usize);
            proof {
                assert forall|x: nat| #[trigger] g.empty.map@.contains(x) && x > slab implies x >= self.end@ by {
                    assert(g0.empty.map@.contains(x));
                }
            }
        } else {
            self.oldest = min_nonzero(self.oldest, dirty_at);
        }
        self.last = Ghost(slab as int);
    }
}

/// `purge_empty`'s visitor: purge every slab stamped at or before `cutoff`.
struct PurgeOlder {
    cutoff: u64,
    p: Purger,
}

impl EmptyVisitor for PurgeOlder {
    closed spec fn inv(&self, cs: &ClassState, g: Inner) -> bool {
        self.p.inv(cs, g)
    }

    closed spec fn last(&self) -> int {
        self.p.last@
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
    pub fn purge_empty(&self, cutoff: u64) -> (r: usize)
        ensures
            r <= self.spec_nslabs(),
    {
        broadcast use {super::axiom_class_lock, super::axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock();
        let mut v = PurgeOlder { cutoff, p: Purger::new(spp) };
        proof {
            assert(0usize & sub(spp, 1) == 0) by (bit_vector);
            assert(v.p.inv(self, g@));
        }
        self.walk_empty(&mut g, &mut v);
        v.p.batch.flush();
        self.gate.set_oldest_empty(v.p.oldest);
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
    /// word), and the batch holds at most the bytes of the class region
    /// below the group, which ends below `2^57`.
    // The loop carries a Verus invariant.
    #[allow(clippy::while_let_loop)]
    fn purge_group(&self, g: &mut Inner, slab: usize, spp: usize, batch: &mut Batch) -> (n: usize)
        requires
            old(g).inv(),
            old(g).table@ == self.spec_table(),
            old(g).nbits() == self.spec_nslabs(),
            spp >= 1,
            (slab & !((spp - 1) as usize)) + spp <= self.spec_nslabs(),
            forall|s: nat| (slab & !((spp - 1) as usize)) <= s < (slab & !((spp - 1) as usize)) + spp ==> #[trigger] old(g).owns(s),
            old(batch).wf(),
            old(batch).total() <= 0x200_0000_0000_0000,
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
            n <= spp,
    {
        proof {
            use_type_invariant(self);
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
    fn purge_curve(&self, now: u64, curve: &Curve, keep16: u32) -> usize {
        broadcast use {super::axiom_class_lock, super::axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let spp = self.table().slabs_per_page();
        let mut g = self.inner.lock();
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
        let mut v = PurgeCurve { now, curve, cut, keep, p: Purger::new(spp) };
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
        self.decay_with(now, decay_ns, crate::policy::KEEP_ALL)
    }

    /// [`decay`](ClassState::decay) with the cgroup policy's keep ratio
    /// (design 11): the walk keeps at most `keep16 / 65536` of its `Empty`
    /// slabs on top of the curve's allowance.
    pub fn decay_with(&self, now: u64, decay_ns: u64, keep16: u32) -> Option<u64> {
        self.gate.decay(now, decay_ns, |now, curve| {
            self.purge_curve(now, curve, keep16);
        })
    }
}

} // verus!
