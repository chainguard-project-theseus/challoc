//! CPU feature report (design.md section 17, "Floor and ceiling").
//!
//! Everything here is *reported*, never acted on: no fast path reads these
//! values and no feature is turned on by them. The report exists so that a
//! host's ceiling is visible (`challoc_features()`, `malloc_stats_print`)
//! before a later task lights a feature, and so that the feature matrix can
//! record which hosts expose what (MTE on Axion, LAM on the Xeon 8581C).
//!
//! Sources, all read once at init on cold paths and never through libc:
//!
//! - arm64: `AT_HWCAP` / `AT_HWCAP2` from the auxiliary vector (the kernel's
//!   view of what user space may use, which is what matters for MTE and PAC:
//!   a CPU can have them and the kernel can still withhold them), and the
//!   cache-line size from `CTR_EL0` (`mrs` from EL0; Linux traps and
//!   emulates the read where the hardware would fault).
//! - x86_64: `cpuid` leaf 7 (subleaves 0 and 1) for the feature bits, leaf
//!   `0x80000021` for AMD's upper address ignore (`uai`, the LAM analogue;
//!   the leaf exists only where the extended range reaches it), the
//!   deterministic cache parameters (leaf 4 on Intel, `0x8000001D` on AMD)
//!   for the cache sizes, and a hinted `mmap` above 2^47 for whether the
//!   kernel runs five-level paging (`la57_enabled`: `CR4` is not readable
//!   from user space, and `cpuid` only says whether the CPU could).
//! - Both: `/sys/devices/system/cpu/cpu0/cache/index*/{level,type,size}`
//!   with raw syscalls into fixed buffers as the fallback for the cache
//!   sizes; an absent or empty file (Docker Desktop's VM has the directory
//!   but no `size`) reads as `unknown`.
//!
//! The decoders at the top are pure and unit tested on the host against
//! captured register and file contents; the probes run in the container.

use core::fmt;

/// One `cpuid` result (x86_64), or a captured one in a test fixture.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Regs {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

// arm64 `AT_HWCAP` / `AT_HWCAP2` bits, from `arch/arm64/include/uapi/asm/
// hwcap.h`; the container test cross-checks them against the installed
// header where one exists.
pub const HWCAP_ATOMICS: u64 = 1 << 8;
pub const HWCAP_SVE: u64 = 1 << 22;
pub const HWCAP_USCAT: u64 = 1 << 25;
pub const HWCAP_PACA: u64 = 1 << 30;
pub const HWCAP_PACG: u64 = 1 << 31;
pub const HWCAP2_BTI: u64 = 1 << 17;
pub const HWCAP2_MTE: u64 = 1 << 18;
pub const HWCAP2_MTE3: u64 = 1 << 22;

/// arm64 features the allocator may later use, decoded from the HWCAP words.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Arm {
    /// `HWCAP2_MTE`: memory tagging (tagged pointers, a later task).
    pub mte: bool,
    /// `HWCAP2_MTE3`: asymmetric tag-check faulting (MTE with `FEAT_MTE3`).
    pub mte3: bool,
    /// `HWCAP_PACA` and `HWCAP_PACG` both: pointer authentication (signed
    /// cache pointers, a later task).
    pub pac: bool,
    /// `HWCAP_ATOMICS`: LSE atomics (`ldadd`, `cas`, ...).
    pub lse: bool,
    /// `HWCAP_USCAT`: LSE2, single-copy atomic 16-byte loads and stores
    /// (128-bit descriptor atomics, a later task).
    pub lse2: bool,
    /// `HWCAP2_BTI`: branch target identification.
    pub bti: bool,
    /// `HWCAP_SVE`: scalable vector extension.
    pub sve: bool,
}

impl Arm {
    /// Decode the two HWCAP words.
    pub fn from_hwcap(hwcap: u64, hwcap2: u64) -> Arm {
        Arm {
            mte: hwcap2 & HWCAP2_MTE != 0,
            mte3: hwcap2 & HWCAP2_MTE3 != 0,
            pac: hwcap & HWCAP_PACA != 0 && hwcap & HWCAP_PACG != 0,
            lse: hwcap & HWCAP_ATOMICS != 0,
            lse2: hwcap & HWCAP_USCAT != 0,
            bti: hwcap2 & HWCAP2_BTI != 0,
            sve: hwcap & HWCAP_SVE != 0,
        }
    }
}

/// x86_64 features the allocator may later use, decoded from `cpuid`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct X86 {
    /// Leaf 7 subleaf 1 `EAX[26]`: linear address masking (tagged pointers,
    /// a later task).
    pub lam: bool,
    /// Leaf `0x80000021` `EAX[7]`: AMD's upper address ignore (Zen 4 and
    /// later; the LAM analogue, same later task). Only read when leaf
    /// `0x80000000` reports an extended range that reaches `0x80000021`.
    pub uai: bool,
    /// Leaf 7 subleaf 0 `ECX[16]`: the CPU supports five-level paging.
    pub la57: bool,
    /// The kernel runs five-level paging: a hinted `mmap` above 2^47 was
    /// honoured. Set by the probe, never by the decoder.
    pub la57_enabled: bool,
    /// Leaf 7 subleaf 0 `ECX[25]`: `cldemote` (cache-line demotion).
    pub cldemote: bool,
    /// Leaf 7 subleaf 0 `ECX[5]`: `umonitor`/`umwait`/`tpause`.
    pub waitpkg: bool,
    /// Leaf 7 subleaf 0 `EBX[9]`: enhanced `rep movsb`/`stosb`.
    pub erms: bool,
    /// Leaf 7 subleaf 0 `EDX[4]`: fast short `rep movsb`.
    pub fsrm: bool,
    /// Leaf 7 subleaf 0 `EBX[16]`: AVX-512 foundation.
    pub avx512f: bool,
}

