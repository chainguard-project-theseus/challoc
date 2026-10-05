//! Claim completeness (`verus/THEOREMS.md`, "Slab claim never misses free
//! capacity"): a ghost model of one arena's state of one class, the part of
//! the slab protocol a refill's slab selection depends on.
//!
//! The class lock's half (each slab's state, the committed set and the
//! three slab sets) is what [`super::Inner`] holds; the lock-free half
//! (each slab's bits and `live` count, the reservations `take` made that
//! `finish` has not claimed, the claimed slots a refill has not handed
//! out, the released bits `dec_live` has not subtracted, and the `Reopen`
//! entries a release batch has not flushed) is what Verus does not see in
//! the code: the bits are atomics behind per-word invariants and `live` an
//! atomic Verus gives any value. [`Proto`] has one step per atomic update
//! or lock hold that touches them, each with what the code knows at that
//! point as its precondition, and its invariant ([`Proto::inv`]) holds in
//! every state the steps reach (`lemma_step_*`), however they interleave.
//!
//! The invariant's point is the closed slabs: an `Active` slab out of the
//! partial set with no `Reopen` entry pending has every slot claimed or
//! reserved, and while a reservation is outstanding all its claimed slots
//! are the reserving refill's, not yet handed out. A release of a slot
//! of a closed slab therefore finds its word full and records a `Reopen`
//! entry, and the entry's flush puts the slab back. At a quiescent state
//! ([`Proto::quiescent`]: nothing in flight) a closed slab is full
//! ([`Proto::theorem_quiescent`]), so a lock hold that finds both idle sets
//! empty and every partial slab's `live` at `nslots` leaves no free slot
//! in any committed slab ([`Proto::theorem_dry`]). With the code's half,
//! [`super::ClassState::exhausted`] (what `plan` and `claim_with` ensure of
//! a hold that reserved nothing), this is
//! [`super::ClassState::theorem_claim_complete`].

use super::ClassState;
#[cfg(verus_only)]
use super::Inner;
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::slab::State;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Map, Set, int, nat, verus};
#[cfg(verus_only)]
use vstd::set_lib::set_int_range;

verus! {

/// Reservations of refills `0..n` in slab `s`.
pub open spec fn rsum(r: spec_fn(nat, nat) -> int, s: nat, n: nat) -> int
    decreases n,
{
    if n == 0 {
        0
    } else {
        rsum(r, s, (n - 1) as nat) + r((n - 1) as nat, s)
    }
}

/// With every reservation non-negative, the sum is at least each of its
/// terms (and at least any two distinct ones together).
proof fn lemma_rsum_ge(r: spec_fn(nat, nat) -> int, s: nat, n: nat, c1: nat, c2: nat)
    requires
        forall|c: nat| #[trigger] r(c, s) >= 0,
    ensures
        rsum(r, s, n) >= 0,
        c1 < n ==> rsum(r, s, n) >= r(c1, s),
        c1 < n && c2 < n && c1 != c2 ==> rsum(r, s, n) >= r(c1, s) + r(c2, s),
    decreases n,
{
    if n > 0 {
        lemma_rsum_ge(r, s, (n - 1) as nat, c1, c2);
        lemma_rsum_ge(r, s, (n - 1) as nat, c2, c1);
        assert(r((n - 1) as nat, s) >= 0);
        assert(r(c1, s) >= 0);
        assert(r(c2, s) >= 0);
    }
}

/// Changing one refill's reservation in `s` by `d` changes the sum by `d`
/// when the refill is among `0..n`.
proof fn lemma_rsum_upd(r1: spec_fn(nat, nat) -> int, r2: spec_fn(nat, nat) -> int, s: nat, n: nat, c0: nat, d: int)
    requires
        forall|c: nat| c != c0 ==> #[trigger] r2(c, s) == r1(c, s),
        r2(c0, s) == r1(c0, s) + d,
    ensures
        rsum(r2, s, n) == rsum(r1, s, n) + (if c0 < n { d } else { 0 }),
    decreases n,
{
    if n > 0 {
        lemma_rsum_upd(r1, r2, s, (n - 1) as nat, c0, d);
        if (n - 1) as nat != c0 {
            assert(r2((n - 1) as nat, s) == r1((n - 1) as nat, s));
        }
    }
}

/// A sum over the same terms is the same sum.
proof fn lemma_rsum_same(r1: spec_fn(nat, nat) -> int, r2: spec_fn(nat, nat) -> int, s: nat, n: nat)
    requires
        forall|c: nat| #[trigger] r2(c, s) == r1(c, s),
    ensures
        rsum(r2, s, n) == rsum(r1, s, n),
{
    lemma_rsum_upd(r1, r2, s, n, 0, 0);
}

/// A set of slots of a slab with `n` slots and at least `n` members is
/// all of them.
proof fn lemma_full(b: Set<int>, n: int)
    requires
        n >= 0,
        b.subset_of(set_int_range(0, n)),
        b.len() >= n,
    ensures
        b == set_int_range(0, n),
{
    vstd::set_lib::lemma_int_range(0, n);
    assert forall|y: int| #[trigger] set_int_range(0, n).contains(y) implies b.contains(y) by {
        if !b.contains(y) {
            let r = set_int_range(0, n).remove(y);
            assert(b.subset_of(r));
            vstd::set_lib::lemma_len_subset(b, r);
        }
    }
    assert(b =~= set_int_range(0, n));
}

