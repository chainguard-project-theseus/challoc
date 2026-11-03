//! The source-wide facts that the weak-memory arguments of the seams
//! citing loom models rest on (verus/SEAMS.md, "Status", and the comments
//! above those seams in verus/ledger/src/**/*.overrides): which lines of
//! challoc's shipped source write the atomics a model stands for, and that
//! each permission type is minted once per atomic. A permission's value
//! reads as its atomic's last write only if every write goes through the
//! wrappers that update the permission.
//!
//! The checks match rustfmt-formatted source text, test and Kani code left
//! out (`#[cfg(test)]`, `#[cfg(kani)]` and `#[cfg(all(test, ..))]` items,
//! `src/kani/`, and the `tests.rs`, `linux_tests.rs` and `testutil.rs`
//! files `verus/seams.sh` leaves out too): a write through a pointer or a
//! name they do not look for is caught only by review. Each check has a
//! control that adds one line it must reject.

use std::collections::BTreeMap;
use std::path::Path;

/// `src/` as `(path under the repository root, text)`, in path order.
type Tree = BTreeMap<String, String>;

fn walk(root: &Path, dir: &Path, out: &mut Tree) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        let rel = p.strip_prefix(root).unwrap().to_str().unwrap().to_string();
        if p.is_dir() {
            if rel != "src/kani" {
                walk(root, &p, out);
            }
        } else if rel.ends_with(".rs")
            && !["tests.rs", "linux_tests.rs", "testutil.rs"]
                .iter()
                .any(|f| rel.ends_with(&format!("/{f}")))
        {
            out.insert(rel, std::fs::read_to_string(&p).unwrap());
        }
    }
}

fn tree() -> Tree {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut t = Tree::new();
    walk(&root, &root.join("src"), &mut t);
    assert!(
        t.contains_key("src/lib.rs"),
        "src/lib.rs: got = missing, want = the tree"
    );
    t
}

/// `t` with `line` inserted after the first line of `file` that reads
/// `after` (both trimmed), for a control.
fn with(t: &Tree, file: &str, after: &str, line: &str) -> Tree {
    let text = &t[file];
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    let mut done = false;
    for l in text.lines() {
        out.push_str(l);
        out.push('\n');
        if !done && l.trim() == after {
            out.push_str(line);
            out.push('\n');
            done = true;
        }
    }
    assert!(
        done,
        "{file}: got = no line {after:?}, want = the control's anchor"
    );
    let mut t = t.clone();
    t.insert(file.to_string(), out);
    t
}

/// One line of shipped code: its number, its code (string and char
/// literal contents blanked, comments removed, trimmed), the innermost
/// `fn` it is in, and whether it is ghost code (inside a `spec fn`, a
/// `proof fn` or a `proof { .. }` block).
struct Line {
    n: usize,
    code: String,
    func: String,
    ghost: bool,
}

/// Whether `t`, a trimmed code line, starts a `fn` item.
fn fn_item(t: &str) -> Option<&str> {
    let mut t = t;
    loop {
        let rest = [
            "pub ",
            "const ",
            "unsafe ",
            "extern \"\" ",
            "proof ",
            "spec ",
            "exec ",
            "open ",
            "closed ",
            "broadcast ",
            "axiom ",
        ]
        .iter()
        .find_map(|q| t.strip_prefix(q))
        .or_else(|| {
            t.strip_prefix("pub(")
                .and_then(|r| r.split_once(") ").map(|(_, r)| r))
        });
        match rest {
            Some(r) => t = r,
            None => {
                let name = t.strip_prefix("fn ")?;
                return Some(&name[..name.find(['(', '<']).unwrap_or(name.len())]);
            }
        }
    }
}

/// Whether a `#[cfg(..)]` line keeps its item out of the shipped library.
fn unshipped(t: &str) -> bool {
    t == "#[cfg(test)]" || t == "#[cfg(kani)]" || t.starts_with("#[cfg(all(test,")
}

/// The code of each line of `text`, with string and char literal contents
/// blanked and comments removed (a `/* .. */` comment and a string may
/// span lines).
fn code_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut in_str, mut raw, mut block) = (false, None::<usize>, 0usize);
    for l in text.lines() {
        let c: Vec<char> = l.chars().collect();
        let mut s = String::with_capacity(l.len());
        let mut i = 0;
        while i < c.len() {
            if block > 0 {
                if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    block -= 1;
                    i += 1;
                } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    block += 1;
                    i += 1;
                }
                i += 1;
                continue;
            }
            if let Some(h) = raw {
                if c[i] == '"' && (1..=h).all(|k| c.get(i + k) == Some(&'#')) {
                    s.push('"');
                    raw = None;
                    i += h;
                }
                i += 1;
                continue;
            }
            if in_str {
                if c[i] == '\\' {
                    i += 2;
                    continue;
                }
                if c[i] == '"' {
                    s.push('"');
                    in_str = false;
                }
                i += 1;
                continue;
            }
            let prev_ident = i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_');
            match c[i] {
                '/' if c.get(i + 1) == Some(&'/') => break,
                '/' if c.get(i + 1) == Some(&'*') => {
                    block = 1;
                    i += 2;
                    continue;
                }
                '"' => {
                    s.push('"');
                    in_str = true;
                }
                'r' if !prev_ident && matches!(c.get(i + 1), Some('"' | '#')) => {
                    let h = c[i + 1..].iter().take_while(|&&x| x == '#').count();
                    if c.get(i + 1 + h) == Some(&'"') {
                        s.push_str("r\"");
                        raw = Some(h);
                        i += h + 2;
                        continue;
                    }
                    s.push('r');
                }
                '\'' if c.get(i + 1) == Some(&'\\') => {
                    let end = (i + 3..c.len())
                        .find(|&k| c[k] == '\'')
                        .unwrap_or(c.len() - 1);
                    s.push_str("' '");
                    i = end + 1;
                    continue;
                }
                '\'' if c.get(i + 2) == Some(&'\'') => {
                    s.push_str("' '");
                    i += 3;
                    continue;
                }
                ch => s.push(ch),
            }
            i += 1;
        }
        out.push(s.trim().to_string());
    }
    out
}

