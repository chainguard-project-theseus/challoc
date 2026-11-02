//! The C caller's buffers: what the C contract of an entry point hands
//! challoc for one call through a pointer argument (a string, a byte
//! buffer and its length, an array of `size_t`s and its length), as one
//! linear ghost token per call, [`CCall`].
//!
//! The trust sits at the C boundary: the export wrapper of each C symbol
//! is to mint the token once from its own parameters and hand it to the
//! Rust entry point, which threads it, borrowed, to the code that reads
//! the caller's buffers and begins the writes into them (`api/mallctl.rs`,
//! `api/print.rs`). Until the export wrappers are verified, every caller
//! outside `verus!` passes a placeholder token.
//!
//! What the token gives: the facts of its arguments' contracts
//! ([`CArg::facts`], at the mint, [`CCall::fresh`]), a view of the call's
//! string argument ([`CStr`]), and shared read permissions to its bytes
//! and its `size_t`s ([`CCall::str_byte`], [`CCall::str_c_byte`],
//! [`CCall::sized`], [`CCall::counted`]), borrowed from the token, only
//! before any write through the call's buffers or call back into C
//! ([`CCall::written`]). A write into a caller's buffer needs
//! [`c_writes`], which only [`CCall::open_writes`] gives.
//! A C caller's buffer is the program's memory, apart from every byte
//! challoc holds a permission to ([`c_apart`]), or one of the counters it
//! publishes for C to read ([`c_published`]). Verified code also holds the
//! usable bytes of blocks it is handing out (`vspec::user::UserMem`), in
//! the slab and huge regions, which [`c_apart`] does not exclude: a
//! contract-keeping argument lies in a block the program holds, never in
//! one the call is handing out, so those stay apart by the contract.
//! `core::ptr::write_bytes`, which verified code never points at a C
//! caller's buffer, also needs the bytes outside those regions
//! ([`blocks_apart`]). A C program that breaks the contract (a string
//! that is not NUL-terminated, wraps the address space or lies in
//! challoc's memory other than a published counter) makes the token's
//! facts false.

#[cfg(verus_keep_ghost)]
use crate::layout::{disjoint, process_regions, shared_static_addr, shared_static_len};
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{alloc_len, in_alloc, r_len, rw_len};
use vstd::prelude::*;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::MemContents;
#[cfg(verus_keep_ghost)]
use vstd::raw_ptr::PointsTo;
use vstd::raw_ptr::Provenance;

