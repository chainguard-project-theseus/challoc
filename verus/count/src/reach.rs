//! The entry-point reachability gate (verus/REACH): every function a C
//! caller can reach is verified, or discharged by Kani, or named in
//! verus/REACH with the reason it is not.
//!
//! The roots are the counter's (`entry_roots`):
//! - every function of a shipped crate that exports its symbol
//!   (`no_mangle`, `export_name`): `pcre3_version` and the POSIX wrappers;
//! - every function a `const` or `static` hands out as a value;
//! - every body a `width::exports!` block exports, and the
//!   `width::FromC::from_c` conversions its `_8`, `_16` and `_32` wrappers
//!   apply: the identity one, and each one a `width::fn_ptr!` writes,
//!   which the counter expands from the macro's own definition
//!   (`Counter::expand_fn_ptrs`).
//!
//! From the roots the walk follows calls through every shipped crate by
//! name, over-approximating as the compile-inputs walk does
//! ([`crate::inputs`]): a call it resolves goes to every function it
//! could name. A call through any other function pointer, or through a
//! trait object of another crate, is not seen.
//!
//! The run fails on:
//! - a reachable function that is neither verified, discharged nor
//!   listed (a new unverified function a C caller reaches);
//! - a listed one no root reaches or that is now verified (a stale row,
//!   so the list stays the set of what is left to verify);
//! - a row without a reason;
//! - a trusted row citing no seam, or a row citing a seam ID no seams
//!   table has ([`check_seams`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::TokenTree;
use verus_syn::{Item, parse_file};

/// A row of verus/REACH: the function's file and name (`Owner::name` for
/// a method), the reason it is not verified, and whether the row is
/// marked `new` (added on purpose, [`check_added`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) file: String,
    pub(crate) name: String,
    pub(crate) reason: String,
    pub(crate) added: bool,
    pub(crate) line: usize,
}

/// The rows of `text` (verus/REACH): one function a line, as `[new] FILE
/// NAME REASON...`; blank lines and `#` comments are skipped. A row with
/// no reason is returned with an empty one, which [`check`] refuses.
pub(crate) fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(n, l)| {
            let mut words = l.split_whitespace().peekable();
            let added = words.next_if_eq(&"new").is_some();
            let file = words.next().unwrap_or("").to_string();
            let name = words.next().unwrap_or("").to_string();
            let reason = words.collect::<Vec<_>>().join(" ");
            Row {
                file,
                name,
                reason,
                added,
                line: n + 1,
            }
        })
        .collect()
}

/// With a base revision's rows: a row the base does not have fails unless
/// it is marked `new`, so a change cannot add an unverified function a C
/// caller reaches without saying so in the row itself. `list` is the
/// REACH file's path, for the report.
pub(crate) fn check_added(head: &[Row], base: &[Row], rev: &str, list: &str) -> Vec<String> {
    let had: BTreeSet<(&str, &str)> = base
        .iter()
        .map(|r| (r.file.as_str(), r.name.as_str()))
        .collect();
    head.iter()
        .filter(|r| !r.added && !had.contains(&(r.file.as_str(), r.name.as_str())))
        .map(|r| {
            format!(
                "{list}:{}: `{} {}` is not in {rev}:{list}: a C entry point reaches a function Verus does not check that it did not reach before; verify it, or mark the row `new {} {} REASON` to add it on purpose",
                r.line, r.file, r.name, r.file, r.name
            )
        })
        .collect()
}

/// Whether `tokens` invoke `mac!` at any depth: the identifier, `!` and a
/// delimited group, read as tokens, so whitespace between them
/// (`width::exports !`) does not hide an invocation, and neither a
/// mention in a comment nor a comparison (`exports != n`) is one.
fn invokes(tokens: proc_macro2::TokenStream, mac: &str) -> bool {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    (0..trees.len()).any(|i| invoked_at(&trees, i, mac))
        || trees.iter().any(|t| match t {
            TokenTree::Group(g) => invokes(g.stream(), mac),
            _ => false,
        })
}

