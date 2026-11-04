//! Raw Linux syscalls (design 19).
//!
//! Everything the allocator asks the kernel for goes through this module via
//! `asm!` syscall instructions, never through libc. That keeps `errno`
//! untouched (glibc requires `free` to preserve it), keeps ld.so's early
//! `malloc` calls free of libc re-entrancy, and makes the set of kernel
//! interfaces the crate depends on explicit.
//!
//! The pure error mapping lives in `errno.rs`; everything else talks to
//! the kernel and is exercised by `cargo test` inside the dev container.

pub mod auxv;
pub mod cgroup;
pub mod errno;
pub mod features;
pub mod pac;
pub mod thp_host;
pub mod tls;
pub use errno::Errno;
use vstd::prelude::*;

mod abort;
mod nr;
mod raw;

pub use abort::*;
pub use linux::*;

// ---- constants -------------------------------------------------------
// Values are identical on x86_64 and aarch64 (both use the asm-generic
// definitions for everything below).

verus! {

pub const PROT_NONE: i32 = 0;
pub const PROT_READ: i32 = 1;
pub const PROT_WRITE: i32 = 2;

#[cfg(test)]
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
#[cfg(test)]
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_NORESERVE: i32 = 0x4000;
#[cfg(test)]
pub const MAP_FIXED_NOREPLACE: i32 = 0x10_0000;

pub const MADV_DONTNEED: i32 = 4;
pub const MADV_FREE: i32 = 8;
pub const MADV_HUGEPAGE: i32 = 14;
pub const MADV_NOHUGEPAGE: i32 = 15;
pub const MADV_GUARD_INSTALL: i32 = 102;
pub const MADV_GUARD_REMOVE: i32 = 103;

/// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, addr, len, name)`.
pub const PR_SET_VMA: i32 = 0x5356_4d41;
pub const PR_SET_VMA_ANON_NAME: usize = 0;

pub const FUTEX_WAIT: i32 = 0;
pub const FUTEX_WAKE: i32 = 1;
pub const FUTEX_PRIVATE_FLAG: i32 = 128;

pub const CLOCK_MONOTONIC: i32 = 1;
pub const CLOCK_MONOTONIC_COARSE: i32 = 6;

pub const O_RDONLY: i32 = 0;
#[cfg(test)]
pub const O_WRONLY: i32 = 1;
pub const O_CLOEXEC: i32 = 0o2_000_000;

pub const AT_FDCWD: i32 = -100;

pub const SIGABRT: i32 = 6;
const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;
const SIGSET_WORDS_BYTES: usize = 8;
/// glibc's internal real-time signals: `SIGCANCEL` (32) cancels a thread
/// and `SIGSETXID` (33) makes every thread apply a `setuid`-family call,
/// whose caller holds glibc's thread-list lock until each thread has
/// answered. A thread that keeps `SIGSETXID` blocked (one created under
/// this mask inherits it) never answers, nor does one that blocks it and
/// then waits for that lock (`pthread_create` on a user stack takes it),
/// and the caller hangs. `sigprocmask` and `pthread_sigmask` never block
/// them; a raw `rt_sigprocmask` must not either. Signal `n` is bit
/// `n - 1`.
pub const SIGCANCEL: u32 = 32;
pub const SIGSETXID: u32 = 33;
/// Every signal but glibc's two internal ones: the set
/// [`block_all_signals`] blocks.
pub const BLOCKABLE_SIGNALS: u64 = !((1u64 << (SIGCANCEL - 1)) | (1u64 << (SIGSETXID - 1)));

pub const STDERR_FILENO: i32 = 2;

} // verus!

// ---- which functions may enter the kernel ------------------------------

verus! {

/// The caller may make a system call. The `raw::syscall0`..`syscall6`
/// seams, which hold every instruction that enters the kernel, require
/// it, and so must every trusted body that can reach the kernel. So a
/// verified function can reach the kernel only if its own `requires` (or
/// its loop's invariant) carries the fact.
/// `tests::syscall_capability_only_as_a_clause` checks the source: it
/// appears only as such a clause, or in the type invariants of
/// `abort::Stderr` and `api::print::Sink`, which only their `new` builds
/// under such a clause.
///
/// Opaque, with no meaning: a capability, not a claim about the world.
/// Closed: no module outside `sys` and its submodules can reveal it.
#[verifier::opaque]
pub closed spec fn may_syscall() -> bool {
    true
}

/// Grant [`may_syscall`]. Called only at the roots of the allocator's
/// call graph: the bodies of `extern "C"` functions (the C caller lets the
/// allocator enter the kernel; outside tests no Rust code calls them) and
/// of functions that never return (only `abort_fmt`: the abort's write
/// and `SIGABRT`). `tests::syscall_grants_only_at_roots` checks the source
/// for that.
pub proof fn grant_syscalls()
    ensures
        may_syscall(),
{
    reveal(may_syscall);
}

/// The caller may abort the process: reach `abort_fmt`, which every abort
/// goes through (`abort`, the trusted bodies that can panic, and the
/// panic handler). Every function that can reach an abort requires this,
/// or one of the narrower capabilities of the aborts it reaches
/// ([`may_abort_corrupt`], [`may_abort_kernel`], [`may_abort_pending`]),
/// so a verified function that neither requires one, nor carries it in a
/// loop invariant, nor calls [`grant_aborts`] never aborts.
/// `tests::abort_capabilities_only_as_clauses` checks the source: besides
/// their definitions, the grant and `abort::relabel` (which turns a narrower
/// capability into this one), they appear only in `requires`/invariants.
///
/// Opaque, with no meaning: a capability, not a claim about the world.
/// Closed: no module outside `sys` and its submodules can reveal it.
#[verifier::opaque]
pub closed spec fn may_abort() -> bool {
    true
}

/// Grant [`may_abort`]. Called only at the roots of the allocator's call
/// graph: the bodies of `extern "C"` functions and of the panic handler
/// (the panic runtime calls it; no code in the crate does), and, unlike
/// [`grant_syscalls`], never `abort_fmt`'s.
/// `tests::abort_grants_only_at_roots` checks the source for that.
pub proof fn grant_aborts()
    ensures
        may_abort(),
{
    reveal(may_abort);
}

/// The capability of the integrity checks alone: an abort only where a
/// test of allocator state that no verified invariant constrains (a word
/// other threads update with plain atomics) fails, so only if that state
/// is corrupt. Uninterpreted, so no module can prove it: only a function
/// that requires it has it.
pub uninterp spec fn integrity_aborts() -> bool;

/// The caller may abort at an integrity check ([`integrity_aborts`]).
/// Every holder of [`may_abort`] has it.
pub open spec fn may_abort_corrupt() -> bool {
    may_abort() || integrity_aborts()
}

/// The capability of the resource-failure aborts alone: an abort only
/// where a resource the allocator needs fails it (a system call that
/// returns an error, such as an `madvise`, or metadata that the
/// reservation or the kernel cannot supply), which nothing the program
/// does rules out. Uninterpreted, as [`integrity_aborts`] is.
pub uninterp spec fn kernel_aborts() -> bool;

/// The caller may abort at a resource failure ([`kernel_aborts`]): what
/// [`abort_kernel`] requires. Every holder of [`may_abort`] has it.
pub open spec fn may_abort_kernel() -> bool {
    may_abort() || kernel_aborts()
}

/// The capability of the free paths' checks not yet proved unreachable
/// for a live block (the watermark, the live count, the slab state and
/// its shadow byte, the owner word): checks that fire on an invalid free,
/// and on a valid one only if allocator state no verified invariant
/// constrains is corrupt. Apart from [`integrity_aborts`], so a function
/// that holds only that reaches none of them. Uninterpreted, as
/// [`integrity_aborts`] is.
pub uninterp spec fn pending_aborts() -> bool;

/// The caller may abort at one of the pending checks
/// ([`pending_aborts`]): what [`abort_pending`] requires. Every holder of
/// [`may_abort`] has it.
pub open spec fn may_abort_pending() -> bool {
    may_abort() || pending_aborts()
}

/// The aborts a valid program's free may reach: a resource failure
/// ([`kernel_aborts`]) and a pending check ([`pending_aborts`]). A function
/// on the covered free paths requires [`may_abort`] or, in its place,
/// this, with what a live block gives it (its token, or a fact the token
/// implies); every other abort it reaches is proved unreachable then.
pub open spec fn valid_aborts() -> bool {
    kernel_aborts() && pending_aborts()
}

} // verus!

// ---- the calling thread's signal mask ------------------------------------

verus! {

/// The signals the kernel never blocks: `SIGKILL` (9) and `SIGSTOP` (19),
/// bits 8 and 18 of the 64-bit kernel sigset.
pub open spec fn unblockable() -> u64 {
    0x4_0100
}

/// The mask `rt_sigprocmask` leaves in force when asked for `m`: the
/// kernel silently drops `SIGKILL` and `SIGSTOP`.
pub open spec fn effective(m: u64) -> u64 {
    m & !unblockable()
}

/// Every blockable signal blocked but glibc's `SIGCANCEL` and `SIGSETXID`:
/// what [`block_all_signals`] leaves.
pub open spec fn all_blocked() -> u64 {
    effective(BLOCKABLE_SIGNALS)
}

/// `effective` is idempotent: restoring a mask the kernel held leaves
/// exactly that mask. It also keeps glibc's two internal signals clear
/// when `m` does, and [`all_blocked`] has them clear.
pub proof fn lemma_effective_idempotent(m: u64)
    ensures
        effective(effective(m)) == effective(m),
        m & !BLOCKABLE_SIGNALS == 0 ==> effective(m) & !BLOCKABLE_SIGNALS == 0,
        all_blocked() & !BLOCKABLE_SIGNALS == 0,
{
    assert(BLOCKABLE_SIGNALS == 0xFFFF_FFFE_7FFF_FFFFu64) by (compute_only);
    assert((m & !0x4_0100u64) & !0x4_0100u64 == m & !0x4_0100u64) by (bit_vector);
    assert(m & !0xFFFF_FFFE_7FFF_FFFFu64 == 0 ==> (m & !0x4_0100u64) & !0xFFFF_FFFE_7FFF_FFFFu64 == 0)
        by (bit_vector);
    assert((0xFFFF_FFFE_7FFF_FFFFu64 & !0x4_0100u64) & !0xFFFF_FFFE_7FFF_FFFFu64 == 0) by (bit_vector);
}

/// Premise (the calling thread's mask): the token stands for the signal
/// mask of the thread that calls this. It names no value, but states
/// [`SigMask::wf`] of it: the mask is one the kernel holds (`SIGKILL` and
/// `SIGSTOP` cannot be blocked) and has glibc's `SIGCANCEL` and `SIGSETXID`
/// unblocked, which holds because glibc's `sigprocmask` and
/// `pthread_sigmask` strip those two, and nothing in the process blocks
/// signals through a raw `rt_sigprocmask` (challoc's own calls,
/// [`block_all_signals`] and [`set_signal_mask`], keep them unblocked).
/// Called once at a handler's entry (`api::fork_prepare`; the test
/// `api::tests::fork_hold_is_resumed_once` counts the calls); nothing on
/// the thread changes the mask while the token is held except through it,
/// and the C caller's code does not run until the handler returns. A
/// second call would hand out a second token for the same mask; no lemma
/// relates two tokens.
pub(crate) axiom fn axiom_thread_mask() -> (tracked sig: SigMask)
    ensures
        sig.wf(),
;

} // verus!

/// The calling thread's signal-mask token, in a module of its own: its
/// field is private here, so no code in `sys` (or anywhere) builds one;
/// [`axiom_thread_mask`] hands them out, and the unverified callers pass
/// placeholders (`Tracked::assume_new()`).
mod sigmask {
    #[cfg(verus_keep_ghost)]
    use super::{BLOCKABLE_SIGNALS, effective};
    use vstd::prelude::*;

