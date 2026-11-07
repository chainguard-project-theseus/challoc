use super::*;
use std::process::Command;
use std::{env, format, vec::Vec};

const PAGE: usize = 4096;

/// `grant_syscalls` is called only in the body of an `extern "C"`
/// function or of `abort_fmt`, which never returns ([`may_syscall`]'s
/// premise).
#[test]
fn syscall_grants_only_at_roots() {
    // Spelled in parts, so that this test's own lines match nothing.
    let sites = crate::testutil::grant_sites(&format!("{}{}", "grant_", "syscalls"));
    assert!(!sites.is_empty());
    for (sig, at) in &sites {
        assert!(
            sig.contains("extern \"C\" fn ") || sig.starts_with("pub fn abort_fmt("),
            "{at}: a syscall grant in `{sig}`"
        );
    }
}

/// `grant_aborts` is called only in the body of an `extern "C"`
/// function or of the panic handler ([`may_abort`]'s premise), never
/// in `abort_fmt`'s.
#[test]
fn abort_grants_only_at_roots() {
    // Spelled in parts, so that this test's own lines match nothing.
    let sites = crate::testutil::grant_sites(&format!("{}{}", "grant_", "aborts"));
    assert!(!sites.is_empty());
    for (sig, at) in &sites {
        assert!(
            sig.contains("extern \"C\" fn ")
                || sig.starts_with("fn panic(info: &core::panic::PanicInfo"),
            "{at}: an abort grant in `{sig}`"
        );
    }
}

/// `grant_user_reads` is called once in each of `purge::check_zero`
/// and the realloc cores `api::libc::realloc_block`,
/// `api::libc::realloc_usable_stored` and `api::jemalloc::rallocx_zeroed`,
/// and nowhere else (the callers
/// [`crate::vspec::user::may_read_user`]'s doc lists).
#[test]
fn user_read_grants_only_in_listed_callers() {
    crate::testutil::assert_only_at(
        concat!("grant_", "user_reads("),
        &[
            ("purge.rs:", "pub fn check_zero(", 1),
            ("api/libc.rs:", "pub fn realloc_block(", 1),
            ("api/libc.rs:", "pub fn realloc_usable_stored(", 1),
            ("api/jemalloc.rs:", "pub fn rallocx_zeroed(", 1),
        ],
    );
}

/// The user-read capability (`vspec::user::may_read_user`) is
/// established only by its grants: it appears in an `ensures` only as
/// `grant_user_reads`' own and as `purge::check_zero`'s on a `true`
/// result (`r ==> ..`), and elsewhere in shipped code only as a
/// `requires` or loop invariant clause that ends with it, besides its
/// definition and the grant's `reveal`. So no other function hands the
/// capability to its caller, and the grant sites
/// [`user_read_grants_only_in_listed_callers`] pins are where it starts.
#[test]
fn user_read_capability_ensured_only_at_grants() {
    // Spelled in parts, so that this test's own lines match nothing.
    let cap = std::format!("{}{}", "may_", "read_user");
    let grant = std::format!("pub proof fn {}{}()", "grant_", "user_reads");
    let clause = std::format!("crate::vspec::user::{cap}(),");
    let (short, reveal) = (std::format!("{cap}(),"), std::format!("reveal({cap});"));
    let def = std::format!("pub closed spec fn {cap}() -> bool {{");
    let mut ensured = Vec::new();
    for (rel, text) in crate::testutil::shipped_sources() {
        let (mut sig, mut kw) = (std::string::String::new(), "");
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if crate::testutil::fn_item(t) {
                (sig, kw) = (t.into(), "");
            }
            if t == "{" || t.ends_with(" {") {
                kw = "";
            }
            // A clause keyword starts its line, alone or before the
            // clause (`ensures x,`).
            if let Some(k) = [
                "requires",
                "recommends",
                "ensures",
                "returns",
                "invariant",
                "invariant_except_break",
                "decreases",
            ]
            .into_iter()
            .find(|k| t.split_whitespace().next() == Some(*k))
            {
                kw = k;
            }
            if !t.contains(cap.as_str()) {
                continue;
            }
            let at = std::format!("{rel}:{}: `{t}`", i + 1);
            let user = rel == "vspec/user.rs";
            let in_grant = user && sig == grant;
            if (user && t == def) || (in_grant && t == reveal) {
                continue;
            }
            if kw == "ensures" {
                let grant_own = in_grant && t == short;
                let check_zero = rel == "purge.rs"
                    && sig.starts_with("pub fn check_zero(")
                    && t == std::format!("r ==> {clause}");
                assert!(
                    grant_own || check_zero,
                    "{at}: `{cap}` in an `ensures` of `{sig}`: only the grants establish it"
                );
                ensured.push(at);
            } else {
                assert!(
                    (t.ends_with(clause.as_str()) || (user && t == short))
                        && (kw == "requires" || kw.starts_with("invariant")),
                    "{at}: `{cap}` used other than as a requires or invariant clause"
                );
            }
        }
    }
    assert_eq!(
        ensured.len(),
        2,
        "got = {ensured:?}, want = the grant's own `ensures` and check_zero's"
    );
}

