//! The permission model for user memory (`verus/TODO.md`, "Permissions and
//! trusted boundaries"): a [`UserMem`] is the permission to a live block's
//! usable bytes, with the bytes' contents as verified code knows them. A
//! block's [`Live`](crate::heap::live::Live) token holds it from the
//! allocation that hands the block out to the free that takes it back;
//! the fills and copies verified code makes on user memory take it
//! ([`zero_user`], [`copy_user`]) and record their effect in it.
//!
//! Reading a user block's bytes takes a capability too,
//! [`may_read_user`] (whole-allocator theorem 46 in `verus/THEOREMS.md`):
//! only realloc's copy and the `check_zero` debug reads have it, so no
//! other allocator decision depends on what the program wrote.

use crate::vspec::zero::Zeroed;
use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_only)]
use vstd::raw_ptr::Provenance;

verus! {

/// The caller may read the bytes of a user block (theorem 46 in
/// `verus/THEOREMS.md`). The read primitives over a [`UserMem`]
/// ([`UserMem::lemma_valid_reads`] and [`copy_user`], and through them
/// `class::verify::read_word` and `Huge::read_run_word`) require it, and a
/// `UserMem` is the only permission verified code can hold to a block's
/// bytes. So a verified function can read a user block only if its own
/// `requires` (or its loop's invariant) carries the fact, it calls
/// [`grant_user_reads`], or it holds a `true` that `purge::check_zero()`
/// returned (whose `ensures` grants it).
///
/// Opaque, with no meaning: a capability, not a claim about the world.
#[verifier::opaque]
pub open spec fn may_read_user() -> bool {
    true
}

/// Grant [`may_read_user`]. Called only by the realloc cores
/// (`libc::realloc_block`, `libc::realloc_usable_stored` and
/// `jemalloc::rallocx_zeroed`, for their copy of the old block into the
/// new one) and by `purge::check_zero`, whose `true` (the opt-in debug
/// mode is on) grants it to the `check_zero` reads.
/// `api::tests::user_reads_only_at_grants` checks the source for that.
pub proof fn grant_user_reads()
    ensures
        may_read_user(),
{
    reveal(may_read_user);
}

/// Ghost permission to user memory: the bytes `dom()` of provenance
/// `provenance()` (a vstd `PointsToRaw`, so two permissions held at once
/// share no byte, [`UserMem::lemma_apart`]), and `bytes()`, the value a
/// load of each of them reads.
///
/// The permission makes its bytes valid for reads and writes
/// ([`UserMem::lemma_valid`], from `vspec::ptr::axiom_raw_valid`; a
/// borrowed one only for reads, [`UserMem::lemma_valid_reads`], which
/// requires [`may_read_user`]). The byte
/// state is a ghost record, not memory: it is true because every write
/// verified code makes to user memory takes the permission and updates it
/// ([`zero_user`], [`copy_user`]), and the program, which writes the bytes
/// as it likes, holds the permission (in the block's token) while it does.
/// Where a permission is made its bytes are unknown (any values): at the
/// hand-out of a block (`heap::live`'s mint, inside `Live::from_slot` and
/// `Live::from_run`) and at the C boundary (`heap::live::axiom_c_claim`).
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
        &&& self.wf()
        &&& forall|i: int| #[trigger] self.dom().contains(i) <==> a <= i < a + len
    }

    /// The `n` bytes from `a` read as zero.
    pub open spec fn reads_zero(self, a: int, n: int) -> bool {
        forall|i: int| a <= i < a + n ==> #[trigger] self.bytes()[i] == 0
    }

    /// The `n` bytes from `a` read as the `n` bytes of `src` from `b`.
    pub open spec fn reads_as(self, a: int, src: UserMem, b: int, n: int) -> bool {
        forall|x: int| a <= x < a + n ==> #[trigger] self.bytes()[x] == src.bytes()[b + (x - a)]
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

    /// No bytes: what a token of no block holds.
    pub proof fn empty(prov: Provenance) -> (tracked m: UserMem)
        ensures
            m.dom() == Set::<int>::empty(),
            m.provenance() == prov,
            m.wf(),
    {
        UserMem { raw: PointsToRaw::empty(prov), bytes: Map::empty() }
    }

    /// The permission makes the `len` bytes from `p` (of its provenance)
    /// valid for reads and writes (`vspec::ptr::axiom_raw_valid`). Writes
    /// take the permission itself: a borrowed one only reads
    /// ([`UserMem::lemma_valid_reads`]). Unchanged. Private: a write bound
    /// is a read bound too (`valid_writes` implies `valid_reads`), so its
    /// callers are this module's [`zero_user`], which only writes, and
    /// [`copy_user`], which requires [`may_read_user`].
    proof fn lemma_valid(tracked &mut self, p: *mut u8, len: int)
        requires
            0 <= len,
            old(self).provenance() == p@.provenance,
            forall|i: int| p@.addr <= i < p@.addr + len ==> #[trigger] old(self).dom().contains(i),
        ensures
            *final(self) == *old(self),
            crate::vspec::ptr::valid_writes(p, len),
            crate::vspec::ptr::valid_reads(p as *const u8, len),
    {
        assert(self.raw.contains_range(p@.addr as int, len)) by {
            assert forall|i: int| #[trigger] Set::range(p@.addr as int, p@.addr + len).contains(i) implies self.dom().contains(i) by {
                broadcast use vstd::set_lib::range_set_properties;
            }
        }
        crate::vspec::ptr::axiom_raw_valid(&self.raw, p, len);
    }

    /// The permission makes the `len` bytes from `p` (of its provenance)
    /// valid for reads, inside the allocation `p` is derived from
    /// (`vspec::ptr::axiom_raw_valid`). The one read of user memory open
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
            crate::vspec::ptr::in_alloc(p, len),
    {
        assert(self.raw.contains_range(p@.addr as int, len)) by {
            assert forall|i: int| #[trigger] Set::range(p@.addr as int, p@.addr + len).contains(i) implies self.dom().contains(i) by {
                broadcast use vstd::set_lib::range_set_properties;
            }
        }
        crate::vspec::ptr::axiom_raw_valid(&self.raw, p, len);
    }

    /// Two permissions held at once own no byte in common
    /// (`vspec::ptr::lemma_raw_disjoint`). Both are unchanged.
    pub proof fn lemma_apart(tracked &mut self, tracked other: &mut UserMem)
        requires
            forall|x: int| #[trigger] old(self).dom().contains(x) ==> 0 <= x <= usize::MAX,
        ensures
            *final(self) == *old(self),
            *final(other) == *old(other),
            final(self).dom().disjoint(final(other).dom()),
    {
        let ghost d = self.dom();
        let tracked mut a = PointsToRaw::empty(self.raw.provenance());
        let tracked mut b = PointsToRaw::empty(other.raw.provenance());
        vstd::modes::tracked_swap(&mut self.raw, &mut a);
        vstd::modes::tracked_swap(&mut other.raw, &mut b);
        assert forall|x: int| #[trigger] a.dom().contains(x) implies 0 <= x <= usize::MAX by {
            assert(d.contains(x));
        }
        let tracked (a, b) = crate::vspec::ptr::lemma_raw_disjoint(a, b);
        self.raw = a;
        other.raw = b;
    }
}