    verus! {

    /// The calling thread's signal mask (the kernel's 64-bit sigset), as
    /// [`block_all_signals`](super::block_all_signals) and
    /// [`set_signal_mask`](super::set_signal_mask) change it. Ghost: no
    /// code. Changed only through `&mut` by those two functions' trusted
    /// raw calls, so a holder knows the mask stays as they left it while
    /// they hold the token. Premises (trusted, of whoever mints one): the
    /// token stays on its thread, and nothing else on that thread changes
    /// the mask while it is held (the C caller's code does not run inside
    /// an allocator call, and `abort`'s own unblocking of `SIGABRT` never
    /// returns).
    // Ghost: built and read only by proof code.
    #[allow(dead_code)]
    pub tracked struct SigMask {
        ghost mask: u64,
    }

    impl SigMask {
        /// The mask in force on the calling thread.
        pub closed spec fn mask(self) -> u64 {
            self.mask
        }

        /// A mask the kernel can hold (without `SIGKILL` and `SIGSTOP`) and
        /// challoc can leave: without glibc's `SIGCANCEL` and `SIGSETXID`,
        /// which glibc's calls never block and challoc's raw calls never
        /// add, so a `SIG_BLOCK` of [`BLOCKABLE_SIGNALS`] leaves exactly
        /// [`all_blocked`](super::all_blocked).
        pub open spec fn wf(self) -> bool {
            &&& effective(self.mask()) == self.mask()
            &&& self.mask() & !BLOCKABLE_SIGNALS == 0
        }
    }

    } // verus!
}

pub use sigmask::SigMask;

// ---- pure helpers shared by the probes and the cgroup parsers ----------

verus! {

/// `yes` or `no`: the value of every boolean key of the feature line.
pub const fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// An ASCII decimal digit.
pub open spec fn is_digit(b: u8) -> bool {
    48 <= b <= 57
}

/// The number of leading decimal digits of `s`.
pub open spec fn digit_run(s: Seq<u8>) -> nat
    decreases s.len(),
{
    if s.len() > 0 && is_digit(s[0]) {
        1 + digit_run(s.drop_first())
    } else {
        0
    }
}

/// The value of the decimal digits `s`, most significant first.
pub open spec fn decimal_value(s: Seq<u8>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        decimal_value(s.drop_last()) * 10 + (s.last() - 48)
    }
}

/// What [`parse_decimal`] returns: the value of the leading digits of `s`
/// when there is at least one and it fits a `u64`.
pub open spec fn parse_decimal_spec(s: Seq<u8>) -> Option<u64> {
    let k = digit_run(s);
    if k == 0 {
        None
    } else if decimal_value(s.take(k as int)) <= u64::MAX {
        Some(decimal_value(s.take(k as int)) as u64)
    } else {
        None
    }
}