verus! {

/// The `len` bytes at `addr` are apart from challoc's own memory: from
/// the metadata mapping (`Regions::meta_map`; the one `Unbacked` token,
/// the source of the metadata permissions verified code holds, is part
/// of it, and `Meta::free` takes back blocks of it) and from the process's
/// `Shared` static (`Shared::begin`'s permission, the source of the
/// others). The separation premise of every write into a C caller's
/// buffer; reads take [`c_ro_apart`].
pub open spec fn c_apart(addr: int, len: int) -> bool {
    let m = process_regions().meta_map;
    &&& disjoint(addr, len, m.addr as int, m.len as int)
    &&& disjoint(addr, len, shared_static_addr(), shared_static_len())
}

/// The `len` bytes at `addr` are apart from the mappings that hold user
/// blocks: the class superblock (`Regions::superblock_map`, every slab
/// slot) and the huge region (`Regions::huge_map`, every huge run). The
/// bytes of a block verified code is handing out are there, under a
/// `vspec::user::UserMem` whose byte state a write it does not see would
/// make stale. Not part of [`c_apart`]: a C caller's buffer may well be
/// a block the program holds there. The premise of `core::ptr::write_bytes`
/// (`vspec::ptr`), which verified code never uses on a C caller's buffer.
pub open spec fn blocks_apart(addr: int, len: int) -> bool {
    let r = process_regions();
    &&& disjoint(addr, len, r.superblock_map.addr as int, r.superblock_map.len as int)
    &&& disjoint(addr, len, r.huge_map.addr as int, r.huge_map.len as int)
}

/// The `len` bytes at `addr` are counters challoc publishes for C to
/// read: a thread heap's `allocated` and `deallocated` totals
/// (`Heap::snapshot`), whose addresses `mallctl`'s `thread.allocatedp`
/// and `thread.deallocatedp` hand out. They lie in the metadata mapping,
/// but no ghost permission covers them (the heap is reached through
/// references): the premise under which a read permission to them
/// ([`CCall::str_byte`], [`CCall::sized`], [`CCall::counted`]) aliases
/// nothing verified code holds. A permission model that covered them (a
/// linear `PointsTo<Heap>`, say) would have to leave them out.
/// Uninterpreted: nothing states which bytes these are.
pub uninterp spec fn c_published(addr: int, len: int) -> bool;

/// The separation of an argument challoc only reads: apart from
/// challoc's memory ([`c_apart`]) or one of the counters it publishes
/// ([`c_published`]).
pub open spec fn c_ro_apart(addr: int, len: int) -> bool {
    c_apart(addr, len) || c_published(addr, len)
}


/// The `len` bytes from `p` are a C caller's object: at most `isize::MAX`
/// bytes (an object's limit), not wrapping the address space, and inside
/// one allocation that `p` is derived from.
pub open spec fn c_object<T>(p: *mut T, len: int) -> bool {
    &&& 0 <= len <= isize::MAX
    &&& p@.addr + len <= usize::MAX
    &&& in_alloc(p, len)
}

/// The `len` bytes from `p` are a C caller's buffer ([`c_object`]), apart
/// from challoc's memory ([`c_apart`]) and, once writes through its
/// provenance are open ([`c_writes`]), valid for reads and writes from
/// every address in them to their end (`mallctl`'s `oldp`): so no write
/// (`write_unaligned`, `copy_nonoverlapping`) reaches it
/// while a read permission the call's token minted is held. As for every
/// validity fact, it holds when it is established (the call) and verified
/// code uses it right away.
pub open spec fn c_buffer<T>(p: *mut T, len: int) -> bool {
    &&& c_object(p, len)
    &&& c_apart(p@.addr as int, len)
    &&& (c_writes(p@.provenance) ==> forall|a: usize| p@.addr <= a <= p@.addr + len ==> #[trigger] rw_len(p@.provenance, a) >= p@.addr + len - a)
}

/// The `len` bytes from `p` are a C caller's buffer ([`c_object`]) that
/// challoc only reads ([`c_ro_apart`]), valid for reads only, from every
/// address in them to their end (`mallctl`'s `newp`, which may be
/// read-only memory: `r_len`, not `rw_len`).
pub open spec fn c_readable<T>(p: *mut T, len: int) -> bool {
    &&& c_object(p, len)
    &&& c_ro_apart(p@.addr as int, len)
    &&& forall|a: usize| p@.addr <= a <= p@.addr + len ==> #[trigger] r_len(p@.provenance, a) >= p@.addr + len - a
}

/// The caller's buffer from `k` bytes in, through `q`, any pointer with
/// that address and the buffer's provenance: its last `len - k` bytes are
/// valid for reads and writes (once writes through its provenance are
/// open, [`c_writes`]) and apart from challoc's memory, and from
/// its start (`k == 0`) all of them are inside the allocation.
pub proof fn lemma_c_buffer_at<T, U>(p: *mut T, len: int, q: *mut U, k: int)
    requires
        c_buffer(p, len),
        c_writes(p@.provenance),
        0 <= k <= len,
        q@.addr == p@.addr + k,
        q@.provenance == p@.provenance,
    ensures
        crate::vspec::ptr::valid_writes(q, len - k),
        crate::vspec::ptr::valid_reads(q as *const U, len - k),
        c_apart(q@.addr as int, len - k),
        k == 0 ==> in_alloc(q, len),
{
    assert(rw_len(p@.provenance, q@.addr) >= p@.addr + len - q@.addr);
}

/// [`lemma_c_buffer_at`] for a read-only buffer: its last `len - k` bytes
/// are valid for reads.
pub proof fn lemma_c_readable_at<T, U>(p: *mut T, len: int, q: *mut U, k: int)
    requires
        c_readable(p, len),
        0 <= k <= len,
        q@.addr == p@.addr + k,
        q@.provenance == p@.provenance,
    ensures
        crate::vspec::ptr::valid_reads(q as *const U, len - k),
        k == 0 ==> in_alloc(q, len),
{
    assert(r_len(p@.provenance, q@.addr) >= p@.addr + len - q@.addr);
}


/// What the C contract of an entry point says of one pointer argument.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost enum CArg {
    /// A NUL-terminated string, readable up to and including its NUL, and
    /// unchanged by C during the call until challoc calls back into C
    /// (`mallctl`'s `name`, `malloc_stats_print`'s `opts`, whose
    /// `write_cb` is C).
    Str(*const u8),
    /// `len` readable bytes (`mallctl`'s `newp` and `newlen`).
    Buffer(*mut u8, usize),
    /// A buffer and the initialised `size_t` that holds its length in
    /// bytes (`mallctl`'s `oldp` and `oldlenp`).
    Sized(*mut u8, *const usize),
    /// `n` readable `size_t`s (`mallctlbymib`'s `mib` and `miblen`).
    Words(*const usize, usize),
    /// An array of `size_t`s and the initialised `size_t` that holds its
    /// length in elements, which challoc writes back
    /// (`mallctlnametomib`'s `mibp` and `miblenp`).
    Counted(*mut usize, *const usize),
}

impl CArg {
    /// What the contract gives on its own: a read-only buffer's validity,
    /// a `size_t`'s separation, and a written-back `size_t`'s validity as
    /// a buffer of its own ([`c_buffer`]).
    pub open spec fn facts(self) -> bool {
        match self {
            CArg::Buffer(p, len) => c_readable(p, len as int),
            CArg::Sized(_, lenp) => c_ro_apart(lenp@.addr as int, vstd::layout::size_of::<usize>() as int),
            CArg::Words(p, n) => c_readable(p as *mut usize, n as int * vstd::layout::size_of::<usize>()),
            CArg::Counted(_, lenp) => c_buffer(lenp as *mut usize, vstd::layout::size_of::<usize>() as int),
            CArg::Str(_) => true,
        }
    }
}


/// The C entry point a call came through, with the buffer and length
/// arguments C passed it: ghost values of the entry point's own
/// parameters, as its export wrapper names them ([`CCall`]).
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost enum CEntry {
    /// `mallctl(name, oldp, oldlenp, newp, newlen)`.
    Mallctl { name: *const u8, oldp: *mut u8, oldlenp: *const usize, newp: *mut u8, newlen: usize },
    /// `mallctlnametomib(name, mibp, miblenp)`.
    NameToMib { name: *const u8, mibp: *mut usize, miblenp: *const usize },
    /// `mallctlbymib(mib, miblen, oldp, oldlenp, newp, newlen)`.
    ByMib { mib: *const usize, miblen: usize, oldp: *mut u8, oldlenp: *const usize, newp: *mut u8, newlen: usize },
    /// `malloc_stats_print(write_cb, cbopaque, opts)`.
    StatsPrint { opts: *const u8 },
    /// A call that hands over the block at `ptr` (`free`, `free_sized`,
    /// `free_aligned_sized`, `realloc`, `reallocarray`, their `je_` twins,
    /// `challoc_realloc_usable` and `challoc_free_usable`; jemalloc's
    /// `rallocx`, `dallocx` and `sdallocx` and their `je_` twins; the C++
    /// `operator delete` family): the pointer C passed,
    /// whose block C hands over ([`CCall::block`],
    /// `heap::live::axiom_c_claim`). It has no buffer
    /// argument.
    Frees { ptr: *mut u8 },
}

impl CEntry {
    /// `a` is one of the call's pointer arguments that carry a contract
    /// ([`CArg`]): one C passed not null (a null one carries none; the
    /// entry points test them first).
    pub open spec fn passed(self, a: CArg) -> bool {
        match self {
            CEntry::Mallctl { name, oldp, oldlenp, newp, newlen } => {
                ||| name@.addr != 0 && a == CArg::Str(name)
                ||| oldp@.addr != 0 && oldlenp@.addr != 0 && a == CArg::Sized(oldp, oldlenp)
                ||| newp@.addr != 0 && a == CArg::Buffer(newp, newlen)
            },
            CEntry::NameToMib { name, mibp, miblenp } => {
                ||| name@.addr != 0 && a == CArg::Str(name)
                ||| mibp@.addr != 0 && miblenp@.addr != 0 && a == CArg::Counted(mibp, miblenp)
            },
            CEntry::ByMib { mib, miblen, oldp, oldlenp, newp, newlen } => {
                ||| mib@.addr != 0 && a == CArg::Words(mib, miblen)
                ||| oldp@.addr != 0 && oldlenp@.addr != 0 && a == CArg::Sized(oldp, oldlenp)
                ||| newp@.addr != 0 && a == CArg::Buffer(newp, newlen)
            },
            CEntry::StatsPrint { opts } => opts@.addr != 0 && a == CArg::Str(opts),
            CEntry::Frees { .. } => false,
        }
    }

    /// The call's string argument, if it has one ([`CArg::Str`]).
    pub open spec fn string(self) -> Option<*const u8> {
        match self {
            CEntry::Mallctl { name, .. } => Some(name),
            CEntry::NameToMib { name, .. } => Some(name),
            CEntry::ByMib { .. } => None,
            CEntry::StatsPrint { opts } => Some(opts),
            CEntry::Frees { .. } => None,
        }
    }

    /// The facts of the arguments C passed ([`CArg::facts`]).
    pub open spec fn facts(self) -> bool {
        match self {
            CEntry::Mallctl { oldp, oldlenp, newp, newlen, .. } => {
                &&& (oldp@.addr != 0 && oldlenp@.addr != 0 ==> CArg::Sized(oldp, oldlenp).facts())
                &&& (newp@.addr != 0 ==> CArg::Buffer(newp, newlen).facts())
            },
            CEntry::NameToMib { mibp, miblenp, .. } => mibp@.addr != 0 && miblenp@.addr != 0 ==> CArg::Counted(mibp, miblenp).facts(),
            CEntry::ByMib { mib, miblen, oldp, oldlenp, newp, newlen } => {
                &&& (mib@.addr != 0 ==> CArg::Words(mib, miblen).facts())
                &&& (oldp@.addr != 0 && oldlenp@.addr != 0 ==> CArg::Sized(oldp, oldlenp).facts())
                &&& (newp@.addr != 0 ==> CArg::Buffer(newp, newlen).facts())
            },
            CEntry::StatsPrint { .. } => true,
            CEntry::Frees { .. } => true,
        }
    }
}

/// Ghost view of a C caller's string at one call: its bytes up to and
/// including the NUL (`bytes()`), at `addr()` with provenance `prov()`.
/// A [`CCall`] holds the one of its call ([`CCall::string`]), and
/// [`CCall::str_byte`] gives a byte's read permission.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub ghost struct CStr {
    pub addr: usize,
    pub prov: Provenance,
    pub bytes: Seq<u8>,
}

