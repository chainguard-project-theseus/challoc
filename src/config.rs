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
//! The *inert* keys ([`INERT_KEYS`]: `stats_print`) are parsed but nothing
//! in the allocator reads them (`stats_print` has nothing to print at
//! exit). Each one is reported as [`Issue::Inert`] so that `abort_conf`
//! rejects it and a program cannot believe it turned something on;
//! `malloc_stats_print` marks them "accepted, no effect". `zero_on_free`
//! and `slot_randomize` left the list with Phase 2 task 3, `thp` with Phase
//! 2.5 task 9 (`thp.rs`): all implemented, opt-in and off by default.
//! `percpu_arena` left the *ignored* jemalloc keys ([`IGNORED_KEYS`]) with
//! Phase 2.5 task 8: it is a real key ([`PercpuArena`]) whose jemalloc
//! spellings are accepted as aliases. `thp_process` ([`ThpProcess`], plan
//! Phase 2.5 task 16) and `prefault` (task 11) are challoc keys, opt-in and
//! off by default; this module holds the keys, `thp.rs` the `prctl` and
//! `purge.rs`/`class.rs` the prefault pass.

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

/// `percpu_arena` values (design 15, plan Phase 2.5 task 8): whether the
/// caches and arenas are keyed by the running CPU's rseq `mm_cid` instead
/// of by thread. The mode needs the runtime probe to pass: glibc has
/// registered an rseq area whose `__rseq_size >= 28` covers the `mm_cid`
/// field (kernel 6.3+, glibc 2.41+, `CONFIG_SCHED_MM_CID`); nothing here
/// assumes it from a header constant. jemalloc's spellings are accepted as
/// aliases (`disabled` is `off`, `phycpu` is `percpu`: the nearest mode, a
/// concurrency id is not a physical CPU) so a conf string written against
/// jemalloc, which this key used to accept with any value, still parses
/// clean under `abort_conf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercpuArena {
    /// The default: on when the probe passes, thread caches otherwise;
    /// nothing is reported either way.
    Auto,
    /// The request is forced: on when the probe passes; when it fails the
    /// allocator still falls back to thread caches and the feature report
    /// notes that `percpu_arena:percpu` was asked for and not available.
    Percpu,
    /// Thread caches whatever the probe says: byte for byte the allocator
    /// without the key.
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
    /// `auto` and `percpu` both follow the probe (a forced request cannot
    /// read a field the kernel does not write); `off` never runs.
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

/// `slice_ext` values (design 17, "Time slice extension"; plan Phase 2.5
/// task 15): whether a lock holder asks the kernel, through the rseq
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

