//! The live-block ghost model (`verus/THEOREMS.md`, "Interface contract",
//! gap 1): a [`Live`] token stands for one block the program holds, from
//! the allocation that hands it out to the free that takes it back.
//!
//! A slab slot's token is the slot's uniqueness token (`verus/TODO.md`,
//! "Uniqueness: slot tokens"), the one the bitmap word, a claim or a cache
//! held before; a huge run's is its [`RunToken`]. The allocation paths
//! return the token of the block they hand out ([`Live::block`]); the free
//! paths take a token that claims the pointer they are given
//! ([`Live::claims`]) and give its slot or run token back to the allocator.
//! Two tokens are of different blocks, and their bytes are disjoint
//! ([`Live::theorem_apart`]).
//!
//! C callers carry no ghost state. The Rust entry points that the C ABI
//! exports call (`api/`) hand the token of an allocation to C (the program
//! holds the block, so it holds the token), and take one back for a
//! pointer C frees through the one trusted step, [`axiom_c_claim`]: the C
//! contract of `free` and `realloc`, that only a block's holder frees it.
//! The step uses up the token of the C call ([`crate::vspec::cbuf::CCall`]
//! of a free or a realloc), which only the `extern "C"` bodies mint, one
//! per call, for the pointer C passed.

#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::huge::GUARD_PAGES;
use crate::huge::RunToken;
#[cfg(verus_only)]
use crate::huge::free_page_byte;
#[cfg(verus_only)]
use crate::layout::{NCLASSES, class_table, disjoint, process_regions};
#[cfg(verus_only)]
use crate::vspec::cbuf::CCall;
use crate::vspec::user::UserMem;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{int, verus};
#[cfg(verus_only)]
use vstd::raw_ptr::Provenance;
use vstd::resource::set::GhostSingleton;

