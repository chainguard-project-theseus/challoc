//! The `check_zero` debug mode's checks and their aborts, and the
//! test-only pointer checks.

#[cfg(test)]
use super::abort::wrong_arena;
#[cfg(verus_only)]
use super::slot_loc;
#[cfg(verus_only)]
use super::table::bits_shape;
use super::{ClassState, Pick, SlabTable, SlotToks};
#[cfg(verus_only)]
use crate::layout::{NCLASSES, class_table, process_regions, slots_in};
use crate::purge;
use crate::slab::{SlabDesc, State};
#[cfg(verus_only)]
use crate::vspec::addr::cast_provenance;
use crate::vspec::user::UserMem;
#[cfg(verus_only)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;

impl SlabTable {
    /// Tests: [`check_located`] for a pointer not yet located (`p` must be
    /// a slot start whose bit is set). Returns the arena that owns the
    /// slab. The release path locates through `Regions::classify` and
    /// calls `check_located` or `check_live` itself.
    ///
    /// [`check_located`]: SlabTable::check_located
    #[cfg(test)]
    pub fn check(&self, p: *mut u8) -> u16 {
        let (slab, slot) = self.locate(p);
        self.check_located(slab, slot, p as usize)
    }
}

impl ClassState {
    /// Tests: `SlabTable::check` plus the owner test (`p` must be a slot
    /// start whose bit is set, in a slab of this arena). The release path
    /// runs `check_located`/`check_live` on the located slot and its own
    /// owner test (`wrong_arena` in [`release`]).
    ///
    /// [`release`]: ClassState::release
    #[cfg(test)]
    pub fn check(&self, p: *mut u8) {
        let arena = self.table().check(p);
        if arena != self.arena {
            wrong_arena(p as usize, self.class as usize, arena, self.arena);
        }
    }
}

