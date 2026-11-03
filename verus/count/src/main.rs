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
//! or `#[cfg(kani)]` (the Kani harnesses) are left out, and so are
//! executable functions whose own `cfg` only `test`, `kani` or
//! `verus_keep_ghost` builds meet (`UNSHIPPED`). Lines run from a
//! function's `fn` keyword to its closing brace.
//!
//! A trusted function is also classed by the status of its seam in the
//! ledger (verus/ledger, verus/SEAMS.md, "Status"), matched by the
//! `src/FILE:LINE` of its `fn` keyword: *discharged* when the status is
//! "discharged" (a check outside Verus covers its whole contract) or
//! "discharged on premises (Isla: ...; premises: ...)" (an Isla check
//! covers it but for the platform premises named by the P and K labels
//! verus/isla/seams.md defines, which only Isla's checks may cite),
//! *partly checked* when it is "trusted (partly checked, ...)", and
//! unchecked otherwise. The *combined* share counts verified and
//! discharged functions; partly checked ones are shown apart and not
//! counted. A trusted function with no ledger entry is a warning; a
//! discharged function seam (`external_body`, `external_body proof`,
//! `axiom`) that matches no trusted function, or a seam of a kind this
//! counter does not know, fails the run.
//!
//! Usage: `cargo run --release -- [--list] [--floor FILE [--base-rev REV]]
//! ROOT` from verus/count, with ROOT the repository root (default `../..`).
//! `--list` prints one line per executable function: crate, class, lines,
//! file:line and name. `--floor FILE` fails the run when the crate's
//! verified or combined share of functions or of lines is below the
//! figure FILE records (verus/FLOOR), and names the figure to record when
//! the share has risen past it. `--base-rev REV` also fails a figure FILE
//! sets below `REV:verus/FLOOR`'s without a new `lowered` line for it, and
//! fails when REV does not resolve.

use std::collections::{BTreeMap, BTreeSet};
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
    /// The `src/FILE:LINE` of each trusted function and its lines, to match
    /// against the seam ledger.
    trusted: Vec<(String, usize)>,
    /// Files and `verus!` blocks that could not be read or parsed. Their
    /// functions are missing from every count, so any of them fails the run.
    errors: usize,
}

