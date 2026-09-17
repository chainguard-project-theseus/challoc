//! `malloc_conf` parsing (design.md section 12).
//!
//! The parser is pure logic over byte slices so it can be unit tested on
//! the host; the source readers (weak `malloc_conf` symbol, `/etc/challoc.conf`,
//! environment) are Linux-only and never allocate: the file is read into a
//! stack buffer with raw syscalls and the environment is walked through
//! `environ` without `getenv`.
//!
//! Sources apply in increasing priority: compiled defaults, the weak
//! `malloc_conf` and `je_malloc_conf` globals (jemalloc style; Redis and
//! Valkey define the latter), the first line of `/etc/challoc.conf`, then the
//! `MALLOC_CONF` and `CHALLOC_CONF` environment variables. A later source
//! overrides an earlier one key by key.
//!
//! Format is jemalloc's `key:value,key:value`. A value may be wrapped in
//! double quotes. Unknown keys and malformed values are reported to the
//! caller's sink and otherwise ignored, as jemalloc does; when `abort_conf`
//! ends up set and anything was reported, [`load`] aborts. Out-of-range
//! numbers are clamped and reported, matching jemalloc's `CONF_HANDLE_*`
//! macros. `abort_conf` is evaluated after every source has been read, so
//! it applies to keys that appeared before it in the same or an earlier
//! source; jemalloc gets the same effect with a separate first pass.

use core::fmt::{self, Write as _};

use crate::heap::{MAX_CAP, MIN_CAP};

/// Largest accepted `tcache_nslots_small_max`. A cache array is allocated
/// from the metadata region as one object, and `Meta::alloc` serves layouts
/// up to 1 MiB; 65536 slots is 512 KiB plus the header, well inside that.
pub const MAX_NSLOTS: u32 = 65_536;
/// Range of `lg_tcache_nslots_mul`, as in jemalloc.
pub const LG_NSLOTS_MUL_MIN: i32 = -16;
pub const LG_NSLOTS_MUL_MAX: i32 = 16;
/// Largest `guard_slab_interval`: at most one guard per class region's
/// 2^22 slabs.
pub const MAX_GUARD_SLAB_INTERVAL: u32 = 1 << 22;
/// Largest decay / GC interval in milliseconds (about 31 years); jemalloc
/// caps decay at `NSTIME_SEC_MAX * 1000`, which is far larger, but a `u64`
/// of nanoseconds must not overflow when the interval is converted.
pub const MAX_INTERVAL_MS: i64 = 1_000_000_000_000;
/// Largest `tcache_compact_shift`: one scheduled pop per 2^20 pops is
/// already no compaction to speak of; 0 turns the schedule off.
pub const MAX_COMPACT_SHIFT: u32 = 20;
/// `thp` values (`default` is accepted as `never`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thp {
    Never,
    Always,
}

impl Thp {
    pub const fn name(self) -> &'static str {
        match self {
            Thp::Never => "never",
            Thp::Always => "always",
        }
    }
}

/// Every tunable of design section 12 with its compiled default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub background_thread: bool,
    /// `-1` means never purge.
    pub dirty_decay_ms: i64,
    /// Accepted for compatibility; nothing reads it.
    pub muzzy_decay_ms: i64,
    pub narenas: u32,
    pub tcache: bool,
    pub tcache_nslots_small_max: u32,
    pub lg_tcache_nslots_mul: i32,
    pub tcache_gc_interval_ms: i64,
    /// Every `2^shift` pops of a class are served from the lowest-address
    /// cached slot instead of the top of the stack (design 6.4); 0 means
    /// never.
    pub tcache_compact_shift: u32,
    pub thp: Thp,
    pub lazy_free: bool,
    pub guards: bool,
    /// One guard per N slabs of 64 KiB (per N * 64 KiB of slabs for the
    /// classes with smaller slabs, see `heap::guard_interval_for`); 0 means
    /// none, 1 means every slab.
    pub guard_slab_interval: u32,
    pub zero_on_free: bool,
    pub slot_randomize: bool,
    pub double_free_scan: bool,
    pub abort_conf: bool,
    pub cgroup: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config::DEFAULT
    }
}

impl Config {
    /// The compiled defaults (design section 12).
    pub const DEFAULT: Config = Config {
        background_thread: false,
        dirty_decay_ms: 10_000,
        muzzy_decay_ms: 0,
        narenas: 1,
        tcache: true,
        tcache_nslots_small_max: MAX_CAP,
        lg_tcache_nslots_mul: 1,
        tcache_gc_interval_ms: 10_000,
        tcache_compact_shift: 3,
        thp: Thp::Never,
        lazy_free: false,
        guards: true,
        guard_slab_interval: 8,
        zero_on_free: false,
        slot_randomize: false,
        double_free_scan: false,
        abort_conf: false,
        cgroup: true,
    };

