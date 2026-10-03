//! The accounting ledger of exact stats (`verus/THEOREMS.md`, "Exact
//! stats"): a ghost model of what the counters [`super::collect_arena`]
//! reads count, and of where the blocks they count are.
//!
//! The counters are updated by the slow paths, apart from the token moves
//! they count: `take` adds a refill's reservation to `claimed` under the
//! class lock before `finish` claims the slots from their bitmap words,
//! `release_owned` adds a batch to `released` after `release_word` gave
//! the slots back, and a huge run is added to its arena's `large` word
//! after `Huge::take` made it live and subtracted after `Huge::free_run`
//! retired it. [`Books`] has one step per such atomic update, and one per
//! move of a slot between flight, a thread cache and a live block. Its
//! invariant ([`Books::inv`]) holds in every state the steps reach
//! (`lemma_step_*`), however they interleave: a counter is its
//! reservations' and releases' sum, ahead of or behind the slots out of
//! the bitmaps by exactly the updates in progress. At a quiescent state
//! ([`Books::quiescent`]: no update in progress) the counters are the true
//! counts: `claimed - released` summed over the arenas is the number of
//! cached and live slots of the class, and each arena's `large` word is its
//! live runs and their pages ([`Books::theorem_quiescent`]).
//!
//! Each step's precondition is what the code's tokens give at that point
//! (the slot's token came out of its bitmap word, so no holder has it; a
//! freed run's token is the descriptor's extent); `verus/THEOREMS.md`
//! lists the code behind each step.

#[cfg(verus_only)]
use super::*;
#[cfg(verus_only)]
use crate::heap::live::{BlockKey, Live, key_len, keys_apart, lemma_all_apart};
use vstd::prelude::{Set, int, verus};
#[cfg(verus_only)]
use vstd::set_lib::set_int_range;

verus! {

/// The pages of extent `e`, `(start, pages)`.
pub open spec fn ext_pages(e: (int, int)) -> Set<int> {
    set_int_range(e.0, e.0 + e.1)
}

/// The ledger. Arenas `0..k` exist; the maps are total, and zero or empty
/// for arenas not built yet.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct Books {
    pub k: int,
    /// `claimed - released` of arena `a`'s state of class `c`, unbounded
    /// (the two `u64` counters hold it modulo `2^64`).
    pub net: spec_fn(int, int) -> int,
    /// Slots `take` reserved (in `claimed`) that `finish` has not claimed
    /// from their bitmap yet.
    pub resv: spec_fn(int, int) -> int,
    /// Slots `release_owned` gave back to their bitmap and has not added
    /// to `released` yet.
    pub unc: spec_fn(int, int) -> int,
    /// The slots of class `c` out of their bitmap that arena `a` claimed
    /// (from a slab it owns) and has not released.
    pub out: spec_fn(int, int) -> Set<int>,
    /// Out slots of class `c` in flight: claimed and not yet cached or
    /// handed out, or taken from a cache or a free for release.
    pub flight: spec_fn(int) -> Set<int>,
    /// Out slots of class `c` in a thread cache.
    pub cached: spec_fn(int) -> Set<int>,
    /// Out slots of class `c` in a live block.
    pub live: spec_fn(int) -> Set<int>,
    /// Arena `a`'s `large` word: its runs and its pages.
    pub wruns: spec_fn(int) -> int,
    pub wpages: spec_fn(int) -> int,
    /// The live huge runs whose descriptor names arena `a`, as extents
    /// `(start, pages)` (the guard included), and their pages.
    pub runs: spec_fn(int) -> Set<(int, int)>,
    pub pages: spec_fn(int) -> Set<int>,
    /// Runs (and pages) of arena `a` made live and not yet added to its
    /// word.
    pub ta_runs: spec_fn(int) -> int,
    pub ta_pages: spec_fn(int) -> int,
    /// Runs (and pages) of arena `a` retired and not yet subtracted from
    /// its word.
    pub tf_runs: spec_fn(int) -> int,
    pub tf_pages: spec_fn(int) -> int,
}

impl Books {
    /// The out slots of class `c` of arenas `0..a`.
    pub open spec fn out_upto(self, c: int, a: int) -> Set<int>
        decreases a,
    {
        if a <= 0 {
            Set::empty()
        } else {
            self.out_upto(c, a - 1).union((self.out)(a - 1, c))
        }
    }

    /// `claimed - released` of class `c`, summed over arenas `0..a`.
    pub open spec fn net_upto(self, c: int, a: int) -> int
        decreases a,
    {
        if a <= 0 {
            0
        } else {
            self.net_upto(c, a - 1) + (self.net)(a - 1, c)
        }
    }

    /// The live runs of arenas `0..a`.
    pub open spec fn runs_upto(self, a: int) -> Set<(int, int)>
        decreases a,
    {
        if a <= 0 {
            Set::empty()
        } else {
            self.runs_upto(a - 1).union((self.runs)(a - 1))
        }
    }

    /// The pages of the live runs of arenas `0..a`.
    pub open spec fn pages_upto(self, a: int) -> Set<int>
        decreases a,
    {
        if a <= 0 {
            Set::empty()
        } else {
            self.pages_upto(a - 1).union((self.pages)(a - 1))
        }
    }

    /// The `large` words of arenas `0..a`, summed: runs.
    pub open spec fn wruns_upto(self, a: int) -> int
        decreases a,
    {
        if a <= 0 {
            0
        } else {
            self.wruns_upto(a - 1) + (self.wruns)(a - 1)
        }
    }

    /// The `large` words of arenas `0..a`, summed: pages.
    pub open spec fn wpages_upto(self, a: int) -> int
        decreases a,
    {
        if a <= 0 {
            0
        } else {
            self.wpages_upto(a - 1) + (self.wpages)(a - 1)
        }
    }

    /// Arena `a`'s counters of class `c` are its out slots plus the slots
    /// in both counters' updates still in progress: reserved and not yet
    /// claimed, released and not yet counted.
    pub open spec fn slots_ok(self, a: int, c: int) -> bool {
        &&& (self.resv)(a, c) >= 0
        &&& (self.unc)(a, c) >= 0
        &&& (self.net)(a, c) == (self.out)(a, c).len() + (self.resv)(a, c) + (self.unc)(a, c)
    }

