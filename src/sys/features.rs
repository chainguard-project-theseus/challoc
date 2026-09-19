//! Kernel feature probing (design.md section 10, step 1) and the feature
//! line that reports it together with the CPU report of `sys::cpu`.
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

use super::cpu::Cpu;

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

/// One transparent-huge-page sysfs setting, the bracketed word of
/// `/sys/kernel/mm/transparent_hugepage/enabled` (`always [madvise] never`)
/// or of a per-size `hugepages-<n>kB/enabled` (`always inherit madvise
/// [never]`, kernel 6.8+).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThpSetting {
    Always,
    /// Per-size files only: follow the top-level `enabled`.
    Inherit,
    Madvise,
    Never,
}

impl ThpSetting {
    pub const fn name(self) -> &'static str {
        match self {
            ThpSetting::Always => "always",
            ThpSetting::Inherit => "inherit",
            ThpSetting::Madvise => "madvise",
            ThpSetting::Never => "never",
        }
    }
}

/// The bracketed word of a THP `enabled` file. `None` for anything that is
/// not one of the four words in brackets (an empty or foreign file).
pub fn parse_thp_setting(bytes: &[u8]) -> Option<ThpSetting> {
    let open = bytes.iter().position(|&b| b == b'[')?;
    let rest = &bytes[open + 1..];
    let close = rest.iter().position(|&b| b == b']')?;
    match &rest[..close] {
        b"always" => Some(ThpSetting::Always),
        b"inherit" => Some(ThpSetting::Inherit),
        b"madvise" => Some(ThpSetting::Madvise),
        b"never" => Some(ThpSetting::Never),
        _ => None,
    }
}

/// What `madvise(MADV_COLLAPSE)` did when probed (design 10 step 1, plan
/// Phase 2.5 task 9). The syscall acts on whole PMD-sized ranges only
/// (through 7.0 a request over a smaller range returns 0 having collapsed
/// nothing), so the probe populates and collapses one PMD-sized range and
/// costs a PMD of memory for a moment; init runs it only when the `thp`
/// key asks for huge pages at all.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Collapse {
    /// Not attempted (`thp:never`, the default).
    Unprobed,
    /// Attempted and refused (`EINVAL`, `ENOSYS`, `EPERM`, `ENOMEM`, or
    /// `EAGAIN`) or there is no THP at all.
    No,
    /// Collapsed a populated range of `1 << lg` bytes.
    Yes(u8),
}

/// The kernel's transparent-huge-page configuration as read at init from
/// `/sys/kernel/mm/transparent_hugepage` with raw syscalls (design 10 step
/// 1, plan Phase 2.5 task 9). Folio sizes are bit sets over `log2(size)`:
/// bit `k` set means the `1 << k`-byte size. Every candidate size from four
/// pages up to the PMD is looked up by path, so a size the kernel does not
/// list (a directory without an `enabled` file, as `hugepages-8kB` on 7.0,
/// counts as unlisted) is simply absent, and a kernel before 6.8 with no
/// per-size directories lists the PMD size alone under the top-level
/// setting.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Thp {
    /// The top-level `enabled`; `None` when the file is missing (a kernel
    /// without `CONFIG_TRANSPARENT_HUGEPAGE`).
    pub enabled: Option<ThpSetting>,
    /// `log2` of the PMD size (`hpage_pmd_size`, else `page * page / 8`).
    pub lg_pmd: u8,
    /// Sizes the kernel lists (has an `enabled` file for).
    pub listed: u64,
    /// Sizes whose effective setting is `always` (`inherit` resolved
    /// through the top-level setting).
    pub always: u64,
    /// Sizes whose effective setting is `madvise`.
    pub madvise: u64,
    /// The `MADV_COLLAPSE` probe.
    pub collapse: Collapse,
}

impl Thp {
    /// No THP at all: nothing listed, nothing enabled.
    pub const NONE: Thp = Thp { enabled: None, lg_pmd: 21, listed: 0, always: 0, madvise: 0, collapse: Collapse::No };

