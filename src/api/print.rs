//! The reports (design 13.1 and 16): jemalloc's
//! `malloc_stats_print`, glibc's `malloc_info` (XML) and `malloc_stats`.
//!
//! Everything is formatted with `core::fmt` into a small stack buffer and
//! handed on in chunks: to the caller's `write_cb`, to the `malloc_message`
//! hook, to a `FILE *` through `fwrite`, or to stderr with `write(2)`.
//! Nothing allocates from the heap: the per-arena figures are collected
//! into one or two `stats::Scratch` records, metadata objects taken at the
//! entry point before any lock and given back before it returns, so a
//! report runs on a `PTHREAD_STACK_MIN` thread; without them it writes
//! `(no memory)`. No lock is held across a write, so a callback may
//! allocate or report. The figures are *live* (`stats::collect`, not the
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
use crate::QUANTUM;
use crate::arena::Arena;
use crate::config::{INERT_KEYS, KEYS, PercpuArena, Value};
use crate::heap::Shared;
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, NBINS, NCLASSES};
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::axiom_entry;
use crate::purge;
use crate::stats::{self, ArenaStats, Snapshot, fig_add, fig_mul};
use crate::sys::bytes_is;
use crate::vspec::cbuf;
use crate::vspec::errno::{ThreadErrno, set_errno};
use crate::vspec::print::base_slab_sizes;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;
use vstd::prelude::{Tracked, verus};
use vstd::raw_ptr::ptr_ref;
#[cfg(verus_keep_ghost)]
use vstd::string::StringSliceAdditionalSpecFns;

/// `void (*write_cb)(void *cbopaque, const char *s)`.
pub type WriteCb = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

/// The caller's [`WriteCb`], wrapped so that [`Target`] and
/// [`malloc_stats_print`] can be inside `verus!` (Verus rejects function
/// pointer types; it sees this type as opaque). Transparent, so it has
/// `WriteCb`'s ABI and the C exports take it as their `write_cb`.
#[repr(transparent)]
pub struct WriteCallback(pub WriteCb);

