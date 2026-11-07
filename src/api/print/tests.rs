use super::*;
use crate::api::libc::{free, malloc};
use std::string::String;
use std::sync::Mutex;
use std::vec::Vec;

/// The entry point as the export wrapper calls it, with a placeholder
/// token for the call.
fn malloc_stats_print(write_cb: WriteCallback, cbopaque: *mut c_void, opts: *const c_char) {
    super::malloc_stats_print(write_cb, cbopaque, opts, Tracked::assume_new())
}

static CAPTURE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Held across a whole `report`: the callback appends one chunk at a
/// time under `CAPTURE`'s lock, so two tests reporting at once would
/// interleave their chunks (seen as a text report that began with
/// another test's JSON report).
static REPORT: Mutex<()> = Mutex::new(());

unsafe extern "C" fn capture(opaque: *mut c_void, s: *const c_char) {
    assert_eq!(opaque as usize, 0xC0FFEE);
    // SAFETY: the hook is handed a NUL-terminated chunk.
    let s = unsafe { core::ffi::CStr::from_ptr(s) };
    CAPTURE.lock().unwrap().extend_from_slice(s.to_bytes());
}

fn report(opts: &str) -> String {
    let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
    CAPTURE.lock().unwrap().clear();
    let c = std::ffi::CString::new(opts).unwrap();
    malloc_stats_print(
        WriteCallback(Some(capture)),
        0xC0FFEE as *mut c_void,
        if opts.is_empty() {
            core::ptr::null()
        } else {
            c.as_ptr()
        },
    );
    String::from_utf8(CAPTURE.lock().unwrap().clone()).unwrap()
}

/// A two-byte character that lands on the last free byte of the
/// 1023-byte buffer: the sink must not slice the `str` inside it, and
/// the bytes reach the callback whole and in order.
#[test]
fn sink_splits_a_character_at_the_buffer_end() {
    let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
    CAPTURE.lock().unwrap().clear();
    let mut sink = Sink::new(
        Target::Callback(WriteCallback(Some(capture)), 0xC0FFEE as *mut c_void),
        Tracked::assume_new(),
    );
    let head = "x".repeat(1022);
    sink.write_str(&head).unwrap();
    sink.write_str("\u{e9} tail").unwrap();
    sink.flush();
    let got = String::from_utf8(CAPTURE.lock().unwrap().clone()).unwrap();
    assert_eq!(got, std::format!("{head}\u{e9} tail"));
}

/// Page-sized slabs (design 3): after init no class has a slab
/// smaller than the kernel page (up to 64 KiB), so on a 16 or 64 KiB
/// kernel the 512..1024-byte classes have page-sized slabs, every
/// table's page group is one slab, and the report says so; on a 4 KiB
/// kernel the table is the compile-time one and the report is silent.
#[test]
fn slabs_are_never_smaller_than_the_page() {
    use crate::layout::{
        BASE_CLASSES, CLASSES, LG_SMALL_SLAB, MIN_SLAB, NCLASSES, page_sized_classes,
    };
    let s = crate::api::shared();
    let page = s.regions.page_size();
    let floor = page.min(1 << LG_SMALL_SLAB);
    for c in 0..NCLASSES {
        let cls = &CLASSES[c];
        assert!(
            cls.slab_size() >= floor,
            "class {c}: slab {} below the page {page}",
            cls.slab_size()
        );
        assert_eq!(cls.nslots(), cls.slab_size() / cls.size(), "class {c}");
        assert_eq!(
            cls.lg_slab as u32,
            (BASE_CLASSES[c].lg_slab as u32).max(page.trailing_zeros().min(LG_SMALL_SLAB)),
            "class {c}"
        );
        assert_eq!(
            s.tables()[c].slabs_per_page(),
            (page / cls.slab_size()).max(1),
            "class {c}"
        );
        if page <= 1 << LG_SMALL_SLAB {
            assert_eq!(
                s.tables()[c].slabs_per_page(),
                1,
                "class {c}: page groups are dormant"
            );
        }
    }
    let text = report("");
    if page > MIN_SLAB {
        let (lo, hi) = page_sized_classes().expect("a large page raises the small-slab classes");
        assert_eq!((CLASSES[lo].size(), CLASSES[hi].size()), (512, 1024));
        let want = std::format!(
            "Page-sized slabs: classes {lo}..{hi} (512..1024 bytes) at {} bytes (4096..16384 in the table)\n",
            floor
        );
        assert!(text.contains(&want), "{text}");
    } else {
        assert_eq!(page_sized_classes(), None);
        assert_eq!(&*CLASSES, &BASE_CLASSES);
        assert!(!text.contains("Page-sized slabs"), "{text}");
    }
}

