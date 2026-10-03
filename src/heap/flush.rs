//! Refill and flush: the batches between a cache and its slabs. The
//! refill claims a batch into an empty cache; the flush releases a
//! cache's highest addresses grouped by slab ([`group_by_slab`]) and
//! aborts on a double free that straddles the split.

use core::ptr;

use crate::arena::Arena;
use crate::layout::CLASSES;
#[cfg(verus_only)]
use crate::layout::{NCLASSES, process_regions, slots_in};
use crate::sys::pac;
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::zero::Zeroed;

#[cfg(verus_only)]
use super::CachePerms;
use super::shared::seed;
use super::{CacheHeader, FOREIGN, Heap, Shared, assert_refill_empty, stack_mut};
#[cfg(verus_only)]
use super::{
    cell_ok, from_all, gather_all_ents, in_entries, lemma_from_refl, lemma_from_slots,
    lemma_from_sub, lemma_from_trans, lemma_log_same, lemma_plain_stored, lemma_stack_fill,
    log_adds, move_reversed, pac_mode, plain, scatter_ents, slot_ptr, split_ok, tagged, toks_kept,
};
#[cfg(verus_only)]
use crate::class::slot_loc;
#[cfg(verus_only)]
use crate::class::{aset, distinct, ptr_addrs};
use vstd::map::Map;
#[cfg(verus_only)]
use vstd::modes::tracked_swap;
#[cfg(verus_only)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, int, verus};
use vstd::raw_ptr::{ptr_mut_ref, ptr_ref};

