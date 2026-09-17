//! The reports (design.md sections 13.1 and 16): jemalloc's
//! `malloc_stats_print`, glibc's `malloc_info` (XML) and `malloc_stats`.
//!
//! Everything is formatted with `core::fmt` into a small stack buffer and
//! handed on in chunks: to the caller's `write_cb`, to the `malloc_message`
//! hook, to a `FILE *` through `fwrite`, or to stderr with `write(2)`.
//! Nothing allocates. The figures are *live* (`stats::collect`, not the
//! `epoch` snapshot), as jemalloc's report and glibc's `malloc_info` are.
//!
//! `malloc_stats_print(write_cb, cbopaque, opts)` follows jemalloc's general
//! shape: a general section (version, build and run-time options, kernel
//! features, totals), then one section per arena with its bins. `opts`
//! characters are jemalloc's: `J` emits JSON instead (under a top-level
//! `"jemalloc"` object, which is what existing consumers of the JSON form
//! look for), `g` omits the general section, `a` omits the arenas, `b` omits
//! the per-arena bin tables; the other jemalloc letters are accepted and
//! ignored. The cgroup section is a placeholder until Phase 2.

use core::ffi::{c_char, c_int, c_void};
use core::fmt::{self, Write};

use super::jemalloc::malloc_write;
use super::mallctl::VERSION_STR;
use crate::QUANTUM;
use crate::arena::Arena;
use crate::config::{KEYS, Value};
use crate::heap::Shared;
use crate::layout::NBINS;
use crate::purge;
use crate::stats::{self, ArenaStats, Snapshot};

/// `void (*write_cb)(void *cbopaque, const char *s)`.
pub type WriteCb = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

/// Where a report goes.
enum Target {
    /// The caller's callback, or with `None` the `malloc_message` path.
    Callback(WriteCb, *mut c_void),
    /// glibc's `malloc_info` stream.
    File(*mut libc::FILE),
    /// `write(2)` to stderr, bypassing any hook (glibc's `malloc_stats`).
    Stderr,
}

/// A `fmt::Write` that buffers up to 1023 bytes and forwards them,
/// NUL-terminated where the target wants a C string.
struct Sink {
    target: Target,
    buf: [u8; 1024],
    len: usize,
}

impl Sink {
    fn new(target: Target) -> Sink {
        Sink { target, buf: [0; 1024], len: 0 }
    }

    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }
        match self.target {
            Target::Callback(Some(cb), opaque) => {
                self.buf[self.len] = 0;
                // SAFETY: the caller's `void (*)(void *, const char *)`; the
                // chunk is NUL-terminated within the buffer.
                unsafe { cb(opaque, self.buf.as_ptr() as *const c_char) };
            }
            Target::Callback(None, _) => malloc_write(&self.buf[..self.len]),
            Target::File(f) => {
                // SAFETY: `f` is the caller's open stream; `len` bytes are
                // readable.
                unsafe { libc::fwrite(self.buf.as_ptr() as *const c_void, 1, self.len, f) };
            }
            Target::Stderr => crate::sys::write_all(crate::sys::STDERR_FILENO, &self.buf[..self.len]),
        }
        self.len = 0;
    }
}

impl Write for Sink {
    fn write_str(&mut self, mut s: &str) -> fmt::Result {
        while !s.is_empty() {
            let room = self.buf.len() - 1 - self.len;
            if room == 0 {
                self.flush();
                continue;
            }
            let n = s.len().min(room);
            self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
            s = &s[n..];
        }
        Ok(())
    }
}

/// Parsed `opts`.
#[derive(Default)]
struct Opts {
    json: bool,
    no_general: bool,
    no_arenas: bool,
    no_bins: bool,
}

impl Opts {
    fn parse(opts: *const c_char) -> Opts {
        let mut o = Opts::default();
        if opts.is_null() {
            return o;
        }
        for i in 0..64 {
            // SAFETY: the caller's NUL-terminated option string; reads stop
            // at the terminator or after 64 bytes.
            let b = unsafe { opts.add(i).read() } as u8;
            match b {
                0 => break,
                b'J' => o.json = true,
                b'g' => o.no_general = true,
                b'a' => o.no_arenas = true,
                b'b' => o.no_bins = true,
                _ => {}
            }
        }
        o
    }
}

