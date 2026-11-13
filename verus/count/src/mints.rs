//! The token-mint gate (verus/MINTS, `[mints]` in the configuration): a
//! port's C API makes every C caller's argument token through one trusted
//! function, the mint function (pcre3's `object::c_mint`), which makes a
//! token of any kind that implements the token trait (pcre3's `CToken`).
//! Each kind is a premise (the C caller's contract its mint's `# Safety`
//! states), and each function that calls the mint function is a kind's
//! mint, verified over it. So a new kind, or a new mint, is a new premise,
//! and this gate makes it one that verus/MINTS has to list, with the seam
//! whose row states it. Below, `c_mint` and `CToken` stand for the names
//! the configuration gives.
//!
//! The check reads every crate's sources with `verus_syn`, at any depth
//! (inside `verus!` blocks, inline modules and `impl`s), and fails on:
//! - an `impl CToken for KIND` (by any path to the trait) whose kind is
//!   not a `kind` row, and a `kind` row no impl matches (stale);
//! - a function whose body or signature calls `c_mint` (by any path, with
//!   or without a turbofish) that is not a `mint` row, a `mint` row whose
//!   function calls it no more (stale), and a mint whose return type's
//!   `Tracked<KIND>` is not the kind its row names, or names no listed
//!   kind;
//! - a function that calls `c_mint` more than once, and a row listed
//!   twice (a duplicate);
//! - `c_mint` named anywhere but in a call (`let f = c_mint::<K>;`, a
//!   `const`, a cast): through such an alias a call would not name it;
//! - a `use` that renames `c_mint` or `CToken` (`use .. c_mint as m`):
//!   through the new name neither a call nor an impl would name them;
//! - `c_mint` or `CToken` inside an item-level macro other than `verus!`,
//!   whose tokens the check does not expand;
//! - `CToken` named anywhere but an item-level impl's trait path, the
//!   trait's definition, a `use` and a bound (`K: CToken`) in a
//!   signature or an impl's generics: an impl inside a function body, a
//!   `const _` block or a macro there is not an item the kind gate reads;
//! - a mint whose body is anything but one call of `c_mint`, alone or in
//!   an `unsafe` block, as its tail, so the kind it makes is the
//!   `Tracked<KIND>` it returns; and a turbofish naming another kind;
//! - a definition of `c_mint` other than the one in object.rs;
//! - a file or `verus!` block that does not parse, and a `kind` row
//!   citing a seam ID no seams table has.
//!
//! Every function is read from the syntax tree, so a call is credited to
//! the function whose body holds it, whatever a comment on its line says
//! or how the lines are laid out.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use verus_syn::__private::ToTokens;
use verus_syn::{ImplItem, Item, Signature, TraitItem, UseTree};

/// The names the gate holds the sources to: the mint function (`c_mint`),
/// the token trait (`CToken`), the file that defines the mint function,
/// the MINTS file, the seams tables as the report names them, and the
/// parse stand-ins ([`crate::config::stand_in`]).
#[derive(Debug, Clone)]
pub(crate) struct Names {
    pub(crate) mint: String,
    pub(crate) token: String,
    pub(crate) home: String,
    pub(crate) list: String,
    pub(crate) tables: String,
    pub(crate) stand_ins: Vec<[String; 2]>,
}

/// What the files hold: each `impl CToken for KIND` (kind, line), each
/// function that calls `c_mint` (name, the kind its return type's
/// `Tracked<..>` names, how many calls, line), each definition of
/// `c_mint`, and the errors.
#[derive(Debug)]
pub(crate) struct Scan {
    pub(crate) names: Names,
    pub(crate) kinds: Vec<(String, String, usize)>,
    pub(crate) mints: Vec<Mint>,
    pub(crate) defs: Vec<(String, usize)>,
    pub(crate) errors: Vec<String>,
}

/// A function whose body or signature calls `c_mint`.
#[derive(Debug)]
pub(crate) struct Mint {
    pub(crate) file: String,
    pub(crate) name: String,
    pub(crate) kind: Option<String>,
    pub(crate) calls: usize,
    pub(crate) line: usize,
}

/// A row of verus/MINTS: `kind FILE KIND SEAM` or `mint FILE FN KIND`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Kind {
        file: String,
        kind: String,
        seam: String,
        line: usize,
    },
    Mint {
        file: String,
        name: String,
        kind: String,
        line: usize,
    },
}