    /// No slot is out through two arenas.
    pub open spec fn outs_apart(self) -> bool {
        forall|a: int, b: int, c: int|
            0 <= a < self.k && 0 <= b < self.k && a != b && 0 <= c < NCLASSES ==> #[trigger] (self.out)(a, c).disjoint(
                #[trigger] (self.out)(b, c),
            )
    }

    /// The out slots of class `c` are exactly those in flight, cached and
    /// live, each in one place.
    pub open spec fn holders_ok(self, c: int) -> bool {
        &&& (self.flight)(c).disjoint((self.cached)(c))
        &&& (self.flight)(c).disjoint((self.live)(c))
        &&& (self.cached)(c).disjoint((self.live)(c))
        &&& self.out_upto(c, self.k) == (self.flight)(c).union((self.cached)(c)).union((self.live)(c))
    }

    /// Arena `a`'s `large` word is its live runs and their pages, less the
    /// runs made live and not yet added, plus those retired and not yet
    /// subtracted.
    pub open spec fn huge_ok(self, a: int) -> bool {
        &&& forall|p: int|
            #[trigger] (self.pages)(a).contains(p) <==> exists|e: (int, int)|
                #[trigger] (self.runs)(a).contains(e) && ext_pages(e).contains(p)
        &&& forall|e: (int, int)| #[trigger] (self.runs)(a).contains(e) ==> e.0 >= 0 && e.1 > GUARD_PAGES
        &&& (self.ta_runs)(a) >= 0
        &&& (self.ta_pages)(a) >= 0
        &&& (self.tf_runs)(a) >= 0
        &&& (self.tf_pages)(a) >= 0
        &&& (self.wruns)(a) == (self.runs)(a).len() - (self.ta_runs)(a) + (self.tf_runs)(a)
        &&& (self.wpages)(a) == (self.pages)(a).len() - (self.ta_pages)(a) + (self.tf_pages)(a)
    }

    /// Two live runs share no page.
    pub open spec fn runs_apart(self) -> bool {
        forall|a: int, b: int, e1: (int, int), e2: (int, int)|
            0 <= a < self.k && 0 <= b < self.k && #[trigger] (self.runs)(a).contains(e1) && #[trigger] (
            self.runs)(b).contains(e2) && (a != b || e1 != e2) ==> ext_pages(e1).disjoint(ext_pages(e2))
    }

    /// The ledger's invariant.
    pub open spec fn inv(self) -> bool {
        &&& self.k >= 0
        &&& forall|a: int, c: int| 0 <= a < self.k && 0 <= c < NCLASSES ==> #[trigger] self.slots_ok(a, c)
        &&& self.outs_apart()
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] self.holders_ok(c)
        &&& forall|a: int| 0 <= a < self.k ==> #[trigger] self.huge_ok(a)
        &&& self.runs_apart()
    }

    /// No counter update is in progress: no reservation is unclaimed, no
    /// release uncounted, no slot in flight, no run's word update pending.
    pub open spec fn quiescent(self) -> bool {
        &&& forall|a: int, c: int| 0 <= a < self.k && 0 <= c < NCLASSES ==> #[trigger] (self.resv)(a, c) == 0 && (
        self.unc)(a, c) == 0
        &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] (self.flight)(c) == Set::<int>::empty()
        &&& forall|a: int| 0 <= a < self.k ==> #[trigger] (self.ta_runs)(a) == 0 && (self.ta_pages)(a) == 0 && (
        self.tf_runs)(a) == 0 && (self.tf_pages)(a) == 0
    }

    /// The process's start: one arena, nothing claimed, nothing live.
    pub open spec fn start() -> Books {
        Books {
            k: 1,
            net: |a: int, c: int| 0int,
            resv: |a: int, c: int| 0int,
            unc: |a: int, c: int| 0int,
            out: |a: int, c: int| Set::<int>::empty(),
            flight: |c: int| Set::<int>::empty(),
            cached: |c: int| Set::<int>::empty(),
            live: |c: int| Set::<int>::empty(),
            wruns: |a: int| 0int,
            wpages: |a: int| 0int,
            runs: |a: int| Set::<(int, int)>::empty(),
            pages: |a: int| Set::<int>::empty(),
            ta_runs: |a: int| 0int,
            ta_pages: |a: int| 0int,
            tf_runs: |a: int| 0int,
            tf_pages: |a: int| 0int,
        }
    }

    // The steps. Each is one atomic update of the code; its precondition
    // is what the code knows at that point.

    /// `Arenas::create` built arena `k`, whose counters start at zero.
    pub open spec fn new_arena(self) -> Books {
        Books { k: self.k + 1, ..self }
    }

    /// Arena `a`'s maps are zero and empty.
    pub open spec fn fresh(self, a: int) -> bool {
        &&& forall|c: int| #[trigger] (self.net)(a, c) == 0 && (self.resv)(a, c) == 0 && (self.unc)(a, c) == 0 && (
        self.out)(a, c) == Set::<int>::empty()
        &&& (self.wruns)(a) == 0 && (self.wpages)(a) == 0
        &&& (self.runs)(a) == Set::<(int, int)>::empty() && (self.pages)(a) == Set::<int>::empty()
        &&& (self.ta_runs)(a) == 0 && (self.ta_pages)(a) == 0 && (self.tf_runs)(a) == 0 && (self.tf_pages)(a) == 0
    }

    /// `ClassState::take`, under the class lock: `claimed += n`, `n` slots
    /// reserved.
    pub open spec fn reserve(self, a: int, c: int, n: int) -> Books {
        Books {
            net: |a2: int, c2: int| if a2 == a && c2 == c { (self.net)(a2, c2) + n } else { (self.net)(a2, c2) },
            resv: |a2: int, c2: int| if a2 == a && c2 == c { (self.resv)(a2, c2) + n } else { (self.resv)(a2, c2) },
            ..self
        }
    }

    /// `ClassState::finish` or `claim_prefix`: one reserved slot `x`
    /// claimed from its bitmap word (its token comes out of the word, so
    /// no holder has it: it is out through no arena).
    pub open spec fn claim(self, a: int, c: int, x: int) -> Books {
        Books {
            resv: |a2: int, c2: int| if a2 == a && c2 == c { (self.resv)(a2, c2) - 1 } else { (self.resv)(a2, c2) },
            out: |a2: int, c2: int| if a2 == a && c2 == c { (self.out)(a2, c2).insert(x) } else { (self.out)(a2, c2) },
            flight: |c2: int| if c2 == c { (self.flight)(c2).insert(x) } else { (self.flight)(c2) },
            ..self
        }
    }

    pub open spec fn claim_pre(self, a: int, c: int, x: int) -> bool {
        &&& 0 <= a < self.k
        &&& 0 <= c < NCLASSES
        &&& (self.resv)(a, c) >= 1
        &&& !self.out_upto(c, self.k).contains(x)
    }

    /// Slot `x` of class `c` moves from `from` to `to` (0 flight, 1
    /// cached, 2 live): a refill's push into a cache (flight to cached), a
    /// hand-out (flight or cached to live), a free's push (live to
    /// cached), a flush's or a direct free's take for release (cached or
    /// live to flight).
    pub open spec fn holder(self, c: int, i: int) -> Set<int> {
        if i == 0 {
            (self.flight)(c)
        } else if i == 1 {
            (self.cached)(c)
        } else {
            (self.live)(c)
        }
    }

    pub open spec fn move_slot(self, c: int, x: int, from: int, to: int) -> Books {
        let f = |c2: int, i: int|
            if c2 == c && i == from {
                self.holder(c2, i).remove(x)
            } else if c2 == c && i == to {
                self.holder(c2, i).insert(x)
            } else {
                self.holder(c2, i)
            };
        Books { flight: |c2: int| f(c2, 0), cached: |c2: int| f(c2, 1), live: |c2: int| f(c2, 2), ..self }
    }

    pub open spec fn move_pre(self, c: int, x: int, from: int, to: int) -> bool {
        &&& 0 <= c < NCLASSES
        &&& 0 <= from <= 2
        &&& 0 <= to <= 2
        &&& from != to
        &&& self.holder(c, from).contains(x)
    }

    /// `ClassState::release_word`: slot `x`, in flight, back in its bitmap
    /// word through arena `a`, which claimed it (it owns the slot's slab).
    pub open spec fn release(self, a: int, c: int, x: int) -> Books {
        Books {
            unc: |a2: int, c2: int| if a2 == a && c2 == c { (self.unc)(a2, c2) + 1 } else { (self.unc)(a2, c2) },
            out: |a2: int, c2: int| if a2 == a && c2 == c { (self.out)(a2, c2).remove(x) } else { (self.out)(a2, c2) },
            flight: |c2: int| if c2 == c { (self.flight)(c2).remove(x) } else { (self.flight)(c2) },
            ..self
        }
    }

    pub open spec fn release_pre(self, a: int, c: int, x: int) -> bool {
        &&& 0 <= a < self.k
        &&& 0 <= c < NCLASSES
        &&& (self.flight)(c).contains(x)
        &&& (self.out)(a, c).contains(x)
    }

    /// `ClassState::release_owned`'s `released.fetch_add(n)`: `n` released
    /// slots counted.
    pub open spec fn count(self, a: int, c: int, n: int) -> Books {
        Books {
            net: |a2: int, c2: int| if a2 == a && c2 == c { (self.net)(a2, c2) - n } else { (self.net)(a2, c2) },
            unc: |a2: int, c2: int| if a2 == a && c2 == c { (self.unc)(a2, c2) - n } else { (self.unc)(a2, c2) },
            ..self
        }
    }

    pub open spec fn count_pre(self, a: int, c: int, n: int) -> bool {
        &&& 0 <= a < self.k
        &&& 0 <= c < NCLASSES
        &&& 0 <= n <= (self.unc)(a, c)
    }

    /// `Huge::take`, under the huge lock: run `e` of arena `a` made live
    /// (its token's pages come from the page authority, so no live run has
    /// them).
    pub open spec fn take_run(self, a: int, e: (int, int)) -> Books {
        Books {
            runs: |a2: int| if a2 == a { (self.runs)(a2).insert(e) } else { (self.runs)(a2) },
            pages: |a2: int| if a2 == a { (self.pages)(a2).union(ext_pages(e)) } else { (self.pages)(a2) },
            ta_runs: |a2: int| if a2 == a { (self.ta_runs)(a2) + 1 } else { (self.ta_runs)(a2) },
            ta_pages: |a2: int| if a2 == a { (self.ta_pages)(a2) + e.1 } else { (self.ta_pages)(a2) },
            ..self
        }
    }

    pub open spec fn take_run_pre(self, a: int, e: (int, int)) -> bool {
        &&& 0 <= a < self.k
        &&& e.0 >= 0
        &&& e.1 > GUARD_PAGES
        &&& forall|b: int| 0 <= b < self.k ==> #[trigger] (self.pages)(b).disjoint(ext_pages(e))
    }

    /// `Shared::alloc_huge`'s `note_huge_alloc(n)`: a run of `n` pages
    /// added to arena `a`'s word.
    pub open spec fn note(self, a: int, n: int) -> Books {
        Books {
            wruns: |a2: int| if a2 == a { (self.wruns)(a2) + 1 } else { (self.wruns)(a2) },
            wpages: |a2: int| if a2 == a { (self.wpages)(a2) + n } else { (self.wpages)(a2) },
            ta_runs: |a2: int| if a2 == a { (self.ta_runs)(a2) - 1 } else { (self.ta_runs)(a2) },
            ta_pages: |a2: int| if a2 == a { (self.ta_pages)(a2) - n } else { (self.ta_pages)(a2) },
            ..self
        }
    }

    pub open spec fn note_pre(self, a: int, n: int) -> bool {
        &&& 0 <= a < self.k
        &&& (self.ta_runs)(a) >= 1
        &&& 0 <= n <= (self.ta_pages)(a)
    }

    /// `Huge::free_run`, under the huge lock: live run `e` of arena `a`
    /// retired (its token's extent is the descriptor's, whose arena is
    /// the one it was made live in).
    pub open spec fn free_run(self, a: int, e: (int, int)) -> Books {
        Books {
            runs: |a2: int| if a2 == a { (self.runs)(a2).remove(e) } else { (self.runs)(a2) },
            pages: |a2: int| if a2 == a { (self.pages)(a2).difference(ext_pages(e)) } else { (self.pages)(a2) },
            tf_runs: |a2: int| if a2 == a { (self.tf_runs)(a2) + 1 } else { (self.tf_runs)(a2) },
            tf_pages: |a2: int| if a2 == a { (self.tf_pages)(a2) + e.1 } else { (self.tf_pages)(a2) },
            ..self
        }
    }

    pub open spec fn free_run_pre(self, a: int, e: (int, int)) -> bool {
        &&& 0 <= a < self.k
        &&& (self.runs)(a).contains(e)
    }

    /// `Shared::free_huge`'s `note_huge_free(n)`: a run of `n` pages
    /// subtracted from arena `a`'s word.
    pub open spec fn unnote(self, a: int, n: int) -> Books {
        Books {
            wruns: |a2: int| if a2 == a { (self.wruns)(a2) - 1 } else { (self.wruns)(a2) },
            wpages: |a2: int| if a2 == a { (self.wpages)(a2) - n } else { (self.wpages)(a2) },
            tf_runs: |a2: int| if a2 == a { (self.tf_runs)(a2) - 1 } else { (self.tf_runs)(a2) },
            tf_pages: |a2: int| if a2 == a { (self.tf_pages)(a2) - n } else { (self.tf_pages)(a2) },
            ..self
        }
    }

    pub open spec fn unnote_pre(self, a: int, n: int) -> bool {
        &&& 0 <= a < self.k
        &&& (self.tf_runs)(a) >= 1
        &&& 0 <= n <= (self.tf_pages)(a)
    }

    // The unions and sums over arenas.

    /// A slot is in `out_upto(c, a)` iff an arena below `a` has it out.
    pub proof fn lemma_out_upto(self, c: int, a: int)
        ensures
            forall|x: int|
                #[trigger] self.out_upto(c, a).contains(x) <==> exists|b: int|
                    0 <= b < a && #[trigger] (self.out)(b, c).contains(x),
        decreases a,
    {
        if a > 0 {
            self.lemma_out_upto(c, a - 1);
            assert forall|x: int| #[trigger] self.out_upto(c, a).contains(x) <==> exists|b: int|
                0 <= b < a && #[trigger] (self.out)(b, c).contains(x) by {
                if self.out_upto(c, a).contains(x) {
                    if self.out_upto(c, a - 1).contains(x) {
                        let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.out)(b, c).contains(x);
                        assert(0 <= b < a && (self.out)(b, c).contains(x));
                    } else {
                        assert((self.out)(a - 1, c).contains(x));
                    }
                }
                if exists|b: int| 0 <= b < a && #[trigger] (self.out)(b, c).contains(x) {
                    let b = choose|b: int| 0 <= b < a && #[trigger] (self.out)(b, c).contains(x);
                    if b < a - 1 {
                        assert(self.out_upto(c, a - 1).contains(x));
                    }
                }
            }
        } else {
            assert(self.out_upto(c, a) == Set::<int>::empty());
        }
    }

    /// `out_upto` reads `out` only.
    pub proof fn lemma_out_upto_frame(self, o: Books, c: int, a: int)
        requires
            forall|b: int| 0 <= b < a ==> #[trigger] (o.out)(b, c) == (self.out)(b, c),
        ensures
            o.out_upto(c, a) == self.out_upto(c, a),
        decreases a,
    {
        if a > 0 {
            self.lemma_out_upto_frame(o, c, a - 1);
        }
    }

    /// `out_upto` after `x` joins arena `a`'s out slots of class `c`.
    pub proof fn lemma_out_upto_insert(self, o: Books, c: int, a: int, x: int, m: int)
        requires
            0 <= a,
            forall|b: int| 0 <= b < m && b != a ==> #[trigger] (o.out)(b, c) == (self.out)(b, c),
            a < m ==> (o.out)(a, c) == (self.out)(a, c).insert(x),
        ensures
            o.out_upto(c, m) == if a < m {
                self.out_upto(c, m).insert(x)
            } else {
                self.out_upto(c, m)
            },
        decreases m,
    {
        if m > 0 {
            self.lemma_out_upto_insert(o, c, a, x, m - 1);
            if a < m {
                if a == m - 1 {
                    self.lemma_out_upto_frame(o, c, m - 1);
                }
                assert(o.out_upto(c, m) =~= self.out_upto(c, m).insert(x));
            }
        }
    }

    /// `out_upto` after `x` leaves arena `a`'s out slots of class `c`,
    /// which no other arena has.
    pub proof fn lemma_out_upto_remove(self, o: Books, c: int, a: int, x: int, m: int)
        requires
            0 <= a,
            forall|b: int| 0 <= b < m && b != a ==> #[trigger] (o.out)(b, c) == (self.out)(b, c),
            forall|b: int| 0 <= b < m && b != a ==> !(#[trigger] (self.out)(b, c)).contains(x),
            a < m ==> (o.out)(a, c) == (self.out)(a, c).remove(x),
        ensures
            o.out_upto(c, m) == if a < m {
                self.out_upto(c, m).remove(x)
            } else {
                self.out_upto(c, m)
            },
        decreases m,
    {
        if m > 0 {
            self.lemma_out_upto_remove(o, c, a, x, m - 1);
            if a < m {
                if a == m - 1 {
                    self.lemma_out_upto_frame(o, c, m - 1);
                    self.lemma_out_upto(c, m - 1);
                    assert(!self.out_upto(c, m - 1).contains(x)) by {
                        if self.out_upto(c, m - 1).contains(x) {
                            let b = choose|b: int| 0 <= b < m - 1 && #[trigger] (self.out)(b, c).contains(x);
                        }
                    }
                }
                assert(o.out_upto(c, m) =~= self.out_upto(c, m).remove(x));
            }
        }
    }

    /// A page is in `pages_upto(a)` iff an arena below `a` has a live run
    /// with it; a run is in `runs_upto(a)` iff an arena below `a` has it.
    pub proof fn lemma_huge_upto(self, a: int)
        ensures
            forall|p: int|
                #[trigger] self.pages_upto(a).contains(p) <==> exists|b: int|
                    0 <= b < a && #[trigger] (self.pages)(b).contains(p),
            forall|e: (int, int)|
                #[trigger] self.runs_upto(a).contains(e) <==> exists|b: int|
                    0 <= b < a && #[trigger] (self.runs)(b).contains(e),
        decreases a,
    {
        if a > 0 {
            self.lemma_huge_upto(a - 1);
            assert forall|p: int| #[trigger] self.pages_upto(a).contains(p) <==> exists|b: int|
                0 <= b < a && #[trigger] (self.pages)(b).contains(p) by {
                if self.pages_upto(a).contains(p) {
                    if self.pages_upto(a - 1).contains(p) {
                        let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.pages)(b).contains(p);
                        assert(0 <= b < a && (self.pages)(b).contains(p));
                    } else {
                        assert((self.pages)(a - 1).contains(p));
                    }
                }
                if exists|b: int| 0 <= b < a && #[trigger] (self.pages)(b).contains(p) {
                    let b = choose|b: int| 0 <= b < a && #[trigger] (self.pages)(b).contains(p);
                    if b < a - 1 {
                        assert(self.pages_upto(a - 1).contains(p));
                    }
                }
            }
            assert forall|e: (int, int)| #[trigger] self.runs_upto(a).contains(e) <==> exists|b: int|
                0 <= b < a && #[trigger] (self.runs)(b).contains(e) by {
                if self.runs_upto(a).contains(e) {
                    if self.runs_upto(a - 1).contains(e) {
                        let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.runs)(b).contains(e);
                        assert(0 <= b < a && (self.runs)(b).contains(e));
                    } else {
                        assert((self.runs)(a - 1).contains(e));
                    }
                }
                if exists|b: int| 0 <= b < a && #[trigger] (self.runs)(b).contains(e) {
                    let b = choose|b: int| 0 <= b < a && #[trigger] (self.runs)(b).contains(e);
                    if b < a - 1 {
                        assert(self.runs_upto(a - 1).contains(e));
                    }
                }
            }
        } else {
            assert(self.pages_upto(a) == Set::<int>::empty());
            assert(self.runs_upto(a) == Set::<(int, int)>::empty());
        }
    }

    /// `out_upto`, `runs_upto` and `pages_upto` are unchanged by a step
    /// that keeps `out`, `runs` and `pages`.
    pub proof fn lemma_upto_frame(self, o: Books, m: int)
        requires
            o.out == self.out,
            o.runs == self.runs,
            o.pages == self.pages,
        ensures
            forall|c: int| #[trigger] o.out_upto(c, m) == self.out_upto(c, m),
            o.runs_upto(m) == self.runs_upto(m),
            o.pages_upto(m) == self.pages_upto(m),
        decreases m,
    {
        if m > 0 {
            self.lemma_upto_frame(o, m - 1);
            assert forall|c: int| #[trigger] o.out_upto(c, m) == self.out_upto(c, m) by {
                assert(o.out_upto(c, m - 1) == self.out_upto(c, m - 1));
                assert((o.out)(m - 1, c) == (self.out)(m - 1, c));
            }
            assert((o.runs)(m - 1) == (self.runs)(m - 1));
            assert((o.pages)(m - 1) == (self.pages)(m - 1));
        }
    }

    /// A step that keeps the huge fields keeps `huge_ok`.
    pub proof fn lemma_huge_frame(self, o: Books, a: int)
        requires
            o.runs == self.runs,
            o.pages == self.pages,
            o.wruns == self.wruns,
            o.wpages == self.wpages,
            o.ta_runs == self.ta_runs,
            o.ta_pages == self.ta_pages,
            o.tf_runs == self.tf_runs,
            o.tf_pages == self.tf_pages,
            self.huge_ok(a),
        ensures
            o.huge_ok(a),
    {
        assert((o.runs)(a) == (self.runs)(a));
        assert((o.pages)(a) == (self.pages)(a));
    }

    // Each step keeps the invariant.

    /// A new arena with zero counters keeps the invariant.
    pub proof fn lemma_step_new_arena(self)
        requires
            self.inv(),
            self.fresh(self.k),
        ensures
            self.new_arena().inv(),
    {
        let o = self.new_arena();
        self.lemma_upto_frame(o, self.k);
        assert forall|a: int, c: int| 0 <= a < o.k && 0 <= c < NCLASSES implies #[trigger] o.slots_ok(a, c) by {
            if a < self.k {
                assert(self.slots_ok(a, c));
            } else {
                assert((self.net)(a, c) == 0);
                assert((self.out)(a, c) == Set::<int>::empty());
            }
        }
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] o.holders_ok(c) by {
            assert(o.out_upto(c, self.k) == self.out_upto(c, self.k));
            assert((self.net)(self.k, c) == 0);
            assert((o.out)(self.k, c) == Set::<int>::empty());
            assert(o.out_upto(c, o.k) =~= self.out_upto(c, self.k));
            assert(self.holders_ok(c));
        }
        assert forall|a: int| 0 <= a < o.k implies #[trigger] o.huge_ok(a) by {
            if a < self.k {
                assert(self.huge_ok(a));
            } else {
                assert((o.runs)(a) == Set::<(int, int)>::empty());
                assert((o.pages)(a) == Set::<int>::empty());
            }
        }
        assert forall|a: int, b: int, c: int|
            0 <= a < o.k && 0 <= b < o.k && a != b && 0 <= c < NCLASSES implies #[trigger] (o.out)(a, c).disjoint(
            #[trigger] (o.out)(b, c)) by {
            if a < self.k && b < self.k {
                assert((self.out)(a, c).disjoint((self.out)(b, c)));
            } else {
                assert((self.net)(self.k, c) == 0);
            }
        }
        assert forall|a: int, b: int, e1: (int, int), e2: (int, int)|
            0 <= a < o.k && 0 <= b < o.k && #[trigger] (o.runs)(a).contains(e1) && #[trigger] (o.runs)(b).contains(e2)
                && (a != b || e1 != e2) implies ext_pages(e1).disjoint(ext_pages(e2)) by {
            assert((self.runs)(self.k) == Set::<(int, int)>::empty());
        }
    }

    /// A reservation keeps the invariant.
    pub proof fn lemma_step_reserve(self, a: int, c: int, n: int)
        requires
            self.inv(),
            0 <= a < self.k,
            0 <= c < NCLASSES,
            n >= 0,
        ensures
            self.reserve(a, c, n).inv(),
    {
        let o = self.reserve(a, c, n);
        self.lemma_upto_frame(o, self.k);
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            self.lemma_huge_frame(o, a2);
        }
    }

    /// A claim keeps the invariant: the slot was out through no arena.
    pub proof fn lemma_step_claim(self, a: int, c: int, x: int)
        requires
            self.inv(),
            self.claim_pre(a, c, x),
        ensures
            self.claim(a, c, x).inv(),
    {
        let o = self.claim(a, c, x);
        self.lemma_out_upto(c, self.k);
        assert(!(self.out)(a, c).contains(x));
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|a1: int, b: int, c2: int|
            0 <= a1 < o.k && 0 <= b < o.k && a1 != b && 0 <= c2 < NCLASSES implies #[trigger] (o.out)(a1, c2).disjoint(
            #[trigger] (o.out)(b, c2)) by {
            assert((self.out)(a1, c2).disjoint((self.out)(b, c2)));
            if c2 == c && (a1 == a || b == a) {
                let other = if a1 == a { b } else { a1 };
                assert(!(self.out)(other, c).contains(x));
            }
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
            if c2 == c {
                self.lemma_out_upto_insert(o, c2, a, x, self.k);
                assert(o.out_upto(c2, o.k) =~= (o.flight)(c2).union((o.cached)(c2)).union((o.live)(c2)));
            } else {
                self.lemma_out_upto_frame(o, c2, self.k);
                assert((o.flight)(c2) == (self.flight)(c2));
            }
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            self.lemma_huge_frame(o, a2);
        }
    }

    /// A move between flight, a cache and a live block keeps the
    /// invariant.
    pub proof fn lemma_step_move(self, c: int, x: int, from: int, to: int)
        requires
            self.inv(),
            self.move_pre(c, x, from, to),
        ensures
            self.move_slot(c, x, from, to).inv(),
    {
        let o = self.move_slot(c, x, from, to);
        self.lemma_upto_frame(o, self.k);
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
            if c2 == c {
                assert(o.out_upto(c2, o.k) =~= (o.flight)(c2).union((o.cached)(c2)).union((o.live)(c2)));
            } else {
                assert((o.flight)(c2) == (self.flight)(c2));
                assert((o.cached)(c2) == (self.cached)(c2));
                assert((o.live)(c2) == (self.live)(c2));
            }
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            self.lemma_huge_frame(o, a2);
        }
    }

    /// A release keeps the invariant: the slot leaves the arena's out
    /// slots and is uncounted until `count`.
    pub proof fn lemma_step_release(self, a: int, c: int, x: int)
        requires
            self.inv(),
            self.release_pre(a, c, x),
        ensures
            self.release(a, c, x).inv(),
    {
        let o = self.release(a, c, x);
        assert forall|b: int| 0 <= b < self.k && b != a implies !(#[trigger] (self.out)(b, c)).contains(x) by {
            assert((self.out)(a, c).disjoint((self.out)(b, c)));
        }
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|a1: int, b: int, c2: int|
            0 <= a1 < o.k && 0 <= b < o.k && a1 != b && 0 <= c2 < NCLASSES implies #[trigger] (o.out)(a1, c2).disjoint(
            #[trigger] (o.out)(b, c2)) by {
            assert((self.out)(a1, c2).disjoint((self.out)(b, c2)));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
            if c2 == c {
                self.lemma_out_upto_remove(o, c2, a, x, self.k);
                assert(o.out_upto(c2, o.k) =~= (o.flight)(c2).union((o.cached)(c2)).union((o.live)(c2)));
            } else {
                self.lemma_out_upto_frame(o, c2, self.k);
            }
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            self.lemma_huge_frame(o, a2);
        }
    }

    /// Counting released slots keeps the invariant.
    pub proof fn lemma_step_count(self, a: int, c: int, n: int)
        requires
            self.inv(),
            self.count_pre(a, c, n),
        ensures
            self.count(a, c, n).inv(),
    {
        let o = self.count(a, c, n);
        self.lemma_upto_frame(o, self.k);
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            self.lemma_huge_frame(o, a2);
        }
    }

    /// Making a run live keeps the invariant: its pages are no live run's.
    pub proof fn lemma_step_take_run(self, a: int, e: (int, int))
        requires
            self.inv(),
            self.take_run_pre(a, e),
        ensures
            self.take_run(a, e).inv(),
    {
        let o = self.take_run(a, e);
        let r = ext_pages(e);
        vstd::set_lib::lemma_int_range(e.0, e.0 + e.1);
        assert(r.contains(e.0));
        assert(self.huge_ok(a));
        assert(!(self.runs)(a).contains(e)) by {
            if (self.runs)(a).contains(e) {
                assert((self.pages)(a).contains(e.0));
                assert((self.pages)(a).disjoint(r));
            }
        }
        assert((self.pages)(a).disjoint(r));
        vstd::set_lib::lemma_set_disjoint_lens((self.pages)(a), r);
        assert(o.out == self.out);
        assert forall|c2: int| #[trigger] o.out_upto(c2, self.k) == self.out_upto(c2, self.k) by {
            self.lemma_out_upto_frame(o, c2, self.k);
        }
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            if a2 == a {
                assert((o.runs)(a2) == (self.runs)(a2).insert(e));
                assert((o.runs)(a2).len() == (self.runs)(a2).len() + 1);
                assert((o.pages)(a2) == (self.pages)(a2).union(r));
                assert((o.pages)(a2).len() == (self.pages)(a2).len() + e.1);
                assert forall|e2: (int, int)| #[trigger] (o.runs)(a2).contains(e2) implies e2.0 >= 0 && e2.1 > GUARD_PAGES by {
                    if e2 != e {
                        assert((self.runs)(a2).contains(e2));
                    }
                }
                assert forall|p: int| #[trigger] (o.pages)(a2).contains(p) <==> exists|e2: (int, int)|
                    #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p) by {
                    if (o.pages)(a2).contains(p) {
                        if (self.pages)(a2).contains(p) {
                            let e2 = choose|e2: (int, int)| #[trigger] (self.runs)(a2).contains(e2) && ext_pages(e2).contains(p);
                            assert((o.runs)(a2).contains(e2) && ext_pages(e2).contains(p));
                        } else {
                            assert((o.runs)(a2).contains(e) && ext_pages(e).contains(p));
                        }
                    }
                    if exists|e2: (int, int)| #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p) {
                        let e2 = choose|e2: (int, int)| #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p);
                        if e2 != e {
                            assert((self.runs)(a2).contains(e2));
                            assert((self.pages)(a2).contains(p));
                        }
                    }
                }
            } else {
                assert((o.runs)(a2) == (self.runs)(a2));
                assert((o.pages)(a2) == (self.pages)(a2));
            }
        }
        assert forall|a1: int, b: int, e1: (int, int), e2: (int, int)|
            0 <= a1 < o.k && 0 <= b < o.k && #[trigger] (o.runs)(a1).contains(e1) && #[trigger] (o.runs)(b).contains(e2)
                && (a1 != b || e1 != e2) implies ext_pages(e1).disjoint(ext_pages(e2)) by {
            let new1 = a1 == a && e1 == e && !(self.runs)(a1).contains(e1);
            let new2 = b == a && e2 == e && !(self.runs)(b).contains(e2);
            if new1 && new2 {
                // the same new run twice: excluded by `a1 != b || e1 != e2`
            } else if new1 {
                assert(self.huge_ok(b));
                assert forall|p: int| ext_pages(e2).contains(p) implies (self.pages)(b).contains(p) by {}
                assert((self.pages)(b).disjoint(r));
            } else if new2 {
                assert(self.huge_ok(a1));
                assert forall|p: int| ext_pages(e1).contains(p) implies (self.pages)(a1).contains(p) by {}
                assert((self.pages)(a1).disjoint(r));
            } else {
                assert((self.runs)(a1).contains(e1));
                assert((self.runs)(b).contains(e2));
            }
        }
    }

    /// Adding a run to its arena's word keeps the invariant.
    pub proof fn lemma_step_note(self, a: int, n: int)
        requires
            self.inv(),
            self.note_pre(a, n),
        ensures
            self.note(a, n).inv(),
    {
        let o = self.note(a, n);
        self.lemma_upto_frame(o, self.k);
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
        }
    }

    /// Retiring a run keeps the invariant: its pages are its own.
    pub proof fn lemma_step_free_run(self, a: int, e: (int, int))
        requires
            self.inv(),
            self.free_run_pre(a, e),
        ensures
            self.free_run(a, e).inv(),
    {
        let o = self.free_run(a, e);
        let r = ext_pages(e);
        assert(self.huge_ok(a));
        vstd::set_lib::lemma_int_range(e.0, e.0 + e.1);
        assert(r.subset_of((self.pages)(a)));
        vstd::set_lib::lemma_len_difference((self.pages)(a), r);
        assert((self.pages)(a).difference(r).len() == (self.pages)(a).len() - r.len()) by {
            assert((self.pages)(a).intersect(r) =~= r);
            vstd::set_lib::lemma_set_difference_len((self.pages)(a), r);
        }
        assert(o.out == self.out);
        assert forall|c2: int| #[trigger] o.out_upto(c2, self.k) == self.out_upto(c2, self.k) by {
            self.lemma_out_upto_frame(o, c2, self.k);
        }
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
            if a2 == a {
                assert forall|p: int| #[trigger] (o.pages)(a2).contains(p) <==> exists|e2: (int, int)|
                    #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p) by {
                    if (o.pages)(a2).contains(p) {
                        let e2 = choose|e2: (int, int)| #[trigger] (self.runs)(a2).contains(e2) && ext_pages(e2).contains(p);
                        assert(e2 != e);
                        assert((o.runs)(a2).contains(e2));
                    }
                    if exists|e2: (int, int)| #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p) {
                        let e2 = choose|e2: (int, int)| #[trigger] (o.runs)(a2).contains(e2) && ext_pages(e2).contains(p);
                        assert((self.runs)(a2).contains(e2) && e2 != e);
                        assert(ext_pages(e2).disjoint(r));
                    }
                }
            }
        }
    }

    /// Subtracting a run from its arena's word keeps the invariant.
    pub proof fn lemma_step_unnote(self, a: int, n: int)
        requires
            self.inv(),
            self.unnote_pre(a, n),
        ensures
            self.unnote(a, n).inv(),
    {
        let o = self.unnote(a, n);
        self.lemma_upto_frame(o, self.k);
        assert forall|a2: int, c2: int| 0 <= a2 < o.k && 0 <= c2 < NCLASSES implies #[trigger] o.slots_ok(a2, c2) by {
            assert(self.slots_ok(a2, c2));
        }
        assert forall|c2: int| 0 <= c2 < NCLASSES implies #[trigger] o.holders_ok(c2) by {
            assert(self.holders_ok(c2));
        }
        assert forall|a2: int| 0 <= a2 < o.k implies #[trigger] o.huge_ok(a2) by {
            assert(self.huge_ok(a2));
        }
    }

    /// The process starts in the invariant.
    pub proof fn lemma_start()
        ensures
            Books::start().inv(),
            Books::start().quiescent(),
    {
        let b = Books::start();
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] b.holders_ok(c) by {
            assert(b.out_upto(c, 0) == Set::<int>::empty());
            assert(b.out_upto(c, 1) =~= Set::<int>::empty());
            assert(Set::<int>::empty().union(Set::<int>::empty()).union(Set::<int>::empty()) =~= Set::<int>::empty());
        }
    }

    // What a quiescent state's counters are.

    /// With no update in progress, `claimed - released` summed over
    /// arenas `0..a` is the number of their out slots.
    pub proof fn lemma_net_upto(self, c: int, a: int)
        requires
            self.inv(),
            self.quiescent(),
            0 <= c < NCLASSES,
            0 <= a <= self.k,
        ensures
            self.net_upto(c, a) == self.out_upto(c, a).len(),
        decreases a,
    {
        if a > 0 {
            self.lemma_net_upto(c, a - 1);
            self.lemma_out_upto(c, a - 1);
            assert(self.slots_ok(a - 1, c));
            assert(self.out_upto(c, a - 1).disjoint((self.out)(a - 1, c))) by {
                assert forall|x: int| #[trigger] self.out_upto(c, a - 1).contains(x) implies !(self.out)(a - 1, c).contains(
                    x,
                ) by {
                    let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.out)(b, c).contains(x);
                    assert((self.out)(b, c).disjoint((self.out)(a - 1, c)));
                }
            }
            vstd::set_lib::lemma_set_disjoint_lens(self.out_upto(c, a - 1), (self.out)(a - 1, c));
        } else {
            assert(self.out_upto(c, 0) == Set::<int>::empty());
        }
    }

    /// With no update in progress, the `large` words of arenas `0..a`
    /// summed are their live runs and those runs' pages.
    pub proof fn lemma_words_upto(self, a: int)
        requires
            self.inv(),
            self.quiescent(),
            0 <= a <= self.k,
        ensures
            self.wruns_upto(a) == self.runs_upto(a).len(),
            self.wpages_upto(a) == self.pages_upto(a).len(),
        decreases a,
    {
        if a > 0 {
            self.lemma_words_upto(a - 1);
            self.lemma_huge_upto(a - 1);
            assert(self.huge_ok(a - 1));
            assert(self.runs_upto(a - 1).disjoint((self.runs)(a - 1))) by {
                assert forall|e: (int, int)| #[trigger] self.runs_upto(a - 1).contains(e) implies !(self.runs)(a - 1).contains(
                    e,
                ) by {
                    let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.runs)(b).contains(e);
                    assert(self.huge_ok(b));
                    vstd::set_lib::lemma_int_range(e.0, e.0 + e.1);
                    assert(ext_pages(e).contains(e.0));
                }
            }
            assert(self.pages_upto(a - 1).disjoint((self.pages)(a - 1))) by {
                assert forall|p: int| #[trigger] self.pages_upto(a - 1).contains(p) implies !(self.pages)(a - 1).contains(
                    p,
                ) by {
                    let b = choose|b: int| 0 <= b < a - 1 && #[trigger] (self.pages)(b).contains(p);
                    assert(self.huge_ok(b));
                    assert(self.huge_ok(a - 1));
                    let e1 = choose|e1: (int, int)| #[trigger] (self.runs)(b).contains(e1) && ext_pages(e1).contains(p);
                    if (self.pages)(a - 1).contains(p) {
                        let e2 = choose|e2: (int, int)| #[trigger] (self.runs)(a - 1).contains(e2) && ext_pages(e2).contains(p);
                        assert(ext_pages(e1).disjoint(ext_pages(e2)));
                    }
                }
            }
            vstd::set_lib::lemma_set_disjoint_lens(self.runs_upto(a - 1), (self.runs)(a - 1));
            vstd::set_lib::lemma_set_disjoint_lens(self.pages_upto(a - 1), (self.pages)(a - 1));
        } else {
            assert(self.runs_upto(0) == Set::<(int, int)>::empty());
            assert(self.pages_upto(0) == Set::<int>::empty());
        }
    }

    /// The counters at a quiescent state: for each class, `claimed -
    /// released` summed over the arenas is the number of cached and live
    /// slots; each arena's `large` word is its live runs and their pages,
    /// and the words summed are all live runs and their pages.
    pub proof fn theorem_quiescent(self)
        requires
            self.inv(),
            self.quiescent(),
        ensures
            forall|c: int|
                0 <= c < NCLASSES ==> #[trigger] self.net_upto(c, self.k) == (self.cached)(c).len() + (self.live)(c).len(),
            forall|a: int, c: int| 0 <= a < self.k && 0 <= c < NCLASSES ==> #[trigger] (self.net)(a, c) == (self.out)(a, c).len(),
            forall|a: int| 0 <= a < self.k ==> #[trigger] (self.wruns)(a) == (self.runs)(a).len() && (self.wpages)(a) == (
            self.pages)(a).len(),
            self.wruns_upto(self.k) == self.runs_upto(self.k).len(),
            self.wpages_upto(self.k) == self.pages_upto(self.k).len(),
    {
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] self.net_upto(c, self.k) == (self.cached)(c).len() + (
        self.live)(c).len() by {
            self.lemma_net_upto(c, self.k);
            assert(self.holders_ok(c));
            assert((self.flight)(c) == Set::<int>::empty());
            assert(self.out_upto(c, self.k) =~= (self.cached)(c).union((self.live)(c)));
            vstd::set_lib::lemma_set_disjoint_lens((self.cached)(c), (self.live)(c));
        }
        assert forall|a: int, c: int| 0 <= a < self.k && 0 <= c < NCLASSES implies #[trigger] (self.net)(a, c) == (
        self.out)(a, c).len() by {
            assert(self.slots_ok(a, c));
        }
        assert forall|a: int| 0 <= a < self.k implies #[trigger] (self.wruns)(a) == (self.runs)(a).len() && (self.wpages)(
            a,
        ) == (self.pages)(a).len() by {
            assert(self.huge_ok(a));
        }
        self.lemma_words_upto(self.k);
    }
}

