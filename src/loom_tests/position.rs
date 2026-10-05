//! The policy's position (`policy.rs`, `Policy::scaled16`): the real
//! wrappers of `vspec/count.rs` (`swap`, `load`) over loom's `AtomicU32`,
//! driven by copies of the `Policy` steps that call them, with their
//! orderings (both `Relaxed`):
//!
//! - [`Policy::apply`] is the position's step of `Policy::apply`:
//!   `count::swap` of one more than the band's position (0 outside the
//!   scaled band), whose previous value decides `changed`;
//! - [`Policy::position`] is `Policy::position`: `count::load`.
//!
//! `apply` takes no lock: `policy::evaluate` runs on whichever thread's
//! sample is due, and `Shared::init` applies once before publishing. Two
//! threads apply concurrently here. Checked: every load returns a value
//! some `apply` stored, so at most `FRAC_ONE + 1` (what `FracPred`
//! bounds); and the swaps form one chain: the values the swaps return and
//! the final value are the initial 0 and the stored values, each once, so
//! each swap returned the value just before its own in the position's
//! modification order (what `changed` compares). Left out: the other
//! fields `apply` writes (`Relaxed` heuristics, out of scope in
//! `verus/WEAKMEM.md`).

use super::model;
use crate::sync::atomic::{AtomicU32, Ordering};
use crate::vspec::count;
use loom::thread;
use std::boxed::Box;
use vstd::prelude::Tracked;

/// `policy::FRAC_ONE`.
const FRAC_ONE: u32 = 0x1_0000;

/// The two values applied: one more than the positions of
/// `Band::Scaled(0x8000)` and of `Band::Scaled(FRAC_ONE)`, the largest
/// (`apply` clamps the position to `FRAC_ONE`).
const A: u32 = 0x8000 + 1;
const B: u32 = FRAC_ONE + 1;

struct Policy {
    /// `Policy::scaled16`.
    scaled16: AtomicU32,
    /// The swap of [`Policy::apply`] split into a load and a store, for
    /// the control.
    split: bool,
}

// `cfg(test)` again: `verus/seams.sh` skips test code only by the
// attribute on the item, and the permissions minted below are not seams.
#[cfg(test)]
impl Policy {
    /// `Policy::new`: position 0.
    fn new(split: bool) -> Policy {
        Policy {
            scaled16: AtomicU32::new(0),
            split,
        }
    }

    /// `Policy::position`.
    fn position(&self) -> u32 {
        count::load(&self.scaled16, Ordering::Relaxed, Tracked::assume_new())
    }

    /// `Policy::apply`'s swap of the position: the previous value.
    fn apply(&self, scaled: u32) -> u32 {
        if self.split {
            let prev = self.scaled16.load(Ordering::Relaxed);
            self.scaled16.store(scaled, Ordering::Relaxed);
            prev
        } else {
            count::swap(
                &self.scaled16,
                scaled,
                Ordering::Relaxed,
                Tracked::assume_new(),
            )
        }
    }
}

/// Leaked, as in `init.rs`: a failed model must not drop loom objects.
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

/// Two threads apply `A` and `B` while the main thread reads the
/// position twice.
fn run(split: bool) {
    model(move || {
        let p = leak(Policy::new(split));
        let a = thread::spawn(move || p.apply(A));
        let b = thread::spawn(move || p.apply(B));
        for _ in 0..2 {
            let f = p.position();
            assert!(f <= FRAC_ONE + 1, "position {f:#x} above FRAC_ONE + 1");
            assert!([0, A, B].contains(&f), "position {f:#x} never stored");
        }
        let (pa, pb) = (a.join().unwrap(), b.join().unwrap());
        let mut chain = [pa, pb, p.position()];
        chain.sort_unstable();
        let mut stored = [0, A, B];
        stored.sort_unstable();
        assert_eq!(
            chain, stored,
            "the swaps' previous positions are not one chain"
        );
    });
}

/// The shipped code: one atomic swap.
#[test]
fn loom_position_apply_swaps() {
    run(false);
}

/// The chain check has teeth: a swap split into a load and a store lets
/// both `apply`s return the same previous position.
#[test]
#[should_panic(expected = "one chain")]
fn loom_position_split_swap_breaks_the_chain() {
    run(true);
}
