//! Test fixtures shared across modules: [`TestShared`], the one builder of
//! a [`Shared`] over a synthetic layout, that layout ([`regions_at`]) and
//! its real reservations ([`Reservation`]), [`map_rw`] for scratch
//! memory, a deterministic [`xorshift`],
//! [`global_state`], the guard of the tests that write or assert the
//! process-wide allocator's settings, [`mlock_or_skip`] for the tests of
//! purges that fail on locked memory, and the source scans of the guard
//! tests and other source checks ([`grant_sites`], [`assert_only_at`],
//! [`shipped_sources`], [`lock_spec_impls`]), and the weak-memory stress
//! tests' settings and report ([`stress_duration`], [`stress_threads`],
//! [`stress_report`]).

use core::mem::MaybeUninit;
use std::boxed::Box;

use crate::config::Config;
use crate::heap::Shared;
use crate::layout::{LG_MAX_SLAB, MAX_SLAB, Mapping, NCLASSES, Regions};
use crate::meta::Meta;
use crate::sys::pac;

/// The synthetic layout's class regions: the smallest `from_parts`
/// accepts, 4 MiB, so the 64 KiB classes have 64 slabs and the 4 MiB
/// class has one.
pub(crate) const LG_REGION: u32 = LG_MAX_SLAB;
pub(crate) const LG_STRIDE: u32 = LG_REGION + 2;
/// The synthetic layout's superblock: every class region and the slack
/// that aligns the first.
pub(crate) const SB_LEN: usize = (NCLASSES << LG_STRIDE) + MAX_SLAB;
/// The synthetic layout's metadata region.
pub(crate) const META_LEN: usize = 16 << 20;
/// 64 MiB huge region, reserved for real.
const HUGE_LEN: usize = 64 << 20;

/// The page size the synthetic layout is built with: the kernel's (the
/// reservations are real).
pub(crate) fn page_size() -> usize {
    crate::sys::features::page_size()
}

pub(crate) use crate::meta::align_up;

/// The synthetic layout: [`SB_LEN`] bytes of class regions at
/// `superblock`, `huge` and `meta` as `(base, len)`, and a
/// `1 << lg_page`-byte page. The addresses need not be mapped; the tests
/// that touch memory pass [`Reservation`]s.
pub(crate) fn regions_at(
    superblock: usize,
    huge: (usize, usize),
    meta: (usize, usize),
    lg_page: u32,
) -> Regions {
    Regions::from_parts(
        superblock,
        LG_REGION,
        LG_STRIDE,
        &[0usize; NCLASSES],
        huge,
        meta,
        lg_page,
        [
            Mapping {
                addr: superblock,
                len: SB_LEN,
            },
            Mapping {
                addr: huge.0,
                len: huge.1,
            },
            Mapping {
                addr: meta.0,
                len: meta.1,
            },
        ],
    )
}

/// A real `PROT_NONE`, `MAP_NORESERVE` reservation, unmapped on drop: `len`
/// bytes from `base`. Bind it before anything that points into it (a
/// [`Meta`], a [`Regions`]), so that those drop first.
pub(crate) struct Reservation {
    pub(crate) base: usize,
    pub(crate) len: usize,
    /// The whole mapping, alignment slack included.
    mapping: (usize, usize),
}

impl Reservation {
    /// `len` bytes at a page-aligned base.
    pub(crate) fn new(len: usize) -> Reservation {
        Reservation::aligned(len, page_size())
    }

    /// `len` bytes at a base aligned to `align`, a power of two: `mmap`
    /// only promises the page, so this reserves `align - page` bytes of
    /// slack beyond `len`.
    pub(crate) fn aligned(len: usize, align: usize) -> Reservation {
        use crate::sys::{MAP_ANONYMOUS, MAP_NORESERVE, MAP_PRIVATE, PROT_NONE};
        let total = len + align.saturating_sub(page_size());
        // SAFETY: a fresh anonymous reservation at an address the kernel
        // picks replaces nothing.
        let raw = unsafe {
            crate::sys::mmap(
                0,
                total,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                -1,
                0,
            )
        }
        .expect("reserve");
        Reservation {
            base: align_up(raw, align),
            len,
            mapping: (raw, total),
        }
    }