/// A line of the shipped sources, for the capability tests below.
struct Line {
    /// The file, relative to `src/`.
    rel: std::string::String,
    /// `file:line: `text``.
    at: std::string::String,
    /// The `fn` item line it belongs to: the nearest one above it.
    sig: std::string::String,
    /// The clause it is in (`requires`, `ensures`, `invariant`, ..),
    /// or `""` from the body's `{` on.
    kw: &'static str,
    /// The line, trimmed.
    t: std::string::String,
}

/// The lines of [`crate::testutil::shipped_sources`], less comment
/// lines and `api/restated.rs` (a test module in a file of its own).
fn shipped_lines() -> Vec<Line> {
    let mut out = Vec::new();
    for (rel, text) in crate::testutil::shipped_sources() {
        if rel == "api/restated.rs" {
            continue;
        }
        let (mut sig, mut kw) = (std::string::String::new(), "");
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.is_empty() || t.starts_with("//") {
                continue;
            }
            if crate::testutil::fn_item(t) {
                (sig, kw) = (t.into(), "");
            }
            if t == "{" || t.ends_with(" {") {
                kw = "";
            }
            // A clause keyword starts its line, alone or before the
            // clause (`ensures x,`).
            if let Some(k) = [
                "requires",
                "recommends",
                "ensures",
                "returns",
                "invariant",
                "invariant_except_break",
                "decreases",
            ]
            .into_iter()
            .find(|k| t.split_whitespace().next() == Some(*k))
            {
                kw = k;
            }
            out.push(Line {
                rel: rel.clone(),
                at: format!("{rel}:{}: `{t}`", i + 1),
                sig: sig.clone(),
                kw,
                t: t.into(),
            });
        }
    }
    assert!(out.len() > 10_000, "the source tree was not found");
    out
}

/// [`may_syscall`] appears in the shipped sources only as a `requires`
/// or loop invariant clause of its own (`crate::sys::may_syscall(),`),
/// besides its definition, [`grant_syscalls`]' `ensures` and `reveal`,
/// and the type invariants of `abort::Stderr` and of the report's
/// `api::print::Sink`, which only their `new` builds, under a
/// `requires` of it. So no function or axiom ensures it for its
/// caller, and the grants [`syscall_grants_only_at_roots`] pins are
/// where it starts.
#[test]
fn syscall_capability_only_as_a_clause() {
    // Spelled in parts, so that this test's own lines match nothing.
    let cap = format!("{}{}", "may_", "syscall");
    let clause = format!("crate::sys::{cap}(),");
    let grant = format!("pub proof fn {}{}()", "grant_", "syscalls");
    let own = [
        (
            "sys/mod.rs",
            "pub closed spec fn ",
            format!("pub closed spec fn {cap}() -> bool {{"),
        ),
        ("sys/mod.rs", grant.as_str(), format!("{cap}(),")),
        ("sys/mod.rs", grant.as_str(), format!("reveal({cap});")),
        (
            "sys/abort.rs",
            "spec fn wf(&self)",
            format!("self.len <= 512 && crate::sys::{cap}()"),
        ),
        (
            "api/print.rs",
            "spec fn wf(&self)",
            format!("self.len < 1024 && crate::sys::{cap}() && self.ord@.top() == 0"),
        ),
    ];
    let mut seen = [0; 5];
    let mut clauses = 0;
    for l in shipped_lines() {
        if !l.t.contains(cap.as_str()) {
            continue;
        }
        if let Some(k) = own
            .iter()
            .position(|(f, s, t)| l.rel == *f && l.sig.starts_with(s) && l.t == *t)
        {
            seen[k] += 1;
            continue;
        }
        assert!(
            l.t == clause && (l.kw == "requires" || l.kw.starts_with("invariant")),
            "{}: `{cap}` used other than as a requires or invariant clause `{clause}` \
             (in `{}`, clause `{}`)",
            l.at,
            l.sig,
            l.kw
        );
        clauses += 1;
    }
    assert_eq!(seen, [1; 5], "got = {seen:?}, want = each of {own:?} once");
    assert!(clauses > 100, "got = {clauses} clauses, want = the tree's");
}