/// The shipped lines of `text` (blank lines left out), each with its fn
/// and whether it is ghost code. Items are bounded by their braces.
fn shipped(text: &str) -> Vec<Line> {
    let code = code_lines(text);
    let mut out = Vec::new();
    let mut depth = 0i64;
    // The depths at which an unshipped item, a ghost region and each
    // enclosing fn opened.
    let mut skip: Option<i64> = None;
    let mut ghost: Option<i64> = None;
    let mut fns: Vec<(String, i64)> = Vec::new();
    let mut pending_skip = false;
    let mut pending_fn: Option<(String, bool)> = None;
    for (i, t) in code.iter().enumerate() {
        let opens = t.matches('{').count() as i64;
        let closes = t.matches('}').count() as i64;
        let start = depth;
        if skip.is_none() && unshipped(t) {
            pending_skip = true;
        }
        let attr = t.starts_with("#[") || t.starts_with("#!");
        if pending_skip && !attr && !t.is_empty() {
            pending_skip = false;
            if opens > closes {
                skip = Some(start);
            } else if !t.ends_with(';') && opens == 0 {
                // A signature whose body opens on a later line, or a
                // `static` or `const` whose initialiser ends on one.
                skip = Some(start);
            }
            depth += opens - closes;
            if skip == Some(start) && depth <= start && opens > 0 {
                skip = None;
            }
            continue;
        }
        if let Some(name) = fn_item(t) {
            let g = t.contains("spec fn ") || t.contains("proof fn ");
            pending_fn = Some((name.to_string(), g));
        }
        if skip.is_none() && !t.is_empty() && !attr {
            let func = pending_fn
                .as_ref()
                .map(|(f, _)| f.clone())
                .or_else(|| fns.last().map(|(f, _)| f.clone()))
                .unwrap_or_default();
            let g =
                ghost.is_some() || pending_fn.as_ref().is_some_and(|(_, g)| *g) || t == "proof {";
            out.push(Line {
                n: i + 1,
                code: t.clone(),
                func,
                ghost: g,
            });
        }
        if opens > 0 {
            if let Some((name, g)) = pending_fn.take() {
                fns.push((name, start));
                if g && ghost.is_none() {
                    ghost = Some(start);
                }
            } else if t == "proof {" && ghost.is_none() {
                ghost = Some(start);
            }
        } else if t.ends_with(';') && pending_fn.is_some() && !t.contains('(') {
            pending_fn = None;
        }
        depth += opens - closes;
        if skip.is_some_and(|d| depth <= d) && (closes > 0 || t.ends_with(';')) {
            skip = None;
        }
        if ghost.is_some_and(|d| depth <= d) && closes > 0 {
            ghost = None;
        }
        while fns.last().is_some_and(|(_, d)| depth <= *d) && closes > 0 {
            fns.pop();
        }
    }
    out
}

