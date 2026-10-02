//! The zero prefix of the thread caches, as ghost tokens (`verus/TODO.md`,
//! "Zeroed memory"). A cache header's `slots[..zeroed]` came from a
//! zeroed claim and nothing has been pushed below `zeroed` since (module
//! doc of `heap`), so a pop from inside the prefix reports its slot zero
//! and `calloc` skips its `memset`. [`CachePerms::zero_ok`] is that
//! claim as a ghost invariant: each of the first `min(len, zeroed)`
//! entries of an owned header's stack has a [`Zeroed`] token for its
//! slot's class-size bytes, kept in [`CachePerms::zero`] under the
//! entry's address. The refill and the run rebuild put the claim's tokens
//! there, a pop from inside the prefix moves its entry's token to
//! [`CachePerms::handed`], and every other writer of a header keeps the
//! invariant ([`Heap::wf`](super::Heap::wf)).

use super::CachePerms;
#[cfg(verus_only)]
use super::{CacheHeader, plain};
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::vspec::zero::Zeroed;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Tracked, int, verus};

verus! {

impl CachePerms {
    /// Entry `i` of owned header `p` holds a slot whose class-size bytes
    /// have a token in `zero`.
    pub(super) open spec fn zero_at(&self, p: *mut CacheHeader, i: int) -> bool {
        let q = self.ent(p, i);
        &&& self.zero.contains_key(q)
        &&& self.zero[q].addr() == plain(self.slots[q].value(), self.hdr(p).slots)
        &&& self.zero[q].len() >= class_table()[self.cls[p]].size
    }

    /// Header `h`'s zero prefix: the first `min(len, zeroed)` entries.
    pub(super) open spec fn in_prefix(h: CacheHeader, i: int) -> bool {
        0 <= i < h.len && i < h.zeroed
    }

    /// `handed` is the token of slot `p` of class `c`.
    pub(super) open spec fn handed_ok(handed: Option<Zeroed>, p: *mut u8, c: int) -> bool {
        &&& handed is Some
        &&& handed.unwrap().addr() == p@.addr
        &&& handed.unwrap().len() >= class_table()[c].size
    }

    /// Every entry of every owned header's zero prefix has its token.
    pub(super) closed spec fn zero_ok(&self) -> bool {
        forall|p: *mut CacheHeader, i: int|
            #![trigger self.ent(p, i)]
            self.hdrs.contains_key(p) && Self::in_prefix(self.hdr(p), i) ==> self.zero_at(p, i)
    }

    /// An entry of a zero prefix has its token.
    pub(super) proof fn lemma_zero_get(&self, p: *mut CacheHeader, i: int)
        requires
            self.zero_ok(),
            self.hdrs.contains_key(p),
            Self::in_prefix(self.hdr(p), i),
        ensures
            self.zero_at(p, i),
    {
        assert(self.zero_at(p, i));
    }

    /// A change confined to header `p`, its stack entries and their
    /// tokens keeps [`CachePerms::zero_ok`] when `p`'s own zero prefix
    /// has its tokens. The other headers' entries are the stack entries
    /// `owner` maps to them ([`CachePerms::tracks`]), so a caller that
    /// writes only `p`'s entries and tokens meets the premise.
    pub(super) proof fn lemma_zero_update(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.zero_ok(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            forall|q: *mut *mut u8|
                #[trigger] old.owner.contains_key(q) && old.owner[q].0 != p && old.slots.contains_key(q) ==> {
                    &&& new.slots[q] == old.slots[q]
                    &&& (old.zero.contains_key(q) ==> new.zero.contains_key(q) && new.zero[q] == old.zero[q])
                },
            forall|i: int| #[trigger] Self::in_prefix(new.hdr(p), i) ==> new.zero_at(p, i),
        ensures
            new.zero_ok(),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            #![trigger new.ent(p2, j)]
            new.hdrs.contains_key(p2) && Self::in_prefix(new.hdr(p2), j) implies new.zero_at(p2, j) by {
            if p2 != p {
                assert(old.hdrs.contains_key(p2));
                assert(new.hdr(p2) == old.hdr(p2));
                assert(super::cell_ok(p2, old.hdrs[p2], old.slots));
                let q = old.ent(p2, j);
                assert(old.zero_at(p2, j));
                assert(old.owner.contains_key(q) && old.owner[q] == (p2, j));
                assert(new.ent(p2, j) == q);
            } else {
                assert(Self::in_prefix(new.hdr(p), j));
            }
        }
    }

    /// [`CachePerms::lemma_zero_update`] with the other headers' entries
    /// named by header and index (as `lemma_slice_frame` gives them) and
    /// the tokens unchanged but for `p`'s own entries.
    pub(super) proof fn lemma_zero_update_ent(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.zero_ok(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            forall|p2: *mut CacheHeader, j: int|
                old.hdrs.contains_key(p2) && p2 != p && 0 <= j < old.hdr(p2).cap
                    ==> #[trigger] new.slots[old.ent(p2, j)] == old.slots[old.ent(p2, j)],
            forall|q: *mut *mut u8|
                #[trigger] old.zero.contains_key(q) && !(old.owner.contains_key(q) && old.owner[q].0 == p)
                    ==> new.zero.contains_key(q) && new.zero[q] == old.zero[q],
            forall|i: int| #[trigger] Self::in_prefix(new.hdr(p), i) ==> new.zero_at(p, i),
        ensures
            new.zero_ok(),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            #![trigger new.ent(p2, j)]
            new.hdrs.contains_key(p2) && Self::in_prefix(new.hdr(p2), j) implies new.zero_at(p2, j) by {
            if p2 != p {
                assert(old.hdrs.contains_key(p2));
                assert(new.hdr(p2) == old.hdr(p2));
                assert(super::cell_ok(p2, old.hdrs[p2], old.slots));
                let q = old.ent(p2, j);
                assert(old.zero_at(p2, j));
                assert(old.owner.contains_key(q) && old.owner[q] == (p2, j));
                assert(new.ent(p2, j) == q);
                assert(new.slots[q] == old.slots[q]);
            } else {
                assert(Self::in_prefix(new.hdr(p), j));
            }
        }
    }

    /// [`CachePerms::zero_ok`] from its definition: every entry of every
    /// owned header's zero prefix has its token (for proofs that move
    /// whole headers, `CachePerms::lemma_swap`, or start from no header,
    /// `CachePerms::tracked_empty`).
    pub(super) proof fn lemma_zero_intro(&self)
        requires
            forall|p: *mut CacheHeader, i: int|
                #![trigger self.ent(p, i)]
                self.hdrs.contains_key(p) && Self::in_prefix(self.hdr(p), i) ==> self.zero_at(p, i),
        ensures
            self.zero_ok(),
    {
    }

    /// A header `p` that `old` does not own joining it with nothing cached
    /// keeps [`CachePerms::zero_ok`].
    pub(super) proof fn lemma_zero_insert(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.zero_ok(),
            !old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls.insert(p, new.cls[p]),
            new.slots == old.slots,
            new.zero == old.zero,
            new.hdr(p).len == 0,
        ensures
            new.zero_ok(),
    {
        assert forall|p2: *mut CacheHeader, j: int|
            #![trigger new.ent(p2, j)]
            new.hdrs.contains_key(p2) && Self::in_prefix(new.hdr(p2), j) implies new.zero_at(p2, j) by {
            assert(p2 != p);
            assert(old.zero_at(p2, j));
        }
    }

    /// A store to header `p` alone that keeps its stack base and does not
    /// grow its zero prefix keeps [`CachePerms::zero_ok`].
    pub(super) proof fn lemma_zero_hdr(old: CachePerms, new: CachePerms, p: *mut CacheHeader)
        requires
            old.wf(),
            old.tracks(),
            old.zero_ok(),
            old.hdrs.contains_key(p),
            new.hdrs == old.hdrs.insert(p, new.hdrs[p]),
            new.cls == old.cls,
            new.slots == old.slots,
            new.zero == old.zero,
            new.hdr(p).slots == old.hdr(p).slots,
            new.hdr(p).len <= old.hdr(p).len,
            new.hdr(p).zeroed <= old.hdr(p).zeroed,
        ensures
            new.zero_ok(),
    {
        assert forall|i: int| #[trigger] Self::in_prefix(new.hdr(p), i) implies new.zero_at(p, i) by {
            old.lemma_zero_get(p, i);
        }
        Self::lemma_zero_update(old, new, p);
    }

    /// A change to the tokens alone (`zero`, `handed`) keeps what the
    /// permissions say about the headers and their stacks.
    pub(super) proof fn lemma_same_but_tokens(a: CachePerms, b: CachePerms, o: CachePerms)
        requires
            b.hdrs == a.hdrs,
            b.slots == a.slots,
            b.owner == a.owner,
            b.cls == a.cls,
        ensures
            a.wf() ==> b.wf(),
            a.tracks() ==> b.tracks(),
            a.keeps(o) ==> b.keeps(o),
            a.tokened() && b.toks == a.toks ==> b.tokened(),
            a.follows(o) ==> b.follows(o),
    {
        assert forall|p: *mut CacheHeader, i: int| #![trigger b.ent(p, i)] b.ent(p, i) == a.ent(p, i) by {}
        if a.tokened() && b.toks == a.toks {
            assert forall|p: *mut CacheHeader, i: int|
                #![trigger b.ent(p, i)]
                b.hdrs.contains_key(p) && 0 <= i < b.hdr(p).len implies b.tok_at(p, i) by {
                assert(a.tok_at(p, i));
            }
            assert(b.tokened());
        }
        if a.tracks() {
            assert forall|p: *mut CacheHeader, i: int|
                #![trigger b.ent(p, i)]
                b.hdrs.contains_key(p) && 0 <= i < b.hdr(p).cap implies {
                    &&& b.owner.contains_key(b.ent(p, i))
                    &&& b.owner[b.ent(p, i)] == (p, i)
                    &&& (i < b.hdr(p).len ==> b.holds(b.cls[p], b.ent(p, i), b.hdr(p).slots))
                } by {
                assert(a.ent(p, i) == b.ent(p, i));
            }
        }
    }

    /// Move the tokens of a reversed claim into a stack: `src[n - 1 - i]`
    /// becomes `dst[slot_ptr(base, i)]` for every `i < k` (a refill and a run
    /// rebuild store the claim reversed at `base`, entries `0..n`, which
    /// `owner` maps to header `p`). The rest of `dst` outside `p`'s entries
    /// is untouched.
    pub(super) proof fn lemma_move_tokens(
        tracked src: &mut Map<int, Zeroed>,
        tracked dst: &mut Map<*mut *mut u8, Zeroed>,
        base: *mut *mut u8,
        n: int,
        k: int,
        owner: Map<*mut *mut u8, (*mut CacheHeader, int)>,
        p: *mut CacheHeader,
    )
        requires
            0 <= k <= n,
            forall|i: int| 0 <= i < k ==> #[trigger] old(src).contains_key(n - 1 - i),
            forall|i: int| 0 <= i < n ==> owner.contains_key(#[trigger] super::slot_ptr(base, i)) && owner[super::slot_ptr(base, i)] == (p, i),
        ensures
            forall|i: int|
                0 <= i < k ==> #[trigger] final(dst).contains_key(super::slot_ptr(base, i))
                    && final(dst)[super::slot_ptr(base, i)] == old(src)[n - 1 - i],
            forall|q: *mut *mut u8|
                #[trigger] old(dst).contains_key(q) && !(owner.contains_key(q) && owner[q].0 == p)
                    ==> final(dst).contains_key(q) && final(dst)[q] == old(dst)[q],
            forall|j: int|
                #[trigger] old(src).contains_key(j) && !(n - k <= j < n) ==> final(src).contains_key(j) && final(src)[j] == old(src)[j],
        decreases k,
    {
        if k > 0 {
            Self::lemma_move_tokens(src, dst, base, n, k - 1, owner, p);
            assert(old(src).contains_key(n - 1 - (k - 1)));
            let tracked t = src.tracked_remove(n - k);
            let q = super::slot_ptr(base, k - 1);
            dst.tracked_insert(q, t);
            assert forall|i: int| 0 <= i < k - 1 implies super::slot_ptr(base, i) != q by {
                assert(owner[super::slot_ptr(base, i)] == (p, i));
                assert(owner[q] == (p, k - 1));
            }
        }
    }

    /// Install the tokens `z` of a zeroed claim as the zero prefix of
    /// header `p`: entry `i < n` of its stack holds (as [`plain`] reads it)
    /// the claim's slot `claimed[n - 1 - i]`, which `z[n - 1 - i]` covers, and
    /// the prefix is at most those `n` entries (none unless `zeroed`). `pre`
    /// is the well-formed state before the refill or run rebuild that
    /// wrote the entries, confined to `p`'s stack.
    pub(super) proof fn lemma_install_claim(
        tracked perms: &mut CachePerms,
        tracked z: Map<int, Zeroed>,
        pre: CachePerms,
        p: *mut CacheHeader,
        n: int,
        zeroed: bool,
        claimed: Seq<*mut u8>,
    )
        requires
            pre.wf(),
            pre.tracks(),
            pre.zero_ok(),
            pre.hdrs.contains_key(p),
            old(perms).wf(),
            old(perms).tracks(),
            old(perms).hdrs == pre.hdrs.insert(p, old(perms).hdrs[p]),
            old(perms).cls == pre.cls,
            old(perms).owner == pre.owner,
            old(perms).zero == pre.zero,
            old(perms).hdr(p).slots == pre.hdr(p).slots,
            old(perms).hdr(p).cap == pre.hdr(p).cap,
            0 <= n <= old(perms).hdr(p).len,
            old(perms).hdr(p).zeroed <= (if zeroed { n } else { 0 }),
            forall|p2: *mut CacheHeader, j: int|
                pre.hdrs.contains_key(p2) && p2 != p && 0 <= j < pre.hdr(p2).cap
                    ==> #[trigger] old(perms).slots[pre.ent(p2, j)] == pre.slots[pre.ent(p2, j)],
            forall|i: int| 0 <= i < n ==> #[trigger] plain(old(perms).slots[old(perms).ent(p, i)].value(), old(perms).hdr(p).slots) == claimed[n - 1 - i]@.addr,
            zeroed ==> forall|k: int| 0 <= k < n ==> #[trigger] z.contains_key(k) && z[k].covers(claimed[k], class_table()[pre.cls[p]].size as int),
        ensures
            final(perms).zero_ok(),
            final(perms).hdrs == old(perms).hdrs,
            final(perms).slots == old(perms).slots,
            final(perms).owner == old(perms).owner,
            final(perms).cls == old(perms).cls,
            final(perms).toks == old(perms).toks,
    {
        let base = old(perms).hdr(p).slots;
        let tracked mut zz = z;
        assert(super::cell_ok(p, old(perms).hdrs[p], old(perms).slots)) by {
            assert(old(perms).hdrs.contains_key(p));
        }
        assert forall|i: int| 0 <= i < n implies #[trigger] old(perms).ent(p, i) == super::slot_ptr(base, i) by {}
        if zeroed {
            assert forall|i: int| 0 <= i < n implies #[trigger] zz.contains_key(n - 1 - i) by {}
            assert forall|i: int| 0 <= i < n implies perms.owner.contains_key(#[trigger] super::slot_ptr(base, i)) && perms.owner[super::slot_ptr(base, i)] == (p, i) by {
                assert(old(perms).ent(p, i) == super::slot_ptr(base, i));
            }
            Self::lemma_move_tokens(&mut zz, &mut perms.zero, base, n, n, perms.owner, p);
        }
        assert forall|i: int| #[trigger] Self::in_prefix(perms.hdr(p), i) implies perms.zero_at(p, i) by {
            assert(perms.ent(p, i) == super::slot_ptr(base, i));
            assert(perms.ent(p, i) == old(perms).ent(p, i));
            assert(zeroed && i < n);
            assert(perms.zero.contains_key(super::slot_ptr(base, i)));
            assert(perms.zero[super::slot_ptr(base, i)] == z[n - 1 - i]);
            assert(z.contains_key(n - 1 - i));
            assert(z[n - 1 - i].covers(claimed[n - 1 - i], class_table()[pre.cls[p]].size as int));
            assert(plain(perms.slots[perms.ent(p, i)].value(), perms.hdr(p).slots) == claimed[n - 1 - i]@.addr);
        }
        Self::lemma_zero_update_ent(pre, *perms, p);
    }

    /// Unchanged headers, entries and tokens keep [`CachePerms::zero_ok`].
    pub(super) proof fn lemma_zero_same(old: CachePerms, new: CachePerms)
        requires
            old.zero_ok(),
            new.hdrs == old.hdrs,
            new.slots == old.slots,
            new.zero == old.zero,
            new.cls == old.cls,
        ensures
            new.zero_ok(),
    {
        assert forall|p: *mut CacheHeader, i: int|
            #![trigger new.ent(p, i)]
            new.hdrs.contains_key(p) && Self::in_prefix(new.hdr(p), i) implies new.zero_at(p, i) by {
            assert(old.zero_at(p, i));
        }
    }
}

/// A run rebuild's entries: after the block of `n` claimed slots
/// (stored reversed in `outf`, written at `len..len + want` of the stack
/// at `base` in `mid`) is moved to the bottom (`allf`, from `all0` the
/// way `lemma_rebuild_moves` describes), entry `i < n` holds slot
/// `claimed[n - 1 - i]`, as [`plain`] reads it.
pub(super) proof fn lemma_rebuild_entries(
    new_slots: Map<*mut *mut u8, vstd::raw_ptr::PointsTo<*mut u8>>,
    mid: Map<*mut *mut u8, vstd::raw_ptr::PointsTo<*mut u8>>,
    base: *mut *mut u8,
    len: int,
    want: int,
    n: int,
    c: int,
    claimed: Seq<*mut u8>,
    outf: Seq<*mut u8>,
    all0: Seq<*mut u8>,
    allf: Seq<*mut u8>,
)
    requires
        super::regions_ok(),
        0 <= len,
        0 <= n <= want,
        claimed.len() == want,
        crate::layout::slots_in(claimed, c, 0, n),
        forall|k: int| 0 <= k < n ==> (#[trigger] outf[k])@.addr == super::stored(claimed[n - 1 - k]@.addr, base),
        forall|i: int| len <= i < len + want ==> #[trigger] mid[super::slot_ptr(base, i)].is_init(),
        forall|i: int| len <= i < len + want ==> #[trigger] mid[super::slot_ptr(base, i)].value() == outf[i - len],
        forall|i: int|
            0 <= i < len + n && mid[super::slot_ptr(base, i)].is_init() ==> all0[i] == #[trigger] mid[super::slot_ptr(base, i)].value(),
        len >= n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else if i < len { all0[i] } else { all0[i - len] },
        len < n ==> forall|i: int| 0 <= i < len + n ==> #[trigger] allf[i] == if i < n { all0[len + i] } else { all0[i - n] },
        forall|i: int| 0 <= i < len + n ==> #[trigger] new_slots[super::slot_ptr(base, i)].value() == allf[i],
    ensures
        forall|i: int| 0 <= i < n ==> #[trigger] plain(new_slots[super::slot_ptr(base, i)].value(), base) == claimed[n - 1 - i]@.addr,
{
    assert forall|i: int| 0 <= i < n implies #[trigger] plain(new_slots[super::slot_ptr(base, i)].value(), base) == claimed[n - 1 - i]@.addr by {
        assert(new_slots[super::slot_ptr(base, i)].value() == allf[i]);
        assert(allf[i] == all0[len + i]);
        assert(mid[super::slot_ptr(base, len + i)].is_init());
        assert(all0[len + i] == mid[super::slot_ptr(base, len + i)].value());
        assert(mid[super::slot_ptr(base, len + i)].value() == outf[i]);
        assert(crate::layout::process_regions().is_slot(c, claimed[n - 1 - i]@.addr));
        super::lemma_plain_stored(outf[i], claimed[n - 1 - i], c, base);
    }
}

impl super::Heap {
    /// Hand out token `t` of slot `p` of class `c` when `put`, as a pop does.
    /// No code: ghost only.
    #[inline(always)]
    pub(super) fn put_handed(&mut self, t: Tracked<Zeroed>, Ghost(put): Ghost<bool>, Ghost(p): Ghost<*mut u8>, Ghost(c): Ghost<int>)
        requires
            old(self).wf(),
            put ==> t@.covers(p, class_table()[c].size as int),
        ensures
            final(self).keeps(*old(self)),
            put ==> final(self).handed_ok(p, c),
    {
        let Tracked(t) = t;
        proof {
            if put {
                self.perms.borrow_mut().handed = Some(t);
            }
            super::Heap::lemma_keeps_handed(*old(self), *self);
            super::lemma_handed_ok(*self, p, c);
        }
    }

    /// Take the token the last pop handed out when `take` (an empty one
    /// otherwise). No code: ghost only.
    #[inline(always)]
    pub fn take_handed(&mut self, Ghost(take): Ghost<bool>, Ghost(p): Ghost<*mut u8>, Ghost(c): Ghost<int>) -> (z: Tracked<Zeroed>)
        requires
            old(self).wf(),
            take ==> old(self).handed_ok(p, c),
        ensures
            final(self).keeps(*old(self)),
            take ==> z@.covers(p, class_table()[c].size as int),
    {
        let tracked mut z = Zeroed::empty(0);
        proof {
            super::lemma_handed_ok(*old(self), p, c);
            if take {
                z = self.perms.borrow_mut().handed.tracked_take();
            }
            super::Heap::lemma_keeps_handed(*old(self), *self);
        }
        Tracked(z)
    }
}

} // verus!
