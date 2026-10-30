//! The lock-protected fields of a slab descriptor (`src/slab.rs`,
//! `DescPerm`), a copy. The accessors below are `SlabDesc`'s bodies line
//! for line: `init`'s five `Relaxed` stores, the `Relaxed` loads of
//! `state_byte_held`, `dirty_held` and `arena_held`, `set_state`'s two
//! stores and `set_dirty_at`'s one. The locks are the copy of `RawLock`
//! (`lock.rs`), and [`Perm`] is the ghost `DescPerm` made concrete: the
//! three values the last write through the permission stored, in a loom
//! `UnsafeCell` under the lock whose value holds the permission, as the
//! class table's commit lock (`TableInner::dperms`) holds it before the
//! slab is handed out and the taking arena's class lock
//! (`class::Inner::dperms`) after.
//!
//! The protocol modelled is the one `ClassState`'s descriptor accessors
//! and `SlabTable::take_group` follow (`src/class/fresh.rs`,
//! `src/class/table.rs`): with the arena's class lock held, `take_group`
//! takes the commit lock, moves the permission out of the table's value,
//! runs `init` through it and releases the commit lock, and the caller
//! keeps the permission in the class lock's value; still under the class
//! lock the slab goes `Active` (`set_state`). Another thread, under the
//! class lock, reads the three fields through the permission when the
//! value has it and writes `Empty` and a stamp (`set_state`,
//! `set_dirty_at`).
//!
//! Checked: every read through the permission returns the value the
//! permission records, the last write through it (a stale read fails an
//! assertion), and no permission cell is touched without its lock (a
//! reported race). The control reads the state outside the class lock,
//! having learnt that the permission was handed over from a `Relaxed`
//! flag, and sees a stale value: the locks' edges, not the descriptor's
//! orderings, are what the reads rely on.
//!
//! Left out: `live` (an `AcqRel` read-modify-write count, `bitmap.rs`'s),
//! which `init` also stores before the slab is published; `shadow`, which
//! no permission records; the class code between the steps, which is
//! verified (sequentially consistent) and touches the descriptor only
//! through these accessors.

use crate::lock::RawLock;
use crate::{leak, model};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use loom::thread;

/// The states the model writes (`slab::State`'s values).
#[derive(Clone, Copy)]
#[repr(u8)]
enum State {
    Fresh = 0,
    Active = 1,
    Empty = 2,
}

/// `slab::SlabDesc`'s fields.
struct SlabDesc {
    state: AtomicU8,
    shadow: AtomicU8,
    arena: AtomicU16,
    live: AtomicU32,
    dirty_at: AtomicU64,
}

impl SlabDesc {
    /// Zero, as the kernel hands the metadata pages out.
    fn zero() -> SlabDesc {
        SlabDesc {
            state: AtomicU8::new(0),
            shadow: AtomicU8::new(0),
            arena: AtomicU16::new(0),
            live: AtomicU32::new(0),
            dirty_at: AtomicU64::new(0),
        }
    }

    /// `SlabDesc::init`.
    fn init(&self, arena: u16) {
        self.state.store(State::Fresh as u8, Ordering::Relaxed);
        self.shadow.store(State::Fresh as u8 + 1, Ordering::Relaxed);
        self.arena.store(arena, Ordering::Relaxed);
        self.live.store(0, Ordering::Relaxed);
        self.dirty_at.store(0, Ordering::Relaxed);
    }

    /// `SlabDesc::state_byte_held`.
    fn state_byte_held(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }

    /// `SlabDesc::dirty_held`.
    fn dirty_held(&self) -> u64 {
        self.dirty_at.load(Ordering::Relaxed)
    }

    /// `SlabDesc::set_state`.
    fn set_state(&self, s: State) {
        self.state.store(s as u8, Ordering::Relaxed);
        self.shadow.store(s as u8 + 1, Ordering::Relaxed);
    }

    /// `SlabDesc::arena_held`.
    fn arena_held(&self) -> u16 {
        self.arena.load(Ordering::Relaxed)
    }