verus! {

/// Ghost token of a live block: the block at `addr`, a slot of class
/// `class` holding its slot token (`slot`), or a huge run holding its
/// [`RunToken`] (`run`), with the permission to its usable bytes (`mem`,
/// [`UserMem`]). Made by the allocation paths ([`Live::from_slot`],
/// [`Live::from_run`]) and, for a pointer C passes to a free, by
/// [`axiom_c_claim`]; taken apart by the free paths
/// ([`Live::take_slot`], [`Live::into_run`]), which drop the permission
/// as they give the slot or run token out; [`Live::take_mem`] and
/// [`Live::put_mem`] lend the permission to a fill or a copy.
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

    /// The token holds the run token of a run of `npages` pages (guard
    /// included) that starts at page `s`.
    pub closed spec fn run_at(self, s: int, npages: int) -> bool {
        &&& self.run_of()
        &&& self.run.unwrap().start() == s
        &&& self.run.unwrap().npages() == npages
    }

    /// The token holds the permission to exactly the block's usable bytes.
    pub closed spec fn mem_ok(self) -> bool {
        &&& self.mem matches Some(m) && m.covers(self.addr as int, self.len())
    }

    /// The permission the token holds (meaningful under [`Live::mem_ok`]).
    pub closed spec fn mem(self) -> UserMem {
        self.mem.unwrap()
    }

    /// The permission, if any.
    pub closed spec fn mem_opt(self) -> Option<UserMem> {
        self.mem
    }

    /// The provenance of the pointers the block's permission is valid
    /// through: the block's pointer's, as the allocation returned it or C
    /// passed it back.
    pub open spec fn prov(self) -> Provenance {
        self.mem().provenance()
    }

    /// What a load of each usable byte of the block reads (the byte state
    /// of its permission, [`UserMem::bytes`]).
    pub open spec fn bytes(self) -> Map<int, u8> {
        self.mem().bytes()
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

    /// Borrow the permission, for a read of the block's bytes (the source
    /// of a copy).
    pub proof fn mem_ref(tracked &self) -> (tracked r: &UserMem)
        requires
            self.mem_ok(),
        ensures
            *r == self.mem(),
    {
        self.mem.tracked_borrow()
    }

    /// The block's first `n` usable bytes read as `src`'s first `n`
    /// (`realloc`'s copy into a new block).
    pub open spec fn copied_from(self, src: Live, n: int) -> bool {
        self.mem().reads_as(self.addr() as int, src.mem(), src.addr() as int, n)
    }

    /// The block's `n` usable bytes from offset `from` read as zero: a load
    /// of each, through the block's pointer, reads 0 (whole-allocator
    /// theorem 6 as a byte state, `verus/THEOREMS.md`).
    pub open spec fn reads_zero(self, from: int, n: int) -> bool {
        self.mem().reads_zero(self.addr() + from, n)
    }

    /// A live block, as the allocation paths hand it out: a slot of class
    /// `class` at `addr` with its slot token, or a run at `addr` with its
    /// run token and its usable bytes in the huge region; with the
    /// permission to its usable bytes.
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

    /// A slot's token covers the class's size.
    pub proof fn lemma_slot_len(&self, p: *mut u8, c: int)
        requires
            self.slot_block(p, c),
        ensures
            self.len() == class_table()[c].size,
            process_regions().is_slot(c, p@.addr),
    {
    }

    /// A live block has usable bytes: a class's size, or a run's pages.
    pub proof fn lemma_len_pos(&self)
        requires
            self.wf(),
        ensures
            self.len() > 0,
    {
        if self.run is None {
            crate::vspec::class::axiom_class_table_wf(self.class);
        }
    }

    /// The token a free of `p` takes: if `p` is a slot start, the token of
    /// the slot, of a class `p` is a slot of; else, if `p` is a page start
    /// of the huge region, the token of the run that starts there, its
    /// usable bytes in the region. Nothing for any other pointer, which the
    /// free paths reject at run time (whole-allocator theorem 3). Each case
    /// asks for one token, so some token claims any pointer whatever the
    /// regions are; with the process's regions well formed and separated
    /// ([`regions_ok`](crate::heap::regions_ok)) a slot start has one class
    /// and is no huge page ([`Live::lemma_claims_slot`]).
    pub closed spec fn claims(self, p: *mut u8) -> bool {
        &&& self.addr == p@.addr
        &&& crate::heap::slot_start(p) ==> {
            &&& process_regions().is_slot(self.class, p@.addr)
            &&& self.slot_of(self.class)
            &&& self.run is None
            &&& self.mem_ok()
            &&& self.prov() == p@.provenance
        }
        &&& !crate::heap::slot_start(p) && process_regions().is_huge_block(p@.addr, 1) ==> {
            &&& self.run_of()
            &&& self.slot is None
            &&& process_regions().is_huge_block(p@.addr, self.len())
            &&& self.mem_ok()
            &&& self.prov() == p@.provenance
        }
    }

    /// The token of nothing: what a null result comes with.
    pub proof fn empty() -> (tracked r: Live) {
        Live { addr: 0, class: 0, slot: None, run: None, mem: None }
    }

    /// Replace the token with `v` (an out-parameter's result).
    pub proof fn set(tracked &mut self, tracked v: Live)
        ensures
            *final(self) == v,
    {
        *self = v;
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
        lemma_slot_end(c, p@.addr);
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
            r.run_at(t.start(), t.npages()),
            r.run_opt() == Some(t),
            r.len() == run_usable(t.npages()),
    {
        let len = run_usable(t.npages());
        lemma_huge_end(p@.addr, len);
        let tracked m = axiom_hand_out(p, len);
        Live { addr: p@.addr, class: 0, slot: None, run: Some(t), mem: Some(m) }
    }

    /// The token holds a slot token.
    pub closed spec fn has_slot(self) -> bool {
        self.slot is Some
    }

    /// `self` without its slot token and the permission, which leave
    /// together.
    pub closed spec fn without_slot(self) -> Live {
        Live { slot: None, mem: None, ..self }
    }

    /// The slot token `self` holds.
    pub closed spec fn slot_tok(self) -> GhostSingleton<int> {
        self.slot.unwrap()
    }

    /// Take the slot token out, for a free of the slot. The permission to
    /// the slot's bytes is used up with it (dropped): a slot token is
    /// never apart from a permission that is still held, so
    /// [`axiom_hand_out`] never mints a second one ([`Live::from_slot`]
    /// and [`Live::put_slot`] take a bare token). Writes the free makes
    /// to the slot come first, through [`Live::take_mem`].
    pub proof fn take_slot(tracked &mut self, c: int) -> (tracked t: GhostSingleton<int>)
        requires
            old(self).slot_of(c),
            old(self).mem_ok(),
        ensures
            t == old(self).slot_tok(),
            t.id() == slot_loc(c),
            t@ == old(self).addr() as int,
            *final(self) == old(self).without_slot(),
    {
        let tracked mut m = None;
        vstd::modes::tracked_swap(&mut self.mem, &mut m);
        self.slot.tracked_take()
    }

    /// A token that claims `p`, a slot start of class `c`, is the live
    /// slot's.
    pub proof fn lemma_claimed_slot(&self, p: *mut u8, c: int)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            process_regions().is_slot(c, p@.addr),
        ensures
            self.slot_block(p, c),
            self.has_slot(),
    {
        self.lemma_claims_slot(p, c);
    }

    /// Put back a slot token [`Live::take_slot`] took, with a fresh
    /// permission to the slot's bytes ([`axiom_hand_out`]; the one
    /// `take_slot` dropped is gone, and the token was bare), their values
    /// unknown: a free that declined (a full cache) leaves the token as
    /// it was but for its byte state.
    pub proof fn put_slot(tracked &mut self, tracked t: GhostSingleton<int>, orig: Live, p: *mut u8, c: int)
        requires
            crate::heap::regions_ok(),
            *old(self) == orig.without_slot(),
            t == orig.slot_tok(),
            orig.slot_block(p, c),
        ensures
            final(self).slot_block(p, c),
            final(self).addr() == orig.addr(),
            final(self).len() == orig.len(),
            forall|q: *mut u8| #[trigger] orig.claims(q) ==> final(self).claims(q),
    {
        let len = class_table()[c].size as int;
        lemma_slot_end(c, p@.addr);
        let tracked m = axiom_hand_out(p, len);
        self.slot = Some(t);
        self.mem = Some(m);
        assert forall|q: *mut u8| #[trigger] orig.claims(q) implies self.claims(q) by {
            if crate::heap::slot_start(q) {
                assert(orig.slot_of(orig.class));
            }
        }
    }

    /// Take the permission out, for a fill or a copy that takes it
    /// ([`Live::put_mem`] puts it back). The token keeps its slot or run
    /// token, which [`Live::take_slot`] and [`Live::into_run`] give out
    /// only with the permission back in place.
    pub proof fn take_mem(tracked &mut self) -> (tracked m: UserMem)
        requires
            old(self).mem_ok(),
        ensures
            m == old(self).mem(),
            *final(self) == old(self).without_mem(),
    {
        self.mem.tracked_take()
    }

    /// `self` without the permission.
    pub closed spec fn without_mem(self) -> Live {
        Live { mem: None, ..self }
    }

    /// Put back a permission [`Live::take_mem`] took, its bytes as they
    /// were (only their values, and for `zero_huge`'s cast the
    /// provenance, may differ): the token is as it was but for its byte
    /// state.
    pub proof fn put_mem(tracked &mut self, tracked m: UserMem, orig: Live)
        requires
            *old(self) == orig.without_mem(),
            orig.mem_ok(),
            m.dom() == orig.mem().dom(),
            m.wf() == orig.mem().wf(),
        ensures
            final(self).mem() == m,
            final(self).mem_ok(),
            final(self).addr() == orig.addr(),
            final(self).len() == orig.len(),
            final(self).wf() == orig.wf(),
            final(self).run_opt() == orig.run_opt(),
            forall|c: int| #[trigger] orig.slot_of(c) ==> final(self).slot_of(c),
            m.provenance() == orig.mem().provenance() ==> final(self).same_but_bytes(orig),
    {
        self.mem = Some(m);
    }

    /// `self` is `orig` but for the values of its usable bytes: the same
    /// block, tokens, permission bytes and provenance.
    pub open spec fn same_but_bytes(self, orig: Live) -> bool {
        &&& self.addr() == orig.addr()
        &&& self.len() == orig.len()
        &&& self.wf() == orig.wf()
        &&& self.prov() == orig.prov()
        &&& self.mem().dom() == orig.mem().dom()
        &&& forall|p: *mut u8| #[trigger] orig.claims(p) ==> self.claims(p)
        &&& forall|p: *mut u8| #[trigger] orig.block(p) ==> self.block(p)
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

    /// A token that claims `p`, a page start of the huge region with page
    /// index `page`, is the live run's at `page`.
    pub proof fn lemma_run_block(&self, p: *mut u8, page: usize)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            process_regions().is_huge_block(p@.addr, 1),
            page == ((p@.addr - process_regions().huge_base) as usize) >> process_regions().lg_page,
        ensures
            self.run_block(page as int),
            self.block(p),
    {
        self.lemma_run_page(p, page);
        self.lemma_claimed_block(p);
    }

    /// Take the run token out, for a free of the run at page `page`. The
    /// permission to the run's bytes is used up with it, as in
    /// [`Live::take_slot`]; the free's clear comes first, through
    /// [`Live::take_mem`].
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

    /// Borrow the run token, if any, for a read of a run's usable size.
    pub proof fn run_ref(tracked &self) -> (tracked r: &Option<RunToken>)
        ensures
            *r == self.run_opt(),
    {
        &self.run
    }

    /// A token that claims `p`, a page start of the huge region, holds the
    /// token of the run at `p`'s page `page`, whose usable bytes are the
    /// block's.
    pub proof fn lemma_run_page(&self, p: *mut u8, page: usize)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            process_regions().is_huge_block(p@.addr, 1),
            page == ((p@.addr - process_regions().huge_base) as usize) >> process_regions().lg_page,
        ensures
            self.run_opt() is Some,
            self.run_opt().unwrap().wf(),
            self.run_opt().unwrap().start() == page,
            self.len() == run_usable(self.run_opt().unwrap().npages()),
    {
        lemma_not_slot(p);
        lemma_page_of(self.run.unwrap(), p, page);
    }

    /// A live block's token claims the block's pointer: a slot start of
    /// its class and no page of the huge region, or a huge page start and
    /// no slot.
    pub proof fn lemma_claims(&self, p: *mut u8)
        requires
            crate::heap::regions_ok(),
            self.block(p),
        ensures
            self.claims(p),
    {
        if self.run is None {
            assert(process_regions().is_slot(self.class, p@.addr));
        } else {
            assert(process_regions().is_huge_block(p@.addr, 1));
            lemma_not_slot(p);
        }
    }

    /// The slot class of the block a token claims, for a pointer the free
    /// paths classified as a slot of class `c`: a slot start has one class
    /// (`zero::lemma_slot_unique`).
    pub proof fn lemma_claims_slot(&self, p: *mut u8, c: int)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            process_regions().is_slot(c, p@.addr),
        ensures
            self.slot_of(c),
            self.slot_tok().id() == slot_loc(c),
            self.slot_tok()@ == p@.addr as int,
            self.has_slot(),
    {
        crate::vspec::zero::lemma_slot_unique(self.class, c, p@.addr);
    }

    /// The run a token claims, for a pointer the free paths classified as
    /// a page start of the huge region: no slot start is one
    /// (`zero::lemma_slot_not_huge`).
    pub proof fn lemma_claims_run(&self, p: *mut u8)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            process_regions().is_huge_block(p@.addr, 1),
        ensures
            self.run_of(),
    {
        lemma_not_slot(p);
    }

    /// A token that claims a slot start or a huge page start is the
    /// token of the live block there.
    pub proof fn lemma_claimed_block(&self, p: *mut u8)
        requires
            crate::heap::regions_ok(),
            self.claims(p),
            crate::heap::free_target(p),
        ensures
            self.block(p),
    {
        if !crate::heap::slot_start(p) {
            lemma_not_slot(p);
        }
    }

    /// The token of the live block `p` with at least `size` usable
    /// bytes, or nothing (`p` null): what an allocation of `size` bytes
    /// returns with its result.
    pub open spec fn gives(self, p: *mut u8, size: int) -> bool {
        p@.addr != 0 ==> {
            &&& self.block(p)
            &&& size <= self.len()
        }
    }

    /// A live slot's token and any other token of its class's authority
    /// (a cached slot's, a bitmap word's, a claim's) are of different
    /// slots: the live block is held nowhere else.
    pub proof fn lemma_slot_apart(tracked &mut self, tracked t: &GhostSingleton<int>, c: int)
        requires
            old(self).slot_of(c),
            t.id() == slot_loc(c),
        ensures
            t@ != old(self).addr() as int,
            final(self).slot_of(c),
            final(self).addr() == old(self).addr(),
            final(self).len() == old(self).len(),
            final(self).mem() == old(self).mem(),
            final(self).wf() == old(self).wf(),
            forall|p: *mut u8| #[trigger] old(self).claims(p) ==> final(self).claims(p),
    {
        let ghost o = *self;
        let tracked mut s = self.slot.tracked_take();
        s.disjoint(t);
        self.slot = Some(s);
        assert forall|p: *mut u8| #[trigger] o.claims(p) implies self.claims(p) by {
            if crate::heap::slot_start(p) {
                assert(o.slot_of(o.class));
            }
        }
    }

    /// Live blocks are disjoint (the D column of `verus/THEOREMS.md`,
    /// "Interface contract"; theorem 5 for slots): the usable
    /// bytes of two live blocks (two tokens, wherever they are held)
    /// share no byte. Two slots of one class are two slot tokens of one
    /// authority, so different slots, a class size or more apart; slots of
    /// two classes lie in two class regions; a slot and a run lie in the
    /// class superblock and the huge region; two runs hold disjoint pages
    /// ([`RunToken::lemma_apart`]).
    pub proof fn theorem_apart(tracked &mut self, tracked other: &Live)
        requires
            crate::heap::regions_ok(),
            old(self).wf(),
            other.wf(),
        ensures
            final(self).wf(),
            final(self).addr() == old(self).addr(),
            final(self).len() == old(self).len(),
            final(self).mem() == old(self).mem(),
            forall|p: *mut u8| #[trigger] old(self).claims(p) ==> final(self).claims(p),
            disjoint(final(self).addr() as int, final(self).len(), other.addr() as int, other.len()),
    {
        let r = process_regions();
        let ghost o = *self;
        if self.run is None && other.run is None {
            let c1 = self.class;
            let c2 = other.class;
            if c1 == c2 {
                let tracked mut t = self.slot.tracked_take();
                t.disjoint(other.slot.tracked_borrow());
                self.slot = Some(t);
                lemma_slots_apart(c1, self.addr, other.addr);
            } else {
                lemma_classes_apart(c1, self.addr, c2, other.addr);
            }
        } else if self.run is Some && other.run is Some {
            let tracked mut t = self.run.tracked_take();
            t.lemma_apart(other.run.tracked_borrow());
            t.lemma_wf();
            other.run.unwrap().lemma_wf();
            self.run = Some(t);
            lemma_runs_apart(t.start(), t.npages(), other.run.unwrap().start(), other.run.unwrap().npages());
        } else if self.run is None {
            lemma_slot_run_apart(self.class, self.addr, other.addr, other.len());
        } else {
            lemma_slot_run_apart(other.class, other.addr, self.addr, self.len());
        }
        assert forall|p: *mut u8| #[trigger] o.claims(p) implies self.claims(p) by {
            if crate::heap::slot_start(p) {
                assert(o.slot_of(o.class));
            }
        }
    }

    /// No live block at address 0 (`verus/THEOREMS.md`, "No block at
    /// address 0"): a live token's block is a slot start or a huge page
    /// start ([`Regions::lemma_block_nonzero`]), with at least one usable
    /// byte (a class's size, or a run's pages before its guard).
    ///
    /// [`Regions::lemma_block_nonzero`]: crate::layout::Regions::lemma_block_nonzero
    pub proof fn lemma_nonnull(&self)
        requires
            crate::heap::regions_ok(),
            self.wf(),
        ensures
            self.addr() != 0,
            0 < self.len(),
    {
        process_regions().lemma_block_nonzero(self.class, self.addr, self.len(), 0);
        if self.run is None {
            crate::vspec::class::axiom_class_table_wf(self.class);
        }
    }

    /// Two live blocks are at two addresses ([`Live::theorem_apart`] with
    /// [`Live::lemma_nonnull`]: their usable bytes are disjoint and each
    /// has at least one). So `malloc(0)` returns, when it does not fail,
    /// a pointer no other live block has: a block of its own, as the
    /// module doc of `api::libc` promises.
    pub proof fn theorem_distinct(tracked &mut self, tracked other: &Live)
        requires
            crate::heap::regions_ok(),
            old(self).wf(),
            other.wf(),
        ensures
            final(self).wf(),
            final(self).addr() == old(self).addr(),
            final(self).len() == old(self).len(),
            final(self).mem() == old(self).mem(),
            forall|p: *mut u8| #[trigger] old(self).claims(p) ==> final(self).claims(p),
            final(self).addr() != 0,
            final(self).addr() != other.addr(),
    {
        self.theorem_apart(other);
        self.lemma_nonnull();
        other.lemma_nonnull();
    }

    /// Purge never discards live data, for huge purges (whole-allocator
    /// theorem "Purge never discards live data", `verus/THEOREMS.md`): a
    /// byte a huge purge may discard, one of a page of the huge region the
    /// page authority `auth` does not hold ([`free_page_byte`], in the
    /// process's terms, which are the huge allocator's under `Shared::wf`),
    /// is no usable byte of any live block, wherever its token is held. A
    /// run's pages are in the authority ([`RunToken::theorem_purge_apart`]);
    /// a slot lies in the class superblock, apart from the huge region.
    pub proof fn theorem_purge_huge(tracked &self, tracked auth: &vstd::resource::set::GhostSetAuth<int>, b: int)
        requires
            crate::heap::regions_ok(),
            self.wf(),
            auth.id() == crate::huge::page_loc(),
            free_page_byte(
                process_regions().huge_base as int,
                page_size(),
                (process_regions().huge_len >> process_regions().lg_page) as int,
                auth@,
                b,
            ),
        ensures
            !(self.addr() <= b < self.addr() + self.len()),
    {
        let r = process_regions();
        let base = r.huge_base as int;
        let pg = page_size();
        let l8 = r.lg_page;
        let lg = l8 as usize;
        vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
        vstd::bits::lemma_usize_shl_is_mul(1, lg);
        vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
        assert((1usize << l8) == (1usize << lg)) by (bit_vector)
            requires
                lg == l8 as usize,
        ;
        assert(pg > 0);
        if self.run is Some {
            let tracked t = self.run.tracked_borrow();
            t.lemma_wf();
            t.theorem_purge_apart(auth, base, pg, (r.huge_len >> r.lg_page) as int, b);
            let (s, n) = (t.start(), t.npages());
            assert(s * pg + (n - GUARD_PAGES) * pg <= (s + n) * pg) by (nonlinear_arith)
                requires
                    GUARD_PAGES == 1,
                    pg > 0,
            ;
        } else {
            // The slot lies in its class's stride of the superblock; `b`
            // in the huge region, which `separated` keeps apart.
            let c = self.class;
            let stride = (1usize << r.lg_stride) as int;
            r.lemma_slot_in_region(c, self.addr);
            crate::vspec::class::axiom_class_table_wf(c);
            assert(r.superblock + c * stride <= r.class_base[c]);
            assert(r.class_base[c] + (1usize << r.lg_region) <= r.superblock + (c + 1) * stride);
            assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
                requires
                    c < NCLASSES,
                    stride >= 0,
            ;
            assert(0 <= c * stride) by (nonlinear_arith)
                requires
                    c >= 0,
                    stride >= 0,
            ;
            // `b` is in one of the region's pages, below its end.
            let np = (r.huge_len >> r.lg_page) as int;
            let k = (b - base) / pg;
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b - base, pg);
            vstd::arithmetic::div_mod::lemma_mod_bound(b - base, pg);
            let len = r.huge_len;
            let l32 = r.lg_page;
            vstd::bits::lemma_usize_shr_is_div(len, lg);
            assert(len >> l32 == len >> lg) by (bit_vector)
                requires
                    lg == l32 as usize,
            ;
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(len as int, pg);
            vstd::arithmetic::div_mod::lemma_mod_bound(len as int, pg);
            assert(k * pg + pg <= np * pg) by (nonlinear_arith)
                requires
                    k + 1 <= np,
                    pg > 0,
            ;
            assert(pg * k == k * pg && pg * np == np * pg) by (nonlinear_arith);
            assert(b < base + len);
        }
    }

    /// An interior pointer of a live block is no free target
    /// (`verus/THEOREMS.md`, "Bad input"): a pointer `k` bytes into the
    /// block, `0 < k < len()`, is no slot start and no huge page start, so
    /// a free of it aborts (theorem 3), unless the block is a huge run and
    /// `k` a whole number of pages. That page start is caught at run time
    /// (`Huge::free_run` finds no run starting there), but the proof
    /// excludes it only by the C contract ([`axiom_c_claim`]).
    pub proof fn theorem_interior(&self, p: *mut u8, k: int)
        requires
            crate::heap::regions_ok(),
            self.wf(),
            0 < k < self.len(),
            p@.addr == self.addr() + k,
        ensures
            crate::heap::free_target(p) ==> self.run_of() && k % page_size() == 0,
    {
        if self.run is None {
            lemma_slot_interior(self.class, self.addr, p@.addr);
        } else {
            lemma_run_interior(self.addr, self.len(), p@.addr);
        }
    }
}