/// One arena's state of one class (see the module doc). Slabs are their
/// numbers; refills (a `claim`, from its first `plan` to its return) are
/// numbered `0..k` in the order they started.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct Proto {
    /// The class's slots per slab.
    pub nslots: int,
    /// The slabs the arena owns and their states (`Inner::states`).
    pub st: Map<nat, State>,
    /// `Inner::committed` and the three slab sets of `Inner`.
    pub committed: Set<nat>,
    pub partial: Set<nat>,
    pub empty: Set<nat>,
    pub purged: Set<nat>,
    /// Slot `x`'s bit of slab `s` is set: `x` is in `bits(s)`.
    pub bits: spec_fn(nat) -> Set<int>,
    /// The descriptor's `live` count.
    pub live: spec_fn(nat) -> int,
    /// Bits `release_word` cleared that `dec_live` has not subtracted.
    pub pend: spec_fn(nat) -> int,
    /// Entries naming slab `s` in the `Reopen` lists of release batches,
    /// not yet flushed.
    pub reopen: spec_fn(nat) -> int,
    /// Refills started.
    pub k: nat,
    /// `resv(c, s)`: slots refill `c`'s `take` reserved in slab `s` that
    /// its `finish` (or `claim_prefix`) has not claimed yet.
    pub resv: spec_fn(nat, nat) -> int,
    /// Slots `(slab, slot)` refill `c` claimed and has not handed out.
    pub flight: spec_fn(nat) -> Set<(nat, int)>,
    /// The refill whose `take` last closed slab `s` (reserved all of it).
    pub closer: spec_fn(nat) -> nat,
}

impl Proto {
    /// A slab's slots.
    pub open spec fn slots(self) -> Set<int> {
        set_int_range(0, self.nslots)
    }

    /// Slots reserved in slab `s` and not claimed yet.
    pub open spec fn rs(self, s: nat) -> int {
        rsum(self.resv, s, self.k)
    }

    /// Slab `s` is owned and `Active`.
    pub open spec fn active(self, s: nat) -> bool {
        self.st.contains_key(s) && self.st[s] == State::Active
    }

    /// Slab `s` is closed: `Active`, out of the partial set, and named by
    /// no pending `Reopen` entry.
    pub open spec fn closed(self, s: nat) -> bool {
        &&& self.active(s)
        &&& !self.partial.contains(s)
        &&& (self.reopen)(s) == 0
    }

    /// The slab's part of bitmap word `w` of slab `s` is full.
    pub open spec fn word_full(self, s: nat, w: int) -> bool {
        forall|y: int| 0 <= y < self.nslots && y / 64 == w ==> #[trigger] (self.bits)(s).contains(y)
    }

    /// The invariant at slab `s`.
    pub open spec fn slab_inv(self, s: nat) -> bool {
        &&& (self.bits)(s).subset_of(self.slots())
        &&& (self.pend)(s) >= 0
        &&& (self.reopen)(s) >= 0
        // `live` counts the set bits, the reservations not yet claimed and
        // the releases not yet subtracted; it never exceeds `nslots`.
        &&& (self.live)(s) == (self.bits)(s).len() + self.rs(s) + (self.pend)(s)
        &&& (self.live)(s) <= self.nslots
        // The lock's half (`Inner::slab_core`).
        &&& self.partial.contains(s) ==> self.active(s)
        &&& self.empty.contains(s) == (self.st.contains_key(s) && self.st[s] == State::Empty)
        &&& self.purged.contains(s) == (self.st.contains_key(s) && self.st[s] == State::Purged)
        &&& self.committed.contains(s) == (self.st.contains_key(s) && self.st[s] != State::Fresh)
        // Only an `Active` slab has anything out or in flight.
        &&& !self.active(s) ==> (self.live)(s) == 0
        // A closed slab has every slot claimed or reserved, and while a
        // reservation is outstanding it is all its closer's, whose
        // claimed slots are all of the slab's set bits.
        &&& self.closed(s) ==> self.nslots - (self.bits)(s).len() <= self.rs(s)
        &&& self.closed(s) && self.rs(s) > 0 ==> (self.closer)(s) < self.k
        &&& self.closed(s) && self.rs(s) > 0 ==> (self.resv)((self.closer)(s), s) == self.rs(s)
        &&& self.closed(s) && self.rs(s) > 0 ==> forall|x: int| #[trigger] (self.bits)(s).contains(x)
            ==> (self.flight)((self.closer)(s)).contains((s, x))
    }