/// Whether `trees[i..]` begin `mac ! (..)`, a group of any delimiter.
fn invoked_at(trees: &[TokenTree], i: usize, mac: &str) -> bool {
    matches!(
        trees.get(i..i + 3),
        Some([TokenTree::Ident(m), TokenTree::Punct(p), TokenTree::Group(_)])
            if names(m, mac) && p.as_char() == '!'
    )
}

/// Whether `id` names `mac`, written plain or raw (`r#exports` is the
/// identifier `exports`, but compares unequal to it).
fn names(id: &proc_macro2::Ident, mac: &str) -> bool {
    let id = id.to_string();
    id.strip_prefix("r#").unwrap_or(&id) == mac
}

/// How many times `tokens` invoke `mac!` at any depth, but inside the body
/// of `macro_rules! mac` itself (its own definition, whose invocations are
/// its expansions'), and whether they rename it (`mac as` in a `use`, an
/// import under another name, whose invocations the scan cannot see; a
/// cast such as `exports as u32` is none). `in_use` says `tokens` are a
/// group of a `use` item's tree.
fn invocations(tokens: proc_macro2::TokenStream, mac: &str, in_use: bool) -> (usize, bool) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let (mut n, mut renamed) = (0, false);
    let mut used = in_use;
    for (i, t) in trees.iter().enumerate() {
        match t {
            TokenTree::Ident(u) if u == "use" => used = true,
            TokenTree::Punct(p) if p.as_char() == ';' => used = in_use,
            TokenTree::Ident(id) if names(id, mac) => match trees.get(i + 1) {
                _ if invoked_at(&trees, i, mac) => n += 1,
                Some(TokenTree::Ident(a)) if a == "as" && used => renamed = true,
                _ => {}
            },
            TokenTree::Group(g) => {
                let own = matches!(
                    (i.checked_sub(3).map(|k| &trees[k]), i.checked_sub(1).map(|k| &trees[k])),
                    (Some(TokenTree::Ident(kw)), Some(TokenTree::Ident(name)))
                        if kw == "macro_rules" && names(name, mac)
                );
                if !own {
                    // A `use` tree's group follows `use` or `::`; any other
                    // (a body after `+ use<..>`) is no part of it.
                    let tree = in_use
                        || (used
                            && match i.checked_sub(1).map(|k| &trees[k]) {
                                Some(TokenTree::Punct(p)) => p.as_char() == ':',
                                Some(TokenTree::Ident(u)) => u == "use",
                                _ => false,
                            });
                    let (m, r) = invocations(g.stream(), mac, tree);
                    n += m;
                    renamed |= r;
                }
            }
            _ => {}
        }
    }
    (n, renamed)
}

/// How a row names a function: `Owner::name` for a method (its owner's
/// base type), else `name`.
pub(crate) fn key(owner: &str, name: &str) -> String {
    if owner.is_empty() {
        name.to_string()
    } else {
        format!("{owner}::{name}")
    }
}