/// The rows of `text` (verus/MINTS); blank lines and `#` comments are
/// skipped. A KIND is the type with its generic arguments and lifetimes
/// left out (`&mut CObj`), so it may hold a space: a `kind` row's seam is
/// its last word, and a `mint` row's kind is all the words after the
/// function. A row of neither shape is an error, which names the file as
/// `list`.
pub(crate) fn rows(text: &str, list: &str) -> (Vec<Row>, Vec<String>) {
    let mut out = Vec::new();
    let mut errs = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let w: Vec<&str> = t.split_whitespace().collect();
        let line = n + 1;
        match w.as_slice() {
            ["kind", file, kind @ .., seam] if !kind.is_empty() => out.push(Row::Kind {
                file: file.to_string(),
                kind: kind.join(" "),
                seam: seam.to_string(),
                line,
            }),
            ["mint", file, name, kind @ ..] if !kind.is_empty() => out.push(Row::Mint {
                file: file.to_string(),
                name: name.to_string(),
                kind: kind.join(" "),
                line,
            }),
            _ => errs.push(format!(
                "{list}:{line}: `{t}` is neither `kind FILE KIND SEAM` nor `mint FILE FN KIND`"
            )),
        }
    }
    (out, errs)
}

/// A type as a `kind` row names it: its tokens without lifetimes, up to
/// its first generic argument list (`&'a mut CObj<T>` is `&mut CObj`).
pub(crate) fn kind_name(ty: TokenStream) -> String {
    let toks: Vec<TokenTree> = ty.into_iter().collect();
    let mut s = String::new();
    let mut prev_word = false;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Punct(p) if p.as_char() == '\'' => {
                // A lifetime: the quote and its name.
                i += 2;
                continue;
            }
            TokenTree::Punct(p) if p.as_char() == '<' => break,
            TokenTree::Punct(p) => {
                s.push(p.as_char());
                prev_word = false;
            }
            TokenTree::Ident(id) => {
                if prev_word {
                    s.push(' ');
                }
                s.push_str(&id.to_string());
                prev_word = true;
            }
            other => {
                s.push_str(&other.to_string());
                prev_word = false;
            }
        }
        i += 1;
    }
    s
}

/// The kind a signature's return type names as `Tracked<KIND>`.
fn returned_kind(sig: &Signature) -> Option<String> {
    fn find(ts: TokenStream) -> Option<String> {
        let toks: Vec<TokenTree> = ts.into_iter().collect();
        for (i, t) in toks.iter().enumerate() {
            if let TokenTree::Group(g) = t
                && let Some(k) = find(g.stream())
            {
                return Some(k);
            }
            if matches!(t, TokenTree::Ident(id) if id == "Tracked")
                && matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '<')
            {
                let mut depth = 0usize;
                let mut inner = TokenStream::new();
                for t in &toks[i + 1..] {
                    match t {
                        TokenTree::Punct(p) if p.as_char() == '<' => {
                            depth += 1;
                            if depth == 1 {
                                continue;
                            }
                        }
                        TokenTree::Punct(p) if p.as_char() == '>' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(kind_name(inner));
                            }
                        }
                        _ => {}
                    }
                    inner.extend([t.clone()]);
                }
            }
        }
        None
    }
    find(sig.output.to_token_stream())
}

/// A mention of `c_mint`: a call, its definition (`fn c_mint`), or
/// anything else.
#[derive(Debug, PartialEq, Eq)]
enum Use {
    Call,
    Def,
    Other,
}

/// Each mention of `mint` (`c_mint`) in `ts`, at any depth, with its line.
fn mentions(ts: TokenStream, mint: &str, out: &mut Vec<(Use, usize)>) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => mentions(g.stream(), mint, out),
            TokenTree::Ident(id) if id == mint => {
                let line = id.span().start().line;
                if i > 0 && matches!(&toks[i - 1], TokenTree::Ident(f) if f == "fn") {
                    out.push((Use::Def, line));
                    continue;
                }
                let call = matches!(toks.get(crate::past_turbofish(&toks, i)),
                    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
                out.push((if call { Use::Call } else { Use::Other }, line));
            }
            _ => {}
        }
    }
}

/// Whether `ts` names `name` at any depth.
fn names(ts: TokenStream, name: &str) -> bool {
    ts.into_iter().any(|t| match t {
        TokenTree::Group(g) => names(g.stream(), name),
        TokenTree::Ident(id) => id == name,
        _ => false,
    })
}

/// The lines of each mention of `token` (`CToken`) in `ts`, at any depth,
/// that is not a bound (`K: CToken`, `T: A + CToken`) where `bounds`
/// allows one. An `impl CToken for` at item level is read apart
/// ([`Scan::items`]); anywhere else (a function body, a `const` block, a
/// type) a mention could be an impl the kind gate would not see, so it is
/// refused.
fn strays(ts: TokenStream, token: &str, bounds: bool, out: &mut Vec<usize>) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => strays(g.stream(), token, bounds, out),
            TokenTree::Ident(id) if id == token => {
                let bound = i > 0
                    && ((punct(i - 1, ':') && !(i > 1 && punct(i - 2, ':'))) || punct(i - 1, '+'));
                if !(bounds && bound) {
                    out.push(id.span().start().line);
                }
            }
            _ => {}
        }
    }
}

