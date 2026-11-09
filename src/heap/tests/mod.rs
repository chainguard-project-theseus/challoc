//! Heap tests checked against the allocator's own state, mostly over a
//! synthetic layout (`testutil::TestShared`): the cache pop and push, refill
//! and flush, the fast and slow paths, the double-free checks, the class
//! lines and the configuration keys the heap reads. A heap test whose
//! claim is about the process or the hardware (thread-local storage,
//! thread exit, fork, several threads, pointer authentication) belongs in
//! `linux_tests.rs`.

use super::compact::Verdict;
use super::flush::{flag_sort, group_by_slab};
use super::*;
use crate::arena::Arena;
use crate::class::ClassStats;
use crate::config::Config;
use crate::layout::{Kind, class_of};
use crate::slab::State;
use crate::sys::pac;
use crate::testutil::{TestShared, align_up, page_size, shared, xorshift};
use std::boxed::Box;
use std::collections::HashSet;
use std::vec::Vec;
use vstd::prelude::Tracked;

/// Default capacity of a cache for a class up to 128 bytes, and the
/// refill/flush batch that goes with it.
const CAP: usize = MAX_CAP as usize;
const HALF: usize = CAP / 2;

/// The message of a caught panic (`catch_unwind`'s payload is a
/// `String` from `panic!` with arguments, a `&str` without).
pub(crate) fn panic_message(e: Box<dyn std::any::Any + Send>) -> std::string::String {
    e.downcast_ref::<std::string::String>()
        .cloned()
        .or_else(|| {
            e.downcast_ref::<&str>()
                .map(|m| std::string::String::from(*m))
        })
        .expect("a message")
}

pub(crate) fn class_with(size: usize) -> usize {
    CLASSES.iter().position(|c| c.size() == size).unwrap()
}

pub(crate) struct TestHeap {
    h: *mut Heap,
    s: &'static Shared,
}

impl TestHeap {
    pub(crate) fn new(s: &'static Shared) -> TestHeap {
        let (h, _, _) = Heap::create(s, Tracked::assume_new());
        assert!(!h.is_null());
        TestHeap { h, s }
    }

    // The test heap stands in for the thread's heap (`heap_for_thread`).
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn heap(&self) -> &mut Heap {
        // SAFETY: `h` is the live heap `new` created, freed only by `destroy`;
        // each test holds one reference from here at a time.
        unsafe { &mut *self.h }
    }

    pub(crate) fn alloc(&self, c: usize) -> *mut u8 {
        let p = self.heap().alloc(
            self.s,
            c,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p.is_null());
        p
    }

    pub(crate) fn alloc_zeroed(&self, c: usize) -> (*mut u8, bool) {
        let (p, z) = self.heap().alloc_zeroed(
            self.s,
            c,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p.is_null());
        (p, z)
    }

    pub(crate) fn free(&self, p: *mut u8) {
        self.heap().free(
            self.s,
            p,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }

    pub(crate) fn alloc_huge(&self, size: usize, align: usize) -> (*mut u8, bool) {
        let (p, zero, _) = self.heap().alloc_huge(
            self.s,
            size,
            align,
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        (p, zero)
    }

    pub(crate) fn stats(&self) -> ThreadStats {
        self.heap().stats()
    }

    fn gc(&self) {
        self.heap().gc(self.s, Tracked::assume_new())
    }

    pub(crate) fn flush_all(&self) {
        self.heap().flush_all(self.s, Tracked::assume_new())
    }

    fn ticks(&self) -> u32 {
        self.heap().slow_ticks()
    }

    fn cache(&self, c: usize) -> &CacheHeader {
        self.heap().cache(c).expect("cache exists")
    }

    /// The entries of `c`'s cache as plain pointers (authenticated
    /// where the stack is signed), bottom first.
    fn cached_plain(&self, c: usize) -> Vec<*mut u8> {
        let k = self.cache(c);
        k.cached()
            .iter()
            .map(|&e| k.plain(e, self.s.pac()))
            .collect()
    }

    /// The run of `c`'s cache as plain pointers.
    fn run_plain(&self, c: usize) -> Vec<*mut u8> {
        let k = self.cache(c);
        k.run().iter().map(|&e| k.plain(e, self.s.pac())).collect()
    }

    fn len(&self, c: usize) -> usize {
        self.cache(c).len()
    }

    pub(crate) fn destroy(self) {
        // SAFETY: `h` came from `Heap::create` on `s`, and `destroy` consumes the
        // `TestHeap`, so nothing reaches the heap afterwards.
        unsafe { Heap::destroy(self.h, self.s, Tracked::assume_new(), Tracked::assume_new()) }
    }
}

/// Sum of `live` over every committed slab of a class.
pub(crate) fn live(s: &Shared, c: usize) -> usize {
    let cs = &s.arena0.classes[c];
    (0..cs.committed_slabs())
        .filter(|&i| !cs.is_guard_slab(i))
        .map(|i| cs.desc(i).live())
        .sum()
}

/// Fill `slabs` slabs of `c` on `t`, free a random `frac` of the blocks
/// and return the survivors: the post-delete residue of the churn
/// probe, with the holes in the slabs (the cache is flushed).
/// Slots the churn models fill: 40 slabs of the 64-slot 1 KiB class the
/// models were written for (`bench/probes/churn.c`), 640 of today's
/// 4-slot slabs.
const MODEL_SLOTS: usize = 2560;

fn residue(t: &TestHeap, c: usize, slabs: usize, frac_per_mille: u64, seed: u64) -> Vec<*mut u8> {
    let nslots = CLASSES[c].nslots();
    let mut st = seed;
    let mut keep = Vec::new();
    for p in (0..slabs * nslots).map(|_| t.alloc(c)).collect::<Vec<_>>() {
        if xorshift(&mut st) % 1000 < frac_per_mille {
            t.free(p)
        } else {
            keep.push(p)
        }
    }
    t.flush_all();
    keep
}

mod aborts;
mod arenas;
mod cache;
mod class_lines;
mod compaction;
mod config_keys;
mod gc;
mod hardening;
mod zeroed_prefix;
