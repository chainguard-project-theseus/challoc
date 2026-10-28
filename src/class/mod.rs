//! Per-class state, slab selection, commit, and the claim/release protocol
//! (design.md sections 4 and 5).
//!
//! One [`SlabTable`] exists per class: the class region's descriptor array
//! and the commit watermark, shared by every arena, behind a commit lock.
//! One [`ClassState`] exists per `(arena, class)`. It holds three
//! [`TwoLevelBitmap`] views over slab numbers, into the arena's
//! [`SetTable`] (`Set::in_table`): `partial` (slabs believed to have
//! a claimable slot), `empty` (`live == 0`, dirty, not yet purged) and
//! `purged` (`live == 0`, pages discarded). All three are behind one futex
//! lock that is taken only on slab transitions: selecting a slab for a
//! refill, a slab filling up or emptying, a word going from full to
//! non-full, purge, commit (which nests the table's commit lock inside).
//! Claims and releases against a slab's bits happen outside the lock.
//!
//! The module is one directory, split by concern. `table` holds the
//! [`SlabTable`] (descriptors, class bitmap, owner array, the hand-out of
//! groups and the commit of slabs); `sets` the lazily committed set tables
//! ([`Lazy`](sets::Lazy), [`Set`], [`SetTable`]); this file the
//! [`ClassState`] and the protocol below (claim, plan, take, release,
//! reopen); `fresh` the fresh-slab path; `decay` the purge and the
//! decay walk; `verify` the
//! `check_zero` debug mode and the test-only pointer checks; `abort` the
//! `#[cold]` heap-inconsistency aborts; `tests` the tests.
//!
//! Slabs are handed to arenas in *groups* of consecutive slabs
//! (`SlabTable::group`: the slabs whose bits fill one 64-byte line of the
//! class bitmap, `Class::slabs_per_line`, or one page group if that is
//! larger), lowest group first to whichever arena runs out of slabs next.
//! A group's slabs are committed by their arena as it needs them, a run
//! up to the next guard slab at a time (`ClassState::next_fresh_slab`,
//! `SlabTable::commit_run`), so an arena owns whole bitmap lines and
//! whole descriptor lines: the `free` check of one arena's slot never
//! reads a line another arena's refills and flushes write. Every slab
//! belongs to the arena of its group (`SlabTable::owner`, a dense array
//! of one `u16` per group, and `SlabDesc::arena`), and only that arena's
//! state may claim from or release to it.
//!
//! A group stays with its arena for good: the owner word is written once,
//! when `take_group` hands the group out, so a slab's arena never changes.
//! `reopen` and `on_live_zero` still test the owner word under the lock
//! before they touch the arena's sets.
//!
//! Protocol. A refill takes the lock, picks the lowest partial slab (else
//! the lowest empty, else the lowest purged, else commits the next slab),
//! moves it to `Active`, *reserves* the wanted count in the descriptor's
//! `live` counter, and releases the lock. It then claims bits, hands out
//! what it got and gives back the unused reservation. Reserving before
//! claiming means `live` is never zero while a claim is in flight, so a
//! purge (which requires `live == 0` under the lock) can never discard
//! memory a thread is about to hand out. A slab observed `Fresh` or
//! `Purged` at selection is the exception to the unlocked bit claim: its
//! slots (the lowest `n` bits; `live` is zero, so nothing contends) are
//! claimed in the same lock hold, before the slab enters the partial set.
//! That is what makes the refill's zero report sound: a bit decided after
//! the lock is dropped could be one that another thread claimed from the
//! now-partial slab, handed to the program and got back dirty in between,
//! and `calloc` would skip its memset over it (the calloc race).
//!
//! A release clears bits with `fetch_and` and then subtracts from `live`.
//! If a word was full before the clear the releaser sets the partial bit
//! under the lock (once per batch, not per word); if `live` reaches zero
//! the releaser makes the slab `Empty` under the lock. A refill plans all
//! its slabs in one lock hold, reserving at most `nslots - live` from each,
//! and clears the partial bit of a slab it finds full (`is_full` under the
//! lock), so a concurrent release either is visible to that check or sets
//! the bit afterwards.

use crate::arena::Published;
#[cfg(verus_keep_ghost)]
use crate::arena::process_pub_loc;
use crate::bitmap::FullSeen;
#[cfg(verus_keep_ghost)]
use crate::bitmap::{
    Slots, addrs, has_bit, lemma_addrs_difference, lemma_addrs_union, lemma_bit_ops,
    lemma_subset_bits, mask_bits, subset,
};
use crate::layout::{CLASSES, Class, MAX_SLAB_WORDS, Regions};
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, bit_slots_in, class_table, process_regions, slots_in};
use crate::lock::{Lock, LockOrder, PAD_ALIGN, Padded, RawLock};
#[cfg(verus_keep_ghost)]
use crate::lock::{lock_frame, lock_init, lock_inv, rank_class, rank_meta, rank_sets, rank_table};
use crate::meta::Meta;
#[cfg(verus_keep_ghost)]
use crate::purge::PurgedZero;
use crate::purge::{self, Decayer};
use crate::slab::{DescPerm, SlabDesc, State};
#[cfg(verus_keep_ghost)]
use crate::slab::{StateTicket, desc_id, state_byte};
#[cfg(verus_keep_ghost)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::zero::{ZeroRaw, Zeroed};
use core::sync::atomic::{AtomicU64, Ordering};
// The whole prelude only under Verus, which builds no tests: in a test
// build it would reach `tests.rs` through `use super::*`.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Ghost, Map, Tracked, int, nat, verus};
use vstd::raw_ptr::PointsTo;
#[cfg(verus_keep_ghost)]
use vstd::resource::Loc;
use vstd::resource::set::{GhostPersistentSubset, GhostSetAuth, GhostSingleton, GhostSubset};

mod abort;
mod decay;
mod fresh;
mod sets;
mod table;
#[cfg(test)]
mod tests;
mod verify;

#[cfg(verus_keep_ghost)]
use crate::vspec::core_int::popcount;
use abort::{
    double_free, guard_free, live_above_slots, owner_mismatch, release_into_fresh_slab,
    reservation_unmet, wrong_arena, zero_slab_had_bits_set,
};
#[cfg(test)]
pub use sets::SET_GROUPS;
pub(crate) use sets::Set;
pub use sets::SetTable;
pub use table::SlabTable;
#[cfg(verus_keep_ghost)]
pub use table::slot_loc;
#[cfg(verus_keep_ghost)]
use table::{
    bits_ghost, bits_shape, desc_at, guard_marked, lemma_bit_in_slab, slab_marked, slab_mask_of,
    slab_owner, slot_owner, words_of,
};
use verify::{fresh_with_live, release_into_fresh};
#[cfg(verus_keep_ghost)]
use vstd::set::Set as VSet;
#[cfg(verus_keep_ghost)]
use vstd::std_specs::bits::{axiom_u64_trailing_zeros, u64_trailing_zeros};
#[cfg(verus_keep_ghost)]
use vstd::std_specs::iter::IteratorSpec;

verus! {

/// Counters kept under the class lock. Slab counts only; `curregs` and
/// the byte totals are derived from them in `stats.rs`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassStats {
    /// Slabs committed for allocation (excludes guard slabs).
    pub committed: usize,
    /// Guard slabs committed (never handed out).
    pub guards: usize,
    /// Slabs currently `Empty`.
    pub empty: usize,
    /// Slabs currently `Purged`.
    pub purged: usize,
    /// Slabs in the partial set (jemalloc's `nonfull_slabs`).
    pub nonfull: usize,
}

impl ClassStats {
    /// Slabs that can hold live slots (neither `Empty` nor `Purged`):
    /// jemalloc's `curslabs`, and the slab count behind `active` and the
    /// compaction gate's capacity.
    #[inline]
    pub fn curslabs(&self) -> (r: usize)
        requires
            self.empty + self.purged <= self.committed,
        ensures
            r == self.committed - self.empty - self.purged,
    {
        self.committed - self.empty - self.purged
    }
}


/// xorshift64 step for `slot_randomize` (the state must not be zero; the
/// heap seeds it so). Not a cryptographic generator: the key randomises
/// the order slots are handed out in, which is a heap-layout obstacle, not
/// a secret.
#[inline]
pub(crate) fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

} // verus!

/// Fisher-Yates shuffle of `out` with [`xorshift`] ([`shuffle_zero`] with
/// no tokens to carry).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn shuffle(out: &mut [*mut u8], rng: &mut u64) {
    shuffle_zero(
        out,
        rng,
        Ghost::assume_new(),
        Ghost::assume_new(),
        Tracked::assume_new(),
        Ghost::assume_new(),
        Tracked::assume_new(),
    );
}

verus! {

/// `z[i]` is a zero token covering `size` bytes of `s[i]`, for every entry.
pub open spec fn tokens_cover(z: Map<int, Zeroed>, s: Seq<*mut u8>, size: int) -> bool {
    forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < s.len() ==> z.contains_key(i) && z[i].covers(s[i], size)
}

/// [`shuffle`], moving the zero token of each entry (`z[i]` for `out[i]`,
/// when `zeroed`) along with it, and its slot token (`t[i]`, when `loc`
/// names their authority).
#[verifier::spinoff_prover]
pub(crate) fn shuffle_zero(
    out: &mut [*mut u8],
    rng: &mut u64,
    Ghost(zeroed): Ghost<bool>,
    Ghost(size): Ghost<int>,
    Tracked(z): Tracked<&mut Map<int, Zeroed>>,
    Ghost(loc): Ghost<Option<vstd::resource::Loc>>,
    Tracked(t): Tracked<&mut Map<int, GhostSingleton<int>>>,
)
    requires
        zeroed ==> tokens_cover(*old(z), old(out)@, size),
        loc matches Some(l) ==> keyed(*old(t), ptr_addrs(old(out)@), l, 0, old(out)@.len() as int),
    ensures
        final(out)@.len() == old(out)@.len(),
        permutes(final(out)@, old(out)@),
        forall|i: int| 0 <= i < old(out)@.len() ==> #[trigger] from_seq(final(out)@, old(out)@, i),
        zeroed ==> tokens_cover(*final(z), final(out)@, size),
        loc matches Some(l) ==> keyed(*final(t), ptr_addrs(final(out)@), l, 0, final(out)@.len() as int),
        keyed_frame(*final(t), *old(t), 0, old(out)@.len() as int),
{
    proof {
        lemma_from_seq_refl(out@);
    }
    for i in (1..out.len()).rev()
        invariant
            out@.len() == old(out)@.len(),
            permutes(out@, old(out)@),
            forall|k: int| 0 <= k < old(out)@.len() ==> #[trigger] from_seq(out@, old(out)@, k),
            loc matches Some(l) ==> keyed(*t, ptr_addrs(out@), l, 0, out@.len() as int),
            keyed_frame(*t, *old(t), 0, old(out)@.len() as int),
            zeroed ==> tokens_cover(*z, out@, size),
    {
        let j = (xorshift(rng) % (i as u64 + 1)) as usize;
        let ghost cur = out@;
        out.swap(i, j);
        proof {
            lemma_swap_permutes(cur, i as int, j as int);
            assert forall|k: int| 0 <= k < old(out)@.len() implies #[trigger] from_seq(out@, old(out)@, k) by {
                let src = if k == i as int { j as int } else if k == j as int { i as int } else { k };
                assert(from_seq(cur, old(out)@, src));
                let w = choose|w: int| 0 <= w < old(out)@.len() && cur[src] == old(out)@[w];
                assert(out@[k] == old(out)@[w]);
            }
            let z0 = *z;
            if zeroed && i != j {
                let tracked a = z.tracked_remove(i as int);
                let tracked b = z.tracked_remove(j as int);
                z.tracked_insert(i as int, b);
                z.tracked_insert(j as int, a);
            }
            let t0 = *t;
            if loc is Some && i != j {
                let tracked a = t.tracked_remove(i as int);
                let tracked b = t.tracked_remove(j as int);
                t.tracked_insert(i as int, b);
                t.tracked_insert(j as int, a);
            }
            assert forall|k: int| #![trigger t.contains_key(k)] #![trigger old(t).contains_key(k)] !(0 <= k < old(out)@.len()) implies {
                &&& t.contains_key(k) == old(t).contains_key(k)
                &&& old(t).contains_key(k) ==> t[k] == old(t)[k]
            } by {
                assert(t0.contains_key(k) == old(t).contains_key(k));
                if old(t).contains_key(k) {
                    assert(t0[k] == old(t)[k]);
                }
            }
            if loc is Some {
                let l = loc.unwrap();
                assert forall|k: int| #![trigger t.contains_key(k)] #![trigger t[k]] 0 <= k < out@.len() implies {
                    &&& t.contains_key(k)
                    &&& t[k].id() == l
                    &&& t[k]@ == ptr_addrs(out@)[k]
                } by {
                    assert(t0.contains_key(k) && t0.contains_key(i as int) && t0.contains_key(j as int));
                    assert(ptr_addrs(cur)[i as int] == cur[i as int]@.addr as int);
                    assert(ptr_addrs(cur)[j as int] == cur[j as int]@.addr as int);
                    assert(ptr_addrs(cur)[k] == cur[k]@.addr as int);
                    assert(ptr_addrs(out@)[k] == out@[k]@.addr as int);
                    if k == i as int {
                        assert(out@[k] == cur[j as int]);
                    } else if k == j as int {
                        assert(out@[k] == cur[i as int]);
                    } else {
                        assert(out@[k] == cur[k]);
                    }
                }
            }
            assert(keyed_frame(*t, *old(t), 0, old(out)@.len() as int));
            if loc is Some {
                assert(keyed(*t, ptr_addrs(out@), loc.unwrap(), 0, out@.len() as int));
            }
            if zeroed {
                assert forall|k: int| #![trigger z.contains_key(k)] #![trigger z[k]] 0 <= k < out@.len() implies z.contains_key(k) && z[k].covers(out@[k], size) by {
                    assert(z0.contains_key(k) && z0.contains_key(i as int) && z0.contains_key(j as int));
                    if k == i as int {
                        assert(out@[k] == cur[j as int]);
                    } else if k == j as int {
                        assert(out@[k] == cur[i as int]);
                    } else {
                        assert(out@[k] == cur[k]);
                    }
                }
                assert(tokens_cover(*z, out@, size));
            }
        }
    }
}

/// `s` is a permutation of `o`: every pointer occurs in `s` as many
/// times as in `o`.
pub(crate) open spec fn permutes(s: Seq<*mut u8>, o: Seq<*mut u8>) -> bool {
    s.to_multiset() == o.to_multiset()
}

/// Swapping two entries of `s` (what `<[T]>::swap` does) permutes it.
proof fn lemma_swap_permutes(s: Seq<*mut u8>, i: int, j: int)
    requires
        0 <= i < s.len(),
        0 <= j < s.len(),
    ensures
        permutes(s.update(i, s[j]).update(j, s[i]), s),
{
    broadcast use vstd::seq_lib::to_multiset_update, vstd::seq_lib::to_multiset_contains, vstd::multiset::group_multiset_axioms;
    let m = s.to_multiset();
    let s1 = s.update(i, s[j]);
    assert(s1[j] == s[j]);
    assert(s1.to_multiset() == m.insert(s[j]).remove(s[i]));
    assert(m.contains(s[i]));
    assert(m.insert(s[j]).remove(s[i]).insert(s[i]).remove(s[j]) =~= m);
}

/// Every entry of `s` is an entry of `s`.
proof fn lemma_from_seq_refl(s: Seq<*mut u8>)
    ensures
        forall|k: int| 0 <= k < s.len() ==> #[trigger] from_seq(s, s, k),
{
    assert forall|k: int| 0 <= k < s.len() implies #[trigger] from_seq(s, s, k) by {
        // A named copy: `s[k] == s[k]` alone simplifies away before the
        // witness `k` can be matched.
        let x = s[k];
        assert(x == s[k]);
    }
}

/// Entry `i` of `s` is an entry of `o`: what a permutation keeps.
pub(crate) open spec fn from_seq(s: Seq<*mut u8>, o: Seq<*mut u8>, i: int) -> bool {
    exists|j: int| 0 <= j < o.len() && s[i] == #[trigger] o[j]
}

/// Result of one [`ClassState::claim`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// Slots written to the output; fewer than asked means the region or
    /// the metadata region is exhausted or a commit failed.
    pub n: usize,
    /// Every returned slot came from a slab that was `Fresh` or `Purged`
    /// when this thread selected it and had its bit claimed in that same
    /// lock hold, before anything else could allocate from the slab, so
    /// its memory reads as zero.
    pub zeroed: bool,
}

/// The zero tokens `z` of claim `c` of `class`, whose slots are `out[..n]`:
/// when the claim is zeroed, one per slot, covering the slot's class size.
pub open spec fn claim_zero_ok(c: Claim, out: Seq<*mut u8>, z: Map<int, Zeroed>, class: int) -> bool {
    c.zeroed ==> forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]]
        0 <= i < c.n ==> z.contains_key(i) && z[i].covers(out[i], class_table()[class].size as int)
}

} // verus!

verus! {

/// Ghost: the slot tokens of a batch of claimed slot pointers, by key
/// (the pointer's index, from a base): the class bitmap bit behind each
/// pointer (`bits`), and each pointer's token (`toks`, one address each).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct SlotToks {
    pub ghost bits: Map<int, int>,
    pub tracked toks: Map<int, GhostSingleton<int>>,
}

/// The addresses of the slots of mask `x` of word `word` of `slab` (slab
/// coordinates), in table `t`'s slot tokens.
pub open spec fn word_addrs(t: SlabTable, slab: int, word: int, x: u64) -> VSet<int> {
    addrs(t.spec_slots(), mask_bits(t.spec_slots(), slab * t.spec_slots().sb, word, x))
}

/// The addresses of a set with one more slot.
pub proof fn lemma_addrs_insert(s: Slots, a: VSet<int>, b: int)
    ensures
        addrs(s, a.insert(b)) == addrs(s, a).insert(s.addr(b)),
{
    broadcast use VSet::lemma_map_contains;
    assert forall|y: int| #[trigger] addrs(s, a.insert(b)).contains(y) == addrs(s, a).insert(s.addr(b)).contains(y) by {
        if addrs(s, a.insert(b)).contains(y) {
            let x = choose|x: int| a.insert(b).contains(x) && y == s.addr(x);
            if x != b {
                assert(a.contains(x));
            }
        }
        if y == s.addr(b) {
            assert(a.insert(b).contains(b));
        }
        if addrs(s, a).contains(y) {
            let x = choose|x: int| a.contains(x) && y == s.addr(x);
            assert(a.insert(b).contains(x));
        }
    }
    assert(addrs(s, a.insert(b)) =~= addrs(s, a).insert(s.addr(b)));
}

impl SlotToks {
    /// Entries `lo..hi` of `out` are claimed bits of table `t` (keys
    /// `base + i`), slots, each at its bit's address, with its token.
    pub open spec fn holds(self, t: SlabTable, out: Seq<*mut u8>, base: int, lo: int, hi: int) -> bool {
        forall|i: int| lo <= i < hi ==> self.entry(t, #[trigger] out[i], base + i)
    }

    /// Key `key` holds the bit behind `p` (in the region, at `p`), which
    /// is a slot of a slab that is not a guard slab, and `p`'s token.
    pub open spec fn entry(self, t: SlabTable, p: *mut u8, key: int) -> bool {
        let s = t.spec_slots();
        &&& self.bits.contains_key(key)
        &&& 0 <= self.bits[key] < s.nbits
        &&& p@.addr as int == s.addr(self.bits[key])
        &&& s.real(self.bits[key])
        &&& !t.spec_is_guard((self.bits[key] / s.sb) as usize)
        &&& self.toks.contains_key(key)
        &&& self.toks[key].id() == s.loc
        &&& self.toks[key]@ == p@.addr as int
    }

    /// `toks` holds the token of each of `out[lo..hi]` (keys `base + i`).
    /// Opaque, so the callers that do not move tokens do not pay for it.
    #[verifier::opaque]
    pub open spec fn owns(self, t: SlabTable, out: Seq<*mut u8>, base: int, lo: int, hi: int) -> bool {
        forall|i: int| #![trigger out[i]] #![trigger self.toks.contains_key(base + i)] #![trigger self.toks[base + i]] lo <= i < hi ==> {
            &&& self.toks.contains_key(base + i)
            &&& self.toks[base + i].id() == t.spec_slots().loc
            &&& self.toks[base + i]@ == (out[i])@.addr as int
        }
    }

    /// [`SlotToks::owns`] at one index `i` of `lo..hi`.
    proof fn lemma_owns_at(self, t: SlabTable, out: Seq<*mut u8>, base: int, lo: int, hi: int, i: int)
        requires
            self.owns(t, out, base, lo, hi),
            lo <= i < hi,
        ensures
            self.toks.contains_key(base + i),
            self.toks[base + i].id() == t.spec_slots().loc,
            self.toks[base + i]@ == out[i]@.addr as int,
    {
        reveal(SlotToks::owns);
        assert(self.toks[base + i]@ == out[i]@.addr as int);
    }

    /// [`SlotToks::owns`] of `out[..n]` from key 0 is [`keyed`] of their
    /// addresses.
    pub proof fn lemma_owns_keyed(self, t: SlabTable, out: Seq<*mut u8>, n: int)
        requires
            0 <= n <= out.len(),
        ensures
            self.owns(t, out, 0, 0, n) <==> keyed(self.toks, ptr_addrs(out.subrange(0, n)), t.spec_slots().loc, 0, n),
    {
        reveal(SlotToks::owns);
        let a = ptr_addrs(out.subrange(0, n));
        let loc = t.spec_slots().loc;
        assert forall|i: int| 0 <= i < n implies a[i] == (#[trigger] out[i])@.addr as int by {
            assert(out.subrange(0, n)[i] == out[i]);
        }
        if self.owns(t, out, 0, 0, n) {
            assert forall|i: int| #![trigger self.toks.contains_key(i)] #![trigger self.toks[i]] 0 <= i < n implies {
                &&& self.toks.contains_key(i)
                &&& self.toks[i].id() == loc
                &&& self.toks[i]@ == a[i]
            } by {
                assert(a[i] == out[i]@.addr as int);
                self.lemma_owns_at(t, out, 0, 0, n, i);
            }
            assert(keyed(self.toks, a, loc, 0, n));
        }
        if keyed(self.toks, a, loc, 0, n) {
            assert forall|i: int| #![trigger out[i]] #![trigger self.toks.contains_key(0 + i)] #![trigger self.toks[0 + i]] 0 <= i < n implies {
                &&& self.toks.contains_key(0 + i)
                &&& self.toks[0 + i].id() == loc
                &&& self.toks[0 + i]@ == (out[i])@.addr as int
            } by {
                lemma_keyed_at(self.toks, a, loc, 0, n, i);
                assert(a[i] == out[i]@.addr as int);
            }
            assert(self.owns(t, out, 0, 0, n));
        }
    }

    /// [`SlotToks::owns`] of claimed bits.
    pub proof fn lemma_owns_of_real(self, t: SlabTable, out: Seq<*mut u8>, base: int, n: int)
        requires
            self.holds(t, out, base, 0, n),
        ensures
            self.owns(t, out, base, 0, n),
    {
        reveal(SlotToks::owns);
        assert forall|i: int| #![trigger out[i]] #![trigger self.toks.contains_key(base + i)] #![trigger self.toks[base + i]] 0 <= i < n implies {
            &&& self.toks.contains_key(base + i)
            &&& self.toks[base + i].id() == t.spec_slots().loc
            &&& self.toks[base + i]@ == (out[i])@.addr as int
        } by {
            assert(self.entry(t, out[i], base + i));
        }
    }

    /// Keys outside `lo..hi` are as in `old`.
    pub open spec fn frame(self, old: SlotToks, lo: int, hi: int) -> bool {
        &&& forall|j: int| !(lo <= j < hi) ==> (#[trigger] self.bits.contains_key(j)) == old.bits.contains_key(j)
        &&& forall|j: int| !(lo <= j < hi) && old.bits.contains_key(j) ==> #[trigger] self.bits[j] == old.bits[j]
        &&& forall|j: int| !(lo <= j < hi) ==> (#[trigger] self.toks.contains_key(j)) == old.toks.contains_key(j)
        &&& forall|j: int| !(lo <= j < hi) && old.toks.contains_key(j) ==> #[trigger] self.toks[j] == old.toks[j]
    }

    /// One more entry, `o2[n]` (key `base + n`), recorded after `o1[..n]`,
    /// which `o2` keeps.
    pub proof fn lemma_extend(
        old: SlotToks,
        new: SlotToks,
        orig: SlotToks,
        t: SlabTable,
        o1: Seq<*mut u8>,
        o2: Seq<*mut u8>,
        base: int,
        n: int,
        bit: int,
    )
        requires
            old.holds(t, o1, base, 0, n),
            old.frame(orig, base, base + n),
            0 <= n < o2.len(),
            o1.len() == o2.len(),
            forall|i: int| 0 <= i < n ==> o2[i] == o1[i],
            new.bits == old.bits.insert(base + n, bit),
            forall|j: int| j != base + n ==> (#[trigger] new.toks.contains_key(j)) == old.toks.contains_key(j),
            forall|j: int| j != base + n && old.toks.contains_key(j) ==> #[trigger] new.toks[j] == old.toks[j],
            0 <= bit < t.spec_slots().nbits,
            o2[n]@.addr as int == t.spec_slots().addr(bit),
            t.spec_slots().real(bit),
            !t.spec_is_guard((bit / t.spec_slots().sb) as usize),
            new.toks.contains_key(base + n),
            new.toks[base + n].id() == t.spec_slots().loc,
            new.toks[base + n]@ == o2[n]@.addr as int,
        ensures
            new.holds(t, o2, base, 0, n + 1),
            new.frame(orig, base, base + n + 1),
    {
        assert forall|i: int| 0 <= i < n + 1 implies new.entry(t, #[trigger] o2[i], base + i) by {
            if i < n {
                assert(o2[i] == o1[i]);
                assert(old.entry(t, o1[i], base + i));
            }
        }
    }

    /// Record `bit` and its token under `key`.
    pub proof fn put(tracked &mut self, key: int, bit: int, tracked tok: GhostSingleton<int>)
        ensures
            final(self).bits == old(self).bits.insert(key, bit),
            final(self).toks == old(self).toks.insert(key, tok),
    {
        self.bits = self.bits.insert(key, bit);
        self.toks.tracked_insert(key, tok);
    }

}

/// `z` holds, under key `base + i`, a zero token covering `size` bytes of
/// `out[i]`, for every `lo <= i < hi`.
pub open spec fn zero_keys(z: Map<int, Zeroed>, out: Seq<*mut u8>, base: int, lo: int, hi: int, size: int) -> bool {
    forall|i: int| #![trigger z.contains_key(base + i)] #![trigger z[base + i]] lo <= i < hi ==> z.contains_key(base + i) && z[base + i].covers(out[i], size)
}

/// [`zero_frame`] over `lo..hi` holds over any wider `lo..hi2`.
proof fn lemma_zero_frame_widen(new: Map<int, Zeroed>, old: Map<int, Zeroed>, lo: int, hi: int, hi2: int)
    requires
        zero_frame(new, old, lo, hi),
        hi <= hi2,
    ensures
        zero_frame(new, old, lo, hi2),
{
    assert forall|k: int| #![trigger new.contains_key(k)] #![trigger old.contains_key(k)] !(lo <= k < hi2) implies {
        &&& new.contains_key(k) == old.contains_key(k)
        &&& old.contains_key(k) ==> new[k] == old[k]
    } by {
        assert(!(lo <= k < hi));
    }
}

/// [`zero_keys`] of one more slot, `out[n]`, whose token `z1` holds under
/// key `base + n`, after `out[..n]`, which `out` keeps from `o1`.
proof fn lemma_zero_keys_push(z0: Map<int, Zeroed>, z1: Map<int, Zeroed>, o1: Seq<*mut u8>, out: Seq<*mut u8>, base: int, n: int, size: int)
    requires
        0 <= n < o1.len(),
        zero_keys(z0, o1, base, 0, n, size),
        out == o1.update(n, out[n]),
        z1 == z0.insert(base + n, z1[base + n]),
        z1[base + n].covers(out[n], size),
    ensures
        zero_keys(z1, out, base, 0, n + 1, size),
{
    assert forall|j: int| #![trigger z1.contains_key(base + j)] #![trigger z1[base + j]] 0 <= j < n + 1 implies z1.contains_key(base + j) && z1[base + j].covers(out[j], size) by {
        if j < n {
            assert(out[j] == o1[j]);
            assert(z0.contains_key(base + j));
        }
    }
}

