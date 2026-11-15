// GenMC port of tests/loom/src/slotstate.rs: `double_free_scan`'s state
// byte (`heap::slotstate`). `free`'s one compare-and-exchange from `LIVE`
// (`slotstate::step` as `slotstate::free` calls it) runs through
// challoc's `vspec::byte::compare_exchange`, unchanged over vstd's
// `PAtomicU8` (`SeqCst` on `core`'s `AtomicU8`), in two threads on one
// slot's byte, as two simultaneous frees of one live slot make it (one
// onto a cache, `CACHED`; the other straight to the slab, `FREE`).
//
// Checked: exactly one of the two frees finds the slot `LIVE` and moves
// it, and the byte ends as the passing free's target. `LOAD_STORE` is
// loom's control: the load-then-store form of the step, through the
// wrappers' `load` and `store`, lets both pass.
//
// The wrappers are verified (no seams); the permission is erased
// (`Tracked::assume_new()`), and the byte is a slot's state byte (the
// call in `slotstate::step`), not the `Deferred` phase the loom models
// check.

use crate::sync::patomic::PAtomicU8;
use crate::vspec::byte;
use std::thread;
use vstd::prelude::Tracked;

// `slotstate::FREE`, `CACHED` and `LIVE`.
const FREE: u8 = 0;
const CACHED: u8 = 1;
const LIVE: u8 = 2;

static B: PAtomicU8 = byte::new(LIVE).0;

// `slotstate::free`'s step: `true` when it moved the byte (the free
// passes), `false` when it found anything but `LIVE` (the abort).
fn free(b: &PAtomicU8, to: u8) -> bool {
    if LOAD_STORE {
        if byte::load(b, Tracked::assume_new()) == LIVE {
            byte::store(b, to, Tracked::assume_new());
            true
        } else {
            false
        }
    } else {
        byte::compare_exchange(b, LIVE, to, Tracked::assume_new()).is_ok()
    }
}

fn harness() {
    let t = thread::spawn(|| free(&B, CACHED));
    let here = free(&B, FREE);
    let there = t.join().unwrap();
    gassert!(
        here != there,
        "two simultaneous frees of one slot: both passed or both aborted"
    );
    let v = byte::load(&B, Tracked::assume_new());
    gassert!(
        v == if here { FREE } else { CACHED },
        "the byte is not the passing free's target"
    );
}
