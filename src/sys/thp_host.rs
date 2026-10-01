//! The kernel's transparent-huge-page configuration, read once at init
//! from `/sys/kernel/mm/transparent_hugepage` with raw syscalls
//! (design.md section 10, step 1), and its two feature-line keys, `thp`
//! and `mthp`. The policy that acts on it is `crate::thp`.

use core::fmt;

use super::features::{Value, write_words};
use super::parse_decimal;
use vstd::prelude::*;

verus! {

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

} // verus!

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

verus! {

/// The kernel's transparent-huge-page configuration as read at init from
/// `/sys/kernel/mm/transparent_hugepage` with raw syscalls (design 10 step
/// 1). Folio sizes are bit sets over `log2(size)`:
/// bit `k` set means the `1 << k`-byte size. Every candidate size from two
/// pages up to the PMD is looked up by path, so a size the kernel does not
/// list (a directory without an `enabled` file, such as `hugepages-8kB`,
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
}

} // verus!

impl Thp {
    /// No THP at all: nothing listed, nothing enabled.
    pub const NONE: Thp = Thp {
        enabled: None,
        lg_pmd: 21,
        listed: 0,
        always: 0,
        madvise: 0,
    };

    /// Compute the table from the files' contents: `read(path, buf)` fills
    /// `buf` with the file at the NUL-terminated `path` and returns the
    /// byte count, 0 when it is missing. Pure over the reader, so the
    /// tests feed it fixtures.
    pub fn from_reader(page: usize, mut read: impl FnMut(&[u8], &mut [u8]) -> usize) -> Thp {
        const DIR: &[u8] = b"/sys/kernel/mm/transparent_hugepage/";
        let mut buf = [0u8; 128];
        let n = read(b"/sys/kernel/mm/transparent_hugepage/enabled\0", &mut buf);
        let enabled = if n == 0 {
            None
        } else {
            parse_thp_setting(&buf[..n])
        };
        let lg_page = page.trailing_zeros() as u8;
        let mut lg_pmd = (2 * lg_page).saturating_sub(3);
        let n = read(
            b"/sys/kernel/mm/transparent_hugepage/hpage_pmd_size\0",
            &mut buf,
        );
        if let Some(v) = parse_decimal(&buf[..n]) {
            if v.is_power_of_two() && v.trailing_zeros() as u8 > lg_page {
                lg_pmd = v.trailing_zeros() as u8;
            }
        }
        let mut t = Thp {
            enabled,
            lg_pmd,
            listed: 0,
            always: 0,
            madvise: 0,
        };
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
            let Some(setting) = parse_thp_setting(&buf[..n]) else {
                continue;
            };
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
}

verus! {

impl Thp {
    /// Sizes a `MADV_HUGEPAGE`d range can be backed with: effective
    /// `always` or `madvise`.
    #[inline]
    pub fn enabled_sizes(&self) -> (r: u64)
        ensures
            r == self.always | self.madvise,
    {
        self.always | self.madvise
    }

    /// The enabled sizes below the PMD (the multi-size THP path). Shifts
    /// `1u64` by `lg_pmd`, so needs it below 64.
    #[inline]
    pub fn sub_pmd_sizes(&self) -> (r: u64)
        requires
            self.lg_pmd < 64,
        ensures
            r == (self.always | self.madvise) & sub(1u64 << self.lg_pmd, 1u64),
    {
        let ghost lg = self.lg_pmd;
        proof {
            assert(lg < 64 ==> 1u64 << lg >= 1) by (bit_vector);
        }
        self.enabled_sizes() & ((1u64 << self.lg_pmd) - 1)
    }

    /// The effective setting of the PMD size: what `MADV_HUGEPAGE` would
    /// change for 2 MiB pages. `Never` when nothing is listed.
    pub fn pmd_setting(&self) -> (r: ThpSetting)
        requires
            self.lg_pmd < 64,
        ensures
            r == (if self.always & (1u64 << self.lg_pmd) != 0 {
                ThpSetting::Always
            } else if self.madvise & (1u64 << self.lg_pmd) != 0 {
                ThpSetting::Madvise
            } else {
                ThpSetting::Never
            }),
    {
        let bit = 1u64 << self.lg_pmd;
        if self.always & bit != 0 {
            ThpSetting::Always
        } else if self.madvise & bit != 0 {
            ThpSetting::Madvise
        } else {
            ThpSetting::Never
        }
    }

    /// `log2` of the smallest size in `set`, if any: the index of the
    /// lowest set bit, with every bit below it clear.
    pub fn smallest(set: u64) -> (r: Option<u8>)
        ensures
            set == 0 <==> r is None,
            r matches Some(lg) ==> lg < 64 && set & (1u64 << lg) != 0 && set & sub(1u64 << lg, 1u64) == 0,
    {
        proof {
            vstd::std_specs::bits::axiom_u64_trailing_zeros(set);
            let tz = vstd::std_specs::bits::u64_trailing_zeros(set) as u64;
            if set != 0 {
                assert(tz < 64 && (set >> tz) & 1u64 == 1u64 ==> set & (1u64 << tz) != 0) by (bit_vector);
                assert(tz < 64 && set << sub(64, tz) == 0 ==> set & sub(1u64 << tz, 1u64) == 0) by (bit_vector);
            }
        }
        (set != 0).then(|| -> (r: u8)
            ensures
                r as u32 == vstd::std_specs::bits::u64_trailing_zeros(set),
            { set.trailing_zeros() as u8 })
    }
}

} // verus!

impl Thp {
    /// The two THP keys: `thp=<setting|none> mthp=<sizes|none>`.
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> {
        [
            (
                "thp",
                Value::Word(self.enabled.map_or("none", ThpSetting::name)),
            ),
            ("mthp", Value::Sizes(self.enabled_sizes())),
        ]
        .into_iter()
    }
}

impl fmt::Display for Thp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_words(f, self.words())
    }
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

