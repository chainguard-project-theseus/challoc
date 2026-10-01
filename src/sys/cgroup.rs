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

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use super::parse_decimal;

/// Longest cgroup directory (mount point plus path) that is tracked. Real
/// paths are a few hundred bytes (Kubernetes' pod and container scopes);
/// anything longer is reported as untracked rather than truncated.
pub const PATH_CAP: usize = 2048;
/// Room after the directory for `/` plus the longest file name read here
/// (`memory.current`, 14 bytes) plus the NUL, rounded up.
const FILE_ROOM: usize = 32;
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

// ---------------------------------------------------------------------------
// Pure parsing (unit tested against captured contents).
// ---------------------------------------------------------------------------

/// Split `bytes` into lines; a final line without a newline counts.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty())
}

/// Leading decimal number of `bytes`, ignoring what follows the digits.
/// `None` without a digit or on overflow (`sys::parse_decimal`).
fn parse_u64_prefix(bytes: &[u8]) -> Option<u64> {
    parse_decimal(bytes)
}

/// Strip leading and trailing ASCII whitespace.
fn trim(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if !first.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    while let [rest @ .., last] = b {
        if !last.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    b
}

/// The unified-hierarchy path of one `/proc/self/cgroup` line: `0::/a/b`
/// gives `/a/b`; inside a cgroup namespace the line is `0::/`. `None` for
/// any other line (a v1 controller's `N:name:/path`).
pub fn cgroup_path_from_line(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"0::")?;
    let rest = trim(rest);
    (rest.first() == Some(&b'/')).then_some(rest)
}

/// The unified-hierarchy path from the whole of `/proc/self/cgroup`, or
/// `None` when there is no `0::` line (a cgroup v1 only system).
pub fn cgroup_path(contents: &[u8]) -> Option<&[u8]> {
    lines(contents).find_map(cgroup_path_from_line)
}

/// The mount point of one `/proc/self/mountinfo` line if it mounts a
/// `cgroup2` filesystem. The format is
/// `id parent major:minor root mount_point options [optional...] - fstype source super_options`;
/// the optional fields vary in number, which is what the ` - ` separator is
/// for. Octal escapes in the mount point (`\040` for a space) are left as
/// they are: no cgroup2 mount has one.
pub fn cgroup2_mount_from_line(line: &[u8]) -> Option<&[u8]> {
    let sep = line.windows(3).position(|w| w == b" - ")?;
    let (pre, post) = (&line[..sep], &line[sep + 3..]);
    let fstype = post.split(|&b| b == b' ').next()?;
    if fstype != b"cgroup2" {
        return None;
    }
    let mount = pre.split(|&b| b == b' ').nth(4)?;
    (mount.first() == Some(&b'/')).then_some(mount)
}

/// The first cgroup2 mount point in `/proc/self/mountinfo` contents, or
/// `None` when there is none (no unified hierarchy mounted).
pub fn cgroup2_mount(contents: &[u8]) -> Option<&[u8]> {
    lines(contents).find_map(cgroup2_mount_from_line)
}

