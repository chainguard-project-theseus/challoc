//! verus-count: counts the executable functions of challoc (`src/`) by how
//! Verus sees them (verus/README.md, "What counts as verified").
//!
//! A function is *verified* when it sits inside `verus! { ... }`, is not
//! `#[verifier::external_body]` or `#[verifier::external]`, and its body
//! calls none of `assume`, `admit` and `Tracked::assume_new` (which mints
//! a ghost token on trust); *trusted* when it sits inside
//! `verus!` but is one of those; and *outside* when it is not inside
//! `verus!` at all. `spec` and `proof` functions are ghost code, erased
//! from ordinary builds, and are counted apart. Items under `#[cfg(test)]`
//! are left out, and so are executable functions whose own `cfg` only
//! `test` or `verus_keep_ghost` builds meet (`UNSHIPPED`). Lines run from a
//! function's `fn` keyword to its closing brace.
//!
//! Usage: `cargo run --release -- [--list] [--floor FILE [--base-rev REV]]
//! ROOT` from verus/count, with ROOT the repository root (default `../..`).
//! `--list` prints one line per executable function: crate, class, lines,
//! file:line and name. `--floor FILE` fails the run when the crate's
//! verified share of functions or of lines is below the figure FILE
//! records (verus/FLOOR), and names the figure to record when the share has
//! risen past it. `--base-rev REV` also fails a figure FILE sets below
//! `REV:verus/FLOOR`'s without a new `lowered` line for it, and fails when
//! REV does not resolve.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use verus_syn::{
    Attribute, FnMode, ImplItem, Item, Meta, Signature, TraitItem, parse_file, parse2,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class {
    Verified,
    Trusted,
    Outside,
}

#[derive(Default)]
struct Tally {
    fns: [usize; 3],
    lines: [usize; 3],
    ghost: usize,
}

struct Counter {
    krate: String,
    file: PathBuf,
    list: bool,
    tallies: BTreeMap<String, Tally>,
    /// Files and `verus!` blocks that could not be read or parsed. Their
    /// functions are missing from every count, so any of them fails the run.
    errors: usize,
}

/// The files of modules declared `#[cfg(test)] mod name;` in `file`: `name.rs`
/// and the directory `name/` (its `mod.rs` and its submodules) beside it (or
/// under its directory, for a `mod.rs` or crate root).
fn test_module_files(file: &Path, items: &[Item], out: &mut Vec<PathBuf>) {
    let dir = match file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs" | "lib.rs" | "main.rs") => file.parent().map(Path::to_path_buf),
        _ => file
            .parent()
            .map(|p| p.join(file.file_stem().unwrap_or_default())),
    };
    let Some(dir) = dir else { return };
    for item in items {
        if let Item::Mod(m) = item
            && m.content.is_none()
            && cfg_needs(&m.attrs, TEST)
        {
            let name = m.ident.to_string();
            out.push(dir.join(format!("{name}.rs")));
            out.push(dir.join(name));
        }
    }
}

/// Names whose `cfg` holds only in test builds. Verus never sees an item
/// under such a `cfg`, and no build ships it.
const TEST: &[&str] = &["test"];

/// Names a shipped build never sets: `test`, and `verus_keep_ghost`, which
/// is set only while Verus verifies. Verus sees an item that needs only
/// these, so a proof under it still counts, but no build ships it.
const UNSHIPPED: &[&str] = &["test", "verus_keep_ghost"];

/// Whether one of `attrs` is a `cfg` that holds only when one of `names`
/// is set (`cfg_predicate_needs`).
fn cfg_needs(attrs: &[Attribute], names: &[&str]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && matches!(&a.meta, Meta::List(l) if cfg_predicate_needs(&l.tokens.to_string(), names))
    })
}

/// Whether a `cfg` predicate holds only when one of `names` is set: a name
/// in `names` itself, `all(..)` with such an argument, or `any(..)` whose
/// every argument is one. `not(..)` and other names (`unix`, or a feature
/// named like `test-util`) can hold without them.
fn cfg_predicate_needs(tokens: &str, names: &[&str]) -> bool {
    let t = tokens.trim();
    if names.contains(&t) {
        return true;
    }
    if let Some(args) = cfg_args(t, "all") {
        return args.into_iter().any(|a| cfg_predicate_needs(a, names));
    }
    cfg_args(t, "any").is_some_and(|args| args.into_iter().all(|a| cfg_predicate_needs(a, names)))
}