/// One entry of [`zero_keys`], for callers whose `base` is a literal
/// (the quantifier's trigger, `base + i`, does not match there).
proof fn lemma_zero_keys_at(z: Map<int, Zeroed>, out: Seq<*mut u8>, base: int, lo: int, hi: int, size: int, i: int)
    requires
        zero_keys(z, out, base, lo, hi, size),
        lo <= i < hi,
    ensures
        z.contains_key(base + i),
        z[base + i].covers(out[i], size),
{
    assert(z.contains_key(base + i));
}

/// [`zero_keys`] at base 0 from its entries keyed `i` (the quantifier's
/// trigger, `0 + i`, need not match a caller's `i`).
proof fn lemma_zero_keys_from0(z: Map<int, Zeroed>, out: Seq<*mut u8>, hi: int, size: int)
    requires
        forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < hi ==> z.contains_key(i) && z[i].covers(out[i], size),
    ensures
        zero_keys(z, out, 0, 0, hi, size),
{
    assert forall|i: int| #![trigger z.contains_key(0 + i)] #![trigger z[0 + i]] 0 <= i < hi implies z.contains_key(0 + i) && z[0 + i].covers(out[i], size) by {
        assert(0 + i == i);
        assert(z.contains_key(i));
        assert(z[i].covers(out[i], size));
    }
}

/// Every key of `new` outside `lo..hi` is as in `old`.
pub open spec fn zero_frame(new: Map<int, Zeroed>, old: Map<int, Zeroed>, lo: int, hi: int) -> bool {
    forall|k: int| #![trigger new.contains_key(k)] #![trigger old.contains_key(k)] !(lo <= k < hi) ==> {
        &&& new.contains_key(k) == old.contains_key(k)
        &&& old.contains_key(k) ==> new[k] == old[k]
    }
}

/// Slot `k + 1` of a slab starts `size` bytes after slot `k`, for
/// `k + 1 < sb`.
proof fn lemma_next_slot_addr(s: Slots, slab: int, k: int)
    requires
        s.wf(),
        0 <= slab,
        0 <= k,
        k + 1 < s.sb,
    ensures
        s.addr(slab * s.sb + k + 1) == s.addr(slab * s.sb + k) + s.size,
{
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(slab * s.sb + k, s.sb, slab, k);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(slab * s.sb + k + 1, s.sb, slab, k + 1);
    assert((k + 1) * s.size == k * s.size + s.size) by (nonlinear_arith);
}

/// A bit that is a slot of a slab that is not a guard slab is at a slot
/// start of the class (`Regions::is_slot`).
proof fn lemma_real_is_slot(t: SlabTable, b: int, p: usize)
    requires
        t.inv(),
        t.spec_slots().real(b),
        !t.spec_is_guard((b / t.spec_slots().sb) as usize),
        p as int == t.spec_slots().addr(b),
    ensures
        process_regions().is_slot(t.spec_class() as int, p),
{
    t.lemma_slots_wf();
    let s = t.spec_slots();
    let (slab, slot) = (b / s.sb, b % s.sb);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b, s.sb);
    vstd::arithmetic::div_mod::lemma_mod_bound(b, s.sb);
    vstd::arithmetic::div_mod::lemma_div_pos_is_pos(b, s.sb);
    assert(b == s.sb * slab + slot);
    assert(slab < t.spec_nslabs()) by (nonlinear_arith)
        requires b == s.sb * slab + slot, 0 <= slot, b < s.nbits, s.nbits == t.spec_nslabs() * s.sb, 0 < s.sb;
    t.lemma_slot_bit(slab, slot);
    assert(slab * s.sb + slot == b) by (nonlinear_arith)
        requires b == s.sb * slab + slot;
    assert(p as int == process_regions().slot_at(t.spec_class() as int, slab, slot));
    t.lemma_guard_slab(slab);
    process_regions().lemma_is_slot(t.spec_class() as int, slab, slot, p);
}

/// Claimed bits, which are all slots: the entries are slot starts
/// (`Regions::is_slot`) and `toks` holds the token of each.
#[verifier::spinoff_prover]
proof fn lemma_holds_real(t: SlabTable, toks: SlotToks, out: Seq<*mut u8>, base: int, n: int)
    requires
        t.inv(),
        0 <= n <= out.len(),
        toks.holds(t, out, base, 0, n),
    ensures
        toks.owns(t, out, base, 0, n),
        slots_in(out, t.spec_class() as int, 0, n),
{
    assert forall|i: int| 0 <= i < n implies process_regions().is_slot(t.spec_class() as int, (#[trigger] out[i])@.addr) by {
        assert(toks.entry(t, out[i], base + i));
        lemma_real_is_slot(t, toks.bits[base + i], out[i]@.addr);
    }
    toks.lemma_owns_of_real(t, out, base, n);
}

/// Taking pick `lo`'s tokens out leaves those of picks `lo + 1..hi`.
proof fn lemma_picks_toks_rest(
    t: SlabTable,
    p: Seq<Pick>,
    lo: int,
    hi: int,
    t0: Map<int, GhostSubset<int>>,
    t1: Map<int, GhostSubset<int>>,
)
    requires
        picks_toks(t, p, lo, hi, t0),
        t1 == t0.remove(lo),
    ensures
        picks_toks(t, p, lo + 1, hi, t1),
{
    assert forall|j: int| #![trigger t1.contains_key(j)] #![trigger t1[j]] lo + 1 <= j < hi implies {
        &&& t1.contains_key(j)
        &&& t1[j].id() == t.spec_slots().loc
        &&& t1[j]@ == pick_addrs(t, p[j])
    } by {
        assert(t0.contains_key(j));
    }
}

/// Taking slot `b` back out of a set of slots leaves the others' addresses.
pub proof fn lemma_addrs_remove(s: Slots, rest: VSet<int>, b: int)
    requires
        s.wf(),
        forall|x: int| #[trigger] rest.contains(x) ==> s.real(x),
        s.real(b),
        !rest.contains(b),
    ensures
        !addrs(s, rest).contains(s.addr(b)),
        addrs(s, rest.insert(b)).remove(s.addr(b)) == addrs(s, rest),
{
    broadcast use VSet::lemma_map_contains;
    if addrs(s, rest).contains(s.addr(b)) {
        let x = choose|x: int| rest.contains(x) && s.addr(b) == s.addr(x);
        crate::bitmap::lemma_addr_inj(s, x, b);
    }
    lemma_addrs_insert(s, rest, b);
    assert(addrs(s, rest).insert(s.addr(b)).remove(s.addr(b)) =~= addrs(s, rest));
}

/// Taking the lowest bit `b` off mask `m0` takes its slot (if it is one)
/// off the mask's slots.
proof fn lemma_word_pop(s: Slots, f: int, w: int, m0: u64, b: u64)
    requires
        m0 != 0,
        b == u64_trailing_zeros(m0),
    ensures
        !mask_bits(s, f, w, m0 & ((m0 - 1) as u64)).contains(f + 64 * w + b),
        mask_bits(s, f, w, m0) == if s.real(f + 64 * w + b) {
            mask_bits(s, f, w, m0 & ((m0 - 1) as u64)).insert(f + 64 * w + b)
        } else {
            mask_bits(s, f, w, m0 & ((m0 - 1) as u64))
        },
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
    crate::bitmap::lemma_clear_lowest(m0);
    crate::bitmap::lemma_tz(m0);
    let m = m0 & ((m0 - 1) as u64);
    let nb = f + 64 * w + b;
    let rhs = if s.real(nb) { mask_bits(s, f, w, m).insert(nb) } else { mask_bits(s, f, w, m) };
    assert forall|x: int| #[trigger] mask_bits(s, f, w, m0).contains(x) == rhs.contains(x) by {
        if f + 64 * w <= x < f + 64 * w + 64 {
            let k = (x - f - 64 * w) as u64;
            assert(has_bit(m, k) == (has_bit(m0, k) && k != b));
        }
    }
    assert(mask_bits(s, f, w, m0) =~= rhs);
}

/// The addresses of a batch of slot pointers, as the tokens name them.
pub open spec fn ptr_addrs(s: Seq<*mut u8>) -> Seq<int> {
    Seq::new(s.len(), |i: int| s[i]@.addr as int)
}

/// `m` holds the token of `a[i]` under key `i`, of authority `loc`, for
/// every `lo <= i < hi`.
pub open spec fn keyed(m: Map<int, GhostSingleton<int>>, a: Seq<int>, loc: vstd::resource::Loc, lo: int, hi: int) -> bool {
    forall|i: int| #![trigger m.contains_key(i)] #![trigger m[i]] lo <= i < hi ==> {
        &&& m.contains_key(i)
        &&& m[i].id() == loc
        &&& m[i]@ == a[i]
    }
}

/// [`keyed`] at one key `i` of `lo..hi`.
proof fn lemma_keyed_at(m: Map<int, GhostSingleton<int>>, a: Seq<int>, loc: vstd::resource::Loc, lo: int, hi: int, i: int)
    requires
        keyed(m, a, loc, lo, hi),
        lo <= i < hi,
    ensures
        m.contains_key(i),
        m[i].id() == loc,
        m[i]@ == a[i],
{
    assert(m.contains_key(i));
}

/// Every key of `new` outside `lo..hi` is as in `old`.
pub open spec fn keyed_frame(new: Map<int, GhostSingleton<int>>, old: Map<int, GhostSingleton<int>>, lo: int, hi: int) -> bool {
    forall|k: int| #![trigger new.contains_key(k)] #![trigger old.contains_key(k)] !(lo <= k < hi) ==> {
        &&& new.contains_key(k) == old.contains_key(k)
        &&& old.contains_key(k) ==> new[k] == old[k]
    }
}

} // verus!

verus! {

/// Slabs one lock hold of a refill may reserve from (see
/// [`ClassState::plan`]), and slabs one lock hold of a release batch may
/// reopen ([`Reopen`]). Sized so that the common refill and flush of every
/// class fit in one hold: with the 4 KiB slabs of the 512 and 1024-byte
/// classes (`layout::LG_MIN_SLAB`) a 128-slot refill of the 512 class and a
/// 64-slot one of the 1024 class each span 16 slabs.
pub(crate) const MAX_PICKS: usize = 16;

} // verus!

/// The atomic steps of the protocol, named in the order the code takes
/// them, for the model (`model::steps`): each enum is one operation of
/// [`ClassState`], each variant one step that interleaves with other
/// threads' steps (a lock hold that only touches lock-protected state is
/// one step; a lock-free atomic is one step), with its discriminant the
/// model's sub-step number. The model matches every enum exhaustively,
/// so a step added here without a model step, or a reordering that
/// leaves the model's numbering behind, does not compile. Kept next to
/// the code it names: a change to the protocol is a change here first.
#[cfg(test)]
pub(crate) mod protocol {
    /// [`claim`](super::ClassState::claim): one round of `claim_with`'s
    /// loop (`plan`, then `finish`; short, plan again).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Refill {
        /// `plan`: `self.inner.lock()`.
        Lock = 0,
        /// `plan` under the lock, one step: the partial walk
        /// (`clear_partial` of a full slab), `next_slab` (the empty and
        /// purged sets, the group cursor with `commit_run`, `take_group`),
        /// and `take`
        /// per slab: the `live` reservation, the `Active` transition, the
        /// partial bit, `claim_prefix` for a zero pick.
        PlanLocked = 1,
        /// `plan` returns: the lock hold ends.
        Unlock = 2,
        /// `finish`, one pick at a time: a zero pick's slots `0..n` as
        /// claimed, else one `claim_from` (`fetch_or`) on the slab's word.
        Finish = 3,
    }

    /// [`release_owned`](super::ClassState::release_owned) of a batch
    /// grouped by slab, through `Heap::release_routed` (one run per owner
    /// arena), with `dec_live`, `on_live_zero` and `reopen`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Release {
        /// The batch grouped by slab (`Heap::flush`'s bucket pass),
        /// before anything is touched.
        Group = 0,
        /// `release_word`: the owner word read, the descriptor's arena
        /// checked against it, the `Fresh` check, `bits(slab).release(word,
        /// mask)` (the `fetch_and`) and the double-free check on what it
        /// returned.
        FetchAnd = 1,
        /// `dec_live`: `sub_live(n)`, and whether it reached zero.
        SubLive = 2,
        /// `on_live_zero`: `self.inner.lock()`.
        LockLiveZero = 3,
        /// `on_live_zero` under the lock: the owner test, `Active` with
        /// `live == 0` to `Empty` and into the empty set (`empty_add`),
        /// unlock.
        OnLiveZero = 4,
        /// `release_owned` runs on to the next slab of the batch while it
        /// is the same owner's, else the run is done and `reopen` is
        /// pending for it.
        NextSlab = 5,
        /// `reopen`: `self.inner.lock()`.
        LockReopen = 6,
        /// `reopen` under the lock: per slab whose word was full, the
        /// `Active` and owner test and `set_partial`; unlock.
        Reopen = 7,
    }

    /// [`purge_empty`](super::ClassState::purge_empty).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u8)]
    pub(crate) enum Purge {
        /// `self.inner.lock()`.
        Lock = 0,
        /// Under the lock, one step: `walk_empty` (the state assertion),
        /// `push_group`, the discard (`batch.flush()`) and its `settle`
        /// (`Purged`, out of `empty`, into `purged`), unlock.
        PurgeLocked = 1,
    }

    impl Refill {
        /// The step at sub-step `sub` (`Finish` repeats, one pick at a time).
        pub(crate) fn at(sub: u8) -> Refill {
            match sub {
                0 => Refill::Lock,
                1 => Refill::PlanLocked,
                2 => Refill::Unlock,
                3 => Refill::Finish,
                _ => panic!("protocol: Refill has no step {sub}"),
            }
        }
    }

    impl Release {
        pub(crate) fn at(sub: u8) -> Release {
            match sub {
                0 => Release::Group,
                1 => Release::FetchAnd,
                2 => Release::SubLive,
                3 => Release::LockLiveZero,
                4 => Release::OnLiveZero,
                5 => Release::NextSlab,
                6 => Release::LockReopen,
                7 => Release::Reopen,
                _ => panic!("protocol: Release has no step {sub}"),
            }
        }
    }

    impl Purge {
        pub(crate) fn at(sub: u8) -> Purge {
            match sub {
                0 => Purge::Lock,
                1 => Purge::PurgeLocked,
                _ => panic!("protocol: Purge has no step {sub}"),
            }
        }
    }
}

verus! {

/// Full bitmap scans of one slab that find nothing before a claim gives up
/// on its reservation and aborts. A scan comes up empty only while other
/// threads keep moving free bits between words, so this many in a row
/// means the `live` count no longer bounds the set bits.
const MAX_EMPTY_PASSES: u32 = 1 << 22;

/// One slab's share of a refill: `n` slots reserved in its `live` count.
#[derive(Copy, Clone)]
struct Pick {
    slab: u32,
    n: u32,
    /// The slab was `Fresh` or `Purged` (and zero) when this refill made
    /// it `Active`, and bits `0..n` were claimed under the lock in `take`
    /// ([`ClassState::claim_prefix`]): `finish` hands out slots `0..n`
    /// without the bit loop.
    zero: bool,
    /// The slab's state when this refill selected it (for `check_zero`).
    state: State,
}

/// Slots reserved by the first `k` picks.
spec fn picks_sum(p: Seq<Pick>, k: int) -> int
    decreases k,
{
    if k <= 0 { 0 } else { picks_sum(p, k - 1) + p[k - 1].n }
}

/// The first `np` picks name slabs below `nslabs` and reserve at most a
/// slab's `nslots` each.
spec fn picks_ok(p: Seq<Pick>, np: int, nslabs: usize, nslots: nat) -> bool {
    &&& 0 <= np <= p.len()
    &&& forall|i: int| #![trigger p[i]] 0 <= i < np ==> (p[i].slab as usize) < nslabs && p[i].n <= nslots
}

/// `picks_sum` of a prefix depends only on that prefix, and is not negative.
proof fn lemma_picks_sum(a: Seq<Pick>, b: Seq<Pick>, k: int)
    requires
        0 <= k <= a.len(),
        k <= b.len(),
        forall|i: int| 0 <= i < k ==> a[i] == b[i],
    ensures
        picks_sum(a, k) == picks_sum(b, k),
        picks_sum(a, k) >= 0,
    decreases k,
{
    if k > 0 {
        lemma_picks_sum(a, b, k - 1);
    }
}

} // verus!

#[cfg(test)]
impl Pick {
    /// Tests: an unused entry of a plan's pick array (no slab, no slots).
    /// `claim_with` spells the same literal out: written as this const
    /// there, the array fill compiles to one instruction fewer
    /// (`scripts/codegen-diff.sh`), which is a code change, not a cleanup.
    const EMPTY: Pick = Pick {
        slab: 0,
        n: 0,
        zero: false,
        state: State::Active,
    };
}

verus! {

/// Slabs a release batch must put back in the partial set. Consecutive
/// duplicates are dropped; a full list is flushed under the lock.
struct Reopen {
    slabs: [u32; MAX_PICKS],
    len: usize,
}

impl Reopen {
    #[inline(always)]
    fn new() -> (r: Reopen)
        ensures
            r.len == 0,
    {
        Reopen {
            slabs: [0; MAX_PICKS],
            len: 0,
        }
    }

    /// Every recorded slab is below `n`.
    spec fn below(&self, n: usize) -> bool {
        forall|i: int| 0 <= i < self.len ==> (#[trigger] self.slabs[i]) < n
    }

    /// Record `slab`; `false` if the list is full (and `slab` is not the
    /// last entry), in which case the caller must flush first.
    fn push(&mut self, slab: usize) -> (r: bool)
        requires
            old(self).len <= MAX_PICKS,
        ensures
            final(self).len <= MAX_PICKS,
            r ==> final(self).len > 0 && final(self).slabs[final(self).len - 1] == slab as u32,
            !r ==> final(self).len == MAX_PICKS && *final(self) == *old(self),
            forall|n: usize| old(self).below(n) && slab < n ==> #[trigger] final(self).below(n),
    {
        if self.len > 0 && self.slabs[self.len - 1] == slab as u32 {
            return true;
        }
        if self.len == MAX_PICKS {
            return false;
        }
        self.slabs[self.len] = slab as u32;
        self.len += 1;
        true
    }
}

struct Inner {
    partial: Set,
    empty: Set,
    purged: Set,
    stats: ClassStats,
    /// This arena's current slab group: `group_next..group_end` are handed
    /// to it and not yet handed out; the rest of the run that committed
    /// the slab before `group_next` is already read-write (see
    /// [`ClassState::next_fresh_slab`]).
    group_next: usize,
    group_end: usize,
    /// Ghost: the slabs this arena owns (their group's owner word names
    /// this arena, guard slabs included), each with the state its
    /// descriptor holds while this lock is held. The accessors that tie it
    /// to the descriptors (`state_locked` and the others) are unverified;
    /// the descriptor permissions ([`Inner::dperms`]) record it.
    #[allow(dead_code)]
    states: Ghost<Map<nat, State>>,
    /// Ghost: this arena's committed slabs of this class (guards
    /// excluded), the slabs `stats.committed` counts.
    #[allow(dead_code)]
    committed: Ghost<vstd::set::Set<nat>>,
    /// Ghost: the `dirty_at` stamp each owned slab's descriptor holds
    /// while this lock is held.
    #[allow(dead_code)]
    dirty: Ghost<Map<nat, u64>>,
    /// Ghost: the arena and the class table this lock value belongs to
    /// (set by `new`, never changed).
    #[allow(dead_code)]
    arena: Ghost<u16>,
    #[allow(dead_code)]
    table: Ghost<SlabTable>,
    /// Ghost: the fresh markers of every slab this arena owns but its
    /// guards, which `AtomicBitmap::init` returned when `take_group`
    /// handed the slab's group out: what a claim from the slab needs
    /// (`AtomicBitmap::inited_by`); and the init markers of its guard
    /// slabs, which `take_group` took as they were (`guard_marked`).
    #[allow(dead_code)]
    inited: Tracked<GhostPersistentSubset<int>>,
    /// Ghost: the permissions ([`DescPerm`]) of the descriptors of the
    /// slabs this arena owns, which `take_group` handed over with the
    /// slabs; each records the state and stamp `states` and `dirty` hold,
    /// and, but for a guard slab, this arena ([`descs_ok`]).
    #[allow(dead_code)]
    dperms: Tracked<Map<nat, DescPerm>>,
    /// Ghost: the authority of the record of this state's owned slabs,
    /// whose every element is an owned slab, at a location of this state
    /// alone (minted by `new`). `take_group` adds a group's slabs as it
    /// stores the group's owner word, and keeps their persistent part in
    /// the word's invariant (`table::OwnerPred`), for the owner checks to
    /// tell that a group whose owner word names this arena is this
    /// state's.
    #[allow(dead_code)]
    oauth: Tracked<GhostSetAuth<int>>,
}

} // verus!

// SAFETY: `TwoLevelBitmap` holds a raw pointer into the metadata region,
// which is process-global and only ever accessed through this lock.
unsafe impl Send for Inner {}

verus! {

/// State of one `(arena, class)` pair: the three slab sets and their
/// statistics behind the class lock, plus a pointer to the class's shared
/// [`SlabTable`].
///
/// Layout matters: the fields before `inner` are written once by `new` and
/// read by every refill and flush, while `inner` holds the class lock and
/// the state every refill and flush writes. `repr(C)` keeps the read-mostly
/// fields first and [`Padded`] starts `inner` on its own line, so a lock
/// acquisition on one core does not evict the table pointer from the
/// others. The alignment makes `[ClassState; NCLASSES]` line-granular too.
///
/// The stride of that array must not be a multiple of 512 bytes. With 4 KiB
/// pages a 512-byte stride puts the hot line of every class on the same
/// handful of L1 sets: on Apple silicon (128-byte lines, sets indexed by
/// page-offset bits) single-threaded `alloc-test` slowed from 2.13 s to
/// 2.52 s. `_tail` makes the stride 640 = 5 lines, which visits every set.
#[repr(C, align(128))]
pub struct ClassState {
    class: u16,
    arena: u16,
    /// The class's shared table, which lives as long as the allocator
    /// (inside `Shared`, or leaked by the tests).
    table: TablePtr,
    /// The decay step gate (`purge::Decayer`): the oldest `Empty` slab's
    /// `dirty_at`, written under the lock (set when the empty set goes
    /// from none to one, recomputed by every purge walk) and read without
    /// it by the decay driver, and the next walk's deadline.
    gate: Decayer,
    inner: Padded<Lock<Inner>>,
    /// Slots ever reserved by `take` for this arena (written only under
    /// the class lock). With `released` below it gives `curregs`, the
    /// number of slots out of the bitmaps (in caches or in use), as
    /// `claimed - released`: that is the sum of `live` over this arena's
    /// slabs without a walk over the descriptors or a lock per flush. The
    /// two counters share the last line of the state, which nothing else
    /// touches: a refill writes `claimed` while it holds the lock and a
    /// flush adds to `released` once per batch (`stats.rs`).
    claimed: AtomicU64,
    /// Slots ever released to this arena's bitmaps, one add per batch.
    released: AtomicU64,
    /// Slots claimed by cache *refills* (`Heap::refill`, one add per
    /// batch): the part of `claimed` that ordinary allocation traffic
    /// accounts for, as opposed to compaction rebuilds and direct
    /// (`MALLOCX_TCACHE_NONE`) claims such as Valkey's defragmenter's. The
    /// compaction gate in `heap/` watches its rate.
    refilled: AtomicU64,
    /// Compaction diagnostics, printed in the bins table of
    /// `malloc_stats_print`: slots claimed by
    /// compaction rebuilds, caches that entered and left compact mode, and
    /// probes the refill-rate gate declined. All written on cold paths.
    compacted: AtomicU64,
    compact_on: AtomicU64,
    compact_off: AtomicU64,
    gate_refill: AtomicU64,
    /// Times `bin_stats` clamped `claimed - released` to the capacity of
    /// the slabs that can hold live slots. A transient clamp is legitimate
    /// (`released` is added to after the slab's `live` reached zero, so a
    /// reader between the two sees an `Empty` slab and a count that still
    /// includes its slots); a count that keeps growing while nothing is
    /// freed is a bookkeeping bug. Printed in the bins table.
    clamped: AtomicU64,
    _tail: [u8; PAD_ALIGN - 64],
}

} // verus!

const _: () = assert!(core::mem::offset_of!(ClassState, inner) == PAD_ALIGN);
const _: () =
    assert!(core::mem::offset_of!(ClassState, gate) + core::mem::size_of::<Decayer>() <= PAD_ALIGN);
const _: () = assert!(core::mem::offset_of!(ClassState, claimed) % PAD_ALIGN == 0);
const _: () = assert!(core::mem::size_of::<ClassState>().is_multiple_of(PAD_ALIGN));
const _: () = assert!(!core::mem::size_of::<ClassState>().is_multiple_of(512));
const _: () = assert!(core::mem::size_of::<TablePtr>() == core::mem::size_of::<*const SlabTable>());

// SAFETY: the only non-`Sync`, non-`Send` field is the table pointer
// (`TablePtr`, whose other fields are ghost), which targets a `SlabTable`
// (itself `Sync`) that outlives every `ClassState` pointing at it: both
// live in `Shared` for the life of the process, or are leaked by the
// tests. Sharing the state across threads (`Sync`) or handing it to
// another thread (`Send`) therefore hands out nothing but a pointer to a
// process-lifetime, thread-safe table.
unsafe impl Sync for ClassState {}
unsafe impl Send for ClassState {}

verus! {

/// The tables of every class, from `base` on, through the frozen
/// permissions `m`, one per element: each initialised, at its index's
/// address, and the table of the class it is the index of. What a
/// [`ClassState`] keeps of its class's table ([`TablePtr`]).
pub open spec fn tables_ok(m: Map<int, PointsTo<SlabTable>>, base: *mut SlabTable) -> bool {
    &&& forall|c: int| 0 <= c < NCLASSES ==> #[trigger] m.contains_key(c)
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).ptr() == crate::vspec::ptr::ptr_add(base, c as usize)
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).is_init()
    &&& forall|c: int| 0 <= c < NCLASSES ==> (#[trigger] m[c]).value().spec_class() == c
    &&& crate::vspec::ptr::in_alloc(base, NCLASSES as int * vstd::layout::size_of::<SlabTable>())
    &&& NCLASSES * vstd::layout::size_of::<SlabTable>() <= isize::MAX
}

} // verus!

/// The pointer to its table a [`ClassState`] keeps, in a module of its own
/// so that [`table_ptr`](tptr::table_ptr) is the only maker of one.
mod tptr {
    use super::SlabTable;
    #[cfg(verus_keep_ghost)]
    use super::tables_ok;
    #[cfg(verus_keep_ghost)]
    use crate::layout::NCLASSES;
    use vstd::prelude::*;
    use vstd::raw_ptr::PointsTo;