    /// Apply one `key:value,key:value` string on top of `self`. Each problem
    /// is passed to `report`; the corresponding pair is skipped (or clamped,
    /// for [`Issue::OutOfRange`]) and parsing continues with the next pair.
    /// Returns the number of problems reported.
    pub fn parse(&mut self, text: &[u8], mut report: impl FnMut(Issue<'_>)) -> usize {
        let mut issues = 0;
        for pair in Pairs(text) {
            if let Err(issue) = self.apply_pair(pair) {
                issues += 1;
                report(issue);
            }
        }
        issues
    }

    /// Split one pair and set the key. Empty pairs (from `a,,b` or a
    /// trailing comma) are ignored, as jemalloc ignores them.
    fn apply_pair<'a>(&mut self, pair: &'a [u8]) -> Result<(), Issue<'a>> {
        if pair.is_empty() {
            return Ok(());
        }
        let Some(colon) = pair.iter().position(|&b| b == b':') else {
            return Err(Issue::Malformed(pair));
        };
        let (key, value) = (&pair[..colon], &pair[colon + 1..]);
        let value = unquote(value);
        if key.is_empty() || value.is_empty() {
            return Err(Issue::Malformed(pair));
        }
        self.set(key, value)
    }

    /// Set one key from its textual value. An out-of-range number is
    /// clamped *and* returned as [`Issue::OutOfRange`].
    pub fn set<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> Result<(), Issue<'a>> {
        let bad = || Issue::BadValue { key, value };
        // Numeric keys: parse, clamp, assign through `store`, then report a
        // clamp as an issue.
        let num = |cfg: &mut Config, min: i64, max: i64, store: fn(&mut Config, i64)| -> Result<(), Issue<'a>> {
            let (v, clamped) = clamped(key, value, min, max)?;
            store(cfg, v);
            if clamped { Err(Issue::OutOfRange { key, value }) } else { Ok(()) }
        };
        match key {
            b"background_thread" => self.background_thread = parse_bool(value).ok_or_else(bad)?,
            b"dirty_decay_ms" => return num(self, -1, MAX_INTERVAL_MS, |c, v| c.dirty_decay_ms = v),
            b"muzzy_decay_ms" => return num(self, -1, MAX_INTERVAL_MS, |c, v| c.muzzy_decay_ms = v),
            b"narenas" => return num(self, 1, u32::MAX as i64, |c, v| c.narenas = v as u32),
            b"tcache" => self.tcache = parse_bool(value).ok_or_else(bad)?,
            b"tcache_nslots_small_max" => {
                return num(self, MIN_CAP as i64, MAX_NSLOTS as i64, |c, v| c.tcache_nslots_small_max = v as u32);
            }
            b"lg_tcache_nslots_mul" => {
                return num(self, LG_NSLOTS_MUL_MIN as i64, LG_NSLOTS_MUL_MAX as i64, |c, v| {
                    c.lg_tcache_nslots_mul = v as i32
                });
            }
            b"tcache_gc_interval_ms" => {
                return num(self, 0, MAX_INTERVAL_MS, |c, v| c.tcache_gc_interval_ms = v);
            }
            b"tcache_compact_shift" => {
                return num(self, 0, MAX_COMPACT_SHIFT as i64, |c, v| c.tcache_compact_shift = v as u32);
            }
            b"thp" => {
                self.thp = match value {
                    b"never" | b"default" => Thp::Never,
                    b"always" => Thp::Always,
                    _ => return Err(bad()),
                }
            }
            b"lazy_free" => self.lazy_free = parse_bool(value).ok_or_else(bad)?,
            b"guards" => self.guards = parse_bool(value).ok_or_else(bad)?,
            b"guard_slab_interval" => {
                return num(self, 0, MAX_GUARD_SLAB_INTERVAL as i64, |c, v| c.guard_slab_interval = v as u32);
            }
            b"zero_on_free" => self.zero_on_free = parse_bool(value).ok_or_else(bad)?,
            b"slot_randomize" => self.slot_randomize = parse_bool(value).ok_or_else(bad)?,
            b"double_free_scan" => self.double_free_scan = parse_bool(value).ok_or_else(bad)?,
            b"abort_conf" => self.abort_conf = parse_bool(value).ok_or_else(bad)?,
            b"cgroup" => self.cgroup = parse_bool(value).ok_or_else(bad)?,
            // jemalloc keys accepted for compatibility and ignored: their
            // values are not validated because we do nothing with them.
            b"junk" | b"zero" | b"retain" | b"oversize_threshold" | b"percpu_arena"
            | b"metadata_thp" | b"stats_print" | b"confirm_conf" => {}
            _ => return Err(Issue::UnknownKey(key)),
        }
        Ok(())
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`. `None` for
    /// keys that are only accepted and ignored, and for unknown keys.
    pub fn get(&self, key: &[u8]) -> Option<Value> {
        Some(match key {
            b"background_thread" => Value::Bool(self.background_thread),
            b"dirty_decay_ms" => Value::Int(self.dirty_decay_ms),
            b"muzzy_decay_ms" => Value::Int(self.muzzy_decay_ms),
            b"narenas" => Value::Uint(self.narenas as u64),
            b"tcache" => Value::Bool(self.tcache),
            b"tcache_nslots_small_max" => Value::Uint(self.tcache_nslots_small_max as u64),
            b"lg_tcache_nslots_mul" => Value::Int(self.lg_tcache_nslots_mul as i64),
            b"tcache_gc_interval_ms" => Value::Int(self.tcache_gc_interval_ms),
            b"tcache_compact_shift" => Value::Uint(self.tcache_compact_shift as u64),
            b"thp" => Value::Str(self.thp.name()),
            b"lazy_free" => Value::Bool(self.lazy_free),
            b"guards" => Value::Bool(self.guards),
            b"guard_slab_interval" => Value::Uint(self.guard_slab_interval as u64),
            b"zero_on_free" => Value::Bool(self.zero_on_free),
            b"slot_randomize" => Value::Bool(self.slot_randomize),
            b"double_free_scan" => Value::Bool(self.double_free_scan),
            b"abort_conf" => Value::Bool(self.abort_conf),
            b"cgroup" => Value::Bool(self.cgroup),
            _ => return None,
        })
    }

    /// Cache capacity for a class under this configuration: the design's
    /// ladder (`tcache_nslots_small_max` slots up to 128 bytes, halving per
    /// doubling of the class size) shifted by `lg_tcache_nslots_mul - 1`
    /// doublings, clamped to `[MIN_CAP, tcache_nslots_small_max]`. Zero when
    /// `tcache` is off, which makes every thread serve straight from the
    /// slab bitmaps.
    pub fn cache_cap(&self, class: usize) -> u32 {
        if !self.tcache {
            return 0;
        }
        cap_for(class, self.tcache_nslots_small_max, self.lg_tcache_nslots_mul)
    }

    /// `tcache_gc_interval_ms` in nanoseconds.
    pub fn gc_interval_ns(&self) -> u64 {
        (self.tcache_gc_interval_ms as u64).saturating_mul(1_000_000)
    }

    /// Mask for the scheduled pop: a pop whose count has no bit of the mask
    /// set is served from the bottom of the sorted run. `u32::MAX` for
    /// `tcache_compact_shift:0` (one in 2^32 pops, i.e. off).
    pub fn compact_mask(&self) -> u32 {
        match self.tcache_compact_shift {
            0 => u32::MAX,
            s => (1u32 << s.min(MAX_COMPACT_SHIFT)) - 1,
        }
    }

    /// The guard slab interval actually used: 0 when guards are off.
    pub fn effective_guard_slab_interval(&self) -> u32 {
        if self.guards { self.guard_slab_interval } else { 0 }
    }
}

/// Every key readable through [`Config::get`], in table order, for the
/// `opt.` namespace and the tests.
pub const KEYS: &[&str] = &[
    "background_thread",
    "dirty_decay_ms",
    "muzzy_decay_ms",
    "narenas",
    "tcache",
    "tcache_nslots_small_max",
    "lg_tcache_nslots_mul",
    "tcache_gc_interval_ms",
    "tcache_compact_shift",
    "thp",
    "lazy_free",
    "guards",
    "guard_slab_interval",
    "zero_on_free",
    "slot_randomize",
    "double_free_scan",
    "abort_conf",
    "cgroup",
];

/// jemalloc keys that are accepted and ignored.
pub const IGNORED_KEYS: &[&str] = &[
    "junk",
    "zero",
    "retain",
    "oversize_threshold",
    "percpu_arena",
    "metadata_thp",
    "stats_print",
    "confirm_conf",
];

/// Cache capacity ladder: `max` slots for classes up to 128 bytes, halved
/// per doubling of the class size, shifted by `lg_mul - 1` doublings and
/// clamped to `[MIN_CAP, max]`. With `max = MAX_CAP` and `lg_mul = 1` this
/// is the design 6.2 table (`heap::default_cap`).
pub const fn cap_for(class: usize, max: u32, lg_mul: i32) -> u32 {
    let size = crate::layout::CLASSES[class].size();
    // Doublings above 128 bytes: sizes in (128, 256] halve once, (256, 512]
    // twice, and so on.
    let doublings = if size <= 128 { 0 } else { (size - 1).ilog2() as i32 + 1 - 7 };
    let shift = doublings - (lg_mul - 1);
    let cap = if shift >= 32 {
        0
    } else if shift <= -32 {
        u32::MAX
    } else if shift >= 0 {
        max >> shift
    } else {
        let grown = (max as u64).saturating_mul(1u64 << (-shift));
        if grown > u32::MAX as u64 { u32::MAX } else { grown as u32 }
    };
    let cap = if cap > max { max } else { cap };
    if cap < MIN_CAP { MIN_CAP } else { cap }
}

/// A readable configuration value (`mallctl` `opt.*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Str(&'static str),
}

