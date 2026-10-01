//! Kernel feature probing (design.md section 10, step 1) and the feature
//! line that reports it.
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
//! unmapped again). The pure decoders are unit tested against captured
//! inputs; the probes themselves run against the kernel the tests run on.
//!
//! The line is a table: [`Features::words`] yields every `(key, value)`
//! pair in order, and `Display`, [`FeatureLine`] and the tests all iterate
//! it, so a new kernel feature is one probe, one field and one row.

use core::fmt;

pub use super::thp_host::{Thp, ThpSetting};
use super::yn;

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

/// Bit sets of sizes as `64K,2M`, or `none`.
struct SizeSet(u64);

impl fmt::Display for SizeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("none");
        }
        let mut first = true;
        for lg in 0..64u8 {
            if self.0 & (1u64 << lg) != 0 {
                if !first {
                    f.write_str(",")?;
                }
                first = false;
                write!(f, "{}", LgSize(lg))?;
            }
        }
        Ok(())
    }
}

/// `1 << lg` bytes as `64K`, `2M`, `1G`.
struct LgSize(u8);

impl fmt::Display for LgSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            lg if lg >= 30 => write!(f, "{}G", 1u64 << (lg - 30)),
            lg if lg >= 20 => write!(f, "{}M", 1u64 << (lg - 20)),
            lg if lg >= 10 => write!(f, "{}K", 1u64 << (lg - 10)),
            lg => write!(f, "{}", 1u64 << lg),
        }
    }
}

/// One value of the feature line, printed after its `key=`. Every
/// variant prints one token without spaces or `=`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// `yes` or `no`.
    YesNo(bool),
    /// A plain number (`page_size`, `rseq_size`).
    Num(u64),
    /// A fixed word (`madvise`, `kernel-disabled`).
    Word(&'static str),
    /// The `process_madvise` target: `no`, `pidfd_self` or `pidfd:<fd>`.
    Target(ProcessMadvise),
    /// A bit set over `log2(size)` as `64K,2M`, or `none`.
    Sizes(u64),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Value::YesNo(b) => f.write_str(yn(b)),
            Value::Num(n) => write!(f, "{n}"),
            Value::Word(w) => f.write_str(w),
            Value::Target(ProcessMadvise::Unavailable) => f.write_str("no"),
            Value::Target(ProcessMadvise::PidfdSelf) => f.write_str("pidfd_self"),
            Value::Target(ProcessMadvise::Pidfd(fd)) => write!(f, "pidfd:{fd}"),
            Value::Sizes(set) => write!(f, "{}", SizeSet(set)),
        }
    }
}

/// `key=value` pairs separated by single spaces: the shape of the whole
/// line and of each part that prints on its own (`Thp`).
pub fn write_words(f: &mut fmt::Formatter<'_>, words: impl Iterator<Item = (&'static str, Value)>) -> fmt::Result {
    for (i, (key, value)) in words.enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{key}={value}")?;
    }
    Ok(())
}

/// The `rseq=` and `rseq_size=` words from `rseq::probe` (design 17; the
/// three weak symbols the slice extension reads, read once at init, and
/// this thread's `cpu_id`). `rseq=yes` when glibc registered an area
/// (`__rseq_size` covers the original 20-byte struct, [`rseq::BASE_SIZE`])
/// and the kernel has written this thread's `cpu_id`; `rseq_size` is the
/// symbol's value even when the registration did not take (`0` without
/// one: `GLIBC_TUNABLES=glibc.pthread.rseq=0` or a kernel without
/// `CONFIG_RSEQ`; 33 on the 7.0 GCE kernels, 20 on a kernel that reports
/// no feature size).
///
/// [`rseq::BASE_SIZE`]: crate::rseq::BASE_SIZE
pub fn rseq_words(p: &crate::rseq::Probe) -> impl Iterator<Item = (&'static str, Value)> {
    let registered = p.present && p.registered && p.size >= crate::rseq::BASE_SIZE;
    [("rseq", Value::YesNo(registered)), ("rseq_size", Value::Num(u64::from(p.size)))].into_iter()
}

/// The kernel's answer for the rseq time slice extension (design 17, "Time
/// slice extension"), the `slice_ext=` key of the
/// feature line. The probe itself is `slice::probe` (`slice.rs`, read
/// here through its public API, never duplicated): glibc's
/// registration, its size against `slice_ctrl`, and bit 4 of the thread's
/// `flags`. `Features::probe` records what the kernel offers before the
/// configuration is read; `Features::note_slice_ext` folds in the
/// configured outcome (`slice::configure`'s verdict) once `config::load`
/// has run, so the word says what is in force, not only what could be.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SliceExt {
    /// The extension is requested by every lock holder of this process
    /// (`slice::on()`): the kernel has it and the key allowed it.
    On,
    /// The kernel offers it (bit 4 set) and the process does not use it
    /// (`slice_ext:off`, or the report is built before `configure` ran).
    Available,
    /// The kernel offers it, the `prctl` enable failed on the init thread.
    PrctlFailed,
    /// `__rseq_size >= 32` but bit 4 clear: the kernel has the option
    /// compiled out or disabled (`rseq_slice_ext=0`); Ubuntu's
    /// `7.0.0-1011-gcp` on the GCE VMs.
    KernelDisabled,
    /// `__rseq_size < 32`: the kernel does not write `slice_ctrl` (6.18
    /// and earlier).
    NoSliceCtrl,
    /// No rseq registration at all.
    NoRseq,
}

