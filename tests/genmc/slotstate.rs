// GenMC port of tests/loom/src/slotstate.rs: `double_free_scan`'s state
// byte (`heap::slotstate`). `free`'s one compare-and-exchange from `LIVE`
// (`slotstate::step` as `slotstate::free` calls it) runs through
// challoc's `vspec::byte::compare_exchange`, unchanged over `core`
// atomics, in two threads on one slot's byte, as two simultaneous frees
// of one live slot make it (one onto a cache, `CACHED`; the other
// straight to the slab, `FREE`).
//
// Checked: exactly one of the two frees finds the slot `LIVE` and moves
// it, and the byte ends as the passing free's target. `LOAD_STORE` is
// loom's control: the load-then-store form of the step lets both pass.

use crate::vspec::byte;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use vstd::prelude::Tracked;

// `slotstate::FREE`, `CACHED` and `LIVE`.
const FREE: u8 = 0;
const CACHED: u8 = 1;
const LIVE: u8 = 2;

static B: AtomicU8 = AtomicU8::new(LIVE);

// `slotstate::free`'s step: `true` when it moved the byte (the free
// passes), `false` when it found anything but `LIVE` (the abort).
fn free(b: &AtomicU8, to: u8) -> bool {
    if LOAD_STORE {
        if b.load(Ordering::Relaxed) == LIVE {
            b.store(to, Ordering::Relaxed);
            true
        } else {
            false
        }
    } else {
        byte::compare_exchange(
            b,
            LIVE,
            to,
            Ordering::Relaxed,
            Ordering::Relaxed,
            Tracked::assume_new(),
        )
        .is_ok()
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
    let v = B.load(Ordering::Relaxed);
    gassert!(
        v == if here { FREE } else { CACHED },
        "the byte is not the passing free's target"
    );
}