/// The arguments of the `cfg` predicate `t` when it is `name(..)`, split at
/// its top-level commas, so a nested `all(..)` stays one argument.
fn cfg_args<'a>(t: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let inner = t
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let mut args = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(&inner[start..]);
    args.retain(|a| !a.trim().is_empty());
    Some(args)
}

fn is_external(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let text = quote_attr(a);
        text.contains("external_body")
            || text.contains("verifier :: external") && !text.contains("external_fn_specification")
    })
}

fn quote_attr(a: &Attribute) -> String {
    match &a.meta {
        Meta::Path(p) => path_string(p),
        Meta::List(l) => format!("{} ({})", path_string(&l.path), l.tokens),
        Meta::NameValue(nv) => path_string(&nv.path),
    }
}

fn path_string(p: &verus_syn::Path) -> String {
    p.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join(" :: ")
}

fn calls_assume(tokens: TokenStream) -> bool {
    let mut prev_ident: Option<String> = None;
    for tt in tokens {
        match tt {
            TokenTree::Ident(i) => prev_ident = Some(i.to_string()),
            TokenTree::Group(g) => {
                if g.delimiter() == proc_macro2::Delimiter::Parenthesis
                    && matches!(
                        prev_ident.as_deref(),
                        Some("assume") | Some("admit") | Some("assume_new")
                    )
                {
                    return true;
                }
                if calls_assume(g.stream()) {
                    return true;
                }
                prev_ident = None;
            }
            _ => prev_ident = None,
        }
    }
    false
}

fn is_ghost(sig: &Signature) -> bool {
    !matches!(sig.mode, FnMode::Exec(_) | FnMode::Default)
}

/// A proof function that is taken on trust: an `axiom fn`, or a `proof fn`
/// that is `external_body` or calls `assume`, `admit` or
/// `Tracked::assume_new`. Its `ensures` is
/// assumed wherever it is used, so it is a seam, the strongest kind.
fn is_trusted_proof(sig: &Signature, attrs: &[Attribute], body: Option<&TokenStream>) -> bool {
    match sig.mode {
        FnMode::ProofAxiom(_) => true,
        FnMode::Proof(_) => is_external(attrs) || body.is_some_and(|b| calls_assume(b.clone())),
        _ => false,
    }
}

impl Counter {
    fn record(
        &mut self,
        in_verus: bool,
        attrs: &[Attribute],
        sig: &Signature,
        body: Option<TokenStream>,
        start: usize,
        end: usize,
    ) {
        // A proof under `verus_keep_ghost` still counts: Verus uses it.
        if cfg_needs(attrs, TEST) || (!is_ghost(sig) && cfg_needs(attrs, UNSHIPPED)) {
            return;
        }
        let t = self.tallies.entry(self.krate.clone()).or_default();
        if in_verus && is_ghost(sig) && !is_trusted_proof(sig, attrs, body.as_ref()) {
            t.ghost += 1;
            return;
        }
        let class = if !in_verus {
            Class::Outside
        } else if is_trusted_proof(sig, attrs, body.as_ref())
            || is_external(attrs)
            || body.is_none_or(calls_assume)
        {
            Class::Trusted
        } else {
            Class::Verified
        };
        let lines = end.saturating_sub(start) + 1;
        t.fns[class as usize] += 1;
        t.lines[class as usize] += lines;
        if self.list {
            println!(
                "{}\t{:?}\t{}\t{}:{}\t{}",
                self.krate,
                class,
                lines,
                self.file.display(),
                start,
                sig.ident
            );
        }
    }

