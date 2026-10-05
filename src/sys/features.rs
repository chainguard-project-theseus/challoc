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
//! or a seccomp profile).
//!
//! The probes never allocate, take a few microseconds, and leave at most one
//! `PROT_NONE` page mapped (the sealed page, which by definition cannot be
//! unmapped again). The THP table is read by `sys::thp_host` and the rseq
//! registration by `rseq::probe`; this module records both.
//!
//! The line is a table: [`Features::words`] yields every `(key, value)`
//! pair in order, and `Display`, [`FeatureLine`] and the tests all iterate
//! it, so a new kernel feature is one probe, one field and one row.

use core::fmt;

use super::auxv::{AT_PAGESZ, getauxval};
use super::thp_host::Thp;
use super::{
    MADV_GUARD_INSTALL, MADV_GUARD_REMOVE, MAP_ANONYMOUS, MAP_PRIVATE, PROT_NONE, PROT_READ,
    PROT_WRITE, yn,
};
use crate::sys;
use vstd::prelude::verus;
#[cfg(verus_only)]
use vstd::prelude::*;

verus! {

/// Bit sets of sizes as `64K,2M`, or `none`.
struct SizeSet(u64);

// Every set formats (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for SizeSet {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for SizeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("none");
        }
        let mut first = true;
        for lg in 0..64u8
            invariant
                lg <= 64,
        {
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

// A size formats when `1 << lg` is a `u64`: the one caller passes a bit
// index of a `u64` set (ghost only; closed because the field is private
// to this module).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for LgSize {
    closed spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        self.0 < 64
    }
}

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
    /// A bit set over `log2(size)` as `64K,2M`, or `none`.
    Sizes(u64),
}

// Every value formats (ghost only).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Value {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Value::YesNo(b) => f.write_str(yn(b)),
            Value::Num(n) => write!(f, "{n}"),
            Value::Word(w) => f.write_str(w),
            Value::Sizes(set) => write!(f, "{}", SizeSet(set)),
        }
    }
}

/// `key=value` pairs separated by single spaces: the shape of the whole
/// line and of each part that prints on its own (`Thp`).
pub fn write_words(f: &mut fmt::Formatter<'_>, words: &[(&'static str, Value)]) -> fmt::Result {
    // `for (i, (key, value)) in words.iter().enumerate()`, with an index:
    // Verus does not specify a slice's iterator.
    let mut i = 0;
    while i < words.len()
        invariant
            i <= words@.len(),
        decreases words@.len() - i,
    {
        let (key, value) = words[i];
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{key}={value}")?;
        i += 1;
    }
    Ok(())
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
    /// `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME)` works
    /// (kernel 5.17+ with `CONFIG_ANON_VMA_NAME`).
    pub anon_name: bool,
    /// The transparent-huge-page configuration: what the `thp` key can
    /// turn on.
    pub thp: Thp,
    /// glibc's rseq registration as `rseq::probe` read it at init: the
    /// `rseq` and `rseq_size` keys (`rseq::Probe::words`).
    pub rseq: crate::rseq::Probe,
    /// Pointer authentication (`sys::pac::available`, `HWCAP_PACA` and
    /// `HWCAP_PACG`): what `pac_pointers` can turn on.
    pub pac: bool,
}

impl Features {
    /// Every key of the line in order: the kernel probes, then `pac`.
    /// Values are `yes`/`no` except `page_size`, the THP keys
    /// (`thp=never|madvise|always|none`, `mthp=64K,2M|none`), `rseq_size`
    /// (a byte count).
    /// `tests/c/features.c` lists the same keys.
    pub fn words(&self) -> [(&'static str, Value); 9] {
        let thp = self.thp.words();
        let rseq = self.rseq.words();
        [
            ("page_size", Value::Num(self.page_size as u64)),
            ("guard_install", Value::YesNo(self.guard_install)),
            ("mseal", Value::YesNo(self.mseal)),
            ("anon_name", Value::YesNo(self.anon_name)),
            thp[0],
            thp[1],
            rseq[0],
            rseq[1],
            ("pac", Value::YesNo(self.pac)),
        ]
    }
}

// Every line formats (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Features {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for Features {
    /// One line, `key=value` separated by spaces, for `challoc_features()`,
    /// `malloc_stats_print` and abort messages: [`Features::words`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_words(f, &self.words())
    }
}