    /// `SlabDesc::set_dirty_at`.
    fn set_dirty_at(&self, ns: u64) {
        self.dirty_at.store(ns, Ordering::Relaxed);
    }
}

/// `DescPerm`'s three values.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Perm {
    state: u8,
    dirty: u64,
    arena: u16,
}

/// The arena `take_group` hands the slab to.
const ARENA: u16 = 3;
/// The stamp the second holder writes.
const STAMP: u64 = 9;

struct World {
    desc: SlabDesc,
    /// The class table's commit lock and its value's permission.
    commit: RawLock,
    table_perm: UnsafeCell<Option<Perm>>,
    /// The arena's class lock and its value's permission.
    class: RawLock,
    class_perm: UnsafeCell<Option<Perm>>,
    /// Set, `Relaxed`, once the slab is `Active`: the control's only
    /// news of the hand-over.
    handed: AtomicBool,
}

// SAFETY: the cells are touched only under their locks, which is what
// the model checks.
unsafe impl Sync for World {}

/// The three reads through the permission `p`.
fn check_held(d: &SlabDesc, p: Perm) {
    assert_eq!(d.state_byte_held(), p.state, "stale state");
    assert_eq!(d.dirty_held(), p.dirty, "stale dirty_at");
    assert_eq!(d.arena_held(), p.arena, "stale arena");
}

fn world() -> &'static World {
    leak(World {
        desc: SlabDesc::zero(),
        commit: RawLock::new(),
        table_perm: UnsafeCell::new(Some(Perm {
            state: 0,
            dirty: 0,
            arena: 0,
        })),
        class: RawLock::new(),
        class_perm: UnsafeCell::new(None),
        handed: AtomicBool::new(false),
    })
}

/// `take_group` and the first `set_state`, under the class lock.
fn take_and_activate(w: &World) {
    w.class.lock();
    w.commit.lock();
    // SAFETY: under the commit lock.
    w.table_perm
        .with_mut(|c| unsafe { (*c).take() })
        .expect("the table holds the permission");
    w.desc.init(ARENA);
    let mut p = Perm {
        state: State::Fresh as u8,
        dirty: 0,
        arena: ARENA,
    };
    w.commit.unlock();
    w.desc.set_state(State::Active);
    p.state = State::Active as u8;
    // SAFETY: under the class lock.
    w.class_perm.with_mut(|c| unsafe { *c = Some(p) });
    w.handed.store(true, Ordering::Relaxed);
    w.class.unlock();
}

/// A later holder of the class lock: reads through the permission when
/// the lock's value has it, then empties the slab.
fn read_and_empty(w: &World) {
    w.class.lock();
    // SAFETY: under the class lock.
    if let Some(mut p) = w.class_perm.with(|c| unsafe { *c }) {
        check_held(&w.desc, p);
        w.desc.set_state(State::Empty);
        p.state = State::Empty as u8;
        w.desc.set_dirty_at(STAMP);
        p.dirty = STAMP;
        // SAFETY: under the class lock.
        w.class_perm.with_mut(|c| unsafe { *c = Some(p) });
    }
    w.class.unlock();
}

/// The hand-over and a second holder, then a read ordered after both by
/// the joins.
#[test]
fn loom_slab_desc_reads_through_the_lock() {
    model(|| {
        let w = world();
        let t = thread::spawn(move || take_and_activate(w));
        let u = thread::spawn(move || read_and_empty(w));
        t.join().unwrap();
        u.join().unwrap();
        // SAFETY: both threads are joined.
        let p = w.class_perm.with(|c| unsafe { *c }).unwrap();
        check_held(&w.desc, p);
    });
}

/// Control: a reader outside the class lock, told of the hand-over only
/// by a `Relaxed` flag, can read the state as it was before `set_state`.
#[test]
#[should_panic(expected = "stale state")]
fn loom_slab_desc_unlocked_read_is_stale() {
    model(|| {
        let w = world();
        let t = thread::spawn(move || take_and_activate(w));
        if w.handed.load(Ordering::Relaxed) {
            assert_eq!(w.desc.state_byte_held(), State::Active as u8, "stale state");
        }
        t.join().unwrap();
    });
}
