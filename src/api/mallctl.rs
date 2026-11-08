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
//! Every node is an [`Entry`]: a directory, a read-only key that carries
//! its answer in the tree (a [`Reader`], `stat(...)`, called by [`ctl`]
//! after the read-only check with the numeric MIB components as its
//! argument; adding such a key is a named reader, its `Reader` variant
//! and `call` arm, and one `stat` line), or an [`Op`], a key that does
//! something (`epoch`, the `arena.<i>.*` operations,
//! `arenas.create`/`lookup`, the `thread.*` and `tcache.*` writes, the
//! utilization queries), which is an arm of `ctl`.
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
use crate::api::ReadyMark;
use crate::arena::{ARENAS_ALL, MAX_ARENAS};
use crate::config::{KEYS, Value};
use crate::heap::{self, Heap, Shared};
use crate::huge::RunState;
#[cfg(verus_keep_ghost)]
use crate::layout::class_table;
use crate::layout::{CLASSES, Kind, NBINS, NCLASSES};
use crate::lock::LockOrder;
#[cfg(verus_keep_ghost)]
use crate::lock::{axiom_entry, rank_class};
use crate::purge;
use crate::stats::{self, ArenaStats};
use crate::sys;
use crate::sys::bytes_is;
use crate::sys::errno::{EAGAIN, EFAULT, EINVAL, ENOENT, EPERM};
#[cfg(verus_keep_ghost)]
use crate::sys::{decimal_value, digit_run};
use crate::vspec::addr::addr_to_ptr;
use crate::vspec::cbuf::CCall;
#[cfg(verus_keep_ghost)]
use crate::vspec::cbuf::{
    CArg, CEntry, c_buffer, c_readable, c_writes, lemma_c_buffer_at, lemma_c_readable_at,
};
use crate::vspec::ctl::Change;
use crate::vspec::errno::ThreadErrno;
use crate::vspec::ptr::ref_ptr;
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{axiom_ptr_add_addr, ptr_add};
use Op::*;
use Reading::*;
use vstd::prelude::*;
use vstd::raw_ptr::ptr_ref;
#[cfg(verus_keep_ghost)]
use vstd::string::*;

verus! {

/// [`VERSION_STR`] for verified code, its value opaque to the verifier.
/// `VERSION_STR` is outside `verus!`, in the form scripts/install.sh
/// reads; `verus!` builds a `const` as a function in the verifier's view,
/// so the lifetime is spelled out here.
#[verifier::external_body]
#[allow(clippy::redundant_static_lifetimes)]
pub(crate) const VERSION: &'static str = VERSION_STR;

} // verus!

/// `version`: jemalloc's `<major>.<minor>.<bugfix>-<nrev>-g<gid>` shape,
/// so parsers that split on `-` and `.` (tikv-jemalloc-ctl, Redis's
/// `mem_allocator` line) work; the "gid" says who this really is.
pub const VERSION_STR: &str = "5.3.0-0-gchalloc";

verus! {

#[allow(clippy::redundant_static_lifetimes)]
exec const VERSION_CSTR: &'static [u8] = b"5.3.0-0-gchalloc\0";
#[allow(clippy::redundant_static_lifetimes)]
exec const EMPTY_CSTR: &'static [u8] = b"\0";

} // verus!

const _: () = assert!(VERSION_CSTR.len() == VERSION_STR.len() + 1);

verus! {

/// Longest MIB (`stats.arenas.<i>.bins.<j>.curregs` has 6 components).
pub const MAX_MIB: usize = 8;
/// Longest name accepted.
const MAX_NAME: usize = 256;

/// The keys that do something: the arms of [`ctl`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Op {
    Epoch,
    BackgroundThread,
    MaxBackgroundThreads,
    ArenasCreate,
    ArenasLookup,
    /// `arena.<i>.*`; `mib[1]` is the arena (or `MALLCTL_ARENAS_ALL`).
    ArenaPurge,
    ArenaDecay,
    ArenaDestroy,
    ArenaDirtyDecayMs,
    ThreadTcacheFlush,
    ThreadTcacheEnabled,
    ThreadArena,
    TcacheCreate,
    TcacheFlush,
    TcacheDestroy,
    UtilQuery,
    UtilBatchQuery,
}

/// `MALLCTL_ARENAS_DESTROYED` (4097 in both shipped headers): jemalloc's
/// merged statistics of the arenas destroyed so far. challoc never
/// destroys an arena (`arena.<i>.destroy` purges it and keeps it
/// registered, design 13.4), so `stats.arenas.4097.*` answers zeros and
/// `arena.4097.*` is `ENOENT` like every index at or above `MAX_ARENAS`.
const ARENAS_DESTROYED: usize = 4097;

} // verus!

const _: () = assert!(ARENAS_DESTROYED >= MAX_ARENAS && ARENAS_DESTROYED != ARENAS_ALL as usize);

verus! {

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

} // verus!

verus! {

/// A read-only key's implementation, by name: [`call`] runs it with the
/// MIB (for its numeric components) and returns the answer or an error
/// code. The allocator state is [`shared`], not a parameter: a `&Shared`
/// argument the optimiser cannot trace to the one static would cost
/// every function downstream (`heap_for_thread`, `stats::snapshot`,
/// `Config::get`) the constant it is specialised on today.
///
/// One variant per reader rather than a function pointer: [`call`] is a
/// `match` of direct calls. `opt.*` reads through `OptValue`; the others
/// are named after their reader functions, which follow [`OPT`].
#[derive(Copy, Clone)]
enum Reader {
    BinSize,
    BinNregs,
    BinSlabSize,
    ArenasNarenas,
    ArenasNbins,
    ArenasNlextents,
    ArenasPage,
    ArenasQuantum,
    BinCurregs,
    BinCurslabs,
    BinNonfullSlabs,
    SmallAllocated,
    SmallResident,
    LargeAllocated,
    ArenaPactive,
    ArenaPdirty,
    ArenaPmuzzy,
    ArenaResident,
    ArenaMapped,
    ArenaRetained,
    CgroupLimit,
    CgroupHigh,
    CgroupCurrent,
    CgroupDecayMs,
    CgroupRetentionCap,
    CgroupEmptyBytes,
    CgroupFullPurges,
    CgroupPurgeAdvice,
    CgroupLazyPurges,
    CgroupEagerPurges,
    ThpMode,
    ThpClassRanges,
    ThpClassBytes,
    ThpHugeRanges,
    ThpMetaRanges,
    ThpFailures,
    ThpNohugeRanges,
    StatsAllocated,
    StatsActive,
    StatsMetadata,
    StatsResident,
    StatsMapped,
    StatsRetained,
    ThreadAllocated,
    ThreadAllocatedp,
    ThreadDeallocated,
    ThreadDeallocatedp,
    ConfigMallocConf,
    ConfigDebug,
    ConfigStats,
    Version,
    OptValue,
}

impl Reader {
    /// The reader needs `may_abort`: the `thread.*` readers, which may
    /// create the thread's heap. The others never abort: those that load
    /// the allocator state take the [`ReadyMark`], with which their
    /// `shared()` takes the path that does not abort
    /// ([`crate::api::shared_marked`]).
    spec fn aborts(self) -> bool {
        match self {
            Reader::ThreadAllocated | Reader::ThreadAllocatedp | Reader::ThreadDeallocated | Reader::ThreadDeallocatedp => true,
            _ => false,
        }
    }
}

/// `read(mib)`: the reader's function, called directly, with `mark`
/// for the readers that load the allocator state.
#[inline(always)]
fn call(read: Reader, mib: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::sys::may_abort() || !read.aborts(),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    match read {
        Reader::BinSize => bin_size(mib),
        Reader::BinNregs => bin_nregs(mib),
        Reader::BinSlabSize => bin_slab_size(mib),
        Reader::ArenasNarenas => arenas_narenas(mib, Tracked(mark)),
        Reader::ArenasNbins => arenas_nbins(mib),
        Reader::ArenasNlextents => arenas_nlextents(mib),
        Reader::ArenasPage => arenas_page(mib, Tracked(mark)),
        Reader::ArenasQuantum => arenas_quantum(mib),
        Reader::BinCurregs => bin_curregs(mib, Tracked(mark)),
        Reader::BinCurslabs => bin_curslabs(mib, Tracked(mark)),
        Reader::BinNonfullSlabs => bin_nonfull_slabs(mib, Tracked(mark)),
        Reader::SmallAllocated => small_allocated(mib, Tracked(mark)),
        Reader::SmallResident => small_resident(mib, Tracked(mark)),
        Reader::LargeAllocated => large_allocated(mib, Tracked(mark)),
        Reader::ArenaPactive => arena_pactive(mib, Tracked(mark)),
        Reader::ArenaPdirty => arena_pdirty(mib, Tracked(mark)),
        Reader::ArenaPmuzzy => arena_pmuzzy(mib, Tracked(mark)),
        Reader::ArenaResident => arena_resident(mib, Tracked(mark)),
        Reader::ArenaMapped => arena_mapped(mib, Tracked(mark)),
        Reader::ArenaRetained => arena_retained(mib, Tracked(mark)),
        Reader::CgroupLimit => cgroup_limit(mib, Tracked(mark)),
        Reader::CgroupHigh => cgroup_high(mib, Tracked(mark)),
        Reader::CgroupCurrent => cgroup_current(mib, Tracked(mark)),
        Reader::CgroupDecayMs => cgroup_decay_ms(mib, Tracked(mark)),
        Reader::CgroupRetentionCap => cgroup_retention_cap(mib, Tracked(mark)),
        Reader::CgroupEmptyBytes => cgroup_empty_bytes(mib, Tracked(mark)),
        Reader::CgroupFullPurges => cgroup_full_purges(mib, Tracked(mark)),
        Reader::CgroupPurgeAdvice => cgroup_purge_advice(mib),
        Reader::CgroupLazyPurges => cgroup_lazy_purges(mib),
        Reader::CgroupEagerPurges => cgroup_eager_purges(mib),
        Reader::ThpMode => thp_mode(mib),
        Reader::ThpClassRanges => thp_class_ranges(mib),
        Reader::ThpClassBytes => thp_class_bytes(mib),
        Reader::ThpHugeRanges => thp_huge_ranges(mib),
        Reader::ThpMetaRanges => thp_meta_ranges(mib),
        Reader::ThpFailures => thp_failures(mib),
        Reader::ThpNohugeRanges => thp_nohuge_ranges(mib),
        Reader::StatsAllocated => stats_allocated(mib, Tracked(mark)),
        Reader::StatsActive => stats_active(mib, Tracked(mark)),
        Reader::StatsMetadata => stats_metadata(mib, Tracked(mark)),
        Reader::StatsResident => stats_resident(mib, Tracked(mark)),
        Reader::StatsMapped => stats_mapped(mib, Tracked(mark)),
        Reader::StatsRetained => stats_retained(mib, Tracked(mark)),
        Reader::ThreadAllocated => thread_allocated(mib, Tracked(mark)),
        Reader::ThreadAllocatedp => thread_allocatedp(mib, Tracked(mark)),
        Reader::ThreadDeallocated => thread_deallocated(mib, Tracked(mark)),
        Reader::ThreadDeallocatedp => thread_deallocatedp(mib, Tracked(mark)),
        Reader::ConfigMallocConf => config_malloc_conf(mib),
        Reader::ConfigDebug => config_debug(mib),
        Reader::ConfigStats => config_stats(mib),
        Reader::Version => version(mib),
        Reader::OptValue => opt_value(mib, Tracked(mark)),
    }
}

/// What [`ctl`] does at `n` never aborts: an interior node, a read whose
/// reader does not abort (every key but `thread.*`'s four, so every key
/// under `opt`, `arenas` and `stats`), or a utilization query.
spec fn ctl_never_aborts(n: Node, mib: Seq<usize>) -> bool {
    match n.entry {
        Entry::Dir(_) => true,
        Entry::Read(r) => !r.aborts(),
        Entry::Op(op) => op == Op::UtilQuery || op == Op::UtilBatchQuery,
    }
}

/// The allocator state, initialised (the readers run inside `mallctl`,
/// after its own [`super::shared_published`], whose [`ReadyMark`] is
/// `mark`, so this takes the path that does not abort).
fn shared(Tracked(mark): Tracked<&'static ReadyMark>) -> (r: &'static Shared)
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
    ensures
        r.wf(),
{
    super::shared_marked(Tracked(mark))
}


/// One node of the name tree. A node named [`NUMERIC`] is the only child
/// of its parent and matches any decimal component, which becomes the MIB
/// entry.
#[derive(Copy, Clone)]
struct Node {
    name: &'static str,
    entry: Entry,
}

/// What a node is.
#[derive(Copy, Clone)]
enum Entry {
    /// An interior node: it resolves to a MIB, but using it as a key is
    /// `ENOENT`.
    Dir(&'static [Node]),
    /// A read-only key answered by its reader.
    Read(Reader),
    /// A key that does something, an arm of [`ctl`].
    Op(Op),
}

impl Node {
    /// The children of a directory; none for a key.
    fn children(&self) -> (r: &'static [Node])
        ensures
            r@ == spec_children(*self),
    {
        match self.entry {
            Entry::Dir(children) => children,
            _ => &[],
        }
    }
}

// The lifetime is spelled out, as for `VERSION`.
#[allow(clippy::redundant_static_lifetimes)]
const NUMERIC: &'static str = "*";

const fn op(name: &'static str, o: Op) -> Node {
    Node {
        name,
        entry: Entry::Op(o),
    }
}

const fn dir(name: &'static str, children: &'static [Node]) -> Node {
    Node {
        name,
        entry: Entry::Dir(children),
    }
}

const fn num(children: &'static [Node]) -> Node {
    dir(NUMERIC, children)
}

/// A read-only key, answered by its reader ([`Reader`]).
const fn stat(name: &'static str, read: Reader) -> Node {
    Node {
        name,
        entry: Entry::Read(read),
    }
}

/// One `opt.<key>` node per [`KEYS`] entry, in order, each read by `read`
/// ([`Reader::OptValue`]). The loop also tests `out`'s length, which is
/// `KEYS.len()` by its type: the verifier does not evaluate the array
/// length.
const fn opt_nodes(read: Reader) -> [Node; KEYS.len()] {
    let keys = KEYS;
    let mut out = [dir("", &[]); KEYS.len()];
    let mut i = 0;
    while i < keys.len() && i < out.len()
        invariant
            i <= keys@.len(),
        decreases keys@.len() - i,
    {
        out[i] = stat(keys[i], read);
        i += 1;
    }
    out
}

} // verus!

static OPT: [Node; KEYS.len()] = opt_nodes(Reader::OptValue);

verus! {

// The readers of `arenas.*` and `stats.*`, named so that Verus checks
// them. Those that read the allocator state require `may_abort`: they get
// it through [`shared`], whose `api::shared` may run init. `opt.*` reads
// through `opt_value`. The `thread.*` readers, `config.*` and `version`
// follow; `thread.*` also may create the thread's heap.

fn bin_size(m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(bin_class(m)?.size()))
}

fn bin_nregs(m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(U32(bin_class(m)?.nslots() as u32))
}

fn bin_slab_size(m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(bin_class(m)?.slab_size()))
}

fn arenas_narenas(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(Unsigned(shared(Tracked(mark)).arenas.count() as c_uint))
}

fn arenas_nbins(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Unsigned(NBINS as c_uint))
}

fn arenas_nlextents(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Unsigned(0))
}

fn arenas_page(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    let s = shared(Tracked(mark));
    proof {
        s.lemma_wf();
    }
    Ok(Size(s.regions.page_size()))
}

fn arenas_quantum(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(QUANTUM))
}

fn bin_curregs(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Curregs, Tracked(mark))
}

fn bin_curslabs(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Curslabs, Tracked(mark))
}

fn bin_nonfull_slabs(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::NonfullSlabs, Tracked(mark))
}

fn small_allocated(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::SmallAllocated, Tracked(mark))
}

fn small_resident(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::SmallResident, Tracked(mark))
}