/// Whether `code` calls a method that writes an atomic: `.store(`,
/// `.swap(`, `.compare_exchange(`, `.compare_exchange_weak(` or
/// `.fetch_<op>(`.
fn writes(code: &str) -> bool {
    [
        ".store(",
        ".swap(",
        ".compare_exchange(",
        ".compare_exchange_weak(",
    ]
    .iter()
    .any(|m| code.contains(m))
        || code.match_indices(".fetch_").any(|(i, m)| {
            let rest = &code[i + m.len()..];
            let k = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
                .unwrap_or(rest.len());
            k > 0 && rest[k..].starts_with('(')
        })
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The byte offsets where `name` occurs in `code` with no identifier
/// character before it, and after it none and none of `not_after`.
fn occurs(code: &str, name: &str, not_after: &[char]) -> Vec<usize> {
    code.match_indices(name)
        .filter(|(i, _)| {
            let before = code[..*i].chars().next_back();
            let after = code[i + name.len()..].chars().next();
            let first = name.chars().next().unwrap();
            let last = name.chars().next_back().unwrap();
            (!ident(first) || !before.is_some_and(ident))
                && (!ident(last) || !after.is_some_and(|c| ident(c) || not_after.contains(&c)))
        })
        .map(|(i, _)| i)
        .collect()
}

fn calls(code: &str, f: &str) -> bool {
    !occurs(code, &format!("{f}("), &[]).is_empty()
}

fn report(what: &str, bad: Vec<String>) {
    assert!(
        bad.is_empty(),
        "{what}: got = these lines:\n  {}\nwant = none (the seams' arguments in verus/ledger rely on it)",
        bad.join("\n  ")
    );
}

fn bad(file: &str, l: &Line) -> String {
    format!("{file}:{}: {}", l.n, l.code)
}

// The bitmap words (`vspec/word.rs`'s seams): non-test bitmap.rs writes a
// word only through `vspec::word`'s wrappers, and class/table.rs, which
// casts word addresses to `&AtomicU64`, calls no atomic write method but
// the owner word's store in `store_owner` (once; it writes the watermark
// and its copy through `vspec::atom`'s wrappers).

fn check_words(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for l in shipped(&t["src/bitmap.rs"]) {
        if !l.ghost && writes(&l.code) && !l.code.contains("crate::vspec::word::") {
            out.push(bad("src/bitmap.rs", &l));
        }
    }
    let mut owner = 0;
    for l in shipped(&t["src/class/table.rs"]) {
        if l.ghost || !writes(&l.code) {
            continue;
        }
        if l.func == "store_owner" && l.code == "unsafe { (*w).store(arena, Ordering::Relaxed) }" {
            owner += 1;
        } else {
            out.push(bad("src/class/table.rs", &l));
        }
    }
    if owner != 1 {
        out.push(format!(
            "src/class/table.rs: {owner} owner word stores in store_owner, not 1"
        ));
    }
    out
}

#[test]
fn access_bitmap_words_only_through_vspec_word() {
    report("word writes", check_words(&tree()));
}

#[test]
fn access_bitmap_words_control() {
    let t = tree();
    let anchor = "crate::vspec::word::fetch_or(w, pad, Ordering::Relaxed, Tracked(&mut perm));";
    assert!(
        !check_words(&with(
            &t,
            "src/bitmap.rs",
            anchor,
            "w.store(0, Ordering::Relaxed);"
        ))
        .is_empty()
    );
    let anchor = "unsafe { (*w).store(arena, Ordering::Relaxed) }";
    assert!(
        !check_words(&with(
            &t,
            "src/class/table.rs",
            anchor,
            "w.fetch_or(1, Ordering::Relaxed);"
        ))
        .is_empty()
    );
}

// The watermark and its class-line copy (`vspec/atom.rs`'s seams,
// `class::table::store_owner`): nothing writes `committed_slabs` by a
// method, non-test heap/shared.rs writes no class line's `committed`
// directly, only class/table.rs (and vspec/atom.rs) calls the wrappers,
// only heap/shared.rs makes a copy's pointer (`usize_ptr`), and only
// class/table.rs names the owner words' invariants (`owner_invs`), which
// hold their permissions.

const ATOM: &[&str] = &[
    "new_usize",
    "load_usize",
    "load_usize_seen",
    "load_u16_at",
    "store_usize",
    "store_usize_at",
];

fn check_watermark(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for (file, text) in t {
        for l in shipped(text) {
            let c = &l.code;
            let wm = occurs(c, "committed_slabs", &[]).into_iter().any(|i| {
                let rest = &c[i + "committed_slabs".len()..];
                rest.starts_with('.') && writes(rest)
            });
            let copy = file == "src/heap/shared.rs"
                && occurs(c, ".committed", &[]).into_iter().any(|i| {
                    let rest = &c[i + ".committed".len()..];
                    rest.starts_with('.') && writes(rest)
                });
            let atom = file != "src/class/table.rs"
                && file != "src/vspec/atom.rs"
                && ATOM.iter().any(|f| calls(c, f));
            let ptr = file != "src/heap/shared.rs"
                && file != "src/vspec/atom.rs"
                && calls(c, "usize_ptr");
            let invs = file != "src/class/table.rs" && !occurs(c, "owner_invs", &[]).is_empty();
            if wm || copy || atom || ptr || invs {
                out.push(bad(file, &l));
            }
        }
    }
    out
}

#[test]
fn access_watermark_only_through_vspec_atom() {
    report("watermark accesses", check_watermark(&tree()));
}

#[test]
fn access_watermark_control() {
    let t = tree();
    let anchor = "use crate::vspec::flags::RouteByte;";
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        anchor,
        "fn f(a: &A) { crate::vspec::atom::store_usize(a, 0, R, Tracked(p)); }",
    );
    assert!(!check_watermark(&t2).is_empty());
    let anchor = "unsafe { (*w).store(arena, Ordering::Relaxed) }";
    let t2 = with(
        &t,
        "src/class/table.rs",
        anchor,
        "self.committed_slabs.store(0, Ordering::Release);",
    );
    assert!(!check_watermark(&t2).is_empty());
}

// The counters (`vspec/count.rs`'s seams): arena.rs names the bound counts
// (`bound`) and the arena count (`self.count`), and policy.rs the position
// (`scaled16`), only in a `vspec::count` wrapper call, the spec `count_id`
// and ghost `assert`/`forall` lines, and moves a count into place raw
// only where `Arenas::put_head` and `Arenas::put_entry` write the atomic
// `vspec::count::new` made.

