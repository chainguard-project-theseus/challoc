//! `macro_rules!` expansion, so that the functions a port's own item
//! macros write (chainz's `export!`, pcre3's `width::exports!`) are
//! counted like any other ([`crate::Counter::item_macro`]).
//!
//! [`Rules::parse`] reads a definition's rules; [`Rules::expand`] matches
//! an invocation against them in order, as rustc does, and writes the
//! first matching rule's transcriber with the invocation's fragments
//! bound. The matcher reads the fragment kinds rustc has (`ident`, `ty`,
//! `expr`, `block`, `tt`, ...; the parsed ones by parsing a prefix of the
//! input with `verus_syn`), and repetitions with or without a separator,
//! nested to any depth, trying every way to match before giving up. The
//! transcriber writes `$var`, `$crate` and nested `$( .. )` repetitions,
//! each repeated as many times as the metavariables in it were matched.
//! Every token keeps its span: a token the transcriber writes keeps the
//! definition's, and a fragment keeps the invocation's.
//!
//! It fails closed: a definition it cannot read, an invocation no rule
//! matches, and a repetition whose metavariables repeat unequally are each
//! an error, never an empty expansion. A `$name` a rule does not bind is
//! written through, as rustc writes it (a macro that writes a
//! `macro_rules!`); anywhere else the expansion then does not parse.

use std::cell::Cell;
use std::collections::BTreeMap;

use proc_macro2::{Delimiter, Group, Ident, TokenStream, TokenTree};
use verus_syn::parse::{ParseStream, Parser};

thread_local! {
    /// The edition of the crate whose macros are expanded: below 2024, an
    /// `expr` fragment is what 2024 calls `expr_2021`, and below 2021 a
    /// `pat` is a `pat_param`, which takes no top-level `|` (the Rust
    /// reference, "Metavariables"). The walk sets it per crate
    /// ([`set_edition`]).
    static EDITION: Cell<u32> = const { Cell::new(2024) };
}

/// Sets the edition the matcher reads `expr` and `pat` by, for the crate
/// the walk is in.
pub(crate) fn set_edition(edition: u32) {
    EDITION.with(|e| e.set(edition));
}

/// Which of the editions the matcher tells apart `edition` falls in:
/// below 2021, 2021 up to 2024, and 2024 on. Two editions of one era match
/// every fragment alike.
pub(crate) fn matcher_era(edition: u32) -> u8 {
    match edition {
        ..2021 => 0,
        2021..2024 => 1,
        _ => 2,
    }
}

/// More ways than this to match one repetition is an error, not a search.
const MAX_MATCHES: usize = 4096;

/// One part of a rule's matcher.
#[derive(Clone, Debug)]
enum Pat {
    /// A token the input must repeat.
    Tok(TokenTree),
    /// A delimited group, matched inside.
    Group(Delimiter, Vec<Pat>),
    /// `$name:kind`.
    Frag(String, String),
    /// `$( .. ) sep op`, `op` one of `*`, `+` and `?`, and `sep` one token
    /// (one or more trees: `::` is two).
    Rep(Vec<Pat>, Option<Vec<TokenTree>>, char),
}

/// What a metavariable matched: a fragment, or one binding per
/// repetition.
#[derive(Clone, Debug)]
enum Bind {
    One(Vec<TokenTree>),
    Seq(Vec<Bind>),
}

type Binds = BTreeMap<String, Bind>;

/// The fragment kinds rustc's `macro_rules!` takes.
const KINDS: &[&str] = &[
    "block",
    "expr",
    "expr_2021",
    "ident",
    "item",
    "lifetime",
    "literal",
    "meta",
    "pat",
    "pat_param",
    "path",
    "stmt",
    "tt",
    "ty",
    "vis",
];

/// A `macro_rules!` definition: each rule's matcher and transcriber.
#[derive(Clone, Debug)]
pub(crate) struct Rules(Vec<(Vec<Pat>, TokenStream)>);