fn large_allocated(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::LargeAllocated, Tracked(mark))
}

fn arena_pactive(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Pactive, Tracked(mark))
}

fn arena_pdirty(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Pdirty, Tracked(mark))
}

fn arena_pmuzzy(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Pmuzzy, Tracked(mark))
}

fn arena_resident(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Resident, Tracked(mark))
}

fn arena_mapped(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Mapped, Tracked(mark))
}

fn arena_retained(m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    arena_figure(m, Fig::Retained, Tracked(mark))
}

fn cgroup_limit(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(Size(tracked(Tracked(mark))?.limit() as usize))
}

fn cgroup_high(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(Size(tracked(Tracked(mark))?.high() as usize))
}

fn cgroup_current(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Rate limited to once per 10 ms, so a polling reader costs at
    // most that many file reads.
    let cg = tracked(Tracked(mark))?;
    cg.sample(sys::now_coarse_ns());
    Ok(Size(cg.current() as usize))
}

fn cgroup_decay_ms(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    tracked(Tracked(mark))?;
    let ns = shared(Tracked(mark)).policy.effective_decay_ns(shared(Tracked(mark)).decay_ns());
    // `NEVER` reads back as -1, which is `usize::MAX` here.
    Ok(Size(purge::ms_of(ns) as usize))
}

fn cgroup_retention_cap(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    tracked(Tracked(mark))?;
    Ok(Size(shared(Tracked(mark)).policy.cap() as usize))
}

fn cgroup_empty_bytes(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    tracked(Tracked(mark))?;
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(crate::policy::empty_bytes(shared(Tracked(mark)), Tracked(&mut axiom_entry())) as usize))
}

fn cgroup_full_purges(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    tracked(Tracked(mark))?;
    Ok(Size(shared(Tracked(mark)).policy.full_purges() as usize))
}

fn cgroup_purge_advice(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size((purge::advice() == purge::Advice::Lazy) as usize))
}

fn cgroup_lazy_purges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(purge::batches().0 as usize))
}

fn cgroup_eager_purges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(purge::batches().1 as usize))
}

fn thp_mode(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::decision().mode as usize))
}

fn thp_class_ranges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().class_ranges as usize))
}

fn thp_class_bytes(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().class_bytes as usize))
}

fn thp_huge_ranges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().huge_ranges as usize))
}

fn thp_meta_ranges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().meta_ranges as usize))
}

fn thp_failures(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().failures as usize))
}

fn thp_nohuge_ranges(_m: &[usize]) -> Result<Reading, c_int>
    requires
        crate::sys::may_syscall(),
{
    Ok(Size(crate::thp::stats().nohuge_ranges as usize))
}

fn stats_allocated(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.allocated))
}

fn stats_active(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.active))
}

fn stats_metadata(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.metadata))
}

fn stats_resident(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.resident))
}

fn stats_mapped(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.mapped))
}

fn stats_retained(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    // Premise (lock order): called from `mallctl`, which holds no lock.
    Ok(Size(stats::snapshot(shared(Tracked(mark)), Tracked(&mut axiom_entry())).totals.retained))
}

fn thread_allocated(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::sys::may_abort(),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(U64(thread_heap(shared(Tracked(mark)))?.materialise_stats().allocated))
}

fn thread_allocatedp(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::sys::may_abort(),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(U64Ptr(ptr::from_ref(&thread_heap(shared(Tracked(mark)))?.materialise_stats().allocated)))
}

fn thread_deallocated(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::sys::may_abort(),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(U64(thread_heap(shared(Tracked(mark)))?.materialise_stats().deallocated))
}

fn thread_deallocatedp(_m: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::sys::may_abort(),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    Ok(U64Ptr(ptr::from_ref(&thread_heap(shared(Tracked(mark)))?.materialise_stats().deallocated)))
}

fn config_malloc_conf(_m: &[usize]) -> Result<Reading, c_int> {
    Ok(Str(EMPTY_CSTR.as_ptr() as *const c_char))
}

fn config_debug(_m: &[usize]) -> Result<Reading, c_int> {
    Ok(Bool(false))
}

fn config_stats(_m: &[usize]) -> Result<Reading, c_int> {
    Ok(Bool(true))
}

fn version(_m: &[usize]) -> Result<Reading, c_int> {
    Ok(Str(VERSION_CSTR.as_ptr() as *const c_char))
}

} // verus!

static BIN_GEOMETRY: [Node; 3] = [
    stat("size", Reader::BinSize),
    stat("nregs", Reader::BinNregs),
    stat("slab_size", Reader::BinSlabSize),
];
static ARENAS_BIN: [Node; 1] = [num(&BIN_GEOMETRY)];
static ARENAS: [Node; 8] = [
    stat("narenas", Reader::ArenasNarenas),
    stat("nbins", Reader::ArenasNbins),
    stat("nlextents", Reader::ArenasNlextents),
    stat("page", Reader::ArenasPage),
    stat("quantum", Reader::ArenasQuantum),
    dir("bin", &ARENAS_BIN),
    op("create", ArenasCreate),
    op("lookup", ArenasLookup),
];
static ARENA_I: [Node; 4] = [
    op("purge", ArenaPurge),
    op("decay", ArenaDecay),
    op("destroy", ArenaDestroy),
    op("dirty_decay_ms", ArenaDirtyDecayMs),
];
static ARENA: [Node; 1] = [num(&ARENA_I)];
static STATS_BIN_FIELDS: [Node; 3] = [
    stat("curregs", Reader::BinCurregs),
    stat("curslabs", Reader::BinCurslabs),
    stat("nonfull_slabs", Reader::BinNonfullSlabs),
];
static STATS_BINS: [Node; 1] = [num(&STATS_BIN_FIELDS)];
static STATS_SMALL: [Node; 2] = [
    stat("allocated", Reader::SmallAllocated),
    stat("resident", Reader::SmallResident),
];
static STATS_LARGE: [Node; 1] = [stat("allocated", Reader::LargeAllocated)];
static STATS_ARENA_I: [Node; 9] = [
    stat("pactive", Reader::ArenaPactive),
    stat("pdirty", Reader::ArenaPdirty),
    stat("pmuzzy", Reader::ArenaPmuzzy),
    dir("small", &STATS_SMALL),
    dir("large", &STATS_LARGE),
    stat("resident", Reader::ArenaResident),
    stat("mapped", Reader::ArenaMapped),
    stat("retained", Reader::ArenaRetained),
    dir("bins", &STATS_BINS),
];
static STATS_ARENAS: [Node; 1] = [num(&STATS_ARENA_I)];
// `stats.cgroup.*` (design 11): the first seven need a tracked cgroup
// (`tracked`); the policy's figures are arena 0's effective decay period
// in ms (`SIZE_MAX` for never), the retention cap in bytes (`SIZE_MAX`
// without a limit), the `Empty` bytes held now (live, not the last
// evaluation's figure: a reader without a limit would otherwise see 0)
// and the full purges run on `memory.events` `high` rises. The last three
// are the two-advice purge (design 8): the advice the next purge issues
// (1 lazy, `MADV_FREE`; 0 eager, `MADV_DONTNEED`) and the batches with
// each that discarded anything so far; they read with or without a
// cgroup: the advice exists (eager without one) whether or not one is
// read.
static STATS_CGROUP: [Node; 10] = [
    stat("limit", Reader::CgroupLimit),
    stat("high", Reader::CgroupHigh),
    stat("current", Reader::CgroupCurrent),
    stat("decay_ms", Reader::CgroupDecayMs),
    stat("retention_cap", Reader::CgroupRetentionCap),
    stat("empty_bytes", Reader::CgroupEmptyBytes),
    stat("full_purges", Reader::CgroupFullPurges),
    stat("purge_advice", Reader::CgroupPurgeAdvice),
    stat("lazy_purges", Reader::CgroupLazyPurges),
    stat("eager_purges", Reader::CgroupEagerPurges),
];
// `stats.thp.*` (design 7 and 17, `thp.rs`): the mode in force (0 off,
// 1 folios, 2 PMD chunks), the ranges advised per region kind, and the
// reservations `never` `MADV_NOHUGEPAGE`d (the class superblock and the
// metadata region; 0 under `auto` and `always`).
static STATS_THP: [Node; 7] = [
    stat("mode", Reader::ThpMode),
    stat("class_ranges", Reader::ThpClassRanges),
    stat("class_bytes", Reader::ThpClassBytes),
    stat("huge_ranges", Reader::ThpHugeRanges),
    stat("meta_ranges", Reader::ThpMetaRanges),
    stat("failures", Reader::ThpFailures),
    stat("nohuge_ranges", Reader::ThpNohugeRanges),
];
static STATS: [Node; 9] = [
    stat("allocated", Reader::StatsAllocated),
    stat("active", Reader::StatsActive),
    stat("metadata", Reader::StatsMetadata),
    stat("resident", Reader::StatsResident),
    stat("mapped", Reader::StatsMapped),
    stat("retained", Reader::StatsRetained),
    dir("arenas", &STATS_ARENAS),
    dir("cgroup", &STATS_CGROUP),
    dir("thp", &STATS_THP),
];
static THREAD_TCACHE: [Node; 2] = [
    op("flush", ThreadTcacheFlush),
    op("enabled", ThreadTcacheEnabled),
];
static THREAD: [Node; 6] = [
    dir("tcache", &THREAD_TCACHE),
    stat("allocated", Reader::ThreadAllocated),
    stat("allocatedp", Reader::ThreadAllocatedp),
    stat("deallocated", Reader::ThreadDeallocated),
    stat("deallocatedp", Reader::ThreadDeallocatedp),
    op("arena", ThreadArena),
];
static TCACHE: [Node; 3] = [
    op("create", TcacheCreate),
    op("flush", TcacheFlush),
    op("destroy", TcacheDestroy),
];
static UTILIZATION: [Node; 2] = [op("query", UtilQuery), op("batch_query", UtilBatchQuery)];
static EXPERIMENTAL: [Node; 1] = [dir("utilization", &UTILIZATION)];
static CONFIG: [Node; 3] = [
    stat("malloc_conf", Reader::ConfigMallocConf),
    stat("debug", Reader::ConfigDebug),
    stat("stats", Reader::ConfigStats),
];
static ROOT: [Node; 12] = [
    stat("version", Reader::Version),
    op("epoch", Epoch),
    op("background_thread", BackgroundThread),
    op("max_background_threads", MaxBackgroundThreads),
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

verus! {

/// The first component of a dotted name and the rest after its dot, if
/// there is a dot.
fn split_component(name: &[u8]) -> (r: (&[u8], Option<&[u8]>))
    ensures
        forall|j: int| 0 <= j < r.0@.len() ==> #[trigger] r.0@[j] != 46,
        r.1 is None ==> r.0@ == name@,
        r.1 matches Some(t) ==> name@ == r.0@ + seq![46u8] + t@,
{
    // `match name.iter().position(|&b| b == b'.')`, spelled out: Verus
    // does not specify `Iterator::position`.
    let mut i = 0;
    while i < name.len()
        invariant
            i <= name@.len(),
            forall|j: int| 0 <= j < i ==> #[trigger] name@[j] != 46,
        decreases name@.len() - i,
    {
        if name[i] == b'.' {
            let r = (&name[..i], Some(&name[i + 1..]));
            proof {
                assert(name@ =~= name@.subrange(0, i as int) + seq![46u8] + name@.subrange(
                    i + 1,
                    name@.len() as int,
                ));
            }
            return r;
        }
        i += 1;
    }
    (name, None)
}

/// A decimal component (jemalloc parses these with `strtoumax(.., 10)`):
/// its value, or `None` when it is empty, holds a byte that is not a
/// digit, or does not fit a `size_t`.
fn parse_index(s: &[u8]) -> (r: Option<usize>)
    ensures
        r is Some <==> s@.len() > 0 && digit_run(s@) == s@.len() && decimal_value(s@) <= usize::MAX,
        r matches Some(v) ==> v == decimal_value(s@),
{
    // The whole component is digits: `parse_decimal` alone would also
    // take `1x` as 1.
    if sys::leading_digits(s) != s.len() {
        return None;
    }
    proof {
        assert(s@.take(s@.len() as int) =~= s@);
    }
    match sys::parse_decimal(s) {
        Some(v) => {
            proof {
                sys::lemma_decimal_value_nonneg(s@);
            }
            Some(v as usize)
        },
        None => None,
    }
}

/// The node is the numeric wildcard [`NUMERIC`].
spec fn numeric(n: Node) -> bool {
    n.name.spec_bytes() == NUMERIC.spec_bytes()
}

/// `n.name == NUMERIC`.
#[inline(always)]
fn is_numeric(n: &Node) -> (r: bool)
    ensures
        r == numeric(*n),
{
    bytes_is(n.name.as_bytes(), NUMERIC.as_bytes())
}

/// The nodes of [`ROOT`]: uninterpreted, since the static's initialiser
/// lies outside `verus!`. [`root`] returns them, so both traversals start
/// from the same tree.
uninterp spec fn root_nodes() -> Seq<Node>;

/// [`ROOT`]. Trusted: the address of a static outside `verus!`, whose
/// initialiser Verus does not see. It claims only that every call
/// returns the same nodes, [`root_nodes`].
#[verifier::external_body]
#[inline(always)]
fn root() -> (r: &'static [Node])
    ensures
        r@ == root_nodes(),
{
    &ROOT
}

/// The children of `n`, as [`Node::children`] returns them.
spec fn spec_children(n: Node) -> Seq<Node> {
    match n.entry {
        Entry::Dir(children) => children@,
        _ => Seq::empty(),
    }
}

/// The child of `nodes` at MIB entry `i`, as [`child_by_index`] finds it:
/// the lone numeric node for any `i`, else the child at index `i`.
spec fn step(nodes: Seq<Node>, i: usize) -> Option<Node> {
    if nodes.len() == 1 && numeric(nodes[0]) {
        Some(nodes[0])
    } else if i < nodes.len() {
        Some(nodes[i as int])
    } else {
        None
    }
}

/// The node the MIB `mib` names below `nodes` ([`mib_to_node`]'s walk):
/// each entry picks a child with [`step`], and the next entry looks among
/// that child's children. `None` for an empty MIB or a bad entry.
spec fn resolve(nodes: Seq<Node>, mib: Seq<usize>) -> Option<Node>
    decreases mib.len(),
{
    if mib.len() == 0 {
        None
    } else {
        match step(nodes, mib[0]) {
            None => None,
            Some(n) => if mib.len() == 1 {
                Some(n)
            } else {
                resolve(spec_children(n), mib.drop_first())
            },
        }
    }
}

/// The child of `nodes` that component `c` names with MIB entry `i`
/// ([`child`]'s match): under a lone numeric node, `c` is a decimal
/// number and `i` its value; otherwise `i` indexes a child named `c`.
spec fn names(nodes: Seq<Node>, c: Seq<u8>, i: usize) -> Option<Node> {
    if nodes.len() == 1 && numeric(nodes[0]) {
        if c.len() > 0 && digit_run(c) == c.len() && i == decimal_value(c) {
            Some(nodes[0])
        } else {
            None
        }
    } else if i < nodes.len() && nodes[i as int].name.spec_bytes() == c {
        Some(nodes[i as int])
    } else {
        None
    }
}

/// The node the components `comps` name below `nodes` when `mib` is their
/// MIB ([`name_to_mib`]'s walk): one entry per component, each matched by
/// [`names`] among the children of the node before it.
spec fn walk(nodes: Seq<Node>, comps: Seq<Seq<u8>>, mib: Seq<usize>) -> Option<Node>
    decreases mib.len(),
{
    if mib.len() == 0 || comps.len() != mib.len() {
        None
    } else {
        match names(nodes, comps[0], mib[0]) {
            None => None,
            Some(n) => if mib.len() == 1 {
                Some(n)
            } else {
                walk(spec_children(n), comps.drop_first(), mib.drop_first())
            },
        }
    }
}

/// The components joined with dots.
spec fn join(comps: Seq<Seq<u8>>) -> Seq<u8>
    decreases comps.len(),
{
    if comps.len() <= 1 {
        if comps.len() == 1 {
            comps[0]
        } else {
            Seq::empty()
        }
    } else {
        comps[0] + seq![46u8] + join(comps.drop_first())
    }
}

/// `comps` are the dotted components of `name`: at least one, none
/// holding a dot, joined back into `name`. They are unique
/// ([`lemma_split_unique`]).
spec fn split(name: Seq<u8>, comps: Seq<Seq<u8>>) -> bool {
    &&& comps.len() >= 1
    &&& forall|k: int, j: int|
        0 <= k < comps.len() && 0 <= j < comps[k].len() ==> #[trigger] comps[k][j] != 46
    &&& join(comps) == name
}

/// The sibling list the entry `mib[j]` picks a child from: `nodes` for
/// the first entry, then the children of each node the entries before it
/// reach.
spec fn level(nodes: Seq<Node>, mib: Seq<usize>, j: int) -> Seq<Node>
    decreases j,
{
    if j <= 0 {
        nodes
    } else {
        match step(nodes, mib[0]) {
            Some(n) => level(spec_children(n), mib.drop_first(), j - 1),
            None => Seq::empty(),
        }
    }
}

/// A name's walk follows the MIB's resolution: a matched component is a
/// [`step`] to the same child.
proof fn lemma_walk_resolves(nodes: Seq<Node>, comps: Seq<Seq<u8>>, mib: Seq<usize>)
    requires
        walk(nodes, comps, mib) is Some,
    ensures
        resolve(nodes, mib) == walk(nodes, comps, mib),
    decreases mib.len(),
{
    if mib.len() > 1 {
        let n = names(nodes, comps[0], mib[0])->Some_0;
        lemma_walk_resolves(spec_children(n), comps.drop_first(), mib.drop_first());
    }
}

/// Entry `j` of `mib` is the one component `j` of `comps` names: at a
/// numeric level, the component's decimal value; otherwise the index of
/// a child named by the component.
spec fn entry_named(nodes: Seq<Node>, comps: Seq<Seq<u8>>, mib: Seq<usize>, j: int) -> bool {
    let l = level(nodes, mib, j);
    &&& l.len() == 1 && numeric(l[0]) ==> comps[j].len() > 0 && digit_run(comps[j]) == comps[j].len() && mib[j]
        == decimal_value(comps[j])
    &&& !(l.len() == 1 && numeric(l[0])) ==> mib[j] < l.len() && l[mib[j] as int].name.spec_bytes()
        == comps[j]
}

/// Along a name's walk, every entry is the one its component names.
proof fn lemma_walk_entries(nodes: Seq<Node>, comps: Seq<Seq<u8>>, mib: Seq<usize>, j: int)
    requires
        walk(nodes, comps, mib) is Some,
        0 <= j < mib.len(),
    ensures
        entry_named(nodes, comps, mib, j),
    decreases j,
{
    if j > 0 {
        let n = names(nodes, comps[0], mib[0])->Some_0;
        lemma_walk_entries(spec_children(n), comps.drop_first(), mib.drop_first(), j - 1);
    }
}

/// Extending a walk by one component matched among the last node's
/// children.
proof fn lemma_walk_push(
    nodes: Seq<Node>,
    comps: Seq<Seq<u8>>,
    mib: Seq<usize>,
    last: Node,
    c: Seq<u8>,
    i: usize,
)
    requires
        walk(nodes, comps, mib) == Some(last),
        names(spec_children(last), c, i) is Some,
    ensures
        walk(nodes, comps.push(c), mib.push(i)) == names(spec_children(last), c, i),
    decreases mib.len(),
{
    let n = names(nodes, comps[0], mib[0])->Some_0;
    assert(comps.push(c)[0] == comps[0]);
    assert(mib.push(i)[0] == mib[0]);
    assert(names(nodes, comps.push(c)[0], mib.push(i)[0]) == Some(n));
    if mib.len() == 1 {
        assert(n == last);
        assert(comps.push(c).drop_first() =~= seq![c]);
        assert(mib.push(i).drop_first() =~= seq![i]);
        assert(seq![c][0] == c && seq![i][0] == i);
        assert(walk(spec_children(last), seq![c], seq![i]) == names(spec_children(last), c, i));
    } else {
        lemma_walk_push(spec_children(n), comps.drop_first(), mib.drop_first(), last, c, i);
        assert(comps.push(c).drop_first() =~= comps.drop_first().push(c));
        assert(mib.push(i).drop_first() =~= mib.drop_first().push(i));
        assert(walk(spec_children(n), comps.push(c).drop_first(), mib.push(i).drop_first())
            == names(spec_children(last), c, i));
    }
}

/// Joining one more component appends a dot and the component.
proof fn lemma_join_push(comps: Seq<Seq<u8>>, c: Seq<u8>)
    requires
        comps.len() >= 1,
    ensures
        join(comps.push(c)) == join(comps) + seq![46u8] + c,
    decreases comps.len(),
{
    assert(comps.push(c)[0] == comps[0]);
    if comps.len() == 1 {
        assert(comps.push(c).drop_first() =~= seq![c]);
        assert(seq![c][0] == c);
        assert(join(seq![c]) == c);
        assert(join(comps) == comps[0]);
    } else {
        let rest = comps.drop_first();
        lemma_join_push(rest, c);
        assert(comps.push(c).drop_first() =~= rest.push(c));
        assert(join(comps.push(c)) == comps[0] + seq![46u8] + join(rest.push(c)));
        assert(join(comps) == comps[0] + seq![46u8] + join(rest));
        assert(join(comps.push(c)) =~= join(comps) + seq![46u8] + c);
    }
}

/// A name splits into its components one way only.
proof fn lemma_split_unique(name: Seq<u8>, a: Seq<Seq<u8>>, b: Seq<Seq<u8>>)
    requires
        split(name, a),
        split(name, b),
    ensures
        a == b,
    decreases a.len(),
{
    // The first component is `name` up to its first dot (or all of it).
    let (x, y) = (a[0], b[0]);
    assert forall|j: int| 0 <= j < x.len() implies #[trigger] x[j] != 46 by {
        assert(a[0][j] != 46);
    }
    assert forall|j: int| 0 <= j < y.len() implies #[trigger] y[j] != 46 by {
        assert(b[0][j] != 46);
    }
    assert(x.len() <= name.len() && name.subrange(0, x.len() as int) =~= x) by {
        if a.len() > 1 {
            assert(name =~= x + (seq![46u8] + join(a.drop_first())));
        }
    }
    assert(y.len() <= name.len() && name.subrange(0, y.len() as int) =~= y) by {
        if b.len() > 1 {
            assert(name =~= y + (seq![46u8] + join(b.drop_first())));
        }
    }
    // A shorter first component is followed by a dot, inside the longer.
    if x.len() < y.len() {
        assert(a.len() > 1);
        assert(name =~= x + (seq![46u8] + join(a.drop_first())));
        assert(name[x.len() as int] == 46);
        assert(y[x.len() as int] == name[x.len() as int]);
    }
    if y.len() < x.len() {
        assert(b.len() > 1);
        assert(name =~= y + (seq![46u8] + join(b.drop_first())));
        assert(name[y.len() as int] == 46);
        assert(x[y.len() as int] == name[y.len() as int]);
    }
    assert(x =~= y);
    if a.len() == 1 || b.len() == 1 {
        // One side is a single component: `name` has no dot, so neither
        // has a second.
        if a.len() > 1 {
            assert(name =~= x + (seq![46u8] + join(a.drop_first())));
            assert(name[x.len() as int] == 46);
            assert(name.len() == y.len());
        }
        if b.len() > 1 {
            assert(name =~= y + (seq![46u8] + join(b.drop_first())));
            assert(name[y.len() as int] == 46);
            assert(name.len() == x.len());
        }
        assert(a =~= b);
    } else {
        let rest = name.subrange(x.len() + 1int, name.len() as int);
        assert(name =~= x + (seq![46u8] + join(a.drop_first())));
        assert(name =~= y + (seq![46u8] + join(b.drop_first())));
        assert(join(a.drop_first()) =~= rest);
        assert(join(b.drop_first()) =~= rest);
        assert forall|k: int, j: int|
            0 <= k < a.drop_first().len() && 0 <= j < a.drop_first()[k].len() implies
            #[trigger] a.drop_first()[k][j] != 46 by {
            assert(a[k + 1][j] != 46);
        }
        assert forall|k: int, j: int|
            0 <= k < b.drop_first().len() && 0 <= j < b.drop_first()[k].len() implies
            #[trigger] b.drop_first()[k][j] != 46 by {
            assert(b[k + 1][j] != 46);
        }
        lemma_split_unique(rest, a.drop_first(), b.drop_first());
        assert(a =~= seq![x] + a.drop_first());
        assert(b =~= seq![y] + b.drop_first());
    }
}

