//! Drivers for the C test programs: the per-feature programs in `tests/c/`
//! (plan Phase 0 task 12 and everything since), the fuzz driver in `fuzz/`
//! (task 13), the negative tests (task 14), the glibc malloc tests in
//! `tests/glibc/` (task 15) and mimalloc's stress test in `tests/mimalloc/`
//! (task 16). The programs check their own invariants; this file only
//! reports their verdict.
//!
//! One table, one driver. Every test is a row of [`CTest`] (or a few rows
//! under one name): the program, its arguments, `MALLOC_CONF` and other
//! environment for the preloaded run, compiler flags, what to expect
//! ([`Expect::Ok`]: exit 0 and an `ok <prog>` line; [`Expect::Abort`]: a
//! signal and the `challoc: ` line on stderr), strings the stdout must and
//! must not carry, and a [`Gate`] naming the host feature the row needs.
//! [`run_row`] compiles the program once per distinct (dir, program, flags)
//! through `scripts/ctest-cc.sh`, the one compiler line every driver
//! shares (a process-wide cache: negative.c used to be compiled once per
//! mode), runs it bare (it must refuse to run without the preload), then
//! under `LD_PRELOAD=libchalloc.so`, and checks the expectation.
//! [`ctests!`] makes one `#[test]` per table entry so `cargo test` still
//! names every test.
//!
//! Skips. A row whose gate is closed on this host, and a program that
//! prints a `SKIP <name> <reason>` line (tests/c/common.h `skip_note`),
//! is reported as one `SKIP <test> <reason>` line on stderr and fails the
//! test unless `CHALLOC_TEST_ALLOW_SKIP` is set (docs/testing.md,
//! "Skips"); the Makefile sets it for local runs.
//!
//! These tests need `target/release/libchalloc.so`; `make test` builds it
//! first. They never touch the in-process allocator, but they live under
//! `api::` so `make test` groups them with the other `LD_PRELOAD` tests.

use std::collections::HashMap;
use std::env;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus};
use std::string::String;
use std::sync::{Arc, Mutex, OnceLock};
use std::vec::Vec;

/// Linux signal numbers (same on x86_64 and aarch64) for the negative tests.
const SIGABRT: i32 = 6;
const SIGSEGV: i32 = 11;

fn root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

fn target_dir() -> String {
    env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| std::format!("{}/target", root()))
}

/// The release `.so` the C tests load (`CARGO_TARGET_DIR` or the tree's
/// `target/`); shared with the symbol-table tests of the sibling modules.
pub(crate) fn release_so() -> String {
    let so = std::format!("{}/release/libchalloc.so", target_dir());
    assert!(
        std::path::Path::new(&so).exists(),
        "{so} missing: run `cargo build --release` first (`make test` does)"
    );
    so
}

// ---------------------------------------------------------------------------
// Skips
// ---------------------------------------------------------------------------

/// Whether a skip is a pass with a note (`CHALLOC_TEST_ALLOW_SKIP` set) or
/// a failure (unset: the strict mode of a host that must have every
/// feature, the reference VMs).
fn skips_allowed() -> bool {
    env::var_os("CHALLOC_TEST_ALLOW_SKIP").is_some()
}

/// Print the one structured skip line and, in the strict mode, fail.
fn skip_line(test: &str, reason: &str) {
    std::eprintln!("SKIP {test} {reason}");
    assert!(
        skips_allowed(),
        "SKIP {test} {reason}\n(set CHALLOC_TEST_ALLOW_SKIP=1 to let a test pass with a note on a host without the feature)"
    );
}

/// `skip!(test, "reason {}", arg)`: report the skip as `SKIP <test>
/// <reason>`, fail unless skips are allowed, and return from the test.
macro_rules! skip {
    ($test:expr, $($arg:tt)*) => {{
        skip_line($test, &std::format!($($arg)*));
        return;
    }};
}

/// The `SKIP <name> <reason>` lines a program printed: each becomes a
/// `SKIP <test> <name> <reason>` line here and, in the strict mode, a
/// failure.
fn report_program_skips(test: &str, stdout: &str) {
    let skips: Vec<&str> = stdout.lines().filter(|l| l.starts_with("SKIP ")).collect();
    for s in &skips {
        std::eprintln!("SKIP {test} {}", &s[5..]);
    }
    assert!(
        skips.is_empty() || skips_allowed(),
        "{test}: the program skipped a claim on this host:\n{}\n(set CHALLOC_TEST_ALLOW_SKIP=1 to accept)",
        skips.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// What the preloaded run must do.
#[derive(Clone, Copy)]
enum Expect<'a> {
    /// Exit 0 and print a line `ok <prog>`.
    Ok,
    /// Die of `signal` after printing a `<prog>: ...` line (the program
    /// started); for `SIGABRT` stderr must carry `challoc: ` lines and every
    /// string in `stderr_contains` must appear in one of them. The program
    /// must not have finished (no `survived`, no `ok <prog>`).
    Abort { signal: i32, stderr_contains: &'a [&'a str] },
}

/// The host feature a row needs; a closed gate is a skip.
#[derive(Clone, Copy)]
enum Gate {
    Always,
    /// The kernel installs guard regions (`MADV_GUARD_INSTALL`, 6.13+), so
    /// guard slabs exist.
    GuardInstall,
}

/// One run of one C program.
#[derive(Clone, Copy)]
struct CTest<'a> {
    /// Directory of the source, relative to the crate root.
    dir: &'a str,
    /// Program name: `<dir>/<prog>.c` (`.cpp` with `cxx`), and the name in
    /// its `ok <prog>` line.
    prog: &'a str,
    /// Compile with the C++ compiler (`-std=c++17`).
    cxx: bool,
    /// Extra compiler flags, before the source (`-D`, `-I`, `-Wno-`).
    cflags: &'a [&'a str],
    /// Extra linker flags, after the source (`-L`, `-l`).
    ldflags: &'a [&'a str],
    /// Program arguments (the same for the bare and the preloaded run).
    args: &'a [&'a str],
    /// `MALLOC_CONF` for the preloaded run.
    conf: Option<&'a str>,
    /// Other environment for the preloaded run.
    env: &'a [(&'a str, &'a str)],
    expect: Expect<'a>,
    /// Strings the preloaded run's stdout must contain.
    stdout_has: &'a [&'a str],
    /// Strings the preloaded run's stdout must not contain.
    stdout_lacks: &'a [&'a str],
    /// Strings the preloaded run's stderr must contain.
    stderr_has: &'a [&'a str],
    /// Strings the preloaded run's stderr must not contain.
    stderr_lacks: &'a [&'a str],
    gate: Gate,
}

