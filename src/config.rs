//! `malloc_conf` parsing (design.md section 12).
//!
//! The parser is pure logic over byte slices so it can be unit tested
//! against fixtures; the source readers (weak `malloc_conf` symbol,
//! `/etc/challoc.conf`, environment) never allocate: the file is read into
//! a stack buffer with raw syscalls and the environment is walked through
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
use crate::sys::bytes_is;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;
#[cfg(verus_only)]
use vstd::string::*;

verus! {

/// Largest accepted `tcache_nslots_small_max`. A cache array is allocated
/// from the metadata region as one object, and `Meta::alloc` serves layouts
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
/// verifier's view (Verus takes no `CStr` literal); nothing is claimed
/// about them.
#[verifier::external_body]
const THP_NAMES: [&'static CStr; 3] = [c"never", c"auto", c"always"];

/// The names [`PercpuArena::name`] gives, in variant order, as
/// [`THP_NAMES`].
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

/// Every tunable of design section 12; [`Config::DEFAULT`] holds the
/// compiled defaults. `docs/tuning.md` section 2 documents each key: its
/// values, what it does, when to change it and what that costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Run decay on a background thread (`purge::background`).
    pub background_thread: bool,
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

    pub proof fn lemma_default_wf()
        ensures
            Config::DEFAULT.wf(),
    {
        vstd::arithmetic::power2::lemma2_to64();
        assert(8usize as nat == vstd::arithmetic::power2::pow2(3));
        assert(crate::meta::is_pow2(8usize));
        assert(MAX_GUARD_SLAB_INTERVAL == 0x40_0000) by (compute);
    }

    /// The compiled defaults (design section 12).
    pub const DEFAULT: Config = Config {
        background_thread: false,
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
        abort_conf: false,
        cgroup: true,
        check_zero: false,
    };

    /// Apply one `key:value,key:value` string on top of `self`. Each problem
    /// is passed to `report`; the corresponding pair is skipped (or clamped,
    /// for [`Issue::OutOfRange`]) and parsing continues with the next pair.
    /// Returns the number of problems reported.
    pub fn parse(&mut self, text: &[u8], mut report: impl FnMut(Issue<'_>)) -> (issues: usize)
        requires
            old(self).wf(),
            forall|i: Issue| call_requires(report, (i,)),
        ensures
            issues <= text.len(),
            old(self).wf() ==> final(self).wf(),
    {
        let mut issues = 0;
        let mut pairs = Pairs(text);
        // `for pair in Pairs(text)`, spelled out: Verus has no
        // specification for a hand-written `Iterator`.
        while let Some(pair) = pairs.next_pair()
            invariant
                pairs.0@.len() <= text.len(),
                issues <= text.len() - pairs.0@.len(),
                forall|i: Issue| call_requires(report, (i,)),
                self.wf(),
            decreases pairs.0@.len(),
        {
            if let Err(issue) = self.apply_pair(pair) {
                issues += 1;
                report(issue);
            }
        }
        issues
    }

    /// Split one pair and set the key. Empty pairs (from `a,,b` or a
    /// trailing comma) are ignored, as jemalloc ignores them.
    fn apply_pair<'a>(&mut self, pair: &'a [u8]) -> (r: Result<(), Issue<'a>>)
        requires
            old(self).wf(),
        ensures
            pair@.len() == 0 ==> r is Ok && *final(self) == *old(self),
            pair@.len() > 0 && (forall|j: int| 0 <= j < pair@.len() ==> pair@[j] != b':')
                ==> r == Err::<(), Issue<'a>>(Issue::Malformed(pair)),
            pair@.len() > 0 && pair@[0] == b':' ==> r == Err::<(), Issue<'a>>(Issue::Malformed(pair)),
            old(self).wf() ==> final(self).wf(),
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
        let value = unquote(value);
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
            !listed(key@) ==> *final(self) == *old(self),
            old(self).wf() ==> final(self).wf(),
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
    /// overflow past `2^31` (Kani found it; `kani_config` below).
    #[verifier::external_body]
    #[inline(always)]
    fn set_listed<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> (r: Option<Result<(), Issue<'a>>>)
        requires
            old(self).wf(),
        ensures
            r is None <==> !listed(key@),
            r is None ==> *final(self) == *old(self),
            old(self).wf() ==> final(self).wf(),
    {
        Key::find(key).map(|k| (k.parse)(self, key, value))
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`: the
    /// [`TABLE`] entry's reader. `None` for keys that are only accepted
    /// and ignored, for inert keys and for unknown keys. Trusted: the
    /// table holds function pointers, as for `set_listed`.
    #[verifier::external_body]
    pub fn get(&self, key: &[u8]) -> (r: Option<Value>)
        ensures
            r is Some <==> listed(key@),
    {
        Key::find(key).map(|k| (k.read)(self))
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

/// Cap of the automatic `narenas` default (design section 13.3).
pub const MAX_AUTO_ARENAS: u32 = 64;

/// Whether `key` names a [`TABLE`] entry (the table holds function
/// pointers, so it stays outside the verified code).
pub uninterp spec fn listed(key: Seq<u8>) -> bool;

} // verus!

/// A key's parser: `(config, key, value)`; the key is passed so the issue
/// can name it.
type Parse = for<'a> fn(&mut Config, &'a [u8], &'a [u8]) -> Result<(), Issue<'a>>;

/// One tunable of design section 12: its name (`opt.<name>`), the parser
/// [`Config::set`] runs on its value and the reader [`Config::get`]
/// answers with. The entries are [`TABLE`]; [`KEYS`] is their names.
pub struct Key {
    pub name: &'static str,
    parse: Parse,
    read: fn(&Config) -> Value,
}

impl Key {
    /// The entry named `key`, if any.
    fn find(key: &[u8]) -> Option<&'static Key> {
        TABLE.iter().find(|k| k.name.as_bytes() == key)
    }
}

verus! {

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
        if bytes_is(names[i].0.as_bytes(), value) {
            proof {
                assert(names@[i as int].0.spec_bytes() == value@);
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
            parse: |c, k, v| bool_into(&mut c.$field, k, v),
            read: |c| Value::Bool(c.$field),
        }
    };
}

/// Every tunable, in table order (the order of the `opt.` namespace, the
/// report and `KEYS`). A new key is one entry here plus its field; the
/// tests check that every entry reads back, round-trips through `set`
/// and is in no other table.
pub const TABLE: &[Key] = &[
    bool_key!("background_thread", background_thread),
    Key {
        name: "dirty_decay_ms",
        parse: |c, k, v| num(c, k, v, -1, MAX_INTERVAL_MS, |c, v| c.dirty_decay_ms = v),
        read: |c| Value::Int(c.dirty_decay_ms),
    },
    Key {
        name: "muzzy_decay_ms",
        parse: |c, k, v| num(c, k, v, -1, MAX_INTERVAL_MS, |c, v| c.muzzy_decay_ms = v),
        read: |c| Value::Int(c.muzzy_decay_ms),
    },
    Key {
        name: "narenas",
        parse: |c, k, v| num(c, k, v, 0, u32::MAX as i64, |c, v| c.narenas = v as u32),
        read: |c| Value::Uint(c.narenas as u64),
    },
    Key {
        name: "percpu_arena",
        parse: |c, k, v| {
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
        },
        read: |c| Value::Str(c.percpu_arena.name()),
    },
    bool_key!("tcache", tcache),
    Key {
        name: "tcache_max",
        parse: |c, k, v| {
            num(c, k, v, 0, crate::layout::MEDIUM_MAX as i64, |c, v| {
                c.tcache_max = v as usize
            })
        },
        read: |c| Value::Size(c.tcache_max),
    },
    Key {
        name: "tcache_nslots_small_max",
        parse: |c, k, v| {
            num(c, k, v, MIN_CAP as i64, MAX_NSLOTS as i64, |c, v| {
                c.tcache_nslots_small_max = v as u32
            })
        },
        read: |c| Value::Uint(c.tcache_nslots_small_max as u64),
    },
    Key {
        name: "lg_tcache_nslots_mul",
        parse: |c, k, v| {
            num(
                c,
                k,
                v,
                LG_NSLOTS_MUL_MIN as i64,
                LG_NSLOTS_MUL_MAX as i64,
                |c, v| c.lg_tcache_nslots_mul = v as i32,
            )
        },
        read: |c| Value::Int(c.lg_tcache_nslots_mul as i64),
    },
    Key {
        // `default` is jemalloc's spelling of `never`.
        name: "thp",
        parse: |c, k, v| {
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
        },
        read: |c| Value::Str(c.thp.name()),
    },
    bool_key!("lazy_band", lazy_band),
    bool_key!("guards", guards),
    Key {
        name: "guard_slab_interval",
        parse: |c, k, v| {
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
        },
        read: |c| Value::Uint(c.guard_slab_interval as u64),
    },
    Key {
        name: "huge_retain_max",
        parse: |c, k, v| {
            num(c, k, v, 0, MAX_HUGE_RETAIN_MAX as i64, |c, v| {
                c.huge_retain_max = v as usize
            })
        },
        read: |c| Value::Size(c.huge_retain_max),
    },
    Key {
        // Unset it follows `dirty_decay_ms`, which is what it reads back.
        name: "huge_dirty_decay_ms",
        parse: |c, k, v| {
            num(c, k, v, -1, MAX_INTERVAL_MS, |c, v| {
                c.huge_dirty_decay_ms = Some(v)
            })
        },
        read: |c| Value::Int(c.huge_decay_ms()),
    },
    bool_key!("zero_on_free", zero_on_free),
    bool_key!("pac_pointers", pac_pointers),
    bool_key!("slot_randomize", slot_randomize),
    bool_key!("double_free_scan", double_free_scan),
    bool_key!("abort_conf", abort_conf),
    bool_key!("cgroup", cgroup),
    bool_key!("check_zero", check_zero),
];

/// The names of [`TABLE`], in its order.
const fn names() -> [&'static str; TABLE.len()] {
    let mut out = [""; TABLE.len()];
    let mut i = 0;
    while i < TABLE.len() {
        out[i] = TABLE[i].name;
        i += 1;
    }
    out
}

const NAMES: [&str; TABLE.len()] = names();

verus! {

/// Every key readable through [`Config::get`], in table order, for the
/// `opt.` namespace (`mallctl` builds its `opt.` nodes from this slice at
/// compile time, by position) and the tests: the names of [`TABLE`].
/// Trusted: `NAMES` is built from `TABLE`, which holds function
/// pointers; every name in it is a `TABLE` entry's, so `listed` holds of it.
// The lifetimes are spelled out: `verus!` builds an `exec const` as a
// function in the verifier's view, where `&[&str]` would have none.
#[allow(clippy::redundant_static_lifetimes)]
#[verifier::external_body]
pub exec const KEYS: &'static [&'static str]
    ensures
        forall|i: int| 0 <= i < KEYS@.len() ==> listed(#[trigger] KEYS@[i].spec_bytes()),
{
    &NAMES
}

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
        if bytes_is(names[i].as_bytes(), key) {
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
/// is `const char *` (a static name, handed out as is).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Size(usize),
    Str(&'static CStr),
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
}

// Every issue formats (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Issue<'_> {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

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
        }
    }
}

/// Bytes shown as text, one `?` per non-printable byte, at most 64 bytes.
struct Bytes<'a>(&'a [u8]);

// Any bytes format (ghost only).
#[cfg(verus_only)]
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

/// Cursor over the comma-separated pairs of a conf string.
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
            },
    {
        if self.0.is_empty() {
            return None;
        }
        // A quoted value may contain commas: skip to the closing quote when
        // the value starts with one.
        let mut i = 0;
        let mut quoted = false;
        while i < self.0.len()
            invariant
                i <= self.0@.len(),
                self.0 == old(self).0,
            decreases self.0@.len() - i,
        {
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
        self.0 = if i < self.0.len() {
            &self.0[i + 1..]
        } else {
            &[]
        };
        Some(pair)
    }
}

/// `value` without one pair of surrounding double quotes.
pub open spec fn unquote_spec(value: Seq<u8>) -> Seq<u8> {
    if value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"' {
        value.subrange(1, value.len() - 1)
    } else {
        value
    }
}

/// Strip one pair of surrounding double quotes.
fn unquote(value: &[u8]) -> (r: &[u8])
    ensures
        r@ == unquote_spec(value@),
{
    if value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"' {
        &value[1..value.len() - 1]
    } else {
        value
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
    if bytes_is(value, b"true") {
        Some(true)
    } else if bytes_is(value, b"false") {
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
    let (radix, digits) = if digits.len() >= 3
        && digits[0] == b'0'
        && (digits[1] == b'x' || digits[1] == b'X')
    {
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

} // verus!

pub use linux::load;
#[cfg(test)]
use linux::{MAX_TEXT, env_var, first_line_of, malloc_conf_symbol};

mod linux {
    use core::ffi::{c_char, c_void};
    use core::fmt::Write as _;

    use super::{Config, Issue, position_of};
    use crate::sys;
    use crate::sys::bytes_is;
    use vstd::prelude::verus;
    #[cfg(verus_only)]
    use vstd::prelude::*;

    verus! {

    /// Longest conf text read from any one source. Longer inputs are cut
    /// here; no real configuration comes close.
    pub const MAX_TEXT: usize = 4096;

    } // verus!

    use crate::sys::auxv::{AT_SECURE, getauxval};

    unsafe extern "C" {
        /// glibc's environment pointer. Read directly so no `getenv` runs
        /// inside `malloc`.
        static environ: *const *const c_char;
    }

    verus! {

    /// [`Config::parse`] of `text` from `source`, printing each problem as
    /// `challoc: <source>: <issue>` to `out`. Trusted: the report is a
    /// closure that formats into `out`, a mutable capture Verus does not
    /// model; the contract is `parse`'s.
    #[verifier::external_body]
    #[inline(always)]
    fn parse_from(cfg: &mut Config, text: &[u8], source: &str, out: &mut sys::Stderr) -> (issues: usize)
        requires
            old(cfg).wf(),
        ensures
            issues <= text.len(),
            old(cfg).wf() ==> final(cfg).wf(),
    {
        cfg.parse(text, |i: Issue<'_>| {
            let _ = writeln!(out, "challoc: {source}: {i}");
        })
    }

    /// `/etc/challoc.conf`, NUL-terminated (spelled as bytes so the
    /// verifier sees the NUL `first_line_of` requires).
    const CHALLOC_CONF: [u8; 18] = [
        b'/', b'e', b't', b'c', b'/', b'c', b'h', b'a', b'l', b'l', b'o', b'c', b'.', b'c', b'o', b'n', b'f', 0,
    ];

    /// Read every source in priority order into a `Config`, printing each
    /// problem as `challoc: <source>: <issue>` on stderr. Aborts when
    /// `abort_conf` is set by the time all sources are read and anything was
    /// reported, an inert key included. Never allocates: the file goes
    /// through a stack buffer and the environment is walked in place. The
    /// result is well formed: the defaults are (`Config::lemma_default_wf`)
    /// and every source goes through `Config::parse`, which keeps it.
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
        if let Some(text) = sym {
            issues += parse_from(&mut cfg, text, "malloc_conf", &mut out);
        }
        if let Some(text) = je_sym {
            issues += parse_from(&mut cfg, text, "je_malloc_conf", &mut out);
        }
        let mut buf = [0u8; MAX_TEXT];
        if let Some(line) = first_line_of(&CHALLOC_CONF, &mut buf) {
            issues += parse_from(&mut cfg, line, "/etc/challoc.conf", &mut out);
        }
        if getauxval(AT_SECURE) == 0 {
            if let Some(text) = env_var(b"MALLOC_CONF") {
                issues += parse_from(&mut cfg, text, "MALLOC_CONF", &mut out);
            }
            if let Some(text) = env_var(b"CHALLOC_CONF") {
                issues += parse_from(&mut cfg, text, "CHALLOC_CONF", &mut out);
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

    /// The string the `malloc_conf` (`which == 0`, defined by the library,
    /// interposed by a program's definition) or weak `je_malloc_conf`
    /// (`which == 1`) global points at, `None` when the process defines
    /// neither the symbol nor a string. The symbol's pointer comes through
    /// the GOT and the string through `cstr`, which stops at [`MAX_TEXT`]
    /// bytes.
    pub fn malloc_conf_symbol(which: u32) -> (r: Option<&'static [u8]>)
        ensures
            r matches Some(s) ==> s@.len() <= MAX_TEXT,
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

    /// The `const char *` the symbol at `slot` holds. Trusted: a raw read;
    /// nothing is claimed about the pointer.
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

    /// A NUL-terminated string as a slice of at most [`MAX_TEXT`] bytes;
    /// `None` for null or empty.
    fn cstr(p: *const c_char) -> (r: Option<&'static [u8]>)
        ensures
            r matches Some(s) ==> s@.len() <= MAX_TEXT,
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
        // SAFETY: `n` bytes before the terminator were just read.
        Some(unsafe { bytes_at(p, n) })
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

    /// The value of environment variable `name` (no `=` in it), read from
    /// `environ` without `getenv`. `None` when unset or empty.
    ///
    /// Trusted: walks `environ` through raw pointers; each entry is read
    /// through `cstr`, which stops at [`MAX_TEXT`] bytes, and the value is
    /// a suffix of the entry (`env_match`).
    #[verifier::external_body]
    pub fn env_var(name: &[u8]) -> (r: Option<&'static [u8]>)
        ensures
            r matches Some(s) ==> s@.len() <= MAX_TEXT,
    {
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
                if let Some(value) = env_match(text, name) {
                    return if value.is_empty() { None } else { Some(value) };
                }
            }
            // SAFETY: not yet at the terminating null.
            e = unsafe { e.add(1) };
        }
    }

    /// The value in the environment entry `text` (`NAME=value`) when it
    /// sets `name`, possibly empty; `None` for another variable.
    #[inline(always)]
    fn env_match<'a>(text: &'a [u8], name: &[u8]) -> (r: Option<&'a [u8]>)
        ensures
            r is Some <==> text@.len() > name@.len() && text@.take(name@.len() as int) == name@
                && text@[name@.len() as int] == b'=',
            r matches Some(value) ==> value@ == text@.skip(name@.len() + 1int),
    {
        if text.len() > name.len()
            && bytes_is(&text[..name.len()], name)
            && text[name.len()] == b'='
        {
            Some(&text[name.len() + 1..])
        } else {
            None
        }
    }

    /// The first line of the file at NUL-terminated `path`, without its
    /// newline, read with raw syscalls into `buf`. `None` if the file is
    /// missing, unreadable or its first line is empty.
    pub fn first_line_of<'b>(path: &[u8], buf: &'b mut [u8]) -> (r: Option<&'b [u8]>)
        requires
            crate::sys::may_syscall(),
            path@.len() > 0,
            path@.last() == 0,
        ensures
            r matches Some(line) ==> line@.len() <= old(buf)@.len(),
    {
        let fd = sys::open(path, sys::O_RDONLY).ok()?;
        let n = sys::read_full(fd, buf);
        let _ = sys::close(fd);
        let n = n.ok()?;
        first_line(&buf[..n])
    }

    /// The first line of `text` without its `\n` and one `\r` before it;
    /// `None` when that line is empty.
    #[inline(always)]
    fn first_line(text: &[u8]) -> (r: Option<&[u8]>)
        ensures
            r matches Some(line) ==> {
                &&& 0 < line@.len() <= text@.len()
                &&& line@ == text@.take(line@.len() as int)
                &&& forall|j: int| 0 <= j < line@.len() ==> line@[j] != b'\n'
            },
    {
        let end = position_of(text, b'\n').unwrap_or(text.len());
        let line = &text[..end];
        // `line.strip_suffix(b"\r").unwrap_or(line)`, spelled out: vstd
        // does not specify `strip_suffix`.
        let line = if !line.is_empty() && line[line.len() - 1] == b'\r' {
            &line[..line.len() - 1]
        } else {
            line
        };
        if line.is_empty() { None } else { Some(line) }
    }

    } // verus!
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CLASSES, NBINS, NCLASSES};
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

    /// Every value in `bad` is refused for `key`: one issue, `invalid conf
    /// value` (or `malformed` for the empty value, which has no
    /// `key:value` shape), and the config left at the defaults.
    fn rejects_bad_values(key: &str, bad: &[&str]) {
        for bad in bad {
            let text = std::format!("{key}:{bad}");
            let (cfg, issues) = parse(&text);
            assert_eq!(cfg, Config::DEFAULT, "{text} changed the config");
            assert_eq!(issues.len(), 1, "{text}: {issues:?}");
            let expect = if bad.is_empty() {
                "malformed"
            } else {
                "invalid conf value \""
            };
            assert!(issues[0].contains(expect), "{text}: {}", issues[0]);
        }
    }

    #[test]
    fn defaults_match_the_design_table() {
        let d = Config::DEFAULT;
        assert!(!d.background_thread);
        assert_eq!(d.dirty_decay_ms, 10_000);
        assert_eq!(d.narenas, 0);
        assert_eq!(d.percpu_arena, PercpuArena::Disabled, "jemalloc's default");
        assert!(d.tcache);
        assert_eq!(d.tcache_max, 32 * 1024);
        assert_eq!(d.tcache_nslots_small_max, 1024);
        assert_eq!(d.lg_tcache_nslots_mul, 1);
        assert_eq!(d.thp, Thp::Never);
        assert!(d.lazy_band);
        assert!(!clean("lazy_band:false").lazy_band);
        assert!(d.guards);
        assert_eq!(d.guard_slab_interval, 8);
        assert_eq!(d.huge_retain_max, 64 << 20);
        assert_eq!(d.huge_retain_max, crate::huge::DEFAULT_RETAIN_MAX);
        assert!(
            !d.zero_on_free
                && !d.slot_randomize
                && !d.double_free_scan
                && !d.abort_conf
                && !d.check_zero
        );
        assert!(!d.pac_pointers, "signed pointers are opt-in");
        assert!(d.cgroup);
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
            "lazy_band",
            "guards",
            "zero_on_free",
            "pac_pointers",
            "slot_randomize",
            "double_free_scan",
            "abort_conf",
            "cgroup",
            "check_zero",
        ];
        for key in keys {
            for v in [true, false] {
                let text = std::format!("{key}:{v}");
                assert_eq!(
                    clean(&text).get(key.as_bytes()),
                    Some(Value::Bool(v)),
                    "{key}:{v}"
                );
            }
            rejects_bad_values(key, &["yes", "1", "TRUE", "", "truex"]);
        }
    }

    /// `narenas:0` (the default) resolves to `min(4 * ncpus, 64)`; an
    /// explicit value is taken as is.
    #[test]
    fn narenas_default_resolves_from_the_cpu_count() {
        let d = Config::DEFAULT;
        assert_eq!(d.effective_narenas(1), 4);
        assert_eq!(
            d.effective_narenas(0),
            4,
            "a failed probe counts as one CPU"
        );
        assert_eq!(d.effective_narenas(4), 16);
        assert_eq!(d.effective_narenas(16), 64);
        assert_eq!(d.effective_narenas(192), 64);
        assert_eq!(d.effective_narenas(u32::MAX), 64);
        assert_eq!(clean("narenas:1").effective_narenas(16), 1);
        assert_eq!(clean("narenas:16").effective_narenas(1), 16);
        assert_eq!(clean("narenas:0").effective_narenas(2), 8);
    }

    #[test]
    fn every_numeric_key_parses_and_clamps() {
        // (key, in-range value, below-min input, clamped min, above-max input, clamped max)
        let cases: [(&str, i64, &str, i64, &str, i64); 9] = [
            (
                "dirty_decay_ms",
                5000,
                "-2",
                -1,
                "9999999999999999",
                MAX_INTERVAL_MS,
            ),
            (
                "huge_dirty_decay_ms",
                2000,
                "-2",
                -1,
                "9999999999999999",
                MAX_INTERVAL_MS,
            ),
            (
                "muzzy_decay_ms",
                0,
                "-5",
                -1,
                "9999999999999999",
                MAX_INTERVAL_MS,
            ),
            ("narenas", 4, "-1", 0, "99999999999", u32::MAX as i64),
            (
                "tcache_max",
                4096,
                "-1",
                0,
                "9999999",
                crate::layout::MEDIUM_MAX as i64,
            ),
            (
                "tcache_nslots_small_max",
                1000,
                "1",
                MIN_CAP as i64,
                "1000000",
                MAX_NSLOTS as i64,
            ),
            ("lg_tcache_nslots_mul", 3, "-17", -16, "17", 16),
            (
                "guard_slab_interval",
                2,
                "-1",
                0,
                "5000000",
                MAX_GUARD_SLAB_INTERVAL as i64,
            ),
            (
                "huge_retain_max",
                1 << 20,
                "-1",
                0,
                "9999999999999999",
                MAX_HUGE_RETAIN_MAX as i64,
            ),
        ];
        for (key, ok, low, min, high, max) in cases {
            let as_value = |v: i64| match key {
                "tcache_max" | "huge_retain_max" => Value::Size(v as usize),
                "narenas" | "tcache_nslots_small_max" | "guard_slab_interval" => {
                    Value::Uint(v as u64)
                }
                _ => Value::Int(v),
            };
            let cfg = clean(&std::format!("{key}:{ok}"));
            assert_eq!(cfg.get(key.as_bytes()), Some(as_value(ok)), "{key}:{ok}");

            let (cfg, issues) = parse(&std::format!("{key}:{low}"));
            assert_eq!(
                cfg.get(key.as_bytes()),
                Some(as_value(min)),
                "{key}:{low} clamps to min"
            );
            assert_eq!(issues.len(), 1);
            assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

            let (cfg, issues) = parse(&std::format!("{key}:{high}"));
            assert_eq!(
                cfg.get(key.as_bytes()),
                Some(as_value(max)),
                "{key}:{high} clamps to max"
            );
            assert_eq!(issues.len(), 1);
            assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

            for bad in ["x", "1x", "0x", "--1", "1.5", " 1", "true"] {
                let (cfg, issues) = parse(&std::format!("{key}:{bad}"));
                assert_eq!(cfg, Config::DEFAULT, "{key}:{bad} changed the config");
                assert_eq!(issues.len(), 1, "{key}:{bad}");
                assert!(
                    issues[0].contains("invalid conf value"),
                    "{key}:{bad}: {}",
                    issues[0]
                );
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
        assert_eq!(
            clean("tcache_nslots_small_max:0x100").tcache_nslots_small_max,
            256
        );
    }

    #[test]
    fn thp_values() {
        assert_eq!(clean("thp:always").thp, Thp::Always);
        assert_eq!(clean("thp:auto").thp, Thp::Auto);
        assert_eq!(clean("thp:never").thp, Thp::Never);
        assert_eq!(clean("thp:default").thp, Thp::Never);
        assert_eq!(clean("thp:always").get(b"thp"), Some(Value::Str(c"always")));
        assert_eq!(clean("thp:auto").get(b"thp"), Some(Value::Str(c"auto")));
        assert_eq!(Config::DEFAULT.thp, Thp::Never);
        let (cfg, issues) = parse("thp:madvise");
        assert_eq!(cfg.thp, Thp::Never);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid conf value \"madvise\" for thp"));
        // Accepted under abort_conf.
        assert_eq!(clean("thp:auto,abort_conf:true").thp, Thp::Auto);
    }

    /// `percpu_arena`: jemalloc's three values, anything else reported and
    /// left at the default.
    #[test]
    fn percpu_arena_values() {
        assert!(
            KEYS.contains(&"percpu_arena"),
            "a real key, readable through opt.percpu_arena"
        );
        for (value, want) in [
            ("disabled", PercpuArena::Disabled),
            ("percpu", PercpuArena::Percpu),
            ("phycpu", PercpuArena::Phycpu),
        ] {
            let cfg = clean(&std::format!("percpu_arena:{value},abort_conf:true"));
            assert_eq!(cfg.percpu_arena, want, "{value}");
            assert_eq!(
                cfg.get(b"percpu_arena"),
                Some(Value::Str(want.name())),
                "{value}"
            );
            assert_eq!(want.name().to_str(), Ok(value));
            assert!(cfg.abort_conf);
        }
        rejects_bad_values(
            "percpu_arena",
            &[
                "true", "false", "1", "on", "auto", "off", "PERCPU", "", "percpux",
            ],
        );
        // The last pair wins, as for every key.
        assert_eq!(
            clean("percpu_arena:percpu,percpu_arena:disabled").percpu_arena,
            PercpuArena::Disabled
        );
        assert_eq!(
            clean("percpu_arena:\"percpu\"").percpu_arena,
            PercpuArena::Percpu
        );
    }

    /// The one inert key: a bool by value, no field, unreadable, one
    /// `Issue::Inert` per pair (what `abort_conf` aborts on), and in no
    /// other table.
    #[test]
    fn inert_keys_are_reported_and_rejected_under_abort_conf() {
        assert_eq!(INERT_KEYS, &["stats_print"]);
        for key in INERT_KEYS {
            assert!(!KEYS.contains(key) && !IGNORED_KEYS.contains(key), "{key}");
            let value = "true";
            let text = std::format!("{key}:{value}");
            let (cfg, issues) = parse(&text);
            assert_eq!(issues.len(), 1, "{text}: {issues:?}");
            assert_eq!(
                issues[0],
                std::format!("conf key {key} is accepted but has no effect (not implemented)"),
                "{text}"
            );
            assert_eq!(cfg, Config::DEFAULT, "{key} has no field");
            assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
            let (cfg, issues) = parse(&std::format!("{key}:nonsense"));
            assert_eq!(cfg, Config::DEFAULT, "{key}:nonsense changed the config");
            assert_eq!(issues.len(), 1, "{key}:nonsense: {issues:?}");
            assert!(
                issues[0].contains("invalid conf value"),
                "{key}:nonsense: {}",
                issues[0]
            );
        }
        // The two hardening keys are real, opt-in keys: no report, stored,
        // readable, and `abort_conf` accepts them.
        for key in ["zero_on_free", "slot_randomize"] {
            let (cfg, issues) = parse(&std::format!("{key}:true,abort_conf:true"));
            assert!(issues.is_empty(), "{key}: {issues:?}");
            assert_eq!(cfg.get(key.as_bytes()), Some(Value::Bool(true)), "{key}");
            assert!(cfg.abort_conf);
        }
        let (cfg, _) = parse("zero_on_free:true,slot_randomize:true");
        assert!(cfg.zero_on_free && cfg.slot_randomize);
        // Mixed with real keys the real ones still apply and the inert ones
        // are counted: what `load` aborts on under `abort_conf`.
        let (cfg, issues) =
            parse("narenas:3,thp:always,cgroup:false,abort_conf:true,stats_print:true");
        assert_eq!(cfg.narenas, 3);
        assert!(cfg.abort_conf && !cfg.cgroup && cfg.thp == Thp::Always);
        assert_eq!(issues.len(), 1, "cgroup and thp are real keys: {issues:?}");
    }

    /// `huge_dirty_decay_ms` (design 7): unset it follows `dirty_decay_ms`
    /// (the default reads as the slab period, and moves with it); set, it
    /// is the huge tier's own period with `dirty_decay_ms`'s range, `-1`
    /// never and `0` at once, and it round-trips through `set`/`get`.
    #[test]
    fn huge_dirty_decay_ms_follows_dirty_decay_ms_until_set() {
        let d = Config::DEFAULT;
        assert_eq!(d.huge_dirty_decay_ms, None);
        assert_eq!(d.huge_decay_ms(), d.dirty_decay_ms);
        assert_eq!(d.get(b"huge_dirty_decay_ms"), Some(Value::Int(10_000)));
        let cfg = clean("dirty_decay_ms:3000");
        assert_eq!(
            cfg.huge_dirty_decay_ms, None,
            "the slab key leaves it following"
        );
        assert_eq!(cfg.huge_decay_ms(), 3000);
        assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(3000)));
        let cfg = clean("huge_dirty_decay_ms:2000,dirty_decay_ms:3000");
        assert_eq!(cfg.huge_dirty_decay_ms, Some(2000));
        assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (2000, 3000));
        assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(2000)));
        assert_eq!(
            cfg.get(b"dirty_decay_ms"),
            Some(Value::Int(3000)),
            "the huge key leaves the slab key alone"
        );
        assert_eq!(clean("huge_dirty_decay_ms:0").huge_dirty_decay_ms, Some(0));
        assert_eq!(
            clean("huge_dirty_decay_ms:-1").huge_dirty_decay_ms,
            Some(-1)
        );
        assert_eq!(
            clean("huge_dirty_decay_ms:-1").get(b"huge_dirty_decay_ms"),
            Some(Value::Int(-1))
        );
        // Set to the slab period explicitly it stays pinned there when
        // the slab period later differs.
        let cfg = clean("huge_dirty_decay_ms:10000,dirty_decay_ms:500");
        assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (10_000, 500));
        let (cfg, issues) = parse("huge_dirty_decay_ms:-2");
        assert_eq!(
            cfg.huge_dirty_decay_ms,
            Some(-1),
            "clamped like dirty_decay_ms"
        );
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains("out-of-range conf value \"-2\" for huge_dirty_decay_ms"),
            "{}",
            issues[0]
        );
    }

    #[test]
    fn huge_retain_max_is_a_byte_count() {
        assert_eq!(clean("huge_retain_max:0").huge_retain_max, 0);
        assert_eq!(clean("huge_retain_max:0x100000").huge_retain_max, 1 << 20);
        assert_eq!(
            clean("huge_retain_max:1099511627776").huge_retain_max,
            MAX_HUGE_RETAIN_MAX
        );
        assert_eq!(
            clean("huge_retain_max:1").get(b"huge_retain_max"),
            Some(Value::Size(1))
        );
        let (cfg, issues) = parse("huge_retain_max:1099511627777");
        assert_eq!(
            cfg.huge_retain_max, MAX_HUGE_RETAIN_MAX,
            "clamped to the huge region"
        );
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains("out-of-range conf value \"1099511627777\" for huge_retain_max"),
            "{}",
            issues[0]
        );
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
        assert!(
            issues[0].contains("unknown conf key \"bogus\""),
            "{}",
            issues[0]
        );
        assert!(
            issues[1].contains("unknown conf key \"another_bogus\""),
            "{}",
            issues[1]
        );
        // Keys are case sensitive and not trimmed, as in jemalloc.
        for text in [
            "Tcache:false",
            " tcache:false",
            "tcache :false",
            "tcache: false",
        ] {
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
        assert!(
            issues[0].contains("malformed conf pair \"nocolon\""),
            "{}",
            issues[0]
        );
        assert!(
            issues[1].contains("malformed conf pair \":novalue\""),
            "{}",
            issues[1]
        );
        assert!(
            issues[2].contains("malformed conf pair \"nokey:\""),
            "{}",
            issues[2]
        );
        // A lone colon and a value with a second colon.
        let (_, issues) = parse(":");
        assert_eq!(issues.len(), 1);
        let (cfg, issues) = parse("narenas:1:2");
        assert_eq!(
            cfg.narenas,
            Config::DEFAULT.narenas,
            "an invalid value leaves the default"
        );
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains("invalid conf value \"1:2\""),
            "{}",
            issues[0]
        );
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
        let cfg = clean(
            "background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0,narenas:2,abort_conf:true",
        );
        assert!(cfg.background_thread && cfg.abort_conf);
        assert_eq!(cfg.dirty_decay_ms, 0);
        assert_eq!(cfg.narenas, 2);
        // jemalloc's own `percpu_arena` values parse clean under `abort_conf`.
        let cfg = clean("percpu_arena:disabled,abort_conf:true");
        assert_eq!(cfg.percpu_arena, PercpuArena::Disabled);
        assert_eq!(
            clean("percpu_arena:phycpu,narenas:4").percpu_arena,
            PercpuArena::Phycpu
        );
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
                Value::Size(n) => std::format!("{n}"),
                Value::Str(s) => s.to_str().unwrap().into(),
            };
            let text = std::format!("{key}:{value}");
            let parsed = clean(&text);
            if *key == "huge_dirty_decay_ms" {
                // Its default is "follow `dirty_decay_ms`" (`None`), which
                // reads back as that period; setting the reading pins it.
                assert_eq!(
                    parsed.get(key.as_bytes()),
                    cfg.get(key.as_bytes()),
                    "{text}"
                );
                assert_eq!(
                    parsed.huge_dirty_decay_ms,
                    Some(cfg.dirty_decay_ms),
                    "{text}"
                );
                assert_eq!(
                    Config {
                        huge_dirty_decay_ms: None,
                        ..parsed
                    },
                    cfg,
                    "{text} changed another field"
                );
            } else {
                assert_eq!(parsed, cfg, "{key}:{value} round trip");
            }
        }
        assert_eq!(cfg.get(b"nonsense"), None);
        assert_eq!(cfg.get(b""), None);
        for key in IGNORED_KEYS {
            assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
        }
    }

    /// The three tables and `set` agree: every name in `KEYS`,
    /// `IGNORED_KEYS` and `INERT_KEYS` is one `set` knows (a bad value is
    /// refused as a value, never as an unknown key; an ignored key takes
    /// any value; an inert key is reported as inert), the tables are
    /// disjoint, and a name in none of them is [`Issue::UnknownKey`], so a
    /// key added to one table that another also names (or a typo in a
    /// test) fails here rather than in `opt.<key>` or `malloc_stats_print`.
    #[test]
    fn set_knows_exactly_the_tabled_keys() {
        for key in KEYS.iter().chain(IGNORED_KEYS).chain(INERT_KEYS) {
            let mut cfg = Config::DEFAULT;
            let r = cfg.set(key.as_bytes(), b"nonsense-value");
            assert!(!matches!(r, Err(Issue::UnknownKey(_))), "{key}: {r:?}");
            if IGNORED_KEYS.contains(key) {
                assert_eq!(r, Ok(()), "{key} takes any value");
            } else if INERT_KEYS.contains(key) {
                assert_eq!(
                    r,
                    Err(Issue::BadValue {
                        key: key.as_bytes(),
                        value: b"nonsense-value"
                    }),
                    "{key}"
                );
                assert_eq!(
                    cfg.set(key.as_bytes(), b"true"),
                    Err(Issue::Inert(key.as_bytes())),
                    "{key}"
                );
            } else {
                assert_eq!(
                    r,
                    Err(Issue::BadValue {
                        key: key.as_bytes(),
                        value: b"nonsense-value"
                    }),
                    "{key}"
                );
                assert!(cfg.get(key.as_bytes()).is_some(), "{key} is readable");
            }
            let n = [
                KEYS.contains(key),
                IGNORED_KEYS.contains(key),
                INERT_KEYS.contains(key),
            ];
            assert_eq!(
                n.iter().filter(|&&b| b).count(),
                1,
                "{key} is in exactly one table"
            );
        }
        for unknown in [
            "nonsense",
            "tcache_maxx",
            "Tcache",
            "stats_prin",
            "thp_proces",
            "",
        ] {
            let mut cfg = Config::DEFAULT;
            assert_eq!(
                cfg.set(unknown.as_bytes(), b"true"),
                Err(Issue::UnknownKey(unknown.as_bytes())),
                "{unknown:?}"
            );
            assert_eq!(cfg, Config::DEFAULT);
        }
    }

    /// `KEYS` is `TABLE`'s names in `TABLE`'s order (what `mallctl`
    /// indexes the `opt.` MIB by), every name is unique, and every entry's
    /// reader and parser are the field's: reading an entry, setting a
    /// different value of its type through `set` and reading it back
    /// changes that entry and no other. Replaces a hand-kept count: a new
    /// key is one `TABLE` entry, and it is checked here whatever its name.
    #[test]
    fn the_table_is_the_one_spelling_of_every_key() {
        assert_eq!(KEYS.len(), TABLE.len());
        for (i, key) in TABLE.iter().enumerate() {
            assert_eq!(KEYS[i], key.name);
            assert_eq!(
                KEYS.iter().filter(|k| **k == key.name).count(),
                1,
                "{} is named once",
                key.name
            );
            assert!(
                !IGNORED_KEYS.contains(&key.name) && !INERT_KEYS.contains(&key.name),
                "{}",
                key.name
            );
            // A value of the key's type that differs from the default.
            let d = Config::DEFAULT;
            let other = match (key.read)(&d) {
                Value::Bool(b) => std::format!("{}", !b),
                Value::Int(i) => std::format!("{}", if i == 0 { 1 } else { 0 }),
                // Doubling stays in every unsigned key's range and keeps
                // `guard_slab_interval` a power of two.
                Value::Uint(u) => std::format!("{}", if u == 0 { 2 } else { u * 2 }),
                Value::Size(n) => std::format!("{}", if n == 0 { 2 } else { n * 2 }),
                Value::Str(s) => match key.name {
                    "percpu_arena" => "percpu".into(),
                    "thp" => "always".into(),
                    _ => panic!("{}: a string key with no test value ({s:?})", key.name),
                },
            };
            let mut cfg = d;
            assert_eq!(
                cfg.set(key.name.as_bytes(), other.as_bytes()),
                Ok(()),
                "{}:{other}",
                key.name
            );
            assert_ne!(
                (key.read)(&cfg),
                (key.read)(&d),
                "{}:{other} read back through its own entry",
                key.name
            );
            assert_eq!(
                cfg.get(key.name.as_bytes()),
                Some((key.read)(&cfg)),
                "{}",
                key.name
            );
            for o in TABLE.iter().filter(|o| o.name != key.name) {
                if key.name == "dirty_decay_ms" && o.name == "huge_dirty_decay_ms" {
                    // Unset, the huge period reads as the slab period
                    // (design 7): the one reading that moves with another key.
                    assert_eq!(
                        (o.read)(&cfg),
                        (key.read)(&cfg),
                        "huge_dirty_decay_ms follows dirty_decay_ms"
                    );
                    continue;
                }
                assert_eq!(
                    (o.read)(&cfg),
                    (o.read)(&d),
                    "{}:{other} changed {}",
                    key.name,
                    o.name
                );
            }
        }
        // The aliases of the named-value keys: one entry, several spellings.
        assert_eq!(clean("thp:default").thp, clean("thp:never").thp);
    }

    #[test]
    fn cache_caps_follow_the_ladder_and_the_overrides() {
        let d = Config::DEFAULT;
        // Classes above tcache_max (32 KiB, jemalloc's default) are not cached.
        fn cached(cfg: &Config) -> impl Iterator<Item = usize> {
            let max = cfg.tcache_max;
            (0..NCLASSES).filter(move |&c| CLASSES[c].size() <= max)
        }
        fn uncached(cfg: &Config) -> impl Iterator<Item = usize> {
            let max = cfg.tcache_max;
            (0..NCLASSES).filter(move |&c| CLASSES[c].size() > max)
        }
        assert!(
            cached(&d).count() >= 36 && uncached(&d).count() >= 8,
            "{} cached, {} above",
            cached(&d).count(),
            uncached(&d).count()
        );
        for c in cached(&d) {
            assert_eq!(d.cap_for(0, 0, c), ladder(c, MAX_CAP, 1));
            assert_eq!(d.cap_for(0, 0, c), crate::heap::default_cap(c), "class {c}");
        }
        for c in uncached(&d) {
            assert_eq!(
                d.cap_for(0, 0, c),
                0,
                "class {c} ({} bytes) is above tcache_max",
                CLASSES[c].size()
            );
            assert!(
                crate::heap::default_cap(c) >= MIN_CAP,
                "the ladder itself never reaches zero"
            );
        }
        // Redis: x4 ladder clamped at 1000, never below MIN_CAP.
        let redis = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
        for c in cached(&redis) {
            let size = CLASSES[c].size();
            let cap = redis.cap_for(0, 0, c);
            assert!(
                cap >= MIN_CAP && cap <= 1000,
                "class {c} ({size} bytes): {cap}"
            );
            if size <= 512 {
                assert_eq!(cap, 1000, "class {c} ({size} bytes)");
            }
            assert!(
                cap >= d.cap_for(0, 0, c).min(1000),
                "class {c} never shrinks below the default ladder"
            );
        }
        // A small max caps every class; lg_mul below 1 shrinks the ladder.
        let small = clean("tcache_nslots_small_max:16");
        assert!(
            cached(&small)
                .all(|c| small.cap_for(0, 0, c) <= 16 && small.cap_for(0, 0, c) >= MIN_CAP)
        );
        let shrunk = clean("lg_tcache_nslots_mul:0");
        assert_eq!(shrunk.cap_for(0, 0, 0), 512);
        assert_eq!(shrunk.cap_for(0, 0, NCLASSES - 1), 0);
        assert_eq!(
            shrunk.cap_for(0, 0, cached(&shrunk).last().unwrap()),
            MIN_CAP
        );
        let grown = clean("lg_tcache_nslots_mul:16,tcache_nslots_small_max:65536");
        assert!(cached(&grown).all(|c| grown.cap_for(0, 0, c) == 65536));
        // tcache_max moves the boundary: every slab class, or none.
        let all = clean("tcache_max:262144");
        assert!((0..NCLASSES).all(|c| all.cap_for(0, 0, c) >= MIN_CAP));
        assert_eq!(all.cap_for(0, 0, NCLASSES - 1), MIN_CAP);
        let none = clean("tcache_max:0");
        assert!((0..NCLASSES).all(|c| none.cap_for(0, 0, c) == 0));
        let k4 = clean("tcache_max:4096");
        assert!((0..NCLASSES).all(|c| (k4.cap_for(0, 0, c) == 0) == (CLASSES[c].size() > 4096)));
        // Out of range clamps and reports, like every numeric key.
        assert_eq!(clean("tcache_max:32768").tcache_max, 32768);
        // tcache:false turns caching off entirely.
        let off = clean("tcache:false");
        assert!((0..NCLASSES).all(|c| off.cap_for(0, 0, c) == 0));
        // Extreme shifts do not overflow.
        assert_eq!(ladder(0, MAX_NSLOTS, -16), MIN_CAP);
        assert_eq!(ladder(NCLASSES - 1, MAX_NSLOTS, 16), MAX_NSLOTS);
    }

    /// `cap_for` is the one rule for which classes are cached where: the
    /// home ladder, nothing for a shared class (above `tcache_max`) or
    /// with `tcache:false`, and nothing above `BIN_MAX` for another arena.
    #[test]
    fn cap_for_is_the_one_rule() {
        let d = Config::DEFAULT;
        for c in 0..NCLASSES {
            let size = CLASSES[c].size();
            assert_eq!(d.shared_class(c), size > d.tcache_max, "class {c}");
            assert_eq!(d.cap_for(3, 3, c), d.cap_for(0, 0, c), "class {c}");
            assert_eq!(d.cap_for(3, 3, c) == 0, d.shared_class(c), "class {c}");
            let foreign = d.cap_for(3, 0, c);
            assert_eq!(
                foreign,
                if c >= NBINS { 0 } else { d.cap_for(0, 0, c) },
                "class {c}"
            );
            assert_eq!(d.cap_for(0, 3, c), foreign);
        }
        let none = clean("tcache:false");
        assert!((0..NCLASSES).all(|c| none.cap_for(0, 0, c) == 0 && none.cap_for(0, 1, c) == 0));
        let k4 = clean("tcache_max:4096");
        assert!((0..NCLASSES).all(|c| k4.shared_class(c) == (CLASSES[c].size() > 4096)));
    }

    #[test]
    fn guards_off_zeroes_the_interval() {
        assert_eq!(clean("guards:false").effective_guard_slab_interval(), 0);
        assert_eq!(
            clean("guard_slab_interval:0").effective_guard_slab_interval(),
            0
        );
        assert_eq!(
            clean("guard_slab_interval:2").effective_guard_slab_interval(),
            2
        );
        // 1 would leave no slabs to allocate: out of range, clamped to 2.
        let (cfg, issues) = parse("guard_slab_interval:1");
        assert_eq!(cfg.effective_guard_slab_interval(), 2);
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains("out-of-range conf value \"1\" for guard_slab_interval"),
            "{}",
            issues[0]
        );
        // The interval is a power of two (the sized path's guard test is a
        // mask): anything else rounds up, is reported, and the rounded value
        // is what `opt.guard_slab_interval` returns; the maximum is one.
        for (text, want) in [("3", 4u32), ("5", 8), ("12", 16), ("4194303", 1 << 22)] {
            let (cfg, issues) = parse(&std::format!("guard_slab_interval:{text}"));
            assert_eq!(cfg.effective_guard_slab_interval(), want, "{text}");
            assert_eq!(
                cfg.get(b"guard_slab_interval"),
                Some(Value::Uint(want as u64))
            );
            assert_eq!(issues.len(), 1, "{text}");
            assert_eq!(
                issues[0],
                std::format!(
                    "conf value \"{text}\" for guard_slab_interval is not a power of two (rounded up to {want})"
                )
            );
        }
        for text in ["0", "2", "4", "8", "64", "4194304"] {
            let (cfg, issues) = parse(&std::format!("guard_slab_interval:{text}"));
            assert_eq!(cfg.guard_slab_interval, text.parse::<u32>().unwrap());
            assert!(issues.is_empty(), "{text}: {issues:?}");
        }
        // Under `abort_conf` a rounded value counts as a problem, as the
        // clamp does (`load` aborts on any issue).
        let (cfg, issues) = parse("guard_slab_interval:6,abort_conf:true");
        assert!(cfg.abort_conf && cfg.guard_slab_interval == 8 && issues.len() == 1);
    }

    #[test]
    fn issue_display_is_printable_and_bounded() {
        let long: Vec<u8> = (0..200u8).collect();
        let text = std::format!("{}", Issue::UnknownKey(&long));
        assert!(text.contains("..."));
        assert!(text.len() < 120, "{text}");
        assert!(!text.bytes().any(|b| b < 0x20 || b >= 0x7f), "{text:?}");
        assert_eq!(
            std::format!("{}", Issue::Malformed(b"a\tb")),
            "malformed conf pair \"a?b\""
        );
    }
}

