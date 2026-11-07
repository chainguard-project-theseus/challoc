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
//! Three tables name the keys. [`TABLE`] holds the tunables the allocator
//! reads, jemalloc's and challoc's own (`lazy_band`, `guards`,
//! ...): one [`Key`] per tunable with its name,
//! its parser and its reader, so [`Config::set`], [`Config::get`]
//! (`opt.<key>`) and the name list [`KEYS`] all come from the one entry;
//! the modules named in each field's doc act on the values.
//! [`IGNORED_KEYS`] are jemalloc keys accepted with any value and ignored,
//! so a conf string written against jemalloc parses clean. The *inert*
//! key ([`INERT_KEYS`]: `stats_print`) is parsed as a bool but nothing in
//! the allocator reads it (there is nothing to print at exit); it is
//! reported as [`Issue::Inert`] so that `abort_conf` rejects it and a
//! program cannot believe it turned something on, and
//! `malloc_stats_print` lists it as "accepted, no effect".

use core::ffi::CStr;
use core::fmt::{self, Write as _};

use crate::heap::{MAX_CAP, MIN_CAP};
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

} // verus!

/// Largest `huge_retain_max`: the preferred huge region (2^40 bytes,
/// `layout::LG_HUGE_PREFERRED`); no run can be larger, so anything above
/// means "retain every freed run for decay".
pub const MAX_HUGE_RETAIN_MAX: usize = 1 << crate::layout::LG_HUGE_PREFERRED;

