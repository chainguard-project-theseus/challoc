//! The `check_zero` debug mode's checks and their aborts, and the
//! test-only pointer checks.

#[cfg(test)]
use super::abort::wrong_arena;
#[cfg(verus_only)]
use super::table::bits_shape;
use super::{ClassState, Pick, SlabTable};
#[cfg(verus_only)]
use crate::layout::class_table;
use crate::purge;
use crate::slab::{SlabDesc, State};
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

// The `check_zero` debug checks. The walks are verified; the reads of
// slot memory and the formatted aborts are trusted helpers below.
impl ClassState {
    /// The `check_zero` debug mode: the first and
    /// last word of every slot of a pick reported zero must read zero.
    /// Aborts with everything the C-level assertion cannot say: the class,
    /// slab, the slab's state when the refill selected it (`Fresh` blames
    /// the commit path, `Purged` the discard path), the arena, the discard
    /// backend and the offending word.
    #[cold]
    #[inline(never)]
    pub(super) fn check_zero(&self, slots: &[*mut u8], pick: &Pick)
        requires
            crate::sys::may_abort(),
    {
        let size = self.class().size();
        for &p in slots
            invariant
                crate::sys::may_abort(),
                size >= 8,
        {
            for &off in &[0, size - 8]
                invariant
                    crate::sys::may_abort(),
                    size >= 8,
            {
                // SAFETY: `p` is a slot this claim just reserved in a
                // committed slab; slots are at least 16 bytes and 8-aligned.
                let word = unsafe { read_word(p, off) };
                if word != 0 {
                    self.dirty_word(pick, size, p, off, word);
                }
            }
        }
    }

    /// `check_zero`: the first word of every slot of a `Fresh` slab must
    /// read zero before its first claim; a dirty one means the memory was
    /// written before this table ever handed the slab out.
    #[cold]
    #[inline(never)]
    pub(super) fn check_fresh_slab_is_zero(&self, slab: usize)
        requires
            crate::sys::may_abort(),
            slab < self.spec_nslabs(),
    {
        let cls = self.class();
        let bm = self.table().bits(slab);
        for slot in 0..cls.nslots()
            invariant
                crate::sys::may_abort(),
                slab < self.spec_nslabs(),
                *cls == class_table()[self.spec_class() as int],
                cls.wf(),
                bits_shape(bm, class_table()[self.spec_class() as int].slab_bits as usize),
        {
            let p = self.slot_ptr(slab, slot);
            // SAFETY: a slot of a committed slab; the class lock is held.
            let word = unsafe { read_word(p, 0) };
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

// The derived `Debug` of `Option<State>` writes under any formatter
// (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DebugSpecImpl for Says {
    open spec fn fmt_req(&self, f: &core::fmt::Formatter<'_>) -> bool {
        true
    }
}

} // verus!

impl core::fmt::Debug for Says {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

verus! {

/// The address of `d`, for an abort message. Trusted: Verus has no
/// reference-to-pointer cast; nothing is claimed about the address.
#[inline(always)]
#[verifier::external_body]
pub(super) fn desc_ptr_addr(d: &SlabDesc) -> usize {
    d as *const SlabDesc as usize
}

/// The word at `p + off`, read once (trusted: nothing is claimed about the
/// value).
///
/// # Safety
/// `p + off` is an aligned word of committed slot memory.
#[inline(always)]
#[verifier::external_body]
unsafe fn read_word(p: *mut u8, off: usize) -> u64 {
    // SAFETY: guaranteed by the caller.
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

/// Kani checks of `Says`'s `Debug` and `desc_ptr_addr` (records under
/// `verus/seams/`).
#[cfg(kani)]
mod kani_verify {
    use super::{Says, desc_ptr_addr};
    use crate::kani_proofs::fmt::formats_debug_flat;
    use crate::slab::{SlabDesc, State};

    /// S95 `DebugSpecImpl for Says` (`fmt_req` true): `None` and every
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

    /// S96 `desc_ptr_addr`, no `ensures`: it returns (the address of the
    /// descriptor); the body reads no field, so one descriptor stands
    /// for all.
    #[kani::proof]
    fn desc_ptr_addr_returns() {
        // SAFETY: `SlabDesc` is five atomic integers, for which all-zero
        // is a valid value.
        let d: SlabDesc = unsafe { core::mem::zeroed() };
        assert_eq!(desc_ptr_addr(&d), &d as *const SlabDesc as usize);
    }
}
