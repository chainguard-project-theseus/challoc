//! Linux syscall numbers for the two supported architectures.
//!
//! Hand-declared rather than taken from the `libc` crate so the set is
//! explicit and independent of the crate version. `tests/c/sysnr.c` prints
//! the same names from the kernel headers and the Linux-only unit test in
//! `sys::tests` checks every constant against it.

/// Name/number pairs, in the same order as `tests/c/sysnr.c`, for the
/// header cross-check test.
#[cfg(test)]
pub const TABLE: &[(&str, usize)] = &[
    ("read", READ),
    ("write", WRITE),
    ("close", CLOSE),
    ("mmap", MMAP),
    ("mprotect", MPROTECT),
    ("munmap", MUNMAP),
    ("rt_sigaction", RT_SIGACTION),
    ("rt_sigprocmask", RT_SIGPROCMASK),
    ("madvise", MADVISE),
    ("getpid", GETPID),
    ("prctl", PRCTL),
    ("gettid", GETTID),
    ("futex", FUTEX),
    ("clock_gettime", CLOCK_GETTIME),
    ("exit_group", EXIT_GROUP),
    ("tgkill", TGKILL),
    ("openat", OPENAT),
    ("getrandom", GETRANDOM),
    ("pidfd_open", PIDFD_OPEN),
    ("process_madvise", PROCESS_MADVISE),
    ("mseal", MSEAL),
];

#[cfg(target_arch = "x86_64")]
mod arch {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const CLOSE: usize = 3;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const MADVISE: usize = 28;
    pub const GETPID: usize = 39;
    pub const PRCTL: usize = 157;
    pub const GETTID: usize = 186;
    pub const FUTEX: usize = 202;
    pub const CLOCK_GETTIME: usize = 228;
    pub const EXIT_GROUP: usize = 231;
    pub const TGKILL: usize = 234;
    pub const OPENAT: usize = 257;
    pub const GETRANDOM: usize = 318;
}

#[cfg(target_arch = "aarch64")]
mod arch {
    pub const OPENAT: usize = 56;
    pub const CLOSE: usize = 57;
    pub const READ: usize = 63;
    pub const WRITE: usize = 64;
    pub const EXIT_GROUP: usize = 94;
    pub const FUTEX: usize = 98;
    pub const CLOCK_GETTIME: usize = 113;
    pub const TGKILL: usize = 131;
    pub const RT_SIGACTION: usize = 134;
    pub const RT_SIGPROCMASK: usize = 135;
    pub const PRCTL: usize = 167;
    pub const GETPID: usize = 172;
    pub const GETTID: usize = 178;
    pub const MUNMAP: usize = 215;
    pub const MMAP: usize = 222;
    pub const MPROTECT: usize = 226;
    pub const MADVISE: usize = 233;
    pub const GETRANDOM: usize = 278;
}

pub use arch::*;

// Numbers from the unified table (>= 424) are the same on every architecture.
pub const PIDFD_OPEN: usize = 434;
pub const PROCESS_MADVISE: usize = 440;
pub const MSEAL: usize = 462;
