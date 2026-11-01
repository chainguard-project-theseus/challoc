//! The `kani` presubmit check (`.chainguard/ci/kani.yaml`) runs only on
//! pull requests that change a path in its `changed:` list; `kani-main`
//! runs every harness after each landing. `src/kani/mod.rs` documents the
//! files left to `kani-main` alone (the hot files most proof pull requests
//! touch, and the checked items outside any listed path). Both lists are
//! kept by hand, so a `kani_*` module added to a file in neither, or a
//! seam a ledger discharges with Kani in such a file, would break unseen
//! until after it lands. This test fails on such a file until it is added
//! to one of the two lists. It finds harnesses by their inline `kani_*`
//! module; an out-of-line `kani_*` module other than lib.rs's
//! `kani_proofs` (src/kani/) fails until the test learns where its file
//! is. It lives with the loom pins because the `loom` check runs on every
//! pull request.

use std::path::{Path, PathBuf};

/// The files under `dir` whose names end in `ext`, in path order.
fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, ext, out);
        } else if path.to_str().unwrap().ends_with(ext) {
            out.push(path);
        }
    }
}

/// The paths of the one `changed:` list in `kani.yaml`'s text.
fn changed_list(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().map(str::trim).collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i] == "changed:")
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "kani.yaml: got = {} `changed:` lists, want = 1 (update this test if the check's trigger moved)",
        starts.len()
    );
    let list: Vec<String> = lines[starts[0] + 1..]
        .iter()
        .map_while(|l| l.strip_prefix("- "))
        .map(|l| l.trim_matches('"').to_string())
        .collect();
    assert!(
        !list.is_empty(),
        "kani.yaml: got = an empty `changed:` list, want = its paths"
    );
    list
}

/// The files `src/kani/mod.rs`'s doc leaves to `kani-main`: every
/// backquoted `*.rs` between "`kani-main` runs" and "The list is kept by
/// hand", under `src/`.
fn hot_list(doc: &str) -> Vec<String> {
    let text: String = doc
        .lines()
        .filter_map(|l| l.trim().strip_prefix("//!"))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    let start = text
        .find("`kani-main` runs")
        .expect("src/kani/mod.rs: got = no \"`kani-main` runs\", want = the hot-file paragraph");
    let end = start
        + text[start..].find("The list is kept by hand").expect(
            "src/kani/mod.rs: got = no \"The list is kept by hand\", want = the hot-file paragraph",
        );
    let list: Vec<String> = text[start..end]
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|t| t.ends_with(".rs"))
        .map(|t| format!("src/{t}"))
        .collect();
    assert!(
        !list.is_empty(),
        "src/kani/mod.rs: got = no hot files, want = the files left to kani-main"
    );
    list
}

/// Whether `path` matches `pattern`: a path, or a directory and `/**`.
fn matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix("/**") {
        Some(dir) => path.starts_with(&format!("{dir}/")),
        None => {
            assert!(
                !pattern.contains('*'),
                "kani.yaml: got = pattern {pattern:?}, want = a path or `dir/**` (teach this test the glob)"
            );
            pattern == path
        }
    }
}

#[test]
fn kani_trigger_covers_kani_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    let changed = changed_list(&read(".chainguard/ci/kani.yaml"));
    let hot = hot_list(&read("src/kani/mod.rs"));

    // (file under the repository root, why it needs Kani on a change).
    let mut need: Vec<(String, String)> = Vec::new();
    let mut files = Vec::new();
    walk(&root.join("src"), ".rs", &mut files);
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().to_str().unwrap();
        for (i, line) in std::fs::read_to_string(path).unwrap().lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") || !t.contains("mod kani_") || t == "mod kani_proofs;" {
                continue;
            }
            if t.ends_with(';') {
                // An out-of-line module, whose file this test cannot place:
                // it fails until the test is taught that file.
                need.push((
                    String::new(),
                    format!("{rel}:{}: `{t}` (out of line)", i + 1),
                ));
            } else {
                need.push((rel.into(), format!("{rel}:{}: `{t}`", i + 1)));
            }
        }
    }
    let ledger = root.join("verus/ledger");
    let mut overrides = Vec::new();
    walk(&ledger, ".rs.overrides", &mut overrides);
    for path in &overrides {
        let rel = path.strip_prefix(&ledger).unwrap().to_str().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        if let Some(line) = text.lines().find(|l| l.contains("Kani: ")) {
            let seam = rel.strip_suffix(".overrides").unwrap();
            let entry = line.split('\t').next().unwrap();
            need.push((
                seam.into(),
                format!("verus/ledger/{rel}: {entry} cites Kani"),
            ));
        }
    }

    let missing: Vec<&str> = need
        .iter()
        .filter(|(file, _)| !changed.iter().any(|p| matches(p, file)) && !hot.contains(file))
        .map(|(_, why)| why.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "got = files outside kani.yaml's `changed:` and src/kani/mod.rs's kani-main list:\n  {}\n\
         want = each in `changed:` (a file proof lanes rarely touch) or named in src/kani/mod.rs's \
         list of files left to kani-main (a hot file)",
        missing.join("\n  ")
    );
}
