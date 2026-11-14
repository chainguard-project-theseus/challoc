//! The compile-inputs check (`[inputs]` in the configuration; pcre3's
//! verus/theorems/CD1.md): no function reachable from the configured
//! roots (pcre3's `Regex::new` and `serialize::encode_as`), or, with
//! `lazy`, from a lazy builder (a function a `get_or_init` calls),
//! reads an input other than its arguments unless the configured table
//! names it with a reason; with `lazy`, nor does a `get_or_init`
//! initializer in place (a closure's body, or for an argument that is not
//! a closure written there, such as a local `init`, the whole body that
//! holds it), whatever function holds it.
//! A configured root that matches no function of the configured crates
//! fails the check, since no walk starts from it.
//!
//! A hidden input is a read of:
//! - a `static` whose type can change after it is made (an atomic, a
//!   `Cell`, `RefCell`, `OnceLock`, `OnceCell`, `LazyLock`, `Mutex`,
//!   `RwLock` or `UnsafeCell`, or a `static mut`). A static of plain data
//!   (the Unicode and character tables) is a constant and is not one.
//! - a `thread_local!` static.
//! - the environment (`env::` or `std::env`, an imported `std::env`
//!   function, or a `use .. as` name of `env` or of one of its functions,
//!   through chains of renames).
//! - a `HashMap`, `HashSet` or `RandomState`, whose iteration order is
//!   seeded per process.
//!
//! Only the configured crates (those the roots run in) are searched.
//! Calls are resolved by name, over-approximating:
//! - `f(..)` is every free function named `f`;
//! - `Type::f(..)` is every method `f` of `Type` (or of the caller's type,
//!   for `Self`), and `module::f(..)` every free function `f`;
//! - `x.f(..)` is every method named `f`, of any type;
//! - a function passed by name (a whole argument that is a name) is every
//!   function of that name, whatever local shares the name: the check does
//!   not reason about bindings, so it fails closed;
//! - the calls the compiler inserts: `?` (every `from`), a `for` loop
//!   (every `into_iter` and `next`), `Default` (every `default`), and the
//!   `drop`, `deref` and `deref_mut` any body may run.
//!
//! A call reaches only the candidates whose parameters its argument count
//! fits, unless a closure's `|` or a `<` among its arguments leaves the
//! count unknown, when it reaches them all.
//!
//! A call through a function value held in a field or a variable, or
//! through a trait object of another crate, is not seen (CD1.md).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// A call in a function body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// `f(..)`, with its argument count ([`args`]).
    Free(String, Arity),
    /// `x.f(..)`.
    Method(String, Arity),
    /// `q::f(..)`: the segment before `f`, and `f`.
    Path(String, String, Arity),
    /// A call the compiler inserts (`?`'s `From::from`, `Default::default`):
    /// every method `f` of these crates, with or without a receiver.
    Any(String, Arity),
}

/// The arguments a [`Site`] passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arity {
    /// A call with this many arguments.
    Known(usize),
    /// A call whose count is unsure ([`args`]); it fits every candidate.
    Unknown,
    /// No call: the function is passed by name ([`by_name`]), so its
    /// address is taken. It fits every candidate.
    ByName,
}

/// The arguments in a call's parentheses: the commas at its top level, or
/// unknown when a closure's `|..|` or a `<` there makes the count unsure.
/// A closure's parameters (`|a, b|`) have commas that are not the call's.
/// A `<` may open generic arguments (`make::<u8, u16>()`), whose commas
/// are not the call's, or be a comparison (`a < b, c > d`), which
/// nesting cannot tell apart from one.
fn args(g: &proc_macro2::Group) -> Arity {
    let toks: Vec<TokenTree> = g.stream().into_iter().collect();
    if toks.is_empty() {
        return Arity::Known(0);
    }
    if toks
        .iter()
        .any(|t| matches!(t, TokenTree::Punct(p) if matches!(p.as_char(), '|' | '<')))
    {
        return Arity::Unknown;
    }
    let commas = toks
        .iter()
        .filter(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
        .count();
    let trailing = matches!(toks.last(), Some(TokenTree::Punct(p)) if p.as_char() == ',');
    Arity::Known(commas + 1 - usize::from(trailing))
}

/// What one function's body names, for the check.
#[derive(Clone, Debug, Default)]
pub(crate) struct Body {
    pub(crate) sites: Vec<Site>,
    pub(crate) idents: BTreeSet<String>,
    /// The body names `env::` (`std::env`).
    pub(crate) env: bool,
    /// The body names `Instant` or `SystemTime` (a clock read).
    pub(crate) clock: bool,
    /// The body takes a pointer's address (`addr()`, or `as usize` where it
    /// handles a raw pointer).
    pub(crate) address: bool,
    /// The calls in the argument of a `get_or_init` (the builder of a part
    /// the pattern keeps), which the check takes as roots.
    pub(crate) lazy: Vec<Site>,
    /// What each `get_or_init`'s argument reads in place (a closure's
    /// body; for any other argument, the whole body that holds it), which
    /// the check reads with `lazy` though no root reaches the function
    /// that holds it.
    pub(crate) inline: Vec<Body>,
}

/// A `static` item: its name, whether it is a hidden input whatever its
/// type (`static mut`, a `thread_local!`), and its type's tokens.
#[derive(Clone, Debug)]
pub(crate) struct Static {
    pub(crate) name: String,
    pub(crate) forced: bool,
    pub(crate) ty: TokenStream,
}

/// The structs, enums and type aliases of the crates: a type's name and
/// the fields' (or alias's) types of every definition of that name, in
/// any crate or module. A name defined twice is plain only when every
/// definition is, so the map fails closed where names collide.
pub(crate) type Types = BTreeMap<String, Vec<TokenStream>>;

/// The names a `type` alias or a `use .. as` rename introduces, each with
/// the names it stands for (the target's last segment). A name renamed in
/// several places stands for all of its targets. One of `std::env`'s
/// function names imported under its own name stands for the segment it
/// is imported from with it (`use process::var;` makes `var` stand for
/// `process::var`), which no function is named, so that the environment
/// read is found through a rename of `env` ([`findings`]).
pub(crate) type Aliases = BTreeMap<String, BTreeSet<String>>;

/// One function, as the check sees it.
pub(crate) struct Func<'a> {
    pub(crate) krate: &'a str,
    pub(crate) file: &'a Path,
    pub(crate) line: usize,
    /// The type or trait of a method (its first identifier), or empty.
    pub(crate) owner: String,
    pub(crate) name: &'a str,
    /// Its parameters, the receiver aside, and whether it has one.
    pub(crate) params: usize,
    pub(crate) receiver: bool,
    pub(crate) body: &'a Body,
}

/// A read the check reports: the function (an index into the list), the
/// input, and the chain of calls that reaches it.
#[derive(Debug)]
pub(crate) struct Finding {
    pub(crate) func: usize,
    pub(crate) input: String,
    pub(crate) chain: Vec<String>,
}

/// Where the walk starts and which crates it follows calls through
/// (`[inputs]` in the configuration).
pub(crate) struct Roots<'a> {
    pub(crate) crates: Vec<&'a str>,
    pub(crate) roots: &'a [crate::config::Root],
    pub(crate) lazy: bool,
}

/// Types whose value can change after the static holding them is made.
const INTERIOR: [&str; 13] = [
    "AtomicBool",
    "AtomicU8",
    "AtomicU32",
    "AtomicU64",
    "AtomicUsize",
    "Cell",
    "RefCell",
    "OnceLock",
    "OnceCell",
    "LazyLock",
    "Mutex",
    "RwLock",
    "UnsafeCell",
];

/// `std::env`'s functions, read through a `use std::env::{..}` import.
const ENV_FNS: [&str; 9] = [
    "var",
    "var_os",
    "vars",
    "vars_os",
    "args",
    "args_os",
    "current_dir",
    "temp_dir",
    "home_dir",
];

/// Types whose iteration order is seeded per process.
const SEEDED: [&str; 3] = ["HashMap", "HashSet", "RandomState"];

/// Whether a static of type `ty` (its tokens) is a hidden input. It fails
/// closed: a static is hidden unless its type is plain data, primitives in
/// arrays, slices, tuples and `&'static` references ([`plain`]), so a
/// struct or alias that holds an atomic or a cell is hidden too.
pub(crate) fn interior(ty: &TokenStream, plain_set: &BTreeSet<String>) -> bool {
    let mut names = BTreeSet::new();
    let mut env = false;
    collect_idents(ty.clone(), &mut names, &mut env);
    INTERIOR.iter().any(|t| names.contains(*t))
        || names.iter().any(|n| n.starts_with("Atomic"))
        || !plain(ty.clone(), plain_set)
}

/// The std types that hold no interior mutability, so a static of them
/// (of plain data) cannot change.
const STD_PLAIN: [&str; 8] = [
    "Option",
    "Vec",
    "Box",
    "String",
    "Arc",
    "PhantomData",
    "Range",
    "RangeInclusive",
];

/// Adds one definition of `name` (its fields', or an alias's, types) to
/// `types`. Every definition adds: a name is plain only when all its
/// definitions are.
pub(crate) fn add_type(
    types: &mut Types,
    name: String,
    fields: impl IntoIterator<Item = TokenStream>,
) {
    types.entry(name).or_default().extend(fields);
}

/// The types of `types` that are plain data: a struct, enum or alias whose
/// every field is ([`plain`]), found by a fixpoint from none. A type the
/// crates do not define is not plain unless it is a primitive or in
/// [`STD_PLAIN`].
pub(crate) fn plain_types(types: &Types) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    loop {
        let mut grew = false;
        for (name, fields) in types {
            if !set.contains(name) && fields.iter().all(|f| !interior(f, &set)) {
                set.insert(name.clone());
                grew = true;
            }
        }
        if !grew {
            return set;
        }
    }
}

/// The primitive types a plain static may hold.
const PRIMITIVES: [&str; 17] = [
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "bool",
    "char", "str", "f32", "f64",
];