/// A pointer strictly inside slot `a` of class `c` is no slot start and no
/// huge page start.
pub proof fn lemma_slot_interior(c: int, a: usize, b: usize)
    requires
        crate::heap::regions_ok(),
        process_regions().is_slot(c, a),
        a < b < a + class_table()[c].size,
    ensures
        forall|d: int| !#[trigger] process_regions().is_slot(d, b),
        !process_regions().is_huge_block(b, 1),
{
    let r = process_regions();
    crate::vspec::class::axiom_class_table_wf(c);
    assert forall|d: int| !#[trigger] r.is_slot(d, b) by {
        if r.is_slot(d, b) {
            crate::vspec::class::axiom_class_table_wf(d);
            if d == c {
                lemma_slots_apart(c, a, b);
            } else {
                lemma_classes_apart(c, a, d, b);
            }
        }
    }
    if r.is_huge_block(b, 1) {
        lemma_slot_run_apart(c, a, b, 1);
    }
}

/// A pointer inside the `len` bytes of the huge region at page start `a`
/// that is not a whole number of pages past `a` is no slot start and no
/// huge page start.
pub proof fn lemma_run_interior(a: usize, len: int, b: usize)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(a, len),
        a < b < a + len,
    ensures
        forall|d: int| !#[trigger] process_regions().is_slot(d, b),
        process_regions().is_huge_block(b, 1) ==> (b - a) % page_size() == 0,
{
    let r = process_regions();
    assert forall|d: int| !#[trigger] r.is_slot(d, b) by {
        if r.is_slot(d, b) {
            crate::vspec::class::axiom_class_table_wf(d);
            lemma_slot_run_apart(d, b, a, len);
        }
    }
    if r.is_huge_block(b, 1) {
        let lgp = r.lg_page;
        assert((1usize << lgp) > 0) by (bit_vector)
            requires
                lgp < 64,
        ;
        let pg = page_size();
        let x = b - r.huge_base;
        let y = a - r.huge_base;
        assert(x % pg == 0 && y % pg == 0);
        vstd::arithmetic::div_mod::lemma_sub_mod_noop(x, y, pg);
        vstd::arithmetic::div_mod::lemma_small_mod(0, pg as nat);
        assert((x - y) % pg == 0);
        assert(b - a == x - y);
    }
}

