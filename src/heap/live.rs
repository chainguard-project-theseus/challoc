//! The live-block ghost model: a [`Live`] token stands for one block the
//! allocator hands out, a slab slot or a huge run, with the permission to
//! its usable bytes ([`UserMem`]).
//!
//! A slot's token holds the slot's uniqueness token, the one the class
//! bitmap or a claim held before; a run's holds its [`RunToken`]. The
//! `check_zero` reads are its users: `ClassState::check_zero` makes the
//! token of each slot a claim just took ([`Live::from_slot`]) and
//! `Huge::check_zero` the token of the run `take` just handed it
//! ([`Live::from_run`]); each reads the block's words through the
//! permission and takes the slot or run token back out
//! ([`Live::take_slot`], [`Live::into_run`]), dropping the permission.

#[cfg(verus_keep_ghost)]
use crate::class::slot_loc;
#[cfg(verus_keep_ghost)]
use crate::huge::GUARD_PAGES;
use crate::huge::RunToken;
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, class_table, process_regions};
use crate::vspec::user::UserMem;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;
use vstd::resource::set::GhostSingleton;

verus! {

/// Ghost token of a live block: the block at `addr`, a slot of class
/// `class` holding its slot token (`slot`), or a huge run holding its
/// [`RunToken`] (`run`), with the permission to its usable bytes (`mem`,
/// [`UserMem`]). Made by [`Live::from_slot`] and [`Live::from_run`];
/// taken apart by [`Live::take_slot`] and [`Live::into_run`], which drop
/// the permission as they give the slot or run token out.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct Live {
    ghost addr: usize,
    ghost class: int,
    tracked slot: Option<GhostSingleton<int>>,
    tracked run: Option<RunToken>,
    tracked mem: Option<UserMem>,
}

/// The process's page, in bytes.
pub open spec fn page_size() -> int {
    (1usize << process_regions().lg_page) as int
}

/// The address of page `s` of the process's huge region.
pub open spec fn page_addr(s: int) -> int {
    process_regions().huge_base + s * page_size()
}

/// The usable bytes of a run of `n` pages, its guard included: the pages
/// before the guard.
pub open spec fn run_usable(n: int) -> int {
    (n - GUARD_PAGES) * page_size()
}

impl Live {
    /// The block's address.
    pub closed spec fn addr(self) -> usize {
        self.addr
    }

    /// The block's usable bytes: the class's size for a slot, the pages
    /// before the guard for a run.
    pub closed spec fn len(self) -> int {
        match self.run {
            Some(t) => run_usable(t.npages()),
            None => class_table()[self.class].size as int,
        }
    }

    /// The token holds the slot token of `addr` in class `c`.
    pub closed spec fn slot_of(self, c: int) -> bool {
        &&& self.class == c
        &&& self.slot matches Some(t) && t.id() == slot_loc(c) && t@ == self.addr as int
    }

    /// The token holds the run token of the run that starts at `addr`.
    pub closed spec fn run_of(self) -> bool {
        &&& self.run matches Some(t) && t.wf() && page_addr(t.start()) == self.addr as int
    }

    /// The token holds the permission to exactly the block's usable bytes.
    pub closed spec fn mem_ok(self) -> bool {
        &&& self.mem matches Some(m) && m.covers(self.addr as int, self.len())
    }

    /// The permission the token holds (meaningful under [`Live::mem_ok`]).
    pub closed spec fn mem(self) -> UserMem {
        self.mem.unwrap()
    }

    /// The provenance of the pointers the block's permission is valid
    /// through.
    pub open spec fn prov(self) -> Provenance {
        self.mem().provenance()
    }

    /// A live block's token holds the permission to its usable bytes.
    pub proof fn lemma_mem(&self)
        requires
            self.wf(),
        ensures
            self.mem_ok(),
            self.mem().covers(self.addr() as int, self.len()),
    {
    }

