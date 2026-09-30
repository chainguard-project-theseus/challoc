//! The per-CPU mode (design 15): the [`Percpu`] table of [`CpuCache`]s
//! keyed by rseq `mm_cid` and the per-CPU `impl Heap` blocks
//! ([`Heap::percpu_alloc`], [`Heap::percpu_free`], refill, flush, GC,
//! drain), reached from the slow paths of `heap/mod.rs` alone: a heap in
//! the mode keeps its home caches `EMPTY`, so the entry points' inlined
//! pop and push fall through to `malloc_slow`/`free_slow` and carry no
//! test for the mode.

use core::ptr;

use crate::config::Config;
use crate::layout::NCLASSES;
use crate::meta::Meta;
use crate::rseq;

use super::{CACHE_HEADER, CacheHeader, Heap, Shared};
#[cfg(target_os = "linux")]
use super::flush::{group_by_slab, straddling_double_free};
#[cfg(target_os = "linux")]
use super::{MAX_CAP, MODE_COMPACT, MODE_OFF, MODE_PROBE, PROBE_MASK, Verdict, zero_slot};


/// The caches of one concurrency id in the per-CPU mode (design 15): the
/// same header array as a heap's home caches, one line per class, so the
/// per-CPU paths and the thread paths share [`CacheHeader`] and its stack
/// layout. The table is `possible_cpus` of these, contiguous, reserved
/// zeroed from the metadata region at init and touched only as ids are
/// used: a zeroed header is `len == cap == 0`, which both fast paths treat
/// as "slow path", and the first slow path on it installs the stack
/// ([`Heap::percpu_ensure`]). Memory is bounded by the number of possible
/// CPUs, never by the number of threads.
#[repr(C, align(64))]
pub struct CpuCache {
    pub caches: [CacheHeader; NCLASSES],
}

/// Distance between two ids' tables ([`rseq::STRIDE`]).
pub const CPU_CACHE_SIZE: usize = core::mem::size_of::<CpuCache>();
const _: () = assert!(CPU_CACHE_SIZE == NCLASSES * CACHE_HEADER);

/// The per-CPU mode (design 15): the table of
/// [`CpuCache`]s indexed by the running thread's rseq `mm_cid`, or why
/// there is none. Built by [`Shared::init`] from the configuration
/// (`percpu_arena`), the rseq probe (`rseq::probe`) and the possible-CPU
/// count; read-only afterwards.
#[repr(C)]
pub struct Percpu {
    /// The table: `n` contiguous [`CpuCache`]s; null when the mode is off.
    base: *mut u8,
    /// The runs (design 6.4 under the mode): a second table of the same
    /// shape whose header `(id, class)` is the id's *run* for the class, a
    /// stack of `cap / 4` low-address slots claimed from the arena's lowest
    /// partial slab, served one per scheduled pop in compact mode through
    /// the same sections as the caches ([`Heap::percpu_run_pop`]). Null
    /// when the mode is off.
    runs: *mut u8,
    /// `__rseq_offset`: thread pointer to the thread's rseq area.
    pub(super) offset: isize,
    /// Table entries: the kernel's `nr_cpu_ids`, the bound on `mm_cid`.
    n: u32,
    state: PercpuState,
    /// What glibc registered, for the report.
    probe: rseq::Probe,
    /// `percpu_arena:percpu` was asked for (the report notes a fallback).
    forced: bool,
}

/// Why the per-CPU mode is on or off ([`Percpu::state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PercpuState {
    /// Caches keyed by `mm_cid`.
    On = 0,
    /// `percpu_arena:off`.
    OffKey = 1,
    /// `percpu_arena:auto`, the key's default: thread caches (the mode
    /// measured behind them on every row, see [`Percpu::build`]);
    /// `percpu` is the opt-in.
    OffAuto = 6,
    /// The rseq probe failed (`rseq::Probe::verdict` says how).
    OffProbe = 2,
    /// `/sys/devices/system/cpu/possible` could not be read.
    OffCpus = 3,
    /// `double_free_scan:true`: the scan of the top entries is not written
    /// into the critical sections, so the two keys exclude each other and
    /// the hardening wins.
    OffScan = 4,
    /// The metadata region could not hold the table.
    OffMemory = 5,
}

impl PercpuState {
    pub const fn name(self) -> &'static str {
        match self {
            PercpuState::On => "on",
            PercpuState::OffKey => "off",
            PercpuState::OffAuto => "off (auto: opt in with percpu_arena:percpu)",
            PercpuState::OffProbe => "off (rseq)",
            PercpuState::OffCpus => "off (no cpu count)",
            PercpuState::OffScan => "off (double_free_scan)",
            PercpuState::OffMemory => "off (no metadata)",
        }
    }
}

// SAFETY: `base` names a process-lifetime metadata reservation whose
// headers are only ever mutated inside rseq critical sections (one running
// thread per id) or read racily with volatile loads for diagnostics; the
// other fields are written once by `init`.
unsafe impl Sync for Percpu {}
unsafe impl Send for Percpu {}

impl Percpu {
    const OFF: Percpu = Percpu {
        base: ptr::null_mut(),
        runs: ptr::null_mut(),
        offset: 0,
        n: 0,
        state: PercpuState::OffKey,
        probe: rseq::Probe::NONE,
        forced: false,
    };

