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

use core::ffi::{c_char, c_int, c_void};
use core::fmt::{self, Write};

use super::jemalloc::malloc_write;
use crate::QUANTUM;
use crate::arena::Arena;
use crate::config::{INERT_KEYS, KEYS, Name, PercpuArena, Thp, Value};
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

/// glibc's `PTHREAD_CANCEL_DISABLE`, the same on both targets, and its
/// `pthread_setcancelstate`, which the `libc` crate does not bind: what
/// [`cancel_disable`] and [`cancel_restore`] call (premise C9).
const PTHREAD_CANCEL_DISABLE: i32 = 1;
unsafe extern "C" {
    fn pthread_setcancelstate(state: i32, old: *mut i32) -> i32;
    /// glibc's `fwrite(3)`, with the `libc` crate's signature and the same
    /// symbol: what [`write_file`] calls (premise C14 of
    /// `verus/kani/premises.md`). Declared in this crate rather than
    /// taken from the `libc` crate so that `kani_write_file` can stub
    /// it: Kani 0.68 stubs a foreign function only where this crate
    /// declares it, and silently ignores a stub on `libc::fwrite`.
    fn fwrite(ptr: *const c_void, size: usize, nobj: usize, stream: *mut libc::FILE) -> usize;
}

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
/// allocator. To the caller's callback ([`call_back_nocancel`]) as the C string
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
            if !call_back_nocancel(cb, *opaque, buf, Tracked(ord)) {
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

/// [`call_back`] with cancellation disabled, as [`write_file`]'s `fwrite`
/// runs: a deferred `pthread_cancel` is not acted on at a cancellation
/// point inside the caller's callback, unless the callback turns
/// cancellation back on (design 8.1). `ord` has top 0, as for [`emit`].
#[inline(always)]
fn call_back_nocancel(
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
    let old = cancel_disable();
    let called = call_back(cb, opaque, buf, Tracked(ord));
    cancel_restore(old);
    called
}

/// Disable the calling thread's cancellation (`pthread_setcancelstate`);
/// the state it had, for [`cancel_restore`]. Trusted: a call into libc;
/// claims nothing.
#[verifier::external_body]
#[inline(always)]
pub(super) fn cancel_disable() -> i32
    requires
        crate::sys::may_syscall(),
{
    let mut old = 0;
    // SAFETY: `old` is a live int the call writes.
    unsafe { pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut old) };
    old
}

/// Give the calling thread back the cancel state `old` that
/// [`cancel_disable`] returned (`pthread_setcancelstate`). Trusted: a call
/// into libc; claims nothing.
#[verifier::external_body]
#[inline(always)]
pub(super) fn cancel_restore(old: i32)
    requires
        crate::sys::may_syscall(),
{
    let mut old = old;
    // SAFETY: `old` is a live int the call writes.
    unsafe { pthread_setcancelstate(old, &mut old) };
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
/// cancellation point instead (design 8.1). `kani_write_file` checks it
/// on premises C9, C13 and C14 of `verus/kani/premises.md`: the
/// cancel-state calls, the program's stream, and glibc's `fwrite`.
#[verifier::external_body]
#[inline(always)]
fn write_file(f: *mut libc::FILE, s: &[u8], Tracked(ord): Tracked<&LockOrder>)
    requires
        crate::sys::may_syscall(),
        ord.top() == 0,
{
    let old = cancel_disable();
    // SAFETY: `f` is the caller's open stream; `s` is readable.
    unsafe { fwrite(s.as_ptr() as *const c_void, 1, s.len(), f) };
    cancel_restore(old);
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

// New impls go at the end of the module, after this one: rustc numbers a
// module's impls in order, the number enters the mangled names of each
// impl's items, and those names order `.text`, so an impl placed earlier
// would renumber the ones after it.
impl Report for Sink {
    proof fn lend<'b>(tracked &'b mut self) -> (tracked o: LockOrder<'b>) {
        use_type_invariant(&*self);
        self.ord.borrow_mut().lend()
    }
}

/// A configuration value's name (`config::Value::Str`, the `thp` and
/// `percpu_arena` keys): the text of [`Name::cstr`]
/// (`tests::word_is_the_c_name` checks).
fn word(n: Name) -> &'static str {
    match n {
        Name::Thp(Thp::Never) => "never",
        Name::Thp(Thp::Auto) => "auto",
        Name::Thp(Thp::Always) => "always",
        Name::PercpuArena(PercpuArena::Disabled) => "disabled",
        Name::PercpuArena(PercpuArena::Percpu) => "percpu",
        Name::PercpuArena(PercpuArena::Phycpu) => "phycpu",
    }
}