    /// Borrow the permission, for a read of the block's bytes.
    pub proof fn mem_ref(tracked &self) -> (tracked r: &UserMem)
        requires
            self.mem_ok(),
        ensures
            *r == self.mem(),
    {
        self.mem.tracked_borrow()
    }

    /// A live block: a slot of class `class` at `addr` with its slot
    /// token, or a run at `addr` with its run token and its usable bytes in
    /// the huge region; with the permission to its usable bytes.
    pub closed spec fn wf(self) -> bool {
        &&& {
            ||| {
                &&& self.slot_of(self.class)
                &&& self.run is None
                &&& process_regions().is_slot(self.class, self.addr)
            }
            ||| {
                &&& self.slot is None
                &&& self.run_of()
                &&& process_regions().is_huge_block(self.addr, self.len())
            }
        }
        &&& self.mem_ok()
    }

    /// The token of the live block at `p`, whose permission is valid
    /// through `p`.
    pub open spec fn block(self, p: *mut u8) -> bool {
        &&& self.wf()
        &&& self.addr() == p@.addr
        &&& self.prov() == p@.provenance
    }

    /// The token of the live slot `p` of class `c`.
    pub open spec fn slot_block(self, p: *mut u8, c: int) -> bool {
        &&& self.block(p)
        &&& self.slot_of(c)
    }

    /// The token of slot `t@` of class `c`, from its slot token, with the
    /// permission to the slot's bytes ([`axiom_hand_out`]).
    pub proof fn from_slot(tracked t: GhostSingleton<int>, c: int, p: *mut u8) -> (tracked r: Live)
        requires
            crate::heap::regions_ok(),
            t.id() == slot_loc(c),
            t@ == p@.addr as int,
            process_regions().is_slot(c, p@.addr),
        ensures
            r.block(p),
            r.slot_of(c),
            r.slot_tok() == t,
            r.len() == class_table()[c].size,
    {
        let len = class_table()[c].size as int;
        assert(process_regions().is_slot(c, p@.addr) && len == class_table()[c].size);
        let tracked m = axiom_hand_out(p, len);
        Live { addr: p@.addr, class: c, slot: Some(t), run: None, mem: Some(m) }
    }

    /// The token of the run `t` at `p`, whose usable bytes are in the huge
    /// region, with the permission to them ([`axiom_hand_out`]).
    pub proof fn from_run(tracked t: RunToken, p: *mut u8) -> (tracked r: Live)
        requires
            crate::heap::regions_ok(),
            t.wf(),
            page_addr(t.start()) == p@.addr as int,
            process_regions().is_huge_block(p@.addr, run_usable(t.npages())),
        ensures
            r.block(p),
            r.run_opt() == Some(t),
            r.len() == run_usable(t.npages()),
    {
        let len = run_usable(t.npages());
        let tracked m = axiom_hand_out(p, len);
        Live { addr: p@.addr, class: 0, slot: None, run: Some(t), mem: Some(m) }
    }

    /// The slot token `self` holds.
    pub closed spec fn slot_tok(self) -> GhostSingleton<int> {
        self.slot.unwrap()
    }

    /// Take the slot token out. The permission to the slot's bytes is used
    /// up with it (dropped): a slot token is never apart from a permission
    /// that is still held, so [`axiom_hand_out`] never mints a second one
    /// ([`Live::from_slot`] takes a bare token).
    pub proof fn take_slot(tracked self, c: int) -> (tracked t: GhostSingleton<int>)
        requires
            self.slot_of(c),
        ensures
            t == self.slot_tok(),
            t.id() == slot_loc(c),
            t@ == self.addr() as int,
    {
        let tracked Live { addr, class, slot, run, mem } = self;
        slot.tracked_unwrap()
    }

    /// The live run that starts at page `page` of the huge region: its
    /// token, at the page's address, with the permission to its usable
    /// bytes.
    pub open spec fn run_block(self, page: int) -> bool {
        &&& self.wf()
        &&& self.run_opt() is Some
        &&& self.run_opt().unwrap().wf()
        &&& self.run_opt().unwrap().start() == page
        &&& self.addr() == page_addr(page)
        &&& self.len() == run_usable(self.run_opt().unwrap().npages())
    }