/// The feature line as a NUL-terminated string, built once at init for
/// `challoc_features()` (design.md section 16) and reused by
/// `malloc_stats_print`, so the two report the same bytes.
pub struct FeatureLine {
    buf: [u8; FeatureLine::CAP],
    len: usize,
}

impl FeatureLine {
    /// Room for the NUL after the line.
    #[verifier::type_invariant]
    spec fn wf(&self) -> bool {
        self.len < Self::CAP
    }

    /// Room for the line and its NUL; the longest possible line (every
    /// probe `yes`, every THP size listed) is under `CAP - 64` bytes (`feature_line_fits`).
    pub const CAP: usize = 640;

    /// Format `features` into a fresh line. A line that did not fit is cut,
    /// which the tests would notice; it cannot happen with the current keys.
    pub fn build(features: &Features) -> FeatureLine {
        use fmt::Write as _;
        let mut line = FeatureLine {
            buf: [0; Self::CAP],
            len: 0,
        };
        let _ = write!(line, "{}", *features);
        proof {
            use_type_invariant(&line);
        }
        line.buf[line.len] = 0;
        line
    }

    /// The line without its NUL.
    pub fn as_str(&self) -> &str {
        proof {
            use_type_invariant(self);
        }
        // Only `str` fragments were copied, cut at a fragment boundary.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// The NUL-terminated line, for C callers.
    pub fn as_ptr(&self) -> *const core::ffi::c_char {
        self.buf.as_ptr() as *const core::ffi::c_char
    }
}

impl fmt::Write for FeatureLine {
    // `fmt::Error::default()` below: Verus cannot name the constructor of
    // an external unit struct.
    #[allow(clippy::default_constructed_unit_structs)]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        proof {
            use_type_invariant(&*self);
        }
        let b = s.as_bytes();
        let room = Self::CAP - 1 - self.len;
        if b.len() > room {
            return Err(fmt::Error::default());
        }
        // `self.buf[self.len..self.len + s.len()].copy_from_slice(..)`,
        // spelled out: Verus does not take a `&mut` through a field of a
        // type with an invariant in a call that may unwind.
        let mut i = 0;
        while i < b.len()
            invariant
                self.len + b@.len() < Self::CAP,
                i <= b@.len(),
            decreases b@.len() - i,
        {
            self.buf[self.len + i] = b[i];
            i += 1;
        }
        self.len += b.len();
        Ok(())
    }
}

/// The system page size, `getauxval(AT_PAGESZ)`. Without it, abort:
/// guessing a page size would corrupt every mapping we make.
pub fn page_size() -> (r: usize)
    requires
        crate::sys::may_abort(),
    ensures
        r != 0,
        crate::meta::is_pow2(r),
        r <= 0x1_0000,
{
    let v = getauxval(AT_PAGESZ);
    if v != 0 && v.is_power_of_two() {
        return v;
    }
    sys::abort("cannot determine the page size (AT_PAGESZ missing)")
}

/// Map a fresh private anonymous scratch page with the given protection.
fn scratch(page: usize, prot: i32) -> Option<usize>
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: fresh anonymous private mapping, no fixed address.
    unsafe { sys::mmap(0, page, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.ok()
}

/// Release a scratch page. Failure is a heap inconsistency: we just
/// mapped it, so it is unmapped only if something is very wrong.
fn release(addr: usize, page: usize)
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    broadcast use crate::vspec::fmt::lemma_errno_fmt_req;
    // SAFETY: `addr` is a scratch page nothing else references.
    if let Err(e) = unsafe { sys::munmap(addr, page) } {
        sys::abort_fmt(format_args!("munmap of probe page failed: {e}"));
    }
}