/// Whether `block` is exactly one call of `mint` (`c_mint(args)`, by
/// any path, with or without a turbofish), alone or in an `unsafe`
/// block, as its tail: then the kind it makes is the function's return
/// type's. With a turbofish, the kind it names.
fn whole_body_call(block: &verus_syn::Block, mint: &str) -> Option<Option<String>> {
    use verus_syn::{Expr, GenericArgument, PathArguments, Stmt};
    let [Stmt::Expr(e, None)] = block.stmts.as_slice() else {
        return None;
    };
    let call = match e {
        Expr::Unsafe(u) => match u.block.stmts.as_slice() {
            [Stmt::Expr(Expr::Call(c), None)] => c,
            _ => return None,
        },
        Expr::Call(c) => c,
        _ => return None,
    };
    let Expr::Path(p) = &*call.func else {
        return None;
    };
    let last = p.path.segments.last()?;
    if last.ident != mint {
        return None;
    }
    match &last.arguments {
        PathArguments::None => Some(None),
        PathArguments::AngleBracketed(a) => match a.args.iter().collect::<Vec<_>>().as_slice() {
            [GenericArgument::Type(t)] => Some(Some(kind_name(t.to_token_stream()))),
            _ => None,
        },
        PathArguments::Parenthesized(_) => None,
    }
}

impl Scan {
    pub(crate) fn new(names: Names) -> Scan {
        Scan {
            names,
            kinds: Vec::new(),
            mints: Vec::new(),
            defs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// A function `name` (`Owner::name` for a method) with `sig` and
    /// `block`, its body if it has one.
    fn function(
        &mut self,
        file: &str,
        name: String,
        sig: &Signature,
        block: Option<&verus_syn::Block>,
    ) {
        let (mint, token) = (self.names.mint.clone(), self.names.token.clone());
        let line = sig.fn_token.span.start().line;
        let body = block.map(ToTokens::to_token_stream).unwrap_or_default();
        let mut found = Vec::new();
        mentions(sig.to_token_stream(), &mint, &mut found);
        mentions(body.clone(), &mint, &mut found);
        let mut calls = 0;
        for (u, at) in found {
            match u {
                Use::Call => calls += 1,
                Use::Def => self.defs.push((file.to_string(), at)),
                Use::Other => self.errors.push(format!(
                    "{file}:{at}: `{name}` names {mint} other than in a call (an alias); call it, or not at all"
                )),
            }
        }
        let mut bad = Vec::new();
        strays(sig.to_token_stream(), &token, true, &mut bad);
        strays(body, &token, false, &mut bad);
        for at in bad {
            self.errors.push(format!(
                "{file}:{at}: `{name}` names {token} other than as a bound in its signature; a kind is an item-level `impl {token} for KIND`"
            ));
        }
        if calls == 0 {
            return;
        }
        let kind = returned_kind(sig);
        match block.and_then(|b| whole_body_call(b, &mint)) {
            None => self.errors.push(format!(
                "{file}:{line}: `{name}`'s body is not just `unsafe {{ {mint}(args) }}`, so the kind it makes need not be the `Tracked<KIND>` it returns"
            )),
            Some(Some(t)) if Some(&t) != kind.as_ref() => self.errors.push(format!(
                "{file}:{line}: `{name}` calls `{mint}::<{t}>` but returns `Tracked<{}>`",
                kind.as_deref().unwrap_or("?")
            )),
            Some(_) => {}
        }
        self.mints.push(Mint {
            file: file.to_string(),
            name,
            kind,
            calls,
            line,
        });
    }

    /// An item that is not a function: `c_mint` may not appear in it,
    /// nor `CToken` but as a bound where `bounds` allows one.
    fn other(&mut self, file: &str, ts: TokenStream, what: &str, bounds: bool) {
        let (mint, token) = (self.names.mint.clone(), self.names.token.clone());
        let mut found = Vec::new();
        mentions(ts.clone(), &mint, &mut found);
        for (u, at) in found {
            if u == Use::Def {
                self.defs.push((file.to_string(), at));
            } else {
                self.errors.push(format!(
                    "{file}:{at}: {what} names {mint} outside a function; only a mint's body may call it"
                ));
            }
        }
        let mut bad = Vec::new();
        strays(ts, &token, bounds, &mut bad);
        for at in bad {
            self.errors.push(format!(
                "{file}:{at}: {what} names {token}; a kind is an item-level `impl {token} for KIND`"
            ));
        }
    }

    /// A `use` tree: a rename of `c_mint` or `CToken` is an alias.
    fn use_tree(&mut self, file: &str, t: &UseTree) {
        match t {
            UseTree::Path(p) => self.use_tree(file, &p.tree),
            UseTree::Group(g) => {
                for i in &g.items {
                    self.use_tree(file, i);
                }
            }
            UseTree::Rename(r) if r.ident == self.names.mint || r.ident == self.names.token => {
                self.errors.push(format!(
                    "{file}:{}: `use .. {} as {}` renames it; through the new name neither a call nor an impl names it",
                    r.ident.span().start().line,
                    r.ident,
                    r.rename
                ));
            }
            _ => {}
        }
    }

    /// The items of `file`, at any depth; `owner` is the `impl`'s type or
    /// the trait whose items they are.
    pub(crate) fn items(&mut self, file: &str, items: &[Item], owner: &str) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    self.function(
                        file,
                        crate::reach::key(owner, &f.sig.ident.to_string()),
                        &f.sig,
                        Some(&f.block),
                    );
                }
                Item::Impl(i) => {
                    let is_token = i
                        .trait_
                        .as_ref()
                        .and_then(|(_, p, _)| p.segments.last())
                        .is_some_and(|s| s.ident == self.names.token);
                    if is_token {
                        self.kinds.push((
                            file.to_string(),
                            kind_name(i.self_ty.to_token_stream()),
                            i.impl_token.span.start().line,
                        ));
                    } else if let Some((_, p, _)) = &i.trait_ {
                        self.other(file, p.to_token_stream(), "an impl's trait", false);
                    }
                    self.other(
                        file,
                        i.generics.to_token_stream(),
                        "an impl's generics",
                        true,
                    );
                    self.other(file, i.self_ty.to_token_stream(), "an impl's type", false);
                    let ty = crate::inputs::base(&i.self_ty.to_token_stream().to_string());
                    for ii in &i.items {
                        match ii {
                            ImplItem::Fn(f) => {
                                self.function(
                                    file,
                                    crate::reach::key(&ty, &f.sig.ident.to_string()),
                                    &f.sig,
                                    Some(&f.block),
                                );
                            }
                            other => {
                                self.other(file, other.to_token_stream(), "an impl item", false)
                            }
                        }
                    }
                }
                Item::Trait(t) => {
                    for ti in &t.items {
                        match ti {
                            TraitItem::Fn(f) => {
                                self.function(
                                    file,
                                    crate::reach::key(
                                        &t.ident.to_string(),
                                        &f.sig.ident.to_string(),
                                    ),
                                    &f.sig,
                                    f.default.as_ref(),
                                );
                            }
                            other => {
                                self.other(file, other.to_token_stream(), "a trait item", false)
                            }
                        }
                    }
                }
                Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        self.items(file, inner, "");
                    }
                }
                Item::Macro(m) if crate::verus_path(&m.mac.path) => {
                    match verus_syn::parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                        Ok(v) => self.items(file, &v.items, owner),
                        Err(e) => self.errors.push(format!("{file}: verus! block: {e}")),
                    }
                }
                Item::Macro(m) => {
                    for n in [self.names.mint.clone(), self.names.token.clone()] {
                        if names(m.mac.tokens.clone(), &n) {
                            self.errors.push(format!(
                                "{file}:{}: a macro other than verus! names {n}; the check does not expand it",
                                m.mac.bang_token.span.start().line
                            ));
                        }
                    }
                }
                Item::Use(u) => self.use_tree(file, &u.tree),
                other => self.other(file, other.to_token_stream(), "an item", false),
            }
        }
    }
}