/// The owner of a call through a qualified path (`<T as Trait>::f`), whose
/// type the check does not resolve: every method `f`.
const QUALIFIED: &str = "<>";

/// Whether `ty` is plain data: primitives, the plain std types and the
/// crates' plain types (`plain_set`), in arrays (whatever their length
/// expression), slices, tuples, generics and shared `'static` references.
fn plain(ty: TokenStream, plain_set: &BTreeSet<String>) -> bool {
    let toks: Vec<TokenTree> = ty.into_iter().collect();
    let mut length = false;
    for (i, t) in toks.iter().enumerate() {
        if length {
            continue;
        }
        match t {
            TokenTree::Group(g) => {
                if !plain(g.stream(), plain_set) {
                    return false;
                }
            }
            TokenTree::Ident(id) => {
                let s = id.to_string();
                let segment =
                    matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == ':');
                if !(PRIMITIVES.contains(&s.as_str())
                    || STD_PLAIN.contains(&s.as_str())
                    || plain_set.contains(&s)
                    || s == "static"
                    || segment)
                {
                    return false;
                }
            }
            TokenTree::Punct(p) => match p.as_char() {
                ';' => length = true,
                '&' | '\'' | ',' | '<' | '>' | ':' => {}
                _ => return false,
            },
            TokenTree::Literal(_) => {}
        }
    }
    true
}

/// The base name of a type's tokens: its first identifier past `&`, `mut`
/// and `dyn`.
pub(crate) fn base(ty: &str) -> String {
    ty.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|w| !w.is_empty() && *w != "mut" && *w != "dyn")
        .unwrap_or("")
        .to_string()
}

fn collect_idents(tokens: TokenStream, out: &mut BTreeSet<String>, env: &mut bool) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => collect_idents(g.stream(), out, env),
            TokenTree::Ident(id) => {
                let s = id.to_string();
                let path_follows = matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == ':')
                    && matches!(toks.get(i + 2), Some(TokenTree::Punct(p)) if p.as_char() == ':');
                if s == "env" && path_follows {
                    *env = true;
                }
                out.insert(s);
            }
            _ => {}
        }
    }
}

/// The calls and names in a body's tokens. A by-name site is kept even
/// when a local has its name: the check does not reason about bindings, so
/// a local named like a function costs an extra edge, never a missing one.
pub(crate) fn body(tokens: TokenStream) -> Body {
    let mut b = calls(tokens.clone());
    collect_lazy(tokens.clone(), &mut b.lazy);
    collect_inline(tokens.clone(), &tokens, &mut b.inline);
    b
}

/// [`body`] but for the `get_or_init` initializers: the calls, the
/// inserted ones too, and the names.
fn calls(tokens: TokenStream) -> Body {
    let mut b = Body::default();
    collect_idents(tokens.clone(), &mut b.idents, &mut b.env);
    collect_sites(tokens.clone(), &mut b.sites);
    let mut sites = std::mem::take(&mut b.sites);
    collect_inserted(tokens, &mut sites, &mut b);
    b.sites = sites;
    // Every body may drop a value and auto-deref one, which call `drop`,
    // `deref` and `deref_mut` with no syntax of their own.
    for m in ["drop", "deref", "deref_mut"] {
        b.sites.push(Site::Method(m.to_string(), Arity::Known(0)));
    }
    b
}

/// What a body's syntax shows of the calls the compiler or std makes for
/// it: `?`, `for`, `Default`, operators, indexing, sorting and comparing,
/// formatting and the conversions `into`, `try_into` and `collect` make.
#[derive(Default)]
struct Inserted {
    question: bool,
    for_loop: bool,
    default: bool,
    eq: bool,
    order: bool,
    arith: bool,
    bits: bool,
    index: bool,
    fmt: bool,
    into: bool,
    try_into: bool,
    collect: bool,
    clock: bool,
    address: bool,
    pointer: bool,
}

/// The macros that format their arguments (`Display` or `Debug`'s `fmt`).
const FORMATS: [&str; 14] = [
    "format",
    "write",
    "writeln",
    "print",
    "println",
    "eprint",
    "eprintln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "unreachable",
    "todo",
];

/// The methods that call `cmp` or `partial_cmp` on the items.
const ORDERS: [&str; 9] = [
    "sort",
    "sort_unstable",
    "sort_by_key",
    "max",
    "min",
    "binary_search",
    "max_by_key",
    "min_by_key",
    "dedup",
];

/// The calls a body's syntax shows the compiler or std inserting, each
/// followed to every method of its name in the crates (any type):
/// - `?`: `from`; a `for` loop: `into_iter` and `next`; naming `Default`,
///   `default` or `unwrap_or_default`: `default`;
/// - `==` and `!=`: `eq` and `ne`; `<`, `>` and the sorting and `max`/`min`
///   methods ([`ORDERS`]): `cmp`, `partial_cmp`, `lt`, `le`, `gt` and `ge`;
/// - `+ - * / %` and the bit operators, with their `=` forms: `add` and the
///   rest (`neg` and `not` for the unary ones);
/// - `x[..]`: `index` and `index_mut`;
/// - a formatting macro ([`FORMATS`]) or `to_string`: `fmt`;
/// - `into`, `try_into` and `collect`: `from`, `try_from` and `from_iter`.
fn collect_inserted(tokens: TokenStream, out: &mut Vec<Site>, body: &mut Body) {
    let mut seen = Inserted::default();
    scan_inserted(tokens, &mut seen);
    let mut push = |names: &[&str], args: usize, any: bool| {
        for n in names {
            out.push(if any {
                Site::Any((*n).to_string(), Arity::Known(args))
            } else {
                Site::Method((*n).to_string(), Arity::Known(args))
            });
        }
    };
    if seen.question {
        push(&["from"], 1, true);
    }
    if seen.for_loop {
        push(&["into_iter", "next"], 0, false);
    }
    if seen.default {
        push(&["default"], 0, true);
    }
    if seen.eq {
        push(&["eq", "ne"], 1, false);
    }
    if seen.order {
        push(&["cmp", "partial_cmp", "lt", "le", "gt", "ge"], 1, false);
    }
    if seen.arith {
        push(
            &[
                "add",
                "sub",
                "mul",
                "div",
                "rem",
                "add_assign",
                "sub_assign",
                "mul_assign",
                "div_assign",
                "rem_assign",
            ],
            1,
            false,
        );
        push(&["neg"], 0, false);
    }
    if seen.bits {
        push(
            &[
                "bitand",
                "bitor",
                "bitxor",
                "shl",
                "shr",
                "bitand_assign",
                "bitor_assign",
                "bitxor_assign",
                "shl_assign",
                "shr_assign",
            ],
            1,
            false,
        );
        push(&["not"], 0, false);
    }
    if seen.index {
        push(&["index", "index_mut"], 1, false);
    }
    if seen.fmt {
        push(&["fmt"], 1, false);
    }
    if seen.into {
        push(&["from"], 1, true);
    }
    if seen.try_into {
        push(&["try_from"], 1, true);
    }
    if seen.collect {
        push(&["from_iter"], 1, true);
    }
    body.clock = seen.clock;
    body.address = seen.address && seen.pointer;
}

fn scan_inserted(tokens: TokenStream, seen: &mut Inserted) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => {
                // `x[..]` (not an array `[..]` or an attribute `#[..]`).
                if g.delimiter() == Delimiter::Bracket
                    && i >= 1
                    && matches!(
                        toks.get(i - 1),
                        Some(TokenTree::Ident(_)) | Some(TokenTree::Group(_))
                    )
                {
                    seen.index = true;
                }
                scan_inserted(g.stream(), seen);
            }
            TokenTree::Punct(p) => match p.as_char() {
                '?' => seen.question = true,
                '=' if punct(i + 1, '=') => seen.eq = true,
                '!' if punct(i + 1, '=') => seen.eq = true,
                '<' | '>' => seen.order = true,
                '+' | '-' | '/' | '%' => seen.arith = true,
                '*' => {
                    seen.arith = true;
                    if matches!(toks.get(i + 1), Some(TokenTree::Ident(k)) if k == "const" || k == "mut")
                    {
                        seen.pointer = true;
                    }
                }
                '&' | '|' | '^' => seen.bits = true,
                _ => {}
            },
            TokenTree::Ident(id) => {
                let s = id.to_string();
                let bang = punct(i + 1, '!');
                match s.as_str() {
                    "for" => seen.for_loop = true,
                    "Default" | "default" | "unwrap_or_default" => seen.default = true,
                    "to_string" => seen.fmt = true,
                    "into" => seen.into = true,
                    "try_into" => seen.try_into = true,
                    "collect" => seen.collect = true,
                    "Instant" | "SystemTime" => seen.clock = true,
                    "addr" | "expose_provenance" => seen.address = true,
                    "as_ptr" | "as_mut_ptr" | "from_ref" | "from_mut" | "addr_of"
                    | "addr_of_mut" => seen.pointer = true,
                    "as" if matches!(toks.get(i + 1), Some(TokenTree::Ident(k)) if k == "usize" || k == "u64" || k == "isize") =>
                    {
                        seen.address = true;
                    }
                    _ => {}
                }
                if bang && FORMATS.contains(&s.as_str()) {
                    seen.fmt = true;
                }
                if ORDERS.contains(&s.as_str()) {
                    seen.order = true;
                }
            }
            _ => {}
        }
    }
}

fn collect_lazy(tokens: TokenStream, out: &mut Vec<Site>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Ident(id) if id == "get_or_init" => {
                if let Some(TokenTree::Group(g)) = toks.get(i + 1)
                    && g.delimiter() == Delimiter::Parenthesis
                {
                    sites_in(g.stream(), true, out);
                }
            }
            TokenTree::Group(g) => collect_lazy(g.stream(), out),
            _ => {}
        }
    }
}

