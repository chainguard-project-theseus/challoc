//! cgroup v2 discovery and sampling (design.md section 11).
//!
//! This is the measurement layer only: where the process's cgroup is, what
//! its effective memory limits are, how much it is charged right now, and
//! whether the kernel has raised the `high`/`max` event counters. The
//! policy that acts on the figures is `policy.rs` (design 11): the
//! headroom band over the effective limit (the retention cap, the decay
//! window, cache GC at every tick and the immediate purge above 90%), the
//! lazy-or-eager purge advice (`MADV_FREE` under 70% of a known limit,
//! `MADV_DONTNEED` above), the full purge on a `high` event and the
//! `thp:always` force to `never` under 1 GiB; `stats.cgroup.*` and
//! `malloc_stats_print` expose the same figures.
//!
//! Everything is read with raw `open`/`read`/`close` on fixed-size stack
//! buffers through `sys`, never libc stdio, and nothing here allocates. The
//! parsers at the top of the module are pure functions over byte slices and
//! are unit tested on the host with captured file contents; the runtime
//! half (`discover`, `sample`) is Linux-only and exercised in the
//! container.
//!
//! Costs: discovery is one read of `/proc/self/cgroup`, one line scan of
//! `/proc/self/mountinfo` and two small reads per level of the cgroup path,
//! once at init. Sampling is two small reads at most once per
//! [`SAMPLE_INTERVAL_NS`] (10 ms on `CLOCK_MONOTONIC_COARSE`), from slow
//! paths only; the limit walk is repeated at most once per
//! [`LIMIT_INTERVAL_NS`] so a `docker update --memory` is seen.

use crate::sync::atomic::{AtomicU64, Ordering};
use core::fmt;

#[cfg(verus_only)]
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
/// Buffer for one cgroup file. `memory.events` is a handful of lines;
/// `/proc/self/cgroup` can carry a dozen v1 lines on a hybrid host and
/// still fits.
const FILE_BUF: usize = 1024;
/// Chunk size for the `/proc/self/mountinfo` line scan.
const SCAN_BUF: usize = 2048;

/// Minimum interval between two samples of `memory.current` and
/// `memory.events` (design 11).
pub const SAMPLE_INTERVAL_NS: u64 = 10_000_000;
/// Minimum interval between two walks of `memory.max`/`memory.high`.
pub const LIMIT_INTERVAL_NS: u64 = 1_000_000_000;

/// `max` in a limit file: no limit at this level.
pub const UNLIMITED: u64 = u64::MAX;

} // verus!

// ---------------------------------------------------------------------------
// Pure parsing (unit tested against captured contents).
// ---------------------------------------------------------------------------