/// The abort capabilities ([`may_abort`], [`may_abort_corrupt`],
/// [`may_abort_kernel`], [`may_abort_pending`], and [`valid_aborts`],
/// which stands for two of them) appear in the shipped sources only in
/// `requires` and loop invariant clauses, besides their definitions,
/// [`grant_aborts`]' `ensures` and `reveal`, and those of
/// `abort::relabel`, which only the three narrower sinks call
/// (`abort::tests::abort_relabels_only_in_the_sinks`): no other
/// function or axiom ensures one for its caller. The uninterpreted
/// capabilities ([`integrity_aborts`], [`kernel_aborts`],
/// [`pending_aborts`]) appear, besides their definitions and the
/// bodies of the capabilities above, only as a `requires` clause of
/// their own in a theorem's witness (`fn theorem_`).
#[test]
fn abort_capabilities_only_as_clauses() {
    // Spelled in parts, so that this test's own lines match nothing.
    let cap = format!("{}{}", "may_", "abort");
    let valid = format!("{}{}", "valid_", "aborts");
    let grant = format!("pub proof fn {}{}()", "grant_", "aborts");
    let relabel = format!("proof fn {}{}()", "rel", "abel");
    let uninterp = ["integrity_", "kernel_", "pending_"].map(|p| format!("{p}{}", "aborts"));
    // `sys/mod.rs`'s definitions, whose bodies name the others.
    let defs = [
        format!("pub closed spec fn {cap}() -> bool {{"),
        format!("pub open spec fn {cap}_corrupt() -> bool {{"),
        format!("pub open spec fn {cap}_kernel() -> bool {{"),
        format!("pub open spec fn {cap}_pending() -> bool {{"),
        format!("pub open spec fn {valid}() -> bool {{"),
    ];
    // Their bodies, each the one line of its definition's.
    let [i, k, p] = &uninterp;
    let bodies = [
        format!("{cap}() || {i}()"),
        format!("{cap}() || {k}()"),
        format!("{cap}() || {p}()"),
        format!("{k}() && {p}()"),
    ];
    let decls = uninterp
        .clone()
        .map(|u| format!("pub uninterp spec fn {u}() -> bool;"));
    let (mut clauses, mut witnesses) = (0, 0);
    for l in shipped_lines() {
        let names_cap = l.t.contains(cap.as_str()) || l.t.contains(valid.as_str());
        let names_uninterp = uninterp.iter().any(|u| l.t.contains(u.as_str()));
        if !names_cap && !names_uninterp {
            continue;
        }
        if l.rel == "sys/mod.rs"
            && ((defs.contains(&l.sig) && (l.t == l.sig || bodies.contains(&l.t)))
                || decls.contains(&l.t)
                || (l.sig == grant
                    && ((l.kw == "ensures" && l.t == format!("{cap}(),"))
                        || l.t == format!("reveal({cap});"))))
        {
            continue;
        }
        if l.rel == "sys/abort.rs"
            && l.sig == relabel
            && ((l.kw == "ensures" && l.t == format!("crate::sys::{cap}(),"))
                || l.t == format!("reveal(crate::sys::{cap});"))
        {
            continue;
        }
        if names_uninterp {
            assert!(
                uninterp
                    .iter()
                    .any(|u| l.t == format!("crate::sys::{u}(),"))
                    && l.kw == "requires"
                    && l.sig.contains("fn theorem_"),
                "{}: an uninterpreted abort capability outside a witness's requires \
                 (in `{}`, clause `{}`)",
                l.at,
                l.sig,
                l.kw
            );
            witnesses += 1;
        } else {
            assert!(
                l.kw == "requires" || l.kw.starts_with("invariant"),
                "{}: `{cap}` or `{valid}` used other than in a requires or invariant clause \
                 (in `{}`, clause `{}`)",
                l.at,
                l.sig,
                l.kw
            );
            clauses += 1;
        }
    }
    assert!(clauses > 100, "got = {clauses} clauses, want = the tree's");
    assert!(witnesses > 0, "got = no witness requires");
}

/// A call site: `(file, fn item line start, calls)`, as for
/// [`crate::testutil::assert_only_at`].
type Site = (&'static str, &'static str, usize);

/// The abort sinks below `abort_fmt` that take a narrower capability
/// than [`may_abort`]: the integrity checks (`abort_integrity` and
/// `vspec::check::check`, under [`may_abort_corrupt`]), the resource
/// failures (`abort_kernel`, under [`may_abort_kernel`]) and the
/// pending checks (`abort_pending`, under [`may_abort_pending`]), each
/// as `(name, sites)`.
const SINKS: &[(&str, &[Site])] = &[
    (
        concat!("abort_", "integrity"),
        &[("stats.rs", "fn huge_pages_short(", 1)],
    ),
    (
        concat!("chec", "k"),
        &[
            ("bitmap.rs", "pub fn allocated(", 1),
            ("class/table.rs", "pub fn new(", 1),
        ],
    ),
    (
        concat!("abort_", "kernel"),
        &[
            ("heap/slotstate.rs", "pub(super) fn init(", 2),
            ("heap/slotstate.rs", "pub(crate) fn commit(", 1),
            ("huge/free.rs", "pub fn free_run(", 1),
            ("purge.rs", "fn discard(", 1),
        ],
    ),
    (
        concat!("abort_", "pending"),
        &[
            ("arena.rs", "fn unbind_unbound(", 1),
            ("class/abort.rs", "pub(super) fn never_committed(", 1),
            ("class/abort.rs", "pub(super) fn owner_mismatch(", 1),
            (
                "class/abort.rs",
                "pub(super) fn release_into_fresh_slab(",
                1,
            ),
            ("class/decay.rs", "pub(super) fn walk_empty<", 1),
            ("class/verify.rs", "fn shadow_written(", 1),
            ("class/verify.rs", "pub(super) fn release_into_fresh(", 1),
            ("heap/shared.rs", "fn no_such_arena(", 1),
            ("heap/slotstate.rs", "pub(super) fn free(", 3),
            ("heap/slotstate.rs", "pub(super) fn claim(", 1),
            ("heap/slotstate.rs", "pub(super) fn pop(", 1),
            ("heap/slotstate.rs", "pub(super) fn release(", 1),
            ("heap/slotstate.rs", "pub(super) fn check_not_cached(", 1),
            ("huge/mod.rs", "fn huge_count_underflow(", 1),
            ("huge/table.rs", "pub(super) fn insert(", 1),
            ("slab.rs", "fn bad_state_byte(", 1),
            ("slab.rs", "pub fn sub_live(", 1),
        ],
    ),
];