/// The C contract of `free`, `realloc` and the sized frees: only a
/// block's holder frees or reallocates it, and the holder of the block at
/// `p` holds its token, which the allocation that made it handed to C
/// with the block. So for a pointer `p` C passes to a free (`c`, the
/// token of that call, a free or a realloc of `p`: `c.block() ==
/// Some(p)`, [`CEntry::Frees`](crate::vspec::cbuf::CEntry::Frees)), C
/// holds a token that claims it ([`Live::claims`]): the slot token if `p`
/// is a slot start, the token of the run that starts at `p` if `p` is a
/// huge page start, each with the permission to the block's usable bytes,
/// valid through `p`, their values whatever the program left in them;
/// nothing otherwise (whole-allocator theorem 3 then aborts).
///
/// Trusted, at the C ABI boundary only: the Rust entry point that the
/// export of each free or realloc symbol calls (`libc::free`,
/// `free_sized_hint`, `realloc`, `realloc_usable`, `free_usable`,
/// `jemalloc::rallocx`, `dallocx`, `sdallocx`; the C++ `delete`s go
/// through the `libc` ones) takes the export's call token for the pointer
/// C passed, uses it up here, and passes the block's token to the
/// verified core (as does, once per branch, the verification-only
/// `libc::theorem_no_syscall_on_fast_paths`, with its parameter's).
/// Verified code has no other way to call it: the only
/// [`CCall`](crate::vspec::cbuf::CCall) of a free or a realloc it holds
/// is the one passed down from an export, minted in the `extern "C"` body
/// for the export's pointer (`axiom_c_block`, private to each `exports`
/// module of `api/libc.rs`, `api/jemalloc.rs` and `api/cxx.rs`), and
/// using that up leaves none.
/// `api::tests::c_blocks_only_for_the_entrys_pointer` checks the source
/// for at most one mint per body, for the body's own pointer parameter.
/// It needs no well-formed regions: some token claims any pointer. The
/// axiom is only as sound as the contract: a pointer freed twice, or one
/// never allocated that happens to be a slot or huge page start, has no
/// token in C's hands, and two tokens of one slot or overlapping runs let
/// a proof derive `false`. The run-time checks behind the frees (the
/// bitmap bit test, the huge table lookup, the cache flush's release)
/// stay in the code for such programs. The contract also keeps the call's
/// other pointer arguments out of the block (`challoc_realloc_usable`'s
/// `usable`, whose permission `api::libc`'s `axiom_c_out` gives): its
/// bytes are the call's, and a write through such an argument would
/// change them behind the permission this token holds.
pub axiom fn axiom_c_claim(tracked c: CCall) -> (tracked r: Live)
    requires
        c.block() is Some,
    ensures
        r.claims(c.block()->0),