/// The files of modules declared `#[cfg(test)] mod name;` (or under another
/// [`TEST`] name) in `file`: `name.rs` and the directory `name/` (its `mod.rs`
/// and its submodules) beside it (or under its directory, for a `mod.rs` or
/// crate root); for a module with a `#[path = "p"]` attribute, `p` beside
/// `file`, and the directory of `p` when `p` is a `mod.rs`.
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
            let path = m.attrs.iter().find_map(|a| match &a.meta {
                Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
                    verus_syn::Expr::Lit(l) => match &l.lit {
                        verus_syn::Lit::Str(s) => Some(s.value()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
            match path {
                Some(p) => {
                    let p = file.parent().unwrap_or(Path::new("")).join(p);
                    if p.file_name().is_some_and(|n| n == "mod.rs")
                        && let Some(d) = p.parent()
                    {
                        out.push(d.to_path_buf());
                    }
                    out.push(p);
                }
                None => {
                    let name = m.ident.to_string();
                    out.push(dir.join(format!("{name}.rs")));
                    out.push(dir.join(name));
                }
            }
        }
    }
}

/// Names whose `cfg` holds only in test builds, or only in the builds Kani
/// makes of its harnesses (`kani`). Verus never sees an item under such a
/// `cfg`, and no build ships it.
const TEST: &[&str] = &["test", "kani"];

/// Names a shipped build never sets: the [`TEST`] names, and
/// `verus_keep_ghost`, which is set only while Verus verifies. Verus sees
/// an item that needs only these, so a proof under it still counts, but
/// no build ships it.
const UNSHIPPED: &[&str] = &["test", "kani", "verus_keep_ghost"];

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
        if class == Class::Trusted {
            self.trusted
                .push((format!("{}:{}", self.file.display(), start), lines));
        }
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

/// The files under `dir` whose names end in `suffix`, in path order.
fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, suffix, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(suffix))
        {
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
        trusted: Vec::new(),
        errors: 0,
    };
    let src = root.join("src");
    if !src.is_dir() {
        eprintln!("no src/ under {}", root.display());
        std::process::exit(2);
    }
    let mut files = Vec::new();
    walk(&src, ".rs", &mut files);
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
    let isla = root.join("verus/isla/seams.md");
    let premises = premise_labels(&fs::read_to_string(&isla).unwrap_or_else(|e| {
        eprintln!("{}: {e}", isla.display());
        std::process::exit(2);
    }));
    if premises.is_empty() {
        eprintln!("{}: no platform premise labels", isla.display());
        std::process::exit(2);
    }
    let seams = read_ledger(&root.join("verus/ledger"), &premises).unwrap_or_else(|e| {
        eprintln!("verus/ledger: {e}");
        std::process::exit(2);
    });
    let (checked, warnings, errors) = match_ledger(&c.trusted, &seams);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "crate\tverified+discharged\tdischarged\tpartly checked\tverified+discharged%\tlines verified+discharged\tlines discharged\tlines partly checked\tlines verified+discharged%"
    );
    print_combined_row(&c.krate, &t, &checked);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("FAIL ledger: {e}");
        }
        std::process::exit(1);
    }
    if let Some(f) = floor {
        let head = read_floor(
            &fs::read_to_string(&f).unwrap_or_else(|e| {
                eprintln!("{}: {e}", f.display());
                std::process::exit(2);
            }),
            &f.display().to_string(),
        );
        let got = shares(&t, &checked);
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

/// The four [`FIGURES`] of a tally and its discharged functions.
fn shares(t: &Tally, c: &Checked) -> Floor {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    Floor([
        tenths(t.fns[0], fns),
        tenths(t.lines[0], lines),
        tenths(t.fns[0] + c.discharged.0, fns),
        tenths(t.lines[0] + c.discharged.1, lines),
    ])
}

fn print_combined_row(name: &str, t: &Tally, c: &Checked) {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    let (vf, vl) = (t.fns[0] + c.discharged.0, t.lines[0] + c.discharged.1);
    println!(
        "{name}\t{vf}\t{}\t{}\t{}\t{vl}\t{}\t{}\t{}",
        c.discharged.0,
        c.partly.0,
        pct(vf, fns),
        c.discharged.1,
        c.partly.1,
        pct(vl, lines),
    );
}

/// How far a seam's ledger status says a check outside Verus covers it
/// (verus/SEAMS.md, "Status").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    /// `discharged (...)`: the check covers the whole contract; or
    /// [`PREMISED`]: Isla's check covers it but for the premises the
    /// status names.
    Discharged,
    /// `trusted (partly checked, ...)`.
    Partly,
    /// `trusted`, or any other status.
    Unchecked,
}

/// One ledger entry: its `src/FILE:LINE`, its kind and its status.
#[derive(Debug, PartialEq, Eq)]
struct Seam {
    loc: String,
    kind: String,
    status: Status,
}

/// The seam kinds `seams.sh` places at a function's `fn` line, and that
/// the counter classes as trusted.
const FN_KINDS: &[&str] = &["external_body", "external_body proof", "axiom"];

/// The other kinds `seams.sh` writes: items that are not functions, or
/// that sit elsewhere than a trusted function's `fn` line (a
/// `no_decreases` function counts as verified; an `assume_new` seam is the
/// line of the call, inside a function that counts as trusted).
const OTHER_KINDS: &[&str] = &[
    "external_body type",
    "external_body const",
    "assume_spec",
    "ext_type_spec",
    "ext_trait_spec",
    "spec_impl",
    "no_decreases",
    "assume_new",
    "ghost_assume_new",
];

