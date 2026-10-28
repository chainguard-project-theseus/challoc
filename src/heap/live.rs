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
use crate::layout::{class_table, process_regions};
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
    tracked mem: UserMem,
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
        self.mem.covers(self.addr as int, self.len())
    }

    /// The permission the token holds.
    pub closed spec fn mem(self) -> UserMem {
        self.mem
    }

    /// The provenance of the pointers the block's permission is valid
    /// through.
    pub open spec fn prov(self) -> Provenance {
        self.mem().provenance()
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
        ensures
            *r == self.mem(),
    {
        &self.mem
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
            r.slot_block(p, c),
            r.slot_tok() == t,
            r.len() == class_table()[c].size,
    {
        let len = class_table()[c].size as int;
        assert(process_regions().is_slot(c, p@.addr) && len == class_table()[c].size);
        let tracked m = axiom_hand_out(p, len);
        Live { addr: p@.addr, class: c, slot: Some(t), run: None, mem: m }
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
        let tracked m = axiom_hand_out(p, run_usable(t.npages()));
        Live { addr: p@.addr, class: 0, slot: None, run: Some(t), mem: m }
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

    /// The run token the token holds, if any.
    pub closed spec fn run_opt(self) -> Option<RunToken> {
        self.run
    }

    /// Take the run token out. The permission to the run's bytes is used
    /// up with it, as in [`Live::take_slot`].
    pub proof fn into_run(tracked self) -> (tracked t: RunToken)
        requires
            self.run_opt() is Some,
        ensures
            Some(t) == self.run_opt(),
    {
        let tracked Live { addr, class, slot, run, mem } = self;
        run.tracked_unwrap()
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

} // verus!