    verus! {

    /// A pointer to the table of class `c`, with (ghost, no bytes: the
    /// layout is the pointer's) the frozen permissions to every class's
    /// table, from `base` on ([`tables_ok`]), whose element `c` is the
    /// pointer's.
    pub(super) struct TablePtr {
        p: *mut SlabTable,
        #[allow(dead_code)]
        tables: Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        /// Element `c` of `tables`, held apart so that [`TablePtr::get`]
        /// passes it on as it is: a `Tracked(..)` built there is one more
        /// distinct `Tracked` type under `Heap::flush`, which puts it past
        /// the MIR inliner's limit of instances of one generic item and
        /// changes its machine code (x86_64).
        perm: Tracked<&'static PointsTo<SlabTable>>,
        #[allow(dead_code)]
        base: Ghost<*mut SlabTable>,
        #[allow(dead_code)]
        c: Ghost<int>,
    }

    impl TablePtr {
        /// The pointer is to element `c` of the tables its permissions
        /// cover (what [`table_ptr`] requires).
        #[verifier::type_invariant]
        spec fn inv(self) -> bool {
            &&& tables_ok(*self.tables@, self.base@)
            &&& 0 <= self.c@ < NCLASSES
            &&& self.tables@[self.c@].ptr() == self.p
            &&& *self.perm@ == self.tables@[self.c@]
        }
    }

    /// The table `p` points at.
    pub(super) closed spec fn table_at(p: TablePtr) -> SlabTable {
        p.tables@[p.c@].value()
    }

    /// The class whose table `p` points at.
    pub(super) closed spec fn index_of(p: TablePtr) -> int {
        p.c@
    }

    /// The pointer `p` to element `c` of the tables `tables` covers, from
    /// `base` on, as a [`TablePtr`], which [`table_at`] maps to that
    /// table.
    #[inline(always)]
    pub(super) fn table_ptr(
        p: *mut SlabTable,
        Tracked(tables): Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        Ghost(base): Ghost<*mut SlabTable>,
        Ghost(c): Ghost<int>,
    ) -> (r: TablePtr)
        requires
            tables_ok(*tables, base),
            0 <= c < NCLASSES,
            tables[c].ptr() == p,
        ensures
            table_at(r) == tables[c].value(),
            index_of(r) == c,
    {
        TablePtr {
            p,
            tables: Tracked(tables),
            perm: Tracked(tables.tracked_borrow(c)),
            base: Ghost(base),
            c: Ghost(c),
        }
    }

    impl TablePtr {
        /// The table, through its frozen permission.
        #[inline(always)]
        pub(super) fn get(&self) -> (r: &SlabTable)
            ensures
                *r == table_at(*self),
        {
            proof {
                use_type_invariant(self);
            }
            vstd::raw_ptr::ptr_ref(self.p, self.perm)
        }

        /// The first class's table and the permissions to all of them
        /// ([`tables_ok`]), from the pointer to it.
        #[inline(always)]
        pub(super) fn tables(&self) -> (r: (*mut SlabTable, Tracked<&'static Map<int, PointsTo<SlabTable>>>))
            requires
                index_of(*self) == 0,
            ensures
                tables_ok(*r.1@, r.0),
        {
            proof {
                use_type_invariant(self);
                crate::vspec::ptr::axiom_ptr_add_addr(self.base@, 0);
                assert(self.p@ == self.base@@);
            }
            (self.p, Tracked(*self.tables.borrow()))
        }
    }

    } // verus!
}

use tptr::{TablePtr, table_ptr};
#[cfg(verus_keep_ghost)]
use tptr::{index_of, table_at};

verus! {

/// A set of the class state: a well-formed bitmap over `n` slabs whose
/// members are slab numbers below `n`.
spec fn set_wf(s: Set, n: nat) -> bool {
    &&& s.map.wf()
    &&& s.map.spec_nbits() == n
    &&& forall|b: nat| #[trigger] s.map@.contains(b) ==> b < n
}

/// Owned slab `s`'s descriptor permission in `dperms` is to its
/// descriptor in `t`, and records the ghost state `st` and stamp `dirty`
/// and, unless `s` is a guard slab (whose descriptor stays zero), the
/// arena `arena`.
spec fn desc_ok(dperms: Map<nat, DescPerm>, t: SlabTable, arena: u16, s: nat, st: State, dirty: u64) -> bool {
    &&& dperms.contains_key(s)
    &&& dperms[s].id() == desc_id(&desc_at(t, s as usize))
    &&& dperms[s].state() == state_byte(st)
    &&& dperms[s].dirty() == dirty
    &&& !t.spec_is_guard(s as usize) ==> dperms[s].arena() == arena
}

/// [`desc_ok`] at every slab `states` owns: the class lock value holds
/// the permission of every owned slab's descriptor, which agrees with
/// its ghost. Opaque ([`Inner::lemma_desc`] reads it at one slab); a
/// function of the fields it reads, so that a write of any other field
/// keeps it.
#[verifier::opaque]
spec fn descs_ok(states: Map<nat, State>, dirty: Map<nat, u64>, dperms: Map<nat, DescPerm>, t: SlabTable, arena: u16) -> bool {
    forall|s: nat| #[trigger] states.contains_key(s) ==> desc_ok(dperms, t, arena, s, states[s], dirty[s])
}

/// Every slab the record `rec` ([`Inner::oauth`]) has is one `states`
/// owns: only which slabs are owned matters, as for [`whole_groups`].
spec fn owned_rec(rec: vstd::set::Set<int>, states: Map<nat, State>) -> bool {
    &&& forall|x: int| #[trigger] rec.contains(x) ==> 0 <= x
    &&& forall|x: int| #[trigger] rec.contains(x) ==> states.contains_key(x as nat)
}

/// `owned` is whole hand-out groups of `t`: with a slab, every slab of its
/// group ([`SlabTable::spec_group_range`]). `take_group` hands an arena a
/// whole group, which `next_fresh_slab` adds to the ghost at once, and no
/// owned slab leaves. Opaque ([`lemma_whole_group`] reads it at two
/// slabs); a function of the owned set, so that a write of an owned
/// slab's state keeps it.
#[verifier::opaque]
spec fn whole_groups(owned: vstd::set::Set<nat>, t: SlabTable) -> bool {
    forall|x: nat, y: nat| #![trigger owned.contains(x), owned.contains(y)]
        owned.contains(x) && t.spec_group_range((x as usize) >> t.lg_group).0 <= y
            && y < t.spec_group_range((x as usize) >> t.lg_group).1 ==> owned.contains(y)
}

/// [`whole_groups`] at `x` and `y`: a slab `y` of the hand-out group of
/// an owned slab `x` is owned.
proof fn lemma_whole_group(owned: vstd::set::Set<nat>, t: SlabTable, x: nat, y: nat)
    requires
        whole_groups(owned, t),
        owned.contains(x),
        t.spec_group_range((x as usize) >> t.lg_group).0 <= y,
        y < t.spec_group_range((x as usize) >> t.lg_group).1,
    ensures
        owned.contains(y),
{
    reveal(whole_groups);
}

/// [`state_byte`] is one to one.
proof fn lemma_state_byte_inj(a: State, b: State)
    ensures
        state_byte(a) == state_byte(b) ==> a == b,
{
}

/// [`Inner::descs`] after a write of owned slab `k`'s descriptor: `g1`
/// is `g0` with `k`'s state, stamp and permission replaced in agreement.
proof fn lemma_descs_update(g0: Inner, g1: Inner, k: nat)
    requires
        g0.descs(),
        g0.owns(k),
        g1.states@.dom() == g0.states@.dom(),
        forall|s: nat| s != k ==> #[trigger] g1.states@[s] == g0.states@[s],
        forall|s: nat| s != k ==> #[trigger] g1.dirty@[s] == g0.dirty@[s],
        forall|s: nat| s != k ==> #[trigger] g1.dperms@[s] == g0.dperms@[s],
        forall|s: nat| s != k && #[trigger] g0.dperms@.contains_key(s) ==> g1.dperms@.contains_key(s),
        g1.table == g0.table,
        g1.arena == g0.arena,
        desc_ok(g1.dperms@, g1.table@, g1.arena@, k, g1.st(k), g1.dirty@[k]),
    ensures
        g1.descs(),
{
    reveal(descs_ok);
    assert forall|s: nat| #[trigger] g1.states@.contains_key(s)
        implies desc_ok(g1.dperms@, g1.table@, g1.arena@, s, g1.states@[s], g1.dirty@[s]) by {
        if s != k {
            assert(g0.states@.contains_key(s));
            assert(g0.dperms@.contains_key(s));
        }
    }
}

/// A set of the class state that lacks slab `x` holds fewer than `n` slabs.
proof fn lemma_set_len(s: Set, n: nat, x: nat)
    requires
        set_wf(s, n),
        x < n,
        !s.map@.contains(x),
    ensures
        s.map@.len() < n,
{
    broadcast use vstd::set_lib::range_set_properties;
    let r = vstd::set::Set::<nat>::range(0, n).remove(x);
    assert(s.map@.subset_of(r));
    vstd::set_lib::lemma_len_subset(s.map@, r);
}

/// The set/count seam: each of the three sets is one bitmap plus one
/// `ClassStats` count, and the two are written together, here or at the
/// two sites that spell the pair out and say why (`next_slab`,
/// `settle`), so `nonfull`, `empty` and `purged` are always the
/// population of their set. `partial` is idempotent (a release batch may
/// name a slab whose bit a refill set meanwhile); `empty` and `purged`
/// are exact: a slab enters when its state changes and leaves when a
/// refill takes it or a purge moves it, so a count that would go below
/// zero is a heap inconsistency and aborts (`slab_count_underflow`).
impl Inner {
    /// Slabs of the class (the sets' size).
    spec fn nbits(&self) -> nat {
        self.partial.map.spec_nbits()
    }

    /// Slab `s` belongs to this arena (ghost).
    spec fn owns(&self, s: nat) -> bool {
        self.states@.contains_key(s)
    }

    /// The state of owned slab `s` (ghost).
    spec fn st(&self, s: nat) -> State {
        self.states@[s]
    }

    /// The value holds the agreeing permission of every owned slab's
    /// descriptor ([`descs_ok`]).
    spec fn descs(&self) -> bool {
        descs_ok(self.states@, self.dirty@, self.dperms@, self.table@, self.arena@)
    }

    /// [`descs`](Inner::descs) at owned slab `s`.
    proof fn lemma_desc(&self, s: nat)
        requires
            self.descs(),
            self.owns(s),
        ensures
            desc_ok(self.dperms@, self.table@, self.arena@, s, self.st(s), self.dirty@[s]),
    {
        reveal(descs_ok);
    }

    /// The seam's counts and shapes: three well-formed sets over the
    /// class's slabs, each a view of the shared set table over a group of
    /// the class's slab count ([`Set::shared_over`]) and each count the
    /// population of its set, the committed count the size of the
    /// committed set, the owned and committed slabs among the class's (at
    /// most `2^52` of them), and the current group's cursor inside the
    /// class and, while slabs are left in it, inside one hand-out group,
    /// whose end is the current group's end; the slab before the current
    /// group's end is owned, and each owned slab's markers are held (fresh
    /// ones from `init`, a guard slab's init markers), which puts both
    /// below the watermark (`SlabTable::next_slab_locked`); every owned
    /// slab's owner ([`slab_owner`]) is this arena, the owned slabs being
    /// whole hand-out groups ([`whole_groups`]); the value holds every
    /// owned slab's descriptor permission, which agrees with the ghost
    /// ([`Inner::descs`]); and the owned-slab record has owned slabs only
    /// ([`owned_rec`]).
    spec fn base(&self) -> bool {
        &&& set_wf(self.partial, self.nbits())
        &&& set_wf(self.empty, self.nbits())
        &&& set_wf(self.purged, self.nbits())
        &&& self.partial.shared_over(self.nbits())
        &&& self.empty.shared_over(self.nbits())
        &&& self.purged.shared_over(self.nbits())
        &&& self.stats.nonfull == self.partial.map@.len()
        &&& self.stats.empty == self.empty.map@.len()
        &&& self.stats.purged == self.purged.map@.len()
        &&& self.stats.committed == self.committed@.len()
        &&& self.group_next <= self.group_end <= self.nbits()
        &&& self.group_next < self.group_end
            ==> self.group_end == self.table@.spec_group_range(self.group_next >> self.table@.lg_group).1
        &&& self.nbits() <= usize::MAX >> 12
        &&& forall|s: nat| #[trigger] self.committed@.contains(s) ==> s < self.nbits()
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) ==> s < self.nbits()
        &&& self.inited@.id() == self.table@.spec_slots().mloc
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) && !self.table@.spec_is_guard(s as usize)
            ==> slab_marked(self.table@, s as int, self.inited@@)
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s) && self.table@.spec_is_guard(s as usize)
            ==> guard_marked(self.table@, s as int, self.inited@@)
        &&& self.group_end > 0 ==> self.owns((self.group_end - 1) as nat)
        &&& forall|s: nat| #[trigger] self.states@.contains_key(s)
            ==> slab_owner(self.table@.spec_class() as int, s as int) == self.arena@
        &&& whole_groups(self.states@.dom(), self.table@)
        &&& self.descs()
        &&& owned_rec(self.oauth@@, self.states@)
    }

    /// The ghost link at slab `s`, apart from the current group: the
    /// committed slabs are exactly the owned ones that left `Fresh` (a
    /// slab `next_fresh_slab` commits is made `Active` by `take` in the
    /// same lock hold); a partial slab is `Active`; the `empty` and
    /// `purged` sets are exactly the owned slabs in those states; an
    /// `Empty` slab's `dirty_at` stamp is not 0; and a guard slab is
    /// uncommitted and, when owned, `Fresh` (its descriptor stays zero).
    spec fn slab_core(&self, s: nat) -> bool {
        &&& self.committed@.contains(s) ==> self.owns(s)
        &&& self.committed@.contains(s) ==> self.st(s) != State::Fresh
        &&& self.owns(s) && self.st(s) != State::Fresh ==> self.committed@.contains(s)
        &&& self.partial.map@.contains(s) ==> self.owns(s) && self.st(s) == State::Active
        &&& self.empty.map@.contains(s) == (self.owns(s) && self.st(s) == State::Empty)
        &&& self.purged.map@.contains(s) == (self.owns(s) && self.st(s) == State::Purged)
        &&& self.owns(s) && self.st(s) == State::Empty ==> self.dirty@[s] != 0
        &&& self.table@.spec_is_guard(s as usize) ==> !self.committed@.contains(s)
            && (self.owns(s) ==> self.st(s) == State::Fresh)
    }

    /// The ghost link's current-group part at slab `s`: the current
    /// group's slabs from the cursor on are owned, `Fresh` and not
    /// committed.
    spec fn group_ok(&self, s: nat) -> bool {
        self.group_next <= s < self.group_end ==> self.owns(s) && self.st(s) == State::Fresh
            && !self.committed@.contains(s)
    }

    /// The ghost link at slab `s`.
    spec fn slab_ok(&self, s: nat) -> bool {
        &&& self.slab_core(s)
        &&& self.group_ok(s)
    }

    /// The seam's invariant, with the ghost link at every slab.
    spec fn inv(&self) -> bool {
        &&& self.base()
        &&& forall|s: nat| #[trigger] self.slab_ok(s)
    }

    /// The invariant with slab `x` exempt from the ghost link: the state
    /// between two writes of one transition (a slab's state and its set,
    /// or a slab taken out of a set and not yet made `Active`).
    spec fn inv_but(&self, x: int) -> bool {
        &&& self.base()
        &&& forall|s: nat| s != x ==> #[trigger] self.slab_ok(s)
    }

    /// Slab `s` is ready for `ClassState::take`: owned, committed (so not
    /// a guard slab), in neither idle set nor the current group's
    /// uncommitted rest, and the invariant holds elsewhere (a slab
    /// `next_slab` just took out of an idle set or committed fresh).
    spec fn ready(&self, s: nat) -> bool {
        &&& self.inv_but(s as int)
        &&& s < self.nbits()
        &&& self.owns(s)
        &&& self.committed@.contains(s)
        &&& !self.table@.spec_is_guard(s as usize)
        &&& !self.empty.map@.contains(s)
        &&& !self.purged.map@.contains(s)
        &&& !(self.group_next <= s < self.group_end)
    }

    /// `self` and `o` agree on everything [`Set::ensure`] keeps of a view
    /// of the shared set table (only an own store changes), so on the
    /// invariant too ([`lemma_same_sets_inv`]).
    spec fn same_sets(&self, o: Inner) -> bool {
        &&& self.same_sets_but_rec(o)
        &&& self.oauth@@ == o.oauth@@
        &&& self.oauth@.id() == o.oauth@.id()
    }

    /// [`Inner::same_sets`] but for the owned-slab record
    /// ([`Inner::oauth`]), which `SlabTable::take_group` adds a group to.
    spec fn same_sets_but_rec(&self, o: Inner) -> bool {
        &&& self.partial.map == o.partial.map
        &&& self.empty.map == o.empty.map
        &&& self.purged.map == o.purged.map
        &&& self.partial.store == o.partial.store
        &&& self.empty.store == o.empty.store
        &&& self.purged.store == o.purged.store
        &&& self.stats == o.stats
        &&& self.group_next == o.group_next
        &&& self.group_end == o.group_end
        &&& self.states == o.states
        &&& self.committed == o.committed
        &&& self.dirty == o.dirty
        &&& self.arena == o.arena
        &&& self.table == o.table
        &&& self.inited == o.inited
        &&& self.dperms == o.dperms
    }

    /// Values that agree as [`Inner::same_sets`] says agree on the invariant.
    proof fn lemma_same_sets(&self, o: Inner)
        requires
            self.same_sets(o),
        ensures
            self.nbits() == o.nbits(),
            self.base() == o.base(),
            forall|s: nat| #[trigger] self.slab_ok(s) == o.slab_ok(s),
            self.inv() == o.inv(),
    {
        assert forall|s: nat| #[trigger] self.slab_ok(s) == o.slab_ok(s) by {}
        if o.inv() {
            assert forall|s: nat| #[trigger] self.slab_ok(s) by {
                assert(o.slab_ok(s));
            }
        }
        if self.inv() {
            assert forall|s: nat| #[trigger] o.slab_ok(s) by {
                assert(self.slab_ok(s));
            }
        }
    }

    /// `self` and `o` agree on everything but the sets and counts.
    spec fn same_rest(&self, o: &Inner) -> bool {
        &&& self.stats.committed == o.stats.committed
        &&& self.stats.guards == o.stats.guards
        &&& self.group_next == o.group_next
        &&& self.group_end == o.group_end
        &&& self.nbits() == o.nbits()
        &&& self.states == o.states
        &&& self.committed == o.committed
        &&& self.dirty == o.dirty
        &&& self.arena == o.arena
        &&& self.table == o.table
        &&& self.inited == o.inited
        &&& self.dperms == o.dperms
        &&& self.oauth@@ == o.oauth@@
        &&& self.oauth@.id() == o.oauth@.id()
    }

    /// The exempt slab back in the link closes the invariant.
    proof fn lemma_close(&self, x: nat)
        requires
            self.inv_but(x as int),
            self.slab_ok(x),
        ensures
            self.inv(),
    {
        assert forall|s: nat| #[trigger] self.slab_ok(s) by {
            if s != x {
                assert(self.slab_ok(s));
            }
        }
    }

    /// Commit the set words that hold every bit up to and including
    /// `slab` in the three sets ([`Set::ensure`]), for a group the arena
    /// is about to receive (`take_group`, under the commit lock). The sets
    /// are views of the shared set table, whose commit leaves them as they
    /// are.
    #[inline(always)]
    fn ensure_sets(&mut self, meta: &Meta, page: usize, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Result<(), crate::sys::Errno>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_sets(0)),
            old(ord).allows(rank_meta()),
            old(self).base(),
            slab < old(self).nbits(),
            meta.is_process(),
        ensures
            final(self).same_sets(*old(self)),
            *final(ord) == *old(ord),
    {
        self.partial.ensure(meta, page, slab, Tracked(ord))?;
        self.empty.ensure(meta, page, slab, Tracked(ord))?;
        self.purged.ensure(meta, page, slab, Tracked(ord))
    }

    /// The idle slabs are among the committed ones, which are among the
    /// class's.
    proof fn lemma_counts(&self)
        requires
            self.inv(),
        ensures
            self.stats.empty + self.stats.purged <= self.stats.committed <= self.nbits(),
    {
        broadcast use vstd::set_lib::range_set_properties;
        let e = self.empty.map@;
        let p = self.purged.map@;
        let c = self.committed@;
        assert(e.disjoint(p)) by {
            assert forall|s: nat| !(e.contains(s) && p.contains(s)) by {
                assert(self.slab_ok(s));
            }
        }
        assert((e + p).subset_of(c)) by {
            assert forall|s: nat| #[trigger] (e + p).contains(s) implies c.contains(s) by {
                assert(self.slab_ok(s));
            }
        }
        vstd::set_lib::lemma_set_disjoint_lens(e, p);
        vstd::set_lib::lemma_len_subset(e + p, c);
        let r = vstd::set::Set::<nat>::range(0, self.nbits());
        assert(c.subset_of(r));
        vstd::set_lib::lemma_len_subset(c, r);
    }

    fn set_partial(&mut self, slab: usize)
        requires
            old(self).inv(),
            slab < old(self).nbits(),
            old(self).owns(slab as nat),
            old(self).st(slab as nat) == State::Active,
        ensures
            final(self).inv(),
            final(self).same_rest(old(self)),
            final(self).partial.map@ == old(self).partial.map@.insert(slab as nat),
            final(self).empty == old(self).empty,
            final(self).purged == old(self).purged,
    {
        if !self.partial.map.is_set(slab) {
            proof {
                lemma_set_len(self.partial, self.nbits(), slab as nat);
            }
            self.partial.map.set(slab);
            self.stats.nonfull += 1;
        } else {
            proof {
                assert(self.partial.map@.insert(slab as nat) =~= self.partial.map@);
            }
        }
        proof {
            assert forall|s: nat| #[trigger] self.slab_ok(s) by {
                assert(old(self).slab_ok(s));
            }
        }
    }

    fn clear_partial(&mut self, slab: usize)
        requires
            old(self).base(),
            slab < old(self).nbits(),
        ensures
            final(self).base(),
            final(self).same_rest(old(self)),
            final(self).partial.map@ == old(self).partial.map@.remove(slab as nat),
            final(self).empty == old(self).empty,
            final(self).purged == old(self).purged,
            old(self).inv() ==> final(self).inv(),
            forall|x: int| #[trigger] old(self).inv_but(x) ==> final(self).inv_but(x),
    {
        if self.partial.map.is_set(slab) {
            self.partial.map.clear(slab);
            self.stats.nonfull -= 1;
        } else {
            proof {
                assert(self.partial.map@.remove(slab as nat) =~= self.partial.map@);
            }
        }
        proof {
            assert forall|s: nat| old(self).slab_ok(s) implies #[trigger] self.slab_ok(s) by {}
        }
    }

    /// [`clear_partial`](Inner::clear_partial) of a slab whose bits a read
    /// found all set: `seen` is that read's record
    /// (`AtomicBitmap::is_full_seen`) for the slab's view of the class
    /// bitmap, so a partial bit leaves this way only for a slab the caller
    /// saw full.
    #[inline(always)]
    fn clear_partial_full(&mut self, slab: usize, Tracked(seen): Tracked<Option<FullSeen>>)
        requires
            old(self).base(),
            slab < old(self).nbits(),
            seen matches Some(w) && w.first() == slab * old(self).table@.spec_slots().sb
                && w.slots() == old(self).table@.spec_slots(),
        ensures
            final(self).base(),
            final(self).same_rest(old(self)),
            final(self).partial.map@ == old(self).partial.map@.remove(slab as nat),
            final(self).empty == old(self).empty,
            final(self).purged == old(self).purged,
            old(self).inv() ==> final(self).inv(),
    {
        self.clear_partial(slab)
    }

    /// `slab` into the empty set (it went `Empty`). The caller closes the
    /// link at `slab`.
    fn empty_add(&mut self, slab: usize)
        requires
            old(self).base(),
            slab < old(self).nbits(),
            !old(self).empty.map@.contains(slab as nat),
        ensures
            final(self).base(),
            final(self).same_rest(old(self)),
            final(self).empty.map@ == old(self).empty.map@.insert(slab as nat),
            final(self).partial == old(self).partial,
            final(self).purged == old(self).purged,
            old(self).inv_but(slab as int) ==> final(self).inv_but(slab as int),
    {
        proof {
            lemma_set_len(self.empty, self.nbits(), slab as nat);
        }
        debug_assert!(
            !self.empty.map.is_set(slab),
            "empty_add: slab {slab} already in the empty set"
        );
        self.empty.map.set(slab);
        self.stats.empty += 1;
        proof {
            assert forall|s: nat| s != slab && old(self).slab_ok(s) implies #[trigger] self.slab_ok(s) by {}
        }
    }
}

} // verus!

impl Inner {
    /// `slab` out of the empty set. Tests only: a refill and a purge
    /// spell the pair out (`next_slab`, `settle`).
    #[cfg(test)]
    fn empty_take(&mut self, slab: usize) {
        debug_assert!(
            self.empty.map.is_set(slab),
            "empty_take: slab {slab} not in the empty set"
        );
        self.empty.map.clear(slab);
        self.stats.empty = self
            .stats
            .empty
            .checked_sub(1)
            .unwrap_or_else(|| abort::slab_count_underflow("empty"));
    }

    /// `slab` into the purged set. Tests only: a purge spells the pair
    /// out (`settle`).
    #[cfg(test)]
    fn purged_add(&mut self, slab: usize) {
        debug_assert!(
            !self.purged.map.is_set(slab),
            "purged_add: slab {slab} already in the purged set"
        );
        self.purged.map.set(slab);
        self.stats.purged += 1;
    }

    /// `slab` out of the purged set. Tests only: a refill spells the
    /// pair out (`next_slab`).
    #[cfg(test)]
    fn purged_take(&mut self, slab: usize) {
        debug_assert!(
            self.purged.map.is_set(slab),
            "purged_take: slab {slab} not in the purged set"
        );
        self.purged.map.clear(slab);
        self.stats.purged = self
            .stats
            .purged
            .checked_sub(1)
            .unwrap_or_else(|| abort::slab_count_underflow("purged"));
    }
}

verus! {

broadcast use {table::lemma_spec_nslabs, table::lemma_spec_class};

impl ClassState {
    /// `class` is a class number, `table` points at that class's table
    /// (element `class` of the tables it has the permissions to), the
    /// sets behind the lock are over its slabs, and the lock ranks as
    /// this arena's lock of the class. `new` proves it: it takes `class`
    /// from the table and builds the sets over the set table's count for
    /// the class's slab size, which equals `table.nslabs` because both
    /// are the process's regions' count for the class (`SetTable::over`,
    /// `SlabTable::inv`).
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& self.class < NCLASSES
        &&& table_at(self.table).inv()
        &&& table_at(self.table).spec_class() == self.class as usize
        &&& index_of(self.table) == self.class as int
        &&& lock_init(self.inner.0).nbits() == table_at(self.table).spec_nslabs()
        &&& lock_init(self.inner.0).arena@ == self.arena
        &&& lock_init(self.inner.0).table@ == table_at(self.table)
        &&& self.inner.0.rank() == rank_class(self.arena as int, self.class as int)
    }

    /// The class's shared table.
    pub closed spec fn spec_table(&self) -> SlabTable {
        table_at(self.table)
    }

    /// Slabs of the class region.
    pub closed spec fn spec_nslabs(&self) -> usize {
        table_at(self.table).spec_nslabs()
    }

    /// A class region has at most `2^45` slabs: it ends below `2^57` and
    /// its slabs are 4 KiB or more. No code: a proof step for callers
    /// that hold the state by reference.
    #[inline(always)]
    pub fn nslabs_fit(&self)
        ensures
            self.spec_nslabs() <= 0x2000_0000_0000,
    {
        proof {
            use_type_invariant(self);
            let t = table_at(self.table);
            let (n, b, lg) = (t.nslabs, t.base, class_table()[t.class as int].lg_slab);
            assert(n <= ((1usize << 57) - b) as usize >> lg && 12 <= lg <= 22 && b < 1usize << 57 ==> n <= 0x2000_0000_0000)
                by (bit_vector);
        }
    }

    /// The class number.
    pub closed spec fn spec_class(&self) -> usize {
        self.class as usize
    }

    /// The arena this state belongs to.
    pub closed spec fn spec_arena(&self) -> u16 {
        self.arena
    }

    /// The location of this state's owned-slab record ([`Inner::oauth`]):
    /// its class lock value's, made by `new`.
    pub closed spec fn spec_oloc(&self) -> Loc {
        lock_init(self.inner.0).oauth@.id()
    }

    /// `w` names this state as the published state of its class and arena
    /// index: the class `spec_class` state of the arena the process
    /// registry published at `spec_arena` ([`Published`]). Only the
    /// published states take groups (the callers of `take_group` hold such
    /// a record), and two records of one index agree, so one state per
    /// (arena index, class) takes groups.
    pub open spec fn published_by(&self, w: Published) -> bool {
        &&& w.id() == process_pub_loc()
        &&& w.key() == self.spec_arena() as int
        &&& w.value().len() == NCLASSES
        &&& w.value()[self.spec_class() as int] == self.spec_oloc()
    }
}