/// The defaults a row overrides with `..BASE`.
const BASE: CTest<'static> = CTest {
    dir: "tests/c",
    prog: "",
    cxx: false,
    cflags: &[],
    ldflags: &[],
    args: &[],
    conf: None,
    env: &[],
    expect: Expect::Ok,
    stdout_has: &[],
    stdout_lacks: &[],
    stderr_has: &[],
    stderr_lacks: &[],
    gate: Gate::Always,
};

/// A mode of `tests/c/negative.c` that must die of `signal` with the
/// strings in its `challoc: ` line (the driver checks the program printed
/// `negative: <mode>` first). Further fields follow by name.
macro_rules! negative {
    ($mode:literal, $signal:expr, $stderr:expr $(, $field:ident : $value:expr)* $(,)?) => {
        CTest {
            prog: "negative",
            args: &[$mode],
            expect: Expect::Abort { signal: $signal, stderr_contains: $stderr },
            $($field: $value,)*
            ..BASE
        }
    };
}

/// `mimalloc/test-stress.c` through `tests/mimalloc/stress.c` at THREADS=8
/// SCALE=50 ITER=10 (plan Phase 0 task 16); the include shims stand in for
/// mimalloc's headers. `-Wno-unused-parameter`: `run_os_threads()` ignores
/// its subproc argument on POSIX.
const MIMALLOC_STRESS: CTest<'static> = CTest {
    dir: "tests/mimalloc",
    prog: "stress",
    cflags: &["-Wno-unused-parameter", "-Itests/mimalloc/include"],
    args: &["8", "50", "10"],
    ..BASE
};

/// One unmodified glibc malloc test from `tests/glibc/malloc/` (plan Phase
/// 0 task 15). `tests/glibc/test-skeleton.c` supplies `main` and the `ok
/// <name>` line; `tests/glibc/include/` shims glibc's internal headers.
/// glibc builds its tests with `-Wall -Werror` but not `-Wextra`; the
/// sources are unmodified, so drop the one `-Wextra` warning they trip.
macro_rules! glibc {
    ($name:literal) => {
        CTest {
            dir: "tests/glibc/malloc",
            prog: $name,
            cflags: &["-D_GNU_SOURCE", "-Wno-unused-parameter", "-Itests/glibc/include", concat!("-DTEST_NAME=\"", $name, "\"")],
            ..BASE
        }
    };
}