/// Every existing arena with its live figures.
fn for_each_arena(shared: &Shared, mut f: impl FnMut(&Arena, &ArenaStats)) {
    for arena in shared.arenas.iter() {
        let s = stats::collect_arena(shared, arena);
        f(arena, &s);
    }
}

fn decay_ms(ns: u64) -> i64 {
    if ns == purge::NEVER { -1 } else { (ns / 1_000_000) as i64 }
}

// ---------------------------------------------------------------------------
// Text report.
// ---------------------------------------------------------------------------

fn text_general<W: Write>(out: &mut W, shared: &Shared, snap: &Snapshot) -> fmt::Result {
    // jemalloc's banner, not "challoc statistics": consumers key on it (Valkey's
    // `MEMORY MALLOC-STATS` coverage test asserts `*jemalloc*`), just as the
    // JSON form keeps its top-level "jemalloc" object. The `Version:` line
    // that follows says which library produced the report.
    writeln!(out, "___ Begin jemalloc statistics ___")?;
    writeln!(out, "Version: \"{VERSION_STR}\"")?;
    writeln!(out, "Build-time option settings")?;
    writeln!(out, "  config.debug: false")?;
    writeln!(out, "  config.stats: true")?;
    writeln!(out, "  config.quantum: {QUANTUM}")?;
    writeln!(out, "Run-time option settings")?;
    for key in KEYS {
        match shared.config.get(key.as_bytes()) {
            Some(Value::Bool(b)) => writeln!(out, "  opt.{key}: {b}")?,
            Some(Value::Int(i)) => writeln!(out, "  opt.{key}: {i}")?,
            Some(Value::Uint(u)) => writeln!(out, "  opt.{key}: {u}")?,
            Some(Value::Str(s)) => writeln!(out, "  opt.{key}: \"{s}\"")?,
            None => {}
        }
    }
    writeln!(out, "Kernel features: {}", super::features())?;
    writeln!(out, "Background thread: {}", if purge::background::present() { "present" } else { "absent" })?;
    writeln!(
        out,
        "Arenas: {} ({} automatic), explicit caches: {}",
        snap.narenas,
        shared.arenas.auto(),
        shared.tcaches.len()
    )?;
    writeln!(out, "Quantum size: {QUANTUM}")?;
    writeln!(out, "Page size: {}", shared.regions.page_size())?;
    let t = &snap.totals;
    writeln!(
        out,
        "Allocated: {}, active: {}, metadata: {}, resident: {}, mapped: {}, retained: {}",
        t.allocated, t.active, t.metadata, t.resident, t.mapped, t.retained
    )?;
    let h = &snap.huge;
    writeln!(
        out,
        "Huge: {} runs ({} pages), {} empty extents ({} pages), {} purged extents ({} pages), high-water page {}",
        h.runs, h.run_pages, h.empty, h.empty_pages, h.purged, h.purged_pages, h.next_page
    )?;
    let m = &snap.meta;
    writeln!(
        out,
        "Metadata: reserved {}, committed {}, allocated {}, bumped {}, tables {}",
        m.len, m.committed, m.allocated, m.bumped, m.reserved
    )?;
    writeln!(out, "cgroup: not tracked")
}

