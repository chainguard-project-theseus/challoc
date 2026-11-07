use super::*;
use std::vec::Vec;

#[test]
fn no_malloc_conf_symbol_in_the_test_binary() {
    // The test binary defines neither `malloc_conf` nor `je_malloc_conf`,
    // so both weak references resolve to null. `tests/c/conf.c` covers
    // the defined case under the real `.so`.
    assert_eq!(malloc_conf_symbol(0), None);
    assert_eq!(malloc_conf_symbol(1), None);
}

#[test]
fn env_var_is_read_from_environ() {
    // SAFETY: not fully met: this is the only test that mutates the
    // environment, but `env_var` in a concurrent test (or the global
    // init) walks `environ` without std's lock and could race it.
    unsafe {
        std::env::set_var("CHALLOC_CONF_TEST", "narenas:3,tcache:false");
        std::env::set_var("CHALLOC_CONF_TEST_EMPTY", "");
    }
    assert_eq!(
        env_var(b"CHALLOC_CONF_TEST"),
        Some((&b"narenas:3,tcache:false"[..], false))
    );
    assert_eq!(env_var(b"CHALLOC_CONF_TEST_EMPTY"), None);
    // The cap applies to the value, not to the `NAME=value` entry.
    unsafe {
        std::env::set_var("CHALLOC_CONF_TEST_LONG", "a".repeat(MAX_TEXT));
    }
    let (value, cut) = env_var(b"CHALLOC_CONF_TEST_LONG").unwrap();
    assert_eq!((value.len(), cut), (MAX_TEXT, false));
    unsafe {
        std::env::set_var("CHALLOC_CONF_TEST_LONG", "a".repeat(MAX_TEXT + 1));
    }
    let (value, cut) = env_var(b"CHALLOC_CONF_TEST_LONG").unwrap();
    assert_eq!((value.len(), cut), (MAX_TEXT, true));
    assert_eq!(env_var(b"CHALLOC_CONF_TES"), None, "prefix must not match");
    assert_eq!(env_var(b"CHALLOC_CONF_TEST_MISSING"), None);
    let mut cfg = Config::DEFAULT;
    assert_eq!(
        cfg.parse(env_var(b"CHALLOC_CONF_TEST").unwrap().0, |_: Issue<'_>| {}),
        0
    );
    assert_eq!(cfg.narenas, 3);
    assert!(!cfg.tcache);
}

#[test]
fn first_line_of_a_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(std::format!("challoc-conf-{}", std::process::id()));
    let mut buf = [0u8; MAX_TEXT + 1];
    let mut cpath = path.to_str().unwrap().as_bytes().to_vec();
    cpath.push(0);

    std::fs::write(&path, "tcache:false,narenas:2\r\nsecond line is ignored\n").unwrap();
    assert_eq!(
        first_line_of(&cpath, &mut buf),
        Some((&b"tcache:false,narenas:2"[..], false))
    );
    std::fs::write(&path, "no newline at all").unwrap();
    assert_eq!(
        first_line_of(&cpath, &mut buf),
        Some((&b"no newline at all"[..], false))
    );
    std::fs::write(&path, "\nsecond").unwrap();
    assert_eq!(first_line_of(&cpath, &mut buf), None, "empty first line");
    std::fs::write(&path, "").unwrap();
    assert_eq!(first_line_of(&cpath, &mut buf), None);
    // Longer than the buffer: cut at MAX_TEXT and flagged as cut.
    let long = std::format!("{}narenas:9", "tcache:true,".repeat(MAX_TEXT / 6));
    std::fs::write(&path, &long).unwrap();
    let (line, cut) = first_line_of(&cpath, &mut buf).unwrap();
    assert_eq!(line.len(), MAX_TEXT);
    assert!(cut);
    // Exactly MAX_TEXT bytes, with or without a newline, is not cut.
    let exact = "a".repeat(MAX_TEXT);
    for text in [exact.clone(), std::format!("{exact}\nnext")] {
        std::fs::write(&path, &text).unwrap();
        assert_eq!(
            first_line_of(&cpath, &mut buf),
            Some((exact.as_bytes(), false))
        );
    }
    // A long file whose first line ends inside the buffer is not cut.
    std::fs::write(&path, std::format!("narenas:9\n{long}")).unwrap();
    assert_eq!(
        first_line_of(&cpath, &mut buf),
        Some((&b"narenas:9"[..], false))
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(first_line_of(&cpath, &mut buf), None, "missing file");
    assert_eq!(first_line_of(b"/etc\0", &mut buf), None, "a directory");
}

/// A source longer than `MAX_TEXT` is cut in the middle of a pair; the
/// cut pair must not be applied with its shortened value, and the cut
/// counts as an issue, which `load` aborts on under `abort_conf`.
#[test]
fn a_cut_source_drops_the_cut_pair_and_reports_it() {
    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(b"abort_conf:true,junk:");
    text.extend(std::iter::repeat_n(b'x', MAX_TEXT - 31));
    text.extend_from_slice(b",narenas:12345\0");
    let (s, cut) = cstr(text.as_ptr().cast()).expect("non-empty");
    assert_eq!(s.len(), MAX_TEXT);
    assert!(s.ends_with(b",narenas:1"), "the cut splits narenas");
    assert!(cut);
    let mut cfg = Config::DEFAULT;
    let mut issues = Vec::new();
    let n = cfg.parse_source(s, cut, |i: Issue<'_>| issues.push(std::format!("{i}")));
    assert_eq!(n, issues.len());
    assert_eq!(cfg.narenas, Config::DEFAULT.narenas, "{issues:?}");
    assert!(cfg.abort_conf, "the pairs before the cut still apply");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].contains("longer than 4096 bytes"), "{issues:?}");
}

