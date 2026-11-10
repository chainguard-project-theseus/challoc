//! The lines of challoc's source that the models copy must still be
//! there, whitespace-trimmed and in this order in each file. A change to
//! a copied step or ordering fails here until the model is revisited, and
//! so does a new line with an `Ordering::`, a `fence(` or a `futex_` call
//! inside a function a pin falls in (`loom_pins_cover_every_atomic_in_pinned_fns`) until it is
//! pinned, or waived in `WAIVED` with the reason the model leaves it out.
//!
//! The GenMC harnesses of tests/genmc copy steps too: `GENMC_PINS` pairs
//! each copied source line with the harness's copy, and
//! `genmc_copies_in_place` fails when either side of a pair changes
//! without the other, when a pass case in tests/genmc/cases runs a
//! harness with another ordering or step than the source's
//! (`GENMC_SHIPPED`), or when a harness has no pins. The functions GenMC
//! pins fall in get the same check for new atomic lines as loom's.

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
            "return Due::Done(None);",
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
    // Slab pages, the walk racing a claim (tests/loom/src/pages.rs):
    // close_page's steps, finish's mask loads, re-check and give-back,
    // probe's read-modify-write and the claims' load of the mask.
    (
        "src/class/pages.rs",
        &[
            "self.set_pages_locked(g, pg, slab, new, stamp);",
            "if bm.probe(w) & v.page_word(p, w) != 0 {",
            "self.set_pages_locked(g, pg, slab, v.closed, stamp);",
            "if !crate::purge::discard_one(a, len, Ghost(closed_page(new, p as u16, a as int, len as int))) {",
        ],
    ),
    (
        "src/class/mod.rs",
        &[
            "Some(pg) => geometry.with_closed(pg.closed()),",
            "Some(pg) => pg.closed(),",
            "let back = won & !open;",
            "let _ = bm.release(w, back, Tracked(&mut bt));",
        ],
    ),
    (
        "src/bitmap.rs",
        &["prev = crate::vspec::word::fetch_or(w, 0, Ordering::AcqRel, Tracked(&mut perm));"],
    ),
    ("src/slab.rs", &["self.closed.load(Ordering::Acquire)"]),
    // `SlabPages`' accessors of the lock-protected fields: pages_held,
    // set_pages (tests/loom/src/slab.rs).
    (
        "src/slab.rs",
        &[
            "pub fn pages_held(&self, Ghost(k): Ghost<nat>, Tracked(m): Tracked<&Map<nat, DescPerm>>) -> (r: (u16, u64))",
            "(self.closed.load(Ordering::Relaxed), self.stamp.load(Ordering::Relaxed))",
            "pub fn set_pages(&self, closed: u16, stamp: u64, Tracked(p): Tracked<&mut DescPerm>)",
            "self.stamp.store(stamp, Ordering::Relaxed);",
            "self.closed.store(closed, Ordering::Release);",
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
            "let want = lowest_n_bits(!cur & self.mask & (allow << self.shift), n);",
            "if want == 0 {",
            "return 0;",
            "prev = crate::vspec::word::fetch_or(w, want, Ordering::Acquire, Tracked(&mut perm));",
            "(want & !prev) >> self.shift",
            "let got = self.claim_word(i, n, view.word(i), Tracked(&mut *toks), Tracked(inited), Tracked(seen));",
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
            "let Some((w, won)) = bm.claim_from(word, want - k, view, Tracked(&mut wt), Tracked(&cm), Tracked(&cs)) else {",
            "empty_passes += 1;",
            "if empty_passes == MAX_EMPTY_PASSES {",
            "fn take(",
            "let live = desc.live();",
            "let avail = self.class().nslots() - nclosed - live;",
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
            "let g = self.lock.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
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
            "if self.build(shared, index as u16, Tracked(c.half.borrow_mut()), Tracked(&mut under)).is_none() {",
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
    // `double_free_scan`'s state values and `free`'s compare-and-exchange
    // (tests/loom/src/slotstate.rs).
    (
        "src/heap/slotstate.rs",
        &[
            "pub const FREE: u8 = 0;",
            "pub const CACHED: u8 = 1;",
            "pub const LIVE: u8 = 2;",
            "match crate::vspec::byte::compare_exchange(b, from, to, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut perm)) {",
        ],
    ),
    // load_state (its `vspec::count::load`), load_line, claim_line and
    // publish_line (their `vspec::byte` calls on the line's word), shared,
    // shared_if_ready (then shared_published and shared_marked, shared's
    // body with the mark), line_ready,
    // claim_init and publish_ready (their `vspec::count` calls on
    // `STATE`'s word), init_slow and feature_line_fork_child
    // (tests/loom/src/init.rs, oncecell.rs).
    (
        "src/api/mod.rs",
        &[
            "s = crate::vspec::count::load(word, Ordering::Acquire, Tracked(&v.perm));",
            "s = crate::vspec::byte::load(w, Ordering::Acquire, Tracked(&v.perm));",
            "won = crate::vspec::byte::compare_exchange(w, LINE_UNBUILT, LINE_BUILDING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));",
            "crate::vspec::byte::store(w, LINE_READY, Ordering::Release, Tracked(&mut v.perm));",
            "if load_state(Tracked(&mut seen)) == READY {",
            "init_slow(Tracked(ord)).0",
            "if load_state(Tracked(&mut seen)) == READY {",
            "if load_state(Tracked(&mut seen)) == READY {",
            "init_slow(Tracked(ord))",
            "if load_state_marked(Tracked(&mut seen), Tracked(mark)) == READY {",
            "init_slow(Tracked(&mut spare)).0",
            "if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {",
            "let (won, Tracked(cell)) = claim_line(Tracked(inv));",
            "if won.is_ok() {",
            "build_line(Tracked(&mut cell));",
            "return publish_line(Tracked(inv), Tracked(cell));",
            "if let Some(r) = wait_line(Tracked(published)) {",
            "if load_line(Tracked(inv), Tracked(&mut seen)) == LINE_READY {",
            "if crate::lock::in_child_fork_window() {",
            "feature_line_fork_child(Tracked(published));",
            "core::hint::spin_loop();",
            "won = crate::vspec::count::compare_exchange(word, UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));",
            "crate::vspec::count::store(word, READY, Ordering::Release, Tracked(&mut v.perm));",
            "let (won, Tracked(parts)) = claim_init(Tracked(&mut seen));",
            "match won {",
            "Ok(_) => {",
            "INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);",
            "init(Tracked(steps), Tracked(owned.tracked_unwrap()), Tracked(&mut seen), Tracked(ord));",
            "let Tracked(m) = publish_ready(Tracked(seen.published.tracked_unwrap()), Tracked(half.tracked_unwrap()));",
            "Err(READY) => {}",
            "if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {",
            "sys::abort(\"allocation during initialisation (re-entrant init)\");",
            "while load_state(Tracked(&mut seen)) != READY {",
            "core::hint::spin_loop();",
            "let _reset = crate::vspec::byte::compare_exchange(w, LINE_BUILDING, LINE_UNBUILT, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.perm));",
        ],
    ),
    // `Deferred`: get, discover, published, fork_child, load_phase, claim,
    // publish (tests/loom/src/oncecell.rs).
    (
        "src/sys/cgroup.rs",
        &[
            "if self.load_phase(Tracked(&mut seen)) == PHASE_DONE {",
            "match self.claim(Tracked(&mut claimed)) {",
            "let cg = self.cell_discover(Tracked(&mut cell));",
            "found(cg);",
            "let Tracked(frozen) = self.publish(Tracked(cell));",
            "if self.load_phase(Tracked(seen)) == PHASE_DONE {",
            "if crate::lock::in_child_fork_window() {",
            "self.fork_child();",
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
            "let g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "REQUESTED.store(on, Ordering::Relaxed);",
            "if !on {",
            "kick();",
            "SEQ.fetch_add(1, Ordering::SeqCst);",
            "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
            "let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
            "if g.present {",
            "if g.creating || (free && g.thread.is_some()) {",
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
        "src/heap/slotstate.rs",
        "step",
        "v = crate::vspec::byte::load(b, Ordering::Relaxed, Tracked(perm));",
        "step without exchange: the claims, pops and releases, made only by the thread that holds the slot; slotstate.rs models the free's compare-and-exchange, the one racing step",
    ),
    (
        "src/heap/slotstate.rs",
        "step",
        "crate::vspec::byte::store(b, to, Ordering::Relaxed, Tracked(&mut perm));",
        "step without exchange, as above",
    ),
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
        "due",
        "let retry = self.retry_at.load(Ordering::Relaxed);",
        "due's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Due::Done(Some) or Due::Walk, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
    ),
    (
        "src/purge.rs",
        "due",
        "let next = self.next_walk.load(Ordering::Relaxed);",
        "due's deadline hints (retry_at, next_walk): with oldest_empty nonzero every branch returns Due::Done(Some) or Due::Walk, and the models rest only on the oldest_empty == 0 -> None path and take no timeout (exit.rs)",
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

/// `(harness under tests/genmc, source file, [(source line, copy)])`: the
/// lines of a source file a GenMC harness copies, in the file's order,
/// each with the harness's copy of it. A copy is one trimmed line of the
/// harness, or several consecutive ones joined by `\n` (a call rustfmt
/// splits). The copies need not be in order, and one copy may stand for
/// two source lines. An empty copy marks an anchor: a source line pinned
/// only to place the next pins (a `fn` line, or a branch the harness
/// takes another way). Each source line must be in the same `fn` as the
/// one before it unless it is a `fn` line itself, so a short line such as
/// `return None;` cannot match the same line of a later function. An
/// ordering the harness takes from a case constant (`PUBLISH`, `ENTRY`)
/// is pinned in `GENMC_SHIPPED`.
const GENMC_PINS: &[(&str, &str, &[(&str, &str)])] = &[
    // `READY`'s publication and load (`api::publish_ready`, `load_state`).
    (
        "flags.rs",
        "src/api/mod.rs",
        &[
            ("pub const UNINIT: u32 = 0;", "const UNINIT: u32 = 0;"),
            ("pub const READY: u32 = 2;", "const READY: u32 = 2;"),
            (
                "fn load_state(Tracked(seen): Tracked<&mut Seen>) -> (s: u32)",
                "",
            ),
            (
                "s = crate::vspec::count::load(word, Ordering::Acquire, Tracked(&v.perm));",
                "self.state.load(Ordering::Acquire) == READY",
            ),
            (
                "fn publish_ready(Tracked(published): Tracked<&'static Published>, Tracked(half): Tracked<GhostVar<bool>>) -> (r: Tracked<&'static ReadyMark>)",
                "",
            ),
            (
                "crate::vspec::count::store(word, READY, Ordering::Release, Tracked(&mut v.perm));",
                "self.state.store(READY, PUBLISH);",
            ),
        ],
    ),
    // `set_multi_arena`, `put_flags`, `free_flags`.
    (
        "flags.rs",
        "src/heap/shared.rs",
        &[
            (
                "crate::vspec::flags::fetch_or(&self.free_flags, FREE_ROUTE, Ordering::Relaxed);",
                "Some((fetch_or(b, bits, Ordering::Relaxed), push))",
            ),
            ("unsafe fn put_flags<'a>(", ""),
            (
                "ptr_mut_write(field_at(this, S_PUSH_FLAGS), Tracked(pf), flags);",
                "*self.push.get() = push;",
            ),
            (
                "ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));",
                "(*self.byte.get()).write(new(push));",
            ),
            ("pub fn free_flags(&self) -> (r: u8)", ""),
            (
                "crate::vspec::flags::load(&self.free_flags, Ordering::Relaxed)",
                "let r0 = load(b, Ordering::Relaxed);\nlet r1 = load(b, Ordering::Relaxed);",
            ),
        ],
    ),
    // `load_mirror`, `load_owner`, `store_owner`, `mirror_watermark`,
    // `committed_slabs`, `watermark`, `take_group` and `next_slab_locked`.
    (
        "watermark.rs",
        "src/class/table.rs",
        &[
            (
                "r = load_usize(a, Ordering::Relaxed, Tracked(&v.0));",
                "let m = atom::load_usize(&self.mirror, Ordering::Relaxed, Tracked::assume_new());",
            ),
            (
                "unsafe fn load_owner(t: &SlabTable, w: *const AtomicU16, Ghost(slab): Ghost<usize>, seen: Tracked<Option<&'static GhostPersistentSubset<int>>>) -> (r: u16)",
                "",
            ),
            (
                "r = unsafe { load_u16_at(w, Ordering::Relaxed, Tracked(&v.0)) };",
                "atom::load_u16_at(&self.owners[slab], Ordering::Relaxed, Tracked::assume_new())",
            ),
            ("unsafe fn store_owner(", ""),
            (
                "unsafe { (*w).store(arena, Ordering::Relaxed) }",
                "self.owners[s].store(arena, Ordering::Relaxed);",
            ),
            ("pub unsafe fn mirror_watermark(", ""),
            (
                "let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));",
                "let g = self.lock.lock().unwrap();",
            ),
            (
                "let wm = gi.next_slab;",
                "let wm = unsafe { *self.next.get() };",
            ),
            (
                "unsafe { store_usize_at(mirror, wm, Ordering::Release, Tracked(&mut v.0)) };",
                "self.store_mirror(wm);",
            ),
            (
                "gi.mirror = mirror;",
                "unsafe { *self.installed.get() = true };",
            ),
            ("pub fn committed_slabs(&self) -> (r: usize)", ""),
            (
                "load_usize_seen(&self.committed_slabs, Ordering::Acquire, &self.committed_inv).0",
                "if order == Ordering::Acquire {\nvspec::load_usize_seen(&self.committed, order)",
            ),
            ("fn watermark(&self, order: Ordering) -> (r: usize)", ""),
            (
                "r = load_usize(&self.committed_slabs, order, Tracked(&v.0));",
                "atom::load_usize(&self.committed, order, Tracked::assume_new())",
            ),
            ("pub(super) fn take_group(", ""),
            (
                "let mut g = self.inner.lock(Tracked(ord), Tracked(&mut under));",
                "let g = self.lock.lock().unwrap();",
            ),
            (
                "let s = self.next_slab_locked(&*g, &*sets);",
                "let s = unsafe { *self.next.get() };",
            ),
            ("if s >= self.nslabs {", "if s >= NSLABS {"),
            ("return None;", "return None;"),
            (
                "if purge::check_zero() && s != self.committed_slabs.load(Ordering::Relaxed) {",
                "s == self.watermark(Ordering::Relaxed),",
            ),
            (
                "unsafe { store_owner(owner_word, arena, Ghost(*self), Ghost(s >> self.lg_group), Tracked(&mut v.0), Tracked(om)) };",
                "self.owners[s].store(arena, Ordering::Relaxed);",
            ),
            ("gi.next_slab = end;", "unsafe { *self.next.get() = end };"),
            (
                "store_usize(&self.committed_slabs, end, Ordering::Release, Tracked(&mut v.0));",
                "atom::store_usize(&self.committed, end, PUBLISH, Tracked::assume_new());",
            ),
            (
                "if !gi.mirror.is_null() {",
                "if installed && !MIRROR_UNLOCKED {\nself.store_mirror(end);",
            ),
            (
                "unsafe { store_usize_at(gi.mirror, end, Ordering::Release, Tracked(&mut v.0)) };",
                "atom::store_usize_at(\natom::usize_ptr(&self.mirror),\nv,\nOrdering::Release,\nTracked::assume_new(),\n)",
            ),
            ("g.unlock();", "drop(g);"),
            (
                "fn next_slab_locked(&self, t: &TableInner, v: &Inner) -> (r: usize)",
                "",
            ),
            ("t.next_slab", "let s = unsafe { *self.next.get() };"),
        ],
    ),
    // `ClassLine::set_slab`'s load of the copy.
    (
        "watermark.rs",
        "src/heap/shared.rs",
        &[(
            "let committed = crate::class::load_mirror(&self.committed, self.committed_inv);",
            "let m = atom::load_usize(&self.mirror, Ordering::Relaxed, Tracked::assume_new());",
        )],
    ),
    // `unbind`, `assign`, `load_bound`, `count_in`, `try_bind_idle` and
    // `bind_if_idle`.
    (
        "count.rs",
        "src/arena.rs",
        &[
            (
                "prev = crate::vspec::count::fetch_sub(&self.bound[index as usize], 1, Ordering::Relaxed, Tracked(&mut perm));",
                "let prev = count::fetch_sub(&self.bound[i], 1, Ordering::Relaxed, Tracked::assume_new());",
            ),
            (
                "if prev == 0 {",
                "gassert!(prev != 0, \"unbinding a heap that was never bound (count 0)\");",
            ),
            (
                "pub fn assign(&self, Tracked(ord): Tracked<&mut LockOrder>) -> (r: (u16, Tracked<Homed>, Ghost<Seq<(u32, bool)>>))",
                "",
            ),
            (
                "let g = self.lock.lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
                "let g = self.lock.lock().unwrap();",
            ),
            (
                "let mut best_key = (u32::MAX, true);",
                "let mut best_key = (u32::MAX, true);",
            ),
            (
                "let key = (self.load_bound(i), self.table[i].load(Ordering::Relaxed).is_null());",
                "let key = (self.load_bound(i), !built);",
            ),
            ("if key_less(key, best_key) {", "if key < best_key {"),
            ("if key == (0, false) {", "if key == (0, false) {"),
            ("let t = self.count_in(best);", "self.count_in(best);"),
            ("g.unlock();", "drop(g);"),
            ("fn load_bound(&self, i: usize) -> u32", ""),
            (
                "n = crate::vspec::count::load(&self.bound[i], Ordering::Relaxed, Tracked(&perm));",
                "count::load(&self.bound[i], Ordering::Relaxed, Tracked::assume_new())",
            ),
            ("fn count_in(&self, i: usize) -> (r: Tracked<Homed>)", ""),
            (
                "crate::vspec::count::fetch_add(&self.bound[i], 1, Ordering::Relaxed, Tracked(&mut perm));",
                "count::fetch_add(&self.bound[i], 1, Ordering::Relaxed, Tracked::assume_new());",
            ),
            (
                "pub fn try_bind_idle(&self, index: u16) -> (r: (bool, Tracked<Homed>))",
                "",
            ),
            (
                "if self.load_bound(i) != 0 {",
                "if self.load_bound(i) != 0 {",
            ),
            (
                "return (false, Tracked(no_ticket(index)));",
                "return false;",
            ),
            (
                "fn bind_if_idle(&self, i: usize) -> (r: (bool, Tracked<Homed>))",
                "",
            ),
            (
                "r = crate::vspec::count::compare_exchange(&self.bound[i], 0, 1, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut perm));",
                "count::compare_exchange(\n&self.bound[i],\n0,\n1,\nOrdering::Relaxed,\nOrdering::Relaxed,\nTracked::assume_new(),\n)",
            ),
        ],
    ),
    // `Heap::try_rehome`'s call of `try_bind_idle`, with no lock.
    (
        "count.rs",
        "src/heap/mod.rs",
        &[(
            "let (won, Tracked(t)) = shared.arenas.try_bind_idle(to);",
            "let won = a.try_bind_idle(1);",
        )],
    ),
    // `get_existing`'s entry load, `count`, `build`'s entry store and
    // `create`.
    (
        "narenas.rs",
        "src/arena.rs",
        &[
            (
                "p = crate::vspec::entry::load(&self.table[i], Ordering::Acquire, Tracked(v.entries.tracked_borrow(i as int)));",
                "entry::load(&self.table[i], Ordering::Acquire, Tracked::assume_new())",
            ),
            (
                "pub fn count(&self) -> (r: u32)",
                "fn count(&self) -> u32 {",
            ),
            (
                "r = crate::vspec::count::load(&self.count, Ordering::Acquire, Tracked(&v.count));",
                "count::load(&self.count, Ordering::Acquire, Tracked::assume_new())",
            ),
            ("fn build<'a>(", ""),
            (
                "crate::vspec::entry::store(&self.table[index as usize], p, Ordering::Release, Tracked(v.entries.tracked_borrow_mut(index as int)));",
                "entry::store(\n&self.table[i],\nptr::dangling_mut(),\nENTRY,\nTracked::assume_new(),\n);",
            ),
            (
                "pub fn create(&self, shared: &Shared, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>) -> (r: Option<u16>)",
                "",
            ),
            (
                "let mut g = self.lock.lock(Tracked(ord), Tracked(&mut under));",
                "let g = self.lock.lock().unwrap();",
            ),
            (
                "let index = g.n;",
                "let index = unsafe { *self.next.get() };",
            ),
            (
                "if index as usize >= MAX_ARENAS {",
                "if index >= MAX_ARENAS {",
            ),
            ("return None;", "return None;"),
            (
                "if self.build(shared, index as u16, Tracked(c.half.borrow_mut()), Tracked(&mut under)).is_none() {",
                "unsafe { *self.fields[i].get() = u64::from(index) + 1 };",
            ),
            (
                "c.n = index + 1;",
                "unsafe { *self.next.get() = index + 1 };",
            ),
            (
                "crate::vspec::count::store(&self.count, index + 1, Ordering::Release, Tracked(&mut v.count));",
                "count::store(&self.count, index + 1, PUBLISH, Tracked::assume_new());",
            ),
        ],
    ),
    // `empty_bytes`'s walk below the arena count.
    (
        "narenas.rs",
        "src/policy.rs",
        &[
            (
                "let n = shared.arenas.count() as usize;",
                "let n = self.count();",
            ),
            (
                "if let Some(arena) = shared.arenas.get_existing(i as u16) {",
                "if self.entry(i).is_null() {",
            ),
        ],
    ),
    // The state values, `step`'s compare-and-exchange and `free`'s call
    // of it from `LIVE`.
    (
        "slotstate.rs",
        "src/heap/slotstate.rs",
        &[
            ("pub const FREE: u8 = 0;", "const FREE: u8 = 0;"),
            ("pub const CACHED: u8 = 1;", "const CACHED: u8 = 1;"),
            ("pub const LIVE: u8 = 2;", "const LIVE: u8 = 2;"),
            (
                "fn step(shared: &Shared, class: usize, p: *mut u8, from: u8, to: u8, exchange: bool, Tracked(obs): Tracked<&mut Ghost<(u8, u8)>>) -> (r: u8)",
                "",
            ),
            ("if exchange {", ""),
            (
                "match crate::vspec::byte::compare_exchange(b, from, to, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut perm)) {",
                "byte::compare_exchange(\nb,\nLIVE,\nto,\nOrdering::Relaxed,\nOrdering::Relaxed,\nTracked::assume_new(),\n)",
            ),
            (
                "pub(super) fn free(shared: &Shared, class: usize, p: *mut u8, to: u8)",
                "",
            ),
            (
                "let r = step(shared, class, p, LIVE, to, true, Tracked(&mut obs));",
                "fn free(b: &AtomicU8, to: u8) -> bool {",
            ),
        ],
    ),
    // `AtomicBitmap::init` (whole words), `claim_word`, `claim_from` and
    // `release`.
    (
        "bitmap.rs",
        "src/bitmap.rs",
        &[
            (
                "pub fn init(&self, nbits: usize, Ghost(slab): Ghost<int>, Tracked(f): Tracked<GhostSubset<int>>) -> (r: Tracked<GhostPersistentSubset<int>>)",
                "",
            ),
            ("let lo = i * 64;", "let lo = i * 64;"),
            (
                "let pad = if nbits >= lo + 64 {",
                "let pad = if nbits >= lo + 64 {",
            ),
            ("} else if nbits <= lo {", "} else if nbits <= lo {"),
            ("!0u64 << (nbits - lo)", "!0u64 << (nbits - lo)"),
            (
                "crate::vspec::word::store(w, pad, Ordering::Relaxed, Tracked(&mut perm));",
                "word::store(w, pad, Ordering::Relaxed, Tracked::assume_new());",
            ),
            ("pub fn claim_word(", ""),
            (
                "let cur = w.load(Ordering::Relaxed);",
                "let cur = w.load(Ordering::Relaxed);",
            ),
            (
                "let want = lowest_n_bits(!cur & self.mask & (allow << self.shift), n);",
                "let want = lowest_n_bits(!cur, n);",
            ),
            ("if want == 0 {", "if want == 0 {"),
            ("return 0;", "return 0;"),
            (
                "prev = crate::vspec::word::fetch_or(w, want, Ordering::Acquire, Tracked(&mut perm));",
                "let prev = word::fetch_or(w, want, Ordering::Acquire, Tracked::assume_new());",
            ),
            ("(want & !prev) >> self.shift", "want & !prev"),
            ("pub fn claim_from(", ""),
            ("let len = self.words.len();", "let len = self.words.len();"),
            (
                "let got = self.claim_word(i, n, view.word(i), Tracked(&mut *toks), Tracked(inited), Tracked(seen));",
                "let got = self.claim_word(i, n);",
            ),
            ("if got != 0 {", "if got != 0 {"),
            ("return Some((i, got));", "return Some((i, got));"),
            ("i += 1;", "i += 1;"),
            ("if i == len {", "if i == len {"),
            ("i = 0;", "i = 0;"),
            (
                "pub fn release(&self, i: usize, mask: u64, Tracked(toks): Tracked<&mut GhostSubset<int>>) -> (r: (u64, bool))",
                "",
            ),
            (
                "prev = crate::vspec::word::fetch_and(w, !m, Ordering::Release, Tracked(&mut perm));",
                "word::fetch_and(\n&self.words[0],\n!mask,\nOrdering::Release,\nTracked::assume_new(),\n)",
            ),
        ],
    ),
    // The class bitmap pass, `take`'s reservation under the class lock,
    // the free path's release and `dec_live`.
    (
        "bitmap.rs",
        "src/class/mod.rs",
        &[
            (
                "let Some((w, won)) = bm.claim_from(word, want - k, view, Tracked(&mut wt), Tracked(&cm), Tracked(&cs)) else {",
                "match self.claim_from(0, want - got.count_ones() as usize) {",
            ),
            ("empty_passes += 1;", "empty += 1;"),
            (
                "if empty_passes == MAX_EMPTY_PASSES {",
                "if empty >= passes {",
            ),
            ("fn take(", "fn reserve(&self, want: usize) -> usize {"),
            ("g: &mut Inner,", "let g = self.lock.lock().unwrap();"),
            (
                "let live = desc.live();",
                "let live = self.live.load(Ordering::Acquire) as usize;",
            ),
            (
                "let avail = self.class().nslots() - nclosed - live;",
                "let n = min(NSLOTS - live, want);",
            ),
            (
                "let n = avail.min(*remaining);",
                "let n = min(NSLOTS - live, want);",
            ),
            (
                "desc.add_live(n);",
                "self.live.fetch_add(n as u32, Ordering::AcqRel);",
            ),
            (
                "fn release_word(&self, slab: usize, word: usize, mask: u64, reopen: &mut Reopen, Tracked(tok): Tracked<&mut GhostSubset<int>>, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)",
                "",
            ),
            (
                "let (prev, was_full) = bm.release(word, mask, Tracked(tok));",
                "fn release(&self, mask: u64, relaxed: bool) {",
            ),
            (
                "if prev & mask != mask {",
                "gassert!(prev & mask == mask, \"released a clear bit\");",
            ),
            (
                "fn dec_live(&self, slab: usize, n: usize, pubw: Tracked<&'static Published>, Tracked(ord): Tracked<&mut LockOrder>)",
                "",
            ),
            (
                "if self.desc(slab).sub_live(n) == n {",
                "let prev = self.live.fetch_sub(n, Ordering::AcqRel);",
            ),
        ],
    ),
    // `SlabDesc`'s `live`, `add_live` and `sub_live`.
    (
        "bitmap.rs",
        "src/slab.rs",
        &[
            (
                "self.live.load(Ordering::Acquire) as usize",
                "let live = self.live.load(Ordering::Acquire) as usize;",
            ),
            ("pub fn add_live(&self, n: usize) -> usize {", ""),
            (
                "self.live.fetch_add(n as u32, Ordering::AcqRel) as usize",
                "self.live.fetch_add(n as u32, Ordering::AcqRel);",
            ),
            ("pub fn sub_live(&self, n: usize) -> (r: usize)", ""),
            (
                "let prev = self.live.fetch_sub(n as u32, Ordering::AcqRel) as usize;",
                "let prev = self.live.fetch_sub(n, Ordering::AcqRel);",
            ),
        ],
    ),
    // `committed_slabs`, and `take_group`'s bitmap init then the
    // watermark's store, which publishes it.
    (
        "bitmap.rs",
        "src/class/table.rs",
        &[
            (
                "load_usize_seen(&self.committed_slabs, Ordering::Acquire, &self.committed_inv).0",
                "if vspec::load_usize_seen(&s.committed, Ordering::Acquire) == 1 {",
            ),
            ("pub(super) fn take_group(", ""),
            (
                "let Tracked(m) = self.bits_unchecked(slab).init(cls.nslots(), Ghost(slab as int), Tracked(fs));",
                "s.init(NSLOTS);\natom::store_usize(&s.committed, 1, PUBLISH, Tracked::assume_new());",
            ),
            (
                "store_usize(&self.committed_slabs, end, Ordering::Release, Tracked(&mut v.0));",
                "atom::store_usize(&s.committed, 1, PUBLISH, Tracked::assume_new());",
            ),
        ],
    ),
    // `api::load_state`, `shared`, `shared_if_ready`, `claim_init`,
    // `publish_ready` and `init_slow`.
    (
        "init.rs",
        "src/api/mod.rs",
        &[
            ("pub const UNINIT: u32 = 0;", "const UNINIT: u32 = 0;"),
            (
                "pub const INITIALISING: u32 = 1;",
                "const INITIALISING: u32 = 1;",
            ),
            ("pub const READY: u32 = 2;", "const READY: u32 = 2;"),
            (
                "fn load_state(Tracked(seen): Tracked<&mut Seen>) -> (s: u32)",
                "",
            ),
            (
                "s = crate::vspec::count::load(word, Ordering::Acquire, Tracked(&v.perm));",
                "let s = self.state.load(Ordering::Acquire);",
            ),
            (
                "pub fn shared(Tracked(ord): Tracked<&mut LockOrder>) -> (r: &'static Shared)",
                "",
            ),
            (
                "if load_state(Tracked(&mut seen)) == READY {",
                "if self.state_load(seen) == READY {",
            ),
            ("init_slow(Tracked(ord)).0", "self.init_slow(tid, seen)"),
            (
                "pub fn shared_if_ready() -> (r: Option<&'static Shared>)",
                "",
            ),
            (
                "if load_state(Tracked(&mut seen)) == READY {",
                "if self.state_load(&mut seen) == READY {",
            ),
            (
                "fn claim_init(Tracked(seen): Tracked<&mut Seen>) -> (r: (Result<u32, u32>, Tracked<ClaimTokens>))",
                "",
            ),
            (
                "won = crate::vspec::count::compare_exchange(word, UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire, Tracked(&mut v.perm));",
                "self.state\n.compare_exchange(UNINIT, INITIALISING, Ordering::Acquire, Ordering::Acquire);",
            ),
            (
                "fn publish_ready(Tracked(published): Tracked<&'static Published>, Tracked(half): Tracked<GhostVar<bool>>) -> (r: Tracked<&'static ReadyMark>)",
                "",
            ),
            (
                "crate::vspec::count::store(word, READY, Ordering::Release, Tracked(&mut v.perm));",
                "self.state.store(READY, PUBLISH);",
            ),
            (
                "fn init_slow(Tracked(ord): Tracked<&mut LockOrder>) -> (r: (&'static Shared, Tracked<&'static ReadyMark>))",
                "",
            ),
            (
                "let (won, Tracked(parts)) = claim_init(Tracked(&mut seen));",
                "",
            ),
            ("match won {", "match won {"),
            ("Ok(_) => {", "Ok(_) => {"),
            (
                "INIT_TID.store(sys::gettid() as u32, Ordering::Relaxed);",
                "self.init_tid.store(tid, Ordering::Relaxed);",
            ),
            (
                "init(Tracked(steps), Tracked(owned.tracked_unwrap()), Tracked(&mut seen), Tracked(ord));",
                "self.init(tid, seen);",
            ),
            (
                "let Tracked(m) = publish_ready(Tracked(seen.published.tracked_unwrap()), Tracked(half.tracked_unwrap()));",
                "self.state.store(READY, PUBLISH);",
            ),
            ("Err(READY) => {}", "Err(READY) => {}"),
            (
                "if INIT_TID.load(Ordering::Relaxed) == sys::gettid() as u32 {",
                "if self.init_tid.load(Ordering::Relaxed) == tid {",
            ),
            (
                "sys::abort(\"allocation during initialisation (re-entrant init)\");",
                "return ABORT;",
            ),
            (
                "while load_state(Tracked(&mut seen)) != READY {",
                "await_until!(self.state_load(seen) == READY);",
            ),
        ],
    ),
    // The free: `on_live_zero`'s first `Empty` and its `note_empty`.
    (
        "background.rs",
        "src/class/mod.rs",
        &[
            (
                "if self.gate.note_first_empty(now) {",
                "if note_first_empty() {",
            ),
            ("purge::note_empty();", "wake();"),
        ],
    ),
    // `Decayer::oldest_empty`, `note_first_empty` and `due`'s no-deadline
    // path, and `note_empty`.
    (
        "background.rs",
        "src/purge.rs",
        &[
            ("pub fn oldest_empty(&self) -> u64 {", ""),
            (
                "self.oldest_empty.load(Ordering::Relaxed)",
                "let oldest = OLDEST_EMPTY.load(Ordering::Relaxed);",
            ),
            ("pub fn note_first_empty(&self, now: u64) -> bool {", ""),
            (
                "if self.oldest_empty.load(Ordering::Relaxed) != 0 {",
                "if OLDEST_EMPTY.load(Ordering::Relaxed) != 0 {",
            ),
            (
                "self.oldest_empty.store(now, Ordering::Relaxed);",
                "OLDEST_EMPTY.store(1, Ordering::Relaxed);",
            ),
            ("pub fn due(&self, now: u64, decay_ns: u64) -> (r: Due)", ""),
            (
                "let oldest = self.oldest_empty();",
                "let oldest = OLDEST_EMPTY.load(Ordering::Relaxed);",
            ),
            ("if oldest == 0 {", ""),
            ("return Due::Done(None);", ""),
            ("pub fn note_empty()", ""),
            ("background::wake();", "wake();"),
        ],
    ),
    // `present`, `wake`, `kick`, `start`'s creation, publication and
    // kick, and `main`'s loop.
    (
        "background.rs",
        "src/purge/background.rs",
        &[
            ("pub fn present() -> bool {", ""),
            (
                "PRESENT.load(Ordering::Relaxed)",
                "if PRESENT.load(Ordering::Relaxed) {",
            ),
            ("pub fn wake()", ""),
            (
                "fence(Ordering::SeqCst);",
                "if FENCE_FREE {\nfence(Ordering::SeqCst);",
            ),
            ("if present() {", "if PRESENT.load(Ordering::Relaxed) {"),
            ("kick();", "kick();"),
            ("fn kick()", ""),
            (
                "SEQ.fetch_add(1, Ordering::SeqCst);",
                "SEQ.fetch_add(1, Ordering::SeqCst);",
            ),
            (
                "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
                "futex_wake(&SEQ);",
            ),
            (
                "pub fn start(shared: &Shared, free: bool, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(cell): Tracked<&mut TlsCell>) -> bool",
                "",
            ),
            (
                "let (stack, thread) = create(shared, stack, previous, Tracked(ord), Tracked(cell));",
                "create();",
            ),
            (
                "PRESENT.store(g.present, Ordering::Relaxed);",
                "PRESENT.store(true, Ordering::Relaxed);",
            ),
            ("if present {", ""),
            (
                "fence(Ordering::SeqCst);",
                "if FENCE_START {\nfence(Ordering::SeqCst);",
            ),
            ("kick();", "kick();"),
            (
                "extern \"C\" fn main(arg: *mut c_void) -> *mut c_void {",
                "",
            ),
            (
                "let seq = SEQ.load(Ordering::SeqCst);",
                "let seq = SEQ.load(Ordering::SeqCst);",
            ),
            (
                "let deadline = super::decay(shared, now, Tracked(&mut ord));",
                "let oldest = OLDEST_EMPTY.load(Ordering::Relaxed);",
            ),
            (
                "let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };",
                "futex_wait(&SEQ, seq, &mut spurious);",
            ),
        ],
    ),
    // `requested`, `request(false)`, `kick` and `main`'s loop.
    (
        "exit.rs",
        "src/purge/background.rs",
        &[
            ("pub fn requested() -> bool {", ""),
            (
                "REQUESTED.load(Ordering::Relaxed)",
                "if !REQUESTED.load(Ordering::Relaxed) {",
            ),
            (
                "pub fn request(on: bool, Tracked(cc): Tracked<&CCall>, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>)",
                "",
            ),
            (
                "REQUESTED.store(on, Ordering::Relaxed);",
                "REQUESTED.store(false, Ordering::Relaxed);",
            ),
            ("if !on {", "if WITHDRAW == 0 {"),
            ("kick();", "kick();"),
            ("fn kick()", ""),
            (
                "SEQ.fetch_add(1, Ordering::SeqCst);",
                "SEQ.fetch_add(1, Ordering::SeqCst);",
            ),
            (
                "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
                "futex_wake(&SEQ);",
            ),
            (
                "extern \"C\" fn main(arg: *mut c_void) -> *mut c_void {",
                "",
            ),
            (
                "let seq = SEQ.load(Ordering::SeqCst);",
                "let seq = SEQ.load(SEQ_LOAD);",
            ),
            (
                "if !requested() {",
                "if !REQUESTED.load(Ordering::Relaxed) {",
            ),
            ("return core::ptr::null_mut();", "return;"),
            (
                "let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };",
                "futex_wait(&SEQ, seq, &mut spurious);",
            ),
        ],
    ),
    // `request(false)`, `kick`, `start`'s two holds and its kick, and
    // `main`'s loop and exit.
    (
        "publish.rs",
        "src/purge/background.rs",
        &[
            (
                "pub fn request(on: bool, Tracked(cc): Tracked<&CCall>, Tracked(fx): Tracked<&mut Ghost<Change>>, Tracked(ord): Tracked<&mut LockOrder>)",
                "",
            ),
            (
                "let g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
                "let g = STATE.lock().unwrap();",
            ),
            (
                "REQUESTED.store(on, Ordering::Relaxed);",
                "REQUESTED.store(false, Ordering::Relaxed);",
            ),
            ("g.unlock();", "drop(g);"),
            ("if !on {", ""),
            ("kick();", "kick();"),
            ("fn kick()", ""),
            (
                "SEQ.fetch_add(1, Ordering::SeqCst);",
                "SEQ.fetch_add(1, Ordering::SeqCst);",
            ),
            (
                "let _ = unsafe { sys::futex_wake(SEQ.as_ptr(), 1) };",
                "futex_wake(&SEQ);",
            ),
            (
                "pub fn start(shared: &Shared, free: bool, Tracked(ord): Tracked<&mut LockOrder>, Tracked(errno): Tracked<&mut ThreadErrno>, Tracked(cell): Tracked<&mut TlsCell>) -> bool",
                "",
            ),
            (
                "let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
                "let mut g = STATE.lock().unwrap();",
            ),
            ("if g.present {", "if g.present {"),
            ("return true;", "return true;"),
            // An allocation's start (`free` false): a `free`'s refusal for a
            // recorded thread is not in the harness.
            (
                "if g.creating || (free && g.thread.is_some()) {",
                "if g.creating {",
            ),
            ("return false;", "return false;"),
            ("g.creating = true;", "g.creating = true;"),
            ("g.exited = false;", "g.exited = false;"),
            ("g.unlock();", "drop(g);"),
            (
                "let (stack, thread) = create(shared, stack, previous, Tracked(ord), Tracked(cell));",
                "create();",
            ),
            (
                "let mut g = state().lock(Tracked(ord), Tracked(&mut LockOrder::spare()));",
                "let mut g = STATE.lock().unwrap();",
            ),
            ("g.creating = false;", "g.creating = false;"),
            (
                "g.present = thread.is_some() && !g.exited;",
                "g.present = !CHECKS_EXITED || !g.exited;",
            ),
            (
                "PRESENT.store(g.present, Ordering::Relaxed);",
                "PRESENT.store(g.present, Ordering::Relaxed);",
            ),
            ("if present {", "if present {"),
            ("fence(Ordering::SeqCst);", "fence(Ordering::SeqCst);"),
            ("kick();", "kick();"),
            (
                "extern \"C\" fn main(arg: *mut c_void) -> *mut c_void {",
                "",
            ),
            (
                "let seq = SEQ.load(Ordering::SeqCst);",
                "let seq = SEQ.load(SEQ_LOAD);",
            ),
            (
                "if !requested() {",
                "if !REQUESTED.load(Ordering::Relaxed) {",
            ),
            (
                "let mut g = state().lock(Tracked(&mut ord), Tracked(&mut LockOrder::spare()));",
                "let mut g = STATE.lock().unwrap();",
            ),
            ("g.present = false;", "g.present = false;"),
            ("g.exited = true;", "g.exited = true;"),
            (
                "PRESENT.store(false, Ordering::Relaxed);",
                "PRESENT.store(false, Ordering::Relaxed);",
            ),
            ("g.unlock();", "drop(g);"),
            ("return core::ptr::null_mut();", "return;"),
            (
                "let _ = unsafe { sys::futex_wait(SEQ.as_ptr(), seq, timeout.as_ref()) };",
                "futex_wait(&SEQ, seq, &mut spurious);",
            ),
        ],
    ),
    // Slab pages, the walk racing a claim (pages.rs, the port of
    // tests/loom/src/pages.rs): close_page's mask stores, probe test and
    // discard; the probe's read-modify-write; finish's re-check and
    // give-back; the claims' load of the mask.
    (
        "pages.rs",
        "src/class/pages.rs",
        &[
            (
                "self.set_pages_locked(g, pg, slab, new, stamp);",
                "self.closed.store(0b10, Ordering::Release);",
            ),
            (
                "if bm.probe(w) & v.page_word(p, w) != 0 {",
                "if prev & PAGE_SLOTS[1] != 0 {",
            ),
            (
                "self.set_pages_locked(g, pg, slab, v.closed, stamp);",
                "self.closed.store(0, Ordering::Release);",
            ),
            (
                "if !crate::purge::discard_one(a, len, Ghost(closed_page(new, p as u16, a as int, len as int))) {",
                "self.write(PAGE_SLOTS[1], 0);",
            ),
        ],
    ),
    (
        "pages.rs",
        "src/bitmap.rs",
        &[(
            "prev = crate::vspec::word::fetch_or(w, 0, Ordering::AcqRel, Tracked(&mut perm));",
            "word::fetch_or(&self.word, 0, Ordering::AcqRel, Tracked::assume_new())",
        )],
    ),
    (
        "pages.rs",
        "src/class/mod.rs",
        &[
            ("let back = won & !open;", "let back = won & !open;"),
            (
                "let _ = bm.release(w, back, Tracked(&mut bt));",
                "word::fetch_and(&self.word, !back, Ordering::Release, Tracked::assume_new());",
            ),
        ],
    ),
    (
        "pages.rs",
        "src/slab.rs",
        &[(
            "self.closed.load(Ordering::Acquire)",
            "let closed = self.closed.load(Ordering::Acquire);",
        )],
    ),
];

/// `(harness, constants)`: the case constants that stand for the
/// source's orderings and steps, each of which every `pass` case of the
/// harness in tests/genmc/cases must write as given. A control may set
/// them otherwise. Every `Ordering` constant a case of the harness sets
/// must be listed.
const GENMC_SHIPPED: &[(&str, &[&str])] = &[
    (
        "flags.rs",
        &[
            "const PUBLISH: Ordering = Ordering::Release;",
            "const BITS: u8 = ROUTE;",
        ],
    ),
    (
        "watermark.rs",
        &[
            "const PUBLISH: Ordering = Ordering::Release;",
            "const MIRROR_UNLOCKED: bool = false;",
        ],
    ),
    (
        "count.rs",
        &[
            "const SPLIT: bool = false;",
            "const SPLIT_CAS: bool = false;",
        ],
    ),
    (
        "narenas.rs",
        &[
            "const PUBLISH: Ordering = Ordering::Release;",
            "const ENTRY: Ordering = Ordering::Release;",
        ],
    ),
    ("slotstate.rs", &["const LOAD_STORE: bool = false;"]),
    ("pages.rs", &["const MUTANT: u32 = 0;"]),
    (
        "bitmap.rs",
        &[
            "const MUTANT: u32 = 0;",
            "const PUBLISH: Ordering = Ordering::Release;",
        ],
    ),
    (
        "init.rs",
        &[
            "const PUBLISH: Ordering = Ordering::Release;",
            "const UNPUBLISHED: bool = false;",
        ],
    ),
    (
        "background.rs",
        &[
            "const FENCE_FREE: bool = true;",
            "const FENCE_START: bool = true;",
        ],
    ),
    (
        "exit.rs",
        &[
            "const WITHDRAW: u32 = 0;",
            "const SEQ_LOAD: Ordering = Ordering::SeqCst;",
        ],
    ),
    (
        "publish.rs",
        &[
            "const CHECKS_EXITED: bool = true;",
            "const SEQ_LOAD: Ordering = Ordering::SeqCst;",
        ],
    ),
];

/// The GenMC templates that copy no source step.
const GENMC_NO_COPIES: &[&str] = &["litmus.rs"];

/// Whether `copy` (lines joined by `\n`) is a run of consecutive lines of
/// `src`, each trimmed.
fn has_copy(src: &[&str], copy: &str) -> bool {
    let want: Vec<&str> = copy.split('\n').collect();
    src.windows(want.len())
        .any(|w| w.iter().map(|l| l.trim()).eq(want.iter().copied()))
}

/// The GenMC harnesses' copies of source steps, both sides: each source
/// line in `GENMC_PINS` is still in its file in order, each copy is still
/// in its harness, every pass case runs a harness with the source's
/// orderings (`GENMC_SHIPPED`), and every template but those in
/// `GENMC_NO_COPIES` has pins.
#[test]
fn genmc_copies_in_place() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let genmc = root.join("tests/genmc");
    let mut bad = Vec::new();
    for (harness, file, pairs) in GENMC_PINS {
        let src = std::fs::read_to_string(root.join(file)).unwrap();
        let src: Vec<&str> = src.lines().collect();
        let lines: Vec<&str> = pairs.iter().map(|(l, _)| *l).collect();
        match pinned_at(&src, &lines) {
            Err(missing) => bad.push(format!(
                "{file}: no line {missing:?} in order (copied by {harness})"
            )),
            Ok(at) => {
                for k in 1..at.len() {
                    let (prev, cur) = (fn_span(&src, at[k - 1]), fn_span(&src, at[k]));
                    if cur != prev && !fn_item(src[at[k]].trim()) {
                        bad.push(format!(
                            "{file}:{}: {:?} (copied by {harness}) is not in the fn of the pin before it; \
                             pin its fn line first",
                            at[k] + 1,
                            lines[k]
                        ));
                    }
                }
            }
        }
        let text = std::fs::read_to_string(genmc.join(harness)).unwrap();
        let text: Vec<&str> = text.lines().collect();
        for (line, copy) in *pairs {
            if copy.is_empty() {
                continue;
            }
            if copy.split('\n').any(str::is_empty) {
                bad.push(format!(
                    "GENMC_PINS: {harness}: copy {copy:?} of {line:?} has an empty line"
                ));
            } else if !has_copy(&text, copy) {
                bad.push(format!(
                    "tests/genmc/{harness}: no copy {copy:?} of {file}'s {line:?}"
                ));
            }
        }
    }

    // `name | template | models | constants | expected [| report]...`,
    // each constant an item ending in `;`.
    let text = std::fs::read_to_string(genmc.join("cases")).unwrap();
    let mut cases = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        if f.len() < 5 {
            bad.push(format!("tests/genmc/cases: malformed line {line:?}"));
            continue;
        }
        let consts: Vec<&str> = f[3]
            .split(';')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        cases.push((f[0], f[1], consts, f[4]));
    }
    for (harness, shipped) in GENMC_SHIPPED {
        let tmpl = harness.strip_suffix(".rs").unwrap();
        let shipped: Vec<&str> = shipped.iter().map(|c| c.trim_end_matches(';')).collect();
        let mut passes = 0;
        for (name, _, consts, expected) in cases.iter().filter(|c| c.1 == tmpl) {
            for c in consts.iter().filter(|c| c.contains(": Ordering =")) {
                let decl = &c[..c.find('=').unwrap()];
                if !shipped.iter().any(|s| s.starts_with(decl)) {
                    bad.push(format!(
                        "tests/genmc/cases: case {name} sets {c:?}, which GENMC_SHIPPED does not list for {harness}"
                    ));
                }
            }
            if *expected != "pass" {
                continue;
            }
            passes += 1;
            for s in &shipped {
                if !consts.contains(s) {
                    bad.push(format!("tests/genmc/cases: pass case {name} has no {s:?};"));
                }
            }
        }
        if passes == 0 {
            bad.push(format!("tests/genmc/cases: no pass case of {tmpl}"));
        }
    }

    for name in GENMC_NO_COPIES {
        if !genmc.join(name).exists() {
            bad.push(format!("GENMC_NO_COPIES: no tests/genmc/{name}"));
        }
    }

    for entry in std::fs::read_dir(&genmc).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.ends_with(".rs")
            || name == "prelude.rs"
            || GENMC_NO_COPIES.contains(&name.as_str())
        {
            continue;
        }
        if !GENMC_PINS.iter().any(|(h, _, _)| *h == name) {
            bad.push(format!("tests/genmc/{name}: no pins in GENMC_PINS"));
        }
        if !GENMC_SHIPPED.iter().any(|(h, _)| *h == name) {
            bad.push(format!("tests/genmc/{name}: no entry in GENMC_SHIPPED"));
        }
    }

    assert!(
        bad.is_empty(),
        "got = GenMC copies out of step with the source:\n  {}\n\
         want = each source line and its harness copy changed together (GENMC_PINS), \
         and every pass case on the source's orderings (GENMC_SHIPPED)",
        bad.join("\n  ")
    );
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
/// a function a pin falls in (a loom pin in `PINS` or a GenMC one in
/// `GENMC_PINS`) must be pinned itself, in either, or waived in `WAIVED`
/// with its reason. Pins only check that the copied lines are still there,
/// in order; this catches such a line inserted between them (a store, a
/// fence, a direct futex call) that the model has not copied. It does not
/// see an atomic whose ordering is a variable, or one reached through a
/// helper (a second `kick()`). Each waiver must match exactly one line, in
/// the function it names.
#[test]
fn loom_pins_cover_every_atomic_in_pinned_fns() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut groups: Vec<(&str, Vec<&str>)> = PINS.iter().map(|(f, l)| (*f, l.to_vec())).collect();
    groups.extend(
        GENMC_PINS
            .iter()
            .map(|(_, f, p)| (*f, p.iter().map(|(l, _)| *l).collect())),
    );
    let mut files: Vec<&str> = groups.iter().map(|(f, _)| *f).collect();
    files.sort();
    files.dedup();
    let mut unpinned = Vec::new();
    let mut waived_uses = vec![0; WAIVED.len()];
    for file in files {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        let src: Vec<&str> = text.lines().collect();
        let mut pinned = vec![false; src.len()];
        let mut in_fn = vec![false; src.len()];
        for (_, lines) in groups.iter().filter(|(f, _)| *f == file) {
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
         want = each copied into its loom model or GenMC harness and pinned in PINS or GENMC_PINS, \
         or listed in WAIVED with a reason",
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
