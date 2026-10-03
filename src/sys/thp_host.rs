//! The kernel's transparent-huge-page configuration, read once at init
//! from `/sys/kernel/mm/transparent_hugepage` with raw syscalls
//! (design.md section 10, step 1), and its two feature-line keys, `thp`
//! and `mthp`. The policy that acts on it is `crate::thp`.

use core::fmt;

use super::features::{Value, write_words};
#[cfg(verus_only)]
use super::find_byte;
use super::{bytes_is, find_from, parse_decimal};
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

verus! {

/// The setting a bracketed word names.
pub open spec fn thp_word(w: Seq<u8>) -> Option<ThpSetting> {
    if w == b"always"@ {
        Some(ThpSetting::Always)
    } else if w == b"inherit"@ {
        Some(ThpSetting::Inherit)
    } else if w == b"madvise"@ {
        Some(ThpSetting::Madvise)
    } else if w == b"never"@ {
        Some(ThpSetting::Never)
    } else {
        None
    }
}

/// What [`parse_thp_setting`] returns: the word between the first `[` and
/// the first `]` after it.
pub open spec fn thp_setting_spec(b: Seq<u8>) -> Option<ThpSetting> {
    let open = find_byte(b, b'[');
    if open >= b.len() {
        None
    } else {
        let rest = b.skip(open as int + 1);
        let close = find_byte(rest, b']');
        if close >= rest.len() {
            None
        } else {
            thp_word(rest.take(close as int))
        }
    }
}

/// The bracketed word of a THP `enabled` file. `None` for anything that is
/// not one of the four words in brackets (an empty or foreign file).
pub fn parse_thp_setting(bytes: &[u8]) -> (r: Option<ThpSetting>)
    ensures
        r == thp_setting_spec(bytes@),
{
    // `bytes.iter().position(|&b| b == b'[')?` and the `]` after it,
    // spelled out (Verus does not specify `Iterator::position`), and the
    // `match` on byte-string patterns as comparisons (such patterns crash
    // Verus).
    let open = find_from(bytes, 0, b'[');
    proof {
        assert(bytes@.skip(0) =~= bytes@);
    }
    if open == bytes.len() {
        return None;
    }
    let rest = &bytes[open + 1..];
    let close = find_from(rest, 0, b']');
    proof {
        assert(rest@.skip(0) =~= rest@);
        assert(rest@ =~= bytes@.skip(open + 1));
    }
    if close == rest.len() {
        return None;
    }
    let word = &rest[..close];
    proof {
        assert(word@ =~= rest@.take(close as int));
    }
    if bytes_is(word, b"always") {
        Some(ThpSetting::Always)
    } else if bytes_is(word, b"inherit") {
        Some(ThpSetting::Inherit)
    } else if bytes_is(word, b"madvise") {
        Some(ThpSetting::Madvise)
    } else if bytes_is(word, b"never") {
        Some(ThpSetting::Never)
    } else {
        None
    }
}

} // verus!

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