    /// The model's invariant.
    pub open spec fn inv(self) -> bool {
        &&& self.nslots > 0
        &&& forall|c: nat, s: nat| #[trigger] (self.resv)(c, s) >= 0
        &&& forall|c: nat, s: nat| c >= self.k ==> #[trigger] (self.resv)(c, s) == 0
        &&& forall|c: nat| c >= self.k ==> #[trigger] (self.flight)(c) == Set::<(nat, int)>::empty()
        &&& forall|c: nat, p: (nat, int)| #[trigger] (self.flight)(c).contains(p) ==> (self.bits)(p.0).contains(p.1)
        &&& forall|s: nat| #[trigger] self.slab_inv(s)
    }

    /// Nothing in flight: no reservation unclaimed, no claimed slot not
    /// handed out, no release not subtracted, no `Reopen` entry unflushed.
    pub open spec fn quiescent(self) -> bool {
        &&& forall|c: nat, s: nat| #[trigger] (self.resv)(c, s) == 0
        &&& forall|c: nat| #[trigger] (self.flight)(c) == Set::<(nat, int)>::empty()
        &&& forall|s: nat| #[trigger] (self.pend)(s) == 0
        &&& forall|s: nat| #[trigger] (self.reopen)(s) == 0
    }

    /// An arena that owns nothing yet.
    pub open spec fn start(nslots: int) -> Proto {
        Proto {
            nslots,
            st: Map::empty(),
            committed: Set::empty(),
            partial: Set::empty(),
            empty: Set::empty(),
            purged: Set::empty(),
            bits: |s: nat| Set::<int>::empty(),
            live: |s: nat| 0int,
            pend: |s: nat| 0int,
            reopen: |s: nat| 0int,
            k: 0,
            resv: |c: nat, s: nat| 0int,
            flight: |c: nat| Set::<(nat, int)>::empty(),
            closer: |s: nat| 0nat,
        }
    }

    // The steps. Each is one atomic update or lock hold of the code, or a
    // finer part of one: `claim` and `release` are one bit of a multi-bit
    // `fetch_or` or `fetch_and`, and the zero pick's prefix claims, made
    // inside `take`'s lock hold, are `claim` steps after it. A finer step
    // only admits more interleavings. Its precondition is what the code
    // knows at that point. The code's steps are named by `protocol`'s
    // `Refill`, `Release` and `Purge` (`class/mod.rs`), as for `model::`.

    /// A refill starts (`claim_with` is entered).
    pub open spec fn begin(self) -> Proto {
        Proto { k: self.k + 1, ..self }
    }

    /// `next_fresh_slab`'s `take_group`, under the class lock: slab `s`, of a
    /// group no arena owned, joins the arena `Fresh`.
    pub open spec fn own_pre(self, s: nat) -> bool {
        !self.st.contains_key(s)
    }

    pub open spec fn own(self, s: nat) -> Proto {
        Proto { st: self.st.insert(s, State::Fresh), ..self }
    }

    /// `ClassState::take`, under the class lock, for refill `c`: reserve
    /// `n <= nslots - live` slots of a partial slab, of one `next_slab`
    /// took out of an idle set, or of a `Fresh` one `next_fresh_slab`
    /// committed; make it `Active`, and take it out of the partial set
    /// exactly when it had `live == 0` and every slot is reserved.
    ///
    /// Protocol step: `Refill::PlanLocked`.
    pub open spec fn take_pre(self, c: nat, s: nat, n: int) -> bool {
        &&& c < self.k
        &&& self.st.contains_key(s)
        &&& self.partial.contains(s) || self.empty.contains(s) || self.purged.contains(s) || self.st[s] == State::Fresh
        &&& 0 <= n <= self.nslots - (self.live)(s)
    }

    /// `take` closes the slab.
    pub open spec fn closes(self, s: nat, n: int) -> bool {
        (self.live)(s) == 0 && n == self.nslots - (self.live)(s)
    }

    pub open spec fn take(self, c: nat, s: nat, n: int) -> Proto {
        Proto {
            st: self.st.insert(s, State::Active),
            committed: self.committed.insert(s),
            partial: if self.closes(s, n) { self.partial.remove(s) } else { self.partial.insert(s) },
            empty: self.empty.remove(s),
            purged: self.purged.remove(s),
            live: |t: nat| if t == s { (self.live)(s) + n } else { (self.live)(t) },
            resv: |d: nat, t: nat| if d == c && t == s { (self.resv)(c, s) + n } else { (self.resv)(d, t) },
            closer: |t: nat| if t == s && self.closes(s, n) { c } else { (self.closer)(t) },
            ..self
        }
    }