verus! {

// The `check_zero` debug checks. The walks and the reads of slot memory
// (through permissions to the slots) are verified; the formatted aborts
// are helpers below.
impl ClassState {
    /// The `check_zero` debug mode: the first and
    /// last word of every slot of a pick reported zero must read zero.
    /// Aborts with everything the C-level assertion cannot say: the class,
    /// slab, the slab's state when the refill selected it (`Fresh` blames
    /// the commit path, `Purged` the discard path), the arena, the discard
    /// backend and the offending word. `toks` holds the slots' tokens
    /// (keys from `key`), which the claim just took. The reads of each
    /// slot go through the permission its hand-out will mint
    /// (`heap::live::Live::from_slot`): it is minted here, used for the
    /// reads and dropped with the token given back (`Live::take_slot`), so
    /// no permission to the slots is held when this returns.
    #[cold]
    #[inline(never)]
    pub(super) fn check_zero(&self, slots: &[*mut u8], pick: &Pick, Tracked(toks): Tracked<&mut SlotToks>, Ghost(key): Ghost<int>)
        requires
            crate::sys::may_abort(),
            slots_in(slots@, self.spec_class() as int, 0, slots@.len() as int),
            old(toks).holds(self.spec_table(), slots@, key, 0, slots@.len() as int),
        ensures
            *final(toks) == *old(toks),
    {
        proof {
            use_type_invariant(self);
            self.spec_table().lemma_region();
            self.spec_table().lemma_regions_ok();
            self.spec_table().lemma_slots_wf();
        }
        let size = self.class().size();
        proof {
            lemma_slot_size(self.spec_class() as int);
        }
        for &p in it: slots
            invariant
                crate::sys::may_abort(),
                crate::heap::regions_ok(),
                size >= 8,
                size == class_table()[self.spec_class() as int].size,
                size as int % 8 == 0,
                self.spec_class() < NCLASSES,
                crate::layout::base_aligned(process_regions().class_base[self.spec_class() as int] as int, self.spec_class() as int, process_regions().lg_page),
                self.spec_table().spec_slots().loc == slot_loc(self.spec_class() as int),
                it.seq().len() == slots@.len(),
                forall|i: int| 0 <= i < slots@.len() ==> *#[trigger] it.seq()[i] == slots@[i],
                slots_in(slots@, self.spec_class() as int, 0, slots@.len() as int),
                old(toks).holds(self.spec_table(), slots@, key, 0, slots@.len() as int),
                *toks == *old(toks),
        {
            let ghost c = self.spec_class() as int;
            let ghost k = key + it.index();
            proof {
                assert(p == *it.seq()[it.index() as int]);
                assert(process_regions().is_slot(c, p@.addr));
                lemma_slot_word(c, p);
                assert(toks.entry(self.spec_table(), slots@[it.index() as int], k));
            }
            // The slot's token, out of `toks` into the hand-out's token,
            // with the permission to the slot's bytes.
            let tracked blk = crate::heap::live::Live::from_slot(toks.toks.tracked_remove(k), c, p);
            let ghost t0 = old(toks).toks[k];
            proof {
                blk.lemma_mem();
                blk.lemma_slot_len(p, c);
            }
            for &off in o: &[0, size - 8]
                invariant
                    crate::sys::may_abort(),
                    crate::heap::regions_ok(),
                    size >= 8,
                    size == class_table()[self.spec_class() as int].size,
                    size as int % 8 == 0,
                    c == self.spec_class() as int,
                    p@.addr as int % 8 == 0,
                    process_regions().is_slot(self.spec_class() as int, p@.addr),
                    blk.slot_block(p, c),
                    blk.slot_tok() == t0,
                    blk.mem_ok(),
                    blk.mem().covers(p@.addr as int, size as int),
                    o.seq().len() == 2,
                    *o.seq()[0] == 0,
                    *o.seq()[1] == size - 8,
            {
                proof {
                    assert(off == *o.seq()[o.index() as int]);
                }
                // SAFETY: `p` is a slot this claim just reserved in a
                // committed slab; slots are at least 8 bytes and 8-aligned,
                // and `blk`'s permission covers the slot.
                let word = unsafe { read_word(p, off, Ghost(self.spec_class() as int), Tracked(blk.mem_ref())) };
                if word != 0 {
                    self.dirty_word(pick, size, p, off, word);
                }
            }
            proof {
                // The token back, the permission dropped with it.
                let tracked t = blk.take_slot(c);
                toks.toks.tracked_insert(k, t);
                assert(toks.toks =~= old(toks).toks);
            }
        }
    }

    /// `check_zero`: the first word of every slot of a `Fresh` slab must
    /// read zero before its first claim; a dirty one means the memory was
    /// written before this table ever handed the slab out. The reads go
    /// through `m`, the permission to the slab's slots that `take` lends
    /// under the class lock (`axiom_fresh_slab_mem`).
    #[cold]
    #[inline(never)]
    pub(super) fn check_fresh_slab_is_zero(&self, slab: usize, Tracked(m): Tracked<&UserMem>)
        requires
            crate::sys::may_abort(),
            slab < self.spec_nslabs(),
            !self.spec_table().spec_is_guard(slab),
            m.covers(
                process_regions().slot_at(self.spec_class() as int, slab as int, 0),
                class_table()[self.spec_class() as int].nslots * class_table()[self.spec_class() as int].size,
            ),
            m.provenance() == cast_provenance(),
    {
        proof {
            use_type_invariant(self);
            self.spec_table().lemma_region();
            self.spec_table().lemma_regions_ok();
            self.spec_table().lemma_guard_slab(slab as int);
            lemma_slot_size(self.spec_class() as int);
        }
        let cls = self.class();
        let bm = self.table().bits(slab);
        for slot in 0..cls.nslots()
            invariant
                crate::sys::may_abort(),
                crate::heap::regions_ok(),
                slab < self.spec_nslabs(),
                *cls == class_table()[self.spec_class() as int],
                cls.wf(),
                bits_shape(bm, class_table()[self.spec_class() as int].slab_bits as usize),
                self.spec_class() < NCLASSES,
                !crate::layout::guard_slab(self.spec_class() as int, slab as int),
                crate::layout::base_aligned(process_regions().class_base[self.spec_class() as int] as int, self.spec_class() as int, process_regions().lg_page),
                self.spec_nslabs() as int == process_regions().spec_nslabs(self.spec_class() as int),
                class_table()[self.spec_class() as int].size >= 8,
                m.covers(
                    process_regions().slot_at(self.spec_class() as int, slab as int, 0),
                    class_table()[self.spec_class() as int].nslots * class_table()[self.spec_class() as int].size,
                ),
                m.provenance() == cast_provenance(),
        {
            let p = self.slot_ptr(slab, slot);
            proof {
                let c = self.spec_class() as int;
                process_regions().lemma_is_slot(c, slab as int, slot as int, p@.addr);
                lemma_slot_word(c, p);
                // The slot's first word is inside the slab's slots.
                let (b, sz, n, s) = (process_regions().slot_at(c, slab as int, 0), cls.size as int, cls.nslots as int, slot as int);
                assert(p@.addr as int == b + s * sz);
                assert(s * sz + 8 <= n * sz) by (nonlinear_arith)
                    requires s < n, sz >= 8;
            }
            // SAFETY: a slot of a committed slab; the class lock is held,
            // and `m` covers the slab's slots.
            let word = unsafe { read_word(p, 0, Ghost(self.spec_class() as int), Tracked(m)) };
            if word != 0 {
                self.fresh_slab_dirty(slab, slot, p, word, &bm);
            }
        }
    }

    /// `check_zero`: abort when `slab`'s state byte disagrees with its
    /// shadow, i.e. something outside `SlabDesc` wrote the descriptor.
    #[cold]
    #[inline(never)]
    pub(super) fn check_shadow(&self, slab: usize, at: &str)
        requires
            crate::sys::may_abort(),
    {
        let d = self.desc(slab);
        let (state, shadow) = d.state_bytes();
        if shadow != state.wrapping_add(1) {
            self.shadow_written(slab, at, d, state, shadow)
        }
    }

    /// [`check_zero`](ClassState::check_zero)'s abort, inlined into it:
    /// gathers the slab's state for [`dirty_zero_slot`].
    #[inline(always)]
    fn dirty_word(&self, pick: &Pick, size: usize, p: *mut u8, off: usize, word: u64) -> !
        requires
            crate::sys::may_abort(),
    {
        let d = self.desc(pick.slab as usize);
        let bm = self.table().bits(pick.slab as usize);
        let now = (d.state(), d.live(), d.arena(), bm.allocated(self.class().nslots()), self.class().nslots(), desc_ptr_addr(d));
        dirty_zero_slot(self.class as usize, size, pick, self.arena, p as usize, off, word, now)
    }

    /// [`check_fresh_slab_is_zero`](ClassState::check_fresh_slab_is_zero)'s
    /// abort, inlined into it.
    #[inline(always)]
    fn fresh_slab_dirty(&self, slab: usize, slot: usize, p: *mut u8, word: u64, bm: &crate::bitmap::AtomicBitmap<'_>) -> !
        requires
            crate::sys::may_abort(),
            bits_shape(*bm, class_table()[self.spec_class() as int].slab_bits as usize),
    {
        proof {
            use_type_invariant(self);
            broadcast use crate::vspec::class::axiom_class_table_wf;
            assert(class_table()[self.spec_class() as int].wf());
        }
        let cls = self.class();
        crate::sys::abort_fmt(format_args!(
            "check_zero: class {} slab {slab} (arena {}) is Fresh before its first claim but slot {slot} at {:#x} reads {word:#x} ({} of {} bits set, watermark {}, descriptor at {:#x})",
            self.class,
            self.arena,
            p as usize,
            bm.allocated(cls.nslots()),
            cls.nslots(),
            self.table().committed_slabs(),
            desc_ptr_addr(self.desc(slab))
        ));
    }

    /// [`check_shadow`](ClassState::check_shadow)'s abort, inlined into it.
    #[inline(always)]
    fn shadow_written(&self, slab: usize, at: &str, d: &SlabDesc, state: u8, shadow: u8) -> !
        requires
            crate::sys::may_abort(),
    {
        crate::sys::abort_fmt(format_args!(
            "check_zero: class {} slab {slab} (arena {}) descriptor at {:#x} was written outside the API, seen at {at}: state byte {state:#x}, shadow {shadow:#x} (says {:?}), live {}, arena field {}, dirty_at {}",
            self.class,
            self.arena,
            desc_ptr_addr(d),
            Says(State::try_from_u8(shadow.wrapping_sub(1))),
            d.live(),
            d.arena(),
            d.dirty_at()
        ))
    }
}

/// The decoded shadow byte of [`ClassState::shadow_written`]'s message,
/// formatted as its `Option<State>` (Verus specifies `Debug` per type,
/// and has nothing for `Option`).
struct Says(Option<State>);

// `Says`'s `Debug` (below) writes under any formatter
// (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DebugSpecImpl for Says {
    open spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}

// The derived `Debug` of `Option<State>` under `{:?}`, the only way
// challoc formats `Says`, as a `match`: Verus has no specification of
// `Option`'s `Debug`. Under `{:#?}` the derived one would split the
// `Some` over lines; nothing formats `Says` that way.
impl core::fmt::Debug for Says {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            None => f.write_str("None"),
            Some(s) => {
                f.write_str("Some(")?;
                core::fmt::Debug::fmt(&s, f)?;
                f.write_str(")")
            },
        }
    }
}