/// `k` digits followed by a non-digit or the end: `k` is the digit run.
pub proof fn lemma_digit_run(s: Seq<u8>, k: int)
    requires
        0 <= k <= s.len(),
        forall|j: int| 0 <= j < k ==> is_digit(#[trigger] s[j]),
        k == s.len() || !is_digit(s[k]),
    ensures
        digit_run(s) == k,
    decreases k,
{
    if k > 0 {
        lemma_digit_run(s.drop_first(), k - 1);
    }
}

/// The value of digits never shrinks as digits are appended.
pub proof fn lemma_decimal_value_prefix(s: Seq<u8>, i: int, k: int)
    requires
        0 <= i <= k <= s.len(),
        forall|j: int| 0 <= j < s.len() ==> is_digit(#[trigger] s[j]),
    ensures
        0 <= decimal_value(s.take(i)) <= decimal_value(s.take(k)),
    decreases k - i,
{
    lemma_decimal_value_nonneg(s.take(i));
    if i < k {
        lemma_decimal_value_prefix(s, i + 1, k);
        assert(s.take(i + 1).drop_last() =~= s.take(i));
    }
}

/// The value of digits is never negative.
pub proof fn lemma_decimal_value_nonneg(s: Seq<u8>)
    requires
        forall|j: int| 0 <= j < s.len() ==> is_digit(#[trigger] s[j]),
    ensures
        decimal_value(s) >= 0,
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_decimal_value_nonneg(s.drop_last());
    }
}

/// The leading decimal digits of `bytes` as a `u64`, whatever follows
/// them: `42 x` and `2097152\n` read 42 and 2097152 (a `memory.events`
/// value, a sysfs number with its newline). `None` without a digit or on
/// overflow. (The digit run and its value as `while` loops, not the
/// `take_while` and `try_fold` iterator chain: Verus does not specify
/// those adapters.)
// Verus does not support `?` on `Option` here.
#[allow(clippy::question_mark)]
pub fn parse_decimal(bytes: &[u8]) -> (r: Option<u64>)
    ensures
        r == parse_decimal_spec(bytes@),
{
    let mut digits = 0;
    while digits < bytes.len() && bytes[digits].is_ascii_digit()
        invariant
            digits <= bytes@.len(),
            forall|j: int| 0 <= j < digits ==> is_digit(#[trigger] bytes@[j]),
        decreases bytes@.len() - digits,
    {
        digits += 1;
    }
    proof {
        lemma_digit_run(bytes@, digits as int);
    }
    if digits == 0 {
        return None;
    }
    let ghost s = bytes@.take(digits as int);
    let mut acc: u64 = 0;
    let mut i = 0;
    while i < digits
        invariant
            i <= digits <= bytes@.len(),
            digits > 0,
            digit_run(bytes@) == digits,
            s == bytes@.take(digits as int),
            forall|j: int| 0 <= j < digits ==> is_digit(#[trigger] bytes@[j]),
            decimal_value(s.take(i as int)) == acc as int,
        decreases digits - i,
    {
        proof {
            assert(s.take(i + 1).drop_last() =~= s.take(i as int));
            assert(s.take(i + 1).last() == bytes@[i as int]);
            lemma_decimal_value_prefix(s, i + 1, digits as int);
            assert(s.take(digits as int) =~= s);
        }
        let d = (bytes[i] - b'0') as u64;
        proof {
            assert(decimal_value(s.take(i + 1)) == acc * 10 + d);
            assert(decimal_value(s.take(i + 1)) <= decimal_value(s));
        }
        acc = match acc.checked_mul(10) {
            Some(m) => match m.checked_add(d) {
                Some(v) => v,
                None => return None,
            },
            None => return None,
        };
        i += 1;
    }
    proof {
        assert(s.take(digits as int) =~= s);
    }
    Some(acc)
}

/// `a == b` for byte slices: the comparison the parsers make in place of
/// `match`ing a byte-string pattern, which Verus does not take.
pub fn bytes_is(a: &[u8], b: &[u8]) -> (r: bool)
    ensures
        r <==> a@ == b@,
{
    let r = a == b;
    proof {
        if r {
            assert(a@ =~= b@);
        }
    }
    r
}

/// The index of the first `b` in `s`, or `s.len()` when there is none.
pub open spec fn find_byte(s: Seq<u8>, b: u8) -> nat
    decreases s.len(),
{
    if s.len() == 0 || s[0] == b {
        0
    } else {
        1 + find_byte(s.drop_first(), b)
    }
}

/// `k` bytes other than `b`, then `b` or the end: `k` is where `b` is.
pub proof fn lemma_find_byte(s: Seq<u8>, b: u8, k: int)
    requires
        0 <= k <= s.len(),
        forall|j: int| 0 <= j < k ==> #[trigger] s[j] != b,
        k == s.len() || s[k] == b,
    ensures
        find_byte(s, b) == k,
    decreases k,
{
    if k > 0 {
        lemma_find_byte(s.drop_first(), b, k - 1);
    }
}

/// The index of the first `b` in `s` at or after `start`, or `s.len()`
/// (`s[start..].iter().position(..)`, spelled out: Verus does not specify
/// `Iterator::position`).
pub fn find_from(s: &[u8], start: usize, b: u8) -> (r: usize)
    requires
        start <= s@.len(),
    ensures
        start <= r <= s@.len(),
        r - start == find_byte(s@.skip(start as int), b),
{
    let mut i = start;
    while i < s.len() && s[i] != b
        invariant
            start <= i <= s@.len(),
            forall|j: int| start <= j < i ==> #[trigger] s@[j] != b,
        decreases s@.len() - i,
    {
        i += 1;
    }
    proof {
        let t = s@.skip(start as int);
        assert forall|j: int| 0 <= j < i - start implies #[trigger] t[j] != b by {
            assert(t[j] == s@[start + j]);
        }
        lemma_find_byte(t, b, i - start);
    }
    i
}

/// The length of the first line of `s`, without its newline.
pub open spec fn line_len(s: Seq<u8>) -> nat {
    find_byte(s, 10)
}

/// What follows the first line of `s` and its newline.
pub open spec fn after_line(s: Seq<u8>) -> Seq<u8> {
    if line_len(s) < s.len() {
        s.skip(line_len(s) as int + 1)
    } else {
        Seq::empty()
    }
}

/// One step of a line scan: the line of `s` that starts at `start` ends at
/// `end`, and the scan resumes after it.
pub proof fn lemma_line_step(s: Seq<u8>, start: int, end: int)
    requires
        0 <= start < s.len(),
        start <= end <= s.len(),
        end - start == line_len(s.skip(start)),
    ensures
        s.skip(start).take(end - start) == s.subrange(start, end),
        end < s.len() ==> after_line(s.skip(start)) == s.skip(end + 1),
        end == s.len() ==> after_line(s.skip(start)) == Seq::<u8>::empty(),
{
    assert(s.skip(start).take(end - start) =~= s.subrange(start, end));
    if end < s.len() {
        assert(s.skip(start).skip(end - start + 1) =~= s.skip(end + 1));
    }
}

/// A borrowed byte string's contents, if any.
pub open spec fn opt_view(r: Option<&[u8]>) -> Option<Seq<u8>> {
    match r {
        Some(q) => Some(q@),
        None => None,
    }
}

} // verus!

mod linux {
    #[cfg(verus_keep_ghost)]
    use super::errno::MAX_ERRNO;
    use super::errno::{Errno, decode};
    use super::{
        AT_FDCWD, BLOCKABLE_SIGNALS, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, FUTEX_PRIVATE_FLAG,
        FUTEX_WAIT, FUTEX_WAKE, O_CLOEXEC, PR_SET_VMA, PR_SET_VMA_ANON_NAME, SIG_BLOCK,
        SIG_SETMASK, SIGSET_WORDS_BYTES, SigMask, abort_fmt,
    };
    #[cfg(verus_keep_ghost)]
    use super::{all_blocked, effective, lemma_effective_idempotent};
    use super::{nr, raw};
    #[cfg(verus_keep_ghost)]
    use crate::vspec::core_int::popcount;
    use vstd::prelude::*;

    verus! {

    // ---- structs ---------------------------------------------------------

    /// `struct timespec` (64-bit `time_t` on both architectures).
    #[repr(C)]
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct Timespec {
        pub sec: i64,
        pub nsec: i64,
    }

    impl Timespec {
        /// Whole nanoseconds since the clock's epoch. Wraps after ~292 years.
        pub const fn as_nanos(self) -> (r: u64)
            ensures
                self.sec >= 0 && self.nsec >= 0 && self.sec * 1_000_000_000 + self.nsec <= u64::MAX
                    ==> r == self.sec * 1_000_000_000 + self.nsec,
        {
            (self.sec as u64)
                .wrapping_mul(1_000_000_000)
                .wrapping_add(self.nsec as u64)
        }
    }

    // ---- memory ----------------------------------------------------------

    /// `mmap(2)`. Returns the mapped address.
    ///
    /// # Safety
    /// Standard `mmap` contract; `addr` with `MAP_FIXED` replaces mappings.
    #[inline]
    pub unsafe fn mmap(
        addr: usize,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: usize,
    ) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall6(
                nr::MMAP,
                addr,
                len,
                prot as isize as usize,
                flags as isize as usize,
                fd as isize as usize,
                offset,
            )
        })
    }

    /// `munmap(2)`.
    ///
    /// # Safety
    /// Nothing may still reference the range.
    #[inline]
    pub unsafe fn munmap(addr: usize, len: usize) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall2(nr::MUNMAP, addr, len) }).map(|_| ())
    }

    /// The kernel answered an `mprotect(addr, len, prot)` with success:
    /// the word `raw::syscall3` returned for it is not an error. It says
    /// only that; `vspec::unbacked::committed` says what a successful
    /// `PROT_READ | PROT_WRITE` one does to the bytes.
    pub closed spec fn mprotect_ok(addr: usize, len: usize, prot: i32) -> bool {
        exists|w: usize| #[trigger] raw::returned3(nr::MPROTECT, addr, len, prot as isize as usize, w) && w <= usize::MAX - MAX_ERRNO
    }

    /// `mprotect(2)`. On `Ok` the kernel answered with success
    /// ([`mprotect_ok`]).
    ///
    /// # Safety
    /// Removing permissions from memory in use faults later accesses.
    #[inline]
    pub unsafe fn mprotect(addr: usize, len: usize, prot: i32) -> (r: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r is Ok ==> mprotect_ok(addr, len, prot),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall3(nr::MPROTECT, addr, len, prot as isize as usize) })
            .map(|_| ())
    }

    /// The kernel answered an `madvise(addr, len, advice)` with success:
    /// the word `raw::syscall3` returned for it is not an error. It says
    /// only that, of some such call.
    pub closed spec fn madvise_ok(addr: usize, len: usize, advice: i32) -> bool {
        exists|w: usize| #[trigger] raw::returned3(nr::MADVISE, addr, len, advice as isize as usize, w) && w <= usize::MAX - MAX_ERRNO
    }

    /// The kernel answered an `madvise(addr, len, advice)` with an error:
    /// the word `raw::syscall3` returned for it is one. It says only that,
    /// of some such call.
    pub closed spec fn madvise_err(addr: usize, len: usize, advice: i32) -> bool {
        exists|w: usize| #[trigger] raw::returned3(nr::MADVISE, addr, len, advice as isize as usize, w) && w > usize::MAX - MAX_ERRNO
    }

    /// `madvise(2)`. On `Ok` the kernel answered with success
    /// ([`madvise_ok`]), on `Err` with an error ([`madvise_err`]).
    ///
    /// # Safety
    /// `MADV_DONTNEED`/`MADV_FREE` discard contents the caller must not need.
    #[inline]
    pub unsafe fn madvise(addr: usize, len: usize, advice: i32) -> (r: Result<(), Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r is Ok ==> madvise_ok(addr, len, advice),
            r is Err ==> madvise_err(addr, len, advice),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall3(nr::MADVISE, addr, len, advice as isize as usize) })
            .map(|_| ())
    }

    /// `mseal(2)`. Forbids `munmap`, `mremap`, `mprotect` and `madvise`
    /// discards on the range for the life of the process.
    ///
    /// # Safety
    /// Irreversible; the range must never need to change again.
    #[inline]
    pub unsafe fn mseal(addr: usize, len: usize, flags: usize) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall3(nr::MSEAL, addr, len, flags) }).map(|_| ())
    }

    /// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, addr, len, name)`.
    ///
    /// # Safety
    /// `name` must be NUL-terminated and stay valid for the mapping's life
    /// (the kernel copies it, but we require a `'static` anyway to be safe).
    #[inline]
    pub unsafe fn set_vma_anon_name(
        addr: usize,
        len: usize,
        name: &'static [u8],
    ) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
            name@.len() > 0,
            name@.last() == 0,
    {
        debug_assert!(!name.is_empty() && name[name.len() - 1] == 0);
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall5(
                nr::PRCTL,
                PR_SET_VMA as isize as usize,
                PR_SET_VMA_ANON_NAME,
                addr,
                len,
                name.as_ptr() as usize,
            )
        })
        .map(|_| ())
    }

    /// Generic `prctl(2)` for options other than `PR_SET_VMA`.
    ///
    /// # Safety
    /// Option-specific.
    #[inline]
    pub unsafe fn prctl(
        option: i32,
        arg2: usize,
        arg3: usize,
        arg4: usize,
        arg5: usize,
    ) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall5(nr::PRCTL, option as isize as usize, arg2, arg3, arg4, arg5)
        })
    }

    // ---- misc ------------------------------------------------------------

    /// Kernel fact (`getrandom(2)`, Linux's `drivers/char/random.c`): a
    /// `getrandom` that does not fail returns the number of bytes it
    /// stored, at most `buflen`, unless a ptrace tracer or a seccomp
    /// supervisor rewrites the result (assumed absent). About the call `raw::syscall3` names in `r`;
    /// nothing about the bytes.
    pub broadcast axiom fn axiom_getrandom_count(buf: usize, buflen: usize, flags: usize, r: usize)
        requires
            #[trigger] raw::returned3(nr::GETRANDOM, buf, buflen, flags, r),
        ensures
            r <= usize::MAX - MAX_ERRNO ==> r <= buflen,
    ;

    /// `getrandom(2)`. Returns the number of bytes written, which may be
    /// short; callers that need the full buffer should loop.
    ///
    /// The count bound is the kernel's ([`axiom_getrandom_count`]).
    #[inline]
    pub fn getrandom(buf: &mut [u8], flags: u32) -> (r: Result<usize, Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r matches Ok(n) ==> n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
    {
        broadcast use axiom_getrandom_count;

        // SAFETY: the pointer/length pair describes a live, writable slice.
        decode(unsafe {
            raw::syscall3(
                nr::GETRANDOM,
                buf.as_mut_ptr() as usize,
                buf.len(),
                flags as usize,
            )
        })
    }

    /// Fill `buf` completely from `getrandom`, retrying short reads and
    /// `EINTR`. Fails only if the kernel refuses outright. May spin as long
    /// as the kernel keeps answering `EINTR`. (An offset into `buf` rather
    /// than a reborrowed tail, and `EINTR` as a guard rather than a pattern:
    /// Verus takes neither of those.)
    #[verifier::exec_allows_no_decreases_clause]
    pub fn getrandom_exact(buf: &mut [u8]) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
    {
        let mut filled = 0;
        while filled < buf.len()
            invariant
                crate::sys::may_syscall(),
                filled <= buf@.len(),
                buf@.len() == old(buf)@.len(),
        {
            match getrandom(&mut buf[filled..], 0) {
                Ok(0) => return Err(Errno::EIO),
                Ok(n) => filled += n,
                Err(e) if e.0 == Errno::EINTR.0 => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// `futex(2)`, the raw form behind [`futex_wait`] and [`futex_wake`].
    ///
    /// # Safety
    /// `uaddr` must be a live, 4-byte-aligned `u32`; `timeout` null or valid.
    #[inline]
    unsafe fn futex(
        uaddr: *const u32,
        op: i32,
        val: u32,
        timeout: *const Timespec,
        uaddr2: *const u32,
        val3: u32,
    ) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall6(
                nr::FUTEX,
                uaddr as usize,
                op as isize as usize,
                val as usize,
                timeout as usize,
                uaddr2 as usize,
                val3 as usize,
            )
        })
    }

    /// `futex(FUTEX_WAIT | FUTEX_PRIVATE_FLAG)`: sleep while `*uaddr == val`.
    /// `Ok(())` on wake, `Err(EAGAIN)` if the value already differed,
    /// `Err(ETIMEDOUT)` on timeout, `Err(EINTR)` on signal.
    ///
    /// # Safety
    /// `uaddr` must be a live, 4-byte-aligned `u32`.
    #[inline]
    // A closure rather than the function item: Verus does not take a
    // function item where a closure is expected.
    #[allow(clippy::redundant_closure)]
    pub unsafe fn futex_wait(
        uaddr: *const u32,
        val: u32,
        timeout: Option<&Timespec>,
    ) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
    {
        let ts = timeout.map_or(core::ptr::null(), |t| core::ptr::from_ref(t));
        // SAFETY: forwarded to the caller's contract.
        unsafe {
            futex(
                uaddr,
                FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                val,
                ts,
                core::ptr::null(),
                0,
            )
        }
        .map(|_| ())
    }

    /// `futex(FUTEX_WAKE | FUTEX_PRIVATE_FLAG)`: wake up to `n` waiters.
    /// Returns how many were woken.
    ///
    /// # Safety
    /// `uaddr` must be a live, 4-byte-aligned `u32`.
    #[inline]
    pub unsafe fn futex_wake(uaddr: *const u32, n: u32) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: forwarded to the caller's contract.
        unsafe {
            futex(
                uaddr,
                FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
                n,
                core::ptr::null(),
                core::ptr::null(),
                0,
            )
        }
    }

    /// `clock_gettime(2)`.
    #[inline]
    pub fn clock_gettime(clock: i32) -> Result<Timespec, Errno>
        requires
            crate::sys::may_syscall(),
    {
        let mut ts = Timespec { sec: 0, nsec: 0 };
        // SAFETY: `ts` is a live, writable `Timespec` matching the kernel ABI.
        decode(unsafe {
            raw::syscall2(
                nr::CLOCK_GETTIME,
                clock as isize as usize,
                core::ptr::from_mut(&mut ts) as usize,
            )
        })
        .map(|_| ts)
    }

    /// Monotonic coarse clock in nanoseconds; the only clock the allocator
    /// reads on its own behalf (design 11).
    #[inline]
    pub fn now_coarse_ns() -> u64
        requires
            crate::sys::may_syscall(),
    {
        match clock_gettime(CLOCK_MONOTONIC_COARSE) {
            Ok(ts) => ts.as_nanos(),
            // COARSE is older than our floor kernel; this is unreachable, but
            // fall back rather than abort in a timing helper.
            Err(_) => clock_gettime(CLOCK_MONOTONIC).map_or(0, Timespec::as_nanos),
        }
    }

    // ---- files -----------------------------------------------------------

    /// `openat(AT_FDCWD, path, flags | O_CLOEXEC, 0)`. `path` must be
    /// NUL-terminated.
    pub fn open(path: &[u8], flags: i32) -> Result<i32, Errno>
        requires
            crate::sys::may_syscall(),
            path@.len() > 0,
            path@.last() == 0,
    {
        debug_assert!(!path.is_empty() && path[path.len() - 1] == 0);
        // SAFETY: `path` is a live NUL-terminated byte string.
        decode(unsafe {
            raw::syscall4(
                nr::OPENAT,
                AT_FDCWD as isize as usize,
                path.as_ptr() as usize,
                (flags | O_CLOEXEC) as isize as usize,
                0,
            )
        })
        .map(|fd| fd as i32)
    }

    /// Kernel fact (`read(2)`, Linux's `fs/read_write.c`): a `read` that
    /// does not fail returns the number of bytes it stored, at most
    /// `count`, unless a ptrace tracer or a seccomp supervisor rewrites
    /// the result or a driver returns more than it was asked for (the VFS
    /// does not clamp it), all assumed absent. About the call `raw::syscall3` names in
    /// `r`; nothing about the bytes.
    pub broadcast axiom fn axiom_read_count(fd: usize, buf: usize, count: usize, r: usize)
        requires
            #[trigger] raw::returned3(nr::READ, fd, buf, count, r),
        ensures
            r <= usize::MAX - MAX_ERRNO ==> r <= count,
    ;

    /// `read(2)`.
    ///
    /// The count bound is the kernel's ([`axiom_read_count`]).
    #[inline]
    pub fn read(fd: i32, buf: &mut [u8]) -> (r: Result<usize, Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r matches Ok(n) ==> n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
    {
        broadcast use axiom_read_count;

        // SAFETY: the pointer/length pair describes a live, writable slice.
        decode(unsafe {
            raw::syscall3(
                nr::READ,
                fd as isize as usize,
                buf.as_mut_ptr() as usize,
                buf.len(),
            )
        })
    }

    /// Read as much of `buf` as the file yields, retrying `EINTR`. Returns
    /// the number of bytes read; stops at EOF or a full buffer. May spin as
    /// long as the kernel keeps answering `EINTR`.
    #[verifier::exec_allows_no_decreases_clause]
    pub fn read_full(fd: i32, buf: &mut [u8]) -> (r: Result<usize, Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r matches Ok(n) ==> n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
    {
        let mut filled = 0;
        while filled < buf.len()
            invariant
                crate::sys::may_syscall(),
                filled <= buf@.len(),
                buf@.len() == old(buf)@.len(),
        {
            match read(fd, &mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.0 == Errno::EINTR.0 => {}
                Err(e) => return Err(e),
            }
        }
        Ok(filled)
    }

    /// One small file (sysfs, cgroupfs, `/proc/self/*`) into `buf`:
    /// `open`, [`read_full`], `close`. The byte count, or `None` when the
    /// file cannot be opened (absent, or the controller is not here) or
    /// read. A file longer than `buf` is cut at `buf.len()`.
    pub fn read_small_file(path: &[u8], buf: &mut [u8]) -> (r: Option<usize>)
        requires
            crate::sys::may_syscall(),
            path@.len() > 0,
            path@.last() == 0,
        ensures
            r matches Some(n) ==> n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
    {
        let fd = open(path, super::O_RDONLY).ok()?;
        let r = read_full(fd, buf).ok();
        let _ = close(fd);
        r
    }

    /// Kernel fact (`write(2)`, Linux's `fs/read_write.c`): a `write` that
    /// does not fail returns the number of bytes it took, at most `count`,
    /// unless a ptrace tracer or a seccomp supervisor rewrites the result
    /// (assumed absent).
    /// About the call `raw::syscall3` names in `r`.
    pub broadcast axiom fn axiom_write_count(fd: usize, buf: usize, count: usize, r: usize)
        requires
            #[trigger] raw::returned3(nr::WRITE, fd, buf, count, r),
        ensures
            r <= usize::MAX - MAX_ERRNO ==> r <= count,
    ;

    /// `write(2)`.
    ///
    /// The count bound is the kernel's ([`axiom_write_count`]).
    #[inline]
    pub fn write(fd: i32, buf: &[u8]) -> (r: Result<usize, Errno>)
        requires
            crate::sys::may_syscall(),
        ensures
            r matches Ok(n) ==> n <= buf@.len(),
    {
        broadcast use axiom_write_count;

        // SAFETY: the pointer/length pair describes a live slice.
        decode(unsafe {
            raw::syscall3(
                nr::WRITE,
                fd as isize as usize,
                buf.as_ptr() as usize,
                buf.len(),
            )
        })
    }

    /// Write all of `buf`, retrying short writes and `EINTR`. Any other error
    /// is swallowed: this is used on the abort path, where there is nothing
    /// better to do. May spin as long as the kernel keeps answering `EINTR`.
    /// (An offset into `buf`, as in [`getrandom_exact`].)
    #[verifier::exec_allows_no_decreases_clause]
    pub fn write_all(fd: i32, buf: &[u8])
        requires
            crate::sys::may_syscall(),
    {
        let mut done = 0;
        while done < buf.len()
            invariant
                crate::sys::may_syscall(),
                done <= buf@.len(),
        {
            match write(fd, &buf[done..]) {
                Ok(0) => return,
                Ok(n) => done += n,
                Err(e) if e.0 == Errno::EINTR.0 => {}
                Err(_) => return,
            }
        }
    }

    /// `close(2)`.
    #[inline]
    pub fn close(fd: i32) -> Result<(), Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: no pointers.
        decode(unsafe { raw::syscall1(nr::CLOSE, fd as isize as usize) }).map(|_| ())
    }

    // ---- process ---------------------------------------------------------

    #[inline]
    pub fn getpid() -> i32
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: no arguments; cannot fail.
        unsafe { raw::syscall0(nr::GETPID) as i32 }
    }

    #[inline]
    pub fn gettid() -> i32
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: no arguments; cannot fail.
        unsafe { raw::syscall0(nr::GETTID) as i32 }
    }

    /// The calling thread's `pthread_self`: the forking thread keeps it in
    /// the child of a `fork`, where its kernel thread ID (`gettid`) is
    /// new. Trusted: libc (glibc's thread pointer); nothing is claimed of
    /// the value but that it tells threads of one process apart and is
    /// never 0, 1 or 2 (a thread descriptor's address).
    #[verifier::external_body]
    #[inline(always)]
    pub fn pthread_self() -> usize {
        // SAFETY: `pthread_self` has no preconditions and cannot fail.
        unsafe { ::libc::pthread_self() as usize }
    }

    /// Whether thread `tid` of this process has exited: `tgkill` with
    /// signal 0, which only checks that the thread exists, says `ESRCH`.
    /// (In a `fork` child every thread of the parent is gone; the forking
    /// one has a new ID.)
    // The unit tests have no scratch pool, its one caller.
    #[cfg_attr(test, allow(dead_code))]
    pub fn thread_gone(tid: u32) -> bool
        requires
            crate::sys::may_syscall(),
    {
        let pid = getpid() as usize;
        // SAFETY: signal 0 delivers nothing.
        let r = unsafe { raw::syscall3(nr::TGKILL, pid, tid as usize, 0) };
        // `ESRCH` (3 on both targets).
        r as isize == -3
    }

    /// `RLIMIT_DATA` in `<sys/resource.h>`, the same on both targets.
    const RLIMIT_DATA: usize = 2;

    /// Whether the process runs under a finite `RLIMIT_DATA` (the soft
    /// limit of `prlimit64(0, RLIMIT_DATA)`). Since Linux 4.7 the limit
    /// charges every private writable mapping, committed metadata
    /// included, so the metadata allocator keeps never-touched tables off
    /// the writable side under one (`meta.rs`). A refused call (a seccomp
    /// filter) reads as limited: the safe side.
    pub fn data_limited() -> bool
        requires
            crate::sys::may_syscall(),
    {
        // `struct rlimit64`: the soft and the hard limit.
        let mut lim = [0u64; 2];
        // SAFETY: `lim` is a writable `rlimit64` for the whole call; no
        // new limit is passed.
        let r = unsafe { raw::syscall4(nr::PRLIMIT64, 0, RLIMIT_DATA, 0, lim.as_mut_ptr() as usize) };
        match decode(r) {
            Ok(_) => lim[0] != u64::MAX,
            Err(_) => true,
        }
    }

    /// A word below `2^k` has at most `k` bits set.
    proof fn lemma_popcount_bound(x: u64, k: nat)
        requires
            x < vstd::arithmetic::power2::pow2(k),
        ensures
            popcount(x) <= k,
        decreases k,
    {
        vstd::arithmetic::power2::lemma_pow2_pos(k);
        if x != 0 {
            assert(k > 0) by {
                if k == 0 {
                    vstd::arithmetic::power2::lemma2_to64();
                }
            }
            vstd::arithmetic::power2::lemma_pow2_unfold(k);
            lemma_popcount_bound(x / 2, (k - 1) as nat);
        }
    }

    /// CPUs the calling thread may run on: the bits set in the affinity
    /// mask `sched_getaffinity(0)` returns, so `taskset` and cgroup
    /// cpusets count, not the machine (jemalloc's `malloc_ncpus`). The
    /// kernel writes its own mask width and refuses a buffer narrower than
    /// it with `EINVAL`; 8192 CPUs covers every supported kernel. 1 when
    /// the call fails (a seccomp filter, or a wider mask), never 0.
    pub fn ncpus() -> (r: u32)
        requires
            crate::sys::may_syscall(),
        ensures
            1 <= r <= 8192,
    {
        let mut mask = [0u64; 128];
        // `size_of_val(&mask)`, which Verus does not specify.
        let len = 128 * 8;
        // SAFETY: `mask` is writable for `len` bytes and outlives the call.
        let r = unsafe { raw::syscall3(nr::SCHED_GETAFFINITY, 0, len, mask.as_mut_ptr() as usize) };
        match decode(r) {
            Ok(written) => {
                let words = written.min(len).div_ceil(8);
                // `mask[..words].iter().map(|w| w.count_ones()).sum()`, spelled
                // out: Verus does not specify `Map` or `Sum`.
                let mut bits: u32 = 0;
                let mut i = 0;
                while i < words
                    invariant
                        words <= 128,
                        i <= words,
                        bits <= 64 * i,
                    decreases words - i,
                {
                    proof {
                        vstd::arithmetic::power2::lemma2_to64();
                        vstd::arithmetic::power2::lemma2_to64_rest();
                        lemma_popcount_bound(mask[i as int], 64);
                    }
                    bits += mask[i].count_ones();
                    i += 1;
                }
                bits.max(1)
            }
            Err(_) => 1,
        }
    }

    // ---- scheduling ------------------------------------------------------

    const SCHED_OTHER: i32 = 0;
    const PRIO_PROCESS: i32 = 0;

    /// A CPU mask the size of glibc's `cpu_set_t` (1024 CPUs).
    pub type CpuSet = [u64; 16];

    /// `sched_setscheduler(0, SCHED_OTHER, &{ sched_priority: 0 })`: put
    /// the calling thread on the default time-sharing policy.
    pub fn sched_set_other() -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // `struct sched_param` is one `int`.
        let param = [0i32; 1];
        // SAFETY: `param` is a live `sched_param` for the whole call; the
        // kernel only reads it.
        decode(unsafe {
            raw::syscall3(
                nr::SCHED_SETSCHEDULER,
                0,
                SCHED_OTHER as isize as usize,
                param.as_ptr() as usize,
            )
        })
    }

    /// `sched_getaffinity(pid)` into `set`. On `Ok(n)` the kernel has
    /// stored the first `n` bytes of the mask and left the rest of `set`
    /// as it was (glibc's wrapper zeroes them: start from a zeroed set).
    pub fn sched_getaffinity(pid: i32, set: &mut CpuSet) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // `size_of::<CpuSet>()`, which Verus does not specify.
        let len = 16 * 8;
        // SAFETY: `set` is writable for `len` bytes and outlives the call.
        decode(unsafe {
            raw::syscall3(
                nr::SCHED_GETAFFINITY,
                pid as isize as usize,
                len,
                set.as_mut_ptr() as usize,
            )
        })
    }

    /// `sched_setaffinity(pid, set)`.
    pub fn sched_setaffinity(pid: i32, set: &CpuSet) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // `size_of::<CpuSet>()`, as in [`sched_getaffinity`].
        let len = 16 * 8;
        // SAFETY: `set` is readable for `len` bytes and outlives the call.
        decode(unsafe {
            raw::syscall3(
                nr::SCHED_SETAFFINITY,
                pid as isize as usize,
                len,
                set.as_ptr() as usize,
            )
        })
    }

    /// `getpriority(PRIO_PROCESS, pid)`: the nice value of thread `pid`.
    /// The raw syscall returns `20 - nice` (1 to 40) on success, so unlike
    /// glibc's wrapper a failure is not confused with a nice value of -1.
    /// The conversion is glibc's: the word as an `int`, subtracted from 20.
    pub fn getpriority(pid: i32) -> Result<i32, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: no pointers.
        let r = decode(unsafe {
            raw::syscall2(
                nr::GETPRIORITY,
                PRIO_PROCESS as isize as usize,
                pid as isize as usize,
            )
        });
        match r {
            Ok(v) => {
                #[cfg_attr(verus_keep_ghost, verifier::truncate)]
                let v = v as i32;
                Ok(20i32.wrapping_sub(v))
            },
            Err(e) => Err(e),
        }
    }

    /// `setpriority(PRIO_PROCESS, pid, nice)`.
    pub fn setpriority(pid: i32, nice: i32) -> Result<usize, Errno>
        requires
            crate::sys::may_syscall(),
    {
        // SAFETY: no pointers.
        decode(unsafe {
            raw::syscall3(
                nr::SETPRIORITY,
                PRIO_PROCESS as isize as usize,
                pid as isize as usize,
                nice as isize as usize,
            )
        })
    }

    /// `rt_sigprocmask(SIG_BLOCK, all)`: block every signal on the calling
    /// thread but glibc's `SIGCANCEL` and `SIGSETXID`, as `pthread_sigmask`
    /// would, and return the mask that was in force before. The kernel
    /// silently leaves `SIGKILL` and `SIGSTOP` unblocked. `sig` is
    /// the calling thread's mask ([`SigMask`]).
    pub fn block_all_signals(Tracked(sig): Tracked<&mut SigMask>) -> (r: u64)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            old(sig).wf(),
        ensures
            r == old(sig).mask(),
            final(sig).mask() == all_blocked(),
            final(sig).wf(),
    {
        let all: u64 = BLOCKABLE_SIGNALS;
        let mut old: u64 = 0;
        // SAFETY: both pointers are live 8-byte sigsets for the whole call.
        let r = unsafe { sigprocmask_block(&all, &mut old, Tracked(sig)) };
        if let Err(e) = decode(r) {
            sigprocmask_failed("SIG_BLOCK", e);
        }
        proof {
            lemma_effective_idempotent(BLOCKABLE_SIGNALS);
        }
        old
    }

    /// `rt_sigprocmask(SIG_SETMASK, mask)`: restore a mask saved by
    /// [`block_all_signals`] on the calling thread. `sig` is the calling
    /// thread's mask ([`SigMask`]). `mask` leaves glibc's two internal
    /// signals unblocked, as every mask `block_all_signals` returns does.
    pub fn set_signal_mask(mask: u64, Tracked(sig): Tracked<&mut SigMask>)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
            mask & !BLOCKABLE_SIGNALS == 0,
        ensures
            final(sig).mask() == effective(mask),
            final(sig).wf(),
    {
        // SAFETY: `mask` is a live 8-byte sigset; the old-set pointer is null.
        let r = unsafe { sigprocmask_setmask(&mask, Tracked(sig)) };
        if let Err(e) = decode(r) {
            sigprocmask_failed("SIG_SETMASK", e);
        }
        proof {
            lemma_effective_idempotent(mask);
        }
    }

    /// The raw `rt_sigprocmask(SIG_BLOCK, all, before)`, returning the
    /// syscall's word. Trusted (the kernel's semantics): a successful call
    /// (a word `decode` takes for success) wrote the mask in force before
    /// it to `*before` and left blocked every signal of `*all`, every bit
    /// but glibc's `SIGCANCEL` and `SIGSETXID` (the kernel drops `SIGKILL`
    /// and `SIGSTOP`). `SIG_BLOCK` adds to the mask in force, so the mask
    /// after is exactly `all_blocked()` only because no thread has those
    /// two blocked: glibc's `sigprocmask` and `pthread_sigmask` refuse to
    /// block them, and challoc's raw calls block only `*all` and restore
    /// masks saved from those (a premise, as the token is). Verus does
    /// not see the kernel's write through the address. On failure nothing
    /// is said: the caller aborts.
    ///
    /// # Safety
    /// As `raw::syscall4`; both pointers are live 8-byte sigsets.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn sigprocmask_block(
        all: &u64,
        before: &mut u64,
        Tracked(sig): Tracked<&mut SigMask>,
    ) -> (r: usize)
        requires
            crate::sys::may_syscall(),
            *all == BLOCKABLE_SIGNALS,
        ensures
            r <= usize::MAX - MAX_ERRNO ==> (*final(before) == old(sig).mask() && final(sig).mask()
                == all_blocked()),
    {
        // SAFETY: guaranteed by the caller.
        unsafe {
            raw::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_BLOCK as usize,
                core::ptr::from_ref(all) as usize,
                core::ptr::from_mut(before) as usize,
                SIGSET_WORDS_BYTES,
            )
        }
    }

    /// The raw `rt_sigprocmask(SIG_SETMASK, mask, NULL)`, returning the
    /// syscall's word. Trusted (the kernel's semantics): a successful call
    /// leaves `*mask` in force, less `SIGKILL` and `SIGSTOP`. On failure
    /// nothing is said: the caller aborts.
    ///
    /// # Safety
    /// As `raw::syscall4`; `mask` is a live 8-byte sigset.
    #[verifier::external_body]
    #[inline(always)]
    unsafe fn sigprocmask_setmask(mask: &u64, Tracked(sig): Tracked<&mut SigMask>) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            r <= usize::MAX - MAX_ERRNO ==> final(sig).mask() == effective(*mask),
    {
        // SAFETY: guaranteed by the caller.
        unsafe {
            raw::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_SETMASK as usize,
                core::ptr::from_ref(mask) as usize,
                0,
                SIGSET_WORDS_BYTES,
            )
        }
    }

    /// Abort on a failed `rt_sigprocmask(how)`.
    #[cold]
    #[inline(never)]
    fn sigprocmask_failed(how: &str, e: Errno) -> !
        requires
            crate::sys::may_abort(),
    {
        broadcast use crate::vspec::fmt::lemma_errno_fmt_req;

        abort_fmt(format_args!("rt_sigprocmask({how}) failed: {e}"))
    }

    /// `exit_group(2)`: terminate every thread in the process.
    pub fn exit_group(status: i32) -> !
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
    {
        // SAFETY: no pointers; does not return.
        unsafe {
            raw::syscall1(nr::EXIT_GROUP, status as isize as usize);
        }
        // The kernel never returns from exit_group; trap if it somehow did.
        trap()
    }

    /// Architecture trap instruction, for the impossible case where the
    /// kernel returns from a terminating syscall.
    #[verifier::external_body]
    #[inline(always)]
    pub fn trap() -> !
        requires
            crate::sys::may_abort(),
    {
        // SAFETY: both instructions raise a fatal signal and nothing else.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            core::arch::asm!("ud2", options(noreturn, nostack));
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!("brk #0x1", options(noreturn, nostack));
        }
    }

    } // verus!
}