/// `(fn, line)`: the raw writes of a count `vspec::count::new` made.
const RAW_COUNTS: &[(&str, &str)] = &[
    (
        "put_head",
        "ptr_mut_write(field_at(this, AR_COUNT), Tracked(&mut parts.count), c);",
    ),
    (
        "put_entry",
        "ptr_mut_write(elem_at(bound, i, MAX_ARENAS), Tracked(b), c);",
    ),
];

fn check_counts(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for l in shipped(&t["src/arena.rs"]) {
        let c = &l.code;
        if l.ghost || c.contains("crate::vspec::count::") || c.contains("count_id(") {
            continue;
        }
        let bound = !occurs(c, ".bound", &[]).is_empty() || c.contains("(*bound)");
        let count = !occurs(c, "self.count", &['(']).is_empty() && !c.contains("self.count.ptr()");
        // The raw writes that move a minted count into place.
        let raw = (c.contains("elem_at(bound,") || c.contains("field_at(this, AR_COUNT)"))
            && !RAW_COUNTS.contains(&(l.func.as_str(), c.as_str()));
        if (bound && !c.contains("assert(") && !c.contains("forall|")) || count || raw {
            out.push(bad("src/arena.rs", &l));
        }
    }
    for l in shipped(&t["src/policy.rs"]) {
        let c = &l.code;
        if !l.ghost
            && !occurs(c, ".scaled16", &[]).is_empty()
            && !c.contains("crate::vspec::count::")
            && !c.contains("count_id(")
        {
            out.push(bad("src/policy.rs", &l));
        }
    }
    out
}

#[test]
fn access_counts_only_through_vspec_count() {
    report("counter accesses", check_counts(&tree()));
}

#[test]
fn access_counts_control() {
    let t = tree();
    let anchor = "let (c, Tracked(cperm)) = crate::vspec::count::new(0);";
    assert!(
        !check_counts(&with(
            &t,
            "src/arena.rs",
            anchor,
            "self.bound[0].store(1, Ordering::Relaxed);"
        ))
        .is_empty()
    );
    assert!(
        !check_counts(&with(
            &t,
            "src/arena.rs",
            anchor,
            "self.count.fetch_add(1, Ordering::Relaxed);"
        ))
        .is_empty()
    );
    assert!(
        !check_counts(&with(
            &t,
            "src/arena.rs",
            anchor,
            "ptr_mut_write(elem_at(bound, 0, MAX_ARENAS), Tracked(b), c);"
        ))
        .is_empty()
    );
}

// The entry tables (`vspec/entry.rs`'s seams): arena.rs and heap/tcaches.rs
// each write an entry of their table only through the wrapper calls
// `ENTRY_TABLES` lists, each in the functions it names (an
// `elem_at(table, ..)` argument of one), or their `put_entry`'s write of
// the atomic `vspec::entry::new` made; no `.table[..]` method writes; and
// no other file calls `vspec::entry`.

/// Each table's file, its `put_entry` write line, and the wrapper calls
/// it makes as `(wrapper, function)`.
type EntryTable = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const ENTRY_TABLES: &[EntryTable] = &[
    (
        "src/arena.rs",
        "ptr_mut_write(elem_at(table, i, MAX_ARENAS), Tracked(t), e);",
        &[("store", "put_arena0"), ("store", "build")],
    ),
    (
        "src/heap/tcaches.rs",
        "ptr_mut_write(elem_at(table, i, MAX_TCACHES), Tracked(t), e);",
        &[("store", "create"), ("swap", "destroy")],
    ),
];

/// The writing wrappers of `vspec/entry.rs`.
const ENTRY_WRITES: &[&str] = &["store", "swap"];

fn check_entries(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    let call = |w: &str| format!("crate::vspec::entry::{w}(");
    let wrapped = |c: &str| ENTRY_WRITES.iter().any(|w| c.contains(&call(w)));
    let opens = |c: &str| ENTRY_WRITES.iter().any(|w| c.ends_with(&call(w)));
    for (file, put, allowed) in ENTRY_TABLES {
        let lines = shipped(&t[*file]);
        for (k, l) in lines.iter().enumerate() {
            let c = &l.code;
            if l.ghost {
                continue;
            }
            let stray = ENTRY_WRITES.iter().any(|w| {
                c.contains(&call(w)) && !allowed.iter().any(|(a, f)| a == w && l.func == *f)
            });
            let table_write = c.match_indices(".table[").any(|(i, _)| {
                let rest = &c[i..];
                rest.find("].").is_some_and(|j| writes(&rest[j + 1..]))
            });
            let elem = c.contains("elem_at(table,")
                && !wrapped(c)
                && !(k > 0 && opens(&lines[k - 1].code))
                && !(l.func == "put_entry" && c == put);
            if stray || table_write || elem {
                out.push(bad(file, l));
            }
        }
    }
    for (file, text) in t {
        if file == "src/vspec/entry.rs" || ENTRY_TABLES.iter().any(|(f, _, _)| f == file) {
            continue;
        }
        for l in shipped(text) {
            if l.code.contains("vspec::entry::") {
                out.push(bad(file, &l));
            }
        }
    }
    out
}