impl X86 {
    /// Decode the feature bits. `max_leaf` is leaf 0 `EAX` (leaf 7 is only
    /// valid when it is at least 7); `leaf7_1` is consulted only when leaf 7
    /// subleaf 0 reports at least one subleaf in `EAX`; `ext21` is leaf
    /// `0x80000021`, which the probe passes as `Regs::default()` when the
    /// extended range does not reach it (Intel's ends at `0x80000008`).
    /// `la57_enabled` is left `false` for the probe to set.
    pub fn from_cpuid(max_leaf: u32, leaf7: Regs, leaf7_1: Regs, ext21: Regs) -> X86 {
        if max_leaf < 7 {
            return X86::default();
        }
        let leaf7_1 = if leaf7.eax >= 1 { leaf7_1 } else { Regs::default() };
        X86 {
            lam: leaf7_1.eax & (1 << 26) != 0,
            uai: ext21.eax & (1 << 7) != 0,
            la57: leaf7.ecx & (1 << 16) != 0,
            la57_enabled: false,
            cldemote: leaf7.ecx & (1 << 25) != 0,
            waitpkg: leaf7.ecx & (1 << 5) != 0,
            erms: leaf7.ebx & (1 << 9) != 0,
            fsrm: leaf7.edx & (1 << 4) != 0,
            avx512f: leaf7.ebx & (1 << 16) != 0,
        }
    }
}

/// The instruction set the report describes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Isa {
    Arm(Arm),
    X86(X86),
}

/// Cache geometry in bytes; `0` means unknown.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Caches {
    /// L1 data cache line.
    pub line: u32,
    /// L1 data cache size.
    pub l1d: u32,
    /// L2 (unified, or data if the core splits it) size.
    pub l2: u32,
}

impl Caches {
    pub const UNKNOWN: Caches = Caches { line: 0, l1d: 0, l2: 0 };

    /// Fill this value's unknown fields from `other`.
    pub fn or(self, other: Caches) -> Caches {
        Caches {
            line: if self.line != 0 { self.line } else { other.line },
            l1d: if self.l1d != 0 { self.l1d } else { other.l1d },
            l2: if self.l2 != 0 { self.l2 } else { other.l2 },
        }
    }

    /// Every field known.
    pub fn complete(self) -> bool {
        self.line != 0 && self.l1d != 0 && self.l2 != 0
    }
}

/// Where the cache figures came from (for the results files and tests; not
/// part of the line).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CacheSource {
    /// Every figure from `cpuid` (leaf 4 or `0x8000001D`).
    Cpuid,
    /// At least one figure from sysfs.
    Sysfs,
    /// Nothing but the cache line (or nothing at all).
    None,
}

/// The CPU part of the feature report.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub isa: Isa,
    pub caches: Caches,
    pub cache_source: CacheSource,
}

/// Decode deterministic cache parameter subleaves (Intel leaf 4, AMD leaf
/// `0x8000001D`; the same layout), given in subleaf order and stopping at
/// the first null entry. The L1 data cache gives the line size; the L2 is
/// taken from a unified or data cache at level 2.
pub fn caches_from_cpuid(subleaves: &[Regs]) -> Caches {
    let mut c = Caches::UNKNOWN;
    for r in subleaves {
        let ty = r.eax & 0x1f;
        if ty == 0 {
            break;
        }
        let level = (r.eax >> 5) & 7;
        let line = (r.ebx & 0xfff) + 1;
        let partitions = ((r.ebx >> 12) & 0x3ff) + 1;
        let ways = (r.ebx >> 22) + 1;
        let sets = r.ecx.wrapping_add(1);
        let size = u64::from(ways) * u64::from(partitions) * u64::from(line) * u64::from(sets);
        let size = u32::try_from(size).unwrap_or(0);
        match (level, ty) {
            (1, 1) => {
                c.l1d = size;
                c.line = line;
            }
            (2, 1 | 3) if c.l2 == 0 => c.l2 = size,
            _ => {}
        }
    }
    c
}

/// Parse a decimal sysfs value (`level`, `coherency_line_size`), tolerating
/// a trailing newline. `None` for an empty or malformed file.
pub fn parse_sysfs_dec(b: &[u8]) -> Option<u32> {
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || !b[digits..].iter().all(|c| *c == b'\n') {
        return None;
    }
    b[..digits].iter().try_fold(0u32, |acc, c| acc.checked_mul(10)?.checked_add(u32::from(c - b'0')))
}

/// Parse a sysfs `size` file: decimal bytes with an optional `K` or `M`
/// suffix and a trailing newline (the kernel prints `64K`, `2048K`).
pub fn parse_sysfs_size(b: &[u8]) -> Option<u32> {
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let n = parse_sysfs_dec(&b[..digits])?;
    let rest = b[digits..].strip_suffix(b"\n").unwrap_or(&b[digits..]);
    match rest {
        b"" => Some(n),
        b"K" => n.checked_mul(1024),
        b"M" => n.checked_mul(1024 * 1024),
        _ => None,
    }
}

/// One sysfs cache index directory as read: the contents of `level`,
/// `type`, `size` and `coherency_line_size` (empty when the file is absent
/// or empty).
#[derive(Copy, Clone, Debug, Default)]
pub struct SysfsCache<'a> {
    pub level: &'a [u8],
    pub kind: &'a [u8],
    pub size: &'a [u8],
    pub line: &'a [u8],
}