impl Rules {
    /// Reads `tokens`, the body of `macro_rules! name { .. }`.
    pub(crate) fn parse(tokens: TokenStream) -> Result<Rules, String> {
        let mut rules = Vec::new();
        let mut it = tokens.into_iter().peekable();
        while let Some(t) = it.next() {
            let TokenTree::Group(m) = t else {
                return Err(format!("a rule starts with `{t}`, not a matcher"));
            };
            match (it.next(), it.next()) {
                (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
                    if a.as_char() == '=' && b.as_char() == '>' => {}
                _ => return Err("a matcher is not followed by `=>`".into()),
            }
            let Some(TokenTree::Group(body)) = it.next() else {
                return Err("`=>` is not followed by a transcriber".into());
            };
            rules.push((pattern(m.stream())?, body.stream()));
            if let Some(TokenTree::Punct(p)) = it.peek()
                && p.as_char() == ';'
            {
                it.next();
            }
        }
        if rules.is_empty() {
            return Err("it has no rules".into());
        }
        Ok(Rules(rules))
    }

    /// The tokens `input`, an invocation's, expands to: the first rule
    /// whose matcher takes all of it, transcribed.
    pub(crate) fn expand(&self, input: TokenStream) -> Result<TokenStream, String> {
        let input: Vec<TokenTree> = input.into_iter().collect();
        for (pats, body) in &self.0 {
            let found = matches(pats, &input)?
                .into_iter()
                .find(|(n, _)| *n == input.len());
            if let Some((_, binds)) = found {
                let mut out = TokenStream::new();
                transcribe(body.clone(), &binds, &mut Vec::new(), &mut out)?;
                return Ok(out);
            }
        }
        Err("no rule matches the invocation".into())
    }
}

/// Reads one matcher.
fn pattern(ts: TokenStream) -> Result<Vec<Pat>, String> {
    let mut out = Vec::new();
    let trees: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while let Some(t) = trees.get(i) {
        i += 1;
        match t {
            TokenTree::Punct(p) if p.as_char() == '$' => match trees.get(i) {
                Some(TokenTree::Ident(name)) => match (trees.get(i + 1), trees.get(i + 2)) {
                    (Some(TokenTree::Punct(c)), Some(TokenTree::Ident(kind)))
                        if c.as_char() == ':' =>
                    {
                        i += 3;
                        let kind = kind.to_string();
                        if !KINDS.contains(&kind.as_str()) {
                            return Err(format!("`${name}:{kind}`: no such fragment kind"));
                        }
                        out.push(Pat::Frag(name.to_string(), kind));
                    }
                    _ => return Err(format!("`${name}` in a matcher has no `:kind`")),
                },
                Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                    let inner = pattern(g.stream())?;
                    let (sep, op, n) = repetition(&trees[i + 1..])?;
                    i += 1 + n;
                    out.push(Pat::Rep(inner, sep, op));
                }
                other => return Err(format!("`$` in a matcher followed by {other:?}")),
            },
            TokenTree::Group(g) => out.push(Pat::Group(g.delimiter(), pattern(g.stream())?)),
            t => out.push(Pat::Tok(t.clone())),
        }
    }
    Ok(out)
}

/// The separator, if any, and the operator after a `$( .. )`, read from
/// `rest`, the trees after it, and how many trees they take. The first
/// token is read whole, as rustc's lexer reads it ([`token_len`]), before
/// it is taken as the operator: in `$( .. )+=*` the separator is `+=`.
fn repetition(rest: &[TokenTree]) -> Result<(Option<Vec<TokenTree>>, char, usize), String> {
    let op = |t: Option<&TokenTree>| match t {
        Some(TokenTree::Punct(p)) if matches!(p.as_char(), '*' | '+' | '?') => Some(p.as_char()),
        _ => None,
    };
    let n = token_len(rest);
    if n == 1
        && let Some(o) = op(rest.first())
    {
        return Ok((None, o, 1));
    }
    match op(rest.get(n)) {
        Some(o) if n > 0 && o != '?' => Ok((Some(rest[..n].to_vec()), o, n + 1)),
        _ => Err("a repetition has no `*`, `+` or `?`".into()),
    }
}

/// The tokens rustc's lexer makes of several punctuation characters
/// (the Rust reference, "Punctuation"): proc-macro2 writes each as one
/// `Punct` per character, the ones before the last `Spacing::Joint`.
const GLUED: &[&str] = &[
    "<<=", ">>=", "...", "..=", "::", "->", "=>", "<-", "==", "!=", "<=", ">=", "&&", "||", "+=",
    "-=", "*=", "/=", "%=", "^=", "&=", "|=", "<<", ">>", "..",
];

/// How many of `input`'s leading trees make its first token, as rustc's
/// lexer reads tokens: a lifetime (`'` and an identifier), a run of joint
/// punctuation that is one of [`GLUED`] (the longest), else one tree.
fn token_len(input: &[TokenTree]) -> usize {
    let Some(TokenTree::Punct(p)) = input.first() else {
        return usize::from(!input.is_empty());
    };
    if p.as_char() == '\'' && matches!(input.get(1), Some(TokenTree::Ident(_))) {
        return 2;
    }
    let mut run = String::new();
    for t in input.iter().take(3) {
        let TokenTree::Punct(q) = t else { break };
        run.push(q.as_char());
        if q.spacing() == proc_macro2::Spacing::Alone {
            break;
        }
    }
    (2..=run.len())
        .rev()
        .find(|&n| GLUED.contains(&&run[..n]))
        .unwrap_or(1)
}

/// Whether input token `t` is matcher token `want`.
fn same(t: &TokenTree, want: &TokenTree) -> bool {
    match (t, want) {
        (TokenTree::Ident(a), TokenTree::Ident(b)) => a == b,
        (TokenTree::Punct(a), TokenTree::Punct(b)) => a.as_char() == b.as_char(),
        (TokenTree::Literal(a), TokenTree::Literal(b)) => a.to_string() == b.to_string(),
        _ => false,
    }
}

/// The fragment kinds a matched fragment of which is forwarded to another
/// macro as its tokens. Every other kind is forwarded opaque: the
/// matchers of the macro it is forwarded to see one fragment of that kind,
/// which no literal token matches (the Rust reference, "Forwarding a
/// matched fragment").
const TRANSPARENT: &[&str] = &["ident", "lifetime", "tt"];

/// The kinds forwarded opaque, each written as that many nested invisible
/// groups ([`bound`]), so that the kind travels with the fragment as
/// rustc's `MetaVarKind` does: `block` is one group, `vis` twelve. syn
/// reads through invisible groups, so the expansion parses as the tokens
/// inside would.
const OPAQUE: &[&str] = &[
    "block",
    "expr",
    "expr_2021",
    "item",
    "literal",
    "meta",
    "pat",
    "pat_param",
    "path",
    "stmt",
    "ty",
    "vis",
];

