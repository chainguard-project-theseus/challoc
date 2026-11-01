//! The lines of challoc's source that the models copy must still be
//! there, whitespace-trimmed and in this order in each file. A change to
//! a copied step or ordering fails here until the model is revisited, and
//! so does a new line with an `Ordering::`, a `fence(` or a `futex_` call
//! inside a function a pin falls in (`loom_pins_cover_every_atomic_in_pinned_fns`) until it is
//! pinned, or waived in `WAIVED` with the reason the model leaves it out.

use std::ops::Range;
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
    // claim_line, publish_line, shared, shared_if_ready (then
    // shared_published and shared_marked, shared's body with the mark),
    // line_ready,
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
            "init_slow().0",
            "if load_state(Tracked(&mut seen)) == READY {",
            "if load_state(Tracked(&mut seen)) == READY {",
            "init_slow()",
            "if load_state_marked(Tracked(&mut seen), Tracked(mark)) == READY {",
            "init_slow().0",
            "if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {",
            "let (won, Tracked(cell)) = claim_line(Tracked(inv));",
            "if won.is_ok() {",
            "build_line(Tracked(&mut cell));",
            "return publish_line(Tracked(inv), Tracked(cell));",
            "while load_line(Tracked(inv), Tracked(&mut seen)) != LINE_READY",
            "core::hint::spin_loop();",
            "won = state_claim(Tracked(&mut v.perm));",
            "state_publish(Tracked(&mut v.perm));",
            "let (won, Tracked(parts)) = claim_init(Tracked(&mut seen));",
            "match won {",
            "Ok(_) => {",
            "INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);",
            "init(Tracked(steps), Tracked(&mut seen));",
            "let Tracked(m) = publish_ready(Tracked(seen.published.tracked_unwrap()), Tracked(half.tracked_unwrap()));",
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

