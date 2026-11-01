//! Refill and flush: the batches between a cache and its slabs. The
//! refill claims a batch into an empty cache; the flush releases a
//! cache's highest addresses grouped by slab ([`group_by_slab`]) and
//! aborts on a double free that straddles the split.

use core::ptr;

use crate::arena::{Arena, Published};
use crate::layout::CLASSES;
#[cfg(verus_keep_ghost)]
use crate::layout::{NCLASSES, class_table, process_regions, slots_in};
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::rank_class;
use crate::sys::pac;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::zero::Zeroed;

#[cfg(verus_keep_ghost)]
use super::CachePerms;
use super::shared::seed;
use super::{CacheHeader, FOREIGN, Heap, Shared, assert_refill_empty, stack_mut};
#[cfg(verus_keep_ghost)]
use super::{
    cell_ok, ents_frame, from_all, gather_all_ents, in_entries, keyed_ents, lemma_entry_signed,
    lemma_from_refl, lemma_from_slots, lemma_from_sub, lemma_from_trans, lemma_log_same,
    lemma_plain_stored, lemma_stack_fill, log_adds, move_reversed, pac_mode, plain, scatter_ents,
    slot_entry, slot_ptr, split_ok, tagged, toks_kept,
};
#[cfg(verus_keep_ghost)]
use crate::class::slot_loc;
#[cfg(verus_keep_ghost)]
use crate::class::{aset, distinct, ptr_addrs};
use vstd::map::Map;
#[cfg(verus_keep_ghost)]
use vstd::modes::tracked_swap;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, int, verus};
use vstd::raw_ptr::{ptr_mut_ref, ptr_ref};
#[cfg(verus_keep_ghost)]
use vstd::resource::set::{GhostSingleton, GhostSubset};