fn text_arena<W: Write>(out: &mut W, arena: &Arena, s: &ArenaStats, bins: bool) -> fmt::Result {
    writeln!(out, "arenas[{}]:", arena.index())?;
    writeln!(out, "  dirty_decay_ms: {}", decay_ms(arena.decay_ns()))?;
    writeln!(out, "  pactive: {}, pdirty: {}, pmuzzy: {}", s.pactive, s.pdirty, s.pmuzzy)?;
    writeln!(out, "                            allocated       active        dirty     retained")?;
    writeln!(
        out,
        "  small:               {:>14} {:>12} {:>12} {:>12}",
        s.small_allocated, s.small_active, s.small_dirty, s.small_retained
    )?;
    writeln!(
        out,
        "  large:               {:>14} {:>12} {:>12} {:>12}  ({} huge runs)",
        s.large_allocated,
        s.large_active,
        s.large_dirty + s.huge_dirty,
        s.large_retained + s.huge_retained,
        s.large_runs
    )?;
    writeln!(out, "  metadata: {}, resident: {}, mapped: {}, retained: {}", s.metadata, s.resident, s.mapped, s.retained)?;
    if !bins {
        return Ok(());
    }
    // The bins, then the slab-allocated classes above BIN_MAX that the
    // mallctl namespace reports as large (layout::BIN_MAX).
    // The last five columns are challoc's compaction diagnostics (design
    // 6.4): slots claimed by cache refills and by compaction rebuilds,
    // caches that entered and left compact mode, probes declined by the
    // refill-rate gate.
    writeln!(
        out,
        "  bins:        size ind    nregs  slab_size   curregs  curslabs   nonfull     empty    purged   refilled  compacted   c_on  c_off  gated"
    )?;
    let mut large_header = false;
    for (j, b) in s.bins.iter().enumerate() {
        if b.curslabs + b.empty_slabs + b.purged_slabs == 0 {
            continue;
        }
        if j >= NBINS && !large_header {
            large_header = true;
            writeln!(out, "  large classes (slab-allocated, not bins):")?;
        }
        writeln!(
            out,
            "  {:>13} {:>3} {:>8} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10} {:>6} {:>6} {:>6}",
            b.size,
            j,
            b.nregs,
            b.slab_size,
            b.curregs,
            b.curslabs,
            b.nonfull_slabs,
            b.empty_slabs,
            b.purged_slabs,
            b.refilled,
            b.compacted,
            b.compact_on,
            b.compact_off,
            b.gate_refill
        )?;
    }
    slab_histogram(out, arena, s)
}

/// Per-class occupancy histogram of this arena's non-empty slabs (a
/// diagnostic for the compaction work of Phase 1 acceptance step 3): the
/// slab count per eighth of occupancy (`live * 8 / nslots`, the last
/// bucket is full slabs), the slabs whose `live` disagrees with their
/// bitmap's set bits, and the non-full slabs missing from the partial set.
/// One class lock hold per non-full slab; only classes with non-full slabs
/// are listed.
fn slab_histogram<W: Write>(out: &mut W, arena: &Arena, s: &ArenaStats) -> fmt::Result {
    let mut header = false;
    for (j, b) in s.bins.iter().enumerate() {
        if b.nonfull_slabs == 0 {
            continue;
        }
        let cs = arena.class(j);
        let cls = cs.class();
        let nslots = cls.nslots();
        let mut hist = [0usize; 9];
        let (mut mismatched, mut unlisted, mut slabs) = (0usize, 0usize, 0usize);
        for i in 0..cs.committed_slabs() {
            if cs.is_guard_slab(i) {
                continue;
            }
            let d = cs.desc(i);
            if d.arena() != arena.index() {
                continue;
            }
            let live = d.live();
            if live == 0 {
                continue;
            }
            slabs += 1;
            hist[(live * 8 / nslots).min(8)] += 1;
            let bm = d.bitmap(cls);
            let set: usize = (0..bm.words()).map(|w| bm.load(w).count_ones() as usize).sum();
            if set.saturating_sub(bm.words() * 64 - nslots) != live {
                mismatched += 1;
            }
            if live < nslots && !cs.is_partial(i) {
                unlisted += 1;
            }
        }
        if !header {
            header = true;
            writeln!(out, "  slab occupancy (non-empty slabs per eighth of nslots, last = full):")?;
        }
        writeln!(
            out,
            "  {:>13} {:>3} slabs {:>6}  hist {:?}  live!=bits {}  nonfull not in partial set {}",
            b.size, j, slabs, hist, mismatched, unlisted
        )?;
    }
    Ok(())
}

fn text_report<W: Write>(out: &mut W, shared: &Shared, o: &Opts) -> fmt::Result {
    let snap = stats::collect(shared);
    if !o.no_general {
        text_general(out, shared, &snap)?;
    }
    if !o.no_arenas {
        let mut r = Ok(());
        for_each_arena(shared, |a, s| {
            if r.is_ok() {
                r = text_arena(out, a, s, !o.no_bins);
            }
        });
        r?;
    }
    writeln!(out, "--- End jemalloc statistics ---")
}

// ---------------------------------------------------------------------------
// JSON report.
// ---------------------------------------------------------------------------