/// Lines inside a pinned function that hold an atomic access, a fence or a
/// futex call and are deliberately not pinned:
/// `(file, enclosing fn, line, reason)`.
const WAIVED: &[(&str, &str, &str, &str)] = &[
    (
        "src/arena.rs",
        "get_existing_published",
        "p = crate::vspec::entry::load(&self.table[i], Ordering::Acquire, Tracked(v.entries.tracked_borrow(i as int)));",
        "get_existing_published's entry load, the same operation as get_existing's pinned one (narenas.rs: Arenas::entry)",
    ),
    (
        "src/bitmap.rs",
        "init",
        "crate::vspec::word::fetch_and(w, !self.mask, Ordering::Relaxed, Tracked(&mut perm));",
        "init of a sub-word bitmap (mask != !0): bitmap.rs leaves sub-word bitmaps out",
    ),
    (
        "src/bitmap.rs",
        "init",
        "crate::vspec::word::fetch_or(w, pad, Ordering::Relaxed, Tracked(&mut perm));",
        "init of a sub-word bitmap (mask != !0): bitmap.rs leaves sub-word bitmaps out",
    ),
    (
        "src/class/mod.rs",
        "take",
        "self.claimed.store(self.claimed.load(Ordering::Relaxed).wrapping_add(n as u64), Ordering::Relaxed);",
        "take's `claimed` statistic, under the class lock; no model reads it",
    ),
    (
        "src/class/table.rs",
        "take_group",
        "crate::sys::abort_fmt(format_args!(\"check_zero: class {class}: take_group at next_slab {s} but the watermark is {}\", self.committed_slabs.load(Ordering::Relaxed)));",
        "the check_zero abort message's second read of the watermark; the pinned comparison above decides",
    ),
    (
        "src/class/table.rs",
        "take_group",
        "previous = unsafe { load_u16_at(owner_word, Ordering::Relaxed, Tracked(&v.0)) };",
        "take_group's read of the previous owner word, under the class and commit locks; watermark.rs leaves it out",
    ),
    (
        "src/policy.rs",
        "apply",
        "let mut changed = self.limited.swap(limited, Ordering::Relaxed) != limited;",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "changed = (self.immediate.swap(immediate, Ordering::Relaxed) != immediate) || changed;",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "self.cap.store(cap, Ordering::Relaxed);",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        ".store(if limited { retention } else { 0 }, Ordering::Relaxed);",
        "apply's other fields (empty_bytes), each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "Ordering::Relaxed,",
        "apply's other fields (keep16's store, split over lines), each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "self.eager_excess.store(false, Ordering::Relaxed);",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "self.eager_excess.store(true, Ordering::Relaxed);",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "let seen = self.high_rises_seen.swap(high_rises, Ordering::Relaxed);",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/policy.rs",
        "apply",
        "self.full_purge_pending.store(true, Ordering::Relaxed);",
        "apply's other fields, each its own Relaxed atomic: position.rs leaves them out",
    ),
    (
        "src/purge.rs",
        "decay",
        "let retry = self.retry_at.load(Ordering::Relaxed);",
        "decay's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Some, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
    ),
    (
        "src/purge.rs",
        "decay",
        "let next = self.next_walk.load(Ordering::Relaxed);",
        "decay's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Some, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
    ),
    (
        "src/purge.rs",
        "decay",
        "self.next_walk.store(next, Ordering::Relaxed);",
        "decay's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Some, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
    ),
    (
        "src/purge.rs",
        "decay",
        "let retry2 = self.retry_at.load(Ordering::Relaxed);",
        "decay's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Some, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
    ),
    (
        "src/purge/background.rs",
        "request",
        "AUTO.store(false, Ordering::Relaxed);",
        "request's AUTO store, which nothing in publish.rs or exit.rs reads (publish.rs says so)",
    ),
    (
        "src/purge/background.rs",
        "main",
        "EXITS.fetch_add(1, Ordering::SeqCst);",
        "cfg(test) counter, not in the shipped library",
    ),
    (
        "src/purge/background.rs",
        "main",
        "UNTIMED_WAITS.fetch_add(1, Ordering::SeqCst);",
        "cfg(test) counter, not in the shipped library",
    ),
];

/// The indexes of the lines of `src` that `pins` match, trimmed and in
/// order (each the first match after the previous one), or the first pin
/// with no match.
fn pinned_at<'a>(src: &[&str], pins: &[&'a str]) -> Result<Vec<usize>, &'a str> {
    let mut want = pins.iter().peekable();
    let mut at = Vec::with_capacity(pins.len());
    for (i, line) in src.iter().map(|l| l.trim()).enumerate() {
        if want.peek() == Some(&&line) {
            want.next();
            at.push(i);
        }
    }
    match want.next() {
        Some(missing) => Err(missing),
        None => Ok(at),
    }
}

#[test]
fn copied_lines_in_place() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, lines) in PINS {
        let src = std::fs::read_to_string(root.join(file)).unwrap();
        let src: Vec<&str> = src.lines().collect();
        if let Err(missing) = pinned_at(&src, lines) {
            panic!("{file}: got = no line {missing:?} in order, want = every pinned line");
        }
    }
}

/// Whether `t` starts a `fn` item: qualifiers, then `fn `.
fn fn_item(t: &str) -> bool {
    let mut t = t;
    loop {
        let rest = [
            "pub ",
            "const ",
            "unsafe ",
            "extern \"C\" ",
            "proof ",
            "spec ",
            "exec ",
            "open ",
            "closed ",
        ]
        .iter()
        .find_map(|q| t.strip_prefix(q))
        .or_else(|| {
            t.strip_prefix("pub(")
                .and_then(|r| r.split_once(") ").map(|(_, r)| r))
        });
        match rest {
            Some(r) => t = r,
            None => return t.starts_with("fn "),
        }
    }
}