    /// The whole mapping as `(addr, len)`, alignment slack included.
    pub(crate) fn mapping(&self) -> (usize, usize) {
        self.mapping
    }

    /// A metadata allocator over `base..base + len` at the kernel's page.
    pub(crate) fn meta(&self) -> Meta {
        Meta::new(self.base, self.len, page_size())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // SAFETY: the mapping `aligned` made; its users were bound after
        // it and are gone.
        unsafe { crate::sys::munmap(self.mapping.0, self.mapping.1) }.unwrap();
    }
}

/// A fresh anonymous read-write mapping of `len` bytes; the caller
/// unmaps it, or leaves it to the process.
pub(crate) fn map_rw(len: usize) -> usize {
    use crate::sys::{MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    // SAFETY: a fresh anonymous mapping at an address the kernel picks
    // replaces nothing.
    unsafe {
        crate::sys::mmap(
            0,
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    }
    .expect("map")
}

/// A `Shared` over a synthetic 4 MiB-per-class layout. The regions are
/// real `PROT_NONE` reservations so slot memory can be touched. The
/// result is leaked on purpose: the test thread's own heap is torn down
/// by the key destructor after the test body has returned.
pub(crate) struct TestShared {
    config: Config,
    guards: bool,
    pac: bool,
    gc_interval_ns: Option<u64>,
}

impl TestShared {
    /// The heap tests' configuration: one automatic arena, so every heap
    /// of a test shares arena 0 (the default would spread concurrently
    /// live heaps over `4 * ncpus`), and `pac_pointers` on, so the signed
    /// path runs on a host with pointer authentication and the plain path
    /// elsewhere.
    pub(crate) fn new() -> TestShared {
        TestShared::with(Config {
            narenas: 1,
            pac_pointers: true,
            ..Config::DEFAULT
        })
    }

    /// `config` as given. No guard slabs, the PAC probe's real answer.
    pub(crate) fn with(config: Config) -> TestShared {
        TestShared {
            config,
            guards: false,
            pac: pac::available(),
            gc_interval_ns: None,
        }
    }

    /// Guard slabs at the configured interval (`guard_install` as on a
    /// kernel with guard regions).
    pub(crate) fn guards(mut self) -> TestShared {
        self.guards = true;
        self
    }

    /// Stand in for the PAC probe: `false` on a host that has the
    /// extension builds the fallback (plain pointers) that CPUs without it
    /// take.
    pub(crate) fn pac(mut self, available: bool) -> TestShared {
        self.pac = available;
        self
    }

    /// Replace the GC interval `Shared::init` stores
    /// (`Shared::gc_interval_ns`).
    pub(crate) fn gc_interval_ns(mut self, ns: u64) -> TestShared {
        self.gc_interval_ns = Some(ns);
        self
    }

    pub(crate) fn build(self) -> &'static Shared {
        // Leaked with the `Shared`.
        let sb = core::mem::ManuallyDrop::new(Reservation::aligned(SB_LEN, MAX_SLAB));
        let meta = core::mem::ManuallyDrop::new(Reservation::new(META_LEN));
        let huge = core::mem::ManuallyDrop::new(Reservation::new(HUGE_LEN));
        let regions = regions_at(
            sb.base,
            (huge.base, huge.len),
            (meta.base, meta.len),
            page_size().trailing_zeros(),
        );
        // One raw pointer for `init`, the field edit and the returned
        // `&Shared`, as in `api::init`: `init` keeps pointers derived from
        // it, which a reborrow through a `&mut` in between would
        // invalidate.
        let p = Box::into_raw(Box::new(MaybeUninit::<Shared>::uninit())).cast::<Shared>();
        // SAFETY: `p` is a leaked, never-moved allocation for a `Shared`
        // that nothing reads before `init` returns, and the regions are
        // the reservations made above.
        assert!(unsafe {
            Shared::init(
                p,
                regions,
                vstd::prelude::Tracked::assume_new(),
                self.config,
                self.guards,
                self.pac,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            )
        });
        if let Some(ns) = self.gc_interval_ns {
            // SAFETY: `*p` is initialised, and no reference to it exists
            // yet; the field is plain data.
            unsafe { (*p).gc_interval_ns = ns };
        }
        // SAFETY: `init` returned `true`, so `*p` is initialised; it is
        // leaked, so the reference is valid for `'static`.
        unsafe { &*p }
    }
}

/// `TestShared::new().build()`.
pub(crate) fn shared() -> &'static Shared {
    TestShared::new().build()
}