/// The kind of `t` if it is a fragment forwarded opaque ([`bound`]), and
/// its tokens.
fn opaque(t: &TokenTree) -> Option<(&'static str, Vec<TokenTree>)> {
    let mut depth = 0;
    let mut inside = vec![t.clone()];
    while let [TokenTree::Group(g)] = &inside[..]
        && g.delimiter() == Delimiter::None
        && depth < OPAQUE.len()
    {
        inside = g.stream().into_iter().collect();
        depth += 1;
    }
    depth.checked_sub(1).map(|d| (OPAQUE[d], inside))
}

/// Whether rustc's matcher tries a fragment of kind `want` on a forwarded
/// fragment of kind `got` holding `inside` (`nonterminal_may_begin_with`
/// in `rustc_parse`, with `Token::can_begin_expr`, `can_begin_type`,
/// `can_begin_literal_maybe_minus` and `can_begin_pattern` for an
/// invisible group). When it does not, the rule does not match there and
/// the next is tried: a `ty` is never an `expr`.
fn may_begin(want: &str, got: &str, inside: &[TokenTree]) -> bool {
    let expr = matches!(got, "expr" | "expr_2021");
    match want {
        "expr" | "expr_2021" => expr || matches!(got, "block" | "literal" | "path"),
        "ty" => matches!(got, "ty" | "path"),
        "vis" => matches!(got, "ty" | "path" | "vis"),
        "literal" => got == "literal" || (expr && begins_literal(inside)),
        "block" => expr || matches!(got, "block" | "stmt" | "literal"),
        "path" | "meta" => !matches!(got, "item" | "block" | "vis"),
        "pat" | "pat_param" => {
            expr || matches!(
                got,
                "literal" | "meta" | "pat" | "pat_param" | "path" | "ty"
            )
        }
        "item" | "stmt" => true,
        _ => false,
    }
}

/// Whether a forwarded fragment's tokens begin as a literal does: a
/// literal token, `true` or `false`, or `-`.
fn begins_literal(inside: &[TokenTree]) -> bool {
    match inside.first() {
        Some(TokenTree::Literal(_)) => true,
        Some(TokenTree::Ident(i)) => i == "true" || i == "false",
        Some(TokenTree::Punct(p)) => p.as_char() == '-',
        _ => false,
    }
}

/// Whether, once rustc's matcher tries a fragment of kind `want` on a
/// forwarded fragment of kind `got` holding `inside` ([`may_begin`]), its
/// parser reads that fragment: one of its own kind, or one the parser
/// re-reads (an `expr` reads a literal, a path or a block; a `literal`
/// reads an `expr` that is a literal; a `path` reads a `ty` that is a
/// path; a `vis` is empty before a type). Where it does not, rustc stops
/// with an error.
fn takes(want: &str, got: &str, inside: &[TokenTree]) -> bool {
    let whole = |kind: &str| tokens_taken(kind, inside) == Some(inside.len());
    want == got
        || match (want, got) {
            ("expr" | "expr_2021", "expr" | "expr_2021" | "literal" | "path" | "block")
            | ("ty", "path")
            | ("meta", "path")
            | ("vis", "ty" | "path")
            | ("pat" | "pat_param", "pat" | "pat_param" | "literal" | "path")
            | ("stmt", "expr" | "expr_2021" | "item" | "block" | "literal" | "path") => true,
            ("literal", "expr" | "expr_2021") => whole("literal"),
            ("path" | "meta", "ty") => whole("path"),
            _ => false,
        }
}

/// What a fragment of `kind` that matched `trees` binds: the trees, or for
/// a kind forwarded opaque, its tokens in invisible groups
/// (`Delimiter::None`, as rustc's own expansion writes a forwarded
/// fragment), as many nested as [`OPAQUE`] says for the kind. A forwarded
/// fragment matched again is unwrapped first, so it carries its latest
/// kind alone.
fn bound(kind: &str, trees: &[TokenTree]) -> Vec<TokenTree> {
    let Some(depth) = OPAQUE.iter().position(|k| *k == kind) else {
        return trees.to_vec();
    };
    let inside = match trees {
        [t] => opaque(t).map_or_else(|| trees.to_vec(), |(_, inside)| inside),
        _ => trees.to_vec(),
    };
    let span = trees.first().map(TokenTree::span);
    let mut ts: Vec<TokenTree> = inside;
    for _ in 0..=depth {
        let mut g = Group::new(Delimiter::None, ts.into_iter().collect());
        if let Some(s) = span {
            g.set_span(s);
        }
        ts = vec![TokenTree::Group(g)];
    }
    ts
}

/// How many of `input`'s leading trees a fragment of `kind` takes, as
/// rustc's parser would take them, or `None` when it cannot start there.
/// A fragment forwarded opaque ([`bound`]) starts one only of a kind
/// rustc tries on it ([`may_begin`]), and no literal token matches it;
/// one rustc tries but cannot read ([`takes`]) is an error, as in rustc.
fn fragment(kind: &str, input: &[TokenTree]) -> Result<Option<usize>, String> {
    if let Some((got, inside)) = input.first().and_then(opaque)
        && !TRANSPARENT.contains(&kind)
    {
        if !may_begin(kind, got, &inside) {
            return Ok(None);
        }
        if !takes(kind, got, &inside) {
            return Err(format!(
                "the counter does not read this forwarded `{got}` fragment as `{kind}` (it reads a `literal` from an `expr` that is a literal, a `path` from a `ty` that is a path, and a kind from its own kind); rewrite the macro to match it as `{got}`"
            ));
        }
        // A `vis` before a type is empty. A forwarded `vis` is the fragment
        // whole, as rustc's `parse_visibility` eats it, empty or not (syn's
        // `Visibility` would read the nested invisible groups of an empty
        // one as no tokens at all). A `block` and a `literal` are the
        // fragment too. The other kinds read on past it, as rustc's parser
        // does (`$e + 1` is one `expr`).
        match kind {
            "vis" if got != "vis" => return Ok(Some(0)),
            "vis" | "block" | "literal" => return Ok(Some(1)),
            _ => {}
        }
    }
    Ok(tokens_taken(kind, input))
}