/// The address of `d`, for an abort message; nothing is claimed about it.
#[inline(always)]
pub(super) fn desc_ptr_addr(d: &SlabDesc) -> usize {
    core::ptr::from_ref(d) as usize
}

/// Slot `p` of class `c`, guard slabs included, is word aligned when the
/// class's base is aligned to its slab (`SlabTable::inv`).
proof fn lemma_slot_word(c: int, p: *mut u8)
    requires
        process_regions().is_slot_addr(c, p@.addr),
        crate::layout::base_aligned(process_regions().class_base[c] as int, c, process_regions().lg_page),
    ensures
        p@.addr as int % 8 == 0,
{
    lemma_slot_size(c);
    vstd::arithmetic::power2::lemma2_to64();
    assert(crate::meta::is_pow2(8)) by {
        assert(8 == pow2(3));
    }
    process_regions().lemma_slot_aligned_in(c, p@.addr, 8);
}

/// Class `c`'s size is a whole number of words, at least one.
proof fn lemma_slot_size(c: int)
    requires
        0 <= c < NCLASSES,
    ensures
        class_table()[c].size as int % 8 == 0,
        class_table()[c].size >= 8,
{
    crate::vspec::class::axiom_class_table_wf(c);
    crate::layout::lemma_class_size_quantum(c);
    assert(crate::QUANTUM as int % 8 == 0);
    crate::layout::lemma_aligned_down(class_table()[c].size as int, crate::QUANTUM as int, 8);
}

