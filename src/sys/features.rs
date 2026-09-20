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
//!
//! The line is a table: [`Features::words`] yields every `(key, value)`
//! pair in order, and `Display`, [`FeatureLine`] and the tests all iterate
//! it, so a new kernel feature is one probe, one field and one row.

use core::fmt;

use super::auxv::{AT_NULL, AT_PAGESZ};
use super::cpu::Cpu;
use super::{Trailing, parse_decimal, yn};

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

/// What `madvise(MADV_COLLAPSE)` did when probed (design 10 step 1).
/// The syscall acts on whole PMD-sized ranges only
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

/// The kernel's answer for `prctl(PR_SET_THP_DISABLE, 1,
/// PR_THP_DISABLE_EXCEPT_ADVISED)` (kernel 6.18; design 17), the
/// `thp_except_advised=` key. Probed by the call itself, never by a
/// version: on a process that carries no THP policy
/// (`PR_GET_THP_DISABLE` reads 0) the flag is set and cleared again, one
/// `prctl` each way; a kernel before 6.18 refuses a non-zero flags word
/// with `EINVAL`. A process whose parent set a policy (`PR_GET_THP_DISABLE`
/// non-zero: `PR_SET_THP_DISABLE` is inherited across `fork` and `execve`)
/// is left as it is and the key says so; the `thp_process` key then issues
/// nothing either (`thp.rs`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThpDisable {
    /// The kernel took the flag (6.18+); the process carried no policy.
    Yes,
    /// The kernel refused it (`EINVAL` before 6.18; any other error).
    No,
    /// `PR_GET_THP_DISABLE` read this non-zero value: a policy inherited
    /// from the parent, not probed, not changed (`1` = THP disabled for
    /// the process, `3` = disabled except where advised).
    Inherited(u8),
}

impl ThpDisable {
    /// `yes`, `no`, `inherited`.
    pub const fn name(self) -> &'static str {
        match self {
            ThpDisable::Yes => "yes",
            ThpDisable::No => "no",
            ThpDisable::Inherited(_) => "inherited",
        }
    }
}

impl fmt::Display for ThpDisable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "thp_except_advised={}", self.name())
    }
}

/// The kernel's transparent-huge-page configuration as read at init from
/// `/sys/kernel/mm/transparent_hugepage` with raw syscalls (design 10 step
/// 1). Folio sizes are bit sets over `log2(size)`:
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
        if let Some(v) = parse_decimal(&buf[..n], Trailing::Ignore) {
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

impl Thp {
    /// The three THP keys: `thp=<setting|none> mthp=<sizes|none>
    /// collapse=<unprobed|no|size>`.
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> {
        [
            ("thp", Value::Word(self.enabled.map_or("none", ThpSetting::name))),
            ("mthp", Value::Sizes(self.enabled_sizes())),
            (
                "collapse",
                match self.collapse {
                    Collapse::Unprobed => Value::Word("unprobed"),
                    Collapse::No => Value::Word("no"),
                    Collapse::Yes(lg) => Value::LgSize(lg),
                },
            ),
        ]
        .into_iter()
    }
}

impl fmt::Display for Thp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_words(f, self.words())
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
    /// A fixed word (`madvise`, `unprobed`, `kernel-disabled`).
    Word(&'static str),
    /// The `process_madvise` target: `no`, `pidfd_self` or `pidfd:<fd>`.
    Target(ProcessMadvise),
    /// A bit set over `log2(size)` as `64K,2M`, or `none`.
    Sizes(u64),
    /// `1 << lg` bytes as `64K`, `2M`, `1G`.
    LgSize(u8),
    /// A cache size in bytes as `<n>M`, `<n>K` or `<n>`, `unknown` for 0.
    Cache(u32),
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
            Value::LgSize(lg) => write!(f, "{}", LgSize(lg)),
            Value::Cache(0) => f.write_str("unknown"),
            Value::Cache(n) if n % (1 << 20) == 0 => write!(f, "{}M", n >> 20),
            Value::Cache(n) if n % (1 << 10) == 0 => write!(f, "{}K", n >> 10),
            Value::Cache(n) => write!(f, "{n}"),
        }
    }
}

