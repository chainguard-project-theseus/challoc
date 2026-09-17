//! Drivers for the per-feature C programs in `tests/c/` (plan Phase 0
//! task 12), the fuzz driver in `fuzz/` (task 13), the negative tests
//! (task 14), the glibc malloc tests in `tests/glibc/` (task 15) and
//! mimalloc's stress test in `tests/mimalloc/` (task 16). Each
//! program is compiled with the container's C compiler, run once without
//! the preload to prove it notices when challoc is not serving `malloc`,
//! then run under `LD_PRELOAD=libchalloc.so` and must exit 0 and print
//! `ok <name>`. The programs check their own invariants; this file only
//! reports their verdict.
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
/// `common.h`, plus `flags`) and return the path of the binary.
fn compile(dir: &str, name: &str, flags: &[&str]) -> String {
    compile_link(dir, name, flags, &[])
}

/// [`compile`] with `ldflags` placed after the source file, where `-l`
/// options must go for the linker to resolve the program's references.
fn compile_link(dir: &str, name: &str, flags: &[&str], ldflags: &[&str]) -> String {
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
        .args(["-O2", "-g", "-Wall", "-Wextra", "-Werror", "-pthread", &quantum, &include])
        .args(flags)
        .args([&src, "-o", &out, "-ldl"])
        .args(ldflags)
        .status()
        .unwrap();
    assert!(status.success(), "compiling {src} failed");
    out
}

/// Compile `tests/c/<name>.cpp` with the container's C++ compiler (C++17
/// for `std::align_val_t`) and return the path of the binary.
fn compile_cxx(name: &str) -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::format!("{root}/tests/c/{name}.cpp");
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let out = std::format!("{}/challoc-{name}-{}-{seq}", env::temp_dir().display(), std::process::id());
    let cxx = env::var("CXX").unwrap_or_else(|_| "c++".into());
    let quantum = std::format!("-DQUANTUM={QUANTUM}");
    let status = Command::new(&cxx)
        .args(["-std=c++17", "-O2", "-g", "-Wall", "-Wextra", "-Werror", "-pthread", &quantum])
        .args([&src, "-o", &out, "-ldl"])
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
    run_c_flags(dir, name, args, &[]);
}

fn run_c_flags(dir: &str, name: &str, args: &[&str], flags: &[&str]) {
    let bin = compile(dir, name, flags);
    run_bin(name, &bin, args);
}

/// Run a compiled test program bare (must fail) and under the preload
/// (must pass and print `ok <name>`), then delete it.
fn run_bin(name: &str, bin: &str, args: &[&str]) {
    run_bin_env(name, bin, args, &[]);
}