/// The THP table from `/sys/kernel/mm/transparent_hugepage` (about ten
/// sysfs reads on a 4 KiB kernel).
pub fn probe(page: usize) -> Thp {
    Thp::from_reader(page, |path, buf| {
        super::read_small_file(path, buf).unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake sysfs: `(path, contents)` pairs; a missing path reads 0 bytes.
    fn sysfs<'a>(files: &'a [(&'a str, &'a str)]) -> impl FnMut(&[u8], &mut [u8]) -> usize + 'a {
        move |path: &[u8], buf: &mut [u8]| {
            let p = core::str::from_utf8(&path[..path.len() - 1]).unwrap();
            assert!(path.last() == Some(&0), "path not NUL-terminated");
            let p = p
                .strip_prefix("/sys/kernel/mm/transparent_hugepage/")
                .unwrap_or_else(|| panic!("{p}"));
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
        assert_eq!(
            parse_thp_setting(b"always [madvise] never\n"),
            Some(ThpSetting::Madvise)
        );
        assert_eq!(
            parse_thp_setting(b"[always] madvise never\n"),
            Some(ThpSetting::Always)
        );
        assert_eq!(
            parse_thp_setting(b"always madvise [never]\n"),
            Some(ThpSetting::Never)
        );
        assert_eq!(
            parse_thp_setting(b"always [inherit] madvise never\n"),
            Some(ThpSetting::Inherit)
        );
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

    /// A 4 KiB-page kernel with per-size directories: `hugepages-8kB` exists
    /// without an `enabled` file, 16K..2M have one, 2M inherits.
    const PER_SIZE_4K: &[(&str, &str)] = &[
        ("enabled", "always [madvise] never\n"),
        ("hpage_pmd_size", "2097152\n"),
        ("hugepages-16kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-32kB/enabled", "always inherit madvise [never]\n"),
        ("hugepages-64kB/enabled", "always inherit madvise [never]\n"),
        (
            "hugepages-128kB/enabled",
            "always inherit madvise [never]\n",
        ),
        (
            "hugepages-256kB/enabled",
            "always inherit madvise [never]\n",
        ),
        (
            "hugepages-512kB/enabled",
            "always inherit madvise [never]\n",
        ),
        (
            "hugepages-1024kB/enabled",
            "always inherit madvise [never]\n",
        ),
        (
            "hugepages-2048kB/enabled",
            "always [inherit] madvise never\n",
        ),
    ];

    #[test]
    fn thp_table_per_size_lists_eight_sizes_and_enables_the_pmd_alone() {
        let t = Thp::from_reader(4096, sysfs(PER_SIZE_4K));
        assert_eq!(t.enabled, Some(ThpSetting::Madvise));
        assert_eq!(t.lg_pmd, 21);
        assert_eq!(
            t.listed,
            ((1u64 << 22) - 1) & !((1u64 << 14) - 1),
            "16K..2M"
        );
        assert_eq!(t.always, 0);
        assert_eq!(t.madvise, 1 << 21, "2M inherits madvise");
        assert_eq!(t.pmd_setting(), ThpSetting::Madvise);
        assert_eq!(t.sub_pmd_sizes(), 0);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=2M");
    }

    #[test]
    fn thp_table_with_64k_madvise_and_pmd_never() {
        let mut files = PER_SIZE_4K.to_vec();
        files.retain(|(f, _)| {
            !f.starts_with("hugepages-64kB") && !f.starts_with("hugepages-2048kB")
        });
        files.push(("hugepages-64kB/enabled", "always inherit [madvise] never\n"));
        files.push((
            "hugepages-2048kB/enabled",
            "always inherit madvise [never]\n",
        ));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(t.always, 0);
        assert_eq!(t.pmd_setting(), ThpSetting::Never);
        assert_eq!(t.sub_pmd_sizes(), 1 << 16);
        assert_eq!(Thp::smallest(t.sub_pmd_sizes()), Some(16));
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=64K");
        // A per-size `madvise` counts even when the top level is `never`.
        files.retain(|(f, _)| *f != "enabled");
        files.push(("enabled", "always madvise [never]\n"));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.enabled, Some(ThpSetting::Never));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(std::format!("{t}"), "thp=never mthp=64K");
        // `always` at the top level makes every `inherit` size always.
        files.retain(|(f, _)| *f != "enabled");
        files.push(("enabled", "[always] madvise never\n"));
        let t = Thp::from_reader(4096, sysfs(&files));
        assert_eq!(t.madvise, 1 << 16);
        assert_eq!(t.always, 0, "every other size is never, not inherit");
        assert_eq!(std::format!("{t}"), "thp=always mthp=64K");
    }

    #[test]
    fn thp_table_before_6_8_has_the_pmd_alone() {
        let t = Thp::from_reader(
            4096,
            sysfs(&[
                ("enabled", "always [madvise] never\n"),
                ("hpage_pmd_size", "2097152\n"),
            ]),
        );
        assert_eq!(t.listed, 1 << 21);
        assert_eq!(t.madvise, 1 << 21);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=2M");
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "[always] madvise never\n")]));
        assert_eq!(
            t.lg_pmd, 21,
            "computed from the page size when hpage_pmd_size is missing"
        );
        assert_eq!(t.always, 1 << 21);
        assert_eq!(std::format!("{t}"), "thp=always mthp=2M");
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "always madvise [never]\n")]));
        assert_eq!(t.enabled_sizes(), 0);
        assert_eq!(std::format!("{t}"), "thp=never mthp=none");
        // 64 KiB pages: the PMD is 512 MiB.
        let t = Thp::from_reader(65536, sysfs(&[("enabled", "always [madvise] never\n")]));
        assert_eq!(t.lg_pmd, 29);
        assert_eq!(std::format!("{t}"), "thp=madvise mthp=512M");
    }

    #[test]
    fn thp_table_missing_directory_is_none() {
        let t = Thp::from_reader(4096, sysfs(&[]));
        assert_eq!(t, Thp::NONE);
        assert_eq!(t.enabled, None);
        assert_eq!(t.enabled_sizes(), 0);
        assert_eq!(t.pmd_setting(), ThpSetting::Never);
        assert_eq!(std::format!("{t}"), "thp=none mthp=none");
        // A garbled `enabled` file is treated as no THP as well.
        let t = Thp::from_reader(4096, sysfs(&[("enabled", "garbage\n")]));
        assert_eq!(t.enabled, None);
        assert_eq!(t, Thp::NONE);
    }
}