/// The bodies the `mac!` blocks (pcre3's `width::exports!`) in `files`
/// export: each block's `fn BODY [..]` entries, with the file that holds
/// the block, at any depth (inside an inline `mod` or a `verus!` block
/// too). Files are parsed with `stand_ins` applied
/// ([`crate::config::stand_in`]). Fails closed: a file that cannot be
/// read or split into tokens, or one that invokes `mac!` ([`invokes`])
/// and does not parse, is an error, since its roots would otherwise drop
/// out unseen.
pub(crate) fn exported(
    root: &Path,
    files: &BTreeSet<PathBuf>,
    mac: &str,
    stand_ins: &[[String; 2]],
) -> (BTreeSet<(PathBuf, String)>, Vec<String>) {
    fn scan(
        f: &Path,
        items: &[Item],
        mac: &str,
        (out, seen): (&mut BTreeSet<(PathBuf, String)>, &mut usize),
        errs: &mut Vec<String>,
    ) {
        for item in items {
            match item {
                Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        scan(f, inner, mac, (out, seen), errs);
                    }
                }
                Item::Macro(m) if crate::verus_path(&m.mac.path) => {
                    match verus_syn::parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                        Ok(v) => scan(f, &v.items, mac, (out, seen), errs),
                        Err(e) => errs.push(format!("{}: verus! block: {e}", f.display())),
                    }
                }
                Item::Macro(m)
                    if m.mac
                        .path
                        .segments
                        .last()
                        .is_some_and(|s| names(&s.ident, mac)) =>
                {
                    *seen += 1;
                    let toks: Vec<TokenTree> = m.mac.tokens.clone().into_iter().collect();
                    for w in toks.windows(3) {
                        if let [
                            TokenTree::Ident(kw),
                            TokenTree::Ident(body),
                            TokenTree::Group(g),
                        ] = w
                            && kw == "fn"
                            && g.delimiter() == proc_macro2::Delimiter::Bracket
                        {
                            out.insert((f.to_path_buf(), body.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    let mut errs = Vec::new();
    for f in files {
        let text = match fs::read_to_string(root.join(f)) {
            Ok(t) => t,
            Err(e) => {
                errs.push(format!("{}: {e}", f.display()));
                continue;
            }
        };
        let text = crate::config::stand_in(stand_ins, &text);
        let (invoked, renamed) = match text.parse::<proc_macro2::TokenStream>() {
            Ok(t) => {
                if !invokes(t.clone(), mac) && !invocations(t.clone(), mac, false).1 {
                    continue;
                }
                invocations(t, mac, false)
            }
            Err(e) => {
                errs.push(format!("{}: {e}", f.display()));
                continue;
            }
        };
        if renamed {
            errs.push(format!(
                "{}: `{mac}` is imported under another name, whose `!` invocations the scan does not read; invoke `{mac}!` by its own name",
                f.display()
            ));
        }
        let mut seen = 0;
        match parse_file(&text) {
            Ok(file) => scan(f, &file.items, mac, (&mut out, &mut seen), &mut errs),
            Err(e) => {
                errs.push(format!("{}: {e}", f.display()));
                continue;
            }
        }
        // An invocation the scan does not reach (inside another macro's
        // tokens, a function body, an initializer) roots nothing it
        // exports: an error, not a root that drops out.
        if seen < invoked {
            errs.push(format!(
                "{}: {} of its {invoked} `{mac}!` invocations sit where the scan does not read them (inside another macro, a function body or an initializer); write each at the top of a module or `verus!` block",
                f.display(),
                invoked - seen
            ));
        }
    }
    (out, errs)
}

/// The seam IDs the tables in `texts` define: the first cell of each row
/// that starts `| S-` (verus/SEAMS.md and each theorem's seams table).
pub(crate) fn seam_ids<'a>(texts: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    texts
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|l| l.strip_prefix("| S-"))
        .filter_map(|l| l.split(" |").next())
        .map(|id| format!("S-{}", id.trim()))
        .collect()
}

/// The seam IDs a REASON cites: each word that starts `S-`, without the
/// punctuation around it.
fn cited(reason: &str) -> Vec<&str> {
    reason
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '-')))
        .filter(|w| w.starts_with("S-"))
        .collect()
}