/// One problem found while parsing; the offending bytes borrow the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue<'a> {
    /// No `key:value` shape (missing colon, empty key or empty value).
    Malformed(&'a [u8]),
    /// A key from no table.
    UnknownKey(&'a [u8]),
    /// A value that does not parse for its key's type.
    BadValue { key: &'a [u8], value: &'a [u8] },
    /// A number outside the key's range; the value was clamped.
    OutOfRange { key: &'a [u8], value: &'a [u8] },
}

impl fmt::Display for Issue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Malformed(pair) => write!(f, "malformed conf pair \"{}\"", Bytes(pair)),
            Issue::UnknownKey(key) => write!(f, "unknown conf key \"{}\"", Bytes(key)),
            Issue::BadValue { key, value } => {
                write!(f, "invalid conf value \"{}\" for {}", Bytes(value), Bytes(key))
            }
            Issue::OutOfRange { key, value } => {
                write!(f, "out-of-range conf value \"{}\" for {} (clamped)", Bytes(value), Bytes(key))
            }
        }
    }
}

/// Bytes shown as text, one `?` per non-printable byte, at most 64 bytes.
struct Bytes<'a>(&'a [u8]);

impl fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0.iter().take(64) {
            f.write_char(if (0x20..0x7f).contains(&b) { b as char } else { '?' })?;
        }
        if self.0.len() > 64 {
            f.write_str("...")?;
        }
        Ok(())
    }
}

/// Iterator over the comma-separated pairs of a conf string.
struct Pairs<'a>(&'a [u8]);

impl<'a> Iterator for Pairs<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.0.is_empty() {
            return None;
        }
        // A quoted value may contain commas: skip to the closing quote when
        // the value starts with one.
        let mut i = 0;
        let mut quoted = false;
        while i < self.0.len() {
            let b = self.0[i];
            if quoted {
                if b == b'"' {
                    quoted = false;
                }
            } else if b == b',' {
                break;
            } else if b == b'"' && i > 0 && self.0[i - 1] == b':' {
                quoted = true;
            }
            i += 1;
        }
        let pair = &self.0[..i];
        self.0 = if i < self.0.len() { &self.0[i + 1..] } else { &[] };
        Some(pair)
    }
}