/// `thp_process` values (design 12 and 17, plan Phase 2.5 task 16, the
/// prctl half): the process-wide THP policy challoc asks the kernel for at
/// init, `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)`,
/// so that on an `always` host (Chainguard OS) only the regions challoc
/// `MADV_HUGEPAGE`s under `thp:auto|always` get huge pages and the rest
/// of the process (libc's own mappings, the stack, other allocators) does
/// not. Kernel 6.18+ accepts the flag; it is probed by the call's return
/// (`EINVAL` below), never assumed from a header constant. Off by default
/// because it is process-wide: a library must not change the THP policy
/// of the program that loaded it unless asked. The call itself lives in
/// `thp.rs` (lane B); this key only says whether to issue it.
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
    /// Prefaulted fresh slabs (plan Phase 2.5 task 11): the background
    /// thread commits and `MADV_POPULATE_WRITE`s (5.14+, probed; fallback:
    /// touch) the next fresh slab of every class that refilled since its
    /// last pass, one slab per class per pass and within the cgroup
    /// headroom, so the claim path's first write into a fresh slab does
    /// not take the page faults. Needs the background thread
    /// (`background_thread:true`): without it the key changes nothing
    /// ([`Config::prefault_active`]). Off by default until measured (the
    /// Valkey soak's write-phase p99 and main-thread page faults). Read at
    /// init by the thread (`purge.rs`) and the commit path (`class.rs`).
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
    /// [`PercpuArena`]): `auto` follows the runtime probe, `percpu` forces
    /// the request (thread caches with a report note when the probe
    /// fails), `off` disables. Read at init through this field, as `thp`
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
    /// design 17, plan Phase 2.5 task 16): `default` issues nothing,
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
    /// (`SlabTable::is_guard_slab`, plan Phase 2.5 task 4c): a configured
    /// value that is not one rounds up to the next power of two, reported
    /// as [`Issue::Rounded`] (an abort under `abort_conf`) and readable
    /// through `opt.guard_slab_interval`; 1 is out of range (clamps to 2).
    pub guard_slab_interval: u32,
    /// Freed huge runs with more usable bytes than this are purged the
    /// moment they are freed instead of waiting for decay
    /// (`Huge::set_retain_max`, design 7).
    pub huge_retain_max: usize,
    /// Clear a freed block before it is cached or released (a slot's
    /// class-size bytes on the plain and sized free paths, a huge run's
    /// usable bytes): `Shared::push_flags`, `heap::zero_slot`,
    /// `Shared::zero_huge`. Off by default: a `memset` per free is more
    /// than the 3% budget on Valkey's 1 KiB values (review 2026-09-18
    /// section 5.1; priced in `bench/results/2026-09-18-*-gce-tunables.md`).
    pub zero_on_free: bool,
    /// Sign the thread caches' slot pointers and the heap pointer in TLS
    /// with `pacda` where the CPU and kernel expose pointer authentication
    /// (`HWCAP_PACA`/`PACG`; `sys::pac`, design 6.1, 6.2 and 17; plan
    /// Phase 2.5 task 7). `Shared::push_flags` bit `PUSH_PAC`. Opt-in:
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
    /// `stats.cgroup.*` and the reports. `false` reads nothing. The policy
    /// that acts on the figures is Phase 2 task 5.
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
    /// for and cost that row 2.4% of RSS on aarch64 (Phase 1.5 task 15,
    /// `bench/results/2026-09-18-{aarch64,x86_64}-gce-group-handoff.md`);
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
            b"prefault" => self.prefault = parse_bool(value).ok_or_else(bad)?,
            b"dirty_decay_ms" => return num(self, -1, MAX_INTERVAL_MS, |c, v| c.dirty_decay_ms = v),
            b"muzzy_decay_ms" => return num(self, -1, MAX_INTERVAL_MS, |c, v| c.muzzy_decay_ms = v),
            b"narenas" => return num(self, 0, u32::MAX as i64, |c, v| c.narenas = v as u32),
            b"percpu_arena" => {
                self.percpu_arena = match value {
                    b"auto" => PercpuArena::Auto,
                    b"percpu" | b"phycpu" => PercpuArena::Percpu,
                    b"off" | b"disabled" => PercpuArena::Off,
                    _ => return Err(bad()),
                }
            }
            b"tcache" => self.tcache = parse_bool(value).ok_or_else(bad)?,
            b"tcache_max" => {
                return num(self, 0, crate::layout::MEDIUM_MAX as i64, |c, v| c.tcache_max = v as usize);
            }
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
            b"spin_hint" => {
                self.spin_hint = match value {
                    b"auto" => SpinChoice::Auto,
                    b"baseline" => SpinChoice::Baseline,
                    b"umwait" => SpinChoice::Umwait,
                    b"wfe" => SpinChoice::Wfe,
                    _ => return Err(bad()),
                }
            }
            b"thp" => {
                self.thp = match value {
                    b"never" | b"default" => Thp::Never,
                    b"auto" => Thp::Auto,
                    b"always" => Thp::Always,
                    _ => return Err(bad()),
                }
            }
            b"thp_process" => {
                self.thp_process = match value {
                    b"default" => ThpProcess::Default,
                    b"advised" => ThpProcess::Advised,
                    _ => return Err(bad()),
                }
            }
            b"slice_ext" => {
                self.slice_ext = match value {
                    b"auto" => SliceExt::Auto,
                    b"on" => SliceExt::On,
                    b"off" => SliceExt::Off,
                    _ => return Err(bad()),
                }
            }
            b"lazy_free" => self.lazy_free = parse_bool(value).ok_or_else(bad)?,
            b"lazy_band" => self.lazy_band = parse_bool(value).ok_or_else(bad)?,
            b"guards" => self.guards = parse_bool(value).ok_or_else(bad)?,
            b"guard_slab_interval" => {
                // 0 is off and N >= 2 is one guard per N x 64 KiB; 1 would
                // make every slab a guard and leave nothing to allocate
                // (`SlabTable::new` refuses it), so it is out of range and
                // clamps to the smallest interval with any slabs, 2. The
                // interval must be a power of two (the sized fast path's
                // guard test is a mask): anything else rounds up and is
                // reported, so `abort_conf` refuses it and a plain run says
                // what it got. The maximum is a power of two, so a clamped
                // value needs no rounding.
                let r = num(self, 0, MAX_GUARD_SLAB_INTERVAL as i64, |c, v| c.guard_slab_interval = v as u32);
                if self.guard_slab_interval == 1 {
                    self.guard_slab_interval = 2;
                    return Err(Issue::OutOfRange { key, value });
                }
                if self.guard_slab_interval != 0 && !self.guard_slab_interval.is_power_of_two() {
                    self.guard_slab_interval = self.guard_slab_interval.next_power_of_two();
                    return Err(Issue::Rounded { key, value, to: self.guard_slab_interval as u64 });
                }
                return r;
            }
            b"huge_retain_max" => {
                return num(self, 0, MAX_HUGE_RETAIN_MAX as i64, |c, v| c.huge_retain_max = v as usize);
            }
            b"zero_on_free" => self.zero_on_free = parse_bool(value).ok_or_else(bad)?,
            b"pac_pointers" => self.pac_pointers = parse_bool(value).ok_or_else(bad)?,
            b"slot_randomize" => self.slot_randomize = parse_bool(value).ok_or_else(bad)?,
            b"cgroup" => self.cgroup = parse_bool(value).ok_or_else(bad)?,
            // jemalloc's `stats_print` (report at exit): no field, since
            // there is nothing to print at exit; validated as a bool and
            // reported as inert so `abort_conf` does not let a program expect
            // the report.
            b"stats_print" => {
                parse_bool(value).ok_or_else(bad)?;
                return Err(Issue::Inert(key));
            }
            b"double_free_scan" => self.double_free_scan = parse_bool(value).ok_or_else(bad)?,
            b"abort_conf" => self.abort_conf = parse_bool(value).ok_or_else(bad)?,
            b"check_zero" => self.check_zero = parse_bool(value).ok_or_else(bad)?,
            b"group_handoff" => self.group_handoff = parse_bool(value).ok_or_else(bad)?,
            // jemalloc keys accepted for compatibility and ignored: their
            // values are not validated because we do nothing with them.
            b"junk" | b"zero" | b"retain" | b"oversize_threshold" | b"metadata_thp"
            | b"confirm_conf" => {}
            _ => return Err(Issue::UnknownKey(key)),
        }
        Ok(())
    }

    /// The value of a key by name, for `mallctl("opt.<key>")`. `None` for
    /// keys that are only accepted and ignored, and for unknown keys.
    pub fn get(&self, key: &[u8]) -> Option<Value> {
        Some(match key {
            b"background_thread" => Value::Bool(self.background_thread),
            b"prefault" => Value::Bool(self.prefault),
            b"dirty_decay_ms" => Value::Int(self.dirty_decay_ms),
            b"muzzy_decay_ms" => Value::Int(self.muzzy_decay_ms),
            b"narenas" => Value::Uint(self.narenas as u64),
            b"percpu_arena" => Value::Str(self.percpu_arena.name()),
            b"tcache" => Value::Bool(self.tcache),
            b"tcache_max" => Value::Uint(self.tcache_max as u64),
            b"tcache_nslots_small_max" => Value::Uint(self.tcache_nslots_small_max as u64),
            b"lg_tcache_nslots_mul" => Value::Int(self.lg_tcache_nslots_mul as i64),
            b"tcache_gc_interval_ms" => Value::Int(self.tcache_gc_interval_ms),
            b"tcache_compact_shift" => Value::Uint(self.tcache_compact_shift as u64),
            b"thp" => Value::Str(self.thp.name()),
            b"thp_process" => Value::Str(self.thp_process.name()),
            b"spin_hint" => Value::Str(self.spin_hint.name()),
            b"slice_ext" => Value::Str(self.slice_ext.name()),
            b"lazy_free" => Value::Bool(self.lazy_free),
            b"lazy_band" => Value::Bool(self.lazy_band),
            b"guards" => Value::Bool(self.guards),
            b"guard_slab_interval" => Value::Uint(self.guard_slab_interval as u64),
            b"huge_retain_max" => Value::Uint(self.huge_retain_max as u64),
            b"zero_on_free" => Value::Bool(self.zero_on_free),
            b"pac_pointers" => Value::Bool(self.pac_pointers),
            b"slot_randomize" => Value::Bool(self.slot_randomize),
            b"double_free_scan" => Value::Bool(self.double_free_scan),
            b"abort_conf" => Value::Bool(self.abort_conf),
            b"cgroup" => Value::Bool(self.cgroup),
            b"check_zero" => Value::Bool(self.check_zero),
            b"group_handoff" => Value::Bool(self.group_handoff),
            _ => return None,
        })
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
}