    /// Take the run token out of the token of the run at page `page`. The
    /// permission to the run's bytes is used up with it, as in
    /// [`Live::take_slot`].
    pub proof fn into_run(tracked self, page: int) -> (tracked t: RunToken)
        requires
            self.run_block(page),
        ensures
            t.wf(),
            t.start() == page,
            Some(t) == self.run_opt(),
    {
        let tracked Live { addr, class, slot, run, mem } = self;
        run.tracked_unwrap()
    }

    /// The run token the token holds, if any.
    pub closed spec fn run_opt(self) -> Option<RunToken> {
        self.run
    }

}

/// The permission to the `len` usable bytes of the block at `p`, a slot
/// of a class (`len` its size) or a huge run, valid through `p`. Trusted,
/// for what the allocator does not track: the block's bytes are committed
/// (a slot's slab is committed before its first slot is claimed and
/// decommitted only when it holds no allocated slot; a run's pages are
/// committed by `Huge::prepare`, before `Huge::take`, and released only by
/// its free), and no other permission covers them. The commit is not in
/// the premises: the axiom trusts that its callers' slot tokens come from
/// a claim and their run tokens from `Huge::take` (the `check_zero` reads,
/// the only callers of [`Live::from_slot`] and [`Live::from_run`]). A slot
/// token still in the class bitmap's word invariants (a free slot, maybe
/// of a slab above the commit watermark) would get a permission to
/// uncommitted bytes; no caller makes one from such a token (no `PointsToRaw` is
/// made for the class or huge regions but here and in
/// `class::axiom_fresh_slab_mem`, the `check_zero` read of a `Fresh` slab,
/// none of whose slots has a token outside the bitmap; the metadata
/// region's permissions lie apart from them).
///
/// At most one permission per block at a time, by the discipline of this
/// module's API, which Verus checks at every caller: the axiom is private,
/// and its callers, [`Live::from_slot`] and [`Live::from_run`], each use up
/// a bare slot or run token into the token they make with the permission.
/// A permission exists only inside the [`Live`] that holds its block's
/// slot or run token, and the slot or run token comes out bare only
/// through [`Live::take_slot`] and [`Live::into_run`], which drop the
/// permission. There is one token per block (the class bitmap's and the
/// huge allocator's authorities), so no second permission can be minted
/// for bytes whose first is still held. The one permission made elsewhere,
/// `class::axiom_fresh_slab_mem`'s, covers a `Fresh` slab's slots, which no
/// claim has taken (their tokens are in the bitmap), so this axiom mints
/// none for them while it is lent.
axiom fn axiom_hand_out(p: *mut u8, len: int) -> (tracked m: UserMem)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(p@.addr, len) || exists|c: int|
            #[trigger] process_regions().is_slot(c, p@.addr) && len == class_table()[c].size,
    ensures
        m.covers(p@.addr as int, len),
        m.provenance() == p@.provenance,
;

// Exact stats: which block a token
// stands for, and that two tokens stand for different blocks.

/// The block a token stands for, `(slot, x, y)`: a slot of class `x` at
/// address `y`, or (`!slot`) a run of `y` pages (its guard included)
/// from page `x` of the huge region.
// Ghost: only proofs name it.
#[allow(dead_code)]
pub type BlockKey = (bool, int, int);

/// Two blocks a ledger counts once each: different slots, or runs whose
/// pages are apart.
pub open spec fn keys_apart(k1: BlockKey, k2: BlockKey) -> bool {
    &&& k1.0 && k2.0 ==> (k1.1, k1.2) != (k2.1, k2.2)
    &&& !k1.0 && !k2.0 ==> k1.1 + k1.2 <= k2.1 || k2.1 + k2.2 <= k1.1
}

