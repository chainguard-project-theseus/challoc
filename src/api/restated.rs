//! Checks that the verification-only theorems that restate executable
//! code still make the calls of the code they restate.
//!
//! `libc::theorem_no_syscall_on_fast_paths` restates the fast paths of
//! `libc::malloc_class`, `free_block`, `free_sized_block` and
//! `free_usable_block`, and `theorem_read_only_never_aborts` the bodies of
//! `jemalloc::nallocx`, `challoc_features` and `glibc::mallinfo2`, and
//! `mallctl::theorem_mallctl_reads_never_abort` and
//! `theorem_mallctlbymib_reads_never_abort` the bodies of `mallctl` and
//! `mallctlbymib`. Verus
//! checks only the callees a theorem calls, so nothing in the proof ties a
//! restatement to its body. These tests read both from the source and
//! compare the ordered call names: a call added to (or dropped from) a
//! body or its restatement fails here until the other side follows.
//!
//! What is compared is the sequence of called function and method names
//! (the last path segment, `Flags::decode` as `decode`), in source order,
//! with comments, attributes, `proof { .. }` blocks, `let tracked` and
//! `let ghost` statements, `assert`, `assume` and `reveal` calls,
//! `Tracked`/`Ghost` paths and constructor calls
//! (an upper-case name: `Some(..)`, `Tracked(..)`) left out. Arguments,
//! control flow and the order of evaluation are not compared, and each
//! case lists the names it skips on both sides (`Case::skip`), with the
//! reason.

use std::string::String;
use std::vec::Vec;

/// A body and its restatement in a theorem.
struct Case {
    /// The restated function's file under `src/`.
    file: &'static str,
    /// The start of its definition, unique in the file.
    def: &'static str,
    /// A callee whose body stands in for its call: its definition in
    /// the same file. The theorem restates the callee's calls inline.
    inline: Option<(&'static str, &'static str)>,
    /// The theorem's part that restates it: a `match which` arm's
    /// pattern, or the name in backticks at the head of a comment block.
    part: &'static str,
    /// Call names left out on both sides.
    skip: &'static [&'static str],
}

const FAST_PATHS: &str = "api/libc.rs";
const FAST_PATHS_DEF: &str = "fn theorem_no_syscall_on_fast_paths(";
const READ_ONLY: &str = "api/mod.rs";
const READ_ONLY_DEF: &str = "fn theorem_read_only_never_aborts(";
const MALLCTL: &str = "api/mallctl.rs";

/// `theorem_no_syscall_on_fast_paths`'s restated arms. Skipped: the slow
/// paths' tail calls, which the theorem leaves out because they may make
/// system calls; the tests that pick between the fast and slow path
/// (`is_some`, `is_null`, aarch64's null test included); and the class
/// computation the theorem restates from the callers (`malloc_block`,
/// `malloc_medium`, `free_sized_with`, `free_sized_medium`).
const FAST_PATH_CASES: &[Case] = &[
    Case {
        file: FAST_PATHS,
        def: "fn malloc_class(",
        inline: None,
        part: "0",
        skip: &[
            "malloc_slow",
            "malloc_slow_class",
            "class_index_small",
            "class_index",
        ],
    },
    Case {
        file: FAST_PATHS,
        def: "pub fn free_block(",
        inline: None,
        part: "2",
        skip: &["free_slow", "is_some"],
    },
    Case {
        file: FAST_PATHS,
        def: "pub fn free_sized_block(",
        inline: None,
        part: "3",
        skip: &[
            "free_sized_unhinted",
            "free_sized_slow",
            "is_null",
            "class_index_small",
            "class_index",
        ],
    },
    Case {
        file: FAST_PATHS,
        def: "pub fn free_usable_block(",
        inline: None,
        part: "_",
        skip: &["free_usable_slow", "is_null"],
    },
];

/// `theorem_read_only_never_aborts`'s restated bodies. Skipped: `shared`,
/// for which the theorem takes its result as a parameter; and in
/// `mallinfo2`, its arithmetic (`saturating_sub` and the figure
/// arithmetic `fig_add` and `fig_mul`, none with a precondition), of which
/// the theorem samples `fig_mul` once.
const READ_ONLY_CASES: &[Case] = &[
    Case {
        file: "api/jemalloc.rs",
        def: "pub fn nallocx(",
        inline: None,
        part: "nallocx",
        skip: &["shared"],
    },
    Case {
        file: READ_ONLY,
        def: "pub extern \"C\" fn challoc_features(",
        inline: Some(("feature_line_ready", "fn feature_line_ready(")),
        part: "challoc_features",
        skip: &["shared"],
    },
    Case {
        file: "api/glibc.rs",
        def: "pub fn mallinfo2(",
        inline: None,
        part: "mallinfo2",
        skip: &["shared", "saturating_sub", "fig_add", "fig_mul"],
    },
];

/// `theorem_mallctl_reads_never_abort`'s and
/// `theorem_mallctlbymib_reads_never_abort`'s restated bodies. Skipped:
/// `shared_published`, for which the theorems take its results as
/// parameters, and `never_aborts`, the theorems' test that the key is
/// one they cover.
const MALLCTL_SKIP: &[&str] = &["shared_published", "never_aborts"];

