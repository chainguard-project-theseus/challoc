//! Raw Linux syscalls (design.md section 19).
//!
//! Everything the allocator asks the kernel for goes through this module via
//! `asm!` syscall instructions, never through libc. That keeps `errno`
//! untouched (glibc requires `free` to preserve it), keeps ld.so's early
//! `malloc` calls free of libc re-entrancy, and makes the set of kernel
//! interfaces the crate depends on explicit.
//!
//! The pure error mapping lives in `errno.rs` and is unit tested on the host.
//! Everything else is Linux-only and is exercised by `cargo test --features
//! std` inside the dev container.

pub mod auxv;
pub mod cgroup;
pub mod errno;
pub mod features;
pub mod pac;
pub use errno::{Errno, decode};

#[cfg(target_os = "linux")]
mod abort;
#[cfg(target_os = "linux")]
mod nr;
#[cfg(target_os = "linux")]
mod raw;

#[cfg(target_os = "linux")]
pub use abort::*;
#[cfg(target_os = "linux")]
pub use linux::*;

// ---- constants -------------------------------------------------------
// Values are identical on x86_64 and aarch64 (both use the asm-generic
// definitions for everything below).

pub const PROT_NONE: i32 = 0;
pub const PROT_READ: i32 = 1;
pub const PROT_WRITE: i32 = 2;

#[cfg(test)]
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_NORESERVE: i32 = 0x4000;
#[cfg(test)]
pub const MAP_FIXED_NOREPLACE: i32 = 0x10_0000;

pub const MADV_DONTNEED: i32 = 4;
pub const MADV_FREE: i32 = 8;
pub const MADV_HUGEPAGE: i32 = 14;
pub const MADV_NOHUGEPAGE: i32 = 15;
#[cfg(test)]
pub const MADV_COLD: i32 = 20;
/// Populate (prefault) page tables writable (kernel 5.14): the pages
/// fault in as if written, contents untouched.
pub const MADV_POPULATE_WRITE: i32 = 23;
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
pub const O_RDWR: i32 = 2;
pub const O_NONBLOCK: i32 = 0o4000;
pub const O_CLOEXEC: i32 = 0o2_000_000;

/// `eventfd2` flags (the same bits as the `O_*` flags they mirror).
pub const EFD_NONBLOCK: i32 = O_NONBLOCK;
pub const EFD_CLOEXEC: i32 = O_CLOEXEC;

pub const POLLIN: i16 = 0x001;
pub const POLLPRI: i16 = 0x002;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;
pub const AT_FDCWD: i32 = -100;

/// Sentinel pidfd meaning "the calling thread" (`PIDFD_SELF_THREAD` of
/// `include/uapi/linux/pidfd.h`), accepted by `process_madvise` on
/// kernels that have it: 6.15, the first release whose header defines
/// it and whose `pidfd_get_task` honours it (6.14 has neither).
/// Probed at runtime by `sys::features`;
/// older kernels return `EBADF`.
pub const PIDFD_SELF: i32 = -10000;

pub const SIGABRT: i32 = 6;
#[cfg(target_os = "linux")]
const SIG_BLOCK: i32 = 0;
#[cfg(target_os = "linux")]
const SIG_UNBLOCK: i32 = 1;
#[cfg(target_os = "linux")]
const SIG_SETMASK: i32 = 2;
#[cfg(target_os = "linux")]
const SIGSET_WORDS_BYTES: usize = 8;

pub const STDERR_FILENO: i32 = 2;

// ---- pure helpers shared by the probes and the cgroup parsers ----------

/// `yes` or `no`: the value of every boolean key of the feature line and
/// the CPU report.
pub const fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// What [`parse_decimal`] accepts after the digits.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Trailing {
    /// Anything: `42 x` and `2097152\n` both read 42 and 2097152 (a
    /// `memory.events` value, a sysfs number with its newline).
    Ignore,
    /// Newlines only: `1\n` reads 1, `1x` is `None` (the cache `level`
    /// files, where a stray byte means the file is not what was expected).
    Newlines,
}

