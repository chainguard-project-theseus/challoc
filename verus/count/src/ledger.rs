//! The generated seam ledger back end (`[ledger]` in the configuration):
//! which trusted functions are discharged, read from a ledger of
//! `*.rs.md` files that a generator writes from the source, one entry per
//! seam, matched to each trusted function by the `FILE:LINE` of its `fn`
//! keyword. challoc's `verus/seams.sh` writes such a ledger.
//!
//! An entry is a paragraph whose first line is `FILE:LINE. Kind: K.
//! Category: ...` and which holds `Status: S`. A status classes the seam:
//!
//! - *discharged*: `discharged (...)` (a check outside Verus covers its
//!   whole contract), or [`PREMISED`] with an Isla check or Kani
//!   harnesses that cover it but for the premises it names, each a label
//!   the configured premise file defines, and for Kani an optional
//!   `; callers: <test>` clause naming a `#[test]` fn, the guard test
//!   that confines the item's callers, required when a premise of a
//!   section marked `callers` is named;
//! - *partly checked*: `trusted (partly checked, ...)`;
//! - unchecked otherwise.
//!
//! The combined share counts verified and discharged functions; partly
//! checked ones are shown apart and not counted. A trusted function with
//! no ledger entry is a warning; a discharged function seam that matches
//! no trusted function, or a seam of a kind the counter does not know,
//! fails the run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;
use verus_syn::Item;

/// `[ledger]`: where the ledger is, and the files that define the
/// premises a [`PREMISED`] status may name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The directory of the `*.rs.md` ledger files.
    pub(crate) dir: String,
    /// The premises an Isla status may name.
    pub(crate) isla: Option<PremiseFile>,
    /// The premises a Kani status may name.
    pub(crate) kani: Option<PremiseFile>,
}

/// A file that defines premise labels, by section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PremiseFile {
    pub(crate) file: String,
    pub(crate) sections: Vec<Section>,
}

/// One section of a [`PremiseFile`]: its heading line (`## Premises`), the
/// letters its labels start with, and whether a status naming one of its
/// labels must also name its guard test (`; callers: <test>`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Section {
    pub(crate) heading: String,
    pub(crate) prefixes: String,
    #[serde(default)]
    pub(crate) callers: bool,
}

/// How far a seam's ledger status says a check outside Verus covers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    /// `discharged (...)`: the check covers the whole contract; or
    /// [`PREMISED`]: Isla's check or a Kani harness covers it but for the
    /// premises the status names.
    Discharged,
    /// `trusted (partly checked, ...)`.
    Partly,
    /// `trusted`, `trusted (claims nothing: ...)`, or any other status.
    Unchecked,
}

/// One ledger entry: its `FILE:LINE`, its kind and its status.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Seam {
    pub(crate) loc: String,
    pub(crate) kind: String,
    pub(crate) status: Status,
}

/// The seam kinds a generator places at a function's `fn` line, and that
/// the counter classes as trusted.
const FN_KINDS: &[&str] = &["external_body", "external_body proof", "axiom"];

/// The other kinds a generator writes: items that are not functions, or
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

/// The status of an Isla check or a Kani harness whose contract rests on
/// premises it names: `discharged on premises (Isla: FILE; premises:
/// ...)` or `discharged on premises (Kani: <harness paths>; premises:
/// ...)`.
pub(crate) const PREMISED: &str = "discharged on premises (";

/// The labels a [`PREMISED`] status may name, by the check it cites.
#[derive(Debug, Default)]
pub(crate) struct Premises {
    /// Isla's premise file and the labels it defines.
    pub(crate) isla: Option<Labels>,
    /// Kani's premise file and the labels it defines.
    pub(crate) kani: Option<Labels>,
    /// The paths of the `#[test]` fns ([`test_fns`]), which a Kani
    /// status's `callers:` clause may name.
    pub(crate) tests: BTreeSet<String>,
}