/// How many of `input`'s leading trees a fragment of `kind` takes, read as
/// tokens ([`fragment`]).
fn tokens_taken(kind: &str, input: &[TokenTree]) -> Option<usize> {
    let first = input.first();
    match kind {
        // One token, as rustc's lexer makes them: `<<` and `'a` are one
        // each, though proc-macro2 writes each as two trees.
        "tt" => first.map(|_| token_len(input)),
        "ident" => match first {
            Some(TokenTree::Ident(i)) if i != "_" => Some(1),
            _ => None,
        },
        "lifetime" => match (first, input.get(1)) {
            (Some(TokenTree::Punct(p)), Some(TokenTree::Ident(_))) if p.as_char() == '\'' => {
                Some(2)
            }
            _ => None,
        },
        // A literal expression: a literal token, `true` or `false` (which
        // tokenize as identifiers), or a negated literal token.
        "literal" => match (first, input.get(1)) {
            (Some(TokenTree::Literal(_)), _) => Some(1),
            (Some(TokenTree::Ident(i)), _) if i == "true" || i == "false" => Some(1),
            (Some(TokenTree::Punct(p)), Some(TokenTree::Literal(_))) if p.as_char() == '-' => {
                Some(2)
            }
            _ => None,
        },
        "block" => match first {
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => Some(1),
            _ => None,
        },
        // rustc's matcher begins no `expr` at `let`, and no `expr_2021` at
        // `let`, `const` or `_` (`nonterminal_may_begin_with` in
        // `rustc_parse`; the Rust reference, "Metavariables", leaves const
        // blocks and `_` out of `expr_2021`): the rule does not match there
        // and the next is tried. syn's `Expr` reads all three. Below
        // edition 2024 an `expr` is an `expr_2021`.
        "expr" | "expr_2021"
            if matches!(first, Some(TokenTree::Ident(i))
                if i == "let"
                    || ((kind == "expr_2021" || EDITION.with(Cell::get) < 2024)
                        && (i == "const" || i == "_"))) =>
        {
            None
        }
        _ => {
            let ts: TokenStream = input.iter().cloned().collect();
            let left = |s: ParseStream| -> verus_syn::Result<usize> {
                match kind {
                    "vis" => drop(s.parse::<verus_syn::Visibility>()?),
                    "meta" => drop(s.parse::<verus_syn::Meta>()?),
                    "ty" => drop(s.parse::<verus_syn::Type>()?),
                    "expr" | "expr_2021" => drop(s.parse::<verus_syn::Expr>()?),
                    "path" => drop(s.parse::<verus_syn::Path>()?),
                    // Below edition 2021 a `pat` is a `pat_param`.
                    "pat" if EDITION.with(Cell::get) >= 2021 => {
                        drop(verus_syn::Pat::parse_multi_with_leading_vert(s)?)
                    }
                    "pat" | "pat_param" => drop(verus_syn::Pat::parse_single(s)?),
                    "stmt" => stmt(s)?,
                    _ => drop(s.parse::<verus_syn::Item>()?),
                }
                Ok(s.parse::<TokenStream>()?.into_iter().count())
            };
            let n = input.len().checked_sub(left.parse2(ts).ok()?)?;
            (n > 0 || kind == "vis").then_some(n)
        }
    }
}

/// Parses a `stmt` fragment as rustc does: a statement without its
/// trailing semicolon, but for an item's that the item requires (the Rust
/// reference, "Metavariables"). So it is an item, with any `;` the item
/// ends in; else a `let`, which stops before its `;`; else an expression.
fn stmt(s: ParseStream) -> verus_syn::Result<()> {
    if s.fork().parse::<verus_syn::Item>().is_ok() {
        return s.parse::<verus_syn::Item>().map(drop);
    }
    verus_syn::Attribute::parse_outer(s)?;
    if !s.peek(verus_syn::Token![let]) {
        return s.parse::<verus_syn::Expr>().map(drop);
    }
    s.parse::<verus_syn::Token![let]>()?;
    verus_syn::Pat::parse_single(s)?;
    if s.peek(verus_syn::Token![:]) {
        s.parse::<verus_syn::Token![:]>()?;
        s.parse::<verus_syn::Type>()?;
    }
    if s.peek(verus_syn::Token![=]) {
        s.parse::<verus_syn::Token![=]>()?;
        s.parse::<verus_syn::Expr>()?;
        if s.peek(verus_syn::Token![else]) {
            s.parse::<verus_syn::Token![else]>()?;
            s.parse::<verus_syn::Block>()?;
        }
    }
    Ok(())
}

/// Every metavariable `pats` binds.
fn vars(pats: &[Pat], out: &mut Vec<String>) {
    for p in pats {
        match p {
            Pat::Frag(n, _) => out.push(n.clone()),
            Pat::Group(_, inner) | Pat::Rep(inner, _, _) => vars(inner, out),
            Pat::Tok(_) => {}
        }
    }
}