/// One step of Marsaglia's xorshift64; `state` must not be zero.
pub(crate) use crate::class::xorshift;

/// The tests of the process-wide allocator (`api::shared()`) that write a
/// process-wide setting (decay, the background thread request, the stats
/// epoch, the fork handlers' lock state) or assert one take turns on this
/// guard. libtest runs the tests of one process in parallel, and without it
/// one test's write lands between another's write and its assertion.
/// Tests that only allocate and free need not take it: none of the guarded
/// assertions depends on another thread not allocating, and the one that
/// would (every lock free after the fork handlers) runs in a fork child.
static GLOBAL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`GLOBAL_STATE`]; a panic in one holder must not fail the rest.
pub(crate) fn global_state() -> std::sync::MutexGuard<'static, ()> {
    GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` in a child of a real `fork` and assert that it returned. The
/// child has one thread, so a check of process-wide state there cannot be
/// raced by the other tests of this process, and what `f` writes (a
/// sticky flag, a lock state) never reaches them. A panic in `f`
/// exits the child with status 1 (its message goes to stderr) instead of
/// unwinding into a copy of the test harness; a child still running after
/// `timeout_ms` is killed and fails the test. The test binary's own
/// allocator is glibc's, which `fork` leaves usable in the child, so `f`
/// may allocate and use `assert!`.
pub(crate) fn in_fork_child(what: &str, timeout_ms: u64, f: impl FnOnce()) {
    // SAFETY: the child only runs `f` and exits; see the doc above for
    // why the test binary's allocator is usable there.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "{what}: fork failed");
    if pid == 0 {
        let r = std::panic::catch_unwind(core::panic::AssertUnwindSafe(f));
        // SAFETY: `_exit` has no preconditions; it skips the harness's
        // exit handlers, which belong to the parent.
        unsafe { libc::_exit(if r.is_ok() { 0 } else { 1 }) };
    }
    let mut status = 0;
    let mut waited = 0;
    loop {
        // SAFETY: `status` is a valid `int` to write.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert!(r >= 0, "{what}: waitpid failed");
        if r == pid {
            break;
        }
        if waited >= timeout_ms {
            // SAFETY: `pid` is our unreaped child; `status` as above.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
            panic!("{what}: fork child still running after {timeout_ms} ms");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        waited += 10;
    }
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "{what}: fork child failed (status {status:#x}; its panic is on stderr)"
    );
}

/// `mlock` `[addr, addr + len)` for a test, or, when the host refuses (a
/// low `RLIMIT_MEMLOCK`), [`skip`] the test and return false.
pub(crate) fn mlock_or_skip(test: &str, addr: usize, len: usize) -> bool {
    // SAFETY: `mlock` only pins pages; it fails on an unmapped range.
    if unsafe { libc::mlock(addr as *const _, len) } == 0 {
        return true;
    }
    skip(
        test,
        &std::format!("mlock({len}) failed: {}", std::io::Error::last_os_error()),
    );
    false
}

/// Write `SKIP <test> <reason>` to stderr on a line of its own, in one
/// write. `ci/skip-summary.sh` keys a skip on the word after a `SKIP` that
/// starts a line, and `make test` merges the container's stdout and
/// stderr out of order: libtest writes `test <name> ... ` and `ok` as
/// separate pieces, and so does `eprintln!` with arguments. One write
/// keeps the key whole, and the leading newline puts `SKIP` at the start
/// of a line even after a cut libtest line.
pub(crate) fn print_skip(test: &str, reason: &str) {
    use std::io::Write;
    let line = std::format!("\nSKIP {test} {reason}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
}

/// Print `test`'s `SKIP` line with `reason`, the host lacking a feature
/// the test needs; the skip fails the test unless
/// `CHALLOC_TEST_ALLOW_SKIP` is set (docs/testing.md, "Skips").
pub(crate) fn skip(test: &str, reason: &str) {
    print_skip(test, reason);
    assert!(
        std::env::var_os("CHALLOC_TEST_ALLOW_SKIP").is_some(),
        "SKIP {test} {reason}\n(set CHALLOC_TEST_ALLOW_SKIP=1 to let a test pass with a note on a host without the feature)"
    );
}

/// The `.rs` files under `dir`, in path order.
pub(crate) fn walk(dir: &std::path::Path, out: &mut std::vec::Vec<std::path::PathBuf>) {
    let mut entries: std::vec::Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether `t` starts a `fn` item: qualifiers, then `fn `.
pub(crate) fn fn_item(t: &str) -> bool {
    let mut t = t;
    loop {
        let rest = [
            "pub ",
            "const ",
            "unsafe ",
            "extern \"C\" ",
            "proof ",
            "open ",
            "closed ",
            "broadcast ",
            "spec ",
            "axiom ",
            "uninterp ",
        ]
        .iter()
        .find_map(|q| t.strip_prefix(q))
        .or_else(|| {
            t.strip_prefix("pub(")
                .and_then(|r| r.split_once(") ").map(|(_, r)| r))
        });
        match rest {
            Some(r) => t = r,
            None => return t.starts_with("fn "),
        }
    }
}

/// Every line under `src/` that mentions `grant`, as the `fn` item line it
/// belongs to (the nearest one above it) and its place
/// (`file:line: `text``, the file relative to `src/`), skipping comments
/// and the line `pub proof fn {grant}()`. Test code is scanned too, so a
/// test that names what it looks for spells the name in parts.
pub(crate) fn grant_sites(
    grant: &str,
) -> std::vec::Vec<(std::string::String, std::string::String)> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = std::vec::Vec::new();
    walk(&src, &mut files);
    let def = std::format!("pub proof fn {grant}()");
    let mut sites = std::vec::Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&src).unwrap().to_str().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        let mut sig = "";
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") || t == def {
                continue;
            }
            if fn_item(t) {
                sig = t;
            }
            if t.contains(grant) {
                sites.push((sig.into(), std::format!("{rel}:{}: `{t}`", i + 1)));
            }
        }
    }
    sites
}

/// A count for [`assert_only_at`]: one call or more.
pub(crate) const ANY: usize = usize::MAX;

/// Assert that `call` (a name and its `(`, such as `axiom_x(`) is made
/// only in the functions `sites` lists, as `(file, fn, count)`: a file
/// relative to `src/` with its `:` (`"heap/mod.rs:"`), the start of the
/// function's `fn` item line (`"pub fn destroy("`), and the number of
/// calls in it ([`ANY`]: at least one). A line that declares the name
/// (`fn name(` or `fn name<`) is not a call. The match is on the exact
/// spelling: a call through a `use` alias or `Self::` is not seen, so the
/// pinned items are called by their full spelling. Every
/// listed function must still make the call, so a call that moves or goes
/// away updates the list too. Pass `call` built from parts
/// (`concat!("axiom_", "x(")`), so that the caller's own lines match nothing.
///
/// The guard tests (`verus/README.md`, "Guard tests") pin with it the
/// call sites of trusted axioms whose premise holds only where they are
/// called: a new call needs that premise argued for its own caller, in the
/// axiom's doc, before it is added to the list.
pub(crate) fn assert_only_at(call: &str, sites: &[(&str, &str, usize)]) {
    // The name `call` declares (`entry` for `TlsSlot::entry()`).
    let name = call
        .rsplit("::")
        .next()
        .unwrap()
        .trim_end_matches(['(', ')']);
    let declares = |text: &str| {
        text.contains(&std::format!("fn {name}(")) || text.contains(&std::format!("fn {name}<"))
    };
    // (fn item line, place, calls on the line).
    let found: std::vec::Vec<_> = grant_sites(call)
        .into_iter()
        .filter_map(|(sig, at)| {
            let text = at.split_once(": `").map_or("", |(_, t)| t);
            let calls = text.matches(call).count();
            (!declares(text)).then_some((sig, at, calls))
        })
        .collect();
    assert!(!found.is_empty(), "no call of `{call}` under src/");
    for (sig, at, _) in &found {
        assert!(
            sites
                .iter()
                .any(|(file, f, _)| at.starts_with(file) && sig.starts_with(f)),
            "{at}: a call of `{call}` in `{sig}`, which is not one of its listed sites. \
             Its premise is argued per caller: name the caller in the doc of `{name}` with \
             why the premise holds there, and add it to this test's list, in one change."
        );
    }
    for &(file, f, want) in sites {
        let n: usize = found
            .iter()
            .filter(|(sig, at, _)| at.starts_with(file) && sig.starts_with(f))
            .map(|(_, _, k)| k)
            .sum();
        if want == ANY {
            assert!(
                n > 0,
                "{file} `{f}`: got = no call of `{call}`, want = at least one \
                 (drop the site from the list and from the axiom's doc)"
            );
        } else {
            assert_eq!(
                n, want,
                "{file} `{f}`: got = {n} calls of `{call}`, want = {want}"
            );
        }
    }
}

