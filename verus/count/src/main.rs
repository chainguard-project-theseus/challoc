//! verus-count: counts the executable functions of a proved port's crates
//! by how Verus sees them, and holds them to the port's FLOOR
//! (kit/count/README.md). The port's `verus/count.toml` ([`config`]) names
//! its crates, its test-only `cfg`s and features, and where each table the
//! checks below read lives; a check whose section it leaves out does not
//! run. The paths below are pcre3's, which the defaults follow.
//!
//! A function is *verified* when it sits inside `verus! { ... }`, or
//! carries Verus's attribute form (`#[verus_verify]` or `#[verus_spec]`,
//! directly or under a `cfg_attr` whose predicate Verus's build is sure
//! to meet),
//! is not `#[verifier::external_body]`,
//! `#[verifier::external]` or `verus_verify(external_body)`, and its body
//! calls none of `assume`, `admit` and `Tracked::assume_new` (which mints
//! a ghost token on trust); *trusted* when it sits inside
//! `verus!` but is one of those; and *outside* when it is not inside
//! `verus!` at all. A trusted function marked
//! `#[cfg_attr(any(), discharged_by = "kani")]` is *discharged* instead:
//! Kani checks its contract (verus/README.md, "Kani"), on the bounded inputs
//! its record names, so it counts neither as verified nor as trusted. The
//! marker is inert in every build (`any()` is false), and a marked function
//! no `verus/seams/kani-*.md` record covers fails the run. With a
//! `[ledger]` section, a generated seam ledger ([`ledger`]) says which
//! trusted functions are discharged instead, and the marker is refused.
//! `spec` and `proof` functions are ghost code, erased from ordinary
//! builds, and are counted apart. Items under the test-only `cfg`s
//! (`#[cfg(test)]`, `#[cfg(kani)]`, the Kani harnesses) or test-only
//! features (`#[cfg(feature = "test-util")]`, which only tests enable; a
//! build of the workspace that cargo resolves with one enabled fails the
//! run) are left out, with the files of such modules and, for a `mod.rs`
//! one, its directory. Items built only for Verus (`verus_keep_ghost`, or
//! a configured Verus-only feature) are read for their rlimits and their
//! trusted functions alone. A shipped function under a `cfg` Verus's build
//! might not meet (on the function, or on an `impl`, trait, inline `mod`,
//! `verus!` block or out-of-line `mod x;` around it), or under a
//! `cfg_attr` it might not apply, is *trusted* rather than verified: it
//! ships, and nothing proves Verus compiled it. Only the atoms the
//! configuration settles there are sure: `verus_keep_ghost`, the
//! Verus-only features, the test-only atoms (never set), and the
//! `verus_sets` and `verus_never_sets` atoms; a platform such as
//! `target_os = "windows"`, or any other atom, may be either.
//! Lines run from a function's `fn` keyword to its closing brace.
//!
//! A function an item macro writes counts too ([`Counter::item_macro`]):
//! an invocation of a `macro_rules!` the crate defines is expanded
//! ([`expand`]), and each function in the expansion counts as outside
//! `verus!`, listed at the invocation's line, its lines those of the
//! invocation and the definition it came from. An item macro the crate
//! does not define counts as one function outside `verus!`, named
//! `NAME!`, so no function leaves the tally for being written by a macro;
//! one the crate defines that does not expand fails the run. Fragments
//! match by the crate's edition ([`walked_editions`]): below 2024 an
//! `expr` is an `expr_2021` (no `const` block, no `_`), and below 2021 a
//! `pat` is a `pat_param` (no top-level `|`). A crate whose read targets
//! (library, binaries, and tests, benches or examples under `src/`) sit
//! on both sides of 2021 or of 2024 fails the run.
//! `compile_error!`, which writes nothing that ships ([`STD_NO_FN`]),
//! counts nothing where it is provably the standard library's
//! ([`Counter::std_writes_nothing`]). `verus!` is Verus's bare or as
//! `::vstd::prelude::verus!` / `::verus_builtin_macros::verus!`, written
//! by the crate or by a macro it expands; a block of another path, or of
//! one of these whose rule does not hold, that writes a function fails
//! the run. Each form has a rule true by rustc's own semantics ([`Took`]),
//! read from the crate's bindings rather than by resolving names.
//!
//! Usage: `verus-count [--config FILE] [--list] [--floor FILE [--base-rev
//! REV | --no-base]] [--rlimit STATS] ROOT`, with ROOT the repository root (default
//! `.`) and FILE the configuration (default `ROOT/verus/count.toml`);
//! `--version` prints the counter's version, and `verus-count [--config
//! FILE] --badge ROOT` the coverage badge line ROOT's FLOOR gives
//! ([`badge_line`]).
//! `--list` prints one line per executable function: crate, class, lines,
//! file:line and name, before the table; every check runs as without it.
//! `--floor FILE` fails the run when the `shipped`
//! row's verified share of functions or of lines, or its verified and
//! discharged share, is below the figure FILE records (verus/FLOOR), and
//! names the figure to record when the share has risen past it; FILE must
//! be the configuration's `[floor] path`. `--base-rev REV` also fails a
//! figure FILE sets below the figure in REV's FLOOR (at REV's `[floor]
//! path`) without a new `lowered` line for it, and fails when REV does
//! not resolve. With `[floor] renamed_from`, REV's copy may give the
//! checked figures under that older prefix, so the change that renames
//! them compares with its base. It also fails when the configuration
//! removes or narrows a gate `REV`'s copy of it runs with no new
//! `[[narrowed]]` line naming the change ([`check_config`]), since a gate
//! whose section is gone does not run at all. `--floor` with neither
//! `--base-rev` nor `--no-base` fails the run (exit 1), so a run that
//! should compare with a base cannot pass by leaving the base off;
//! `--no-base` says there is none to compare with (a local run, or the
//! change that adds the first FLOOR), and with `--base-rev` it is an error
//! (exit 2).
//!
//! With `[badge]`, `--floor FILE` also fails unless the badge file
//! (`README.md`) holds exactly one rendered Verus badge, in any case or
//! form ([`is_badge`]; fences and HTML comments do not render), and it is
//! the line `--badge` prints for FILE's `functions` and `lines` figures
//! ([`check_badge`]), so the badge a reader sees cannot drift from FLOOR.
//!
//! With `[recursion]`, it checks recursion (verus/theorems/RN1.md): it fails on a
//! cycle of direct calls, within one file, through a function of a shipped
//! crate that Verus does not check (outside `verus!`, or trusted or
//! discharged), and on one through a verified function that RN1.md's table
//! does not name.
//!
//! `--rlimit STATS` also checks rlimit headroom ([`rlimit`]; verus/README.md,
//! "Headroom"): STATS is what `cargo verus verify -- --time-expanded
//! --output-json` printed, and the run fails when a function of a crate it
//! covers used more of its rlimit than verus/RLIMIT allows. With
//! `--base-rev REV`, verus/RLIMIT may not loosen `REV:verus/RLIMIT`, and a
//! function's `#[verifier::rlimit]` may rise past REV's only with a new
//! `rlimit-raised` line in verus/RLIMIT. A function that loses its
//! attribute runs under the default (10), so dropping one below the
//! default is a rise too.
//!
//! With `[inputs]`, it checks compile's inputs (verus/theorems/CD1.md,
//! [`inputs`]): it fails when a function reachable from the configured
//! roots (`Regex::new`), or from a builder a pattern runs on first use,
//! reads a static that can change, a thread-local, the environment or a
//! seeded `HashMap`, unless CD1.md's table names the read, and on a row of
//! that table no such read matches.
//!
//! With `[reach]`, it checks what a C caller reaches ([`reach`], verus/REACH): it fails
//! when a function reachable from an entry point (an exported symbol, a
//! function a `const` or `static` hands out, a `width::exports!` body or
//! a `FromC` conversion) is neither verified nor discharged nor a row of
//! verus/REACH, on a row that is verified or unreached, on a seam citation
//! no seams table has, and, with `--base-rev REV`, on a row
//! `REV:verus/REACH` lacks that is not marked `new`. It also fails on a
//! shipped file whose export blocks it cannot read ([`reach::exported`]):
//! one that does not parse, one that imports the exports macro under
//! another name, and one whose invocations of it (found by tokens, plain
//! or raw) outnumber the blocks the scan reads.
//!
//! With `[mints]`, it checks the C caller's token mints ([`mints`],
//! verus/MINTS): it fails on a kind of token (`impl CToken for KIND`) or a
//! function that calls `c_mint` that verus/MINTS does not list, on a mint
//! that returns another kind than its row names, on a stale or duplicate
//! row, and on an alias of `c_mint` or `CToken` through which either would
//! go unseen.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

mod config;
mod expand;
mod inputs;
mod ledger;
mod mints;
mod reach;
mod rlimit;

use config::Config;

use proc_macro2::{TokenStream, TokenTree};
use verus_syn::{
    Attribute, FnMode, ImplItem, Item, Meta, Signature, TraitItem, parse_file, parse2,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class {
    Verified,
    Trusted,
    Outside,
    Discharged,
}

#[derive(Clone, Default)]
struct Tally {
    fns: [usize; 4],
    lines: [usize; 4],
    ghost: usize,
}

impl Tally {
    fn add(&mut self, t: &Tally) {
        for i in 0..4 {
            self.fns[i] += t.fns[i];
            self.lines[i] += t.lines[i];
        }
        self.ghost += t.ghost;
    }
}

struct Counter {
    krate: String,
    file: PathBuf,
    list: bool,
    tallies: BTreeMap<String, Tally>,
    /// Files and `verus!` blocks that could not be read or parsed. Their
    /// functions are missing from every count, so any of them fails the run.
    errors: usize,
    /// Each discharged function's file, relative to the root, and name, for
    /// [`unrecorded`].
    discharged: Vec<(PathBuf, String)>,
    /// Every executable function, for [`recursive`].
    fns: Vec<FnRef>,
    /// Each source file a shipped build compiles, with its crate, whether
    /// or not it holds a function: the files [`entry_reach`] scans for
    /// export blocks.
    files: Vec<(String, PathBuf)>,
    /// Every `static`, for the compile-inputs check ([`inputs`]).
    statics: Vec<inputs::Static>,
    /// The structs, enums and aliases, for which statics are plain data
    /// ([`inputs::plain_types`]).
    types: inputs::Types,
    /// The `use .. as` renames and `type` aliases ([`inputs::aliases_in`]).
    aliases: inputs::Aliases,
    /// Every trait's name, for the compile-inputs check.
    traits: std::collections::BTreeSet<String>,
    /// The type (an `impl`) or trait whose functions [`Counter::items`] is
    /// walking; empty for free functions.
    owner: String,
    /// The module [`Counter::items`] is walking, as Verus names it: the
    /// crate (`pcre3_syntax`), then each module of the file's path and of
    /// any inline `mod` around the item.
    module: Vec<String>,
    /// Every function's `#[verifier::rlimit]`, ghost ones too, for the
    /// headroom check ([`rlimit::shares`]): Verus-only code's too, which
    /// Verus checks.
    limits: Vec<rlimit::Limit>,
    /// Every function Verus checks, rlimit or not, by the name a
    /// [`rlimit::Limit`] would give it, with where it starts: a function
    /// here that has no limit has the default ([`rlimit::check_raised`]).
    verus_fns: BTreeMap<String, String>,
    /// Whether the code [`Counter::items`] is walking is built by a
    /// shipped build: false inside a module, `impl` or trait built only
    /// for Verus, whose functions are walked for their rlimits and not
    /// counted.
    shipped: bool,
    /// Whether Verus's build is sure to compile the code
    /// [`Counter::items`] is walking: false inside a module, `impl`, trait
    /// or `verus!` block whose `cfg` it might not meet
    /// ([`Built::Unsure`]), whose verified functions count as trusted.
    sure: bool,
    /// The `cfg` predicates of the code [`Counter::items`] is walking: its
    /// file's ([`module_guards`]) and those of each module, `impl`, trait
    /// and `verus!` block around it. An item is built where they and its
    /// own all hold ([`guarded_built`]).
    guards: Vec<String>,
    /// `width::fn_ptr!`'s definition and invocations, expanded once every
    /// file is read ([`Counter::expand_fn_ptrs`]).
    fn_ptrs: FnPtrs,
    /// The functions a `const` or `static` initializer names as a value
    /// (`MallocPtr(default_malloc)`), by crate and name: whoever holds the
    /// pointer may call it, so each is an entry point ([`Counter::finish`]).
    addressed: Vec<(String, String)>,
    /// The `cfg` atoms that hold only in test builds or only in Verus's
    /// ([`Atoms`]).
    atoms: Atoms,
    /// The `fn_ptr!` macro's name (`[reach] fn_ptr_macro`), if any.
    fn_ptr_macro: Option<String>,
    /// Each trusted function's crate, `FILE:LINE` and lines, for the
    /// generated ledger ([`ledger::match_ledger`]).
    trusted: Vec<(String, String, usize)>,
    /// Whether ghost functions of code built only for Verus count in the
    /// ghost column (`[crates] verus_only_ghost`).
    verus_only_ghost: bool,
    /// The crate's `macro_rules!` definitions, by name, for
    /// [`Counter::item_macro`].
    macros: BTreeMap<String, Vec<MacroDef>>,
    /// Each module's `use` paths, for [`Counter::item_macro`] to resolve a
    /// macro an invocation names by an import ([`use_paths`]).
    uses: BTreeMap<Vec<String>, Vec<UsePath>>,
    /// While [`Counter::item_macro`] walks an expansion: the outermost
    /// invocation's line, which each function it writes is listed at.
    expansion: Option<usize>,
    /// How deep the expansion being walked is nested.
    depth: usize,
    /// While [`Counter::item_macro`] compares the definitions an
    /// invocation may expand: the rows a candidate's expansion writes,
    /// gathered in place of the tally ([`Counter::tally`]).
    dry: Option<Vec<(Class, String, usize)>>,
    /// The invocations that resolved to no definition and were counted as
    /// another crate's macro (`NAME!`), with where they are: once the crate
    /// is read, one whose name the crate defines fails the run
    /// ([`Counter::walk_crate`]), so that no function the crate's macro
    /// writes leaves the tally for a lookup the counter does not follow.
    unresolved: Vec<(String, String)>,
    /// Every module of the crate, by path from its root, so that a `use`
    /// path whose first segment is one of them reads as this crate's.
    modules: std::collections::BTreeSet<Vec<String>>,
    /// The crates a `#[macro_use] extern crate` at the crate root loads
    /// macros from ([`macro_use_crates`]): their names are in scope in
    /// every module, ahead of the standard library's prelude.
    macro_use: Vec<String>,
    /// Every binding the crate's sources write, but those only tests
    /// build ([`Binding`]): what the rules read ([`Took`]).
    bindings: Vec<Binding>,
    /// Every binding a macro expansion writes, read once the crate is
    /// walked, against what the walk took.
    emitted: Vec<Binding>,
    /// The crate's edition (its Cargo.toml's, or the workspace's it
    /// inherits; 2015 when it gives none, as cargo reads it).
    edition: u32,
    /// Each name among `vstd`, `verus_builtin_macros`, `core`, `std` and `alloc`
    /// that a dependency of the crate takes for another package (`vstd =
    /// { package = "other" }`), with why.
    renamed_deps: Vec<(String, String)>,
    /// Each `verus!` block holding a function, and each `compile_error!`
    /// counted as nothing, that the walk took on its rule: the form, the
    /// place, the module and the name the rule reads. An expansion that
    /// writes a binding breaking it fails the run once the crate is walked.
    took: Vec<(Took, String, Vec<String>, String)>,
}

/// One path a `use` names: its segments as written (the imported item
/// last), whether it ends in `*`, and the name it brings into scope (the
/// last segment, or the new name of a `use .. as`; empty for `*`).
type UseTarget = (Vec<String>, bool, String);

/// A [`UseTarget`] of this crate, with where its `use` (with its `cfg`s
/// and its enclosing modules') is built: a test-only import imports
/// nothing a shipped build sees.
type UsePath = (Vec<String>, bool, String, Built);

/// One `macro_rules!` definition: the file and line it is at, where it is
/// built, where its textual scope ends, the paths that name it, and its
/// rules, or why they could not be read.
#[derive(Clone)]
struct MacroDef {
    file: PathBuf,
    line: usize,
    /// Whether an expansion wrote it: it is listed at the outermost
    /// invocation's line, and is in scope from that invocation on.
    emitted: bool,
    /// The module it is in.
    module: Vec<String>,
    /// The paths that name it: the crate root's for `#[macro_export]`, and
    /// its module's for a `use NAME;` there (`pub(crate) use exports;`).
    paths: Vec<Vec<String>>,
    /// Where its `cfg`s, its enclosing modules' and its file's let it be
    /// built: a definition a build does not compile defines nothing there.
    built: Built,
    /// Whether a `cfg` on it or an enclosing item may be true or false in a
    /// build ([`Built::Unsure`]), so that it may not shadow an earlier
    /// definition of its name.
    unsure: bool,
    /// The module whose end ends its textual scope: the module it is in,
    /// or for one in a `#[macro_use]` module (inline, or a file declared
    /// `#[macro_use] mod name;`), the module that one is in.
    scope: Vec<String>,
    /// Whether it has `#[macro_export]`, which puts it at the crate root.
    exported: bool,
    rules: Result<expand::Rules, String>,
}

/// The narrower of two places an item can be built.
fn narrower(a: Built, b: Built) -> Built {
    match (a, b) {
        (Built::Test, _) | (_, Built::Test) => Built::Test,
        (Built::Verus, _) | (_, Built::Verus) => Built::Verus,
        (Built::Unsure, _) | (_, Built::Unsure) => Built::Unsure,
        _ => Built::Shipped,
    }
}

/// An expansion nested deeper than this is an error: rustc's default
/// `recursion_limit`.
const MAX_DEPTH: usize = 128;

/// The `cfg` atoms, without whitespace, that the configuration settles:
/// those only test builds meet (`test`, `kani`, `feature="test-util"`);
/// those only Verus's build meets (`verus_keep_ghost`, and `feature="F"`
/// for each configured Verus-only feature); and those Verus's build is
/// sure to meet (`sets`, `[crates] verus_sets`) or never meets (`never`,
/// `verus_never_sets`), which a shipped build may or may not. Every other
/// atom may be true or false in any build.
#[derive(Debug, Clone, Default)]
struct Atoms {
    test: Vec<String>,
    verus: Vec<String>,
    sets: Vec<String>,
    never: Vec<String>,
}

impl Atoms {
    fn of(c: &Config) -> Atoms {
        Atoms {
            test: c.test_atoms(),
            verus: c.verus_atoms(),
            sets: c.verus_set_atoms(),
            never: c.verus_never_atoms(),
        }
    }
}

impl Counter {
    /// A counter that has read nothing yet.
    fn new(list: bool, atoms: Atoms, fn_ptr_macro: Option<String>) -> Counter {
        Counter {
            krate: String::new(),
            file: PathBuf::new(),
            list,
            tallies: BTreeMap::new(),
            errors: 0,
            discharged: Vec::new(),
            fns: Vec::new(),
            files: Vec::new(),
            statics: Vec::new(),
            types: BTreeMap::new(),
            aliases: BTreeMap::new(),
            traits: std::collections::BTreeSet::new(),
            owner: String::new(),
            module: Vec::new(),
            limits: Vec::new(),
            verus_fns: BTreeMap::new(),
            shipped: true,
            sure: true,
            guards: Vec::new(),
            fn_ptrs: FnPtrs::default(),
            addressed: Vec::new(),
            atoms,
            fn_ptr_macro,
            trusted: Vec::new(),
            verus_only_ghost: false,
            macros: BTreeMap::new(),
            uses: BTreeMap::new(),
            dry: None,
            unresolved: Vec::new(),
            modules: std::collections::BTreeSet::new(),
            macro_use: Vec::new(),
            bindings: Vec::new(),
            emitted: Vec::new(),
            edition: 2015,
            renamed_deps: Vec::new(),
            took: Vec::new(),
            expansion: None,
            depth: 0,
        }
    }
}

/// A function declared in another's body, as [`Counter::nested`] reads
/// it: its attributes, signature and body.
type NestedFn<'a> = (&'a [Attribute], &'a Signature, &'a verus_syn::Block);

/// The `fn_ptr!` macro's one rule and its invocations: the definition
/// lives in one file (pcre3-capi's width.rs) and the invocations in
/// others, read in any order.
#[derive(Default)]
struct FnPtrs {
    /// The transcriber of `macro_rules! fn_ptr`'s rule, and how many rules
    /// it has (the expansion understands exactly one).
    rule: Option<TokenStream>,
    rules: usize,
    /// Each invocation: crate, file, line and the tokens it passes.
    uses: Vec<(String, PathBuf, usize, TokenStream)>,
}

/// One executable function, for the recursion check ([`recursive`]): its
/// crate, file and line, owner ([`Counter::owner`]), name, class, and the
/// functions its body calls directly ([`direct_calls`]).
struct FnRef {
    krate: String,
    file: PathBuf,
    line: usize,
    owner: String,
    name: String,
    class: Class,
    calls: Vec<(bool, String)>,
    /// What the body names, for the compile-inputs check ([`inputs`]).
    body: inputs::Body,
    /// Its parameters, the receiver aside, and whether it has one.
    params: usize,
    receiver: bool,
    /// Whether a caller outside the walk can enter it ([`reach`]): it
    /// exports its symbol ([`exports_symbol`]), or a `const` or `static`
    /// hands out its address ([`Counter::addressed`]).
    entry: bool,
}

/// `rule`, a `macro_rules!` transcriber, with each `$var` replaced by its
/// binding in `binds`, `$crate` by `crate`, and each `$( .. )` repetition
/// written once when every metavariable in it is bound, else not at all
/// (an optional or empty fragment). A metavariable `binds` does not name,
/// or one with no binding outside a repetition, is an error.
fn transcribe(
    rule: TokenStream,
    binds: &BTreeMap<&str, Option<TokenStream>>,
) -> Result<TokenStream, String> {
    fn vars(ts: TokenStream, out: &mut Vec<String>) {
        let mut it = ts.into_iter().peekable();
        while let Some(t) = it.next() {
            match t {
                TokenTree::Punct(p) if p.as_char() == '$' => {
                    if let Some(TokenTree::Ident(v)) = it.peek() {
                        out.push(v.to_string());
                    }
                }
                TokenTree::Group(g) => vars(g.stream(), out),
                _ => {}
            }
        }
    }
    let mut out = TokenStream::new();
    let mut it = rule.into_iter().peekable();
    while let Some(t) = it.next() {
        match t {
            TokenTree::Punct(p) if p.as_char() == '$' => match it.next() {
                Some(TokenTree::Ident(v)) if v == "crate" => {
                    out.extend([TokenTree::Ident(proc_macro2::Ident::new("crate", v.span()))]);
                }
                Some(TokenTree::Ident(v)) => match binds.get(v.to_string().as_str()) {
                    Some(Some(b)) => out.extend(b.clone()),
                    Some(None) => return Err(format!("leaves `${v}` unbound")),
                    None => return Err(format!("uses `${v}`, which it does not bind")),
                },
                Some(TokenTree::Group(g)) => {
                    // The separator, if any, then the repetition operator.
                    while let Some(TokenTree::Punct(op)) = it.peek() {
                        let c = op.as_char();
                        it.next();
                        if matches!(c, '*' | '?' | '+') {
                            break;
                        }
                    }
                    let mut inner = Vec::new();
                    vars(g.stream(), &mut inner);
                    for v in &inner {
                        if !binds.contains_key(v.as_str()) && v != "crate" {
                            return Err(format!("uses `${v}`, which it does not bind"));
                        }
                    }
                    if inner
                        .iter()
                        .all(|v| v == "crate" || binds.get(v.as_str()).is_some_and(Option::is_some))
                    {
                        out.extend(transcribe(g.stream(), binds)?);
                    }
                }
                _ => return Err("has a `$` that starts no metavariable".into()),
            },
            TokenTree::Group(g) => {
                let mut ng = proc_macro2::Group::new(g.delimiter(), transcribe(g.stream(), binds)?);
                ng.set_span(g.span());
                out.extend([TokenTree::Group(ng)]);
            }
            t => out.extend([t]),
        }
    }
    Ok(out)
}

/// Whether `sig`'s ABI is one C can call: `extern "C"`, `extern
/// "C-unwind"`, or a bare `extern`, which is C.
fn is_c_abi(sig: &Signature) -> bool {
    sig.abi.as_ref().is_some_and(|a| {
        a.name
            .as_ref()
            .is_none_or(|n| matches!(n.value().as_str(), "C" | "C-unwind"))
    })
}

/// Whether C can call the function by its name: `#[no_mangle]` or
/// `#[export_name = ..]`, either one inside `#[unsafe(..)]` too. That, not
/// the ABI, makes a function an entry point ([`entry_roots`]); the ABI is
/// then [`is_c_abi`]'s to check ([`Counter::record`]).
fn exports_symbol(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let tokens = verus_syn::__private::ToTokens::to_token_stream(&a.meta);
        let mut idents = Vec::new();
        let mut stack = vec![tokens];
        while let Some(ts) = stack.pop() {
            for t in ts {
                match t {
                    TokenTree::Ident(i) => idents.push(i.to_string()),
                    TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Parenthesis => {
                        stack.push(g.stream());
                    }
                    _ => {}
                }
            }
        }
        match idents.first().map(String::as_str) {
            Some("no_mangle" | "export_name") => true,
            Some("unsafe") => idents
                .get(1)
                .is_some_and(|i| i == "no_mangle" || i == "export_name"),
            _ => false,
        }
    })
}

/// A signature's parameters, the receiver aside, and whether it has one.
fn arity(sig: &Signature) -> (usize, bool) {
    let receiver = sig
        .inputs
        .iter()
        .any(|a| matches!(a.kind, verus_syn::FnArgKind::Receiver(_)));
    (sig.inputs.len() - usize::from(receiver), receiver)
}

/// What the first pass learns of the module files ([`module_files`]):
/// each file's own inner `#![cfg(..)]` predicates (`inner`); each
/// out-of-line declaration of a file, with the file that declares it and
/// the `cfg` predicates it is under in that file (`declared`: its own,
/// `#[cfg(..)] mod name;`, and those of the inline modules, `verus!`
/// blocks and macro bodies around it), one record per declaration, since a
/// file declared twice is built where either holds; and the files a
/// `#[path]` names (`path_named`), which rustc reads as a `mod.rs`, so the
/// modules they declare are their siblings; and the crate's root files
/// (`roots`: `src/lib.rs`, `src/main.rs`, and each `src/bin` target),
/// which nothing declares. [`module_guards`] reads a file's guards from
/// them, and from those of the files that declare it.
#[derive(Default)]
struct ModFiles {
    inner: Vec<(PathBuf, Vec<String>)>,
    declared: Vec<Declared>,
    path_named: Vec<PathBuf>,
    roots: Vec<PathBuf>,
}

/// One out-of-line declaration ([`ModFiles`]): the file declared, the file
/// declaring it, and the `cfg` predicates it is under there.
struct Declared {
    file: PathBuf,
    from: PathBuf,
    guards: Vec<String>,
}

impl ModFiles {
    /// The directory of the modules `file` declares ([`module_dir`]), a
    /// crate root and a file a `#[path]` names counting as a `mod.rs`.
    fn dir_of(&self, file: &Path) -> Option<PathBuf> {
        if self.path_named.iter().chain(&self.roots).any(|p| p == file) {
            return file.parent().map(Path::to_path_buf);
        }
        module_dir(file)
    }
}

/// The atom a module file whose declaration the counter cannot read is
/// guarded by: one declared only inside a macro body other than `verus!`
/// (`cfg_if! { if #[cfg(windows)] { mod win; } }`), whose `cfg`s the
/// counter does not evaluate, and a crate file no declaration it reads
/// names (one a `#[cfg_attr(P, path = "..")]` under a macro names, or
/// one some other way reaches). No configuration settles this atom, so
/// the file is unsure ([`Built::Unsure`]): it counts as trusted, never as
/// verified.
const UNREAD_DECLARATION: &str = "verus_count_unread_declaration";

/// The edition of the crate in `dir` (its Cargo.toml's `[package]
/// edition`, or, with `edition.workspace = true`, `[workspace.package]
/// edition` of `root`'s Cargo.toml; 2015 when none is given, as cargo
/// reads it; the lowest of that and any target's own, `[lib] edition`
/// and the like), and each of `vstd`, `verus_builtin_macros`, `core`, `std` and `alloc`
/// that a dependency table (`[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]`, or a `[target.*]` one) takes for another
/// package (`vstd = { package = "other" }`, directly or through a
/// `{ workspace = true }` entry of `[workspace.dependencies]`), or
/// inherits from an entry the root does not give, with why. A key with
/// no `package` is the package of its name, as cargo requires. A
/// manifest that does not parse gives 2015, where no `::` path is taken.
fn crate_manifest(root: &Path, dir: &Path) -> (u32, Vec<(String, String)>) {
    let read = |p: &Path| -> toml::Table {
        fs::read_to_string(p.join("Cargo.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    };
    let manifest = read(dir);
    let workspace = read(root);
    let workspace = workspace.get("workspace");
    let inherits =
        |v: &toml::Value| v.get("workspace").and_then(toml::Value::as_bool) == Some(true);
    let package = package_edition(&manifest, workspace);
    // A target may set its own edition (`[lib] edition = "2015"`): the
    // lowest of the package's and every target's is the crate's.
    let targets = manifest.get("lib").into_iter().chain(
        ["bin", "example", "test", "bench"]
            .iter()
            .filter_map(|k| manifest.get(*k)?.as_array())
            .flatten(),
    );
    let edition = targets
        .filter_map(|t| t.get("edition"))
        .map(|e| e.as_str().and_then(|e| e.parse().ok()).unwrap_or(2015))
        .fold(package, u32::min);
    let mut tables: Vec<&toml::Value> = ["dependencies", "dev-dependencies", "build-dependencies"]
        .iter()
        .filter_map(|k| manifest.get(*k))
        .collect();
    if let Some(toml::Value::Table(targets)) = manifest.get("target") {
        for t in targets.values() {
            for k in ["dependencies", "dev-dependencies", "build-dependencies"] {
                tables.extend(t.get(k));
            }
        }
    }
    let mut renamed = Vec::new();
    for table in tables.into_iter().filter_map(toml::Value::as_table) {
        for (key, dep) in table {
            let name = key.replace('-', "_");
            if !["vstd", "verus_builtin_macros", "core", "std", "alloc"].contains(&name.as_str()) {
                continue;
            }
            // `{ workspace = true }` takes the workspace's entry of the key,
            // its `package` too; one the root does not give is unknown.
            let dep = match inherits(dep) {
                true => match workspace.and_then(|w| w.get("dependencies")?.get(key)) {
                    Some(d) => d,
                    None => {
                        renamed.push((
                            name.clone(),
                            format!("Cargo.toml's dependency `{key}` inherits a `[workspace.dependencies]` entry the workspace root does not give"),
                        ));
                        continue;
                    }
                },
                false => dep,
            };
            if let Some(p) = dep.get("package").and_then(toml::Value::as_str)
                && p.replace('-', "_") != name
            {
                renamed.push((
                    name.clone(),
                    format!("Cargo.toml's dependency `{key}` is the package `{p}`"),
                ));
            }
        }
    }
    (edition, renamed)
}

/// The `[package] edition` of `manifest`, or with `edition.workspace =
/// true` the `[workspace.package] edition` of `workspace` (the root's
/// `[workspace]` table); 2015 when none is given, as cargo reads it.
fn package_edition(manifest: &toml::Table, workspace: Option<&toml::Value>) -> u32 {
    match manifest.get("package").and_then(|p| p.get("edition")) {
        Some(toml::Value::String(e)) => e.parse().unwrap_or(2015),
        Some(t @ toml::Value::Table(_))
            if t.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
        {
            workspace
                .and_then(|w| w.get("package")?.get("edition")?.as_str()?.parse().ok())
                .unwrap_or(2015)
        }
        _ => 2015,
    }
}

/// The editions of the crate's targets whose files the walk reads, by
/// which its macros are matched ([`expand::set_edition`]): the library's
/// (`[lib] edition`, else the package's), where the package has a library
/// (a `[lib] path`, or `src/lib.rs` unless `autolib = false` and no
/// `[lib]`), and each binary's (`[[bin]]
/// edition`, else the package's), with workspace inheritance as
/// [`crate_manifest`] reads it. The binaries are the `[[bin]]` tables and,
/// unless `autobins = false`, those cargo finds as [`crate_roots`] does
/// (`src/main.rs`, each `src/bin/NAME.rs` and `src/bin/NAME/main.rs`), at
/// the package's edition but where a `[[bin]]` of that name or path gives
/// its own. A `[[test]]`, `[[bench]]` or `[[example]]` ships nothing, but
/// one whose `path` is a file under `src/` is a root the walk reads as
/// shipped, so its edition is listed too; one outside `src/` is never
/// read.
fn walked_editions(root: &Path, dir: &Path) -> Vec<u32> {
    let read = |p: &Path| -> toml::Table {
        fs::read_to_string(p.join("Cargo.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    };
    let manifest = read(dir);
    let workspace = read(root);
    let package = package_edition(&manifest, workspace.get("workspace"));
    let own = |t: &toml::Value| {
        t.get("edition").map_or(package, |e| {
            e.as_str().and_then(|e| e.parse().ok()).unwrap_or(2015)
        })
    };
    let lib = manifest.get("lib");
    let autolib = manifest
        .get("package")
        .and_then(|p| p.get("autolib"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    let has_lib = lib.is_some_and(|l| l.get("path").is_some())
        || ((lib.is_some() || autolib) && dir.join("src/lib.rs").is_file());
    let mut out: Vec<u32> = has_lib
        .then(|| lib.map_or(package, own))
        .into_iter()
        .collect();
    let bins: Vec<&toml::Value> = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    out.extend(bins.iter().map(|b| own(b)));
    let src = fs::canonicalize(dir.join("src")).ok();
    for kind in ["test", "bench", "example"] {
        out.extend(
            manifest
                .get(kind)
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|t| {
                    t.get("path")
                        .and_then(toml::Value::as_str)
                        .and_then(|p| fs::canonicalize(dir.join(p)).ok())
                        .is_some_and(|p| src.as_ref().is_some_and(|s| p.starts_with(s)))
                })
                .map(own),
        );
    }
    let autobins = manifest
        .get("package")
        .and_then(|p| p.get("autobins"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    if autobins {
        let src = dir.join("src");
        let package_name = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        if src.join("main.rs").is_file() {
            found.push((package_name.to_owned(), src.join("main.rs")));
        }
        if let Ok(rd) = fs::read_dir(src.join("bin")) {
            for e in rd.flatten() {
                let p = e.path();
                let (name, file) = if p.is_dir() {
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        p.join("main.rs"),
                    )
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                    (stem.into_owned(), p)
                } else {
                    continue;
                };
                if file.is_file() {
                    found.push((name, file));
                }
            }
        }
        let declared = |name: &str, file: &Path| {
            bins.iter().any(|b| {
                b.get("name").and_then(toml::Value::as_str) == Some(name)
                    || b.get("path")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|p| canonical(&dir.join(p)) == canonical(file))
            })
        };
        out.extend(
            found
                .iter()
                .filter(|(name, file)| !declared(name, file))
                .map(|_| package),
        );
    }
    // A package with no target the walk reads matches by its own edition.
    if out.is_empty() {
        out.push(package);
    }
    out
}

/// The root files of the crate in `dir`, canonical, as its Cargo.toml
/// declares its targets: the `[lib]` `path`, else `src/lib.rs` unless
/// `autolib = false`; each `path` of a `[[bin]]`, `[[example]]`, `[[test]]`
/// and `[[bench]]`; and, unless `autobins = false`, the binaries cargo
/// finds (`src/main.rs`, each `src/bin/NAME.rs` and `src/bin/NAME/main.rs`).
/// The counter reads only `src`, so the examples, tests and benches cargo
/// finds outside it do not matter. With `autobins = false`, a binary file
/// no `[[bin]]` names is no root, so a module it would hold counts as
/// undeclared, never verified. A manifest it cannot read gives the default
/// targets.
fn crate_roots(dir: &Path) -> Vec<PathBuf> {
    let manifest: toml::Table = fs::read_to_string(dir.join("Cargo.toml"))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    let auto = |key: &str| {
        manifest
            .get("package")
            .and_then(|p| p.get(key))
            .and_then(toml::Value::as_bool)
            .unwrap_or(true)
    };
    let path_of = |t: &toml::Value| {
        t.get("path")
            .and_then(toml::Value::as_str)
            .map(|p| dir.join(p))
    };
    let src = dir.join("src");
    let mut roots = Vec::new();
    match manifest.get("lib").and_then(path_of) {
        Some(p) => roots.push(p),
        None if auto("autolib") => roots.push(src.join("lib.rs")),
        None => {}
    }
    for kind in ["bin", "example", "test", "bench"] {
        if let Some(targets) = manifest.get(kind).and_then(toml::Value::as_array) {
            roots.extend(targets.iter().filter_map(path_of));
        }
    }
    if auto("autobins") {
        roots.push(src.join("main.rs"));
        if let Ok(rd) = fs::read_dir(src.join("bin")) {
            for e in rd.flatten() {
                let p = e.path();
                roots.push(if p.is_dir() { p.join("main.rs") } else { p });
            }
        }
    }
    roots
        .into_iter()
        .filter_map(|r| fs::canonicalize(r).ok())
        .collect()
}

/// [`ModFiles`] for the parsed module files `files`, each with its path,
/// of the crate whose root files are `roots` ([`crate_roots`]). The pass
/// runs twice: the first learns which files a `#[path]` names, so that the
/// second resolves the modules those files declare beside them. Errors
/// are those of the second pass.
fn module_files(
    files: &[(PathBuf, verus_syn::File)],
    roots: Vec<PathBuf>,
    atoms: &Atoms,
    errors: &mut Vec<String>,
) -> ModFiles {
    let mut first = ModFiles {
        roots: roots.clone(),
        ..ModFiles::default()
    };
    for (f, parsed) in files {
        test_module_files(f, parsed, atoms, &mut first, &mut Vec::new());
    }
    let mut out = ModFiles {
        path_named: first.path_named,
        roots,
        ..ModFiles::default()
    };
    for (f, parsed) in files {
        test_module_files(f, parsed, atoms, &mut out, errors);
    }
    out
}

/// Adds to `out` the file `file` when its inner attributes, `attrs`, hold
/// a `cfg` (`#![cfg(target_os = "windows")]` atop a module file or a crate
/// root, which then guards every file under it), and then the files of
/// modules declared in `file` under a `cfg`
/// ([`ModFiles`]), whether it builds them only for tests (`#[cfg(test)]
/// mod name;`, `#[cfg(kani)]`), only for Verus (`#[cfg(verus_keep_ghost)]`,
/// `#[cfg(any(test, verus_keep_ghost))]`), where Verus's build might not
/// compile them (`#[cfg(not(miri))]` with `miri` unsettled), or in every
/// build (`#[cfg(target_arch = "x86_64")]` with it declared, which a
/// function under another architecture's `cfg` in the file conflicts with).
/// It descends into inline modules, `verus!` blocks and other item macros'
/// bodies, whose `cfg`s the declarations inside them inherit; one inside
/// another macro's body is also guarded by [`UNREAD_DECLARATION`]. A
/// `#[cfg_attr(P, path = "..")]` names the module's file where `P` holds
/// and no earlier path attribute's predicate does ([`ordered_paths`], in
/// source order, a bare `#[path]` ending the list), and the default file
/// is the one where none of them applies. A
/// module's file is the one its `#[path]` names, relative to the directory of the
/// module that declares it (`file`'s, for a top-level declaration; an
/// inline module's, inside one), or else `name.rs` or `name/mod.rs` in the
/// directory of the modules the enclosing module declares
/// ([`ModFiles::dir_of`], then each inline module's name, or its `#[path]`
/// from the declaring module's directory). Each is canonical, as `main`
/// compares them. A `cfg` it cannot read is an error, in `errors`, and the
/// module is skipped.
fn test_module_files(
    file: &Path,
    parsed: &verus_syn::File,
    atoms: &Atoms,
    out: &mut ModFiles,
    errors: &mut Vec<String>,
) {
    match attrs_built(&parsed.attrs, atoms) {
        Err(e) => errors.push(format!("{}: {e}", file.display())),
        Ok(_) => {
            let own = cfg_predicates(&parsed.attrs);
            if !own.is_empty()
                && let Ok(f) = fs::canonicalize(file)
            {
                out.inner.push((f, own));
            }
        }
    }
    let canonical = canonical(file);
    let Some(dir) = out.dir_of(&canonical) else {
        return;
    };
    let Some(base) = canonical.parent().map(Path::to_path_buf) else {
        return;
    };
    declared_module_files(file, (&dir, &base), &parsed.items, &[], atoms, out, errors);
}

/// [`test_module_files`] for the items of one module: `dir` is where its
/// `mod name;` files live, `base` the declaring module's directory, which
/// a `#[path]` is relative to, and `guards` the `cfg` predicates of the
/// inline modules and macros around it.
fn declared_module_files(
    file: &Path,
    (dir, base): (&Path, &Path),
    items: &[Item],
    guards: &[String],
    atoms: &Atoms,
    out: &mut ModFiles,
    errors: &mut Vec<String>,
) {
    for item in items {
        let attrs = match item {
            Item::Mod(m) => &m.attrs,
            Item::Macro(m) => &m.attrs,
            _ => continue,
        };
        if let Err(e) = attrs_built(attrs, atoms) {
            errors.push(format!("{}: {e}", file.display()));
            continue;
        }
        let mut inner = guards.to_vec();
        inner.extend(cfg_predicates(attrs));
        if let Item::Macro(m) = item {
            if verus_path(&m.mac.path) {
                // The walk reports a body it cannot parse.
                if let Ok(f) = parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                    declared_module_files(file, (dir, base), &f.items, &inner, atoms, out, errors);
                }
            } else {
                inner.push(UNREAD_DECLARATION.to_owned());
                let mut found = Vec::new();
                macro_module_files(m.mac.tokens.clone(), (dir, base), &mut found);
                let from = canonical(file);
                for (f, named) in found {
                    let Ok(f) = fs::canonicalize(f) else { continue };
                    if named {
                        out.path_named.push(f.clone());
                    }
                    out.declared.push(Declared {
                        file: f,
                        from: from.clone(),
                        guards: inner.clone(),
                    });
                }
            }
            continue;
        }
        let Item::Mod(m) = item else { continue };
        let name = m.ident.to_string();
        if let Some((_, items)) = &m.content {
            // The inline module's directory is chosen as an out-of-line
            // module's file is: by its path attributes in source order.
            for (path, extra) in path_choices(&m.attrs) {
                let sub = match path {
                    Some(p) => base.join(p),
                    None => dir.join(&name),
                };
                let mut guards = inner.clone();
                guards.extend(extra);
                declared_module_files(file, (&sub, &sub), items, &guards, atoms, out, errors);
            }
            continue;
        }
        // Each file the declaration may name, whether a `#[path]` names
        // it, and the predicates beyond `inner` under which it does.
        let mut candidates: Vec<(PathBuf, bool, Vec<String>)> = Vec::new();
        for (path, extra) in path_choices(&m.attrs) {
            match path {
                Some(p) => candidates.push((base.join(p), true, extra)),
                None => candidates.extend(
                    [
                        dir.join(format!("{name}.rs")),
                        dir.join(&name).join("mod.rs"),
                    ]
                    .into_iter()
                    .map(|f| (f, false, extra.clone())),
                ),
            }
        }
        for (f, named, extra) in candidates {
            let Ok(f) = fs::canonicalize(f) else { continue };
            if named {
                out.path_named.push(f.clone());
            }
            let mut guards = inner.clone();
            guards.extend(extra);
            out.declared.push(Declared {
                file: f,
                from: canonical(file),
                guards,
            });
        }
    }
}

/// The places a module with `attrs` may be: each path its path attributes
/// give ([`ordered_paths`]), or `None` for its default place, each with the
/// predicates under which it is the one. rustc takes the first path
/// attribute in effect, in source order: each path is the module's where
/// its predicate holds and no earlier one's does; a bare `#[path]` holds
/// wherever none earlier does and ends the list; the default is the
/// module's where no path applies.
fn path_choices(attrs: &[Attribute]) -> Vec<(Option<String>, Vec<String>)> {
    let excluded = |earlier: &[String]| {
        (!earlier.is_empty()).then(|| format!("not(any({}))", earlier.join(",")))
    };
    let mut out = Vec::new();
    let mut earlier: Vec<String> = Vec::new();
    for (pred, p) in ordered_paths(attrs) {
        let mut extra: Vec<String> = pred.iter().cloned().collect();
        extra.extend(excluded(&earlier));
        out.push((Some(p), extra));
        match pred {
            Some(pred) => earlier.push(pred),
            None => return out,
        }
    }
    out.push((None, excluded(&earlier).into_iter().collect()));
    out
}

/// The path attributes of `attrs` in source order: each `#[path = ".."]`
/// with no predicate, and each `path = ".."` a `cfg_attr` applies, at any
/// depth ([`applied_attrs`]), with the conjunction of the predicates
/// around it.
fn ordered_paths(attrs: &[Attribute]) -> Vec<(Option<String>, String)> {
    applied_attrs(attrs)
        .into_iter()
        .filter_map(|(preds, toks)| {
            path_in_attribute(toks.into_iter().collect())
                .map(|p| ((!preds.is_empty()).then(|| all_of(&preds)), p))
        })
        .collect()
}

/// The files of the `mod name;` declarations in a macro body, `tokens`,
/// at any depth, each with whether a `#[path]` named it: the file a
/// `#[path = ".."]` before the declaration names, relative to `base` (the
/// declaring module's directory), or else `name.rs` or `name/mod.rs` under
/// `dir`. Inside an inline `mod outer { .. }` in the body, both are its
/// directory: `outer` under `dir`, or its own `#[path]` from `base`.
fn macro_module_files(
    tokens: TokenStream,
    (dir, base): (&Path, &Path),
    out: &mut Vec<(PathBuf, bool)>,
) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    // The `#[path]` of the item the attributes so far precede.
    let mut path: Option<String> = None;
    let mut skip = 0;
    for (i, t) in toks.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        match (t, toks.get(i + 1), toks.get(i + 2)) {
            (TokenTree::Punct(p), Some(TokenTree::Group(g)), _)
                if p.as_char() == '#' && g.delimiter() == proc_macro2::Delimiter::Bracket =>
            {
                if let Some(p) = path_in_attribute(g.stream()) {
                    path = Some(p);
                }
                skip = 1;
            }
            (TokenTree::Ident(k), Some(TokenTree::Ident(name)), Some(TokenTree::Punct(p)))
                if k == "mod" && p.as_char() == ';' =>
            {
                match path.take() {
                    Some(p) => out.push((base.join(p), true)),
                    None => {
                        out.push((dir.join(format!("{name}.rs")), false));
                        out.push((dir.join(name.to_string()).join("mod.rs"), false));
                    }
                }
                skip = 2;
            }
            (TokenTree::Ident(k), Some(TokenTree::Ident(name)), Some(TokenTree::Group(g)))
                if k == "mod" && g.delimiter() == proc_macro2::Delimiter::Brace =>
            {
                let sub = match path.take() {
                    Some(p) => base.join(p),
                    None => dir.join(name.to_string()),
                };
                macro_module_files(g.stream(), (&sub, &sub), out);
                skip = 2;
            }
            // A restricted visibility (`pub(crate)`, `pub(super)`,
            // `pub(in path)`) is part of the item the attributes precede.
            (TokenTree::Group(g), _, _)
                if g.delimiter() == proc_macro2::Delimiter::Parenthesis
                    && i > 0
                    && matches!(&toks[i - 1], TokenTree::Ident(v) if v == "pub") => {}
            (TokenTree::Group(g), _, _) => {
                path = None;
                macro_module_files(g.stream(), (dir, base), out);
            }
            (TokenTree::Punct(p), _, _) if p.as_char() == ';' => path = None,
            _ => {}
        }
    }
}

/// The string an attribute's tokens, `path = ".."`, give, if they are a
/// `#[path]`'s.
fn path_in_attribute(tokens: TokenStream) -> Option<String> {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    let [
        TokenTree::Ident(k),
        TokenTree::Punct(eq),
        TokenTree::Literal(l),
    ] = toks.as_slice()
    else {
        return None;
    };
    if k != "path" || eq.as_char() != '=' {
        return None;
    }
    match verus_syn::Lit::new(l.clone()) {
        verus_syn::Lit::Str(s) => Some(s.value()),
        _ => None,
    }
}

/// `file`, canonical, or as given when it cannot be.
fn canonical(file: &Path) -> PathBuf {
    fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf())
}

/// The directory of the modules a module file declares: a `mod.rs`'s (or
/// crate root's) own, else `foo/` beside `foo.rs`.
fn module_dir(file: &Path) -> Option<PathBuf> {
    match file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs" | "lib.rs" | "main.rs") => file.parent().map(Path::to_path_buf),
        _ => file
            .parent()
            .map(|p| p.join(file.file_stem().unwrap_or_default())),
    }
}

/// The `cfg` predicates that must hold for the file `f` to be built, given
/// the module files `mods` records: its inner `#![cfg(..)]`s, and for each
/// declaration of it, that declaration's guards and the declaring file's
/// own ([`module_guards`] of it). A file declared once takes that
/// declaration's; one declared several times is built where any
/// declaration holds, so it takes their `any(all(..), ..)`, and nothing
/// more when one declaration needs nothing. A crate file no declaration
/// names, other than a root, is unsure ([`UNREAD_DECLARATION`]) and takes
/// the guards of each recorded file whose module directory ([`module_dir`])
/// it is under, so one under a test-only module stays test-only. A root
/// has its inner `cfg`s alone.
fn module_guards(f: &Path, mods: &ModFiles) -> Vec<String> {
    guards_of(f, mods, &mut Vec::new())
}

/// [`module_guards`], with `seen` the files whose guards are being read,
/// so that a declaration cycle ends.
fn guards_of(f: &Path, mods: &ModFiles, seen: &mut Vec<PathBuf>) -> Vec<String> {
    if seen.iter().any(|s| s == f) {
        return Vec::new();
    }
    seen.push(f.to_path_buf());
    let mut out: Vec<String> = mods
        .inner
        .iter()
        .filter(|(i, _)| i == f)
        .flat_map(|(_, g)| g.iter().cloned())
        .collect();
    let declarations: Vec<&Declared> = mods.declared.iter().filter(|d| d.file == f).collect();
    if declarations.is_empty() && !mods.roots.iter().any(|r| r == f) {
        // No declaration the counter reads names this file, so nothing
        // shows where it is built: it is unsure, and keeps the guards of
        // the files whose module directory holds it.
        out.push(UNREAD_DECLARATION.to_owned());
        let mut above: Vec<&PathBuf> = mods
            .inner
            .iter()
            .map(|(m, _)| m)
            .chain(mods.declared.iter().map(|d| &d.file))
            .filter(|m| *m != f && module_dir(m).is_some_and(|d| f.starts_with(d)))
            .collect();
        above.sort();
        above.dedup();
        for m in above {
            out.extend(guards_of(m, mods, seen));
        }
    } else if !declarations.is_empty() {
        let alternatives: Vec<Vec<String>> = declarations
            .iter()
            .map(|d| {
                let mut g = d.guards.clone();
                g.extend(guards_of(&d.from, mods, seen));
                g
            })
            .collect();
        match alternatives.as_slice() {
            [one] => out.extend(one.iter().cloned()),
            several if several.iter().any(Vec::is_empty) => {}
            several => out.push(format!(
                "any({})",
                several
                    .iter()
                    .map(|g| format!("all({})", g.join(",")))
                    .collect::<Vec<_>>()
                    .join(",")
            )),
        }
    }
    seen.pop();
    out
}

/// Where the file `f` is built ([`guarded_built`] of its
/// [`module_guards`]): a test-only module inside one Verus might skip is
/// test-only, and one under `target_arch = "x86_64"` inside one under
/// `target_arch = "aarch64"` is built nowhere, so test-only too.
fn module_class(f: &Path, mods: &ModFiles, atoms: &Atoms) -> Result<Built, String> {
    guarded_built(&module_guards(f, mods), atoms)
}

/// Where a `cfg` lets an item be built: by a shipped build, and surely by
/// Verus's; by a shipped build, where Verus's build might not compile it
/// (`unix`, or any atom the configuration does not settle), so nothing
/// proves Verus checked it; only where Verus checks the code
/// (`verus_keep_ghost`), so Verus checks it and no shipped build compiles
/// it; or only for tests (`test`, `kani`, the `test-util` feature, which
/// only tests enable: [`test_util_enablers`] fails the run otherwise),
/// which Verus does not see either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Built {
    Shipped,
    Unsure,
    Verus,
    Test,
}

/// Whether a `cfg` predicate can be true, and can be false, in one kind of
/// build.
#[derive(Clone, Copy)]
struct Can {
    t: bool,
    f: bool,
}

/// [`Can`] for the predicate `p` in a shipped build (`verus` false: none
/// of `atoms`' test or Verus atoms) or a Verus build (the Verus atoms and
/// the ones it is sure to set, and none of the test ones or the ones it
/// never sets). Any other name or feature may be either.
/// `all`, `any` and `not` nest to any depth; a predicate it cannot read
/// is an error.
fn cfg_eval(p: &str, verus: bool, atoms: &Atoms) -> Result<Can, String> {
    let p = p.trim();
    let call = |name: &str| {
        p.strip_prefix(name)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
    };
    if let Some(inner) = call("all") {
        // A conjunction that needs two values of one single-valued key
        // holds in no build, whatever the atoms say on their own.
        if cfg_forced(p, verus, atoms)?.is_none() {
            return Ok(Can { t: false, f: true });
        }
        let mut c = Can { t: true, f: false };
        for a in cfg_args(inner)? {
            let x = cfg_eval(a, verus, atoms)?;
            c.t &= x.t;
            c.f |= x.f;
        }
        return Ok(c);
    }
    if let Some(inner) = call("any") {
        let mut c = Can { t: false, f: true };
        for a in cfg_args(inner)? {
            let x = cfg_eval(a, verus, atoms)?;
            c.t |= x.t;
            c.f &= x.f;
        }
        return Ok(c);
    }
    if let Some(inner) = call("not") {
        let args = cfg_args(inner)?;
        let [a] = args.as_slice() else {
            return Err(format!("unreadable cfg predicate `{p}`: not takes one"));
        };
        let x = cfg_eval(a, verus, atoms)?;
        return Ok(Can { t: x.f, f: x.t });
    }
    let w: String = p.split_whitespace().collect();
    // Outside a string, a name has no parenthesis or comma.
    let bare: String = w.split('"').step_by(2).collect();
    if w.is_empty() || bare.contains(['(', ')', ',']) {
        return Err(format!("unreadable cfg predicate `{p}`"));
    }
    Ok(if atoms.test.contains(&w) {
        Can { t: false, f: true }
    } else if atoms.verus.contains(&w) {
        Can {
            t: verus,
            f: !verus,
        }
    } else if verus && atoms.sets.contains(&w) {
        Can { t: true, f: false }
    } else if verus && atoms.never.contains(&w) {
        Can { t: false, f: true }
    } else {
        Can { t: true, f: true }
    })
}

/// The `cfg` keys rustc gives exactly one value per target (`rustc --print
/// cfg`): `target_family` is not one (emscripten is both `unix` and
/// `wasm`), nor are `feature`, `target_feature` and `target_has_atomic`.
const SINGLE_VALUED: [&str; 8] = [
    "target_arch",
    "target_os",
    "target_env",
    "target_abi",
    "target_vendor",
    "target_endian",
    "target_pointer_width",
    "panic",
];

/// Single-valued keys and the one value a predicate needs each to have.
type Forced = std::collections::BTreeMap<String, String>;

/// The single-valued `key="value"` atoms ([`SINGLE_VALUED`]) the predicate
/// `p` needs to hold, without whitespace: each conjunct's, and those every
/// alternative of an `any` needs. `None` when no build can meet it: it
/// needs two values of one key (`all(target_arch = "x86_64", target_arch
/// = "aarch64")`), or it is an `any` none of whose alternatives can hold
/// (`any()`). An alternative that cannot hold in the build `verus` and
/// `atoms` describe ([`cfg_eval`]: `test`, here) needs nothing of the
/// others, so `all(target_arch = "x86_64", any(target_arch = "aarch64",
/// test))` needs both values. A `not` needs nothing. A predicate it cannot
/// read is an error, as [`cfg_eval`] reads it.
fn cfg_forced(p: &str, verus: bool, atoms: &Atoms) -> Result<Option<Forced>, String> {
    let p = p.trim();
    let call = |name: &str| {
        p.strip_prefix(name)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
    };
    if let Some(inner) = call("all") {
        let mut out = Forced::new();
        for a in cfg_args(inner)? {
            let Some(f) = cfg_forced(a, verus, atoms)? else {
                return Ok(None);
            };
            for (k, v) in f {
                if out.get(&k).is_some_and(|w| *w != v) {
                    return Ok(None);
                }
                out.insert(k, v);
            }
        }
        return Ok(Some(out));
    }
    if let Some(inner) = call("any") {
        let mut out: Option<Forced> = None;
        for a in cfg_args(inner)? {
            if !cfg_eval(a, verus, atoms)?.t {
                continue;
            }
            let Some(f) = cfg_forced(a, verus, atoms)? else {
                continue;
            };
            out = Some(match out {
                None => f,
                Some(o) => o.into_iter().filter(|(k, v)| f.get(k) == Some(v)).collect(),
            });
        }
        return Ok(out);
    }
    if call("not").is_some() {
        return Ok(Some(Forced::new()));
    }
    let w: String = p.split_whitespace().collect();
    Ok(Some(match w.split_once('=') {
        Some((k, v)) if SINGLE_VALUED.contains(&k) => [(k.to_owned(), v.to_owned())].into(),
        _ => Forced::new(),
    }))
}

/// The ways the predicate `p` can hold, each as the atoms of the keys in
/// `multi` it needs true (without whitespace): `all` joins its arguments'
/// ways, `any` lists each argument's, and `not` swaps the two, so an atom
/// under an odd number of `not`s is needed false and adds nothing, and one
/// under an even number is needed true. With `negated`, the ways `p` can
/// fail. A predicate it cannot read is an error.
fn cfg_ways(
    p: &str,
    negated: bool,
    multi: &[String],
) -> Result<Vec<std::collections::BTreeSet<String>>, String> {
    let p = p.trim();
    let call = |name: &str| {
        p.strip_prefix(name)
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
    };
    if let Some(inner) = call("not") {
        let mut out = Vec::new();
        for a in cfg_args(inner)? {
            out.extend(cfg_ways(a, !negated, multi)?);
        }
        return Ok(out);
    }
    for (name, joins) in [("all", !negated), ("any", negated)] {
        let Some(inner) = call(name) else { continue };
        let mut out = vec![std::collections::BTreeSet::new()];
        if !joins {
            out.clear();
        }
        for a in cfg_args(inner)? {
            let ways = cfg_ways(a, negated, multi)?;
            out = if joins {
                out.iter()
                    .flat_map(|o| ways.iter().map(move |w| o.union(w).cloned().collect()))
                    .collect()
            } else {
                out.into_iter().chain(ways).collect()
            };
        }
        return Ok(out);
    }
    let w: String = p.split_whitespace().collect();
    let counts = !negated
        && w.split_once('=')
            .is_some_and(|(k, _)| multi.iter().any(|m| m == k));
    Ok(vec![if counts {
        [w].into()
    } else {
        std::collections::BTreeSet::new()
    }])
}

/// Whether the predicate `p` names atoms of more than one of the proof
/// runs the configuration declares: `verus_sets` holds two values of a
/// single-valued key ([`SINGLE_VALUED`]) when a port runs Verus on several
/// hosts (challoc's `target_arch = "x86_64"` and `target_arch =
/// "aarch64"`), and the counter reads each atom on its own, not each run's
/// set. Under one such atom a function is checked by the run that sets it.
/// When some way the predicate can hold ([`cfg_ways`]) needs two or more
/// distinct ones true (`target_os = "macos"` on a module and `target_arch
/// = "x86_64"` on a function in it, with both oses and both arches
/// declared, or the same with `not(not(target_os = "macos"))`), it may
/// ship on a host no run is, so it is not sure. Alternatives of an `any`
/// are ways of their own: `any(target_arch = "x86_64", target_arch =
/// "aarch64")` needs one atom either way.
fn names_several_runs(p: &str, atoms: &Atoms) -> Result<bool, String> {
    let key = |a: &str| a.split_once('=').map(|(k, _)| k.to_owned());
    let multi: Vec<String> = SINGLE_VALUED
        .iter()
        .filter(|k| {
            atoms
                .sets
                .iter()
                .filter(|a| key(a).as_deref() == Some(**k))
                .count()
                > 1
        })
        .map(|k| (*k).to_owned())
        .collect();
    if multi.is_empty() {
        return Ok(false);
    }
    Ok(cfg_ways(p, false, &multi)?.iter().any(|w| w.len() > 1))
}

/// The arguments of an `all`, `any` or `not`: split at the commas outside
/// any parentheses and string.
fn cfg_args(inner: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0;
    for (i, ch) in inner.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth < 0 {
                    return Err(format!(
                        "unreadable cfg predicate `{inner}`: a `)` too many"
                    ));
                }
            }
            ',' if !quoted && depth == 0 => {
                out.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 || quoted {
        return Err(format!("unreadable cfg predicate `{inner}`: unbalanced"));
    }
    // A trailing comma ends the list; an empty argument before it is an
    // error, read as an empty name.
    if !inner[start..].trim().is_empty() {
        out.push(&inner[start..]);
    }
    Ok(out)
}

/// Where the `cfg` predicate `tokens` lets an item be built ([`Built`]).
/// A shipped item is [`Built::Unsure`] when the predicate can be false in
/// Verus's build: only the atoms the configuration settles there are sure,
/// and only one run's of them at a time ([`names_several_runs`]).
fn cfg_built(tokens: &str, atoms: &Atoms) -> Result<Built, String> {
    let verus = cfg_eval(tokens, true, atoms)?;
    Ok(if cfg_eval(tokens, false, atoms)?.t {
        if verus.f || names_several_runs(tokens, atoms)? {
            Built::Unsure
        } else {
            Built::Shipped
        }
    } else if verus.t {
        Built::Verus
    } else {
        Built::Test
    })
}

/// Each attribute `attrs` apply, through `cfg_attr`s nested to any depth,
/// as rustc expands them: its tokens (`path = ".."`, `cfg(Q)`,
/// `verus_verify`) with the predicates of the `cfg_attr`s around it, all of
/// which must hold for it to apply. `cfg_attr(P, a, b)` applies each of `a`
/// and `b` under `P`; an attribute outside any `cfg_attr` has none.
fn applied_attrs(attrs: &[Attribute]) -> Vec<(Vec<String>, Vec<TokenTree>)> {
    let mut out = Vec::new();
    for a in attrs {
        let toks: Vec<TokenTree> = verus_syn::__private::ToTokens::to_token_stream(&a.meta)
            .into_iter()
            .collect();
        applied(&toks, &[], &mut out);
    }
    out
}

/// [`applied_attrs`] for one attribute's tokens, `toks`, under `preds`.
fn applied(toks: &[TokenTree], preds: &[String], out: &mut Vec<(Vec<String>, Vec<TokenTree>)>) {
    if let [TokenTree::Ident(i), TokenTree::Group(g)] = toks
        && i == "cfg_attr"
        && g.delimiter() == proc_macro2::Delimiter::Parenthesis
    {
        let inner: Vec<TokenTree> = g.stream().into_iter().collect();
        let mut parts = inner.split(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','));
        let Some(predicate) = parts.next() else {
            return;
        };
        let mut preds = preds.to_vec();
        preds.push(
            predicate
                .iter()
                .cloned()
                .collect::<TokenStream>()
                .to_string(),
        );
        for part in parts.filter(|p| !p.is_empty()) {
            applied(part, &preds, out);
        }
        return;
    }
    out.push((preds.to_vec(), toks.to_vec()));
}

/// The conjunction of the non-empty `preds`, as one predicate.
fn all_of(preds: &[String]) -> String {
    match preds {
        [one] => one.clone(),
        _ => format!("all({})", preds.join(",")),
    }
}

/// The predicates of `attrs`' `cfg`s, all of which must hold: each
/// `#[cfg(Q)]`'s `Q`, and each `cfg(Q)` a `cfg_attr` applies under `P`
/// (at any depth, [`applied_attrs`]) as `any(not(P), Q)`, since the item is
/// left out only where `P` holds and `Q` does not.
fn cfg_predicates(attrs: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for (preds, toks) in applied_attrs(attrs) {
        if let [TokenTree::Ident(id), TokenTree::Group(g)] = toks.as_slice()
            && id == "cfg"
            && g.delimiter() == proc_macro2::Delimiter::Parenthesis
        {
            out.push(if preds.is_empty() {
                g.stream().to_string()
            } else {
                format!("any(not({}),{})", all_of(&preds), g.stream())
            });
        }
    }
    out
}

/// Where an item under every one of the `cfg` predicates `guards` is
/// built: their conjunction, read as one predicate, so that two guards
/// needing two values of one single-valued key (`#[cfg(target_arch =
/// "x86_64")]` on a module, `#[cfg(target_arch = "aarch64")]` on a
/// function in it) hold in no build ([`cfg_forced`]).
fn guarded_built(guards: &[String], atoms: &Atoms) -> Result<Built, String> {
    match guards {
        [] => Ok(Built::Shipped),
        [one] => cfg_built(one, atoms),
        _ => cfg_built(&format!("all({})", guards.join(",")), atoms),
    }
}

/// Whether Verus's build is sure to compile an item under every one of
/// the `cfg` predicates `guards`: their conjunction, read as one predicate
/// as [`guarded_built`] reads it, cannot be false there and needs at most
/// one run's atoms ([`names_several_runs`]). Two guards needing two values
/// of one single-valued key, each sure alone, hold in no build.
fn guarded_verus_sure(guards: &[String], atoms: &Atoms) -> Result<bool, String> {
    if guards.is_empty() {
        return Ok(true);
    }
    let all = format!("all({})", guards.join(","));
    let can = cfg_eval(&all, true, atoms)?;
    Ok(can.t && !can.f && !names_several_runs(&all, atoms)?)
}

/// Whether every proof run that compiles a module is sure to compile an
/// item in it: the item under the module's `cfg`s `shared` (those of the
/// file and the modules around it, which every invocation in the module
/// is under too) and its own `own` (its attributes and the `verus!`
/// blocks around it). Their conjunction must be sure
/// ([`guarded_verus_sure`]), and `own` must hold in each run, not only in
/// one: where `verus_sets` holds two values of a single-valued key
/// ([`SINGLE_VALUED`]), each run sets one of them, so an atom of that key
/// is unsettled for `own` unless `shared` forces the key's value
/// ([`cfg_forced`], read with those atoms unsettled as well), which every
/// run compiling the module then has. So
/// `#[cfg(target_arch = "x86_64")] use vstd::prelude::*;` beside an
/// unguarded `verus!` is not sure, as the aarch64 run compiles the
/// invocation without the import, while the same import in a module under
/// `target_arch = "x86_64"` is.
fn every_run_sure(shared: &[String], own: &[String], atoms: &Atoms) -> Result<bool, String> {
    if !guarded_verus_sure(&[shared, own].concat(), atoms)? {
        return Ok(false);
    }
    if own.is_empty() {
        return Ok(true);
    }
    let key = |a: &str| a.split_once('=').map(|(k, _)| k.to_owned());
    let multi: Vec<&str> = SINGLE_VALUED
        .iter()
        .copied()
        .filter(|k| {
            atoms
                .sets
                .iter()
                .filter(|a| key(a).as_deref() == Some(*k))
                .count()
                > 1
        })
        .collect();
    if multi.is_empty() {
        return Ok(true);
    }
    let mut each = atoms.clone();
    each.sets
        .retain(|a| key(a).is_none_or(|k| !multi.contains(&k.as_str())));
    // What `shared` forces is read with those atoms unsettled too: read
    // with them sure, `any(target_arch = "x86_64", not(target_arch =
    // "x86_64"))` would skip its second alternative as unable to hold,
    // and force a value the aarch64 run does not have.
    let forced = match shared {
        [] => Forced::new(),
        _ => cfg_forced(&format!("all({})", shared.join(",")), true, &each)?.unwrap_or_default(),
    };
    for a in &atoms.sets {
        let Some(k) = key(a) else { continue };
        let Some(v) = forced.get(&k).filter(|_| multi.contains(&k.as_str())) else {
            continue;
        };
        if *a == format!("{k}={v}") {
            each.sets.push(a.clone());
        } else {
            each.never.push(a.clone());
        }
    }
    let can = cfg_eval(&format!("all({})", own.join(",")), true, &each)?;
    Ok(can.t && !can.f)
}

/// Where `attrs`' `cfg`s, all of which must hold, let an item be built.
fn attrs_built(attrs: &[Attribute], atoms: &Atoms) -> Result<Built, String> {
    guarded_built(&cfg_predicates(attrs), atoms)
}

/// Whether a `cfg` predicate keeps an item out of shipped builds.
#[cfg(test)]
fn cfg_needs_test(tokens: &str, atoms: &Atoms) -> bool {
    matches!(cfg_built(tokens, atoms), Ok(Built::Test | Built::Verus))
}

/// The packages that a build of the workspace at `root` compiles with one
/// of the test-only `features` (pcre3's `test-util`), each with the
/// feature, so that [`cfg_needs_test`] could not leave its
/// items out, as cargo itself resolves them: `cargo tree` over every
/// member (a plain `cargo build` builds them all and unifies their
/// features), for every target, along normal and build edges only, once
/// with the default features and once with every member's every other
/// feature too ([`member_features`]), as a build such as
/// `--features pcre3-capi/hardened` turns them on. Tests' dev-dependencies
/// are the one place that may enable it, with a member's own `test-util`
/// feature, which forwards it. Reading cargo's resolution rather than the
/// manifests leaves no TOML form (quoting, tables, forwarding features,
/// globbed members) to miss. cargo runs `--locked --offline`: the lockfile
/// is checked in, and `make verus` has fetched every dependency before
/// verus-count runs. Its failure, or no cargo at all, is an error rather
/// than a pass.
fn test_util_enablers(root: &Path, test: &[String]) -> Result<Vec<(String, String)>, String> {
    let features = member_features(root, test)?;
    let mut enablers = Vec::new();
    for extra in [None, Some(features.join(","))] {
        let mut args = vec![
            "tree",
            "--locked",
            "--offline",
            "--workspace",
            "--target",
            "all",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}\t{f}",
        ];
        if let Some(f) = extra.as_deref().filter(|f| !f.is_empty()) {
            args.extend(["--features", f]);
        }
        let out = cargo(root, &args)?;
        enablers.extend(out.lines().flat_map(|l| {
            let (package, features) = l.split_once('\t').unwrap_or_default();
            features
                .trim_end_matches(" (*)")
                .split(',')
                .filter(|f| test.iter().any(|t| t == f.trim()))
                .map(|f| (package.to_string(), f.trim().to_string()))
                .collect::<Vec<_>>()
        }));
    }
    enablers.sort();
    enablers.dedup();
    Ok(enablers)
}

/// Every feature of every workspace member under `root` but the test-only
/// ones (`test`), as `member/feature`, from `cargo metadata`.
fn member_features(root: &Path, test: &[String]) -> Result<Vec<String>, String> {
    let out = cargo(
        root,
        &[
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--no-deps",
        ],
    )?;
    let meta: serde_json::Value = serde_json::from_str(&out)
        .map_err(|e| format!("cargo metadata in {}: {e}", root.display()))?;
    let mut features = Vec::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        let Some(name) = p["name"].as_str() else {
            continue;
        };
        for f in p["features"].as_object().into_iter().flat_map(|o| o.keys()) {
            if !test.contains(f) {
                features.push(format!("{name}/{f}"));
            }
        }
    }
    Ok(features)
}

/// `cargo args` in `root` (cargo from `$CARGO`, as `cargo run` and `cargo
/// test` set it, or else from `PATH`): its standard output, or an error.
fn cargo(root: &Path, args: &[&str]) -> Result<String, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = std::process::Command::new(&cargo)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", cargo.to_string_lossy()))?;
    if !out.status.success() {
        return Err(format!(
            "cargo {} failed in {}: {}",
            args.first().unwrap_or(&""),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `#[cfg_attr(any(), discharged_by = "kani")]` marker: Kani checks
/// this trusted function's contract (verus/README.md, "Kani"). The marker
/// is inert: `discharged_by` is no attribute rustc knows, so it may only
/// sit under predicates that hold in no build. It is read through
/// [`applied_attrs`], so it counts nested or beside other attributes
/// (`cfg_attr(any(), inline, discharged_by = "kani")`), and only where the
/// predicates around it hold in no build whatever the configuration
/// declares (`any()`, or `all(any(), unix)`). Under one that some build
/// may meet (`windows`, `test`) it fails closed: the function is not
/// discharged, as it would not be in that build.
fn is_discharged(attrs: &[Attribute]) -> bool {
    applied_attrs(attrs).into_iter().any(|(preds, toks)| {
        toks.iter()
            .cloned()
            .collect::<TokenStream>()
            .to_string()
            .split_whitespace()
            .collect::<String>()
            == "discharged_by=\"kani\""
            && !preds.is_empty()
            && never_holds(&all_of(&preds))
    })
}

/// Whether the predicate `p` holds in no build, with no atom settled:
/// neither a shipped build nor Verus's can meet it. One it cannot read is
/// taken as one that may hold.
fn never_holds(p: &str) -> bool {
    let none = Atoms::default();
    [false, true]
        .into_iter()
        .all(|verus| cfg_eval(p, verus, &none).is_ok_and(|c| !c.t))
}

/// Whether `attrs` apply `#[name]` in some build: directly, or through a
/// `cfg_attr` whatever its predicate ([`applied_attrs`]).
fn has_applied(attrs: &[Attribute], name: &str) -> bool {
    applied_attrs(attrs)
        .into_iter()
        .any(|(_, toks)| matches!(toks.first(), Some(TokenTree::Ident(i)) if i == name))
}

/// Whether `attrs` make a function external to Verus (`external_body`,
/// `verifier::external`, `verus_verify(external..)`), read through
/// [`applied_attrs`]. It fails closed: an attribute a `cfg_attr` applies
/// counts whatever its predicate, so a function external in some build is
/// trusted, never verified.
fn is_external(attrs: &[Attribute]) -> bool {
    applied_attrs(attrs).into_iter().any(|(_, toks)| {
        // A `name = value` attribute (a doc comment) is read by its name
        // alone, so prose that mentions `external_body` is not one.
        let text = toks
            .into_iter()
            .take_while(|t| !matches!(t, TokenTree::Punct(p) if p.as_char() == '='))
            .collect::<TokenStream>()
            .to_string();
        text.contains("external_body")
            || text.contains("verifier :: external") && !text.contains("external_fn_specification")
            || text.contains("verus_verify (external")
    })
}

/// Whether `attrs` hold Verus's attribute form, which marks a plain Rust
/// function outside `verus!` for verification: `#[verus_verify]` or
/// `#[verus_spec(..)]` (by any path), alone or as an attribute a
/// `cfg_attr` applies (`#[cfg_attr(feature = "verus", verus_spec(..))]`,
/// the form a port whose MSRV is below vstd's uses). Such a function is
/// classed as one inside `verus!` is ([`classify`]): `Some(true)` when
/// Verus's build is sure to apply it, `Some(false)` when it might not, and
/// `None` when it never does. A `cfg_attr` is sure when Verus's build is
/// sure to meet its predicate (by [`cfg_eval`]: it can be true there and
/// cannot be false), as a configured Verus-only feature,
/// `verus_keep_ghost`, a `verus_sets` atom or `not` of a
/// `verus_never_sets` one is. One no build meets (`any()`) or only a test
/// build meets (`test`) never applies the attribute to the code Verus
/// checks. One that names a platform or any other atom the configuration
/// does not settle (`target_os = "windows"`) may not apply it on the host
/// Verus runs on, so the function counts as trusted, as one under a `cfg`
/// Verus might not meet does ([`Built::Unsure`]). A predicate it cannot
/// read is an error. A `cfg_attr`'s predicate is read together with the
/// `cfg`s the function is under, `guards`: one under `target_arch =
/// "x86_64"` whose attribute needs `target_arch = "aarch64"` never applies.
fn verus_attribute(
    attrs: &[Attribute],
    guards: &[String],
    atoms: &Atoms,
) -> Result<Option<bool>, String> {
    const NAMES: [&str; 2] = ["verus_verify", "verus_spec"];
    let mut maybe = false;
    for (preds, toks) in applied_attrs(attrs) {
        // The attribute's path is the tokens before its arguments; it is
        // the attribute form by its last segment.
        let last = toks
            .iter()
            .take_while(|t| {
                !matches!(t, TokenTree::Group(_))
                    && !matches!(t, TokenTree::Punct(p) if p.as_char() == '=')
            })
            .filter_map(|t| match t {
                TokenTree::Ident(i) => Some(i.to_string()),
                _ => None,
            })
            .last();
        if !last.is_some_and(|l| NAMES.contains(&l.as_str())) {
            continue;
        }
        if preds.is_empty() {
            return Ok(Some(true));
        }
        let mut all = guards.to_vec();
        all.extend(preds);
        let all = format!("all({})", all.join(","));
        let can = cfg_eval(&all, true, atoms)?;
        if can.t && !can.f && !names_several_runs(&all, atoms)? {
            return Ok(Some(true));
        }
        maybe |= can.t;
    }
    Ok(maybe.then_some(false))
}

/// The `#[verifier::rlimit(N)]` among `attrs`: `Some(Some(N))`,
/// `Some(None)` for `infinity`, or `None` without one. An argument that
/// is neither a number nor `infinity` is an error.
fn rlimit_attr(attrs: &[Attribute]) -> Result<Option<Option<f32>>, String> {
    for a in attrs {
        let Meta::List(l) = &a.meta else { continue };
        if path_string(&l.path) != "verifier :: rlimit" {
            continue;
        }
        let arg = l.tokens.to_string();
        return match arg.trim() {
            "infinity" => Ok(Some(None)),
            n => n
                .parse::<f32>()
                .map(|v| Some(Some(v)))
                .map_err(|_| format!("#[verifier::rlimit({n})]: not a number or infinity")),
        };
    }
    Ok(None)
}

/// The name Verus gives an `impl`'s type or a trait, from
/// [`Counter::owner`]: its last path segment, without generic arguments
/// (`Machine < 'a , U >` is `Machine`); `None` for free functions.
fn owner_name(owner: &str) -> Option<String> {
    let base = owner.split('<').next().unwrap_or("");
    let last = base.rsplit("::").next().unwrap_or("").trim();
    (!last.is_empty()).then(|| last.to_owned())
}

/// The module path Verus names a file's items under: the crate's name
/// with `-` as `_`, then the file's path under `src/`, less `lib.rs`,
/// `main.rs` and `mod.rs` and the `.rs` extension.
fn module_path(krate: &str, rel: &Path) -> Vec<String> {
    let parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut out = vec![krate.replace('-', "_")];
    for (i, p) in parts.iter().enumerate() {
        if i + 1 < parts.len() {
            out.push(p.clone());
        } else if !matches!(p.as_str(), "lib.rs" | "main.rs" | "mod.rs") {
            out.push(p.trim_end_matches(".rs").to_owned());
        }
    }
    out
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

/// How the tally classes a function, `in_verus` when it is under `verus!`:
/// `None` for a ghost one (spec or proof) that is not a trusted proof, which
/// is counted apart. Shipped code and code built only for Verus are classed
/// by this one rule.
fn classify(
    in_verus: bool,
    sig: &Signature,
    attrs: &[Attribute],
    body: Option<&TokenStream>,
) -> Option<Class> {
    if !in_verus {
        return Some(Class::Outside);
    }
    let trusted_proof = is_trusted_proof(sig, attrs, body);
    if is_ghost(sig) && !trusted_proof {
        return None;
    }
    Some(
        if trusted_proof || is_external(attrs) || body.is_none_or(|b| calls_assume(b.clone())) {
            if is_discharged(attrs) {
                Class::Discharged
            } else {
                Class::Trusted
            }
        } else {
            Class::Verified
        },
    )
}

impl Counter {
    /// The `cfg` predicates an item with `attrs` here is under: those
    /// around it ([`Counter::guards`]) and its own.
    fn guards_with(&self, attrs: &[Attribute]) -> Vec<String> {
        let mut g = self.guards.clone();
        g.extend(cfg_predicates(attrs));
        g
    }

    /// Where an item with `attrs` here is built ([`guarded_built`] of
    /// [`Counter::guards_with`]); a `cfg` it cannot read is an error, and
    /// the item is taken as shipped.
    fn built(&mut self, attrs: &[Attribute]) -> Built {
        match guarded_built(&self.guards_with(attrs), &self.atoms) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", self.file.display());
                self.errors += 1;
                Built::Shipped
            }
        }
    }

    /// Enters code (a module, `impl`, trait or `verus!` block) with `attrs`
    /// whose `cfg` classes it `built`, other than test-only: returns what
    /// [`Counter::leave`] restores.
    fn enter(&mut self, built: Built, attrs: &[Attribute]) -> (bool, bool, usize) {
        let was = (self.shipped, self.sure, self.guards.len());
        self.shipped &= matches!(built, Built::Shipped | Built::Unsure);
        self.sure &= built != Built::Unsure;
        self.guards.extend(cfg_predicates(attrs));
        was
    }

    /// Leaves the code [`Counter::enter`] entered.
    fn leave(&mut self, (shipped, sure, guards): (bool, bool, usize)) {
        (self.shipped, self.sure) = (shipped, sure);
        self.guards.truncate(guards);
    }

    /// Whether a shipped build compiles an item with `attrs` here.
    fn item_ships(&mut self, attrs: &[Attribute]) -> bool {
        self.shipped && matches!(self.built(attrs), Built::Shipped | Built::Unsure)
    }

    fn record(
        &mut self,
        in_verus: bool,
        attrs: &[Attribute],
        sig: &Signature,
        body: Option<TokenStream>,
        start: usize,
        close: proc_macro2::Span,
    ) -> Option<Class> {
        let end = close.end().line;
        let built = self.built(attrs);
        if built == Built::Test {
            return None;
        }
        // Verus's attribute form verifies a function outside `verus!`, if
        // Verus's build applies it.
        let (in_verus, applied) = if in_verus {
            (true, true)
        } else {
            match verus_attribute(attrs, &self.guards_with(attrs), &self.atoms) {
                Ok(Some(sure)) => (true, sure),
                Ok(None) => (false, true),
                Err(e) => {
                    eprintln!("{}:{start}: {e}", self.file.display());
                    self.errors += 1;
                    (false, true)
                }
            }
        };
        // Whether Verus's build is sure to compile this function, and so
        // to check it: no `cfg` on it or around it that Verus's build might
        // not meet, and the attribute form sure to apply.
        let sure = self.sure && built != Built::Unsure && applied;
        // Verus checks code built only for it, so its rlimits count for
        // the headroom check, though the code is not shipped.
        if in_verus {
            let mut name = self.module.clone();
            name.extend(owner_name(&self.owner));
            name.push(sig.ident.to_string());
            let name = name.join("::");
            let at = format!("{}:{start}", self.file.display());
            self.verus_fns
                .entry(name.clone())
                .or_insert_with(|| at.clone());
            match rlimit_attr(attrs) {
                Ok(None) => {}
                Ok(Some(units)) => self.limits.push(rlimit::Limit { name, at, units }),
                Err(e) => {
                    eprintln!("{}:{start}: {e}", self.file.display());
                    self.errors += 1;
                }
            }
        }
        let class = classify(in_verus, sig, attrs, body.as_ref());
        let (at, lines) = match &self.expansion {
            Some(line) => (*line, expansion_lines(sig, body.as_ref(), close)),
            None => (start, end.saturating_sub(start) + 1),
        };
        if built == Built::Verus || !self.shipped {
            // A shipped build compiles none of this code, so its verified,
            // ghost and outside functions leave the tally. A trusted one
            // still counts: a shipped function's proof can rest on it, so
            // it is a seam.
            // With `verus_only_ghost`, its ghost functions join the ghost
            // column, which Verus checks all the same.
            match class {
                Some(class @ (Class::Trusted | Class::Discharged)) => {
                    self.tally(class, sig.ident.to_string(), at, lines);
                }
                None if self.verus_only_ghost => {
                    self.tallies.entry(self.krate.clone()).or_default().ghost += 1;
                }
                _ => {}
            }
            return None;
        }
        let Some(class) = class else {
            self.tallies.entry(self.krate.clone()).or_default().ghost += 1;
            return None;
        };
        // A shipped function Verus's build might not compile ships with
        // nothing to prove it checked: it is trusted.
        let class = if class == Class::Verified && !sure {
            Class::Trusted
        } else {
            class
        };
        // A macro's expansion is counted, not walked: the walks read the
        // configured macros their own way (`[reach]`'s `exports_macro`
        // and `fn_ptr_macro`).
        if self.expansion.is_some() {
            self.tally(class, sig.ident.to_string(), at, lines);
            return None;
        }
        let mut calls = Vec::new();
        let mut names = inputs::Body::default();
        if let Some(b) = &body {
            direct_calls(b.clone(), &mut calls);
            names = inputs::body(b.clone());
            inputs::statics_in(b.clone(), false, &mut self.statics);
            inputs::aliases_in(b.clone(), &mut self.aliases);
        }
        let entry = self.entry(attrs, sig, start);
        self.fns.push(FnRef {
            krate: self.krate.clone(),
            file: self.file.clone(),
            line: start,
            owner: self.owner.clone(),
            name: sig.ident.to_string(),
            class,
            calls,
            body: names,
            params: arity(sig).0,
            receiver: arity(sig).1,
            entry,
        });
        self.tally(class, sig.ident.to_string(), at, lines);
        Some(class)
    }

    /// Counts function `name`, of class `class`, `lines` long and listed at
    /// line `start`, in the tally, lists it under `--list`, and keeps a
    /// discharged one for the record check ([`unrecorded`]).
    fn tally(&mut self, class: Class, name: String, start: usize, lines: usize) {
        if let Some(rows) = &mut self.dry {
            rows.push((class, name, lines));
            return;
        }
        let t = self.tallies.entry(self.krate.clone()).or_default();
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
                name
            );
        }
        if class == Class::Discharged {
            self.discharged.push((self.file.clone(), name));
        } else if class == Class::Trusted {
            self.trusted.push((
                self.krate.clone(),
                format!("{}:{start}", self.file.display()),
                lines,
            ));
        }
    }

    /// Keeps each function a `const` or `static` initializer `expr` names
    /// as a value: a whole argument that is a function's name, as the
    /// walk reads one ([`inputs::Arity::ByName`]), such as `default_malloc`
    /// in `MallocPtr(default_malloc)`. A call, even one of unknown arity
    /// (`helper(make::<u8, u16>())`), does not hand out its callee.
    fn addressed_in(&mut self, expr: Option<&verus_syn::Expr>) {
        let Some(e) = expr else { return };
        let b = inputs::body(verus_syn::__private::ToTokens::to_token_stream(e));
        for s in b.sites {
            if let inputs::Site::Free(n, inputs::Arity::ByName)
            | inputs::Site::Path(_, n, inputs::Arity::ByName) = s
            {
                self.addressed.push((self.krate.clone(), n));
            }
        }
    }

    /// What can be done only once every file is read: the `fn_ptr!`
    /// expansions ([`Counter::expand_fn_ptrs`]), and marking each free
    /// function a `const` or `static` hands out ([`Counter::addressed`])
    /// as an entry point.
    fn finish(&mut self) {
        self.expand_fn_ptrs();
        let addressed: std::collections::BTreeSet<(String, String)> =
            std::mem::take(&mut self.addressed).into_iter().collect();
        for f in &mut self.fns {
            if f.owner.is_empty() && addressed.contains(&(f.krate.clone(), f.name.clone())) {
                f.entry = true;
            }
        }
    }

    /// Keeps a `width::fn_ptr!` invocation for [`Counter::expand_fn_ptrs`].
    fn fn_ptr(&mut self, m: &verus_syn::ItemMacro) {
        let line = m.mac.bang_token.span.start().line;
        self.fn_ptrs.uses.push((
            self.krate.clone(),
            self.file.clone(),
            line,
            m.mac.tokens.clone(),
        ));
    }

    /// Keeps `macro_rules! fn_ptr`'s transcriber, and counts its rules.
    fn fn_ptr_rule(&mut self, tokens: &TokenStream) {
        let toks: Vec<TokenTree> = tokens.clone().into_iter().collect();
        for w in toks.windows(2) {
            if let [TokenTree::Punct(p), TokenTree::Group(g)] = w
                && p.as_char() == '>'
                && g.delimiter() == proc_macro2::Delimiter::Brace
            {
                self.fn_ptrs.rules += 1;
                self.fn_ptrs.rule = Some(g.stream());
            }
        }
    }

    /// The functions each `width::fn_ptr!` invocation writes: its
    /// definition's transcriber, with the invocation's struct bound to its
    /// metavariables, parsed. For pcre3-capi that is the struct's `Clone`
    /// and the `FromC` conversion an export applies to a C function
    /// pointer argument, and whatever those bodies call is walked from
    /// them. They are outside `verus!`, and are recorded for the walks
    /// alone, as nested functions are: no tally counts them. Fails closed:
    /// an invocation with no definition, a definition of more than one
    /// rule, a metavariable the expansion does not bind, or an expansion
    /// that does not parse is an error.
    fn expand_fn_ptrs(&mut self) {
        use verus_syn::__private::ToTokens;
        let uses = std::mem::take(&mut self.fn_ptrs.uses);
        for (krate, file, line, tokens) in uses {
            let at = format!("{}:{line}", file.display());
            let rule = match (&self.fn_ptrs.rule, self.fn_ptrs.rules) {
                (Some(r), 1) => r.clone(),
                (_, n) => {
                    eprintln!("{at}: fn_ptr! has {n} macro_rules rules, not the one it expands");
                    self.errors += 1;
                    continue;
                }
            };
            let expanded = parse2::<verus_syn::ItemStruct>(tokens)
                .map_err(|e| format!("does not declare a struct: {e}"))
                .and_then(|s| {
                    let field = s.fields.iter().next().ok_or("its struct has no field")?;
                    let p = s
                        .generics
                        .type_params()
                        .next()
                        .map(|t| t.ident.to_token_stream());
                    let binds: BTreeMap<&str, Option<TokenStream>> = [
                        ("attr", None),
                        ("vis", Some(s.vis.to_token_stream())),
                        ("name", Some(s.ident.to_token_stream())),
                        ("p", p),
                        ("fn", Some(field.ty.to_token_stream())),
                    ]
                    .into();
                    let src = transcribe(rule, &binds)?;
                    parse2::<verus_syn::File>(src).map_err(|e| format!("does not parse: {e}"))
                });
            let expanded = match expanded {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{at}: fn_ptr!'s expansion {e}");
                    self.errors += 1;
                    continue;
                }
            };
            for item in &expanded.items {
                let Item::Impl(i) = item else { continue };
                let owner = i.self_ty.to_token_stream().to_string();
                for ii in &i.items {
                    let ImplItem::Fn(g) = ii else { continue };
                    let body: TokenStream = g
                        .block
                        .stmts
                        .iter()
                        .map(ToTokens::to_token_stream)
                        .collect();
                    let mut calls = Vec::new();
                    let names = inputs::body(body.clone());
                    direct_calls(body, &mut calls);
                    self.fns.push(FnRef {
                        krate: krate.clone(),
                        file: file.clone(),
                        line,
                        owner: owner.clone(),
                        name: g.sig.ident.to_string(),
                        class: Class::Outside,
                        calls,
                        body: names,
                        params: arity(&g.sig).0,
                        receiver: arity(&g.sig).1,
                        entry: false,
                    });
                }
            }
        }
    }

    /// Whether a function with `attrs` and `sig`, at `line`, is a C entry
    /// point ([`exports_symbol`]). An exported symbol whose ABI C cannot
    /// call ([`is_c_abi`]) is an error.
    fn entry(&mut self, attrs: &[Attribute], sig: &Signature, line: usize) -> bool {
        let entry = exports_symbol(attrs);
        if entry && !is_c_abi(sig) {
            eprintln!(
                "{}:{line}: `{}` exports its symbol but its ABI is not C's (extern \"C\", \"C-unwind\" or bare extern)",
                self.file.display(),
                sig.ident
            );
            self.errors += 1;
        }
        entry
    }

    /// The functions declared in `stmts`, the body of a function of class
    /// `class`, at any depth of its blocks and expressions: free
    /// functions, and the methods of an `impl` written there (such as a
    /// guard's `Drop`), each of that class. They are recorded
    /// for the recursion check ([`recursive`]) and the walks ([`inputs`],
    /// [`reach`]); no tally counts them, as none counts a nested function.
    fn nested(&mut self, stmts: &[verus_syn::Stmt], class: Class) {
        use verus_syn::__private::ToTokens;
        use verus_syn::visit::Visit;
        // The items written anywhere in the body's expressions (an `if`'s
        // block, a closure's, a `match` arm's), not only among its own
        // statements; each one's own body is read in turn below.
        // An item is kept, and an inline module's items, and those in a
        // `const` or `static`'s initializer (`const _: () = { .. };`), are
        // read too; a function's own body is read where it is recorded.
        // Each is kept with the `cfg`s of the items around it in the body
        // and its own.
        struct BodyItems<'a> {
            found: Vec<(&'a Item, Vec<String>)>,
            guards: Vec<String>,
        }
        impl<'a> Visit<'a> for BodyItems<'a> {
            fn visit_item(&mut self, i: &'a Item) {
                let attrs: &[Attribute] = match i {
                    Item::Fn(f) => &f.attrs,
                    Item::Mod(m) => &m.attrs,
                    Item::Impl(m) => &m.attrs,
                    Item::Trait(t) => &t.attrs,
                    Item::Const(c) => &c.attrs,
                    Item::Static(s) => &s.attrs,
                    _ => &[],
                };
                let was = self.guards.len();
                self.guards.extend(cfg_predicates(attrs));
                self.found.push((i, self.guards.clone()));
                match i {
                    Item::Mod(m) => {
                        for inner in m.content.iter().flat_map(|(_, items)| items) {
                            self.visit_item(inner);
                        }
                    }
                    Item::Const(c) => {
                        if let Some(e) = &c.expr {
                            self.visit_expr(e);
                        }
                    }
                    Item::Static(s) => {
                        if let Some(e) = &s.expr {
                            self.visit_expr(e);
                        }
                    }
                    _ => {}
                }
                self.guards.truncate(was);
            }
        }
        let mut found = BodyItems {
            found: Vec::new(),
            guards: Vec::new(),
        };
        for s in stmts {
            found.visit_stmt(s);
        }
        for (item, guards) in found.found {
            let (owner, fns): (String, Vec<NestedFn>) = match item {
                Item::Fn(g) => (String::new(), vec![(&g.attrs[..], &g.sig, &*g.block)]),
                Item::Impl(i) => (
                    i.self_ty.to_token_stream().to_string(),
                    i.items
                        .iter()
                        .filter_map(|ii| match ii {
                            ImplItem::Fn(f) => Some((&f.attrs[..], &f.sig, &f.block)),
                            _ => None,
                        })
                        .collect(),
                ),
                // A trait's default methods.
                Item::Trait(t) => (
                    t.ident.to_string(),
                    t.items
                        .iter()
                        .filter_map(|ti| match ti {
                            TraitItem::Fn(f) => {
                                f.default.as_ref().map(|b| (&f.attrs[..], &f.sig, b))
                            }
                            _ => None,
                        })
                        .collect(),
                ),
                _ => continue,
            };
            for (attrs, sig, block) in fns {
                // One built only for tests (by its own `cfg`, or one of an
                // item around it) ships nothing, and is not recorded; the
                // `cfg`s go on to the functions in its body.
                let was = self.guards.len();
                self.guards.extend(guards.iter().cloned());
                if self.built(attrs) == Built::Test {
                    self.guards.truncate(was);
                    continue;
                }
                self.guards.extend(cfg_predicates(attrs));
                let body: TokenStream = block.stmts.iter().map(ToTokens::to_token_stream).collect();
                let mut calls = Vec::new();
                let names = inputs::body(body.clone());
                direct_calls(body, &mut calls);
                let line = sig.fn_token.span.start().line;
                let entry = self.entry(attrs, sig, line);
                self.fns.push(FnRef {
                    krate: self.krate.clone(),
                    file: self.file.clone(),
                    line,
                    owner: owner.clone(),
                    name: sig.ident.to_string(),
                    class,
                    calls,
                    body: names,
                    params: arity(sig).0,
                    receiver: arity(sig).1,
                    entry,
                });
                self.nested(&block.stmts, class);
                self.guards.truncate(was);
            }
        }
    }

    fn items(&mut self, items: &[Item], in_verus: bool) {
        for item in items {
            // The bindings an expansion writes, which the rules read once
            // the crate is walked ([`Counter::walk_crate`]).
            if let Some(at) = self.expansion {
                let base = Binding {
                    module: self.module.clone(),
                    shipped: true,
                    verus: true,
                    ..Binding::default()
                };
                let mut found = Vec::new();
                match item {
                    Item::Macro(m) if m.mac.path.is_ident("macro_rules") => {
                        if let Some(i) = &m.ident {
                            found.push(Binding {
                                name: i.to_string(),
                                ..base.clone()
                            });
                        }
                    }
                    Item::ExternCrate(e) if has_applied(&e.attrs, "macro_use") => {
                        found.push(Binding {
                            name: e.ident.to_string(),
                            macro_use: true,
                            ..base.clone()
                        });
                    }
                    _ => {}
                }
                let file = self.file.clone();
                let bind_at = BindAt {
                    file: &file,
                    module: self.module.clone(),
                    guards: Vec::new(),
                    shared: 0,
                };
                bindings_in(
                    std::slice::from_ref(item),
                    &bind_at,
                    &self.atoms,
                    &mut found,
                );
                // Every build may compile what an expansion writes: its
                // `cfg`s are not read.
                for b in &mut found {
                    b.expanded = true;
                    b.shipped = true;
                    b.verus = true;
                    b.verus_sure = false;
                    b.place = format!("{}:{at}", file.display());
                }
                self.emitted.extend(found);
            }
            // An expansion is walked for its functions alone
            // ([`Counter::record`]).
            if self.expansion.is_some()
                && !matches!(
                    item,
                    Item::Fn(_) | Item::Impl(_) | Item::Trait(_) | Item::Mod(_) | Item::Macro(_)
                )
            {
                continue;
            }
            match item {
                Item::Fn(f) => {
                    let start = f.sig.fn_token.span.start().line;
                    let end = f.block.brace_token.span.close();
                    let body = Some(
                        f.block
                            .stmts
                            .iter()
                            .map(verus_syn::__private::ToTokens::to_token_stream)
                            .collect(),
                    );
                    if let Some(c) = self.record(in_verus, &f.attrs, &f.sig, body, start, end) {
                        self.nested(&f.block.stmts, c);
                    }
                }
                Item::Impl(i) => {
                    let built = self.built(&i.attrs);
                    if built == Built::Test {
                        continue;
                    }
                    let was = self.enter(built, &i.attrs);
                    let outer = std::mem::replace(
                        &mut self.owner,
                        verus_syn::__private::ToTokens::to_token_stream(&*i.self_ty).to_string(),
                    );
                    for ii in &i.items {
                        if let ImplItem::Fn(f) = ii {
                            let start = f.sig.fn_token.span.start().line;
                            let end = f.block.brace_token.span.close();
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
                            if let Some(c) = self.record(in_verus, &attrs, &f.sig, body, start, end)
                            {
                                self.nested(&f.block.stmts, c);
                            }
                        } else if let ImplItem::Macro(im) = ii {
                            let mut wrap: TokenStream = i
                                .attrs
                                .iter()
                                .filter(|a| is_external(std::slice::from_ref(a)))
                                .map(verus_syn::__private::ToTokens::to_token_stream)
                                .collect();
                            wrap.extend(verus_syn::__private::ToTokens::to_token_stream(
                                &i.impl_token,
                            ));
                            wrap.extend(verus_syn::__private::ToTokens::to_token_stream(
                                &*i.self_ty,
                            ));
                            self.item_macro(&im.attrs, &im.mac, Some(wrap));
                        }
                    }
                    self.owner = outer;
                    self.leave(was);
                }
                Item::Trait(t) => {
                    let built = self.built(&t.attrs);
                    if built == Built::Test {
                        continue;
                    }
                    let was = self.enter(built, &t.attrs);
                    if self.shipped {
                        self.traits.insert(t.ident.to_string());
                    }
                    let outer = std::mem::replace(&mut self.owner, t.ident.to_string());
                    for ti in &t.items {
                        if let TraitItem::Fn(f) = ti
                            && let Some(block) = &f.default
                        {
                            let start = f.sig.fn_token.span.start().line;
                            let end = block.brace_token.span.close();
                            let body = Some(
                                block
                                    .stmts
                                    .iter()
                                    .map(verus_syn::__private::ToTokens::to_token_stream)
                                    .collect(),
                            );
                            if let Some(c) =
                                self.record(in_verus, &f.attrs, &f.sig, body, start, end)
                            {
                                self.nested(&block.stmts, c);
                            }
                        } else if let TraitItem::Macro(tm) = ti {
                            let mut wrap =
                                verus_syn::__private::ToTokens::to_token_stream(&t.trait_token);
                            wrap.extend(verus_syn::__private::ToTokens::to_token_stream(&t.ident));
                            self.item_macro(&tm.attrs, &tm.mac, Some(wrap));
                        }
                    }
                    self.owner = outer;
                    self.leave(was);
                }
                Item::Mod(m) => {
                    let built = self.built(&m.attrs);
                    if built != Built::Test
                        && let Some((_, inner)) = &m.content
                    {
                        let was = self.enter(built, &m.attrs);
                        self.module.push(m.ident.to_string());
                        self.items(inner, in_verus);
                        self.module.pop();
                        self.leave(was);
                    }
                }
                Item::Static(s) if self.item_ships(&s.attrs) => {
                    self.statics.push(inputs::Static {
                        name: s.ident.to_string(),
                        forced: !matches!(s.mutability, verus_syn::StaticMutability::None),
                        ty: verus_syn::__private::ToTokens::to_token_stream(&*s.ty),
                    });
                    self.addressed_in(s.expr.as_deref());
                }
                Item::Const(c) if self.item_ships(&c.attrs) => {
                    self.addressed_in(c.expr.as_deref());
                }
                Item::Struct(s) if self.item_ships(&s.attrs) => {
                    let fields = s
                        .fields
                        .iter()
                        .map(|f| verus_syn::__private::ToTokens::to_token_stream(&f.ty));
                    inputs::add_type(&mut self.types, s.ident.to_string(), fields);
                }
                Item::Enum(e) if self.item_ships(&e.attrs) => {
                    let fields = e
                        .variants
                        .iter()
                        .flat_map(|v| v.fields.iter())
                        .map(|f| verus_syn::__private::ToTokens::to_token_stream(&f.ty));
                    inputs::add_type(&mut self.types, e.ident.to_string(), fields);
                }
                Item::Type(t) if self.item_ships(&t.attrs) => {
                    inputs::aliases_in(
                        verus_syn::__private::ToTokens::to_token_stream(t),
                        &mut self.aliases,
                    );
                    inputs::add_type(
                        &mut self.types,
                        t.ident.to_string(),
                        [verus_syn::__private::ToTokens::to_token_stream(&*t.ty)],
                    );
                }
                Item::Use(u) if self.item_ships(&u.attrs) => {
                    inputs::aliases_in(
                        verus_syn::__private::ToTokens::to_token_stream(u),
                        &mut self.aliases,
                    );
                }
                Item::Macro(m)
                    if m.mac.path.is_ident("thread_local")
                        && self.expansion.is_none()
                        && self.item_ships(&m.attrs) =>
                {
                    inputs::statics_in(m.mac.tokens.clone(), true, &mut self.statics);
                }
                Item::Macro(m)
                    if self.fn_ptr_macro.as_ref().is_some_and(|f| {
                        m.mac.path.segments.last().is_some_and(|s| s.ident == f)
                    }) && self.expansion.is_none()
                        && self.item_ships(&m.attrs) =>
                {
                    self.fn_ptr(m);
                    self.item_macro(&m.attrs, &m.mac, None);
                }
                Item::Macro(m)
                    if m.mac.path.is_ident("macro_rules")
                        && self.expansion.is_none()
                        && self
                            .fn_ptr_macro
                            .as_ref()
                            .is_some_and(|f| m.ident.as_ref().is_some_and(|i| i == f))
                        && self.item_ships(&m.attrs) =>
                {
                    self.fn_ptr_rule(&m.mac.tokens);
                }
                Item::Macro(m) if verus_path(&m.mac.path) => {
                    // A `verus!` block under a `cfg` is built where it says
                    // (`#[cfg(feature = "verus")] verus! { .. }`).
                    let built = self.built(&m.attrs);
                    if built == Built::Test {
                        continue;
                    }
                    // One that may write a function is Verus's only by the
                    // rule of its form ([`Took`]): bare, or a path with a
                    // leading `::`; any other path fails the run.
                    if holds_fn(m.mac.tokens.clone()) {
                        let place = format!(
                            "{}:{}",
                            self.file.display(),
                            m.mac.bang_token.span.start().line
                        );
                        let segs = segments(&m.mac.path);
                        let colon = m.mac.path.leading_colon.is_some();
                        let written =
                            format!("{}{}!", if colon { "::" } else { "" }, segs.join("::"));
                        let (kind, name) = match (colon, segs.len()) {
                            (false, 1) => (Took::VerusBare, "verus".to_owned()),
                            (true, _) => (Took::VerusPath, segs[0].clone()),
                            (false, _) => {
                                eprintln!(
                                    "{place}: {written} may not be Verus's `verus!`: without a leading `::` its first segment may name an item of this crate; write `::{}!` or a bare `verus!`",
                                    segs.join("::")
                                );
                                self.errors += 1;
                                continue;
                            }
                        };
                        if let Some(s) = self.doubt(&self.bindings, kind, &name, &self.module) {
                            eprintln!(
                                "{place}: {written} may not be Verus's `verus!`: {s}; remove or rename it"
                            );
                            self.errors += 1;
                            continue;
                        }
                        self.took.push((kind, place, self.module.clone(), name));
                    }
                    let was = self.enter(built, &m.attrs);
                    match parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                        Ok(f) => self.items(&f.items, true),
                        Err(e) => {
                            eprintln!("{}: verus! block: {e}", self.file.display());
                            self.errors += 1;
                        }
                    }
                    self.leave(was);
                }
                // A `macro_rules!` an expansion writes defines its macro from
                // here on, as rustc's would: kept as written at the outermost
                // invocation's line, in the module the expansion is in.
                Item::Macro(m)
                    if m.mac.path.is_ident("macro_rules")
                        && self.expansion.is_some()
                        && m.ident.is_some() =>
                {
                    let at = self.expansion.unwrap_or_default();
                    let name = m
                        .ident
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    let file = self.file.clone();
                    let mut def = macro_def(
                        m,
                        &DefsAt {
                            file: &file,
                            module: self.module.clone(),
                            built: match (self.shipped, self.sure) {
                                (false, _) => Built::Verus,
                                (true, false) => Built::Unsure,
                                (true, true) => Built::Shipped,
                            },
                            scope: self.module.clone(),
                        },
                        &self.atoms,
                        true,
                    );
                    def.line = at;
                    self.macros.entry(name).or_default().push(def);
                }
                Item::Macro(m) if !m.mac.path.is_ident("macro_rules") => {
                    self.item_macro(&m.attrs, &m.mac, None);
                }
                _ => {}
            }
        }
    }

    /// Reads crate [`Counter::krate`], in directory `k` under `root`, as
    /// every run reads a crate, the working tree's or a base revision's
    /// ([`base_limits`]): its module files placed and classed by their
    /// declarations ([`module_files`], [`module_class`]), a test-only one
    /// left out, a Verus-only one read for its rlimits alone, one Verus's
    /// build might not compile counting no function verified. The crate's
    /// `macro_rules!` definitions are kept first, from every file
    /// ([`macro_defs`]), so that an invocation in one file expands a
    /// definition in another; then each file is walked under its modules'
    /// `cfg`s. `tests`, if given, gathers the `#[test]` functions a ledger
    /// may name.
    fn walk_crate(
        &mut self,
        cfg: &Config,
        (root, k): (&Path, &Path),
        mut tests: Option<&mut std::collections::BTreeSet<String>>,
    ) {
        let src = k.join("src");
        let src = src.as_path();
        let mut paths = Vec::new();
        rust_files(src, &mut paths);
        // A same-length stand-in for an identifier Verus keeps as a keyword
        // (pcre3's `assert::`) keeps the line numbers.
        let read: Vec<(PathBuf, Option<verus_syn::Result<verus_syn::File>>)> = paths
            .into_iter()
            .map(|f| {
                let p = fs::read_to_string(&f)
                    .ok()
                    .map(|t| parse_file(&cfg.stand_in(&t)));
                (f, p)
            })
            .collect();
        let mut ok: Vec<(PathBuf, verus_syn::File)> = Vec::with_capacity(read.len());
        for (f, p) in &read {
            if let Some(Ok(file)) = p {
                if let Some(t) = tests.as_deref_mut() {
                    ledger::test_fns(&ledger::module_path(src, f), &file.items, t);
                }
                ok.push((f.clone(), file.clone()));
            }
        }
        let mut bad = Vec::new();
        let mods = module_files(&ok, crate_roots(k), &self.atoms, &mut bad);
        for e in &bad {
            eprintln!("{e}");
        }
        self.errors += bad.len();
        // Each file's class (a `cfg` [`module_files`] reported counts as
        // shipped) and the `cfg`s of the modules around it.
        let classes: Vec<(Built, Vec<String>)> = read
            .iter()
            .map(|(f, _)| {
                let cf = canonical(f);
                (
                    module_class(&cf, &mods, &self.atoms).unwrap_or(Built::Shipped),
                    module_guards(&cf, &mods),
                )
            })
            .collect();
        let class = |i: usize| classes[i].0;
        let files: Vec<PathBuf> = read.iter().map(|(f, _)| f.clone()).collect();
        let parsed: Vec<Option<verus_syn::Result<verus_syn::File>>> =
            read.into_iter().map(|(_, p)| p).collect();
        self.macros.clear();
        self.uses.clear();
        // The modules a `#[macro_use] mod name;` declares, whose textual
        // scope carries on into the module that declares them.
        let mut carried = std::collections::BTreeSet::new();
        self.modules.clear();
        self.macro_use.clear();
        (self.edition, self.renamed_deps) = crate_manifest(root, k);
        // Macros match by the edition of the target compiling them. The
        // counter does not split the files it reads by target, so targets
        // that read `expr` or `pat` differently fail the run.
        let walked = walked_editions(root, k);
        let era = |e: &u32| expand::matcher_era(*e);
        if walked.iter().map(era).min() != walked.iter().map(era).max() {
            eprintln!(
                "{}: the targets whose files are read (the library, binaries, and any test, bench or example under src/) set editions {walked:?}, which read a macro's `expr` or `pat` fragment differently (2024's `expr` takes `const` blocks and `_`, 2021's `pat` a top-level `|`); give them one side of 2021 and of 2024",
                k.join("Cargo.toml").display()
            );
            self.errors += 1;
        }
        expand::set_edition(walked[0]);
        let mut bindings = Vec::new();
        for (i, (f, p)) in files.iter().zip(&parsed).enumerate() {
            let Some(Ok(file)) = p else { continue };
            let module = module_path(&self.krate, f.strip_prefix(src).unwrap_or(f));
            self.modules.insert(module.clone());
            macro_use_mods(&file.items, &module, (&mut carried, &mut self.modules));
            if class(i) == Built::Test {
                continue;
            }
            // rustc takes a `#[macro_use] extern crate` only at a crate
            // root; every file's is read.
            macro_use_crates(&file.items, &self.atoms, &mut self.macro_use);
            let at = BindAt {
                file: f.strip_prefix(root).unwrap_or(f),
                module,
                guards: classes[i].1.clone(),
                shared: classes[i].1.len(),
            };
            bindings_in(&file.items, &at, &self.atoms, &mut bindings);
        }
        for (i, (f, p)) in files.iter().zip(&parsed).enumerate() {
            let Some(Ok(file)) = p else { continue };
            let module = module_path(&self.krate, f.strip_prefix(src).unwrap_or(f));
            let mut scope = module.clone();
            while carried.contains(&scope) {
                scope.pop();
            }
            let at = DefsAt {
                file: f.strip_prefix(root).unwrap_or(f),
                scope,
                module,
                built: class(i),
            };
            macro_defs(
                &file.items,
                &at,
                &self.atoms,
                (&mut self.macros, &mut self.uses),
            );
        }
        // A definition's module re-exporting it (`pub(crate) use exports;`,
        // or `pub(crate) use ex as build;` under the new name) gives it that
        // module's path, under each name it is exported as.
        for (name, defs) in &mut self.macros {
            for d in defs.iter_mut() {
                let exports: Vec<String> = self
                    .uses
                    .get(&d.module)
                    .into_iter()
                    .flatten()
                    .filter(|(p, glob, _, built)| {
                        !glob
                            && *built != Built::Test
                            && (p.as_slice() == std::slice::from_ref(name)
                                || p == &["self".to_owned(), name.clone()])
                    })
                    .map(|(_, _, named, _)| named.clone())
                    .collect();
                for named in exports {
                    let mut path = d.module.clone();
                    path.push(named);
                    if !d.paths.contains(&path) {
                        d.paths.push(path);
                    }
                }
            }
        }
        self.follow_reexports();
        // Beside the bindings the files write ([`bindings_in`]): the
        // `macro_rules!` and the `#[macro_use]` crates.
        for (name, defs) in &self.macros {
            for d in defs.iter().filter(|d| d.built != Built::Test) {
                bindings.push(Binding {
                    module: d.module.clone(),
                    name: name.clone(),
                    place: format!("`macro_rules! {name}` at {}:{}", d.file.display(), d.line),
                    shipped: matches!(d.built, Built::Shipped | Built::Unsure),
                    verus: true,
                    ..Binding::default()
                });
            }
        }
        for k in &self.macro_use {
            bindings.push(Binding {
                module: vec![self.krate.replace('-', "_")],
                name: k.clone(),
                macro_use: true,
                place: "the crate root".to_owned(),
                shipped: true,
                verus: true,
                ..Binding::default()
            });
        }
        self.bindings = bindings;
        self.emitted.clear();
        self.took.clear();
        for (i, (f, p)) in files.iter().zip(&parsed).enumerate() {
            // A module built only for Verus is read for its rlimits alone,
            // and one Verus's build might not compile counts no function
            // verified. Its items are read under its modules' `cfg`s.
            (self.shipped, self.sure) = match class(i) {
                Built::Test => continue,
                Built::Verus => (false, true),
                Built::Unsure => (true, false),
                Built::Shipped => (true, true),
            };
            self.guards = classes[i].1.clone();
            self.file = f.strip_prefix(root).unwrap_or(f).to_path_buf();
            if self.shipped {
                self.files.push((self.krate.clone(), self.file.clone()));
            }
            self.module = module_path(&self.krate, f.strip_prefix(src).unwrap_or(f));
            match p {
                Some(Ok(file)) => self.items(&file.items, false),
                Some(Err(e)) => {
                    eprintln!("{}:{}: {e}", self.file.display(), e.span().start().line);
                    self.errors += 1;
                }
                None => {
                    eprintln!("{}: unreadable", f.display());
                    self.errors += 1;
                }
            }
        }
        // An invocation counted as another crate's macro whose name this
        // crate defines (a definition reached by a path the counter does
        // not follow, or one an expansion in a file read later writes)
        // fails the run: its functions would otherwise leave the tally.
        for (here, name) in std::mem::take(&mut self.unresolved) {
            // Defined under that name, or exported under it (`pub(crate) use
            // ex as build;`).
            let defined = self.macros.iter().any(|(n, defs)| {
                defs.iter().any(|d| {
                    d.built != Built::Test
                        && (*n == name || d.paths.iter().any(|p| p.last() == Some(&name)))
                })
            });
            if defined {
                eprintln!(
                    "{here}: {name}! resolves to none of this crate's definitions of `{name}`, though rustc may reach one; invoke it by its definition's path, or in that definition's textual scope"
                );
                self.errors += 1;
            }
        }
        // The walk took each `verus!` and `compile_error!` on the sources'
        // bindings; a binding an expansion writes that breaks its rule
        // fails the run, wherever the expansion is.
        let took = std::mem::take(&mut self.took);
        for (kind, at, module, name) in &took {
            if let Some(s) = self.doubt(&self.emitted, *kind, name, module) {
                eprintln!(
                    "{at}: {s} may make this macro another than the {} one the counter took it for; write that binding outside the macro, or remove it",
                    match kind {
                        Took::StdPath | Took::StdBare => "std",
                        Took::VerusPath | Took::VerusBare => "Verus",
                    }
                );
                self.errors += 1;
            }
        }
    }

    /// Follows the crate's `use` chains to its macros: a module's `use` of
    /// a path that names a definition (`pub(crate) use crate::v::build;`)
    /// gives the definition that module's path under the name the `use`
    /// brings in, and a module's `use p::*;` gives each definition `p`
    /// names its path there too, until no `use` adds a path. A `use` built
    /// only for tests adds none.
    fn follow_reexports(&mut self) {
        // Each pass can only add paths; a chain is no longer than the
        // crate has modules.
        for _ in 0..=self.modules.len() {
            let mut add: Vec<(String, usize, Vec<String>)> = Vec::new();
            for (module, uses) in &self.uses {
                for (p, glob, named, built) in uses {
                    if *built == Built::Test {
                        continue;
                    }
                    for (name, defs) in &self.macros {
                        for (i, d) in defs.iter().enumerate() {
                            let new: Vec<Vec<String>> = d
                                .paths
                                .iter()
                                .filter_map(|dp| match glob {
                                    false => names_from(module, p, dp)
                                        .then(|| [module.clone(), vec![named.clone()]].concat()),
                                    true => {
                                        let (last, parent) = dp.split_last()?;
                                        names_from(module, p, parent)
                                            .then(|| [module.clone(), vec![last.clone()]].concat())
                                    }
                                })
                                .filter(|n| !d.paths.contains(n))
                                .collect();
                            add.extend(new.into_iter().map(|n| (name.clone(), i, n)));
                        }
                    }
                }
            }
            if add.is_empty() {
                return;
            }
            for (name, i, path) in add {
                if let Some(d) = self.macros.get_mut(&name).and_then(|v| v.get_mut(i))
                    && !d.paths.contains(&path)
                {
                    d.paths.push(path);
                }
            }
        }
    }

    /// The module of the crate that `segs`, written in this module, names:
    /// through `crate`, `self` or `super`, or a module beside this one or
    /// at the root.
    fn module_of(&self, segs: &[String]) -> Option<Vec<String>> {
        self.module_from(&self.module, segs)
    }

    /// The module of the crate that `segs`, written in module `here`,
    /// names ([`Counter::module_of`]).
    fn module_from(&self, here: &[String], segs: &[String]) -> Option<Vec<String>> {
        let up = segs.iter().take_while(|s| *s == "super").count();
        let candidates = match segs.first().map(String::as_str) {
            Some("crate") => vec![[here.get(..1)?, &segs[1..]].concat()],
            Some("self") => vec![[here, &segs[1..]].concat()],
            Some("super") => {
                let base = here.len().checked_sub(up)?;
                vec![[&here[..base], &segs[up..]].concat()]
            }
            _ => vec![[here, segs].concat(), [here.get(..1)?, segs].concat()],
        };
        candidates.into_iter().find(|m| self.modules.contains(m))
    }

    /// Counts the functions macro invocation `mac`, an item or an `impl`'s
    /// or trait's item with `attrs`, writes, so that none leaves the tally
    /// for being written by a macro. In an `impl` or trait, `wrap` is the
    /// header (`impl Type`, `trait Name`, with an `impl`'s `external`
    /// attribute) its expansion is read inside. A `macro_rules!`
    /// definition of the crate ([`Counter::macros`]) resolves it, as rustc
    /// resolves it ([`Counter::resolve`]); several must count the same
    /// rows, class and lines included ([`Counter::dry_rows`]), else it is
    /// an error. Its expansion
    /// ([`expand::Rules::expand`]) is walked as code outside `verus!`
    /// (Verus's `verus!` does not expand the macros it holds; a `verus!`
    /// block the expansion writes is read as one), and each function in it
    /// is counted, listed at the invocation's line, with its lines from the
    /// invocation and the definition ([`expansion_lines`]). A macro the
    /// crate does not define cannot be read, so the invocation counts as
    /// one function outside `verus!`, named `NAME!`, over the invocation's
    /// lines. A definition that resolves but does not expand, or an
    /// expansion that does not parse, is an error. `thread_local!` writes
    /// statics, not functions.
    fn item_macro(
        &mut self,
        attrs: &[Attribute],
        mac: &verus_syn::Macro,
        wrap: Option<TokenStream>,
    ) {
        let Some(name) = mac.path.segments.last().map(|s| s.ident.to_string()) else {
            return;
        };
        if name == "thread_local" {
            return;
        }
        let built = self.built(attrs);
        if built == Built::Test {
            return;
        }
        let bang = mac.bang_token.span;
        let line = bang.start().line;
        let outer = self.expansion.is_none();
        let at = *self.expansion.get_or_insert(line);
        // The expansion is read under the invocation's `cfg`s: one Verus's
        // build might not meet ([`Built::Unsure`]) leaves its functions
        // shipped but not sure, so trusted, never verified.
        let was = self.enter(built, attrs);
        // Each definition the invocation could expand, by where it is.
        let here = format!("{}:{line}", self.file.display());
        let found: Vec<(String, Result<expand::Rules, String>)> =
            match self.resolve(&name, &mac.path, at) {
                Ok(defs) => defs
                    .into_iter()
                    .map(|d| (format!("{}:{}", d.file.display(), d.line), d.rules.clone()))
                    .collect(),
                Err(e) => {
                    eprintln!("{here}: {e}");
                    self.errors += 1;
                    self.leave(was);
                    if outer {
                        self.expansion = None;
                    }
                    return;
                }
            };
        // The expansion by `rules`, read as items (inside `wrap`, in an
        // `impl` or trait).
        let expand = |rules: &expand::Rules| {
            rules.expand(mac.tokens.clone()).and_then(|t| {
                let src = match &wrap {
                    Some(w) => {
                        let mut s = w.clone();
                        s.extend([TokenTree::Group(proc_macro2::Group::new(
                            proc_macro2::Delimiter::Brace,
                            t,
                        ))]);
                        s
                    }
                    None => t,
                };
                parse2::<verus_syn::File>(src).map_err(|e| e.to_string())
            })
        };
        match found {
            defs if defs.is_empty() => {
                // A name the module imports from this crate (`use
                // crate::w::ex as build;`, `use w::ex;` with `w` a module of
                // the crate) is the crate's macro, by a path the counter did
                // not follow. Any other is checked, under the name it was
                // imported as, once the crate is read.
                let imported = self.imported(&name, false);
                if imported.iter().any(|p| self.local(p)) {
                    eprintln!(
                        "{here}: {name}! is imported from this crate by a path the counter does not follow; invoke it by its definition's path"
                    );
                    self.errors += 1;
                } else {
                    self.unresolved.push((here.clone(), name.clone()));
                    for p in &imported {
                        if let Some(orig) = p.last()
                            && *orig != name
                        {
                            self.unresolved.push((here.clone(), orig.clone()));
                        }
                    }
                }
                if self.shipped {
                    if let Some((kind, read)) = self.std_writes_nothing(&name, &mac.path) {
                        self.took
                            .push((kind, here.clone(), self.module.clone(), read));
                    } else {
                        let mut seen = std::collections::BTreeSet::new();
                        place(bang, &mut seen);
                        token_lines(mac.tokens.clone(), &mut seen);
                        place(mac.delimiter.span().close(), &mut seen);
                        self.tally(Class::Outside, format!("{name}!"), at, seen.len());
                    }
                }
            }
            _ if self.depth >= MAX_DEPTH => {
                eprintln!("{here}: {name}! expands more than {MAX_DEPTH} macros deep");
                self.errors += 1;
            }
            defs => {
                // Each definition that could be the one rustc expands here:
                // more than one (a pair under `cfg`s the counter cannot
                // settle) must write the same functions, or the count would
                // depend on which.
                let mut files = Vec::new();
                for (at, rules) in &defs {
                    match rules
                        .as_ref()
                        .map_err(|e| (true, e.clone()))
                        .and_then(|r| expand(r).map_err(|e| (false, e)))
                    {
                        Ok(f) => files.push(f),
                        Err((true, e)) => {
                            eprintln!("{here}: {name}!'s definition at {at} cannot be read: {e}");
                            self.errors += 1;
                        }
                        Err((false, e)) => {
                            eprintln!("{here}: {name}! does not expand: {e}");
                            self.errors += 1;
                        }
                    }
                }
                // The rows each writes, class and lines too, from a walk
                // that leaves the count as it was.
                let rows: Vec<Option<Vec<(Class, String, usize)>>> = if files.len() > 1 {
                    files.iter().map(|f| self.dry_rows(f)).collect()
                } else {
                    Vec::new()
                };
                let at: Vec<&str> = defs.iter().map(|(a, _)| a.as_str()).collect();
                if rows.iter().any(Option::is_none) {
                    eprintln!(
                        "{here}: {name}! could be any of {} definitions ({}), and one of them does not count (above)",
                        defs.len(),
                        at.join(", ")
                    );
                    self.errors += 1;
                } else if rows.windows(2).any(|w| w[0] != w[1]) {
                    eprintln!(
                        "{here}: {name}! could be any of {} definitions ({}), which count differently",
                        defs.len(),
                        at.join(", ")
                    );
                    self.errors += 1;
                } else if files.len() == defs.len()
                    && let Some(f) = files.first()
                {
                    self.depth += 1;
                    self.items(&f.items, false);
                    self.depth -= 1;
                }
            }
        }
        self.leave(was);
        if outer {
            self.expansion = None;
        }
    }

    /// The definitions of macro `name` that an invocation by `path`, at
    /// line `at` of the walk's file, may expand, as rustc resolves it,
    /// among those this build compiles:
    ///
    /// - a path of more than one segment (`width::exports!`,
    ///   `$crate::m!`) names the definitions it is a path of: the crate
    ///   root's for `#[macro_export]`, a module's for its `use NAME;`;
    /// - a bare name takes the last definition before it in its file whose
    ///   textual scope it is in, with each before that while the later is
    ///   under a `cfg` the counter cannot settle ([`attrs_unsure`]);
    /// - failing that, each definition the module imports by name or `*`,
    ///   each in another file whose textual scope reaches here (an
    ///   enclosing module's, or one `#[macro_use] mod` carries out), and,
    ///   at the crate root, each `#[macro_export]` puts there.
    ///
    /// It is an error when some definition is found and an imported path
    /// the invocation may stand for (a module alias's, or a `use` of the
    /// name) names none: the build that takes that path would expand what
    /// the counter cannot see.
    fn resolve(
        &self,
        name: &str,
        path: &verus_syn::Path,
        at: usize,
    ) -> Result<Vec<&MacroDef>, String> {
        // A definition under a `cfg` the counter cannot settle may be the
        // one: it is a candidate.
        let active = |d: &&MacroDef| match d.built {
            Built::Shipped | Built::Unsure => true,
            Built::Verus => !self.shipped,
            Built::Test => false,
        };
        // The definitions that `path`, read from this module, names: of any
        // name, as a re-export under a new name (`pub(crate) use ex as
        // build;`) makes `w::build` a path of `ex`.
        let by_path = |path: &[String]| -> Vec<&MacroDef> {
            self.macros
                .values()
                .flatten()
                .filter(active)
                .filter(|d| d.paths.iter().any(|p| names(&self.module, path, p)))
                .collect()
        };
        // The definitions each of `paths` names, and the paths that name
        // none.
        let gather = |paths: Vec<Vec<String>>| -> (Vec<&MacroDef>, Vec<String>) {
            let mut found = Vec::new();
            let mut missing = Vec::new();
            for p in paths {
                let named = by_path(&p);
                if named.is_empty() {
                    missing.push(p.join("::"));
                }
                found.extend(named);
            }
            (found, missing)
        };
        // An imported path that names none while another names some.
        let missed = |missing: &[String], any: bool| match missing.first() {
            Some(m) if any => Err(format!(
                "{name}! may stand for `{m}`, which names none of this crate's definitions of `{name}` by a path the counter follows, where another path it may stand for does; invoke it by its definition's path"
            )),
            _ => Ok(()),
        };
        let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segs.len() > 1 {
            // A first segment the module imports (`use crate::w as x;`)
            // stands for each path it imports: under `cfg`s the counter
            // cannot settle there may be several, and each one's
            // definitions are candidates, compared as any others are.
            // A module of the crate of that name, beside this one or at the
            // root, is one more: the path as written.
            let imported = self.imported(&segs[0], false);
            if imported.is_empty() {
                let found = by_path(&segs);
                // A module of the crate that brings the name in by a `use`
                // the counter could not follow to a definition holds the
                // crate's macro out of sight.
                if found.is_empty()
                    && let Some(m) = self.module_of(&segs[..segs.len() - 1])
                    && self.binds(&m, name)
                {
                    return Err(format!(
                        "{}! names `{name}` in `{}`, which brings it in by a `use` the counter does not follow; invoke it by its definition's path",
                        segs.join("::"),
                        m.join("::")
                    ));
                }
                return Ok(found);
            }
            let mut paths: Vec<Vec<String>> = imported
                .into_iter()
                .map(|p| [p, segs[1..].to_vec()].concat())
                .collect();
            if self.local(&segs[..1]) {
                paths.push(segs.clone());
            }
            let (found, missing) = gather(paths);
            missed(&missing, !found.is_empty())?;
            return Ok(joined(Vec::new(), found));
        }
        let defs: &[MacroDef] = self.macros.get(name).map_or(&[], Vec::as_slice);
        let visible = |d: &&MacroDef| self.module.starts_with(&d.scope);
        // Latest first: by line, an emitted definition after any written
        // on its line, and among equals (the definitions one expansion
        // writes, all at its invocation's line) the one registered later.
        let mut before: Vec<(usize, &MacroDef)> = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| active(d) && visible(d))
            .filter(|(_, d)| d.file == self.file && (d.line < at || (d.emitted && d.line == at)))
            .collect();
        before.sort_by_key(|(i, d)| std::cmp::Reverse((d.line, d.emitted, *i)));
        let before: Vec<&MacroDef> = before.into_iter().map(|(_, d)| d).collect();
        // When every one is under such a `cfg`, a build may compile none of
        // them, and then the definitions below are rustc's: they join.
        let textual: Vec<&MacroDef> = match before.iter().position(|d| !d.unsure) {
            Some(i) => return Ok(before[..=i].to_vec()),
            None => before,
        };
        let others: Vec<&MacroDef> = defs
            .iter()
            .filter(active)
            .filter(|d| {
                (d.file != self.file && visible(d)) || (d.exported && self.module.len() == 1)
            })
            .collect();
        // A `*` may hold the name or not, so only a `use` of the name is
        // held to naming a definition. But a `*` of a module of the crate
        // that re-exports the name by a `use` the counter could not follow
        // to a definition ([`Counter::follow_reexports`]) does hold it, and
        // what it brings in is out of sight: an error.
        let (globbed, _) = gather(self.imported(name, true));
        for p in self.globs() {
            if by_path(&[p.clone(), vec![name.to_owned()]].concat()).is_empty()
                && let Some(m) = self.module_of(&p)
                && self.binds(&m, name)
            {
                return Err(format!(
                    "{name}! may come through `{}::*`, which re-exports `{name}` by a path the counter does not follow; invoke it by its definition's path",
                    p.join("::")
                ));
            }
        }
        let (named, missing) = gather(self.imported(name, false));
        let out = joined(joined(joined(textual, others), globbed), named);
        missed(&missing, !out.is_empty())?;
        Ok(out)
    }

    /// Whether module `m` brings `name` in by a `use` of it (not a `*`, and
    /// not one built only for tests).
    fn binds(&self, m: &[String], name: &str) -> bool {
        self.uses.get(m).is_some_and(|us| {
            us.iter()
                .any(|(_, glob, named, built)| !glob && *built != Built::Test && named == name)
        })
    }

    /// The paths this module's `use p::*;`s glob, where the code walked can
    /// be built with them ([`Counter::imported`]).
    fn globs(&self) -> Vec<Vec<String>> {
        self.uses
            .get(&self.module)
            .into_iter()
            .flatten()
            .filter(|(_, glob, _, built)| {
                *glob
                    && match built {
                        Built::Shipped | Built::Unsure => true,
                        Built::Verus => !self.shipped,
                        Built::Test => false,
                    }
            })
            .map(|(p, ..)| p.clone())
            .collect()
    }

    /// The paths, as written, of what this module's `use`s bring into
    /// scope as `name`: the path of a `use p::name;` or `use p::orig as
    /// name;`, and with `globs`, `p::name` for each `use p::*;`. A `use`
    /// is one only where the code walked can be built with it: a test-only
    /// `use` never, a Verus-only one in Verus-only code, any other (one
    /// under a `cfg` the counter cannot settle too) always.
    fn imported(&self, name: &str, globs: bool) -> Vec<Vec<String>> {
        self.uses
            .get(&self.module)
            .into_iter()
            .flatten()
            .filter(|(_, _, _, built)| match built {
                Built::Shipped | Built::Unsure => true,
                Built::Verus => !self.shipped,
                Built::Test => false,
            })
            .filter_map(|(p, glob, named, _)| match glob {
                true => globs.then(|| [p.clone(), vec![name.to_owned()]].concat()),
                false => (named == name).then(|| p.clone()),
            })
            .collect()
    }

    /// Why the rule of `kind` ([`Took`]) does not hold for `name` (the
    /// leading `::` segment, or the bare macro's name) invoked in module
    /// `at`, given `bindings`, if it does not: for a leading `::`, an
    /// edition before 2018 (where `::name` is this crate's root) or a
    /// dependency renamed to `name` ([`Counter::renamed_deps`]); then the
    /// first binding that breaks it ([`breaks`]), read in Verus's build for
    /// `verus!` and a shipped build for `compile_error!`.
    fn doubt(&self, bindings: &[Binding], kind: Took, name: &str, at: &[String]) -> Option<String> {
        if matches!(kind, Took::VerusPath | Took::StdPath) {
            if self.edition < 2018 {
                return Some(format!(
                    "the crate's edition is {}, where `::{name}` names this crate's root module",
                    self.edition
                ));
            }
            if let Some((_, why)) = self.renamed_deps.iter().find(|(n, _)| n == name) {
                return Some(why.clone());
            }
        }
        // A bare `verus!` comes from Verus's crates (`vstd::prelude::*`,
        // `#[macro_use] extern crate vstd;`): one renamed to another
        // package may bring another `verus`.
        if kind == Took::VerusBare
            && let Some((_, why)) = self
                .renamed_deps
                .iter()
                .find(|(n, _)| matches!(n.as_str(), "vstd" | "verus_builtin_macros"))
        {
            return Some(why.clone());
        }
        // A bare `compile_error!` comes from the prelude of the crate the
        // name `std` (or `core`) loads: one renamed to another package
        // (cargo passes it as `--extern std=..`) brings its own.
        if kind == Took::StdBare
            && let Some((_, why)) = self
                .renamed_deps
                .iter()
                .find(|(n, _)| matches!(n.as_str(), "std" | "core" | "alloc"))
        {
            return Some(why.clone());
        }
        let broken = bindings
            .iter()
            .filter(|b| match kind {
                Took::StdPath | Took::StdBare => b.shipped,
                Took::VerusPath | Took::VerusBare => b.verus,
            })
            .find(|b| breaks(b, kind, name, at))
            .map(Binding::describe);
        if broken.is_some() || kind != Took::VerusBare {
            return broken;
        }
        self.unsettled(bindings, at, &mut std::collections::BTreeSet::new())
            .map(|b| {
                format!(
                    "{} may bring another crate's `verus`, and no import of Verus's (`use vstd::prelude::*;` or `use vstd::prelude::verus;`) beside it settles the name",
                    b.describe()
                )
            })
    }

    /// The `*` a bare `verus!` in module `at` may take another crate's
    /// `verus` through (a dependency's own macro of that name), if any,
    /// among the crate's bindings and `extra`'s, read in Verus's build: a
    /// `*` of another crate, or of a module of this one where such a `*`
    /// is, unless the module that holds it also imports Verus's where
    /// every proof run that compiles the module is sure to compile the
    /// import ([`every_run_sure`]). A `use`
    /// of Verus's by name shadows a `*`, and a `*` of Verus's beside one
    /// that brought another would make the name ambiguous, which rustc
    /// refuses. `seen` holds the modules already read.
    fn unsettled<'a>(
        &'a self,
        extra: &'a [Binding],
        at: &[String],
        seen: &mut std::collections::BTreeSet<Vec<String>>,
    ) -> Option<&'a Binding> {
        if !seen.insert(at.to_vec()) {
            return None;
        }
        let here: Vec<&Binding> = self
            .bindings
            .iter()
            .chain(extra)
            .filter(|b| b.verus && b.module == at)
            .collect();
        // A `*` that may bring another `verus` is read where Verus's build
        // may compile it; an import that settles the name, only where that
        // build is sure to.
        let confirmed = here.iter().any(|b| {
            b.verus_sure
                && match b.glob {
                    true => one_of(&b.path, &VERUS_GLOBS),
                    false => b.name == "verus" && one_of(&b.path, &VERUS_PATHS),
                }
        });
        if confirmed {
            return None;
        }
        for b in here
            .into_iter()
            .filter(|b| b.glob && !one_of(&b.path, &VERUS_GLOBS))
        {
            // Without `crate`, `self` or `super`, a path names a module of
            // this crate only beside `at`, and with a leading `::` never
            // (it names an extern crate); in edition 2015 either is from the
            // crate root. Any other is another crate's.
            let local = match b.path.first().map(String::as_str) {
                Some("crate" | "self" | "super") if !b.absolute => self.module_from(at, &b.path),
                _ if self.edition < 2018 => Some([&at[..1], &b.path[..]].concat()),
                _ if b.absolute => None,
                _ => Some([at, &b.path[..]].concat()),
            }
            .filter(|m| self.modules.contains(m));
            match local {
                Some(m) => {
                    if let Some(far) = self.unsettled(extra, &m, seen) {
                        return Some(far);
                    }
                }
                None => return Some(b),
            }
        }
        None
    }

    /// How item macro `name`, invoked by `path` and defined nowhere in the
    /// crate, is one of the standard library's that write nothing that
    /// ships ([`STD_NO_FN`]), if it is: by `::core::` or `::std::`, or by
    /// its bare name, where the rule of that form holds ([`Took`]).
    /// Anything else counts as a function.
    fn std_writes_nothing(&self, name: &str, path: &verus_syn::Path) -> Option<(Took, String)> {
        let (_, paths) = STD_NO_FN.iter().find(|(n, _)| *n == name)?;
        let segs = segments(path);
        let (kind, read) = if segs.len() == 1 && path.leading_colon.is_none() {
            (Took::StdBare, name.to_owned())
        } else if path.leading_colon.is_some() && one_of(&segs, paths) {
            (Took::StdPath, segs[0].clone())
        } else {
            return None;
        };
        self.doubt(&self.bindings, kind, &read, &self.module)
            .is_none()
            .then_some((kind, read))
    }

    /// Whether `path`, a non-`*` `use` path in this module, names something
    /// of this crate: it starts at `crate`, `self` or `super`, or at a
    /// module of the crate, beside this one or at the root.
    fn local(&self, path: &[String]) -> bool {
        match path.first().map(String::as_str) {
            Some("crate" | "self" | "super") => true,
            Some(first) => {
                let here = [self.module.clone(), vec![first.to_owned()]].concat();
                let root = [self.module[..1].to_vec(), vec![first.to_owned()]].concat();
                self.modules.contains(&here) || self.modules.contains(&root)
            }
            None => false,
        }
    }

    /// The rows `f`, an expansion, writes (class, name and lines), from a
    /// walk of it that leaves the count as it was.
    fn dry_rows(&mut self, f: &verus_syn::File) -> Option<Vec<(Class, String, usize)>> {
        let tallies = self.tallies.clone();
        let (limits, errors, unresolved) = (self.limits.len(), self.errors, self.unresolved.len());
        let verus_fns = self.verus_fns.clone();
        let macros = self.macros.clone();
        let outer = self.dry.replace(Vec::new());
        self.depth += 1;
        self.items(&f.items, false);
        self.depth -= 1;
        let rows = std::mem::replace(&mut self.dry, outer).unwrap_or_default();
        // A candidate whose walk failed (a nested invocation that does not
        // expand, or is itself ambiguous) has no rows to compare: `None`.
        let failed = self.errors > errors;
        self.tallies = tallies;
        self.limits.truncate(limits);
        self.unresolved.truncate(unresolved);
        // The bindings each candidate writes, and the blocks it took on
        // their rules, are kept: any of them may be the one rustc
        // expands, so each is checked once the crate is walked.
        self.errors = errors;
        self.verus_fns = verus_fns;
        self.macros = macros;
        (!failed).then_some(rows)
    }
}

/// `out` with each of `more` it does not hold yet, in order.
fn joined<'a>(mut out: Vec<&'a MacroDef>, more: Vec<&'a MacroDef>) -> Vec<&'a MacroDef> {
    for d in more {
        if !out.iter().any(|t| std::ptr::eq(*t, d)) {
            out.push(d);
        }
    }
    out
}

/// Whether `segs`, a path written in module `module`, names `full`, a
/// path from the crate root: through `crate::`, `self::` or `super::`
/// exactly, else as its last segments, which a `use` of a parent brings
/// in scope (`width::exports` in a module that has `width`).
fn names(module: &[String], segs: &[String], full: &[String]) -> bool {
    match segs.first().map(String::as_str) {
        Some("crate") => full.get(1..) == segs.get(1..),
        Some("self" | "super") => {
            let up = segs.iter().take_while(|s| *s == "super").count();
            let rest = &segs[up.max(usize::from(segs[0] == "self"))..];
            let Some(base) = module.len().checked_sub(up) else {
                return false;
            };
            [&module[..base], rest].concat() == full
        }
        _ => full.ends_with(segs),
    }
}

/// [`names`], with a path from neither `crate`, `self` nor `super` read
/// as from `module` or from the crate root alone, as a `use` reads it
/// ([`Counter::follow_reexports`]): `use ex;` in `w` names `w::ex`, not
/// every `ex`.
fn names_from(module: &[String], segs: &[String], full: &[String]) -> bool {
    match segs.first().map(String::as_str) {
        Some("crate" | "self" | "super") => names(module, segs, full),
        _ => {
            [module, segs].concat() == full
                || module
                    .first()
                    .is_some_and(|root| [std::slice::from_ref(root), segs].concat() == full)
        }
    }
}

/// A source line: the file a token was parsed from, and its line there.
type Place = (String, usize);

/// Adds to `seen` the line `s` starts on, in the file it was parsed from.
/// Each file the counter parses is its own source to proc-macro2, named
/// apart (`Span::file`), so lines of the same number in the invocation's
/// file and a definition's are two lines, and one line is one line.
fn place(s: proc_macro2::Span, seen: &mut std::collections::BTreeSet<Place>) {
    seen.insert((s.file(), s.start().line));
}

/// Adds to `seen` the line of each token of `ts`, and of each group's
/// closing delimiter ([`place`]).
fn token_lines(ts: TokenStream, seen: &mut std::collections::BTreeSet<Place>) {
    for t in ts {
        // A fragment forwarded opaque is an invisible group, with no
        // delimiters of its own to place ([`expand`]).
        if let TokenTree::Group(g) = &t
            && g.delimiter() == proc_macro2::Delimiter::None
        {
            token_lines(g.stream(), seen);
            continue;
        }
        place(t.span(), seen);
        if let TokenTree::Group(g) = t {
            place(g.span_close(), seen);
            token_lines(g.stream(), seen);
        }
    }
}

/// The lines of a function a macro wrote: each line one of its tokens
/// (signature, body and closing brace `close`) came from, in the
/// invocation or in a definition, so that a line added to either adds to
/// it.
fn expansion_lines(sig: &Signature, body: Option<&TokenStream>, close: proc_macro2::Span) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    token_lines(
        verus_syn::__private::ToTokens::to_token_stream(sig),
        &mut seen,
    );
    if let Some(b) = body {
        token_lines(b.clone(), &mut seen);
    }
    place(close, &mut seen);
    seen.len()
}

/// Where [`macro_defs`] is reading: the file, the module it is in, where
/// that module is built ([`Built::Unsure`] under a `cfg` the counter
/// cannot settle), and the module that ends a definition's textual scope
/// there.
struct DefsAt<'a> {
    file: &'a Path,
    module: Vec<String>,
    built: Built,
    scope: Vec<String>,
}

/// Whether `attrs` has `#[name]` itself, not through a `cfg_attr`: the
/// expander's markers (`#[macro_use] mod`, `#[macro_export]`), where one a
/// `cfg_attr` applies only in some build would carry definitions into
/// scope in the others ([`has_applied`] reads those, for the rules,
/// where reading more fails closed).
fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// The paths of more than one segment that name Verus's `verus!`: its
/// crate's, and `vstd::prelude`'s re-export of it.
const VERUS_PATHS: [&[&str]; 2] = [
    &["vstd", "prelude", "verus"],
    &["verus_builtin_macros", "verus"],
];

/// The modules of [`VERUS_PATHS`], whose `*` imports Verus's `verus!`.
const VERUS_GLOBS: [&[&str]; 2] = [&["vstd", "prelude"], &["verus_builtin_macros"]];

/// The standard library's item macros that write nothing that ships,
/// each with the paths of more than one segment that name it: only
/// `compile_error!`, which stops the build, and is in the prelude. The
/// rest of std's and core's macros are left out: `global_asm!` can write
/// executable code (a symbol Rust calls through an `extern` block) that
/// the counter cannot tell from a directive, `thread_local!` writes
/// statics (read as such), `include!` writes whatever its file holds, and
/// the others (`asm!`, `concat!`, `env!`, ...) are expressions, which
/// rustc refuses in item position. An item macro not named here counts
/// as a function.
const STD_NO_FN: [(&str, &[&[&str]]); 1] = [(
    "compile_error",
    &[&["core", "compile_error"], &["std", "compile_error"]],
)];

/// The names whose items the rules read ([`Took`]): the items of these
/// names are gathered as bindings.
const SHADOW_NAMES: [&str; 6] = [
    "vstd",
    "verus_builtin_macros",
    "verus",
    "compile_error",
    "core",
    "std",
];

/// The forms of a macro the walk takes as Verus's or std's, each by a
/// rule that holds by rustc's own semantics rather than by resolving
/// names ([`Counter::doubt`]); the bindings a macro expansion writes are
/// held to the same rule once the crate is walked.
#[derive(Clone, Copy, PartialEq)]
enum Took {
    /// `::vstd::prelude::verus!` or `::verus_builtin_macros::verus!`: in
    /// edition 2018 and later a leading `::` names an extern crate, so
    /// only an `extern crate .. as` of the name or a dependency renamed to
    /// it can make it another.
    VerusPath,
    /// A bare `verus!`: no binding of `vstd`, `verus_builtin_macros` or
    /// `verus` but Verus's own, no `#[macro_use] extern crate` but
    /// Verus's or std's, and no `*` in its module but Verus's unless the
    /// module imports Verus's too (`use vstd::prelude::*;`).
    VerusBare,
    /// `::core::compile_error!` or `::std::compile_error!`, by the rule
    /// for a leading `::`.
    StdPath,
    /// A bare `compile_error!`: nothing in the crate binds the name (an
    /// item, `macro_rules!`, `use` or `extern crate`), no `#[macro_use]
    /// extern crate` but std's, and no `*` in its module.
    StdBare,
}

/// A name the crate binds, in `module`, at `place`: an item or module of
/// that name (`path` empty), a `use` or `extern crate` of `path` as
/// `name`, a `*` of `path` (`glob`), or a `#[macro_use] extern crate
/// name` (`macro_use`); `expanded` when a macro expansion writes it.
/// `shipped` and `verus` say whether a shipped build and Verus's may
/// compile it.
#[derive(Clone, Debug, Default)]
struct Binding {
    module: Vec<String>,
    path: Vec<String>,
    glob: bool,
    macro_use: bool,
    name: String,
    place: String,
    expanded: bool,
    shipped: bool,
    verus: bool,
    /// Whether Verus's build is sure to compile it (`verus` says only that
    /// it may); a binding written by an expansion is never taken as sure.
    verus_sure: bool,
    /// An `extern crate path as name;`.
    extern_crate: bool,
    /// A `use ::path..;`, whose path names an extern crate.
    absolute: bool,
}

impl Binding {
    /// The binding, as a message names it.
    fn describe(&self) -> String {
        let what = if self.macro_use {
            format!("`#[macro_use] extern crate {};`", self.name)
        } else if self.glob {
            let root = if self.absolute { "::" } else { "" };
            format!("`use {root}{}::*;`", self.path.join("::"))
        } else if self.path.is_empty() {
            format!("an item or module named `{}`", self.name)
        } else if self.extern_crate {
            format!("`extern crate {} as {};`", self.path.join("::"), self.name)
        } else {
            format!("`use {} as {};`", self.path.join("::"), self.name)
        };
        let by = if self.expanded {
            ", which a macro expansion writes"
        } else {
            ""
        };
        format!("{what} ({}{by})", self.place)
    }
}

/// Whether binding `b` breaks the rule of `kind` ([`Took`]) for `name`
/// (the leading `::` segment, or the bare macro's name) invoked in module
/// `at`. For a leading `::`: an `extern crate` that puts another crate in
/// the extern prelude under `name`. For a bare `verus!`: a binding of
/// `vstd`, `verus_builtin_macros` or `verus` but the crate itself or the
/// macro's own path (`use vstd::prelude::verus;`), or a `#[macro_use]` of
/// another crate. For a bare `compile_error!`: any binding of the name,
/// a `#[macro_use]` of another crate, or a `*` in module `at`.
fn breaks(b: &Binding, kind: Took, name: &str, at: &[String]) -> bool {
    match kind {
        Took::VerusPath | Took::StdPath => b.extern_crate && b.name == name && b.path != [name],
        Took::VerusBare => {
            if b.macro_use {
                return !matches!(
                    b.name.as_str(),
                    "vstd" | "verus_builtin_macros" | "std" | "core" | "alloc"
                );
            }
            !b.glob
                && matches!(b.name.as_str(), "vstd" | "verus_builtin_macros" | "verus")
                && b.path != [b.name.as_str()]
                && !one_of(&b.path, &VERUS_PATHS)
        }
        Took::StdBare => {
            if b.macro_use {
                return !matches!(b.name.as_str(), "std" | "core" | "alloc");
            }
            match b.glob {
                true => b.module == at,
                false => b.name == name,
            }
        }
    }
}

/// Where [`bindings_in`] reads: the file, the module, and the `cfg`s of
/// the file and the items around, of which the first `shared` are the
/// module's own (the file's and the modules' around), which every item
/// in the module is under ([`every_run_sure`]).
struct BindAt<'a> {
    file: &'a Path,
    module: Vec<String>,
    guards: Vec<String>,
    shared: usize,
}

/// Adds to `out` the bindings among `items` (in inline modules and
/// `verus!` blocks too) that the rules read ([`Took`]): every `use`,
/// `extern crate` and `*`, and every item or module named one of
/// [`SHADOW_NAMES`] (an enum's variants among them), each with whether a
/// shipped build and Verus's may compile it, under its `cfg`s and those
/// around it.
fn bindings_in(items: &[Item], at: &BindAt, atoms: &Atoms, out: &mut Vec<Binding>) {
    for item in items {
        let attrs: &[Attribute] = match item {
            Item::Use(u) => &u.attrs,
            Item::ExternCrate(e) => &e.attrs,
            Item::Mod(m) => &m.attrs,
            Item::Macro(m) => &m.attrs,
            Item::Fn(f) => &f.attrs,
            Item::Struct(s) => &s.attrs,
            Item::Enum(e) => &e.attrs,
            Item::Union(u) => &u.attrs,
            Item::Type(t) => &t.attrs,
            Item::Const(c) => &c.attrs,
            Item::Static(s) => &s.attrs,
            Item::Trait(t) => &t.attrs,
            Item::TraitAlias(t) => &t.attrs,
            _ => &[],
        };
        let mut guards = at.guards.clone();
        guards.extend(cfg_predicates(attrs));
        let sees = |verus: bool| {
            guards
                .iter()
                .all(|p| cfg_eval(p, verus, atoms).map_or(true, |c| c.t))
        };
        let (shipped, verus) = (sees(false), sees(true));
        let base = Binding {
            module: at.module.clone(),
            shipped,
            verus,
            verus_sure: every_run_sure(&guards[..at.shared], &guards[at.shared..], atoms)
                .unwrap_or(false),
            ..Binding::default()
        };
        let line = |s: proc_macro2::Span| format!("{}:{}", at.file.display(), s.start().line);
        match item {
            Item::Use(u) => {
                let mut targets = Vec::new();
                use_paths(&u.tree, &mut Vec::new(), &mut targets);
                out.extend(targets.into_iter().map(|(path, glob, name)| Binding {
                    path,
                    glob,
                    name,
                    absolute: u.leading_colon.is_some(),
                    place: line(u.use_token.span),
                    ..base.clone()
                }));
            }
            Item::ExternCrate(e) => out.push(Binding {
                path: vec![e.ident.to_string()],
                name: e.rename.as_ref().map_or(&e.ident, |(_, r)| r).to_string(),
                place: line(e.extern_token.span),
                extern_crate: true,
                ..base.clone()
            }),
            _ => {}
        }
        let mut idents: Vec<&proc_macro2::Ident> = Vec::new();
        match item {
            Item::Mod(m) => idents.push(&m.ident),
            Item::Fn(f) => idents.push(&f.sig.ident),
            Item::Struct(s) => idents.push(&s.ident),
            Item::Enum(e) => {
                idents.push(&e.ident);
                idents.extend(e.variants.iter().map(|v| &v.ident));
            }
            Item::Union(u) => idents.push(&u.ident),
            Item::Type(t) => idents.push(&t.ident),
            Item::Const(c) => idents.push(&c.ident),
            Item::Static(s) => idents.push(&s.ident),
            Item::Trait(t) => idents.push(&t.ident),
            Item::TraitAlias(t) => idents.push(&t.ident),
            _ => {}
        }
        for i in idents
            .into_iter()
            .filter(|i| SHADOW_NAMES.iter().any(|n| *i == n))
        {
            out.push(Binding {
                name: i.to_string(),
                place: line(i.span()),
                ..base.clone()
            });
        }
        let inner = |module: Vec<String>, shared: usize| BindAt {
            file: at.file,
            module,
            guards: guards.clone(),
            shared,
        };
        match item {
            // An inline module's `cfg`s are its own items' shared ones; a
            // `verus!` block's cover only what it holds.
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    let module = [&at.module[..], &[m.ident.to_string()]].concat();
                    bindings_in(items, &inner(module, guards.len()), atoms, out);
                }
            }
            Item::Macro(m) if verus_path(&m.mac.path) => {
                if let Ok(f) = parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                    bindings_in(&f.items, &inner(at.module.clone(), at.shared), atoms, out);
                }
            }
            _ => {}
        }
    }
}

/// Whether `tokens` hold a `fn` at any depth: a `verus!` block that may
/// write a function.
fn holds_fn(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|t| match t {
        TokenTree::Ident(i) => i == "fn",
        TokenTree::Group(g) => holds_fn(g.stream()),
        _ => false,
    })
}

/// The segments of `path`, as written.
fn segments(path: &verus_syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

/// Whether `segs` is one of `paths`.
fn one_of(segs: &[String], paths: &[&[&str]]) -> bool {
    paths
        .iter()
        .any(|p| segs.iter().map(String::as_str).eq(p.iter().copied()))
}

/// Adds to `out` the crate of each `#[macro_use] extern crate` among
/// `items` (`macro_use` applied directly or through a `cfg_attr`), in
/// `verus!` blocks too, but those built only for tests.
fn macro_use_crates(items: &[Item], atoms: &Atoms, out: &mut Vec<String>) {
    for item in items {
        match item {
            Item::ExternCrate(e)
                if has_applied(&e.attrs, "macro_use")
                    && attrs_built(&e.attrs, atoms) != Ok(Built::Test) =>
            {
                out.push(e.ident.to_string());
            }
            Item::Macro(m) if verus_path(&m.mac.path) => {
                if let Ok(f) = parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                    macro_use_crates(&f.items, atoms, out);
                }
            }
            _ => {}
        }
    }
}

/// Whether an item macro invoked by `path` is Verus's `verus!` by the
/// path's shape: a bare `verus`, or one of [`VERUS_PATHS`] with or
/// without a leading `::`. The walk takes a block that writes a function
/// only bare or with a leading `::`, by the rule of that form ([`Took`]),
/// and fails the run on any other.
pub(crate) fn verus_path(path: &verus_syn::Path) -> bool {
    let segs = segments(path);
    (path.leading_colon.is_none() && segs == ["verus"]) || one_of(&segs, &VERUS_PATHS)
}

/// The modules among `items` (in `module`, and in its inline modules)
/// that `#[macro_use] mod name;` declares in a file of their own, into
/// `carried`: the textual scope of a definition at such a file's top
/// carries on into the module that declares it. Every module `items`
/// declares goes into `all`, one declared inside a `verus!` block too.
fn macro_use_mods(
    items: &[Item],
    module: &[String],
    (carried, all): (
        &mut std::collections::BTreeSet<Vec<String>>,
        &mut std::collections::BTreeSet<Vec<String>>,
    ),
) {
    for item in items {
        if let Item::Macro(v) = item
            && verus_path(&v.mac.path)
            && let Ok(f) = parse2::<verus_syn::File>(v.mac.tokens.clone())
        {
            macro_use_mods(&f.items, module, (carried, all));
            continue;
        }
        let Item::Mod(m) = item else { continue };
        let mut inner = module.to_vec();
        inner.push(m.ident.to_string());
        all.insert(inner.clone());
        match &m.content {
            None if has_attr(&m.attrs, "macro_use") => {
                carried.insert(inner);
            }
            Some((_, items)) => macro_use_mods(items, &inner, (carried, all)),
            None => {}
        }
    }
}

/// The paths `tree`, a `use`, names, under `prefix`.
fn use_paths(tree: &verus_syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<UseTarget>) {
    match tree {
        verus_syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            use_paths(&p.tree, prefix, out);
            prefix.pop();
        }
        verus_syn::UseTree::Name(n) => {
            let mut p = prefix.clone();
            p.push(n.ident.to_string());
            out.push((p, false, n.ident.to_string()));
        }
        verus_syn::UseTree::Glob(_) => out.push((prefix.clone(), true, String::new())),
        verus_syn::UseTree::Group(g) => {
            for t in &g.items {
                use_paths(t, prefix, out);
            }
        }
        // A rename keeps the path it imports, under its new name.
        verus_syn::UseTree::Rename(r) => {
            let mut p = prefix.clone();
            p.push(r.ident.to_string());
            out.push((p, false, r.rename.to_string()));
        }
    }
}

/// The definition `m`, a `macro_rules!` item, at `at`.
fn macro_def(m: &verus_syn::ItemMacro, at: &DefsAt, atoms: &Atoms, emitted: bool) -> MacroDef {
    let built = narrower(
        at.built,
        attrs_built(&m.attrs, atoms).unwrap_or(Built::Shipped),
    );
    MacroDef {
        file: at.file.to_path_buf(),
        line: m.mac.bang_token.span.start().line,
        emitted,
        module: at.module.clone(),
        paths: if has_attr(&m.attrs, "macro_export") {
            vec![vec![
                at.module.first().cloned().unwrap_or_default(),
                m.ident
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ]]
        } else {
            Vec::new()
        },
        built,
        unsure: built == Built::Unsure,
        scope: at.scope.clone(),
        exported: has_attr(&m.attrs, "macro_export"),
        rules: expand::Rules::parse(m.mac.tokens.clone()),
    }
}

/// The crate's `macro_rules!` definitions, by name.
type MacroDefs = BTreeMap<String, Vec<MacroDef>>;

/// Each module's `use` paths.
type Uses = BTreeMap<Vec<String>, Vec<UsePath>>;

/// Keeps each `macro_rules!` definition in `items` (and in the inline
/// modules and `verus!` blocks among them) under its name, for
/// [`Counter::item_macro`], with where it is built (under `#[cfg(test)]`,
/// in a test-only module or file, it is built for tests alone) and how far
/// its scope reaches, and each module's `use` paths. `at` is where the
/// items are.
fn macro_defs(
    items: &[Item],
    at: &DefsAt,
    atoms: &Atoms,
    (out, uses): (&mut MacroDefs, &mut Uses),
) {
    let built = |attrs: &[Attribute]| {
        narrower(
            at.built,
            attrs_built(attrs, atoms).unwrap_or(Built::Shipped),
        )
    };
    for item in items {
        match item {
            Item::Macro(m) if m.mac.path.is_ident("macro_rules") => {
                if let Some(name) = &m.ident {
                    out.entry(name.to_string())
                        .or_default()
                        .push(macro_def(m, at, atoms, false));
                }
            }
            Item::Use(u) => {
                let mut targets = Vec::new();
                use_paths(&u.tree, &mut Vec::new(), &mut targets);
                let b = built(&u.attrs);
                uses.entry(at.module.clone()).or_default().extend(
                    targets
                        .into_iter()
                        .map(|(p, glob, named)| (p, glob, named, b)),
                );
            }
            Item::Macro(m) if verus_path(&m.mac.path) => {
                if let Ok(f) = parse2::<verus_syn::File>(m.mac.tokens.clone()) {
                    let inner_at = DefsAt {
                        file: at.file,
                        module: at.module.clone(),
                        built: built(&m.attrs),
                        scope: at.scope.clone(),
                    };
                    macro_defs(&f.items, &inner_at, atoms, (out, uses));
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut module = at.module.clone();
                    module.push(m.ident.to_string());
                    let scope = if has_attr(&m.attrs, "macro_use") {
                        at.scope.clone()
                    } else {
                        module.clone()
                    };
                    let inner_at = DefsAt {
                        file: at.file,
                        module,
                        built: built(&m.attrs),
                        scope,
                    };
                    macro_defs(inner, &inner_at, atoms, (out, uses));
                }
            }
            _ => {}
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

/// Whether a crate ships: every crate but one whose Cargo.toml says
/// `publish = false` (pcre3-testdata, the conformance harness's reader).
fn ships(krate: &Path) -> bool {
    fs::read_to_string(krate.join("Cargo.toml")).map_or(true, |t| !unpublished(&t))
}

/// Whether the Cargo.toml `text` sets its package's `publish` to `false`,
/// read as TOML: a `publish = false` in another table
/// (`[package.metadata]`) is not the package's. A manifest that does not
/// parse, or a `publish` of any other value, publishes: the crate ships.
fn unpublished(text: &str) -> bool {
    toml::from_str::<toml::Table>(text)
        .ok()
        .and_then(|t| t.get("package")?.get("publish")?.as_bool())
        == Some(false)
}

/// A crate the counter reads: its name, its directory (holding `src/`),
/// whether it ships ([`ships`], or the configuration's `ships`), and
/// whether its Cargo.toml decides that (`by_rule`: a `dir` crate, or a
/// member without `ships`).
struct Krate {
    name: String,
    dir: PathBuf,
    ships: bool,
    by_rule: bool,
}

/// The crates `c` names under `root`, in reading order: each
/// subdirectory of `[crates] dir` with a `src/`, in path order, named for
/// the subdirectory, then each of `[crates] members`.
fn crate_dirs(root: &Path, c: &Config) -> Result<Vec<Krate>, String> {
    // `--base-rev` compares crates by their configured paths
    // (`Config::narrowed_from`), so two spellings of one directory, through
    // `..`, a symlink or a path from `/`, would let a crate the base read
    // pass as a new one.
    for p in c
        .crates
        .dir
        .iter()
        .chain(c.crates.members.iter().map(|m| &m.path))
    {
        let path = Path::new(p);
        if path.has_root() {
            return Err(format!(
                "[crates] path {p} is not relative: give each crate's directory as one path under {}",
                root.display()
            ));
        }
        if path
            .components()
            .any(|k| k == std::path::Component::ParentDir)
        {
            return Err(format!(
                "[crates] path {p} has a `..`: give each crate's directory as one path under {}",
                root.display()
            ));
        }
        if let Some(link) = symlink_in(root, path) {
            return Err(format!(
                "[crates] path {p} passes through {}, a symlink: give each crate's directory as one path under {}",
                link.display(),
                root.display()
            ));
        }
    }
    let mut out = Vec::new();
    if let Some(d) = &c.crates.dir {
        let Ok(rd) = fs::read_dir(root.join(d)) else {
            return Err(format!("no {d}/ under {}", root.display()));
        };
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("src").is_dir())
            .collect();
        dirs.sort();
        out.extend(dirs.into_iter().map(|p| {
            Krate {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                ships: ships(&p),
                by_rule: true,
                dir: p,
            }
        }));
    }
    for m in &c.crates.members {
        let p = root.join(&m.path);
        if !p.join("src").is_dir() {
            return Err(format!("crate {}: no src/ under {}", m.name, p.display()));
        }
        out.push(Krate {
            name: m.name.clone(),
            ships: m.ships.unwrap_or_else(|| ships(&p)),
            by_rule: m.ships.is_none(),
            dir: p,
        });
    }
    // A crate of `dir` that is a link, or sources reached through one,
    // would read another crate's code under a new name, as a configured
    // path through a symlink would.
    for k in &out {
        let rel = k.dir.strip_prefix(root).unwrap_or(&k.dir);
        if let Some(link) = symlink_in(root, rel).or_else(|| symlink_below(root, &rel.join("src")))
        {
            return Err(format!(
                "[crates] crate {} reads {}, a symlink: give each crate's directory and sources as files under {}",
                k.name,
                link.display(),
                root.display()
            ));
        }
    }
    // The tally and the shipped row go by crate name, so a name read
    // twice would let one `ships = false` take both out, and a directory
    // read twice would count its crate twice.
    for (i, k) in out.iter().enumerate() {
        let dir = fs::canonicalize(&k.dir).unwrap_or_else(|_| k.dir.clone());
        if let Some(j) = out[..i].iter().find(|j| {
            j.name == k.name || fs::canonicalize(&j.dir).unwrap_or_else(|_| j.dir.clone()) == dir
        }) {
            return Err(format!(
                "[crates] reads crate {} ({}) and crate {} ({}): give each crate one name and one directory",
                j.name,
                j.dir.display(),
                k.name,
                k.dir.display()
            ));
        }
    }
    Ok(out)
}

/// The seams tables `c` names under `root`: its `tables`, then each `*.md`
/// of its `theorems` directory, in path order; and how the report names
/// them (`verus/SEAMS.md, verus/theorems/*.md`).
fn seam_tables(root: &Path, c: &config::Seams) -> (Vec<String>, String) {
    let mut texts: Vec<String> = c
        .tables
        .iter()
        .map(|t| fs::read_to_string(root.join(t)).unwrap_or_default())
        .collect();
    let mut names = c.tables.clone();
    if let Some(dir) = &c.theorems {
        let mut theorems: Vec<PathBuf> = fs::read_dir(root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        theorems.sort();
        texts.extend(theorems.iter().filter_map(|p| fs::read_to_string(p).ok()));
        names.push(format!("{dir}/*.md"));
    }
    (texts, names.join(", "))
}

/// Exits 2 with `e` printed.
fn usage(e: impl std::fmt::Display) -> ! {
    eprintln!("{e}");
    std::process::exit(2);
}

/// The command line, as `parse_args` reads it.
#[derive(Debug, Default, PartialEq)]
struct Args {
    list: bool,
    version: bool,
    badge: bool,
    floor: Option<PathBuf>,
    base_rev: Option<String>,
    no_base: bool,
    stats: Option<PathBuf>,
    config_file: Option<PathBuf>,
    root: Option<PathBuf>,
}

/// Reads the arguments after the program name. An argument that starts
/// with `-` and is not an option fails, as does a second ROOT: a
/// misspelt `--flor FILE` would otherwise be read as ROOT, and the run
/// would pass with no floor checked. `--badge` with a checking option
/// fails too, since it prints the badge line and checks nothing.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => out.list = true,
            "--version" => out.version = true,
            "--no-base" => out.no_base = true,
            "--badge" => out.badge = true,
            "--floor" | "--base-rev" | "--rlimit" | "--config" => {
                let v = args.next().ok_or_else(|| format!("{a} needs a value"))?;
                match a.as_str() {
                    "--floor" => out.floor = Some(PathBuf::from(v)),
                    "--rlimit" => out.stats = Some(PathBuf::from(v)),
                    "--config" => out.config_file = Some(PathBuf::from(v)),
                    _ => out.base_rev = Some(v),
                }
            }
            _ if a.starts_with('-') => return Err(format!("unknown option {a}")),
            _ if out.root.is_some() => {
                return Err(format!("a second ROOT {a}: give one repository root"));
            }
            _ => out.root = Some(PathBuf::from(a)),
        }
    }
    if out.no_base && out.base_rev.is_some() {
        return Err("--no-base with --base-rev: give one".to_owned());
    }
    if out.badge
        && (out.list
            || out.floor.is_some()
            || out.base_rev.is_some()
            || out.no_base
            || out.stats.is_some())
    {
        return Err(
            "--badge prints the badge line and checks nothing: give it without --list, --floor, --base-rev, --no-base or --rlimit".into(),
        );
    }
    Ok(out)
}

fn main() {
    let Args {
        list,
        version,
        badge,
        floor,
        base_rev,
        no_base,
        stats,
        config_file,
        root,
    } = parse_args(std::env::args().skip(1)).unwrap_or_else(|e| usage(e));
    if version {
        println!("verus-count {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // A FLOOR held with no base would pass a lowered figure, a dropped
    // gate or a loosened RLIMIT, so leaving the base off takes a flag.
    if floor.is_some() && base_rev.is_none() && !no_base {
        eprintln!(
            "FAIL coverage floor: --floor with no --base-rev, so a lowered figure or a dropped gate would pass: give --base-rev REV, or --no-base for a local run or the change that adds the first FLOOR"
        );
        std::process::exit(1);
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let config_file = config_file.unwrap_or_else(|| root.join("verus/count.toml"));
    let cfg = Config::read(&config_file).unwrap_or_else(|e| usage(e));
    if badge {
        let path = root.join(&cfg.floor.path);
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| usage(format!("{}: {e}", path.display())));
        let head = Figures::new(&cfg.floor.checked, &cfg.floor.path)
            .read(&text, &path.display().to_string());
        println!("{}", badge_line(&head.floor));
        return;
    }
    let base = base_rev.map(|rev| Base {
        cfg: check_config(&root, &config_file, &cfg, &rev),
        rev,
    });
    let crates = crate_dirs(&root, &cfg).unwrap_or_else(|e| usage(e));
    let mut unshipped = Vec::new();
    let mut c = Counter {
        verus_only_ghost: cfg.crates.verus_only_ghost,
        ..Counter::new(
            list,
            Atoms::of(&cfg),
            cfg.reach.as_ref().and_then(|r| r.fn_ptr_macro.clone()),
        )
    };
    // Test-only features' items are left out as test-only (cfg_needs_test):
    // no build of the workspace may enable one, as cargo resolves the
    // features.
    if !cfg.crates.test_features.is_empty() {
        let enablers = match test_util_enablers(&root, &cfg.crates.test_features) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("FAIL test-util: {e}");
                std::process::exit(2);
            }
        };
        for (p, f) in &enablers {
            eprintln!(
                "FAIL test-util: a build of the workspace compiles {p} with its `{f}` feature, which only tests' dev-dependencies may enable, so a shipped build could compile the items verus-count leaves out as test-only"
            );
        }
        if !enablers.is_empty() {
            std::process::exit(1);
        }
    }
    // The `#[test]` fns a ledger status's `callers:` clause may name.
    let mut tests = std::collections::BTreeSet::new();
    for Krate {
        name,
        dir: k,
        ships,
        ..
    } in &crates
    {
        c.krate = name.clone();
        c.tallies.entry(c.krate.clone()).or_default();
        if !ships {
            unshipped.push(c.krate.clone());
        }
        c.walk_crate(&cfg, (&root, k), cfg.ledger.is_some().then_some(&mut tests));
    }
    c.finish();
    if cfg.ledger.is_some() && !c.discharged.is_empty() {
        for (file, name) in &c.discharged {
            eprintln!(
                "FAIL discharged: {}: `{name}` has the discharged_by = \"kani\" marker, but with [ledger] configured the ledger, not the marker, says what is discharged",
                file.display()
            );
        }
        std::process::exit(1);
    }
    let records = cfg.kani.records.as_deref();
    let missing = unrecorded(
        &c.discharged,
        &records.map_or_else(Vec::new, |d| kani_records(&root.join(d))),
    );
    for (file, name) in &missing {
        eprintln!(
            "FAIL discharged: {}: `{name}` has the discharged_by = \"kani\" marker, but no {}/kani-*.md record names its file and quotes `fn {name}`",
            file.display(),
            records.unwrap_or("[kani] records")
        );
    }
    if !missing.is_empty() {
        std::process::exit(1);
    }
    if let Some(r) = &cfg.recursion {
        check_recursion(&root, &r.table, &c.fns, &unshipped);
    }
    let (tables, tables_named) = seam_tables(&root, &cfg.seams);
    let seams = reach::seam_ids(tables.iter().map(String::as_str));
    if let Some(i) = &cfg.inputs
        && !compile_inputs(&root, i, &c)
    {
        std::process::exit(1);
    }
    if let Some(r) = &cfg.reach
        && !entry_reach(
            &root,
            r,
            &cfg.crates.stand_ins,
            &c,
            &unshipped,
            (&seams, &tables_named),
            base.as_ref(),
        )
    {
        std::process::exit(1);
    }
    if let Some(m) = &cfg.mints {
        let names = mints::Names {
            mint: m.mint.clone(),
            token: m.token_trait.clone(),
            home: m.home.clone(),
            list: m.path.clone(),
            tables: tables_named.clone(),
            stand_ins: cfg.crates.stand_ins.clone(),
        };
        if !token_mints(&root, names, &crates, &seams) {
            std::process::exit(1);
        }
    }
    let shipped = report(&root, &cfg, &c, &unshipped, &tests);
    let mut failed = false;
    if let Some(f) = floor {
        let fig = Figures::new(&cfg.floor.checked, &cfg.floor.path);
        let head = fig.read(
            &fs::read_to_string(&f).unwrap_or_else(|e| usage(format!("{}: {e}", f.display()))),
            &f.display().to_string(),
        );
        // The base's FLOOR is read at the base configuration's `[floor]
        // path`, so that path must be the file a head run reads: else one
        // change could point it at a file no run holds, and every later
        // run would compare with that file, or with none. Nor may the path
        // pass through a symlink, as the configuration may not
        // (`check_config`): once a link merged, the base's copy would be
        // the link's text.
        if let Some(link) = symlink_in(&root, Path::new(&cfg.floor.path)) {
            usage(format!(
                "FAIL coverage floor: [floor] path {} passes through {}, a symlink, so a later base's copy would be the link's text: give `path` the file itself",
                cfg.floor.path,
                link.display()
            ));
        }
        match (
            fs::canonicalize(&f),
            fs::canonicalize(root.join(&cfg.floor.path)),
        ) {
            (Ok(a), Ok(b)) if a == b => {}
            _ => usage(format!(
                "FAIL coverage floor: --floor {} is not {}'s [floor] path {}: give `path` the file the run holds",
                f.display(),
                root.display(),
                cfg.floor.path
            )),
        }
        if let Some(name) = fig.missing(&head.floor) {
            usage(format!("{}: want a {name} figure", f.display()));
        }
        let (mut ok, report) = fig.check(&shipped, &head.floor);
        print!("{report}");
        if let Some(b) = &cfg.badge {
            ok &= check_badge(&root, &b.path, &cfg.floor.path, &head.floor);
        }
        if let Some(b) = &base {
            let rev = &b.rev;
            match b.copy(&root, &cfg.floor.path, GateFile::Floor) {
                Ok((path, Some(text))) => {
                    let base = fig
                        .for_base(cfg.floor.renamed_from.as_deref())
                        .read(&text, &format!("{rev}:{path}"));
                    let (lowering_ok, report) = fig.lowering(&head, &base);
                    print!("{report}");
                    ok &= lowering_ok;
                }
                Ok((path, None)) => {
                    println!("ok coverage floor: {rev} has no {path} to compare with")
                }
                Err(e) => {
                    eprintln!("FAIL coverage floor: {e}");
                    std::process::exit(2);
                }
            }
        }
        failed |= !ok;
    }
    if let Some(s) = stats {
        // The crates with code Verus checks, as Verus spells their names:
        // each with a function the walk recorded for Verus
        // ([`Counter::verus_fns`]), its proofs built only for Verus
        // (`verus_keep_ghost`) too, which no coverage tally counts, and
        // each whose tally counts a function inside `verus!`.
        let verus_crates: Vec<String> = c
            .verus_fns
            .keys()
            .filter_map(|n| n.split("::").next().map(str::to_owned))
            .chain(
                c.tallies
                    .iter()
                    .filter(|(_, t)| {
                        t.ghost + t.fns.iter().sum::<usize>() > t.fns[Class::Outside as usize]
                    })
                    .map(|(k, _)| k.replace('-', "_")),
            )
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect();
        failed |= !check_rlimit(&root, &cfg, &crates, &s, &verus_crates, &c, base.as_ref());
    }
    if failed {
        std::process::exit(1);
    }
}

/// Holds the configuration to `rev`'s copy ([`Config::explain`]): exits 1
/// when it removes or narrows a gate the base runs with no new
/// `[[narrowed]]` line naming that change, or has a new line that names
/// no narrowing, and 2 when the base's copy does not parse. A crate whose
/// Cargo.toml gained `publish = false` ([`newly_unpublished`]) is such a
/// narrowing too. Each named
/// narrowing is printed with its reason, so review sees it. The base's copy is read at the
/// configuration's path under `root`, as the repository names it, or at
/// `verus/count.toml` when the configuration lies outside `root`. A
/// configuration under `root` reached through a symlink exits 2: the
/// base would be read at the link's path while the head is read through
/// it, and once a link merged, the base's copy would be the link's text.
/// A base with no copy, the change that adopts the counter, has nothing
/// to compare. The base's copy, when there is one, for [`Base::copy`].
fn check_config(root: &Path, file: &Path, cfg: &Config, rev: &str) -> Option<Config> {
    let under = std::path::absolute(file)
        .ok()
        .zip(std::path::absolute(root).ok())
        .and_then(|(f, r)| f.strip_prefix(r).ok().map(Path::to_path_buf));
    if let Some(link) = under.as_deref().and_then(|u| symlink_in(root, u)) {
        eprintln!(
            "FAIL configuration: {} is a symlink, so {rev}'s copy cannot be compared with the head's: give the configuration as a file",
            link.display()
        );
        std::process::exit(2);
    }
    let rel = under.unwrap_or_else(|| PathBuf::from("verus/count.toml"));
    let rel = rel.to_string_lossy();
    let text = match base_file(root, rev, &rel) {
        Ok(Some(text)) => text,
        Ok(None) => {
            println!("ok configuration: {rev} has no {rel} to compare with");
            return None;
        }
        Err(e) => {
            eprintln!("FAIL configuration: {e}");
            std::process::exit(2);
        }
    };
    let base = Config::parse(&text).unwrap_or_else(|e| {
        eprintln!("FAIL configuration: {rev}:{rel}: {e}");
        std::process::exit(2);
    });
    let crate_changes = base_dir_narrowed(root, cfg, &base, rev)
        .and_then(|mut v| {
            v.extend(newly_unpublished(root, cfg, &base, rev)?);
            Ok(v)
        })
        .unwrap_or_else(|e| {
            eprintln!("FAIL configuration: {e}");
            std::process::exit(2);
        });
    let explained = cfg.explain(&base, crate_changes);
    for (n, reason) in &explained.named {
        println!("ok configuration: {rel}: {n}, with a reason: {reason}");
    }
    for n in &explained.unnamed {
        eprintln!(
            "FAIL configuration: {rel}: {n}, where {rev}:{rel} runs it: restore it, or give it a new [[narrowed]] line with change = {:?} and a reason",
            n.change
        );
    }
    for change in &explained.stale {
        eprintln!(
            "FAIL configuration: {rel}: the [[narrowed]] line for {change:?} names no change from {rev}:{rel}: remove it, or name the change as this run prints it"
        );
    }
    if !explained.unnamed.is_empty() || !explained.stale.is_empty() {
        std::process::exit(1);
    }
    if explained.named.is_empty() {
        println!("ok configuration: {rel} keeps every gate {rev}:{rel} runs");
    } else {
        println!("ok configuration: {rel} keeps every other gate {rev}:{rel} runs");
    }
    Some(base)
}

/// The crates that leave the shipped tally because their own Cargo.toml
/// gained `publish = false` since `rev`: each crate the head reads whose
/// Cargo.toml decides that it ships and says it does not, where `base`
/// read the same crate by that rule too, at the same directory or under
/// the same name (a member moved, or a crate of `dir` made a member), and
/// `rev`'s Cargo.toml there does not say so. The configuration compares
/// the rest: a crate the head no longer reads, or whose `ships` it sets,
/// is a narrowing of [`Config::narrowed_from`]'s or [`base_dir_narrowed`]'s.
/// A crate `rev` lacks under its name and at its directory is new, with
/// nothing to compare. Each is a narrowing the `[[narrowed]]` lines name.
fn newly_unpublished(
    root: &Path,
    cfg: &Config,
    base: &Config,
    rev: &str,
) -> Result<Vec<config::Narrowing>, String> {
    // A head whose crates do not read fails the run later, with the reason.
    let Ok(crates) = crate_dirs(root, cfg) else {
        return Ok(Vec::new());
    };
    let in_base_dir = match &base.crates.dir {
        Some(d) => base_dir_crates(root, rev, d)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for k in crates.iter().filter(|k| k.by_rule && !k.ships) {
        let rel = crate_path(root, k);
        // Where the base read this crate by the rule: at its directory, or
        // under its name.
        let mut at = Vec::new();
        if base.reads_by_rule(&rel) {
            at.push(rel.clone());
        }
        match base.crates.members.iter().find(|m| m.name == k.name) {
            Some(m) if m.ships.is_none() => at.push(m.path.clone()),
            Some(_) => {}
            None => {
                if let Some(d) = &base.crates.dir
                    && in_base_dir.contains(&k.name)
                {
                    at.push(format!("{d}/{}", k.name));
                }
            }
        }
        for p in at {
            let manifest = config::normal(&p).join("Cargo.toml");
            if let Some(text) = base_file(root, rev, &manifest.to_string_lossy())?
                && !unpublished(&text)
            {
                out.push(config::Narrowing {
                    change: format!(
                        "[crates] crate {:?} at {rel:?} gains publish = false",
                        k.name
                    ),
                    effect: "so it no longer counts as shipped".into(),
                });
                break;
            }
        }
    }
    Ok(out)
}

/// The crates `rev` read through its `[crates] dir` that the head no
/// longer reads as it did: with the same `dir`, a crate the head reads
/// neither under its name nor at its directory (`is no longer read`, as a
/// member dropped is), or reads under its name somewhere else (`moves`, as
/// a member moved does); and, whatever the head's `dir`, one the head reads
/// under its name as a member with `ships` set to anything but `true`
/// where the configuration compares nothing (a member at a path `base`
/// neither names nor reads through `dir`). The configuration alone cannot
/// see these: it names `dir`, not the crates in it. So a `dir` crate
/// renamed is the old crate no longer read and a new one, as a member
/// renamed to a new path is.
fn base_dir_narrowed(
    root: &Path,
    cfg: &Config,
    base: &Config,
    rev: &str,
) -> Result<Vec<config::Narrowing>, String> {
    let Some(d) = &base.crates.dir else {
        return Ok(Vec::new());
    };
    let Ok(crates) = crate_dirs(root, cfg) else {
        return Ok(Vec::new());
    };
    let same_dir = cfg.crates.dir.as_ref().map(|h| config::normal(h)) == Some(config::normal(d));
    let mut out = Vec::new();
    for name in base_dir_crates(root, rev, d)? {
        let was = config::normal(d).join(&name);
        let was = was.to_string_lossy();
        if crates.iter().any(|k| crate_path(root, k) == was) {
            continue;
        }
        let Some(k) = crates.iter().find(|k| k.name == name) else {
            if same_dir {
                out.push(config::Narrowing {
                    change: format!("[crates] crate {name:?} at {was:?} is no longer read"),
                    effect: "so it is no longer read".into(),
                });
            }
            continue;
        };
        let now = crate_path(root, k);
        if same_dir {
            out.push(config::Narrowing {
                change: format!("[crates] crate {name:?} moves from {was:?} to {now:?}"),
                effect: "so the crate it read may no longer be read".into(),
            });
        }
        let compared = base
            .crates
            .members
            .iter()
            .any(|m| config::normal(&m.path) == config::normal(&now))
            || base.reads_by_rule(&now);
        if !k.by_rule && !k.ships && !compared {
            out.push(config::Narrowing {
                change: format!(
                    "[crates] member {name:?} at {now:?} changes ships from unset to false"
                ),
                effect: "so the crate the base read there may no longer count as shipped".into(),
            });
        }
    }
    Ok(out)
}

/// `k`'s directory relative to `root`, as the narrowings name it: `.` for
/// the root.
fn crate_path(root: &Path, k: &Krate) -> String {
    let rel = k.dir.strip_prefix(root).unwrap_or(&k.dir);
    let rel = config::normal(&rel.to_string_lossy());
    if rel.as_os_str().is_empty() {
        ".".into()
    } else {
        rel.to_string_lossy().into_owned()
    }
}

/// The first of `rel`'s paths under `root` (`verus`, then
/// `verus/count.toml` or `verus/FLOOR`) that is a symlink.
fn symlink_in(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut at = PathBuf::new();
    rel.components().find_map(|c| {
        at.push(c);
        fs::symlink_metadata(root.join(&at))
            .is_ok_and(|m| m.file_type().is_symlink())
            .then(|| at.clone())
    })
}

/// The first path at or below `rel` under `root`, in path order, that is
/// a symlink: `rel` itself, or a file or directory under it.
fn symlink_below(root: &Path, rel: &Path) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(root.join(rel)).ok()?;
    if meta.file_type().is_symlink() {
        return Some(rel.to_path_buf());
    }
    if !meta.is_dir() {
        return None;
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(root.join(rel))
        .ok()?
        .flatten()
        .map(|e| rel.join(e.file_name()))
        .collect();
    entries.sort();
    entries.iter().find_map(|e| symlink_below(root, e))
}

/// The revision `--base-rev` names, and its copy of the configuration
/// when it has one ([`check_config`]).
struct Base {
    rev: String,
    cfg: Option<Config>,
}

/// A file a gate holds to its base revision's copy.
#[derive(Clone, Copy, Debug)]
enum GateFile {
    Floor,
    Rlimit,
    Reach,
}

impl Base {
    /// `rev`'s copy of `file`, and the path it was read at: the path
    /// `rev`'s configuration gives the file (`[floor] path`, which
    /// defaults, `[rlimit] path`, likewise, or `[reach] path`). So a
    /// change that moves FLOOR, RLIMIT or REACH, or points the key at a
    /// path the base lacks, is still compared with the base's copy. With
    /// no base configuration, or no `[reach]` in it, the path is `head`,
    /// the head's. A file whose section the base's configuration has but
    /// `rev` lacks is an error; any other absence is `None`, nothing to
    /// compare.
    fn copy(
        &self,
        root: &Path,
        head: &str,
        file: GateFile,
    ) -> Result<(String, Option<String>), String> {
        let rev = &self.rev;
        let (path, declared) = self
            .cfg
            .as_ref()
            .and_then(|c| match file {
                GateFile::Floor => Some((c.floor.path.as_str(), c.floor.declared)),
                GateFile::Rlimit => Some((c.rlimit.path.as_str(), c.rlimit.declared)),
                GateFile::Reach => c.reach.as_ref().map(|r| (r.path.as_str(), true)),
            })
            .unwrap_or((head, false));
        let text = base_file(root, rev, path)?;
        if text.is_none() && declared {
            return Err(format!(
                "{rev}'s configuration names {path}, but {rev} has no {path}, so there is no base copy to compare with"
            ));
        }
        Ok((path.to_owned(), text))
    }
}

/// The recursion check (`[recursion]`, pcre3's verus/theorems/RN1.md):
/// exits 1 on a cycle through a function Verus does not check, or through
/// a verified one that `table`'s table does not name ([`documented`]).
fn check_recursion(root: &Path, table: &str, fns: &[FnRef], unshipped: &[String]) {
    let rn1 = fs::read_to_string(root.join(table)).unwrap_or_default();
    let (verified, cycles): (Vec<_>, Vec<_>) = recursive(fns, unshipped)
        .into_iter()
        .partition(|(i, _)| fns[*i].class == Class::Verified);
    let undocumented: Vec<_> = verified
        .iter()
        .filter(|(i, _)| !documented(&rn1, &fns[*i]))
        .collect();
    for (i, path) in &undocumented {
        let f = &fns[*i];
        eprintln!(
            "FAIL recursion: {}:{}: `{}` recurses ({}), but {table}'s table has no row naming its file and quoting `fn {}`",
            f.file.display(),
            f.line,
            f.name,
            path.join(" -> "),
            f.name
        );
    }
    for (i, path) in &cycles {
        let f = &fns[*i];
        eprintln!(
            "FAIL recursion: {}:{}: `{}` ({:?}) recurses: {} ({table})",
            f.file.display(),
            f.line,
            f.name,
            f.class,
            path.join(" -> ")
        );
    }
    if !cycles.is_empty() || !undocumented.is_empty() {
        std::process::exit(1);
    }
}

/// Prints the tables, fails the run on a file that did not parse, and
/// returns the `shipped` row's four figures. With the Kani marker (no
/// `[ledger]`), one table: each crate, `total` and `shipped`, the
/// discharged functions in a column of their own. With `[ledger]`, the
/// table without that column, then the ledger's combined table, each with
/// a row per crate and, for more than one crate, `total` and `shipped`;
/// a premise file, the ledger or a ledger entry it cannot read exits 2,
/// and a ledger error ([`ledger::match_ledger`]) exits 1.
fn report(
    root: &Path,
    cfg: &Config,
    c: &Counter,
    unshipped: &[String],
    tests: &std::collections::BTreeSet<String>,
) -> Floor {
    let mut total = Tally::default();
    let mut shipped = Tally::default();
    for (k, t) in &c.tallies {
        total.add(t);
        if !unshipped.contains(k) {
            shipped.add(t);
        }
    }
    let Some(l) = &cfg.ledger else {
        println!(
            "crate\tverified\ttrusted\tdischarged\toutside\tverified%\tchecked%\tlines verified\tlines trusted\tlines discharged\tlines outside\tlines verified%\tlines checked%\tghost"
        );
        for (k, t) in &c.tallies {
            print_row(k, t);
        }
        print_row("total", &total);
        print_row("shipped", &shipped);
        fail_on_errors(c.errors);
        return shares(&shipped, (0, 0));
    };
    let several = c.tallies.len() > 1;
    println!(
        "crate\tverified\ttrusted\toutside\tverified%\tlines verified\tlines trusted\tlines outside\tlines verified%\tghost"
    );
    for (k, t) in &c.tallies {
        print_ledger_row(k, t);
    }
    if several {
        print_ledger_row("total", &total);
        print_ledger_row("shipped", &shipped);
    }
    fail_on_errors(c.errors);
    let premises = ledger::Premises {
        isla: l
            .isla
            .as_ref()
            .map(|p| ledger::Labels::read(root, p).unwrap_or_else(|e| usage(e))),
        kani: l
            .kani
            .as_ref()
            .map(|p| ledger::Labels::read(root, p).unwrap_or_else(|e| usage(e))),
        tests: tests.clone(),
    };
    let seams = ledger::read_ledger(&root.join(&l.dir), &premises)
        .unwrap_or_else(|e| usage(format!("{}: {e}", l.dir)));
    let (checked, warnings, errors) = ledger::match_ledger(&c.trusted, &seams);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "crate\tverified+discharged\tdischarged\tpartly checked\tverified+discharged%\tlines verified+discharged\tlines discharged\tlines partly checked\tlines verified+discharged%"
    );
    let mut total_checked = ledger::Checked::default();
    let mut shipped_checked = ledger::Checked::default();
    for (k, t) in &c.tallies {
        let ch = checked.get(k).copied().unwrap_or_default();
        print_combined_row(k, t, &ch);
        total_checked.add(&ch);
        if !unshipped.contains(k) {
            shipped_checked.add(&ch);
        }
    }
    if several {
        print_combined_row("total", &total, &total_checked);
        print_combined_row("shipped", &shipped, &shipped_checked);
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("FAIL ledger: {e}");
        }
        std::process::exit(1);
    }
    shares(&shipped, shipped_checked.discharged)
}

/// One row of the ledger back end's first table: the classes Verus sees,
/// without the Kani marker's discharged column.
fn print_ledger_row(name: &str, t: &Tally) {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    let [v, tr, o] = [Class::Verified, Class::Trusted, Class::Outside].map(|c| c as usize);
    println!(
        "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        t.fns[v],
        t.fns[tr],
        t.fns[o],
        pct(t.fns[v], fns),
        t.lines[v],
        t.lines[tr],
        t.lines[o],
        pct(t.lines[v], lines),
        t.ghost
    );
}

/// One row of the ledger back end's combined table: verified and
/// discharged, discharged, partly checked, and the combined share.
fn print_combined_row(name: &str, t: &Tally, c: &ledger::Checked) {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    let v = Class::Verified as usize;
    let (vf, vl) = (t.fns[v] + c.discharged.0, t.lines[v] + c.discharged.1);
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

/// The headroom check ([`rlimit`]): reads Verus's statistics from
/// `stats` and the RLIMIT file under `root`, prints each crate's results and
/// the report, and, with `base`, holds the RLIMIT file to that revision's
/// copy ([`Base::copy`]). A crate of `crates` (those with `verus!` code)
/// that the statistics leave out fails the run with `base`
/// ([`rlimit::check_covered`]).
/// `dirs` are the crates' directories, whose base sources [`base_limits`]
/// reads; `c` holds the head's limits and every function Verus checks
/// ([`Counter::verus_fns`]). Whether it passes.
fn check_rlimit(
    root: &Path,
    cfg: &Config,
    dirs: &[Krate],
    stats: &Path,
    crates: &[String],
    c: &Counter,
    base: Option<&Base>,
) -> bool {
    let limits = &c.limits;
    let list = cfg.rlimit.path.as_str();
    let fail = |e: String| -> ! {
        eprintln!("FAIL rlimit headroom: {e}");
        std::process::exit(2);
    };
    let text =
        fs::read_to_string(stats).unwrap_or_else(|e| fail(format!("{}: {e}", stats.display())));
    let stats =
        rlimit::read_stats(&text).unwrap_or_else(|e| fail(format!("{}: {e}", stats.display())));
    let file = root.join(list);
    let head = fs::read_to_string(&file)
        .map_err(|e| e.to_string())
        .and_then(|t| rlimit::parse_rlimit_file(&t))
        .unwrap_or_else(|e| fail(format!("{}: {e}", file.display())));
    for (k, s) in stats.iter().filter(|(k, _)| crates.contains(k)) {
        println!("{k}: {} verified, {} errors", s.verified, s.errors);
    }
    let (covered, report) = rlimit::check_covered(crates, &stats, base.is_some());
    print!("{report}");
    let shares = rlimit::shares(&stats, crates, limits).unwrap_or_else(|e| fail(e));
    let (mut ok, report) = rlimit::check(&shares, &head, 15, list);
    ok &= covered;
    print!("{report}");
    if let Some(b) = base {
        let rev = b.rev.as_str();
        let base = match b.copy(root, list, GateFile::Rlimit) {
            Ok((path, Some(text))) => {
                let base = rlimit::parse_rlimit_file(&text)
                    .unwrap_or_else(|e| fail(format!("{rev}:{path}: {e}")));
                let (kept, report) = rlimit::check_ratchet(&head, &base, list);
                print!("{report}");
                ok &= kept;
                base
            }
            Ok((path, None)) => {
                println!("ok rlimit headroom: {rev} has no {path} to compare with");
                rlimit::RlimitFile::default()
            }
            Err(e) => fail(e),
        };
        let was = base_limits(root, cfg, dirs, rev).unwrap_or_else(|e| fail(e));
        let (kept, report) = rlimit::check_raised(limits, &c.verus_fns, &was, &head, &base, list);
        print!("{report}");
        ok &= kept;
    }
    ok
}

/// The `#[verifier::rlimit]` limits of `rev`'s sources of the crates in
/// `dirs` (name and directory, under `root`), read from `rev`'s copy of
/// each crate's files by the walk the working tree's are read with
/// ([`Counter::walk_crate`]): test-only modules left out, and the crate's
/// `macro_rules!` collected and expanded, so that a limit a macro writes,
/// with its definition in any file, is the base's limit too.
fn base_limits(
    root: &Path,
    cfg: &Config,
    dirs: &[Krate],
    rev: &str,
) -> Result<Vec<rlimit::Limit>, String> {
    // Each crate's `src/`, relative to the root, as git names paths.
    let srcs: Vec<(String, PathBuf)> = dirs
        .iter()
        .map(|k| {
            let rel: PathBuf = k
                .dir
                .strip_prefix(root)
                .unwrap_or(&k.dir)
                .join("src")
                .components()
                .filter(|c| !matches!(c, std::path::Component::CurDir))
                .collect();
            (k.name.clone(), rel)
        })
        .collect();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "safe.directory=*"])
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let mut c = Counter::new(
        false,
        Atoms::of(cfg),
        cfg.reach.as_ref().and_then(|r| r.fn_ptr_macro.clone()),
    );
    // The revision's copy of each crate (its sources and its Cargo.toml,
    // which names its roots) is written to a scratch directory and read by
    // the walk the working tree is read with.
    let scratch = std::env::temp_dir().join(format!(
        "verus-count-base-{}-{}",
        std::process::id(),
        rev.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_")
    ));
    let _ = fs::remove_dir_all(&scratch);
    let read = (|| -> Result<(), String> {
        // The revision's root Cargo.toml, whose `[workspace]` tables a
        // crate inherits its edition and dependencies from
        // ([`crate_manifest`]), when the revision has one.
        let shown = git(&["show", &format!("{rev}:Cargo.toml")])?;
        if shown.status.success() {
            fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
            fs::write(scratch.join("Cargo.toml"), &shown.stdout)
                .map_err(|e| format!("{}: {e}", scratch.display()))?;
        }
        for (krate, src) in &srcs {
            let kdir = src.parent().unwrap_or(Path::new("")).to_path_buf();
            let listed = git(&[
                "ls-tree",
                "-r",
                "--name-only",
                rev,
                "--",
                &src.display().to_string(),
                &kdir.join("Cargo.toml").display().to_string(),
            ])?;
            if !listed.status.success() {
                return Err(format!(
                    "git ls-tree {rev}: {}",
                    String::from_utf8_lossy(&listed.stderr).trim()
                ));
            }
            for path in String::from_utf8_lossy(&listed.stdout).lines() {
                if !path.ends_with(".rs") && !path.ends_with("Cargo.toml") {
                    continue;
                }
                let shown = git(&["show", &format!("{rev}:{path}")])?;
                if !shown.status.success() {
                    return Err(format!(
                        "git show {rev}:{path}: {}",
                        String::from_utf8_lossy(&shown.stderr).trim()
                    ));
                }
                let to = scratch.join(path);
                if let Some(d) = to.parent() {
                    fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
                }
                fs::write(&to, &shown.stdout).map_err(|e| format!("{}: {e}", to.display()))?;
            }
            fs::create_dir_all(scratch.join(src)).map_err(|e| format!("{e}"))?;
            c.krate = krate.clone();
            c.walk_crate(cfg, (&scratch, &scratch.join(&kdir)), None);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&scratch);
    read?;
    if c.errors > 0 {
        return Err(format!(
            "{rev}: {} errors reading its sources for their rlimits",
            c.errors
        ));
    }
    Ok(c.limits)
}

/// The figures of `t`, as verus/FLOOR records them: the verified share of
/// functions and of lines, then the verified and discharged share of each,
/// with `ledger` the functions and lines the generated ledger discharges
/// besides the Kani marker's.
fn shares(t: &Tally, ledger: (usize, usize)) -> Floor {
    let (fns, lines) = (t.fns.iter().sum(), t.lines.iter().sum());
    let (v, d) = (Class::Verified as usize, Class::Discharged as usize);
    Floor([
        Some(tenths(t.fns[v], fns)),
        Some(tenths(t.lines[v], lines)),
        Some(tenths(t.fns[v] + t.fns[d] + ledger.0, fns)),
        Some(tenths(t.lines[v] + t.lines[d] + ledger.1, lines)),
    ])
}

/// One row of the table. `verified%` is the share Verus checks;
/// `checked%` adds the functions Kani discharges, which hold on the
/// bounded inputs their records name.
fn print_row(name: &str, t: &Tally) {
    let fns: usize = t.fns.iter().sum();
    let lines: usize = t.lines.iter().sum();
    let [v, tr, o, d] = [
        Class::Verified,
        Class::Trusted,
        Class::Outside,
        Class::Discharged,
    ]
    .map(|c| c as usize);
    println!(
        "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        t.fns[v],
        t.fns[tr],
        t.fns[d],
        t.fns[o],
        pct(t.fns[v], fns),
        pct(t.fns[v] + t.fns[d], fns),
        t.lines[v],
        t.lines[tr],
        t.lines[d],
        t.lines[o],
        pct(t.lines[v], lines),
        pct(t.lines[v] + t.lines[d], lines),
        t.ghost
    );
}

/// `REV:FILE` under `root`'s repository (FILE verus/FLOOR or
/// verus/RLIMIT), failing closed: a
/// revision that does not resolve to a commit (a shallow clone, another
/// uid's checkout git refuses, an empty name) is an error, and only a
/// resolved commit without the file is `None`.
fn base_file(root: &Path, rev: &str, file: &str) -> Result<Option<String>, String> {
    let commit = base_commit(root, rev, file)?;
    let path = format!("{commit}:{file}");
    if !git_in(root, &["cat-file", "-e", &path])?.status.success() {
        return Ok(None);
    }
    let shown = git_in(root, &["show", &path])?;
    if !shown.status.success() {
        return Err(format!(
            "git show {path}: {}",
            String::from_utf8_lossy(&shown.stderr).trim()
        ));
    }
    Ok(Some(String::from_utf8_lossy(&shown.stdout).into_owned()))
}

/// The crates `rev`'s tree holds under `dir`, as [`crate_dirs`] reads a
/// `dir` at the head: each subdirectory with a `src/` directory, by name,
/// in path order. A `dir` the tree lacks holds none.
fn base_dir_crates(root: &Path, rev: &str, dir: &str) -> Result<Vec<String>, String> {
    let commit = base_commit(root, rev, dir)?;
    let dir = config::normal(dir).to_string_lossy().into_owned();
    let tree = format!("{commit}:{dir}");
    if !git_in(root, &["cat-file", "-e", &tree])?.status.success() {
        return Ok(Vec::new());
    }
    let listed = git_in(root, &["ls-tree", "-z", "-d", "--name-only", &tree])?;
    if !listed.status.success() {
        return Err(format!(
            "git ls-tree {tree}: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        ));
    }
    let mut out = Vec::new();
    for name in String::from_utf8_lossy(&listed.stdout)
        .split('\0')
        .filter(|n| !n.is_empty())
    {
        let src = Path::new(&dir).join(name).join("src");
        let kind = git_in(
            root,
            &[
                "cat-file",
                "-t",
                &format!("{commit}:{}", src.to_string_lossy()),
            ],
        )?;
        if kind.status.success() && String::from_utf8_lossy(&kind.stdout).trim() == "tree" {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

/// The commit `rev` names under `root`'s repository, failing closed as
/// [`base_file`] does, for reading `what` there.
fn base_commit(root: &Path, rev: &str, what: &str) -> Result<String, String> {
    let commit = git_in(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )?;
    if !commit.status.success() {
        return Err(format!(
            "base revision {rev:?} does not resolve to a commit, so {what} cannot be compared with it"
        ));
    }
    Ok(String::from_utf8_lossy(&commit.stdout).trim().to_owned())
}

/// `git ARGS` in `root`'s repository, whoever owns the checkout.
fn git_in(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "safe.directory=*"])
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))
}

/// The figures the FLOOR file holds, and the file: the verified share of
/// functions and of their lines (`functions`, `lines`), then the share
/// verified or discharged of each, named with the configured prefix
/// (`checked-functions` and `checked-lines`, or challoc's `combined-`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Figures {
    names: [String; 4],
    file: String,
    /// The names a base revision's copy may still give the last two
    /// figures under (`[floor] renamed_from`), read as those figures.
    renamed: Option<[String; 2]>,
}

impl Default for Figures {
    fn default() -> Self {
        let c = config::Floor::default();
        Figures::new(&c.checked, &c.path)
    }
}

/// What each figure counts, for the report.
const MEANING: [&str; 4] = [
    "verified",
    "verified",
    "verified or discharged",
    "verified or discharged",
];

/// The figures every FLOOR file gives, the first of [`Figures::names`].
/// The others are required of the file under test (`main`) but may be
/// missing from a base revision's copy, which is then not compared on
/// them.
const REQUIRED: usize = 2;

/// Shares in tenths of a percent, rounded down (84 is 8.4%), by
/// [`Figures::names`]' index; `None` where a file gives none.
#[derive(Debug, PartialEq, Eq)]
struct Floor([Option<u32>; 4]);

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

impl Figures {
    /// The figures with the verified-or-discharged ones named
    /// `{checked}-functions` and `{checked}-lines`, of the FLOOR file
    /// `file`.
    fn new(checked: &str, file: &str) -> Figures {
        Figures {
            names: [
                "functions".into(),
                "lines".into(),
                format!("{checked}-functions"),
                format!("{checked}-lines"),
            ],
            file: file.into(),
            renamed: None,
        }
    }

    /// These figures as a base revision's copy may give them: with
    /// `renamed_from`, its `{renamed_from}-functions` and
    /// `{renamed_from}-lines` are read as the checked figures, and a
    /// `lowered` line naming one as naming that figure.
    fn for_base(&self, renamed_from: Option<&str>) -> Figures {
        Figures {
            renamed: renamed_from.map(|p| [format!("{p}-functions"), format!("{p}-lines")]),
            ..self.clone()
        }
    }

    /// [`Figures::parse`], or exits naming `name` and the error.
    fn read(&self, text: &str, name: &str) -> FloorFile {
        self.parse(text).unwrap_or_else(|e| {
            eprintln!("{name}: {e}");
            std::process::exit(2);
        })
    }

    /// Reads the FLOOR file: `#` comments, one `FIGURE P` line per figure (P a
    /// percentage with at most one decimal; `functions` and `lines` required,
    /// `checked-functions` and `checked-lines` optional here), and any number
    /// of `lowered FIGURE P REASON` lines.
    fn parse(&self, text: &str) -> Result<FloorFile, String> {
        let mut floor = Floor([None; 4]);
        let mut lowered = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let n = i + 1;
            let figure = |key: &str, value: &str| -> Result<(usize, u32), String> {
                let renamed = || {
                    let old = self.renamed.as_ref()?;
                    old.iter().position(|f| f == key).map(|j| REQUIRED + j)
                };
                let Some(k) = self.names.iter().position(|f| f == key).or_else(renamed) else {
                    return Err(format!("line {n}: unknown figure {key:?}"));
                };
                parse_tenths(value)
                    .map(|v| (k, v))
                    .ok_or_else(|| format!("line {n}: {value:?} is not P or P.D"))
            };
            match words.as_slice() {
                ["lowered", key, value, reason @ ..] => {
                    if reason.is_empty() {
                        return Err(format!("line {n}: a lowered line needs a reason"));
                    }
                    let (k, v) = figure(key, value)?;
                    lowered.push((self.names[k].clone(), v));
                }
                [key, value] => {
                    let (k, v) = figure(key, value)?;
                    if floor.0[k].replace(v).is_some() {
                        return Err(format!("line {n}: {key} given twice"));
                    }
                }
                _ => {
                    return Err(format!(
                        "line {n}: want `FIGURE P` (FIGURE one of {}) or `lowered FIGURE P REASON`",
                        self.names.join(", ")
                    ));
                }
            }
        }
        if floor.0[..REQUIRED].iter().any(Option::is_none) {
            return Err("want both a functions and a lines figure".into());
        }
        Ok(FloorFile { floor, lowered })
    }

    /// The first figure `f` does not give, if any: the file under test gives
    /// them all.
    fn missing(&self, f: &Floor) -> Option<&str> {
        f.0.iter()
            .position(Option::is_none)
            .map(|k| self.names[k].as_str())
    }

    /// Whether each figure in `head` is at least the base revision's, or is
    /// set lower with a `lowered FIGURE P REASON` line naming that figure and
    /// value that the base's file does not already have, so lowering the floor
    /// is never silent and an old reason never covers a new drop. A figure
    /// the base does not give is not compared.
    fn lowering(&self, head: &FloorFile, base: &FloorFile) -> (bool, String) {
        let file = &self.file;
        let mut ok = true;
        let mut report = String::new();
        for (k, name) in self.names.iter().enumerate() {
            let (Some(h), Some(b)) = (head.floor.0[k], base.floor.0[k]) else {
                continue;
            };
            if h >= b {
                continue;
            }
            let explained = head.lowered.iter().any(|(key, v)| {
                key == name && *v == h && !base.lowered.iter().any(|(bk, bv)| bk == key && bv == v)
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
                    "FAIL coverage floor: {file} lowers {name} from {}% to {}% with no `lowered {name} {} <reason>` line\n",
                    shown(b),
                    shown(h),
                    shown(h)
                );
            }
        }
        (ok, report)
    }

    /// Whether `got` meets `want`, with one line per figure `want` gives
    /// saying so. A figure above its floor names the value to record, so the
    /// floor only rises.
    fn check(&self, got: &Floor, want: &Floor) -> (bool, String) {
        let file = &self.file;
        let mut ok = true;
        let mut report = String::new();
        for (k, name) in self.names.iter().enumerate() {
            let (Some(g), Some(w)) = (got.0[k], want.0[k]) else {
                continue;
            };
            let what = MEANING[k];
            if g < w {
                ok = false;
                report += &format!(
                    "FAIL coverage floor: {name} {what} {}%, below the floor's {}%\n",
                    shown(g),
                    shown(w)
                );
            } else if g > w {
                report += &format!(
                    "ok coverage floor: {name} {what} {}%, above the floor's {}%; raise {file} to `{name} {}`\n",
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
}

/// The one coverage badge line FLOOR's `functions` and `lines` figures
/// give (`[badge]`, `--badge`): the figures with one decimal, in the alt
/// text and in the shields.io URL, linking to verus/README.md's
/// "Coverage". The repositories are private, so shields.io cannot read
/// the figures; they live in the line, and [`check_badge`] holds them to
/// FLOOR.
fn badge_line(floor: &Floor) -> String {
    let [functions, lines, ..] = floor.0.map(|v| shown(v.unwrap_or(0)));
    format!(
        "[![Verus: {functions}% of functions, {lines}% of lines](https://img.shields.io/badge/Verus-{functions}%25%20of%20functions%2C%20{lines}%25%20of%20lines-blue)](verus/README.md#coverage)"
    )
}

/// Whether `rendered`, the part of a line Markdown renders, holds a Verus
/// badge in any form: `verus` in any case, with `shields.io` or an image
/// (`![`). The match is wide on purpose: a line it takes for a badge that
/// is not one fails the gate, while a second badge it missed would pass.
fn is_badge(rendered: &str) -> bool {
    let lower = rendered.to_lowercase();
    lower.contains("verus") && (lower.contains("shields.io") || lower.contains("!["))
}

/// The fence a line opens or closes: its character (`` ` `` or `~`) and
/// how many, after up to three spaces of indent. CommonMark refuses a
/// backtick fence whose info string holds a backtick, since that line is
/// inline code.
fn fence(line: &str) -> Option<(char, usize)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let c = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = rest.chars().take_while(|x| *x == c).count();
    (n >= 3 && !(c == '`' && rest[n..].contains('`'))).then_some((c, n))
}

/// Each line of `text`, numbered from 1, with the part Markdown renders:
/// a line inside a fenced code block renders as none, and the spans
/// inside HTML comments (`<!--` to `-->`, across lines) are dropped. A
/// badge on a line that does not render is neither the badge nor a second
/// one.
fn rendered_lines(text: &str) -> Vec<(usize, &str, String)> {
    let mut open: Option<(char, usize)> = None;
    let mut comment = false;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Some((c, n)) = open {
            if fence(line)
                .is_some_and(|(d, m)| d == c && m >= n && line.trim().chars().all(|x| x == c))
            {
                open = None;
            }
            out.push((i + 1, line, String::new()));
            continue;
        }
        if !comment && let Some(f) = fence(line) {
            open = Some(f);
            out.push((i + 1, line, String::new()));
            continue;
        }
        let mut shown = String::new();
        let mut rest = line;
        loop {
            if comment {
                match rest.find("-->") {
                    Some(end) => {
                        comment = false;
                        rest = &rest[end + 3..];
                    }
                    None => break,
                }
            } else {
                match rest.find("<!--") {
                    Some(start) => {
                        shown += &rest[..start];
                        comment = true;
                        rest = &rest[start + 4..];
                    }
                    None => {
                        shown += rest;
                        break;
                    }
                }
            }
        }
        out.push((i + 1, line, shown));
    }
    out
}

/// Holds `text`, the badge file `name`, to `want`, the line
/// [`badge_line`] gives for the FLOOR file `floor`: exactly one rendered
/// line holds a Verus badge ([`is_badge`], [`rendered_lines`]), and it is
/// `want`. The report line, or the failure.
fn badge_report(text: &str, name: &str, floor: &str, want: &str) -> Result<String, String> {
    let found: Vec<(usize, &str)> = rendered_lines(text)
        .into_iter()
        .filter(|(_, _, shown)| is_badge(shown))
        .map(|(n, l, _)| (n, l))
        .collect();
    match found.as_slice() {
        [] => Err(format!(
            "FAIL badge: {name} has no Verus badge; add this line, which gives {floor}'s functions and lines figures:\n{want}"
        )),
        [(n, line)] if *line == want => Ok(format!(
            "ok badge: {name}:{n} gives {floor}'s functions and lines figures"
        )),
        [(n, _)] => Err(format!(
            "FAIL badge: {name}:{n} is not the badge {floor}'s functions and lines figures give; replace it with this line:\n{want}"
        )),
        _ => Err(format!(
            "FAIL badge: {name} has {} Verus badges (lines {}); keep one, this line:\n{want}",
            found.len(),
            found
                .iter()
                .map(|(n, _)| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The badge gate (`[badge]`, run with `--floor`): `path` under `root`
/// must hold the one badge line `floor_file`'s figures `floor` give
/// ([`badge_report`]), so the badge cannot drift from FLOOR. A `path` that
/// is not relative, has a `..`, or passes through a symlink exits 2, as a
/// `[floor] path` through one does: the base comparison of `[badge] path`
/// ([`Config::narrowed_from`]) goes by the path, which another spelling or
/// a link would slip past. Returns whether the badge passes.
fn check_badge(root: &Path, path: &str, floor_file: &str, floor: &Floor) -> bool {
    let rel = Path::new(path);
    if rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        usage(format!(
            "FAIL badge: [badge] path {path} is not a relative path without `..`: give the file under {}",
            root.display()
        ));
    }
    if let Some(link) = symlink_in(root, rel) {
        usage(format!(
            "FAIL badge: [badge] path {path} passes through {}, a symlink: give `path` the file itself",
            link.display()
        ));
    }
    let text = match fs::read_to_string(root.join(rel)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FAIL badge: {path}: {e}");
            return false;
        }
    };
    match badge_report(&text, path, floor_file, &badge_line(floor)) {
        Ok(r) => {
            println!("{r}");
            true
        }
        Err(e) => {
            eprintln!("{e}");
            false
        }
    }
}

/// `P` or `P.D` as tenths of a percent, at most 100.0.
fn parse_tenths(s: &str) -> Option<u32> {
    parse_tenths_to(s, 1000)
}

/// `P` or `P.D` as tenths of a percent, at most `max` tenths.
fn parse_tenths_to(s: &str, max: u32) -> Option<u32> {
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
    (v <= max).then_some(v)
}

/// The text of every `kani-*.md` record in `dir` (verus/seams).
fn kani_records(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("kani-") && n.ends_with(".md"))
        })
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect()
}

/// The discharged functions no Kani record covers. A record covers one
/// when it names the function's file in backquotes and quotes `fn NAME`
/// (followed by no further identifier character), as a record's contract
/// section does. The marker alone moves a function into the checked share,
/// so a marker without its record fails the run.
fn unrecorded(discharged: &[(PathBuf, String)], records: &[String]) -> Vec<(PathBuf, String)> {
    discharged
        .iter()
        .filter(|(file, name)| {
            let file = format!("`{}`", file.display());
            let quote = format!("fn {name}");
            !records.iter().any(|r| {
                r.contains(&file)
                    && r.match_indices(&quote).any(|(i, _)| {
                        !r[i + quote.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
            })
        })
        .cloned()
        .collect()
}

/// Appends to `out` each function `tokens` calls directly, at any depth of
/// nesting: `(false, name)` for `name(..)`, a free function of the file, and
/// `(true, name)` for `Self::name(..)` or `self.name(..)`, one of the same
/// `impl` or trait, each with or without a turbofish (`name::<T>(..)`). A call
/// through another path or receiver (`Vec::new()`, `self.v.get(..)`) names
/// some other function, so it is left out; so are macro calls and
/// declarations (`fn name(..)`).
/// The index after `toks[i]`, past a turbofish (`::<..>`, balanced, with
/// `->` inside it not closing it) when one follows.
fn past_turbofish(toks: &[TokenTree], i: usize) -> usize {
    let punct =
        |k: usize, c: char| matches!(toks.get(k), Some(TokenTree::Punct(p)) if p.as_char() == c);
    if !(punct(i + 1, ':') && punct(i + 2, ':') && punct(i + 3, '<')) {
        return i + 1;
    }
    let mut depth = 0usize;
    let mut k = i + 3;
    while k < toks.len() {
        if punct(k, '<') {
            depth += 1;
        } else if punct(k, '>') && !punct(k - 1, '-') {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    i + 1
}

fn direct_calls(tokens: TokenStream, out: &mut Vec<(bool, String)>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        if let TokenTree::Group(g) = t {
            direct_calls(g.stream(), out);
            continue;
        }
        let TokenTree::Ident(id) = t else { continue };
        let paren = matches!(toks.get(past_turbofish(&toks, i)),
            Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Parenthesis);
        if !paren {
            continue;
        }
        let before = |k: usize| i.checked_sub(k).and_then(|j| toks.get(j));
        let call = match before(1) {
            None => Some(false),
            Some(TokenTree::Punct(p)) if p.as_char() == '.' => {
                matches!(before(2), Some(TokenTree::Ident(r)) if r == "self").then_some(true)
            }
            Some(TokenTree::Punct(p)) if p.as_char() == ':' => {
                matches!(before(3), Some(TokenTree::Ident(r)) if r == "Self").then_some(true)
            }
            Some(TokenTree::Ident(k)) if k == "fn" => None,
            Some(_) => Some(false),
        };
        if let Some(method) = call {
            out.push((method, id.to_string()));
        }
    }
}

/// The recursion check: each function whose name lies on a cycle of direct calls
/// ([`direct_calls`]) among the functions of its file, with the cycle, in the shipped crates. Verus
/// checks recursion through verified functions (each needs a `decreases`
/// whose bound verus/theorems/RN1.md states); the bodies it does not check
/// may not recurse at all (RN1). Functions are matched by name within a
/// file, which joins two methods of one name in one file: such a cycle is
/// reported too. Recursion through a call across files, or through another
/// receiver or path, is not seen (RN1).
fn recursive(fns: &[FnRef], unshipped: &[String]) -> Vec<(usize, Vec<String>)> {
    use std::collections::{BTreeSet, VecDeque};
    type Key<'a> = (&'a Path, &'a str, &'a str);
    let mut edges: BTreeMap<Key, BTreeSet<Key>> = BTreeMap::new();
    let mut names: BTreeSet<Key> = BTreeSet::new();
    for f in fns {
        names.insert((&f.file, &f.owner, &f.name));
    }
    for f in fns {
        let e = edges.entry((&f.file, &f.owner, &f.name)).or_default();
        for (method, c) in &f.calls {
            let to = (
                f.file.as_path(),
                if *method { f.owner.as_str() } else { "" },
                c.as_str(),
            );
            if names.contains(&to) {
                e.insert(to);
            }
        }
    }
    // The shortest cycle through `start`, by breadth-first search back to it.
    let cycle = |start: Key| -> Option<Vec<String>> {
        let mut parent: BTreeMap<Key, Key> = BTreeMap::new();
        let mut queue = VecDeque::new();
        for &n in edges.get(&start)? {
            if n == start {
                return Some(vec![start.2.to_string(), start.2.to_string()]);
            }
            if let std::collections::btree_map::Entry::Vacant(e) = parent.entry(n) {
                e.insert(start);
                queue.push_back(n);
            }
        }
        while let Some(n) = queue.pop_front() {
            for &m in edges.get(&n).into_iter().flatten() {
                if m == start {
                    let mut back = Vec::new();
                    let mut at = n;
                    while at != start {
                        back.push(at.2.to_string());
                        at = parent[&at];
                    }
                    back.push(start.2.to_string());
                    back.reverse();
                    back.push(start.2.to_string());
                    return Some(back);
                }
                if m != start
                    && let std::collections::btree_map::Entry::Vacant(e) = parent.entry(m)
                {
                    e.insert(n);
                    queue.push_back(m);
                }
            }
        }
        None
    };
    let mut found = Vec::new();
    for (i, f) in fns.iter().enumerate() {
        if unshipped.contains(&f.krate) {
            continue;
        }
        if let Some(c) = cycle((&f.file, &f.owner, &f.name)) {
            found.push((i, c));
        }
    }
    found
}

/// The functions of `fns` as the walks ([`inputs`]) read them.
fn funcs(fns: &[FnRef]) -> Vec<inputs::Func<'_>> {
    fns.iter()
        .map(|f| inputs::Func {
            krate: &f.krate,
            file: &f.file,
            line: f.line,
            owner: inputs::base(&f.owner),
            name: &f.name,
            params: f.params,
            receiver: f.receiver,
            body: &f.body,
        })
        .collect()
}

/// The compile-inputs check (`[inputs]`, pcre3's verus/theorems/CD1.md,
/// [`inputs`]): every hidden input a function reachable from the
/// configured roots reads is in the table `i` names, and every row of the
/// table is still read. Prints what it finds; `false` when it fails.
fn compile_inputs(root: &Path, i: &config::Inputs, c: &Counter) -> bool {
    let cd1 = fs::read_to_string(root.join(&i.table)).unwrap_or_default();
    let table = &i.table;
    let short = Path::new(table)
        .file_name()
        .map_or(table.clone(), |n| n.to_string_lossy().into_owned());
    let describe = &i.describe;
    let roots = inputs::Roots {
        crates: i.crates.iter().map(String::as_str).collect(),
        roots: &i.roots,
        lazy: i.lazy,
    };
    let funcs = funcs(&c.fns);
    let found = inputs::findings(&funcs, &c.statics, &c.traits, &c.types, &c.aliases, &roots);
    let reached = inputs::reachable(&funcs, &c.traits, &c.aliases, &roots).len();
    let mut ok = true;
    let mut resolved = Vec::new();
    for (r, matched) in i.roots.iter().zip(inputs::resolved_roots(&funcs, &roots)) {
        let name = r
            .owner
            .as_ref()
            .map_or(r.name.clone(), |o| format!("{o}::{}", r.name));
        if matched.is_empty() {
            ok = false;
            eprintln!(
                "FAIL compile inputs: the root `{name}` in a file ending in {} matches no function of the crates {}, so the walk never starts from it; correct its file, owner or name",
                r.file,
                i.crates.join(", ")
            );
        }
        for m in matched {
            let f = &funcs[m];
            resolved.push(format!("{name} at {}:{}", f.file.display(), f.line));
        }
    }
    let mut allowed = 0;
    for x in &found {
        let f = &funcs[x.func];
        if inputs::allowed(&cd1, f, &x.input) {
            allowed += 1;
            continue;
        }
        ok = false;
        eprintln!(
            "FAIL compile inputs: {}:{}: `{}` reads `{}` and is reachable from {describe} ({}), but {table}'s table has no row naming the input, its file and `fn {}`",
            f.file.display(),
            f.line,
            f.name,
            x.input,
            x.chain.join(" -> "),
            f.name
        );
    }
    for row in inputs::stale_rows(&cd1, &funcs, &found) {
        ok = false;
        eprintln!("FAIL compile inputs: {table} allows a read no reachable function makes: {row}");
    }
    if ok {
        println!(
            "ok compile inputs: {reached} functions reachable from {describe} (roots {}); {allowed} reads of hidden inputs, each in {short}'s table",
            resolved.join(", ")
        );
    }
    ok
}

/// The functions of `crates` a C caller enters by ([`reach`]): each one
/// that exports its symbol (`#[no_mangle]` or `#[export_name]`, whose ABI
/// [`Counter::entry`] holds to C's) or whose address a `const` or
/// `static` hands out (`FnRef::entry`); each free function an exports
/// macro block (pcre3's `width::exports!`) in its file exports
/// (`exported`); and each `conversion` method, one-argument and without a
/// receiver (pcre3's `from_c`, the `width::FromC` conversions the exports'
/// wrappers apply to their arguments: the identity one, and those
/// `width::fn_ptr!` writes).
fn entry_roots(
    fns: &[FnRef],
    crates: &[&str],
    exported: &std::collections::BTreeSet<(PathBuf, String)>,
    conversion: Option<&str>,
) -> std::collections::BTreeSet<usize> {
    (0..fns.len())
        .filter(|&i| {
            let f = &fns[i];
            crates.contains(&f.krate.as_str())
                && (f.entry
                    || (f.owner.is_empty() && exported.contains(&(f.file.clone(), f.name.clone())))
                    || (conversion == Some(f.name.as_str())
                        && !f.owner.is_empty()
                        && !f.receiver
                        && f.params == 1))
        })
        .collect()
}

/// Each body an `exports!` block names (`exported`) that no free function
/// of `crates` in the block's file matches: the block's wrappers call a
/// body [`entry_roots`] cannot root, so it is an error rather than a root
/// that drops out (a body moved to another module and imported, say).
fn unmatched_exports(
    fns: &[FnRef],
    crates: &[&str],
    exported: &std::collections::BTreeSet<(PathBuf, String)>,
    mac: &str,
) -> Vec<String> {
    exported
        .iter()
        .filter(|(file, name)| {
            !fns.iter().any(|f| {
                crates.contains(&f.krate.as_str())
                    && f.owner.is_empty()
                    && &f.file == file
                    && &f.name == name
            })
        })
        .map(|(file, name)| {
            format!(
                "{}: an {mac}! block exports `{name}`, but the file defines no free function of that name, so it roots nothing; define the body in the block's file",
                file.display()
            )
        })
        .collect()
}

/// The entry-point reachability gate (`[reach]` `r`, [`reach`], verus/REACH):
/// every function of a shipped crate reachable from a C entry point is
/// verified, discharged, or listed in REACH with a reason, and every row
/// of REACH is such a function, citing seams of `seams` (their IDs, and
/// the tables as the report names them); with `base`, a row that
/// revision's REACH ([`Base::copy`]) does not have is marked `new`
/// ([`reach::check_added`]). Prints what it finds; `false` when it fails.
fn entry_reach(
    root: &Path,
    r: &config::Reach,
    stand_ins: &[[String; 2]],
    c: &Counter,
    unshipped: &[String],
    seams: (&std::collections::BTreeSet<String>, &str),
    base: Option<&Base>,
) -> bool {
    let (fns, traits, aliases) = (&c.fns, &c.traits, &c.aliases);
    let list = r.path.as_str();
    // A base revision's REACH is read at its `[reach] path`, so the file
    // must exist wherever the section names it, rows or none.
    let text = match fs::read_to_string(root.join(list)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "FAIL entry reach: {list}: {e}: [reach] names it, so it must exist, if only with no rows"
            );
            return false;
        }
    };
    let funcs = funcs(fns);
    let shipped: std::collections::BTreeSet<&str> = fns
        .iter()
        .map(|f| f.krate.as_str())
        .chain(c.files.iter().map(|(k, _)| k.as_str()))
        .filter(|k| !unshipped.iter().any(|u| u == k))
        .collect();
    let crates: Vec<&str> = shipped.into_iter().collect();
    // Every file a shipped build compiles, not only those holding a
    // function (an export block in a file of `use`s alone still exports),
    // and each that holds one.
    let files: std::collections::BTreeSet<PathBuf> = c
        .files
        .iter()
        .map(|(k, f)| (k, f))
        .chain(fns.iter().map(|f| (&f.krate, &f.file)))
        .filter(|(k, _)| crates.contains(&k.as_str()))
        .map(|(_, f)| f.clone())
        .collect();
    let (exported, unread) = match &r.exports_macro {
        Some(mac) => reach::exported(root, &files, mac, stand_ins),
        None => Default::default(),
    };
    let roots = entry_roots(fns, &crates, &exported, r.conversion.as_deref());
    let parent = inputs::walk(
        &funcs,
        traits,
        aliases,
        &crates,
        &|i| roots.contains(&i),
        false,
    );
    let at = |k: usize| {
        format!(
            "{} ({}:{})",
            funcs[k].name,
            funcs[k].file.display(),
            funcs[k].line
        )
    };
    let reached: Vec<reach::Reached> = parent
        .keys()
        .map(|&i| {
            let mut chain = vec![at(i)];
            let mut up = parent[&i];
            while let Some(p) = up {
                chain.push(at(p));
                up = parent[&p];
            }
            chain.reverse();
            reach::Reached {
                file: fns[i].file.display().to_string(),
                line: fns[i].line,
                key: reach::key(&funcs[i].owner, &fns[i].name),
                checked: matches!(fns[i].class, Class::Verified | Class::Discharged),
                chain,
            }
        })
        .collect();
    let head = reach::rows(&text);
    let mut report = reach::check(roots.len(), &reached, &head, list);
    let mac = r.exports_macro.as_deref().unwrap_or("exports");
    report.failures.extend(
        unread
            .into_iter()
            .map(|e| format!("cannot read the {mac}! blocks of {e}")),
    );
    report
        .failures
        .extend(unmatched_exports(fns, &crates, &exported, mac));
    report
        .failures
        .extend(reach::check_seams(&head, seams.0, list, seams.1));
    if let Some(b) = base {
        let rev = b.rev.as_str();
        match b.copy(root, list, GateFile::Reach) {
            Ok((_, Some(base))) => {
                report
                    .failures
                    .extend(reach::check_added(&head, &reach::rows(&base), rev, list))
            }
            Ok((path, None)) => println!("ok entry reach: {rev} has no {path} to compare with"),
            Err(e) => report.failures.push(e),
        }
    }
    for f in &report.failures {
        eprintln!("FAIL entry reach: {f}");
    }
    // The trusted functions of the walked crates no root reaches: the
    // rest of FLOOR's trusted count beside REACH's trusted rows.
    let unreached: Vec<&FnRef> = fns
        .iter()
        .enumerate()
        .filter(|(i, f)| {
            f.class == Class::Trusted
                && crates.contains(&f.krate.as_str())
                && !parent.contains_key(i)
        })
        .map(|(_, f)| f)
        .collect();
    if report.failures.is_empty() {
        println!(
            "ok entry reach: {} functions reachable from {} C entry points; {} verified or discharged, {} in {list}; {} trusted functions no entry point reaches",
            report.reached,
            report.roots,
            report.checked,
            report.listed,
            unreached.len()
        );
        for f in &unreached {
            println!(
                "  unreached trusted: {}:{} {}",
                f.file.display(),
                f.line,
                reach::key(&inputs::base(&f.owner), &f.name)
            );
        }
    }
    report.failures.is_empty()
}

/// The token-mint gate (`[mints]`, [`mints`]): reads every crate's
/// sources (`crates`, name and directory) and the MINTS file under
/// `root`, holds them to `names` and the seam IDs `seams`, and prints the
/// failures or the ok line. Fails closed: a missing MINTS file, a file it
/// cannot read or parse, or a row it cannot read fails it. Whether it
/// passes.
fn token_mints(
    root: &Path,
    names: mints::Names,
    crates: &[Krate],
    seams: &std::collections::BTreeSet<String>,
) -> bool {
    let list = names.list.clone();
    let mint = names.mint.clone();
    let text = match fs::read_to_string(root.join(&list)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FAIL token mints: {list}: {e}");
            return false;
        }
    };
    let (rows, mut fails) = mints::rows(&text, &list);
    let mut scan = mints::Scan::new(names);
    for k in crates {
        let mut files = Vec::new();
        rust_files(&k.dir.join("src"), &mut files);
        for f in files {
            let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
            match fs::read_to_string(&f) {
                Ok(text) => mints::scan_file(&rel, &text, &mut scan),
                Err(e) => fails.push(format!("{rel}: {e}")),
            }
        }
    }
    fails.extend(mints::check(&scan, &rows, seams));
    for f in &fails {
        eprintln!("FAIL token mints: {f}");
    }
    if fails.is_empty() {
        println!(
            "ok token mints: {} token kinds and {} mints, each a {list} row; {mint}, in {}, is the one place a C caller's token is made",
            scan.kinds.len(),
            scan.mints.len(),
            scan.defs.first().map(|(f, _)| f.as_str()).unwrap_or("?")
        );
    }
    fails.is_empty()
}

/// Whether RN1.md documents the verified recursive function `f`: a table
/// row that names its file in backquotes and quotes `fn NAME`.
fn documented(rn1: &str, f: &FnRef) -> bool {
    let file = format!("`{}`", f.file.display());
    let quote = format!("`fn {}`", f.name);
    rn1.lines()
        .any(|l| l.starts_with('|') && l.contains(&file) && l.contains(&quote))
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

    /// The `cfg` atoms of pcre3's configuration, which the fixtures use:
    /// `test`, `kani` and the `test-util` feature test-only,
    /// `verus_keep_ghost` and the `verus` feature Verus-only, the `jit`
    /// feature sure in Verus's build and `miri` never set there.
    fn atoms() -> Atoms {
        Atoms {
            test: vec!["test".into(), "kani".into(), "feature=\"test-util\"".into()],
            verus: vec!["verus_keep_ghost".into(), "feature=\"verus\"".into()],
            sets: vec!["feature=\"jit\"".into()],
            never: vec!["miri".into()],
        }
    }

    fn test_util() -> Vec<String> {
        vec!["test-util".into()]
    }

    /// A counter as pcre3's configuration sets one up, with its `fn_ptr`
    /// macro.
    fn counter() -> Counter {
        Counter::new(false, atoms(), Some("fn_ptr".into()))
    }

    /// The tally of one crate whose only file is `src`.
    fn count(src: &str) -> (Tally, usize) {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        c.items(&parse_file(src).expect("parses").items, false);
        (c.tallies.remove("k").unwrap_or_default(), c.errors)
    }

    /// The guard behind leaving `test-util` items out, through cargo's own
    /// resolution: a workspace member, shipped or not, that enables the
    /// feature outside its dev-dependencies fails the run, whatever the
    /// manifest's TOML form; one whose tests do does not.
    #[test]
    fn test_util_only_in_dev_dependencies() {
        const ROOT: &str =
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/lib\", \"crates/data\"]\n";
        const LIB: &str = "[package]\nname = \"lib\"\nedition = \"2024\"\n\n[features]\ntest-util = []\njit = []\n";
        const DATA: &str = "crates/data/Cargo.toml";
        let data = |body: &str| {
            format!("[package]\nname = \"data\"\nedition = \"2024\"\npublish = false\n\n{body}")
        };
        let rows: Vec<(Vec<(&str, String)>, bool)> = vec![
            // The tests' own sections, a member's own `test-util` feature that
            // forwards it, and a comment.
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\" }\n\n[dev-dependencies]\nlib = { path = \"../lib\", features = [\"test-util\"] }\n",
                    ),
                )],
                false,
            ),
            (
                vec![(DATA, data("[dev-dependencies.lib]\npath = '../lib'\nfeatures = ['test-util']\n"))],
                false,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[target.'cfg(unix)'.dev-dependencies]\nlib = { path = \"../lib\", features = [\"test-util\"] }\n",
                    ),
                )],
                false,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\" }\n\n[features]\ntest-util = [\"lib/test-util\"]\n",
                    ),
                )],
                false,
            ),
            (
                vec![(DATA, data("[dependencies]\nlib = { path = \"../lib\" } # not \"test-util\"\n"))],
                false,
            ),
            // Normal and build dependencies, in every form.
            (
                vec![(
                    DATA,
                    data("[dependencies]\nlib = { path = \"../lib\", features = [\"test-util\"] }\n"),
                )],
                true,
            ),
            (
                vec![(DATA, data("[dependencies.lib]\npath = \"../lib\"\nfeatures = [\"test-util\"]\n"))],
                true,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[target.'cfg(windows)'.dependencies]\nlib = { path = \"../lib\", features = [\"test-util\"] }\n",
                    ),
                )],
                true,
            ),
            (
                vec![(
                    DATA,
                    data("[build-dependencies]\nlib = { path = \"../lib\", features = [\"test-util\"] }\n"),
                )],
                true,
            ),
            (
                vec![
                    (
                        "Cargo.toml",
                        format!(
                            "{ROOT}\n[workspace.dependencies]\nlib = {{ path = \"crates/lib\", features = [\"test-util\"] }}\n"
                        ),
                    ),
                    (DATA, data("[dependencies]\nlib.workspace = true\n")),
                ],
                true,
            ),
            // Features that forward it, on by default or not (found in
            // review: pcre3-capi's `hardened`, which docs/design.md, ci/abi.sh
            // and ci/consumers.sh build with, is not on by default), or name it
            // by default.
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\" }\n\n[features]\nx = ['lib/test-util']\n",
                    ),
                )],
                true,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\" }\n\n[features]\nhardened = [\"lib/jit\", \"lib/test-util\"]\n",
                    ),
                )],
                true,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\", optional = true }\n\n[features]\nx = [\"dep:lib\", \"lib?/test-util\"]\ndefault = [\"x\"]\n",
                    ),
                )],
                true,
            ),
            (
                vec![
                    ("crates/lib/Cargo.toml", format!("{LIB}default = [\n    \"jit\",\n    \"test-util\",\n]\n")),
                    (DATA, data("[dependencies]\nlib = { path = \"../lib\" }\n")),
                ],
                true,
            ),
            // The forms a manifest scanner missed (found in review): single
            // quotes, a `#` inside them, and a globbed member.
            (
                vec![(
                    DATA,
                    data("[dependencies]\nlib = { path = '../lib', features = ['test-util'] }\n"),
                )],
                true,
            ),
            (
                vec![(
                    DATA,
                    data(
                        "[dependencies]\nlib = { path = \"../lib\" }\n\n[features]\ndefault = ['lib/test-util']\n",
                    ),
                )],
                true,
            ),
            (
                vec![
                    (
                        "Cargo.toml",
                        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/l#b\", \"crates/data\"]\n".to_string(),
                    ),
                    ("crates/l#b/Cargo.toml", LIB.to_string()),
                    (
                        DATA,
                        data("[dependencies]\nlib = { path = '../l#b', features = [\"test-util\"] }\n"),
                    ),
                ],
                true,
            ),
            (
                vec![
                    (
                        "Cargo.toml",
                        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/lib\", \"extra/*\"]\n".to_string(),
                    ),
                    (
                        "extra/data/Cargo.toml",
                        data("[dependencies]\nlib = { path = \"../../crates/lib\", features = [\"test-util\"] }\n"),
                    ),
                ],
                true,
            ),
        ];
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        for (i, (files, want)) in rows.into_iter().enumerate() {
            let root = std::env::temp_dir()
                .join(format!("verus-count-test-util-{}-{i}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let base = [
                ("Cargo.toml", ROOT.to_string()),
                ("crates/lib/Cargo.toml", LIB.to_string()),
                (DATA, data("")),
            ];
            for (p, t) in base.into_iter().chain(files.iter().cloned()) {
                let f = root.join(p);
                fs::create_dir_all(f.parent().unwrap().join("src")).unwrap();
                fs::write(f.parent().unwrap().join("src/lib.rs"), "").unwrap();
                fs::write(&f, t).unwrap();
            }
            let lock = std::process::Command::new(&cargo)
                .args(["generate-lockfile", "--offline"])
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                lock.status.success(),
                "{files:?}: {}",
                String::from_utf8_lossy(&lock.stderr)
            );
            let got: Vec<String> = test_util_enablers(&root, &test_util())
                .unwrap()
                .iter()
                .map(|(p, f)| {
                    assert_eq!(f, "test-util");
                    p.split(' ').next().unwrap_or_default().to_string()
                })
                .collect();
            let wanted: Vec<String> = if want { vec!["lib".into()] } else { Vec::new() };
            assert_eq!(got, wanted, "{files:?}: got = {got:?}, wanted = {wanted:?}");
            let _ = fs::remove_dir_all(&root);
        }
    }

    /// A module declared for tests alone is left out with its file, one
    /// declared for Verus alone is read for its rlimits only, and one whose
    /// `cfg` Verus's build might not meet ships unsure, whichever of the
    /// forms the cfg takes; a module a shipped build compiles, and Verus's
    /// is sure to, is none of them.
    #[test]
    fn test_and_verification_modules_are_left_out() {
        let root = std::env::temp_dir().join(format!("verus-count-modules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        let lib = "#[cfg(any(test, verus_keep_ghost))]\nmod grouped;\n#[cfg(verus_keep_ghost)]\nmod layout;\n#[cfg(test)]\nmod tests;\nmod shipped;\n#[cfg(any(test, unix))]\nmod either;\n#[cfg(all(feature = \"jit\", not(miri)))]\nmod settled;\n";
        fs::write(root.join("src/lib.rs"), lib).unwrap();
        for m in ["grouped", "layout", "tests", "shipped", "either", "settled"] {
            fs::write(root.join(format!("src/{m}.rs")), "").unwrap();
        }
        // A test-only module inside an unsure one is test-only.
        fs::create_dir_all(root.join("src/either")).unwrap();
        fs::write(root.join("src/either.rs"), "#[cfg(test)]\nmod t;\n").unwrap();
        fs::write(root.join("src/either/t.rs"), "").unwrap();
        let file = parse_file(lib).unwrap();
        let mut mods = ModFiles {
            roots: crate_roots(&root),
            ..ModFiles::default()
        };
        let mut errors = Vec::new();
        test_module_files(
            &root.join("src/lib.rs"),
            &file,
            &atoms(),
            &mut mods,
            &mut errors,
        );
        test_module_files(
            &root.join("src/either.rs"),
            &parse_file("#[cfg(test)]\nmod t;\n").unwrap(),
            &atoms(),
            &mut mods,
            &mut errors,
        );
        assert!(errors.is_empty(), "errors: got = {errors:?}, wanted = []");
        // A file a test-, Verus-only or unsure module declares, at any
        // depth, inherits its class, beside a `foo.rs` (`foo/bar.rs`) as
        // beside a `mod.rs`.
        for sub in [
            "layout/deep.rs",
            "layout/deep/deeper.rs",
            "tests/inner.rs",
            "shipped/x.rs",
            "layout_other.rs",
            "either/x.rs",
        ] {
            let p = root.join("src").join(sub);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "").unwrap();
        }
        for (sub, want) in [
            ("layout.rs", Built::Verus),
            ("layout/deep.rs", Built::Verus),
            ("layout/deep/deeper.rs", Built::Verus),
            ("grouped.rs", Built::Verus),
            ("tests.rs", Built::Test),
            ("tests/inner.rs", Built::Test),
            ("shipped.rs", Built::Shipped),
            // Files no declaration names: unsure, under the guards of
            // the modules whose directories hold them.
            ("shipped/x.rs", Built::Unsure),
            ("layout_other.rs", Built::Unsure),
            ("lib.rs", Built::Shipped),
            ("settled.rs", Built::Shipped),
            ("either.rs", Built::Unsure),
            ("either/x.rs", Built::Unsure),
            ("either/t.rs", Built::Test),
        ] {
            let f = fs::canonicalize(root.join("src").join(sub)).unwrap();
            let got = module_class(&f, &mods, &atoms());
            assert_eq!(got, Ok(want), "{sub}: got = {got:?}, wanted = {want:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// A workspace cargo cannot resolve (here, no lockfile under
    /// `--locked`) is an error, not a pass.
    #[test]
    fn test_util_check_fails_without_resolution() {
        let root =
            std::env::temp_dir().join(format!("verus-count-unresolved-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"lib\"\nedition = \"2024\"\n\n[workspace]\n",
        )
        .unwrap();
        assert!(test_util_enablers(&root, &test_util()).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// A misspelt option fails instead of becoming ROOT, so a run cannot
    /// pass with a gate quietly left off.
    #[test]
    fn unknown_options_and_a_second_root_fail() {
        let parse = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
        assert_eq!(
            parse(&[
                "--floor",
                "verus/FLOOR",
                "--base-rev",
                "HEAD^",
                "--rlimit",
                "s.json",
                "--list",
                "r"
            ]),
            Ok(Args {
                list: true,
                floor: Some("verus/FLOOR".into()),
                base_rev: Some("HEAD^".into()),
                stats: Some("s.json".into()),
                root: Some("r".into()),
                ..Args::default()
            })
        );
        assert_eq!(parse(&[]), Ok(Args::default()));
        assert_eq!(
            parse(&["--floor", "verus/FLOOR", "--no-base"]),
            Ok(Args {
                floor: Some("verus/FLOOR".into()),
                no_base: true,
                ..Args::default()
            })
        );
        assert_eq!(
            parse(&["--badge", "--config", "c.toml", "r"]),
            Ok(Args {
                badge: true,
                config_file: Some("c.toml".into()),
                root: Some("r".into()),
                ..Args::default()
            })
        );
        for (args, want) in [
            (
                &["--no-base", "--base-rev", "HEAD^"][..],
                "--no-base with --base-rev",
            ),
            (&["--flor", "verus/FLOOR", "."][..], "unknown option --flor"),
            (&["-l", "."][..], "unknown option -l"),
            (&["verus/FLOOR", "."][..], "a second ROOT ."),
            (&["--floor"][..], "--floor needs a value"),
            (&["--badge", "--floor", "verus/FLOOR"][..], "--badge prints"),
            (&["--badge", "--base-rev", "HEAD"][..], "--badge prints"),
            (&["--badge", "--no-base"][..], "--badge prints"),
            (&["--list", "--badge"][..], "--badge prints"),
            (&["--badge", "--rlimit", "s.json"][..], "--badge prints"),
        ] {
            let got = parse(args).unwrap_err();
            assert!(
                got.contains(want),
                "{args:?}: got = {got:?}, wanted = {want:?}"
            );
        }
    }

    /// The badge line is challoc's form with FLOOR's figures, one decimal
    /// each, in the alt text and the URL; a file passes only with that
    /// line, once. Negative: no badge, the alt text or the URL alone off
    /// by a tenth, the line with other text on it, two badges (even two
    /// canonical lines, or a second in any case or form), and the
    /// canonical line only where it does not render (a fence or an HTML
    /// comment) each fail, naming the line to write. A badge that does not
    /// render is not a second one.
    #[test]
    fn the_badge_gives_floors_figures_once() {
        let floor = Floor([Some(863), Some(944), Some(900), Some(950)]);
        let want = badge_line(&floor);
        assert_eq!(
            want,
            "[![Verus: 86.3% of functions, 94.4% of lines](https://img.shields.io/badge/Verus-86.3%25%20of%20functions%2C%2094.4%25%20of%20lines-blue)](verus/README.md#coverage)"
        );
        assert_eq!(
            badge_line(&Floor([Some(1000), Some(0), None, None])),
            "[![Verus: 100.0% of functions, 0.0% of lines](https://img.shields.io/badge/Verus-100.0%25%20of%20functions%2C%200.0%25%20of%20lines-blue)](verus/README.md#coverage)"
        );
        let readme = |badge: &str| format!("# port\n\n{badge}\n\nText.\n");
        assert_eq!(
            badge_report(&readme(&want), "README.md", "verus/FLOOR", &want),
            Ok("ok badge: README.md:3 gives verus/FLOOR's functions and lines figures".to_owned())
        );
        let alt = want.replacen("86.3% of functions", "86.2% of functions", 1);
        // Another Verus badge where it does not render is not a second one.
        for hidden in [
            format!("```markdown\n{alt}\n```"),
            format!("~~~\n{alt}\n~~~"),
            format!("<!--\n{alt}\n-->"),
            format!("<!-- {alt} -->"),
        ] {
            assert_eq!(
                badge_report(
                    &readme(&format!("{want}\n{hidden}")),
                    "README.md",
                    "verus/FLOOR",
                    &want
                ),
                Ok(
                    "ok badge: README.md:3 gives verus/FLOOR's functions and lines figures"
                        .to_owned()
                ),
                "{hidden:?}"
            );
        }
        let url = want.replacen("86.3%25", "86.2%25", 1);
        let alt_lines = want.replacen("94.4% of lines", "94.1% of lines", 1);
        let url_lines = want.replacen("94.4%25", "94.1%25", 1);
        for (text, wanted) in [
            (
                "# port\n\nText.\n".to_owned(),
                "FAIL badge: README.md has no Verus badge; add this line, which gives verus/FLOOR's functions and lines figures:\n",
            ),
            (
                readme(&alt),
                "FAIL badge: README.md:3 is not the badge verus/FLOOR's functions and lines figures give; replace it with this line:\n",
            ),
            (readme(&url), "FAIL badge: README.md:3 is not the badge"),
            (
                readme(&alt_lines),
                "FAIL badge: README.md:3 is not the badge",
            ),
            (
                readme(&url_lines),
                "FAIL badge: README.md:3 is not the badge",
            ),
            (
                readme(&format!("{want} [![CI](x)](y)")),
                "FAIL badge: README.md:3 is not the badge",
            ),
            (
                readme(&format!("{want}\n{want}")),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4); keep one, this line:\n",
            ),
            (
                readme(&format!(
                    "{want}\n![Verus](https://img.shields.io/badge/Verus-1%25-blue)"
                )),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            (
                readme(&format!(
                    "{want}\n[![Verus: 90%](https://example.com/v.svg)](x)"
                )),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            // A Verus badge in any case: a lowercase label, and the
            // `static/v1` endpoint with no `[` before the `![`.
            (
                readme(&format!(
                    "{want}\n![verus](https://img.shields.io/badge/verus-99%25%20verified-green)"
                )),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            (
                readme(&format!(
                    "{want}\n![Verus](https://img.shields.io/static/v1?label=Verus&message=99%25&color=blue)"
                )),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            (
                readme(&format!("{want}\n[![VERUS](x.svg)](y)")),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            (
                readme(&format!(
                    "{want}\n<img src=\"https://img.shields.io/badge/verus-1%25-blue\">"
                )),
                "FAIL badge: README.md has 2 Verus badges (lines 3, 4)",
            ),
            // The canonical line where it does not render: inside a
            // multi-line HTML comment, a ``` fence, a ~~~ fence, and an
            // inline comment; and with a stale badge rendered outside.
            (
                readme(&format!("<!--\n{want}\n-->")),
                "FAIL badge: README.md has no Verus badge",
            ),
            (
                readme(&format!("```markdown\n{want}\n```")),
                "FAIL badge: README.md has no Verus badge",
            ),
            (
                readme(&format!("~~~\n{want}\n~~~")),
                "FAIL badge: README.md has no Verus badge",
            ),
            (
                readme(&format!("<!-- {want} -->")),
                "FAIL badge: README.md has no Verus badge",
            ),
            (
                readme(&format!("<!--\n{want}\n-->\n{alt}")),
                "FAIL badge: README.md:6 is not the badge",
            ),
            (
                readme(&format!("```\n{want}\n```\n{alt}")),
                "FAIL badge: README.md:6 is not the badge",
            ),
            // A fence only closes on a line of its own character, at least
            // as long, so a badge after "````" inside a ~~~ fence, or after
            // "```" inside a ```` fence, stays inside it.
            (
                readme(&format!("~~~\n````\n{want}\n~~~")),
                "FAIL badge: README.md has no Verus badge",
            ),
            (
                readme(&format!("````\n```\n{want}\n````")),
                "FAIL badge: README.md has no Verus badge",
            ),
            // A comment ends at `-->`, and text after it renders; inline
            // code with three backticks is not a fence.
            (
                readme(&format!("<!-- note --> {want}")),
                "FAIL badge: README.md:3 is not the badge",
            ),
            (
                readme(&format!("```x``` {want}")),
                "FAIL badge: README.md:3 is not the badge",
            ),
        ] {
            let got = badge_report(&text, "README.md", "verus/FLOOR", &want);
            assert!(
                got.as_ref()
                    .is_err_and(|e| e.contains(wanted) && e.ends_with(&want)),
                "{text:?}: got = {got:?}, wanted = Err({wanted:?} ... {want:?})"
            );
        }
    }

    #[test]
    fn cfg_predicates_that_hold_only_under_test() {
        for (cfg, want) in [
            ("test", true),
            ("all (test , feature = \"x\")", true),
            ("all (feature = \"x\" , test)", true),
            ("not (test)", false),
            ("any (test , feature = \"x\")", false),
            ("feature = \"test-util\"", true),
            ("feature = \"other-util\"", false),
            ("any (test , feature = \"test-util\")", true),
            ("any (feature = \"test-util\" , unix)", false),
            ("all (feature = \"test-util\" , unix)", true),
            ("not (feature = \"test-util\")", false),
            ("all (not (test) , unix)", false),
            ("kani", true),
            ("all (kani , feature = \"x\")", true),
            ("not (kani)", false),
            ("any (kani , test)", true),
            // `any()` holds nowhere: no build compiles it.
            ("any ()", true),
            ("verus_keep_ghost", true),
            ("any (test , verus_keep_ghost)", true),
            ("all (verus_keep_ghost , unix)", true),
            ("not (verus_keep_ghost)", false),
            ("any (verus_keep_ghost , unix)", false),
            // Nested predicates, read to any depth.
            ("any (test , all (test , unix))", true),
            ("any (test , all (verus_keep_ghost , unix))", true),
            ("any (test , all (not (test) , unix))", false),
            ("not (any (test , kani))", false),
            ("not (all (unix , not (test)))", false),
            (
                "all (feature = \"jit\" , not (miri) , any (test , feature = \"x86-jit\"))",
                false,
            ),
            ("all (feature = \"a,b\" , test)", true),
            // A configured Verus-only feature, as `verus_keep_ghost`.
            ("feature = \"verus\"", true),
            ("any (feature = \"verus\" , unix)", false),
        ] {
            assert_eq!(
                cfg_needs_test(cfg, &atoms()),
                want,
                "{cfg}: got = {}, wanted = {want}",
                !want
            );
        }
        // The atoms come from the configuration: with none configured but
        // the defaults, `test-util` and `verus` are ordinary features, and
        // `feature = "verus"` builds where Verus's does.
        let plain = Atoms::of(&Config::parse("[crates]\ndir = \"crates\"\n").expect("parses"));
        assert!(!cfg_needs_test("feature = \"test-util\"", &plain));
        assert!(!cfg_needs_test("feature = \"verus\"", &plain));
        assert_eq!(cfg_built("feature = \"verus\"", &atoms()), Ok(Built::Verus));
        assert_eq!(cfg_built("kani", &plain), Ok(Built::Test));
    }

    /// A `cfg` predicate the counter cannot read is an error, not a guess:
    /// unbalanced, an empty argument, `not` of other than one.
    #[test]
    fn unreadable_cfg_predicates_are_errors() {
        for cfg in [
            "all (test , any (kani)",
            "any (test) )",
            "all (test , , unix)",
            "not (test , kani)",
            "",
        ] {
            assert!(
                cfg_built(cfg, &atoms()).is_err(),
                "{cfg:?}: got = Ok, wanted = an error"
            );
        }
        // Where a nested predicate lets an item be built.
        for (cfg, want) in [
            (
                "any (target_arch = \"aarch64\" , all (target_os = \"linux\" , not (miri)))",
                Built::Unsure,
            ),
            ("any (test , all (verus_keep_ghost , unix))", Built::Verus),
            ("all (test , any (verus_keep_ghost , unix))", Built::Test),
        ] {
            let got = cfg_built(cfg, &atoms());
            assert_eq!(got, Ok(want), "{cfg}: got = {got:?}, wanted = {want:?}");
        }
    }

    /// A `cfg` a shipped build can meet is sure in Verus's build only
    /// when the configuration settles it there: a `verus_sets` atom, `not`
    /// of a `verus_never_sets` one, or a predicate those, the Verus atoms
    /// and the test atoms decide. Anything else Verus's build might not
    /// meet, the issue's `target_os = "windows"` included.
    #[test]
    fn only_settled_atoms_are_sure_in_verus_s_build() {
        for (cfg, want) in [
            ("feature = \"jit\"", Built::Shipped),
            ("not (miri)", Built::Shipped),
            ("all (feature = \"jit\" , not (miri))", Built::Shipped),
            ("any (unix , feature = \"jit\")", Built::Shipped),
            ("not (test)", Built::Shipped),
            ("not (verus_keep_ghost)", Built::Unsure),
            ("any (verus_keep_ghost , unix)", Built::Shipped),
            ("target_os = \"windows\"", Built::Unsure),
            ("unix", Built::Unsure),
            ("miri", Built::Unsure),
            ("not (feature = \"jit\")", Built::Unsure),
            ("all (feature = \"jit\" , unix)", Built::Unsure),
            ("feature = \"x86-jit\"", Built::Unsure),
            ("any (miri , test)", Built::Unsure),
        ] {
            let got = cfg_built(cfg, &atoms());
            assert_eq!(got, Ok(want), "{cfg}: got = {got:?}, wanted = {want:?}");
        }
        // Several `cfg`s that must hold together: the narrowest class.
        let (shipped, unsure, verus, test) =
            ("feature = \"jit\"", "unix", "verus_keep_ghost", "test");
        for (a, b, want) in [
            (shipped, unsure, Built::Unsure),
            (unsure, verus, Built::Verus),
            (unsure, test, Built::Test),
            (shipped, shipped, Built::Shipped),
        ] {
            for (x, y) in [(a, b), (b, a)] {
                let got = guarded_built(&[x.to_owned(), y.to_owned()], &atoms());
                assert_eq!(
                    got,
                    Ok(want),
                    "{x} and {y}: got = {got:?}, wanted = {want:?}"
                );
            }
        }
        // Two `cfg`s, each needing its own value of one key, hold in no
        // build together, as their `all` does.
        let arches = Atoms {
            sets: vec![
                "target_arch=\"x86_64\"".to_owned(),
                "target_arch=\"aarch64\"".to_owned(),
            ],
            ..atoms()
        };
        let got = guarded_built(
            &[
                "target_arch = \"x86_64\"".to_owned(),
                "target_arch = \"aarch64\"".to_owned(),
            ],
            &arches,
        );
        assert_eq!(
            got,
            Ok(Built::Test),
            "two architectures' cfgs: got = {got:?}, wanted = Test"
        );
        // One key, two values: no build meets it, though each atom on its
        // own is sure where both are declared (challoc's two runs). Keys
        // rustc gives several values (`target_family`, `feature`) and a
        // `not` of the impossible conjunction are not that.
        let both = Atoms {
            sets: vec![
                "target_arch=\"x86_64\"".into(),
                "target_arch=\"aarch64\"".into(),
            ],
            ..atoms()
        };
        for (cfg, want) in [
            (
                "all (target_arch = \"x86_64\" , target_arch = \"aarch64\")",
                Built::Test,
            ),
            (
                "all (target_arch = \"x86_64\" , any (target_arch = \"aarch64\" , test))",
                Built::Test,
            ),
            (
                "any (all (target_arch = \"x86_64\" , target_arch = \"aarch64\") , kani)",
                Built::Test,
            ),
            (
                "all (target_os = \"linux\" , all (unix , target_os = \"macos\"))",
                Built::Test,
            ),
            ("all (panic = \"abort\" , panic = \"unwind\")", Built::Test),
            (
                "all (target_pointer_width = \"32\" , target_pointer_width = \"64\")",
                Built::Test,
            ),
            (
                "not (all (target_arch = \"x86_64\" , target_arch = \"aarch64\"))",
                Built::Shipped,
            ),
            (
                "all (target_family = \"unix\" , target_family = \"wasm\")",
                Built::Unsure,
            ),
            ("all (feature = \"a\" , feature = \"b\")", Built::Unsure),
            ("target_arch = \"x86_64\"", Built::Shipped),
            // Each atom is read on its own: the aarch64 run checks this,
            // but the counter cannot tell the runs apart.
            ("not (target_arch = \"x86_64\")", Built::Unsure),
        ] {
            let got = cfg_built(cfg, &both);
            assert_eq!(got, Ok(want), "{cfg}: got = {got:?}, wanted = {want:?}");
        }
        // Without the declarations, `jit` and `miri` are unsettled.
        let plain = Atoms::of(&Config::parse("[crates]\ndir = \"crates\"\n").expect("parses"));
        for cfg in ["feature = \"jit\"", "not (miri)"] {
            let got = cfg_built(cfg, &plain);
            assert_eq!(
                got,
                Ok(Built::Unsure),
                "{cfg}: got = {got:?}, wanted = Unsure"
            );
        }
    }

    /// The functions [`recursive`] finds recursing in one file, `src`, that
    /// Verus does not check (`verified` false), or that it does (true).
    fn recursing(src: &str, verified: bool) -> Vec<String> {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        c.items(&parse_file(src).expect("parses").items, false);
        recursive(&c.fns, &[])
            .into_iter()
            .filter(|(i, _)| (c.fns[*i].class == Class::Verified) == verified)
            .map(|(i, path)| format!("{}: {}", c.fns[i].name, path.join(" -> ")))
            .collect()
    }

    #[test]
    fn recursion_outside_verus_is_reported() {
        // Positive: direct, through `Self::` and `self.`, and mutual.
        assert_eq!(
            recursing("fn f(n: u32) { if n > 0 { f(n - 1) } }", false),
            ["f: f -> f"]
        );
        assert_eq!(
            recursing(
                "struct S; impl S { fn g(&self) { Self::g(self) } fn h(&self) { self.h() } }",
                false
            ),
            ["g: g -> g", "h: h -> h"]
        );
        // With a turbofish, and through a nested function.
        assert_eq!(
            recursing(
                "struct S; impl S { fn t<const N: bool>(&self) { self.t::<N>() } \
                 fn u<T: Fn() -> u8>(&self) { Self::u::<T>(self) } } \
                 fn v<const N: bool>() { v::<N>() }",
                false
            ),
            ["t: t -> t", "u: u -> u", "v: v -> v"]
        );
        assert_eq!(
            recursing(
                "fn outer() { fn inner(n: u32) { inner(n) } inner(1) }",
                false
            ),
            ["inner: inner -> inner"]
        );
        // A nested function in an expression's block: an `if`'s, a
        // closure's, a `match` arm's, a block's, a loop's; and an `impl`'s
        // method in one.
        assert_eq!(
            recursing(
                "fn outer(c: bool) { if c { fn a(n: u32) { a(n) } a(1) } \
                 let _ = || { fn b(n: u32) { b(n) } }; \
                 match c { true => { fn d(n: u32) { d(n) } } _ => {} } \
                 { fn e(n: u32) { e(n) } } \
                 loop { fn g(n: u32) { g(n) } break; } \
                 if c { struct S; impl S { fn h(&self) { self.h() } } } }",
                false
            ),
            [
                "a: a -> a",
                "b: b -> b",
                "d: d -> d",
                "e: e -> e",
                "g: g -> g",
                "h: h -> h"
            ]
        );
        // A function in a module, a trait's default method, and one in a
        // `const` initializer, each written in a body.
        assert_eq!(
            recursing(
                "fn outer() { mod m { fn i(n: u32) { i(n) } } \
                 trait T { fn j(&self) { self.j() } } \
                 const _: () = { fn k(n: u32) { k(n) } }; }",
                false
            ),
            ["i: i -> i", "j: j -> j", "k: k -> k"]
        );
        // A nested function built only for tests ships nothing: by its own
        // `cfg`, a local module's, an `impl`'s, or an enclosing nested
        // function's. One built otherwise (`not(test)`, a feature) is still
        // reported, beside the gated ones.
        assert_eq!(
            recursing(
                "fn outer() { if true { #[cfg(test)] fn helper() { helper(); } } \
                 #[cfg(test)] mod t { fn in_mod() { in_mod(); } } \
                 struct S; #[cfg(test)] impl S { fn m(&self) { self.m() } } \
                 #[cfg(test)] fn wrap() { fn deep() { deep(); } } \
                 #[cfg(not(test))] fn shipped() { shipped(); } \
                 #[cfg(feature = \"x\")] mod f { fn featured() { featured(); } } \
                 { fn plain() { plain(); } } }",
                false
            ),
            [
                "shipped: shipped -> shipped",
                "featured: featured -> featured",
                "plain: plain -> plain"
            ]
        );
        assert_eq!(
            recursing(
                "fn a() { b() } fn b() { if x() { a() } } fn x() -> bool { true }",
                false
            ),
            ["a: a -> b -> a", "b: b -> a -> b"]
        );
        // Trusted bodies are Verus's blind spot too.
        assert_eq!(
            recursing(
                "verus! { #[verifier::external_body] fn t(n: u32) { t(n) } }",
                false
            ),
            ["t: t -> t"]
        );
        // Negative: a verified function (Verus checks its `decreases`), a
        // call through another path or receiver, a macro, a declaration.
        assert!(
            recursing(
                "verus! { fn v(n: u32) decreases n { if n > 0 { v(n - 1) } } }",
                false
            )
            .is_empty()
        );
        assert_eq!(
            recursing(
                "verus! { fn v(n: u32) decreases n { if n > 0 { v(n - 1) } } }",
                true
            ),
            ["v: v -> v"]
        );
        assert!(
            recursing(
                "struct S; impl S { fn new() -> Vec<u8> { Vec::new() } }",
                false
            )
            .is_empty()
        );
        assert!(
            recursing(
                "struct S { v: Vec<u8> } impl S { fn get(&self) -> Option<&u8> { self.v.get(0) } }",
                false
            )
            .is_empty()
        );
        assert!(recursing("fn p() { p!(1); fn q() {} }", false).is_empty());
    }

    /// The functions `src`, one file of crate `k`, records, by its walk.
    fn walked(src: &str) -> Counter {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("crates/k/src/lib.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        c.items(&parse_file(src).expect("parses").items, false);
        c.finish();
        c
    }

    /// The rules ([`breaks`]) over every kind of binding, for each form: a
    /// leading `::` is broken only by an `extern crate .. as` of its name;
    /// a bare `verus!` by a binding of a Verus name but the crate itself or
    /// the macro's own path, or a `#[macro_use]` of another crate; a bare
    /// `compile_error!` by any binding of the name, a `#[macro_use]` of
    /// another crate, or a `*` in its module (`k`), not in another (`k::s`).
    #[test]
    fn a_binding_breaks_only_its_forms_rule() {
        use Took::{StdBare as S, StdPath as SP, VerusBare as B, VerusPath as P};
        // Each row: the binding (`item NAME`, `use NAME <- PATH`, `extern
        // NAME <- CRATE`, `glob [s] PATH` (in `k::s` with `s`), `macro_use
        // CRATE`), the form, and whether it breaks the form's rule for
        // `vstd` (P), `verus` (B), `core` (SP) or `compile_error` (S).
        #[rustfmt::skip]
        let rows = [
            ("item vstd", P, false),
            ("use vstd <- other", P, false),
            ("glob other", P, false),
            ("macro_use other", P, false),
            ("extern vstd <- other", P, true),
            ("extern vstd <- vstd", P, false),
            ("extern core <- asmkit", SP, true),
            ("extern std <- asmkit", SP, false),
            ("item verus", B, true),
            ("item vstd", B, true),
            ("use vstd <- vstd", B, false),
            ("use verus <- vstd::prelude::verus", B, false),
            ("use verus <- other::verus", B, true),
            ("extern vstd <- other", B, true),
            ("glob other", B, false),
            ("macro_use other", B, true),
            ("macro_use vstd", B, false),
            ("macro_use std", B, false),
            ("item compile_error", S, true),
            ("use compile_error <- core::compile_error", S, true),
            ("use compile_error <- asmkit::compile_error", S, true),
            ("use core <- asmkit", S, false),
            ("glob core::ptr", S, true),
            ("glob vstd::prelude", S, true),
            ("glob s other", S, false),
            ("macro_use std", S, false),
            ("macro_use vstd", S, true),
        ];
        for (row, kind, want) in rows {
            let name = match kind {
                P => "vstd",
                B => "verus",
                SP => "core",
                S => "compile_error",
            };
            let (what, rest) = row.split_once(' ').expect("a row");
            let split = |p: &str| -> Vec<String> {
                p.trim_start_matches("::")
                    .split("::")
                    .map(str::to_owned)
                    .collect()
            };
            let k = vec!["k".to_owned()];
            let b = match what {
                "item" => Binding {
                    name: rest.into(),
                    module: k.clone(),
                    ..Binding::default()
                },
                "use" | "extern" => {
                    let (name, path) = rest.split_once(" <- ").expect("a use");
                    Binding {
                        name: name.into(),
                        path: split(path),
                        extern_crate: what == "extern",
                        module: k.clone(),
                        ..Binding::default()
                    }
                }
                "glob" => {
                    let (module, path) = match rest.strip_prefix("s ") {
                        Some(p) => (vec!["k".to_owned(), "s".to_owned()], p),
                        None => (k.clone(), rest),
                    };
                    Binding {
                        path: split(path),
                        glob: true,
                        module,
                        ..Binding::default()
                    }
                }
                _ => Binding {
                    name: rest.into(),
                    macro_use: true,
                    module: k.clone(),
                    ..Binding::default()
                },
            };
            let got = breaks(&b, kind, name, &k);
            assert!(
                got == want,
                "{row} (for {name}): got = {got}, wanted = {want}"
            );
        }
    }

    /// pcre3-capi's `width::fn_ptr!`, as width.rs defines it.
    const FN_PTR_RULE: &str = r#"
macro_rules! fn_ptr {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident $(<$p:ident>)? ($fn:ty);
    ) => {
        $(#[$attr])*
        #[repr(transparent)]
        #[derive(Debug)]
        $vis struct $name $(<$p>)? (pub(crate) $fn);

        impl $(<$p>)? Clone for $name $(<$p>)? {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl $(<$p>)? Copy for $name $(<$p>)? {}

        impl $(<$p>)? $crate::width::FromC<Option<$fn>> for Option<$name $(<$p>)?> {
            #[inline(always)]
            fn from_c(f: Option<$fn>) -> Self {
                f.map($name)
            }
        }
    };
}
"#;

    /// What the C entry points of `src`, one file of crate `k`, reach.
    fn entry_walked(src: &str) -> (Counter, Vec<String>) {
        let c = walked(src);
        let funcs: Vec<inputs::Func> = c
            .fns
            .iter()
            .map(|f| inputs::Func {
                krate: &f.krate,
                file: &f.file,
                line: f.line,
                owner: inputs::base(&f.owner),
                name: &f.name,
                params: f.params,
                receiver: f.receiver,
                body: &f.body,
            })
            .collect();
        let roots = entry_roots(
            &c.fns,
            &["k"],
            &std::collections::BTreeSet::new(),
            Some("from_c"),
        );
        let parent = inputs::walk(
            &funcs,
            &c.traits,
            &c.aliases,
            &["k"],
            &|i| roots.contains(&i),
            false,
        );
        let mut names: Vec<String> = parent.keys().map(|&i| c.fns[i].name.clone()).collect();
        names.sort();
        (c, names)
    }

    // A C callback argument's `fn_ptr!` conversion is expanded from the
    // macro's own definition, so a helper its body calls is reached from
    // the entry points, whichever file holds the definition. Negative: a
    // helper nothing calls is not.
    #[test]
    fn fn_ptr_bodies_come_from_its_definition() {
        let rule = FN_PTR_RULE.replace("f.map($name)", "c_helper(); f.map($name)");
        assert_ne!(rule, FN_PTR_RULE);
        let src = r#"
width::fn_ptr! {
    /// A callback.
    pub(crate) struct Callback(unsafe extern "C" fn(i32) -> i32);
}
fn c_helper() {}
fn unused_helper() {}
"#
        .to_string()
            + &rule;
        let (c, reached) = entry_walked(&src);
        assert_eq!(c.errors, 0);
        assert!(reached.contains(&"from_c".to_string()), "{reached:?}");
        assert!(reached.contains(&"c_helper".to_string()), "{reached:?}");
        assert!(
            !reached.contains(&"unused_helper".to_string()),
            "{reached:?}"
        );
    }

    // Negative: the expansion fails closed. An invocation with no
    // definition, a definition of two rules, and a transcriber that uses a
    // metavariable the expansion does not bind are each an error.
    #[test]
    fn fn_ptr_expansion_fails_closed() {
        let call = "width::fn_ptr! { pub(crate) struct P(unsafe extern \"C\" fn()); }\n";
        assert_eq!(walked(call).errors, 1);
        let two = FN_PTR_RULE.replace("    };\n}", "    };\n    () => {};\n}");
        assert_ne!(two, FN_PTR_RULE);
        assert_eq!(walked(&(call.to_string() + &two)).errors, 1);
        let unbound = FN_PTR_RULE.replace("f.map($name)", "f.map($other)");
        assert_eq!(walked(&(call.to_string() + &unbound)).errors, 1);
        assert_eq!(walked(&(call.to_string() + FN_PTR_RULE)).errors, 0);
    }

    // The entry points: functions that export their symbol (`no_mangle`,
    // `export_name`, inside `unsafe(..)` or not, with an `extern "C"`,
    // `"C-unwind"` or bare `extern` ABI), a function a `const` hands out,
    // an exported body, and the `FromC` conversions (the identity one and
    // `fn_ptr!`'s). Negative: an `extern "C"` function that exports no
    // symbol, a method named like the exported body, a helper,
    // `fn_ptr!`'s `Clone` and a three-argument `from_c` are not; nor is
    // anything of another crate.
    #[test]
    fn entry_roots_are_the_c_entry_points() {
        let src = FN_PTR_RULE.to_string()
            + r#"
#[unsafe(no_mangle)]
extern "C" fn c_entry() {}
#[no_mangle]
pub unsafe extern fn bare_entry() {}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn unwind_entry() {}
#[unsafe(export_name = "named_entry")]
extern "C" fn renamed_entry() {}
pub extern "C" fn not_exported() {}
unsafe extern "C-unwind" fn handed_out(n: usize) {}
const HANDED: P = P(handed_out);
unsafe fn body_a<U>() {}
struct S;
impl S { fn body_a(&self) {} }
impl Buf { fn from_c(p: usize, n: usize, t: usize) -> Self { Buf } }
fn helper() {}
impl<T> FromC<T> for T { fn from_c(t: T) -> T { t } }
width::fn_ptr! {
    /// A pointer.
    pub(crate) struct P(unsafe extern "C" fn());
}
"#;
        let c = walked(&src);
        assert_eq!(c.errors, 0);
        let exported: std::collections::BTreeSet<(PathBuf, String)> =
            [(PathBuf::from("crates/k/src/lib.rs"), "body_a".to_string())].into();
        let roots = |crates: &[&str], conversion: Option<&str>| {
            let mut got: Vec<String> = entry_roots(&c.fns, crates, &exported, conversion)
                .into_iter()
                .map(|i| format!("{}::{}", inputs::base(&c.fns[i].owner), c.fns[i].name))
                .collect();
            got.sort();
            got
        };
        let named = |crates: &[&str]| roots(crates, Some("from_c"));
        // With no conversion configured, the `from_c` methods root nothing.
        assert!(
            !roots(&["k"], None).iter().any(|r| r.ends_with("from_c")),
            "{:?}",
            roots(&["k"], None)
        );
        assert_eq!(
            named(&["k"]),
            [
                "::bare_entry",
                "::body_a",
                "::c_entry",
                "::handed_out",
                "::renamed_entry",
                "::unwind_entry",
                "Option::from_c",
                "T::from_c"
            ]
        );
        assert!(
            c.fns
                .iter()
                .any(|f| f.name == "clone" && f.owner.starts_with('P'))
        );
        assert_eq!(named(&["other"]), Vec::<String>::new());
    }

    // A function a `static` or `const` passes by name, bare or by path, is
    // an entry point. Negative: one it only calls is not, though a `<` or
    // a closure in the call's arguments leaves the arity unknown.
    #[test]
    fn a_static_hands_out_only_what_it_passes_by_name() {
        let c = walked(
            "fn passed() {}
fn pathed() {}
fn helper(x: u8) -> u8 { x }
fn run(f: u8) -> u8 { f }
fn make<A, B>() -> u8 { 0 }
static S: u8 = helper(make::<u8, u16>());
static R: u8 = run(|x| x);
static Q: P = P(passed);
const C: P = P(self::pathed);
",
        );
        assert_eq!(c.errors, 0, "errors: got = {}, wanted = 0", c.errors);
        let got: Vec<&str> = c
            .fns
            .iter()
            .filter(|f| f.entry)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            got,
            ["passed", "pathed"],
            "entry points: got = {got:?}, wanted = [passed, pathed]"
        );
    }

    // A configured root that matches a function of the configured crates
    // passes, with an empty table (a port's first CD1). Negative: a root
    // misspelled, moved to another file, of another owner, or in a crate
    // the walk does not follow fails, though the table has no rows for a
    // stale row to fail on.
    #[test]
    fn a_root_that_matches_nothing_fails() {
        let c = walked("struct R;\nimpl R { fn new() {} }\nfn other() {}\n");
        let root = |file: &str, owner: Option<&str>, name: &str| config::Root {
            file: file.into(),
            owner: owner.map(Into::into),
            name: name.into(),
        };
        let run = |roots: Vec<config::Root>, crates: &[&str]| {
            compile_inputs(
                Path::new("/nonexistent"),
                &config::Inputs {
                    table: "CD1.md".into(),
                    crates: crates.iter().map(|k| k.to_string()).collect(),
                    roots,
                    lazy: false,
                    describe: "the roots".into(),
                },
                &c,
            )
        };
        let good = || {
            vec![
                root("k/src/lib.rs", Some("R"), "new"),
                root("lib.rs", None, "other"),
            ]
        };
        assert!(
            run(good(), &["k"]),
            "matching roots: got = fail, wanted = ok"
        );
        for (bad, crates) in [
            (root("k/src/lib.rs", Some("R"), "nwe"), &["k"]),
            (root("k/src/moved.rs", Some("R"), "new"), &["k"]),
            (root("k/src/lib.rs", Some("S"), "new"), &["k"]),
            (root("k/src/lib.rs", None, "other"), &["other"]),
        ] {
            let mut roots = good();
            roots.push(bad);
            assert!(
                !run(roots, crates),
                "unmatched root: got = ok, wanted = fail"
            );
        }
    }

    // An exported body defined as a free function in the block's file
    // matches. Negative: one the file does not define (moved elsewhere and
    // imported), or defines only as a method, is an error.
    #[test]
    fn unmatched_exports_are_errors() {
        let c = walked("fn body_a<U>() {}\nstruct S;\nimpl S { fn body_c(&self) {} }\n");
        let file = PathBuf::from("crates/k/src/lib.rs");
        let exported: std::collections::BTreeSet<(PathBuf, String)> =
            ["body_a", "body_b", "body_c"]
                .into_iter()
                .map(|n| (file.clone(), n.to_string()))
                .collect();
        let got = unmatched_exports(&c.fns, &["k"], &exported, "exports");
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got[0].contains("an exports! block exports `body_b`"),
            "{got:?}"
        );
        assert!(got[1].contains("exports `body_c`"), "{got:?}");
        assert_eq!(
            unmatched_exports(&c.fns, &["other"], &exported, "exports").len(),
            3
        );
    }

    // Negative: a function that exports its symbol with an ABI C cannot
    // call is an error.
    #[test]
    fn exported_symbol_needs_a_c_abi() {
        assert_eq!(walked("#[no_mangle]\nextern \"C\" fn ok() {}\n").errors, 0);
        assert_eq!(
            walked("#[no_mangle]\nextern \"Rust\" fn bad() {}\n").errors,
            1
        );
        assert_eq!(walked("#[unsafe(no_mangle)]\nfn bad() {}\n").errors, 1);
    }

    // A method of an `impl` written inside a function body (a guard's
    // `Drop`, as `without_cancellation`'s) is recorded, with its owner and
    // the function's class, and the entry walk reaches it. Negative: such
    // a method in a function nothing calls is not reached.
    #[test]
    fn nested_impl_methods_are_walked() {
        let src = r#"
#[unsafe(no_mangle)]
extern "C" fn entry() { guarded() }
fn guarded() {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) { restore_state() }
    }
    let _r = Restore(true);
}
fn restore_state() {}
fn uncalled() {
    struct Other;
    impl Other {
        fn method_never_called(&self) {}
    }
}
"#;
        let (c, reached) = entry_walked(src);
        assert_eq!(c.errors, 0);
        let drop = c.fns.iter().find(|f| f.name == "drop");
        assert!(
            drop.is_some_and(|f| inputs::base(&f.owner) == "Restore" && f.class == Class::Outside),
            "{:?}",
            c.fns
                .iter()
                .map(|f| (&f.owner, &f.name))
                .collect::<Vec<_>>()
        );
        for name in ["guarded", "drop", "restore_state"] {
            assert!(reached.contains(&name.to_string()), "{name}: {reached:?}");
        }
        assert!(c.fns.iter().any(|f| f.name == "method_never_called"));
        assert!(
            !reached.contains(&"method_never_called".to_string()),
            "{reached:?}"
        );
    }

    #[test]
    fn verified_recursion_needs_a_row() {
        let f = FnRef {
            krate: "k".into(),
            file: PathBuf::from("crates/k/src/a.rs"),
            line: 1,
            owner: String::new(),
            name: "walk".into(),
            class: Class::Verified,
            calls: Vec::new(),
            body: inputs::Body::default(),
            params: 0,
            receiver: false,
            entry: false,
        };
        assert!(documented("| `fn walk` | `crates/k/src/a.rs` | 3 |", &f));
        // Negative: another file, another name, or not a table row.
        assert!(!documented("| `fn walk` | `crates/k/src/b.rs` | 3 |", &f));
        assert!(!documented("| `fn walker` | `crates/k/src/a.rs` | 3 |", &f));
        assert!(!documented("`fn walk` in `crates/k/src/a.rs`", &f));
    }

    /// The markers read through `cfg_attr` go through the one expander,
    /// [`applied_attrs`]. `discharged_by = "kani"` counts nested or beside
    /// other attributes, and only under predicates that hold in no build
    /// whatever is declared; under one some build may meet it fails
    /// closed. An `external_body` a `cfg_attr` applies counts whatever its
    /// predicate (the function is trusted), and prose in a doc comment is
    /// no attribute.
    #[test]
    fn markers_are_read_through_every_cfg_attr() {
        let attrs = |src: &str| {
            let f = parse_file(&format!("{src}\nfn f() {{}}\n")).unwrap();
            let Item::Fn(func) = &f.items[0] else {
                panic!("not a function: {src}")
            };
            func.attrs.clone()
        };
        for (src, want) in [
            ("#[cfg_attr(any(), discharged_by = \"kani\")]", true),
            (
                "#[cfg_attr(unix, cfg_attr(any(), discharged_by = \"kani\"))]",
                true,
            ),
            (
                "#[cfg_attr(any(), cfg_attr(windows, discharged_by = \"kani\"))]",
                true,
            ),
            ("#[cfg_attr(any(), inline, discharged_by = \"kani\")]", true),
            (
                "#[cfg_attr(all(target_arch = \"x86_64\", target_arch = \"aarch64\"), discharged_by = \"kani\")]",
                true,
            ),
            ("#[cfg_attr(windows, discharged_by = \"kani\")]", false),
            (
                "#[cfg_attr(unix, cfg_attr(windows, discharged_by = \"kani\"))]",
                false,
            ),
            ("#[cfg_attr(unix, inline, discharged_by = \"kani\")]", false),
            ("#[cfg_attr(test, discharged_by = \"kani\")]", false),
            ("#[cfg_attr(any(), discharged_by = \"other\")]", false),
        ] {
            let got = is_discharged(&attrs(src));
            assert_eq!(got, want, "discharged {src}: got = {got}, wanted = {want}");
        }
        for (src, want) in [
            ("#[verifier::external_body]", true),
            (
                "#[cfg_attr(unix, cfg_attr(feature = \"verus\", verifier::external_body))]",
                true,
            ),
            (
                "#[cfg_attr(windows, inline, verus_verify(external_body))]",
                true,
            ),
            ("/// Not `external_body`: it is verified.", false),
            ("#[verifier::external_fn_specification]", false),
        ] {
            let got = is_external(&attrs(src));
            assert_eq!(got, want, "external {src}: got = {got}, wanted = {want}");
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
                #[verifier::external_body]
                spec fn external_ghost() -> bool { true }
                proof fn lemma() {}
                axiom fn axiom() ensures true;
                #[verifier::external_body]
                proof fn external_lemma() {}
                proof fn admitted() { admit(); }
                fn mints() -> Tracked<u8> { Tracked::assume_new() }
                proof fn mints_in_proof() { let tracked t: u8 = Tracked::<u8>::assume_new().get(); }
                #[cfg(test)]
                fn test_only() {}
                #[cfg(kani)]
                fn kani_only() {}
                #[cfg_attr(any(), discharged_by = "kani")]
                #[verifier::external_body]
                fn discharged() {}
                #[cfg_attr(any(), discharged_by = "kani")]
                axiom fn discharged_axiom() ensures true;
                #[cfg_attr(any(), discharged_by = "kani")]
                proof fn discharged_lemma() {}
                #[cfg_attr(any(), discharged_by = "kani")]
                fn discharged_but_verified() {}
                #[cfg_attr(any(), discharged_by = "other")]
                #[verifier::external_body]
                fn other_tool() {}
                // Built only for Verus: checked, never shipped, so none
                // of these is counted, whatever holds it.
                #[cfg(verus_keep_ghost)]
                fn verus_only() {}
                #[cfg(verus_keep_ghost)]
                impl Machine { fn verus_only_method(&self) {} }
                #[cfg(verus_keep_ghost)]
                trait VerusOnly { fn verus_only_default(&self) {} }
                #[cfg(any(test, verus_keep_ghost))]
                mod verus_only_mod { fn verus_only_inner() {} }
                // Built only for Verus but trusted: a shipped proof can
                // rest on it, so each counts as trusted.
                #[cfg(verus_keep_ghost)]
                #[verifier::external_body]
                fn verus_only_external() {}
                #[cfg(verus_keep_ghost)]
                proof fn verus_only_admits() { admit(); }
                #[cfg(verus_keep_ghost)]
                mod verus_only_seams { #[verifier::external_body] fn inner_external() {} }
                // Built only for Verus and ghost but no trusted proof, as
                // shipped: none is counted.
                #[cfg(verus_keep_ghost)]
                #[verifier::external_body]
                spec fn verus_only_external_ghost() -> bool { true }
                #[cfg(verus_keep_ghost)]
                mod verus_only_ghosts { #[verifier::external_body] spec fn inner_ghost() -> bool { true } }
            }
            fn outside() {}
            #[cfg(not(test))]
            fn shipped_outside() {}
            #[cfg(test)]
            fn test_only_outside() {}
            #[cfg(test)]
            mod tests { fn helper() {} }
            #[cfg(kani)]
            mod proofs { #[kani::proof] fn harness() {} }
            "#,
        );
        assert_eq!(errors, 0, "errors: got = {errors}, wanted = 0");
        assert_eq!(
            t.fns,
            [2, 13, 2, 2],
            "verified, trusted, outside, discharged: got = {:?}, wanted = [2, 13, 2, 2]",
            t.fns
        );
        assert_eq!(t.ghost, 4, "ghost: got = {}, wanted = 4", t.ghost);
    }

    /// A marked function built only for Verus is discharged as a shipped
    /// one is, so it needs a Kani record too. The case is S6's usize fill
    /// axiom (`crates/pcre3-unicode/src/vspec.rs`) moved under
    /// `cfg(verus_keep_ghost)`, against `verus/seams/kani-s6.md` with and
    /// without its quote.
    #[test]
    fn a_verus_only_discharged_function_needs_a_record() {
        let file = "crates/pcre3-unicode/src/vspec.rs";
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from(file),
            module: vec!["k".into()],
            ..counter()
        };
        c.items(
            &parse_file(
                r#"
                verus! {
                pub uninterp spec fn fill_copies<T>() -> bool;
                #[cfg(verus_keep_ghost)]
                #[cfg_attr(any(), discharged_by = "kani")]
                #[verifier::external_body]
                pub proof fn axiom_fill_copies_usize()
                    ensures
                        fill_copies::<usize>(),
                {
                }
                #[cfg_attr(any(), discharged_by = "kani")]
                #[verifier::external_body]
                pub proof fn axiom_fill_copies_option_usize()
                    ensures
                        fill_copies::<Option<usize>>(),
                {
                }
                }
                "#,
            )
            .expect("parses")
            .items,
            false,
        );
        assert_eq!(c.errors, 0, "errors: got = {}, wanted = 0", c.errors);
        let t = c.tallies.remove("k").unwrap_or_default();
        assert_eq!(
            t.fns,
            [0, 0, 0, 2],
            "fns: got = {:?}, wanted = [0, 0, 0, 2]",
            t.fns
        );
        let d = |n: &str| (PathBuf::from(file), n.to_owned());
        let want = vec![
            d("axiom_fill_copies_usize"),
            d("axiom_fill_copies_option_usize"),
        ];
        assert_eq!(
            c.discharged, want,
            "discharged: got = {:?}, wanted = {want:?}",
            c.discharged
        );
        let usize_quote = "#[cfg_attr(any(), discharged_by = \"kani\")]\n\
            #[verifier::external_body]\n\
            pub proof fn axiom_fill_copies_usize()\n    ensures\n        fill_copies::<usize>(),\n{\n}\n";
        let option_quote = "#[cfg_attr(any(), discharged_by = \"kani\")]\n\
            #[verifier::external_body]\n\
            pub proof fn axiom_fill_copies_option_usize()\n    ensures\n        fill_copies::<Option<usize>>(),\n{\n}\n";
        let record = |quotes: &str| {
            format!(
                "Seam S6 of `verus/SEAMS.md` (`{file}`).\n\n## 1. Contract\n\n```rust\n{quotes}```\n"
            )
        };
        let got = unrecorded(
            &c.discharged,
            &[record(&format!("{usize_quote}{option_quote}"))],
        );
        assert!(
            got.is_empty(),
            "with both quotes: got = {got:?}, wanted = []"
        );
        let got = unrecorded(&c.discharged, &[record(option_quote)]);
        let want = vec![d("axiom_fill_copies_usize")];
        assert_eq!(
            got, want,
            "without the usize quote: got = {got:?}, wanted = {want:?}"
        );
    }

    /// Code under an item built only for Verus (`shipped` false) counts
    /// nowhere in the tally, even a plain verified fn, but its rlimit is
    /// still registered for the headroom check.
    #[test]
    fn unshipped_code_is_not_tallied_but_keeps_its_rlimit() {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            shipped: false,
            ..counter()
        };
        c.items(
            &parse_file("verus! { #[verifier::rlimit(20)] fn plain() {} }")
                .expect("parses")
                .items,
            false,
        );
        assert_eq!(c.errors, 0, "errors: got = {}, wanted = 0", c.errors);
        let t = c.tallies.remove("k").unwrap_or_default();
        assert_eq!(
            t.fns,
            [0, 0, 0, 0],
            "fns: got = {:?}, wanted = [0, 0, 0, 0]",
            t.fns
        );
        let got: Vec<_> = c.limits.into_iter().map(|l| (l.name, l.units)).collect();
        let want = vec![("k::plain".to_owned(), Some(20.0))];
        assert_eq!(got, want, "limits: got = {got:?}, wanted = {want:?}");
    }

    /// A trusted fn under an item built only for Verus (`shipped` false)
    /// still counts as trusted, a seam a shipped proof can rest on, but
    /// joins no list of shipped functions.
    #[test]
    fn unshipped_trusted_code_is_tallied_as_trusted() {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            shipped: false,
            ..counter()
        };
        c.items(
            &parse_file("verus! {\n#[verifier::external_body]\nfn seam() {\n}\n}")
                .expect("parses")
                .items,
            false,
        );
        assert_eq!(c.errors, 0, "errors: got = {}, wanted = 0", c.errors);
        let t = c.tallies.remove("k").unwrap_or_default();
        assert_eq!(
            t.fns,
            [0, 1, 0, 0],
            "fns: got = {:?}, wanted = [0, 1, 0, 0]",
            t.fns
        );
        assert_eq!(
            t.lines,
            [0, 2, 0, 0],
            "lines: got = {:?}, wanted = [0, 2, 0, 0]",
            t.lines
        );
        assert!(
            c.fns.is_empty(),
            "fn list: got = {} entries, wanted = 0",
            c.fns.len()
        );
    }

    #[test]
    fn the_floor_file_parses_and_rejects_what_it_should() {
        let got = Figures::default().parse(
            "# c\nfunctions 8.4 # now\n\nlines 10\nlowered lines 10 the bulk test helpers moved in\n",
        );
        assert_eq!(
            got,
            Ok(FloorFile {
                floor: Floor([Some(84), Some(100), None, None]),
                lowered: vec![("lines".into(), 100)],
            }),
            "floor: got = {got:?}, wanted = 8.4 and 10.0, lines lowered to 10.0"
        );
        let got = Figures::default().parse(
            "functions 8.4\nlines 10\nchecked-functions 20.5\nchecked-lines 30\nlowered checked-lines 30 a reason\n",
        );
        assert_eq!(
            got,
            Ok(FloorFile {
                floor: Floor([Some(84), Some(100), Some(205), Some(300)]),
                lowered: vec![("checked-lines".into(), 300)],
            }),
            "floor: got = {got:?}, wanted = all four figures, checked-lines lowered to 30.0"
        );
        let full = got.expect("parses").floor;
        assert_eq!(
            Figures::default().missing(&full),
            None,
            "missing of a full file: got = {:?}, wanted = None",
            Figures::default().missing(&full)
        );
        let partial = Figures::default()
            .parse("functions 8.4\nlines 10\nchecked-lines 30\n")
            .expect("parses")
            .floor;
        assert_eq!(
            Figures::default().missing(&partial),
            Some("checked-functions"),
            "missing: got = {:?}, wanted = checked-functions",
            Figures::default().missing(&partial)
        );
        for bad in [
            "functions 8.4\nlines 1\nchecked-functions 1\nchecked-functions 2\n",
            "checked-functions 1\nchecked-lines 1\n",
            "functions 8.4\nlines 1\nchecked 3\n",
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
            assert!(
                Figures::default().parse(bad).is_err(),
                "{bad:?}: got = Ok, wanted = Err"
            );
        }
    }

    #[test]
    fn lowering_the_floor_needs_a_reason_for_that_figure() {
        let head = |text: &str| Figures::default().parse(text).expect("parses");
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
            // A reworded reason does not make a line new: the base already
            // names that figure and value.
            (
                "functions 13.4\nlines 13.1\nlowered functions 13.4 an old reason\nlowered functions 13.4 a new reason\n",
                false,
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
            let (ok, report) = Figures::default().lowering(&head(text), &base);
            assert_eq!(ok, want, "{text:?}: got = {ok}, wanted = {want}\n{report}");
        }
        // A base without the checked figures (from before Kani counted) is
        // compared on the two it has; one with them, on all four.
        let checked = "checked-functions 20.0\nchecked-lines 30.0\n";
        let (ok, report) = Figures::default().lowering(
            &head(&format!("functions 13.7\nlines 13.1\n{checked}")),
            &base,
        );
        assert!(
            ok,
            "against a base without them: got = fail, wanted = ok\n{report}"
        );
        let base = head(&format!("functions 13.7\nlines 13.1\n{checked}"));
        for (text, want) in [
            ("checked-functions 20.0\nchecked-lines 30.0\n", true),
            ("checked-functions 19.9\nchecked-lines 30.0\n", false),
            (
                "checked-functions 19.9\nchecked-lines 30.0\nlowered checked-functions 19.9 x moved out\n",
                true,
            ),
            (
                "checked-functions 20.0\nchecked-lines 29.0\nlowered functions 29.0 wrong figure\n",
                false,
            ),
        ] {
            let text = format!("functions 13.7\nlines 13.1\n{text}");
            let (ok, report) = Figures::default().lowering(&head(&text), &base);
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
        let got = base_file(&dir, "HEAD", "verus/FLOOR");
        assert_eq!(
            got,
            Ok(Some("functions 1.0\nlines 2.0\n".into())),
            "HEAD: got = {got:?}, wanted = its FLOOR"
        );
        let got = base_file(&dir, "HEAD^", "verus/FLOOR");
        assert_eq!(got, Ok(None), "HEAD^: got = {got:?}, wanted = no FLOOR");
        for rev in ["HEAD~5", "", "no-such-branch"] {
            let got = base_file(&dir, rev, "verus/FLOOR");
            assert!(got.is_err(), "{rev:?}: got = {got:?}, wanted = an error");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// `--base-rev`'s copy of FLOOR, RLIMIT and REACH is read at the path
    /// the base's configuration gives it, so a head that points
    /// `[floor] path`, `[rlimit] path` or `[reach] path` at a file the
    /// base lacks is still compared with the base's file. Negative: a
    /// file whose section the base's configuration has, but the base
    /// lacks, is an error. A `[reach]` the base leaves out, or no base
    /// configuration, keeps the head's path, and an absent file there or
    /// at an undeclared default is nothing to compare.
    #[test]
    fn the_base_copy_is_read_at_the_base_configurations_path() {
        use GateFile::{Floor, Reach, Rlimit};
        let dir = std::env::temp_dir().join(format!("verus-count-copy-{}", std::process::id()));
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
        git(&["commit", "-q", "-m", "no gate files"]);
        for f in ["FLOOR", "RLIMIT", "REACH"] {
            fs::write(dir.join("verus").join(f), f).expect("write");
        }
        git(&["add", "verus"]);
        git(&["commit", "-q", "-m", "gate files"]);
        let declared = || {
            Config::parse(
                "[crates]\ndir = \"crates\"\n[floor]\npath = \"verus/FLOOR\"\n[rlimit]\npath = \"verus/RLIMIT\"\n[reach]\npath = \"verus/REACH\"\n",
            )
            .expect("parses")
        };
        let bare = || Config::parse("[crates]\ndir = \"crates\"\n").expect("parses");
        let base = |rev: &str, cfg: Option<Config>| Base {
            rev: rev.into(),
            cfg,
        };
        let copy = |b: &Base, head: &str, at: GateFile| b.copy(&dir, head, at);
        // The head points each path at a file the base lacks: the base's
        // own file is read, with or without the section for FLOOR and
        // RLIMIT, whose default path the base then reads.
        for cfg in [declared, bare] {
            for (head, at, want) in [
                ("verus/FLOOR.old", Floor, "FLOOR"),
                ("verus/RLIMIT.old", Rlimit, "RLIMIT"),
            ] {
                let got = copy(&base("HEAD", Some(cfg())), head, at);
                let path = format!("verus/{want}");
                assert_eq!(
                    got,
                    Ok((path.clone(), Some(want.into()))),
                    "{head}: got = {got:?}, wanted = {path}"
                );
            }
        }
        let got = copy(&base("HEAD", Some(declared())), "verus/REACH.old", Reach);
        assert_eq!(
            got,
            Ok(("verus/REACH".into(), Some("REACH".into()))),
            "[reach] path: got = {got:?}, wanted = verus/REACH"
        );
        // The base's configuration has the section, but the base not the
        // file.
        for (at, path) in [
            (Floor, "verus/FLOOR"),
            (Rlimit, "verus/RLIMIT"),
            (Reach, "verus/REACH"),
        ] {
            let got = copy(&base("HEAD^", Some(declared())), path, at);
            assert!(
                got.as_ref()
                    .is_err_and(|e| e.contains(&format!("HEAD^ has no {path}"))),
                "{path}: got = {got:?}, wanted = an error"
            );
        }
        // Nothing the base declares: an absent file is nothing to compare.
        for (head, at, want) in [
            ("verus/FLOOR.new", Floor, "verus/FLOOR"),
            ("verus/RLIMIT.new", Rlimit, "verus/RLIMIT"),
            ("verus/REACH.new", Reach, "verus/REACH.new"),
        ] {
            let got = copy(&base("HEAD^", Some(bare())), head, at);
            assert_eq!(
                got,
                Ok((want.into(), None)),
                "{head}: got = {got:?}, wanted = none at {want}"
            );
        }
        // No base configuration: the head's path.
        for (head, want) in [
            ("verus/FLOOR", Some("FLOOR".to_owned())),
            ("verus/FLOOR.old", None),
        ] {
            let got = copy(&base("HEAD", None), head, Floor);
            assert_eq!(
                got,
                Ok((head.into(), want.clone())),
                "{head}: got = {got:?}, wanted = {want:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_floor_fails_below_and_names_a_rise() {
        let want = Floor([Some(84), Some(105), Some(200), Some(300)]);
        let (ok, report) =
            Figures::default().check(&Floor([Some(84), Some(105), Some(200), Some(300)]), &want);
        assert!(ok, "at the floor: got = fail, wanted = ok\n{report}");
        let (ok, report) =
            Figures::default().check(&Floor([Some(83), Some(120), Some(200), Some(300)]), &want);
        assert!(!ok, "below on functions: got = ok, wanted = fail\n{report}");
        let (ok, report) =
            Figures::default().check(&Floor([Some(84), Some(105), Some(199), Some(310)]), &want);
        assert!(
            !ok,
            "below on checked-functions: got = ok, wanted = fail\n{report}"
        );
        assert!(
            report.contains(
                "FAIL coverage floor: checked-functions verified or discharged 19.9%, below the floor's 20.0%"
            ),
            "report: got = {report:?}"
        );
        assert!(
            report.contains("raise verus/FLOOR to `checked-lines 31.0`"),
            "report: got = {report:?}"
        );
        let (_, report) =
            Figures::default().check(&Floor([Some(83), Some(120), Some(200), Some(300)]), &want);
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
    fn a_discharged_function_needs_a_record() {
        let d = |f: &str, n: &str| (PathBuf::from(f), n.to_owned());
        let records = vec![
            "Seam S-x (`crates/a/src/x.rs`).\n```rust\nfn sort(v: &mut Ranges)\n```".to_owned(),
        ];
        let got = unrecorded(
            &[
                d("crates/a/src/x.rs", "sort"),
                d("crates/a/src/x.rs", "sor"),
                d("crates/a/src/y.rs", "sort"),
                d("crates/a/src/x.rs", "dedup"),
            ],
            &records,
        );
        let want = vec![
            d("crates/a/src/x.rs", "sor"),
            d("crates/a/src/y.rs", "sort"),
            d("crates/a/src/x.rs", "dedup"),
        ];
        assert_eq!(got, want, "unrecorded: got = {got:?}, wanted = {want:?}");
    }

    #[test]
    fn an_unparsable_verus_block_is_an_error() {
        let (t, errors) = count("verus! { fn 1() {} }");
        assert_eq!(errors, 1, "errors: got = {errors}, wanted = 1");
        assert_eq!(
            t.fns,
            [0, 0, 0, 0],
            "fns: got = {:?}, wanted = [0, 0, 0, 0]",
            t.fns
        );
    }

    /// The `#[verifier::rlimit]` limits one file, `lib.rs` of crate `k`,
    /// declares, as `(name, units)`, with the parse errors.
    fn limits(src: &str) -> (Vec<(String, Option<f32>)>, usize) {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        c.items(&parse_file(src).expect("parses").items, false);
        let got = c.limits.into_iter().map(|l| (l.name, l.units)).collect();
        (got, c.errors)
    }

    // The walk keeps every function Verus checks, with or without an
    // rlimit, named as `rlimit::Limit` names it, so a sub-default limit
    // dropped from a free function or a method raises it to the default.
    // Negative: a function outside `verus!` is not kept.
    #[test]
    fn functions_verus_checks_are_kept_for_the_rlimit_ratchet() {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        let src = "verus! {
fn f() {}
impl T {
    fn g(&self) {}
}
}
fn outside() {}
";
        c.items(&parse_file(src).expect("parses").items, false);
        assert_eq!(c.errors, 0, "errors: got = {}, wanted = 0", c.errors);
        let want: BTreeMap<String, String> = [
            ("k::T::g".to_string(), "lib.rs:4".to_string()),
            ("k::f".to_string(), "lib.rs:2".to_string()),
        ]
        .into();
        assert_eq!(
            c.verus_fns, want,
            "verus_fns: got = {:?}, wanted = {want:?}",
            c.verus_fns
        );
        let base = [
            rlimit::Limit {
                name: "k::f".into(),
                at: "lib.rs:2".into(),
                units: Some(5.0),
            },
            rlimit::Limit {
                name: "k::T::g".into(),
                at: "lib.rs:4".into(),
                units: Some(5.0),
            },
        ];
        let none = rlimit::parse_rlimit_file("max-share 70\n").expect("parses");
        let (ok, report) =
            rlimit::check_raised(&c.limits, &c.verus_fns, &base, &none, &none, "verus/RLIMIT");
        assert!(!ok, "check_raised: got = {ok}, wanted = false ({report})");
        for name in ["k::f", "k::T::g"] {
            assert!(
                report.contains(&format!("raises {name}'s rlimit from 5 to 10")),
                "report for {name}: got = {report:?}"
            );
        }
    }

    #[test]
    fn rlimit_attributes_are_named_as_verus_names_them() {
        let (got, errors) = limits(
            "verus! {
                #[verifier::rlimit(30)] fn free() {}
                impl<'a, U: Unit> Machine<'a, U> {
                    #[verifier::spinoff_prover]
                    #[verifier::rlimit(60)]
                    fn run(&mut self) {}
                    fn plain(&self) {}
                }
                trait Walk { #[verifier::rlimit(infinity)] fn walk(&self) {} }
                proof fn lemma_a() {}
                #[verifier::rlimit(15)] proof fn lemma_b() {}
                mod inner { #[verifier::rlimit(20)] fn deep() {} }
                #[cfg(test)] mod tests { #[verifier::rlimit(99)] fn t() {} }
                #[cfg(verus_keep_ghost)] #[verifier::rlimit(20)] fn ghostly() {}
                #[cfg(any(test, verus_keep_ghost))] mod grouped { #[verifier::rlimit(25)] fn g() {} }
                #[cfg(kani)] #[verifier::rlimit(98)] fn harness() {}
            }
            #[verifier::rlimit(40)] fn outside() {}",
        );
        assert_eq!(errors, 0, "errors: got = {errors}, wanted = 0");
        let want = vec![
            ("k::free".to_owned(), Some(30.0)),
            ("k::Machine::run".to_owned(), Some(60.0)),
            ("k::Walk::walk".to_owned(), None),
            ("k::lemma_b".to_owned(), Some(15.0)),
            ("k::inner::deep".to_owned(), Some(20.0)),
            ("k::ghostly".to_owned(), Some(20.0)),
            ("k::grouped::g".to_owned(), Some(25.0)),
        ];
        assert_eq!(got, want, "limits: got = {got:?}, wanted = {want:?}");
        // Negative: an argument that is neither a number nor infinity.
        let (got, errors) = limits("verus! { #[verifier::rlimit(lots)] fn f() {} }");
        assert_eq!(errors, 1, "errors: got = {errors}, wanted = 1");
        assert!(got.is_empty(), "limits: got = {got:?}, wanted = []");
        // Negative: a `cfg` the counter cannot read.
        let (_, errors) = limits("verus! { #[cfg(all(test,,unix))] fn odd() {} }");
        assert_eq!(errors, 1, "errors: got = {errors}, wanted = 1");
    }

    #[test]
    fn module_paths_follow_the_file() {
        for (krate, rel, want) in [
            ("pcre3-syntax", "lib.rs", "pcre3_syntax"),
            ("pcre3", "vm/exec.rs", "pcre3::vm::exec"),
            ("pcre3", "vm/mod.rs", "pcre3::vm"),
            ("pcre3", "vm/assert.rs", "pcre3::vm::assert"),
        ] {
            let got = module_path(krate, Path::new(rel)).join("::");
            assert_eq!(
                got, want,
                "module_path({krate}, {rel}): got = {got}, wanted = {want}"
            );
        }
        for (owner, want) in [
            ("Machine < 'a , U >", Some("Machine")),
            ("crate :: vm :: Machine", Some("Machine")),
            ("", None),
        ] {
            let got = owner_name(owner);
            assert_eq!(
                got.as_deref(),
                want,
                "owner_name({owner:?}): got = {got:?}, wanted = {want:?}"
            );
        }
    }

    /// A trusted function is recorded at its `fn` line, after its
    /// attributes, as `FILE:LINE` with its line count: the key the
    /// generated ledger's entries are matched on ([`ledger::match_ledger`]).
    /// challoc's counter's test of the same name.
    #[test]
    fn the_counter_records_where_trusted_functions_start() {
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("src/a.rs"),
            module: vec!["k".into()],
            ..counter()
        };
        let src = "verus! {\n    fn verified() {}\n    #[verifier::external_body]\n    fn trusted() {\n        1;\n    }\n}\n";
        c.items(&parse_file(src).expect("parses").items, false);
        let want = vec![("k".to_owned(), "src/a.rs:4".to_owned(), 3)];
        assert_eq!(
            c.trusted, want,
            "trusted: got = {:?}, wanted = {want:?}",
            c.trusted
        );
    }

    /// Verus's attribute form (expat-rs's: a contract in `verus_spec` under
    /// a `cfg_attr`, for a port whose MSRV is below vstd's) verifies a plain
    /// function outside `verus!`, under a predicate the configuration
    /// settles (the Verus feature, a `verus_sets` feature, `not` of a
    /// `verus_never_sets` atom); `verus_verify(external_body)` is trusted,
    /// and so is a `cfg_attr` whose predicate Verus's build may not meet (a
    /// platform, alone or beside the Verus feature), which may leave the
    /// function unchecked. Negative: a `cfg_attr` of another attribute, a
    /// path that only passes through a `verus_spec` module, or a `cfg_attr`
    /// whose predicate Verus's build never meets (`any()`, `test`, a
    /// predicate with a test atom, the negated Verus feature, a
    /// `verus_never_sets` atom), is not the attribute form.
    #[test]
    fn the_attribute_form_counts_as_verus_code() {
        let src = r#"
#[cfg_attr(feature = "verus", verus_verify)]
#[cfg_attr(feature = "verus", verus_spec(r =>
    requires n <= 4,
    ensures r == (n > 4),
))]
pub fn spec_attr(n: usize) -> bool { n > 4 }
#[verus_verify]
fn direct() {}
#[vstd::prelude::verus_spec(ensures true)]
fn by_path() {}
#[cfg_attr(feature = "verus", verus_verify(external_body))]
fn trusted_attr() {}
#[cfg_attr(feature = "verus", verus_verify)]
fn assumes() { proof!(assume(false)); }
#[cfg_attr(feature = "verus", inline)]
fn other_attr() {}
#[cfg_attr(feature = "verus", verus_spec::helper)]
fn through_a_module() {}
#[cfg_attr(any(), verus_verify)]
fn never() {}
#[cfg_attr(test, verus_verify)]
fn in_tests() {}
#[cfg_attr(all(feature = "verus", kani), verus_verify)]
fn with_a_test_atom() {}
#[cfg_attr(not(feature = "verus"), verus_spec(ensures true))]
fn not_for_verus() {}
#[cfg_attr(target_os = "windows", vstd::prelude::verus_verify)]
fn on_one_platform() {}
#[cfg_attr(all(feature = "verus", unix), verus_verify)]
fn with_a_platform() {}
#[cfg_attr(any(feature = "verus", unix), verus_verify)]
fn either() {}
#[cfg_attr(all(feature = "jit", not(miri)), verus_verify)]
fn settled() {}
#[cfg_attr(miri, verus_verify)]
fn under_miri() {}
fn plain() {}
#[cfg(feature = "verus")]
verus! {
    spec fn lead(b: u8) -> nat { 0 }
    fn verus_only() {}
}
"#;
        let (t, errors) = count(src);
        assert_eq!(errors, 0, "errors: got = {errors}, wanted = 0");
        // spec_attr, direct, by_path, either (the Verus feature settles
        // the `any`) and settled verified; trusted_attr, assumes,
        // on_one_platform and with_a_platform trusted; other_attr,
        // through_a_module, never, in_tests, with_a_test_atom,
        // not_for_verus, under_miri and plain outside. The `verus!` block
        // is built only for Verus (`feature = "verus"`), so its exec
        // function and its spec function leave the tally.
        let want = [5, 4, 8, 0];
        assert_eq!(t.fns, want, "fns: got = {:?}, wanted = {want:?}", t.fns);
        assert_eq!(t.ghost, 0, "ghost: got = {}, wanted = 0", t.ghost);
        // With `verus_only_ghost`, the Verus-only spec function joins the
        // ghost column, and nothing else moves.
        let mut c = Counter {
            krate: "k".into(),
            file: PathBuf::from("lib.rs"),
            module: vec!["k".into()],
            verus_only_ghost: true,
            ..counter()
        };
        c.items(&parse_file(src).expect("parses").items, false);
        let t = c.tallies.remove("k").unwrap_or_default();
        assert_eq!(t.fns, want, "fns: got = {:?}, wanted = {want:?}", t.fns);
        assert_eq!(t.ghost, 1, "ghost: got = {}, wanted = 1", t.ghost);
    }

    /// The FLOOR file's verified-or-discharged figures carry the configured
    /// prefix (challoc's `combined-`), in what it reads and what it says.
    #[test]
    fn the_checked_figures_take_the_configured_prefix() {
        let fig = Figures::new("combined", "x/FLOOR");
        let head = fig
            .parse("functions 1.0\nlines 2.0\ncombined-functions 3.0\ncombined-lines 4.0\n")
            .expect("parses");
        assert_eq!(fig.missing(&head.floor), None);
        assert!(
            fig.parse("functions 1.0\nlines 2.0\nchecked-lines 4.0\n")
                .is_err()
        );
        let (ok, report) = fig.check(
            &Floor([Some(10), Some(20), Some(31), Some(40)]),
            &head.floor,
        );
        assert!(ok, "{report}");
        assert!(
            report.contains("raise x/FLOOR to `combined-functions 3.1`"),
            "report: got = {report:?}"
        );
        let base = fig
            .parse("functions 1.0\nlines 2.0\ncombined-functions 3.5\n")
            .unwrap();
        let (ok, report) = fig.lowering(&head, &base);
        assert!(!ok);
        assert!(
            report.contains("x/FLOOR lowers combined-functions from 3.5% to 3.0%"),
            "report: got = {report:?}"
        );
        // The ledger's discharged functions count in the checked figures.
        let mut t = Tally::default();
        t.fns[Class::Verified as usize] = 6;
        t.fns[Class::Trusted as usize] = 4;
        t.lines[Class::Verified as usize] = 60;
        t.lines[Class::Trusted as usize] = 40;
        assert_eq!(
            shares(&t, (2, 30)),
            Floor([Some(600), Some(600), Some(800), Some(900)])
        );
    }

    /// With `[floor] renamed_from`, the base revision's copy is read under
    /// the old prefix as well, so the change that renames the checked
    /// figures (challoc's `combined-` to `checked-`) still compares with
    /// its base; the file under test takes the new names alone.
    #[test]
    fn a_base_may_give_the_checked_figures_their_old_names() {
        let fig = Figures::new("checked", "x/FLOOR");
        let old = "functions 1.0\nlines 2.0\ncombined-functions 3.5\ncombined-lines 4.0\nlowered combined-lines 3.9 an old reason\n";
        assert!(
            fig.parse(old).is_err(),
            "old names without renamed_from: got = Ok, wanted = Err"
        );
        let base = fig.for_base(Some("combined")).parse(old).expect("parses");
        assert_eq!(
            base,
            FloorFile {
                floor: Floor([Some(10), Some(20), Some(35), Some(40)]),
                lowered: vec![("checked-lines".into(), 39)],
            },
            "base: got = {base:?}, wanted = the old figures as the checked ones"
        );
        assert!(
            fig.for_base(Some("combined"))
                .parse("functions 1.0\nlines 2.0\ncombined-lines 4.0\nchecked-lines 4.0\n")
                .is_err(),
            "a figure under both names: got = Ok, wanted = Err"
        );
        for (text, want) in [
            ("checked-functions 3.5\nchecked-lines 4.0\n", true),
            ("checked-functions 3.4\nchecked-lines 4.0\n", false),
            (
                "checked-functions 3.4\nchecked-lines 4.0\nlowered checked-functions 3.4 moved out\n",
                true,
            ),
            // The base's reason, under its old name, is not new here.
            (
                "checked-functions 3.5\nchecked-lines 3.9\nlowered checked-lines 3.9 a new reason\n",
                false,
            ),
            (
                "checked-functions 3.5\nchecked-lines 3.8\nlowered checked-lines 3.8 moved out\n",
                true,
            ),
            ("checked-functions 3.5\nchecked-lines 3.8\n", false),
        ] {
            let text = format!("functions 1.0\nlines 2.0\n{text}");
            let head = fig.parse(&text).expect("parses");
            let (ok, report) = fig.lowering(&head, &base);
            assert_eq!(ok, want, "{text:?}: got = {ok}, wanted = {want}\n{report}");
        }
    }

    /// A crate's edition is its own, the workspace's it inherits, or 2015
    /// (none given, a workspace that gives none, a manifest that does not
    /// parse); a dependency key among `vstd`, `verus_builtin_macros`,
    /// `core` and `std` (hyphens read as `_`) whose `package` is another
    /// is renamed, in any dependency table, a `[target.*]` one too.
    #[test]
    fn the_manifest_gives_the_edition_and_renamed_dependencies() {
        let root =
            std::env::temp_dir().join(format!("verus-count-manifest-{}", std::process::id()));
        let k = root.join("crates/k");
        fs::create_dir_all(&k).unwrap();
        for (ws, krate, edition, renamed) in [
            ("", "[package]\nedition = \"2024\"\n", 2024, vec![]),
            ("", "[package]\nname = \"k\"\n", 2015, vec![]),
            ("", "[package\n", 2015, vec![]),
            (
                "[workspace.package]\nedition = \"2021\"\n",
                "[package]\nedition.workspace = true\n",
                2021,
                vec![],
            ),
            (
                "[workspace]\n",
                "[package]\nedition.workspace = true\n",
                2015,
                vec![],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[dependencies]\nvstd = { package = \"other\" }\nfoo = { package = \"vstd\" }\n",
                2024,
                vec!["vstd"],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[target.'cfg(unix)'.dev-dependencies]\nverus-builtin-macros = { package = \"x\" }\n",
                2024,
                vec!["verus_builtin_macros"],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[dependencies]\nvstd = { package = \"vstd\" }\nverus-builtin-macros = { package = \"verus_builtin_macros\" }\n",
                2024,
                vec![],
            ),
            // Through `[workspace.dependencies]`: renamed there, the same
            // package, or an entry the root does not give.
            (
                "[workspace.dependencies]\nvstd = { package = \"other\", path = \"x\" }\n",
                "[package]\nedition = \"2024\"\n[dependencies]\nvstd = { workspace = true }\n",
                2024,
                vec!["vstd"],
            ),
            (
                "[workspace.dependencies]\nvstd = { version = \"1\" }\n",
                "[package]\nedition = \"2024\"\n[target.'cfg(unix)'.dependencies]\nvstd.workspace = true\n",
                2024,
                vec![],
            ),
            (
                "[workspace]\n",
                "[package]\nedition = \"2024\"\n[dependencies]\nvstd = { workspace = true }\n",
                2024,
                vec!["vstd"],
            ),
            // A target's own edition, the lowest one.
            (
                "",
                "[package]\nedition = \"2024\"\n[lib]\nedition = \"2015\"\n",
                2015,
                vec![],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[[bin]]\nname = \"b\"\nedition = \"2021\"\n",
                2021,
                vec![],
            ),
        ] {
            fs::write(root.join("Cargo.toml"), ws).unwrap();
            fs::write(k.join("Cargo.toml"), krate).unwrap();
            let (got, deps) = crate_manifest(&root, &k);
            let got_renamed: Vec<&str> = deps.iter().map(|(n, _)| n.as_str()).collect();
            assert!(
                got == edition && got_renamed == renamed,
                "{krate:?}: got = {got}, {got_renamed:?}; wanted = {edition}, {renamed:?}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// The editions macros match by are the library's (where the package
    /// has one) and each binary's, their own or the package's (inherited
    /// from the workspace too), and
    /// those of the tests, benches and examples whose `path` is a file
    /// under `src/`; one with no `path`, or a path outside `src/`, is not
    /// read.
    #[test]
    fn the_walked_targets_give_the_matching_editions() {
        let root = std::env::temp_dir().join(format!("verus-count-shipped-{}", std::process::id()));
        let k = root.join("crates/k");
        fs::create_dir_all(k.join("src")).unwrap();
        fs::create_dir_all(k.join("tests")).unwrap();
        fs::write(k.join("src/legacy.rs"), "").unwrap();
        fs::write(k.join("src/lib.rs"), "").unwrap();
        fs::write(k.join("tests/t.rs"), "").unwrap();
        for (ws, krate, want) in [
            // `autolib = false` leaves `src/lib.rs` out, but not a `[lib]`.
            (
                "",
                "[package]\nedition = \"2024\"\nautolib = false\n[[bin]]\nname = \"a\"\npath = \"src/legacy.rs\"\nedition = \"2021\"\n",
                vec![2021],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\nautolib = false\n[lib]\nedition = \"2021\"\n",
                vec![2021],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[[test]]\nname = \"t\"\npath = \"src/legacy.rs\"\nedition = \"2021\"\n",
                vec![2024, 2021],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[[bench]]\nname = \"b\"\npath = \"src/legacy.rs\"\nedition = \"2018\"\n",
                vec![2024, 2018],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[[example]]\nname = \"e\"\npath = \"src/../src/legacy.rs\"\n",
                vec![2024, 2024],
            ),
            (
                "",
                "[package]\nedition = \"2024\"\n[[test]]\nname = \"t\"\npath = \"tests/t.rs\"\nedition = \"2021\"\n[[example]]\nname = \"e\"\npath = \"src/missing.rs\"\nedition = \"2021\"\n",
                vec![2024],
            ),
            ("", "[package]\nedition = \"2024\"\n", vec![2024]),
            ("", "[package]\nname = \"k\"\n", vec![2015]),
            (
                "",
                "[package]\nedition = \"2024\"\n[[test]]\nname = \"t\"\nedition = \"2021\"\n[[bench]]\nname = \"b\"\nedition = \"2021\"\n[[example]]\nname = \"e\"\nedition = \"2015\"\n",
                vec![2024],
            ),
            (
                "[workspace.package]\nedition = \"2024\"\n",
                "[package]\nedition.workspace = true\n[lib]\nedition = \"2021\"\n[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\nedition = \"2018\"\n",
                vec![2021, 2024, 2018],
            ),
        ] {
            fs::write(root.join("Cargo.toml"), ws).unwrap();
            fs::write(k.join("Cargo.toml"), krate).unwrap();
            let got = walked_editions(&root, &k);
            assert_eq!(got, want, "{krate:?}: got = {got:?}, wanted = {want:?}");
        }
        // With no `src/lib.rs`, a package has a library only by a `[lib]
        // path`: a binary-only one gives its binaries' editions alone, and
        // one with no target the package's.
        fs::remove_file(k.join("src/lib.rs")).unwrap();
        fs::write(k.join("src/main.rs"), "").unwrap();
        for (krate, want) in [
            (
                "[package]\nname = \"k\"\nedition = \"2024\"\nautobins = false\n[[bin]]\nname = \"k\"\npath = \"src/main.rs\"\nedition = \"2021\"\n",
                vec![2021],
            ),
            ("[package]\nname = \"k\"\nedition = \"2021\"\n", vec![2021]),
            (
                "[package]\nname = \"k\"\nedition = \"2024\"\nautobins = false\n[lib]\npath = \"src/legacy.rs\"\nedition = \"2021\"\n",
                vec![2021],
            ),
            (
                "[package]\nname = \"k\"\nedition = \"2018\"\nautobins = false\n",
                vec![2018],
            ),
        ] {
            fs::write(root.join("Cargo.toml"), "").unwrap();
            fs::write(k.join("Cargo.toml"), krate).unwrap();
            let got = walked_editions(&root, &k);
            assert_eq!(got, want, "{krate:?}: got = {got:?}, wanted = {want:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// `publish = false` is the package's only in its `[package]` table:
    /// one under `[package.metadata]` or another table, in a comment or a
    /// string, or in a manifest that does not parse leaves the crate
    /// shipped.
    #[test]
    fn publish_is_read_from_the_package_table() {
        for (text, want) in [
            ("[package]\nname = \"a\"\npublish = false\n", true),
            ("[package]\npublish=false # never\n", true),
            ("package = { name = \"a\", publish = false }\n", true),
            (
                "[package]\nname = \"a\"\n\n[package.metadata]\npublish = false\n",
                false,
            ),
            (
                "[package]\nname = \"a\"\n\n[package.metadata.x]\npublish = false\n",
                false,
            ),
            ("[package]\nname = \"a\"\n# publish = false\n", false),
            (
                "[package]\ndescription = \"\"\"\npublish = false\n\"\"\"\n",
                false,
            ),
            ("[package]\npublish = true\n", false),
            ("[package]\npublish = [\"crates-io\"]\n", false),
            ("[package\npublish = false\n", false),
        ] {
            let got = unpublished(text);
            assert!(got == want, "{text:?}: got = {got}, wanted = {want}");
        }
    }

    /// The crates come from the configuration: `dir`'s subdirectories with a
    /// `src/`, then the members, each shipping unless its Cargo.toml says
    /// `publish = false` or its `ships` says otherwise.
    #[test]
    fn crates_come_from_the_configuration() {
        let root = std::env::temp_dir().join(format!("verus-count-crates-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["crates/a/src", "crates/b/src", "crates/notes", "src"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\npublish = false\n",
        )
        .unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\npublish = false\n").unwrap();
        std::os::unix::fs::symlink("crates/a", root.join("alias")).unwrap();
        std::os::unix::fs::symlink("crates", root.join("linked")).unwrap();
        // Links a configured path does not name: a crate of a `dir` that is
        // one, a `src` that is one, and a source file that is one.
        for d in ["more/real/src", "other", "deep/src"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        std::os::unix::fs::symlink("../crates/a", root.join("more/alias")).unwrap();
        std::os::unix::fs::symlink("../crates/a/src", root.join("other/src")).unwrap();
        std::os::unix::fs::symlink("../../crates/a/src/lib.rs", root.join("deep/src/lib.rs"))
            .unwrap();
        let read = |text: &str| -> Vec<(String, bool)> {
            crate_dirs(&root, &Config::parse(text).expect("parses"))
                .expect("reads")
                .into_iter()
                .map(|k| (k.name, k.ships))
                .collect()
        };
        assert_eq!(
            read("[crates]\ndir = \"crates\"\n"),
            [("a".to_string(), true), ("b".to_string(), false)]
        );
        assert_eq!(
            read("[crates]\nmembers = [{ name = \"top\", path = \".\" }]\n"),
            [("top".to_string(), false)]
        );
        assert_eq!(
            read("[crates]\nmembers = [{ name = \"top\", path = \".\", ships = true }]\n"),
            [("top".to_string(), true)]
        );
        let got = crate_dirs(
            &root,
            &Config::parse("[crates]\nmembers = [{ name = \"n\", path = \"crates/notes\" }]\n")
                .unwrap(),
        );
        assert!(
            got.is_err(),
            "a member without src/: got = Ok, wanted = an error"
        );
        let got = crate_dirs(&root, &Config::parse("[crates]\ndir = \"gone\"\n").unwrap());
        assert!(got.is_err(), "a missing dir: got = Ok, wanted = an error");
        // A crate read twice, under one name or from one directory, is
        // refused: the tally goes by name, so `ships = false` on the second
        // would take the first out of the shipped row too.
        for (text, want) in [
            (
                "[crates]\ndir = \"crates\"\nmembers = [{ name = \"a\", path = \"crates/a\", ships = false }]\n",
                "reads crate a (",
            ),
            (
                "[crates]\nmembers = [{ name = \"a\", path = \"crates/a\" }, { name = \"a\", path = \"crates/b\" }]\n",
                "reads crate a (",
            ),
            (
                "[crates]\nmembers = [{ name = \"x\", path = \"crates/a\" }, { name = \"y\", path = \"crates/a\" }]\n",
                "reads crate x (",
            ),
            // Nor is a path that spells a directory another way, through
            // `..` or a symlink, which the base comparison would take for
            // a new crate.
            (
                "[crates]\nmembers = [{ name = \"r\", path = \"crates/../crates/a\" }]\n",
                "[crates] path crates/../crates/a has a `..`",
            ),
            (
                "[crates]\ndir = \"crates/..\"\n",
                "[crates] path crates/.. has a `..`",
            ),
            (
                "[crates]\nmembers = [{ name = \"r\", path = \"alias\" }]\n",
                "[crates] path alias passes through alias, a symlink",
            ),
            (
                "[crates]\nmembers = [{ name = \"r\", path = \"linked/a\" }]\n",
                "[crates] path linked/a passes through linked, a symlink",
            ),
            (
                "[crates]\ndir = \"linked\"\n",
                "[crates] path linked passes through linked, a symlink",
            ),
            (
                "[crates]\ndir = \"more\"\n",
                "[crates] crate alias reads more/alias, a symlink",
            ),
            (
                "[crates]\nmembers = [{ name = \"o\", path = \"other\" }]\n",
                "[crates] crate o reads other/src, a symlink",
            ),
            (
                "[crates]\nmembers = [{ name = \"d\", path = \"deep\" }]\n",
                "[crates] crate d reads deep/src/lib.rs, a symlink",
            ),
        ] {
            let got = crate_dirs(&root, &Config::parse(text).expect("parses"));
            assert!(
                got.as_ref().is_err_and(|e| e.contains(want)),
                "{text:?}: got = {:?}, wanted = an error with {want:?}",
                got.map(|k| k.into_iter().map(|k| k.name).collect::<Vec<_>>())
            );
        }
        // A path from `/`, even to a crate under the root, is refused:
        // `root.join` would read it while it compares unequal to the base's
        // relative path.
        let abs = root.join("crates/a").to_string_lossy().into_owned();
        let got = crate_dirs(
            &root,
            &Config::parse(&format!(
                "[crates]\nmembers = [{{ name = \"r\", path = {abs:?} }}]\n"
            ))
            .expect("parses"),
        );
        let want = format!("[crates] path {abs} is not relative");
        assert!(
            got.as_ref().is_err_and(|e| e.contains(&want)),
            "an absolute member path: got = {:?}, wanted = an error with {want:?}",
            got.map(|k| k.into_iter().map(|k| k.name).collect::<Vec<_>>())
        );
        let _ = fs::remove_dir_all(&root);
    }
}
