//! Transfer batches (issue #33, the `xfer_batches` key): a small bounded
//! stack per `(arena, class)` of whole batches of slots a foreign cache
//! flushed, which the arena's next cache refill takes before it scans the
//! bitmaps.
//!
//! A batch is a third holder of set bits beside the program and the
//! caches (design 4): its slots stay marked allocated, so `curregs`
//! (`claimed - released`) counts them as it counts cached slots, and
//! `free`'s bit test treats them as it treats cached ones. A batch enters
//! only after [`ClassState::xfer_check`] has seen every slot's bit set and
//! every slab owned by this arena, and only strictly ascending (no
//! duplicate, grouped by slab), so the checks a release would have made
//! are made at the push; the refill that takes it hands the slots out as
//! a claim would. The decay pass releases batches older than one step of
//! the curve ([`ClassState::xfer_drain`]), and `purge_all` every batch, so
//! address order and compaction see the slots again within a step.
//!
//! The stack is behind the class lock: a push or pop is one lock hold per
//! batch (a refill takes that lock anyway), and the fork handlers that
//! take every class lock quiesce it. Batches live in metadata buffers of
//! the foreign cache's capacity; a push swaps the cache's full stack for
//! an empty buffer, so the push copies nothing.

use core::alloc::Layout;
use core::ptr;
use core::sync::atomic::Ordering;

use super::ClassState;
use super::abort::double_free;
use crate::meta::Meta;

/// The most batches one `(arena, class)` holds (the key's upper bound).
pub const XFER_MAX: usize = 4;

/// The stack. Every field is read and written under the class lock.
#[repr(C)]
pub(super) struct Xfer {
    /// Capacity of every buffer, in slots (the foreign cache's).
    cap: u32,
    /// Full batches: `bufs[..n]`, oldest first.
    n: u32,
    /// Empty buffers: `spare[..nspare]`.
    nspare: u32,
    bufs: [*mut *mut u8; XFER_MAX],
    lens: [u32; XFER_MAX],
    /// Coarse monotonic ns of each batch's push.
    at: [u64; XFER_MAX],
    spare: [*mut *mut u8; XFER_MAX],
}

fn buf_layout(cap: u32) -> Layout {
    Layout::from_size_align(8 * cap as usize, 16).expect("xfer buffer layout")
}

impl ClassState {
    /// Full batches waiting (a relaxed read of the count the lock holder
    /// publishes): the refill's and the decay pass's gate.
    #[inline]
    pub fn xfer_pending(&self) -> bool {
        self.xfer_n.load(Ordering::Relaxed) != 0
    }

    /// The stack, created on first use for buffers of `cap` slots. Null
    /// when the metadata region is exhausted.
    fn xfer_obj(&self, meta: &Meta, cap: u32) -> *mut Xfer {
        let x = self.xfer.load(Ordering::Acquire);
        if !x.is_null() {
            return x;
        }
        let layout = Layout::new::<Xfer>();
        let x = meta.alloc(layout) as *mut Xfer;
        if x.is_null() {
            return x;
        }
        // SAFETY: a fresh metadata object, exclusively ours until published.
        unsafe {
            ptr::write(
                x,
                Xfer {
                    cap,
                    n: 0,
                    nspare: 0,
                    bufs: [ptr::null_mut(); XFER_MAX],
                    lens: [0; XFER_MAX],
                    at: [0; XFER_MAX],
                    spare: [ptr::null_mut(); XFER_MAX],
                },
            );
        }
        match self
            .xfer
            .compare_exchange(ptr::null_mut(), x, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => x,
            Err(other) => {
                meta.free(x as *mut u8, layout);
                other
            }
        }
    }

    /// Whether `ptrs` (strictly ascending slot starts of this class) can
    /// enter this arena's stack: `false` when a slab belongs to another
    /// arena. Every slot's bit must be set, as a release would require: a
    /// clear bit is a double or invalid free and aborts here.
    pub fn xfer_check(&self, ptrs: &[*mut u8]) -> bool {
        let t = self.table();
        let mut cur = usize::MAX;
        for &p in ptrs {
            let (slab, slot) = t.locate(p);
            if slab != cur {
                if t.owner(slab) != self.arena {
                    return false;
                }
                cur = slab;
            }
            // SAFETY: `owner` checked the watermark; `locate` the slot.
            if !unsafe { t.is_set_unchecked(slab, slot) } {
                double_free(p as usize, self.class as usize, slab, slot);
            }
        }
        true
    }

    /// Push the full stack `buf` (`len` checked slots, `cap` its capacity)
    /// as a batch, at most `limit` held. Returns the empty buffer of `cap`
    /// slots that replaces it, null when the batch was not taken (the
    /// stack is full, the capacity differs, metadata is exhausted).
    pub fn xfer_push(
        &self,
        meta: &Meta,
        buf: *mut *mut u8,
        len: usize,
        cap: u32,
        limit: usize,
        now: u64,
    ) -> *mut *mut u8 {
        let x = self.xfer_obj(meta, cap);
        if x.is_null() {
            return ptr::null_mut();
        }
        let limit = limit.min(XFER_MAX) as u32;
        let mut fresh: *mut *mut u8 = ptr::null_mut();
        loop {
            let g = self.inner.lock();
            // SAFETY: the stack is ours while the class lock is held.
            unsafe {
                if (*x).cap != cap || (*x).n >= limit {
                    drop(g);
                    if !fresh.is_null() {
                        meta.free(fresh as *mut u8, buf_layout(cap));
                    }
                    return ptr::null_mut();
                }
                let repl = if (*x).nspare > 0 {
                    (*x).nspare -= 1;
                    (*x).spare[(*x).nspare as usize]
                } else if !fresh.is_null() && (*x).n + (*x).nspare < limit {
                    core::mem::replace(&mut fresh, ptr::null_mut())
                } else if fresh.is_null() && (*x).n + (*x).nspare < limit {
                    drop(g);
                    fresh = meta.alloc(buf_layout(cap)) as *mut *mut u8;
                    if fresh.is_null() {
                        return ptr::null_mut();
                    }
                    continue;
                } else {
                    drop(g);
                    if !fresh.is_null() {
                        meta.free(fresh as *mut u8, buf_layout(cap));
                    }
                    return ptr::null_mut();
                };
                let i = (*x).n as usize;
                (*x).bufs[i] = buf;
                (*x).lens[i] = len as u32;
                (*x).at[i] = now;
                (*x).n += 1;
                self.xfer_n.store((*x).n, Ordering::Relaxed);
                drop(g);
                if !fresh.is_null() {
                    meta.free(fresh as *mut u8, buf_layout(cap));
                }
                return repl;
            }
        }
    }