/// Holds each row's citations to the seam tables (`seams`, [`seam_ids`]):
/// a `trusted` row cites at least one seam, since a trusted function is a
/// seam with a row (AGENTS.md, "What each seam carries"), and every ID a
/// row cites is a row of a table. `list` is the REACH file's path and
/// `tables` the seams tables', for the report.
pub(crate) fn check_seams(
    rows: &[Row],
    seams: &BTreeSet<String>,
    list: &str,
    tables: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        let ids = cited(&row.reason);
        if row.reason.starts_with("trusted") && ids.is_empty() {
            out.push(format!(
                "{list}:{}: `{} {}` is trusted but cites no seam: name the seams-table row whose contract it is",
                row.line, row.file, row.name
            ));
        }
        for id in ids.into_iter().filter(|id| !seams.contains(*id)) {
            out.push(format!(
                "{list}:{}: `{} {}` cites {id}, which no seams table ({tables}) has a row for",
                row.line, row.file, row.name
            ));
        }
    }
    out
}

/// What [`check`] found.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Entry points the walk starts from.
    pub(crate) roots: usize,
    /// Functions reachable from them.
    pub(crate) reached: usize,
    /// Of those, the verified or discharged ones.
    pub(crate) checked: usize,
    /// Of those, the ones REACH lists.
    pub(crate) listed: usize,
    /// One line per failure.
    pub(crate) failures: Vec<String>,
}

/// One reachable function, as [`check`] sees it.
pub(crate) struct Reached {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) key: String,
    /// Verified, or discharged by Kani.
    pub(crate) checked: bool,
    /// The chain of calls from a root, root first.
    pub(crate) chain: Vec<String>,
}