/// The usable bytes of the block `k` stands for: its class's size, or the
/// run's pages before its guard.
pub open spec fn key_len(k: BlockKey) -> int {
    if k.0 {
        class_table()[k.1].size as int
    } else {
        run_usable(k.2)
    }
}

impl Live {
    /// The block this token stands for.
    pub closed spec fn key(self) -> BlockKey {
        match self.run {
            Some(t) => (false, t.start(), t.npages()),
            None => (true, self.class, self.addr as int),
        }
    }

    /// A live token's key: a slot of a class, or a run past its guard
    /// from a page of the region; the token's usable bytes are the key's.
    pub proof fn lemma_key(&self)
        requires
            self.wf(),
        ensures
            self.key().0 ==> 0 <= self.key().1 < NCLASSES && self.key().2 == self.addr(),
            !self.key().0 ==> self.key().1 >= 0 && self.key().2 > GUARD_PAGES,
            self.len() == key_len(self.key()),
    {
        if let Some(t) = self.run {
            t.lemma_wf();
        }
    }
}

/// Two live tokens held at once stand for different blocks
/// ([`keys_apart`]): two slot tokens of one class are of different slots,
/// two run tokens hold disjoint pages. `a` keeps its key.
pub proof fn lemma_keys_apart(tracked a: &mut Live, tracked b: &Live)
    requires
        old(a).wf(),
        b.wf(),
    ensures
        final(a).wf(),
        final(a).key() == old(a).key(),
        final(a).addr() == old(a).addr(),
        final(a).len() == old(a).len(),
        final(a).mem() == old(a).mem(),
        keys_apart(final(a).key(), b.key()),
{
    if a.run is None && b.run is None && a.class == b.class {
        let tracked mut s = a.slot.tracked_take();
        s.disjoint(b.slot.tracked_borrow());
        a.slot = Some(s);
    } else if a.run is Some && b.run is Some {
        let tracked mut t = a.run.tracked_take();
        t.lemma_apart(b.run.tracked_borrow());
        a.run = Some(t);
    }
}

/// The tokens `m[0..n]`, held at once, stand for pairwise different
/// blocks; the tokens keep their keys.
pub proof fn lemma_all_apart(tracked m: &mut Map<int, Live>, n: int)
    requires
        forall|i: int| #![trigger old(m).contains_key(i)] #![trigger old(m)[i]] 0 <= i < n ==> old(m).contains_key(i) && old(m)[i].wf(),
    ensures
        forall|i: int| #![trigger final(m).contains_key(i)] #![trigger final(m)[i]] 0 <= i < n ==> final(m).contains_key(i) && final(m)[i].wf() && final(m)[i].key()
            == old(m)[i].key() && final(m)[i].len() == old(m)[i].len(),
        forall|i: int, j: int| 0 <= i < n && 0 <= j < n && i != j ==> keys_apart(#[trigger] final(m)[i].key(), #[trigger] final(m)[j].key()),
        forall|i: int| #![trigger final(m).contains_key(i)] #![trigger old(m).contains_key(i)] !(0 <= i < n) ==> final(m).contains_key(i) == old(m).contains_key(i) && (old(
            m,
        ).contains_key(i) ==> final(m)[i] == old(m)[i]),
    decreases n,
{
    if n > 0 {
        let ghost m0 = *m;
        lemma_all_apart(m, n - 1);
        let ghost m1 = *m;
        assert(m1.contains_key(n - 1) && m1[n - 1] == m0[n - 1]);
        let tracked mut last = m.tracked_remove(n - 1);
        let ghost m2 = *m;
        lemma_one_apart(m, &mut last, n - 1);
        let ghost m3 = *m;
        let ghost l = last;
        m.tracked_insert(n - 1, last);
        assert forall|i: int| #![trigger m[i]] #![trigger m3[i]] 0 <= i < n - 1 implies m[i] == m3[i] && m3[i].key() == m1[i].key() by {
            assert(m3.contains_key(i));
            assert(m2[i] == m1[i]);
        }
        assert(m[n - 1] == l);
        assert forall|i: int, j: int| 0 <= i < n && 0 <= j < n && i != j implies keys_apart(#[trigger] m[i].key(), #[trigger] m[j].key()) by {
            if i < n - 1 && j < n - 1 {
                assert(keys_apart(m1[i].key(), m1[j].key()));
            } else if i == n - 1 {
                assert(m3.contains_key(j));
                assert(keys_apart(l.key(), m3[j].key()));
            } else {
                assert(m3.contains_key(i));
                assert(keys_apart(m3[i].key(), l.key()));
            }
        }
        assert forall|i: int| #![trigger m.contains_key(i)] #![trigger m[i]] 0 <= i < n implies m.contains_key(i) && m[i].wf() && m[i].key() == m0[i].key() && m[i].len() == m0[i].len() by {
            if i < n - 1 {
                assert(m3.contains_key(i));
                assert(m2[i] == m1[i]);
            }
        }
        assert forall|i: int| #![trigger m.contains_key(i)] #![trigger m0.contains_key(i)] !(0 <= i < n) implies m.contains_key(i) == m0.contains_key(i) && (m0.contains_key(i) ==> m[i] == m0[i]) by {
            assert(m1.contains_key(i) == m0.contains_key(i));
            assert(m2.contains_key(i) == m1.contains_key(i));
            assert(m3.contains_key(i) == m2.contains_key(i));
        }
    }
}