#[test]
fn text_report_has_every_section_and_live_figures() {
    let live: Vec<*mut u8> = (0..100)
        .map(|_| malloc(700, vstd::prelude::Tracked::assume_new()))
        .collect();
    let text = report("");
    assert!(
        text.starts_with("___ Begin jemalloc statistics ___\n"),
        "{text}"
    );
    assert!(
        text.ends_with("--- End jemalloc statistics ---\n"),
        "{text}"
    );
    for needle in [
        "Version: \"5.3.0-0-gchalloc\"",
        "config.quantum:",
        "Run-time option settings",
        "opt.dirty_decay_ms: ",
        "opt.thp: \"never\"\n",
        "thp: never -> off (thp:never) nohuge ",
        " (classes, metadata; huge follows host) class_ranges 0 (0 bytes, 0 chunks) huge_ranges 0 meta_ranges 0 failures 0; host ",
        "opt.zero_on_free: false\n",
        "opt.slot_randomize: false\n",
        "opt.cgroup: true\n",
        "opt.stats_print: accepted, no effect",
        "opt.double_free_scan: false\n",
        "opt.huge_retain_max: 67108864\n",
        "Kernel and CPU features: page_size=",
        "Sealed ranges: ",
        "Background thread: ",
        "Arenas: ",
        "\nQuantum size: ",
        "\nPage size: ",
        "Allocated: ",
        "Huge: ",
        "Metadata: reserved ",
        "  gated clamped\n",
        "\ncgroup: ",
        "\ncgroup policy: ",
        "\npurge advice: eager MADV_DONTNEED (no headroom known) lazy_purges 0 (0 bytes) eager_purges ",
        "\nthp: ",
        "arenas[0]:",
        "  dirty_decay_ms: ",
        "  small:",
        "  large:",
        "  bins:        size ind",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
    // One bins table per built arena; this thread's heap is on
    // whichever automatic arena `Arenas::assign` gave it, so the live
    // blocks show in the class's row of that table.
    let size = crate::layout::class_size_of(700).unwrap();
    // The slab histogram rows start with the size too but are not all
    // numeric; the bins rows are.
    let rows: Vec<Vec<usize>> = text
        .lines()
        .filter(|l| l.trim_start().starts_with(&std::format!("{size} ")))
        .filter_map(|l| {
            l.split_whitespace()
                .map(|w| w.parse::<usize>().ok())
                .collect::<Option<Vec<usize>>>()
        })
        .collect();
    assert!(!rows.is_empty(), "bin row for the live class in:\n{text}");
    let curregs: usize = rows.iter().map(|cols| cols[4]).sum();
    assert!(curregs >= 100, "curregs in {rows:?}");
    // The Allocated figure is live, not the epoch snapshot.
    let line = text.lines().find(|l| l.starts_with("Allocated: ")).unwrap();
    let allocated: usize = line
        .split(", ")
        .next()
        .unwrap()
        .trim_start_matches("Allocated: ")
        .parse()
        .unwrap();
    assert!(allocated >= 100 * size);
    for p in live {
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    // Option letters.
    let g = report("g");
    assert!(!g.contains("Version:") && g.contains("arenas[0]:"), "{g}");
    let a = report("a");
    assert!(a.contains("Version:") && !a.contains("arenas[0]:"), "{a}");
    let b = report("b");
    assert!(b.contains("arenas[0]:") && !b.contains("  bins:"), "{b}");
    let ga = report("gab");
    assert_eq!(ga, "--- End jemalloc statistics ---\n");
}

#[test]
fn json_report_is_well_formed_enough_to_parse_by_hand() {
    let text = report("J");
    assert!(text.starts_with("{\n\t\"jemalloc\": {\n"), "{text}");
    assert!(text.trim_end().ends_with('}'));
    assert!(text.contains("\"version\": \"5.3.0-0-gchalloc\""));
    assert!(text.contains("\"opt\": {"));
    assert!(text.contains("\"stats\": {\"allocated\": "));
    assert!(text.contains("\"cgroup\": {\"tracked\": "));
    assert!(
        text.contains("\"thp\": {\"key\": \"never\", \"mode\": \"off\", "),
        "{text}"
    );
    assert!(
        text.contains("\"failures\": 0, \"nohuge_ranges\": ") && text.contains(", \"host\": \""),
        "{text}"
    );
    assert!(
        text.contains("\"purge_advice\": \"eager\", \"lazy_purges\": 0, \"eager_purges\": "),
        "{text}"
    );
    assert!(text.contains("\"stats.arenas\": {"));
    assert!(text.contains("\"0\": {"));
    assert!(text.contains("\"bins\": ["));
    // Braces and brackets balance; no trailing commas before a closer.
    let mut depth = 0i32;
    for ch in text.chars() {
        match ch {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0);
    }
    assert_eq!(depth, 0, "unbalanced: {text}");
    let compact: String = text.split_whitespace().collect();
    assert!(
        !compact.contains(",}") && !compact.contains(",]"),
        "trailing comma in:\n{text}"
    );
    // One bins array of NBINS entries per arena section in this very
    // text (other tests create arenas concurrently).
    assert_eq!(
        text.matches("\"size\": ").count(),
        NBINS * text.matches("\"pactive\": ").count()
    );
    let ga = report("Jga");
    assert!(
        ga.contains("\"version\"") && !ga.contains("\"opt\"") && !ga.contains("\"stats.arenas\""),
        "{ga}"
    );
}

/// Every printable character of a report except the figures: digit runs
/// (with a sign) become `N`, `true`/`false` become `B`, a quoted value
/// after `: ` becomes `"S"`. Keys, punctuation and indentation stay.
fn masked(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix(": \"") {
            let end = r.find('"').expect("closing quote");
            out.push_str(": \"S\"");
            rest = &r[end + 1..];
        } else if let Some(r) = rest.strip_prefix("true") {
            out.push('B');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("false") {
            out.push('B');
            rest = r;
        } else if c.is_ascii_digit()
            || (c == '-' && rest[1..].starts_with(|d: char| d.is_ascii_digit()))
        {
            out.push('N');
            rest = rest[1..].trim_start_matches(|d: char| d.is_ascii_digit());
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The JSON report's shape, pinned character for character with the
/// figures masked (`masked`): the general section once, then one
/// arena block per arena in the report with `NBINS` bin rows each.
/// A change to the report's layout has to change this text; a refactor
/// of the writer that keeps the output byte-identical does not.
#[test]
fn json_report_shape_is_pinned() {
    let mut general = String::from(
        "{\n\t\"jemalloc\": {\n\t\t\"version\": \"S\",\n\t\t\"implementation\": \"S\",\n\t\t\"config\": {\"debug\": B, \"stats\": B, \"quantum\": N},\n\t\t\"opt\": {\n",
    );
    let opts: Vec<String> = KEYS
        .iter()
        .filter_map(|key| {
            let v = crate::api::shared().config.get(key.as_bytes())?;
            let shape = match v {
                Value::Bool(_) => "B",
                Value::Int(_) | Value::Uint(_) | Value::Size(_) => "N",
                Value::Str(_) => "\"S\"",
            };
            Some(std::format!("\t\t\t\"{key}\": {shape}"))
        })
        .collect();
    general.push_str(&opts.join(",\n"));
    general.push_str(concat!(
        "\n\t\t},\n",
        "\t\t\"arenas\": {\"narenas\": N, \"nbins\": N, \"quantum\": N, \"page\": N},\n",
        "\t\t\"background_thread\": {\"present\": B},\n",
        "\t\t\"thp\": {\"key\": \"S\", \"mode\": \"S\", \"reason\": \"S\", \"class_ranges\": N, \"class_bytes\": N, \"chunks\": N, \"huge_ranges\": N, \"meta_ranges\": N, \"failures\": N, \"nohuge_ranges\": N, \"host\": \"S\"},\n",
        "\t\t\"cgroup\": {\"tracked\": B, \"limit\": N, \"high\": N, \"current\": N, \"decay_ms\": N, \"retention_cap\": N, \"empty_bytes\": N, \"full_purges\": N, \"thp_forced\": B, \"purge_advice\": \"S\", \"lazy_purges\": N, \"eager_purges\": N, \"purged_is_zero\": B, \"failed_discards\": N, \"failed_discard_bytes\": N},\n",
        "\t\t\"stats\": {\"allocated\": N, \"active\": N, \"metadata\": N, \"resident\": N, \"mapped\": N, \"retained\": N},\n",
        "\t\t\"stats.arenas\": {\n",
    ));
    let mut arena = String::from(concat!(
        "\t\t\t\"N\": {\n",
        "\t\t\t\t\"dirty_decay_ms\": N,\n",
        "\t\t\t\t\"pactive\": N, \"pdirty\": N, \"pmuzzy\": N,\n",
        "\t\t\t\t\"small\": {\"allocated\": N, \"active\": N, \"dirty\": N, \"retained\": N},\n",
        "\t\t\t\t\"large\": {\"allocated\": N, \"active\": N, \"dirty\": N, \"retained\": N, \"runs\": N},\n",
        "\t\t\t\t\"metadata\": N, \"resident\": N, \"mapped\": N, \"retained\": N,\n",
        "\t\t\t\t\"bins\": [\n",
    ));
    for j in 0..NBINS {
        arena.push_str("\t\t\t\t\t{\"size\": N, \"nregs\": N, \"slab_size\": N, \"curregs\": N, \"curslabs\": N, \"nonfull_slabs\": N, \"empty_slabs\": N, \"purged_slabs\": N}");
        arena.push_str(if j + 1 == NBINS { "\n" } else { ",\n" });
    }
    arena.push_str("\t\t\t\t]\n\t\t\t}");
    let text = report("J");
    let got = masked(&text);
    let narenas = text.matches("\"pactive\": ").count();
    assert!(narenas >= 1, "{text}");
    let mut want = general;
    want.push_str(&std::vec![arena.as_str(); narenas].join(",\n"));
    want.push_str("\n\t\t}\n\t}\n}\n");
    assert_eq!(got, want, "masked JSON report:\n{got}");
    // Without bins the arena block ends at the totals line; without
    // arenas the general section closes the object itself.
    let b = masked(&report("Jb"));
    assert!(b.contains("\"retained\": N\n\t\t\t}"), "{b}");
    assert!(!b.contains("\"bins\""), "{b}");
    let a = masked(&report("Ja"));
    assert!(a.ends_with("\"retained\": N}\n\t}\n}\n"), "{a}");
    let g = masked(&report("Jg"));
    assert!(g.starts_with("{\n\t\"jemalloc\": {\n\t\t\"version\": \"S\",\n\t\t\"implementation\": \"S\",\n\t\t\"stats.arenas\": {\n"), "{g}");
}

/// The text report's line set, pinned by each line's head: its text up
/// to the first `:` with the figures masked (`masked`), the whole line
/// when it has no `:`, and `<row>` for a run of table rows. The general
/// section is pinned line for line; every arena block is the fixed
/// lines down to the bins header, then the bins rows, the slab-allocated
/// large classes and the occupancy histogram, each present only when it
/// has rows. A refactor of the writers keeps this; a renamed, added or
/// dropped line has to change it.
#[test]
fn text_report_line_set_is_pinned() {
    fn head(line: &str) -> String {
        let m = masked(line);
        if m.trim_start().starts_with('N') {
            return "<row>".into();
        }
        match m.find(':') {
            Some(i) => m[..=i].into(),
            None => m,
        }
    }
    let text = report("");
    let mut heads: Vec<String> = Vec::new();
    for line in text.lines() {
        let h = head(line);
        if h != "<row>" || heads.last().is_none_or(|p| p != "<row>") {
            heads.push(h);
        }
    }
    let mut want: Vec<String> = std::vec![
        "___ Begin jemalloc statistics ___".into(),
        "Version:".into(),
        "Build-time option settings".into(),
        "  config.debug:".into(),
        "  config.stats:".into(),
        "  config.quantum:".into(),
        "Run-time option settings".into(),
    ];
    want.extend(
        KEYS.iter()
            .chain(INERT_KEYS)
            .map(|key| std::format!("  opt.{key}:")),
    );
    want.extend(
        [
            "Kernel and CPU features:",
            "Sealed ranges:",
            "Background thread:",
            "Arenas:",
            "Quantum size:",
            "Page size:",
        ]
        .map(String::from),
    );
    if crate::layout::page_sized_classes().is_some() {
        want.push("Page-sized slabs:".into());
    }
    want.extend(
        [
            "Allocated:",
            "Huge:",
            "Metadata:",
            "cgroup:",
            "cgroup policy:",
            "purge advice:",
            "thp:",
        ]
        .map(String::from),
    );
    let general = heads
        .iter()
        .position(|h| h == "arenas[N]:")
        .unwrap_or_else(|| panic!("no arena block in:\n{text}"));
    assert_eq!(heads[..general], want[..], "general section of:\n{text}");
    assert_eq!(
        heads.last().map(String::as_str),
        Some("--- End jemalloc statistics ---")
    );
    let fixed = [
        "arenas[N]:",
        "  dirty_decay_ms:",
        "  pactive:",
        "                            allocated       active        dirty     retained",
        "  small:",
        "  large:",
        "  metadata:",
        "  bins:",
    ];
    let tables = [
        "",
        "  large classes (slab-allocated, not bins):",
        "  slab occupancy (non-empty slabs per eighth of nslots, last = full):",
    ];
    let arenas = &heads[general..heads.len() - 1];
    let starts: Vec<usize> = (0..arenas.len())
        .filter(|&i| arenas[i] == "arenas[N]:")
        .collect();
    for (k, &start) in starts.iter().enumerate() {
        let block = &arenas[start..starts.get(k + 1).copied().unwrap_or(arenas.len())];
        assert!(
            block.len() >= fixed.len() && block[..fixed.len()] == fixed,
            "arena block {block:?} in:\n{text}"
        );
        let mut rest = &block[fixed.len()..];
        for table in tables {
            if !table.is_empty() && rest.first().is_some_and(|h| h == table) {
                rest = &rest[1..];
                assert!(
                    rest.first().is_some_and(|h| h == "<row>"),
                    "`{table}` without rows in {block:?}"
                );
            }
            if rest.first().is_some_and(|h| h == "<row>") {
                rest = &rest[1..];
            }
        }
        assert!(
            rest.is_empty(),
            "unexpected lines {rest:?} in arena block {block:?} of:\n{text}"
        );
    }
}

/// The module docs promise that nothing in a report allocates: the
/// calling thread's `thread.allocated` and `thread.deallocated` do not
/// move across a text and a JSON report written to a sink callback
/// that itself allocates nothing.
#[test]
fn stats_print_does_not_allocate() {
    unsafe extern "C" fn sink(_opaque: *mut c_void, _s: *const c_char) {}
    let s = crate::api::shared();
    // Make sure this thread has a heap (the counters live in it), then
    // read them through the same materialisation `mallctl` uses.
    free(
        malloc(64, vstd::prelude::Tracked::assume_new()),
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let counters = || {
        let st = crate::heap::heap_for_thread(s).unwrap().materialise_stats();
        (st.allocated, st.deallocated)
    };
    let before = counters();
    let _serial = REPORT.lock().unwrap_or_else(|e| e.into_inner());
    malloc_stats_print(
        WriteCallback(Some(sink)),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    let json = std::ffi::CString::new("J").unwrap();
    malloc_stats_print(
        WriteCallback(Some(sink)),
        core::ptr::null_mut(),
        json.as_ptr(),
    );
    assert_eq!(
        counters(),
        before,
        "a report moved this thread's allocation counters"
    );
}

#[test]
fn stats_print_without_callback_uses_the_message_hook() {
    let _serial = crate::testutil::global_state();
    use crate::api::jemalloc::set_message_hook_for_tests;
    static HOOKED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    unsafe extern "C" fn hook(opaque: *mut c_void, s: *const c_char) {
        assert!(opaque.is_null());
        HOOKED
            .lock()
            .unwrap()
            // SAFETY: the hook is handed a NUL-terminated message.
            .extend_from_slice(unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes());
    }
    set_message_hook_for_tests(Some(hook));
    malloc_stats_print(
        WriteCallback(None),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    set_message_hook_for_tests(None);
    let got = String::from_utf8(HOOKED.lock().unwrap().clone()).unwrap();
    assert!(
        got.starts_with("___ Begin jemalloc statistics ___\n"),
        "{got}"
    );
    assert!(got.ends_with("--- End jemalloc statistics ---\n"));
}

/// Design 8.1: a report runs the program's `write_cb` and its
/// `malloc_message` hook with cancellation disabled, and gives the
/// thread its cancel state back afterwards.
#[test]
fn report_callbacks_run_with_cancellation_disabled() {
    use crate::api::jemalloc::set_message_hook_for_tests;
    use core::sync::atomic::{AtomicUsize, Ordering};
    // glibc's values, which the `libc` crate does not bind.
    const PTHREAD_CANCEL_ENABLE: i32 = 0;
    const PTHREAD_CANCEL_DISABLE: i32 = 1;
    unsafe extern "C" {
        fn pthread_setcancelstate(state: i32, old: *mut i32) -> i32;
    }
    /// The calling thread's cancel state, left unchanged.
    fn cancel_state() -> i32 {
        let (mut old, mut ignored) = (0, 0);
        // SAFETY: both are live ints; the second call restores the
        // state the first one read.
        unsafe {
            pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut old);
            pthread_setcancelstate(old, &mut ignored);
        }
        old
    }
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static ENABLED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn record(_opaque: *mut c_void, _s: *const c_char) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if cancel_state() != PTHREAD_CANCEL_DISABLE {
            ENABLED.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _serial = crate::testutil::global_state();
    // Each case starts from one cancel state, which the report must
    // leave as it found it.
    for start in [PTHREAD_CANCEL_ENABLE, PTHREAD_CANCEL_DISABLE] {
        let mut old = 0;
        // SAFETY: `old` is a live int.
        unsafe { pthread_setcancelstate(start, &mut old) };
        // The `write_cb`, then (with none) the `malloc_message` hook.
        for (write_cb, hook) in [
            (WriteCallback(Some(record)), None),
            (
                WriteCallback(None),
                Some(record as unsafe extern "C" fn(_, _)),
            ),
        ] {
            CALLS.store(0, Ordering::Relaxed);
            ENABLED.store(0, Ordering::Relaxed);
            set_message_hook_for_tests(hook);
            malloc_stats_print(write_cb, core::ptr::null_mut(), core::ptr::null());
            set_message_hook_for_tests(None);
            assert!(CALLS.load(Ordering::Relaxed) > 0, "no callback ran");
            assert_eq!(
                ENABLED.load(Ordering::Relaxed),
                0,
                "callback calls with cancellation enabled: got = {}, want = 0",
                ENABLED.load(Ordering::Relaxed)
            );
            assert_eq!(cancel_state(), start, "cancel state after the report");
        }
        // SAFETY: `old` is a live int; this restores the test thread's state.
        unsafe { pthread_setcancelstate(old, &mut old) };
    }
}

#[test]
fn malloc_info_writes_glibc_xml_to_the_stream() {
    let live: Vec<*mut u8> = (0..50)
        .map(|_| malloc(3000, vstd::prelude::Tracked::assume_new()))
        .collect();
    let big = malloc(1 << 20, vstd::prelude::Tracked::assume_new());
    // SAFETY: `tmpfile` has no preconditions.
    let f = unsafe { libc::tmpfile() };
    assert!(!f.is_null());
    assert_eq!(malloc_info(0, f, Tracked::assume_new()), 0);
    // SAFETY: `f` is the open stream from `tmpfile`.
    unsafe { libc::rewind(f) };
    let mut buf = std::vec![0u8; 1 << 16];
    // SAFETY: `buf` has `buf.len()` writable bytes; `f` is open.
    let n = unsafe { libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), f) };
    // SAFETY: `f` is open; the `malloc_info(1, f)` below rejects its
    // options before it would touch the closed stream.
    unsafe { libc::fclose(f) };
    let xml = String::from_utf8(buf[..n].to_vec()).unwrap();
    assert!(xml.starts_with("<malloc version=\"1\">\n"), "{xml}");
    assert!(xml.ends_with("</malloc>\n"), "{xml}");
    assert!(xml.contains("<heap nr=\"0\">"));
    let size = crate::layout::class_size_of(3000).unwrap();
    assert!(
        xml.contains(&std::format!(
            "<size from=\"{size}\" to=\"{size}\" total=\""
        )),
        "{xml}"
    );
    assert!(xml.contains("<total type=\"mmap\" count=\""));
    assert_eq!(
        xml.matches("<heap nr=").count(),
        xml.matches("</heap>").count()
    );
    if let Err(e) = xml_check(&xml) {
        panic!("malloc_info is not well-formed XML: {e}\n{xml}");
    }
    assert_eq!(malloc_info(1, f, Tracked::assume_new()), -1);
    // SAFETY: `__errno_location` returns this thread's `errno`.
    assert_eq!(unsafe { *libc::__errno_location() }, 22);
    assert_eq!(
        malloc_info(0, core::ptr::null_mut(), Tracked::assume_new()),
        -1
    );
    free(
        big,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    for p in live {
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    // glibc's malloc_stats just has to run and stay on stderr.
    malloc_stats();
}

/// How long a report that re-enters the allocator may take; one that
/// has not finished by then is deadlocked.
const REENTRY_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

/// What a report's external call does in the re-entry tests: allocate
/// and free a small bin (16), a larger bin (3000), a slab-allocated
/// large class (64 KiB) and a huge run (4 MiB), collect the live
/// statistics (the locks a report's collection takes; `epoch` would
/// also take them, but it moves the process-wide epoch other tests
/// read), and print a nested report, which takes every lock the outer
/// one takes. The nested report's callback discards its text and does
/// not re-enter. A lock held only over a stretch in which no chunk is
/// flushed is not caught.
fn reenter() {
    let live: Vec<*mut u8> = [16usize, 3000, 64 << 10, 4 << 20]
        .iter()
        .map(|&n| malloc(n, vstd::prelude::Tracked::assume_new()))
        .collect();
    for p in live {
        assert!(!p.is_null());
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    let mut tmp = stats::ArenaStats::empty(0);
    let _ = stats::collect(crate::api::shared(), &mut tmp, Tracked::assume_new());
    unsafe extern "C" fn discard(_: *mut c_void, _: *const c_char) {}
    malloc_stats_print(
        WriteCallback(Some(discard)),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
}

/// A `write_cb` that counts its calls in the `AtomicUsize` behind
/// `opaque` and re-enters the allocator.
unsafe extern "C" fn reentering_cb(opaque: *mut c_void, _s: *const c_char) {
    // SAFETY: every caller passes a live `AtomicUsize` as `opaque`.
    let calls = unsafe { &*(opaque as *const core::sync::atomic::AtomicUsize) };
    calls.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    reenter();
}

/// Run `report` on its own thread and fail unless it finishes within
/// [`REENTRY_BOUND`]. A deadlocked report keeps the lock it waits
/// behind, so every later test that needs that lock would hang too:
/// on a timeout the process aborts after saying why.
fn finishes(what: &str, report: impl FnOnce() + Send + 'static) {
    use std::sync::mpsc::RecvTimeoutError;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        report();
        let _ = tx.send(());
    });
    match rx.recv_timeout(REENTRY_BOUND) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => {
            std::eprintln!(
                "{what} did not finish within {REENTRY_BOUND:?}: \
                 an allocator lock held across its external call?"
            );
            std::process::abort();
        }
        Err(RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

/// The run-time evidence for `call_back`'s premise that the calling
/// thread holds no allocator lock (`ord.top() == 0`): every call of a
/// text and a JSON report re-enters the allocator ([`reenter`]). A
/// lock a report takes, held across a call, would deadlock the call's
/// collection or nested report.
#[test]
fn a_report_callback_may_reenter_the_allocator() {
    for opts in ["", "J"] {
        finishes(&std::format!("malloc_stats_print({opts:?})"), move || {
            let calls = core::sync::atomic::AtomicUsize::new(0);
            let c = std::ffi::CString::new(opts).unwrap();
            malloc_stats_print(
                WriteCallback(Some(reentering_cb)),
                &calls as *const _ as *mut c_void,
                c.as_ptr(),
            );
            assert!(
                calls.load(core::sync::atomic::Ordering::Relaxed) > 1,
                "the report fits one chunk: nothing was called mid-report"
            );
        });
    }
}

/// As [`a_report_callback_may_reenter_the_allocator`], for
/// `write_file`'s `fwrite`: an unbuffered `fopencookie` stream whose
/// write function re-enters the allocator, so every `fwrite` the sink
/// makes calls it before returning. A block live in every bin class
/// gives the XML a `<size>` row per bin, so it outgrows the sink's
/// 1023-byte buffer and at least one write lands mid-report.
#[test]
fn a_malloc_info_stream_may_reenter_the_allocator() {
    /// glibc's `cookie_io_functions_t`.
    #[repr(C)]
    struct CookieIo {
        read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> isize>,
        write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> isize>,
        seek: Option<unsafe extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int>,
        close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    }
    unsafe extern "C" {
        fn fopencookie(cookie: *mut c_void, mode: *const c_char, io: CookieIo) -> *mut libc::FILE;
    }
    unsafe extern "C" fn write(cookie: *mut c_void, _buf: *const c_char, n: usize) -> isize {
        // SAFETY: the cookie is the test's `AtomicUsize`.
        unsafe { reentering_cb(cookie, core::ptr::null()) };
        n as isize
    }
    finishes("malloc_info", || {
        let live: Vec<*mut u8> = (0..NBINS)
            .map(|c| malloc(CLASSES[c].size(), vstd::prelude::Tracked::assume_new()))
            .collect();
        let calls = core::sync::atomic::AtomicUsize::new(0);
        let io = CookieIo {
            read: None,
            write: Some(write),
            seek: None,
            close: None,
        };
        // SAFETY: `calls` outlives the stream, which is closed below;
        // the mode is a C string.
        let f = unsafe { fopencookie(&calls as *const _ as *mut c_void, c"w".as_ptr(), io) };
        assert!(!f.is_null());
        // SAFETY: `f` is open and has not been written to.
        let unbuffered = unsafe { libc::setvbuf(f, core::ptr::null_mut(), libc::_IONBF, 0) };
        assert_eq!(unbuffered, 0);
        assert_eq!(malloc_info(0, f, Tracked::assume_new()), 0);
        let during = calls.load(core::sync::atomic::Ordering::Relaxed);
        // SAFETY: `f` is the open stream from `fopencookie`.
        unsafe { libc::fclose(f) };
        for p in live {
            free(
                p,
                vstd::prelude::Tracked::assume_new(),
                vstd::prelude::Tracked::assume_new(),
            );
        }
        assert!(
            during > 1,
            "the XML fits one chunk: nothing was written mid-report"
        );
    });
}

/// A strict JSON (RFC 8259) parser that only validates: `Err` names
/// the byte offset of the first error. Enough for the reports, which
/// a consumer (tikv-jemalloc-ctl's users, `jq`) parses as JSON.
fn json_check(text: &str) -> Result<(), String> {
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn err<T>(&self, what: &str) -> Result<T, String> {
            let ctx = &self.b[self.i.saturating_sub(40)..self.b.len().min(self.i + 20)];
            Err(std::format!(
                "{what} at byte {}: ...{}...",
                self.i,
                String::from_utf8_lossy(ctx)
            ))
        }
        fn ws(&mut self) {
            while self.i < self.b.len() && b" \t\n\r".contains(&self.b[self.i]) {
                self.i += 1;
            }
        }
        fn eat(&mut self, c: u8) -> bool {
            self.ws();
            if self.b.get(self.i) == Some(&c) {
                self.i += 1;
                true
            } else {
                false
            }
        }
        fn value(&mut self) -> Result<(), String> {
            self.ws();
            match self.b.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    if self.eat(b'}') {
                        return Ok(());
                    }
                    loop {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return self.err("expected a key");
                        }
                        self.string()?;
                        if !self.eat(b':') {
                            return self.err("expected ':'");
                        }
                        self.value()?;
                        if self.eat(b'}') {
                            return Ok(());
                        }
                        if !self.eat(b',') {
                            return self.err("expected ',' or '}'");
                        }
                    }
                }
                Some(b'[') => {
                    self.i += 1;
                    if self.eat(b']') {
                        return Ok(());
                    }
                    loop {
                        self.value()?;
                        if self.eat(b']') {
                            return Ok(());
                        }
                        if !self.eat(b',') {
                            return self.err("expected ',' or ']'");
                        }
                    }
                }
                Some(b'"') => self.string(),
                Some(b't') => self.word("true"),
                Some(b'f') => self.word("false"),
                Some(b'n') => self.word("null"),
                Some(b'-' | b'0'..=b'9') => self.number(),
                _ => self.err("expected a value"),
            }
        }
        fn word(&mut self, w: &str) -> Result<(), String> {
            if self.b[self.i..].starts_with(w.as_bytes()) {
                self.i += w.len();
                Ok(())
            } else {
                self.err("bad literal")
            }
        }
        fn string(&mut self) -> Result<(), String> {
            self.i += 1;
            while let Some(&c) = self.b.get(self.i) {
                self.i += 1;
                match c {
                    b'"' => return Ok(()),
                    b'\\' => match self.b.get(self.i) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            let hex = self.b.get(self.i + 1..self.i + 5);
                            if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                return self.err("bad \\u escape");
                            }
                            self.i += 5;
                        }
                        _ => return self.err("bad escape"),
                    },
                    0..=0x1f => return self.err("control character in a string"),
                    _ => {}
                }
            }
            self.err("unterminated string")
        }
        fn number(&mut self) -> Result<(), String> {
            let digits = |p: &mut Self| {
                let s = p.i;
                while p.b.get(p.i).is_some_and(u8::is_ascii_digit) {
                    p.i += 1;
                }
                p.i > s
            };
            if self.b[self.i] == b'-' {
                self.i += 1;
            }
            if self.b.get(self.i) == Some(&b'0') {
                self.i += 1;
            } else if !digits(self) {
                return self.err("bad number");
            }
            if self.b.get(self.i) == Some(&b'.') {
                self.i += 1;
                if !digits(self) {
                    return self.err("bad fraction");
                }
            }
            if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
                self.i += 1;
                if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                    self.i += 1;
                }
                if !digits(self) {
                    return self.err("bad exponent");
                }
            }
            Ok(())
        }
    }
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
    };
    p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return p.err("trailing bytes after the document");
    }
    Ok(())
}