;

/// The permission to the `len` usable bytes of the block at `p` that the
/// allocator is handing out (or, for the `check_zero` reads, a run
/// `Huge::take` has just taken or a slot a claim has just taken, before
/// its hand-out: `Huge::check_zero` and `ClassState::check_zero` give the
/// token back out through `Live::into_run` or `Live::take_slot`, which
/// drop the permission), valid through `p`, the bytes' values unknown.
/// Trusted, for what the allocation paths do not track: the block's bytes
/// are committed (a slot's slab is committed before its first slot is
/// claimed and decommitted only when it holds no allocated slot; a run's
/// pages are committed by `Huge::prepare`, before `Huge::take`, and released
/// only by its free),
/// and nothing else holds a permission to them (no `PointsToRaw` is ever
/// made for the class or huge regions but here, in [`axiom_c_claim`] and
/// in `class::axiom_fresh_slab_mem`, which lends one, for reads, to the
/// slots of a `Fresh` slab, none of whose slot tokens is out of the
/// bitmap, for as long as its descriptor permission is borrowed, which
/// its one caller does under the class lock; the metadata region's
/// permissions lie apart from them; and the
/// permissions `vspec::cbuf` gives for a C caller's arguments are to
/// memory the program holds, never a block being handed out).
///
/// Once-only, by the discipline of the module's API, which Verus checks
/// at every caller (the axiom is private, so its callers are this
/// module's [`Live::from_slot`], [`Live::from_run`] and
/// [`Live::put_slot`], each of which uses up a bare slot or run token
/// into the token it makes with the permission). A permission exists only
/// inside the [`Live`] that holds its block's slot or run token, or taken
/// out of it by [`Live::take_mem`]; the token comes out bare only through
/// [`Live::take_slot`] and [`Live::into_run`], which require the
/// permission back in place and drop it. So while a permission to a
/// block's bytes is held, its slot or run token is not bare, and there is
/// one token per block (the class's and the huge allocator's authorities),
/// so no second permission can be minted for bytes whose first is still
/// held.
axiom fn axiom_hand_out(p: *mut u8, len: int) -> (tracked m: UserMem)
    requires
        0 < p@.addr,
        0 < len,
        p@.addr + len <= usize::MAX,
    ensures
        m.covers(p@.addr as int, len),
        m.provenance() == p@.provenance,