// The live blocks' tokens as the ground truth.

/// The slots of class `c` among the block keys `ks`.
pub open spec fn key_slots(ks: Seq<BlockKey>, c: int) -> Set<int>
    decreases ks.len(),
{
    if ks.len() == 0 {
        Set::empty()
    } else {
        let k = ks.last();
        let s = key_slots(ks.drop_last(), c);
        if k.0 && k.1 == c {
            s.insert(k.2)
        } else {
            s
        }
    }
}

/// The run extents among the block keys `ks`.
pub open spec fn key_runs(ks: Seq<BlockKey>) -> Set<(int, int)>
    decreases ks.len(),
{
    if ks.len() == 0 {
        Set::empty()
    } else {
        let k = ks.last();
        let s = key_runs(ks.drop_last());
        if !k.0 {
            s.insert((k.1, k.2))
        } else {
            s
        }
    }
}

/// The pages of the runs among the block keys `ks`.
pub open spec fn key_pages(ks: Seq<BlockKey>) -> Set<int>
    decreases ks.len(),
{
    if ks.len() == 0 {
        Set::empty()
    } else {
        let k = ks.last();
        let s = key_pages(ks.drop_last());
        if !k.0 {
            s.union(ext_pages((k.1, k.2)))
        } else {
            s
        }
    }
}