    /// Compute the table from the files' contents: `read(path, buf)` fills
    /// `buf` with the file at the NUL-terminated `path` and returns the
    /// byte count, 0 when it is missing. Pure over the reader, so the host
    /// tests feed it fixtures.
    pub fn from_reader(page: usize, mut read: impl FnMut(&[u8], &mut [u8]) -> usize) -> Thp {
        const DIR: &[u8] = b"/sys/kernel/mm/transparent_hugepage/";
        let mut buf = [0u8; 128];
        let n = read(b"/sys/kernel/mm/transparent_hugepage/enabled\0", &mut buf);
        let enabled = if n == 0 { None } else { parse_thp_setting(&buf[..n]) };
        let lg_page = page.trailing_zeros() as u8;
        let mut lg_pmd = (2 * lg_page).saturating_sub(3);
        let n = read(b"/sys/kernel/mm/transparent_hugepage/hpage_pmd_size\0", &mut buf);
        if let Some(v) = parse_decimal(&buf[..n]) {
            if v.is_power_of_two() && v.trailing_zeros() as u8 > lg_page {
                lg_pmd = v.trailing_zeros() as u8;
            }
        }
        let mut t = Thp { enabled, lg_pmd, listed: 0, always: 0, madvise: 0, collapse: Collapse::Unprobed };
        let Some(top) = enabled else { return Thp::NONE };
        // Every candidate size from two pages to the PMD: hugepages-<n>kB/enabled.
        let mut path = [0u8; 96];
        for lg in (lg_page + 1)..=lg_pmd {
            let mut len = 0;
            for &b in DIR.iter().chain(b"hugepages-") {
                path[len] = b;
                len += 1;
            }
            len += write_decimal(&mut path[len..], (1u64 << lg) >> 10);
            for &b in b"kB/enabled\0" {
                path[len] = b;
                len += 1;
            }
            let n = read(&path[..len], &mut buf);
            if n == 0 {
                continue;
            }
            let Some(setting) = parse_thp_setting(&buf[..n]) else { continue };
            t.listed |= 1 << lg;
            match setting {
                ThpSetting::Always => t.always |= 1 << lg,
                ThpSetting::Madvise => t.madvise |= 1 << lg,
                ThpSetting::Inherit => match top {
                    ThpSetting::Always => t.always |= 1 << lg,
                    ThpSetting::Madvise => t.madvise |= 1 << lg,
                    _ => {}
                },
                ThpSetting::Never => {}
            }
        }
        if t.listed == 0 {
            // Before 6.8: the PMD size alone, under the top-level setting.
            t.listed = 1 << lg_pmd;
            match top {
                ThpSetting::Always => t.always = 1 << lg_pmd,
                ThpSetting::Madvise => t.madvise = 1 << lg_pmd,
                _ => {}
            }
        }
        t
    }

    /// Sizes a `MADV_HUGEPAGE`d range can be backed with: effective
    /// `always` or `madvise`.
    #[inline]
    pub fn enabled_sizes(&self) -> u64 {
        self.always | self.madvise
    }

    /// The enabled sizes below the PMD (the multi-size THP path).
    #[inline]
    pub fn sub_pmd_sizes(&self) -> u64 {
        self.enabled_sizes() & ((1u64 << self.lg_pmd) - 1)
    }

    /// The effective setting of the PMD size: what `MADV_HUGEPAGE` would
    /// change for 2 MiB pages. `Never` when nothing is listed.
    pub fn pmd_setting(&self) -> ThpSetting {
        let bit = 1u64 << self.lg_pmd;
        if self.always & bit != 0 {
            ThpSetting::Always
        } else if self.madvise & bit != 0 {
            ThpSetting::Madvise
        } else {
            ThpSetting::Never
        }
    }

    /// `log2` of the smallest size in `set`, if any.
    pub fn smallest(set: u64) -> Option<u8> {
        (set != 0).then(|| set.trailing_zeros() as u8)
    }
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

impl fmt::Display for Thp {
    /// `thp=<setting|none> mthp=<sizes|none> collapse=<unprobed|no|size>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "thp={}", self.enabled.map_or("none", ThpSetting::name))?;
        write!(f, " mthp={}", SizeSet(self.enabled_sizes()))?;
        match self.collapse {
            Collapse::Unprobed => f.write_str(" collapse=unprobed"),
            Collapse::No => f.write_str(" collapse=no"),
            Collapse::Yes(lg) => write!(f, " collapse={}", LgSize(lg)),
        }
    }
}

/// Leading decimal digits of `bytes` (a sysfs number with its newline).
fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    let mut v: u64 = 0;
    let mut any = false;
    for &b in bytes {
        if !b.is_ascii_digit() {
            break;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
        any = true;
    }
    any.then_some(v)
}