    /// `plan`'s clear, under the class lock: a partial `Active` slab whose
    /// `live` load read `nslots` or more and whose words `is_full` read full
    /// leaves the partial set. Neither load is a snapshot (a release may
    /// clear a bit and lower `live` meanwhile), so the step asks only what
    /// holds at the clear: each word is full or was released since its
    /// load, and the first such release found it full and recorded a
    /// `Reopen` entry, which waits for this lock. So the slab is full or
    /// has an entry pending.
    ///
    /// Protocol step: `Refill::PlanLocked`.
    pub open spec fn clear_pre(self, s: nat) -> bool {
        &&& self.partial.contains(s)
        &&& (self.bits)(s) == self.slots() || (self.reopen)(s) > 0
    }

    pub open spec fn clear(self, s: nat) -> Proto {
        Proto { partial: self.partial.remove(s), ..self }
    }

    /// `finish` (or `claim_prefix`), refill `c`: a `fetch_or` wins the free
    /// bit of slot `x` against a reservation `c` holds in slab `s`.
    ///
    /// Protocol step: `Refill::Finish` (a zero pick's prefix: `Refill::PlanLocked`).
    pub open spec fn claim_pre(self, c: nat, s: nat, x: int) -> bool {
        &&& c < self.k
        &&& (self.resv)(c, s) > 0
        &&& 0 <= x < self.nslots
        &&& !(self.bits)(s).contains(x)
    }

    pub open spec fn claim(self, c: nat, s: nat, x: int) -> Proto {
        Proto {
            bits: |t: nat| if t == s { (self.bits)(s).insert(x) } else { (self.bits)(t) },
            resv: |d: nat, t: nat| if d == c && t == s { (self.resv)(c, s) - 1 } else { (self.resv)(d, t) },
            flight: |d: nat| if d == c { (self.flight)(c).insert((s, x)) } else { (self.flight)(d) },
            ..self
        }
    }

    /// `claim_with` returns, refill `c`: every reservation met (`finish`
    /// claims all it reserved), its slots handed out.
    ///
    /// Protocol step: past `Refill::Finish`, the claim's return.
    pub open spec fn hand_out_pre(self, c: nat) -> bool {
        &&& c < self.k
        &&& forall|s: nat| #[trigger] (self.resv)(c, s) == 0
    }

    pub open spec fn hand_out(self, c: nat) -> Proto {
        Proto { flight: |d: nat| if d == c { Set::<(nat, int)>::empty() } else { (self.flight)(d) }, ..self }
    }

    /// `release_word` (`AtomicBitmap::release`'s `fetch_and`): a slot `x`
    /// of an owned slab, out and in no refill's flight, is given back. If
    /// the slab's part of its word was full, the batch's `Reopen` list
    /// gets an entry for the slab, or (`dedup`) already ends with one.
    ///
    /// Protocol step: `Release::FetchAnd`.
    pub open spec fn release_pre(self, s: nat, x: int, dedup: bool) -> bool {
        &&& self.st.contains_key(s)
        &&& (self.bits)(s).contains(x)
        &&& forall|c: nat| c < self.k ==> !#[trigger] (self.flight)(c).contains((s, x))
        &&& dedup ==> (self.reopen)(s) > 0
    }

    pub open spec fn release(self, s: nat, x: int, dedup: bool) -> Proto {
        Proto {
            bits: |t: nat| if t == s { (self.bits)(s).remove(x) } else { (self.bits)(t) },
            pend: |t: nat| if t == s { (self.pend)(s) + 1 } else { (self.pend)(t) },
            reopen: |t: nat| if t == s && self.word_full(s, x / 64) && !dedup { (self.reopen)(s) + 1 } else { (self.reopen)(t) },
            ..self
        }
    }

    /// `dec_live`'s `sub_live(n)` for `n` released bits of slab `s`.
    ///
    /// Protocol step: `Release::SubLive`.
    pub open spec fn dec_pre(self, s: nat, n: int) -> bool {
        0 < n <= (self.pend)(s)
    }

    pub open spec fn dec(self, s: nat, n: int) -> Proto {
        Proto {
            live: |t: nat| if t == s { (self.live)(s) - n } else { (self.live)(t) },
            pend: |t: nat| if t == s { (self.pend)(s) - n } else { (self.pend)(t) },
            ..self
        }
    }

    /// `on_live_zero`, under the class lock: an `Active` slab whose `live`
    /// read 0 goes `Empty`, out of the partial set into the empty set.
    ///
    /// Protocol step: `Release::OnLiveZero`.
    pub open spec fn live_zero_pre(self, s: nat) -> bool {
        &&& self.active(s)
        &&& (self.live)(s) == 0
    }