verus! {

/// Leading decimal number of `bytes`, ignoring what follows the digits.
/// `None` without a digit or on overflow (`sys::parse_decimal`).
fn parse_u64_prefix(bytes: &[u8]) -> (r: Option<u64>)
    ensures
        r == parse_decimal_spec(bytes@),
{
    parse_decimal(bytes)
}

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
/// `None` when there is no `0::` line (a cgroup v1 only system).
// The nested `if` keeps the loop body in the form the proof states.
#[allow(clippy::collapsible_if)]
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
    while line.len() - sep >= 3 && !(line[sep] == b' ' && line[sep + 1] == b'-' && line[sep + 2] == b' ')
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

/// What [`cgroup2_mount`] returns: the mount point of the first line that
/// has one.
pub open spec fn cgroup2_mount_spec(s: Seq<u8>) -> Option<Seq<u8>>
    decreases s.len(),
{
    if s.len() == 0 {
        None
    } else {
        let l = s.take(line_len(s) as int);
        if l.len() > 0 && cgroup2_mount_line_spec(l) is Some {
            cgroup2_mount_line_spec(l)
        } else {
            cgroup2_mount_spec(after_line(s))
        }
    }
}

/// The first cgroup2 mount point in `/proc/self/mountinfo` contents, or
/// `None` when there is none (no unified hierarchy mounted).
#[cfg(test)]
// The nested `if` keeps the loop body in the form the proof states.
#[allow(clippy::collapsible_if)]
pub fn cgroup2_mount(contents: &[u8]) -> (r: Option<&[u8]>)
    ensures
        opt_view(r) == cgroup2_mount_spec(contents@),
{
    proof {
        assert(contents@.skip(0) =~= contents@);
    }
    let mut start = 0;
    while start < contents.len()
        invariant
            start <= contents@.len(),
            cgroup2_mount_spec(contents@) == cgroup2_mount_spec(contents@.skip(start as int)),
        decreases contents@.len() - start,
    {
        let end = find_from(contents, start, b'\n');
        proof {
            lemma_line_step(contents@, start as int, end as int);
        }
        let line = &contents[start..end];
        if !line.is_empty() {
            if let Some(m) = cgroup2_mount_from_line(line) {
                return Some(m);
            }
        }
        if end == contents.len() {
            proof {
                assert(cgroup2_mount_spec(Seq::<u8>::empty()) is None);
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
    parse_u64_prefix(t)
}

/// The counters of `memory.events` (cgroup v2 format, one `key value` per
/// line, all five since kernel 4.13: `low 0`, `high 3`, `max 1`, `oom 0`,
/// `oom_kill 0`; `oom_kill` was added in 4.13 and reads 0 on the 4.5..4.12
/// kernels that lack the line, and `oom_group_kill`, 5.17, is ignored). A
/// missing line reads as 0. [`Cgroup::events`] returns the last sample of
/// all five; the policy acts on `high` and `max`.
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
    /// Processes of the cgroup the OOM killer killed (4.13+).
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
        if let Some(v) = parse_u64_prefix(trim(&line[k + 1..])) {
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
    let mut e = Events { low: 0, high: 0, max: 0, oom: 0, oom_kill: 0 };
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

/// Walk from the leaf directory `dir` up to the cgroup2 mount (the first
/// `mount_len` bytes of `dir`, inclusive) and return the minimum of what
/// `read` yields per level; `read` gets the directory of the level and
/// answers `None` where the file is absent. [`UNLIMITED`] when no level
/// has a limit. Pure: the reader is a closure so the tests feed it
/// captured contents.
pub fn walk_min<F: FnMut(&[u8]) -> Option<u64>>(dir: &[u8], mount_len: usize, mut read: F) -> u64
    requires
        forall|d: &[u8]| d@.len() <= dir@.len() ==> #[trigger] call_requires(read, (d,)),
{
    let mut min = UNLIMITED;
    let mut end = dir.len();
    loop
        invariant
            end <= dir@.len(),
            forall|d: &[u8]| d@.len() <= dir@.len() ==> #[trigger] call_requires(read, (d,)),
        decreases end,
    {
        if let Some(v) = read(&dir[..end]) {
            min = min.min(v);
        }
        if end <= mount_len {
            break;
        }
        // The last `/` of `dir[mount_len..end]`, which ends the parent.
        let mut i = end;
        while i > mount_len && dir[i - 1] != b'/'
            invariant
                mount_len <= i <= end,
                end <= dir@.len(),
            decreases i,
        {
            i -= 1;
        }
        if i == mount_len {
            break;
        }
        end = i - 1;
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
    let mount = if !mount.is_empty() && mount[mount.len() - 1] == b'/' { &mount[..mount.len() - 1] } else { mount };
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
/// `None` on error (which ends the scan). Runs until `read` reports the
/// end or an error, so a reader that never does keeps it going.
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
            Some(0) | None => break,
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

} // verus!

// ---------------------------------------------------------------------------
// Runtime state.
// ---------------------------------------------------------------------------

verus! {

/// Why the cgroup is not tracked, or that it is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// `cgroup:false`: nothing was read.
    Disabled = 0,
    /// No `0::` line in `/proc/self/cgroup` (cgroup v1 only).
    NoUnifiedPath = 1,
    /// No `cgroup2` filesystem in `/proc/self/mountinfo`.
    NoMount = 2,
    /// The directory exceeds [`PATH_CAP`].
    PathTooLong = 3,
    /// Reads work; the figures below are live.
    Tracked = 4,
}

/// The process's memory cgroup as last read (design 11). One per process,
/// inside `heap::Shared`; written by `discover` at init and by the
/// samplers afterwards, so every mutable field is an atomic.
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

    /// The last `memory.events` counters sampled, every field the file
    /// has (zero before the first sample, and for a line the kernel does
    /// not print: `oom_kill` before 4.13).
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
}

/// A path shown as text, one `?` per byte that is not printable ASCII, for
/// the `cgroup:` line.
struct PathBytes<'a>(&'a [u8]);

// Any bytes format (ghost only: ordinary builds have no `std_specs`).
#[cfg(verus_only)]
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

/// The printable ASCII bytes, `' '..='~'`, in order.
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
    unsafe { core::str::from_utf8_unchecked(&table[i..i + 1]) }
}

/// A limit, `max` when unlimited, for the `cgroup:` line.
struct Limit(u64);

// Any limit formats (ghost only).
#[cfg(verus_only)]
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
            "cgroup: {} (mount {}, path {}) limit {} high {} current {} events high {} (+{}) max {} (+{})",
            PathBytes(self.dir()),
            PathBytes(self.mount()),
            PathBytes(self.path()),
            Limit(self.limit()),
            Limit(self.high()),
            self.current(),
            self.events().high,
            hr,
            self.events().max,
            mr,
        )
    }
}

} // verus!

mod linux {
    use super::*;
    use crate::sys::{self, O_RDONLY, read_small_file};

