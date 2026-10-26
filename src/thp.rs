//! Transparent huge pages for the regions (design.md sections 7, 10 and 17).
//!
//! The `thp` key (`config::Thp`) names what the program wants; the kernel's
//! sysfs table (`sys::thp_host::Thp`, read at init) says what a
//! `MADV_HUGEPAGE`d range can get; [`decide`] turns the two into the
//! effective [`Mode`] and the thresholds the commit paths read:
//!
//! - `never` (the default): [`Mode::Off`], nothing is advised
//!   `MADV_HUGEPAGE`. On a kernel with THP the class superblock and the
//!   metadata region are `MADV_NOHUGEPAGE`d at reservation
//!   ([`advise_reservation`], from `layout::Regions::reserve_owning`), so the slab
//!   regions get no huge page even where the host's global mode is
//!   `always`. The huge region ([`Reservation::Huge`]) follows the host's
//!   policy, as jemalloc's large extents do: a huge run is a whole
//!   allocation the program asked for. The advice is a VMA flag every later
//!   split inherits, so one call per reservation is enough.
//! - `auto`: [`Mode::Folios`], the multi-size THP path. `SlabTable::
//!   take_group` advises the slab groups of each class whose slab can hold
//!   an enabled folio size below the PMD, and metadata commits are rounded
//!   to and advised with that folio. Never a PMD-sized page: where the PMD
//!   size is `madvise`-gated the advice would also make the region eligible
//!   for 2 MiB pages, at the fault and through khugepaged, and bring back
//!   the fork copy-on-write cost that makes Redis and Valkey forbid huge
//!   pages, so `auto` is inert there ([`Reason::PmdMadvise`]).
//! - `always`: every enabled size. With the PMD size enabled and no larger
//!   than [`LG_PMD_CHUNK_MAX`] (2 MiB, the PMD of 4 KiB pages),
//!   [`Mode::Chunks`]: `SlabTable::commit_slab` commits and advises the
//!   PMD-sized chunk around the slab (clipped to the region) so the first
//!   touch maps a 2 MiB page, huge runs of a PMD or more are advised, and
//!   metadata takes the sub-PMD folio if one is enabled. At 16 and 64 KiB
//!   pages the PMD is 32 MiB and 512 MiB and a chunk per class would be the
//!   process's RSS floor, so `always` takes the folio path over the enabled
//!   sub-PMD sizes there, and is inert ([`Reason::PmdTooLarge`]) when none
//!   is.
//!
//! `auto` and `always` issue no `MADV_NOHUGEPAGE`, so on an `always` host
//! their unadvised regions stay eligible for the host's PMD pages.
//!
//! Design 11's rule ([`forced_by_limit`]: an effective cgroup limit under
//! 1 GiB forces a configured `thp:always` to `never`) is applied by
//! `Shared::init` to the stored key; [`force_never`] mirrors it here and
//! advises the class and metadata reservations already made. `auto` is not
//! forced: a 64 KiB folio is the slab's own granularity.
//!
//! Everything here is cold: a decision at init, one `madvise` per slab
//! group, per PMD chunk, per huge run or per metadata commit. The fast
//! paths never read this module.

use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::config::Thp as Key;
use crate::sys::features::Features;
use crate::sys::thp_host::{Thp as Table, ThpSetting};
use vstd::prelude::*;

verus! {

/// The effective mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Nothing is advised.
    Off = 0,
    /// `MADV_HUGEPAGE` on slab groups (folios below the PMD).
    Folios = 1,
    /// PMD-sized chunks committed and advised at slab commit.
    Chunks = 2,
}

impl Mode {
    fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Folios,
            2 => Mode::Chunks,
            _ => Mode::Off,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Folios => "folios",
            Mode::Chunks => "chunks",
        }
    }
}

/// Why the mode is what it is, for the report.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reason {
    /// `thp:never`.
    Never = 0,
    /// The kernel has no transparent huge pages (`thp=none`).
    NoThp = 1,
    /// `auto`: the kernel enables no folio size below the PMD.
    NoSubPmd = 2,
    /// `auto`: the PMD size is `madvise`-gated, so the advice would enable
    /// 2 MiB pages.
    PmdMadvise = 3,
    /// `always`: every size is `never` in sysfs.
    NoneEnabled = 4,
    /// Forced to `never` by a cgroup limit under 1 GiB (design 11,
    /// [`forced_by_limit`]).
    Forced = 5,
    /// The mode is in force.
    Active = 6,
    /// `always`: the PMD is larger than [`LG_PMD_CHUNK_MAX`] (a 16 or 64
    /// KiB page kernel) and no size below it is enabled.
    PmdTooLarge = 7,
}

impl Reason {
    fn from_u8(v: u8) -> Reason {
        match v {
            1 => Reason::NoThp,
            2 => Reason::NoSubPmd,
            3 => Reason::PmdMadvise,
            4 => Reason::NoneEnabled,
            5 => Reason::Forced,
            6 => Reason::Active,
            7 => Reason::PmdTooLarge,
            _ => Reason::Never,
        }
    }

    pub const fn text(self) -> &'static str {
        match self {
            Reason::Never => "thp:never",
            Reason::NoThp => "the kernel has no transparent huge pages",
            Reason::NoSubPmd => "no folio size below the PMD is enabled in sysfs",
            Reason::PmdMadvise => {
                "the PMD size is madvise-gated in sysfs, the advice would enable 2 MiB pages"
            }
            Reason::NoneEnabled => "every THP size is never in sysfs",
            Reason::Forced => "forced never by a cgroup limit under 1 GiB",
            Reason::Active => "active",
            Reason::PmdTooLarge => {
                "the PMD is larger than 2 MiB on this page size and no smaller size is enabled"
            }
        }
    }
}

