//! `mallctl`, `mallctlnametomib` and `mallctlbymib` (design.md section
//! 13.4), with the key list chosen from what Redis, Valkey, Ruby and the
//! jemalloc client libraries actually call.
//!
//! Names are resolved against a static tree ([`ROOT`]). A MIB is the path
//! of child indices through that tree, one `size_t` per component, and a
//! numeric component (`arena.<i>`, `arenas.bin.<j>`, `stats.arenas.<i>`,
//! `stats.arenas.<i>.bins.<j>`) is stored as the number itself. That is
//! jemalloc's encoding and Valkey's defragmenter relies on it: it resolves
//! `stats.arenas.0.bins.0.curregs` once with `mallctlnametomib`, then sets
//! `mib[2]` to the arena and `mib[4]` to the bin before each
//! `mallctlbymib`. `opt.<key>`'s second component is the key's index in
//! `config::KEYS`.
//!
//! Error codes follow jemalloc: `ENOENT` for a name that is not in the
//! tree (including every `prof.*` key), an interior node used as a leaf,
//! or an arena or bin index that does not exist; `EPERM` for writing a
//! read-only key or reading a write-only one; `EINVAL` when `*oldlenp` or
//! `newlen` is not the value's size (the readable prefix is still copied,
//! as jemalloc does); `EFAULT` for an arena that cannot be used as asked
//! (`thread.arena` to an index that does not exist, `arena.0.destroy`);
//! `EAGAIN` when a resource could not be created.
//!
//! A read-only key carries its answer in the tree: a [`Reader`] on the
//! node (`stat(...)`), called by [`ctl`] after the read-only check, with
//! the numeric MIB components as its argument. Adding such a key is one
//! `Leaf` variant and one `stat` line. The keys that do something (`epoch`,
//! the `arena.<i>.*` operations, `arenas.create`/`lookup`, the `thread.*`
//! and `tcache.*` writes, the utilization queries) are arms of `ctl`.
//!
//! `stats.*` values are those stored by the last `epoch` (all zero before
//! the first), as in jemalloc; `stats.arenas.4096.*` (`MALLCTL_ARENAS_ALL`)
//! sums every existing arena and `stats.arenas.4097.*`
//! (`MALLCTL_ARENAS_DESTROYED`) is all zero, see [`ARENAS_DESTROYED`]. The
//! type of each value ([`Reading`]) is jemalloc's: `size_t` for byte and
//! page counts (`opt.tcache_max` and `opt.huge_retain_max` included: the
//! `opt.*` types are the `config::Value` variants), `unsigned` for
//! indices and counts of arenas, bins and slabs (`opt.narenas`,
//! `opt.tcache_nslots_small_max`), `uint32_t` for
//! `arenas.bin.<j>.nregs`, `ssize_t` for decay times and the other signed
//! options, `uint64_t` for `epoch` and the thread counters, `bool` (one
//! byte) for flags, `const char *` for strings.
//!
//! `experimental.utilization.query` fills jemalloc's
//! `extent_util_stats_verbose_t` (`slabcur_addr, nfree, nregs, size,
//! bin_nfree, bin_nregs`) and `batch_query` one `extent_util_stats_t`
//! (`nfree, nregs, size`) per pointer. For a slab slot `nfree` is
//! `nslots - live` from the descriptor, `size` the slab size, the bin
//! figures come from the class state (one lock hold, verbose query only)
//! and `slabcur_addr` is the lowest partial slab of the class in the
//! pointer's arena, the slab a refill would take from next. A huge run is
//! its own extent: `nfree` 0, `nregs` 1, `size` its usable bytes. A pointer
//! that is not a live allocation gets zeros rather than an abort: the
//! query is a statistics read, not a `free`.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::size_of;
use core::ptr;

use crate::QUANTUM;
use crate::arena::{ARENAS_ALL, MAX_ARENAS};
use crate::config::{KEYS, Value};
use crate::heap::{self, Heap, Shared};
use crate::huge::RunState;
use crate::layout::{CLASSES, Kind, NBINS};
use crate::purge;
use crate::stats::{self, ArenaStats, BinStats};
use crate::sys;
use crate::sys::Errno;
use Leaf::*;
use Reading::*;

// `mallctl`'s return codes as `c_int`; the numbers live in `sys::errno`.
const EPERM: c_int = Errno::EPERM.0;
const ENOENT: c_int = Errno::ENOENT.0;
const EAGAIN: c_int = Errno::EAGAIN.0;
const EFAULT: c_int = Errno::EFAULT.0;
const EINVAL: c_int = Errno::EINVAL.0;

/// `version`: jemalloc's `<major>.<minor>.<bugfix>-<nrev>-g<gid>` shape,
/// so parsers that split on `-` and `.` (tikv-jemalloc-ctl, Redis's
/// `mem_allocator` line) work; the "gid" says who this really is.
pub const VERSION_STR: &str = "5.3.0-0-gchalloc";
const VERSION_CSTR: &[u8] = b"5.3.0-0-gchalloc\0";
const _: () = assert!(VERSION_CSTR.len() == VERSION_STR.len() + 1);
const EMPTY_CSTR: &[u8] = b"\0";

/// Longest MIB (`stats.arenas.<i>.bins.<j>.curregs` has 6 components).
pub const MAX_MIB: usize = 8;
/// Longest name accepted.
const MAX_NAME: usize = 256;

/// Every readable or writable key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Leaf {
    Version,
    Epoch,
    BackgroundThread,
    MaxBackgroundThreads,
    /// `opt.<key>`; `mib[1]` indexes `config::KEYS`.
    Opt,
    ArenasNarenas,
    ArenasNbins,
    ArenasNlextents,
    ArenasPage,
    ArenasQuantum,
    /// `arenas.bin.<j>.*`; `mib[2]` is the bin.
    ArenasBinSize,
    ArenasBinNregs,
    ArenasBinSlabSize,
    ArenasCreate,
    ArenasLookup,
    /// `arena.<i>.*`; `mib[1]` is the arena (or `MALLCTL_ARENAS_ALL`).
    ArenaPurge,
    ArenaDecay,
    ArenaDestroy,
    ArenaDirtyDecayMs,
    StatsAllocated,
    StatsActive,
    StatsMetadata,
    StatsResident,
    StatsMapped,
    StatsRetained,
    /// `stats.cgroup.*` (design 11): `ENOENT` when no cgroup is tracked.
    StatsCgroupLimit,
    StatsCgroupHigh,
    StatsCgroupCurrent,
    StatsCgroupPressureEvents,
    /// The policy's outcome (design 11, `policy.rs`): arena 0's effective
    /// decay period in ms (`SIZE_MAX` for never), the retention cap in
    /// bytes (`SIZE_MAX` without a limit), the `Empty` bytes held now, and
    /// the full purges run on `memory.events` `high` rises.
    StatsCgroupDecayMs,
    StatsCgroupRetentionCap,
    StatsCgroupEmptyBytes,
    StatsCgroupFullPurges,
    /// The two-advice purge (design 8): the advice
    /// the next purge issues (1 lazy, `MADV_FREE`; 0 eager,
    /// `MADV_DONTNEED`) and the batches issued with each so far. Readable
    /// without a tracked cgroup too: the advice exists (eager without
    /// one) whether or not a cgroup is read.
    StatsCgroupPurgeAdvice,
    StatsCgroupLazyPurges,
    StatsCgroupEagerPurges,
    /// `stats.thp.*` (design 7 and 17, `thp.rs`): the mode in force
    /// (0 off, 1 folios, 2 PMD chunks) and the ranges advised per region
    /// kind. Read-only, `size_t`.
    StatsThpMode,
    StatsThpClassRanges,
    StatsThpClassBytes,
    StatsThpHugeRanges,
    StatsThpMetaRanges,
    StatsThpFailures,
    /// `stats.prefault.*` (design 8.2, `purge::prefault`):
    /// whether the pass runs, whether `MADV_POPULATE_WRITE` is the populate,
    /// and its counters. Read-only, `size_t`.
    StatsPrefaultEnabled,
    StatsPrefaultPopulateWrite,
    StatsPrefaultPasses,
    StatsPrefaultSlabs,
    StatsPrefaultBytes,
    StatsPrefaultSkipped,
    StatsPrefaultFallbacks,
    StatsPrefaultWakes,
    StatsPrefaultActive,
    /// `stats.thp.nohuge_ranges` (design 17, `thp.rs`): the
    /// reservations `never` `MADV_NOHUGEPAGE`d, the class superblock and
    /// the metadata region (2 on a kernel with THP; the huge region follows
    /// the host's policy, design 17); 0 under `auto` and `always`.
    StatsThpNohugeRanges,
    /// `stats.arenas.<i>.*`; `mib[2]` is the arena.
    StatsArenaPactive,
    StatsArenaPdirty,
    StatsArenaPmuzzy,
    StatsArenaSmallAllocated,
    StatsArenaSmallResident,
    StatsArenaLargeAllocated,
    StatsArenaResident,
    StatsArenaMapped,
    StatsArenaRetained,
    /// `stats.arenas.<i>.bins.<j>.*`; `mib[2]` is the arena, `mib[4]` the bin.
    StatsArenaBinCurregs,
    StatsArenaBinCurslabs,
    StatsArenaBinNonfullSlabs,
    ThreadTcacheFlush,
    ThreadTcacheEnabled,
    ThreadAllocated,
    ThreadAllocatedp,
    ThreadDeallocated,
    ThreadDeallocatedp,
    ThreadArena,
    TcacheCreate,
    TcacheFlush,
    TcacheDestroy,
    UtilQuery,
    UtilBatchQuery,
    ConfigMallocConf,
    ConfigDebug,
    ConfigStats,
}

/// `MALLCTL_ARENAS_DESTROYED` (4097 in both shipped headers): jemalloc's
/// merged statistics of the arenas destroyed so far. challoc never
/// destroys an arena (`arena.<i>.destroy` purges it and keeps it
/// registered, design 13.4), so `stats.arenas.4097.*` answers zeros and
/// `arena.4097.*` is `ENOENT` like every index at or above `MAX_ARENAS`.
const ARENAS_DESTROYED: usize = 4097;
const _: () = assert!(ARENAS_DESTROYED >= MAX_ARENAS && ARENAS_DESTROYED != ARENAS_ALL as usize);

/// A read-only key's answer, tagged with the C type the caller's buffer
/// must have (`*oldlenp` is checked against it).
#[derive(Copy, Clone)]
enum Reading {
    /// `bool`, one byte.
    Bool(bool),
    /// `unsigned`.
    Unsigned(c_uint),
    /// `uint32_t`.
    U32(u32),
    /// `size_t`.
    Size(usize),
    /// `ssize_t`.
    Ssize(isize),
    /// `uint64_t`.
    U64(u64),
    /// `const char *`, a NUL-terminated static string.
    Str(*const c_char),
    /// `uint64_t *` (`thread.allocatedp`, `thread.deallocatedp`).
    U64Ptr(*const u64),
}