verus! {

impl Heap {
    /// The batch claim behind a refill or a run rebuild: `cs.claim`, or
    /// with `slot_randomize` `cs.claim_random` with this heap's xorshift
    /// (the scan starts at a random bitmap word and the batch is shuffled;
    /// design 4). The key is read from the configuration here, on
    /// the slow path, so the fast paths carry nothing for it.
    #[inline]
    #[allow(clippy::let_and_return, reason = "a proof reads the claim before it is returned")]
    pub(super) fn claim(
        &mut self,
        shared: &Shared,
        cs: &crate::class::ClassState,
        out: &mut [*mut u8],
        Tracked(toks): Tracked<&mut crate::class::SlotToks>,
        pubw: Tracked<&'static Published>,
        Tracked(ord): Tracked<&mut LockOrder>,
    ) -> (r: (crate::class::Claim, Tracked<Map<int, Zeroed>>))
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            cs.published_by(*pubw@),
            shared.wf(),
        ensures
            *final(ord) == *old(ord),
            final(self).perms == old(self).perms,
            final(self).list == old(self).list,
            final(self).keys == old(self).keys,
            final(self).arena == old(self).arena,
            final(self).home_hdrs == old(self).home_hdrs,
            final(self).others == old(self).others,
            final(self).shared == old(self).shared,
            final(self).bound == old(self).bound,
            final(self).homed == old(self).homed,
            final(self).rehome == old(self).rehome,
            final(self).heads == old(self).heads,
            r.0.n <= old(out)@.len(),
            final(out)@.len() == old(out)@.len(),
            slots_in(final(out)@, cs.spec_class() as int, 0, r.0.n as int),
            final(toks).owns(cs.spec_table(), final(out)@, 0, 0, r.0.n as int),
            crate::class::keyed(final(toks).toks, ptr_addrs(final(out)@.subrange(0, r.0.n as int)), slot_loc(cs.spec_class() as int), 0, r.0.n as int),
            cs.spec_table().spec_slots().loc == slot_loc(cs.spec_class() as int),
            crate::class::claim_zero_ok(r.0, final(out)@, r.1@, cs.spec_class() as int),
    {
        cs.slot_tokens_are_the_class_s();
        proof {
            shared.lemma_wf();
        }
        let r = if slot_randomize(shared) {
            let rng = self.rng();
            cs.claim_random_zero(&shared.meta, out, rng, Tracked(toks), pubw, Tracked(ord))
        } else {
            cs.claim_tokens(&shared.meta, out, Tracked(toks), pubw, Tracked(ord))
        };
        proof {
            toks.lemma_owns_keyed(cs.spec_table(), out@, r.0.n as int);
        }
        r
    }


    /// This heap's xorshift state for `slot_randomize`, seeded on first use
    /// from `getrandom` (mixed with the heap's address and thread id so two
    /// heaps seeded in the same instant differ even if the kernel returned
    /// the same bytes, and never zero, which xorshift cannot leave).
    /// Only `rng` changes; the address mix ([`addr_mix`], trusted) and
    /// `seed` (verified, a syscall) may return anything.
    #[cold]
    #[inline(never)]
    pub(super) fn rng(&mut self) -> (r: &mut u64)
        requires
            crate::sys::may_syscall(),
        ensures
            *final(self) == (Heap { rng: *final(r), ..*old(self) }),
    {
        if self.rng == 0 {
            let mut seed = seed();
            seed ^= addr_mix(self) ^ ((self.tid as u64) << 32);
            if seed == 0 {
                seed = 0x9E37_79B9_7F4A_7C15;
            }
            self.rng = seed;
        }
        &mut self.rng
    }

    /// Claim up to `cap / 2` slots of `class` in `arena` into an empty
    /// cache, lowest address on top so allocations stay address-ordered;
    /// the batch is the cache's run. Returns the new length.
    ///
    /// `low_water` restarts at the batch length rather than zero: the run
    /// needs `run_end <= low_water`, and a GC that then finds the batch
    /// untouched releases three quarters of it, which is what the GC is
    /// for (design 6.4).
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and is empty:
    /// callers arrive from a fast path that fell through on `len == 0`,
    /// from `ensure_cache` on a fresh header, or after re-reading `len`
    /// following a `tick`. The `assert_eq!` ([`assert_refill_empty`]) is
    /// the abort on a heap inconsistency, not the contract.
    pub(super) unsafe fn refill(&mut self, shared: &Shared, arena: &Arena, pubw: Tracked<&'static Published>, cache: *mut CacheHeader, class: usize, Tracked(ord): Tracked<&mut LockOrder>) -> (r: u32)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            crate::arena::published(*pubw@, *arena),
            class < NCLASSES,
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
            shared.wf(),
            old(self).hdr(cache).cap != 0,
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            r == final(self).hdr(cache).len,
    {
        let ghost old_perms = self.perms@;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // SAFETY: the cache is empty and has `cap` slots of storage, and the
        // header reads and writes below go through `cache`'s permission
        // (`owns(cache)` in the requires).
        unsafe {
            assert_refill_empty(&self.hdr_ref(cache).len);
            let want = (self.hdr_ref(cache).cap / 2).max(1) as usize;
            // The stack's permissions move out of the heap while the slice
            // borrows them, so that `claim` can take `self`.
            let tracked mut sl = Map::tracked_empty();
            proof {
                tracked_swap(&mut self.perms.borrow_mut().slots, &mut sl);
            }
            let out = stack_mut(self.hdr_at(cache).slots, 0, want, Tracked(&mut sl));
            // The claimed slots' tokens, by index into `out`.
            let tracked mut toks = crate::class::SlotToks::empty();
            let cs = arena.class(class);
            let (claim, Tracked(mut ztok)) = self.claim(shared, cs, out, Tracked(&mut toks), pubw, Tracked(ord));
            let ghost claimed = out@;
            proof {
                assert(toks.owns(cs.spec_table(), claimed, 0, 0, claim.n as int));
            }
            arena.class(class).note_refill(claim.n);
            out[..claim.n].reverse();
            let ghost reversed = out@;
            if shared.pac() {
                CacheHeader::sign_all(self.hdr_at(cache).slots, &mut out[..claim.n]);
            }
            proof {
                tracked_swap(&mut self.perms.borrow_mut().slots, &mut sl);
            }
            let ghost mid = self.perms@;
            let tracked perms = self.perms.borrow_mut();
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            h.len = claim.n as u32;
            h.moved = h.moved.wrapping_add(claim.n as u64);
            h.low_water = claim.n as u32;
            h.run_end = claim.n as u32;
            h.zeroed = if claim.zeroed { claim.n as u32 } else { 0 };
            proof {
                lemma_stack_fill(old_perms.hdr(cache), *h, old_perms.slots, perms.slots, 0, want as int);
                CachePerms::lemma_update(old_perms, *perms, cache);
                shared.lemma_wf();
                let n = claim.n as int;
                let slots = old_perms.hdr(cache).slots;
                assert forall|i: int| 0 <= i < n implies #[trigger] perms.holds(class as int, perms.ent(cache, i), perms.hdr(cache).slots) by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    assert(reversed[i] == claimed.subrange(0, n).reverse()[i]);
                    assert(claimed.subrange(0, n).reverse()[i] == claimed[n - 1 - i]);
                    if pac_mode() {
                        assert(out@[i]@.addr == pac::spec_sign(reversed[i]@.addr, slots@.addr));
                        lemma_plain_stored(out@[i], claimed[n - 1 - i], class as int, slots);
                    } else {
                        assert(out@ == reversed);
                    }
                }
                CachePerms::lemma_slice_frame(old_perms, old_perms.slots, perms.slots, cache, 0, want as int);
                CachePerms::lemma_tracks_stack(old_perms, *perms, cache);
                // The claim's tokens, reversed with its slots, are the new
                // zero prefix.
                let ghost mid2 = *perms;
                assert forall|i: int| 0 <= i < n implies #[trigger] plain(perms.slots[perms.ent(cache, i)].value(), perms.hdr(cache).slots) == claimed[n - 1 - i]@.addr by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    assert(perms.slots[slot_ptr(slots, i)].value() == out@[i]);
                    assert(reversed[i] == claimed.subrange(0, n).reverse()[i]);
                    assert(claimed.subrange(0, n).reverse()[i] == claimed[n - 1 - i]);
                    if pac_mode() {
                        assert(out@[i]@.addr == pac::spec_sign(reversed[i]@.addr, slots@.addr));
                        lemma_plain_stored(out@[i], claimed[n - 1 - i], class as int, slots);
                    } else {
                        assert(out@ == reversed);
                    }
                }
                if claim.zeroed {
                    // One quantifier per conjunct, each instantiating
                    // `claim_zero_ok` through its trigger term, so the
                    // precondition below does not depend on which conjunct
                    // Z3 splits on.
                    assert(class_table()[old_perms.cls[cache]].size == class_table()[class as int].size);
                    assert forall|k: int| 0 <= k < n implies #[trigger] ztok.contains_key(k) by {
                        assert(0 <= k < claim.n);
                        assert(ztok.contains_key(k));
                    }
                    assert forall|k: int| 0 <= k < n implies #[trigger] ztok[k].covers(claimed[k], class_table()[old_perms.cls[cache]].size as int) by {
                        assert(0 <= k < claim.n);
                        assert(ztok.contains_key(k));
                        assert(ztok[k].covers(claimed[k], class_table()[class as int].size as int));
                    }
                }
                CachePerms::lemma_install_claim(perms, ztok, old_perms, cache, n, claim.zeroed, claimed);
                CachePerms::lemma_same_but_tokens(mid2, *perms, old_perms);
            }
            proof {
                // The tokens go onto the stack with their slots, reversed.
                let n = claim.n as int;
                let slots = old_perms.hdr(cache).slots;
                let before = *perms;
                assert forall|i: int, j: int| 0 <= i < j < n implies slot_ptr(slots, i) != slot_ptr(slots, j) by {
                    assert(old_perms.owner[old_perms.ent(cache, i)] == (cache, i));
                    assert(old_perms.owner[old_perms.ent(cache, j)] == (cache, j));
                }
                let from0 = toks.toks;
                reveal(crate::class::SlotToks::owns);
                assert forall|i: int| 0 <= i < n implies #[trigger] from0.contains_key(n - 1 - i) by {
                    let x = claimed[n - 1 - i];
                    assert(claimed[n - 1 - i] == x);
                }
                move_reversed(&mut toks.toks, &mut perms.toks, slots, n, 0);
                CachePerms::lemma_toks_only(before, *perms, old_perms);
                assert forall|i: int| 0 <= i < n implies #[trigger] perms.tok_at(cache, i) by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    let x = claimed[n - 1 - i];
                    assert(claimed[n - 1 - i] == x);
                    assert(perms.slots[slot_ptr(slots, i)].value() == out@[i]);
                    assert(reversed[i] == claimed.subrange(0, n).reverse()[i]);
                    assert(claimed.subrange(0, n).reverse()[i] == claimed[n - 1 - i]);
                    if pac_mode() {
                        assert(out@[i]@.addr == pac::spec_sign(reversed[i]@.addr, slots@.addr));
                        lemma_plain_stored(out@[i], claimed[n - 1 - i], class as int, slots);
                    } else {
                        assert(out@ == reversed);
                    }
                }
                assert(before.toks == old_perms.toks);
                assert(toks_kept(perms.toks, old_perms.toks, slots, n));
                CachePerms::lemma_tokened_refill(old_perms, *perms, cache, n);
                CachePerms::lemma_claimed_hdr(old_perms, *perms, cache);
            }
            claim.n as u32
        }
    }

    /// Release the `n` highest-address entries of a cache to their slabs
    /// (in `arena`, which owns them) in one batch grouped by slab and keep
    /// the rest. Selecting by address sends the holes to the high slabs,
    /// which is what lets them drain (design 6.4). One `sort_unstable` of
    /// the whole slot array per flush would be 8..9% of glibc-simple and
    /// 5% of the Python json row under tiny-object churn on both VMs, the
    /// largest single term wherever challoc trailed mimalloc or tcmalloc
    /// by more than 10%, so the ordering work is the least that keeps the release grouped:
    ///
    /// - A slot array already in address order (a cache pushed in the order
    ///   a producer's refill batch was handed out, xmalloc-test's consumer
    ///   side, where even a sort's O(n) presorted path is about 1.25k
    ///   cycles per flush) is split directly: `is_sorted` is one pass that
    ///   stops at the first inversion, the released half is sorted and so
    ///   grouped, and the kept half stays sorted for the next flush. That
    ///   includes a whole flush (`n == len`, the `FOREIGN` cache, which is
    ///   where xmalloc-test's consumers release the producers' slots in
    ///   allocation order): sorted, it needs no [`group_by_slab`] pass,
    ///   whose two sweeps over a sorted batch were about 5% of the row.
    /// - Otherwise `select_nth_unstable` on the address (O(len), in place)
    ///   puts the `n` highest at the top, and [`group_by_slab`] groups them
    ///   by slab (a counting pass over the slab numbers, O(n); nothing when
    ///   they are one slab); `release_owned` needs nothing more.
    /// - The kept half is left as the selection leaves it, except on the
    ///   first two consecutive unsorted flushes of a cache and every 32nd
    ///   after that (`unsorted_flushes`), when it is sorted: a cache pushed
    ///   in order that met one unsorted flush (its first fill, a producer
    ///   moving to a lower slab) gets back onto the sorted path, where a
    ///   scrambled kept half would have kept it on the selection forever
    ///   (xmalloc-test +16% that way; a full sort re-establishes the order
    ///   at every flush), while a cache pushed in arbitrary order pays two
    ///   sorts of half the array and then one in 32 (the unconditional
    ///   kept sort cost glibc-simple and Python their whole gain). The run
    ///   is dropped in every case and rebuilt by the scheduled pop (design
    ///   6.4); the zero prefix is given up.
    ///
    /// Candidates rejected, all in the results file: a selection followed
    /// by a sort of the released half (the selection scrambles it, so that
    /// is a full sort: xmalloc +51%); an in-place bucket pass over the
    /// whole array by address (about 10 cycles per entry, a full sort's
    /// cost on random input and ten times its cost on sorted input: xmalloc
    /// +49%); the selection alone (xmalloc +21%, no self-healing).
    ///
    /// Double frees. `release_owned` aborts on a duplicate inside the
    /// released batch (a slab's slots are contiguous after the bucket pass
    /// and land on one scratch bitmap, so the second copy hits a set bit; a
    /// duplicate in two groups of one slab hits a clear bit in the class
    /// bitmap). A pair kept together stays cached and consistent (both
    /// copies in the cache, the bit set) until a flush releases it. The
    /// pair that straddles the split would have one copy released, its bit
    /// cleared, and the other still cached to be handed out over a slot a
    /// refill can claim again: after the split every kept entry is at most
    /// `kept_max = slots[rest_len - 1]` and every released entry at least
    /// that, so a straddling pair is exactly a released entry equal to
    /// `kept_max`, and the lowest released address (found by the bucket
    /// pass's first sweep, or `slots[rest_len]` on the sorted path) is
    /// compared with it. One compare, the same strength as the sorted
    /// form's neighbour compare at the split; a pass over the kept half was
    /// measured at 4.9% / 7.3% of xmalloc-test and is not done.
    ///
    /// # Safety
    /// `cache` is this heap's real cache for `(arena, class)` and `n <= len`.
    // Hot free path: the body's source is as written, except that its two
    // sorts go through the `#[inline(always)]` wrappers `sort_ptrs_by_key`
    // and `sort_ptrs`, and its three header writes pass the header's own
    // permission (`Tracked(..)`). Each erased `Tracked(..)` argument is a
    // closure instance, and the three put `flush_arena` past rustc's
    // MIR-inliner cycle-check budget, so the inliner lays this path out
    // differently from a body with no such arguments.
    //
    // The proof's cost is the kept/released split's sequence facts. The
    // cache's re-establishment after the release (the stack's `stack_ok`,
    // the kept entries' class and slot tokens, the other headers' frame)
    // is `lemma_flush_kept`, and the signed entries `lemma_flush_signed`,
    // each in its own query. `spinoff_prover` gives flush its own solver
    // so unrelated items stay out, and the rlimit leaves about four times
    // what flush needs. When it runs out, move more of the proof into
    // such lemmas rather than raise the limit.
    #[verifier::spinoff_prover]
    #[verifier::rlimit(100)]
    #[allow(clippy::manual_is_multiple_of)]
    pub(super) unsafe fn flush(&mut self, shared: &Shared, arena: u16, cache: *mut CacheHeader, class: usize, n: usize, Tracked(ord): Tracked<&mut LockOrder>)
        requires
            // The covered free path's capability ([`crate::sys::valid_aborts`]),
            // without the integrity one: every cached entry holds its slot
            // as a push stores it ([`slot_entry`]), signed with the stack base
            // under `pac_mode`, so `pac::auth` takes no integrity abort on
            // the entries the flush authenticates.
            crate::sys::may_abort() || crate::sys::valid_aborts(),
            crate::sys::may_syscall(),
            old(ord).allows(rank_class(0, 0)),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
            n <= old(self).hdr(cache).len,
        ensures
            *final(ord) == *old(ord),
            final(self).keeps(*old(self)),
            final(self).hdr(cache).len == old(self).hdr(cache).len - n,
            final(self).same_shape(*old(self)),
            final(self).tcache_on() == old(self).tcache_on(),
            forall|p: *mut CacheHeader| old(self).owns(p) && p != cache ==> #[trigger] final(self).held(p) == old(self).held(p),
            forall|p: *mut CacheHeader| old(self).owns(p) && p != cache ==> #[trigger] final(self).hdr(p) == old(self).hdr(p),
            // The accounting: the slots `cache` no longer holds are
            // the ones appended to the released log, and it holds no new one.
            final(self).held(cache).to_set() <= old(self).held(cache).to_set(),
            log_adds(final(self).released(), old(self).released(), old(self).held(cache).to_set() - final(self).held(cache).to_set()),
    {
        // The flush writes the permissions alone; `lemma_keeps_perms`
        // carries the `others` list across, so its conjuncts stay hidden.
        hide(Heap::list_ok);
        hide(crate::class::live_slots);
        // The pairwise facts are proved and used in the lemmas below.
        hide(crate::class::distinct);
        broadcast use crate::class::lemma_aset;
        if n == 0 {
            proof {
                Heap::lemma_keeps_refl(*self);
                lemma_log_same(self.released());
                assert(self.held(cache).to_set() - self.held(cache).to_set() =~= Set::empty());
            }
            return;
        }
        let ghost old_perms = self.perms@;
        let ghost old_h = old_perms.hdr(cache);
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // The accounting, out of the block below: the cached
        // addresses, the kept ones and the released ones.
        let ghost mut g_old = Seq::<int>::empty();
        let ghost mut g_kept = Seq::<int>::empty();
        let ghost mut g_rel = Seq::<int>::empty();
        // SAFETY: `n <= len <= cap` entries exist, and the header reads and
        // writes below go through `cache`'s permission (`owns(cache)` in
        // the requires).
        unsafe {
            let pac = shared.pac();
            let tracked perms = self.perms.borrow_mut();
            let slots = ptr_ref(cache, Tracked(perms.hdrs.tracked_borrow(cache))).slots;
            // The cached slots' tokens, gathered by address for the
            // selection and sorts to permute; the kept ones go back under
            // their new entries and the released ones to their bitmap words.
            let ghost len = old_h.len as int;
            let ghost aold = Seq::new(old_h.len as nat, |i: int| plain(old_perms.slots[slot_ptr(slots, i)].value(), slots) as int);
            proof {
                assert(slots == old_h.slots);
                assert(forall|m: int| 0 <= m < aold.len() ==> #[trigger] aold[m] == plain(old_perms.slots[slot_ptr(old_perms.hdr(cache).slots, m)].value(), old_perms.hdr(cache).slots) as int) by {}
            }
            let tracked mut bag = lemma_flush_gather(&mut perms.toks, old_perms, cache, aold);
            let ghost t1 = perms.toks;
            // With signed entries the selection, the sortedness test and
            // the kept sort order by the address bits alone (`!pac::mask()`
            // masks the PAC field, one `and` per compare); only the
            // released half is authenticated, once, before the release,
            // and the kept half stays as it is. Authenticating the whole
            // stack and re-signing the kept half was measured first:
            // xmalloc-test -15.8% on the C4A (the consumer's flushes are
            // the sorted path, 1.25k cycles each, and the two passes
            // doubled them).
            // The keyed forms only when signed: `sort_unstable_by_key` and
            // friends take a comparator closure and lose the primitive
            // pointer sort's specialisations, which costs every flush even
            // when the key is the identity. The mask itself only when
            // signed: `pac::mask` is an `xpacd`, UNDEFINED on a CPU without
            // the extension (`SIGILL` on Neoverse N1, the Altra), and no
            // PAC instruction may run unless the probe said yes.
            let addr_mask = if pac { !pac::mask() } else { usize::MAX };
            let key = |e: &*mut u8| (*e as usize) & addr_mask;
            let all = stack_mut(
                slots,
                0,
                ptr_ref(cache, Tracked(perms.hdrs.tracked_borrow(cache))).len as usize,
                Tracked(&mut perms.slots),
            );
            let ghost a0 = all@;
            proof {
                lemma_from_refl(a0);
            }
            let rest_len = all.len() - n;
            let kept_max;
            // Where `kept_max` sits among the kept entries (the straddle check).
            let ghost mut km_at: int = 0;
            let sorted = if pac { all.is_sorted_by_key(key) } else { all.is_sorted() };
            if sorted {
                // Already in address order: the split is direct, the
                // released half is sorted and so grouped, and the kept half
                // stays sorted for the next flush. A whole flush keeps
                // nothing, so nothing can straddle the split.
                kept_max = if rest_len > 0 { *all.get_unchecked(rest_len - 1) } else { ptr::null_mut() };
                ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache))).unsorted_flushes = 0;
                proof {
                    if rest_len > 0 {
                        km_at = rest_len - 1;
                    }
                }
            } else {
                kept_max = if rest_len > 0 {
                    // `all[..rest_len - 1] <= all[rest_len - 1] <= all[rest_len..]`.
                    if pac {
                        all.select_nth_unstable_by_key(rest_len - 1, key);
                    } else {
                        all.select_nth_unstable(rest_len - 1);
                    }
                    proof {
                        assert(all@.to_multiset() == a0.to_multiset());
                    }
                    *all.get_unchecked(rest_len - 1)
                } else {
                    ptr::null_mut()
                };
                let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
                let k = h.unsorted_flushes;
                h.unsorted_flushes = k.wrapping_add(1);
                proof {
                    assert(all@.to_multiset() == a0.to_multiset());
                }
                let ghost a1 = all@;
                proof {
                    km_at = rest_len - 1;
                }
                if rest_len > 1 && (k < 2 || k % 32 == 0) {
                    if pac {
                        crate::vspec::slice::sort_ptrs_by_key(all.get_unchecked_mut(..rest_len), key);
                    } else {
                        crate::vspec::slice::sort_ptrs(all.get_unchecked_mut(..rest_len));
                    }
                    proof {
                        lemma_from_sub(a1, rest_len as int, all@);
                        lemma_from_trans(a0, a1, all@, rest_len as int);
                        let s1 = a1.subrange(0, rest_len as int);
                        let s2 = a1.subrange(rest_len as int, a1.len() as int);
                        assert(a1 =~= s1 + s2);
                        assert(all@ =~= all@.subrange(0, rest_len as int) + s2);
                        vstd::seq_lib::lemma_multiset_commutative(s1, s2);
                        vstd::seq_lib::lemma_multiset_commutative(all@.subrange(0, rest_len as int), s2);
                        // The kept maximum stays among the kept entries.
                        if rest_len > 0 {
                            km_at = lemma_kept_index(a1, all@, rest_len as int, rest_len - 1);
                        }
                    }
                }
            }
            let ghost a2 = all@;
            proof {
                // Every kept entry was an entry before: the selection and
                // the sort only move them.
                if rest_len > 0 {
                    assert(from_all(a2, a0, rest_len as int));
                    // `kept_max` is a kept entry (the straddle check below).
                    assert(0 <= km_at < rest_len && a2[km_at] == kept_max);
                }
            }
            // The kept entries' tokens back under their entries.
            let ghost akept = addrs_of(a2, rest_len as int, slots);
            proof {
                assert(a2.to_multiset() == a0.to_multiset());
                assert(addrs_of(a0, len, slots) =~= aold) by {
                    assert forall|i: int| 0 <= i < len implies #[trigger] addrs_of(a0, len, slots)[i] == aold[i] by {
                        assert(old_perms.slots[slot_ptr(slots, i)].is_init());
                    }
                }
                lemma_flush_perm(slots, aold, a0, a2, rest_len as int);
                lemma_flush_scatter(&mut bag, &mut perms.toks, old_perms, cache, akept);
            }
            let ghost t2 = perms.toks;
            let high = all.get_unchecked_mut(rest_len..);
            let ghost h0 = high@;
            proof {
                // The released entries and `kept_max` are signed, so
                // authenticating them takes no integrity abort.
                if pac {
                    shared.lemma_wf();
                    lemma_flush_signed(old_perms, cache, a0, a2, h0, rest_len as int, km_at);
                }
            }
            if pac {
                // The release needs plain pointers. A corrupted entry
                // faults here (FEAT_FPAC) or in `release_owned`.
                CacheHeader::auth_all(slots, high);
            }
            let ghost h1 = high@;
            let low = if sorted { *high.get_unchecked(0) } else { group_by_slab(shared, class, high) };
            proof {
                shared.lemma_wf();
                assert(high@.to_multiset() == h1.to_multiset());
                // The released entries' addresses are those of their tokens.
                assert(ptr_addrs(h1) =~= addrs_of(a2.subrange(rest_len as int, len), len - rest_len, slots)) by {
                    assert forall|j: int| 0 <= j < h1.len() implies #[trigger] ptr_addrs(h1)[j] == plain(a2[rest_len + j], slots) as int by {
                        assert(h0[j] == a2[rest_len + j]);
                    }
                }
                assert(aset(ptr_addrs(h1), 0, h1.len() as int) <= aset(aold, 0, len) - aset(akept, 0, rest_len as int));
                crate::class::lemma_perm_addrs(h1, high@);
                // `kept_max` is `a2[km_at]`, signed (`lemma_flush_signed`).
                if pac && rest_len > 0 {
                    assert(a2[km_at] == kept_max);
                }
            }
            let kept_max_plain = if pac && rest_len > 0 { addr_to_ptr(pac::auth(kept_max as usize, slots as usize)) } else { kept_max };
            proof {
                // No straddling pair, and a batch of slots of `class`
                // (`lemma_released`): the compare below fails for every
                // caller.
                let km = if rest_len > 0 { plain(kept_max, slots) as int } else { 0 };
                assert(rest_len > 0 ==> aset(akept, 0, rest_len as int).contains(km)) by {
                    if rest_len > 0 {
                        assert(akept[km_at] == plain(kept_max, slots) as int);
                    }
                }
                lemma_released(old_perms, cache, aold, akept, h1, high@, low, km);
                assert(kept_max_plain@.addr == km) by {}
                assert(old_perms.cls[cache] == class);
                assert(process_regions().is_slot(class as int, high@[0]@.addr)) by {
                    reveal(crate::class::live_slots);
                }
            }
            if low == kept_max_plain {
                straddling_double_free(low, class);
            }
            // The `FOREIGN` cache's batch starts wherever its first slab
            // belongs; `release_routed` follows the owner words from there.
            let owner = if arena == FOREIGN { shared.arena_of_slot(class, *high.get_unchecked(0)) } else { arena };
            shared.release_routed(owner, class, high, Tracked(&mut bag), Tracked(ord));
            proof {
                // Every cached slot is kept or released.
                let hr = ptr_addrs(high@);
                lemma_flush_split(aold, akept, ptr_addrs(h1), hr);
                g_old = aold;
                g_kept = akept;
                g_rel = hr;
                perms.released = perms.released + tagged(class as int, hr);
            }
            let h = ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache)));
            h.len = rest_len as u32;
            h.moved = h.moved.wrapping_sub(n as u64);
            h.zeroed = 0;
            h.run_end = 0;
            h.low_water = h.low_water.min(rest_len as u32);
            proof {
                lemma_stack_fill(old_h, *h, old_perms.slots, perms.slots, 0, old_h.len as int);
                assert(perms.hdrs =~= old_perms.hdrs.insert(cache, perms.hdrs[cache]));
                CachePerms::lemma_update(old_perms, *perms, cache);
                // The cache's invariants, re-established out of this query.
                shared.lemma_wf();
                assert forall|i: int| 0 <= i < rest_len implies #[trigger] perms.slots[slot_ptr(slots, i)].value() == a2[i] by {
                    assert(all@[i] == a2[i]);
                }
                lemma_flush_kept(old_perms, *perms, cache, a0, a2, rest_len as int, akept, t1);
            }
        }
        proof {
            CachePerms::lemma_claimed_hdr(old_perms, self.perms@, cache);
            Heap::lemma_keeps_perms(*old(self), *self);
            Heap::lemma_flush_accounting(*old(self), *self, cache, g_old, g_kept, g_rel);
        }
    }
}

