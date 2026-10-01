//! Prefaulted fresh slabs (the `purge` module doc). The pass runs on the
//! background thread; the commit path calls [`note_commit`]; init calls
//! [`configure`].

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::heap::Shared;
use crate::layout::NCLASSES;

/// `prefault:true` with the background thread (`Config::prefault_active`).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The kernel takes `MADV_POPULATE_WRITE` (else the touch fallback).
static POPULATE: AtomicBool = AtomicBool::new(false);
/// A pass is running: its own commits wake nobody.
static IN_PASS: AtomicBool = AtomicBool::new(false);
/// Passes run, slabs prefaulted, bytes populated, passes skipped for
/// want of headroom, `MADV_POPULATE_WRITE` calls that failed and fell
/// back to the touch.
static PASSES: AtomicU64 = AtomicU64::new(0);
static SLABS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
/// Wakes the commit path issued.
static WAKES: AtomicU64 = AtomicU64::new(0);
/// Wakes the claim path issued (`ClassState::plan`: a refill took the
/// arena's last idle slab of a class).
static TAKEN: AtomicU64 = AtomicU64::new(0);
/// Passes that found at least one class refilling (and so re-armed
/// the poll).
static ACTIVE: AtomicU64 = AtomicU64::new(0);

/// The background thread's sleep while classes keep refilling: at
/// 100 k Valkey SETs of a few hundred bytes per second a hot class
/// takes a 64 KiB slab every millisecond or so, and a pass is a few
/// microseconds, so a millisecond keeps the next slab ready without
/// the thread spinning; a quiet pass stops the poll.
pub const POLL_NS: u64 = 1_000_000;

/// The most slabs a pass keeps ready per `(arena, class)`: the
/// class's refills since the last pass in slabs, at least one and at
/// most this many (`ClassState::prefault_ahead`; one slab per class
/// is the floor, the burst rate raises it). Eight 64 KiB
/// slabs is half a megabyte resident ahead of a class taking several
/// slabs per millisecond, and nothing more for a class that does not.
pub const MAX_AHEAD: u64 = 8;

/// Init: the key's outcome and the probe's.
pub fn configure(active: bool, populate_write: bool) {
    POPULATE.store(populate_write, Ordering::Relaxed);
    ENABLED.store(active, Ordering::Release);
}

/// Whether the pass runs at all.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `MADV_POPULATE_WRITE` is the populate in use (else the touch).
pub fn uses_populate_write() -> bool {
    POPULATE.load(Ordering::Relaxed)
}

/// The counters: passes, slabs, bytes, skipped passes, fallbacks, wakes.
pub fn stats() -> Stats {
    Stats {
        passes: PASSES.load(Ordering::Relaxed),
        slabs: SLABS.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        skipped: SKIPPED.load(Ordering::Relaxed),
        fallbacks: FALLBACKS.load(Ordering::Relaxed),
        wakes: WAKES.load(Ordering::Relaxed),
        active: ACTIVE.load(Ordering::Relaxed),
        taken: TAKEN.load(Ordering::Relaxed),
    }
}

/// The counters, for the report and `stats.prefault.*`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub passes: u64,
    pub slabs: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub fallbacks: u64,
    pub wakes: u64,
    pub active: u64,
    pub taken: u64,
}

/// A fresh slab went into service (`SlabTable::commit_slab`): wake the
/// thread for a pass, unless the pass itself is committing. Off: one
/// relaxed load.
#[inline]
pub fn note_commit() {
    if enabled() && !IN_PASS.load(Ordering::Relaxed) {
        WAKES.fetch_add(1, Ordering::Relaxed);
        super::background::wake();
    }
}

/// A refill took the arena's last idle slab of a class
/// (`ClassState::plan`, the claim side):
/// wake the thread so the class's next slab is committed now rather
/// than at the next poll or the next fresh commit. The caller tests
/// [`enabled`] first.
#[inline]
pub fn note_taken() {
    if enabled() {
        TAKEN.fetch_add(1, Ordering::Relaxed);
        super::background::wake();
    }
}

/// One pass over every `(arena, class)`: the classes that refilled
/// since the last pass get their next fresh slabs committed and
/// populated (`ClassState::prefault_ahead`), as many as the class
/// refilled since the last pass in slabs, at least one and at most
/// [`MAX_AHEAD`], when the cgroup policy shows headroom. Returns
/// whether any class had refilled (the
/// thread then polls, `POLL_NS`). Background thread only.
pub fn pass(shared: &Shared) -> bool {
    if !enabled() {
        return false;
    }
    if !shared.policy.prefault_allowed() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    PASSES.fetch_add(1, Ordering::Relaxed);
    IN_PASS.store(true, Ordering::Relaxed);
    let page = shared.regions.page_size();
    let mut due = false;
    for arena in shared.arenas.iter() {
        for class in 0..NCLASSES {
            let cs = arena.class(class);
            let delta = cs.prefault_delta();
            if delta == 0 {
                continue;
            }
            due = true;
            // The class's refills since the last pass, in slabs: the
            // pipeline to keep ready (`MAX_AHEAD`).
            let ahead = delta.div_ceil(cs.class().nslots() as u64).clamp(1, MAX_AHEAD) as usize;
            let n = cs.prefault_ahead(&shared.meta, ahead, |addr, len| populate(addr, len, page));
            SLABS.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    IN_PASS.store(false, Ordering::Relaxed);
    if due {
        ACTIVE.fetch_add(1, Ordering::Relaxed);
    }
    due
}

/// Fault `[addr, addr + len)` in writable: `MADV_POPULATE_WRITE` where
/// the probe found it, else one write of zero per page (the slab is
/// fresh, unpublished and zero, so the write changes nothing and is
/// this thread's alone). Counted.
pub fn populate(addr: usize, len: usize, page: usize) {
    BYTES.fetch_add(len as u64, Ordering::Relaxed);
    if POPULATE.load(Ordering::Relaxed) && madvise_populate_write(addr, len) {
        return;
    }
    if POPULATE.load(Ordering::Relaxed) {
        FALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
    let mut off = 0;
    while off < len {
        // SAFETY: a committed, read-write slab this thread holds the
        // class lock for and nothing else references yet; a zero over
        // a zero.
        unsafe { ((addr + off) as *mut u8).write_volatile(0) };
        off += page;
    }
}

fn madvise_populate_write(addr: usize, len: usize) -> bool {
    // SAFETY: a committed range of a class region this process owns;
    // populating changes residency, never contents.
    unsafe { crate::sys::madvise(addr, len, crate::sys::MADV_POPULATE_WRITE) }.is_ok()
}

#[cfg(test)]
pub(crate) fn reset_for_test(active: bool, populate_write: bool) {
    configure(active, populate_write);
    for c in [&PASSES, &SLABS, &BYTES, &SKIPPED, &FALLBACKS, &WAKES, &ACTIVE, &TAKEN] {
        c.store(0, Ordering::Relaxed);
    }
}