/// [`run_bin`] with extra environment variables for the preloaded run.
fn run_bin_env(name: &str, bin: &str, args: &[&str], env: &[(&str, &str)]) {
    let so = release_so();

    let bare = Command::new(bin).args(args).output().unwrap();
    assert!(
        !bare.status.success(),
        "{name} passed without libchalloc preloaded:\n{}",
        String::from_utf8_lossy(&bare.stdout)
    );

    let mut cmd = Command::new(bin);
    cmd.args(args).env("LD_PRELOAD", &so);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let run = cmd.output().unwrap();
    let _ = std::fs::remove_file(bin);
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
    let bin = compile("tests/c", "negative", &[]);

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

/// `tests/c/conf.c` (plan Phase 1 task 1): the configuration sources under
/// the real `.so`. `conf` becomes the program's `je_malloc_conf` (statically
/// initialised and exported with `-rdynamic`, as a program linked against
/// the allocator would have it), `env` is added to the environment. With
/// `expect_abort` the first allocation must abort and stderr must carry
/// both the issue text and the `abort_conf` line.
fn run_conf(name: &str, conf: Option<&str>, env: &[(&str, &str)], expect_abort: Option<&str>) {
    run_conf_flags(name, conf, &[], env, expect_abort);
}

/// [`run_conf`] with extra compiler flags (`-DPLAIN` puts `conf` in the
/// program's `malloc_conf` instead of `je_malloc_conf`).
fn run_conf_flags(name: &str, conf: Option<&str>, extra: &[&str], env: &[(&str, &str)], expect_abort: Option<&str>) {
    use std::os::unix::process::ExitStatusExt;
    let so = release_so();
    let define = conf.map(|c| std::format!("-DCONF=\"{c}\""));
    let mut flags = std::vec!["-rdynamic"];
    flags.extend_from_slice(extra);
    if let Some(d) = &define {
        flags.push(d);
    }
    let bin = compile("tests/c", "conf", &flags);
    let mut cmd = Command::new(&bin);
    cmd.env("LD_PRELOAD", &so);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let run = cmd.output().unwrap();
    let _ = std::fs::remove_file(&bin);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let detail = std::format!("({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    match expect_abort {
        None => {
            assert!(run.status.success(), "conf {name} failed {detail}");
            assert!(stdout.lines().any(|l| l == "ok conf"), "conf {name} did not print `ok conf` {detail}");
            assert!(!stderr.contains("challoc:"), "conf {name} complained {detail}");
        }
        Some(text) => {
            assert_eq!(run.status.signal(), Some(SIGABRT), "conf {name} did not abort {detail}");
            assert!(!stdout.contains("ok conf"), "conf {name} survived {detail}");
            assert!(stderr.contains(text), "conf {name}: stderr lacks `{text}` {detail}");
            assert!(stderr.contains("challoc: abort_conf:"), "conf {name}: no abort_conf line {detail}");
        }
    }
    std::eprintln!("conf {name}: {}", stderr.trim_end());
}

#[test]
fn c_conf_plain_symbol_interposes_the_library_definition() {
    // The program defines `malloc_conf` (Ruby-style, unprefixed); the
    // library defines one too, and the program's must be the one read: a
    // bad key in it aborts under abort_conf.
    run_conf_flags(
        "plain",
        Some("abort_conf:true,no_such_key:1"),
        &["-DPLAIN"],
        &[],
        Some("challoc: malloc_conf: unknown conf key \"no_such_key\""),
    );
    run_conf_flags("plain_ok", Some("tcache:false,abort_conf:true"), &["-DPLAIN"], &[], None);
}

#[test]
fn c_conf_symbol() {
    // Redis's own string plus abort_conf: parsed from the weak symbol
    // without complaint.
    run_conf(
        "symbol",
        Some("abort_conf:true,lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000,junk:false"),
        &[],
        None,
    );
}

#[test]
fn c_conf_env() {
    run_conf(
        "env",
        None,
        &[("MALLOC_CONF", "abort_conf:true,tcache_nslots_small_max:64,lg_tcache_nslots_mul:3")],
        None,
    );
}

#[test]
fn c_conf_no_tcache() {
    run_conf("notcache", None, &[("MALLOC_CONF", "abort_conf:true,tcache:false")], None);
}

#[test]
fn c_conf_bad_symbol_aborts() {
    run_conf(
        "badsym",
        Some("abort_conf:true,no_such_key:1"),
        &[],
        Some("challoc: je_malloc_conf: unknown conf key \"no_such_key\""),
    );
}

#[test]
fn c_conf_bad_env_aborts_even_when_abort_conf_comes_later() {
    // The bad key is in CHALLOC_CONF, the highest-priority source, and
    // abort_conf arrives from the lower-priority symbol: it still applies.
    run_conf(
        "badenv",
        Some("abort_conf:true"),
        &[("CHALLOC_CONF", "tcache_nslots_small_max:1x")],
        Some("challoc: CHALLOC_CONF: invalid conf value \"1x\" for tcache_nslots_small_max"),
    );
}

#[test]
fn c_conf_env_overrides_the_symbol() {
    // The symbol asks for abort_conf, the environment turns it back off,
    // so the unknown key in the symbol only warns.
    let so = release_so();
    let bin = compile("tests/c", "conf", &["-rdynamic", "-DCONF=\"abort_conf:true,no_such_key:1\""]);
    let run = Command::new(&bin)
        .env("LD_PRELOAD", &so)
        .env("MALLOC_CONF", "abort_conf:false,narenas:-1")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&bin);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    assert!(stdout.lines().any(|l| l == "ok conf"), "stdout: {stdout}");
    assert!(stderr.contains("challoc: je_malloc_conf: unknown conf key \"no_such_key\""), "stderr: {stderr}");
    assert!(stderr.contains("challoc: MALLOC_CONF: out-of-range conf value \"-1\" for narenas"), "stderr: {stderr}");
    std::eprintln!("conf override: {}", stderr.trim_end());
}

