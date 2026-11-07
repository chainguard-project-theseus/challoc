// The start of every harness (`run.sh` writes each case's `main.rs` as
// `#![no_main]`, this file, the case's constants, then its template).
// challoc's `src/sync.rs` and the `vspec` wrappers, compiled unchanged
// over `core`'s atomics, as the release build compiles them. The
// wrapper modules are the loom crate's `vspec.rs` (its `#[path]`s
// resolve from its own directory), so both crates compile one list.
#[path = "../../src/sync.rs"]
mod sync;
#[allow(dead_code)]
#[path = "../loom/src/vspec.rs"]
mod vspec;

// GenMC's Rust front end finds `main` behind `std::rt::lang_start` only
// in unoptimised IR, and the harnesses are compiled at `-Copt-level=1`
// (`run.sh`), so the entry point is a C `main`.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    harness();
    0
}

// GenMC reports a call of `__VERIFIER_assert_fail` (genmc-std defines it)
// as a safety violation carrying the message. `gassert!` is `assert!`
// over it: `core`'s panic machinery is precompiled, not in the IR GenMC
// interprets, so a harness never panics on purpose.
unsafe extern "C" {
    fn __VERIFIER_assert_fail(msg: *const u8, file: *const u8, line: i32);
}
macro_rules! gassert {
    ($c:expr, $m:literal) => {
        if !$c {
            // SAFETY: both strings are NUL-terminated literals, as
            // `__VERIFIER_assert_fail` reads them.
            unsafe {
                crate::__VERIFIER_assert_fail(
                    concat!($m, "\0").as_ptr(),
                    concat!(file!(), "\0").as_ptr(),
                    line!() as i32,
                )
            }
        }
    };
}

// A spin loop as GenMC models one, with its manual spin-assume markers
// (genmc-std's): `await_until!(cond)` stands for `while !cond {}` with a
// `cond` of loads, plus bookkeeping of the thread's own that a failing
// pass may leave (`init.rs`'s `state_load` records the value it read and
// checks it). GenMC keeps one pass and blocks the thread when it fails.
// A blocked execution is dropped, unless the run checks liveness
// (`+live` in tests/genmc/cases: `--check-liveness`, with GenMC's
// automatic spin-assume pass off, so these are the only spin loops):
// then a thread blocked with every load of its last pass reading its
// location's last write (in modification order), so that no later pass
// could read anything else, is a reported "Liveness violation".
unsafe extern "C" {
    fn __VERIFIER_loop_begin();
    fn __VERIFIER_spin_start();
    fn __VERIFIER_spin_end(cond: bool);
}
#[allow(unused_macros)]
macro_rules! await_until {
    ($c:expr) => {{
        // SAFETY: GenMC's markers, which touch no memory.
        unsafe {
            crate::__VERIFIER_loop_begin();
            crate::__VERIFIER_spin_start();
        }
        let ok = $c;
        // SAFETY: as above.
        unsafe { crate::__VERIFIER_spin_end(ok) };
    }};
}

// The futex, which GenMC has none of, for the harnesses on
// `background::SEQ` (background.rs, exit.rs, publish.rs), each with one
// waiter, as the code has (`FUTEX_WAKE` of one). `futex_wait`
// (`FUTEX_WAIT` with no timeout) blocks until a `Relaxed` load of the
// word reads another value than `val`, then returns; while `*spurious`
// is not 0 it instead returns at once, without the compare, and counts
// it down. `futex_wake` (`FUTEX_WAKE`) does nothing. What the model
// assumes of the kernel:
// - no lost wake-up given the value check: a waiter whose compare read
//   `val` is woken by a wake that follows a change of the word. The
//   model wakes it at the change itself, so it holds the code to that
//   pairing: `kick`'s `fetch_add` then `futex_wake` (tests/loom/tests/
//   pins.rs pins both lines), which review, not a test, finds to be the
//   only write of `SEQ`;
// - spurious wake-ups (`EINTR`, a stray `FUTEX_WAKE`) are allowed: a
//   case's `SPURIOUS` makes the thread's first `SPURIOUS` waits return
//   with no compare and no change of the word. GenMC explores every
//   interleaving, so a return at once stands for one at any later point
//   in that wait;
// - no ordering: the compare is `Relaxed`, so a wait adds no
//   happens-before edge (the kernel's hash-bucket lock would), and the
//   callers' own `SEQ` loads are all that order them.
// The word goes through `black_box` (an empty `asm` that takes its
// address), so rustc cannot treat a static nothing writes (`SEQ` in a
// control with no kick) as a constant and fold the wait's load away: a
// wait with no load is `__VERIFIER_spin_end(false)`, which GenMC drops
// with no report, even with `+live` (exit.rs has the case).
#[allow(dead_code)]
fn futex_wait(word: &std::sync::atomic::AtomicU32, val: u32, spurious: &mut u32) {
    if *spurious > 0 {
        *spurious -= 1;
        return;
    }
    let word = core::hint::black_box(word);
    await_until!(word.load(std::sync::atomic::Ordering::Relaxed) != val);
}
#[allow(dead_code)]
fn futex_wake(_word: &std::sync::atomic::AtomicU32) {}

// A thread created by a thread other than the main one, which GenMC 0.20
// cannot run under IMM and ARMv8 (the new thread's first read of its
// closure is `Attempt to access non-allocated memory!`, or an internal
// check, `it != allocInfoMap_.end()`). So the main thread spawns it at
// the start, as `created(f)`, and it waits for its creator's `create()`,
// a `Release` store where the creator would create it: the
// happens-before edge `pthread_create` gives, from the creator's steps
// before the creation to the new thread's first. One such thread per
// harness.
#[allow(dead_code)]
static CREATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[allow(dead_code)]
fn create() {
    CREATED.store(true, std::sync::atomic::Ordering::Release);
}
#[allow(dead_code)]
fn created(f: fn()) -> impl FnOnce() + Send + 'static {
    move || {
        await_until!(CREATED.load(std::sync::atomic::Ordering::Acquire));
        f()
    }
}