/// `v` in decimal into `out`; returns the digit count.
fn write_decimal(out: &mut [u8], mut v: u64) -> usize {
    let mut tmp = [0u8; 20];
    let mut n = 0;
    loop {
        tmp[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        out[i] = tmp[n - 1 - i];
    }
    n
}

/// glibc's registered rseq area as the running process sees it (design 15
/// and 17, plan Phase 2.5 task 8): the three variables glibc 2.35+ exports
/// beside its registration, read from the symbols at init (weak references,
/// so a libc without them reads as "no rseq"), and the kernel-written
/// `cpu_id` field of the area itself, which says whether the registration
/// took. Nothing here is a header constant: `__rseq_offset` differs between
/// architectures and moves with the static TLS layout (192 from
/// `tpidr_el0` on aarch64, -192 or -256 from `%fs:0` on x86_64 depending on
/// what else is preloaded), and `__rseq_size` is whatever the kernel's
/// `AT_RSEQ_FEATURE_SIZE` let glibc register (33 on the 7.0 GCE kernels,
/// 20 on a 6.10 linuxkit kernel that reports no feature size).
///
/// The struct is `Copy` plain data; the report prints `rseq=yes|no
/// rseq_size=<n> mm_cid=yes|no`. The per-CPU mode (design 15) reads the
/// `mm_cid` value itself through its own accessor, gated on [`mm_cid`]
/// here; this type only answers whether the field exists.
///
/// [`mm_cid`]: Rseq::mm_cid
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rseq {
    /// `__rseq_size`: the bytes of `struct rseq` the kernel registered
    /// (glibc's `rseq_area` feature size); `0` when glibc did not register
    /// (`GLIBC_TUNABLES=glibc.pthread.rseq=0`, a kernel without
    /// `CONFIG_RSEQ`, or the symbol absent).
    pub size: u32,
    /// `__rseq_flags`: the flags of the registration (`0` today).
    pub flags: u32,
    /// `__rseq_offset`: the area's offset from the thread pointer, as read.
    pub offset: isize,
    /// The area's `cpu_id` field (offset 4), read once after the symbols:
    /// `>= 0` once the kernel has written the area, `-1`
    /// (`RSEQ_CPU_ID_UNINITIALIZED`) or `-2` (`RSEQ_CPU_ID_REGISTRATION_FAILED`)
    /// otherwise. `-1` when the area was not read (`size == 0`).
    pub cpu_id: i32,
}

impl Rseq {
    /// Offset of the `mm_cid` field in `struct rseq` (`cpu_id_start` 0,
    /// `cpu_id` 4, `rseq_cs` 8, `flags` 16, `node_id` 20, `mm_cid` 24).
    pub const MM_CID_OFFSET: u32 = 24;
    /// The registered size that covers `mm_cid` (kernel 6.3+ registrations).
    pub const MM_CID_END: u32 = Self::MM_CID_OFFSET + 4;
    /// Offset of the `cpu_id` field.
    pub const CPU_ID_OFFSET: u32 = 4;
    /// The size of the original `struct rseq` (through `flags`), the least
    /// a successful registration reports.
    pub const BASE_SIZE: u32 = 20;

    /// No rseq at all: the symbols are absent or glibc registered nothing.
    pub const NONE: Rseq = Rseq { size: 0, flags: 0, offset: 0, cpu_id: -1 };

    /// The verdict from the symbol values and the area's `cpu_id`. Pure
    /// logic, host tested.
    pub const fn from_symbols(size: u32, flags: u32, offset: isize, cpu_id: i32) -> Rseq {
        Rseq { size, flags, offset, cpu_id }
    }

    /// glibc registered an rseq area and the kernel has written it: the
    /// size covers the original struct and `cpu_id` is a CPU number.
    pub const fn registered(&self) -> bool {
        self.size >= Self::BASE_SIZE && self.cpu_id >= 0
    }

    /// The registered area covers the `mm_cid` field (`__rseq_size >= 28`):
    /// the per-CPU mode of design 15 may read it. `false` without a
    /// registration.
    pub const fn mm_cid(&self) -> bool {
        self.registered() && self.size >= Self::MM_CID_END
    }
}

impl fmt::Display for Rseq {
    /// `rseq=yes rseq_size=33 mm_cid=yes`; `rseq_size` is the symbol's
    /// value even when the registration did not take.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn yn(b: bool) -> &'static str {
            if b { "yes" } else { "no" }
        }
        write!(f, "rseq={} rseq_size={} mm_cid={}", yn(self.registered()), self.size, yn(self.mm_cid()))
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
    /// The transparent-huge-page configuration and the `MADV_COLLAPSE`
    /// probe (plan Phase 2.5 task 9): what the `thp` key can turn on.
    pub thp: Thp,
    /// glibc's rseq registration and whether it covers `mm_cid` (plan
    /// Phase 2.5 task 8): the per-CPU mode's `auto` decision reads
    /// [`Rseq::mm_cid`]; the value itself is read elsewhere.
    pub rseq: Rseq,
    /// The CPU report (design.md section 17): reported, not acted on.
    pub cpu: Cpu,
}

