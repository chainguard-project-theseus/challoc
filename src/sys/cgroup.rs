//! cgroup v2 discovery and sampling (design 11).
//!
//! This is the measurement layer only: where the process's cgroup is, what
//! its effective memory limits are, how much it is charged right now, and
//! whether the kernel has raised the `high`/`max` event counters. The
//! policy that acts on the figures is `policy.rs` (design 11): the
//! headroom band over the effective limit (the retention cap, the decay
//! window, cache GC at every tick and the immediate purge above 90%), the
//! purge advice (`MADV_DONTNEED`, or with the `lazy_band:true` opt-in
//! `MADV_FREE` under 70% of a known limit), the full purge on a `high`
//! event and the `thp:always` force to `never` under 1 GiB; `stats.cgroup.*` and
//! `malloc_stats_print` expose the same figures.
//!
//! Everything is read with raw `open`/`read`/`close` on fixed-size stack
//! buffers through `sys`, never libc stdio, and nothing here allocates. The
//! parsers at the top of the module are pure functions over byte slices,
//! unit tested with captured file contents; the runtime half (`discover`,
//! `sample`) reads the live files, and its samplers are tested through a
//! reader over captured files.
//!
//! Costs: discovery is one read of `/proc/self/cgroup`, one line scan of
//! `/proc/self/mountinfo` and two small reads per level of the cgroup path,
//! once, [`DEFER_NS`] into the process's life ([`Deferred`]) rather than at
//! init: about 90 us of system time per exec that a process living less
//! than that never pays. Sampling is two small reads, plus one per ancestor
//! in [`LEVELS`] with a limit of its own, at most once per
//! [`SAMPLE_INTERVAL_NS`] (10 ms on `CLOCK_MONOTONIC_COARSE`), from slow
//! paths only; the limit walk is repeated at most once per
//! [`LIMIT_INTERVAL_NS`] so a `docker update --memory` is seen.

use crate::vspec::byte::BytePerm;
#[cfg(verus_keep_ghost)]
use crate::vspec::byte::byte_id;
use core::fmt;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use vstd::cell::CellId;
use vstd::cell::pcell::{PCell, PointsTo as PCellPerm};
use vstd::invariant::{AtomicInvariant, InvariantPredicate};

#[cfg(verus_keep_ghost)]
use super::{after_line, find_byte, lemma_line_step, line_len, opt_view, parse_decimal_spec};
use super::{bytes_is, find_from, parse_decimal};
use vstd::prelude::*;

verus! {

/// Longest cgroup directory (mount point plus path) that is tracked. Real
/// paths are a few hundred bytes (Kubernetes' pod and container scopes);
/// anything longer is reported as untracked rather than truncated.
pub const PATH_CAP: usize = 2048;
/// Room after the directory for `/` plus the longest file name read here
/// (`memory.current`, 14 bytes) plus the NUL, rounded up.
pub const FILE_ROOM: usize = 32;
/// Buffer for one cgroup file (`memory.events` is a handful of lines).
const FILE_BUF: usize = 1024;
/// Chunk size for the `/proc/self/cgroup` and `/proc/self/mountinfo` line
/// scans ([`scan_lines`]). A longer line is dropped: a `0::` line that
/// long has a path past what [`PATH_CAP`] leaves room for, and it reads as
/// no unified path rather than as a cut one.
const SCAN_BUF: usize = 2048;

/// Minimum interval between two samples of `memory.current` and
/// `memory.events` (design 11).
pub const SAMPLE_INTERVAL_NS: u64 = 10_000_000;
/// Minimum interval between two walks of `memory.max`/`memory.high`.
pub const LIMIT_INTERVAL_NS: u64 = 1_000_000_000;
/// How long after init discovery waits ([`Deferred`]): the first periodic
/// tick at least this far into the process's life discovers the cgroup,
/// as does the first read of its figures (`stats.cgroup.*`, the report).
/// Two sampling windows: a process that ends sooner (a shell command, a
/// short compiler step, an interpreter's start) never reads the files.
pub const DEFER_NS: u64 = 2 * SAMPLE_INTERVAL_NS;

/// `max` in a limit file: no limit at this level.
pub const UNLIMITED: u64 = u64::MAX;
/// Levels, from the leaf up, whose own limit and usage are kept: each one
/// with a limit has its `memory.current` sampled, because the kernel
/// enforces an ancestor's limit against the ancestor's usage, which counts
/// its other children too. Levels above these count only toward the
/// effective limit.
pub const LEVELS: usize = 16;

} // verus!

// ---------------------------------------------------------------------------
// Pure parsing (unit tested against captured contents).
//
// The scans are `while` loops over indices, and the comparisons are with
// `bytes_is` against byte arrays: Verus does not specify the slice
// iterators and adapters (`split`, `position`, `windows`, `find_map`),
// nor take byte-string literals or patterns.
// ---------------------------------------------------------------------------

