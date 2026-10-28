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
//! What the token gives so far: a view of the call's string argument
//! ([`CStr`]) and shared read permissions to its bytes
//! ([`CCall::str_byte`], [`CCall::str_c_byte`]), borrowed from the token,
//! only while it reads the string ([`CCall::reads_str`]: before any write
//! through the call's buffers or call back into C, [`CCall::written`]).
//! A C caller's string is the program's memory, apart from every byte
//! challoc holds a permission to ([`c_apart`]), or one of the counters it
//! publishes for C to read ([`c_published`]). Verified code also holds the
//! usable bytes of blocks it is handing out (`vspec::user::UserMem`), in
//! the slab and huge regions, which [`c_apart`] does not exclude: a
//! contract-keeping argument lies in a block the program holds, never in
//! one the call is handing out, so those stay apart by the contract. A C
//! program that breaks the contract (a string that is not NUL-terminated,
//! wraps the address space or lies in challoc's memory other than a
//! published counter) makes the token's facts false.

#[cfg(verus_keep_ghost)]
use crate::layout::{disjoint, process_regions, shared_static_addr, shared_static_len};
#[cfg(verus_keep_ghost)]
use crate::vspec::ptr::{alloc_len, r_len};
use vstd::prelude::*;
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

/// The `len` bytes at `addr` are counters challoc publishes for C to
/// read: a thread heap's `allocated` and `deallocated` totals
/// (`Heap::snapshot`), whose addresses `mallctl`'s `thread.allocatedp`
/// and `thread.deallocatedp` hand out. They lie in the metadata mapping,
/// but no ghost permission covers them (the heap is reached through
/// references): the premise under which a read permission to them
/// ([`CCall::str_byte`]) aliases nothing verified code holds. A permission model that covered them (a linear `PointsTo<Heap>`,
/// say) would have to leave them out. Uninterpreted: nothing states which
/// bytes these are.
pub uninterp spec fn c_published(addr: int, len: int) -> bool;

/// The separation of an argument challoc only reads: apart from
/// challoc's memory ([`c_apart`]) or one of the counters it publishes
/// ([`c_published`]).
pub open spec fn c_ro_apart(addr: int, len: int) -> bool {
    c_apart(addr, len) || c_published(addr, len)
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


/// The token of one call from C through a C entry point
/// ([`CCall::entry`]): the C contract of its pointer arguments, as
/// verified code can use it. Linear (not `Copy`, no constructor here).
/// The token lends shared read permissions to the bytes of the call's
/// string ([`CCall::str_byte`], [`CCall::str_c_byte`]) for as long as it
/// is borrowed, and only while no write through the call's buffers and
/// no call back into C has begun ([`CCall::written`]).
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

    /// `a` is one of the call's pointer arguments ([`CEntry::passed`]).
    pub open spec fn has(self, a: CArg) -> bool {
        self.entry().passed(a)
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

}

} // verus!