/// `key=value` pairs separated by single spaces: the shape of the whole
/// line and of each part that prints on its own (`Thp`, `Cpu`).
pub fn write_words(f: &mut fmt::Formatter<'_>, words: impl Iterator<Item = (&'static str, Value)>) -> fmt::Result {
    for (i, (key, value)) in words.enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{key}={value}")?;
    }
    Ok(())
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

/// The `rseq=`, `rseq_size=` and `mm_cid=` words from `rseq::probe`
/// (design 15 and 17; the same three weak symbols the per-CPU mode reads,
/// read once at init, and this thread's `cpu_id`). `rseq=yes` when glibc
/// registered an area (`__rseq_size` covers the original 20-byte struct,
/// [`rseq::BASE_SIZE`]) and the kernel has written this thread's `cpu_id`;
/// `rseq_size` is the symbol's value even when the registration did not
/// take (`0` without one: `GLIBC_TUNABLES=glibc.pthread.rseq=0`, a kernel
/// without `CONFIG_RSEQ`, a libc without the symbols); `mm_cid=yes` when
/// the registered area covers the field (`__rseq_size >= 28`,
/// [`rseq::MM_CID_END`]; 33 on the 7.0 GCE kernels, 20 on a 6.10 linuxkit
/// kernel that reports no feature size). The mode's own gate is
/// [`rseq::Probe::mm_cid_usable`], which also wants `flags == 0`: the line
/// reports what exists, the mode what it acts on.
///
/// [`rseq::BASE_SIZE`]: crate::rseq::BASE_SIZE
/// [`rseq::MM_CID_END`]: crate::rseq::MM_CID_END
/// [`rseq::Probe::mm_cid_usable`]: crate::rseq::Probe::mm_cid_usable
pub fn rseq_words(p: &crate::rseq::Probe) -> impl Iterator<Item = (&'static str, Value)> {
    let registered = p.present && p.registered && p.size >= crate::rseq::BASE_SIZE;
    [
        ("rseq", Value::YesNo(registered)),
        ("rseq_size", Value::Num(u64::from(p.size))),
        ("mm_cid", Value::YesNo(registered && p.size >= crate::rseq::MM_CID_END)),
    ]
    .into_iter()
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
    /// kernel's word. Pure, host tested.
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
    /// `madvise(MADV_POPULATE_WRITE)` works (kernel 5.14+): the prefault
    /// pass populates a slab with one call, else touches it a page at a
    /// time.
    pub populate_write: bool,
    /// The transparent-huge-page configuration and the `MADV_COLLAPSE`
    /// probe: what the `thp` key can turn on.
    pub thp: Thp,
    /// Whether the kernel takes `PR_THP_DISABLE_EXCEPT_ADVISED`: what
    /// `thp_process:advised` can issue.
    pub thp_disable: ThpDisable,
    /// glibc's rseq registration as `rseq::probe` read it at init: the
    /// `rseq`, `rseq_size` and `mm_cid` keys ([`rseq_words`]); the
    /// per-CPU mode reads the same probe for its gate.
    pub rseq: crate::rseq::Probe,
    /// The kernel's answer for the rseq time slice extension and whether
    /// the process uses it (`slice.rs` decides,
    /// this is the report of it).
    pub slice_ext: SliceExt,
    /// The CPU report (design.md section 17): reported, not acted on.
    pub cpu: Cpu,
}