    /// Decide the mode and reserve the table. `allowed` is the caller's
    /// say (init passes true; the unit tests' `Shared` builders false, so
    /// their thread-cache assertions hold on a host with `mm_cid`): the
    /// mode is on when it is allowed, the key is `percpu` and the probe
    /// passes (`PercpuArena::enabled`), `double_free_scan` is off, the
    /// possible-CPU count is readable and the metadata region has room for
    /// `n * CPU_CACHE_SIZE` bytes (reserved zeroed, touched as ids are
    /// used). `auto`, the key's default, keeps thread caches
    /// ([`PercpuState::OffAuto`]): the mode measured 8..24% behind thread
    /// caches on `quick`'s rows and worse on the Valkey model on both
    /// reference VMs (`bench/results/2026-09-19-*-task8-percpu*.md`), so
    /// thread caches stay the default; `auto` is read here, next to the
    /// probe, and `config::PercpuArena` documents the three values. Off
    /// Linux there is no rseq and the mode is off.
    pub(super) fn build(meta: &Meta, config: &Config, allowed: bool) -> Percpu {
        use crate::config::PercpuArena;
        let probe = rseq::probe();
        let forced = config.percpu_arena.forced();
        let off = |state: PercpuState| Percpu { state, probe, forced, ..Percpu::OFF };
        match config.percpu_arena {
            PercpuArena::Off => return off(PercpuState::OffKey),
            PercpuArena::Auto => return off(PercpuState::OffAuto),
            PercpuArena::Percpu => {}
        }
        if !allowed || !config.percpu_arena.enabled(probe.mm_cid_usable()) {
            return off(PercpuState::OffProbe);
        }
        if config.double_free_scan {
            return off(PercpuState::OffScan);
        }
        #[cfg(target_os = "linux")]
        {
            let Some(n) = rseq::possible_cpus() else { return off(PercpuState::OffCpus) };
            let n = n.clamp(1, crate::arena::MAX_ARENAS as u32);
            let Some(base) = meta.reserve_committed(n as usize * CPU_CACHE_SIZE, 64) else {
                return off(PercpuState::OffMemory);
            };
            let Some(runs) = meta.reserve_committed(n as usize * CPU_CACHE_SIZE, 64) else {
                return off(PercpuState::OffMemory);
            };
            Percpu { base: base as *mut u8, runs: runs as *mut u8, offset: probe.offset, n, state: PercpuState::On, probe, forced }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = meta;
            off(PercpuState::OffProbe)
        }
    }

    /// Whether the caches are keyed by `mm_cid`.
    #[inline(always)]
    pub fn on(&self) -> bool {
        self.state == PercpuState::On
    }

    pub fn state(&self) -> PercpuState {
        self.state
    }

    /// The rseq probe the decision was made on.
    pub fn probe(&self) -> rseq::Probe {
        self.probe
    }

    /// `percpu_arena:percpu` was configured.
    pub fn forced(&self) -> bool {
        self.forced
    }

    /// Table entries (possible CPUs); 0 when off.
    pub fn ids(&self) -> u32 {
        self.n
    }

    /// The critical sections' view of the table for `class`: the id-0
    /// header of that class and the id bound.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    pub(super) fn table(&self, class: usize) -> rseq::Table {
        debug_assert!(self.on() && class < NCLASSES);
        // SAFETY: `class < NCLASSES` and the table has `n >= 1` entries.
        rseq::Table { base: unsafe { self.base.add(class * CACHE_HEADER) }, n: self.n }
    }

    /// The sections' view of the run table for `class` (see `runs`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[inline(always)]
    fn run_table(&self, class: usize) -> rseq::Table {
        debug_assert!(self.on() && class < NCLASSES);
        // SAFETY: `class < NCLASSES` and the table has `n >= 1` entries.
        rseq::Table { base: unsafe { self.runs.add(class * CACHE_HEADER) }, n: self.n }
    }

    /// The header of `(id, class)` for diagnostics and statistics (the
    /// fields are read racily with volatile loads; the owner may be
    /// writing them from a critical section on another CPU).
    pub fn header(&self, id: u32, class: usize) -> Option<*const CacheHeader> {
        if !self.on() || id >= self.n || class >= NCLASSES {
            return None;
        }
        // SAFETY: inside the table.
        Some(unsafe { self.base.add(id as usize * CPU_CACHE_SIZE + class * CACHE_HEADER) } as *const CacheHeader)
    }

    /// The run header of `(id, class)` (see `runs`), as [`Percpu::header`].
    pub(super) fn run_header(&self, id: u32, class: usize) -> Option<*const CacheHeader> {
        if !self.on() || id >= self.n || class >= NCLASSES {
            return None;
        }
        // SAFETY: inside the run table.
        Some(unsafe { self.runs.add(id as usize * CPU_CACHE_SIZE + class * CACHE_HEADER) } as *const CacheHeader)
    }

    /// Installed headers and cached slots over both tables, the caches and
    /// the runs (a racy snapshot for `stats.percpu.*` and the report).
    pub fn snapshot(&self) -> (usize, usize) {
        let (mut headers, mut slots) = (0usize, 0usize);
        for id in 0..self.n {
            for class in 0..NCLASSES {
                for h in [self.header(id, class).unwrap(), self.run_header(id, class).unwrap()] {
                    // SAFETY: a live header; racy reads of two `u32`s.
                    let (cap, len) = unsafe {
                        (ptr::read_volatile(&raw const (*h).cap), ptr::read_volatile(&raw const (*h).len))
                    };
                    if cap != 0 {
                        headers += 1;
                        slots += len as usize;
                    }
                }
            }
        }
        (headers, slots)
    }
}