/// The flush's cache re-establishment, out of `flush`'s query: after the
/// release, `cache`'s stack keeps its first `k` entries, the kept half
/// `a2[..k]` of a permutation of its old entries `a0`, with their tokens
/// scattered back by address (`akept`, from `t1`, the tokens after the
/// gather), and its header says `len == k` with no zero prefix. Then every
/// invariant still holds, the other headers' entries are as they were,
/// and the kept entries stand for the kept addresses.
#[verifier::spinoff_prover]
proof fn lemma_flush_kept(
    o: CachePerms,
    np: CachePerms,
    cache: *mut CacheHeader,
    a0: Seq<*mut u8>,
    a2: Seq<*mut u8>,
    k: int,
    akept: Seq<int>,
    t1: Map<*mut *mut u8, GhostSingleton<int>>,
)
    requires
        crate::heap::regions_ok(),
        o.wf(),
        o.tracks(),
        o.zero_ok(),
        o.tokened(),
        o.hdrs.contains_key(cache),
        cell_ok(cache, o.hdrs[cache], o.slots),
        np.hdrs == o.hdrs.insert(cache, np.hdrs[cache]),
        np.cls == o.cls,
        np.owner == o.owner,
        np.zero == o.zero,
        np.hdr(cache).slots == o.hdr(cache).slots,
        np.hdr(cache).cap == o.hdr(cache).cap,
        np.hdr(cache).len == k,
        np.hdr(cache).zeroed == 0,
        0 <= k <= o.hdr(cache).len,
        a0.len() == o.hdr(cache).len,
        a2.len() == a0.len(),
        forall|i: int|
            0 <= i < a0.len() && o.slots[slot_ptr(o.hdr(cache).slots, i)].is_init()
                ==> a0[i] == #[trigger] o.slots[slot_ptr(o.hdr(cache).slots, i)].value(),
        forall|i: int| 0 <= i < k ==> #[trigger] np.slots[slot_ptr(o.hdr(cache).slots, i)].value() == a2[i],
        super::slots_grow(o.slots, np.slots),
        forall|q: *mut *mut u8|
            o.slots.contains_key(q) && (forall|i: int| 0 <= i < o.hdr(cache).len ==> q != #[trigger] slot_ptr(o.hdr(cache).slots, i))
                ==> #[trigger] np.slots[q] == o.slots[q],
        k > 0 ==> from_all(a2, a0, k),
        ents_frame(t1, o.toks, o.hdr(cache).slots, 0, o.hdr(cache).len as int),
        keyed_ents(np.toks, o.hdr(cache).slots, akept, slot_loc(o.cls[cache]), 0, k),
        ents_frame(np.toks, t1, o.hdr(cache).slots, 0, k),
        akept == addrs_of(a2, k, o.hdr(cache).slots),
    ensures
        np.tracks(),
        np.follows(o),
        np.zero_ok(),
        np.tokened(),
        forall|p2: *mut CacheHeader, j: int|
            o.hdrs.contains_key(p2) && p2 != cache && 0 <= j < o.hdr(p2).cap
                ==> np.slots[#[trigger] o.ent(p2, j)] == o.slots[o.ent(p2, j)],
        forall|i: int| 0 <= i < k ==> plain(np.slots[#[trigger] np.ent(cache, i)].value(), np.hdr(cache).slots) as int == akept[i],
{
    let slots = o.hdr(cache).slots;
    let len = o.hdr(cache).len as int;
    // The kept entries are old entries, so they still hold slots of the
    // header's class.
    let c = o.cls[cache];
    if k > 0 {
        assert forall|j: int| 0 <= j < a0.len() implies slot_entry(c, #[trigger] a0[j], slots) by {
            assert(o.ent(cache, j) == slot_ptr(slots, j));
            assert(o.slots[slot_ptr(slots, j)].is_init());
        }
        lemma_from_slots(a0, a2, c, k, slots);
    }
    assert forall|i: int| 0 <= i < k implies #[trigger] np.holds(c, np.ent(cache, i), np.hdr(cache).slots) by {
        assert(np.ent(cache, i) == slot_ptr(slots, i));
    }
    CachePerms::lemma_slice_frame(o, o.slots, np.slots, cache, 0, len);
    CachePerms::lemma_tracks_stack(o, np, cache);
    CachePerms::lemma_zero_update_ent(o, np, cache);
    // The kept entries hold their tokens; no other header's entry is one
    // of this stack's.
    assert forall|p2: *mut CacheHeader, j: int|
        o.hdrs.contains_key(p2) && p2 != cache && 0 <= j < o.hdr(p2).cap implies {
            let q = #[trigger] o.ent(p2, j);
            &&& np.slots[q] == o.slots[q]
            &&& (o.toks.contains_key(q) ==> np.toks.contains_key(q) && np.toks[q] == o.toks[q])
        } by {
        let q = o.ent(p2, j);
        if in_entries(slots, q, 0, len) {
            let kk = choose|kk: int| 0 <= kk < len && q == slot_ptr(slots, kk);
            o.lemma_ent_apart(cache, kk, p2, j);
        }
        if in_entries(slots, q, 0, k) {
            let kk = choose|kk: int| 0 <= kk < k && q == slot_ptr(slots, kk);
            assert(in_entries(slots, q, 0, len));
        }
        assert(t1.contains_key(q) == o.toks.contains_key(q));
        assert(np.toks.contains_key(q) == t1.contains_key(q));
        if o.toks.contains_key(q) {
            assert(t1[q] == o.toks[q]);
            assert(np.toks[q] == t1[q]);
        }
    }
    assert forall|i: int| 0 <= i < np.hdr(cache).len implies #[trigger] np.tok_at(cache, i) by {
        assert(np.ent(cache, i) == slot_ptr(slots, i));
    }
    CachePerms::lemma_tok_update(o, np, cache);
    assert forall|i: int| 0 <= i < k implies plain(np.slots[#[trigger] np.ent(cache, i)].value(), np.hdr(cache).slots) as int == akept[i] by {
        assert(np.ent(cache, i) == slot_ptr(slots, i));
    }
}

/// With [`pac_mode`], the entries `flush` authenticates are signed with
/// the stack base: the released half `hi` of `a2` (from `n` on) and, when
/// something is kept, the kept maximum `a2[km]`. `a2` is a permutation of
/// the entries `a0` of `cache`'s stack (the selection and sorts), and each
/// of those holds its slot as a push stored it ([`slot_entry`]), so
/// authenticating them takes no integrity abort. Out of `flush`'s query,
/// which only sees the facts about `hi` and `a2[km]`.
proof fn lemma_flush_signed(
    perms: CachePerms,
    cache: *mut CacheHeader,
    a0: Seq<*mut u8>,
    a2: Seq<*mut u8>,
    hi: Seq<*mut u8>,
    n: int,
    km: int,
)
    requires
        crate::heap::regions_ok(),
        pac_mode(),
        perms.tracks(),
        perms.hdrs.contains_key(cache),
        cell_ok(cache, perms.hdrs[cache], perms.slots),
        a0.len() == perms.hdr(cache).len,
        a2.to_multiset() == a0.to_multiset(),
        forall|i: int|
            0 <= i < a0.len() && perms.slots[slot_ptr(perms.hdr(cache).slots, i)].is_init()
                ==> a0[i] == #[trigger] perms.slots[slot_ptr(perms.hdr(cache).slots, i)].value(),
        0 <= n <= a2.len(),
        hi == a2.subrange(n, a2.len() as int),
        n > 0 ==> 0 <= km < n,
    ensures
        forall|i: int| 0 <= i < hi.len() ==> pac::signed((#[trigger] hi[i])@.addr, perms.hdr(cache).slots@.addr),
        n > 0 ==> pac::signed(a2[km]@.addr, perms.hdr(cache).slots@.addr),
{
    let slots = perms.hdr(cache).slots;
    let c = perms.cls[cache];
    let len = a0.len() as int;
    a0.to_multiset_ensures();
    a2.to_multiset_ensures();
    assert(a0 =~= a0.subrange(0, len) + a2.subrange(len, len));
    assert forall|k: int| 0 <= k < a2.len() implies pac::signed((#[trigger] a2[k])@.addr, slots@.addr) by {
        let i = lemma_kept_index(a2, a0, len, k);
        assert(perms.ent(cache, i) == slot_ptr(slots, i));
        assert(perms.slots[slot_ptr(slots, i)].is_init());
        assert(perms.holds(c, perms.ent(cache, i), slots));
        lemma_entry_signed(c, a0[i], slots);
    }
    assert forall|i: int| 0 <= i < hi.len() implies pac::signed((#[trigger] hi[i])@.addr, slots@.addr) by {
        assert(hi[i] == a2[n + i]);
    }
}

/// `a1[j]`, one of `a1`'s first `n` entries, is still among the first `n`
/// of `a`, a permutation of `a1` that left the rest in place (the flush's
/// sort of its kept entries; with `n` the whole length, any permutation of
/// `a1`): its index there.
proof fn lemma_kept_index(a1: Seq<*mut u8>, a: Seq<*mut u8>, n: int, j: int) -> (i: int)
    requires
        0 <= j < n <= a1.len(),
        a.to_multiset() == a1.to_multiset(),
        a.len() == a1.len(),
        a =~= a.subrange(0, n) + a1.subrange(n, a1.len() as int),
    ensures
        0 <= i < n,
        a[i] == a1[j],
{
    let s1 = a1.subrange(0, n);
    let s2 = a1.subrange(n, a1.len() as int);
    let s3 = a.subrange(0, n);
    let x = a1[j];
    assert(a1 =~= s1 + s2);
    vstd::seq_lib::lemma_multiset_commutative(s1, s2);
    vstd::seq_lib::lemma_multiset_commutative(s3, s2);
    s1.to_multiset_ensures();
    s3.to_multiset_ensures();
    assert(s1[j] == x);
    assert(s1.contains(x));
    assert(s3.to_multiset().add(s2.to_multiset()).count(x) == s1.to_multiset().add(s2.to_multiset()).count(x));
    assert(s3.contains(x));
    let i = choose|i: int| 0 <= i < n && s3[i] == x;
    assert(s3[i] == a[i]);
    i
}

/// The flush's two facts about its released half, out of `flush`'s
/// query: every released entry `hi[k]` is a cached slot of the header's
/// class (what `release_routed` needs to release the batch for a valid
/// program), and the lowest released address `low` is no kept entry's
/// (`km`: the kept maximum's address, or 0 when nothing is kept). So the
/// straddle check fails for every caller: the cached entries stand for
/// distinct slots, since their tokens are distinct.
proof fn lemma_released(perms: CachePerms, cache: *mut CacheHeader, aold: Seq<int>, akept: Seq<int>, h1: Seq<*mut u8>, hi: Seq<*mut u8>, low: *mut u8, km: int)
    requires
        crate::heap::regions_ok(),
        perms.tracks(),
        perms.hdrs.contains_key(cache),
        perms.hdr(cache).len <= perms.hdr(cache).cap,
        aold.len() == perms.hdr(cache).len,
        forall|m: int| 0 <= m < aold.len() ==> #[trigger] aold[m] == plain(perms.slots[slot_ptr(perms.hdr(cache).slots, m)].value(), perms.hdr(cache).slots) as int,
        aset(ptr_addrs(h1), 0, h1.len() as int) <= aset(aold, 0, aold.len() as int) - aset(akept, 0, akept.len() as int),
        aset(ptr_addrs(hi), 0, hi.len() as int) <= aset(ptr_addrs(h1), 0, h1.len() as int),
        h1.contains(low),
        akept.len() > 0 ==> aset(akept, 0, akept.len() as int).contains(km),
        akept.len() == 0 ==> km == 0,
    ensures
        crate::class::live_slots(perms.cls[cache], hi),
        low@.addr != km,
{
    broadcast use crate::class::lemma_aset;
    let len = aold.len() as int;
    let c = perms.cls[cache];
    assert forall|m: int| 0 <= m < len implies #[trigger] process_regions().is_slot(c, aold[m] as usize) by {
        assert(perms.ent(cache, m) == slot_ptr(perms.hdr(cache).slots, m));
        assert(perms.holds(c, perms.ent(cache, m), perms.hdr(cache).slots));
    }
    assert forall|k: int| 0 <= k < hi.len() implies #[trigger] process_regions().is_slot(c, hi[k]@.addr) by {
        assert(ptr_addrs(hi)[k] == hi[k]@.addr);
        assert(aset(ptr_addrs(hi), 0, hi.len() as int).contains(hi[k]@.addr as int));
        assert(aset(aold, 0, len).contains(hi[k]@.addr as int));
        let m = choose|m: int| 0 <= m < len && aold[m] == hi[k]@.addr as int;
        assert(process_regions().is_slot(c, aold[m] as usize));
    }
    let j = choose|j: int| 0 <= j < h1.len() && h1[j] == low;
    assert(ptr_addrs(h1)[j] == low@.addr);
    assert(aset(ptr_addrs(h1), 0, h1.len() as int).contains(low@.addr as int));
    assert(aset(aold, 0, len).contains(low@.addr as int));
    assert(!aset(akept, 0, akept.len() as int).contains(low@.addr as int));
    let m = choose|m: int| 0 <= m < len && aold[m] == low@.addr as int;
    assert(aold[m] as usize == low@.addr);
    assert(process_regions().is_slot(c, aold[m] as usize));
    process_regions().lemma_block_nonzero(c, low@.addr, 0, 0);
}

/// The addresses `a[..n]`, entries of the stack at `slots`, stand for.
spec fn addrs_of(a: Seq<*mut u8>, n: int, slots: *mut *mut u8) -> Seq<int> {
    Seq::new(n as nat, |i: int| plain(a[i], slots) as int)
}

/// The flush's gather, out of `flush`'s query (its pairwise facts would
/// stay in it): the tokens of `cache`'s cached slots, taken from their
/// entries into one bag by address, for the selection and the sorts to
/// permute the entries freely.
#[verifier::spinoff_prover]
proof fn lemma_flush_gather(tracked toks: &mut Map<*mut *mut u8, GhostSingleton<int>>, perms: CachePerms, cache: *mut CacheHeader, aold: Seq<int>) -> (tracked bag: GhostSubset<int>)
    requires
        *old(toks) == perms.toks,
        perms.wf(),
        perms.tracks(),
        perms.tokened(),
        perms.hdrs.contains_key(cache),
        cell_ok(cache, perms.hdrs[cache], perms.slots),
        0 < aold.len() == perms.hdr(cache).len,
        forall|m: int| 0 <= m < aold.len() ==> #[trigger] aold[m] == plain(perms.slots[slot_ptr(perms.hdr(cache).slots, m)].value(), perms.hdr(cache).slots) as int,
    ensures
        bag.id() == slot_loc(perms.cls[cache]),
        bag@ == aset(aold, 0, aold.len() as int),
        distinct(aold, 0, aold.len() as int),
        ents_frame(*final(toks), *old(toks), perms.hdr(cache).slots, 0, aold.len() as int),
{
    let slots = perms.hdr(cache).slots;
    let len = aold.len() as int;
    let c = perms.cls[cache];
    assert forall|i: int| 0 <= i < len implies {
        let q = #[trigger] slot_ptr(slots, i);
        &&& toks.contains_key(q)
        &&& toks[q].id() == slot_loc(c)
        &&& toks[q]@ == aold[i]
    } by {
        perms.lemma_tok_get(cache, i);
        assert(perms.ent(cache, i) == slot_ptr(slots, i));
    }
    assert forall|i: int, j: int| 0 <= i < j < len implies slot_ptr(slots, i) != slot_ptr(slots, j) by {
        perms.lemma_ent_apart(cache, i, cache, j);
    }
    gather_all_ents(toks, slots, aold, slot_loc(c), len)
}

/// The flush's two halves, out of `flush`'s query: `a2`, a permutation of
/// the cached entries `a0` (whose addresses, `aold`, are distinct), keeps
/// distinct cached addresses in `a2[..n]`, and its released entries
/// `a2[n..]` stand for distinct cached addresses that are not kept.
#[verifier::spinoff_prover]
proof fn lemma_flush_perm(slots: *mut *mut u8, aold: Seq<int>, a0: Seq<*mut u8>, a2: Seq<*mut u8>, n: int)
    requires
        0 <= n <= a0.len(),
        a2.to_multiset() == a0.to_multiset(),
        addrs_of(a0, a0.len() as int, slots) == aold,
        distinct(aold, 0, aold.len() as int),
    ensures
        a2.len() == a0.len(),
        distinct(addrs_of(a2, n, slots), 0, n),
        aset(addrs_of(a2, n, slots), 0, n) <= aset(aold, 0, aold.len() as int),
        distinct(addrs_of(a2.subrange(n, a2.len() as int), a2.len() - n, slots), 0, a2.len() - n),
        aset(addrs_of(a2.subrange(n, a2.len() as int), a2.len() - n, slots), 0, a2.len() - n)
            <= aset(aold, 0, aold.len() as int) - aset(addrs_of(a2, n, slots), 0, n),
{
    broadcast use crate::class::lemma_aset;
    let f = |e: *mut u8| plain(e, slots) as int;
    let len = a0.len() as int;
    let akept = addrs_of(a2, n, slots);
    let ar = addrs_of(a2.subrange(n, len), len - n, slots);
    assert(crate::class::injective_on(a0, f)) by {
        reveal(crate::class::injective_on);
        assert forall|i: int, j: int| 0 <= i < j < len implies f(a0[i]) != f(a0[j]) by {
            assert(aold[i] == addrs_of(a0, len, slots)[i]);
            assert(aold[j] == addrs_of(a0, len, slots)[j]);
        }
    }
    crate::class::lemma_perm_distinct(a0, a2, f);
    reveal(crate::class::injective_on);
    assert forall|i: int, j: int| 0 <= i < j < n implies akept[i] != akept[j] by {
        assert(f(a2[i]) != f(a2[j]));
    }
    assert forall|i: int, j: int| 0 <= i < j < len - n implies ar[i] != ar[j] by {
        assert(f(a2[n + i]) != f(a2[n + j]));
    }
    assert forall|v: int| aset(akept, 0, n).contains(v) implies aset(aold, 0, len).contains(v) by {
        let i = choose|i: int| 0 <= i < n && akept[i] == v;
        assert(a0.contains(a2[i]));
        let k = choose|k: int| 0 <= k < len && a0[k] == a2[i];
        assert(aold[k] == addrs_of(a0, len, slots)[k]);
    }
    assert forall|v: int| #![trigger aset(ar, 0, len - n).contains(v)] aset(ar, 0, len - n).contains(v) implies aset(aold, 0, len).contains(v) && !aset(akept, 0, n).contains(v) by {
        let j = choose|j: int| 0 <= j < len - n && ar[j] == v;
        let i = n + j;
        assert(ar[j] == f(a2[i]));
        assert(a0.contains(a2[i]));
        let k = choose|k: int| 0 <= k < len && a0[k] == a2[i];
        assert(aold[k] == addrs_of(a0, len, slots)[k]);
        if aset(akept, 0, n).contains(v) {
            let m = choose|m: int| 0 <= m < n && akept[m] == v;
            assert(f(a2[m]) != f(a2[i]));
        }
    }
}

/// The flush's scatter, out of `flush`'s query: the kept addresses'
/// tokens from the bag back under the first `akept.len()` entries of
/// `cache`'s stack.
#[verifier::spinoff_prover]
proof fn lemma_flush_scatter(tracked bag: &mut GhostSubset<int>, tracked toks: &mut Map<*mut *mut u8, GhostSingleton<int>>, perms: CachePerms, cache: *mut CacheHeader, akept: Seq<int>)
    requires
        perms.wf(),
        perms.tracks(),
        perms.hdrs.contains_key(cache),
        cell_ok(cache, perms.hdrs[cache], perms.slots),
        akept.len() <= perms.hdr(cache).len,
        distinct(akept, 0, akept.len() as int),
        aset(akept, 0, akept.len() as int) <= old(bag)@,
    ensures
        final(bag).id() == old(bag).id(),
        final(bag)@ == old(bag)@ - aset(akept, 0, akept.len() as int),
        keyed_ents(*final(toks), perms.hdr(cache).slots, akept, old(bag).id(), 0, akept.len() as int),
        ents_frame(*final(toks), *old(toks), perms.hdr(cache).slots, 0, akept.len() as int),
{
    let slots = perms.hdr(cache).slots;
    let n = akept.len() as int;
    assert forall|i: int, j: int| 0 <= i < j < n implies slot_ptr(slots, i) != slot_ptr(slots, j) by {
        perms.lemma_ent_apart(cache, i, cache, j);
    }
    scatter_ents(bag, toks, slots, akept, 0, n);
}

/// The split of one flush: the addresses the cache held (`aold`)
/// are the ones it keeps (`akept`) and the ones released (`hr`), and none
/// is both. By counting: the kept ones are cached ones, the released
/// ones are among `h1a`'s (the batch before `group_by_slab` reordered it),
/// which are cached and not kept, all three are distinct, and the two parts
/// add up to the whole.
proof fn lemma_flush_split(aold: Seq<int>, akept: Seq<int>, h1a: Seq<int>, hr: Seq<int>)
    requires
        distinct(aold, 0, aold.len() as int),
        distinct(akept, 0, akept.len() as int),
        distinct(hr, 0, hr.len() as int),
        aold.len() == akept.len() + hr.len(),
        aset(akept, 0, akept.len() as int) <= aset(aold, 0, aold.len() as int),
        aset(h1a, 0, h1a.len() as int) <= aset(aold, 0, aold.len() as int) - aset(akept, 0, akept.len() as int),
        aset(hr, 0, hr.len() as int) <= aset(h1a, 0, h1a.len() as int),
    ensures
        split_ok(aold, akept, hr),
{
    let o = aset(aold, 0, aold.len() as int);
    let k = aset(akept, 0, akept.len() as int);
    let r = aset(hr, 0, hr.len() as int);
    lemma_aset_len(aold, aold.len() as int);
    lemma_aset_len(akept, akept.len() as int);
    lemma_aset_len(hr, hr.len() as int);
    assert(k.disjoint(r));
    assert(k + r <= o);
    vstd::set_lib::lemma_set_disjoint_lens(k, r);
    vstd::set_lib::lemma_subset_equality(k + r, o);
    reveal(split_ok);
}

/// `aset(a, 0, i)` of distinct addresses has `i` of them.
proof fn lemma_aset_len(a: Seq<int>, i: int)
    requires
        0 <= i <= a.len(),
        distinct(a, 0, a.len() as int),
    ensures
        aset(a, 0, i).len() == i,
    decreases i,
{
    if i == 0 {
        crate::class::lemma_aset_empty(a, 0, 0);
    } else {
        lemma_aset_len(a, i - 1);
        crate::class::lemma_aset_step(a, 0, i - 1, a.len() as int);
    }
}

} // verus!
verus! {

/// Buckets of [`group_by_slab`]'s counting pass: a batch whose slab
/// numbers span fewer than this many slabs is grouped in one in-place
/// pass; a batch spread wider is sorted instead.
const GROUP_BUCKETS: usize = 256;

} // verus!

verus! {

/// Group a flush's released batch by slab, in place, and return its lowest
/// address. `release_owned` needs a slab's slots contiguous (one owner
/// check, one descriptor update and one bitmap pass per slab) and nothing
/// else, so the pass is a counting sort on the slab number, `(p - base) >>
/// lg_slab` (design 3's geometry): one sweep finds the lowest and highest
/// slab number (and the batch's minimum address); a batch from one slab is
/// done; one spanning fewer than [`GROUP_BUCKETS`] slabs is permuted by
/// [`flag_sort`] on `slab - lowest`; anything wider, a cache holding slots
/// of more than 256 slabs' span, falls back to `sort_unstable` on the
/// address. Every path is O(n) but the last, a full sort of half the
/// entries.
///
/// Verus checks the sweep and the dispatch: every key is in `[kmin,
/// kmax]`, so [`flag_sort`]'s keys are below `span <= GROUP_BUCKETS`,
/// and only the batch's order changes (the multiset of entries is kept,
/// so their slot tokens follow). The result is an entry of the batch;
/// that it is the lowest is not part of the contract (the flush's
/// straddle check needs it only for its strength, not for any proof).
/// The fallback `sort_unstable` is the core
/// library's, through `vspec::slice::sort_ptrs` (a permutation).
/// The body hands
/// [`flag_sort`] batches of up to the cache's capacity, at most
/// `tcache_nslots_small_max`'s bound `MAX_NSLOTS` (65536), the length `flag_sort` is verified for since
/// its cursors are `u32`.
pub(super) fn group_by_slab(shared: &Shared, class: usize, high: &mut [*mut u8]) -> (low: *mut u8)
    requires
        class < NCLASSES,
        old(high)@.len() > 0,
        old(high)@.len() <= crate::config::MAX_NSLOTS,
    ensures
        final(high)@.len() == old(high)@.len(),
        final(high)@.to_multiset() == old(high)@.to_multiset(),
        old(high)@.contains(low),
{
    let ghost h0 = high@;
    let base = shared.regions.class_base[class];
    let lg = CLASSES.table()[class].lg_slab as u32;
    proof {
        assert(class_table()[class as int].wf());
    }
    let key = |p: *mut u8| -> (k: usize)
        requires
            1 <= lg < 64,
        ensures
            k == slab_key(p, base, lg),
            k <= 0x7fff_ffff_ffff_ffff,
    {
        let a = (p as usize).wrapping_sub(base);
        proof {
            assert(a >> lg <= 0x7fff_ffff_ffff_ffff) by (bit_vector)
                requires
                    1 <= lg < 64,
            ;
        }
        a >> lg
    };
    let mut low = high[0];
    let (mut kmin, mut kmax) = (usize::MAX, 0usize);
    for &p in it: high.iter()
        invariant
            high@ == h0,
            h0.len() > 0,
            1 <= lg < 64,
            forall|p: *mut u8| #[trigger] key.requires((p,)),
            forall|p: *mut u8, k: usize| #[trigger] key.ensures((p,), k) ==> k == slab_key(p, base, lg),
            forall|p: *mut u8, k: usize| #[trigger] key.ensures((p,), k) ==> k <= 0x7fff_ffff_ffff_ffff,
            h0.contains(low),
            kmax <= 0x7fff_ffff_ffff_ffff,
            forall|j: int| 0 <= j < it.index() ==> kmin <= #[trigger] slab_key(h0[j], base, lg) <= kmax,
            it.index() > 0 ==> kmin <= kmax,
    {
        proof {
            assert(*it.seq()[it.index() as int] == h0[it.index() as int]);
        }
        let k = key(p);
        kmin = kmin.min(k);
        kmax = kmax.max(k);
        if p < low {
            low = p;
        }
    }
    if kmin == kmax {
        return low;
    }
    let span = kmax - kmin + 1;
    if span <= GROUP_BUCKETS {
        flag_sort(
            high,
            span,
            |p: *mut u8| -> (k: usize)
                requires
                    1 <= lg < 64,
                    kmin <= slab_key(p, base, lg) <= kmax,
                ensures
                    k == slab_key(p, base, lg) - kmin,
                { key(p) - kmin },
        );
    } else {
        crate::vspec::slice::sort_ptrs(high);
    }
    low
}

/// The slab number [`group_by_slab`] keys `p` by: its offset from the
/// class's `base`, wrapping, shifted down by the slab size's log.
spec fn slab_key(p: *mut u8, base: usize, lg: u32) -> usize {
    vstd::wrapping::usize_specs::wrapping_sub(p@.addr, base) >> lg
}

} // verus!

verus! {

/// The bucket `key` gives `p` (`key` is deterministic, see [`flag_sort`]).
pub(super) open spec fn kf<F: Fn(*mut u8) -> usize>(key: F, p: *mut u8) -> int {
    (choose|k: usize| key.ensures((p,), k)) as int
}

/// Entries of `s` in bucket `c`.
pub(super) open spec fn count<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, c: int) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        count(key, s.drop_last(), c) + if kf(key, s.last()) == c { 1nat } else { 0nat }
    }
}

/// Entries of `s` in buckets below `n`.
spec fn count_below<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int) -> nat
    decreases n,
{
    if n <= 0 {
        0
    } else {
        count_below(key, s, n - 1) + count(key, s, n - 1)
    }
}