verus! {

/// An ASCII whitespace byte (`u8::is_ascii_whitespace`).
pub open spec fn is_ws(b: u8) -> bool {
    b == 32 || b == 9 || b == 10 || b == 12 || b == 13
}

/// The number of leading whitespace bytes of `s`.
pub open spec fn ws_lead(s: Seq<u8>) -> nat
    decreases s.len(),
{
    if s.len() > 0 && is_ws(s[0]) {
        1 + ws_lead(s.drop_first())
    } else {
        0
    }
}

/// The number of trailing whitespace bytes of `s`.
pub open spec fn ws_trail(s: Seq<u8>) -> nat
    decreases s.len(),
{
    if s.len() > 0 && is_ws(s.last()) {
        1 + ws_trail(s.drop_last())
    } else {
        0
    }
}

/// `s` without its leading and trailing whitespace.
pub open spec fn trim_spec(s: Seq<u8>) -> Seq<u8> {
    let a = s.skip(ws_lead(s) as int);
    a.take(a.len() - ws_trail(a))
}

proof fn lemma_ws_lead(s: Seq<u8>, k: int)
    requires
        0 <= k <= s.len(),
        forall|j: int| 0 <= j < k ==> is_ws(#[trigger] s[j]),
        k == s.len() || !is_ws(s[k]),
    ensures
        ws_lead(s) == k,
    decreases k,
{
    if k > 0 {
        lemma_ws_lead(s.drop_first(), k - 1);
    }
}

proof fn lemma_ws_trail(s: Seq<u8>, e: int)
    requires
        0 <= e <= s.len(),
        forall|j: int| e <= j < s.len() ==> is_ws(#[trigger] s[j]),
        e == 0 || !is_ws(s[e - 1]),
    ensures
        ws_trail(s) == s.len() - e,
    decreases s.len() - e,
{
    if e < s.len() {
        lemma_ws_trail(s.drop_last(), e);
    }
}

/// Strip leading and trailing ASCII whitespace.
fn trim(b: &[u8]) -> (r: &[u8])
    ensures
        r@.len() <= b@.len(),
        r@ == trim_spec(b@),
{
    let mut start = 0;
    while start < b.len() && b[start].is_ascii_whitespace()
        invariant
            start <= b@.len(),
            forall|j: int| 0 <= j < start ==> is_ws(#[trigger] b@[j]),
        decreases b@.len() - start,
    {
        start += 1;
    }
    let mut end = b.len();
    while end > start && b[end - 1].is_ascii_whitespace()
        invariant
            start <= end <= b@.len(),
            forall|j: int| end <= j < b@.len() ==> is_ws(#[trigger] b@[j]),
        decreases end,
    {
        end -= 1;
    }
    proof {
        lemma_ws_lead(b@, start as int);
        let a = b@.skip(start as int);
        assert forall|j: int| end - start <= j < a.len() implies is_ws(#[trigger] a[j]) by {
            assert(a[j] == b@[start + j]);
        }
        if end > start {
            assert(a[end - start - 1] == b@[end - 1]);
        }
        lemma_ws_trail(a, end - start);
        assert(b@.subrange(start as int, end as int) =~= a.take(end - start));
    }
    &b[start..end]
}

/// What [`cgroup_path_from_line`] returns: the trimmed rest of a `0::`
/// line when it starts with `/`.
pub open spec fn cgroup_path_line_spec(l: Seq<u8>) -> Option<Seq<u8>> {
    if l.len() >= 3 && l[0] == 48 && l[1] == 58 && l[2] == 58 {
        let r = trim_spec(l.skip(3));
        if r.len() > 0 && r[0] == 47 {
            Some(r)
        } else {
            None
        }
    } else {
        None
    }
}

/// The unified-hierarchy path of one `/proc/self/cgroup` line: `0::/a/b`
/// gives `/a/b`; inside a cgroup namespace the line is `0::/`. `None` for
/// any other line (a v1 controller's `N:name:/path`).
pub fn cgroup_path_from_line(line: &[u8]) -> (r: Option<&[u8]>)
    ensures
        r matches Some(p) ==> p@.len() <= line@.len(),
        opt_view(r) == cgroup_path_line_spec(line@),
{
    if line.len() < 3 || line[0] != b'0' || line[1] != b':' || line[2] != b':' {
        return None;
    }
    let rest = trim(&line[3..]);
    if !rest.is_empty() && rest[0] == b'/' {
        Some(rest)
    } else {
        None
    }
}

/// What [`cgroup_path`] returns: the path of the first line that has one.
pub open spec fn cgroup_path_spec(s: Seq<u8>) -> Option<Seq<u8>>
    decreases s.len(),
{
    if s.len() == 0 {
        None
    } else {
        let l = s.take(line_len(s) as int);
        if l.len() > 0 && cgroup_path_line_spec(l) is Some {
            cgroup_path_line_spec(l)
        } else {
            cgroup_path_spec(after_line(s))
        }
    }
}

/// The unified-hierarchy path from the whole of `/proc/self/cgroup`, or
/// `None` when there is no `0::` line (a cgroup v1 only system). Every
/// non-empty line is tried in order; a final line without a newline
/// counts, so `contents` must be the whole file. Discovery streams the
/// file instead ([`proc_cgroup_path`]); this is the tests' reference.
// One test per step of the line scan, not one collapsed `if`.
#[allow(clippy::collapsible_if)]
#[cfg_attr(not(test), allow(dead_code))]
pub fn cgroup_path(contents: &[u8]) -> (r: Option<&[u8]>)
    ensures
        r matches Some(p) ==> p@.len() <= contents@.len(),
        opt_view(r) == cgroup_path_spec(contents@),
{
    proof {
        assert(contents@.skip(0) =~= contents@);
    }
    let mut start = 0;
    while start < contents.len()
        invariant
            start <= contents@.len(),
            cgroup_path_spec(contents@) == cgroup_path_spec(contents@.skip(start as int)),
        decreases contents@.len() - start,
    {
        let end = find_from(contents, start, b'\n');
        proof {
            lemma_line_step(contents@, start as int, end as int);
        }
        let line = &contents[start..end];
        if !line.is_empty() {
            if let Some(p) = cgroup_path_from_line(line) {
                return Some(p);
            }
        }
        if end == contents.len() {
            proof {
                assert(cgroup_path_spec(Seq::<u8>::empty()) is None);
            }
            return None;
        }
        start = end + 1;
    }
    proof {
        assert(contents@.skip(start as int) =~= Seq::<u8>::empty());
    }
    None
}

/// ` - ` starts at `j` in `s`.
pub open spec fn sep_at(s: Seq<u8>, j: int) -> bool {
    0 <= j && j + 3 <= s.len() && s[j] == 32 && s[j + 1] == 45 && s[j + 2] == 32
}

/// The index of the first ` - ` in `s`.
pub open spec fn find_sep(s: Seq<u8>) -> Option<nat>
    decreases s.len(),
{
    if s.len() < 3 {
        None
    } else if sep_at(s, 0) {
        Some(0)
    } else {
        match find_sep(s.drop_first()) {
            Some(i) => Some(i + 1),
            None => None,
        }
    }
}

proof fn lemma_find_sep(s: Seq<u8>, k: int)
    requires
        0 <= k <= s.len(),
        forall|j: int| 0 <= j < k ==> !#[trigger] sep_at(s, j),
        k + 3 > s.len() || sep_at(s, k),
    ensures
        find_sep(s) == (if k + 3 > s.len() { None } else { Some(k as nat) }),
    decreases k,
{
    if k > 0 && s.len() >= 3 {
        assert forall|j: int| 0 <= j < k - 1 implies !#[trigger] sep_at(s.drop_first(), j) by {
            assert(!sep_at(s, j + 1));
        }
        assert(!sep_at(s, 0));
        lemma_find_sep(s.drop_first(), k - 1);
    }
}

/// Field `n` (from 0) of `s` split at every space, empty fields counted
/// (`split(' ').nth(n)`).
pub open spec fn field(s: Seq<u8>, n: nat) -> Option<Seq<u8>>
    decreases n,
{
    let k = find_byte(s, 32);
    if n == 0 {
        Some(s.take(k as int))
    } else if k < s.len() {
        field(s.skip(k as int + 1), (n - 1) as nat)
    } else {
        None
    }
}

/// A mount point field: kept when it starts with `/`.
pub open spec fn mount_field(f: Option<Seq<u8>>) -> Option<Seq<u8>> {
    match f {
        Some(m) => if m.len() > 0 && m[0] == 47 {
            Some(m)
        } else {
            None
        },
        None => None,
    }
}

/// What [`cgroup2_mount_from_line`] returns: field 5 of a line whose
/// filesystem type is `cgroup2`, when it starts with `/`.
pub open spec fn cgroup2_mount_line_spec(l: Seq<u8>) -> Option<Seq<u8>> {
    match find_sep(l) {
        None => None,
        Some(sep) => {
            let post = l.skip(sep as int + 3);
            // `cgroup2`
            if post.take(find_byte(post, 32) as int) != seq![99u8, 103, 114, 111, 117, 112, 50] {
                None
            } else {
                mount_field(field(l.take(sep as int), 4))
            }
        },
    }
}

/// The mount point of one `/proc/self/mountinfo` line if it mounts a
/// `cgroup2` filesystem. The format is
/// `id parent major:minor root mount_point options [optional...] - fstype source super_options`;
/// the optional fields vary in number, which is what the ` - ` separator is
/// for. Octal escapes in the mount point (`\040` for a space) are left as
/// they are: no cgroup2 mount has one.
// Verus rejects byte-string literals.
#[allow(clippy::byte_char_slices)]
pub fn cgroup2_mount_from_line(line: &[u8]) -> (r: Option<&[u8]>)
    ensures
        opt_view(r) == cgroup2_mount_line_spec(line@),
{
    let mut sep = 0;
    while line.len() - sep >= 3
        && !(line[sep] == b' ' && line[sep + 1] == b'-' && line[sep + 2] == b' ')
        invariant
            sep <= line@.len(),
            forall|j: int| 0 <= j < sep ==> !#[trigger] sep_at(line@, j),
        decreases line@.len() - sep,
    {
        sep += 1;
    }
    proof {
        lemma_find_sep(line@, sep as int);
    }
    if line.len() - sep < 3 {
        return None;
    }
    let pre = &line[..sep];
    let post = &line[sep + 3..];
    let fend = find_from(post, 0, b' ');
    proof {
        assert(post@.skip(0) =~= post@);
    }
    if !bytes_is(&post[..fend], &[b'c', b'g', b'r', b'o', b'u', b'p', b'2']) {
        return None;
    }
    proof {
        assert(pre@.skip(0) =~= pre@);
        assert(cgroup2_mount_line_spec(line@) == mount_field(field(pre@, 4)));
    }
    // Field 4 (from 0) of `pre` split at every space.
    let mut start = 0;
    let mut n = 0;
    while n < 4
        invariant
            start <= pre@.len(),
            n <= 4,
            field(pre@, 4) == field(pre@.skip(start as int), (4 - n) as nat),
            cgroup2_mount_line_spec(line@) == mount_field(field(pre@, 4)),
        decreases 4 - n,
    {
        let k = find_from(pre, start, b' ');
        if k == pre.len() {
            return None;
        }
        proof {
            assert(pre@.skip(start as int).skip(k - start + 1) =~= pre@.skip(k + 1));
        }
        start = k + 1;
        n += 1;
    }
    let end = find_from(pre, start, b' ');
    proof {
        assert(pre@.skip(start as int).take(end - start) =~= pre@.subrange(start as int, end as int));
    }
    let mount = &pre[start..end];
    if !mount.is_empty() && mount[0] == b'/' {
        Some(mount)
    } else {
        None
    }
}

/// The first cgroup2 mount point in `/proc/self/mountinfo` contents, or
/// `None` when there is none (no unified hierarchy mounted).
#[cfg(test)]
// One test per step of the line scan, not one collapsed `if`.
#[allow(clippy::collapsible_if)]
pub fn cgroup2_mount(contents: &[u8]) -> Option<&[u8]> {
    let mut start = 0;
    while start < contents.len() {
        let end = find_from(contents, start, b'\n');
        let line = &contents[start..end];
        if !line.is_empty() {
            if let Some(m) = cgroup2_mount_from_line(line) {
                return Some(m);
            }
        }
        if end == contents.len() {
            return None;
        }
        start = end + 1;
    }
    None
}

/// `max`.
pub open spec fn max_word() -> Seq<u8> {
    seq![109u8, 97, 120]
}

/// What [`parse_limit`] returns.
pub open spec fn parse_limit_spec(s: Seq<u8>) -> Option<u64> {
    let t = trim_spec(s);
    if t == max_word() {
        Some(UNLIMITED)
    } else {
        parse_decimal_spec(t)
    }
}

/// `memory.max`, `memory.high` or `memory.current`: a decimal byte count,
/// or `max` meaning [`UNLIMITED`]. `None` for anything else (an absent
/// file is the caller's `None` before this is reached).
// Verus rejects byte-string literals.
#[allow(clippy::byte_char_slices)]
pub fn parse_limit(contents: &[u8]) -> (r: Option<u64>)
    ensures
        r == parse_limit_spec(contents@),
{
    let t = trim(contents);
    proof {
        assert(seq![b'm', b'a', b'x'] =~= max_word());
    }
    if bytes_is(t, &[b'm', b'a', b'x']) {
        return Some(UNLIMITED);
    }
    parse_decimal(t)
}

/// The counters of `memory.events` (cgroup v2 format, one `key value` per
/// line, all five on every supported kernel: `low 0`, `high 3`, `max 1`,
/// `oom 0`, `oom_kill 0`; `oom_group_kill`, 5.17, is ignored). A missing
/// line reads as 0. [`Cgroup::events`] returns the last sample of all
/// five; the policy acts on `high` and `max`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// Times the cgroup was reclaimed below `memory.low`.
    pub low: u64,
    /// Times `memory.high` was exceeded and reclaim started.
    pub high: u64,
    /// Times `memory.max` was hit.
    pub max: u64,
    /// Times the cgroup's usage reached the limit and the kernel had
    /// nothing left to reclaim (an OOM condition, kill or not).
    pub oom: u64,
    /// Processes of the cgroup the OOM killer killed.
    pub oom_kill: u64,
}

/// Every counter zero.
pub open spec fn events_zero() -> Events {
    Events { low: 0, high: 0, max: 0, oom: 0, oom_kill: 0 }
}

/// One `key value` line of `memory.events` applied to `e`: a known key
/// with a number takes it; anything else leaves `e` as it is.
pub open spec fn events_line(e: Events, l: Seq<u8>) -> Events {
    let k = find_byte(l, 32);
    if k >= l.len() {
        e
    } else {
        let key = l.take(k as int);
        match parse_decimal_spec(trim_spec(l.skip(k as int + 1))) {
            None => e,
            Some(v) => if key == seq![108u8, 111, 119] {
                Events { low: v, ..e }
            } else if key == seq![104u8, 105, 103, 104] {
                Events { high: v, ..e }
            } else if key == max_word() {
                Events { max: v, ..e }
            } else if key == seq![111u8, 111, 109] {
                Events { oom: v, ..e }
            } else if key == seq![111u8, 111, 109, 95, 107, 105, 108, 108] {
                Events { oom_kill: v, ..e }
            } else {
                e
            },
        }
    }
}

/// Every non-empty line of `s` applied to `e` in order.
pub open spec fn events_fold(e: Events, s: Seq<u8>) -> Events
    decreases s.len(),
{
    if s.len() == 0 {
        e
    } else {
        let l = s.take(line_len(s) as int);
        events_fold(if l.len() > 0 { events_line(e, l) } else { e }, after_line(s))
    }
}

/// One line of `memory.events` applied to `e` ([`events_line`]).
// Verus rejects byte-string literals.
#[allow(clippy::byte_char_slices)]
fn events_note_line(e: Events, line: &[u8]) -> (r: Events)
    ensures
        r == events_line(e, line@),
{
    let mut e = e;
    let k = find_from(line, 0, b' ');
    proof {
        assert(line@.skip(0) =~= line@);
    }
    if k < line.len() {
        let key = &line[..k];
        if let Some(v) = parse_decimal(trim(&line[k + 1..])) {
            proof {
                assert(seq![b'l', b'o', b'w'] =~= seq![108u8, 111, 119]);
                assert(seq![b'h', b'i', b'g', b'h'] =~= seq![104u8, 105, 103, 104]);
                assert(seq![b'm', b'a', b'x'] =~= max_word());
                assert(seq![b'o', b'o', b'm'] =~= seq![111u8, 111, 109]);
                assert(seq![b'o', b'o', b'm', b'_', b'k', b'i', b'l', b'l'] =~= seq![111u8, 111, 109, 95, 107, 105, 108, 108]);
            }
            if bytes_is(key, &[b'l', b'o', b'w']) {
                e.low = v;
            } else if bytes_is(key, &[b'h', b'i', b'g', b'h']) {
                e.high = v;
            } else if bytes_is(key, &[b'm', b'a', b'x']) {
                e.max = v;
            } else if bytes_is(key, &[b'o', b'o', b'm']) {
                e.oom = v;
            } else if bytes_is(key, &[b'o', b'o', b'm', b'_', b'k', b'i', b'l', b'l']) {
                e.oom_kill = v;
            }
        }
    }
    e
}

/// `memory.events` read into [`Events`]: every non-empty line applied in
/// order ([`events_fold`]), so a key seen twice keeps its last value.
pub fn parse_events(contents: &[u8]) -> (r: Events)
    ensures
        r == events_fold(events_zero(), contents@),
{
    let mut e = Events {
        low: 0,
        high: 0,
        max: 0,
        oom: 0,
        oom_kill: 0,
    };
    proof {
        assert(contents@.skip(0) =~= contents@);
    }
    let mut start = 0;
    while start < contents.len()
        invariant
            start <= contents@.len(),
            events_fold(events_zero(), contents@) == events_fold(e, contents@.skip(start as int)),
        decreases contents@.len() - start,
    {
        let end = find_from(contents, start, b'\n');
        proof {
            lemma_line_step(contents@, start as int, end as int);
        }
        let line = &contents[start..end];
        if !line.is_empty() {
            e = events_note_line(e, line);
        }
        if end == contents.len() {
            proof {
                assert(events_fold(e, Seq::<u8>::empty()) == e);
            }
            return e;
        }
        start = end + 1;
    }
    proof {
        assert(contents@.skip(start as int) =~= Seq::<u8>::empty());
    }
    e
}

} // verus!

verus! {

/// The levels of a walk from the leaf directory `dir` up to the cgroup2
/// mount (the first `mount_len` bytes of `dir`, inclusive), in turn: each
/// level (0 at the leaf) and its directory. A cursor rather than a
/// visitor closure: the samplers' reader is a `&mut` such a closure would
/// capture, and Verus has no closures that capture by mutable reference.
struct Levels<'a> {
    dir: &'a [u8],
    mount_len: usize,
    end: usize,
    level: usize,
    done: bool,
}

impl<'a> Levels<'a> {
    /// The level's directory is within `dir`, and the level count is
    /// bounded by the bytes walked past.
    spec fn wf(&self) -> bool {
        &&& self.end <= self.dir@.len()
        &&& self.level + self.end <= self.dir@.len()
    }

    /// What is left of the walk: nothing once the mount has been visited.
    spec fn rest(&self) -> nat {
        if self.done {
            0
        } else {
            self.end as nat + 1
        }
    }

    /// The walk over `dir`, from its leaf.
    fn new(dir: &'a [u8], mount_len: usize) -> (r: Levels<'a>)
        ensures
            r.wf(),
            r.dir == dir,
    {
        Levels {
            dir,
            mount_len,
            end: dir.len(),
            level: 0,
            done: false,
        }
    }

    /// The next level and its directory, `None` once the mount has been
    /// visited.
    fn next_level(&mut self) -> (r: Option<(usize, &'a [u8])>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self).dir == old(self).dir,
            r is None ==> old(self).rest() == 0,
            r matches Some((_, d)) ==> d@.len() <= old(self).dir@.len() && final(self).rest() < old(self).rest(),
    {
        if self.done {
            return None;
        }
        let d = &self.dir[..self.end];
        let level = self.level;
        if self.end <= self.mount_len {
            self.done = true;
            return Some((level, d));
        }
        // The last `/` of `dir[mount_len..end]`, which ends the parent.
        let mut i = self.end;
        while i > self.mount_len && self.dir[i - 1] != b'/'
            invariant
                self.mount_len <= i <= self.end,
                self.end <= self.dir@.len(),
            decreases i,
        {
            i -= 1;
        }
        if i == self.mount_len {
            self.done = true;
        } else {
            self.end = i - 1;
            self.level += 1;
        }
        Some((level, d))
    }
}

/// The minimum of what `read` yields per level of the walk ([`Levels`]);
/// `read` gets the level and its directory and answers `None` where the
/// file is absent. [`UNLIMITED`] when no level has a limit. Pure: the
/// reader is a closure so the tests feed it captured contents. The
/// samplers walk [`Levels`] in loops of their own, since their reader is a
/// `&mut` a closure would capture.
#[cfg_attr(not(test), allow(dead_code))]
pub fn walk_min<F: FnMut(usize, &[u8]) -> Option<u64>>(
    dir: &[u8],
    mount_len: usize,
    mut read: F,
) -> u64
    requires
        forall|l: usize, d: &[u8]| d@.len() <= dir@.len() ==> #[trigger] call_requires(read, (l, d)),
{
    let mut min = UNLIMITED;
    let mut levels = Levels::new(dir, mount_len);
    while let Some((level, d)) = levels.next_level()
        invariant
            levels.wf(),
            levels.dir == dir,
            forall|l: usize, d: &[u8]| d@.len() <= dir@.len() ==> #[trigger] call_requires(read, (l, d)),
        decreases levels.rest(),
    {
        if let Some(v) = read(level, d) {
            min = min.min(v);
        }
    }
    min
}

/// `s` without one trailing `/`.
pub open spec fn strip_slash(s: Seq<u8>) -> Seq<u8> {
    if s.len() > 0 && s.last() == 47 {
        s.drop_last()
    } else {
        s
    }
}

/// The path part [`join_dir`] appends: nothing for `/`, else the path
/// without a trailing `/`.
pub open spec fn join_path_part(p: Seq<u8>) -> Seq<u8> {
    if p == seq![47u8] {
        Seq::empty()
    } else {
        strip_slash(p)
    }
}

/// Join `mount` and `path` into `out` as the cgroup directory without a
/// trailing slash: (`/sys/fs/cgroup`, `/a/b`) is `/sys/fs/cgroup/a/b` and
/// (`/sys/fs/cgroup`, `/`) is `/sys/fs/cgroup`. Returns the directory
/// length and the mount's length within it, or `None` when it would not
/// fit in `out` less [`FILE_ROOM`].
pub fn join_dir(out: &mut [u8], mount: &[u8], path: &[u8]) -> (r: Option<(usize, usize)>)
    requires
        mount@.len() + path@.len() + FILE_ROOM <= usize::MAX,
    ensures
        final(out)@.len() == old(out)@.len(),
        ({
            let m = strip_slash(mount@);
            let p = join_path_part(path@);
            if m.len() + p.len() + FILE_ROOM > old(out)@.len() {
                r is None && final(out)@ == old(out)@
            } else {
                r == Some(((m.len() + p.len()) as usize, m.len() as usize))
                    && final(out)@.take((m.len() + p.len()) as int) == m + p
            }
        }),
{
    let ghost mount0 = mount@;
    let ghost path0 = path@;
    let mount = if !mount.is_empty() && mount[mount.len() - 1] == b'/' {
        &mount[..mount.len() - 1]
    } else {
        mount
    };
    let path = if path.len() == 1 && path[0] == b'/' {
        &path[..0]
    } else if !path.is_empty() && path[path.len() - 1] == b'/' {
        &path[..path.len() - 1]
    } else {
        path
    };
    proof {
        assert(mount@ =~= strip_slash(mount0)) by {
            if mount0.len() > 0 && mount0.last() == 47 {
                assert(mount@ =~= mount0.drop_last());
            }
        }
        assert(path@ =~= join_path_part(path0)) by {
            if path0 == seq![47u8] {
                assert(path@ =~= Seq::<u8>::empty());
            } else if path0.len() == 1 && path0[0] == 47 {
                assert(path0 =~= seq![47u8]);
            } else if path0.len() > 0 && path0.last() == 47 {
                assert(path@ =~= path0.drop_last());
            }
        }
    }
    let len = mount.len() + path.len();
    if len + FILE_ROOM > out.len() {
        return None;
    }
    out[..mount.len()].copy_from_slice(mount);
    out[mount.len()..len].copy_from_slice(path);
    proof {
        assert(out@.take(len as int) =~= mount@ + path@);
    }
    Some((len, mount.len()))
}

} // verus!

verus! {

/// Feed `buf`-sized chunks from `read` to `on_line` one complete line at a
/// time (without the newline), carrying a partial line across chunks;
/// stops early when `on_line` returns `true`, and returns where that line
/// is in `buf` (`None` when no line was taken). A line longer than `buf`
/// is dropped. `read` returns the bytes it filled, 0 at end of file, or
/// `None` on error, which ends the scan with no line taken: a final line
/// without a newline is passed on only at end of file, never one the
/// error may have cut. Runs until `read` reports the end or an error, so
/// a reader that never does keeps it going.
#[verifier::exec_allows_no_decreases_clause]
pub fn scan_lines<R, F>(buf: &mut [u8], mut read: R, mut on_line: F) -> (found: Option<(usize, usize)>)
    where
        R: FnMut(&mut [u8]) -> Option<usize>,
        F: FnMut(&[u8]) -> bool,
    requires
        forall|b: &mut [u8]| call_requires(read, (b,)),
        forall|b: &mut [u8], r: Option<usize>|
            call_ensures(read, (b,), r) ==> (r matches Some(n) ==> n <= b@.len()) && final(b)@.len() == b@.len(),
        forall|l: &[u8]| call_requires(on_line, (l,)),
    ensures
        final(buf)@.len() == old(buf)@.len(),
        found matches Some((lo, hi)) ==> lo <= hi <= final(buf)@.len(),
{
    let mut len = 0;
    let mut dropping = false;
    loop
        invariant
            len <= buf@.len(),
            buf@.len() == old(buf)@.len(),
            forall|b: &mut [u8]| call_requires(read, (b,)),
            forall|b: &mut [u8], r: Option<usize>|
                call_ensures(read, (b,), r) ==> (r matches Some(n) ==> n <= b@.len()) && final(b)@.len() == b@.len(),
            forall|l: &[u8]| call_requires(on_line, (l,)),
    {
        let n = match read(&mut buf[len..]) {
            Some(0) => break,
            None => return None,
            Some(n) => n,
        };
        proof {
            assert(buf.len() == buf@.len());
            assert(n <= buf@.len() - len);
        }
        len += n;
        let mut start = 0;
        loop
            invariant
                start <= len <= buf@.len(),
                buf@.len() == old(buf)@.len(),
                forall|l: &[u8]| call_requires(on_line, (l,)),
            decreases len - start,
        {
            let i = find_from(&buf[..len], start, b'\n');
            if i == len {
                break;
            }
            let line = &buf[start..i];
            let lo = start;
            start = i + 1;
            if dropping {
                dropping = false;
            } else if on_line(line) {
                return Some((lo, i));
            }
        }
        if start == 0 && len == buf.len() {
            // No newline in a full buffer: drop this line up to its end.
            dropping = true;
            len = 0;
        } else {
            buf.copy_within(start..len, 0);
            len -= start;
        }
    }
    if len > 0 && !dropping && on_line(&buf[..len]) {
        return Some((0, len));
    }
    None
}

/// The unified-hierarchy path of `/proc/self/cgroup` streamed from `read`
/// through `scan` ([`scan_lines`]), in `scan`. The file is scanned whole
/// (a hybrid v1+v2 host lists a dozen v1 lines before the `0::` line), so
/// a `0::` line is never taken cut short. `None` without one, on a read
/// error, or when it is longer than `scan`. The scan stops at the first
/// `0::` line and leaves it in `scan`, where it is parsed again (the line
/// closure cannot hand back a borrow of `scan`).
pub fn proc_cgroup_path<R: FnMut(&mut [u8]) -> Option<usize>>(scan: &mut [u8], read: R) -> (r: Option<&[u8]>)
    requires
        forall|b: &mut [u8]| call_requires(read, (b,)),
        forall|b: &mut [u8], r: Option<usize>|
            call_ensures(read, (b,), r) ==> (r matches Some(n) ==> n <= b@.len()) && final(b)@.len() == b@.len(),
    ensures
        r matches Some(p) ==> p@.len() <= old(scan)@.len(),
{
    let found = scan_lines(
        scan,
        read,
        |line: &[u8]| -> bool { cgroup_path_from_line(line).is_some() },
    );
    let (lo, hi) = found?;
    cgroup_path_from_line(&scan[lo..hi])
}

} // verus!

// ---------------------------------------------------------------------------
// Runtime state.
// ---------------------------------------------------------------------------

/// [`discovery_scratch`]'s storage (Verus has no `static mut`).
#[cfg(not(test))]
static mut DISCOVERY_SCRATCH: Scratch = Scratch::new();

verus! {

/// Why the cgroup is not tracked, or that it is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// `cgroup:false`: nothing was read.
    Disabled = 0,
    /// No usable `0::` line in `/proc/self/cgroup`: none (cgroup v1
    /// only), the file could not be read whole, or the line is longer
    /// than the 2 KiB scan buffer (a path past [`PATH_CAP`]).
    NoUnifiedPath = 1,
    /// No `cgroup2` filesystem in `/proc/self/mountinfo`.
    NoMount = 2,
    /// The directory exceeds [`PATH_CAP`].
    PathTooLong = 3,
    /// Reads work; the figures below are live.
    Tracked = 4,
}

/// The process's memory cgroup as last read (design 11). One per process,
/// inside `heap::Shared` ([`Deferred`]); written by `discover` (at init or
/// later) and by the samplers afterwards, so every mutable field is an
/// atomic.
pub struct Cgroup {
    /// Directory of the cgroup, `dir[..dir_len]`, without a trailing slash.
    dir: [u8; PATH_CAP],
    dir_len: u16,
    /// Length of the cgroup2 mount point prefix within `dir`.
    mount_len: u16,
    state: State,
    /// `memory.max` at the leaf level exists (the memory controller is on
    /// the unified hierarchy for this cgroup).
    has_limit_files: bool,
    has_current: bool,
    has_events: bool,
    /// Effective `memory.max`: the minimum over the walk; [`UNLIMITED`].
    limit: AtomicU64,
    /// Effective `memory.high`, likewise.
    high: AtomicU64,
    /// Last `memory.current` sample.
    current: AtomicU64,
    /// Per level from the leaf up ([`LEVELS`]): its own `min(memory.max,
    /// memory.high)` as of the last limit walk ([`UNLIMITED`] for none),
    /// and its last `memory.current` sample (read only where the level
    /// has a limit; unused at level 0, whose usage is `current`).
    level_limit: [AtomicU64; LEVELS],
    level_current: [AtomicU64; LEVELS],
    /// The last `memory.current` read failed: `current` is older.
    current_stale: AtomicBool,
    /// Coarse-clock time of the last sample and of the last limit walk.
    sampled_at: AtomicU64,
    limits_at: AtomicU64,
    /// Last `memory.events` counters read (all five, [`Events`]), and how
    /// many samples saw `high` and `max` rise (what the policy acts on: a
    /// rise of `high` means the kernel started reclaiming from this
    /// cgroup).
    events_low: AtomicU64,
    events_high: AtomicU64,
    events_max: AtomicU64,
    events_oom: AtomicU64,
    events_oom_kill: AtomicU64,
    high_rises: AtomicU64,
    max_rises: AtomicU64,
}

/// The buffers a discovery scans `/proc/self/cgroup` and
/// `/proc/self/mountinfo` through, and the mount point it copies out:
/// about 6 KiB, off the stack of the thread that discovers
/// ([`discovery_scratch`]).
pub struct Scratch {
    pub cgroup_scan: [u8; SCAN_BUF],
    pub mount_scan: [u8; SCAN_BUF],
    pub mount: [u8; PATH_CAP],
}

impl Scratch {
    pub const fn new() -> Scratch {
        Scratch { cgroup_scan: [0; SCAN_BUF], mount_scan: [0; SCAN_BUF], mount: [0; PATH_CAP] }
    }
}

/// The deferred discovery's scratch: one static, used only by the thread
/// that won a [`Deferred`] record's exchange from `PENDING` to `RUNNING`,
/// until its `DONE` store (the process has one record, and the exchange
/// lets one thread through), so no two references are live at once.
/// Trusted: that exclusion is the record's protocol, which Verus checks
/// for the cell's permission but not for this static. (A child of a `fork`
/// from a signal handler that interrupted the discovery can run a second
/// one when the handler allocates: the residue [`axiom_cell_orphaned`]
/// states, now also on these buffers.) The unit tests make their own
/// records and discover concurrently, so each call there gets fresh
/// buffers.
/// Private to this module, and only for a holder of the cell's permission
/// `won` (which [`Deferred::claim`] hands only to the exchange's winner):
/// [`Deferred::cell_discover`] is its one caller.
#[verifier::external_body]
fn discovery_scratch(Tracked(won): Tracked<&CellPerm>) -> &'static mut Scratch {
    #[cfg(not(test))]
    {
        // SAFETY: the exclusion above.
        unsafe { &mut *core::ptr::addr_of_mut!(DISCOVERY_SCRATCH) }
    }
    #[cfg(test)]
    {
        std::boxed::Box::leak(std::boxed::Box::new(Scratch::new()))
    }
}

/// `slot` holds [`Cgroup::untracked`]`(state)`, written in place (the
/// record is about 2.4 KiB; through a value it went by the stack of the
/// thread that discovers). Trusted: the bytes are the constructor's, an
/// all-zero record with the state and the `UNLIMITED` limits stored (the
/// test `untracked_in_place_is_untracked` compares the fields).
#[verifier::external_body]
#[inline(never)]
pub fn untracked_in_place(slot: &mut MaybeUninit<Cgroup>, state: State)
    ensures
        final(slot).mem_contents().is_init(),
        final(slot).mem_contents().value().layout() == (0nat, 0nat),
{
    let p = slot.as_mut_ptr();
    // SAFETY: `p` is the slot's storage, writable and unaliased; zero is a
    // valid byte pattern for every field (integers, `false`, atomics) but
    // `state`, written next.
    unsafe {
        core::ptr::write_bytes(p, 0, 1);
        core::ptr::addr_of_mut!((*p).state).write(state);
        (*p).limit.store(UNLIMITED, Ordering::Relaxed);
        (*p).high.store(UNLIMITED, Ordering::Relaxed);
        for l in (*p).level_limit.iter() {
            l.store(UNLIMITED, Ordering::Relaxed);
        }
    }
}

impl Cgroup {
    /// The directory leaves room for `/`, a file name and the NUL, and
    /// the mount is a prefix of it.
    #[verifier::type_invariant]
    spec fn wf(&self) -> bool {
        self.dir_len as int + FILE_ROOM <= PATH_CAP && self.mount_len <= self.dir_len
    }

    /// The lengths of the directory and of the mount within it.
    pub closed spec fn layout(&self) -> (nat, nat) {
        (self.dir_len as nat, self.mount_len as nat)
    }

    /// Nothing tracked, for `cgroup:false` and hosts without cgroup2.
    pub const fn untracked(state: State) -> (r: Cgroup)
        ensures
            r.layout() == (0nat, 0nat),
    {
        Cgroup {
            dir: [0; PATH_CAP],
            dir_len: 0,
            mount_len: 0,
            state,
            has_limit_files: false,
            has_current: false,
            has_events: false,
            limit: AtomicU64::new(UNLIMITED),
            high: AtomicU64::new(UNLIMITED),
            current: AtomicU64::new(0),
            // `[const { AtomicU64::new(UNLIMITED) }; LEVELS]` and
            // `[const { AtomicU64::new(0) }; LEVELS]`, spelled out: Verus has
            // no array-fill expression of a type that is not `Copy`.
            level_limit: [
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
                AtomicU64::new(UNLIMITED),
            ],
            level_current: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            current_stale: AtomicBool::new(false),
            sampled_at: AtomicU64::new(0),
            limits_at: AtomicU64::new(0),
            events_low: AtomicU64::new(0),
            events_high: AtomicU64::new(0),
            events_max: AtomicU64::new(0),
            events_oom: AtomicU64::new(0),
            events_oom_kill: AtomicU64::new(0),
            high_rises: AtomicU64::new(0),
            max_rises: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> State {
        self.state
    }

    /// Reads work and the figures are live.
    #[inline]
    pub fn tracked(&self) -> bool {
        self.state == State::Tracked
    }

    /// The cgroup directory (empty when untracked).
    pub fn dir(&self) -> (r: &[u8])
        ensures
            r@.len() == self.layout().0,
            r@.len() + FILE_ROOM <= PATH_CAP,
    {
        proof {
            use_type_invariant(self);
        }
        &self.dir[..self.dir_len as usize]
    }

    /// The cgroup2 mount point (empty when untracked).
    pub fn mount(&self) -> &[u8] {
        proof {
            use_type_invariant(self);
        }
        &self.dir[..self.mount_len as usize]
    }

    /// The path relative to the mount, as `/proc/self/cgroup` gave it
    /// (`/` at the mount itself).
    pub fn path(&self) -> &[u8] {
        proof {
            use_type_invariant(self);
        }
        let p = &self.dir[self.mount_len as usize..self.dir_len as usize];
        if p.is_empty() { b"/" } else { p }
    }

    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    pub fn high(&self) -> u64 {
        self.high.load(Ordering::Relaxed)
    }

    pub fn current(&self) -> u64 {
        self.current.load(Ordering::Relaxed)
    }

    /// `(limit, current)` of the ancestor `level` levels up (the parent at
    /// 1, below [`LEVELS`]): its own limit ([`UNLIMITED`] for none) and its
    /// own usage as last sampled. The leaf is not among them: its own
    /// limit is never below the effective one, against the same usage.
    /// One ancestor at a time rather than an iterator over them, which
    /// Verus does not specify.
    pub fn ancestor(&self, level: usize) -> (u64, u64)
        requires
            level < LEVELS,
    {
        (
            self.level_limit[level].load(Ordering::Relaxed),
            self.level_current[level].load(Ordering::Relaxed),
        )
    }

    /// The last `memory.events` counters sampled, every field the file
    /// has (zero before the first sample, and for a line the file lacks).
    pub fn events(&self) -> Events {
        Events {
            low: self.events_low.load(Ordering::Relaxed),
            high: self.events_high.load(Ordering::Relaxed),
            max: self.events_max.load(Ordering::Relaxed),
            oom: self.events_oom.load(Ordering::Relaxed),
            oom_kill: self.events_oom_kill.load(Ordering::Relaxed),
        }
    }

    /// Record one read of `memory.events`: store all five counters and
    /// count a sample that saw `high` or `max` rise. The sampler's store,
    /// split out so the tests can drive it with fixtures.
    fn note_events(&self, e: Events) {
        self.events_low.store(e.low, Ordering::Relaxed);
        self.events_oom.store(e.oom, Ordering::Relaxed);
        self.events_oom_kill.store(e.oom_kill, Ordering::Relaxed);
        if e.high > self.events_high.swap(e.high, Ordering::Relaxed) {
            self.high_rises.fetch_add(1, Ordering::Relaxed);
        }
        if e.max > self.events_max.swap(e.max, Ordering::Relaxed) {
            self.max_rises.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Samples that saw `memory.events` `high` and `max` rise.
    pub fn rises(&self) -> (u64, u64) {
        (
            self.high_rises.load(Ordering::Relaxed),
            self.max_rises.load(Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    pub fn has_limit_files(&self) -> bool {
        self.has_limit_files
    }

    #[cfg(test)]
    pub fn has_current(&self) -> bool {
        self.has_current
    }

    #[cfg(test)]
    pub fn has_events(&self) -> bool {
        self.has_events
    }

    /// Coarse time of the last `memory.current` sample (0 before the first).
    #[cfg(test)]
    pub fn sampled_at(&self) -> u64 {
        self.sampled_at.load(Ordering::Relaxed)
    }

    /// A tracked cgroup at `mount` joined with `path`, every file present,
    /// for the tests that drive the samplers with captured files.
    #[cfg(test)]
    pub fn at(mount: &[u8], path: &[u8]) -> Cgroup {
        let mut cg = Cgroup::untracked(State::Tracked);
        let (dir_len, mount_len) = join_dir(&mut cg.dir, mount, path).unwrap();
        cg.dir_len = dir_len as u16;
        cg.mount_len = mount_len as u16;
        cg.has_limit_files = true;
        cg.has_current = true;
        cg.has_events = true;
        cg
    }
}

/// A path shown as text, one `?` per byte that is not printable ASCII, for
/// the `cgroup:` line. (This type, [`unsafe_char`] and [`Limit`] are items
/// of the module rather than of `Cgroup::fmt`'s body, where Verus does
/// not see them.)
struct PathBytes<'a>(&'a [u8]);

// Any bytes format (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for PathBytes<'_> {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for PathBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            f.write_str(if b == b' ' || b.is_ascii_graphic() {
                unsafe_char(b)
            } else {
                "?"
            })?;
        }
        Ok(())
    }
}

/// The printable ASCII bytes, `' '..='~'`, in order. Computed rather than
/// written as a byte-string literal, whose bytes Verus does not see.
const fn printable_ascii() -> (r: [u8; 95])
    ensures
        forall|i: int| 0 <= i < 95 ==> #[trigger] r[i] == 0x20 + i,
{
    let mut t = [0u8; 95];
    let mut i = 0;
    while i < 95
        invariant
            i <= 95,
            forall|j: int| 0 <= j < i ==> #[trigger] t[j] == 0x20 + j,
        decreases 95 - i,
    {
        t[i] = 0x20 + i as u8;
        i += 1;
    }
    t
}

/// [`printable_ascii`]'s table, for [`unsafe_char`].
exec const PRINTABLE: [u8; 95]
    ensures
        forall|i: int| 0 <= i < 95 ==> #[trigger] PRINTABLE[i] == 0x20 + i,
{
    printable_ascii()
}

/// One printable ASCII byte as a `&'static str`.
fn unsafe_char(b: u8) -> &'static str
    requires
        0x20 <= b <= 0x7e,
{
    let table: &'static [u8; 95] = &PRINTABLE;
    let i = (b - b' ') as usize;
    proof {
        let s = table@.subrange(i as int, i + 1);
        assert(s =~= seq![b]);
        reveal_with_fuel(vstd::utf8::valid_utf8, 2);
        assert(vstd::utf8::valid_utf8(s));
    }
    // SAFETY: `PRINTABLE` is ASCII, so every one-byte window is UTF-8.
    unsafe { str::from_utf8_unchecked(&table[i..i + 1]) }
}

/// A limit, `max` when unlimited, for the `cgroup:` line.
struct Limit(u64);

// Any limit formats (ghost only).
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Limit {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == UNLIMITED {
            f.write_str("max")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

// A `Cgroup` formats under any formatter, for the `cgroup:` line of the
// report (`api/print.rs`). Ghost only.
#[cfg(verus_keep_ghost)]
impl vstd::std_specs::fmt::DisplaySpecImpl for Cgroup {
    open spec fn fmt_req(&self, f: &fmt::Formatter<'_>) -> bool {
        true
    }
}

impl fmt::Display for Cgroup {
    /// The `cgroup:` line of `malloc_stats_print`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state {
            State::Disabled => return f.write_str("cgroup: not tracked (cgroup:false)"),
            State::NoUnifiedPath => {
                return f
                    .write_str("cgroup: not tracked (no unified hierarchy in /proc/self/cgroup)");
            }
            State::NoMount => return f.write_str("cgroup: not tracked (no cgroup2 mount)"),
            State::PathTooLong => return f.write_str("cgroup: not tracked (path too long)"),
            State::Tracked => {}
        }
        let (hr, mr) = self.rises();
        write!(
            f,
            "cgroup: {} (mount {}, path {}) limit {} high {} current {}{} events high {} (+{}) max {} (+{})",
            PathBytes(self.dir()),
            PathBytes(self.mount()),
            PathBytes(self.path()),
            Limit(self.limit()),
            Limit(self.high()),
            self.current(),
            if self.current_stale.load(Ordering::Relaxed) {
                " (stale)"
            } else {
                ""
            },
            self.events().high,
            hr,
            self.events().max,
            mr,
        )
    }
}

/// The process's [`Cgroup`], discovered late (design 11): at init only
/// when something needs the limit then (`thp:always`, which a limit under
/// 1 GiB forces to `never`), otherwise at the first periodic tick
/// [`DEFER_NS`] after init ([`Deferred::due`]), at the first read of its
/// figures or at the first `fork` after that (`api::fork_prepare`),
/// whichever comes first. The record is discovered in place in the cell
/// ([`Deferred::cell_discover`]). Until then [`Deferred::get`] is `None`: no limit known, the
/// policy at its defaults, the eager purge advice, nothing sampled, the
/// same as a host without cgroup2.
///
/// One thread discovers (a compare-exchange from `PENDING`) into the
/// cell and publishes it with a release store of `DONE`; readers see the
/// cell only after an acquire load of `DONE` (the discoverer, after its
/// own write), so the record's plain fields are never read while they
/// are written. The phase's invariant
/// ([`PhasePred`]) carries that protocol: it holds the cell's permission
/// while the phase is `PENDING`, hands it to the exchange's winner, and
/// holds it frozen from the `DONE` store on, for every reader.
pub struct Deferred {
    phase: AtomicU8,
    /// Coarse-clock time of init.
    born: u64,
    /// Initialised once `phase` is `DONE`.
    cell: PCell<MaybeUninit<Cgroup>>,
    /// Ghost (no bytes): the phase's invariant ([`PhasePred`]), which
    /// holds the phase's permission and the cell's.
    #[cfg_attr(not(verus_keep_ghost), allow(dead_code))]
    phase_inv: Tracked<PhaseInv>,
}

pub const PHASE_DONE: u8 = 0;
pub const PHASE_PENDING: u8 = 1;
pub const PHASE_RUNNING: u8 = 2;

/// The permission to a [`Deferred`] record's cell.
pub type CellPerm = PCellPerm<MaybeUninit<Cgroup>>;

/// `p` is the permission to the cell with identity `id`, and the cell
/// holds a record.
pub open spec fn cell_done(p: CellPerm, id: CellId) -> bool {
    &&& p.id() == id
    &&& p.value().mem_contents() is Init
}

/// Namespace of the phase invariants ([`PhasePred`]).
pub open spec fn phase_ns() -> int {
    0x7068_6173
}

/// What a phase's invariant holds: the phase's permission, the cell's
/// permission while the phase is `PENDING`, and the cell's permission,
/// frozen, from `DONE` on.
pub type PhaseVal = (BytePerm, Option<CellPerm>, Option<&'static CellPerm>);

/// A phase's invariant, keyed by the phase's identity
/// (`vspec::byte::byte_id`) and the cell's: the phase is `DONE`,
/// `PENDING` or `RUNNING`; while it is `PENDING` the invariant holds the
/// cell's permission, and from `DONE` on the cell's permission frozen,
/// the cell holding a record. `Deferred::now` makes it at `DONE` with
/// the record written, `Deferred::later` at `PENDING` with the cell
/// empty. The exchange from `PENDING` to `RUNNING`
/// ([`Deferred::claim`]) takes the cell's permission out, the `DONE`
/// store ([`Deferred::publish`]) puts it back frozen, after the winner's
/// write, and the fork child's reset from `RUNNING` to `PENDING`
/// ([`Deferred::fork_child`]) puts back the permission that
/// [`axiom_cell_orphaned`] hands it: nothing else writes the phase
/// (`vspec::byte`), so `DONE` is never left.
pub struct PhasePred;

impl InvariantPredicate<(int, CellId), PhaseVal> for PhasePred {
    open spec fn inv(k: (int, CellId), v: PhaseVal) -> bool {
        &&& v.0.id() == k.0
        &&& v.0.value() == PHASE_DONE || v.0.value() == PHASE_PENDING || v.0.value() == PHASE_RUNNING
        &&& v.0.value() == PHASE_PENDING ==> (v.1 matches Some(p) && p.id() == k.1)
        &&& v.0.value() == PHASE_DONE ==> (v.2 matches Some(p) && cell_done(*p, k.1))
    }
}

/// The invariant of a deferred record's phase.
pub type PhaseInv = AtomicInvariant<(int, CellId), PhaseVal, PhasePred>;

/// The cell's frozen permission a thread has seen, if any: what
/// [`Deferred::load_phase`] hands back when it reads `DONE`.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct CellSeen {
    pub tracked frozen: Option<&'static CellPerm>,
}

/// The cell's permission a thread has claimed, if any: what
/// [`Deferred::claim`] hands its winner.
// Ghost: built and read only by proof code.
#[allow(dead_code)]
pub tracked struct CellClaim {
    pub tracked cell: Option<CellPerm>,
}

/// The caller is the child of a `fork`, in the child's fork handler
/// (`api::fork_child`) or in the child's fork window before it
/// (`lock::in_child_fork_window`: a wait for a discovery or a feature
/// line another parent thread had under way, design 9), where only the
/// forking thread runs: what
/// [`Deferred::fork_child`] and [`axiom_cell_orphaned`] require, so that
/// no other verified code can reset a phase and take the cell's
/// permission. Opaque, with no meaning: a capability, as
/// `sys::may_syscall` is.
#[verifier::opaque]
pub open spec fn in_fork_child() -> bool {
    true
}

/// Premise (the fork child's orphaned cell): in the child of a `fork`,
/// a record whose phase the child's reset found `RUNNING` (`before`, the
/// phase's permission just before the reset, and `perm`, just after it,
/// at `PENDING`) was being discovered by a thread of the parent, which
/// holds the cell's permission between its winning exchange and its
/// `DONE` store ([`Deferred::claim`], [`Deferred::publish`]). The child
/// has only the forking thread, so when the discoverer was another
/// thread, no thread of the child holds that permission, and the reset
/// takes it (`before` and [`in_fork_child`] are the caller's word).
/// Called only by [`Deferred::fork_child`], right after its
/// successful reset, which only the child runs: its fork handler
/// (`api::fork_child`, through `Shared::cgroup_fork_child`), or
/// [`Deferred::discover`] in the child's fork window
/// (`lock::in_child_fork_window`), where the discoverer it would wait for
/// is not in the child (this module's unit tests, unverified, also call
/// it on records of their own).
///
/// The residue this does not cover: when the discoverer was the forking
/// thread itself (a signal handler that forked while its thread was
/// discovering), the permission exists twice in the child, the reset's
/// and the interrupted discovery's. The child may then discover again
/// and publish `DONE`, and the interrupted discovery, resumed when the
/// handler returns, writes the cell a second time and stores `DONE`
/// again: a record a reader of the first `DONE` may hold a reference
/// into is overwritten with a second discovery of the same cgroup. The
/// two discoveries run one after the other when both are the forking
/// thread's; if the child has started another thread by then (the
/// handler allocates and starts the lazy background thread, whose tick
/// discovers, say), they can run concurrently, a data race on the cell.
axiom fn axiom_cell_orphaned(d: &Deferred, before: BytePerm, tracked perm: &BytePerm) -> (tracked p: CellPerm)
    requires
        in_fork_child(),
        before.id() == byte_id(&d.phase),
        before.value() == PHASE_RUNNING,
        perm.id() == before.id(),
        perm.value() == PHASE_PENDING,
    ensures
        p.id() == d.cell.id(),
;

impl Deferred {
    /// The phase's invariant is the one over this record's phase and
    /// cell, in [`phase_ns`].
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        &&& self.phase_inv@.constant() == (byte_id(&self.phase), self.cell.id())
        &&& self.phase_inv@.namespace() == phase_ns()
    }

    /// A record discovered now (`Cgroup::discover`).
    pub fn now(cg: Cgroup) -> Deferred {
        let (phase, Tracked(pperm)) = crate::vspec::byte::new(PHASE_DONE);
        let (cell, Tracked(cperm)) = PCell::new(MaybeUninit::new(cg));
        let tracked frozen = vstd::modes::tracked_static_ref(cperm);
        let tracked phase_inv = PhaseInv::new((byte_id(&phase), cell.id()), (pperm, None, Some(frozen)), phase_ns());
        Deferred {
            phase,
            born: 0,
            cell,
            phase_inv: Tracked(phase_inv),
        }
    }

    /// A record to discover later; `born` is init's coarse-clock time.
    pub const fn later(born: u64) -> Deferred {
        let (phase, Tracked(pperm)) = crate::vspec::byte::new(PHASE_PENDING);
        let (cell, Tracked(cperm)) = PCell::new(MaybeUninit::uninit());
        let tracked phase_inv = PhaseInv::new((byte_id(&phase), cell.id()), (pperm, Some(cperm), None), phase_ns());
        Deferred {
            phase,
            born,
            cell,
            phase_inv: Tracked(phase_inv),
        }
    }

    /// The record once discovery has run, `None` before.
    #[inline]
    pub fn get(&self) -> Option<&Cgroup> {
        let tracked mut seen = CellSeen { frozen: None };
        if self.load_phase(Tracked(&mut seen)) == PHASE_DONE {
            // `DONE` was published after the cell was written: its
            // permission, frozen then.
            Some(self.cell_ref(Tracked(seen.frozen.tracked_unwrap())))
        } else {
            None
        }
    }

    /// Whether discovery is still to come and `now` is at least
    /// [`DEFER_NS`] after init.
    #[inline]
    pub fn due(&self, now: u64) -> bool {
        self.phase.load(Ordering::Relaxed) == PHASE_PENDING
            && now.saturating_sub(self.born) >= DEFER_NS
    }

    /// Run discovery if it has not run: `true` when this call ran it, so
    /// the caller sets the policy up from the first figures. `found` sees
    /// the record before it is published, so what it sets up (the policy's
    /// `high` baseline) is in place before any other thread's sample can
    /// act on the record. A call that finds another thread discovering
    /// waits for it when `wait` (termination is not claimed: the wait ends
    /// when the other thread's discovery does), and otherwise returns at
    /// once with the record still pending.
    ///
    /// `seen` gets the cell's frozen permission whenever `wait`: from this
    /// call's `DONE` store, or from the acquire load of `DONE` that ended
    /// its wait. [`Deferred::published`] reads the record through it.
    #[verifier::exec_allows_no_decreases_clause]
    pub fn discover<F: FnOnce(&Cgroup)>(&self, wait: bool, found: F, Tracked(seen): Tracked<&mut CellSeen>) -> bool
        requires
            crate::sys::may_syscall(),
            forall|cg: &Cgroup| #[trigger] call_requires(found, (cg,)),
        ensures
            wait ==> self.seen_done(*final(seen)),
    {
        let tracked mut claimed = CellClaim { cell: None };
        match self.claim(Tracked(&mut claimed)) {
            Ok(_) => {
                let tracked mut cell = claimed.cell.tracked_unwrap();
                // This thread won the exchange, so it holds the cell's
                // permission and the discovery scratch; no reader looks at
                // the cell before `DONE`. The record is built in the cell,
                // never on this thread's stack (design 11: any thread's
                // `malloc` may run this).
                let cg = self.cell_discover(Tracked(&mut cell));
                found(cg);
                let Tracked(frozen) = self.publish(Tracked(cell));
                proof {
                    seen.frozen = Some(frozen);
                }
                true
            },
            Err(_) => {
                if !wait {
                    return false;
                }
                loop
                    invariant
                        crate::sys::may_syscall(),
                        wait,
                        forall|cg: &Cgroup| #[trigger] call_requires(found, (cg,)),
                {
                    if self.load_phase(Tracked(seen)) == PHASE_DONE {
                        return false;
                    }
                    // A fork child's handler in the fork window: the
                    // discovering thread is not in the child, and the wait
                    // would never end. Reset as the child handler would,
                    // and discover here.
                    if crate::lock::in_child_fork_window() {
                        self.fork_child();
                        return self.discover(wait, found, Tracked(seen));
                    }
                    core::hint::spin_loop();
                }
            },
        }
    }

    /// `seen` holds this record's cell's frozen permission, with a record
    /// in the cell: the record is published.
    pub closed spec fn seen_done(&self, seen: CellSeen) -> bool {
        seen.frozen matches Some(p) && cell_done(*p, self.cell.id())
    }

    /// The published record, read through the cell's frozen permission
    /// that a waiting [`Deferred::discover`] handed back; no load of the
    /// phase.
    #[inline]
    pub fn published(&self, Tracked(seen): Tracked<CellSeen>) -> &Cgroup
        requires
            self.seen_done(seen),
    {
        self.cell_ref(Tracked(seen.frozen.tracked_unwrap()))
    }

    /// The child of a `fork` has only the forking thread: a discovery
    /// another thread had under way is not coming, so it starts over.
    /// The reset puts back the cell's permission, which that thread held
    /// ([`axiom_cell_orphaned`]).
    pub fn fork_child(&self)
        requires
            in_fork_child(),
    {
        proof {
            use_type_invariant(self);
        }
        vstd::open_atomic_invariant!(self.phase_inv.borrow() => v => {
            let ghost before = v.0;
            let _reset = crate::vspec::byte::compare_exchange(&self.phase, PHASE_RUNNING, PHASE_PENDING, Ordering::Relaxed, Ordering::Relaxed, Tracked(&mut v.0));
            proof {
                if _reset is Ok {
                    v.1 = Some(axiom_cell_orphaned(self, before, &v.0));
                }
            }
        });
    }

    /// An acquire load of the phase, with the cell's frozen permission
    /// when it reads `DONE` ([`PhasePred`]).
    #[inline(always)]
    fn load_phase(&self, Tracked(seen): Tracked<&mut CellSeen>) -> (r: u8)
        ensures
            r == PHASE_DONE ==> (final(seen).frozen matches Some(p) && cell_done(*p, self.cell.id())),
    {
        proof {
            use_type_invariant(self);
        }
        let r;
        vstd::open_atomic_invariant!(self.phase_inv.borrow() => v => {
            r = crate::vspec::byte::load(&self.phase, Ordering::Acquire, Tracked(&v.0));
            proof {
                if r == PHASE_DONE {
                    seen.frozen = v.2;
                }
            }
        });
        r
    }

    /// The phase's exchange from `PENDING` to `RUNNING`: the winner gets
    /// the cell's permission (in `claimed`), which the invariant holds
    /// while the phase is `PENDING` ([`PhasePred`]).
    #[inline(always)]
    fn claim(&self, Tracked(claimed): Tracked<&mut CellClaim>) -> (r: Result<u8, u8>)
        ensures
            r is Ok ==> (final(claimed).cell matches Some(p) && p.id() == self.cell.id()),
    {
        proof {
            use_type_invariant(self);
        }
        let won;
        vstd::open_atomic_invariant!(self.phase_inv.borrow() => v => {
            won = crate::vspec::byte::compare_exchange(&self.phase, PHASE_PENDING, PHASE_RUNNING, Ordering::Acquire, Ordering::Relaxed, Tracked(&mut v.0));
            proof {
                if won is Ok {
                    claimed.cell = Some(v.1.tracked_take());
                }
            }
        });
        won
    }

    /// The phase's `DONE` store, `Release`, with the cell's permission
    /// frozen into the invariant for every reader ([`PhasePred`]); the
    /// frozen permission is handed back to the publisher too.
    #[inline(always)]
    fn publish(&self, Tracked(cell): Tracked<CellPerm>) -> (r: Tracked<&'static CellPerm>)
        requires
            cell_done(cell, self.cell.id()),
        ensures
            *r@ == cell,
    {
        proof {
            use_type_invariant(self);
        }
        let tracked frozen = vstd::modes::tracked_static_ref(cell);
        vstd::open_atomic_invariant!(self.phase_inv.borrow() => v => {
            crate::vspec::byte::store(&self.phase, PHASE_DONE, Ordering::Release, Tracked(&mut v.0));
            proof {
                v.2 = Some(frozen);
            }
        });
        Tracked(frozen)
    }

    /// The discovered record in the cell, through its permission.
    #[inline(always)]
    fn cell_ref<'a>(&'a self, Tracked(p): Tracked<&'a CellPerm>) -> &'a Cgroup
        requires
            cell_done(*p, self.cell.id()),
        opens_invariants none
    {
        // SAFETY: the permission says the cell holds a record.
        unsafe { self.cell.borrow(Tracked(p)).assume_init_ref() }
    }

    /// The record discovered into the cell, in place, through its
    /// permission, which [`Deferred::get`] reads once `DONE` is published:
    /// the cell first gets an untracked record ([`untracked_in_place`]),
    /// which [`Cgroup::discover_in`] fills, with the discovery scratch
    /// ([`discovery_scratch`]) the winner of the exchange owns.
    #[inline(never)]
    fn cell_discover<'a>(&'a self, Tracked(p): Tracked<&'a mut CellPerm>) -> &'a Cgroup
        requires
            crate::sys::may_syscall(),
            old(p).id() == self.cell.id(),
        ensures
            cell_done(*final(p), self.cell.id()),
    {
        let scratch = discovery_scratch(Tracked(&*p));
        let slot = self.cell.borrow_mut(Tracked(p));
        untracked_in_place(slot, State::NoUnifiedPath);
        // SAFETY: `untracked_in_place` initialised it.
        let cg = unsafe { slot.assume_init_mut() };
        cg.discover_in(true, scratch);
        &*cg
    }
}

} // verus!

// SAFETY: the cell is written once, by the thread that won the
// `PENDING -> RUNNING` exchange, before the release store of `DONE`, and
// read only after an acquire load of `DONE` (the type doc); everything
// else in `Cgroup` is atomic.
unsafe impl Sync for Deferred {}

mod linux {
    use super::*;
    use crate::sys::{self, O_RDONLY, read_small_file};

    verus! {

    /// `/proc/self/cgroup`, NUL-terminated (spelled as bytes so the
    /// verifier sees the NUL `sys::open` requires).
    const PROC_SELF_CGROUP: [u8; 18] = [
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'e', b'l', b'f', b'/', b'c', b'g', b'r', b'o',
        b'u', b'p', 0,
    ];

    /// `/proc/self/mountinfo`, NUL-terminated.
    const PROC_SELF_MOUNTINFO: [u8; 21] = [
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'e', b'l', b'f', b'/', b'm', b'o', b'u', b'n',
        b't', b'i', b'n', b'f', b'o', 0,
    ];

    /// `dir/name`, NUL-terminated, in `out`.
    fn file_path<'a>(out: &'a mut [u8; PATH_CAP + FILE_ROOM], dir: &[u8], name: &[u8]) -> (r: &'a [u8])
        requires
            dir@.len() + name@.len() + 2 <= PATH_CAP + FILE_ROOM,
        ensures
            r@.len() == dir@.len() + name@.len() + 2,
            r@.last() == 0,
    {
        let n = dir.len();
        out[..n].copy_from_slice(dir);
        out[n] = b'/';
        out[n + 1..n + 1 + name.len()].copy_from_slice(name);
        out[n + 1 + name.len()] = 0;
        &out[..n + name.len() + 2]
    }

    /// Read `dir/name` into `buf`; `None` when absent.
    fn read_in(dir: &[u8], name: &[u8], buf: &mut [u8]) -> (r: Option<usize>)
        requires
            crate::sys::may_syscall(),
            dir@.len() + FILE_ROOM <= PATH_CAP,
            name@.len() + 2 <= FILE_ROOM,
        ensures
            r matches Some(n) ==> n <= old(buf)@.len(),
            final(buf)@.len() == old(buf)@.len(),
    {
        let mut p = [0u8; PATH_CAP + FILE_ROOM];
        let path = file_path(&mut p, dir, name);
        read_small_file(path, buf)
    }

    /// What [`read_in`] promises of a read, as the samplers need it of any
    /// reader they take (`read_in`, or a test's captured files): callable
    /// for a directory and a file name that fit [`file_path`], and a count
    /// within the buffer.
    pub open spec fn reader_ok<R: FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>>(read: R) -> bool {
        &&& forall|d: &[u8], n: &[u8], b: &mut [u8]|
            d@.len() + FILE_ROOM <= PATH_CAP && n@.len() + 2 <= FILE_ROOM ==> #[trigger] call_requires(read, (d, n, b))
        &&& forall|d: &[u8], n: &[u8], b: &mut [u8], r: Option<usize>|
            #[trigger] call_ensures(read, (d, n, b), r) ==> (r matches Some(k) ==> k <= b@.len())
    }

    /// [`read_in`] as the samplers' reader ([`reader_ok`]), where syscalls
    /// are allowed. (A closure over it rather than the function item
    /// `read_in` itself, whose `call_requires` the verifier does not
    /// carry into a `&mut` argument.)
    #[inline(always)]
    fn reader() -> (r: impl FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>)
        requires
            crate::sys::may_syscall(),
        ensures
            reader_ok(r),
    {
        |d: &[u8], n: &[u8], b: &mut [u8]| -> (r: Option<usize>)
            requires
                crate::sys::may_syscall(),
                d@.len() + FILE_ROOM <= PATH_CAP,
                n@.len() + 2 <= FILE_ROOM,
            ensures
                r matches Some(k) ==> k <= old(b)@.len(),
            { read_in(d, n, b) }
    }

    /// `dir/name` parsed as a limit file through `read` (`read_in`, or a
    /// test's captured files); `None` when absent or malformed.
    fn read_limit<R: FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>>(
        read: &mut R,
        dir: &[u8],
        name: &[u8],
    ) -> Option<u64>
        requires
            reader_ok(*old(read)),
            dir@.len() + FILE_ROOM <= PATH_CAP,
            name@.len() + 2 <= FILE_ROOM,
        ensures
            *final(read) == *old(read),
    {
        let mut buf = [0u8; 64];
        let n = read(dir, name, &mut buf)?;
        parse_limit(&buf[..n])
    }

    /// The minimum of the limit file `name` over the levels of the walk of
    /// `dir`, each level within [`LEVELS`] also lowering its own entry of
    /// `own`. A loop over [`Levels`] rather than [`walk_min`] with a closure:
    /// the closure would capture `read` and `own` by mutable reference, which
    /// Verus does not support.
    // No let chain (`if level < LEVELS && let Some(v) = v`): Verus does not
    // take one.
    #[allow(clippy::collapsible_if)]
    fn own_min<R: FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>>(
        dir: &[u8],
        mount_len: usize,
        read: &mut R,
        own: &mut [u64; LEVELS],
        name: &[u8],
    ) -> u64
        requires
            reader_ok(*old(read)),
            dir@.len() + FILE_ROOM <= PATH_CAP,
            name@.len() + 2 <= FILE_ROOM,
        ensures
            *final(read) == *old(read),
    {
        let mut min = UNLIMITED;
        let mut levels = Levels::new(dir, mount_len);
        while let Some((level, d)) = levels.next_level()
            invariant
                levels.wf(),
                levels.dir == dir,
                dir@.len() + FILE_ROOM <= PATH_CAP,
                name@.len() + 2 <= FILE_ROOM,
                *read == *old(read),
                reader_ok(*read),
            decreases levels.rest(),
        {
            let v = read_limit(read, d, name);
            // `if let (Some(o), Some(v)) = (own.get_mut(level), v)`,
            // spelled out.
            if level < LEVELS {
                if let Some(v) = v {
                    own[level] = own[level].min(v);
                }
            }
            if let Some(v) = v {
                min = min.min(v);
            }
        }
        min
    }

    /// The first cgroup2 mount point in `/proc/self/mountinfo`, copied into
    /// `mount`, and its length: 0 when there is none or it is longer than
    /// [`PATH_CAP`]. The scan stops at the first line whose mount fits
    /// and leaves it in the scan buffer, where it is parsed again for the
    /// copy (the line closure does not write `mount`: Verus has no
    /// closures that capture by mutable reference).
    fn mountinfo_cgroup2(mount: &mut [u8; PATH_CAP], scan: &mut [u8; SCAN_BUF]) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            r <= PATH_CAP,
    {
        let Ok(fd) = sys::open(&PROC_SELF_MOUNTINFO, O_RDONLY) else {
            return 0;
        };
        let found = scan_lines(
            scan,
            |b: &mut [u8]| -> (r: Option<usize>)
                ensures
                    r matches Some(n) ==> n <= old(b)@.len(),
                    final(b)@.len() == old(b)@.len(),
                { sys::read_full(fd, b).ok() },
            |line: &[u8]| -> bool {
                match cgroup2_mount_from_line(line) {
                    Some(m) => m.len() <= PATH_CAP,
                    None => false,
                }
            },
        );
        let _ = sys::close(fd);
        let Some((lo, hi)) = found else { return 0 };
        match cgroup2_mount_from_line(&scan[lo..hi]) {
            Some(m) if m.len() <= PATH_CAP => {
                mount[..m.len()].copy_from_slice(m);
                m.len()
            }
            _ => 0,
        }
    }

    impl Cgroup {
        /// Design 10 step 7: find the cgroup and read its limits once.
        /// With `enabled == false` (`cgroup:false`) nothing is opened. By
        /// value: init's (under `thp:always`, and `cgroup:false`'s
        /// untracked record) and the tests'; the deferred discovery builds
        /// the record in place ([`Cgroup::discover_in`]). Only an enabled
        /// discovery reaches [`Cgroup::discover_scanned`] and its scratch
        /// on the stack.
        pub fn discover(enabled: bool) -> Cgroup
            requires
                crate::sys::may_syscall(),
        {
            if !enabled {
                return Cgroup::untracked(State::Disabled);
            }
            Cgroup::discover_scanned()
        }

        /// An enabled [`Cgroup::discover`], with the scan and mount buffers
        /// on its own stack (about 8.6 KiB). Out of line: init calls it
        /// only under `thp:always`.
        #[inline(never)]
        fn discover_scanned() -> Cgroup
            requires
                crate::sys::may_syscall(),
        {
            let enabled = true;
            let mut cg = Cgroup::untracked(State::NoUnifiedPath);
            let mut scratch = Scratch::new();
            cg.discover_in(enabled, &mut scratch);
            cg
        }

        /// [`Cgroup::discover`] into `self`, an untracked record (its
        /// lengths 0), with the scan and mount buffers in `scratch`.
        pub fn discover_in(&mut self, enabled: bool, scratch: &mut Scratch)
            requires
                crate::sys::may_syscall(),
                old(self).layout() == (0nat, 0nat),
        {
            let cg = self;
            if !enabled {
                cg.state = State::Disabled;
                return;
            }
            cg.state = State::NoUnifiedPath;
            proof {
                use_type_invariant(&*cg);
            }
            let scan = &mut scratch.cgroup_scan;
            let Ok(fd) = sys::open(&PROC_SELF_CGROUP, O_RDONLY) else {
                return;
            };
            let path = proc_cgroup_path(
                scan,
                |b: &mut [u8]| -> (r: Option<usize>)
                    ensures
                        r matches Some(n) ==> n <= old(b)@.len(),
                        final(b)@.len() == old(b)@.len(),
                    { sys::read_full(fd, b).ok() },
            );
            let _ = sys::close(fd);
            let Some(path) = path else {
                return;
            };

            let mount = &mut scratch.mount;
            let mount_len = mountinfo_cgroup2(mount, &mut scratch.mount_scan);
            if mount_len == 0 {
                cg.state = State::NoMount;
                return;
            }
            proof {
                use_type_invariant(&*cg);
            }
            let Some((dir_len, mlen)) = join_dir(&mut cg.dir, &mount[..mount_len], path) else {
                cg.state = State::PathTooLong;
                return;
            };
            cg.dir_len = dir_len as u16;
            cg.mount_len = mlen as u16;
            cg.state = State::Tracked;

            let dir = &cg.dir[..dir_len];
            // The scan buffer is free again (`path` was copied into
            // `cg.dir`).
            let scan = &mut scratch.cgroup_scan;
            cg.has_limit_files = read_in(dir, b"memory.max", scan).is_some();
            cg.has_current = read_in(dir, b"memory.current", scan).is_some();
            cg.has_events = read_in(dir, b"memory.events", scan).is_some();
            let now = sys::now_coarse_ns();
            cg.walk_limits(now);
            cg.sample_now(now);
        }

        fn walk_limits(&self, now: u64)
            requires
                crate::sys::may_syscall(),
        {
            self.walk_limits_with(now, &mut reader());
        }

        fn sample_now(&self, now: u64)
            requires
                crate::sys::may_syscall(),
        {
            self.sample_now_with(now, &mut reader());
        }

        /// The effective limits: the minimum of `memory.max` and of
        /// `memory.high` over the levels from the leaf to the mount; and
        /// each of the first [`LEVELS`] levels' own `min(max, high)`.
        /// `read` reads one file (`read_in`, or a test's captured files).
        pub(super) fn walk_limits_with<R: FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>>(
            &self,
            now: u64,
            read: &mut R,
        )
            requires
                reader_ok(*old(read)),
            ensures
                *final(read) == *old(read),
        {
            self.limits_at.store(now, Ordering::Relaxed);
            let (dir, mlen) = (self.dir(), self.mount_len as usize);
            let mut own = [UNLIMITED; LEVELS];
            self.limit.store(
                own_min(dir, mlen, read, &mut own, b"memory.max"),
                Ordering::Relaxed,
            );
            self.high.store(
                own_min(dir, mlen, read, &mut own, b"memory.high"),
                Ordering::Relaxed,
            );
            // `for (slot, v) in self.level_limit.iter().zip(own)`, spelled
            // out: Verus does not specify `Zip`.
            let mut i = 0;
            while i < LEVELS
                invariant
                    i <= LEVELS,
                decreases LEVELS - i,
            {
                self.level_limit[i].store(own[i], Ordering::Relaxed);
                i += 1;
            }
        }

        /// Read `memory.current` and `memory.events` now, unconditionally,
        /// and `memory.current` of every ancestor in [`LEVELS`] with a
        /// limit of its own; re-walk the limits if [`LIMIT_INTERVAL_NS`]
        /// has passed. `read` as in [`Cgroup::walk_limits_with`].
        /// A failed `memory.current` read keeps the last figure and marks it
        /// stale until a read succeeds.
        // No let chain in the ancestor walk, as in `own_min`.
        #[allow(clippy::collapsible_if)]
        pub(super) fn sample_now_with<R: FnMut(&[u8], &[u8], &mut [u8]) -> Option<usize>>(
            &self,
            now: u64,
            read: &mut R,
        )
            requires
                reader_ok(*old(read)),
            ensures
                *final(read) == *old(read),
        {
            // The walk first, so an ancestor that just gained a limit has
            // its usage read in this same sample.
            if now.saturating_sub(self.limits_at.load(Ordering::Relaxed)) >= LIMIT_INTERVAL_NS {
                self.walk_limits_with(now, read);
            }
            if self.has_current {
                let v = read_limit(read, self.dir(), b"memory.current");
                if let Some(v) = v {
                    self.current.store(v, Ordering::Relaxed);
                }
                self.current_stale.store(v.is_none(), Ordering::Relaxed);
            }
            // The ancestors, a loop over [`Levels`]: a closure would capture
            // `read` by mutable reference, which Verus does not support.
            let dir = self.dir();
            let mut levels = Levels::new(dir, self.mount_len as usize);
            while let Some((level, d)) = levels.next_level()
                invariant
                    levels.wf(),
                    levels.dir == dir,
                    dir@.len() + FILE_ROOM <= PATH_CAP,
                    *read == *old(read),
                    reader_ok(*read),
                decreases levels.rest(),
            {
                if level != 0
                    && level < LEVELS
                    && self.level_limit[level].load(Ordering::Relaxed) != UNLIMITED
                {
                    if let Some(v) = read_limit(read, d, b"memory.current") {
                        self.level_current[level].store(v, Ordering::Relaxed);
                    }
                }
            }
            if self.has_events {
                let mut buf = [0u8; FILE_BUF];
                if let Some(n) = read(self.dir(), b"memory.events", &mut buf) {
                    self.note_events(parse_events(&buf[..n]));
                }
            }
        }

        /// Sample if at least [`SAMPLE_INTERVAL_NS`] of the coarse clock
        /// has passed since the last sample; one thread wins the interval.
        /// A `now` older than the last sample (read before another thread
        /// sampled) is not due, so the window only moves forward. `true`
        /// when this call read the files. Slow paths only.
        pub fn sample(&self, now: u64) -> bool
            requires
                crate::sys::may_syscall(),
        {
            if !self.tracked() {
                return false;
            }
            let last = self.sampled_at.load(Ordering::Relaxed);
            if now.saturating_sub(last) < SAMPLE_INTERVAL_NS {
                return false;
            }
            if self
                .sampled_at
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
            {
                return false;
            }
            self.sample_now(now);
            true
        }
    }

    } // verus!
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod linux_tests;

/// The Kani harness of [`untracked_in_place`], next to the private
/// fields it reads.
#[cfg(kani)]
mod kani_in_place {
    use super::{Cgroup, FILE_ROOM, PATH_CAP, State, UNLIMITED, untracked_in_place};
    use core::mem::{MaybeUninit, size_of};
    use core::sync::atomic::Ordering::Relaxed;

    /// `untracked_in_place`, `ensures final(slot).mem_contents().is_init()`,
    /// `final(slot).mem_contents().value().layout() == (0, 0)`: for every
    /// state, on a slot holding arbitrary bytes, the call is defined (Kani
    /// checks its writes), the slot is then what a read of a `Cgroup`
    /// needs (`kani::mem::can_dereference`: aligned, in bounds, the bytes
    /// a valid value, the state and the `bool`s included), `dir_len` and
    /// `mount_len` (`layout()`) are 0, and so the type invariant `wf`
    /// holds (`dir_len + FILE_ROOM <= PATH_CAP`, `mount_len <= dir_len`).
    /// Beyond the contract: the state is the one passed, and `limit`,
    /// `high` and every level's limit are `UNLIMITED`, as
    /// `Cgroup::untracked` has them.
    #[kani::proof]
    #[kani::unwind(17)]
    fn untracked_in_place_is_untracked() {
        let states = [
            State::Disabled,
            State::NoUnifiedPath,
            State::NoMount,
            State::PathTooLong,
            State::Tracked,
        ];
        let s: usize = kani::any();
        kani::assume(s < states.len());
        let mut slot = MaybeUninit::<Cgroup>::uninit();
        // SAFETY: the slot's storage, of `size_of::<Cgroup>()` bytes; a
        // byte array has alignment 1.
        unsafe {
            slot.as_mut_ptr()
                .cast::<[u8; size_of::<Cgroup>()]>()
                .write(kani::any())
        };
        untracked_in_place(&mut slot, states[s]);
        let p = slot.as_ptr();
        assert!(kani::mem::can_dereference(p));
        // SAFETY: `can_dereference`, checked above.
        let c = unsafe { &*p };
        assert_eq!((c.dir_len, c.mount_len), (0, 0));
        assert!(c.dir_len as usize + FILE_ROOM <= PATH_CAP && c.mount_len <= c.dir_len);
        assert!(c.state == states[s]);
        assert_eq!(
            (c.limit.load(Relaxed), c.high.load(Relaxed)),
            (UNLIMITED, UNLIMITED)
        );
        let l: usize = kani::any();
        kani::assume(l < c.level_limit.len());
        assert_eq!(c.level_limit[l].load(Relaxed), UNLIMITED);
    }
}

/// The Kani harness of [`discovery_scratch`], next to the private function
/// and its static.
#[cfg(kani)]
mod kani_discovery_scratch {
    use super::{DISCOVERY_SCRATCH, Scratch, discovery_scratch};
    use core::mem::size_of;
    use vstd::prelude::Tracked;

    /// `discovery_scratch`: no `ensures`; the call is defined, and the
    /// `&'static mut Scratch` it returns is `DISCOVERY_SCRATCH`
    /// (`core::ptr::eq`), writable (`kani::mem::can_write`), and a byte
    /// written at any offset of it reads back there. The harness runs the
    /// body a build outside `cfg(test)` has. Not checked: that no two
    /// references are live at once, which rests on the process having one
    /// `Deferred` record, whose exchange from `PENDING` to `RUNNING` lets
    /// one thread through to the item's one caller,
    /// `Deferred::cell_discover`, and which fails in the residue
    /// `axiom_cell_orphaned` states (a second discovery in the child of a
    /// `fork` from a signal handler that interrupted one, when the handler
    /// allocates).
    #[kani::proof]
    fn discovery_scratch_is_the_static() {
        let r = discovery_scratch(Tracked::assume_new());
        let p: *mut Scratch = r;
        assert!(core::ptr::eq(p, core::ptr::addr_of_mut!(DISCOVERY_SCRATCH)));
        assert!(kani::mem::can_write(p));
        let (i, v): (usize, u8) = (kani::any(), kani::any());
        kani::assume(i < size_of::<Scratch>());
        // SAFETY: `p` is `r`, the only reference, writable above, and `i`
        // is inside it.
        unsafe { p.cast::<u8>().add(i).write(v) };
        // SAFETY: as above.
        assert_eq!(unsafe { p.cast::<u8>().add(i).read() }, v);
    }
}

/// Kani harness for the `Display` of `Cgroup`, run under the real
/// `core::fmt` (Verus checks it against the `Formatter` specifications of
/// `vspec/fmt.rs`).
#[cfg(kani)]
mod kani_fmt {
    use super::{Cgroup, State};
    use crate::kani_proofs::fmt::formats;
    use core::sync::atomic::Ordering::Relaxed;

    /// The longest directory checked.
    const DIR_MAX: u16 = 3;

    /// `DisplaySpecImpl for sys::cgroup::Cgroup` (`fmt_req` true): every
    /// state, every figure, and every directory of up to `DIR_MAX` bytes
    /// (every byte value) with every mount prefix of it (`mount_len <=
    /// dir_len`, the type invariant that `mount()` and `path()` slice by).
    ///
    /// The bound covers `core::fmt::write`, which loops once per entry of
    /// the format template: 23 for the tracked line (11 pieces, 11
    /// arguments and the end).
    #[kani::proof]
    #[kani::unwind(25)]
    fn display_cgroup() {
        let states = [
            State::Disabled,
            State::NoUnifiedPath,
            State::NoMount,
            State::PathTooLong,
            State::Tracked,
        ];
        let s: usize = kani::any();
        kani::assume(s < states.len());
        let mut c = Cgroup::untracked(states[s]);
        let (dir_len, mount_len): (u16, u16) = (kani::any(), kani::any());
        kani::assume(dir_len <= DIR_MAX && mount_len <= dir_len);
        for k in 0..DIR_MAX as usize {
            c.dir[k] = kani::any();
        }
        c.dir_len = dir_len;
        c.mount_len = mount_len;
        for a in [
            &c.limit,
            &c.high,
            &c.current,
            &c.events_low,
            &c.events_high,
            &c.events_max,
            &c.events_oom,
            &c.events_oom_kill,
            &c.high_rises,
            &c.max_rises,
        ] {
            a.store(kani::any(), Relaxed);
        }
        c.current_stale.store(kani::any(), Relaxed);
        formats(&c);
    }
}