impl SliceExt {
    /// The word in the line; the same vocabulary as the `Slice extension:`
    /// report line's verdict words, plus `on` and `available`.
    pub const fn name(self) -> &'static str {
        match self {
            SliceExt::On => "on",
            SliceExt::Available => "available",
            SliceExt::PrctlFailed => "prctl-failed",
            SliceExt::KernelDisabled => "kernel-disabled",
            SliceExt::NoSliceCtrl => "no-slice_ctrl",
            SliceExt::NoRseq => "no-rseq",
        }
    }

    /// What the kernel offers, from the probe's verdict alone (`Enabled`
    /// there means "available": the probe does not enable). Pure, host
    /// tested.
    pub const fn from_probe(v: crate::slice::Verdict) -> SliceExt {
        use crate::slice::Verdict;
        match v {
            Verdict::NoRseq => SliceExt::NoRseq,
            Verdict::NoSliceCtrl => SliceExt::NoSliceCtrl,
            Verdict::KernelDisabled => SliceExt::KernelDisabled,
            Verdict::PrctlFailed => SliceExt::PrctlFailed,
            Verdict::Enabled => SliceExt::Available,
            // `Unprobed`, `Key`, `NoSyscall` are not kernel answers; a
            // probe never returns them.
            Verdict::Unprobed | Verdict::Key | Verdict::NoSyscall => SliceExt::NoRseq,
        }
    }

    /// Fold the configured outcome into the kernel's answer: `Enabled`
    /// makes it `on`, a failed `prctl` says so, a key that turned it off
    /// (or a kernel answer that never reached the `prctl`) leaves the
    /// kernel's word. Pure.
    pub const fn with_configured(self, v: crate::slice::Verdict) -> SliceExt {
        use crate::slice::Verdict;
        match v {
            Verdict::Enabled => SliceExt::On,
            Verdict::PrctlFailed => SliceExt::PrctlFailed,
            _ => self,
        }
    }
}

impl fmt::Display for SliceExt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slice_ext={}", self.name())
    }
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
    /// `process_madvise(MADV_FREE)` through the same target works (probed
    /// separately: the two-advice purge of design 8 issues `MADV_FREE`
    /// under the cgroup headroom band, and a kernel may take
    /// `MADV_DONTNEED` from a self-targeting `process_madvise` but refuse
    /// `MADV_FREE`; lazy batches then go through the `madvise` loop).
    /// Always `false` when `process_madvise` is unavailable.
    pub process_madvise_free: bool,
    /// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME)` works
    /// (kernel 5.17+ with `CONFIG_ANON_VMA_NAME`).
    pub anon_name: bool,
    /// The transparent-huge-page configuration: what the `thp` key can
    /// turn on.
    pub thp: Thp,
    /// glibc's rseq registration as `rseq::probe` read it at init: the
    /// `rseq` and `rseq_size` keys ([`rseq_words`]).
    pub rseq: crate::rseq::Probe,
    /// The kernel's answer for the rseq time slice extension and whether
    /// the process uses it (`slice.rs` decides,
    /// this is the report of it).
    pub slice_ext: SliceExt,
    /// Pointer authentication (`sys::pac::available`, `HWCAP_PACA` and
    /// `HWCAP_PACG`): what `pac_pointers` can turn on.
    pub pac: bool,
}

impl Features {
    /// Every key of the line in order: the kernel probes, then `pac`.
    /// Values are `yes`/`no` except `page_size`,
    /// `process_madvise` (`no`, `pidfd_self`, or `pidfd:<fd>`), the THP
    /// keys (`thp=never|madvise|always|none`, `mthp=64K,2M|none`),
    /// `rseq_size` (a byte count, `0` without a
    /// registration), `slice_ext` (`on`, `available`, `prctl-failed`,
    /// `kernel-disabled`, `no-slice_ctrl`, `no-rseq`).
    /// `tests/c/features.c` lists the same keys.
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        [
            ("page_size", Value::Num(self.page_size as u64)),
            ("guard_install", Value::YesNo(self.guard_install)),
            ("mseal", Value::YesNo(self.mseal)),
            ("process_madvise", Value::Target(self.process_madvise)),
            ("process_madvise_free", Value::YesNo(self.process_madvise_free)),
            ("anon_name", Value::YesNo(self.anon_name)),
        ]
        .into_iter()
        .chain(self.thp.words())
        .chain(rseq_words(&self.rseq))
        .chain([("slice_ext", Value::Word(self.slice_ext.name()))])
        .chain([("pac", Value::YesNo(self.pac))])
    }
}