impl CStr {
    pub open spec fn addr(self) -> usize {
        self.addr
    }

    pub open spec fn prov(self) -> Provenance {
        self.prov
    }

    pub open spec fn bytes(self) -> Seq<u8> {
        self.bytes
    }

    /// The string is the one at `p`.
    pub open spec fn at<T>(self, p: *const T) -> bool {
        self.addr() == p@.addr && self.prov() == p@.provenance
    }

    /// What the C contract of a string argument gives: the bytes end at
    /// the string's NUL, its only one, are apart from challoc's memory or
    /// published ([`c_ro_apart`]), lie in one allocation and are readable.
    pub open spec fn wf(self) -> bool {
        &&& self.bytes().len() > 0
        &&& self.bytes().last() == 0
        &&& forall|i: int| 0 <= i < self.bytes().len() - 1 ==> #[trigger] self.bytes()[i] != 0
        &&& self.bytes().len() <= isize::MAX
        &&& self.addr() + self.bytes().len() <= usize::MAX
        &&& c_ro_apart(self.addr() as int, self.bytes().len() as int)
        &&& self.bytes().len() <= alloc_len(self.prov(), self.addr())
        &&& self.bytes().len() <= r_len(self.prov(), self.addr())
    }

    /// No byte before `n` is the NUL.
    pub open spec fn before_nul(self, n: int) -> bool {
        forall|i: int| 0 <= i < n ==> #[trigger] self.bytes()[i] != 0
    }