/// The leading decimal digits of `bytes` as a `u64`; `None` without a
/// digit, on overflow, or when `trailing` refuses what follows them.
pub fn parse_decimal(bytes: &[u8], trailing: Trailing) -> Option<u64> {
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || (trailing == Trailing::Newlines && !bytes[digits..].iter().all(|&b| b == b'\n')) {
        return None;
    }
    bytes[..digits].iter().try_fold(0u64, |acc, &b| acc.checked_mul(10)?.checked_add(u64::from(b - b'0')))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::errno::{Errno, decode};
    use super::{nr, raw};
    use super::{
        abort_fmt, AT_FDCWD, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, FUTEX_PRIVATE_FLAG, FUTEX_WAIT,
        FUTEX_WAKE, O_CLOEXEC, PR_SET_VMA, PR_SET_VMA_ANON_NAME, SIGSET_WORDS_BYTES, SIG_BLOCK,
        SIG_SETMASK,
    };

    // ---- structs ---------------------------------------------------------

    /// `struct iovec`.
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct IoVec {
        pub base: *const u8,
        pub len: usize,
    }

    /// `struct pollfd`.
    #[repr(C)]
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct PollFd {
        pub fd: i32,
        pub events: i16,
        pub revents: i16,
    }

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
            (self.sec as u64).wrapping_mul(1_000_000_000).wrapping_add(self.nsec as u64)
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

    /// `mincore(2)`: one byte per page of `[addr, addr + len)` into `vec`
    /// (bit 0 set when resident). `vec` must hold `len / page` bytes.
    pub fn mincore(addr: usize, len: usize, vec: &mut [u8]) -> Result<(), Errno> {
        // SAFETY: the kernel writes at most `len / page` bytes, which the
        // caller sized `vec` for; nothing else is read or written.
        decode(unsafe { raw::syscall3(nr::MINCORE, addr, len, vec.as_mut_ptr() as usize) }).map(|_| ())
    }

    /// `process_madvise(2)`. Returns the number of bytes advised.
    ///
    /// # Safety
    /// `iov` must point to `iovcnt` valid entries for the duration of the call.
    #[inline]
    pub unsafe fn process_madvise(
        pidfd: i32,
        iov: *const IoVec,
        iovcnt: usize,
        advice: i32,
        flags: u32,
    ) -> Result<usize, Errno> {
        // SAFETY: forwarded to the caller's contract.
        decode(unsafe {
            raw::syscall5(
                nr::PROCESS_MADVISE,
                pidfd as isize as usize,
                iov as usize,
                iovcnt,
                advice as isize as usize,
                flags as usize,
            )
        })
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

    /// `pidfd_open(2)`. Fallback source of a pidfd for `process_madvise` on
    /// kernels without `PIDFD_SELF`.
    #[inline]
    pub fn pidfd_open(pid: i32, flags: u32) -> Result<i32, Errno> {
        // SAFETY: no pointers; the kernel validates both arguments.
        decode(unsafe { raw::syscall2(nr::PIDFD_OPEN, pid as isize as usize, flags as usize) })
            .map(|fd| fd as i32)
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
        debug_assert!(name.last() == Some(&0));
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
            raw::syscall3(nr::GETRANDOM, buf.as_mut_ptr() as usize, buf.len(), flags as usize)
        })
    }

    /// Fill `buf` completely from `getrandom`, retrying short reads and
    /// `EINTR`. Fails only if the kernel refuses outright.
    pub fn getrandom_exact(mut buf: &mut [u8]) -> Result<(), Errno> {
        while !buf.is_empty() {
            match getrandom(buf, 0) {
                Ok(0) => return Err(Errno::EIO),
                Ok(n) => buf = &mut buf[n..],
                Err(Errno::EINTR) => {}
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
    pub unsafe fn futex_wait(
        uaddr: *const u32,
        val: u32,
        timeout: Option<&Timespec>,
    ) -> Result<(), Errno> {
        let ts = timeout.map_or(core::ptr::null(), |t| t as *const Timespec);
        // SAFETY: forwarded to the caller's contract.
        unsafe { futex(uaddr, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, val, ts, core::ptr::null(), 0) }
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
        let mut ts = Timespec::default();
        // SAFETY: `ts` is a live, writable `Timespec` matching the kernel ABI.
        decode(unsafe {
            raw::syscall2(nr::CLOCK_GETTIME, clock as isize as usize, &mut ts as *mut _ as usize)
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
        debug_assert!(path.last() == Some(&0));
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
            raw::syscall3(nr::READ, fd as isize as usize, buf.as_mut_ptr() as usize, buf.len())
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
                Err(Errno::EINTR) => {}
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
            raw::syscall3(nr::WRITE, fd as isize as usize, buf.as_ptr() as usize, buf.len())
        })
    }

    /// Write all of `buf`, retrying short writes and `EINTR`. Any other error
    /// is swallowed: this is used on the abort path, where there is nothing
    /// better to do.
    pub fn write_all(fd: i32, mut buf: &[u8]) {
        while !buf.is_empty() {
            match write(fd, buf) {
                Ok(0) => return,
                Ok(n) => buf = &buf[n..],
                Err(Errno::EINTR) => {}
                Err(_) => return,
            }
        }
    }

    /// `ppoll(2)` with no signal mask change. `timeout` of `None` waits
    /// indefinitely. Returns the number of descriptors with events (0 on
    /// timeout). The kernel may write the remaining time back, so it gets
    /// a copy.
    pub fn ppoll(fds: &mut [PollFd], timeout: Option<&Timespec>) -> Result<usize, Errno> {
        let mut ts = timeout.copied();
        let tsp = ts.as_mut().map_or(0, |t| t as *mut Timespec as usize);
        // SAFETY: `fds` is a live, writable slice of the kernel's `pollfd`
        // layout; `ts` outlives the call; a null sigmask changes nothing.
        decode(unsafe {
            raw::syscall5(nr::PPOLL, fds.as_mut_ptr() as usize, fds.len(), tsp, 0, SIGSET_WORDS_BYTES)
        })
    }

    /// `eventfd2(initval, flags)`: a counter descriptor a sleeping thread
    /// can `ppoll` and another can bump with an 8-byte `write`.
    pub fn eventfd(initval: u32, flags: i32) -> Result<i32, Errno> {
        // SAFETY: no pointers.
        decode(unsafe { raw::syscall2(nr::EVENTFD2, initval as usize, flags as isize as usize) }).map(|fd| fd as i32)
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

    /// `sched_yield(2)`: give the CPU up; any syscall ends an rseq time
    /// slice grant (`slice.rs`), this is the cheapest that also means
    /// "yield". Always `Ok(0)`.
    #[inline]
    pub fn sched_yield() -> Result<usize, Errno> {
        // SAFETY: no arguments, no memory effect of ours.
        decode(unsafe { raw::syscall0(nr::SCHED_YIELD) })
    }

    /// `rseq_slice_yield(2)` (kernel 7.0, `CONFIG_RSEQ_SLICE_EXTENSION`):
    /// end a granted time slice extension now. `Ok(1)` when the kernel
    /// had marked the calling thread as yielded, `Ok(0)` otherwise,
    /// `Err(ENOSYS)` on a kernel without it or behind a seccomp filter
    /// that predates the number (Docker's default profile).
    #[inline]
    pub fn rseq_slice_yield() -> Result<usize, Errno> {
        // SAFETY: no arguments, no memory effect of ours.
        decode(unsafe { raw::syscall0(nr::RSEQ_SLICE_YIELD) })
    }

    /// CPUs the calling thread may run on: the bits set in the affinity
    /// mask `sched_getaffinity(0)` returns, so `taskset` and cgroup
    /// cpusets count, not the machine (jemalloc's `malloc_ncpus`). The
    /// kernel writes its own mask width and refuses a buffer narrower than
    /// it with `EINVAL`; 8192 CPUs covers every supported kernel. 1 when
    /// the call fails (a seccomp filter, or a wider mask), never 0.
    pub fn ncpus() -> u32 {
        let mut mask = [0u64; 128];
        let len = core::mem::size_of_val(&mask);
        // SAFETY: `mask` is writable for `len` bytes and outlives the call.
        let r = unsafe { raw::syscall3(nr::SCHED_GETAFFINITY, 0, len, mask.as_mut_ptr() as usize) };
        match decode(r) {
            Ok(written) => {
                let words = written.min(len).div_ceil(8);
                let bits: u32 = mask[..words].iter().map(|w| w.count_ones()).sum();
                bits.max(1)
            }
            Err(_) => 1,
        }
    }

    /// `rt_sigprocmask(SIG_BLOCK, all)`: block every blockable signal on the
    /// calling thread and return the mask that was in force before. The
    /// kernel silently leaves `SIGKILL` and `SIGSTOP` unblocked.
    pub fn block_all_signals() -> u64 {
        let all: u64 = !0;
        let mut old: u64 = 0;
        // SAFETY: both pointers are live 8-byte sigsets for the whole call.
        let r = unsafe {
            raw::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_BLOCK as usize,
                &all as *const u64 as usize,
                &mut old as *mut u64 as usize,
                SIGSET_WORDS_BYTES,
            )
        };
        if let Err(e) = decode(r) {
            abort_fmt(format_args!("rt_sigprocmask(SIG_BLOCK) failed: {}", e));
        }
        old
    }

    /// `rt_sigprocmask(SIG_SETMASK, mask)`: restore a mask saved by
    /// [`block_all_signals`] on the calling thread.
    pub fn set_signal_mask(mask: u64) {
        // SAFETY: `mask` is a live 8-byte sigset; the old-set pointer is null.
        let r = unsafe {
            raw::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_SETMASK as usize,
                &mask as *const u64 as usize,
                0,
                SIGSET_WORDS_BYTES,
            )
        };
        if let Err(e) = decode(r) {
            abort_fmt(format_args!("rt_sigprocmask(SIG_SETMASK) failed: {}", e));
        }
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
    use super::{Trailing, parse_decimal, yn};

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
        for t in [Trailing::Ignore, Trailing::Newlines] {
            assert_eq!(parse_decimal(b"2097152\n", t), Some(2097152), "{t:?}");
            assert_eq!(parse_decimal(b"0", t), Some(0), "{t:?}");
            assert_eq!(parse_decimal(b"18446744073709551615\n", t), Some(u64::MAX), "{t:?}");
            assert_eq!(parse_decimal(b"18446744073709551616\n", t), None, "{t:?}: overflow");
            assert_eq!(parse_decimal(b"", t), None, "{t:?}");
            assert_eq!(parse_decimal(b"\n", t), None, "{t:?}");
            assert_eq!(parse_decimal(b"-1\n", t), None, "{t:?}");
            assert_eq!(parse_decimal(b"x", t), None, "{t:?}");
        }
        assert_eq!(parse_decimal(b"42 x", Trailing::Ignore), Some(42));
        assert_eq!(parse_decimal(b"64K\n", Trailing::Ignore), Some(64));
        assert_eq!(parse_decimal(b"42 x", Trailing::Newlines), None);
        assert_eq!(parse_decimal(b"64K\n", Trailing::Newlines), None);
        assert_eq!(parse_decimal(b"1\n\n", Trailing::Newlines), Some(1));
    }
}

#[cfg(all(test, target_os = "linux"))]
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
            mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0).unwrap()
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
        assert!(matches!(r, Err(Errno::ENOMEM) | Err(Errno::EINVAL)), "{r:?}");
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

    #[test]
    fn mseal_blocks_munmap_when_supported() {
        let addr = map_page();
        // SAFETY: sealing a page we never reuse is harmless (it leaks one page
        // for the life of the test process).
        match unsafe { mseal(addr, PAGE, 0) } {
            Ok(()) => {
                // SAFETY: the kernel refuses; nothing changes.
                assert_eq!(unsafe { munmap(addr, PAGE) }, Err(Errno::EPERM));
                assert_eq!(unsafe { mprotect(addr, PAGE, PROT_NONE) }, Err(Errno::EPERM));
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
    fn process_madvise_via_pidfd() {
        let addr = map_page();
        // SAFETY: fresh writable page.
        unsafe { (addr as *mut u8).write(1) };
        let iov = IoVec { base: addr as *const u8, len: PAGE };
        let fd = pidfd_open(getpid(), 0);
        match fd {
            Ok(fd) => {
                // SAFETY: `iov` is live for the call; MADV_COLD is non-destructive.
                let r = unsafe { process_madvise(fd, &iov, 1, MADV_COLD, 0) };
                match r {
                    Ok(n) => assert_eq!(n, PAGE),
                    Err(Errno::ENOSYS) => std::eprintln!("process_madvise: ENOSYS, skipping"),
                    // Before the self-process exemption (6.13-era kernels),
                    // process_madvise needs CAP_SYS_NICE even on oneself.
                    // Containers usually lack it. The feature probe must
                    // treat this as "unavailable".
                    Err(Errno::EPERM) => std::eprintln!("process_madvise: EPERM (no CAP_SYS_NICE), skipping"),
                    Err(e) => panic!("process_madvise: unexpected {e}"),
                }
                close(fd).unwrap();
            }
            Err(Errno::ENOSYS) => std::eprintln!("pidfd_open: ENOSYS, skipping"),
            Err(e) => panic!("pidfd_open: unexpected {e}"),
        }
        // Bad pidfd must decode cleanly whatever the kernel. EPERM appears
        // when a seccomp profile (Docker's default) filters the syscall.
        // SAFETY: as above.
        let r = unsafe { process_madvise(-1, &iov, 1, MADV_COLD, 0) };
        assert!(
            matches!(r, Err(Errno::EBADF) | Err(Errno::ENOSYS) | Err(Errno::EPERM)),
            "{r:?}"
        );
        // SAFETY: page still valid.
        assert_eq!(unsafe { (addr as *const u8).read() }, 1);
        // SAFETY: we own `addr`.
        unsafe { munmap(addr, PAGE).unwrap() };
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
        assert_eq!(open(b"/nonexistent/challoc\0", O_RDONLY), Err(Errno::ENOENT));
        // O_CLOEXEC is forced on.
        let fd = open(b"/dev/null\0", O_RDONLY).unwrap();
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
        let flags_line = info.lines().find(|l| l.starts_with("flags:")).unwrap();
        let oflags = usize::from_str_radix(flags_line.split_whitespace().nth(1).unwrap(), 8)
            .unwrap();
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
        assert_eq!(&buf[..n], std::fs::read("/proc/self/comm").unwrap().as_slice());
        let mut two = [0u8; 2];
        assert_eq!(read_small_file(b"/proc/self/comm\0", &mut two), Some(2));
        assert_eq!(two, buf[..2]);
        assert_eq!(read_small_file(b"/nonexistent/challoc\0", &mut buf), None);
        assert_eq!(read_small_file(b"/proc\0", &mut buf), None, "a directory opens and fails to read");
        assert_eq!(read_small_file(b"/dev/null\0", &mut buf), Some(0));
    }

    /// Both yields reach the kernel: `sched_yield` always answers 0;
    /// `rseq_slice_yield` answers 0 (no grant outstanding) or `ENOSYS`,
    /// never anything else, and never `EINVAL` for the number.
    #[test]
    fn yields_answer_zero_or_enosys() {
        assert_eq!(sched_yield(), Ok(0));
        let r = rseq_slice_yield();
        assert!(matches!(r, Ok(0) | Err(Errno::ENOSYS)), "{r:?}");
        std::eprintln!("rseq_slice_yield: {r:?}");
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
            let ts = Timespec { sec: 0, nsec: 5_000_000 };
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
        assert!(conf >= 1 && n as i64 <= conf, "ncpus {n}, _SC_NPROCESSORS_CONF {conf}");
        // SAFETY: a zeroed `cpu_set_t` is a valid empty set; the call writes
        // at most `size_of::<cpu_set_t>()` bytes into it, and `CPU_COUNT`
        // only reads it.
        let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
        assert_eq!(unsafe { libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set) }, 0);
        assert_eq!(n as i32, unsafe { libc::CPU_COUNT(&set) });
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
            assert!(seen.iter().any(|s| s == name), "{name} missing from sysnr.c");
        }
    }

}
