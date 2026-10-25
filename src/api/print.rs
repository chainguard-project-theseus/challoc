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
//! ignored. The cgroup line and object are the sampled figures of
//! `sys::cgroup` (design 11).

use core::ffi::{CStr, c_char, c_int, c_void};
use core::fmt::{self, Write};

use super::jemalloc::malloc_write;
use super::mallctl::VERSION_STR;
use crate::QUANTUM;
use crate::arena::Arena;
use crate::config::{INERT_KEYS, KEYS, PercpuArena, Value};
use crate::heap::Shared;
use crate::layout::{CLASSES, NBINS};
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
        Sink {
            target,
            buf: [0; 1024],
            len: 0,
        }
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
                // readable. `fwrite` may itself call `malloc` (glibc
                // allocates a stream's buffer on its first write), which
                // re-enters this allocator from inside `malloc_info`: that
                // is safe because no allocator lock is held while the sink
                // flushes (`stats::collect_arena` returns its figures before
                // they are formatted, and `Arenas::iter` is a lock-free scan
                // of the table) and the report's own state is this stack
                // buffer.
                unsafe { libc::fwrite(self.buf.as_ptr() as *const c_void, 1, self.len, f) };
            }
            Target::Stderr => {
                crate::sys::write_all(crate::sys::STDERR_FILENO, &self.buf[..self.len])
            }
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
            // Cut on a character boundary: slicing `s` inside a multi-byte
            // character would panic in the middle of a report.
            let mut n = s.len().min(room);
            while !s.is_char_boundary(n) {
                n -= 1;
            }
            if n == 0 {
                self.flush();
                continue;
            }
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
    /// `opts` is the caller's option string: null, or readable up to and
    /// including its NUL terminator (jemalloc's contract); at most 64
    /// bytes are read, so a longer string is truncated, never overrun.
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

/// A configuration value's name (`config::Value::Str`, the `thp` key).
fn word(s: &CStr) -> &str {
    s.to_str().unwrap_or("?")
}

fn decay_ms(ns: u64) -> i64 {
    if ns == purge::NEVER {
        -1
    } else {
        (ns / 1_000_000) as i64
    }
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
    writeln!(out, "Version: \"{}\"", VERSION_STR)?;
    writeln!(out, "Build-time option settings")?;
    writeln!(out, "  config.debug: false")?;
    writeln!(out, "  config.stats: true")?;
    writeln!(out, "  config.quantum: {QUANTUM}")?;
    writeln!(out, "Run-time option settings")?;
    for key in KEYS {
        let note = if key == &"thp" && shared.policy.thp_forced() {
            " (accepted, no effect; forced to never by a cgroup limit under 1 GiB)"
        } else if key == &"percpu_arena" && shared.config.percpu_arena != PercpuArena::Disabled {
            " (accepted, no effect; caches are per thread)"
        } else {
            ""
        };
        match shared.config.get(key.as_bytes()) {
            Some(Value::Bool(b)) => writeln!(out, "  opt.{}: {}{}", key, b, note)?,
            Some(Value::Int(i)) => writeln!(out, "  opt.{}: {}{}", key, i, note)?,
            Some(Value::Uint(u)) => writeln!(out, "  opt.{}: {}{}", key, u, note)?,
            Some(Value::Size(n)) => writeln!(out, "  opt.{}: {}{}", key, n, note)?,
            Some(Value::Str(s)) => writeln!(out, "  opt.{}: \"{}\"{}", key, word(s), note)?,
            None => {}
        }
    }
    // An inert key (`config::INERT_KEYS`) is parsed and validated but has
    // no field and is read by nothing; say so, so a report never suggests
    // the feature is on.
    for key in INERT_KEYS {
        writeln!(out, "  opt.{}: accepted, no effect", key)?;
    }
    writeln!(out, "Kernel and CPU features: {}", super::feature_line())?;
    writeln!(
        out,
        "Sealed ranges: {} of {}",
        super::sealed_ranges(),
        shared.regions.never_committed().1
    )?;
    writeln!(
        out,
        "Background thread: {}",
        if purge::background::present() {
            "present"
        } else if purge::background::auto() {
            "absent (lazy start armed)"
        } else {
            "absent"
        }
    )?;
    writeln!(
        out,
        "Arenas: {} ({} automatic), explicit caches: {}",
        snap.narenas,
        shared.arenas.auto(),
        shared.tcaches.len()
    )?;
    writeln!(out, "Quantum size: {}", QUANTUM)?;
    writeln!(out, "Page size: {}", shared.regions.page_size())?;
    if let Some((lo, hi)) = crate::layout::page_sized_classes() {
        // Design 3: on a kernel whose page is larger than a class's table
        // slab, the class's slab is the page.
        let base = |f: fn(usize, usize) -> usize| {
            (lo..=hi)
                .map(|c| crate::layout::BASE_CLASSES[c].slab_size())
                .reduce(f)
                .unwrap_or(0)
        };
        writeln!(
            out,
            "Page-sized slabs: classes {}..{} ({}..{} bytes) at {} bytes ({}..{} in the table)",
            lo,
            hi,
            CLASSES.table()[lo].size(),
            CLASSES.table()[hi].size(),
            CLASSES.table()[lo].slab_size(),
            base(usize::min),
            base(usize::max)
        )?;
    }
    let t = &snap.totals;
    writeln!(
        out,
        "Allocated: {}, active: {}, metadata: {}, resident: {}, mapped: {}, retained: {}",
        t.allocated, t.active, t.metadata, t.resident, t.mapped, t.retained
    )?;
    let h = &snap.huge;
    write!(
        out,
        "Huge: {} runs ({} pages), {} empty extents ({} pages), {} purged extents ({} pages), high-water page {}",
        h.runs, h.run_pages, h.empty, h.empty_pages, h.purged, h.purged_pages, h.next_page
    )?;
    // The huge tier's own decay period (`huge_dirty_decay_ms`, design 7),
    // named only when it differs from arena 0's, which it follows otherwise.
    if shared.huge_decay_ns() != shared.decay_ns() {
        write!(
            out,
            ", dirty_decay_ms {} (huge_dirty_decay_ms)",
            decay_ms(shared.huge_decay_ns())
        )?;
    }
    writeln!(out)?;
    let m = &snap.meta;
    writeln!(
        out,
        "Metadata: reserved {}, committed {}, writable {}, allocated {}, bumped {}, tables {}, small tables {}",
        m.len, m.committed, m.writable, m.allocated, m.bumped, m.reserved, m.eager
    )?;
    // Design 11: the figures as last sampled (a sample is taken here if the
    // 10 ms window allows), or why nothing is tracked.
    let cgroup = shared.cgroup_now();
    cgroup.sample(crate::sys::now_coarse_ns());
    writeln!(out, "{}", cgroup)?;
    policy_line(out, shared)?;
    advice_line(out)?;
    thp_line(out)
}

