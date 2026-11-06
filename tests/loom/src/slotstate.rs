//! `double_free_scan`'s state byte (`heap::slotstate`): `free`'s one
//! compare-and-exchange from `LIVE` (`slotstate::step` as `slotstate::free`
//! calls it, through the real `vspec::byte::compare_exchange`), run by two
//! threads on one slot's byte, as two simultaneous frees of one live slot
//! make it (one onto a cache, `CACHED`; the other straight to the slab,
//! `FREE`; any pair of targets is the same race).
//!
//! Checked: in every interleaving exactly one of the two frees finds the
//! slot `LIVE` and moves it; the other sees what the first stored and
//! aborts by name. Control: the load-then-store form of the same step
//! lets both pass in some interleaving (the window the
//! compare-and-exchange closes), so it fails the assertion. Left out: the
//! address arithmetic and the watermark compare before the byte (no
//! atomics but the watermark's `Acquire` load, `watermark.rs`'s), and the
//! other transitions, plain stores by the thread that holds the slot.

use crate::vspec::byte;
use crate::{leak, model};
use loom::sync::atomic::{AtomicU8, Ordering};
use loom::thread;
use vstd::prelude::Tracked;

/// `slotstate::FREE`.
const FREE: u8 = 0;
/// `slotstate::CACHED`.
const CACHED: u8 = 1;
/// `slotstate::LIVE`.
const LIVE: u8 = 2;

/// `slotstate::free`'s step: `true` when it moved the byte (the free
/// passes), `false` when it found anything but `LIVE` (the abort).
fn free(b: &AtomicU8, to: u8) -> bool {
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

/// The control: a load, then a store.
fn free_load_store(b: &AtomicU8, to: u8) -> bool {
    if b.load(Ordering::Relaxed) == LIVE {
        b.store(to, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn two_frees(step: fn(&AtomicU8, u8) -> bool) {
    model(move || {
        let b = leak(AtomicU8::new(LIVE));
        let t = thread::spawn(move || step(b, CACHED));
        let here = step(b, FREE);
        let there = t.join().unwrap();
        assert!(
            here != there,
            "two simultaneous frees of one slot: both passed or both aborted"
        );
        let v = b.load(Ordering::Relaxed);
        assert!(
            v == if here { FREE } else { CACHED },
            "the byte is the passing free's target"
        );
    });
}

#[test]
fn loom_slotstate_one_of_two_frees_passes() {
    two_frees(free);
}

#[test]
#[should_panic(expected = "both passed")]
fn loom_slotstate_load_then_store_lets_both_pass() {
    two_frees(free_load_store);
}