/// Decode the sysfs cache index directories: the L1 `Data` cache and the
/// first level-2 `Unified` (or `Data`) cache.
pub fn caches_from_sysfs(entries: &[SysfsCache<'_>]) -> Caches {
    fn kind_of(b: &[u8]) -> &[u8] {
        b.strip_suffix(b"\n").unwrap_or(b)
    }
    let mut c = Caches::UNKNOWN;
    for e in entries {
        let Some(level) = parse_sysfs_dec(e.level) else { continue };
        let size = parse_sysfs_size(e.size).unwrap_or(0);
        match (level, kind_of(e.kind)) {
            (1, b"Data") => {
                c.l1d = size;
                c.line = parse_sysfs_dec(e.line).unwrap_or(0);
            }
            (2, b"Unified" | b"Data") if c.l2 == 0 => c.l2 = size,
            _ => {}
        }
    }
    c
}

/// Cache-line size from `CTR_EL0`: `DminLine` (bits 19:16) is log2 of the
/// number of 4-byte words in the smallest data-cache line.
pub fn cacheline_from_ctr(ctr: u64) -> u32 {
    4u32 << ((ctr >> 16) & 0xf)
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// A size in bytes as `<n>M`, `<n>K` or `<n>`, or `unknown` for 0.
struct Size(u32);

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => f.write_str("unknown"),
            n if n % (1 << 20) == 0 => write!(f, "{}M", n >> 20),
            n if n % (1 << 10) == 0 => write!(f, "{}K", n >> 10),
            n => write!(f, "{n}"),
        }
    }
}