/// The bodies of the arguments of each `get_or_init` in `tokens`, a part
/// of `holder`, the body of the function that holds them. An argument
/// that is not a closure written in place (`get_or_init(init)`, a local
/// closure or a value made elsewhere in `holder`), or a closure that
/// calls a name a pattern in `holder` binds ([`bound_names`]: `let init =
/// ..; .. move || init()`), has its body
/// somewhere the argument's tokens do not show, so it stands for all of
/// `holder`.
fn collect_inline(tokens: TokenStream, holder: &TokenStream, out: &mut Vec<Body>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Ident(id) if id == "get_or_init" => {
                if let Some(TokenTree::Group(g)) = toks.get(i + 1)
                    && g.delimiter() == Delimiter::Parenthesis
                {
                    let mut bound = BTreeSet::new();
                    bound_names(holder.clone(), &mut bound);
                    let arg = body(g.stream());
                    let local = arg
                        .sites
                        .iter()
                        .any(|s| matches!(s, Site::Free(n, _) if bound.contains(n)));
                    out.push(if is_closure(g.stream()) && !local {
                        arg
                    } else {
                        calls(holder.clone())
                    });
                }
            }
            TokenTree::Group(g) => collect_inline(g.stream(), holder, out),
            _ => {}
        }
    }
}

/// The names `tokens` bind, at any depth, by every pattern a body can
/// hold: a `let`'s (`let mut NAME`, `let (a, b)`, `let S { x, .. }`, an
/// `if let` or `while let` too), up to its type or its `=`; a `for`'s, up
/// to its `in`; a closure's parameters, between its `|`s; and a `match`
/// arm's, back from its `=>` to the arm before. A path, field or guard
/// name is taken too, as is whatever sits between two `|`s of an `|`
/// expression: a name taken that binds nothing can only make an argument
/// stand for more of its holder.
fn bound_names(tokens: TokenStream, out: &mut BTreeSet<String>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    let take = |p: &TokenTree, out: &mut BTreeSet<String>| {
        if matches!(p, TokenTree::Ident(_) | TokenTree::Group(_)) {
            pattern_names(TokenStream::from(p.clone()), out);
        }
    };
    // The closing `|` of the last parameter list read.
    let mut closed = None;
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Ident(l) if l == "let" => {
                for (k, p) in toks.iter().enumerate().skip(i + 1) {
                    match p {
                        TokenTree::Punct(p) if matches!(p.as_char(), '=' | ';') => break,
                        TokenTree::Punct(p)
                            if p.as_char() == ':' && !punct(k - 1, ':') && !punct(k + 1, ':') =>
                        {
                            break;
                        }
                        _ => take(p, out),
                    }
                }
            }
            // `for<'a>` is a bound's, not a loop's.
            TokenTree::Ident(f) if f == "for" && !punct(i + 1, '<') => {
                for p in &toks[i + 1..] {
                    match p {
                        TokenTree::Ident(n) if n == "in" => break,
                        _ => take(p, out),
                    }
                }
            }
            // A closure's parameters, between its `|`s; `||` has none, and
            // the `|` that closes a list opens none.
            TokenTree::Punct(p)
                if p.as_char() == '|'
                    && !punct(i + 1, '|')
                    && !(i > 0 && punct(i - 1, '|'))
                    && closed != Some(i) =>
            {
                for (k, p) in toks.iter().enumerate().skip(i + 1) {
                    match p {
                        TokenTree::Punct(p) if p.as_char() == '|' => {
                            closed = Some(k);
                            break;
                        }
                        _ => take(p, out),
                    }
                }
            }
            // A `match` arm's pattern and guard, back to the arm before:
            // its `,`, or its block (a `{ .. }` right after `=>`). Braces
            // in the pattern (`Pair { a: init }`) or guard (`if { .. }`)
            // are read.
            TokenTree::Punct(p) if p.as_char() == '=' && punct(i + 1, '>') => {
                for k in (0..i).rev() {
                    match &toks[k] {
                        TokenTree::Punct(p) if p.as_char() == ',' => break,
                        TokenTree::Group(g)
                            if g.delimiter() == Delimiter::Brace
                                && k >= 2
                                && punct(k - 1, '>')
                                && punct(k - 2, '=') =>
                        {
                            break;
                        }
                        p => take(p, out),
                    }
                }
            }
            _ => {}
        }
        if let TokenTree::Group(g) = t {
            bound_names(g.stream(), out);
        }
    }
}

/// Every name in pattern `tokens`, at any depth, but `mut` and `ref`.
fn pattern_names(tokens: TokenStream, out: &mut BTreeSet<String>) {
    for t in tokens {
        match t {
            TokenTree::Ident(n) if n != "mut" && n != "ref" => {
                out.insert(n.to_string());
            }
            TokenTree::Group(g) => pattern_names(g.stream(), out),
            _ => {}
        }
    }
}

/// Whether `tokens` are a closure expression: `|..| ..`, `|| ..`, or
/// either after `move`.
fn is_closure(tokens: TokenStream) -> bool {
    let mut toks = tokens.into_iter();
    match toks.next() {
        Some(TokenTree::Punct(p)) => p.as_char() == '|',
        Some(TokenTree::Ident(m)) if m == "move" => {
            matches!(toks.next(), Some(TokenTree::Punct(p)) if p.as_char() == '|')
        }
        _ => false,
    }
}

fn collect_sites(tokens: TokenStream, out: &mut Vec<Site>) {
    sites_in(tokens, false, out);
}

/// The calls in `tokens`, the stream of a parenthesised group when `paren`
/// (where a whole argument that is a name is a function passed by name).
fn sites_in(tokens: TokenStream, paren: bool, out: &mut Vec<Site>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    for (i, t) in toks.iter().enumerate() {
        if let TokenTree::Group(g) = t {
            sites_in(g.stream(), g.delimiter() == Delimiter::Parenthesis, out);
            continue;
        }
        let TokenTree::Ident(id) = t else { continue };
        let after = super::past_turbofish(&toks, i);
        let n_args = match toks.get(after) {
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => args(g),
            _ => {
                if paren && let Some(site) = by_name(&toks, i, after) {
                    out.push(site);
                }
                continue;
            }
        };
        let name = id.to_string();
        if i >= 1 && punct(i - 1, '.') {
            out.push(Site::Method(name, n_args));
        } else if i >= 2 && punct(i - 1, ':') && punct(i - 2, ':') {
            // The segment before `::`, past a turbofish's generics. A
            // `<..>` that no name or `::` precedes is a qualified path
            // (`<T as Trait>::f`, `<T>::f`), whose type the check does not
            // know: it is [`QUALIFIED`].
            let mut k = i - 3;
            let mut depth = 0usize;
            loop {
                match toks.get(k) {
                    Some(TokenTree::Punct(p)) if p.as_char() == '>' => depth += 1,
                    Some(TokenTree::Punct(p)) if p.as_char() == '<' => {
                        depth = depth.saturating_sub(1);
                        let named = match toks.get(k.wrapping_sub(1)) {
                            Some(TokenTree::Ident(_)) => true,
                            Some(TokenTree::Punct(p)) => p.as_char() == ':',
                            _ => false,
                        };
                        if depth == 0 && !named {
                            out.push(Site::Path(QUALIFIED.to_string(), name, n_args));
                            break;
                        }
                    }
                    Some(TokenTree::Ident(q)) if depth == 0 => {
                        out.push(Site::Path(q.to_string(), name, n_args));
                        break;
                    }
                    None => break,
                    _ => {}
                }
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        } else if !matches!(toks.get(i.wrapping_sub(1)), Some(TokenTree::Ident(k)) if k == "fn") {
            out.push(Site::Free(name, n_args));
        }
    }
}

/// A function passed by name: the name at `i` (or the end of a path to it)
/// makes up a whole argument, after `(` or `,` and before `,` or `)`, and
/// starts with a lowercase letter, as a function's does. Its arity is
/// [`Arity::ByName`].
fn by_name(toks: &[TokenTree], i: usize, after: usize) -> Option<Site> {
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    if after < toks.len() && !punct(after, ',') {
        return None;
    }
    let TokenTree::Ident(id) = &toks[i] else {
        return None;
    };
    let name = id.to_string();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    let mut start = i;
    while start >= 3
        && punct(start - 1, ':')
        && punct(start - 2, ':')
        && matches!(toks.get(start - 3), Some(TokenTree::Ident(_)))
    {
        start -= 3;
    }
    if start > 0 && !punct(start - 1, ',') {
        return None;
    }
    if start < i {
        let TokenTree::Ident(q) = &toks[i - 3] else {
            return None;
        };
        Some(Site::Path(q.to_string(), name, Arity::ByName))
    } else {
        Some(Site::Free(name, Arity::ByName))
    }
}

/// The statics declared in `tokens` (a `thread_local!` block, or a body):
/// `static NAME: TYPE`. Every static of a `thread_local!` is a hidden
/// input.
pub(crate) fn statics_in(tokens: TokenStream, thread_local: bool, out: &mut Vec<Static>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        if let TokenTree::Group(g) = t {
            statics_in(g.stream(), thread_local, out);
            continue;
        }
        let TokenTree::Ident(kw) = t else { continue };
        if kw != "static" {
            continue;
        }
        let mut k = i + 1;
        let mutable = matches!(toks.get(k), Some(TokenTree::Ident(m)) if m == "mut");
        if mutable {
            k += 1;
        }
        let Some(TokenTree::Ident(name)) = toks.get(k) else {
            continue;
        };
        if !matches!(toks.get(k + 1), Some(TokenTree::Punct(p)) if p.as_char() == ':') {
            continue;
        }
        let ty: TokenStream = toks[k + 2..]
            .iter()
            .take_while(
                |t| !matches!(t, TokenTree::Punct(p) if p.as_char() == '=' || p.as_char() == ';'),
            )
            .cloned()
            .collect();
        out.push(Static {
            name: name.to_string(),
            forced: thread_local || mutable,
            ty,
        });
    }
}