/// Every way `pats` matches a prefix of `input`: how many trees it takes
/// and what it binds, the longest repetitions first.
fn matches(pats: &[Pat], input: &[TokenTree]) -> Result<Vec<(usize, Binds)>, String> {
    let Some((p, rest)) = pats.split_first() else {
        return Ok(vec![(0, Binds::new())]);
    };
    // Each way the first part matches: trees taken and bindings.
    let heads: Vec<(usize, Binds)> = match p {
        Pat::Tok(want) => match input.first() {
            Some(t) if same(t, want) => vec![(1, Binds::new())],
            _ => vec![],
        },
        Pat::Group(d, inner) => match input.first() {
            Some(TokenTree::Group(g)) if g.delimiter() == *d => {
                let inside: Vec<TokenTree> = g.stream().into_iter().collect();
                matches(inner, &inside)?
                    .into_iter()
                    .filter(|(n, _)| *n == inside.len())
                    .take(1)
                    .map(|(_, b)| (1, b))
                    .collect()
            }
            _ => vec![],
        },
        Pat::Frag(name, kind) => match fragment(kind, input)? {
            Some(n) => vec![(
                n,
                Binds::from([(name.clone(), Bind::One(bound(kind, &input[..n])))]),
            )],
            None => vec![],
        },
        Pat::Rep(inner, sep, op) => {
            let mut names = Vec::new();
            vars(inner, &mut names);
            // Each way to stop: trees taken and one binding per iteration.
            let mut ends: Vec<(usize, Vec<Binds>)> = Vec::new();
            let mut states: Vec<(usize, Vec<Binds>)> = vec![(0, Vec::new())];
            while !states.is_empty() {
                let mut next = Vec::new();
                for (at, iters) in states {
                    let room = match op {
                        '?' => iters.is_empty(),
                        _ => true,
                    };
                    if room {
                        let mut from = at;
                        let sep_ok = match (sep, iters.is_empty()) {
                            (Some(s), false) => match input.get(at..at + s.len()) {
                                Some(ts) if ts.iter().zip(s).all(|(t, w)| same(t, w)) => {
                                    from += s.len();
                                    true
                                }
                                _ => false,
                            },
                            _ => true,
                        };
                        if sep_ok {
                            for (n, b) in matches(inner, &input[from..])? {
                                if n == 0 {
                                    continue;
                                }
                                let mut more = iters.clone();
                                more.push(b);
                                next.push((from + n, more));
                            }
                        }
                    }
                    if *op != '+' || !iters.is_empty() {
                        ends.push((at, iters));
                    }
                }
                if ends.len() + next.len() > MAX_MATCHES {
                    return Err("a repetition matches in too many ways to search".into());
                }
                states = next;
            }
            ends.into_iter()
                .rev()
                .map(|(at, iters)| {
                    let binds = names
                        .iter()
                        .map(|n| {
                            let seq = iters
                                .iter()
                                .map(|b| b.get(n).cloned().unwrap_or(Bind::Seq(Vec::new())))
                                .collect();
                            (n.clone(), Bind::Seq(seq))
                        })
                        .collect();
                    (at, binds)
                })
                .collect()
        }
    };
    let mut out = Vec::new();
    for (n, b) in heads {
        for (m, mut rb) in matches(rest, &input[n..])? {
            rb.extend(b.clone());
            out.push((n + m, rb));
            if out.len() > MAX_MATCHES {
                return Err("a matcher matches in too many ways to search".into());
            }
        }
    }
    Ok(out)
}

/// `b` at repetition indices `idx`: a fragment, or the repetition still
/// to be indexed. A metavariable matched at a shallower depth than it is
/// used is the same fragment in each inner repetition.
fn at<'a>(mut b: &'a Bind, idx: &[usize]) -> Result<&'a Bind, String> {
    let mut d = 0;
    while let Bind::Seq(v) = b {
        let Some(&i) = idx.get(d) else { break };
        b = v.get(i).ok_or("a repetition indexed past its end")?;
        d += 1;
    }
    Ok(b)
}

/// The names of the metavariables `ts`, a transcriber, uses.
fn used(ts: TokenStream, out: &mut Vec<String>) {
    let mut it = ts.into_iter().peekable();
    while let Some(t) = it.next() {
        match t {
            TokenTree::Punct(p) if p.as_char() == '$' => {
                if let Some(TokenTree::Ident(v)) = it.peek() {
                    out.push(v.to_string());
                }
            }
            TokenTree::Group(g) => used(g.stream(), out),
            _ => {}
        }
    }
}