/// Entries of `s` with a bucket below `n`, counted directly.
spec fn count_lt<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        count_lt(key, s.drop_last(), n) + if 0 <= kf(key, s.last()) < n { 1nat } else { 0nat }
    }
}

/// Total slack of the buckets below `n`: slots between `next` and `end`.
spec fn slack(next: Seq<u32>, end: Seq<u32>, n: int) -> int
    decreases n,
{
    if n <= 0 {
        0
    } else {
        slack(next, end, n - 1) + end[n - 1] - next[n - 1]
    }
}

proof fn lemma_count_le<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, c: int)
    ensures
        count(key, s, c) <= s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_count_le(key, s.drop_last(), c);
    }
}

proof fn lemma_count_push<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, p: *mut u8, c: int)
    ensures
        count(key, s.push(p), c) == count(key, s, c) + if kf(key, p) == c { 1nat } else { 0nat },
{
    assert(s.push(p).drop_last() =~= s);
}

proof fn lemma_count_lt_step<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int)
    requires
        n >= 0,
    ensures
        count_lt(key, s, n + 1) == count_lt(key, s, n) + count(key, s, n),
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_count_lt_step(key, s.drop_last(), n);
    }
}

proof fn lemma_count_below<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int)
    requires
        n >= 0,
    ensures
        count_below(key, s, n) == count_lt(key, s, n),
    decreases n,
{
    if n == 0 {
        lemma_count_lt_zero(key, s);
    } else {
        lemma_count_below(key, s, n - 1);
        lemma_count_lt_step(key, s, n - 1);
    }
}

