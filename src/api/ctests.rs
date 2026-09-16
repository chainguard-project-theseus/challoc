//! Drivers for the per-feature C programs in `tests/c/` (plan Phase 0
//! task 12). Each program is compiled with the container's C compiler, run
//! once without the preload to prove it notices when challoc is not
//! serving `malloc`, then run under `LD_PRELOAD=libchalloc.so` and must
//! exit 0 and print `ok <name>`. The programs check their own invariants;
//! this file only reports their verdict.
//!
//! These tests need `target/release/libchalloc.so`; `make test` builds it
//! first. They never touch the in-process allocator, but they live under
//! `api::` so `make test` groups them with the other `LD_PRELOAD` tests.

use crate::QUANTUM;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::env;
use std::process::Command;
use std::string::String;

/// Linux signal numbers (same on x86_64 and aarch64) for the negative tests.
const SIGABRT: i32 = 6;
const SIGSEGV: i32 = 11;

fn release_so() -> String {
    let target =
        env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| std::format!("{}/target", env!("CARGO_MANIFEST_DIR")));
    let so = std::format!("{target}/release/libchalloc.so");
    assert!(
        std::path::Path::new(&so).exists(),
        "{so} missing: run `cargo build --release` first (`make test` does)"
    );
    so
}

/// Compile `<dir>/<name>.c` (with `tests/c` on the include path for
/// `common.h`) and return the path of the binary.
fn compile(dir: &str, name: &str) -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::format!("{root}/{dir}/{name}.c");
    let include = std::format!("-I{root}/tests/c");
    // Tests run concurrently in one process and two may compile the same
    // program with different arguments, so the path needs more than the pid.
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let out = std::format!("{}/challoc-{name}-{}-{seq}", env::temp_dir().display(), std::process::id());
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let quantum = std::format!("-DQUANTUM={QUANTUM}");
    let status = Command::new(&cc)
        .args(["-O2", "-g", "-Wall", "-Wextra", "-Werror", "-pthread", &quantum, &include, &src, "-o", &out, "-ldl"])
        .status()
        .unwrap();
    assert!(status.success(), "compiling {src} failed");
    out
}

/// Build, run bare (must fail), run under the preload (must pass).
fn run_c(name: &str, args: &[&str]) {
    run_c_in("tests/c", name, args);
}

fn run_c_in(dir: &str, name: &str, args: &[&str]) {
    let so = release_so();
    let bin = compile(dir, name);

    let bare = Command::new(&bin).args(args).output().unwrap();
    assert!(
        !bare.status.success(),
        "{name} passed without libchalloc preloaded:\n{}",
        String::from_utf8_lossy(&bare.stdout)
    );

    let run = Command::new(&bin).args(args).env("LD_PRELOAD", &so).output().unwrap();
    let _ = std::fs::remove_file(&bin);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{name} failed ({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    let ok = std::format!("ok {name}");
    assert!(stdout.lines().any(|l| l == ok), "{name} did not print `{ok}`\nstdout: {stdout}\nstderr: {stderr}");
    std::eprintln!("{name}: {}", stdout.trim_end());
}

/// Run one mode of `tests/c/negative.c` (plan Phase 0 task 14). Bare, the
/// program must exit non-zero without a signal (`require_challoc` refuses
/// to run). Under the preload it must die with `signal`, its stdout must
/// show it reached the bad operation and never survived it, and for aborts
/// stderr must carry the `challoc: ` line with every string in `expect`.
fn run_c_negative(mode: &str, signal: i32, expect: &[&str]) {
    use std::os::unix::process::ExitStatusExt;
    let so = release_so();
    let bin = compile("tests/c", "negative");

    let bare = Command::new(&bin).arg(mode).output().unwrap();
    assert!(
        !bare.status.success() && bare.status.signal().is_none(),
        "negative {mode} without libchalloc preloaded: {:?}\n{}",
        bare.status,
        String::from_utf8_lossy(&bare.stderr)
    );

    let run = Command::new(&bin).arg(mode).env("LD_PRELOAD", &so).output().unwrap();
    let _ = std::fs::remove_file(&bin);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let detail = std::format!("({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    assert!(stdout.lines().any(|l| l == std::format!("negative: {mode}")), "{mode} never started {detail}");
    assert!(!stdout.contains("survived"), "{mode} survived {detail}");
    assert_eq!(run.status.signal(), Some(signal), "{mode} wrong signal {detail}");
    if signal == SIGABRT {
        let line = stderr.lines().find(|l| l.starts_with("challoc: "));
        let line = line.unwrap_or_else(|| panic!("{mode} printed no `challoc: ` line {detail}"));
        for e in expect {
            assert!(line.contains(e), "{mode}: `{line}` lacks `{e}` {detail}");
        }
        std::eprintln!("negative {mode}: {line}");
    } else {
        std::eprintln!("negative {mode}: signal {signal}");
    }
}

#[test]
fn c_negative_double_free_stale() {
    // The first free flushed to the slab before the second: the slot bit is
    // clear, so the one-load check on `free` catches it.
    run_c_negative("double_free_stale", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"]);
}

#[test]
fn c_negative_double_free_cached() {
    // Both frees land in the thread cache; the batched release to the slab
    // sees the duplicate (or the second release finds the bit clear).
    run_c_negative("double_free_cached", SIGABRT, &["double free of 0x"]);
}

#[test]
fn c_negative_interior_free() {
    run_c_negative("interior_free", SIGABRT, &["invalid free of 0x", "not an allocation"]);
}

#[test]
fn c_negative_stack_free() {
    run_c_negative("stack_free", SIGABRT, &["invalid free of 0x", "not an allocation"]);
}

#[test]
fn c_negative_huge_overflow() {
    // The byte after a huge run is its guard page (`PROT_NONE`, or
    // `MADV_GUARD_INSTALL` where the kernel has it); either way SIGSEGV.
    run_c_negative("huge_overflow", SIGSEGV, &[]);
}

#[test]
fn c_align() {
    run_c("align", &[]);
}

#[test]
fn c_realloc_tiers() {
    run_c("realloc_tiers", &[]);
}

#[test]
fn c_calloc_zero() {
    run_c("calloc_zero", &[]);
}

#[test]
fn c_random_ops_one_million_across_eight_threads() {
    run_c("random_ops", &["1000000", "8"]);
}

#[test]
fn c_random_ops_single_thread() {
    // Same driver, one thread: no cross-thread traffic, so a failure here
    // is in the per-thread paths alone.
    run_c("random_ops", &["200000", "1", "0x1234"]);
}

#[test]
fn c_producer_consumer() {
    run_c("producer_consumer", &[]);
}

#[test]
fn c_fork_alloc() {
    run_c("fork_alloc", &[]);
}

#[test]
fn c_thread_exit() {
    run_c("thread_exit", &[]);
}

#[test]
fn c_fuzz_short() {
    // The Phase 0 task 13 fuzz driver at a fraction of `make fuzz` (10M ops,
    // 4 threads) so every `make test` runs it. Same seed as `make fuzz`.
    run_c_in("fuzz", "fuzz", &["400000", "4"]);
}

#[test]
fn c_fuzz_single_thread_small_table() {
    // One thread and a 64-entry table: high churn per slot, no cross-thread
    // traffic, so slots are reused quickly through the thread cache.
    run_c_in("fuzz", "fuzz", &["200000", "1", "0x7", "64"]);
}