// The flag and the `Regions` fields every `free` reads share one 128-byte
// line (`Regions` is `repr(C)`, so `superblock` and `lg_stride` lead it).
/// The per-CPU paths (design 15). A process in the
/// mode ([`Shared::percpu_on`]) keys its plain
/// `malloc`/`free` caches by the running thread's rseq `mm_cid` instead of
/// by thread: the table of [`CpuCache`]s in [`Percpu`], one header per
/// (concurrency id, class), every header operation a critical section of
/// `rseq.rs` that reads the id, so no lock and no atomic read-modify-write
/// is added to the fast paths and two threads never mutate one header at
/// the same time (an id is held by one running thread; a thread preempted
/// inside a section is moved to the abort handler and retries). The thread
/// heap stays: it is the init witness the fast paths already load, it
/// keeps the thread's byte counters (every per-CPU pop and push is counted
/// there, `thread.allocated` stays exact), it serves huge runs as before,
/// and a heap off the mode ([`Heap::percpu`] false) takes the thread paths.
///
/// How the mode is reached (the zero-cost dispatch, 2026-09-21): a heap
/// in the mode never installs a home cache (`Heap::ensure_cache` refuses
/// its home arena; an explicit `MALLOCX_ARENA` request for that arena is
/// served directly), so every home header stays `CacheHeader::EMPTY`
/// (`len == cap == 0`), the state both entry-point fast paths already
/// treat as "slow path": `malloc`'s inlined pop sees `len == 0`, `free`'s
/// inlined push sees `len == cap`, and each takes the cold tail call it
/// has anyway (`malloc_slow`, `free_slow`), where `Heap::alloc`,
/// `Heap::alloc_zeroed` and `Heap::free` test the heap's `percpu` byte and
/// come here ([`Heap::percpu_alloc`], [`Heap::percpu_free`]). The thread
/// paths carry no test, no flag bit and no tail-call shuffle for the
/// mode; what the mode pays is the entry point's wasted header load and
/// the slow path's prologue (the init-state load, the second TLS read and
/// `class_of` or the second slot check) before the section. The first
/// form, a `PUSH_PERCPU` bit in the flag byte tested by the entry points
/// with a tail call to a per-CPU wrapper, cost the default path +9
/// instructions and +1.8% cycles per larson operation on the C4A
/// (`bench/results/2026-09-20-aarch64-larson1-drift.md`); this form and
/// its numbers: `bench/results/2026-09-21-*-percpu-dispatch*.md`.
///
/// Arenas are per concurrency id: the header of `(id, class)` is bound to
/// arena `id % narenas` ([`Percpu::arena_of`]) at its first use and its
/// refills claim there, so the slabs a CPU touches are its own arena's;
/// a slot freed on another CPU is pushed onto that CPU's cache whatever
/// its arena (the flush routes it back through the owner array,
/// [`Shared::release_routed`], as the sized path's cache already does) and
/// the `free` fast path skips the owner word. What a slow path touches
/// besides the header goes through a thread-private buffer: a refill
/// claims its batch onto the stack and pushes it in one section
/// ([`rseq::push_batch`]); a flush takes the whole stack out
/// ([`rseq::take_all`]), selects the highest-address half as the thread
/// flush does, releases it and pushes the kept half back; if the thread
/// migrated in between, the batch simply lands on the new CPU's cache and
/// what does not fit is released to its slabs. Capacities follow the
/// thread ladder, clamped to [`MAX_CAP`] (the buffers are stack arrays of
/// that size). Compaction (design 6.4) keeps its gate per header (probe
/// every 4096 pops, compact while the class is fragmented and refills are
/// not draining it) and its run as a header of its own: the thread run
/// lives at the bottom of the cache's stack and a run pop moves the top
/// entry into the hole, two words no single commit store can make
/// consistent under an abort, so under the mode the run is a second table
/// of headers (`Percpu::runs`, one per (id, class), capacity `cap / 4`)
/// that the same `pop`/`push_batch`/`take_all`/`install` sections serve:
/// a scheduled pop in compact mode pops the id's run
/// ([`Heap::percpu_run_pop`]), and an exhausted run is rebuilt by one
/// block claim from the arena's lowest partial slab, one lock hold per
/// `cap / 4` scheduled pops as in the thread rebuild (one locked claim
/// per scheduled pop instead was most of the Valkey model's cost under
/// the mode, `bench/results/2026-09-19-*-task8-percpu-fastpath.md`).
/// The cache GC and `thread.tcache.flush` act on the caches of the CPU the
/// thread is running on (the drain takes the run too; the GC returns a
/// run whose class stopped compacting); an idle CPU's caches are drained
/// the next time a thread runs there and ticks, which is the residue the
/// mode bounds by cores. Thread exit leaves the CPU caches alone (they
/// are not the thread's), and fork needs nothing (no lock, and every
/// section's pre-commit stores leave a consistent header).
#[cfg(target_os = "linux")]
impl Heap {
    /// The per-CPU cache capacity for `class`: the thread ladder
    /// (`Config::cache_cap`) clamped to [`MAX_CAP`]; 0 means uncached.
    #[inline]
    pub(super) fn percpu_cap(shared: &Shared, class: usize) -> u32 {
        shared.config.cache_cap(class).min(MAX_CAP)
    }

