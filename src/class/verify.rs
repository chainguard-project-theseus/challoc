//! The `check_zero` debug mode's checks and their aborts, and the
//! test-only pointer checks.

#[cfg(test)]
use super::abort::wrong_arena;
use super::{ClassState, Pick, SlabTable};
use crate::purge;
use crate::slab::{SlabDesc, State};

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

    /// The `check_zero` debug mode: the first and
    /// last word of every slot of a pick reported zero must read zero.
    /// Aborts with everything the C-level assertion cannot say: the class,
    /// slab, the slab's state when the refill selected it (`Fresh` blames
    /// the commit path, `Purged` the discard path), the arena, the discard
    /// backend and the offending word.
    #[cold]
    #[inline(never)]
    pub(super) fn check_zero(&self, slots: &[*mut u8], pick: &Pick) {
        let size = self.class().size();
        for &p in slots {
            for off in [0, size - 8] {
                // SAFETY: `p` is a slot this claim just reserved in a
                // committed slab; slots are at least 16 bytes and 8-aligned.
                let word = unsafe { core::ptr::read_volatile(p.add(off) as *const u64) };
                if word != 0 {
                    let d = self.desc(pick.slab as usize);
                    let bm = self.table().bits(pick.slab as usize);
                    let now = (d.state(), d.live(), d.arena(), bm.allocated(self.class().nslots()), self.class().nslots(), d as *const SlabDesc as usize);
                    dirty_zero_slot(self.class as usize, size, pick, self.arena, p as usize, off, word, now);
                }
            }
        }
    }

    /// `check_zero`: the first word of every slot of a `Fresh` slab must
    /// read zero before its first claim; a dirty one means the memory was
    /// written before this table ever handed the slab out.
    #[cold]
    #[inline(never)]
    pub(super) fn check_fresh_slab_is_zero(&self, slab: usize) {
        let cls = self.class();
        let bm = self.table().bits(slab);
        for slot in 0..cls.nslots() {
            let p = self.slot_ptr(slab, slot);
            // SAFETY: a slot of a committed slab; the class lock is held.
            let word = unsafe { core::ptr::read_volatile(p as *const u64) };
            if word != 0 {
                panic!(
                    "check_zero: class {} slab {slab} (arena {}) is Fresh before its first claim but slot {slot} at {:#x} reads {word:#x} ({} of {} bits set, watermark {}, descriptor at {:#x})",
                    self.class,
                    self.arena,
                    p as usize,
                    bm.allocated(cls.nslots()),
                    cls.nslots(),
                    self.table().committed_slabs(),
                    self.desc(slab) as *const SlabDesc as usize
                );
            }
        }
    }

    /// `check_zero`: abort when `slab`'s state byte disagrees with its
    /// shadow, i.e. something outside `SlabDesc` wrote the descriptor.
    #[cold]
    #[inline(never)]
    pub(super) fn check_shadow(&self, slab: usize, at: &str) {
        let d = self.desc(slab);
        let (state, shadow) = d.state_bytes();
        if shadow != state.wrapping_add(1) {
            panic!(
                "check_zero: class {} slab {slab} (arena {}) descriptor at {:#x} was written outside the API, seen at {at}: state byte {state:#x}, shadow {shadow:#x} (says {:?}), live {}, arena field {}, dirty_at {}",
                self.class,
                self.arena,
                d as *const SlabDesc as usize,
                State::try_from_u8(shadow.wrapping_sub(1)),
                d.live(),
                d.arena(),
                d.dirty_at()
            )
        }
    }
}

/// `check_zero` invariant: a slab with live slots was `Fresh` at selection.
#[cold]
#[inline(never)]
pub(super) fn fresh_with_live(class: usize, slab: usize, arena: u16, desc: &SlabDesc, set: usize) -> ! {
    panic!(
        "check_zero: class {class} slab {slab} (arena {arena}) is Fresh with live {} ({set} bits set, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc as *const SlabDesc as usize
    )
}

/// `check_zero` invariant: a release reached a slab that reads `Fresh`.
#[cold]
#[inline(never)]
pub(super) fn release_into_fresh(class: usize, slab: usize, arena: u16, desc: &SlabDesc, n: usize) -> ! {
    panic!(
        "check_zero: release of {n} slot(s) into class {class} slab {slab} (arena {arena}) whose descriptor reads Fresh (live {}, owner {}, dirty_at {}), descriptor at {:#x}",
        desc.live(),
        desc.arena(),
        desc.dirty_at(),
        desc as *const SlabDesc as usize
    )
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
) -> ! {
    let (backend, advice) = purge::backend_name();
    let (state, live, owner, set, nslots, _) = now;
    panic!(
        "check_zero: slot {p:#x} (class {class}, size {size}, slab {}, arena {arena}) was handed out as zero from a {:?} slab but its word at offset {off} reads {word:#x} (discard backend {backend}, {advice}; descriptor at {:#x} now {state:?} live {live} owner {owner}, {set} of {nslots} bits set, pick of {} slots)",
        pick.slab, pick.state, now.5, pick.n
    )
}
