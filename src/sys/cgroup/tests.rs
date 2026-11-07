/// `untracked_in_place` writes `Cgroup::untracked(state)` over storage
/// that held anything before: every field compares equal.
#[test]
fn untracked_in_place_is_untracked() {
    for state in [State::Disabled, State::NoUnifiedPath, State::Tracked] {
        let want = Cgroup::untracked(state);
        let mut slot = MaybeUninit::<Cgroup>::uninit();
        // SAFETY: the slot's bytes are writable.
        unsafe {
            core::ptr::write_bytes(
                slot.as_mut_ptr() as *mut u8,
                0xA5,
                core::mem::size_of::<Cgroup>(),
            )
        };
        untracked_in_place(&mut slot, state);
        // SAFETY: initialised just above.
        let got = unsafe { slot.assume_init_ref() };
        let r = Ordering::Relaxed;
        assert_eq!(got.dir, want.dir);
        assert_eq!(
            (got.dir_len, got.mount_len, got.state),
            (want.dir_len, want.mount_len, want.state)
        );
        assert_eq!(
            (got.has_limit_files, got.has_current, got.has_events),
            (want.has_limit_files, want.has_current, want.has_events)
        );
        let words = |c: &Cgroup| {
            let mut v = std::vec![
                c.limit.load(r),
                c.high.load(r),
                c.current.load(r),
                c.current_stale.load(r) as u64,
                c.sampled_at.load(r),
                c.limits_at.load(r),
                c.events_low.load(r),
                c.events_high.load(r),
                c.events_max.load(r),
                c.events_oom.load(r),
                c.events_oom_kill.load(r),
                c.high_rises.load(r),
                c.max_rises.load(r),
            ];
            v.extend(c.level_limit.iter().map(|a| a.load(r)));
            v.extend(c.level_current.iter().map(|a| a.load(r)));
            v
        };
        assert_eq!(words(got), words(&want), "{state:?}");
    }
}

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
// The root field (`/../../docker/abc`) is ignored: discovery joins the
// `/proc/self/cgroup` path to the mount point as is, exact when the
// mount's root is the process's cgroup namespace root.
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

/// `file` streamed through `proc_cgroup_path` with a scan buffer of
/// `scan` bytes, `chunk` bytes per read; the read fails once `fail_at`
/// bytes have been handed over.
fn streamed(file: &[u8], chunk: usize, scan: usize, fail_at: Option<usize>) -> Option<Vec<u8>> {
    let mut b: Vec<u8> = std::vec![0; scan];
    let mut pos = 0;
    let end = fail_at.unwrap_or(file.len()).min(file.len());
    proc_cgroup_path(&mut b, |dst| {
        if pos == end && fail_at.is_some() {
            return None;
        }
        let n = chunk.min(dst.len()).min(end - pos);
        dst[..n].copy_from_slice(&file[pos..pos + n]);
        pos += n;
        Some(n)
    })
    .map(|p| p.to_vec())
}

/// A Kubernetes container's cgroup path (191 bytes).
const K8S_PATH: &str = "/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod0f6a1c2e_3b4d_4e5f_8a9b_0c1d2e3f4a5b.slice/cri-containerd-9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08.scope";

/// A hybrid v1+v2 host's `/proc/self/cgroup`: a dozen v1 hierarchies'
/// lines carrying [`K8S_PATH`] (about 2.3 KiB), then the `0::` line
/// carrying `unified`.
fn proc_cgroup_hybrid(unified: &str) -> Vec<u8> {
    let mut file = String::new();
    for h in [
        "12:pids",
        "11:hugetlb",
        "10:net_cls,net_prio",
        "9:cpu,cpuacct",
        "8:memory",
        "7:blkio",
        "6:freezer",
        "5:devices",
        "4:cpuset",
        "3:perf_event",
        "2:rdma",
        "1:name=systemd",
    ] {
        file.push_str(&std::format!("{h}:{K8S_PATH}\n"));
    }
    file.push_str(&std::format!("0::{unified}\n"));
    file.into_bytes()
}