    /// The run's capacity for `class`: the thread rebuild's block, a
    /// quarter of the cache (`Heap::rebuild_run`), at least one; 0 for an
    /// uncached class.
    #[inline]
    fn percpu_run_cap(shared: &Shared, class: usize) -> u32 {
        let cap = Self::percpu_cap(shared, class);
        if cap == 0 { 0 } else { (cap / 4).max(1) }
    }

    /// `malloc`/`calloc` of a heap in the mode ([`Heap::alloc`] and
    /// [`Heap::alloc_zeroed`] route here on the heap's `percpu` byte; the
    /// entry point's inlined pop has fallen through on the `EMPTY` home
    /// header): the `pop` section on the running CPU's header for `class`,
    /// counted in the thread's bytes on a hit; a scheduled pop goes to
    /// [`Heap::percpu_scheduled`], an empty cache to the refill
    /// ([`Heap::percpu_alloc_slow`]), an id out of range aborts. Out of
    /// line so the section is compiled once, not into every caller of
    /// `Heap::alloc` (`malloc_slow`, `calloc`, the aligned and `mallocx`
    /// paths); not cold, it is the mode's hit path, which tests one bit of
    /// the section's raw status word and has no call of its own (every
    /// miss is one cold tail call, [`Heap::percpu_alloc_miss`], so this
    /// function needs no frame).
    #[inline(never)]
    pub(super) fn percpu_alloc(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        // SAFETY: the owning thread registered this area; the table is live.
        let (p, st) = unsafe { rseq::pop_raw(self.rseq_area, shared.percpu.table(class), shared.pac()) };
        if st & rseq::POP_HIT != 0 {
            self.count_direct_alloc(class);
            return (p, st & rseq::POP_ZERO != 0);
        }
        self.percpu_alloc_miss(shared, class, st)
    }

    /// Every outcome of [`Heap::percpu_alloc`]'s section but a hit: the
    /// scheduled pop, an empty cache (the refill), an id out of range.
    #[cold]
    #[inline(never)]
    fn percpu_alloc_miss(&mut self, shared: &Shared, class: usize, st: u32) -> (*mut u8, bool) {
        match st {
            rseq::POP_EMPTY => self.percpu_alloc_slow(shared, class),
            rseq::POP_SCHEDULED => match self.percpu_scheduled(shared, class) {
                Some(r) => r,
                None => self.percpu_alloc_slow(shared, class),
            },
            _ => cid_out_of_range(),
        }
    }

    /// `free` of a heap in the mode ([`Heap::free`] routes here; the
    /// entry point's inlined check-and-push has fallen through on the
    /// `EMPTY` home header): the exact check ([`Shared::classify_free`],
    /// which aborts on anything that is not a live slot or huge run start,
    /// the message the plain path gives), then the `push` section on the
    /// running CPU's header for the slot's class whatever its arena (the
    /// flush routes it back through the owner word, `release_routed`),
    /// counted in the thread's bytes; a full or uninstalled cache and
    /// `zero_on_free` go to [`Heap::percpu_free_slow`], a huge run to the
    /// huge allocator. Out of line for the same reason as
    /// [`Heap::percpu_alloc`], and like it the hit path tests the raw
    /// status word and every miss is a tail call.
    #[inline(never)]
    pub(super) fn percpu_free(&mut self, shared: &Shared, p: *mut u8) {
        match shared.classify_free(p) {
            Ok((class, _arena)) => {
                if !shared.zero_on_free() {
                    // SAFETY: this thread's area; `p` is a verified slot
                    // start of `class`.
                    let st = unsafe { rseq::push_raw(self.rseq_area, shared.percpu.table(class), p, shared.pac()) };
                    if st == rseq::PUSH_PUSHED {
                        self.count_direct_free(class);
                        return;
                    }
                    if st != rseq::PUSH_FULL {
                        cid_out_of_range();
                    }
                }
                self.percpu_free_slow(shared, class, p)
            }
            Err(page) => self.free_huge(shared, page),
        }
    }

    /// A pop with the schedule already consumed: `Some` or empty.
    #[inline(never)]
    fn percpu_pop_plain(&mut self, shared: &Shared, class: usize) -> Option<(*mut u8, bool)> {
        // SAFETY: as in `pop_percpu_mode`.
        match unsafe { rseq::pop(self.rseq_area, shared.percpu.table(class), shared.pac()) } {
            rseq::Pop::Popped(p, zero) => {
                self.count_direct_alloc(class);
                Some((p, zero))
            }
            rseq::Pop::Empty | rseq::Pop::Scheduled => None,
            rseq::Pop::OutOfRange => cid_out_of_range(),
        }
    }

