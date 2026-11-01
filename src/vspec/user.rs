//! The permission model for user memory: a [`UserMem`] is the permission
//! to a block's usable bytes (a slab slot or a huge run), with the bytes'
//! contents as verified code knows them, which a
//! [`Live`](crate::heap::live::Live) token holds: from `Shared::alloc_direct`
//! or `Shared::alloc_huge`, which hand the block out, lent to
//! `Shared::zero_huge`'s clear, and while `ClassState::check_zero` or
//! `Huge::check_zero` reads the block. Or to a `Fresh` slab's slots, lent
//! under the class lock for `ClassState::check_fresh_slab_is_zero`
//! (`class::axiom_fresh_slab_mem`). `realloc` and `rallocx` hold two at
//! once, the old block's and the new one's, for the copy ([`copy_user`]).
//!
//! Reading a user block's bytes takes a capability too,
//! [`may_read_user`], which `purge::check_zero` grants to the opt-in debug
//! reads and the realloc cores to their copy of a moved block
//! ([`copy_user`], which requires it).

use crate::vspec::zero::Zeroed;
use vstd::prelude::*;
use vstd::raw_ptr::PointsToRaw;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::Provenance;

verus! {

/// The caller may read the bytes of a user block. The read primitive
/// over a [`UserMem`] ([`UserMem::lemma_valid_reads`], and through it
/// the class `read_word`, `Huge::read_run_word` and [`copy_user`])
/// requires it, and a `UserMem` is the only permission verified code can
/// hold to a block's bytes. So a verified function can read a user block
/// only if its own `requires` (or its loop's invariant) carries the fact,
/// or it calls [`grant_user_reads`], directly or through a `true` from
/// `purge::check_zero()`.
///
/// Opaque, with no meaning: a capability, not a claim about the world. It
/// marks the reads for audit; Verus does not restrict who calls
/// [`grant_user_reads`] (its callers are `purge::check_zero` and the
/// realloc cores, `api::libc::realloc_block`,
/// `api::libc::realloc_usable_stored` and `api::jemalloc::rallocx_zeroed`).
/// `sys::tests::user_read_grants_only_in_listed_callers` checks the source
/// for that.
#[verifier::opaque]
pub closed spec fn may_read_user() -> bool {
    true
}

/// Grant [`may_read_user`]. Called by `purge::check_zero`, whose `true`
/// (the opt-in debug mode is on) grants it to the `check_zero` reads, and
/// by the realloc cores (`api::libc::realloc_block`,
/// `api::libc::realloc_usable_stored`, `api::jemalloc::rallocx_zeroed`)
/// for the copy of a moved block, and by nothing else:
/// `sys::tests::user_read_grants_only_in_listed_callers` checks the source
/// for that.
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

    /// The `len` bytes from `p` (of its provenance) are in the raw
    /// permission, what [`zero_raw`](crate::vspec::zero::zero_raw) takes
    /// `&mut` to fill them, or [`copy_raw`] to copy into them. Private: its
    /// callers are this module's [`zero_user`] and [`copy_user`].
    proof fn lemma_raw_range(tracked &self, p: *mut u8, len: int)
        requires
            0 <= len,
            self.provenance() == p@.provenance,
            forall|i: int| p@.addr <= i < p@.addr + len ==> #[trigger] self.dom().contains(i),
        ensures
            self.raw.contains_range(p@.addr as int, len),
            self.raw.provenance() == p@.provenance,
    {
        assert(self.raw.contains_range(p@.addr as int, len)) by {
            assert forall|i: int| #[trigger] Set::range(p@.addr as int, p@.addr + len).contains(i) implies self.dom().contains(i) by {
                broadcast use vstd::set_lib::range_set_properties;
            }
        }
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
/// the fill it just made returned: the known-zero hand-out of
/// `libc::calloc_zeroed` and `libc::calloc_usable_zeroed` (`heap::alloc_zeroed`'s token, or the huge path's
/// from `Heap::alloc_huge` or `Shared::alloc_huge`, each made by the
/// allocation before any write to the block), the known-zero hand-out of
/// `api::jemalloc::alloc_with` (`heap::mallocx::alloc`'s or
/// `alloc_huge`'s token, made by the allocation before any write), and
/// [`zero_user`].
#[verifier::external_body]
pub proof fn axiom_zeroed_bytes(tracked z: &Zeroed, tracked m: &mut UserMem)
    requires
        forall|i: int| z.addr() <= i < z.addr() + z.len() ==> #[trigger] old(m).dom().contains(i),
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
/// is that the bytes are challoc's: every `UserMem` is (its sources are
/// `heap::live`'s hand-out, for blocks of the class and huge regions, and
/// `class::axiom_fresh_slab_mem`'s loan of a fresh slab's bytes). Caller:
/// `Shared::zero_huge`, which clears a run through the cast of its page's
/// address.
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
/// (`write_bytes(p, 0, n)` through `vspec::zero::zero_raw`, with the
/// permission's raw bytes taken `&mut`), returning
/// the token for what it wrote; `m`'s byte state reads them as zero after.
/// `p` is non-null, as `write_bytes` requires even of an empty fill.
#[inline(always)]
pub unsafe fn zero_user(p: *mut u8, n: usize, Tracked(m): Tracked<&mut UserMem>) -> (z: Tracked<Zeroed>)
    requires
        p@.addr != 0,
        old(m).provenance() == p@.provenance,
        forall|i: int| p@.addr <= i < p@.addr + n ==> #[trigger] old(m).dom().contains(i),
    ensures
        z@.addr() == p@.addr,
        z@.len() == n,
        final(m).frame(*old(m), p@.addr as int, n as int),
        final(m).reads_zero(p@.addr as int, n as int),
{
    proof {
        m.lemma_raw_range(p, n as int);
    }
    // SAFETY: the permission makes the `n` bytes at `p` writable.
    let z = unsafe { crate::vspec::zero::zero_raw(p, n, Tracked(&mut m.raw)) };
    proof {
        axiom_zeroed_bytes(z.borrow(), m);
    }
    z
}

/// `copy_nonoverlapping(src, dst, n)` for `u8` with the permissions to
/// both ranges, and its effect on `d`'s byte state: `d` reads the `n`
/// bytes from `dst` as `s` reads the `n` bytes from `src`, and its other
/// bytes, its domain, its provenance and its well-formedness are kept.
/// The copy writes `dst`'s bytes through `d`, taken `&mut` so that no
/// other view of them is held across it (as for
/// [`zero_raw`](crate::vspec::zero::zero_raw)). Trusted: the `core`
/// contract of `copy_nonoverlapping` (`src` valid for reads of `n` bytes,
/// `dst` valid for writes of them, which `d`'s raw permission stands for,
/// as for `vspec::ptr::axiom_raw_valid`, and the two ranges apart), and
/// its effect, which nothing in vstd states. The apartness premise
/// follows from the others and is kept as `core` states it. `s` and `d`
/// share no byte, so the copy leaves `s`'s bytes as they were. Private:
/// its caller is [`copy_user`].
#[verifier::external_body]
#[inline(always)]
unsafe fn copy_raw(src: *const u8, dst: *mut u8, n: usize, Tracked(s): Tracked<&UserMem>, Tracked(d): Tracked<&mut UserMem>)
    requires
        crate::vspec::ptr::valid_reads(src, n as int),
        old(d).raw.contains_range(dst@.addr as int, n as int),
        old(d).raw.provenance() == dst@.provenance,
        n == 0 || src@.addr + n <= dst@.addr || dst@.addr + n <= src@.addr,
        forall|i: int| src@.addr <= i < src@.addr + n ==> #[trigger] s.dom().contains(i),
        s.dom().disjoint(old(d).dom()),
    ensures
        final(d).frame(*old(d), dst@.addr as int, n as int),
        final(d).reads_as(dst@.addr as int, *s, src@.addr as int, n as int),
{
    // SAFETY: the caller's contract: `src` readable and `dst` writable
    // for `n` bytes, the two apart.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, n) };
}

/// `copy_nonoverlapping(src, dst, n)` with the permissions to both ranges:
/// `s` to the `n` bytes at `src`, `d` to the `n` bytes at `dst`, which
/// share no byte with them (two permissions held at once never do:
/// [`UserMem::lemma_apart`]). Afterwards `d`'s byte state reads the `n`
/// bytes at `dst` as `s`'s at `src` (through [`copy_raw`], with `d` taken
/// `&mut`). It reads user memory, so it needs [`may_read_user`].
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
        s.lemma_valid_reads(src, n as int);
        d.lemma_raw_range(dst, n as int);
        if n > 0 && !(src@.addr + n <= dst@.addr || dst@.addr + n <= src@.addr) {
            // The later of the two first bytes lies in both ranges, so in
            // both permissions, which share no byte.
            let x = if src@.addr <= dst@.addr { dst@.addr as int } else { src@.addr as int };
            assert(s.dom().contains(x) && d.dom().contains(x));
        }
    }
    // SAFETY: the permissions make `src` readable and `dst` writable for
    // `n` bytes, and two permissions share no byte.
    unsafe { copy_raw(src as *const u8, dst, n, Tracked(s), Tracked(d)) };
}

} // verus!