proof fn lemma_count_lt_zero<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>)
    ensures
        count_lt(key, s, 0) == 0,
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_count_lt_zero(key, s.drop_last());
    }
}

proof fn lemma_count_lt_le<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int)
    ensures
        count_lt(key, s, n) <= s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_count_lt_le(key, s.drop_last(), n);
    }
}

/// Writing `p` at `j` moves one entry from the old value's bucket to `p`'s.
proof fn lemma_count_update<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, j: int, p: *mut u8, c: int)
    requires
        0 <= j < s.len(),
    ensures
        count(key, s.update(j, p), c) + (if kf(key, s[j]) == c { 1int } else { 0int })
            == count(key, s, c) + (if kf(key, p) == c { 1int } else { 0int }),
    decreases s.len(),
{
    if j < s.len() - 1 {
        lemma_count_update(key, s.drop_last(), j, p, c);
        assert(s.update(j, p).drop_last() =~= s.drop_last().update(j, p));
    } else {
        assert(s.update(j, p).drop_last() =~= s.drop_last());
    }
}

/// A range of `s` whose entries are all in bucket `c`, and one more entry
/// of bucket `c` outside it: `c` has more entries than the range is long.
proof fn lemma_count_ge<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, a: int, b: int, i: int, c: int)
    requires
        0 <= a <= b <= s.len(),
        forall|j: int| a <= j < b ==> kf(key, #[trigger] s[j]) == c,
        0 <= i < s.len(),
        !(a <= i < b),
        kf(key, s[i]) == c,
    ensures
        count(key, s, c) >= b - a + 1,
    decreases s.len(),
{
    let n = s.len() - 1;
    let t = s.drop_last();
    if n == i {
        if b <= n {
            lemma_count_range_all(key, t, a, b, c);
        }
    } else if n >= b {
        lemma_count_ge(key, t, a, b, i, c);
    } else if a <= n {
        assert(kf(key, s[n]) == c);
        lemma_count_ge(key, t, a, b - 1, i, c);
    } else {
        lemma_count_ge(key, t, 0, 0, i, c);
    }
}

/// A range whose entries are all in bucket `c` gives `c` at least that many.
proof fn lemma_count_range_all<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, a: int, b: int, c: int)
    requires
        0 <= a <= b <= s.len(),
        forall|j: int| a <= j < b ==> kf(key, #[trigger] s[j]) == c,
    ensures
        count(key, s, c) >= b - a,
    decreases s.len(),
{
    if s.len() > 0 {
        let n = s.len() - 1;
        if n >= b {
            lemma_count_range_all(key, s.drop_last(), a, b, c);
        } else if a < b {
            lemma_count_range_all(key, s.drop_last(), a, b - 1, c);
            assert(kf(key, s[n]) == c);
        }
    }
}

proof fn lemma_slack_step(next: Seq<u32>, end: Seq<u32>, n: int, k: int, x: u32)
    requires
        0 <= k < n <= next.len(),
        n <= end.len(),
        x == next[k] + 1,
    ensures
        slack(next.update(k, x), end, n) == slack(next, end, n) - 1,
    decreases n,
{
    if k < n - 1 {
        lemma_slack_step(next, end, n - 1, k, x);
    } else {
        lemma_slack_same(next, next.update(k, x), end, n - 1);
    }
}

proof fn lemma_slack_same(a: Seq<u32>, b: Seq<u32>, end: Seq<u32>, n: int)
    requires
        n <= a.len(),
        n <= b.len(),
        forall|c: int| 0 <= c < n ==> a[c] == b[c],
    ensures
        slack(a, end, n) == slack(b, end, n),
    decreases n,
{
    if n > 0 {
        lemma_slack_same(a, b, end, n - 1);
    }
}

proof fn lemma_slack_nonneg(next: Seq<u32>, end: Seq<u32>, n: int)
    requires
        n <= next.len(),
        n <= end.len(),
        forall|c: int| 0 <= c < n ==> next[c] <= end[c],
    ensures
        slack(next, end, n) >= 0,
    decreases n,
{
    if n > 0 {
        lemma_slack_nonneg(next, end, n - 1);
    }
}

proof fn lemma_count_below_mono<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, a: int, b: int)
    requires
        0 <= a <= b,
    ensures
        count_below(key, s, a) <= count_below(key, s, b),
    decreases b - a,
{
    if a < b {
        lemma_count_below_mono(key, s, a, b - 1);
    }
}