#[cfg(test)]
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
            std::env::set_var("CHALLOC_CONF_TEST", "narenas:3,tcache:false");
            std::env::set_var("CHALLOC_CONF_TEST_EMPTY", "");
        }
        assert_eq!(
            env_var(b"CHALLOC_CONF_TEST"),
            Some(&b"narenas:3,tcache:false"[..])
        );
        assert_eq!(env_var(b"CHALLOC_CONF_TEST_EMPTY"), None);
        assert_eq!(env_var(b"CHALLOC_CONF_TES"), None, "prefix must not match");
        assert_eq!(env_var(b"CHALLOC_CONF_TEST_MISSING"), None);
        let mut cfg = Config::DEFAULT;
        assert_eq!(cfg.parse(env_var(b"CHALLOC_CONF_TEST").unwrap(), |_| {}), 0);
        assert_eq!(cfg.narenas, 3);
        assert!(!cfg.tcache);
    }

    #[test]
    fn first_line_of_a_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(std::format!("challoc-conf-{}", std::process::id()));
        let mut buf = [0u8; MAX_TEXT];
        let mut cpath = path.to_str().unwrap().as_bytes().to_vec();
        cpath.push(0);

        std::fs::write(&path, "tcache:false,narenas:2\r\nsecond line is ignored\n").unwrap();
        assert_eq!(
            first_line_of(&cpath, &mut buf),
            Some(&b"tcache:false,narenas:2"[..])
        );
        std::fs::write(&path, "no newline at all").unwrap();
        assert_eq!(
            first_line_of(&cpath, &mut buf),
            Some(&b"no newline at all"[..])
        );
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
        assert_eq!(
            env_var(b"MALLOC_CONF"),
            None,
            "MALLOC_CONF is set; the test assumes a clean environment"
        );
        assert_eq!(
            env_var(b"CHALLOC_CONF"),
            None,
            "CHALLOC_CONF is set; the test assumes a clean environment"
        );
        let mut buf = [0u8; MAX_TEXT];
        if first_line_of(b"/etc/challoc.conf\0", &mut buf).is_none() {
            // The constants whatever the CPU: the L2-derived ladder of
            // design 6.2 ("Cache geometry") was priced and dropped.
            let got = load();
            assert_eq!(got, Config::DEFAULT);
        }
    }
}