    /// No byte before `n` is the NUL, so byte `n` is in the string.
    pub proof fn lemma_in(self, n: int)
        requires
            self.wf(),
            0 <= n,
            self.before_nul(n),
        ensures
            n < self.bytes().len(),
    {
        if n >= self.bytes().len() {
            assert(self.bytes()[self.bytes().len() - 1] != 0);
        }
    }
}

/// Writes through pointers of provenance `prov` are open: the call whose
/// argument has it took its token `&mut` ([`CCall::open_writes`]), so no
/// read permission minted from the token before is still held, and the
/// token mints none after. The premise of the write bound of a C caller's
/// buffer ([`c_buffer`]), and so of every write into one (the unaligned
/// writes and `write_bytes` of `vspec::ptr`, and `copy_nonoverlapping`,
/// which require it).
/// It belongs to a provenance, not to a token:
/// it is sound with one token per proof context, which holds since only
/// the export wrappers mint them, each one, and no verified function
/// takes two. Uninterpreted: only `open_writes` states it.
pub uninterp spec fn c_writes(prov: Provenance) -> bool;

/// Trusted: what [`CCall::open_writes`] stands for (private: only it
/// takes the step, with the token taken `&mut`).
axiom fn axiom_c_writes(prov: Provenance)
    ensures
        c_writes(prov),
