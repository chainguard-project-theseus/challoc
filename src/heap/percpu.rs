//! The per-CPU mode (design 15): the [`Percpu`] table of [`CpuCache`]s
//! keyed by rseq `mm_cid`, the per-CPU `impl Heap` blocks (pop, refill,
//! flush, GC, drain) and the entry points' push wrappers.

use core::ptr;

use crate::config::Config;
use crate::layout::NCLASSES;
use crate::meta::Meta;
use crate::rseq;

use super::{CACHE_HEADER, CacheHeader, Heap, Shared};
#[cfg(target_os = "linux")]
use super::flush::{group_by_slab, straddling_double_free};
#[cfg(target_os = "linux")]
use super::{MAX_CAP, MODE_COMPACT, MODE_OFF, MODE_PROBE, PROBE_MASK, PUSH_PAC, PUSH_ZERO, Verdict, zero_slot};


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
/// mode ([`Shared::percpu_on`], the [`PUSH_PERCPU`] bit) keys its plain
/// `malloc`/`free` caches by the running thread's rseq `mm_cid` instead of
/// by thread: the table of [`CpuCache`]s in [`Percpu`], one header per
/// (concurrency id, class), every header operation a critical section of
/// `rseq.rs` that reads the id, so no lock and no atomic read-modify-write
/// is added to the fast paths and two threads never mutate one header at
/// the same time (an id is held by one running thread; a thread preempted
/// inside a section is moved to the abort handler and retries). The thread
/// heap stays: it is the init witness the fast paths already load, it
/// keeps the thread's byte counters (every per-CPU pop and push is counted
/// there, `thread.allocated` stays exact), its home caches serve explicit
/// `MALLOCX_ARENA` traffic and huge runs as before, and a heap off the mode
/// ([`Heap::percpu`] false) takes the thread paths.
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

    /// The pop of a heap in a per-CPU process: the [`PUSH_PERCPU`] branch
    /// of [`Heap::alloc_fast`], [`Heap::alloc_zeroed`] and the entry
    /// points' per-CPU wrappers (`api::libc::percpu_malloc`), which inline
    /// it: the section itself sits in the wrapper the entry point
    /// tail-calls, so the mode's pop is one jump, the `percpu` byte, the
    /// section and the byte count, with no further call (an out-of-line
    /// call here and then into `rseq::pop`'s wrapper costs two frames and
    /// the decode, +65 instructions per larson operation on the C4;
    /// `bench/results/2026-09-19-*-task8-percpu-fastpath.md`). A heap off
    /// the mode pops its own cache out of line ([`Heap::pop_off_mode`],
    /// cold, so the thread pop's registers never join this frame). `None`
    /// sends the caller to the slow path.
    #[inline(always)]
    pub(crate) fn pop_percpu_mode(&mut self, class: usize, flags: u8) -> Option<(*mut u8, bool)> {
        let (p, st) = self.percpu_pop_raw(class, flags);
        if st & rseq::POP_HIT != 0 { Some((p, st & rseq::POP_ZERO != 0)) } else { self.percpu_pop_miss(class, flags, st) }
    }

    /// The hit path of the per-CPU pop, raw: the section on the running
    /// CPU's header for `class` and its status word (`rseq::POP_*`;
    /// [`rseq::POP_OFF_MODE`] for a heap off the mode, before any section),
    /// the byte count taken on a hit. The entry points inline this and
    /// test [`rseq::POP_HIT`]; everything else goes to
    /// [`Heap::percpu_pop_miss`] in one cold tail call, which is what
    /// keeps the wrapper frameless (a miss that returned here to be
    /// decoded gave it a frame and the decode).
    #[inline(always)]
    pub(crate) fn percpu_pop_raw(&mut self, class: usize, flags: u8) -> (*mut u8, u32) {
        if !self.percpu {
            return (ptr::null_mut(), rseq::POP_OFF_MODE);
        }
        // SAFETY: a heap never outlives its `Shared`.
        let shared: &Shared = unsafe { &*self.shared };
        // SAFETY: the owning thread registered this area; the table is live.
        let (p, st) = unsafe { rseq::pop_raw(self.rseq_area, shared.percpu.table(class), flags & PUSH_PAC != 0) };
        if st & rseq::POP_HIT != 0 {
            self.count_direct_alloc(class);
        }
        (p, st)
    }

    /// Every outcome of [`Heap::percpu_pop_raw`] but a hit: the thread pop
    /// of a heap off the mode (an explicit cache, `thread.tcache.enabled:
    /// false`, a thread homed on an explicit arena; [`Heap::alloc_fast`]'s
    /// body), the scheduled pop, an empty cache (`None`: the caller
    /// refills), an id out of range (abort).
    #[cold]
    #[inline(never)]
    pub(crate) fn percpu_pop_miss(&mut self, class: usize, flags: u8, st: u32) -> Option<(*mut u8, bool)> {
        match st {
            rseq::POP_OFF_MODE => {
                let cache = self.cache_ptr(class);
                // SAFETY: this thread's own header, unused or with a stack.
                unsafe {
                    let len = (*cache).len;
                    if len == 0 {
                        return None;
                    }
                    Some(self.pop(cache, class, len, flags & PUSH_PAC != 0))
                }
            }
            rseq::POP_EMPTY => None,
            rseq::POP_SCHEDULED => {
                // SAFETY: a heap never outlives its `Shared`.
                let shared: &Shared = unsafe { &*self.shared };
                self.percpu_scheduled(shared, class)
            }
            _ => cid_out_of_range(),
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
                        unsafe { CacheHeader::set_mode_volatile(h, MODE_PROBE, &shared.config) };
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
                    CacheHeader::set_mode_volatile(h, MODE_COMPACT, &shared.config);
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
                unsafe { CacheHeader::set_mode_volatile(h, MODE_PROBE, &shared.config) };
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
            let mode = if run || shared.config.tcache_compact_shift == 0 { MODE_OFF } else { MODE_PROBE };
            // SAFETY: a live header not yet published (`cap == 0`); these
            // stores are identical from every installer of this header.
            unsafe {
                ptr::write_volatile(&raw mut (*h).arena, arena);
                CacheHeader::set_mode_volatile(h, mode, &shared.config);
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

/// Off Linux the mode never turns on; the entry points still compile.
#[cfg(not(target_os = "linux"))]
impl Heap {
    pub(crate) fn pop_percpu_mode(&mut self, class: usize, flags: u8) -> Option<(*mut u8, bool)> {
        let (_, st) = self.percpu_pop_raw(class, flags);
        self.percpu_pop_miss(class, flags, st)
    }

    pub(crate) fn percpu_pop_raw(&mut self, _class: usize, _flags: u8) -> (*mut u8, u32) {
        (ptr::null_mut(), rseq::POP_EMPTY)
    }

    pub(crate) fn percpu_pop_miss(&mut self, _class: usize, _flags: u8, _st: u32) -> Option<(*mut u8, bool)> {
        None
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

/// The `free` and sized-free fast paths of a process in the per-CPU mode
/// (design 15), raw. The entry points (`api::libc`) test [`PUSH_PERCPU`]
/// on the flag byte they load anyway and tail-call their per-CPU wrapper,
/// which inlines this and compares the status with [`rseq::PUSH_PUSHED`];
/// every other outcome is one cold tail call to [`percpu_push_miss`], so
/// the thread paths keep their shape (no frame, as the disassembly of
/// `malloc`/`free` in `bench/results/2026-09-19-*-task8-percpu*.md`
/// records) and
/// the mode's push is one jump, the check, the section and the byte count
/// with no call. A heap off the mode ([`Heap::percpu`] false) reports
/// [`rseq::PUSH_OFF_MODE`] before any check; otherwise the slot is
/// verified (the bit test without the arena test,
/// [`Shared::checked_slot_any`], or the sized path's arithmetic with the
/// hint; [`rseq::PUSH_DECLINED`] when it is not, or under `zero_on_free`,
/// which clears on the slow path before the push), pushed onto the running
/// CPU's cache in one critical section and counted in the thread's byte
/// counters.
#[cfg(target_os = "linux")]
#[inline(always)]
pub fn percpu_push_raw(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, hint: Option<usize>, sized: bool) -> u32 {
    if !h.percpu {
        return rseq::PUSH_OFF_MODE;
    }
    let class = if sized { shared.sized_slot(p, hint) } else { shared.checked_slot_any(p) };
    let Some(class) = class else { return rseq::PUSH_DECLINED };
    if flags & PUSH_ZERO != 0 {
        return rseq::PUSH_DECLINED;
    }
    // SAFETY: the heap's thread registered this area; the table is live;
    // `p` is a verified slot start of `class`.
    let st = unsafe { rseq::push_raw(h.rseq_area, shared.percpu.table(class), p, flags & PUSH_PAC != 0) };
    if st == rseq::PUSH_PUSHED {
        h.count_direct_free(class);
    }
    st
}

/// Every outcome of [`percpu_push_raw`] but a push: the thread push of a
/// heap off the mode ([`free_fast`] or [`free_sized_fast`]), an id out of
/// range (abort), and `false` for a full cache or a declined pointer,
/// which the caller sends to its exact slow path.
#[cfg(target_os = "linux")]
#[cold]
#[inline(never)]
pub fn percpu_push_miss(shared: &Shared, h: &mut Heap, p: *mut u8, flags: u8, hint: Option<usize>, sized: bool, st: u32) -> bool {
    match st {
        rseq::PUSH_OFF_MODE => {
            let class = if sized { shared.sized_slot(p, hint) } else { shared.checked_slot(p, h) };
            match class {
                Some(class) => h.push_fast(class, p, flags),
                None => false,
            }
        }
        rseq::PUSH_OUT_OF_RANGE => cid_out_of_range(),
        _ => false,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn percpu_push_raw(_: &Shared, _: &mut Heap, _: *mut u8, _: u8, _: Option<usize>, _: bool) -> u32 {
    rseq::PUSH_DECLINED
}

#[cfg(not(target_os = "linux"))]
pub fn percpu_push_miss(_: &Shared, _: &mut Heap, _: *mut u8, _: u8, _: Option<usize>, _: bool, _: u32) -> bool {
    false
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
