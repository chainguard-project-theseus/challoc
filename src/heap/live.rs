//! The live-block ghost model for huge runs: a [`Live`] token holds a run's
//! [`RunToken`] together with the permission to the run's usable bytes
//! ([`UserMem`]). `Huge::check_zero` is its one user: it makes the token
//! of the run `take` just handed it ([`Live::from_run`]), reads the run's
//! words through the permission, and takes the run token back out
//! ([`Live::into_run`]), dropping the permission.

#[cfg(verus_keep_ghost)]
use crate::huge::GUARD_PAGES;
use crate::huge::RunToken;
#[cfg(verus_keep_ghost)]
use crate::layout::process_regions;
use crate::vspec::user::UserMem;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// Ghost token of a live huge run: the run at `addr`, its [`RunToken`]
/// (`run`), and the permission to its usable bytes (`mem`, [`UserMem`]).
/// Made by [`Live::from_run`] and taken apart by [`Live::into_run`], which
/// drops the permission as it gives the run token out.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct Live {
    ghost addr: usize,
    tracked run: RunToken,
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
    /// The run's address.
    pub closed spec fn addr(self) -> usize {
        self.addr
    }

    /// The run token the token holds.
    pub closed spec fn run(self) -> RunToken {
        self.run
    }

    /// The run's usable bytes: the pages before the guard.
    pub open spec fn len(self) -> int {
        run_usable(self.run().npages())
    }

    /// The permission the token holds.
    pub closed spec fn mem(self) -> UserMem {
        self.mem
    }

    /// The provenance of the pointers the run's permission is valid
    /// through.
    pub open spec fn prov(self) -> Provenance {
        self.mem().provenance()
    }

    /// A live run: its run token's, at `addr`, with its usable bytes in
    /// the huge region and the permission to exactly them.
    pub closed spec fn wf(self) -> bool {
        &&& self.run.wf()
        &&& page_addr(self.run.start()) == self.addr as int
        &&& process_regions().is_huge_block(self.addr, self.len())
        &&& self.mem.covers(self.addr as int, self.len())
    }

    /// The token of the live run at `p`, whose permission is valid through
    /// `p`.
    pub open spec fn block(self, p: *mut u8) -> bool {
        &&& self.wf()
        &&& self.addr() == p@.addr
        &&& self.prov() == p@.provenance
    }

    /// A live run's token holds the permission to its usable bytes.
    pub proof fn lemma_mem(&self)
        requires
            self.wf(),
        ensures
            self.mem().covers(self.addr() as int, self.len()),
    {
    }

    /// Borrow the permission, for a read of the run's bytes.
    pub proof fn mem_ref(tracked &self) -> (tracked r: &UserMem)
        ensures
            *r == self.mem(),
    {
        &self.mem
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
            r.run() == t,
    {
        let tracked m = axiom_hand_out(p, run_usable(t.npages()));
        Live { addr: p@.addr, run: t, mem: m }
    }

    /// Take the run token out. The permission to the run's bytes is used
    /// up with it.
    pub proof fn into_run(tracked self) -> (tracked t: RunToken)
        ensures
            t == self.run(),
    {
        let tracked Live { addr, run, mem } = self;
        run
    }
}

/// The permission to the `len` usable bytes of the huge run at `p`, valid
/// through `p`. Trusted, for what the huge allocator does not track: the
/// run's pages are committed by `Huge::prepare`, before `Huge::take`, and
/// released only by its free, and no other permission covers them (no
/// `PointsToRaw` is made for the huge region but here; the metadata
/// region's permissions lie apart from it).
///
/// At most one permission per run at a time, by the discipline of this
/// module's API, which Verus checks at every caller: the axiom is private,
/// and its one caller, [`Live::from_run`], uses up a bare run token into
/// the token it makes with the permission. A permission exists only inside
/// the [`Live`] that holds its run token, and the run token comes out bare
/// only through [`Live::into_run`], which drops the permission. There is
/// one run token per run (the huge allocator's authorities), so no second
/// permission can be minted for bytes whose first is still held.
axiom fn axiom_hand_out(p: *mut u8, len: int) -> (tracked m: UserMem)
    requires
        crate::heap::regions_ok(),
        process_regions().is_huge_block(p@.addr, len),
    ensures
        m.covers(p@.addr as int, len),
        m.provenance() == p@.provenance,
;

} // verus!