/// Writes transcriber `ts` with `binds`, inside the repetitions `idx`
/// indexes, to `out`.
fn transcribe(
    ts: TokenStream,
    binds: &Binds,
    idx: &mut Vec<usize>,
    out: &mut TokenStream,
) -> Result<(), String> {
    let trees: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while let Some(t) = trees.get(i).cloned() {
        i += 1;
        let next = trees.get(i).cloned();
        if matches!(t, TokenTree::Punct(ref p) if p.as_char() == '$') {
            i += 1;
        }
        match t {
            TokenTree::Punct(p) if p.as_char() == '$' => match next {
                Some(TokenTree::Ident(v)) if v == "crate" => {
                    out.extend([TokenTree::Ident(Ident::new("crate", v.span()))]);
                }
                Some(TokenTree::Ident(v)) => {
                    // A `$name` the rule does not bind is written through,
                    // as rustc writes it: a macro that writes a
                    // `macro_rules!` writes the new macro's metavariables
                    // so. Anywhere else, the expansion does not parse.
                    let Some(b) = binds.get(&v.to_string()) else {
                        out.extend([TokenTree::Punct(p), TokenTree::Ident(v)]);
                        continue;
                    };
                    match at(b, idx)? {
                        Bind::One(f) => out.extend(f.iter().cloned()),
                        Bind::Seq(_) => {
                            return Err(format!(
                                "uses `${v}` outside the repetition it matched in"
                            ));
                        }
                    }
                }
                Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                    let (sep, _, n) = repetition(&trees[i..])?;
                    i += n;
                    let mut names = Vec::new();
                    used(g.stream(), &mut names);
                    let mut len = None;
                    for n in &names {
                        let Some(b) = binds.get(n) else { continue };
                        if let Bind::Seq(v) = at(b, idx)? {
                            match len {
                                None => len = Some(v.len()),
                                Some(l) if l != v.len() => {
                                    return Err(format!(
                                        "a repetition's metavariables repeat {l} and {} times",
                                        v.len()
                                    ));
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    let len = len.ok_or("a repetition uses no metavariable that repeats")?;
                    for i in 0..len {
                        if i > 0
                            && let Some(s) = &sep
                        {
                            out.extend(s.iter().cloned());
                        }
                        idx.push(i);
                        transcribe(g.stream(), binds, idx, out)?;
                        idx.pop();
                    }
                }
                other => return Err(format!("`$` in a transcriber followed by {other:?}")),
            },
            TokenTree::Group(g) => {
                let mut inner = TokenStream::new();
                transcribe(g.stream(), binds, idx, &mut inner)?;
                let mut ng = Group::new(g.delimiter(), inner);
                ng.set_span(g.span());
                out.extend([TokenTree::Group(ng)]);
            }
            t => out.extend([t]),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> TokenStream {
        s.parse().expect("tokens")
    }

    /// `s` without whitespace: token spacing aside.
    fn flat(s: &str) -> String {
        s.split_whitespace().collect()
    }

    /// The expansion of `invocation` by the rules `def`, as text without
    /// whitespace.
    fn expand(def: &str, invocation: &str) -> Result<String, String> {
        Rules::parse(ts(def))?
            .expand(ts(invocation))
            .map(|t| flat(&t.to_string()))
    }

    #[test]
    fn nested_repetitions_with_separators_expand() {
        let def = "($(fn $name:ident / $z:ident($($arg:ident: $ty:ty),*) -> $ret:ty $body:block)*) => { $(pub fn $name($($arg: $ty),*) -> $ret $body pub fn $z($($arg: $ty),*) -> $ret { $name($($arg),*) })* };";
        let got = expand(
            def,
            "fn a / z_a(x: u32, y: *const u8) -> u32 { x } fn b / z_b() -> u8 { 0 }",
        )
        .expect("expands");
        let want = flat(
            "pub fn a(x: u32, y: *const u8) -> u32 { x } pub fn z_a(x: u32, y: *const u8) -> u32 { a(x, y) } pub fn b() -> u8 { 0 } pub fn z_b() -> u8 { b() }",
        );
        assert_eq!(got, want, "export!: got = {got}, wanted = {want}");
    }

    #[test]
    fn optional_parts_and_expressions_expand() {
        let def = "($(fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? => $c:ident, $fail:expr;)*) => { $(fn $name($($arg: $ty),*) $(-> $ret)? { g($fail, || $c($($arg),*)) })* };";
        let got = expand(
            def,
            "fn v() -> *const u8 => z_v, null(); fn e(err: i32) => z_e, 0;",
        )
        .expect("expands");
        let want = flat(
            "fn v() -> *const u8 { g(null(), || z_v()) } fn e(err: i32) { g(0, || z_e(err)) }",
        );
        assert_eq!(got, want, "forward!: got = {got}, wanted = {want}");
    }

    /// Rules are tried in order, and `$crate` is written `crate`.
    #[test]
    fn rules_are_tried_in_order() {
        let def = "(@inner $n:ident) => { fn $n() {} }; ($($n:ident),+ $(,)?) => { $($crate::m!(@inner $n);)+ };";
        let got = expand(def, "a, b,").expect("expands");
        let want = flat("crate::m!(@inner a); crate::m!(@inner b);");
        assert_eq!(got, want, "outer rule: got = {got}, wanted = {want}");
        let got = expand(def, "@inner c").expect("expands");
        let want = flat("fn c() {}");
        assert_eq!(got, want, "inner rule: got = {got}, wanted = {want}");
    }

    /// An `expr_2021` is not tried at `const` (a const block) or `_`, nor
    /// either `expr` at `let`, so the fallback rule writes the function, as
    /// rustc's matcher selects it; an `expr` takes `const` and `_`.
    #[test]
    fn an_expr_2021_is_no_const_block_or_underscore() {
        let def = |kind: &str| {
            format!(
                "($e:{kind}) => {{ fn first() {{}} }}; ($($t:tt)*) => {{ fn fallback() {{}} }};"
            )
        };
        for (kind, invocation, want) in [
            ("expr_2021", "1 + 2", "fnfirst(){}"),
            ("expr_2021", "const { 1 }", "fnfallback(){}"),
            ("expr_2021", "_", "fnfallback(){}"),
            ("expr_2021", "let x = 1", "fnfallback(){}"),
            ("expr", "const { 1 }", "fnfirst(){}"),
            ("expr", "_", "fnfirst(){}"),
            ("expr", "let x = 1", "fnfallback(){}"),
        ] {
            let got = expand(&def(kind), invocation).expect("expands");
            assert_eq!(
                got, want,
                "{kind} {invocation}: got = {got}, wanted = {want}"
            );
        }
        // Below edition 2024 an `expr` is an `expr_2021`.
        set_edition(2021);
        for invocation in ["const { 1 }", "_"] {
            let got = expand(&def("expr"), invocation).expect("expands");
            assert_eq!(
                got, "fnfallback(){}",
                "edition 2021, expr {invocation}: got = {got}"
            );
        }
        // Below edition 2021 a `pat` is a `pat_param`: no top-level `|`.
        for (edition, kind, want) in [
            (2021, "pat", "fnfirst(){}"),
            (2018, "pat", "fnfallback(){}"),
            (2015, "pat", "fnfallback(){}"),
            (2021, "pat_param", "fnfallback(){}"),
        ] {
            set_edition(edition);
            let got = expand(&def(kind), "A | B").expect("expands");
            assert_eq!(
                got, want,
                "edition {edition}, {kind} A | B: got = {got}, wanted = {want}"
            );
            let got = expand(&def(kind), "A").expect("expands");
            assert_eq!(
                got, "fnfirst(){}",
                "edition {edition}, {kind} A: got = {got}, wanted = fnfirst(){{}}"
            );
        }
        set_edition(2024);
        // An empty first rule would hide the fallback's `unproved`.
        let got = expand(
            "($e:expr_2021) => {}; ($t:tt) => { fn unproved() {} };",
            "_",
        )
        .expect("expands");
        assert_eq!(got, "fnunproved(){}", "m!(_): got = {got}");
    }

    /// Doc comments match `$(#[$attr:meta])*`, and visibility may be empty.
    #[test]
    fn attributes_and_visibility_match() {
        let def = "($(#[$a:meta])* $v:vis struct $n:ident;) => { $(#[$a])* $v fn $n() {} };";
        let got = expand(def, "/// docs\n#[inline] struct S;").expect("expands");
        assert!(
            got.contains("fnS()") && got.contains("inline") && got.contains("docs"),
            "meta and empty vis: got = {got}, wanted = the attributes and fn S"
        );
        let got = expand(def, "pub(crate) struct T;").expect("expands");
        assert!(
            got.contains("pub(crate)fnT()"),
            "pub(crate): got = {got}, wanted = pub(crate) fn T()"
        );
    }

    /// `true` and `false` are literals, as a negated number is; an
    /// identifier is not.
    #[test]
    fn booleans_are_literals() {
        let def = "($l:literal) => { fn f() -> T { $l } };";
        for (invocation, want) in [
            ("true", Some("fnf()->T{true}")),
            ("false", Some("fnf()->T{false}")),
            ("-1", Some("fnf()->T{-1}")),
            ("x", None),
        ] {
            let got = expand(def, invocation).ok();
            assert_eq!(
                got.as_deref(),
                want,
                "{invocation}: got = {got:?}, wanted = {want:?}"
            );
        }
    }

    /// A `stmt` fragment is a statement without its trailing semicolon, but
    /// for the one an item requires: `let x = 1` matches `$s:stmt` and
    /// `let x = 1;` does not, while `struct S;` matches with its `;`.
    #[test]
    fn statements_match_without_their_semicolon() {
        let def = "($s:stmt) => { fn f() { $s; } };";
        for (invocation, want) in [
            ("let x = 1", Some("fnf(){letx=1;}")),
            ("let x: u8 = 1", Some("fnf(){letx:u8=1;}")),
            ("g(1)", Some("fnf(){g(1);}")),
            ("struct S;", Some("fnf(){structS;;}")),
            ("let x = 1;", None),
            ("g(1);", None),
        ] {
            let got = expand(def, invocation).ok();
            assert_eq!(
                got.as_deref(),
                want,
                "{invocation}: got = {got:?}, wanted = {want:?}"
            );
        }
    }

    /// A `ty` forwarded to another macro is opaque and keeps its kind:
    /// neither `inner!`'s literal `(u8)` rule nor its `$e:expr` rule
    /// matches it, and its `$t:ty` rule does (the Rust reference,
    /// "Forwarding a matched fragment"). A `tt` is forwarded as its token,
    /// which the literal rule matches, and a forwarded `literal` is an
    /// `expr`.
    #[test]
    fn forwarded_fragments_keep_their_kind() {
        let inner = Rules::parse(ts(
            "(u8) => { lit }; ($e:expr) => { expr }; ($t:ty) => { fn f() {} };",
        ))
        .expect("rules");
        for (outer, input, want) in [
            ("($t:ty) => { inner!($t); };", "u8", "fnf(){}"),
            ("($t:tt) => { inner!($t); };", "u8", "lit"),
            ("($l:literal) => { inner!($l); };", "1", "expr"),
        ] {
            let written: Vec<TokenTree> = Rules::parse(ts(outer))
                .and_then(|r| r.expand(ts(input)))
                .expect("outer expands")
                .into_iter()
                .collect();
            let Some(TokenTree::Group(args)) = written.get(2) else {
                panic!("{outer}: got = {written:?}, wanted = inner!(..);");
            };
            let got = inner
                .expand(args.stream())
                .map(|t| flat(&t.to_string()))
                .expect("inner expands");
            assert_eq!(got, want, "{outer}: got = {got}, wanted = {want}");
        }
    }

    /// Of a forwarded fragment of another kind, the counter's `literal`
    /// reads an `expr` that is a literal (`1`, `-1`, `true`), and its
    /// `path` a `ty` that is a path; one it is tried on and does not read
    /// (a `ty` that is not a path, as a `path`) is an error that says to
    /// rewrite the macro. Each is `outer` forwarding `input` to `inner`.
    #[test]
    fn forwarded_fragments_are_read_as_rustc_reads_them() {
        for (inner, outer, input, want) in [
            (
                "($l:literal) => { fn f() {} }; ($e:expr) => {};",
                "($e:expr) => { inner!($e); };",
                "1",
                Ok("fnf(){}"),
            ),
            (
                "($l:literal) => { fn f() {} }; ($e:expr) => {};",
                "($e:expr) => { inner!($e); };",
                "-1",
                Ok("fnf(){}"),
            ),
            (
                "($l:literal) => { fn f() {} }; ($e:expr) => {};",
                "($e:expr) => { inner!($e); };",
                "true",
                Ok("fnf(){}"),
            ),
            (
                "($l:literal) => { fn f() {} }; ($e:expr) => { e };",
                "($e:expr) => { inner!($e); };",
                "x + 1",
                Ok("e"),
            ),
            (
                "($p:path) => { fn f() {} }; ($t:ty) => {};",
                "($t:ty) => { inner!($t); };",
                "u8",
                Ok("fnf(){}"),
            ),
            (
                "($p:path) => { fn f() {} }; ($t:ty) => {};",
                "($t:ty) => { inner!($t); };",
                "&u8",
                Err("the counter does not read this forwarded `ty` fragment as `path`"),
            ),
        ] {
            let written: Vec<TokenTree> = Rules::parse(ts(outer))
                .and_then(|r| r.expand(ts(input)))
                .expect("outer expands")
                .into_iter()
                .collect();
            let Some(TokenTree::Group(args)) = written.get(2) else {
                panic!("{outer}: got = {written:?}, wanted = inner!(..);");
            };
            let got = Rules::parse(ts(inner))
                .expect("rules")
                .expand(args.stream())
                .map(|t| flat(&t.to_string()));
            match (&got, want) {
                (Ok(g), Ok(w)) if g == w => {}
                (Err(e), Err(w)) if e.contains(w) => {}
                _ => panic!("{input}: got = {got:?}, wanted = {want:?}"),
            }
        }
    }

    /// `tt` takes one token as rustc's lexer makes them (`<<`, `->`, a
    /// lifetime), not one proc-macro2 tree, and a separator is one token
    /// (`::`).
    #[test]
    fn a_tt_and_a_separator_are_one_token() {
        let shift = "($name:ident, $op:tt) => { fn $name() { a $op b } }; ($($rest:tt)*) => {};";
        for (def, invocation, want) in [
            (shift, "shl, <<", "fnshl(){a<<b}"),
            (shift, "ge, >=", "fnge(){a>=b}"),
            (shift, "sa, <<=", "fnsa(){a<<=b}"),
            ("($l:tt, $r:tt) => { $l $r };", "'a, ->", "'a->"),
            (
                "($($seg:ident)::*) => { use $($seg)::*; };",
                "a::b::c",
                "usea::b::c;",
            ),
            (
                "($($n:ident)+=*) => { $(fn $n() {})* };",
                "a += b",
                "fna(){}fnb(){}",
            ),
            (
                "($($n:ident)*=*) => { $(fn $n() {})* };",
                "a *= b",
                "fna(){}fnb(){}",
            ),
        ] {
            let got = expand(def, invocation).expect("expands");
            assert_eq!(got, want, "{invocation}: got = {got}, wanted = {want}");
        }
    }

    /// A `$name` the rule does not bind is written through, as rustc
    /// writes it, so a macro can write a `macro_rules!`.
    #[test]
    fn an_unbound_metavariable_is_written_through() {
        let def = "($n:ident) => { macro_rules! $n { ($x:expr) => { $x }; } };";
        let got = expand(def, "m").expect("expands");
        let want = "macro_rules!m{($x:expr)=>{$x};}";
        assert_eq!(got, want, "got = {got}, wanted = {want}");
    }

    /// Negative: no rule matches, a definition that does not parse, an
    /// unknown kind, unequal repetitions, and a `+` matched none.
    #[test]
    fn what_cannot_expand_is_an_error() {
        for (def, invocation, want) in [
            ("($n:ident) => { fn $n() {} };", "1", "no rule matches"),
            ("($n:ident) => { fn $n() {} };", "a b", "no rule matches"),
            ("($n:ident) => fn $n() {};", "a", "transcriber"),
            ("($n:foo) => {};", "a", "no such fragment kind"),
            (
                "($($a:ident)* ; $($b:ident)*) => { $($a $b)* };",
                "x y ; z",
                "repeat 2 and 1 times",
            ),
            ("($($a:ident),+) => {};", "", "no rule matches"),
            ("($($a:ident)*) => { $a };", "x", "outside the repetition"),
        ] {
            match expand(def, invocation) {
                Ok(got) => panic!("{def} on {invocation:?}: got = {got}, wanted = an error"),
                Err(e) => assert!(
                    e.contains(want),
                    "{def} on {invocation:?}: got = {e}, wanted = {want:?}"
                ),
            }
        }
    }
}