/// The decision at init: the mode and the thresholds the commit paths read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub mode: Mode,
    pub reason: Reason,
    /// `log2` of the smallest slab size a class needs for its groups (or
    /// chunks) to be advised; `NO_SIZE` when no class is.
    pub lg_slab_min: u8,
    /// `log2` of the PMD size.
    pub lg_pmd: u8,
    /// `log2` of the folio metadata commits are rounded to and advised
    /// with; `NO_SIZE` when metadata is left alone.
    pub lg_meta: u8,
    /// Huge runs of a PMD or more are advised.
    pub huge: bool,
}

/// "No size": no threshold; the readers test for it before shifting.
pub const NO_SIZE: u8 = u8::MAX;
/// The largest folio metadata commits are rounded to: the bump area's
/// commit chunk (`meta::COMMIT_CHUNK`, 64 KiB), which a larger folio could
/// not fill.
const LG_META_MAX: u8 = 16;
/// The largest PMD `always` commits and advises in chunks: 2 MiB, the PMD
/// of a 4 KiB page kernel and jemalloc's unit (the module doc).
pub const LG_PMD_CHUNK_MAX: u8 = 21;

impl Decision {
    pub const OFF: Decision = Decision {
        mode: Mode::Off,
        reason: Reason::Never,
        lg_slab_min: NO_SIZE,
        lg_pmd: 21,
        lg_meta: NO_SIZE,
        huge: false,
    };

    fn off(reason: Reason, lg_pmd: u8) -> (d: Decision)
        ensures
            d.mode == Mode::Off,
            d.reason == reason,
            d.lg_slab_min == NO_SIZE,
            d.lg_pmd == lg_pmd,
            d.lg_meta == NO_SIZE,
            !d.huge,
    {
        Decision {
            reason,
            lg_pmd,
            ..Decision::OFF
        }
    }

    /// Every threshold is a valid shift amount (below 64), or `NO_SIZE`
    /// where the readers test for it first.
    pub open spec fn wf(self) -> bool {
        lg_ok(self.lg_slab_min) && self.lg_pmd < 64 && lg_ok(self.lg_meta)
    }
}

/// A `log2` threshold a reader can shift by once it has ruled out
/// `NO_SIZE`.
pub open spec fn lg_ok(lg: u8) -> bool {
    lg < 64 || lg == NO_SIZE
}

/// The PMD size is `madvise`-gated: advising a range would make it
/// eligible for PMD pages.
pub open spec fn pmd_madvise_gated(t: Table) -> bool {
    t.always & (1u64 << t.lg_pmd) == 0 && t.madvise & (1u64 << t.lg_pmd) != 0
}

/// A size in `sub_pmd_sizes` (bits below `lg_pmd`) is below the PMD.
proof fn lemma_sub_pmd_below(set: u64, lg: u8, lg_pmd: u8)
    requires
        lg < 64,
        lg_pmd < 64,
        (set & sub(1u64 << lg_pmd, 1u64)) & (1u64 << lg) != 0,
    ensures
        lg < lg_pmd,
{
    let (s, l, p) = (set, lg as u64, lg_pmd as u64);
    assert((s & sub(1u64 << p, 1u64)) & (1u64 << l) != 0 && l < 64 && p < 64 ==> l < p) by (bit_vector);
}

/// The folio metadata commits take: the smallest enabled size below the
/// PMD when it fits the bump chunk, else none.
fn meta_folio(t: &Table) -> (lg: u8)
    requires
        t.lg_pmd < 64,
    ensures
        lg == NO_SIZE || lg <= LG_META_MAX && lg < t.lg_pmd,
{
    match Table::smallest(t.sub_pmd_sizes()) {
        Some(lg) if lg <= LG_META_MAX => {
            proof {
                lemma_sub_pmd_below(t.always | t.madvise, lg, t.lg_pmd);
            }
            lg
        },
        _ => NO_SIZE,
    }
}

/// The effective mode for `key` on a kernel described by `t`. Pure.
pub fn decide(key: Key, t: &Table) -> (d: Decision)
    requires
        t.lg_pmd < 64,
    ensures
        d.wf(),
        d.lg_pmd == t.lg_pmd,
        (d.mode == Mode::Off) == (d.reason != Reason::Active),
        d.mode == Mode::Off ==> d.lg_slab_min == NO_SIZE && d.lg_meta == NO_SIZE && !d.huge,
        key == Key::Never ==> d.mode == Mode::Off && d.reason == Reason::Never,
        // Folios are sizes below the PMD, never the PMD itself.
        d.mode == Mode::Folios ==> d.lg_slab_min < d.lg_pmd,
        d.lg_meta == NO_SIZE || d.lg_meta <= 16 && d.lg_meta < d.lg_pmd,
        d.mode == Mode::Chunks ==> key == Key::Always && d.lg_pmd <= LG_PMD_CHUNK_MAX,
        d.huge ==> key == Key::Always,
        // `auto` refuses a host whose PMD size is `madvise`-gated.
        key == Key::Auto && d.mode != Mode::Off ==> !pmd_madvise_gated(*t),
{
    let lg_pmd = t.lg_pmd;
    match key {
        Key::Never => Decision::off(Reason::Never, lg_pmd),
        Key::Auto => {
            if t.enabled.is_none() {
                return Decision::off(Reason::NoThp, lg_pmd);
            }
            let Some(lg_min) = Table::smallest(t.sub_pmd_sizes()) else {
                return Decision::off(Reason::NoSubPmd, lg_pmd);
            };
            proof {
                lemma_sub_pmd_below(t.always | t.madvise, lg_min, lg_pmd);
            }
            if t.pmd_setting() == ThpSetting::Madvise {
                return Decision::off(Reason::PmdMadvise, lg_pmd);
            }
            Decision {
                mode: Mode::Folios,
                reason: Reason::Active,
                lg_slab_min: lg_min,
                lg_pmd,
                lg_meta: meta_folio(t),
                huge: false,
            }
        }
        Key::Always => {
            if t.enabled.is_none() {
                return Decision::off(Reason::NoThp, lg_pmd);
            }
            if Table::smallest(t.enabled_sizes()).is_none() {
                return Decision::off(Reason::NoneEnabled, lg_pmd);
            }
            if t.pmd_setting() != ThpSetting::Never && lg_pmd <= LG_PMD_CHUNK_MAX {
                // Every class: the chunk makes a 64 KiB slab part of a
                // 2 MiB page whatever its own size.
                return Decision {
                    mode: Mode::Chunks,
                    reason: Reason::Active,
                    lg_slab_min: 0,
                    lg_pmd,
                    lg_meta: meta_folio(t),
                    huge: true,
                };
            }
            // The PMD size is `never`, or too large to commit in chunks
            // (the module doc): the folio path over the sizes below it.
            match Table::smallest(t.sub_pmd_sizes()) {
                Some(lg_min) => {
                    proof {
                        lemma_sub_pmd_below(t.always | t.madvise, lg_min, lg_pmd);
                    }
                    Decision {
                        mode: Mode::Folios,
                        reason: Reason::Active,
                        lg_slab_min: lg_min,
                        lg_pmd,
                        lg_meta: meta_folio(t),
                        huge: true,
                    }
                },
                None => Decision::off(Reason::PmdTooLarge, lg_pmd),
            }
        }
    }
}