    fn items(&mut self, items: &[Item], in_verus: bool) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    let start = f.sig.fn_token.span.start().line;
                    let end = f.block.brace_token.span.close().end().line;
                    let body = Some(
                        f.block
                            .stmts
                            .iter()
                            .map(verus_syn::__private::ToTokens::to_token_stream)
                            .collect(),
                    );
                    self.record(in_verus, &f.attrs, &f.sig, body, start, end);
                }
                Item::Impl(i) if !cfg_needs(&i.attrs, TEST) => {
                    for ii in &i.items {
                        if let ImplItem::Fn(f) = ii {
                            let start = f.sig.fn_token.span.start().line;
                            let end = f.block.brace_token.span.close().end().line;
                            let body = Some(
                                f.block
                                    .stmts
                                    .iter()
                                    .map(verus_syn::__private::ToTokens::to_token_stream)
                                    .collect(),
                            );
                            let mut attrs = f.attrs.clone();
                            attrs.extend(
                                i.attrs
                                    .iter()
                                    .filter(|a| is_external(std::slice::from_ref(a)))
                                    .cloned(),
                            );
                            self.record(in_verus, &attrs, &f.sig, body, start, end);
                        }
                    }
                }
                Item::Trait(t) if !cfg_needs(&t.attrs, TEST) => {
                    for ti in &t.items {
                        if let TraitItem::Fn(f) = ti
                            && let Some(block) = &f.default
                        {
                            let start = f.sig.fn_token.span.start().line;
                            let end = block.brace_token.span.close().end().line;
                            let body = Some(
                                block
                                    .stmts
                                    .iter()
                                    .map(verus_syn::__private::ToTokens::to_token_stream)
                                    .collect(),
                            );
                            self.record(in_verus, &f.attrs, &f.sig, body, start, end);
                        }
                    }
                }
                Item::Mod(m) if !cfg_needs(&m.attrs, TEST) => {
                    if let Some((_, inner)) = &m.content {
                        self.items(inner, in_verus);
                    }
                }
                Item::Macro(m) if m.mac.path.is_ident("verus") => {
                    match parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                        Ok(f) => self.items(&f.items, true),
                        Err(e) => {
                            eprintln!("{}: verus! block: {e}", self.file.display());
                            self.errors += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// A share to a tenth of a percent, rounded down as the floor rounds it.
fn pct(a: usize, b: usize) -> String {
    if b == 0 {
        "-".into()
    } else {
        format!("{}%", shown(tenths(a, b)))
    }
}

fn shown(t: u32) -> String {
    format!("{}.{}", t / 10, t % 10)
}

fn main() {
    let mut list = false;
    let mut floor = None;
    let mut base_rev = None;
    let mut root = PathBuf::from("../..");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--list" {
            list = true;
        } else if a == "--floor" || a == "--base-rev" {
            let Some(v) = args.next() else {
                eprintln!("{a} needs a value");
                std::process::exit(2);
            };
            if a == "--floor" {
                floor = Some(PathBuf::from(v));
            } else {
                base_rev = Some(v);
            }
        } else {
            root = PathBuf::from(a);
        }
    }
    let mut c = Counter {
        krate: "challoc".into(),
        file: PathBuf::new(),
        list,
        tallies: BTreeMap::new(),
        errors: 0,
    };
    let src = root.join("src");
    if !src.is_dir() {
        eprintln!("no src/ under {}", root.display());
        std::process::exit(2);
    }
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    // `tracked` is a Verus keyword; challoc's tests bind a variable of that
    // name (api/mallctl.rs). A same-length stand-in keeps the line numbers,
    // and Verus code never writes `let tracked =`.
    let stand_in = |text: String| text.replace("let tracked =", "let trackd_ =");
    let mut skip = Vec::new();
    for f in &files {
        if let Ok(text) = fs::read_to_string(f)
            && let Ok(file) = parse_file(&stand_in(text))
        {
            test_module_files(f, &file.items, &mut skip);
        }
    }
    for f in files {
        if skip.iter().any(|s| f.starts_with(s)) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&f) else {
            eprintln!("{}: unreadable", f.display());
            c.errors += 1;
            continue;
        };
        c.file = f.strip_prefix(&root).unwrap_or(&f).to_path_buf();
        let text = stand_in(text);
        match parse_file(&text) {
            Ok(file) => c.items(&file.items, false),
            Err(e) => {
                eprintln!("{}:{}: {e}", c.file.display(), e.span().start().line);
                c.errors += 1;
            }
        }
    }
    if list {
        fail_on_errors(c.errors);
        return;
    }
    println!(
        "crate\tverified\ttrusted\toutside\tverified%\tlines verified\tlines trusted\tlines outside\tlines verified%\tghost"
    );
    let t = c.tallies.remove(&c.krate).unwrap_or_default();
    print_row(&c.krate, &t);
    fail_on_errors(c.errors);
    if let Some(f) = floor {
        let head = read_floor(
            &fs::read_to_string(&f).unwrap_or_else(|e| {
                eprintln!("{}: {e}", f.display());
                std::process::exit(2);
            }),
            &f.display().to_string(),
        );
        let got = Floor {
            functions: tenths(t.fns[0], t.fns.iter().sum()),
            lines: tenths(t.lines[0], t.lines.iter().sum()),
        };
        let (mut ok, report) = check_floor(&got, &head.floor);
        print!("{report}");
        if let Some(rev) = base_rev {
            match base_floor(&root, &rev) {
                Ok(Some(text)) => {
                    let base = read_floor(&text, &format!("{rev}:verus/FLOOR"));
                    let (lowering_ok, report) = check_lowering(&head, &base);
                    print!("{report}");
                    ok &= lowering_ok;
                }
                Ok(None) => println!("ok coverage floor: {rev} has no verus/FLOOR to compare with"),
                Err(e) => {
                    eprintln!("FAIL coverage floor: {e}");
                    std::process::exit(2);
                }
            }
        }
        if !ok {
            std::process::exit(1);
        }
    }
}