#[cfg(test)]
mod pure_tests {
    use super::{parse_decimal, yn};

    #[test]
    fn yes_no() {
        assert_eq!(yn(true), "yes");
        assert_eq!(yn(false), "no");
    }

    /// The one digit loop behind the sysfs, cgroup and THP readers: a
    /// prefix read ignores what follows the digits, a newline-only read
    /// refuses anything else, both refuse an empty prefix and overflow.
    #[test]
    fn decimal_prefixes_and_trailing_bytes() {
        assert_eq!(parse_decimal(b"2097152\n"), Some(2097152));
        assert_eq!(parse_decimal(b"0"), Some(0));
        assert_eq!(parse_decimal(b"18446744073709551615\n"), Some(u64::MAX));
        assert_eq!(parse_decimal(b"18446744073709551616\n"), None, "overflow");
        assert_eq!(parse_decimal(b""), None);
        assert_eq!(parse_decimal(b"\n"), None);
        assert_eq!(parse_decimal(b"-1\n"), None);
        assert_eq!(parse_decimal(b"x"), None);
        assert_eq!(parse_decimal(b"42 x"), Some(42));
        assert_eq!(parse_decimal(b"64K\n"), Some(64));
        assert_eq!(parse_decimal(b"1\n\n"), Some(1));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;
    use std::{env, format, vec::Vec};

    const PAGE: usize = 4096;

    /// `grant_syscalls` is called only in the body of an `extern "C"`
    /// function or of `abort_fmt`, which never returns ([`may_syscall`]'s
    /// premise).
    #[test]
    fn syscall_grants_only_at_roots() {
        // Spelled in parts, so that this test's own lines match nothing.
        let sites = crate::testutil::grant_sites(&format!("{}{}", "grant_", "syscalls"));
        assert!(!sites.is_empty());
        for (sig, at) in &sites {
            assert!(
                sig.contains("extern \"C\" fn ") || sig.starts_with("pub fn abort_fmt("),
                "{at}: a syscall grant in `{sig}`"
            );
        }
    }

    /// `grant_aborts` is called only in the body of an `extern "C"`
    /// function or of the panic handler ([`may_abort`]'s premise), never
    /// in `abort_fmt`'s.
    #[test]
    fn abort_grants_only_at_roots() {
        // Spelled in parts, so that this test's own lines match nothing.
        let sites = crate::testutil::grant_sites(&format!("{}{}", "grant_", "aborts"));
        assert!(!sites.is_empty());
        for (sig, at) in &sites {
            assert!(
                sig.contains("extern \"C\" fn ")
                    || sig.starts_with("fn panic(info: &core::panic::PanicInfo"),
                "{at}: an abort grant in `{sig}`"
            );
        }
    }

    /// `grant_user_reads` is called once in each of `purge::check_zero`
    /// and the realloc cores `api::libc::realloc_block`,
    /// `api::libc::realloc_usable_stored` and `api::jemalloc::rallocx_zeroed`,
    /// and nowhere else (the callers
    /// [`crate::vspec::user::may_read_user`]'s doc lists).
    #[test]
    fn user_read_grants_only_in_listed_callers() {
        crate::testutil::assert_only_at(
            concat!("grant_", "user_reads("),
            &[
                ("purge.rs:", "pub fn check_zero(", 1),
                ("api/libc.rs:", "pub fn realloc_block(", 1),
                ("api/libc.rs:", "pub fn realloc_usable_stored(", 1),
                ("api/jemalloc.rs:", "pub fn rallocx_zeroed(", 1),
            ],
        );
    }

    /// The user-read capability (`vspec::user::may_read_user`) is
    /// established only by its grants: it appears in an `ensures` only as
    /// `grant_user_reads`' own and as `purge::check_zero`'s on a `true`
    /// result (`r ==> ..`), and elsewhere in shipped code only as a
    /// `requires` or loop invariant clause that ends with it, besides its
    /// definition and the grant's `reveal`. So no other function hands the
    /// capability to its caller, and the grant sites
    /// [`user_read_grants_only_in_listed_callers`] pins are where it starts.
    #[test]
    fn user_read_capability_ensured_only_at_grants() {
        // Spelled in parts, so that this test's own lines match nothing.
        let cap = std::format!("{}{}", "may_", "read_user");
        let grant = std::format!("pub proof fn {}{}()", "grant_", "user_reads");
        let clause = std::format!("crate::vspec::user::{cap}(),");
        let (short, reveal) = (std::format!("{cap}(),"), std::format!("reveal({cap});"));
        let def = std::format!("pub closed spec fn {cap}() -> bool {{");
        let mut ensured = Vec::new();
        for (rel, text) in crate::testutil::shipped_sources() {
            let (mut sig, mut kw) = (std::string::String::new(), "");
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                if crate::testutil::fn_item(t) {
                    (sig, kw) = (t.into(), "");
                }
                if t == "{" || t.ends_with(" {") {
                    kw = "";
                }
                // A clause keyword starts its line, alone or before the
                // clause (`ensures x,`).
                if let Some(k) = [
                    "requires",
                    "recommends",
                    "ensures",
                    "returns",
                    "invariant",
                    "invariant_except_break",
                    "decreases",
                ]
                .into_iter()
                .find(|k| t.split_whitespace().next() == Some(*k))
                {
                    kw = k;
                }
                if !t.contains(cap.as_str()) {
                    continue;
                }
                let at = std::format!("{rel}:{}: `{t}`", i + 1);
                let user = rel == "vspec/user.rs";
                let in_grant = user && sig == grant;
                if (user && t == def) || (in_grant && t == reveal) {
                    continue;
                }
                if kw == "ensures" {
                    let grant_own = in_grant && t == short;
                    let check_zero = rel == "purge.rs"
                        && sig.starts_with("pub fn check_zero(")
                        && t == std::format!("r ==> {clause}");
                    assert!(
                        grant_own || check_zero,
                        "{at}: `{cap}` in an `ensures` of `{sig}`: only the grants establish it"
                    );
                    ensured.push(at);
                } else {
                    assert!(
                        (t.ends_with(clause.as_str()) || (user && t == short))
                            && (kw == "requires" || kw.starts_with("invariant")),
                        "{at}: `{cap}` used other than as a requires or invariant clause"
                    );
                }
            }
        }
        assert_eq!(
            ensured.len(),
            2,
            "got = {ensured:?}, want = the grant's own `ensures` and check_zero's"
        );
    }