/// The usable bytes of the blocks `ks`, summed.
pub open spec fn key_bytes(ks: Seq<BlockKey>) -> int
    decreases ks.len(),
{
    if ks.len() == 0 {
        0
    } else {
        key_bytes(ks.drop_last()) + key_len(ks.last())
    }
}

/// Keys of live blocks: slots of a class, runs past their guard.
pub open spec fn keys_ok(ks: Seq<BlockKey>) -> bool {
    &&& forall|i: int| 0 <= i < ks.len() && #[trigger] ks[i].0 ==> 0 <= ks[i].1 < NCLASSES
    &&& forall|i: int| 0 <= i < ks.len() && !#[trigger] ks[i].0 ==> ks[i].1 >= 0 && ks[i].2 > GUARD_PAGES
    &&& forall|i: int, j: int| 0 <= i < ks.len() && 0 <= j < ks.len() && i != j ==> keys_apart(#[trigger] ks[i], #[trigger] ks[j])
}

/// The keys of the tokens `live[0..n]`.
pub open spec fn live_keys(live: Map<int, Live>, n: int) -> Seq<BlockKey> {
    Seq::new(n as nat, |i: int| live[i].key())
}

/// The usable bytes of the live blocks `live[0..n]`, summed: the true
/// `allocated` of the blocks the program holds.
pub open spec fn live_bytes(live: Map<int, Live>, n: int) -> int
    decreases n,
{
    if n <= 0 {
        0
    } else {
        live_bytes(live, n - 1) + live[n - 1].len()
    }
}

/// The slots of each class in a cache, by class.
pub open spec fn cached_counts(b: Books) -> Seq<int> {
    Seq::new(NCLASSES as nat, |c: int| (b.cached)(c).len() as int)
}

/// What `key_slots`, `key_runs` and `key_pages` contain.
pub proof fn lemma_keys_contain(ks: Seq<BlockKey>)
    ensures
        forall|c: int, x: int|
            #[trigger] key_slots(ks, c).contains(x) <==> exists|i: int|
                0 <= i < ks.len() && #[trigger] ks[i] == (true, c, x),
        forall|e: (int, int)|
            #[trigger] key_runs(ks).contains(e) <==> exists|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (false, e.0, e.1),
        forall|p: int|
            #[trigger] key_pages(ks).contains(p) <==> exists|i: int|
                0 <= i < ks.len() && !(#[trigger] ks[i]).0 && ext_pages((ks[i].1, ks[i].2)).contains(p),
    decreases ks.len(),
{
    if ks.len() > 0 {
        let k0 = ks.drop_last();
        lemma_keys_contain(k0);
        let l = ks.len() - 1;
        assert forall|i: int| 0 <= i < l implies #[trigger] ks[i] == k0[i] by {}
        assert forall|c: int, x: int| #[trigger] key_slots(ks, c).contains(x) <==> exists|i: int|
            0 <= i < ks.len() && #[trigger] ks[i] == (true, c, x) by {
            if key_slots(ks, c).contains(x) {
                if key_slots(k0, c).contains(x) {
                    let i = choose|i: int| 0 <= i < k0.len() && #[trigger] k0[i] == (true, c, x);
                    assert(ks[i] == (true, c, x));
                } else {
                    assert(ks[l] == (true, c, x));
                }
            }
            if exists|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (true, c, x) {
                let i = choose|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (true, c, x);
                if i < l {
                    assert(k0[i] == (true, c, x));
                }
            }
        }
        assert forall|e: (int, int)| #[trigger] key_runs(ks).contains(e) <==> exists|i: int|
            0 <= i < ks.len() && #[trigger] ks[i] == (false, e.0, e.1) by {
            if key_runs(ks).contains(e) {
                if key_runs(k0).contains(e) {
                    let i = choose|i: int| 0 <= i < k0.len() && #[trigger] k0[i] == (false, e.0, e.1);
                    assert(ks[i] == (false, e.0, e.1));
                } else {
                    assert(ks[l] == (false, e.0, e.1));
                }
            }
            if exists|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (false, e.0, e.1) {
                let i = choose|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (false, e.0, e.1);
                if i < l {
                    assert(k0[i] == (false, e.0, e.1));
                }
            }
        }
        assert forall|p: int| #[trigger] key_pages(ks).contains(p) <==> exists|i: int|
            0 <= i < ks.len() && !(#[trigger] ks[i]).0 && ext_pages((ks[i].1, ks[i].2)).contains(p) by {
            if key_pages(ks).contains(p) {
                if key_pages(k0).contains(p) {
                    let i = choose|i: int| 0 <= i < k0.len() && !(#[trigger] k0[i]).0 && ext_pages((k0[i].1, k0[i].2)).contains(p);
                    assert(ks[i] == k0[i]);
                } else {
                    assert(!ks[l].0 && ext_pages((ks[l].1, ks[l].2)).contains(p));
                }
            }
            if exists|i: int| 0 <= i < ks.len() && !(#[trigger] ks[i]).0 && ext_pages((ks[i].1, ks[i].2)).contains(p) {
                let i = choose|i: int| 0 <= i < ks.len() && !(#[trigger] ks[i]).0 && ext_pages((ks[i].1, ks[i].2)).contains(p);
                if i < l {
                    assert(k0[i] == ks[i]);
                }
            }
        }
    }
}

/// `class_bytes` of one slot of class `c`.
pub proof fn lemma_class_bytes_unit(c: int, lo: int, hi: int)
    requires
        0 <= c < NCLASSES,
        0 <= lo,
        hi <= NCLASSES,
    ensures
        class_bytes(Seq::new(NCLASSES as nat, |j: int| if j == c { 1int } else { 0int }), lo, hi) == if lo <= c < hi {
            class_table()[c].size as int
        } else {
            0
        },
    decreases hi - lo,
{
    if hi > lo {
        lemma_class_bytes_unit(c, lo, hi - 1);
        let u = Seq::new(NCLASSES as nat, |j: int| if j == c { 1int } else { 0int });
        let z = class_table()[hi - 1].size as int;
        if hi - 1 == c {
            assert(u[hi - 1] == 1);
            assert(u[hi - 1] * z == z);
        } else if 0 <= hi - 1 < NCLASSES {
            assert(u[hi - 1] == 0);
            assert(u[hi - 1] * z == 0);
        }
    }
}

/// Pairwise different blocks count once each: their bytes are the bytes
/// of their slots by class plus those of their runs, whose pages are
/// apart.
pub proof fn lemma_key_bytes(ks: Seq<BlockKey>)
    requires
        keys_ok(ks),
    ensures
        key_bytes(ks) == class_bytes(Seq::new(NCLASSES as nat, |c: int| key_slots(ks, c).len() as int), 0, NCLASSES as int)
            + huge_bytes(key_runs(ks).len() as int, key_pages(ks).len() as int),
    decreases ks.len(),
{
    let n = Seq::new(NCLASSES as nat, |c: int| key_slots(ks, c).len() as int);
    if ks.len() == 0 {
        lemma_class_bytes_zero(n, 0, NCLASSES as int);
        let pg = crate::heap::live::page_size();
        assert((0 - 0 * GUARD_PAGES) * pg == 0) by (nonlinear_arith);
    } else {
        let k0 = ks.drop_last();
        let l = ks.len() - 1;
        let k = ks.last();
        assert forall|i: int| 0 <= i < l implies #[trigger] ks[i] == k0[i] by {}
        assert(keys_ok(k0)) by {
            assert forall|i: int, j: int| 0 <= i < k0.len() && 0 <= j < k0.len() && i != j implies keys_apart(#[trigger] k0[i], #[trigger] k0[j]) by {
                assert(keys_apart(ks[i], ks[j]));
            }
        }
        lemma_key_bytes(k0);
        lemma_keys_contain(k0);
        let n0 = Seq::new(NCLASSES as nat, |c: int| key_slots(k0, c).len() as int);
        if k.0 {
            let c = k.1;
            assert(0 <= c < NCLASSES);
            assert(!key_slots(k0, c).contains(k.2)) by {
                if key_slots(k0, c).contains(k.2) {
                    let i = choose|i: int| 0 <= i < k0.len() && #[trigger] k0[i] == (true, c, k.2);
                    assert(keys_apart(ks[i], ks[l]));
                }
            }
            let u = Seq::new(NCLASSES as nat, |j: int| if j == c { 1int } else { 0int });
            lemma_class_bytes_add(n0, u, n, 0, NCLASSES as int);
            lemma_class_bytes_unit(c, 0, NCLASSES as int);
            assert(key_runs(ks) == key_runs(k0));
            assert(key_pages(ks) == key_pages(k0));
        } else {
            let e = (k.1, k.2);
            assert(e.1 > GUARD_PAGES);
            lemma_class_bytes_frame(n0, n, 0, NCLASSES as int);
            assert(!key_runs(k0).contains(e)) by {
                if key_runs(k0).contains(e) {
                    let i = choose|i: int| 0 <= i < k0.len() && #[trigger] k0[i] == (false, e.0, e.1);
                    assert(keys_apart(ks[i], ks[l]));
                }
            }
            assert(key_pages(k0).disjoint(ext_pages(e))) by {
                assert forall|p: int| #[trigger] key_pages(k0).contains(p) implies !ext_pages(e).contains(p) by {
                    let i = choose|i: int| 0 <= i < k0.len() && !(#[trigger] k0[i]).0 && ext_pages((k0[i].1, k0[i].2)).contains(p);
                    assert(keys_apart(ks[i], ks[l]));
                }
            }
            vstd::set_lib::lemma_set_disjoint_lens(key_pages(k0), ext_pages(e));
            vstd::set_lib::lemma_int_range(e.0, e.0 + e.1);
            lemma_huge_bytes_add(key_runs(k0).len() as int, key_pages(k0).len() as int, 1, e.1);
        }
    }
}

/// The arenas' figures `reads` (with huge pages `pages`) read the ledger's
/// counters of arenas `0..a`: their sums are the ledger's.
pub proof fn lemma_reads_books(b: Books, reads: Seq<ArenaStats>, pages: Seq<int>, a: int)
    requires
        0 <= a <= reads.len(),
        reads.len() == pages.len(),
        forall|i: int, c: int| 0 <= i < reads.len() && 0 <= c < NCLASSES ==> #[trigger] reads[i].bins[c].curregs == (b.net)(i, c),
        forall|i: int| 0 <= i < reads.len() ==> #[trigger] reads[i].large_runs == (b.wruns)(i),
        forall|i: int| 0 <= i < reads.len() ==> #[trigger] pages[i] == (b.wpages)(i),
    ensures
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] sum_curregs(reads.subrange(0, a), c) == b.net_upto(c, a),
        sum_runs(reads.subrange(0, a)) == b.wruns_upto(a),
        sum_ints(pages.subrange(0, a)) == b.wpages_upto(a),
    decreases a,
{
    if a > 0 {
        lemma_reads_books(b, reads, pages, a - 1);
        assert(reads.subrange(0, a).drop_last() =~= reads.subrange(0, a - 1));
        assert(pages.subrange(0, a).drop_last() =~= pages.subrange(0, a - 1));
        assert(reads.subrange(0, a).last() == reads[a - 1]);
        assert(pages.subrange(0, a).last() == pages[a - 1]);
        assert(reads[a - 1].large_runs == (b.wruns)(a - 1));
        assert forall|c: int| 0 <= c < NCLASSES implies #[trigger] sum_curregs(reads.subrange(0, a), c) == b.net_upto(c, a) by {
            assert(reads[a - 1].bins[c].curregs == (b.net)(a - 1, c));
            assert(sum_curregs(reads.subrange(0, a), c) == sum_curregs(reads.subrange(0, a).drop_last(), c) + reads.subrange(0, a).last().bins[c].curregs);
        }
    } else {
        lemma_reads_empty();
        assert(reads.subrange(0, 0) =~= Seq::<ArenaStats>::empty());
        assert(pages.subrange(0, 0) =~= Seq::<int>::empty());
    }
}