verus! {

/// `/sys/kernel/mm/transparent_hugepage/`, the directory of every file
/// [`Thp::from_reader`] reads.
const THP_DIR: [u8; 36] = *b"/sys/kernel/mm/transparent_hugepage/";

/// `bytes` copied into `out` at `at`; the length after them.
#[inline(always)]
fn append(out: &mut [u8; 96], at: usize, bytes: &[u8]) -> (r: usize)
    requires
        at + bytes@.len() <= 96,
    ensures
        r == at + bytes@.len(),
        final(out)@.subrange(at as int, r as int) == bytes@,
        forall|j: int| 0 <= j < at ==> final(out)@[j] == old(out)@[j],
{
    let mut i = 0;
    while i < bytes.len()
        invariant
            i <= bytes@.len(),
            at + bytes@.len() <= 96,
            forall|j: int| 0 <= j < i ==> out@[at + j] == bytes@[j],
            forall|j: int| 0 <= j < at ==> out@[j] == old(out)@[j],
        decreases bytes@.len() - i,
    {
        out[at + i] = bytes[i];
        i += 1;
    }
    proof {
        assert(out@.subrange(at as int, at + bytes@.len()) =~= bytes@);
    }
    at + bytes.len()
}

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
    /// tests feed it fixtures. Shifts wrap as they do in release builds, so
    /// a page size past 2^33 (none exists) yields garbage bits rather than
    /// a panic; at every real page size nothing wraps and `lg_pmd` is
    /// below 64.
    pub fn from_reader(page: usize, mut read: impl FnMut(&[u8], &mut [u8]) -> usize) -> (r: Thp)
        requires
            forall|p: &[u8], b: &mut [u8]| p@.len() > 0 && p@.last() == 0 ==> #[trigger] call_requires(read, (p, b)),
            forall|p: &[u8], b: &mut [u8], n: usize|
                #[trigger] call_ensures(read, (p, b), n) ==> n <= b@.len() && final(b)@.len() == b@.len(),
        ensures
            0 < page <= 1usize << 33 ==> r.lg_pmd < 64,
    {
        let mut buf = [0u8; 128];
        // The paths are built in `path` from `THP_DIR` and a NUL rather
        // than passed as `b"...\0"` literals: Verus does not see a byte
        // string literal's bytes, and `read` needs the NUL.
        let mut path = [0u8; 96];
        let len = append(&mut path, 0, &THP_DIR);
        let len = append(&mut path, len, b"enabled");
        path[len] = 0;
        proof {
            assert(path@.subrange(0, len + 1).last() == 0);
        }
        let n = read(&path[..len + 1], &mut buf);
        let enabled = if n == 0 {
            None
        } else {
            parse_thp_setting(&buf[..n])
        };
        let lg_page = page.trailing_zeros() as u8;
        proof {
            let p = page as u64;
            vstd::std_specs::bits::axiom_u64_trailing_zeros(p);
            let z = vstd::std_specs::bits::u64_trailing_zeros(p) as u64;
            assert(1usize << 33 == 0x2_0000_0000usize) by (bit_vector);
            if 0 < page <= 1usize << 33 {
                assert(z < 64);
                assert((p >> z) & 1u64 == 1u64);
                assert(p != 0 && p <= 0x2_0000_0000 && z < 64 && (p >> z) & 1u64 == 1u64 ==> z <= 33) by (bit_vector);
            }
        }
        let mut lg_pmd = (2 * lg_page).saturating_sub(3);
        proof {
            if 0 < page <= 1usize << 33 {
                assert(lg_page <= 33);
                assert(lg_pmd < 64);
            }
        }
        let len = append(&mut path, 0, &THP_DIR);
        let len = append(&mut path, len, b"hpage_pmd_size");
        path[len] = 0;
        proof {
            assert(path@.subrange(0, len + 1).last() == 0);
        }
        let n = read(&path[..len + 1], &mut buf);
        if let Some(v) = parse_decimal(&buf[..n]) {
            if v.is_power_of_two() && v.trailing_zeros() as u8 > lg_page {
                proof {
                    let k = choose|k: nat| k < 64 && v as nat == vstd::arithmetic::power2::pow2(k);
                    vstd::arithmetic::power2::lemma_pow2_pos(k);
                    vstd::std_specs::bits::axiom_u64_trailing_zeros(v);
                }
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
        // `for lg in (lg_page + 1)..=lg_pmd`, spelled out (Verus does not
        // specify `RangeInclusive`'s iterator), and the paths written with
        // `append` instead of a loop over `THP_DIR.iter().chain(..)`; the shifts
        // are `wrapping_shl`, which is what `<<` compiles to in release.
        let mut lg = lg_page;
        while lg < lg_pmd
            invariant
                lg_page <= 64,
                lg_pmd <= 125,
                lg_page <= lg <= 125,
                t.lg_pmd == lg_pmd,
                0 < page <= 1usize << 33 ==> lg_pmd < 64,
                buf@.len() == 128,
                forall|p: &[u8], b: &mut [u8]| p@.len() > 0 && p@.last() == 0 ==> #[trigger] call_requires(read, (p, b)),
                forall|p: &[u8], b: &mut [u8], n: usize|
                    #[trigger] call_ensures(read, (p, b), n) ==> n <= b@.len() && final(b)@.len() == b@.len(),
            decreases lg_pmd - lg,
        {
            lg += 1;
            let len = append(&mut path, 0, &THP_DIR);
            let len = append(&mut path, len, b"hugepages-");
            let kib = 1u64.wrapping_shl(lg as u32) >> 10;
            proof {
                lemma_decimal_digits_u64(kib as nat);
            }
            let len = len + write_decimal(&mut path[len..], kib);
            let len = append(&mut path, len, b"kB/enabled");
            path[len] = 0;
            proof {
                assert(path@.subrange(0, len + 1).last() == 0);
            }
            let n = read(&path[..len + 1], &mut buf);
            if n == 0 {
                continue;
            }
            let Some(setting) = parse_thp_setting(&buf[..n]) else {
                continue;
            };
            let bit = 1u64.wrapping_shl(lg as u32);
            t.listed |= bit;
            match setting {
                ThpSetting::Always => t.always |= bit,
                ThpSetting::Madvise => t.madvise |= bit,
                ThpSetting::Inherit => match top {
                    ThpSetting::Always => t.always |= bit,
                    ThpSetting::Madvise => t.madvise |= bit,
                    _ => {}
                },
                ThpSetting::Never => {}
            }
        }
        if t.listed == 0 {
            // Before 6.8: the PMD size alone, under the top-level setting.
            let bit = 1u64.wrapping_shl(lg_pmd as u32);
            t.listed = bit;
            match top {
                ThpSetting::Always => t.always = bit,
                ThpSetting::Madvise => t.madvise = bit,
                _ => {}
            }
        }
        t
    }
}

} // verus!

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

verus! {

impl Thp {
    /// The pairs [`Thp::words`] yields.
    #[inline(always)]
    fn word_pairs(&self) -> [(&'static str, Value); 2] {
        // `self.enabled.map_or("none", ThpSetting::name)`, spelled as a
        // `match`: Verus does not take a function item as a closure.
        [
            (
                "thp",
                Value::Word(match self.enabled {
                    Some(s) => s.name(),
                    None => "none",
                }),
            ),
            ("mthp", Value::Sizes(self.enabled_sizes())),
        ]
    }
}

} // verus!

impl Thp {
    /// The two THP keys: `thp=<setting|none> mthp=<sizes|none>`. Outside
    /// `verus!`: an array's `IntoIter` has no specification;
    /// [`Thp::word_pairs`] builds them.
    pub fn words(&self) -> impl Iterator<Item = (&'static str, Value)> {
        self.word_pairs().into_iter()
    }
}

impl fmt::Display for Thp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_words(f, self.words())
    }
}

