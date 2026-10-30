//! The lines of challoc's source that the models copy must still be
//! there, whitespace-trimmed and in this order in each file. A change to
//! a copied step or ordering fails here until the model is revisited.

use std::path::Path;

/// `(file under the repository root, lines in order)`. A verified
/// function's signature line ends before its `requires` (no `{`).
const PINS: &[(&str, &[&str])] = &[
    // The free, `F` in tests/loom/src/background.rs.
    (
        "src/class/mod.rs",
        &[
            "if self.gate.note_first_empty(now) {",
            "purge::note_empty();",
        ],
    ),
    (
        "src/purge.rs",
        &[
            "self.oldest_empty.load(Ordering::Relaxed)",
            "if self.oldest_empty.load(Ordering::Relaxed) != 0 {",
            "self.oldest_empty.store(now, Ordering::Relaxed);",
            "let oldest = self.oldest_empty();",
            "if oldest == 0 {",
            "return None;",
            "pub fn note_empty()",
            "background::wake();",
        ],
    ),
    // `wake`, `kick`, `start`'s publication and kick, and the thread's
    // loop (`S` and `B` in tests/loom/src/background.rs).
    (
        "src/purge/background.rs",
        &[
            "PRESENT.load(Ordering::Relaxed)",
            "pub fn wake()",
            "fence(Ordering::SeqCst);",
            "if present() {",
            "kick();",
            "fn kick()",
            "SEQ.fetch_add(1, Ordering::SeqCst);",
            "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
            "PRESENT.store(g.present, Ordering::Relaxed);",
            "if present {",
            "fence(Ordering::SeqCst);",
            "kick();",
            "let seq = SEQ.load(Ordering::SeqCst);",
            "let deadline = super::decay(shared, now, Tracked(&mut ord));",
            "let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };",
        ],
    ),
    // `RawLock`: try_acquire, lock, lock_slow, release and the futex
    // calls (tests/loom/src/lock.rs).
    (
        "src/lock.rs",
        &[
            "const UNLOCKED: u32 = 0;",
            "const LOCKED: u32 = 1;",
            "const CONTENDED: u32 = 2;",
            "const SPINS: u32 = 100;",
            ".compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)",
            "if !self.try_acquire() {",
            "self.lock_slow();",
            "for _ in 0..SPINS {",
            "if self.state.load(Ordering::Relaxed) == UNLOCKED && self.try_acquire() {",
            "return;",
            "core::hint::spin_loop();",
            "let mut c = self.state.swap(CONTENDED, Ordering::Acquire);",
            "while c != UNLOCKED",
            "wait(&self.state, CONTENDED);",
            "c = self.state.swap(CONTENDED, Ordering::Acquire);",
            "if self.state.swap(UNLOCKED, Ordering::Release) == CONTENDED {",
            "wake(&self.state);",
            "let _ = unsafe { crate::sys::futex_wait(state.as_ptr(), val, None) };",
            "let _ = unsafe { crate::sys::futex_wake(state.as_ptr(), 1) };",
        ],
    ),
    // `SlabDesc`'s accessors of the lock-protected fields: init,
    // state_byte_held, dirty_held, set_state, arena_held, set_dirty_at
    // (tests/loom/src/slab.rs).
    (
        "src/slab.rs",
        &[
            "self.state.store(State::Fresh as u8, Ordering::Relaxed);",
            "self.shadow.store(State::Fresh as u8 + 1, Ordering::Relaxed);",
            "self.arena.store(arena, Ordering::Relaxed);",
            "self.live.store(0, Ordering::Relaxed);",
            "self.dirty_at.store(0, Ordering::Relaxed);",
            "fn state_byte_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u8)",
            "self.state.load(Ordering::Relaxed)",
            "pub fn dirty_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u64)",
            "self.dirty_at.load(Ordering::Relaxed)",
            "pub fn set_state(&self, s: State, Tracked(p): Tracked<&mut DescPerm>)",
            "self.state.store(s as u8, Ordering::Relaxed);",
            "self.shadow.store(s as u8 + 1, Ordering::Relaxed);",
            "pub fn arena_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: u16)",
            "self.arena.load(Ordering::Relaxed)",
            "pub fn set_dirty_at(&self, ns: u64, Tracked(p): Tracked<&mut DescPerm>)",
            "self.dirty_at.store(ns, Ordering::Relaxed);",
        ],
    ),
];

#[test]
fn copied_lines_in_place() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, lines) in PINS {
        let src = std::fs::read_to_string(root.join(file)).unwrap();
        let mut want = lines.iter().peekable();
        for line in src.lines().map(str::trim) {
            if want.peek() == Some(&&line) {
                want.next();
            }
        }
        if let Some(missing) = want.next() {
            panic!("{file}: got = no line {missing:?} in order, want = every pinned line");
        }
    }
}
