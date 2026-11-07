use super::*;
use crate::sys;
use std::string::String;
use std::{eprintln, format, vec::Vec};

/// A deferred record is the untracked placeholder until it is due
/// (`DEFER_NS` after its birth) or read; one call discovers, and the
/// record is then what an immediate `discover` finds. A discovery cut
/// short by a `fork` starts over in the child.
#[test]
fn deferred_discovery_waits_then_runs_once() {
    let born = 1_000_000_000;
    let d = Deferred::later(born);
    assert!(d.get().is_none());
    assert!(!d.due(born));
    assert!(!d.due(born + DEFER_NS - 1));
    assert!(d.due(born + DEFER_NS));
    let mut seen = None;
    assert!(
        d.discover(
            false,
            |cg| seen = Some((cg.state(), d.get().is_none())),
            Tracked::assume_new()
        ),
        "the first call discovers"
    );
    let (state, unpublished) = seen.expect("`found` ran");
    assert!(unpublished, "`found` runs before the record is published");
    assert!(d.get().is_some());
    assert!(
        !d.discover(true, |_| (), Tracked::assume_new()),
        "and only the first"
    );
    assert!(!d.due(u64::MAX));
    let now = Cgroup::discover(true);
    assert_eq!(state, now.state());
    assert_eq!(d.get().unwrap().state(), now.state());
    assert_eq!(d.get().unwrap().dir(), now.dir());
    assert_eq!(d.get().unwrap().limit(), now.limit());
    d.fork_child();
    assert_eq!(
        d.get().unwrap().state(),
        now.state(),
        "a finished discovery stays"
    );
    let e = Deferred::later(born);
    e.phase.store(PHASE_RUNNING, Ordering::Relaxed);
    assert!(
        !e.discover(false, |_| (), Tracked::assume_new()),
        "another thread's discovery under way"
    );
    assert!(e.get().is_none());
    e.fork_child();
    assert!(e.due(born + DEFER_NS), "the child starts it over");
    assert!(e.discover(false, |_| (), Tracked::assume_new()));
    assert_eq!(e.get().unwrap().state(), now.state());
    let off = Deferred::now(Cgroup::discover(false));
    assert_eq!(off.get().unwrap().state(), State::Disabled);
    assert!(!off.due(u64::MAX) && !off.discover(true, |_| (), Tracked::assume_new()));
}

fn read_opt(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

/// `min` of a limit file over the walk, computed with std for the
/// cross-check.
fn std_walk(dir: &str, mount: &str, file: &str) -> u64 {
    let mut min = UNLIMITED;
    let mut d = String::from(dir);
    loop {
        if let Some(c) = read_opt(&format!("{d}/{file}"))
            && let Some(v) = parse_limit(c.as_bytes())
        {
            min = min.min(v);
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

/// The window only moves forward. A caller can hold a `now` read
/// before another thread sampled (`Heap::tick` reads the clock before
/// its cache GC): that `now` is not due, and the timestamp stays.
#[test]
fn an_older_now_never_moves_the_window_back() {
    let cg = Cgroup::untracked(State::Tracked);
    let t = 5 * LIMIT_INTERVAL_NS;
    assert!(cg.sample(t));
    assert!(!cg.sample(t - SAMPLE_INTERVAL_NS), "an older now sampled");
    assert!(!cg.sample(t - 1), "an older now sampled");
    assert_eq!(cg.sampled_at(), t);
    assert!(!cg.sample(t + SAMPLE_INTERVAL_NS - 1));
    assert!(cg.sample(t + SAMPLE_INTERVAL_NS));
    assert_eq!(cg.sampled_at(), t + SAMPLE_INTERVAL_NS);
}

/// A failed `memory.current` read keeps the last figure, and the
/// report marks it stale until a read succeeds. The sample still
/// counts, so the policy still sees `memory.events` and the retention.
#[test]
fn a_failed_current_read_marks_the_figure_stale() {
    let base = std::env::temp_dir().join(format!("challoc-cgroup-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let base_s = base.to_str().unwrap();
    let mut cg = Cgroup::untracked(State::Tracked);
    cg.dir[..base_s.len()].copy_from_slice(base_s.as_bytes());
    cg.dir_len = base_s.len() as u16;
    cg.mount_len = base_s.len() as u16;
    cg.has_current = true;
    cg.current.store(123, Ordering::Relaxed);
    let t = 5 * LIMIT_INTERVAL_NS;
    assert!(cg.sample(t));
    assert_eq!(cg.current(), 123);
    let line = format!("{cg}");
    assert!(line.contains(" current 123 (stale) "), "{line}");
    std::fs::write(base.join("memory.current"), "4096\n").unwrap();
    assert!(cg.sample(t + SAMPLE_INTERVAL_NS));
    let line = format!("{cg}");
    std::fs::remove_dir_all(&base).unwrap();
    assert_eq!(cg.current(), 4096);
    assert!(line.contains(" current 4096 events "), "{line}");
}

#[test]
fn sampling_is_rate_limited_to_ten_milliseconds() {
    let cg = Cgroup::discover(true);
    if !cg.tracked() {
        crate::testutil::skip(
            "sampling_is_rate_limited_to_ten_milliseconds",
            &format!("cgroup untracked here (state {})", cg.state() as u8),
        );
        return;
    }
    // Call `sample` with the coarse clock as fast as possible for at
    // least 100 ms and until three reads. Each call must read exactly
    // when `SAMPLE_INTERVAL_NS` has passed since the last read: two
    // reads are never closer, and a due sample is never suppressed.
    // The check is per call, so a thread descheduled for most of the
    // spin makes fewer calls but never a wrong one.
    let start = sys::now_coarse_ns();
    let mut last = cg.sampled_at();
    let mut reads = 0;
    let mut calls = 0u64;
    loop {
        let now = sys::now_coarse_ns();
        if now - start >= 100_000_000 && reads >= 3 {
            break;
        }
        let due = now.saturating_sub(last) >= SAMPLE_INTERVAL_NS;
        let read = cg.sample(now);
        assert_eq!(read, due, "call {calls}: now {now} ns, last read {last} ns");
        if read {
            last = now;
            reads += 1;
        }
        calls += 1;
    }
    assert_eq!(cg.sampled_at(), last);
    eprintln!(
        "cgroup: {reads} reads in {calls} sample calls over {} ms",
        (sys::now_coarse_ns() - start) / 1_000_000
    );
    // Two calls in the same instant: the second never reads.
    let now = sys::now_coarse_ns() + SAMPLE_INTERVAL_NS;
    assert!(cg.sample(now));
    assert!(!cg.sample(now));
    assert_eq!(cg.sampled_at(), now);
    // The interval is 10 ms to the nanosecond, which the coarse clock's
    // ticks cannot resolve: 1 ns short does not read, 10 ms does.
    assert_eq!(SAMPLE_INTERVAL_NS, 10_000_000);
    assert!(!cg.sample(now + SAMPLE_INTERVAL_NS - 1));
    assert!(cg.sample(now + SAMPLE_INTERVAL_NS));
}

#[test]
fn current_tracks_a_large_allocation() {
    let cg = Cgroup::discover(true);
    if !cg.tracked() || !cg.has_current() {
        crate::testutil::skip(
            "current_tracks_a_large_allocation",
            "no cgroup memory.current here",
        );
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