    /// The scheduled pop of a per-CPU cache (design 6.4 under the mode):
    /// in probe mode run the periodic work and ask the class whether it is
    /// fragmented and left alone by refills, entering compact mode if so;
    /// in compact mode serve the id's run ([`Heap::percpu_run_pop`]: the
    /// lowest slots of the arena's lowest partial slab, claimed a block at
    /// a time), re-checking the class every [`PROBE_MASK`] + 1 scheduled
    /// pops and dropping back to probing once it is dense; otherwise pop
    /// the top. The header's mode fields are hints written with volatile
    /// stores (another thread may own the id by the time they land; a
    /// stale mode changes only which pops are scheduled, never a stack).
    #[cold]
    #[inline(never)]
    fn percpu_scheduled(&mut self, shared: &Shared, class: usize) -> Option<(*mut u8, bool)> {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
        let h = h as *mut CacheHeader;
        // SAFETY: a live header of the table; racy volatile reads.
        let (mode, arena) = unsafe { (ptr::read_volatile(&raw const (*h).mode), ptr::read_volatile(&raw const (*h).arena)) };
        let cs = shared.owner(arena).class(class);
        match mode {
            MODE_COMPACT => {
                // SAFETY: as above; `run_end` is unused by per-CPU caches
                // (the run is a header of its own) and counts scheduled
                // pops here.
                let k = unsafe { ptr::read_volatile(&raw const (*h).run_end) }.wrapping_add(1);
                unsafe { ptr::write_volatile(&raw mut (*h).run_end, k) };
                if k & PROBE_MASK == 0 {
                    self.tick(shared);
                    if !Self::fragmented(shared, arena, class) {
                        cs.note_compact_mode(false);
                        // SAFETY: as above.
                        unsafe { CacheHeader::set_mode_volatile(h, MODE_PROBE) };
                        return self.percpu_pop_plain(shared, class);
                    }
                }
                return self.percpu_run_pop(shared, class, h, arena);
            }
            MODE_PROBE => {
                self.tick(shared);
                // SAFETY: as above.
                let prev = unsafe { ptr::read_volatile(&raw const (*h).probe_refilled) };
                let (refilled, verdict) = Self::probe_verdict(shared, arena, class, prev);
                unsafe { ptr::write_volatile(&raw mut (*h).probe_refilled, refilled) };
                match verdict {
                    Verdict::Refilling | Verdict::Dense => return self.percpu_pop_plain(shared, class),
                    Verdict::Fragmented => {}
                }
                // SAFETY: as above.
                unsafe {
                    CacheHeader::set_mode_volatile(h, MODE_COMPACT);
                    ptr::write_volatile(&raw mut (*h).run_end, 0);
                }
            }
            _ => return self.percpu_pop_plain(shared, class),
        }
        self.percpu_run_pop(shared, class, h, arena)
    }