;


/// The token of one call from C through a C entry point
/// ([`CCall::entry`]): the C contract of its pointer arguments, as
/// verified code can use it. Linear (not `Copy`, no constructor here).
/// The token lends shared read permissions to the bytes of the call's
/// string ([`CCall::str_byte`], [`CCall::str_c_byte`]) for as long as it
/// is borrowed, and to the `size_t`s that give its buffers' lengths
/// ([`CCall::sized`], [`CCall::counted`]), only while no write through
/// the call's buffers and no call back into C has begun
/// ([`CCall::written`]). A write needs [`c_writes`], which only
/// [`CCall::open_writes`] gives, taking the token `&mut` (so every
/// permission minted before is dropped) and marking it written for good.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct CCall {
    ghost entry: CEntry,
    ghost string: CStr,
    ghost lens: spec_fn(*const usize) -> usize,
    ghost written: bool,
}

impl CCall {
    /// The entry point and the arguments C passed it.
    pub closed spec fn entry(self) -> CEntry {
        self.entry
    }

    /// The call's string argument as it is during the call until challoc
    /// writes or calls back into C (the C contract: C does not change it
    /// meanwhile), if the entry has one.
    pub closed spec fn string(self) -> CStr {
        self.string
    }

    /// A write through the call's buffers, or a call back into C, has
    /// begun.
    pub closed spec fn written(self) -> bool {
        self.written
    }


    /// The value of the call's `size_t` argument at `lenp` (`mallctl`'s
    /// `*oldlenp`, `mallctlnametomib`'s `*miblenp`) as it is during the
    /// call until challoc writes or calls back into C (the C contract: C
    /// does not change it meanwhile), what [`CCall::sized`] and
    /// [`CCall::counted`] read.
    pub closed spec fn len(self, lenp: *const usize) -> usize {
        (self.lens)(lenp)
    }
    /// `a` is one of the call's pointer arguments ([`CEntry::passed`]).
    pub open spec fn has(self, a: CArg) -> bool {
        self.entry().passed(a)
    }

    /// The block C hands over through a free ([`CEntry::Frees`]): the
    /// pointer C passed; `None` for every other entry point.
    pub open spec fn block(self) -> Option<*mut u8> {
        match self.entry() {
            CEntry::Frees { ptr } => Some(ptr),
            _ => None,
        }
    }

    /// The token reads the call's string argument `p`: no write has
    /// begun, `p` is the string C passed, and the view
    /// ([`CCall::string`]) is the string at `p`, well formed.
    pub open spec fn reads_str(self, p: *const u8) -> bool {
        &&& !self.written()
        &&& self.has(CArg::Str(p))
        &&& self.string().at(p)
        &&& self.string().wf()
    }


