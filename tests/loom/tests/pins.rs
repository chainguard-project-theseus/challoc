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
    // `AtomicBitmap`: init's whole-word padding, claim_word, claim_from
    // and release (tests/loom/src/bitmap.rs).
    (
        "src/bitmap.rs",
        &[
            "let pad = if nbits >= lo + 64 {",
            "} else if nbits <= lo {",
            "!0u64 << (nbits - lo)",
            "crate::vspec::word::store(w, pad, Ordering::Relaxed, Tracked(&mut perm));",
            "let cur = w.load(Ordering::Relaxed);",
            "let want = lowest_n_bits(!cur & self.mask, n);",
            "if want == 0 {",
            "return 0;",
            "prev = crate::vspec::word::fetch_or(w, want, Ordering::Acquire, Tracked(&mut perm));",
            "(want & !prev) >> self.shift",
            "let got = self.claim_word(i, n, Tracked(&mut *toks), Tracked(inited));",
            "if got != 0 {",
            "return Some((i, got));",
            "i += 1;",
            "if i == len {",
            "i = 0;",
            "let m = mask << self.shift;",
            "prev = crate::vspec::word::fetch_and(w, !m, Ordering::Release, Tracked(&mut perm));",
        ],
    ),
    // The class steps around the bitmap: the claim loop's empty passes,
    // take's reservation and dec_live (tests/loom/src/bitmap.rs).
    (
        "src/class/mod.rs",
        &[
            "let Some((w, bits)) = bm.claim_from(word, want - k, Tracked(&mut wt), Tracked(&cm)) else {",
            "empty_passes += 1;",
            "if empty_passes == MAX_EMPTY_PASSES {",
            "fn take(",
            "let live = desc.live();",
            "let avail = self.class().nslots() - live;",
            "let n = avail.min(*remaining);",
            "desc.add_live(n);",
            "if self.desc(slab).sub_live(n) == n {",
        ],
    ),
    // `SlabDesc`'s `live`: live, add_live, sub_live
    // (tests/loom/src/bitmap.rs).
    (
        "src/slab.rs",
        &[
            "self.live.load(Ordering::Acquire) as usize",
            "self.live.fetch_add(n as u32, Ordering::AcqRel) as usize",
            "let prev = self.live.fetch_sub(n as u32, Ordering::AcqRel) as usize;",
        ],
    ),
    // The free path's table reads: load_mirror, load_owner, store_owner,
    // committed_slabs, watermark, is_handed_out, the bit word, owner_at,
    // check_located, check_live, take_group's hand-out tail and owner_from
    // (tests/loom/src/free.rs).
    (
        "src/class/table.rs",
        &[
            "r = load_usize(a, Ordering::Relaxed, Tracked(&v.0));",
            "r = unsafe { load_u16_at(w, Ordering::Relaxed, Tracked(&v.0)) };",
            "unsafe { (*w).store(arena, Ordering::Relaxed) }",
            "load_usize_seen(&self.committed_slabs, Ordering::Acquire, &self.committed_inv).0",
            "r = load_usize(&self.committed_slabs, order, Tracked(&v.0));",
            "slab < self.watermark(Ordering::Relaxed)",
            "x = crate::vspec::word::load(w, Ordering::Relaxed, Tracked(&perm));",
            "unsafe { load_owner(self, word, Ghost(slab), seen) }",
            "self.check_live(slab, slot, p, held);",
            "unsafe { self.owner_at(slab, Tracked(none)) }",
            "if !self.is_handed_out(slab) {",
            "never_committed(self.class as usize, slab);",
            "if !unsafe { self.is_set_unchecked(slab, slot, Tracked(held)) } {",
            "let Tracked(m) = self.bits_unchecked(slab).init(cls.nslots(), Ghost(slab as int), Tracked(fs));",
            "unsafe { store_owner(owner_word, arena, Ghost(*self), Ghost(s >> self.lg_group), Tracked(&mut v.0), Tracked(om)) };",
            "store_usize(&self.committed_slabs, end, Ordering::Release, Tracked(&mut v.0));",
            "if !gi.mirror.is_null() {",
            "unsafe { store_usize_at(gi.mirror, end, Ordering::Release, Tracked(&mut v.0)) };",
            "unsafe { load_owner(self, word, Ghost(slab), Tracked(None)) }",
        ],
    ),
    // `ClassLine::set_slab` and `owner`, `Shared::home_owns` and
    // `set_slot` (tests/loom/src/free.rs).
    (
        "src/heap/shared.rs",
        &[
            "let committed = crate::class::load_mirror(&self.committed, self.committed_inv);",
            "if slab >= committed {",
            "let word = unsafe { table.bit_word(bits_base, index, Ghost(slab), Ghost(slot as usize), Ghost(bit)) };",
            "if (word >> (bit % 64)) & 1 == 0 {",
            "unsafe { table.owner_from(self.owners_base, self.lg_group, slab) }",
            "let home = h.arena_volatile();",
            "crate::vspec::slice::array_get_unchecked(&self.lines, class).owner(",
            ") == home",
            "let Some(slab) = line.set_slab(p, table) else {",
        ],
    ),
    // `free_fast_slot` and `free_fast_tail` (tests/loom/src/free.rs).
    (
        "src/heap/paths.rs",
        &[
            "let Some((class, slab)) = shared.set_slot(p) else {",
            "let flags = shared.free_flags();",
            "0 => unsafe { heap_from_slot(t, tp, false, Tracked::assume_new()) }.push_fast(",
            "FREE_ROUTE => {",
            "shared.home_owns(class, slab, h) && h.push_fast(class, p, 0, Tracked::assume_new())",
            "(flags & FREE_ROUTE == 0 || shared.home_owns(class, slab, h))",
        ],
    ),
    // `mirror_watermark`, take_group's lock, bound, check_zero comparison,
    // `next_slab = end`, the watermark's store and its copy's, and
    // next_slab_locked's read of `next_slab` (tests/loom/src/watermark.rs).
    (
        "src/class/table.rs",
        &[
            "let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));",
            "let wm = gi.next_slab;",
            "unsafe { store_usize_at(mirror, wm, Ordering::Release, Tracked(&mut v.0)) };",
            "gi.mirror = mirror;",
            "let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));",
            "let s = self.next_slab_locked(&*g, &*sets);",
            "if s >= self.nslabs {",
            "return None;",
            "if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {",
            "unsafe { store_owner(owner_word, arena, Ghost(*self), Ghost(s >> self.lg_group), Tracked(&mut v.0), Tracked(om)) };",
            "gi.next_slab = end;",
            "store_usize(&self.committed_slabs, end, Ordering::Release, Tracked(&mut v.0));",
            "if !gi.mirror.is_null() {",
            "unsafe { store_usize_at(gi.mirror, end, Ordering::Release, Tracked(&mut v.0)) };",
            "t.next_slab",
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
