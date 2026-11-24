//! `malloc_conf` parsing (design 12).
//!
//! The parser is pure logic over byte slices so it can be unit tested
//! against fixtures; the source readers (the `malloc_conf` symbols,
//! `/etc/challoc.conf`, environment) never allocate: the file is read into
//! a stack buffer with raw syscalls and the environment is walked through
//! `environ` without `getenv`.
//!
//! Sources apply in increasing priority: compiled defaults, the
//! `malloc_conf` global and the weak `je_malloc_conf` reference (Redis and
//! Valkey define the latter), the first line of `/etc/challoc.conf`, then the
//! `MALLOC_CONF` and `CHALLOC_CONF` environment variables. A later source
//! overrides an earlier one key by key. A secure-execution process
//! (`AT_SECURE` set: setuid, setgid, file capabilities or an LSM
//! transition) reads neither variable (`linux::theorem_secure_conf`).
//!
//! Format is jemalloc's `key:value,key:value`. A value may be wrapped in
//! double quotes. Unknown keys and malformed values are reported to the
//! caller's sink and otherwise ignored, as jemalloc does; when `abort_conf`
//! ends up set and anything was reported, [`load`] aborts. Out-of-range
//! numbers are clamped and reported, matching jemalloc's `CONF_HANDLE_*`
//! macros. `abort_conf` is evaluated after every source has been read, so
//! it applies to keys that appeared before it in the same or an earlier
//! source; jemalloc gets the same effect with a separate first pass.
//!
//! Two tables name the keys. [`KEYS`] names the tunables the allocator
//! reads, jemalloc's and challoc's own (`lazy_band`, `guards`, ...), in
//! table order; [`Config::set`] and [`Config::get`] (`opt.<key>`) find a
//! key's index there and run that index's parser or reader; the modules
//! named in each field's doc act on the values.
//! [`IGNORED_KEYS`] are jemalloc keys accepted with any value and ignored,
//! so a conf string written against jemalloc parses clean.

use core::ffi::CStr;
use core::fmt::{self, Write as _};

use crate::heap::{MAX_CAP, MIN_CAP};
#[cfg(verus_keep_ghost)]
use vstd::arithmetic::power2::pow2;
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::string::*;

verus! {

/// Largest accepted `tcache_nslots_small_max`. A cache array is allocated
/// from the metadata region as one object, and `Meta::alloc_raw` serves layouts
/// up to 1 MiB; 65536 slots is 512 KiB plus the header, well inside that.
pub const MAX_NSLOTS: u32 = 65_536;
/// Default `tcache_max`: jemalloc's. Slots of classes above it are never
/// cached; at 16 arenas, 8 cached 40 KiB slots per (thread, arena) pair
/// held 70 MiB of xmalloc-test's RSS and most of mstress's excess.
pub const DEFAULT_TCACHE_MAX: usize = 32 * 1024;
/// Range of `lg_tcache_nslots_mul`, as in jemalloc.
pub const LG_NSLOTS_MUL_MIN: i32 = -16;
pub const LG_NSLOTS_MUL_MAX: i32 = 16;
/// Largest `guard_slab_interval`: at most one guard per class region's
/// 2^22 slabs.
pub const MAX_GUARD_SLAB_INTERVAL: u32 = 1 << 22;
/// Largest decay interval in milliseconds (about 31 years); jemalloc
/// caps decay at `NSTIME_SEC_MAX * 1000`, which is far larger, but a `u64`
/// of nanoseconds must not overflow when the interval is converted.
pub const MAX_INTERVAL_MS: i64 = 1_000_000_000_000;
/// Largest `huge_retain_max`: the preferred huge region (2^40 bytes,
/// `layout::LG_HUGE_PREFERRED`); no run can be larger, so anything above
/// means "retain every freed run for decay".
pub const MAX_HUGE_RETAIN_MAX: usize = 1 << crate::layout::LG_HUGE_PREFERRED;

/// `thp` values (`default` is accepted as `never`; design 7, 10 and 17,
/// `thp.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thp {
    /// Nothing is advised: byte for byte the allocator without the key.
    Never,
    /// Multi-size THP only: `MADV_HUGEPAGE` on class regions whose slab
    /// size can hold an enabled folio size below the PMD (64 KiB on the
    /// aarch64 kernels that enable it) and on metadata commits, never a
    /// 2 MiB page, so the fork copy-on-write cost that makes Redis and
    /// Valkey forbid huge pages stays absent. Inert on a kernel whose PMD
    /// size is `madvise`-gated (the advice would enable 2 MiB pages) or
    /// that enables no size below the PMD.
    Auto,
    /// Every enabled size, the PMD included: class regions are committed
    /// and advised in PMD-sized chunks, huge runs of a PMD or more and
    /// metadata commits are advised.
    Always,
}

/// `percpu_arena` values: jemalloc's key, accepted with jemalloc's three
/// values so a conf string written against jemalloc parses clean under
/// `abort_conf`, and read back through `opt.percpu_arena`. Nothing else
/// reads it: caches are per thread whatever the value, and the report notes
/// that when `percpu` or `phycpu` was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercpuArena {
    /// jemalloc's default.
    Disabled,
    Percpu,
    Phycpu,
}

/// The names [`Thp::name`] gives, in variant order. Outside the
/// verifier's view (Verus takes no `CStr` literal); a const assert after
/// this block checks them.
#[allow(clippy::redundant_static_lifetimes)]
#[verifier::external_body]
const THP_NAMES: [&'static CStr; 3] = [c"never", c"auto", c"always"];

/// The names [`PercpuArena::name`] gives, in variant order, as
/// [`THP_NAMES`].
#[allow(clippy::redundant_static_lifetimes)]
#[verifier::external_body]
const PERCPU_ARENA_NAMES: [&'static CStr; 3] = [c"disabled", c"percpu", c"phycpu"];

impl Thp {
    pub const fn name(self) -> &'static CStr {
        match self {
            Thp::Never => THP_NAMES[0],
            Thp::Auto => THP_NAMES[1],
            Thp::Always => THP_NAMES[2],
        }
    }
}

impl PercpuArena {
    pub const fn name(self) -> &'static CStr {
        match self {
            PercpuArena::Disabled => PERCPU_ARENA_NAMES[0],
            PercpuArena::Percpu => PERCPU_ARENA_NAMES[1],
            PercpuArena::Phycpu => PERCPU_ARENA_NAMES[2],
        }
    }
}

/// Every tunable of design 12; [`Config::DEFAULT`] holds the
/// compiled defaults. `docs/tuning.md` section 2 documents each key: its
/// values, what it does, when to change it and what that costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Run decay on a background thread (`purge::background`): `Some(true)`
    /// from the first periodic tick, `Some(false)` never, `None` (the
    /// default) lazily, once the process holds enough dirty memory
    /// (`purge::background::configure`).
    pub background_thread: Option<bool>,
    /// The decay window of the slabs and huge extents; `-1` never purges.
    pub dirty_decay_ms: i64,
    /// jemalloc's key, stored and readable; nothing acts on it.
    pub muzzy_decay_ms: i64,
    /// Automatic arenas; 0 is [`Config::effective_narenas`]'s default.
    pub narenas: u32,
    /// jemalloc's key ([`PercpuArena`]); caches are per thread whatever it says.
    pub percpu_arena: PercpuArena,
    /// Thread caches on the fast paths.
    pub tcache: bool,
    /// Largest cached class size; larger classes are shared ([`Config::shared_class`]).
    pub tcache_max: usize,
    /// Cache capacity of the classes up to 128 bytes, the top of [`ladder`].
    pub tcache_nslots_small_max: u32,
    /// Shifts [`ladder`] by `lg_tcache_nslots_mul - 1` doublings.
    pub lg_tcache_nslots_mul: i32,
    /// Transparent huge pages for the regions ([`Thp`], `thp.rs`).
    pub thp: Thp,
    /// `MADV_FREE` for purges under 70% of a known cgroup limit (`purge`);
    /// off by default, an opt-in.
    pub lazy_band: bool,
    /// Guard slabs and guards after huge runs.
    pub guards: bool,
    /// One guard per N x 64 KiB of slabs; 0 is none; a power of two (the sized free's test is a mask).
    pub guard_slab_interval: u32,
    /// Freed huge runs larger than this are purged at once (`Huge::set_retain_max`).
    pub huge_retain_max: usize,
    /// The huge tier's own decay window; `None` follows arena 0's ([`Config::huge_decay_ms`]).
    pub huge_dirty_decay_ms: Option<i64>,
    /// Clear a freed block before it is cached or released (`Shared::push_flags`).
    pub zero_on_free: bool,
    /// Sign cached slot pointers and the TLS heap pointer with PAC (`sys::pac`).
    pub pac_pointers: bool,
    /// Random claim order on cache refills (`ClassState::claim_random`).
    pub slot_randomize: bool,
    /// Compare each free against the recently cached slots.
    pub double_free_scan: bool,
    /// Sized frees run plain `free`'s allocation check (`ClassLine::of`).
    pub exact_sized_free: bool,
    /// Abort after parsing when any source reported a problem ([`load`]).
    pub abort_conf: bool,
    /// Read and act on the memory cgroup (`sys::cgroup`, `policy.rs`).
    pub cgroup: bool,
    /// Check every known-zero slot or page handed out (slow paths only).
    pub check_zero: bool,
    /// `challoc_cow_begin` opens fork epochs (`crate::cow`; design 5,
    /// "Fork epochs"); `false` makes it return `ENOTSUP`.
    pub cow_epoch: bool,
    /// Print the `malloc_stats_print` text report through `malloc_message`
    /// at exit (jemalloc's `stats_print`; `api::print::stats_print_at_exit`).
    pub stats_print: bool,
}

impl Config {
    /// The ranges the cache-capacity rule ([`Config::cap_for`]) relies on:
    /// `tcache_nslots_small_max` in `[MIN_CAP, MAX_NSLOTS]` and
    /// `lg_tcache_nslots_mul` in its jemalloc range; and the guard slab
    /// interval `SlabTable::new` accepts: 0 or a power of two from 2 to
    /// [`MAX_GUARD_SLAB_INTERVAL`]. [`Config::DEFAULT`] has them
    /// ([`Config::lemma_default_wf`]) and every [`Config::set`] keeps them
    /// (the parsers clamp; the interval's also lifts 1 to 2 and rounds up
    /// to a power of two).
    pub open spec fn wf(&self) -> bool {
        &&& MIN_CAP <= self.tcache_nslots_small_max <= MAX_NSLOTS
        &&& LG_NSLOTS_MUL_MIN <= self.lg_tcache_nslots_mul <= LG_NSLOTS_MUL_MAX
        &&& self.guard_slab_interval == 0 || (2 <= self.guard_slab_interval <= MAX_GUARD_SLAB_INTERVAL
            && crate::meta::is_pow2(self.guard_slab_interval as usize))
    }

