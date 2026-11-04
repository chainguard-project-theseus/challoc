//! One-shot tokens for the steps of building the allocator that happen
//! once per process. An axiom that fixes a process-wide ghost fact (the
//! permission to the `Shared` storage, a class bitmap's word permissions)
//! is true only if it runs once:
//! a second run would fix the fact a second way and prove `false`. Each
//! such axiom uses up a [`Once`] token for its step, so a second run no
//! longer verifies. The process's tokens, one per step, are in `api::STATE`'s
//! invariant (`api::axiom_state`) until `api::claim_init`, the
//! compare-exchange that wins initialisation, takes them, once per process.

use vstd::prelude::*;

verus! {

/// A step of building the allocator that happens once per process.
// Ghost: for the proofs that take a step's token.
#[allow(dead_code)]
pub ghost enum Step {
    /// `heap::shared::Shared::begin`: the permission to the process's
    /// `Shared` storage.
    Shared,
    /// The word permissions of class `c`'s bitmap (`class::table`,
    /// `SlabTable::new`).
    FreshWords(int),
    /// `api::axiom_statics`: the permissions to the storage of the statics
    /// `api::init` writes itself, the class table and the feature statics.
    Statics,
}

/// The right to take step [`Once::step`] once. Linear (not `Copy`), and
/// no verified code builds one (the field is private and this module has
/// no constructor): the process's set comes from `api::STATE`'s
/// invariant (`api::axiom_state`). An axiom that takes the token by
/// value cannot be called twice with it.
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

/// The tokens of every step but [`Step::Statics`], which `api::init`
/// takes itself: what `heap::Shared::init` is handed.
pub open spec fn shared_steps(m: Map<Step, Once>) -> bool {
    forall|s: Step| s != Step::Statics ==> #[trigger] holds(m, s)
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