/// The word at `p + off` of a slot `p` of class `c`, read once through
/// the permission `m` to it (nothing is claimed about the value). The
/// contract places the word inside the slot, aligned; `m` covers the
/// slot's bytes up to the word's end, through `p`'s provenance.
///
/// # Safety
/// `p + off` is an aligned word that `m` covers.
#[inline(always)]
unsafe fn read_word(p: *mut u8, off: usize, Ghost(c): Ghost<int>, Tracked(m): Tracked<&UserMem>) -> u64
    requires
        crate::heap::regions_ok(),
        process_regions().is_slot(c, p@.addr),
        off as int + 8 <= class_table()[c].size,
        (p@.addr as int + off as int) % 8 == 0,
        m.provenance() == p@.provenance,
        forall|x: int| p@.addr <= x < p@.addr + off + 8 ==> #[trigger] m.dom().contains(x),
{
    proof {
        broadcast use {vstd::layout::layout_of_primitives, vstd::layout::group_align_properties, vstd::layout::align_nonzero};
        crate::vspec::class::axiom_class_table_wf(c);
        process_regions().lemma_slot_in_region(c, p@.addr);
        // The word is inside the allocation, which `add` needs.
        m.lemma_valid_reads(p, off + 8);
        crate::vspec::ptr::axiom_ptr_add_addr(p, off);
        let q = crate::vspec::ptr::ptr_add(p, off);
        assert forall|x: int| q@.addr <= x < q@.addr + 8 implies #[trigger] m.dom().contains(x) by {
            assert(p@.addr <= x < p@.addr + off + 8);
        }
        m.lemma_valid_reads(q, 8);
        crate::vspec::ptr::axiom_plain_u64();
        // A `u64` is aligned to its size, so to its alignment.
        crate::layout::lemma_aligned_down(q@.addr as int, 8, vstd::layout::align_of::<u64>() as int);
    }
    // SAFETY: `m` makes the eight bytes at `p + off` valid for reads
    // through `p`, and they are aligned.
    unsafe { core::ptr::read_volatile(p.add(off) as *const u64) }
}