;

/// A slot lies inside the address space, above 0.
proof fn lemma_slot_end(c: int, a: usize)
    requires
        crate::heap::regions_ok(),
        process_regions().is_slot(c, a),
    ensures
        0 < a,
        0 < class_table()[c].size,
        a + class_table()[c].size <= usize::MAX,
{
    let r = process_regions();
    r.lemma_block_nonzero(c, a, 0, 0);
    r.lemma_slot_in_region(c, a);
    crate::vspec::class::axiom_class_table_wf(c);
    assert(r.class_base[c] + (1usize << r.lg_region) <= usize::MAX);
}

/// `len` bytes of the huge region from a page start lie inside the address
/// space, above 0.
proof fn lemma_huge_end(a: usize, len: int)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(a, len),
    ensures
        0 < a,
        a + len <= usize::MAX,
{
    process_regions().lemma_block_nonzero(0, a, len, 0);
}

impl Live {
    /// A live block's usable bytes lie inside the address space, above 0,
    /// and so does every byte of its permission.
    pub proof fn lemma_end(&self)
        requires
            crate::heap::regions_ok(),
            self.wf(),
        ensures
            0 < self.addr(),
            0 < self.len(),
            self.addr() + self.len() <= usize::MAX,
            forall|x: int| #[trigger] self.mem().dom().contains(x) ==> 0 <= x <= usize::MAX,
    {
        if self.run is None {
            lemma_slot_end(self.class, self.addr);
        } else {
            lemma_huge_end(self.addr, self.len());
        }
    }

