//! Raw Linux syscalls (design.md section 19).
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
const SIGCANCEL: u32 = 32;
const SIGSETXID: u32 = 33;
/// Every signal but glibc's two internal ones: the set
/// [`block_all_signals`] blocks.
const BLOCKABLE_SIGNALS: u64 = !((1u64 << (SIGCANCEL - 1)) | (1u64 << (SIGSETXID - 1)));

pub const STDERR_FILENO: i32 = 2;

} // verus!

// ---- which functions may enter the kernel ------------------------------

verus! {

/// The caller may make a system call. The `raw::syscall0`..`syscall6`
/// seams, which hold every instruction that enters the kernel, require
/// it, and so must every trusted body that can reach the kernel. So a
/// verified function can reach the kernel only if its own `requires` (or
/// its loop's invariant) carries the fact.
///
/// Opaque, with no meaning: a capability, not a claim about the world.
#[verifier::opaque]
pub open spec fn may_syscall() -> bool {
    true
}

} // verus!

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

} // verus!

mod linux {
    use super::errno::{Errno, decode};
    use super::{
        AT_FDCWD, BLOCKABLE_SIGNALS, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, FUTEX_PRIVATE_FLAG,
        FUTEX_WAIT, FUTEX_WAKE, O_CLOEXEC, PR_SET_VMA, PR_SET_VMA_ANON_NAME, SIG_BLOCK,
        SIG_SETMASK, SIGSET_WORDS_BYTES, abort_fmt,
    };
    use super::{nr, raw};

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
        pub const fn as_nanos(self) -> u64 {
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
    ) -> Result<usize, Errno> {
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
    pub unsafe fn munmap(addr: usize, len: usize) -> Result<(), Errno> {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall2(nr::MUNMAP, addr, len) }).map(|_| ())
    }

    /// `mprotect(2)`.
    ///
    /// # Safety
    /// Removing permissions from memory in use faults later accesses.
    #[inline]
    pub unsafe fn mprotect(addr: usize, len: usize, prot: i32) -> Result<(), Errno> {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe { raw::syscall3(nr::MPROTECT, addr, len, prot as isize as usize) })
            .map(|_| ())
    }

    /// `madvise(2)`.
    ///
    /// # Safety
    /// `MADV_DONTNEED`/`MADV_FREE` discard contents the caller must not need.
    #[inline]
    pub unsafe fn madvise(addr: usize, len: usize, advice: i32) -> Result<(), Errno> {
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
    pub unsafe fn mseal(addr: usize, len: usize, flags: usize) -> Result<(), Errno> {
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
    ) -> Result<(), Errno> {
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
    ) -> Result<usize, Errno> {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall5(nr::PRCTL, option as isize as usize, arg2, arg3, arg4, arg5)
        })
    }

    // ---- misc ------------------------------------------------------------

    /// `getrandom(2)`. Returns the number of bytes written, which may be
    /// short; callers that need the full buffer should loop.
    #[inline]
    pub fn getrandom(buf: &mut [u8], flags: u32) -> Result<usize, Errno> {
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
    /// `EINTR`. Fails only if the kernel refuses outright. (An offset into
    /// `buf` rather than a reborrowed tail, and `EINTR` as a guard rather
    /// than a pattern: Verus takes neither of those.)
    pub fn getrandom_exact(buf: &mut [u8]) -> Result<(), Errno> {
        let mut filled = 0;
        while filled < buf.len() {
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
    ) -> Result<usize, Errno> {
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
    ) -> Result<(), Errno> {
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
    pub unsafe fn futex_wake(uaddr: *const u32, n: u32) -> Result<usize, Errno> {
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
    pub fn clock_gettime(clock: i32) -> Result<Timespec, Errno> {
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
    /// reads on its own behalf (design.md section 11).
    #[inline]
    pub fn now_coarse_ns() -> u64 {
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
    pub fn open(path: &[u8], flags: i32) -> Result<i32, Errno> {
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

    /// `read(2)`.
    #[inline]
    pub fn read(fd: i32, buf: &mut [u8]) -> Result<usize, Errno> {
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
    /// the number of bytes read; stops at EOF or a full buffer.
    pub fn read_full(fd: i32, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut filled = 0;
        while filled < buf.len() {
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
    pub fn read_small_file(path: &[u8], buf: &mut [u8]) -> Option<usize> {
        let fd = open(path, super::O_RDONLY).ok()?;
        let r = read_full(fd, buf).ok();
        let _ = close(fd);
        r
    }

    /// `write(2)`.
    #[inline]
    pub fn write(fd: i32, buf: &[u8]) -> Result<usize, Errno> {
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
    /// better to do. (An offset into `buf`, as in [`getrandom_exact`].)
    pub fn write_all(fd: i32, buf: &[u8]) {
        let mut done = 0;
        while done < buf.len() {
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
    pub fn close(fd: i32) -> Result<(), Errno> {
        // SAFETY: no pointers.
        decode(unsafe { raw::syscall1(nr::CLOSE, fd as isize as usize) }).map(|_| ())
    }

    // ---- process ---------------------------------------------------------

    #[inline]
    pub fn getpid() -> i32 {
        // SAFETY: no arguments; cannot fail.
        unsafe { raw::syscall0(nr::GETPID) as i32 }
    }

    #[inline]
    pub fn gettid() -> i32 {
        // SAFETY: no arguments; cannot fail.
        unsafe { raw::syscall0(nr::GETTID) as i32 }
    }

    /// CPUs the calling thread may run on: the bits set in the affinity
    /// mask `sched_getaffinity(0)` returns, so `taskset` and cgroup
    /// cpusets count, not the machine (jemalloc's `malloc_ncpus`). The
    /// kernel writes its own mask width and refuses a buffer narrower than
    /// it with `EINVAL`; 8192 CPUs covers every supported kernel. 1 when
    /// the call fails (a seccomp filter, or a wider mask), never 0.
    pub fn ncpus() -> u32 {
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
                while i < words {
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
    pub fn sched_set_other() -> Result<usize, Errno> {
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
    pub fn sched_getaffinity(pid: i32, set: &mut CpuSet) -> Result<usize, Errno> {
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
    pub fn sched_setaffinity(pid: i32, set: &CpuSet) -> Result<usize, Errno> {
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
    pub fn getpriority(pid: i32) -> Result<i32, Errno> {
        // SAFETY: no pointers.
        let r = decode(unsafe {
            raw::syscall2(
                nr::GETPRIORITY,
                PRIO_PROCESS as isize as usize,
                pid as isize as usize,
            )
        });
        match r {
            Ok(v) => Ok(20i32.wrapping_sub(v as i32)),
            Err(e) => Err(e),
        }
    }

    /// `setpriority(PRIO_PROCESS, pid, nice)`.
    pub fn setpriority(pid: i32, nice: i32) -> Result<usize, Errno> {
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
    /// silently leaves `SIGKILL` and `SIGSTOP` unblocked.
    pub fn block_all_signals() -> u64 {
        let all: u64 = BLOCKABLE_SIGNALS;
        let mut old: u64 = 0;
        // SAFETY: both pointers are live 8-byte sigsets for the whole call.
        let r = unsafe { sigprocmask_block(&all, &mut old) };
        if let Err(e) = decode(r) {
            sigprocmask_failed("SIG_BLOCK", e);
        }
        old
    }

    /// `rt_sigprocmask(SIG_SETMASK, mask)`: restore a mask saved by
    /// [`block_all_signals`] on the calling thread.
    pub fn set_signal_mask(mask: u64) {
        // SAFETY: `mask` is a live 8-byte sigset; the old-set pointer is null.
        let r = unsafe { sigprocmask_setmask(&mask) };
        if let Err(e) = decode(r) {
            sigprocmask_failed("SIG_SETMASK", e);
        }
    }

    /// The raw `rt_sigprocmask(SIG_BLOCK, all, before)`, returning the
    /// syscall's word: a successful call wrote the mask in force before it
    /// to `*before`. Apart from [`block_all_signals`] so that the raw call,
    /// whose effect Verus cannot see, is a function of its own.
    ///
    /// # Safety
    /// As `raw::syscall4`; both pointers are live 8-byte sigsets.
    #[inline(always)]
    unsafe fn sigprocmask_block(all: &u64, before: &mut u64) -> usize {
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
    /// syscall's word. Apart from [`set_signal_mask`], as
    /// [`sigprocmask_block`] is.
    ///
    /// # Safety
    /// As `raw::syscall4`; `mask` is a live 8-byte sigset.
    #[inline(always)]
    unsafe fn sigprocmask_setmask(mask: &u64) -> usize {
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
    fn sigprocmask_failed(how: &str, e: Errno) -> ! {
        abort_fmt(format_args!("rt_sigprocmask({how}) failed: {e}"))
    }

    /// `exit_group(2)`: terminate every thread in the process.
    pub fn exit_group(status: i32) -> ! {
        // SAFETY: no pointers; does not return.
        unsafe {
            raw::syscall1(nr::EXIT_GROUP, status as isize as usize);
        }
        // The kernel never returns from exit_group; trap if it somehow did.
        trap()
    }

    /// Architecture trap instruction, for the impossible case where the
    /// kernel returns from a terminating syscall.
    #[inline(always)]
    pub fn trap() -> ! {
        // SAFETY: both instructions raise a fatal signal and nothing else.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            core::arch::asm!("ud2", options(noreturn, nostack));
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!("brk #0x1", options(noreturn, nostack));
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
mod tests {
    use super::*;
    use std::process::Command;
    use std::{env, format, vec::Vec};

    const PAGE: usize = 4096;

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
            let before = block_all_signals();
            let during = block_all_signals();
            set_signal_mask(before);
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
                    unsafe { mprotect(addr, PAGE, PROT_NONE) },
                    Err(Errno::EPERM)
                );
                // MADV_DONTNEED on a sealed writable anonymous mapping is
                // allowed; discarding is only refused for read-only ranges.
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
            unsafe {
                libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set)
            },
            0
        );
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