/// Adds to `out` the names the `use .. as` renames and `type` aliases in
/// `tokens` introduce, at item level or in a function body: `use a::B as C;`
/// makes `C` stand for `B`, and `type C<T> = a::B<T>;` makes `C` stand for
/// `B`. An alias whose target has no name (a tuple, a slice) stands for `""`,
/// which no type owns, so calls through it follow every method of the name.
pub(crate) fn aliases_in(tokens: TokenStream, out: &mut Aliases) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let semi = |from: usize| {
        toks[from..]
            .iter()
            .position(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ';'))
            .map_or(toks.len(), |n| from + n)
    };
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Group(g) => aliases_in(g.stream(), out),
            TokenTree::Ident(kw) if kw == "use" => {
                let end = semi(i + 1);
                renames(&toks[i + 1..end], out);
                i = end;
            }
            TokenTree::Ident(kw) if kw == "type" => {
                let Some(TokenTree::Ident(name)) = toks.get(i + 1) else {
                    i += 1;
                    continue;
                };
                let end = semi(i + 2);
                let Some(eq) = toks[i + 2..end]
                    .iter()
                    .position(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == '='))
                else {
                    i = end;
                    continue;
                };
                let target = &toks[i + 2 + eq + 1..end];
                out.entry(name.to_string())
                    .or_default()
                    .insert(named(target));
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
}

/// The renames in a `use` tree's tokens: each `B as C`, in nested groups too.
fn renames(toks: &[TokenTree], out: &mut Aliases) {
    renames_under(toks, None, out);
}

/// [`renames`] in a `use` group whose path ends in `parent` (`env` for
/// `std::env::{..}`): a `self as name` there renames `parent`. A `self`
/// whose parent the tokens do not show is kept as `self`. One of
/// `std::env`'s functions imported under its own name (`use std::env::var;`,
/// `use process::{var, args};`, `use cfg::*;`) stands for the segment it is
/// imported from with it (`env::var`, `process::var`): whether that segment
/// is `env` or a rename of it is known only once every crate's renames are
/// ([`findings`]).
fn renames_under(toks: &[TokenTree], parent: Option<&str>, out: &mut Aliases) {
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    // The segment the name at `i` is imported from: the one before its
    // `::`, or the group's when it begins a group or follows a `,`.
    let from = |i: usize| {
        if i >= 3 && punct(i - 1, ':') && punct(i - 2, ':') {
            match &toks[i - 3] {
                TokenTree::Ident(s) => Some(s.to_string()),
                _ => None,
            }
        } else if i == 0 || punct(i - 1, ',') {
            parent.map(str::to_owned)
        } else {
            None
        }
    };
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Ident(n)
                if ENV_FNS.contains(&n.to_string().as_str())
                    && !matches!(toks.get(i + 1), Some(TokenTree::Ident(a)) if a == "as") =>
            {
                if let Some(m) = from(i) {
                    out.entry(n.to_string())
                        .or_default()
                        .insert(format!("{m}::{n}"));
                }
            }
            TokenTree::Punct(p) if p.as_char() == '*' => {
                if let Some(m) = from(i) {
                    for n in ENV_FNS {
                        out.entry(n.to_owned())
                            .or_default()
                            .insert(format!("{m}::{n}"));
                    }
                }
            }
            TokenTree::Group(g) => {
                // The segment before the group's `::`.
                let before = match (
                    i.checked_sub(3).map(|p| &toks[p]),
                    i.checked_sub(1).map(|p| &toks[p]),
                ) {
                    (Some(TokenTree::Ident(seg)), Some(TokenTree::Punct(p)))
                        if p.as_char() == ':' =>
                    {
                        Some(seg.to_string())
                    }
                    _ => None,
                };
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                renames_under(&inner, before.as_deref(), out);
            }
            TokenTree::Ident(kw) if kw == "as" => {
                if let (Some(TokenTree::Ident(from)), Some(TokenTree::Ident(to))) =
                    (i.checked_sub(1).and_then(|p| toks.get(p)), toks.get(i + 1))
                    && to != "_"
                {
                    let from = match (from.to_string().as_str(), parent) {
                        ("self", Some(p)) => p.to_owned(),
                        (f, _) => f.to_owned(),
                    };
                    out.entry(to.to_string()).or_default().insert(from);
                }
            }
            _ => {}
        }
    }
}

/// The type a type's tokens name: the last segment of its leading path,
/// past `&`, `mut`, `dyn` and lifetimes; `""` when it starts with no path.
fn named(toks: &[TokenTree]) -> String {
    let mut last = String::new();
    let mut i = 0;
    while let Some(t) = toks.get(i) {
        match t {
            TokenTree::Punct(p) if p.as_char() == '&' => {}
            TokenTree::Punct(p) if p.as_char() == '\'' => i += 1,
            TokenTree::Ident(id) if id == "mut" || id == "dyn" => {}
            TokenTree::Ident(id) => {
                last = id.to_string();
                let path = matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == ':')
                    && matches!(toks.get(i + 2), Some(TokenTree::Punct(p)) if p.as_char() == ':');
                if !path {
                    break;
                }
                i += 2;
            }
            TokenTree::Punct(p) if p.as_char() == ':' => {}
            _ => break,
        }
        i += 1;
    }
    last
}

/// Whether CD1.md's table allows `f` to read `input`: a row naming the
/// input and the file in backquotes and quoting `fn NAME`.
pub(crate) fn allowed(cd1: &str, f: &Func, input: &str) -> bool {
    row(cd1, f.file, f.name, input)
}

fn row(cd1: &str, file: &Path, name: &str, input: &str) -> bool {
    let file = format!("`{}`", file.display());
    let quote = format!("`fn {name}`");
    let input = format!("`{input}`");
    cd1.lines().any(|l| {
        l.starts_with('|') && l.contains(&file) && l.contains(&quote) && l.contains(&input)
    })
}

/// The functions reachable from `roots`: each configured root (a function
/// `name`, of `owner` when one is given, in a file whose path ends in
/// `file`, such as pcre3's `Regex::new` in crates/pcre3/src/regex.rs),
/// and with `lazy` every function that calls `get_or_init` (a part built
/// on first use and kept). Each reachable function's index maps to its
/// caller on a shortest chain (`None` for a root).
pub(crate) fn reachable(
    fns: &[Func],
    traits: &BTreeSet<String>,
    aliases: &Aliases,
    roots: &Roots,
) -> BTreeMap<usize, Option<usize>> {
    let root = |i: usize| roots.roots.iter().any(|r| is_root(&fns[i], r));
    walk(fns, traits, aliases, &roots.crates, &root, roots.lazy)
}

fn is_root(f: &Func, r: &crate::config::Root) -> bool {
    f.file.ends_with(&r.file) && r.owner.as_ref().is_none_or(|o| f.owner == *o) && f.name == r.name
}

/// The functions of `roots.crates` (indices into `fns`) each configured
/// root matches, in the configuration's order. A root that matches none
/// (misspelled, or moved to another file) starts no walk, so the check
/// fails on it rather than pass having inspected nothing from it.
pub(crate) fn resolved_roots(fns: &[Func], roots: &Roots) -> Vec<Vec<usize>> {
    roots
        .roots
        .iter()
        .map(|r| {
            (0..fns.len())
                .filter(|&i| roots.crates.contains(&fns[i].krate) && is_root(&fns[i], r))
                .collect()
        })
        .collect()
}