/// The source of every non-test file under `src/` that the build or
/// Verus compiles, as (path relative to `src/`, text), with its
/// top-level test and Kani modules (`#[cfg(test)]`, `#[cfg(all(test, ..))]`
/// or `#[cfg(kani)]` on a `mod name {` at column 0, through the next `}`
/// at column 0) blanked to empty lines, so line numbers are kept. Files
/// that are test-only or Kani-only as a whole are skipped: `tests.rs`,
/// `*_tests.rs`, the files of a `tests/` module directory, this file,
/// `api/c_programs.rs`, and the `model/` and `kani/` trees.
pub(crate) fn shipped_sources() -> std::vec::Vec<(std::string::String, std::string::String)> {
    use std::string::{String, ToString};
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = std::vec::Vec::new();
    walk(&src, &mut files);
    let mut out = std::vec::Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&src).unwrap().to_str().unwrap();
        let name = rel.rsplit('/').next().unwrap();
        if name == "tests.rs"
            || name.ends_with("_tests.rs")
            || rel.contains("/tests/")
            || rel == "testutil.rs"
            || rel == "api/c_programs.rs"
            || rel.starts_with("model/")
            || rel.starts_with("kani/")
        {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        let lines: std::vec::Vec<&str> = text.lines().collect();
        let mut kept = String::new();
        let mut skip = false;
        for (i, line) in lines.iter().enumerate() {
            let next = lines.get(i + 1).map_or("", |l| l.trim_start());
            let next = next
                .strip_prefix("pub(crate) ")
                .or_else(|| next.strip_prefix("pub "))
                .unwrap_or(next);
            if !skip
                && (*line == "#[cfg(test)]"
                    || *line == "#[cfg(kani)]"
                    || line.starts_with("#[cfg(all(test,"))
                && next.starts_with("mod ")
                && next.ends_with('{')
            {
                skip = true;
            }
            if !skip {
                kept.push_str(line);
            }
            kept.push('\n');
            if skip && *line == "}" {
                skip = false;
            }
        }
        out.push((rel.to_string(), kept));
    }
    out
}