fn json_report<W: Write>(out: &mut W, shared: &Shared, o: &Opts) -> fmt::Result {
    let snap = stats::collect(shared);
    writeln!(out, "{{")?;
    writeln!(out, "\t\"jemalloc\": {{")?;
    writeln!(out, "\t\t\"version\": \"{VERSION_STR}\",")?;
    writeln!(out, "\t\t\"implementation\": \"challoc\",")?;
    if !o.no_general {
        writeln!(out, "\t\t\"config\": {{\"debug\": false, \"stats\": true, \"quantum\": {QUANTUM}}},")?;
        writeln!(out, "\t\t\"opt\": {{")?;
        let mut first = true;
        for key in KEYS {
            let Some(v) = shared.config.get(key.as_bytes()) else { continue };
            if !first {
                writeln!(out, ",")?;
            }
            first = false;
            match v {
                Value::Bool(b) => write!(out, "\t\t\t\"{key}\": {b}")?,
                Value::Int(i) => write!(out, "\t\t\t\"{key}\": {i}")?,
                Value::Uint(u) => write!(out, "\t\t\t\"{key}\": {u}")?,
                Value::Str(s) => write!(out, "\t\t\t\"{key}\": \"{s}\"")?,
            }
        }
        writeln!(out)?;
        writeln!(out, "\t\t}},")?;
        writeln!(
            out,
            "\t\t\"arenas\": {{\"narenas\": {}, \"nbins\": {NBINS}, \"quantum\": {QUANTUM}, \"page\": {}}},",
            snap.narenas,
            shared.regions.page_size()
        )?;
        writeln!(out, "\t\t\"background_thread\": {{\"present\": {}}},", purge::background::present())?;
        let t = &snap.totals;
        writeln!(
            out,
            "\t\t\"stats\": {{\"allocated\": {}, \"active\": {}, \"metadata\": {}, \"resident\": {}, \"mapped\": {}, \"retained\": {}}}{}",
            t.allocated,
            t.active,
            t.metadata,
            t.resident,
            t.mapped,
            t.retained,
            if o.no_arenas { "" } else { "," }
        )?;
    }
    if !o.no_arenas {
        writeln!(out, "\t\t\"stats.arenas\": {{")?;
        let mut r = Ok(());
        let mut first = true;
        for_each_arena(shared, |a, s| {
            if r.is_err() {
                return;
            }
            r = (|| {
                if !first {
                    writeln!(out, ",")?;
                }
                first = false;
                writeln!(out, "\t\t\t\"{}\": {{", a.index())?;
                writeln!(out, "\t\t\t\t\"dirty_decay_ms\": {},", decay_ms(a.decay_ns()))?;
                writeln!(out, "\t\t\t\t\"pactive\": {}, \"pdirty\": {}, \"pmuzzy\": {},", s.pactive, s.pdirty, s.pmuzzy)?;
                writeln!(
                    out,
                    "\t\t\t\t\"small\": {{\"allocated\": {}, \"active\": {}, \"dirty\": {}, \"retained\": {}}},",
                    s.small_allocated, s.small_active, s.small_dirty, s.small_retained
                )?;
                writeln!(
                    out,
                    "\t\t\t\t\"large\": {{\"allocated\": {}, \"active\": {}, \"dirty\": {}, \"retained\": {}, \"runs\": {}}},",
                    s.large_allocated,
                    s.large_active,
                    s.large_dirty + s.huge_dirty,
                    s.large_retained + s.huge_retained,
                    s.large_runs
                )?;
                writeln!(
                    out,
                    "\t\t\t\t\"metadata\": {}, \"resident\": {}, \"mapped\": {}, \"retained\": {}{}",
                    s.metadata,
                    s.resident,
                    s.mapped,
                    s.retained,
                    if o.no_bins { "" } else { "," }
                )?;
                if !o.no_bins {
                    writeln!(out, "\t\t\t\t\"bins\": [")?;
                    for (j, b) in s.bins[..NBINS].iter().enumerate() {
                        writeln!(
                            out,
                            "\t\t\t\t\t{{\"size\": {}, \"nregs\": {}, \"slab_size\": {}, \"curregs\": {}, \"curslabs\": {}, \"nonfull_slabs\": {}, \"empty_slabs\": {}, \"purged_slabs\": {}}}{}",
                            b.size,
                            b.nregs,
                            b.slab_size,
                            b.curregs,
                            b.curslabs,
                            b.nonfull_slabs,
                            b.empty_slabs,
                            b.purged_slabs,
                            if j + 1 == NBINS { "" } else { "," }
                        )?;
                    }
                    writeln!(out, "\t\t\t\t]")?;
                }
                write!(out, "\t\t\t}}")
            })();
        });
        r?;
        writeln!(out)?;
        writeln!(out, "\t\t}}")?;
    }
    writeln!(out, "\t}}")?;
    writeln!(out, "}}")
}