/// Strip one pair of surrounding double quotes.
fn unquote(value: &[u8]) -> &[u8] {
    if value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"' {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// `true` / `false` only, as in jemalloc.
pub fn parse_bool(value: &[u8]) -> Option<bool> {
    match value {
        b"true" => Some(true),
        b"false" => Some(false),
        _ => None,
    }
}

/// A decimal, `0x` hexadecimal or leading-zero octal integer with an
/// optional sign (jemalloc's `malloc_strtoumax` with base 0). `None` on
/// anything else, including overflow.
pub fn parse_int(value: &[u8]) -> Option<i64> {
    let (negative, digits) = match value {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, value),
    };
    let (radix, digits) = match digits {
        [b'0', b'x' | b'X', rest @ ..] if !rest.is_empty() => (16u64, rest),
        [b'0', rest @ ..] if !rest.is_empty() => (8, rest),
        _ => (10, digits),
    };
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: u64 = 0;
    for &b in digits {
        let d = (b as char).to_digit(radix as u32)? as u64;
        magnitude = magnitude.checked_mul(radix)?.checked_add(d)?;
    }
    if negative {
        if magnitude > i64::MAX as u64 + 1 {
            return None;
        }
        Some((magnitude as i64).wrapping_neg())
    } else {
        i64::try_from(magnitude).ok()
    }
}

/// Parse an integer and clamp it into `[min, max]`. `Ok((v, false))` for a
/// value in range, `Ok((clamped, true))` for one outside it (the caller
/// assigns the clamped value and reports [`Issue::OutOfRange`], as
/// jemalloc's `CONF_HANDLE_*` macros do), `Err` when it does not parse.
fn clamped<'a>(key: &'a [u8], value: &'a [u8], min: i64, max: i64) -> Result<(i64, bool), Issue<'a>> {
    let v = parse_int(value).ok_or(Issue::BadValue { key, value })?;
    if v < min {
        Ok((min, true))
    } else if v > max {
        Ok((max, true))
    } else {
        Ok((v, false))
    }
}

#[cfg(target_os = "linux")]
pub use linux::{env_var, first_line_of, load, malloc_conf_symbol, MAX_TEXT};

#[cfg(target_os = "linux")]
mod linux {
    use core::ffi::{c_char, c_void};
    use core::fmt::Write as _;

    use super::{Config, Issue};
    use crate::sys;

    /// Longest conf text read from any one source. Longer inputs are cut
    /// here; no real configuration comes close.
    pub const MAX_TEXT: usize = 4096;

    /// `AT_SECURE`: set for setuid/setgid/capability-elevated processes,
    /// whose environment is untrusted (glibc's `secure_getenv` rule).
    const AT_SECURE: usize = 23;

    unsafe extern "C" {
        /// glibc's environment pointer. Read directly so no `getenv` runs
        /// inside `malloc`.
        static environ: *const *const c_char;
        /// glibc/musl `getauxval(3)` (see `sys::features`).
        safe fn getauxval(kind: usize) -> usize;
    }

    /// Read every source in priority order into a `Config`, printing each
    /// problem as `challoc: <source>: <issue>` on stderr. Aborts when
    /// `abort_conf` is set by the time all sources are read and anything was
    /// reported. Never allocates: the file goes through a stack buffer and
    /// the environment is walked in place.
    pub fn load() -> Config {
        let mut cfg = Config::DEFAULT;
        let mut issues = 0;
        let mut out = sys::Stderr::new();
        let mut report = |source: &str, issue: Issue<'_>| {
            let _ = writeln!(out, "challoc: {source}: {issue}");
        };