verus! {

/// The number of decimal digits of `v`.
pub open spec fn decimal_digits(v: nat) -> nat
    decreases v,
{
    if v < 10 {
        1
    } else {
        1 + decimal_digits(v / 10)
    }
}

proof fn lemma_decimal_digits_u64(v: nat)
    requires
        v <= u64::MAX,
    ensures
        1 <= decimal_digits(v) <= 20,
{
    assert(pow10(20) == 100_000_000_000_000_000_000nat) by (compute);
    lemma_decimal_digits_bound(v, 20);
}

proof fn lemma_decimal_digits_bound(v: nat, k: nat)
    requires
        k >= 1,
        v < pow10(k),
    ensures
        1 <= decimal_digits(v) <= k,
    decreases k,
{
    if v >= 10 {
        assert(k > 1) by {
            if k == 1 {
                assert(pow10(1) == 10) by {
                    assert(pow10(0) == 1);
                }
            }
        }
        assert(v / 10 < pow10((k - 1) as nat)) by {
            assert(pow10(k) == 10 * pow10((k - 1) as nat));
        }
        lemma_decimal_digits_bound(v / 10, (k - 1) as nat);
    }
}

/// `10^k`.
pub open spec fn pow10(k: nat) -> nat
    decreases k,
{
    if k == 0 {
        1
    } else {
        10 * pow10((k - 1) as nat)
    }
}

/// `v` in decimal into `out`; returns the digit count.
fn write_decimal(out: &mut [u8], mut v: u64) -> (n: usize)
    requires
        old(out)@.len() >= decimal_digits(v as nat),
    ensures
        n == decimal_digits(v as nat),
        final(out)@.len() == old(out)@.len(),
{
    let ghost v0 = v;
    let mut tmp = [0u8; 20];
    let mut n = 0;
    proof {
        lemma_decimal_digits_u64(v as nat);
    }
    loop
        invariant_except_break
            n + decimal_digits(v as nat) == decimal_digits(v0 as nat),
        invariant
            decimal_digits(v0 as nat) <= 20,
            n <= 20,
        ensures
            n == decimal_digits(v0 as nat),
        decreases v,
    {
        tmp[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..n
        invariant
            n <= 20,
            n == decimal_digits(v0 as nat),
            out@.len() == old(out)@.len(),
            out@.len() >= n,
    {
        out[i] = tmp[n - 1 - i];
    }
    n
}

/// The THP table from `/sys/kernel/mm/transparent_hugepage` (about ten
/// sysfs reads on a 4 KiB kernel).
pub fn probe(page: usize) -> (r: Thp)
    requires
        crate::sys::may_syscall(),
    ensures
        0 < page <= 1usize << 33 ==> r.lg_pmd < 64,
{
    Thp::from_reader(page, |path: &[u8], buf: &mut [u8]| -> (n: usize)
        requires
            path@.len() > 0,
            path@.last() == 0,
        ensures
            n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
        {
            super::read_small_file(path, buf).unwrap_or(0)
        })
}

} // verus!

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