impl fmt::Display for Features {
    /// One line, `key=value` separated by spaces, for `challoc_features()`,
    /// `malloc_stats_print` and abort messages: the kernel probes, then the
    /// CPU report. Values are `yes`/`no` except `page_size`,
    /// `process_madvise` (`no`, `pidfd_self`, or `pidfd:<fd>`) and the
    /// cache sizes (`64K`, `2M`, `unknown`), the THP keys (`thp=never|
    /// madvise|always|none`, `mthp=64K,2M|none`, `collapse=unprobed|no|2M`)
    /// and `rseq_size` (a byte count, `0` without a registration).
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
        write!(
            f,
            " process_madvise_free={} anon_name={} {} {} {}",
            yn(self.process_madvise_free),
            yn(self.anon_name),
            self.thp,
            self.rseq,
            self.cpu
        )
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
    /// probe `yes`, `pidfd:2147483647`, byte-exact cache sizes, every THP
    /// size listed) is under 400 bytes (`feature_line_fits`).
    pub const CAP: usize = 512;

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

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::{AT_PAGESZ, Collapse, Cpu, Features, ProcessMadvise, Rseq, Thp, page_size_from_auxv};
    use crate::sys::{
        self, IoVec, MADV_COLLAPSE, MADV_DONTNEED, MADV_FREE, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE,
        MAP_ANONYMOUS, MAP_PRIVATE, PIDFD_SELF, PROT_NONE, PROT_READ, PROT_WRITE,
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

    /// `PIDFD_SELF` first (kernel 6.14, no fd); then a real pidfd for the
    /// calling process (kernel 5.10, needs the self-target exemption or
    /// `CAP_SYS_NICE`); otherwise unavailable. The second value is whether
    /// `MADV_FREE` goes through the same target (plan Phase 2.5 task 10).
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


    /// Read one sysfs file into `buf`; 0 when it is missing or unreadable.
    fn read_sysfs(path: &[u8], buf: &mut [u8]) -> usize {
        let Ok(fd) = sys::open(path, sys::O_RDONLY) else { return 0 };
        let n = sys::read_full(fd, buf).unwrap_or(0);
        let _ = sys::close(fd);
        n
    }

    /// Address of a weak data symbol through the GOT, or null when the
    /// process does not define it (the `je_malloc_conf` recipe of
    /// `config.rs`). Each of the three rseq variables has its own copy
    /// because the symbol name must be spelled in the instruction.
    macro_rules! weak_addr {
        ($name:ident, $sym:literal) => {
            #[inline(never)]
            fn $name() -> *const u8 {
                let out: *const u8;
                // SAFETY: a GOT load of a (possibly absent, then null) data
                // symbol; nothing is written.
                unsafe {
                    #[cfg(target_arch = "x86_64")]
                    core::arch::asm!(
                        concat!(".weak ", $sym),
                        concat!("mov {out}, qword ptr [rip + ", $sym, "@GOTPCREL]"),
                        out = out(reg) out,
                        options(nostack, readonly, preserves_flags),
                    );
                    #[cfg(target_arch = "aarch64")]
                    core::arch::asm!(
                        concat!(".weak ", $sym),
                        concat!("adrp {out}, :got:", $sym),
                        concat!("ldr {out}, [{out}, :got_lo12:", $sym, "]"),
                        out = out(reg) out,
                        options(nostack, readonly, preserves_flags),
                    );
                }
                out
            }
        };
    }
    weak_addr!(rseq_size_addr, "__rseq_size");
    weak_addr!(rseq_offset_addr, "__rseq_offset");
    weak_addr!(rseq_flags_addr, "__rseq_flags");

    /// The calling thread's thread pointer: what `__rseq_offset` is
    /// relative to (`%fs:0` holds the TCB's own address on x86_64;
    /// `tpidr_el0` on aarch64).
    #[inline(always)]
    fn thread_pointer() -> usize {
        let tp: usize;
        // SAFETY: reads a register (and, on x86_64, the TCB's self word).
        unsafe {
            #[cfg(target_arch = "x86_64")]
            core::arch::asm!("mov {tp}, qword ptr fs:0", tp = out(reg) tp, options(nostack, readonly, preserves_flags));
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
        }
        tp
    }

    /// glibc's rseq registration (plan Phase 2.5 task 8): `__rseq_size`,
    /// `__rseq_flags` and `__rseq_offset` from the weak symbols (all three
    /// or nothing: glibc exports them together since 2.35), then the
    /// area's `cpu_id` at `tp + __rseq_offset + 4`, read only when the
    /// size says the field exists. Read-only: the area is kernel-owned and
    /// glibc's, nothing here writes it. Never allocates, no syscall.
    pub fn probe_rseq() -> Rseq {
        let (ps, po, pf) = (rseq_size_addr(), rseq_offset_addr(), rseq_flags_addr());
        if ps.is_null() || po.is_null() || pf.is_null() {
            return Rseq::NONE;
        }
        // SAFETY: the symbols resolved to glibc's `unsigned int
        // __rseq_size`, `ptrdiff_t __rseq_offset` and `unsigned int
        // __rseq_flags` objects.
        let (size, offset, flags) = unsafe { (ps.cast::<u32>().read(), po.cast::<isize>().read(), pf.cast::<u32>().read()) };
        let cpu_id = if size >= Rseq::BASE_SIZE {
            let area = (thread_pointer() as isize).wrapping_add(offset) as usize;
            // SAFETY: glibc registered `size` bytes at `tp + offset` for
            // this thread (`__rseq_size != 0`); `cpu_id` is inside them.
            unsafe { ((area + Rseq::CPU_ID_OFFSET as usize) as *const i32).read_volatile() }
        } else {
            -1
        };
        Rseq::from_symbols(size, flags, offset, cpu_id)
    }

    /// The THP table from `/sys/kernel/mm/transparent_hugepage` (about ten
    /// sysfs reads on a 4 KiB kernel). The `MADV_COLLAPSE` probe is not run
    /// here: see [`Features::probe_collapse`].
    pub fn probe_thp(page: usize) -> Thp {
        Thp::from_reader(page, read_sysfs)
    }

    /// `madvise(MADV_COLLAPSE)` over a populated, aligned, PMD-sized range
    /// of a fresh private mapping. The syscall acts on whole PMD ranges
    /// only (a smaller range returns 0 having done nothing), so the PMD is
    /// the one size the probe can test; it populates `1 << lg_pmd` bytes
    /// and releases them, which is why init runs it only when `thp` is not
    /// `never`. `Yes` needs the call to return 0; the kernel answers
    /// `EINVAL` before 6.1, `ENOMEM`/`EAGAIN` when it cannot assemble the
    /// page, `EPERM`/`ENOSYS` under a filter.
    pub fn probe_collapse(lg_pmd: u8, page: usize) -> Collapse {
        let size = 1usize << lg_pmd;
        // SAFETY: fresh anonymous private mapping, no fixed address.
        let Ok(m) = (unsafe { sys::mmap(0, 2 * size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) })
        else {
            return Collapse::No;
        };
        let a = (m + size - 1) & !(size - 1);
        let mut off = 0;
        while off < size {
            // SAFETY: inside the writable mapping just made.
            unsafe { ((a + off) as *mut u8).write_volatile(0xA5) };
            off += page;
        }
        // SAFETY: our scratch range; collapse changes backing, not contents.
        let r = unsafe { sys::madvise(a, size, MADV_COLLAPSE) };
        release(m, 2 * size);
        if r.is_ok() { Collapse::Yes(lg_pmd) } else { Collapse::No }
    }

    impl Features {
        /// Probe everything. Called once from init (design.md section 10);
        /// calling it again is harmless but leaks a sealed page and may open
        /// another pidfd, so init stores the result.
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
                thp: probe_thp(page_size),
                rseq: probe_rseq(),
                cpu: Cpu::probe(page_size),
            }
        }