    pub open spec fn live_zero(self, s: nat) -> Proto {
        Proto {
            st: self.st.insert(s, State::Empty),
            partial: self.partial.remove(s),
            empty: self.empty.insert(s),
            ..self
        }
    }

    /// `reopen`, under the class lock: one `Reopen` entry for slab `s` is
    /// flushed, which puts the slab back in the partial set if it is
    /// `Active`.
    ///
    /// Protocol step: `Release::Reopen`.
    pub open spec fn reopen_pre(self, s: nat) -> bool {
        (self.reopen)(s) > 0
    }

    pub open spec fn reopen_step(self, s: nat) -> Proto {
        Proto {
            reopen: |t: nat| if t == s { (self.reopen)(s) - 1 } else { (self.reopen)(t) },
            partial: if self.active(s) { self.partial.insert(s) } else { self.partial },
            ..self
        }
    }

    /// `purge_group`, under the class lock: an `Empty` slab is discarded and
    /// moves to the purged set.
    ///
    /// Protocol step: `Purge::PurgeLocked`.
    pub open spec fn purge_pre(self, s: nat) -> bool {
        self.empty.contains(s)
    }

    pub open spec fn purge(self, s: nat) -> Proto {
        Proto {
            st: self.st.insert(s, State::Purged),
            empty: self.empty.remove(s),
            purged: self.purged.insert(s),
            ..self
        }
    }

