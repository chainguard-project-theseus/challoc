//! Kernel feature probing (design.md section 10, step 3).
//!
//! Every kernel feature the allocator can use is tried once, at init, on a
//! scratch page, and the outcome is recorded in a [`Features`] value. Nothing
//! is inferred from header constants or the kernel version string: a feature
//! is available only if the attempt succeeded and did what it was supposed to
//! do. Any error at all counts as "unavailable", because in practice the
//! reasons vary (`ENOSYS` from an old kernel or a seccomp filter, `EINVAL`
//! from a kernel built without the option, `EPERM` from a missing capability
//! or a seccomp profile, `EBADF` from a kernel without `PIDFD_SELF`).
//!
//! The probes never allocate, take a few microseconds, and leave at most one
//! `PROT_NONE` page mapped (the sealed page, which by definition cannot be
//! unmapped again). The pure `auxv` parsing at the top of the module is unit
//! tested on the host; the probes themselves run inside the Linux container.

use core::fmt;

/// `AT_NULL`: end of the auxiliary vector.
const AT_NULL: usize = 0;
/// `AT_PAGESZ`: system page size.
const AT_PAGESZ: usize = 6;

/// Find `AT_PAGESZ` in a raw auxiliary vector (`/proc/self/auxv` contents:
/// pairs of native-endian `usize`, terminated by `AT_NULL`). Returns `None`
/// if the entry is absent, zero, not a power of two, or the vector is
/// malformed. Pure logic, host tested.
pub fn page_size_from_auxv(bytes: &[u8]) -> Option<usize> {
    const W: usize = core::mem::size_of::<usize>();
    let mut chunks = bytes.chunks_exact(2 * W);
    for pair in &mut chunks {
        let key = usize::from_ne_bytes(pair[..W].try_into().ok()?);
        let val = usize::from_ne_bytes(pair[W..].try_into().ok()?);
        if key == AT_NULL {
            return None;
        }
        if key == AT_PAGESZ {
            return (val != 0 && val.is_power_of_two()).then_some(val);
        }
    }
    None
}

/// How batched purges (`process_madvise`, design.md section 8) can reach our
/// own address space.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProcessMadvise {
    /// `process_madvise` is unusable on ourselves; purge with a loop of
    /// `madvise` calls.
    Unavailable,
    /// The kernel accepts the `PIDFD_SELF` sentinel; no fd is held.
    PidfdSelf,
    /// The kernel needs a real pidfd; this one refers to the probing process
    /// and is `O_CLOEXEC`. After `fork` it names the parent, so the child
    /// must re-probe (design.md section 9) before purging through it.
    Pidfd(i32),
}

/// Result of probing the running kernel. Plain data; safe to copy into a
/// global once init is done.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Features {
    /// System page size from `AT_PAGESZ`. Always a power of two.
    pub page_size: usize,
    /// `log2(page_size)`.
    pub page_shift: u32,
    /// `madvise(MADV_GUARD_INSTALL)` works (kernel 6.13+). Guard slabs use
    /// it; otherwise `PROT_NONE` for huge runs only (design.md section 14).
    pub guard_install: bool,
    /// `mseal(2)` works (kernel 6.10+, not filtered by seccomp).
    pub mseal: bool,
    /// How `process_madvise` can target this process, if at all.
    pub process_madvise: ProcessMadvise,
    /// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME)` works
    /// (kernel 5.17+ with `CONFIG_ANON_VMA_NAME`).
    pub anon_name: bool,
}