/// The `thp:` line (design 7, 10 and 17): the key,
/// the mode in force and why, the thresholds, and the ranges advised so
/// far per region kind.
fn thp_line<W: Write>(out: &mut W) -> fmt::Result {
    let d = crate::thp::decision();
    let s = crate::thp::stats();
    write!(
        out,
        "thp: {} -> {} ({})",
        word(crate::thp::key().name()),
        d.mode.name(),
        d.reason.text()
    )?;
    // Design 17 (`MADV_NOHUGEPAGE` under `never`): `never` covers the
    // class and metadata reservations; the huge region follows the host's
    // global mode (named at the end of the line).
    if crate::thp::is_never() {
        write!(
            out,
            " nohuge {} (classes, metadata; huge follows host)",
            s.nohuge_ranges
        )?;
    }
    if d.mode != crate::thp::Mode::Off {
        write!(out, " slab_min {}", 1usize << d.lg_slab_min)?;
        write!(out, " pmd {}", 1usize << d.lg_pmd)?;
        match d.lg_meta {
            crate::thp::NO_SIZE => write!(out, " meta off")?,
            lg => write!(out, " meta {}", 1usize << lg)?,
        }
        write!(out, " huge {}", if d.huge { "on" } else { "off" })?;
    }
    write!(
        out,
        " class_ranges {} ({} bytes, {} chunks) huge_ranges {} meta_ranges {} failures {}",
        s.class_ranges, s.class_bytes, s.chunks, s.huge_ranges, s.meta_ranges, s.failures
    )?;
    // Design 17: the host's global mode.
    writeln!(
        out,
        "; host {}",
        crate::thp::host_mode().map_or("none", crate::sys::thp_host::ThpSetting::name)
    )
}

/// The `cgroup policy:` line (design 11): the band as last evaluated and
/// what it gives arena 0.
fn policy_line<W: Write>(out: &mut W, shared: &Shared) -> fmt::Result {
    use crate::policy::{Band, FRAC_ONE};
    let p = &shared.policy;
    if !shared.cgroup_now().tracked() {
        return writeln!(out, "cgroup policy: off (not tracked)");
    }
    write!(out, "cgroup policy: ")?;
    match p.band() {
        Band::Unlimited => write!(out, "defaults (no limit)")?,
        Band::Low => write!(out, "under 70% (defaults, retention capped)")?,
        Band::Scaled(f) => write!(
            out,
            "70..90% (decay scaled, {}% into the band)",
            f as u64 * 100 / FRAC_ONE as u64
        )?,
        Band::Immediate => write!(out, "over 90% (purge at once, cache GC every tick)")?,
    }
    write!(
        out,
        " decay_ms {}",
        decay_ms(p.effective_decay_ns(shared.decay_ns()))
    )?;
    if shared.huge_decay_ns() != shared.decay_ns() {
        write!(
            out,
            " huge_decay_ms {}",
            decay_ms(p.effective_decay_ns(shared.huge_decay_ns()))
        )?;
    }
    match p.cap() {
        crate::sys::cgroup::UNLIMITED => write!(out, " retention_cap max")?,
        c => write!(out, " retention_cap {c}")?,
    }
    writeln!(
        out,
        " keep {}% empty_bytes {} full_purges {}{} thp {}",
        p.keep16() as u64 * 100 / FRAC_ONE as u64,
        crate::policy::empty_bytes(shared),
        p.full_purges(),
        if p.full_purge_pending() {
            " (one pending)"
        } else {
            ""
        },
        if p.thp_forced() {
            "forced never (limit under 1 GiB)"
        } else {
            "as configured"
        }
    )
}