/// The labels one premise file defines.
#[derive(Debug, Default)]
pub(crate) struct Labels {
    /// The file, as a status cites it.
    pub(crate) file: String,
    pub(crate) labels: BTreeSet<String>,
    /// The labels of the sections marked `callers`, with each section's
    /// heading.
    pub(crate) callers: BTreeMap<String, String>,
}

impl Labels {
    /// Reads `p`'s file under `root`.
    pub(crate) fn read(root: &Path, p: &PremiseFile) -> Result<Labels, String> {
        let path = root.join(&p.file);
        let md = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Labels::parse(&p.file, &md, &p.sections).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The labels `md` defines for `sections`, cited as `file`.
    pub(crate) fn parse(file: &str, md: &str, sections: &[Section]) -> Result<Labels, String> {
        let mut out = Labels {
            file: file.to_owned(),
            ..Labels::default()
        };
        for s in sections {
            let prefixes: Vec<char> = s.prefixes.chars().collect();
            let got = premise_labels(md, &s.heading, &prefixes)?;
            if s.callers {
                let heading = s.heading.trim_start_matches('#').trim();
                out.callers
                    .extend(got.iter().map(|l| (l.clone(), heading.to_owned())));
            }
            out.labels.extend(got);
        }
        Ok(out)
    }
}

/// The seams of one ledger file: each entry's `FILE:LINE. Kind: K.
/// Category: ...` line (FILE a `.rs` path) and the `Status: S` that
/// follows it in the same paragraph (`S` may run on over the paragraph's
/// next lines). An entry without a status is an error, and so is a
/// [`PREMISED`] status that cites a check other than Isla's alone or
/// Kani's alone, or names anything but labels the cited check's file
/// defines (`premises`), or none, and a status starting `discharged` in
/// any other form than `discharged (`.
pub(crate) fn parse_ledger(text: &str, premises: &Premises) -> Result<Vec<Seam>, String> {
    let mut seams = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((i, line)) = lines.next() {
        let Some((loc, rest)) = line.split_once(". Kind: ") else {
            continue;
        };
        if !loc.rsplit_once(':').is_some_and(|(f, n)| {
            f.ends_with(".rs")
                && !f.contains(char::is_whitespace)
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        }) {
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
        } else if status.contains("; callers: ") {
            return Err(format!(
                "line {}: {loc}: a `callers:` clause outside a `{PREMISED}Kani: ...)` status",
                i + 1
            ));
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

/// The class of a [`PREMISED`] status, given the text after it: discharged
/// when it cites the Isla runs alone (`Isla: FILE`) or Kani harnesses
/// alone ([`harness_list`]) and names at least one premise, each a label
/// that check's file defines ([`known_labels`]); an error otherwise, since
/// only Isla's and Kani's checks count with named premises, and each only
/// with its own. A Kani status may end in a `; callers: <test path>`
/// clause ([`callers_clause`]), the guard test that confines the item's
/// callers, which must name a `#[test]` fn in `premises.tests`, and must
/// be there when the status names a label of a section marked `callers`;
/// no Isla status may.
fn premised(rest: &str, premises: &Premises) -> Result<Status, String> {
    let isla = premises.isla.as_ref().and_then(|l| {
        rest.strip_prefix(&format!("Isla: {}; premises: ", l.file))
            .map(|p| (p, l))
    });
    let (p, labels, uncalled) = if let Some((p, labels)) = isla {
        (p.to_owned(), labels, false)
    } else if let Some(labels) = &premises.kani
        && let Some((h, p)) = rest
            .strip_prefix("Kani: ")
            .and_then(|r| r.split_once("; premises: "))
        && harness_list(h)
    {
        let (p, callers) = callers_clause(p);
        if let Some(t) = callers
            && !premises.tests.contains(t)
        {
            return Err(format!(
                "{PREMISED}...)'s `callers:` clause names no #[test] fn (one path, `a::tests::f`): {rest:?}"
            ));
        }
        (p, labels, callers.is_none())
    } else {
        return Err(format!(
            "{PREMISED}...) cites a check other than Isla's alone or Kani's alone, names a harness path not of the form `a::b::f`, or names no premise: {rest:?}"
        ));
    };
    let Some(cited) = known_labels(&p, &labels.labels) else {
        return Err(format!(
            "{PREMISED}...) names no premise, or one that is not the label of a premise {} defines: {rest:?}",
            labels.file
        ));
    };
    if uncalled
        && let Some((l, heading)) = cited
            .iter()
            .find_map(|l| labels.callers.get(l).map(|h| (l, h)))
    {
        return Err(format!(
            "{PREMISED}...) names the premise {l} but no `; callers: <test>` clause, the guard test that confines the item's callers to what {l} covers ({}, \"{heading}\"): {rest:?}",
            labels.file
        ));
    }
    Ok(Status::Discharged)
}

/// A Kani premise list (after `premises: `) with its trailing `; callers:
/// <test path>` clause, outside every gloss, taken out: the list up to the
/// clause and the rest from the `)` that ends the clause, and the path.
/// The list as it is, and none, when it has no such clause.
fn callers_clause(p: &str) -> (String, Option<&str>) {
    const TAG: &str = "; callers: ";
    let mut depth = 0usize;
    for (i, c) in p.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ';' if depth == 0 && p[i..].starts_with(TAG) => {
                let after = &p[i + TAG.len()..];
                let end = after.find(')').unwrap_or(after.len());
                return (format!("{}{}", &p[..i], &after[end..]), Some(&after[..end]));
            }
            _ => {}
        }
    }
    (p.to_owned(), None)
}

/// Whether `list` is one or more Kani harness paths (`a::kani_f::f`, at
/// least two `::`-separated identifiers each), separated by `, `, ` and `
/// or `, and `; another check's tag (`Isla: ...`) is no path.
fn harness_list(list: &str) -> bool {
    list.split(", ").flat_map(|s| s.split(" and ")).all(|h| {
        let h = h.strip_prefix("and ").unwrap_or(h);
        let segs: Vec<&str> = h.split("::").collect();
        segs.len() >= 2
            && segs
                .iter()
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    })
}

/// The labels a premise list (`K2 (gloss) and K3 (gloss)`, then the `)`
/// that closes the status and only `.` and whitespace after it) names, when
/// it names at least one premise and only labels in `known` (the cited
/// check's premises, [`premise_labels`]), each optionally followed by a
/// gloss in parentheses; none otherwise. Prose that is no such label, a
/// label that file does not define, text after the closing `)`, or a
/// gloss that never closes gives none.
fn known_labels(list: &str, known: &BTreeSet<String>) -> Option<Vec<String>> {
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
    if !tail?.chars().all(|c| c == '.' || c.is_whitespace()) {
        return None;
    }
    let labels: Vec<String> = bare
        .split(", ")
        .flat_map(|s| s.split(" and "))
        .map(|l| l.trim().to_owned())
        .collect();
    labels.iter().all(|l| known.contains(l)).then_some(labels)
}

/// The labels of the premises one section of a file defines: the letter
/// in `prefixes` and the number that open each `- **LABEL, title.**`
/// bullet of the section `heading` (up to the next `## ` heading). An
/// error names a section that defines no label (a heading renamed or
/// gone).
fn premise_labels(md: &str, heading: &str, prefixes: &[char]) -> Result<BTreeSet<String>, String> {
    let got: BTreeSet<String> = md
        .lines()
        .skip_while(|l| *l != heading)
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("- **")?.split_once(',').map(|(p, _)| p))
        .filter(|p| {
            p.strip_prefix(prefixes)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
        .collect();
    if got.is_empty() {
        return Err(format!("no premise labels in {heading}"));
    }
    Ok(got)
}

/// Every seam in the `*.rs.md` files under `dir`, with `premises` the
/// labels a [`PREMISED`] status may name.
pub(crate) fn read_ledger(dir: &Path, premises: &Premises) -> Result<Vec<Seam>, String> {
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

/// The files under `dir` whose names end in `suffix`, in path order.
fn walk(dir: &Path, suffix: &str, out: &mut Vec<std::path::PathBuf>) {
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

/// Functions and lines of the trusted functions whose seams are
/// discharged, and of those partly checked.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Checked {
    pub(crate) discharged: (usize, usize),
    pub(crate) partly: (usize, usize),
}

impl Checked {
    pub(crate) fn add(&mut self, o: &Checked) {
        self.discharged.0 += o.discharged.0;
        self.discharged.1 += o.discharged.1;
        self.partly.0 += o.partly.0;
        self.partly.1 += o.partly.1;
    }
}

/// Classes each trusted function (`FILE:LINE`, its crate, lines) by the
/// status of the function seam (`FN_KINDS`) at its location. Returns the
/// tally by crate, warnings (a trusted function with no seam) and errors
/// (a seam of an unknown kind, two function seams on one line, or a
/// discharged function seam that matches no trusted function, whose
/// coverage would otherwise drop out of the combined share unnoticed).
pub(crate) fn match_ledger(
    trusted: &[(String, String, usize)],
    seams: &[Seam],
) -> (BTreeMap<String, Checked>, Vec<String>, Vec<String>) {
    let mut checked: BTreeMap<String, Checked> = BTreeMap::new();
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
    for (krate, loc, lines) in trusted {
        let c = checked.entry(krate.clone()).or_default();
        let slot = match by_loc.get(loc.as_str()).map(|s| s.status) {
            Some(Status::Discharged) => &mut c.discharged,
            Some(Status::Partly) => &mut c.partly,
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
            && !trusted.iter().any(|(_, loc, _)| *loc == s.loc)
        {
            errors.push(format!(
                "{}: discharged {} seam matches no trusted function",
                s.loc, s.kind
            ));
        }
    }
    (checked, warnings, errors)
}

/// The paths of the `#[test]` fns in `items`, a file's or an inline
/// module's, whose module path is `module` (`api::mallctl`, or empty for
/// the crate root): `module::name` for each, through inline modules
/// (`api::mallctl::tests::f`), which a [`PREMISED`] status's `callers:`
/// clause names.
pub(crate) fn test_fns(module: &str, items: &[Item], out: &mut BTreeSet<String>) {
    let path = |name: &str| {
        if module.is_empty() {
            name.to_owned()
        } else {
            format!("{module}::{name}")
        }
    };
    for item in items {
        match item {
            Item::Fn(f) if f.attrs.iter().any(|a| a.path().is_ident("test")) => {
                out.insert(path(&f.sig.ident.to_string()));
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    test_fns(&path(&m.ident.to_string()), inner, out);
                }
            }
            _ => {}
        }
    }
}

/// The module path of `file` under `src` (`src/api/mallctl.rs` is
/// `api::mallctl`, `src/heap/mod.rs` is `heap`, `src/lib.rs` is empty),
/// as a `callers:` clause names it, without the crate.
pub(crate) fn module_path(src: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(src).unwrap_or(file).with_extension("");
    let mut segs: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if (segs.len() == 1 && segs[0] == "lib") || segs.last().is_some_and(|s| s == "mod") {
        segs.pop();
    }
    segs.join("::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use verus_syn::parse_file;

    /// The "Platform premises" of an Isla seams file cut down to P1 to P13
    /// and K1 to K3, with a bullet in another section that must not count.
    const SEAMS_MD: &str = "# The seams Isla checks\n\n- **K9, not a premise.** Outside the section.\n\n## Platform premises\n\nThe runs start from a stated state.\n\n- **P1, the build.** a\n- **P2, user memory.** b\n- **P3, x.** c\n- **P4, x.** d\n- **P5, x.** e\n- **P6, x.** f\n- **P7, x.** g\n- **P8, GOT entry contents.** h\n  continued, with a comma\n- **P9, x.** i\n- **P10, x.** j\n- **P11, x.** k\n- **P12, x.** l\n- **P13, x.** m\n- **K1, the kernel's side of a system call.** n\n- **K2, no resume past a trap.** o\n- **K3, a fixed data key A and EnDA.** p\n- **Note, not a label.** q\n\n## The GOT functions\n\n- **K5, not a premise.** Outside the section.\n";

    /// A Kani premises file with C1 and C2 in "Premises" and L1 and L2 in
    /// "Loader and kernel premises"; in each section a bullet whose label
    /// has the other section's letter or another check's, and a bullet in
    /// another section, none of which must count.
    const KANI_MD: &str = "# The premises Kani statuses name\n\n- **C9, not a premise.** Outside the section.\n\n## Premises\n\nWhat no harness reaches.\n\n- **C1, the hook slots.** a\n  continued, with a comma\n- **C2, x.** b\n- **P1, not a Kani label.** c\n- **L7, not a C premise.** d\n\n## Loader and kernel premises\n\nWhat the loader and the kernel set up.\n\n- **L1, glibc's rseq globals.** e\n  continued, with a comma\n- **L2, the rseq area.** f\n- **C8, not an L premise.** g\n- **K1, not a Kani label.** h\n\n## Provenance of the addresses\n\n- **L3, not a premise.** Outside the sections.\n";

    const ISLA_FILE: &str = "verus/isla/seams.md";
    const KANI_FILE: &str = "verus/kani/premises.md";

    fn section(heading: &str, prefixes: &str, callers: bool) -> Section {
        Section {
            heading: heading.into(),
            prefixes: prefixes.into(),
            callers,
        }
    }

    fn isla_labels(md: &str) -> Result<Labels, String> {
        Labels::parse(
            ISLA_FILE,
            md,
            &[section("## Platform premises", "PK", false)],
        )
    }

    fn kani_labels(md: &str) -> Result<Labels, String> {
        Labels::parse(
            KANI_FILE,
            md,
            &[
                section("## Premises", "C", false),
                section("## Loader and kernel premises", "L", true),
            ],
        )
    }

    fn premises() -> Premises {
        Premises {
            isla: Some(isla_labels(SEAMS_MD).unwrap()),
            kani: Some(kani_labels(KANI_MD).unwrap()),
            tests: ["a::tests::guard", "b::m::tests::other"]
                .map(str::to_owned)
                .into(),
        }
    }

    #[test]
    fn the_platform_premise_labels_are_the_sections_bullets() {
        let got = premises().isla.unwrap().labels;
        let want: BTreeSet<String> = (1..=13)
            .map(|n| format!("P{n}"))
            .chain((1..=3).map(|n| format!("K{n}")))
            .collect();
        assert_eq!(got, want, "labels: got = {got:?}, want = {want:?}");
        let got = isla_labels("- **K1, x.** y\n");
        assert!(got.is_err(), "no section: got = {got:?}, want = an error");
        // Isla's section of a Kani file, and the reverse, define nothing.
        let got = isla_labels(KANI_MD);
        assert!(
            got.is_err(),
            "Kani file read as Isla's: got = {got:?}, want = an error"
        );
        let got = kani_labels(SEAMS_MD);
        assert!(
            got.is_err(),
            "Isla file read as Kani's: got = {got:?}, want = an error"
        );
    }

    /// The `#[test]` fns a `callers:` clause may name: each at its file's
    /// module path and its inline modules', and nothing else (a Kani
    /// harness, a plain fn, a test inside a fn).
    #[test]
    fn callers_name_the_test_fns_by_their_module_paths() {
        let src = Path::new("src");
        for (file, want) in [
            ("src/lib.rs", ""),
            ("src/api/mallctl.rs", "api::mallctl"),
            ("src/heap/mod.rs", "heap"),
            ("src/heap/heap_at_tests.rs", "heap::heap_at_tests"),
        ] {
            let got = module_path(src, Path::new(file));
            assert_eq!(got, want, "{file}: got = {got:?}, want = {want:?}");
        }
        let text = "#[test]\nfn top() {}\nfn plain() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn guard() {}\n    #[kani::proof]\n    fn harness() {}\n    mod inner {\n        #[test]\n        fn deep() { #[test] fn nested() {} }\n    }\n}\n";
        let file = parse_file(text).expect("fixture parses");
        let mut got = BTreeSet::new();
        test_fns("api::m", &file.items, &mut got);
        let want: BTreeSet<String> = [
            "api::m::top",
            "api::m::tests::guard",
            "api::m::tests::inner::deep",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(got, want, "tests: got = {got:?}, want = {want:?}");
        let mut got = BTreeSet::new();
        test_fns("", &file.items, &mut got);
        assert!(
            got.contains("top") && got.contains("tests::guard"),
            "crate root: got = {got:?}"
        );
    }

    #[test]
    fn the_kani_premise_labels_are_the_c_and_l_sections_bullets() {
        let got = premises().kani.unwrap();
        let want: BTreeSet<String> = ["C1", "C2", "L1", "L2"].map(str::to_owned).into();
        assert_eq!(got.labels, want, "labels: got = {got:?}, want = {want:?}");
        let want: BTreeMap<String, String> = ["L1", "L2"]
            .map(|l| (l.to_owned(), "Loader and kernel premises".to_owned()))
            .into();
        assert_eq!(got.callers, want, "callers: got = {got:?}, want = {want:?}");
        let got = kani_labels("- **C1, x.** y\n");
        assert!(got.is_err(), "no section: got = {got:?}, want = an error");
        // Each section must define a label: a file with only one of the
        // two is an error, which names the missing one.
        for (md, missing) in [
            (
                "## Premises\n\n- **C1, x.** a\n",
                "## Loader and kernel premises",
            ),
            (
                "## Loader and kernel premises\n\n- **L1, x.** a\n",
                "## Premises",
            ),
            (
                "## Premises\n\n- **C1, x.** a\n\n## Loader and kernel premises\n\n- **C2, x.** b\n",
                "## Loader and kernel premises",
            ),
        ] {
            let got = kani_labels(md).map(|l| l.labels);
            assert_eq!(
                got,
                Err(format!("no premise labels in {missing}")),
                "{md:?}: got = {got:?}, want = an error naming {missing}"
            );
        }
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

## `m`

src/a.rs:50. Kind: ext_type_spec. Category: core library.
Tool: Kani. Status: trusted (claims nothing: an opaque type).

## `n`

crates/b/src/n.rs:60. Kind: axiom. Category: core library.
Status: trusted.

## not an entry

See README.md:3. Kind: of a note. Status: discharged
"#;
        let got = parse_ledger(text, &premises());
        let want = vec![
            seam("src/a.rs:10", "axiom", Status::Discharged),
            seam("src/a.rs:20", "external_body", Status::Partly),
            seam("src/a.rs:30", "external_body const", Status::Unchecked),
            seam("src/a.rs:40", "external_body proof", Status::Discharged),
            // A seam that claims nothing is trusted, not checked.
            seam("src/a.rs:50", "ext_type_spec", Status::Unchecked),
            seam("crates/b/src/n.rs:60", "axiom", Status::Unchecked),
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
    fn an_isla_or_kani_check_on_named_premises_counts_as_discharged() {
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
            (
                "discharged on premises (Kani: a::kani_f::f; premises: C1 (the program stores only null or a hook))",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f, a::kani_f::g and kani_proofs::m::h; premises: C1 (a gloss, with a comma) and C2)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f, a::kani_f::g, and a::kani_f::h; premises: C2)",
                Status::Discharged,
            ),
            // The callers-section premises, alone or with a C premise, each
            // with the guard test that confines the item's callers.
            (
                "discharged on premises (Kani: a::kani_f::f; premises: L1 (the loader's globals, with a comma); callers: a::tests::guard)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f; premises: L1 and L2 (the area (aligned 32)); callers: a::tests::guard)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f; premises: C1, L1 and L2; callers: b::m::tests::other)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f; premises: L1 (x) and L2 (y; with a semicolon); callers: a::tests::guard)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f; premises: C2 (a gloss; with a semicolon); callers: a::tests::guard)",
                Status::Discharged,
            ),
            (
                "discharged on premises (Kani: a::kani_f::f and a::kani_f::g; premises: C1 and C2; callers: b::m::tests::other)",
                Status::Discharged,
            ),
            // A gloss is text: a `; callers: ` inside one is no clause.
            (
                "discharged on premises (Kani: a::kani_f::f; premises: C2 (x; callers: a::tests::missing))",
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
        // Only Isla's and Kani's checks count on premises, and only on
        // named ones.
        for status in [
            "discharged on premises (Kani: a::kani_f::f; premises: K1 (the kernel's side of the call))",
            "discharged on premises (emptyasm: f in src/a.rs; premises: the pointer's provenance)",
            "discharged on premises (Isla: verus/isla/seams.md)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: )",
            "discharged on premises (Isla: verus/isla/other.md; premises: K1)",
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
            // Each check names only its own file's labels.
            "discharged on premises (Isla: verus/isla/seams.md; premises: C1)",
            "discharged on premises (Kani: a::kani_f::f; premises: P8 (the loader's GOT fill))",
            "discharged on premises (Kani: a::kani_f::f; premises: C1 and K1)",
            // The callers-section premises are Kani's alone, and each
            // section's letter counts only in that section.
            "discharged on premises (Isla: verus/isla/seams.md; premises: L1)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: P8 and L1)",
            "discharged on premises (Kani: a::kani_f::f; premises: L1 and P10 (the thread pointer))",
            "discharged on premises (Kani: a::kani_f::f; premises: L3 (outside the sections))",
            "discharged on premises (Kani: a::kani_f::f; premises: L7 (an L bullet among the C premises))",
            "discharged on premises (Kani: a::kani_f::f; premises: C8 (a C bullet among the L premises))",
            "discharged on premises (Kani: a::kani_f::f; premises: L0)",
            "discharged on premises (Kani: a::kani_f::f; premises: L1 and the pointer's provenance)",
            // A callers-section premise holds only for what the item's
            // callers pass, so a status naming one also names their guard
            // test.
            "discharged on premises (Kani: a::kani_f::f; premises: L1 (the loader's globals, with a comma))",
            "discharged on premises (Kani: a::kani_f::f; premises: L1 and L2 (the area (aligned 32)))",
            "discharged on premises (Kani: a::kani_f::f; premises: C1, L1 and L2)",
            "discharged on premises (Kani: a::kani_f::f; premises: C2 and L2 (y; callers: a::tests::guard))",
            // A Kani status names a harness and at least one defined premise.
            "discharged on premises (Kani: a::kani_f::f)",
            "discharged on premises (Kani: a::kani_f::f; premises: )",
            "discharged on premises (Kani: ; premises: C1)",
            "discharged on premises (Kani: f; premises: C1)",
            "discharged on premises (Kani: a::kani_f::f, ; premises: C1)",
            "discharged on premises (Kani: a::kani_f::f, and ; premises: C1)",
            "discharged on premises (Kani: a::kani_f::f; premises: C3)",
            "discharged on premises (Kani: a::kani_f::f; premises: C9 (outside the section))",
            "discharged on premises (Kani: a::kani_f::f; premises: C1 and the caller's pointer)",
            "discharged on premises (Kani: a::kani_f::f; premises: C1) and the caller's pointer",
            // Two checks, or another tag inside the harness list.
            "discharged on premises (Kani: a::kani_f::f, and Isla: verus/isla/seams.md; premises: C1)",
            "discharged on premises (Kani: a::kani_f::f and loom: m::t; premises: C1)",
            // A near miss of either discharged form does not slip through as
            // plain discharged.
            "discharged on premises(Kani: a::kani_f::f; premises: K1)",
            "discharged on premises: K1",
            "discharged, on premises (Kani: a::kani_f::f)",
            // A `callers:` clause names one #[test] fn the tree has, after a
            // Kani premise list, and nothing follows its `)`.
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::tests::missing)",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::kani_f::f)",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: )",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::tests::guard and b::m::tests::other)",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::tests::guard; callers: a::tests::guard)",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::tests::guard) and more",
            "discharged on premises (Kani: a::kani_f::f; premises: C2; callers: a::tests::guard",
            "discharged on premises (Kani: a::kani_f::f; premises: ; callers: a::tests::guard)",
            "discharged on premises (Kani: a::kani_f::f; callers: a::tests::guard; premises: C2)",
            "discharged on premises (Isla: verus/isla/seams.md; premises: K1; callers: a::tests::guard)",
            "discharged (Kani: a::kani_f::f; callers: a::tests::guard)",
            "trusted (partly checked, Kani: a::kani_f::f; callers: a::tests::guard; not checked: x)",
        ] {
            let got = parse_ledger(&entry(status), &premises());
            assert!(got.is_err(), "{status}: got = {got:?}, want = an error");
        }
        // The error for a missing `callers:` clause names the premise and
        // its section.
        let status = "discharged on premises (Kani: a::kani_f::f; premises: C1 and L2 (y))";
        let got = parse_ledger(&entry(status), &premises());
        assert!(
            got.as_ref().is_err_and(|e| e
                .contains("names the premise L2 but no `; callers: <test>` clause")
                && e.contains("(verus/kani/premises.md, \"Loader and kernel premises\")")),
            "{status}: got = {got:?}, want = an error naming L2 and the missing `callers:` clause"
        );
        // A check with no premise file configured counts nothing on
        // premises.
        let none = Premises::default();
        for status in [
            "discharged on premises (Isla: verus/isla/seams.md; premises: K1)",
            "discharged on premises (Kani: a::kani_f::f; premises: C1)",
        ] {
            let got = parse_ledger(&entry(status), &none);
            assert!(got.is_err(), "{status}: got = {got:?}, want = an error");
        }
    }

    #[test]
    fn trusted_functions_are_classed_by_their_seams() {
        let t = |loc: &str, lines| ("k".to_owned(), loc.to_owned(), lines);
        let trusted = vec![
            t("src/a.rs:10", 5),
            t("src/a.rs:20", 7),
            t("src/a.rs:30", 11),
            t("src/a.rs:40", 13),
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
        let want: BTreeMap<String, Checked> = [(
            "k".to_owned(),
            Checked {
                discharged: (1, 5),
                partly: (1, 7),
            },
        )]
        .into();
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
    fn a_ledger_dir_without_files_fails() {
        let dir = std::env::temp_dir().join(format!("kit-ledger-empty-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let got = read_ledger(&dir, &Premises::default());
        assert!(got.is_err(), "empty ledger: got = {got:?}, want = an error");
        fs::write(
            dir.join("a.rs.md"),
            "src/a.rs:1. Kind: axiom.\nStatus: trusted\n",
        )
        .expect("write");
        let got = read_ledger(&dir, &Premises::default());
        assert_eq!(
            got,
            Ok(vec![seam("src/a.rs:1", "axiom", Status::Unchecked)])
        );
        fs::remove_dir_all(&dir).expect("rm");
    }
}
