//! The `free` fast path's dispatch byte (`heap::Shared::free_flags`): a
//! copy of `vspec/flags.rs`'s `RouteByte` (`new`, `fetch_or`, `load`,
//! each the bare `core` operation on its `AtomicU8` with the caller's
//! ordering), driven by copies of the steps that call them (the `Shared`
//! they live in is a static whose storage starts uninitialised):
//!
//! - [`Model::init`] is `init`'s `Shared::put_flags`: plain writes of
//!   `push_flags` and of the byte `flags::new(push_flags)` made, into the
//!   unpublished storage (here one loom `UnsafeCell`), then `init_slow`'s
//!   `READY` store (`api::state_publish`, `Release` as shipped);
//! - [`Model::build`] is `Arenas::build`'s `Shared::set_multi_arena`,
//!   which runs with the published `Shared` (`api::shared`'s `Acquire`
//!   load of `STATE` saw `READY` earlier in the thread):
//!   `flags::fetch_or(ROUTE, Relaxed)`;
//! - [`Model::free`] is the `free` fast path's `Shared::free_flags`,
//!   reached through `api::shared_base` once the thread's heap proves
//!   init complete (the heap is created after `api::shared`'s `Acquire`
//!   load); the model stands in for that edge with
//!   `api::shared_if_ready`'s `Acquire` load, then `flags::load(Relaxed)`,
//!   twice.
//!
//! Checked: every value a load or the `fetch_or` returns is `push_flags`
//! with or without `ROUTE` (`flags::fixed`, which `Shared::route_fixed`
//! ties to `push_flags`), with no race on the storage `init` wrote (a
//! reader not ordered after `init`'s writes is a reported causality
//! violation); a thread's loads never lose `ROUTE` once they saw it
//! (read-read coherence); and once every thread is joined the byte is
//! `push_flags`, plus `ROUTE` if a builder ran. Controls: a `Relaxed`
//! `READY` store is a reported race, and a builder setting a bit outside
//! `ROUTE` (which `fetch_or`'s `requires` rules out) fails the assertions.
//! Left out: what `init` writes besides the two fields (it is verified,
//! and it does not touch the byte), the rest of `build` (its arena and
//! entry are `narenas.rs`'s), and the slow paths that read `push_flags`
//! (a plain field, never written after `init`).

use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use loom::thread;

const UNINIT: u32 = 0;
const READY: u32 = 2;

/// `flags::ROUTE` (`heap::FREE_ROUTE`).
const ROUTE: u8 = 8;

/// `push_flags` in the model: `PUSH_STATE | PUSH_ZERO | PUSH_PAC`, every
/// bit `init` may set.
const PUSH: u8 = 0b111;

/// `vspec::flags::RouteByte`.
struct RouteByte {
    b: AtomicU8,
}

/// `flags::new`.
fn new(v: u8) -> RouteByte {
    RouteByte {
        b: AtomicU8::new(v),
    }
}

/// `flags::load`.
fn load(b: &RouteByte, ordering: Ordering) -> u8 {
    b.b.load(ordering)
}

/// `flags::fetch_or`.
fn fetch_or(b: &RouteByte, v: u8, ordering: Ordering) -> u8 {
    b.b.fetch_or(v, ordering)
}

struct Model {
    /// `api::STATE`.
    state: AtomicU32,
    /// `Shared`'s `push_flags` and `free_flags`, written by `init` before
    /// `READY` (`None`: the storage before `init`).
    shared: UnsafeCell<Option<(u8, RouteByte)>>,
    /// The ordering of the `READY` store (`Release` as shipped).
    publish: Ordering,
}

// SAFETY: `shared` is written by `init` before `READY` and only read
// after, which is what is checked.
unsafe impl Sync for Model {}

impl Model {
    fn new(publish: Ordering) -> Model {
        Model {
            state: AtomicU32::new(UNINIT),
            shared: UnsafeCell::new(None),
            publish,
        }
    }

    /// `put_flags`, then the `READY` store.
    fn init(&self, push: u8) {
        // SAFETY: the storage is not published yet.
        self.shared
            .with_mut(|p| unsafe { *p = Some((push, new(push))) });
        self.state.store(READY, self.publish);
    }