/// Reads `text`, the file `file` (relative to the root), into `scan`. A
/// file that does not parse is an error.
pub(crate) fn scan_file(file: &str, text: &str, scan: &mut Scan) {
    // The counter's stand-ins, for the keywords Verus keeps.
    let text = crate::config::stand_in(&scan.names.stand_ins, text);
    match verus_syn::parse_file(&text) {
        Ok(f) => scan.items(file, &f.items, ""),
        Err(e) => scan
            .errors
            .push(format!("{file}:{}: {e}", e.span().start().line)),
    }
}

/// Holds what `scan` found to `rows` (verus/MINTS) and the seam IDs the
/// tables define. The failures; none means the gate passes.
pub(crate) fn check(scan: &Scan, rows: &[Row], seams: &BTreeSet<String>) -> Vec<String> {
    let Names {
        mint,
        token,
        home,
        list,
        tables,
        ..
    } = &scan.names;
    let mut fails = scan.errors.clone();
    let mut kind_rows: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    let mut mint_rows: BTreeMap<(&str, &str), Vec<(&str, usize)>> = BTreeMap::new();
    for r in rows {
        match r {
            Row::Kind {
                file,
                kind,
                seam,
                line,
            } => {
                kind_rows.entry((file, kind)).or_default().push(*line);
                if !seams.contains(seam) {
                    fails.push(format!(
                        "{list}:{line}: kind `{kind}` cites {seam}, which no seams table ({tables}) has a row for"
                    ));
                }
            }
            Row::Mint {
                file,
                name,
                kind,
                line,
            } => mint_rows
                .entry((file, name))
                .or_default()
                .push((kind, *line)),
        }
    }
    for ((file, kind), lines) in &kind_rows {
        if lines.len() > 1 {
            fails.push(format!(
                "{list}:{}: kind `{file} {kind}` is listed {} times",
                lines[1],
                lines.len()
            ));
        }
    }
    for ((file, name), rs) in &mint_rows {
        if rs.len() > 1 {
            fails.push(format!(
                "{list}:{}: mint `{file} {name}` is listed {} times",
                rs[1].1,
                rs.len()
            ));
        }
    }
    let listed_kinds: BTreeSet<&str> = kind_rows.keys().map(|(_, k)| *k).collect();
    // The kinds.
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (file, kind, line) in &scan.kinds {
        *seen.entry((file, kind)).or_default() += 1;
        if !kind_rows.contains_key(&(file.as_str(), kind.as_str())) {
            fails.push(format!(
                "{file}:{line}: `impl {token} for {kind}` is a token kind {list} does not list: a new premise needs a `kind {file} {kind} SEAM` row and that seam's row"
            ));
        }
    }
    for ((file, kind), n) in &seen {
        if *n > 1 {
            fails.push(format!(
                "{file}: `{token} for {kind}` is implemented {n} times"
            ));
        }
    }
    for ((file, kind), lines) in &kind_rows {
        if !seen.contains_key(&(*file, *kind)) {
            fails.push(format!(
                "{list}:{}: kind `{file} {kind}` has no `impl {token} for {kind}` in {file}: drop the row",
                lines[0]
            ));
        }
    }
    // The mints.
    let mut found: BTreeSet<(&str, &str)> = BTreeSet::new();
    for m in &scan.mints {
        let at = format!("{}:{}", m.file, m.line);
        if !found.insert((&m.file, &m.name)) {
            fails.push(format!(
                "{at}: `{}` is a second function of that name that calls {mint}",
                m.name
            ));
        }
        if m.calls > 1 {
            fails.push(format!(
                "{at}: `{}` calls {mint} {} times; a mint calls it once",
                m.name, m.calls
            ));
        }
        let Some(rs) = mint_rows.get(&(m.file.as_str(), m.name.as_str())) else {
            fails.push(format!(
                "{at}: `{}` calls {mint} but {list} does not list it: a new mint needs a `mint {} {} KIND` row",
                m.name, m.file, m.name
            ));
            continue;
        };
        let listed = rs[0].0;
        match &m.kind {
            None => fails.push(format!(
                "{at}: `{}` calls {mint} but returns no `Tracked<KIND>`",
                m.name
            )),
            Some(k) if k != listed => fails.push(format!(
                "{at}: `{}` returns `Tracked<{k}>`, but {list}:{} lists it as a mint of `{listed}`",
                m.name, rs[0].1
            )),
            Some(_) => {}
        }
        if !listed_kinds.contains(listed) {
            fails.push(format!(
                "{list}:{}: mint `{}` is of kind `{listed}`, which no `kind` row lists",
                rs[0].1, m.name
            ));
        }
    }
    for ((file, name), rs) in &mint_rows {
        if !found.contains(&(*file, *name)) {
            fails.push(format!(
                "{list}:{}: mint `{file} {name}` does not call {mint}: drop the row",
                rs[0].1
            ));
        }
    }
    // The one definition.
    match scan.defs.as_slice() {
        [(f, _)] if f == home => {}
        defs => fails.push(format!(
            "{mint} is defined {} times ({}); the one definition is in {home}",
            defs.len(),
            defs.iter()
                .map(|(f, l)| format!("{f}:{l}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
    fails
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "# comment\n\
kind crates/pcre3-capi/src/object.rs CBuf S-cc3\n\
kind crates/pcre3-capi/src/object.rs &CObj S-cc3\n\
kind crates/pcre3-capi/src/object.rs &mut CObj S-cc3\n\
mint crates/pcre3-capi/src/object.rs c_buffer CBuf\n\
mint crates/pcre3-capi/src/object.rs c_object &CObj\n\
mint crates/pcre3-capi/src/object.rs c_object_mut &mut CObj\n";

    /// A clean object.rs: the trait, `c_mint`, three kinds and their mints,
    /// inside a `verus!` block as the real one is.
    const CLEAN: &str = r#"
verus! {
pub(crate) trait CToken {
    type Args;
    spec fn minted(self, args: Self::Args) -> bool;
}
#[verifier::external_body]
pub(crate) unsafe fn c_mint<K: CToken>(args: K::Args) -> (r: Tracked<K>)
    ensures
        r@.minted(args),
{
    Tracked::assume_new()
}
impl<T> CToken for CBuf<T> {
    type Args = (*mut T, usize);
    open spec fn minted(self, args: (*mut T, usize)) -> bool { self.is(args.0, args.1) }
}
impl<T: Object> CToken for &CObj<T> {
    type Args = *const T;
    open spec fn minted(self, ptr: *const T) -> bool { self.ptr() == ptr as *mut T }
}
impl<T: Object> CToken for &mut CObj<T> {
    type Args = *mut T;
    open spec fn minted(self, ptr: *mut T) -> bool { self.ptr() == ptr }
}
pub(crate) unsafe fn c_buffer<T>(ptr: *mut T, len: usize) -> (r: Tracked<CBuf<T>>)
    ensures
        r@.is(ptr, len),
{
    unsafe { c_mint((ptr, len)) }
}
pub(crate) unsafe fn c_object<'a, T: Object>(ptr: *const T) -> (r: Tracked<&'a CObj<T>>)
{
    unsafe { c_mint(ptr) }
}
pub(crate) unsafe fn c_object_mut<'a, T: Object>(ptr: *mut T) -> (r: Tracked<&'a mut CObj<T>>)
{
    unsafe { object::c_mint::<&'a mut CObj<T>>(ptr) }
}
} // verus!
"#;

    const FILE: &str = "crates/pcre3-capi/src/object.rs";

    /// pcre3's names, which the fixtures use.
    fn names() -> Names {
        Names {
            mint: "c_mint".into(),
            token: "CToken".into(),
            home: FILE.into(),
            list: "verus/MINTS".into(),
            tables: "verus/SEAMS.md, verus/theorems/*.md".into(),
            stand_ins: vec![["assert::".into(), "assrtx::".into()]],
        }
    }

    /// The failures for `src` as object.rs, with `list` as verus/MINTS.
    fn run(src: &str, list: &str) -> Vec<String> {
        let mut s = Scan::new(names());
        scan_file(FILE, src, &mut s);
        let (rows, mut errs) = rows(list, "verus/MINTS");
        let seams: BTreeSet<String> = ["S-cc3".to_string()].into();
        errs.extend(check(&s, &rows, &seams));
        errs
    }

    /// `CLEAN` with `extra` added inside its `verus!` block.
    fn with(extra: &str) -> String {
        CLEAN.replace("} // verus!", &format!("{extra}\n}} // verus!"))
    }

    fn fails_with(src: &str, list: &str, want: &str) {
        let got = run(src, list);
        assert!(
            got.iter().any(|f| f.contains(want)),
            "want a failure containing {want:?}, got {got:#?}"
        );
    }

    #[test]
    fn clean_tree_passes() {
        assert_eq!(run(CLEAN, LIST), Vec::<String>::new());
    }

    #[test]
    fn kind_names_drop_lifetimes_and_generics() {
        let k = |s: &str| kind_name(s.parse().unwrap());
        assert_eq!(k("&'a mut CObj<T>"), "&mut CObj");
        assert_eq!(k("&CCode<U>"), "&CCode");
        assert_eq!(k("CConvertOut<U>"), "CConvertOut");
        assert_eq!(k("&CWhere"), "&CWhere");
    }

    #[test]
    fn rows_parse() {
        let (r, e) = rows(LIST, "verus/MINTS");
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(r.len(), 6);
        assert!(r.contains(&Row::Kind {
            file: FILE.into(),
            kind: "&mut CObj".into(),
            seam: "S-cc3".into(),
            line: 4
        }));
        let (_, e) = rows("kind a.rs\nmint a.rs f\nother a b c\n", "x/MINTS");
        assert_eq!(e.len(), 3, "{e:?}");
        assert!(e[0].starts_with("x/MINTS:1: "), "{e:?}");
    }

    // Negative: a new kind, by the plain path, by another path, and split
    // over two lines.
    #[test]
    fn unlisted_kinds_fail() {
        for impl_ in [
            "impl CToken for MemBlock { type Args = (); }",
            "impl crate::object::CToken for MemBlock { type Args = (); }",
            "impl crate::object::CToken\n    for MemBlock { type Args = (); }",
            "} impl CToken for MemBlock { type Args = (); } fn x() {",
        ] {
            let src = with(impl_);
            fails_with(
                &src,
                LIST,
                "`impl CToken for MemBlock` is a token kind verus/MINTS does not list",
            );
        }
    }

    // Negative: an alias of CToken, or of c_mint, and a braced rename.
    #[test]
    fn renames_fail() {
        for (u, name) in [
            ("use crate::object::CToken as Tok;", "CToken as Tok"),
            ("use self::c_mint as m;", "c_mint as m"),
            ("use crate::object::{c_mint as m, CBuf};", "c_mint as m"),
        ] {
            fails_with(&with(u), LIST, &format!("`use .. {name}` renames it"));
        }
        // The aliased kind and a mint returning it: the rename fails it,
        // as `c_dealloc` returning a kind made under `Tok` would.
        let src = with(
            "use self::CToken as Tok;\nimpl Tok for MemBlock { type Args = (); }\npub(crate) unsafe fn c_dealloc(p: *mut c_void) -> (r: Tracked<MemBlock>) { unsafe { m(p) } }",
        );
        fails_with(&src, LIST, "`use .. CToken as Tok` renames it");
    }

    // Negative: c_mint named but not called, in a fn and in a const.
    #[test]
    fn non_call_mentions_fail() {
        fails_with(
            &with("fn item() { let f = c_mint::<CBuf<u8>>; }"),
            LIST,
            "`item` names c_mint other than in a call",
        );
        fails_with(
            &with("const F: unsafe fn(()) -> Tracked<CBuf<u8>> = c_mint;"),
            LIST,
            "names c_mint outside a function",
        );
    }

    // Negative: an unlisted mint, after a `}` on the same line, after the
    // definition on the same line, and one a comment names as another.
    #[test]
    fn unlisted_mints_fail() {
        for src in [
            with("pub(crate) fn sneaky(p: *mut u8, n: usize) -> Tracked<CBuf<u8>> { unsafe { c_mint((p, n)) } }"),
            CLEAN.replace(
                "    unsafe { c_mint((ptr, len)) }\n}",
                "    unsafe { c_mint((ptr, len)) }\n} pub(crate) fn sneaky(p: *mut u8, n: usize) -> Tracked<CBuf<u8>> { unsafe { c_mint((p, n)) } }",
            ),
            CLEAN.replace(
                "    Tracked::assume_new()\n}",
                "    Tracked::assume_new()\n} fn sneaky(p: *mut u8, n: usize) -> Tracked<CBuf<u8>> { unsafe { c_mint((p, n)) } }",
            ),
        ] {
            fails_with(&src, LIST, "`sneaky` calls c_mint but verus/MINTS does not list it");
        }
        // c_buffer delegates to sneaky, whose call carries a comment naming
        // c_buffer: sneaky is unlisted, and c_buffer no longer mints.
        let src = CLEAN.replace(
            "    unsafe { c_mint((ptr, len)) }\n}",
            "    unsafe { sneaky(ptr, len) }\n}\npub(crate) unsafe fn sneaky<T>(ptr: *mut T, len: usize) -> Tracked<CBuf<T>> {\n    unsafe { c_mint((ptr, len)) } // as fn c_buffer does\n}",
        );
        fails_with(
            &src,
            LIST,
            "`sneaky` calls c_mint but verus/MINTS does not list it",
        );
        fails_with(
            &src,
            LIST,
            "mint `crates/pcre3-capi/src/object.rs c_buffer` does not call c_mint",
        );
    }

    // Negative: a mint that calls c_mint twice, a duplicate row, a stale
    // kind row and a stale mint row.
    #[test]
    fn duplicates_and_stale_rows_fail() {
        let twice = CLEAN.replace(
            "    unsafe { c_mint((ptr, len)) }\n}",
            "    let _t: Tracked<CBuf<T>> = unsafe { c_mint((ptr, len)) };\n    unsafe { c_mint((ptr, len)) }\n}",
        );
        fails_with(&twice, LIST, "`c_buffer` calls c_mint 2 times");
        let dup = format!("{LIST}mint {FILE} c_buffer CBuf\n");
        fails_with(
            CLEAN,
            &dup,
            "mint `crates/pcre3-capi/src/object.rs c_buffer` is listed 2 times",
        );
        let dupk = format!("{LIST}kind {FILE} CBuf S-cc3\n");
        fails_with(
            CLEAN,
            &dupk,
            "kind `crates/pcre3-capi/src/object.rs CBuf` is listed 2 times",
        );
        let stale = format!("{LIST}kind {FILE} CGone S-cc3\nmint {FILE} c_gone CBuf\n");
        fails_with(
            CLEAN,
            &stale,
            "kind `crates/pcre3-capi/src/object.rs CGone` has no `impl CToken for CGone`",
        );
        fails_with(
            CLEAN,
            &stale,
            "mint `crates/pcre3-capi/src/object.rs c_gone` does not call c_mint",
        );
    }

    // Negative: a mint whose return type changes to another kind, listed or
    // not, and a kind row citing a seam no table has.
    #[test]
    fn a_mint_that_changes_kind_fails() {
        let src = CLEAN.replace(
            "(r: Tracked<CBuf<T>>)\n    ensures\n        r@.is(ptr, len),",
            "(r: Tracked<&'static CObj<T>>)",
        );
        fails_with(
            &src,
            LIST,
            "`c_buffer` returns `Tracked<&CObj>`, but verus/MINTS:5 lists it as a mint of `CBuf`",
        );
        let bad = LIST.replace("CBuf S-cc3", "CBuf S-zz9");
        fails_with(CLEAN, &bad, "cites S-zz9, which no seams table");
    }

    // Negative: an impl of CToken that is not an item-level impl: in a fn
    // body, in a `const _` block, in a macro inside a body, in a method;
    // and CToken as a type. A bound in a signature or impl header passes
    // (CLEAN's `c_mint<K: CToken>`).
    #[test]
    fn nested_ctoken_fails() {
        for (src, want) in [
            (
                "fn hide() { impl CToken for Bogus { type Args = (); } }",
                "`hide` names CToken other than as a bound",
            ),
            (
                "const _: () = { impl CToken for Bogus { type Args = (); open spec fn minted(self, a: ()) -> bool { false } } };",
                "an item names CToken",
            ),
            (
                "fn m() { my_macro! { impl CToken for Bogus {} } }",
                "`m` names CToken other than as a bound",
            ),
            (
                "impl S { fn f(&self) { impl crate::object::CToken for Bogus {} } }",
                "`S::f` names CToken other than as a bound",
            ),
            ("type T = dyn CToken;", "an item names CToken"),
        ] {
            fails_with(&with(src), LIST, want);
        }
        assert_eq!(
            run(
                &with("fn g<K: CToken, J: Copy + CToken>() {}\nimpl<K: CToken> Holder<K> {}"),
                LIST
            ),
            Vec::<String>::new()
        );
    }

    // Negative: a listed mint whose body does more than call c_mint (it
    // could make another kind and return a token from that one's
    // premise), and one whose turbofish names another kind.
    #[test]
    fn mint_bodies_are_one_call() {
        let src = CLEAN.replace(
            "    unsafe { c_mint((ptr, len)) }\n}",
            "    let t: Tracked<Bogus> = unsafe { c_mint(()) };\n    proof_from_false(t)\n}",
        );
        fails_with(
            &src,
            LIST,
            "`c_buffer`'s body is not just `unsafe { c_mint(args) }`",
        );
        let src = CLEAN.replace(
            "    unsafe { c_mint((ptr, len)) }\n}",
            "    unsafe { c_mint::<CDealloc>((ptr, len)) }\n}",
        );
        fails_with(
            &src,
            LIST,
            "`c_buffer` calls `c_mint::<CDealloc>` but returns `Tracked<CBuf>`",
        );
    }

    // Negative: a second definition, a macro hiding either name, and a
    // file that does not parse.
    #[test]
    fn fails_closed() {
        let mut s = Scan::new(names());
        scan_file(FILE, CLEAN, &mut s);
        scan_file("crates/x/src/lib.rs", "fn c_mint() {}", &mut s);
        let (rows, _) = rows(LIST, "verus/MINTS");
        let got = check(&s, &rows, &["S-cc3".to_string()].into()).join("\n");
        assert!(got.contains("c_mint is defined 2 times"), "{got}");
        fails_with(
            &with("my_macro! { c_mint((p, n)) }"),
            LIST,
            "a macro other than verus! names c_mint",
        );
        fails_with(
            &with("my_macro! { impl CToken for X {} }"),
            LIST,
            "a macro other than verus! names CToken",
        );
        fails_with("fn (", LIST, "crates/pcre3-capi/src/object.rs:1");
        fails_with("verus! { fn () }", LIST, "verus! block");
        fails_with("::vstd::prelude::verus! { fn () }", LIST, "verus! block");
    }

    // The names come from the configuration: under other names the clean
    // tree passes, and the gate holds the sources to those names alone.
    #[test]
    fn names_are_configured() {
        let rename = |s: &str| s.replace("c_mint", "make_tok").replace("CToken", "Tok");
        let other = Names {
            mint: "make_tok".into(),
            token: "Tok".into(),
            home: "crates/z/src/tok.rs".into(),
            list: "z/MINTS".into(),
            tables: "z/SEAMS.md".into(),
            stand_ins: Vec::new(),
        };
        let list = LIST.replace("crates/pcre3-capi/src/object.rs", "crates/z/src/tok.rs");
        let run = |src: &str| {
            let mut s = Scan::new(other.clone());
            scan_file("crates/z/src/tok.rs", src, &mut s);
            let (rows, mut errs) = rows(&list, "z/MINTS");
            errs.extend(check(&s, &rows, &["S-cc3".to_string()].into()));
            errs
        };
        assert_eq!(run(&rename(CLEAN)), Vec::<String>::new());
        // The pcre3 names mean nothing here: CLEAN defines no `make_tok`.
        let got = run(CLEAN).join("\n");
        assert!(
            got.contains("make_tok is defined 0 times") && got.contains("z/MINTS:"),
            "{got}"
        );
        let got = run(&rename(&with(
            "impl CToken for MemBlock { type Args = (); }",
        )))
        .join("\n");
        assert!(
            got.contains("`impl Tok for MemBlock` is a token kind z/MINTS does not list"),
            "{got}"
        );
    }
}