    /// Pop the newest batch into `out`, highest address first (the order
    /// a refill leaves, lowest on top), and return its length; 0 when
    /// there is none or it does not fit.
    pub fn xfer_pop(&self, out: &mut [*mut u8]) -> usize {
        let x = self.xfer.load(Ordering::Acquire);
        if x.is_null() {
            return 0;
        }
        let _g = self.inner.lock();
        // SAFETY: the stack is ours while the class lock is held; a batch
        // buffer holds `lens[i] <= cap` slots.
        unsafe {
            let n = (*x).n as usize;
            if n == 0 {
                return 0;
            }
            let len = (*x).lens[n - 1] as usize;
            if len > out.len() {
                return 0;
            }
            let b = (*x).bufs[n - 1];
            for (k, o) in out[..len].iter_mut().enumerate() {
                *o = *b.add(len - 1 - k);
            }
            (*x).n -= 1;
            (*x).spare[(*x).nspare as usize] = b;
            (*x).nspare += 1;
            self.xfer_n.store((*x).n, Ordering::Relaxed);
            len
        }
    }

    /// Release to the bitmaps every batch pushed at or before `cutoff`
    /// (`u64::MAX`: all of them) and return the push time of the oldest
    /// one left (0 when none). The emptied buffers go back as spares, or
    /// to `meta` if a push replaced them meanwhile.
    pub fn xfer_drain(&self, meta: &Meta, cutoff: u64) -> u64 {
        if !self.xfer_pending() {
            return 0;
        }
        let x = self.xfer.load(Ordering::Acquire);
        let mut out = [(ptr::null_mut::<*mut u8>(), 0u32); XFER_MAX];
        let mut k = 0;
        let left;
        {
            let _g = self.inner.lock();
            // SAFETY: the stack is ours while the class lock is held.
            unsafe {
                let n = (*x).n as usize;
                while k < n && (*x).at[k] <= cutoff {
                    out[k] = ((*x).bufs[k], (*x).lens[k]);
                    k += 1;
                }
                for i in k..n {
                    (*x).bufs[i - k] = (*x).bufs[i];
                    (*x).lens[i - k] = (*x).lens[i];
                    (*x).at[i - k] = (*x).at[i];
                }
                (*x).n = (n - k) as u32;
                left = if n > k { (*x).at[0] } else { 0 };
                self.xfer_n.store((*x).n, Ordering::Relaxed);
            }
        }
        #[cfg(xfer_diag)]
        for _ in 0..k {
            diag::bump(6);
        }
        for &(b, len) in &out[..k] {
            // SAFETY: a batch buffer of `len` checked slots, taken out of
            // the stack above, so nothing else reads it.
            let ptrs = unsafe { core::slice::from_raw_parts(b, len as usize) };
            self.release(ptrs);
        }
        if k != 0 {
            let _g = self.inner.lock();
            // SAFETY: as above. A push during the release may have
            // allocated a buffer in place of a taken one, so `n + nspare`
            // (the buffers the stack owns) is kept within `XFER_MAX`.
            unsafe {
                for &(b, _) in &out[..k] {
                    if (((*x).n + (*x).nspare) as usize) < XFER_MAX {
                        (*x).spare[(*x).nspare as usize] = b;
                        (*x).nspare += 1;
                    } else {
                        meta.free(b as *mut u8, buf_layout((*x).cap));
                    }
                }
            }
        }
        left
    }
}

/// Temporary diagnostics (`--cfg xfer_diag`).
#[cfg(xfer_diag)]
pub mod diag {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub static C: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    pub const NAMES: [&str; 8] = [
        "flushes", "unsorted", "owner", "full", "pushed", "popped", "drained", "",
    ];
    static LAST: AtomicU64 = AtomicU64::new(0);
    #[inline]
    pub fn bump(i: usize) {
        C[i].fetch_add(1, Relaxed);
    }
    pub fn maybe_print(now: u64) {
        let l = LAST.load(Relaxed);
        if now < l + 1_000_000_000 || LAST.compare_exchange(l, now, Relaxed, Relaxed).is_err() {
            return;
        }
        let mut buf = [0u8; 256];
        let mut n = 0;
        for (i, name) in NAMES.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            for &b in name.as_bytes().iter().chain(b"=") {
                buf[n] = b;
                n += 1;
            }
            let mut v = C[i].load(Relaxed);
            let mut d = [0u8; 20];
            let mut k = 0;
            loop {
                d[k] = b'0' + (v % 10) as u8;
                k += 1;
                v /= 10;
                if v == 0 {
                    break;
                }
            }
            while k > 0 {
                k -= 1;
                buf[n] = d[k];
                n += 1;
            }
            buf[n] = b' ';
            n += 1;
        }
        buf[n] = b'\n';
        n += 1;
        crate::sys::write_all(2, &buf[..n]);
    }
}