/// A minimal XML well-formedness check for `malloc_info`'s output:
/// one root element, every tag closed in order, attributes
/// `name="value"` with no `<` or `&` in a value, no stray text.
fn xml_check(text: &str) -> Result<(), String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = text;
    let mut roots = 0;
    loop {
        let t = rest.trim_start();
        if t.is_empty() {
            break;
        }
        if !t.starts_with('<') {
            return Err(std::format!("text outside a tag: {:.40}", t));
        }
        let end = t.find('>').ok_or("unterminated tag")?;
        let tag = &t[1..end];
        rest = &t[end + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            match stack.pop() {
                Some(open) if open == name => {}
                other => return Err(std::format!("</{name}> closes {other:?}")),
            }
            continue;
        }
        let (body, empty) = match tag.strip_suffix('/') {
            Some(b) => (b, true),
            None => (tag, false),
        };
        let (name, mut attrs) = body.split_once(' ').unwrap_or((body, ""));
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(std::format!("bad element name in <{tag}>"));
        }
        loop {
            attrs = attrs.trim_start();
            if attrs.is_empty() {
                break;
            }
            let (key, v) = attrs
                .split_once("=\"")
                .ok_or(std::format!("bad attribute in <{tag}>"))?;
            if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(std::format!("bad attribute name in <{tag}>"));
            }
            let close = v
                .find('"')
                .ok_or(std::format!("unterminated attribute in <{tag}>"))?;
            if v[..close].contains(['<', '&']) {
                return Err(std::format!("unescaped character in <{tag}>"));
            }
            attrs = &v[close + 1..];
            if !attrs.is_empty() && !attrs.starts_with(' ') {
                return Err(std::format!("attributes not separated in <{tag}>"));
            }
        }
        if stack.is_empty() {
            roots += 1;
        }
        if !empty {
            stack.push(name);
        } else if stack.is_empty() {
            return Err("empty root element".into());
        }
    }
    if !stack.is_empty() {
        return Err(std::format!("unclosed {stack:?}"));
    }
    if roots != 1 {
        return Err(std::format!("{roots} root elements"));
    }
    Ok(())
}