/// The calls in `t` of `name` as a whole name (`x::name(` or
/// `name(`, not `may_name(` or `x.name(`), none if `t` declares it.
fn sink_calls(t: &str, name: &str) -> usize {
    if t.contains(&format!("fn {name}(")) {
        return 0;
    }
    let call = format!("{name}(");
    t.match_indices(call.as_str())
        .filter(|(j, _)| {
            t[..*j]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        })
        .count()
}

/// The narrower aborts are called only at their check sites
/// ([`SINKS`]), with the counts listed. A new call is a new way for a
/// holder of the narrower capability to abort, so it fails here until
/// it is added to the list. The test does not check that a theorem
/// argues for a site: `verus/THEOREMS.md`'s "Valid programs never
/// abort" does not list `heap::slotstate`'s.
#[test]
fn narrow_aborts_only_at_their_check_sites() {
    let lines = shipped_lines();
    for &(name, sites) in SINKS {
        // (file, fn item line) -> calls.
        let mut found: std::collections::BTreeMap<(&str, &str), usize> =
            std::collections::BTreeMap::new();
        for l in &lines {
            let n = sink_calls(&l.t, name);
            if n == 0 {
                continue;
            }
            let Some(&(file, f, _)) = sites
                .iter()
                .find(|(file, f, _)| l.rel == *file && l.sig.starts_with(f))
            else {
                panic!(
                    "{}: a call of `{name}` in `{}`, not one of its check sites",
                    l.at, l.sig
                );
            };
            *found.entry((file, f)).or_default() += n;
        }
        for &(file, f, want) in sites {
            let got = found.get(&(file, f)).copied().unwrap_or(0);
            assert_eq!(
                got, want,
                "{file} `{f}`: got = {got} calls of `{name}`, want = {want}"
            );
        }
    }
}

/// No `use` (of any visibility, over any number of lines) outside
/// `sys/` and `vspec/` names one of the narrower aborts ([`SINKS`]),
/// and none inside them renames one (`as`), so a call cannot escape
/// [`narrow_aborts_only_at_their_check_sites`] under another name. The
/// one exception is `class/table.rs`'s import of `vspec::check::check`
/// under its own name, whose call that test sees.
#[test]
fn no_use_of_the_narrow_aborts() {
    let allowed = [(
        "class/table.rs",
        concat!("use crate::vspec::chec", "k::check;"),
    )];
    let mut seen = 0;
    let mut in_use = std::string::String::new();
    for l in shipped_lines() {
        let first = l.t.split_whitespace().take(2).any(|w| w == "use");
        if !first && in_use != l.rel {
            continue;
        }
        in_use = if l.t.ends_with(';') {
            std::string::String::new()
        } else {
            l.rel.clone()
        };
        if allowed.iter().any(|(f, t)| l.rel == *f && l.t == *t) {
            seen += 1;
            continue;
        }
        // A sink's name; for `check`, a common word, the paths
        // `check::check`, `check::{` and `check::*`, or `check` as a
        // word of a `use` list's later line.
        let check = concat!("chec", "k");
        let paths = [
            format!("{check}::{check}"),
            format!("{check}::{{"),
            format!("{check}::*"),
        ];
        let names_sink = SINKS
            .iter()
            .any(|(name, _)| *name != check && l.t.contains(name))
            || paths.iter().any(|p| l.t.contains(p.as_str()))
            || (!first && l.t.split([' ', ',', '{', '}']).any(|w| w == check));
        if l.rel.starts_with("sys/") || l.rel.starts_with("vspec/") {
            assert!(
                !(names_sink && l.t.contains(" as ")),
                "{}: a `use` that renames a narrower abort",
                l.at
            );
        } else {
            assert!(!names_sink, "{}: a `use` of a narrower abort", l.at);
        }
    }
    assert_eq!(
        seen,
        allowed.len(),
        "got = {seen}, want = each of {allowed:?}"
    );
}

