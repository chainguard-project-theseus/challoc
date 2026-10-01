//! Linux syscall numbers for the two supported architectures.
//!
//! Hand-declared rather than taken from the `libc` crate so the set is
//! explicit and independent of the crate version. `tests/c/sysnr.c` prints
//! the same names from the kernel headers and the Linux-only unit test in
//! `sys::tests` checks every constant against it.

/// Name/number pairs, in the same order as `tests/c/sysnr.c`, for the
/// header cross-check test. `table_covers_every_constant` below keeps it
/// complete: a constant added to `arch` or the unified list without a row
/// here would never be checked against the headers.
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
    ("mseal", MSEAL),
    ("sched_getaffinity", SCHED_GETAFFINITY),
    ("ppoll", PPOLL),
    ("eventfd2", EVENTFD2),
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
    pub const SCHED_GETAFFINITY: usize = 204;
    pub const CLOCK_GETTIME: usize = 228;
    pub const EXIT_GROUP: usize = 231;
    pub const TGKILL: usize = 234;
    pub const OPENAT: usize = 257;
    pub const PPOLL: usize = 271;
    pub const EVENTFD2: usize = 290;
    pub const GETRANDOM: usize = 318;
}

#[cfg(target_arch = "aarch64")]
mod arch {
    pub const EVENTFD2: usize = 19;
    pub const OPENAT: usize = 56;
    pub const CLOSE: usize = 57;
    pub const READ: usize = 63;
    pub const WRITE: usize = 64;
    pub const PPOLL: usize = 73;
    pub const EXIT_GROUP: usize = 94;
    pub const FUTEX: usize = 98;
    pub const CLOCK_GETTIME: usize = 113;
    pub const SCHED_GETAFFINITY: usize = 123;
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
pub const MSEAL: usize = 462;

#[cfg(test)]
mod tests {
    /// Every `pub const NAME: usize` in this file (both `arch` blocks and
    /// the unified numbers) has a `TABLE` row under its lowercase name,
    /// and every row names a constant. Reads the source, so a new
    /// constant fails here until it is header-checked.
    #[test]
    fn table_covers_every_constant() {
        let src = std::fs::read_to_string(std::concat!(env!("CARGO_MANIFEST_DIR"), "/src/sys/nr.rs")).unwrap();
        let mut consts = std::vec::Vec::new();
        for line in src.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub const ") else { continue };
            let Some((name, _)) = rest.split_once(": usize") else { continue };
            if name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
                consts.push(name.to_ascii_lowercase());
            }
        }
        consts.sort();
        consts.dedup();
        assert!(consts.len() >= super::TABLE.len(), "{consts:?}");
        for name in &consts {
            assert!(super::TABLE.iter().any(|(n, _)| n == name), "{name} declared in nr.rs but missing from TABLE");
        }
        for (name, _) in super::TABLE {
            assert!(consts.iter().any(|c| c == name), "TABLE row {name} names no constant");
        }
    }
}
