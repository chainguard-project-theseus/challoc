//! Evaluation-only counters (issue #27 attribution): capacity grows and
//! shrinks, GCs and interval GCs, printed on stderr at exit.
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static GROW: AtomicU64 = AtomicU64::new(0);
pub static SHRINK: AtomicU64 = AtomicU64::new(0);
pub static GC: AtomicU64 = AtomicU64::new(0);
pub static GC_INTERVAL: AtomicU64 = AtomicU64::new(0);
pub static SHRINK_FLUSH: AtomicU64 = AtomicU64::new(0);
pub static COMPACT_CEIL: AtomicU64 = AtomicU64::new(0);
pub static CREATE: AtomicU64 = AtomicU64::new(0);
pub static CREATE_LOW: AtomicU64 = AtomicU64::new(0);
pub static VISIT: AtomicU64 = AtomicU64::new(0);
pub static VISIT_LOW: AtomicU64 = AtomicU64::new(0);
pub static REFILL: AtomicU64 = AtomicU64::new(0);
pub static REFILL_SHORT: AtomicU64 = AtomicU64::new(0);
pub static REFILL_PHASED: AtomicU64 = AtomicU64::new(0);
pub static REFILL_SLABS: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Relaxed);
}

fn put(buf: &mut [u8; 512], at: &mut usize, s: &[u8]) {
    for &b in s {
        if *at < buf.len() {
            buf[*at] = b;
            *at += 1;
        }
    }
}

fn num(buf: &mut [u8; 512], at: &mut usize, mut v: u64) {
    let mut d = [0u8; 20];
    let mut i = d.len();
    loop {
        i -= 1;
        d[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    put(buf, at, &d[i..]);
}

extern "C" fn fini() {
    let mut buf = [0u8; 512];
    let mut at = 0;
    let items: [(&[u8], &AtomicU64); 13] = [
        (b"challoc-capstat grow=", &GROW),
        (b" shrink=", &SHRINK),
        (b" shrink_flush=", &SHRINK_FLUSH),
        (b" gc=", &GC),
        (b" gc_interval=", &GC_INTERVAL),
        (b" compact_ceil=", &COMPACT_CEIL),
        (b" create=", &CREATE),
        (b" create_low=", &CREATE_LOW),
        (b" visit=", &VISIT),
        (b" visit_low=", &VISIT_LOW),
        (b" refill=", &REFILL),
        (b" refill_short=", &REFILL_SHORT),
        (b" refill_phased=", &REFILL_PHASED),
    ];
    for (k, c) in items {
        put(&mut buf, &mut at, k);
        num(&mut buf, &mut at, c.load(Relaxed));
    }
    put(&mut buf, &mut at, b"\n");
    crate::sys::write_all(crate::sys::STDERR_FILENO, &buf[..at]);
}

#[used]
#[unsafe(link_section = ".fini_array")]
static FINI: extern "C" fn() = fini;