/// `check_zero` invariant: a slab with live slots was `Fresh` at selection.
#[cold]
#[inline(never)]
pub(super) fn fresh_with_live(class: usize, slab: usize, arena: u16, desc: &SlabDesc, set: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!(
        "check_zero: class {class} slab {slab} (arena {arena}) is Fresh with live {} ({set} bits set, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc_ptr_addr(desc)
    ))
}

/// `check_zero` invariant: a release reached a slab that reads `Fresh`.
#[cold]
#[inline(never)]
pub(super) fn release_into_fresh(class: usize, slab: usize, arena: u16, desc: &SlabDesc, n: usize) -> !
    requires
        crate::sys::may_abort(),
{
    crate::sys::abort_fmt(format_args!(
        "check_zero: release of {n} slot(s) into class {class} slab {slab} (arena {arena}) whose descriptor reads Fresh (live {}, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc_ptr_addr(desc)
    ))
}

/// The `check_zero` abort: a slot handed out as zero has a dirty word.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn dirty_zero_slot(
    class: usize,
    size: usize,
    pick: &Pick,
    arena: u16,
    p: usize,
    off: usize,
    word: u64,
    now: (State, usize, u16, usize, usize, usize),
) -> !
    requires
        crate::sys::may_abort(),
{
    let advice = purge::advice_name();
    let (state, live, owner, set, nslots, _) = now;
    crate::sys::abort_fmt(format_args!(
        "check_zero: slot {p:#x} (class {class}, size {size}, slab {}, arena {arena}) was handed out as zero from a {:?} slab but its word at offset {off} reads {word:#x} (purge advice {advice}; descriptor at {:#x} now {state:?} live {live} owner {owner}, {set} of {nslots} bits set, pick of {} slots)",
        pick.slab, pick.state, now.5, pick.n
    ))
}

} // verus!

/// Kani check of `Says`'s `Debug` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_verify {
    use super::Says;
    use crate::kani_proofs::fmt::formats_debug_flat;
    use crate::slab::State;

    /// S93 `DebugSpecImpl for Says` (`fmt_req` true): `None` and every
    /// state, each formatted in turn (`{:?}` only: Kani does not finish
    /// `{:#?}` of an `Option`, whose `PadAdapter` path the record names
    /// as a gap; challoc formats `Says` with `{:?}`).
    #[kani::proof]
    #[kani::unwind(6)]
    fn debug_says() {
        let v = [
            None,
            Some(State::Fresh),
            Some(State::Active),
            Some(State::Empty),
            Some(State::Purged),
        ];
        for s in v {
            formats_debug_flat(&Says(s));
        }
    }
}