/// The functions of `crates` reachable from the ones `root` picks (by
/// index into `fns`), and
/// with `lazy` from every function that calls `get_or_init`, by the call
/// resolution the module's documentation describes. Each reachable
/// function's index maps to its caller on a shortest chain (`None` for a
/// root).
pub(crate) fn walk(
    fns: &[Func],
    traits: &BTreeSet<String>,
    aliases: &Aliases,
    crates: &[&str],
    root: &dyn Fn(usize) -> bool,
    lazy: bool,
) -> BTreeMap<usize, Option<usize>> {
    let compile: Vec<usize> = (0..fns.len())
        .filter(|&i| crates.contains(&fns[i].krate))
        .collect();
    let mut by_free: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut by_method: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut by_owned: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for &i in &compile {
        let f = &fns[i];
        if f.owner.is_empty() {
            by_free.entry(f.name).or_default().push(i);
        } else {
            by_method.entry(f.name).or_default().push(i);
            by_owned
                .entry((f.owner.as_str(), f.name))
                .or_default()
                .push(i);
        }
    }
    let empty = Vec::new();
    // A call of `args` arguments can be a function of `params`, or (for
    // `Type::f(x, ..)`) a method taking its receiver as the first.
    let fits = |t: &Func, args: Arity, ufcs: bool| match args {
        Arity::Unknown | Arity::ByName => true,
        Arity::Known(a) => a == t.params || (ufcs && t.receiver && a == t.params + 1),
    };
    // `module::f(..)`: the free functions `f` of a file named for the
    // module (`module.rs`, or `module/mod.rs`), or every free `f` when
    // none is (a path through `crate`, `super` or another crate).
    let in_module = |t: &Func, q: &str| {
        t.file.file_stem().is_some_and(|s| s == q)
            || (t.file.file_stem().is_some_and(|s| s == "mod")
                && t.file
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|d| d == q))
    };
    let resolve = |f: &Func, s: &Site| -> Vec<usize> {
        match s {
            Site::Free(n, a) => std::iter::once(n)
                .chain(aliases.get(n).into_iter().flatten())
                .flat_map(|m| by_free.get(m.as_str()).unwrap_or(&empty).iter().copied())
                .filter(|&t| fits(&fns[t], *a, false))
                .collect(),
            Site::Method(n, a) => by_method
                .get(n.as_str())
                .unwrap_or(&empty)
                .iter()
                .copied()
                .filter(|&t| fns[t].receiver && fits(&fns[t], *a, false))
                .collect(),
            Site::Any(n, a) => by_method
                .get(n.as_str())
                .unwrap_or(&empty)
                .iter()
                .copied()
                .filter(|&t| fits(&fns[t], *a, false))
                .collect(),
            Site::Path(q, n, a) => {
                let owner = if q == "Self" {
                    f.owner.as_str()
                } else {
                    q.as_str()
                };
                if owner.starts_with(|c: char| c.is_ascii_uppercase())
                    || owner == QUALIFIED
                    || PRIMITIVES.contains(&owner)
                {
                    // A type's methods `f`. A trait of these crates, or an
                    // owner with no method `f` here (a type parameter such
                    // as `Unit::f`, a primitive such as `u8::f`, a qualified
                    // `<T as Trait>::f`, another crate's type) names every
                    // method `f`, failing closed.
                    // An alias or a rename stands for its targets; a target with no
                    // method `f` here (another crate's type, or one the check
                    // cannot resolve) names every method `f`, failing closed.
                    let mut pool: BTreeSet<usize> = BTreeSet::new();
                    let all = by_method.get(n.as_str()).unwrap_or(&empty);
                    match by_owned.get(&(owner, n.as_str())) {
                        Some(owned) if !traits.contains(owner) => pool.extend(owned),
                        _ if aliases.contains_key(owner) && !traits.contains(owner) => {}
                        _ => pool.extend(all),
                    }
                    for target in aliases.get(owner).into_iter().flatten() {
                        match by_owned.get(&(target.as_str(), n.as_str())) {
                            Some(owned) if !traits.contains(target) => pool.extend(owned),
                            _ => pool.extend(all),
                        }
                    }
                    pool.into_iter()
                        .filter(|&t| fits(&fns[t], *a, true))
                        .collect()
                } else {
                    let all: Vec<usize> = by_free
                        .get(n.as_str())
                        .unwrap_or(&empty)
                        .iter()
                        .copied()
                        .filter(|&t| fits(&fns[t], *a, false))
                        .collect();
                    // A renamed module (`use super::assert as groups;`) is also
                    // the files named for its targets; the name itself stays,
                    // since renames are pooled across crates and another may
                    // have a real `groups` module.
                    let modules: Vec<&str> = std::iter::once(q)
                        .chain(aliases.get(q).into_iter().flatten())
                        .map(String::as_str)
                        .collect();
                    let named: Vec<usize> = all
                        .iter()
                        .copied()
                        .filter(|&t| modules.iter().any(|m| in_module(&fns[t], m)))
                        .collect();
                    if named.is_empty() { all } else { named }
                }
            }
        }
    };
    let mut parent: BTreeMap<usize, Option<usize>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for &i in &compile {
        if root(i) {
            parent.insert(i, None);
            queue.push_back(i);
        }
    }
    for &i in compile.iter().filter(|_| lazy) {
        // Each initializer's calls: those it names (a builder passed by
        // name too) and every call its body makes in place, the ones the
        // compiler inserts (an operator's `add`, a `drop`) included.
        let b = fns[i].body;
        for s in b
            .lazy
            .iter()
            .chain(b.inline.iter().flat_map(|inl| inl.sites.iter()))
        {
            for t in resolve(&fns[i], s) {
                if let std::collections::btree_map::Entry::Vacant(e) = parent.entry(t) {
                    e.insert(None);
                    queue.push_back(t);
                }
            }
        }
    }
    while let Some(i) = queue.pop_front() {
        let f = &fns[i];
        for s in &f.body.sites {
            for t in resolve(f, s) {
                if let std::collections::btree_map::Entry::Vacant(e) = parent.entry(t) {
                    e.insert(Some(i));
                    queue.push_back(t);
                }
            }
        }
    }
    parent
}

/// Every hidden input a reachable function reads, with its chain.
pub(crate) fn findings(
    fns: &[Func],
    statics: &[Static],
    traits: &BTreeSet<String>,
    types: &Types,
    aliases: &Aliases,
    roots: &Roots,
) -> Vec<Finding> {
    let plain_set = plain_types(types);
    let mut hidden: BTreeSet<&str> = statics
        .iter()
        .filter(|s| s.forced || interior(&s.ty, &plain_set))
        .map(|s| s.name.as_str())
        .collect();
    // A hidden static read under a `use .. as` name is hidden by that name too.
    let renamed: Vec<&str> = aliases
        .iter()
        .filter(|(_, targets)| targets.iter().any(|t| hidden.contains(t.as_str())))
        .map(|(name, _)| name.as_str())
        .collect();
    hidden.extend(renamed);
    let parent = reachable(fns, traits, aliases, roots);
    let mut out = Vec::new();
    // A free call of one of `std::env`'s functions that the crates do not
    // define reads the environment through a `use std::env::{..}`.
    let free: BTreeSet<&str> = fns
        .iter()
        .filter(|f| f.owner.is_empty() && roots.crates.contains(&f.krate))
        .map(|f| f.name)
        .collect();
    // The names a `use .. as` gives `std::env` or one of its functions
    // (`use std::env as process;`, `use std::env::var as get;`), through
    // chains of renames. The alias table keeps no paths, so a rename of
    // any `env` or `var` counts: it fails closed.
    let env_mod = renamed_to(aliases, &["env"]);
    let mut env_fns = renamed_to(aliases, &ENV_FNS);
    // And the names a `use` imports under their own name from `env` or one
    // of its renames (`use process::var;`, recorded as `process::var`).
    env_fns.extend(
        aliases
            .iter()
            .filter(|(n, targets)| {
                targets.iter().any(|t| {
                    t.strip_suffix(n.as_str())
                        .and_then(|m| m.strip_suffix("::"))
                        .is_some_and(|m| m == "env" || env_mod.contains(m))
                })
            })
            .map(|(n, _)| n.as_str()),
    );
    let inputs_of = |b: &Body| -> Vec<String> {
        let mut inputs: Vec<String> = b
            .idents
            .iter()
            .filter(|n| hidden.contains(n.as_str()) || SEEDED.contains(&n.as_str()))
            .cloned()
            .collect();
        // A call of a `std::env` function, by its name or a rename, free
        // (`get(..)`) or by a path (`reads::var(..)`, `self::get(..)`,
        // through a module that re-exports or globs it), called or passed
        // by name; a name the crates define for a free function of their
        // own is theirs, unless a `use` imports it under that name from
        // `env` or a rename of it ([`renames_under`]).
        let env_call = |n: &str| (ENV_FNS.contains(&n) && !free.contains(n)) || env_fns.contains(n);
        let imported_env = b.sites.iter().any(|s| match s {
            Site::Free(n, _) | Site::Path(_, n, _) => env_call(n),
            _ => false,
        });
        let renamed_env = b.idents.iter().any(|n| env_mod.contains(n.as_str()));
        if b.env || imported_env || renamed_env {
            inputs.push("std::env".to_string());
        }
        if b.clock {
            inputs.push("clock".to_string());
        }
        if b.address {
            inputs.push("pointer address".to_string());
        }
        inputs
    };
    let at_ = |k: usize| {
        format!(
            "{} ({}:{})",
            fns[k].name,
            fns[k].file.display(),
            fns[k].line
        )
    };
    for &i in parent.keys() {
        for input in inputs_of(fns[i].body) {
            let mut chain = vec![at_(i)];
            let mut at = parent[&i];
            while let Some(p) = at {
                chain.push(at_(p));
                at = parent[&p];
            }
            chain.reverse();
            out.push(Finding {
                func: i,
                input,
                chain,
            });
        }
    }
    // With `lazy`, what a `get_or_init` initializer reads in place (a
    // closure's body, not a builder it calls, which the walk roots) is
    // read on first use, by whichever caller gets there first: each read
    // is the calling function's, though no root reaches it.
    if roots.lazy {
        for i in (0..fns.len()).filter(|i| roots.crates.contains(&fns[*i].krate)) {
            if parent.contains_key(&i) {
                continue;
            }
            let found: BTreeSet<String> = fns[i].body.inline.iter().flat_map(inputs_of).collect();
            for input in found {
                out.push(Finding {
                    func: i,
                    input,
                    chain: vec![format!("{} (a get_or_init initializer)", at_(i))],
                });
            }
        }
    }
    out
}

/// The names `aliases` maps, through any chain of renames, to one of
/// `names`.
fn renamed_to<'a>(aliases: &'a Aliases, names: &[&str]) -> BTreeSet<&'a str> {
    let mut out: BTreeSet<&str> = BTreeSet::new();
    loop {
        let more: Vec<&str> = aliases
            .iter()
            .filter(|(n, targets)| {
                !out.contains(n.as_str())
                    && targets
                        .iter()
                        .any(|t| names.contains(&t.as_str()) || out.contains(t.as_str()))
            })
            .map(|(n, _)| n.as_str())
            .collect();
        if more.is_empty() {
            return out;
        }
        out.extend(more);
    }
}