/// `t` and each of the tokens `m[0..n]` stand for different blocks, both
/// ways round; every token keeps its key.
proof fn lemma_one_apart(tracked m: &mut Map<int, Live>, tracked t: &mut Live, n: int)
    requires
        forall|i: int| #![trigger old(m).contains_key(i)] #![trigger old(m)[i]] 0 <= i < n ==> old(m).contains_key(i) && old(m)[i].wf(),
        old(t).wf(),
    ensures
        forall|i: int| #![trigger final(m).contains_key(i)] #![trigger final(m)[i]] 0 <= i < n ==> final(m).contains_key(i) && final(m)[i].wf() && final(m)[i].key()
            == old(m)[i].key() && final(m)[i].len() == old(m)[i].len(),
        forall|i: int| 0 <= i < n ==> #[trigger] final(m).contains_key(i) == old(m).contains_key(i),
        forall|i: int| #![trigger final(m)[i]] 0 <= i < n ==> keys_apart(final(m)[i].key(), final(t).key()) && keys_apart(
            final(t).key(),
            final(m)[i].key(),
        ),
        final(t).wf(),
        final(t).key() == old(t).key(),
        final(t).addr() == old(t).addr(),
        final(t).len() == old(t).len(),
        forall|i: int| #![trigger final(m).contains_key(i)] #![trigger old(m).contains_key(i)] !(0 <= i < n) ==> final(m).contains_key(i) == old(m).contains_key(i) && (old(
            m,
        ).contains_key(i) ==> final(m)[i] == old(m)[i]),
    decreases n,
{
    if n > 0 {
        let ghost m0 = *m;
        lemma_one_apart(m, t, n - 1);
        // Spelled out for the solver: the recursive call keeps `m[n - 1]`.
        assert(m.contains_key(n - 1) && m[n - 1] == m0[n - 1]);
        assert(m[n - 1].wf());
        let ghost m1 = *m;
        let tracked mut x = m.tracked_remove(n - 1);
        lemma_keys_apart(&mut x, t);
        lemma_keys_apart(t, &x);
        m.tracked_insert(n - 1, x);
        assert forall|i: int| #![trigger m.contains_key(i)] #![trigger m1.contains_key(i)] !(0 <= i < n) implies m.contains_key(i) == m1.contains_key(i) && (m1.contains_key(i) ==> m[i] == m1[i]) by {}
    }
}

} // verus!
