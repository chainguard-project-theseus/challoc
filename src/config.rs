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
//!
//! Three tables name the keys. [`TABLE`] holds the tunables the allocator
//! reads, jemalloc's and challoc's own (`thp_process`, `prefault`,
//! `spin_hint`, `slice_ext`, ...): one [`Key`] per tunable with its name,
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

use core::fmt::{self, Write as _};

use crate::heap::{MAX_CAP, MIN_CAP};

/// Largest accepted `tcache_nslots_small_max`. A cache array is allocated
/// from the metadata region as one object, and `Meta::alloc` serves layouts
/// up to 1 MiB; 65536 slots is 512 KiB plus the header, well inside that.
pub const MAX_NSLOTS: u32 = 65_536;
/// Default `tcache_max`: jemalloc's. Slots of classes above it are never
/// cached; the acceptance probe (`bench/results/2026-09-18-*-gce-arena-rss.md`)
/// found 8 cached 40 KiB slots per (thread, arena) pair holding 70 MiB of
/// xmalloc-test's RSS and most of mstress's excess at 16 arenas.
pub const DEFAULT_TCACHE_MAX: usize = 32 * 1024;
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
/// Largest `huge_retain_max`: the preferred huge region (2^40 bytes,
/// `layout::LG_HUGE_PREFERRED`); no run can be larger, so anything above
/// means "retain every freed run for decay".
pub const MAX_HUGE_RETAIN_MAX: usize = 1 << crate::layout::LG_HUGE_PREFERRED;
/// `spin_hint` values (design 5): what a contended lock waits with between
/// two polls. `auto` is the CPU probe's choice (`sys::cpu::spin_hint`:
/// `umwait` with WAITPKG, else the baseline; never `wfe`, which lost 3%
/// and a quarter of the RSS on xmalloc-test on the Altra and the Axion,
/// `bench/results/2026-09-19-lane-c-spin-and-geometry.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinChoice {
    Auto,
    Baseline,
    Umwait,
    Wfe,
}

impl SpinChoice {
    pub const fn name(self) -> &'static str {
        match self {
            SpinChoice::Auto => "auto",
            SpinChoice::Baseline => "baseline",
            SpinChoice::Umwait => "umwait",
            SpinChoice::Wfe => "wfe",
        }
    }

    /// The lock's hint for an explicit choice; `None` for `auto`.
    pub const fn hint(self) -> Option<crate::lock::SpinHint> {
        use crate::lock::SpinHint;
        match self {
            SpinChoice::Auto => None,
            SpinChoice::Baseline => Some(SpinHint::Baseline),
            SpinChoice::Umwait => Some(SpinHint::Umwait),
            SpinChoice::Wfe => Some(SpinHint::Wfe),
        }
    }
}

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

impl Thp {
    pub const fn name(self) -> &'static str {
        match self {
            Thp::Never => "never",
            Thp::Auto => "auto",
            Thp::Always => "always",
        }
    }
}

/// `percpu_arena` values (design 15): whether the
/// caches and arenas are keyed by the running CPU's rseq `mm_cid` instead
/// of by thread. The mode needs the runtime probe to pass (`rseq::probe`,
/// checked at run time, never assumed from a header constant or a library
/// version): glibc exports `__rseq_offset`, `__rseq_size` and
/// `__rseq_flags` (2.35+) and the registered `__rseq_size` is at least 28,
/// so the area covers the `mm_cid` field the kernel writes on 6.3+ with
/// `CONFIG_SCHED_MM_CID` (glibc reports the kernel's feature size there
/// since 2.41, docs/research.md). jemalloc's spellings are accepted as
/// aliases (`disabled` is `off`, `phycpu` is `percpu`: the nearest mode, a
/// concurrency id is not a physical CPU) so a conf string written against
/// jemalloc, which this key used to accept with any value, still parses
/// clean under `abort_conf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercpuArena {
    /// The default: thread caches (`heap::PercpuState::OffAuto`, design 12
    /// and 15). The per-CPU mode is the opt-in below; nothing is reported.
    Auto,
    /// The request is forced: on when the probe passes; when it fails the
    /// allocator still falls back to thread caches and the feature report
    /// notes that `percpu_arena:percpu` was asked for and not available.
    Percpu,
    /// Thread caches whatever the probe says, no dispatch: byte for byte
    /// the allocator without the key.
    Off,
}

impl PercpuArena {
    pub const fn name(self) -> &'static str {
        match self {
            PercpuArena::Auto => "auto",
            PercpuArena::Percpu => "percpu",
            PercpuArena::Off => "off",
        }
    }

    /// Whether the per-CPU mode runs, given the probe's verdict
    /// (`mm_cid_usable`: the registered `__rseq_size` covers `mm_cid`).
    /// `percpu` follows the probe (a forced request cannot read a field
    /// the kernel does not write); `off` never runs. `auto` answers as
    /// `percpu` here, but `heap::Percpu::build` returns thread caches for
    /// `auto` before asking, so that arm is reached by the unit tests only.
    #[inline]
    pub const fn enabled(self, mm_cid_usable: bool) -> bool {
        match self {
            PercpuArena::Auto | PercpuArena::Percpu => mm_cid_usable,
            PercpuArena::Off => false,
        }
    }

    /// Whether a failed probe is worth a note in the report: only for the
    /// explicit request, `auto` falls back silently.
    #[inline]
    pub const fn forced(self) -> bool {
        matches!(self, PercpuArena::Percpu)
    }
}

/// `slice_ext` values (design 17, "Time slice extension"): whether a lock
/// holder asks the kernel, through the rseq
/// `slice_ctrl` bytes, to defer a preemption until the release
/// (`slice.rs`). The runtime probe must pass: glibc's registered
/// `__rseq_size >= 32`, the kernel's `RSEQ_CS_FLAG_SLICE_EXT_AVAILABLE`
/// bit in the area (kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION`), the
/// `prctl` enable and the `rseq_slice_yield` syscall; nothing is assumed
/// from a header constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceExt {
    /// The default: on when the probe passes, nothing otherwise, nothing
    /// reported either way.
    Auto,
    /// The request is forced: on when the probe passes; when it fails
    /// nothing is requested (a field the kernel does not read cannot be
    /// forced) and the report notes that `slice_ext:on` was refused.
    On,
    /// Never requested, whatever the kernel: byte for byte the lock
    /// without the key but for one relaxed load per hold.
    Off,
}

impl SliceExt {
    pub const fn name(self) -> &'static str {
        match self {
            SliceExt::Auto => "auto",
            SliceExt::On => "on",
            SliceExt::Off => "off",
        }
    }

    /// Whether the probe is consulted at all.
    #[inline]
    pub const fn wanted(self) -> bool {
        !matches!(self, SliceExt::Off)
    }

    /// Whether a failed probe is worth a note in the report.
    #[inline]
    pub const fn forced(self) -> bool {
        matches!(self, SliceExt::On)
    }
}

/// `thp_process` values (design 12 and 17, the prctl half): the
/// process-wide THP policy challoc asks the kernel for at
/// init, `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)`,
/// so that on an `always` host (Chainguard OS) only the regions challoc
/// `MADV_HUGEPAGE`s under `thp:auto|always` get huge pages and the rest
/// of the process (libc's own mappings, the stack, other allocators) does
/// not. Kernel 6.18+ accepts the flag; it is probed by the call's return
/// (`EINVAL` below), never assumed from a header constant. Off by default
/// because it is process-wide: a library must not change the THP policy
/// of the program that loaded it unless asked. The call itself lives in
/// `thp.rs`; this key only says whether to issue it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThpProcess {
    /// The default: no `prctl` is issued; the process keeps the policy it
    /// inherited (byte for byte the allocator without the key).
    Default,
    /// Issue the `prctl` at init; where the kernel refuses it the process
    /// keeps its inherited policy and the report says so.
    Advised,
}

impl ThpProcess {
    pub const fn name(self) -> &'static str {
        match self {
            ThpProcess::Default => "default",
            ThpProcess::Advised => "advised",
        }
    }

    /// Whether init issues the `prctl` at all (`advised`); `default` never
    /// calls it, so a kernel without the flag is never even asked.
    #[inline]
    pub const fn requested(self) -> bool {
        matches!(self, ThpProcess::Advised)
    }
}