    /// A line of the shipped sources, for the capability tests below.
    struct Line {
        /// The file, relative to `src/`.
        rel: std::string::String,
        /// `file:line: `text``.
        at: std::string::String,
        /// The `fn` item line it belongs to: the nearest one above it.
        sig: std::string::String,
        /// The clause it is in (`requires`, `ensures`, `invariant`, ..),
        /// or `""` from the body's `{` on.
        kw: &'static str,
        /// The line, trimmed.
        t: std::string::String,
    }

    /// The lines of [`crate::testutil::shipped_sources`], less comment
    /// lines and `api/restated.rs` (a test module in a file of its own).
    fn shipped_lines() -> Vec<Line> {
        let mut out = Vec::new();
        for (rel, text) in crate::testutil::shipped_sources() {
            if rel == "api/restated.rs" {
                continue;
            }
            let (mut sig, mut kw) = (std::string::String::new(), "");
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.is_empty() || t.starts_with("//") {
                    continue;
                }
                if crate::testutil::fn_item(t) {
                    (sig, kw) = (t.into(), "");
                }
                if t == "{" || t.ends_with(" {") {
                    kw = "";
                }
                // A clause keyword starts its line, alone or before the
                // clause (`ensures x,`).
                if let Some(k) = [
                    "requires",
                    "recommends",
                    "ensures",
                    "returns",
                    "invariant",
                    "invariant_except_break",
                    "decreases",
                ]
                .into_iter()
                .find(|k| t.split_whitespace().next() == Some(*k))
                {
                    kw = k;
                }
                out.push(Line {
                    rel: rel.clone(),
                    at: format!("{rel}:{}: `{t}`", i + 1),
                    sig: sig.clone(),
                    kw,
                    t: t.into(),
                });
            }
        }
        assert!(out.len() > 10_000, "the source tree was not found");
        out
    }

    /// [`may_syscall`] appears in the shipped sources only as a `requires`
    /// or loop invariant clause of its own (`crate::sys::may_syscall(),`),
    /// besides its definition, [`grant_syscalls`]' `ensures` and `reveal`,
    /// and the type invariants of `abort::Stderr` and of the report's
    /// `api::print::Sink`, which only their `new` builds, under a
    /// `requires` of it. So no function or axiom ensures it for its
    /// caller, and the grants [`syscall_grants_only_at_roots`] pins are
    /// where it starts.
    #[test]
    fn syscall_capability_only_as_a_clause() {
        // Spelled in parts, so that this test's own lines match nothing.
        let cap = format!("{}{}", "may_", "syscall");
        let clause = format!("crate::sys::{cap}(),");
        let grant = format!("pub proof fn {}{}()", "grant_", "syscalls");
        let own = [
            (
                "sys/mod.rs",
                "pub closed spec fn ",
                format!("pub closed spec fn {cap}() -> bool {{"),
            ),
            ("sys/mod.rs", grant.as_str(), format!("{cap}(),")),
            ("sys/mod.rs", grant.as_str(), format!("reveal({cap});")),
            (
                "sys/abort.rs",
                "spec fn wf(&self)",
                format!("self.len <= 512 && crate::sys::{cap}()"),
            ),
            (
                "api/print.rs",
                "spec fn wf(&self)",
                format!("self.len < 1024 && crate::sys::{cap}() && self.ord@.top() == 0"),
            ),
        ];
        let mut seen = [0; 5];
        let mut clauses = 0;
        for l in shipped_lines() {
            if !l.t.contains(cap.as_str()) {
                continue;
            }
            if let Some(k) = own
                .iter()
                .position(|(f, s, t)| l.rel == *f && l.sig.starts_with(s) && l.t == *t)
            {
                seen[k] += 1;
                continue;
            }
            assert!(
                l.t == clause && (l.kw == "requires" || l.kw.starts_with("invariant")),
                "{}: `{cap}` used other than as a requires or invariant clause `{clause}` \
                 (in `{}`, clause `{}`)",
                l.at,
                l.sig,
                l.kw
            );
            clauses += 1;
        }
        assert_eq!(seen, [1; 5], "got = {seen:?}, want = each of {own:?} once");
        assert!(clauses > 100, "got = {clauses} clauses, want = the tree's");
    }

    /// The abort capabilities ([`may_abort`], [`may_abort_corrupt`],
    /// [`may_abort_kernel`], [`may_abort_pending`], and [`valid_aborts`],
    /// which stands for two of them) appear in the shipped sources only in
    /// `requires` and loop invariant clauses, besides their definitions,
    /// [`grant_aborts`]' `ensures` and `reveal`, and those of
    /// `abort::relabel`, which only the three narrower sinks call
    /// (`abort::tests::abort_relabels_only_in_the_sinks`): no other
    /// function or axiom ensures one for its caller. The uninterpreted
    /// capabilities ([`integrity_aborts`], [`kernel_aborts`],
    /// [`pending_aborts`]) appear, besides their definitions and the
    /// bodies of the capabilities above, only as a `requires` clause of
    /// their own in a theorem's witness (`fn theorem_`).
    #[test]
    fn abort_capabilities_only_as_clauses() {
        // Spelled in parts, so that this test's own lines match nothing.
        let cap = format!("{}{}", "may_", "abort");
        let valid = format!("{}{}", "valid_", "aborts");
        let grant = format!("pub proof fn {}{}()", "grant_", "aborts");
        let relabel = format!("proof fn {}{}()", "rel", "abel");
        let uninterp = ["integrity_", "kernel_", "pending_"].map(|p| format!("{p}{}", "aborts"));
        // `sys/mod.rs`'s definitions, whose bodies name the others.
        let defs = [
            format!("pub closed spec fn {cap}() -> bool {{"),
            format!("pub open spec fn {cap}_corrupt() -> bool {{"),
            format!("pub open spec fn {cap}_kernel() -> bool {{"),
            format!("pub open spec fn {cap}_pending() -> bool {{"),
            format!("pub open spec fn {valid}() -> bool {{"),
        ];
        // Their bodies, each the one line of its definition's.
        let [i, k, p] = &uninterp;
        let bodies = [
            format!("{cap}() || {i}()"),
            format!("{cap}() || {k}()"),
            format!("{cap}() || {p}()"),
            format!("{k}() && {p}()"),
        ];
        let decls = uninterp
            .clone()
            .map(|u| format!("pub uninterp spec fn {u}() -> bool;"));
        let (mut clauses, mut witnesses) = (0, 0);
        for l in shipped_lines() {
            let names_cap = l.t.contains(cap.as_str()) || l.t.contains(valid.as_str());
            let names_uninterp = uninterp.iter().any(|u| l.t.contains(u.as_str()));
            if !names_cap && !names_uninterp {
                continue;
            }
            if l.rel == "sys/mod.rs"
                && ((defs.contains(&l.sig) && (l.t == l.sig || bodies.contains(&l.t)))
                    || decls.contains(&l.t)
                    || (l.sig == grant
                        && ((l.kw == "ensures" && l.t == format!("{cap}(),"))
                            || l.t == format!("reveal({cap});"))))
            {
                continue;
            }
            if l.rel == "sys/abort.rs"
                && l.sig == relabel
                && ((l.kw == "ensures" && l.t == format!("crate::sys::{cap}(),"))
                    || l.t == format!("reveal(crate::sys::{cap});"))
            {
                continue;
            }
            if names_uninterp {
                assert!(
                    uninterp
                        .iter()
                        .any(|u| l.t == format!("crate::sys::{u}(),"))
                        && l.kw == "requires"
                        && l.sig.contains("fn theorem_"),
                    "{}: an uninterpreted abort capability outside a witness's requires \
                     (in `{}`, clause `{}`)",
                    l.at,
                    l.sig,
                    l.kw
                );
                witnesses += 1;
            } else {
                assert!(
                    l.kw == "requires" || l.kw.starts_with("invariant"),
                    "{}: `{cap}` or `{valid}` used other than in a requires or invariant clause \
                     (in `{}`, clause `{}`)",
                    l.at,
                    l.sig,
                    l.kw
                );
                clauses += 1;
            }
        }
        assert!(clauses > 100, "got = {clauses} clauses, want = the tree's");
        assert!(witnesses > 0, "got = no witness requires");
    }

    /// A call site: `(file, fn item line start, calls)`, as for
    /// [`crate::testutil::assert_only_at`].
    type Site = (&'static str, &'static str, usize);

    /// The abort sinks below `abort_fmt` that take a narrower capability
    /// than [`may_abort`]: the integrity checks (`abort_integrity` and
    /// `vspec::check::check`, under [`may_abort_corrupt`]), the resource
    /// failures (`abort_kernel`, under [`may_abort_kernel`]) and the
    /// pending checks (`abort_pending`, under [`may_abort_pending`]), each
    /// as `(name, sites)`.
    const SINKS: &[(&str, &[Site])] = &[
        (
            concat!("abort_", "integrity"),
            &[("stats.rs", "fn huge_pages_short(", 1)],
        ),
        (
            concat!("chec", "k"),
            &[
                ("bitmap.rs", "pub fn allocated(", 1),
                ("class/table.rs", "pub fn new(", 1),
            ],
        ),
        (
            concat!("abort_", "kernel"),
            &[
                ("heap/slotstate.rs", "pub(super) fn init(", 2),
                ("heap/slotstate.rs", "pub(crate) fn commit(", 1),
                ("huge/mod.rs", "pub fn free_run(", 1),
                ("purge.rs", "fn discard(", 1),
            ],
        ),
        (
            concat!("abort_", "pending"),
            &[
                ("arena.rs", "fn unbind_unbound(", 1),
                ("class/abort.rs", "pub(super) fn never_committed(", 1),
                ("class/abort.rs", "pub(super) fn owner_mismatch(", 1),
                (
                    "class/abort.rs",
                    "pub(super) fn release_into_fresh_slab(",
                    1,
                ),
                ("class/decay.rs", "pub(super) fn walk_empty<", 1),
                ("class/verify.rs", "fn shadow_written(", 1),
                ("class/verify.rs", "pub(super) fn release_into_fresh(", 1),
                ("heap/shared.rs", "fn no_such_arena(", 1),
                ("heap/slotstate.rs", "pub(super) fn free(", 3),
                ("heap/slotstate.rs", "pub(super) fn claim(", 1),
                ("heap/slotstate.rs", "pub(super) fn pop(", 1),
                ("heap/slotstate.rs", "pub(super) fn release(", 1),
                ("heap/slotstate.rs", "pub(super) fn check_not_cached(", 1),
                ("huge/mod.rs", "fn huge_count_underflow(", 1),
                ("huge/table.rs", "pub(super) fn insert(", 1),
                ("slab.rs", "fn bad_state_byte(", 1),
                ("slab.rs", "pub fn sub_live(", 1),
            ],
        ),
    ];

    /// The calls in `t` of `name` as a whole name (`x::name(` or
    /// `name(`, not `may_name(` or `x.name(`), none if `t` declares it.
    fn sink_calls(t: &str, name: &str) -> usize {
        if t.contains(&format!("fn {name}(")) {
            return 0;
        }
        let call = format!("{name}(");
        t.match_indices(call.as_str())
            .filter(|(j, _)| {
                t[..*j]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            })
            .count()
    }

    /// The narrower aborts are called only at their check sites
    /// ([`SINKS`]), with the counts listed. A new call is a new way for a
    /// holder of the narrower capability to abort, so it fails here until
    /// it is added to the list. The test does not check that a theorem
    /// argues for a site: `verus/THEOREMS.md`'s "Valid programs never
    /// abort" does not list `heap::slotstate`'s.
    #[test]
    fn narrow_aborts_only_at_their_check_sites() {
        let lines = shipped_lines();
        for &(name, sites) in SINKS {
            // (file, fn item line) -> calls.
            let mut found: std::collections::BTreeMap<(&str, &str), usize> =
                std::collections::BTreeMap::new();
            for l in &lines {
                let n = sink_calls(&l.t, name);
                if n == 0 {
                    continue;
                }
                let Some(&(file, f, _)) = sites
                    .iter()
                    .find(|(file, f, _)| l.rel == *file && l.sig.starts_with(f))
                else {
                    panic!(
                        "{}: a call of `{name}` in `{}`, not one of its check sites",
                        l.at, l.sig
                    );
                };
                *found.entry((file, f)).or_default() += n;
            }
            for &(file, f, want) in sites {
                let got = found.get(&(file, f)).copied().unwrap_or(0);
                assert_eq!(
                    got, want,
                    "{file} `{f}`: got = {got} calls of `{name}`, want = {want}"
                );
            }
        }
    }

    /// No `use` (of any visibility, over any number of lines) outside
    /// `sys/` and `vspec/` names one of the narrower aborts ([`SINKS`]),
    /// and none inside them renames one (`as`), so a call cannot escape
    /// [`narrow_aborts_only_at_their_check_sites`] under another name. The
    /// one exception is `class/table.rs`'s import of `vspec::check::check`
    /// under its own name, whose call that test sees.
    #[test]
    fn no_use_of_the_narrow_aborts() {
        let allowed = [(
            "class/table.rs",
            concat!("use crate::vspec::chec", "k::check;"),
        )];
        let mut seen = 0;
        let mut in_use = std::string::String::new();
        for l in shipped_lines() {
            let first = l.t.split_whitespace().take(2).any(|w| w == "use");
            if !first && in_use != l.rel {
                continue;
            }
            in_use = if l.t.ends_with(';') {
                std::string::String::new()
            } else {
                l.rel.clone()
            };
            if allowed.iter().any(|(f, t)| l.rel == *f && l.t == *t) {
                seen += 1;
                continue;
            }
            // A sink's name; for `check`, a common word, the paths
            // `check::check`, `check::{` and `check::*`, or `check` as a
            // word of a `use` list's later line.
            let check = concat!("chec", "k");
            let paths = [
                format!("{check}::{check}"),
                format!("{check}::{{"),
                format!("{check}::*"),
            ];
            let names_sink = SINKS
                .iter()
                .any(|(name, _)| *name != check && l.t.contains(name))
                || paths.iter().any(|p| l.t.contains(p.as_str()))
                || (!first && l.t.split([' ', ',', '{', '}']).any(|w| w == check));
            if l.rel.starts_with("sys/") || l.rel.starts_with("vspec/") {
                assert!(
                    !(names_sink && l.t.contains(" as ")),
                    "{}: a `use` that renames a narrower abort",
                    l.at
                );
            } else {
                assert!(!names_sink, "{}: a `use` of a narrower abort", l.at);
            }
        }
        assert_eq!(
            seen,
            allowed.len(),
            "got = {seen}, want = each of {allowed:?}"
        );
    }

    /// Every trusted (`external_body`) exec function in the shipped
    /// sources that can abort requires a capability for it. "Can abort"
    /// is read from the text: the function does not return (`-> !`), or
    /// its body has one of the tokens below (`assert!`, `panic!`, an
    /// `unwrap`, a call of an abort, the trap instruction). Such a function
    /// requires the bare clause `crate::sys::may_abort(),` (a line of its
    /// own after `requires` or a `,`); a header line that names an abort
    /// capability in any other form fails, unless it is listed here as
    /// that function's compound clause, by its exact text
    /// (`assert_flushed`, `assert_refill_empty` and `assert_rekey_flushed`'s
    /// `may_abort() || *len == 0`, and `vspec::check::check`'s
    /// `may_abort_corrupt() || c`). Or it is listed as unfailing (its panic
    /// is its `requires`). Every listed function must still be one. Not
    /// seen: a panic of an index or a slice (`vspec::print::base_slab_sizes` and
    /// `layout::regions`' `word` index under their `requires`), and an
    /// abort only through a function the body calls (Verus checks a call
    /// against the callee's `requires` only in a verified body).
    #[test]
    fn trusted_aborts_require_a_capability() {
        let cap = format!("{}{}", "may_", "abort");
        let valid = format!("{}{}", "valid_", "aborts");
        // The compound clauses, each `(file, fn item line start, clause)`:
        // the abort is needed only when the asserted fact fails.
        let len_zero = concat!("crate::sys::may_", "abort() || *len == 0,");
        let compound = [
            ("heap/mod.rs", "fn assert_flushed(", len_zero),
            ("heap/mod.rs", "fn assert_refill_empty(", len_zero),
            ("heap/mod.rs", "fn assert_rekey_flushed(", len_zero),
            (
                "vspec/check.rs",
                "pub fn check(",
                concat!("crate::sys::may_", "abort_corrupt() || c,"),
            ),
        ];
        let unfailing = [("heap/others.rs", "fn assert_not_home(")];
        let aborts = [
            concat!("abo", "rt("),
            concat!("abo", "rt!("),
            concat!("abo", "rt_"),
            "assert!(",
            "assert_eq!(",
            "assert_ne!(",
            "panic!(",
            "unreachable!(",
            "todo!(",
            "unimplemented!(",
            ".unwrap()",
            ".expect(",
            "trap()",
            "exit_group(",
            "\"ud2\"",
            "\"brk #",
        ];
        let (mut trusted, mut bad) = (0, Vec::new());
        let mut used = std::collections::BTreeSet::new();
        for (rel, text) in crate::testutil::shipped_sources() {
            if rel == "api/restated.rs" {
                continue;
            }
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            for (i, &t) in lines.iter().enumerate() {
                if t != concat!("#[verifier::", "external_body]") {
                    continue;
                }
                // The item the attribute is on: the next line that is not
                // an attribute or a comment.
                let Some(f) = (i + 1..lines.len())
                    .find(|&j| !lines[j].starts_with("#[") && !lines[j].starts_with("//"))
                    .filter(|&j| crate::testutil::fn_item(lines[j]))
                else {
                    continue;
                };
                // An exec function with a body: not a ghost item, and its
                // header ends at the body's `{`, not at a `;`.
                if ["proof ", "spec ", "axiom "]
                    .iter()
                    .any(|q| lines[f].contains(&format!("{q}fn ")))
                {
                    continue;
                }
                let Some(b) = (f..lines.len()).find(|&j| {
                    let l = lines[j];
                    !l.starts_with("//") && (l == "{" || l.ends_with('{') || l.ends_with(';'))
                }) else {
                    continue;
                };
                if lines[b].ends_with(';') {
                    continue;
                }
                let mut depth = 0i32;
                let mut end = b;
                for (j, l) in lines.iter().enumerate().skip(b) {
                    if !l.starts_with("//") {
                        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                    }
                    end = j;
                    if depth <= 0 {
                        break;
                    }
                }
                let header = lines[f..b].join(" ");
                let body = lines[b..=end]
                    .iter()
                    .filter(|l| !l.starts_with("//"))
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace("debug_assert", "");
                let ends = lines[f..=b].join(" ").contains(") -> !");
                if !ends && !aborts.iter().any(|a| body.contains(a)) {
                    continue;
                }
                trusted += 1;
                let at = format!("{rel}:{}: `{}`", f + 1, lines[f]);
                // Whether `l` names this function; its entry is marked used.
                let mut listed = |l: &[(&'static str, &'static str)]| {
                    let hit = l.iter().find(|(p, s)| *p == rel && lines[f].starts_with(s));
                    if let Some(&e) = hit {
                        used.insert(e);
                    }
                    hit.is_some()
                };
                // Each header line that names an abort capability: a bare
                // clause of `requires`, starting a clause, or the listed
                // compound clause of this function.
                let (mut kw, mut prev) = ("", "");
                let (mut granted, mut other) = (false, false);
                for &h in lines[f..b].iter().filter(|l| !l.starts_with("//")) {
                    let word = h.split_whitespace().next();
                    if let Some(k) = ["requires", "ensures", "recommends", "decreases", "returns"]
                        .into_iter()
                        .find(|k| word == Some(*k))
                    {
                        kw = k;
                    }
                    if h.contains(cap.as_str()) || h.contains(valid.as_str()) {
                        let fresh = kw == "requires" && (prev == "requires" || prev.ends_with(','));
                        let ok = (fresh && h == format!("crate::sys::{cap}(),"))
                            || compound
                                .iter()
                                .any(|&(p, s, c)| h == c && listed(&[(p, s)]));
                        granted |= ok;
                        other |= !ok;
                    }
                    prev = h;
                }
                if !((granted && !other) || listed(&unfailing)) {
                    bad.push(format!(
                        "{at}: a trusted body that can abort, without a bare `{cap}` clause \
                         or its listed compound one: `{header}`"
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        for e in unfailing
            .into_iter()
            .chain(compound.map(|(p, s, _)| (p, s)))
        {
            assert!(
                used.contains(&e),
                "{e:?}: listed, but no trusted body that can abort"
            );
        }
        assert!(
            trusted > 5,
            "got = {trusted} trusted bodies that abort, want = the tree's"
        );
    }

    /// A fresh read/write private anonymous mapping of `len` bytes, for
    /// this module's tests and `features::linux_tests`.
    pub(super) fn map_anon(len: usize) -> usize {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        unsafe {
            mmap(
                0,
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
            .unwrap()
        }
    }

    fn map_page() -> usize {
        map_anon(PAGE)
    }

    #[test]
    fn mmap_mprotect_madvise_munmap() {
        let addr = map_page();
        assert_eq!(addr % PAGE, 0);
        // SAFETY: `addr` is our fresh read/write page.
        unsafe {
            let p = addr as *mut u8;
            p.write(0xA5);
            assert_eq!(p.read(), 0xA5);
            mprotect(addr, PAGE, PROT_READ).unwrap();
            assert_eq!(p.read(), 0xA5);
            mprotect(addr, PAGE, PROT_READ | PROT_WRITE).unwrap();
            madvise(addr, PAGE, MADV_DONTNEED).unwrap();
            assert_eq!(p.read(), 0, "MADV_DONTNEED must zero anonymous memory");
            munmap(addr, PAGE).unwrap();
        }
    }

    #[test]
    fn mmap_errors_decode() {
        // SAFETY: invalid arguments never create a mapping.
        let r = unsafe { mmap(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
        assert_eq!(r, Err(Errno::EINVAL));
        // SAFETY: fd 12345 is not open; MAP_SHARED without ANONYMOUS needs one.
        let r = unsafe { mmap(0, PAGE, PROT_READ, MAP_SHARED, 12345, 0) };
        assert_eq!(r, Err(Errno::EBADF));
        // SAFETY: mprotect on an unmapped range changes nothing.
        let r = unsafe { mprotect(usize::MAX - 0xFFFF, PAGE, PROT_READ) };
        assert!(
            matches!(r, Err(Errno::ENOMEM) | Err(Errno::EINVAL)),
            "{r:?}"
        );
    }

    #[test]
    fn mmap_fixed_noreplace_refuses_overlap() {
        let addr = map_page();
        // SAFETY: MAP_FIXED_NOREPLACE never clobbers; the kernel either
        // returns EEXIST or (pre-4.17) a different address.
        let r = unsafe {
            mmap(
                addr,
                PAGE,
                PROT_READ,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        match r {
            Err(Errno::EEXIST) => {}
            Ok(other) => {
                assert_ne!(other, addr);
                // SAFETY: we own `other`.
                unsafe { munmap(other, PAGE).unwrap() };
            }
            Err(e) => panic!("unexpected {e}"),
        }
        // SAFETY: we own `addr`.
        unsafe { munmap(addr, PAGE).unwrap() };
    }

    /// `block_all_signals` blocks every signal but glibc's `SIGCANCEL` (32)
    /// and `SIGSETXID` (33): a thread with `SIGSETXID` blocked never answers
    /// the broadcast of a `setuid`-family call, which then waits forever.
    #[test]
    fn block_all_signals_leaves_glibc_internal_signals_unblocked() {
        std::thread::spawn(|| {
            let before = block_all_signals(Tracked::assume_new());
            let during = block_all_signals(Tracked::assume_new());
            set_signal_mask(before, Tracked::assume_new());
            assert_eq!(during & (1 << 31), 0, "SIGCANCEL blocked: {during:#x}");
            assert_eq!(during & (1 << 32), 0, "SIGSETXID blocked: {during:#x}");
            for sig in [1u32, 2, 10, 15, 17, 31, 34, 64] {
                assert_ne!(during & (1 << (sig - 1)), 0, "signal {sig} not blocked");
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn mseal_blocks_munmap_when_supported() {
        let addr = map_page();
        // SAFETY: sealing a page we never reuse is harmless (it leaks one page
        // for the life of the test process).
        match unsafe { mseal(addr, PAGE, 0) } {
            Ok(()) => {
                // SAFETY: the kernel refuses; nothing changes.
                assert_eq!(unsafe { munmap(addr, PAGE) }, Err(Errno::EPERM));
                assert_eq!(
                    // SAFETY: as above.
                    unsafe { mprotect(addr, PAGE, PROT_NONE) },
                    Err(Errno::EPERM)
                );
                // MADV_DONTNEED on a sealed writable anonymous mapping is
                // allowed; discarding is only refused for read-only ranges.
                // SAFETY: the test's own anonymous page; discarding only zeroes it.
                unsafe { madvise(addr, PAGE, MADV_DONTNEED).unwrap() };
            }
            Err(Errno::ENOSYS) => {
                std::eprintln!("mseal: ENOSYS on this kernel, skipping");
                // SAFETY: we own `addr`.
                unsafe { munmap(addr, PAGE).unwrap() };
            }
            Err(e) => panic!("mseal: unexpected {e}"),
        }
    }

    #[test]
    fn vma_naming_shows_in_maps_when_supported() {
        let addr = map_page();
        // SAFETY: `addr` is our mapping and the name is static and NUL-terminated.
        match unsafe { set_vma_anon_name(addr, PAGE, b"challoc:test\0") } {
            Ok(()) => {
                let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
                let line = maps
                    .lines()
                    .find(|l| l.starts_with(&format!("{addr:x}-")))
                    .expect("mapping present in maps");
                assert!(line.contains("[anon:challoc:test]"), "{line}");
            }
            Err(Errno::EINVAL) => std::eprintln!("PR_SET_VMA_ANON_NAME: EINVAL, skipping"),
            Err(e) => panic!("prctl: unexpected {e}"),
        }
        // SAFETY: we own `addr`.
        unsafe { munmap(addr, PAGE).unwrap() };
        // Unknown prctl option must decode as EINVAL.
        // SAFETY: option 0 is invalid and has no effect.
        assert_eq!(unsafe { prctl(0, 0, 0, 0, 0) }, Err(Errno::EINVAL));
    }

    #[test]
    fn getrandom_fills() {
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        getrandom_exact(&mut a).unwrap();
        getrandom_exact(&mut b).unwrap();
        assert_ne!(a, [0u8; 64]);
        assert_ne!(a, b);
        assert_eq!(getrandom(&mut [], 0), Ok(0));
        assert_eq!(getrandom(&mut a, 0xFFFF_FFFF), Err(Errno::EINVAL));
    }

    #[test]
    fn clocks_advance() {
        let a = clock_gettime(CLOCK_MONOTONIC).unwrap();
        assert!(a.nsec >= 0 && a.nsec < 1_000_000_000);
        let c = clock_gettime(CLOCK_MONOTONIC_COARSE).unwrap();
        assert!(c.nsec >= 0 && c.nsec < 1_000_000_000);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = clock_gettime(CLOCK_MONOTONIC).unwrap();
        assert!(b.as_nanos() > a.as_nanos());
        assert!(now_coarse_ns() > 0);
        assert_eq!(clock_gettime(12345), Err(Errno::EINVAL));
    }

    #[test]
    fn files_open_read_close() {
        let fd = open(b"/proc/self/comm\0", O_RDONLY).unwrap();
        assert!(fd >= 0);
        let mut buf = [0u8; 64];
        let n = read_full(fd, &mut buf).unwrap();
        assert!(n > 0 && n < 64);
        assert_eq!(buf[n - 1], b'\n');
        close(fd).unwrap();
        assert_eq!(close(fd), Err(Errno::EBADF));
        assert_eq!(
            open(b"/nonexistent/challoc\0", O_RDONLY),
            Err(Errno::ENOENT)
        );
        // O_CLOEXEC is forced on.
        let fd = open(b"/dev/null\0", O_RDONLY).unwrap();
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
        let flags_line = info.lines().find(|l| l.starts_with("flags:")).unwrap();
        let oflags =
            usize::from_str_radix(flags_line.split_whitespace().nth(1).unwrap(), 8).unwrap();
        assert_ne!(oflags & (O_CLOEXEC as usize), 0, "{flags_line}");
        close(fd).unwrap();
    }

    /// The shared small-file reader: the count of a real file, a cut at
    /// the buffer, `None` for an absent path and for a read that fails
    /// (a directory opens but does not read).
    #[test]
    fn small_files_read_whole_cut_or_not_at_all() {
        let mut buf = [0u8; 64];
        let n = read_small_file(b"/proc/self/comm\0", &mut buf).unwrap();
        assert!(n > 0 && n < 64 && buf[n - 1] == b'\n', "{n}");
        assert_eq!(
            &buf[..n],
            std::fs::read("/proc/self/comm").unwrap().as_slice()
        );
        let mut two = [0u8; 2];
        assert_eq!(read_small_file(b"/proc/self/comm\0", &mut two), Some(2));
        assert_eq!(two, buf[..2]);
        assert_eq!(read_small_file(b"/nonexistent/challoc\0", &mut buf), None);
        assert_eq!(
            read_small_file(b"/proc\0", &mut buf),
            None,
            "a directory opens and fails to read"
        );
        assert_eq!(read_small_file(b"/dev/null\0", &mut buf), Some(0));
    }

    #[test]
    fn write_to_devnull_and_bad_fd() {
        let fd = open(b"/dev/null\0", O_WRONLY).unwrap();
        assert_eq!(write(fd, b"hello"), Ok(5));
        write_all(fd, b"more");
        close(fd).unwrap();
        assert_eq!(write(-1, b"x"), Err(Errno::EBADF));
        // write_all must swallow errors rather than loop.
        write_all(-1, b"x");
    }

    #[test]
    fn futex_wake_and_mismatch() {
        let word: u32 = 1;
        // SAFETY: `word` is a live aligned u32.
        unsafe {
            assert_eq!(futex_wake(&word, 1), Ok(0));
            assert_eq!(futex_wait(&word, 2, None), Err(Errno::EAGAIN));
            let ts = Timespec {
                sec: 0,
                nsec: 5_000_000,
            };
            // A stray signal (seen under emulation) legitimately yields EINTR;
            // retry until the timeout path is exercised.
            loop {
                match futex_wait(&word, 1, Some(&ts)) {
                    Err(Errno::EINTR) => continue,
                    r => {
                        assert_eq!(r, Err(Errno::ETIMEDOUT));
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn futex_wakes_a_sleeping_thread() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static WORD: AtomicU32 = AtomicU32::new(0);
        WORD.store(0, Ordering::SeqCst);
        let waiter = std::thread::spawn(|| {
            let ts = Timespec { sec: 5, nsec: 0 };
            loop {
                if WORD.load(Ordering::SeqCst) != 0 {
                    return true;
                }
                // SAFETY: static aligned u32.
                match unsafe { futex_wait(WORD.as_ptr(), 0, Some(&ts)) } {
                    Ok(()) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => {}
                    Err(Errno::ETIMEDOUT) => return false,
                    Err(e) => panic!("futex_wait: {e}"),
                }
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        WORD.store(1, Ordering::SeqCst);
        // SAFETY: static aligned u32.
        unsafe { futex_wake(WORD.as_ptr(), 1).unwrap() };
        assert!(waiter.join().unwrap(), "waiter timed out");
    }

    #[test]
    fn pid_and_tid() {
        assert_eq!(getpid() as u32, std::process::id());
        assert!(gettid() > 0);
        // The main thread of a process has tid == pid; a spawned one does not.
        let other = std::thread::spawn(gettid).join().unwrap();
        assert_ne!(other, gettid());
    }

    #[test]
    fn errno_display_from_syscall() {
        assert_eq!(format!("{}", close(-1).unwrap_err()), "EBADF");
    }

    #[test]
    fn ncpus_counts_the_affinity_mask() {
        let n = ncpus();
        assert!(n >= 1);
        // The mask can only be narrower than the configured CPU count.
        // SAFETY: `sysconf` has no preconditions.
        let conf = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
        assert!(
            conf >= 1 && n as i64 <= conf,
            "ncpus {n}, _SC_NPROCESSORS_CONF {conf}"
        );
        // SAFETY: a zeroed `cpu_set_t` is a valid empty set; the call writes
        // at most `size_of::<cpu_set_t>()` bytes into it, and `CPU_COUNT`
        // only reads it.
        let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
        assert_eq!(
            // SAFETY: as above.
            unsafe {
                libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set)
            },
            0
        );
        // SAFETY: as above.
        assert_eq!(n as i32, unsafe { libc::CPU_COUNT(&set) });
    }

    /// The raw scheduling wrappers read what glibc's do: the affinity mask
    /// of `sched_getaffinity` and the nice value of `getpriority`.
    #[test]
    fn sched_wrappers_match_libc() {
        let pid = getpid();
        let mut set: CpuSet = [0u64; 16];
        assert!(sched_getaffinity(pid, &mut set).is_ok());
        // SAFETY: a zeroed `cpu_set_t` is a valid empty set, and the call
        // writes at most `size_of::<cpu_set_t>()` bytes into it.
        let mut libc_set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
        let size = core::mem::size_of::<libc::cpu_set_t>();
        assert_eq!(size, core::mem::size_of::<CpuSet>());
        assert_eq!(
            // SAFETY: as above.
            unsafe { libc::sched_getaffinity(pid, size, &mut libc_set) },
            0
        );
        // SAFETY: both are `size` bytes of plain data.
        let libc_words: CpuSet = unsafe { core::mem::transmute(libc_set) };
        assert_eq!(set, libc_words);
        // SAFETY: `getpriority` has no memory preconditions.
        let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t) };
        assert_eq!(getpriority(pid), Ok(nice));
    }

    /// Cross-check `nr.rs` against the kernel headers via a tiny C program.
    #[test]
    fn syscall_numbers_match_kernel_headers() {
        let root = env!("CARGO_MANIFEST_DIR");
        let src = format!("{root}/tests/c/sysnr.c");
        let out = format!("{}/sysnr-{}", env::temp_dir().display(), std::process::id());
        let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
        let status = Command::new(&cc).args([&src, "-o", &out]).status().unwrap();
        assert!(status.success(), "compiling {src} failed");
        let output = Command::new(&out).output().unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(output.status.success());
        let text = std::str::from_utf8(&output.stdout).unwrap();
        let mut seen = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let name = parts.next().unwrap();
            let value = parts.next().unwrap();
            let (_, ours) = super::nr::TABLE
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("{name} printed by sysnr.c but not in nr.rs"));
            if value == "-" {
                std::eprintln!("sysnr: header lacks __NR_{name}, cannot cross-check");
            } else {
                assert_eq!(value.parse::<usize>().unwrap(), *ours, "__NR_{name}");
            }
            seen.push(std::string::String::from(name));
        }
        for (name, _) in super::nr::TABLE {
            assert!(
                seen.iter().any(|s| s == name),
                "{name} missing from sysnr.c"
            );
        }
    }
}