verus! {

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
    /// `MADV_FREE` for purges under 70% of a known cgroup limit (`purge`).
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
        lazy_band: true,
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
    /// ([`TABLE`]), an inert key ([`INERT_KEYS`]: validated as a bool, then
    /// [`Issue::Inert`]), an ignored key ([`IGNORED_KEYS`]: any value), else
    /// [`Issue::UnknownKey`]. An out-of-range number is clamped *and*
    /// returned as [`Issue::OutOfRange`].
    pub fn set<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
    {
        if let Some(r) = self.set_listed(key, value) {
            return r;
        }
        if contains(INERT_KEYS, key) {
            // jemalloc's `stats_print` (report at exit): no field, since
            // there is nothing to print at exit; validated as a bool and
            // reported as inert so `abort_conf` does not let a program
            // expect the report.
            parse_bool(value).ok_or(Issue::BadValue { key, value })?;
            return Err(Issue::Inert(key));
        }
        if contains(IGNORED_KEYS, key) {
            // jemalloc keys accepted for compatibility and ignored: their
            // values are not validated because we do nothing with them.
            return Ok(());
        }
        Err(Issue::UnknownKey(key))
    }

    /// The [`TABLE`] entry's parser for `key`, `None` when no entry is
    /// named `key`. Trusted: the table holds function pointers. The three
    /// fields of [`Config::wf`] are written only by their own parsers,
    /// through `num` with the `wf` range as its bounds; the guard slab
    /// interval's then lifts 1 to 2 and rounds a value that is not a power
    /// of two up to the next, which stays within the bound (itself a power
    /// of two). The config must be well formed: on a value that does not
    /// parse the interval keeps its old value, which that rounding would
    /// overflow past `2^31`.
    #[verifier::external_body]
    #[inline(always)]
    fn set_listed<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Option<Result<(), Issue<'a>>>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
    {
        Key::find(key).map(|k| (k.parse.0)(self, key, value))
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`: the
    /// [`TABLE`] entry's reader. `None` for keys that are only accepted
    /// and ignored, for inert keys and for unknown keys. Trusted: the
    /// table holds function pointers; nothing is claimed about the value.
    #[verifier::external_body]
    pub fn get(&self, key: &[u8]) -> Option<Value> {
        Key::find(key).map(|k| (k.read.0)(self))
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

/// A key's parser: `(config, key, value)`; the key is passed so the issue
/// can name it. A newtype, so the verifier, which rejects function
/// pointer types, can take it as an opaque type.
// The newtype is the opaque type the verifier needs (above).
#[allow(clippy::type_complexity)]
struct Parse(for<'a> fn(&mut Config, &'a [u8], &'a [u8]) -> Result<(), Issue<'a>>);

/// A key's reader, the value [`Config::get`] answers with; opaque, as
/// [`Parse`].
struct Read(fn(&Config) -> Value);

verus! {

#[verifier::external_type_specification]
#[verifier::external_body]
// Never constructed: it only names `Parse` to the verifier.
#[allow(dead_code)]
struct ExParse(Parse);

#[verifier::external_type_specification]
#[verifier::external_body]
// Never constructed: it only names `Read` to the verifier.
#[allow(dead_code)]
struct ExRead(Read);

/// One tunable of design 12: its name (`opt.<name>`), the parser
/// [`Config::set`] runs on its value and the reader [`Config::get`]
/// answers with. The entries are [`TABLE`]; [`KEYS`] is their names.
pub struct Key {
    pub name: &'static str,
    parse: Parse,
    read: Read,
}

impl Key {
    /// The entry named `key`, if any.
    fn find(key: &[u8]) -> (r: Option<&'static Key>)
        ensures
            r matches Some(k) ==> k.name.spec_bytes() == key@,
    {
        // `TABLE.iter().find(|k| k.name.as_bytes() == key)`, with an
        // index: Verus does not specify `Iterator::find`.
        let table = TABLE;
        let mut i = 0;
        while i < table.len()
            invariant
                i <= table@.len(),
            decreases table@.len() - i,
        {
            if table[i].name.as_bytes() == key {
                proof {
                    assert(table@[i as int].name.spec_bytes() =~= key@);
                }
                return Some(&table[i]);
            }
            i += 1;
        }
        None
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

/// A numeric key: parse, clamp into `[min, max]`, assign through `store`,
/// then report a clamp as [`Issue::OutOfRange`].
fn num<'a>(
    cfg: &mut Config,
    key: &'a [u8],
    value: &'a [u8],
    min: i64,
    max: i64,
    store: impl FnOnce(&mut Config, i64),
) -> (r: Result<(), Issue<'a>>)
    requires
        min <= max,
        forall|c: &mut Config, v: i64| min <= v <= max ==> call_requires(store, (c, v)),
    ensures
        parse_int_spec(value@) is None ==> r == Err::<(), Issue<'a>>(Issue::BadValue { key, value }),
        parse_int_spec(value@) matches Some(v) ==> (r is Ok <==> min <= v <= max),
        r is Err && parse_int_spec(value@) is Some ==> r == Err::<(), Issue<'a>>(Issue::OutOfRange { key, value }),
{
    let (v, clamped) = clamped(key, value, min, max)?;
    store(cfg, v);
    if clamped {
        Err(Issue::OutOfRange { key, value })
    } else {
        Ok(())
    }
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

} // verus!

/// A [`TABLE`] entry for a bool key stored in `field`.
macro_rules! bool_key {
    ($name:literal, $field:ident) => {
        Key {
            name: $name,
            parse: Parse(|c, k, v| bool_into(&mut c.$field, k, v)),
            read: Read(|c| Value::Bool(c.$field)),
        }
    };
}

/// A [`TABLE`] entry for a numeric key: `field` of type `$t`, parsed by
/// [`num`] in `[$min, $max]` and read back as `Value::$kind` of `$u`.
macro_rules! num_key {
    ($name:literal, $field:ident: $t:ty, [$min:expr, $max:expr] => $kind:ident($u:ty)) => {
        Key {
            name: $name,
            parse: Parse(|c, k, v| num(c, k, v, $min, $max, |c, v| c.$field = v as $t)),
            read: Read(|c| Value::$kind(c.$field as $u)),
        }
    };
}

/// Every tunable, in table order (the order of the `opt.` namespace, the
/// report and `KEYS`). A new key is one entry here plus its field; the
/// tests check that every entry reads back, round-trips through `set`
/// and is in no other table. Kept apart from [`TABLE`], so the verifier
/// can take the table, whose entries hold closures, as one trusted
/// constant.
const ENTRIES: &[Key] = &[
    Key {
        // Unset it starts lazily (`purge::background::configure`) and
        // reads back `false`, jemalloc's default: no thread was asked for.
        name: "background_thread",
        parse: Parse(|c, k, v| {
            c.background_thread = Some(parse_bool(v).ok_or(Issue::BadValue { key: k, value: v })?);
            Ok(())
        }),
        read: Read(|c| Value::Bool(c.background_thread == Some(true))),
    },
    num_key!("dirty_decay_ms", dirty_decay_ms: i64, [-1, MAX_INTERVAL_MS] => Int(i64)),
    num_key!("muzzy_decay_ms", muzzy_decay_ms: i64, [-1, MAX_INTERVAL_MS] => Int(i64)),
    num_key!("narenas", narenas: u32, [0, u32::MAX as i64] => Uint(u64)),
    Key {
        name: "percpu_arena",
        parse: Parse(|c, k, v| {
            c.percpu_arena = pick(
                &[
                    ("disabled", PercpuArena::Disabled),
                    ("percpu", PercpuArena::Percpu),
                    ("phycpu", PercpuArena::Phycpu),
                ],
                k,
                v,
            )?;
            Ok(())
        }),
        read: Read(|c| Value::Str(Name::PercpuArena(c.percpu_arena))),
    },
    bool_key!("tcache", tcache),
    num_key!("tcache_max", tcache_max: usize, [0, crate::layout::MEDIUM_MAX as i64] => Size(usize)),
    num_key!("tcache_nslots_small_max", tcache_nslots_small_max: u32,
        [MIN_CAP as i64, MAX_NSLOTS as i64] => Uint(u64)),
    num_key!("lg_tcache_nslots_mul", lg_tcache_nslots_mul: i32,
        [LG_NSLOTS_MUL_MIN as i64, LG_NSLOTS_MUL_MAX as i64] => Int(i64)),
    Key {
        // `default` is jemalloc's spelling of `never`.
        name: "thp",
        parse: Parse(|c, k, v| {
            c.thp = pick(
                &[
                    ("never", Thp::Never),
                    ("default", Thp::Never),
                    ("auto", Thp::Auto),
                    ("always", Thp::Always),
                ],
                k,
                v,
            )?;
            Ok(())
        }),
        read: Read(|c| Value::Str(Name::Thp(c.thp))),
    },
    bool_key!("lazy_band", lazy_band),
    bool_key!("guards", guards),
    Key {
        name: "guard_slab_interval",
        parse: Parse(|c, k, v| {
            // 0 is off and N >= 2 is one guard per N x 64 KiB; 1 would
            // make every slab a guard and leave nothing to allocate
            // (`SlabTable::new` refuses it), so it is out of range and
            // clamps to the smallest interval with any slabs, 2. The
            // interval must be a power of two (the sized fast path's
            // guard test is a mask): anything else rounds up and is
            // reported, so `abort_conf` refuses it and a plain run says
            // what it got. The maximum is a power of two, so a clamped
            // value needs no rounding.
            let r = num(c, k, v, 0, MAX_GUARD_SLAB_INTERVAL as i64, |c, v| {
                c.guard_slab_interval = v as u32
            });
            if c.guard_slab_interval == 1 {
                c.guard_slab_interval = 2;
                return Err(Issue::OutOfRange { key: k, value: v });
            }
            if c.guard_slab_interval != 0 && !c.guard_slab_interval.is_power_of_two() {
                c.guard_slab_interval = c.guard_slab_interval.next_power_of_two();
                return Err(Issue::Rounded {
                    key: k,
                    value: v,
                    to: c.guard_slab_interval as u64,
                });
            }
            r
        }),
        read: Read(|c| Value::Uint(c.guard_slab_interval as u64)),
    },
    num_key!("huge_retain_max", huge_retain_max: usize,
        [0, MAX_HUGE_RETAIN_MAX as i64] => Size(usize)),
    Key {
        // Unset it follows `dirty_decay_ms`, which is what it reads back.
        name: "huge_dirty_decay_ms",
        parse: Parse(|c, k, v| {
            num(c, k, v, -1, MAX_INTERVAL_MS, |c, v| {
                c.huge_dirty_decay_ms = Some(v)
            })
        }),
        read: Read(|c| Value::Int(c.huge_decay_ms())),
    },
    bool_key!("zero_on_free", zero_on_free),
    bool_key!("pac_pointers", pac_pointers),
    bool_key!("slot_randomize", slot_randomize),
    bool_key!("double_free_scan", double_free_scan),
    bool_key!("exact_sized_free", exact_sized_free),
    bool_key!("abort_conf", abort_conf),
    bool_key!("cgroup", cgroup),
    bool_key!("check_zero", check_zero),
];

verus! {

/// [`ENTRIES`], the tunables. Trusted: the verifier cannot see the
/// entries (they hold closures); nothing is claimed about them.
// The lifetimes are spelled out: `verus!` builds an `exec const` as a
// function in the verifier's view, where `&[Key]` would have none.
#[allow(clippy::redundant_static_lifetimes)]
#[verifier::external_body]
pub exec const TABLE: &'static [Key] = ENTRIES;

/// The names of [`TABLE`], in its order. The loop also tests `out`'s
/// length, which is `TABLE.len()` by its type: the verifier does not
/// evaluate the array length.
const fn names() -> [&'static str; TABLE.len()] {
    let table = TABLE;
    let mut out = [""; TABLE.len()];
    let mut i = 0;
    while i < table.len() && i < out.len()
        invariant
            i <= table@.len(),
        decreases table@.len() - i,
    {
        out[i] = table[i].name;
        i += 1;
    }
    out
}

} // verus!

const NAMES: [&str; TABLE.len()] = names();

verus! {

/// Every key readable through [`Config::get`], in table order, for the
/// `opt.` namespace (`mallctl` builds its `opt.` nodes from this slice at
/// compile time, by position) and the tests: the names of [`TABLE`].
/// Trusted only as a value of its type: `NAMES` is built from `TABLE`,
/// which holds function pointers; nothing is claimed about the names.
// The lifetimes are spelled out: `verus!` builds an `exec const` as a
// function in the verifier's view, where `&[&str]` would have none.
#[allow(clippy::redundant_static_lifetimes)]
#[verifier::external_body]
pub exec const KEYS: &'static [&'static str] = &NAMES;

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

/// Keys that parse (as bools) but that no code acts on and no field
/// stores: `stats_print` has nothing to print at exit. Setting one is
/// [`Issue::Inert`], which `abort_conf` turns into an abort;
/// `malloc_stats_print` lists them as "accepted, no effect".
#[allow(clippy::redundant_static_lifetimes)]
pub exec const INERT_KEYS: &'static [&'static str] = &["stats_print"];

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
    /// One of [`INERT_KEYS`]: parsed, but no field stores it and nothing
    /// reads it.
    Inert(&'a [u8]),
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
            Issue::Inert(key) => {
                write!(
                    f,
                    "conf key {} is accepted but has no effect (not implemented)",
                    Bytes(key)
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
            assert((b as char) as u32 == b as u32);
            lemma_digits_prefix(digits@, i + 1, radix as u32);
        }
        let d = (b as char).to_digit(radix as u32)? as u64;
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
        Some(magnitude.wrapping_neg())
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

pub use linux::load;
#[cfg(test)]
use linux::{MAX_TEXT, cstr, env_var, first_line_of, malloc_conf_symbol};

mod linux {
    use core::ffi::{c_char, c_void};
    use core::fmt::Write as _;

    use super::{Config, Issue, Report, position_of};
    use crate::sys;
    use vstd::prelude::*;

    verus! {

    /// Longest conf text read from any one source. Longer inputs are cut
    /// here and the cut is reported ([`Config::parse_source`]); no real
    /// configuration comes close.
    pub const MAX_TEXT: usize = 4096;

    } // verus!

    /// [`conf_buffer`]'s storage (Verus has no `static mut`).
    #[cfg(not(test))]
    static mut CONF_BUF: [u8; MAX_TEXT + 1] = [0; MAX_TEXT + 1];

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

    /// The buffer [`load`] reads `/etc/challoc.conf` into: a static, so
    /// init's frame does not carry 4 KiB of it (design 13.8: init's frame
    /// is capped, though challoc initialises before `main`, so init does
    /// not normally run on a program's small thread). Trusted: `load` is called only by
    /// `api::init`, which runs once (its exchange on `STATE` lets one
    /// thread through, and a re-entry aborts before it allocates), so no
    /// two references are live at once. The unit tests call `load`
    /// concurrently, so each call there gets its own buffer.
    #[verifier::external_body]
    fn conf_buffer() -> (r: &'static mut [u8; MAX_TEXT + 1]) {
        #[cfg(not(test))]
        {
            // SAFETY: the exclusion above.
            unsafe { &mut *core::ptr::addr_of_mut!(CONF_BUF) }
        }
        #[cfg(test)]
        {
            std::boxed::Box::leak(std::boxed::Box::new([0u8; MAX_TEXT + 1]))
        }
    }

    /// Read every source in priority order into a `Config`, printing each
    /// problem as `challoc: <source>: <issue>` on stderr. Aborts when
    /// `abort_conf` is set by the time all sources are read and anything was
    /// reported, an inert key or a cut source included. Never allocates: the
    /// file goes through a static buffer ([`conf_buffer`]) and the
    /// environment is walked in place. The result is well formed: the defaults are
    /// (`Config::lemma_default_wf`) and every source goes through
    /// `Config::parse_source`, which keeps it.
    pub fn load() -> (r: Config)
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
        let mut issues = 0;
        let mut out = sys::Stderr::new();
        // `for (name, text) in [...]` over the two symbols and the two
        // variables, spelled out: Verus does not specify an array's
        // `IntoIter`. Both symbols are read before either is parsed.
        let (sym, je_sym) = (malloc_conf_symbol(0), malloc_conf_symbol(1));
        if let Some((text, cut)) = sym {
            issues += parse_from(&mut cfg, text, cut, "malloc_conf", &mut out);
        }
        if let Some((text, cut)) = je_sym {
            issues += parse_from(&mut cfg, text, cut, "je_malloc_conf", &mut out);
        }
        let buf = conf_buffer();
        if let Some((line, cut)) = first_line_of(&CHALLOC_CONF, buf) {
            issues += parse_from(&mut cfg, line, cut, "/etc/challoc.conf", &mut out);
        }
        if getauxval(AT_SECURE) == 0 {
            if let Some((text, cut)) = env_var(b"MALLOC_CONF") {
                issues += parse_from(&mut cfg, text, cut, "MALLOC_CONF", &mut out);
            }
            if let Some((text, cut)) = env_var(b"CHALLOC_CONF") {
                issues += parse_from(&mut cfg, text, cut, "CHALLOC_CONF", &mut out);
            }
        }
        out.flush();
        if cfg.abort_conf && issues > 0 {
            sys::abort_fmt(format_args!(
                "abort_conf: {issues} configuration problem(s), see above"
            ));
        }
        cfg
    }

    /// Secure execution ignores the environment, closing the shape of
    /// CVE-2023-4911 (glibc's `GLIBC_TUNABLES`, malloc tunables among them,
    /// parsed by a setuid program's loader): in a process started with
    /// `AT_SECURE` set (`sys::auxv::secure_exec`: setuid, setgid, file
    /// capabilities or an LSM transition), no call of [`env_var`] returns,
    /// whatever its name and result: Verus rejects every verified caller
    /// that has not established `!secure_exec()`. So [`load`], whose two
    /// calls meet `env_var`'s precondition only through its
    /// `getauxval(AT_SECURE) == 0` test, reads neither `MALLOC_CONF` nor
    /// `CHALLOC_CONF`, and its other sources (the defaults, the
    /// `malloc_conf` and `je_malloc_conf` symbols, the first line of
    /// `/etc/challoc.conf`) do not read the environment: the configuration
    /// is the one the same process would get with neither variable set.
    /// That last step is not a Verus fact: `Config::parse_source` is
    /// verified to keep `Config::wf`, not as a function of its text.
    ///
    /// Trusted: `getauxval(AT_SECURE)` returns the process's one
    /// `AT_SECURE` word (`sys::auxv::at_secure`, in the `getauxval` seam);
    /// `env_var`'s body is verified against its contract. Checked by a
    /// test, not by Verus: `env_var` is the only reader of `environ`
    /// (through [`environ_array`]), and outside the tests `load` is its
    /// only caller (`config::linux_tests::environment_has_one_reader`).
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
    /// the value.
    ///
    /// # Safety
    /// `p + n` is a readable byte of the caller's string.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn byte_at(p: *const u8, n: usize) -> u8 {
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

    /// The value of environment variable `name` (no `=` in it), read from
    /// `environ` without `getenv`, and whether it was cut, as [`cstr`]
    /// returns it for the value alone. `None` when unset or empty.
    ///
    /// Each entry is read through `cstr`, and when it sets `name`
    /// ([`env_sets`]) its value is read through `cstr` again, which stops at
    /// [`MAX_TEXT`] bytes. Termination is assumed: the walk ends at the null
    /// pointer that ends `environ`, a fact about the C program.
    ///
    /// Requires a process that is not a secure-execution one
    /// (`sys::auxv::secure_exec`), whose environment is untrusted, so Verus
    /// checks that every verified caller has tested `AT_SECURE` first. The
    /// ensures restates it, so that [`theorem_secure_conf`] can state it of
    /// `env_var` itself: Verus gives a function's `call_ensures` its
    /// postcondition, but not its `call_requires` its precondition.
    // Verus has no let-chains, so the `env_sets` test stays nested.
    #[allow(clippy::collapsible_if)]
    #[verifier::exec_allows_no_decreases_clause]
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
        loop
            invariant
                !crate::sys::auxv::secure_exec(),
        {
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

    /// The Kani harness of [`conf_buffer`], next to the private function
    /// and its static.
    #[cfg(kani)]
    mod kani_conf_buffer {
        use super::{CONF_BUF, MAX_TEXT, conf_buffer};

        /// `conf_buffer`: no `ensures`; the call is defined, and the
        /// `&'static mut [u8; MAX_TEXT + 1]` it returns is `CONF_BUF`
        /// (`core::ptr::eq`), writable (`kani::mem::can_write`), and a
        /// byte written at any index reads back there. Not checked: that
        /// no two references are live at once, which rests on `load`'s
        /// caller, `api::init`, running once.
        #[kani::proof]
        fn conf_buffer_is_conf_buf() {
            let r = conf_buffer();
            let p: *mut [u8; MAX_TEXT + 1] = r;
            assert!(core::ptr::eq(p, core::ptr::addr_of_mut!(CONF_BUF)));
            assert!(kani::mem::can_write(p));
            let (i, v): (usize, u8) = (kani::any(), kani::any());
            kani::assume(i <= MAX_TEXT);
            // SAFETY: `p` is `r`, the only reference, writable above.
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
/// (`verus/ledger/src/config.rs.md`): `Config::set_listed`, `Config::get`
/// and `KEYS`; and the `Display` of `Issue` and `Bytes` that `parse_from`
/// prints each problem with, run under the real `core::fmt` (Verus checks
/// it against the `Formatter` specifications of `vspec/fmt.rs`).
#[cfg(kani)]
mod kani_config {
    use super::*;
    use crate::kani_proofs::fmt::formats;

    /// `listed(key)`: some entry of `TABLE` is named `key`.
    fn listed(key: &[u8]) -> bool {
        let mut i = 0;
        while i < TABLE.len() {
            if TABLE[i].name.as_bytes() == key {
                return true;
            }
            i += 1;
        }
        false
    }

    /// `Config::wf`, `is_pow2` read as `is_power_of_two` (checked against
    /// the spec in `kani_proofs::core_int::usize_is_power_of_two`).
    fn wf(c: &Config) -> bool {
        MIN_CAP <= c.tcache_nslots_small_max
            && c.tcache_nslots_small_max <= MAX_NSLOTS
            && LG_NSLOTS_MUL_MIN <= c.lg_tcache_nslots_mul
            && c.lg_tcache_nslots_mul <= LG_NSLOTS_MUL_MAX
            && (c.guard_slab_interval == 0
                || (2 <= c.guard_slab_interval
                    && c.guard_slab_interval <= MAX_GUARD_SLAB_INTERVAL
                    && (c.guard_slab_interval as usize).is_power_of_two()))
    }

    /// A `Config` with every field arbitrary.
    fn any_config() -> Config {
        Config {
            background_thread: kani::any(),
            dirty_decay_ms: kani::any(),
            muzzy_decay_ms: kani::any(),
            narenas: kani::any(),
            percpu_arena: match kani::any::<u8>() % 3 {
                0 => PercpuArena::Disabled,
                1 => PercpuArena::Percpu,
                _ => PercpuArena::Phycpu,
            },
            tcache: kani::any(),
            tcache_max: kani::any(),
            tcache_nslots_small_max: kani::any(),
            lg_tcache_nslots_mul: kani::any(),
            thp: match kani::any::<u8>() % 3 {
                0 => Thp::Never,
                1 => Thp::Auto,
                _ => Thp::Always,
            },
            lazy_band: kani::any(),
            guards: kani::any(),
            guard_slab_interval: kani::any(),
            huge_retain_max: kani::any(),
            huge_dirty_decay_ms: if kani::any() { Some(kani::any()) } else { None },
            zero_on_free: kani::any(),
            pac_pointers: kani::any(),
            slot_randomize: kani::any(),
            double_free_scan: kani::any(),
            exact_sized_free: kani::any(),
            abort_conf: kani::any(),
            cgroup: kani::any(),
            check_zero: kani::any(),
        }
    }

    /// The longest value checked: 8 bytes reach past every bound a
    /// `Config::wf` field is clamped to (`0x` and six hex digits is
    /// 16 Mi, above `MAX_GUARD_SLAB_INTERVAL`).
    const VALUE_MAX: usize = 8;
    /// The longest arbitrary key checked.
    const KEY_MAX: usize = 4;

    /// The name of an arbitrary `TABLE` entry, or arbitrary bytes up to
    /// `KEY_MAX` long (mostly unlisted; `thp` is one of them).
    fn any_key(buf: &[u8; KEY_MAX]) -> &[u8] {
        if kani::any() {
            let i: usize = kani::any();
            kani::assume(i < TABLE.len());
            TABLE[i].name.as_bytes()
        } else {
            let n: usize = kani::any();
            kani::assume(n <= KEY_MAX);
            &buf[..n]
        }
    }

    /// `Config::set_listed`, `requires old(self).wf()`, `ensures
    /// final(self).wf()`: under it the call returns and keeps `wf`, for
    /// every well-formed config, every entry's name and every key up to
    /// `KEY_MAX` bytes, and every value up to `VALUE_MAX` bytes. Also `None`
    /// when no entry is named `key`, as its comment says, and beyond the
    /// comment, `Some` for every listed key and the config unchanged on
    /// `None`.
    #[kani::proof]
    #[kani::unwind(25)]
    #[kani::solver(kissat)]
    fn set_listed_contract() {
        let kbuf: [u8; KEY_MAX] = kani::any();
        let key = any_key(&kbuf);
        let vbuf: [u8; VALUE_MAX] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= VALUE_MAX);
        let value = &vbuf[..n];
        let old = any_config();
        kani::assume(wf(&old));
        let mut c = old;
        let r = c.set_listed(key, value);
        assert_eq!(r.is_none(), !listed(key));
        if r.is_none() {
            assert!(c == old);
        }
        assert!(wf(&c));
    }

    /// `Config::get`, no `requires`, no `ensures`: the call returns for
    /// every config and every key `any_key` gives, and (its comment) is
    /// `Some` exactly when an entry is named `key`.
    #[kani::proof]
    #[kani::unwind(25)]
    fn get_contract() {
        let kbuf: [u8; KEY_MAX] = kani::any();
        let key = any_key(&kbuf);
        let c = any_config();
        assert_eq!(c.get(key).is_some(), listed(key));
    }

    /// `KEYS`, trusted as a value of its type: it is also what its
    /// comment says, the names of `TABLE` in table order.
    #[kani::proof]
    #[kani::unwind(25)]
    fn keys_are_the_table_names() {
        assert_eq!(KEYS.len(), TABLE.len());
        let mut i = 0;
        while i < KEYS.len() {
            assert!(KEYS[i] == TABLE[i].name);
            i += 1;
        }
    }

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

    /// [`issue_formats`] of `Inert`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue_inert() {
        issue_formats(|k, _| Issue::Inert(k));
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
