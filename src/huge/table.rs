//! The huge tier's descriptor table: page number to [`HugeDesc`] in an
//! open-addressing hash table with linear probing and backward-shift
//! deletion, grown by doubling. Storage comes from the metadata region;
//! every access is under the huge lock.

use core::alloc::Layout;
use core::ptr;

use crate::meta::{self, Meta};

use super::HugeDesc;

/// `log2` of the initial descriptor table capacity.
const LG_INITIAL_TABLE: u32 = 10;

/// One hash table slot: the key it was inserted under and the descriptor.
/// A null descriptor means empty.
#[derive(Clone, Copy)]
struct Entry {
    key: usize,
    desc: *mut HugeDesc,
}

/// Open-addressing hash table with linear probing and backward-shift
/// deletion. Load is kept at or below one half by [`Table::ensure`].
pub(super) struct Table {
    entries: *mut Entry,
    lg_cap: u32,
    pub(super) count: usize,
    /// Storage came from `Meta::alloc` (and can be freed) rather than a
    /// reservation.
    binned: bool,
}

impl Table {
    /// Storage layout of a table of `1 << lg_cap` entries: the one source
    /// for `storage` and `release`.
    const fn layout(lg_cap: u32) -> Layout {
        match Layout::from_size_align(core::mem::size_of::<Entry>() << lg_cap, 16) {
            Ok(l) => l,
            Err(_) => panic!("huge: descriptor table layout"),
        }
    }

    fn storage(meta: &Meta, page: usize, lg_cap: u32) -> Option<(*mut Entry, bool)> {
        let layout = Table::layout(lg_cap);
        if layout.size() <= meta::MAX_BINNED {
            let p = meta.alloc_zeroed(layout);
            if p.is_null() {
                None
            } else {
                Some((p as *mut Entry, true))
            }
        } else {
            let p = meta.reserve_committed(layout.size(), page)?;
            Some((p as *mut Entry, false))
        }
    }

    /// Give the storage back if it was binned. A reserved table cannot be
    /// given back; it is leaked (at most the size of the final table in
    /// total).
    fn release(&self, meta: &Meta) {
        if self.binned {
            meta.free(self.entries as *mut u8, Table::layout(self.lg_cap));
        }
    }

    pub(super) fn new(meta: &Meta, page: usize) -> Option<Table> {
        let (entries, binned) = Table::storage(meta, page, LG_INITIAL_TABLE)?;
        Some(Table {
            entries,
            lg_cap: LG_INITIAL_TABLE,
            count: 0,
            binned,
        })
    }

    #[inline]
    pub(super) fn cap(&self) -> usize {
        1 << self.lg_cap
    }

