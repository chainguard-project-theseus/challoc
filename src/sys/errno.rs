//! Kernel error mapping. Pure logic.
//!
//! Raw Linux syscalls return a single machine word. Values in
//! `-4095..=-1` are negated `errno` codes; everything else is a success
//! value (a length, a file descriptor, or an address). This module turns
//! that convention into a `Result` without touching libc's `errno`, which
//! `free` must leave untouched.

use core::ffi::c_int;

use vstd::prelude::*;

verus! {

/// A Linux `errno` value. Only the codes the allocator inspects are named;
/// anything else is carried through as its raw number.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Errno(pub i32);

impl Errno {
    pub const EPERM: Errno = Errno(1);
    pub const ENOENT: Errno = Errno(2);
    pub const EINTR: Errno = Errno(4);
    pub const EIO: Errno = Errno(5);
    pub const EBADF: Errno = Errno(9);
    pub const EAGAIN: Errno = Errno(11);
    pub const ENOMEM: Errno = Errno(12);
    pub const EACCES: Errno = Errno(13);
    pub const EFAULT: Errno = Errno(14);
    pub const EBUSY: Errno = Errno(16);
    pub const EEXIST: Errno = Errno(17);
    pub const EINVAL: Errno = Errno(22);
    pub const EROFS: Errno = Errno(30);
    pub const ENOSYS: Errno = Errno(38);
    pub const ETIMEDOUT: Errno = Errno(110);

    /// Short symbolic name for diagnostics. Unknown codes print as a number.
    pub const fn name(self) -> &'static str {
        // An `if` chain over the constants: Verus does not match on a
        // struct constant in a pattern.
        let c = self.0;
        if c == Errno::EPERM.0 {
            "EPERM"
        } else if c == Errno::ENOENT.0 {
            "ENOENT"
        } else if c == Errno::EINTR.0 {
            "EINTR"
        } else if c == Errno::EIO.0 {
            "EIO"
        } else if c == Errno::EBADF.0 {
            "EBADF"
        } else if c == Errno::EAGAIN.0 {
            "EAGAIN"
        } else if c == Errno::ENOMEM.0 {
            "ENOMEM"
        } else if c == Errno::EACCES.0 {
            "EACCES"
        } else if c == Errno::EFAULT.0 {
            "EFAULT"
        } else if c == Errno::EBUSY.0 {
            "EBUSY"
        } else if c == Errno::EEXIST.0 {
            "EEXIST"
        } else if c == Errno::EINVAL.0 {
            "EINVAL"
        } else if c == Errno::EROFS.0 {
            "EROFS"
        } else if c == Errno::ENOSYS.0 {
            "ENOSYS"
        } else if c == Errno::ETIMEDOUT.0 {
            "ETIMEDOUT"
        } else {
            ""
        }
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = self.name();
        if name.is_empty() {
            write!(f, "errno {}", self.0)
        } else {
            f.write_str(name)
        }
    }
}

// The codes C callers see as `c_int`: `errno`, `posix_memalign`'s and
// `mallctl`'s return values.
pub const EPERM: c_int = Errno::EPERM.0;
pub const ENOENT: c_int = Errno::ENOENT.0;
pub const EAGAIN: c_int = Errno::EAGAIN.0;
pub const ENOMEM: c_int = Errno::ENOMEM.0;
pub const EFAULT: c_int = Errno::EFAULT.0;
pub const EINVAL: c_int = Errno::EINVAL.0;

/// Largest `errno` the kernel will ever return, per `include/linux/err.h`.
pub const MAX_ERRNO: usize = 4095;

/// Convert a raw syscall return word into a `Result`.
///
/// Values in `-MAX_ERRNO..=-1` (as a signed word) are errors; everything
/// else, including addresses with the top bit clear or set, is success.
#[inline(always)]
pub const fn decode(ret: usize) -> (r: Result<usize, Errno>)
    ensures
        ret <= usize::MAX - MAX_ERRNO ==> r == Ok::<usize, Errno>(ret),
        ret > usize::MAX - MAX_ERRNO ==> (r matches Err(e) && 1 <= e.0 <= MAX_ERRNO),
{
    if ret > usize::MAX - MAX_ERRNO {
        #[cfg_attr(verus_keep_ghost, verifier::truncate)]
        let word = ret as isize;
        proof {
            assert(-4096 < word < 0) by (bit_vector)
                requires
                    word == (#[verifier::truncate] (ret as isize)),
                    ret > 0xFFFF_FFFF_FFFF_F000usize,
            ;
        }
        Err(Errno(word.wrapping_neg() as i32))
    } else {
        Ok(ret)
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::{Errno, MAX_ERRNO, decode};

    #[test]
    fn zero_and_small_positives_are_ok() {
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(1), Ok(1));
        assert_eq!(decode(4096), Ok(4096));
    }

    #[test]
    fn negative_errnos_map() {
        assert_eq!(decode((-22isize) as usize), Err(Errno::EINVAL));
        assert_eq!(decode((-12isize) as usize), Err(Errno::ENOMEM));
        assert_eq!(decode((-38isize) as usize), Err(Errno::ENOSYS));
        assert_eq!(decode((-1isize) as usize), Err(Errno::EPERM));
        assert_eq!(decode((-4isize) as usize), Err(Errno::EINTR));
        assert_eq!(decode((-110isize) as usize), Err(Errno::ETIMEDOUT));
    }

    #[test]
    fn boundary_is_exactly_max_errno() {
        // -4095 is the last error; -4096 is a valid (if odd) success value.
        assert_eq!(decode((-(MAX_ERRNO as isize)) as usize), Err(Errno(4095)));
        let just_below = (-(MAX_ERRNO as isize) - 1) as usize;
        assert_eq!(decode(just_below), Ok(just_below));
    }

    #[test]
    fn addresses_are_ok() {
        // Typical mmap results and a high canonical address that would be
        // negative if naively interpreted as isize.
        assert_eq!(decode(0x7f00_0000_0000), Ok(0x7f00_0000_0000));
        assert_eq!(decode(0x8000_0000_0000_0000), Ok(0x8000_0000_0000_0000));
        assert_eq!(decode(usize::MAX - MAX_ERRNO), Ok(usize::MAX - MAX_ERRNO));
    }

    #[test]
    fn every_code_round_trips() {
        for code in 1..=MAX_ERRNO {
            let raw = (-(code as isize)) as usize;
            assert_eq!(decode(raw), Err(Errno(code as i32)));
        }
    }

    /// Every named code has a name: the `pub const E...: Errno = Errno(n)`
    /// lines of this file, read from the source so a code added without a
    /// `name` arm fails here.
    #[test]
    fn every_named_code_has_a_name() {
        let src = std::fs::read_to_string(std::concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/sys/errno.rs"
        ))
        .unwrap();
        let mut seen = 0;
        for line in src.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub const E") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(": Errno = Errno(") else {
                continue;
            };
            let code: i32 = value.trim_end_matches([')', ';']).parse().unwrap();
            let name = std::format!("E{name}");
            assert_eq!(Errno(code).name(), name, "Errno({code})");
            assert_eq!(std::format!("{}", Errno(code)), name);
            seen += 1;
        }
        assert!(seen >= 15, "found {seen} named codes");
    }

    #[test]
    fn display_names_known_and_unknown() {
        assert_eq!(std::format!("{}", Errno::EINVAL), "EINVAL");
        assert_eq!(std::format!("{}", Errno::ENOSYS), "ENOSYS");
        assert_eq!(std::format!("{}", Errno(999)), "errno 999");
        assert_eq!(Errno(999).name(), "");
    }
}