/// The bytes a [`Zeroed`] token covers read as zero, so a permission's
/// byte state for them is zero. Trusted: the token's meaning (its doc:
/// made only where the bytes are known to be zero, and true while no one
/// writes them), with a premise Verus does not check: the token is fresh,
/// made for these bytes after their last write. A stale token (one kept
/// across a write to its bytes, as `CachePerms::handed` may be) would set
/// a false byte state; the token is borrowed, not used up, because the
/// callers return it too. Callers, each with the token the allocation or
/// the fill it just made returned: the known-zero hand-outs of
/// `libc::calloc_zeroed`, `libc::calloc_usable_zeroed` and
/// `jemalloc::alloc_with`, and [`zero_user`].
#[verifier::external_body]
pub proof fn axiom_zeroed_bytes(tracked z: &Zeroed, tracked m: &mut UserMem)
    requires
        forall|i: int| #[trigger] z.has(i) ==> old(m).dom().contains(i),
    ensures
        final(m).frame(*old(m), z.addr(), z.len()),
        final(m).reads_zero(z.addr(), z.len()),
{
}

/// The permission to bytes of challoc's memory, of any provenance, as the
/// permission to them through an integer-to-pointer cast
/// ([`addr_to_ptr`](crate::vspec::addr::addr_to_ptr), of
/// [`cast_provenance`](crate::vspec::addr::cast_provenance)), the bytes
/// and their values unchanged. Trusted: challoc's memory is mapped with
/// `mmap`, whose addresses come back from the kernel as integers, so every
/// pointer into it is derived from a cast and its provenance is exposed;
/// a cast of an address in it picks up that provenance (Rust's exposed
/// provenance), so the cast pointer is valid wherever the first one was.
/// The permission is changed in place, so the bytes keep one. Its premise
/// is that the bytes are challoc's: every `UserMem` is (the only sources
/// are `heap::live`'s hand-out and the C contract, both for blocks of the
/// class and huge regions). Caller: `Shared::zero_huge`, which clears a
/// run through the cast of its page's address.
#[verifier::external_body]
pub proof fn axiom_cast(tracked m: &mut UserMem)
    ensures
        final(m).dom() == old(m).dom(),
        final(m).wf() == old(m).wf(),
        final(m).bytes() == old(m).bytes(),
        final(m).provenance() == crate::vspec::addr::cast_provenance(),
{
}