exec static MODE: AtomicU8 = AtomicU8::new(Mode::Off as u8);
exec static REASON: AtomicU8 = AtomicU8::new(Reason::Never as u8);
exec static LG_SLAB_MIN: AtomicU8 = AtomicU8::new(NO_SIZE);
exec static LG_PMD: AtomicU8 = AtomicU8::new(21);
exec static LG_META: AtomicU8 = AtomicU8::new(NO_SIZE);
exec static HUGE: AtomicU8 = AtomicU8::new(0);
/// The key as configured, for the report.
exec static KEY: AtomicU8 = AtomicU8::new(0);

/// Ranges advised (and bytes) per region kind, and `madvise` failures.
exec static CLASS_RANGES: AtomicU64 = AtomicU64::new(0);
exec static CLASS_BYTES: AtomicU64 = AtomicU64::new(0);
exec static HUGE_RANGES: AtomicU64 = AtomicU64::new(0);
exec static META_RANGES: AtomicU64 = AtomicU64::new(0);
exec static FAILURES: AtomicU64 = AtomicU64::new(0);
/// Chunk commits (ranges whose first slab was the one committing).
exec static CHUNKS: AtomicU64 = AtomicU64::new(0);
/// Reservations `MADV_NOHUGEPAGE`d under `never` (the module doc): the
/// class superblock and the metadata region, so 2 on a kernel with THP.
exec static NOHUGE_RANGES: AtomicU64 = AtomicU64::new(0);
/// The kernel has transparent huge pages at all (the sysfs `enabled` file
/// exists): `MADV_NOHUGEPAGE` is `EINVAL` without them, so `never` then
/// has nothing to advise.
exec static KERNEL_THP: AtomicU8 = AtomicU8::new(0);
/// The reservations `never` covers, as made so far (`advise_reservation`):
/// the class superblock and the metadata mapping of
/// `layout::Regions::reserve_owning` (never the huge one, which follows the host),
/// so [`force_never`] can advise them after the fact. `(addr, len)`, `len`
/// 0 when unused.
const MAX_RESERVATIONS: usize = 4;
exec static RESERVATIONS: [(AtomicUsize, AtomicUsize); MAX_RESERVATIONS] = [
    (AtomicUsize::new(0), AtomicUsize::new(0)),
    (AtomicUsize::new(0), AtomicUsize::new(0)),
    (AtomicUsize::new(0), AtomicUsize::new(0)),
    (AtomicUsize::new(0), AtomicUsize::new(0)),
];

/// The three thresholds as stored. They hold [`Decision::wf`] values
/// (they start so and only [`store`] writes them, from [`configure`]'s
/// decisions and `off` ones), but no proof relies on it: vstd gives a
/// `core` atomic's load any value, and the readers shift by them with
/// `wrapping_shl`, which is what `<<` compiles to in the shipped build.
#[inline(always)]
fn load_lg_slab_min() -> u8 {
    LG_SLAB_MIN.load(Ordering::Relaxed)
}

#[inline(always)]
fn load_lg_pmd() -> u8 {
    LG_PMD.load(Ordering::Relaxed)
}

#[inline(always)]
fn load_lg_meta() -> u8 {
    LG_META.load(Ordering::Relaxed)
}

fn store(d: &Decision) {
    LG_SLAB_MIN.store(d.lg_slab_min, Ordering::Relaxed);
    LG_PMD.store(d.lg_pmd, Ordering::Relaxed);
    LG_META.store(d.lg_meta, Ordering::Relaxed);
    HUGE.store(d.huge as u8, Ordering::Relaxed);
    REASON.store(d.reason as u8, Ordering::Relaxed);
    // The mode last: a reader that sees it sees the thresholds.
    MODE.store(d.mode as u8, Ordering::Release);
}

/// Apply `key` on the probed kernel. Called once from init, before any
/// region is committed.
pub fn configure(key: Key, features: &Features) -> (d: Decision)
    requires
        features.thp.lg_pmd < 64,
    ensures
        d.wf(),
{
    KEY.store(
        match key {
            Key::Never => 0,
            Key::Auto => 1,
            Key::Always => 2,
        },
        Ordering::Relaxed,
    );
    KERNEL_THP.store(features.thp.enabled.is_some() as u8, Ordering::Relaxed);
    HOST_MODE.store(features.thp.enabled.map_or(0, host_code), Ordering::Relaxed);
    let d = decide(key, &features.thp);
    store(&d);
    d
}