/// `MADV_GUARD_INSTALL` then `MADV_GUARD_REMOVE` on a scratch page. Both
/// must succeed for the feature to count: a kernel that installs guards
/// it cannot remove would break slab reuse.
pub fn probe_guard_install(page: usize) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
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
pub fn probe_mseal(page: usize) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
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

/// The VMA name the anonymous-name probe sets, NUL-terminated (spelled
/// as bytes so the verifier sees the NUL `set_vma_anon_name` requires).
const PROBE_NAME: [u8; 14] = [
    b'c', b'h', b'a', b'l', b'l', b'o', b'c', b':', b'p', b'r', b'o', b'b', b'e', 0,
];

/// `PR_SET_VMA_ANON_NAME` on a scratch page.
pub fn probe_anon_name(page: usize) -> bool
    requires
        crate::sys::may_abort(),
        crate::sys::may_syscall(),
{
    let Some(addr) = scratch(page, PROT_NONE) else {
        return false;
    };
    // SAFETY: `addr` is our scratch page; the name is static and
    // NUL-terminated.
    let ok = unsafe { sys::set_vma_anon_name(addr, page, &PROBE_NAME) }.is_ok();
    release(addr, page);
    ok
}

impl Features {
    /// Probe everything. Called once from init (design.md section 10);
    /// calling it again is harmless but leaks a sealed page, so init
    /// stores the result. Pure apart from that page.
    pub fn probe() -> (r: Features)
        requires
            crate::sys::may_abort(),
            crate::sys::may_syscall(),
        ensures
            r.page_size != 0,
            crate::meta::is_pow2(r.page_size),
            r.page_size <= 1usize << 33 ==> r.thp.lg_pmd < 64,
            r.page_size <= 0x1_0000,
            r.pac ==> crate::sys::pac::pac_available(),
    {
        let page_size = page_size();
        Features {
            page_size,
            page_shift: page_size.trailing_zeros(),
            guard_install: probe_guard_install(page_size),
            mseal: probe_mseal(page_size),
            anon_name: probe_anon_name(page_size),
            thp: crate::sys::thp_host::probe(page_size),
            rseq: crate::rseq::probe(),
            pac: crate::sys::pac::available(),
        }
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rseq::Probe;
    use crate::sys::thp_host::ThpSetting;

    /// What `rseq::probe` reads on a host whose glibc registered `size`
    /// bytes at `offset` (registered when the size is not 0).
    fn rseq_probe(size: u32, offset: isize) -> Probe {
        Probe {
            present: true,
            size,
            flags: 0,
            offset,
            registered: size != 0,
        }
    }

    #[test]
    fn display_is_one_line_of_key_values() {
        let f = Features {
            page_size: 4096,
            page_shift: 12,
            guard_install: false,
            mseal: true,
            anon_name: false,
            thp: Thp {
                enabled: Some(ThpSetting::Madvise),
                lg_pmd: 21,
                listed: (1 << 16) | (1 << 21),
                always: 0,
                madvise: (1 << 16) | (1 << 21),
            },
            rseq: rseq_probe(33, 192),
            pac: true,
        };
        let s = std::format!("{f}");
        assert_eq!(
            s,
            "page_size=4096 guard_install=no mseal=yes anon_name=no thp=madvise \
             mthp=64K,2M rseq=yes rseq_size=33 pac=yes"
        );
        // A kernel that reports no feature size (6.10): registered at 20.
        let f = Features {
            rseq: rseq_probe(20, 192),
            ..f
        };
        assert!(std::format!("{f}").contains(" mthp=64K,2M rseq=yes rseq_size=20 pac=yes"));
        let f = Features {
            rseq: Probe::NONE,
            ..f
        };
        assert!(std::format!("{f}").contains(" rseq=no rseq_size=0 pac=yes"));
        let f = Features {
            rseq: rseq_probe(33, -192),
            ..f
        };
        let f = Features {
            thp: Thp::NONE,
            ..f
        };
        assert!(std::format!("{f}").contains(" thp=none mthp=none rseq=yes "));
        let f = Features {
            thp: Thp {
                enabled: Some(ThpSetting::Never),
                listed: 1 << 21,
                always: 0,
                madvise: 0,
                lg_pmd: 21,
            },
            ..f
        };
        assert!(std::format!("{f}").contains(" thp=never mthp=none rseq=yes "));
    }

    #[test]
    fn feature_line_is_the_display_with_a_nul() {
        let f = Features {
            page_size: 16384,
            page_shift: 14,
            guard_install: true,
            mseal: true,
            anon_name: true,
            thp: Thp::NONE,
            rseq: Probe::NONE,
            pac: true,
        };
        let line = FeatureLine::build(&f);
        assert_eq!(line.as_str(), std::format!("{f}"));
        // SAFETY: `as_ptr` names the line's own buffer, which holds `len`
        // bytes of text and the NUL after them.
        let bytes =
            unsafe { core::slice::from_raw_parts(line.as_ptr() as *const u8, line.len + 1) };
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
            rseq: Probe {
                present: true,
                size: u32::MAX,
                flags: u32::MAX,
                offset: isize::MIN,
                registered: true,
            },
            pac: true,
        };
        let s = std::format!("{f}");
        assert!(s.len() < FeatureLine::CAP - 64, "{} bytes: {s}", s.len());
        assert_eq!(FeatureLine::build(&f).as_str(), s);
        // The table is what the line is made of: the same bytes, word
        // by word.
        let by_words: usize = f
            .words()
            .into_iter()
            .map(|(k, v)| k.len() + 1 + std::format!("{v}").len() + 1)
            .sum::<usize>()
            - 1;
        assert_eq!(by_words, s.len());
    }