const MALLCTL_CASES: &[Case] = &[Case {
    file: MALLCTL,
    def: "pub fn mallctl(",
    inline: None,
    part: "mallctl",
    skip: MALLCTL_SKIP,
}];

const MALLCTLBYMIB_CASES: &[Case] = &[Case {
    file: MALLCTL,
    def: "pub fn mallctlbymib(",
    inline: None,
    part: "mallctlbymib",
    skip: MALLCTL_SKIP,
}];

fn source(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The index just past the bracket that closes the one at `open`.
fn close(s: &[u8], open: usize) -> usize {
    let (o, c) = match s[open] {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'<' => (b'<', b'>'),
        x => panic!("no bracket at {open}: {:?}", x as char),
    };
    let mut depth = 0;
    for (i, &b) in s.iter().enumerate().skip(open) {
        if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    panic!("unbalanced {:?} at {open}", o as char)
}

/// The body (braces included) of the one function in `text` whose
/// definition starts with `def`: the first `{` after its parameters, or,
/// when a `requires` or `ensures` clause comes first (whose expressions
/// may hold braces, `CEntry::Mallctl { .. }`), the first line that is
/// a lone `{` at column 0, where a top-level function's body opens.
fn body<'a>(text: &'a str, def: &str) -> &'a str {
    let hits: Vec<_> = text.match_indices(def).collect();
    assert_eq!(hits.len(), 1, "`{def}` is not defined exactly once");
    let at = hits[0].0 + def.len() - 1;
    let s = text.as_bytes();
    let params = close(s, at);
    let rest = &text[params..];
    let first = rest.find('{').unwrap();
    let open = params
        + if rest[..first].contains("requires") || rest[..first].contains("ensures") {
            rest.find("\n{").unwrap() + 1
        } else {
            first
        };
    &text[open..close(s, open)]
}