    /// The facts the steps' proofs share: the sums are at least their
    /// terms, a slab's bits are finite, and a slab with nothing out has
    /// none set, nothing reserved and nothing pending.
    proof fn lemma_facts(self, s: nat)
        requires
            self.inv(),
        ensures
            self.slab_inv(s),
            self.rs(s) >= 0,
            forall|c: nat| c < self.k ==> self.rs(s) >= #[trigger] (self.resv)(c, s),
            (self.bits)(s).len() <= self.nslots,
            (self.live)(s) == 0 ==> (self.bits)(s) == Set::<int>::empty() && self.rs(s) == 0 && (self.pend)(s) == 0,
            self.rs(s) == 0 ==> forall|c: nat| #[trigger] (self.resv)(c, s) == 0,
    {
        assert(self.slab_inv(s));
        lemma_rsum_ge(self.resv, s, self.k, 0, 0);
        assert forall|c: nat| c < self.k implies self.rs(s) >= #[trigger] (self.resv)(c, s) by {
            lemma_rsum_ge(self.resv, s, self.k, c, c);
        }
        vstd::set_lib::lemma_int_range(0, self.nslots);
        vstd::set_lib::lemma_len_subset((self.bits)(s), self.slots());
        if (self.live)(s) == 0 {
            assert((self.bits)(s).len() == 0);
            assert((self.bits)(s) =~= Set::<int>::empty());
        }
        if self.rs(s) == 0 {
            assert forall|c: nat| #[trigger] (self.resv)(c, s) == 0 by {
                if c < self.k {
                    lemma_rsum_ge(self.resv, s, self.k, c, c);
                }
            }
        }
    }

    pub proof fn lemma_start(nslots: int)
        requires
            nslots > 0,
        ensures
            Proto::start(nslots).inv(),
    {
        let p = Proto::start(nslots);
        assert forall|s: nat| #[trigger] p.slab_inv(s) by {
            assert(p.rs(s) == 0);
            assert((p.bits)(s).len() == 0);
        }
    }

    pub proof fn lemma_step_begin(self)
        requires
            self.inv(),
        ensures
            self.begin().inv(),
    {
        let n = self.begin();
        assert forall|s: nat| #[trigger] n.slab_inv(s) by {
            self.lemma_facts(s);
            assert((self.resv)(self.k, s) == 0);
            assert(n.rs(s) == self.rs(s));
        }
    }

    pub proof fn lemma_step_own(self, s: nat)
        requires
            self.inv(),
            self.own_pre(s),
        ensures
            self.own(s).inv(),
    {
        let n = self.own(s);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
        }
    }

    pub proof fn lemma_step_take(self, c: nat, s: nat, m: int)
        requires
            self.inv(),
            self.take_pre(c, s, m),
        ensures
            self.take(c, s, m).inv(),
    {
        let n = self.take(c, s, m);
        self.lemma_facts(s);
        assert forall|d: nat, t: nat| #[trigger] (n.resv)(d, t) >= 0 by {
            assert((self.resv)(d, t) >= 0);
        }
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            if t == s {
                lemma_rsum_upd(self.resv, n.resv, s, self.k, c, m);
                assert(n.rs(s) == self.rs(s) + m);
                if self.closes(s, m) {
                    assert((n.resv)(c, s) == m);
                    assert forall|x: int| #[trigger] (n.bits)(s).contains(x)
                        implies (n.flight)((n.closer)(s)).contains((s, x)) by {
                        assert((self.bits)(s).contains(x));
                    }
                }
            } else {
                lemma_rsum_same(self.resv, n.resv, t, self.k);
                assert(n.rs(t) == self.rs(t));
            }
        }
    }

    pub proof fn lemma_step_clear(self, s: nat)
        requires
            self.inv(),
            self.clear_pre(s),
        ensures
            self.clear(s).inv(),
    {
        let n = self.clear(s);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
            if t == s && (self.reopen)(s) == 0 {
                vstd::set_lib::lemma_int_range(0, self.nslots);
                assert((self.bits)(s).len() == self.nslots);
            }
        }
    }

    pub proof fn lemma_step_claim(self, c: nat, s: nat, x: int)
        requires
            self.inv(),
            self.claim_pre(c, s, x),
        ensures
            self.claim(c, s, x).inv(),
    {
        let n = self.claim(c, s, x);
        self.lemma_facts(s);
        assert forall|d: nat, t: nat| #[trigger] (n.resv)(d, t) >= 0 by {
            assert((self.resv)(d, t) >= 0);
        }
        assert forall|d: nat, p: (nat, int)| #[trigger] (n.flight)(d).contains(p) implies (n.bits)(p.0).contains(p.1) by {
            if !(d == c && p == (s, x)) {
                assert((self.flight)(d).contains(p));
            }
        }
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            if t == s {
                lemma_rsum_upd(self.resv, n.resv, s, self.k, c, -1);
                assert(n.rs(s) == self.rs(s) - 1);
                assert((n.bits)(s).len() == (self.bits)(s).len() + 1);
                if self.closed(s) {
                    // The reservation `c` claims against is the closer's.
                    let cc = (self.closer)(s);
                    if cc != c {
                        lemma_rsum_ge(self.resv, s, self.k, cc, c);
                    }
                    assert(cc == c);
                    assert forall|y: int| #[trigger] (n.bits)(s).contains(y)
                        implies (n.flight)((n.closer)(s)).contains((s, y)) by {
                        if y != x {
                            assert((self.bits)(s).contains(y));
                        }
                    }
                }
            } else {
                lemma_rsum_same(self.resv, n.resv, t, self.k);
                assert(n.rs(t) == self.rs(t));
                if self.closed(t) && self.rs(t) > 0 {
                    assert forall|y: int| #[trigger] (n.bits)(t).contains(y)
                        implies (n.flight)((n.closer)(t)).contains((t, y)) by {
                        assert((self.bits)(t).contains(y));
                    }
                }
            }
        }
    }

    pub proof fn lemma_step_hand_out(self, c: nat)
        requires
            self.inv(),
            self.hand_out_pre(c),
        ensures
            self.hand_out(c).inv(),
    {
        let n = self.hand_out(c);
        assert forall|d: nat, p: (nat, int)| #[trigger] (n.flight)(d).contains(p) implies (n.bits)(p.0).contains(p.1) by {
            assert((self.flight)(d).contains(p));
        }
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
            if self.closed(t) && self.rs(t) > 0 {
                // The closer still reserves, so it is not `c`.
                assert((self.resv)(c, t) == 0);
                assert((self.closer)(t) != c);
                assert forall|y: int| #[trigger] (n.bits)(t).contains(y)
                    implies (n.flight)((n.closer)(t)).contains((t, y)) by {
                    assert((self.bits)(t).contains(y));
                }
            }
        }
    }

    pub proof fn lemma_step_release(self, s: nat, x: int, dedup: bool)
        requires
            self.inv(),
            self.release_pre(s, x, dedup),
        ensures
            self.release(s, x, dedup).inv(),
    {
        let n = self.release(s, x, dedup);
        self.lemma_facts(s);
        assert forall|d: nat, p: (nat, int)| #[trigger] (n.flight)(d).contains(p) implies (n.bits)(p.0).contains(p.1) by {
            assert((self.bits)(p.0).contains(p.1));
            if p == (s, x) {
                if d < self.k {
                    assert(!(self.flight)(d).contains((s, x)));
                } else {
                    assert((self.flight)(d) == Set::<(nat, int)>::empty());
                }
            }
        }
        if self.closed(s) {
            // A closed slab with a reservation out has only its closer's
            // slots set, and `x` is in no flight: so nothing is reserved,
            // the slab is full, and so is `x`'s word.
            if self.rs(s) > 0 {
                assert((self.flight)((self.closer)(s)).contains((s, x)));
                assert(false);
            }
            lemma_full((self.bits)(s), self.nslots);
            assert(self.word_full(s, x / 64));
            assert((n.reopen)(s) == 1);
        }
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
            if t == s {
                assert((n.bits)(s).len() == (self.bits)(s).len() - 1);
            } else if self.closed(t) && self.rs(t) > 0 {
                assert forall|y: int| #[trigger] (n.bits)(t).contains(y)
                    implies (n.flight)((n.closer)(t)).contains((t, y)) by {
                    assert((self.bits)(t).contains(y));
                }
            }
        }
    }

    pub proof fn lemma_step_dec(self, s: nat, m: int)
        requires
            self.inv(),
            self.dec_pre(s, m),
        ensures
            self.dec(s, m).inv(),
    {
        let n = self.dec(s, m);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
        }
    }

    pub proof fn lemma_step_live_zero(self, s: nat)
        requires
            self.inv(),
            self.live_zero_pre(s),
        ensures
            self.live_zero(s).inv(),
    {
        let n = self.live_zero(s);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
        }
    }

    pub proof fn lemma_step_reopen(self, s: nat)
        requires
            self.inv(),
            self.reopen_pre(s),
        ensures
            self.reopen_step(s).inv(),
    {
        let n = self.reopen_step(s);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
        }
    }

    pub proof fn lemma_step_purge(self, s: nat)
        requires
            self.inv(),
            self.purge_pre(s),
        ensures
            self.purge(s).inv(),
    {
        let n = self.purge(s);
        assert forall|t: nat| #[trigger] n.slab_inv(t) by {
            self.lemma_facts(t);
            assert(n.rs(t) == self.rs(t));
        }
    }

    /// At a quiescent state `live` is the set bits, and an `Active` slab
    /// out of the partial set is full.
    pub proof fn theorem_quiescent(self, s: nat)
        requires
            self.inv(),
            self.quiescent(),
        ensures
            (self.live)(s) == (self.bits)(s).len(),
            self.active(s) && !self.partial.contains(s) ==> (self.bits)(s) == self.slots() && (self.live)(s) == self.nslots,
    {
        self.lemma_facts(s);
        assert forall|c: nat| #[trigger] (self.resv)(c, s) == 0 by {}
        lemma_rsum_same(|c: nat, t: nat| 0int, self.resv, s, self.k);
        lemma_rsum_zero(s, self.k);
        assert((self.pend)(s) == 0);
        assert((self.reopen)(s) == 0);
        if self.active(s) && !self.partial.contains(s) {
            lemma_full((self.bits)(s), self.nslots);
            vstd::set_lib::lemma_int_range(0, self.nslots);
        }
    }

    /// A lock hold at a quiescent state that finds both idle sets empty
    /// and every partial slab's `live` at `nslots` or more leaves no free
    /// slot in any committed slab: each is `Active`, with every bit set
    /// and `live == nslots`.
    pub proof fn theorem_dry(self, s: nat)
        requires
            self.inv(),
            self.quiescent(),
            self.empty == Set::<nat>::empty(),
            self.purged == Set::<nat>::empty(),
            forall|t: nat| #[trigger] self.partial.contains(t) ==> (self.live)(t) >= self.nslots,
            self.committed.contains(s),
        ensures
            self.active(s),
            (self.bits)(s) == self.slots(),
            (self.live)(s) == self.nslots,
    {
        self.lemma_facts(s);
        self.theorem_quiescent(s);
        assert(!self.empty.contains(s));
        assert(!self.purged.contains(s));
        if self.partial.contains(s) {
            lemma_full((self.bits)(s), self.nslots);
        }
    }
}

