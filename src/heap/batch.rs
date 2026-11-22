//! The release of `challoc_free_batch` (`api::batch`; proof of concept,
//! not verified): a run of slots of one class, sorted by address, released
//! straight to their slabs through each slab's owning arena.
//!
//! This is `Shared::release_routed`'s loop, kept separate so that function
//! keeps its single caller (`Heap::flush`) and its inlining there. The
//! ghost arguments are `Tracked::assume_new()`: nothing here is verified.
//! A verified version would take the batch's block tokens from the caller
//! (one `Live` per pointer, as `free` claims one) and hand their slot tokens
//! to `release_owned` in a `GhostSubset`, as the flush does.

use super::Shared;
use vstd::prelude::Tracked;

/// Release `run` (slots of `class`, sorted by address, each checked by
/// `Shared::classify_free`), the first owned by `arena`. `release_owned`
/// clears each bit with the flush's checks: a duplicate in the run hits the
/// scratch bit, an already-clear bit aborts, and the owner word is checked
/// against the slab descriptor.
pub fn release_run(shared: &Shared, arena: u16, class: usize, run: &[*mut u8]) {
    let mut done = 0;
    let mut a = arena;
    while done < run.len() {
        let (oa, ow) = shared.owner(a);
        done += oa.class(class).release_owned(
            &run[done..],
            Tracked::assume_new(),
            ow,
            Tracked::assume_new(),
        );
        if done == run.len() {
            return;
        }
        let next = shared.arena_of_slot(class, run[done]);
        if next == a {
            crate::sys::abort_fmt(format_args!(
                "challoc_free_batch: slab of class {class} changed arena {a}"
            ));
        }
        a = next;
    }
}