/// The seams of one ledger file: each entry's `src/FILE:LINE. Kind: K.
/// Category: ...` line and the `Status: S` that follows it in the same
/// paragraph (`S` may run on over the paragraph's next lines). An entry
/// without a status is an error, and so is a [`PREMISED`] status that
/// cites a check other than Isla's alone or names anything but labels in
/// `premises` (or none), and a status starting `discharged` in any other
/// form than `discharged (`.
fn parse_ledger(text: &str, premises: &BTreeSet<String>) -> Result<Vec<Seam>, String> {
    let mut seams = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((i, line)) = lines.next() {
        let Some((loc, rest)) = line.split_once(". Kind: ") else {
            continue;
        };
        if !loc.starts_with("src/")
            || !loc
                .rsplit_once(':')
                .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let kind = rest
            .split_once(". Category:")
            .map_or(rest.trim_end_matches('.'), |(k, _)| k)
            .to_owned();
        // The status is in the rest of the entry's paragraph.
        let mut para = rest.to_owned();
        while let Some((_, next)) = lines.peek()
            && !next.trim().is_empty()
        {
            para.push(' ');
            para.push_str(next.trim());
            lines.next();
        }
        let Some((_, status)) = para.split_once("Status: ") else {
            return Err(format!("line {}: {loc} has no status", i + 1));
        };
        let status = if let Some(rest) = status.strip_prefix(PREMISED) {
            premised(rest, premises).map_err(|e| format!("line {}: {loc}: {e}", i + 1))?
        } else if status.starts_with("discharged (") {
            Status::Discharged
        } else if status.starts_with("discharged") {
            return Err(format!(
                "line {}: {loc}: a discharged status not of the form `discharged (...)` or `{PREMISED}...)`",
                i + 1
            ));
        } else if status.starts_with("trusted (partly checked") {
            Status::Partly
        } else {
            Status::Unchecked
        };
        seams.push(Seam {
            loc: loc.to_owned(),
            kind,
            status,
        });
    }
    Ok(seams)
}

/// The status of an Isla check whose contract rests on platform premises
/// it names (verus/SEAMS.md, "Status"): `discharged on premises (Isla:
/// verus/isla/seams.md; premises: ...)`.
const PREMISED: &str = "discharged on premises (";

/// The class of a [`PREMISED`] status, given the text after it: discharged
/// when it cites the Isla runs alone (`Isla: verus/isla/seams.md`) and
/// names at least one premise, each the label of a platform premise in
/// `premises` ([`platform_labels`]), an error otherwise, since only Isla's
/// checks count with named premises, and only with platform ones.
fn premised(rest: &str, premises: &BTreeSet<String>) -> Result<Status, String> {
    let Some(p) = rest.strip_prefix("Isla: verus/isla/seams.md; premises: ") else {
        return Err(format!(
            "{PREMISED}...) cites a check other than Isla's alone, or names no premise: {rest:?}"
        ));
    };
    if !platform_labels(p, premises) {
        return Err(format!(
            "{PREMISED}...) names no premise, or one that is not the label of a platform premise verus/isla/seams.md defines: {rest:?}"
        ));
    }
    Ok(Status::Discharged)
}

/// Whether a premise list (`K2 (gloss) and K3 (gloss)`, then the `)` that
/// closes the status and only `.` and whitespace after it) names at least
/// one premise and only labels in `premises` (verus/isla/seams.md's
/// platform premises, [`premise_labels`]), each optionally followed by a
/// gloss in parentheses. A premise of Verus's own model (`the TLS slot
/// premise`, `TlsCell::tp`) is no such label, nor is one that file does
/// not define (`K4`), nor text after the closing `)` or inside a gloss
/// that never closes.
fn platform_labels(list: &str, premises: &BTreeSet<String>) -> bool {
    let mut bare = String::new();
    let mut depth = 0usize;
    let mut tail = None;
    for (i, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                tail = Some(&list[i + 1..]);
                break;
            }
            ')' => depth -= 1,
            _ if depth == 0 => bare.push(c),
            _ => {}
        }
    }
    let Some(tail) = tail else {
        return false;
    };
    if !tail.chars().all(|c| c == '.' || c.is_whitespace()) {
        return false;
    }
    let labels: Vec<&str> = bare
        .split(", ")
        .flat_map(|s| s.split(" and "))
        .map(str::trim)
        .collect();
    labels.iter().all(|l| premises.contains(*l))
}