/// One mode of `tests/c/decay.c` (plan Phase 1 task 2) with `MALLOC_CONF`.
fn run_decay(mode: &str, conf: &str) {
    let bin = compile("tests/c", "decay", &[]);
    run_bin_env("decay", &bin, &[mode], &[("MALLOC_CONF", conf)]);
}

#[test]
fn c_decay_trim() {
    run_decay("trim", "abort_conf:true");
}

#[test]
fn c_decay_window() {
    run_decay("decay", "abort_conf:true,dirty_decay_ms:200");
}

#[test]
fn c_decay_never() {
    run_decay("never", "abort_conf:true,dirty_decay_ms:-1");
}

#[test]
fn c_decay_background_thread() {
    run_decay("background", "abort_conf:true,background_thread:true,dirty_decay_ms:200");
}

#[test]
fn c_decay_lazy_free() {
    run_decay("lazy", "abort_conf:true,lazy_free:true");
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

/// A signal handler that forks, delivered while this thread is inside the
/// fork handlers with every allocator lock held (the race in glibc's
/// `tst-mallocfork`, made deterministic).
#[test]
fn c_fork_signal() {
    run_c("fork_signal", &[]);
}

#[test]
fn c_thread_exit() {
    run_c("thread_exit", &[]);
}

/// The C++ operators (`src/api/cxx.rs`): resolution into libchalloc,
/// round trips, `std::bad_alloc` thrown through challoc's frames and the
/// `new_handler` loop.
/// `tests/c/jemalloc_api.c` (plan Phase 1 task 5): the jemalloc and glibc
/// extras under the real `.so`, every non-standard symbol reached through
/// a weak reference.
#[test]
fn c_jemalloc_api() {
    run_c("jemalloc_api", &[]);
}

/// The release `.so` exports the whole design 13.1/13.2 symbol list: the
/// `*allocx` family, `mallctl*`, the reports, the glibc extras and every
/// `je_` alias as global functions; `malloc_message`/`je_malloc_message`
/// and `malloc_conf` as global objects a program can preempt, and
/// `je_malloc_conf` as a weak undefined reference.
#[test]
fn jemalloc_symbols_in_release_so() {
    let so = release_so();
    let dynsym = Command::new("readelf").args(["--dyn-syms", "-W", &so]).output().unwrap();
    assert!(dynsym.status.success(), "readelf --dyn-syms failed");
    let dynsym = String::from_utf8_lossy(&dynsym.stdout);
    let line = |sym: &str| dynsym.lines().find(|l| l.ends_with(&std::format!(" {sym}")) && !l.contains(" UND "));
    for sym in [
        "mallocx", "rallocx", "xallocx", "sallocx", "dallocx", "sdallocx", "nallocx",
        "mallctl", "mallctlnametomib", "mallctlbymib", "malloc_stats_print",
        "malloc_info", "malloc_stats", "malloc_trim", "mallopt", "mallinfo", "mallinfo2",
        "je_malloc", "je_free", "je_calloc", "je_realloc", "je_aligned_alloc", "je_memalign",
        "je_posix_memalign", "je_valloc", "je_malloc_usable_size",
        "je_mallocx", "je_rallocx", "je_xallocx", "je_sallocx", "je_dallocx", "je_sdallocx", "je_nallocx",
        "je_mallctl", "je_mallctlnametomib", "je_mallctlbymib", "je_malloc_stats_print",
    ] {
        let l = line(sym).unwrap_or_else(|| panic!("{sym} is not defined in {so}:\n{dynsym}"));
        assert!(l.contains(" FUNC ") && l.contains(" GLOBAL "), "{sym} must be a global function: {l}");
    }
    for sym in ["malloc_message", "je_malloc_message", "malloc_conf"] {
        let l = line(sym).unwrap_or_else(|| panic!("{sym} is not defined in {so}:\n{dynsym}"));
        assert!(l.contains(" OBJECT ") && l.contains(" GLOBAL "), "{sym} must be a global object: {l}");
    }
    // `je_malloc_conf` is read from the program, never defined here: a weak
    // undefined reference resolved through the GOT.
    for sym in ["je_malloc_conf"] {
        let l = dynsym.lines().find(|l| l.ends_with(&std::format!(" {sym}")));
        let l = l.unwrap_or_else(|| panic!("{sym} not referenced by {so}:\n{dynsym}"));
        assert!(l.contains(" WEAK ") && l.contains(" UND "), "{sym} must be a weak undefined reference: {l}");
    }
}

#[test]
fn cxx_new() {
    let bin = compile_cxx("cxx_new");
    run_bin("cxx_new", &bin, &[]);
}

/// The release `.so` defines all 20 operator symbols, references
/// libstdc++'s two helpers only weakly (so a C process without libstdc++
/// still loads it under `BIND_NOW`), and has `.eh_frame` coverage for the
/// operator that `std::bad_alloc` unwinds through.
#[test]
fn cxx_symbols_in_release_so() {
    let so = release_so();
    let dynsym = Command::new("readelf").args(["--dyn-syms", "-W", &so]).output().unwrap();
    assert!(dynsym.status.success(), "readelf --dyn-syms failed");
    let dynsym = String::from_utf8_lossy(&dynsym.stdout);
    let defined = |sym: &str| {
        dynsym.lines().any(|l| l.contains(" FUNC ") && l.contains(" GLOBAL ") && !l.contains(" UND ") && l.ends_with(&std::format!(" {sym}")))
    };
    for sym in [
        "_Znwm", "_Znam", "_ZnwmRKSt9nothrow_t", "_ZnamRKSt9nothrow_t",
        "_ZnwmSt11align_val_t", "_ZnamSt11align_val_t",
        "_ZnwmSt11align_val_tRKSt9nothrow_t", "_ZnamSt11align_val_tRKSt9nothrow_t",
        "_ZdlPv", "_ZdaPv", "_ZdlPvRKSt9nothrow_t", "_ZdaPvRKSt9nothrow_t",
        "_ZdlPvm", "_ZdaPvm", "_ZdlPvSt11align_val_t", "_ZdaPvSt11align_val_t",
        "_ZdlPvSt11align_val_tRKSt9nothrow_t", "_ZdaPvSt11align_val_tRKSt9nothrow_t",
        "_ZdlPvmSt11align_val_t", "_ZdaPvmSt11align_val_t",
    ] {
        assert!(defined(sym), "{sym} is not a defined global function in {so}:\n{dynsym}");
    }
    for sym in ["_ZSt15get_new_handlerv", "_ZSt17__throw_bad_allocv"] {
        let line = dynsym.lines().find(|l| l.ends_with(&std::format!(" {sym}")));
        let line = line.unwrap_or_else(|| panic!("{sym} not referenced by {so}"));
        assert!(line.contains(" WEAK ") && line.contains(" UND "), "{sym} must be a weak undefined reference: {line}");
    }
    assert!(!dynsym.contains("libstdc++"), "no DT_NEEDED on libstdc++ expected");
    let needed = Command::new("readelf").args(["-d", &so]).output().unwrap();
    let needed = String::from_utf8_lossy(&needed.stdout);
    assert!(!needed.contains("libstdc++"), "libchalloc.so must not need libstdc++:\n{needed}");

    // `.eh_frame` must cover `_Znwm` so the unwinder can pass through it.
    let nm = Command::new("nm").args(["-D", "--defined-only", &so]).output().unwrap();
    let nm = String::from_utf8_lossy(&nm.stdout);
    let addr = nm.lines().find(|l| l.ends_with(" _Znwm")).and_then(|l| l.split(' ').next()).expect("_Znwm in nm");
    let addr = usize::from_str_radix(addr, 16).unwrap();
    let frames = Command::new("readelf").args(["--debug-dump=frames", &so]).output().unwrap();
    let frames = String::from_utf8_lossy(&frames.stdout);
    let covered = frames.lines().filter(|l| l.contains("FDE")).any(|l| {
        let Some(pc) = l.split("pc=").nth(1) else { return false };
        let mut r = pc.split("..");
        let lo = usize::from_str_radix(r.next().unwrap_or("").trim(), 16).unwrap_or(usize::MAX);
        let hi = usize::from_str_radix(r.next().unwrap_or("").trim(), 16).unwrap_or(0);
        lo <= addr && addr < hi
    });
    assert!(covered, "no FDE covers _Znwm at {addr:#x}");
}

/// Run one unmodified glibc malloc test from `tests/glibc/malloc/` (plan
/// Phase 0 task 15). `tests/glibc/test-skeleton.c` supplies `main` and the
/// `ok <name>` line; `tests/glibc/include/` shims glibc's internal headers.
fn run_glibc(name: &str) {
    let root = env!("CARGO_MANIFEST_DIR");
    let include = std::format!("-I{root}/tests/glibc/include");
    let test_name = std::format!("-DTEST_NAME=\"{name}\"");
    // glibc builds its tests with `-Wall -Werror` but not `-Wextra`; the
    // sources are unmodified, so drop the one `-Wextra` warning they trip.
    let flags = ["-D_GNU_SOURCE", "-Wno-unused-parameter", &include, &test_name];
    run_c_flags("tests/glibc/malloc", name, &[], &flags);
}

#[test]
fn glibc_tst_malloc() {
    run_glibc("tst-malloc");
}

#[test]
fn glibc_tst_calloc() {
    run_glibc("tst-calloc");
}

#[test]
fn glibc_tst_mallocfork() {
    run_glibc("tst-mallocfork");
}

#[test]
fn glibc_tst_posix_memalign() {
    run_glibc("tst-posix_memalign");
}

#[test]
fn glibc_tst_realloc() {
    run_glibc("tst-realloc");
}

#[test]
fn mimalloc_test_stress() {
    // mimalloc's test/test-stress.c with USE_STD_MALLOC at THREADS=8
    // SCALE=50 ITER=10 (plan Phase 0 task 16). `tests/mimalloc/stress.c`
    // wraps the unmodified source; the include shims stand in for
    // mimalloc's headers. `-Wno-unused-parameter`: run_os_threads()
    // ignores its subproc argument on POSIX.
    let root = env!("CARGO_MANIFEST_DIR");
    let include = std::format!("-I{root}/tests/mimalloc/include");
    run_c_flags("tests/mimalloc", "stress", &["8", "50", "10"], &["-Wno-unused-parameter", &include]);
}

/// `tests/c/huge_hole.c`: the alignment holes the huge allocator leaves
/// below an aligned run are committed memory. Once in the probed guard mode
/// (guard-install on kernels with `MADV_GUARD_INSTALL`, where the Phase 0
/// regression lived) and once forced onto the `PROT_NONE` fallback.
#[test]
fn c_huge_hole() {
    run_c("huge_hole", &[]);
}

#[test]
fn c_huge_hole_no_guards() {
    let bin = compile("tests/c", "huge_hole", &[]);
    run_bin_env("huge_hole", &bin, &[], &[("MALLOC_CONF", "abort_conf:true,guards:false")]);
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

/// Phase 1 task 6: the `jemalloc-soname` build staged by
/// `scripts/jemalloc-compat.sh` (`make jemalloc-compat`; `make test` runs it
/// after the release build). The staged `libjemalloc.so.2` carries that
/// soname and exactly the export list of `libchalloc.so`, which itself has
/// no soname; the `libjemalloc.so` link name and `jemalloc.pc` resolve
/// through `pkg-config`; and `tests/c/jemalloc_link.c`, linked with nothing
/// but `$(pkg-config --cflags --libs jemalloc)`, records `libjemalloc.so.2`
/// in `DT_NEEDED`, fails without the staging directory on the library path,
/// and with it has every malloc entry point served from that file.
#[test]
fn jemalloc_compat_stage() {
    let target =
        env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| std::format!("{}/target", env!("CARGO_MANIFEST_DIR")));
    let lib = std::format!("{target}/jemalloc-compat/stage/lib");
    let so2 = std::format!("{lib}/libjemalloc.so.2");
    assert!(
        std::path::Path::new(&so2).exists(),
        "{so2} missing: run `sh scripts/jemalloc-compat.sh` first (`make test` does)"
    );
    let default_so = release_so();

    let dynamic = |path: &str| {
        let out = Command::new("readelf").args(["-d", path]).output().unwrap();
        assert!(out.status.success(), "readelf -d {path} failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let compat_dyn = dynamic(&so2);
    assert!(compat_dyn.contains("Library soname: [libjemalloc.so.2]"), "{so2} lacks the soname:\n{compat_dyn}");
    let default_dyn = dynamic(&default_so);
    assert!(!default_dyn.contains("(SONAME)"), "the default build must not carry a soname:\n{default_dyn}");

    // The feature changes the soname and nothing else that is visible.
    let exports = |path: &str| -> std::collections::BTreeSet<String> {
        let out = Command::new("readelf").args(["--dyn-syms", "-W", path]).output().unwrap();
        assert!(out.status.success(), "readelf --dyn-syms {path} failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(" GLOBAL ") && !l.contains(" UND "))
            .filter_map(|l| l.split_whitespace().last().map(String::from))
            .collect()
    };
    let (compat, default) = (exports(&so2), exports(&default_so));
    assert!(
        compat == default,
        "export lists differ\nonly in libjemalloc.so.2: {:?}\nonly in libchalloc.so: {:?}",
        compat.difference(&default).collect::<std::vec::Vec<_>>(),
        default.difference(&compat).collect::<std::vec::Vec<_>>()
    );
    for sym in ["malloc", "free", "mallctl", "mallocx", "je_malloc", "je_mallctl"] {
        assert!(compat.contains(sym), "{sym} not exported by {so2}");
    }

    // Link name and pkg-config file.
    let link = std::fs::read_link(std::format!("{lib}/libjemalloc.so")).expect("libjemalloc.so symlink");
    assert_eq!(link, std::path::Path::new("libjemalloc.so.2"));
    let pc_dir = std::format!("{lib}/pkgconfig");
    let pkg_config = |args: &[&str]| {
        let out = Command::new("pkg-config").env("PKG_CONFIG_PATH", &pc_dir).args(args).arg("jemalloc").output().unwrap();
        assert!(
            out.status.success(),
            "pkg-config {args:?} jemalloc failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from(String::from_utf8_lossy(&out.stdout).trim())
    };
    let version = super::mallctl::VERSION_STR.split('-').next().unwrap();
    assert_eq!(pkg_config(&["--modversion"]), version);
    pkg_config(&["--exists", "--atleast-version=5.3"]);
    let libs = pkg_config(&["--libs"]);
    let cflags = pkg_config(&["--cflags"]);
    let libs: std::vec::Vec<&str> = libs.split_whitespace().collect();
    let cflags: std::vec::Vec<&str> = cflags.split_whitespace().collect();
    assert!(libs.contains(&std::format!("-L{lib}").as_str()) && libs.contains(&"-ljemalloc"), "pkg-config --libs: {libs:?}");

    // A link-only consumer: DT_NEEDED names the soname, not the file it was
    // linked against; the loader needs the staging directory to find it.
    let bin = compile_link("tests/c", "jemalloc_link", &cflags, &libs);
    let needed = dynamic(&bin);
    assert!(needed.contains("Shared library: [libjemalloc.so.2]"), "jemalloc_link DT_NEEDED:\n{needed}");
    assert!(!needed.contains("libchalloc"), "jemalloc_link must not need libchalloc:\n{needed}");

    let bare = Command::new(&bin).arg(&lib).output().unwrap();
    assert!(
        !bare.status.success(),
        "jemalloc_link passed without the staged library on the path:\n{}",
        String::from_utf8_lossy(&bare.stdout)
    );
    let run = Command::new(&bin).arg(&lib).env("LD_LIBRARY_PATH", &lib).output().unwrap();
    let _ = std::fs::remove_file(&bin);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "jemalloc_link failed ({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    assert!(stdout.lines().any(|l| l == "ok jemalloc_link"), "stdout: {stdout}\nstderr: {stderr}");
    std::eprintln!("jemalloc_link: {}", stdout.trim_end());
}