        for (name, text) in [("malloc_conf", malloc_conf_symbol(0)), ("je_malloc_conf", malloc_conf_symbol(1))] {
            if let Some(text) = text {
                issues += cfg.parse(text, |i| report(name, i));
            }
        }
        let mut buf = [0u8; MAX_TEXT];
        if let Some(line) = first_line_of(b"/etc/challoc.conf\0", &mut buf) {
            issues += cfg.parse(line, |i| report("/etc/challoc.conf", i));
        }
        if getauxval(AT_SECURE) == 0 {
            for name in ["MALLOC_CONF", "CHALLOC_CONF"] {
                if let Some(text) = env_var(name.as_bytes()) {
                    issues += cfg.parse(text, |i| report(name, i));
                }
            }
        }
        out.flush();
        if cfg.abort_conf && issues > 0 {
            sys::abort_fmt(format_args!("abort_conf: {issues} configuration problem(s), see above"));
        }
        cfg
    }

    /// The string the `malloc_conf` (`which == 0`, defined by the library,
    /// interposed by a program's definition) or weak `je_malloc_conf`
    /// (`which == 1`) global points at, `None` when the process defines
    /// neither the symbol nor a string.
    pub fn malloc_conf_symbol(which: u32) -> Option<&'static [u8]> {
        let slot: *const *const c_char = match which {
            0 => malloc_conf_addr(),
            _ => je_malloc_conf_addr(),
        }
        .cast();
        if slot.is_null() {
            return None;
        }
        // SAFETY: the symbol resolved to a `const char *` object defined by
        // the process (jemalloc's documented `malloc_conf` type).
        cstr(unsafe { slot.read() })
    }

    /// Address of the `malloc_conf` global the loader resolved: the
    /// program's own definition when it has one, else the library's null
    /// slot (`api::jemalloc::malloc_conf`). The load goes through the GOT
    /// so the program's definition interposes; a direct reference could be
    /// bound to the library's own copy at link time.
    #[inline(never)]
    fn malloc_conf_addr() -> *const c_void {
        let out: *const c_void;
        // SAFETY: a GOT load of a defined data symbol; no memory is written.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            core::arch::asm!(
                "mov {out}, qword ptr [rip + malloc_conf@GOTPCREL]",
                out = out(reg) out,
                options(nostack, readonly, preserves_flags),
            );
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!(
                "adrp {out}, :got:malloc_conf",
                "ldr {out}, [{out}, :got_lo12:malloc_conf]",
                out = out(reg) out,
                options(nostack, readonly, preserves_flags),
            );
        }
        out
    }

    /// Address of the weak `je_malloc_conf` global (Redis and Valkey define
    /// this one), or null.
    #[inline(never)]
    fn je_malloc_conf_addr() -> *const c_void {
        let out: *const c_void;
        // SAFETY: as in `malloc_conf_addr`.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            core::arch::asm!(
                ".weak je_malloc_conf",
                "mov {out}, qword ptr [rip + je_malloc_conf@GOTPCREL]",
                out = out(reg) out,
                options(nostack, readonly, preserves_flags),
            );
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!(
                ".weak je_malloc_conf",
                "adrp {out}, :got:je_malloc_conf",
                "ldr {out}, [{out}, :got_lo12:je_malloc_conf]",
                out = out(reg) out,
                options(nostack, readonly, preserves_flags),
            );
        }
        out
    }

    /// A NUL-terminated string as a slice of at most [`MAX_TEXT`] bytes;
    /// `None` for null or empty.
    fn cstr(p: *const c_char) -> Option<&'static [u8]> {
        if p.is_null() {
            return None;
        }
        let p = p.cast::<u8>();
        let mut n = 0;
        // SAFETY: the caller hands us a NUL-terminated string; reads stop at
        // the terminator or at the cap.
        while n < MAX_TEXT && unsafe { p.add(n).read() } != 0 {
            n += 1;
        }
        if n == 0 {
            return None;
        }
        // SAFETY: `n` bytes before the terminator were just read.
        Some(unsafe { core::slice::from_raw_parts(p, n) })
    }

    /// The value of environment variable `name` (no `=` in it), read from
    /// `environ` without `getenv`. `None` when unset or empty.
    pub fn env_var(name: &[u8]) -> Option<&'static [u8]> {
        // SAFETY: `environ` is glibc's NULL-terminated array of NUL-terminated
        // strings; it is null before libc has set it up.
        let mut e = unsafe { environ };
        if e.is_null() {
            return None;
        }
        loop {
            // SAFETY: `e` walks the array up to its terminating null.
            let entry = unsafe { e.read() };
            if entry.is_null() {
                return None;
            }
            if let Some(text) = cstr(entry) {
                if text.len() > name.len() && &text[..name.len()] == name && text[name.len()] == b'=' {
                    let value = &text[name.len() + 1..];
                    return if value.is_empty() { None } else { Some(value) };
                }
            }
            // SAFETY: not yet at the terminating null.
            e = unsafe { e.add(1) };
        }
    }

    /// The first line of the file at NUL-terminated `path`, without its
    /// newline, read with raw syscalls into `buf`. `None` if the file is
    /// missing, unreadable or its first line is empty.
    pub fn first_line_of<'b>(path: &[u8], buf: &'b mut [u8]) -> Option<&'b [u8]> {
        let fd = sys::open(path, sys::O_RDONLY).ok()?;
        let n = sys::read_full(fd, buf);
        let _ = sys::close(fd);
        let n = n.ok()?;
        let text = &buf[..n];
        let end = text.iter().position(|&b| b == b'\n').unwrap_or(text.len());
        let line = &text[..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() { None } else { Some(line) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CLASSES, NCLASSES};
    use std::vec::Vec;

    fn parse(text: &str) -> (Config, Vec<std::string::String>) {
        let mut cfg = Config::DEFAULT;
        let mut issues = Vec::new();
        let n = cfg.parse(text.as_bytes(), |i| issues.push(std::format!("{i}")));
        assert_eq!(n, issues.len());
        (cfg, issues)
    }

    fn clean(text: &str) -> Config {
        let (cfg, issues) = parse(text);
        assert!(issues.is_empty(), "{text}: {issues:?}");
        cfg
    }

    #[test]
    fn defaults_match_the_design_table() {
        let d = Config::DEFAULT;
        assert!(!d.background_thread);
        assert_eq!(d.dirty_decay_ms, 10_000);
        assert_eq!(d.narenas, 1);
        assert!(d.tcache);
        assert_eq!(d.tcache_nslots_small_max, 1024);
        assert_eq!(d.lg_tcache_nslots_mul, 1);
        assert_eq!(d.tcache_gc_interval_ms, 10_000);
        assert_eq!(d.tcache_compact_shift, 3);
        assert_eq!(d.compact_mask(), 7);
        assert_eq!(clean("tcache_compact_shift:0").compact_mask(), u32::MAX);
        assert_eq!(clean("tcache_compact_shift:20").compact_mask(), (1 << 20) - 1);
        assert_eq!(d.thp, Thp::Never);
        assert!(!d.lazy_free);
        assert!(d.guards);
        assert_eq!(d.guard_slab_interval, 8);
        assert!(!d.zero_on_free && !d.slot_randomize && !d.double_free_scan && !d.abort_conf);
        assert!(d.cgroup);
        assert_eq!(Config::default(), d);
        assert_eq!(d.gc_interval_ns(), 10_000_000_000);
        assert_eq!(d.effective_guard_slab_interval(), 8);
    }

    #[test]
    fn empty_and_separator_only_input_changes_nothing() {
        for text in ["", ",", ",,,"] {
            assert_eq!(clean(text), Config::DEFAULT, "{text:?}");
        }
    }

    #[test]
    fn every_bool_key_parses_true_and_false() {
        let keys = [
            "background_thread",
            "tcache",
            "lazy_free",
            "guards",
            "zero_on_free",
            "slot_randomize",
            "double_free_scan",
            "abort_conf",
            "cgroup",
        ];
        for key in keys {
            for v in [true, false] {
                let cfg = clean(&std::format!("{key}:{v}"));
                assert_eq!(cfg.get(key.as_bytes()), Some(Value::Bool(v)), "{key}:{v}");
            }
            for bad in ["yes", "1", "TRUE", "", "truex"] {
                let (cfg, issues) = parse(&std::format!("{key}:{bad}"));
                assert_eq!(cfg, Config::DEFAULT, "{key}:{bad} changed the config");
                assert_eq!(issues.len(), 1, "{key}:{bad}");
                let expect = if bad.is_empty() { "malformed" } else { "invalid conf value" };
                assert!(issues[0].contains(expect), "{key}:{bad}: {}", issues[0]);
            }
        }
    }

    #[test]
    fn every_numeric_key_parses_and_clamps() {
        // (key, in-range value, below-min input, clamped min, above-max input, clamped max)
        let cases: [(&str, i64, &str, i64, &str, i64); 8] = [
            ("dirty_decay_ms", 5000, "-2", -1, "9999999999999999", MAX_INTERVAL_MS),
            ("muzzy_decay_ms", 0, "-5", -1, "9999999999999999", MAX_INTERVAL_MS),
            ("narenas", 4, "0", 1, "99999999999", u32::MAX as i64),
            ("tcache_nslots_small_max", 1000, "1", MIN_CAP as i64, "1000000", MAX_NSLOTS as i64),
            ("lg_tcache_nslots_mul", 3, "-17", -16, "17", 16),
            ("tcache_gc_interval_ms", 250, "-1", 0, "9999999999999999", MAX_INTERVAL_MS),
            ("tcache_compact_shift", 4, "-1", 0, "64", MAX_COMPACT_SHIFT as i64),
            ("guard_slab_interval", 1, "-1", 0, "5000000", MAX_GUARD_SLAB_INTERVAL as i64),
        ];
        for (key, ok, low, min, high, max) in cases {
            let as_value = |v: i64| match key {
                "narenas" | "tcache_nslots_small_max" | "guard_slab_interval" | "tcache_compact_shift" => {
                    Value::Uint(v as u64)
                }
                _ => Value::Int(v),
            };
            let cfg = clean(&std::format!("{key}:{ok}"));
            assert_eq!(cfg.get(key.as_bytes()), Some(as_value(ok)), "{key}:{ok}");

            let (cfg, issues) = parse(&std::format!("{key}:{low}"));
            assert_eq!(cfg.get(key.as_bytes()), Some(as_value(min)), "{key}:{low} clamps to min");
            assert_eq!(issues.len(), 1);
            assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

            let (cfg, issues) = parse(&std::format!("{key}:{high}"));
            assert_eq!(cfg.get(key.as_bytes()), Some(as_value(max)), "{key}:{high} clamps to max");
            assert_eq!(issues.len(), 1);
            assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

            for bad in ["x", "1x", "0x", "--1", "1.5", " 1", "true"] {
                let (cfg, issues) = parse(&std::format!("{key}:{bad}"));
                assert_eq!(cfg, Config::DEFAULT, "{key}:{bad} changed the config");
                assert_eq!(issues.len(), 1, "{key}:{bad}");
                assert!(issues[0].contains("invalid conf value"), "{key}:{bad}: {}", issues[0]);
            }
        }
    }

    #[test]
    fn integers_accept_hex_octal_and_signs() {
        assert_eq!(parse_int(b"0"), Some(0));
        assert_eq!(parse_int(b"42"), Some(42));
        assert_eq!(parse_int(b"+42"), Some(42));
        assert_eq!(parse_int(b"-1"), Some(-1));
        assert_eq!(parse_int(b"0x20"), Some(32));
        assert_eq!(parse_int(b"0X1f"), Some(31));
        assert_eq!(parse_int(b"010"), Some(8));
        assert_eq!(parse_int(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_int(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int(b"9223372036854775808"), None);
        assert_eq!(parse_int(b"-9223372036854775809"), None);
        assert_eq!(parse_int(b"99999999999999999999"), None);
        assert_eq!(parse_int(b""), None);
        assert_eq!(parse_int(b"-"), None);
        assert_eq!(parse_int(b"0x"), None);
        assert_eq!(parse_int(b"08"), None);
        assert_eq!(parse_int(b"1_000"), None);
        assert_eq!(clean("tcache_nslots_small_max:0x100").tcache_nslots_small_max, 256);
    }

    #[test]
    fn thp_values() {
        assert_eq!(clean("thp:always").thp, Thp::Always);
        assert_eq!(clean("thp:never").thp, Thp::Never);
        assert_eq!(clean("thp:default").thp, Thp::Never);
        assert_eq!(clean("thp:always").get(b"thp"), Some(Value::Str("always")));
        let (cfg, issues) = parse("thp:madvise");
        assert_eq!(cfg.thp, Thp::Never);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid conf value \"madvise\" for thp"));
    }

    #[test]
    fn compatibility_keys_are_accepted_with_any_value() {
        for key in IGNORED_KEYS {
            for value in ["true", "false", "17", "alloc", "whatever"] {
                let cfg = clean(&std::format!("{key}:{value}"));
                assert_eq!(cfg, Config::DEFAULT, "{key}:{value} changed the config");
                assert_eq!(cfg.get(key.as_bytes()), None);
            }
        }
    }

    #[test]
    fn unknown_keys_are_reported_and_skipped() {
        let (cfg, issues) = parse("bogus:1,tcache:false,another_bogus:true");
        assert!(!cfg.tcache);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("unknown conf key \"bogus\""), "{}", issues[0]);
        assert!(issues[1].contains("unknown conf key \"another_bogus\""), "{}", issues[1]);
        // Keys are case sensitive and not trimmed, as in jemalloc.
        for text in ["Tcache:false", " tcache:false", "tcache :false", "tcache: false"] {
            let (cfg, issues) = parse(text);
            assert_eq!(cfg, Config::DEFAULT, "{text:?}");
            assert_eq!(issues.len(), 1, "{text:?}");
        }
    }

    #[test]
    fn malformed_pairs_are_reported_and_the_rest_still_applies() {
        let (cfg, issues) = parse("nocolon,tcache:false,:novalue,nokey:,narenas:3");
        assert!(!cfg.tcache);
        assert_eq!(cfg.narenas, 3);
        assert_eq!(issues.len(), 3);
        assert!(issues[0].contains("malformed conf pair \"nocolon\""), "{}", issues[0]);
        assert!(issues[1].contains("malformed conf pair \":novalue\""), "{}", issues[1]);
        assert!(issues[2].contains("malformed conf pair \"nokey:\""), "{}", issues[2]);
        // A lone colon and a value with a second colon.
        let (_, issues) = parse(":");
        assert_eq!(issues.len(), 1);
        let (cfg, issues) = parse("narenas:1:2");
        assert_eq!(cfg.narenas, 1);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid conf value \"1:2\""), "{}", issues[0]);
    }

    #[test]
    fn quoted_values_and_the_last_pair_win() {
        assert_eq!(clean("narenas:\"7\"").narenas, 7);
        assert_eq!(clean("thp:\"always\"").thp, Thp::Always);
        // A quoted value may contain a comma; an unbalanced quote is a value.
        let (cfg, issues) = parse("thp:\"al,ways\",narenas:2");
        assert_eq!(cfg.narenas, 2);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("\"al,ways\""), "{}", issues[0]);
        let (_, issues) = parse("narenas:\"3");
        assert_eq!(issues.len(), 1);
        // Later pairs override earlier ones; a trailing comma is fine.
        let cfg = clean("narenas:2,narenas:5,tcache:false,tcache:true,");
        assert_eq!(cfg.narenas, 5);
        assert!(cfg.tcache);
    }

    #[test]
    fn redis_and_valkey_conf_strings_parse_clean() {
        let cfg = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
        assert_eq!(cfg.lg_tcache_nslots_mul, 3);
        assert_eq!(cfg.tcache_nslots_small_max, 1000);
        let cfg = clean("background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0,narenas:2,abort_conf:true");
        assert!(cfg.background_thread && cfg.abort_conf);
        assert_eq!(cfg.dirty_decay_ms, 0);
        assert_eq!(cfg.narenas, 2);
    }

    #[test]
    fn get_covers_every_key_and_only_those() {
        let cfg = Config::DEFAULT;
        for key in KEYS {
            assert!(cfg.get(key.as_bytes()).is_some(), "{key} unreadable");
            // Every readable key also parses (with a value of its type).
            let value = match cfg.get(key.as_bytes()).unwrap() {
                Value::Bool(b) => std::format!("{b}"),
                Value::Int(i) => std::format!("{i}"),
                Value::Uint(u) => std::format!("{u}"),
                Value::Str(s) => s.into(),
            };
            assert_eq!(clean(&std::format!("{key}:{value}")), cfg, "{key}:{value} round trip");
        }
        assert_eq!(KEYS.len(), 18);
        assert_eq!(cfg.get(b"nonsense"), None);
        assert_eq!(cfg.get(b""), None);
        for key in IGNORED_KEYS {
            assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
        }
    }

    #[test]
    fn cache_caps_follow_the_ladder_and_the_overrides() {
        let d = Config::DEFAULT;
        for c in 0..NCLASSES {
            assert_eq!(d.cache_cap(c), cap_for(c, MAX_CAP, 1));
            assert_eq!(d.cache_cap(c), crate::heap::default_cap(c), "class {c}");
        }
        // Redis: x4 ladder clamped at 1000, never below MIN_CAP.
        let redis = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
        for c in 0..NCLASSES {
            let size = CLASSES[c].size();
            let cap = redis.cache_cap(c);
            assert!(cap >= MIN_CAP && cap <= 1000, "class {c} ({size} bytes): {cap}");
            if size <= 512 {
                assert_eq!(cap, 1000, "class {c} ({size} bytes)");
            }
            assert!(cap >= d.cache_cap(c).min(1000), "class {c} never shrinks below the default ladder");
        }
        // A small max caps every class; lg_mul below 1 shrinks the ladder.
        let small = clean("tcache_nslots_small_max:16");
        assert!((0..NCLASSES).all(|c| small.cache_cap(c) <= 16 && small.cache_cap(c) >= MIN_CAP));
        let shrunk = clean("lg_tcache_nslots_mul:0");
        assert_eq!(shrunk.cache_cap(0), 512);
        assert_eq!(shrunk.cache_cap(NCLASSES - 1), MIN_CAP);
        let grown = clean("lg_tcache_nslots_mul:16,tcache_nslots_small_max:65536");
        assert!((0..NCLASSES).all(|c| grown.cache_cap(c) == 65536));
        // tcache:false turns caching off entirely.
        let off = clean("tcache:false");
        assert!((0..NCLASSES).all(|c| off.cache_cap(c) == 0));
        // Extreme shifts do not overflow.
        assert_eq!(cap_for(0, MAX_NSLOTS, -16), MIN_CAP);
        assert_eq!(cap_for(NCLASSES - 1, MAX_NSLOTS, 16), MAX_NSLOTS);
    }

    #[test]
    fn guards_off_zeroes_the_interval() {
        assert_eq!(clean("guards:false").effective_guard_slab_interval(), 0);
        assert_eq!(clean("guard_slab_interval:0").effective_guard_slab_interval(), 0);
        assert_eq!(clean("guard_slab_interval:1").effective_guard_slab_interval(), 1);
        assert_eq!(clean("tcache_gc_interval_ms:0").gc_interval_ns(), 0);
        assert_eq!(clean("tcache_gc_interval_ms:1").gc_interval_ns(), 1_000_000);
    }

    #[test]
    fn issue_display_is_printable_and_bounded() {
        let long: Vec<u8> = (0..200u8).collect();
        let text = std::format!("{}", Issue::UnknownKey(&long));
        assert!(text.contains("..."));
        assert!(text.len() < 120, "{text}");
        assert!(!text.bytes().any(|b| b < 0x20 || b >= 0x7f), "{text:?}");
        assert_eq!(std::format!("{}", Issue::Malformed(b"a\tb")), "malformed conf pair \"a?b\"");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    fn no_malloc_conf_symbol_in_the_test_binary() {
        // The test binary defines neither `malloc_conf` nor `je_malloc_conf`,
        // so both weak references resolve to null. `tests/c/conf.c` covers
        // the defined case under the real `.so`.
        assert_eq!(malloc_conf_symbol(0), None);
        assert_eq!(malloc_conf_symbol(1), None);
    }

    #[test]
    fn env_var_is_read_from_environ() {
        // This is the only test that mutates the environment.
        unsafe {
            std::env::set_var("CHALLOC_CONF_TEST", "narenas:3,thp:always");
            std::env::set_var("CHALLOC_CONF_TEST_EMPTY", "");
        }
        assert_eq!(env_var(b"CHALLOC_CONF_TEST"), Some(&b"narenas:3,thp:always"[..]));
        assert_eq!(env_var(b"CHALLOC_CONF_TEST_EMPTY"), None);
        assert_eq!(env_var(b"CHALLOC_CONF_TES"), None, "prefix must not match");
        assert_eq!(env_var(b"CHALLOC_CONF_TEST_MISSING"), None);
        let mut cfg = Config::DEFAULT;
        assert_eq!(cfg.parse(env_var(b"CHALLOC_CONF_TEST").unwrap(), |_| {}), 0);
        assert_eq!(cfg.narenas, 3);
        assert_eq!(cfg.thp, Thp::Always);
    }

    #[test]
    fn first_line_of_a_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(std::format!("challoc-conf-{}", std::process::id()));
        let mut buf = [0u8; MAX_TEXT];
        let mut cpath = path.to_str().unwrap().as_bytes().to_vec();
        cpath.push(0);

        std::fs::write(&path, "tcache:false,narenas:2\r\nsecond line is ignored\n").unwrap();
        assert_eq!(first_line_of(&cpath, &mut buf), Some(&b"tcache:false,narenas:2"[..]));
        std::fs::write(&path, "no newline at all").unwrap();
        assert_eq!(first_line_of(&cpath, &mut buf), Some(&b"no newline at all"[..]));
        std::fs::write(&path, "\nsecond").unwrap();
        assert_eq!(first_line_of(&cpath, &mut buf), None, "empty first line");
        std::fs::write(&path, "").unwrap();
        assert_eq!(first_line_of(&cpath, &mut buf), None);
        // Longer than the buffer: cut at MAX_TEXT, still parseable up to the cut.
        let long = std::format!("{}narenas:9", "tcache:true,".repeat(MAX_TEXT / 6));
        std::fs::write(&path, &long).unwrap();
        let line = first_line_of(&cpath, &mut buf).unwrap();
        assert_eq!(line.len(), MAX_TEXT);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(first_line_of(&cpath, &mut buf), None, "missing file");
        assert_eq!(first_line_of(b"/etc\0", &mut buf), None, "a directory");
    }

    #[test]
    fn load_with_a_clean_environment_gives_the_defaults() {
        // No `MALLOC_CONF`/`CHALLOC_CONF` in the test environment, no
        // `/etc/challoc.conf` in the container, no weak symbol: defaults.
        assert_eq!(env_var(b"MALLOC_CONF"), None, "MALLOC_CONF is set; the test assumes a clean environment");
        assert_eq!(env_var(b"CHALLOC_CONF"), None, "CHALLOC_CONF is set; the test assumes a clean environment");
        let mut buf = [0u8; MAX_TEXT];
        if first_line_of(b"/etc/challoc.conf\0", &mut buf).is_none() {
            assert_eq!(load(), Config::DEFAULT);
        }
    }
}