proof fn lemma_count_below_le<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int)
    requires
        n >= 0,
    ensures
        count_below(key, s, n) <= s.len(),
{
    lemma_count_below(key, s, n);
    lemma_count_lt_le(key, s, n);
}

/// Bucket `c` owns `[count_below(c), count_below(c + 1))`.
spec fn lo<F: Fn(*mut u8) -> usize>(key: F, v0: Seq<*mut u8>, c: int) -> int {
    count_below(key, v0, c) as int
}

/// Entry `j` of `s` sits below bucket `c`'s cursor and is in bucket `c`.
spec fn placed<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, c: int, j: int) -> bool {
    lo(key, v0, c) <= j < next[c] ==> kf(key, s[j]) == c
}

/// The placement state of [`flag_sort`]: bucket `c < n` owns
/// `[lo(c), lo(c + 1))`, `end[c]` is its end and `next[c]` its cursor, the
/// entries below every cursor are in their bucket, and `s` holds as many
/// entries of each bucket as `v0`.
spec fn inv<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, end: Seq<u32>, n: int) -> bool {
    &&& next.len() == GROUP_BUCKETS
    &&& end.len() == GROUP_BUCKETS
    &&& 0 <= n <= GROUP_BUCKETS
    &&& s.len() == v0.len()
    &&& forall|c: int| 0 <= c < n ==> #[trigger] end[c] == lo(key, v0, c + 1)
    &&& forall|c: int| 0 <= c < n ==> end[c] <= s.len()
    &&& forall|c: int| 0 <= c < n ==> lo(key, v0, c) <= #[trigger] next[c] <= end[c]
    &&& forall|c: int, j: int| 0 <= c < n && 0 <= j < s.len() ==> #[trigger] placed(key, s, v0, next, c, j)
    &&& forall|c: int| 0 <= c < n ==> #[trigger] count(key, s, c) == count(key, v0, c)
}