/// Holds the reachable functions to the rows of REACH (`list`, its path).
pub(crate) fn check(roots: usize, reached: &[Reached], rows: &[Row], list: &str) -> Report {
    let mut r = Report {
        roots,
        reached: reached.len(),
        ..Report::default()
    };
    let mut listed: BTreeMap<(&str, &str), &Row> = BTreeMap::new();
    for row in rows {
        if row.reason.is_empty() {
            r.failures.push(format!(
                "{list}:{}: `{} {}` gives no reason",
                row.line, row.file, row.name
            ));
        }
        if listed.insert((&row.file, &row.name), row).is_some() {
            r.failures.push(format!(
                "{list}:{}: `{} {}` is listed twice",
                row.line, row.file, row.name
            ));
        }
    }
    let mut unchecked: BTreeSet<(&str, &str)> = BTreeSet::new();
    for f in reached {
        let k = (f.file.as_str(), f.key.as_str());
        if f.checked {
            r.checked += 1;
            continue;
        }
        unchecked.insert(k);
        if listed.contains_key(&k) {
            r.listed += 1;
        } else {
            r.failures.push(format!(
                "{}:{}: `{}` is reachable from a C entry point ({}) and is not verified: verify it, or add `{} {} REASON` to {list}",
                f.file,
                f.line,
                f.key,
                f.chain.join(" -> "),
                f.file,
                f.key
            ));
        }
    }
    for (k, row) in &listed {
        if !unchecked.contains(k) {
            r.failures.push(format!(
                "{list}:{}: `{} {}` is verified or reachable from no C entry point: drop the row",
                row.line, row.file, row.name
            ));
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reached(file: &str, key: &str, checked: bool) -> Reached {
        Reached {
            file: file.into(),
            line: 1,
            key: key.into(),
            checked,
            chain: vec!["root".into()],
        }
    }

    #[test]
    fn rows_parse_and_skip_comments() {
        let r = rows("# a comment\n\na.rs f the reason here\nnew b.rs T::g\n");
        assert_eq!(
            r,
            vec![
                Row {
                    file: "a.rs".into(),
                    name: "f".into(),
                    reason: "the reason here".into(),
                    added: false,
                    line: 3,
                },
                Row {
                    file: "b.rs".into(),
                    name: "T::g".into(),
                    reason: String::new(),
                    added: true,
                    line: 4,
                },
            ]
        );
    }

    // The seam tables' IDs, and the rows' citations of them. A trusted row
    // citing a table's row passes, as does an outside row citing none.
    // Negative: a trusted row citing no seam, and a row citing an ID no
    // table has (an ID cut short included), each fail.
    #[test]
    fn rows_cite_seams_the_tables_have() {
        let seams = seam_ids([
            "| S-cr14 | `c_codes` | trusted |\n| not a row |",
            "| S-op1-1 | `nfa_of` | x |\n",
        ]);
        assert_eq!(seams, ["S-cr14".to_string(), "S-op1-1".to_string()].into());
        let good = rows(
            "a.rs f trusted: seam S-cr14\na.rs g trusted: seams S-cr14, S-op1-1\na.rs h outside: a Debug impl\n",
        );
        assert_eq!(
            check_seams(&good, &seams, LIST, TABLES),
            Vec::<String>::new()
        );
        let bad = rows(
            "a.rs f trusted: external_body\na.rs g trusted: seam S-op1\na.rs h outside: seam S-zz9\n",
        );
        let got = check_seams(&bad, &seams, LIST, TABLES).join("\n");
        assert!(
            got.contains("verus/REACH:2: `a.rs g` cites S-op1, which no seams table (verus/SEAMS.md, verus/theorems/*.md) has a row for"),
            "{got}"
        );
        assert!(
            got.contains("`a.rs f` is trusted but cites no seam"),
            "{got}"
        );
        assert!(
            got.contains("`a.rs g` cites S-op1, which no seams table"),
            "{got}"
        );
        assert!(
            got.contains("`a.rs h` cites S-zz9, which no seams table"),
            "{got}"
        );
        assert_eq!(check_seams(&bad, &seams, LIST, TABLES).len(), 3, "{got}");
    }

    const LIST: &str = "verus/REACH";
    const TABLES: &str = "verus/SEAMS.md, verus/theorems/*.md";
    const STAND_INS: [[&str; 2]; 1] = [["assert::", "assrtx::"]];

    fn stand_ins() -> Vec<[String; 2]> {
        STAND_INS.map(|p| p.map(String::from)).to_vec()
    }

    // A row the base has, or one marked `new`, passes; negative: an
    // unmarked row the base does not have fails.
    #[test]
    fn added_rows_need_the_new_mark() {
        let base = rows("a.rs f why\n");
        let head = rows("a.rs f why\nnew a.rs g why\na.rs h why\n");
        let got = check_added(&head, &base, "main", LIST);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].contains("`a.rs h` is not in main:verus/REACH"),
            "{got:?}"
        );
        // The configured path names the file.
        let got = check_added(&head, &base, "main", "x/REACH");
        assert!(
            got[0].starts_with("x/REACH:3: `a.rs h` is not in main:x/REACH"),
            "{got:?}"
        );
    }

    // A verified function and a listed one pass; the counts say so.
    #[test]
    fn listed_and_verified_pass() {
        let r = check(
            1,
            &[reached("a.rs", "f", true), reached("a.rs", "T::g", false)],
            &rows("a.rs T::g a reason\n"),
            LIST,
        );
        assert_eq!(r.failures, Vec::<String>::new());
        assert_eq!((r.reached, r.checked, r.listed), (2, 1, 1));
    }

    // Negative: an unlisted unverified function fails, naming its chain.
    #[test]
    fn unlisted_unverified_fails() {
        let r = check(1, &[reached("a.rs", "h", false)], &[], LIST);
        assert_eq!(r.failures.len(), 1);
        assert!(
            r.failures[0].contains("`h` is reachable"),
            "{:?}",
            r.failures
        );
        assert!(r.failures[0].contains("(root)"), "{:?}", r.failures);
    }

    // Negative: a row with no reason, a duplicate row, a row for a
    // verified function and one for an unreached function each fail.
    #[test]
    fn bad_and_stale_rows_fail() {
        let text = "a.rs f\na.rs g why\na.rs g why\na.rs v why\na.rs gone why\n";
        let r = check(
            1,
            &[
                reached("a.rs", "f", false),
                reached("a.rs", "g", false),
                reached("a.rs", "v", true),
            ],
            &rows(text),
            LIST,
        );
        let joined = r.failures.join("\n");
        assert!(joined.contains("`a.rs f` gives no reason"), "{joined}");
        assert!(joined.contains("`a.rs g` is listed twice"), "{joined}");
        assert!(
            joined.contains("`a.rs v` is verified or reachable from no"),
            "{joined}"
        );
        assert!(
            joined.contains("`a.rs gone` is verified or reachable from no"),
            "{joined}"
        );
        assert_eq!(r.failures.len(), 4, "{joined}");
    }

    #[test]
    fn exports_blocks_name_their_bodies() {
        let dir = std::env::temp_dir().join(format!("reach-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let src = "width::exports! {\n    /// doc\n    fn body_a [a_8, a_16, a_32] (x: i32) -> i32;\n    fn body_b [b_8, b_16, b_32] ();\n}\nfn body_a<U>(x: i32) -> i32 { x }\n";
        let _ = fs::write(dir.join("m.rs"), src);
        // Whitespace between the name and its `!` is still an invocation;
        // a mention in a comment is none (the file would not parse).
        let _ = fs::write(
            dir.join("w.rs"),
            "width::exports ! { fn body_c [c_8, c_16, c_32] (); }\nexports\n!{ fn body_d [d_8, d_16, d_32] (); }\n",
        );
        let _ = fs::write(dir.join("c.rs"), "// see exports! in w.rs\nfn fn fn;\n");
        let files: BTreeSet<PathBuf> = ["m.rs", "w.rs", "c.rs"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let (got, errs) = exported(&dir, &files, "exports", &stand_ins());
        // Under another macro name, the block names nothing.
        let (other, _) = exported(&dir, &files, "wrappers", &stand_ins());
        let _ = fs::remove_dir_all(&dir);
        let want: BTreeSet<(PathBuf, String)> = [
            (PathBuf::from("m.rs"), "body_a".to_string()),
            (PathBuf::from("m.rs"), "body_b".to_string()),
            (PathBuf::from("w.rs"), "body_c".to_string()),
            (PathBuf::from("w.rs"), "body_d".to_string()),
        ]
        .into();
        assert_eq!(got, want);
        assert_eq!(errs, Vec::<String>::new());
        assert!(other.is_empty(), "{other:?}");
    }

    // A block inside an inline `mod` or a `verus!` block (by any path
    // that names Verus's) is found too.
    // Negative: a file that cannot be read, or that names `exports!` and
    // does not parse, is an error, not a set of roots that drops out.
    #[test]
    fn exported_finds_nested_blocks_and_fails_closed() {
        let dir = std::env::temp_dir().join(format!("reach-n-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let nested = "mod inner {\n    width::exports! { fn in_mod [a, b, c] (); }\n}\nverus! {\n    width::exports! { fn in_verus [d, e, f] (); }\n}\n::vstd::prelude::verus! {\n    width::exports! { fn in_path [g, h, i] (); }\n}\n";
        let _ = fs::write(dir.join("n.rs"), nested);
        let _ = fs::write(
            dir.join("bad.rs"),
            "width::exports! { fn x [a, b, c] (); }\nfn (\n",
        );
        // Balanced, so it lexes and invokes `exports!`, but not Rust: the
        // parse fails.
        let _ = fs::write(
            dir.join("invalid.rs"),
            "width::exports! { fn x [a, b, c] (); }\nfn fn;\n",
        );
        // An import under another name, and an invocation inside another
        // macro's tokens: neither is read, so each is an error. The
        // macro's own definition invoking itself is none.
        let _ = fs::write(
            dir.join("renamed.rs"),
            "use width::exports as wrap;\nwrap! { fn x [a, b, c] (); }\n",
        );
        let _ = fs::write(
            dir.join("grouped.rs"),
            "use width::{exports as wrap, other};\nwrap! { fn x [a, b, c] (); }\n",
        );
        let _ = fs::write(
            dir.join("local.rs"),
            "fn f() { use width::exports as wrap; }\n",
        );
        // A cast of a binding named like the macro (`exports as u32`) is no
        // import: a file with no block is no error, and one with a block
        // has it read.
        let _ = fs::write(
            dir.join("cast.rs"),
            "fn width(exports: usize) -> u32 { exports as u32 }\n",
        );
        let _ = fs::write(
            dir.join("cast_block.rs"),
            "use std::mem;\nwidth::exports! { fn cast_body [a, b, c] (); }\nfn width(exports: usize) -> u32 { exports as u32 }\n",
        );
        let _ = fs::write(
            dir.join("wrapped.rs"),
            "macro_rules! both { ($($t:tt)*) => { width::exports! { $($t)* } } }\nboth! { fn x [a, b, c] (); }\n",
        );
        let _ = fs::write(
            dir.join("def.rs"),
            "macro_rules! exports { () => {}; ($x:ident) => { exports!(); } }\n",
        );
        // A comparison of a binding named like the macro (`exports != n`)
        // invokes nothing: the file's one block is read, with no error.
        let _ = fs::write(
            dir.join("compare.rs"),
            "width::exports! { fn cmp [a, b, c] (); }\nfn different(exports: u8, n: u8) -> bool { exports != n }\n",
        );
        // A raw identifier names the same macro: `r#exports!` at item
        // position is read, and one inside another macro is an error.
        let _ = fs::write(
            dir.join("raw.rs"),
            "width::r#exports! { fn raw_body [a, b, c] (); }\n",
        );
        let _ = fs::write(
            dir.join("raw_wrapped.rs"),
            "macro_rules! both { ($($t:tt)*) => { width::r#exports! { $($t)* } } }\nboth! { fn x [a, b, c] (); }\n",
        );
        let files: BTreeSet<PathBuf> = [
            "raw.rs",
            "raw_wrapped.rs",
            "compare.rs",
            "n.rs",
            "bad.rs",
            "invalid.rs",
            "gone.rs",
            "renamed.rs",
            "grouped.rs",
            "local.rs",
            "cast.rs",
            "cast_block.rs",
            "wrapped.rs",
            "def.rs",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let (got, errs) = exported(&dir, &files, "exports", &stand_ins());
        let _ = fs::remove_dir_all(&dir);
        let want: BTreeSet<(PathBuf, String)> = [
            (PathBuf::from("cast_block.rs"), "cast_body".to_string()),
            (PathBuf::from("compare.rs"), "cmp".to_string()),
            (PathBuf::from("n.rs"), "in_mod".to_string()),
            (PathBuf::from("n.rs"), "in_verus".to_string()),
            (PathBuf::from("n.rs"), "in_path".to_string()),
            (PathBuf::from("raw.rs"), "raw_body".to_string()),
        ]
        .into();
        assert_eq!(got, want);
        assert_eq!(errs.len(), 8, "{errs:?}");
        for file in ["grouped.rs:", "local.rs:"] {
            assert!(
                errs.iter()
                    .any(|e| e.starts_with(file) && e.contains("imported under another name")),
                "{file} {errs:?}"
            );
        }
        assert!(
            errs.iter().any(|e| e.starts_with("raw_wrapped.rs:")
                && e.contains("1 of its 1 `exports!` invocations")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.starts_with("renamed.rs:") && e.contains("imported under another name")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.starts_with("wrapped.rs:")
                    && e.contains("1 of its 1 `exports!` invocations")),
            "{errs:?}"
        );
        assert!(errs.iter().any(|e| e.starts_with("bad.rs:")), "{errs:?}");
        assert!(
            errs.iter().any(|e| e.starts_with("invalid.rs:")),
            "{errs:?}"
        );
        assert!(errs.iter().any(|e| e.starts_with("gone.rs:")), "{errs:?}");
    }
}