/// Resolving a MIB whose first `k` entries name `n` continues among `n`'s
/// children.
proof fn lemma_resolve_split(nodes: Seq<Node>, mib: Seq<usize>, k: int, n: Node)
    requires
        1 <= k < mib.len(),
        resolve(nodes, mib.take(k)) == Some(n),
    ensures
        resolve(nodes, mib) == resolve(spec_children(n), mib.skip(k)),
    decreases k,
{
    assert(mib.take(k)[0] == mib[0]);
    if k == 1 {
        assert(mib.skip(1) =~= mib.drop_first());
    } else {
        let m = step(nodes, mib[0])->Some_0;
        assert(mib.take(k).drop_first() =~= mib.drop_first().take(k - 1));
        assert(mib.skip(k) =~= mib.drop_first().skip(k - 1));
        lemma_resolve_split(spec_children(m), mib.drop_first(), k - 1, n);
    }
}

/// **MIB round-trip.** If `name` splits into `comps` and `walk` matches
/// them to `n` with the MIB `mib` (what [`name_to_mib`] returns), the MIB
/// resolves to `n` (so [`mib_to_node`] returns it), the split is the only
/// one, and every entry is the one its component names: at a numeric
/// level (an arena or bin index) the component's decimal value.
proof fn theorem_mib_round_trip(name: Seq<u8>, comps: Seq<Seq<u8>>, mib: Seq<usize>, n: Node)
    requires
        split(name, comps),
        walk(root_nodes(), comps, mib) == Some(n),
    ensures
        resolve(root_nodes(), mib) == Some(n),
        forall|other: Seq<Seq<u8>>| #[trigger] split(name, other) ==> other == comps,
        mib.len() == comps.len(),
        forall|j: int| 0 <= j < mib.len() ==> #[trigger] entry_named(root_nodes(), comps, mib, j),
{
    lemma_walk_resolves(root_nodes(), comps, mib);
    assert forall|other: Seq<Seq<u8>>| #[trigger] split(name, other) implies other == comps by {
        lemma_split_unique(name, other, comps);
    }
    assert forall|j: int| 0 <= j < mib.len() implies #[trigger] entry_named(
        root_nodes(),
        comps,
        mib,
        j,
    ) by {
        lemma_walk_entries(root_nodes(), comps, mib, j);
    }
}

/// The child of `nodes` that `comp` names, with its MIB entry: the
/// number itself under a numeric node, else the child's index.
fn child(nodes: &'static [Node], comp: &[u8]) -> (r: Option<(usize, &'static Node)>)
    ensures
        r matches Some((i, n)) ==> names(nodes@, comp@, i) == Some(*n),
{
    // `if let [n] = nodes` and `nodes.iter().position(..).map(..)`,
    // spelled out: Verus supports neither slice patterns nor
    // `Iterator::position`.
    if nodes.len() == 1 {
        let n = &nodes[0];
        if is_numeric(n) {
            return match parse_index(comp) {
                Some(i) => Some((i, n)),
                None => None,
            };
        }
    }
    let mut i = 0;
    while i < nodes.len()
        invariant
            i <= nodes@.len(),
            !(nodes@.len() == 1 && numeric(nodes@[0])),
        decreases nodes@.len() - i,
    {
        if bytes_is(nodes[i].name.as_bytes(), comp) {
            return Some((i, &nodes[i]));
        }
        i += 1;
    }
    None
}

/// The child of `nodes` at MIB entry `i`.
fn child_by_index(nodes: &'static [Node], i: usize) -> (r: Option<&'static Node>)
    ensures
        r matches Some(n) ==> step(nodes@, i) == Some(*n),
        r is None <==> step(nodes@, i) is None,
{
    // `match nodes { [n] if n.name == NUMERIC => .., _ => .. }`, spelled
    // out (no slice patterns in Verus).
    if nodes.len() == 1 && is_numeric(&nodes[0]) {
        return Some(&nodes[0]);
    }
    nodes.get(i)
}

/// Resolve a dotted name to its MIB. The node reached may be interior
/// (`mallctlnametomib("arenas")` is valid in jemalloc); using it as a key
/// is then `ENOENT`. Writes `mib[..len]` only. The MIB is the name's:
/// its components match the entries one by one ([`walk`]), and
/// [`mib_to_node`] maps it back to the same node
/// ([`theorem_mib_round_trip`]).
fn name_to_mib(name: &[u8], mib: &mut [usize; MAX_MIB]) -> (r: Result<(usize, &'static Node), c_int>)
    ensures
        r matches Ok((len, _)) ==> 1 <= len <= MAX_MIB && forall|j: int|
            len <= j < MAX_MIB ==> #[trigger] final(mib)[j] == old(mib)[j],
        r matches Ok((len, n)) ==> exists|comps: Seq<Seq<u8>>|
            #[trigger] split(name@, comps) && walk(
                root_nodes(),
                comps,
                final(mib)@.subrange(0, len as int),
            ) == Some(*n),
        r matches Ok((len, n)) ==> resolve(root_nodes(), final(mib)@.subrange(0, len as int))
            == Some(*n),
        r matches Err(e) ==> e == ENOENT,
{
    let mut nodes: &'static [Node] = root();
    let mut rest = name;
    let mut len = 0;
    // The components matched so far, and the node the last one named.
    let ghost mut comps: Seq<Seq<u8>> = Seq::empty();
    let ghost mut last: Node = arbitrary();
    loop
        invariant
            len <= MAX_MIB,
            forall|j: int| len <= j < MAX_MIB ==> #[trigger] mib[j] == old(mib)[j],
            comps.len() == len,
            forall|k: int, j: int|
                0 <= k < comps.len() && 0 <= j < comps[k].len() ==> #[trigger] comps[k][j] != 46,
            len == 0 ==> nodes@ == root_nodes() && rest@ == name@,
            len > 0 ==> walk(root_nodes(), comps, mib@.subrange(0, len as int)) == Some(last)
                && nodes@ == spec_children(last) && name@ == join(comps) + seq![46u8] + rest@,
        decreases rest@.len(),
    {
        if len == MAX_MIB {
            return Err(ENOENT);
        }
        let (comp, tail) = split_component(rest);
        let (i, node) = child(nodes, comp).ok_or(ENOENT)?;
        let ghost prefix = mib@.subrange(0, len as int);
        mib[len] = i;
        len += 1;
        proof {
            assert(mib@.subrange(0, len as int) =~= prefix.push(i));
            if len == 1 {
                assert(comps.push(comp@) =~= seq![comp@]);
                assert(walk(root_nodes(), seq![comp@], seq![i]) == Some(*node));
            } else {
                lemma_walk_push(root_nodes(), comps, prefix, last, comp@, i);
                lemma_join_push(comps, comp@);
            }
            assert forall|k: int, j: int|
                0 <= k < comps.push(comp@).len() && 0 <= j < comps.push(comp@)[k].len()
                implies #[trigger] comps.push(comp@)[k][j] != 46 by {
                if k < comps.len() {
                    assert(comps.push(comp@)[k] == comps[k]);
                }
            }
            comps = comps.push(comp@);
            last = *node;
        }
        match tail {
            None => {
                proof {
                    assert(split(name@, comps));
                    lemma_walk_resolves(root_nodes(), comps, mib@.subrange(0, len as int));
                }
                return Ok((len, node));
            },
            Some(t) => {
                let Entry::Dir(children) = node.entry else {
                    return Err(ENOENT);
                };
                nodes = children;
                proof {
                    assert(rest@.len() == comp@.len() + 1 + t@.len());
                    assert(name@ =~= join(comps) + seq![46u8] + t@);
                }
                rest = t;
            },
        }
    }
}

