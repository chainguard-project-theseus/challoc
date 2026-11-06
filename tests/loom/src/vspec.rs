//! challoc's own atomic wrappers (`src/vspec/{atom,byte,count,entry,flags,word}.rs`),
//! compiled here from the crate's files under `cfg(loom)`, where their
//! `crate::sync::atomic` is loom's (`src/sync.rs`): the models call these
//! bodies, not copies of them. The `const fn new`s of `byte`, `count` and
//! `entry` are left out under `cfg(loom)` (loom's atomics are not built
//! in `const`), so the models make the atomic with loom's `new` and pass
//! the erased permission as `Tracked::assume_new()`.
//!
//! The GenMC harnesses (`tests/genmc/prelude.rs`) compile this file too,
//! without `cfg(loom)`, so there `crate::sync::atomic` is `core`'s and
//! GenMC interprets the wrappers' atomic instructions.

#[path = "../../../src/vspec/atom.rs"]
pub mod atom;
#[path = "../../../src/vspec/byte.rs"]
pub mod byte;
#[path = "../../../src/vspec/count.rs"]
pub mod count;
#[path = "../../../src/vspec/entry.rs"]
pub mod entry;
#[path = "../../../src/vspec/flags.rs"]
pub mod flags;
#[path = "../../../src/vspec/word.rs"]
pub mod word;

use crate::sync::atomic::{AtomicUsize, Ordering};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};
use vstd::prelude::*;

/// The predicate of the invariant `atom::load_usize_seen` takes, which
/// this build erases with the invariant itself.
pub struct Seen;

verus! {

impl InvariantPredicate<(), atom::SeenVal> for Seen {
    open spec fn inv(k: (), v: atom::SeenVal) -> bool {
        true
    }
}

} // verus!

/// `atom::load_usize_seen(a, ordering, inv)`'s value, with the erased
/// invariant.
pub fn load_usize_seen(a: &AtomicUsize, ordering: Ordering) -> usize {
    let inv: Tracked<AtomicInvariant<(), atom::SeenVal, Seen>> = Tracked::assume_new();
    atom::load_usize_seen(a, ordering, &inv).0
}
