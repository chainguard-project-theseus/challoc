// GenMC port of tests/loom/src/flags.rs (P5's dispatch byte): challoc's
// `vspec/flags.rs` wrappers (`new`, `fetch_or`, `load`) run unchanged
// over `core` atomics; the callers (`put_flags` + `READY` publish,
// `set_multi_arena`, `free_flags`) are copies, as in the loom model.
// The storage `init` writes is three plain bytes (GenMC rejects
// mixed-size accesses, so no `Option<(u8, RouteByte)>` as in loom).

use crate::vspec::flags::{ROUTE, RouteByte, fetch_or, load, new};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

const UNINIT: u32 = 0;
const READY: u32 = 2;
const PUSH: u8 = 0b111;

struct Model {
    state: AtomicU32,
    /// Whether `init` wrote the two fields (the loom model's `Some`).
    made: UnsafeCell<u8>,
    push: UnsafeCell<u8>,
    byte: UnsafeCell<MaybeUninit<RouteByte>>,
}
// SAFETY: `made`, `push` and `byte` are written by `init` before `READY`;
// after it `made` and `push` are only read and `byte` is only accessed
// through its atomic, which is what is checked.
unsafe impl Sync for Model {}

impl Model {
    /// `put_flags` (plain writes into unpublished storage), then the
    /// `READY` store (`api::state_publish`).
    fn init(&self, push: u8) {
        // SAFETY: the storage is not published yet.
        unsafe {
            *self.push.get() = push;
            (*self.byte.get()).write(new(push));
            *self.made.get() = 1;
        }
        self.state.store(READY, PUBLISH);
    }
    fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY
    }
    fn fields(&self) -> (u8, &RouteByte) {
        // SAFETY: called after `READY` (or after every thread is joined),
        // when no plain write touches the storage (`byte` changes only
        // through its atomic); `made` checks that `init`'s writes,
        // `byte`'s initialisation among them, are seen.
        unsafe {
            gassert!(
                core::ptr::read_volatile(self.made.get()) == 1,
                "READY before init's writes"
            );
            (
                core::ptr::read_volatile(self.push.get()),
                (*self.byte.get()).assume_init_ref(),
            )
        }
    }
    /// `Arenas::build`'s `Shared::set_multi_arena`.
    fn build(&self, bits: u8) -> Option<(u8, u8)> {
        if !self.ready() {
            return None;
        }
        let (push, b) = self.fields();
        Some((fetch_or(b, bits, Ordering::Relaxed), push))
    }
    /// The `free` fast path's `Shared::free_flags`, twice.
    fn free(&self) -> Option<(u8, u8, u8)> {
        if !self.ready() {
            return None;
        }
        let (push, b) = self.fields();
        let r0 = load(b, Ordering::Relaxed);
        let r1 = load(b, Ordering::Relaxed);
        Some((r0, r1, push))
    }
}

fn check_build(o: Option<(u8, u8)>) -> bool {
    match o {
        Some((prev, push)) => {
            gassert!(
                prev & !ROUTE == push & !ROUTE,
                "fetch_or read a byte init did not make"
            );
            true
        }
        None => false,
    }
}

/// A static, as `api::SHARED` is (GenMC also rejects some heap accesses
/// in racy executions).
static M: Model = Model {
    state: AtomicU32::new(UNINIT),
    made: UnsafeCell::new(0),
    push: UnsafeCell::new(0),
    byte: UnsafeCell::new(MaybeUninit::uninit()),
};

fn harness() {
    let m: &'static Model = &M;
    let init = thread::spawn(move || m.init(PUSH));
    let b1 = thread::spawn(move || m.build(BITS));
    let b2 = if BUILDERS > 1 {
        Some(thread::spawn(move || m.build(BITS)))
    } else {
        None
    };
    let freed = m.free();
    let mut any_build = check_build(b1.join().unwrap());
    if let Some(b2) = b2 {
        any_build |= check_build(b2.join().unwrap());
    }
    if let Some((r0, r1, push)) = freed {
        gassert!(
            r0 & !ROUTE == push & !ROUTE,
            "load read a byte init did not make"
        );
        gassert!(
            r1 & !ROUTE == push & !ROUTE,
            "load read a byte init did not make"
        );
        gassert!(r0 & ROUTE == 0 || r1 & ROUTE != 0, "a load lost ROUTE");
    }
    init.join().unwrap();
    let (push, b) = m.fields();
    let last = load(b, Ordering::Relaxed);
    let want = if any_build { push | ROUTE } else { push };
    gassert!(
        last == want,
        "the joined byte: one init did not make, or ROUTE wrong"
    );
}