/// The indentation of `line`, in bytes.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The lines of the innermost `fn` item of `src` around line `at`: from
/// its `fn` line to the first `}` below it at the same indentation. `None`
/// when `at` is outside every `fn` (a module-level `const`).
fn fn_span(src: &[&str], at: usize) -> Option<Range<usize>> {
    (0..=at).rev().find_map(|start| {
        if !fn_item(src[start].trim()) {
            return None;
        }
        let n = indent(src[start]);
        let end = (start + 1..src.len()).find(|&k| {
            let t = src[k].trim();
            !t.is_empty() && (indent(src[k]) < n || (indent(src[k]) == n && t.starts_with('}')))
        })?;
        (end >= at).then_some(start..end + 1)
    })
}

/// Whether the code of `line`, before any `//` comment, touches an atomic:
/// an `Ordering::`, a `fence(` or a `futex_` call.
fn touches_atomic(line: &str) -> bool {
    let code = line.split("//").next().unwrap();
    ["Ordering::", "fence(", "futex_"]
        .iter()
        .any(|m| code.contains(m))
}

/// The name of the `fn` item whose line is `line`.
fn fn_name(line: &str) -> &str {
    let rest = &line[line.find("fn ").unwrap() + 3..];
    &rest[..rest.find(['(', '<']).unwrap_or(rest.len())]
}

/// Every line with an `Ordering::`, a `fence(` or a `futex_` call inside
/// a function a pin falls in must be pinned itself, or waived in `WAIVED`
/// with its reason. Pins only check that the copied lines are still there,
/// in order; this catches such a line inserted between them (a store, a
/// fence, a direct futex call) that the model has not copied. It does not
/// see an atomic whose ordering is a variable, or one reached through a
/// helper (a second `kick()`). Each waiver must match exactly one line, in
/// the function it names.
#[test]
fn loom_pins_cover_every_atomic_in_pinned_fns() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files: Vec<&str> = PINS.iter().map(|(f, _)| *f).collect();
    files.sort();
    files.dedup();
    let mut unpinned = Vec::new();
    let mut waived_uses = vec![0; WAIVED.len()];
    for file in files {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        let src: Vec<&str> = text.lines().collect();
        let mut pinned = vec![false; src.len()];
        let mut in_fn = vec![false; src.len()];
        for (_, lines) in PINS.iter().filter(|(f, _)| *f == file) {
            for i in pinned_at(&src, lines).unwrap_or_default() {
                pinned[i] = true;
                if let Some(span) = fn_span(&src, i) {
                    in_fn[span].fill(true);
                }
            }
        }
        for (i, line) in src.iter().enumerate() {
            let t = line.trim();
            if !in_fn[i] || pinned[i] || t.starts_with("//") || !touches_atomic(t) {
                continue;
            }
            let name = fn_name(src[fn_span(&src, i).unwrap().start]);
            match WAIVED
                .iter()
                .position(|(f, n, l, _)| *f == file && *n == name && *l == t)
            {
                Some(w) => waived_uses[w] += 1,
                None => unpinned.push(format!("{file}:{}: in {name}: {t:?}", i + 1)),
            }
        }
    }
    assert!(
        unpinned.is_empty(),
        "got = atomic lines in pinned functions that are neither pinned nor waived:\n  {}\n\
         want = each copied into its loom model and pinned in PINS, or listed in WAIVED with a reason",
        unpinned.join("\n  ")
    );
    let wrong: Vec<String> = WAIVED
        .iter()
        .zip(&waived_uses)
        .filter(|(_, uses)| **uses != 1)
        .map(|((f, n, l, _), uses)| format!("{f}: in {n}: {l:?}: {uses} lines"))
        .collect();
    assert!(
        wrong.is_empty(),
        "got = waivers that do not match exactly one atomic line in their pinned function:\n  {}\n\
         want = each waiver matching one line (drop a stale one; pin or waive each extra copy)",
        wrong.join("\n  ")
    );
}