/// The rows of CD1.md's allowlist table that allow no finding: a stale
/// row, which the check reports so that the table stays the list of what
/// compile reads.
pub(crate) fn stale_rows<'a>(cd1: &'a str, fns: &[Func], found: &[Finding]) -> Vec<&'a str> {
    let start = cd1.find("### Hidden inputs").unwrap_or(cd1.len());
    let table = &cd1[start..];
    let table = table.split("\n### ").next().unwrap_or("");
    table
        .lines()
        .filter(|l| l.starts_with("| `") && l.contains("`fn "))
        .filter(|l| {
            !found
                .iter()
                .any(|x| row(l, fns[x.func].file, fns[x.func].name, &x.input))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use verus_syn::{ImplItem, Item, parse_file};

    /// The functions and statics of `src` (an `impl Regex` holds the root;
    /// [`reads`] places them in crates/pcre3/src/regex.rs).
    /// A function's owner, name, body, parameters and receiver.
    type Scanned = (String, String, Body, usize, bool);

    /// The structs of `items`, in inline modules too, each definition
    /// adding its fields under its bare name as verus/count's scan does.
    fn collect_types(items: &[Item], types: &mut Types) {
        for item in items {
            match item {
                Item::Struct(s) => add_type(
                    types,
                    s.ident.to_string(),
                    s.fields
                        .iter()
                        .map(|f| verus_syn::__private::ToTokens::to_token_stream(&f.ty)),
                ),
                Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        collect_types(inner, types);
                    }
                }
                _ => {}
            }
        }
    }

    fn scan(src: &str) -> (Vec<Scanned>, Vec<Static>, Types, Aliases) {
        let file = parse_file(src).unwrap();
        let mut fns = Vec::new();
        let mut statics = Vec::new();
        let mut types = Types::new();
        let mut aliases = Aliases::new();
        let tokens = |b: &verus_syn::Block| -> TokenStream {
            b.stmts
                .iter()
                .map(verus_syn::__private::ToTokens::to_token_stream)
                .collect()
        };
        for item in &file.items {
            match item {
                Item::Fn(f) => {
                    fns.push((
                        String::new(),
                        f.sig.ident.to_string(),
                        body(tokens(&f.block)),
                        f.sig.inputs.len(),
                        false,
                    ));
                    statics_in(tokens(&f.block), false, &mut statics);
                    aliases_in(tokens(&f.block), &mut aliases);
                }
                Item::Impl(i) => {
                    let owner = base(
                        &verus_syn::__private::ToTokens::to_token_stream(&*i.self_ty).to_string(),
                    );
                    for ii in &i.items {
                        if let ImplItem::Fn(f) = ii {
                            aliases_in(tokens(&f.block), &mut aliases);
                            fns.push((
                                owner.clone(),
                                f.sig.ident.to_string(),
                                body(tokens(&f.block)),
                                f.sig.inputs.len() - usize::from(f.sig.receiver().is_some()),
                                f.sig.receiver().is_some(),
                            ));
                        }
                    }
                }
                Item::Static(s) => statics.push(Static {
                    name: s.ident.to_string(),
                    forced: false,
                    ty: verus_syn::__private::ToTokens::to_token_stream(&*s.ty),
                }),
                Item::Struct(_) => collect_types(std::slice::from_ref(item), &mut types),
                // A module's types, and its `use`s, as the walk reads an
                // inline module's.
                Item::Mod(_) => {
                    collect_types(std::slice::from_ref(item), &mut types);
                    aliases_in(
                        verus_syn::__private::ToTokens::to_token_stream(item),
                        &mut aliases,
                    );
                }
                Item::Use(_) | Item::Type(_) => aliases_in(
                    verus_syn::__private::ToTokens::to_token_stream(item),
                    &mut aliases,
                ),
                Item::Macro(m) if m.mac.path.is_ident("thread_local") => {
                    statics_in(m.mac.tokens.clone(), true, &mut statics);
                }
                _ => {}
            }
        }
        (fns, statics, types, aliases)
    }

    /// pcre3's roots, which the fixtures use: `Regex::new`,
    /// `serialize::encode_as` and the lazy builders.
    fn pcre3_roots() -> &'static [crate::config::Root] {
        static ROOTS: std::sync::OnceLock<Vec<crate::config::Root>> = std::sync::OnceLock::new();
        ROOTS.get_or_init(|| {
            vec![
                crate::config::Root {
                    file: "crates/pcre3/src/regex.rs".into(),
                    owner: Some("Regex".into()),
                    name: "new".into(),
                },
                crate::config::Root {
                    file: "crates/pcre3/src/serialize.rs".into(),
                    owner: None,
                    name: "encode_as".into(),
                },
            ]
        })
    }

    fn roots() -> Roots<'static> {
        Roots {
            crates: vec!["pcre3", "pcre3-syntax", "pcre3-unicode"],
            roots: pcre3_roots(),
            lazy: true,
        }
    }

    fn reads(src: &str) -> Vec<(String, String)> {
        let (raw, statics, types, aliases) = scan(src);
        let path = Path::new("crates/pcre3/src/regex.rs");
        let fns: Vec<Func> = raw
            .iter()
            .map(|(owner, name, b, params, receiver)| Func {
                krate: "pcre3",
                file: path,
                line: 0,
                owner: owner.clone(),
                name,
                params: *params,
                receiver: *receiver,
                body: b,
            })
            .collect();
        findings(&fns, &statics, &BTreeSet::new(), &types, &aliases, &roots())
            .into_iter()
            .map(|f| (fns[f.func].name.to_string(), f.input))
            .collect()
    }

    /// The roots come from the configuration: another root, and no lazy
    /// builders, change what is reached.
    #[test]
    fn roots_are_configured() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { } }
            fn other(p: u8) { N.load(Relaxed); }
            fn later(c: &OnceLock<u8>) { c.get_or_init(|| build()); }
            fn build() -> u8 { N.load(Relaxed); 0 }";
        let (raw, statics, types, aliases) = scan(src);
        let path = Path::new("crates/z/src/lib.rs");
        let fns: Vec<Func> = raw
            .iter()
            .map(|(owner, name, b, params, receiver)| Func {
                krate: "z",
                file: path,
                line: 0,
                owner: owner.clone(),
                name,
                params: *params,
                receiver: *receiver,
                body: b,
            })
            .collect();
        let other = [crate::config::Root {
            file: "z/src/lib.rs".into(),
            owner: None,
            name: "other".into(),
        }];
        let run = |lazy: bool, crates: Vec<&'static str>| -> Vec<String> {
            let r = Roots {
                crates,
                roots: &other,
                lazy,
            };
            findings(&fns, &statics, &BTreeSet::new(), &types, &aliases, &r)
                .into_iter()
                .map(|f| fns[f.func].name.to_string())
                .collect()
        };
        assert_eq!(run(false, vec!["z"]), ["other"]);
        assert_eq!(run(true, vec!["z"]), ["other", "build"]);
        // A crate the configuration does not name is not walked.
        assert_eq!(run(true, vec!["y"]), Vec::<String>::new());
    }

    #[test]
    fn an_alias_owner_resolves_to_its_target() {
        for alias in ["type Shown = Inner;", "type Shown<T> = crate::m::Inner<T>;"] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                {alias}
                impl Regex {{ fn new(p: u8) {{ Shown::step(p) }} }}
                impl Inner {{ fn step(p: u8) {{ N.load(Relaxed); }} }}
                impl Other {{ fn step(p: u8) {{ N.load(Relaxed); }} }}"
            );
            assert_eq!(
                reads(&src),
                vec![("step".to_string(), "N".to_string())],
                "{alias}"
            );
        }
    }

    #[test]
    fn a_renamed_owner_resolves_to_its_target() {
        for (item, call) in [
            ("use crate::m::Inner as Shown;", "Shown::step(p)"),
            ("use crate::m::{Other, Inner as Shown};", "Shown::step(p)"),
            ("", "use crate::m::Inner as Shown; Shown::step(p)"),
        ] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                {item}
                impl Regex {{ fn new(p: u8) {{ {call} }} }}
                impl Inner {{ fn step(p: u8) {{ N.load(Relaxed); }} }}
                impl Other {{ fn step(p: u8) {{ N.load(Relaxed); }} }}"
            );
            assert_eq!(
                reads(&src),
                vec![("step".to_string(), "N".to_string())],
                "{item} {call}"
            );
        }
    }

    #[test]
    fn an_unresolved_alias_owner_follows_every_method_of_the_name() {
        // `Foreign` has no methods here; the alias fails closed to every `step`.
        for item in [
            "type Shown = ext::Foreign;",
            "use ext::Foreign as Shown;",
            "type Shown = (u8, u8);",
        ] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                {item}
                impl Regex {{ fn new(p: u8) {{ Shown::step(p) }} }}
                impl Inner {{ fn step(p: u8) {{ N.load(Relaxed); }} }}"
            );
            assert_eq!(
                reads(&src),
                vec![("step".to_string(), "N".to_string())],
                "{item}"
            );
        }
    }

    #[test]
    fn a_renamed_function_is_followed() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            use crate::m::helper as go;
            impl Regex { fn new(p: u8) { go(p) } }
            fn helper(p: u8) { N.load(Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "N".to_string())]);
    }

    #[test]
    fn a_renamed_module_is_the_file_of_its_target() {
        // `groups::step` names vm/assert.rs's `step` through the rename,
        // as well as the `step` of the file named groups.rs.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            use super::assert as groups;
            impl Regex { fn new(p: u8) { groups::step(p) } }
            fn step(p: u8) { N.load(Relaxed); }
            fn step(p: u8) {}";
        let (raw, statics, types, aliases) = scan(src);
        let files = [
            Path::new("crates/pcre3/src/regex.rs"),
            Path::new("crates/pcre3/src/vm/assert.rs"),
            Path::new("crates/pcre3-syntax/src/parse/groups.rs"),
        ];
        let fns: Vec<Func> = raw
            .iter()
            .zip(files)
            .map(|((owner, name, b, params, receiver), file)| Func {
                krate: "pcre3",
                file,
                line: 0,
                owner: owner.clone(),
                name,
                params: *params,
                receiver: *receiver,
                body: b,
            })
            .collect();
        let got: Vec<(String, String)> =
            findings(&fns, &statics, &BTreeSet::new(), &types, &aliases, &roots())
                .into_iter()
                .map(|f| (fns[f.func].file.display().to_string(), f.input))
                .collect();
        assert_eq!(
            got,
            vec![("crates/pcre3/src/vm/assert.rs".to_string(), "N".to_string())]
        );
    }

    #[test]
    fn a_renamed_static_is_hidden_under_its_rename() {
        let src = "static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            use crate::ids::NEXT_ID as ID;
            impl Regex { fn new(p: u8) { helper(p) } }
            fn helper(p: u8) { ID.fetch_add(1, Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "ID".to_string())]);
    }

    #[test]
    fn a_reachable_changing_static_is_reported() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { helper(p) } }
            fn helper(p: u8) { N.fetch_add(1, Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "N".to_string())]);
    }

    #[test]
    fn plain_data_and_unreached_reads_are_not() {
        let src = "static TABLE: [u8; 2] = [0, 1];
            static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { TABLE[0]; } }
            fn at_match() { N.load(Relaxed); }";
        assert!(reads(src).is_empty());
    }

    #[test]
    fn thread_locals_env_and_seeded_maps_are_reported() {
        let src = "thread_local! { static OFF: Cell<bool> = const { Cell::new(false) }; }
            impl Regex { fn new(p: u8) { a(); b(); c(); } }
            fn a() { OFF.with(Cell::get); }
            fn b() { std::env::var(\"X\"); }
            fn c() { let m: HashMap<u8, u8> = HashMap::new(); }";
        let got = reads(src);
        for want in [("a", "OFF"), ("b", "std::env"), ("c", "HashMap")] {
            assert!(
                got.contains(&(want.0.to_string(), want.1.to_string())),
                "{want:?} in {got:?}"
            );
        }
    }

    #[test]
    fn a_lazy_builder_is_a_root() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u8>) { c.get_or_init(|| build()); }
            fn build() -> u8 { N.load(Relaxed) as u8 }";
        assert_eq!(reads(src), vec![("build".to_string(), "N".to_string())]);
    }

    #[test]
    fn a_builder_passed_by_name_is_a_root() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u8>) { c.get_or_init(build); }
            fn build() -> u8 { N.load(Relaxed) as u8 }";
        assert_eq!(reads(src), vec![("build".to_string(), "N".to_string())]);
    }

    /// What a `get_or_init` initializer reads in place (a closure's body)
    /// is the holding function's read, though no root reaches it: a hidden
    /// static and the environment. A reached function's initializer is
    /// read once, with the rest of its body.
    #[test]
    fn an_initializer_read_in_place_is_reported() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u64>) { c.get_or_init(|| N.load(Relaxed)); }
            fn env_later(c: &OnceLock<u8>) { c.get_or_init(|| { std::env::var(\"X\"); 0 }); }
            impl Regex { fn new(p: u8) { C.get_or_init(|| N.load(Relaxed)); } }";
        let mut got = reads(src);
        got.sort();
        let want: Vec<(String, String)> = [("env_later", "std::env"), ("later", "N"), ("new", "N")]
            .iter()
            .map(|(f, i)| ((*f).to_string(), (*i).to_string()))
            .collect();
        assert_eq!(got, want);
        // Every call an initializer makes in place roots the walk, the ones
        // the compiler inserts too: `Foo + Foo` reaches `add`.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u64>) { c.get_or_init(|| Foo + Foo); }
            struct Foo;
            impl std::ops::Add for Foo { type Output = u64; fn add(self, _: Self) -> u64 { N.load(Relaxed) } }";
        let got = reads(src);
        assert!(
            got.contains(&("add".to_string(), "N".to_string())),
            "inserted call: got = {got:?}, wanted = add reading N"
        );
        // A guard the initializer builds and drops: its `Drop` is reached.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u64>) { c.get_or_init(|| { let _g = Guard; 0 }); }
            struct Guard;
            impl Drop for Guard { fn drop(&mut self) { N.load(Relaxed); } }";
        let got = reads(src);
        assert!(
            got.contains(&("drop".to_string(), "N".to_string())),
            "inserted drop: got = {got:?}, wanted = drop reading N"
        );
        // An initializer that is not a closure written in place (a local
        // closure passed by name, or one a closure in place calls, bound
        // by name or in a pattern: a `let`'s, a `match` arm's, a `for`'s,
        // a closure parameter): the whole holder is read, its reads
        // and its calls. Negative: a closure in place reads only itself.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            static M: AtomicU64 = AtomicU64::new(0);
            fn later(c: &OnceLock<u64>) { let init = || N.load(Relaxed); c.get_or_init(init); }
            fn later_call(c: &OnceLock<u64>) { let init = || reader(); c.get_or_init(move || init()); }
            fn later_tuple(c: &OnceLock<u64>) { let (init,) = (|| N.load(Relaxed),); c.get_or_init(move || init()); }
            fn later_struct(c: &OnceLock<u64>) { let Pair { a: mut init, .. }: Pair<_> = pair(|| N.load(Relaxed)); c.get_or_init(move || init()); }
            fn later_match(c: &OnceLock<u64>) { match Some(|| N.load(Relaxed)) { Some(init) => { c.get_or_init(move || init()); } _ => {} } }
            fn later_match_struct(c: &OnceLock<u64>) { match (Pair { a: || N.load(Relaxed) }) { Pair { a: init } => { c.get_or_init(move || init()); } } }
            fn later_match_guard(c: &OnceLock<u64>) { match Some(|| N.load(Relaxed)) { None => {} Some(init) if { true } => { c.get_or_init(move || init()); } _ => {} } }
            fn later_for(c: &OnceLock<u64>) { for init in [|| N.load(Relaxed)] { c.get_or_init(move || init()); } }
            fn later_param(c: &OnceLock<u64>) { let run = |init: fn() -> u64| { c.get_or_init(move || init()); }; run(|| N.load(Relaxed)); }
            fn reader() -> u64 { M.load(Relaxed) }
            fn in_place(c: &OnceLock<u64>) { M.load(Relaxed); c.get_or_init(|| 0); }";
        let mut got = reads(src);
        got.sort();
        let want: Vec<(String, String)> = [
            ("later", "N"),
            ("later_for", "N"),
            ("later_match", "N"),
            ("later_match_guard", "N"),
            ("later_match_struct", "N"),
            ("later_param", "N"),
            ("later_struct", "N"),
            ("later_tuple", "N"),
            ("reader", "M"),
        ]
        .iter()
        .map(|(f, i)| ((*f).to_string(), (*i).to_string()))
        .collect();
        assert_eq!(got, want);
    }

    /// The environment read under a `use .. as` name, of a function or of
    /// `std::env` itself, through a chain of renames too. Negative: a call
    /// of the crate's own function of that name, with no rename.
    #[test]
    fn a_renamed_environment_read_is_reported() {
        for (src, want) in [
            (
                "use std::env::var as get; impl Regex { fn new(p: u8) { let _ = get(\"X\"); } }",
                true,
            ),
            (
                "use std::env as process; impl Regex { fn new(p: u8) { process::var(\"X\"); } }",
                true,
            ),
            (
                "use std::env as e1; use e1 as e2; impl Regex { fn new(p: u8) { e2::var(\"X\"); } }",
                true,
            ),
            // A grouped `self as` rename, and a qualified call of a renamed
            // function a module re-exports.
            (
                "use std::env::{self as cfg}; impl Regex { fn new(p: u8) { cfg::var(\"X\"); } }",
                true,
            ),
            (
                "use std::env::{self as cfg}; impl Regex { fn new(p: u8) { cfg::current_exe(); } }",
                true,
            ),
            (
                "mod reads { pub use std::env::var as get; } impl Regex { fn new(p: u8) { reads::get(\"X\"); } }",
                true,
            ),
            // No rename: a module that re-exports or globs `std::env`'s
            // functions, called by a path; and a rename called through
            // `self::` or `crate::`, or passed by name.
            (
                "mod reads { pub use std::env::var; } impl Regex { fn new(p: u8) { reads::var(\"X\"); } }",
                true,
            ),
            (
                "mod cfg { pub use std::env::*; } impl Regex { fn new(p: u8) { cfg::var(\"X\"); } }",
                true,
            ),
            (
                "use std::env::var as get; impl Regex { fn new(p: u8) { self::get(\"X\"); } }",
                true,
            ),
            (
                "use std::env::var as get; impl Regex { fn new(p: u8) { crate::get(\"X\"); } }",
                true,
            ),
            (
                "mod reads { pub use std::env::var as get; } impl Regex { fn new(p: u8) { p.map(reads::get); } }",
                true,
            ),
            (
                "impl Regex { fn new(p: u8) { get(1); } } fn get(x: u8) {}",
                false,
            ),
            // An import of `env`'s function under its own name, beside an
            // unrelated function of that name: the import decides.
            (
                "mod reads { pub use std::env::var; } fn var() {} impl Regex { fn new(p: u8) { reads::var(\"X\"); } }",
                true,
            ),
            (
                "mod reads { pub use std::env::{args, var}; } fn var() {} impl Regex { fn new(p: u8) { reads::var(\"X\"); } }",
                true,
            ),
            (
                "mod reads { pub use std::env::*; } fn var() {} impl Regex { fn new(p: u8) { reads::var(\"X\"); } }",
                true,
            ),
            // The same through a rename of `env`, beside a function of
            // the crate's own by that name: by `as`, by a grouped
            // `self as`, globbed, and in a module called by a path. (The
            // fixtures' scan reads no inline module's functions, so the
            // crate's own `var` is at the top.)
            (
                "use std::env as process; use process::var; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(\"X\"); } }",
                true,
            ),
            (
                "use std::env::{self as cfg}; use cfg::{var}; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(\"X\"); } }",
                true,
            ),
            (
                "use std::env as process; use process::*; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(\"X\"); } }",
                true,
            ),
            (
                "mod reads { use std::env as process; pub use process::var; } fn var(x: u8) {} impl Regex { fn new(p: u8) { reads::var(\"X\"); } }",
                true,
            ),
            // An import of that name from a module that is not `env`.
            (
                "use other::var; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(1); } }",
                false,
            ),
            (
                "use other::*; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(1); } }",
                false,
            ),
            // The crate's own `ENV_FNS`-named function, with no import of
            // `env`'s, or with it imported only under another name.
            (
                "fn var(x: u8) {} impl Regex { fn new(p: u8) { var(1); } }",
                false,
            ),
            (
                "use std::env::var as get; fn var(x: u8) {} impl Regex { fn new(p: u8) { var(1); } }",
                false,
            ),
        ] {
            let got = reads(src)
                .iter()
                .any(|(f, i)| f == "new" && i == "std::env");
            assert!(got == want, "{src}: got = {got}, wanted = {want}");
        }
    }

    #[test]
    fn a_function_passed_by_name_is_followed() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { p.ok_or_else(internal); q.map_err(errors::from_parse); } }
            fn internal() { N.load(Relaxed); }
            fn from_parse(e: u8) { N.load(Relaxed); }";
        let got = reads(src);
        for want in ["internal", "from_parse"] {
            assert!(
                got.contains(&(want.to_string(), "N".to_string())),
                "{want} in {got:?}"
            );
        }
    }

    #[test]
    fn a_local_named_like_a_function_keeps_the_edge() {
        // The check does not reason about bindings: a parameter, a `let` and
        // a pattern named like a function keep the edge to it, which costs
        // an extra edge and never a missing one.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(helper: u8) { g(helper); let other = 1; g(other); let (third, x) = (1, 2); g(third); } }
            fn g(x: u8) {}
            fn helper() { N.load(Relaxed); }
            fn other() { N.load(Relaxed); }
            fn third() { N.load(Relaxed); }";
        let mut got = reads(src);
        got.sort();
        let want: Vec<(String, String)> = ["helper", "other", "third"]
            .iter()
            .map(|f| (f.to_string(), "N".to_string()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn the_question_mark_reaches_from() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) -> Result<u8, E> { let x = parse(p)?; Ok(x) } }
            fn parse(p: u8) -> Result<u8, u8> { Ok(p) }
            impl E { fn from(e: u8) -> E { N.load(Relaxed); E } }";
        assert_eq!(reads(src), vec![("from".to_string(), "N".to_string())]);
    }

    #[test]
    fn any_atomic_static_is_hidden() {
        let src = "static N: AtomicI32 = AtomicI32::new(0);
            impl Regex { fn new(p: u8) { N.load(Relaxed); } }";
        assert_eq!(reads(src), vec![("new".to_string(), "N".to_string())]);
    }

    #[test]
    fn a_static_fails_closed() {
        // A struct that holds an atomic is hidden, though its own name is
        // not one; a struct of plain data, and an array of it, are not.
        let src = "struct Counter(AtomicU64);
            struct Record { a: u8, b: &'static [u16] }
            static N: Counter = Counter(AtomicU64::new(0));
            static R: [Record; 2] = [Record { a: 0, b: &[] }, Record { a: 1, b: &[] }];
            static E: Elsewhere = Elsewhere;
            impl Regex { fn new(p: u8) { N.0.load(Relaxed); R[0].a; E; } }";
        let got = reads(src);
        assert!(
            got.contains(&("new".to_string(), "N".to_string())),
            "{got:?}"
        );
        assert!(
            !got.contains(&("new".to_string(), "R".to_string())),
            "{got:?}"
        );
        // A type the crates do not define is not known to be plain.
        assert!(
            got.contains(&("new".to_string(), "E".to_string())),
            "{got:?}"
        );
    }

    #[test]
    fn a_struct_field_binds_no_name() {
        // A struct literal's field named like a function does not hide the
        // function passed by name beside it.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { let i = Info { helper: 0 }; p.map(helper); } }
            fn helper() { N.load(Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "N".to_string())]);
    }

    #[test]
    fn a_bitwise_or_binds_no_name() {
        // A `|` that is a bitwise or, beside or inside a closure, does not
        // open a closure's parameters.
        for call in ["f(a | B, helper);", "f(|x| x | 1, helper);"] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                impl Regex {{ fn new(a: u8) {{ {call} }} }}
                fn f(a: u8, b: u8) {{}}
                fn helper() {{ N.load(Relaxed); }}"
            );
            assert_eq!(
                reads(&src),
                vec![("helper".to_string(), "N".to_string())],
                "{call}"
            );
        }
    }

    #[test]
    fn a_closure_parameter_keeps_the_edge() {
        // A closure's parameter named like a function, and a closure with
        // no parameters, change nothing: the edge stays.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { p.map(|helper| g(helper)); p.map(|| 1); } }
            fn g(x: u8) {}
            fn helper() { N.load(Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "N".to_string())]);
    }

    #[test]
    fn generic_arguments_keep_the_edge() {
        // The commas of a generic argument list, or of a comparison, in a
        // call's arguments are not the call's: the one-argument `helper`
        // is still reached.
        for call in [
            "helper(make::<u8, u16>())",
            "helper(Vec::<(u8, u16)>::new())",
            "helper(a < b)",
        ] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                impl Regex {{ fn new(a: u8, b: u8) {{ {call}; }} }}
                fn make<A, B>() -> u8 {{ 0 }}
                fn helper(x: u8) {{ N.load(Relaxed); }}"
            );
            assert_eq!(
                reads(&src),
                vec![("helper".to_string(), "N".to_string())],
                "{call}"
            );
        }
    }

    #[test]
    fn a_match_guard_keeps_the_edge() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { match p { _ if g(helper) => 1, _ => 2 }; p.map(helper); } }
            fn g(x: u8) -> bool { true }
            fn helper() { N.load(Relaxed); }";
        assert_eq!(reads(src), vec![("helper".to_string(), "N".to_string())]);
    }

    #[test]
    fn a_local_named_len_is_harmless() {
        // A local that shares a method's name (`len`) adds no read: the
        // method is followed as a method, the local only as a free name
        // that no function of these crates has.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(v: u8) { let len = v.len(); g(len); } }
            fn g(x: u8) {}
            impl V { fn len(&self) -> u8 { 0 } }";
        assert!(reads(src).is_empty(), "{:?}", reads(src));
    }

    #[test]
    fn an_index_or_a_borrow_binds_no_name() {
        for stmt in [
            "let mut a = [0]; a[helper] = 1; f(helper);",
            "f(&mut helper); f(helper);",
        ] {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                impl Regex {{ fn new(p: u8) {{ {stmt} }} }}
                fn f(a: u8) {{}}
                fn helper() {{ N.load(Relaxed); }}"
            );
            assert_eq!(
                reads(&src),
                vec![("helper".to_string(), "N".to_string())],
                "{stmt}"
            );
        }
    }

    #[test]
    fn operators_and_formatting_reach_their_impls() {
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(a: u8, b: u8) { a == b; } }
            impl T { fn eq(&self, o: &T) -> bool { N.load(Relaxed); true } }
            impl W { fn go(&self) { format!(\"{}\", self); } }
            impl V { fn fmt(&self, f: u8) { N.load(Relaxed); } }";
        let got = reads(src);
        assert!(
            got.contains(&("eq".to_string(), "N".to_string())),
            "{got:?}"
        );
        assert!(!got.iter().any(|(f, _)| f == "fmt"), "{got:?}");
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { format!(\"{}\", p); } }
            impl V { fn fmt(&self, f: u8) { N.load(Relaxed); } }";
        assert_eq!(reads(src), vec![("fmt".to_string(), "N".to_string())]);
    }

    #[test]
    fn imported_env_clocks_and_addresses_are_hidden() {
        let src = "impl Regex { fn new(p: u8) { a(); b(); c(&[]); } }
            fn a() { var_os(\"X\"); }
            fn b() { Instant::now(); }
            fn c(s: &[u8]) -> usize { s.as_ptr() as usize }";
        let got = reads(src);
        for want in [("a", "std::env"), ("b", "clock"), ("c", "pointer address")] {
            assert!(
                got.contains(&(want.0.to_string(), want.1.to_string())),
                "{want:?} in {got:?}"
            );
        }
    }

    #[test]
    fn a_name_defined_twice_is_plain_only_if_both_are() {
        // `y::Counter` is plain data but `x::Counter` holds an atomic: a
        // static of `Counter` is hidden, whichever definition is read last.
        let src = "mod x { struct Counter(AtomicU64); }
            mod y { struct Counter { n: u8 } }
            static S: Counter = Counter { n: 0 };
            impl Regex { fn new(p: u8) { S.n; } }";
        assert_eq!(reads(src), vec![("new".to_string(), "S".to_string())]);
    }

    #[test]
    fn calls_resolve_by_name_arity_and_owner() {
        // `x.f(1)` reaches a method `f` of one argument only; `Other::g()`
        // reaches `Other`'s `g`, not `Regex`'s.
        let src = "static N: AtomicU64 = AtomicU64::new(0);
            impl Regex { fn new(p: u8) { p.f(1); Other::g(); } fn g() { N.load(Relaxed); } }
            impl A { fn f(&self, a: u8) {} }
            impl B { fn f(&self, a: u8, b: u8) { N.load(Relaxed); } }
            impl Other { fn g() {} }";
        assert!(reads(src).is_empty(), "{:?}", reads(src));
    }

    #[test]
    fn an_owner_with_no_such_method_follows_every_method_of_the_name() {
        // A type parameter longer than two characters, a primitive (by name,
        // and as `Self` in an impl for it), and a qualified path: none owns a
        // `step` here, so each reaches every `step`.
        let missed: Vec<&str> = [
            "fn new<Unit: Step>(p: u8) { Unit::step(p) } }",
            "fn new(p: u8) { u8::step(p) } }",
            "fn new(p: u8) { p.go() } } impl Go for u8 { fn go(&self) { Self::step(*self) } }",
            "fn new(p: u8) { let v = p; <Inner as Step>::step(v) } }",
        ]
        .into_iter()
        .filter(|body| {
            let src = format!(
                "static N: AtomicU64 = AtomicU64::new(0);
                impl Regex {{ {body}
                impl Inner {{ fn step(p: u8) {{ N.load(Relaxed); }} }}"
            );
            reads(&src) != vec![("step".to_string(), "N".to_string())]
        })
        .collect();
        assert!(missed.is_empty(), "missed: {missed:?}");
    }

    #[test]
    fn rows_allow_by_input_function_and_file() {
        let row = "| `N` | `fn helper` | `crates/pcre3/src/regex.rs` | why |";
        assert!(super::row(
            row,
            Path::new("crates/pcre3/src/regex.rs"),
            "helper",
            "N"
        ));
        assert!(!super::row(
            row,
            Path::new("crates/pcre3/src/engine.rs"),
            "helper",
            "N"
        ));
        assert!(!super::row(
            row,
            Path::new("crates/pcre3/src/regex.rs"),
            "helper",
            "M"
        ));
    }
}
