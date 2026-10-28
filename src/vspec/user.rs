//! The permission model for user memory: a [`UserMem`] is the permission
//! to a block's usable bytes (a slab slot or a huge run), which a
//! [`Live`](crate::heap::live::Live) token holds while
//! `ClassState::check_zero` or `Huge::check_zero` reads the block, or to
//! a `Fresh` slab's slots, lent under the class lock for
//! `ClassState::check_fresh_slab_is_zero` (`class::axiom_fresh_slab_mem`).
//!
//! Reading a user block's bytes takes a capability too,
//! [`may_read_user`], which `purge::check_zero` grants to the opt-in debug
//! reads.

use crate::vspec::zero::Zeroed;
use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// The caller may read the bytes of a user block. The read primitive
/// over a [`UserMem`] ([`UserMem::lemma_valid_reads`], and through it
/// the class `read_word` and `Huge::read_run_word`) requires it, and a
/// `UserMem` is the only permission verified code can hold to a block's
/// bytes. So a verified function can read a user block only if its own
/// `requires` (or its loop's invariant) carries the fact, or it calls
/// [`grant_user_reads`], directly or through a `true` from
/// `purge::check_zero()`.
///
/// Opaque, with no meaning: a capability, not a claim about the world. It
/// marks the reads for audit; Verus does not restrict who calls
/// [`grant_user_reads`] (its one caller is `purge::check_zero`).
#[verifier::opaque]
pub closed spec fn may_read_user() -> bool {
    true
}

/// Grant [`may_read_user`]. Called only by `purge::check_zero`, whose
/// `true` (the opt-in debug mode is on) grants it to the `check_zero`
/// reads.
pub proof fn grant_user_reads()
    ensures
        may_read_user(),
{
    reveal(may_read_user);
}

/// Ghost permission to user memory: the bytes `dom()` of provenance
/// `provenance()` (a vstd `PointsToRaw`), and `bytes()`, the value a load
/// of each of them reads. A borrowed permission makes its bytes valid
/// for reads ([`UserMem::lemma_valid_reads`], which requires
/// [`may_read_user`]).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct UserMem {
    tracked raw: PointsToRaw,
    ghost bytes: Map<int, u8>,
}

impl UserMem {
    /// The bytes the permission covers.
    pub closed spec fn dom(self) -> Set<int> {
        self.raw.dom()
    }

    /// The provenance of the pointers the permission is valid through.
    pub closed spec fn provenance(self) -> Provenance {
        self.raw.provenance()
    }

    /// What a load of each byte of `dom()` reads.
    pub closed spec fn bytes(self) -> Map<int, u8> {
        self.bytes
    }

    /// The byte state has a value for each byte the permission covers.
    pub closed spec fn wf(self) -> bool {
        self.bytes.dom() == self.raw.dom()
    }

    /// The permission is exactly the `len` bytes from `a`.
    pub open spec fn covers(self, a: int, len: int) -> bool {
        forall|i: int| #[trigger] self.dom().contains(i) <==> a <= i < a + len
    }

    /// The `n` bytes from `a` read as zero.
    pub open spec fn reads_zero(self, a: int, n: int) -> bool {
        forall|i: int| a <= i < a + n ==> #[trigger] self.bytes()[i] == 0
    }

    /// `self` is `old` with at most the `n` bytes from `a` changed: the
    /// same bytes, provenance and well-formedness, the other bytes' values
    /// kept.
    pub open spec fn frame(self, old: UserMem, a: int, n: int) -> bool {
        &&& self.dom() == old.dom()
        &&& self.provenance() == old.provenance()
        &&& self.wf() == old.wf()
        &&& forall|i: int| !(a <= i < a + n) ==> #[trigger] self.bytes()[i] == old.bytes()[i]
    }

    /// The permission makes the `len` bytes from `p` (of its provenance)
    /// valid for reads and, when there are some, inside the allocation `p`
    /// is derived from (`vspec::ptr::axiom_raw_valid`). The one read of user memory open
    /// to verified code outside this module, so it needs
    /// [`may_read_user`].
    pub proof fn lemma_valid_reads(tracked &self, p: *mut u8, len: int)
        requires
            may_read_user(),
            0 <= len,
            self.provenance() == p@.provenance,
            forall|i: int| p@.addr <= i < p@.addr + len ==> #[trigger] self.dom().contains(i),
        ensures
            crate::vspec::ptr::valid_reads(p as *const u8, len),
            len > 0 ==> crate::vspec::ptr::in_alloc(p, len),
    {
        assert(self.raw.contains_range(p@.addr as int, len)) by {
            assert forall|i: int| #[trigger] Set::range(p@.addr as int, p@.addr + len).contains(i) implies self.dom().contains(i) by {
                broadcast use vstd::set_lib::range_set_properties;
            }
        }
        crate::vspec::ptr::axiom_raw_valid(&self.raw, p, len);
        crate::vspec::ptr::lemma_writes_read(p, len);
    }
}

/// Clear the `n` bytes at `p` with the permission `m` to them
/// (`write_bytes(p, 0, n)`), returning the token for what it wrote; `m`'s
/// byte state reads them as zero after.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn zero_user(p: *mut u8, n: usize, Tracked(m): Tracked<&mut UserMem>) -> (z: Tracked<Zeroed>)
    requires
        old(m).provenance() == p@.provenance,
        forall|i: int| p@.addr <= i < p@.addr + n ==> #[trigger] old(m).dom().contains(i),
    ensures
        z@.addr() == p@.addr,
        z@.len() == n,
        final(m).frame(*old(m), p@.addr as int, n as int),
        final(m).reads_zero(p@.addr as int, n as int),
{
    // SAFETY: the permission makes the `n` bytes at `p` writable.
    unsafe { core::ptr::write_bytes(p, 0, n) };
    Tracked::assume_new()
}

} // verus!