impl Features {
    /// Every key of the line in order: the kernel probes, then the CPU
    /// report (`Cpu::words`). Values are `yes`/`no` except `page_size`,
    /// `process_madvise` (`no`, `pidfd_self`, or `pidfd:<fd>`), the THP
    /// keys (`thp=never|madvise|always|none`, `mthp=64K,2M|none`,
    /// `collapse=unprobed|no|2M`), `thp_except_advised` (`yes`, `no`,
    /// `inherited`), `rseq_size` (a byte count, `0` without a
    /// registration), `slice_ext` (`on`, `available`, `prctl-failed`,
    /// `kernel-disabled`, `no-slice_ctrl`, `no-rseq`) and the cache sizes
    /// (`64K`, `2M`, `unknown`). `tests/c/features.c` lists the same keys.
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        [
            ("page_size", Value::Num(self.page_size as u64)),
            ("guard_install", Value::YesNo(self.guard_install)),
            ("mseal", Value::YesNo(self.mseal)),
            ("process_madvise", Value::Target(self.process_madvise)),
            ("process_madvise_free", Value::YesNo(self.process_madvise_free)),
            ("anon_name", Value::YesNo(self.anon_name)),
            ("populate_write", Value::YesNo(self.populate_write)),
        ]
        .into_iter()
        .chain(self.thp.words())
        .chain([("thp_except_advised", Value::Word(self.thp_disable.name()))])
        .chain(rseq_words(&self.rseq))
        .chain([("slice_ext", Value::Word(self.slice_ext.name()))])
        .chain(self.cpu.words())
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
    /// probe `yes`, `pidfd:2147483647`, byte-exact cache sizes, every THP
    /// size listed, the longest `thp_except_advised` and `slice_ext`
    /// words) is under `CAP - 64` bytes (`feature_line_fits`: 462 with the
    /// `populate_write`, `slice_ext` and `thp_except_advised` keys; 512
    /// held 414 before them).
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

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::{AT_PAGESZ, Collapse, Cpu, Features, ProcessMadvise, Thp, ThpDisable, page_size_from_auxv};
    use crate::sys::auxv::getauxval;
    use crate::sys::{
        self, IoVec, MADV_COLLAPSE, MADV_DONTNEED, MADV_FREE, MADV_GUARD_INSTALL, MADV_GUARD_REMOVE,
        MADV_POPULATE_WRITE, MAP_ANONYMOUS, MAP_PRIVATE, PIDFD_SELF, PR_GET_THP_DISABLE, PR_SET_THP_DISABLE,
        PR_THP_DISABLE_EXCEPT_ADVISED, PROT_NONE, PROT_READ, PROT_WRITE,
    };

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

    /// `madvise(MADV_POPULATE_WRITE)` on a fresh scratch page: the call
    /// must succeed and the page must be resident
    /// afterwards (`mincore`), so a kernel that takes the advice and does
    /// nothing would not count. `EINVAL` before 5.14.
    pub fn probe_populate_write(page: usize) -> bool {
        let Some(addr) = scratch(page, PROT_READ | PROT_WRITE) else {
            return false;
        };
        // SAFETY: our scratch page; populating changes residency, not
        // contents.
        let ok = unsafe { sys::madvise(addr, page, MADV_POPULATE_WRITE) }.is_ok() && {
            let mut vec = [0u8; 1];
            // SAFETY: one page, one byte of output.
            sys::mincore(addr, page, &mut vec).is_ok() && vec[0] & 1 != 0
        };
        release(addr, page);
        ok
    }