/// A read-only key's implementation: the MIB (for its numeric components)
/// in, the answer or an error code out. The allocator state is [`shared`],
/// not a parameter: a reader is called through a pointer, and a `&Shared`
/// argument the optimiser cannot trace to the one static would cost every
/// function downstream (`heap_for_thread`, `stats::snapshot`,
/// `Config::get`) the constant it is specialised on today.
type Reader = fn(&[usize]) -> Result<Reading, c_int>;

/// The allocator state, initialised (the readers run inside `mallctl`,
/// after its own [`super::shared`]).
fn shared() -> &'static Shared {
    super::shared()
}

/// One node of the name tree. A node named [`NUMERIC`] is the only child
/// of its parent and matches any decimal component, which becomes the MIB
/// entry. A leaf with a `read` is a read-only key answered by it; a leaf
/// without one is an arm of [`ctl`].
#[derive(Copy, Clone)]
struct Node {
    name: &'static str,
    leaf: Option<Leaf>,
    read: Option<Reader>,
    children: &'static [Node],
}

const NUMERIC: &str = "*";

const fn leaf(name: &'static str, l: Leaf) -> Node {
    Node { name, leaf: Some(l), read: None, children: &[] }
}

const fn stat(name: &'static str, l: Leaf, read: Reader) -> Node {
    Node { name, leaf: Some(l), read: Some(read), children: &[] }
}

const fn dir(name: &'static str, children: &'static [Node]) -> Node {
    Node { name, leaf: None, read: None, children }
}

const fn num(children: &'static [Node]) -> Node {
    Node { name: NUMERIC, leaf: None, read: None, children }
}

const fn opt_nodes() -> [Node; KEYS.len()] {
    let mut out = [dir("", &[]); KEYS.len()];
    let mut i = 0;
    while i < KEYS.len() {
        out[i] = stat(KEYS[i], Leaf::Opt, opt_value);
        i += 1;
    }
    out
}

static OPT: [Node; KEYS.len()] = opt_nodes();
static BIN_GEOMETRY: [Node; 3] = [
    stat("size", ArenasBinSize, |m| Ok(Size(bin_class(m)?.size()))),
    stat("nregs", ArenasBinNregs, |m| Ok(U32(bin_class(m)?.nslots() as u32))),
    stat("slab_size", ArenasBinSlabSize, |m| Ok(Size(bin_class(m)?.slab_size()))),
];
static ARENAS_BIN: [Node; 1] = [num(&BIN_GEOMETRY)];
static ARENAS: [Node; 8] = [
    stat("narenas", ArenasNarenas, |_| Ok(Unsigned(shared().arenas.count() as c_uint))),
    stat("nbins", ArenasNbins, |_| Ok(Unsigned(NBINS as c_uint))),
    stat("nlextents", ArenasNlextents, |_| Ok(Unsigned(0))),
    stat("page", ArenasPage, |_| Ok(Size(shared().regions.page_size()))),
    stat("quantum", ArenasQuantum, |_| Ok(Size(QUANTUM))),
    dir("bin", &ARENAS_BIN),
    leaf("create", ArenasCreate),
    leaf("lookup", ArenasLookup),
];
static ARENA_I: [Node; 4] = [
    leaf("purge", ArenaPurge),
    leaf("decay", ArenaDecay),
    leaf("destroy", ArenaDestroy),
    leaf("dirty_decay_ms", ArenaDirtyDecayMs),
];
static ARENA: [Node; 1] = [num(&ARENA_I)];
static STATS_BIN_FIELDS: [Node; 3] = [
    stat("curregs", StatsArenaBinCurregs, |m| Ok(Size(bin_stats(m)?.curregs))),
    stat("curslabs", StatsArenaBinCurslabs, |m| Ok(Size(bin_stats(m)?.curslabs))),
    stat("nonfull_slabs", StatsArenaBinNonfullSlabs, |m| Ok(Size(bin_stats(m)?.nonfull_slabs))),
];
static STATS_BINS: [Node; 1] = [num(&STATS_BIN_FIELDS)];
static STATS_SMALL: [Node; 2] = [
    stat("allocated", StatsArenaSmallAllocated, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.small_allocated))),
    stat("resident", StatsArenaSmallResident, |m| {
        let a = arena_stats(shared(), at(m, 2))?;
        Ok(Size(a.small_active + a.small_dirty))
    }),
];
static STATS_LARGE: [Node; 1] =
    [stat("allocated", StatsArenaLargeAllocated, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.large_allocated)))];
static STATS_ARENA_I: [Node; 9] = [
    stat("pactive", StatsArenaPactive, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.pactive))),
    stat("pdirty", StatsArenaPdirty, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.pdirty))),
    stat("pmuzzy", StatsArenaPmuzzy, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.pmuzzy))),
    dir("small", &STATS_SMALL),
    dir("large", &STATS_LARGE),
    stat("resident", StatsArenaResident, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.resident))),
    stat("mapped", StatsArenaMapped, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.mapped))),
    stat("retained", StatsArenaRetained, |m| Ok(Size(arena_stats(shared(), at(m, 2))?.retained))),
    dir("bins", &STATS_BINS),
];
static STATS_ARENAS: [Node; 1] = [num(&STATS_ARENA_I)];
// `stats.cgroup.*` (design 11): the first eight need a tracked cgroup
// (`tracked`); the policy's figures are arena 0's effective decay period
// in ms (`SIZE_MAX` for never), the retention cap in bytes (`SIZE_MAX`
// without a limit), the `Empty` bytes held now (live, not the last
// evaluation's figure: a reader without a limit would otherwise see 0)
// and the full purges run on `memory.events` `high` rises. The last three
// read with or without a cgroup: the advice exists (eager without one)
// whether or not one is read.
static STATS_CGROUP: [Node; 11] = [
    stat("limit", StatsCgroupLimit, |_| Ok(Size(tracked()?.limit() as usize))),
    stat("high", StatsCgroupHigh, |_| Ok(Size(tracked()?.high() as usize))),
    stat("current", StatsCgroupCurrent, |_| {
        // Rate limited to once per 10 ms, so a polling reader costs at
        // most that many file reads.
        let cg = tracked()?;
        cg.sample(sys::now_coarse_ns());
        Ok(Size(cg.current() as usize))
    }),
    stat("pressure_events", StatsCgroupPressureEvents, |_| Ok(Size(tracked()?.pressure_events() as usize))),
    stat("decay_ms", StatsCgroupDecayMs, |_| {
        tracked()?;
        Ok(Size(match shared().policy.effective_decay_ns(shared().decay_ns()) {
            purge::NEVER => usize::MAX,
            ns => (ns / 1_000_000) as usize,
        }))
    }),
    stat("retention_cap", StatsCgroupRetentionCap, |_| {
        tracked()?;
        Ok(Size(shared().policy.cap() as usize))
    }),
    stat("empty_bytes", StatsCgroupEmptyBytes, |_| {
        tracked()?;
        Ok(Size(crate::policy::empty_bytes(shared()) as usize))
    }),
    stat("full_purges", StatsCgroupFullPurges, |_| {
        tracked()?;
        Ok(Size(shared().policy.full_purges() as usize))
    }),
    stat("purge_advice", StatsCgroupPurgeAdvice, |_| Ok(Size((purge::advice() == purge::Advice::Lazy) as usize))),
    stat("lazy_purges", StatsCgroupLazyPurges, |_| Ok(Size(purge::batches().0 as usize))),
    stat("eager_purges", StatsCgroupEagerPurges, |_| Ok(Size(purge::batches().1 as usize))),
];
static STATS_THP: [Node; 7] = [
    stat("mode", StatsThpMode, |_| Ok(Size(crate::thp::decision().mode as usize))),
    stat("class_ranges", StatsThpClassRanges, |_| Ok(Size(crate::thp::stats().class_ranges as usize))),
    stat("class_bytes", StatsThpClassBytes, |_| Ok(Size(crate::thp::stats().class_bytes as usize))),
    stat("huge_ranges", StatsThpHugeRanges, |_| Ok(Size(crate::thp::stats().huge_ranges as usize))),
    stat("meta_ranges", StatsThpMetaRanges, |_| Ok(Size(crate::thp::stats().meta_ranges as usize))),
    stat("failures", StatsThpFailures, |_| Ok(Size(crate::thp::stats().failures as usize))),
    stat("nohuge_ranges", StatsThpNohugeRanges, |_| Ok(Size(crate::thp::stats().nohuge_ranges as usize))),
];
static STATS_PREFAULT: [Node; 9] = [
    stat("enabled", StatsPrefaultEnabled, |_| Ok(Size(purge::prefault::enabled() as usize))),
    stat("populate_write", StatsPrefaultPopulateWrite, |_| Ok(Size(purge::prefault::uses_populate_write() as usize))),
    stat("passes", StatsPrefaultPasses, |_| Ok(Size(purge::prefault::stats().passes as usize))),
    stat("slabs", StatsPrefaultSlabs, |_| Ok(Size(purge::prefault::stats().slabs as usize))),
    stat("bytes", StatsPrefaultBytes, |_| Ok(Size(purge::prefault::stats().bytes as usize))),
    stat("skipped", StatsPrefaultSkipped, |_| Ok(Size(purge::prefault::stats().skipped as usize))),
    stat("fallbacks", StatsPrefaultFallbacks, |_| Ok(Size(purge::prefault::stats().fallbacks as usize))),
    stat("wakes", StatsPrefaultWakes, |_| Ok(Size(purge::prefault::stats().wakes as usize))),
    stat("active", StatsPrefaultActive, |_| Ok(Size(purge::prefault::stats().active as usize))),
];
static STATS: [Node; 10] = [
    stat("allocated", StatsAllocated, |_| Ok(Size(stats::snapshot(shared()).totals.allocated))),
    stat("active", StatsActive, |_| Ok(Size(stats::snapshot(shared()).totals.active))),
    stat("metadata", StatsMetadata, |_| Ok(Size(stats::snapshot(shared()).totals.metadata))),
    stat("resident", StatsResident, |_| Ok(Size(stats::snapshot(shared()).totals.resident))),
    stat("mapped", StatsMapped, |_| Ok(Size(stats::snapshot(shared()).totals.mapped))),
    stat("retained", StatsRetained, |_| Ok(Size(stats::snapshot(shared()).totals.retained))),
    dir("arenas", &STATS_ARENAS),
    dir("cgroup", &STATS_CGROUP),
    dir("thp", &STATS_THP),
    dir("prefault", &STATS_PREFAULT),
];
static THREAD_TCACHE: [Node; 2] = [leaf("flush", ThreadTcacheFlush), leaf("enabled", ThreadTcacheEnabled)];
static THREAD: [Node; 6] = [
    dir("tcache", &THREAD_TCACHE),
    stat("allocated", ThreadAllocated, |_| Ok(U64(thread_heap(shared())?.materialise_stats().allocated))),
    stat("allocatedp", ThreadAllocatedp, |_| Ok(U64Ptr(&thread_heap(shared())?.materialise_stats().allocated))),
    stat("deallocated", ThreadDeallocated, |_| Ok(U64(thread_heap(shared())?.materialise_stats().deallocated))),
    stat("deallocatedp", ThreadDeallocatedp, |_| Ok(U64Ptr(&thread_heap(shared())?.materialise_stats().deallocated))),
    leaf("arena", ThreadArena),
];
static TCACHE: [Node; 3] = [leaf("create", TcacheCreate), leaf("flush", TcacheFlush), leaf("destroy", TcacheDestroy)];
static UTILIZATION: [Node; 2] = [leaf("query", UtilQuery), leaf("batch_query", UtilBatchQuery)];
static EXPERIMENTAL: [Node; 1] = [dir("utilization", &UTILIZATION)];
static CONFIG: [Node; 3] = [
    stat("malloc_conf", ConfigMallocConf, |_| Ok(Str(EMPTY_CSTR.as_ptr() as *const c_char))),
    stat("debug", ConfigDebug, |_| Ok(Bool(false))),
    stat("stats", ConfigStats, |_| Ok(Bool(true))),
];
static ROOT: [Node; 12] = [
    stat("version", Version, |_| Ok(Str(VERSION_CSTR.as_ptr() as *const c_char))),
    leaf("epoch", Epoch),
    leaf("background_thread", BackgroundThread),
    leaf("max_background_threads", MaxBackgroundThreads),
    dir("opt", &OPT),
    dir("arenas", &ARENAS),
    dir("arena", &ARENA),
    dir("stats", &STATS),
    dir("thread", &THREAD),
    dir("tcache", &TCACHE),
    dir("experimental", &EXPERIMENTAL),
    dir("config", &CONFIG),
];