/// Every trusted (`external_body`) exec function in the shipped
/// sources that can abort requires a capability for it. "Can abort"
/// is read from the text: the function does not return (`-> !`), or
/// its body has one of the tokens below (`assert!`, `panic!`, an
/// `unwrap`, a call of an abort, the trap instruction). Such a function
/// requires the bare clause `crate::sys::may_abort(),` (a line of its
/// own after `requires` or a `,`); a header line that names an abort
/// capability in any other form fails, unless it is listed here as
/// that function's compound clause, by its exact text
/// (`assert_flushed`, `assert_refill_empty` and `assert_rekey_flushed`'s
/// `may_abort() || *len == 0`, and `vspec::check::check`'s
/// `may_abort_corrupt() || c`). Or it is listed as unfailing (its panic
/// is its `requires`). Every listed function must still be one. Not
/// seen: a panic of an index or a slice (`vspec::print::base_slab_sizes` and
/// `layout::regions`' `word` index under their `requires`), and an
/// abort only through a function the body calls (Verus checks a call
/// against the callee's `requires` only in a verified body).
#[test]
fn trusted_aborts_require_a_capability() {
    let cap = format!("{}{}", "may_", "abort");
    let valid = format!("{}{}", "valid_", "aborts");
    // The compound clauses, each `(file, fn item line start, clause)`:
    // the abort is needed only when the asserted fact fails.
    let len_zero = concat!("crate::sys::may_", "abort() || *len == 0,");
    let compound = [
        ("heap/mod.rs", "fn assert_flushed(", len_zero),
        ("heap/mod.rs", "fn assert_refill_empty(", len_zero),
        ("heap/mod.rs", "fn assert_rekey_flushed(", len_zero),
        (
            "vspec/check.rs",
            "pub fn check(",
            concat!("crate::sys::may_", "abort_corrupt() || c,"),
        ),
    ];
    let unfailing = [("heap/others.rs", "fn assert_not_home(")];
    let aborts = [
        concat!("abo", "rt("),
        concat!("abo", "rt!("),
        concat!("abo", "rt_"),
        "assert!(",
        "assert_eq!(",
        "assert_ne!(",
        "panic!(",
        "unreachable!(",
        "todo!(",
        "unimplemented!(",
        ".unwrap()",
        ".expect(",
        "trap()",
        "exit_group(",
        "\"ud2\"",
        "\"brk #",
    ];
    let (mut trusted, mut bad) = (0, Vec::new());
    let mut used = std::collections::BTreeSet::new();
    for (rel, text) in crate::testutil::shipped_sources() {
        if rel == "api/restated.rs" {
            continue;
        }
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (i, &t) in lines.iter().enumerate() {
            if t != concat!("#[verifier::", "external_body]") {
                continue;
            }
            // The item the attribute is on: the next line that is not
            // an attribute or a comment.
            let Some(f) = (i + 1..lines.len())
                .find(|&j| !lines[j].starts_with("#[") && !lines[j].starts_with("//"))
                .filter(|&j| crate::testutil::fn_item(lines[j]))
            else {
                continue;
            };
            // An exec function with a body: not a ghost item, and its
            // header ends at the body's `{`, not at a `;`.
            if ["proof ", "spec ", "axiom "]
                .iter()
                .any(|q| lines[f].contains(&format!("{q}fn ")))
            {
                continue;
            }
            let Some(b) = (f..lines.len()).find(|&j| {
                let l = lines[j];
                !l.starts_with("//") && (l == "{" || l.ends_with('{') || l.ends_with(';'))
            }) else {
                continue;
            };
            if lines[b].ends_with(';') {
                continue;
            }
            let mut depth = 0i32;
            let mut end = b;
            for (j, l) in lines.iter().enumerate().skip(b) {
                if !l.starts_with("//") {
                    depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                }
                end = j;
                if depth <= 0 {
                    break;
                }
            }
            let header = lines[f..b].join(" ");
            let body = lines[b..=end]
                .iter()
                .filter(|l| !l.starts_with("//"))
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
                .replace("debug_assert", "");
            let ends = lines[f..=b].join(" ").contains(") -> !");
            if !ends && !aborts.iter().any(|a| body.contains(a)) {
                continue;
            }
            trusted += 1;
            let at = format!("{rel}:{}: `{}`", f + 1, lines[f]);
            // Whether `l` names this function; its entry is marked used.
            let mut listed = |l: &[(&'static str, &'static str)]| {
                let hit = l.iter().find(|(p, s)| *p == rel && lines[f].starts_with(s));
                if let Some(&e) = hit {
                    used.insert(e);
                }
                hit.is_some()
            };
            // Each header line that names an abort capability: a bare
            // clause of `requires`, starting a clause, or the listed
            // compound clause of this function.
            let (mut kw, mut prev) = ("", "");
            let (mut granted, mut other) = (false, false);
            for &h in lines[f..b].iter().filter(|l| !l.starts_with("//")) {
                let word = h.split_whitespace().next();
                if let Some(k) = ["requires", "ensures", "recommends", "decreases", "returns"]
                    .into_iter()
                    .find(|k| word == Some(*k))
                {
                    kw = k;
                }
                if h.contains(cap.as_str()) || h.contains(valid.as_str()) {
                    let fresh = kw == "requires" && (prev == "requires" || prev.ends_with(','));
                    let ok = (fresh && h == format!("crate::sys::{cap}(),"))
                        || compound
                            .iter()
                            .any(|&(p, s, c)| h == c && listed(&[(p, s)]));
                    granted |= ok;
                    other |= !ok;
                }
                prev = h;
            }
            if !((granted && !other) || listed(&unfailing)) {
                bad.push(format!(
                    "{at}: a trusted body that can abort, without a bare `{cap}` clause \
                     or its listed compound one: `{header}`"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    for e in unfailing
        .into_iter()
        .chain(compound.map(|(p, s, _)| (p, s)))
    {
        assert!(
            used.contains(&e),
            "{e:?}: listed, but no trusted body that can abort"
        );
    }
    assert!(
        trusted > 5,
        "got = {trusted} trusted bodies that abort, want = the tree's"
    );
}

/// One fresh read/write private anonymous page, for this module's
/// tests.
fn map_page() -> usize {
    crate::testutil::map_rw(PAGE)
}

#[test]
fn mmap_mprotect_madvise_munmap() {
    let addr = map_page();
    assert_eq!(addr % PAGE, 0);
    // SAFETY: `addr` is our fresh read/write page.
    unsafe {
        let p = addr as *mut u8;
        p.write(0xA5);
        assert_eq!(p.read(), 0xA5);
        mprotect(addr, PAGE, PROT_READ).unwrap();
        assert_eq!(p.read(), 0xA5);
        mprotect(addr, PAGE, PROT_READ | PROT_WRITE).unwrap();
        madvise(addr, PAGE, MADV_DONTNEED).unwrap();
        assert_eq!(p.read(), 0, "MADV_DONTNEED must zero anonymous memory");
        munmap(addr, PAGE).unwrap();
    }
}

#[test]
fn mmap_errors_decode() {
    // SAFETY: invalid arguments never create a mapping.
    let r = unsafe { mmap(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
    assert_eq!(r, Err(Errno::EINVAL));
    // SAFETY: fd 12345 is not open; MAP_SHARED without ANONYMOUS needs one.
    let r = unsafe { mmap(0, PAGE, PROT_READ, MAP_SHARED, 12345, 0) };
    assert_eq!(r, Err(Errno::EBADF));
    // SAFETY: mprotect on an unmapped range changes nothing.
    let r = unsafe { mprotect(usize::MAX - 0xFFFF, PAGE, PROT_READ) };
    assert!(
        matches!(r, Err(Errno::ENOMEM) | Err(Errno::EINVAL)),
        "{r:?}"
    );
}

#[test]
fn mmap_fixed_noreplace_refuses_overlap() {
    let addr = map_page();
    // SAFETY: MAP_FIXED_NOREPLACE never clobbers; the kernel either
    // returns EEXIST or (pre-4.17) a different address.
    let r = unsafe {
        mmap(
            addr,
            PAGE,
            PROT_READ,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    match r {
        Err(Errno::EEXIST) => {}
        Ok(other) => {
            assert_ne!(other, addr);
            // SAFETY: we own `other`.
            unsafe { munmap(other, PAGE).unwrap() };
        }
        Err(e) => panic!("unexpected {e}"),
    }
    // SAFETY: we own `addr`.
    unsafe { munmap(addr, PAGE).unwrap() };
}

/// `block_all_signals` blocks every signal but glibc's `SIGCANCEL` (32)
/// and `SIGSETXID` (33): a thread with `SIGSETXID` blocked never answers
/// the broadcast of a `setuid`-family call, which then waits forever.
#[test]
fn block_all_signals_leaves_glibc_internal_signals_unblocked() {
    std::thread::spawn(|| {
        let before = block_all_signals(Tracked::assume_new());
        let during = block_all_signals(Tracked::assume_new());
        set_signal_mask(before, Tracked::assume_new());
        assert_eq!(during & (1 << 31), 0, "SIGCANCEL blocked: {during:#x}");
        assert_eq!(during & (1 << 32), 0, "SIGSETXID blocked: {during:#x}");
        for sig in [1u32, 2, 10, 15, 17, 31, 34, 64] {
            assert_ne!(during & (1 << (sig - 1)), 0, "signal {sig} not blocked");
        }
    })
    .join()
    .unwrap();
}

#[test]
fn mseal_blocks_munmap_when_supported() {
    let addr = map_page();
    // SAFETY: sealing a page we never reuse is harmless (it leaks one page
    // for the life of the test process).
    match unsafe { mseal(addr, PAGE, 0) } {
        Ok(()) => {
            // SAFETY: the kernel refuses; nothing changes.
            assert_eq!(unsafe { munmap(addr, PAGE) }, Err(Errno::EPERM));
            assert_eq!(
                // SAFETY: as above.
                unsafe { mprotect(addr, PAGE, PROT_NONE) },
                Err(Errno::EPERM)
            );
            // MADV_DONTNEED on a sealed writable anonymous mapping is
            // allowed; discarding is only refused for read-only ranges.
            // SAFETY: the test's own anonymous page; discarding only zeroes it.
            unsafe { madvise(addr, PAGE, MADV_DONTNEED).unwrap() };
        }
        Err(Errno::ENOSYS) => {
            crate::testutil::skip(
                "mseal_blocks_munmap_when_supported",
                "mseal: ENOSYS (a kernel before 6.10)",
            );
            // SAFETY: we own `addr`.
            unsafe { munmap(addr, PAGE).unwrap() };
        }
        Err(e) => panic!("mseal: unexpected {e}"),
    }
}

#[test]
fn vma_naming_shows_in_maps_when_supported() {
    let addr = map_page();
    // SAFETY: `addr` is our mapping and the name is static and NUL-terminated.
    match unsafe { set_vma_anon_name(addr, PAGE, b"challoc:test\0") } {
        Ok(()) => {
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            let line = maps
                .lines()
                .find(|l| l.starts_with(&format!("{addr:x}-")))
                .expect("mapping present in maps");
            assert!(line.contains("[anon:challoc:test]"), "{line}");
        }
        Err(Errno::EINVAL) => crate::testutil::skip(
            "vma_naming_shows_in_maps_when_supported",
            "PR_SET_VMA_ANON_NAME: EINVAL (a kernel without CONFIG_ANON_VMA_NAME)",
        ),
        Err(e) => panic!("prctl: unexpected {e}"),
    }
    // SAFETY: we own `addr`.
    unsafe { munmap(addr, PAGE).unwrap() };
    // Unknown prctl option must decode as EINVAL.
    // SAFETY: option 0 is invalid and has no effect.
    assert_eq!(unsafe { prctl(0, 0, 0, 0, 0) }, Err(Errno::EINVAL));
}

#[test]
fn getrandom_fills() {
    let mut a = [0u8; 64];
    let mut b = [0u8; 64];
    getrandom_exact(&mut a).unwrap();
    getrandom_exact(&mut b).unwrap();
    assert_ne!(a, [0u8; 64]);
    assert_ne!(a, b);
    assert_eq!(getrandom(&mut [], 0), Ok(0));
    assert_eq!(getrandom(&mut a, 0xFFFF_FFFF), Err(Errno::EINVAL));
}

#[test]
fn clocks_advance() {
    let a = clock_gettime(CLOCK_MONOTONIC).unwrap();
    assert!(a.nsec >= 0 && a.nsec < 1_000_000_000);
    let c = clock_gettime(CLOCK_MONOTONIC_COARSE).unwrap();
    assert!(c.nsec >= 0 && c.nsec < 1_000_000_000);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let b = clock_gettime(CLOCK_MONOTONIC).unwrap();
    assert!(b.as_nanos() > a.as_nanos());
    assert!(now_coarse_ns() > 0);
    assert_eq!(clock_gettime(12345), Err(Errno::EINVAL));
}

#[test]
fn files_open_read_close() {
    let fd = open(b"/proc/self/comm\0", O_RDONLY).unwrap();
    assert!(fd >= 0);
    let mut buf = [0u8; 64];
    let n = read_full(fd, &mut buf).unwrap();
    assert!(n > 0 && n < 64);
    assert_eq!(buf[n - 1], b'\n');
    close(fd).unwrap();
    assert_eq!(close(fd), Err(Errno::EBADF));
    assert_eq!(
        open(b"/nonexistent/challoc\0", O_RDONLY),
        Err(Errno::ENOENT)
    );
    // O_CLOEXEC is forced on.
    let fd = open(b"/dev/null\0", O_RDONLY).unwrap();
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
    let flags_line = info.lines().find(|l| l.starts_with("flags:")).unwrap();
    let oflags = usize::from_str_radix(flags_line.split_whitespace().nth(1).unwrap(), 8).unwrap();
    assert_ne!(oflags & (O_CLOEXEC as usize), 0, "{flags_line}");
    close(fd).unwrap();
}

/// The shared small-file reader: the count of a real file, a cut at
/// the buffer, `None` for an absent path and for a read that fails
/// (a directory opens but does not read).
#[test]
fn small_files_read_whole_cut_or_not_at_all() {
    let mut buf = [0u8; 64];
    let n = read_small_file(b"/proc/self/comm\0", &mut buf).unwrap();
    assert!(n > 0 && n < 64 && buf[n - 1] == b'\n', "{n}");
    assert_eq!(
        &buf[..n],
        std::fs::read("/proc/self/comm").unwrap().as_slice()
    );
    let mut two = [0u8; 2];
    assert_eq!(read_small_file(b"/proc/self/comm\0", &mut two), Some(2));
    assert_eq!(two, buf[..2]);
    assert_eq!(read_small_file(b"/nonexistent/challoc\0", &mut buf), None);
    assert_eq!(
        read_small_file(b"/proc\0", &mut buf),
        None,
        "a directory opens and fails to read"
    );
    assert_eq!(read_small_file(b"/dev/null\0", &mut buf), Some(0));
}

#[test]
fn write_to_devnull_and_bad_fd() {
    let fd = open(b"/dev/null\0", O_WRONLY).unwrap();
    assert_eq!(write(fd, b"hello"), Ok(5));
    write_all(fd, b"more");
    close(fd).unwrap();
    assert_eq!(write(-1, b"x"), Err(Errno::EBADF));
    // write_all must swallow errors rather than loop.
    write_all(-1, b"x");
}

#[test]
fn futex_wake_and_mismatch() {
    let word: u32 = 1;
    // SAFETY: `word` is a live aligned u32.
    unsafe {
        assert_eq!(futex_wake(&word, 1), Ok(0));
        assert_eq!(futex_wait(&word, 2, None), Err(Errno::EAGAIN));
        let ts = Timespec {
            sec: 0,
            nsec: 5_000_000,
        };
        // A stray signal (seen under emulation) legitimately yields EINTR;
        // retry until the timeout path is exercised.
        loop {
            match futex_wait(&word, 1, Some(&ts)) {
                Err(Errno::EINTR) => continue,
                r => {
                    assert_eq!(r, Err(Errno::ETIMEDOUT));
                    break;
                }
            }
        }
    }
}

#[test]
fn futex_wakes_a_sleeping_thread() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static WORD: AtomicU32 = AtomicU32::new(0);
    WORD.store(0, Ordering::SeqCst);
    let waiter = std::thread::spawn(|| {
        let ts = Timespec { sec: 5, nsec: 0 };
        loop {
            if WORD.load(Ordering::SeqCst) != 0 {
                return true;
            }
            // SAFETY: static aligned u32.
            match unsafe { futex_wait(WORD.as_ptr(), 0, Some(&ts)) } {
                Ok(()) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => {}
                Err(Errno::ETIMEDOUT) => return false,
                Err(e) => panic!("futex_wait: {e}"),
            }
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    WORD.store(1, Ordering::SeqCst);
    // SAFETY: static aligned u32.
    unsafe { futex_wake(WORD.as_ptr(), 1).unwrap() };
    assert!(waiter.join().unwrap(), "waiter timed out");
}

#[test]
fn pid_and_tid() {
    assert_eq!(getpid() as u32, std::process::id());
    assert!(gettid() > 0);
    // The main thread of a process has tid == pid; a spawned one does not.
    let other = std::thread::spawn(gettid).join().unwrap();
    assert_ne!(other, gettid());
}

#[test]
fn errno_display_from_syscall() {
    assert_eq!(format!("{}", close(-1).unwrap_err()), "EBADF");
}

#[test]
fn ncpus_counts_the_affinity_mask() {
    let n = ncpus();
    assert!(n >= 1);
    // The mask can only be narrower than the configured CPU count.
    // SAFETY: `sysconf` has no preconditions.
    let conf = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
    assert!(
        conf >= 1 && n as i64 <= conf,
        "ncpus {n}, _SC_NPROCESSORS_CONF {conf}"
    );
    // SAFETY: a zeroed `cpu_set_t` is a valid empty set; the call writes
    // at most `size_of::<cpu_set_t>()` bytes into it, and `CPU_COUNT`
    // only reads it.
    let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
    assert_eq!(
        // SAFETY: as above.
        unsafe { libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set) },
        0
    );
    // SAFETY: as above.
    assert_eq!(n as i32, unsafe { libc::CPU_COUNT(&set) });
}

/// The raw scheduling wrappers read what glibc's do: the affinity mask
/// of `sched_getaffinity` and the nice value of `getpriority`.
#[test]
fn sched_wrappers_match_libc() {
    let pid = getpid();
    let mut set: CpuSet = [0u64; 16];
    assert!(sched_getaffinity(pid, &mut set).is_ok());
    // SAFETY: a zeroed `cpu_set_t` is a valid empty set, and the call
    // writes at most `size_of::<cpu_set_t>()` bytes into it.
    let mut libc_set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
    let size = core::mem::size_of::<libc::cpu_set_t>();
    assert_eq!(size, core::mem::size_of::<CpuSet>());
    assert_eq!(
        // SAFETY: as above.
        unsafe { libc::sched_getaffinity(pid, size, &mut libc_set) },
        0
    );
    // SAFETY: both are `size` bytes of plain data.
    let libc_words: CpuSet = unsafe { core::mem::transmute(libc_set) };
    assert_eq!(set, libc_words);
    // SAFETY: `getpriority` has no memory preconditions.
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t) };
    assert_eq!(getpriority(pid), Ok(nice));
}

/// Cross-check `nr.rs` against the kernel headers via a tiny C program.
#[test]
fn syscall_numbers_match_kernel_headers() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = format!("{root}/tests/c/sysnr.c");
    let out = format!("{}/sysnr-{}", env::temp_dir().display(), std::process::id());
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let status = Command::new(&cc).args([&src, "-o", &out]).status().unwrap();
    assert!(status.success(), "compiling {src} failed");
    let output = Command::new(&out).output().unwrap();
    let _ = std::fs::remove_file(&out);
    assert!(output.status.success());
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let mut seen = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap();
        let value = parts.next().unwrap();
        let (_, ours) = super::nr::TABLE
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} printed by sysnr.c but not in nr.rs"));
        if value == "-" {
            std::eprintln!("sysnr: header lacks __NR_{name}, cannot cross-check");
        } else {
            assert_eq!(value.parse::<usize>().unwrap(), *ours, "__NR_{name}");
        }
        seen.push(std::string::String::from(name));
    }
    for (name, _) in super::nr::TABLE {
        assert!(
            seen.iter().any(|s| s == name),
            "{name} missing from sysnr.c"
        );
    }
}
