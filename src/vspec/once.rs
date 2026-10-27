//! One-shot tokens for the steps of building the allocator that happen
//! once per process. An axiom that fixes a process-wide ghost fact (the
//! permission to the `Shared` storage, a class's slot-token authority,
//! the huge allocator's run authorities) is true only if it runs once:
//! a second run would fix the fact a second way and prove `false`. Each
//! such axiom uses up a [`Once`] token for its step, so a second run no
//! longer verifies.

use vstd::prelude::*;

verus! {

/// A step of building the allocator that happens once per process.
// Ghost: for the proofs that take a step's token.
#[allow(dead_code)]
pub ghost enum Step {
    /// `heap::shared::Shared::begin`: the permission to the process's
    /// `Shared` storage.
    Shared,
    /// The huge allocator's run authorities (`huge::axiom_run_locs`,
    /// `Huge::new`).
    Huge,
    /// Class `c`'s slot-token authority (`class::table`,
    /// `SlabTable::new`).
    SlotLoc(int),
    /// The word permissions of class `c`'s bitmap (`class::table`,
    /// `SlabTable::new`).
    FreshWords(int),
}

/// The right to take step `step` once. Linear (not `Copy`), and no
/// verified code builds one: the field is private and this module has
/// no constructor.
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

/// The tokens of every step: what `heap::Shared::init` is handed.
pub open spec fn shared_steps(m: Map<Step, Once>) -> bool {
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