    /// The permissions of two live blocks share no byte (they are two
    /// `PointsToRaw`s held at once: [`UserMem::lemma_apart`]); both tokens
    /// are unchanged.
    pub proof fn lemma_mem_apart(tracked &mut self, tracked other: &mut Live)
        requires
            crate::heap::regions_ok(),
            old(self).wf(),
            old(other).mem_ok(),
        ensures
            *final(self) == *old(self),
            *final(other) == *old(other),
            final(self).mem().dom().disjoint(final(other).mem().dom()),
    {
        self.lemma_end();
        let tracked mut ma = self.mem.tracked_take();
        let tracked mut mb = other.mem.tracked_take();
        ma.lemma_apart(&mut mb);
        self.mem = Some(ma);
        other.mem = Some(mb);
    }
}

/// A page start of the huge region is no slot start: the class regions
/// are apart from the huge region (`zero::lemma_slot_not_huge`).
proof fn lemma_not_slot(p: *mut u8)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(p@.addr, 1),
    ensures
        !crate::heap::slot_start(p),
{
    assert forall|c: int| !#[trigger] process_regions().is_slot(c, p@.addr) by {
        if process_regions().is_slot(c, p@.addr) {
            crate::vspec::zero::lemma_slot_not_huge(c, p@.addr);
        }
    }
}

/// The run token of the run at `p`, a page start of the huge region,
/// starts at `p`'s page index `page`.
proof fn lemma_page_of(t: RunToken, p: *mut u8, page: usize)
    requires
        process_regions().wf(),
        t.wf(),
        page_addr(t.start()) == p@.addr as int,
        process_regions().is_huge_block(p@.addr, 1),
        page == ((p@.addr - process_regions().huge_base) as usize) >> process_regions().lg_page,
    ensures
        t.start() == page,
{
    let r = process_regions();
    let lgp = r.lg_page;
    let lg = lgp as usize;
    let d = (p@.addr - r.huge_base) as usize;
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    vstd::bits::lemma_usize_shr_is_div(d, lg);
    assert(d >> lgp == d >> lg) by (bit_vector)
        requires
            lg == lgp as usize,
    ;
    let pg = page_size();
    assert(pg == vstd::arithmetic::power2::pow2(lg as nat));
    vstd::arithmetic::power2::lemma_pow2_pos(lg as nat);
    t.lemma_wf();
    let s = t.start();
    assert(d as int == s * pg);
    assert(s * pg == pg * s) by (nonlinear_arith);
    vstd::arithmetic::div_mod::lemma_div_multiples_vanish(s, pg);
    assert(page as int == (d as nat / vstd::arithmetic::power2::pow2(lg as nat)) as int);
}