/// A class region's slots all have numbers in a `usize`: `nslabs` slabs of
/// at most `1 << lg_slab / size` slots each.
proof fn lemma_slots_fit(t: SlabTable)
    requires
        t.inv(),
    ensures
        (t.spec_nslabs() as int) * (class_table()[t.spec_class() as int].nslots as int) <= usize::MAX,
        (t.spec_nslabs() as int) * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int) <= usize::MAX,
        (t.spec_nslabs() as int) * ((1usize << class_table()[t.spec_class() as int].lg_slab) as int) <= 0x200_0000_0000_0000,
        (t.spec_nslabs() as int) * (class_table()[t.spec_class() as int].nslots as int) <= 0x200_0000_0000_0000,
{
    broadcast use {table::lemma_spec_nslabs, table::lemma_spec_class};
    let c = class_table()[t.spec_class() as int];
    let lg = c.lg_slab as usize;
    let n = t.spec_nslabs();
    let m = (usize::MAX - t.base) as usize;
    vstd::bits::lemma_usize_pow2_no_overflow(lg as nat);
    vstd::bits::lemma_usize_shl_is_mul(1, lg);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m);
    let m57 = ((1usize << 57usize) - t.base) as usize;
    assert((1usize << 57usize) == 0x200_0000_0000_0000usize) by (bit_vector);
    vstd::bits::lemma_usize_mul_pow2_le_max_iff_max_shr(n, lg, m57);
    let l8 = c.lg_slab;
    assert((1usize << l8) == (1usize << lg)) by (bit_vector)
        requires lg == l8 as usize;
    let p = (1usize << lg) as int;
    assert(n as int * p <= usize::MAX);
    assert((c.nslots as int) <= p) by (nonlinear_arith)
        requires (c.nslots as int) * (c.size as int) <= p, c.size >= 1;
    assert((n as int) * (c.nslots as int) <= (n as int) * p) by (nonlinear_arith)
        requires (c.nslots as int) <= p, n >= 0;
}

/// [`Inner::lemma_counts`] wherever the invariant is known: the idle
/// counts are at most the committed count, which is at most the class's
/// slab count.
broadcast proof fn lemma_inv_counts(v: Inner)
    ensures
        #[trigger] v.inv() ==> v.stats.empty + v.stats.purged <= v.stats.committed <= v.nbits(),
{
    if v.inv() {
        v.lemma_counts();
    }
}

/// Values that agree as [`Inner::same_sets`] says agree on the invariant
/// ([`Inner::lemma_same_sets`]), wherever [`Inner::same_sets`] is known.
broadcast proof fn lemma_same_sets_inv(a: Inner, b: Inner)
    ensures
        #[trigger] a.same_sets(b) ==> a.inv() == b.inv() && a.nbits() == b.nbits(),
{
    if a.same_sets(b) {
        a.lemma_same_sets(b);
    }
}

/// Assumption (`lock.rs`): the class lock's value keeps the set/count
/// seam's invariant, ghost link included, while the lock is free.
/// Verified holders show it through the postconditions of the `Inner`
/// methods and of `next_slab` and `next_fresh_slab` (with
/// `SlabTable::take_group` under it); `ClassState::new` proves it of the
/// value it creates the lock with, `lowest_partial` and `is_partial`
/// only read the value, and the refill's lock hold (`plan`, with `take`)
/// keeps it by their postconditions, as do the release path (`reopen`,
/// `on_live_zero`) and the purge and decay walks (`decay.rs`). The
/// unverified holders are the tests, but for those that break the counts
/// on purpose to reach an abort. Each of them writes an owned slab's
/// state and stamp only through the bridges (`set_state_locked`,
/// `set_dirty_locked`), which write the descriptor through the permission the value
/// holds and update the ghost state and stamp with it: verified, so they
/// keep `Inner::descs` when their callers meet their preconditions, which
/// nothing checks at these unverified call sites. None of them updates the other ghost fields (`committed`,
/// `inited`, `oauth`), so for them the assumption is also the set and
/// count shapes (`Inner::base`), the sets' disjointness, and the ghost
/// link between the sets and the ghost states (`Inner::slab_core`), which
/// their set updates and the bridges' state updates are meant to keep
/// together. The fork handlers take the raw lock without touching the
/// value.
broadcast axiom fn axiom_class_lock_inv(v: Inner)
    ensures
        #[trigger] lock_inv(v) == v.inv(),
;

/// Assumption (`lock.rs`): the sets behind a class lock stay over the
/// slab count they were created with, and the value's arena, table and
/// owned-slab record's location are the ones `new` set. Every `Inner`
/// method keeps `nbits` and those fields (`same_rest`, `same_sets`),
/// nothing replaces the sets, and no code writes those ghost fields after
/// `new` (`take_group` adds to the record, at its location).
broadcast axiom fn axiom_class_lock(init: Inner, v: Inner)
    ensures
        #[trigger] lock_frame(init, v) == (v.nbits() == init.nbits() && v.arena == init.arena && v.table == init.table
            && v.oauth@.id() == init.oauth@.id()),
;

impl ClassState {
    /// Create the state for `arena` over class `c`'s table, at `tp`
    /// (element `c` of the tables `tables` covers from `base` on), with
    /// its three slab sets as views of the arena's set table at
    /// `sets_ptr` (slot `kind * count + pos[class]` of the class's set
    /// group, `Set::in_table`), all empty, and nothing owned or committed.
    /// The state keeps the tables' frozen permissions, and the sets
    /// `sets_perm`, the set table's. The tables and the set table must
    /// outlive the state. `meta` and `regions` are unused: the sets live
    /// in the set table, the descriptors in the class's table. The three
    /// sets take their slots' permissions out of `slots` (`SetTable::new`
    /// handed them out); all of these permissions are ghost.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        meta: &Meta,
        regions: &Regions,
        tp: *mut SlabTable,
        Tracked(tables): Tracked<&'static Map<int, PointsTo<SlabTable>>>,
        Ghost(base): Ghost<*mut SlabTable>,
        Ghost(c): Ghost<int>,
        arena: u16,
        sets_ptr: *const SetTable,
        Tracked(sets_perm): Tracked<&'static PointsTo<SetTable>>,
        Tracked(slots): Tracked<&mut Map<(int, int), ZeroRaw>>,
    ) -> (r: Option<ClassState>)
        requires
            tables_ok(*tables, base),
            0 <= c < NCLASSES,
            tables[c].ptr() == tp,
            sets_perm.ptr() == sets_ptr,
            sets_perm.is_init(),
            sets_perm.value().over(process_regions()),
            forall|k: int| 0 <= k < sets::SETS_PER_CLASS ==> #[trigger] sets_perm.value().holds_set(*old(slots), c, k),
        ensures
            r matches Some(s) ==> s.spec_class() == c && s.spec_table() == tables[c].value() && s.spec_arena() == arena,
            *final(slots) == old(slots).remove((c, 0)).remove((c, 1)).remove((c, 2)),
    {
        broadcast use axiom_class_lock_inv;
        let table = vstd::raw_ptr::ptr_ref(tp, Tracked(tables.tracked_borrow(c)));
        proof {
            use_type_invariant(table);
        }
        let _ = (meta, regions);
        let sets = vstd::raw_ptr::ptr_ref(sets_ptr, Tracked(sets_perm));
        let class = table.class as usize;
        proof {
            assert(sets.class_ok(process_regions(), class as int));
            assert(sets.holds_set(*slots, class as int, 0));
            assert(sets.holds_set(*slots, class as int, 1));
            assert(sets.holds_set(*slots, class as int, 2));
        }
        debug_assert!(table.nslabs == sets.nslabs[SetTable::group_of(class)]);
        let tracked w0 = slots.tracked_remove((class as int, 0));
        let tracked w1 = slots.tracked_remove((class as int, 1));
        let tracked w2 = slots.tracked_remove((class as int, 2));
        let partial = Set::in_table(sets_ptr, Tracked(sets_perm), class, 0, Tracked(w0));
        let empty = Set::in_table(sets_ptr, Tracked(sets_perm), class, 1, Tracked(w1));
        let purged = Set::in_table(sets_ptr, Tracked(sets_perm), class, 2, Tracked(w2));
        let tracked im = table.empty_marks();
        let tracked dm = Map::<nat, DescPerm>::tracked_empty();
        let tracked (oa, _) = GhostSetAuth::<int>::new(vstd::set::Set::empty());
        let inner = Inner {
            partial,
            empty,
            purged,
            stats: ClassStats {
                committed: 0,
                guards: 0,
                empty: 0,
                purged: 0,
                nonfull: 0,
            },
            group_next: 0,
            group_end: 0,
            // No slab owned or committed yet.
            states: Ghost(Map::empty()),
            committed: Ghost(vstd::set::Set::empty()),
            dirty: Ghost(Map::empty()),
            arena: Ghost(arena),
            table: Ghost(*table),
            inited: Tracked(im),
            dperms: Tracked(dm),
            oauth: Tracked(oa),
        };
        proof {
            let n = inner.nbits();
            assert(set_wf(inner.partial, n) && set_wf(inner.empty, n) && set_wf(inner.purged, n));
            assert(inner.partial.map@.len() == 0 && inner.committed@.len() == 0);
            assert(usize::MAX >> 12 == 0xF_FFFF_FFFF_FFFFusize) by (bit_vector);
            assert(n <= usize::MAX >> 12);
            assert(inner.descs()) by {
                reveal(descs_ok);
            }
            assert(whole_groups(inner.states@.dom(), inner.table@)) by {
                reveal(whole_groups);
            }
            assert(owned_rec(inner.oauth@@, inner.states@));
            assert(inner.base());
            assert forall|s: nat| #[trigger] inner.slab_ok(s) by {}
        }
        Some(ClassState {
            class: table.class,
            arena,
            table: table_ptr(tp, Tracked(tables), Ghost(base), Ghost(c)),
            gate: Decayer::new(),
            inner: Padded(Lock::new(inner, Ghost(rank_class(arena as int, table.class as int)))),
            claimed: AtomicU64::new(0),
            refilled: AtomicU64::new(0),
            released: AtomicU64::new(0),
            compacted: AtomicU64::new(0),
            compact_on: AtomicU64::new(0),
            compact_off: AtomicU64::new(0),
            gate_refill: AtomicU64::new(0),
            clamped: AtomicU64::new(0),
            _tail: [0; PAD_ALIGN - 64],
        })
    }

    /// The class's shared table.
    #[inline(always)]
    pub fn table(&self) -> (r: &SlabTable)
        ensures
            *r == self.spec_table(),
            r.spec_class() == self.spec_class(),
            r.spec_nslabs() == self.spec_nslabs(),
    {
        proof {
            use_type_invariant(self);
        }
        self.table.get()
    }

    /// Every class's table and the frozen permissions to them
    /// ([`tables_ok`]), from class 0's state: what
    /// [`Arena::init`](crate::arena::Arena::init) builds another arena's
    /// states over.
    #[inline(always)]
    pub(crate) fn tables(&self) -> (r: (*mut SlabTable, Tracked<&'static Map<int, PointsTo<SlabTable>>>))
        requires
            self.spec_class() == 0,
        ensures
            tables_ok(*r.1@, r.0),
    {
        proof {
            use_type_invariant(self);
        }
        self.table.tables()
    }

    #[inline]
    pub fn class(&self) -> (r: &'static Class)
        ensures
            *r == class_table()[self.spec_class() as int],
            r.wf(),
    {
        proof {
            use_type_invariant(self);
        }
        &CLASSES.table()[self.class as usize]
    }

    /// The state's slab count is its class region's in the process's
    /// regions (`SlabTable::inv`). No code: a proof step for callers that
    /// hold the state by reference.
    #[inline(always)]
    pub fn nslabs_are_the_region_s(&self)
        ensures
            self.spec_nslabs() as int == process_regions().spec_nslabs(self.spec_class() as int),
    {
        proof {
            use_type_invariant(self);
        }
    }

    #[inline]
    #[cfg(test)]
    pub fn arena(&self) -> u16 {
        self.arena
    }

    #[inline]
    #[cfg(test)]
    pub fn nslabs(&self) -> usize {
        self.table().nslabs
    }

    /// The class lock, for fork handlers.
    #[inline]
    pub fn raw_lock(&self) -> (r: &RawLock)
        ensures
            *r == self.spec_raw_lock(),
            r.rank() == rank_class(self.spec_arena() as int, self.spec_class() as int),
    {
        proof {
            use_type_invariant(self);
        }
        self.inner.raw()
    }

    /// The lock [`Self::raw_lock`] returns.
    pub closed spec fn spec_raw_lock(&self) -> RawLock {
        self.inner.0.spec_raw()
    }

    /// [`SlabTable::is_guard_slab`].
    #[inline]
    pub fn is_guard_slab(&self, slab: usize) -> bool {
        self.table().is_guard_slab(slab)
    }

    /// [`SlabTable::committed_slabs`]: slabs of every arena.
    #[inline]
    pub fn committed_slabs(&self) -> (r: usize)
        ensures
            r <= self.spec_nslabs(),
    {
        self.table().committed_slabs()
    }
}

impl ClassState {
    /// [`SlabTable::desc`].
    #[inline]
    pub fn desc(&self, slab: usize) -> (r: &SlabDesc)
        requires
            crate::sys::may_abort_pending(),
        ensures
            *r == desc_at(self.spec_table(), slab),
    {
        self.table().desc(slab)
    }
}

} // verus!

verus! {

impl ClassState {
    #[inline]
    fn slab_addr(&self, slab: usize) -> (r: usize)
        requires
            slab < self.spec_nslabs(),
        ensures
            r == self.spec_table().base + slab * ((1usize << class_table()[self.spec_class() as int].lg_slab) as int),
            r + (1usize << class_table()[self.spec_class() as int].lg_slab) <= 1usize << 57,
    {
        self.table().slab_addr(slab)
    }

    #[inline]
    fn slot_ptr(&self, slab: usize, slot: usize) -> (r: *mut u8)
        requires
            slab < self.spec_nslabs(),
            slot < class_table()[self.spec_class() as int].slab_bits,
        ensures
            r@.addr == process_regions().slot_at(self.spec_class() as int, slab as int, slot as int),
            process_regions().is_bit_slot(self.spec_class() as int, r@.addr),
            r@.provenance == cast_provenance(),
    {
        proof {
            use_type_invariant(self);
        }
        self.table().slot_ptr(slab, slot)
    }

    /// [`SlabTable::locate`].
    #[inline]
    pub fn locate(&self, p: *mut u8) -> (r: (usize, usize))
        requires
            crate::sys::may_abort() || (crate::heap::regions_ok() && process_regions().is_slot_addr(self.spec_class() as int, p@.addr)),
        ensures
            r.0 < self.spec_nslabs(),
            r.1 < class_table()[self.spec_class() as int].nslots,
            process_regions().slot_at(self.spec_class() as int, r.0 as int, r.1 as int) == p@.addr,
    {
        proof {
            use_type_invariant(self);
        }
        self.table().locate(p)
    }

    // The accessors below read and write the owner word and a slab
    // descriptor under this arena's class lock (`g`, the value the caller
    // holds). Each compiles to the one access in its body; `g` and `slab`
    // name what the access is under and of.

    /// The arena that owns `slab` ([`slab_owner`], [`SlabTable::owner`]),
    /// read under this arena's class lock (`g`); if it is this arena, `g`
    /// owns `slab`. `pubw` is the registry's record of this state's arena
    /// ([`ClassState::published_by`]): the owner word of a handed-out group
    /// records its taker, the published state of the arena it names
    /// (`take_group` requires the taker's record), and the group's slabs
    /// in that state's owned-slab record (`table::OwnerPred`); two records
    /// of one index name one arena, so the taker is this state, and `g`
    /// has the slab in its record, whose every slab is owned
    /// ([`Inner::base`]).
    // `g` and `pubw` are read only by the proof (ghost).
    #[allow(unused_variables)]
    #[inline(always)]
    fn owner_locked(&self, g: &Inner, slab: usize, pubw: Tracked<&'static Published>) -> (r: u16)
        requires
            crate::sys::may_abort_pending(),
            g.arena@ == self.arena,
            g.table@ == self.spec_table(),
            g.base(),
            g.oauth@.id() == self.spec_oloc(),
            self.published_by(*pubw@),
        ensures
            r == self.arena ==> g.owns(slab as nat),
            r == slab_owner(self.spec_class() as int, slab as int),
    {
        proof {
            use_type_invariant(self);
        }
        let (r, seen) = self.table().owner(slab);
        self.table().owned_by(slab, seen, g, pubw);
        r
    }
    /// The state of slab `slab`'s descriptor `d`, read under this arena's
    /// class lock (`g`): for a slab the arena owns, the state its ghost
    /// records, through the permission `g` holds ([`Inner::descs`]).
    /// Every write of an owned slab's state byte under the lock goes
    /// through that permission, with the ghost update to the same state
    /// ([`set_state_locked`]), and `take_group` initialised the descriptor
    /// through it, `Fresh`, before `next_fresh_slab` added the slab
    /// `Fresh` to the ghost.
    ///
    /// [`set_state_locked`]: ClassState::set_state_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn state_locked(&self, g: &Inner, d: &SlabDesc, slab: usize) -> (r: State)
        requires
            g.descs(),
            g.table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
        ensures
            g.owns(slab as nat) ==> r == g.st(slab as nat),
    {
        proof {
            if g.owns(slab as nat) {
                g.lemma_desc(slab as nat);
            }
        }
        let r = d.state_held(Ghost(slab as nat), Tracked(g.dperms.borrow()));
        proof {
            if g.owns(slab as nat) {
                lemma_state_byte_inj(r, g.st(slab as nat));
            }
        }
        r
    }

    /// The `dirty_at` stamp of slab `slab`'s descriptor `d`, read under
    /// this arena's class lock (`g`) through the permission `g` holds, as
    /// [`state_locked`] reads the state. Every write of a stamp goes
    /// through that permission ([`set_dirty_locked`], `take_group`'s
    /// `init`).
    ///
    /// [`state_locked`]: ClassState::state_locked
    /// [`set_dirty_locked`]: ClassState::set_dirty_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn dirty_locked(&self, g: &Inner, d: &SlabDesc, slab: usize) -> (r: u64)
        requires
            g.descs(),
            g.table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
        ensures
            g.owns(slab as nat) ==> r == g.dirty@[slab as nat],
    {
        proof {
            if g.owns(slab as nat) {
                g.lemma_desc(slab as nat);
            }
        }
        d.dirty_held(Ghost(slab as nat), Tracked(g.dperms.borrow()))
    }

    /// `SlabDesc::set_state` on owned slab `slab`'s descriptor `d` under
    /// this arena's class lock (`g`), through the permission `g` holds for
    /// it, with the ghost state updated to match (the write side of
    /// [`state_locked`]). Nothing else changes. Never to `Fresh`: an owned
    /// slab that left `Fresh` does not return to it.
    ///
    /// [`state_locked`]: ClassState::state_locked
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_state_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, s: State)
        requires
            old(g).descs(),
            old(g).table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
            old(g).owns(slab as nat),
            s != State::Fresh,
        ensures
            final(g).descs(),
            final(g).states@ == old(g).states@.insert(slab as nat, s),
            final(g).states@.dom() == old(g).states@.dom(),
            final(g).partial == old(g).partial,
            final(g).empty == old(g).empty,
            final(g).purged == old(g).purged,
            final(g).stats == old(g).stats,
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
            final(g).committed == old(g).committed,
            final(g).dirty == old(g).dirty,
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            final(g).inited == old(g).inited,
            final(g).oauth == old(g).oauth,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(slab as nat);
        d.set_state(s, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(slab as nat, p);
            g.states@ = g.states@.insert(k, s);
            assert(g.states@.dom() =~= g0.states@.dom());
            lemma_descs_update(g0, *g, k);
        }
    }

    /// `SlabDesc::set_dirty_at` on owned slab `slab`'s descriptor `d`
    /// under this arena's class lock (`g`), through the permission `g`
    /// holds for it, with the ghost stamp updated to match. Nothing else
    /// changes.
    #[allow(unused_variables)]
    #[inline(always)]
    fn set_dirty_locked(&self, g: &mut Inner, d: &SlabDesc, slab: usize, ns: u64)
        requires
            old(g).descs(),
            old(g).table@ == self.spec_table(),
            *d == desc_at(self.spec_table(), slab),
            old(g).owns(slab as nat),
        ensures
            final(g).descs(),
            final(g).dirty@ == old(g).dirty@.insert(slab as nat, ns),
            final(g).states == old(g).states,
            final(g).partial == old(g).partial,
            final(g).empty == old(g).empty,
            final(g).purged == old(g).purged,
            final(g).stats == old(g).stats,
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
            final(g).committed == old(g).committed,
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            final(g).inited == old(g).inited,
            final(g).oauth == old(g).oauth,
    {
        let ghost g0 = *g;
        let ghost k = slab as nat;
        proof {
            g.lemma_desc(k);
        }
        let tracked mut p = g.dperms.borrow_mut().tracked_remove(slab as nat);
        d.set_dirty_at(ns, Tracked(&mut p));
        proof {
            g.dperms.borrow_mut().tracked_insert(slab as nat, p);
            g.dirty@ = g.dirty@.insert(k, ns);
            lemma_descs_update(g0, *g, k);
        }
    }
}

} // verus!