verus! {

/// Opaque: `WriteCallback` holds a function pointer type, which Verus
/// rejects.
#[verifier::external_type_specification]
#[verifier::external_body]
#[allow(dead_code)]
pub struct ExWriteCallback(WriteCallback);

/// Where a report goes.
enum Target {
    /// The caller's callback, or with `None` the `malloc_message` path.
    Callback(WriteCallback, *mut c_void),
    /// glibc's `malloc_info` stream.
    File(*mut libc::FILE),
    /// `write(2)` to stderr, bypassing any hook (glibc's `malloc_stats`).
    Stderr,
}

/// Hand `buf[..len]` to `target`: the report's only calls out of the
/// allocator. To the caller's callback ([`call_back`]) as the C string
/// `buf` holds, NUL-terminated at `len`; with no callback, to the
/// `malloc_message` path ([`malloc_write`]: the hook, or `write(2)` to
/// stderr); to the caller's stream ([`write_file`]); or with `write(2)` to
/// stderr. The calling thread holds no allocator lock: `ord`, the report's
/// lock-order token ([`Sink`]), has top 0 (see [`write_file`] for why that
/// matters).
#[inline(always)]
fn emit(target: &Target, buf: &[u8; 1024], len: usize, Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        len < 1024,
        buf@[len as int] == 0,
        ord.top() == 0,
{
    match target {
        Target::Callback(cb, opaque) => {
            if !call_back(cb, *opaque, buf, Tracked(ord)) {
                malloc_write(&buf[..len], Tracked(ord));
            }
        }
        Target::File(f) => write_file(*f, &buf[..len], Tracked(ord)),
        Target::Stderr => crate::sys::write_all(crate::sys::STDERR_FILENO, &buf[..len]),
    }
}

/// Call the caller's callback `cb`, when it gave one, with `opaque` and
/// the C string in `buf`, which holds a NUL; whether it did. Trusted: a
/// call through the caller's function pointer; claims nothing. The calling
/// thread holds no allocator lock (`ord` has top 0), as for [`emit`].
#[verifier::external_body]
#[inline(always)]
fn call_back(
    cb: &WriteCallback,
    opaque: *mut c_void,
    buf: &[u8; 1024],
    Tracked(ord): Tracked<&LockOrder>,
) -> bool
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
        exists|i: int| 0 <= i < 1024 && #[trigger] buf@[i] == 0,
{
    let Some(cb) = cb.0 else {
        return false;
    };
    // SAFETY: the caller's `void (*)(void *, const char *)`; the chunk is
    // NUL-terminated within the buffer.
    unsafe { cb(opaque, buf.as_ptr() as *const c_char) };
    true
}

/// `fwrite` of `s` to the caller's stream `f`. Trusted: a call into libc;
/// claims nothing. `fwrite` may itself call `malloc` (glibc allocates a
/// stream's buffer on its first write), which re-enters this allocator
/// from inside `malloc_info`: that is safe because no allocator lock is
/// held while the sink flushes (`stats::collect_arena` returns its figures
/// before they are formatted, and the arena scan reads the registry
/// without a lock) and the report's own state is the sink's stack buffer.
/// `ord`, the report's lock-order token, has top 0, as for [`emit`].
/// `fwrite` is a cancellation point; it runs with cancellation disabled,
/// so a deferred `pthread_cancel` never unwinds through challoc's
/// (non-unwinding) frames and is acted on at the program's next
/// cancellation point instead (design 8.1).
#[verifier::external_body]
#[inline(always)]
fn write_file(f: *mut libc::FILE, s: &[u8], Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
{
    // glibc's, which the `libc` crate does not bind; the same on both
    // targets.
    const PTHREAD_CANCEL_DISABLE: i32 = 1;
    unsafe extern "C" {
        fn pthread_setcancelstate(state: i32, old: *mut i32) -> i32;
    }
    // SAFETY: `f` is the caller's open stream; `s` is readable; `old` is
    // a live int the cancel-state calls write.
    unsafe {
        let mut old = 0;
        pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut old);
        libc::fwrite(s.as_ptr() as *const c_void, 1, s.len(), f);
        pthread_setcancelstate(old, &mut old);
    }
}

/// A `fmt::Write` that buffers up to 1023 bytes and forwards them,
/// NUL-terminated where the target wants a C string.
///
/// The sink holds the report's lock-order token ([`LockOrder`]), made at
/// top 0 by the report's entry point, from creation to drop. Every call of
/// the report that may take an allocator lock gets a token lent from it
/// ([`Report::lend`], through [`LockOrder::lend`]). The lent token borrows
/// the sink, so the sink is not written, and so not flushed, until that
/// token and every guard taken with it are gone, and it is not a root, so
/// no lock outlives its guard ([`crate::lock::RawLock::lock_held`] needs a
/// root); [`emit`] requires top 0.
struct Sink {
    target: Target,
    buf: [u8; 1024],
    len: usize,
    /// Ghost: the report's lock-order token, for [`emit`] and the calls
    /// that take a lock (used only in ghost code).
    #[allow(dead_code)]
    ord: Tracked<LockOrder<'static>>,
}

impl Sink {
    /// The buffer has room for a NUL after its bytes, the sink exists only
    /// where a system call may be made (a flush is one, as for
    /// `sys::abort::Stderr`), and its owner holds no allocator lock.
    #[verifier::type_invariant]
    spec fn wf(&self) -> bool {
        self.len < 1024 && crate::sys::may_syscall() && self.ord@.top() == 0
    }

    /// Requires the syscall capability: a write may flush to the kernel,
    /// so a sink is built only where a system call may be made. `ord` is
    /// the report's token, with no lock held.
    fn new(target: Target, ord: Tracked<LockOrder<'static>>) -> (r: Sink)
        requires
            crate::sys::may_syscall(),
            ord@.top() == 0,
        ensures
            r.len == 0,
    {
        Sink {
            target,
            buf: [0; 1024],
            len: 0,
            ord,
        }
    }

    /// Hand the buffered bytes to the target ([`emit`], under the
    /// report's token), NUL-terminated, and empty the buffer. The NUL fits
    /// and `emit`'s bounds hold by the type invariant (`len < 1024`, top 0).
    fn flush(&mut self)
        ensures
            final(self).len == 0,
    {
        proof {
            use_type_invariant(&*self);
        }
        if self.len == 0 {
            return;
        }
        self.buf[self.len] = 0;
        emit(&self.target, &self.buf, self.len, Tracked(self.ord.borrow()));
        self.len = 0;
    }
}

/// A report writer that holds the report's lock-order token: the report
/// writers stay generic over the writer and reach the token through
/// [`Report::lend`], which is ghost, so the bound erases and the writers
/// compile to the same code.
/// [`Sink`] is the only implementation.
trait Report {
    /// A token for one call of the report that may take an allocator
    /// lock: top 0 and not a root, borrowing the writer for as long as it
    /// lives, as [`LockOrder::lend`].
    proof fn lend<'b>(tracked &'b mut self) -> (tracked o: LockOrder<'b>)
        ensures
            o.top() == 0,
            !o.root();
}

/// The longest prefix of `s` that fits in `room` bytes and ends on a
/// character boundary, so slicing `s` there cannot panic. For a non-empty
/// `s`, 0 only when its first character does not fit: `room >= 4` always
/// takes one.
#[inline(always)]
fn char_cut(s: &str, room: usize) -> (n: usize)
    requires
        // Every `str` fits in memory (at most `isize::MAX` bytes).
        s.spec_bytes().len() <= usize::MAX,
    ensures
        n <= s.spec_bytes().len(),
        n <= room,
        vstd::utf8::is_char_boundary(s.spec_bytes(), n as int),
        s@.len() > 0 && room >= 4 ==> n > 0,
{
    proof {
        lemma_first_char_boundary(s);
    }
    let ghost first = vstd::utf8::length_of_first_scalar(s.spec_bytes());
    let mut n = s.len().min(room);
    assert(n == s.spec_bytes().len() || n == room);
    while !s.is_char_boundary(n)
        invariant
            n <= s.spec_bytes().len(),
            n <= room,
            vstd::utf8::is_char_boundary(s.spec_bytes(), 0),
            s@.len() > 0 ==> 1 <= first <= 4 && first <= s.spec_bytes().len()
                && vstd::utf8::is_char_boundary(s.spec_bytes(), first),
            s@.len() > 0 && room >= 4 ==> first <= n,
        decreases n,
    {
        n -= 1;
    }
    n
}

/// A non-empty `str` starts with a character of 1 to 4 bytes, which ends
/// on a character boundary, as 0 is one.
proof fn lemma_first_char_boundary(s: &str)
    ensures
        vstd::utf8::valid_utf8(s.spec_bytes()),
        vstd::utf8::is_char_boundary(s.spec_bytes(), 0),
        s@.len() > 0 ==> {
            let first = vstd::utf8::length_of_first_scalar(s.spec_bytes());
            &&& 1 <= first <= 4
            &&& first <= s.spec_bytes().len()
            &&& vstd::utf8::is_char_boundary(s.spec_bytes(), first)
        },
{
    let b = s.spec_bytes();
    vstd::utf8::encode_utf8_valid_utf8(s@);
    vstd::utf8::encode_utf8_decode_utf8(s@);
    if s@.len() > 0 {
        assert(b.len() > 0);
        assert(vstd::utf8::valid_utf8(vstd::utf8::pop_first_scalar(b)));
        assert(vstd::utf8::is_char_boundary(vstd::utf8::pop_first_scalar(b), 0));
    }
}

// The cut on a character boundary is [`char_cut`]; the copy is a byte
// loop rather than `copy_from_slice`: Verus does not take a `&mut`
// through a field of a type with an invariant in a call that may unwind.
impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut s = s;
        while !s.is_empty()
            decreases s.spec_bytes().len() * 2048 + self.len,
        {
            proof {
                use_type_invariant(&*self);
            }
            let room = self.buf.len() - 1 - self.len;
            if room == 0 {
                self.flush();
                continue;
            }
            // Cut on a character boundary: slicing `s` inside a multi-byte
            // character would panic in the middle of a report.
            let b = s.as_bytes();
            proof {
                assert(b@.len() == b.len());
            }
            let n = char_cut(s, room);
            if n == 0 {
                self.flush();
                continue;
            }
            let mut i = 0;
            while i < n
                invariant
                    crate::sys::may_syscall(),
                    self.ord@.top() == 0,
                    self.len + n < 1024,
                    n <= b@.len(),
                    i <= n,
                decreases n - i,
            {
                self.buf[self.len + i] = b[i];
                i += 1;
            }
            self.len += n;
            // `s = &s[n..]`, as `split_at`, whose specification Verus has.
            s = s.split_at(n).1;
        }
        Ok(())
    }
}

/// Parsed `opts`.
struct Opts {
    json: bool,
    no_general: bool,
    no_arenas: bool,
    no_bins: bool,
}

/// Byte `i` of the caller's option string, through the read permission
/// the call's token gives ([`cbuf::CCall::str_c_byte`]): the byte its view
/// names, in the string because no byte before it is the NUL.
///
/// # Safety
/// `opts + i` is a readable byte of the caller's string.
#[inline(always)]
#[allow(clippy::unnecessary_cast)]
unsafe fn opt_byte(opts: *const c_char, i: usize, Tracked(cc): Tracked<&cbuf::CCall>) -> (r: u8)
    requires
        cc.reads_str(opts as *const u8),
        cc.string().before_nul(i as int),
    ensures
        r == cc.string().bytes()[i as int],
{
    proof {
        broadcast use vstd::layout::layout_of_primitives, vstd::raw_ptr::axiom_ptr_mut_from_data;
        cc.string().lemma_in(i as int);
        crate::vspec::ptr::axiom_ptr_add_addr(opts as *mut c_char, i);
    }
    // SAFETY: guaranteed by the caller.
    let q = unsafe { opts.add(i) };
    let tracked b = cc.str_c_byte(opts as *const u8, q, i as int);
    *ptr_ref(q, Tracked(b)) as u8
}

impl Opts {
    /// `opts` is the caller's option string: null, or readable up to and
    /// including its NUL terminator (jemalloc's contract,
    /// [`cbuf::CArg::Str`]), read through the call's token `cc`; at most
    /// 64 bytes are read, so a longer string is truncated, never overrun.
    fn parse(opts: *const c_char, Tracked(cc): Tracked<&cbuf::CCall>) -> (o: Opts)
        requires
            opts@.addr != 0 ==> cc.reads_str(opts as *const u8),
        ensures
            opts@.addr == 0 ==> !o.json && !o.no_general && !o.no_arenas && !o.no_bins,
    {
        // `Opts::default()`, spelled out: Verus does not see the derived
        // `Default`.
        let mut o = Opts {
            json: false,
            no_general: false,
            no_arenas: false,
            no_bins: false,
        };
        if opts.is_null() {
            return o;
        }
        for i in 0..64
            invariant_except_break
                cc.string().before_nul(i as int),
            invariant
                cc.reads_str(opts as *const u8),
        {
            // SAFETY: the caller's NUL-terminated option string; reads stop
            // at the terminator or after 64 bytes.
            let b = unsafe { opt_byte(opts, i, Tracked(cc)) };
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

// The last impl of the module: rustc numbers a module's impls in order,
// the number enters the mangled names of each impl's items, and those
// names order `.text`, so an impl placed earlier would renumber the ones
// after it.
impl Report for Sink {
    proof fn lend<'b>(tracked &'b mut self) -> (tracked o: LockOrder<'b>) {
        use_type_invariant(&*self);
        self.ord.borrow_mut().lend()
    }
}

/// A configuration value's name (`config::Value::Str`, the `thp` key).
/// Trusted: `CStr::to_str`, which vstd does not specify; nothing is
/// claimed about the result.
#[verifier::external_body]
fn word(s: &CStr) -> &str {
    s.to_str().unwrap_or("?")
}

fn decay_ms(ns: u64) -> (r: i64)
    ensures
        r == if ns == purge::NEVER {
            -1
        } else {
            (ns / 1_000_000) as int
        },
{
    if ns == purge::NEVER {
        -1
    } else {
        (ns / 1_000_000) as i64
    }
}

// ---------------------------------------------------------------------------
// Text report.
// ---------------------------------------------------------------------------

/// The text report's general section: [`text_settings`], then the
/// cgroup (whose sample reads its files, the deepest call of a report),
/// policy, advice and `thp:` lines, each from a frame of its own (design
/// 13.8).
#[inline(never)]
fn text_general<W: Write + Report>(out: &mut W, shared: &Shared, snap: &Snapshot, features: &str) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    text_settings(out, shared, snap, features)?;
    cgroup_line(out, shared)?;
    policy_line(out, shared)?;
    advice_line(out)?;
    thp_line(out)
}

/// The general section's banner, options, features (the feature line,
/// `features`) and totals.
// The indexed loops stand for `for key in KEYS`: Verus does not specify a
// slice's `IntoIterator`.
#[allow(clippy::needless_range_loop)]
// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
fn text_settings<W: Write + Report>(out: &mut W, shared: &Shared, snap: &Snapshot, features: &str) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    proof {
        shared.lemma_wf();
    }
    // jemalloc's banner, not "challoc statistics": consumers key on it (Valkey's
    // `MEMORY MALLOC-STATS` coverage test asserts `*jemalloc*`), just as the
    // JSON form keeps its top-level "jemalloc" object. The `Version:` line
    // that follows says which library produced the report.
    writeln!(out, "___ Begin jemalloc statistics ___")?;
    writeln!(out, "Version: \"{}\"", super::mallctl::VERSION)?;
    writeln!(out, "Build-time option settings")?;
    writeln!(out, "  config.debug: false")?;
    writeln!(out, "  config.stats: true")?;
    writeln!(out, "  config.quantum: {QUANTUM}")?;
    writeln!(out, "Run-time option settings")?;
    // `for key in KEYS`, with an index: Verus does not specify a slice's
    // `IntoIterator`.
    let keys = KEYS;
    for k in 0..keys.len() {
        let key = &keys[k];
        let note = if bytes_is(key.as_bytes(), b"thp") && shared.policy.thp_forced() {
            " (accepted, no effect; forced to never by a cgroup limit under 1 GiB)"
        } else if bytes_is(key.as_bytes(), b"percpu_arena") && shared.config.percpu_arena != PercpuArena::Disabled {
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
    let inert = INERT_KEYS;
    for k in 0..inert.len() {
        let key = &inert[k];
        writeln!(out, "  opt.{}: accepted, no effect", key)?;
    }
    writeln!(out, "Kernel and CPU features: {}", features)?;
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
        let (base_min, base_max) = base_slab_sizes(lo, hi);
        proof {
            crate::vspec::class::lemma_class_table_wf(lo as int);
        }
        writeln!(
            out,
            "Page-sized slabs: classes {}..{} ({}..{} bytes) at {} bytes ({}..{} in the table)",
            lo,
            hi,
            CLASSES.table()[lo].size(),
            CLASSES.table()[hi].size(),
            CLASSES.table()[lo].slab_size(),
            base_min,
            base_max
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
    )
}

/// The cgroup line (design 11): the figures as last sampled (by
/// [`sample_cgroup`], at the entry point), or why nothing is tracked.
// Out of line, as `text_settings`.
#[inline(never)]
fn cgroup_line<W: Write + Report>(out: &mut W, shared: &Shared) -> fmt::Result
    requires
        crate::sys::may_syscall(),
        shared.wf(),
{
    let tracked mut lent = out.lend();
    let cgroup = shared.cgroup_now(Tracked(&mut lent));
    // The `Cgroup` itself, not the reference to it.
    writeln!(out, "{}", *cgroup)
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
        // `1usize << lg`, as `wrapping_shl`: what `<<` compiles to in
        // release, and the thresholds are atomics no invariant bounds.
        write!(out, " slab_min {}", 1usize.wrapping_shl(d.lg_slab_min as u32))?;
        write!(out, " pmd {}", 1usize.wrapping_shl(d.lg_pmd as u32))?;
        match d.lg_meta {
            crate::thp::NO_SIZE => write!(out, " meta off")?,
            lg => write!(out, " meta {}", 1usize.wrapping_shl(lg as u32))?,
        }
        write!(out, " huge {}", if d.huge { "on" } else { "off" })?;
    }
    write!(
        out,
        " class_ranges {} ({} bytes, {} chunks) huge_ranges {} meta_ranges {} failures {}",
        s.class_ranges, s.class_bytes, s.chunks, s.huge_ranges, s.meta_ranges, s.failures
    )?;
    // Design 17: the host's global mode.
    // `map_or("none", ThpSetting::name)`, spelled as a `match`: Verus does
    // not take a function item as a closure.
    writeln!(
        out,
        "; host {}",
        match crate::thp::host_mode() {
            Some(m) => m.name(),
            None => "none",
        }
    )
}

/// The `cgroup policy:` line (design 11): the band as last evaluated and
/// what it gives arena 0.
// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
fn policy_line<W: Write + Report>(out: &mut W, shared: &Shared) -> fmt::Result
    requires
        crate::sys::may_syscall(),
        shared.wf(),
{
    use crate::policy::{Band, FRAC_ONE};
    proof {
        crate::policy::frac_one_value();
    }
    let p = &shared.policy;
    let tracked mut lent = out.lend();
    if !shared.cgroup_now(Tracked(&mut lent)).tracked() {
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
    // Out of the `writeln!` so that its ghost argument is erased (the
    // `verus!` expansion does not reach into a macro's arguments).
    let tracked mut lent = out.lend();
    let empty_bytes = crate::policy::empty_bytes(shared, Tracked(&mut lent));
    writeln!(
        out,
        " keep {}% empty_bytes {} full_purges {}{} thp {}",
        p.keep16() as u64 * 100 / FRAC_ONE as u64,
        empty_bytes,
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

// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
fn text_arena<W: Write + Report>(out: &mut W, arena: &Arena, s: &ArenaStats, bins: bool) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
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
    // `fig_add` is `+` as the build computes it (the figures have no
    // proved bound).
    writeln!(
        out,
        "  large:               {:>14} {:>12} {:>12} {:>12}  ({} huge runs)",
        s.large_allocated,
        s.large_active,
        fig_add(s.large_dirty, s.huge_dirty),
        fig_add(s.large_retained, s.huge_retained),
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
    // `for (j, b) in s.bins.iter().enumerate()` with its `continue`, as an
    // indexed loop and an `if`: Verus specifies neither `Enumerate` nor
    // `continue` in a `for`.
    for j in 0..NCLASSES {
        let b = &s.bins[j];
        if fig_add(fig_add(b.curslabs, b.empty_slabs), b.purged_slabs) != 0 {
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
#[allow(clippy::while_let_loop)]
// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
fn slab_histogram<W: Write + Report>(out: &mut W, arena: &Arena, s: &ArenaStats) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let mut header = false;
    // `for (j, b) in s.bins.iter().enumerate()` and `for i in
    // 0..cs.committed_slabs()`, with their `continue`s, spelled out: Verus
    // specifies neither `Enumerate` nor `continue` in a `for`.
    let mut classes = 0..NCLASSES;
    loop
        invariant
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            classes.start <= classes.end == NCLASSES,
        decreases classes.end - classes.start,
    {
        let j = match classes.next() {
            Some(j) => j,
            None => break,
        };
        let b = &s.bins[j];
        if b.nonfull_slabs == 0 {
            continue;
        }
        let cs = arena.class(j);
        let cls = cs.class();
        let nslots = cls.nslots();
        let mut hist = [0usize; 9];
        let (mut mismatched, mut unlisted, mut slabs) = (0usize, 0usize, 0usize);
        let mut committed = 0..cs.committed_slabs();
        loop
            invariant
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                committed.start <= committed.end <= cs.spec_nslabs(),
                slabs <= committed.start,
                mismatched <= committed.start,
                unlisted <= committed.start,
                forall|k: int| 0 <= k < 9 ==> hist[k] <= committed.start,
                *cls == class_table()[cs.spec_class() as int],
                cls.wf(),
                nslots == cls.nslots,
            decreases committed.end - committed.start,
        {
            let i = match committed.next() {
                Some(i) => i,
                None => break,
            };
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
            let tracked mut lent = out.lend();
            if live < nslots && !cs.is_partial(i, Tracked(&mut lent)) {
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
        hist_line(out, b.size, j, slabs, hist, mismatched, unlisted)?;
    }
    Ok(())
}

/// One class's line of [`slab_histogram`]. Trusted: `{:?}` of an array is
/// outside what vstd's formatting specifies; a write has no other effect
/// the proofs track.
#[verifier::external_body]
fn hist_line<W: Write>(
    out: &mut W,
    size: usize,
    j: usize,
    slabs: usize,
    hist: [usize; 9],
    mismatched: usize,
    unlisted: usize,
) -> fmt::Result {
    writeln!(
        out,
        "  {:>13} {:>3} slabs {:>6}  hist {:?}  live!=bits {}  nonfull not in partial set {}",
        size, j, slabs, hist, mismatched, unlisted
    )
}

/// The text report; `tmp` is the [`stats::Scratch`] record each arena's
/// figures are collected into in turn, and `features` the feature line.
fn text_report<W: Write + Report>(out: &mut W, shared: &Shared, o: &Opts, tmp: &mut ArenaStats, features: &str) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    let tracked mut lent = out.lend();
    let snap = stats::collect(shared, tmp, Tracked(&mut lent));
    if !o.no_general {
        text_general(out, shared, &snap, features)?;
    }
    if !o.no_arenas {
        // Every arena with its live figures: a scan of every index below
        // the count (`stats::arena_at`). An error returns at once.
        let n = shared.arenas.count() as usize;
        for i in 0..n
            invariant
                n <= crate::arena::MAX_ARENAS,
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                shared.wf(),
        {
            if let Some(a) = stats::arena_at(shared, i) {
                let tracked mut lent = out.lend();
                stats::collect_arena(shared, a, tmp, Tracked(&mut lent));
                text_arena(out, a, tmp, !o.no_bins)?;
            }
        }
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
// As in `text_general`.
#[allow(clippy::needless_range_loop)]
// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
fn json_general<W: Write + Report>(
    out: &mut W,
    shared: &Shared,
    snap: &Snapshot,
    arenas_follow: bool,
) -> fmt::Result
    requires
        crate::sys::may_syscall(),
        shared.wf(),
{
    proof {
        shared.lemma_wf();
    }
    writeln!(
        out,
        "\t\t\"config\": {{\"debug\": false, \"stats\": true, \"quantum\": {QUANTUM}}},"
    )?;
    writeln!(out, "\t\t\"opt\": {{")?;
    let mut first = true;
    // `for key in KEYS` with its `let .. else { continue }`, with an index
    // and an `if let`: Verus does not specify a slice's `IntoIterator`.
    let keys = KEYS;
    for k in 0..keys.len() {
        let key = &keys[k];
        if let Some(v) = shared.config.get(key.as_bytes()) {
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
        match crate::thp::host_mode() {
            Some(m) => m.name(),
            None => "none",
        }
    )?;
    let tracked mut lent = out.lend();
    // As last sampled, by `sample_cgroup` at the entry point.
    let cg = shared.cgroup_now(Tracked(&mut lent));
    // Out of the `writeln!`, as in `policy_line`.
    let tracked mut lent = out.lend();
    let empty_bytes = crate::policy::empty_bytes(shared, Tracked(&mut lent));
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
        empty_bytes,
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
// Out of line: the report's sections' formatting frames do not add up
// in one (design 13.8).
#[inline(never)]
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
        fig_add(s.large_dirty, s.huge_dirty),
        fig_add(s.large_retained, s.huge_retained),
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
        // `for (j, b) in s.bins[..NBINS].iter().enumerate()`, with an
        // index.
        let nbins = nbins();
        for j in 0..nbins
            invariant
                nbins <= NCLASSES,
        {
            let b = &s.bins[j];
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
/// [`json_arena`]s, each under its option letter as in [`text_report`]
/// (`tmp` as there).
fn json_report<W: Write + Report>(out: &mut W, shared: &Shared, o: &Opts, tmp: &mut ArenaStats) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    let tracked mut lent = out.lend();
    let snap = stats::collect(shared, tmp, Tracked(&mut lent));
    writeln!(out, "{{")?;
    writeln!(out, "\t\"jemalloc\": {{")?;
    writeln!(out, "\t\t\"version\": \"{}\",", super::mallctl::VERSION)?;
    // The comma only when a member follows: `g` and `a` together leave
    // `implementation` the object's last member.
    writeln!(
        out,
        "\t\t\"implementation\": \"challoc\"{}",
        if o.no_general && o.no_arenas { "" } else { "," }
    )?;
    if !o.no_general {
        json_general(out, shared, &snap, !o.no_arenas)?;
    }
    if !o.no_arenas {
        writeln!(out, "\t\t\"stats.arenas\": {{")?;
        // Every arena, as in `text_report`.
        let mut first = true;
        let n = shared.arenas.count() as usize;
        for i in 0..n
            invariant
                n <= crate::arena::MAX_ARENAS,
                crate::sys::may_abort(),
                crate::sys::may_syscall(),
                shared.wf(),
        {
            if let Some(a) = stats::arena_at(shared, i) {
                let tracked mut lent = out.lend();
                stats::collect_arena(shared, a, tmp, Tracked(&mut lent));
                if !first {
                    writeln!(out, ",")?;
                }
                first = false;
                json_arena(out, a, tmp, !o.no_bins)?;
            }
        }
        writeln!(out)?;
        writeln!(out, "\t\t}}")?;
    }
    writeln!(out, "\t}}")?;
    writeln!(out, "}}")
}

/// [`NBINS`], which is at most `NCLASSES` (`layout.rs` asserts it at
/// compile time; the verifier cannot see the `exec const`'s value), as a
/// constant the compiler folds.
#[inline(always)]
fn nbins() -> (r: usize)
    ensures
        r <= NCLASSES,
{
    let n = NBINS;
    if n < NCLASSES { n } else { NCLASSES }
}

/// [`malloc_stats_print`] into `target`, `opts` read through the call's
/// token `cc`, whose reads end before the first call back into C (the
/// sink's `write_cb`, which may change the string).
#[inline(always)]
fn stats_print(target: Target, opts: *const c_char, Tracked(cc): Tracked<&mut cbuf::CCall>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        // The C contract of `opts` (`malloc_stats_print`'s caller's).
        opts@.addr != 0 ==> old(cc).reads_str(opts as *const u8),
{
    let shared = super::shared();
    let o = Opts::parse(opts, Tracked(&*cc));
    proof {
        if opts@.addr != 0 {
            cc.open_writes(cbuf::CArg::Str(opts as *const u8));
        }
    }
    // Premise (lock order): the report's C caller holds no allocator
    // lock, and `shared` releases every lock it takes before it returns.
    let tracked mut ord = axiom_entry();
    // The general section's two deep calls, made here rather than below
    // the report's frames (design 13.8): the cgroup sample and the
    // feature line (built at its first request).
    let mut features = "";
    if !o.no_general {
        sample_cgroup(shared, Tracked(&mut ord));
        if !o.json {
            features = super::feature_line();
        }
    }
    stats_report(target, shared, &o, features, Tracked(ord));
}

/// The general section's cgroup sample (design 11; taken if the 10 ms
/// window allows), from the entry point's frame rather than below the
/// report's: the sample reads the cgroup's files, the deepest call a
/// report makes (design 13.8). [`cgroup_line`] and [`json_general`] print
/// it.
#[inline(never)]
fn sample_cgroup(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_syscall(),
        old(ord).allows(crate::lock::rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    shared.cgroup_now(Tracked(ord)).sample(crate::sys::now_coarse_ns());
}

/// The report of [`stats_print`] into `target` with the lock order token
/// `ord` and the feature line `features`, out of line so its sink and
/// frames are below the entry point's.
#[inline(never)]
fn stats_report(target: Target, shared: &Shared, o: &Opts, features: &str, Tracked(ord): Tracked<LockOrder<'static>>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        ord.top() == 0,
{
    // The record each arena's figures are collected into (about 6 KiB
    // with its bins), taken before any lock rather than on the caller's
    // stack, and given back before the report's last write.
    let tracked mut ord = ord;
    let scratch = stats::Scratch::new(&shared.meta, Tracked(&mut ord));
    let mut sink = Sink::new(target, Tracked(ord));
    match scratch {
        Some(mut sc) => {
            let _ = if o.json {
                json_report(&mut sink, shared, o, &mut sc.record().a)
            } else {
                text_report(&mut sink, shared, o, &mut sc.record().a, features)
            };
            let tracked mut lent = sink.lend();
            sc.free(&shared.meta, Tracked(&mut lent));
        },
        None => {
            let _ = no_memory(&mut sink);
        },
    }
    sink.flush();
}

/// A report's one line when there is no metadata object for its scratch
/// records.
fn no_memory<W: Write>(out: &mut W) -> fmt::Result {
    writeln!(out, "(no memory)")
}

/// `malloc_stats_print(write_cb, cbopaque, opts)`. The C entry points
/// take `write_cb` as a [`WriteCallback`] (its ABI is the function
/// pointer's), and `cc` is the call's token, minted
/// for exactly these arguments ([`cbuf::CEntry::StatsPrint`]) by
/// `exports::stats_print_entry`.
pub fn malloc_stats_print(write_cb: WriteCallback, cbopaque: *mut c_void, opts: *const c_char, Tracked(cc): Tracked<cbuf::CCall>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        cc.fresh(cbuf::CEntry::StatsPrint { opts: opts as *const u8 }),
{
    let tracked mut cc = cc;
    stats_print(Target::Callback(write_cb, cbopaque), opts, Tracked(&mut cc))
}

// ---------------------------------------------------------------------------
// glibc.
// ---------------------------------------------------------------------------

fn xml_heap<W: Write>(out: &mut W, nr: usize, s: &ArenaStats) -> fmt::Result {
    writeln!(out, "<heap nr=\"{nr}\">")?;
    writeln!(out, "<sizes>")?;
    let mut count = 0usize;
    // `for b in s.bins[..NBINS].iter()` with its `continue`, with an index
    // and an `if`.
    let nbins = nbins();
    for j in 0..nbins
        invariant
            nbins <= NCLASSES,
    {
        let b = &s.bins[j];
        if b.curregs != 0 {
            count = fig_add(count, b.curregs);
            writeln!(
                out,
                "<size from=\"{}\" to=\"{}\" total=\"{}\" count=\"{}\"/>",
                b.size,
                b.size,
                fig_mul(b.curregs, b.size),
                b.curregs
            )?;
        }
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

/// The `malloc_info` document; `sum` and `tmp` are the [`stats::Scratch`]
/// records of the totals and of each arena's figures in turn.
fn xml_report<W: Write + Report>(out: &mut W, shared: &Shared, sum: &mut ArenaStats, tmp: &mut ArenaStats) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    writeln!(out, "<malloc version=\"1\">")?;
    // Every arena, as in `text_report`.
    let n = shared.arenas.count() as usize;
    for i in 0..n
        invariant
            n <= crate::arena::MAX_ARENAS,
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
    {
        if let Some(a) = stats::arena_at(shared, i) {
            let tracked mut lent = out.lend();
            stats::collect_arena(shared, a, tmp, Tracked(&mut lent));
            xml_heap(out, a.index() as usize, tmp)?;
        }
    }
    let tracked mut rd: Ghost<(Seq<stats::ArenaStats>, Seq<int>)> = Ghost(arbitrary());
    let tracked mut lent = out.lend();
    stats::collect_all(shared, sum, tmp, Tracked(&mut rd), Tracked(&mut lent));
    let all: &ArenaStats = sum;
    // The two `iter().map(..).sum()`s over `bins[..NBINS]` and
    // `bins[NBINS..]`, as one indexed loop.
    let (mut count, mut large) = (0usize, 0usize);
    for j in 0..NCLASSES {
        if j < NBINS {
            count = fig_add(count, all.bins[j].curregs);
        } else {
            large = fig_add(large, all.bins[j].curregs);
        }
    }
    let large_count = fig_add(all.large_runs, large);
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
pub fn malloc_info(options: c_int, stream: *mut libc::FILE, Tracked(errno): Tracked<&mut ThreadErrno>) -> (r: c_int)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
    ensures
        r == 0 || r == -1,
        r == -1 <==> options != 0 || stream@.addr == 0,
        r == -1 ==> final(errno).val() == crate::sys::Errno::EINVAL.0,
{
    if options != 0 || stream.is_null() {
        set_errno(crate::sys::Errno::EINVAL.0, Tracked(errno));
        return -1;
    }
    let shared = super::shared();
    // Premise (lock order): the report's C caller holds no allocator
    // lock, and `shared` releases every lock it takes before it returns.
    let tracked mut ord = axiom_entry();
    // The totals' and each arena's records, as in `stats_print`.
    let scratch = stats::Scratch::new(&shared.meta, Tracked(&mut ord));
    let mut sink = Sink::new(file_target(stream), Tracked(ord));
    pair_report(&mut sink, shared, scratch, true);
    sink.flush();
    0
}

/// glibc's `malloc_stats()`: a few lines on stderr in glibc's layout, with
/// challoc's meaning of each figure (see `glibc.rs` for the mapping).
pub fn malloc_stats()
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let shared = super::shared();
    // Premise (lock order): the report's C caller holds no allocator
    // lock, and `shared` releases every lock it takes before it returns.
    let tracked mut ord = axiom_entry();
    // The totals' and each arena's records, as in `stats_print`.
    let scratch = stats::Scratch::new(&shared.meta, Tracked(&mut ord));
    let mut sink = Sink::new(stderr_target(), Tracked(ord));
    pair_report(&mut sink, shared, scratch, false);
    sink.flush();
}

/// [`xml_report`] (`xml`) or [`glibc_stats`] into `sink` with the
/// scratch's two records, which it gives back; a [`no_memory`] line when
/// there is none.
fn pair_report<W: Write + Report>(sink: &mut W, shared: &Shared, scratch: Option<stats::Scratch>, xml: bool)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        scratch matches Some(s) ==> s.wf() && s.meta_id() == shared.meta.live_id(),
{
    match scratch {
        Some(mut sc) => {
            let pair = sc.record();
            let _ = if xml {
                xml_report(sink, shared, &mut pair.a, &mut pair.b)
            } else {
                glibc_stats(sink, shared, &mut pair.a, &mut pair.b)
            };
            let tracked mut lent = sink.lend();
            sc.free(&shared.meta, Tracked(&mut lent));
        },
        None => {
            let _ = no_memory(sink);
        },
    }
}

/// The lines of [`malloc_stats`]: the immediately called closure, as a
/// function.
fn glibc_stats<W: Write + Report>(sink: &mut W, shared: &Shared, sum: &mut ArenaStats, tmp: &mut ArenaStats) -> fmt::Result
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    let n = shared.arenas.count() as usize;
    for i in 0..n
        invariant
            n <= crate::arena::MAX_ARENAS,
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            shared.wf(),
    {
        if let Some(a) = stats::arena_at(shared, i) {
            let tracked mut lent = sink.lend();
            stats::collect_arena(shared, a, tmp, Tracked(&mut lent));
            let s: &ArenaStats = tmp;
            writeln!(sink, "Arena {}:", a.index())?;
            writeln!(
                sink,
                "system bytes     = {:>10}",
                s.resident.saturating_sub(s.large_allocated)
            )?;
            writeln!(sink, "in use bytes     = {:>10}", s.small_allocated)?;
        }
    }
    let tracked mut rd: Ghost<(Seq<stats::ArenaStats>, Seq<int>)> = Ghost(arbitrary());
    let tracked mut lent = sink.lend();
    stats::collect_all(shared, sum, tmp, Tracked(&mut rd), Tracked(&mut lent));
    let all: &ArenaStats = sum;
    writeln!(sink, "Total (incl. mmap):")?;
    writeln!(sink, "system bytes     = {:>10}", all.resident)?;
    writeln!(sink, "in use bytes     = {:>10}", fig_add(all.small_allocated, all.large_allocated))?;
    writeln!(sink, "max mmap regions = {:>10}", all.large_runs)?;
    writeln!(sink, "max mmap bytes   = {:>10}", all.large_allocated)
}

/// `Target::File(stream)`.
#[inline(always)]
fn file_target(stream: *mut libc::FILE) -> Target {
    Target::File(stream)
}

/// `Target::Stderr`.
#[inline(always)]
fn stderr_target() -> Target {
    Target::Stderr
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    #[cfg(verus_keep_ghost)]
    use crate::vspec::cbuf::{CCall, CEntry};
    use core::ffi::{c_char, c_int, c_void};
    use vstd::prelude::{Tracked, verus};

    // `verified_export_pair!` expands inside its own `verus!`; this outer
    // block holds it (Verus passes the invocation through) and the mint.
    verus! {

    /// Trusted (the C contract of the entry point, `vspec::cbuf`): the
    /// token of this call from C, through the entry point and with the
    /// arguments `e` names, which C passed under its contract
    /// ([`CCall::fresh`]: the string `opts`, of memory of the program's,
    /// and its view). Private to this module, whose functions are the C
    /// entry points: [`stats_print_entry`], the verified half of the
    /// `malloc_stats_print` exports, mints its call's token once, from its
    /// own parameters. A string that breaks the contract (not
    /// NUL-terminated, wrapping the address space, in challoc's memory
    /// other than a published counter) makes it false.
    /// `stats_print_entry` is its one caller and is called once, by the
    /// export pair: checked by
    /// `api::tests::c_call_tokens_only_in_export_bodies`.
    axiom fn axiom_c_call(e: CEntry) -> (tracked c: CCall)
        requires
            e is StatsPrint,
        ensures
            c.fresh(e),
    ;

    /// The body of the `malloc_stats_print` exports: the call's token for
    /// `opts`, and the Rust entry point.
    #[inline(always)]
    fn stats_print_entry(write_cb: super::WriteCallback, cbopaque: *mut c_void, opts: *const c_char)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
    {
        let tracked cc = axiom_c_call(CEntry::StatsPrint { opts: opts as *const u8 });
        super::malloc_stats_print(write_cb, cbopaque, opts, Tracked(cc))
    }

    verified_export_pair! {
        fn malloc_stats_print / je_malloc_stats_print(write_cb: super::WriteCallback, cbopaque: *mut c_void, opts: *const c_char) {
            stats_print_entry(write_cb, cbopaque, opts)
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_info(options: c_int, stream: *mut libc::FILE) -> c_int {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        let Tracked(mut errno) = crate::vspec::errno::lend();
        super::malloc_info(options, stream, Tracked(&mut errno))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn malloc_stats() {
        proof {
            crate::sys::grant_syscalls();
            crate::sys::grant_aborts();
        }
        super::malloc_stats()
    }

    } // verus!
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::libc::{free, malloc};
    use std::string::String;
    use std::sync::Mutex;
    use std::vec::Vec;

    /// The entry point as the export wrapper calls it, with a placeholder
    /// token for the call.
    fn malloc_stats_print(write_cb: WriteCallback, cbopaque: *mut c_void, opts: *const c_char) {
        super::malloc_stats_print(write_cb, cbopaque, opts, Tracked::assume_new())
    }

    static CAPTURE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    /// Held across a whole `report`: the callback appends one chunk at a
    /// time under `CAPTURE`'s lock, so two tests reporting at once would
    /// interleave their chunks (seen as a text report that began with
    /// another test's JSON report).
    static REPORT: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn capture(opaque: *mut c_void, s: *const c_char) {
        assert_eq!(opaque as usize, 0xC0FFEE);
        // SAFETY: the hook is handed a NUL-terminated chunk.
        let s = unsafe { core::ffi::CStr::from_ptr(s) };
        CAPTURE.lock().unwrap().extend_from_slice(s.to_bytes());
    }

    fn report(opts: &str) -> String {
        let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        CAPTURE.lock().unwrap().clear();
        let c = std::ffi::CString::new(opts).unwrap();
        malloc_stats_print(
            WriteCallback(Some(capture)),
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
        let mut sink = Sink::new(
            Target::Callback(WriteCallback(Some(capture)), 0xC0FFEE as *mut c_void),
            Tracked::assume_new(),
        );
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
        let live: Vec<*mut u8> = (0..100)
            .map(|_| malloc(700, vstd::prelude::Tracked::assume_new()))
            .collect();
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
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
        free(
            malloc(64, vstd::prelude::Tracked::assume_new()),
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let counters = || {
            let st = crate::heap::heap_for_thread(s).unwrap().materialise_stats();
            (st.allocated, st.deallocated)
        };
        let before = counters();
        let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        malloc_stats_print(
            WriteCallback(Some(sink)),
            core::ptr::null_mut(),
            core::ptr::null(),
        );
        let json = std::ffi::CString::new("J").unwrap();
        malloc_stats_print(
            WriteCallback(Some(sink)),
            core::ptr::null_mut(),
            json.as_ptr(),
        );
        assert_eq!(
            counters(),
            before,
            "a report moved this thread's allocation counters"
        );
    }

    #[test]
    fn stats_print_without_callback_uses_the_message_hook() {
        let _serial = crate::testutil::global_state();
        use crate::api::jemalloc::set_message_hook_for_tests;
        static HOOKED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
            assert!(opaque.is_null());
            HOOKED
                .lock()
                .unwrap()
                // SAFETY: the hook is handed a NUL-terminated message.
                .extend_from_slice(unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes());
        }
        set_message_hook_for_tests(Some(hook));
        malloc_stats_print(
            WriteCallback(None),
            core::ptr::null_mut(),
            core::ptr::null(),
        );
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
        let live: Vec<*mut u8> = (0..50)
            .map(|_| malloc(3000, vstd::prelude::Tracked::assume_new()))
            .collect();
        let big = malloc(1 << 20, vstd::prelude::Tracked::assume_new());
        // SAFETY: `tmpfile` has no preconditions.
        let f = unsafe { libc::tmpfile() };
        assert!(!f.is_null());
        assert_eq!(malloc_info(0, f, Tracked::assume_new()), 0);
        // SAFETY: `f` is the open stream from `tmpfile`.
        unsafe { libc::rewind(f) };
        let mut buf = std::vec![0u8; 1 << 16];
        // SAFETY: `buf` has `buf.len()` writable bytes; `f` is open.
        let n = unsafe { libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), f) };
        // SAFETY: `f` is open; the `malloc_info(1, f)` below rejects its
        // options before it would touch the closed stream.
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
        if let Err(e) = xml_check(&xml) {
            panic!("malloc_info is not well-formed XML: {e}\n{xml}");
        }
        assert_eq!(malloc_info(1, f, Tracked::assume_new()), -1);
        // SAFETY: `__errno_location` returns this thread's `errno`.
        assert_eq!(unsafe { *libc::__errno_location() }, 22);
        assert_eq!(
            malloc_info(0, core::ptr::null_mut(), Tracked::assume_new()),
            -1
        );
        free(
            big,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        for p in live {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        // glibc's malloc_stats just has to run and stay on stderr.
        malloc_stats();
    }

    /// How long a report that re-enters the allocator may take; one that
    /// has not finished by then is deadlocked.
    const REENTRY_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

    /// What a report's external call does in the re-entry tests: allocate
    /// and free a small bin (16), a larger bin (3000), a slab-allocated
    /// large class (64 KiB) and a huge run (4 MiB), collect the live
    /// statistics (the locks a report's collection takes; `epoch` would
    /// also take them, but it moves the process-wide epoch other tests
    /// read), and print a nested report, which takes every lock the outer
    /// one takes. The nested report's callback discards its text and does
    /// not re-enter. A lock held only over a stretch in which no chunk is
    /// flushed is not caught.
    fn reenter() {
        let live: Vec<*mut u8> = [16usize, 3000, 64 << 10, 4 << 20]
            .iter()
            .map(|&n| malloc(n, vstd::prelude::Tracked::assume_new()))
            .collect();
        for p in live {
            assert!(!p.is_null());
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        let mut tmp = stats::ArenaStats::empty(0);
        let _ = stats::collect(crate::api::shared(), &mut tmp, Tracked::assume_new());
        unsafe extern "C" fn discard(_: *mut c_void, _: *const c_char) {}
        malloc_stats_print(
            WriteCallback(Some(discard)),
            core::ptr::null_mut(),
            core::ptr::null(),
        );
    }

    /// A `write_cb` that counts its calls in the `AtomicUsize` behind
    /// `opaque` and re-enters the allocator.
    unsafe extern "C" fn reentering_cb(opaque: *mut c_void, _s: *const c_char) {
        // SAFETY: every caller passes a live `AtomicUsize` as `opaque`.
        let calls = unsafe { &*(opaque as *const core::sync::atomic::AtomicUsize) };
        calls.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        reenter();
    }

    /// Run `report` on its own thread and fail unless it finishes within
    /// [`REENTRY_BOUND`]. A deadlocked report keeps the lock it waits
    /// behind, so every later test that needs that lock would hang too:
    /// on a timeout the process aborts after saying why.
    fn finishes(what: &str, report: impl FnOnce() + Send + 'static) {
        use std::sync::mpsc::RecvTimeoutError;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            report();
            let _ = tx.send(());
        });
        match rx.recv_timeout(REENTRY_BOUND) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                std::eprintln!(
                    "{what} did not finish within {REENTRY_BOUND:?}: \
                     an allocator lock held across its external call?"
                );
                std::process::abort();
            }
            Err(RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
        }
    }

    /// The run-time evidence for `call_back`'s premise that the calling
    /// thread holds no allocator lock (`ord.top() == 0`): every call of a
    /// text and a JSON report re-enters the allocator ([`reenter`]). A
    /// lock a report takes, held across a call, would deadlock the call's
    /// collection or nested report.
    #[test]
    fn a_report_callback_may_reenter_the_allocator() {
        for opts in ["", "J"] {
            finishes(&std::format!("malloc_stats_print({opts:?})"), move || {
                let calls = core::sync::atomic::AtomicUsize::new(0);
                let c = std::ffi::CString::new(opts).unwrap();
                malloc_stats_print(
                    WriteCallback(Some(reentering_cb)),
                    &calls as *const _ as *mut c_void,
                    c.as_ptr(),
                );
                assert!(
                    calls.load(core::sync::atomic::Ordering::Relaxed) > 1,
                    "the report fits one chunk: nothing was called mid-report"
                );
            });
        }
    }

    /// As [`a_report_callback_may_reenter_the_allocator`], for
    /// `write_file`'s `fwrite`: an unbuffered `fopencookie` stream whose
    /// write function re-enters the allocator, so every `fwrite` the sink
    /// makes calls it before returning. A block live in every bin class
    /// gives the XML a `<size>` row per bin, so it outgrows the sink's
    /// 1023-byte buffer and at least one write lands mid-report.
    #[test]
    fn a_malloc_info_stream_may_reenter_the_allocator() {
        /// glibc's `cookie_io_functions_t`.
        #[repr(C)]
        struct CookieIo {
            read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> isize>,
            write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> isize>,
            seek: Option<unsafe extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int>,
            close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        }
        unsafe extern "C" {
            fn fopencookie(
                cookie: *mut c_void,
                mode: *const c_char,
                io: CookieIo,
            ) -> *mut libc::FILE;
        }
        unsafe extern "C" fn write(cookie: *mut c_void, _buf: *const c_char, n: usize) -> isize {
            // SAFETY: the cookie is the test's `AtomicUsize`.
            unsafe { reentering_cb(cookie, core::ptr::null()) };
            n as isize
        }
        finishes("malloc_info", || {
            let live: Vec<*mut u8> = (0..NBINS)
                .map(|c| malloc(CLASSES[c].size(), vstd::prelude::Tracked::assume_new()))
                .collect();
            let calls = core::sync::atomic::AtomicUsize::new(0);
            let io = CookieIo {
                read: None,
                write: Some(write),
                seek: None,
                close: None,
            };
            // SAFETY: `calls` outlives the stream, which is closed below;
            // the mode is a C string.
            let f = unsafe { fopencookie(&calls as *const _ as *mut c_void, c"w".as_ptr(), io) };
            assert!(!f.is_null());
            // SAFETY: `f` is open and has not been written to.
            let unbuffered = unsafe { libc::setvbuf(f, core::ptr::null_mut(), libc::_IONBF, 0) };
            assert_eq!(unbuffered, 0);
            assert_eq!(malloc_info(0, f, Tracked::assume_new()), 0);
            let during = calls.load(core::sync::atomic::Ordering::Relaxed);
            // SAFETY: `f` is the open stream from `fopencookie`.
            unsafe { libc::fclose(f) };
            for p in live {
                free(
                    p,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
            assert!(
                during > 1,
                "the XML fits one chunk: nothing was written mid-report"
            );
        });
    }

    /// A strict JSON (RFC 8259) parser that only validates: `Err` names
    /// the byte offset of the first error. Enough for the reports, which
    /// a consumer (tikv-jemalloc-ctl's users, `jq`) parses as JSON.
    fn json_check(text: &str) -> Result<(), String> {
        struct P<'a> {
            b: &'a [u8],
            i: usize,
        }
        impl P<'_> {
            fn err<T>(&self, what: &str) -> Result<T, String> {
                let ctx = &self.b[self.i.saturating_sub(40)..self.b.len().min(self.i + 20)];
                Err(std::format!(
                    "{what} at byte {}: ...{}...",
                    self.i,
                    String::from_utf8_lossy(ctx)
                ))
            }
            fn ws(&mut self) {
                while self.i < self.b.len() && b" \t\n\r".contains(&self.b[self.i]) {
                    self.i += 1;
                }
            }
            fn eat(&mut self, c: u8) -> bool {
                self.ws();
                if self.b.get(self.i) == Some(&c) {
                    self.i += 1;
                    true
                } else {
                    false
                }
            }
            fn value(&mut self) -> Result<(), String> {
                self.ws();
                match self.b.get(self.i) {
                    Some(b'{') => {
                        self.i += 1;
                        if self.eat(b'}') {
                            return Ok(());
                        }
                        loop {
                            self.ws();
                            if self.b.get(self.i) != Some(&b'"') {
                                return self.err("expected a key");
                            }
                            self.string()?;
                            if !self.eat(b':') {
                                return self.err("expected ':'");
                            }
                            self.value()?;
                            if self.eat(b'}') {
                                return Ok(());
                            }
                            if !self.eat(b',') {
                                return self.err("expected ',' or '}'");
                            }
                        }
                    }
                    Some(b'[') => {
                        self.i += 1;
                        if self.eat(b']') {
                            return Ok(());
                        }
                        loop {
                            self.value()?;
                            if self.eat(b']') {
                                return Ok(());
                            }
                            if !self.eat(b',') {
                                return self.err("expected ',' or ']'");
                            }
                        }
                    }
                    Some(b'"') => self.string(),
                    Some(b't') => self.word("true"),
                    Some(b'f') => self.word("false"),
                    Some(b'n') => self.word("null"),
                    Some(b'-' | b'0'..=b'9') => self.number(),
                    _ => self.err("expected a value"),
                }
            }
            fn word(&mut self, w: &str) -> Result<(), String> {
                if self.b[self.i..].starts_with(w.as_bytes()) {
                    self.i += w.len();
                    Ok(())
                } else {
                    self.err("bad literal")
                }
            }
            fn string(&mut self) -> Result<(), String> {
                self.i += 1;
                while let Some(&c) = self.b.get(self.i) {
                    self.i += 1;
                    match c {
                        b'"' => return Ok(()),
                        b'\\' => match self.b.get(self.i) {
                            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                                self.i += 1
                            }
                            Some(b'u') => {
                                let hex = self.b.get(self.i + 1..self.i + 5);
                                if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                    return self.err("bad \\u escape");
                                }
                                self.i += 5;
                            }
                            _ => return self.err("bad escape"),
                        },
                        0..=0x1f => return self.err("control character in a string"),
                        _ => {}
                    }
                }
                self.err("unterminated string")
            }
            fn number(&mut self) -> Result<(), String> {
                let digits = |p: &mut Self| {
                    let s = p.i;
                    while p.b.get(p.i).is_some_and(u8::is_ascii_digit) {
                        p.i += 1;
                    }
                    p.i > s
                };
                if self.b[self.i] == b'-' {
                    self.i += 1;
                }
                if self.b.get(self.i) == Some(&b'0') {
                    self.i += 1;
                } else if !digits(self) {
                    return self.err("bad number");
                }
                if self.b.get(self.i) == Some(&b'.') {
                    self.i += 1;
                    if !digits(self) {
                        return self.err("bad fraction");
                    }
                }
                if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
                    self.i += 1;
                    if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                        self.i += 1;
                    }
                    if !digits(self) {
                        return self.err("bad exponent");
                    }
                }
                Ok(())
            }
        }
        let mut p = P {
            b: text.as_bytes(),
            i: 0,
        };
        p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return p.err("trailing bytes after the document");
        }
        Ok(())
    }

    /// A minimal XML well-formedness check for `malloc_info`'s output:
    /// one root element, every tag closed in order, attributes
    /// `name="value"` with no `<` or `&` in a value, no stray text.
    fn xml_check(text: &str) -> Result<(), String> {
        let mut stack: Vec<&str> = Vec::new();
        let mut rest = text;
        let mut roots = 0;
        loop {
            let t = rest.trim_start();
            if t.is_empty() {
                break;
            }
            if !t.starts_with('<') {
                return Err(std::format!("text outside a tag: {:.40}", t));
            }
            let end = t.find('>').ok_or("unterminated tag")?;
            let tag = &t[1..end];
            rest = &t[end + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                match stack.pop() {
                    Some(open) if open == name => {}
                    other => return Err(std::format!("</{name}> closes {other:?}")),
                }
                continue;
            }
            let (body, empty) = match tag.strip_suffix('/') {
                Some(b) => (b, true),
                None => (tag, false),
            };
            let (name, mut attrs) = body.split_once(' ').unwrap_or((body, ""));
            if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(std::format!("bad element name in <{tag}>"));
            }
            loop {
                attrs = attrs.trim_start();
                if attrs.is_empty() {
                    break;
                }
                let (key, v) = attrs
                    .split_once("=\"")
                    .ok_or(std::format!("bad attribute in <{tag}>"))?;
                if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                    return Err(std::format!("bad attribute name in <{tag}>"));
                }
                let close = v
                    .find('"')
                    .ok_or(std::format!("unterminated attribute in <{tag}>"))?;
                if v[..close].contains(['<', '&']) {
                    return Err(std::format!("unescaped character in <{tag}>"));
                }
                attrs = &v[close + 1..];
                if !attrs.is_empty() && !attrs.starts_with(' ') {
                    return Err(std::format!("attributes not separated in <{tag}>"));
                }
            }
            if stack.is_empty() {
                roots += 1;
            }
            if !empty {
                stack.push(name);
            } else if stack.is_empty() {
                return Err("empty root element".into());
            }
        }
        if !stack.is_empty() {
            return Err(std::format!("unclosed {stack:?}"));
        }
        if roots != 1 {
            return Err(std::format!("{roots} root elements"));
        }
        Ok(())
    }

    #[test]
    fn json_and_xml_checkers_reject_what_they_should() {
        assert!(json_check("{\"a\": [1, -2.5e3, true, null, \"x\\n\"], \"b\": {}}").is_ok());
        for bad in [
            "{\"a\": 1,}",
            "[1,]",
            "{\"a\" 1}",
            "{\"a\": 01}",
            "{} {}",
            "{\"a\": \"\t\"}",
            "{",
        ] {
            assert!(json_check(bad).is_err(), "{bad}");
        }
        assert!(xml_check("<m v=\"1\">\n<a x=\"2\"/>\n</m>\n").is_ok());
        for bad in [
            "<m><a></m></a>",
            "<m>",
            "<m/>",
            "<m x=1></m>",
            "<m></m><n></n>",
            "<m>t</m>",
        ] {
            assert!(xml_check(bad).is_err(), "{bad}");
        }
    }

    /// Every option string jemalloc's `malloc_stats_print` defines, with
    /// and without `J`: the letters challoc honours (`g`, `a`, `b`) in
    /// every combination, each with none, each one and all of the letters
    /// it accepts without effect (`m`, `d`, `l`, `x`, `e`, `h`). Every
    /// `J` report parses as JSON (`g` with `a` makes `implementation` the
    /// last member of `jemalloc`, with no comma after it); every text
    /// report ends with the end line
    /// and starts with the begin line exactly when `g` is absent.
    #[test]
    fn every_option_string_gives_a_well_formed_report() {
        let live: Vec<*mut u8> = (0..20)
            .map(|i| malloc(64 << (i % 8), vstd::prelude::Tracked::assume_new()))
            .collect();
        let inert = ["", "m", "d", "l", "x", "e", "h", "mdlxeh"];
        for mask in 0..8u32 {
            let mut honoured = String::new();
            for (bit, c) in ['g', 'a', 'b'].into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    honoured.push(c);
                }
            }
            for extra in inert {
                for json in [true, false] {
                    let opts = std::format!("{}{honoured}{extra}", if json { "J" } else { "" });
                    let text = report(&opts);
                    if json {
                        if let Err(e) = json_check(&text) {
                            panic!("malloc_stats_print({opts:?}) is not JSON: {e}\n{text}");
                        }
                    } else {
                        assert!(
                            text.ends_with("--- End jemalloc statistics ---\n"),
                            "{opts:?}: {text}"
                        );
                        assert_eq!(
                            text.starts_with("___ Begin jemalloc statistics ___\n"),
                            !honoured.contains('g'),
                            "{opts:?}: {text}"
                        );
                    }
                }
            }
        }
        for p in live {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
    }
}

/// Kani harnesses for this file's seams that Kani can run
/// (`verus/ledger/src/api/print.rs.md`): `word`, `hist_line` and
/// `call_back`.
#[cfg(kani)]
mod kani_print {
    use super::{WriteCallback, call_back, hist_line, word};
    use core::ffi::{c_char, c_void};
    use core::ptr;
    use vstd::prelude::Tracked;

    /// How many times `record` was called, and the last call's
    /// `cbopaque` and `s` as addresses.
    static mut CALLS: usize = 0;
    static mut GOT: (usize, usize) = (0, 0);

    /// A `write_cb` that records its arguments.
    unsafe extern "C" fn record(opaque: *mut c_void, s: *const c_char) {
        // SAFETY: the harness's own statics; no other code runs in a
        // harness.
        unsafe {
            CALLS += 1;
            GOT = (opaque as usize, s as usize);
        }
    }

    /// `call_back`: no `ensures`; it returns for a callback or none, any
    /// `opaque` and any buffer holding a NUL (its `requires`; the token is
    /// ghost), and (beyond the contract) calls the callback once, with
    /// `opaque` and the buffer's address, when there is one, else not at
    /// all, and returns whether it called. What the program's callback
    /// does is premise C3 (`verus/kani/premises.md`).
    #[kani::proof]
    fn call_back_calls_once() {
        let cb = WriteCallback(if kani::any() { Some(record) } else { None });
        let opaque: *mut c_void = ptr::without_provenance_mut(kani::any());
        let buf: [u8; 1024] = kani::any();
        let nul: usize = kani::any();
        kani::assume(nul < 1024 && buf[nul] == 0);
        let called = call_back(&cb, opaque, &buf, Tracked::assume_new());
        // SAFETY: as in `record`.
        let (calls, got) = unsafe { (CALLS, GOT) };
        assert_eq!(called, cb.0.is_some());
        assert_eq!(calls, usize::from(called));
        if called {
            assert_eq!(got, (opaque as usize, buf.as_ptr() as usize));
        }
    }

    /// `word`, no `requires`, no `ensures`: it returns for every C string
    /// up to 3 bytes before its NUL, UTF-8 or not (built at run time:
    /// Kani 0.68 has no C string literals), and gives the string when it
    /// is UTF-8, else `?`.
    #[kani::proof]
    #[kani::unwind(6)]
    fn word_returns() {
        let a: [u8; 4] = kani::any();
        let n: usize = kani::any();
        kani::assume(n < 4 && a[n] == 0 && a[..n].iter().all(|&b| b != 0));
        let s = core::ffi::CStr::from_bytes_with_nul(&a[..=n]).unwrap();
        let w = word(s);
        assert_eq!(
            w.as_bytes() == &a[..n],
            core::str::from_utf8(&a[..n]).is_ok()
        );
        assert!(w == "?" || w.as_bytes() == &a[..n]);
    }

    /// `hist_line`, no `requires`, no `ensures`: it returns for every
    /// argument, whatever the writer returns (`Sink` may fail any write;
    /// Kani fails the harness on any panic).
    #[kani::proof]
    #[kani::unwind(24)]
    fn hist_line_returns() {
        let hist: [usize; 9] = kani::any();
        let _ = hist_line(
            &mut crate::kani_proofs::fmt::Sink,
            kani::any(),
            kani::any(),
            kani::any(),
            hist,
            kani::any(),
            kani::any(),
        );
    }
}