/// Two different slot starts of one class are a class size or more apart.
proof fn lemma_slots_apart(c: int, a: usize, b: usize)
    requires
        process_regions().wf(),
        process_regions().is_slot(c, a),
        process_regions().is_slot(c, b),
        a != b,
    ensures
        disjoint(a as int, class_table()[c].size as int, b as int, class_table()[c].size as int),
{
    let r = process_regions();
    crate::vspec::class::axiom_class_table_wf(c);
    let cls = class_table()[c];
    let (sa, ia) = choose|slab: int, slot: int|
        0 <= slab < r.spec_nslabs(c) && 0 <= slot < cls.nslots && a as int == #[trigger] r.slot_at(c, slab, slot);
    let (sb, ib) = choose|slab: int, slot: int|
        0 <= slab < r.spec_nslabs(c) && 0 <= slot < cls.nslots && b as int == #[trigger] r.slot_at(c, slab, slot);
    let s = (1usize << cls.lg_slab) as int;
    let z = cls.size as int;
    assert(ia * z + z <= s) by (nonlinear_arith)
        requires
            0 <= ia < cls.nslots,
            (cls.nslots as int) * z <= s,
            z >= 0,
    ;
    assert(ib * z + z <= s) by (nonlinear_arith)
        requires
            0 <= ib < cls.nslots,
            (cls.nslots as int) * z <= s,
            z >= 0,
    ;
    if sa == sb {
        assert(ia != ib);
        if ia < ib {
            assert(ia * z + z <= ib * z) by (nonlinear_arith)
                requires
                    ia < ib,
                    z >= 0,
            ;
        } else {
            assert(ib * z + z <= ia * z) by (nonlinear_arith)
                requires
                    ib < ia,
                    z >= 0,
            ;
        }
    } else if sa < sb {
        assert(sa * s + s <= sb * s) by (nonlinear_arith)
            requires
                sa < sb,
                s >= 0,
        ;
        assert(0 <= ib * z) by (nonlinear_arith)
            requires
                0 <= ib,
                z >= 0,
        ;
    } else {
        assert(sb * s + s <= sa * s) by (nonlinear_arith)
            requires
                sb < sa,
                s >= 0,
        ;
        assert(0 <= ia * z) by (nonlinear_arith)
            requires
                0 <= ia,
                z >= 0,
        ;
    }
}

/// Slots of two different classes lie in two class regions, inside two
/// strides of the superblock.
proof fn lemma_classes_apart(c1: int, a: usize, c2: int, b: usize)
    requires
        process_regions().wf(),
        process_regions().separated(),
        process_regions().is_slot(c1, a),
        process_regions().is_slot(c2, b),
        c1 != c2,
    ensures
        disjoint(a as int, class_table()[c1].size as int, b as int, class_table()[c2].size as int),
{
    let r = process_regions();
    let stride = (1usize << r.lg_stride) as int;
    r.lemma_slot_in_region(c1, a);
    r.lemma_slot_in_region(c2, b);
    assert(r.superblock + c1 * stride <= r.class_base[c1]);
    assert(r.class_base[c1] + (1usize << r.lg_region) <= r.superblock + (c1 + 1) * stride);
    assert(r.superblock + c2 * stride <= r.class_base[c2]);
    assert(r.class_base[c2] + (1usize << r.lg_region) <= r.superblock + (c2 + 1) * stride);
    if c1 < c2 {
        assert((c1 + 1) * stride <= c2 * stride) by (nonlinear_arith)
            requires
                c1 + 1 <= c2,
                stride >= 0,
        ;
    } else {
        assert((c2 + 1) * stride <= c1 * stride) by (nonlinear_arith)
            requires
                c2 + 1 <= c1,
                stride >= 0,
        ;
    }
}

/// A slot lies in the class superblock and `len` bytes of the huge region
/// at `b` in the huge region, which `separated` keeps apart.
proof fn lemma_slot_run_apart(c: int, a: usize, b: usize, len: int)
    requires
        process_regions().wf(),
        process_regions().separated(),
        process_regions().is_slot(c, a),
        process_regions().is_huge_block(b, len),
    ensures
        disjoint(a as int, class_table()[c].size as int, b as int, len),
{
    let r = process_regions();
    let stride = (1usize << r.lg_stride) as int;
    r.lemma_slot_in_region(c, a);
    crate::vspec::class::axiom_class_table_wf(c);
    assert(r.superblock + c * stride <= r.class_base[c]);
    assert(r.class_base[c] + (1usize << r.lg_region) <= r.superblock + (c + 1) * stride);
    assert((c + 1) * stride <= NCLASSES * stride) by (nonlinear_arith)
        requires
            c < NCLASSES,
            stride >= 0,
    ;
    assert(0 <= c * stride) by (nonlinear_arith)
        requires
            c >= 0,
            stride >= 0,
    ;
}

/// Runs on disjoint page ranges have disjoint usable bytes.
proof fn lemma_runs_apart(s1: int, n1: int, s2: int, n2: int)
    requires
        s1 + n1 <= s2 || s2 + n2 <= s1,
        GUARD_PAGES < n1,
        GUARD_PAGES < n2,
    ensures
        disjoint(page_addr(s1), run_usable(n1), page_addr(s2), run_usable(n2)),
{
    let pg = page_size();
    assert(pg >= 0);
    if s1 + n1 <= s2 {
        assert(s1 * pg + (n1 - GUARD_PAGES) * pg <= s2 * pg) by (nonlinear_arith)
            requires
                s1 + n1 <= s2,
                pg >= 0,
                GUARD_PAGES == 1,
        ;
    } else {
        assert(s2 * pg + (n2 - GUARD_PAGES) * pg <= s1 * pg) by (nonlinear_arith)
            requires
                s2 + n2 <= s1,
                pg >= 0,
                GUARD_PAGES == 1,
        ;
    }
}

// Exact stats (`verus/THEOREMS.md`, "Exact stats"): which block a token
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
