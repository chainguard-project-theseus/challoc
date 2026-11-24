//! verus/WEAKMEM.md's inventory (its "Inventory" section) counts the
//! source lines that carry an `Ordering::`, in total and per file. The
//! test here recounts them with the rule the section states and fails when
//! the total or a table row differs, or when a file with such a line has
//! no row, so a change that adds or removes an ordering line is told the
//! figures to write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The attributes whose modules the rule leaves out, inline (`mod NAME {`)
/// or in their own files (`mod NAME;`): test code, Kani harnesses,
/// `sync.rs`'s `cfg(loom)` stand-ins for vstd's atomics (built only by the
/// loom crate), and `heap/tlscheck.rs` (built only with
/// `cfg(challoc_tls_check)`).
const LEFT_OUT: &[&str] = &[
    "#[cfg(test)]",
    "#[cfg(kani)]",
    "#[cfg(loom)]",
    "#[cfg(challoc_tls_check)]",
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The module trimmed `t` declares, `[pub[(..)] ]mod NAME {` or `... ;`:
/// its name and whether it is inline.
fn declares_mod(t: &str) -> Option<(&str, bool)> {
    let t = t
        .strip_prefix("pub ")
        .or_else(|| {
            t.strip_prefix("pub(")
                .and_then(|r| r.split_once(") ").map(|(_, r)| r))
        })
        .unwrap_or(t);
    let rest = t.strip_prefix("mod ")?;
    if let Some(name) = rest.strip_suffix(';') {
        Some((name.trim(), false))
    } else {
        rest.strip_suffix('{').map(|name| (name.trim(), true))
    }
}

/// What a left-out attribute at line `i` of `src` covers: the inline
/// module's lines (from its `mod` line to the first `}` line at its
/// indentation), or the file module's name and `#[path]`.
enum Covers<'a> {
    Inline(usize),
    File(&'a str, Option<&'a str>),
}

fn covers<'a>(src: &[&'a str], i: usize) -> Option<Covers<'a>> {
    if !LEFT_OUT.contains(&src[i].trim()) {
        return None;
    }
    let mut path = None;
    let mut k = i + 1;
    while k < src.len() {
        let u = src[k].trim();
        if let Some(p) = u.strip_prefix("#[path = \"") {
            path = p.strip_suffix("\"]");
        } else if !(u.starts_with("#[") || u.starts_with("//")) {
            break;
        }
        k += 1;
    }
    match declares_mod(src.get(k)?.trim())? {
        (_, true) => {
            let ind = indent(src[k]);
            let end = (k + 1..src.len())
                .find(|&j| indent(src[j]) == ind && src[j].trim().starts_with('}'))
                .unwrap_or_else(|| panic!("no end for the module at line {}", k + 1));
            Some(Covers::Inline(end))
        }
        (name, false) => Some(Covers::File(name, path)),
    }
}