/// Cap of the automatic `narenas` default (design section 13.3).
pub const MAX_AUTO_ARENAS: u32 = 64;

/// Every key readable through [`Config::get`], in table order, for the
/// `opt.` namespace and the tests.
pub const KEYS: &[&str] = &[
    "background_thread",
    "prefault",
    "dirty_decay_ms",
    "muzzy_decay_ms",
    "narenas",
    "percpu_arena",
    "tcache",
    "tcache_max",
    "tcache_nslots_small_max",
    "lg_tcache_nslots_mul",
    "tcache_gc_interval_ms",
    "tcache_compact_shift",
    "thp",
    "thp_process",
    "spin_hint",
    "slice_ext",
    "lazy_free",
    "lazy_band",
    "guards",
    "guard_slab_interval",
    "huge_retain_max",
    "zero_on_free",
    "pac_pointers",
    "slot_randomize",
    "double_free_scan",
    "abort_conf",
    "cgroup",
    "check_zero",
    "group_handoff",
];

/// jemalloc keys that are accepted and ignored.
pub const IGNORED_KEYS: &[&str] = &[
    "junk",
    "zero",
    "retain",
    "oversize_threshold",
    "metadata_thp",
    "confirm_conf",
];

/// Keys that parse but that no code acts on: `stats_print` has nothing to
/// print at exit. Setting one is [`Issue::Inert`], which `abort_conf`
/// turns into an abort; `malloc_stats_print` lists them as "accepted, no
/// effect". `thp` left the list with plan Phase 2.5 task 9.
pub const INERT_KEYS: &[&str] = &["stats_print"];