/// `s` with `//` and `/* */` comments dropped and string literals
/// emptied (`""`), so neither a `//` nor a call inside a string counts.
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("\"\"");
        } else if s[i..].starts_with("//") {
            i += s[i..].find('\n').unwrap_or(s.len() - i);
        } else if s[i..].starts_with("/*") {
            i += s[i..].find("*/").map_or(s.len() - i, |e| e + 2);
            out.push(' ');
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether the word at `i` in `s` is `w`, standing alone.
fn word_at(s: &[u8], i: usize, w: &str) -> bool {
    s[i..].starts_with(w.as_bytes())
        && (i == 0 || !is_ident(s[i - 1]))
        && s.get(i + w.len()).is_none_or(|&b| !is_ident(b))
}

/// `code` (comments already stripped) without its ghost parts:
/// attributes, `proof { .. }` blocks, Verus's ghost statements in exec
/// code (`assert(..)`, `assume(..)`, `reveal(..)`), and `let tracked` and
/// `let ghost` statements.
fn strip_ghost(code: &str) -> String {
    const GHOST_CALLS: &[&str] = &["assert", "assume", "reveal"];
    let s = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    let skip_ws = |mut j: usize| {
        while j < s.len() && s[j].is_ascii_whitespace() {
            j += 1;
        }
        j
    };
    while i < s.len() {
        if s[i] == b'#' && s.get(skip_ws(i + 1)) == Some(&b'[') {
            i = close(s, skip_ws(i + 1));
        } else if word_at(s, i, "proof") && s.get(skip_ws(i + 5)) == Some(&b'{') {
            i = close(s, skip_ws(i + 5));
        } else if let Some(w) = GHOST_CALLS.iter().find(|w| word_at(s, i, w))
            && s.get(skip_ws(i + w.len())) == Some(&b'(')
        {
            i = close(s, skip_ws(i + w.len()));
        } else if word_at(s, i, "let")
            && (word_at(s, skip_ws(i + 3), "tracked") || word_at(s, skip_ws(i + 3), "ghost"))
        {
            // To the `;` that ends the statement, outside brackets.
            let mut depth = 0i32;
            while i < s.len() && !(depth == 0 && s[i] == b';') {
                match s[i] {
                    b'(' | b'{' | b'[' => depth += 1,
                    b')' | b'}' | b']' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            i += 1;
        } else {
            out.push(s[i] as char);
            i += 1;
        }
    }
    out
}

/// The names of the calls in `code`, in source order: a lower-case
/// identifier followed by `(` (or by a turbofish, then `(`), unless it is
/// a keyword or its path goes through `Tracked` or `Ghost`.
fn call_names(code: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "if", "while", "match", "for", "return", "in", "as", "let", "fn", "move", "loop",
    ];
    let code = strip_ghost(&strip_comments(code));
    let s = code.as_bytes();
    let mut names = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if !is_ident(s[i]) || (i > 0 && is_ident(s[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < s.len() && is_ident(s[i]) {
            i += 1;
        }
        let name = &code[start..i];
        let mut j = i;
        if code[j..].starts_with("::<") {
            j = close(s, j + 2);
        }
        while j < s.len() && s[j].is_ascii_whitespace() {
            j += 1;
        }
        if s.get(j) != Some(&b'(')
            || !name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            || KEYWORDS.contains(&name)
        {
            continue;
        }
        // The path segments before the name: `a::B::name`.
        let mut k = start;
        let mut ghost = false;
        while k >= 2 && &code[k - 2..k] == "::" {
            let end = k - 2;
            k = end;
            while k > 0 && is_ident(s[k - 1]) {
                k -= 1;
            }
            ghost |= matches!(&code[k..end], "Tracked" | "Ghost");
        }
        if !ghost {
            names.push(name.into());
        }
    }
    names
}

/// The text of `theorem`'s arm whose pattern is `pat` in its `match which`.
fn arm<'a>(theorem: &'a str, pat: &str) -> &'a str {
    const MATCH: &str = "match which {";
    let s = theorem.as_bytes();
    let mut i = theorem.find(MATCH).expect("`match which {` in the theorem") + MATCH.len();
    loop {
        let rest = &theorem[i..];
        let arrow = rest.find("=>").unwrap_or_else(|| panic!("no arm `{pat}`"));
        let open = i + arrow + rest[arrow..].find('{').unwrap();
        let end = close(s, open);
        if rest[..arrow].trim() == pat {
            return &theorem[open..end];
        }
        i = end
            + theorem[end..]
                .find(|c: char| c != ',' && !c.is_whitespace())
                .unwrap();
    }
}

/// The part of `theorem` headed by a comment line that starts with
/// `` // `name` ``, up to the next such line.
fn part(theorem: &str, name: &str) -> String {
    let head = std::format!("// `{name}`");
    let mut out = String::new();
    let mut inside = false;
    for line in theorem.lines() {
        let t = line.trim();
        if t.starts_with("// `") {
            inside = t.starts_with(&head);
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(!out.is_empty(), "no part headed `{head}` in the theorem");
    out
}

/// The calls of `case`'s body, with its inlined callee's calls in place
/// of the call to it, less the skipped names.
fn body_calls(case: &Case) -> Vec<String> {
    let text = source(case.file);
    let mut calls = Vec::new();
    for name in call_names(body(&text, case.def)) {
        match case.inline {
            Some((callee, def)) if name == callee => calls.extend(call_names(body(&text, def))),
            _ => calls.push(name),
        }
    }
    calls.retain(|n| !case.skip.contains(&n.as_str()));
    calls
}

fn check(cases: &[Case], file: &str, def: &str, select: fn(&str, &str) -> String) {
    let text = source(file);
    let theorem = body(&text, def);
    let mut failures = Vec::new();
    for case in cases {
        let want = body_calls(case);
        let mut got = call_names(&select(theorem, case.part));
        got.retain(|n| !case.skip.contains(&n.as_str()));
        assert!(!want.is_empty(), "{}: no calls found", case.def);
        if got != want {
            failures.push(std::format!(
                "`{}` in src/{} makes the calls {want:?}, but its restatement `{}` in `{}` \
                 (src/{file}) makes {got:?}",
                case.def.trim_end_matches('('),
                case.file,
                case.part,
                def.trim_end_matches('('),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a restated body and its restatement differ; change both together \
         (verus/README.md, \"Restated bodies\"):\n{}",
        failures.join("\n")
    );
}

/// `theorem_no_syscall_on_fast_paths`'s arms 0, 2, 3 and `_` make the
/// calls of the fast paths they restate.
#[test]
fn fast_path_restatements_match_their_bodies() {
    check(FAST_PATH_CASES, FAST_PATHS, FAST_PATHS_DEF, |t, p| {
        arm(t, p).into()
    });
}

/// `theorem_read_only_never_aborts`'s parts for `nallocx`,
/// `challoc_features` and `mallinfo2` make the calls of those bodies.
#[test]
fn read_only_restatements_match_their_bodies() {
    check(READ_ONLY_CASES, READ_ONLY, READ_ONLY_DEF, part);
}

/// `theorem_mallctl_reads_never_abort` and
/// `theorem_mallctlbymib_reads_never_abort` make the calls of `mallctl`
/// and `mallctlbymib` after their `shared_published`.
#[test]
fn mallctl_restatements_match_their_bodies() {
    check(
        MALLCTL_CASES,
        MALLCTL,
        "fn theorem_mallctl_reads_never_abort(",
        part,
    );
    check(
        MALLCTLBYMIB_CASES,
        MALLCTL,
        "fn theorem_mallctlbymib_reads_never_abort(",
        part,
    );
}

/// The call scan keeps exec calls in order and leaves out comments,
/// attributes, ghost code, constructors and `Tracked`/`Ghost` paths.
#[test]
fn call_names_skip_ghost_code() {
    let code = r#"{
        // a(); /* b() */
        let tracked t = c();
        let ghost g = d(e());
        #[cfg(not(x()))]
        if f(Some(1)) { proof { h(); } assert(p()); }
        let x = m::Ty::k::<u8>(Tracked(&mut t), "l()").n();
        let _ = vstd::prelude::Tracked::assume_new();
        unsafe { o() }
    }"#;
    assert_eq!(call_names(code), ["f", "k", "n", "o"]);
}