impl ClassState {
    /// Claim up to `out.len()` slots, lowest address first, writing slot
    /// pointers to `out[..n]`. Each lock hold reserves from up to
    /// [`MAX_PICKS`] slabs (see [`plan`]); the loop repeats only when a
    /// plan ran out of picks before the request was met.
    ///
    /// [`plan`]: ClassState::plan
    pub fn claim(&self, meta: &Meta, out: &mut [*mut u8]) -> Claim {
        self.claim_tokens(
            meta,
            out,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0
    }

    /// [`claim`] for `slot_randomize` (design section 4): the same slabs
    /// and the same reservations (`plan` is untouched,
    /// so the slabs are still the lowest partial ones and a `Fresh` or
    /// `Purged` slab's prefix is still claimed under the lock), but the
    /// bit scan of every other pick starts at a word drawn from `rng`
    /// instead of word 0, and the batch is shuffled with `rng` before it
    /// is returned, so the cache hands out its slots in a random order
    /// instead of by ascending address. Shuffling changes the order of
    /// `out[..n]`, not the set of slots, so [`Claim::zeroed`] means what it
    /// always means: every slot came from a slab nothing else could touch.
    ///
    /// [`claim`]: ClassState::claim
    pub fn claim_random(&self, meta: &Meta, out: &mut [*mut u8], rng: &mut u64) -> Claim {
        self.claim_random_zero(
            meta,
            out,
            rng,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
        .0
    }
}

verus! {

/// `picks_sum` only grows along the picks.
proof fn lemma_picks_sum_mono(p: Seq<Pick>, i: int, j: int)
    requires
        0 <= i <= j,
    ensures
        picks_sum(p, i) <= picks_sum(p, j),
    decreases j,
{
    if j > i {
        lemma_picks_sum_mono(p, i, j - 1);
    }
}

/// Clearing the lowest set bit of a non-zero word takes one off its count.
proof fn lemma_popcount_clear_lowest(m: u64)
    requires
        m != 0,
    ensures
        popcount(m & sub(m, 1)) + 1 == popcount(m),
    decreases m,
{
    let x = m & sub(m, 1);
    let h = m / 2;
    if m % 2 == 1 {
        assert(x == sub(m, 1) && x % 2 == 0 && x / 2 == h) by (bit_vector)
            requires m % 2 == 1, x == m & sub(m, 1), h == m / 2;
    } else {
        assert(h != 0 && x % 2 == 0 && x / 2 == h & sub(h, 1)) by (bit_vector)
            requires m % 2 == 0, m != 0, x == m & sub(m, 1), h == m / 2;
        lemma_popcount_clear_lowest(h);
    }
}

impl ClassState {
    /// [`claim`], with the claimed slots' tokens: `toks` gets the token of
    /// `out[i]` under key `i`, for every `i < n`, and keeps its other keys;
    /// and the zero tokens of the slots it wrote, one per slot when the
    /// claim is zeroed ([`claim_zero_ok`]).
    ///
    /// [`claim`]: ClassState::claim
    pub fn claim_tokens(&self, meta: &Meta, out: &mut [*mut u8], Tracked(toks): Tracked<&mut SlotToks>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Claim, Tracked<Map<int, Zeroed>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
            meta.is_process(),
        ensures
            *final(ord) == *old(ord),
            r.0.n <= old(out)@.len(),
            final(out)@.len() == old(out)@.len(),
            slots_in(final(out)@, self.spec_class() as int, 0, r.0.n as int),
            final(toks).owns(self.spec_table(), final(out)@, 0, 0, r.0.n as int),
            final(toks).frame(*old(toks), 0, r.0.n as int),
            claim_zero_ok(r.0, final(out)@, r.1@, self.spec_class() as int),
    {
        let tracked mut ex: Ghost<(Inner, Map<nat, usize>)> = Ghost(arbitrary());
        self.claim_with(meta, out, None, Tracked(toks), Tracked(&mut ex), pubw, Tracked(ord))
    }

    /// [`claim_random`] with the zero tokens of the slots it wrote
    /// ([`claim_zero_ok`]) and their slot tokens, as for [`claim_tokens`],
    /// both shuffled along with the slots.
    ///
    /// [`claim_random`]: ClassState::claim_random
    /// [`claim_tokens`]: ClassState::claim_tokens
    #[verifier::spinoff_prover]
    pub fn claim_random_zero(&self, meta: &Meta, out: &mut [*mut u8], rng: &mut u64, Tracked(toks): Tracked<&mut SlotToks>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Claim, Tracked<Map<int, Zeroed>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
            meta.is_process(),
        ensures
            *final(ord) == *old(ord),
            r.0.n <= old(out)@.len(),
            final(out)@.len() == old(out)@.len(),
            slots_in(final(out)@, self.spec_class() as int, 0, r.0.n as int),
            final(toks).owns(self.spec_table(), final(out)@, 0, 0, r.0.n as int),
            final(toks).frame(*old(toks), 0, r.0.n as int),
            claim_zero_ok(r.0, final(out)@, r.1@, self.spec_class() as int),
    {
        let tracked mut ex: Ghost<(Inner, Map<nat, usize>)> = Ghost(arbitrary());
        let (c, Tracked(z)) = self.claim_with(meta, out, Some(rng), Tracked(toks), Tracked(&mut ex), pubw, Tracked(ord));
        let tracked mut z = z;
        let ghost mid = out@;
        let ghost size = class_table()[self.spec_class() as int].size as int;
        proof {
            if c.zeroed {
                assert forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < c.n implies z.contains_key(i) && z[i].covers(mid.subrange(0, c.n as int)[i], size) by {
                    assert(mid.subrange(0, c.n as int)[i] == mid[i]);
                    assert(claim_zero_ok(c, mid, z, self.spec_class() as int));
                }
                assert(tokens_cover(z, mid.subrange(0, c.n as int), size));
            }
        }
        proof {
            toks.lemma_owns_keyed(self.spec_table(), mid, c.n as int);
        }
        let ghost t0 = *toks;
        let ghost o0 = *old(toks);
        shuffle_zero(&mut out[..c.n], rng, Ghost(c.zeroed), Ghost(size), Tracked(&mut z), Ghost(Some(self.spec_table().spec_slots().loc)), Tracked(&mut toks.toks));
        proof {
            assert(out@.subrange(0, c.n as int) == out@[..c.n as int]);
            // The batch handed out is the batch `claim_with` wrote, reordered.
            assert(permutes(out@.subrange(0, c.n as int), mid.subrange(0, c.n as int)));
            toks.lemma_owns_keyed(self.spec_table(), out@, c.n as int);
            assert(toks.bits == t0.bits);
            assert forall|j: int| !(0 <= j < c.n) implies (#[trigger] toks.toks.contains_key(j)) == o0.toks.contains_key(j) by {
                assert(t0.toks.contains_key(j) == o0.toks.contains_key(j));
            }
            assert forall|j: int| !(0 <= j < c.n) && o0.toks.contains_key(j) implies #[trigger] toks.toks[j] == o0.toks[j] by {
                assert(t0.toks.contains_key(j));
                assert(toks.toks.contains_key(j));
                assert(t0.toks[j] == o0.toks[j]);
            }
            assert forall|i: int| 0 <= i < c.n implies process_regions().is_slot(self.spec_class() as int, (#[trigger] out@[i])@.addr) by {
                assert(from_seq(out@.subrange(0, c.n as int), mid.subrange(0, c.n as int), i));
                let w = choose|w: int| 0 <= w < c.n && out@.subrange(0, c.n as int)[i] == mid.subrange(0, c.n as int)[w];
                assert(mid[w] == mid.subrange(0, c.n as int)[w]);
            }
            if c.zeroed {
                assert forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < c.n implies z.contains_key(i) && z[i].covers(out@[i], size) by {
                    assert(out@.subrange(0, c.n as int)[i] == out@[i]);
                }
            }
        }
        (c, Tracked(z))
    }

    /// The loop of [`claim`] and [`claim_random`].
    ///
    /// The loop ends because every plan that returns picks reserves at
    /// least one slot (a partial slab is picked only with `live < nslots`,
    /// and `next_slab` hands out idle slabs), which rests on `live`, an
    /// atomic other threads lower without the lock that Verus does not
    /// model (`exec_allows_no_decreases_clause`). The bound on `n` is the
    /// loop condition and `finish`'s count.
    ///
    /// The slots and their tokens: each `finish` writes the bit addresses
    /// of the bits it won and hands `toks` the token of each
    /// (`SlotToks::holds`), under the pointer's index in `out`. They are
    /// slot starts (`Regions::is_slot`) because a claim never wins a
    /// padding bit: `plan` hands `finish` the fresh markers of each pick's
    /// slab, which show `AtomicBitmap::init` set its padding bits
    /// (`AtomicBitmap::claim_word`), and `plan` picks no guard slab (the
    /// class lock's `ready`; `picks_marks`), so none is in a guard slab.
    ///
    /// The zero tokens ([`claim_zero_ok`]): a claim is zeroed only when
    /// every pick of every plan was a zero pick (`finish` clears the flag
    /// for any other), and `take` made a zero pick's token for the slots
    /// `claim_prefix` claimed in the same lock hold; `finish` splits it
    /// into one token per slot it writes.
    ///
    /// [`claim`]: ClassState::claim
    /// [`claim_random`]: ClassState::claim_random
    // The last four arguments are ghost and tracked state.
    #[allow(clippy::too_many_arguments)]
    #[verifier::spinoff_prover]
    #[verifier::exec_allows_no_decreases_clause]
    fn claim_with(&self, meta: &Meta, out: &mut [*mut u8], mut rng: Option<&mut u64>, Tracked(toks): Tracked<&mut SlotToks>, Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (Claim, Tracked<Map<int, Zeroed>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
            meta.is_process(),
        ensures
            *final(ord) == *old(ord),
            r.0.n <= old(out)@.len(),
            final(out)@.len() == old(out)@.len(),
            slots_in(final(out)@, self.spec_class() as int, 0, r.0.n as int),
            final(toks).owns(self.spec_table(), final(out)@, 0, 0, r.0.n as int),
            final(toks).frame(*old(toks), 0, r.0.n as int),
            claim_zero_ok(r.0, final(out)@, r.1@, self.spec_class() as int),
            // Claim completeness: a non-empty claim returns nothing only after a
            // lock hold that ran dry ([`ClassState::exhausted`]).
            r.0.n == 0 && old(out)@.len() > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),
    {
        // The claim does not use the picks' owners (`plan` states them).
        hide(picks_owned);
        let ghost t = self.spec_table();
        let ghost s = t.spec_slots();
        proof {
            use_type_invariant(self);
            t.lemma_slots_wf();
            broadcast use vstd::slice::axiom_spec_len;
        }
        let tracked mut z: Map<int, Zeroed> = Map::tracked_empty();
        let mut got = 0;
        let mut zeroed = true;
        let mut picks = [Pick { slab: 0, n: 0, zero: false, state: State::Active }; MAX_PICKS];
        let ghost mut dry = false;
        while got < out.len()
            invariant_except_break
                !dry,
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                self.published_by(*pubw@),
                meta.is_process(),
                t == self.spec_table(),
                s == t.spec_slots(),
                t.inv(),
                t.spec_class() == self.spec_class(),
                s.size == class_table()[self.spec_class() as int].size as int,
                got <= out@.len(),
                out@.len() == old(out)@.len(),
                out@.len() == out.len(),
                toks.holds(t, out@, 0, 0, got as int),
                toks.frame(*old(toks), 0, got as int),
                zeroed ==> zero_keys(z, out@, 0, 0, got as int, s.size),
                dry ==> (got == 0 && out@.len() > 0 ==> self.exhausted(ex@.0, ex@.1)),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
            ensures
                dry || got >= out@.len(),
        {
            let tracked mut ptoks: Map<int, GhostSubset<int>> = Map::tracked_empty();
            let tracked mut pz: Map<int, Zeroed> = Map::tracked_empty();
            let tracked mut pm: Map<int, GhostPersistentSubset<int>> = Map::tracked_empty();
            let np = self.plan(meta, out.len() - got, &mut picks, Tracked(&mut ptoks), Tracked(&mut pz), Tracked(&mut pm), Tracked(&mut *ex), pubw, Tracked(ord));
            if np == 0 {
                proof {
                    dry = true;
                }
                break;
            }
            let ghost g0 = got as int;
            let ghost o0 = out@;
            let ghost t0 = *toks;
            let ghost z0 = z;
            let ghost zeroed0 = zeroed;
            proof {
                assert(picks@.subrange(0, np as int).len() == np);
                assert forall|i: int| 0 <= i < np implies picks@.subrange(0, np as int)[i] == #[trigger] picks@[i] by {}
                lemma_picks_sum(picks@.subrange(0, np as int), picks@, np as int);
                assert(picks_zero(t, picks@.subrange(0, np as int), 0, np as int, pz));
                assert(picks_toks(t, picks@.subrange(0, np as int), 0, np as int, ptoks));
                assert(picks_marks(t, picks@.subrange(0, np as int), 0, np as int, pm));
            }
            got += self.finish(&picks[..np], &mut out[got..], &mut zeroed, rng.as_deref_mut(), Tracked(ptoks), Tracked(pz), Tracked(pm), Tracked(&mut *toks), Tracked(&mut z), Ghost(got as int));
            proof {
                assert forall|i: int| 0 <= i < g0 implies #[trigger] out@[i] == o0[i] by {}
                assert forall|i: int| 0 <= i < got implies toks.entry(t, #[trigger] out@[i], 0 + i) by {
                    if i < g0 {
                        assert(t0.entry(t, o0[i], i));
                    } else {
                        assert(toks.entry(t, out@.subrange(g0, out@.len() as int)[i - g0], g0 + (i - g0)));
                    }
                }
                if zeroed {
                    assert forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < got implies z.contains_key(i) && z[i].covers(out@[i], s.size) by {
                        if i < g0 {
                            assert(zeroed0);
                            lemma_zero_keys_at(z0, o0, 0, 0, g0, s.size, i);
                            assert(out@[i] == o0[i]);
                        } else {
                            let sub = out@.subrange(g0, out@.len() as int);
                            assert(sub[i - g0] == out@[i]);
                            lemma_zero_keys_at(z, sub, g0, 0, got - g0, s.size, i - g0);
                        }
                    }
                    lemma_zero_keys_from0(z, out@, got as int, s.size);
                }
            }
        }
        proof {
            lemma_holds_real(t, *toks, out@, 0, got as int);
            if got > 0 && zeroed {
                assert forall|i: int| #![trigger z.contains_key(i)] #![trigger z[i]] 0 <= i < got implies z.contains_key(i) && z[i].covers(out@[i], class_table()[self.spec_class() as int].size as int) by {
                    lemma_zero_keys_at(z, out@, 0, 0, got as int, s.size, i);
                }
            }
        }
        (Claim { n: got, zeroed: got > 0 && zeroed }, Tracked(z))
    }

    /// The unlocked half of a refill: write the slots of `picks` (one
    /// lock hold of [`plan`]) to `out`, lowest address first, and return
    /// how many. A pick flagged zero had its slots decided under the lock
    /// in [`take`] (the lowest `n` of a slab nothing else could touch), so
    /// only the other picks run the bit loop here; `zeroed` is cleared
    /// when any of them is not zero.
    ///
    /// With `rng` (`slot_randomize`) the bit loop of a pick starts at a
    /// random word rather than word 0; a rescan after a full pass still
    /// starts at 0. Without it the code is the address-ordered claim.
    ///
    /// Modelled at `model::Model::step_refill`, `protocol::Refill::Finish`
    /// (one pick at a time).
    ///
    /// [`plan`]: ClassState::plan
    /// [`take`]: ClassState::take
    // The extra arguments are ghost and tracked state.
    #[verifier::spinoff_prover]
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        picks: &[Pick],
        out: &mut [*mut u8],
        zeroed: &mut bool,
        mut rng: Option<&mut u64>,
        Tracked(ptoks): Tracked<Map<int, GhostSubset<int>>>,
        Tracked(pz): Tracked<Map<int, Zeroed>>,
        Tracked(pm): Tracked<Map<int, GhostPersistentSubset<int>>>,
        Tracked(otoks): Tracked<&mut SlotToks>,
        Tracked(zt): Tracked<&mut Map<int, Zeroed>>,
        Ghost(base): Ghost<int>,
    ) -> (r: usize)
        requires
            crate::sys::may_abort(),
            picks_ok(picks@, picks@.len() as int, self.spec_nslabs(), class_table()[self.spec_class() as int].nslots as nat),
            picks_sum(picks@, picks@.len() as int) <= old(out)@.len(),
            picks_zero(self.spec_table(), picks@, 0, picks@.len() as int, pz),
            picks_toks(self.spec_table(), picks@, 0, picks@.len() as int, ptoks),
            picks_marks(self.spec_table(), picks@, 0, picks@.len() as int, pm),
        ensures
            r == picks_sum(picks@, picks@.len() as int),
            *final(zeroed) ==> *old(zeroed),
            *final(zeroed) ==> zero_keys(*final(zt), final(out)@, base, 0, r as int, class_table()[self.spec_class() as int].size as int),
            zero_frame(*final(zt), *old(zt), base, base + r),
            final(out)@.len() == old(out)@.len(),
            bit_slots_in(final(out)@, self.spec_class() as int, 0, r as int),
            final(otoks).holds(self.spec_table(), final(out)@, base, 0, r as int),
            final(otoks).frame(*old(otoks), base, base + r),
    {
        let cls = self.class();
        let ghost sb = class_table()[self.spec_class() as int].slab_bits as usize;
        let ghost nslots = class_table()[self.spec_class() as int].nslots as usize;
        let _t0 = self.table();
        let ghost s = _t0.spec_slots();
        let tracked mut ptoks = ptoks;
        let tracked mut pz = pz;
        let tracked mut pm = pm;
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
            broadcast use vstd::slice::axiom_spec_len;
            assert(out@.len() == out.len());
            _t0.lemma_slots_wf();
        }
        let check_zero = purge::check_zero();
        let mut got: usize = 0;
        // `for pick in picks`, spelled out: Verus rejects `continue` in a
        // `for` loop, and the `continue` below keeps the aarch64 codegen of
        // the plain loop (an `if`/`else` in its place does not).
        let mut iter = picks.iter();
        let ghost mut i: int = 0;
        loop
            invariant
                crate::sys::may_abort(),
                check_zero ==> crate::vspec::user::may_read_user(),
                0 <= i <= picks@.len(),
                iter.remaining() == picks@.skip(i).as_ref(),
                picks_ok(picks@, picks@.len() as int, self.spec_nslabs(), nslots as nat),
                out@.len() <= usize::MAX,
                picks_sum(picks@, picks@.len() as int) <= out@.len(),
                got == picks_sum(picks@, i),
                out@.len() == old(out)@.len(),
                *cls == class_table()[self.spec_class() as int],
                cls.wf(),
                sb == cls.slab_bits,
                nslots == cls.nslots,
                self.spec_class() < NCLASSES,
                bit_slots_in(out@, self.spec_class() as int, 0, got as int),
                *_t0 == self.spec_table(),
                _t0.inv(),
                _t0.spec_class() == self.spec_class(),
                _t0.spec_nslabs() == self.spec_nslabs(),
                s == _t0.spec_slots(),
                s.wf(),
                s.sb == sb,
                s.nslots == nslots,
                picks_zero(*_t0, picks@, i, picks@.len() as int, pz),
                *zeroed ==> *old(zeroed),
                *zeroed ==> zero_keys(*zt, out@, base, 0, got as int, s.size),
                zero_frame(*zt, *old(zt), base, base + got),
                picks_toks(*_t0, picks@, i, picks@.len() as int, ptoks),
                picks_marks(*_t0, picks@, i, picks@.len() as int, pm),
                otoks.holds(*_t0, out@, base, 0, got as int),
                otoks.frame(*old(otoks), base, base + got),
            ensures
                got == picks_sum(picks@, picks@.len() as int),
                out@.len() == old(out)@.len(),
                bit_slots_in(out@, self.spec_class() as int, 0, got as int),
                otoks.holds(*_t0, out@, base, 0, got as int),
                otoks.frame(*old(otoks), base, base + got),
                *zeroed ==> *old(zeroed),
                *zeroed ==> zero_keys(*zt, out@, base, 0, got as int, s.size),
                zero_frame(*zt, *old(zt), base, base + got),
            decreases picks@.len() - i,
        {
            let pick = match iter.next() {
                Some(pick) => pick,
                None => {
                    proof {
                        assert(picks@.skip(i).len() == 0);
                    }
                    break;
                },
            };
            let ghost pi = i;
            proof {
                assert(picks@.skip(i).as_ref()[0] == pick);
                assert(*pick == picks@[i]);
                lemma_picks_sum_mono(picks@, i + 1, picks@.len() as int);
                assert(picks_sum(picks@, i + 1) == got + pick.n);
                assert(MAX_EMPTY_PASSES == 0x40_0000) by (bit_vector);
                i = i + 1;
            }
            // The pick's tokens: a zero pick's prefix, claimed in `take`.
            proof {
                assert(ptoks.contains_key(pi));
                assert(ptoks[pi].id() == s.loc);
            }
            let ghost pt0 = ptoks;
            let ghost pz0 = pz;
            proof {
                assert(pz.contains_key(pi));
            }
            let tracked mut zc = pz.tracked_remove(pi);
            let tracked mut cur = ptoks.tracked_remove(pi);
            let ghost pm0 = pm;
            proof {
                assert(pm.contains_key(pi));
            }
            let tracked cm = pm.tracked_remove(pi);
            proof {
                assert forall|j: int| #![trigger pm.contains_key(j)] #![trigger pm[j]] #![trigger picks@[j]] i <= j < picks@.len() implies {
                    &&& pm.contains_key(j)
                    &&& pm[j].id() == s.mloc
                    &&& slab_marked(*_t0, picks@[j].slab as int, pm[j]@)
                    &&& !_t0.spec_is_guard(picks@[j].slab as usize)
                } by {
                    assert(pm0.contains_key(j));
                }
                assert(picks_zero(*_t0, picks@, i, picks@.len() as int, pz));
                assert(picks_marks(*_t0, picks@, i, picks@.len() as int, pm));
                assert(cm.id() == s.mloc);
                assert(slab_marked(*_t0, picks@[pi].slab as int, cm@));
                lemma_picks_toks_rest(*_t0, picks@, pi, picks@.len() as int, pt0, ptoks);
                assert forall|j: int| #![trigger pz.contains_key(j)] #![trigger pz[j]] i <= j < picks@.len() implies pz.contains_key(j) && pick_zero(*_t0, picks@[j], pz[j]) by {
                    assert(pz0.contains_key(j));
                }
                assert(pick_zero(*_t0, picks@[pi], zc));
                assert(cur.id() == s.loc);
                assert(cur@ == pick_addrs(*_t0, picks@[pi]));
            }
            let slab = pick.slab as usize;
            proof {
                // The pick's slab is not a guard slab (`picks_marks`).
                assert(pm0.contains_key(pi));
                assert(!_t0.spec_is_guard(slab));
            }
            let want = pick.n as usize;
            let ghost f = slab * s.sb;
            if pick.zero {
                // Bits `0..want` were claimed under the lock (`claim_prefix`).
                for k in 0..want
                    invariant
                        got + want <= out@.len(),
                        out@.len() <= usize::MAX,
                        out@.len() == old(out)@.len(),
                        slab < self.spec_nslabs(),
                        want <= nslots,
                        nslots <= sb,
                        self.spec_class() < NCLASSES,
                        sb == class_table()[self.spec_class() as int].slab_bits,
                        bit_slots_in(out@, self.spec_class() as int, 0, (got + k) as int),
                        *_t0 == self.spec_table(),
                        _t0.inv(),
                        _t0.spec_class() == self.spec_class(),
                        _t0.spec_nslabs() == self.spec_nslabs(),
                        s == _t0.spec_slots(),
                        s.wf(),
                        s.sb == sb,
                        s.nslots == nslots,
                        f == slab * s.sb,
                        !_t0.spec_is_guard(slab),
                        cur.id() == s.loc,
                        k < want ==> zc.addr() == s.addr(f + k),
                        zc.len() == (want - k) * s.size,
                        *zeroed ==> *old(zeroed),
                        *zeroed ==> zero_keys(*zt, out@, base, 0, (got + k) as int, s.size),
                        zero_frame(*zt, *old(zt), base, base + got + k),
                        picks_zero(*_t0, picks@, i, picks@.len() as int, pz),
                        picks_marks(*_t0, picks@, i, picks@.len() as int, pm),
                        cur@ == addrs(s, VSet::<int>::range(f + k, f + want)),
                        otoks.holds(*_t0, out@, base, 0, (got + k) as int),
                        otoks.frame(*old(otoks), base, base + got + k),
                        slots_in(out@, self.spec_class() as int, got as int, (got + k) as int),
                {
                    let ghost o1 = out@;
                    out[got + k] = self.slot_ptr(slab, k);
                    proof {
                        broadcast use {vstd::set_lib::range_set_properties, VSet::lemma_map_contains};
                        _t0.lemma_slot_bit(slab as int, k as int);
                        _t0.lemma_slots_wf();
                        let b = f + k;
                        let rest = VSet::<int>::range(f + k + 1, f + want);
                        assert(VSet::<int>::range(f + k, f + want) =~= rest.insert(b));
                        lemma_addrs_insert(s, rest, b);
                        assert forall|x: int| #[trigger] rest.contains(x) implies s.real(x) by {
                            assert(f + k + 1 <= x < f + want);
                            _t0.lemma_slot_bit(slab as int, x - f);
                            assert(slab * s.sb + (x - f) == x);
                        }
                        assert(!rest.contains(b));
                        lemma_addrs_remove(s, rest, b);
                        let tracked one = cur.split_singleton(s.addr(b));
                        let old_t = *otoks;
                        otoks.put(base + got + k, b, one);
                        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(b, s.sb, slab as int, k as int);
                        SlotToks::lemma_extend(old_t, *otoks, *old(otoks), *_t0, o1, out@, base, (got + k) as int, b);
                        // Slot `k`'s zero token, split off the pick's.
                        assert(out@[got + k]@.addr as int == s.addr(b));
                        assert(s.size <= zc.len()) by (nonlinear_arith)
                            requires zc.len() == (want - k) * s.size, k < want, s.size > 0;
                        let tracked (zk, zr) = zc.split(s.size);
                        zc = zr;
                        assert(zc.len() == (want - (k + 1)) * s.size) by (nonlinear_arith)
                            requires zc.len() == (want - k) * s.size - s.size;
                        if k + 1 < want {
                            lemma_next_slot_addr(s, slab as int, k as int);
                        }
                        let z1 = *zt;
                        zt.tracked_insert(base + got + k, zk);
                        assert(zt.contains_key(base + (got + k)) && zt[base + (got + k)].covers(out@[got + k], s.size));
                        if *zeroed {
                            assert(out@ =~= o1.update(got + k, out@[got + k]));
                            lemma_zero_keys_push(z1, *zt, o1, out@, base, (got + k) as int, s.size);
                        }
                        assert forall|kk: int| #![trigger zt.contains_key(kk)] #![trigger old(zt).contains_key(kk)] !(base <= kk < base + got + k + 1) implies {
                            &&& zt.contains_key(kk) == old(zt).contains_key(kk)
                            &&& old(zt).contains_key(kk) ==> zt[kk] == old(zt)[kk]
                        } by {
                            assert(z1.contains_key(kk) == old(zt).contains_key(kk));
                        }
                    }
                    proof {
                        // Slot `k` of a pick's slab, not a guard, is a
                        // slot start (`check_zero` reads it).
                        _t0.lemma_region();
                        _t0.lemma_guard_slab(slab as int);
                        process_regions().lemma_is_slot(self.spec_class() as int, slab as int, k as int, out@[got + k]@.addr);
                        assert forall|j: int| got <= j < got + k + 1 implies process_regions().is_slot(self.spec_class() as int, (#[trigger] out@[j])@.addr) by {
                            if j < got + k {
                                assert(out@[j] == o1[j]);
                            }
                        }
                    }
                }
                if check_zero {
                    proof {
                        // The pick's slots' tokens, keyed from `base + got`.
                        let sl = out@.subrange(got as int, (got + want) as int);
                        assert forall|j: int| 0 <= j < want implies otoks.entry(*_t0, #[trigger] sl[j], base + got + j) by {
                            assert(sl[j] == out@[got + j]);
                            assert(otoks.entry(*_t0, out@[got + j], base + (got + j)));
                        }
                    }
                    self.check_zero(&out[got..got + want], pick, Tracked(&mut *otoks), Ghost(base + got));
                }
                got += want;
                proof {
                    assert(picks_toks(*_t0, picks@, i, picks@.len() as int, ptoks));
                    assert(picks_marks(*_t0, picks@, i, picks@.len() as int, pm));
                    lemma_zero_frame_widen(*zt, *old(zt), base, base + got, base + got);
                }
                continue;
            }
            *zeroed = false;
            let t = self.table();
            let bm = t.bits(slab);
            let words = bm.words();
            let mut k: usize = 0;
            let mut word = match rng.as_deref_mut() {
                Some(r) => (xorshift(r) as usize) % words,
                None => 0,
            };
            let mut empty_passes = 0u32;
            proof {
                assert(picks_marks(*_t0, picks@, i, picks@.len() as int, pm));
            }
            while k < want
                invariant
                    crate::sys::may_abort(),
                    k <= want,
                    word < words,
                    words == bm.spec_words(),
                    bits_shape(bm, sb),
                    bits_ghost(bm, *_t0, slab),
                    empty_passes < MAX_EMPTY_PASSES,
                    out@.len() <= usize::MAX,
                    bm.spec_nbits() == sb,
                    got + want <= out@.len(),
                    out@.len() == old(out)@.len(),
                    slab < self.spec_nslabs(),
                    want <= nslots,
                    nslots <= sb,
                    sb == class_table()[self.spec_class() as int].slab_bits,
                    self.spec_class() < NCLASSES,
                    MAX_EMPTY_PASSES == 0x40_0000,
                    *cls == class_table()[self.spec_class() as int],
                    nslots == cls.nslots,
                    bit_slots_in(out@, self.spec_class() as int, 0, (got + k) as int),
                    *t == *_t0,
                    *_t0 == self.spec_table(),
                    _t0.inv(),
                    _t0.spec_class() == self.spec_class(),
                    _t0.spec_nslabs() == self.spec_nslabs(),
                    s == _t0.spec_slots(),
                    s.wf(),
                    s.sb == sb,
                    s.nslots == nslots,
                    f == slab * s.sb,
                    !_t0.spec_is_guard(slab),
                    otoks.holds(*_t0, out@, base, 0, (got + k) as int),
                    otoks.frame(*old(otoks), base, base + got + k),
                    !*zeroed,
                    zero_frame(*zt, *old(zt), base, base + got),
                    picks_zero(*_t0, picks@, i, picks@.len() as int, pz),
                    picks_marks(*_t0, picks@, i, picks@.len() as int, pm),
                    bm.inited_by(cm),
                decreases want - k, MAX_EMPTY_PASSES - empty_passes,
            {
                let tracked mut wt = t.empty_tokens();
                let Some((w, bits)) = bm.claim_from(word, want - k, Tracked(&mut wt), Tracked(&cm)) else {
                    // Every word was full when inspected. `plan` reserved
                    // no more than `nslots - live` and set bits never
                    // exceed `live`, so the slots exist: a concurrent
                    // claimer took the bits we looked at and left ours
                    // in a word we had already passed. Scan again.
                    empty_passes += 1;
                    if empty_passes == MAX_EMPTY_PASSES {
                        let d = self.desc(slab);
                        reservation_unmet(self.class as usize, slab, want, k, d.live(), bm.allocated(cls.nslots()), cls.nslots());
                    }
                    word = 0;
                    continue;
                };
                let mut m = bits;
                let ghost k0 = k;
                proof {
                    lemma_popcount_clear_lowest(m);
                    assert(VSet::<int>::empty().union(bm.slot_addrs(w as int, bits)) =~= bm.slot_addrs(w as int, bits));
                    assert(subset(bits, bits)) by (bit_vector);
                }
                while m != 0
                    invariant
                        k + popcount(m) == k0 + popcount(bits),
                        k0 + popcount(bits) <= want,
                        subset(m, slab_mask_of(sb)),
                        w < words_of(sb),
                        bits_shape(bm, sb),
                        bits_ghost(bm, *_t0, slab),
                        out@.len() <= usize::MAX,
                        got + want <= out@.len(),
                        out@.len() == old(out)@.len(),
                        slab < self.spec_nslabs(),
                        self.spec_class() < NCLASSES,
                        sb == class_table()[self.spec_class() as int].slab_bits,
                        bit_slots_in(out@, self.spec_class() as int, 0, (got + k) as int),
                        *_t0 == self.spec_table(),
                        _t0.inv(),
                        _t0.spec_class() == self.spec_class(),
                        _t0.spec_nslabs() == self.spec_nslabs(),
                        s == _t0.spec_slots(),
                        s.wf(),
                        s.sb == sb,
                        f == slab * s.sb,
                        !_t0.spec_is_guard(slab),
                        wt.id() == s.loc,
                        wt@ == word_addrs(*_t0, slab as int, w as int, m),
                        subset(m, bits),
                        bm.all_real(w as int, bits),
                        otoks.holds(*_t0, out@, base, 0, (got + k) as int),
                        otoks.frame(*old(otoks), base, base + got + k),
                        !*zeroed,
                        zero_frame(*zt, *old(zt), base, base + got),
                        picks_zero(*_t0, picks@, i, picks@.len() as int, pz),
                    decreases m,
                {
                    proof {
                        lemma_popcount_clear_lowest(m);
                        axiom_u64_trailing_zeros(m);
                        let tz = u64_trailing_zeros(m) as u64;
                        assert(subset(m, m)) by (bit_vector);
                        lemma_bit_in_slab(m, m, tz, sb, w);
                        let sm = slab_mask_of(sb);
                        let x = m & sub(m, 1);
                        assert(subset(x, sm) && x < m) by (bit_vector)
                            requires m != 0, x == m & sub(m, 1), m & !sm == 0u64;
                        assert(subset(x, bits)) by (bit_vector)
                            requires x == m & sub(m, 1), m & !bits == 0u64;
                    }
                    let ghost m0 = m;
                    let b = m.trailing_zeros() as usize;
                    m &= m - 1;
                    let ghost o1 = out@;
                    out[got + k] = self.slot_ptr(slab, w * 64 + b);
                    proof {
                        broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
                        let bit = f + 64 * w + b;
                        _t0.lemma_slot_bit(slab as int, (w * 64 + b) as int);
                        assert(slab * s.sb + (w * 64 + b) == bit);
                        lemma_word_pop(s, f, w as int, m0, b as u64);
                        let rest = mask_bits(s, f, w as int, m);
                        let old_t = *otoks;
                        // A claimed bit is a slot (`claim_from`).
                        lemma_subset_bits(m0, bits);
                        assert(has_bit(m0, b as u64));
                        assert(has_bit(bits, b as u64));
                        assert(s.real(bit));
                        lemma_addrs_insert(s, rest, bit);
                        lemma_addrs_remove(s, rest, bit);
                        assert(wt@.contains(s.addr(bit)));
                        let tracked one = wt.split_singleton(s.addr(bit));
                        otoks.put(base + got + k, bit, one);
                        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(bit, s.sb, slab as int, (w * 64 + b) as int);
                        SlotToks::lemma_extend(old_t, *otoks, *old(otoks), *_t0, o1, out@, base, (got + k) as int, bit);
                    }
                    k += 1;
                }
                word = if w + 1 == words { 0 } else { w + 1 };
            }
            let ghost g1 = got as int;
            got += want;
            proof {
                lemma_zero_frame_widen(*zt, *old(zt), base, base + g1, base + got);
            }
        }
        got
    }
}

} // verus!

