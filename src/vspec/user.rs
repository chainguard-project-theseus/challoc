//! The permission model for user memory (`verus/TODO.md`, "Permissions and
//! trusted boundaries"): a [`UserMem`] is the permission to a live block's
//! usable bytes, with the bytes' contents as verified code knows them. A
//! block's [`Live`](crate::heap::live::Live) token holds it from the
//! allocation that hands the block out to the free that takes it back;
//! the fills and copies verified code makes on user memory take it
//! ([`zero_user`], [`copy_user`]) and record their effect in it.

use crate::vspec::zero::Zeroed;
use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_only)]
use vstd::raw_ptr::Provenance;

verus! {

/// Ghost permission to user memory: the bytes `dom()` of provenance
/// `provenance()` (a vstd `PointsToRaw`, so two permissions held at once
/// share no byte, [`UserMem::lemma_apart`]), and `bytes()`, the value a
/// load of each of them reads.
///
/// The permission makes its bytes valid for reads and writes
/// ([`UserMem::lemma_valid`], from `vspec::ptr::axiom_raw_valid`). The byte
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
    /// valid for reads and writes (`vspec::ptr::axiom_raw_valid`).
    pub proof fn lemma_valid(tracked &self, p: *mut u8, len: int)
        requires
            0 <= len,
            self.provenance() == p@.provenance,
            forall|i: int| p@.addr <= i < p@.addr + len ==> #[trigger] self.dom().contains(i),
        ensures
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

/// `copy_nonoverlapping(src, dst, n)` with the permissions to both ranges:
/// `s` to the `n` bytes at `src`, `d` to the `n` bytes at `dst`, which
/// share no byte with them (two permissions held at once never do:
/// [`UserMem::lemma_apart`]). Afterwards `d`'s byte state reads the `n`
/// bytes at `dst` as `s`'s at `src`. Trusted for that effect, which the
/// `core` specification (`vspec::addr`) does not state; the safety
/// contract is `core`'s, from the permissions.
#[verifier::external_body]
#[inline(always)]
pub unsafe fn copy_user(src: *mut u8, dst: *mut u8, n: usize, Tracked(s): Tracked<&UserMem>, Tracked(d): Tracked<&mut UserMem>)
    requires
        s.provenance() == src@.provenance,
        old(d).provenance() == dst@.provenance,
        forall|i: int| src@.addr <= i < src@.addr + n ==> #[trigger] s.dom().contains(i),
        forall|i: int| dst@.addr <= i < dst@.addr + n ==> #[trigger] old(d).dom().contains(i),
        s.dom().disjoint(old(d).dom()),
    ensures
        final(d).frame(*old(d), dst@.addr as int, n as int),
        final(d).reads_as(dst@.addr as int, *s, src@.addr as int, n as int),
{
    // SAFETY: the permissions make `src` readable and `dst` writable for
    // `n` bytes, and two permissions share no byte.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, n) }
}

} // verus!