impl fmt::Display for Features {
    /// One line, `key=value` separated by spaces, for `challoc_features()`,
    /// `malloc_stats_print` and abort messages: [`Features::words`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_words(f, self.words())
    }
}

/// The feature line as a NUL-terminated string, built once at init for
/// `challoc_features()` (design.md section 16) and reused by
/// `malloc_stats_print`, so the two report the same bytes. Rebuilt in a
/// fork child, where the purge backend is re-probed.
pub struct FeatureLine {
    buf: [u8; FeatureLine::CAP],
    len: usize,
}

impl FeatureLine {
    /// Room for the line and its NUL; the longest possible line (every
    /// probe `yes`, `pidfd:2147483647`, every THP size listed, the longest
    /// `slice_ext` word) is under `CAP - 64` bytes (`feature_line_fits`).
    pub const CAP: usize = 640;

    /// Format `features` into a fresh line. A line that did not fit is cut,
    /// which the tests would notice; it cannot happen with the current keys.
    pub fn build(features: &Features) -> FeatureLine {
        use fmt::Write as _;
        let mut line = FeatureLine { buf: [0; Self::CAP], len: 0 };
        let _ = write!(line, "{features}");
        line.buf[line.len] = 0;
        line
    }

    /// The line without its NUL.
    pub fn as_str(&self) -> &str {
        // Only `str` fragments were copied, cut at a fragment boundary.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// The NUL-terminated line, for C callers.
    pub fn as_ptr(&self) -> *const core::ffi::c_char {
        self.buf.as_ptr() as *const core::ffi::c_char
    }
}

impl fmt::Write for FeatureLine {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = Self::CAP - 1 - self.len;
        if s.len() > room {
            return Err(fmt::Error);
        }
        self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}

pub use linux::*;

mod linux {
    use super::{Features, ProcessMadvise};
    use crate::sys::auxv::{AT_PAGESZ, getauxval};
    use crate::sys::{
        self, IoVec, MADV_DONTNEED, MADV_FREE, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE, MAP_ANONYMOUS,
        MAP_PRIVATE, PIDFD_SELF, PROT_NONE, PROT_READ, PROT_WRITE,
    };