/// Buckets `c != d` own disjoint ranges.
proof fn lemma_disjoint<F: Fn(*mut u8) -> usize>(key: F, v0: Seq<*mut u8>, c: int, d: int, x: int, y: int)
    requires
        0 <= c,
        0 <= d,
        c != d,
        lo(key, v0, c) <= x < lo(key, v0, c + 1),
        lo(key, v0, d) <= y < lo(key, v0, d + 1),
    ensures
        x != y,
{
    if c < d {
        lemma_count_below_mono(key, v0, c + 1, d);
    } else {
        lemma_count_below_mono(key, v0, d + 1, c);
    }
}

/// The entry in hand, of bucket `kb != b`, has a free slot in its bucket.
proof fn lemma_room<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, end: Seq<u32>, n: int, b: int, i: int, p: *mut u8)
    requires
        inv(key, v.update(i, p), v0, next, end, n),
        0 <= b < n,
        i == next[b],
        i < end[b],
        0 <= kf(key, p) < n,
        kf(key, p) != b,
    ensures
        next[kf(key, p)] < end[kf(key, p)],
        next[kf(key, p)] != i,
{
    let s = v.update(i, p);
    let kb = kf(key, p);
    assert(next[kb] <= end[kb]);
    assert(lo(key, v0, b) <= i);
    assert forall|j: int| lo(key, v0, kb) <= j < next[kb] implies kf(key, #[trigger] s[j]) == kb by {
        assert(placed(key, s, v0, next, kb, j));
    }
    if lo(key, v0, kb) <= i < next[kb] {
        lemma_disjoint(key, v0, kb, b, i, i);
    }
    lemma_count_ge(key, s, lo(key, v0, kb), next[kb] as int, i, kb);
    assert(count(key, s, kb) == count(key, v0, kb));
    assert(count_below(key, v0, kb + 1) == count_below(key, v0, kb) + count(key, v0, kb));
    assert(next[kb] < end[kb]);
    lemma_disjoint(key, v0, kb, b, next[kb] as int, i);
}

/// Swapping the entry in hand with the one at bucket `kb`'s cursor keeps
/// the state.
proof fn lemma_step<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, end: Seq<u32>, n: int, b: int, i: int, p: *mut u8, j: int, x: u32)
    requires
        inv(key, v.update(i, p), v0, next, end, n),
        0 <= b < n,
        i == next[b],
        i < end[b],
        0 <= kf(key, p) < n,
        kf(key, p) != b,
        j == next[kf(key, p)],
        j < end[kf(key, p)],
        j != i,
        x == next[kf(key, p)] + 1,
    ensures
        inv(key, v.update(j, p).update(i, v[j]), v0, next.update(kf(key, p), x), end, n),
{
    let kb = kf(key, p);
    let s = v.update(i, p);
    let t = v.update(j, p).update(i, v[j]);
    let nx = next.update(kb, x);
    assert(t =~= s.update(j, s[i]).update(i, s[j]));
    assert forall|c: int| 0 <= c < n implies #[trigger] count(key, t, c) == count(key, v0, c) by {
        lemma_count_update(key, s, j, s[i], c);
        lemma_count_update(key, s.update(j, s[i]), i, s[j], c);
        assert(count(key, s, c) == count(key, v0, c));
    }
    assert forall|c: int, jj: int| 0 <= c < n && 0 <= jj < t.len() implies #[trigger] placed(key, t, v0, nx, c, jj) by {
        if lo(key, v0, c) <= jj < nx[c] {
            if c == kb && jj == j {
            } else {
                assert(jj < next[c]);
                assert(placed(key, s, v0, next, c, jj));
                if jj == i {
                    if c == b {
                    } else {
                        assert(next[c] <= end[c]);
                        lemma_disjoint(key, v0, c, b, jj, i);
                    }
                }
                if jj == j {
                    assert(next[kb] <= end[kb]);
                    assert(c != kb);
                    lemma_disjoint(key, v0, c, kb, jj, j);
                }
                assert(t[jj] == s[jj]);
            }
        }
    }
    assert forall|c: int| 0 <= c < n implies lo(key, v0, c) <= #[trigger] nx[c] <= end[c] by {
        assert(lo(key, v0, c) <= next[c] <= end[c]);
    }
    assert forall|c: int| 0 <= c < n implies #[trigger] end[c] == lo(key, v0, c + 1) by {
        assert(end[c] == lo(key, v0, c + 1));
    }
}

/// Dropping the entry in hand, of bucket `b`, into the hole at `b`'s
/// cursor and advancing it keeps the state.
proof fn lemma_fill<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, end: Seq<u32>, n: int, b: int, i: int, p: *mut u8, x: u32)
    requires
        inv(key, v.update(i, p), v0, next, end, n),
        0 <= b < n,
        i == next[b],
        i < end[b],
        kf(key, p) == b,
        x == next[b] + 1,
    ensures
        inv(key, v.update(i, p), v0, next.update(b, x), end, n),
{
    let s = v.update(i, p);
    let nx = next.update(b, x);
    assert forall|c: int, jj: int| 0 <= c < n && 0 <= jj < s.len() implies #[trigger] placed(key, s, v0, nx, c, jj) by {
        if lo(key, v0, c) <= jj < nx[c] && !(c == b && jj == i) {
            assert(placed(key, s, v0, next, c, jj));
        }
    }
    assert forall|c: int| 0 <= c < n implies lo(key, v0, c) <= #[trigger] nx[c] <= end[c] by {
        assert(lo(key, v0, c) <= next[c] <= end[c]);
    }
    assert forall|c: int| 0 <= c < n implies #[trigger] end[c] == lo(key, v0, c + 1) by {
        assert(end[c] == lo(key, v0, c + 1));
    }
    assert forall|c: int| 0 <= c < n implies #[trigger] count(key, s, c) == count(key, v0, c) by {
        assert(count(key, s, c) == count(key, v0, c));
    }
}

/// Every entry of `s` in a bucket below `n`: all of them are counted.
proof fn lemma_count_lt_all<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, n: int)
    requires
        forall|j: int| 0 <= j < s.len() ==> 0 <= #[trigger] kf(key, s[j]) < n,
    ensures
        count_lt(key, s, n) == s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        let t = s.drop_last();
        assert forall|j: int| 0 <= j < t.len() implies 0 <= #[trigger] kf(key, t[j]) < n by {
            assert(t[j] == s[j]);
        }
        lemma_count_lt_all(key, t, n);
        assert(0 <= kf(key, s[s.len() - 1]) < n);
    }
}

/// The bucket whose range holds position `j < lo(n)`.
proof fn lemma_bucket_of<F: Fn(*mut u8) -> usize>(key: F, v0: Seq<*mut u8>, n: int, j: int) -> (c: int)
    requires
        0 <= n,
        0 <= j < lo(key, v0, n),
    ensures
        0 <= c < n,
        lo(key, v0, c) <= j < lo(key, v0, c + 1),
    decreases n,
{
    if j < lo(key, v0, n - 1) {
        lemma_bucket_of(key, v0, n - 1, j)
    } else {
        n - 1
    }
}

/// With every cursor at its end, the entries are grouped by bucket in
/// ascending order.
proof fn lemma_sorted<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u32>, end: Seq<u32>, n: int)
    requires
        inv(key, s, v0, next, end, n),
        forall|c: int| 0 <= c < n ==> next[c] == end[c],
        forall|j: int| 0 <= j < v0.len() ==> 0 <= #[trigger] kf(key, v0[j]) < n,
    ensures
        forall|i: int, j: int| 0 <= i <= j < s.len() ==> #[trigger] kf(key, s[i]) <= #[trigger] kf(key, s[j]),
{
    lemma_count_lt_all(key, v0, n);
    lemma_count_below(key, v0, n);
    assert forall|i: int, j: int| 0 <= i <= j < s.len() implies #[trigger] kf(key, s[i]) <= #[trigger] kf(key, s[j]) by {
        let ci = lemma_bucket_of(key, v0, n, i);
        let cj = lemma_bucket_of(key, v0, n, j);
        assert(placed(key, s, v0, next, ci, i));
        assert(placed(key, s, v0, next, cj, j));
        if ci > cj {
            lemma_count_below_mono(key, v0, cj + 1, ci);
        }
    }
}

/// An entry of a permutation of `v0` is one of `v0`'s.
proof fn lemma_in_perm(s: Seq<*mut u8>, v0: Seq<*mut u8>, i: int)
    requires
        s.to_multiset() == v0.to_multiset(),
        0 <= i < s.len(),
    ensures
        v0.contains(s[i]),
{
    broadcast use vstd::seq_lib::group_to_multiset_ensures;
    assert(s.contains(s[i]));
    assert(s.to_multiset().count(s[i]) > 0);
    assert(v0.to_multiset().count(s[i]) > 0);
}

/// Swapping two entries keeps the multiset.
proof fn lemma_swap_multiset(s: Seq<*mut u8>, i: int, j: int)
    requires
        0 <= i < s.len(),
        0 <= j < s.len(),
    ensures
        s.update(j, s[i]).update(i, s[j]).to_multiset() == s.to_multiset(),
{
    broadcast use {vstd::seq_lib::group_to_multiset_ensures, vstd::multiset::group_multiset_axioms};
    let s1 = s.update(j, s[i]);
    if i == j {
        assert(s1.update(i, s[j]) =~= s);
    } else {
        assert(s1[i] == s[i]);
        assert(s.to_multiset().count(s[j]) > 0);
        assert(s1.update(i, s[j]).to_multiset() =~= s.to_multiset());
    }
}