/// Whether `key` is one of [`INERT_KEYS`].
pub fn is_inert(key: &[u8]) -> bool {
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
    /// A number the key needs as a power of two; the value was rounded up
    /// to `to` (`guard_slab_interval`).
    Rounded { key: &'a [u8], value: &'a [u8], to: u64 },
    /// One of [`INERT_KEYS`]: parsed and (where it has a field) stored, but
    /// nothing reads it.
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

    /// Parse a string whose only problems are inert keys (one report per
    /// inert pair, `n` of them) and return the config.
    fn inert(text: &str, n: usize) -> Config {
        let (cfg, issues) = parse(text);
        assert_eq!(issues.len(), n, "{text}: {issues:?}");
        for issue in &issues {
            assert!(issue.contains("accepted but has no effect"), "{text}: {issue}");
        }
        cfg
    }

    #[test]
    fn defaults_match_the_design_table() {
        let d = Config::DEFAULT;
        assert!(!d.background_thread);
        assert_eq!(d.dirty_decay_ms, 10_000);
        assert_eq!(d.narenas, 0);
        assert_eq!(d.percpu_arena, PercpuArena::Auto, "on where the mm_cid probe passes (plan Phase 2.5 task 8)");
        assert_eq!(d.slice_ext, SliceExt::Auto, "on where the slice extension probe passes (plan Phase 2.5 task 15)");
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
        assert_eq!(d.thp_process, ThpProcess::Default, "process-wide, so opt-in (plan Phase 2.5 task 16)");
        assert!(!d.prefault, "off until measured (plan Phase 2.5 task 11)");
        assert!(!d.prefault_active());
        assert!(!d.lazy_free);
        assert!(d.lazy_band);
        assert!(!clean("lazy_band:false").lazy_band);
        assert!(d.guards);
        assert_eq!(d.guard_slab_interval, 8);
        assert_eq!(d.huge_retain_max, 64 << 20);
        assert_eq!(d.huge_retain_max, crate::huge::DEFAULT_RETAIN_MAX);
        assert!(!d.zero_on_free && !d.slot_randomize && !d.double_free_scan && !d.abort_conf && !d.check_zero);
        assert!(!d.pac_pointers, "signed pointers are opt-in (plan Phase 2.5 task 7, priced in bench/results/2026-09-19-lane-a-pac-pointers.md)");
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
                let cfg = if is_inert(key.as_bytes()) { inert(&text, 1) } else { clean(&text) };
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
        let cases: [(&str, i64, &str, i64, &str, i64); 10] = [
            ("dirty_decay_ms", 5000, "-2", -1, "9999999999999999", MAX_INTERVAL_MS),
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
                "narenas" | "tcache_max" | "tcache_nslots_small_max" | "guard_slab_interval" | "tcache_compact_shift"
                | "huge_retain_max" => Value::Uint(v as u64),
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
        // No longer inert (plan Phase 2.5 task 9): accepted under abort_conf.
        assert!(!is_inert(b"thp"));
        assert_eq!(clean("thp:auto,abort_conf:true").thp, Thp::Auto);
    }

    /// The inert keys: stored and readable, but every one is reported so
    /// that `abort_conf` rejects a configuration that relies on it; a bad
    /// value is still a bad value, not an inert report.
    /// `percpu_arena` (plan Phase 2.5 task 8): the three values, jemalloc's
    /// spellings as aliases, anything else reported and left at the
    /// default, and the helpers lane A's init reads.
    #[test]
    fn percpu_arena_values() {
        assert!(!IGNORED_KEYS.contains(&"percpu_arena"), "a real key since Phase 2.5 task 8");
        assert!(!is_inert(b"percpu_arena"));
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
        for bad in ["true", "false", "1", "on", "Auto", "PERCPU", "", "percpux"] {
            let (cfg, issues) = parse(&std::format!("percpu_arena:{bad}"));
            assert_eq!(cfg, Config::DEFAULT, "percpu_arena:{bad} changed the config");
            assert_eq!(issues.len(), 1, "percpu_arena:{bad}: {issues:?}");
            let expect = if bad.is_empty() { "malformed" } else { "invalid conf value \"" };
            assert!(issues[0].contains(expect), "percpu_arena:{bad}: {}", issues[0]);
        }
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

    /// `slice_ext` (plan Phase 2.5 task 15): the three values, anything
    /// else reported and left at the default, `opt.slice_ext` reads back,
    /// and the helpers `slice::configure` is called with.
    #[test]
    fn slice_ext_values() {
        assert!(KEYS.contains(&"slice_ext"));
        assert!(!IGNORED_KEYS.contains(&"slice_ext") && !is_inert(b"slice_ext"));
        for (value, want) in [("auto", SliceExt::Auto), ("on", SliceExt::On), ("off", SliceExt::Off)] {
            let cfg = clean(&std::format!("slice_ext:{value},abort_conf:true"));
            assert_eq!(cfg.slice_ext, want, "{value}");
            assert_eq!(cfg.get(b"slice_ext"), Some(Value::Str(want.name())), "{value}");
            assert_eq!(want.name(), value);
            assert!(cfg.abort_conf);
        }
        for bad in ["true", "false", "1", "yes", "Auto", "ON", "", "onn", "percpu"] {
            let (cfg, issues) = parse(&std::format!("slice_ext:{bad}"));
            assert_eq!(cfg, Config::DEFAULT, "slice_ext:{bad} changed the config");
            assert_eq!(issues.len(), 1, "slice_ext:{bad}: {issues:?}");
            let expect = if bad.is_empty() { "malformed" } else { "invalid conf value \"" };
            assert!(issues[0].contains(expect), "slice_ext:{bad}: {}", issues[0]);
        }
        assert_eq!(clean("slice_ext:on,slice_ext:off").slice_ext, SliceExt::Off);
        assert_eq!(clean("slice_ext:\"on\"").slice_ext, SliceExt::On);
        // `auto` and `on` consult the probe, `off` never; only `on`
        // reports a failed probe.
        assert!(SliceExt::Auto.wanted() && SliceExt::On.wanted() && !SliceExt::Off.wanted());
        assert!(SliceExt::On.forced() && !SliceExt::Auto.forced() && !SliceExt::Off.forced());
    }

    /// `thp_process` (plan Phase 2.5 task 16, the prctl half): the two
    /// values, anything else reported and left at the default,
    /// `opt.thp_process` reads back, the accessor `thp.rs` decides the
    /// `prctl` on, and `thp` untouched by it (they are separate keys: the
    /// per-region advice and the process-wide policy).
    #[test]
    fn thp_process_values() {
        assert!(KEYS.contains(&"thp_process"));
        assert!(!IGNORED_KEYS.contains(&"thp_process") && !is_inert(b"thp_process"));
        for (value, want) in [("default", ThpProcess::Default), ("advised", ThpProcess::Advised)] {
            let cfg = clean(&std::format!("thp_process:{value},abort_conf:true"));
            assert_eq!(cfg.thp_process, want, "{value}");
            assert_eq!(cfg.thp_process(), want, "{value}");
            assert_eq!(cfg.get(b"thp_process"), Some(Value::Str(want.name())), "{value}");
            assert_eq!(want.name(), value);
            assert!(cfg.abort_conf);
            assert_eq!(cfg.thp, Thp::Never, "thp_process leaves thp alone");
        }
        for bad in ["true", "false", "1", "never", "always", "auto", "Default", "ADVISED", "", "advise"] {
            let (cfg, issues) = parse(&std::format!("thp_process:{bad}"));
            assert_eq!(cfg, Config::DEFAULT, "thp_process:{bad} changed the config");
            assert_eq!(issues.len(), 1, "thp_process:{bad}: {issues:?}");
            let expect = if bad.is_empty() { "malformed" } else { "invalid conf value \"" };
            assert!(issues[0].contains(expect), "thp_process:{bad}: {}", issues[0]);
        }
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

    /// `prefault` (plan Phase 2.5 task 11): a bool key, off by default,
    /// readable, accepted under `abort_conf`; `prefault_active` is the
    /// key and the background thread that runs the pass.
    #[test]
    fn prefault_values() {
        assert!(KEYS.contains(&"prefault"));
        assert!(!IGNORED_KEYS.contains(&"prefault") && !is_inert(b"prefault"));
        for (value, want) in [("true", true), ("false", false)] {
            let cfg = clean(&std::format!("prefault:{value},abort_conf:true"));
            assert_eq!(cfg.prefault, want, "{value}");
            assert_eq!(cfg.prefault(), want, "{value}");
            assert_eq!(cfg.get(b"prefault"), Some(Value::Bool(want)), "{value}");
            assert!(cfg.abort_conf);
            assert!(!cfg.prefault_active(), "no background thread, no pass");
        }
        for bad in ["1", "0", "yes", "on", "True", "", "advised"] {
            let (cfg, issues) = parse(&std::format!("prefault:{bad}"));
            assert_eq!(cfg, Config::DEFAULT, "prefault:{bad} changed the config");
            assert_eq!(issues.len(), 1, "prefault:{bad}: {issues:?}");
            let expect = if bad.is_empty() { "malformed" } else { "invalid conf value \"" };
            assert!(issues[0].contains(expect), "prefault:{bad}: {}", issues[0]);
        }
        assert!(!clean("prefault:true,prefault:false").prefault);
        assert!(clean("prefault:\"true\"").prefault);
        // The pass needs the thread, in either key order.
        assert!(clean("prefault:true,background_thread:true").prefault_active());
        assert!(clean("background_thread:true,prefault:true").prefault_active());
        assert!(!clean("background_thread:true").prefault_active());
        assert!(!clean("background_thread:true,prefault:true,background_thread:false").prefault_active());
    }

    #[test]
    fn inert_keys_are_stored_reported_and_rejected_under_abort_conf() {
        assert_eq!(INERT_KEYS.len(), 1, "stats_print");
        for key in INERT_KEYS {
            assert!(is_inert(key.as_bytes()), "{key}");
            assert!(!IGNORED_KEYS.contains(key), "{key} cannot be both ignored and inert");
            let value = "true";
            let text = std::format!("{key}:{value}");
            let (cfg, issues) = parse(&text);
            assert_eq!(issues.len(), 1, "{text}: {issues:?}");
            assert_eq!(
                issues[0],
                std::format!("conf key {key} is accepted but has no effect (not implemented)"),
                "{text}"
            );
            match *key {
                "stats_print" => {
                    assert_eq!(cfg, Config::DEFAULT, "stats_print has no field");
                    assert_eq!(cfg.get(b"stats_print"), None);
                }
                _ => assert_eq!(cfg.get(key.as_bytes()), Some(Value::Bool(true)), "{key}"),
            }
            let (cfg, issues) = parse(&std::format!("{key}:nonsense"));
            assert_eq!(cfg, Config::DEFAULT, "{key}:nonsense changed the config");
            assert_eq!(issues.len(), 1, "{key}:nonsense: {issues:?}");
            assert!(issues[0].contains("invalid conf value"), "{key}:nonsense: {}", issues[0]);
        }
        for key in KEYS {
            assert_eq!(is_inert(key.as_bytes()), INERT_KEYS.contains(key), "{key}");
        }
        assert!(!is_inert(b"double_free_scan"), "implemented since Phase 1.5 task 9");
        // Phase 2 task 3: the two hardening keys are real, opt-in keys: no
        // report, stored, readable, and `abort_conf` accepts them.
        for key in ["zero_on_free", "slot_randomize"] {
            assert!(!is_inert(key.as_bytes()), "{key}");
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
        assert_eq!(issues.len(), 1, "cgroup (Phase 2 task 4) and thp (Phase 2.5 task 9) are real keys: {issues:?}");
        assert!(!is_inert(b"cgroup"));
        assert!(!is_inert(b"thp"));
    }

    #[test]
    fn huge_retain_max_is_a_byte_count() {
        assert_eq!(clean("huge_retain_max:0").huge_retain_max, 0);
        assert_eq!(clean("huge_retain_max:0x100000").huge_retain_max, 1 << 20);
        assert_eq!(clean("huge_retain_max:1099511627776").huge_retain_max, MAX_HUGE_RETAIN_MAX);
        assert_eq!(clean("huge_retain_max:1").get(b"huge_retain_max"), Some(Value::Uint(1)));
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
                Value::Str(s) => s.into(),
            };
            let text = std::format!("{key}:{value}");
            let round = if is_inert(key.as_bytes()) { inert(&text, 1) } else { clean(&text) };
            assert_eq!(round, cfg, "{key}:{value} round trip");
        }
        assert_eq!(KEYS.len(), 29, "27 + thp_process and prefault (plan Phase 2.5 tasks 16 and 11)");
        assert_eq!(cfg.get(b"nonsense"), None);
        assert_eq!(cfg.get(b""), None);
        for key in IGNORED_KEYS {
            assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
        }
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
        // A configured hint is kept against the probe's suggestion.
        let forced = spin_hint();
        let _ = Cpu::probe(crate::sys::features::page_size());
        assert_eq!(spin_hint(), forced);
        // Back to the probe's choice for the rest of the suite.
        crate::lock::set_spin_hint(auto);
        assert_eq!(spin_hint(), auto);
    }
}