    /// [`Config::DEFAULT`] is well formed.
    pub proof fn lemma_default_wf()
        ensures
            Config::DEFAULT.wf(),
    {
        vstd::arithmetic::power2::lemma2_to64();
        assert(8usize as nat == vstd::arithmetic::power2::pow2(3));
        assert(crate::meta::is_pow2(8usize));
        assert(MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
    }

    /// The compiled defaults (design 12).
    pub const DEFAULT: Config = Config {
        background_thread: None,
        dirty_decay_ms: 10_000,
        muzzy_decay_ms: 0,
        narenas: 0,
        percpu_arena: PercpuArena::Disabled,
        tcache: true,
        tcache_max: DEFAULT_TCACHE_MAX,
        tcache_nslots_small_max: MAX_CAP,
        lg_tcache_nslots_mul: 1,
        thp: Thp::Never,
        lazy_band: false,
        guards: true,
        guard_slab_interval: 8,
        huge_retain_max: crate::huge::DEFAULT_RETAIN_MAX,
        huge_dirty_decay_ms: None,
        zero_on_free: false,
        pac_pointers: false,
        slot_randomize: false,
        double_free_scan: false,
        exact_sized_free: false,
        abort_conf: false,
        cgroup: true,
        check_zero: false,
        cow_epoch: true,
        stats_print: false,
    };

    /// Apply one `key:value,key:value` string on top of `self`. Each problem
    /// is passed to `report`; the corresponding pair is skipped (or clamped,
    /// for [`Issue::OutOfRange`]) and parsing continues with the next pair.
    /// Returns the number of problems reported.
    pub fn parse(&mut self, text: &[u8], mut report: impl Report) -> (issues: usize)
        requires
            old(self).wf(),
        ensures
            issues <= text.len(),
            final(self).wf(),
    {
        self.parse_with(text, &mut report)
    }

    /// [`parse`](Self::parse) through a borrowed `report`, which the
    /// caller can go on using.
    fn parse_with<R: Report>(&mut self, text: &[u8], report: &mut R) -> (issues: usize)
        requires
            old(self).wf(),
        ensures
            issues <= text.len(),
            final(self).wf(),
    {
        let mut issues = 0;
        let mut pairs = Pairs(text);
        // `for pair in Pairs(text)`, spelled out: Verus has no
        // specification for a hand-written `Iterator`.
        while let Some(pair) = pairs.next_pair()
            invariant
                pairs.0@.len() <= text.len(),
                issues <= text.len() - pairs.0@.len(),
                self.wf(),
            decreases pairs.0@.len(),
        {
            if let Err(issue) = self.apply_pair(pair) {
                issues += 1;
                report.report(issue);
            }
        }
        issues
    }

    /// [`parse`](Self::parse) the text a source reader returned, `cut` when
    /// the reader stopped at [`MAX_TEXT`](linux::MAX_TEXT) bytes before the
    /// end of the source. A cut source is reported as [`Issue::Cut`], and
    /// the pair that runs to the cut is dropped rather than applied with a
    /// shortened value. Returns the number of problems reported, at least
    /// one for a cut source.
    pub fn parse_source(
        &mut self,
        text: &[u8],
        cut: bool,
        mut report: impl Report,
    ) -> (issues: usize)
        requires
            old(self).wf(),
            text@.len() < usize::MAX,
        ensures
            issues <= text.len() + 1,
            cut ==> issues > 0,
            final(self).wf(),
    {
        if !cut {
            return self.parse(text, report);
        }
        let keep = before_last_pair(text);
        let issues = self.parse_with(&text[..keep], &mut report);
        report.report(Issue::Cut);
        issues + 1
    }

    /// Split one pair and set the key. Empty pairs (from `a,,b` or a
    /// trailing comma) are ignored, as jemalloc ignores them. A value with
    /// an unterminated quote is malformed whatever the key.
    fn apply_pair<'a>(&mut self, pair: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
        ensures
            pair@.len() == 0 ==> r is Ok && *final(self) == *old(self),
            pair@.len() > 0 && (forall|j: int| 0 <= j < pair@.len() ==> pair@[j] != b':')
                ==> r == Err::<(), Issue<'a>>(Issue::Malformed(pair)),
            pair@.len() > 0 && pair@[0] == b':' ==> r == Err::<(), Issue<'a>>(Issue::Malformed(pair)),
            final(self).wf(),
    {
        if pair.is_empty() {
            return Ok(());
        }
        let Some(colon) = position_of(pair, b':') else {
            return Err(Issue::Malformed(pair));
        };
        proof {
            assert(colon < pair@.len());
            assert(colon + 1 <= pair.len());
        }
        let (key, value) = (&pair[..colon], &pair[colon + 1..]);
        let Some(value) = unquote(value) else {
            return Err(Issue::Malformed(pair));
        };
        if key.is_empty() || value.is_empty() {
            return Err(Issue::Malformed(pair));
        }
        self.set(key, value)
    }

    /// Set one key from its textual value: a [`KEYS`] entry's parser
    /// ([`Config::set_at`]), an ignored key ([`IGNORED_KEYS`]: any
    /// value), else [`Issue::UnknownKey`]. An out-of-range number is
    /// clamped *and* returned as [`Issue::OutOfRange`].
    pub fn set<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
    {
        if let Some(r) = self.set_listed(key, value) {
            return r;
        }
        if contains(IGNORED_KEYS, key) {
            // jemalloc keys accepted for compatibility and ignored: their
            // values are not validated because we do nothing with them.
            return Ok(());
        }
        Err(Issue::UnknownKey(key))
    }

    /// The parser of the [`KEYS`] entry named `key`, `None` (the config
    /// unchanged) when no entry is.
    #[inline(always)]
    fn set_listed<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Option<Result<(), Issue<'a>>>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            r is None <==> !listed(key@),
            r is None ==> *final(self) == *old(self),
    {
        let i = find(key)?;
        Some(self.set_at(i, key, value))
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`: the reader
    /// of the [`KEYS`] entry named `key`. `None` for keys that are only
    /// accepted and ignored and for unknown keys.
    pub fn get(&self, key: &[u8]) -> (r: Option<Value>)
        ensures
            r is None <==> !listed(key@),
    {
        let i = find(key)?;
        Some(self.read_at(i))
    }

    /// Parse `value` into the tunable `KEYS[i]` names. The fields of
    /// [`Config::wf`] are written only by their own arms, clamped to the
    /// `wf` range; the guard slab interval's then lifts 1 to 2 and rounds
    /// a value that is not a power of two up to the next, which stays
    /// within the bound (itself a power of two). The config must be well
    /// formed: on a value that does not parse the interval keeps its old
    /// value, which that rounding would overflow past `2^31`.
    fn set_at<'a>(&mut self, i: usize, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
            i < NAMES@.len(),
        ensures
            final(self).wf(),
    {
        match i {
            0 => {
                // background_thread: unset it starts lazily
                // (`purge::background::configure`) and reads back
                // `false`, jemalloc's default: no thread was asked for.
                self.background_thread = Some(parse_bool(value).ok_or(Issue::BadValue { key, value })?);
                Ok(())
            },
            1 => {
                let (v, cut) = clamped(key, value, -1, MAX_INTERVAL_MS)?;
                self.dirty_decay_ms = v;
                range(cut, key, value)
            },
            2 => {
                let (v, cut) = clamped(key, value, -1, MAX_INTERVAL_MS)?;
                self.muzzy_decay_ms = v;
                range(cut, key, value)
            },
            3 => {
                let (v, cut) = clamped(key, value, 0, u32::MAX as i64)?;
                self.narenas = v as u32;
                range(cut, key, value)
            },
            4 => {
                self.percpu_arena = pick(
                    &[
                        ("disabled", PercpuArena::Disabled),
                        ("percpu", PercpuArena::Percpu),
                        ("phycpu", PercpuArena::Phycpu),
                    ],
                    key,
                    value,
                )?;
                Ok(())
            },
            5 => bool_into(&mut self.tcache, key, value),
            6 => {
                let (v, cut) = clamped(key, value, 0, crate::layout::MEDIUM_MAX as i64)?;
                self.tcache_max = v as usize;
                range(cut, key, value)
            },
            7 => {
                let (v, cut) = clamped(key, value, MIN_CAP as i64, MAX_NSLOTS as i64)?;
                self.tcache_nslots_small_max = v as u32;
                range(cut, key, value)
            },
            8 => {
                let (v, cut) = clamped(key, value, LG_NSLOTS_MUL_MIN as i64, LG_NSLOTS_MUL_MAX as i64)?;
                self.lg_tcache_nslots_mul = v as i32;
                range(cut, key, value)
            },
            9 => {
                // thp: `default` is jemalloc's spelling of `never`.
                self.thp = pick(
                    &[
                        ("never", Thp::Never),
                        ("default", Thp::Never),
                        ("auto", Thp::Auto),
                        ("always", Thp::Always),
                    ],
                    key,
                    value,
                )?;
                Ok(())
            },
            10 => bool_into(&mut self.lazy_band, key, value),
            11 => bool_into(&mut self.guards, key, value),
            12 => self.set_guard_slab_interval(key, value),
            13 => {
                proof {
                    assert(MAX_HUGE_RETAIN_MAX == 0x100_0000_0000) by (compute);
                }
                let (v, cut) = clamped(key, value, 0, MAX_HUGE_RETAIN_MAX as i64)?;
                self.huge_retain_max = v as usize;
                range(cut, key, value)
            },
            14 => {
                // huge_dirty_decay_ms: unset it follows `dirty_decay_ms`,
                // which is what it reads back.
                let (v, cut) = clamped(key, value, -1, MAX_INTERVAL_MS)?;
                self.huge_dirty_decay_ms = Some(v);
                range(cut, key, value)
            },
            15 => bool_into(&mut self.zero_on_free, key, value),
            16 => bool_into(&mut self.pac_pointers, key, value),
            17 => bool_into(&mut self.slot_randomize, key, value),
            18 => bool_into(&mut self.double_free_scan, key, value),
            19 => bool_into(&mut self.exact_sized_free, key, value),
            20 => bool_into(&mut self.abort_conf, key, value),
            21 => bool_into(&mut self.cgroup, key, value),
            22 => bool_into(&mut self.check_zero, key, value),
            23 => bool_into(&mut self.cow_epoch, key, value),
            _ => bool_into(&mut self.stats_print, key, value),
        }
    }

    /// `guard_slab_interval`: 0 is off and N >= 2 is one guard per
    /// N x 64 KiB; 1 would make every slab a guard and leave nothing to
    /// allocate (`SlabTable::new` refuses it), so it is out of range and
    /// clamps to the smallest interval with any slabs, 2. The interval
    /// must be a power of two (the sized fast path's guard test is a
    /// mask): anything else rounds up and is reported, so `abort_conf`
    /// refuses it and a plain run says what it got. The maximum is a
    /// power of two, so a clamped value needs no rounding.
    fn set_guard_slab_interval<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
    {
        proof {
            vstd::arithmetic::power2::lemma2_to64();
            assert(MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
        }
        let r = match clamped(key, value, 0, MAX_GUARD_SLAB_INTERVAL as i64) {
            Ok((v, cut)) => {
                self.guard_slab_interval = v as u32;
                range(cut, key, value)
            },
            Err(e) => Err(e),
        };
        if self.guard_slab_interval == 1 {
            self.guard_slab_interval = 2;
            proof {
                assert(2usize as nat == pow2(1));
            }
            return Err(Issue::OutOfRange { key, value });
        }
        if self.guard_slab_interval != 0 && !self.guard_slab_interval.is_power_of_two() {
            let x = self.guard_slab_interval;
            proof {
                // Not a power of two, so not 2.
                assert(2u32 as nat == pow2(1));
            }
            let p = (x as usize).next_power_of_two();
            proof {
                lemma_round_within(x as usize, p);
            }
            self.guard_slab_interval = p as u32;
            return Err(Issue::Rounded { key, value, to: self.guard_slab_interval as u64 });
        }
        r
    }

    /// The value of the tunable `KEYS[i]` names, as [`Config::get`]
    /// answers it.
    fn read_at(&self, i: usize) -> (r: Value)
        requires
            i < NAMES@.len(),
    {
        match i {
            0 => Value::Bool(self.background_thread == Some(true)),
            1 => Value::Int(self.dirty_decay_ms),
            2 => Value::Int(self.muzzy_decay_ms),
            3 => Value::Uint(self.narenas as u64),
            4 => Value::Str(Name::PercpuArena(self.percpu_arena)),
            5 => Value::Bool(self.tcache),
            6 => Value::Size(self.tcache_max),
            7 => Value::Uint(self.tcache_nslots_small_max as u64),
            8 => Value::Int(self.lg_tcache_nslots_mul as i64),
            9 => Value::Str(Name::Thp(self.thp)),
            10 => Value::Bool(self.lazy_band),
            11 => Value::Bool(self.guards),
            12 => Value::Uint(self.guard_slab_interval as u64),
            13 => Value::Size(self.huge_retain_max),
            14 => Value::Int(self.huge_decay_ms()),
            15 => Value::Bool(self.zero_on_free),
            16 => Value::Bool(self.pac_pointers),
            17 => Value::Bool(self.slot_randomize),
            18 => Value::Bool(self.double_free_scan),
            19 => Value::Bool(self.exact_sized_free),
            20 => Value::Bool(self.abort_conf),
            21 => Value::Bool(self.cgroup),
            22 => Value::Bool(self.check_zero),
            23 => Value::Bool(self.cow_epoch),
            _ => Value::Bool(self.stats_print),
        }
    }

    /// Whether `class` is a *shared* class: larger than `tcache_max`, so no
    /// heap caches it and the default path (`Heap::alloc_slow`) allocates
    /// it from arena 0 whatever the thread's arena (design 13.3): its slabs
    /// are 1..4 MiB and one partial slab per class per arena, times 16
    /// arenas, is 60 MiB of mstress's RSS. Explicit
    /// `MALLOCX_ARENA` allocations keep their arena.
    #[inline]
    pub fn shared_class(&self, class: usize) -> (r: bool)
        requires
            class < crate::layout::NCLASSES,
            crate::api::ready(),
        ensures
            r == (crate::layout::class_table()[class as int].size > self.tcache_max),
    {
        crate::layout::CLASSES.table()[class].size() > self.tcache_max
    }

    /// The one rule for which classes are cached where: the capacity of
    /// the cache a heap homed on `home` keeps for `class` in `arena`. Zero
    /// means no cache: the slot goes straight to the bitmaps and the heap
    /// creates no `others` node for it. Zero when `tcache` is off, when
    /// the class is a [`shared_class`], and, for another arena than the
    /// home one, above `layout::BIN_MAX`: a thread caches slots of every
    /// arena it frees into, so a foreign cache's residue is multiplied by
    /// the arena count (16 threads freeing 4..32 KiB slots of 16 arenas
    /// held 8 slots per (thread, arena, class), 190 MiB of mstress's RSS,
    /// where jemalloc's single per-thread cache holds one set of 20). Else
    /// the design's ladder: `tcache_nslots_small_max` slots up to 128
    /// bytes, halving per doubling of the class size, shifted by
    /// `lg_tcache_nslots_mul - 1` doublings and clamped to `[MIN_CAP,
    /// tcache_nslots_small_max]`.
    ///
    /// [`shared_class`]: Config::shared_class
    pub fn cap_for(&self, home: u16, arena: u16, class: usize) -> (r: u32)
        requires
            class < crate::layout::NCLASSES,
            crate::api::ready(),
            LG_NSLOTS_MUL_MIN <= self.lg_tcache_nslots_mul <= LG_NSLOTS_MUL_MAX,
        ensures
            !self.tcache ==> r == 0,
            crate::layout::class_table()[class as int].size > self.tcache_max ==> r == 0,
            self.tcache && crate::layout::class_table()[class as int].size <= self.tcache_max && arena == home
                ==> MIN_CAP <= r <= vstd::math::max(self.tcache_nslots_small_max as int, MIN_CAP as int),
            r <= vstd::math::max(self.tcache_nslots_small_max as int, MIN_CAP as int),
            self.tcache_nslots_small_max <= MAX_NSLOTS ==> r <= MAX_NSLOTS,
    {
        if !self.tcache
            || self.shared_class(class)
            || (arena != home && class >= crate::layout::NBINS)
        {
            return 0;
        }
        ladder(
            class,
            self.tcache_nslots_small_max,
            self.lg_tcache_nslots_mul,
        )
    }

    /// The guard slab interval actually used: 0 when guards are off.
    pub fn effective_guard_slab_interval(&self) -> (r: u32)
        ensures
            r == if self.guards { self.guard_slab_interval } else { 0 },
    {
        if self.guards {
            self.guard_slab_interval
        } else {
            0
        }
    }

    /// The number of automatic arenas actually used: `narenas` when set,
    /// otherwise jemalloc's `4 * ncpus` capped at [`MAX_AUTO_ARENAS`]
    /// (`ncpus` is the affinity mask's width, `sys::ncpus`; a failed probe
    /// reports 1, so the fallback is 4 arenas). The cap bounds the arena
    /// table walk `Arenas::assign` does per heap creation and the number
    /// of partial slab sets a thread-heavy program can hold; with the
    /// least-loaded assignment only `min(live threads, narenas)` arenas
    /// are ever built.
    pub fn effective_narenas(&self, ncpus: u32) -> (r: u32)
        ensures
            self.narenas != 0 ==> r == self.narenas,
            self.narenas == 0 ==> r == vstd::math::min(4 * vstd::math::max(ncpus as int, 1), MAX_AUTO_ARENAS as int),
            self.narenas == 0 ==> 4 <= r <= MAX_AUTO_ARENAS,
    {
        if self.narenas != 0 {
            return self.narenas;
        }
        ncpus.max(1).saturating_mul(4).min(MAX_AUTO_ARENAS)
    }

    /// The huge tier's decay period in milliseconds as configured:
    /// `huge_dirty_decay_ms` when set, else `dirty_decay_ms` (the value
    /// `opt.huge_dirty_decay_ms` reads). At run time the huge tier
    /// follows arena 0's live period when the key is unset
    /// (`Shared::huge_decay_ns`).
    pub fn huge_decay_ms(&self) -> (r: i64)
        ensures
            r == match self.huge_dirty_decay_ms {
                Some(ms) => ms,
                None => self.dirty_decay_ms,
            },
    {
        self.huge_dirty_decay_ms.unwrap_or(self.dirty_decay_ms)
    }
}

/// Cap of the automatic `narenas` default (design 13.3).
pub const MAX_AUTO_ARENAS: u32 = 64;

} // verus!

// `THP_NAMES`, checked: the names `Thp::name` gives, in variant order, are
// the C strings `never`, `auto` and `always`. The compiler evaluates this
// in every build; Verus sees the constant only as a value of its type.
const _: () = assert!(
    matches!(THP_NAMES[0].to_bytes(), b"never")
        && matches!(THP_NAMES[1].to_bytes(), b"auto")
        && matches!(THP_NAMES[2].to_bytes(), b"always")
);

// `PERCPU_ARENA_NAMES`, checked: the names `PercpuArena::name` gives, in
// variant order, are the C strings `disabled`, `percpu` and `phycpu`, as
// for `THP_NAMES`.
const _: () = assert!(
    matches!(PERCPU_ARENA_NAMES[0].to_bytes(), b"disabled")
        && matches!(PERCPU_ARENA_NAMES[1].to_bytes(), b"percpu")
        && matches!(PERCPU_ARENA_NAMES[2].to_bytes(), b"phycpu")
);

verus! {

/// Whether some [`KEYS`] entry is named `key`.
pub closed spec fn listed(key: Seq<u8>) -> bool {
    exists|j: int| 0 <= j < NAMES@.len() && #[trigger] NAMES@[j].spec_bytes() == key
}

/// The index of the [`KEYS`] entry named `key`, if any.
fn find(key: &[u8]) -> (r: Option<usize>)
    ensures
        r matches Some(i) ==> i < NAMES@.len() && NAMES@[i as int].spec_bytes() == key@,
        r is None <==> !listed(key@),
{
    // `KEYS.iter().position(|k| k.as_bytes() == key)`, with an index:
    // the loop's invariant is what `r is None` needs.
    let mut i = 0;
    while i < NAMES.len()
        invariant
            i <= NAMES@.len(),
            forall|j: int| 0 <= j < i ==> #[trigger] NAMES@[j].spec_bytes() != key@,
        decreases NAMES@.len() - i,
    {
        if NAMES[i].as_bytes() == key {
            proof {
                assert(NAMES@[i as int].spec_bytes() =~= key@);
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `Ok(())`, or [`Issue::OutOfRange`] when the value was clamped (`cut`).
fn range<'a>(cut: bool, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
    ensures
        r == if cut { Err::<(), Issue<'a>>(Issue::OutOfRange { key, value }) } else { Ok(()) },
{
    if cut {
        Err(Issue::OutOfRange { key, value })
    } else {
        Ok(())
    }
}

/// The guard slab interval's round-up stays well formed: the next power
/// of two `p` of an interval `x` in `(2, MAX_GUARD_SLAB_INTERVAL]` is
/// below `2x` (`usize::next_power_of_two`), so below
/// `2 * MAX_GUARD_SLAB_INTERVAL`, and a power of two below twice a power
/// of two is at most it.
proof fn lemma_round_within(x: usize, p: usize)
    requires
        2 < x <= MAX_GUARD_SLAB_INTERVAL,
        exists|k: nat| k < 64 && p as nat == #[trigger] pow2(k),
        x <= p < 2 * x,
    ensures
        2 <= p <= MAX_GUARD_SLAB_INTERVAL,
        crate::meta::is_pow2(p),
        (p as u32) as usize == p,
{
    vstd::arithmetic::power2::lemma2_to64();
    assert(MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
    let k = choose|k: nat| k < 64 && p as nat == #[trigger] pow2(k);
    if k > 23 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(23, k);
    }
    if k < 22 {
        vstd::arithmetic::power2::lemma_pow2_strictly_increases(k, 22);
    }
}

/// A bool key: `true` / `false` into `slot`.
fn bool_into<'a>(slot: &mut bool, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
    ensures
        match parse_bool_spec(value@) {
            Some(b) => r is Ok && *final(slot) == b,
            None => r == Err::<(), Issue<'a>>(Issue::BadValue { key, value }) && *final(slot) == *old(slot),
        },
{
    *slot = parse_bool(value).ok_or(Issue::BadValue { key, value })?;
    Ok(())
}

/// A named-value key: `value` looked up in `names` (aliases are extra
/// rows), anything else [`Issue::BadValue`].
fn pick<'a, T: Copy>(names: &[(&str, T)], key: &'a [u8], value: &'a [u8]) -> (r: Result<T, Issue<'a>>)
    ensures
        r matches Ok(t) ==> exists|j: int| 0 <= j < names@.len() && #[trigger] names@[j].0.spec_bytes() == value@ && names@[j].1 == t,
        r is Err <==> forall|j: int| 0 <= j < names@.len() ==> #[trigger] names@[j].0.spec_bytes() != value@,
        r is Err ==> r == Err::<T, Issue<'a>>(Issue::BadValue { key, value }),
{
    // `names.iter().find(..).map(..).ok_or(..)`, spelled out: Verus does
    // not specify `Iterator::find`.
    let mut i = 0;
    while i < names.len()
        invariant
            i <= names@.len(),
            forall|j: int| 0 <= j < i ==> #[trigger] names@[j].0.spec_bytes() != value@,
        decreases names@.len() - i,
    {
        if names[i].0.as_bytes() == value {
            proof {
                assert(names@[i as int].0.spec_bytes() =~= value@);
            }
            return Ok(names[i].1);
        }
        i += 1;
    }
    Err(Issue::BadValue { key, value })
}

/// Every tunable's name, in table order: the order of the `opt.`
/// namespace (`mallctl` builds its `opt.` nodes from this slice at
/// compile time, by position), of the report, and of the arms of
/// [`Config::set_at`] and [`Config::read_at`], which index it. A new key
/// is its name here, its field, and an arm in each of those two; the
/// tests check that every entry reads back, round-trips through `set`
/// and is in no other table.
// The lifetimes are spelled out: `verus!` builds an `exec const` as a
// function in the verifier's view, where `&[&str]` would have none.
#[allow(clippy::redundant_static_lifetimes)]
pub exec const KEYS: &'static [&'static str] = &NAMES;

/// [`KEYS`] as an array, the form the proofs read: a slice constant is
/// exec only.
// The lifetime is spelled out for `verus!`, as for `KEYS`.
#[allow(clippy::redundant_static_lifetimes)]
const NAMES: [&'static str; 25] = [
    "background_thread",
    "dirty_decay_ms",
    "muzzy_decay_ms",
    "narenas",
    "percpu_arena",
    "tcache",
    "tcache_max",
    "tcache_nslots_small_max",
    "lg_tcache_nslots_mul",
    "thp",
    "lazy_band",
    "guards",
    "guard_slab_interval",
    "huge_retain_max",
    "huge_dirty_decay_ms",
    "zero_on_free",
    "pac_pointers",
    "slot_randomize",
    "double_free_scan",
    "exact_sized_free",
    "abort_conf",
    "cgroup",
    "check_zero",
    "cow_epoch",
    "stats_print",
];

/// jemalloc keys that are accepted and ignored.
#[allow(clippy::redundant_static_lifetimes)]
pub exec const IGNORED_KEYS: &'static [&'static str] = &[
    "junk",
    "zero",
    "retain",
    "oversize_threshold",
    "metadata_thp",
    "confirm_conf",
];

/// Whether `key` is one of `names` (`names.iter().any(..)`, spelled out:
/// Verus does not specify `Iterator::any`).
fn contains(names: &[&str], key: &[u8]) -> (r: bool)
    ensures
        r <==> exists|j: int| 0 <= j < names@.len() && #[trigger] names@[j].spec_bytes() == key@,
{
    let mut i = 0;
    while i < names.len()
        invariant
            i <= names@.len(),
            forall|j: int| 0 <= j < i ==> #[trigger] names@[j].spec_bytes() != key@,
        decreases names@.len() - i,
    {
        if names[i].as_bytes() == key {
            proof {
                assert(names@[i as int].spec_bytes() =~= key@);
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Cache capacity ladder: `max` slots for classes up to 128 bytes, halved
/// per doubling of the class size, shifted by `lg_mul - 1` doublings and
/// clamped to `[MIN_CAP, max]`. With `max = MAX_CAP` and `lg_mul = 1` this
/// is the design 6.2 table (`heap::default_cap`); [`Config::cap_for`] adds
/// the rules for which classes are cached at all.
pub const fn ladder(class: usize, max: u32, lg_mul: i32) -> (r: u32)
    requires
        class < crate::layout::NCLASSES,
        LG_NSLOTS_MUL_MIN <= lg_mul <= LG_NSLOTS_MUL_MAX,
    ensures
        MIN_CAP <= r,
        r <= vstd::math::max(max as int, MIN_CAP as int),
{
    // Sizes are compile-time (only the slab geometry is fixed at init).
    let size = crate::layout::BASE_CLASSES[class].size();
    // Doublings above 128 bytes: sizes in (128, 256] halve once, (256, 512]
    // twice, and so on.
    let doublings = if size <= 128 {
        0
    } else {
        (size - 1).ilog2() as i32 + 1 - 7
    };
    let shift = doublings - (lg_mul - 1);
    let cap = if shift >= 32 {
        0
    } else if shift <= -32 {
        u32::MAX
    } else if shift >= 0 {
        max >> shift
    } else {
        let grown = (max as u64).saturating_mul(1u64 << (-shift));
        if grown > u32::MAX as u64 {
            u32::MAX
        } else {
            grown as u32
        }
    };
    let cap = if cap > max { max } else { cap };
    if cap < MIN_CAP { MIN_CAP } else { cap }
}

/// A readable configuration value (`mallctl` `opt.*`, the report's
/// `opt.` lines). The variant is the key's C type in `mallctl`,
/// jemalloc's for the keys jemalloc has: `Int` is `ssize_t` (the decay
/// times and the other signed keys), `Uint` is `unsigned` (a count:
/// `narenas`, `tcache_nslots_small_max`, `guard_slab_interval`), `Size`
/// is `size_t` (a byte count: `tcache_max`, `huge_retain_max`), `Str`
/// is `const char *` (a static name, handed out as [`Name::cstr`] gives
/// it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Size(usize),
    Str(Name),
}

/// One problem found while parsing; the offending bytes borrow the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue<'a> {
    /// No `key:value` shape (missing colon, empty key, empty value or an
    /// unterminated quote).
    Malformed(&'a [u8]),
    /// A key from no table.
    UnknownKey(&'a [u8]),
    /// A value that does not parse for its key's type.
    BadValue { key: &'a [u8], value: &'a [u8] },
    /// A number outside the key's range; the value was clamped.
    OutOfRange { key: &'a [u8], value: &'a [u8] },
    /// A number the key needs as a power of two; the value was rounded up
    /// to `to` (`guard_slab_interval`).
    Rounded {
        key: &'a [u8],
        value: &'a [u8],
        to: u64,
    },
    /// A source longer than [`MAX_TEXT`](linux::MAX_TEXT) bytes: everything
    /// after the last pair that ends before the cut was ignored.
    Cut,
}

/// Where [`Config::parse`] sends each problem it finds.
pub trait Report {
    /// Take one problem; nothing is claimed about what is done with it.
    fn report(&mut self, issue: Issue<'_>);
}

// `Issue` formats under any formatter, for the `challoc: <source>:
// <issue>` lines of `config::load`. Ghost only: ordinary builds have no
// `std_specs`.
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Issue<'_> {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

} // verus!

/// The tests' reports are closures.
#[cfg(test)]
impl<F: FnMut(Issue<'_>)> Report for F {
    fn report(&mut self, issue: Issue<'_>) {
        self(issue)
    }
}

verus! {

impl fmt::Display for Issue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Malformed(pair) => write!(f, "malformed conf pair \"{}\"", Bytes(pair)),
            Issue::UnknownKey(key) => write!(f, "unknown conf key \"{}\"", Bytes(key)),
            Issue::BadValue { key, value } => {
                write!(
                    f,
                    "invalid conf value \"{}\" for {}",
                    Bytes(value),
                    Bytes(key)
                )
            }
            Issue::OutOfRange { key, value } => {
                write!(
                    f,
                    "out-of-range conf value \"{}\" for {} (clamped)",
                    Bytes(value),
                    Bytes(key)
                )
            }
            Issue::Rounded { key, value, to } => {
                write!(
                    f,
                    "conf value \"{}\" for {} is not a power of two (rounded up to {})",
                    Bytes(value),
                    Bytes(key),
                    to
                )
            }
            Issue::Cut => write!(
                f,
                "conf text longer than {} bytes (ignored after the last complete pair)",
                linux::MAX_TEXT
            ),
        }
    }
}

/// Bytes shown as text, one `?` per non-printable byte, at most 64 bytes.
struct Bytes<'a>(&'a [u8]);

// Any bytes format (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Bytes<'_> {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0.iter().take(64) {
            f.write_char(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            })?;
        }
        if self.0.len() > 64 {
            f.write_str("...")?;
        }
        Ok(())
    }
}

/// Iterator over the comma-separated pairs of a conf string.
struct Pairs<'a>(&'a [u8]);

impl<'a> Pairs<'a> {
    /// The next pair, `None` once the string is used up.
    fn next_pair(&mut self) -> (r: Option<&'a [u8]>)
        ensures
            r is None <==> old(self).0@.len() == 0,
            r is None ==> final(self).0@ == old(self).0@,
            r matches Some(pair) ==> {
                &&& pair@.len() <= old(self).0@.len()
                &&& pair@ == old(self).0@.take(pair@.len() as int)
                &&& final(self).0@ == if pair@.len() < old(self).0@.len() {
                    old(self).0@.skip(pair@.len() + 1int)
                } else {
                    Seq::empty()
                }
                &&& pair@.len() < old(self).0@.len() ==> old(self).0@[pair@.len() as int] == b','
            },
    {
        if self.0.is_empty() {
            return None;
        }
        // A quoted value may contain commas: skip to the closing quote when
        // the value (after the first colon) starts with one. A quote that
        // never closes does not extend the pair; it ends at the next comma
        // like any other.
        let mut i = 0;
        let mut colon: Option<usize> = None;
        let mut quoted = false;
        while i < self.0.len()
            invariant
                i <= self.0@.len(),
                self.0 == old(self).0,
                colon matches Some(c) ==> c < i,
            ensures
                i <= self.0@.len(),
                i < self.0@.len() ==> self.0@[i as int] == b',',
            decreases self.0@.len() - i,
        {
            let b = self.0[i];
            if quoted {
                if b == b'"' {
                    quoted = false;
                }
            } else if b == b',' {
                break;
            } else if b == b':' && colon.is_none() {
                colon = Some(i);
            } else if b == b'"' && matches!(colon, Some(c) if c + 1 == i) {
                quoted = true;
            }
            i += 1;
        }
        if quoted {
            // `self.0.iter().position(|&b| b == b',').unwrap_or(i)`, spelled
            // out: Verus does not specify `Iterator::position`.
            i = position_of(self.0, b',').unwrap_or(i);
        }
        let pair = &self.0[..i];
        self.0 = if i < self.0.len() {
            &self.0[i + 1..]
        } else {
            &[]
        };
        Some(pair)
    }
}

/// The length of `text` without the pair that runs to its end: all of
/// it when `text` is empty or its last pair ends at a comma
/// (`Pairs(text).last()` and a test that it ends where `text` does,
/// spelled out: Verus has no specification for a hand-written
/// `Iterator`). What is kept is whole pairs: it is all of `text`, empty,
/// or ends at a comma.
fn before_last_pair(text: &[u8]) -> (keep: usize)
    ensures
        keep <= text@.len(),
        keep < text@.len() ==> keep == 0 || text@[keep - 1] == b',',
{
    let mut keep = text.len();
    let mut pairs = Pairs(text);
    loop
        invariant
            pairs.0@.len() <= text@.len(),
            keep <= text@.len(),
            keep < text@.len() ==> keep == 0 || text@[keep - 1] == b',',
            pairs.0@.len() > 0 ==> {
                &&& pairs.0@ == text@.skip(text@.len() - pairs.0@.len())
                &&& pairs.0@.len() == text@.len() || text@[text@.len() - pairs.0@.len() - 1] == b','
            },
        decreases pairs.0@.len(),
    {
        let rest = pairs.0.len();
        let Some(pair) = pairs.next_pair() else {
            break;
        };
        if pair.len() == rest {
            keep = text.len() - pair.len();
        }
        proof {
            if pairs.0@.len() > 0 {
                assert(pair@.len() < rest);
                assert(pairs.0@ =~= text@.skip(text@.len() - pairs.0@.len()));
            }
        }
    }
    keep
}

/// `value` without one pair of surrounding double quotes; `None` when it
/// opens a quote it does not close at its end (a quote inside the value
/// included).
pub open spec fn unquote_spec(value: Seq<u8>) -> Option<Seq<u8>> {
    if value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"'
        && !value.subrange(1, value.len() - 1).contains(b'"') {
        Some(value.subrange(1, value.len() - 1))
    } else if value.len() >= 1 && value[0] == b'"' {
        None
    } else {
        Some(value)
    }
}

/// Strip one pair of surrounding double quotes; `None` when the value opens
/// a quote it does not close at its end. (`match value { [b'"', inner @
/// .., b'"'] if !inner.contains(&b'"') => Some(inner), [b'"', ..] => None,
/// _ => Some(value) }`, spelled out: Verus has no slice patterns.)
fn unquote(value: &[u8]) -> (r: Option<&[u8]>)
    ensures
        r matches Some(v) ==> unquote_spec(value@) == Some(v@),
        r is None ==> unquote_spec(value@) is None,
{
    if value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"' {
        let inner = &value[1..value.len() - 1];
        if position_of(inner, b'"').is_none() {
            proof {
                assert(!inner@.contains(b'"'));
            }
            return Some(inner);
        }
        proof {
            let j = choose|j: int| 0 <= j < inner@.len() && inner@[j] == b'"';
            assert(inner@ == value@.subrange(1, value@.len() - 1));
            assert(value@.subrange(1, value@.len() - 1).contains(b'"')) by {
                assert(value@.subrange(1, value@.len() - 1)[j] == b'"');
            }
        }
        return None;
    }
    if !value.is_empty() && value[0] == b'"' {
        None
    } else {
        Some(value)
    }
}

/// The index of the first `b` in `s` (`s.iter().position(..)`, spelled
/// out: Verus does not specify `Iterator::position`).
fn position_of(s: &[u8], b: u8) -> (r: Option<usize>)
    ensures
        r is None ==> forall|j: int| 0 <= j < s@.len() ==> s@[j] != b,
        r matches Some(i) ==> i < s@.len() && s@[i as int] == b && forall|j: int| 0 <= j < i ==> s@[j] != b,
{
    let mut i = 0;
    while i < s.len()
        invariant
            i <= s@.len(),
            forall|j: int| 0 <= j < i ==> s@[j] != b,
        decreases s@.len() - i,
    {
        if s[i] == b {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// What [`parse_bool`] accepts.
pub open spec fn parse_bool_spec(value: Seq<u8>) -> Option<bool> {
    if value == b"true"@ {
        Some(true)
    } else if value == b"false"@ {
        Some(false)
    } else {
        None
    }
}

/// `true` / `false` only, as in jemalloc.
pub fn parse_bool(value: &[u8]) -> (r: Option<bool>)
    ensures
        r == parse_bool_spec(value@),
{
    // A `match` on byte-string patterns, spelled as comparisons: such
    // patterns crash Verus.
    if value == b"true" {
        proof {
            assert(value@ =~= b"true"@);
        }
        Some(true)
    } else if value == b"false" {
        proof {
            assert(value@ =~= b"false"@);
        }
        Some(false)
    } else {
        None
    }
}

/// `char::to_digit` on the character `c` (`core` documentation): `0`..`9`,
/// then `a`..`z` or `A`..`Z` for 10..35, when below `radix`.
pub open spec fn to_digit_spec(c: u32, radix: u32) -> Option<u32> {
    let d = if 48 <= c <= 57 {
        c - 48
    } else if 97 <= c <= 122 {
        c - 97 + 10
    } else if 65 <= c <= 90 {
        c - 65 + 10
    } else {
        36
    };
    if d < radix { Some(d as u32) } else { None }
}

/// `(b as char).to_digit(radix)`: the value of the digit `b` in `radix`.
// Verus specifies the comparisons, not `RangeInclusive::contains`.
#[allow(clippy::manual_range_contains)]
fn digit_value(b: u8, radix: u32) -> (r: Option<u32>)
    ensures
        r == to_digit_spec(b as u32, radix),
{
    let d = if b'0' <= b && b <= b'9' {
        (b - b'0') as u32
    } else if b'a' <= b && b <= b'z' {
        (b - b'a') as u32 + 10
    } else if b'A' <= b && b <= b'Z' {
        (b - b'A') as u32 + 10
    } else {
        36
    };
    if d < radix { Some(d) } else { None }
}

/// The value of the digits `s` in `radix`, most significant first; `None`
/// when a byte is not a digit of `radix`.
pub open spec fn digits_spec(s: Seq<u8>, radix: u32) -> Option<int>
    decreases s.len(),
{
    if s.len() == 0 {
        Some(0)
    } else {
        match (digits_spec(s.drop_last(), radix), to_digit_spec(s.last() as u32, radix)) {
            (Some(v), Some(d)) => Some(v * radix + d),
            _ => None,
        }
    }
}

/// The sign, the radix and the digits of a number: a leading `-` or `+`,
/// then `0x` or `0X` with at least one more byte after it for hexadecimal,
/// a leading `0` with at least one more byte after it for octal, else
/// decimal.
pub open spec fn int_parts(value: Seq<u8>) -> (bool, u32, Seq<u8>) {
    let (negative, digits) = if value.len() > 0 && value[0] == b'-' {
        (true, value.drop_first())
    } else if value.len() > 0 && value[0] == b'+' {
        (false, value.drop_first())
    } else {
        (false, value)
    };
    if digits.len() >= 3 && digits[0] == b'0' && (digits[1] == b'x' || digits[1] == b'X') {
        (negative, 16, digits.skip(2))
    } else if digits.len() >= 2 && digits[0] == b'0' {
        (negative, 8, digits.drop_first())
    } else {
        (negative, 10, digits)
    }
}

/// What [`parse_int`] returns: the number `value` spells, when it spells
/// one that fits an `i64`.
pub open spec fn parse_int_spec(value: Seq<u8>) -> Option<i64> {
    let (negative, radix, digits) = int_parts(value);
    if digits.len() == 0 {
        None
    } else {
        match digits_spec(digits, radix) {
            Some(m) => {
                let v = if negative { -m } else { m };
                if i64::MIN <= v <= i64::MAX { Some(v as i64) } else { None }
            },
            None => None,
        }
    }
}

proof fn lemma_digits_nonneg(s: Seq<u8>, radix: u32)
    requires
        radix >= 1,
    ensures
        digits_spec(s, radix) matches Some(v) ==> v >= 0,
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_digits_nonneg(s.drop_last(), radix);
        if let Some(v) = digits_spec(s.drop_last(), radix) {
            assert(v * radix >= 0) by (nonlinear_arith)
                requires
                    v >= 0,
                    radix >= 1,
            ;
        }
    }
}

/// A prefix that is not a number, or is a larger one, bounds the whole.
proof fn lemma_digits_prefix(s: Seq<u8>, k: int, radix: u32)
    requires
        radix >= 1,
        0 <= k <= s.len(),
    ensures
        digits_spec(s.take(k), radix) is None ==> digits_spec(s, radix) is None,
        digits_spec(s.take(k), radix) matches Some(p) ==> (digits_spec(s, radix) matches Some(v) ==> p <= v),
    decreases s.len() - k,
{
    if k == s.len() {
        assert(s.take(k) =~= s);
    } else {
        lemma_digits_prefix(s, k + 1, radix);
        assert(s.take(k + 1).drop_last() =~= s.take(k));
        lemma_digits_nonneg(s.take(k), radix);
        if let Some(p) = digits_spec(s.take(k), radix) {
            if let Some(q) = digits_spec(s.take(k + 1), radix) {
                let d = to_digit_spec(s.take(k + 1).last() as u32, radix)->Some_0;
                assert(q == p * radix + d);
                assert(p <= p * radix + d) by (nonlinear_arith)
                    requires
                        p >= 0,
                        radix >= 1,
                        d >= 0,
                ;
            }
        }
    }
}

/// A decimal, `0x` hexadecimal or leading-zero octal integer with an
/// optional sign (jemalloc's `malloc_strtoumax` with base 0). `None` on
/// anything else, including overflow.
#[verifier::rlimit(40)]
pub fn parse_int(value: &[u8]) -> (r: Option<i64>)
    ensures
        r == parse_int_spec(value@),
{
    // The slice patterns (`[b'-', rest @ ..]`, `[b'0', b'x' | b'X', rest
    // @ ..] if !rest.is_empty()`), spelled as length and byte tests:
    // Verus does not support slice patterns.
    let (negative, digits) = if !value.is_empty() && value[0] == b'-' {
        (true, &value[1..])
    } else if !value.is_empty() && value[0] == b'+' {
        (false, &value[1..])
    } else {
        (false, value)
    };
    let (radix, digits) =
        if digits.len() >= 3 && digits[0] == b'0' && (digits[1] == b'x' || digits[1] == b'X') {
            (16u64, &digits[2..])
        } else if digits.len() >= 2 && digits[0] == b'0' {
            (8, &digits[1..])
        } else {
            (10, digits)
        };
    proof {
        assert(int_parts(value@) == (negative, radix as u32, digits@));
    }
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: u64 = 0;
    // `for &b in digits`, spelled out with an index for the invariant.
    let mut i = 0;
    while i < digits.len()
        invariant
            i <= digits@.len(),
            digits@.len() > 0,
            radix == 8 || radix == 10 || radix == 16,
            int_parts(value@) == (negative, radix as u32, digits@),
            digits_spec(digits@.take(i as int), radix as u32) == Some(magnitude as int),
        decreases digits@.len() - i,
    {
        let b = digits[i];
        proof {
            assert(digits@.take(i + 1).drop_last() =~= digits@.take(i as int));
            assert(digits@.take(i + 1).last() == b);
            lemma_digits_prefix(digits@, i + 1, radix as u32);
        }
        let d = digit_value(b, radix as u32)? as u64;
        proof {
            assert(digits_spec(digits@.take(i + 1), radix as u32) == Some(magnitude * radix + d));
        }
        magnitude = magnitude.checked_mul(radix)?.checked_add(d)?;
        i += 1;
    }
    proof {
        assert(digits@.take(digits@.len() as int) =~= digits@);
    }
    if negative {
        if magnitude > i64::MAX as u64 + 1 {
            return None;
        }
        proof {
            assert(magnitude == 0x8000_0000_0000_0000 ==> #[verifier::truncate] (magnitude as i64) == i64::MIN)
                by (bit_vector);
            assert(magnitude < 0x8000_0000_0000_0000 ==> #[verifier::truncate] (magnitude as i64) == magnitude)
                by (bit_vector);
            assert(parse_int_spec(value@) == Some((-(magnitude as int)) as i64));
        }
        #[cfg_attr(verus_keep_ghost, verifier::truncate)]
        let magnitude = magnitude as i64;
        Some(0i64.wrapping_sub(magnitude))
    } else {
        proof {
            assert(parse_int_spec(value@) == (if magnitude <= i64::MAX as u64 { Some(magnitude as i64) } else { None }));
        }
        // `i64::try_from(magnitude).ok()`, spelled out: vstd does not
        // specify that conversion.
        if magnitude <= i64::MAX as u64 {
            Some(magnitude as i64)
        } else {
            None
        }
    }
}

/// Parse an integer and clamp it into `[min, max]`. `Ok((v, false))` for a
/// value in range, `Ok((clamped, true))` for one outside it (the caller
/// assigns the clamped value and reports [`Issue::OutOfRange`], as
/// jemalloc's `CONF_HANDLE_*` macros do), `Err` when it does not parse.
fn clamped<'a>(
    key: &'a [u8],
    value: &'a [u8],
    min: i64,
    max: i64,
) -> (r: Result<(i64, bool), Issue<'a>>)
    requires
        min <= max,
    ensures
        match parse_int_spec(value@) {
            None => r == Err::<(i64, bool), Issue<'a>>(Issue::BadValue { key, value }),
            Some(v) => r == Ok::<(i64, bool), Issue<'a>>(
                if v < min { (min, true) } else if v > max { (max, true) } else { (v, false) },
            ),
        },
{
    let v = parse_int(value).ok_or(Issue::BadValue { key, value })?;
    if v < min {
        Ok((min, true))
    } else if v > max {
        Ok((max, true))
    } else {
        Ok((v, false))
    }
}

// `Name` and its impl come last in the module: rustc numbers a module's
// impls (the derived ones included) in order, the number enters the
// mangled names of each impl's items, and those names order `.text`.
/// The static name a [`Value::Str`] carries: the value of the `thp` or
/// the `percpu_arena` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Name {
    Thp(Thp),
    PercpuArena(PercpuArena),
}

impl Name {
    /// The name as a C string ([`Thp::name`], [`PercpuArena::name`]).
    pub const fn cstr(self) -> &'static CStr {
        match self {
            Name::Thp(t) => t.name(),
            Name::PercpuArena(p) => p.name(),
        }
    }
}

} // verus!