#[test]
fn cstr_flags_only_strings_longer_than_max_text() {
    for (len, want_cut) in [(1, false), (MAX_TEXT, false), (MAX_TEXT + 1, true)] {
        let mut text = std::vec![b'a'; len];
        text.push(0);
        let (s, cut) = cstr(text.as_ptr().cast()).expect("non-empty");
        assert_eq!(s.len(), len.min(MAX_TEXT), "len {len}");
        assert_eq!(cut, want_cut, "len {len}");
    }
    assert_eq!(cstr(c"".as_ptr()), None);
    assert_eq!(cstr(core::ptr::null()), None);
}

#[test]
fn load_with_a_clean_environment_gives_the_defaults() {
    // No `MALLOC_CONF`/`CHALLOC_CONF` in the test environment, no
    // `/etc/challoc.conf` in the container, no weak symbol: defaults.
    assert_eq!(
        env_var(b"MALLOC_CONF"),
        None,
        "MALLOC_CONF is set; the test assumes a clean environment"
    );
    assert_eq!(
        env_var(b"CHALLOC_CONF"),
        None,
        "CHALLOC_CONF is set; the test assumes a clean environment"
    );
    let mut buf = [0u8; MAX_TEXT + 1];
    if first_line_of(b"/etc/challoc.conf\0", &mut buf).is_none() {
        // The constants whatever the CPU: the L2-derived ladder of
        // design 6.2 ("Cache geometry") was priced and dropped.
        let got = load();
        assert_eq!(got, Config::DEFAULT);
    }
}

/// The environment has one reader, which `theorem_secure_conf`'s
/// premise names: `environ` (or glibc's `_environ` or `__environ`) is
/// read only in `environ_array`, which only `env_var`'s body calls, and
/// `env_var` is called only by `load` (twice, after its `AT_SECURE`
/// test, which Verus checks against `env_var`'s precondition) and by
/// the tests above. No code calls `getenv` or `secure_getenv`.
#[test]
fn environment_has_one_reader() {
    use crate::testutil::grant_sites;
    // Spelled in parts, so that this test's own lines match nothing.
    let var = std::format!("{}{}", "envi", "ron");
    let call = std::format!("{}{}(", "env_", "var");
    let def = std::format!("pub fn {call}");
    let shim = std::format!("{var}_array(");
    let shim_def = std::format!("fn {shim}");
    // A token naming `environ` or one of glibc's aliases for it
    // (`_environ`, `__environ`), not an identifier ending in it.
    let word = |at: &str| {
        at.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| w.trim_start_matches('_') == var)
    };
    let reads: Vec<_> = grant_sites(&var)
        .into_iter()
        .filter(|(_, at)| word(at))
        .collect();
    for (sig, at) in &reads {
        assert!(
            at.starts_with("config.rs:")
                && (sig.starts_with(&shim_def) || at.contains("static envi")),
            "{at}: the environment read in `{sig}`"
        );
    }
    assert_eq!(reads.len(), 2, "{reads:?}");
    let shim_calls = grant_sites(&shim);
    for (sig, at) in &shim_calls {
        assert!(
            at.starts_with("config.rs:") && (sig.starts_with(&def) || sig.starts_with(&shim_def)),
            "{at}: `environ_array` called in `{sig}`"
        );
    }
    assert_eq!(shim_calls.len(), 2, "{shim_calls:?}");
    let libc_lookups = grant_sites(&std::format!("{}{}", "get", "env"));
    assert!(libc_lookups.is_empty(), "{libc_lookups:?}");
    let calls = grant_sites(&call);
    let tests = [
        "env_var_is_read_from_environ",
        "load_with_a_clean_environment_gives_the_defaults",
    ]
    .map(|t| std::format!("fn {t}() {{"));
    for (sig, at) in &calls {
        // `load` and `env_var` itself in config.rs; the two tests in
        // this file.
        assert!(
            (at.starts_with("config.rs:")
                && (sig.starts_with("pub fn load(") || sig.starts_with(&def)))
                || (at.starts_with("config/linux_tests.rs:") && tests.contains(sig)),
            "{at}: `env_var` called in `{sig}`"
        );
    }
    let in_load = calls
        .iter()
        .filter(|(sig, _)| sig.starts_with("pub fn load("))
        .count();
    assert_eq!(in_load, 2, "{calls:?}");
}