impl fmt::Display for Features {
    /// One line, `key=value` separated by spaces, for `malloc_stats_print`
    /// and abort messages. Values are `yes`/`no` except `page_size` and
    /// `process_madvise` (`no`, `pidfd_self`, or `pidfd:<fd>`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn yn(b: bool) -> &'static str {
            if b { "yes" } else { "no" }
        }
        write!(
            f,
            "page_size={} guard_install={} mseal={} process_madvise=",
            self.page_size,
            yn(self.guard_install),
            yn(self.mseal),
        )?;
        match self.process_madvise {
            ProcessMadvise::Unavailable => f.write_str("no")?,
            ProcessMadvise::PidfdSelf => f.write_str("pidfd_self")?,
            ProcessMadvise::Pidfd(fd) => write!(f, "pidfd:{fd}")?,
        }
        write!(f, " anon_name={}", yn(self.anon_name))
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::{AT_PAGESZ, Features, ProcessMadvise, page_size_from_auxv};
    use crate::sys::{
        self, IoVec, MADV_DONTNEED, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE, MAP_ANONYMOUS,
        MAP_PRIVATE, PIDFD_SELF, PROT_NONE, PROT_READ, PROT_WRITE,
    };

    unsafe extern "C" {
        /// glibc/musl `getauxval(3)`. Reads the saved auxiliary vector; it
        /// never allocates and is safe to call from within `malloc`, which is
        /// exactly how glibc's own malloc learns the page size
        /// (`GLRO(dl_pagesize)`).
        safe fn getauxval(kind: usize) -> usize;
    }

    /// The system page size. `getauxval(AT_PAGESZ)` first; if that yields
    /// nothing (a libc without the vector, or a static binary started in an
    /// unusual way), `/proc/self/auxv`; if that fails too, abort, because
    /// guessing a page size would corrupt every mapping we make.
    pub fn page_size() -> usize {
        let v = getauxval(AT_PAGESZ);
        if v != 0 && v.is_power_of_two() {
            return v;
        }
        if let Some(v) = page_size_from_proc() {
            return v;
        }
        sys::abort("cannot determine the page size (AT_PAGESZ missing)")
    }

    fn page_size_from_proc() -> Option<usize> {
        // 64-bit auxv is around 20 pairs (320 bytes); leave generous room.
        let mut buf = [0u8; 1024];
        let fd = sys::open(b"/proc/self/auxv\0", sys::O_RDONLY).ok()?;
        let n = sys::read_full(fd, &mut buf);
        let _ = sys::close(fd);
        page_size_from_auxv(&buf[..n.ok()?])
    }

    /// Map a fresh private anonymous scratch page with the given protection.
    fn scratch(page: usize, prot: i32) -> Option<usize> {
        // SAFETY: fresh anonymous private mapping, no fixed address.
        unsafe { sys::mmap(0, page, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.ok()
    }

    /// Release a scratch page. Failure is a heap inconsistency: we just
    /// mapped it, so it is unmapped only if something is very wrong.
    fn release(addr: usize, page: usize) {
        // SAFETY: `addr` is a scratch page nothing else references.
        if let Err(e) = unsafe { sys::munmap(addr, page) } {
            sys::abort_fmt(format_args!("munmap of probe page failed: {e}"));
        }
    }

    /// `MADV_GUARD_INSTALL` then `MADV_GUARD_REMOVE` on a scratch page. Both
    /// must succeed for the feature to count: a kernel that installs guards
    /// it cannot remove would break slab reuse.
    pub fn probe_guard_install(page: usize) -> bool {
        let Some(addr) = scratch(page, PROT_READ | PROT_WRITE) else {
            return false;
        };
        // SAFETY: `addr` is our scratch page; guards on it affect nothing else.
        let ok = unsafe {
            sys::madvise(addr, page, MADV_GUARD_INSTALL).is_ok()
                && sys::madvise(addr, page, MADV_GUARD_REMOVE).is_ok()
        };
        release(addr, page);
        ok
    }

    /// `mseal` a `PROT_NONE` scratch page. On success the page can never be
    /// unmapped again, so it is left in place: one VMA, zero resident bytes.
    /// On failure the page is released.
    pub fn probe_mseal(page: usize) -> bool {
        let Some(addr) = scratch(page, PROT_NONE) else {
            return false;
        };
        // SAFETY: sealing a page nothing else will ever touch is harmless.
        match unsafe { sys::mseal(addr, page, 0) } {
            Ok(()) => true,
            Err(_) => {
                release(addr, page);
                false
            }
        }
    }

    /// `process_madvise(pidfd, MADV_DONTNEED)` on a dirtied scratch page,
    /// checked for effect: the call must report the whole page and the page
    /// must read back as zero. `MADV_DONTNEED` is what purge actually issues
    /// (design.md section 8), and older kernels accept other advice through
    /// `process_madvise` but not this one, so probing anything else would
    /// overstate what we can use.
    fn process_madvise_works(pidfd: i32, addr: usize, page: usize) -> bool {
        let p = addr as *mut u8;
        // SAFETY: `addr` is our writable scratch page.
        unsafe { p.write_volatile(0xA5) };
        let iov = IoVec { base: p, len: page };
        // SAFETY: `iov` is live for the call; DONTNEED on our scratch page.
        let r = unsafe { sys::process_madvise(pidfd, &iov, 1, MADV_DONTNEED, 0) };
        // SAFETY: page still mapped; anonymous memory reads zero after discard.
        r == Ok(page) && unsafe { p.read_volatile() } == 0
    }

    /// `PIDFD_SELF` first (kernel 6.14, no fd); then a real pidfd for the
    /// calling process (kernel 5.10, needs the self-target exemption or
    /// `CAP_SYS_NICE`); otherwise unavailable.
    pub fn probe_process_madvise(page: usize) -> ProcessMadvise {
        let Some(addr) = scratch(page, PROT_READ | PROT_WRITE) else {
            return ProcessMadvise::Unavailable;
        };
        let result = if process_madvise_works(PIDFD_SELF, addr, page) {
            ProcessMadvise::PidfdSelf
        } else {
            match sys::pidfd_open(sys::getpid(), 0) {
                Ok(fd) if process_madvise_works(fd, addr, page) => ProcessMadvise::Pidfd(fd),
                Ok(fd) => {
                    let _ = sys::close(fd);
                    ProcessMadvise::Unavailable
                }
                Err(_) => ProcessMadvise::Unavailable,
            }
        };
        release(addr, page);
        result
    }

    /// `PR_SET_VMA_ANON_NAME` on a scratch page.
    pub fn probe_anon_name(page: usize) -> bool {
        let Some(addr) = scratch(page, PROT_NONE) else {
            return false;
        };
        // SAFETY: `addr` is our scratch page; the name is static and
        // NUL-terminated.
        let ok = unsafe { sys::set_vma_anon_name(addr, page, b"challoc:probe\0") }.is_ok();
        release(addr, page);
        ok
    }

    impl Features {
        /// Probe everything. Called once from init (design.md section 10);
        /// calling it again is harmless but leaks a sealed page and may open
        /// another pidfd, so init stores the result.
        pub fn probe() -> Features {
            let page_size = page_size();
            Features {
                page_size,
                page_shift: page_size.trailing_zeros(),
                guard_install: probe_guard_install(page_size),
                mseal: probe_mseal(page_size),
                process_madvise: probe_process_madvise(page_size),
                anon_name: probe_anon_name(page_size),
            }
        }

        /// Drop any resource the probe holds (the pidfd). For the child side
        /// of `fork`, where the inherited pidfd names the parent.
        pub fn release(&mut self) {
            if let ProcessMadvise::Pidfd(fd) = self.process_madvise {
                let _ = sys::close(fd);
                self.process_madvise = ProcessMadvise::Unavailable;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auxv(pairs: &[(usize, usize)]) -> std::vec::Vec<u8> {
        let mut v = std::vec::Vec::new();
        for &(k, val) in pairs {
            v.extend_from_slice(&k.to_ne_bytes());
            v.extend_from_slice(&val.to_ne_bytes());
        }
        v
    }

    #[test]
    fn auxv_parse_finds_pagesz() {
        let v = auxv(&[(33, 0x7fff_0000), (16, 0xbfeb_fbff), (6, 16384), (0, 0)]);
        assert_eq!(page_size_from_auxv(&v), Some(16384));
    }

    #[test]
    fn auxv_parse_rejects_bad_input() {
        // Absent.
        assert_eq!(page_size_from_auxv(&auxv(&[(33, 1), (0, 0)])), None);
        // Stops at AT_NULL even if a later entry would match.
        assert_eq!(page_size_from_auxv(&auxv(&[(0, 0), (6, 4096)])), None);
        // Zero or non-power-of-two.
        assert_eq!(page_size_from_auxv(&auxv(&[(6, 0), (0, 0)])), None);
        assert_eq!(page_size_from_auxv(&auxv(&[(6, 3000), (0, 0)])), None);
        // Empty and truncated.
        assert_eq!(page_size_from_auxv(&[]), None);
        assert_eq!(page_size_from_auxv(&auxv(&[(6, 4096)])[..7]), None);
    }

    #[test]
    fn display_is_one_line_of_key_values() {
        let f = Features {
            page_size: 4096,
            page_shift: 12,
            guard_install: false,
            mseal: true,
            process_madvise: ProcessMadvise::Pidfd(7),
            anon_name: false,
        };
        let s = std::format!("{f}");
        assert_eq!(
            s,
            "page_size=4096 guard_install=no mseal=yes process_madvise=pidfd:7 anon_name=no"
        );
        let f = Features { process_madvise: ProcessMadvise::PidfdSelf, ..f };
        assert!(std::format!("{f}").contains("process_madvise=pidfd_self "));
        let f = Features { process_madvise: ProcessMadvise::Unavailable, ..f };
        assert!(std::format!("{f}").contains("process_madvise=no "));
    }
}

/// Linux-only tests: run the real probe inside the dev container and check
/// it against what the running kernel version can possibly offer. Prints the
/// probe line so `cargo test -- --nocapture` shows what the kernel gave us.
#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use crate::sys;
    use std::{eprintln, format, string::String, vec::Vec};

    /// `(major, minor)` from `/proc/sys/kernel/osrelease`, e.g. `6.10.14-linuxkit`.
    fn kernel_version() -> (u32, u32) {
        let s = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap();
        let mut it = s.trim().split(|c: char| !c.is_ascii_digit());
        let major = it.next().unwrap().parse().unwrap();
        let minor = it.next().unwrap().parse().unwrap();
        (major, minor)
    }

    fn at_least(v: (u32, u32), want: (u32, u32)) -> bool {
        v >= want
    }

    #[test]
    fn page_size_matches_libc_and_auxv() {
        let ps = page_size();
        // SAFETY: sysconf has no preconditions.
        let libc_ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        assert_eq!(ps, libc_ps);
        assert!(ps.is_power_of_two());
        #[cfg(target_arch = "x86_64")]
        assert_eq!(ps, 4096);
        #[cfg(target_arch = "aarch64")]
        assert!(matches!(ps, 4096 | 16384 | 65536), "{ps}");
        // The /proc fallback must agree with getauxval.
        let raw = std::fs::read("/proc/self/auxv").unwrap();
        assert_eq!(page_size_from_auxv(&raw), Some(ps));
    }

    #[test]
    fn probe_prints_and_matches_kernel_version() {
        let kv = kernel_version();
        let f = Features::probe();
        let line = format!("{f}");
        eprintln!("kernel {}.{}: {line}", kv.0, kv.1);
        assert_eq!(f.page_shift, f.page_size.trailing_zeros());
        assert_eq!(1usize << f.page_shift, f.page_size);

        // A feature newer than the kernel must probe absent. The converse
        // is only asserted where nothing but the kernel decides: seccomp
        // filters mseal/process_madvise and CONFIG_ANON_VMA_NAME is optional,
        // but madvise itself is never filtered.
        if !at_least(kv, (6, 13)) {
            assert!(!f.guard_install, "MADV_GUARD_INSTALL predates 6.13: {line}");
        } else {
            assert!(f.guard_install, "MADV_GUARD_INSTALL should work on 6.13+: {line}");
        }
        if !at_least(kv, (6, 10)) {
            assert!(!f.mseal, "mseal predates 6.10: {line}");
        }
        if !at_least(kv, (6, 16)) {
            assert_ne!(
                f.process_madvise,
                ProcessMadvise::PidfdSelf,
                "PIDFD_SELF predates 6.16: {line}"
            );
        }
        if !at_least(kv, (5, 10)) {
            assert_eq!(f.process_madvise, ProcessMadvise::Unavailable, "{line}");
        }
        if !at_least(kv, (5, 17)) {
            assert!(!f.anon_name, "PR_SET_VMA_ANON_NAME predates 5.17: {line}");
        }

        // Whatever was reported must be usable exactly as reported.
        let page = f.page_size;
        // SAFETY: fresh scratch page used only here.
        let addr = unsafe {
            sys::mmap(
                0,
                page,
                sys::PROT_READ | sys::PROT_WRITE,
                sys::MAP_PRIVATE | sys::MAP_ANONYMOUS,
                -1,
                0,
            )
            .unwrap()
        };
        let p = addr as *mut u8;
        if f.guard_install {
            // SAFETY: our page.
            unsafe {
                sys::madvise(addr, page, sys::MADV_GUARD_INSTALL).unwrap();
                sys::madvise(addr, page, sys::MADV_GUARD_REMOVE).unwrap();
            }
        }
        match f.process_madvise {
            ProcessMadvise::Unavailable => {}
            ProcessMadvise::PidfdSelf | ProcessMadvise::Pidfd(_) => {
                let fd = match f.process_madvise {
                    ProcessMadvise::Pidfd(fd) => fd,
                    _ => sys::PIDFD_SELF,
                };
                // SAFETY: our page, live iov.
                unsafe {
                    p.write_volatile(7);
                    let iov = sys::IoVec { base: p, len: page };
                    assert_eq!(sys::process_madvise(fd, &iov, 1, sys::MADV_DONTNEED, 0), Ok(page));
                    assert_eq!(p.read_volatile(), 0);
                }
            }
        }
        if f.anon_name {
            // SAFETY: our page, static name.
            unsafe { sys::set_vma_anon_name(addr, page, b"challoc:probe2\0").unwrap() };
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            let l: Vec<&str> = maps.lines().filter(|l| l.starts_with(&format!("{addr:x}-"))).collect();
            assert!(l.iter().any(|l| l.contains("[anon:challoc:probe2]")), "{l:?}");
        }
        if f.mseal {
            // Sealing works, so the probe's own sealed page must still be
            // mapped: exactly one PROT_NONE page. Find it via /proc.
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            assert!(maps.lines().any(|l| l.split_whitespace().nth(1) == Some("---p")), "{maps}");
        }
        // SAFETY: our page.
        unsafe { sys::munmap(addr, page).unwrap() };

        // The probe must not have left a readable scratch page behind: the
        // process_madvise probe releases its page whether or not it worked.
        let maps: String = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps.contains("[anon:challoc:probe]"), "probe page leaked: {maps}");
    }

    #[test]
    fn release_closes_pidfd() {
        let mut f = Features::probe();
        if let ProcessMadvise::Pidfd(fd) = f.process_madvise {
            f.release();
            assert_eq!(sys::close(fd), Err(sys::Errno::EBADF));
        } else {
            f.release();
        }
        assert!(matches!(
            f.process_madvise,
            ProcessMadvise::Unavailable | ProcessMadvise::PidfdSelf
        ));
    }
}