/// The steps' preconditions are not vacuous, and `theorem_dry`'s premises
/// meet with a committed slab in a state the steps reach: a one-slot slab
/// is owned, taken (closed) by a refill, claimed, handed out and
/// released, which records a `Reopen` entry; the entry's flush puts the
/// slab back in the partial set, `dec_live` makes `live` 0 and
/// `on_live_zero` makes the slab `Empty`, which a purge may take.
proof fn lemma_example() {
    let p0 = Proto::start(1);
    Proto::lemma_start(1);
    assert(p0.own_pre(0));
    let p1 = p0.own(0).begin();
    assert(p1.rs(0) == 0) by {
        lemma_rsum_zero(0, 1);
        lemma_rsum_same(|c: nat, t: nat| 0int, p1.resv, 0, 1);
    }
    assert(p1.take_pre(0, 0, 1));
    let p2 = p1.take(0, 0, 1);
    assert(!p2.partial.contains(0));
    assert(p2.claim_pre(0, 0, 0));
    let p3 = p2.claim(0, 0, 0);
    assert(p3.hand_out_pre(0));
    let p4 = p3.hand_out(0);
    // The state reached so far keeps the invariant, is quiescent, and
    // meets `theorem_dry`'s premises with a committed slab.
    p0.lemma_step_own(0);
    p0.own(0).lemma_step_begin();
    p1.lemma_step_take(0, 0, 1);
    p2.lemma_step_claim(0, 0, 0);
    p3.lemma_step_hand_out(0);
    assert(p4.quiescent());
    assert(p4.committed.contains(0));
    assert(p4.empty =~= Set::<nat>::empty());
    assert(p4.purged =~= Set::<nat>::empty());
    assert(p4.partial =~= Set::<nat>::empty());
    p4.theorem_dry(0);
    assert(p4.release_pre(0, 0, false));
    assert(p4.word_full(0, 0));
    let p5 = p4.release(0, 0, false);
    assert((p5.reopen)(0) == 1);
    assert(p5.reopen_pre(0));
    let p6 = p5.reopen_step(0);
    assert(p6.partial.contains(0));
    assert(p6.dec_pre(0, 1));
    let p7 = p6.dec(0, 1);
    assert(p7.live_zero_pre(0));
    let p8 = p7.live_zero(0);
    assert(p8.empty.contains(0) && !p8.partial.contains(0));
    assert(p8.purge_pre(0));
}