verus! {

/// The addresses of slots `0..n` of `slab`.
spec fn prefix_addrs(t: SlabTable, slab: int, n: int) -> VSet<int> {
    addrs(t.spec_slots(), VSet::<int>::range(slab * t.spec_slots().sb, slab * t.spec_slots().sb + n))
}

/// The kernel fact of `mmap(2)` (a private anonymous mapping reads as
/// zero until it is first written) for slots `0..n` of slab `slab` of
/// table `t`: their `n * size` bytes from slot 0 read as zero.
///
/// Required, so proved at the call site: `ticket`, which this uses up, is
/// a state ticket (`slab::DescPerm::ticket`) from the slab's descriptor
/// permission recording `Fresh`, so made from the permission the slab's
/// arena's class lock value holds; the slab is one of the table's and not
/// a guard slab; and the caller holds the slots' uniqueness tokens
/// (`held`), so none of them is allocated, cached or in another claim at
/// the call; the `n` slots are the slab's own (`n <= nslots`), so the
/// range ends inside the slab.
///
/// Premise, the caller's and trusted: a slab whose descriptor reads
/// `Fresh` was never written since its class region was mapped. That rests
/// on `take_group` initialising each group's descriptors once, to `Fresh`,
/// through `SlabDesc::init` on its permission (the trusted accessor and
/// `table::axiom_zero_desc`), on every later write of an owned slab's
/// state byte going through `ClassState::set_state_locked`, which never
/// writes `Fresh` (it requires so; its unverified callers pass `Active`,
/// `Empty` and `Purged`), and on `take` moving a slab to `Active` before it
/// claims a slot. Call site: `ClassState::take`.
axiom fn axiom_fresh_slots_zero(
    tracked ticket: StateTicket,
    t: SlabTable,
    slab: int,
    n: int,
    tracked held: &GhostSubset<int>,
) -> (tracked z: Zeroed)
    requires
        ticket.id() == desc_id(&desc_at(t, slab as usize)),
        ticket.state() == state_byte(State::Fresh),
        0 <= slab < t.spec_nslabs(),
        !t.spec_is_guard(slab as usize),
        0 <= n <= t.spec_slots().nslots,
        held.id() == t.spec_slots().loc,
        prefix_addrs(t, slab, n).subset_of(held@),
    ensures
        z.addr() == t.spec_slots().addr(slab * t.spec_slots().sb),
        z.len() == n * t.spec_slots().size,
;

/// The kernel fact of `madvise(2)` (after `MADV_DONTNEED`, private
/// anonymous memory reads as zero) for slots `0..n` of slab `slab` of
/// table `t`: their `n * size` bytes from slot 0 read as zero.
///
/// Required, so proved at the call site: as for
/// [`axiom_fresh_slots_zero`], with the ticket recording `Purged`; and
/// `witness`, which this uses up, from a read of `purge::purged_zero` that
/// returned true (`purge::PurgedZero`).
///
/// Premise, the caller's and trusted: the witness's read was made in the
/// lock hold that claims the slots (`take` reads it there and passes it
/// straight on), and then a `Purged` slab was discarded with
/// `MADV_DONTNEED` and not written since: no batch of the process has
/// used `MADV_FREE` before that read (`purge::Batch::flush` sets the flag
/// before it issues one, and the lock orders that store before the read),
/// `Purged` is set only in the lock hold that flushes the slab's discard
/// (`ClassState::purge_empty`, `purge_curve`), and a `Purged` slab has no
/// allocated slot until a claim makes it `Active`. Call site:
/// `ClassState::take`.
axiom fn axiom_discarded_slots_zero(
    tracked ticket: StateTicket,
    tracked witness: PurgedZero,
    t: SlabTable,
    slab: int,
    n: int,
    tracked held: &GhostSubset<int>,
) -> (tracked z: Zeroed)
    requires
        ticket.id() == desc_id(&desc_at(t, slab as usize)),
        ticket.state() == state_byte(State::Purged),
        0 <= slab < t.spec_nslabs(),
        !t.spec_is_guard(slab as usize),
        0 <= n <= t.spec_slots().nslots,
        held.id() == t.spec_slots().loc,
        prefix_addrs(t, slab, n).subset_of(held@),
    ensures
        z.addr() == t.spec_slots().addr(slab * t.spec_slots().sb),
        z.len() == n * t.spec_slots().size,
;

/// The tokens a pick holds: the prefix a zero pick claimed under the lock.
spec fn pick_addrs(t: SlabTable, p: Pick) -> VSet<int> {
    if p.zero { prefix_addrs(t, p.slab as int, p.n as int) } else { VSet::<int>::empty() }
}

/// The zero token a pick holds: for a zero pick, the bytes of slots
/// `0..n` of its slab, which `take` found zero (any token otherwise).
spec fn pick_zero(t: SlabTable, p: Pick, z: Zeroed) -> bool {
    p.zero ==> {
        &&& z.addr() == t.spec_slots().addr(p.slab as int * t.spec_slots().sb)
        &&& z.len() == p.n as int * t.spec_slots().size
    }
}

/// `z` holds the zero tokens of picks `lo..hi`, by pick index.
spec fn picks_zero(t: SlabTable, p: Seq<Pick>, lo: int, hi: int, z: Map<int, Zeroed>) -> bool {
    forall|j: int| #![trigger z.contains_key(j)] #![trigger z[j]] lo <= j < hi ==> z.contains_key(j) && pick_zero(t, p[j], z[j])
}

/// [`picks_zero`] of one more pick, recorded under key `k`.
proof fn lemma_picks_zero_step(t: SlabTable, p0: Seq<Pick>, p1: Seq<Pick>, k: int, z0: Map<int, Zeroed>, z1: Map<int, Zeroed>)
    requires
        0 <= k,
        picks_zero(t, p0, 0, k, z0),
        forall|i: int| 0 <= i < k ==> p1[i] == p0[i],
        z1 == z0.insert(k, z1[k]),
        pick_zero(t, p1[k], z1[k]),
    ensures
        picks_zero(t, p1, 0, k + 1, z1),
{
    assert forall|j: int| #![trigger z1.contains_key(j)] #![trigger z1[j]] 0 <= j < k + 1 implies z1.contains_key(j) && pick_zero(t, p1[j], z1[j]) by {
        if j < k {
            assert(z0.contains_key(j));
            assert(p1[j] == p0[j]);
            assert(pick_zero(t, p0[j], z0[j]));
            assert(z1[j] == z0[j]);
        }
    }
    assert(picks_zero(t, p1, 0, k + 1, z1));
}

/// One more pick, `k`, with its tokens under key `k`, after picks `0..k`
/// that keep theirs.
proof fn lemma_picks_toks_step(
    t: SlabTable,
    p0: Seq<Pick>,
    p1: Seq<Pick>,
    k: int,
    t0: Map<int, GhostSubset<int>>,
    t1: Map<int, GhostSubset<int>>,
)
    requires
        0 <= k,
        picks_toks(t, p0, 0, k, t0),
        forall|i: int| 0 <= i < k ==> p1[i] == p0[i],
        t1 == t0.insert(k, t1[k]),
        t1[k].id() == t.spec_slots().loc,
        t1[k]@ == pick_addrs(t, p1[k]),
    ensures
        picks_toks(t, p1, 0, k + 1, t1),
{
    assert forall|j: int| #![trigger t1.contains_key(j)] #![trigger t1[j]] 0 <= j < k + 1 implies {
        &&& t1.contains_key(j)
        &&& t1[j].id() == t.spec_slots().loc
        &&& t1[j]@ == pick_addrs(t, p1[j])
    } by {
        if j < k {
            assert(t0.contains_key(j));
            assert(p1[j] == p0[j]);
        }
    }
}

/// Arena isolation at the claim: picks `lo..hi` are of slabs arena `a`
/// owns ([`slab_owner`]) in class `c`'s table. `take` picks only slabs the
/// class lock's ghost holds for the arena (`ready`), whose owner is the
/// arena (`Inner::base`).
spec fn picks_owned(c: int, a: u16, p: Seq<Pick>, lo: int, hi: int) -> bool {
    forall|j: int| lo <= j < hi ==> slab_owner(c, (#[trigger] p[j]).slab as int) == a
}

/// `pm` holds, by pick index, the fresh markers of the slab of each of
/// picks `lo..hi`: `init` ran on it (`AtomicBitmap::inited_by`), so it is
/// not a guard slab.
spec fn picks_marks(t: SlabTable, p: Seq<Pick>, lo: int, hi: int, pm: Map<int, GhostPersistentSubset<int>>) -> bool {
    forall|j: int| #![trigger pm.contains_key(j)] #![trigger pm[j]] #![trigger p[j]] lo <= j < hi ==> {
        &&& pm.contains_key(j)
        &&& pm[j].id() == t.spec_slots().mloc
        &&& slab_marked(t, p[j].slab as int, pm[j]@)
        &&& !t.spec_is_guard(p[j].slab as usize)
    }
}

/// [`picks_marks`] of one more pick, `k`, after picks `0..k` that keep
/// theirs.
proof fn lemma_picks_marks_step(
    t: SlabTable,
    p0: Seq<Pick>,
    p1: Seq<Pick>,
    k: int,
    m0: Map<int, GhostPersistentSubset<int>>,
    m1: Map<int, GhostPersistentSubset<int>>,
)
    requires
        0 <= k,
        picks_marks(t, p0, 0, k, m0),
        forall|i: int| 0 <= i < k ==> p1[i] == p0[i],
        m1 == m0.insert(k, m1[k]),
        m1[k].id() == t.spec_slots().mloc,
        slab_marked(t, p1[k].slab as int, m1[k]@),
        !t.spec_is_guard(p1[k].slab as usize),
    ensures
        picks_marks(t, p1, 0, k + 1, m1),
{
    assert forall|j: int| #![trigger m1.contains_key(j)] #![trigger m1[j]] #![trigger p1[j]] 0 <= j < k + 1 implies {
        &&& m1.contains_key(j)
        &&& m1[j].id() == t.spec_slots().mloc
        &&& slab_marked(t, p1[j].slab as int, m1[j]@)
        &&& !t.spec_is_guard(p1[j].slab as usize)
    } by {
        if j < k {
            assert(m0.contains_key(j));
            assert(p1[j] == p0[j]);
        }
    }
}

/// `toks` holds the tokens of picks `lo..hi`, by pick index.
spec fn picks_toks(t: SlabTable, p: Seq<Pick>, lo: int, hi: int, toks: Map<int, GhostSubset<int>>) -> bool {
    forall|j: int| #![trigger toks.contains_key(j)] #![trigger toks[j]] lo <= j < hi ==> {
        &&& toks.contains_key(j)
        &&& toks[j].id() == t.spec_slots().loc
        &&& toks[j]@ == pick_addrs(t, p[j])
    }
}

impl ClassState {
    /// What a refill's lock hold that reserved nothing saw (claim
    /// completeness): the lock value `g` it left,
    /// with the invariant, both idle sets empty, and each partial slab's
    /// `live` load, kept in `seen`, at `nslots` or more. With the link,
    /// every committed slab of the arena is then `Active`.
    spec fn exhausted(&self, g: Inner, seen: Map<nat, usize>) -> bool {
        &&& g.inv()
        &&& g.nbits() == self.spec_nslabs()
        &&& g.arena@ == self.arena
        &&& g.table@ == self.spec_table()
        &&& g.oauth@.id() == self.spec_oloc()
        &&& g.empty.map@ == VSet::<nat>::empty()
        &&& g.purged.map@ == VSet::<nat>::empty()
        &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) ==> seen.contains_key(x)
        &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) ==> seen[x] >= class_table()[self.spec_class() as int].nslots
    }

    /// `plan`'s scan so far: every partial slab below `lim` has its `live`
    /// load in `seen`, at `nslots` or more.
    spec fn seen_below(&self, g: Inner, seen: Map<nat, usize>, lim: int) -> bool {
        &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) && x < lim ==> seen.contains_key(x)
        &&& forall|x: nat| #[trigger] g.partial.map@.contains(x) && x < lim ==> seen[x] >= class_table()[self.spec_class() as int].nslots
    }

    /// One lock hold of a refill: reserve up to `want` slots across up to
    /// [`MAX_PICKS`] slabs, lowest address first, and return the number of
    /// picks written. Zero picks means nothing more can be committed.
    ///
    /// `nslots - live` is a lower bound on a slab's claimable slots: every
    /// claimer reserves in `live` under this lock before it sets bits, and
    /// every releaser clears bits before it subtracts. So a reservation of
    /// at most that many is always met (the claimer may have to rescan when
    /// a concurrent claimer wins the bits it looked at), and a partial slab
    /// whose bound is zero is either full (its bit is cleared here, the
    /// only place a partial bit is cleared while the slab stays `Active`)
    /// or has an in-flight claimer and is skipped. A slab with `live == 0` whose
    /// every slot is reserved here leaves the partial set at once: nothing
    /// in it can be released before this claim hands slots out, so no
    /// release can race the clear.
    ///
    /// No picks for a non-zero `want` means the hold ran dry: `ex` is the
    /// lock value it left and the `live` loads of its partial slabs, which
    /// [`ClassState::exhausted`] describes (claim completeness).
    // The last six arguments are ghost and tracked state.
    #[allow(clippy::too_many_arguments)]
    // Not isolated: the loops return while the guard borrows `ord`, whose
    // value `Lock::lock` states before them.
    #[verifier::loop_isolation(false)]
    fn plan(
        &self,
        meta: &Meta,
        want: usize,
        picks: &mut [Pick; MAX_PICKS],
        Tracked(ptoks): Tracked<&mut Map<int, GhostSubset<int>>>,
        Tracked(pz): Tracked<&mut Map<int, Zeroed>>,
        Tracked(pm): Tracked<&mut Map<int, GhostPersistentSubset<int>>>,
        Tracked(ex): Tracked<&mut Ghost<(Inner, Map<nat, usize>)>>,
        pubw: Tracked<&'static Published>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (np: usize)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
            meta.is_process(),
        ensures
            *final(ord) == *old(ord),
            np == 0 && want > 0 ==> self.exhausted(final(ex)@.0, final(ex)@.1),
            picks_zero(self.spec_table(), final(picks)@, 0, np as int, *final(pz)),
            picks_marks(self.spec_table(), final(picks)@, 0, np as int, *final(pm)),
            picks_owned(self.spec_class() as int, self.spec_arena(), final(picks)@, 0, np as int),
            picks_toks(self.spec_table(), final(picks)@, 0, np as int, *final(ptoks)),
            np <= MAX_PICKS,
            picks_ok(final(picks)@, np as int, self.spec_nslabs(), class_table()[self.spec_class() as int].nslots as nat),
            picks_sum(final(picks)@, np as int) <= want,
    {
        let cls = self.class();
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let tracked mut under = LockOrder::spare();
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));
        let mut np = 0;
        let mut remaining = want;
        let mut next = g.partial.map.lowest_set();
        let ghost mut seen: Map<nat, usize> = Map::empty();
        while let Some(s) = next
            invariant
                crate::sys::may_abort(),
                under.allows(rank_table(0)),
                np == 0 ==> self.seen_below(g@, seen, match next {
                    Some(x) => x as int,
                    None => self.spec_nslabs() as int,
                }),
                g@.inv(),
                g@.nbits() == self.spec_nslabs(),
                g@.arena@ == self.arena,
                g@.table@ == self.spec_table(),
                g@.oauth@.id() == self.spec_oloc(),
                self.published_by(*pubw@),
                *cls == class_table()[self.spec_class() as int],
                np <= MAX_PICKS,
                picks_ok(picks@, np as int, self.spec_nslabs(), cls.nslots as nat),
                picks_sum(picks@, np as int) + remaining == want,
                picks_toks(self.spec_table(), picks@, 0, np as int, *ptoks),
                picks_zero(self.spec_table(), picks@, 0, np as int, *pz),
                picks_marks(self.spec_table(), picks@, 0, np as int, *pm),
                picks_owned(self.spec_class() as int, self.arena, picks@, 0, np as int),
                next matches Some(x) ==> x < self.spec_nslabs() && g@.partial.map@.contains(x as nat),
            decreases
                match next {
                    Some(x) => self.spec_nslabs() - x,
                    None => 0,
                },
        {
            if remaining == 0 || np == MAX_PICKS {
                proof {
                    *ex = Ghost((g@, seen));
                }
                return np;
            }
            let desc = self.desc(s);
            let live = desc.live();
            if live >= cls.nslots() {
                let ghost g0 = g@;
                proof {
                    seen = seen.insert(s as nat, live);
                }
                if desc.state() == State::Active {
                    let (full, seen_full) = self.table().bits(s).is_full_seen();
                    if full {
                        g.clear_partial_full(s, seen_full);
                    }
                }
                proof {
                    assert(g@.partial.map@.subset_of(g0.partial.map@));
                }
            } else {
                let ghost before = picks@;
                let ghost k = np;
                proof {
                    assert(g@.slab_ok(s as nat));
                }
                let ghost z0 = *pz;
                let ghost t0 = *ptoks;
                let ghost m0 = *pm;
                self.take(&mut g, s, &mut remaining, picks, &mut np, Tracked(&mut *ptoks), Tracked(&mut *pz), Tracked(&mut *pm));
                proof {
                    lemma_picks_zero_step(self.spec_table(), before, picks@, k as int, z0, *pz);
                    lemma_picks_sum(before, picks@, k as int);
                    lemma_picks_toks_step(self.spec_table(), before, picks@, k as int, t0, *ptoks);
                    lemma_picks_marks_step(self.spec_table(), before, picks@, k as int, m0, *pm);
                    assert forall|j: int| 0 <= j < k + 1 implies slab_owner(self.spec_class() as int, (#[trigger] picks@[j]).slab as int) == self.arena by {
                        if j < k {
                            assert(picks@[j] == before[j]);
                        }
                    }
                }
            }
            next = g.partial.map.lowest_set_from(s + 1);
        }
        let ghost mut dry = false;
        while remaining > 0 && np < MAX_PICKS
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                under.allows(rank_table(0)),
                g@.inv(),
                g@.nbits() == self.spec_nslabs(),
                g@.arena@ == self.arena,
                g@.table@ == self.spec_table(),
                g@.oauth@.id() == self.spec_oloc(),
                self.published_by(*pubw@),
                *cls == class_table()[self.spec_class() as int],
                np <= MAX_PICKS,
                picks_ok(picks@, np as int, self.spec_nslabs(), cls.nslots as nat),
                picks_sum(picks@, np as int) + remaining == want,
                picks_toks(self.spec_table(), picks@, 0, np as int, *ptoks),
                picks_zero(self.spec_table(), picks@, 0, np as int, *pz),
                picks_marks(self.spec_table(), picks@, 0, np as int, *pm),
                picks_owned(self.spec_class() as int, self.arena, picks@, 0, np as int),
                np == 0 ==> self.seen_below(g@, seen, self.spec_nslabs() as int),
            decreases MAX_PICKS - np,
        {
            let s = match self.next_slab(&mut g, meta, pubw, Tracked(&mut under)) {
                Some(s) => s,
                None => {
                    proof {
                        dry = true;
                    }
                    break;
                },
            };
            let ghost before = picks@;
            let ghost k = np;
            let ghost z0 = *pz;
            let ghost t0 = *ptoks;
            let ghost m0 = *pm;
            self.take(&mut g, s, &mut remaining, picks, &mut np, Tracked(&mut *ptoks), Tracked(&mut *pz), Tracked(&mut *pm));
            proof {
                lemma_picks_zero_step(self.spec_table(), before, picks@, k as int, z0, *pz);
                lemma_picks_sum(before, picks@, k as int);
                lemma_picks_toks_step(self.spec_table(), before, picks@, k as int, t0, *ptoks);
                lemma_picks_marks_step(self.spec_table(), before, picks@, k as int, m0, *pm);
                assert forall|j: int| 0 <= j < k + 1 implies slab_owner(self.spec_class() as int, (#[trigger] picks@[j]).slab as int) == self.arena by {
                    if j < k {
                        assert(picks@[j] == before[j]);
                    }
                }
            }
        }
        proof {
            lemma_picks_sum(picks@, picks@, np as int);
            // Dry: the scan saw every partial slab and `next_slab` found
            // both idle sets empty.
            if np == 0 && want > 0 {
                assert(dry);
            }
            *ex = Ghost((g@, seen));
        }
        np
    }

    /// Reserve `min(nslots - live, *remaining)` slots of `slab` under the
    /// lock, fix up its state and partial bit, and record the pick.
    /// Modelled at `model::Model::take`, inside the locked step
    /// `protocol::Refill::PlanLocked` of `model::Model::step_refill`.
    ///
    /// `live` is not modelled (releasers subtract from it without the
    /// lock), so `live <= nslots` stays a runtime check that aborts
    /// ([`live_above_slots`]; the release profile would wrap without it).
    /// The state and stamp writes go through the bridges, and `ready` is
    /// what makes the invariant true afterwards.
    // The negated test is the form the proof states; the last three
    // arguments are tracked state.
    #[allow(clippy::nonminimal_bool, clippy::too_many_arguments)]
    fn take(
        &self,
        g: &mut Inner,
        slab: usize,
        remaining: &mut usize,
        picks: &mut [Pick; MAX_PICKS],
        np: &mut usize,
        Tracked(ptoks): Tracked<&mut Map<int, GhostSubset<int>>>,
        Tracked(pz): Tracked<&mut Map<int, Zeroed>>,
        Tracked(pm): Tracked<&mut Map<int, GhostPersistentSubset<int>>>,
    )
        requires
            crate::sys::may_abort(),
            old(g).ready(slab as nat),
            old(g).nbits() == self.spec_nslabs(),
            old(g).arena@ == self.arena,
            old(g).table@ == self.spec_table(),
            slab < self.spec_nslabs(),
            *old(np) < MAX_PICKS,
        ensures
            final(g).inv(),
            final(g).nbits() == old(g).nbits(),
            final(g).states@ == old(g).states@.insert(slab as nat, State::Active),
            final(g).committed == old(g).committed,
            final(g).dirty@ == (if old(g).st(slab as nat) == State::Active { old(g).dirty@ } else { old(g).dirty@.insert(slab as nat, 0) }),
            final(g).group_next == old(g).group_next,
            final(g).group_end == old(g).group_end,
            final(g).arena == old(g).arena,
            final(g).table == old(g).table,
            final(g).oauth@.id() == old(g).oauth@.id(),
            *final(np) == *old(np) + 1,
            final(picks)[*old(np) as int].n as int + *final(remaining) == *old(remaining),
            (final(picks)[*old(np) as int].slab as usize) < self.spec_nslabs(),
            final(picks)[*old(np) as int].n <= class_table()[self.spec_class() as int].nslots,
            forall|i: int| 0 <= i < MAX_PICKS && i != *old(np) ==> final(picks)[i] == old(picks)[i],
            *final(ptoks) == old(ptoks).insert(*old(np) as int, final(ptoks)[*old(np) as int]),
            final(ptoks)[*old(np) as int].id() == self.spec_table().spec_slots().loc,
            final(ptoks)[*old(np) as int]@ == pick_addrs(self.spec_table(), final(picks)[*old(np) as int]),
            *final(pz) == old(pz).insert(*old(np) as int, final(pz)[*old(np) as int]),
            pick_zero(self.spec_table(), final(picks)[*old(np) as int], final(pz)[*old(np) as int]),
            *final(pm) == old(pm).insert(*old(np) as int, final(pm)[*old(np) as int]),
            final(pm)[*old(np) as int].id() == self.spec_table().spec_slots().mloc,
            slab_marked(self.spec_table(), final(picks)[*old(np) as int].slab as int, final(pm)[*old(np) as int]@),
            !self.spec_table().spec_is_guard(final(picks)[*old(np) as int].slab as usize),
            slab_owner(self.spec_class() as int, final(picks)[*old(np) as int].slab as int) == self.arena,
            // A slab this hold did not fill stays where a refill finds it.
            final(picks)[*old(np) as int].n < class_table()[self.spec_class() as int].nslots
                ==> final(g).partial.map@.contains(slab as nat),
    {
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
            // The slab is owned.
            assert(g.states@.contains_key(slab as nat));
        }
        let desc = self.desc(slab);
        let live = desc.live();
        self.take_checks(&*g, desc, slab, live);
        // `live > nslots` would reserve a wrapped count (the release
        // profile does not check overflow; the dev profile does).
        if !(live <= self.class().nslots()) {
            live_above_slots();
        }
        let avail = self.class().nslots() - live;
        let n = avail.min(*remaining);
        let state = self.state_locked(&*g, desc, slab);
        if purge::check_zero() {
            self.check_shadow(slab, "take");
            if state == State::Fresh {
                if live != 0 {
                    fresh_with_live(self.class as usize, slab, self.arena, desc, self.table().bits(slab).allocated(self.class().nslots()));
                }
                self.check_fresh_slab_is_zero(slab);
            }
        }
        // The state byte the slab's descriptor permission records while it
        // is still `Fresh` or `Purged`: what the zero claim below rests on,
        // after the slab is `Active`.
        proof {
            g.lemma_desc(slab as nat);
        }
        let tracked ticket = g.dperms.borrow().tracked_borrow(slab as nat).ticket();
        proof {
            lemma_state_byte_inj(state, g.st(slab as nat));
        }
        let ghost g0 = *g;
        if state != State::Active {
            self.set_state_locked(g, desc, slab, State::Active);
            self.set_dirty_locked(g, desc, slab, 0);
        } else {
            proof {
                assert(g.states@.insert(slab as nat, State::Active) =~= g.states@);
            }
        }
        proof {
            assert(g.slab_ok(slab as nat));
            assert forall|x: nat| x != slab implies #[trigger] g.slab_ok(x) by {
                assert(g0.slab_ok(x));
            }
            assert(g.inv_but(slab as int));
            g.lemma_close(slab as nat);
        }
        desc.add_live(n);
        // Under the lock (`g`), so a plain load-add-store is exclusive.
        self.claimed.store(self.claimed.load(Ordering::Relaxed).wrapping_add(n as u64), Ordering::Relaxed);
        if live == 0 && n == avail {
            g.clear_partial(slab);
        } else {
            g.set_partial(slab);
        }
        // `Purged` memory is zero only while every discard so far was
        // `MADV_DONTNEED` (`purge::purged_is_zero`); the read comes with its
        // witness, which the zero claim of a `Purged` slab uses up.
        let (purged_zero, _purged_witness) = if state == State::Purged {
            purge::purged_zero()
        } else {
            (true, Tracked(None))
        };
        let zero = state.is_zero() && purged_zero;
        // The table, named for the proof's facts about it.
        let _t = self.table();
        let tracked mut pt = _t.empty_tokens();
        if zero {
            self.claim_prefix(slab, n, state, live, Tracked(&mut pt), Tracked(g.inited.borrow()));
        }
        // The zero pick's bytes, slots `0..n` of the slab: a `Fresh` slab
        // was never handed out since its pages were mapped, so they are
        // fresh anonymous memory (`axiom_fresh_slots_zero`); a `Purged` one
        // was discarded with `MADV_DONTNEED` in the lock hold that made it
        // `Purged` and has had no allocated slot since, and
        // `purged_is_zero` read true in this hold (its witness,
        // `axiom_discarded_slots_zero`). `ticket` is the state byte the
        // slab's descriptor permission recorded before `take` made the slab
        // `Active`, and `pt` holds the uniqueness tokens of the slots
        // `claim_prefix` claimed in this hold.
        proof {
            _t.lemma_slots_wf();
        }
        let tracked zt = if zero {
            if state == State::Fresh {
                axiom_fresh_slots_zero(ticket, *_t, slab as int, n as int, &pt)
            } else {
                axiom_discarded_slots_zero(ticket, _purged_witness.get().tracked_unwrap(), *_t, slab as int, n as int, &pt)
            }
        } else {
            Zeroed::empty(0)
        };
        proof {
            assert(self.spec_nslabs() <= 0x400_0000) by {
                broadcast use {table::lemma_spec_nslabs};
            }
        }
        picks[*np] = Pick { slab: slab as u32, n: n as u32, zero, state };
        proof {
            assert(VSet::<int>::empty().union(prefix_addrs(self.spec_table(), slab as int, n as int))
                =~= prefix_addrs(self.spec_table(), slab as int, n as int));
            ptoks.tracked_insert(*np as int, pt);
            pz.tracked_insert(*np as int, zt);
            pm.tracked_insert(*np as int, g.inited.borrow().duplicate());
        }
        *np += 1;
        *remaining -= n;
    }

    /// `take`'s debug checks: the slab's owner word and its descriptor's
    /// arena name this arena. Inlined and `track_caller`, so a failure
    /// names `take`. Neither fails: an owned slab's owner is this arena
    /// ([`Inner::base`]), and the descriptor of an owned slab that is not a
    /// guard slab records it ([`Inner::descs`]: `take_group` wrote it with
    /// the owner word).
    #[allow(unused_variables)]
    #[inline(always)]
    #[track_caller]
    fn take_checks(&self, g: &Inner, desc: &SlabDesc, slab: usize, live: usize)
        requires
            crate::sys::may_abort(),
            g.base(),
            g.owns(slab as nat),
            !self.spec_table().spec_is_guard(slab),
            g.arena@ == self.arena,
            g.table@ == self.spec_table(),
            *desc == desc_at(self.spec_table(), slab),
    {
        proof {
            g.lemma_desc(slab as nat);
        }
        if cfg!(debug_assertions) {
            let owner = self.table().owner(slab).0;
            let arena = desc.arena_held(Ghost(slab as nat), Tracked(g.dperms.borrow()));
            debug_assert!(
                owner == self.arena,
                "take: slab {slab} state {:?} live {live}: owner {owner}, not arena {}",
                desc.state(),
                self.arena
            );
            debug_assert!(
                arena == self.arena,
                "take: slab {slab} descriptor arena {arena}, not arena {}",
                self.arena
            );
        }
    }

    /// The slots of a zero pick, decided under the class lock. A `Fresh`
    /// or `Purged` slab has `live == 0` and no bit
    /// set, so its lowest `n` bits are free and nothing contends for them.
    /// Setting them here, in the lock hold that makes the slab `Active` and
    /// partial, is what makes the pick's zero flag sound: decided after
    /// the lock is dropped (the bit loop of [`finish`]), a bit could be one
    /// that another refill claimed from the now-partial slab, handed out,
    /// and got back dirty in the meantime. Anything but a clean claim of
    /// bits `0..n` means `live` no longer bounds the set bits: abort.
    ///
    /// [`finish`]: ClassState::finish
    fn claim_prefix(
        &self,
        slab: usize,
        n: usize,
        state: State,
        live: usize,
        Tracked(toks): Tracked<&mut GhostSubset<int>>,
        Tracked(inited): Tracked<&GhostPersistentSubset<int>>,
    )
        requires
            crate::sys::may_abort(),
            n <= class_table()[self.spec_class() as int].nslots,
            slab < self.spec_nslabs(),
            old(toks).id() == self.spec_table().spec_slots().loc,
            inited.id() == self.spec_table().spec_slots().mloc,
            slab_marked(self.spec_table(), slab as int, inited@),
        ensures
            final(toks).id() == old(toks).id(),
            final(toks)@ == old(toks)@.union(prefix_addrs(self.spec_table(), slab as int, n as int)),
            old(toks)@.disjoint(prefix_addrs(self.spec_table(), slab as int, n as int)),
    {
        let bm = self.table().bits(slab);
        let ghost sb = class_table()[self.spec_class() as int].slab_bits as usize;
        let ghost s = self.spec_table().spec_slots();
        let ghost f = slab * s.sb;
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
            self.spec_table().lemma_slots_wf();
            broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter, VSet::lemma_map_contains};
            assert(prefix_addrs(self.spec_table(), slab as int, 0) =~= VSet::<int>::empty());
            assert(old(toks)@.union(VSet::<int>::empty()) =~= old(toks)@);
        }
        let mut k = 0;
        let mut w = 0;
        while k < n
            invariant
                crate::sys::may_abort(),
                bits_shape(bm, sb),
                bits_ghost(bm, self.spec_table(), slab),
                bm.inited_by(*inited),
                self.spec_table().inv(),
                self.spec_table().spec_class() == self.spec_class(),
                self.spec_table().spec_nslabs() == self.spec_nslabs(),
                n <= sb,
                n <= s.nslots,
                k <= n,
                k < n ==> k == w * 64,
                slab < self.spec_nslabs(),
                s == self.spec_table().spec_slots(),
                s.wf(),
                sb == s.sb,
                f == slab * s.sb,
                toks.id() == old(toks).id(),
                old(toks).id() == s.loc,
                toks@ == old(toks)@.union(prefix_addrs(self.spec_table(), slab as int, k as int)),
                old(toks)@.disjoint(prefix_addrs(self.spec_table(), slab as int, k as int)),
            decreases n - k,
        {
            proof {
                assert(w as int * 64 < sb as int);
                assert(w < bm.spec_words()) by (nonlinear_arith)
                    requires
                        w as int * 64 < sb as int,
                        sb >= 64 ==> sb % 64 == 0 && bm.spec_words() == sb / 64,
                        sb < 64 ==> bm.spec_words() == 1;
            }
            let want = (n - k).min(64);
            proof {
                assert(want < 64 ==> (1u64 << want) >= 1u64) by (bit_vector);
            }
            let expect = if want == 64 { !0 } else { (1u64 << want) - 1 };
            let got = bm.claim_word(w, want, Tracked(&mut *toks), Tracked(inited));
            if got != expect {
                zero_slab_had_bits_set(self.class as usize, slab, self.arena, state, live, w, got, expect);
            }
            proof {
                broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
                let wu = want as u64;
                assert forall|kk: u64| kk < 64 implies #[trigger] has_bit(expect, kk) == (kk < wu) by {
                    assert(has_bit(!0u64, kk)) by (bit_vector) requires kk < 64;
                    if wu < 64 {
                        assert(has_bit(sub(1u64 << wu, 1), kk) == (kk < wu)) by (bit_vector)
                            requires wu < 64, kk < 64;
                    }
                }
                self.spec_table().lemma_slots_wf();
                // The claimed word is slots `k..k + want` of the slab.
                let a = prefix_addrs(self.spec_table(), slab as int, k as int);
                let won = bm.tokens(w as int, got);
                let lo = VSet::<int>::range(f, f + k);
                let hi = VSet::<int>::range(f + k, f + k + want);
                assert forall|b: int| #[trigger] won.contains(b) == hi.contains(b) by {
                    if f + k <= b < f + k + want {
                        let t = b - f;
                        assert(0 <= t < s.nslots);
                        self.spec_table().lemma_slot_bit(slab as int, t);
                    }
                }
                assert(won =~= hi);
                assert(VSet::<int>::range(f, f + k + want) =~= lo.union(hi));
                lemma_addrs_union(s, lo, hi);
            }
            k += want;
            w += 1;
        }
    }

}