pub(crate) use linux::byte_at;
#[cfg(verus_keep_ghost)]
pub(crate) use linux::conf_storage;
pub use linux::load;
pub(crate) use linux::{CONF_LEN, conf_buffer};
#[cfg(test)]
use linux::{MAX_TEXT, cstr, env_var, first_line_of, malloc_conf_symbol};

mod linux {
    use core::ffi::{c_char, c_void};
    use core::fmt::Write as _;

    use super::{Config, Issue, Report, position_of};
    use crate::sys;
    use vstd::prelude::*;
    use vstd::raw_ptr::PointsTo;

    verus! {

    /// Longest conf text read from any one source. Longer inputs are cut
    /// here and the cut is reported ([`Config::parse_source`]); no real
    /// configuration comes close.
    pub const MAX_TEXT: usize = 4096;

    /// The length of the buffer [`load`] reads `/etc/challoc.conf` into:
    /// one byte more than [`MAX_TEXT`].
    pub const CONF_LEN: usize = MAX_TEXT + 1;

    } // verus!

    /// [`conf_buffer`]'s storage (Verus has no `static mut`), read and
    /// written only through its pointer ([`conf_ptr`]).
    static mut CONF_BUF: [u8; CONF_LEN] = [0; CONF_LEN];

    use crate::sys::auxv::{AT_SECURE, getauxval};