    /// The key table: every key of `tests/c/features.c`, once, in order;
    /// every value one token. Adding a feature means one row here, one
    /// in `features.c`, and the probe.
    #[test]
    fn words_are_the_key_table() {
        let kernel = [
            "page_size",
            "guard_install",
            "mseal",
            "anon_name",
            "thp",
            "mthp",
            "rseq",
            "rseq_size",
            "pac",
        ];
        for pac in [true, false] {
            let f = Features {
                page_size: 4096,
                page_shift: 12,
                guard_install: true,
                mseal: false,
                anon_name: true,
                thp: Thp::NONE,
                rseq: rseq_probe(33, 192),
                pac,
            };
            let keys: std::vec::Vec<&str> = f.words().into_iter().map(|(k, _)| k).collect();
            assert_eq!(keys, kernel);
            let mut seen = std::collections::BTreeSet::new();
            for (k, v) in f.words() {
                assert!(seen.insert(k), "{k} twice");
                let v = std::format!("{v}");
                assert!(
                    !k.is_empty()
                        && !v.is_empty()
                        && !k.contains([' ', '='])
                        && !v.contains([' ', '=']),
                    "{k}={v}"
                );
            }
            let joined: std::vec::Vec<std::string::String> = f
                .words()
                .into_iter()
                .map(|(k, v)| std::format!("{k}={v}"))
                .collect();
            assert_eq!(std::format!("{f}"), joined.join(" "));
            assert_eq!(std::format!("{}", f.thp), joined[4..6].join(" "));
        }
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
    /// afresh (harmless: one more sealed page).
    fn probed() -> ((u32, u32), Features, String) {
        let kv = kernel_version();
        let f = Features::probe();
        let line = format!("{f}");
        eprintln!("kernel {}.{}: {line}", kv.0, kv.1);
        (kv, f, line)
    }

    // A feature newer than the kernel must probe absent. The converse is
    // only asserted where nothing but the kernel decides: seccomp filters
    // mseal and CONFIG_ANON_VMA_NAME is optional, but
    // madvise itself is never filtered.

    /// The line prints and the page geometry is consistent.
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
            assert!(
                f.guard_install,
                "MADV_GUARD_INSTALL should work on 6.13+: {line}"
            );
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
            assert!(
                maps.lines()
                    .any(|l| l.split_whitespace().nth(1) == Some("---p")),
                "{maps}"
            );
        }
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
            let l: Vec<&str> = maps
                .lines()
                .filter(|l| l.starts_with(&format!("{addr:x}-")))
                .collect();
            assert!(
                l.iter().any(|l| l.contains("[anon:challoc:probe2]")),
                "{l:?}"
            );
            // SAFETY: our page.
            unsafe { sys::munmap(addr, page).unwrap() };
        }
        // The probe must not have left its named scratch page behind: it
        // releases the page whether or not the prctl worked.
        let maps: String = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(
            !maps.contains("[anon:challoc:probe]"),
            "probe page leaked: {maps}"
        );
    }

    /// The rseq words against glibc's own symbols through `dlsym` and the
    /// kernel's `AT_RSEQ_FEATURE_SIZE`: the values must be the symbols',
    /// the probe the line is built from must be `rseq::probe`'s, and a
    /// registration covers every field the kernel offers (a kernel without
    /// a feature size gets the 20-byte original struct).
    /// Both ways: it runs on hosts of each kind.
    #[test]
    fn rseq_words_match_glibc_symbols_and_the_feature_size() {
        let (_, f, line) = probed();
        assert_eq!(
            f.rseq,
            crate::rseq::probe(),
            "the line's probe is rseq::probe's"
        );
        let sym = |name: &[u8]| -> *const u8 {
            // SAFETY: `dlsym` on a NUL-terminated name in the test.
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }.cast()
        };
        let (ps, po, pf) = (
            sym(b"__rseq_size\0"),
            sym(b"__rseq_offset\0"),
            sym(b"__rseq_flags\0"),
        );
        if ps.is_null() {
            assert_eq!(
                f.rseq,
                crate::rseq::Probe::NONE,
                "no __rseq_size symbol: {line}"
            );
            assert!(line.contains(" rseq=no rseq_size=0 "), "{line}");
            return;
        }
        // SAFETY: glibc's exported objects of these types.
        let (size, offset, flags) = unsafe {
            (
                ps.cast::<u32>().read(),
                po.cast::<isize>().read(),
                pf.cast::<u32>().read(),
            )
        };
        assert!(f.rseq.present, "{line}");
        assert_eq!(
            (f.rseq.size, f.rseq.offset, f.rseq.flags),
            (size, offset, flags),
            "{line}"
        );
        let registered = size >= crate::rseq::BASE_SIZE && f.rseq.registered;
        eprintln!(
            "rseq: __rseq_size={size} __rseq_offset={offset} __rseq_flags={flags} registered={}",
            f.rseq.registered
        );
        assert!(
            line.contains(&format!(" rseq={} rseq_size={size} ", sys::yn(registered))),
            "{line}"
        );
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
            assert!(
                size >= feat,
                "kernel offers {feat} bytes, glibc registered {size}: {line}"
            );
        }
    }

    /// The THP table against the same files read through std.
    #[test]
    fn probe_thp_table_matches_sysfs() {
        let (kv, f, line) = probed();
        let dir = std::path::Path::new("/sys/kernel/mm/transparent_hugepage");
        match std::fs::read_to_string(dir.join("enabled")) {
            Ok(e) => {
                assert_eq!(
                    f.thp.enabled,
                    crate::sys::thp_host::parse_thp_setting(e.as_bytes()),
                    "{line}"
                );
                let mut listed = 0u64;
                for entry in std::fs::read_dir(dir).unwrap() {
                    let name = entry.unwrap().file_name();
                    let name = name.to_str().unwrap();
                    if let Some(kb) = name
                        .strip_prefix("hugepages-")
                        .and_then(|s| s.strip_suffix("kB"))
                    {
                        if dir.join(name).join("enabled").exists() {
                            listed |= 1 << (kb.parse::<u64>().unwrap() * 1024).trailing_zeros();
                        }
                    }
                }
                if listed == 0 {
                    listed = 1 << f.thp.lg_pmd;
                } else {
                    assert!(
                        at_least(kv, (6, 8)),
                        "per-size directories predate 6.8: {line}"
                    );
                }
                assert_eq!(f.thp.listed, listed, "{line}");
            }
            Err(_) => assert_eq!(f.thp, Thp::NONE, "{line}"),
        }
        assert!(f.thp.lg_pmd as usize > f.page_shift as usize, "{line}");
    }
}

