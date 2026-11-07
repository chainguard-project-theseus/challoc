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
/// `PROT_READ | PROT_WRITE`.
pub const PROT_RW: i32 = PROT_READ | PROT_WRITE;

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
/// `MADV_POPULATE_WRITE` (Linux 5.14, `include/uapi/asm-generic/mman-common.h`).
pub const MADV_POPULATE_WRITE: i32 = 23;

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

/// The number of leading decimal digits of `bytes`: its [`digit_run`].
/// (A `while` loop, not `iter().take_while(..).count()`: Verus does not
/// specify those adapters.)
pub fn leading_digits(bytes: &[u8]) -> (k: usize)
    ensures
        k <= bytes@.len(),
        k == digit_run(bytes@),
        forall|j: int| 0 <= j < k ==> is_digit(#[trigger] bytes@[j]),
{
    let mut k = 0;
    while k < bytes.len() && bytes[k].is_ascii_digit()
        invariant
            k <= bytes@.len(),
            forall|j: int| 0 <= j < k ==> is_digit(#[trigger] bytes@[j]),
        decreases bytes@.len() - k,
    {
        k += 1;
    }
    proof {
        lemma_digit_run(bytes@, k as int);
    }
    k
}

/// The leading decimal digits of `bytes` as a `u64`, whatever follows
/// them: `42 x` and `2097152\n` read 42 and 2097152 (a `memory.events`
/// value, a sysfs number with its newline). `None` without a digit or on
/// overflow. (The value as a `while` loop, not a `try_fold`: Verus does
/// not specify that adapter.)
// Verus does not support `?` on `Option` here.
#[allow(clippy::question_mark)]
pub fn parse_decimal(bytes: &[u8]) -> (r: Option<u64>)
    ensures
        r == parse_decimal_spec(bytes@),
{
    let digits = leading_digits(bytes);
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

    /// The glibc function that shares a name with an item of this module.
    mod glibc {
        unsafe extern "C" {
            /// glibc's `pthread_self(3)`, with the `libc` crate's signature
            /// and the same symbol (premise C12 of
            /// `verus/kani/premises.md`). Declared in this crate rather
            /// than taken from the `libc` crate so that `kani_self` can
            /// stub it: Kani 0.68 stubs a foreign function only where this
            /// crate declares it, and silently ignores a stub on
            /// `libc::pthread_self`. In a module of its own because the
            /// trusted wrapper that calls it, `super::pthread_self`, has its
            /// name.
            pub(super) fn pthread_self() -> libc::pthread_t;
        }
    }

    verus! {

    /// A C `int` argument as the syscall word: sign-extended, as the
    /// kernel ABI takes it.
    #[inline(always)]
    const fn arg(x: i32) -> (w: usize)
        ensures
            w == x as isize as usize,
    {
        x as isize as usize
    }

    /// `r` without its success value (`r.map(|_| ())`, spelled out so the
    /// contract says what it keeps).
    #[inline(always)]
    fn ok_unit(r: Result<usize, Errno>) -> (u: Result<(), Errno>)
        ensures
            u is Ok <==> r is Ok,
            r matches Err(e) ==> u == Err::<(), Errno>(e),
    {
        match r {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

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
                arg(prot),
                arg(flags),
                arg(fd),
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
        ok_unit(decode(unsafe { raw::syscall2(nr::MUNMAP, addr, len) }))
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
        ok_unit(decode(unsafe { raw::syscall3(nr::MPROTECT, addr, len, arg(prot)) }))
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
        ok_unit(decode(unsafe { raw::syscall3(nr::MADVISE, addr, len, arg(advice)) }))
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
        ok_unit(decode(unsafe { raw::syscall3(nr::MSEAL, addr, len, flags) }))
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
        ok_unit(decode(unsafe {
            raw::syscall5(
                nr::PRCTL,
                arg(PR_SET_VMA),
                PR_SET_VMA_ANON_NAME,
                addr,
                len,
                name.as_ptr() as usize,
            )
        }))
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
            raw::syscall5(nr::PRCTL, arg(option), arg2, arg3, arg4, arg5)
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
                Err(e) if e == Errno::EINTR => {}
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
                arg(op),
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
        ok_unit(unsafe {
            futex(
                uaddr,
                FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                val,
                ts,
                core::ptr::null(),
                0,
            )
        })
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
                arg(clock),
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
                arg(AT_FDCWD),
                path.as_ptr() as usize,
                arg(flags | O_CLOEXEC),
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
                arg(fd),
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
                Err(e) if e == Errno::EINTR => {}
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
                arg(fd),
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
                Err(e) if e == Errno::EINTR => {}
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
        ok_unit(decode(unsafe { raw::syscall1(nr::CLOSE, arg(fd)) }))
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
    /// new. Trusted: libc (glibc's thread pointer); the contract claims
    /// only that the call returns, which `kani_self` checks on premise C12
    /// of `verus/kani/premises.md`. What callers assume of the value (a
    /// thread descriptor's address) is theirs to state: that it tells the
    /// threads of one process apart and stays the forking thread's in a
    /// fork child, `lock::axiom_child_window`; that it is never 0, 1 or
    /// 2, the init gate's values (`api::GATE_IDLE` and its kin).
    #[verifier::external_body]
    #[inline(always)]
    pub fn pthread_self() -> usize {
        // SAFETY: `pthread_self` has no preconditions and cannot fail.
        unsafe { glibc::pthread_self() as usize }
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
                let words = crate::vspec::div_ceil(written.min(len), 8);
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
                arg(SCHED_OTHER),
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
                arg(pid),
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
                arg(pid),
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
                arg(PRIO_PROCESS),
                arg(pid),
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
                arg(PRIO_PROCESS),
                arg(pid),
                arg(nice),
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
            raw::syscall1(nr::EXIT_GROUP, arg(status));
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

    /// Kani check of `pthread_self` (`verus/ledger/src/sys/mod.rs.md`;
    /// `cfg(kani)` only, see `verus/README.md`, "Kani").
    #[cfg(kani)]
    mod kani_self {
        use super::pthread_self;

        /// What the stand-in for glibc's `pthread_self` returns, and how
        /// many times it was called.
        static mut ID: libc::pthread_t = 0;
        static mut CALLS: usize = 0;

        /// A stand-in for glibc's `pthread_self` (premise C12): it records
        /// its call and returns `ID`.
        #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
        unsafe extern "C" fn self_model() -> libc::pthread_t {
            // SAFETY: the harness's own statics; no other code runs in a
            // harness.
            unsafe {
                CALLS += 1;
                ID
            }
        }

        /// `pthread_self`: no `requires`, no `ensures`; it returns, and
        /// (beyond the contract) calls glibc's function once and returns
        /// its ID unchanged, as a `usize`, for any ID.
        #[kani::proof]
        #[kani::stub(super::glibc::pthread_self, self_model)]
        fn pthread_self_returns_the_id() {
            let id: libc::pthread_t = kani::any();
            // SAFETY: as in `self_model`.
            unsafe { ID = id };
            let r = pthread_self();
            // SAFETY: as in `self_model`.
            let calls = unsafe { CALLS };
            assert_eq!(calls, 1);
            assert_eq!(r as u64, id);
        }
    }
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
pub(crate) mod tests;