    /// `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)` tried
    /// and undone: the answer is the call's
    /// return, `EINVAL` before 6.18. Nothing is tried on a process that
    /// already carries a THP policy from its parent. Three `prctl`s at
    /// most, no memory touched.
    pub fn probe_thp_disable() -> ThpDisable {
        // SAFETY: a query with no pointer arguments.
        let before = match unsafe { sys::prctl(PR_GET_THP_DISABLE, 0, 0, 0, 0) } {
            Ok(v) => v,
            Err(_) => return ThpDisable::No,
        };
        if before != 0 {
            return ThpDisable::Inherited(before.min(u8::MAX as usize) as u8);
        }
        // SAFETY: integer arguments; the flag is process-wide and cleared
        // again below, so nothing else observes it for more than the
        // instant between the two calls.
        let taken = unsafe { sys::prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED, 0, 0) }.is_ok();
        if taken {
            // SAFETY: as above; `0` restores the state read before.
            if unsafe { sys::prctl(PR_SET_THP_DISABLE, 0, 0, 0, 0) }.is_err() {
                sys::abort("prctl(PR_SET_THP_DISABLE, 0) failed after the probe set it");
            }
            ThpDisable::Yes
        } else {
            ThpDisable::No
        }
    }

    /// The THP table from `/sys/kernel/mm/transparent_hugepage` (about ten
    /// sysfs reads on a 4 KiB kernel). The `MADV_COLLAPSE` probe is not run
    /// here: see [`Features::probe_collapse`].
    pub fn probe_thp(page: usize) -> Thp {
        Thp::from_reader(page, |path, buf| sys::read_small_file(path, buf).unwrap_or(0))
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
            let cpu = Cpu::probe(page_size);
            // The `cldemote` verdict: installed for `sys::cldemote::demote`,
            // which no path issues today.
            crate::sys::cldemote::configure(matches!(cpu.isa, Isa::X86(x) if x.cldemote));
            Features {
                page_size,
                page_shift: page_size.trailing_zeros(),
                guard_install: probe_guard_install(page_size),
                mseal: probe_mseal(page_size),
                process_madvise,
                process_madvise_free,
                anon_name: probe_anon_name(page_size),
                populate_write: probe_populate_write(page_size),
                thp: probe_thp(page_size),
                thp_disable: probe_thp_disable(),
                rseq: crate::rseq::probe(),
                // The kernel's answer alone: `config::load` has not run
                // yet, so nothing is enabled; `note_slice_ext` follows.
                slice_ext: super::SliceExt::from_probe(crate::slice::probe().verdict()),
                cpu,
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

        /// Run the `MADV_COLLAPSE` probe. Separate
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
        assert_eq!(parse_decimal(b"2097152\n", Trailing::Ignore), Some(2097152));
        assert_eq!(parse_decimal(b"\n", Trailing::Ignore), None);
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
    use crate::rseq::Probe;

    use crate::sys::cpu::{Arm, CacheSource, Caches, Cpu, Isa, X86};
    /// What `rseq::probe` reads on a host whose glibc registered `size`
    /// bytes at `offset` (registered when the size is not 0).
    fn rseq_probe(size: u32, offset: isize) -> Probe {
        Probe { present: true, size, flags: 0, offset, registered: size != 0 }
    }


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
            populate_write: true,
            thp: Thp { enabled: Some(ThpSetting::Madvise), lg_pmd: 21, listed: (1 << 16) | (1 << 21), always: 0, madvise: (1 << 16) | (1 << 21), collapse: Collapse::Unprobed },
            thp_disable: ThpDisable::Yes,
            rseq: rseq_probe(33, 192),
            slice_ext: SliceExt::KernelDisabled,
            cpu: axion_cpu(),
        };
        let s = std::format!("{f}");
        assert_eq!(
            s,
            "page_size=4096 guard_install=no mseal=yes process_madvise=pidfd:7 process_madvise_free=yes anon_name=no populate_write=yes \
             thp=madvise mthp=64K,2M collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=33 mm_cid=yes slice_ext=kernel-disabled \
             mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M"
        );
        // Before 6.18 the flag is refused; a parent's policy is kept.
        let f = Features { thp_disable: ThpDisable::No, ..f };
        assert!(std::format!("{f}").contains(" collapse=unprobed thp_except_advised=no rseq=yes "));
        let f = Features { thp_disable: ThpDisable::Inherited(3), ..f };
        assert!(std::format!("{f}").contains(" collapse=unprobed thp_except_advised=inherited rseq=yes "));
        assert_eq!(ThpDisable::Inherited(1).name(), "inherited");
        let f = Features { thp_disable: ThpDisable::Yes, ..f };
        // The Mac's Docker VM (linuxkit 6.10): registered, no `mm_cid`, no
        // `slice_ctrl`.
        let f = Features { rseq: rseq_probe(20, 192), slice_ext: SliceExt::NoSliceCtrl, ..f };
        assert!(std::format!("{f}").contains(" collapse=unprobed thp_except_advised=yes rseq=yes rseq_size=20 mm_cid=no slice_ext=no-slice_ctrl mte="));
        let f = Features { rseq: Probe::NONE, slice_ext: SliceExt::NoRseq, ..f };
        assert!(std::format!("{f}").contains(" rseq=no rseq_size=0 mm_cid=no slice_ext=no-rseq "));
        // A 7.0.14 built with the option (a GCE C4): on under the
        // default key, available under `slice_ext:off`.
        let f = Features { rseq: rseq_probe(33, -192), slice_ext: SliceExt::On, ..f };
        assert!(std::format!("{f}").contains(" mm_cid=yes slice_ext=on "));
        let f = Features { slice_ext: SliceExt::Available, ..f };
        assert!(std::format!("{f}").contains(" mm_cid=yes slice_ext=available "));
        let f = Features { thp: Thp { collapse: Collapse::Yes(21), ..f.thp }, ..f };
        assert!(std::format!("{f}").contains(" thp=madvise mthp=64K,2M collapse=2M thp_except_advised=yes "));
        let f = Features { thp: Thp::NONE, ..f };
        assert!(std::format!("{f}").contains(" thp=none mthp=none collapse=no "));
        let f = Features { thp: Thp { enabled: Some(ThpSetting::Never), listed: 1 << 21, always: 0, madvise: 0, collapse: Collapse::No, lg_pmd: 21 }, ..f };
        assert!(std::format!("{f}").contains(" thp=never mthp=none collapse=no "));
        let f = Features { process_madvise: ProcessMadvise::PidfdSelf, ..f };
        assert!(std::format!("{f}").contains("process_madvise=pidfd_self process_madvise_free=yes "));
        let f = Features { process_madvise: ProcessMadvise::Unavailable, process_madvise_free: false, ..f };
        assert!(std::format!("{f}").contains("process_madvise=no process_madvise_free=no "));
        // Before 5.14.
        let f = Features { populate_write: false, ..f };
        assert!(std::format!("{f}").contains(" anon_name=no populate_write=no thp="));
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
            populate_write: false,
            thp: Thp::NONE,
            thp_disable: ThpDisable::No,
            rseq: Probe::NONE,
            slice_ext: SliceExt::NoRseq,
            cpu: axion_cpu(),
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
                populate_write: true,
                // A 64 KiB-page kernel listing every size from 128K to 512M.
                thp: Thp {
                    enabled: Some(ThpSetting::Madvise),
                    lg_pmd: 29,
                    listed: ((1u64 << 30) - 1) & !((1u64 << 17) - 1),
                    always: ((1u64 << 30) - 1) & !((1u64 << 17) - 1),
                    madvise: 0,
                    collapse: Collapse::Yes(29),
                },
                // The longest words.
                thp_disable: ThpDisable::Inherited(u8::MAX),
                rseq: Probe { present: true, size: u32::MAX, flags: u32::MAX, offset: isize::MIN, registered: true },
                slice_ext: SliceExt::KernelDisabled,
                cpu,
            };
            let s = std::format!("{f}");
            assert!(s.len() < FeatureLine::CAP - 64, "{} bytes: {s}", s.len());
            // The table is what the line is made of: the same bytes, word
            // by word.
            let by_words: usize = f.words().map(|(k, v)| k.len() + 1 + std::format!("{v}").len() + 1).sum::<usize>() - 1;
            assert_eq!(by_words, s.len());
        }
    }

    /// The key table: every key of `tests/c/features.c`, once, in order,
    /// the kernel probes first and this architecture's CPU keys after;
    /// every value one token. Adding a feature means one row here, one
    /// in `features.c`, and the probe.
    #[test]
    fn words_are_the_key_table() {
        let kernel = [
            "page_size", "guard_install", "mseal", "process_madvise", "process_madvise_free", "anon_name", "populate_write",
            "thp", "mthp", "collapse", "thp_except_advised", "rseq", "rseq_size", "mm_cid", "slice_ext",
        ];
        let arm = ["mte", "mte3", "pac", "lse", "lse2", "bti", "sve", "cacheline", "l1d", "l2"];
        let x86 = ["lam", "uai", "la57", "la57_enabled", "cldemote", "waitpkg", "erms", "fsrm", "avx512f", "cacheline", "l1d", "l2"];
        let x86_cpu = Cpu { isa: Isa::X86(X86::default()), caches: Caches::UNKNOWN, cache_source: CacheSource::None };
        for (cpu, cpu_keys) in [(axion_cpu(), &arm[..]), (x86_cpu, &x86[..])] {
            let f = Features {
                page_size: 4096,
                page_shift: 12,
                guard_install: true,
                mseal: false,
                process_madvise: ProcessMadvise::PidfdSelf,
                process_madvise_free: true,
                anon_name: true,
                populate_write: true,
                thp: Thp::NONE,
                thp_disable: ThpDisable::No,
                rseq: rseq_probe(33, 192),
                slice_ext: SliceExt::Available,
                cpu,
            };
            let keys: std::vec::Vec<&str> = f.words().map(|(k, _)| k).collect();
            let want: std::vec::Vec<&str> = kernel.iter().chain(cpu_keys).copied().collect();
            assert_eq!(keys, want);
            let mut seen = std::collections::BTreeSet::new();
            for (k, v) in f.words() {
                assert!(seen.insert(k), "{k} twice");
                let v = std::format!("{v}");
                assert!(!k.is_empty() && !v.is_empty() && !k.contains([' ', '=']) && !v.contains([' ', '=']), "{k}={v}");
            }
            let joined: std::vec::Vec<std::string::String> = f.words().map(|(k, v)| std::format!("{k}={v}")).collect();
            assert_eq!(std::format!("{f}"), joined.join(" "));
            assert_eq!(std::format!("{}", f.cpu), joined[kernel.len()..].join(" "));
            assert_eq!(std::format!("{}", f.thp), joined[7..10].join(" "));
            assert_eq!(FeatureLine::build(&f).as_str(), s);
        }
    }

    /// The three rseq words on the values the prerequisites note recorded:
    /// the GCE VMs (glibc 2.44 on 7.0, `__rseq_size=33`, offset 192 on
    /// aarch64 and -192/-256 on x86_64), the Mac's Docker VM (6.10,
    /// `__rseq_size=20`), a libc without the symbols, a registration the
    /// kernel never wrote, and the sizes around the `mm_cid` boundary.
    #[test]
    fn rseq_words_follow_size_and_registration() {
        let line = |p: &Probe| -> std::string::String {
            let v: std::vec::Vec<std::string::String> = rseq_words(p).map(|(k, v)| std::format!("{k}={v}")).collect();
            v.join(" ")
        };
        for off in [192isize, -192, -256] {
            let p = rseq_probe(33, off);
            assert!(p.mm_cid_usable(), "{p:?}");
            assert_eq!(line(&p), "rseq=yes rseq_size=33 mm_cid=yes");
        }
        let p = rseq_probe(20, 192);
        assert!(!p.mm_cid_usable());
        assert_eq!(line(&p), "rseq=yes rseq_size=20 mm_cid=no");
        assert_eq!(line(&Probe::NONE), "rseq=no rseq_size=0 mm_cid=no");
        assert_eq!(line(&rseq_probe(0, 0)), "rseq=no rseq_size=0 mm_cid=no", "symbols present, nothing registered");
        // The kernel did not write the area: no registration, whatever the size says.
        let p = Probe { registered: false, ..rseq_probe(33, -192) };
        assert_eq!(line(&p), "rseq=no rseq_size=33 mm_cid=no");
        // Flags the mode does not know keep the mode off; the line still
        // says what exists.
        let p = Probe { flags: 1, ..rseq_probe(33, 192) };
        assert!(!p.mm_cid_usable());
        assert_eq!(line(&p), "rseq=yes rseq_size=33 mm_cid=yes");
        // The boundary: 27 bytes do not hold a 4-byte `mm_cid` at 24; 28
        // do; 32 (a glibc that rounds to the alignment) does.
        assert!(line(&rseq_probe(27, 192)).ends_with(" mm_cid=no"));
        assert!(line(&rseq_probe(28, 192)).ends_with(" mm_cid=yes"));
        assert!(line(&rseq_probe(32, 192)).ends_with(" mm_cid=yes"));
        // Below the original struct nothing is registered.
        assert_eq!(line(&rseq_probe(19, 192)), "rseq=no rseq_size=19 mm_cid=no");
        assert_eq!(crate::rseq::MM_CID_END, 28);
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

    /// `MADV_POPULATE_WRITE`: 5.14, and never filtered (plain `madvise`),
    /// so both directions hold. Usable as reported: a fresh page is not
    /// resident, one call makes it so, and its contents stay zero.
    #[test]
    fn probe_populate_write_follows_the_kernel_both_ways() {
        let (kv, f, line) = probed();
        if !at_least(kv, (5, 14)) {
            assert!(!f.populate_write, "MADV_POPULATE_WRITE predates 5.14: {line}");
        } else {
            assert!(f.populate_write, "MADV_POPULATE_WRITE should work on 5.14+: {line}");
        }
        if f.populate_write {
            let page = f.page_size;
            let fresh = sys::tests::map_anon(page);
            let mut vec = [0u8; 1];
            sys::mincore(fresh, page, &mut vec).unwrap();
            assert_eq!(vec[0] & 1, 0, "fresh scratch page resident before the populate");
            // SAFETY: our page.
            unsafe { sys::madvise(fresh, page, sys::MADV_POPULATE_WRITE).unwrap() };
            sys::mincore(fresh, page, &mut vec).unwrap();
            assert_eq!(vec[0] & 1, 1, "page not resident after MADV_POPULATE_WRITE");
            // SAFETY: our page, mapped above and unmapped here.
            unsafe {
                assert_eq!((fresh as *mut u8).read_volatile(), 0);
                sys::munmap(fresh, page).unwrap();
            }
        }
    }

    /// The rseq words against glibc's own symbols through `dlsym` and the
    /// kernel's `AT_RSEQ_FEATURE_SIZE`: the values must be the symbols',
    /// the probe the line is built from must be `rseq::probe`'s, and
    /// `mm_cid` is `yes` exactly when glibc registered an area whose size
    /// reaches 28, which the kernel allows from 6.3 (the GCE 7.0 VMs: 33;
    /// the Mac's 6.10 linuxkit VM reports no feature size and glibc
    /// registers 20). Both ways: it runs on hosts of each kind.
    #[test]
    fn rseq_words_match_glibc_symbols_and_the_feature_size() {
        assert_eq!(f.rseq, crate::rseq::probe(), "the line's probe is the mode's");
        let (kv, f, line) = probed();
        let sym = |name: &[u8]| -> *const u8 {
            // SAFETY: `dlsym` on a NUL-terminated name in the test.
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }.cast()
        };
        let (ps, po, pf) = (sym(b"__rseq_size\0"), sym(b"__rseq_offset\0"), sym(b"__rseq_flags\0"));
        if ps.is_null() {
            assert_eq!(f.rseq, crate::rseq::Probe::NONE, "no __rseq_size symbol: {line}");
            assert!(line.contains(" rseq=no rseq_size=0 mm_cid=no "), "{line}");
            return;
        }
        // SAFETY: glibc's exported objects of these types.
        assert!(f.rseq.present, "{line}");
        let (size, offset, flags) = unsafe { (ps.cast::<u32>().read(), po.cast::<isize>().read(), pf.cast::<u32>().read()) };
        assert_eq!((f.rseq.size, f.rseq.offset, f.rseq.flags), (size, offset, flags), "{line}");
        let registered = size >= crate::rseq::BASE_SIZE && f.rseq.registered;
        eprintln!("rseq: __rseq_size={size} __rseq_offset={offset} __rseq_flags={flags} registered={}", f.rseq.registered);
        assert!(line.contains(&format!(" rseq={} rseq_size={size} mm_cid=", sys::yn(registered))), "{line}");
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
        if feat >= crate::rseq::MM_CID_END && size >= crate::rseq::BASE_SIZE {
            assert!(at_least(kv, (6, 3)), "mm_cid predates 6.3: {line}");
            assert!(size >= crate::rseq::MM_CID_END, "kernel offers {feat} bytes, glibc registered {size}: {line}");
            assert!(line.contains(" rseq=yes rseq_size=") && line.contains(" mm_cid=yes "), "{line}");
        } else {
            assert!(line.contains(" mm_cid=no "), "{line}");
        }
    }

    /// `PR_THP_DISABLE_EXCEPT_ADVISED`, both ways: `yes` needs 6.18 and a
    /// process without a THP policy; below 6.18 the flag is `EINVAL`, so
    /// `no`; and the probe leaves the process's policy as it found it
    /// (`PR_GET_THP_DISABLE` unchanged), which the kernel's own
    /// `/proc/self/status` field confirms.
    #[test]
    fn probe_thp_disable_follows_the_kernel_and_restores_the_policy() {
        let (kv, f, line) = probed();
        // SAFETY: a query with no pointer arguments.
        let get = unsafe { sys::prctl(sys::PR_GET_THP_DISABLE, 0, 0, 0, 0) }.unwrap_or(usize::MAX);
        match f.thp_disable {
            ThpDisable::Yes => {
                assert!(at_least(kv, (6, 18)), "PR_THP_DISABLE_EXCEPT_ADVISED predates 6.18: {line}");
                assert_eq!(get, 0, "the probe changed the process's policy: {line}");
            }
            ThpDisable::No => {
                // A kernel from 6.18 on takes it unless something in
                // front of `prctl` refuses (seccomp).
                assert!(get == 0 || get == usize::MAX, "{line}");
            }
            ThpDisable::Inherited(v) => {
                assert_eq!(get, v as usize, "{line}");
            }
        }
        if !at_least(kv, (6, 18)) {
            assert_ne!(f.thp_disable, ThpDisable::Yes, "{line}");
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let thp_enabled = status.lines().find(|l| l.starts_with("THP_enabled:")).map(str::trim);
        eprintln!("thp_except_advised: {} (PR_GET_THP_DISABLE {get}, {})", f.thp_disable.name(), thp_enabled.unwrap_or("no THP_enabled field"));
        if f.thp_disable == ThpDisable::Yes {
            // The probe: take it and undo it once more, watching the
            // kernel's view move and come back.
            // SAFETY: integer arguments only; the flag is process-wide and
            // cleared again two calls below, restoring the state read
            // above, so no other test observes a changed policy.
            unsafe {
                let mid = sys::prctl(sys::PR_SET_THP_DISABLE, 1, sys::PR_THP_DISABLE_EXCEPT_ADVISED, 0, 0);
                assert_eq!(mid, Ok(0), "{line}");
                let got = sys::prctl(sys::PR_GET_THP_DISABLE, 0, 0, 0, 0).unwrap();
                assert_eq!(got, 1 | sys::PR_THP_DISABLE_EXCEPT_ADVISED, "{line}");
                assert_eq!(sys::prctl(sys::PR_SET_THP_DISABLE, 0, 0, 0, 0), Ok(0));
                assert_eq!(sys::prctl(sys::PR_GET_THP_DISABLE, 0, 0, 0, 0), Ok(0));
            }
            assert_eq!(probe_thp_disable(), ThpDisable::Yes);
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