/// Clear the `n` bytes at `p` with the permission `m` to them
/// (`write_bytes(p, 0, n)` through `vspec::zero::write_zero`), returning
/// the token for what it wrote; `m`'s byte state reads them as zero after.
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
    proof {
        broadcast use vstd::layout::align_of_u8;
        m.lemma_valid(p, n as int);
    }
    // SAFETY: the permission makes the `n` bytes at `p` writable.
    let z = unsafe { crate::vspec::zero::write_zero(p, n) };
    proof {
        axiom_zeroed_bytes(z.borrow(), m);
    }
    z
}

/// The effect on the byte state of [`copy_user`]'s
/// `copy_nonoverlapping(src, dst, n)`, made with the permissions `s` to
/// the `n` bytes from `src` and `d` to the `n` bytes from `dst`, which
/// share no byte: `d` reads the `n` bytes from `dst` as `s` reads the `n`
/// bytes from `src`, and its other bytes, its domain, its provenance and
/// its well-formedness are kept. Trusted: the `core` specification
/// (`vspec::addr`) states the copy's safety contract, not its effect (the
/// Kani harness `ptr_copy_nonoverlapping` checks that effect on memory).
/// Private, so its only caller is `copy_user`, right after the copy and
/// with the permissions and addresses it was made with; a call with no
/// copy would set a false byte state, not prove `false`.
axiom fn axiom_copied_bytes(tracked s: &UserMem, tracked d: &mut UserMem, src: int, dst: int, n: int)
    requires
        0 <= n,
        forall|i: int| src <= i < src + n ==> #[trigger] s.dom().contains(i),
        forall|i: int| dst <= i < dst + n ==> #[trigger] old(d).dom().contains(i),
        s.dom().disjoint(old(d).dom()),
    ensures
        final(d).frame(*old(d), dst, n),
        final(d).reads_as(dst, *s, src, n),
;

/// `copy_nonoverlapping(src, dst, n)` with the permissions to both ranges:
/// `s` to the `n` bytes at `src`, `d` to the `n` bytes at `dst`, which
/// share no byte with them (two permissions held at once never do:
/// [`UserMem::lemma_apart`]). Afterwards `d`'s byte state reads the `n`
/// bytes at `dst` as `s`'s at `src` ([`axiom_copied_bytes`]). It reads
/// user memory, so it needs [`may_read_user`].
#[inline(always)]
pub unsafe fn copy_user(src: *mut u8, dst: *mut u8, n: usize, Tracked(s): Tracked<&UserMem>, Tracked(d): Tracked<&mut UserMem>)
    requires
        may_read_user(),
        s.provenance() == src@.provenance,
        old(d).provenance() == dst@.provenance,
        forall|i: int| src@.addr <= i < src@.addr + n ==> #[trigger] s.dom().contains(i),
        forall|i: int| dst@.addr <= i < dst@.addr + n ==> #[trigger] old(d).dom().contains(i),
        s.dom().disjoint(old(d).dom()),
    ensures
        final(d).frame(*old(d), dst@.addr as int, n as int),
        final(d).reads_as(dst@.addr as int, *s, src@.addr as int, n as int),
{
    proof {
        broadcast use vstd::layout::align_of_u8;
        s.lemma_valid_reads(src, n as int);
        d.lemma_valid(dst, n as int);
        if n > 0 && !(src@.addr + n <= dst@.addr || dst@.addr + n <= src@.addr) {
            // The later of the two first bytes lies in both ranges, so in
            // both permissions, which share no byte.
            let x = if src@.addr <= dst@.addr { dst@.addr as int } else { src@.addr as int };
            assert(s.dom().contains(x) && d.dom().contains(x));
        }
    }
    // SAFETY: the permissions make `src` readable and `dst` writable for
    // `n` bytes, and two permissions share no byte.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, n) };
    proof {
        axiom_copied_bytes(s, d, src@.addr as int, dst@.addr as int, n as int);
    }
}

} // verus!
