// GenMC port of tests/loom/src/count.rs: the arena registry's bound
// counts (`src/arena.rs`, `Arenas::bound`). challoc's `vspec/count.rs`
// wrappers (`fetch_add`, `fetch_sub`, `load`, `compare_exchange`) run
// unchanged over vstd's `PAtomicU32` (`SeqCst` on `core`'s `AtomicU32`);
// `Arenas::count_in`, `unbind`, `load_bound`, `assign` and
// `try_bind_idle` are copies, as in the loom model. The creation lock is
// genmc-std's `Mutex` (GenMC's own lock), not the `RawLock` copy: GenMC
// has no futex.
//
// `MODE` 0 is loom's `run`: the main thread's heap is homed on arena 0
// and counts a second heap in on arena 1 and out again, while two
// threads are each assigned an arena and unbind. `MODE` 1 is loom's
// `run_idle`: two threads race `try_bind_idle` on the idle arena 1,
// with (`RIVAL`) the main thread counting a heap in and out of it.
// `SPLIT` and `SPLIT_CAS` are loom's controls: the `fetch_add` or the
// `compare_exchange` split into the wrappers' `load` and `store`.
//
// The wrappers are verified (no seams); the pass cases check the
// callers' ticket protocol around them. The permissions are erased in
// this build (`Tracked::assume_new()`).

use crate::sync::patomic::PAtomicU32;
use crate::vspec::count;
use std::sync::Mutex;
use std::thread;
use vstd::prelude::Tracked;

const NARENAS: usize = 2;

// Whether each arena is built (`table[i]` not null), fixed for the model.
const BUILT: [bool; NARENAS] = [true, false];

struct Arenas {
    lock: Mutex<()>,
    bound: [PAtomicU32; NARENAS],
}

static A: Arenas = Arenas {
    lock: Mutex::new(()),
    bound: [count::new(0).0, count::new(0).0],
};

impl Arenas {
    // `Arenas::load_bound`.
    fn load_bound(&self, i: usize) -> u32 {
        count::load(&self.bound[i], Tracked::assume_new())
    }

    // `Arenas::count_in`: a fresh ticket for arena `i`.
    fn count_in(&self, i: usize) {
        if SPLIT {
            let n = count::load(&self.bound[i], Tracked::assume_new());
            count::store(&self.bound[i], n.wrapping_add(1), Tracked::assume_new());
        } else {
            count::fetch_add(&self.bound[i], 1, Tracked::assume_new());
        }
    }

    // `Arenas::unbind`: the ticket for arena `i` goes back.
    fn unbind(&self, i: usize) {
        let prev = count::fetch_sub(&self.bound[i], 1, Tracked::assume_new());
        gassert!(prev != 0, "unbinding a heap that was never bound (count 0)");
    }

    // `Arenas::assign` (more than one automatic arena).
    fn assign(&self) -> usize {
        let g = self.lock.lock().unwrap();
        let mut best = 0usize;
        let mut best_key = (u32::MAX, true);
        for (i, built) in BUILT.iter().enumerate() {
            let key = (self.load_bound(i), !built);
            if key < best_key {
                best = i;
                best_key = key;
                if key == (0, false) {
                    break;
                }
            }
        }
        self.count_in(best);
        drop(g);
        best
    }

    // `Arenas::try_bind_idle` and `bind_if_idle`: a fresh ticket for arena
    // `i` only if none is out (no lock, as `Heap::try_rehome` calls it).
    fn try_bind_idle(&self, i: usize) -> bool {
        if self.load_bound(i) != 0 {
            return false;
        }
        if SPLIT_CAS {
            if count::load(&self.bound[i], Tracked::assume_new()) != 0 {
                return false;
            }
            count::store(&self.bound[i], 1, Tracked::assume_new());
            true
        } else {
            count::compare_exchange(&self.bound[i], 0, 1, Tracked::assume_new()).is_ok()
        }
    }
}

fn holder() {
    let a: &'static Arenas = &A;
    let i = a.assign();
    gassert!(a.load_bound(i) >= 1, "a holder read its arena's count as 0");
    a.unbind(i);
}

fn run() {
    let a: &'static Arenas = &A;
    a.count_in(0);
    let x = thread::spawn(holder);
    let y = thread::spawn(holder);
    a.count_in(1);
    gassert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
    a.unbind(1);
    gassert!(a.load_bound(0) >= 1, "a holder read its arena's count as 0");
    x.join().unwrap();
    y.join().unwrap();
    gassert!(a.load_bound(0) == 1, "arena 0's count is not its tickets");
    gassert!(a.load_bound(1) == 0, "arena 1's count is not its tickets");
}

fn racer() -> bool {
    let a: &'static Arenas = &A;
    let won = a.try_bind_idle(1);
    if won {
        gassert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
    }
    won
}

fn run_idle() {
    let a: &'static Arenas = &A;
    a.count_in(0);
    let x = thread::spawn(racer);
    let y = thread::spawn(racer);
    if RIVAL {
        a.count_in(1);
        gassert!(a.load_bound(1) >= 1, "a holder read its arena's count as 0");
        a.unbind(1);
    }
    let won = x.join().unwrap() as u32 + y.join().unwrap() as u32;
    gassert!(
        won <= 1,
        "two heaps bound to one idle arena (count 0 to 1 twice)"
    );
    if !RIVAL {
        gassert!(
            won == 1,
            "no heap bound the idle arena (its count compare-and-swap failed)"
        );
    }
    gassert!(a.load_bound(0) == 1, "arena 0's count is not its tickets");
    gassert!(a.load_bound(1) == won, "arena 1's count is not its tickets");
}

fn harness() {
    if MODE == 0 { run() } else { run_idle() }
}