/// The node a MIB names; `ENOENT` for a bad index or an empty MIB. It
/// fails exactly when the MIB resolves to nothing.
fn mib_to_node(mib: &[usize]) -> (r: Result<&'static Node, c_int>)
    ensures
        r matches Ok(n) ==> resolve(root_nodes(), mib@) == Some(*n),
        r is Err <==> resolve(root_nodes(), mib@) is None,
        r matches Err(e) ==> e == ENOENT,
        mib@.len() == 0 ==> r is Err,
{
    let mut nodes: &'static [Node] = root();
    let mut last: Option<&'static Node> = None;
    // `for &i in mib`, spelled out with an index for the invariant.
    let mut k = 0;
    while k < mib.len()
        invariant
            k <= mib@.len(),
            k == 0 ==> last is None && nodes@ == root_nodes(),
            k > 0 ==> (last matches Some(l) && resolve(root_nodes(), mib@.take(k as int))
                == Some(*l) && nodes@ == spec_children(*l)),
        decreases mib@.len() - k,
    {
        proof {
            // A bad entry here leaves the whole MIB unresolved.
            if k > 0 {
                lemma_resolve_split(root_nodes(), mib@, k as int, *(last->Some_0));
                assert(mib@.skip(k as int)[0] == mib@[k as int]);
            }
        }
        let n = child_by_index(nodes, mib[k]).ok_or(ENOENT)?;
        proof {
            if k > 0 {
                let l = *(last->Some_0);
                assert(mib@.take(k + 1).take(k as int) =~= mib@.take(k as int));
                assert(mib@.take(k + 1).skip(k as int) =~= seq![mib@[k as int]]);
                lemma_resolve_split(root_nodes(), mib@.take(k + 1), k as int, l);
            }
        }
        nodes = n.children();
        last = Some(n);
        k += 1;
    }
    proof {
        assert(mib@.take(k as int) =~= mib@);
    }
    last.ok_or(ENOENT)
}

} // verus!

/// The name of the key a MIB names; `ENOENT` for an interior node or a
/// bad index.
#[cfg(test)]
fn mib_to_key(mib: &[usize]) -> Result<&'static str, c_int> {
    let n = mib_to_node(mib)?;
    if let Entry::Dir(_) = n.entry {
        Err(ENOENT)
    } else {
        Ok(n.name)
    }
}