/// Every set bit of `x` in word `word` of a slab is a slot (below
/// `nslots`).
pub open spec fn mask_slots(word: int, x: u64, nslots: int) -> bool {
    forall|k: u64| k < 64 && #[trigger] has_bit(x, k) ==> word * 64 + k < nslots
}

/// The slots set in the scratch words `local[lo..=hi]` of the slab whose
/// slot 0 is bit `f`.
pub open spec fn scratch_bits(s: Slots, f: int, local: Seq<u64>, lo: int, hi: int) -> VSet<int> {
    VSet::<int>::range(f + 64 * lo, f + 64 * hi + 64).filter(
        |b: int| has_bit(local[(b - f) / 64], ((b - f) % 64) as u64) && s.real(b))
}

/// The scratch words `lo..=hi` are word `lo` and the words after it.
proof fn lemma_scratch_split(s: Slots, f: int, local: Seq<u64>, lo: int, hi: int)
    requires
        0 <= lo <= hi,
    ensures
        scratch_bits(s, f, local, lo, hi) == mask_bits(s, f, lo, local[lo]).union(scratch_bits(s, f, local, lo + 1, hi)),
        mask_bits(s, f, lo, local[lo]).disjoint(scratch_bits(s, f, local, lo + 1, hi)),
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
    let a = mask_bits(s, f, lo, local[lo]);
    let r = scratch_bits(s, f, local, lo + 1, hi);
    assert forall|b: int| #[trigger] scratch_bits(s, f, local, lo, hi).contains(b) == a.union(r).contains(b) by {
        if f + 64 * lo <= b < f + 64 * lo + 64 {
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(b - f, 64, lo, b - f - 64 * lo);
        }
    }
    assert(scratch_bits(s, f, local, lo, hi) =~= a.union(r));
}


/// Setting bit `k` of scratch word `word` adds that one slot, and the range
/// grows to hold the word; words outside the old range were zero.
proof fn lemma_scratch_insert(
    s: Slots,
    f: int,
    l_old: Seq<u64>,
    l_new: Seq<u64>,
    lo: int,
    hi: int,
    lo2: int,
    hi2: int,
    word: int,
    k: u64,
    empty: bool,
)
    requires
        0 <= lo2 <= word <= hi2 < l_old.len(),
        l_new.len() == l_old.len(),
        k < 64,
        s.real(f + 64 * word + k),
        forall|v: int| 0 <= v < l_old.len() && v != word ==> #[trigger] l_new[v] == l_old[v],
        forall|k2: u64| k2 < 64 ==> #[trigger] has_bit(l_new[word], k2) == (has_bit(l_old[word], k2) || k2 == k),
        forall|v: int| 0 <= v < l_old.len() && !(lo <= v <= hi) ==> #[trigger] l_old[v] == 0,
        empty ==> forall|v: int| 0 <= v < l_old.len() ==> #[trigger] l_old[v] == 0,
        !empty ==> lo <= hi && lo2 <= lo && hi <= hi2,
    ensures
        scratch_bits(s, f, l_new, lo2, hi2) == (if empty { VSet::<int>::empty() } else { scratch_bits(s, f, l_old, lo, hi) }).insert(f + 64 * word + k),
{
    broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
    let old_set = if empty { VSet::<int>::empty() } else { scratch_bits(s, f, l_old, lo, hi) };
    let nb = f + 64 * word + k;
    assert forall|kk: u64| kk < 64 implies !#[trigger] has_bit(0u64, kk) by {
        assert((0u64 >> kk) & 1u64 != 1u64) by (bit_vector);
    }
    assert forall|b: int| #[trigger] scratch_bits(s, f, l_new, lo2, hi2).contains(b) == old_set.insert(nb).contains(b) by {
        if f + 64 * lo2 <= b < f + 64 * hi2 + 64 {
            let v = (b - f) / 64;
            let kk = ((b - f) % 64) as u64;
            vstd::arithmetic::div_mod::lemma_fundamental_div_mod(b - f, 64);
            assert(lo2 <= v <= hi2);
            if b == nb {
                vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(b - f, 64, word, k as int);
            }
            if v == word {
                assert(has_bit(l_new[v], kk) == (has_bit(l_old[v], kk) || kk == k));
                if kk == k {
                    assert(b == nb);
                }
            }
        } else {
            if !empty && f + 64 * lo <= b < f + 64 * hi + 64 {
                assert(false);
            }
            if b == nb {
                assert(false);
            }
        }
    }
    assert(scratch_bits(s, f, l_new, lo2, hi2) =~= old_set.insert(nb));
}

/// The process's regions are well formed and every pointer of `ptrs` is a
/// slot of class `c` outside the guard slabs, as the slots of live blocks
/// are.
pub open spec fn live_slots(c: int, ptrs: Seq<*mut u8>) -> bool {
    &&& crate::heap::regions_ok()
    &&& forall|i: int| 0 <= i < ptrs.len() ==> #[trigger] process_regions().is_slot(c, ptrs[i]@.addr)
}

/// [`live_slots`], each in a slab `arena` owns.
pub open spec fn owned_slots(c: int, arena: u16, ptrs: Seq<*mut u8>) -> bool {
    &&& live_slots(c, ptrs)
    &&& forall|i: int| 0 <= i < ptrs.len() ==> #[trigger] slot_owner(c, ptrs[i]@.addr) == arena
}

/// The addresses `a[lo..hi]`, as a set ([`lemma_aset`]).
pub closed spec fn aset(a: Seq<int>, lo: int, hi: int) -> VSet<int> {
    vstd::set_lib::set_int_range(lo, hi).map(|i: int| a[i])
}

/// What [`aset`] contains.
pub broadcast proof fn lemma_aset(a: Seq<int>, lo: int, hi: int, x: int)
    ensures
        #[trigger] aset(a, lo, hi).contains(x) <==> exists|i: int| lo <= i < hi && a[i] == x,
{
    broadcast use {vstd::set_lib::range_set_properties, VSet::lemma_map_contains};
    if aset(a, lo, hi).contains(x) {
        let i = choose|i: int| vstd::set_lib::set_int_range(lo, hi).contains(i) && x == a[i];
        assert(lo <= i < hi);
    }
    if exists|i: int| lo <= i < hi && a[i] == x {
        let i = choose|i: int| lo <= i < hi && a[i] == x;
        assert(vstd::set_lib::set_int_range(lo, hi).contains(i));
    }
}

/// No address repeats in `a[lo..hi]`.
pub open spec fn distinct(a: Seq<int>, lo: int, hi: int) -> bool {
    forall|i: int, j: int| lo <= i < j < hi ==> a[i] != a[j]
}

/// `a[i]` is in `aset(a, lo, hi)` for `lo <= i < hi`, and, with `a`
/// distinct there, not in `aset(a, lo, i)`, which it extends to
/// `aset(a, lo, i + 1)`.
#[verifier::spinoff_prover]
pub proof fn lemma_aset_step(a: Seq<int>, lo: int, i: int, hi: int)
    requires
        lo <= i < hi,
    ensures
        aset(a, lo, hi).contains(a[i]),
        distinct(a, lo, hi) ==> !aset(a, lo, i).contains(a[i]),
        aset(a, lo, i + 1) == aset(a, lo, i).insert(a[i]),
{
    broadcast use lemma_aset;
    assert(aset(a, lo, hi).contains(a[i]));
    if distinct(a, lo, hi) && aset(a, lo, i).contains(a[i]) {
        let k = choose|k: int| lo <= k < i && a[k] == a[i];
        assert(a[k] != a[i]);
    }
    assert forall|x: int| aset(a, lo, i + 1).contains(x) <==> aset(a, lo, i).insert(a[i]).contains(x) by {
        if aset(a, lo, i + 1).contains(x) {
            let k = choose|k: int| lo <= k < i + 1 && a[k] == x;
            if k < i {
                assert(aset(a, lo, i).contains(x));
            }
        }
        if aset(a, lo, i).contains(x) {
            let k = choose|k: int| lo <= k < i && a[k] == x;
            assert(lo <= k < i + 1);
        }
    }
    assert(aset(a, lo, i + 1) =~= aset(a, lo, i).insert(a[i]));
}

/// Slot `slot` of a slab of `slab_bits <= 64 * MAX_SLAB_WORDS` bits sits in
/// scratch word `slot / 64`, inside the slab's view of the bitmap, at a bit
/// the slab owns.
proof fn lemma_slot_bit(slot: usize, slab_bits: usize)
    requires
        slot < slab_bits,
        slab_bits <= 64 * MAX_SLAB_WORDS,
        slab_bits >= 64 ==> slab_bits % 64 == 0,
    ensures
        slot / 64 < MAX_SLAB_WORDS,
        slot / 64 < words_of(slab_bits),
        subset(1u64 << ((slot % 64) as u64), slab_mask_of(slab_bits)),
{
    if slab_bits >= 64 {
        assert(slot / 64 < slab_bits / 64) by (nonlinear_arith)
            requires slot < slab_bits, slab_bits % 64 == 0;
        assert(subset(1u64 << ((slot % 64) as u64), !0u64)) by (bit_vector);
    } else {
        let s = slot as u64;
        let b = slab_bits as u64;
        assert(subset(1u64 << (s % 64), sub(1u64 << b, 1u64))) by (bit_vector)
            requires s < b, b < 64;
    }
}


impl ClassState {
    /// Release the slots of `mask` in `word` of `slab`, whose tokens are
    /// `tok` (by reference, left empty, as `AtomicBitmap::release` takes
    /// them). The `live` update is separate so a batch can do one per slab.
    /// The double-free abort cannot fire: the tokens say every bit is set
    /// (`AtomicBitmap::release`).
    // The extra arguments are ghost and tracked state.
    #[allow(clippy::too_many_arguments)]
    fn release_word(&self, slab: usize, word: usize, mask: u64, reopen: &mut Reopen, Tracked(tok): Tracked<&mut GhostSubset<int>>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            slab < self.spec_nslabs(),
            word < words_of(class_table()[self.spec_class() as int].slab_bits as usize),
            subset(mask, slab_mask_of(class_table()[self.spec_class() as int].slab_bits as usize)),
            mask_slots(word as int, mask, class_table()[self.spec_class() as int].nslots as int),
            old(tok).id() == self.spec_table().spec_slots().loc,
            old(tok)@ == word_addrs(self.spec_table(), slab as int, word as int, mask),
            old(reopen).len <= MAX_PICKS,
            old(reopen).below(self.spec_nslabs()),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
        ensures
            final(reopen).len <= MAX_PICKS,
            final(reopen).below(self.spec_nslabs()),
            *final(ord) == *old(ord),
            final(tok).id() == old(tok).id(),
            final(tok)@.is_empty(),
    {
        let ghost sb = class_table()[self.spec_class() as int].slab_bits as usize;
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
        }
        let t = self.table();
        let bm = t.bits(slab);
        proof {
            use_type_invariant(t);
            assert forall|b: u64| b < 64 && #[trigger] has_bit(mask, b)
                implies bm.spec_slots().real(bm.spec_first() + 64 * word + b) by {
                assert(subset(mask, mask)) by (bit_vector);
                lemma_bit_in_slab(mask, mask, b, sb, word);
                t.lemma_slot_bit(slab as int, 64 * word + b);
            }
        }
        let (prev, was_full) = bm.release(word, mask, Tracked(tok));
        if prev & mask != mask {
            proof {
                // The released slots' bits were set: `tok` holds their
                // tokens, so the word did not (`AtomicBitmap::release`).
                assert(prev & mask == mask) by (bit_vector)
                    requires mask & !prev == 0u64;
                assert(false);
                let x = mask & !prev;
                assert(x != 0u64 && subset(x, mask)) by (bit_vector)
                    requires prev & mask != mask, x == mask & !prev;
                axiom_u64_trailing_zeros(x);
                lemma_bit_in_slab(x, mask, u64_trailing_zeros(x) as u64, sb, word);
            }
            let bad = (mask & !prev).trailing_zeros() as usize;
            let slot = word * 64 + bad;
            double_free(self.slot_ptr(slab, slot) as usize, self.class as usize, slab, slot);
        }
        if was_full && !reopen.push(slab) {
            self.reopen(reopen, pubw, Tracked(ord));
            reopen.push(slab);
        }
    }

    /// Put the slabs in `reopen` back in the partial set, then clear the
    /// list. A slab that went `Empty` (or was purged) since its word was
    /// released stays out: it is in the empty or purged set, and a refill
    /// taking it from there sets its partial bit itself. Modelled at
    /// `model::Model::step_release`, `protocol::Release::LockReopen` (the
    /// lock) and `Reopen` (the state and owner test per slab).
    // Verus needs the explicit reborrow.
    #[allow(clippy::explicit_auto_deref)]
    fn reopen(&self, reopen: &mut Reopen, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(reopen).len <= MAX_PICKS,
            old(reopen).below(self.spec_nslabs()),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
        ensures
            final(reopen).len == 0,
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        if reopen.len == 0 {
            return;
        }
        let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        for &s in it: &reopen.slabs[..reopen.len]
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                g@.inv(),
                g@.nbits() == self.spec_nslabs(),
                g@.arena@ == self.arena,
                g@.table@ == self.spec_table(),
                g@.oauth@.id() == self.spec_oloc(),
                self.published_by(*pubw@),

                it.seq().len() == reopen.len,
                forall|i: int| 0 <= i < reopen.len ==> *#[trigger] it.seq()[i] == reopen.slabs[i],
                reopen.len <= MAX_PICKS,
                reopen.below(self.spec_nslabs()),
        {
            proof {
                assert(s == reopen.slabs[it.index() as int]);
                assert(s < self.spec_nslabs());
            }
            let slab = s as usize;
            // Between the word's clear and this lock hold the slab may have
            // gone `Empty` (in `dec_live`); only an `Active` slab of this
            // arena goes back into the partial set.
            if self.state_locked(&*g, self.desc(slab), slab) == State::Active && self.owner_locked(&*g, slab, pubw) == self.arena {
                g.set_partial(slab);
            }
        }
        reopen.len = 0;
    }