/// American flag sort: permute `v` in place so that entries with equal
/// `key` (below `nb`, at most [`GROUP_BUCKETS`]) are contiguous, buckets
/// in ascending key order. One counting sweep, a prefix sum, and one
/// cycle-following sweep that moves every entry once: O(len + nb), no
/// scratch beyond the bucket cursors. The cursors are `u32`: a whole
/// flush of the largest cache passes
/// [`MAX_NSLOTS`](crate::config::MAX_NSLOTS) (65536) entries, one more
/// than `u16` can count.
///
/// Verus checks it for a `key` that, on `v`'s entries, may be called, is
/// deterministic and is below `nb` (it may be partial elsewhere: the
/// group pass's `slab - lowest` is defined only on its batch): every cursor
/// stays inside its bucket (no index leaves `v`, no `u32` counter
/// overflows), every loop ends, and `v` ends up a permutation of what it
/// held (the same multiset), in ascending bucket order.
pub(super) fn flag_sort(v: &mut [*mut u8], nb: usize, key: impl Fn(*mut u8) -> usize)
    requires
        nb <= 256,
        old(v).len() <= crate::config::MAX_NSLOTS,
        forall|p: *mut u8| #![trigger key.requires((p,))] old(v)@.contains(p) ==> key.requires((p,)),
        forall|p: *mut u8, k: usize| #![trigger key.ensures((p,), k)] old(v)@.contains(p) && key.ensures((p,), k) ==> k < nb,
        forall|p: *mut u8, k1: usize, k2: usize| #![trigger key.ensures((p,), k1), key.ensures((p,), k2)] old(v)@.contains(p) && key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
    ensures
        final(v)@.len() == old(v)@.len(),
        forall|c: int| 0 <= c < nb ==> count(key, final(v)@, c) == count(key, old(v)@, c),
        final(v)@.to_multiset() == old(v)@.to_multiset(),
        forall|i: int, j: int| 0 <= i <= j < final(v)@.len() ==> #[trigger] kf(key, final(v)@[i]) <= #[trigger] kf(key, final(v)@[j]),
{
    debug_assert!(nb <= GROUP_BUCKETS && v.len() <= crate::config::MAX_NSLOTS as usize);
    let ghost v0 = v@;
    let mut next = [0u32; GROUP_BUCKETS];
    let mut end = [0u32; GROUP_BUCKETS];
    for &p in it: v.iter()
        invariant
            v@ == v0,
            nb <= GROUP_BUCKETS,
            v0.len() <= crate::config::MAX_NSLOTS,
            forall|p: *mut u8| #![trigger key.requires((p,))] v0.contains(p) ==> key.requires((p,)),
            forall|p: *mut u8, k: usize| #![trigger key.ensures((p,), k)] v0.contains(p) && key.ensures((p,), k) ==> k < nb,
            forall|p: *mut u8, k1: usize, k2: usize| #![trigger key.ensures((p,), k1), key.ensures((p,), k2)] v0.contains(p) && key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
            it.index() <= v0.len(),
            forall|c: int| 0 <= c < GROUP_BUCKETS ==> #[trigger] end[c] == count(key, v0.take(it.index() as int), c),
            forall|j: int| 0 <= j < it.index() ==> 0 <= #[trigger] kf(key, v0[j]) < nb,
    {
        proof {
            assert(*it.seq()[it.index() as int] == v0[it.index() as int]);
            assert(v0.take(it.index() + 1) =~= v0.take(it.index() as int).push(p));
            assert forall|c: int| 0 <= c < GROUP_BUCKETS implies count(key, v0.take(it.index() + 1), c)
                == count(key, v0.take(it.index() as int), c) + if kf(key, p) == c { 1nat } else { 0nat } by {
                lemma_count_push(key, v0.take(it.index() as int), p, c);
            }
            assert forall|c: int| 0 <= c < GROUP_BUCKETS implies count(key, v0.take(it.index() as int), c) <= it.index() by {
                lemma_count_le(key, v0.take(it.index() as int), c);
            }
        }
        proof {
            assert(v0.contains(p));
        }
        end[key(p)] += 1;
    }
    proof {
        assert(v0.take(v0.len() as int) =~= v0);
        lemma_count_below_le(key, v0, nb as int);
    }
    let mut acc = 0u32;
    for b in 0..nb
        invariant
            v@ == v0,
            nb <= GROUP_BUCKETS,
            v0.len() <= crate::config::MAX_NSLOTS,
            count_below(key, v0, nb as int) <= v0.len(),
            acc == count_below(key, v0, b as int),
            forall|c: int| #![trigger next@[c]] #![trigger end@[c]] 0 <= c < b ==> next@[c] == count_below(key, v0, c) && end@[c] == count_below(key, v0, c + 1),
            forall|c: int| b <= c < GROUP_BUCKETS ==> #[trigger] end@[c] == count(key, v0, c),
            forall|j: int| 0 <= j < v0.len() ==> 0 <= #[trigger] kf(key, v0[j]) < nb,
    {
        proof {
            lemma_count_below_mono(key, v0, b + 1, nb as int);
        }
        next[b] = acc;
        acc += end[b];
        end[b] = acc;
    }
    proof {
        assert forall|c: int| 0 <= c < nb implies end@[c] <= v0.len() by {
            lemma_count_below_mono(key, v0, c + 1, nb as int);
        }
        assert forall|c: int, j: int| 0 <= c < nb && 0 <= j < v0.len() implies #[trigger] placed(key, v@, v0, next@, c, j) by {
        }
    }
    for b in 0..nb
        invariant
            v@.len() == v0.len(),
            nb <= GROUP_BUCKETS,
            v0.len() <= crate::config::MAX_NSLOTS,
            forall|p: *mut u8| #![trigger key.requires((p,))] v0.contains(p) ==> key.requires((p,)),
            forall|p: *mut u8, k: usize| #![trigger key.ensures((p,), k)] v0.contains(p) && key.ensures((p,), k) ==> k < nb,
            forall|p: *mut u8, k1: usize, k2: usize| #![trigger key.ensures((p,), k1), key.ensures((p,), k2)] v0.contains(p) && key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
            count_below(key, v0, nb as int) <= v0.len(),
            v@.to_multiset() == v0.to_multiset(),
            inv(key, v@, v0, next@, end@, nb as int),
            forall|j: int| 0 <= j < v0.len() ==> 0 <= #[trigger] kf(key, v0[j]) < nb,
            forall|c: int| 0 <= c < b ==> next@[c] == end@[c],
    {
        while next[b] < end[b]
            invariant
                b < nb,
                v@.len() == v0.len(),
                nb <= GROUP_BUCKETS,
                v0.len() <= crate::config::MAX_NSLOTS,
                forall|p: *mut u8| #![trigger key.requires((p,))] v0.contains(p) ==> key.requires((p,)),
                forall|p: *mut u8, k: usize| #![trigger key.ensures((p,), k)] v0.contains(p) && key.ensures((p,), k) ==> k < nb,
                forall|p: *mut u8, k1: usize, k2: usize| #![trigger key.ensures((p,), k1), key.ensures((p,), k2)] v0.contains(p) && key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
                count_below(key, v0, nb as int) <= v0.len(),
                v@.to_multiset() == v0.to_multiset(),
                inv(key, v@, v0, next@, end@, nb as int),
                forall|j: int| 0 <= j < v0.len() ==> 0 <= #[trigger] kf(key, v0[j]) < nb,
                forall|c: int| 0 <= c < b ==> next@[c] == end@[c],
            decreases end@[b as int] - next@[b as int],
        {
            let i = next[b] as usize;
            proof {
                lemma_count_below_mono(key, v0, b + 1, nb as int);
            }
            let mut p = v[i];
            proof {
                lemma_in_perm(v@, v0, i as int);
            }
            let mut kb = key(p);
            proof {
                assert(v@.update(i as int, p) =~= v@);
            }
            while kb != b
                invariant
                    b < nb,
                    v@.len() == v0.len(),
                    nb <= GROUP_BUCKETS,
                    v0.len() <= crate::config::MAX_NSLOTS,
                    forall|p: *mut u8| #![trigger key.requires((p,))] v0.contains(p) ==> key.requires((p,)),
                    forall|p: *mut u8, k: usize| #![trigger key.ensures((p,), k)] v0.contains(p) && key.ensures((p,), k) ==> k < nb,
                    forall|p: *mut u8, k1: usize, k2: usize| #![trigger key.ensures((p,), k1), key.ensures((p,), k2)] v0.contains(p) && key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
                    count_below(key, v0, nb as int) <= v0.len(),
                    i == next@[b as int],
                    i < end@[b as int],
                    kb < nb,
                    kb == kf(key, p),
                    inv(key, v@.update(i as int, p), v0, next@, end@, nb as int),
                    v@.update(i as int, p).to_multiset() == v0.to_multiset(),
                    forall|j: int| 0 <= j < v0.len() ==> 0 <= #[trigger] kf(key, v0[j]) < nb,
                    forall|c: int| 0 <= c < b ==> next@[c] == end@[c],
                decreases slack(next@, end@, nb as int),
            {
                proof {
                    lemma_room(key, v@, v0, next@, end@, nb as int, b as int, i as int, p);
                    assert(kf(key, p) > b);
                }
                let ghost v1 = v@;
                let ghost p1 = p;
                let ghost n1 = next@;
                let j = next[kb] as usize;
                next[kb] += 1;
                core::mem::swap(&mut p, &mut v[j]);
                proof {
                    assert(v1.update(i as int, p1)[j as int] == p);
                    lemma_in_perm(v1.update(i as int, p1), v0, j as int);
                }
                kb = key(p);
                proof {
                    lemma_step(key, v1, v0, n1, end@, nb as int, b as int, i as int, p1, j as int, next@[kf(key, p1)]);
                    assert(next@ =~= n1.update(kf(key, p1), next@[kf(key, p1)]));
                    assert(v@ =~= v1.update(j as int, p1));
                    let ghost s = v1.update(i as int, p1);
                    lemma_swap_multiset(s, i as int, j as int);
                    assert(v@.update(i as int, p) =~= s.update(j as int, s[i as int]).update(i as int, s[j as int]));
                    lemma_slack_step(n1, end@, nb as int, kf(key, p1), next@[kf(key, p1)]);
                    lemma_slack_nonneg(n1, end@, nb as int);
                    assert(slack(next@, end@, nb as int) == slack(n1, end@, nb as int) - 1);
                    assert(slack(n1, end@, nb as int) >= 0);
                    lemma_slack_nonneg(next@, end@, nb as int);
                }
            }
            let ghost v1 = v@;
            let ghost n1 = next@;
            v[i] = p;
            next[b] += 1;
            proof {
                lemma_fill(key, v1, v0, n1, end@, nb as int, b as int, i as int, p, next@[b as int]);
                assert(next@ =~= n1.update(b as int, next@[b as int]));
                assert(v@ =~= v1.update(i as int, p));
            }
        }
    }
    proof {
        lemma_sorted(key, v@, v0, next@, end@, nb as int);
    }
}

} // verus!

verus! {

/// Two copies of one slot in a cache at flush time, one on each side of
/// the split (`Heap::flush`): the flush would have released one and kept
/// the other.
#[cold]
#[inline(never)]
pub(super) fn straddling_double_free(p: *mut u8, class: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!("double free of {:#x} (class {class}): both copies cached, one at the flush boundary", p as usize))
}

} // verus!

verus! {

/// `Config::slot_randomize` (design 4), read on the slow path.
#[inline(always)]
fn slot_randomize(shared: &Shared) -> bool {
    shared.config.slot_randomize
}

/// `rng`'s mix of the heap's address into its seed: the address of `h`,
/// rotated left by 17 bits.
///
/// Trusted: Verus rejects the reference-to-integer cast; claims nothing
/// about the value.
#[inline(always)]
#[verifier::external_body]
fn addr_mix(h: &Heap) -> u64 {
    (h as *const Heap as u64).rotate_left(17)
}

} // verus!

/// Kani check of `addr_mix` (`verus/ledger/src/heap/flush.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_flush {
    use super::Heap;

    /// `addr_mix` has no `ensures`, so Verus assumes only that it
    /// returns. The harness checks more: for a `Heap` on the stack, at
    /// the address Kani gives it, the call returns that address rotated
    /// left by 17 bits. The fields (zeroed here) do not enter the result.
    #[kani::proof]
    fn addr_mix_returns() {
        // SAFETY: every field of `Heap` is valid all-zero.
        let h: Heap = unsafe { core::mem::zeroed() };
        let a = &h as *const Heap as u64;
        assert_eq!(super::addr_mix(&h), a.rotate_left(17));
    }
}
