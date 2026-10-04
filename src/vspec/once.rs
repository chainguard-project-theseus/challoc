//! One-shot tokens for the steps of building the allocator that happen
//! once per process. An axiom that fixes a process-wide ghost fact (the
//! permission to the `Shared` storage, a class's slot-token authority,
//! the huge allocator's run authorities) is true only if it runs once:
//! a second run would fix the fact a second way and prove `false`. Each
//! such axiom uses up a [`Once`] token for its step, so a second run no
//! longer verifies. The process's tokens, one per step, come from
//! `api::claim_init`, the compare-exchange that wins initialisation,
//! which succeeds once per process.

use vstd::prelude::*;

verus! {

/// A step of building the allocator that happens once per process.
pub ghost enum Step {
    /// `heap::shared::Shared::begin`: the permission to the process's
    /// `Shared` storage.
    Shared,
    /// `huge::axiom_run_locs`: the huge allocator's run authorities.
    Huge,
    /// `class::table::axiom_slot_loc`: class `c`'s slot-token authority
    /// and guard geometry.
    SlotLoc(int),
    /// `class::table::axiom_fresh_words`: the word permissions of class
    /// `c`'s bitmap.
    FreshWords(int),
}

/// The right to take step [`Once::step`] once. Linear (not `Copy`), and
/// no verified code builds one (the field is private and this module has
/// no constructor): the process's set comes from the trusted
/// `api::claim_init`. An axiom that takes the token by value cannot be
/// called twice with it.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct Once {
    ghost step: Step,
}

impl Once {
    /// The step this token is for.
    pub closed spec fn step(self) -> Step {
        self.step
    }
}

/// `m` holds the token of step `s`.
pub open spec fn holds(m: Map<Step, Once>, s: Step) -> bool {
    m.contains_key(s) && m[s].step() == s
}

/// The tokens of every step: what `api::claim_init` hands its winner.
pub open spec fn all_steps(m: Map<Step, Once>) -> bool {
    forall|s: Step| #[trigger] holds(m, s)
}

/// Take the token of step `s` out of `m`.
pub proof fn take(tracked m: &mut Map<Step, Once>, s: Step) -> (tracked t: Once)
    requires
        holds(*old(m), s),
    ensures
        t.step() == s,
        *final(m) == old(m).remove(s),
        forall|s2: Step| s2 != s ==> #[trigger] holds(*final(m), s2) == holds(*old(m), s2),
{
    m.tracked_remove(s)
}

} // verus!