/// The `purge advice:` line (design 8, the two-advice purge): the advice
/// the next purge issues and why, the batches and bytes discarded with
/// each, whether `Purged` memory is still known zero, and the ranges and
/// bytes whose discard failed. Printed whether or not a cgroup is tracked.
fn advice_line<W: Write>(out: &mut W) -> fmt::Result {
    let (lazy, eager) = purge::batches();
    let (lazy_bytes, eager_bytes) = purge::discarded_bytes();
    let (failed, failed_bytes) = purge::failed_discards();
    writeln!(
        out,
        "purge advice: {} {} ({}) lazy_purges {} ({} bytes) eager_purges {} ({} bytes) purged_is_zero {} failed_discards {} ({} bytes)",
        purge::advice().name(),
        purge::advice().madv_name(),
        purge::advice_reason(),
        lazy,
        lazy_bytes,
        eager,
        eager_bytes,
        purge::purged_is_zero(),
        failed,
        failed_bytes,
    )
}

fn text_arena<W: Write>(out: &mut W, arena: &Arena, s: &ArenaStats, bins: bool) -> fmt::Result {
    writeln!(out, "arenas[{}]:", arena.index())?;
    writeln!(out, "  dirty_decay_ms: {}", decay_ms(arena.decay_ns()))?;
    writeln!(
        out,
        "  pactive: {}, pdirty: {}, pmuzzy: {}",
        s.pactive, s.pdirty, s.pmuzzy
    )?;
    writeln!(
        out,
        "                            allocated       active        dirty     retained"
    )?;
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
    writeln!(
        out,
        "  metadata: {}, resident: {}, mapped: {}, retained: {}",
        s.metadata, s.resident, s.mapped, s.retained
    )?;
    if !bins {
        return Ok(());
    }
    // The bins, then the slab-allocated classes above BIN_MAX that the
    // mallctl namespace reports as large (layout::BIN_MAX).
    // The last five columns are challoc's compaction diagnostics (design
    // 6.4): slots claimed by cache refills and by compaction rebuilds,
    // caches that entered and left compact mode, probes declined by the
    // refill-rate gate. `clamped` counts the times `curregs` was clamped
    // to the bin's capacity (`ClassState::bin_stats`).
    writeln!(
        out,
        "  bins:        size ind    nregs  slab_size   curregs  curslabs   nonfull     empty    purged   refilled  compacted   c_on  c_off  gated clamped"
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
            "  {:>13} {:>3} {:>8} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10} {:>6} {:>6} {:>6} {:>7}",
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
            b.gate_refill,
            b.clamped
        )?;
    }
    slab_histogram(out, arena, s)
}

