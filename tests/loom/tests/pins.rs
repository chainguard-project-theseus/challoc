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
            "pub fn note_empty() {",
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
            "let deadline = super::decay(shared, now);",
            "let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };",
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