fn print_row(name: &str, t: &Tally) {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    println!(
        "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        t.fns[0],
        t.fns[1],
        t.fns[2],
        pct(t.fns[0], fns),
        t.lines[0],
        t.lines[1],
        t.lines[2],
        pct(t.lines[0], lines),
        t.ghost
    );
}

/// `REV:verus/FLOOR` under `root`'s repository, failing closed: a
/// revision that does not resolve to a commit (a shallow clone, another
/// uid's checkout git refuses, an empty name) is an error, and only a
/// resolved commit without the file is `None`.
fn base_floor(root: &Path, rev: &str) -> Result<Option<String>, String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "safe.directory=*"])
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let commit = git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{rev}^{{commit}}"),
    ])?;
    if !commit.status.success() {
        return Err(format!(
            "base revision {rev:?} does not resolve to a commit, so verus/FLOOR cannot be compared with it"
        ));
    }
    let commit = String::from_utf8_lossy(&commit.stdout).trim().to_owned();
    let path = format!("{commit}:verus/FLOOR");
    if !git(&["cat-file", "-e", &path])?.status.success() {
        return Ok(None);
    }
    let shown = git(&["show", &path])?;
    if !shown.status.success() {
        return Err(format!(
            "git show {path}: {}",
            String::from_utf8_lossy(&shown.stderr).trim()
        ));
    }
    Ok(Some(String::from_utf8_lossy(&shown.stdout).into_owned()))
}

fn read_floor(text: &str, name: &str) -> FloorFile {
    parse_floor(text).unwrap_or_else(|e| {
        eprintln!("{name}: {e}");
        std::process::exit(2);
    })
}

/// A verified share in tenths of a percent, rounded down: 84 is 8.4%.
#[derive(Debug, PartialEq, Eq)]
struct Floor {
    functions: u32,
    lines: u32,
}

fn tenths(a: usize, b: usize) -> u32 {
    (a * 1000)
        .checked_div(b)
        .map_or(0, |t| u32::try_from(t).unwrap_or(u32::MAX))
}

/// verus/FLOOR: the two figures, and the `lowered` lines that give a
/// reason for each figure set below the base revision's.
#[derive(Debug, PartialEq, Eq)]
struct FloorFile {
    floor: Floor,
    lowered: Vec<(String, u32)>,
}

/// Reads verus/FLOOR: `#` comments, one `functions P` and one `lines P`
/// line (P a percentage with at most one decimal), and any number of
/// `lowered functions|lines P REASON` lines.
fn parse_floor(text: &str) -> Result<FloorFile, String> {
    let mut functions = None;
    let mut lines = None;
    let mut lowered = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let n = i + 1;
        let figure = |key: &str, value: &str| -> Result<u32, String> {
            if key != "functions" && key != "lines" {
                return Err(format!("line {n}: unknown figure {key:?}"));
            }
            parse_tenths(value).ok_or_else(|| format!("line {n}: {value:?} is not P or P.D"))
        };
        match words.as_slice() {
            ["lowered", key, value, reason @ ..] => {
                if reason.is_empty() {
                    return Err(format!("line {n}: a lowered line needs a reason"));
                }
                lowered.push(((*key).to_owned(), figure(key, value)?));
            }
            [key, value] => {
                let v = figure(key, value)?;
                let slot = if *key == "functions" {
                    &mut functions
                } else {
                    &mut lines
                };
                if slot.replace(v).is_some() {
                    return Err(format!("line {n}: {key} given twice"));
                }
            }
            _ => {
                return Err(format!(
                    "line {n}: want `functions P`, `lines P` or `lowered FIGURE P REASON`"
                ));
            }
        }
    }
    match (functions, lines) {
        (Some(functions), Some(lines)) => Ok(FloorFile {
            floor: Floor { functions, lines },
            lowered,
        }),
        _ => Err("want both a functions and a lines figure".into()),
    }
}