verus! {

/// Byte `n` of the caller's name, through the read permission the call's
/// token gives ([`CCall::str_byte`]): the byte its view names, in the
/// string because no byte before it is the NUL.
///
/// # Safety
/// `p + n` is a byte of the caller's name, at or before its NUL.
#[inline(always)]
unsafe fn name_byte(p: *const u8, n: usize, Tracked(call): Tracked<&CCall>) -> (r: u8)
    requires
        call.reads_str(p),
        call.string().before_nul(n as int),
    ensures
        r == call.string().bytes()[n as int],
{
    proof {
        call.string().lemma_in(n as int);
        crate::vspec::ptr::axiom_ptr_add_addr(p as *mut u8, n);
    }
    // SAFETY: guaranteed by the caller.
    let q = unsafe { p.add(n) };
    let tracked b = call.str_byte(p, q, n as int);
    *ptr_ref(q, Tracked(b))
}

/// The caller's name's first `n` bytes, borrowed for as long as the
/// call's token is: they are before its NUL, so readable
/// ([`crate::vspec::cbuf::CStr::wf`]), and no write through the call's
/// buffers can begin while the slice lives (it would take the token
/// `&mut`). Nothing is claimed about their values: `from_raw_parts`
/// states only the length.
///
/// # Safety
/// The `n` bytes at `p` are readable for the duration of the call.
#[inline(always)]
unsafe fn name_bytes(p: *const u8, n: usize, Tracked(call): Tracked<&CCall>) -> (r: &[u8])
    requires
        p@.addr != 0,
        call.reads_str(p),
        n < call.string().bytes().len(),
        n < MAX_NAME,
    ensures
        r@.len() == n,
{
    proof {
        broadcast use vstd::layout::align_of_u8;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// A C string as bytes, at most [`MAX_NAME`] long. `None` for null or a
/// string of `MAX_NAME` bytes or more.
///
/// `p` is the caller's `name` argument: null, or readable up to and
/// including its NUL terminator (the `mallctl` contract, `CArg::Str`),
/// read through the call's token `call`; at most `MAX_NAME` bytes are
/// ever read, and the slice borrows the caller's memory for as long as
/// the token is borrowed.
fn cstr(p: *const c_char, Tracked(call): Tracked<&CCall>) -> (r: Option<&[u8]>)
    requires
        p@.addr != 0 ==> call.reads_str(p as *const u8),
    ensures
        r matches Some(s) ==> s@.len() < MAX_NAME,
        p@.addr == 0 ==> r is None,
{
    if p.is_null() {
        return None;
    }
    // An `as` cast, not `cast::<u8>()`: Verus specifies it as keeping the
    // provenance, which the token's string view needs (the `cast` spec
    // states only the address). Both compile to nothing; the `allow` is
    // for aarch64, where `c_char` is `u8`.
    #[allow(clippy::unnecessary_cast)]
    let p = p as *const u8;
    let mut n = 0;
    // SAFETY: the caller passes a NUL-terminated name; reads stop at the
    // terminator or the cap.
    while n < MAX_NAME && unsafe { name_byte(p, n, Tracked(call)) } != 0
        invariant
            n <= MAX_NAME,
            call.reads_str(p),
            call.string().before_nul(n as int),
        decreases MAX_NAME - n,
    {
        n += 1;
    }
    if n == MAX_NAME {
        return None;
    }
    proof {
        call.string().lemma_in(n as int);
    }
    // SAFETY: `n` bytes before the terminator were just read.
    Some(unsafe { name_bytes(p, n, Tracked(call)) })
}

} // verus!

// ---------------------------------------------------------------------------
// Value transfer with jemalloc's READ / WRITE / READONLY / WRITEONLY rules.
// ---------------------------------------------------------------------------

verus! {

/// The caller's four value pointers.
#[derive(Copy, Clone)]
pub struct Io {
    pub oldp: *mut c_void,
    pub oldlenp: *mut usize,
    pub newp: *const c_void,
    pub newlen: usize,
}

impl Io {
    /// The `mallctl` caller's contract for the four pointers
    /// (`vspec::cbuf`), with `call` the call's token: with both `oldp` and
    /// `oldlenp` given, they are the call's sized buffer ([`CArg::Sized`]:
    /// `oldlenp` is an initialised `size_t` and `oldp` holds that many
    /// bytes); and the new value is [`Io::given`]. [`mallctl`] and
    /// [`mallctlbymib`] give it for the pointers C passed, through their
    /// call tokens ([`CCall::fresh`]).
    pub open spec fn handed(self, call: CCall) -> bool {
        &&& (self.oldp@.addr != 0 && self.oldlenp@.addr != 0 ==> call.has(CArg::Sized(self.oldp as *mut u8, self.oldlenp as *const usize)))
        &&& self.given()
    }

    /// A given `newp` holds `newlen` readable bytes ([`c_readable`]).
    pub open spec fn given(self) -> bool {
        self.newp@.addr != 0 ==> c_readable(self.newp as *mut u8, self.newlen as int)
    }

    /// A read of a `T` through the call `call` succeeds: the caller asked
    /// for nothing, or gave `T`'s size as `*oldlenp` ([`CCall::len`]).
    pub open spec fn reads_ok<T>(self, call: CCall) -> bool {
        self.oldp@.addr == 0 || self.oldlenp@.addr == 0 || call.len(self.oldlenp as *const usize) == vstd::layout::size_of::<T>()
    }
}

/// `*oldlenp`, through the read permission the call's token gives
/// ([`CCall::sized`]), which also makes `oldp` a buffer of that many
/// bytes. Before any write through the call's buffers.
///
/// # Safety
/// `oldlenp` is the caller's out-pointer per the `mallctl` contract.
#[inline(always)]
unsafe fn old_len(oldlenp: *mut usize, Ghost(oldp): Ghost<*mut u8>, Tracked(call): Tracked<&CCall>) -> (r: usize)
    requires
        !call.written(),
        call.has(CArg::Sized(oldp, oldlenp as *const usize)),
    ensures
        c_buffer(oldp, r as int),
        r == call.len(oldlenp as *const usize),
{
    let tracked len = call.sized(oldp, oldlenp as *const usize);
    // SAFETY: guaranteed by the caller.
    *ptr_ref(oldlenp, Tracked(len))
}

/// Copy the first `n` bytes of `v` to `oldp`: a raw copy into the
/// caller's buffer, whose byte length the caller gave as
/// `*oldlenp == oldlen` ([`c_buffer`], from [`old_len`]), once its writes
/// are open ([`c_writes`]). The precondition keeps the copy inside `v`
/// ([`ref_ptr`]) and inside those `oldlen` bytes. Nothing is copied if
/// the two overlap, which they never do for a caller that keeps the
/// `mallctl` contract (its buffer is the program's memory, `v` a value on
/// challoc's stack); the call fails with `EINVAL` either way.
///
/// # Safety
/// `oldp` holds `oldlen` writable bytes.
#[inline(always)]
unsafe fn copy_prefix<T: Copy>(v: &T, oldp: *mut c_void, n: usize, Ghost(oldlen): Ghost<usize>)
    requires
        n <= size_of::<T>(),
        n <= oldlen,
        c_buffer(oldp as *mut u8, oldlen as int),
        c_writes(oldp@.provenance),
{
    let src = ref_ptr(v) as *const u8;
    let (s, d) = (src as usize, oldp as usize);
    if s + n <= d || d + n <= s {
        proof {
            broadcast use vstd::layout::align_of_u8;
            lemma_c_buffer_at(oldp as *mut u8, oldlen as int, oldp as *mut u8, 0);
        }
        // SAFETY: `n` bytes of `v`, readable, into the `oldlen` writable
        // bytes at `oldp`, apart.
        unsafe { ptr::copy_nonoverlapping(src, oldp as *mut u8, n) };
    }
}

/// Store `v` at `oldp`: an unaligned write of the `size_of::<T>()` bytes
/// the caller's buffer holds ([`c_buffer`], `*oldlenp == oldlen`), once
/// its writes are open ([`c_writes`]).
///
/// # Safety
/// `oldp` holds `oldlen` writable bytes.
#[inline(always)]
unsafe fn store<T: Copy>(oldp: *mut c_void, v: T, Ghost(oldlen): Ghost<usize>)
    requires
        oldlen as int == vstd::layout::size_of::<T>(),
        c_buffer(oldp as *mut u8, oldlen as int),
        c_writes(oldp@.provenance),
{
    proof {
        broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
        lemma_c_buffer_at(oldp as *mut u8, oldlen as int, oldp as *mut T, 0);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::write_unaligned(oldp as *mut T, v) };
}

/// The value at `newp`: an unaligned read of the `size_of::<T>()` bytes
/// the caller's buffer holds ([`c_readable`], `newlen`), of a `T` valid
/// for any initialised bytes (`vspec::ptr::plain`); the value is not
/// stated.
///
/// # Safety
/// `newp` holds `newlen` readable bytes.
#[inline(always)]
unsafe fn load<T: Copy>(newp: *const c_void, Ghost(newlen): Ghost<usize>) -> T
    requires
        crate::vspec::ptr::plain::<T>(),
        newlen as int == vstd::layout::size_of::<T>(),
        c_readable(newp as *mut u8, newlen as int),
{
    proof {
        broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
        lemma_c_readable_at(newp as *mut u8, newlen as int, newp as *mut T, 0);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::read_unaligned(newp as *const T) }
}

impl Io {
    /// Copy `v` out if the caller asked for it, through the call's token
    /// `call`, before any write through its buffers. A wrong `*oldlenp`
    /// copies the common prefix and fails with `EINVAL`.
    fn read<T: Copy>(&self, v: T, Tracked(call): Tracked<&mut CCall>) -> (r: Result<(), c_int>)
        requires
            self.handed(*old(call)),
            !old(call).written(),
        ensures
            r matches Err(e) ==> e == EINVAL,
            self.oldp@.addr == 0 || self.oldlenp@.addr == 0 ==> r is Ok,
            final(call).entry() == old(call).entry(),
            // It fails exactly when the caller asked for a value with a
            // `*oldlenp` that is not `T`'s size, as `verify_read` says
            // beforehand; asked for, the value is copied (the token is
            // written) and no more than `*oldlenp` bytes (`copy_prefix`,
            // `store`).
            r is Ok <==> self.reads_ok::<T>(*old(call)),
            self.oldp@.addr != 0 && self.oldlenp@.addr != 0 ==> final(call).written(),
    {
        if self.oldp.is_null() || self.oldlenp.is_null() {
            return Ok(());
        }
        let want = size_of::<T>();
        let ghost oldp = self.oldp as *mut u8;
        // SAFETY: `oldp`/`oldlenp` are the caller's out-pointers per the
        // `mallctl` contract; `*oldlenp` bytes are writable at `oldp`.
        unsafe {
            let have = old_len(self.oldlenp, Ghost(oldp), Tracked(&*call));
            proof {
                call.open_writes(CArg::Sized(oldp, self.oldlenp as *const usize));
            }
            if have != want {
                copy_prefix(&v, self.oldp, have.min(want), Ghost(have));
                return Err(EINVAL);
            }
            store(self.oldp, v, Ghost(have));
        }
        Ok(())
    }

    /// The new value, if the caller passed one; `EINVAL` on a size mismatch.
    /// `T` is valid for any initialised bytes (`vspec::ptr::plain`).
    fn write<T: Copy>(&self) -> (r: Result<Option<T>, c_int>)
        requires
            crate::vspec::ptr::plain::<T>(),
            self.given(),
        ensures
            r matches Err(e) ==> e == EINVAL,
            self.newp@.addr == 0 <==> r == Ok::<Option<T>, c_int>(None),
            r matches Ok(Some(_)) ==> self.newlen == size_of::<T>(),
    {
        if self.newp.is_null() {
            return Ok(None);
        }
        if self.newlen != size_of::<T>() {
            return Err(EINVAL);
        }
        // SAFETY: `newlen == size_of::<T>()` readable bytes at `newp`.
        Ok(Some(unsafe { load(self.newp, Ghost(self.newlen)) }))
    }

    /// A C `bool`: any non-zero byte is true.
    fn write_bool(&self) -> (r: Result<Option<bool>, c_int>)
        requires
            self.given(),
        ensures
            r matches Err(e) ==> e == EINVAL,
            self.newp@.addr == 0 <==> r == Ok::<Option<bool>, c_int>(None),
    {
        proof {
            crate::vspec::ptr::axiom_plain_u8();
        }
        Ok(self.write::<u8>()?.map(|b: u8| -> (r: bool)
            ensures
                r == (b != 0),
            { b != 0 }))
    }

    /// `EINVAL`, copying nothing, if the caller asked for a value and
    /// `*oldlenp` is not `size_of::<T>()`: the size check of jemalloc's
    /// `VERIFY_READ`, for keys that must not act on a read that will fail.
    /// Unlike `VERIFY_READ`, a null `oldp` passes (the caller asked for
    /// nothing) and `*oldlenp` is left as it is. `*oldlenp` is read through
    /// the call's token `call`, before any write through its buffers.
    fn verify_read<T: Copy>(&self, Tracked(call): Tracked<&CCall>) -> (r: Result<(), c_int>)
        requires
            self.handed(*call),
            !call.written(),
        ensures
            r matches Err(e) ==> e == EINVAL,
            r is Ok <==> self.reads_ok::<T>(*call),
    {
        if self.oldp.is_null() || self.oldlenp.is_null() {
            return Ok(());
        }
        let ghost oldp = self.oldp as *mut u8;
        // SAFETY: `oldlenp` is the caller's out-pointer per the `mallctl`
        // contract.
        if unsafe { old_len(self.oldlenp, Ghost(oldp), Tracked(call)) } != size_of::<T>() {
            return Err(EINVAL);
        }
        Ok(())
    }

    fn read_only(&self) -> (r: Result<(), c_int>)
        ensures
            r is Ok <==> self.newp@.addr == 0 && self.newlen == 0,
            r matches Err(e) ==> e == EPERM,
    {
        if self.newp.is_null() && self.newlen == 0 {
            Ok(())
        } else {
            Err(EPERM)
        }
    }

    fn write_only(&self) -> (r: Result<(), c_int>)
        ensures
            r is Ok <==> self.oldp@.addr == 0 && self.oldlenp@.addr == 0,
            r matches Err(e) ==> e == EPERM,
    {
        if self.oldp.is_null() && self.oldlenp.is_null() {
            Ok(())
        } else {
            Err(EPERM)
        }
    }

    fn void(&self) -> (r: Result<(), c_int>)
        ensures
            r matches Err(e) ==> e == EPERM,
    {
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
    exec const ZERO: Utilization
        ensures
            triple_is(Self::ZERO, 0, 0, 0),
            no_bin(Self::ZERO),
    {
        Utilization {
            slabcur_addr: ptr::null_mut(),
            nfree: 0,
            nregs: 0,
            size: 0,
            bin_nfree: 0,
            bin_nregs: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// The utilization and defrag hint as a function of what the query read.
// The figures are atomic loads Verus does not constrain, so the statement
// is over the values the call read: each output is the stated function of
// `p`'s classification and of a record of those reads. The record's
// `allocated`, `committed`, `live`, `found` and `run` are not tied to the
// loads (`desc_if_committed` and the bitmap read have no postcondition),
// so the statement fixes the outputs only once those are given.
// ---------------------------------------------------------------------------

/// `rg.classify(p) == k` as far as [`Regions::classify`]'s postcondition
/// fixes it: a slot start of a class region (which, with separated
/// regions, is `Slab` of its class, slab and slot), a page start of the
/// huge region with its page index, and `Invalid` for anything that is
/// neither.
///
/// [`Regions::classify`]: crate::layout::Regions::classify
pub open spec fn classified(rg: crate::layout::Regions, p: usize, k: Kind) -> bool {
    &&& k matches Kind::Slab { class, slab, slot } ==> class < NCLASSES && slot < class_table()[class as int].nslots
        && rg.is_slot_addr(class as int, p) && p as int == rg.slot_at(class as int, slab as int, slot as int)
        && (slab as int) < rg.spec_nslabs(class as int)
    &&& k matches Kind::Huge { page } ==> rg.is_huge_block(p, 1) && page == ((p - rg.huge_base) as usize) >> rg.lg_page
    &&& rg.separated() ==> forall|c: int| #[trigger] rg.is_slot_addr(c, p) ==> (k matches Kind::Slab { class, .. } && class as int == c)
}

/// What a utilization query read (ghost): `p`'s classification, and the
/// value each load returned. `allocated` is the slot's bitmap bit
/// (`desc_if_committed`), `live` the slab's live count as `nfree` used
/// it (`nslots - nfree`: the load, clamped at `nslots`), `found` that the
/// arena the descriptor names is built, `st` and `curregs` that arena's
/// class counters from one lock hold (`bin_stats`), `lowest` its lowest
/// partial slab, `closed` the slots its slab's closed pages hide
/// (docs/proposals/slab-pages.md, 3.6; 0 when they would hide every slot,
/// so `nregs` is never 0), and `run` the huge descriptor at `p`'s page
/// (`Huge::lookup`, under the huge lock).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct UtilRead {
    pub kind: Kind,
    pub allocated: bool,
    pub committed: bool,
    pub live: int,
    pub found: bool,
    pub st: crate::class::ClassStats,
    pub curregs: int,
    pub lowest: Option<usize>,
    pub run: Option<crate::huge::RunInfo>,
    pub closed: int,
}

/// `a - b`, or 0 below zero (`saturating_sub`).
pub open spec fn sat_sub(a: int, b: int) -> int {
    if a >= b { a - b } else { 0 }
}

/// `u`'s `(nfree, nregs, size)` triple, the one `batch_query` stores.
pub open spec fn triple_is(u: Utilization, nfree: int, nregs: int, size: int) -> bool {
    u.nfree == nfree && u.nregs == nregs && u.size == size
}

/// No bin-wide figures and no current slab (the non-verbose query, and
/// everything that is not a live slot of a bin class).
pub open spec fn no_bin(u: Utilization) -> bool {
    u.bin_nfree == 0 && u.bin_nregs == 0 && u.slabcur_addr@.addr == 0
}

/// The figures a query of `p` reports from the reads `rd`: for a live
/// slot of a bin class (bit set, slab committed), `(nslots - live,
/// nslots, nslots * size)`, plus, verbose and with the owning arena
/// built, `curslabs * nslots` regions of which `curregs` are out and the
/// lowest partial slab's address (null when there is none); `(0, 1,
/// size)` for a live slot of a class above `BIN_MAX` (the classes from
/// `NBINS` on); `(0, 1, usable)` for the start of an active huge run;
/// zeros for everything else.
#[verifier::opaque]
pub open spec fn util_from(rg: crate::layout::Regions, p: usize, verbose: bool, rd: UtilRead, u: Utilization) -> bool {
    match rd.kind {
        Kind::Slab { class, .. } => {
            let c = class_table()[class as int];
            if !rd.allocated || !rd.committed {
                triple_is(u, 0, 0, 0) && no_bin(u)
            } else if c.size > crate::layout::BIN_MAX {
                triple_is(u, 0, 1, c.size as int) && no_bin(u)
            } else {
                let curslabs = rd.st.committed - rd.st.empty - rd.st.purged;
                // Slab pages (docs/proposals/slab-pages.md, 3.6): the slab's
                // open slots, and the bin's.
                let nregs = c.nslots - rd.closed;
                &&& triple_is(u, sat_sub(nregs, rd.live), nregs, nregs * c.size)
                &&& if verbose && rd.found {
                    &&& u.bin_nregs == curslabs * c.nslots - rd.st.closed_slots
                    &&& u.bin_nfree == u.bin_nregs - rd.curregs
                    &&& u.slabcur_addr@.addr == match rd.lowest {
                        Some(s) => rg.class_base[class as int] + (s << c.lg_slab),
                        None => 0,
                    }
                } else {
                    no_bin(u)
                }
            }
        },
        Kind::Huge { page } => match rd.run {
            Some(r) if r.start == page && r.state == RunState::Active => triple_is(u, 0, 1, r.usable as int) && no_bin(u),
            _ => triple_is(u, 0, 0, 0) && no_bin(u),
        },
        Kind::Invalid => triple_is(u, 0, 0, 0) && no_bin(u),
    }
}

/// `rd` is a possible read of a query of `p` that reported `u`: `rd.kind`
/// is `p`'s classification, the counters are within what `bin_stats`
/// returns (no more slots out than the live-capable slabs hold), the
/// lowest partial slab is a slab of the class, and `u` is
/// [`util_from`]`(rd)`.
pub open spec fn util_read(rg: crate::layout::Regions, p: usize, verbose: bool, rd: UtilRead, u: Utilization) -> bool {
    &&& classified(rg, p, rd.kind)
    &&& rd.st.empty + rd.st.purged <= rd.st.committed
    &&& 0 <= rd.curregs <= (rd.st.committed - rd.st.empty - rd.st.purged) * (match rd.kind {
        Kind::Slab { class, .. } => class_table()[class as int].nslots as int,
        _ => 0,
    })
    &&& (rd.lowest matches Some(s) ==> rd.kind matches Kind::Slab { class, .. } && (s as int) < rg.spec_nslabs(class as int))
    &&& rd.curregs + rd.st.closed_slots <= (rd.st.committed - rd.st.empty - rd.st.purged) * (match rd.kind {
        Kind::Slab { class, .. } => class_table()[class as int].nslots as int,
        _ => 0,
    })
    &&& 0 <= rd.closed < (match rd.kind {
        Kind::Slab { class, .. } => class_table()[class as int].nslots as int,
        _ => 1,
    })
    &&& util_from(rg, p, verbose, rd, u)
}

/// The utilisation figures of `p` (see the module docs). `verbose` adds
/// the bin-wide figures and `slabcur_addr`, which cost one class lock.
pub fn utilization(shared: &Shared, p: *const c_void, verbose: bool) -> (u: Utilization)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        // The outputs as a function of a record of the call's reads
        // ([`util_from`]).
        exists|rd: UtilRead| #[trigger] util_read(crate::layout::process_regions(), p as usize, verbose, rd, u),
{
    proof {
        shared.lemma_wf();
        shared.lemma_wf_ready();
        reveal(util_from);
    }
    // Premise (lock order): called from the `mallctl` entry points, which
    // hold no lock.
    let tracked mut ord = axiom_entry();
    let mut u = Utilization::ZERO;
    let ghost mut rd = UtilRead {
        kind: Kind::Invalid,
        allocated: false,
        committed: false,
        live: 0,
        found: false,
        st: crate::class::ClassStats { committed: 0, guards: 0, empty: 0, purged: 0, nonfull: 0, closed_slots: 0, closed_pages: 0, closable_slots: 0 },
        curregs: 0,
        lowest: None,
        run: None,
        closed: 0,
    };
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            proof {
                rd.kind = Kind::Slab { class, slab, slot };
                crate::vspec::class::lemma_class_table_wf(class as int);
            }
            let class = class as usize;
            let t = shared.table(class);
            let cls = t.class();
            if !t.is_allocated(slab as usize, slot as usize) {
                proof {
                    assert(util_read(crate::layout::process_regions(), p as usize, verbose, rd, u));
                }
                return u;
            }
            proof {
                rd.allocated = true;
            }
            let Some(d) = t.desc_if_committed(slab as usize) else {
                proof {
                    assert(util_read(crate::layout::process_regions(), p as usize, verbose, rd, u));
                }
                return u;
            };
            proof {
                rd.committed = true;
            }
            if class >= NBINS {
                // Reported like a large extent (`layout::BIN_MAX`): one
                // region of its own size, no bin figures.
                u.nregs = 1;
                u.size = cls.size();
                proof {
                    assert(util_read(crate::layout::process_regions(), p as usize, verbose, rd, u));
                }
                return u;
            }
            let nfree = d.nfree(cls);
            proof {
                // `nfree` is `nslots` less the live count it loaded.
                rd.live = cls.nslots - nfree;
            }
            // Slab pages (docs/proposals/slab-pages.md, 3.6): the slab's
            // regions are its open slots, its free ones the open free
            // ones. A closed mask that would hide every slot reads as none
            // (the queried slot is live, so its pages are open), so that
            // `nregs` is never 0: Valkey divides by it.
            let closed = match t.pages_if_committed(slab as usize) {
                Some(pg) => t.page_view().with_closed(pg.closed()).closed_slots(),
                None => 0,
            };
            let closed = if closed >= cls.nslots() { 0 } else { closed };
            proof {
                rd.closed = closed as int;
            }
            u.nregs = cls.nslots() - closed;
            u.nfree = nfree.saturating_sub(closed);
            // The region bytes of the slab's open slots, not its
            // power-of-two size: the tail past the last slot belongs to no
            // region, and Valkey's allocatorShouldDefrag divides `size` by
            // `nregs` and asserts that the quotient is a bin size
            // (jemalloc's slabs are exact multiples of their region size).
            proof {
                assert(u.nregs * cls.size <= cls.nslots * cls.size) by (nonlinear_arith)
                    requires u.nregs <= cls.nslots;
                assert(cls.nslots * cls.size <= usize::MAX) by {
                    crate::vspec::class::lemma_class_table_wf(class as int);
                }
            }
            u.size = u.nregs * cls.size();
            // Two `if`s, not a let chain: Verus takes no let chain.
            #[allow(clippy::collapsible_if)]
            if verbose {
                if let Some(a) = shared.arenas.get_existing(d.arena()) {
                    let cs = a.class(class);
                    let (st, curregs) = cs.bin_stats(Tracked(&mut ord));
                    proof {
                        let (c, k, n) = ((st.committed - st.empty - st.purged) as int, st.committed as int, cls.nslots as int);
                        assert(c * n <= k * n) by (nonlinear_arith)
                            requires
                                c <= k,
                                n >= 0,
                        ;
                        rd.found = true;
                        rd.st = st;
                        rd.curregs = curregs as int;
                    }
                    u.bin_nregs = st.curslabs() * cls.nslots() - st.closed_slots;
                    u.bin_nfree = u.bin_nregs.saturating_sub(curregs);
                    if let Some(s) = cs.lowest_partial(Tracked(&mut ord)) {
                        // No code (the body is empty): a step for the
                        // verifier, that the slab is one of the class
                        // region's.
                        cs.nslabs_are_the_region_s();
                        u.slabcur_addr = addr_to_ptr(shared.regions.slab_addr(class, s)) as *mut c_void;
                        proof {
                            rd.lowest = Some(s);
                        }
                    }
                }
            }
        },
        Kind::Huge { page } => {
            proof {
                rd.kind = Kind::Huge { page };
            }
            // As above, two `if`s.
            #[allow(clippy::collapsible_if)]
            if let Some(r) = shared.huge.lookup(page, Tracked(&None), Tracked(&mut ord)) {
                proof {
                    rd.run = Some(r);
                }
                if r.start == page && r.state == RunState::Active {
                    u.nregs = 1;
                    u.size = r.usable;
                }
            }
        },
        Kind::Invalid => {},
    }
    proof {
        assert(util_read(crate::layout::process_regions(), p as usize, verbose, rd, u));
    }
    u
}

/// The arena that owns the live allocation `p` (`arenas.lookup`), or
/// `None` for anything else. Its one abort is `class::SlabTable::owner`'s,
/// for a slab that was never handed out, a pending check, so it needs
/// only `may_abort_pending`.
pub fn owner_of(shared: &Shared, p: *const c_void) -> Option<u16>
    requires
        crate::sys::may_abort_pending(),
        crate::sys::may_syscall(),
        shared.wf(),
{
    proof {
        shared.lemma_wf();
        shared.lemma_wf_ready();
    }
    // Premise (lock order): called from the `mallctl` entry points, which
    // hold no lock.
    let tracked mut ord = axiom_entry();
    match shared.regions.classify(p as usize) {
        Kind::Slab { class, slab, slot } => {
            let t = shared.table(class as usize);
            if t.is_allocated(slab as usize, slot as usize) {
                Some(t.owner(slab as usize).0)
            } else {
                None
            }
        },
        Kind::Huge { page } => {
            let r = shared.huge.lookup(page, Tracked(&None), Tracked(&mut ord))?;
            if r.start == page && r.state == RunState::Active {
                Some(r.arena)
            } else {
                None
            }
        },
        Kind::Invalid => None,
    }
}

/// `(nfree, nregs, size)` is the triple a non-verbose utilization query of
/// `p` reports for some possible read ([`util_read`]).
pub open spec fn util_triple(rg: crate::layout::Regions, p: usize, nfree: int, nregs: int, size: int) -> bool {
    exists|rd: UtilRead, u: Utilization| #[trigger] util_read(rg, p, false, rd, u) && triple_is(u, nfree, nregs, size)
}

/// Pointer `i` of the caller's `newp` array, whose byte length the caller
/// gave as `newlen`: an unaligned read of the 8 bytes at `ptrs + 8 * i`,
/// which the precondition puts inside those `newlen` bytes ([`c_readable`]).
///
/// # Safety
/// `ptrs` holds `newlen` readable bytes.
#[inline(always)]
unsafe fn query_ptr(ptrs: *const *const c_void, i: usize, Ghost(newlen): Ghost<usize>) -> *const c_void
    requires
        (i + 1) * 8 <= newlen,
        c_readable(ptrs as *mut u8, newlen as int),
{
    proof {
        broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
        broadcast use vstd::layout::layout_of_references_and_pointers, vstd::layout::layout_of_references_and_pointers_for_sized_types;
        let base = ptrs as *mut *const c_void;
        lemma_c_readable_at(ptrs as *mut u8, newlen as int, base, 0);
        assert(vstd::layout::size_of::<*const c_void>() == 8);
        assert(i * vstd::layout::size_of::<*const c_void>() <= newlen);
        assert(crate::vspec::ptr::in_alloc(base, newlen as int));
        crate::vspec::ptr::axiom_ptr_add_addr(base, i);
        lemma_c_readable_at(ptrs as *mut u8, newlen as int, crate::vspec::ptr::ptr_add(base, i), 8 * i);
        crate::vspec::ptr::axiom_plain_ptr::<c_void>();
    }
    // SAFETY: guaranteed by the caller.
    unsafe { ptrs.add(i).read_unaligned() }
}

/// Store triple `i`, `(nfree, nregs, size)`, into the caller's `oldp`
/// array, whose byte length the caller gave as `*oldlenp == oldlen`:
/// unaligned writes of words `3 * i .. 3 * i + 3`, which the precondition
/// puts inside those `oldlen` bytes ([`c_buffer`]).
///
/// # Safety
/// `out` holds `oldlen` writable bytes.
#[inline(always)]
unsafe fn store_triple(
    out: *mut usize,
    i: usize,
    nfree: usize,
    nregs: usize,
    size: usize,
    Ghost(oldlen): Ghost<usize>,
    Ghost(q): Ghost<*const c_void>,
)
    requires
        (3 * i + 3) * 8 <= oldlen,
        // What is stored is the non-verbose query of `q`, the pointer
        // `query_ptr` read for index `i`.
        util_triple(crate::layout::process_regions(), q as usize, nfree as int, nregs as int, size as int),
        c_buffer(out as *mut u8, oldlen as int),
        c_writes(out@.provenance),
{
    proof {
        broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
        lemma_c_buffer_at(out as *mut u8, oldlen as int, out, 0);
        let (k0, k1, k2) = ((3 * i) as usize, (3 * i + 1) as usize, (3 * i + 2) as usize);
        crate::vspec::ptr::axiom_ptr_add_addr(out, k0);
        lemma_c_buffer_at(out as *mut u8, oldlen as int, crate::vspec::ptr::ptr_add(out, k0), 8 * k0);
        crate::vspec::ptr::axiom_ptr_add_addr(out, k1);
        lemma_c_buffer_at(out as *mut u8, oldlen as int, crate::vspec::ptr::ptr_add(out, k1), 8 * k1);
        crate::vspec::ptr::axiom_ptr_add_addr(out, k2);
        lemma_c_buffer_at(out as *mut u8, oldlen as int, crate::vspec::ptr::ptr_add(out, k2), 8 * k2);
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        out.add(3 * i).write_unaligned(nfree);
        out.add(3 * i + 1).write_unaligned(nregs);
        out.add(3 * i + 2).write_unaligned(size);
    }
}

/// `experimental.utilization.batch_query`: `newp` is an array of pointers,
/// `oldp` an array of `(nfree, nregs, size)` triples of the same count.
/// Only an exact fit is accepted: `newlen` is `8 * n` and `*oldlenp` is
/// `24 * n` for some `n > 0`, so every read and write of the loop stays
/// inside the caller's buffers. `call` is the call's token, before any
/// write through its buffers.
fn batch_query(shared: &Shared, io: &Io, Tracked(call): Tracked<&mut CCall>) -> Result<(), c_int>
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        io.handed(*old(call)),
        !old(call).written(),
{
    if io.oldp.is_null() || io.oldlenp.is_null() || io.newp.is_null() {
        return Err(EINVAL);
    }
    let psz = size_of::<*const c_void>();
    let tsz = 3 * size_of::<usize>();
    let ghost oldp = io.oldp as *mut u8;
    // SAFETY: `oldlenp` is the caller's length pointer.
    let oldlen = unsafe { old_len(io.oldlenp, Ghost(oldp), Tracked(&*call)) };
    proof {
        call.open_writes(CArg::Sized(oldp, io.oldlenp as *const usize));
    }
    let n = io.newlen / psz;
    // `checked_mul`: a count whose triples overflow must not wrap onto
    // the caller's `oldlen` and send the loop past both arrays.
    if n == 0 || !io.newlen.is_multiple_of(psz) || n.checked_mul(tsz) != Some(oldlen) {
        return Err(EINVAL);
    }
    let ptrs = io.newp as *const *const c_void;
    let out = io.oldp as *mut usize;
    proof {
        broadcast use vstd::raw_ptr::axiom_ptr_mut_from_data;
        assert(c_readable(ptrs as *mut u8, io.newlen as int));
        assert(c_buffer(out as *mut u8, oldlen as int));
    }
    for i in 0..n
        invariant
            crate::sys::may_syscall(),
            shared.wf(),
            n * 8 <= io.newlen,
            n * 24 == oldlen,
            c_buffer(out as *mut u8, oldlen as int),
            c_writes(out@.provenance),
            c_readable(ptrs as *mut u8, io.newlen as int),
    {
        proof {
            assert((i + 1) * 8 <= n * 8) by (nonlinear_arith)
                requires
                    i < n,
            ;
            assert((3 * i + 3) * 8 <= n * 24) by (nonlinear_arith)
                requires
                    i < n,
            ;
        }
        // SAFETY: `n` pointers are readable at `newp` and `3 * n` words
        // writable at `oldp`, both checked against the caller's lengths.
        unsafe {
            let q = query_ptr(ptrs, i, Ghost(io.newlen));
            let u = utilization(shared, q, false);
            store_triple(out, i, u.nfree, u.nregs, u.size, Ghost(oldlen), Ghost(q));
        }
    }
    Ok(())
}

fn arena_index(i: usize) -> (r: Result<u16, c_int>)
    ensures
        r matches Ok(a) ==> a == i && i < MAX_ARENAS,
        r matches Err(e) ==> e == ENOENT && i >= MAX_ARENAS,
{
    if i < MAX_ARENAS {
        Ok(i as u16)
    } else {
        Err(ENOENT)
    }
}

/// `mib[i]`, or an index that matches nothing when the MIB is shorter.
fn at(mib: &[usize], i: usize) -> (r: usize)
    ensures
        r == if i < mib@.len() { mib@[i as int] } else { usize::MAX },
{
    mib.get(i).copied().unwrap_or(usize::MAX)
}

/// `opt.<key>`: the key's value with its jemalloc type, the
/// `config::Value` variant's.
fn opt_value(mib: &[usize], Tracked(mark): Tracked<&'static ReadyMark>) -> Result<Reading, c_int>
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
{
    let key = KEYS.get(at(mib, 1)).ok_or(ENOENT)?;
    Ok(match shared(Tracked(mark)).config.get(key.as_bytes()).ok_or(ENOENT)? {
        Value::Bool(b) => Bool(b),
        Value::Int(i) => Ssize(i as isize),
        Value::Uint(u) => Unsigned(u as c_uint),
        Value::Size(n) => Size(n),
        Value::Str(s) => Str(s.cstr().as_ptr()),
    })
}

/// `arenas.bin.<j>.*`: the bin's class; `ENOENT` past `arenas.nbins`.
// `CLASSES[..NBINS].get(j)`, spelled with `CLASSES.table()`, the verified
// view of the table.
// `j < NCLASSES` is redundant for the compiler, not for the verifier, which
// cannot see `NBINS`'s value.
#[allow(clippy::redundant_comparisons)]
fn bin_class(mib: &[usize]) -> (r: Result<crate::layout::Class, c_int>)
    ensures
        r matches Err(e) ==> e == ENOENT,
        r matches Ok(c) ==> c.wf(),
{
    let j = at(mib, 2);
    if j < NBINS && j < NCLASSES {
        proof {
            crate::vspec::class::lemma_class_table_wf(j as int);
        }
        Ok(CLASSES.table()[j])
    } else {
        Err(ENOENT)
    }
}

/// The tracked cgroup, or `ENOENT`: like jemalloc's keys behind a disabled
/// build option, the name resolves and the read says the facility is off.
fn tracked(Tracked(mark): Tracked<&'static ReadyMark>) -> (r: Result<&'static crate::sys::cgroup::Cgroup, c_int>)
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
    ensures
        r matches Err(e) ==> e == ENOENT,
{
    let s = shared(Tracked(mark));
    // Premise (lock order): called from `mallctl`, which holds no lock.
    let cg = s.cgroup_now(Tracked(&mut axiom_entry()));
    if cg.tracked() { Ok(cg) } else { Err(ENOENT) }
}

/// `arena.<i>.purge` and `arena.<i>.decay` address every arena when `i` is
/// `MALLCTL_ARENAS_ALL` or, jemalloc's pre-5.0 convention that it still
/// honours, `arenas.narenas`. Redis and Valkey (`jemalloc_purge`, behind
/// `MEMORY PURGE` and `FLUSHALL`) use the latter.
fn all_arenas(shared: &Shared, i: usize) -> (r: bool)
    ensures
        i == ARENAS_ALL ==> r,
{
    i == ARENAS_ALL as usize || i == shared.arenas.count() as usize
}

/// The figure of an arena's stored statistics a `stats.arenas.<i>.*`
/// reader returns ([`arena_figure`]).
enum Fig {
    SmallAllocated,
    SmallResident,
    LargeAllocated,
    Pactive,
    Pdirty,
    Pmuzzy,
    Resident,
    Mapped,
    Retained,
    /// `bins.<j>.curregs`, `j` the MIB's fifth component.
    Curregs,
    Curslabs,
    NonfullSlabs,
}

/// `fig` of arena `m[2]`'s stored statistics ([`arena_stats_into`]),
/// read from a [`stats::Scratch`] taken before any lock and given back
/// before the return, so the 6 KiB record is not on the caller's stack;
/// `EAGAIN` when there is none to take. A bin figure is `ENOENT` past
/// `arenas.nbins` (`bins[..NBINS].get(j)`, spelled out as in
/// `bin_class`).
// `j < NCLASSES` is redundant for the compiler, not for the verifier.
#[allow(clippy::redundant_comparisons)]
fn arena_figure(m: &[usize], fig: Fig, Tracked(mark): Tracked<&'static ReadyMark>) -> (r: Result<Reading, c_int>)
    requires
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
    ensures
        r matches Err(e) ==> e == ENOENT || e == EAGAIN,
{
    let shared = shared(Tracked(mark));
    // Premise (lock order): a reader under the `mallctl` entry points,
    // which hold no lock; for the scratch record's metadata lock.
    let tracked mut ord = axiom_entry();
    let Some(mut sc) = stats::Scratch::new(&shared.meta, Tracked(&mut ord)) else {
        return Err(EAGAIN);
    };
    let r = match arena_stats_into(shared, at(m, 2), &mut sc.record().a) {
        Err(e) => Err(e),
        Ok(()) => {
            let a = &sc.get().a;
            let j = at(m, 4);
            let bin = if j < NBINS && j < NCLASSES { Some(&a.bins[j]) } else { None };
            match fig {
                Fig::SmallAllocated => Ok(Size(a.small_allocated)),
                // `+` as the build computes it ([`stats::fig_add`]).
                Fig::SmallResident => Ok(Size(stats::fig_add(a.small_active, a.small_dirty))),
                Fig::LargeAllocated => Ok(Size(a.large_allocated)),
                Fig::Pactive => Ok(Size(a.pactive)),
                Fig::Pdirty => Ok(Size(a.pdirty)),
                Fig::Pmuzzy => Ok(Size(a.pmuzzy)),
                Fig::Resident => Ok(Size(a.resident)),
                Fig::Mapped => Ok(Size(a.mapped)),
                Fig::Retained => Ok(Size(a.retained)),
                Fig::Curregs => match bin {
                    // Slab pages (docs/proposals/slab-pages.md, 3.6): with the
                    // closed slots, so `nregs * curslabs - curregs` counts only
                    // open free slots (Redis's and Valkey's fragmentation).
                    Some(b) => Ok(Size(stats::fig_add(b.curregs, b.closed_slots))),
                    None => Err(ENOENT),
                },
                Fig::Curslabs => match bin {
                    Some(b) => Ok(Size(b.curslabs)),
                    None => Err(ENOENT),
                },
                Fig::NonfullSlabs => match bin {
                    Some(b) => Ok(Size(b.nonfull_slabs)),
                    None => Err(ENOENT),
                },
            }
        },
    };
    sc.free(&shared.meta, Tracked(&mut ord));
    r
}

/// The stored statistics of arena `i`, the `MALLCTL_ARENAS_ALL` merge, or
/// the zeros of `MALLCTL_ARENAS_DESTROYED` (both recognised before the
/// range check, which they fail), into `out`.
fn arena_stats_into(shared: &Shared, i: usize, out: &mut ArenaStats) -> (r: Result<(), c_int>)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        r matches Err(e) ==> e == ENOENT,
{
    if i == ARENAS_ALL as usize {
        stats::arena_stats_all_into(shared, out, Tracked(&mut axiom_entry()));
        return Ok(());
    }
    if i == ARENAS_DESTROYED {
        out.clear(ARENAS_DESTROYED as u16);
        return Ok(());
    }
    if stats::arena_stats_into(shared, arena_index(i)?, out, Tracked(&mut axiom_entry())) {
        Ok(())
    } else {
        Err(ENOENT)
    }
}

/// Return the calling thread's cached slots before a purge so the purge
/// can reach the slabs they pin (`malloc_trim` does the same).
fn flush_caller(shared: &Shared, Tracked(ord): Tracked<&mut LockOrder>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
        old(ord).allows(rank_class(0, 0)),
    ensures
        *final(ord) == *old(ord),
{
    if let Some(h) = heap::existing_heap(heap::shared_ptr(shared)) {
        h.flush_all(shared, Tracked(ord));
    }
}

/// The calling thread's heap; `EAGAIN` when it has none to give (see
/// `heap_for_thread`: being created, torn down, or not to be had).
fn thread_heap(shared: &Shared) -> (r: Result<&mut Heap, c_int>)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        shared.wf(),
    ensures
        r matches Err(e) ==> e == EAGAIN,
        r matches Ok(h) ==> h.wf(),
{
    heap::heap_for_thread(shared).ok_or(EAGAIN)
}

/// Copy a reading out with its type's size, through the call's token.
fn read_out(io: &Io, r: Reading, Tracked(cc): Tracked<&mut CCall>) -> (res: Result<(), c_int>)
    requires
        io.handed(*old(cc)),
        !old(cc).written(),
    ensures
        res matches Err(e) ==> e == EINVAL,
{
    match r {
        Bool(v) => io.read(v, Tracked(cc)),
        Unsigned(v) => io.read(v, Tracked(cc)),
        U32(v) => io.read(v, Tracked(cc)),
        Size(v) => io.read(v, Tracked(cc)),
        Ssize(v) => io.read(v, Tracked(cc)),
        U64(v) => io.read(v, Tracked(cc)),
        Str(v) => io.read(v, Tracked(cc)),
        U64Ptr(v) => io.read(v, Tracked(cc)),
    }
}

/// The two errors that may follow a change `c`, both in jemalloc 5.3.0's
/// order: `epoch`'s read after its refresh (`Change::Refreshed`; jemalloc's
/// `epoch_ctl` reads after `ctl_refresh` too), and `background_thread`'s
/// `EAGAIN` when the thread it requested does not start and no other
/// thread is creating it (`background_thread_ctl` keeps the enabled flag set and fails too).
spec fn error_after_change(node: Node, r: Result<(), c_int>, c: Change) -> bool {
    ||| c is Refreshed
    ||| node.entry matches Entry::Op(Op::BackgroundThread) && r == Err::<(), c_int>(EAGAIN)
}

/// Perform the key `node` names with the numeric components of `mib`:
/// a read-only key through its reader, an [`Op`] as an arm. `ENOENT` for
/// an interior node. `cc` is the call's token, before any write through
/// its buffers. `fx` gets what the call changed ([`Change`], recorded by
/// the callee that makes the change, with `cc` as it stands
/// then): nothing when it fails, but for [`error_after_change`]; and a
/// setting with the old value read first when the caller asked for it.
/// `errno` is the calling thread's
/// `errno` token, for `background_thread`'s start. `mark` is the
/// [`ReadyMark`] of the `READY` the caller saw, for the readers.
// The tokens are parameters of their own, one per contract.
#[allow(clippy::too_many_arguments)]
fn ctl(
    shared: &Shared,
    Tracked(mark): Tracked<&'static ReadyMark>,
    node: &Node,
    mib: &[usize],
    io: &Io,
    Tracked(cc): Tracked<&mut CCall>,
    Tracked(fx): Tracked<&mut Ghost<Change>>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> (r: Result<(), c_int>)
    requires
        crate::sys::may_abort() || ctl_never_aborts(*node, mib@),
        crate::api::marks_ready(*mark),
        crate::sys::may_syscall(),
        shared.wf(),
        io.handed(*old(cc)),
        !old(cc).written(),
        // Both entry points pass the node the MIB resolves to.
        resolve(root_nodes(), mib@) == Some(*node),
    ensures
        r is Err && !error_after_change(*node, r, final(fx)@) ==> final(fx)@ is Unchanged,
        io.oldp@.addr != 0 && io.oldlenp@.addr != 0 ==> (final(fx)@ matches Change::Set { read_first } ==> read_first),
{
    proof {
        *fx = Ghost(Change::Unchanged);
        crate::vspec::ptr::axiom_plain_u64();
        crate::vspec::ptr::axiom_plain_usize();
        crate::vspec::ptr::axiom_plain_isize();
        crate::vspec::ptr::axiom_plain_u32();
        crate::vspec::ptr::axiom_plain_ptr::<c_void>();
    }
    // Premise (lock order): `mallctl`'s caller holds no allocator lock.
    let tracked mut ord = axiom_entry();
    let op = match node.entry {
        Entry::Dir(_) => return Err(ENOENT),
        Entry::Read(read) => {
            io.read_only()?;
            return read_out(io, call(read, mib, Tracked(mark))?, Tracked(cc));
        },
        Entry::Op(op) => op,
    };
    match op {
        Epoch => {
            // A write reads back the number its own refresh stored, not a
            // later writer's.
            let e = match io.write::<u64>()? {
                // `EAGAIN`, with nothing refreshed, when there is no
                // metadata object for the collection's scratch record.
                // Before the refresh, the caller's cache GC when one is due
                // (`Heap::gc_if_due`, design 13.5), so the figures do not
                // count slots that only sit in its caches.
                Some(_) => {
                    if let Some(h) = heap::existing_heap(heap::shared_ptr(shared)) {
                        h.gc_if_due(shared, Tracked(&mut ord));
                    }
                    // And the slab-pages walks, so a consumer's next read
                    // counts the free pages they release as released.
                    crate::purge::note_epoch(shared);
                    stats::epoch(shared, Tracked(&mut *fx), Tracked(&mut ord)).ok_or(EAGAIN)?
                },
                None => stats::snapshot(shared, Tracked(&mut ord)).epoch,
            };
            io.read(e, Tracked(cc))
        },
        BackgroundThread => {
            let on = io.write_bool()?;
            // The old state, before the write changes it.
            io.read(purge::background::present(), Tracked(cc))?;
            if let Some(on) = on {
                // Inside the fork window (an atfork handler registered
                // before challoc's) the thread neither starts nor stops,
                // and the request is not recorded: the fork handlers'
                // bookkeeping of it assumes nothing changes there.
                if crate::lock::in_fork_window() {
                    return Err(EAGAIN);
                }
                purge::background::request(on, Tracked(&*cc), Tracked(&mut *fx), Tracked(&mut ord));
                // The slot token: `start` parks the slot through its cell
                // while it creates the thread. No heap is borrowed here.
                let tracked tls = crate::sys::tls::TlsSlot::entry();
                // A `start` that found another thread (an allocation's
                // tick or another enable) creating the thread is no
                // failure: the thread is starting. jemalloc's racing
                // enable waits on its lock and returns 0; this one
                // returns 0 without waiting. Should that creation fail,
                // the request stays and the next tick tries again.
                if on && !purge::background::present()
                    && !purge::background::start(shared, false, Tracked(&mut ord), Tracked(errno), Tracked(&mut tls.cell))
                    && !purge::background::present_or_creating(Tracked(&mut ord))
                {
                    return Err(EAGAIN);
                }
            }
            Ok(())
        },
        MaxBackgroundThreads => {
            // Two `if`s, not a let chain: Verus takes no let chain.
            #[allow(clippy::collapsible_if)]
            if let Some(n) = io.write::<usize>()? {
                if n == 0 {
                    return Err(EINVAL);
                }
            }
            io.read(1usize, Tracked(cc))
        },
        ArenasCreate => {
            // `newp` may carry extent hooks, which challoc has no use for.
            // A read that would fail must not leave an arena behind.
            io.verify_read::<c_uint>(Tracked(&*cc))?;
            proof {
                shared.lemma_wf_slow();
            }
            let i = shared.arenas.create(shared, Tracked(&mut *fx), Tracked(&mut ord)).ok_or(EAGAIN)?;
            io.read(i as c_uint, Tracked(cc))
        },
        ArenasLookup => {
            let p = io.write::<*const c_void>()?.ok_or(EINVAL)?;
            let a = owner_of(shared, p).ok_or(EFAULT)?;
            io.read(a as c_uint, Tracked(cc))
        },
        ArenaPurge => {
            io.void()?;
            flush_caller(shared, Tracked(&mut ord));
            let i = at(mib, 1);
            if all_arenas(shared, i) {
                let tracked mut left = Ghost(purge::Left::none());
                purge::purge_all(shared, Tracked(&mut left), Tracked(&mut ord));
            } else {
                let tracked mut left = Ghost(purge::Left::none());
                purge::purge_arena(shared, arena_index(i)?, Tracked(&mut left), Tracked(&mut ord)).ok_or(ENOENT)?;
            }
            Ok(())
        },
        ArenaDecay => {
            io.void()?;
            let now = sys::now_coarse_ns();
            let i = at(mib, 1);
            if all_arenas(shared, i) {
                purge::decay(shared, now, Tracked(&mut ord));
            } else {
                purge::decay_arena(shared, arena_index(i)?, now, Tracked(&mut ord)).ok_or(ENOENT)?;
            }
            Ok(())
        },
        ArenaDestroy => {
            io.void()?;
            let i = at(mib, 1);
            if i == 0 || i == ARENAS_ALL as usize {
                return Err(EFAULT);
            }
            flush_caller(shared, Tracked(&mut ord));
            let tracked mut left = Ghost(purge::Left::none());
            purge::purge_arena(shared, arena_index(i)?, Tracked(&mut left), Tracked(&mut ord)).ok_or(ENOENT)?;
            Ok(())
        },
        ArenaDirtyDecayMs => {
            let i = at(mib, 1);
            let new = io.write::<isize>()?;
            // As above, two `if`s.
            #[allow(clippy::collapsible_if)]
            if let Some(ms) = new {
                if ms < -1 {
                    return Err(EFAULT);
                }
            }
            // The old period is read before the new one is applied, so a
            // read error changes nothing.
            if i == ARENAS_ALL as usize {
                io.read(purge::ms_of(shared.decay_ns()) as isize, Tracked(cc))?;
                if let Some(ms) = new {
                    shared.set_decay_ms(ms as i64, Tracked(&*cc), Tracked(&mut *fx));
                }
                return Ok(());
            }
            let arena = shared.arena(arena_index(i)?, Tracked(&mut ord)).ok_or(ENOENT)?;
            io.read(purge::ms_of(arena.0.decay_ns()) as isize, Tracked(cc))?;
            if let Some(ms) = new {
                arena.0.set_decay_ms(ms as i64, Tracked(&*cc), Tracked(&mut *fx));
            }
            Ok(())
        },
        ThreadTcacheFlush => {
            io.void()?;
            flush_caller(shared, Tracked(&mut ord));
            Ok(())
        },
        ThreadTcacheEnabled => {
            let h = thread_heap(shared)?;
            let on = io.write_bool()?;
            io.read(h.tcache_enabled(), Tracked(cc))?;
            if let Some(on) = on {
                h.set_tcache_enabled(shared, on, Tracked(&*cc), Tracked(&mut *fx), Tracked(&mut ord));
            }
            Ok(())
        },
        ThreadArena => {
            let h = thread_heap(shared)?;
            let new = io.write::<c_uint>()?;
            // As above, two `if`s.
            #[allow(clippy::collapsible_if)]
            if let Some(a) = new {
                if a as usize >= MAX_ARENAS || shared.arena(a as u16, Tracked(&mut ord)).is_none() {
                    return Err(EFAULT);
                }
            }
            io.read(h.arena() as c_uint, Tracked(cc))?;
            if let Some(a) = new {
                h.set_arena(shared, a as u16, Tracked(&*cc), Tracked(&mut *fx), Tracked(&mut ord));
            }
            Ok(())
        },
        TcacheCreate => {
            io.read_only()?;
            // A read that would fail must not leave a tcache behind.
            io.verify_read::<c_uint>(Tracked(&*cc))?;
            // `existing_heap(shared).map_or(0, |h| h.arena())`, spelled as
            // a `match`.
            let home = match heap::existing_heap(heap::shared_ptr(shared)) {
                Some(h) => h.arena(),
                None => 0,
            };
            let t = shared.tcaches.create(shared, home, Tracked(&mut *fx), Tracked(&mut ord)).ok_or(EFAULT)?;
            io.read(t as c_uint, Tracked(cc))
        },
        TcacheFlush | TcacheDestroy => {
            io.write_only()?;
            let t = io.write::<c_uint>()?.ok_or(EINVAL)?;
            if t > u16::MAX as c_uint {
                return Err(EFAULT);
            }
            let ok = match op {
                TcacheFlush => shared.tcaches.flush(shared, t as u16, Tracked(&mut ord)),
                _ => {
                    let tracked mut w = heap::TableWrite::new();
                    shared.tcaches.destroy(shared, t as u16, Tracked(&mut w), Tracked(&mut ord))
                },
            };
            if ok { Ok(()) } else { Err(EFAULT) }
        },
        UtilQuery => {
            let p = io.write::<*const c_void>()?.ok_or(EINVAL)?;
            io.read(utilization(shared, p, true), Tracked(cc))
        },
        UtilBatchQuery => batch_query(shared, io, Tracked(cc)),
    }
}

fn result(r: Result<(), c_int>) -> (c: c_int)
    ensures
        r is Ok ==> c == 0,
        r matches Err(e) ==> c == e,
{
    match r {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// `mallctl(name, oldp, oldlenp, newp, newlen)`. The pointers are C's,
/// under the `mallctl` contract: `cc` is the call's token for exactly
/// these arguments ([`CEntry::Mallctl`]), and `errno` the calling thread's `errno`
/// token; the export wrapper mints the first and lends the second.
pub fn mallctl(
    name: *const c_char,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    newp: *const c_void,
    newlen: usize,
    Tracked(cc): Tracked<CCall>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> c_int
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        cc.fresh(CEntry::Mallctl {
            name: name as *const u8,
            oldp: oldp as *mut u8,
            oldlenp: oldlenp as *const usize,
            newp: newp as *mut u8,
            newlen,
        }),
{
    let tracked mut cc = cc;
    let tracked mut fx = Ghost(Change::Unchanged);
    let Some(name) = cstr(name, Tracked(&cc)) else {
        return EINVAL;
    };
    let mut mib = [0usize; MAX_MIB];
    let (len, node) = match name_to_mib(name, &mut mib) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let (shared, mark) = super::shared_published();
    result(ctl(
        shared,
        mark,
        node,
        &mib[..len],
        &Io {
            oldp,
            oldlenp,
            newp,
            newlen,
        },
        Tracked(&mut cc),
        Tracked(&mut fx),
        Tracked(errno),
    ))
}

/// `*miblenp`, through the read permission the call's token gives
/// ([`CArg::Counted`], [`CCall::counted`]), which also makes `mibp` an
/// array of that many `size_t`s, before any write through either.
///
/// # Safety
/// `miblenp` is the caller's non-null capacity pointer.
#[inline(always)]
unsafe fn mib_cap(miblenp: *mut usize, Ghost(mibp): Ghost<*mut usize>, Tracked(cc): Tracked<&CCall>) -> (r: usize)
    requires
        !cc.written(),
        cc.has(CArg::Counted(mibp, miblenp as *const usize)),
    ensures
        c_buffer(mibp, r as int * size_of::<usize>()),
{
    let tracked cap = cc.counted(mibp, miblenp as *const usize);
    // SAFETY: guaranteed by the caller.
    *ptr_ref(miblenp, Tracked(cap))
}

/// Copy `mib[..len]` out to `mibp` and store `len` at `miblenp`: unaligned
/// writes, one word at a time, of `len` words inside the capacity
/// `*miblenp == cap` the caller gave ([`c_buffer`], from [`mib_cap`]), and
/// of the `size_t` at `miblenp` ([`CArg::facts`]), once the writes through
/// both are open ([`c_writes`]).
///
/// # Safety
/// `mibp` holds `cap` writable `size_t`s; `miblenp` is the caller's
/// non-null capacity pointer.
#[inline(always)]
unsafe fn mib_out(mib: &[usize; MAX_MIB], mibp: *mut usize, miblenp: *mut usize, len: usize, Ghost(cap): Ghost<usize>)
    requires
        len <= MAX_MIB,
        len <= cap,
        c_buffer(mibp, cap as int * size_of::<usize>()),
        c_buffer(miblenp, size_of::<usize>() as int),
        c_writes(mibp@.provenance),
        c_writes(miblenp@.provenance),
{
    let mut i = 0;
    while i < len
        invariant
            i <= len <= MAX_MIB,
            len <= cap,
            c_buffer(mibp, cap as int * size_of::<usize>()),
            c_writes(mibp@.provenance),
        decreases len - i,
    {
        proof {
            axiom_ptr_add_addr(mibp, i);
            lemma_c_buffer_at(mibp, cap as int * 8, ptr_add(mibp, i), i as int * 8);
        }
        // SAFETY: word `i < len <= cap` of the caller's array.
        unsafe { mibp.add(i).write_unaligned(mib[i]) };
        i += 1;
    }
    proof {
        lemma_c_buffer_at(miblenp, 8, miblenp, 0);
    }
    // SAFETY: the caller's capacity pointer.
    unsafe { miblenp.write_unaligned(len) };
}

/// `mallctlnametomib(name, mibp, miblenp)`: `*miblenp` is the capacity in
/// and the length out. The pointers are C's, under the
/// `mallctlnametomib` contract: `cc` is the call's token for exactly
/// these arguments ([`CEntry::NameToMib`]), which the export wrapper
/// mints from its own parameters. A failure writes nothing through
/// `mibp` or `miblenp`: every error returns before the token's writes
/// open.
pub fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize, Tracked(cc): Tracked<&mut CCall>) -> (r: c_int)
    requires
        old(cc).fresh(CEntry::NameToMib { name: name as *const u8, mibp, miblenp: miblenp as *const usize }),
    ensures
        r != 0 ==> !final(cc).written(),
{
    let Some(name) = cstr(name, Tracked(&*cc)) else {
        return EINVAL;
    };
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
        let cap = mib_cap(miblenp, Ghost(mibp), Tracked(&*cc));
        if cap < len {
            return EINVAL;
        }
        proof {
            cc.open_writes(CArg::Counted(mibp, miblenp as *const usize));
        }
        mib_out(&mib, mibp, miblenp, len, Ghost(cap));
    }
    0
}

/// Copy the caller's `miblen` MIB entries into `local`: unaligned reads,
/// one word at a time, of the `miblen` `size_t`s the caller gave
/// ([`CArg::Words`], [`c_readable`]).
///
/// # Safety
/// `miblen` entries are readable at `mib`.
#[inline(always)]
unsafe fn mib_in(mib: *const usize, local: &mut [usize; MAX_MIB], miblen: usize)
    requires
        miblen <= MAX_MIB,
        c_readable(mib as *mut usize, miblen as int * size_of::<usize>()),
{
    let mut i = 0;
    while i < miblen
        invariant
            i <= miblen <= MAX_MIB,
            c_readable(mib as *mut usize, miblen as int * size_of::<usize>()),
        decreases miblen - i,
    {
        proof {
            axiom_ptr_add_addr(mib as *mut usize, i);
            lemma_c_readable_at(mib as *mut usize, miblen as int * 8, ptr_add(mib as *mut usize, i), i as int * 8);
            crate::vspec::ptr::axiom_plain_usize();
        }
        // SAFETY: entry `i < miblen` of the caller's MIB.
        local[i] = unsafe { mib.add(i).read_unaligned() };
        i += 1;
    }
}

/// `mallctlbymib(mib, miblen, oldp, oldlenp, newp, newlen)`. The pointers
/// are C's, under the `mallctl` contract: `cc` is the call's token for
/// exactly these arguments ([`CEntry::ByMib`]), and `errno` the calling thread's `errno`
/// token; the export wrapper mints the first and lends the second.
// The C arguments, the call's token and the `errno` token.
#[allow(clippy::too_many_arguments)]
pub fn mallctlbymib(
    mib: *const usize,
    miblen: usize,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    newp: *const c_void,
    newlen: usize,
    Tracked(cc): Tracked<CCall>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
) -> c_int
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
        cc.fresh(CEntry::ByMib {
            mib,
            miblen,
            oldp: oldp as *mut u8,
            oldlenp: oldlenp as *const usize,
            newp: newp as *mut u8,
            newlen,
        }),
{
    let tracked mut cc = cc;
    let tracked mut fx = Ghost(Change::Unchanged);
    if mib.is_null() || miblen == 0 || miblen > MAX_MIB {
        return EINVAL;
    }
    let mut local = [0usize; MAX_MIB];
    // SAFETY: `miblen` entries are readable at `mib`.
    unsafe { mib_in(mib, &mut local, miblen) };
    let node = match mib_to_node(&local[..miblen]) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let (shared, mark) = super::shared_published();
    result(ctl(
        shared,
        mark,
        node,
        &local[..miblen],
        &Io {
            oldp,
            oldlenp,
            newp,
            newlen,
        },
        Tracked(&mut cc),
        Tracked(&mut fx),
        Tracked(errno),
    ))
}

/// [`ctl_never_aborts`], computed: for the witnesses below.
#[cfg(verus_keep_ghost)]
fn never_aborts(node: &Node, _mib: &[usize]) -> (r: bool)
    ensures
        r == ctl_never_aborts(*node, _mib@),
{
    match node.entry {
        Entry::Dir(_) => true,
        Entry::Read(r) => match r {
            Reader::ThreadAllocated | Reader::ThreadAllocatedp | Reader::ThreadDeallocated | Reader::ThreadDeallocatedp => false,
            _ => true,
        },
        Entry::Op(op) => match op {
            UtilQuery | UtilBatchQuery => true,
            _ => false,
        },
    }
}

/// Read-only APIs never abort, for `mallctl`: a copy of [`mallctl`]'s body
/// but for its `super::shared_published()`, whose results are `shared`
/// and `mark`, the [`ReadyMark`] of a `READY` the caller saw, for a
/// read-only key other than `thread.*`'s four (every read key under
/// `opt`, `arenas` and `stats`, the
/// seven `stats.cgroup.*` keys that discover the cgroup first included,
/// `version` and `config.*`) or a utilization query, and every reader of
/// those keys, made where [`crate::sys::may_abort`] does not hold; the
/// readers' own `shared()` takes the path that does not abort, since a
/// holder of the mark loads `READY` (`api::shared_marked`). `mallctlbymib`'s are
/// [`theorem_mallctlbymib_reads_never_abort`]'s. Each call verifies, so
/// none of them aborts. `api::restated::mallctl_restatements_match_their_bodies`
/// checks that the copy and the body make the same calls. The pointers are taken to keep the C contract, as
/// the entry point takes them: `cc` is the call's token
/// ([`CEntry::Mallctl`]), as its export wrapper mints it (one per
/// witness, as one per call: no verified function holds two), and
/// `errno` the thread's `errno` token; the theorem says nothing of a
/// name or buffer that breaks the contract (longer than `isize::MAX`
/// bytes, wrapping the address space, or in challoc's memory).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such
/// function.
#[cfg(verus_keep_ghost)]
fn theorem_mallctl_reads_never_abort(
    shared: &Shared,
    name: *const c_char,
    io: &Io,
    m: &[usize],
    Tracked(mark): Tracked<&'static ReadyMark>,
    Tracked(cc): Tracked<CCall>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        crate::api::marks_ready(*mark),
        cc.fresh(CEntry::Mallctl {
            name: name as *const u8,
            oldp: io.oldp as *mut u8,
            oldlenp: io.oldlenp as *const usize,
            newp: io.newp as *mut u8,
            newlen: io.newlen,
        }),
{
    let tracked mut cc = cc;
    let tracked mut fx = Ghost(Change::Unchanged);
    // `mallctl`'s body, for a covered key.
    if let Some(name) = cstr(name, Tracked(&cc)) {
        let mut mib = [0usize; MAX_MIB];
        if let Ok((len, node)) = name_to_mib(name, &mut mib) {
            if never_aborts(node, &mib[..len]) {
                let _ = result(ctl(shared, Tracked(mark), node, &mib[..len], io, Tracked(&mut cc), Tracked(&mut fx), Tracked(errno)));
            }
        }
    }
    // `call`'s readers for those keys.
    let _ = opt_value(m, Tracked(mark));
    let _ = bin_size(m);
    let _ = bin_nregs(m);
    let _ = bin_slab_size(m);
    let _ = arenas_narenas(m, Tracked(mark));
    let _ = arenas_nbins(m);
    let _ = arenas_nlextents(m);
    let _ = arenas_page(m, Tracked(mark));
    let _ = arenas_quantum(m);
    let _ = bin_curregs(m, Tracked(mark));
    let _ = bin_curslabs(m, Tracked(mark));
    let _ = bin_nonfull_slabs(m, Tracked(mark));
    let _ = small_allocated(m, Tracked(mark));
    let _ = small_resident(m, Tracked(mark));
    let _ = large_allocated(m, Tracked(mark));
    let _ = arena_pactive(m, Tracked(mark));
    let _ = arena_pdirty(m, Tracked(mark));
    let _ = arena_pmuzzy(m, Tracked(mark));
    let _ = arena_resident(m, Tracked(mark));
    let _ = arena_mapped(m, Tracked(mark));
    let _ = arena_retained(m, Tracked(mark));
    let _ = cgroup_limit(m, Tracked(mark));
    let _ = cgroup_high(m, Tracked(mark));
    let _ = cgroup_current(m, Tracked(mark));
    let _ = cgroup_decay_ms(m, Tracked(mark));
    let _ = cgroup_retention_cap(m, Tracked(mark));
    let _ = cgroup_empty_bytes(m, Tracked(mark));
    let _ = cgroup_full_purges(m, Tracked(mark));
    let _ = cgroup_purge_advice(m);
    let _ = cgroup_lazy_purges(m);
    let _ = cgroup_eager_purges(m);
    let _ = thp_mode(m);
    let _ = thp_class_ranges(m);
    let _ = thp_class_bytes(m);
    let _ = thp_huge_ranges(m);
    let _ = thp_meta_ranges(m);
    let _ = thp_failures(m);
    let _ = thp_nohuge_ranges(m);
    let _ = stats_allocated(m, Tracked(mark));
    let _ = stats_active(m, Tracked(mark));
    let _ = stats_metadata(m, Tracked(mark));
    let _ = stats_resident(m, Tracked(mark));
    let _ = stats_mapped(m, Tracked(mark));
    let _ = stats_retained(m, Tracked(mark));
    let _ = config_malloc_conf(m);
    let _ = config_debug(m);
    let _ = config_stats(m);
    let _ = version(m);
}

/// Read-only APIs never abort, for `mallctlbymib`: a copy of its body but
/// for its `super::shared_published()`, whose results are `shared` and `mark`, for a MIB of a key or query
/// [`theorem_mallctl_reads_never_abort`] covers, as there, with `cb` the
/// call's token ([`CEntry::ByMib`]).
///
/// Verification only (`cfg(verus_keep_ghost)`): the build has no such
/// function.
#[cfg(verus_keep_ghost)]
fn theorem_mallctlbymib_reads_never_abort(
    shared: &Shared,
    mibp: *const usize,
    miblen: usize,
    io: &Io,
    Tracked(mark): Tracked<&'static ReadyMark>,
    Tracked(cb): Tracked<CCall>,
    Tracked(errno): Tracked<&mut ThreadErrno>,
)
    requires
        crate::sys::may_syscall(),
        shared.wf(),
        crate::api::marks_ready(*mark),
        cb.fresh(CEntry::ByMib {
            mib: mibp,
            miblen,
            oldp: io.oldp as *mut u8,
            oldlenp: io.oldlenp as *const usize,
            newp: io.newp as *mut u8,
            newlen: io.newlen,
        }),
{
    let tracked mut cb = cb;
    let tracked mut fx = Ghost(Change::Unchanged);
    // `mallctlbymib`'s body.
    if !mibp.is_null() && miblen != 0 && miblen <= MAX_MIB {
        let mut local = [0usize; MAX_MIB];
        // SAFETY: as in `mallctlbymib`.
        unsafe { mib_in(mibp, &mut local, miblen) };
        if let Ok(node) = mib_to_node(&local[..miblen]) {
            if never_aborts(node, &local[..miblen]) {
                let _ = result(ctl(shared, Tracked(mark), node, &local[..miblen], io, Tracked(&mut cb), Tracked(&mut fx), Tracked(errno)));
            }
        }
    }
}

} // verus!

// ---------------------------------------------------------------------------
// C ABI. Compiled out of the test binary (see `libc.rs`).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
mod exports {
    use crate::vspec::cbuf::CCall;
    #[cfg(verus_keep_ghost)]
    use crate::vspec::cbuf::CEntry;
    use core::ffi::{c_char, c_int, c_void};
    use vstd::prelude::{Tracked, verus};

    // `verified_export_pair!` expands inside its own `verus!`; this outer
    // block holds it (Verus passes the invocation through) and the mint.
    verus! {

    /// Trusted (the C contract of the entry point, `vspec::cbuf`): the
    /// token of this call from C, through the entry point and with the
    /// arguments `e` names: the export wrapper's own parameters, which C
    /// passed under the entry point's contract ([`CCall::fresh`]: each
    /// pointer argument's [`crate::vspec::cbuf::CArg`] facts, of memory of
    /// the program's, and the string argument's view). Private to this
    /// module, whose functions are the C entry points and the
    /// `mallctlnametomib` mint below: each export mints its call's token
    /// once, from its own parameters, so no other verified code can mint
    /// one or name another call's arguments. A C caller that breaks the
    /// contract (a buffer past `isize::MAX` bytes or wrapping the address
    /// space, a string that is not NUL-terminated, a pointer into
    /// challoc's memory other than a published counter read as such)
    /// makes it false. The `mallctlnametomib` exports mint theirs through
    /// `nametomib_call`, which each calls once: checked, with the
    /// one-token-per-export rule, by
    /// `api::tests::c_call_tokens_only_in_export_bodies`.
    axiom fn axiom_c_call(e: CEntry) -> (tracked c: CCall)
        requires
            e is Mallctl || e is NameToMib || e is ByMib,
        ensures
            c.fresh(e),
    ;

    /// The token of a `mallctlnametomib` call ([`axiom_c_call`]):
    /// `verified_export_pair!` takes its bodies as Rust blocks, which have
    /// no `tracked` bindings, so the mint is here and the token comes back
    /// as a `Tracked` value, as `vspec::errno::lend`'s. Called once by each
    /// of the two exports.
    #[inline(always)]
    // The parameters are read by the ghost code only.
    #[allow(unused_variables)]
    fn nametomib_call(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> (cc: Tracked<CCall>)
        ensures
            cc@.fresh(CEntry::NameToMib { name: name as *const u8, mibp, miblenp: miblenp as *const usize }),
    {
        let tracked cc = axiom_c_call(CEntry::NameToMib { name: name as *const u8, mibp, miblenp: miblenp as *const usize });
        Tracked(cc)
    }

    verified_export_pair! {
        fn mallctl / je_mallctl(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize
        ) -> (r: c_int) {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::mallctl(
                name,
                oldp,
                oldlenp,
                newp,
                newlen,
                Tracked(axiom_c_call(CEntry::Mallctl {
                    name: name as *const u8,
                    oldp: oldp as *mut u8,
                    oldlenp: oldlenp as *const usize,
                    newp: newp as *mut u8,
                    newlen,
                })),
                Tracked(&mut errno),
            )
        }
        fn mallctlnametomib / je_mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> (r: c_int) {
            let Tracked(mut cc) = nametomib_call(name, mibp, miblenp);
            super::mallctlnametomib(name, mibp, miblenp, Tracked(&mut cc))
        }
        fn mallctlbymib / je_mallctlbymib(
            mib: *const usize,
            miblen: usize,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize
        ) -> (r: c_int) {
            let Tracked(mut errno) = crate::vspec::errno::lend();
            super::mallctlbymib(
                mib,
                miblen,
                oldp,
                oldlenp,
                newp,
                newlen,
                Tracked(axiom_c_call(CEntry::ByMib {
                    mib,
                    miblen,
                    oldp: oldp as *mut u8,
                    oldlenp: oldlenp as *const usize,
                    newp: newp as *mut u8,
                    newlen,
                })),
                Tracked(&mut errno),
            )
        }
    }

    } // verus!
}

#[cfg(test)]
mod tests;

/// Kani checks of `root` and `VERSION` (`verus/ledger/src/api/mallctl.rs.md`;
/// `cfg(kani)` only, see `verus/README.md`, "Kani").
#[cfg(kani)]
mod kani_root {
    use super::{ROOT, root};

    /// `root`, `ensures r@ == root_nodes()`, with `root_nodes`
    /// uninterpreted: every call returns the same nodes. Each call returns
    /// `ROOT` itself, an immutable static, so two calls return the same
    /// slice.
    #[kani::proof]
    fn root_returns() {
        let r = root();
        assert!(core::ptr::eq(r, &ROOT[..]));
        let r2 = root();
        assert!(core::ptr::eq(r2, r));
    }

    /// Every byte of `s` from `i` to `j` is an ASCII digit, and there is
    /// at least one.
    fn digits(s: &[u8], i: usize, j: usize) -> bool {
        i < j && s[i..j].iter().all(u8::is_ascii_digit)
    }

    /// `VERSION`, trusted as a value of its type: it is `VERSION_STR`,
    /// whose shape (its comment) is jemalloc's
    /// `<major>.<minor>.<bugfix>-<nrev>-g<gid>`, which this checks of
    /// `5.3.0-0-gchalloc`, and `VERSION_CSTR` is the same bytes and a NUL.
    #[kani::proof]
    #[kani::unwind(20)]
    fn version_is_version_str() {
        use super::{VERSION, VERSION_CSTR, VERSION_STR};
        assert_eq!(VERSION, VERSION_STR);
        let s = VERSION.as_bytes();
        assert!(digits(s, 0, 1) && s[1] == b'.' && digits(s, 2, 3) && s[3] == b'.');
        assert!(digits(s, 4, 5) && s[5] == b'-' && digits(s, 6, 7) && s[7] == b'-');
        assert!(s[8] == b'g' && s.len() > 9);
        assert_eq!(&VERSION_CSTR[..s.len()], s);
        assert_eq!(VERSION_CSTR[s.len()], 0);
        assert_eq!(VERSION_CSTR.len(), s.len() + 1);
    }
}