    /// `api::shared_if_ready` (and `api::shared` once ready): the
    /// `Acquire` load of `STATE`.
    fn ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY
    }

    /// `set_multi_arena` on a ready `Shared`, setting `bits` (`ROUTE` as
    /// shipped): the previous byte, and `push_flags`.
    fn build(&self, bits: u8) -> Option<(u8, u8)> {
        if !self.ready() {
            return None;
        }
        // SAFETY: after `READY`, nothing writes the storage.
        Some(self.shared.with(|p| {
            let (push, b) = unsafe { (*p).as_ref().expect("READY before init's writes") };
            (fetch_or(b, bits, Ordering::Relaxed), *push)
        }))
    }

    /// `free_flags` on a ready `Shared`, twice: the two bytes, and
    /// `push_flags`.
    fn free(&self) -> Option<(u8, u8, u8)> {
        if !self.ready() {
            return None;
        }
        // SAFETY: as in `build`.
        Some(self.shared.with(|p| {
            let (push, b) = unsafe { (*p).as_ref().expect("READY before init's writes") };
            let r0 = load(b, Ordering::Relaxed);
            let r1 = load(b, Ordering::Relaxed);
            (r0, r1, *push)
        }))
    }
}

/// Run `init` and `builders` builders setting `bits`, with the main
/// thread freeing.
fn run(publish: Ordering, builders: usize, bits: u8) {
    model(move || {
        let m = leak(Model::new(publish));
        let init = thread::spawn(move || m.init(PUSH));
        let mut built = Vec::new();
        for _ in 0..builders {
            built.push(thread::spawn(move || m.build(bits)));
        }
        let freed = m.free();
        let mut any_build = false;
        for t in built {
            if let Some((prev, push)) = t.join().unwrap() {
                assert_eq!(
                    prev & !ROUTE,
                    push & !ROUTE,
                    "fetch_or read a byte init did not make"
                );
                any_build = true;
            }
        }
        if let Some((r0, r1, push)) = freed {
            assert_eq!(
                r0 & !ROUTE,
                push & !ROUTE,
                "load read a byte init did not make"
            );
            assert_eq!(
                r1 & !ROUTE,
                push & !ROUTE,
                "load read a byte init did not make"
            );
            assert!(r0 & ROUTE == 0 || r1 & ROUTE != 0, "a load lost ROUTE");
        }
        init.join().unwrap();
        m.shared.with(|p| {
            // SAFETY: every thread is joined.
            let (push, b) = unsafe { (*p).as_ref().unwrap() };
            let last = load(b, Ordering::Relaxed);
            let want = if any_build { *push | ROUTE } else { *push };
            assert_eq!(
                last, want,
                "the joined byte: one init did not make, or ROUTE wrong"
            );
        });
    });
}

/// As shipped: one builder, the main thread freeing.
///
/// Covers copies of `vspec::flags::new`, `vspec::flags::load` and
/// `vspec::flags::fetch_or`.
#[test]
fn loom_flags_route_byte_keeps_push_flags() {
    run(Ordering::Release, 1, ROUTE);
}

/// Two builders (a third arena built while the second's `fetch_or` is in
/// flight), the main thread freeing.
///
/// Covers copies of `vspec::flags::new`, `vspec::flags::load` and
/// `vspec::flags::fetch_or`.
#[test]
fn loom_flags_two_builders() {
    run(Ordering::Release, 2, ROUTE);
}

/// Control: a `Relaxed` `READY` store does not order `init`'s writes
/// before a reader that sees it.
#[test]
#[should_panic(expected = "Causality violation")]
fn loom_flags_relaxed_publish_is_a_race() {
    run(Ordering::Relaxed, 1, ROUTE);
}

/// Control: a builder that sets a bit outside `ROUTE` (which
/// `fetch_or`'s `requires` rules out) makes the byte lose `push_flags`,
/// which the assertions catch.
#[test]
#[should_panic(expected = "did not make")]
fn loom_flags_bit_outside_route_is_caught() {
    run(Ordering::Release, 1, ROUTE | 0x10);
}
