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
use crate::sys::Errno;
use crate::sys::bytes_is;
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

// `mallctl`'s return codes as `c_int`; the numbers live in `sys::errno`.
const EPERM: c_int = Errno::EPERM.0;
const ENOENT: c_int = Errno::ENOENT.0;
const EAGAIN: c_int = Errno::EAGAIN.0;
const EFAULT: c_int = Errno::EFAULT.0;
const EINVAL: c_int = Errno::EINVAL.0;

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
    Ok(Size(
        match shared(Tracked(mark)).policy.effective_decay_ns(shared(Tracked(mark)).decay_ns()) {
            purge::NEVER => usize::MAX,
            ns => (ns / 1_000_000) as usize,
        },
    ))
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

/// The decimal value of the digits `s` (`strtoumax(.., 10)` without the
/// overflow clamp).
spec fn decimal(s: Seq<u8>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        decimal(s.drop_last()) * 10 + (s.last() - 48)
    }
}

/// Every byte of `s` is `0`..`9`.
spec fn all_digits(s: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> 48 <= #[trigger] s[i] <= 57
}

/// Digits spell a non-negative number that only grows as digits are
/// appended.
proof fn lemma_decimal_grows(s: Seq<u8>, j: int)
    requires
        all_digits(s),
        0 <= j <= s.len(),
    ensures
        0 <= decimal(s.subrange(0, j)) <= decimal(s),
    decreases s.len(),
{
    if s.len() > 0 {
        let d = s.drop_last();
        assert(all_digits(d)) by {
            assert forall|i: int| 0 <= i < d.len() implies 48 <= #[trigger] d[i] <= 57 by {
                assert(d[i] == s[i]);
            }
        }
        lemma_decimal_grows(d, d.len() as int);
        assert(d.subrange(0, d.len() as int) =~= d);
        if j < s.len() {
            lemma_decimal_grows(d, j);
            assert(d.subrange(0, j) =~= s.subrange(0, j));
        } else {
            assert(s.subrange(0, j) =~= s);
        }
        assert(48 <= s[s.len() - 1] <= 57);
    } else {
        assert(s.subrange(0, j) =~= s);
    }
}

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
        r is Some <==> s@.len() > 0 && all_digits(s@) && decimal(s@) <= usize::MAX,
        r matches Some(v) ==> v == decimal(s@),
{
    if s.is_empty() {
        return None;
    }
    let mut v: usize = 0;
    // `for &b in s`, spelled out with an index for the invariant.
    let mut k = 0;
    while k < s.len()
        invariant
            k <= s@.len(),
            all_digits(s@.subrange(0, k as int)),
            v == decimal(s@.subrange(0, k as int)),
        decreases s@.len() - k,
    {
        let b = s[k];
        if !b.is_ascii_digit() {
            return None;
        }
        let ghost next = s@.subrange(0, k + 1);
        proof {
            assert(next.drop_last() =~= s@.subrange(0, k as int));
            assert(all_digits(next));
        }
        // `v.checked_mul(10)?.checked_add(..)?`, each `?` spelled out so
        // the proof can say why the overflow means no number fits.
        let Some(t) = v.checked_mul(10) else {
            proof {
                if all_digits(s@) {
                    lemma_decimal_grows(s@, k + 1);
                }
            }
            return None;
        };
        let Some(w) = t.checked_add((b - b'0') as usize) else {
            proof {
                if all_digits(s@) {
                    lemma_decimal_grows(s@, k + 1);
                }
            }
            return None;
        };
        v = w;
        k += 1;
    }
    proof {
        assert(s@.subrange(0, k as int) =~= s@);
    }
    Some(v)
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
        if c.len() > 0 && all_digits(c) && i == decimal(c) {
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
    &&& l.len() == 1 && numeric(l[0]) ==> comps[j].len() > 0 && all_digits(comps[j]) && mib[j]
        == decimal(comps[j])
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
/// partial slab, and `run` the huge descriptor at `p`'s page
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
                &&& triple_is(u, sat_sub(c.nslots as int, rd.live), c.nslots as int, c.nslots * c.size)
                &&& if verbose && rd.found {
                    &&& u.bin_nregs == curslabs * c.nslots
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
        st: crate::class::ClassStats { committed: 0, guards: 0, empty: 0, purged: 0, nonfull: 0 },
        curregs: 0,
        lowest: None,
        run: None,
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
            u.nfree = d.nfree(cls);
            proof {
                // `nfree` is `nslots` less the live count it loaded.
                rd.live = cls.nslots - u.nfree;
            }
            u.nregs = cls.nslots();
            // The region bytes of the slab, not its power-of-two size: the
            // tail past the last slot belongs to no region, and Valkey's
            // allocatorShouldDefrag divides `size` by `nregs` and asserts
            // that the quotient is a bin size (jemalloc's slabs are exact
            // multiples of their region size).
            u.size = cls.nslots() * cls.size();
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
                    u.bin_nregs = st.curslabs() * cls.nslots();
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
        Value::Str(s) => Str(s.as_ptr()),
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

/// `arena.<i>.dirty_decay_ms` value of a period in nanoseconds.
fn ms_of(ns: u64) -> (r: isize)
    ensures
        r == if ns == purge::NEVER { -1 } else { (ns / 1_000_000) as int },
{
    if ns == purge::NEVER {
        -1
    } else {
        (ns / 1_000_000) as isize
    }
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
                    Some(b) => Ok(Size(b.curregs)),
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
                Some(_) => stats::epoch(shared, Tracked(&mut *fx), Tracked(&mut ord)).ok_or(EAGAIN)?,
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
                    && !purge::background::start(shared, Tracked(&mut ord), Tracked(errno), Tracked(&mut tls.cell))
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
                io.read(ms_of(shared.decay_ns()), Tracked(cc))?;
                if let Some(ms) = new {
                    shared.set_decay_ms(ms as i64, Tracked(&*cc), Tracked(&mut *fx));
                }
                return Ok(());
            }
            let arena = shared.arena(arena_index(i)?, Tracked(&mut ord)).ok_or(ENOENT)?;
            io.read(ms_of(arena.0.decay_ns()), Tracked(cc))?;
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
mod tests {
    use super::*;
    use crate::api::jemalloc::{dallocx, mallocx};
    use crate::api::libc::{free, malloc};
    use crate::api::shared;
    use crate::config::Config;
    use crate::heap::mallocx::{MALLOCX_TCACHE_NONE, mallocx_arena, mallocx_tcache};
    use std::ffi::CString;
    use std::vec::Vec;

    // The three entry points, with a placeholder token for the call and
    // for `errno`.
    fn mallctl(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *const c_void,
        newlen: usize,
    ) -> c_int {
        super::mallctl(
            name,
            oldp,
            oldlenp,
            newp,
            newlen,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }

    fn mallctlnametomib(name: *const c_char, mibp: *mut usize, miblenp: *mut usize) -> c_int {
        super::mallctlnametomib(name, mibp, miblenp, Tracked::assume_new())
    }

    fn mallctlbymib(
        mib: *const usize,
        miblen: usize,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *const c_void,
        newlen: usize,
    ) -> c_int {
        super::mallctlbymib(
            mib,
            miblen,
            oldp,
            oldlenp,
            newp,
            newlen,
            Tracked::assume_new(),
            Tracked::assume_new(),
        )
    }

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
        mallctl(
            cname.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            0,
        )
    }

    fn mib_of(name: &str) -> Vec<usize> {
        let cname = CString::new(name).unwrap();
        let mut mib = [0usize; MAX_MIB];
        let mut len = MAX_MIB;
        let rc = mallctlnametomib(cname.as_ptr(), mib.as_mut_ptr(), &mut len);
        assert_eq!(rc, 0, "mallctlnametomib({name}) = {rc}");
        mib[..len].to_vec()
    }

    /// The read-only subtrees: `opt`, `arenas` and `stats` are entries 4,
    /// 5 and 7 of the root, and no reader under them is one of the
    /// `thread.*` readers, the only ones that may abort
    /// (`Reader::aborts`): theorem "Read-only APIs never abort"
    /// (`theorem_mallctl_reads_never_abort`) covers every read under them.
    #[test]
    fn read_only_subtrees_are_roots_4_5_and_7() {
        assert_eq!(
            (ROOT[4].name, ROOT[5].name, ROOT[7].name),
            ("opt", "arenas", "stats")
        );
        fn readers(n: &Node, count: &mut usize) {
            match n.entry {
                Entry::Dir(children) => {
                    for c in children {
                        readers(c, count);
                    }
                }
                Entry::Read(r) => {
                    assert!(
                        !matches!(
                            r,
                            Reader::ThreadAllocated
                                | Reader::ThreadAllocatedp
                                | Reader::ThreadDeallocated
                                | Reader::ThreadDeallocatedp
                        ),
                        "`{}` is read by a `thread.*` reader",
                        n.name
                    );
                    *count += 1;
                }
                Entry::Op(_) => {}
            }
        }
        // The readers under `opt` (one per key), `arenas` and `stats`.
        let counts = [4, 5, 7].map(|i| {
            let mut count = 0;
            readers(&ROOT[i], &mut count);
            count
        });
        assert_eq!(counts, [KEYS.len(), 8, 35]);
    }

    /// A pointer count whose triples overflow `usize` must not pass the
    /// length check: `n * 24` wrapping to the caller's 8-byte `oldlen`
    /// would have the loop read `n` pointers and write `3 * n` words.
    #[test]
    fn batch_query_rejects_a_count_whose_triples_overflow() {
        let mib = mib_of("experimental.utilization.batch_query");
        let p = malloc(16, vstd::prelude::Tracked::assume_new());
        let ptrs = [p as *const c_void];
        let mut out = [0usize; 1];
        let mut outlen = size_of::<usize>();
        let n = usize::MAX / (3 * size_of::<usize>()) + 1;
        assert_eq!(
            n.wrapping_mul(3 * size_of::<usize>()),
            outlen,
            "the product wraps to the out length"
        );
        let newlen = n * size_of::<*const c_void>();
        let rc = mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            out.as_mut_ptr() as *mut c_void,
            &mut outlen,
            ptrs.as_ptr() as *const c_void,
            newlen,
        );
        assert_eq!(rc, EINVAL);
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    fn get_mib<T: Copy + Default>(mib: &[usize]) -> T {
        let mut v = T::default();
        let mut len = size_of::<T>();
        let rc = mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            &mut v as *mut T as *mut c_void,
            &mut len,
            ptr::null(),
            0,
        );
        assert_eq!(rc, 0, "mallctlbymib({mib:?}) = {rc}");
        v
    }

    fn epoch() -> u64 {
        let mut e: u64 = 0;
        let one: u64 = 1;
        assert_eq!(ctl_rw("epoch", Some(&mut e), Some(&one)), 0);
        e
    }

    /// Each `epoch` write stores a new number and reads that number back,
    /// so concurrent writers all read back different numbers. The four
    /// writers start every write together, on a private allocator so no
    /// other test's epochs interleave.
    #[test]
    fn concurrent_epoch_writes_read_back_their_own_numbers() {
        use std::sync::Barrier;
        let s = crate::testutil::shared();
        let mut mib = [0usize; MAX_MIB];
        let (len, node) = name_to_mib(b"epoch", &mut mib).unwrap();
        let write = || {
            let mut e: u64 = 0;
            let mut elen = size_of::<u64>();
            let one: u64 = 1;
            let io = Io {
                oldp: &mut e as *mut u64 as *mut c_void,
                oldlenp: &mut elen,
                newp: &one as *const u64 as *const c_void,
                newlen: size_of::<u64>(),
            };
            // Checked after the join: a failed write must not leave the
            // other writers waiting at the barrier.
            ctl(
                s,
                Tracked::assume_new(),
                node,
                &mib[..len],
                &io,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .map(|()| e)
        };
        let go = Barrier::new(4);
        let seen: Vec<Result<u64, c_int>> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    sc.spawn(|| {
                        (0..2000)
                            .map(|_| {
                                go.wait();
                                write()
                            })
                            .collect::<Vec<Result<u64, c_int>>>()
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let seen: Vec<u64> = seen.into_iter().map(|r| r.expect("epoch write")).collect();
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            (sorted.len(), sorted.first(), sorted.last()),
            (seen.len(), Some(&1), Some(&(seen.len() as u64))),
            "(distinct, lowest, highest) epoch numbers read back"
        );
    }

    /// `arena.0.decay` walks the huge extents on the period `purge::decay`
    /// gives them, as `arena.<MALLCTL_ARENAS_ALL>.decay` does: the huge
    /// tier's own (`huge_dirty_decay_ms`, design 7) rather than arena 0's,
    /// and both halves scaled by the cgroup policy's band (design 11).
    #[test]
    fn arena_decay_uses_the_periods_of_the_decay_driver() {
        use crate::policy::Band;
        let decay = |s: &Shared, i: u16| {
            let mut mib = [0usize; MAX_MIB];
            let name = std::format!("arena.{i}.decay");
            let (len, node) = name_to_mib(name.as_bytes(), &mut mib).unwrap();
            let io = Io {
                oldp: ptr::null_mut(),
                oldlenp: ptr::null_mut(),
                newp: ptr::null(),
                newlen: 0,
            };
            assert_eq!(
                ctl(
                    s,
                    Tracked::assume_new(),
                    node,
                    &mib[..len],
                    &io,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                    Tracked::assume_new()
                ),
                Ok(()),
                "{name}"
            );
        };
        let free_huge_run = |s: &Shared| {
            let p = s
                .alloc_huge(
                    300 * 1024,
                    16,
                    0,
                    false,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                )
                .0;
            match s.regions.classify(p as usize) {
                Kind::Huge { page } => s.free_huge(
                    page,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                ),
                other => panic!("{other:?}"),
            };
            assert_eq!(
                (
                    s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
                    s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
                ),
                (1, 0)
            );
        };

        // `huge_dirty_decay_ms:-1` never purges a huge run by decay, even
        // with arena 0 purging at once.
        let s = crate::testutil::TestShared::with(Config {
            narenas: 1,
            dirty_decay_ms: 0,
            huge_dirty_decay_ms: Some(-1),
            ..Config::DEFAULT
        })
        .build();
        free_huge_run(s);
        decay(s, 0);
        assert_eq!(
            (
                s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
                s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
            ),
            (1, 0),
            "huge_dirty_decay_ms:-1: retained"
        );

        // Above 90% of a cgroup limit every period is 0: a slab and a huge
        // run freed a moment ago both go, though both windows are 10 s.
        let s = crate::testutil::TestShared::with(Config {
            narenas: 1,
            ..Config::DEFAULT
        })
        .build();
        let class = CLASSES.len() - 1;
        let p = s
            .alloc_direct(
                &s.arena0,
                vstd::prelude::Tracked::assume_new(),
                class,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
            .0;
        assert!(!p.is_null());
        s.free_direct(
            0,
            class,
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(s.arena0.class(class).stats().empty, 1);
        free_huge_run(s);
        s.policy.apply(Band::Immediate, 512 << 20, 0, 0);
        // Only arena 0's call walks the huge extents.
        let a = s
            .arenas
            .create(
                s,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
            .unwrap();
        decay(s, a);
        assert_eq!(
            (
                s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
                s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
            ),
            (1, 0),
            "arena.{a}.decay leaves the huge run"
        );
        decay(s, 0);
        let slabs = s.arena0.class(class).stats();
        assert_eq!(
            (slabs.empty, slabs.purged),
            (0, 1),
            "immediate band: the slab is purged"
        );
        assert_eq!(
            (
                s.huge.stats(vstd::prelude::Tracked::assume_new()).empty,
                s.huge.stats(vstd::prelude::Tracked::assume_new()).purged
            ),
            (0, 1),
            "immediate band: the huge run is purged"
        );
    }

    fn cstring_at(p: *const c_char) -> std::string::String {
        // SAFETY: the callers pass a NUL-terminated string from `mallctl`.
        unsafe { core::ffi::CStr::from_ptr(p) }
            .to_str()
            .unwrap()
            .into()
    }

    /// Every name in the tree (numeric components as `0`), directories
    /// included, in tree order.
    fn all_names() -> Vec<std::string::String> {
        fn walk(nodes: &'static [Node], prefix: &str, out: &mut Vec<std::string::String>) {
            for n in nodes {
                let name = if n.name == NUMERIC { "0" } else { n.name };
                let full = if prefix.is_empty() {
                    name.into()
                } else {
                    std::format!("{prefix}.{name}")
                };
                out.push(full.clone());
                walk(n.children(), &full, out);
            }
        }
        let mut out = Vec::new();
        walk(&ROOT, "", &mut out);
        out
    }

    /// Every key's name resolves to a MIB that leads back to the same key,
    /// and a directory is never a key.
    #[test]
    fn every_name_round_trips_through_its_mib() {
        let names = all_names();
        assert!(names.len() > 100, "{names:?}");
        for name in &names {
            let mib = mib_of(name);
            let node = mib_to_node(&mib).unwrap();
            let last = name.rsplit('.').next().unwrap();
            assert!(
                node.name == last || (node.name == NUMERIC && last == "0"),
                "{name}: {mib:?}"
            );
            match node.entry {
                Entry::Dir(_) => assert_eq!(call(name), ENOENT, "{name}"),
                _ => assert_eq!(mib_to_key(&mib), Ok(node.name), "{name}"),
            }
        }
    }

    #[test]
    fn names_resolve_to_positional_mibs_and_back() {
        // Reads `stats.cgroup.decay_ms` against arena 0's decay.
        let _serial = crate::testutil::global_state();
        assert_eq!(mib_of("version"), [0]);
        assert_eq!(mib_of("epoch"), [1]);
        // Numeric components are stored as the number, at the position
        // Valkey overwrites.
        assert_eq!(mib_of("arenas.bin.7.size"), [5, 5, 7, 0]);
        assert_eq!(mib_of("arena.4096.purge"), [6, 4096, 0]);
        assert_eq!(
            mib_of("stats.arenas.3.bins.51.nonfull_slabs"),
            [7, 6, 3, 8, 51, 2]
        );
        assert_eq!(mib_of("stats.arenas.0.small.allocated"), [7, 6, 0, 3, 0]);
        assert_eq!(mib_of("experimental.utilization.batch_query"), [10, 0, 1]);
        for (i, key) in KEYS.iter().enumerate() {
            assert_eq!(mib_of(&std::format!("opt.{key}")), [4, i]);
        }
        // Interior nodes resolve (jemalloc allows it) but are not keys.
        assert_eq!(mib_of("arenas"), [5]);
        assert_eq!(mib_to_key(&[5]), Err(ENOENT));
        assert_eq!(call("arenas"), ENOENT);
        assert_eq!(mib_to_key(&[5, 5, 7, 0]), Ok("size"));
        assert_eq!(mib_to_key(&[7, 6, 3, 8, 51, 2]), Ok("nonfull_slabs"));
        assert_eq!(
            mib_to_key(&[7, 6, 3, 4, 51, 2]),
            Err(ENOENT),
            "large has one child"
        );
        assert_eq!(mib_to_key(&[99]), Err(ENOENT));
        assert_eq!(mib_to_key(&[5, 5, 7, 9]), Err(ENOENT));
        assert_eq!(mib_to_key(&[]), Err(ENOENT));
        // Unknown, malformed, and prof.* names.
        // `stats.cgroup.*` (design 11): the names resolve; a read is
        // `ENOENT` only where no cgroup2 is tracked.
        assert_eq!(mib_of("stats.cgroup.limit"), [7, 7, 0]);
        assert_eq!(mib_to_key(&[7, 7, 2]), Ok("current"));
        // The policy's keys (design 11, `policy.rs`) follow, in table order.
        assert_eq!(mib_of("stats.cgroup.decay_ms"), [7, 7, 3]);
        assert_eq!(mib_of("stats.cgroup.full_purges"), [7, 7, 6]);
        assert_eq!(mib_to_key(&[7, 7, 4]), Ok("retention_cap"));
        assert_eq!(mib_to_key(&[7, 7, 5]), Ok("empty_bytes"));
        // The two-advice purge's keys (design 8) follow and read
        // with or without a tracked cgroup: eager by default (no limit on
        // the host, none in the default container), nothing lazy issued.
        assert_eq!(mib_of("stats.cgroup.purge_advice"), [7, 7, 7]);
        assert_eq!(mib_of("stats.cgroup.lazy_purges"), [7, 7, 8]);
        assert_eq!(mib_of("stats.cgroup.eager_purges"), [7, 7, 9]);
        assert_eq!(mib_to_key(&[7, 7, 10]), Err(ENOENT));
        let advice: usize = get("stats.cgroup.purge_advice");
        assert_eq!(advice, 0, "eager without a limit");
        let lazy: usize = get("stats.cgroup.lazy_purges");
        assert_eq!(lazy, 0);
        let _eager: usize = get("stats.cgroup.eager_purges");
        assert_eq!(set("stats.cgroup.purge_advice", 1usize), EPERM);
        let tracked = shared()
            .cgroup_now(vstd::prelude::Tracked::assume_new())
            .tracked();
        for key in [
            "stats.cgroup.limit",
            "stats.cgroup.high",
            "stats.cgroup.current",
            "stats.cgroup.decay_ms",
            "stats.cgroup.retention_cap",
            "stats.cgroup.empty_bytes",
            "stats.cgroup.full_purges",
        ] {
            assert_eq!(
                call(key),
                if tracked { 0 } else { ENOENT },
                "{key} tracked={tracked}"
            );
            assert_eq!(
                set(key, 1usize),
                if tracked { EPERM } else { ENOENT },
                "{key}"
            );
        }
        if tracked {
            let limit: usize = get("stats.cgroup.limit");
            let high: usize = get("stats.cgroup.high");
            let current: usize = get("stats.cgroup.current");
            std::eprintln!("stats.cgroup: limit {limit} high {high} current {current}");
            assert_eq!(
                limit,
                shared()
                    .cgroup_now(vstd::prelude::Tracked::assume_new())
                    .limit() as usize
            );
            assert_eq!(
                high,
                shared()
                    .cgroup_now(vstd::prelude::Tracked::assume_new())
                    .high() as usize
            );
            assert!(limit == usize::MAX || limit > 0);
            // The policy's keys: the defaults without a limit (the period
            // as configured, no cap), the cap arithmetic with one.
            let decay_ms: usize = get("stats.cgroup.decay_ms");
            let cap: usize = get("stats.cgroup.retention_cap");
            let empty: usize = get("stats.cgroup.empty_bytes");
            let full: usize = get("stats.cgroup.full_purges");
            std::eprintln!(
                "stats.cgroup: decay_ms {decay_ms} retention_cap {cap} empty_bytes {empty} full_purges {full}"
            );
            let effective = limit.min(high);
            if effective == usize::MAX {
                assert_eq!(decay_ms, (shared().decay_ns() / 1_000_000) as usize);
                assert_eq!(cap, usize::MAX);
            } else {
                // The sampler may not have run in this process yet: the cap
                // is either unpublished or the limit's twentieth.
                assert!(
                    cap == usize::MAX || cap == effective / 20,
                    "cap {cap} for limit {effective}"
                );
            }
            assert_eq!(full, 0);
        }
        // `stats.cgroup` itself is interior: it resolves, and is not a key.
        assert_eq!(mib_of("stats.cgroup"), [7, 7]);
        assert_eq!(call("stats.cgroup"), ENOENT);
        for bad in [
            "nope",
            "arenas.bin.x.size",
            "arenas.bin..size",
            "prof.active",
            "prof.dump",
            "stats.cgroup.nope",
            "version.more",
            "",
        ] {
            let c = CString::new(bad).unwrap();
            let mut mib = [0usize; MAX_MIB];
            let mut len = MAX_MIB;
            assert_eq!(
                mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len),
                ENOENT,
                "{bad}"
            );
            assert_eq!(call(bad), ENOENT, "{bad}");
        }
        assert_eq!(
            mallctl(
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                0
            ),
            EINVAL
        );
        // Too small a MIB buffer.
        let c = CString::new("arenas.bin.7.size").unwrap();
        let mut mib = [0usize; 2];
        let mut len = 2;
        assert_eq!(
            mallctlnametomib(c.as_ptr(), mib.as_mut_ptr(), &mut len),
            EINVAL
        );
        assert_eq!(
            mallctlbymib(
                ptr::null(),
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                0
            ),
            EINVAL
        );
        assert_eq!(
            mallctlbymib(
                mib.as_ptr(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                0
            ),
            EINVAL
        );
    }

    #[test]
    fn read_write_rules_match_jemalloc() {
        // Advances the stats epoch.
        let _serial = crate::testutil::global_state();
        // Reading with the wrong size copies the prefix and fails EINVAL.
        let mut short: u32 = 0;
        let c = CString::new("arenas.page").unwrap();
        let mut len = 4usize;
        let rc = mallctl(
            c.as_ptr(),
            &mut short as *mut u32 as *mut c_void,
            &mut len,
            ptr::null(),
            0,
        );
        assert_eq!(rc, EINVAL);
        assert_eq!(
            short as usize,
            shared().regions.page_size(),
            "the low half was still copied"
        );
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
        // Asserts arena 0's decay, writes `background_thread`.
        let _serial = crate::testutil::global_state();
        let v: *const c_char = get("version");
        assert_eq!(cstring_at(v), VERSION_STR);
        assert!(cstring_at(v).starts_with("5.3.0-0-g"));
        let s: *const c_char = get("config.malloc_conf");
        assert_eq!(cstring_at(s), "");
        assert!(!get::<bool>("config.debug"));
        assert!(get::<bool>("config.stats"));
        assert_eq!(get::<c_uint>("arenas.nbins") as usize, NBINS);
        assert_eq!(
            get::<usize>(&std::format!("arenas.bin.{}.size", NBINS - 1)),
            crate::layout::BIN_MAX
        );
        assert_eq!(get::<c_uint>("arenas.nlextents"), 0);
        assert_eq!(get::<usize>("arenas.quantum"), QUANTUM);
        assert_eq!(get::<usize>("arenas.page"), shared().regions.page_size());
        assert!(get::<c_uint>("arenas.narenas") >= 1);
        assert_eq!(get::<usize>("max_background_threads"), 1);
        assert_eq!(set("max_background_threads", 0usize), EINVAL);
        assert_eq!(set("max_background_threads", 4usize), 0);
        for j in 0..NBINS {
            assert_eq!(
                get::<usize>(&std::format!("arenas.bin.{j}.size")),
                CLASSES[j].size()
            );
            assert_eq!(
                get::<u32>(&std::format!("arenas.bin.{j}.nregs")) as usize,
                CLASSES[j].nslots()
            );
            assert_eq!(
                get::<usize>(&std::format!("arenas.bin.{j}.slab_size")),
                CLASSES[j].slab_size()
            );
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
                Value::Str(s) => assert_eq!(
                    cstring_at(get::<*const c_char>(&name)),
                    s.to_str().unwrap(),
                    "{name}"
                ),
            }
            assert_eq!(set(&name, 0u8), EPERM, "{name} must be read-only");
        }
        for key in ["tcache_max", "huge_retain_max"] {
            assert!(
                matches!(cfg.get(key.as_bytes()), Some(Value::Size(_))),
                "opt.{key} is a byte count of the table"
            );
        }
        assert_eq!(
            call("opt.junk"),
            ENOENT,
            "accepted-and-ignored conf keys are not readable"
        );
        assert_eq!(
            call("opt.stats_print"),
            ENOENT,
            "the inert key without a field is not readable"
        );
        // The two byte-count options are `size_t`, as jemalloc's
        // `opt.tcache_max` is: an `unsigned` buffer is the wrong size and
        // gets the low half plus `EINVAL`, as for any size_t key.
        assert_eq!(get::<usize>("opt.tcache_max"), cfg.tcache_max);
        let mut half: c_uint = 0;
        assert_eq!(ctl_rw("opt.tcache_max", Some(&mut half), None), EINVAL);
        assert_eq!(
            half as usize,
            cfg.tcache_max & u32::MAX as usize,
            "the low half was still copied"
        );
        assert_eq!(
            get::<c_uint>("opt.narenas"),
            cfg.narenas as c_uint,
            "a count stays unsigned"
        );
        assert_eq!(
            get::<isize>("opt.dirty_decay_ms"),
            cfg.dirty_decay_ms as isize,
            "a period stays ssize_t"
        );
        assert_eq!(
            get::<isize>("opt.huge_dirty_decay_ms"),
            cfg.huge_decay_ms() as isize,
            "unset: the slab period"
        );
        // `opt.huge_retain_max` reports the value the huge allocator uses.
        assert_eq!(
            get::<usize>("opt.huge_retain_max"),
            shared().huge.retain_max()
        );
        assert_eq!(
            get::<usize>("opt.huge_retain_max"),
            crate::huge::DEFAULT_RETAIN_MAX
        );
        assert_eq!(
            get::<isize>("arena.0.dirty_decay_ms"),
            cfg.dirty_decay_ms as isize
        );
        // A background thread is neither requested by default nor running.
        let _ = get::<bool>("background_thread");
        assert_eq!(set("background_thread", false), 0);
    }

    #[test]
    fn stats_are_frozen_between_epochs_and_arenas_all_sums() {
        // Asserts that only its own `epoch` moves the epoch.
        let _serial = crate::testutil::global_state();
        let s = shared();
        // Other tests of this binary allocate and free concurrently, and
        // since `tcache_max` a freed 40 KiB block leaves `allocated` at
        // once instead of lingering in a cache, so the growth check retries
        // until a snapshot pair is not swamped by a neighbour's frees.
        let mut live: Vec<*mut u8> = Vec::new();
        let mut a1 = 0usize;
        for attempt in 0..20 {
            // Slots in this thread's cache (a previous attempt's frees)
            // still count as allocated; the blocks must come from the
            // slabs or `allocated` cannot grow.
            assert_eq!(call("thread.tcache.flush"), 0);
            let e0 = epoch();
            let a0: usize = get("stats.allocated");
            live = (0..300)
                .map(|_| malloc(200, vstd::prelude::Tracked::assume_new()))
                .collect();
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
                free(
                    p,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
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
        assert_eq!(
            get::<usize>("stats.arenas.4096.pactive") * s.regions.page_size(),
            active
        );
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
        assert!(
            get::<usize>(&std::format!("stats.arenas.4096.bins.{c}.nonfull_slabs")) <= curslabs
        );
        assert_eq!(
            call(&std::format!("stats.arenas.0.bins.{NBINS}.curregs")),
            ENOENT
        );
        for p in live {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
            assert_eq!(
                call(&std::format!("stats.arenas.4098.{key}")),
                ENOENT,
                "{key}: 4098 is nothing"
            );
        }
        let c = crate::layout::class_of(200).unwrap();
        let mut m = mib_of("stats.arenas.0.bins.0.curregs");
        m[2] = ARENAS_DESTROYED;
        m[4] = c;
        assert_eq!(get_mib::<usize>(&m), 0);
        assert_eq!(
            call(&std::format!("stats.arenas.4097.bins.{NBINS}.curregs")),
            ENOENT,
            "past arenas.nbins"
        );
        for key in ["purge", "decay", "destroy", "dirty_decay_ms"] {
            assert_eq!(
                call(&std::format!("arena.4097.{key}")),
                ENOENT,
                "arena.4097.{key}"
            );
        }
        assert_eq!(set("arena.4097.dirty_decay_ms", 5isize), ENOENT);
        assert_eq!(set("thread.arena", ARENAS_DESTROYED as c_uint), EFAULT);
        assert_eq!(
            ARENAS_DESTROYED,
            ARENAS_ALL as usize + 1,
            "the headers' value"
        );
    }

    #[test]
    fn arena_keys_create_lookup_purge_decay_and_destroy() {
        // Writes every arena's decay through `arena.4096`.
        let _serial = crate::testutil::global_state();
        let s = shared();
        let a: c_uint = get("arenas.create");
        assert!(a >= 1);
        assert!(s.arenas.get_existing(a as u16).is_some());
        assert!(get::<c_uint>("arenas.narenas") > a);
        let p = mallocx(
            500,
            mallocx_arena(a as u16) as c_int,
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(!p.is_null());
        let h = mallocx(
            2 << 20,
            mallocx_arena(a as u16) as c_int,
            vstd::prelude::Tracked::assume_new(),
        );
        // arenas.lookup finds both tiers; garbage is EFAULT, not an abort.
        for q in [p, h] {
            let mut out: c_uint = 0;
            let cname = CString::new("arenas.lookup").unwrap();
            let mut len = size_of::<c_uint>();
            let qp: *const c_void = q as *const c_void;
            let rc = mallctl(
                cname.as_ptr(),
                &mut out as *mut c_uint as *mut c_void,
                &mut len,
                &qp as *const *const c_void as *const c_void,
                size_of::<*const c_void>(),
            );
            assert_eq!(rc, 0);
            assert_eq!(out, a);
        }
        let stack = 0u64;
        let sp: *const c_void = &stack as *const u64 as *const c_void;
        let mut out: c_uint = 0;
        assert_eq!(
            ctl_rw::<c_uint>("arenas.lookup", Some(&mut out), None),
            EINVAL,
            "lookup needs a pointer"
        );
        let cname = CString::new("arenas.lookup").unwrap();
        let mut len = size_of::<c_uint>();
        assert_eq!(
            mallctl(
                cname.as_ptr(),
                &mut out as *mut c_uint as *mut c_void,
                &mut len,
                &sp as *const *const c_void as *const c_void,
                size_of::<*const c_void>()
            ),
            EFAULT
        );
        // Decay period per arena, and for all.
        assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), 250isize), 0);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 250);
        assert_eq!(
            s.arena(a as u16, vstd::prelude::Tracked::assume_new())
                .unwrap()
                .0
                .decay_ns(),
            250_000_000
        );
        assert_eq!(set(&std::format!("arena.{a}.dirty_decay_ms"), -1isize), 0);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), -1);
        assert_eq!(
            set(&std::format!("arena.{a}.dirty_decay_ms"), -2isize),
            EFAULT
        );
        let default = get::<isize>("arena.0.dirty_decay_ms");
        assert_eq!(set("arena.4096.dirty_decay_ms", 777isize), 0);
        assert_eq!(get::<isize>("arena.0.dirty_decay_ms"), 777);
        assert_eq!(get::<isize>(&std::format!("arena.{a}.dirty_decay_ms")), 777);
        assert_eq!(set("arena.4096.dirty_decay_ms", default), 0);
        assert_eq!(get::<isize>("arena.4096.dirty_decay_ms"), default);
        // Free everything in the arena, then purge: its Empty slab goes.
        dallocx(
            p,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        dallocx(
            h,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let arena = s
            .arena(a as u16, vstd::prelude::Tracked::assume_new())
            .unwrap();
        let c = crate::layout::class_of(500).unwrap();
        // The slot sits in this thread's cache for arena `a` until the
        // purge flushes the caller.
        assert_eq!(arena.0.classes[c].stats().empty, 0);
        assert_eq!(call(&std::format!("arena.{a}.purge")), 0);
        assert_eq!(arena.0.classes[c].stats().empty, 0);
        // The first refill spanned `cap / 2` slots' worth of slabs (one
        // 128-slot slab at 512 bytes); all of them were flushed and purged.
        let refill_slabs =
            (crate::heap::default_cap(c) as usize / 2).div_ceil(crate::layout::CLASSES[c].nslots());
        assert_eq!(
            arena.0.classes[c].stats().purged,
            refill_slabs,
            "the flushed slabs were purged"
        );
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
        assert!(
            stable,
            "arenas.narenas moved across eight consecutive probes"
        );
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
        assert!(
            s.arenas.get_existing(a as u16).is_some(),
            "destroy keeps the arena registered"
        );
        assert!(
            get::<c_uint>("arenas.narenas") >= narenas_before,
            "destroy retires no index"
        );
        let again = mallocx(
            500,
            mallocx_arena(a as u16) as c_int,
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(
            !again.is_null(),
            "the destroyed arena still serves MALLOCX_ARENA allocations"
        );
        assert_eq!(
            s.classify_free(again, vstd::prelude::Tracked::assume_new())
                .map(|x| x.1),
            Ok(a as u16)
        );
        assert_eq!(
            call(&std::format!("arena.{a}.destroy")),
            0,
            "destroy is repeatable"
        );
        dallocx(
            again,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }

    #[test]
    fn thread_and_tcache_keys() {
        // Its `tcache.destroy` checks that the id names no cache after;
        // `api::heap_at_tests` creates explicit caches under this guard.
        let _serial = crate::testutil::global_state();
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
        // SAFETY: `thread.allocatedp` points at this thread's counter, live
        // while the thread runs.
        assert_eq!(unsafe { *ap }, a0);
        let p = malloc(1000, vstd::prelude::Tracked::assume_new());
        let size = crate::layout::class_size_of(1000).unwrap() as u64;
        // The pointer is stale until a counter is read again.
        // SAFETY: as above.
        assert_eq!(unsafe { *ap }, a0);
        assert_eq!(get::<u64>("thread.allocated"), a0 + size);
        // SAFETY: as above.
        assert_eq!(unsafe { *ap }, a0 + size, "materialised by the read");
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(get::<u64>("thread.deallocated"), d0 + size);
        // SAFETY: as above, for `thread.deallocatedp`.
        assert_eq!(unsafe { *dp }, d0 + size);
        // thread.tcache.flush empties this thread's caches.
        let many: Vec<*mut u8> = (0..64)
            .map(|_| malloc(48, vstd::prelude::Tracked::assume_new()))
            .collect();
        for q in many {
            free(
                q,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
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
        let q = malloc(48, vstd::prelude::Tracked::assume_new());
        assert!(h.cache(c).is_none(), "no cache is created while disabled");
        assert_eq!(get::<u64>("thread.allocated"), a_before + 48);
        free(
            q,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert!(h.cache(c).is_none());
        assert_eq!(set("thread.tcache.enabled", true), 0);
        let q = malloc(48, vstd::prelude::Tracked::assume_new());
        assert!(h.cache(c).is_some(), "caches come back");
        free(
            q,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        // thread.arena moves the home arena.
        let home: c_uint = get("thread.arena");
        assert_eq!(home as u16, h.arena());
        let a: c_uint = get("arenas.create");
        assert_eq!(set("thread.arena", a), 0);
        assert_eq!(get::<c_uint>("thread.arena"), a);
        // Plain malloc comes from that arena through the thread's own caches.
        let q = malloc(200, vstd::prelude::Tracked::assume_new());
        assert_eq!(
            s.classify_free(q, vstd::prelude::Tracked::assume_new())
                .map(|x| x.1),
            Ok(a as u16)
        );
        free(
            q,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(set("thread.arena", 4000 as c_uint), EFAULT);
        assert_eq!(set("thread.arena", 5000 as c_uint), EFAULT);
        assert_eq!(set("thread.arena", home), 0);
        assert_eq!(get::<c_uint>("thread.arena"), home);
        // Explicit caches.
        let t: c_uint = get("tcache.create");
        let flags = mallocx_tcache(t as u16) as c_int;
        let e = mallocx(200, flags, vstd::prelude::Tracked::assume_new());
        let c = crate::layout::class_of(200).unwrap();
        dallocx(
            e,
            flags,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let th = s.tcaches.get(t as u16, Tracked::assume_new()).unwrap();
        assert!(th.cache_in(th.arena(), c).unwrap().len() >= 1);
        assert_eq!(set("tcache.flush", t), 0);
        assert_eq!(th.cache_in(th.arena(), c).unwrap().len(), 0);
        assert_eq!(set("tcache.destroy", t), 0);
        assert!(s.tcaches.get(t as u16, Tracked::assume_new()).is_none());
        assert_eq!(set("tcache.destroy", t), EFAULT);
        assert_eq!(set("tcache.flush", t), EFAULT);
    }

    #[test]
    fn utilization_queries_answer_valkeys_defragmenter() {
        let s = shared();
        let c = crate::layout::class_of(100).unwrap();
        let cls = &CLASSES[c];
        let ptrs: Vec<*mut u8> = (0..500)
            .map(|_| {
                mallocx(
                    100,
                    MALLOCX_TCACHE_NONE as c_int,
                    vstd::prelude::Tracked::assume_new(),
                )
            })
            .collect();
        // Direct allocations on a thread that has no heap yet come from
        // arena 0 (`heap::mallocx::alloc`, `Tcache::None`); once the thread has a
        // heap they come from its home arena. Either way one arena owns
        // all 500.
        let first = owner_of(s, ptrs[0] as *const c_void).expect("owned");
        // Verbose query on one pointer.
        let cname = CString::new("experimental.utilization.query").unwrap();
        let mut u = Utilization::ZERO;
        let mut len = size_of::<Utilization>();
        let p0: *const c_void = ptrs[0] as *const c_void;
        let rc = mallctl(
            cname.as_ptr(),
            &mut u as *mut Utilization as *mut c_void,
            &mut len,
            &p0 as *const *const c_void as *const c_void,
            size_of::<*const c_void>(),
        );
        assert_eq!(rc, 0);
        assert_eq!(u.nregs, cls.nslots());
        assert_eq!(
            u.size,
            cls.nslots() * cls.size(),
            "region bytes, so size / nregs is the class size"
        );
        assert!(u.nfree < u.nregs);
        assert!(u.bin_nregs >= u.nregs && u.bin_nregs.is_multiple_of(u.nregs));
        assert!(u.bin_nfree <= u.bin_nregs);
        assert!(
            u.bin_nregs - u.bin_nfree >= 500,
            "at least our 500 slots are out"
        );
        if !u.slabcur_addr.is_null() {
            assert!(
                matches!(s.regions.classify(u.slabcur_addr as usize), Kind::Slab { class, slot: 0, .. } if class as usize == c)
            );
        }
        assert_eq!(size_of::<Utilization>(), 6 * size_of::<usize>());
        // Batch query as Valkey does it: nametomib, then bymib with the
        // pointer array in and 3-tuples out.
        let mib = mib_of("experimental.utilization.batch_query");
        let mut out = std::vec![0usize; 3 * ptrs.len()];
        let mut outlen = out.len() * size_of::<usize>();
        let rc = mallctlbymib(
            mib.as_ptr(),
            mib.len(),
            out.as_mut_ptr() as *mut c_void,
            &mut outlen,
            ptrs.as_ptr() as *const c_void,
            ptrs.len() * size_of::<*mut u8>(),
        );
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
        assert_eq!(
            mallctlbymib(
                mib.as_ptr(),
                mib.len(),
                out.as_mut_ptr() as *mut c_void,
                &mut short,
                ptrs.as_ptr() as *const c_void,
                ptrs.len() * size_of::<*mut u8>()
            ),
            EINVAL
        );
        assert_eq!(
            mallctlbymib(
                mib.as_ptr(),
                mib.len(),
                out.as_mut_ptr() as *mut c_void,
                &mut outlen,
                ptr::null(),
                0
            ),
            EINVAL
        );
        // A huge run is one region of its own, and so is a slot of a class
        // above BIN_MAX (reported like jemalloc's large extents); a
        // non-allocation is zeros.
        let h = mallocx(3 << 20, 0, vstd::prelude::Tracked::assume_new());
        let m = mallocx(100 << 10, 0, vstd::prelude::Tracked::assume_new());
        let cm = crate::layout::class_of(100 << 10).unwrap();
        assert!((NBINS..crate::layout::NCLASSES).contains(&cm));
        let stack = 0u64;
        let four = [
            h as *const c_void,
            &stack as *const u64 as *const c_void,
            ptrs[1] as *const c_void,
            m as *const c_void,
        ];
        let mut out4 = [0usize; 12];
        let mut len4 = out4.len() * size_of::<usize>();
        assert_eq!(
            mallctlbymib(
                mib.as_ptr(),
                mib.len(),
                out4.as_mut_ptr() as *mut c_void,
                &mut len4,
                four.as_ptr() as *const c_void,
                four.len() * size_of::<*const c_void>()
            ),
            0
        );
        assert_eq!(&out4[..3], &[0, 1, s.huge.usable_for(3 << 20).unwrap()]);
        assert_eq!(&out4[3..6], &[0, 0, 0]);
        assert_eq!(out4[7], cls.nslots());
        assert_eq!(&out4[9..12], &[0, 1, CLASSES[cm].size()]);
        let um = utilization(s, m as *const c_void, true);
        assert_eq!(
            (um.nfree, um.nregs, um.size, um.bin_nfree, um.bin_nregs),
            (0, 1, CLASSES[cm].size(), 0, 0)
        );
        assert!(um.slabcur_addr.is_null());
        // A class above tcache_max comes from arena 0 on the default path
        // whatever the thread's arena (design 13.3); a cached class from
        // the thread's own arena.
        assert_eq!(owner_of(s, m as *const c_void), Some(0));
        let home = crate::heap::existing_heap(s).unwrap().arena();
        let mid = mallocx(8 << 10, 0, vstd::prelude::Tracked::assume_new());
        assert!(crate::layout::class_of(8 << 10).unwrap() >= NBINS);
        assert_eq!(owner_of(s, mid as *const c_void), Some(home));
        dallocx(
            mid,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        dallocx(
            m,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        dallocx(
            h,
            0,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        // A freed slot (direct release, so the bit is clear) is zeros too.
        let gone = ptrs[2] as *const c_void;
        dallocx(
            ptrs[2],
            MALLOCX_TCACHE_NONE as c_int,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        assert_eq!(utilization(s, gone, true), Utilization::ZERO);
        assert_eq!(owner_of(s, gone), None);
        assert_eq!(owner_of(s, ptrs[3] as *const c_void), Some(first));
        for (i, p) in ptrs.into_iter().enumerate() {
            if i != 2 {
                dallocx(
                    p,
                    MALLOCX_TCACHE_NONE as c_int,
                    vstd::prelude::Tracked::assume_new(),
                    vstd::prelude::Tracked::assume_new(),
                );
            }
        }
    }

    /// The mallctl write/read contract: an error leaves allocator state
    /// unchanged, and with both `oldp` and `newp` the old value is read
    /// before the new one is applied (as jemalloc 5.3.0 reads first in
    /// `arena_i_decay_ms_ctl_impl` and `thread_arena_ctl`, captures the old
    /// value in `thread_tcache_enabled_ctl`, and checks `*oldlenp` with
    /// `VERIFY_READ` in `arenas_create_ctl` and `tcache_create_ctl`).
    fn ctl_raw<T: Copy, U: Copy>(name: &str, old: &mut T, oldlen: usize, new: Option<&U>) -> c_int {
        let cname = CString::new(name).unwrap();
        let mut len = oldlen;
        let (newp, newlen) = match new {
            Some(n) => (n as *const U as *const c_void, size_of::<U>()),
            None => (ptr::null(), 0),
        };
        mallctl(
            cname.as_ptr(),
            old as *mut T as *mut c_void,
            &mut len,
            newp,
            newlen,
        )
    }

    #[test]
    fn contract_dirty_decay_ms_error_leaves_period_unchanged() {
        // `arena.4096.dirty_decay_ms` writes from other tests set every
        // arena's period.
        let _serial = crate::testutil::global_state();
        let a: c_uint = get("arenas.create");
        let key = std::format!("arena.{a}.dirty_decay_ms");
        assert_eq!(set(&key, 250isize), 0);
        // Wrong `*oldlenp` with a valid new value: EINVAL, so nothing
        // may change.
        let mut old = 0u32;
        let rc = ctl_raw::<u32, isize>(&key, &mut old, size_of::<u32>(), Some(&313isize));
        assert_eq!(rc, EINVAL);
        assert_eq!(get::<isize>(&key), 250, "an EINVAL read applied the write");
    }

    #[test]
    fn contract_dirty_decay_ms_reads_the_old_period() {
        let _serial = crate::testutil::global_state();
        let a: c_uint = get("arenas.create");
        let key = std::format!("arena.{a}.dirty_decay_ms");
        assert_eq!(set(&key, 250isize), 0);
        let mut old = 0isize;
        let rc = ctl_raw::<isize, isize>(&key, &mut old, size_of::<isize>(), Some(&313isize));
        assert_eq!(rc, 0);
        assert_eq!(old, 250, "oldp got the new period, not the old one");
    }

    #[test]
    fn contract_arenas_create_error_creates_nothing() {
        // A sentinel no arena index has: a create that went ahead would
        // copy the low half of its new index over it.
        let mut old = u16::MAX;
        let rc = ctl_raw::<u16, u8>("arenas.create", &mut old, size_of::<u16>(), None);
        assert_eq!(rc, EINVAL);
        assert_eq!(
            old,
            u16::MAX,
            "an EINVAL arenas.create created an arena and copied its index"
        );
    }

    #[test]
    fn contract_tcache_create_error_creates_nothing() {
        // A sentinel no tcache ID has, as for `arenas.create`.
        let mut old = u16::MAX;
        let rc = ctl_raw::<u16, u8>("tcache.create", &mut old, size_of::<u16>(), None);
        assert_eq!(rc, EINVAL);
        assert_eq!(
            old,
            u16::MAX,
            "an EINVAL tcache.create created a tcache and copied its ID"
        );
    }

    #[test]
    fn contract_thread_tcache_enabled_reads_the_old_value() {
        assert!(get::<bool>("thread.tcache.enabled"));
        let mut old = true;
        let rc = ctl_raw::<bool, bool>(
            "thread.tcache.enabled",
            &mut old,
            size_of::<bool>(),
            Some(&false),
        );
        assert_eq!(rc, 0);
        assert_eq!(set("thread.tcache.enabled", true), 0);
        assert!(old, "oldp got the new value, not the old one");
    }

    #[test]
    fn contract_thread_arena_reads_the_old_value() {
        let home: c_uint = get("thread.arena");
        let a: c_uint = get("arenas.create");
        let mut old: c_uint = c_uint::MAX;
        let rc = ctl_raw::<c_uint, c_uint>("thread.arena", &mut old, size_of::<c_uint>(), Some(&a));
        assert_eq!(rc, 0);
        assert_eq!(set("thread.arena", home), 0);
        assert_eq!(old, home, "oldp got the new arena, not the old one");
    }

    #[test]
    fn contract_background_thread_error_leaves_request_unchanged() {
        let _serial = crate::testutil::global_state();
        let before = purge::background::requested();
        // A wrong `*oldlenp` with a valid new value: the read fails before
        // the request is made, so no thread is started either.
        let mut old = 0u32;
        let rc = ctl_raw::<u32, bool>("background_thread", &mut old, size_of::<u32>(), Some(&true));
        assert_eq!(rc, EINVAL);
        assert_eq!(
            purge::background::requested(),
            before,
            "an EINVAL read made the request"
        );
    }

    #[test]
    fn background_thread_enable_racing_a_start_in_progress_succeeds() {
        use core::sync::atomic::{AtomicI32, Ordering};
        use std::sync::Arc;
        let _serial = crate::testutil::global_state();
        assert!(!purge::background::present());
        // A second enable, from another thread, while the first one's
        // `start` is between creating the thread and publishing it: that
        // `start` finds the creation in progress, and the enable returns
        // 0, as jemalloc's does once the thread runs.
        let racing = Arc::new(AtomicI32::new(-1));
        let racing_rc = Arc::clone(&racing);
        *purge::background::BEFORE_PUBLISH.lock().unwrap() =
            Some(std::boxed::Box::new(move || {
                // No panics here: unwinding out of `start` would leave the
                // state marked as creating for every later test.
                let rc = std::thread::spawn(|| set("background_thread", true))
                    .join()
                    .unwrap_or(-2);
                racing_rc.store(rc, Ordering::SeqCst);
            }));
        let rc = set("background_thread", true);
        // Another test's allocation tick may have become the creator and
        // run the hook; wait for it before taking the hook back.
        for _ in 0..400 {
            if racing.load(Ordering::SeqCst) != -1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        purge::background::BEFORE_PUBLISH.lock().unwrap().take();
        let present = purge::background::present();
        assert_eq!(set("background_thread", false), 0);
        for _ in 0..400 {
            if !purge::background::present() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let stopped = !purge::background::present();
        if stopped {
            purge::background::join_exited();
        }
        assert_eq!(rc, 0, "the enable that created the thread");
        assert_eq!(
            racing.load(Ordering::SeqCst),
            0,
            "the enable racing the creation (-1: the hook did not run)"
        );
        assert!(present, "background thread did not start");
        assert!(stopped, "background thread did not stop");
    }

    #[test]
    fn background_thread_enable_whose_thread_exits_before_it_publishes_fails() {
        use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};
        use std::sync::Arc;
        let _serial = crate::testutil::global_state();
        // A disable, through `mallctl` on another thread, while the
        // enable's `start` is between creating the thread and publishing
        // it, and the thread exits first: when that `start` returns
        // false, no thread is present or being created, so the enable
        // fails with EAGAIN. Another test's allocation tick may create a
        // thread in the same round (before the enable's `start`, so that
        // it runs the hook, or after the hook's disable, from a request it
        // read before it); such a round is cleaned up and run again, so
        // the checked round's only creation is the enable's.
        let mut checked = None;
        for _ in 0..20 {
            assert!(!purge::background::present());
            // 0: the hook did not run, 1: it ran on this thread and the
            // thread exited, 2: it ran here and the thread did not exit,
            // 3: it ran on another thread.
            let hook = Arc::new(AtomicU8::new(0));
            let hook_seen = Arc::clone(&hook);
            // `EXITS` once the hook saw the thread exit.
            let exited_at = Arc::new(AtomicU32::new(0));
            let exited_at_seen = Arc::clone(&exited_at);
            let me = std::thread::current().id();
            let exits = purge::background::EXITS.load(Ordering::SeqCst);
            *purge::background::BEFORE_PUBLISH.lock().unwrap() =
                Some(std::boxed::Box::new(move || {
                    // No panics here: unwinding out of `start` would leave
                    // the state marked as creating for every later test.
                    if std::thread::current().id() != me {
                        hook_seen.store(3, Ordering::SeqCst);
                        return;
                    }
                    let _ = std::thread::spawn(|| set("background_thread", false)).join();
                    let mut seen = 2;
                    for _ in 0..1000 {
                        let now = purge::background::EXITS.load(Ordering::SeqCst);
                        if now != exits {
                            exited_at_seen.store(now, Ordering::SeqCst);
                            seen = 1;
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    hook_seen.store(seen, Ordering::SeqCst);
                }));
            let rc = set("background_thread", true);
            let present = purge::background::present();
            // Still installed: no creation reached the hook (another
            // creator is still before it, or the creation failed).
            let unused = purge::background::BEFORE_PUBLISH
                .lock()
                .unwrap()
                .take()
                .is_some();
            // Taken: wait for the creator that took it to report.
            if !unused {
                for _ in 0..1200 {
                    if hook.load(Ordering::SeqCst) != 0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            let ran = hook.load(Ordering::SeqCst);
            // Leave no thread, and no creation in progress, for the next
            // round or test.
            assert_eq!(set("background_thread", false), 0);
            for _ in 0..400 {
                if !purge::background::present_or_creating(Tracked::assume_new()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let stopped = !purge::background::present_or_creating(Tracked::assume_new());
            if stopped {
                purge::background::join_exited();
            }
            // Another creation in this round made a thread that exited
            // after the hook's.
            let other = ran == 1
                && purge::background::EXITS.load(Ordering::SeqCst)
                    != exited_at.load(Ordering::SeqCst);
            // A round that did not stop is checked: running another
            // would start from a thread left over.
            if !stopped || (!unused && ran != 3 && !other) {
                checked = Some((ran, rc, present, stopped));
                break;
            }
        }
        let (ran, rc, present, stopped) =
            checked.expect("no round had the enable as the only creator");
        assert_eq!(
            ran, 1,
            "hook: 0 did not run, 2 the thread did not exit before publication"
        );
        assert!(!present, "start published a thread that had already exited");
        assert_eq!(
            rc, EAGAIN,
            "the enable whose thread exited before its start published it"
        );
        assert!(stopped, "background thread did not stop");
    }

    /// The verified reads that rest on premise C2 of
    /// `verus/kani/premises.md` (the C program's name, `newp` and MIB
    /// bytes are initialised and unchanged by any other thread during the
    /// call). In verified code (inside `verus!`, in a function that is not
    /// `external_body`, `external`, `cfg(test)` or `cfg(kani)`, in the
    /// files `testutil::shipped_sources` returns), every use of
    /// `from_raw_parts` and `read_unaligned` is in one of the four helpers
    /// listed, as many times as listed; each helper is used only in the
    /// functions listed: `name_bytes` in `cstr`, which `mallctl` and
    /// `mallctlnametomib` call on their name; `load` in `Io::write` and
    /// `query_ptr` in `batch_query`, on the call's `newp`; `mib_in` in
    /// `mallctlbymib` (and the theorems that restate those entry points'
    /// bodies); and the helpers keep the contracts that tie what they read
    /// to those buffers (`c_readable` of the caller's buffer, and for
    /// `name_bytes` the call's string, with the slice borrowed from the
    /// call's token). The helpers are private to this file, so their uses
    /// are looked for here only: every mention of the name but a method
    /// (`.name`), a `fn` item, or a path to another module's item
    /// (`m::name` but for `self::`, `super::` or `mallctl::`). Known
    /// escapes: a rename through a `use .. as` outside `verus!` (its later
    /// uses are not seen), a use that a `macro_rules!` defined outside
    /// `verus!` expands to, and a kept clause moved or weakened inside its
    /// helper's item (`CONTRACTS` matches text from the `fn` line to the
    /// body's `{`). `verus/README.md`'s guard-test row records the mutants
    /// this test kills.
    #[test]
    fn c_buffer_reads_are_listed() {
        use crate::testutil::{fn_item, shipped_sources};
        const READS: [&str; 2] = ["from_raw_parts", "read_unaligned"];
        const HELPERS: [&str; 5] = ["name_bytes", "cstr", "load", "query_ptr", "mib_in"];
        const HERE: &str = "api/mallctl.rs";
        // (the function in this file, the name it uses, uses).
        const SITES: [(&str, &str, usize); 12] = [
            ("name_bytes", "from_raw_parts", 1),
            ("load", "read_unaligned", 1),
            ("query_ptr", "read_unaligned", 1),
            ("mib_in", "read_unaligned", 1),
            ("cstr", "name_bytes", 1),
            ("mallctl", "cstr", 1),
            ("mallctlnametomib", "cstr", 1),
            ("theorem_mallctl_reads_never_abort", "cstr", 1),
            ("write", "load", 1),
            ("batch_query", "query_ptr", 1),
            ("mallctlbymib", "mib_in", 1),
            ("theorem_mallctlbymib_reads_never_abort", "mib_in", 1),
        ];
        // (helper, a clause of its item, from its `fn` line to its body's
        // `{`, that ties its read to the C buffer).
        const CONTRACTS: [(&str, &str); 5] = [
            (
                "name_bytes",
                "Tracked(call): Tracked<&CCall>) -> (r: &[u8])",
            ),
            ("name_bytes", "call.reads_str(p),"),
            ("load", "c_readable(newp as *mut u8, newlen as int),"),
            ("query_ptr", "c_readable(ptrs as *mut u8, newlen as int),"),
            (
                "mib_in",
                "c_readable(mib as *mut usize, miblen as int * size_of::<usize>()),",
            ),
        ];
        // The name a `fn` item line declares.
        let fn_name = |sig: &str| -> std::string::String {
            let rest = sig.split_once("fn ").map_or("", |(_, r)| r);
            rest.split(['(', '<']).next().unwrap().into()
        };
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let mut found = Vec::new();
        let mut items: Vec<(std::string::String, std::string::String)> = Vec::new();
        for (rel, text) in shipped_sources() {
            let mut depth = 0usize;
            let mut attrs: Vec<&str> = Vec::new();
            let mut sig = "";
            let mut verified = false;
            // The item of the helper whose `fn` line was seen last, until
            // its body's `{`.
            let mut item: Option<(std::string::String, std::string::String)> = None;
            for (i, line) in text.lines().enumerate() {
                let mut t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                if t.ends_with("verus! {") {
                    depth += 1;
                    // Code before the block's first `fn` counts as verified.
                    (sig, verified) = ("", true);
                } else if t == "} // verus!" {
                    depth = depth.checked_sub(1).unwrap_or_else(|| {
                        panic!("{rel}:{}: a `verus!` close with none open", i + 1)
                    });
                }
                // Attributes, on lines of their own or before an item on
                // the same line.
                while t.starts_with("#[") {
                    let Some(end) = t.find(']') else { break };
                    attrs.push(&t[..=end]);
                    t = t[end + 1..].trim_start();
                }
                if t.is_empty() {
                    continue;
                }
                if fn_item(t) {
                    sig = t;
                    verified = !attrs.iter().any(|a| {
                        a.contains("verifier::external_body")
                            || a.contains("verifier::external]")
                            || a.contains("cfg(test)")
                            || a.contains("cfg(kani)")
                    });
                    let f = fn_name(sig);
                    if rel == HERE && HELPERS.contains(&f.as_str()) {
                        item = Some((f, std::string::String::new()));
                    }
                }
                attrs.clear();
                if let Some((f, s)) = &mut item {
                    s.push_str(t);
                    s.push('\n');
                    if t == "{" || t.ends_with(" {") {
                        items.push((core::mem::take(f), core::mem::take(s)));
                        item = None;
                    }
                }
                if depth == 0 || !verified || t.contains("assume_specification") {
                    continue;
                }
                let names = READS.iter().map(|n| (*n, false));
                let helpers = HELPERS.iter().filter(|_| rel == HERE).map(|n| (*n, true));
                for (name, helper) in names.chain(helpers) {
                    for (k, _) in t.match_indices(name) {
                        let (before, after) = (&t[..k], &t[k + name.len()..]);
                        if before.ends_with(ident) || after.starts_with(ident) {
                            continue;
                        }
                        let other_path = before.ends_with("::")
                            && !["self::", "super::", "mallctl::"]
                                .iter()
                                .any(|p| before.ends_with(p));
                        if helper
                            && (before.ends_with('.') || before.ends_with("fn ") || other_path)
                        {
                            continue;
                        }
                        found.push((
                            rel.clone(),
                            fn_name(sig),
                            name,
                            std::format!("{rel}:{}: `{t}`", i + 1),
                        ));
                    }
                }
            }
            assert_eq!(depth, 0, "{rel}: a `verus!` block left open");
        }
        for (rel, f, name, at) in &found {
            assert!(
                rel == HERE && SITES.iter().any(|(s, n, _)| f == s && name == n),
                "{at}: `{name}` in `{f}`, which is not a listed site. A verified read \
                 through it rests on premise C2 only for the C program's name, `newp` \
                 and MIB: argue the new site in verus/kani/premises.md and add it \
                 here, in one change."
            );
        }
        for (s, n, want) in SITES {
            let got = found
                .iter()
                .filter(|(_, f, name, _)| f == s && *name == n)
                .count();
            assert_eq!(
                got, want,
                "{HERE} `{s}`: got = {got} uses of `{n}`, want = {want}"
            );
        }
        for (f, clause) in CONTRACTS {
            let got: Vec<_> = items.iter().filter(|(g, _)| g == f).collect();
            assert_eq!(
                got.len(),
                1,
                "{HERE} `{f}`: got = {} items, want = 1",
                got.len()
            );
            assert!(
                got[0].1.contains(clause),
                "{HERE} `{f}`: got = item\n{}\nwant = one with `{clause}`, which ties its read to \
                 the C buffer premise C2 covers",
                got[0].1
            );
        }
    }
}

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