#[test]
fn access_entry_tables_only_through_vspec_entry() {
    report("entry table writes", check_entries(&tree()));
}

#[test]
fn access_entry_tables_control() {
    let t = tree();
    let anchor = "let (c, Tracked(cperm)) = crate::vspec::count::new(0);";
    let t2 = with(
        &t,
        "src/arena.rs",
        anchor,
        "ptr_mut_write(elem_at(table, 0, MAX_ARENAS), Tracked(t), e);",
    );
    assert!(!check_entries(&t2).is_empty());
    let t2 = with(
        &t,
        "src/arena.rs",
        anchor,
        "self.table[0].store(p, Ordering::Relaxed);",
    );
    assert!(!check_entries(&t2).is_empty());
    let t2 = with(
        &t,
        "src/arena.rs",
        anchor,
        "crate::vspec::entry::swap(&self.table[0], p, Ordering::AcqRel, Tracked(e));",
    );
    assert!(!check_entries(&t2).is_empty());
    let anchor = "let g = self.lock.lock(Tracked(ord), Tracked(&mut under));";
    let t2 = with(
        &t,
        "src/heap/tcaches.rs",
        anchor,
        "ptr_mut_write(elem_at(table, 0, MAX_TCACHES), Tracked(t), e);",
    );
    assert!(!check_entries(&t2).is_empty());
    let t2 = with(
        &t,
        "src/heap/tcaches.rs",
        anchor,
        "self.table[0].swap(ptr::null_mut(), Ordering::AcqRel);",
    );
    assert!(!check_entries(&t2).is_empty());
    let t2 = with(
        &t,
        "src/heap/tcaches.rs",
        anchor,
        "crate::vspec::entry::swap(&self.table[0], ptr::null_mut(), Ordering::AcqRel, Tracked(e));",
    );
    assert!(!check_entries(&t2).is_empty());
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        "use crate::vspec::flags::RouteByte;",
        "fn f(a: &A) { crate::vspec::entry::swap(a, p, R, Tracked(e)); }",
    );
    assert!(!check_entries(&t2).is_empty());
}

// The descriptor fields a permission records (`SlabDesc`'s seams):
// non-test slab.rs names `state`, `shadow`, `arena` and `dirty_at` (private
// fields) only in a `Relaxed` load, ghost code, or the seven write lines of
// `init`, `set_state` and `set_dirty_at`, each once; and no line starts a
// split write chain, takes a field's address or borrow, builds a
// `SlabDesc`, or writes through `as *mut SlabDesc`, `write_bytes`,
// `write_volatile`, `addr_of` or `transmute`.

const DESC_WRITES: &[(&str, &str)] = &[
    (
        "init",
        "self.state.store(State::Fresh as u8, Ordering::Relaxed);",
    ),
    (
        "init",
        "self.shadow.store(State::Fresh as u8 + 1, Ordering::Relaxed);",
    ),
    ("init", "self.arena.store(arena, Ordering::Relaxed);"),
    ("init", "self.dirty_at.store(0, Ordering::Relaxed);"),
    ("set_state", "self.state.store(s as u8, Ordering::Relaxed);"),
    (
        "set_state",
        "self.shadow.store(s as u8 + 1, Ordering::Relaxed);",
    ),
    (
        "set_dirty_at",
        "self.dirty_at.store(ns, Ordering::Relaxed);",
    ),
];

fn check_desc(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = vec![0; DESC_WRITES.len()];
    for l in shipped(&t["src/slab.rs"]) {
        let c = &l.code;
        if l.ghost {
            continue;
        }
        let split = [
            ".store(",
            ".swap(",
            ".fetch_",
            ".compare_exchange",
            ".as_ptr(",
            ".get_mut(",
            ".into_inner(",
        ]
        .iter()
        .any(|m| c.starts_with(m));
        let raw = [
            "addr_of",
            "write_bytes",
            "write_volatile",
            "as *mut SlabDesc",
            "transmute",
        ]
        .iter()
        .any(|m| c.contains(m));
        let built = !occurs(c, "SlabDesc", &[])
            .into_iter()
            .all(|i| !c[i + "SlabDesc".len()..].trim_start().starts_with('{'))
            && c != "pub struct SlabDesc {"
            && c != "impl SlabDesc {";
        let mut field = false;
        for f in [".state", ".shadow", ".arena", ".dirty_at"] {
            for i in occurs(c, f, &['(']) {
                let rest = &c[i + f.len()..];
                let borrowed = c[..i].rfind('&').is_some_and(|b| {
                    c[b + 1..i]
                        .trim_start_matches("mut ")
                        .chars()
                        .all(|ch| ident(ch) || ".()*".contains(ch))
                });
                if borrowed {
                    field = true;
                } else if rest.starts_with(".load(Ordering::Relaxed)") {
                } else if writes(rest) && rest.starts_with('.') {
                    match DESC_WRITES
                        .iter()
                        .position(|(func, w)| *func == l.func && *w == c)
                    {
                        Some(k) => seen[k] += 1,
                        None => field = true,
                    }
                } else {
                    field = true;
                }
            }
        }
        if split || raw || built || field {
            out.push(bad("src/slab.rs", &l));
        }
    }
    for (k, (func, w)) in DESC_WRITES.iter().enumerate() {
        if seen[k] != 1 {
            out.push(format!(
                "src/slab.rs: {} writes in {func}, not 1: {w}",
                seen[k]
            ));
        }
    }
    out
}