    /// The system page size, `getauxval(AT_PAGESZ)`. Without it, abort:
    /// guessing a page size would corrupt every mapping we make.
    pub fn page_size() -> usize {
        let v = getauxval(AT_PAGESZ);
        if v != 0 && v.is_power_of_two() {
            return v;
        }
        sys::abort("cannot determine the page size (AT_PAGESZ missing)")
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

    /// `process_madvise(pidfd, MADV_FREE)` on a dirtied scratch page must
    /// report the whole page. The contents cannot be checked (`MADV_FREE`
    /// leaves them in place until reclaim), so the return value is the
    /// only evidence; a kernel that refuses the advice through
    /// `process_madvise` answers `EINVAL`.
    fn process_madvise_free_works(pidfd: i32, addr: usize, page: usize) -> bool {
        let p = addr as *mut u8;
        // SAFETY: `addr` is our writable scratch page.
        unsafe { p.write_volatile(0xA5) };
        let iov = IoVec { base: p, len: page };
        // SAFETY: `iov` is live for the call; FREE on our scratch page.
        let r = unsafe { sys::process_madvise(pidfd, &iov, 1, MADV_FREE, 0) };
        r == Ok(page)
    }

    /// `PIDFD_SELF` first (kernel 6.15, no fd); then a real pidfd for the
    /// calling process (kernel 5.10, needs the self-target exemption or
    /// `CAP_SYS_NICE`); otherwise unavailable. The second value is whether
    /// `MADV_FREE` goes through the same target.
    pub fn probe_process_madvise(page: usize) -> (ProcessMadvise, bool) {
        let Some(addr) = scratch(page, PROT_READ | PROT_WRITE) else {
            return (ProcessMadvise::Unavailable, false);
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
        let lazy = match result {
            ProcessMadvise::Unavailable => false,
            ProcessMadvise::PidfdSelf => process_madvise_free_works(PIDFD_SELF, addr, page),
            ProcessMadvise::Pidfd(fd) => process_madvise_free_works(fd, addr, page),
        };
        release(addr, page);
        (result, lazy)
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
        /// another pidfd, so init stores the result. Pure apart from those
        /// two resources.
        pub fn probe() -> Features {
            let page_size = page_size();
            let (process_madvise, process_madvise_free) = probe_process_madvise(page_size);
            Features {
                page_size,
                page_shift: page_size.trailing_zeros(),
                guard_install: probe_guard_install(page_size),
                mseal: probe_mseal(page_size),
                process_madvise,
                process_madvise_free,
                anon_name: probe_anon_name(page_size),
                thp: crate::sys::thp_host::probe(page_size),
                rseq: crate::rseq::probe(),
                // The kernel's answer alone: `config::load` has not run
                // yet, so nothing is enabled; `note_slice_ext` follows.
                slice_ext: super::SliceExt::from_probe(crate::slice::probe().verdict()),
                pac: crate::sys::pac::available(),
            }
        }

        /// Fold `slice::configure`'s verdict into the `slice_ext=` word.
        /// Init calls it after `config::load`
        /// (which runs `configure` on the init thread) and before the
        /// line is built, so the line says `on` when lock holders request
        /// the extension.
        pub fn note_slice_ext(&mut self) {
            self.slice_ext = self.slice_ext.with_configured(crate::slice::verdict());
        }

        /// Drop any resource the probe holds (the pidfd). For the child side
        /// of `fork`, where the inherited pidfd names the parent.
        pub fn release(&mut self) {
            if let ProcessMadvise::Pidfd(fd) = self.process_madvise {
                let _ = sys::close(fd);
                self.process_madvise = ProcessMadvise::Unavailable;
                self.process_madvise_free = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rseq::Probe;

    /// What `rseq::probe` reads on a host whose glibc registered `size`
    /// bytes at `offset` (registered when the size is not 0).
    fn rseq_probe(size: u32, offset: isize) -> Probe {
        Probe { present: true, size, flags: 0, offset, registered: size != 0 }
    }

    #[test]
    fn display_is_one_line_of_key_values() {
        let f = Features {
            page_size: 4096,
            page_shift: 12,
            guard_install: false,
            mseal: true,
            process_madvise: ProcessMadvise::Pidfd(7),
            process_madvise_free: true,
            anon_name: false,
            thp: Thp { enabled: Some(ThpSetting::Madvise), lg_pmd: 21, listed: (1 << 16) | (1 << 21), always: 0, madvise: (1 << 16) | (1 << 21) },
            rseq: rseq_probe(33, 192),
            slice_ext: SliceExt::KernelDisabled,
            pac: true,
        };
        let s = std::format!("{f}");
        assert_eq!(
            s,
            "page_size=4096 guard_install=no mseal=yes process_madvise=pidfd:7 process_madvise_free=yes anon_name=no thp=madvise \
             mthp=64K,2M rseq=yes rseq_size=33 slice_ext=kernel-disabled pac=yes"
        );
        // A kernel that reports no feature size (6.10): registered, no
        // `slice_ctrl`.
        let f = Features { rseq: rseq_probe(20, 192), slice_ext: SliceExt::NoSliceCtrl, ..f };
        assert!(std::format!("{f}").contains(" mthp=64K,2M rseq=yes rseq_size=20 slice_ext=no-slice_ctrl pac=yes"));
        let f = Features { rseq: Probe::NONE, slice_ext: SliceExt::NoRseq, ..f };
        assert!(std::format!("{f}").contains(" rseq=no rseq_size=0 slice_ext=no-rseq "));
        // A 7.0.14 built with the option (a GCE C4): on under the
        // default key, available under `slice_ext:off`.
        let f = Features { rseq: rseq_probe(33, -192), slice_ext: SliceExt::On, ..f };
        assert!(std::format!("{f}").contains(" rseq_size=33 slice_ext=on "));
        let f = Features { slice_ext: SliceExt::Available, ..f };
        assert!(std::format!("{f}").contains(" rseq_size=33 slice_ext=available "));
        let f = Features { thp: Thp::NONE, ..f };
        assert!(std::format!("{f}").contains(" thp=none mthp=none rseq=yes "));
        let f = Features { thp: Thp { enabled: Some(ThpSetting::Never), listed: 1 << 21, always: 0, madvise: 0, lg_pmd: 21 }, ..f };
        assert!(std::format!("{f}").contains(" thp=never mthp=none rseq=yes "));
        let f = Features { process_madvise: ProcessMadvise::PidfdSelf, ..f };
        assert!(std::format!("{f}").contains("process_madvise=pidfd_self process_madvise_free=yes "));
        let f = Features { process_madvise: ProcessMadvise::Unavailable, process_madvise_free: false, ..f };
        assert!(std::format!("{f}").contains("process_madvise=no process_madvise_free=no "));
    }

    #[test]
    fn feature_line_is_the_display_with_a_nul() {
        let f = Features {
            page_size: 16384,
            page_shift: 14,
            guard_install: true,
            mseal: true,
            process_madvise: ProcessMadvise::PidfdSelf,
            process_madvise_free: true,
            anon_name: true,
            thp: Thp::NONE,
            rseq: Probe::NONE,
            slice_ext: SliceExt::NoRseq,
            pac: true,
        };
        let line = FeatureLine::build(&f);
        assert_eq!(line.as_str(), std::format!("{f}"));
        // SAFETY: `as_ptr` names the line's own buffer, which holds `len`
        // bytes of text and the NUL after them.
        let bytes = unsafe { core::slice::from_raw_parts(line.as_ptr() as *const u8, line.len + 1) };
        assert_eq!(bytes[line.len], 0);
        assert_eq!(&bytes[..line.len], line.as_str().as_bytes());
        assert!(!line.as_str().contains('\n'));
    }

    #[test]
    fn feature_line_fits() {
        // The longest line: `pac=yes` is the longer word.
        let f = Features {
            page_size: 65536,
            page_shift: 16,
            guard_install: true,
            mseal: true,
            process_madvise: ProcessMadvise::Pidfd(i32::MAX),
            process_madvise_free: true,
            anon_name: true,
            // A 64 KiB-page kernel listing every size from 128K to 512M.
            thp: Thp {
                enabled: Some(ThpSetting::Madvise),
                lg_pmd: 29,
                listed: ((1u64 << 30) - 1) & !((1u64 << 17) - 1),
                always: ((1u64 << 30) - 1) & !((1u64 << 17) - 1),
                madvise: 0,
            },
            // The longest words.
            rseq: Probe { present: true, size: u32::MAX, flags: u32::MAX, offset: isize::MIN, registered: true },
            slice_ext: SliceExt::KernelDisabled,
            pac: true,
        };
        let s = std::format!("{f}");
        assert!(s.len() < FeatureLine::CAP - 64, "{} bytes: {s}", s.len());
        assert_eq!(FeatureLine::build(&f).as_str(), s);
        // The table is what the line is made of: the same bytes, word
        // by word.
        let by_words: usize = f.words().map(|(k, v)| k.len() + 1 + std::format!("{v}").len() + 1).sum::<usize>() - 1;
        assert_eq!(by_words, s.len());
    }

    /// The key table: every key of `tests/c/features.c`, once, in order;
    /// every value one token. Adding a feature means one row here, one
    /// in `features.c`, and the probe.
    #[test]
    fn words_are_the_key_table() {
        let kernel = [
            "page_size", "guard_install", "mseal", "process_madvise", "process_madvise_free", "anon_name",
            "thp", "mthp", "rseq", "rseq_size", "slice_ext", "pac",
        ];
        for pac in [true, false] {
            let f = Features {
                page_size: 4096,
                page_shift: 12,
                guard_install: true,
                mseal: false,
                process_madvise: ProcessMadvise::PidfdSelf,
                process_madvise_free: true,
                anon_name: true,
                thp: Thp::NONE,
                rseq: rseq_probe(33, 192),
                slice_ext: SliceExt::Available,
                pac,
            };
            let keys: std::vec::Vec<&str> = f.words().map(|(k, _)| k).collect();
            assert_eq!(keys, kernel);
            let mut seen = std::collections::BTreeSet::new();
            for (k, v) in f.words() {
                assert!(seen.insert(k), "{k} twice");
                let v = std::format!("{v}");
                assert!(!k.is_empty() && !v.is_empty() && !k.contains([' ', '=']) && !v.contains([' ', '=']), "{k}={v}");
            }
            let joined: std::vec::Vec<std::string::String> = f.words().map(|(k, v)| std::format!("{k}={v}")).collect();
            assert_eq!(std::format!("{f}"), joined.join(" "));
            assert_eq!(std::format!("{}", f.thp), joined[6..8].join(" "));
        }
    }

    /// The rseq words on the values the prerequisites note recorded: the
    /// GCE VMs (glibc 2.44 on 7.0, `__rseq_size=33`, offset 192 on aarch64
    /// and -192/-256 on x86_64), a 6.10 kernel (`__rseq_size=20`), no
    /// symbols, a registration the kernel never wrote, and a size below the
    /// original struct.
    #[test]
    fn rseq_words_follow_size_and_registration() {
        let line = |p: &Probe| -> std::string::String {
            let v: std::vec::Vec<std::string::String> = rseq_words(p).map(|(k, v)| std::format!("{k}={v}")).collect();
            v.join(" ")
        };
        for off in [192isize, -192, -256] {
            assert_eq!(line(&rseq_probe(33, off)), "rseq=yes rseq_size=33");
        }
        assert_eq!(line(&rseq_probe(20, 192)), "rseq=yes rseq_size=20");
        assert_eq!(line(&Probe::NONE), "rseq=no rseq_size=0");
        assert_eq!(line(&rseq_probe(0, 0)), "rseq=no rseq_size=0", "symbols present, nothing registered");
        // The kernel did not write the area: no registration, whatever the size says.
        let p = Probe { registered: false, ..rseq_probe(33, -192) };
        assert_eq!(line(&p), "rseq=no rseq_size=33");
        // Unknown flags do not change what the line says exists.
        let p = Probe { flags: 1, ..rseq_probe(33, 192) };
        assert_eq!(line(&p), "rseq=yes rseq_size=33");
        // Below the original struct nothing is registered.
        assert_eq!(line(&rseq_probe(19, 192)), "rseq=no rseq_size=19");
        assert_eq!(crate::rseq::BASE_SIZE, 20);
    }

    /// The `slice_ext=` word from the probe's
    /// verdict and the configured one: the 7.0 GCE kernels (`33` bytes,
    /// bit 4 clear) read `kernel-disabled` whatever the key; a 6.18 or
    /// 6.10 kernel `no-slice_ctrl`; no registration `no-rseq`; a kernel
    /// with the option reads `on` once `configure` enabled the init thread,
    /// `available` under `slice_ext:off` (or before `configure`), and
    /// `prctl-failed` when the enable was refused. Every word is a single
    /// token without spaces or `=`.
    #[test]
    fn slice_ext_words_follow_the_verdicts() {
        use crate::slice::Verdict;
        let gce = SliceExt::from_probe(Verdict::KernelDisabled);
        assert_eq!(gce, SliceExt::KernelDisabled);
        for v in [Verdict::Unprobed, Verdict::Key, Verdict::KernelDisabled, Verdict::NoRseq] {
            assert_eq!(gce.with_configured(v), SliceExt::KernelDisabled, "{v:?}");
        }
        assert_eq!(SliceExt::from_probe(Verdict::NoSliceCtrl), SliceExt::NoSliceCtrl);
        assert_eq!(SliceExt::from_probe(Verdict::NoSliceCtrl).with_configured(Verdict::Key), SliceExt::NoSliceCtrl);
        assert_eq!(SliceExt::from_probe(Verdict::NoRseq), SliceExt::NoRseq);
        let has = SliceExt::from_probe(Verdict::Enabled);
        assert_eq!(has, SliceExt::Available);
        assert_eq!(has.with_configured(Verdict::Enabled), SliceExt::On);
        assert_eq!(has.with_configured(Verdict::Key), SliceExt::Available);
        assert_eq!(has.with_configured(Verdict::Unprobed), SliceExt::Available);
        assert_eq!(has.with_configured(Verdict::PrctlFailed), SliceExt::PrctlFailed);
        // `Probe::verdict` never yields these; they map to the weakest word.
        for v in [Verdict::Unprobed, Verdict::Key, Verdict::NoSyscall] {
            assert_eq!(SliceExt::from_probe(v), SliceExt::NoRseq, "{v:?}");
        }
        for w in [
            SliceExt::On,
            SliceExt::Available,
            SliceExt::PrctlFailed,
            SliceExt::KernelDisabled,
            SliceExt::NoSliceCtrl,
            SliceExt::NoRseq,
        ] {
            let name = w.name();
            assert!(!name.is_empty() && !name.contains(' ') && !name.contains('='), "{name}");
            assert_eq!(std::format!("{w}"), std::format!("slice_ext={name}"));
        }
        // The report line's words are the same vocabulary.
        assert_eq!(SliceExt::KernelDisabled.name(), Verdict::KernelDisabled.name());
        assert_eq!(SliceExt::NoSliceCtrl.name(), Verdict::NoSliceCtrl.name());
        assert_eq!(SliceExt::NoRseq.name(), Verdict::NoRseq.name());
        assert_eq!(SliceExt::PrctlFailed.name(), Verdict::PrctlFailed.name());
    }
}

/// Linux-only tests: run the real probe inside the dev container and check
/// it against what the running kernel version can possibly offer. Prints the
/// probe line so `cargo test -- --nocapture` shows what the kernel gave us.
#[cfg(test)]
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
    fn page_size_matches_libc() {
        let ps = page_size();
        // SAFETY: sysconf has no preconditions.
        let libc_ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        assert_eq!(ps, libc_ps);
        assert!(ps.is_power_of_two());
        #[cfg(target_arch = "x86_64")]
        assert_eq!(ps, 4096);
        #[cfg(target_arch = "aarch64")]
        assert!(matches!(ps, 4096 | 16384 | 65536), "{ps}");
    }

    /// The real probe and the running kernel's version, the line printed
    /// so `--nocapture` shows what the kernel gave us. Each test probes
    /// afresh (harmless: one more sealed page, perhaps one more pidfd).
    fn probed() -> ((u32, u32), Features, String) {
        let kv = kernel_version();
        let f = Features::probe();
        let line = format!("{f}");
        eprintln!("kernel {}.{}: {line}", kv.0, kv.1);
        (kv, f, line)
    }

    // A feature newer than the kernel must probe absent. The converse is
    // only asserted where nothing but the kernel decides: seccomp filters
    // mseal/process_madvise and CONFIG_ANON_VMA_NAME is optional, but
    // madvise itself is never filtered.

    /// The line prints and the page geometry is consistent. The name is
    /// the filter `bench/probes/calloc-zero-campaign.sh` runs.
    #[test]
    fn probe_prints_and_matches_kernel_version() {
        let (_, f, _) = probed();
        assert_eq!(f.page_shift, f.page_size.trailing_zeros());
        assert_eq!(1usize << f.page_shift, f.page_size);
    }

    #[test]
    fn probe_guard_install_follows_the_kernel_both_ways() {
        let (kv, f, line) = probed();
        if !at_least(kv, (6, 13)) {
            assert!(!f.guard_install, "MADV_GUARD_INSTALL predates 6.13: {line}");
        } else {
            assert!(f.guard_install, "MADV_GUARD_INSTALL should work on 6.13+: {line}");
        }
        if f.guard_install {
            // Usable exactly as reported.
            let page = f.page_size;
            let addr = sys::tests::map_anon(page);
            // SAFETY: our page, mapped above and unmapped here.
            unsafe {
                sys::madvise(addr, page, sys::MADV_GUARD_INSTALL).unwrap();
                sys::madvise(addr, page, sys::MADV_GUARD_REMOVE).unwrap();
                sys::munmap(addr, page).unwrap();
            }
        }
    }

    #[test]
    fn probe_mseal_predates_nothing_before_6_10_and_keeps_its_sealed_page() {
        let (kv, f, line) = probed();
        if !at_least(kv, (6, 10)) {
            assert!(!f.mseal, "mseal predates 6.10: {line}");
        }
        if f.mseal {
            // Sealing works, so the probe's own sealed page must still be
            // mapped: a PROT_NONE page. Find it via /proc.
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            assert!(maps.lines().any(|l| l.split_whitespace().nth(1) == Some("---p")), "{maps}");
        }
    }

    /// `PIDFD_SELF` is 6.15 (`sys::PIDFD_SELF`), `process_madvise` 5.10;
    /// whatever target was reported must work exactly as reported, and the
    /// `MADV_FREE` verdict must agree with a second call through it.
    #[test]
    fn probe_process_madvise_follows_the_kernel_and_is_usable() {
        let (kv, f, line) = probed();
        if !at_least(kv, (6, 15)) {
            assert_ne!(f.process_madvise, ProcessMadvise::PidfdSelf, "PIDFD_SELF predates 6.15: {line}");
        }
        if !at_least(kv, (5, 10)) {
            assert_eq!(f.process_madvise, ProcessMadvise::Unavailable, "{line}");
        }
        let page = f.page_size;
        let addr = sys::tests::map_anon(page);
        let p = addr as *mut u8;
        match f.process_madvise {
            ProcessMadvise::Unavailable => assert!(!f.process_madvise_free, "{line}"),
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
                    p.write_volatile(9);
                    let r = sys::process_madvise(fd, &iov, 1, sys::MADV_FREE, 0);
                    assert_eq!(r == Ok(page), f.process_madvise_free, "{line}: {r:?}");
                }
            }
        }
        // SAFETY: our page.
        unsafe { sys::munmap(addr, page).unwrap() };
    }

    #[test]
    fn probe_anon_name_follows_the_kernel_and_releases_its_page() {
        let (kv, f, line) = probed();
        if !at_least(kv, (5, 17)) {
            assert!(!f.anon_name, "PR_SET_VMA_ANON_NAME predates 5.17: {line}");
        }
        if f.anon_name {
            let page = f.page_size;
            let addr = sys::tests::map_anon(page);
            // SAFETY: our page, static name.
            unsafe { sys::set_vma_anon_name(addr, page, b"challoc:probe2\0").unwrap() };
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            let l: Vec<&str> = maps.lines().filter(|l| l.starts_with(&format!("{addr:x}-"))).collect();
            assert!(l.iter().any(|l| l.contains("[anon:challoc:probe2]")), "{l:?}");
            // SAFETY: our page.
            unsafe { sys::munmap(addr, page).unwrap() };
        }
        // The probe must not have left its named scratch page behind: it
        // releases the page whether or not the prctl worked.
        let maps: String = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps.contains("[anon:challoc:probe]"), "probe page leaked: {maps}");
    }

    /// The rseq words against glibc's own symbols through `dlsym` and the
    /// kernel's `AT_RSEQ_FEATURE_SIZE`: the values must be the symbols',
    /// the probe the line is built from must be `rseq::probe`'s, and a
    /// registration covers every field the kernel offers (the GCE 7.0 VMs:
    /// 33; a 6.10 kernel reports no feature size and glibc registers 20).
    /// Both ways: it runs on hosts of each kind.
    #[test]
    fn rseq_words_match_glibc_symbols_and_the_feature_size() {
        let (_, f, line) = probed();
        assert_eq!(f.rseq, crate::rseq::probe(), "the line's probe is the slice extension's");
        let sym = |name: &[u8]| -> *const u8 {
            // SAFETY: `dlsym` on a NUL-terminated name in the test.
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }.cast()
        };
        let (ps, po, pf) = (sym(b"__rseq_size\0"), sym(b"__rseq_offset\0"), sym(b"__rseq_flags\0"));
        if ps.is_null() {
            assert_eq!(f.rseq, crate::rseq::Probe::NONE, "no __rseq_size symbol: {line}");
            assert!(line.contains(" rseq=no rseq_size=0 "), "{line}");
            return;
        }
        // SAFETY: glibc's exported objects of these types.
        let (size, offset, flags) = unsafe { (ps.cast::<u32>().read(), po.cast::<isize>().read(), pf.cast::<u32>().read()) };
        assert!(f.rseq.present, "{line}");
        assert_eq!((f.rseq.size, f.rseq.offset, f.rseq.flags), (size, offset, flags), "{line}");
        let registered = size >= crate::rseq::BASE_SIZE && f.rseq.registered;
        eprintln!("rseq: __rseq_size={size} __rseq_offset={offset} __rseq_flags={flags} registered={}", f.rseq.registered);
        assert!(line.contains(&format!(" rseq={} rseq_size={size} ", sys::yn(registered))), "{line}");
        if size >= crate::rseq::BASE_SIZE {
            // glibc registered: the kernel wrote a CPU number.
            assert!(f.rseq.registered, "{line}");
            // SAFETY: this thread's registered area of `size` bytes.
            let cpu = unsafe { crate::rseq::cpu_id(crate::rseq::area(offset)) };
            // SAFETY: no preconditions; the count bounds `cpu_id`.
            let ncpu = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
            assert!((cpu as i64) < ncpu, "cpu_id {cpu} of {ncpu}");
        } else {
            assert!(!registered, "{line}");
        }
        // AT_RSEQ_FEATURE_SIZE: the fields the kernel offers.
        let feat = sys::auxv::getauxval(sys::auxv::AT_RSEQ_FEATURE_SIZE) as u32;
        if feat > crate::rseq::BASE_SIZE && size >= crate::rseq::BASE_SIZE {
            assert!(size >= feat, "kernel offers {feat} bytes, glibc registered {size}: {line}");
        }
    }