/// Design 11's rule in one place: a configured `thp:always` under an
/// effective cgroup limit (`min(memory.high, memory.max)`, `limit`) below
/// `policy::THP_MIN_LIMIT` (1 GiB) is forced to `never` (`Shared::init`
/// rewrites the stored key and calls [`force_never`]; the report says
/// so). `auto` and `never` are never forced, and no limit
/// (`sys::cgroup::UNLIMITED`) forces nothing.
pub fn forced_by_limit(key: Key, limit: u64) -> (r: bool)
    ensures
        r == (key == Key::Always && limit < crate::policy::THP_MIN_LIMIT),
{
    key == Key::Always && limit < crate::policy::THP_MIN_LIMIT
}

/// Design 11: the stored key was forced to `never` by a cgroup limit under
/// 1 GiB ([`forced_by_limit`]); mirror it here (init, after the cgroup is
/// read) and give the class and metadata reservations already made the
/// `never` advice (the module doc; the huge reservation was never recorded
/// and stays with the host's policy, as under a configured `never`).
pub fn force_never()
    requires
        crate::sys::may_syscall(),
{
    let d = Decision::off(Reason::Forced, load_lg_pmd());
    store(&d);
    for (addr, len) in RESERVATIONS.iter()
        invariant
            crate::sys::may_syscall(),
    {
        let len = len.load(Ordering::Relaxed);
        if len != 0 {
            advise_nohugepage(addr.load(Ordering::Relaxed), len);
        }
    }
}

/// Which of `layout::Regions::reserve_owning`'s three mappings a reservation is,
/// for [`advise_reservation`]: `never` covers the slab regions (the class
/// superblock and the metadata region) and leaves the huge region to the
/// host's policy (the module doc).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The class superblock: slabs, committed a group at a time.
    Classes,
    /// The huge region: whole runs of a page or more per allocation.
    Huge,
    /// The metadata region: descriptor tables and the bump area.
    Metadata,
}

impl Reservation {
    /// Whether `never` leaves the reservation to the host's THP policy
    /// instead of `MADV_NOHUGEPAGE`ing it.
    pub const fn follows_host(self) -> bool {
        matches!(self, Reservation::Huge)
    }
}

/// A reservation challoc made (`layout::Regions::reserve_owning`), before
/// anything in it is committed. A class or metadata reservation is
/// recorded for [`force_never`] and, under `thp:never` on a kernel with
/// THP, `MADV_NOHUGEPAGE`d here (the module doc); the huge reservation is
/// neither (it follows the host's policy). Returns whether the advice was
/// issued.
pub fn advise_reservation(kind: Reservation, addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    if kind.follows_host() {
        return false;
    }
    for (a, l) in RESERVATIONS.iter() {
        if l.load(Ordering::Relaxed) == 0 {
            a.store(addr, Ordering::Relaxed);
            l.store(len, Ordering::Relaxed);
            break;
        }
    }
    if !is_never() {
        return false;
    }
    advise_nohugepage(addr, len)
}

/// Whether `never` is the policy in force: the key, or `always` forced to
/// `never` by design 11. The key, not the mode: an `auto` the kernel's
/// table made inert keeps its regions eligible for the folios the host may
/// enable later (and `MADV_NOHUGEPAGE` would forbid them as well as the
/// PMD).
pub fn is_never() -> bool {
    key() == Key::Never || Reason::from_u8(REASON.load(Ordering::Relaxed)) == Reason::Forced
}

/// `MADV_NOHUGEPAGE` on a reservation, counted; nothing on a kernel
/// without THP.
fn advise_nohugepage(addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    if len == 0 || KERNEL_THP.load(Ordering::Relaxed) == 0 {
        return false;
    }
    if madvise_nohugepage(addr, len) {
        NOHUGE_RANGES.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        false
    }
}

/// Forget every recorded reservation (the tests share the process-wide
/// slots).
#[cfg(test)]
fn reset_reservations_for_test() {
    for (a, l) in RESERVATIONS.iter() {
        a.store(0, Ordering::Relaxed);
        l.store(0, Ordering::Relaxed);
    }
}

/// The host's global THP mode as read at init (0 for a kernel without
/// THP, else [`host_code`]), for the report.
exec static HOST_MODE: AtomicU8 = AtomicU8::new(0);

/// `ThpSetting` to the byte `HOST_MODE` holds (never 0).
const fn host_code(e: ThpSetting) -> u8 {
    match e {
        ThpSetting::Always => 1,
        ThpSetting::Inherit => 2,
        ThpSetting::Madvise => 3,
        ThpSetting::Never => 4,
    }
}

/// The host's global THP mode as read at init (`None` without THP).
pub fn host_mode() -> Option<ThpSetting> {
    match HOST_MODE.load(Ordering::Relaxed) {
        1 => Some(ThpSetting::Always),
        2 => Some(ThpSetting::Inherit),
        3 => Some(ThpSetting::Madvise),
        4 => Some(ThpSetting::Never),
        _ => None,
    }
}

/// The decision in force.
pub fn decision() -> Decision {
    Decision {
        mode: Mode::from_u8(MODE.load(Ordering::Acquire)),
        reason: Reason::from_u8(REASON.load(Ordering::Relaxed)),
        lg_slab_min: LG_SLAB_MIN.load(Ordering::Relaxed),
        lg_pmd: LG_PMD.load(Ordering::Relaxed),
        lg_meta: LG_META.load(Ordering::Relaxed),
        huge: HUGE.load(Ordering::Relaxed) != 0,
    }
}