/// The labels of the platform premises verus/isla/seams.md defines: the
/// `P` or `K` and a number that open each `- **LABEL, title.**` bullet
/// of its "Platform premises" section.
fn premise_labels(seams_md: &str) -> BTreeSet<String> {
    seams_md
        .lines()
        .skip_while(|l| *l != "## Platform premises")
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("- **")?.split_once(',').map(|(p, _)| p))
        .filter(|p| {
            p.strip_prefix(['P', 'K'])
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
        .collect()
}

/// Every seam in the `*.rs.md` files under `dir` (verus/ledger), with
/// `premises` the platform premise labels a [`PREMISED`] status may name.
fn read_ledger(dir: &Path, premises: &BTreeSet<String>) -> Result<Vec<Seam>, String> {
    let mut files = Vec::new();
    walk(dir, ".rs.md", &mut files);
    if files.is_empty() {
        return Err(format!("no *.rs.md files under {}", dir.display()));
    }
    let mut seams = Vec::new();
    for f in files {
        let text = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        seams.extend(parse_ledger(&text, premises).map_err(|e| format!("{}: {e}", f.display()))?);
    }
    Ok(seams)
}

/// Functions and lines of the trusted functions whose seams are
/// discharged, and of those partly checked.
#[derive(Debug, Default, PartialEq, Eq)]
struct Checked {
    discharged: (usize, usize),
    partly: (usize, usize),
}

/// Classes each trusted function (`src/FILE:LINE`, lines) by the status of
/// the function seam (`FN_KINDS`) at its location. Returns the tally,
/// warnings (a trusted function with no seam) and errors (a seam of an
/// unknown kind, two function seams on one line, or a discharged function
/// seam that matches no trusted function, whose
/// coverage would otherwise drop out of the combined share unnoticed).
fn match_ledger(
    trusted: &[(String, usize)],
    seams: &[Seam],
) -> (Checked, Vec<String>, Vec<String>) {
    let mut checked = Checked::default();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    // Only a function seam classes a function: a seam of another kind on
    // the same line (an `assume_new` in a one-line function) does not.
    let mut by_loc: BTreeMap<&str, &Seam> = BTreeMap::new();
    for s in seams.iter().filter(|s| FN_KINDS.contains(&s.kind.as_str())) {
        if by_loc.insert(s.loc.as_str(), s).is_some() {
            errors.push(format!("{}: two function seams on one line", s.loc));
        }
    }
    for (loc, lines) in trusted {
        let slot = match by_loc.get(loc.as_str()).map(|s| s.status) {
            Some(Status::Discharged) => &mut checked.discharged,
            Some(Status::Partly) => &mut checked.partly,
            Some(Status::Unchecked) => continue,
            None => {
                warnings.push(format!("trusted function at {loc} has no ledger entry"));
                continue;
            }
        };
        slot.0 += 1;
        slot.1 += lines;
    }
    for s in seams {
        let is_fn = FN_KINDS.contains(&s.kind.as_str());
        if !is_fn && !OTHER_KINDS.contains(&s.kind.as_str()) {
            errors.push(format!("{}: unknown seam kind {:?}", s.loc, s.kind));
        } else if is_fn
            && s.status == Status::Discharged
            && !trusted.iter().any(|(loc, _)| *loc == s.loc)
        {
            errors.push(format!(
                "{}: discharged {} seam matches no trusted function",
                s.loc, s.kind
            ));
        }
    }
    (checked, warnings, errors)
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

/// The figures verus/FLOOR records, in [`Floor`]'s order, and what each
/// counts: the verified share of functions and of lines, and the combined
/// share (verified or discharged) of each.
const FIGURES: [(&str, &str); 4] = [
    ("functions", "verified"),
    ("lines", "verified"),
    ("combined-functions", "verified or discharged"),
    ("combined-lines", "verified or discharged"),
];

/// The shares, in tenths of a percent rounded down (84 is 8.4%), in
/// [`FIGURES`]' order.
#[derive(Debug, PartialEq, Eq)]
struct Floor([u32; 4]);

fn tenths(a: usize, b: usize) -> u32 {
    (a * 1000)
        .checked_div(b)
        .map_or(0, |t| u32::try_from(t).unwrap_or(u32::MAX))
}

/// verus/FLOOR: the figures, and the `lowered` lines that give a reason
/// for each figure set below the base revision's.
#[derive(Debug, PartialEq, Eq)]
struct FloorFile {
    floor: Floor,
    lowered: Vec<(String, u32)>,
}

/// Reads verus/FLOOR: `#` comments, one `FIGURE P` line per figure (P a
/// percentage with at most one decimal), and any number of `lowered
/// FIGURE P REASON` lines. `functions` and `lines` are required; a
/// missing `combined-functions` or `combined-lines` is 0.0, so a base
/// revision from before they existed still reads, and dropping one is a
/// lowering like any other.
fn parse_floor(text: &str) -> Result<FloorFile, String> {
    let mut figures = [None; 4];
    let mut lowered = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let n = i + 1;
        let figure = |key: &str, value: &str| -> Result<(usize, u32), String> {
            let Some(at) = FIGURES.iter().position(|(k, _)| *k == key) else {
                return Err(format!("line {n}: unknown figure {key:?}"));
            };
            let v = parse_tenths(value)
                .ok_or_else(|| format!("line {n}: {value:?} is not P or P.D"))?;
            Ok((at, v))
        };
        match words.as_slice() {
            ["lowered", key, value, reason @ ..] => {
                if reason.is_empty() {
                    return Err(format!("line {n}: a lowered line needs a reason"));
                }
                lowered.push(((*key).to_owned(), figure(key, value)?.1));
            }
            [key, value] => {
                let (at, v) = figure(key, value)?;
                if figures[at].replace(v).is_some() {
                    return Err(format!("line {n}: {key} given twice"));
                }
            }
            _ => {
                return Err(format!(
                    "line {n}: want `FIGURE P` or `lowered FIGURE P REASON`"
                ));
            }
        }
    }
    match figures {
        [Some(functions), Some(lines), cf, cl] => Ok(FloorFile {
            floor: Floor([functions, lines, cf.unwrap_or(0), cl.unwrap_or(0)]),
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
    for (i, (name, _)) in FIGURES.iter().enumerate() {
        let (h, b) = (head.floor.0[i], base.floor.0[i]);
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
    for (i, (name, what)) in FIGURES.iter().enumerate() {
        let (g, w) = (got.0[i], want.0[i]);
        if g < w {
            ok = false;
            report += &format!(
                "FAIL coverage floor: {name} {what} {}%, below the floor's {}%\n",
                shown(g),
                shown(w)
            );
        } else if g > w {
            report += &format!(
                "ok coverage floor: {name} {what} {}%, above the floor's {}%; raise verus/FLOOR to `{name} {}`\n",
                shown(g),
                shown(w),
                shown(g)
            );
        } else {
            report += &format!(
                "ok coverage floor: {name} {what} {}%, at the floor\n",
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
            trusted: Vec::new(),
            errors: 0,
        };
        c.items(&parse_file(src).expect("parses").items, false);
        (c.tallies.remove("k").unwrap_or_default(), c.errors)
    }

    #[test]
    fn cfg_predicates_that_hold_only_under_test_or_verification() {
        // (predicate, holds only under `test` or `kani`, holds only under
        // `test`, `kani` or `verus_keep_ghost`)
        for (cfg, test, unshipped) in [
            ("test", true, true),
            ("kani", true, true),
            ("any (test , kani)", true, true),
            ("not (kani)", false, false),
            ("not (any (test , kani))", false, false),
            ("any (kani , verus_keep_ghost)", false, true),
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
                floor: Floor([84, 100, 0, 0]),
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
            "functions 8.4\nlines 1\ncombined-lines 2\ncombined-lines 3\n",
            "functions 8.4\nlines 1\ncombined-functions 1.25\n",
            "functions 8.4\nlines 1\ncombined 3\n",
            "functions 8.4\nlines 1\nlowered combined-lines 3\n",
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
    fn the_combined_figures_are_optional_and_guarded_once_set() {
        let got = parse_floor(
            "functions 1\nlines 2\ncombined-functions 3\ncombined-lines 4.5\nlowered combined-lines 4.5 why\n",
        );
        assert_eq!(
            got,
            Ok(FloorFile {
                floor: Floor([10, 20, 30, 45]),
                lowered: vec![("combined-lines".into(), 45)],
            }),
            "floor: got = {got:?}, want = 1.0, 2.0, 3.0 and 4.5"
        );
        let floor = |text: &str| parse_floor(text).expect("parses");
        // A base from before the combined figures: any head value raises them.
        let (ok, report) = check_lowering(
            &floor("functions 1\nlines 2\ncombined-lines 9\n"),
            &floor("functions 1\nlines 2\n"),
        );
        assert!(ok, "adding combined-lines: got = fail, want = ok\n{report}");
        let base = floor("functions 1\nlines 2\ncombined-functions 3\ncombined-lines 9\n");
        for (text, want) in [
            (
                "functions 1\nlines 2\ncombined-functions 3\ncombined-lines 9\n",
                true,
            ),
            (
                "functions 1\nlines 2\ncombined-functions 3\ncombined-lines 8.9\n",
                false,
            ),
            (
                "functions 1\nlines 2\ncombined-functions 3\ncombined-lines 8.9\nlowered combined-lines 8.9 a seam lost its harness\n",
                true,
            ),
            (
                "functions 1\nlines 2\ncombined-functions 3\ncombined-lines 8.9\nlowered lines 8.9 wrong figure\n",
                false,
            ),
            // Dropping a figure lowers it to 0.0.
            ("functions 1\nlines 2\ncombined-functions 3\n", false),
            (
                "functions 1\nlines 2\ncombined-functions 3\nlowered combined-lines 0 no longer guarded\n",
                true,
            ),
        ] {
            let (ok, report) = check_lowering(&floor(text), &base);
            assert_eq!(ok, want, "{text:?}: got = {ok}, want = {want}\n{report}");
        }
    }

    /// The "Platform premises" of a verus/isla/seams.md cut down to P1 to
    /// P13 and K1 to K3 (as the file defines), with a bullet in another
    /// section that must not count.
    const SEAMS_MD: &str = "# The seams Isla checks\n\n- **K9, not a premise.** Outside the section.\n\n## Platform premises\n\nThe runs start from a stated state.\n\n- **P1, the build.** a\n- **P2, user memory.** b\n- **P3, x.** c\n- **P4, x.** d\n- **P5, x.** e\n- **P6, x.** f\n- **P7, x.** g\n- **P8, GOT entry contents.** h\n  continued, with a comma\n- **P9, x.** i\n- **P10, x.** j\n- **P11, x.** k\n- **P12, x.** l\n- **P13, x.** m\n- **K1, the kernel's side of a system call.** n\n- **K2, no resume past a trap.** o\n- **K3, a fixed data key A and EnDA.** p\n- **Note, not a label.** q\n\n## The GOT functions\n\n- **K5, not a premise.** Outside the section.\n";

    fn premises() -> BTreeSet<String> {
        premise_labels(SEAMS_MD)
    }

    #[test]
    fn the_platform_premise_labels_are_the_sections_bullets() {
        let got = premises();
        let want: BTreeSet<String> = (1..=13)
            .map(|n| format!("P{n}"))
            .chain((1..=3).map(|n| format!("K{n}")))
            .collect();
        assert_eq!(got, want, "labels: got = {got:?}, want = {want:?}");
        let got = premise_labels("- **K1, x.** y\n");
        assert!(got.is_empty(), "no section: got = {got:?}, want = none");
    }

    fn seam(loc: &str, kind: &str, status: Status) -> Seam {
        Seam {
            loc: loc.into(),
            kind: kind.into(),
            status,
        }
    }

    #[test]
    fn a_ledger_entry_gives_its_place_kind_and_status() {
        let text = r#"# Seams in `src/a.rs`

| item | kind | category | tool | status |
|---|---|---|---|---|
| `f` | axiom | core library | Kani | discharged |

## `f`

src/a.rs:10. Kind: axiom. Category: core library.
Tool: Kani. Status: discharged (Kani: a::kani_f::f).

```rust
axiom fn f();
```

## `g`

src/a.rs:20. Kind: external_body. Category: FFI/syscall.
Tool: Kani. Status: trusted (partly checked, Kani: a::kani_g::g; not checked: errno).

## `h`

src/a.rs:30. Kind: external_body const. Category: core library.
Tool: Kani. Status:
trusted

## `k`

src/a.rs:40. Kind: external_body proof. Category: x.
Tool: none (assumption).
Status: discharged (const assert: `k` in src/a.rs).
"#;
        let got = parse_ledger(text, &premises());
        let want = vec![
            seam("src/a.rs:10", "axiom", Status::Discharged),
            seam("src/a.rs:20", "external_body", Status::Partly),
            seam("src/a.rs:30", "external_body const", Status::Unchecked),
            seam("src/a.rs:40", "external_body proof", Status::Discharged),
        ];
        assert_eq!(got, Ok(want), "seams: got = {got:?}");
        // The status belongs to the entry's own paragraph.
        let got = parse_ledger(
            "src/a.rs:10. Kind: axiom. Category: x.\nTool: Kani.\n\nStatus: discharged\n",
            &premises(),
        );
        assert!(
            got.is_err(),
            "no status in the entry: got = {got:?}, want = an error"
        );
        // A header without a category still gives its kind.
        let got = parse_ledger(
            "src/a.rs:10. Kind: axiom.\nStatus: discharged (x).\n",
            &premises(),
        );
        assert_eq!(
            got,
            Ok(vec![seam("src/a.rs:10", "axiom", Status::Discharged)]),
            "no category: got = {got:?}"
        );
    }

    #[test]
    fn an_isla_check_on_named_premises_counts_as_discharged() {
        let entry = |status: &str| {
            format!(
                "src/a.rs:10. Kind: external_body. Category: inline asm.\nTool: Islaris/Isla. Status: {status}.\n"
            )
        };
        for (status, want) in [
            (
                "discharged on premises (Isla: verus/isla/seams.md; premises: K1 (the kernel's side of the call))",
                Status::Discharged,
            ),
            (
                "discharged on premises (Isla: verus/isla/seams.md; premises: K2 (no resume past an autda that faults) and K3 (a fixed data key A and EnDA))",
                Status::Discharged,
            ),
            (
                "discharged on premises (Isla: verus/isla/seams.md; premises: P8 (a gloss, with a comma and (nested) parentheses))",
                Status::Discharged,
            ),
            // The same check and premises, still partly checked.
            (
                "trusted (partly checked, Isla: verus/isla/seams.md; not checked: K1 (the kernel's side of the call))",
                Status::Partly,
            ),
        ] {
            let got = parse_ledger(&entry(status), &premises());
            assert_eq!(
                got,
                Ok(vec![seam("src/a.rs:10", "external_body", want)]),
                "{status}: got = {got:?}, want = {want:?}"
            );
        }
        // Only Isla's checks count on premises, and only on named ones.
        for status in [
            "discharged on premises (Kani: a::kani_f::f; premises: K1 (the kernel's side of the call))",
            "discharged on premises (emptyasm: f in src/a.rs; premises: the pointer's provenance)",
            "discharged on premises (Isla: verus/isla/seams.md)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: )",
            // A premise of Verus's own model is no platform premise.
            "discharged on premises (Isla: verus/isla/seams.md; premises: P8 (the GOT entry holds the slot's TLS offset) and the TLS slot premise)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: P8 (x), the TLS slot premise and TlsCell::tp (TPIDR_EL0 is the cell's tp))",
            "discharged on premises (Isla: verus/isla/seams.md; premises: TlsCell::tp)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: none)",
            // A label of the right shape that no platform premise has.
            "discharged on premises (Isla: verus/isla/seams.md; premises: K4)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: P0)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: K01 (the kernel's side of the call))",
            "discharged on premises (Isla: verus/isla/seams.md; premises: K1 and K5)",
            // Text after the status's closing `)`, or a gloss that never closes.
            "discharged on premises (Isla: verus/isla/seams.md; premises: K1) and TlsCell::tp",
            "discharged on premises (Isla: verus/isla/seams.md; premises: K1 (gloss and the TLS slot premise",
            "discharged on premises (Isla: verus/isla/seams.md, and Kani: a::kani_f::f; premises: K1)",
            // A near miss of either discharged form does not slip through as
            // plain discharged.
            "discharged on premises(Kani: a::kani_f::f; premises: K1)",
            "discharged on premises: K1",
            "discharged, on premises (Kani: a::kani_f::f)",
        ] {
            let got = parse_ledger(&entry(status), &premises());
            assert!(got.is_err(), "{status}: got = {got:?}, want = an error");
        }
    }

    #[test]
    fn trusted_functions_are_classed_by_their_seams() {
        let trusted = vec![
            ("src/a.rs:10".to_owned(), 5),
            ("src/a.rs:20".to_owned(), 7),
            ("src/a.rs:30".to_owned(), 11),
            ("src/a.rs:40".to_owned(), 13),
        ];
        let seams = vec![
            seam("src/a.rs:10", "axiom", Status::Discharged),
            seam("src/a.rs:20", "external_body", Status::Partly),
            seam("src/a.rs:30", "external_body proof", Status::Unchecked),
            // Not functions: no trusted function to match, and none needed.
            seam("src/a.rs:50", "external_body const", Status::Discharged),
            seam("src/a.rs:60", "assume_spec", Status::Discharged),
            // Another kind on a function's line does not class it.
            seam("src/a.rs:40", "assume_new", Status::Discharged),
        ];
        let (checked, warnings, errors) = match_ledger(&trusted, &seams);
        let want = Checked {
            discharged: (1, 5),
            partly: (1, 7),
        };
        assert_eq!(checked, want, "checked: got = {checked:?}, want = {want:?}");
        assert_eq!(
            warnings,
            vec!["trusted function at src/a.rs:40 has no ledger entry".to_owned()],
            "warnings: got = {warnings:?}"
        );
        assert!(errors.is_empty(), "errors: got = {errors:?}, want = none");

        // A discharged function seam that no trusted function matches (its
        // function moved, or now counts as verified) fails, and so does a
        // kind the counter does not know; an unmatched unchecked or partly
        // checked one does not.
        let seams = vec![
            seam("src/a.rs:11", "axiom", Status::Discharged),
            seam("src/a.rs:12", "external_body", Status::Unchecked),
            seam("src/a.rs:13", "external_body", Status::Partly),
            seam("src/a.rs:14", "macro_seam", Status::Unchecked),
        ];
        let (_, _, errors) = match_ledger(&trusted, &seams);
        assert_eq!(
            errors,
            vec![
                "src/a.rs:11: discharged axiom seam matches no trusted function".to_owned(),
                "src/a.rs:14: unknown seam kind \"macro_seam\"".to_owned(),
            ],
            "errors: got = {errors:?}"
        );

        // Two function seams on one line are an error, not a silent pick.
        let seams = vec![
            seam("src/a.rs:10", "axiom", Status::Unchecked),
            seam("src/a.rs:10", "external_body", Status::Discharged),
        ];
        let (_, _, errors) = match_ledger(&trusted, &seams);
        assert_eq!(
            errors,
            vec!["src/a.rs:10: two function seams on one line".to_owned()],
            "errors: got = {errors:?}"
        );
    }

    #[test]
    fn the_four_shares_come_from_the_tally_and_the_discharged_functions() {
        let t = Tally {
            fns: [6, 3, 1],
            lines: [60, 30, 10],
            ghost: 0,
        };
        let c = Checked {
            discharged: (2, 25),
            partly: (1, 5),
        };
        let got = shares(&t, &c);
        let want = Floor([600, 600, 800, 850]);
        assert_eq!(got, want, "shares: got = {got:?}, want = {want:?}");
    }

    #[test]
    fn the_counter_records_where_trusted_functions_start() {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("src/a.rs"),
            list: false,
            tallies: BTreeMap::new(),
            trusted: Vec::new(),
            errors: 0,
        };
        let src = "verus! {\n    fn verified() {}\n    #[verifier::external_body]\n    fn trusted() {\n        1;\n    }\n}\n";
        c.items(&parse_file(src).expect("parses").items, false);
        assert_eq!(
            c.trusted,
            vec![("src/a.rs:4".to_owned(), 3)],
            "trusted: got = {:?}, want = src/a.rs:4 with 3 lines",
            c.trusted
        );
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
        let want = Floor([84, 105, 900, 950]);
        let (ok, report) = check_floor(&Floor([84, 105, 900, 950]), &want);
        assert!(ok, "at the floor: got = fail, wanted = ok\n{report}");
        let (ok, report) = check_floor(&Floor([83, 120, 900, 949]), &want);
        assert!(!ok, "below on functions: got = ok, wanted = fail\n{report}");
        assert!(
            report.contains("FAIL coverage floor: functions verified 8.3%, below the floor's 8.4%"),
            "report: got = {report:?}"
        );
        assert!(
            report.contains("raise verus/FLOOR to `lines 12.0`"),
            "report: got = {report:?}"
        );
        assert!(
            report.contains(
                "FAIL coverage floor: combined-lines verified or discharged 94.9%, below the floor's 95.0%"
            ),
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
    fn a_kani_module_skips_the_files_its_path_names() {
        let file = parse_file(
            "#[cfg(kani)] #[path = \"kani/mod.rs\"] mod kani_proofs; #[cfg(kani)] #[path = \"one.rs\"] mod one; #[path = \"kept/mod.rs\"] mod kept;",
        )
        .expect("parses");
        let mut skip = Vec::new();
        test_module_files(Path::new("src/lib.rs"), &file.items, &mut skip);
        for (f, want) in [
            ("src/kani/mod.rs", true),
            ("src/kani/ptr.rs", true),
            ("src/one.rs", true),
            ("src/kani_proofs.rs", false),
            ("src/kani_proofs/mod.rs", false),
            ("src/kept/mod.rs", false),
            ("src/kanix.rs", false),
        ] {
            let got = skip.iter().any(|s| Path::new(f).starts_with(s));
            assert_eq!(got, want, "{f}: got = {got}, want = {want}");
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