    /// The `slice_ext=` word against `slice::probe`: the probe, not
    /// enabled by `Features::probe` (`configure` has not run for this
    /// value), must read what the module's own probe says; the 7.0 GCE
    /// kernels read `kernel-disabled`, the Mac's 6.10 `no-slice_ctrl`.
    /// Both ways: a kernel with the option reads `available` here (the
    /// line of the shipped library says `on`).
    #[test]
    fn probe_slice_ext_matches_the_slice_module() {
        let (kv, f, line) = probed();
        let p = crate::slice::probe();
        assert_eq!(f.slice_ext, SliceExt::from_probe(p.verdict()), "{line}: {p:?}");
        let word = std::format!(" slice_ext={} ", f.slice_ext.name());
        assert!(line.contains(&word), "{line}");
        if p.size != 0 && p.size < crate::slice::SLICE_CTRL_END {
            assert_eq!(f.slice_ext, SliceExt::NoSliceCtrl, "{line}");
        }
        if p.size >= crate::slice::SLICE_CTRL_END && !p.available {
            assert_eq!(f.slice_ext, SliceExt::KernelDisabled, "{line}");
        }
        if p.available {
            assert!(at_least(kv, (7, 0)), "slice extension predates 7.0: {line}");
            assert_eq!(f.slice_ext, SliceExt::Available, "{line}");
            let mut g = f;
            // The configured outcome folds in as `on` only when the
            // module is on in this process.
            g.note_slice_ext();
            assert_eq!(g.slice_ext == SliceExt::On, crate::slice::on(), "{line}");
        }
        eprintln!("slice_ext: {} (probe {:?})", f.slice_ext.name(), p);
    }