    /// What the export wrapper's mint gives: the token of a call through
    /// `e`, no write begun, with the facts of the arguments' contracts and
    /// the string argument's view.
    pub open spec fn fresh(self, e: CEntry) -> bool {
        &&& self.entry() == e
        &&& !self.written()
        &&& e.facts()
        &&& match e.string() {
            Some(p) => p@.addr != 0 ==> self.string().at(p) && self.string().wf(),
            None => true,
        }
    }

    /// Trusted (the C contract of a string argument): the read permission
    /// to byte `i` of the call's string `p`, through `q`, a pointer to it
    /// derived from the caller's; shared, for as long as the token is
    /// borrowed, and holding the byte the view names ([`CCall::string`]).
    /// Only while the token reads the string ([`CCall::reads_str`]: before
    /// a write or a call back into C, with the view's separation,
    /// [`CStr::wf`]).
    pub axiom fn str_byte<'a>(tracked &'a self, p: *const u8, q: *const u8, i: int) -> (tracked r: &'a PointsTo<u8>)
        requires
            self.reads_str(p),
            0 <= i < self.string().bytes().len(),
            q@.addr == self.string().addr() + i,
            q@.provenance == self.string().prov(),
        ensures
            r.ptr() == q as *mut u8,
            r.is_init(),
            r.value() == self.string().bytes()[i],
    ;

    /// [`CCall::str_byte`] through a `c_char` pointer
    /// (`malloc_stats_print`'s `opts`): the `c_char` whose bits are the
    /// byte.
    pub axiom fn str_c_byte<'a>(tracked &'a self, p: *const u8, q: *const core::ffi::c_char, i: int) -> (tracked r: &'a PointsTo<core::ffi::c_char>)
        requires
            self.reads_str(p),
            0 <= i < self.string().bytes().len(),
            q@.addr == self.string().addr() + i,
            q@.provenance == self.string().prov(),
        ensures
            r.ptr() == q as *mut core::ffi::c_char,
            r.is_init(),
            r.value() as u8 == self.string().bytes()[i],
    ;


    /// Trusted (the C contract of a sized buffer): the shared read
    /// permission to the call's `size_t` at `lenp`, initialised, for as
    /// long as the token is borrowed, holding the value the view names
    /// ([`CCall::len`]), and the buffer at `p` it gives the length of, that
    /// many bytes ([`c_buffer`]). Only before a write
    /// ([`CCall::written`]). The separation of the `size_t` is the
    /// argument's fact ([`CArg::facts`]).
    pub axiom fn sized<'a>(tracked &'a self, p: *mut u8, lenp: *const usize) -> (tracked r: &'a PointsTo<usize>)
        requires
            !self.written(),
            self.has(CArg::Sized(p, lenp)),
        ensures
            r.ptr() == lenp as *mut usize,
            r.is_init(),
            r.value() == self.len(lenp),
            c_buffer(p, r.value() as int),
    ;

    /// Trusted (the C contract of a counted array): the shared read
    /// permission to the call's initialised `size_t` at `lenp`, for as
    /// long as the token is borrowed, holding the value the view names
    /// ([`CCall::len`]), and the array at `p` it gives the length of, that
    /// many `size_t`s ([`c_buffer`]). Only before a write
    /// ([`CCall::written`]). The `size_t`'s own validity and separation
    /// are the argument's fact ([`CArg::facts`]).
    pub axiom fn counted<'a>(tracked &'a self, p: *mut usize, lenp: *const usize) -> (tracked r: &'a PointsTo<usize>)
        requires
            !self.written(),
            self.has(CArg::Counted(p, lenp)),
        ensures
            r.ptr() == lenp as *mut usize,
            r.is_init(),
            r.value() == self.len(lenp),
            c_buffer(p, r.value() as int * vstd::layout::size_of::<usize>()),
    ;

    /// End the token's reads: for the call's out-argument `a` (`mallctl`'s
    /// `oldp`, `mallctlnametomib`'s `mibp` and `miblenp`), begin writing
    /// through it, so writes through its provenance are open
    /// ([`c_writes`]; both pointers' for a counted array); for any
    /// other argument (`malloc_stats_print`'s `opts`, before it calls back
    /// into C, which may change the string) no write is opened. Either way
    /// the token mints no read permission from now on
    /// ([`CCall::written`]), and taking it `&mut` ends every borrow a
    /// permission minted before holds.
    pub proof fn open_writes(tracked &mut self, a: CArg)
        requires
            old(self).has(a),
        ensures
            final(self).entry() == old(self).entry(),
            final(self).string() == old(self).string(),
            final(self).written(),
            a matches CArg::Sized(p, _) ==> c_writes(p@.provenance),
            a matches CArg::Counted(p, _) ==> c_writes(p@.provenance),
            a matches CArg::Counted(_, lenp) ==> c_writes(lenp@.provenance),
    {
        self.written = true;
        match a {
            CArg::Sized(p, _) => axiom_c_writes(p@.provenance),
            CArg::Counted(p, lenp) => {
                axiom_c_writes(p@.provenance);
                axiom_c_writes(lenp@.provenance);
            },
            _ => {},
        }
    }
}