/// `malloc_stats_print(write_cb, cbopaque, opts)`.
pub fn malloc_stats_print(write_cb: WriteCb, cbopaque: *mut c_void, opts: *const c_char) {
    let shared = super::shared();
    let o = Opts::parse(opts);
    let mut sink = Sink::new(Target::Callback(write_cb, cbopaque));
    let _ = if o.json { json_report(&mut sink, shared, &o) } else { text_report(&mut sink, shared, &o) };
    sink.flush();
}

// ---------------------------------------------------------------------------
// glibc.
// ---------------------------------------------------------------------------

fn xml_heap<W: Write>(out: &mut W, nr: usize, s: &ArenaStats) -> fmt::Result {
    writeln!(out, "<heap nr=\"{nr}\">")?;
    writeln!(out, "<sizes>")?;
    let mut count = 0usize;
    for b in s.bins[..NBINS].iter() {
        if b.curregs == 0 {
            continue;
        }
        count += b.curregs;
        writeln!(out, "<size from=\"{}\" to=\"{}\" total=\"{}\" count=\"{}\"/>", b.size, b.size, b.curregs * b.size, b.curregs)?;
    }
    writeln!(out, "</sizes>")?;
    writeln!(out, "<total type=\"fast\" count=\"0\" size=\"0\"/>")?;
    writeln!(out, "<total type=\"rest\" count=\"{count}\" size=\"{}\"/>", s.small_allocated)?;
    writeln!(out, "<system type=\"current\" size=\"{}\"/>", s.resident)?;
    writeln!(out, "<system type=\"max\" size=\"{}\"/>", s.mapped)?;
    writeln!(out, "<aspace type=\"total\" size=\"{}\"/>", s.mapped)?;
    writeln!(out, "<aspace type=\"mprotect\" size=\"{}\"/>", s.retained)?;
    writeln!(out, "</heap>")
}

fn xml_report<W: Write>(out: &mut W, shared: &Shared) -> fmt::Result {
    writeln!(out, "<malloc version=\"1\">")?;
    let mut r = Ok(());
    for_each_arena(shared, |a, s| {
        if r.is_ok() {
            r = xml_heap(out, a.index() as usize, s);
        }
    });
    r?;
    let all = stats::collect_all(shared);
    let count: usize = all.bins[..NBINS].iter().map(|b| b.curregs).sum();
    let large_count: usize = all.large_runs + all.bins[NBINS..].iter().map(|b| b.curregs).sum::<usize>();
    writeln!(out, "<total type=\"fast\" count=\"0\" size=\"0\"/>")?;
    writeln!(out, "<total type=\"rest\" count=\"{count}\" size=\"{}\"/>", all.small_allocated)?;
    writeln!(out, "<total type=\"mmap\" count=\"{large_count}\" size=\"{}\"/>", all.large_allocated)?;
    writeln!(out, "<system type=\"current\" size=\"{}\"/>", all.resident)?;
    writeln!(out, "<system type=\"max\" size=\"{}\"/>", all.mapped)?;
    writeln!(out, "<aspace type=\"total\" size=\"{}\"/>", all.mapped)?;
    writeln!(out, "<aspace type=\"mprotect\" size=\"{}\"/>", all.retained)?;
    writeln!(out, "</malloc>")
}

/// glibc's `malloc_info(options, stream)`: the `<malloc version="1">`
/// document with one `<heap>` per arena and the totals. `options` must be
/// 0 and `stream` non-null, else `-1` with `errno = EINVAL`, as in glibc.
pub fn malloc_info(options: c_int, stream: *mut libc::FILE) -> c_int {
    if options != 0 || stream.is_null() {
        // SAFETY: `__errno_location` returns the calling thread's `errno`.
        unsafe { *libc::__errno_location() = 22 };
        return -1;
    }
    let shared = super::shared();
    let mut sink = Sink::new(Target::File(stream));
    let _ = xml_report(&mut sink, shared);
    sink.flush();
    0
}