    unsafe extern "C" {
        /// glibc's environment pointer. Read directly so no `getenv` runs
        /// inside `malloc`.
        static environ: *const *const c_char;
    }

    verus! {

    /// Prints each problem as `challoc: <source>: <issue>` to `out`.
    struct ToStderr<'a> {
        source: &'a str,
        out: &'a mut sys::Stderr,
    }

    impl Report for ToStderr<'_> {
        fn report(&mut self, issue: Issue<'_>) {
            let _ = writeln!(self.out, "challoc: {}: {}", self.source, issue);
        }
    }

    /// [`Config::parse_source`] of `text` from `source`, cut or not,
    /// printing each problem as `challoc: <source>: <issue>` to `out`.
    #[inline(always)]
    fn parse_from(
        cfg: &mut Config,
        text: &[u8],
        cut: bool,
        source: &str,
        out: &mut sys::Stderr,
    ) -> (issues: usize)
        requires
            old(cfg).wf(),
            text@.len() <= MAX_TEXT,
        ensures
            issues <= text.len() + 1,
            cut ==> issues > 0,
            final(cfg).wf(),
    {
        cfg.parse_source(text, cut, ToStderr { source, out })
    }

    /// `/etc/challoc.conf`, NUL-terminated (spelled as bytes so the
    /// verifier sees the NUL `first_line_of` requires).
    const CHALLOC_CONF: [u8; 18] = [
        b'/', b'e', b't', b'c', b'/', b'c', b'h', b'a', b'l', b'l', b'o', b'c', b'.', b'c', b'o',
        b'n', b'f', 0,
    ];

    /// The address of [`CONF_BUF`]'s storage.
    pub uninterp spec fn conf_storage() -> *mut [u8; CONF_LEN];

    /// The storage of [`CONF_BUF`]. Trusted: a raw-pointer projection of
    /// a static.
    #[verifier::external_body]
    #[inline(always)]
    fn conf_ptr() -> (p: *mut [u8; CONF_LEN])
        ensures
            p == conf_storage(),
        opens_invariants none
    {
        core::ptr::addr_of_mut!(CONF_BUF)
    }

    /// The buffer `api::init` has [`load`] read `/etc/challoc.conf` into:
    /// [`CONF_BUF`], a static, so init's frame does not carry 4 KiB of it
    /// (design 13.8: init's frame is capped, though challoc initialises
    /// before `main`, so init does not normally run on a program's small
    /// thread). Through the permission to its storage, which the
    /// process-start token holds (`api::Statics`), so the reference lives
    /// no longer than the borrow of that one permission.
    pub fn conf_buffer(Tracked(conf): Tracked<&mut PointsTo<[u8; CONF_LEN]>>) -> (r: &mut [u8; CONF_LEN])
        requires
            old(conf).ptr() == conf_storage(),
            old(conf).is_init(),
    {
        vstd::raw_ptr::ptr_mut_ref(conf_ptr(), Tracked(conf))
    }

    /// Read every source in priority order into a `Config`, printing each
    /// problem as `challoc: <source>: <issue>` on stderr. Aborts when
    /// `abort_conf` is set by the time all sources are read and anything was
    /// reported, a cut source included. Never allocates: the
    /// file goes through `buf` (`api::init` passes [`CONF_BUF`], through
    /// [`conf_buffer`]; a unit test, which holds no permission to it, passes
    /// its own) and the environment is walked in place. The result is well formed: the defaults are
    /// (`Config::lemma_default_wf`) and every source goes through
    /// `Config::parse_source`, which keeps it.
    ///
    /// Two steps: [`load_sources`], the sources that read no environment,
    /// then [`load_env`], the two variables, whose contract says that in a
    /// secure-execution process it changes neither the `Config` nor the
    /// issue count ([`theorem_secure_conf`]).
    pub fn load(buf: &mut [u8; CONF_LEN]) -> (r: Config)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
        ensures
            r.wf(),
    {
        let mut cfg = Config::DEFAULT;
        proof {
            Config::lemma_default_wf();
        }
        let mut out = sys::Stderr::new();
        let mut issues = load_sources(&mut cfg, buf, &mut out);
        load_env(&mut cfg, &mut issues, &mut out);
        out.flush();
        if cfg.abort_conf && issues > 0 {
            sys::abort_fmt(format_args!(
                "abort_conf: {issues} configuration problem(s), see above"
            ));
        }
        cfg
    }

    /// [`load`]'s first step, over the defaults `load` passes: the
    /// `malloc_conf` and `je_malloc_conf` symbols, then the first line of
    /// `/etc/challoc.conf`, returning the number of problems reported.
    /// None of these reads the environment.
    #[inline(always)]
    fn load_sources(cfg: &mut Config, buf: &mut [u8; CONF_LEN], out: &mut sys::Stderr) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            old(cfg).wf(),
        ensures
            final(cfg).wf(),
            r <= 3 * (MAX_TEXT + 1),
    {
        let mut issues = 0;
        // `for (name, text) in [...]` over the two symbols (and, in
        // `load_env`, the two variables), spelled out: Verus does not
        // specify an array's `IntoIter`. Both symbols are read before
        // either is parsed.
        let (sym, je_sym) = (malloc_conf_symbol(0), malloc_conf_symbol(1));
        if let Some((text, cut)) = sym {
            issues += parse_from(cfg, text, cut, "malloc_conf", out);
        }
        if let Some((text, cut)) = je_sym {
            issues += parse_from(cfg, text, cut, "je_malloc_conf", out);
        }
        if let Some((line, cut)) = first_line_of(&CHALLOC_CONF, buf) {
            issues += parse_from(cfg, line, cut, "/etc/challoc.conf", out);
        }
        issues
    }

    /// [`load`]'s second step: `MALLOC_CONF`, then `CHALLOC_CONF`, over
    /// `cfg`, adding the number of problems reported to `*issues`. In a
    /// secure-execution process it reads neither variable and leaves
    /// `cfg` and `*issues` as they were. Each variable is parsed only when
    /// [`env_var`] finds it, so with neither variable set it does the
    /// same; that case is read from this body, not from the contract,
    /// which cannot name `env_var`'s results.
    #[inline(always)]
    fn load_env(cfg: &mut Config, issues: &mut usize, out: &mut sys::Stderr)
        requires
            old(cfg).wf(),
            *old(issues) <= 3 * (MAX_TEXT + 1),
        ensures
            final(cfg).wf(),
            *final(issues) <= *old(issues) + 2 * (MAX_TEXT + 1),
            crate::sys::auxv::secure_exec() ==> *final(cfg) == *old(cfg) && *final(issues) == *old(issues),
    {
        if getauxval(AT_SECURE) == 0 {
            if let Some((text, cut)) = env_var(b"MALLOC_CONF") {
                *issues += parse_from(cfg, text, cut, "MALLOC_CONF", out);
            }
            if let Some((text, cut)) = env_var(b"CHALLOC_CONF") {
                *issues += parse_from(cfg, text, cut, "CHALLOC_CONF", out);
            }
        }
    }

    /// Secure execution ignores the environment, closing the shape of
    /// CVE-2023-4911 (glibc's `GLIBC_TUNABLES`, malloc tunables among them,
    /// parsed by a setuid program's loader): in a process started with
    /// `AT_SECURE` set (`sys::auxv::secure_exec`: setuid, setgid, file
    /// capabilities or an LSM transition), no call of [`env_var`] returns,
    /// whatever its name and result: Verus rejects every verified caller
    /// that has not established `!secure_exec()`. So [`load_env`], whose
    /// two calls meet `env_var`'s precondition only through its
    /// `getauxval(AT_SECURE) == 0` test, reads neither `MALLOC_CONF` nor
    /// `CHALLOC_CONF`.
    ///
    /// The configuration is then the one the same process would get with
    /// neither variable set, with no functional spec of
    /// `Config::parse_source`: [`load`] is [`load_sources`] (the defaults,
    /// the `malloc_conf` and `je_malloc_conf` symbols, the first line of
    /// `/etc/challoc.conf`) followed by `load_env`, and `load_env`'s
    /// contract states that in a secure-execution process it returns
    /// with the `Config` it was given and no problems reported. So the
    /// result, and the `abort_conf` decision, are `load_sources`'s. With
    /// neither variable set, `load_env` parses nothing either, read from
    /// its body (each parse is inside the `Some` of its `env_var` call);
    /// `env_var` has no spec of the environment it reads, so no contract
    /// states that case. Not stated either: that `load_sources` reads the
    /// same in the two processes (the same symbols, the same file).
    ///
    /// Trusted: `getauxval(AT_SECURE)` returns the process's one
    /// `AT_SECURE` word (`sys::auxv::at_secure`, in the `getauxval` seam);
    /// `env_var`'s body is verified against its contract. Checked by a
    /// test, not by Verus: `env_var` is the only reader of `environ`
    /// (through [`environ_array`]), outside the tests `load_env` is its
    /// only caller, and `load` is `load_env`'s
    /// (`config::linux_tests::environment_has_one_reader`).
    pub proof fn theorem_secure_conf(name: &[u8], r: Option<(&'static [u8], bool)>)
        requires
            crate::sys::auxv::secure_exec(),
        ensures
            !call_ensures(env_var, (name,), r),
    {
    }

    /// The string the `malloc_conf` (`which == 0`, defined by the library,
    /// interposed by a program's definition) or weak `je_malloc_conf`
    /// (`which == 1`) global points at, and whether it was cut, as
    /// [`cstr`] returns it. `None` when the process defines neither the
    /// symbol nor a string. The symbol's pointer comes through the GOT and
    /// the string through `cstr`, which stops at [`MAX_TEXT`] bytes.
    pub fn malloc_conf_symbol(which: u32) -> (r: Option<(&'static [u8], bool)>)
        ensures
            r matches Some((s, cut)) ==> s@.len() <= MAX_TEXT,
    {
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
        cstr(unsafe { read_slot(slot) })
    }

    /// The `const char *` at `slot` (a symbol, or an `environ` entry).
    /// Trusted: a raw read; nothing is claimed about the pointer.
    ///
    /// # Safety
    /// `slot` is a defined `const char *` object.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn read_slot(slot: *const *const c_char) -> *const c_char {
        // SAFETY: guaranteed by the caller.
        unsafe { slot.read() }
    }

    /// Address of the `malloc_conf` global the loader resolved: the
    /// program's own definition when it has one, else the library's null
    /// slot (`api::jemalloc::malloc_conf`). The load goes through the GOT
    /// so the program's definition interposes; a direct reference could be
    /// bound to the library's own copy at link time. Trusted: inline
    /// assembly; nothing is claimed about the address.
    #[verifier::external_body]
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
    /// this one), or null. Trusted: as `malloc_conf_addr`.
    #[verifier::external_body]
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

    /// A NUL-terminated string as a slice of at most [`MAX_TEXT`] bytes,
    /// and whether the string was cut there (longer than [`MAX_TEXT`]
    /// bytes); `None` for null or empty.
    pub fn cstr(p: *const c_char) -> (r: Option<(&'static [u8], bool)>)
        ensures
            r matches Some((s, cut)) ==> 0 < s@.len() <= MAX_TEXT && (cut ==> s@.len() == MAX_TEXT),
    {
        if p.is_null() {
            return None;
        }
        let p = p.cast::<u8>();
        let mut n = 0;
        // SAFETY: the caller hands us a NUL-terminated string; reads stop at
        // the terminator or at the cap.
        while n < MAX_TEXT && unsafe { byte_at(p, n) } != 0
            invariant
                n <= MAX_TEXT,
            decreases MAX_TEXT - n,
        {
            n += 1;
        }
        if n == 0 {
            return None;
        }
        // SAFETY: at the cap no terminator has been seen yet, so the byte at
        // `n` is still part of the string.
        let cut = n == MAX_TEXT && unsafe { byte_at(p, n) } != 0;
        // SAFETY: `n` bytes before the terminator (or the cap) were just read.
        Some((unsafe { bytes_at(p, n) }, cut))
    }

    /// The byte at `p + n`. Trusted: a raw read; nothing is claimed about
    /// the value. Also `purge::background::single_threaded`'s read of
    /// glibc's `__libc_single_threaded`.
    ///
    /// # Safety
    /// `p + n` is a readable byte (of the caller's string, or a `char`
    /// object).
    #[verifier::external_body]
    #[inline(always)]
    pub(crate) unsafe fn byte_at(p: *const u8, n: usize) -> u8 {
        // SAFETY: guaranteed by the caller.
        unsafe { p.add(n).read() }
    }

    /// The `n` bytes at `p` as a slice. Trusted: a slice from a raw
    /// pointer (the `# Safety` contract).
    ///
    /// # Safety
    /// The `n` bytes at `p` are readable and live for the process.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn bytes_at(p: *const u8, n: usize) -> (r: &'static [u8])
        ensures
            r@.len() == n,
    {
        // SAFETY: guaranteed by the caller.
        unsafe { core::slice::from_raw_parts(p, n) }
    }

    /// glibc's `environ`, null before libc has set it up. Trusted: a read
    /// of the C global; nothing is claimed about the pointer.
    #[verifier::external_body]
    #[inline(always)]
    fn environ_array() -> *const *const c_char {
        // SAFETY: a plain read of a pointer-sized global.
        unsafe { environ }
    }

    /// Most `environ` entries, its null included, that [`env_var`] reads:
    /// the array is one object of 8-byte pointers (challoc builds for
    /// 64-bit targets only), and no object is larger than `isize::MAX`
    /// bytes, so a well-formed `environ` ends before this many entries.
    /// The bound only gives the walk a measure; a const assertion after
    /// this `verus!` block holds it at or above that object bound.
    const ENVIRON_MAX: usize = isize::MAX as usize / 8;

    /// The value of environment variable `name` (no `=` in it), read from
    /// `environ` without `getenv`, and whether it was cut, as [`cstr`]
    /// returns it for the value alone. `None` when unset or empty.
    ///
    /// Each entry is read through `cstr`, and when it sets `name`
    /// ([`env_sets`]) its value is read through `cstr` again, which stops at
    /// [`MAX_TEXT`] bytes. The walk ends at the null pointer that ends
    /// `environ`, and in any case after [`ENVIRON_MAX`] entries, a bound
    /// no `environ` reaches; Verus checks that it terminates.
    ///
    /// Requires a process that is not a secure-execution one
    /// (`sys::auxv::secure_exec`), whose environment is untrusted, so Verus
    /// checks that every verified caller has tested `AT_SECURE` first. The
    /// ensures restates it, so that [`theorem_secure_conf`] can state it of
    /// `env_var` itself: Verus gives a function's `call_ensures` its
    /// postcondition, but not its `call_requires` its precondition.
    // Verus has no let-chains, so the `env_sets` test stays nested.
    #[allow(clippy::collapsible_if)]
    pub fn env_var(name: &[u8]) -> (r: Option<(&'static [u8], bool)>)
        requires
            !crate::sys::auxv::secure_exec(),
        ensures
            !crate::sys::auxv::secure_exec(),
            r matches Some((s, cut)) ==> 0 < s@.len() <= MAX_TEXT && (cut ==> s@.len() == MAX_TEXT),
    {
        // `environ` is glibc's NULL-terminated array of NUL-terminated
        // strings; it is null before libc has set it up.
        let mut e = environ_array();
        if e.is_null() {
            return None;
        }
        let mut read: usize = 0;
        loop
            invariant
                !crate::sys::auxv::secure_exec(),
                read <= ENVIRON_MAX,
            decreases ENVIRON_MAX - read,
        {
            if read == ENVIRON_MAX {
                // No `environ` gets here: see `ENVIRON_MAX`.
                return None;
            }
            read += 1;
            // SAFETY: `e` walks the array up to its terminating null.
            let entry = unsafe { read_slot(e) };
            if entry.is_null() {
                return None;
            }
            if let Some((text, _)) = cstr(entry) {
                if env_sets(text, name) {
                    // The value starts after `name=`, inside the entry just
                    // read.
                    return cstr(entry.wrapping_add(name.len() + 1));
                }
            }
            // Not yet at the terminating null.
            e = e.wrapping_add(1);
        }
    }

    /// The environment entry `text` sets `name`: `text` is `name`, `=`,
    /// then the value.
    spec fn entry_sets(text: Seq<u8>, name: Seq<u8>) -> bool {
        &&& text.len() > name.len()
        &&& text.take(name.len() as int) == name
        &&& text[name.len() as int] == b'='
    }

    /// Whether the environment entry `text` (`NAME=value`) sets `name`.
    #[inline(always)]
    fn env_sets(text: &[u8], name: &[u8]) -> (r: bool)
        ensures
            r == entry_sets(text@, name@),
    {
        let r = text.len() > name.len() && &text[..name.len()] == name && text[name.len()] == b'=';
        proof {
            if text@.len() > name@.len() {
                let head = text@.take(name@.len() as int);
                if head == name@ {
                    assert(forall|i: int| 0 <= i < name@.len() ==> head[i] == name@[i]);
                }
                if r {
                    assert(head =~= name@);
                }
            }
        }
        r
    }

    /// The first line of the file at NUL-terminated `path`, without its
    /// newline, read with raw syscalls into `buf`, and whether it was cut
    /// (longer than `buf.len() - 1` bytes, so the line is at most
    /// `buf.len() - 1` bytes long). `None` if the file is missing,
    /// unreadable or its first line is empty.
    pub fn first_line_of<'b>(path: &[u8], buf: &'b mut [u8]) -> (r: Option<(&'b [u8], bool)>)
        requires
            crate::sys::may_syscall(),
            path@.len() > 0,
            path@.last() == 0,
            old(buf)@.len() > 0,
        ensures
            r matches Some((line, cut)) ==> line@.len() < old(buf)@.len(),
    {
        let n = sys::read_small_file(path, buf)?;
        first_line(&buf[..n], n == buf.len())
    }

    /// The first line of `text` without its `\n` and one `\r` before it,
    /// and whether it was cut: `full` (the read filled its buffer) and no
    /// newline in `text`, when the line also loses the last byte, so that a
    /// cut line is shorter than the buffer. `None` when that line is empty.
    #[inline(always)]
    fn first_line(text: &[u8], full: bool) -> (r: Option<(&[u8], bool)>)
        requires
            full ==> text@.len() > 0,
        ensures
            r matches Some((line, cut)) ==> {
                &&& 0 < line@.len() <= text@.len()
                &&& full ==> line@.len() < text@.len()
                &&& line@ == text@.take(line@.len() as int)
                &&& forall|j: int| 0 <= j < line@.len() ==> line@[j] != b'\n'
                &&& cut <==> full && forall|j: int| 0 <= j < text@.len() ==> text@[j] != b'\n'
            },
    {
        let newline = position_of(text, b'\n');
        let cut = newline.is_none() && full;
        let line = match newline {
            Some(end) => &text[..end],
            None if cut => &text[..text.len() - 1],
            None => text,
        };
        // `line.strip_suffix(b"\r").unwrap_or(line)`, spelled out: vstd
        // does not specify `strip_suffix`.
        let line = if !line.is_empty() && line[line.len() - 1] == b'\r' {
            &line[..line.len() - 1]
        } else {
            line
        };
        if line.is_empty() {
            None
        } else {
            Some((line, cut))
        }
    }

    } // verus!

    // `ENVIRON_MAX` is at least the most pointers one object can hold, so
    // no well-formed `environ` reaches `env_var`'s cap: a smaller value
    // would verify and pass the tests, yet cut long environments short.
    const _: () =
        assert!(ENVIRON_MAX >= isize::MAX as usize / core::mem::size_of::<*const c_char>());

    /// The Kani harness of [`conf_ptr`], next to the private function and
    /// its static.
    #[cfg(kani)]
    mod kani_conf_ptr {
        use super::{CONF_BUF, MAX_TEXT, conf_ptr};

        /// `conf_ptr`, `ensures p == conf_storage()`, uninterpreted: every
        /// call returns the same pointer. Beyond the contract, it is
        /// `CONF_BUF`'s storage (`core::ptr::eq`), non-null, writable for
        /// the whole array (`kani::mem::can_write`), and a byte written at
        /// any index reads back there.
        #[kani::proof]
        fn conf_ptr_is_conf_buf() {
            let p = conf_ptr();
            assert!(conf_ptr() == p);
            assert!(core::ptr::eq(p, core::ptr::addr_of_mut!(CONF_BUF)));
            assert!(!p.is_null());
            assert!(kani::mem::can_write(p));
            let (i, v): (usize, u8) = (kani::any(), kani::any());
            kani::assume(i <= MAX_TEXT);
            // SAFETY: `CONF_BUF`'s storage, writable above; no other code
            // runs in a harness.
            unsafe { (*p)[i] = v };
            // SAFETY: as above.
            assert_eq!(unsafe { (*p)[i] }, v);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;

/// Kani harnesses for this file's seams that Kani can run
/// (`verus/ledger/src/config.rs.md`): the `Display` of `Issue` and
/// `Bytes` that `parse_from` prints each problem with, run under the real
/// `core::fmt` (Verus checks it against the `Formatter` specifications of
/// `vspec/fmt.rs`).
#[cfg(kani)]
mod kani_config {
    use super::*;
    use crate::kani_proofs::fmt::formats;

    /// `Issue`'s `Display`, which `parse_from` prints each problem with
    /// (its `fmt_req` is `true`), returns without a panic
    /// for the issue `make` builds from arbitrary byte strings up to 3
    /// bytes. Each `display_issue_*` harness names one variant in its
    /// closure, so that CBMC runs only that arm of `fmt`.
    fn issue_formats<F: for<'a> Fn(&'a [u8], &'a [u8]) -> Issue<'a>>(make: F) {
        let a: [u8; 3] = kani::any();
        let b: [u8; 3] = kani::any();
        let (i, j): (usize, usize) = (kani::any(), kani::any());
        kani::assume(i <= 3 && j <= 3);
        formats(&make(&a[..i], &b[..j]));
    }

    /// [`issue_formats`] of `Malformed`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_malformed() {
        issue_formats(|_, v| Issue::Malformed(v));
    }

    /// [`issue_formats`] of `UnknownKey`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_unknown_key() {
        issue_formats(|k, _| Issue::UnknownKey(k));
    }

    /// [`issue_formats`] of `BadValue`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_bad_value() {
        issue_formats(|key, value| Issue::BadValue { key, value });
    }

    /// [`issue_formats`] of `OutOfRange`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_out_of_range() {
        issue_formats(|key, value| Issue::OutOfRange { key, value });
    }

    /// [`issue_formats`] of `Rounded`, for every `to`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_rounded() {
        let to: u64 = kani::any();
        issue_formats(|key, value| Issue::Rounded { key, value, to });
    }

    /// [`issue_formats`] of `Cut`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_cut() {
        issue_formats(|_, _| Issue::Cut);
    }

    /// `Bytes`' `Display`, which `Issue`'s calls: it returns without a
    /// panic for every byte string up to 4 bytes, and for one of 65
    /// arbitrary bytes (past the 64 it shows, so the `...` branch runs).
    #[kani::proof]
    #[kani::unwind(67)]
    fn display_bytes() {
        let a: [u8; 4] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 4);
        formats(&Bytes(&a[..n]));
        let long: [u8; 65] = kani::any();
        formats(&Bytes(&long));
    }
}