// ---------------------------------------------------------------------------
// Name and MIB resolution.
// ---------------------------------------------------------------------------

fn split_component(name: &[u8]) -> (&[u8], Option<&[u8]>) {
    match name.iter().position(|&b| b == b'.') {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    }
}

/// A decimal component (jemalloc parses these with `strtoumax(.., 10)`).
fn parse_index(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut v: usize = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(v)
}

fn child(nodes: &'static [Node], comp: &[u8]) -> Option<(usize, &'static Node)> {
    if let [n] = nodes {
        if n.name == NUMERIC {
            return parse_index(comp).map(|i| (i, n));
        }
    }
    nodes.iter().position(|n| n.name.as_bytes() == comp).map(|i| (i, &nodes[i]))
}

fn child_by_index(nodes: &'static [Node], i: usize) -> Option<&'static Node> {
    match nodes {
        [n] if n.name == NUMERIC => Some(n),
        _ => nodes.get(i),
    }
}

/// Resolve a dotted name to its MIB. The node reached may be interior
/// (`mallctlnametomib("arenas")` is valid in jemalloc); using it as a key
/// is then `ENOENT`.
fn name_to_mib(name: &[u8], mib: &mut [usize; MAX_MIB]) -> Result<(usize, &'static Node), c_int> {
    let mut nodes: &'static [Node] = &ROOT;
    let mut rest = name;
    let mut len = 0;
    loop {
        if len == MAX_MIB {
            return Err(ENOENT);
        }
        let (comp, tail) = split_component(rest);
        let (i, node) = child(nodes, comp).ok_or(ENOENT)?;
        mib[len] = i;
        len += 1;
        match tail {
            None => return Ok((len, node)),
            Some(t) => {
                if node.children.is_empty() {
                    return Err(ENOENT);
                }
                nodes = node.children;
                rest = t;
            }
        }
    }
}

/// The node a MIB names; `ENOENT` for a bad index or an empty MIB.
fn mib_to_node(mib: &[usize]) -> Result<&'static Node, c_int> {
    let mut nodes: &'static [Node] = &ROOT;
    let mut last: Option<&'static Node> = None;
    for &i in mib {
        let n = child_by_index(nodes, i).ok_or(ENOENT)?;
        nodes = n.children;
        last = Some(n);
    }
    last.ok_or(ENOENT)
}

/// The key a MIB names; `ENOENT` for an interior node or a bad index.
#[cfg(test)]
fn mib_to_leaf(mib: &[usize]) -> Result<Leaf, c_int> {
    mib_to_node(mib)?.leaf.ok_or(ENOENT)
}

/// A C string as bytes, at most [`MAX_NAME`] long. `None` for null or a
/// string of `MAX_NAME` bytes or more.
///
/// `p` is the caller's `name` argument: null, or readable up to and
/// including its NUL terminator (the `mallctl` contract); at most
/// `MAX_NAME` bytes are ever read, and the slice borrows the caller's
/// memory for the duration of the call.
fn cstr(p: *const c_char) -> Option<&'static [u8]> {
    if p.is_null() {
        return None;
    }
    let p = p.cast::<u8>();
    let mut n = 0;
    // SAFETY: the caller passes a NUL-terminated name; reads stop at the
    // terminator or the cap.
    while n < MAX_NAME && unsafe { p.add(n).read() } != 0 {
        n += 1;
    }
    if n == MAX_NAME {
        return None;
    }
    // SAFETY: `n` bytes before the terminator were just read.
    Some(unsafe { core::slice::from_raw_parts(p, n) })
}

// ---------------------------------------------------------------------------
// Value transfer with jemalloc's READ / WRITE / READONLY / WRITEONLY rules.
// ---------------------------------------------------------------------------

/// The caller's four value pointers.
#[derive(Copy, Clone)]
pub struct Io {
    pub oldp: *mut c_void,
    pub oldlenp: *mut usize,
    pub newp: *const c_void,
    pub newlen: usize,
}

impl Io {
    /// Copy `v` out if the caller asked for it. A wrong `*oldlenp` copies
    /// the common prefix and fails with `EINVAL`.
    fn read<T: Copy>(&self, v: T) -> Result<(), c_int> {
        if self.oldp.is_null() || self.oldlenp.is_null() {
            return Ok(());
        }
        let want = size_of::<T>();
        // SAFETY: `oldp`/`oldlenp` are the caller's out-pointers per the
        // `mallctl` contract; `*oldlenp` bytes are writable at `oldp`.
        unsafe {
            let have = *self.oldlenp;
            if have != want {
                ptr::copy_nonoverlapping(&v as *const T as *const u8, self.oldp as *mut u8, have.min(want));
                return Err(EINVAL);
            }
            ptr::write_unaligned(self.oldp as *mut T, v);
        }
        Ok(())
    }

    /// The new value, if the caller passed one; `EINVAL` on a size mismatch.
    fn write<T: Copy>(&self) -> Result<Option<T>, c_int> {
        if self.newp.is_null() {
            return Ok(None);
        }
        if self.newlen != size_of::<T>() {
            return Err(EINVAL);
        }
        // SAFETY: `newlen == size_of::<T>()` readable bytes at `newp`.
        Ok(Some(unsafe { ptr::read_unaligned(self.newp as *const T) }))
    }

    /// A C `bool`: any non-zero byte is true.
    fn write_bool(&self) -> Result<Option<bool>, c_int> {
        Ok(self.write::<u8>()?.map(|b| b != 0))
    }

    fn read_only(&self) -> Result<(), c_int> {
        if self.newp.is_null() && self.newlen == 0 { Ok(()) } else { Err(EPERM) }
    }

    fn write_only(&self) -> Result<(), c_int> {
        if self.oldp.is_null() && self.oldlenp.is_null() { Ok(()) } else { Err(EPERM) }
    }

    fn void(&self) -> Result<(), c_int> {
        self.read_only()?;
        self.write_only()
    }
}

// ---------------------------------------------------------------------------
// The keys.
// ---------------------------------------------------------------------------

/// `experimental.utilization.query`'s output, jemalloc's
/// `extent_util_stats_verbose_t`.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Utilization {
    pub slabcur_addr: *mut c_void,
    pub nfree: usize,
    pub nregs: usize,
    pub size: usize,
    pub bin_nfree: usize,
    pub bin_nregs: usize,
}

impl Utilization {
    const ZERO: Utilization =
        Utilization { slabcur_addr: ptr::null_mut(), nfree: 0, nregs: 0, size: 0, bin_nfree: 0, bin_nregs: 0 };
}

/// The utilisation figures of `p` (see the module docs). `verbose` adds
/// the bin-wide figures and `slabcur_addr`, which cost one class lock.
pub fn utilization(shared: &Shared, p: *const c_void, verbose: bool) -> Utilization {
    let mut u = Utilization::ZERO;
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let class = class as usize;
            let t = shared.table(class);
            let cls = t.class();
            if !t.is_allocated(slab as usize, slot as usize) {
                return u;
            }
            let Some(d) = t.desc_if_committed(slab as usize) else { return u };
            if class >= NBINS {
                // Reported like a large extent (`layout::BIN_MAX`): one
                // region of its own size, no bin figures.
                u.nregs = 1;
                u.size = cls.size();
                return u;
            }
            u.nfree = d.nfree(cls);
            u.nregs = cls.nslots();
            // The region bytes of the slab, not its power-of-two size: the
            // tail past the last slot belongs to no region, and Valkey's
            // allocatorShouldDefrag divides `size` by `nregs` and asserts
            // that the quotient is a bin size (jemalloc's slabs are exact
            // multiples of their region size).
            u.size = cls.nslots() * cls.size();
            if verbose {
                if let Some(a) = shared.arenas.get_existing(d.arena()) {
                    let cs = a.class(class);
                    let (st, curregs) = cs.bin_stats();
                    u.bin_nregs = st.curslabs() * cls.nslots();
                    u.bin_nfree = u.bin_nregs.saturating_sub(curregs);
                    if let Some(s) = cs.lowest_partial() {
                        u.slabcur_addr = shared.regions.slab_addr(class, s) as *mut c_void;
                    }
                }
            }
        }
        Kind::Huge { page } => {
            if let Some(r) = shared.huge.lookup(page) {
                if r.start == page && r.state == RunState::Active {
                    u.nregs = 1;
                    u.size = r.usable;
                }
            }
        }
        Kind::Invalid => {}
    }
    u
}

/// The arena that owns the live allocation `p` (`arenas.lookup`), without
/// aborting on anything else.
pub fn owner_of(shared: &Shared, p: *const c_void) -> Option<u16> {
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let t = shared.table(class as usize);
            if t.is_allocated(slab as usize, slot as usize) { Some(t.owner(slab as usize)) } else { None }
        }
        Kind::Huge { page } => {
            let r = shared.huge.lookup(page)?;
            if r.start == page && r.state == RunState::Active { Some(r.arena) } else { None }
        }
        Kind::Invalid => None,
    }
}

