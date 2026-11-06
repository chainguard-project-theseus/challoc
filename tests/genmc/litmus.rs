// Two shapes C11 allows and loom 0.7.2 cannot produce, over challoc's
// `vspec/count.rs` wrappers (`load`, `store`, run unchanged, `Relaxed`,
// as `arena.rs` loads the bound counts). Synthetic: no challoc protocol
// is copied.
//
// - `MODE` 0, load buffering: each thread loads one count, then stores
//   the other. `r1 == r2 == 1` needs each load to read a store that is
//   later in the other thread's program order. RC11 forbids it (no
//   `po ∪ rf` cycles); IMM (and ARMv8, POWER) allow it. loom executes
//   each load against stores already made, so it never sees it.
// - `MODE` 1, store history: one thread stores 1..=`N` to a count, then
//   sets a `Relaxed` flag; the other loads the flag, then the count.
//   With no happens-before, C11 lets the count load read any of the `N`
//   stores, 1 included. loom keeps the last 7 stores of an atomic
//   (`MAX_ATOMIC_HISTORY`), so once the flag store has run, 1 is gone.

use crate::vspec::count;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use vstd::prelude::Tracked;

static X: AtomicU32 = AtomicU32::new(0);
static Y: AtomicU32 = AtomicU32::new(0);

fn ld(c: &AtomicU32) -> u32 {
    count::load(c, Ordering::Relaxed, Tracked::assume_new())
}
fn st(c: &AtomicU32, v: u32) {
    count::store(c, v, Ordering::Relaxed, Tracked::assume_new())
}

fn harness() {
    if MODE == 0 {
        let a = thread::spawn(|| {
            let r = ld(&X);
            st(&Y, 1);
            r
        });
        let b = thread::spawn(|| {
            let r = ld(&Y);
            st(&X, 1);
            r
        });
        let (r1, r2) = (a.join().unwrap(), b.join().unwrap());
        gassert!(
            !(r1 == 1 && r2 == 1),
            "load buffering: both loads read the other thread's later store"
        );
    } else {
        let a = thread::spawn(|| {
            for v in 1..=N {
                st(&X, v);
            }
            st(&Y, 1);
        });
        let f = ld(&Y);
        let r = ld(&X);
        a.join().unwrap();
        gassert!(
            !(f == 1 && r == 1),
            "store history: after the flag, the count read its first store"
        );
    }
}