/// The module a file under `src/` is, as a path (`crate::class::table` for
/// `class/table.rs`, `crate::class` for `class/mod.rs`).
pub(crate) fn module_of(rel: &str) -> std::string::String {
    let stem = rel.strip_suffix(".rs").unwrap();
    let stem = stem.strip_suffix("/mod").unwrap_or(stem);
    std::format!("crate::{}", stem.replace('/', "::"))
}

/// The type `ty`, written in module `m`, by its path: as written when it
/// starts with `crate::`, else as an item of `m`.
pub(crate) fn type_key(m: &str, ty: &str) -> std::string::String {
    use std::string::ToString;
    let ty = ty.trim();
    if ty.starts_with("crate::") {
        ty.to_string()
    } else {
        std::format!("{m}::{ty}")
    }
}

/// One `LockSpec` impl in [`shipped_sources`] ([`lock_spec_impls`]).
pub(crate) struct LockSpecImpl {
    /// The file, relative to `src/`.
    pub(crate) rel: std::string::String,
    /// The `impl` line and its closing line, as 0-based line indices
    /// (equal for an impl on one line).
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// The implementing type by its path ([`type_key`] in the file's
    /// module).
    pub(crate) ty: std::string::String,
    /// It defines `lock_spec_inv` (does not delegate to `assumed_inv`).
    pub(crate) inv: bool,
    /// It defines `lock_spec_frame` (does not delegate to `assumed_frame`).
    pub(crate) frame: bool,
    /// The body of its `lock_spec_frame` is the one word `true`.
    pub(crate) true_frame: bool,
}