/// `plan`'s clear is reachable: a two-slot slab reserved and claimed one
/// slot at a time stays partial, and once full it may leave the set.
proof fn lemma_example_clear() {
    let p0 = Proto::start(2);
    let p1 = p0.own(0).begin();
    assert(p1.rs(0) == 0) by {
        lemma_rsum_zero(0, 1);
        lemma_rsum_same(|c: nat, t: nat| 0int, p1.resv, 0, 1);
    }
    assert(p1.take_pre(0, 0, 1));
    let p2 = p1.take(0, 0, 1);
    assert(p2.claim_pre(0, 0, 0));
    let p3 = p2.claim(0, 0, 0).hand_out(0);
    assert(p3.rs(0) == 0) by {
        lemma_rsum_zero(0, 1);
        lemma_rsum_same(|c: nat, t: nat| 0int, p3.resv, 0, 1);
    }
    assert(p3.partial.contains(0));
    assert(p3.take_pre(0, 0, 1));
    let p4 = p3.take(0, 0, 1);
    assert(p4.claim_pre(0, 0, 1));
    let p5 = p4.claim(0, 0, 1).hand_out(0);
    assert((p5.bits)(0) =~= p5.slots());
    assert(p5.clear_pre(0));
}

/// The all-zero sum is zero.
proof fn lemma_rsum_zero(s: nat, n: nat)
    ensures
        rsum(|c: nat, t: nat| 0int, s, n) == 0,
    decreases n,
{
    if n > 0 {
        lemma_rsum_zero(s, (n - 1) as nat);
    }
}

impl ClassState {
    /// Claim completeness. A refill's lock hold that reserved nothing
    /// ([`ClassState::exhausted`]: what `plan` ensures of `ex` when it
    /// returns no picks for a non-zero `want`, and so `claim_with` when it
    /// returns no slots for a non-empty `out`), at a quiescent state of the
    /// arena that the model `b` describes, leaves no free slot in any
    /// committed slab of the arena: each is owned, `Active`, not a guard,
    /// with every slot's bit set and `live == nslots` (nothing
    /// unreserved). The premises name the link: `b`'s lock half is the
    /// lock value `g`, and each `live` load of the hold read `b`'s count.
    pub(super) proof fn theorem_claim_complete(&self, b: Proto, g: Inner, seen: Map<nat, usize>, s: nat)
        requires
            self.exhausted(g, seen),
            b.inv(),
            b.quiescent(),
            b.nslots == class_table()[self.spec_class() as int].nslots as int,
            b.st == g.states@,
            b.committed == g.committed@,
            b.partial == g.partial.map@,
            b.empty == g.empty.map@,
            b.purged == g.purged.map@,
            forall|x: nat| #[trigger] seen.contains_key(x) ==> seen[x] as int == (b.live)(x),
            g.committed@.contains(s),
        ensures
            s < self.spec_nslabs(),
            g.owns(s),
            g.st(s) == State::Active,
            !self.spec_table().spec_is_guard(s as usize),
            forall|x: int| 0 <= x < b.nslots ==> #[trigger] (b.bits)(s).contains(x),
            (b.live)(s) == b.nslots,
    {
        self.lemma_exhausted(g, seen, s);
        assert forall|t: nat| #[trigger] b.partial.contains(t) implies (b.live)(t) >= b.nslots by {
            assert(g.partial.map@.contains(t));
            assert(seen.contains_key(t));
        }
        b.theorem_dry(s);
        assert forall|x: int| 0 <= x < b.nslots implies #[trigger] (b.bits)(s).contains(x) by {
            assert(b.slots().contains(x));
        }
    }
}

} // verus!