/// The key as configured (after any forcing of the stored value).
pub fn key() -> Key {
    match KEY.load(Ordering::Relaxed) {
        1 => Key::Auto,
        2 => Key::Always,
        _ => Key::Never,
    }
}

/// What the commit path of a class with `slab_size`-byte slabs does:
/// `Off`, `Folios` (advise the group in `take_group`) or `Chunks` (commit
/// and advise the PMD chunk in `commit_slab`).
#[inline]
pub fn class_mode(slab_size: usize) -> Mode {
    let mode = Mode::from_u8(MODE.load(Ordering::Acquire));
    if mode == Mode::Off {
        return Mode::Off;
    }
    let lg_min = load_lg_slab_min();
    // `1usize << lg_min`, as `wrapping_shl`: what `<<` compiles to in the
    // shipped build ([`load_lg_slab_min`]).
    if lg_min != NO_SIZE && slab_size >= 1usize.wrapping_shl(lg_min as u32) {
        mode
    } else {
        Mode::Off
    }
}

/// The PMD size in bytes.
#[inline]
pub fn pmd_size() -> (pmd: usize)
    ensures
        pmd > 0,
        pmd & sub(pmd, 1) == 0,
{
    let lg = load_lg_pmd();
    proof {
        let l = (lg as u32 % 64) as usize;
        assert(l < 64 ==> (1usize << l) > 0 && (1usize << l) & sub(1usize << l, 1) == 0) by (bit_vector);
    }
    // `1usize << lg`, as in `class_mode`.
    1usize.wrapping_shl(lg as u32)
}

/// The folio metadata commits are rounded to and advised with; 0 when
/// metadata is left alone.
#[inline]
pub fn meta_folio_size() -> (r: usize)
    ensures
        r == 0 || exists|k: u64| k < 64 && r == #[trigger] (1usize << k),
{
    match load_lg_meta() {
        NO_SIZE => 0,
        // `1usize << lg`, as in `class_mode`.
        lg if MODE.load(Ordering::Acquire) != Mode::Off as u8 => {
            let r = 1usize.wrapping_shl(lg as u32);
            proof {
                assert(r == 1usize << ((lg as u32 % 64) as u64));
            }
            r
        },
        _ => 0,
    }
}

/// The smallest huge run (bytes) that is advised; `usize::MAX` when none.
#[inline]
pub fn huge_min() -> usize {
    if MODE.load(Ordering::Acquire) != Mode::Off as u8 && HUGE.load(Ordering::Relaxed) != 0 {
        pmd_size()
    } else {
        usize::MAX
    }
}