        /// Run the `MADV_COLLAPSE` probe (plan Phase 2.5 task 9). Separate
        /// from [`probe`](Features::probe) because it populates a PMD of
        /// memory: init calls it only when the configuration's `thp` key is
        /// not `never`, and the line says `collapse=unprobed` otherwise.
        /// A kernel without THP (`thp=none`) is `No` without a call.
        pub fn probe_collapse(&mut self) {
            self.thp.collapse = if self.thp.enabled.is_none() {
                Collapse::No
            } else {
                probe_collapse(self.thp.lg_pmd, self.page_size)
            };
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


    /// A fake sysfs: `(path, contents)` pairs; a missing path reads 0 bytes.
    fn sysfs<'a>(files: &'a [(&'a str, &'a str)]) -> impl FnMut(&[u8], &mut [u8]) -> usize + 'a {
        move |path: &[u8], buf: &mut [u8]| {
            let p = core::str::from_utf8(&path[..path.len() - 1]).unwrap();
            assert!(path.last() == Some(&0), "path not NUL-terminated");
            let p = p.strip_prefix("/sys/kernel/mm/transparent_hugepage/").unwrap_or_else(|| panic!("{p}"));
            match files.iter().find(|(f, _)| *f == p) {
                Some((_, c)) => {
                    buf[..c.len()].copy_from_slice(c.as_bytes());
                    c.len()
                }
                None => 0,
            }
        }
    }

    #[test]
    fn thp_setting_parses_the_bracketed_word() {
        assert_eq!(parse_thp_setting(b"always [madvise] never\n"), Some(ThpSetting::Madvise));
        assert_eq!(parse_thp_setting(b"[always] madvise never\n"), Some(ThpSetting::Always));
        assert_eq!(parse_thp_setting(b"always madvise [never]\n"), Some(ThpSetting::Never));
        assert_eq!(parse_thp_setting(b"always [inherit] madvise never\n"), Some(ThpSetting::Inherit));
        assert_eq!(parse_thp_setting(b"always madvise never\n"), None);
        assert_eq!(parse_thp_setting(b"[defer]\n"), None);
        assert_eq!(parse_thp_setting(b""), None);
        assert_eq!(parse_decimal(b"2097152\n"), Some(2097152));
        assert_eq!(parse_decimal(b"\n"), None);
        let mut b = [0u8; 8];
        assert_eq!(write_decimal(&mut b, 2048), 4);
        assert_eq!(&b[..4], b"2048");
        assert_eq!(write_decimal(&mut b, 0), 1);
        assert_eq!(&b[..1], b"0");
    }

    /// The reference VMs' layout (7.0, 4 KiB pages): `hugepages-8kB` exists
    /// without an `enabled` file, 16K..2M have one, 2M inherits.
    const GCE_70: &[(&str, &str)] = &[
        ("enabled", "always [madvise] never\n"),
        ("hpage_pmd_size", "2097152\n"),
        ("hugepages-16kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-32kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-64kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-128kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-256kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-512kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-1024kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-2048kB/enabled", "always [inherit] madvise never\n"),
    ];

    #[test]
    fn thp_table_gce_default_lists_eight_sizes_and_enables_the_pmd_alone() {
        let t = Thp::from_reader(4096, sysfs(GCE_70));
        assert_eq!(t.enabled, Some(ThpSetting::Madvise));
        assert_eq!(t.lg_pmd, 21);
        assert_eq!(t.listed, ((1u64 << 22) - 1) & !((1u64 << 14) - 1), "16K..2M");
        assert_eq!(t.always, 0);
        assert_eq!(t.madvise, 1 << 21, "2M inherits madvise");
        assert_eq!(t.pmd_setting(), ThpSetting::Madvise);
        assert_eq!(t.sub_pmd_sizes(), 0);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=2M collapse=unprobed");
    }

    #[test]
    fn thp_table_with_64k_madvise_and_pmd_never() {
        let mut files = GCE_70.to_vec();
        files.retain(|(f, _)| !f.starts_with("hugepages-64kB") && !f.starts_with("hugepages-2048kB"));
        files.push(("hugepages-64kB/enabled", "always inherit [madvise] never\n"));
        files.push(("hugepages-2048kB/enabled", "always inherit madvise [never]\n"));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(t.always, 0);
        assert_eq!(t.pmd_setting(), ThpSetting::Never);
        assert_eq!(t.sub_pmd_sizes(), 1 << 16);
        assert_eq!(Thp::smallest(t.sub_pmd_sizes()), Some(16));
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=64K collapse=unprobed");
        // A per-size `madvise` counts even when the top level is `never`.
        files.retain(|(f, _)| *f != "enabled");
        files.push(("enabled", "always madvise [never]\n"));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.enabled, Some(ThpSetting::Never));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(std::format!("{t}"), "thp=never mthp=64K collapse=unprobed");
        // `always` at the top level makes every `inherit` size always.
        files.retain(|(f, _)| *f != "enabled");
        files.push(("enabled", "[always] madvise never\n"));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(t.always, 0, "every other size is never, not inherit");
        assert_eq!(std::format!("{t}"), "thp=always mthp=64K collapse=unprobed");
    }

    #[test]
    fn thp_table_before_6_8_has_the_pmd_alone() {
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "always [madvise] never\n"), ("hpage_pmd_size", "2097152\n")]));
        assert_eq!(t.listed, 1 << 21);
        assert_eq!(t.madvise, 1 << 21);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=2M collapse=unprobed");
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "[always] madvise never\n")]));
        assert_eq!(t.lg_pmd, 21, "computed from the page size when hpage_pmd_size is missing");
        assert_eq!(t.always, 1 << 21);
        assert_eq!(std::format!("{t}"), "thp=always mthp=2M collapse=unprobed");
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "always madvise [never]\n")]));
        assert_eq!(t.enabled_sizes(), 0);
        assert_eq!(std::format!("{t}"), "thp=never mthp=none collapse=unprobed");
        // 64 KiB pages: the PMD is 512 MiB.
        let t = Thp::from_reader(65536, sysfs(&[("enabled", "always [madvise] never\n")]));
        assert_eq!(t.lg_pmd, 29);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=512M collapse=unprobed");
    }

    #[test]
    fn thp_table_missing_directory_is_none() {
        let t = Thp::from_reader(4096, sysfs(&[]));
        assert_eq!(t, Thp::NONE);
        assert_eq!(t.enabled, None);
        assert_eq!(t.enabled_sizes(), 0);
        assert_eq!(t.pmd_setting(), ThpSetting::Never);
        assert_eq!(std::format!("{t}"), "thp=none mthp=none collapse=no");
        // A garbled `enabled` file is treated as no THP as well.
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "garbage\n")]));
        assert_eq!(t.enabled, None);
        assert_eq!(t, Thp::NONE);
    }

    use crate::sys::cpu::{Arm, CacheSource, Caches, Cpu, Isa, X86};

    fn axion_cpu() -> Cpu {
        Cpu {
            isa: Isa::Arm(Arm { mte: false, mte3: false, pac: true, lse: true, lse2: true, bti: true, sve: true }),
            caches: Caches { line: 64, l1d: 64 * 1024, l2: 2 * 1024 * 1024 },
            cache_source: CacheSource::Sysfs,
        }
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
            thp: Thp { enabled: Some(ThpSetting::Madvise), lg_pmd: 21, listed: (1 << 16) | (1 << 21), always: 0, madvise: (1 << 16) | (1 << 21), collapse: Collapse::Unprobed },
            rseq: Rseq::from_symbols(33, 0, 192, 5),
            cpu: axion_cpu(),
        };
        let s = std::format!("{f}");
        assert_eq!(
            s,
            "page_size=4096 guard_install=no mseal=yes process_madvise=pidfd:7 process_madvise_free=yes anon_name=no \
             thp=madvise mthp=64K,2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes \
             mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M"
        );
        // The Mac's Docker VM (linuxkit 6.10): registered, no `mm_cid`.
        let f = Features { rseq: Rseq::from_symbols(20, 0, 192, 0), ..f };
        assert!(std::format!("{f}").contains(" collapse=unprobed rseq=yes rseq_size=20 mm_cid=no mte="));
        let f = Features { rseq: Rseq::NONE, ..f };
        assert!(std::format!("{f}").contains(" rseq=no rseq_size=0 mm_cid=no "));
        let f = Features { thp: Thp { collapse: Collapse::Yes(21), ..f.thp }, ..f };
        assert!(std::format!("{f}").contains(" thp=madvise mthp=64K,2M collapse=2M "));
        let f = Features { thp: Thp::NONE, ..f };
        assert!(std::format!("{f}").contains(" thp=none mthp=none collapse=no "));
        let f = Features { thp: Thp { enabled: Some(ThpSetting::Never), listed: 1 << 21, always: 0, madvise: 0, collapse: Collapse::No, lg_pmd: 21 }, ..f };
        assert!(std::format!("{f}").contains(" thp=never mthp=none collapse=no "));
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
            rseq: Rseq::NONE,
            cpu: axion_cpu(),
        };
        let line = FeatureLine::build(&f);
        assert_eq!(line.as_str(), std::format!("{f}"));
        let bytes = unsafe { core::slice::from_raw_parts(line.as_ptr() as *const u8, line.len + 1) };
        assert_eq!(bytes[line.len], 0);
        assert_eq!(&bytes[..line.len], line.as_str().as_bytes());
        assert!(!line.as_str().contains('\n'));
    }

    #[test]
    fn feature_line_fits() {
        // The longest line either architecture can produce.
        let x86 = Cpu {
            isa: Isa::X86(X86 {
                lam: true,
                uai: true,
                la57: true,
                la57_enabled: true,
                cldemote: true,
                waitpkg: true,
                erms: true,
                fsrm: true,
                avx512f: true,
            }),
            caches: Caches { line: 4_000_000_001, l1d: 4_000_000_001, l2: 4_000_000_001 },
            cache_source: CacheSource::Cpuid,
        };
        for cpu in [x86, axion_cpu()] {
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
                    collapse: Collapse::Yes(29),
                },
                rseq: Rseq::from_symbols(u32::MAX, u32::MAX, isize::MIN, i32::MAX),
                cpu,
            };
            let s = std::format!("{f}");
            assert!(s.len() < 400, "{} bytes: {s}", s.len());
            assert_eq!(FeatureLine::build(&f).as_str(), s);
        }
    }

    /// The verdicts on the values the prerequisites note recorded (plan
    /// Phase 2.5 task 8): the GCE VMs (glibc 2.44 on 7.0, `__rseq_size=33`,
    /// offset 192 on aarch64 and -192/-256 on x86_64), the Mac's Docker VM
    /// (6.10, `__rseq_size=20`), a libc without the symbols, a registration
    /// the kernel refused (`cpu_id` -2) or never wrote (-1), and the sizes
    /// around the `mm_cid` boundary.
    #[test]
    fn rseq_verdicts_follow_size_and_cpu_id() {
        for off in [192isize, -192, -256] {
            let r = Rseq::from_symbols(33, 0, off, 3);
            assert!(r.registered() && r.mm_cid(), "{r:?}");
            assert_eq!(std::format!("{r}"), "rseq=yes rseq_size=33 mm_cid=yes");
        }
        let r = Rseq::from_symbols(20, 0, 192, 0);
        assert!(r.registered() && !r.mm_cid(), "{r:?}");
        assert_eq!(std::format!("{r}"), "rseq=yes rseq_size=20 mm_cid=no");
        assert_eq!(std::format!("{}", Rseq::NONE), "rseq=no rseq_size=0 mm_cid=no");
        assert!(!Rseq::NONE.registered() && !Rseq::NONE.mm_cid());
        // The kernel did not write the area: no registration, whatever the size says.
        for cpu_id in [-1, -2] {
            let r = Rseq::from_symbols(33, 0, -192, cpu_id);
            assert!(!r.registered() && !r.mm_cid(), "{r:?}");
            assert_eq!(std::format!("{r}"), "rseq=no rseq_size=33 mm_cid=no");
        }
        // The boundary: 27 bytes do not hold a 4-byte `mm_cid` at 24; 28 do; 32 (a
        // glibc that rounds to the alignment) does.
        assert!(!Rseq::from_symbols(27, 0, 192, 0).mm_cid());
        assert!(Rseq::from_symbols(28, 0, 192, 0).mm_cid());
        assert!(Rseq::from_symbols(32, 0, 192, 0).mm_cid());
        // Below the original struct nothing is registered.
        assert!(!Rseq::from_symbols(19, 0, 192, 0).registered());
        assert_eq!(Rseq::MM_CID_END, 28);
        assert_eq!(Rseq::CPU_ID_OFFSET, 4);
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
        // The rseq probe (plan Phase 2.5 task 8) against glibc's own
        // symbols through `dlsym` and the kernel's `AT_RSEQ_FEATURE_SIZE`:
        // the values must be the symbols', and `mm_cid` is `yes` exactly
        // when glibc registered an area whose size reaches 28, which the
        // kernel allows from 6.3 (the GCE 7.0 VMs: 33; the Mac's 6.10
        // linuxkit VM reports no feature size and glibc registers 20).
        // This is the both-ways test: it runs on hosts of each kind.
        {
            let sym = |name: &[u8]| -> *const u8 {
                // SAFETY: `dlsym` on a NUL-terminated name in the test.
                unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }.cast()
            };
            let (ps, po, pf) = (sym(b"__rseq_size\0"), sym(b"__rseq_offset\0"), sym(b"__rseq_flags\0"));
            if ps.is_null() {
                assert_eq!(f.rseq, Rseq::NONE, "no __rseq_size symbol: {line}");
            } else {
                // SAFETY: glibc's exported objects of these types.
                let (size, offset, flags) = unsafe { (ps.cast::<u32>().read(), po.cast::<isize>().read(), pf.cast::<u32>().read()) };
                assert_eq!((f.rseq.size, f.rseq.offset, f.rseq.flags), (size, offset, flags), "{line}");
                eprintln!("rseq: __rseq_size={size} __rseq_offset={offset} __rseq_flags={flags} cpu_id={}", f.rseq.cpu_id);
                if size >= Rseq::BASE_SIZE {
                    // glibc registered: the kernel wrote a CPU number.
                    assert!(f.rseq.cpu_id >= 0, "{line}");
                    assert!(f.rseq.registered(), "{line}");
                    // SAFETY: no preconditions; the count bounds `cpu_id`.
                    let ncpu = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
                    assert!((f.rseq.cpu_id as i64) < ncpu, "cpu_id {} of {ncpu}", f.rseq.cpu_id);
                } else {
                    assert!(!f.rseq.registered(), "{line}");
                }
                // AT_RSEQ_FEATURE_SIZE (27): the fields the kernel offers.
                // SAFETY: getauxval has no preconditions.
                let feat = unsafe { libc::getauxval(27) } as u32;
                if feat >= Rseq::MM_CID_END && size >= Rseq::BASE_SIZE {
                    assert!(at_least(kv, (6, 3)), "mm_cid predates 6.3: {line}");
                    assert!(f.rseq.mm_cid(), "kernel offers {feat} bytes, glibc registered {size}: {line}");
                    assert!(line.contains(" rseq=yes rseq_size=") && line.contains(" mm_cid=yes "), "{line}");
                } else {
                    assert!(!f.rseq.mm_cid(), "feature size {feat}, registered {size}: {line}");
                    assert!(line.contains(" mm_cid=no "), "{line}");
                }
            }
        }
        // The THP table against the same files read through std.
        let dir = std::path::Path::new("/sys/kernel/mm/transparent_hugepage");
        match std::fs::read_to_string(dir.join("enabled")) {
            Ok(e) => {
                assert_eq!(f.thp.enabled, parse_thp_setting(e.as_bytes()), "{line}");
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
                assert_eq!(f.thp.collapse, Collapse::Unprobed, "probe() does not run the collapse probe: {line}");
            }
            Err(_) => assert_eq!(f.thp, Thp::NONE, "{line}"),
        }
        assert!(f.thp.lg_pmd as usize > f.page_shift as usize, "{line}");

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
                    // The `MADV_FREE` probe (plan Phase 2.5 task 10) agrees
                    // with a second call through the same target.
                    p.write_volatile(9);
                    let r = sys::process_madvise(fd, &iov, 1, sys::MADV_FREE, 0);
                    assert_eq!(r == Ok(page), f.process_madvise_free, "{line}: {r:?}");
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
