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
            "let got = self.claim_word(i, n, Tracked(&mut *toks), Tracked(inited), Tracked(seen));",
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
            "let Some((w, bits)) = bm.claim_from(word, want - k, Tracked(&mut wt), Tracked(&cm), Tracked(&cs)) else {",
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
            "unsafe { self.owner_at(slab, Tracked(seen)) }",
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
            "0 => unsafe { heap_from_slot(t, tp, false, Tracked(&tls.cell), Tracked(&mut tls.loan)) }.push_fast(",
            "FREE_ROUTE => {",
            "shared.home_owns(class, slab, h) && h.push_fast(class, p, 0, Tracked(&mut tk))",
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
    // `Arenas`: unbind, the walk below the count (get_existing), assign,
    // load_bound, count_in, count, try_bind_idle, bind_if_idle, build's
    // set_multi_arena and entry store, create (tests/loom/src/count.rs,
    // narenas.rs, flags.rs).
    (
        "src/arena.rs",
        &[
            "prev = crate::vspec::count::fetch_sub(&self.bound[index as usize], 1, Ordering::Relaxed, Tracked(&mut perm));",
            "if prev == 0 {",
            "unbind_unbound(index);",
            "pub fn get_existing(&self, index: u16) -> (r: Option<&Arena>)",
            "p = crate::vspec::entry::load(&self.table[i], Ordering::Acquire, Tracked(v.entries.tracked_borrow(i as int)));",
            "pub fn get_existing_published(&self, index: u16) -> (r: Option<(&Arena, Tracked<&'static Published>)>)",
            "let _g = self.lock.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "let mut best_key = (u32::MAX, true);",
            "let key = (self.load_bound(i), self.table[i].load(Ordering::Relaxed).is_null());",
            "if key_less(key, best_key) {",
            "if key == (0, false) {",
            "let t = self.count_in(best);",
            "n = crate::vspec::count::load(&self.bound[i], Ordering::Relaxed, Tracked(&perm));",
            "crate::vspec::count::fetch_add(&self.bound[i], 1, Ordering::Relaxed, Tracked(&mut perm));",
            "r = crate::vspec::count::load(&self.count, Ordering::Acquire, Tracked(&v.count));",
            "if self.load_bound(i) != 0 {",
            "return (false, Tracked(no_ticket(index)));",
            "self.bind_if_idle(i)",
            "r = crate::vspec::count::compare_exchange(&self.bound[i], 0, 1, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut perm));",
            "shared.set_multi_arena();",
            "crate::vspec::entry::store(&self.table[index as usize], p, Ordering::Release, Tracked(v.entries.tracked_borrow_mut(index as int)));",
            "let mut g = self.lock.lock(Tracked(ord), Tracked(&mut under));",
            "let index = g.n;",
            "if index as usize >= MAX_ARENAS {",
            "return None;",
            "self.build(shared, index as u16, Tracked(c.half.borrow_mut()), Tracked(&mut under))?;",
            "c.n = index + 1;",
            "crate::vspec::count::store(&self.count, index + 1, Ordering::Release, Tracked(&mut v.count));",
        ],
    ),
    // `Policy::position` and `apply`'s swap (tests/loom/src/position.rs),
    // and `empty_bytes`'s walk below the arena count (narenas.rs).
    (
        "src/policy.rs",
        &[
            "f = crate::vspec::count::load(&self.scaled16, Ordering::Relaxed, Tracked(&perm));",
            "prev_frac = crate::vspec::count::swap(&self.scaled16, scaled, Ordering::Relaxed, Tracked(&mut perm));",
            "changed = (prev_frac != scaled) || changed;",
            "let n = shared.arenas.count() as usize;",
            "if let Some(arena) = shared.arenas.get_existing(i as u16) {",
        ],
    ),
    // `put_flags`, `set_multi_arena`, `free_flags` (tests/loom/src/flags.rs).
    (
        "src/heap/shared.rs",
        &[
            "crate::vspec::flags::fetch_or(&self.free_flags, FREE_ROUTE, Ordering::Relaxed);",
            "ptr_mut_write(field_at(this, S_PUSH_FLAGS), Tracked(pf), flags);",
            "ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));",
            "crate::vspec::flags::load(&self.free_flags, Ordering::Relaxed)",
        ],
    ),
    // `RouteByte`'s new, load, fetch_or (tests/loom/src/flags.rs).
    (
        "src/vspec/flags.rs",
        &[
            "pub const ROUTE: u8 = 8;",
            "RouteByte { b: AtomicU8::new(v) }",
            "b.b.load(ordering)",
            "b.b.fetch_or(v, ordering)",
        ],
    ),
    // The STATE wrappers, load_state, the LINE_STATE wrappers, load_line,
    // claim_line, publish_line, shared, shared_if_ready, line_ready,
    // claim_init, publish_ready, init_slow and feature_line_fork_child
    // (tests/loom/src/init.rs, oncecell.rs).
    (
        "src/api/mod.rs",
        &[
            "STATE.load(Ordering::Acquire)",
            "STATE.compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire)",
            "STATE.store(READY, Ordering::Release);",
            "s = state_load(Tracked(&v.perm));",
            "LINE_STATE.load(ordering)",
            "LINE_STATE.compare_exchange(current, new, success, failure)",
            "LINE_STATE.store(v, ordering)",
            "s = line_load(Ordering::Acquire, Tracked(&v.perm));",
            "won = line_compare_exchange(LINE_UNBUILT, LINE_BUILDING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));",
            "line_store(LINE_READY, Ordering::Release, Tracked(&mut v.perm));",
            "if load_state(Tracked(&mut seen)) == READY {",
            "init_slow()",
            "if load_state(Tracked(&mut seen)) == READY {",
            "if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {",
            "let (won, Tracked(cell)) = claim_line(Tracked(inv));",
            "if won.is_ok() {",
            "build_line(Tracked(&mut cell));",
            "return publish_line(Tracked(inv), Tracked(cell));",
            "while load_line(Tracked(inv), Tracked(&mut seen)) != LINE_READY",
            "core::hint::spin_loop();",
            "won = state_claim(Tracked(&mut v.perm));",
            "state_publish(Tracked(&mut v.perm));",
            "let (won, Tracked(steps)) = claim_init(Tracked(&mut seen));",
            "match won {",
            "Ok(_) => {",
            "INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);",
            "init(Tracked(steps), Tracked(&mut seen));",
            "publish_ready(Tracked(seen.published.tracked_unwrap()));",
            "Err(READY) => {}",
            "if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {",
            "sys::abort(\"allocation during initialisation (re-entrant init)\");",
            "while load_state(Tracked(&mut seen)) != READY {",
            "core::hint::spin_loop();",
            "let _reset = line_compare_exchange(LINE_BUILDING, LINE_UNBUILT, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.perm));",
        ],
    ),
    // `Deferred`: get, discover, published, fork_child, load_phase, claim,
    // publish (tests/loom/src/oncecell.rs).
    (
        "src/sys/cgroup.rs",
        &[
            "if self.load_phase(Tracked(&mut seen)) == PHASE_DONE {",
            "match self.claim(Tracked(&mut claimed)) {",
            "let cg = self.cell_write(cg, Tracked(&mut cell));",
            "found(cg);",
            "let Tracked(frozen) = self.publish(Tracked(cell));",
            "while wait && self.load_phase(Tracked(seen)) != PHASE_DONE {",
            "core::hint::spin_loop();",
            "self.cell_ref(Tracked(seen.frozen.tracked_unwrap()))",
            "let _reset = crate::vspec::byte::compare_exchange(&self.phase, PHASE_RUNNING, PHASE_PENDING, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.0));",
            "r = crate::vspec::byte::load(&self.phase, Ordering::Acquire, Tracked(&v.0));",
            "won = crate::vspec::byte::compare_exchange(&self.phase, PHASE_PENDING, PHASE_RUNNING, Ordering::Acquire, Ordering::Relaxed, Tracked(&mut v.0));",
            "crate::vspec::byte::store(&self.phase, PHASE_DONE, Ordering::Release, Tracked(&mut v.0));",
        ],
    ),
    // `requested`, `request`, `kick`, `start`'s two holds and its kick,
    // and `main`'s loop and exit (tests/loom/src/exit.rs, publish.rs).
    (
        "src/purge/background.rs",
        &[
            "REQUESTED.load(Ordering::Relaxed)",
            "let _g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "REQUESTED.store(on, Ordering::Relaxed);",
            "if !on {",
            "kick();",
            "SEQ.fetch_add(1, Ordering::SeqCst);",
            "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
            "let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "if g.present {",
            "if g.creating {",
            "g.creating = true;",
            "g.exited = false;",
            "let (stack, thread) = create(shared, stack, previous, Tracked(ord), Tracked(cell));",
            "let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "g.creating = false;",
            "g.present = thread.is_some() && !g.exited;",
            "PRESENT.store(g.present, Ordering::Relaxed);",
            "if present {",
            "fence(Ordering::SeqCst);",
            "kick();",
            "let seq = SEQ.load(Ordering::SeqCst);",
            "if !requested() {",
            "let mut g = state().lock(Tracked(&mut ord), Tracked(&mut LockOrder::spare()));",
            "g.present = false;",
            "g.exited = true;",
            "PRESENT.store(false, Ordering::Relaxed);",
            "return core::ptr::null_mut();",
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