    #[inline]
    fn hash(&self, key: usize) -> usize {
        ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - self.lg_cap)) as usize
    }

    #[inline]
    fn slot(&self, i: usize) -> &Entry {
        debug_assert!(i < self.cap());
        // SAFETY: `i < cap` and the storage is `cap` entries that only this
        // table (behind the huge lock) touches.
        unsafe { &*self.entries.add(i) }
    }

    /// Store `e` in slot `i`.
    #[inline]
    fn set_slot(&mut self, i: usize, e: Entry) {
        debug_assert!(i < self.cap());
        // SAFETY: as in `slot`.
        unsafe { self.entries.add(i).write(e) }
    }

    /// Slot index holding `key`, if present. Terminates because the table
    /// always has an empty slot (`insert` keeps `count < cap`).
    fn find(&self, key: usize) -> Option<usize> {
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        loop {
            let e = self.slot(i);
            if e.desc.is_null() {
                return None;
            }
            if e.key == key {
                return Some(i);
            }
            i = (i + 1) & mask;
        }
    }

    pub(super) fn get(&self, key: usize) -> *mut HugeDesc {
        match self.find(key) {
            Some(i) => self.slot(i).desc,
            None => ptr::null_mut(),
        }
    }

    /// Insert `key`, which must not be present. Aborts rather than fill
    /// the last slot: a probe for an absent key ends at an empty slot, so
    /// a full table would spin in `find`. `ensure` keeps production load
    /// at or below one half; only a free after a failed growth (its
    /// `ensure` is best effort) can get here.
    pub(super) fn insert(&mut self, key: usize, desc: *mut HugeDesc) {
        assert!(self.count + 1 < self.cap(), "huge: descriptor table full");
        let mask = self.cap() - 1;
        let mut i = self.hash(key);
        loop {
            let e = self.slot(i);
            if e.desc.is_null() {
                self.set_slot(i, Entry { key, desc });
                self.count += 1;
                return;
            }
            assert!(e.key != key, "huge: key inserted twice");
            i = (i + 1) & mask;
        }
    }

    /// Remove `key`, which must be present.
    pub(super) fn remove(&mut self, key: usize) {
        let Some(mut i) = self.find(key) else {
            panic!("huge: descriptor key {key} missing from the table");
        };
        let mask = self.cap() - 1;
        // Backward shift: pull later entries of the same probe chain into
        // the hole unless their ideal slot lies past it.
        let mut j = i;
        loop {
            j = (j + 1) & mask;
            let e = *self.slot(j);
            if e.desc.is_null() {
                break;
            }
            let ideal = self.hash(e.key);
            let stays = if i <= j {
                i < ideal && ideal <= j
            } else {
                i < ideal || ideal <= j
            };
            if !stays {
                self.set_slot(i, e);
                i = j;
            }
        }
        self.set_slot(
            i,
            Entry {
                key: 0,
                desc: ptr::null_mut(),
            },
        );
        self.count = self
            .count
            .checked_sub(1)
            .unwrap_or_else(|| panic!("huge: descriptor table count underflow"));
    }

    /// Make room for `extra` more keys at a load of one half or less. On
    /// failure the table is unchanged and still works, only fuller.
    pub(super) fn ensure(&mut self, meta: &Meta, page: usize, extra: usize) -> Result<(), ()> {
        if (self.count + extra) * 2 <= self.cap() {
            return Ok(());
        }
        let mut lg = self.lg_cap + 1;
        while (self.count + extra) * 2 > (1usize << lg) {
            lg += 1;
        }
        let (entries, binned) = Table::storage(meta, page, lg).ok_or(())?;
        let old = Table {
            entries: self.entries,
            lg_cap: self.lg_cap,
            count: self.count,
            binned: self.binned,
        };
        *self = Table {
            entries,
            lg_cap: lg,
            count: 0,
            binned,
        };
        for i in 0..old.cap() {
            let e = *old.slot(i);
            if !e.desc.is_null() {
                self.insert(e.key, e.desc);
            }
        }
        old.release(meta);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huge::tests::{Env, page_size};
    use std::vec::Vec;

    /// Backward-shift deletion across the wrap from the last slot to slot
    /// 0, on the table alone: keys that hash to the last slot and to slot
    /// 0 collide there, and removing any of them must pull the rest of the
    /// chain back over the wrap while an entry already in its ideal slot
    /// stays put. The descriptors are opaque non-null pointers; the table
    /// never dereferences them.
    #[test]
    fn table_removal_shifts_across_the_wrap_around() {
        let env = Env::new();
        let page = page_size();
        let mut t = Table::new(&env.meta, page).expect("table");
        let last = t.cap() - 1;
        let key_with_hash =
            |t: &Table, h: usize, from: usize| (from..).find(|&k| t.hash(k) == h).unwrap();
        // Three keys hashing to the last slot, one hashing to slot 0 and
        // one hashing to slot 1.
        let a = key_with_hash(&t, last, 1);
        let b = key_with_hash(&t, last, a + 1);
        let d = key_with_hash(&t, last, b + 1);
        let c = key_with_hash(&t, 0, 1);
        let e = key_with_hash(&t, 1, 1);
        let desc = |k: usize| k as *mut HugeDesc;

        // Insertion order a, b, c, d: a takes the last slot, b wraps to 0,
        // c (ideal 0) is pushed to 1, d (ideal last) to 2.
        for k in [a, b, c, d] {
            t.insert(k, desc(k));
        }
        assert_eq!(
            [t.find(a), t.find(b), t.find(c), t.find(d)],
            [Some(last), Some(0), Some(1), Some(2)]
        );
        // Removing the head of the chain pulls b back across the wrap, c
        // into its ideal slot and d after it.
        t.remove(a);
        assert_eq!(
            [t.find(b), t.find(c), t.find(d)],
            [Some(last), Some(0), Some(1)]
        );
        assert!(t.find(a).is_none() && t.get(a).is_null());
        assert_eq!(t.count, 3);
        for k in [b, c, d] {
            assert_eq!(t.get(k), desc(k));
        }
        // Removing the entry at slot 0 with the chain continuing past it.
        t.remove(c);
        assert_eq!([t.find(b), t.find(d)], [Some(last), Some(0)]);
        // An entry already in its ideal slot is not moved into the hole
        // before it: e sits at 1, its ideal, and stays when d leaves 0.
        t.insert(e, desc(e));
        assert_eq!(t.find(e), Some(1));
        t.remove(d);
        assert_eq!(
            [t.find(b), t.find(e), t.find(d)],
            [Some(last), Some(1), None]
        );
        t.remove(b);
        assert_eq!([t.find(b), t.find(e)], [None, Some(1)]);
        assert_eq!(t.count, 1);
        // The wrap slot is reusable.
        t.insert(a, desc(a));
        assert_eq!(t.find(a), Some(last));
        assert_eq!(t.count, 2);
    }

    /// `Table::ensure` fails cleanly when the metadata region cannot hold
    /// the grown table: the old table is untouched and still answers, and
    /// keeps taking keys up to its capacity.
    #[test]
    fn table_growth_failure_leaves_the_table_working() {
        // 64 KiB of metadata: the initial 16 KiB table fits and so does
        // its first doubling (32 KiB); the second (64 KiB) does not.
        let env = Env::with_meta_len(64 * 1024);
        let page = page_size();
        let mut t = Table::new(&env.meta, page).expect("table");
        assert_eq!(t.cap(), 1 << LG_INITIAL_TABLE);
        let desc = |k: usize| k as *mut HugeDesc;
        let mut keys = Vec::new();
        let mut grew = 0;
        loop {
            let cap = t.cap();
            if t.ensure(&env.meta, page, 1).is_err() {
                break;
            }
            if t.cap() > cap {
                grew += 1;
            }
            let k = keys.len() + 1;
            t.insert(k, desc(k));
            keys.push(k);
            assert!(keys.len() <= 1 << 16, "the table never ran out of metadata");
        }
        assert_eq!(grew, 1, "one doubling fits, the second does not");
        assert_eq!(t.cap(), 2 << LG_INITIAL_TABLE);
        assert_eq!(t.count, keys.len());
        assert_eq!(
            keys.len() * 2,
            t.cap(),
            "ensure refused exactly at half load"
        );
        for &k in &keys {
            assert_eq!(t.get(k), desc(k), "key {k} lost in the failed growth");
        }
        // The table is fuller than `ensure` would allow, and still works
        // up to one slot short of full (a probe for an absent key ends at
        // an empty slot; `insert` refuses to take the last one, see
        // `the_table_never_fills_its_last_slot`).
        let extra = t.cap() - t.count - 1;
        for k in keys.len() + 1..=keys.len() + extra {
            assert!(t.ensure(&env.meta, page, 1).is_err());
            t.insert(k, desc(k));
            assert!(t.find(k).is_some());
        }
        assert_eq!(t.count, t.cap() - 1);
        assert_eq!(t.get(0), ptr::null_mut());
        t.remove(1);
        assert_eq!(t.get(1), ptr::null_mut());
        assert_eq!(t.get(2), desc(2));
        assert_eq!(t.get(keys.len() + extra), desc(keys.len() + extra));
    }

    /// `insert` without `ensure` (as a free after a failed growth does)
    /// takes keys until exactly one slot is left; absent keys are still
    /// found absent (the probe ends at that slot), and the insert that
    /// would fill it aborts instead of leaving a table that `find` cannot
    /// terminate on.
    #[test]
    fn the_table_never_fills_its_last_slot() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size()).expect("table");
        let cap = t.cap();
        let desc = |k: usize| k as *mut HugeDesc;
        for k in 1..cap {
            t.insert(k, desc(k));
        }
        assert_eq!(t.count, cap - 1);
        for k in [0, cap, cap + 1, usize::MAX] {
            assert!(t.find(k).is_none(), "absent key {k} found");
            assert!(t.get(k).is_null());
        }
        for k in [1, cap / 2, cap - 1] {
            assert_eq!(t.get(k), desc(k));
        }
        // Removing and re-adding at the limit works: one slot stays free.
        t.remove(cap / 2);
        assert!(t.get(cap / 2).is_null());
        t.insert(cap / 2, desc(cap / 2));
        assert_eq!(t.get(cap / 2), desc(cap / 2));
        assert_eq!(t.count, cap - 1);
    }

    #[test]
    #[should_panic(expected = "descriptor table full")]
    fn the_table_aborts_before_its_last_slot_fills() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size()).expect("table");
        let cap = t.cap();
        for k in 1..cap {
            t.insert(k, k as *mut HugeDesc);
        }
        assert_eq!(t.count, cap - 1);
        t.insert(cap, cap as *mut HugeDesc);
    }

    #[test]
    #[should_panic(expected = "key inserted twice")]
    fn the_table_rejects_a_duplicate_key() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size()).unwrap();
        let d = 0x10 as *mut HugeDesc;
        t.insert(7, d);
        assert_eq!(t.count, 1);
        t.insert(7, d);
    }

    #[test]
    #[should_panic(expected = "descriptor table count underflow")]
    fn the_table_aborts_when_its_count_disagrees_with_its_entries() {
        let env = Env::new();
        let mut t = Table::new(&env.meta, page_size()).unwrap();
        t.insert(7, 0x10 as *mut HugeDesc);
        t.count = 0;
        t.remove(7);
    }
}