/// The PMD-aligned chunk around `[addr, addr + len)`, clipped to
/// `[lo, hi)`. Pure.
pub fn chunk(addr: usize, len: usize, lo: usize, hi: usize, pmd: usize) -> (r: (usize, usize))
    requires
        pmd > 0,
        pmd & sub(pmd, 1) == 0,
        addr + len <= usize::MAX,
    ensures
        lo <= r.0,
        r.1 <= hi,
        r.0 == lo || r.0 & sub(pmd, 1) == 0,
        r.1 == hi || r.1 & sub(pmd, 1) == 0,
        // The chunk covers the range, where the range lies in `[lo, hi)`
        // and rounding its end up does not overflow.
        lo <= addr ==> r.0 <= addr,
        addr + len <= hi && addr + len + (pmd - 1) <= usize::MAX ==> addr + len <= r.1,
        // A non-empty chunk, where the range starts in `[lo, hi]` and `lo`
        // is a PMD below the top of the address space.
        lo <= addr <= hi && lo + pmd <= usize::MAX ==> r.0 <= r.1,
{
    proof {
        let (a, x, m) = (addr, (addr + len) as usize, sub(pmd, 1));
        assert((a & !m) <= a && (a & !m) & m == 0) by (bit_vector);
        assert(forall|y: usize| #[trigger] (y & !m) & m == 0) by (bit_vector);
        assert(pmd > 0 && pmd & m == 0 && m == pmd - 1 && x + m <= usize::MAX ==> x <= ((x + m) as usize & !m))
            by (bit_vector);
    }
    let start = (addr & !(pmd - 1)).max(lo);
    let end = (addr + len).saturating_add(pmd - 1) & !(pmd - 1);
    proof {
        if lo <= addr <= hi && lo + pmd <= usize::MAX {
            let (x, m) = ((addr + len) as usize, sub(pmd, 1));
            let s = x.saturating_add(m);
            let a = addr;
            assert(s == usize::MAX || s == x + m);
            assert(x <= s);
            assert((s == usize::MAX ==> end == usize::MAX - m) && (s != usize::MAX ==> x <= end) && (a & !m) <= end)
                by (bit_vector)
                requires pmd > 0, pmd & m == 0, m == pmd - 1, end == s & !m, s == usize::MAX || s == x + m, a <= x, x <= s;
        }
    }
    (start, end.min(hi))
}

fn madvise_hugepage(addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: the callers pass ranges inside the allocator's own
    // reservations; `MADV_HUGEPAGE` changes a VMA flag, never contents.
    unsafe { crate::sys::madvise(addr, len, crate::sys::MADV_HUGEPAGE) }.is_ok()
}

fn madvise_nohugepage(addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    // SAFETY: a reservation this process owns; `MADV_NOHUGEPAGE` sets a
    // VMA flag, never contents.
    unsafe { crate::sys::madvise(addr, len, crate::sys::MADV_NOHUGEPAGE) }.is_ok()
}

fn advise(addr: usize, len: usize, ranges: &AtomicU64, count: bool) -> bool
    requires
        crate::sys::may_syscall(),
{
    if len == 0 {
        return true;
    }
    let ok = madvise_hugepage(addr, len);
    if ok {
        if count {
            ranges.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    ok
}

/// `MADV_HUGEPAGE` on a class region range (a slab group, or a PMD chunk
/// from any of its slabs: `count` says whether this call is the one that
/// starts the range, so the counters see each range once).
pub fn advise_class(addr: usize, len: usize, count: bool) -> bool
    requires
        crate::sys::may_syscall(),
{
    let ok = advise(addr, len, &CLASS_RANGES, count);
    if ok && count {
        CLASS_BYTES.fetch_add(len as u64, Ordering::Relaxed);
        if len >= pmd_size() {
            CHUNKS.fetch_add(1, Ordering::Relaxed);
        }
    }
    ok
}

/// `MADV_HUGEPAGE` on a huge run.
pub fn advise_huge(addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    advise(addr, len, &HUGE_RANGES, true)
}

/// `MADV_HUGEPAGE` on a metadata commit.
pub fn advise_meta(addr: usize, len: usize) -> bool
    requires
        crate::sys::may_syscall(),
{
    advise(addr, len, &META_RANGES, true)
}

/// The counters, for the report and `stats.thp.*`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub class_ranges: u64,
    pub class_bytes: u64,
    pub huge_ranges: u64,
    pub meta_ranges: u64,
    pub failures: u64,
    /// Ranges of a PMD or more among `class_ranges` (the chunks).
    pub chunks: u64,
    /// Reservations `MADV_NOHUGEPAGE`d under `never` (the module doc).
    pub nohuge_ranges: u64,
}

pub fn stats() -> Stats {
    Stats {
        class_ranges: CLASS_RANGES.load(Ordering::Relaxed),
        class_bytes: CLASS_BYTES.load(Ordering::Relaxed),
        huge_ranges: HUGE_RANGES.load(Ordering::Relaxed),
        meta_ranges: META_RANGES.load(Ordering::Relaxed),
        failures: FAILURES.load(Ordering::Relaxed),
        chunks: CHUNKS.load(Ordering::Relaxed),
        nohuge_ranges: NOHUGE_RANGES.load(Ordering::Relaxed),
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    fn table(enabled: Option<ThpSetting>, always: u64, madvise: u64) -> Table {
        table_at(21, enabled, always, madvise)
    }

    /// A kernel whose PMD is `1 << lg_pmd` (29 at 64 KiB pages).
    fn table_at(lg_pmd: u8, enabled: Option<ThpSetting>, always: u64, madvise: u64) -> Table {
        Table {
            enabled,
            lg_pmd,
            listed: always | madvise | (1 << lg_pmd),
            always,
            madvise,
        }
    }

    const K64: u64 = 1 << 16;
    const M2: u64 = 1 << 21;

    #[test]
    fn never_is_off_whatever_the_kernel() {
        for t in [
            table(None, 0, 0),
            table(Some(ThpSetting::Always), M2 | K64, 0),
            table(Some(ThpSetting::Madvise), 0, M2),
        ] {
            let d = decide(Key::Never, &t);
            assert_eq!(d.mode, Mode::Off);
            assert_eq!(d.reason, Reason::Never);
            assert_eq!(d.lg_slab_min, NO_SIZE);
            assert_eq!(d.lg_meta, NO_SIZE);
            assert!(!d.huge);
        }
    }

    #[test]
    fn auto_wants_a_sub_pmd_size_and_no_madvise_gated_pmd() {
        // Ubuntu's default: 2M inherits madvise, nothing below it: refused.
        let d = decide(Key::Auto, &table(Some(ThpSetting::Madvise), 0, M2));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::NoSubPmd));
        // 64K madvise but 2M still madvise: refused, the advice would enable 2M.
        let d = decide(Key::Auto, &table(Some(ThpSetting::Madvise), 0, M2 | K64));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::PmdMadvise));
        // 64K madvise, 2M never: the multi-size path.
        let d = decide(Key::Auto, &table(Some(ThpSetting::Madvise), 0, K64));
        assert_eq!((d.mode, d.reason), (Mode::Folios, Reason::Active));
        assert_eq!(d.lg_slab_min, 16);
        assert_eq!(d.lg_meta, 16);
        assert!(!d.huge);
        // 64K always, 2M always (the host's decision): folios, harmless.
        let d = decide(Key::Auto, &table(Some(ThpSetting::Always), K64 | M2, 0));
        assert_eq!((d.mode, d.reason), (Mode::Folios, Reason::Active));
        // Only 128K and up below the PMD: 64 KiB slabs do not qualify and
        // the metadata folio would not fit the bump chunk.
        let d = decide(Key::Auto, &table(Some(ThpSetting::Never), 0, 1 << 17));
        assert_eq!(d.mode, Mode::Folios);
        assert_eq!(d.lg_slab_min, 17);
        assert_eq!(d.lg_meta, NO_SIZE);
        // No THP at all.
        let d = decide(Key::Auto, &table(None, 0, 0));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::NoThp));
    }

    #[test]
    fn always_takes_chunks_with_the_pmd_and_folios_without() {
        let d = decide(Key::Always, &table(Some(ThpSetting::Madvise), 0, M2));
        assert_eq!((d.mode, d.reason), (Mode::Chunks, Reason::Active));
        assert_eq!(d.lg_slab_min, 0, "every class");
        assert_eq!(d.lg_meta, NO_SIZE, "no sub-PMD folio for the metadata");
        assert!(d.huge);
        let d = decide(Key::Always, &table(Some(ThpSetting::Madvise), 0, M2 | K64));
        assert_eq!(d.mode, Mode::Chunks);
        assert_eq!(d.lg_meta, 16);
        let d = decide(Key::Always, &table(Some(ThpSetting::Never), 0, K64));
        assert_eq!((d.mode, d.reason), (Mode::Folios, Reason::Active));
        assert_eq!(d.lg_slab_min, 16);
        assert!(d.huge);
        let d = decide(Key::Always, &table(Some(ThpSetting::Never), 0, 0));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::NoneEnabled));
        let d = decide(Key::Always, &table(None, 0, 0));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::NoThp));
    }

    /// A 64 KiB-page kernel (Ubuntu's `linux-generic-64k`, 6.8): the PMD
    /// is 512 MiB, `hugepages-524288kB` inherits `madvise`, every smaller
    /// size is `never`. `always` must not commit 512 MiB chunks.
    #[test]
    fn always_refuses_chunks_on_a_pmd_over_2m() {
        const M512: u64 = 1 << 29;
        let d = decide(
            Key::Always,
            &table_at(29, Some(ThpSetting::Madvise), 0, M512),
        );
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::PmdTooLarge));
        assert_eq!(d.lg_pmd, 29);
        assert_eq!(d.lg_slab_min, NO_SIZE);
        assert!(!d.huge);
        // With a size below the PMD enabled (2 MiB, 32 pages of 64 KiB):
        // the folio path from 2 MiB slabs up, huge runs of a PMD advised,
        // the metadata left alone (the folio does not fit the bump chunk).
        let d = decide(
            Key::Always,
            &table_at(29, Some(ThpSetting::Madvise), 0, M512 | M2),
        );
        assert_eq!((d.mode, d.reason), (Mode::Folios, Reason::Active));
        assert_eq!(d.lg_slab_min, 21);
        assert_eq!(d.lg_meta, NO_SIZE);
        assert!(d.huge);
        // A 16 KiB page kernel: the PMD is 32 MiB, likewise no chunks.
        let d = decide(
            Key::Always,
            &table_at(25, Some(ThpSetting::Always), 1 << 25, 0),
        );
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::PmdTooLarge));
        // `auto` at 64 KiB: the PMD is madvise-gated, refused as at 4 KiB.
        let d = decide(
            Key::Auto,
            &table_at(29, Some(ThpSetting::Madvise), 0, M512 | M2),
        );
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::PmdMadvise));
        let d = decide(Key::Auto, &table_at(29, Some(ThpSetting::Madvise), 0, M512));
        assert_eq!((d.mode, d.reason), (Mode::Off, Reason::NoSubPmd));
        // `never` is off there too, whatever the sizes.
        assert_eq!(
            decide(
                Key::Never,
                &table_at(29, Some(ThpSetting::Always), M512 | M2, 0)
            )
            .mode,
            Mode::Off
        );
        // The 2 MiB PMD keeps its chunks.
        assert_eq!(
            decide(Key::Always, &table(Some(ThpSetting::Madvise), 0, M2)).mode,
            Mode::Chunks
        );
    }

    #[test]
    fn chunk_is_pmd_aligned_and_clipped() {
        let pmd = 1 << 21;
        let base = 0x1000_0000;
        let end = base + 16 * pmd;
        // A 64 KiB slab in the middle of the third chunk.
        assert_eq!(
            chunk(base + 2 * pmd + 0x30000, 0x10000, base, end, pmd),
            (base + 2 * pmd, base + 3 * pmd)
        );
        // The first slab of the region.
        assert_eq!(chunk(base, 0x10000, base, end, pmd), (base, base + pmd));
        // The last slab.
        assert_eq!(
            chunk(end - 0x10000, 0x10000, base, end, pmd),
            (end - pmd, end)
        );
        // A 4 MiB slab spans two chunks.
        assert_eq!(
            chunk(base + pmd, 2 * pmd, base, end, pmd),
            (base + pmd, base + 3 * pmd)
        );
        // A region shorter than a chunk.
        assert_eq!(
            chunk(base, 0x10000, base, base + 0x40000, pmd),
            (base, base + 0x40000)
        );
    }

    #[test]
    fn statics_follow_the_decision() {
        crate::testutil::in_fork_child("thp decision statics", 30_000, || {
            // Process-wide statics, written in a fork child: a decision
            // stored here would change the paths of the class, huge and
            // metadata tests running beside it (#69).
            let t = table(Some(ThpSetting::Madvise), 0, K64);
            let d = decide(Key::Auto, &t);
            store(&d);
            assert_eq!(decision(), d);
            assert_eq!(class_mode(1 << 16), Mode::Folios);
            assert_eq!(class_mode(1 << 12), Mode::Off);
            assert_eq!(meta_folio_size(), 1 << 16);
            assert_eq!(huge_min(), usize::MAX);
            let d = decide(Key::Always, &table(Some(ThpSetting::Madvise), 0, M2));
            store(&d);
            assert_eq!(class_mode(1 << 12), Mode::Chunks);
            assert_eq!(meta_folio_size(), 0);
            assert_eq!(huge_min(), 1 << 21);
            assert_eq!(pmd_size(), 1 << 21);
            force_never();
            assert_eq!(decision().mode, Mode::Off);
            assert_eq!(decision().reason, Reason::Forced);
            assert_eq!(class_mode(1 << 16), Mode::Off);
            assert_eq!(meta_folio_size(), 0);
            assert_eq!(huge_min(), usize::MAX);
            store(&Decision::OFF);
        });
    }

    /// `never` covers the class and metadata reservations and leaves the
    /// huge one to the host, so the huge reservation is neither advised
    /// nor recorded for `force_never`.
    #[test]
    fn never_leaves_the_huge_reservation_to_the_host() {
        crate::testutil::in_fork_child("thp huge reservation", 30_000, || {
            assert!(!Reservation::Classes.follows_host());
            assert!(!Reservation::Metadata.follows_host());
            assert!(Reservation::Huge.follows_host());
            let before = stats();
            assert!(!advise_reservation(Reservation::Huge, 1 << 40, 1 << 30));
            assert_eq!(stats(), before);
            assert!(RESERVATIONS.iter().all(
                |(a, l)| l.load(Ordering::Relaxed) == 0 || a.load(Ordering::Relaxed) != 1 << 40
            ));
        });
    }

    /// The host mode round trip (the statics are process-wide; leave
    /// them as found).
    #[test]
    fn host_mode_round_trips() {
        let saved = HOST_MODE.load(Ordering::Relaxed);
        HOST_MODE.store(0, Ordering::Relaxed);
        assert_eq!(host_mode(), None);
        for e in [
            ThpSetting::Always,
            ThpSetting::Inherit,
            ThpSetting::Madvise,
            ThpSetting::Never,
        ] {
            HOST_MODE.store(host_code(e), Ordering::Relaxed);
            assert_eq!(
                host_mode(),
                Some(e),
                "{e:?}: the VM printed `host never` on a madvise host once"
            );
        }
        HOST_MODE.store(saved, Ordering::Relaxed);
    }

    /// The recorded slots: `(addr, len)` of every used one, in order.
    fn recorded() -> std::vec::Vec<(usize, usize)> {
        RESERVATIONS
            .iter()
            .map(|(a, l)| (a.load(Ordering::Relaxed), l.load(Ordering::Relaxed)))
            .filter(|&(_, l)| l != 0)
            .collect()
    }

    /// A range the advice can really be issued on: an anonymous mapping
    /// (the `madvise` is real).
    fn region(len: usize) -> usize {
        use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_NONE};
        unsafe { sys::mmap(0, len, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap()
    }

    fn release(addr: usize, len: usize) {
        unsafe { crate::sys::munmap(addr, len) }.unwrap();
    }

    /// `advise_reservation` records the class and metadata reservations
    /// in the first free slots, in order, up to `MAX_RESERVATIONS` (a
    /// fifth is dropped, not overwritten); a kernel without THP gets no
    /// advice and no count; with the kernel THP flag set `never` advises
    /// each class and metadata reservation once and counts the outcome
    /// (a success in `nohuge_ranges`, a refusal in `failures`: the host
    /// stub succeeds, a Linux kernel without THP refuses with `EINVAL`),
    /// and `force_never` advises every recorded slot again with the same
    /// outcome.
    #[test]
    fn reservations_are_recorded_in_order_up_to_the_limit() {
        crate::testutil::in_fork_child("thp reservation slots", 30_000, || {
            reset_reservations_for_test();
            assert_eq!(recorded(), []);
            let kernel_thp = KERNEL_THP.swap(0, Ordering::Relaxed);
            let before = stats();
            assert!(is_never(), "the default key is `never`");
            // No THP on the kernel: recorded, not advised, not counted.
            assert!(!advise_reservation(Reservation::Classes, 0x10_0000, 0x4000));
            assert!(!advise_reservation(
                Reservation::Metadata,
                0x20_0000,
                0x8000
            ));
            assert_eq!(recorded(), [(0x10_0000, 0x4000), (0x20_0000, 0x8000)]);
            assert_eq!(stats(), before);
            // The huge reservation is not recorded (the test above), and
            // neither is a slot beyond the limit.
            assert!(!advise_reservation(Reservation::Huge, 0x30_0000, 0x1000));
            assert!(!advise_reservation(Reservation::Classes, 0x40_0000, 0x1000));
            assert!(!advise_reservation(
                Reservation::Metadata,
                0x50_0000,
                0x1000
            ));
            assert_eq!(recorded().len(), MAX_RESERVATIONS);
            assert!(!advise_reservation(Reservation::Classes, 0x60_0000, 0x1000));
            assert_eq!(
                recorded(),
                [
                    (0x10_0000, 0x4000),
                    (0x20_0000, 0x8000),
                    (0x40_0000, 0x1000),
                    (0x50_0000, 0x1000)
                ],
                "the fifth reservation is dropped, the slots untouched"
            );
            assert_eq!(stats(), before);
            // A kernel with THP: each class and metadata reservation is
            // advised once and its outcome counted; the huge one is neither;
            // `force_never` re-advises both recorded slots.
            reset_reservations_for_test();
            KERNEL_THP.store(1, Ordering::Relaxed);
            let (a, b) = (region(0x4000), region(0x8000));
            let ok = advise_reservation(Reservation::Classes, a, 0x4000);
            assert_eq!(
                advise_reservation(Reservation::Metadata, b, 0x8000),
                ok,
                "the same kernel answer twice"
            );
            assert!(!advise_reservation(Reservation::Huge, 0x30_0000, 0x1000));
            assert_eq!(recorded(), [(a, 0x4000), (b, 0x8000)]);
            let counted = |s: Stats| {
                (
                    s.nohuge_ranges - before.nohuge_ranges,
                    s.failures - before.failures,
                )
            };
            assert_eq!(counted(stats()), if ok { (2, 0) } else { (0, 2) });
            force_never();
            assert_eq!(decision().reason, Reason::Forced);
            assert_eq!(
                counted(stats()),
                if ok { (4, 0) } else { (0, 4) },
                "both slots advised again"
            );
            // Straight to fd 1: the child must not wait on std's stdout
            // lock, which another thread may have held at the fork.
            let line: &[u8] = if ok {
                b"thp test: MADV_NOHUGEPAGE on a fresh mapping taken\n"
            } else {
                b"thp test: MADV_NOHUGEPAGE on a fresh mapping refused (no THP on this kernel)\n"
            };
            unsafe { libc::write(1, line.as_ptr().cast(), line.len()) };
            // Leave the statics as found and unmap the two regions.
            store(&Decision::OFF);
            KERNEL_THP.store(kernel_thp, Ordering::Relaxed);
            reset_reservations_for_test();
            release(a, 0x4000);
            release(b, 0x8000);
        });
    }
}