verus! {

impl Heap {
    /// The batch claim behind a refill or a run rebuild: `cs.claim`, or
    /// with `slot_randomize` `cs.claim_random` with this heap's xorshift
    /// (the scan starts at a random bitmap word and the batch is shuffled;
    /// design section 4). The key is read from the configuration here, on
    /// the slow path, so the fast paths carry nothing for it.
    #[inline]
    pub(super) fn claim(
        &mut self,
        shared: &Shared,
        cs: &crate::class::ClassState,
        out: &mut [*mut u8],
        Tracked(toks): Tracked<&mut crate::class::SlotToks>,
    ) -> (r: (crate::class::Claim, Tracked<Map<int, Zeroed>>))
        requires
            crate::sys::may_syscall(),
        ensures
            final(self).perms == old(self).perms,
            final(self).list == old(self).list,
            final(self).keys == old(self).keys,
            final(self).arena == old(self).arena,
            final(self).home_hdrs == old(self).home_hdrs,
            final(self).others == old(self).others,
            r.0.n <= old(out)@.len(),
            final(out)@.len() == old(out)@.len(),
            slots_in(final(out)@, cs.spec_class() as int, 0, r.0.n as int),
            final(toks).owns(cs.spec_table(), final(out)@, 0, 0, r.0.n as int),
            crate::class::keyed(final(toks).toks, ptr_addrs(final(out)@.subrange(0, r.0.n as int)), slot_loc(cs.spec_class() as int), 0, r.0.n as int),
            cs.spec_table().spec_slots().loc == slot_loc(cs.spec_class() as int),
            crate::class::claim_zero_ok(r.0, final(out)@, r.1@, cs.spec_class() as int),
    {
        cs.slot_tokens_are_the_class_s();
        let r = if slot_randomize(shared) {
            let rng = self.rng();
            cs.claim_random_zero(&shared.meta, out, rng, Tracked(toks))
        } else {
            cs.claim_tokens(&shared.meta, out, Tracked(toks))
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
    /// Only `rng` changes; the address mix ([`addr_mix`]) and `seed` (a
    /// syscall) are trusted and may return anything.
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
    pub(super) unsafe fn refill(&mut self, shared: &Shared, arena: &Arena, cache: *mut CacheHeader, class: usize) -> (r: u32)
        requires
            crate::sys::may_syscall(),
            class < NCLASSES,
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
            shared.wf(),
            old(self).hdr(cache).cap != 0,
        ensures
            final(self).keeps(*old(self)),
            r == final(self).hdr(cache).len,
    {
        let ghost old_perms = self.perms@;
        proof {
            assert(cell_ok(cache, old_perms.hdrs[cache], old_perms.slots));
        }
        // SAFETY: the cache is empty and has `cap` slots of storage.
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
            let (claim, Tracked(mut ztok)) = self.claim(shared, cs, out, Tracked(&mut toks));
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
    ///   grouped, and the kept half stays sorted for the next flush.
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
    #[verifier::spinoff_prover]
    #[verifier::rlimit(300)]
    pub(super) unsafe fn flush(&mut self, shared: &Shared, arena: u16, cache: *mut CacheHeader, class: usize, n: usize)
        requires
            crate::sys::may_syscall(),
            class < NCLASSES,
            shared.wf(),
            old(self).wf(),
            old(self).owns(cache),
            old(self).hdr_class(cache) == class,
            n <= old(self).hdr(cache).len,
        ensures
            final(self).keeps(*old(self)),
            final(self).hdr(cache).len == old(self).hdr(cache).len - n,
            final(self).same_shape(*old(self)),
            final(self).tcache_on() == old(self).tcache_on(),
            forall|p: *mut CacheHeader| old(self).owns(p) && p != cache ==> #[trigger] final(self).held(p) == old(self).held(p),
            forall|p: *mut CacheHeader| old(self).owns(p) && p != cache ==> #[trigger] final(self).hdr(p) == old(self).hdr(p),
            // Theorem 10's accounting: the slots `cache` no longer holds are
            // the ones appended to the released log, and it holds no new one.
            final(self).held(cache).to_set() <= old(self).held(cache).to_set(),
            log_adds(final(self).released(), old(self).released(), old(self).held(cache).to_set() - final(self).held(cache).to_set()),
    {
        // The flush writes the permissions alone; `lemma_keeps_perms`
        // carries the `others` list across, so its conjuncts stay hidden.
        hide(Heap::list_ok);
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
        // Theorem 10's accounting, out of the block below: the cached
        // addresses, the kept ones and the released ones.
        let ghost mut g_old = Seq::<int>::empty();
        let ghost mut g_kept = Seq::<int>::empty();
        let ghost mut g_rel = Seq::<int>::empty();
        // SAFETY: `n <= len <= cap` entries exist.
        unsafe {
            let pac = shared.pac();
            let tracked perms = self.perms.borrow_mut();
            let slots = ptr_ref(cache, Tracked(perms.hdrs.tracked_borrow(cache))).slots;
            // The cached slots' tokens, gathered by address for the
            // selection and sorts to permute; the kept ones go back under
            // their new entries and the released ones to their bitmap words.
            let ghost c = old_perms.cls[cache];
            let ghost len = old_h.len as int;
            let ghost aold = Seq::new(old_h.len as nat, |i: int| plain(old_perms.slots[slot_ptr(slots, i)].value(), slots) as int);
            proof {
                assert(slots == old_h.slots);
                assert forall|i: int| 0 <= i < len implies {
                    let q = #[trigger] slot_ptr(slots, i);
                    &&& perms.toks.contains_key(q)
                    &&& perms.toks[q].id() == slot_loc(c)
                    &&& perms.toks[q]@ == aold[i]
                } by {
                    old_perms.lemma_tok_get(cache, i);
                    assert(old_perms.ent(cache, i) == slot_ptr(slots, i));
                }
                assert forall|i: int, j: int| 0 <= i < j < len implies slot_ptr(slots, i) != slot_ptr(slots, j) by {
                    old_perms.lemma_ent_apart(cache, i, cache, j);
                }
            }
            let tracked mut bag = gather_all_ents(&mut perms.toks, slots, aold, slot_loc(c), len);
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
                ptr_ref(cache, Tracked(perms.hdrs.tracked_borrow(cache))).slots,
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
            let sorted = rest_len > 0 && if pac { all.is_sorted_by_key(key) } else { all.is_sorted() };
            if sorted {
                // Already in address order: the split is direct, the
                // released half is sorted and so grouped, and the kept half
                // stays sorted for the next flush.
                kept_max = *all.get_unchecked(rest_len - 1);
                ptr_mut_ref(cache, Tracked(perms.hdrs.tracked_borrow_mut(cache))).unsorted_flushes = 0;
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
                if rest_len > 1 && (k < 2 || k % 32 == 0) {
                    if pac {
                        all.get_unchecked_mut(..rest_len).sort_unstable_by_key(key);
                    } else {
                        all.get_unchecked_mut(..rest_len).sort_unstable();
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
                    }
                }
            }
            let ghost a2 = all@;
            proof {
                // Every kept entry was an entry before: the selection and
                // the sort only move them.
                if rest_len > 0 {
                    assert(from_all(a2, a0, rest_len as int));
                }
            }
            // The kept entries' tokens back under their entries.
            let ghost f = |e: *mut u8| plain(e, slots) as int;
            let ghost akept = Seq::new(rest_len as nat, |i: int| f(a2[i]));
            proof {
                assert(a2.to_multiset() == a0.to_multiset());
                assert forall|i: int, j: int| 0 <= i < j < a0.len() implies f(a0[i]) != f(a0[j]) by {
                    assert(old_perms.slots[slot_ptr(slots, i)].is_init());
                    assert(old_perms.slots[slot_ptr(slots, j)].is_init());
                    assert(aold[i] != aold[j]);
                }
                crate::class::lemma_perm_distinct(a0, a2, f);
                assert forall|v: int| aset(akept, 0, rest_len as int).contains(v) implies bag@.contains(v) by {
                    let i = choose|i: int| 0 <= i < rest_len && akept[i] == v;
                    assert(a0.contains(a2[i]));
                    let k = choose|k: int| 0 <= k < a0.len() && a0[k] == a2[i];
                    assert(old_perms.slots[slot_ptr(slots, k)].is_init());
                    assert(aold[k] == v);
                }
                assert(aset(akept, 0, rest_len as int) <= aset(aold, 0, len));
                assert forall|i: int| 0 <= i < len implies slot_ptr(slots, i) == old_perms.ent(cache, i) by {}
                scatter_ents(&mut bag, &mut perms.toks, slots, akept, 0, rest_len as int);
            }
            let ghost t2 = perms.toks;
            let high = all.get_unchecked_mut(rest_len..);
            let ghost h0 = high@;
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
                assert forall|j: int| 0 <= j < h1.len() implies #[trigger] ptr_addrs(h1)[j] == f(a2[rest_len + j]) by {
                    assert(h0[j] == a2[rest_len + j]);
                }
                assert forall|i: int, j: int| 0 <= i < j < h1.len() implies ptr_addrs(h1)[i] != ptr_addrs(h1)[j] by {
                    assert(f(a2[rest_len + i]) != f(a2[rest_len + j]));
                }
                assert forall|v: int| aset(ptr_addrs(h1), 0, h1.len() as int).contains(v) implies bag@.contains(v) by {
                    let j = choose|j: int| 0 <= j < h1.len() && ptr_addrs(h1)[j] == v;
                    let i = rest_len + j;
                    assert(a0.contains(a2[i]));
                    let k = choose|k: int| 0 <= k < a0.len() && a0[k] == a2[i];
                    assert(old_perms.slots[slot_ptr(slots, k)].is_init());
                    assert(aold[k] == v);
                    if aset(akept, 0, rest_len as int).contains(v) {
                        let m = choose|m: int| 0 <= m < rest_len && akept[m] == v;
                        assert(f(a2[m]) != f(a2[i]));
                    }
                }
                assert(aset(ptr_addrs(h1), 0, h1.len() as int) <= aset(aold, 0, len) - aset(akept, 0, rest_len as int));
                crate::class::lemma_perm_addrs(h1, high@);
            }
            let kept_max_plain = if pac && rest_len > 0 { addr_to_ptr(pac::auth(kept_max as usize, slots as usize)) } else { kept_max };
            if low == kept_max_plain {
                straddling_double_free(low, class);
            }
            // The `FOREIGN` cache's batch starts wherever its first slab
            // belongs; `release_routed` follows the owner words from there.
            let owner = if arena == FOREIGN { shared.arena_of_slot(class, *high.get_unchecked(0)) } else { arena };
            shared.release_routed(owner, class, high, Tracked(&mut bag));
            proof {
                // Theorem 10: every cached slot is kept or released.
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
                // The kept entries are old entries, so they still hold
                // slots of the header's class.
                shared.lemma_wf();
                let c = old_perms.cls[cache];
                if rest_len > 0 {
                    assert forall|j: int| 0 <= j < a0.len() implies process_regions().is_slot(c, plain(#[trigger] a0[j], slots)) by {
                        assert(old_perms.ent(cache, j) == slot_ptr(slots, j));
                        assert(old_perms.slots[slot_ptr(slots, j)].is_init());
                    }
                    lemma_from_slots(a0, a2, c, rest_len as int, slots);
                }
                assert forall|i: int| 0 <= i < rest_len implies #[trigger] perms.holds(c, perms.ent(cache, i), perms.hdr(cache).slots) by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    assert(all@[i] == a2[i]);
                }
                CachePerms::lemma_slice_frame(old_perms, old_perms.slots, perms.slots, cache, 0, old_h.len as int);
                CachePerms::lemma_tracks_stack(old_perms, *perms, cache);
                CachePerms::lemma_zero_update_ent(old_perms, *perms, cache);
                // The kept entries hold their tokens; no other header's entry
                // is one of this stack's.
                assert forall|p2: *mut CacheHeader, j: int|
                    old_perms.hdrs.contains_key(p2) && p2 != cache && 0 <= j < old_perms.hdr(p2).cap implies {
                        let q = #[trigger] old_perms.ent(p2, j);
                        &&& perms.slots[q] == old_perms.slots[q]
                        &&& (old_perms.toks.contains_key(q) ==> perms.toks.contains_key(q) && perms.toks[q] == old_perms.toks[q])
                    } by {
                    let q = old_perms.ent(p2, j);
                    if in_entries(slots, q, 0, len) {
                        let k = choose|k: int| 0 <= k < len && q == slot_ptr(slots, k);
                        old_perms.lemma_ent_apart(cache, k, p2, j);
                    }
                    if in_entries(slots, q, 0, rest_len as int) {
                        let k = choose|k: int| 0 <= k < rest_len && q == slot_ptr(slots, k);
                        assert(in_entries(slots, q, 0, len));
                    }
                    assert(perms.toks == t2);
                    assert(t1.contains_key(q) == old_perms.toks.contains_key(q));
                    assert(t2.contains_key(q) == t1.contains_key(q));
                    if old_perms.toks.contains_key(q) {
                        assert(t1[q] == old_perms.toks[q]);
                        assert(t2[q] == t1[q]);
                    }
                }
                assert forall|i: int| 0 <= i < perms.hdr(cache).len implies #[trigger] perms.tok_at(cache, i) by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    assert(all@[i] == a2[i]);
                }
                CachePerms::lemma_tok_update(old_perms, *perms, cache);
                // The kept entries stand for the kept addresses.
                assert forall|i: int| 0 <= i < rest_len implies plain(perms.slots[#[trigger] perms.ent(cache, i)].value(), perms.hdr(cache).slots) as int == akept[i] by {
                    assert(perms.ent(cache, i) == slot_ptr(slots, i));
                    assert(all@[i] == a2[i]);
                }
            }
        }
        proof {
            Heap::lemma_keeps_perms(*old(self), *self);
            Heap::lemma_flush_accounting(*old(self), *self, cache, g_old, g_kept, g_rel);
        }
    }
}

/// Theorem 10's split of one flush: the addresses the cache held (`aold`)
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
/// Trusted: the sweep and the fallback sort; only the batch's order
/// changes (the multiset of entries is kept, so their slot tokens
/// follow). The body hands [`flag_sort`] batches of up to the cache's
/// capacity, while `flag_sort` is verified only for at most `u16::MAX`
/// entries, and `tcache_nslots_small_max` accepts 65536, one more. Only a
/// flush of a whole 65536-slot cache reaches that length (every other
/// flush releases at most three quarters of one); a debug build then
/// aborts at `flag_sort`'s entry assertion (the test
/// `whole_flush_of_a_65536_slot_cache_overruns_flag_sort`). The release
/// build still groups it correctly, unverified: the counts stay below
/// 65536 (a batch in one bucket returns before the sort), the prefix sum
/// wraps only at the last bucket, whose `end` becomes 0 so its own pass
/// never runs, and its `next` reaches 65536 (and wraps) only with the
/// last slot of its region filled, after which nothing is placed there.
/// 400 randomised 65536-entry release runs of `flag_sort` came out
/// grouped, and a whole release flush of a 65536-slot cache left the
/// class's `live` counts exact.
#[verifier::external_body]
pub(super) fn group_by_slab(shared: &Shared, class: usize, high: &mut [*mut u8]) -> *mut u8
    requires
        class < NCLASSES,
        old(high)@.len() > 0,
    ensures
        final(high)@.len() == old(high)@.len(),
        final(high)@.to_multiset() == old(high)@.to_multiset(),
{
    let base = shared.regions.class_base[class];
    let lg = CLASSES[class].lg_slab as u32;
    let key = |p: *mut u8| (p as usize).wrapping_sub(base) >> lg;
    let mut low = high[0];
    let (mut kmin, mut kmax) = (usize::MAX, 0usize);
    for &p in high.iter() {
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
        flag_sort(high, span, |p| key(p) - kmin);
    } else {
        high.sort_unstable();
    }
    low
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
spec fn slack(next: Seq<u16>, end: Seq<u16>, n: int) -> int
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

proof fn lemma_slack_step(next: Seq<u16>, end: Seq<u16>, n: int, k: int, x: u16)
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

proof fn lemma_slack_same(a: Seq<u16>, b: Seq<u16>, end: Seq<u16>, n: int)
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

proof fn lemma_slack_nonneg(next: Seq<u16>, end: Seq<u16>, n: int)
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
spec fn placed<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, c: int, j: int) -> bool {
    lo(key, v0, c) <= j < next[c] ==> kf(key, s[j]) == c
}

/// The placement state of [`flag_sort`]: bucket `c < n` owns
/// `[lo(c), lo(c + 1))`, `end[c]` is its end and `next[c]` its cursor, the
/// entries below every cursor are in their bucket, and `s` holds as many
/// entries of each bucket as `v0`.
spec fn inv<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, end: Seq<u16>, n: int) -> bool {
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
proof fn lemma_room<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, end: Seq<u16>, n: int, b: int, i: int, p: *mut u8)
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
proof fn lemma_step<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, end: Seq<u16>, n: int, b: int, i: int, p: *mut u8, j: int, x: u16)
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
proof fn lemma_fill<F: Fn(*mut u8) -> usize>(key: F, v: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, end: Seq<u16>, n: int, b: int, i: int, p: *mut u8, x: u16)
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
proof fn lemma_sorted<F: Fn(*mut u8) -> usize>(key: F, s: Seq<*mut u8>, v0: Seq<*mut u8>, next: Seq<u16>, end: Seq<u16>, n: int)
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
/// scratch beyond the bucket cursors.
///
/// Verus checks it for a deterministic `key` below `nb`: every cursor
/// stays inside its bucket (no index leaves `v`, no `u16` counter
/// overflows), every loop ends, and `v` ends up a permutation of what it
/// held (the same multiset), in ascending bucket order.
pub(super) fn flag_sort(v: &mut [*mut u8], nb: usize, key: impl Fn(*mut u8) -> usize)
    requires
        nb <= 256,
        old(v).len() <= u16::MAX,
        forall|p: *mut u8| key.requires((p,)),
        forall|p: *mut u8, k: usize| key.ensures((p,), k) ==> k < nb,
        forall|p: *mut u8, k1: usize, k2: usize| key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
    ensures
        final(v)@.len() == old(v)@.len(),
        forall|c: int| 0 <= c < nb ==> count(key, final(v)@, c) == count(key, old(v)@, c),
        final(v)@.to_multiset() == old(v)@.to_multiset(),
        forall|i: int, j: int| 0 <= i <= j < final(v)@.len() ==> #[trigger] kf(key, final(v)@[i]) <= #[trigger] kf(key, final(v)@[j]),
{
    debug_assert!(nb <= GROUP_BUCKETS && v.len() <= u16::MAX as usize);
    let ghost v0 = v@;
    let mut next = [0u16; GROUP_BUCKETS];
    let mut end = [0u16; GROUP_BUCKETS];
    for &p in it: v.iter()
        invariant
            v@ == v0,
            nb <= GROUP_BUCKETS,
            v0.len() <= u16::MAX,
            forall|p: *mut u8| key.requires((p,)),
            forall|p: *mut u8, k: usize| key.ensures((p,), k) ==> k < nb,
            forall|p: *mut u8, k1: usize, k2: usize| key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
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
        end[key(p)] += 1;
    }
    proof {
        assert(v0.take(v0.len() as int) =~= v0);
        lemma_count_below_le(key, v0, nb as int);
    }
    let mut acc = 0u16;
    for b in 0..nb
        invariant
            v@ == v0,
            nb <= GROUP_BUCKETS,
            v0.len() <= u16::MAX,
            count_below(key, v0, nb as int) <= v0.len(),
            acc == count_below(key, v0, b as int),
            forall|c: int| 0 <= c < b ==> next@[c] == count_below(key, v0, c) && end@[c] == count_below(key, v0, c + 1),
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
            v0.len() <= u16::MAX,
            forall|p: *mut u8| key.requires((p,)),
            forall|p: *mut u8, k: usize| key.ensures((p,), k) ==> k < nb,
            forall|p: *mut u8, k1: usize, k2: usize| key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
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
                v0.len() <= u16::MAX,
                forall|p: *mut u8| key.requires((p,)),
                forall|p: *mut u8, k: usize| key.ensures((p,), k) ==> k < nb,
                forall|p: *mut u8, k1: usize, k2: usize| key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
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
            let mut kb = key(p);
            proof {
                assert(v@.update(i as int, p) =~= v@);
            }
            while kb != b
                invariant
                    b < nb,
                    v@.len() == v0.len(),
                    nb <= GROUP_BUCKETS,
                    v0.len() <= u16::MAX,
                    forall|p: *mut u8| key.requires((p,)),
                    forall|p: *mut u8, k: usize| key.ensures((p,), k) ==> k < nb,
                    forall|p: *mut u8, k1: usize, k2: usize| key.ensures((p,), k1) && key.ensures((p,), k2) ==> k1 == k2,
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
pub(super) fn straddling_double_free(p: *mut u8, class: usize) -> ! {
    crate::sys::abort_fmt(format_args!("double free of {:#x} (class {class}): both copies cached, one at the flush boundary", p as usize))
}

} // verus!

verus! {

/// `Config::slot_randomize` (design section 4), read on the slow path.
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

/// `seed` reads `getrandom(2)` (or the clock).
///
/// Trusted: `seed` is outside `verus!`; claims nothing about the value.
pub assume_specification[ seed ]() -> u64
    requires
        crate::sys::may_syscall(),
;

} // verus!