impl fmt::Display for Cpu {
    /// The CPU half of the feature line: `key=value` pairs separated by
    /// spaces, `yes`/`no` for the bits, sizes as `64K`/`2M`, `unknown`
    /// where a source was absent.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.isa {
            Isa::Arm(a) => write!(
                f,
                "mte={} mte3={} pac={} lse={} lse2={} bti={} sve={}",
                yn(a.mte),
                yn(a.mte3),
                yn(a.pac),
                yn(a.lse),
                yn(a.lse2),
                yn(a.bti),
                yn(a.sve),
            )?,
            Isa::X86(x) => write!(
                f,
                "lam={} uai={} la57={} la57_enabled={} cldemote={} waitpkg={} erms={} fsrm={} avx512f={}",
                yn(x.lam),
                yn(x.uai),
                yn(x.la57),
                yn(x.la57_enabled),
                yn(x.cldemote),
                yn(x.waitpkg),
                yn(x.erms),
                yn(x.fsrm),
                yn(x.avx512f),
            )?,
        }
        write!(
            f,
            " cacheline={} l1d={} l2={}",
            Size(self.caches.line),
            Size(self.caches.l1d),
            Size(self.caches.l2)
        )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{CacheSource, Caches, Cpu, SysfsCache, caches_from_sysfs};
    use crate::sys;

    impl Cpu {
        /// Probe the CPU: the architecture's registers first, then sysfs for
        /// whatever cache figure they did not give. Never allocates; at most
        /// six small file reads (about 35 us on the C4A, `probe_cost_is_small`)
        /// and, on x86_64 with `la57`, one `mmap`/`munmap` pair of
        /// `page_size` bytes.
        pub fn probe(page_size: usize) -> Cpu {
            let (isa, from_regs) = arch::probe(page_size);
            let mut caches = from_regs;
            let mut source = if caches.l1d != 0 && caches.l2 != 0 { CacheSource::Cpuid } else { CacheSource::None };
            if !caches.complete() {
                let s = sysfs_caches(caches.line != 0);
                caches = caches.or(s);
                if s.l1d != 0 || s.l2 != 0 {
                    source = CacheSource::Sysfs;
                }
            }
            Cpu { isa, caches, cache_source: source }
        }
    }

    /// Read a small sysfs file into `buf`; the empty slice when the file is
    /// absent or unreadable.
    fn read_small<'a>(path: &[u8], buf: &'a mut [u8]) -> &'a [u8] {
        let Ok(fd) = sys::open(path, sys::O_RDONLY) else { return &[] };
        let n = sys::read_full(fd, buf).unwrap_or(0);
        let _ = sys::close(fd);
        &buf[..n]
    }

    const CACHE_DIR: &[u8] = b"/sys/devices/system/cpu/cpu0/cache/index";

    /// `/sys/devices/system/cpu/cpu0/cache/index<i>/<file>\0` into `out`.
    fn cache_path<'a>(out: &'a mut [u8; 96], index: u8, file: &[u8]) -> &'a [u8] {
        let mut n = 0;
        for &b in CACHE_DIR.iter().chain([b'0' + index, b'/'].iter()).chain(file).chain(b"\0") {
            out[n] = b;
            n += 1;
        }
        &out[..n]
    }

    /// The sysfs cache directories, reading as few files as the answer
    /// needs (each open/read/close of a sysfs file costs about 6 us in a
    /// VM, and the whole probe has a 50 us budget): `index0` is the first
    /// level-1 leaf by construction (cacheinfo fills the leaves level by
    /// level, data before instruction; the container test checks
    /// `index0/level` reads 1 on every host), so its `type` and `size` give
    /// the L1D; the L2 is `index1` (unified L1) or `index2` (split L1),
    /// told apart by their `level` files. `coherency_line_size` is read only
    /// when the registers did not give the line.
    fn sysfs_caches(line_known: bool) -> Caches {
        let mut path = [0u8; 96];
        let (mut kind0, mut size0, mut line0) = ([0u8; 16], [0u8; 16], [0u8; 8]);
        let k0 = read_small(cache_path(&mut path, 0, b"type"), &mut kind0);
        if k0.is_empty() {
            return Caches::UNKNOWN;
        }
        let is_l1d = k0.starts_with(b"Data") || k0.starts_with(b"Unified");
        let s0 = if is_l1d { read_small(cache_path(&mut path, 0, b"size"), &mut size0) } else { &[][..] };
        let l0 = if is_l1d && !line_known {
            read_small(cache_path(&mut path, 0, b"coherency_line_size"), &mut line0)
        } else {
            &[][..]
        };
        let mut entries = [SysfsCache { level: b"1", kind: k0, size: s0, line: l0 }, SysfsCache::default()];
        let (mut lvl, mut kind, mut size) = ([0u8; 8], [0u8; 16], [0u8; 16]);
        let mut n = 1;
        for i in 1..3u8 {
            let lv = read_small(cache_path(&mut path, i, b"level"), &mut lvl);
            if lv.is_empty() {
                break;
            }
            if lv[0] == b'2' {
                let kd_n = read_small(cache_path(&mut path, i, b"type"), &mut kind).len();
                let sz_n = read_small(cache_path(&mut path, i, b"size"), &mut size).len();
                entries[1] = SysfsCache { level: b"2", kind: &kind[..kd_n], size: &size[..sz_n], line: &[] };
                n = 2;
                break;
            }
        }
        caches_from_sysfs(&entries[..n])
    }

    #[cfg(target_arch = "aarch64")]
    mod arch {
        use super::super::{Arm, Caches, Isa, cacheline_from_ctr};

        const AT_HWCAP: usize = 16;
        const AT_HWCAP2: usize = 26;

        unsafe extern "C" {
            /// glibc/musl `getauxval(3)`; reads the saved vector, never
            /// allocates (see `features.rs`).
            safe fn getauxval(kind: usize) -> usize;
        }

        /// `CTR_EL0`, readable from EL0 (Linux sets `SCTLR_EL1.UCT`, and
        /// traps and emulates the read where the hardware would not allow it).
        fn ctr_el0() -> u64 {
            let v: u64;
            // SAFETY: a system-register read with no memory or flag effects.
            unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
            v
        }

        pub fn probe(_page_size: usize) -> (Isa, Caches) {
            let arm = Arm::from_hwcap(getauxval(AT_HWCAP) as u64, getauxval(AT_HWCAP2) as u64);
            let caches = Caches { line: cacheline_from_ctr(ctr_el0()), ..Caches::UNKNOWN };
            (Isa::Arm(arm), caches)
        }
    }

    #[cfg(target_arch = "x86_64")]
    mod arch {
        use super::super::{Caches, Isa, Regs, X86, caches_from_cpuid};
        use crate::sys;

        /// One `cpuid`. `rbx` is reserved by the compiler on x86_64, so it
        /// is swapped through a scratch register.
        pub fn cpuid(leaf: u32, subleaf: u32) -> Regs {
            let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
            // SAFETY: `cpuid` reads and writes only the four registers named
            // here; `rbx` is saved and restored around it.
            unsafe {
                core::arch::asm!(
                    "mov {tmp:r}, rbx",
                    "cpuid",
                    "xchg {tmp:r}, rbx",
                    tmp = out(reg) ebx,
                    inout("eax") leaf => eax,
                    inout("ecx") subleaf => ecx,
                    out("edx") edx,
                    options(nostack, nomem, preserves_flags),
                );
            }
            Regs { eax, ebx, ecx, edx }
        }

        /// Whether the kernel runs five-level paging: a hint above the
        /// 47-bit limit is honoured only then; a four-level kernel ignores
        /// it and places the mapping below 2^47. One `PROT_NONE` page,
        /// mapped and unmapped at once.
        fn la57_enabled(page_size: usize) -> bool {
            let hint = 1usize << 56;
            // SAFETY: a fresh private anonymous reservation with a hint,
            // released before returning.
            match unsafe {
                sys::mmap(
                    hint,
                    page_size,
                    sys::PROT_NONE,
                    sys::MAP_PRIVATE | sys::MAP_ANONYMOUS | sys::MAP_NORESERVE,
                    -1,
                    0,
                )
            } {
                Ok(addr) => {
                    // SAFETY: the page we just mapped.
                    let _ = unsafe { sys::munmap(addr, page_size) };
                    addr >= 1 << 47
                }
                Err(_) => false,
            }
        }

        /// The deterministic cache parameters: leaf 4 (Intel; AMD returns a
        /// null entry there) then `0x8000001D` (AMD, when the extended range
        /// reaches it). Up to eight subleaves each, stopping at the first
        /// null type.
        fn cpuid_caches(max_leaf: u32) -> Caches {
            fn read(leaf: u32, out: &mut [Regs; 8]) -> usize {
                let mut n = 0;
                while n < out.len() {
                    out[n] = cpuid(leaf, n as u32);
                    if out[n].eax & 0x1f == 0 {
                        break;
                    }
                    n += 1;
                }
                n
            }
            let mut regs = [Regs::default(); 8];
            if max_leaf >= 4 {
                let n = read(4, &mut regs);
                let c = caches_from_cpuid(&regs[..n]);
                if c.l1d != 0 {
                    return c;
                }
            }
            if cpuid(0x8000_0000, 0).eax >= 0x8000_001D {
                let n = read(0x8000_001D, &mut regs);
                return caches_from_cpuid(&regs[..n]);
            }
            Caches::UNKNOWN
        }

        pub fn probe(page_size: usize) -> (Isa, Caches) {
            let max_leaf = cpuid(0, 0).eax;
            let leaf7 = if max_leaf >= 7 { cpuid(7, 0) } else { Regs::default() };
            let leaf7_1 = if max_leaf >= 7 && leaf7.eax >= 1 { cpuid(7, 1) } else { Regs::default() };
            // The extended range: `0x80000021` (AMD's extended feature
            // identification 2, UAI in `EAX[7]`) exists on Zen 4 and later;
            // Intel's range ends at `0x80000008` and the leaf is not read.
            let ext21 = if cpuid(0x8000_0000, 0).eax >= 0x8000_0021 { cpuid(0x8000_0021, 0) } else { Regs::default() };
            let mut x86 = X86::from_cpuid(max_leaf, leaf7, leaf7_1, ext21);
            if x86.la57 {
                x86.la57_enabled = la57_enabled(page_size);
            }
            let mut caches = cpuid_caches(max_leaf);
            if caches.line == 0 && max_leaf >= 1 {
                // Leaf 1 `EBX[15:8]`: the `clflush` line size in 8-byte units.
                caches.line = ((cpuid(1, 0).ebx >> 8) & 0xff) * 8;
            }
            (Isa::X86(x86), caches)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on the reference VMs (2026-09-19, `LD_SHOW_AUXV=1 /bin/true`
    // and a `cpuid` dump inside the dev container).
    const AXION_HWCAP: (u64, u64) = (0xefff_ffff, 0x3f3ff); // C4A, Neoverse V2, kernel 7.0
    const MAC_VM_HWCAP: (u64, u64) = (0xefb3_ffff, 0x126181); // Apple silicon under Docker Desktop, 6.10
    const XEON_8581C_MAX_LEAF: u32 = 0x20;
    const XEON_8581C_LEAF7: Regs = Regs { eax: 0x0000_0002, ebx: 0xf1bf_2ffb, ecx: 0x1a41_5f46, edx: 0xafc1_4410 };
    const XEON_8581C_LEAF7_1: Regs = Regs { eax: 0x0000_1c30, ebx: 0, ecx: 0, edx: 0 };
    // Intel's extended range ends at 0x80000008: the probe passes zeros.
    const XEON_8581C_EXT21: Regs = Regs { eax: 0, ebx: 0, ecx: 0, edx: 0 };
    const XEON_8581C_LEAF4: [Regs; 5] = [
        Regs { eax: 0x1c00_4121, ebx: 0x02c0_003f, ecx: 0x0000_003f, edx: 0 },
        Regs { eax: 0x1c00_4122, ebx: 0x01c0_003f, ecx: 0x0000_003f, edx: 0 },
        Regs { eax: 0x1c00_4143, ebx: 0x03c0_003f, ecx: 0x0000_07ff, edx: 0 },
        Regs { eax: 0x1c03_c163, ebx: 0x04c0_003f, ecx: 0x0003_3fff, edx: 4 },
        Regs { eax: 0, ebx: 0, ecx: 0, edx: 0 },
    ];

    // Captured by the CPU matrix (Phase 2.5 task 4, 2026-09-19,
    // `bench/gce/cpuid-dump.c` on the cells, `bench/results/matrix/
    // 2026-09-19-cpu-*.log`): a GCE T2A (Ampere Altra, Neoverse N1, MIDR
    // 0x413fd0c1: `atomics` and nothing else the report looks at), a GCE
    // C3D (AMD EPYC 9B14, Genoa, Zen 4: the hypervisor hides the LA57 bit
    // and UAI, leaf 0x80000021 `EAX = 0x10000067` has bit 7 clear) and a
    // GCE N2D (AMD EPYC 7B13, Milan, Zen 3: no leaf 7 subleaf 1, no
    // AVX-512).
    const ALTRA_HWCAP: (u64, u64) = (0x119fff, 0);
    const GENOA_9B14_MAX_LEAF: u32 = 0x10;
    const GENOA_9B14_LEAF7: Regs = Regs { eax: 0x0000_0001, ebx: 0xf1bf_07ab, ecx: 0x0040_5f46, edx: 0x0000_0010 };
    const GENOA_9B14_LEAF7_1: Regs = Regs { eax: 0x0000_0020, ebx: 0, ecx: 0, edx: 0 };
    const GENOA_9B14_EXT21: Regs = Regs { eax: 0x1000_0067, ebx: 0, ecx: 0, edx: 0 };
    const GENOA_9B14_LEAF_8000001D: [Regs; 5] = [
        Regs { eax: 0x0000_4121, ebx: 0x01c0_003f, ecx: 0x0000_003f, edx: 0 },
        Regs { eax: 0x0000_4122, ebx: 0x01c0_003f, ecx: 0x0000_003f, edx: 0 },
        Regs { eax: 0x0000_4143, ebx: 0x01c0_003f, ecx: 0x0000_07ff, edx: 2 },
        Regs { eax: 0x0001_c163, ebx: 0x03c0_003f, ecx: 0x0000_7fff, edx: 1 },
        Regs { eax: 0, ebx: 0, ecx: 0, edx: 0 },
    ];
    const MILAN_7B13_MAX_LEAF: u32 = 0xd;
    const MILAN_7B13_LEAF7: Regs = Regs { eax: 0, ebx: 0x219c_07ab, ecx: 0x0040_0604, edx: 0x0000_0010 };
    const MILAN_7B13_EXT21: Regs = Regs { eax: 0x1000_0020, ebx: 0, ecx: 0, edx: 0 };
    // What a Zen 4 with UAI exposed would say in leaf 0x80000021: no GCE
    // guest showed it, so this one stays composed (the bit alone).
    const ZEN4_EXT21_WITH_UAI: Regs = Regs { eax: 0x1000_0067 | 1 << 7, ebx: 0, ecx: 0, edx: 0 };

    fn cpu(isa: Isa, caches: Caches) -> Cpu {
        Cpu { isa, caches, cache_source: CacheSource::None }
    }

    #[test]
    fn axion_hwcap_decodes_without_mte() {
        let a = Arm::from_hwcap(AXION_HWCAP.0, AXION_HWCAP.1);
        assert_eq!(a, Arm { mte: false, mte3: false, pac: true, lse: true, lse2: true, bti: true, sve: true });
    }

    #[test]
    fn apple_silicon_vm_hwcap_decodes() {
        let a = Arm::from_hwcap(MAC_VM_HWCAP.0, MAC_VM_HWCAP.1);
        assert_eq!(a, Arm { mte: false, mte3: false, pac: true, lse: true, lse2: true, bti: true, sve: false });
    }

    #[test]
    fn altra_hwcap_decodes_to_lse_only() {
        let a = Arm::from_hwcap(ALTRA_HWCAP.0, ALTRA_HWCAP.1);
        assert_eq!(a, Arm { lse: true, ..Arm::default() });
    }

    #[test]
    fn mte_bits_are_seen_when_present() {
        let a = Arm::from_hwcap(AXION_HWCAP.0, AXION_HWCAP.1 | HWCAP2_MTE | HWCAP2_MTE3);
        assert!(a.mte && a.mte3);
        // PAC needs both halves.
        let a = Arm::from_hwcap(HWCAP_PACA, 0);
        assert!(!a.pac);
    }

    #[test]
    fn xeon_8581c_cpuid_decodes() {
        let x = X86::from_cpuid(XEON_8581C_MAX_LEAF, XEON_8581C_LEAF7, XEON_8581C_LEAF7_1, XEON_8581C_EXT21);
        assert_eq!(
            x,
            X86 {
                lam: false,
                uai: false,
                la57: true,
                la57_enabled: false,
                cldemote: true,
                waitpkg: false,
                erms: true,
                fsrm: true,
                avx512f: true,
            }
        );
    }

    #[test]
    fn genoa_9b14_cpuid_decodes_without_la57_or_uai() {
        // The GCE guest: LA57 and UAI hidden, AVX-512 and FSRM shown.
        let x = X86::from_cpuid(GENOA_9B14_MAX_LEAF, GENOA_9B14_LEAF7, GENOA_9B14_LEAF7_1, GENOA_9B14_EXT21);
        assert_eq!(x, X86 { erms: true, fsrm: true, avx512f: true, ..X86::default() });
        // The same CPU with the UAI bit exposed.
        let x = X86::from_cpuid(GENOA_9B14_MAX_LEAF, GENOA_9B14_LEAF7, GENOA_9B14_LEAF7_1, ZEN4_EXT21_WITH_UAI);
        assert_eq!(x, X86 { uai: true, erms: true, fsrm: true, avx512f: true, ..X86::default() });
    }

    #[test]
    fn milan_7b13_cpuid_decodes() {
        // Leaf 7 reports no subleaf 1, so whatever is passed for it is ignored.
        let x = X86::from_cpuid(MILAN_7B13_MAX_LEAF, MILAN_7B13_LEAF7, Regs { eax: 1 << 26, ..Regs::default() }, MILAN_7B13_EXT21);
        assert_eq!(x, X86 { erms: true, fsrm: true, ..X86::default() });
    }

    #[test]
    fn genoa_9b14_leaf_8000001d_gives_32k_l1d_and_1m_l2() {
        let c = caches_from_cpuid(&GENOA_9B14_LEAF_8000001D);
        assert_eq!(c, Caches { line: 64, l1d: 32 * 1024, l2: 1024 * 1024 });
    }

    #[test]
    fn uai_is_leaf_0x80000021_bit_7_alone() {
        // The bit, and nothing else in the leaf, sets `uai`.
        let l7 = Regs { eax: 1, ..Regs::default() };
        assert!(X86::from_cpuid(7, l7, Regs::default(), Regs { eax: 1 << 7, ..Regs::default() }).uai);
        assert!(!X86::from_cpuid(7, l7, Regs::default(), Regs { eax: !(1 << 7), ..Regs::default() }).uai);
        // The probe passes zeros where the extended range stops short
        // (Intel): no `uai`.
        assert!(!X86::from_cpuid(XEON_8581C_MAX_LEAF, XEON_8581C_LEAF7, XEON_8581C_LEAF7_1, Regs::default()).uai);
        // No leaf 7 at all: nothing, whatever the extended leaf says.
        assert_eq!(X86::from_cpuid(6, l7, Regs::default(), ZEN4_EXT21_WITH_UAI), X86::default());
    }

    #[test]
    fn lam_needs_subleaf_1_and_leaf_7() {
        let lam = Regs { eax: 1 << 26, ..Regs::default() };
        let l7 = Regs { eax: 1, ..Regs::default() };
        assert!(X86::from_cpuid(7, l7, lam, Regs::default()).lam);
        // Subleaf 1 not reported by subleaf 0: ignored.
        assert!(!X86::from_cpuid(7, Regs::default(), lam, Regs::default()).lam);
        // No leaf 7 at all: nothing.
        assert_eq!(X86::from_cpuid(6, l7, lam, Regs::default()), X86::default());
    }

    #[test]
    fn xeon_8581c_leaf4_gives_48k_l1d_and_2m_l2() {
        let c = caches_from_cpuid(&XEON_8581C_LEAF4);
        assert_eq!(c, Caches { line: 64, l1d: 48 * 1024, l2: 2 * 1024 * 1024 });
        // The L3 entry after the L2 is ignored, and a null entry stops the walk.
        assert_eq!(caches_from_cpuid(&XEON_8581C_LEAF4[4..]), Caches::UNKNOWN);
        assert_eq!(caches_from_cpuid(&[]), Caches::UNKNOWN);
    }

    #[test]
    fn sysfs_sizes_parse() {
        assert_eq!(parse_sysfs_size(b"64K\n"), Some(64 * 1024));
        assert_eq!(parse_sysfs_size(b"2048K\n"), Some(2 * 1024 * 1024));
        assert_eq!(parse_sysfs_size(b"2M"), Some(2 * 1024 * 1024));
        assert_eq!(parse_sysfs_size(b"81920K\n"), Some(80 * 1024 * 1024));
        assert_eq!(parse_sysfs_size(b"512"), Some(512));
        assert_eq!(parse_sysfs_size(b""), None);
        assert_eq!(parse_sysfs_size(b"\n"), None);
        assert_eq!(parse_sysfs_size(b"64G\n"), None);
        assert_eq!(parse_sysfs_size(b"K"), None);
        assert_eq!(parse_sysfs_dec(b"1\n"), Some(1));
        assert_eq!(parse_sysfs_dec(b"64\n"), Some(64));
        assert_eq!(parse_sysfs_dec(b"x"), None);
        assert_eq!(parse_sysfs_dec(b"99999999999"), None);
    }

    #[test]
    fn sysfs_cache_directories_decode() {
        // C4A as captured.
        let axion = [
            SysfsCache { level: b"1\n", kind: b"Data\n", size: b"64K\n", line: b"64\n" },
            SysfsCache { level: b"1\n", kind: b"Instruction\n", size: b"64K\n", line: b"64\n" },
            SysfsCache { level: b"2\n", kind: b"Unified\n", size: b"2048K\n", line: b"64\n" },
            SysfsCache { level: b"3\n", kind: b"Unified\n", size: b"81920K\n", line: b"64\n" },
        ];
        assert_eq!(caches_from_sysfs(&axion), Caches { line: 64, l1d: 64 * 1024, l2: 2 * 1024 * 1024 });
        // Docker Desktop's VM: the directories exist, the size files are empty.
        let mac = [
            SysfsCache { level: b"1\n", kind: b"Data\n", size: b"", line: b"" },
            SysfsCache { level: b"1\n", kind: b"Instruction\n", size: b"", line: b"" },
            SysfsCache { level: b"2\n", kind: b"Unified\n", size: b"", line: b"" },
        ];
        assert_eq!(caches_from_sysfs(&mac), Caches::UNKNOWN);
        assert_eq!(caches_from_sysfs(&[]), Caches::UNKNOWN);
    }

    #[test]
    fn ctr_el0_gives_the_line_size() {
        assert_eq!(cacheline_from_ctr(0xb444c004), 64); // Axion
        assert_eq!(cacheline_from_ctr(0x9444c004), 64); // Apple silicon VM
        assert_eq!(cacheline_from_ctr(0x0005_0000), 128); // DminLine = 5
    }

    #[test]
    fn caches_fill_from_the_fallback() {
        let regs = Caches { line: 64, ..Caches::UNKNOWN };
        let sysfs = Caches { line: 128, l1d: 1, l2: 2 };
        assert_eq!(regs.or(sysfs), Caches { line: 64, l1d: 1, l2: 2 });
        assert!(regs.or(sysfs).complete() && !regs.complete() && !Caches::UNKNOWN.complete());
    }

    #[test]
    fn display_arm_line() {
        let c = cpu(
            Isa::Arm(Arm::from_hwcap(AXION_HWCAP.0, AXION_HWCAP.1)),
            Caches { line: 64, l1d: 64 * 1024, l2: 2 * 1024 * 1024 },
        );
        assert_eq!(
            std::format!("{c}"),
            "mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M"
        );
        let c = cpu(Isa::Arm(Arm::default()), Caches { line: 64, ..Caches::UNKNOWN });
        assert_eq!(
            std::format!("{c}"),
            "mte=no mte3=no pac=no lse=no lse2=no bti=no sve=no cacheline=64 l1d=unknown l2=unknown"
        );
    }

    #[test]
    fn display_x86_line() {
        let mut x = X86::from_cpuid(XEON_8581C_MAX_LEAF, XEON_8581C_LEAF7, XEON_8581C_LEAF7_1, XEON_8581C_EXT21);
        x.la57_enabled = true;
        let c = cpu(Isa::X86(x), caches_from_cpuid(&XEON_8581C_LEAF4));
        assert_eq!(
            std::format!("{c}"),
            "lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M"
        );
        // Odd sizes print in bytes.
        let c = cpu(Isa::X86(X86::default()), Caches { line: 64, l1d: 3000, l2: 1536 * 1024 });
        assert!(std::format!("{c}").ends_with(" cacheline=64 l1d=3000 l2=1536K"));
    }
}

/// Linux-only: the real probe against `/proc/cpuinfo` and the kernel header.
#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    /// The flag words of the first processor in `/proc/cpuinfo`
    /// (`Features` on arm64, `flags` on x86).
    fn cpuinfo_flags() -> Vec<String> {
        let text = std::fs::read_to_string("/proc/cpuinfo").unwrap();
        let key = if cfg!(target_arch = "aarch64") { "Features" } else { "flags" };
        let line = text
            .lines()
            .find(|l| l.starts_with(key) && l.contains(':'))
            .unwrap_or_else(|| panic!("no `{key}` line in /proc/cpuinfo:\n{text}"));
        line.split_once(':').unwrap().1.split_whitespace().map(String::from).collect()
    }

    /// `(key, value, cpuinfo flags that must all be present for `yes`)`; an
    /// empty flag list means the kernel shows no flag for the bit (`uai`:
    /// Linux has no `X86_FEATURE` name for it) and only the key's presence
    /// in the line is checked.
    fn bits(c: &Cpu) -> Vec<(&'static str, bool, &'static [&'static str])> {
        match c.isa {
            Isa::Arm(a) => std::vec![
                ("mte", a.mte, &["mte"][..]),
                ("mte3", a.mte3, &["mte3"]),
                ("pac", a.pac, &["paca", "pacg"]),
                ("lse", a.lse, &["atomics"]),
                ("lse2", a.lse2, &["uscat"]),
                ("bti", a.bti, &["bti"]),
                ("sve", a.sve, &["sve"]),
            ],
            Isa::X86(x) => std::vec![
                ("lam", x.lam, &["lam"][..]),
                ("uai", x.uai, &[]),
                ("la57", x.la57, &["la57"]),
                ("cldemote", x.cldemote, &["cldemote"]),
                ("waitpkg", x.waitpkg, &["waitpkg"]),
                ("erms", x.erms, &["erms"]),
                ("fsrm", x.fsrm, &["fsrm"]),
                ("avx512f", x.avx512f, &["avx512f"]),
            ],
        }
    }

    #[test]
    fn probe_matches_proc_cpuinfo() {
        let page = crate::sys::features::page_size();
        let c = Cpu::probe(page);
        let line = std::format!("{c}");
        std::eprintln!("cpu: {line} (cache source {:?})", c.cache_source);
        let flags = cpuinfo_flags();
        for (key, value, needed) in bits(&c) {
            if !needed.is_empty() {
                let expect = needed.iter().all(|f| flags.iter().any(|g| g == f));
                assert_eq!(value, expect, "{key} disagrees with /proc/cpuinfo {needed:?} in: {line}");
            } else {
                std::eprintln!("{key}={} has no /proc/cpuinfo flag to check against", if value { "yes" } else { "no" });
            }
            assert!(line.contains(&std::format!(" {key}=")) || line.starts_with(&std::format!("{key}=")), "{line}");
        }
        // The architecture we were built for is the one reported.
        assert_eq!(matches!(c.isa, Isa::Arm(_)), cfg!(target_arch = "aarch64"));
        // Every CPU we run on has a 32..256-byte line and, when known, a
        // plausible L1D and L2.
        assert!(matches!(c.caches.line, 32 | 64 | 128 | 256), "{line}");
        if c.caches.l1d != 0 {
            assert!((8 * 1024..=256 * 1024).contains(&c.caches.l1d), "{line}");
        }
        if c.caches.l2 != 0 {
            assert!((128 * 1024..=64 * 1024 * 1024).contains(&c.caches.l2), "{line}");
        }
        // The reader's assumption about `index0`: the first leaf is level 1
        // and holds data (split or unified).
        let sysfs = "/sys/devices/system/cpu/cpu0/cache/index0";
        if let Ok(l) = std::fs::read(std::format!("{sysfs}/level")) {
            assert_eq!(parse_sysfs_dec(&l), Some(1), "{sysfs}/level");
            let t = std::fs::read_to_string(std::format!("{sysfs}/type")).unwrap_or_default();
            assert!(matches!(t.trim(), "Data" | "Unified"), "{sysfs}/type = {t}");
        }
        // Where sysfs has the sizes, they agree with the report.
        if let Ok(s) = std::fs::read(std::format!("{sysfs}/size")) {
            if let Some(l1d) = parse_sysfs_size(&s) {
                assert_eq!(c.caches.l1d, l1d, "{line}");
            }
        }
        for i in 1..3 {
            let dir = std::format!("/sys/devices/system/cpu/cpu0/cache/index{i}");
            let Ok(l) = std::fs::read(std::format!("{dir}/level")) else { break };
            if parse_sysfs_dec(&l) == Some(2) {
                // Docker Desktop's VM has the directory without a `size`.
                if let Some(l2) = std::fs::read(std::format!("{dir}/size")).ok().and_then(|b| parse_sysfs_size(&b)) {
                    assert_eq!(c.caches.l2, l2, "{line}");
                }
                break;
            }
        }
        // `address sizes: ... 57 bits virtual` is the kernel's report of
        // the CPU's virtual address width (cpuid 0x80000008 `EAX[15:8]`),
        // not of the paging mode: a GCE C3D guest (AMD EPYC 9B14, Genoa)
        // prints 57 bits virtual while the hypervisor hides the LA57
        // feature bit (leaf 7 `ECX[16]`), so its kernel cannot run
        // five-level paging and the hinted `mmap` lands below 2^47
        // (`la57_enabled=no`; the CPU matrix of 2026-09-19). With the
        // feature bit, the kernel's width and the hinted `mmap` agree.
        if let Isa::X86(x) = c.isa {
            let text = std::fs::read_to_string("/proc/cpuinfo").unwrap();
            if let Some(l) = text.lines().find(|l| l.starts_with("address sizes")) {
                if x.la57 {
                    assert_eq!(x.la57_enabled, l.contains("57 bits virtual"), "{l}: {line}");
                } else {
                    assert!(!x.la57_enabled, "{l}: {line}");
                    if l.contains("57 bits virtual") {
                        std::eprintln!("{l} without the la57 bit (a guest that hides LA57): la57_enabled=no is right");
                    }
                }
            }
        }
    }

    /// The probe is a cold path run once at init; this prints its cost so
    /// the results file can record it (the budget is 50 us added to init)
    /// and guards against a pathological regression such as reading all
    /// of `/proc/cpuinfo`.
    #[test]
    fn probe_cost_is_small() {
        let page = crate::sys::features::page_size();
        let mut us: Vec<u128> = (0..1000)
            .map(|_| {
                let t = std::time::Instant::now();
                let c = Cpu::probe(page);
                let d = t.elapsed().as_nanos();
                assert!(matches!(c.caches.line, 32 | 64 | 128 | 256));
                d
            })
            .collect();
        us.sort_unstable();
        std::eprintln!(
            "Cpu::probe: min {} us, median {} us, p90 {} us over {} runs",
            us[0] as f64 / 1000.0,
            us[us.len() / 2] as f64 / 1000.0,
            us[us.len() * 9 / 10] as f64 / 1000.0,
            us.len()
        );
        assert!(us[us.len() / 2] < 1_000_000, "median {} ns", us[us.len() / 2]);
    }

    /// The HWCAP constants against the installed kernel header, where the
    /// container has one for this architecture.
    #[test]
    fn hwcap_constants_match_the_header() {
        let Ok(text) = std::fs::read_to_string("/usr/include/asm/hwcap.h") else {
            std::eprintln!("no /usr/include/asm/hwcap.h here, skipping");
            return;
        };
        let value = |name: &str| -> Option<u64> {
            let l = text.lines().find(|l| l.starts_with("#define ") && l.split_whitespace().nth(1) == Some(name))?;
            let inner = l.split_once('(')?.1.split_once(')')?.0;
            let shift: u32 = inner.split("<<").nth(1)?.trim().parse().ok()?;
            Some(1u64 << shift)
        };
        for (name, ours) in [
            ("HWCAP_ATOMICS", HWCAP_ATOMICS),
            ("HWCAP_SVE", HWCAP_SVE),
            ("HWCAP_USCAT", HWCAP_USCAT),
            ("HWCAP_PACA", HWCAP_PACA),
            ("HWCAP_PACG", HWCAP_PACG),
            ("HWCAP2_BTI", HWCAP2_BTI),
            ("HWCAP2_MTE", HWCAP2_MTE),
            ("HWCAP2_MTE3", HWCAP2_MTE3),
        ] {
            match value(name) {
                Some(v) => assert_eq!(ours, v, "{name}"),
                None => std::eprintln!("{name} not in the header, skipping"),
            }
        }
    }
}