/// Per-class occupancy histogram of this arena's non-empty slabs (a
/// diagnostic for the compaction of design 6.4): the
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
            if cs.table().bits(i).allocated(nslots) != live {
                mismatched += 1;
            }
            if live < nslots && !cs.is_partial(i) {
                unlisted += 1;
            }
        }
        if !header {
            header = true;
            writeln!(
                out,
                "  slab occupancy (non-empty slabs per eighth of nslots, last = full):"
            )?;
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

/// The general section of the JSON report: build and options, the
/// arena/bin geometry, the background thread, the `thp`, `cgroup` and
/// `stats` objects. `arenas_follow` says whether a `stats.arenas` object
/// follows (the `stats` line then ends with a comma).
fn json_general<W: Write>(
    out: &mut W,
    shared: &Shared,
    snap: &Snapshot,
    arenas_follow: bool,
) -> fmt::Result {
    writeln!(
        out,
        "\t\t\"config\": {{\"debug\": false, \"stats\": true, \"quantum\": {QUANTUM}}},"
    )?;
    writeln!(out, "\t\t\"opt\": {{")?;
    let mut first = true;
    for key in KEYS {
        let Some(v) = shared.config.get(key.as_bytes()) else {
            continue;
        };
        if !first {
            writeln!(out, ",")?;
        }
        first = false;
        match v {
            Value::Bool(b) => write!(out, "\t\t\t\"{key}\": {b}")?,
            Value::Int(i) => write!(out, "\t\t\t\"{key}\": {i}")?,
            Value::Uint(u) => write!(out, "\t\t\t\"{key}\": {u}")?,
            Value::Size(n) => write!(out, "\t\t\t\"{key}\": {n}")?,
            Value::Str(s) => write!(out, "\t\t\t\"{key}\": \"{}\"", word(s))?,
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
    writeln!(
        out,
        "\t\t\"background_thread\": {{\"present\": {}}},",
        purge::background::present()
    )?;
    let td = crate::thp::decision();
    let ts = crate::thp::stats();
    writeln!(
        out,
        "\t\t\"thp\": {{\"key\": \"{}\", \"mode\": \"{}\", \"reason\": \"{}\", \"class_ranges\": {}, \"class_bytes\": {}, \"chunks\": {}, \"huge_ranges\": {}, \"meta_ranges\": {}, \"failures\": {}, \"nohuge_ranges\": {}, \"host\": \"{}\"}},",
        word(crate::thp::key().name()),
        td.mode.name(),
        td.reason.text(),
        ts.class_ranges,
        ts.class_bytes,
        ts.chunks,
        ts.huge_ranges,
        ts.meta_ranges,
        ts.failures,
        ts.nohuge_ranges,
        crate::thp::host_mode().map_or("none", crate::sys::thp_host::ThpSetting::name)
    )?;
    let cg = shared.cgroup_now();
    cg.sample(crate::sys::now_coarse_ns());
    writeln!(
        out,
        "\t\t\"cgroup\": {{\"tracked\": {}, \"limit\": {}, \"high\": {}, \"current\": {}, \"decay_ms\": {}, \"retention_cap\": {}, \"empty_bytes\": {}, \"full_purges\": {}, \"thp_forced\": {}, \"purge_advice\": \"{}\", \"lazy_purges\": {}, \"eager_purges\": {}, \"purged_is_zero\": {}, \"failed_discards\": {}, \"failed_discard_bytes\": {}}},",
        cg.tracked(),
        cg.limit(),
        cg.high(),
        cg.current(),
        match shared.policy.effective_decay_ns(shared.decay_ns()) {
            crate::purge::NEVER => -1i64,
            ns => (ns / 1_000_000) as i64,
        },
        shared.policy.cap(),
        crate::policy::empty_bytes(shared),
        shared.policy.full_purges(),
        shared.policy.thp_forced(),
        purge::advice().name(),
        purge::batches().0,
        purge::batches().1,
        purge::purged_is_zero(),
        purge::failed_discards().0,
        purge::failed_discards().1
    )?;
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
        if arenas_follow { "," } else { "" }
    )
}

/// One arena's object of the JSON report's `stats.arenas`, without the
/// separating comma (the caller joins the arenas) or a trailing newline.
fn json_arena<W: Write>(out: &mut W, arena: &Arena, s: &ArenaStats, bins: bool) -> fmt::Result {
    writeln!(out, "\t\t\t\"{}\": {{", arena.index())?;
    writeln!(
        out,
        "\t\t\t\t\"dirty_decay_ms\": {},",
        decay_ms(arena.decay_ns())
    )?;
    writeln!(
        out,
        "\t\t\t\t\"pactive\": {}, \"pdirty\": {}, \"pmuzzy\": {},",
        s.pactive, s.pdirty, s.pmuzzy
    )?;
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
        if bins { "," } else { "" }
    )?;
    if bins {
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
}

/// The JSON report (`J`): the `jemalloc` object with `version` and
/// `implementation`, then [`json_general`] and the `stats.arenas` object of
/// [`json_arena`]s, each under its option letter as in [`text_report`].
fn json_report<W: Write>(out: &mut W, shared: &Shared, o: &Opts) -> fmt::Result {
    let snap = stats::collect(shared);
    writeln!(out, "{{")?;
    writeln!(out, "\t\"jemalloc\": {{")?;
    writeln!(out, "\t\t\"version\": \"{VERSION_STR}\",")?;
    writeln!(out, "\t\t\"implementation\": \"challoc\",")?;
    if !o.no_general {
        json_general(out, shared, &snap, !o.no_arenas)?;
    }
    if !o.no_arenas {
        writeln!(out, "\t\t\"stats.arenas\": {{")?;
        let mut r = Ok(());
        let mut first = true;
        for_each_arena(shared, |a, s| {
            if r.is_ok() {
                if !first {
                    r = writeln!(out, ",");
                }
                first = false;
                if r.is_ok() {
                    r = json_arena(out, a, s, !o.no_bins);
                }
            }
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
    let _ = if o.json {
        json_report(&mut sink, shared, &o)
    } else {
        text_report(&mut sink, shared, &o)
    };
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
        writeln!(
            out,
            "<size from=\"{}\" to=\"{}\" total=\"{}\" count=\"{}\"/>",
            b.size,
            b.size,
            b.curregs * b.size,
            b.curregs
        )?;
    }
    writeln!(out, "</sizes>")?;
    writeln!(out, "<total type=\"fast\" count=\"0\" size=\"0\"/>")?;
    writeln!(
        out,
        "<total type=\"rest\" count=\"{count}\" size=\"{}\"/>",
        s.small_allocated
    )?;
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
    let large_count: usize =
        all.large_runs + all.bins[NBINS..].iter().map(|b| b.curregs).sum::<usize>();
    writeln!(out, "<total type=\"fast\" count=\"0\" size=\"0\"/>")?;
    writeln!(
        out,
        "<total type=\"rest\" count=\"{count}\" size=\"{}\"/>",
        all.small_allocated
    )?;
    writeln!(
        out,
        "<total type=\"mmap\" count=\"{large_count}\" size=\"{}\"/>",
        all.large_allocated
    )?;
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
        unsafe { *libc::__errno_location() = crate::sys::Errno::EINVAL.0 };
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
                    writeln!(
                        sink,
                        "system bytes     = {:>10}",
                        s.resident.saturating_sub(s.large_allocated)
                    )?;
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

    export_pair! {
        fn malloc_stats_print / je_malloc_stats_print(write_cb: super::WriteCb, cbopaque: *mut c_void, opts: *const c_char) {
            super::malloc_stats_print(write_cb, cbopaque, opts)
        }
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
        malloc_stats_print(
            Some(capture),
            0xC0FFEE as *mut c_void,
            if opts.is_empty() {
                core::ptr::null()
            } else {
                c.as_ptr()
            },
        );
        String::from_utf8(CAPTURE.lock().unwrap().clone()).unwrap()
    }

    /// A two-byte character that lands on the last free byte of the
    /// 1023-byte buffer: the sink must not slice the `str` inside it, and
    /// the bytes reach the callback whole and in order.
    #[test]
    fn sink_splits_a_character_at_the_buffer_end() {
        let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        CAPTURE.lock().unwrap().clear();
        let mut sink = Sink::new(Target::Callback(Some(capture), 0xC0FFEE as *mut c_void));
        let head = "x".repeat(1022);
        sink.write_str(&head).unwrap();
        sink.write_str("\u{e9} tail").unwrap();
        sink.flush();
        let got = String::from_utf8(CAPTURE.lock().unwrap().clone()).unwrap();
        assert_eq!(got, std::format!("{head}\u{e9} tail"));
    }

    /// Page-sized slabs (design 3): after init no class has a slab
    /// smaller than the kernel page (up to 64 KiB), so on a 16 or 64 KiB
    /// kernel the 512..1024-byte classes have page-sized slabs, every
    /// table's page group is one slab, and the report says so; on a 4 KiB
    /// kernel the table is the compile-time one and the report is silent.
    #[test]
    fn slabs_are_never_smaller_than_the_page() {
        use crate::layout::{
            BASE_CLASSES, CLASSES, LG_SMALL_SLAB, MIN_SLAB, NCLASSES, page_sized_classes,
        };
        let s = crate::api::shared();
        let page = s.regions.page_size();
        let floor = page.min(1 << LG_SMALL_SLAB);
        for c in 0..NCLASSES {
            let cls = &CLASSES[c];
            assert!(
                cls.slab_size() >= floor,
                "class {c}: slab {} below the page {page}",
                cls.slab_size()
            );
            assert_eq!(cls.nslots(), cls.slab_size() / cls.size(), "class {c}");
            assert_eq!(
                cls.lg_slab as u32,
                (BASE_CLASSES[c].lg_slab as u32).max(page.trailing_zeros().min(LG_SMALL_SLAB)),
                "class {c}"
            );
            assert_eq!(
                s.tables()[c].slabs_per_page(),
                (page / cls.slab_size()).max(1),
                "class {c}"
            );
            if page <= 1 << LG_SMALL_SLAB {
                assert_eq!(
                    s.tables()[c].slabs_per_page(),
                    1,
                    "class {c}: page groups are dormant"
                );
            }
        }
        let text = report("");
        if page > MIN_SLAB {
            let (lo, hi) =
                page_sized_classes().expect("a large page raises the small-slab classes");
            assert_eq!((CLASSES[lo].size(), CLASSES[hi].size()), (512, 1024));
            let want = std::format!(
                "Page-sized slabs: classes {lo}..{hi} (512..1024 bytes) at {} bytes (4096..16384 in the table)\n",
                floor
            );
            assert!(text.contains(&want), "{text}");
        } else {
            assert_eq!(page_sized_classes(), None);
            assert_eq!(&*CLASSES, &BASE_CLASSES);
            assert!(!text.contains("Page-sized slabs"), "{text}");
        }
    }

    #[test]
    fn text_report_has_every_section_and_live_figures() {
        let live: Vec<*mut u8> = (0..100).map(|_| malloc(700)).collect();
        let text = report("");
        assert!(
            text.starts_with("___ Begin jemalloc statistics ___\n"),
            "{text}"
        );
        assert!(
            text.ends_with("--- End jemalloc statistics ---\n"),
            "{text}"
        );
        for needle in [
            "Version: \"5.3.0-0-gchalloc\"",
            "config.quantum:",
            "Run-time option settings",
            "opt.dirty_decay_ms: ",
            "opt.thp: \"never\"\n",
            "thp: never -> off (thp:never) nohuge ",
            " (classes, metadata; huge follows host) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host ",
            "opt.zero_on_free: false\n",
            "opt.slot_randomize: false\n",
            "opt.cgroup: true\n",
            "opt.stats_print: accepted, no effect",
            "opt.double_free_scan: false\n",
            "opt.huge_retain_max: 67108864\n",
            "Kernel and CPU features: page_size=",
            "Sealed ranges: ",
            "Background thread: ",
            "Arenas: ",
            "\nQuantum size: ",
            "\nPage size: ",
            "Allocated: ",
            "Huge: ",
            "Metadata: reserved ",
            "  gated clamped\n",
            "\ncgroup: ",
            "\ncgroup policy: ",
            "\npurge advice: eager MADV_DONTNEED (no headroom known) lazy_purges 0 (0 bytes) eager_purges ",
            "\nthp: ",
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
            .filter_map(|l| {
                l.split_whitespace()
                    .map(|w| w.parse::<usize>().ok())
                    .collect::<Option<Vec<usize>>>()
            })
            .collect();
        assert!(!rows.is_empty(), "bin row for the live class in:\n{text}");
        let curregs: usize = rows.iter().map(|cols| cols[4]).sum();
        assert!(curregs >= 100, "curregs in {rows:?}");
        // The Allocated figure is live, not the epoch snapshot.
        let line = text.lines().find(|l| l.starts_with("Allocated: ")).unwrap();
        let allocated: usize = line
            .split(", ")
            .next()
            .unwrap()
            .trim_start_matches("Allocated: ")
            .parse()
            .unwrap();
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
        assert!(text.contains("\"cgroup\": {\"tracked\": "));
        assert!(
            text.contains("\"thp\": {\"key\": \"never\", \"mode\": \"off\", "),
            "{text}"
        );
        assert!(
            text.contains("\"failures\": 0, \"nohuge_ranges\": ")
                && text.contains(", \"host\": \""),
            "{text}"
        );
        assert!(
            text.contains("\"purge_advice\": \"eager\", \"lazy_purges\": 0, \"eager_purges\": "),
            "{text}"
        );
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
        assert!(
            !compact.contains(",}") && !compact.contains(",]"),
            "trailing comma in:\n{text}"
        );
        // One bins array of NBINS entries per arena section in this very
        // text (other tests create arenas concurrently).
        assert_eq!(
            text.matches("\"size\": ").count(),
            NBINS * text.matches("\"pactive\": ").count()
        );
        let ga = report("Jga");
        assert!(
            ga.contains("\"version\"")
                && !ga.contains("\"opt\"")
                && !ga.contains("\"stats.arenas\""),
            "{ga}"
        );
    }

    /// Every printable character of a report except the figures: digit runs
    /// (with a sign) become `N`, `true`/`false` become `B`, a quoted value
    /// after `: ` becomes `"S"`. Keys, punctuation and indentation stay.
    fn masked(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            if let Some(r) = rest.strip_prefix(": \"") {
                let end = r.find('"').expect("closing quote");
                out.push_str(": \"S\"");
                rest = &r[end + 1..];
            } else if let Some(r) = rest.strip_prefix("true") {
                out.push('B');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("false") {
                out.push('B');
                rest = r;
            } else if c.is_ascii_digit()
                || (c == '-' && rest[1..].starts_with(|d: char| d.is_ascii_digit()))
            {
                out.push('N');
                rest = rest[1..].trim_start_matches(|d: char| d.is_ascii_digit());
            } else {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        out
    }

    /// The JSON report's shape, pinned character for character with the
    /// figures masked (`masked`): the general section once, then one
    /// arena block per arena in the report with `NBINS` bin rows each.
    /// A change to the report's layout has to change this text; a refactor
    /// of the writer that keeps the output byte-identical does not.
    #[test]
    fn json_report_shape_is_pinned() {
        let mut general = String::from(
            "{\n\t\"jemalloc\": {\n\t\t\"version\": \"S\",\n\t\t\"implementation\": \"S\",\n\t\t\"config\": {\"debug\": B, \"stats\": B, \"quantum\": N},\n\t\t\"opt\": {\n",
        );
        let opts: Vec<String> = KEYS
            .iter()
            .filter_map(|key| {
                let v = crate::api::shared().config.get(key.as_bytes())?;
                let shape = match v {
                    Value::Bool(_) => "B",
                    Value::Int(_) | Value::Uint(_) | Value::Size(_) => "N",
                    Value::Str(_) => "\"S\"",
                };
                Some(std::format!("\t\t\t\"{key}\": {shape}"))
            })
            .collect();
        general.push_str(&opts.join(",\n"));
        general.push_str(concat!(
            "\n\t\t},\n",
            "\t\t\"arenas\": {\"narenas\": N, \"nbins\": N, \"quantum\": N, \"page\": N},\n",
            "\t\t\"background_thread\": {\"present\": B},\n",
            "\t\t\"thp\": {\"key\": \"S\", \"mode\": \"S\", \"reason\": \"S\", \"class_ranges\": N, \"class_bytes\": N, \"chunks\": N, \"huge_ranges\": N, \"meta_ranges\": N, \"failures\": N, \"nohuge_ranges\": N, \"host\": \"S\"},\n",
            "\t\t\"cgroup\": {\"tracked\": B, \"limit\": N, \"high\": N, \"current\": N, \"decay_ms\": N, \"retention_cap\": N, \"empty_bytes\": N, \"full_purges\": N, \"thp_forced\": B, \"purge_advice\": \"S\", \"lazy_purges\": N, \"eager_purges\": N, \"purged_is_zero\": B, \"failed_discards\": N, \"failed_discard_bytes\": N},\n",
            "\t\t\"stats\": {\"allocated\": N, \"active\": N, \"metadata\": N, \"resident\": N, \"mapped\": N, \"retained\": N},\n",
            "\t\t\"stats.arenas\": {\n",
        ));
        let mut arena = String::from(concat!(
            "\t\t\t\"N\": {\n",
            "\t\t\t\t\"dirty_decay_ms\": N,\n",
            "\t\t\t\t\"pactive\": N, \"pdirty\": N, \"pmuzzy\": N,\n",
            "\t\t\t\t\"small\": {\"allocated\": N, \"active\": N, \"dirty\": N, \"retained\": N},\n",
            "\t\t\t\t\"large\": {\"allocated\": N, \"active\": N, \"dirty\": N, \"retained\": N, \"runs\": N},\n",
            "\t\t\t\t\"metadata\": N, \"resident\": N, \"mapped\": N, \"retained\": N,\n",
            "\t\t\t\t\"bins\": [\n",
        ));
        for j in 0..NBINS {
            arena.push_str("\t\t\t\t\t{\"size\": N, \"nregs\": N, \"slab_size\": N, \"curregs\": N, \"curslabs\": N, \"nonfull_slabs\": N, \"empty_slabs\": N, \"purged_slabs\": N}");
            arena.push_str(if j + 1 == NBINS { "\n" } else { ",\n" });
        }
        arena.push_str("\t\t\t\t]\n\t\t\t}");
        let text = report("J");
        let got = masked(&text);
        let narenas = text.matches("\"pactive\": ").count();
        assert!(narenas >= 1, "{text}");
        let mut want = general;
        want.push_str(&std::vec![arena.as_str(); narenas].join(",\n"));
        want.push_str("\n\t\t}\n\t}\n}\n");
        assert_eq!(got, want, "masked JSON report:\n{got}");
        // Without bins the arena block ends at the totals line; without
        // arenas the general section closes the object itself.
        let b = masked(&report("Jb"));
        assert!(b.contains("\"retained\": N\n\t\t\t}"), "{b}");
        assert!(!b.contains("\"bins\""), "{b}");
        let a = masked(&report("Ja"));
        assert!(a.ends_with("\"retained\": N}\n\t}\n}\n"), "{a}");
        let g = masked(&report("Jg"));
        assert!(g.starts_with("{\n\t\"jemalloc\": {\n\t\t\"version\": \"S\",\n\t\t\"implementation\": \"S\",\n\t\t\"stats.arenas\": {\n"), "{g}");
    }

    /// The text report's line set, pinned by each line's head: its text up
    /// to the first `:` with the figures masked (`masked`), the whole line
    /// when it has no `:`, and `<row>` for a run of table rows. The general
    /// section is pinned line for line; every arena block is the fixed
    /// lines down to the bins header, then the bins rows, the slab-allocated
    /// large classes and the occupancy histogram, each present only when it
    /// has rows. A refactor of the writers keeps this; a renamed, added or
    /// dropped line has to change it.
    #[test]
    fn text_report_line_set_is_pinned() {
        fn head(line: &str) -> String {
            let m = masked(line);
            if m.trim_start().starts_with('N') {
                return "<row>".into();
            }
            match m.find(':') {
                Some(i) => m[..=i].into(),
                None => m,
            }
        }
        let text = report("");
        let mut heads: Vec<String> = Vec::new();
        for line in text.lines() {
            let h = head(line);
            if h != "<row>" || heads.last().is_none_or(|p| p != "<row>") {
                heads.push(h);
            }
        }
        let mut want: Vec<String> = std::vec![
            "___ Begin jemalloc statistics ___".into(),
            "Version:".into(),
            "Build-time option settings".into(),
            "  config.debug:".into(),
            "  config.stats:".into(),
            "  config.quantum:".into(),
            "Run-time option settings".into(),
        ];
        want.extend(
            KEYS.iter()
                .chain(INERT_KEYS)
                .map(|key| std::format!("  opt.{key}:")),
        );
        want.extend(
            [
                "Kernel and CPU features:",
                "Sealed ranges:",
                "Background thread:",
                "Arenas:",
                "Quantum size:",
                "Page size:",
            ]
            .map(String::from),
        );
        if crate::layout::page_sized_classes().is_some() {
            want.push("Page-sized slabs:".into());
        }
        want.extend(
            [
                "Allocated:",
                "Huge:",
                "Metadata:",
                "cgroup:",
                "cgroup policy:",
                "purge advice:",
                "thp:",
            ]
            .map(String::from),
        );
        let general = heads
            .iter()
            .position(|h| h == "arenas[N]:")
            .unwrap_or_else(|| panic!("no arena block in:\n{text}"));
        assert_eq!(heads[..general], want[..], "general section of:\n{text}");
        assert_eq!(
            heads.last().map(String::as_str),
            Some("--- End jemalloc statistics ---")
        );
        let fixed = [
            "arenas[N]:",
            "  dirty_decay_ms:",
            "  pactive:",
            "                            allocated       active        dirty     retained",
            "  small:",
            "  large:",
            "  metadata:",
            "  bins:",
        ];
        let tables = [
            "",
            "  large classes (slab-allocated, not bins):",
            "  slab occupancy (non-empty slabs per eighth of nslots, last = full):",
        ];
        let arenas = &heads[general..heads.len() - 1];
        let starts: Vec<usize> = (0..arenas.len())
            .filter(|&i| arenas[i] == "arenas[N]:")
            .collect();
        for (k, &start) in starts.iter().enumerate() {
            let block = &arenas[start..starts.get(k + 1).copied().unwrap_or(arenas.len())];
            assert!(
                block.len() >= fixed.len() && block[..fixed.len()] == fixed,
                "arena block {block:?} in:\n{text}"
            );
            let mut rest = &block[fixed.len()..];
            for table in tables {
                if !table.is_empty() && rest.first().is_some_and(|h| h == table) {
                    rest = &rest[1..];
                    assert!(
                        rest.first().is_some_and(|h| h == "<row>"),
                        "`{table}` without rows in {block:?}"
                    );
                }
                if rest.first().is_some_and(|h| h == "<row>") {
                    rest = &rest[1..];
                }
            }
            assert!(
                rest.is_empty(),
                "unexpected lines {rest:?} in arena block {block:?} of:\n{text}"
            );
        }
    }

    /// The module docs promise that nothing in a report allocates: the
    /// calling thread's `thread.allocated` and `thread.deallocated` do not
    /// move across a text and a JSON report written to a sink callback
    /// that itself allocates nothing.
    #[test]
    fn stats_print_does_not_allocate() {
        unsafe extern "C" fn sink(_opaque: *mut c_void, _s: *const c_char) {}
        let s = crate::api::shared();
        // Make sure this thread has a heap (the counters live in it), then
        // read them through the same materialisation `mallctl` uses.
        free(malloc(64));
        let counters = || {
            let st = crate::heap::heap_for_thread(s).unwrap().materialise_stats();
            (st.allocated, st.deallocated)
        };
        let before = counters();
        let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        malloc_stats_print(Some(sink), core::ptr::null_mut(), core::ptr::null());
        let json = std::ffi::CString::new("J").unwrap();
        malloc_stats_print(Some(sink), core::ptr::null_mut(), json.as_ptr());
        assert_eq!(
            counters(),
            before,
            "a report moved this thread's allocation counters"
        );
    }

    #[test]
    fn stats_print_without_callback_uses_the_message_hook() {
        use crate::api::jemalloc::set_message_hook_for_tests;
        static HOOKED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
            assert!(opaque.is_null());
            HOOKED
                .lock()
                .unwrap()
                .extend_from_slice(unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes());
        }
        set_message_hook_for_tests(Some(hook));
        malloc_stats_print(None, core::ptr::null_mut(), core::ptr::null());
        set_message_hook_for_tests(None);
        let got = String::from_utf8(HOOKED.lock().unwrap().clone()).unwrap();
        assert!(
            got.starts_with("___ Begin jemalloc statistics ___\n"),
            "{got}"
        );
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
        assert!(
            xml.contains(&std::format!(
                "<size from=\"{size}\" to=\"{size}\" total=\""
            )),
            "{xml}"
        );
        assert!(xml.contains("<total type=\"mmap\" count=\""));
        assert_eq!(
            xml.matches("<heap nr=").count(),
            xml.matches("</heap>").count()
        );
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