    fn dec_live(&self, slab: usize, n: usize, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            slab < self.spec_nslabs(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
        ensures
            *final(ord) == *old(ord),
    {
        if purge::check_zero() {
            self.check_shadow(slab, "release");
            if self.desc(slab).state() == State::Fresh {
                release_into_fresh(self.class as usize, slab, self.arena, self.desc(slab), n);
            }
        }
        if self.desc(slab).sub_live(n) == n {
            self.on_live_zero(slab, pubw, Tracked(ord));
        }
    }

    /// `live` reached zero: make the slab `Empty` unless a refill has
    /// already reserved it again. Modelled at `model::Model::step_release`,
    /// `protocol::Release::LockLiveZero` (the lock) and `OnLiveZero` (the
    /// owner test and the transition).
    // Verus needs the explicit reborrow.
    #[allow(clippy::explicit_auto_deref)]
    fn on_live_zero(&self, slab: usize, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            slab < self.spec_nslabs(),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        let mut first = false;
        {
            let mut g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
            if purge::check_zero() {
                self.check_shadow(slab, "on_live_zero");
            }
            // A slab of another arena is no business of this arena's sets.
            if self.owner_locked(&*g, slab, pubw) != self.arena {
                return;
            }
            let desc = self.desc(slab);
            if desc.live() == 0 && self.state_locked(&*g, desc, slab) == State::Active {
                // Never 0: an `Empty` slab's stamp is not 0
                // (`Inner::slab_core`).
                let now = crate::sys::now_coarse_ns().max(1);
                let ghost g0 = g@;
                proof {
                    assert(g0.slab_ok(slab as nat));
                }
                self.set_state_locked(&mut *g, desc, slab, State::Empty);
                self.set_dirty_locked(&mut *g, desc, slab, now);
                proof {
                    assert forall|s: nat| s != slab implies #[trigger] g@.slab_ok(s) by {
                        assert(g0.slab_ok(s));
                    }
                    assert(g@.inv_but(slab as int));
                }
                g.clear_partial(slab);
                g.empty_add(slab);
                proof {
                    assert(g@.slab_ok(slab as nat));
                    g@.lemma_close(slab as nat);
                }
                if self.gate.note_first_empty(now) {
                    first = true;
                }
            }
        }
        if first {
            purge::note_empty();
        }
    }

    /// Clear the bits `local[lo..=hi]` accumulated for `slab` (one atomic
    /// per non-zero word), zero them for the next slab, and take `count`
    /// off the slab's `live`. The words are inside the slab's view of the
    /// class bitmap and hold only bits of it; `toks` holds the tokens of
    /// exactly their slots, and each word's go back with its release, so
    /// `toks` is left empty (by reference, as `AtomicBitmap::release`).
    // Hot free path: indexing keeps the codegen as measured.
    #[allow(clippy::needless_range_loop)]
    // The extra arguments are ghost and tracked state.
    #[allow(clippy::too_many_arguments)]
    fn release_slab(
        &self,
        slab: usize,
        local: &mut [u64; MAX_SLAB_WORDS],
        lo: usize,
        hi: usize,
        count: usize,
        reopen: &mut Reopen,
        Tracked(toks): Tracked<&mut GhostSubset<int>>,
        pubw: Tracked<&'static Published>,
        Tracked(ord): Tracked<&mut LockOrder>,
    )
        requires
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            self.published_by(*pubw@),
            slab < self.spec_nslabs(),
            lo <= hi,
            hi < old(local)@.len(),
            hi < words_of(class_table()[self.spec_class() as int].slab_bits as usize),
            forall|v: int| lo <= v <= hi ==> subset(#[trigger] old(local)@[v], slab_mask_of(class_table()[self.spec_class() as int].slab_bits as usize)),
            forall|v: int| lo <= v <= hi ==> mask_slots(v, #[trigger] old(local)@[v], class_table()[self.spec_class() as int].nslots as int),
            old(toks).id() == self.spec_table().spec_slots().loc,
            old(toks)@ == addrs(self.spec_table().spec_slots(), scratch_bits(self.spec_table().spec_slots(),
                slab * self.spec_table().spec_slots().sb, old(local)@, lo as int, hi as int)),
            old(reopen).len <= MAX_PICKS,
            old(reopen).below(self.spec_nslabs()),
            old(ord).allows(rank_class(0, 0)),
        ensures
            final(reopen).len <= MAX_PICKS,
            final(reopen).below(self.spec_nslabs()),
            forall|v: int| lo <= v <= hi ==> #[trigger] final(local)@[v] == 0,
            forall|v: int| 0 <= v < old(local)@.len() && !(lo <= v <= hi) ==> #[trigger] final(local)@[v] == old(local)@[v],
            *final(ord) == *old(ord),
            final(toks).id() == old(toks).id(),
            final(toks)@.is_empty(),
    {
        let ghost s = self.spec_table().spec_slots();
        let ghost f = slab * s.sb;
        let tracked mut toks = toks.take();
        proof {
            use_type_invariant(self);
            self.spec_table().lemma_slots_wf();
        }
        for w in it: lo..=hi
            invariant
                crate::sys::may_abort() || crate::sys::valid_aborts(),
                self.published_by(*pubw@),
                crate::sys::may_syscall(),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                slab < self.spec_nslabs(),
                hi < old(local)@.len(),
                hi < words_of(class_table()[self.spec_class() as int].slab_bits as usize),
                forall|v: int| lo <= v <= hi ==> subset(#[trigger] old(local)@[v], slab_mask_of(class_table()[self.spec_class() as int].slab_bits as usize)),
                forall|v: int| lo <= v <= hi ==> mask_slots(v, #[trigger] old(local)@[v], class_table()[self.spec_class() as int].nslots as int),
                forall|v: int| 0 <= v < old(local)@.len() && !(lo <= v < lo + it.index()) ==> #[trigger] local@[v] == old(local)@[v],
                forall|v: int| lo <= v < lo + it.index() ==> #[trigger] local@[v] == 0,
                lo + it.index() <= hi + 1,
                reopen.len <= MAX_PICKS,
                reopen.below(self.spec_nslabs()),
                s == self.spec_table().spec_slots(),
                s.wf(),
                f == slab * s.sb,
                toks.id() == s.loc,
                toks@ == addrs(s, scratch_bits(s, f, old(local)@, lo + it.index(), hi as int)),
        {
            let ghost i = lo + it.index();
            proof {
                lemma_scratch_split(s, f, old(local)@, i, hi as int);
                let a = mask_bits(s, f, i, old(local)@[i]);
                let r = scratch_bits(s, f, old(local)@, i + 1, hi as int);
                lemma_addrs_union(s, a, r);
                broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};
                lemma_addrs_difference(s, a.union(r), a);
                assert(a.union(r).difference(a) =~= r);
            }
            let mask = local[w];
            let tracked mut tok = toks.split(word_addrs(self.spec_table(), slab as int, w as int, mask));
            if mask != 0 {
                local[w] = 0;
                self.release_word(slab, w, mask, reopen, Tracked(&mut tok), pubw, Tracked(ord));
            }
        }
        self.dec_live(slab, count, pubw, Tracked(ord));
    }

    /// Release a batch of slot pointers. Consecutive pointers in the same
    /// bitmap word share one `fetch_and`, so a batch sorted by address costs
    /// about one atomic per 64 slots. Slabs whose words went from full to
    /// not full are collected and put back in the partial set under one
    /// lock hold at the end (or when the list overflows), rather than one
    /// per word. Aborts on a pointer that is not an allocated slot of this
    /// class, or one whose slab belongs to another arena (the caller routes
    /// by the descriptor's arena; a miss here means a cache was mis-keyed).
    ///
    /// `bag` holds the slot tokens of the batch, whose addresses are
    /// distinct ([`release_owned`]); the released ones go back to their
    /// bitmap words.
    ///
    /// [`release_owned`]: ClassState::release_owned
    pub fn release_with(&self, ptrs: &[*mut u8], Tracked(bag): Tracked<GhostSubset<int>>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && owned_slots(self.spec_class() as int, self.spec_arena(), ptrs@)),
            crate::sys::may_syscall(),
            self.published_by(*pubw@),
            bag.id() == slot_loc(self.spec_class() as int),
            aset(ptr_addrs(ptrs@), 0, ptrs@.len() as int) <= bag@,
            distinct(ptr_addrs(ptrs@), 0, ptrs@.len() as int),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            // Arena isolation: it returns only if every slot's slab is
            // this arena's; otherwise `wrong_arena` aborts.
            forall|i: int| 0 <= i < ptrs@.len() ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
    {
        broadcast use lemma_aset;
        let tracked mut bag = bag;
        let n = self.release_owned(ptrs, Tracked(&mut bag), pubw, Tracked(ord));
        if n != ptrs.len() {
            proof {
                // The first slot left is another arena's.
                assert(!owned_slots(self.spec_class() as int, self.spec_arena(), ptrs@));
            }
            let p = ptrs[n];
            let (slab, _) = self.locate(p);
            wrong_arena(p as usize, self.class as usize, self.table().owner(slab).0, self.arena);
        }
    }

    /// [`release_with`] of the longest prefix of `ptrs` whose slabs this arena
    /// owns, returning its length (`ptrs.len()` when every slab is ours).
    /// The batch is grouped by slab (`Heap::flush`'s bucket pass; a sorted
    /// batch is one such grouping), so a slab's pointers are contiguous and
    /// the prefix ends exactly at the first pointer of a slab of another
    /// arena; the caller routes the rest there (`Heap::flush`: a home cache
    /// takes sized frees of any arena, design 6.3). Within a slab the order
    /// is free: the slab's bits are accumulated in a scratch bitmap
    /// (`MAX_SLAB_WORDS` words on the stack) and released at the slab
    /// switch, one atomic per touched word, so an unsorted slab costs the
    /// same atomics as a sorted one. Every bit cleared is checked (an
    /// already-clear bit is a double or invalid free and aborts), which is
    /// where the sized path's deferred check lands; a duplicate inside one
    /// slab's group is caught earlier, on the scratch bit. The owner is read
    /// from the table's dense owner array, once per slab switch, and checked
    /// against the slab descriptor's `arena` (a line `dec_live` reads
    /// anyway): the word routed the batch here, so a descriptor that
    /// disagrees means one of them is corrupt, and the batch aborts before
    /// a bit is cleared (`owner_mismatch`).
    /// Modelled at `model::Model::step_release`, `protocol::Release::FetchAnd`
    /// (the `fetch_and` and the double-free check) and `SubLive`.
    ///
    /// The tokens: `bag` holds the token of every pointer of the batch,
    /// whose addresses are distinct, and gives up those of the released
    /// prefix, which go back to their bitmap words (`release_slab`). A batch
    /// with one slot twice cannot be given its tokens, and a slot whose bit
    /// is clear has its token in the word, so for a caller that holds the
    /// batch's tokens both double-free aborts (the scratch bit and the
    /// class bit) are unreachable.
    ///
    /// [`release_with`]: ClassState::release_with
    // Hot free path: the counter keeps the codegen as measured (and carries
    // a Verus invariant).
    #[allow(clippy::explicit_counter_loop)]
    pub fn release_owned(&self, ptrs: &[*mut u8], Tracked(bag): Tracked<&mut GhostSubset<int>>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: usize)
        requires
            crate::sys::may_abort() || (crate::sys::valid_aborts() && live_slots(self.spec_class() as int, ptrs@)),
            crate::sys::may_syscall(),
            old(bag).id() == slot_loc(self.spec_class() as int),
            aset(ptr_addrs(ptrs@), 0, ptrs@.len() as int) <= old(bag)@,
            distinct(ptr_addrs(ptrs@), 0, ptrs@.len() as int),
            old(ord).allows(rank_class(0, 0)),
            self.published_by(*pubw@),
        ensures
            *final(ord) == *old(ord),
            r <= ptrs@.len(),
            final(bag).id() == old(bag).id(),
            final(bag)@ == old(bag)@ - aset(ptr_addrs(ptrs@), 0, r as int),
            // Arena isolation: every released slot's slab is this arena's,
            // and the first one left is another arena's.
            forall|i: int| 0 <= i < r ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[i]@.addr) == self.spec_arena(),
            r < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[r as int]@.addr) != self.spec_arena(),
    {
        broadcast use lemma_aset;
        let ghost bag0 = *bag;
        let ghost pa = ptr_addrs(ptrs@);
        let mut local = [0u64; MAX_SLAB_WORDS];
        let mut cur_slab = usize::MAX;
        let (mut lo_w, mut hi_w) = (usize::MAX, 0usize);
        let mut count = 0usize;
        let mut reopen = Reopen::new();
        let mut end = ptrs.len();
        let ghost sb = class_table()[self.spec_class() as int].slab_bits as usize;
        let ghost nslots = class_table()[self.spec_class() as int].nslots as int;
        let _t = self.table();
        let ghost s = _t.spec_slots();
        // The tokens of the slots accumulated in `local` for `cur_slab`.
        let tracked mut ltoks = _t.empty_tokens();
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
            _t.lemma_slots_wf();
            assert(local@.len() == MAX_SLAB_WORDS);
            assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies subset(#[trigger] local@[v], slab_mask_of(sb)) by {
                let m = slab_mask_of(sb);
                assert(local@[v] == 0);
                assert(subset(0u64, m)) by (bit_vector);
            }
            assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies mask_slots(v, #[trigger] local@[v], nslots) by {
                assert forall|k: u64| k < 64 implies !#[trigger] has_bit(local@[v], k) by {
                    assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                }
            }
            assert(aset(pa, 0, 0) =~= VSet::<int>::empty());
            assert(bag@ =~= bag0@ - aset(pa, 0, 0));
        }
        let mut i = 0usize;
        for &p in it: ptrs.iter()
            invariant_except_break
                crate::sys::may_syscall(),
                i == it.index(),
                end == ptrs@.len(),
            invariant
                crate::sys::may_abort() || (crate::sys::valid_aborts() && live_slots(self.spec_class() as int, ptrs@)),
                self.published_by(*pubw@),
                ord.allows(rank_class(0, 0)),
                *ord == *old(ord),
                end == ptrs@.len() || end == i,
                i <= ptrs@.len(),
                end <= ptrs@.len(),
                it.seq().len() == ptrs@.len(),
                class_table()[self.spec_class() as int].wf(),
                sb == class_table()[self.spec_class() as int].slab_bits,
                nslots == class_table()[self.spec_class() as int].nslots,
                local@.len() == MAX_SLAB_WORDS,
                forall|v: int| 0 <= v < MAX_SLAB_WORDS ==> subset(#[trigger] local@[v], slab_mask_of(sb)),
                forall|v: int| 0 <= v < MAX_SLAB_WORDS ==> mask_slots(v, #[trigger] local@[v], nslots),
                forall|v: int| 0 <= v < MAX_SLAB_WORDS && !(lo_w <= v <= hi_w) ==> #[trigger] local@[v] == 0,
                count <= i,
                count != 0 ==> cur_slab < self.spec_nslabs() && lo_w <= hi_w && hi_w < words_of(sb),
                count == 0 ==> lo_w == usize::MAX && hi_w == 0,
                reopen.len <= MAX_PICKS,
                reopen.below(self.spec_nslabs()),
                *_t == self.spec_table(),
                self.spec_table().inv(),
                _t.spec_class() == self.spec_class(),
                _t.spec_nslabs() == self.spec_nslabs(),
                s == _t.spec_slots(),
                s.wf(),
                ltoks.id() == s.loc,
                count == 0 ==> ltoks@ == VSet::<int>::empty(),
                bag.id() == slot_loc(self.spec_class() as int),
                bag0.id() == slot_loc(self.spec_class() as int),
                pa == ptr_addrs(ptrs@),
                aset(pa, 0, ptrs@.len() as int) <= bag0@,
                distinct(pa, 0, ptrs@.len() as int),
                bag@ == bag0@ - aset(pa, 0, i as int),
                count != 0 ==> ltoks@ == addrs(s, scratch_bits(s, cur_slab * s.sb, local@, lo_w as int, hi_w as int)),
                cur_slab != usize::MAX ==> slab_owner(self.spec_class() as int, cur_slab as int) == self.arena,
                forall|j: int| 0 <= j < i ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[j]@.addr) == self.arena,
                end < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[end as int]@.addr) != self.arena,
            ensures
                bag@ == bag0@ - aset(pa, 0, end as int),
                end <= ptrs@.len(),
                forall|j: int| 0 <= j < end ==> #[trigger] slot_owner(self.spec_class() as int, ptrs@[j]@.addr) == self.arena,
                end < ptrs@.len() ==> slot_owner(self.spec_class() as int, ptrs@[end as int]@.addr) != self.arena,
        {
            proof {
                // A live slot is a slot start of the class.
                assert(p == ptrs@[i as int]);
                if live_slots(self.spec_class() as int, ptrs@) {
                    assert(process_regions().is_slot(self.spec_class() as int, ptrs@[i as int]@.addr));
                }
            }
            let (slab, slot) = self.locate(p);
            proof {
                // The slab `locate` found is the one `slot_owner` names.
                assert(p == ptrs@[i as int]);
                process_regions().lemma_slab_index(self.spec_class() as int, slab as int, slot as int, p as usize);
            }
            if slab != cur_slab {
                if count != 0 {
                    let ghost pre = local@;
                    self.release_slab(cur_slab, &mut local, lo_w, hi_w, count, &mut reopen, Tracked(&mut ltoks), pubw, Tracked(ord));
                    proof {
                        assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies subset(#[trigger] local@[v], slab_mask_of(sb)) by {
                            let m = slab_mask_of(sb);
                            if lo_w <= v <= hi_w {
                                assert(local@[v] == 0);
                                assert(subset(0u64, m)) by (bit_vector);
                            } else {
                                assert(local@[v] == pre[v]);
                            }
                        }
                        assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies #[trigger] local@[v] == 0 by {
                            if !(lo_w <= v <= hi_w) {
                                assert(local@[v] == pre[v]);
                            }
                        }
                        assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies mask_slots(v, #[trigger] local@[v], nslots) by {
                            assert forall|k: u64| k < 64 implies !#[trigger] has_bit(local@[v], k) by {
                                assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                            }
                        }
                    }
                    count = 0;
                    lo_w = usize::MAX;
                    hi_w = 0;
                }
                if self.table().owner(slab).0 != self.arena {
                    end = i;
                    break;
                }
                let _t = self.table();
                proof {
                    use_type_invariant(_t);
                }
                // The sized path defers its checks to here (design
                // 6.3). A guard slab or a slab nothing was ever
                // allocated from (`Fresh`: handed to an arena but not
                // committed, or committed and never claimed) has no
                // slot to release; its bits are clear, so the bit
                // test below would abort too, but this names the
                // cause. The descriptor line is read by `dec_live`
                // anyway.
                if self.table().is_guard_slab(slab) {
                    proof {
                        // A live slot is in no guard slab.
                        _t.lemma_guard_slab(slab as int);
                        assert(!process_regions().is_slot(self.spec_class() as int, ptrs@[i as int]@.addr));
                        assert(!live_slots(self.spec_class() as int, ptrs@));
                    }
                    guard_free(p as usize, self.class as usize, slab);
                }
                let d = self.desc(slab);
                if d.arena() != self.arena {
                    owner_mismatch(self.class as usize, slab >> self.table().lg_group, slab, self.arena, d.arena());
                }
                if d.state() == State::Fresh {
                    release_into_fresh_slab(p as usize, self.class as usize, slab);
                }
                cur_slab = slab;
            }
            let word = slot / 64;
            let bit = 1u64 << (slot % 64);
            // `slot < nslots <= slab_bits <= 64 * MAX_SLAB_WORDS`
            // (`locate` checked the slot), so `word` is in range.
            proof {
                lemma_slot_bit(slot, sb);
                let s = slot as u64;
                assert(bit == 1u64 << (s % 64)) by (bit_vector)
                    requires bit == 1u64 << (slot % 64), s == slot;
            }
            let ghost before = local@;
            let ghost (lo0, hi0) = (lo_w, hi_w);
            proof {
                _t.lemma_slot_bit(slab as int, slot as int);
                _t.lemma_slots_wf();
                assert(process_regions().slot_at(self.spec_class() as int, slab as int, slot as int) == p@.addr);
                assert(process_regions().is_slot_addr(self.spec_class() as int, p@.addr));
            }
            // The slot's token, from the batch's.
            proof {
                assert(i < ptrs@.len());
                assert(p == ptrs@[i as int]);
                assert(pa[i as int] == p@.addr as int);
                lemma_aset_step(pa, 0, i as int, ptrs@.len() as int);
            }
            let tracked tok = bag.split_singleton(p@.addr as int);
            proof {
                assert(bag@ =~= bag0@ - aset(pa, 0, i + 1));
            }
            let w = &mut local[word];
            if *w & bit != 0 {
                proof {
                    // The bit is set in the scratch words only for a slot
                    // whose token `ltoks` holds, and `tok` is `p`'s: the
                    // batch's tokens are unique, so it holds `p` once.
                    let f = slab * s.sb;
                    let k = (slot % 64) as u64;
                    let b = f + slot;
                    let x = *w;
                    assert(x == before[word as int]);
                    assert((x >> k) & 1u64 == 1u64) by (bit_vector)
                        requires bit == 1u64 << k, k < 64, x & bit != 0u64;
                    assert(has_bit(x, k));
                    if count == 0 || !(lo_w <= word <= hi_w) {
                        assert(x == 0u64);
                        assert((0u64 >> k) & 1u64 != 1u64) by (bit_vector);
                    }
                    assert(slot == 64 * word + k);
                    assert(b - f == 64 * word + k);
                    assert((b - f) / 64 == word && (b - f) % 64 == k);
                    assert(s.real(b));
                    assert(scratch_bits(s, f, before, lo_w as int, hi_w as int).contains(b));
                    assert(ltoks@.contains(s.addr(b)));
                    ltoks.disjoint_singleton(&tok);
                    assert(false);
                }
                double_free(p as usize, self.class as usize, slab, slot);
            }
            proof {
                let x = *w;
                assert(x == before[word as int]);
                let m = slab_mask_of(sb);
                assert(subset(x | bit, m)) by (bit_vector)
                    requires x & !m == 0u64, bit & !m == 0u64;
            }
            *w |= bit;
            count += 1;
            lo_w = lo_w.min(word);
            hi_w = hi_w.max(word);
            proof {
                let f = slab * s.sb;
                let k = (slot % 64) as u64;
                let b = f + slot;
                assert(b == f + 64 * word + k);
                assert forall|k2: u64| k2 < 64 implies #[trigger] has_bit(bit, k2) == (k2 == k) by {
                    assert(has_bit(1u64 << k, k2) == (k2 == k)) by (bit_vector)
                        requires k < 64, k2 < 64;
                }
                assert forall|k2: u64| k2 < 64 implies #[trigger] has_bit(local@[word as int], k2)
                    == (has_bit(before[word as int], k2) || k2 == k) by {
                    lemma_bit_ops(before[word as int], bit, k2);
                }
                assert(local@ == before.update(word as int, before[word as int] | bit));
                assert forall|v: int| 0 <= v < MAX_SLAB_WORDS implies mask_slots(v, #[trigger] local@[v], nslots) by {
                    if v == word {
                        assert forall|k2: u64| k2 < 64 && #[trigger] has_bit(local@[v], k2) implies v * 64 + k2 < nslots by {
                            if k2 != k {
                                assert(has_bit(before[v], k2));
                            }
                        }
                    }
                }
                let old_set = if count == 1 { VSet::<int>::empty() } else { scratch_bits(s, f, before, lo0 as int, hi0 as int) };
                lemma_scratch_insert(s, f, before, local@, lo0 as int, hi0 as int, lo_w as int, hi_w as int, word as int, k, count == 1);
                _t.lemma_slot_bit(slab as int, slot as int);
                assert(tok@ == s.addr(b));
                ltoks.combine_singleton(tok);
                lemma_addrs_insert(s, old_set, b);
            }
            i += 1;
        }
        if count != 0 {
            self.release_slab(cur_slab, &mut local, lo_w, hi_w, count, &mut reopen, Tracked(&mut ltoks), pubw, Tracked(ord));
        }
        self.reopen(&mut reopen, pubw, Tracked(ord));
        if end != 0 {
            self.released.fetch_add(end as u64, Ordering::Relaxed);
        }
        end
    }
}

} // verus!

impl ClassState {
    /// Tests: [`release_with`] without the slot tokens.
    ///
    /// [`release_with`]: ClassState::release_with
    #[cfg(test)]
    pub(crate) fn release(&self, ptrs: &[*mut u8]) {
        self.release_with(
            ptrs,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }
}

verus! {

impl ClassState {
    /// This arena's slab counts; `guards` is the class-wide count from the
    /// table, since guard slabs belong to no arena.
    #[cfg(test)]
    pub fn stats(&self) -> ClassStats {
        self.bin_stats(Tracked::assume_new()).0
    }

    /// The slab counts and `curregs` (slots out of this arena's bitmaps:
    /// in caches or in use, jemalloc's `curregs`) from one lock hold.
    ///
    /// `released` is read before the lock and `claimed` under it, so the
    /// difference is never negative: a slot is claimed before it is
    /// released. It can exceed the true sum of `live` by the slots
    /// released in between, so it is clamped to the capacity of the slabs
    /// that can hold live slots (those neither `Empty` nor `Purged`),
    /// which keeps `active >= allocated` exact for `stats.rs`. Every clamp
    /// is counted (`clamped`); the count cannot tell the legitimate
    /// transient from a bookkeeping bug, the reader of the bins table can.
    /// `ord` is the caller's lock-order token (ghost) for the class lock.
    pub fn bin_stats(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (ClassStats, usize))
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r.0.empty + r.0.purged <= r.0.committed <= self.spec_nslabs(),
            (r.0.committed as int) * (class_table()[self.spec_class() as int].nslots as int) <= usize::MAX,
            // The clamp: no more slots out than the live-capable slabs hold.
            (r.1 as int) <= ((r.0.committed - r.0.empty - r.0.purged) as int) * (class_table()[self.spec_class() as int].nslots as int),
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv, lemma_inv_counts};
        proof {
            use_type_invariant(self);
            lemma_slots_fit(table_at(self.table));
        }
        let released = self.released.load(Ordering::Relaxed);
        let g = self.inner.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));
        let mut s = g.stats;
        let claimed = self.claimed.load(Ordering::Relaxed);
        drop(g);
        s.guards = self.table().guards(Tracked(ord));
        proof {
            let k = class_table()[self.spec_class() as int].nslots as int;
            assert((s.committed as int) * k <= (self.spec_nslabs() as int) * k) by (nonlinear_arith)
                requires s.committed <= self.spec_nslabs(), k >= 0;
            assert(((s.committed - s.empty - s.purged) as int) * k <= (s.committed as int) * k) by (nonlinear_arith)
                requires s.empty + s.purged <= s.committed, k >= 0;
        }
        let cap = s.curslabs() * self.class().nslots();
        let out = claimed.wrapping_sub(released) as usize;
        let curregs = if out > cap {
            self.clamped.fetch_add(1, Ordering::Relaxed);
            cap
        } else {
            out
        };
        (s, curregs)
    }

    /// Times `bin_stats` clamped `curregs` (see the field).
    #[inline]
    pub fn clamped(&self) -> u64 {
        self.clamped.load(Ordering::Relaxed)
    }

    /// Slots out of this arena's bitmaps (see [`ClassState::bin_stats`]).
    #[cfg(test)]
    pub fn curregs(&self) -> usize {
        self.bin_stats(Tracked::assume_new()).1
    }

    /// Slots ever claimed from this arena's slabs of the class (wrapping).
    #[inline]
    #[cfg(test)]
    pub fn claimed(&self) -> u64 {
        self.claimed.load(Ordering::Relaxed)
    }

    /// Slots claimed by cache refills (see the field); the compaction gate
    /// in `heap/` watches its rate.
    #[inline]
    pub fn refilled(&self) -> u64 {
        self.refilled.load(Ordering::Relaxed)
    }

    /// Count a cache refill of `n` slots.
    #[inline]
    pub fn note_refill(&self, n: usize) {
        self.refilled.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// Count a compaction rebuild's claim of `n` slots.
    #[inline]
    pub fn note_compact(&self, n: usize) {
        self.compacted.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// Count a cache entering (`on`) or leaving compact mode.
    #[inline]
    pub fn note_compact_mode(&self, on: bool) {
        if on {
            &self.compact_on
        } else {
            &self.compact_off
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    /// Count a probe the refill-rate gate declined.
    #[inline]
    pub fn note_gate_refill(&self) {
        self.gate_refill.fetch_add(1, Ordering::Relaxed);
    }

    /// The compaction diagnostics: `(compacted, compact_on, compact_off,
    /// gate_refill)`, see the fields.
    pub fn compaction_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.compacted.load(Ordering::Relaxed),
            self.compact_on.load(Ordering::Relaxed),
            self.compact_off.load(Ordering::Relaxed),
            self.gate_refill.load(Ordering::Relaxed),
        )
    }

    /// `(holes, capacity)`: the free slots of the slabs that can hold live
    /// slots (neither `Empty` nor `Purged`), i.e. slots that are in no
    /// cache and no program, and those slabs' total slot count. The
    /// compaction gate in `heap/` reads this; one lock hold. `ord` is the
    /// caller's lock-order token (ghost) for the class lock.
    pub fn holes(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (usize, usize))
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
            r.0 <= r.1,
            (r.1 as int) <= (self.spec_nslabs() as int) * (class_table()[self.spec_class() as int].nslots as int),
            // Room for `holes * COMPACT_MIN_RATIO` (32) in `heap::compact`.
            r.1 <= usize::MAX / 32,
    {
        proof {
            use_type_invariant(self);
            lemma_slots_fit(table_at(self.table));
        }
        let (s, curregs) = self.bin_stats(Tracked(ord));
        proof {
            let k = class_table()[self.spec_class() as int].nslots as int;
            assert(((s.committed - s.empty - s.purged) as int) * k <= (s.committed as int) * k) by (nonlinear_arith)
                requires s.empty + s.purged <= s.committed, k >= 0;
            assert((s.committed as int) * k <= (self.spec_nslabs() as int) * k) by (nonlinear_arith)
                requires s.committed <= self.spec_nslabs(), k >= 0;
        }
        let capacity = s.curslabs() * self.class().nslots();
        (capacity.saturating_sub(curregs), capacity)
    }
}

} // verus!

impl ClassState {
    /// Tests: whether `slab` is in this arena's partial, empty and purged
    /// sets.
    #[cfg(test)]
    pub(crate) fn set_membership(&self, slab: usize) -> (bool, bool, bool) {
        let g = self.inner.lock_t();
        (
            g.partial.map.is_set(slab),
            g.empty.map.is_set(slab),
            g.purged.map.is_set(slab),
        )
    }
}

verus! {

impl ClassState {
    /// Lowest slab in the partial set, if any (for tests and stats).
    /// `ord` is the caller's lock-order token (ghost) for the class lock.
    pub fn lowest_partial(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<usize>)
        requires
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            r matches Some(s) ==> s < self.spec_nslabs(),
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        self.inner
            .lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
            .partial
            .map
            .lowest_set()
    }

    /// Whether `slab` is in the partial set (tests and the bins table,
    /// `api::print`). `ord` is the caller's lock-order token (ghost) for the
    /// class lock.
    pub fn is_partial(&self, slab: usize, Tracked(ord): Tracked<&mut LockOrder>) -> bool
        requires
            crate::sys::may_syscall(),
            slab < self.spec_nslabs(),
            old(ord).allows(rank_class(0, 0)),
        ensures
            *final(ord) == *old(ord),
    {
        broadcast use {axiom_class_lock, axiom_class_lock_inv};
        proof {
            use_type_invariant(self);
        }
        self.inner
            .lock(Tracked(ord), Tracked(&mut LockOrder::spare()))
            .partial
            .map
            .is_set(slab)
    }
}

} // verus!