/// Whether each figure in `head` is at least the base revision's, or is
/// set lower with a `lowered FIGURE P REASON` line naming that figure and
/// value that the base's file does not already have, so lowering the floor
/// is never silent and an old reason never covers a new drop.
fn check_lowering(head: &FloorFile, base: &FloorFile) -> (bool, String) {
    let mut ok = true;
    let mut report = String::new();
    for (name, h, b) in [
        ("functions", head.floor.functions, base.floor.functions),
        ("lines", head.floor.lines, base.floor.lines),
    ] {
        if h >= b {
            continue;
        }
        let explained = head.lowered.iter().any(|(k, v)| {
            k == name && *v == h && !base.lowered.iter().any(|(bk, bv)| bk == k && bv == v)
        });
        if explained {
            report += &format!(
                "ok coverage floor: {name} lowered from {}% to {}%, with a reason\n",
                shown(b),
                shown(h)
            );
        } else {
            ok = false;
            report += &format!(
                "FAIL coverage floor: verus/FLOOR lowers {name} from {}% to {}% with no `lowered {name} {} <reason>` line\n",
                shown(b),
                shown(h),
                shown(h)
            );
        }
    }
    (ok, report)
}

/// `P` or `P.D` as tenths of a percent, at most 100.0.
fn parse_tenths(s: &str) -> Option<u32> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, "0"));
    if whole.is_empty()
        || frac.len() != 1
        || !whole
            .bytes()
            .chain(frac.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let v = whole
        .parse::<u32>()
        .ok()?
        .checked_mul(10)?
        .checked_add(frac.parse().ok()?)?;
    (v <= 1000).then_some(v)
}

/// Whether `got` meets `want`, with one line per figure saying so. A
/// figure above its floor names the value to record, so the floor only
/// rises.
fn check_floor(got: &Floor, want: &Floor) -> (bool, String) {
    let mut ok = true;
    let mut report = String::new();
    for (name, g, w) in [
        ("functions", got.functions, want.functions),
        ("lines", got.lines, want.lines),
    ] {
        if g < w {
            ok = false;
            report += &format!(
                "FAIL coverage floor: {name} verified {}%, below the floor's {}%\n",
                shown(g),
                shown(w)
            );
        } else if g > w {
            report += &format!(
                "ok coverage floor: {name} verified {}%, above the floor's {}%; raise verus/FLOOR to `{name} {}`\n",
                shown(g),
                shown(w),
                shown(g)
            );
        } else {
            report += &format!(
                "ok coverage floor: {name} verified {}%, at the floor\n",
                shown(g)
            );
        }
    }
    (ok, report)
}