/// Kani check of `copy_raw` (`verus/ledger/src/vspec/user.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_copy {
    use super::copy_raw;
    use vstd::prelude::Tracked;

    /// `copy_raw` between two 8-byte buffers (`requires`: here, the `n`
    /// bytes from offset `i` of one and the `n` bytes from offset `j` of
    /// the other, every `n` that fits and, for each, any `i` and `j` that
    /// fit, any contents; the permissions are ghost, so the harness
    /// passes the erased ones): it returns, and afterwards each of the `n`
    /// bytes from `j` reads as the source byte at the same distance from
    /// `i` (`reads_as`), every other destination byte as before (`frame`'s
    /// byte clause; its `dom`, `provenance` and `wf` clauses are ghost),
    /// and the source buffer is unchanged. The offsets are symbolic; the
    /// lengths are constants (a loop over every value), as in
    /// `kani_proofs::ptr`. With `n == 0` it also returns on any two
    /// pointers, null or dangling, equal or not.
    #[kani::proof]
    #[kani::unwind(10)]
    fn copy_raw_copies() {
        for n in 0..=8 {
            let i: usize = kani::any();
            let j: usize = kani::any();
            kani::assume(i <= 8 - n && j <= 8 - n);
            let mut src: [u8; 8] = kani::any();
            let s0 = src;
            let mut dst: [u8; 8] = kani::any();
            let old = dst;
            // SAFETY: the contract's precondition: both ranges lie in their
            // buffers, and two buffers never overlap.
            unsafe {
                copy_raw(
                    src.as_mut_ptr().add(i) as *const u8,
                    dst.as_mut_ptr().add(j),
                    n,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                )
            };
            for k in 0..8 {
                let want = if k >= j && k < j + n {
                    s0[i + k - j]
                } else {
                    old[k]
                };
                assert_eq!(dst[k], want);
                assert_eq!(src[k], s0[k]);
            }
        }
        let s = core::ptr::without_provenance::<u8>(kani::any());
        let d = if kani::any() {
            s as *mut u8
        } else {
            core::ptr::without_provenance_mut::<u8>(kani::any())
        };
        // SAFETY: an empty copy.
        unsafe { copy_raw(s, d, 0, Tracked::assume_new(), Tracked::assume_new()) };
    }

    /// `copy_raw` within one 8-byte buffer, which its `requires` allows
    /// for two permissions to disjoint bytes of one allocation: from
    /// every offset `i` to every offset `j`, every `n` with both ranges in
    /// the buffer and apart, any contents. Afterwards each of the `n`
    /// bytes from `j` reads as the byte at the same distance from `i`
    /// before the copy (`reads_as`), and every other byte, the source
    /// range's included, as before (`frame`'s byte clause).
    #[kani::proof]
    #[kani::unwind(10)]
    fn copy_raw_copies_within() {
        for i in 0..=8 {
            for j in 0..=8 {
                for n in 0..=8 - if i > j { i } else { j } {
                    if i + n <= j || j + n <= i {
                        let mut a: [u8; 8] = kani::any();
                        let before = a;
                        let base = a.as_mut_ptr();
                        // SAFETY: the contract's precondition: both ranges
                        // lie in `a`, and they are apart.
                        unsafe {
                            copy_raw(
                                base.add(i) as *const u8,
                                base.add(j),
                                n,
                                Tracked::assume_new(),
                                Tracked::assume_new(),
                            )
                        };
                        for k in 0..8 {
                            let want = if k >= j && k < j + n {
                                before[i + k - j]
                            } else {
                                before[k]
                            };
                            assert_eq!(a[k], want);
                        }
                    }
                }
            }
        }
    }
}