/// The live tokens' bytes are their keys'.
pub proof fn lemma_live_bytes(live: Map<int, Live>, n: int)
    requires
        n >= 0,
        forall|i: int| 0 <= i < n ==> #[trigger] live.contains_key(i),
        forall|i: int| 0 <= i < n ==> #[trigger] live[i].wf(),
    ensures
        live_bytes(live, n) == key_bytes(live_keys(live, n)),
    decreases n,
{
    if n > 0 {
        assert forall|i: int| 0 <= i < n - 1 implies #[trigger] live.contains_key(i) && live[i].wf() by {
            assert(live.contains_key(i));
        }
        lemma_live_bytes(live, n - 1);
        assert(live_keys(live, n).drop_last() =~= live_keys(live, n - 1));
        assert(live.contains_key(n - 1) && live[n - 1].wf());
        live[n - 1].lemma_key();
        assert(live_keys(live, n).last() == live[n - 1].key());
    } else {
        assert(live_keys(live, n).len() == 0);
    }
}

/// Exact stats at a quiescent state (`verus/THEOREMS.md`, "Exact stats").
///
/// Let `live[0..n]` be the tokens of the program's live blocks, held at
/// once, and `b` the ledger, quiescent: no counter update in progress.
/// Suppose the figures `reads` a scan of the arenas read, with the pages
/// `pages[i]` of arena `i`'s `large` word (what `collect_into`,
/// `collect_all` and `mallinfo2` hand out through their ghost `rd`), are
/// the ledger's counters of arenas `0..k` in order (every arena below
/// `arenas.count()` exists; at a quiescent state every load reads the last
/// store, and `claimed - released` is below `2^64` and below the clamp),
/// and the ledger's live blocks are the tokens' blocks. Then their true
/// `allocated` ([`reads_allocated`], what those functions report when it
/// fits a `usize`: `totals_exact`, `summed`) is the usable bytes of the
/// live blocks plus the cached slots' bytes.
pub proof fn theorem_exact_stats(
    b: Books,
    tracked live: Map<int, Live>,
    n: int,
    reads: Seq<ArenaStats>,
    pages: Seq<int>,
)
    requires
        b.inv(),
        b.quiescent(),
        reads_ok(reads, pages),
        reads.len() == b.k,
        forall|i: int, c: int| 0 <= i < reads.len() && 0 <= c < NCLASSES ==> #[trigger] reads[i].bins[c].curregs == (b.net)(i, c),
        forall|i: int| 0 <= i < reads.len() ==> #[trigger] reads[i].large_runs == (b.wruns)(i),
        forall|i: int| 0 <= i < reads.len() ==> #[trigger] pages[i] == (b.wpages)(i),
        forall|i: int| 0 <= i < n ==> #[trigger] live.contains_key(i) && live[i].wf(),
        forall|c: int| 0 <= c < NCLASSES ==> #[trigger] (b.live)(c) == key_slots(live_keys(live, n), c),
        n >= 0,
        b.runs_upto(b.k) == key_runs(live_keys(live, n)),
    ensures
        reads_allocated(reads, pages) == live_bytes(live, n) + class_bytes(cached_counts(b), 0, NCLASSES as int),
{
    let ghost l0 = live;
    let tracked mut m = live;
    lemma_all_apart(&mut m, n);
    let ks = live_keys(l0, n);
    assert(ks.len() == n);
    assert forall|i: int| 0 <= i < n implies #[trigger] ks[i] == m[i].key() && l0.contains_key(i) && l0[i].wf() by {
        assert(l0.contains_key(i));
        assert(m.contains_key(i));
    }
    assert(keys_ok(ks)) by {
        assert forall|i: int| 0 <= i < ks.len() && #[trigger] ks[i].0 implies 0 <= ks[i].1 < NCLASSES by {
            assert(l0.contains_key(i));
            l0[i].lemma_key();
        }
        assert forall|i: int| 0 <= i < ks.len() && !#[trigger] ks[i].0 implies ks[i].1 >= 0 && ks[i].2 > GUARD_PAGES by {
            assert(l0.contains_key(i));
            l0[i].lemma_key();
        }
        assert forall|i: int, j: int| 0 <= i < ks.len() && 0 <= j < ks.len() && i != j implies keys_apart(#[trigger] ks[i], #[trigger] ks[j]) by {
            assert(l0.contains_key(i) && l0.contains_key(j));
            assert(m.contains_key(i) && m.contains_key(j));
            assert(keys_apart(m[i].key(), m[j].key()));
        }
    }
    lemma_key_bytes(ks);
    assert forall|i: int| 0 <= i < n implies #[trigger] l0[i].wf() by {
        assert(l0.contains_key(i));
    }
    lemma_live_bytes(l0, n);
    lemma_keys_contain(ks);
    b.theorem_quiescent();
    lemma_reads_books(b, reads, pages, b.k);
    assert(reads.subrange(0, b.k) =~= reads);
    assert(pages.subrange(0, b.k) =~= pages);
    // The ledger's live pages are the runs' pages.
    b.lemma_huge_upto(b.k);
    assert(b.pages_upto(b.k) =~= key_pages(ks)) by {
        assert forall|p: int| #[trigger] b.pages_upto(b.k).contains(p) <==> key_pages(ks).contains(p) by {
            if b.pages_upto(b.k).contains(p) {
                let a = choose|a: int| 0 <= a < b.k && #[trigger] (b.pages)(a).contains(p);
                assert(b.huge_ok(a));
                let e = choose|e: (int, int)| #[trigger] (b.runs)(a).contains(e) && ext_pages(e).contains(p);
                assert(b.runs_upto(b.k).contains(e));
                let i = choose|i: int| 0 <= i < ks.len() && #[trigger] ks[i] == (false, e.0, e.1);
                assert(!ks[i].0 && ext_pages((ks[i].1, ks[i].2)).contains(p));
            }
            if key_pages(ks).contains(p) {
                let i = choose|i: int| 0 <= i < ks.len() && !(#[trigger] ks[i]).0 && ext_pages((ks[i].1, ks[i].2)).contains(p);
                let e = (ks[i].1, ks[i].2);
                assert(key_runs(ks).contains(e));
                let a = choose|a: int| 0 <= a < b.k && #[trigger] (b.runs)(a).contains(e);
                assert(b.huge_ok(a));
                assert((b.pages)(a).contains(p));
            }
        }
    }
    // Slots: cached plus live, by class.
    let nk = Seq::new(NCLASSES as nat, |c: int| key_slots(ks, c).len() as int);
    let cc = cached_counts(b);
    let all = all_curregs(reads);
    assert forall|c: int| 0 <= c < NCLASSES implies all[c] == cc[c] + nk[c] by {
        assert(b.net_upto(c, b.k) == (b.cached)(c).len() + (b.live)(c).len());
    }
    lemma_class_bytes_add(cc, nk, all, 0, NCLASSES as int);
}

} // verus!