#[test]
fn access_desc_fields_only_in_accessors() {
    report("descriptor field accesses", check_desc(&tree()));
}

#[test]
fn access_desc_fields_control() {
    let t = tree();
    let anchor = "self.dirty_at.store(ns, Ordering::Relaxed);";
    assert!(
        !check_desc(&with(
            &t,
            "src/slab.rs",
            anchor,
            "self.state.store(0, Ordering::Relaxed);"
        ))
        .is_empty()
    );
    assert!(!check_desc(&with(&t, "src/slab.rs", anchor, "let p = &self.arena;")).is_empty());
    assert!(
        !check_desc(&with(
            &t,
            "src/slab.rs",
            anchor,
            "self.dirty_at.store(ns, Ordering::Relaxed);"
        ))
        .is_empty()
    );
}

// One permission per atomic (the count, watermark, phase and entry
// wrappers' seams): each file builds its permission type only in its
// minting fn, with one literal, and names the type only in the struct
// and impl lines and in type position (no `Self`, derive, trait impl or
// child module); and no `const` item copies a minted atomic (each use of
// such a const would be a fresh atomic with a copy of its permission).

const PERMS: &[(&str, &str, &str)] = &[
    ("src/vspec/count.rs", "CountPerm", "new"),
    ("src/vspec/atom.rs", "UsizePerm", "new_usize"),
    ("src/vspec/byte.rs", "BytePerm", "new"),
    ("src/vspec/entry.rs", "EntryPerm", "new"),
];

const MINTERS: &[&str] = &[
    "count::new",
    "new_usize",
    "Policy::new",
    "byte::new",
    "entry::new",
    "Deferred::now",
    "Deferred::later",
];

fn check_perms(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for (file, ty, mint) in PERMS {
        let mut literals = 0;
        for l in shipped(&t[*file]) {
            let c = &l.code;
            // The type's own lines. `Send` is a marker trait, which copies
            // nothing (`EntryPerm` has it for the arena and tcache tables' invariants).
            if *c == format!("pub tracked struct {ty} {{")
                || *c == format!("pub tracked struct {ty}<T> {{")
                || *c == format!("impl {ty} {{")
                || *c == format!("impl<T> {ty}<T> {{")
                || *c == format!("unsafe impl<T> Send for {ty}<T> {{")
            {
                continue;
            }
            if !occurs(c, "Self", &[':']).is_empty()
                || c.contains("derive")
                || c.starts_with("mod ")
                || c.starts_with("pub mod ")
                || !occurs(c, &format!("for {ty}"), &[]).is_empty()
            {
                out.push(bad(file, &l));
                continue;
            }
            for i in occurs(c, ty, &[]) {
                let rest = c[i + ty.len()..].trim_start();
                if rest.starts_with('{') && l.func == *mint {
                    literals += 1;
                } else if !rest.starts_with(['>', ')', ',', '|', ']', '<']) {
                    out.push(bad(file, &l));
                }
            }
        }
        if literals != 1 {
            out.push(format!("{file}: {literals} {ty} literals in {mint}, not 1"));
        }
    }
    for (file, text) in t {
        let mut in_const = false;
        for l in shipped(text) {
            let c = &l.code;
            let item = c
                .trim_start_matches("pub ")
                .trim_start_matches("pub(crate) ");
            if item.starts_with("const ")
                && item["const ".len()..]
                    .starts_with(|ch: char| ch.is_ascii_uppercase() || ch == '_')
            {
                in_const = true;
            }
            if in_const && MINTERS.iter().any(|m| calls(c, m)) {
                out.push(bad(file, &l));
            }
            if c.ends_with(';') {
                in_const = false;
            }
        }
    }
    out
}

#[test]
fn access_one_permission_per_atomic() {
    report("permission mints", check_perms(&tree()));
}

#[test]
fn access_one_permission_per_atomic_control() {
    let t = tree();
    let anchor = "(c, Tracked(CountPerm { id: count_id(&c), value: v }))";
    let t2 = with(
        &t,
        "src/vspec/count.rs",
        anchor,
        "(c, Tracked(CountPerm { id: count_id(&c), value: v }))",
    );
    assert!(!check_perms(&t2).is_empty());
    let anchor = "use crate::vspec::flags::RouteByte;";
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        anchor,
        "const C: (AtomicU32, Tracked<CountPerm>) = crate::vspec::count::new(0);",
    );
    assert!(!check_perms(&t2).is_empty());
}

// The init-once words (`api::state_*`, `api::line_*`): `STATE` has two
// writes in shipped code, `state_claim`'s and `state_publish`'s;
// `LINE_STATE` has two, `line_compare_exchange`'s and `line_store`'s, and no
// other file names it.