#[test]
fn proc_cgroup_streams_a_hybrid_file() {
    let file = proc_cgroup_hybrid(K8S_PATH);
    assert!(file.len() > SCAN_BUF);
    for chunk in [1, 7, 64, 1024, 4096] {
        assert_eq!(
            streamed(&file, chunk, SCAN_BUF, None).as_deref(),
            Some(K8S_PATH.as_bytes()),
            "chunk {chunk}"
        );
    }
    // The longest `0::` line the scan buffer holds, newline included.
    let long = std::format!("/{}", "x".repeat(SCAN_BUF - 5));
    let file = proc_cgroup_hybrid(&long);
    assert_eq!(
        streamed(&file, 4096, SCAN_BUF, None).as_deref(),
        Some(long.as_bytes())
    );
    // The same file agrees with the whole-file reference.
    assert_eq!(cgroup_path(&file), Some(long.as_bytes()));
    assert_eq!(
        streamed(PROC_CGROUP_MIXED, 5, SCAN_BUF, None).as_deref(),
        Some(&b"/docker/abc"[..])
    );
    assert_eq!(streamed(PROC_CGROUP_V1, 5, SCAN_BUF, None), None);
    // A final line without a newline at end of file is whole.
    assert_eq!(
        streamed(b"0::/a/b", 3, 64, None).as_deref(),
        Some(&b"/a/b"[..])
    );
}

#[test]
fn proc_cgroup_never_takes_a_cut_line() {
    let file = proc_cgroup_hybrid(K8S_PATH);
    let zero = file.len() - K8S_PATH.len() - 4;
    // Every prefix that ends inside the `0::` line names a wrong
    // (cut) path when taken as the whole file: what a 1 KiB read did
    // on a hybrid host whose `0::` line straddled 1 KiB.
    for end in zero + 4..file.len() - 1 {
        let cut = cgroup_path(&file[..end]).unwrap();
        assert!(cut.len() < K8S_PATH.len() && K8S_PATH.as_bytes().starts_with(cut));
        // A read error there ends the scan with nothing taken.
        assert_eq!(
            streamed(&file, 64, SCAN_BUF, Some(end)),
            None,
            "error at {end}"
        );
    }
    // An error in the v1 lines, too.
    assert_eq!(streamed(&file, 64, SCAN_BUF, Some(1024)), None);
    // A `0::` line longer than the scan buffer is dropped, not cut.
    let long = std::format!("/{}", "x".repeat(SCAN_BUF));
    assert_eq!(
        streamed(&proc_cgroup_hybrid(&long), 4096, SCAN_BUF, None),
        None
    );
}