/// Kani checks of the `Display` impls of `SizeSet` and `LgSize` (records
/// under `verus/seams/`).
#[cfg(kani)]
mod kani_fmt {
    use super::{LgSize, SizeSet};
    use crate::kani_proofs::fmt::formats;

    /// S217 `DisplaySpecImpl for SizeSet` (`fmt_req` true): every set.
    #[kani::proof]
    #[kani::unwind(66)]
    fn display_size_set() {
        formats(&SizeSet(kani::any()));
    }

    /// S218 `DisplaySpecImpl for LgSize` (`fmt_req` is `self.0 < 64`):
    /// every such size.
    #[kani::proof]
    #[kani::unwind(22)]
    fn display_lg_size() {
        let lg: u8 = kani::any();
        kani::assume(lg < 64);
        formats(&LgSize(lg));
    }
}

/// Kani check of `Features`' `Display` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_features {
    use super::{Features, Value};
    use crate::kani_proofs::fmt::formats;
    use crate::rseq::Probe;
    use crate::sys::thp_host::{Thp, ThpSetting};
    use core::fmt;

    /// `Value`'s `fmt` as its own harness (`display_feature_value`, S232)
    /// shows it: it returns, `Ok` or `Err`, without a panic, for every
    /// value. What it writes does not matter to the caller.
    #[allow(dead_code)] // Used through `kani::stub`, which rustc does not see.
    fn value_fmt(_: &Value, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        if kani::any() { Ok(()) } else { Err(fmt::Error) }
    }

    /// S220 `DisplaySpecImpl for Features` (`fmt_req` true), through
    /// `Features::fmt` (verified in Verus): every
    /// `Features` value, under the plain and a padded formatter, whatever
    /// the writer returns. Each `Value` is formatted by the stub above,
    /// whose behaviour (any result, no panic) is what S232's harness
    /// proves of the real `fmt` for every value; the rest (the word
    /// arrays, `write_words`, the keys) runs as built.
    #[kani::proof]
    #[kani::stub(<Value as core::fmt::Display>::fmt, value_fmt)]
    #[kani::unwind(12)]
    fn display_features() {
        let settings = [
            ThpSetting::Always,
            ThpSetting::Inherit,
            ThpSetting::Madvise,
            ThpSetting::Never,
        ];
        let s: usize = kani::any();
        kani::assume(s <= settings.len());
        let f = Features {
            page_size: kani::any(),
            page_shift: kani::any(),
            guard_install: kani::any(),
            mseal: kani::any(),
            anon_name: kani::any(),
            thp: Thp {
                enabled: if s < settings.len() {
                    Some(settings[s])
                } else {
                    None
                },
                lg_pmd: kani::any(),
                listed: kani::any(),
                always: kani::any(),
                madvise: kani::any(),
            },
            rseq: Probe {
                present: kani::any(),
                size: kani::any(),
                flags: kani::any(),
                offset: kani::any(),
                registered: kani::any(),
            },
            pac: kani::any(),
        };
        formats(&f);
    }
}