    /// The compacting half of the scheduled pop (design 6.4's run under
    /// the mode; `h` is the running id's cache header, `arena` its arena).
    /// Pop the running id's run for `class` (the second table of
    /// [`Percpu`], through the same `pop` section as a cache; its header
    /// never schedules, `compact_mask == u32::MAX`). An exhausted or
    /// uninstalled run is *rebuilt* as the thread run is: the class is
    /// re-checked (dense: back to probing, pop the top), else a block of
    /// `cap / 4` slots is claimed from the arena's lowest partial slab
    /// under the class lock, once per block instead of once per scheduled
    /// pop (which would be one lock hold in 8 pops on the Valkey model),
    /// and pushed onto the run in one section, lowest on top,
    /// whatever did not fit (the thread migrated to an id whose run is
    /// full) going back to its slab; then the pop is retried. The zero
    /// prefix travels with the block, so `calloc` stays honest.
    #[cold]
    #[inline(never)]
    fn percpu_run_pop(&mut self, shared: &Shared, class: usize, h: *mut CacheHeader, arena: u16) -> Option<(*mut u8, bool)> {
        let pac = shared.pac();
        let area = self.rseq_area;
        let rt = shared.percpu.run_table(class);
        let cs = shared.owner(arena).class(class);
        loop {
            // SAFETY: this thread's area; the run table is live.
            match unsafe { rseq::pop(area, rt, pac) } {
                rseq::Pop::Popped(p, zero) => {
                    self.count_direct_alloc(class);
                    return Some((p, zero));
                }
                rseq::Pop::OutOfRange => cid_out_of_range(),
                rseq::Pop::Empty | rseq::Pop::Scheduled => {}
            }
            // Rebuild.
            let run_cap = Self::percpu_run_cap(shared, class);
            // SAFETY: this thread's area.
            let cid = unsafe { rseq::mm_cid(area) };
            if run_cap == 0 || !self.percpu_ensure(shared, class, run_cap, cid, true) {
                return self.percpu_pop_plain(shared, class);
            }
            if !Self::fragmented(shared, arena, class) {
                cs.note_compact_mode(false);
                // SAFETY: a live header; racy hints (see `percpu_scheduled`).
                unsafe { CacheHeader::set_mode_volatile(h, MODE_PROBE) };
                return self.percpu_pop_plain(shared, class);
            }
            let want = (run_cap as usize).min(MAX_CAP as usize / 4);
            let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize / 4];
            let out = &mut buf[..want];
            let claim = self.claim(shared, cs, out);
            if claim.n == 0 {
                return self.percpu_pop_plain(shared, class);
            }
            cs.note_compact(claim.n);
            let block = &mut out[..claim.n];
            block.reverse();
            // SAFETY: this thread's area; `block` are plain slot pointers.
            let pushed = match unsafe { rseq::push_batch(area, rt, block, claim.zeroed, pac) } {
                Ok(k) => k,
                Err(()) => cid_out_of_range(),
            };
            if pushed < block.len() {
                // A descending batch keeps each slab's slots contiguous.
                shared.release_routed(arena, class, &block[pushed..]);
            }
        }
    }

    /// One slot of `class` straight from the home arena's bitmaps, counted
    /// (the per-CPU paths' fallback for an uncached class or an exhausted
    /// metadata region).
    fn percpu_alloc_direct(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let Some(a) = shared.arena(self.arena) else { return (ptr::null_mut(), false) };
        let (p, zero) = shared.alloc_direct(a, class);
        if !p.is_null() {
            self.count_direct_alloc(class);
        }
        (p, zero)
    }

    /// Refill the running CPU's cache for `class` and pop (the per-CPU
    /// [`Heap::alloc_slow`]): claim up to `cap / 2` slots from the id's
    /// arena into a stack buffer, hand the lowest to the caller and push
    /// the rest in one section, lowest on top; what the section could not
    /// take (the thread moved to a CPU whose cache is full) goes back to
    /// its slabs. Retries from the top after a scheduled pop that served
    /// nothing.
    #[cold]
    #[inline(never)]
    pub(super) fn percpu_alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        self.tick(shared);
        let cap = Self::percpu_cap(shared, class);
        if cap == 0 {
            return self.percpu_alloc_direct(shared, class);
        }
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        loop {
            // SAFETY: this thread's area; the table is live.
            match unsafe { rseq::pop(area, t, pac) } {
                rseq::Pop::Popped(p, zero) => {
                    self.count_direct_alloc(class);
                    return (p, zero);
                }
                rseq::Pop::Scheduled => {
                    if let Some(r) = self.percpu_scheduled(shared, class) {
                        return r;
                    }
                    continue;
                }
                rseq::Pop::OutOfRange => cid_out_of_range(),
                rseq::Pop::Empty => {}
            }
            // SAFETY: this thread's area.
            let cid = unsafe { rseq::mm_cid(area) };
            if !self.percpu_ensure(shared, class, cap, cid, false) {
                return self.percpu_alloc_direct(shared, class);
            }
            let arena = Percpu::arena_of(shared, cid);
            let Some(a) = shared.arena(arena) else { return (ptr::null_mut(), false) };
            let want = ((cap / 2).max(1) as usize).min(MAX_CAP as usize / 2);
            let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize / 2];
            let out = &mut buf[..want];
            let claim = self.claim(shared, a.class(class), out);
            a.class(class).note_refill(claim.n);
            if claim.n == 0 {
                return (ptr::null_mut(), false);
            }
            let p = out[0];
            let rest = &mut out[1..claim.n];
            rest.reverse();
            // SAFETY: this thread's area; `rest` are plain slot pointers.
            let pushed = match unsafe { rseq::push_batch(area, t, rest, claim.zeroed, pac) } {
                Ok(k) => k,
                Err(()) => cid_out_of_range(),
            };
            if pushed < rest.len() {
                // A descending batch keeps each slab's slots contiguous,
                // which is all `release_owned` needs.
                shared.release_routed(arena, class, &rest[pushed..]);
            }
            self.count_direct_alloc(class);
            return (p, claim.zeroed);
        }
    }

    /// Give the header `(cid, class)` its stack if it has none: the
    /// idempotent fields with plain stores (every installer writes the
    /// same values, so a racing installer's stores are harmless), then
    /// `slots` and `cap` in one section ([`rseq::install`]), which refuses
    /// to publish a header this thread did not prepare (it migrated:
    /// re-read the id and try again). `false` when no stack could be
    /// allocated (the caller serves the request directly).
    ///
    /// `run` selects the run table (a run header never schedules:
    /// `MODE_OFF`, `compact_mask == u32::MAX`).
    #[inline(never)]
    pub(super) fn percpu_ensure(&mut self, shared: &Shared, class: usize, cap: u32, cid: u32, run: bool) -> bool {
        let mut cid = cid;
        loop {
            let h = if run { shared.percpu.run_header(cid, class) } else { shared.percpu.header(cid, class) };
            let Some(h) = h else { cid_out_of_range() };
            let h = h as *mut CacheHeader;
            // SAFETY: a live header; racy read of `cap`.
            if unsafe { ptr::read_volatile(&raw const (*h).cap) } != 0 {
                return true;
            }
            // The id's arena, built here if this is its first use (the
            // header is bound to it; `Shared::owner` would not build it).
            let arena = Percpu::arena_of(shared, cid);
            let Some(a) = shared.arena(arena) else { return false };
            let slots = shared.meta.alloc(CacheHeader::slots_layout(cap));
            if slots.is_null() {
                return false;
            }
            let mode = if run { MODE_OFF } else { MODE_PROBE };
            // SAFETY: a live header not yet published (`cap == 0`); these
            // stores are identical from every installer of this header.
            unsafe {
                ptr::write_volatile(&raw mut (*h).arena, arena);
                CacheHeader::set_mode_volatile(h, mode);
                ptr::write_volatile(&raw mut (*h).probe_refilled, a.class(class).refilled() as u32);
            }
            let t = if run { shared.percpu.run_table(class) } else { shared.percpu.table(class) };
            // SAFETY: this thread's area; `slots` holds `cap` pointers.
            match unsafe { rseq::install(self.rseq_area, t, h as *const u8, slots as *mut *mut u8, cap) } {
                rseq::Install::Installed => return true,
                rseq::Install::Exists => {
                    shared.meta.free(slots, CacheHeader::slots_layout(cap));
                    return true;
                }
                rseq::Install::Moved => {
                    shared.meta.free(slots, CacheHeader::slots_layout(cap));
                    // SAFETY: this thread's area.
                    cid = unsafe { rseq::mm_cid(self.rseq_area) };
                }
                rseq::Install::OutOfRange => cid_out_of_range(),
            }
        }
    }

    /// Push a verified slot onto the running CPU's cache, flushing half of
    /// it first when it is full (the per-CPU [`Heap::free`] for a full or
    /// uninstalled cache and for `zero_on_free`, which clears here).
    #[cold]
    #[inline(never)]
    pub(super) fn percpu_free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        if shared.zero_on_free() {
            // SAFETY: `p` is a slot start of `class` (the caller's check).
            unsafe { zero_slot(class, p) };
        }
        self.tick(shared);
        let cap = Self::percpu_cap(shared, class);
        if cap == 0 {
            shared.free_direct(shared.arena_of_slot(class, p), class, p);
            self.count_direct_free(class);
            return;
        }
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        loop {
            // SAFETY: this thread's area; `p` is a verified slot of `class`.
            match unsafe { rseq::push(area, t, p, pac) } {
                rseq::Push::Pushed => {
                    self.count_direct_free(class);
                    return;
                }
                rseq::Push::OutOfRange => cid_out_of_range(),
                rseq::Push::Full => {}
            }
            // SAFETY: this thread's area.
            let cid = unsafe { rseq::mm_cid(area) };
            if !self.percpu_ensure(shared, class, cap, cid, false) {
                shared.free_direct(shared.arena_of_slot(class, p), class, p);
                self.count_direct_free(class);
                return;
            }
            self.percpu_flush(shared, class, cap, |len| len / 2);
        }
    }

    /// Flush the running CPU's cache for `class`: take every entry out in
    /// one section (into an 8 KiB stack buffer that lives in this frame
    /// alone, `inline(never)`: inlined into `gc` and on into `tick` it
    /// deepened every slow path's frame and larson's thread stacks by
    /// pages), release the `n(len)` highest addresses to their slabs
    /// (grouped by slab, the straddling double-free compare of
    /// [`Heap::flush`] included) and push the rest back in one section,
    /// lowest on top when the stack was in address order. Entries the
    /// push-back cannot place (the thread migrated to a CPU whose cache is
    /// full) are released too.
    #[inline(never)]
    fn percpu_flush(&mut self, shared: &Shared, class: usize, cap: u32, n: impl Fn(usize) -> usize) {
        let pac = shared.pac();
        let area = self.rseq_area;
        let t = shared.percpu.table(class);
        let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize];
        let out = &mut buf[..(cap as usize).min(MAX_CAP as usize)];
        // SAFETY: this thread's area; `out` holds the cache's capacity.
        let len = match unsafe { rseq::take_all(area, t, out, pac) } {
            Ok(len) => len,
            Err(()) => cid_out_of_range(),
        };
        if len == 0 {
            return;
        }
        let all = &mut out[..len];
        let n = n(len).min(len);
        let rest_len = len - n;
        // SAFETY: this thread's area.
        let arena = Percpu::arena_of(shared, unsafe { rseq::mm_cid(area) });
        if n > 0 {
            let sorted = rest_len > 0 && all.is_sorted();
            if !sorted && rest_len > 0 {
                all.select_nth_unstable(rest_len - 1);
            }
            let (kept, high) = all.split_at_mut(rest_len);
            let low = if sorted { high[0] } else { group_by_slab(shared, class, high) };
            if rest_len > 0 && low == kept[rest_len - 1] {
                straddling_double_free(low, class);
            }
            shared.release_routed(arena, class, high);
            if sorted {
                kept.reverse();
            }
        }
        if rest_len == 0 {
            return;
        }
        let kept = &mut all[..rest_len];
        // SAFETY: this thread's area; plain slot pointers.
        let pushed = match unsafe { rseq::push_batch(area, t, kept, false, pac) } {
            Ok(k) => k,
            Err(()) => cid_out_of_range(),
        };
        if pushed < kept.len() {
            let rem = &mut kept[pushed..];
            group_by_slab(shared, class, rem);
            shared.release_routed(arena, class, rem);
        }
    }

    /// Return every entry of the running CPU's caches to the slabs
    /// (`thread.tcache.flush`, `malloc_trim`, the purges' caller flush).
    #[inline(never)]
    pub(super) fn percpu_drain(&mut self, shared: &Shared) {
        for class in 0..NCLASSES {
            let cap = Self::percpu_cap(shared, class);
            if cap == 0 {
                continue;
            }
            if self.percpu_current_has(shared, class, false) {
                self.percpu_flush(shared, class, cap, |len| len);
            }
            if self.percpu_current_has(shared, class, true) {
                self.percpu_run_drain(shared, class);
            }
        }
    }

    /// Return every entry of the running id's run for `class` to the slabs
    /// (`take_all` on the run header, grouped by slab, routed by owner).
    #[inline(never)]
    fn percpu_run_drain(&mut self, shared: &Shared, class: usize) {
        let area = self.rseq_area;
        let mut buf = [ptr::null_mut::<u8>(); MAX_CAP as usize / 4];
        let out = &mut buf[..(Self::percpu_run_cap(shared, class) as usize).min(MAX_CAP as usize / 4)];
        // SAFETY: this thread's area; `out` holds the run's capacity.
        let len = match unsafe { rseq::take_all(area, shared.percpu.run_table(class), out, shared.pac()) } {
            Ok(len) => len,
            Err(()) => cid_out_of_range(),
        };
        if len == 0 {
            return;
        }
        // SAFETY: this thread's area.
        let arena = Percpu::arena_of(shared, unsafe { rseq::mm_cid(area) });
        let all = &mut out[..len];
        group_by_slab(shared, class, all);
        shared.release_routed(arena, class, all);
    }

    /// The running CPU's caches' GC (design 6.4): release three quarters of
    /// each class's `low_water`, the entries unused since the last GC; the
    /// push-back restarts `low_water` at the kept length.
    #[inline(never)]
    pub(super) fn percpu_gc(&mut self, shared: &Shared) {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        for class in 0..NCLASSES {
            let cap = Self::percpu_cap(shared, class);
            let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
            // SAFETY: a live header; racy hints.
            let (hcap, lw) = unsafe { (ptr::read_volatile(&raw const (*h).cap), ptr::read_volatile(&raw const (*h).low_water)) };
            if cap == 0 || hcap == 0 || lw == 0 {
                continue;
            }
            let lw = lw as usize;
            self.percpu_flush(shared, class, cap, move |len| (lw - lw / 4).min(len));
        }
        // A run left behind by a class that stopped compacting is returned
        // whole; a compacting class keeps its run (it is being consumed).
        for class in 0..NCLASSES {
            let Some(h) = shared.percpu.header(cid, class) else { cid_out_of_range() };
            // SAFETY: a live header; racy hint.
            let mode = unsafe { ptr::read_volatile(&raw const (*h).mode) };
            if mode != MODE_COMPACT && self.percpu_current_has(shared, class, true) {
                self.percpu_run_drain(shared, class);
            }
        }
    }

    /// Whether the running CPU's header (or, with `run`, its run header)
    /// for `class` is installed and holds anything (a racy hint that saves
    /// a drain the empty section).
    pub(super) fn percpu_current_has(&self, shared: &Shared, class: usize, run: bool) -> bool {
        // SAFETY: this thread's area.
        let cid = unsafe { rseq::mm_cid(self.rseq_area) };
        let h = if run { shared.percpu.run_header(cid, class) } else { shared.percpu.header(cid, class) };
        let Some(h) = h else { cid_out_of_range() };
        // SAFETY: a live header; racy reads.
        unsafe { ptr::read_volatile(&raw const (*h).cap) != 0 && ptr::read_volatile(&raw const (*h).len) != 0 }
    }
}