#[test]
fn proc_cgroup_exact_fill() {
    // A line plus its newline that fill the scan buffer exactly is
    // whole; a line that fills it without the newline is dropped,
    // whatever follows.
    let path = std::format!("/{}", "x".repeat(64 - 5));
    let line = std::format!("0::{path}\n");
    assert_eq!(line.len(), 64);
    assert_eq!(
        streamed(line.as_bytes(), 64, 64, None).as_deref(),
        Some(path.as_bytes())
    );
    let longer = std::format!("0::{path}y\n");
    assert_eq!(streamed(longer.as_bytes(), 64, 64, None), None);
    assert_eq!(streamed(&longer.as_bytes()[..64], 64, 64, None), None);
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
    let many = b"35 24 0:30 / /mnt/cg rw shared:9 master:3 propagate_from:1 - cgroup2 cgroup2 rw\n";
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
    // A missing line (here `oom_kill`) reads 0.
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
    // A sample without the `oom_kill` line: the field reads 0, the
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
    let v = walk_min(dir.as_bytes(), mount.len(), |_, d| {
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

/// Sample `/sys/fs/cgroup/a/b` from captured files: one limit walk and
/// one sample. Returns the cgroup, its band and every file read.
fn sample_tree(
    t: &BTreeMap<String, BTreeMap<String, String>>,
) -> (Cgroup, crate::policy::Band, Vec<String>) {
    let cg = Cgroup::at(b"/sys/fs/cgroup", b"/a/b");
    let (band, reads) = sample_at(&cg, t, LIMIT_INTERVAL_NS);
    (cg, band, reads)
}

/// One sample of `cg` at `now` from captured files (with the limit
/// walk when [`LIMIT_INTERVAL_NS`] has passed); its band and every
/// file read.
fn sample_at(
    cg: &Cgroup,
    t: &BTreeMap<String, BTreeMap<String, String>>,
    now: u64,
) -> (crate::policy::Band, Vec<String>) {
    let mut reads = Vec::new();
    let mut read = |d: &[u8], name: &[u8], buf: &mut [u8]| {
        let (d, name) = (
            std::str::from_utf8(d).unwrap(),
            std::str::from_utf8(name).unwrap(),
        );
        reads.push(std::format!("{d}/{name}"));
        let c = t.get(d)?.get(name)?.as_bytes();
        buf[..c.len()].copy_from_slice(c);
        Some(c.len())
    };
    cg.sample_now_with(now, &mut read);
    (crate::policy::cgroup_band(cg), reads)
}

/// The kernel holds each level to its own limit by its own usage,
/// which for an ancestor counts its other children too: the band is
/// the most severe over the levels with a limit, not the leaf's usage
/// against the smallest limit.
#[test]
fn ancestor_usage_against_its_own_limit() {
    use crate::policy::Band;
    const MIB: u64 = 1 << 20;
    let mib = |n: u64| std::format!("{}\n", n * MIB);
    // Parent limited to 1000 MiB holding 950 MiB, the leaf 100 MiB of
    // it with no limit of its own: the parent is at 95%.
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.max", "max\n"),
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(100)),
        ("/sys/fs/cgroup/a", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(950)),
        ("/sys/fs/cgroup", "memory.current", &mib(4000)),
    ]);
    let (cg, band, reads) = sample_tree(&t);
    assert_eq!(cg.limit(), 1000 * MIB);
    assert_eq!(cg.current(), 100 * MIB);
    assert_eq!(band, Band::Immediate);
    // The unlimited mount level's usage is never read.
    assert!(
        !reads.contains(&String::from("/sys/fs/cgroup/memory.current")),
        "{reads:?}"
    );
    // The parent at 80% through `memory.high`: the scaled band.
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(100)),
        ("/sys/fs/cgroup/a", "memory.high", &mib(1000)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(800)),
    ]);
    assert!(matches!(sample_tree(&t).1, Band::Scaled(_)));
    // The leaf's own limit binds: 450 of 500 MiB is 90%, while the
    // parent is at 60%.
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.max", &mib(500)),
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(450)),
        ("/sys/fs/cgroup/a", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(600)),
    ]);
    assert_eq!(sample_tree(&t).1, Band::Immediate);
    // Everyone well under their limits: `Low`.
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.max", &mib(500)),
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(100)),
        ("/sys/fs/cgroup/a", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(300)),
    ]);
    assert_eq!(sample_tree(&t).1, Band::Low);
    // No limit anywhere: `Unlimited`, and only the leaf's usage read.
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(100)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(950)),
    ]);
    let (_, band, reads) = sample_tree(&t);
    assert_eq!(band, Band::Unlimited);
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.ends_with("/memory.current"))
            .collect::<Vec<_>>(),
        ["/sys/fs/cgroup/a/b/memory.current"]
    );
    // A parent that gains a limit between two walks counts in the
    // sample that walks: the walk runs before the usage reads.
    let cg = Cgroup::at(b"/sys/fs/cgroup", b"/a/b");
    assert_eq!(sample_at(&cg, &t, LIMIT_INTERVAL_NS).0, Band::Unlimited);
    let t = tree(&[
        ("/sys/fs/cgroup/a/b", "memory.current", &mib(100)),
        ("/sys/fs/cgroup/a", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/a", "memory.current", &mib(950)),
    ]);
    assert_eq!(sample_at(&cg, &t, 2 * LIMIT_INTERVAL_NS).0, Band::Immediate);
}

/// A path deeper than [`LEVELS`]: an ancestor within them is held to
/// its own usage; one beyond them only through the leaf's usage
/// against the effective limit, and its usage is never read.
#[test]
fn ancestor_usage_beyond_the_tracked_levels() {
    use crate::policy::Band;
    const MIB: u64 = 1 << 20;
    let mib = |n: u64| std::format!("{}\n", n * MIB);
    // 17 levels under the mount: the leaf is level 0, `/l1/l2` level
    // 15, `/l1` level 16 and the mount level 17.
    let path: String = (1..=17).map(|i| std::format!("/l{i}")).collect();
    let leaf = std::format!("/sys/fs/cgroup{path}");
    let cg = Cgroup::at(b"/sys/fs/cgroup", path.as_bytes());
    let t = tree(&[
        (&leaf, "memory.current", &mib(100)),
        ("/sys/fs/cgroup/l1/l2", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/l1/l2", "memory.current", &mib(950)),
    ]);
    assert_eq!(sample_at(&cg, &t, LIMIT_INTERVAL_NS).0, Band::Immediate);
    let cg = Cgroup::at(b"/sys/fs/cgroup", path.as_bytes());
    let t = tree(&[
        (&leaf, "memory.current", &mib(100)),
        ("/sys/fs/cgroup/l1", "memory.max", &mib(1000)),
        ("/sys/fs/cgroup/l1", "memory.current", &mib(950)),
    ]);
    let (band, reads) = sample_at(&cg, &t, LIMIT_INTERVAL_NS);
    assert_eq!(cg.limit(), 1000 * MIB);
    assert_eq!(band, Band::Low);
    assert!(
        !reads.contains(&String::from("/sys/fs/cgroup/l1/memory.current")),
        "{reads:?}"
    );
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
    let mut b: Vec<u8> = std::vec![0; buf];
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