/// `memory.max`, `memory.high` or `memory.current`: a decimal byte count,
/// or `max` meaning [`UNLIMITED`]. `None` for anything else (an absent
/// file is the caller's `None` before this is reached).
pub fn parse_limit(contents: &[u8]) -> Option<u64> {
    let t = trim(contents);
    if t == b"max" {
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

pub fn parse_events(contents: &[u8]) -> Events {
    let mut e = Events::default();
    for line in lines(contents) {
        let mut it = line.splitn(2, |&b| b == b' ');
        let (Some(key), Some(val)) = (it.next(), it.next()) else { continue };
        let Some(v) = parse_u64_prefix(trim(val)) else { continue };
        match key {
            b"low" => e.low = v,
            b"high" => e.high = v,
            b"max" => e.max = v,
            b"oom" => e.oom = v,
            b"oom_kill" => e.oom_kill = v,
            _ => {}
        }
    }
    e
}

/// Walk from the leaf directory `dir` up to the cgroup2 mount (the first
/// `mount_len` bytes of `dir`, inclusive) and return the minimum of what
/// `read` yields per level; `read` gets the directory of the level and
/// answers `None` where the file is absent. [`UNLIMITED`] when no level
/// has a limit. Pure: the reader is a closure so the tests feed it
/// captured contents.
pub fn walk_min<F: FnMut(&[u8]) -> Option<u64>>(dir: &[u8], mount_len: usize, mut read: F) -> u64 {
    let mut min = UNLIMITED;
    let mut end = dir.len();
    loop {
        if let Some(v) = read(&dir[..end]) {
            min = min.min(v);
        }
        if end <= mount_len {
            break;
        }
        match dir[mount_len..end].iter().rposition(|&b| b == b'/') {
            Some(i) => end = mount_len + i,
            None => break,
        }
    }
    min
}

/// Join `mount` and `path` into `out` as the cgroup directory without a
/// trailing slash: (`/sys/fs/cgroup`, `/a/b`) is `/sys/fs/cgroup/a/b` and
/// (`/sys/fs/cgroup`, `/`) is `/sys/fs/cgroup`. Returns the directory
/// length and the mount's length within it, or `None` when it would not
/// fit in `out` less [`FILE_ROOM`].
pub fn join_dir(out: &mut [u8], mount: &[u8], path: &[u8]) -> Option<(usize, usize)> {
    let mount = mount.strip_suffix(b"/").unwrap_or(mount);
    let path = if path == b"/" { &b""[..] } else { path.strip_suffix(b"/").unwrap_or(path) };
    let len = mount.len() + path.len();
    if len + FILE_ROOM > out.len() {
        return None;
    }
    out[..mount.len()].copy_from_slice(mount);
    out[mount.len()..len].copy_from_slice(path);
    Some((len, mount.len()))
}

/// Feed `buf`-sized chunks from `read` to `on_line` one complete line at a
/// time (without the newline), carrying a partial line across chunks;
/// stops early when `on_line` returns `true`. A line longer than `buf` is
/// dropped. `read` returns the bytes it filled, 0 at end of file, or
/// `None` on error (which ends the scan).
pub fn scan_lines<R, F>(buf: &mut [u8], mut read: R, mut on_line: F)
where
    R: FnMut(&mut [u8]) -> Option<usize>,
    F: FnMut(&[u8]) -> bool,
{
    let mut len = 0;
    let mut dropping = false;
    loop {
        let n = match read(&mut buf[len..]) {
            Some(0) | None => break,
            Some(n) => n,
        };
        len += n;
        let mut start = 0;
        while let Some(i) = buf[start..len].iter().position(|&b| b == b'\n') {
            let line = &buf[start..start + i];
            start += i + 1;
            if dropping {
                dropping = false;
            } else if on_line(line) {
                return;
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
    if len > 0 && !dropping {
        on_line(&buf[..len]);
    }
}

// ---------------------------------------------------------------------------
// Runtime state.
// ---------------------------------------------------------------------------

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
    /// Nothing tracked, for `cgroup:false` and hosts without cgroup2.
    pub const fn untracked(state: State) -> Cgroup {
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

    pub fn state(&self) -> State {
        self.state
    }

    /// Reads work and the figures are live.
    #[inline]
    pub fn tracked(&self) -> bool {
        self.state == State::Tracked
    }

    /// The cgroup directory (empty when untracked).
    pub fn dir(&self) -> &[u8] {
        &self.dir[..self.dir_len as usize]
    }

    /// The cgroup2 mount point (empty when untracked).
    pub fn mount(&self) -> &[u8] {
        &self.dir[..self.mount_len as usize]
    }

    /// The path relative to the mount, as `/proc/self/cgroup` gave it
    /// (`/` at the mount itself).
    pub fn path(&self) -> &[u8] {
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
        (self.high_rises.load(Ordering::Relaxed), self.max_rises.load(Ordering::Relaxed))
    }

    pub fn has_limit_files(&self) -> bool {
        self.has_limit_files
    }

    pub fn has_current(&self) -> bool {
        self.has_current
    }

    pub fn has_events(&self) -> bool {
        self.has_events
    }

    /// Coarse time of the last `memory.current` sample (0 before the first).
    pub fn sampled_at(&self) -> u64 {
        self.sampled_at.load(Ordering::Relaxed)
    }
}

impl fmt::Display for Cgroup {
    /// The `cgroup:` line of `malloc_stats_print`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Bytes<'a>(&'a [u8]);
        impl fmt::Display for Bytes<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for &b in self.0 {
                    f.write_str(if b == b' ' || b.is_ascii_graphic() { unsafe_char(b) } else { "?" })?;
                }
                Ok(())
            }
        }
        /// One printable ASCII byte as a `&'static str`.
        fn unsafe_char(b: u8) -> &'static str {
            const TABLE: &[u8; 95] =
                b" !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
            let i = (b - b' ') as usize;
            // SAFETY: `TABLE` is ASCII, so every one-byte window is UTF-8.
            unsafe { core::str::from_utf8_unchecked(&TABLE[i..i + 1]) }
        }
        struct Limit(u64);
        impl fmt::Display for Limit {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                if self.0 == UNLIMITED { f.write_str("max") } else { write!(f, "{}", self.0) }
            }
        }
        match self.state {
            State::Disabled => return f.write_str("cgroup: not tracked (cgroup:false)"),
            State::NoUnifiedPath => return f.write_str("cgroup: not tracked (no unified hierarchy in /proc/self/cgroup)"),
            State::NoMount => return f.write_str("cgroup: not tracked (no cgroup2 mount)"),
            State::PathTooLong => return f.write_str("cgroup: not tracked (path too long)"),
            State::Tracked => {}
        }
        let (hr, mr) = self.rises();
        write!(
            f,
            "cgroup: {} (mount {}, path {}) limit {} high {} current {} events high {} (+{}) max {} (+{})",
            Bytes(self.dir()),
            Bytes(self.mount()),
            Bytes(self.path()),
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

mod linux {
    use super::*;
    use crate::sys::{self, O_RDONLY, read_small_file};

    /// `dir/name`, NUL-terminated, in `out`.
    fn file_path<'a>(out: &'a mut [u8; PATH_CAP + FILE_ROOM], dir: &[u8], name: &[u8]) -> &'a [u8] {
        let n = dir.len();
        out[..n].copy_from_slice(dir);
        out[n] = b'/';
        out[n + 1..n + 1 + name.len()].copy_from_slice(name);
        out[n + 1 + name.len()] = 0;
        &out[..n + name.len() + 2]
    }

    /// Read `dir/name` into `buf`; `None` when absent.
    fn read_in(dir: &[u8], name: &[u8], buf: &mut [u8]) -> Option<usize> {
        let mut p = [0u8; PATH_CAP + FILE_ROOM];
        let path = file_path(&mut p, dir, name);
        read_small_file(path, buf)
    }

    /// `dir/name` parsed as a limit file; `None` when absent or malformed.
    fn read_limit(dir: &[u8], name: &[u8]) -> Option<u64> {
        let mut buf = [0u8; 64];
        let n = read_in(dir, name, &mut buf)?;
        parse_limit(&buf[..n])
    }

    impl Cgroup {
        /// Design 10 step 7: find the cgroup and read its limits once.
        /// With `enabled == false` (`cgroup:false`) nothing is opened.
        pub fn discover(enabled: bool) -> Cgroup {
            if !enabled {
                return Cgroup::untracked(State::Disabled);
            }
            let mut cg = Cgroup::untracked(State::NoUnifiedPath);
            let mut pathbuf = [0u8; FILE_BUF];
            let Some(n) = read_small_file(b"/proc/self/cgroup\0", &mut pathbuf) else { return cg };
            let Some(path) = cgroup_path(&pathbuf[..n]) else { return cg };

            let mut mount = [0u8; PATH_CAP];
            let mut mount_len = 0usize;
            let mut scan = [0u8; SCAN_BUF];
            if let Ok(fd) = sys::open(b"/proc/self/mountinfo\0", O_RDONLY) {
                scan_lines(
                    &mut scan,
                    |b| sys::read_full(fd, b).ok(),
                    |line| match cgroup2_mount_from_line(line) {
                        Some(m) if m.len() <= PATH_CAP => {
                            mount[..m.len()].copy_from_slice(m);
                            mount_len = m.len();
                            true
                        }
                        _ => false,
                    },
                );
                let _ = sys::close(fd);
            }
            if mount_len == 0 {
                cg.state = State::NoMount;
                return cg;
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
        fn walk_limits(&self, now: u64) {
            self.limits_at.store(now, Ordering::Relaxed);
            let (dir, mlen) = (self.dir(), self.mount_len as usize);
            self.limit.store(walk_min(dir, mlen, |d| read_limit(d, b"memory.max")), Ordering::Relaxed);
            self.high.store(walk_min(dir, mlen, |d| read_limit(d, b"memory.high")), Ordering::Relaxed);
        }

        /// Read `memory.current` and `memory.events` now, unconditionally,
        /// and re-walk the limits if [`LIMIT_INTERVAL_NS`] has passed.
        fn sample_now(&self, now: u64) {
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
        pub fn sample(&self, now: u64) -> bool {
            if !self.tracked() {
                return false;
            }
            let last = self.sampled_at.load(Ordering::Relaxed);
            if now.wrapping_sub(last) < SAMPLE_INTERVAL_NS {
                return false;
            }
            if self.sampled_at.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
                return false;
            }
            self.sample_now(now);
            true
        }
    }
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
        assert_eq!(cgroup_path(b"10::/x\n"), None, "only hierarchy 0 is the unified one");
        assert_eq!(cgroup_path_from_line(b"0::/a/b"), Some(&b"/a/b"[..]));
    }

    #[test]
    fn mountinfo_lines() {
        assert_eq!(cgroup2_mount(MOUNTINFO_DEFAULT), Some(&b"/sys/fs/cgroup"[..]));
        assert_eq!(cgroup2_mount(MOUNTINFO_DOCKER), Some(&b"/sys/fs/cgroup"[..]));
        assert_eq!(cgroup2_mount(MOUNTINFO_HYBRID), Some(&b"/sys/fs/cgroup/unified"[..]));
        assert_eq!(cgroup2_mount(MOUNTINFO_V1), None);
        assert_eq!(cgroup2_mount(b""), None);
        // The optional-field count varies; the separator keeps field 5 honest.
        let many = b"35 24 0:30 / /mnt/cg rw shared:9 master:3 propagate_from:1 - cgroup2 cgroup2 rw\n";
        assert_eq!(cgroup2_mount(many), Some(&b"/mnt/cg"[..]));
        // A line without the separator or with too few fields is skipped.
        assert_eq!(cgroup2_mount(b"garbage cgroup2\n35 24 0:30 / - cgroup2 x y\n"), None);
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
        assert_eq!(e, Events { low: 0, high: 3, max: 1, oom: 0, oom_kill: 0 });
        // Newer kernels add lines; missing lines are 0; junk is ignored.
        let e = parse_events(b"low 7\nhigh 2\nmax 5\noom 1\noom_kill 1\noom_group_kill 0\n");
        assert_eq!(e, Events { low: 7, high: 2, max: 5, oom: 1, oom_kill: 1 });
        assert_eq!(parse_events(b"high 9\n"), Events { high: 9, ..Events::default() });
        assert_eq!(parse_events(b""), Events::default());
        assert_eq!(parse_events(b"high x\nmax\n"), Events::default());
        // A 4.5..4.12 file has no `oom_kill` line: it reads 0.
        assert_eq!(parse_events(b"low 1\nhigh 2\nmax 3\noom 4\n"), Events { low: 1, high: 2, max: 3, oom: 4, oom_kill: 0 });
    }

    /// The sampled counters are the file's, all five, and the rise counts
    /// move with `high` and `max` alone: three samples of a cgroup that
    /// hit `memory.high` twice and was OOM-killed once.
    #[test]
    fn sampled_events_carry_every_counter() {
        let cg = Cgroup::untracked(State::Tracked);
        assert_eq!(cg.events(), Events::default(), "zero before the first sample");
        cg.note_events(parse_events(b"low 0\nhigh 3\nmax 1\noom 0\noom_kill 0\n"));
        assert_eq!(cg.events(), Events { low: 0, high: 3, max: 1, oom: 0, oom_kill: 0 });
        assert_eq!(cg.rises(), (1, 1), "the first sample rises from zero");
        cg.note_events(parse_events(b"low 7\nhigh 5\nmax 1\noom 1\noom_kill 1\noom_group_kill 0\n"));
        assert_eq!(cg.events(), Events { low: 7, high: 5, max: 1, oom: 1, oom_kill: 1 });
        assert_eq!(cg.rises(), (2, 1), "high rose, max did not; low and oom are not rises");
        cg.note_events(parse_events(b"low 7\nhigh 5\nmax 2\noom 1\noom_kill 1\n"));
        assert_eq!(cg.events(), Events { low: 7, high: 5, max: 2, oom: 1, oom_kill: 1 });
        assert_eq!(cg.rises(), (2, 2));
        // A kernel without the `oom_kill` line: the field reads 0, the
        // others are kept.
        cg.note_events(parse_events(b"low 8\nhigh 5\nmax 2\noom 2\n"));
        assert_eq!(cg.events(), Events { low: 8, high: 5, max: 2, oom: 2, oom_kill: 0 });
        assert_eq!(cg.rises(), (2, 2));
        let s = std::format!("{cg}");
        assert!(s.ends_with(" events high 5 (+2) max 2 (+2)"), "{s}");
    }

    /// A captured tree: directory -> file -> contents. Absent = `None`.
    fn tree(entries: &[(&str, &str, &str)]) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut t: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (d, f, c) in entries {
            t.entry(String::from(*d)).or_default().insert(String::from(*f), String::from(*c));
        }
        t
    }

    fn walk(t: &BTreeMap<String, BTreeMap<String, String>>, dir: &str, mount: &str, file: &str) -> (u64, Vec<String>) {
        let mut visited = Vec::new();
        let v = walk_min(dir.as_bytes(), mount.len(), |d| {
            let d = std::str::from_utf8(d).unwrap();
            visited.push(String::from(d));
            t.get(d).and_then(|files| files.get(file)).and_then(|c| parse_limit(c.as_bytes()))
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
        assert_eq!(visited, ["/sys/fs/cgroup/a/b", "/sys/fs/cgroup/a", "/sys/fs/cgroup"]);
        assert_eq!(walk(&t, "/sys/fs/cgroup/a/b", m, "memory.high").0, 1_073_741_824);
        // Leaf lower than the parent: the leaf wins.
        let t = tree(&[("/sys/fs/cgroup/a/b", "memory.max", "268435456\n"), ("/sys/fs/cgroup/a", "memory.max", "536870912\n")]);
        assert_eq!(walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max").0, 268_435_456);
        // Nothing anywhere (cgroup v1 memory controller, or a root cgroup).
        let t = tree(&[]);
        assert_eq!(walk(&t, "/sys/fs/cgroup/a/b", m, "memory.max").0, UNLIMITED);
        // At the mount itself (a cgroup namespace, path `/`): one level.
        let t = tree(&[("/sys/fs/cgroup", "memory.max", "536870912\n")]);
        let (limit, visited) = walk(&t, "/sys/fs/cgroup", m, "memory.max");
        assert_eq!(limit, 536_870_912);
        assert_eq!(visited, ["/sys/fs/cgroup"]);
        // A malformed file at one level does not hide the others.
        let t = tree(&[("/sys/fs/cgroup/a/b", "memory.max", "garbage\n"), ("/sys/fs/cgroup/a", "memory.max", "4096\n")]);
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
        assert_eq!(visited, ["/sys/fs/cgroup/a/b", "/sys/fs/cgroup/a", "/sys/fs/cgroup"]);
        // A directory shorter than the mount length: one read of the
        // whole directory, then stop.
        let (limit, visited) = walk(&t, "/sys/fs/cgroup", "/sys/fs/cgroup/unified", "memory.max");
        assert_eq!(limit, 4096);
        assert_eq!(visited, ["/sys/fs/cgroup"]);
        let (limit, visited) = walk(&t, "/sys", "/sys/fs/cgroup", "memory.max");
        assert_eq!(limit, UNLIMITED);
        assert_eq!(visited, ["/sys"]);
        // The mount at the root: every `/` of the path is a level.
        let t = tree(&[("/a/b", "memory.max", "8192\n"), ("", "memory.max", "4096\n")]);
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
        assert_eq!(join_dir(&mut small, b"/sys/fs/cgroup", b"/a"), None, "must leave FILE_ROOM");
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
        assert_eq!((n, m), (0, 0), "the root cgroup at a root mount is the empty directory");
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
        let all: Vec<Vec<u8>> = MOUNTINFO_HYBRID.split(|&b| b == b'\n').filter(|l| !l.is_empty()).map(|l| l.to_vec()).collect();
        for chunk in [1, 7, 64, 4096] {
            assert_eq!(scanned(MOUNTINFO_HYBRID, chunk, 256, None), all, "chunk {chunk}");
        }
        // Early stop.
        let first = scanned(MOUNTINFO_HYBRID, 13, 256, Some(&all[1]));
        assert_eq!(first, all[..2]);
        // A line longer than the buffer is dropped; the ones after it survive.
        let mut long = Vec::new();
        long.extend_from_slice(b"short\n");
        long.extend(core::iter::repeat_n(b'L', 300));
        long.extend_from_slice(b"\nafter\nlast");
        assert_eq!(scanned(&long, 50, 128, None), [&b"short"[..], b"after", b"last"]);
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
            assert_eq!(found.as_deref(), Some(&b"/sys/fs/cgroup/unified"[..]), "chunk {chunk}");
        }
    }

    #[test]
    fn untracked_states_display() {
        for (state, needle) in [
            (State::Disabled, "cgroup: not tracked (cgroup:false)"),
            (State::NoUnifiedPath, "cgroup: not tracked (no unified hierarchy"),
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
        let path = cgroup_path(proc_cgroup.as_bytes()).map(|p| String::from_utf8(p.to_vec()).unwrap());
        let mount = cgroup2_mount(mountinfo.as_bytes()).map(|m| String::from_utf8(m.to_vec()).unwrap());
        eprintln!("cgroup: /proc/self/cgroup path {path:?}, cgroup2 mount {mount:?}");
        match (&path, &mount) {
            (Some(path), Some(mount)) => {
                assert!(cg.tracked(), "{line}");
                assert_eq!(std::str::from_utf8(cg.mount()).unwrap(), mount);
                assert_eq!(std::str::from_utf8(cg.path()).unwrap(), path);
                let dir = if path == "/" { mount.clone() } else { format!("{mount}{path}") };
                assert_eq!(std::str::from_utf8(cg.dir()).unwrap(), dir);
                // The limit is `max` or a number, and equal to what std reads.
                let expect_limit = std_walk(&dir, mount, "memory.max");
                let expect_high = std_walk(&dir, mount, "memory.high");
                assert_eq!(cg.limit(), expect_limit, "{line}");
                assert_eq!(cg.high(), expect_high, "{line}");
                assert_eq!(cg.has_limit_files(), std::path::Path::new(&format!("{dir}/memory.max")).exists());
                assert_eq!(cg.has_current(), std::path::Path::new(&format!("{dir}/memory.current")).exists());
                assert_eq!(cg.has_events(), std::path::Path::new(&format!("{dir}/memory.events")).exists());
                if cg.has_events() {
                    // Discovery sampled the file: every counter is the
                    // kernel's (they only grow, so at most a later rise
                    // separates the two reads).
                    let now = parse_events(std::fs::read(format!("{dir}/memory.events")).unwrap().as_slice());
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
                    read_opt(&format!("{dir}/memory.max")).unwrap_or_else(|| "(absent)".into()).trim(),
                    read_opt(&format!("{dir}/memory.high")).unwrap_or_else(|| "(absent)".into()).trim(),
                    read_opt(&format!("{dir}/memory.current")).unwrap_or_else(|| "(absent)".into()).trim(),
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
                    eprintln!("cgroup: effective limit {} matches CHALLOC_EXPECT_CGROUP_LIMIT", cg.limit());
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
            eprintln!("cgroup: untracked here ({}), skipping the rate-limit test", cg.state() as u8);
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
        assert!(after.abs_diff(kernel) <= 32 << 20, "sample {after} vs file {kernel}");
        drop(v);
    }
}