/// Off Linux the mode never turns on (`Heap::percpu` is never set); the
/// slow paths still compile.
#[cfg(not(target_os = "linux"))]
impl Heap {
    pub(super) fn percpu_alloc(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        self.percpu_alloc_slow(shared, class)
    }

    pub(super) fn percpu_free(&mut self, shared: &Shared, p: *mut u8) {
        match shared.classify_free(p) {
            Ok((class, _)) => self.percpu_free_slow(shared, class, p),
            Err(page) => self.free_huge(shared, page),
        }
    }

    pub(super) fn percpu_alloc_slow(&mut self, shared: &Shared, class: usize) -> (*mut u8, bool) {
        let arena = self.arena;
        self.alloc_slow_in(shared, arena, class)
    }

    pub(super) fn percpu_free_slow(&mut self, shared: &Shared, class: usize, p: *mut u8) {
        let arena = self.arena;
        self.free_slow_in(shared, arena, class, p)
    }

    pub(super) fn percpu_drain(&mut self, _shared: &Shared) {}

    pub(super) fn percpu_gc(&mut self, _shared: &Shared) {}
}

impl Percpu {
    /// The arena bound to concurrency id `cid`: `cid % narenas` over the
    /// automatic arenas, so with the default `narenas` (`4 * ncpus`, at
    /// most 64) every id has its own arena up to 64 CPUs.
    #[inline]
    pub fn arena_of(shared: &Shared, cid: u32) -> u16 {
        (cid % shared.arenas.auto().max(1)) as u16
    }
}

/// The kernel wrote an `mm_cid` at or above the possible-CPU count the
/// table was sized for (`rseq::possible_cpus`): the sizing rule is wrong
/// for this kernel, and the heap must not continue on a guess.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[cold]
#[inline(never)]
fn cid_out_of_range() -> ! {
    panic!("rseq mm_cid at or above the possible-CPU count: the per-CPU table is too small")
}