fn check_once(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    let mut state = Vec::new();
    let mut line = Vec::new();
    for (file, text) in t {
        for l in shipped(text) {
            let c = &l.code;
            let w = |name: &str| {
                occurs(c, name, &[]).into_iter().any(|i| {
                    let rest = &c[i + name.len()..];
                    rest.starts_with('.') && writes(rest)
                })
            };
            if w("STATE") {
                state.push((file.clone(), l.func.clone()));
            }
            if w("LINE_STATE") {
                line.push((file.clone(), l.func.clone()));
            }
            if file != "src/api/mod.rs" && !occurs(c, "LINE_STATE", &[]).is_empty() {
                out.push(bad(file, &l));
            }
        }
    }
    let api = |f: &str| ("src/api/mod.rs".to_string(), f.to_string());
    if state != [api("state_claim"), api("state_publish")] {
        out.push(format!(
            "STATE writes: got = {state:?}, want = state_claim's and state_publish's"
        ));
    }
    if line != [api("line_compare_exchange"), api("line_store")] {
        out.push(format!(
            "LINE_STATE writes: got = {line:?}, want = line_compare_exchange's and line_store's"
        ));
    }
    out
}

#[test]
fn access_init_once_words_written_twice() {
    report("init-once writes", check_once(&tree()));
}

#[test]
fn access_init_once_words_control() {
    let t = tree();
    let anchor = "use crate::vspec::flags::RouteByte;";
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        anchor,
        "fn f() { crate::api::STATE.store(0, Ordering::Relaxed); }",
    );
    assert!(!check_once(&t2).is_empty());
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        anchor,
        "fn f() -> u8 { LINE_STATE.load(Ordering::Relaxed) }",
    );
    assert!(!check_once(&t2).is_empty());
}

// The deferred cgroup record's phase (`vspec/byte.rs`'s seams): non-test
// sys/cgroup.rs writes no `.phase` by a method, and no file but it calls
// `vspec::byte`.

fn check_phase(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for (file, text) in t {
        for l in shipped(text) {
            let c = &l.code;
            let phase = file == "src/sys/cgroup.rs"
                && occurs(c, ".phase", &[]).into_iter().any(|i| {
                    let rest = &c[i + ".phase".len()..];
                    rest.starts_with('.') && writes(rest)
                });
            let other = file != "src/sys/cgroup.rs"
                && file != "src/vspec/byte.rs"
                && (c.contains("vspec::byte::")
                    || [
                        "byte::new",
                        "byte::load",
                        "byte::store",
                        "byte::compare_exchange",
                    ]
                    .iter()
                    .any(|f| calls(c, f)));
            if phase || other {
                out.push(bad(file, &l));
            }
        }
    }
    out
}

#[test]
fn access_deferred_phase_only_through_vspec_byte() {
    report("phase accesses", check_phase(&tree()));
}

#[test]
fn access_deferred_phase_control() {
    let t = tree();
    let anchor = "use crate::vspec::byte::BytePerm;";
    let t2 = with(
        &t,
        "src/sys/cgroup.rs",
        anchor,
        "fn f(d: &D) { d.phase.store(0, Ordering::Relaxed); }",
    );
    assert!(!check_phase(&t2).is_empty());
    let anchor = "use crate::vspec::flags::RouteByte;";
    let t2 = with(
        &t,
        "src/heap/shared.rs",
        anchor,
        "use crate::vspec::byte::BytePerm;",
    );
    assert!(!check_phase(&t2).is_empty());
}

// The free path's route byte (`vspec/flags.rs`'s seams; its `AtomicU8` is
// private to that file, so rustc confines every atomic operation on it to
// the wrappers): outside vspec/flags.rs the wrappers are called once each,
// in heap/shared.rs, on the lines below; the one write through the field's
// offset (`S_FREE_FLAGS`) writes the byte `flags::new` made; and no line
// names `free_flags` or `RouteByte` with a raw pointer, transmute, zeroing,
// `mem::swap`/`replace`/`take` or `ptr::write`/`replace`/`swap`/`read`,
// assigns or `&mut`-borrows a `free_flags` field, calls a wrapper by a
// shorter path, or imports from vspec/flags.rs anything but `RouteByte`.

const ROUTE_NEW: &str =
    "ptr_mut_write(field_at(this, S_FREE_FLAGS), Tracked(pa), crate::vspec::flags::new(flags));";
const ROUTE_CALLS: &[&str] = &[
    ROUTE_NEW,
    "crate::vspec::flags::load(&self.free_flags, Ordering::Relaxed)",
    "crate::vspec::flags::fetch_or(&self.free_flags, FREE_ROUTE, Ordering::Relaxed);",
];