/// Kani checks of the `TABLE` accessors, `KEYS` and the `Issue` and
/// `Bytes` impls of `Display` (records under `verus/seams/`).
///
/// `listed` is uninterpreted; the three contracts that name it hold
/// together when it is read as "some `TABLE` entry has this name", which
/// [`listed`] below spells independently of `Key::find`.
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
    fn any_key<'a>(buf: &'a [u8; KEY_MAX]) -> &'a [u8] {
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

    /// S100 `Config::set_listed`: under `requires old(self).wf()`,
    /// `r is None <==> !listed(key)`, an unchanged config on `None`, and
    /// `wf` kept, for every well-formed config, every entry's name and
    /// every key up to `KEY_MAX` bytes, and every value up to `VALUE_MAX`
    /// bytes.
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

    /// `Thp::name`, the same bytes through `CStr::from_bytes_with_nul`
    /// (Kani 0.68 has no C string literals; the original indexes
    /// `THP_NAMES`, three of them).
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    fn stub_thp_name(t: Thp) -> &'static CStr {
        let b: &'static [u8] = match t {
            Thp::Never => b"never\0",
            Thp::Auto => b"auto\0",
            Thp::Always => b"always\0",
        };
        CStr::from_bytes_with_nul(b).unwrap()
    }

    /// `PercpuArena::name`, as [`stub_thp_name`].
    #[allow(dead_code)] // As `stub_thp_name`.
    fn stub_percpu_arena_name(p: PercpuArena) -> &'static CStr {
        let b: &'static [u8] = match p {
            PercpuArena::Disabled => b"disabled\0",
            PercpuArena::Percpu => b"percpu\0",
            PercpuArena::Phycpu => b"phycpu\0",
        };
        CStr::from_bytes_with_nul(b).unwrap()
    }

    /// S101 `Config::get`: `r is Some <==> listed(key)`, for every config
    /// and every key `any_key` gives. The two `name` methods the `thp` and
    /// `percpu_arena` readers call are stubbed (above).
    #[kani::proof]
    #[kani::unwind(25)]
    #[kani::stub(Thp::name, stub_thp_name)]
    #[kani::stub(PercpuArena::name, stub_percpu_arena_name)]
    fn get_contract() {
        let kbuf: [u8; KEY_MAX] = kani::any();
        let key = any_key(&kbuf);
        let c = any_config();
        assert_eq!(c.get(key).is_some(), listed(key));
    }
    /// S102 `KEYS`: every name in it is `listed`.
    #[kani::proof]
    #[kani::unwind(25)]
    fn keys_are_listed() {
        let mut i = 0;
        while i < KEYS.len() {
            assert!(listed(KEYS[i].as_bytes()));
            i += 1;
        }
    }

    /// An arbitrary `Issue` over byte strings of up to 3 bytes.
    fn any_issue<'a>(k: &'a [u8], v: &'a [u8]) -> Issue<'a> {
        match kani::any::<u8>() % 6 {
            0 => Issue::Malformed(v),
            1 => Issue::UnknownKey(k),
            2 => Issue::BadValue { key: k, value: v },
            3 => Issue::OutOfRange { key: k, value: v },
            4 => Issue::Rounded {
                key: k,
                value: v,
                to: kani::any(),
            },
            _ => Issue::Inert(k),
        }
    }

    /// S103 `DisplaySpecImpl for Issue` (`fmt_req` true): every variant,
    /// over arbitrary byte strings up to 3 bytes and every `to`.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_issue() {
        let a: [u8; 3] = kani::any();
        let b: [u8; 3] = kani::any();
        let (i, j): (usize, usize) = (kani::any(), kani::any());
        kani::assume(i <= 3 && j <= 3);
        formats(&any_issue(&a[..i], &b[..j]));
    }

    /// S104 `DisplaySpecImpl for Bytes` (`fmt_req` true): every byte string
    /// up to 4 bytes, and one of 65 arbitrary bytes (past the 64 it shows,
    /// so the `...` branch runs).
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