/// `experimental.utilization.batch_query`: `newp` is an array of pointers,
/// `oldp` an array of `(nfree, nregs, size)` triples of the same count.
fn batch_query(shared: &Shared, io: &Io) -> Result<(), c_int> {
    if io.oldp.is_null() || io.oldlenp.is_null() || io.newp.is_null() {
        return Err(EINVAL);
    }
    let psz = size_of::<*const c_void>();
    let tsz = 3 * size_of::<usize>();
    // SAFETY: `oldlenp` is the caller's length pointer.
    let oldlen = unsafe { *io.oldlenp };
    let n = io.newlen / psz;
    // `checked_mul`: a count whose triples overflow must not wrap onto
    // the caller's `oldlen` and send the loop past both arrays.
    if n == 0 || io.newlen % psz != 0 || n.checked_mul(tsz) != Some(oldlen) {
        return Err(EINVAL);
    }
    let ptrs = io.newp as *const *const c_void;
    let out = io.oldp as *mut usize;
    for i in 0..n {
        // SAFETY: `n` pointers are readable at `newp` and `3 * n` words
        // writable at `oldp`, both checked against the caller's lengths.
        unsafe {
            let u = utilization(shared, ptrs.add(i).read_unaligned(), false);
            out.add(3 * i).write_unaligned(u.nfree);
            out.add(3 * i + 1).write_unaligned(u.nregs);
            out.add(3 * i + 2).write_unaligned(u.size);
        }
    }
    Ok(())
}

fn cstr_of(s: &'static str) -> *const c_char {
    let c: &[u8] = match s {
        "never" => b"never\0",
        "auto" => b"auto\0",
        "always" => b"always\0",
        // `opt.percpu_arena` (config::PercpuArena::name).
        "disabled" => b"disabled\0",
        "percpu" => b"percpu\0",
        "phycpu" => b"phycpu\0",
        "off" => b"off\0",
        // `opt.slice_ext` (config::SliceExt::name); `auto` and `off` share the arms above.
        "on" => b"on\0",
        _ => b"?\0",
    };
    c.as_ptr() as *const c_char
}

fn arena_index(i: usize) -> Result<u16, c_int> {
    if i < MAX_ARENAS { Ok(i as u16) } else { Err(ENOENT) }
}

/// `mib[i]`, or an index that matches nothing when the MIB is shorter.
fn at(mib: &[usize], i: usize) -> usize {
    mib.get(i).copied().unwrap_or(usize::MAX)
}

/// `opt.<key>`: the key's value with its jemalloc type, the
/// `config::Value` variant's.
fn opt_value(mib: &[usize]) -> Result<Reading, c_int> {
    let key = KEYS.get(at(mib, 1)).ok_or(ENOENT)?;
    Ok(match shared().config.get(key.as_bytes()).ok_or(ENOENT)? {
        Value::Bool(b) => Bool(b),
        Value::Int(i) => Ssize(i as isize),
        Value::Uint(u) => Unsigned(u as c_uint),
        Value::Size(n) => Size(n),
        Value::Str(s) => Str(cstr_of(s)),
    })
}

/// `arenas.bin.<j>.*`: the bin's class; `ENOENT` past `arenas.nbins`.
fn bin_class(mib: &[usize]) -> Result<crate::layout::Class, c_int> {
    CLASSES[..NBINS].get(at(mib, 2)).copied().ok_or(ENOENT)
}

/// `stats.arenas.<i>.bins.<j>.*`: the stored bin figures.
fn bin_stats(mib: &[usize]) -> Result<BinStats, c_int> {
    arena_stats(shared(), at(mib, 2))?.bins[..NBINS].get(at(mib, 4)).copied().ok_or(ENOENT)
}

/// The tracked cgroup, or `ENOENT`: like jemalloc's keys behind a disabled
/// build option, the name resolves and the read says the facility is off.
fn tracked() -> Result<&'static crate::sys::cgroup::Cgroup, c_int> {
    let s = shared();
    if s.cgroup.tracked() { Ok(&s.cgroup) } else { Err(ENOENT) }
}

/// `arena.<i>.purge` and `arena.<i>.decay` address every arena when `i` is
/// `MALLCTL_ARENAS_ALL` or, jemalloc's pre-5.0 convention that it still
/// honours, `arenas.narenas`. Redis and Valkey (`jemalloc_purge`, behind
/// `MEMORY PURGE` and `FLUSHALL`) use the latter.
fn all_arenas(shared: &Shared, i: usize) -> bool {
    i == ARENAS_ALL as usize || i == shared.arenas.count() as usize
}

/// `arena.<i>.dirty_decay_ms` value of a period in nanoseconds.
fn ms_of(ns: u64) -> isize {
    if ns == purge::NEVER { -1 } else { (ns / 1_000_000) as isize }
}

/// The stored statistics of arena `i`, the `MALLCTL_ARENAS_ALL` merge, or
/// the zeros of `MALLCTL_ARENAS_DESTROYED` (both recognised before the
/// range check, which they fail).
fn arena_stats(shared: &Shared, i: usize) -> Result<ArenaStats, c_int> {
    if i == ARENAS_ALL as usize {
        return Ok(stats::arena_stats_all(shared));
    }
    if i == ARENAS_DESTROYED {
        return Ok(ArenaStats::empty(ARENAS_DESTROYED as u16));
    }
    stats::arena_stats(shared, arena_index(i)?).ok_or(ENOENT)
}

/// Return the calling thread's cached slots before a purge so the purge
/// can reach the slabs they pin (`malloc_trim` does the same).
fn flush_caller(shared: &Shared) {
    if let Some(h) = heap::existing_heap(shared) {
        h.flush_all(shared);
    }
}

/// The calling thread's heap; `EAGAIN` while the thread is creating or
/// has torn down its heap.
fn thread_heap<'a>(shared: &'a Shared) -> Result<&'a mut Heap, c_int> {
    heap::heap_for_thread(shared).ok_or(EAGAIN)
}

/// Copy a reading out with its type's size.
fn read_out(io: &Io, r: Reading) -> Result<(), c_int> {
    match r {
        Bool(v) => io.read(v),
        Unsigned(v) => io.read(v),
        U32(v) => io.read(v),
        Size(v) => io.read(v),
        Ssize(v) => io.read(v),
        U64(v) => io.read(v),
        Str(v) => io.read(v),
        U64Ptr(v) => io.read(v),
    }
}

/// Perform the key `node` names with the numeric components of `mib`:
/// a read-only key through its reader, the others as arms. `ENOENT` for
/// an interior node.
fn ctl(shared: &Shared, node: &Node, mib: &[usize], io: &Io) -> Result<(), c_int> {
    let leaf = node.leaf.ok_or(ENOENT)?;
    if let Some(read) = node.read {
        io.read_only()?;
        return read_out(io, read(mib)?);
    }
    match leaf {
        Epoch => {
            if io.write::<u64>()?.is_some() {
                stats::epoch(shared);
            }
            io.read(stats::snapshot(shared).epoch)
        }
        BackgroundThread => {
            if let Some(on) = io.write_bool()? {
                purge::background::request(on);
                if on && !purge::background::present() && !purge::background::start(shared) {
                    return Err(EAGAIN);
                }
            }
            io.read(purge::background::present())
        }
        MaxBackgroundThreads => {
            if let Some(n) = io.write::<usize>()? {
                if n == 0 {
                    return Err(EINVAL);
                }
            }
            io.read(1usize)
        }
        ArenasCreate => {
            // `newp` may carry extent hooks, which challoc has no use for.
            let i = shared.arenas.create(shared).ok_or(EAGAIN)?;
            io.read(i as c_uint)
        }
        ArenasLookup => {
            let p = io.write::<*const c_void>()?.ok_or(EINVAL)?;
            let a = owner_of(shared, p).ok_or(EFAULT)?;
            io.read(a as c_uint)
        }
        ArenaPurge => {
            io.void()?;
            flush_caller(shared);
            let i = at(mib, 1);
            if all_arenas(shared, i) {
                purge::purge_all(shared);
            } else {
                purge::purge_arena(shared, arena_index(i)?).ok_or(ENOENT)?;
            }
            Ok(())
        }
        ArenaDecay => {
            io.void()?;
            let now = sys::now_coarse_ns();
            let i = at(mib, 1);
            if all_arenas(shared, i) {
                purge::decay(shared, now);
            } else {
                let arena = shared.arenas.get_existing(arena_index(i)?).ok_or(ENOENT)?;
                arena.decay(now);
                if i == 0 {
                    let d = shared.decay_ns();
                    if d != purge::NEVER {
                        shared.huge.decay(now, d);
                    }
                }
            }
            Ok(())
        }
        ArenaDestroy => {
            io.void()?;
            let i = at(mib, 1);
            if i == 0 || i == ARENAS_ALL as usize {
                return Err(EFAULT);
            }
            flush_caller(shared);
            purge::purge_arena(shared, arena_index(i)?).ok_or(ENOENT)?;
            Ok(())
        }
        ArenaDirtyDecayMs => {
            let i = at(mib, 1);
            let new = io.write::<isize>()?;
            if let Some(ms) = new {
                if ms < -1 {
                    return Err(EFAULT);
                }
            }
            if i == ARENAS_ALL as usize {
                if let Some(ms) = new {
                    shared.set_decay_ms(ms as i64);
                }
                return io.read(ms_of(shared.decay_ns()));
            }
            let arena = shared.arena(arena_index(i)?).ok_or(ENOENT)?;
            if let Some(ms) = new {
                arena.set_decay_ms(ms as i64);
            }
            io.read(ms_of(arena.decay_ns()))
        }
        ThreadTcacheFlush => {
            io.void()?;
            flush_caller(shared);
            Ok(())
        }
        ThreadTcacheEnabled => {
            let h = thread_heap(shared)?;
            if let Some(on) = io.write_bool()? {
                h.set_tcache_enabled(shared, on);
            }
            io.read(h.tcache_enabled())
        }
        ThreadArena => {
            let h = thread_heap(shared)?;
            if let Some(a) = io.write::<c_uint>()? {
                if a as usize >= MAX_ARENAS || shared.arena(a as u16).is_none() {
                    return Err(EFAULT);
                }
                h.set_arena(shared, a as u16);
            }
            io.read(h.arena() as c_uint)
        }
        TcacheCreate => {
            io.read_only()?;
            let home = heap::existing_heap(shared).map_or(0, |h| h.arena());
            let t = shared.tcaches.create(shared, home).ok_or(EFAULT)?;
            io.read(t as c_uint)
        }
        TcacheFlush | TcacheDestroy => {
            io.write_only()?;
            let t = io.write::<c_uint>()?.ok_or(EINVAL)?;
            if t > u16::MAX as c_uint {
                return Err(EFAULT);
            }
            let ok = match leaf {
                TcacheFlush => shared.tcaches.flush(shared, t as u16),
                _ => shared.tcaches.destroy(shared, t as u16),
            };
            if ok { Ok(()) } else { Err(EFAULT) }
        }
        UtilQuery => {
            let p = io.write::<*const c_void>()?.ok_or(EINVAL)?;
            io.read(utilization(shared, p, true))
        }
        UtilBatchQuery => batch_query(shared, io),
        // Every other leaf is a read-only key with a reader.
        _ => Err(ENOENT),
    }
}