    /// The THP table against the same files read through std.
    #[test]
    fn probe_thp_table_matches_sysfs() {
        let (kv, f, line) = probed();
        let dir = std::path::Path::new("/sys/kernel/mm/transparent_hugepage");
        match std::fs::read_to_string(dir.join("enabled")) {
            Ok(e) => {
                assert_eq!(f.thp.enabled, crate::sys::thp_host::parse_thp_setting(e.as_bytes()), "{line}");
                let mut listed = 0u64;
                for entry in std::fs::read_dir(dir).unwrap() {
                    let name = entry.unwrap().file_name();
                    let name = name.to_str().unwrap();
                    if let Some(kb) = name.strip_prefix("hugepages-").and_then(|s| s.strip_suffix("kB")) {
                        if dir.join(name).join("enabled").exists() {
                            listed |= 1 << (kb.parse::<u64>().unwrap() * 1024).trailing_zeros();
                        }
                    }
                }
                if listed == 0 {
                    listed = 1 << f.thp.lg_pmd;
                } else {
                    assert!(at_least(kv, (6, 8)), "per-size directories predate 6.8: {line}");
                }
                assert_eq!(f.thp.listed, listed, "{line}");
            }
            Err(_) => assert_eq!(f.thp, Thp::NONE, "{line}"),
        }
        assert!(f.thp.lg_pmd as usize > f.page_shift as usize, "{line}");
    }

    #[test]
    fn release_closes_pidfd() {
        let mut f = Features::probe();
        if let ProcessMadvise::Pidfd(fd) = f.process_madvise {
            f.release();
            assert_eq!(sys::close(fd), Err(sys::Errno::EBADF));
            assert!(!f.process_madvise_free);
        } else {
            f.release();
        }
        assert!(matches!(
            f.process_madvise,
            ProcessMadvise::Unavailable | ProcessMadvise::PidfdSelf
        ));
    }
}