    verus! {

    /// `/proc/self/cgroup`, NUL-terminated (spelled as bytes so the
    /// verifier sees the NUL `sys::open` requires).
    const PROC_SELF_CGROUP: [u8; 18] = [
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'e', b'l', b'f', b'/', b'c', b'g', b'r', b'o', b'u', b'p', 0,
    ];

    /// `/proc/self/mountinfo`, NUL-terminated.
    const PROC_SELF_MOUNTINFO: [u8; 21] = [
        b'/', b'p', b'r', b'o', b'c', b'/', b's', b'e', b'l', b'f', b'/', b'm', b'o', b'u', b'n', b't', b'i', b'n', b'f', b'o', 0,
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

    /// `dir/name` parsed as a limit file; `None` when absent or malformed.
    fn read_limit(dir: &[u8], name: &[u8]) -> Option<u64>
        requires
            crate::sys::may_syscall(),
            dir@.len() + FILE_ROOM <= PATH_CAP,
            name@.len() + 2 <= FILE_ROOM,
    {
        let mut buf = [0u8; 64];
        let n = read_in(dir, name, &mut buf)?;
        parse_limit(&buf[..n])
    }

    /// The first cgroup2 mount point in `/proc/self/mountinfo`, copied into
    /// `mount`, and its length: 0 when there is none or it is longer than
    /// [`PATH_CAP`]. The scan stops at the first line whose mount fits
    /// and leaves it in the scan buffer, where it is parsed again for the
    /// copy.
    fn mountinfo_cgroup2(mount: &mut [u8; PATH_CAP]) -> (r: usize)
        requires
            crate::sys::may_syscall(),
        ensures
            r <= PATH_CAP,
    {
        let mut scan = [0u8; SCAN_BUF];
        let Ok(fd) = sys::open(&PROC_SELF_MOUNTINFO, O_RDONLY) else { return 0 };
        let found = scan_lines(
            &mut scan,
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
        /// With `enabled == false` (`cgroup:false`) nothing is opened.
        pub fn discover(enabled: bool) -> Cgroup
            requires
                crate::sys::may_syscall(),
        {
            if !enabled {
                return Cgroup::untracked(State::Disabled);
            }
            let mut cg = Cgroup::untracked(State::NoUnifiedPath);
            proof {
                use_type_invariant(&cg);
            }
            let mut pathbuf = [0u8; FILE_BUF];
            let Some(n) = read_small_file(&PROC_SELF_CGROUP, &mut pathbuf) else { return cg };
            let Some(path) = cgroup_path(&pathbuf[..n]) else { return cg };

            let mut mount = [0u8; PATH_CAP];
            let mount_len = mountinfo_cgroup2(&mut mount);
            if mount_len == 0 {
                cg.state = State::NoMount;
                return cg;
            }
            proof {
                use_type_invariant(&cg);
            }
            let Some((dir_len, mlen)) = join_dir(&mut cg.dir, &mount[..mount_len], path) else {
                cg.state = State::PathTooLong;
                return cg;
            };
            cg.dir_len = dir_len as u16;
            cg.mount_len = mlen as u16;
            cg.state = State::Tracked;

            let dir = &cg.dir[..dir_len];
            let mut buf = [0u8; FILE_BUF];
            cg.has_limit_files = read_in(dir, b"memory.max", &mut buf).is_some();
            cg.has_current = read_in(dir, b"memory.current", &mut buf).is_some();
            cg.has_events = read_in(dir, b"memory.events", &mut buf).is_some();
            let now = sys::now_coarse_ns();
            cg.walk_limits(now);
            cg.sample_now(now);
            cg
        }

        /// The effective limits: the minimum of `memory.max` and of
        /// `memory.high` over the levels from the leaf to the mount.
        fn walk_limits(&self, now: u64)
            requires
                crate::sys::may_syscall(),
        {
            self.limits_at.store(now, Ordering::Relaxed);
            let (dir, mlen) = (self.dir(), self.mount_len as usize);
            self.limit.store(
                walk_min(
                    dir,
                    mlen,
                    |d: &[u8]| -> (r: Option<u64>)
                        requires
                            d@.len() + FILE_ROOM <= PATH_CAP,
                        { read_limit(d, b"memory.max") },
                ),
                Ordering::Relaxed,
            );
            self.high.store(
                walk_min(
                    dir,
                    mlen,
                    |d: &[u8]| -> (r: Option<u64>)
                        requires
                            d@.len() + FILE_ROOM <= PATH_CAP,
                        { read_limit(d, b"memory.high") },
                ),
                Ordering::Relaxed,
            );
        }

        /// Read `memory.current` and `memory.events` now, unconditionally,
        /// and re-walk the limits if [`LIMIT_INTERVAL_NS`] has passed.
        fn sample_now(&self, now: u64)
            requires
                crate::sys::may_syscall(),
        {
            if self.has_current {
                if let Some(v) = read_limit(self.dir(), b"memory.current") {
                    self.current.store(v, Ordering::Relaxed);
                }
            }
            if self.has_events {
                let mut buf = [0u8; FILE_BUF];
                if let Some(n) = read_in(self.dir(), b"memory.events", &mut buf) {
                    self.note_events(parse_events(&buf[..n]));
                }
            }
            if now.wrapping_sub(self.limits_at.load(Ordering::Relaxed)) >= LIMIT_INTERVAL_NS {
                self.walk_limits(now);
            }
        }

        /// Sample if at least [`SAMPLE_INTERVAL_NS`] of the coarse clock
        /// has passed since the last sample; one thread wins the interval.
        /// `true` when this call read the files. Slow paths only.
        pub fn sample(&self, now: u64) -> bool
            requires
                crate::sys::may_syscall(),
        {
            if !self.tracked() {
                return false;
            }
            let last = self.sampled_at.load(Ordering::Relaxed);
            if now.wrapping_sub(last) < SAMPLE_INTERVAL_NS {
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::string::String;
    use std::vec::Vec;

    const PROC_CGROUP_V2: &[u8] = b"0::/a/b\n";
    const PROC_CGROUP_MIXED: &[u8] = b"12:memory:/docker/abc\n11:cpu,cpuacct:/docker/abc\n3:pids:/docker/abc\n1:name=systemd:/docker/abc\n0::/docker/abc\n";
    const PROC_CGROUP_NS: &[u8] = b"0::/\n";
    const PROC_CGROUP_V1: &[u8] = b"12:memory:/docker/abc\n11:cpu,cpuacct:/docker/abc\n";

    const MOUNTINFO_DEFAULT: &[u8] = b"22 29 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw\n\
        23 29 0:22 / /sys rw,nosuid,nodev,noexec,relatime shared:2 - sysfs sysfs rw\n\
        35 24 0:30 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime shared:9 - cgroup2 cgroup2 rw,nsdelegate,memory_recursiveprot\n\
        36 24 0:31 / /sys/fs/bpf rw,nosuid,nodev,noexec,relatime shared:10 - bpf bpf rw,mode=700\n";
    const MOUNTINFO_DOCKER: &[u8] = b"1301 1288 0:30 /../../docker/abc /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate\n";
    const MOUNTINFO_HYBRID: &[u8] = b"25 24 0:23 / /sys/fs/cgroup ro,nosuid,nodev,noexec shared:9 - tmpfs tmpfs ro,mode=755\n\
        26 25 0:24 / /sys/fs/cgroup/unified rw,nosuid,nodev,noexec,relatime shared:10 - cgroup2 cgroup2 rw,nsdelegate\n\
        27 25 0:25 / /sys/fs/cgroup/systemd rw,nosuid,nodev,noexec,relatime shared:11 - cgroup cgroup rw,xattr,name=systemd\n\
        30 25 0:28 / /sys/fs/cgroup/memory rw,nosuid,nodev,noexec,relatime shared:14 - cgroup cgroup rw,memory\n";
    const MOUNTINFO_V1: &[u8] = b"25 24 0:23 / /sys/fs/cgroup ro,nosuid,nodev,noexec shared:9 - tmpfs tmpfs ro,mode=755\n\
        30 25 0:28 / /sys/fs/cgroup/memory rw,nosuid,nodev,noexec,relatime shared:14 - cgroup cgroup rw,memory\n";

    #[test]
    fn proc_self_cgroup_lines() {
        assert_eq!(cgroup_path(PROC_CGROUP_V2), Some(&b"/a/b"[..]));
        assert_eq!(cgroup_path(PROC_CGROUP_MIXED), Some(&b"/docker/abc"[..]));
        assert_eq!(cgroup_path(PROC_CGROUP_NS), Some(&b"/"[..]));
        assert_eq!(cgroup_path(PROC_CGROUP_V1), None);
        assert_eq!(cgroup_path(b""), None);
        assert_eq!(cgroup_path(b"0::relative\n"), None, "a path starts with /");
        assert_eq!(
            cgroup_path(b"10::/x\n"),
            None,
            "only hierarchy 0 is the unified one"
        );
        assert_eq!(cgroup_path_from_line(b"0::/a/b"), Some(&b"/a/b"[..]));
    }

    #[test]
    fn mountinfo_lines() {
        assert_eq!(
            cgroup2_mount(MOUNTINFO_DEFAULT),
            Some(&b"/sys/fs/cgroup"[..])
        );
        assert_eq!(
            cgroup2_mount(MOUNTINFO_DOCKER),
            Some(&b"/sys/fs/cgroup"[..])
        );
        assert_eq!(
            cgroup2_mount(MOUNTINFO_HYBRID),
            Some(&b"/sys/fs/cgroup/unified"[..])
        );
        assert_eq!(cgroup2_mount(MOUNTINFO_V1), None);
        assert_eq!(cgroup2_mount(b""), None);
        // The optional-field count varies; the separator keeps field 5 honest.
        let many =
            b"35 24 0:30 / /mnt/cg rw shared:9 master:3 propagate_from:1 - cgroup2 cgroup2 rw\n";
        assert_eq!(cgroup2_mount(many), Some(&b"/mnt/cg"[..]));
        // A line without the separator or with too few fields is skipped.
        assert_eq!(
            cgroup2_mount(b"garbage cgroup2\n35 24 0:30 / - cgroup2 x y\n"),
            None
        );
    }

    #[test]
    fn limit_files() {
        assert_eq!(parse_limit(b"max\n"), Some(UNLIMITED));
        assert_eq!(parse_limit(b"max"), Some(UNLIMITED));
        assert_eq!(parse_limit(b"536870912\n"), Some(536_870_912));
        assert_eq!(parse_limit(b"0\n"), Some(0));
        assert_eq!(parse_limit(b"18446744073709551615\n"), Some(u64::MAX));
        assert_eq!(parse_limit(b"18446744073709551616\n"), None, "overflow");
        assert_eq!(parse_limit(b"\n"), None);
        assert_eq!(parse_limit(b"maxx\n"), None);
        assert_eq!(parse_limit(b"-1\n"), None);
        assert_eq!(parse_u64_prefix(b"42 x"), Some(42));
        assert_eq!(parse_u64_prefix(b""), None);
    }

    #[test]
    fn events_files() {
        let e = parse_events(b"low 0\nhigh 3\nmax 1\noom 0\noom_kill 0\n");
        assert_eq!(
            e,
            Events {
                low: 0,
                high: 3,
                max: 1,
                oom: 0,
                oom_kill: 0
            }
        );
        // Newer kernels add lines; missing lines are 0; junk is ignored.
        let e = parse_events(b"low 7\nhigh 2\nmax 5\noom 1\noom_kill 1\noom_group_kill 0\n");
        assert_eq!(
            e,
            Events {
                low: 7,
                high: 2,
                max: 5,
                oom: 1,
                oom_kill: 1
            }
        );
        assert_eq!(
            parse_events(b"high 9\n"),
            Events {
                high: 9,
                ..Events::default()
            }
        );
        assert_eq!(parse_events(b""), Events::default());
        assert_eq!(parse_events(b"high x\nmax\n"), Events::default());
        // A 4.5..4.12 file has no `oom_kill` line: it reads 0.
        assert_eq!(
            parse_events(b"low 1\nhigh 2\nmax 3\noom 4\n"),
            Events {
                low: 1,
                high: 2,
                max: 3,
                oom: 4,
                oom_kill: 0
            }
        );
    }

    /// The sampled counters are the file's, all five, and the rise counts
    /// move with `high` and `max` alone: three samples of a cgroup that
    /// hit `memory.high` twice and was OOM-killed once.
    #[test]
    fn sampled_events_carry_every_counter() {
        let cg = Cgroup::untracked(State::Tracked);
        assert_eq!(
            cg.events(),
            Events::default(),
            "zero before the first sample"
        );
        cg.note_events(parse_events(b"low 0\nhigh 3\nmax 1\noom 0\noom_kill 0\n"));
        assert_eq!(
            cg.events(),
            Events {
                low: 0,
                high: 3,
                max: 1,
                oom: 0,
                oom_kill: 0
            }
        );
        assert_eq!(cg.rises(), (1, 1), "the first sample rises from zero");
        cg.note_events(parse_events(
            b"low 7\nhigh 5\nmax 1\noom 1\noom_kill 1\noom_group_kill 0\n",
        ));
        assert_eq!(
            cg.events(),
            Events {
                low: 7,
                high: 5,
                max: 1,
                oom: 1,
                oom_kill: 1
            }
        );
        assert_eq!(
            cg.rises(),
            (2, 1),
            "high rose, max did not; low and oom are not rises"
        );
        cg.note_events(parse_events(b"low 7\nhigh 5\nmax 2\noom 1\noom_kill 1\n"));
        assert_eq!(
            cg.events(),
            Events {
                low: 7,
                high: 5,
                max: 2,
                oom: 1,
                oom_kill: 1
            }
        );
        assert_eq!(cg.rises(), (2, 2));
        // A kernel without the `oom_kill` line: the field reads 0, the
        // others are kept.
        cg.note_events(parse_events(b"low 8\nhigh 5\nmax 2\noom 2\n"));
        assert_eq!(
            cg.events(),
            Events {
                low: 8,
                high: 5,
                max: 2,
                oom: 2,
                oom_kill: 0
            }
        );
        assert_eq!(cg.rises(), (2, 2));
        let s = std::format!("{cg}");
        assert!(s.ends_with(" events high 5 (+2) max 2 (+2)"), "{s}");
    }

    /// A captured tree: directory -> file -> contents. Absent = `None`.
    fn tree(entries: &[(&str, &str, &str)]) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut t: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (d, f, c) in entries {
            t.entry(String::from(*d))
                .or_default()
                .insert(String::from(*f), String::from(*c));
        }
        t
    }

    fn walk(
        t: &BTreeMap<String, BTreeMap<String, String>>,
        dir: &str,
        mount: &str,
        file: &str,
    ) -> (u64, Vec<String>) {
        let mut visited = Vec::new();
        let v = walk_min(dir.as_bytes(), mount.len(), |d| {
            let d = std::str::from_utf8(d).unwrap();
            visited.push(String::from(d));
            t.get(d)
                .and_then(|files| files.get(file))
                .and_then(|c| parse_limit(c.as_bytes()))
        });
        (v, visited)
    }

    #[test]
    fn effective_limit_walk() {
        let m = "/sys/fs/cgroup";
        // Parent lower than the leaf: the parent wins. `max` and absent
        // files are unlimited. The mount level itself is read.
        let t = tree(&[
            ("/sys/fs/cgroup/a/b", "memory.max", "max\n"),
            ("/sys/fs/cgroup/a/b", "memory.high", "1073741824\n"),
            ("/sys/fs/cgroup/a", "memory.max", "536870912\n"),
            ("/sys/fs/cgroup/a", "memory.high", "max\n"),
        ]);
        let (limit, visited) = walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max");
        assert_eq!(limit, 536_870_912);
        assert_eq!(
            visited,
            ["/sys/fs/cgroup/a/b", "/sys/fs/cgroup/a", "/sys/fs/cgroup"]
        );
        assert_eq!(
            walk(&t, "/sys/fs/cgroup/a/b", m, "memory.high").0,
            1_073_741_824
        );
        // Leaf lower than the parent: the leaf wins.
        let t = tree(&[
            ("/sys/fs/cgroup/a/b", "memory.max", "268435456\n"),
            ("/sys/fs/cgroup/a", "memory.max", "536870912\n"),
        ]);
        assert_eq!(
            walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max").0,
            268_435_456
        );
        // Nothing anywhere (cgroup v1 memory controller, or a root cgroup).
        let t = tree(&[]);
        assert_eq!(walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max").0, UNLIMITED);
        // At the mount itself (a cgroup namespace, path `/`): one level.
        let t = tree(&[("/sys/fs/cgroup", "memory.max", "536870912\n")]);
        let (limit, visited) = walk(&t, "/sys/fs/cgroup", m, "memory.max");
        assert_eq!(limit, 536_870_912);
        assert_eq!(visited, ["/sys/fs/cgroup"]);
        // A malformed file at one level does not hide the others.
        let t = tree(&[
            ("/sys/fs/cgroup/a/b", "memory.max", "garbage\n"),
            ("/sys/fs/cgroup/a", "memory.max", "4096\n"),
        ]);
        assert_eq!(walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max").0, 4096);
    }

    /// The edges of the walk: `mount_len` is what `join_dir` returns (no
    /// trailing slash), so a slash-terminated mount length stops one level
    /// short of the mount itself rather than visiting it; and a directory
    /// shorter than `mount_len` is read once, whole, and never sliced past
    /// its end.
    #[test]
    fn effective_limit_walk_edges() {
        let t = tree(&[
            ("/sys/fs/cgroup/a/b", "memory.max", "max\n"),
            ("/sys/fs/cgroup/a", "memory.max", "536870912\n"),
            ("/sys/fs/cgroup", "memory.max", "4096\n"),
        ]);
        // Mount given with its trailing slash: `a` is the last level.
        let (limit, visited) = walk(&t, "/sys/fs/cgroup/a/b", "/sys/fs/cgroup/", "memory.max");
        assert_eq!(limit, 536_870_912);
        assert_eq!(visited, ["/sys/fs/cgroup/a/b", "/sys/fs/cgroup/a"]);
        // Without it the mount level is read too.
        let (limit, visited) = walk(&t, "/sys/fs/cgroup/a/b", "/sys/fs/cgroup", "memory.max");
        assert_eq!(limit, 4096);
        assert_eq!(
            visited,
            ["/sys/fs/cgroup/a/b", "/sys/fs/cgroup/a", "/sys/fs/cgroup"]
        );
        // A directory shorter than the mount length: one read of the
        // whole directory, then stop.
        let (limit, visited) = walk(&t, "/sys/fs/cgroup", "/sys/fs/cgroup/unified", "memory.max");
        assert_eq!(limit, 4096);
        assert_eq!(visited, ["/sys/fs/cgroup"]);
        let (limit, visited) = walk(&t, "/sys", "/sys/fs/cgroup", "memory.max");
        assert_eq!(limit, UNLIMITED);
        assert_eq!(visited, ["/sys"]);
        // The mount at the root: every `/` of the path is a level.
        let t = tree(&[
            ("/a/b", "memory.max", "8192\n"),
            ("", "memory.max", "4096\n"),
        ]);
        let (limit, visited) = walk(&t, "/a/b", "", "memory.max");
        assert_eq!(limit, 4096);
        assert_eq!(visited, ["/a/b", "/a", ""]);
    }

    #[test]
    fn join_dir_forms() {
        let mut out = [0u8; PATH_CAP];
        let (n, m) = join_dir(&mut out, b"/sys/fs/cgroup", b"/a/b").unwrap();
        assert_eq!(&out[..n], b"/sys/fs/cgroup/a/b");
        assert_eq!(m, "/sys/fs/cgroup".len());
        let (n, m) = join_dir(&mut out, b"/sys/fs/cgroup", b"/").unwrap();
        assert_eq!(&out[..n], b"/sys/fs/cgroup");
        assert_eq!(n, m);
        let (n, _) = join_dir(&mut out, b"/sys/fs/cgroup/unified/", b"/x/").unwrap();
        assert_eq!(&out[..n], b"/sys/fs/cgroup/unified/x");
        let long: Vec<u8> = core::iter::repeat_n(b'x', PATH_CAP).collect();
        assert_eq!(join_dir(&mut out, b"/sys/fs/cgroup", &long), None);
        let mut small = [0u8; 40];
        assert_eq!(
            join_dir(&mut small, b"/sys/fs/cgroup", b"/a"),
            None,
            "must leave FILE_ROOM"
        );
    }

    /// A cgroup2 mounted at `/` (a container whose root is the cgroup
    /// filesystem): the mount strips to nothing, the path is the whole
    /// directory and the mount length is 0, so `walk_min` visits every
    /// level down to the empty string and `Cgroup::path` reports the path.
    #[test]
    fn join_dir_with_the_mount_at_root() {
        let mut out = [0u8; PATH_CAP];
        let (n, m) = join_dir(&mut out, b"/", b"/a/b").unwrap();
        assert_eq!(&out[..n], b"/a/b");
        assert_eq!(m, 0);
        let (n, m) = join_dir(&mut out, b"/", b"/").unwrap();
        assert_eq!(
            (n, m),
            (0, 0),
            "the root cgroup at a root mount is the empty directory"
        );
    }

    /// Feed `contents` to `scan_lines` in chunks of `chunk` bytes through a
    /// buffer of `buf` bytes and collect the lines seen.
    fn scanned(contents: &[u8], chunk: usize, buf: usize, stop_at: Option<&[u8]>) -> Vec<Vec<u8>> {
        let mut pos = 0;
        let mut b: Vec<u8> = Vec::new();
        b.resize(buf, 0);
        let mut seen = Vec::new();
        scan_lines(
            &mut b,
            |dst| {
                let n = chunk.min(dst.len()).min(contents.len() - pos);
                dst[..n].copy_from_slice(&contents[pos..pos + n]);
                pos += n;
                Some(n)
            },
            |line| {
                seen.push(line.to_vec());
                stop_at == Some(line)
            },
        );
        seen
    }

    #[test]
    fn line_scanner_reassembles_across_chunks() {
        let all: Vec<Vec<u8>> = MOUNTINFO_HYBRID
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| l.to_vec())
            .collect();
        for chunk in [1, 7, 64, 4096] {
            assert_eq!(
                scanned(MOUNTINFO_HYBRID, chunk, 256, None),
                all,
                "chunk {chunk}"
            );
        }
        // Early stop.
        let first = scanned(MOUNTINFO_HYBRID, 13, 256, Some(&all[1]));
        assert_eq!(first, all[..2]);
        // A line longer than the buffer is dropped; the ones after it survive.
        let mut long = Vec::new();
        long.extend_from_slice(b"short\n");
        long.extend(core::iter::repeat_n(b'L', 300));
        long.extend_from_slice(b"\nafter\nlast");
        assert_eq!(
            scanned(&long, 50, 128, None),
            [&b"short"[..], b"after", b"last"]
        );
        // Stopping inside a chunk does not report later lines.
        assert_eq!(scanned(b"a\nb\nc\n", 4096, 64, Some(b"a")), [b"a"]);
        // The real cgroup2 line is found whatever the chunking.
        for chunk in [1, 100] {
            let mut found = None;
            let mut b = [0u8; 256];
            let mut pos = 0;
            scan_lines(
                &mut b,
                |dst| {
                    let n = chunk.min(dst.len()).min(MOUNTINFO_HYBRID.len() - pos);
                    dst[..n].copy_from_slice(&MOUNTINFO_HYBRID[pos..pos + n]);
                    pos += n;
                    Some(n)
                },
                |line| {
                    found = cgroup2_mount_from_line(line).map(|m| m.to_vec());
                    found.is_some()
                },
            );
            assert_eq!(
                found.as_deref(),
                Some(&b"/sys/fs/cgroup/unified"[..]),
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn untracked_states_display() {
        for (state, needle) in [
            (State::Disabled, "cgroup: not tracked (cgroup:false)"),
            (
                State::NoUnifiedPath,
                "cgroup: not tracked (no unified hierarchy",
            ),
            (State::NoMount, "cgroup: not tracked (no cgroup2 mount)"),
            (State::PathTooLong, "cgroup: not tracked (path too long)"),
        ] {
            let cg = Cgroup::untracked(state);
            assert!(!cg.tracked());
            assert_eq!(cg.limit(), UNLIMITED);
            assert_eq!(cg.high(), UNLIMITED);
            assert_eq!(cg.current(), 0);
            let s = std::format!("{cg}");
            assert!(s.starts_with(needle), "{s}");
        }
        let cg = Cgroup::discover(false);
        assert_eq!(cg.state(), State::Disabled);
        assert!(!cg.sample(u64::MAX));
    }
}

#[cfg(test)]
mod linux_tests {
    use super::*;
    use crate::sys;
    use std::string::String;
    use std::{eprintln, format, vec::Vec};

    fn read_opt(p: &str) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    /// `min` of a limit file over the walk, computed with std for the
    /// cross-check.
    fn std_walk(dir: &str, mount: &str, file: &str) -> u64 {
        let mut min = UNLIMITED;
        let mut d = String::from(dir);
        loop {
            if let Some(c) = read_opt(&format!("{d}/{file}")) {
                if let Some(v) = parse_limit(c.as_bytes()) {
                    min = min.min(v);
                }
            }
            if d == mount {
                break;
            }
            match d.rfind('/') {
                Some(i) if i >= mount.len() => d.truncate(i.max(mount.len())),
                _ => break,
            }
            if d.len() < mount.len() {
                break;
            }
        }
        min
    }

    #[test]
    fn discover_matches_std_reading_of_the_same_files() {
        let cg = Cgroup::discover(true);
        let line = format!("{cg}");
        eprintln!("{line}");
        let proc_cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let path =
            cgroup_path(proc_cgroup.as_bytes()).map(|p| String::from_utf8(p.to_vec()).unwrap());
        let mount =
            cgroup2_mount(mountinfo.as_bytes()).map(|m| String::from_utf8(m.to_vec()).unwrap());
        eprintln!("cgroup: /proc/self/cgroup path {path:?}, cgroup2 mount {mount:?}");
        match (&path, &mount) {
            (Some(path), Some(mount)) => {
                assert!(cg.tracked(), "{line}");
                assert_eq!(std::str::from_utf8(cg.mount()).unwrap(), mount);
                assert_eq!(std::str::from_utf8(cg.path()).unwrap(), path);
                let dir = if path == "/" {
                    mount.clone()
                } else {
                    format!("{mount}{path}")
                };
                assert_eq!(std::str::from_utf8(cg.dir()).unwrap(), dir);
                // The limit is `max` or a number, and equal to what std reads.
                let expect_limit = std_walk(&dir, mount, "memory.max");
                let expect_high = std_walk(&dir, mount, "memory.high");
                assert_eq!(cg.limit(), expect_limit, "{line}");
                assert_eq!(cg.high(), expect_high, "{line}");
                assert_eq!(
                    cg.has_limit_files(),
                    std::path::Path::new(&format!("{dir}/memory.max")).exists()
                );
                assert_eq!(
                    cg.has_current(),
                    std::path::Path::new(&format!("{dir}/memory.current")).exists()
                );
                assert_eq!(
                    cg.has_events(),
                    std::path::Path::new(&format!("{dir}/memory.events")).exists()
                );
                if cg.has_events() {
                    // Discovery sampled the file: every counter is the
                    // kernel's (they only grow, so at most a later rise
                    // separates the two reads).
                    let now = parse_events(
                        std::fs::read(format!("{dir}/memory.events"))
                            .unwrap()
                            .as_slice(),
                    );
                    let e = cg.events();
                    eprintln!("cgroup: memory.events {e:?} (file now {now:?})");
                    for (name, sampled, file) in [
                        ("low", e.low, now.low),
                        ("high", e.high, now.high),
                        ("max", e.max, now.max),
                        ("oom", e.oom, now.oom),
                        ("oom_kill", e.oom_kill, now.oom_kill),
                    ] {
                        assert!(sampled <= file, "{name}: sampled {sampled} > file {file}");
                    }
                }
                eprintln!(
                    "cgroup: memory.max={} memory.high={} memory.current={}",
                    read_opt(&format!("{dir}/memory.max"))
                        .unwrap_or_else(|| "(absent)".into())
                        .trim(),
                    read_opt(&format!("{dir}/memory.high"))
                        .unwrap_or_else(|| "(absent)".into())
                        .trim(),
                    read_opt(&format!("{dir}/memory.current"))
                        .unwrap_or_else(|| "(absent)".into())
                        .trim(),
                );
                if cg.has_current() {
                    // This process is charged to its own cgroup, so the
                    // figure is not zero and moves with a big allocation.
                    assert!(cg.current() > 0, "{line}");
                }
                // The container test passes the limit Docker was given.
                if let Ok(want) = std::env::var("CHALLOC_EXPECT_CGROUP_LIMIT") {
                    let want: u64 = want.parse().unwrap();
                    assert_eq!(cg.limit(), want, "effective limit under --memory: {line}");
                    eprintln!(
                        "cgroup: effective limit {} matches CHALLOC_EXPECT_CGROUP_LIMIT",
                        cg.limit()
                    );
                }
            }
            (None, _) => assert_eq!(cg.state(), State::NoUnifiedPath, "{line}"),
            (Some(_), None) => assert_eq!(cg.state(), State::NoMount, "{line}"),
        }
    }

    #[test]
    fn sampling_is_rate_limited_to_ten_milliseconds() {
        let cg = Cgroup::discover(true);
        if !cg.tracked() {
            eprintln!(
                "cgroup: untracked here ({}), skipping the rate-limit test",
                cg.state() as u8
            );
            return;
        }
        // Spin for 100 ms of the coarse clock calling `sample` as fast as
        // possible: at most one read per 10 ms window (plus one for the
        // window the spin starts in), and not far fewer.
        let start = sys::now_coarse_ns();
        let mut reads = 0;
        let mut calls = 0u64;
        loop {
            let now = sys::now_coarse_ns();
            if now - start >= 100_000_000 {
                break;
            }
            if cg.sample(now) {
                reads += 1;
            }
            calls += 1;
        }
        eprintln!("cgroup: {reads} reads in {calls} sample calls over 100 ms");
        assert!(reads <= 12, "{reads} reads in 100 ms");
        assert!(reads >= 3, "{reads} reads in 100 ms");
        // Two calls in the same instant: the second never reads.
        let now = sys::now_coarse_ns() + SAMPLE_INTERVAL_NS;
        assert!(cg.sample(now));
        assert!(!cg.sample(now));
        assert_eq!(cg.sampled_at(), now);
    }

    #[test]
    fn current_tracks_a_large_allocation() {
        let cg = Cgroup::discover(true);
        if !cg.tracked() || !cg.has_current() {
            eprintln!("cgroup: no memory.current here, skipping");
            return;
        }
        let before = cg.current();
        // 64 MiB touched: charged to this cgroup. The other test processes
        // of the container share the cgroup and move the figure on their
        // own, so the assertion is against the kernel's file read just
        // before the sample, not against the growth (printed only).
        let mut v: Vec<u8> = Vec::new();
        v.resize(64 << 20, 1);
        core::hint::black_box(&v);
        let file = format!("{}/memory.current", std::str::from_utf8(cg.dir()).unwrap());
        let kernel = parse_limit(std::fs::read_to_string(&file).unwrap().as_bytes()).unwrap();
        let now = sys::now_coarse_ns() + SAMPLE_INTERVAL_NS;
        assert!(cg.sample(now));
        let after = cg.current();
        eprintln!("cgroup: memory.current {before} -> {after} after 64 MiB (kernel says {kernel})");
        assert!(
            after.abs_diff(kernel) <= 32 << 20,
            "sample {after} vs file {kernel}"
        );
        drop(v);
    }
}

/// Kani check of `Cgroup`'s `Display` (record under `verus/seams/`).
#[cfg(kani)]
mod kani_fmt {
    use super::{Cgroup, State};
    use crate::kani_proofs::fmt::formats;
    use core::sync::atomic::Ordering::Relaxed;

    /// The longest directory checked.
    const DIR_MAX: u16 = 3;

    /// S334 `DisplaySpecImpl for sys::cgroup::Cgroup` (`fmt_req` true):
    /// every state, every figure, and every directory of up to `DIR_MAX`
    /// bytes (every byte value) with every mount prefix of it, the type
    /// invariant's `mount_len <= dir_len` (what `mount()` and `path()`
    /// slice by).
    #[kani::proof]
    #[kani::unwind(22)]
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
            &c.events_high,
            &c.events_max,
            &c.high_rises,
            &c.max_rises,
        ] {
            a.store(kani::any(), Relaxed);
        }
        formats(&c);
    }
}