fn result(r: Result<(), c_int>) -> c_int {
    match r {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// `mallctl(name, oldp, oldlenp, newp, newlen)`.
pub fn mallctl(name: *const c_char, oldp: *mut c_void, oldlenp: *mut usize, newp: *const c_void, newlen: usize) -> c_int {
    let Some(name) = cstr(name) else { return EINVAL };
    let mut mib = [0usize; MAX_MIB];
    let (len, node) = match name_to_mib(name, &mut mib) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let shared = super::shared();
    result(ctl(shared, node, &mib[..len], &Io { oldp, oldlenp, newp, newlen }))
}

/// `mallctlnametomib(name, mibp, miblenp)`: `*miblenp` is the capacity in
/// and the length out.
pub fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> c_int {
    let Some(name) = cstr(name) else { return EINVAL };
    if mibp.is_null() || miblenp.is_null() {
        return EINVAL;
    }
    let mut mib = [0usize; MAX_MIB];
    let (len, _) = match name_to_mib(name, &mut mib) {
        Ok(x) => x,
        Err(e) => return e,
    };
    // SAFETY: the caller's capacity pointer and an array of that many
    // `size_t`.
    unsafe {
        if *miblenp < len {
            return EINVAL;
        }
        ptr::copy_nonoverlapping(mib.as_ptr(), mibp, len);
        *miblenp = len;
    }
    0
}

/// `mallctlbymib(mib, miblen, oldp, oldlenp, newp, newlen)`.
pub fn mallctlbymib(
    mib: *const usize,
    miblen: usize,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    newp: *const c_void,
    newlen: usize,
) -> c_int {
    if mib.is_null() || miblen == 0 || miblen > MAX_MIB {
        return EINVAL;
    }
    let mut local = [0usize; MAX_MIB];
    // SAFETY: `miblen` entries are readable at `mib`.
    unsafe { ptr::copy_nonoverlapping(mib, local.as_mut_ptr(), miblen) };
    let node = match mib_to_node(&local[..miblen]) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let shared = super::shared();
    result(ctl(shared, node, &local[..miblen], &Io { oldp, oldlenp, newp, newlen }))
}

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use core::ffi::{c_char, c_int, c_void};

    export_pair! {
        fn mallctl / je_mallctl(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize
        ) -> c_int {
            super::mallctl(name, oldp, oldlenp, newp, newlen)
        }
        fn mallctlnametomib / je_mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> c_int {
            super::mallctlnametomib(name, mibp, miblenp)
        }
        fn mallctlbymib / je_mallctlbymib(
            mib: *const usize,
            miblen: usize,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize
        ) -> c_int {
            super::mallctlbymib(mib, miblen, oldp, oldlenp, newp, newlen)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::jemalloc::{dallocx, mallocx};
    use crate::api::libc::{free, malloc};
    use crate::arena::{MALLOCX_TCACHE_NONE, mallocx_arena, mallocx_tcache};
    use crate::config::Config;
    use std::ffi::CString;
    use std::vec::Vec;

    /// `mallctl` by name with typed in/out values.
    fn ctl_rw<T: Copy>(name: &str, old: Option<&mut T>, new: Option<&T>) -> c_int {
        let cname = CString::new(name).unwrap();
        let mut oldlen = size_of::<T>();
        let (oldp, oldlenp) = match old {
            Some(o) => (o as *mut T as *mut c_void, &mut oldlen as *mut usize),
            None => (ptr::null_mut(), ptr::null_mut()),
        };
        let (newp, newlen) = match new {
            Some(n) => (n as *const T as *const c_void, size_of::<T>()),
            None => (ptr::null(), 0),
        };
        mallctl(cname.as_ptr(), oldp, oldlenp, newp, newlen)
    }

    fn get<T: Copy + Default>(name: &str) -> T {
        let mut v = T::default();
        let rc = ctl_rw(name, Some(&mut v), None);
        assert_eq!(rc, 0, "mallctl({name}) failed with {rc}");
        v
    }

    fn set<T: Copy>(name: &str, v: T) -> c_int {
        ctl_rw::<T>(name, None, Some(&v))
    }

    fn call(name: &str) -> c_int {
        let cname = CString::new(name).unwrap();
        mallctl(cname.as_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null(), 0)
    }

    fn mib_of(name: &str) -> Vec<usize> {
        let cname = CString::new(name).unwrap();
        let mut mib = [0usize; MAX_MIB];
        let mut len = MAX_MIB;
        let rc = mallctlnametomib(cname.as_ptr(), mib.as_mut_ptr(), &mut len);
        assert_eq!(rc, 0, "mallctlnametomib({name}) = {rc}");
        mib[..len].to_vec()
    }

    /// A pointer count whose triples overflow `usize` must not pass the
    /// length check: `n * 24` wrapping to the caller's 8-byte `oldlen`
    /// would have the loop read `n` pointers and write `3 * n` words.
    #[test]
    fn batch_query_rejects_a_count_whose_triples_overflow() {
        let mib = mib_of("experimental.utilization.batch_query");
        let p = malloc(16);
        let ptrs = [p as *const c_void];
        let mut out = [0usize; 1];
        let mut outlen = size_of::<usize>();
        let n = usize::MAX / (3 * size_of::<usize>()) + 1;
        assert_eq!(n.wrapping_mul(3 * size_of::<usize>()), outlen, "the product wraps to the out length");
        let newlen = n * size_of::<*const c_void>();
        let rc = mallctlbymib(mib.as_ptr(), mib.len(), out.as_mut_ptr() as *mut c_void, &mut outlen, ptrs.as_ptr() as *const c_void, newlen);
        assert_eq!(rc, EINVAL);
        free(p);
    }

    fn get_mib<T: Copy + Default>(mib: &[usize]) -> T {
        let mut v = T::default();
        let mut len = size_of::<T>();
        let rc = mallctlbymib(mib.as_ptr(), mib.len(), &mut v as *mut T as *mut c_void, &mut len, ptr::null(), 0);
        assert_eq!(rc, 0, "mallctlbymib({mib:?}) = {rc}");
        v
    }

    fn epoch() -> u64 {
        let mut e: u64 = 0;
        let one: u64 = 1;
        assert_eq!(ctl_rw("epoch", Some(&mut e), Some(&one)), 0);
        e
    }

    fn cstring_at(p: *const c_char) -> std::string::String {
        unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap().into()
    }

    #[test]
    fn names_resolve_to_positional_mibs_and_back() {
        assert_eq!(mib_of("version"), [0]);
        assert_eq!(mib_of("epoch"), [1]);
        // Numeric components are stored as the number, at the position
        // Valkey overwrites.
        assert_eq!(mib_of("arenas.bin.7.size"), [5, 5, 7, 0]);
        assert_eq!(mib_of("arena.4096.purge"), [6, 4096, 0]);
        assert_eq!(mib_of("stats.arenas.3.bins.51.nonfull_slabs"), [7, 6, 3, 8, 51, 2]);
        assert_eq!(mib_of("stats.arenas.0.small.allocated"), [7, 6, 0, 3, 0]);
        assert_eq!(mib_of("experimental.utilization.batch_query"), [10, 0, 1]);
        for (i, key) in KEYS.iter().enumerate() {
            assert_eq!(mib_of(&std::format!("opt.{key}")), [4, i]);
        }
        // Interior nodes resolve (jemalloc allows it) but are not keys.
        assert_eq!(mib_of("arenas"), [5]);
        assert_eq!(mib_to_leaf(&[5]), Err(ENOENT));
        assert_eq!(call("arenas"), ENOENT);
        assert_eq!(mib_to_leaf(&[5, 5, 7, 0]), Ok(Leaf::ArenasBinSize));
        assert_eq!(mib_to_leaf(&[7, 6, 3, 8, 51, 2]), Ok(Leaf::StatsArenaBinNonfullSlabs));
        assert_eq!(mib_to_leaf(&[7, 6, 3, 4, 51, 2]), Err(ENOENT), "large has one child");
        assert_eq!(mib_to_leaf(&[99]), Err(ENOENT));
        assert_eq!(mib_to_leaf(&[5, 5, 7, 9]), Err(ENOENT));
        assert_eq!(mib_to_leaf(&[]), Err(ENOENT));
        // Unknown, malformed, and prof.* names.
        // `stats.cgroup.*` (design 11): the names resolve; a read is
        // `ENOENT` only where no cgroup2 is tracked.
        assert_eq!(mib_of("stats.cgroup.limit"), [7, 7, 0]);
        assert_eq!(mib_of("stats.cgroup.pressure_events"), [7, 7, 3]);
        assert_eq!(mib_to_leaf(&[7, 7, 2]), Ok(Leaf::StatsCgroupCurrent));
        // The policy's keys (design 11, `policy.rs`) follow, in table order.
        assert_eq!(mib_of("stats.cgroup.decay_ms"), [7, 7, 4]);
        assert_eq!(mib_of("stats.cgroup.full_purges"), [7, 7, 7]);
        assert_eq!(mib_to_leaf(&[7, 7, 5]), Ok(Leaf::StatsCgroupRetentionCap));
        assert_eq!(mib_to_leaf(&[7, 7, 6]), Ok(Leaf::StatsCgroupEmptyBytes));
        // The two-advice purge's keys (design 8) follow and read
        // with or without a tracked cgroup: eager by default (no limit on
        // the host, none in the default container), nothing lazy issued.
        assert_eq!(mib_of("stats.cgroup.purge_advice"), [7, 7, 8]);
        assert_eq!(mib_of("stats.cgroup.lazy_purges"), [7, 7, 9]);
        assert_eq!(mib_of("stats.cgroup.eager_purges"), [7, 7, 10]);
        assert_eq!(mib_to_leaf(&[7, 7, 11]), Err(ENOENT));
        let advice: usize = get("stats.cgroup.purge_advice");
        assert_eq!(advice, 0, "eager without a limit");
        let lazy: usize = get("stats.cgroup.lazy_purges");
        assert_eq!(lazy, 0);
        let _eager: usize = get("stats.cgroup.eager_purges");
        assert_eq!(set("stats.cgroup.purge_advice", 1usize), EPERM);
        let tracked = shared().cgroup.tracked();
        for key in [
            "stats.cgroup.limit",
            "stats.cgroup.high",
            "stats.cgroup.current",
            "stats.cgroup.pressure_events",
            "stats.cgroup.decay_ms",
            "stats.cgroup.retention_cap",
            "stats.cgroup.empty_bytes",
            "stats.cgroup.full_purges",
        ] {
            assert_eq!(call(key), if tracked { 0 } else { ENOENT }, "{key} tracked={tracked}");
            assert_eq!(set(key, 1usize), if tracked { EPERM } else { ENOENT }, "{key}");
        }
        if tracked {
            let limit: usize = get("stats.cgroup.limit");
            let high: usize = get("stats.cgroup.high");
            let current: usize = get("stats.cgroup.current");
            let events: usize = get("stats.cgroup.pressure_events");
            std::eprintln!("stats.cgroup: limit {limit} high {high} current {current} pressure_events {events}");
            assert_eq!(limit, shared().cgroup.limit() as usize);
            assert_eq!(high, shared().cgroup.high() as usize);
            assert!(limit == usize::MAX || limit > 0);
            assert_eq!(events, 0, "no background thread has armed a trigger in this process");
            // The policy's keys: the defaults without a limit (the period
            // as configured, no cap), the cap arithmetic with one.
            let decay_ms: usize = get("stats.cgroup.decay_ms");
            let cap: usize = get("stats.cgroup.retention_cap");
            let empty: usize = get("stats.cgroup.empty_bytes");
            let full: usize = get("stats.cgroup.full_purges");
            std::eprintln!("stats.cgroup: decay_ms {decay_ms} retention_cap {cap} empty_bytes {empty} full_purges {full}");
            let effective = limit.min(high);
            if effective == usize::MAX {
                assert_eq!(decay_ms, (shared().decay_ns() / 1_000_000) as usize);
                assert_eq!(cap, usize::MAX);
            } else {
                // The sampler may not have run in this process yet: the cap
                // is either unpublished or the limit's twentieth.
                assert!(cap == usize::MAX || cap == effective / 20, "cap {cap} for limit {effective}");
            }
            assert_eq!(full, 0);
        }
        // `stats.cgroup` itself is interior: it resolves, and is not a key.
        assert_eq!(mib_of("stats.cgroup"), [7, 7]);
        assert_eq!(call("stats.cgroup"), ENOENT);
        for bad in ["nope", "arenas.bin.x.size", "arenas.bin..size", "prof.active", "prof.dump", "stats.cgroup.nope", "version.more", ""] {
            let c = CString::new(bad).unwrap();
            let mut mib = [0usize; MAX_MIB];
            let mut len = MAX_MIB;
            assert_eq!(mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len), ENOENT, "{bad}");
            assert_eq!(call(bad), ENOENT, "{bad}");
        }
        assert_eq!(mallctl(ptr::null(), ptr::null_mut(), ptr::null_mut(), ptr::null(), 0), EINVAL);
        // Too small a MIB buffer.
        let c = CString::new("arenas.bin.7.size").unwrap();
        let mut mib = [0usize; 2];
        let mut len = 2;
        assert_eq!(mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len), EINVAL);
        assert_eq!(mallctlbymib(ptr::null(), 1, ptr::null_mut(), ptr::null_mut(), ptr::null(), 0), EINVAL);
        assert_eq!(mallctlbymib(mib.as_ptr(), 0, ptr::null_mut(), ptr::null_mut(), ptr::null(), 0), EINVAL);
    }

    #[test]
    fn read_write_rules_match_jemalloc() {
        // Reading with the wrong size copies the prefix and fails EINVAL.
        let mut short: u32 = 0;
        let c = CString::new("arenas.page").unwrap();
        let mut len = 4usize;
        let rc = mallctl(c.as_ptr(), &mut short as *mut u32 as *mut c_void, &mut len, ptr::null(), 0);
        assert_eq!(rc, EINVAL);
        assert_eq!(short as usize, shared().regions.page_size(), "the low half was still copied");
        // Writing a read-only key is EPERM; a wrong newlen on a writable key
        // is EINVAL; a write-only key refuses a read.
        assert_eq!(set("version", 1u64), EPERM);
        assert_eq!(set("stats.allocated", 1usize), EPERM);
        assert_eq!(set("arenas.nbins", 1u32), EPERM);
        assert_eq!(set("epoch", 1u32), EINVAL);
        assert_eq!(set("arena.0.dirty_decay_ms", 5i32), EINVAL);
        let mut out: c_uint = 0;
        assert_eq!(ctl_rw("tcache.flush", Some(&mut out), None), EPERM);
        assert_eq!(set("tcache.flush", 4000 as c_uint), EFAULT);
        assert_eq!(call("tcache.flush"), EINVAL, "write-only key needs a value");
        // Void keys take neither.
        assert_eq!(set("arena.0.purge", 1u32), EPERM);
        let mut v: u32 = 0;
        assert_eq!(ctl_rw("thread.tcache.flush", Some(&mut v), None), EPERM);
        // Null out-pointers mean "do not read".
        assert_eq!(call("version"), 0);
        assert_eq!(call("epoch"), 0);
    }

    #[test]
    fn static_and_opt_keys_report_the_build_and_configuration() {
        let v: *const c_char = get("version");
        assert_eq!(cstring_at(v), VERSION_STR);
        assert!(cstring_at(v).starts_with("5.3.0-0-g"));
        let s: *const c_char = get("config.malloc_conf");
        assert_eq!(cstring_at(s), "");
        assert!(!get::<bool>("config.debug"));
        assert!(get::<bool>("config.stats"));
        assert_eq!(get::<c_uint>("arenas.nbins") as usize, NBINS);
        assert_eq!(get::<usize>(&std::format!("arenas.bin.{}.size", NBINS - 1)), crate::layout::BIN_MAX);
        assert_eq!(get::<c_uint>("arenas.nlextents"), 0);
        assert_eq!(get::<usize>("arenas.quantum"), QUANTUM);
        assert_eq!(get::<usize>("arenas.page"), shared().regions.page_size());
        assert!(get::<c_uint>("arenas.narenas") >= 1);
        assert_eq!(get::<usize>("max_background_threads"), 1);
        assert_eq!(set("max_background_threads", 0usize), EINVAL);
        assert_eq!(set("max_background_threads", 4usize), 0);
        for j in 0..NBINS {
            assert_eq!(get::<usize>(&std::format!("arenas.bin.{j}.size")), CLASSES[j].size());
            assert_eq!(get::<u32>(&std::format!("arenas.bin.{j}.nregs")) as usize, CLASSES[j].nslots());
            assert_eq!(get::<usize>(&std::format!("arenas.bin.{j}.slab_size")), CLASSES[j].slab_size());
        }
        assert_eq!(call(&std::format!("arenas.bin.{NBINS}.size")), ENOENT);
        // Every opt key with its jemalloc type, against the live Config:
        // `ssize_t` for the signed keys, `size_t` for the byte counts
        // (`Value::Size`), `unsigned` for the other counts.
        let cfg: &Config = &shared().config;
        for key in KEYS {
            let name = std::format!("opt.{key}");
            match cfg.get(key.as_bytes()).unwrap() {
                Value::Bool(b) => assert_eq!(get::<bool>(&name), b, "{name}"),
                Value::Int(i) => assert_eq!(get::<isize>(&name), i as isize, "{name}"),
                Value::Uint(u) => assert_eq!(get::<c_uint>(&name), u as c_uint, "{name}"),
                Value::Size(n) => assert_eq!(get::<usize>(&name), n, "{name}"),
                Value::Str(s) => assert_eq!(cstring_at(get::<*const c_char>(&name)), s, "{name}"),
            }
            assert_eq!(set(&name, 0u8), EPERM, "{name} must be read-only");
        }
        for key in ["tcache_max", "huge_retain_max"] {
            assert!(matches!(cfg.get(key.as_bytes()), Some(Value::Size(_))), "opt.{key} is a byte count of the table");
        }
        assert_eq!(call("opt.junk"), ENOENT, "accepted-and-ignored conf keys are not readable");
        assert_eq!(call("opt.stats_print"), ENOENT, "the inert key without a field is not readable");
        // The two byte-count options are `size_t`, as jemalloc's
        // `opt.tcache_max` is: an `unsigned` buffer is the wrong size and
        // gets the low half plus `EINVAL`, as for any size_t key.
        assert_eq!(get::<usize>("opt.tcache_max"), cfg.tcache_max);
        let mut half: c_uint = 0;
        assert_eq!(ctl_rw("opt.tcache_max", Some(&mut half), None), EINVAL);
        assert_eq!(half as usize, cfg.tcache_max & u32::MAX as usize, "the low half was still copied");
        assert_eq!(get::<c_uint>("opt.narenas"), cfg.narenas as c_uint, "a count stays unsigned");
        assert_eq!(get::<isize>("opt.dirty_decay_ms"), cfg.dirty_decay_ms as isize, "a period stays ssize_t");
        assert_eq!(get::<isize>("opt.huge_dirty_decay_ms"), cfg.huge_decay_ms() as isize, "unset: the slab period");
        // `opt.huge_retain_max` reports the value the huge allocator uses.
        assert_eq!(get::<usize>("opt.huge_retain_max"), shared().huge.retain_max());
        assert_eq!(get::<usize>("opt.huge_retain_max"), crate::huge::DEFAULT_RETAIN_MAX);
        assert_eq!(get::<isize>("arena.0.dirty_decay_ms"), cfg.dirty_decay_ms as isize);
        // A background thread is neither requested by default nor running.
        let _ = get::<bool>("background_thread");
        assert_eq!(set("background_thread", false), 0);
    }

    /// `cstr_of` mirrors the `name()` of every string-valued key's enum by
    /// hand, so a variant it does not know reads back as `"?"` through
    /// `opt.<key>`. Every variant of every such enum goes through
    /// `Config::get` (the `opt.<key>` read) and `cstr_of` here; the
    /// exhaustive `match` on each enum makes a new variant a compile error
    /// in this test until it is added to the array (and to `cstr_of`).
    #[test]
    fn every_string_opt_variant_reads_back_through_cstr_of() {
        use crate::config::{PercpuArena, SliceExt, Thp};
        fn check(cfg: &Config, key: &str, want: &str) {
            match cfg.get(key.as_bytes()) {
                Some(Value::Str(s)) => {
                    assert_eq!(s, want, "opt.{key}");
                    assert_eq!(cstring_at(cstr_of(s)), want, "opt.{key}: cstr_of does not know `{want}`");
                }
                other => panic!("opt.{key} is not a string: {other:?}"),
            }
        }
        let mut cfg = Config::DEFAULT;
        for v in [Thp::Never, Thp::Auto, Thp::Always] {
            match v {
                Thp::Never | Thp::Auto | Thp::Always => {}
            }
            cfg.thp = v;
            check(&cfg, "thp", v.name());
        }
        for v in [PercpuArena::Disabled, PercpuArena::Percpu, PercpuArena::Phycpu] {
            match v {
                PercpuArena::Disabled | PercpuArena::Percpu | PercpuArena::Phycpu => {}
            }
            cfg.percpu_arena = v;
            check(&cfg, "percpu_arena", v.name());
        }
        for v in [SliceExt::Auto, SliceExt::On, SliceExt::Off] {
            match v {
                SliceExt::Auto | SliceExt::On | SliceExt::Off => {}
            }
            cfg.slice_ext = v;
            check(&cfg, "slice_ext", v.name());
        }
        // Every string-valued key of the table is one of the three above.
        for key in KEYS {
            if let Some(Value::Str(_)) = cfg.get(key.as_bytes()) {
                assert!(
                    matches!(*key, "thp" | "percpu_arena" | "slice_ext"),
                    "opt.{key} is a string key this test does not cover"
                );
            }
        }
        assert_eq!(cstring_at(cstr_of("not-a-name")), "?");
    }

    #[test]
    fn stats_are_frozen_between_epochs_and_arenas_all_sums() {
        let s = shared();
        // Other tests of this binary allocate and free concurrently, and
        // since `tcache_max` a freed 40 KiB block leaves `allocated` at
        // once instead of lingering in a cache, so the growth check retries
        // until a snapshot pair is not swamped by a neighbour's frees.
        let mut live: Vec<*mut u8> = Vec::new();
        let mut a1 = 0usize;
        for attempt in 0..20 {
            let e0 = epoch();
            let a0: usize = get("stats.allocated");
            live = (0..300).map(|_| malloc(200)).collect();
            assert_eq!(get::<usize>("stats.allocated"), a0, "no epoch, no change");
            let e1 = epoch();
            assert_eq!(e1, e0 + 1);
            assert_eq!(get::<u64>("epoch"), e1);
            a1 = get("stats.allocated");
            if a1 >= a0 + 300 * 200 {
                break;
            }
            assert!(attempt < 19, "allocated {a0} -> {a1} on every attempt");
            for p in live.drain(..) {
                free(p);
            }
        }
        let active: usize = get("stats.active");
        let resident: usize = get("stats.resident");
        let mapped: usize = get("stats.mapped");
        let retained: usize = get("stats.retained");
        let metadata: usize = get("stats.metadata");
        assert!(resident >= active && active >= a1);
        assert_eq!(mapped, resident + retained);
        assert!(metadata > 0);
        // Per-arena figures sum to the totals through MALLCTL_ARENAS_ALL,
        // read by name and through a MIB with the index patched in.
        let mib = mib_of("stats.arenas.0.resident");
        let mut sum = 0usize;
        for i in 0..s.arenas.count() as usize {
            if s.arenas.get_existing(i as u16).is_none() {
                continue;
            }
            let mut m = mib.clone();
            m[2] = i;
            sum += get_mib::<usize>(&m);
        }
        assert_eq!(sum, resident);
        assert_eq!(get::<usize>("stats.arenas.4096.resident"), resident);
        assert_eq!(get::<usize>("stats.arenas.4096.mapped"), mapped);
        assert_eq!(get::<usize>("stats.arenas.4096.pactive") * s.regions.page_size(), active);
        assert_eq!(get::<usize>("stats.arenas.4096.pmuzzy"), 0);
        let small: usize = get("stats.arenas.4096.small.allocated");
        let large: usize = get("stats.arenas.4096.large.allocated");
        assert_eq!(small + large, a1);
        assert!(get::<usize>("stats.arenas.4096.small.resident") >= small);
        assert_eq!(call("stats.arenas.4000.pactive"), ENOENT);
        assert_eq!(call("stats.arenas.5000.pactive"), ENOENT);
        // Bin figures: the class of the 300 live blocks has them.
        let c = crate::layout::class_of(200).unwrap();
        let bin_mib = mib_of("stats.arenas.0.bins.0.curregs");
        let mut m = bin_mib.clone();
        m[2] = ARENAS_ALL as usize;
        m[4] = c;
        assert!(get_mib::<usize>(&m) >= 300);
        let curslabs: usize = get(&std::format!("stats.arenas.4096.bins.{c}.curslabs"));
        assert!(curslabs >= 1);
        assert!(get::<usize>(&std::format!("stats.arenas.4096.bins.{c}.nonfull_slabs")) <= curslabs);
        assert_eq!(call(&std::format!("stats.arenas.0.bins.{NBINS}.curregs")), ENOENT);
        for p in live {
            free(p);
        }
    }

    /// `MALLCTL_ARENAS_DESTROYED` (4097, `ARENAS_DESTROYED`): the
    /// statistics of the arenas destroyed so far are all zero, by name and
    /// through a MIB with the index patched in as Valkey does, bins
    /// included; the index is still no arena for `arena.4097.*`, and 4098
    /// is nothing at all.
    #[test]
    fn destroyed_arenas_index_reads_zeros_and_is_no_arena() {
        for key in [
            "pactive",
            "pdirty",
            "pmuzzy",
            "small.allocated",
            "small.resident",
            "large.allocated",
            "resident",
            "mapped",
            "retained",
            "bins.0.curregs",
            "bins.0.curslabs",
            "bins.0.nonfull_slabs",
        ] {
            let name = std::format!("stats.arenas.4097.{key}");
            assert_eq!(get::<usize>(&name), 0, "{name}");
            assert_eq!(set(&name, 1usize), EPERM, "{name} is read-only");
            assert_eq!(call(&std::format!("stats.arenas.4098.{key}")), ENOENT, "{key}: 4098 is nothing");
        }
        let c = crate::layout::class_of(200).unwrap();
        let mut m = mib_of("stats.arenas.0.bins.0.curregs");
        m[2] = ARENAS_DESTROYED;
        m[4] = c;
        assert_eq!(get_mib::<usize>(&m), 0);
        assert_eq!(call(&std::format!("stats.arenas.4097.bins.{NBINS}.curregs")), ENOENT, "past arenas.nbins");
        for key in ["purge", "decay", "destroy", "dirty_decay_ms"] {
            assert_eq!(call(&std::format!("arena.4097.{key}")), ENOENT, "arena.4097.{key}");
        }
        assert_eq!(set("arena.4097.dirty_decay_ms", 5isize), ENOENT);
        assert_eq!(set("thread.arena", ARENAS_DESTROYED as c_uint), EFAULT);
        assert_eq!(ARENAS_DESTROYED, ARENAS_ALL as usize + 1, "the headers' value");
    }

    #[test]
    fn arena_keys_create_lookup_purge_decay_and_destroy() {
        let s = shared();
        let a: c_uint = get("arenas.create");
        assert!(a >= 1);
        assert!(s.arenas.get_existing(a as u16).is_some());
        assert!(get::<c_uint>("arenas.narenas") > a);
        let p = mallocx(500, mallocx_arena(a as u16) as c_int);
        assert!(!p.is_null());
        let h = mallocx(2 << 20, mallocx_arena(a as u16) as c_int);
        // arenas.lookup finds both tiers; garbage is EFAULT, not an abort.
        for q in [p, h] {
            let mut out: c_uint = 0;
            let cname = CString::new("arenas.lookup").unwrap();
            let mut len = size_of::<c_uint>();
            let qp: *const c_void = q as *const c_void;
            let rc = mallctl(cname.as_ptr(), &mut out as *mut c_uint as *mut c_void, &mut len, &qp as *const *const c_void as *const c_void, size_of::<*const c_void>());
            assert_eq!(rc, 0);
            assert_eq!(out, a);
        }
        let stack = 0u64;
        let sp: *const c_void = &stack as *const u64 as *const c_void;
        let mut out: c_uint = 0;
        assert_eq!(ctl_rw::<c_uint>("arenas.lookup", Some(&mut out), None), EINVAL, "lookup needs a pointer");
        let cname = CString::new("arenas.lookup").unwrap();
        let mut len = size_of::<c_uint>();
        assert_eq!(mallctl(cname.as_ptr(), &mut out as *mut c_uint as *mut c_void, &mut len, &sp as *const *const c_void as *const c_void, size_of::<*const c_void>()), EFAULT);
        // Decay period per arena, and for all.
        assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), 250isize), 0);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 250);
        assert_eq!(s.arena(a as u16).unwrap().decay_ns(), 250_000_000);
        assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), -1isize), 0);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), -1);
        assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), -2isize), EFAULT);
        let default = get::<isize>("arena.0.dirty_decay_ms");
        assert_eq!(set("arena.4096.dirty_decay_ms", 777isize), 0);
        assert_eq!(get::<isize>("arena.0.dirty_decay_ms"), 777);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 777);
        assert_eq!(set("arena.4096.dirty_decay_ms", default), 0);
        assert_eq!(get::<isize>("arena.4096.dirty_decay_ms"), default);
        // Free everything in the arena, then purge: its Empty slab goes.
        dallocx(p, 0);
        dallocx(h, 0);
        let arena = s.arena(a as u16).unwrap();
        let c = crate::layout::class_of(500).unwrap();
        // The slot sits in this thread's cache for arena `a` until the
        // purge flushes the caller.
        assert_eq!(arena.classes[c].stats().empty, 0);
        assert_eq!(call(&std::format!("arena.{a}.purge")), 0);
        assert_eq!(arena.classes[c].stats().empty, 0);
        // The first refill spanned `cap / 2` slots' worth of slabs (one
        // 128-slot slab at 512 bytes); all of them were flushed and purged.
        let refill_slabs = (crate::heap::default_cap(c) as usize / 2).div_ceil(crate::layout::CLASSES[c].nslots());
        assert_eq!(arena.classes[c].stats().purged, refill_slabs, "the flushed slabs were purged");
        assert_eq!(call("arena.4096.purge"), 0);
        // jemalloc's legacy "all arenas" index, which Valkey's MEMORY PURGE uses.
        let narenas: c_uint = get("arenas.narenas");
        assert_eq!(call(&std::format!("arena.{narenas}.purge")), 0);
        assert_eq!(call(&std::format!("arena.{narenas}.decay")), 0);
        // The first unused index is ENOENT at this instant. Another api
        // test's `arenas.create` can land between the count read and the
        // call (the api tests share one process; on the 7.2 Chainguard OS
        // host `arena.{narenas+1}.purge` once answered 0, the moved count's
        // new all-arenas alias), so the pair is retried until the count did
        // not move across it; the far range below no concurrent test reaches.
        let mut stable = false;
        for _ in 0..8 {
            let n: c_uint = get("arenas.narenas");
            let rc = call(&std::format!("arena.{}.purge", n + 1));
            if get::<c_uint>("arenas.narenas") == n {
                assert_eq!(rc, ENOENT, "arena.{}.purge with {n} arenas", n + 1);
                stable = true;
                break;
            }
        }
        assert!(stable, "arenas.narenas moved across eight consecutive probes");
        assert_eq!(call("arena.4000.purge"), ENOENT);
        assert_eq!(call("arena.5000.purge"), ENOENT);
        assert_eq!(call("arena.0.decay"), 0);
        assert_eq!(call(&std::format!("arena.{a}.decay")), 0);
        assert_eq!(call("arena.4096.decay"), 0);
        assert_eq!(call("arena.4000.decay"), ENOENT);
        // destroy: `EFAULT` for arena 0 and for `MALLCTL_ARENAS_ALL`,
        // `ENOENT` for an index that was never created. For an existing
        // arena challoc's `arena.<i>.destroy` is a flush of the caller's
        // caches plus a purge of that arena, nothing more: the arena index
        // stays registered, `arenas.narenas` does not move, and a later
        // `MALLOCX_ARENA(i)` allocation is served from it (jemalloc would
        // retire the index and, with live allocations, fail). Pinned here
        // so a change of these semantics has to change this test.
        assert_eq!(call("arena.0.destroy"), EFAULT);
        assert_eq!(call("arena.4096.destroy"), EFAULT);
        assert_eq!(call("arena.4000.destroy"), ENOENT);
        let narenas_before: c_uint = get("arenas.narenas");
        assert_eq!(call(&std::format!("arena.{a}.destroy")), 0);
        assert!(s.arenas.get_existing(a as u16).is_some(), "destroy keeps the arena registered");
        assert!(get::<c_uint>("arenas.narenas") >= narenas_before, "destroy retires no index");
        let again = mallocx(500, mallocx_arena(a as u16) as c_int);
        assert!(!again.is_null(), "the destroyed arena still serves MALLOCX_ARENA allocations");
        assert_eq!(s.classify_free(again).map(|x| x.1), Ok(a as u16));
        assert_eq!(call(&std::format!("arena.{a}.destroy")), 0, "destroy is repeatable");
        dallocx(again, 0);
    }

    #[test]
    fn thread_and_tcache_keys() {
        let s = shared();
        let h = heap::heap_for_thread(s).unwrap();
        let before = h.stats();
        let a0: u64 = get("thread.allocated");
        let d0: u64 = get("thread.deallocated");
        assert_eq!(a0, before.allocated);
        assert_eq!(d0, before.deallocated);
        let ap: *const u64 = get("thread.allocatedp");
        let dp: *const u64 = get("thread.deallocatedp");
        assert!(!ap.is_null() && !dp.is_null());
        assert_eq!(unsafe { *ap }, a0);
        let p = malloc(1000);
        let size = crate::layout::class_size_of(1000).unwrap() as u64;
        // The pointer is stale until a counter is read again.
        assert_eq!(unsafe { *ap }, a0);
        assert_eq!(get::<u64>("thread.allocated"), a0 + size);
        assert_eq!(unsafe { *ap }, a0 + size, "materialised by the read");
        free(p);
        assert_eq!(get::<u64>("thread.deallocated"), d0 + size);
        assert_eq!(unsafe { *dp }, d0 + size);
        // thread.tcache.flush empties this thread's caches.
        let many: Vec<*mut u8> = (0..64).map(|_| malloc(48)).collect();
        for q in many {
            free(q);
        }
        let c = crate::layout::class_of(48).unwrap();
        let h = heap::heap_for_thread(s).unwrap();
        assert!(h.cache(c).map_or(0, |cc| cc.len()) >= 64);
        assert_eq!(call("thread.tcache.flush"), 0);
        assert_eq!(h.cache(c).map_or(0, |cc| cc.len()), 0);
        // thread.tcache.enabled: off frees the caches and serves directly;
        // counters keep counting; on brings caches back.
        assert!(get::<bool>("thread.tcache.enabled"));
        let a_before: u64 = get("thread.allocated");
        assert_eq!(set("thread.tcache.enabled", false), 0);
        assert!(!get::<bool>("thread.tcache.enabled"));
        let h = heap::heap_for_thread(s).unwrap();
        assert!(h.cache(c).is_none(), "caches were freed");
        assert_eq!(get::<u64>("thread.allocated"), a_before);
        let q = malloc(48);
        assert!(h.cache(c).is_none(), "no cache is created while disabled");
        assert_eq!(get::<u64>("thread.allocated"), a_before + 48);
        free(q);
        assert!(h.cache(c).is_none());
        assert_eq!(set("thread.tcache.enabled", true), 0);
        let q = malloc(48);
        assert!(h.cache(c).is_some(), "caches come back");
        free(q);
        // thread.arena moves the home arena.
        let home: c_uint = get("thread.arena");
        assert_eq!(home as u16, h.arena());
        let a: c_uint = get("arenas.create");
        assert_eq!(set("thread.arena", a), 0);
        assert_eq!(get::<c_uint>("thread.arena"), a);
        // Plain malloc comes from that arena through the thread's own caches.
        let q = malloc(200);
        assert_eq!(s.classify_free(q).map(|x| x.1), Ok(a as u16));
        free(q);
        assert_eq!(set("thread.arena", 4000 as c_uint), EFAULT);
        assert_eq!(set("thread.arena", 5000 as c_uint), EFAULT);
        assert_eq!(set("thread.arena", home), 0);
        assert_eq!(get::<c_uint>("thread.arena"), home);
        // Explicit caches.
        let t: c_uint = get("tcache.create");
        let flags = mallocx_tcache(t as u16) as c_int;
        let e = mallocx(200, flags);
        let c = crate::layout::class_of(200).unwrap();
        dallocx(e, flags);
        let th = s.tcaches.get(t as u16).unwrap();
        assert!(th.cache_in(th.arena(), c).unwrap().len() >= 1);
        assert_eq!(set("tcache.flush", t), 0);
        assert_eq!(th.cache_in(th.arena(), c).unwrap().len(), 0);
        assert_eq!(set("tcache.destroy", t), 0);
        assert!(s.tcaches.get(t as u16).is_none());
        assert_eq!(set("tcache.destroy", t), EFAULT);
        assert_eq!(set("tcache.flush", t), EFAULT);
    }

    #[test]
    fn utilization_queries_answer_valkeys_defragmenter() {
        let s = shared();
        let c = crate::layout::class_of(100).unwrap();
        let cls = &CLASSES[c];
        let ptrs: Vec<*mut u8> = (0..500).map(|_| mallocx(100, MALLOCX_TCACHE_NONE as c_int)).collect();
        // Direct allocations on a thread that has no heap yet come from
        // arena 0 (`arena::alloc`, `Tcache::None`); once the thread has a
        // heap they come from its home arena. Either way one arena owns
        // all 500.
        let first = owner_of(s, ptrs[0] as *const c_void).expect("owned");
        // Verbose query on one pointer.
        let cname = CString::new("experimental.utilization.query").unwrap();
        let mut u = Utilization::ZERO;
        let mut len = size_of::<Utilization>();
        let p0: *const c_void = ptrs[0] as *const c_void;
        let rc = mallctl(cname.as_ptr(), &mut u as *mut Utilization as *mut c_void, &mut len, &p0 as *const *const c_void as *const c_void, size_of::<*const c_void>());
        assert_eq!(rc, 0);
        assert_eq!(u.nregs, cls.nslots());
        assert_eq!(u.size, cls.nslots() * cls.size(), "region bytes, so size / nregs is the class size");
        assert!(u.nfree < u.nregs);
        assert!(u.bin_nregs >= u.nregs && u.bin_nregs % u.nregs == 0);
        assert!(u.bin_nfree <= u.bin_nregs);
        assert!(u.bin_nregs - u.bin_nfree >= 500, "at least our 500 slots are out");
        if !u.slabcur_addr.is_null() {
            assert!(matches!(s.regions.classify(u.slabcur_addr as usize), Kind::Slab { class, slot: 0, .. } if class as usize == c));
        }
        assert_eq!(size_of::<Utilization>(), 6 * size_of::<usize>());
        // Batch query as Valkey does it: nametomib, then bymib with the
        // pointer array in and 3-tuples out.
        let mib = mib_of("experimental.utilization.batch_query");
        let mut out = std::vec![0usize; 3 * ptrs.len()];
        let mut outlen = out.len() * size_of::<usize>();
        let rc = mallctlbymib(mib.as_ptr(), mib.len(), out.as_mut_ptr() as *mut c_void, &mut outlen, ptrs.as_ptr() as *const c_void, ptrs.len() * size_of::<*mut u8>());
        assert_eq!(rc, 0);
        for (i, p) in ptrs.iter().enumerate() {
            let (nfree, nregs, size) = (out[3 * i], out[3 * i + 1], out[3 * i + 2]);
            assert_eq!(nregs, cls.nslots(), "ptr {i}");
            assert_eq!(size, cls.nslots() * cls.size(), "ptr {i}");
            let (slab, _) = s.table(c).locate(*p);
            assert_eq!(nfree, s.table(c).desc(slab).nfree(cls), "ptr {i}");
        }
        // Size mismatches are EINVAL; the pointer array is required.
        let mut short = outlen - size_of::<usize>();
        assert_eq!(mallctlbymib(mib.as_ptr(), mib.len(), out.as_mut_ptr() as *mut c_void, &mut short, ptrs.as_ptr() as *const c_void, ptrs.len() * size_of::<*mut u8>()), EINVAL);
        assert_eq!(mallctlbymib(mib.as_ptr(), mib.len(), out.as_mut_ptr() as *mut c_void, &mut outlen, ptr::null(), 0), EINVAL);
        // A huge run is one region of its own, and so is a slot of a class
        // above BIN_MAX (reported like jemalloc's large extents); a
        // non-allocation is zeros.
        let h = mallocx(3 << 20, 0);
        let m = mallocx(100 << 10, 0);
        let cm = crate::layout::class_of(100 << 10).unwrap();
        assert!(cm >= NBINS && cm < crate::layout::NCLASSES);
        let stack = 0u64;
        let four = [h as *const c_void, &stack as *const u64 as *const c_void, ptrs[1] as *const c_void, m as *const c_void];
        let mut out4 = [0usize; 12];
        let mut len4 = out4.len() * size_of::<usize>();
        assert_eq!(mallctlbymib(mib.as_ptr(), mib.len(), out4.as_mut_ptr() as *mut c_void, &mut len4, four.as_ptr() as *const c_void, four.len() * size_of::<*const c_void>()), 0);
        assert_eq!(&out4[..3], &[0, 1, s.huge.usable_for(3 << 20).unwrap()]);
        assert_eq!(&out4[3..6], &[0, 0, 0]);
        assert_eq!(out4[7], cls.nslots());
        assert_eq!(&out4[9..12], &[0, 1, CLASSES[cm].size()]);
        let um = utilization(s, m as *const c_void, true);
        assert_eq!((um.nfree, um.nregs, um.size, um.bin_nfree, um.bin_nregs), (0, 1, CLASSES[cm].size(), 0, 0));
        assert!(um.slabcur_addr.is_null());
        // A class above tcache_max comes from arena 0 on the default path
        // whatever the thread's arena (design 13.3); a cached class from
        // the thread's own arena.
        assert_eq!(owner_of(s, m as *const c_void), Some(0));
        let home = crate::heap::existing_heap(s).unwrap().arena();
        let mid = mallocx(8 << 10, 0);
        assert!(crate::layout::class_of(8 << 10).unwrap() >= NBINS);
        assert_eq!(owner_of(s, mid as *const c_void), Some(home));
        dallocx(mid, 0);
        dallocx(m, 0);
        dallocx(h, 0);
        // A freed slot (direct release, so the bit is clear) is zeros too.
        let gone = ptrs[2] as *const c_void;
        dallocx(ptrs[2], MALLOCX_TCACHE_NONE as c_int);
        assert_eq!(utilization(s, gone, true), Utilization::ZERO);
        assert_eq!(owner_of(s, gone), None);
        assert_eq!(owner_of(s, ptrs[3] as *const c_void), Some(first));
        for (i, p) in ptrs.into_iter().enumerate() {
            if i != 2 {
                dallocx(p, MALLOCX_TCACHE_NONE as c_int);
            }
        }
    }
}