/// Every tunable of design section 12 with its compiled default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub background_thread: bool,
    /// Prefaulted fresh slabs: the background thread commits and
    /// `MADV_POPULATE_WRITE`s (5.14+, probed; fallback: touch) the next
    /// fresh slabs of every class that refilled since its last pass, as
    /// many as it refilled in slabs (at most `purge::prefault::MAX_AHEAD`)
    /// and within the cgroup headroom, so the claim path's first write
    /// into a fresh slab does not take the page faults. Needs the
    /// background thread (`background_thread:true`): without it the key
    /// changes nothing ([`Config::prefault_active`]). Off by default (the
    /// decision on the Valkey soak's write-phase p99 and main-thread page
    /// faults, design 12). Read at init by the thread (`purge.rs`) and the
    /// commit path (`class/`).
    pub prefault: bool,
    /// `-1` means never purge.
    pub dirty_decay_ms: i64,
    /// Accepted for compatibility; nothing reads it.
    pub muzzy_decay_ms: i64,
    /// Automatic arenas. 0, the default, means `min(4 * ncpus, 64)`,
    /// resolved by [`Config::effective_narenas`] at init; heaps are bound
    /// to the least-loaded automatic arena (`arena.rs`), so a program with
    /// `t` live threads ever touches `min(t, narenas)` of them.
    pub narenas: u32,
    /// Per-CPU caches and arenas keyed by rseq `mm_cid` (design 15,
    /// [`PercpuArena`]): `auto`, the default, is thread caches; `percpu`
    /// turns the mode on where the runtime probe allows and otherwise keeps
    /// thread caches with a note in the report; `off` is thread caches with
    /// no dispatch. Read at init through this field, as `thp`
    /// and `pac_pointers` are; the probe itself lives with the heap.
    pub percpu_arena: PercpuArena,
    pub tcache: bool,
    /// Largest class size whose slots go through thread caches; larger
    /// slab classes are served straight from the bitmaps (jemalloc's
    /// `tcache_max`).
    pub tcache_max: usize,
    pub tcache_nslots_small_max: u32,
    pub lg_tcache_nslots_mul: i32,
    pub tcache_gc_interval_ms: i64,
    /// Every `2^shift` pops of a class are served from the lowest-address
    /// cached slot instead of the top of the stack (design 6.4); 0 means
    /// never.
    pub tcache_compact_shift: u32,
    /// Transparent huge pages for the regions (`thp.rs`, design 7, 10, 17).
    pub thp: Thp,
    /// The process-wide THP policy asked for at init ([`ThpProcess`],
    /// design 17): `default` issues nothing,
    /// `advised` issues `prctl(PR_SET_THP_DISABLE, 1,
    /// PR_THP_DISABLE_EXCEPT_ADVISED)` where the kernel accepts it (6.18+,
    /// probed by the return). Read at init by `thp.rs`, as `thp` is.
    pub thp_process: ThpProcess,
    /// The lock's spin hint (design 5, `lock::SpinHint`): `auto` leaves the
    /// CPU probe's choice; a name installs that hint (`umwait` only where
    /// the CPU has WAITPKG, `wfe` only on aarch64 under the timer event
    /// stream; else reported and left at `auto`).
    pub spin_hint: SpinChoice,
    /// The rseq time slice extension for lock holders (design 17,
    /// `slice.rs`, [`SliceExt`]): `auto` follows the runtime probe, `on`
    /// forces the request (nothing, plus a report note, when the probe
    /// fails), `off` never requests. Applied at the end of `load`
    /// (`Config::apply_slice_ext`) on the init thread.
    pub slice_ext: SliceExt,
    /// `MADV_FREE` for every purge (the override of the two-advice purge,
    /// design 8).
    pub lazy_free: bool,
    /// `MADV_FREE` for purges under 70% of a known cgroup limit (design 8
    /// and 11); off makes every purge `MADV_DONTNEED` unless `lazy_free`.
    pub lazy_band: bool,
    pub guards: bool,
    /// One guard per N slabs of 64 KiB (per N * 64 KiB of slabs for the
    /// classes with smaller slabs, see `heap::guard_interval_for`); 0 means
    /// none. A power of two, so the sized free's guard test is a mask
    /// (`SlabTable::is_guard_slab`): a configured
    /// value that is not one rounds up to the next power of two, reported
    /// as [`Issue::Rounded`] (an abort under `abort_conf`) and readable
    /// through `opt.guard_slab_interval`; 1 is out of range (clamps to 2).
    pub guard_slab_interval: u32,
    /// Freed huge runs with more usable bytes than this are purged the
    /// moment they are freed instead of waiting for decay
    /// (`Huge::set_retain_max`, design 7).
    pub huge_retain_max: usize,
    /// The huge tier's own decay period (design 7): the window over which
    /// a freed huge run at or under `huge_retain_max` goes from `Empty` to
    /// purged. `None`, the default, follows arena 0's period
    /// (`dirty_decay_ms`, and `arena.0.dirty_decay_ms` at run time), so
    /// the huge extents decay with the slabs; `Some(ms)` is the huge
    /// tier's own window (`Shared::huge_decay_ns`, read by the decay
    /// driver), `Some(0)` purges a freed run at once (`Huge::free_run`,
    /// as `huge_retain_max:0`) and `Some(-1)` never purges one.
    /// `opt.huge_dirty_decay_ms` reads the period in force at init:
    /// `dirty_decay_ms` when unset.
    pub huge_dirty_decay_ms: Option<i64>,
    /// Clear a freed block before it is cached or released (a slot's
    /// class-size bytes on the plain and sized free paths, a huge run's
    /// usable bytes): `Shared::push_flags`, `heap::zero_slot`,
    /// `Shared::zero_huge`. Off by default: a `memset` per free is more
    /// than the 3% budget on Valkey's 1 KiB values (priced in
    /// `bench/results/2026-09-18-*-gce-tunables.md`).
    pub zero_on_free: bool,
    /// Sign the thread caches' slot pointers and the heap pointer in TLS
    /// with `pacda` where the CPU and kernel expose pointer authentication
    /// (`HWCAP_PACA`/`PACG`; `sys::pac`, design 6.1, 6.2 and 17).
    /// `Shared::push_flags` bit `PUSH_PAC`. Opt-in:
    /// an entry's two PAC instructions (sign in, authenticate out) are 11%
    /// of xmalloc-test's cross-thread frees and 1..2% of larson on Axion,
    /// over the 1% budget for a default-on hardening
    /// (`bench/results/2026-09-19-lane-a-pac-pointers.md`); inert where
    /// the extension is absent (x86_64, Altra).
    pub pac_pointers: bool,
    /// Cache refills and run rebuilds claim from a random bitmap word and
    /// shuffle the batch with a per-heap xorshift seeded from `getrandom`
    /// on first use (`ClassState::claim_random`, `Heap::claim`), so slots
    /// are handed out in a random order instead of by ascending address.
    /// Off by default: it fights the compaction of design 6.4 (the
    /// scheduled pop then serves a random slot of the low slab, not its
    /// lowest). Slow paths only.
    pub slot_randomize: bool,
    pub double_free_scan: bool,
    pub abort_conf: bool,
    /// Read the memory cgroup (design 11, `sys::cgroup`): the effective
    /// limits, `memory.current`, `memory.events` and the PSI trigger, for
    /// `stats.cgroup.*`, the reports and the policy of `policy.rs` that
    /// acts on the figures. `false` reads nothing.
    pub cgroup: bool,
    /// Debug mode: every slot or page handed out as "known zero" (`Fresh`
    /// or `Purged` memory behind `calloc`) is read at its first and last
    /// word under the class or huge lock and a non-zero word aborts with
    /// the slab's prior state and the discard backend. Slow paths only.
    pub check_zero: bool,
    /// An arena whose slab group of a class is used up takes an idle group
    /// of that class from another arena before it commits a fresh one
    /// (design 13.3, `ClassState::adopt_idle_group`). `false`, the
    /// default, keeps every group with the arena that first committed it:
    /// the handoff moved nothing on the 16-arena mstress row it was built
    /// for and cost that row 2.4% of RSS on aarch64
    /// (`bench/results/2026-09-18-{aarch64,x86_64}-gce-group-handoff.md`);
    /// it takes a fifth off xmalloc-test's RSS, so it stays as an opt-in.
    /// Slow paths only.
    pub group_handoff: bool,
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
        prefault: false,
        dirty_decay_ms: 10_000,
        muzzy_decay_ms: 0,
        narenas: 0,
        percpu_arena: PercpuArena::Auto,
        tcache: true,
        tcache_max: DEFAULT_TCACHE_MAX,
        tcache_nslots_small_max: MAX_CAP,
        lg_tcache_nslots_mul: 1,
        tcache_gc_interval_ms: 10_000,
        tcache_compact_shift: 3,
        thp: Thp::Never,
        thp_process: ThpProcess::Default,
        spin_hint: SpinChoice::Auto,
        slice_ext: SliceExt::Auto,
        lazy_free: false,
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
        group_handoff: false,
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

    /// Set one key from its textual value: a [`KEYS`] entry's parser
    /// ([`TABLE`]), an inert key ([`INERT_KEYS`]: validated as a bool, then
    /// [`Issue::Inert`]), an ignored key ([`IGNORED_KEYS`]: any value), else
    /// [`Issue::UnknownKey`]. An out-of-range number is clamped *and*
    /// returned as [`Issue::OutOfRange`].
    pub fn set<'a>(&mut self, key: &'a [u8], value: &'a [u8]) -> Result<(), Issue<'a>> {
        if let Some(k) = Key::find(key) {
            return (k.parse)(self, key, value);
        }
        if is_inert(key) {
            // jemalloc's `stats_print` (report at exit): no field, since
            // there is nothing to print at exit; validated as a bool and
            // reported as inert so `abort_conf` does not let a program
            // expect the report.
            parse_bool(value).ok_or(Issue::BadValue { key, value })?;
            return Err(Issue::Inert(key));
        }
        if IGNORED_KEYS.iter().any(|k| k.as_bytes() == key) {
            // jemalloc keys accepted for compatibility and ignored: their
            // values are not validated because we do nothing with them.
            return Ok(());
        }
        Err(Issue::UnknownKey(key))
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`: the
    /// [`TABLE`] entry's reader. `None` for keys that are only accepted
    /// and ignored, for inert keys and for unknown keys.
    pub fn get(&self, key: &[u8]) -> Option<Value> {
        Key::find(key).map(|k| (k.read)(self))
    }

    /// Whether `class` is a *shared* class: larger than `tcache_max`, so no
    /// heap caches it and the default path (`Heap::alloc_slow`) allocates
    /// it from arena 0 whatever the thread's arena (design 13.3): its slabs
    /// are 1..4 MiB and one partial slab per class per arena, times 16
    /// arenas, was 60 MiB of mstress's RSS
    /// (`bench/results/2026-09-18-aarch64-gce-arena-rss.md`). Explicit
    /// `MALLOCX_ARENA` allocations keep their arena.
    #[inline]
    pub fn shared_class(&self, class: usize) -> bool {
        crate::layout::CLASSES[class].size() > self.tcache_max
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
    pub fn cap_for(&self, home: u16, arena: u16, class: usize) -> u32 {
        if !self.tcache || self.shared_class(class) || (arena != home && class >= crate::layout::NBINS) {
            return 0;
        }
        ladder(class, self.tcache_nslots_small_max, self.lg_tcache_nslots_mul)
    }

    /// [`cap_for`] for a heap's own arena.
    ///
    /// [`cap_for`]: Config::cap_for
    pub fn cache_cap(&self, class: usize) -> u32 {
        self.cap_for(0, 0, class)
    }

    /// Install the configuration's spin hint (design 5): nothing for `auto`
    /// (the probe's suggestion stands); `umwait` needs WAITPKG on this CPU
    /// and `wfe` an aarch64 kernel with the timer event stream
    /// (`event_stream`), else [`Issue::Unavailable`] is returned, the key
    /// reset to `auto` and nothing installed; `baseline` always installs.
    pub fn apply_spin_hint(&mut self, cpu: &crate::sys::cpu::Cpu, event_stream: bool) -> Result<(), Issue<'static>> {
        use crate::lock::SpinHint;
        use crate::sys::cpu::Isa;
        let Some(hint) = self.spin_hint.hint() else { return Ok(()) };
        let available = match hint {
            SpinHint::Umwait => matches!(cpu.isa, Isa::X86(x) if x.waitpkg),
            SpinHint::Wfe => matches!(cpu.isa, Isa::Arm(_)) && event_stream,
            SpinHint::Baseline => true,
        };
        if !available {
            let value = self.spin_hint;
            self.spin_hint = SpinChoice::Auto;
            return Err(Issue::Unavailable { key: b"spin_hint", value: value.name().as_bytes() });
        }
        crate::lock::set_spin_hint(hint);
        Ok(())
    }

    /// Install the configuration's time slice extension choice (design
    /// 17, `slice::configure`): probe, enable the calling thread, publish.
    /// `off` turns it off; `auto` and `on` follow the probe, and only `on`
    /// makes a failed probe visible (in the report, not as an issue: a
    /// kernel without the option is the documented fallback, not a
    /// configuration error, so `abort_conf` does not refuse it). Returns
    /// the verdict.
    pub fn apply_slice_ext(&self) -> Result<(), crate::slice::Verdict> {
        crate::slice::configure(self.slice_ext.wanted(), self.slice_ext.forced())
    }

    /// The process-wide THP policy to ask for at init (`thp_process`,
    /// [`ThpProcess`]); `thp.rs` issues the `prctl` when
    /// `thp_process().requested()`.
    #[inline]
    pub const fn thp_process(&self) -> ThpProcess {
        self.thp_process
    }

    /// The `prefault` key as set: whether prefaulted fresh slabs were asked
    /// for. See [`Config::prefault_active`] for whether a pass can run.
    #[inline]
    pub const fn prefault(&self) -> bool {
        self.prefault
    }

    /// Whether the prefault pass runs: `prefault:true` and the background
    /// thread that runs it (`background_thread:true`). `prefault` without
    /// the thread is stored and readable (`opt.prefault`) but does nothing,
    /// as `tcache_max` does nothing without `tcache`; the report can say
    /// so from this method.
    #[inline]
    pub const fn prefault_active(&self) -> bool {
        self.prefault && self.background_thread
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

    /// The number of automatic arenas actually used: `narenas` when set,
    /// otherwise jemalloc's `4 * ncpus` capped at [`MAX_AUTO_ARENAS`]
    /// (`ncpus` is the affinity mask's width, `sys::ncpus`; a failed probe
    /// reports 1, so the fallback is 4 arenas). The cap bounds the arena
    /// table walk `Arenas::assign` does per heap creation and the number
    /// of partial slab sets a thread-heavy program can hold; with the
    /// least-loaded assignment only `min(live threads, narenas)` arenas
    /// are ever built (`bench/results/2026-09-17-*-gce-thread-affinity.md`).
    pub fn effective_narenas(&self, ncpus: u32) -> u32 {
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
    pub fn huge_decay_ms(&self) -> i64 {
        self.huge_dirty_decay_ms.unwrap_or(self.dirty_decay_ms)
    }
}

/// Cap of the automatic `narenas` default (design section 13.3).
pub const MAX_AUTO_ARENAS: u32 = 64;

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

/// A bool key: `true` / `false` into `slot`.
fn bool_into<'a>(slot: &mut bool, key: &'a [u8], value: &'a [u8]) -> Result<(), Issue<'a>> {
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
    store: fn(&mut Config, i64),
) -> Result<(), Issue<'a>> {
    let (v, clamped) = clamped(key, value, min, max)?;
    store(cfg, v);
    if clamped { Err(Issue::OutOfRange { key, value }) } else { Ok(()) }
}

/// A named-value key: `value` looked up in `names` (aliases are extra
/// rows), anything else [`Issue::BadValue`].
fn pick<'a, T: Copy>(names: &[(&str, T)], key: &'a [u8], value: &'a [u8]) -> Result<T, Issue<'a>> {
    names.iter().find(|(n, _)| n.as_bytes() == value).map(|&(_, t)| t).ok_or(Issue::BadValue { key, value })
}

/// A [`TABLE`] entry for a bool key stored in `field`.
macro_rules! bool_key {
    ($name:literal, $field:ident) => {
        Key { name: $name, parse: |c, k, v| bool_into(&mut c.$field, k, v), read: |c| Value::Bool(c.$field) }
    };
}

/// Every tunable, in table order (the order of the `opt.` namespace, the
/// report and `KEYS`). A new key is one entry here plus its field; the
/// tests check that every entry reads back, round-trips through `set`
/// and is in no other table.
pub const TABLE: &[Key] = &[
    bool_key!("background_thread", background_thread),
    bool_key!("prefault", prefault),
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
        // jemalloc's `phycpu` and `disabled` spellings are aliases.
        name: "percpu_arena",
        parse: |c, k, v| {
            c.percpu_arena = pick(
                &[
                    ("auto", PercpuArena::Auto),
                    ("percpu", PercpuArena::Percpu),
                    ("phycpu", PercpuArena::Percpu),
                    ("off", PercpuArena::Off),
                    ("disabled", PercpuArena::Off),
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
        parse: |c, k, v| num(c, k, v, 0, crate::layout::MEDIUM_MAX as i64, |c, v| c.tcache_max = v as usize),
        read: |c| Value::Size(c.tcache_max),
    },
    Key {
        name: "tcache_nslots_small_max",
        parse: |c, k, v| num(c, k, v, MIN_CAP as i64, MAX_NSLOTS as i64, |c, v| c.tcache_nslots_small_max = v as u32),
        read: |c| Value::Uint(c.tcache_nslots_small_max as u64),
    },
    Key {
        name: "lg_tcache_nslots_mul",
        parse: |c, k, v| {
            num(c, k, v, LG_NSLOTS_MUL_MIN as i64, LG_NSLOTS_MUL_MAX as i64, |c, v| c.lg_tcache_nslots_mul = v as i32)
        },
        read: |c| Value::Int(c.lg_tcache_nslots_mul as i64),
    },
    Key {
        name: "tcache_gc_interval_ms",
        parse: |c, k, v| num(c, k, v, 0, MAX_INTERVAL_MS, |c, v| c.tcache_gc_interval_ms = v),
        read: |c| Value::Int(c.tcache_gc_interval_ms),
    },
    Key {
        name: "tcache_compact_shift",
        parse: |c, k, v| num(c, k, v, 0, MAX_COMPACT_SHIFT as i64, |c, v| c.tcache_compact_shift = v as u32),
        read: |c| Value::Uint(c.tcache_compact_shift as u64),
    },
    Key {
        // `default` is jemalloc's spelling of `never`.
        name: "thp",
        parse: |c, k, v| {
            c.thp = pick(
                &[("never", Thp::Never), ("default", Thp::Never), ("auto", Thp::Auto), ("always", Thp::Always)],
                k,
                v,
            )?;
            Ok(())
        },
        read: |c| Value::Str(c.thp.name()),
    },
    Key {
        name: "thp_process",
        parse: |c, k, v| {
            c.thp_process = pick(&[("default", ThpProcess::Default), ("advised", ThpProcess::Advised)], k, v)?;
            Ok(())
        },
        read: |c| Value::Str(c.thp_process.name()),
    },
    Key {
        name: "spin_hint",
        parse: |c, k, v| {
            c.spin_hint = pick(
                &[
                    ("auto", SpinChoice::Auto),
                    ("baseline", SpinChoice::Baseline),
                    ("umwait", SpinChoice::Umwait),
                    ("wfe", SpinChoice::Wfe),
                ],
                k,
                v,
            )?;
            Ok(())
        },
        read: |c| Value::Str(c.spin_hint.name()),
    },
    Key {
        name: "slice_ext",
        parse: |c, k, v| {
            c.slice_ext = pick(&[("auto", SliceExt::Auto), ("on", SliceExt::On), ("off", SliceExt::Off)], k, v)?;
            Ok(())
        },
        read: |c| Value::Str(c.slice_ext.name()),
    },
    bool_key!("lazy_free", lazy_free),
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
            let r = num(c, k, v, 0, MAX_GUARD_SLAB_INTERVAL as i64, |c, v| c.guard_slab_interval = v as u32);
            if c.guard_slab_interval == 1 {
                c.guard_slab_interval = 2;
                return Err(Issue::OutOfRange { key: k, value: v });
            }
            if c.guard_slab_interval != 0 && !c.guard_slab_interval.is_power_of_two() {
                c.guard_slab_interval = c.guard_slab_interval.next_power_of_two();
                return Err(Issue::Rounded { key: k, value: v, to: c.guard_slab_interval as u64 });
            }
            r
        },
        read: |c| Value::Uint(c.guard_slab_interval as u64),
    },
    Key {
        name: "huge_retain_max",
        parse: |c, k, v| num(c, k, v, 0, MAX_HUGE_RETAIN_MAX as i64, |c, v| c.huge_retain_max = v as usize),
        read: |c| Value::Size(c.huge_retain_max),
    },
    Key {
        // Unset it follows `dirty_decay_ms`, which is what it reads back.
        name: "huge_dirty_decay_ms",
        parse: |c, k, v| num(c, k, v, -1, MAX_INTERVAL_MS, |c, v| c.huge_dirty_decay_ms = Some(v)),
        read: |c| Value::Int(c.huge_decay_ms()),
    },
    bool_key!("zero_on_free", zero_on_free),
    bool_key!("pac_pointers", pac_pointers),
    bool_key!("slot_randomize", slot_randomize),
    bool_key!("double_free_scan", double_free_scan),
    bool_key!("abort_conf", abort_conf),
    bool_key!("cgroup", cgroup),
    bool_key!("check_zero", check_zero),
    bool_key!("group_handoff", group_handoff),
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

/// Every key readable through [`Config::get`], in table order, for the
/// `opt.` namespace (`mallctl` builds its `opt.` nodes from this slice at
/// compile time, by position) and the tests: the names of [`TABLE`].
pub const KEYS: &[&str] = &NAMES;

/// jemalloc keys that are accepted and ignored.
pub const IGNORED_KEYS: &[&str] = &[
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
pub const INERT_KEYS: &[&str] = &["stats_print"];

/// Whether `key` is one of [`INERT_KEYS`].
fn is_inert(key: &[u8]) -> bool {
    INERT_KEYS.iter().any(|k| k.as_bytes() == key)
}

/// Cache capacity ladder: `max` slots for classes up to 128 bytes, halved
/// per doubling of the class size, shifted by `lg_mul - 1` doublings and
/// clamped to `[MIN_CAP, max]`. With `max = MAX_CAP` and `lg_mul = 1` this
/// is the design 6.2 table (`heap::default_cap`); [`Config::cap_for`] adds
/// the rules for which classes are cached at all.
pub const fn ladder(class: usize, max: u32, lg_mul: i32) -> u32 {
    // Sizes are compile-time (only the slab geometry is fixed at init).
    let size = crate::layout::BASE_CLASSES[class].size();
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

/// A readable configuration value (`mallctl` `opt.*`, the report's
/// `opt.` lines). The variant is the key's C type in `mallctl`,
/// jemalloc's for the keys jemalloc has: `Int` is `ssize_t` (the decay
/// times and the other signed keys), `Uint` is `unsigned` (a count or a
/// shift: `narenas`, `tcache_nslots_small_max`, `tcache_compact_shift`,
/// `guard_slab_interval`), `Size` is `size_t` (a byte count:
/// `tcache_max`, `huge_retain_max`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Size(usize),
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
    /// A number the key needs as a power of two; the value was rounded up
    /// to `to` (`guard_slab_interval`).
    Rounded { key: &'a [u8], value: &'a [u8], to: u64 },
    /// One of [`INERT_KEYS`]: parsed, but no field stores it and nothing
    /// reads it.
    Inert(&'a [u8]),
    /// A value this CPU cannot run (`spin_hint:umwait` without WAITPKG);
    /// the key keeps its default.
    Unavailable { key: &'a [u8], value: &'a [u8] },
}

impl fmt::Display for Issue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Malformed(pair) => write!(f, "malformed conf pair \"{}\"", Bytes(pair)),
            Issue::UnknownKey(key) => write!(f, "unknown conf key \"{}\"", Bytes(key)),
            Issue::Unavailable { key, value } => {
                write!(f, "conf value \"{}\" for {} is not available on this CPU; using the default", Bytes(value), Bytes(key))
            }
            Issue::BadValue { key, value } => {
                write!(f, "invalid conf value \"{}\" for {}", Bytes(value), Bytes(key))
            }
            Issue::OutOfRange { key, value } => {
                write!(f, "out-of-range conf value \"{}\" for {} (clamped)", Bytes(value), Bytes(key))
            }
            Issue::Rounded { key, value, to } => {
                write!(f, "conf value \"{}\" for {} is not a power of two (rounded up to {})", Bytes(value), Bytes(key), to)
            }
            Issue::Inert(key) => {
                write!(f, "conf key {} is accepted but has no effect (not implemented)", Bytes(key))
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

    use super::{Config, Issue, SpinChoice};
    use crate::sys;

    /// Longest conf text read from any one source. Longer inputs are cut
    /// here; no real configuration comes close.
    pub const MAX_TEXT: usize = 4096;

    use crate::sys::auxv::{AT_SECURE, getauxval};

    unsafe extern "C" {
        /// glibc's environment pointer. Read directly so no `getenv` runs
        /// inside `malloc`.
        static environ: *const *const c_char;
    }

    /// Read every source in priority order into a `Config`, printing each
    /// problem as `challoc: <source>: <issue>` on stderr. Aborts when
    /// `abort_conf` is set by the time all sources are read and anything was
    /// reported, an inert key included. Never allocates: the file goes
    /// through a stack buffer and the environment is walked in place.
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
        // The configured spin hint (design 5) needs the CPU: the probe
        // again (init's own result is not in reach here; a few `cpuid`s on
        // x86_64, up to six sysfs reads on arm64, under 50 us, and it
        // suggests the same hint twice). Only when a source set the key.
        if cfg.spin_hint != SpinChoice::Auto {
            let cpu = crate::sys::cpu::Cpu::probe(sys::features::page_size());
            if let Err(issue) = cfg.apply_spin_hint(&cpu, crate::sys::cpu::event_stream()) {
                issues += 1;
                report("spin_hint", issue);
            }
        }
        // The time slice extension (design 17): probed and enabled here,
        // on the init thread, whatever the key (`off` publishes "off").
        // Not an issue when refused: the report says why.
        let _ = cfg.apply_slice_ext();
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
            let expect = if bad.is_empty() { "malformed" } else { "invalid conf value \"" };
            assert!(issues[0].contains(expect), "{text}: {}", issues[0]);
        }
    }

    #[test]
    fn defaults_match_the_design_table() {
        let d = Config::DEFAULT;
        assert!(!d.background_thread);
        assert_eq!(d.dirty_decay_ms, 10_000);
        assert_eq!(d.narenas, 0);
        assert_eq!(d.percpu_arena, PercpuArena::Auto, "thread caches; `percpu` is the opt-in");
        assert_eq!(d.slice_ext, SliceExt::Auto, "on where the slice extension probe passes");
        assert!(d.tcache);
        assert_eq!(d.tcache_max, 32 * 1024);
        assert_eq!(d.tcache_nslots_small_max, 1024);
        assert_eq!(d.lg_tcache_nslots_mul, 1);
        assert_eq!(d.tcache_gc_interval_ms, 10_000);
        assert_eq!(d.tcache_compact_shift, 3);
        assert_eq!(d.compact_mask(), 7);
        assert_eq!(clean("tcache_compact_shift:0").compact_mask(), u32::MAX);
        assert_eq!(clean("tcache_compact_shift:20").compact_mask(), (1 << 20) - 1);
        assert_eq!(d.thp, Thp::Never);
        assert_eq!(d.thp_process, ThpProcess::Default, "process-wide, so opt-in");
        assert!(!d.prefault, "opt-in");
        assert!(!d.prefault_active());
        assert!(!d.lazy_free);
        assert!(d.lazy_band);
        assert!(!clean("lazy_band:false").lazy_band);
        assert!(d.guards);
        assert_eq!(d.guard_slab_interval, 8);
        assert_eq!(d.huge_retain_max, 64 << 20);
        assert_eq!(d.huge_retain_max, crate::huge::DEFAULT_RETAIN_MAX);
        assert!(!d.zero_on_free && !d.slot_randomize && !d.double_free_scan && !d.abort_conf && !d.check_zero);
        assert!(!d.pac_pointers, "signed pointers are opt-in (priced in bench/results/2026-09-19-lane-a-pac-pointers.md)");
        assert!(d.cgroup && !d.group_handoff);
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
            "lazy_band",
            "guards",
            "zero_on_free",
            "pac_pointers",
            "slot_randomize",
            "double_free_scan",
            "abort_conf",
            "cgroup",
            "check_zero",
            "group_handoff",
        ];
        for key in keys {
            for v in [true, false] {
                let text = std::format!("{key}:{v}");
                assert_eq!(clean(&text).get(key.as_bytes()), Some(Value::Bool(v)), "{key}:{v}");
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
        assert_eq!(d.effective_narenas(0), 4, "a failed probe counts as one CPU");
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
        let cases: [(&str, i64, &str, i64, &str, i64); 11] = [
            ("dirty_decay_ms", 5000, "-2", -1, "9999999999999999", MAX_INTERVAL_MS),
            ("huge_dirty_decay_ms", 2000, "-2", -1, "9999999999999999", MAX_INTERVAL_MS),
            ("muzzy_decay_ms", 0, "-5", -1, "9999999999999999", MAX_INTERVAL_MS),
            ("narenas", 4, "-1", 0, "99999999999", u32::MAX as i64),
            ("tcache_max", 4096, "-1", 0, "9999999", crate::layout::MEDIUM_MAX as i64),
            ("tcache_nslots_small_max", 1000, "1", MIN_CAP as i64, "1000000", MAX_NSLOTS as i64),
            ("lg_tcache_nslots_mul", 3, "-17", -16, "17", 16),
            ("tcache_gc_interval_ms", 250, "-1", 0, "9999999999999999", MAX_INTERVAL_MS),
            ("tcache_compact_shift", 4, "-1", 0, "64", MAX_COMPACT_SHIFT as i64),
            ("guard_slab_interval", 2, "-1", 0, "5000000", MAX_GUARD_SLAB_INTERVAL as i64),
            ("huge_retain_max", 1 << 20, "-1", 0, "9999999999999999", MAX_HUGE_RETAIN_MAX as i64),
        ];
        for (key, ok, low, min, high, max) in cases {
            let as_value = |v: i64| match key {
                "tcache_max" | "huge_retain_max" => Value::Size(v as usize),
                "narenas" | "tcache_nslots_small_max" | "guard_slab_interval" | "tcache_compact_shift" => Value::Uint(v as u64),
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
        assert_eq!(clean("thp:auto").thp, Thp::Auto);
        assert_eq!(clean("thp:never").thp, Thp::Never);
        assert_eq!(clean("thp:default").thp, Thp::Never);
        assert_eq!(clean("thp:always").get(b"thp"), Some(Value::Str("always")));
        assert_eq!(clean("thp:auto").get(b"thp"), Some(Value::Str("auto")));
        assert_eq!(Config::DEFAULT.thp, Thp::Never);
        let (cfg, issues) = parse("thp:madvise");
        assert_eq!(cfg.thp, Thp::Never);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid conf value \"madvise\" for thp"));
        // Accepted under abort_conf.
        assert_eq!(clean("thp:auto,abort_conf:true").thp, Thp::Auto);
    }

    /// `percpu_arena`: the three values, jemalloc's spellings as aliases,
    /// anything else reported and left at the default, and the helpers
    /// the heap's init reads.
    #[test]
    fn percpu_arena_values() {
        assert!(KEYS.contains(&"percpu_arena"), "a real key, not an ignored jemalloc one");
        for (value, want) in [
            ("auto", PercpuArena::Auto),
            ("percpu", PercpuArena::Percpu),
            ("off", PercpuArena::Off),
            ("phycpu", PercpuArena::Percpu),
            ("disabled", PercpuArena::Off),
        ] {
            let cfg = clean(&std::format!("percpu_arena:{value},abort_conf:true"));
            assert_eq!(cfg.percpu_arena, want, "{value}");
            assert_eq!(cfg.get(b"percpu_arena"), Some(Value::Str(want.name())), "{value}");
            assert!(cfg.abort_conf);
        }
        rejects_bad_values("percpu_arena", &["true", "false", "1", "on", "Auto", "PERCPU", "", "percpux"]);
        // The last pair wins, as for every key.
        assert_eq!(clean("percpu_arena:percpu,percpu_arena:off").percpu_arena, PercpuArena::Off);
        assert_eq!(clean("percpu_arena:\"percpu\"").percpu_arena, PercpuArena::Percpu);
        // The helpers: `auto` and `percpu` follow the probe, `off` never
        // runs; only `percpu` reports a failed probe.
        for (mode, probe_ok, on) in [
            (PercpuArena::Auto, true, true),
            (PercpuArena::Auto, false, false),
            (PercpuArena::Percpu, true, true),
            (PercpuArena::Percpu, false, false),
            (PercpuArena::Off, true, false),
            (PercpuArena::Off, false, false),
        ] {
            assert_eq!(mode.enabled(probe_ok), on, "{}.enabled({probe_ok})", mode.name());
        }
        assert!(PercpuArena::Percpu.forced() && !PercpuArena::Auto.forced() && !PercpuArena::Off.forced());
        for mode in [PercpuArena::Auto, PercpuArena::Percpu, PercpuArena::Off] {
            assert_eq!(clean(&std::format!("percpu_arena:{}", mode.name())).percpu_arena, mode);
        }
    }

    /// `slice_ext`: the three values, anything else reported and left at
    /// the default, `opt.slice_ext` reads back,
    /// and the helpers `slice::configure` is called with.
    #[test]
    fn slice_ext_values() {
        assert!(KEYS.contains(&"slice_ext"));
        for (value, want) in [("auto", SliceExt::Auto), ("on", SliceExt::On), ("off", SliceExt::Off)] {
            let cfg = clean(&std::format!("slice_ext:{value},abort_conf:true"));
            assert_eq!(cfg.slice_ext, want, "{value}");
            assert_eq!(cfg.get(b"slice_ext"), Some(Value::Str(want.name())), "{value}");
            assert_eq!(want.name(), value);
            assert!(cfg.abort_conf);
        }
        rejects_bad_values("slice_ext", &["true", "false", "1", "yes", "Auto", "ON", "", "onn", "percpu"]);
        assert_eq!(clean("slice_ext:on,slice_ext:off").slice_ext, SliceExt::Off);
        assert_eq!(clean("slice_ext:\"on\"").slice_ext, SliceExt::On);
        // `auto` and `on` consult the probe, `off` never; only `on`
        // reports a failed probe.
        assert!(SliceExt::Auto.wanted() && SliceExt::On.wanted() && !SliceExt::Off.wanted());
        assert!(SliceExt::On.forced() && !SliceExt::Auto.forced() && !SliceExt::Off.forced());
    }

    /// `thp_process` (the prctl half): the two values, anything else
    /// reported and left at the default,
    /// `opt.thp_process` reads back, the accessor `thp.rs` decides the
    /// `prctl` on, and `thp` untouched by it (they are separate keys: the
    /// per-region advice and the process-wide policy).
    #[test]
    fn thp_process_values() {
        assert!(KEYS.contains(&"thp_process"));
        for (value, want) in [("default", ThpProcess::Default), ("advised", ThpProcess::Advised)] {
            let cfg = clean(&std::format!("thp_process:{value},abort_conf:true"));
            assert_eq!(cfg.thp_process, want, "{value}");
            assert_eq!(cfg.thp_process(), want, "{value}");
            assert_eq!(cfg.get(b"thp_process"), Some(Value::Str(want.name())), "{value}");
            assert_eq!(want.name(), value);
            assert!(cfg.abort_conf);
            assert_eq!(cfg.thp, Thp::Never, "thp_process leaves thp alone");
        }
        rejects_bad_values("thp_process", &["true", "false", "1", "never", "always", "auto", "Default", "ADVISED", "", "advise"]);
        assert_eq!(clean("thp_process:advised,thp_process:default").thp_process, ThpProcess::Default);
        assert_eq!(clean("thp_process:\"advised\"").thp_process, ThpProcess::Advised);
        // Only `advised` issues the prctl; `default` never asks.
        assert!(ThpProcess::Advised.requested() && !ThpProcess::Default.requested());
        assert!(!Config::DEFAULT.thp_process().requested());
        // The two THP keys compose: the process policy with any per-region
        // advice, and `thp:default` (an alias of `never`) is not this key.
        let cfg = clean("thp:auto,thp_process:advised");
        assert_eq!((cfg.thp, cfg.thp_process), (Thp::Auto, ThpProcess::Advised));
        let cfg = clean("thp:default");
        assert_eq!((cfg.thp, cfg.thp_process), (Thp::Never, ThpProcess::Default));
    }

    /// `prefault`: a bool key, off by default, readable, accepted under
    /// `abort_conf`; `prefault_active` is the
    /// key and the background thread that runs the pass.
    #[test]
    fn prefault_values() {
        assert!(KEYS.contains(&"prefault"));
        for (value, want) in [("true", true), ("false", false)] {
            let cfg = clean(&std::format!("prefault:{value},abort_conf:true"));
            assert_eq!(cfg.prefault, want, "{value}");
            assert_eq!(cfg.prefault(), want, "{value}");
            assert_eq!(cfg.get(b"prefault"), Some(Value::Bool(want)), "{value}");
            assert!(cfg.abort_conf);
            assert!(!cfg.prefault_active(), "no background thread, no pass");
        }
        rejects_bad_values("prefault", &["1", "0", "yes", "on", "True", "", "advised"]);
        assert!(!clean("prefault:true,prefault:false").prefault);
        assert!(clean("prefault:\"true\"").prefault);
        // The pass needs the thread, in either key order.
        assert!(clean("prefault:true,background_thread:true").prefault_active());
        assert!(clean("background_thread:true,prefault:true").prefault_active());
        assert!(!clean("background_thread:true").prefault_active());
        assert!(!clean("background_thread:true,prefault:true,background_thread:false").prefault_active());
    }

    /// The one inert key: a bool by value, no field, unreadable, one
    /// `Issue::Inert` per pair (what `abort_conf` aborts on), and the
    /// report's `is_inert` agrees with the table.
    #[test]
    fn inert_keys_are_reported_and_rejected_under_abort_conf() {
        assert_eq!(INERT_KEYS, &["stats_print"]);
        for key in INERT_KEYS {
            assert!(is_inert(key.as_bytes()), "{key}");
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
            assert!(issues[0].contains("invalid conf value"), "{key}:nonsense: {}", issues[0]);
        }
        for key in KEYS {
            assert!(!is_inert(key.as_bytes()), "{key}");
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
        let (cfg, issues) = parse("narenas:3,thp:always,cgroup:false,abort_conf:true,stats_print:true");
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
        assert_eq!(cfg.huge_dirty_decay_ms, None, "the slab key leaves it following");
        assert_eq!(cfg.huge_decay_ms(), 3000);
        assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(3000)));
        let cfg = clean("huge_dirty_decay_ms:2000,dirty_decay_ms:3000");
        assert_eq!(cfg.huge_dirty_decay_ms, Some(2000));
        assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (2000, 3000));
        assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(2000)));
        assert_eq!(cfg.get(b"dirty_decay_ms"), Some(Value::Int(3000)), "the huge key leaves the slab key alone");
        assert_eq!(clean("huge_dirty_decay_ms:0").huge_dirty_decay_ms, Some(0));
        assert_eq!(clean("huge_dirty_decay_ms:-1").huge_dirty_decay_ms, Some(-1));
        assert_eq!(clean("huge_dirty_decay_ms:-1").get(b"huge_dirty_decay_ms"), Some(Value::Int(-1)));
        // Set to the slab period explicitly it stays pinned there when
        // the slab period later differs.
        let cfg = clean("huge_dirty_decay_ms:10000,dirty_decay_ms:500");
        assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (10_000, 500));
        let (cfg, issues) = parse("huge_dirty_decay_ms:-2");
        assert_eq!(cfg.huge_dirty_decay_ms, Some(-1), "clamped like dirty_decay_ms");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("out-of-range conf value \"-2\" for huge_dirty_decay_ms"), "{}", issues[0]);
    }

    #[test]
    fn huge_retain_max_is_a_byte_count() {
        assert_eq!(clean("huge_retain_max:0").huge_retain_max, 0);
        assert_eq!(clean("huge_retain_max:0x100000").huge_retain_max, 1 << 20);
        assert_eq!(clean("huge_retain_max:1099511627776").huge_retain_max, MAX_HUGE_RETAIN_MAX);
        assert_eq!(clean("huge_retain_max:1").get(b"huge_retain_max"), Some(Value::Size(1)));
        let (cfg, issues) = parse("huge_retain_max:1099511627777");
        assert_eq!(cfg.huge_retain_max, MAX_HUGE_RETAIN_MAX, "clamped to the huge region");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("out-of-range conf value \"1099511627777\" for huge_retain_max"), "{}", issues[0]);
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
        assert_eq!(cfg.narenas, Config::DEFAULT.narenas, "an invalid value leaves the default");
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
        // jemalloc's own `percpu_arena` spellings (the key used to be
        // accepted with any value) still parse clean under `abort_conf`.
        let cfg = clean("percpu_arena:disabled,abort_conf:true");
        assert_eq!(cfg.percpu_arena, PercpuArena::Off);
        assert_eq!(clean("percpu_arena:phycpu,narenas:4").percpu_arena, PercpuArena::Percpu);
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
                Value::Str(s) => s.into(),
            };
            let text = std::format!("{key}:{value}");
            let parsed = clean(&text);
            if *key == "huge_dirty_decay_ms" {
                // Its default is "follow `dirty_decay_ms`" (`None`), which
                // reads back as that period; setting the reading pins it.
                assert_eq!(parsed.get(key.as_bytes()), cfg.get(key.as_bytes()), "{text}");
                assert_eq!(parsed.huge_dirty_decay_ms, Some(cfg.dirty_decay_ms), "{text}");
                assert_eq!(Config { huge_dirty_decay_ms: None, ..parsed }, cfg, "{text} changed another field");
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
                assert_eq!(r, Err(Issue::BadValue { key: key.as_bytes(), value: b"nonsense-value" }), "{key}");
                assert_eq!(cfg.set(key.as_bytes(), b"true"), Err(Issue::Inert(key.as_bytes())), "{key}");
            } else {
                assert_eq!(r, Err(Issue::BadValue { key: key.as_bytes(), value: b"nonsense-value" }), "{key}");
                assert!(cfg.get(key.as_bytes()).is_some(), "{key} is readable");
            }
            let n = [KEYS.contains(key), IGNORED_KEYS.contains(key), INERT_KEYS.contains(key)];
            assert_eq!(n.iter().filter(|&&b| b).count(), 1, "{key} is in exactly one table");
        }
        for unknown in ["nonsense", "tcache_maxx", "Tcache", "stats_prin", "thp_proces", ""] {
            let mut cfg = Config::DEFAULT;
            assert_eq!(cfg.set(unknown.as_bytes(), b"true"), Err(Issue::UnknownKey(unknown.as_bytes())), "{unknown:?}");
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
            assert_eq!(KEYS.iter().filter(|k| **k == key.name).count(), 1, "{} is named once", key.name);
            assert!(!IGNORED_KEYS.contains(&key.name) && !INERT_KEYS.contains(&key.name), "{}", key.name);
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
                    "percpu_arena" | "slice_ext" => "off".into(),
                    "spin_hint" => "baseline".into(),
                    "thp" => "always".into(),
                    "thp_process" => "advised".into(),
                    _ => panic!("{}: a string key with no test value ({s})", key.name),
                },
            };
            let mut cfg = d;
            assert_eq!(cfg.set(key.name.as_bytes(), other.as_bytes()), Ok(()), "{}:{other}", key.name);
            assert_ne!((key.read)(&cfg), (key.read)(&d), "{}:{other} read back through its own entry", key.name);
            assert_eq!(cfg.get(key.name.as_bytes()), Some((key.read)(&cfg)), "{}", key.name);
            for o in TABLE.iter().filter(|o| o.name != key.name) {
                if key.name == "dirty_decay_ms" && o.name == "huge_dirty_decay_ms" {
                    // Unset, the huge period reads as the slab period
                    // (design 7): the one reading that moves with another key.
                    assert_eq!((o.read)(&cfg), (key.read)(&cfg), "huge_dirty_decay_ms follows dirty_decay_ms");
                    continue;
                }
                assert_eq!((o.read)(&cfg), (o.read)(&d), "{}:{other} changed {}", key.name, o.name);
            }
        }
        // The aliases of the named-value keys: one entry, several spellings.
        assert_eq!(clean("thp:default").thp, clean("thp:never").thp);
        assert_eq!(clean("percpu_arena:phycpu").percpu_arena, clean("percpu_arena:percpu").percpu_arena);
        assert_eq!(clean("percpu_arena:disabled").percpu_arena, clean("percpu_arena:off").percpu_arena);
    }

    /// `spin_hint`, the pure part: the four values parse and read back
    /// through `opt.spin_hint`, anything else is refused and leaves the
    /// default, `hint()` maps the explicit choices to the lock's hints and
    /// `auto` to none; `apply_spin_hint` refuses the other architecture's
    /// hint and this one's without its prerequisite (WAITPKG, the event
    /// stream), resets the key to `auto` and installs nothing (the install
    /// itself is `linux_tests::spin_hint_key_installs_where_available`).
    #[test]
    fn spin_hint_values() {
        use crate::lock::SpinHint;
        use crate::sys::cpu::{Arm, CacheSource, Caches, Cpu, Isa, X86};
        assert!(KEYS.contains(&"spin_hint"));
        assert_eq!(Config::DEFAULT.spin_hint, SpinChoice::Auto);
        for (value, want, hint) in [
            ("auto", SpinChoice::Auto, None),
            ("baseline", SpinChoice::Baseline, Some(SpinHint::Baseline)),
            ("umwait", SpinChoice::Umwait, Some(SpinHint::Umwait)),
            ("wfe", SpinChoice::Wfe, Some(SpinHint::Wfe)),
        ] {
            let cfg = clean(&std::format!("spin_hint:{value},abort_conf:true"));
            assert_eq!(cfg.spin_hint, want, "{value}");
            assert_eq!(cfg.get(b"spin_hint"), Some(Value::Str(want.name())), "{value}");
            assert_eq!(want.name(), value);
            assert_eq!(want.hint(), hint, "{value}");
            assert!(cfg.abort_conf);
        }
        rejects_bad_values("spin_hint", &["true", "false", "1", "pause", "Auto", "UMWAIT", "", "wfet", "yield"]);
        assert_eq!(clean("spin_hint:wfe,spin_hint:baseline").spin_hint, SpinChoice::Baseline);
        assert_eq!(clean("spin_hint:\"umwait\"").spin_hint, SpinChoice::Umwait);
        // Unavailable hints: no install, the key back at `auto`, the
        // issue names the value.
        let x86 = Cpu { isa: Isa::X86(X86::default()), caches: Caches::UNKNOWN, cache_source: CacheSource::None };
        let arm = Cpu { isa: Isa::Arm(Arm::default()), caches: Caches::UNKNOWN, cache_source: CacheSource::None };
        let unavailable = |name: &str| std::format!("conf value \"{name}\" for spin_hint is not available on this CPU; using the default");
        for (cpu, event_stream, value) in [
            (&x86, true, "umwait"),
            (&x86, true, "wfe"),
            (&arm, true, "umwait"),
            (&arm, false, "wfe"),
        ] {
            let mut cfg = clean(&std::format!("spin_hint:{value}"));
            let err = cfg.apply_spin_hint(cpu, event_stream).unwrap_err();
            assert_eq!(err, Issue::Unavailable { key: b"spin_hint", value: value.as_bytes() }, "{value}");
            assert_eq!(std::format!("{err}"), unavailable(value));
            assert_eq!(cfg.spin_hint, SpinChoice::Auto, "{value}: reset to auto");
            assert_eq!(cfg.get(b"spin_hint"), Some(Value::Str("auto")));
        }
        // `auto` is never applied: no probe consulted, nothing installed.
        let mut cfg = Config::DEFAULT;
        assert_eq!(cfg.apply_spin_hint(&x86, false), Ok(()));
        assert_eq!(cfg.spin_hint, SpinChoice::Auto);
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
        assert!(cached(&d).count() >= 36 && uncached(&d).count() >= 8, "{} cached, {} above", cached(&d).count(), uncached(&d).count());
        for c in cached(&d) {
            assert_eq!(d.cache_cap(c), ladder(c, MAX_CAP, 1));
            assert_eq!(d.cache_cap(c), crate::heap::default_cap(c), "class {c}");
        }
        for c in uncached(&d) {
            assert_eq!(d.cache_cap(c), 0, "class {c} ({} bytes) is above tcache_max", CLASSES[c].size());
            assert!(crate::heap::default_cap(c) >= MIN_CAP, "the ladder itself never reaches zero");
        }
        // Redis: x4 ladder clamped at 1000, never below MIN_CAP.
        let redis = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
        for c in cached(&redis) {
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
        assert!(cached(&small).all(|c| small.cache_cap(c) <= 16 && small.cache_cap(c) >= MIN_CAP));
        let shrunk = clean("lg_tcache_nslots_mul:0");
        assert_eq!(shrunk.cache_cap(0), 512);
        assert_eq!(shrunk.cache_cap(NCLASSES - 1), 0);
        assert_eq!(shrunk.cache_cap(cached(&shrunk).last().unwrap()), MIN_CAP);
        let grown = clean("lg_tcache_nslots_mul:16,tcache_nslots_small_max:65536");
        assert!(cached(&grown).all(|c| grown.cache_cap(c) == 65536));
        // tcache_max moves the boundary: every slab class, or none.
        let all = clean("tcache_max:262144");
        assert!((0..NCLASSES).all(|c| all.cache_cap(c) >= MIN_CAP));
        assert_eq!(all.cache_cap(NCLASSES - 1), MIN_CAP);
        let none = clean("tcache_max:0");
        assert!((0..NCLASSES).all(|c| none.cache_cap(c) == 0));
        let k4 = clean("tcache_max:4096");
        assert!((0..NCLASSES).all(|c| (k4.cache_cap(c) == 0) == (CLASSES[c].size() > 4096)));
        // Out of range clamps and reports, like every numeric key.
        assert_eq!(clean("tcache_max:32768").tcache_max, 32768);
        // tcache:false turns caching off entirely.
        let off = clean("tcache:false");
        assert!((0..NCLASSES).all(|c| off.cache_cap(c) == 0));
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
            assert_eq!(d.cap_for(3, 3, c), d.cache_cap(c), "class {c}");
            assert_eq!(d.cap_for(3, 3, c) == 0, d.shared_class(c), "class {c}");
            let foreign = d.cap_for(3, 0, c);
            assert_eq!(foreign, if c >= NBINS { 0 } else { d.cache_cap(c) }, "class {c}");
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
        assert_eq!(clean("guard_slab_interval:0").effective_guard_slab_interval(), 0);
        assert_eq!(clean("guard_slab_interval:2").effective_guard_slab_interval(), 2);
        // 1 would leave no slabs to allocate: out of range, clamped to 2.
        let (cfg, issues) = parse("guard_slab_interval:1");
        assert_eq!(cfg.effective_guard_slab_interval(), 2);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("out-of-range conf value \"1\" for guard_slab_interval"), "{}", issues[0]);
        // The interval is a power of two (the sized path's guard test is a
        // mask): anything else rounds up, is reported, and the rounded value
        // is what `opt.guard_slab_interval` returns; the maximum is one.
        for (text, want) in [("3", 4u32), ("5", 8), ("12", 16), ("4194303", 1 << 22)] {
            let (cfg, issues) = parse(&std::format!("guard_slab_interval:{text}"));
            assert_eq!(cfg.effective_guard_slab_interval(), want, "{text}");
            assert_eq!(cfg.get(b"guard_slab_interval"), Some(Value::Uint(want as u64)));
            assert_eq!(issues.len(), 1, "{text}");
            assert_eq!(
                issues[0],
                std::format!("conf value \"{text}\" for guard_slab_interval is not a power of two (rounded up to {want})")
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
            std::env::set_var("CHALLOC_CONF_TEST", "narenas:3,tcache:false");
            std::env::set_var("CHALLOC_CONF_TEST_EMPTY", "");
        }
        assert_eq!(env_var(b"CHALLOC_CONF_TEST"), Some(&b"narenas:3,tcache:false"[..]));
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
            // The constants whatever the CPU: the L2-derived ladder of
            // design 6.2 ("Cache geometry") was priced and dropped.
            let got = load();
            assert_eq!(got, Config::DEFAULT);
            assert_eq!(got.spin_hint, super::SpinChoice::Auto);
            std::eprintln!("config: defaults (spin hint in use {})", crate::lock::spin_hint().name());
        }
    }

    /// The `spin_hint` key against this CPU: `baseline` installs; the
    /// architecture's own hint installs where the CPU and kernel have
    /// what it needs (`umwait`: WAITPKG; `wfe`: the event stream) and is
    /// otherwise reported and leaves the key at `auto`; the other
    /// architecture's hint is always unavailable; the probe's later
    /// suggestion does not override a configured hint. Leaves the process
    /// on the probe's choice again at the end (other tests read it).
    #[test]
    fn spin_hint_key_installs_where_available() {
        use crate::lock::{SpinHint, spin_hint};
        use crate::sys::cpu::{Cpu, Isa, event_stream};
        let _serial = crate::lock::SPIN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cpu = Cpu::probe(crate::sys::features::page_size());
        let auto = spin_hint();
        let unavailable = |name: &str| std::format!("conf value \"{name}\" for spin_hint is not available on this CPU; using the default");
        let mut c = Config::DEFAULT;
        c.parse(b"spin_hint:baseline", |i| panic!("{i}"));
        assert_eq!(c.apply_spin_hint(&cpu, event_stream()), Ok(()));
        assert_eq!(spin_hint(), SpinHint::Baseline);
        // The other architecture's hint.
        let (other, own, own_hint) = if cfg!(target_arch = "aarch64") { ("umwait", "wfe", SpinHint::Wfe) } else { ("wfe", "umwait", SpinHint::Umwait) };
        let mut c = Config::DEFAULT;
        c.parse(std::format!("spin_hint:{other}").as_bytes(), |i| panic!("{i}"));
        assert_eq!(std::format!("{}", c.apply_spin_hint(&cpu, event_stream()).unwrap_err()), unavailable(other));
        assert_eq!(c.spin_hint, SpinChoice::Auto);
        assert_eq!(spin_hint(), SpinHint::Baseline, "nothing installed");
        // This architecture's hint.
        let mut c = Config::DEFAULT;
        c.parse(std::format!("spin_hint:{own}").as_bytes(), |i| panic!("{i}"));
        let can = match cpu.isa {
            Isa::X86(x) => x.waitpkg,
            Isa::Arm(_) => event_stream(),
        };
        if can {
            assert_eq!(c.apply_spin_hint(&cpu, event_stream()), Ok(()));
            assert_eq!(spin_hint(), own_hint);
        } else {
            assert_eq!(std::format!("{}", c.apply_spin_hint(&cpu, event_stream()).unwrap_err()), unavailable(own));
            assert_eq!(c.spin_hint, SpinChoice::Auto);
            assert_eq!(spin_hint(), SpinHint::Baseline, "nothing installed");
        }
        // `wfe` without the event stream is refused too.
        let mut c = Config::DEFAULT;
        c.parse(b"spin_hint:wfe", |i| panic!("{i}"));
        assert_eq!(std::format!("{}", c.apply_spin_hint(&cpu, false).unwrap_err()), unavailable("wfe"));
        std::eprintln!("config: spin_hint auto={} own {own} available={can} event_stream={}", auto.name(), event_stream());
        // Back to the probe's choice for the rest of the suite.
        crate::lock::set_spin_hint(auto);
        assert_eq!(spin_hint(), auto);
    }
}