/// The lines of `src` the rule counts: those with an `Ordering::` outside
/// comment lines and outside the left-out inline modules.
fn count(src: &[&str]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < src.len() {
        if let Some(Covers::Inline(end)) = covers(src, i) {
            i = end + 1;
            continue;
        }
        let t = src[i].trim();
        if !t.starts_with("//") && t.contains("Ordering::") {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The files and directories (relative to `src/`, a directory ending in
/// `/`) of the left-out modules `src`, the file `rel`, declares.
fn left_out_files(rel: &str, src: &[&str]) -> Vec<String> {
    let (dir, stem) = rel.rsplit_once('/').unwrap_or(("", rel));
    let stem = stem.strip_suffix(".rs").unwrap();
    let base = match (dir, stem) {
        ("", "lib") | (_, "mod") => dir.to_string(),
        ("", _) => stem.to_string(),
        _ => format!("{dir}/{stem}"),
    };
    let join = |p: &str| {
        if base.is_empty() {
            p.to_string()
        } else {
            format!("{base}/{p}")
        }
    };
    let mut out = Vec::new();
    for i in 0..src.len() {
        if let Some(Covers::File(name, path)) = covers(src, i) {
            match path {
                // `#[path]` is relative to the declaring file's directory.
                // A `mod.rs` brings its directory's files with it.
                Some(p) => {
                    let p = if dir.is_empty() {
                        p.to_string()
                    } else {
                        format!("{dir}/{p}")
                    };
                    if let Some(d) = p.strip_suffix("mod.rs") {
                        out.push(d.to_string());
                    }
                    out.push(p);
                }
                None => {
                    out.push(join(&format!("{name}.rs")));
                    out.push(join(&format!("{name}/")));
                }
            }
        }
    }
    out
}

/// The inventory's figures: the total, and each table row's files with
/// their counts.
fn documented(doc: &str) -> (usize, Vec<(String, usize)>) {
    let head = "\n## Inventory\n";
    let at = doc.find(head).expect("WEAKMEM.md has an Inventory section");
    let sect = &doc[at + head.len()..];
    let sect = &sect[..sect.find("\n## ").unwrap_or(sect.len())];
    let total_at = sect
        .find(" lines of source carry an `Ordering::`")
        .expect("the Inventory section states the total");
    let total = sect[..total_at]
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in sect.lines() {
        if line.starts_with("| File | Lines |") {
            in_table = true;
            continue;
        }
        if !in_table || line.starts_with("|---") {
            continue;
        }
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let files: Vec<&str> = cells[1].split(", ").map(|f| f.trim_matches('`')).collect();
        let counts: Vec<usize> = cells[2].split(", ").map(|c| c.parse().unwrap()).collect();
        assert_eq!(
            files.len(),
            counts.len(),
            "row {line:?}: one count per file"
        );
        rows.extend(files.into_iter().map(String::from).zip(counts));
    }
    (total, rows)
}

/// The counted files under `src/` with their counts, leaving out files with
/// none.
fn counted(src_dir: &Path) -> BTreeMap<String, usize> {
    let mut files = Vec::new();
    rs_files(src_dir, &mut files);
    let texts: BTreeMap<String, String> = files
        .iter()
        .map(|f| {
            let rel = f.strip_prefix(src_dir).unwrap().to_str().unwrap();
            (rel.to_string(), std::fs::read_to_string(f).unwrap())
        })
        .collect();
    let out: Vec<String> = texts
        .iter()
        .flat_map(|(rel, t)| left_out_files(rel, &t.lines().collect::<Vec<_>>()))
        .collect();
    texts
        .iter()
        .filter(|(rel, _)| {
            !out.iter().any(|o| {
                if o.ends_with('/') {
                    rel.starts_with(o.as_str())
                } else {
                    *rel == o
                }
            })
        })
        .map(|(rel, t)| (rel.clone(), count(&t.lines().collect::<Vec<_>>())))
        .filter(|(_, n)| *n > 0)
        .collect()
}

#[test]
fn weakmem_inventory_matches_the_source() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let counted = counted(&root.join("src"));
    let total: usize = counted.values().sum();
    let doc = std::fs::read_to_string(root.join("verus/WEAKMEM.md")).unwrap();
    let (doc_total, rows) = documented(&doc);
    let mut wrong = Vec::new();
    if doc_total != total {
        wrong.push(format!("total: written {doc_total}, counted {total}"));
    }
    for (file, n) in &rows {
        let c = counted.get(file).copied().unwrap_or(0);
        if c != *n {
            wrong.push(format!("{file}: written {n}, counted {c}"));
        }
    }
    for (file, c) in &counted {
        if !rows.iter().any(|(f, _)| f == file) {
            wrong.push(format!("{file}: no row, counted {c}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "got = verus/WEAKMEM.md's inventory differs from its counting rule:\n  {}\n\
         want = the total and every row as counted (the section's rule, as LEFT_OUT here states it)",
        wrong.join("\n  ")
    );
}

/// `count` leaves out exactly the inline modules under a `LEFT_OUT`
/// attribute, and comment lines.
#[test]
fn inventory_count_control() {
    let src = [
        "use core::sync::atomic::Ordering;",
        "fn a() { X.load(Ordering::Relaxed); }",
        "// X.load(Ordering::Relaxed)",
        "#[cfg(test)]",
        "mod tests {",
        "    fn b() { X.load(Ordering::Relaxed); }",
        "}",
        "#[cfg(not(test))]",
        "mod exports {",
        "    fn c() { X.load(Ordering::Acquire); }",
        "}",
        "#[cfg(kani)]",
        "#[allow(dead_code)]",
        "mod kani_x {",
        "    fn d() { X.load(Ordering::Relaxed); }",
        "}",
        "#[cfg(test)]",
        "fn e() { X.load(Ordering::Relaxed); }",
    ];
    // `a`, `c` (`not(test)` is shipped) and `e` (a `cfg(test)` function,
    // not a module, which the rule keeps).
    assert_eq!(count(&src), 3);
}

/// `left_out_files` resolves file modules as rustc does: beside `lib.rs`
/// and `mod.rs`, under the stem of any other file, or at `#[path]`.
#[test]
fn inventory_files_control() {
    let src = ["#[cfg(test)]", "mod tests;", "mod shipped;"];
    assert_eq!(left_out_files("lib.rs", &src), ["tests.rs", "tests/"]);
    assert_eq!(
        left_out_files("huge/mod.rs", &src),
        ["huge/tests.rs", "huge/tests/"]
    );
    assert_eq!(
        left_out_files("purge.rs", &src),
        ["purge/tests.rs", "purge/tests/"]
    );
    let src = [
        "#[cfg(kani)]",
        "#[path = \"kani/mod.rs\"]",
        "mod kani_proofs;",
    ];
    assert_eq!(left_out_files("lib.rs", &src), ["kani/", "kani/mod.rs"]);
}