fn check_route(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = [0; 3];
    let mut offset_writes = 0;
    for (file, text) in t {
        if file == "src/vspec/flags.rs" {
            continue;
        }
        for l in shipped(text) {
            let c = &l.code;
            let names = c.contains("free_flags") || c.contains("RouteByte");
            let raw = names
                && [
                    "addr_of",
                    "&raw ",
                    "as *mut",
                    "transmute",
                    "write_bytes",
                    "zeroed",
                    "MaybeUninit",
                    "mem::swap",
                    "mem::replace",
                    "mem::take",
                    "ptr::write",
                    "ptr::replace",
                    "ptr::swap",
                    "ptr::read",
                ]
                .iter()
                .any(|m| c.contains(m));
            let assigned = c.match_indices("free_flags").any(|(i, m)| {
                let rest = c[i + m.len()..].trim_start();
                rest.starts_with('=') && !rest.starts_with("==")
            });
            let borrowed = c
                .find("&mut ")
                .is_some_and(|i| c[i..].split(';').next().unwrap().contains("free_flags"))
                && !c.contains("Tracked(&mut parts.free_flags)");
            let short = ["flags::new(", "flags::load(", "flags::fetch_or("]
                .iter()
                .any(|f| {
                    c.match_indices(f)
                        .any(|(i, _)| !c[..i].ends_with("crate::vspec::"))
                });
            let import = c.starts_with("use ")
                && c.contains("vspec::")
                && c.contains("flags")
                && c != "use crate::vspec::flags::RouteByte;";
            if c.contains("S_FREE_FLAGS")
                && ["write", "copy", "replace", "swap"]
                    .iter()
                    .any(|m| c.contains(m))
            {
                if c == ROUTE_NEW {
                    offset_writes += 1;
                } else {
                    out.push(bad(file, &l));
                }
            }
            let mut call = false;
            for f in [
                "vspec::flags::new(",
                "vspec::flags::load(",
                "vspec::flags::fetch_or(",
            ] {
                if c.contains(f) {
                    match ROUTE_CALLS.iter().position(|w| *w == c) {
                        Some(k) if file == "src/heap/shared.rs" => seen[k] += 1,
                        _ => call = true,
                    }
                }
            }
            if raw || assigned || borrowed || short || import || call {
                out.push(bad(file, &l));
            }
        }
    }
    if offset_writes != 1 || seen != [1, 1, 1] {
        out.push(format!(
            "src/heap/shared.rs: route-byte writes through S_FREE_FLAGS {offset_writes}, calls of new, load, fetch_or {seen:?}, not 1 each"
        ));
    }
    out
}

#[test]
fn access_route_byte_only_through_vspec_flags() {
    report("route byte accesses", check_route(&tree()));
}

#[test]
fn access_route_byte_control() {
    let t = tree();
    let anchor = "use crate::vspec::flags::RouteByte;";
    for line in [
        "fn f(s: &mut Shared) { s.free_flags = crate::vspec::flags::new(0); }",
        "fn f(s: &Shared) -> u8 { crate::vspec::flags::load(&s.free_flags, Ordering::Relaxed) }",
        "fn f(s: &Shared) { unsafe { core::ptr::write(addr_of!(s.free_flags) as *mut u8, 0) } }",
        "use crate::vspec::flags::fetch_or;",
    ] {
        assert!(
            !check_route(&with(&t, "src/heap/shared.rs", anchor, line)).is_empty(),
            "{line}"
        );
    }
}

/// The line model the checks read: test and Kani items, ghost code,
/// comments and string contents are where `shipped` says.
#[test]
fn access_shipped_lines() {
    let text = "\
fn a() {
    x.store(1, R); // y.store(2, R);
    let s = \"a { b // c\";
}
#[cfg(test)]
fn b() {
    t.store(3, R);
}
#[cfg(kani)]
mod k {
    fn h() {
        k.store(4, R);
    }
}
pub closed spec fn g(&self) -> u8 {
    self.state
}
fn c(&self)
    requires
        p,
{
    proof {
        assert(q);
    }
    /* z.store(5, R); */ w.store(6, R);
    let l = '{';
}
#[cfg(test)]
pub static S: [u8; 2] =
    [1, 2];
fn d() {
    v.store(7, R);
}
";
    let got: Vec<(usize, &str, &str, bool)> = shipped(text)
        .iter()
        .map(|l| (l.n, l.code.as_str(), l.func.as_str(), l.ghost))
        .map(|(n, c, f, g)| {
            (
                n,
                String::leak(c.to_string()) as &str,
                String::leak(f.to_string()) as &str,
                g,
            )
        })
        .collect();
    let want: &[(usize, &str, &str, bool)] = &[
        (1, "fn a() {", "a", false),
        (2, "x.store(1, R);", "a", false),
        (3, "let s = \"\";", "a", false),
        (4, "}", "a", false),
        (15, "pub closed spec fn g(&self) -> u8 {", "g", true),
        (16, "self.state", "g", true),
        (17, "}", "g", true),
        (18, "fn c(&self)", "c", false),
        (19, "requires", "c", false),
        (20, "p,", "c", false),
        (21, "{", "c", false),
        (22, "proof {", "c", true),
        (23, "assert(q);", "c", true),
        (24, "}", "c", true),
        (25, "w.store(6, R);", "c", false),
        (26, "let l = ' ';", "c", false),
        (27, "}", "c", false),
        (31, "fn d() {", "d", false),
        (32, "v.store(7, R);", "d", false),
        (33, "}", "d", false),
    ];
    assert_eq!(got, want, "shipped lines: got = left, want = right");
}
