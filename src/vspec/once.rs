//! One-shot tokens for the steps of building the allocator that happen
//! once per process. An axiom that fixes a process-wide ghost fact (such
//! as a class's slot-token authority) is true only if it runs once: a
//! second run would fix the fact a second way and prove `false`. Each
//! such axiom uses up a [`Once`] token for its step, so a second run no
//! longer verifies.

use vstd::prelude::*;

verus! {

/// A step of building the allocator that happens once per process.
// Ghost: for the proofs that take a step's token.
#[allow(dead_code)]
pub ghost enum Step {
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

} // verus!