/// A configuration value as both reports print it: a bool or number as
/// itself, a name ([`word`]) in double quotes.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{}", b),
            Value::Int(i) => write!(f, "{}", i),
            Value::Uint(u) => write!(f, "{}", u),
            Value::Size(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "\"{}\"", word(*s)),
        }
    }
}

// `Value` formats under any formatter. Ghost only: ordinary builds have
// no `std_specs`.
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Value {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
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
        if let Some(v) = shared.config.get(key.as_bytes()) {
            writeln!(out, "  opt.{}: {}{}", key, v, note)?;
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
    proof {
        shared.lemma_wf_ready();
    }
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
            purge::ms_of(shared.huge_decay_ns())
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
        word(Name::Thp(crate::thp::key())),
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
        purge::ms_of(p.effective_decay_ns(shared.decay_ns()))
    )?;
    if shared.huge_decay_ns() != shared.decay_ns() {
        write!(
            out,
            " huge_decay_ms {}",
            purge::ms_of(p.effective_decay_ns(shared.huge_decay_ns()))
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
    writeln!(out, "  dirty_decay_ms: {}", purge::ms_of(arena.decay_ns()))?;
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
    // to the bin's capacity (`ClassState::bin_stats`). `closed` is the
    // slots the closed pages of the bin's slabs hide
    // (docs/proposals/slab-pages.md, 3.6), not in `curregs` here.
    writeln!(
        out,
        "  bins:        size ind    nregs  slab_size   curregs  curslabs    closed   nonfull     empty    purged   refilled  compacted   c_on  c_off  gated clamped"
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
                "  {:>13} {:>3} {:>8} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10} {:>6} {:>6} {:>6} {:>7}",
                b.size,
                j,
                b.nregs,
                b.slab_size,
                b.curregs,
                b.curslabs,
                b.closed_slots,
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
            write!(out, "\t\t\t\"{key}\": {v}")?;
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
        word(Name::Thp(crate::thp::key())),
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
        purge::ms_of(shared.policy.effective_decay_ns(shared.decay_ns())),
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
        purge::ms_of(arena.decay_ns())
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
                // As `stats.arenas.<i>.bins.<j>.curregs` reports it: with the
                // closed slots.
                fig_add(b.curregs, b.closed_slots),
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
        r == -1 ==> final(errno).val() == crate::sys::errno::EINVAL,
{
    if options != 0 || stream.is_null() {
        set_errno(crate::sys::errno::EINVAL, Tracked(errno));
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
mod tests;

/// Kani harnesses for this file's seams that Kani can run
/// (`verus/ledger/src/api/print.rs.md`): `hist_line` and `call_back`;
/// `kani_cancel` below checks `cancel_disable` and `cancel_restore`.
#[cfg(kani)]
mod kani_print {
    use super::{WriteCallback, call_back, hist_line};
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

/// Kani checks of `cancel_disable` and `cancel_restore`
/// (`verus/ledger/src/api/print.rs.md`; `cfg(kani)` only, see
/// `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_cancel {
    use super::{cancel_disable, cancel_restore};
    use core::ffi::c_int;

    /// What the stand-in for `pthread_setcancelstate` returns, and the old
    /// state it stores when that is 0.
    static mut RC: c_int = 0;
    static mut OLD: c_int = 0;
    /// How many times it was called, and the last call's `state`.
    static mut CALLS: usize = 0;
    static mut STATE: c_int = 0;

    /// A stand-in for glibc's `pthread_setcancelstate` (premise C9): it
    /// records its call and `state`, checks that `oldstate` is writable,
    /// and returns `RC`, storing `OLD` in `*oldstate` only when `RC` is 0.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn setcancelstate_model(state: c_int, oldstate: *mut c_int) -> c_int {
        assert!(kani::mem::can_write(oldstate));
        // SAFETY: the harness's own statics, and `oldstate`, writable
        // (checked above); no other code runs in a harness.
        unsafe {
            CALLS += 1;
            STATE = state;
            if RC == 0 {
                *oldstate = OLD;
            }
            RC
        }
    }

    /// `cancel_disable`: no `ensures`; it returns, and (beyond the
    /// contract) calls `pthread_setcancelstate` once with
    /// `PTHREAD_CANCEL_DISABLE`, and returns the old state the call stored
    /// when it returned 0, else 0, for any return value and old state.
    #[kani::proof]
    #[kani::stub(super::pthread_setcancelstate, setcancelstate_model)]
    fn disable_keeps_the_old_state() {
        let rc: c_int = kani::any();
        let old: c_int = kani::any();
        kani::cover!(rc == 0);
        kani::cover!(rc != 0);
        // SAFETY: as in `setcancelstate_model`.
        unsafe {
            RC = rc;
            OLD = old;
        }
        let got = cancel_disable();
        // SAFETY: as in `setcancelstate_model`.
        let (calls, state) = unsafe { (CALLS, STATE) };
        assert_eq!(calls, 1);
        // glibc's `PTHREAD_CANCEL_DISABLE` (C9), not this file's constant.
        assert_eq!(state, 1);
        assert_eq!(got, if rc == 0 { old } else { 0 });
    }

    /// `cancel_restore`: no `ensures`; it returns, and (beyond the
    /// contract) calls `pthread_setcancelstate` once with the state it was
    /// given, for any state, return value and old state.
    #[kani::proof]
    #[kani::stub(super::pthread_setcancelstate, setcancelstate_model)]
    fn restore_passes_the_old_state() {
        let given: c_int = kani::any();
        let rc: c_int = kani::any();
        kani::cover!(given == 0);
        kani::cover!(given == 1);
        kani::cover!(given != 0 && given != 1);
        kani::cover!(rc == 0);
        kani::cover!(rc != 0);
        // SAFETY: as in `setcancelstate_model`.
        unsafe {
            RC = rc;
            OLD = kani::any();
        }
        cancel_restore(given);
        // SAFETY: as in `setcancelstate_model`.
        let (calls, state) = unsafe { (CALLS, STATE) };
        assert_eq!(calls, 1);
        assert_eq!(state, given);
    }
}

/// Kani check of `write_file` (`verus/ledger/src/api/print.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_write_file {
    use super::write_file;
    use core::ffi::{c_int, c_void};
    use vstd::prelude::Tracked;

    /// What the stand-in for `pthread_setcancelstate` returns at its
    /// first and second call, and the old state it stores when that is
    /// 0; what the stand-in for `fwrite` returns.
    static mut RC: [c_int; 2] = [0; 2];
    static mut OLD: c_int = 0;
    static mut WRITTEN: usize = 0;
    /// How many calls either stand-in has seen: each records the count
    /// as its call's place in the order.
    static mut STEP: usize = 0;
    /// `pthread_setcancelstate`'s calls, and the place and `state` of
    /// its first two.
    static mut CANCEL_CALLS: usize = 0;
    static mut STATES: [(usize, c_int); 2] = [(0, 0); 2];
    /// `fwrite`'s calls, and the last one's place, the address of its
    /// bytes, its `size` and `n` and the address of its stream.
    static mut WRITES: usize = 0;
    static mut WROTE: (usize, usize, usize, usize, usize) = (0, 0, 0, 0, 0);

    /// A stand-in for glibc's `pthread_setcancelstate` (premise C9): it
    /// records its call, its place and `state`, checks that `oldstate`
    /// is writable, and returns its call's entry of `RC`, storing `OLD`
    /// in `*oldstate` only when that is 0.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn setcancelstate_model(state: c_int, oldstate: *mut c_int) -> c_int {
        assert!(kani::mem::can_write(oldstate));
        // SAFETY: the harness's own statics, and `oldstate`, writable
        // (checked above); no other code runs in a harness.
        unsafe {
            let i = CANCEL_CALLS;
            CANCEL_CALLS += 1;
            let rc = if i < 2 {
                STATES[i] = (STEP, state);
                RC[i]
            } else {
                0
            };
            STEP += 1;
            if rc == 0 {
                *oldstate = OLD;
            }
            rc
        }
    }

    /// A stand-in for glibc's `fwrite` (premise C14): it records its
    /// call, its place and its arguments, checks that the `size * n`
    /// bytes at `ptr` are readable, without reading them or the stream,
    /// and returns `WRITTEN`.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    unsafe extern "C" fn fwrite_model(
        ptr: *const c_void,
        size: usize,
        n: usize,
        stream: *mut libc::FILE,
    ) -> usize {
        assert!(kani::mem::can_dereference(core::ptr::slice_from_raw_parts(
            ptr as *const u8,
            size.saturating_mul(n)
        )));
        // SAFETY: the harness's own statics; no other code runs in a
        // harness.
        unsafe {
            WRITES += 1;
            WROTE = (STEP, ptr as usize, size, n, stream as usize);
            STEP += 1;
            WRITTEN
        }
    }

    /// `write_file`: its `requires` (`may_syscall`, the report's token at
    /// top 0) are about ghost state and it has no `ensures`; it returns
    /// for any stream address (C13: the program's stream, which the
    /// stand-in does not read through) and any bytes, up to 1024 of them
    /// (`emit` passes at most 1023), and (beyond the contract) disables
    /// cancellation,
    /// calls `fwrite` once with the bytes' address, a `size` of 1, their
    /// count and the stream, then gives back the state the first call
    /// stored (or 0 when it failed), for any return values and old state.
    #[kani::proof]
    #[kani::stub(super::pthread_setcancelstate, setcancelstate_model)]
    #[kani::stub(super::fwrite, fwrite_model)]
    fn write_file_writes_the_bytes() {
        let buf: [u8; 1024] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 1024);
        let s = &buf[..len];
        let f: *mut libc::FILE = core::ptr::without_provenance_mut(kani::any());
        let rc: [c_int; 2] = kani::any();
        let old: c_int = kani::any();
        kani::cover!(len == 0);
        kani::cover!(len == 1023);
        kani::cover!(rc[0] == 0);
        kani::cover!(rc[0] != 0);
        // SAFETY: as in `setcancelstate_model`.
        unsafe {
            RC = rc;
            OLD = old;
            WRITTEN = kani::any();
        }
        write_file(f, s, Tracked::assume_new());
        // SAFETY: as in `setcancelstate_model`.
        let (cancel_calls, states, writes, wrote) =
            unsafe { (CANCEL_CALLS, STATES, WRITES, WROTE) };
        assert_eq!(cancel_calls, 2);
        assert_eq!(writes, 1);
        // glibc's `PTHREAD_CANCEL_DISABLE` (C9), not this file's constant.
        assert_eq!(states[0], (0, 1));
        assert_eq!(wrote, (1, s.as_ptr() as usize, 1, len, f as usize));
        assert_eq!(states[1], (2, if rc[0] == 0 { old } else { 0 }));
    }
}
