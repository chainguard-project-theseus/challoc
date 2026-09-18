//! Raw Linux syscalls (design.md section 17).
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

pub mod errno;
pub mod features;
pub use errno::{Errno, decode};

#[cfg(target_os = "linux")]
mod nr;
#[cfg(target_os = "linux")]
mod raw;

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::errno::{Errno, decode};
    use super::{nr, raw};
    use core::fmt;

    // ---- constants -------------------------------------------------------
    // Values are identical on x86_64 and aarch64 (both use the asm-generic
    // definitions for everything below).

    pub const PROT_NONE: i32 = 0;
    pub const PROT_READ: i32 = 1;
    pub const PROT_WRITE: i32 = 2;

    pub const MAP_SHARED: i32 = 0x01;
    pub const MAP_PRIVATE: i32 = 0x02;
    pub const MAP_FIXED: i32 = 0x10;
    pub const MAP_ANONYMOUS: i32 = 0x20;
    pub const MAP_POPULATE: i32 = 0x8000;
    pub const MAP_NORESERVE: i32 = 0x4000;
    pub const MAP_FIXED_NOREPLACE: i32 = 0x10_0000;
    pub const MAP_FAILED: usize = usize::MAX;

    pub const MADV_NORMAL: i32 = 0;
    pub const MADV_DONTNEED: i32 = 4;
    pub const MADV_FREE: i32 = 8;
    pub const MADV_HUGEPAGE: i32 = 14;
    pub const MADV_NOHUGEPAGE: i32 = 15;
    pub const MADV_COLD: i32 = 20;
    pub const MADV_PAGEOUT: i32 = 21;
    pub const MADV_COLLAPSE: i32 = 25;
    pub const MADV_GUARD_INSTALL: i32 = 102;
    pub const MADV_GUARD_REMOVE: i32 = 103;

    /// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, addr, len, name)`.
    pub const PR_SET_VMA: i32 = 0x5356_4d41;
    pub const PR_SET_VMA_ANON_NAME: usize = 0;

    pub const GRND_NONBLOCK: u32 = 1;
    pub const GRND_RANDOM: u32 = 2;
    pub const GRND_INSECURE: u32 = 4;

    pub const FUTEX_WAIT: i32 = 0;
    pub const FUTEX_WAKE: i32 = 1;
    pub const FUTEX_PRIVATE_FLAG: i32 = 128;

    pub const CLOCK_MONOTONIC: i32 = 1;
    pub const CLOCK_MONOTONIC_COARSE: i32 = 6;

    pub const O_RDONLY: i32 = 0;
    pub const O_CLOEXEC: i32 = 0o2_000_000;
    pub const AT_FDCWD: i32 = -100;

    /// Sentinel pidfd meaning "the calling thread", accepted by
    /// `process_madvise` on kernels that have it (6.14, `docs/research.md`;
    /// `purge.rs` mirrors the value). Probed at runtime by `sys::features`;
    /// older kernels return `EBADF`.
    pub const PIDFD_SELF: i32 = -10000;

    pub const SIGABRT: i32 = 6;
    const SIG_BLOCK: i32 = 0;
    const SIG_UNBLOCK: i32 = 1;
    const SIG_SETMASK: i32 = 2;
    const SIGSET_WORDS_BYTES: usize = 8;

    pub const STDERR_FILENO: i32 = 2;

    // ---- structs ---------------------------------------------------------

    /// `struct iovec`.
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct IoVec {
        pub base: *const u8,
        pub len: usize,
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

    /// The kernel's `struct sigaction` for `rt_sigaction`. Both x86_64 and
    /// aarch64 define `SA_RESTORER`, so the layout is the same.
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct KernelSigaction {
        handler: usize,
        flags: usize,
        restorer: usize,
        mask: u64,
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

    /// `futex(2)`.
    ///
    /// # Safety
    /// `uaddr` must be a live, 4-byte-aligned `u32`; `timeout` null or valid.
    #[inline]
    pub unsafe fn futex(
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

    // ---- abort -----------------------------------------------------------

    /// Fixed-size stderr sink for abort messages. Buffers so a message is
    /// usually one `write(2)`, flushing when full. Never allocates.
    pub struct Stderr {
        buf: [u8; 512],
        len: usize,
    }

    impl Stderr {
        pub const fn new() -> Self {
            Stderr { buf: [0; 512], len: 0 }
        }

        pub fn flush(&mut self) {
            if self.len > 0 {
                write_all(STDERR_FILENO, &self.buf[..self.len]);
                self.len = 0;
            }
        }
    }

    impl Default for Stderr {
        fn default() -> Self {
            Self::new()
        }
    }

    impl fmt::Write for Stderr {
        fn write_str(&mut self, mut s: &str) -> fmt::Result {
            while !s.is_empty() {
                let room = self.buf.len() - self.len;
                if room == 0 {
                    self.flush();
                    continue;
                }
                let n = s.len().min(room);
                self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
                self.len += n;
                s = &s[n..];
            }
            Ok(())
        }
    }

    /// Write `challoc: <msg>` to stderr and terminate with `SIGABRT`.
    #[cold]
    #[inline(never)]
    pub fn abort(msg: &str) -> ! {
        abort_fmt(format_args!("{msg}"))
    }

    /// Formatting variant of [`abort`]. Use through the `abort!` macro.
    #[cold]
    #[inline(never)]
    pub fn abort_fmt(args: fmt::Arguments<'_>) -> ! {
        use fmt::Write as _;
        let mut out = Stderr::new();
        // Errors from the sink are impossible (it never fails), so ignore.
        let _ = out.write_str("challoc: ");
        let _ = out.write_fmt(args);
        let _ = out.write_str("\n");
        out.flush();
        die()
    }

    /// Raise `SIGABRT` on the calling thread, mirroring glibc's `abort`:
    /// unblock the signal, deliver it, and if a handler swallowed it reset the
    /// disposition to default and deliver again. `exit_group` is the last
    /// resort so the process never continues past an inconsistency.
    #[cold]
    fn die() -> ! {
        let set: u64 = 1 << (SIGABRT - 1);
        let pid = getpid() as isize as usize;
        let tid = gettid() as isize as usize;
        // SAFETY: `set` is a live 8-byte sigset; the kernel validates the rest.
        unsafe {
            raw::syscall4(
                nr::RT_SIGPROCMASK,
                SIG_UNBLOCK as usize,
                &set as *const u64 as usize,
                0,
                SIGSET_WORDS_BYTES,
            );
            raw::syscall3(nr::TGKILL, pid, tid, SIGABRT as usize);
            // A handler returned. Reset to SIG_DFL and try once more.
            let dfl = KernelSigaction { handler: 0, flags: 0, restorer: 0, mask: 0 };
            raw::syscall4(
                nr::RT_SIGACTION,
                SIGABRT as usize,
                &dfl as *const KernelSigaction as usize,
                0,
                SIGSET_WORDS_BYTES,
            );
            raw::syscall3(nr::TGKILL, pid, tid, SIGABRT as usize);
        }
        // 128 + SIGABRT, the shell convention for death by signal 6.
        exit_group(134)
    }
}

/// Abort the process with a formatted `challoc: ...` message on stderr.
#[macro_export]
macro_rules! abort {
    ($($arg:tt)*) => {
        $crate::sys::abort_fmt(::core::format_args!($($arg)*))
    };
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::{env, format, vec::Vec};

    const PAGE: usize = 4096;

    fn map_page() -> usize {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        unsafe {
            mmap(0, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0).unwrap()
        }
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
                    // Containers usually lack it. The feature probe (task 3)
                    // must treat this as "unavailable".
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
        let flags = std::fs::read_link(format!("/proc/self/fdinfo/{fd}")).ok();
        let _ = flags;
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
        let flags_line = info.lines().find(|l| l.starts_with("flags:")).unwrap();
        let oflags = usize::from_str_radix(flags_line.split_whitespace().nth(1).unwrap(), 8)
            .unwrap();
        assert_ne!(oflags & (O_CLOEXEC as usize), 0, "{flags_line}");
        close(fd).unwrap();
    }

    #[test]
    fn write_to_devnull_and_bad_fd() {
        let fd = open(b"/dev/null\0", 1 /* O_WRONLY */).unwrap();
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

    /// Cross-check `nr.rs` against the kernel headers via a tiny C program.
    #[test]
    fn ncpus_counts_the_affinity_mask() {
        let n = ncpus();
        assert!(n >= 1);
        // The mask can only be narrower than the configured CPU count.
        let conf = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
        assert!(conf >= 1 && n as i64 <= conf, "ncpus {n}, _SC_NPROCESSORS_CONF {conf}");
        let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
        assert_eq!(unsafe { libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set) }, 0);
        assert_eq!(n as i32, unsafe { libc::CPU_COUNT(&set) });
    }

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

    // ---- abort: the child half runs only when the env var is set ---------

    const ABORT_ENV: &str = "CHALLOC_SYS_TEST_ABORT";

    #[test]
    fn abort_child_hook() {
        match env::var(ABORT_ENV).as_deref() {
            Ok("plain") => abort("test abort"),
            Ok("fmt") => abort!("bad free of {:#x} ({})", 0xdead_beef_usize, "unknown"),
            Ok("handler") => {
                extern "C" fn ignore(_: i32) {}
                // SAFETY: installing a trivial handler in a throwaway child.
                unsafe { libc::signal(libc::SIGABRT, ignore as *const () as usize) };
                abort("with handler")
            }
            Ok("blocked") => {
                // SAFETY: plain sigprocmask in a throwaway child.
                unsafe {
                    let mut set: libc::sigset_t = core::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, libc::SIGABRT);
                    libc::pthread_sigmask(libc::SIG_BLOCK, &set, core::ptr::null_mut());
                }
                abort("while blocked")
            }
            Ok("long") => {
                // Longer than the 512-byte Stderr buffer: exercises flush.
                let long = "x".repeat(1500);
                abort!("{long}")
            }
            _ => {} // parent run: nothing to do
        }
    }

    fn run_abort_child(mode: &str) -> (Option<i32>, std::string::String) {
        let exe = env::current_exe().unwrap();
        let output = Command::new(exe)
            .args(["--exact", "sys::tests::abort_child_hook", "--nocapture", "--test-threads=1"])
            .env(ABORT_ENV, mode)
            .output()
            .unwrap();
        let stderr = std::string::String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.signal(), stderr)
    }

    #[test]
    fn abort_writes_message_and_raises_sigabrt() {
        let (sig, err) = run_abort_child("plain");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: test abort\n"), "stderr: {err}");
    }

    #[test]
    fn abort_macro_formats() {
        let (sig, err) = run_abort_child("fmt");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: bad free of 0xdeadbeef (unknown)\n"), "stderr: {err}");
    }

    #[test]
    fn abort_defeats_a_returning_handler() {
        let (sig, err) = run_abort_child("handler");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: with handler\n"), "stderr: {err}");
    }

    #[test]
    fn abort_defeats_a_blocked_signal() {
        let (sig, err) = run_abort_child("blocked");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: while blocked\n"), "stderr: {err}");
    }

    #[test]
    fn abort_flushes_long_messages() {
        let (sig, err) = run_abort_child("long");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        let expected = format!("challoc: {}\n", "x".repeat(1500));
        assert!(err.contains(&expected), "stderr len {}", err.len());
    }
}