/// glibc's `malloc_stats()`: a few lines on stderr in glibc's layout, with
/// challoc's meaning of each figure (see `glibc.rs` for the mapping).
pub fn malloc_stats() {
    let shared = super::shared();
    let mut sink = Sink::new(Target::Stderr);
    let _ = (|| -> fmt::Result {
        let mut r = Ok(());
        for_each_arena(shared, |a, s| {
            if r.is_ok() {
                r = (|| {
                    writeln!(sink, "Arena {}:", a.index())?;
                    writeln!(sink, "system bytes     = {:>10}", s.resident.saturating_sub(s.large_allocated))?;
                    writeln!(sink, "in use bytes     = {:>10}", s.small_allocated)
                })();
            }
        });
        r?;
        let all = stats::collect_all(shared);
        writeln!(sink, "Total (incl. mmap):")?;
        writeln!(sink, "system bytes     = {:>10}", all.resident)?;
        writeln!(sink, "in use bytes     = {:>10}", all.allocated())?;
        writeln!(sink, "max mmap regions = {:>10}", all.large_runs)?;
        writeln!(sink, "max mmap bytes   = {:>10}", all.large_allocated)
    })();
    sink.flush();
}

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::{c_char, c_int, c_void};

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_stats_print(write_cb: super::WriteCb, cbopaque: *mut c_void, opts: *const c_char) {
        super::malloc_stats_print(write_cb, cbopaque, opts)
    }

    #[unsafe(export_name = "je_malloc_stats_print")]
    pub extern "C" fn je_malloc_stats_print(write_cb: super::WriteCb, cbopaque: *mut c_void, opts: *const c_char) {
        super::malloc_stats_print(write_cb, cbopaque, opts)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_info(options: c_int, stream: *mut libc::FILE) -> c_int {
        super::malloc_info(options, stream)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_stats() {
        super::malloc_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::libc::{free, malloc};
    use std::string::String;
    use std::sync::Mutex;
    use std::vec::Vec;

    static CAPTURE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    /// Held across a whole `report`: the callback appends one chunk at a
    /// time under `CAPTURE`'s lock, so two tests reporting at once would
    /// interleave their chunks (seen as a text report that began with
    /// another test's JSON report).
    static REPORT: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn capture(opaque: *mut c_void, s: *const c_char) {
        assert_eq!(opaque as usize, 0xC0FFEE);
        let s = unsafe { core::ffi::CStr::from_ptr(s) };
        CAPTURE.lock().unwrap().extend_from_slice(s.to_bytes());
    }

    fn report(opts: &str) -> String {
        let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        CAPTURE.lock().unwrap().clear();
        let c = std::ffi::CString::new(opts).unwrap();
        malloc_stats_print(Some(capture), 0xC0FFEE as *mut c_void, if opts.is_empty() { core::ptr::null() } else { c.as_ptr() });
        String::from_utf8(CAPTURE.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn text_report_has_every_section_and_live_figures() {
        let live: Vec<*mut u8> = (0..100).map(|_| malloc(700)).collect();
        let text = report("");
        assert!(text.starts_with("___ Begin jemalloc statistics ___\n"), "{text}");
        assert!(text.ends_with("--- End jemalloc statistics ---\n"), "{text}");
        for needle in [
            "Version: \"5.3.0-0-gchalloc\"",
            "config.quantum:",
            "opt.dirty_decay_ms: ",
            "opt.thp: \"never\"",
            "Kernel features: page_size=",
            "Background thread: ",
            "Arenas: ",
            "Allocated: ",
            "Huge: ",
            "Metadata: reserved ",
            "cgroup: not tracked",
            "arenas[0]:",
            "  dirty_decay_ms: ",
            "  small:",
            "  large:",
            "  bins:        size ind",
        ] {
            assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
        }
        // One bins table per built arena; this thread's heap is on
        // whichever automatic arena `Arenas::assign` gave it, so the live
        // blocks show in the class's row of that table.
        let size = crate::layout::class_size_of(700).unwrap();
        // The slab histogram rows start with the size too but are not all
        // numeric; the bins rows are.
        let rows: Vec<Vec<usize>> = text
            .lines()
            .filter(|l| l.trim_start().starts_with(&std::format!("{size} ")))
            .filter_map(|l| l.split_whitespace().map(|w| w.parse::<usize>().ok()).collect::<Option<Vec<usize>>>())
            .collect();
        assert!(!rows.is_empty(), "bin row for the live class in:\n{text}");
        let curregs: usize = rows.iter().map(|cols| cols[4]).sum();
        assert!(curregs >= 100, "curregs in {rows:?}");
        // The Allocated figure is live, not the epoch snapshot.
        let line = text.lines().find(|l| l.starts_with("Allocated: ")).unwrap();
        let allocated: usize = line.split(", ").next().unwrap().trim_start_matches("Allocated: ").parse().unwrap();
        assert!(allocated >= 100 * size);
        for p in live {
            free(p);
        }
        // Option letters.
        let g = report("g");
        assert!(!g.contains("Version:") && g.contains("arenas[0]:"), "{g}");
        let a = report("a");
        assert!(a.contains("Version:") && !a.contains("arenas[0]:"), "{a}");
        let b = report("b");
        assert!(b.contains("arenas[0]:") && !b.contains("  bins:"), "{b}");
        let ga = report("gab");
        assert_eq!(ga, "--- End jemalloc statistics ---\n");
    }

    #[test]
    fn json_report_is_well_formed_enough_to_parse_by_hand() {
        let text = report("J");
        assert!(text.starts_with("{\n\t\"jemalloc\": {\n"), "{text}");
        assert!(text.trim_end().ends_with('}'));
        assert!(text.contains("\"version\": \"5.3.0-0-gchalloc\""));
        assert!(text.contains("\"opt\": {"));
        assert!(text.contains("\"stats\": {\"allocated\": "));
        assert!(text.contains("\"stats.arenas\": {"));
        assert!(text.contains("\"0\": {"));
        assert!(text.contains("\"bins\": ["));
        // Braces and brackets balance; no trailing commas before a closer.
        let mut depth = 0i32;
        for ch in text.chars() {
            match ch {
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0, "unbalanced: {text}");
        let compact: String = text.split_whitespace().collect();
        assert!(!compact.contains(",}") && !compact.contains(",]"), "trailing comma in:\n{text}");
        // One bins array of NBINS entries per arena section in this very
        // text (other tests create arenas concurrently).
        assert_eq!(text.matches("\"size\": ").count(), NBINS * text.matches("\"pactive\": ").count());
        let ga = report("Jga");
        assert!(ga.contains("\"version\"") && !ga.contains("\"opt\"") && !ga.contains("\"stats.arenas\""), "{ga}");
    }

    #[test]
    fn stats_print_without_callback_uses_the_message_hook() {
        use crate::api::jemalloc::set_message_hook_for_tests;
        static HOOKED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
            assert!(opaque.is_null());
            HOOKED.lock().unwrap().extend_from_slice(unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes());
        }
        set_message_hook_for_tests(Some(hook));
        malloc_stats_print(None, core::ptr::null_mut(), core::ptr::null());
        set_message_hook_for_tests(None);
        let got = String::from_utf8(HOOKED.lock().unwrap().clone()).unwrap();
        assert!(got.starts_with("___ Begin jemalloc statistics ___\n"), "{got}");
        assert!(got.ends_with("--- End jemalloc statistics ---\n"));
    }

    #[test]
    fn malloc_info_writes_glibc_xml_to_the_stream() {
        let live: Vec<*mut u8> = (0..50).map(|_| malloc(3000)).collect();
        let big = malloc(1 << 20);
        let f = unsafe { libc::tmpfile() };
        assert!(!f.is_null());
        assert_eq!(malloc_info(0, f), 0);
        unsafe { libc::rewind(f) };
        let mut buf = std::vec![0u8; 1 << 16];
        let n = unsafe { libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), f) };
        unsafe { libc::fclose(f) };
        let xml = String::from_utf8(buf[..n].to_vec()).unwrap();
        assert!(xml.starts_with("<malloc version=\"1\">\n"), "{xml}");
        assert!(xml.ends_with("</malloc>\n"), "{xml}");
        assert!(xml.contains("<heap nr=\"0\">"));
        let size = crate::layout::class_size_of(3000).unwrap();
        assert!(xml.contains(&std::format!("<size from=\"{size}\" to=\"{size}\" total=\"")), "{xml}");
        assert!(xml.contains("<total type=\"mmap\" count=\""));
        assert_eq!(xml.matches("<heap nr=").count(), xml.matches("</heap>").count());
        assert_eq!(malloc_info(1, f), -1);
        assert_eq!(unsafe { *libc::__errno_location() }, 22);
        assert_eq!(malloc_info(0, core::ptr::null_mut()), -1);
        free(big);
        for p in live {
            free(p);
        }
        // glibc's malloc_stats just has to run and stay on stderr.
        malloc_stats();
    }
}