/// Every `LockSpec` impl in [`shipped_sources`] outside `lock.rs` (which
/// defines the trait; its own impls are in its tests), read from each
/// `impl .. LockSpec for T` line through the next line at that line's
/// indentation that is `}`. An impl defines a method when its body never
/// calls the matching `assumed_*` function.
pub(crate) fn lock_spec_impls() -> std::vec::Vec<LockSpecImpl> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // Spelled in parts, so that this file's own lines match nothing.
    let for_ = std::format!("{}{}", "LockSpec", " for ");
    let assumed_inv = std::format!("{}{}", "assumed_", "inv(");
    let assumed_frame = std::format!("{}{}", "assumed_", "frame(");
    let frame_fn = std::format!("{}{}", "fn lock_spec_", "frame");
    // A use of `name`, not the tail of a longer name.
    let calls = |t: &str, name: &str| t.match_indices(name).any(|(i, _)| !t[..i].ends_with(ident));
    let mut out = std::vec::Vec::new();
    for (rel, text) in shipped_sources() {
        if rel == "lock.rs" {
            continue;
        }
        let m = module_of(&rel);
        let lines: std::vec::Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim_start();
            if t.starts_with("impl") && t.contains(for_.as_str()) {
                let start = i;
                let rest = &t[t.find(for_.as_str()).unwrap() + for_.len()..];
                let ty = rest.split(" where").next().unwrap().trim_end();
                let ty = ty.trim_end_matches('}').trim_end_matches('{').trim_end();
                let indent = &lines[i][..lines[i].len() - t.len()];
                let mut body = std::vec::Vec::new();
                if !t.trim_end().ends_with('}') {
                    i += 1;
                    while lines[i] != std::format!("{indent}}}") {
                        body.push(lines[i]);
                        i += 1;
                    }
                }
                let text = body.join("\n");
                // The body of `lock_spec_frame`, its lines trimmed.
                let frame: std::vec::Vec<&str> = body
                    .iter()
                    .skip_while(|l| !l.contains(frame_fn.as_str()))
                    .skip(1)
                    .map(|l| l.trim())
                    .take_while(|l| *l != "}")
                    .filter(|l| !l.is_empty())
                    .collect();
                out.push(LockSpecImpl {
                    rel: rel.clone(),
                    start,
                    end: i,
                    ty: type_key(&m, ty),
                    inv: !calls(&text, &assumed_inv),
                    frame: !calls(&text, &assumed_frame),
                    true_frame: frame == ["true"],
                });
            }
            i += 1;
        }
    }
    out
}

/// How long one weak-memory stress test runs: `CHALLOC_STRESS_SECS`
/// seconds, 5 by default. The stress tests are `#[ignore]`d, outside the
/// default run; `cargo test --release --lib -- --ignored stress_` runs
/// them.
pub(crate) fn stress_duration() -> std::time::Duration {
    let secs = std::env::var("CHALLOC_STRESS_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);
    std::time::Duration::from_secs(secs)
}

/// The stress tests' thread count: `CHALLOC_STRESS_THREADS`, else the
/// CPUs the process may run on.
pub(crate) fn stress_threads() -> usize {
    std::env::var("CHALLOC_STRESS_THREADS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
}

/// Print one stress test's result on a line of its own, in one write:
/// `STRESS <test> key=value ...`.
pub(crate) fn stress_report(test: &str, fields: &str) {
    use std::io::Write;
    let line = std::format!("\nSTRESS {test} {fields}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
}