/// The permission to the `T` a C caller's out-pointer names, if it is not
/// null ([`COut::of`]): the token of the call for that argument, minted by
/// the export wrapper with the private `axiom_c_out` of `api/libc.rs`'s
/// `exports` module and lent to the Rust entry point, whose writes take
/// it out and put it back; the wrapper hands it back at return
/// (`axiom_out_at_return`, [`c_out`]).
///
/// The same shape as `arena::Held` (a tracked `Option` of a permission,
/// so that a `&mut` to it is ghost) but not the same thing, and not to be
/// merged with it: `Held` is an allocator-internal field's storage under
/// construction, while a `COut` is tied to the C caller's pointer
/// argument: it holds the permission exactly when that pointer is not
/// null, and it is that pointer's.
// Ghost: ordinary builds keep the type but erase the code that reads it.
#[allow(dead_code)]
pub tracked struct COut<T> {
    tracked perm: Option<vstd::raw_ptr::PointsTo<T>>,
}

impl<T> COut<T> {
    /// The permission, `None` for a null out-pointer.
    pub closed spec fn opt(self) -> Option<PointsTo<T>> {
        self.perm
    }

    /// The token is for out-pointer `p`: a permission for `p` exactly when
    /// it is not null.
    pub open spec fn of(self, p: *mut T) -> bool {
        &&& (p@.addr == 0 <==> self.opt() is None)
        &&& (self.opt() matches Some(q) ==> q.ptr() == p)
    }

    /// Take the permission out, to write through it.
    pub proof fn take(tracked &mut self) -> (tracked q: PointsTo<T>)
        requires
            old(self).opt() is Some,
        ensures
            q == old(self).opt()->0,
            final(self).opt() is None,
    {
        self.perm.tracked_take()
    }

    /// Put a permission back.
    pub proof fn put(tracked &mut self, tracked q: PointsTo<T>)
        ensures
            final(self).opt() == Some(q),
    {
        self.perm = Some(q)
    }

    /// The token for out-pointer `p` holds `v`: `*p` reads `v` through
    /// its permission, unless `p` is null.
    pub open spec fn holds(self, p: *mut T, v: T) -> bool {
        p@.addr != 0 ==> (self.opt() matches Some(q) && q.is_init() && q.value() == v)
    }
}

/// The `T` at a C caller's out-pointer `p` as the caller left it when it
/// called the export being verified (it may be uninitialized): what the
/// export's token for `p` starts with (`api/libc.rs`'s `axiom_c_out`).
/// Like `vspec::errno::c_errno`, it names one call, the one of the export
/// whose `ensures` mention it.
pub uninterp spec fn c_out_entry<T>(p: *mut T) -> MemContents<T>;

/// The `T` at a C caller's out-pointer `p` as the C program reads it once
/// the export being verified has returned: what the export's token for
/// `p` holds after its last use (`api/libc.rs`'s `axiom_out_at_return`).
pub uninterp spec fn c_out<T>(p: *mut T) -> MemContents<T>;

} // verus!