/// One `#[test]` per entry, named as the entry; the entry's rows run in
/// order. Doc comments on an entry are kept on the test.
macro_rules! ctests {
    ($( $(#[$m:meta])* $name:ident => [ $($row:expr),+ $(,)? ] ;)+) => {
        $(
            $(#[$m])*
            #[test]
            fn $name() {
                $( run_row(stringify!($name), &$row); )+
            }
        )+
    };
}

// ---------------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------------

/// What makes two compilations the same binary.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    dir: String,
    prog: String,
    cxx: bool,
    cflags: Vec<String>,
    ldflags: Vec<String>,
}

/// The binary for `row`, compiled once per process for each distinct
/// [`Key`] (tests run concurrently: the map lock is held only to find the
/// cell, the cell's `OnceLock` serialises the one compile).
fn compiled(row: &CTest<'_>) -> String {
    static CACHE: OnceLock<Mutex<HashMap<Key, Arc<OnceLock<String>>>>> = OnceLock::new();
    let key = Key {
        dir: row.dir.into(),
        prog: row.prog.into(),
        cxx: row.cxx,
        cflags: row.cflags.iter().map(|s| String::from(*s)).collect(),
        ldflags: row.ldflags.iter().map(|s| String::from(*s)).collect(),
    };
    let cell = {
        let mut map = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
        map.entry(key.clone()).or_insert_with(|| Arc::new(OnceLock::new())).clone()
    };
    cell.get_or_init(|| compile_now(&key)).clone()
}

/// Compile `<dir>/<prog>.c` (or `.cpp`) through `scripts/ctest-cc.sh` into
/// `target/ctests/<prog>-<hash>`: the script holds the flag line and reads
/// QUANTUM from the built library. Relative `-I` flags are made absolute
/// (the table writes them from the crate root).
fn compile_now(key: &Key) -> String {
    use std::hash::{Hash, Hasher};
    let root = root();
    let src = std::format!("{root}/{}/{}.{}", key.dir, key.prog, if key.cxx { "cpp" } else { "c" });
    let dir = std::format!("{}/ctests", target_dir());
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    let out = std::format!("{dir}/{}-{:016x}", key.prog, h.finish());
    let tmp = std::format!("{out}.tmp-{}", std::process::id());
    let cflags: Vec<String> = key
        .cflags
        .iter()
        .map(|f| match f.strip_prefix("-I") {
            Some(p) if !p.starts_with('/') => std::format!("-I{root}/{p}"),
            _ => f.clone(),
        })
        .collect();
    let mut cmd = Command::new("sh");
    cmd.arg(std::format!("{root}/scripts/ctest-cc.sh")).env("CHALLOC_LIB", release_so());
    if key.cxx {
        cmd.arg("--cxx");
    }
    let status = cmd.args(&cflags).args([&src, "-o", &tmp]).args(&key.ldflags).status().unwrap();
    assert!(status.success(), "compiling {src} failed");
    std::fs::rename(&tmp, &out).unwrap();
    out
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Run one row: the gate, the compile, the bare run (must exit non-zero
/// without a signal: `require_challoc` refuses to run on glibc), the
/// preloaded run, the expectation, the stdout/stderr strings, and the
/// program's skip lines.
fn run_row(test: &str, row: &CTest<'_>) {
    let prog = row.prog;
    let args = row.args;
    match row.gate {
        Gate::Always => {}
        Gate::GuardInstall => {
            // Guard slabs exist only where the kernel installs guard regions
            // (`Shared::init` zeroes the interval otherwise); the Rust test
            // `heap::tests::sized_slot_rejects_guard_slabs` forces them on.
            if !crate::sys::features::Features::probe().guard_install {
                skip!(test, "guard_install=no on this kernel; {prog} {} not run", args.join(" "));
            }
        }
    }
    let so = release_so();
    let bin = compiled(row);

    let bare = Command::new(&bin).args(args).env_remove("LD_PRELOAD").output().unwrap();
    assert!(
        !bare.status.success() && bare.status.signal().is_none(),
        "{test}: {prog} {args:?} without libchalloc preloaded: {:?}\nstdout: {}\nstderr: {}",
        bare.status,
        String::from_utf8_lossy(&bare.stdout),
        String::from_utf8_lossy(&bare.stderr)
    );

    let mut cmd = Command::new(&bin);
    // A suite run under `CHALLOC_CONF` (lane D's campaign, 2026-09-20)
    // would override the test's own `MALLOC_CONF` (the environment key is
    // read after it and wins) and turn every both-ways test one way.
    cmd.args(args).env_remove("CHALLOC_CONF").env("LD_PRELOAD", &so);
    if let Some(conf) = row.conf {
        cmd.env("MALLOC_CONF", conf);
    }
    for (k, v) in row.env {
        cmd.env(k, v);
    }
    let run = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let what = std::format!("{test}: {prog} {}", args.join(" "));
    let detail = std::format!("({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    match row.expect {
        Expect::Ok => {
            assert!(run.status.success(), "{what} failed {detail}");
            let ok = std::format!("ok {prog}");
            assert!(stdout.lines().any(|l| l == ok), "{what} did not print `{ok}` {detail}");
        }
        Expect::Abort { signal, stderr_contains } => {
            run_c_negative(&what, prog, args, signal, stderr_contains, run.status, &stdout, &stderr, &detail)
        }
    }
    for s in row.stdout_has {
        assert!(stdout.contains(s), "{what}: stdout lacks `{s}` {detail}");
    }
    for s in row.stdout_lacks {
        assert!(!stdout.contains(s), "{what}: stdout has `{s}` {detail}");
    }
    for s in row.stderr_has {
        assert!(stderr.contains(s), "{what}: stderr lacks `{s}` {detail}");
    }
    for s in row.stderr_lacks {
        assert!(!stderr.contains(s), "{what}: stderr has `{s}` {detail}");
    }
    if matches!(row.expect, Expect::Ok) {
        std::eprintln!("{what}: {}", stdout.trim_end());
    }
    report_program_skips(test, &stdout);
}

/// The checks of a row that must abort (plan Phase 0 task 14): the program
/// started, the run died of `signal`, the program never finished (no `survived`, no `ok
/// <prog>`), and for an abort stderr carries `challoc: ` lines with every
/// string in `expect` found in one of them (the allocator's own message,
/// or for the conf tests the issue line and the `abort_conf` line).
#[allow(clippy::too_many_arguments)]
fn run_c_negative(
    what: &str,
    prog: &str,
    args: &[&str],
    signal: i32,
    expect: &[&str],
    status: ExitStatus,
    stdout: &str,
    stderr: &str,
    detail: &str,
) {
    // A mode program reached its bad operation (`negative: <mode>`). A row
    // without arguments aborts before `main` (tests/c/conf.c: the first
    // allocation is libc's, and `abort_conf` fires there), so nothing on
    // stdout is expected of it.
    if let Some(mode) = args.first() {
        let started = std::format!("{prog}: {mode}");
        assert!(stdout.lines().any(|l| l == started), "{what} never started (no `{started}` line) {detail}");
    }
    assert!(!stdout.contains("survived"), "{what} survived {detail}");
    assert!(!stdout.contains(&std::format!("ok {prog}")), "{what} finished {detail}");
    assert_eq!(status.signal(), Some(signal), "{what} wrong signal {detail}");
    if signal == SIGABRT {
        let lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with("challoc: ")).collect();
        assert!(!lines.is_empty(), "{what} printed no `challoc: ` line {detail}");
        for e in expect {
            assert!(lines.iter().any(|l| l.contains(e)), "{what}: no `challoc: ` line has `{e}` {detail}");
        }
        std::eprintln!("{what}: {}", lines.join(" | "));
    } else {
        assert!(expect.is_empty(), "{what}: stderr strings are checked for SIGABRT only");
        std::eprintln!("{what}: signal {signal}");
    }
}

// ---------------------------------------------------------------------------
// Negative tests (plan Phase 0 task 14, Phase 1.5 tasks 9 and 14, review
// 2026-09-18 item 43): one mode of tests/c/negative.c per row.
// ---------------------------------------------------------------------------

ctests! {
    /// The first free flushed to the slab before the second: the slot bit is
    /// clear, so the one-load check on `free` catches it.
    c_negative_double_free_stale => [
        negative!("double_free_stale", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"]),
    ];
    /// Both frees land in the thread cache; the batched release to the slab
    /// sees the duplicate (or the second release finds the bit clear).
    c_negative_double_free_cached => [
        negative!("double_free_cached", SIGABRT, &["double free of 0x"]),
    ];
    /// A sized double free returns from the second call (the sized path loads
    /// no descriptor) and is caught when the cache flushes both copies.
    c_negative_sized_double_free => [
        negative!("sized_double_free", SIGABRT, &["double free of 0x"],
            stdout_has: &["sized double free of 0x"]),
    ];
    c_negative_sized_mismatch => [
        negative!("sized_mismatch", SIGABRT, &["sized free of 0x", "size hint 1000 does not match"]),
    ];
    c_negative_free_sized_mismatch => [
        negative!("free_sized_mismatch", SIGABRT, &["sized free of 0x", "size hint 65 does not match"]),
    ];
    c_negative_interior_free => [
        negative!("interior_free", SIGABRT, &["invalid free of 0x", "not an allocation"]),
    ];
    c_negative_stack_free => [
        negative!("stack_free", SIGABRT, &["invalid free of 0x", "not an allocation"]),
    ];
    /// Plan Phase 1.5 task 14: `malloc_usable_size` reads the located slot's
    /// bit the way `free` does and still aborts on a slot that is not
    /// allocated (the query of a freed, flushed block).
    c_negative_usable_size_freed => [
        negative!("usable_size_freed", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"],
            stdout_lacks: &["returned"]),
    ];
    /// The byte after a huge run is its guard page (`PROT_NONE`, or
    /// `MADV_GUARD_INSTALL` where the kernel has it); either way SIGSEGV.
    c_negative_huge_overflow => [
        negative!("huge_overflow", SIGSEGV, &[]),
    ];
    /// Plan Phase 1.5 task 9c: with `double_free_scan` the second free is
    /// caught at the call, while the first copy is one of the top four cache
    /// entries, on plain `free` and on the sized path (which otherwise defers
    /// every check to the flush).
    /// Plan Phase 2.5 task 7: the same cached double free with
    /// `pac_pointers:true`, so the C-level detection at the flush split runs
    /// over signed entries (the straddle compare is signed against signed;
    /// the release authenticates the batch first). On a host without PAC the
    /// key is accepted and inert.
    c_negative_double_free_cached_with_pac_pointers => [
        negative!("double_free_cached", SIGABRT, &["double free of 0x"],
            conf: Some("pac_pointers:true,abort_conf:true")),
    ];
    c_negative_double_free_cached_with_scan => [
        negative!("double_free_cached", SIGABRT, &["double free of 0x", "already in the thread cache"],
            conf: Some("double_free_scan:true")),
    ];
    c_negative_sized_double_free_with_scan => [
        negative!("sized_double_free", SIGABRT, &["double free of 0x", "already in the thread cache"],
            conf: Some("double_free_scan:true"),
            stdout_lacks: &["returned"]),
    ];
    /// Plan Phase 1.5 task 9a: the two copies of a cached double free sort to
    /// the flush's split point, one released and one kept; the flush's
    /// neighbour pass over the kept half catches it.
    c_negative_double_free_straddle => [
        negative!("double_free_straddle", SIGABRT, &["double free of 0x", "both copies cached, one at the flush boundary"],
            stdout_has: &["pair at sorted indices"]),
    ];
    /// Plan Phase 1.5 task 9b: a sized free of a slot start inside a guard slab
    /// is rejected on the sized fast path (guard slabs are arithmetic on the
    /// table line) and aborts in the exact slow path with the guard named.
    c_negative_sized_guard => [
        negative!("sized_guard", SIGABRT, &["invalid free of 0x", "guard slab"],
            stdout_has: &["guard region at"],
            gate: Gate::GuardInstall),
    ];
    /// Review 2026-09-18 item 43: a plain `free` into a guard gap. The exact
    /// bit test finds the slot clear and the slab a guard, and says so.
    c_negative_guard_free => [
        negative!("guard_free", SIGABRT, &["invalid free of 0x", "guard slab"],
            stdout_has: &["guard region at"],
            gate: Gate::GuardInstall),
    ];
    /// Item 43: `realloc` and `sallocx` on a stale pointer (freed and
    /// flushed) take the exact bit test of `check_live` and abort.
    c_negative_realloc_stale => [
        negative!("realloc_stale", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"],
            stdout_lacks: &["returned"]),
    ];
    c_negative_sallocx_stale => [
        negative!("sallocx_stale", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"],
            stdout_lacks: &["returned"]),
    ];
    /// Item 43: `dallocx(p, MALLOCX_TCACHE_NONE)` twice. The uncached release
    /// checks the bit at the call, so the second is caught there, not at a
    /// flush.
    c_negative_dallocx_none_twice => [
        negative!("dallocx_none_twice", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"],
            stdout_lacks: &["returned"]),
    ];
    /// Item 43: `free_sized` with a huge size hint on a slab pointer. The hint
    /// names no class, so the sized fast path declines and the slow path's
    /// hint check aborts at the call.
    c_negative_free_sized_huge_hint => [
        negative!("free_sized_huge_hint", SIGABRT, &["sized free of 0x", "size hint 1048576 does not match"],
            stdout_lacks: &["returned"]),
    ];
    /// Item 43: a huge run freed twice, and a free of a page inside a huge
    /// run; both are the huge allocator's own aborts.
    c_negative_huge_double_free => [
        negative!("huge_double_free", SIGABRT, &["free of 0x", "huge allocation"],
            stdout_lacks: &["returned"]),
    ];
    c_negative_huge_interior_free => [
        negative!("huge_interior_free", SIGABRT, &["invalid free of 0x", "not the start of a huge allocation"],
            stdout_lacks: &["returned"]),
    ];
    /// Design 13.7: `challoc_free_usable` is the exact `free` reporting the
    /// class it verified, so a bad pointer dies with `free`'s message at the
    /// call: an interior pointer, a stale (flushed) slot, a huge run that is
    /// no longer live; a cached double free is caught at the flush by default
    /// and at the call with `double_free_scan`.
    c_negative_free_usable_interior => [
        negative!("free_usable_interior", SIGABRT, &["invalid free of 0x", "not an allocation"],
            stdout_lacks: &["returned"]),
    ];
    c_negative_free_usable_stale => [
        negative!("free_usable_stale", SIGABRT, &["invalid or double free of 0x", "slot is not allocated"],
            stdout_lacks: &["returned"]),
    ];
    c_negative_free_usable_cached => [
        negative!("free_usable_cached", SIGABRT, &["double free of 0x"],
            stdout_has: &["returned 64"]),
        negative!("free_usable_cached", SIGABRT, &["double free of 0x", "already in the thread cache"],
            conf: Some("double_free_scan:true"), stdout_lacks: &["returned"]),
    ];
    c_negative_free_usable_huge_twice => [
        negative!("free_usable_huge_twice", SIGABRT, &["double free of 0x", "huge allocation already freed"],
            stdout_lacks: &["returned"]),
    ];
}

// ---------------------------------------------------------------------------
// Positive programs
// ---------------------------------------------------------------------------

ctests! {
    /// Plan Phase 2.5 task 7: `tests/c/tls_threads.c` (64 pthreads, distinct
    /// heaps through `thread.allocatedp`, allocations and frees on every
    /// thread) under `pac_pointers:true`: the shipped library's signed cache
    /// entries and signed TLS heap pointers, from C, on a PAC host; the plain
    /// path with the key accepted elsewhere.
    c_tls_threads_with_pac_pointers => [
        CTest { prog: "tls_threads", conf: Some("pac_pointers:true,abort_conf:true"), ..BASE },
    ];
    /// `tests/c/decay.c` (plan Phase 1 task 2), one mode per test with its
    /// `MALLOC_CONF`.
    c_decay_trim => [ CTest { prog: "decay", args: &["trim"], conf: Some("abort_conf:true"), ..BASE } ];
    c_decay_window => [ CTest { prog: "decay", args: &["decay"], conf: Some("abort_conf:true,dirty_decay_ms:200"), ..BASE } ];
    c_decay_curve => [ CTest { prog: "decay", args: &["curve"], conf: Some("abort_conf:true,dirty_decay_ms:2000"), ..BASE } ];
    c_decay_never => [ CTest { prog: "decay", args: &["never"], conf: Some("abort_conf:true,dirty_decay_ms:-1"), ..BASE } ];
    c_decay_background_thread => [
        CTest { prog: "decay", args: &["background"], conf: Some("abort_conf:true,background_thread:true,dirty_decay_ms:200"), ..BASE },
    ];
    /// `tests/c/decay.c demand` (issue #25): recurring 8 MiB slab bursts are
    /// kept between bursts from the third on under the demand window and
    /// released once it passes; `demand-off` is the control, the same
    /// bursts refaulting under the curve alone.
    c_decay_demand => [
        CTest { prog: "decay", args: &["demand"], conf: Some("abort_conf:true,dirty_decay_ms:200,hpa_peak_demand_window_ms:4000"), ..BASE },
        CTest { prog: "decay", args: &["demand-off"], conf: Some("abort_conf:true,dirty_decay_ms:200"), ..BASE },
    ];
    /// `tests/c/decay.c hugedemand`: a 100 MiB run reallocated every
    /// iteration is kept from its second free under the window, a one-off
    /// 192 MiB run is purged at free; without the window every iteration
    /// is purged at free (`huge_retain_max`, today's rule).
    c_decay_huge_demand => [
        CTest { prog: "decay", args: &["hugedemand"], conf: Some("abort_conf:true,hpa_peak_demand_window_ms:4000"), ..BASE },
        CTest { prog: "decay", args: &["hugedemand-off"], conf: Some("abort_conf:true"), ..BASE },
    ];
    /// The `curve` mode with the window off explicitly (today's decay) and
    /// on (one burst never recurs, so the curve alone decides): both pass
    /// the curve's criteria.
    c_decay_demand_off => [
        CTest { prog: "decay", args: &["curve"], conf: Some("abort_conf:true,dirty_decay_ms:2000,hpa_peak_demand_window_ms:0"), ..BASE },
        CTest { prog: "decay", args: &["curve"], conf: Some("abort_conf:true,dirty_decay_ms:2000,hpa_peak_demand_window_ms:4000"), ..BASE },
    ];
    c_align => [ CTest { prog: "align", ..BASE } ];
    c_realloc_tiers => [ CTest { prog: "realloc_tiers", ..BASE } ];
    c_calloc_zero => [ CTest { prog: "calloc_zero", ..BASE } ];
    /// 64 pthreads each see their own heap (`thread.allocatedp` distinct per
    /// thread, per-thread counters moved only by that thread).
    c_tls_threads => [ CTest { prog: "tls_threads", ..BASE } ];
    /// The standard entry points from C (`tests/c/libc_api.c`), then the
    /// same program with the background thread requested by the
    /// configuration and created by a periodic tick that lands in a `free`:
    /// `errno` must survive it (design 8.1).
    c_libc_api => [
        CTest { prog: "libc_api", ..BASE },
        CTest { prog: "libc_api", args: &["background"], conf: Some("abort_conf:true,background_thread:true"), ..BASE },
    ];
    /// challoc's size-returning entry points from C (`tests/c/usable_api.c`,
    /// design 13.7; the header comes from include/): under the
    /// default configuration and the hardening keys that change the push (`zero_on_free` declines the
    /// fast push, `double_free_scan` scans it, `pac_pointers` signs).
    c_usable_api => [
        CTest { prog: "usable_api", cflags: &["-Iinclude"], ..BASE },
        CTest { prog: "usable_api", cflags: &["-Iinclude"],
            conf: Some("abort_conf:true,zero_on_free:true,double_free_scan:true,pac_pointers:true"), ..BASE },
    ];
    c_random_ops_one_million_across_eight_threads => [ CTest { prog: "random_ops", args: &["1000000", "8"], ..BASE } ];
    /// Same driver, one thread: no cross-thread traffic, so a failure here
    /// is in the per-thread paths alone.
    c_random_ops_single_thread => [ CTest { prog: "random_ops", args: &["200000", "1", "0x1234"], ..BASE } ];
    c_producer_consumer => [ CTest { prog: "producer_consumer", ..BASE } ];
    c_fork_alloc => [ CTest { prog: "fork_alloc", ..BASE } ];
    /// `tests/c/fork_alloc.c mallctl` (plan Phase 1.5 task 10a): forks while
    /// other threads hold the statistics and explicit-cache locks; every child
    /// must call `epoch` and `tcache.create` itself and exit 0 within its alarm.
    c_fork_alloc_under_mallctl => [ CTest { prog: "fork_alloc", args: &["mallctl"], ..BASE } ];
    /// A signal handler that forks, delivered while this thread is inside the
    /// fork handlers with every allocator lock held (the race in glibc's
    /// `tst-mallocfork`, made deterministic).
    c_fork_signal => [ CTest { prog: "fork_signal", ..BASE } ];
    c_thread_exit => [ CTest { prog: "thread_exit", ..BASE } ];
    /// `tests/c/jemalloc_api.c` (plan Phase 1 task 5): the jemalloc and glibc
    /// extras under the real `.so`, every non-standard symbol reached through
    /// a weak reference.
    c_jemalloc_api => [ CTest { prog: "jemalloc_api", ..BASE } ];
    /// Review 2026-09-18 item 43, the positive paths (`tests/c/positive.c`):
    /// `free_sized`/`free_aligned_sized` from C over every tier with the
    /// slots coming back, `xallocx` with `extra > 0` (in place, unchanged,
    /// the real size), and `malloc_trim` with nothing purgeable (0, nothing
    /// touched) then with a freed set (1, RSS down) then again (0).
    c_positive_free_sized => [ CTest { prog: "positive", args: &["free_sized"], ..BASE } ];
    c_positive_xallocx_extra => [ CTest { prog: "positive", args: &["xallocx_extra"], ..BASE } ];
    c_positive_malloc_trim_idle => [ CTest { prog: "positive", args: &["malloc_trim_idle"], conf: Some("abort_conf:true"), ..BASE } ];
    /// The C++ operators (`src/api/cxx.rs`): resolution into libchalloc,
    /// round trips, `std::bad_alloc` thrown through challoc's frames and the
    /// `new_handler` loop.
    cxx_new => [ CTest { prog: "cxx_new", cxx: true, ..BASE } ];
    glibc_tst_malloc => [ glibc!("tst-malloc") ];
    glibc_tst_calloc => [ glibc!("tst-calloc") ];
    glibc_tst_mallocfork => [ glibc!("tst-mallocfork") ];
    glibc_tst_posix_memalign => [ glibc!("tst-posix_memalign") ];
    glibc_tst_realloc => [ glibc!("tst-realloc") ];
    mimalloc_test_stress => [ MIMALLOC_STRESS ];
    /// The same stress test with `check_zero` on (plan Phase 1.5 task 7b):
    /// every `Fresh` or `Purged` slot and page behind its `calloc`s is read by
    /// the allocator before it is handed out, so a dirty "zero" allocation
    /// aborts inside the allocator with the slab's prior state and the
    /// purge advice instead of in the C assertion. Stays in `make test` so a
    /// recurrence of the 2026-09-18 failure is diagnosable.
    mimalloc_test_stress_check_zero => [
        CTest { conf: Some("abort_conf:true,check_zero:true"), ..MIMALLOC_STRESS },
    ];
    /// `check_zero` under `slot_randomize`: a shuffled batch from a `Fresh` or
    /// `Purged` slab is still all zero (the shuffle changes the order, not the
    /// set), so the campaign's default configuration stays clean.
    mimalloc_test_stress_check_zero_slot_randomize => [
        CTest { conf: Some("abort_conf:true,check_zero:true,slot_randomize:true"), ..MIMALLOC_STRESS },
    ];
    /// Plan Phase 2 task 3: the opt-in hardening tunables (`tests/c/tunables.c`).
    /// `zero_on_free`: a written block reads as zero after `free`, `sdallocx`
    /// and `free_sized`, and comes back zero from `malloc`; the control mode
    /// proves the default leaves the bytes. `slot_randomize`: one class's
    /// first allocations in a fresh process are a contiguous slot set in a
    /// non-monotone order; the control is ascending. `abort_conf` accepts
    /// both keys (they are no longer inert).
    c_tunables_zero_on_free => [
        CTest { prog: "tunables", args: &["zero"], conf: Some("abort_conf:true,zero_on_free:true"), ..BASE },
        CTest { prog: "tunables", args: &["dirty"], conf: Some("abort_conf:true"), ..BASE },
    ];
    c_tunables_slot_randomize => [
        CTest { prog: "tunables", args: &["random"], conf: Some("abort_conf:true,slot_randomize:true"), ..BASE },
        CTest { prog: "tunables", args: &["ordered"], conf: Some("abort_conf:true"), ..BASE },
    ];
    /// The fuzz driver (`c_fuzz_short`) once under each key, and both.
    c_fuzz_short_under_zero_on_free => [
        CTest { dir: "fuzz", prog: "fuzz", args: &["400000", "4"], conf: Some("abort_conf:true,zero_on_free:true"), ..BASE },
    ];
    c_fuzz_short_under_slot_randomize => [
        CTest { dir: "fuzz", prog: "fuzz", args: &["400000", "4"], conf: Some("abort_conf:true,slot_randomize:true"), ..BASE },
        CTest { dir: "fuzz", prog: "fuzz", args: &["200000", "4"],
            conf: Some("abort_conf:true,slot_randomize:true,zero_on_free:true,double_free_scan:true"), ..BASE },
    ];
    /// `tests/c/mseal.c` (plan Phase 2 task 2): the never-committed ranges of
    /// the reservations are sealed after init where the kernel allows it, and
    /// the allocator keeps working. A `SKIP` line where `mseal` is
    /// unavailable (Docker's default seccomp profile filters it).
    c_mseal => [ CTest { prog: "mseal", ..BASE } ];
    /// `tests/c/features.c` (plan Phase 2.5 task 2): `challoc_features()` is a
    /// NUL-terminated `key=value` line equal to the `Kernel and CPU features:`
    /// line of `malloc_stats_print`, with every CPU key of this architecture.
    c_features => [ CTest { prog: "features", ..BASE } ];
    /// `tests/c/thp.c never` (plan Phase 2.5 task 9 and 16): the default issues
    /// no `MADV_HUGEPAGE` anywhere (no `hg` on the VMA of a class region,
    /// `stats.thp.mode` 0, nothing advised), `MADV_NOHUGEPAGE`s the class and
    /// metadata reservations (`nh`, `stats.thp.nohuge_ranges` 2) and leaves
    /// the huge region to the host's policy (no `nh` on the huge run; on an
    /// `always` host it shows `AnonHugePages` again).
    c_thp_never => [ CTest { prog: "thp", args: &["never"], ..BASE } ];
    /// `tests/c/thp.c always` under `thp:always`: with the PMD size enabled in
    /// sysfs the class region is committed in 2 MiB chunks, the VMA has `hg`
    /// and `THPeligible`, and 8 MiB touched show at least 4 MiB of
    /// `AnonHugePages`; with only smaller sizes the groups are advised; with
    /// nothing enabled the mode is off (a `SKIP` line). Forced to `never`
    /// under a cgroup limit below 1 GiB (a pass with a note).
    c_thp_always => [ CTest { prog: "thp", args: &["always"], conf: Some("thp:always"), ..BASE } ];
    /// `tests/c/thp.c auto` under `thp:auto`: active only where the kernel
    /// enables a folio size below the PMD and does not `madvise`-gate the PMD
    /// (needs `hugepages-64kB/enabled=madvise` and `hugepages-2048kB/enabled=
    /// never` from root; Ubuntu's default makes it inert, which the program
    /// checks as well, with a `SKIP` line).
    c_thp_auto => [ CTest { prog: "thp", args: &["auto"], conf: Some("thp:auto"), ..BASE } ];
    /// `tests/c/cgroup.c plain` (plan Phase 2 task 4): `stats.cgroup.*` read
    /// from C, `ENOENT` without a cgroup2, else `max` or a number that equals
    /// `CHALLOC_EXPECT_CGROUP_LIMIT` when `make test-cgroup-limit`'s
    /// `--memory=512m` run set it (the variable reaches the program through
    /// the inherited environment), and `current` following a 64 MiB
    /// allocation after the 10 ms sampling window.
    c_cgroup_stats => [ CTest { prog: "cgroup", args: &["plain"], ..BASE } ];
    /// `tests/c/cgroup.c off`: `cgroup:false` reads nothing (the keys are
    /// `ENOENT`, the report says `not tracked (cgroup:false)`) and the key is
    /// accepted by `abort_conf`, since it is no longer inert.
    c_cgroup_off => [ CTest { prog: "cgroup", args: &["off"], conf: Some("cgroup:false,abort_conf:true"), ..BASE } ];
    /// `tests/c/cgroup.c bg`: with a background thread the cgroup is still
    /// tracked and sampled: the report prints the tracked `cgroup:` line and
    /// `stats.cgroup.current` follows the kernel's `memory.current`.
    c_cgroup_background_thread => [
        CTest { prog: "cgroup", args: &["bg"], conf: Some("background_thread:true,abort_conf:true"), ..BASE },
    ];
    /// `tests/c/cgroup_policy.c bands` (plan Phase 2 task 5): the design 11
    /// policy through the public keys. Without a cgroup2 the policy keys are
    /// `ENOENT`; tracked without a limit `decay_ms` is the configured period
    /// and `retention_cap` `SIZE_MAX`; under `make test-cgroup-limit`'s
    /// 512 MiB the program fills the cgroup to 80% (scaled `decay_ms`, then
    /// the 5% retention cap after freeing) and to 94% (`decay_ms` 0, one tick
    /// purges everything). Filling to 94% is why that target runs
    /// single-threaded.
    c_cgroup_policy_bands => [ CTest { prog: "cgroup_policy", args: &["bands"], ..BASE } ];
    /// `tests/c/cgroup_policy.c high`: the `memory.events` `high` row, in a
    /// child cgroup the program makes itself when the mount allows it (a
    /// `--privileged` container on a host without `nsdelegate` at the
    /// container's root); each refusal is a pass with a `SKIP` line.
    c_cgroup_policy_high => [ CTest { prog: "cgroup_policy", args: &["high"], ..BASE } ];
    /// `tests/c/cgroup_policy.c demand` (issue #25): under `make
    /// test-cgroup-limit`'s 512 MiB, a 100 MiB run reallocated at 80% of the
    /// limit is purged at every free with the demand window on (the policy
    /// turns the floor off above 70%); without a limit, a `SKIP` line.
    c_cgroup_policy_demand => [
        CTest { prog: "cgroup_policy", args: &["demand"], conf: Some("abort_conf:true,hpa_peak_demand_window_ms:60000"), ..BASE },
    ];
    /// `tests/c/cgroup_churn.c` (plan Phase 2 task 6 (a)): the churn program
    /// builds and runs under the preload at a small size in `report` mode (no
    /// criterion: the row proves the program runs). The criteria (RSS under
    /// 100 MiB within 2 s of freeing 400 MiB with the policy on, not with
    /// `cgroup:false`) need a container of their own per run with swap off
    /// (`--memory=L --memory-swap=L`) and are `make test-cgroup-churn`,
    /// chained into `make test`.
    c_cgroup_churn_program => [ CTest { prog: "cgroup_churn", args: &["report", "64", "16"], ..BASE } ];
    /// `tests/c/cgroup_oscillate.c` (plan Phase 2 task 6 (b)): the oscillation
    /// program builds and completes a few cycles (3 s, 16..48 MiB live). The
    /// 5-minute run in 512 MiB is `make test-cgroup-oscillate` (opt-in).
    c_cgroup_oscillate_program => [ CTest { prog: "cgroup_oscillate", args: &["3", "16", "48", "200"], ..BASE } ];
    /// `tests/c/guard_slab.c fault` (plan Phase 2 task 1): under the preload,
    /// a write into the guard slab that follows a run of slabs of the 64-byte
    /// class faults (SIGSEGV caught in the program), while the last byte of
    /// the last slot before it and the first slot after it are writable. Guard
    /// slabs exist only where the kernel installs guard regions (6.13+), so
    /// the test skips elsewhere; the program finds the guard as the one slab
    /// no allocation came from, with the geometry from public `mallctl` keys.
    c_guard_slab_fault => [ CTest { prog: "guard_slab", args: &["fault"], gate: Gate::GuardInstall, ..BASE } ];
    /// `tests/c/guard_slab.c vmas` (plan Phase 2 task 1): 10,000 committed
    /// slabs of the 16-byte class (640 MiB of address space, one guard slab
    /// in eight where the kernel has `MADV_GUARD_INSTALL`) intersect at most
    /// `VMA_BOUND` (8) lines of `/proc/self/maps`: guard regions inside the
    /// read-write range do not split it. Runs on every kernel; without guard
    /// regions it checks that no slab was skipped and the bound still holds.
    c_guard_slab_vmas => [ CTest { prog: "guard_slab", args: &["vmas"], ..BASE } ];
    /// `tests/c/pagemap_guard.c on`: under the default configuration the
    /// guard slabs of the 16-, 64-, 256- and 1024-byte classes are found as
    /// the gaps no allocation came from and checked through
    /// `ioctl(PAGEMAP_SCAN)` without touching them: `PAGE_IS_GUARD` on the
    /// guard page and nowhere else (6.16+), or the fallback the program
    /// names (6.13..6.15: not present, swap-type; before 6.13: no guard slab
    /// exists; before 6.7: the gap check alone). Runs on every kernel.
    c_pagemap_guard_on => [ CTest { prog: "pagemap_guard", args: &["on"], ..BASE } ];
    /// `tests/c/pagemap_guard.c off`: the control, under `guards:false` no
    /// span has a gap and no page reads `PAGE_IS_GUARD`.
    c_pagemap_guard_off => [ CTest { prog: "pagemap_guard", args: &["off"], conf: Some("abort_conf:true,guards:false"), ..BASE } ];
    /// `tests/c/huge_hole.c`: the alignment holes the huge allocator leaves
    /// below an aligned run are committed memory. Once in the probed guard mode
    /// (guard-install on kernels with `MADV_GUARD_INSTALL`, where the Phase 0
    /// regression lived) and once forced onto the `PROT_NONE` fallback.
    c_huge_hole => [ CTest { prog: "huge_hole", ..BASE } ];
    c_huge_hole_no_guards => [ CTest { prog: "huge_hole", conf: Some("abort_conf:true,guards:false"), ..BASE } ];
    /// The Phase 0 task 13 fuzz driver at a fraction of `make fuzz` (10M ops,
    /// 4 threads) so every `make test` runs it. Same seed as `make fuzz`.
    c_fuzz_short => [ CTest { dir: "fuzz", prog: "fuzz", args: &["400000", "4"], ..BASE } ];
    /// One thread and a 64-entry table: high churn per slot, no cross-thread
    /// traffic, so slots are reused quickly through the thread cache.
    c_fuzz_single_thread_small_table => [ CTest { dir: "fuzz", prog: "fuzz", args: &["200000", "1", "0x7", "64"], ..BASE } ];
    /// `free(NULL)`, `free_sized(NULL, 0)`, `malloc_usable_size(NULL)`,
    /// `realloc(NULL, 0)`, allocations and `newlocale`/`setlocale` from a
    /// `.preinit_array` entry, before any shared object's initialiser
    /// (glibc 2.44's `__newlocale` frees NULL from a static initialiser).
    c_preinit => [ CTest { prog: "preinit", ..BASE } ];
}

// ---------------------------------------------------------------------------
// Configuration sources (plan Phase 1 task 1): tests/c/conf.c under the
// real `.so`, through `run_conf`.
// ---------------------------------------------------------------------------

/// `conf` becomes the program's `je_malloc_conf` (statically initialised
/// and exported with `-rdynamic`, as a program linked against the
/// allocator would have it), `env` is added to the environment. With
/// `expect_abort` the first allocation must abort and stderr must carry
/// both the issue text and the `abort_conf` line; otherwise the program
/// must print `ok conf` and the allocator must not have complained.
fn run_conf(name: &str, conf: Option<&str>, env: &[(&str, &str)], expect_abort: Option<&str>) {
    run_conf_flags(name, conf, &[], env, expect_abort);
}

/// [`run_conf`] with extra compiler flags (`-DPLAIN` puts `conf` in the
/// program's `malloc_conf` instead of `je_malloc_conf`).
fn run_conf_flags(name: &str, conf: Option<&str>, extra: &[&str], env: &[(&str, &str)], expect_abort: Option<&str>) {
    let define = conf.map(|c| std::format!("-DCONF=\"{c}\""));
    let mut cflags = std::vec!["-rdynamic"];
    cflags.extend_from_slice(extra);
    if let Some(d) = &define {
        cflags.push(d);
    }
    let test = std::format!("c_conf {name}");
    match expect_abort {
        None => run_row(&test, &CTest { prog: "conf", cflags: &cflags, env, stderr_lacks: &["challoc:"], ..BASE }),
        Some(text) => run_row(
            &test,
            &CTest {
                prog: "conf",
                cflags: &cflags,
                env,
                expect: Expect::Abort { signal: SIGABRT, stderr_contains: &[text, "challoc: abort_conf:"] },
                ..BASE
            },
        ),
    }
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
    run_conf("check_zero", None, &[("MALLOC_CONF", "abort_conf:true,check_zero:true")], None);
}

#[test]
fn c_conf_inert_key_aborts_under_abort_conf() {
    // `stats_print` is parsed but nothing prints at exit: with abort_conf a
    // program cannot start believing it turned something on. `thp` left
    // the inert list with plan Phase 2.5 task 9 and passes, as does
    // `huge_retain_max`.
    run_conf(
        "inert",
        None,
        &[("MALLOC_CONF", "abort_conf:true,stats_print:true")],
        Some("challoc: MALLOC_CONF: conf key stats_print is accepted but has no effect (not implemented)"),
    );
    run_conf("huge_retain_max", None, &[("MALLOC_CONF", "abort_conf:true,huge_retain_max:1048576")], None);
    run_conf("thp", None, &[("MALLOC_CONF", "abort_conf:true,thp:auto")], None);
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
    run_row(
        "c_conf_env_overrides_the_symbol",
        &CTest {
            prog: "conf",
            cflags: &["-rdynamic", "-DCONF=\"abort_conf:true,no_such_key:1\""],
            conf: Some("abort_conf:false,narenas:-1"),
            stderr_has: &[
                "challoc: je_malloc_conf: unknown conf key \"no_such_key\"",
                "challoc: MALLOC_CONF: out-of-range conf value \"-1\" for narenas",
            ],
            ..BASE
        },
    );
}

// ---------------------------------------------------------------------------
// The release .so itself: symbol tables and the jemalloc-soname stage.
// ---------------------------------------------------------------------------

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
        "je_posix_memalign", "je_valloc", "je_pvalloc", "je_malloc_usable_size", "challoc_version", "challoc_features",
        "challoc_malloc_usable", "challoc_calloc_usable", "challoc_realloc_usable", "challoc_free_usable",
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

/// Phase 1 task 6: the `jemalloc-soname` build staged by
/// `scripts/jemalloc-compat.sh` (`make jemalloc-compat`; `make test` runs it
/// after the release build). The staged `libjemalloc.so.2` carries that
/// soname and exactly the export list of `libchalloc.so`, whose own soname
/// is `libchalloc.so.0` (design 13.6); the `libjemalloc.so` link name and `jemalloc.pc` resolve
/// through `pkg-config`; and `tests/c/jemalloc_link.c`, linked with nothing
/// but `$(pkg-config --cflags --libs jemalloc)`, records `libjemalloc.so.2`
/// in `DT_NEEDED`, fails without the staging directory on the library path,
/// and with it has every malloc entry point served from that file.
#[test]
fn jemalloc_compat_stage() {
    let lib = std::format!("{}/jemalloc-compat/stage/lib", target_dir());
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
    assert!(
        default_dyn.contains("Library soname: [libchalloc.so.0]"),
        "the default build must carry the soname libchalloc.so.0:\n{default_dyn}"
    );

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
        compat.difference(&default).collect::<Vec<_>>(),
        default.difference(&compat).collect::<Vec<_>>()
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
    let libs: Vec<&str> = libs.split_whitespace().collect();
    let cflags: Vec<&str> = cflags.split_whitespace().collect();
    assert!(libs.contains(&std::format!("-L{lib}").as_str()) && libs.contains(&"-ljemalloc"), "pkg-config --libs: {libs:?}");

    // A link-only consumer: DT_NEEDED names the soname, not the file it was
    // linked against; the loader needs the staging directory to find it.
    let bin = compiled(&CTest { prog: "jemalloc_link", cflags: &cflags, ldflags: &libs, ..BASE });
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "jemalloc_link failed ({:?})\nstdout: {stdout}\nstderr: {stderr}", run.status);
    assert!(stdout.lines().any(|l| l == "ok jemalloc_link"), "stdout: {stdout}\nstderr: {stderr}");
    std::eprintln!("jemalloc_link: {}", stdout.trim_end());
}

/// A few ordinary programs under `LD_PRELOAD`: the library survives real
/// startup sequences (a shell, readelf, the C compiler).
#[test]
fn ordinary_programs_run_under_ld_preload() {
    let so = release_so();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    for cmd in [
        std::vec!["sh", "-c", "ls /usr/lib >/dev/null && echo preload-ok"],
        std::vec!["readelf", "-h", so.as_str()],
        std::vec![cc.as_str(), "--version"],
    ] {
        let run = std::process::Command::new(cmd[0]).args(&cmd[1..]).env("LD_PRELOAD", &so).output().unwrap();
        assert!(
            run.status.success(),
            "{cmd:?} failed under LD_PRELOAD: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}