fn fail_on_errors(errors: usize) {
    if errors > 0 {
        eprintln!("{errors} files or verus! blocks did not parse; the counts leave them out");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tally of one crate whose only file is `src`.
    fn count(src: &str) -> (Tally, usize) {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            list: false,
            tallies: BTreeMap::new(),
            errors: 0,
        };
        c.items(&parse_file(src).expect("parses").items, false);
        (c.tallies.remove("k").unwrap_or_default(), c.errors)
    }

    #[test]
    fn cfg_predicates_that_hold_only_under_test_or_verification() {
        // (predicate, holds only under `test`, holds only under `test` or
        // `verus_keep_ghost`)
        for (cfg, test, unshipped) in [
            ("test", true, true),
            ("all (test , feature = \"x\")", true, true),
            ("all (feature = \"x\" , test)", true, true),
            ("not (test)", false, false),
            ("any (test , feature = \"x\")", false, false),
            ("feature = \"test-util\"", false, false),
            ("all (not (test) , unix)", false, false),
            ("verus_keep_ghost", false, true),
            ("not (verus_keep_ghost)", false, false),
            ("all (verus_keep_ghost , not (test))", false, true),
            ("any (test , verus_keep_ghost)", false, true),
            ("any (all (test , unix) , verus_keep_ghost)", false, true),
            ("any (test , unix)", false, false),
            ("any (verus_keep_ghost , feature = \"x\")", false, false),
            ("any (not (test) , verus_keep_ghost)", false, false),
        ] {
            for (names, want) in [(TEST, test), (UNSHIPPED, unshipped)] {
                assert_eq!(
                    cfg_predicate_needs(cfg, names),
                    want,
                    "{cfg} needs one of {names:?}: got = {}, wanted = {want}",
                    !want
                );
            }
        }
    }

    #[test]
    fn functions_are_classed_as_verus_sees_them() {
        let (t, errors) = count(
            r#"
            verus! {
                fn verified() {}
                #[verifier::external_body]
                fn external_body() {}
                #[verifier::external]
                fn external() {}
                fn assumes() { assume(false); }
                fn admits() { if true { admit(); } }
                spec fn ghost() -> bool { true }
                proof fn lemma() {}
                axiom fn axiom() ensures true;
                #[verifier::external_body]
                proof fn external_lemma() {}
                proof fn admitted() { admit(); }
                fn mints() -> Tracked<u8> { Tracked::assume_new() }
                proof fn mints_in_proof() { let tracked t: u8 = Tracked::<u8>::assume_new().get(); }
                #[cfg(test)]
                fn test_only() {}
                #[cfg(any(test, verus_keep_ghost))]
                fn verification_only() {}
                #[cfg(verus_keep_ghost)]
                fn verification_build_only() {}
                #[cfg(verus_keep_ghost)]
                axiom fn verification_axiom() ensures true;
                #[cfg(verus_keep_ghost)]
                spec fn verification_spec() -> bool { true }
            }
            #[cfg(verus_keep_ghost)]
            mod lemmas { verus! { axiom fn lemma_axiom() ensures true; } }
            fn outside() {}
            #[cfg(not(test))]
            fn shipped_outside() {}
            #[cfg(test)]
            fn test_only_outside() {}
            #[cfg(test)]
            mod tests { fn helper() {} }
            "#,
        );
        assert_eq!(errors, 0, "errors: got = {errors}, wanted = 0");
        assert_eq!(
            t.fns,
            [1, 11, 2],
            "verified, trusted, outside: got = {:?}, wanted = [1, 11, 2]",
            t.fns
        );
        assert_eq!(t.ghost, 3, "ghost: got = {}, wanted = 3", t.ghost);
    }

    #[test]
    fn the_floor_file_parses_and_rejects_what_it_should() {
        let got = parse_floor(
            "# c\nfunctions 8.4 # now\n\nlines 10\nlowered lines 10 the bulk test helpers moved in\n",
        );
        assert_eq!(
            got,
            Ok(FloorFile {
                floor: Floor {
                    functions: 84,
                    lines: 100
                },
                lowered: vec![("lines".into(), 100)],
            }),
            "floor: got = {got:?}, wanted = 8.4 and 10.0, lines lowered to 10.0"
        );
        for bad in [
            "functions 8.4\nlines 1\nlowered lines 1\n",
            "functions 8.4\nlines 1\nlowered branches 1 why\n",
            "functions 8.4\n",
            "functions 8.4\nlines 1.25\n",
            "functions 8.4\nlines -1\n",
            "functions 8.4\nlines 100.1\n",
            "functions 8.4\nfunctions 8.5\nlines 1\n",
            "functions 8.4\nlines 1\nbranches 3\n",
            "functions 8.4 9\nlines 1\n",
            "functions .5\nlines 1\n",
        ] {
            assert!(parse_floor(bad).is_err(), "{bad:?}: got = Ok, wanted = Err");
        }
    }

    #[test]
    fn lowering_the_floor_needs_a_reason_for_that_figure() {
        let head = |text: &str| parse_floor(text).expect("parses");
        let base = head("functions 13.7\nlines 13.1\nlowered functions 13.4 an old reason\n");
        let cases = [
            (
                "functions 13.4\nlines 13.1\nlowered functions 13.4 an old reason\n",
                false,
            ),
            ("functions 13.7\nlines 13.1\n", true),
            ("functions 14.0\nlines 13.1\n", true),
            ("functions 13.6\nlines 13.1\n", false),
            (
                "functions 13.6\nlines 13.1\nlowered functions 13.6 x moved out\n",
                true,
            ),
            (
                "functions 13.6\nlines 13.1\nlowered functions 13.5 stale\n",
                false,
            ),
            (
                "functions 13.6\nlines 13.1\nlowered lines 13.6 wrong figure\n",
                false,
            ),
        ];
        for (text, want) in cases {
            let (ok, report) = check_lowering(&head(text), &base);
            assert_eq!(ok, want, "{text:?}: got = {ok}, wanted = {want}\n{report}");
        }
    }

    #[test]
    fn the_base_floor_fails_closed() {
        let dir = std::env::temp_dir().join(format!("verus-count-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("verus")).expect("mkdir");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?}: got = failure, wanted = success");
        };
        git(&["init", "-q"]);
        fs::write(dir.join("a"), "a").expect("write");
        git(&["add", "a"]);
        git(&["commit", "-q", "-m", "no floor"]);
        fs::write(dir.join("verus/FLOOR"), "functions 1.0\nlines 2.0\n").expect("write");
        git(&["add", "verus/FLOOR"]);
        git(&["commit", "-q", "-m", "floor"]);
        let got = base_floor(&dir, "HEAD");
        assert_eq!(
            got,
            Ok(Some("functions 1.0\nlines 2.0\n".into())),
            "HEAD: got = {got:?}, wanted = its FLOOR"
        );
        let got = base_floor(&dir, "HEAD^");
        assert_eq!(got, Ok(None), "HEAD^: got = {got:?}, wanted = no FLOOR");
        for rev in ["HEAD~5", "", "no-such-branch"] {
            let got = base_floor(&dir, rev);
            assert!(got.is_err(), "{rev:?}: got = {got:?}, wanted = an error");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_floor_fails_below_and_names_a_rise() {
        let want = Floor {
            functions: 84,
            lines: 105,
        };
        let (ok, report) = check_floor(
            &Floor {
                functions: 84,
                lines: 105,
            },
            &want,
        );
        assert!(ok, "at the floor: got = fail, wanted = ok\n{report}");
        let (ok, report) = check_floor(
            &Floor {
                functions: 83,
                lines: 120,
            },
            &want,
        );
        assert!(!ok, "below on functions: got = ok, wanted = fail\n{report}");
        assert!(
            report.contains("FAIL coverage floor: functions verified 8.3%, below the floor's 8.4%"),
            "report: got = {report:?}"
        );
        assert!(
            report.contains("raise verus/FLOOR to `lines 12.0`"),
            "report: got = {report:?}"
        );
        assert_eq!(
            tenths(1, 3),
            333,
            "tenths(1, 3): got = {}, wanted = 333",
            tenths(1, 3)
        );
        assert_eq!(
            tenths(0, 0),
            0,
            "tenths(0, 0): got = {}, wanted = 0",
            tenths(0, 0)
        );
    }

    #[test]
    fn a_test_module_skips_its_file_and_its_directory() {
        let file = parse_file("#[cfg(test)] mod model; #[cfg(all(test, target_os = \"linux\"))] mod linux_tests; mod kept;")
            .expect("parses");
        let mut skip = Vec::new();
        test_module_files(Path::new("src/lib.rs"), &file.items, &mut skip);
        for (f, want) in [
            ("src/model/mod.rs", true),
            ("src/model/steps.rs", true),
            ("src/model.rs", true),
            ("src/linux_tests.rs", true),
            ("src/kept.rs", false),
            ("src/kept/model.rs", false),
            ("src/models.rs", false),
        ] {
            let got = skip.iter().any(|s| Path::new(f).starts_with(s));
            assert_eq!(got, want, "{f}: got = {got}, wanted = {want}");
        }
    }

    #[test]
    fn an_unparsable_verus_block_is_an_error() {
        let (t, errors) = count("verus! { fn 1() {} }");
        assert_eq!(errors, 1, "errors: got = {errors}, wanted = 1");
        assert_eq!(
            t.fns,
            [0, 0, 0],
            "fns: got = {:?}, wanted = [0, 0, 0]",
            t.fns
        );
    }
}