#[test]
fn json_and_xml_checkers_reject_what_they_should() {
    assert!(json_check("{\"a\": [1, -2.5e3, true, null, \"x\\n\"], \"b\": {}}").is_ok());
    for bad in [
        "{\"a\": 1,}",
        "[1,]",
        "{\"a\" 1}",
        "{\"a\": 01}",
        "{} {}",
        "{\"a\": \"\t\"}",
        "{",
    ] {
        assert!(json_check(bad).is_err(), "{bad}");
    }
    assert!(xml_check("<m v=\"1\">\n<a x=\"2\"/>\n</m>\n").is_ok());
    for bad in [
        "<m><a></m></a>",
        "<m>",
        "<m/>",
        "<m x=1></m>",
        "<m></m><n></n>",
        "<m>t</m>",
    ] {
        assert!(xml_check(bad).is_err(), "{bad}");
    }
}

/// Every option string jemalloc's `malloc_stats_print` defines, with
/// and without `J`: the letters challoc honours (`g`, `a`, `b`) in
/// every combination, each with none, each one and all of the letters
/// it accepts without effect (`m`, `d`, `l`, `x`, `e`, `h`). Every
/// `J` report parses as JSON (`g` with `a` makes `implementation` the
/// last member of `jemalloc`, with no comma after it); every text
/// report ends with the end line
/// and starts with the begin line exactly when `g` is absent.
#[test]
fn every_option_string_gives_a_well_formed_report() {
    let live: Vec<*mut u8> = (0..20)
        .map(|i| malloc(64 << (i % 8), vstd::prelude::Tracked::assume_new()))
        .collect();
    let inert = ["", "m", "d", "l", "x", "e", "h", "mdlxeh"];
    for mask in 0..8u32 {
        let mut honoured = String::new();
        for (bit, c) in ['g', 'a', 'b'].into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                honoured.push(c);
            }
        }
        for extra in inert {
            for json in [true, false] {
                let opts = std::format!("{}{honoured}{extra}", if json { "J" } else { "" });
                let text = report(&opts);
                if json {
                    if let Err(e) = json_check(&text) {
                        panic!("malloc_stats_print({opts:?}) is not JSON: {e}\n{text}");
                    }
                } else {
                    assert!(
                        text.ends_with("--- End jemalloc statistics ---\n"),
                        "{opts:?}: {text}"
                    );
                    assert_eq!(
                        text.starts_with("___ Begin jemalloc statistics ___\n"),
                        !honoured.contains('g'),
                        "{opts:?}: {text}"
                    );
                }
            }
        }
    }
    for p in live {
        free(
            p,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
}

/// `word` gives the text of `Name::cstr`, the C string `mallctl`
/// hands out, for every name.
#[test]
fn word_is_the_c_name() {
    for n in [
        Name::Thp(Thp::Never),
        Name::Thp(Thp::Auto),
        Name::Thp(Thp::Always),
        Name::PercpuArena(PercpuArena::Disabled),
        Name::PercpuArena(PercpuArena::Percpu),
        Name::PercpuArena(PercpuArena::Phycpu),
    ] {
        assert_eq!(Ok(word(n)), n.cstr().to_str(), "{n:?}");
    }
}
