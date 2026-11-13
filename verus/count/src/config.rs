//! The port's configuration, `verus/count.toml` (kit/count/README.md):
//! which crates to read and which ship, which `cfg`s are test-only, and
//! where each table the checks read lives. A check whose section is
//! missing does not run, and with `--base-rev` a change that removes or
//! narrows one the base revision's copy has fails
//! ([`Config::narrowed_from`]) unless a `[[narrowed]]` line names it with
//! a reason ([`Config::explain`]); every path is relative to the
//! repository root.
//!
//! Unknown keys are an error, so a misspelled section or key fails the
//! run rather than turning a check off.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) crates: Crates,
    #[serde(default)]
    pub(crate) floor: Floor,
    #[serde(default)]
    pub(crate) rlimit: Rlimit,
    #[serde(default)]
    pub(crate) kani: Kani,
    #[serde(default)]
    pub(crate) seams: Seams,
    /// The recursion check (`recursive`), on when present.
    pub(crate) recursion: Option<Recursion>,
    /// The entry-point reachability gate ([`crate::reach`]), on when
    /// present.
    pub(crate) reach: Option<Reach>,
    /// The compile-inputs check ([`crate::inputs`]), on when present.
    pub(crate) inputs: Option<Inputs>,
    /// The token-mint gate ([`crate::mints`]), on when present.
    pub(crate) mints: Option<Mints>,
    /// The generated seam ledger ([`crate::ledger`]): when present, it and
    /// not the Kani marker says which trusted functions are discharged.
    pub(crate) ledger: Option<crate::ledger::Config>,
    /// The coverage badge gate (`crate::check_badge`), on when present: with
    /// `--floor`, the file must give FLOOR's `functions` and `lines`
    /// figures in the one canonical badge line.
    pub(crate) badge: Option<Badge>,
    /// The narrowings of the base revision's copy made on purpose, each
    /// with its reason ([`Config::explain`]).
    #[serde(default)]
    pub(crate) narrowed: Vec<Narrowed>,
}

/// The crates the counter reads.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Crates {
    /// A directory each of whose subdirectories with a `src/` is a crate,
    /// named for the subdirectory (`crates`).
    pub(crate) dir: Option<String>,
    /// Crates named explicitly, read after `dir`'s.
    #[serde(default)]
    pub(crate) members: Vec<Member>,
    /// `cfg` names that only test builds set (`test`, `kani`): an item
    /// that needs one is left out.
    #[serde(default = "default_test_cfgs")]
    pub(crate) test_cfgs: Vec<String>,
    /// Features that only tests enable: an item under `feature = "F"` is
    /// left out, and a build of the workspace that enables one fails the
    /// run (`test_util_enablers`).
    #[serde(default)]
    pub(crate) test_features: Vec<String>,
    /// Features that only Verus's build enables (expat-rs's `verus`, which
    /// pulls in vstd for a port whose MSRV is below vstd's): an item under
    /// `feature = "F"` is built only for Verus, as one under
    /// `verus_keep_ghost` is.
    #[serde(default)]
    pub(crate) verus_features: Vec<String>,
    /// `cfg` atoms Verus's build is sure to set, written as a `cfg` writes
    /// them (`unix`, `target_os = "linux"`, or `feature = "jit"` for a
    /// default feature the proof run keeps), beyond `verus_keep_ghost` and
    /// the `verus_features`. A shipped build may set them or not. Any atom
    /// the configuration does not settle may be false where Verus runs, so
    /// a function under it counts as trusted, never as verified.
    #[serde(default)]
    pub(crate) verus_sets: Vec<String>,
    /// `cfg` atoms Verus's build never sets (`miri`), written as
    /// `verus_sets` writes them, so `not(miri)` is sure to hold where
    /// Verus runs. A shipped build may set them or not.
    #[serde(default)]
    pub(crate) verus_never_sets: Vec<String>,
    /// Whether the ghost column counts the `spec` and `proof` functions of
    /// code built only for Verus too (challoc's counter does); by default
    /// it counts shipped code's alone. No FLOOR figure reads it.
    #[serde(default)]
    pub(crate) verus_only_ghost: bool,
    /// Text to replace before parsing, each with a stand-in of the same
    /// length so lines keep their numbers: an identifier Verus keeps as a
    /// keyword (`["assert::", "assrtx::"]`).
    #[serde(default)]
    pub(crate) stand_ins: Vec<[String; 2]>,
}

/// One crate: its name, as cargo spells it, its directory, which holds
/// `src/` and `Cargo.toml`, and whether it ships. Without `ships`, a
/// crate ships unless its Cargo.toml says `publish = false`, as every
/// crate `dir` finds; with it, as it says (challoc's one crate is
/// `publish = false`, since it is not a crates.io crate, and ships).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) ships: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Floor {
    /// The FLOOR file a base revision's copy is read from (`--base-rev`).
    #[serde(default = "default_floor")]
    pub(crate) path: String,
    /// The prefix of the two verified-or-discharged figures: `checked`
    /// (`checked-functions`) or `combined`.
    #[serde(default = "default_checked")]
    pub(crate) checked: String,
    /// A prefix the base revision's copy may still give those two figures
    /// under (challoc's `combined`), read there as `checked` names them, so
    /// the change that renames them compares with its base. The file under
    /// test takes `checked` alone.
    #[serde(default)]
    pub(crate) renamed_from: Option<String>,
    /// Whether the configuration has a `[floor]` table: a base revision
    /// whose copy has one must hold the FLOOR file it names.
    #[serde(skip_deserializing, default = "present")]
    pub(crate) declared: bool,
}

impl Default for Floor {
    fn default() -> Self {
        Floor {
            path: default_floor(),
            checked: default_checked(),
            renamed_from: None,
            declared: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Badge {
    /// The file that shows the badge (`README.md`).
    #[serde(default = "default_badge")]
    pub(crate) path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rlimit {
    /// The RLIMIT file `--rlimit` holds the statistics to.
    #[serde(default = "default_rlimit")]
    pub(crate) path: String,
    /// Whether the configuration has an `[rlimit]` table, as
    /// [`Floor::declared`].
    #[serde(skip_deserializing, default = "present")]
    pub(crate) declared: bool,
}

impl Default for Rlimit {
    fn default() -> Self {
        Rlimit {
            path: default_rlimit(),
            declared: false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Kani {
    /// The directory of the `kani-*.md` records that cover the functions
    /// marked `discharged_by = "kani"`. With none, a marked function fails
    /// the run.
    pub(crate) records: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Seams {
    /// Files holding a seams table (`verus/SEAMS.md`).
    #[serde(default)]
    pub(crate) tables: Vec<String>,
    /// A directory whose every `*.md` holds a theorem's seams table.
    pub(crate) theorems: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recursion {
    /// The file whose table names each verified recursive function.
    pub(crate) table: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reach {
    /// The REACH file.
    pub(crate) path: String,
    /// The last segment of a macro whose `fn BODY [..]` entries name
    /// bodies it writes exported wrappers for (`exports`).
    pub(crate) exports_macro: Option<String>,
    /// A `macro_rules!` of one rule that writes a function-pointer
    /// wrapper struct's impls, expanded from its definition (`fn_ptr`).
    pub(crate) fn_ptr_macro: Option<String>,
    /// A one-argument associated function the exported wrappers apply to
    /// their arguments, rooted in every impl (`from_c`).
    pub(crate) conversion: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inputs {
    /// The file whose table names each allowed read.
    pub(crate) table: String,
    /// The crates the walk follows calls through.
    pub(crate) crates: Vec<String>,
    /// The functions the walk starts from.
    pub(crate) roots: Vec<Root>,
    /// Whether every function that calls `get_or_init` is a root too.
    #[serde(default)]
    pub(crate) lazy: bool,
    /// The roots in words, for the report: "`Regex::new` and the lazy
    /// builders".
    pub(crate) describe: String,
}

/// A root of the compile-inputs walk: the free function or method `name`
/// (of `owner`, when given) in a file whose path ends in `file`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Root {
    pub(crate) file: String,
    pub(crate) owner: Option<String>,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mints {
    /// The MINTS file.
    pub(crate) path: String,
    /// The one trusted function that makes a token (`c_mint`).
    pub(crate) mint: String,
    /// The trait each kind of token implements (`CToken`).
    pub(crate) token_trait: String,
    /// The file that defines the mint function.
    pub(crate) home: String,
}

fn default_test_cfgs() -> Vec<String> {
    vec!["test".into(), "kani".into()]
}

fn default_floor() -> String {
    "verus/FLOOR".into()
}

fn default_badge() -> String {
    "README.md".into()
}

fn default_checked() -> String {
    "checked".into()
}

fn default_rlimit() -> String {
    "verus/RLIMIT".into()
}

fn present() -> bool {
    true
}

impl Config {
    /// Reads `path`, failing on a file it cannot read or parse, on a key
    /// it does not know, and on a configuration that names no crate.
    pub(crate) fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Config, String> {
        let c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        if c.crates.dir.is_none() && c.crates.members.is_empty() {
            return Err("[crates] names no crate: give `dir`, `members`, or both".into());
        }
        for n in &c.narrowed {
            if n.change.trim().is_empty() {
                return Err("a [[narrowed]] line needs a change".into());
            }
            if n.reason.trim().is_empty() {
                return Err(format!(
                    "the [[narrowed]] line for {:?} needs a reason",
                    n.change
                ));
            }
        }
        c.check_verus_atoms()?;
        let read = c.read_names();
        for [from, to] in &c.crates.stand_ins {
            if let Some(name) = read.iter().find(|n| rewrites(from, to, n)) {
                return Err(format!(
                    "stand-in [{from:?}, {to:?}] would rewrite `{name}`, which the counter reads to class code; a stand-in may only change text the classifier does not read"
                ));
            }
        }
        Ok(c)
    }

    /// The names the counter's classes and checks read in the source,
    /// which no stand-in may rewrite: a stand-in that turned
    /// `external_body` or `assume` into other text would class a trusted
    /// function verified, one that rewrote `cfg` or a test cfg would move
    /// code in or out of the shipped tally, one that rewrote `fn` would
    /// drop functions from it, and one that rewrote `unsafe` would hide
    /// each `#[unsafe(no_mangle)]` export from the entry points. Verus reads the original text, so it
    /// still passes. The configured macro, conversion and mint names are
    /// among them.
    fn read_names(&self) -> Vec<String> {
        const FIXED: [&str; 25] = [
            "external_body",
            "external",
            "assume",
            "admit",
            "assume_new",
            "verus_verify",
            "verus_spec",
            "discharged_by",
            "kani",
            "verifier",
            "rlimit",
            "cfg",
            "cfg_attr",
            "feature",
            "verus_keep_ghost",
            "verus",
            "proof",
            "spec",
            "axiom",
            "fn",
            "unsafe",
            "no_mangle",
            "export_name",
            "thread_local",
            "macro_rules",
        ];
        let reach = self.reach.iter().flat_map(|r| {
            [&r.exports_macro, &r.fn_ptr_macro, &r.conversion]
                .into_iter()
                .flatten()
                .cloned()
        });
        let mints = self
            .mints
            .iter()
            .flat_map(|m| [m.mint.clone(), m.token_trait.clone()]);
        FIXED
            .iter()
            .map(|n| n.to_string())
            .chain(self.crates.test_cfgs.iter().cloned())
            .chain(self.crates.test_features.iter().cloned())
            .chain(self.crates.verus_features.iter().cloned())
            .chain(
                self.verus_set_atoms()
                    .into_iter()
                    .chain(self.verus_never_atoms())
                    .flat_map(|a| {
                        a.split('=')
                            .map(|p| p.trim_matches('"').to_owned())
                            .collect::<Vec<_>>()
                    }),
            )
            .chain(reach)
            .chain(mints)
            .collect()
    }

    /// Fails on a `verus_sets` or `verus_never_sets` entry that is not one
    /// `cfg` atom (a name, or `name = "value"`), and on one the
    /// configuration already settles: a test atom, a Verus atom, or an
    /// atom named twice, since one atom cannot be both sure and never set.
    fn check_verus_atoms(&self) -> Result<(), String> {
        let mut seen: Vec<(String, &str)> = self
            .test_atoms()
            .into_iter()
            .map(|a| (a, "a test atom"))
            .chain(self.verus_atoms().into_iter().map(|a| (a, "a Verus atom")))
            .collect();
        for (key, names, what) in [
            ("verus_sets", &self.crates.verus_sets, "in verus_sets"),
            (
                "verus_never_sets",
                &self.crates.verus_never_sets,
                "in verus_never_sets",
            ),
        ] {
            for n in names {
                let a = atom(n).ok_or_else(|| {
                    format!(
                        "[crates] {key} entry {n:?} is not one cfg atom: give a name (`miri`) or `name = \"value\"` (`feature = \"jit\"`)"
                    )
                })?;
                if let Some((_, was)) = seen.iter().find(|(s, _)| *s == a) {
                    return Err(format!("[crates] {key} entry {n:?} is already {was}"));
                }
                seen.push((a, what));
            }
        }
        Ok(())
    }

    /// The `cfg` atoms that only Verus's build meets, as `cfg_eval`
    /// compares them: `verus_keep_ghost`, and `feature="F"` for each
    /// Verus feature.
    pub(crate) fn verus_atoms(&self) -> Vec<String> {
        std::iter::once("verus_keep_ghost".to_owned())
            .chain(
                self.crates
                    .verus_features
                    .iter()
                    .map(|f| format!("feature=\"{f}\"")),
            )
            .collect()
    }

    /// `verus_sets`' atoms, as `cfg_eval` compares them (whitespace
    /// removed). An entry that is not an atom is left out: [`Config::parse`]
    /// refuses it.
    pub(crate) fn verus_set_atoms(&self) -> Vec<String> {
        self.crates
            .verus_sets
            .iter()
            .filter_map(|n| atom(n))
            .collect()
    }

    /// `verus_never_sets`' atoms, as [`Config::verus_set_atoms`] gives
    /// `verus_sets`'.
    pub(crate) fn verus_never_atoms(&self) -> Vec<String> {
        self.crates
            .verus_never_sets
            .iter()
            .filter_map(|n| atom(n))
            .collect()
    }

    /// The `cfg` atoms that only test builds meet, as `cfg_eval` compares
    /// them (whitespace removed): each test cfg name, and
    /// `feature="F"` for each test feature.
    pub(crate) fn test_atoms(&self) -> Vec<String> {
        self.crates
            .test_cfgs
            .iter()
            .cloned()
            .chain(
                self.crates
                    .test_features
                    .iter()
                    .map(|f| format!("feature=\"{f}\"")),
            )
            .collect()
    }

    /// `text` with each stand-in applied.
    pub(crate) fn stand_in(&self, text: &str) -> String {
        stand_in(&self.crates.stand_ins, text)
    }

    /// How this configuration turns off or narrows a gate that `base`, the
    /// base revision's, runs: one line per change. A gate whose section is
    /// left out does not run and prints nothing, so deleting its section
    /// would pass every check it makes; the run fails instead, as FLOOR
    /// and RLIMIT fail a loosened figure. The changes are a `[recursion]`,
    /// `[reach]`, `[inputs]`, `[mints]` or `[badge]` section removed; a `[reach]`
    /// exports macro, function-pointer macro or conversion removed or
    /// renamed (each adds entry points); an `[inputs]` crate or root
    /// removed, or `lazy` turned off (each narrows the walk); a `[crates]`
    /// `dir` removed or changed, a member removed or its `path` changed,
    /// or a crate's `ships` changed to anything but `true`, compared by
    /// member name and by path, with whatever reads a crate the base read
    /// (a member under any name, or a `dir`, which reads with `ships`
    /// unset), so a rename, a move onto a crate of the base's `dir` and a
    /// crate handed between `dir` and members are held (each can take a crate out
    /// of the shipped tally and the walks; a member that both moves and
    /// changes `ships` makes two narrowings); a `test_cfgs`,
    /// `test_features` or `verus_features` name added (each takes the
    /// items under it out of the shipped tally); a `verus_sets` or
    /// `verus_never_sets` atom added (each lets the functions it guards
    /// count as verified, which they could not while Verus's build might
    /// skip them); a `stand_ins` pair added
    /// (it changes what the counter reads); the `[badge]` section removed,
    /// or its `path` moved, since the badge a reader sees is in the file
    /// the base held and no base copy of the badge is compared. Renaming a table, a mint,
    /// or the FLOOR, RLIMIT or REACH file is not a narrowing: the gate
    /// still runs, on the files and names the change gives it, and the
    /// base's copy of each file is read at the path the base's
    /// configuration gives it.
    pub(crate) fn narrowed_from(&self, base: &Config) -> Vec<Narrowing> {
        let mut out = Vec::new();
        let mut push = |change: String, effect: &str| {
            out.push(Narrowing {
                change,
                effect: effect.into(),
            })
        };
        let (h, b) = (&self.crates, &base.crates);
        if let Some(d) = &b.dir
            && h.dir.as_ref() != Some(d)
        {
            push(
                format!("[crates] dir is no longer {d:?}"),
                "so the crates under it may no longer be read",
            );
        }
        let ships = |s: Option<bool>| s.map_or("unset".to_owned(), |s| s.to_string());
        let unshipped = |was: Option<bool>, now: Option<bool>| was != now && now != Some(true);
        for m in &b.members {
            let Some(n) = h.members.iter().find(|n| n.name == m.name) else {
                push(
                    format!("[crates] members drops {:?}", m.name),
                    "so it is no longer read",
                );
                continue;
            };
            if n.path != m.path {
                push(
                    format!(
                        "[crates] member {:?} moves from {:?} to {:?}",
                        m.name, m.path, n.path
                    ),
                    "so the crate it read may no longer be read",
                );
            }
            if unshipped(m.ships, n.ships) {
                push(
                    format!(
                        "[crates] member {:?} changes ships from {} to {}",
                        m.name,
                        ships(m.ships),
                        ships(n.ships)
                    ),
                    "so it may no longer count as shipped",
                );
            }
        }
        // A crate is also compared by its path, with whatever reads it now
        // under any name: a member there, or a `dir` it sits in, which
        // reads it with `ships` unset. So a rename, a member moved onto a
        // crate of the base's `dir`, and a crate handed between `dir` and
        // members each have their `ships` change named. (crate_dirs
        // refuses a `..` or a symlink on a path, so paths that compare
        // unequal here name different directories.)
        let in_dir = |dir: &Option<String>, at: &Path| {
            dir.as_ref()
                .is_some_and(|d| at.parent() == Some(normal(d).as_path()))
        };
        let effect = "so the crate the base read there may no longer count as shipped";
        for m in &b.members {
            let at = normal(&m.path);
            // The member under its own name at its own path is compared
            // above.
            if h.members
                .iter()
                .any(|n| n.name == m.name && normal(&n.path) == at)
            {
                continue;
            }
            let (who, now) = match h.members.iter().find(|n| normal(&n.path) == at) {
                Some(n) => (format!("member {:?} at {:?}", n.name, n.path), n.ships),
                None if in_dir(&h.dir, &at) => (
                    format!(
                        "crate at {:?} through dir {:?}",
                        m.path,
                        h.dir.as_deref().unwrap_or_default()
                    ),
                    None,
                ),
                None => continue,
            };
            if unshipped(m.ships, now) {
                push(
                    format!(
                        "[crates] {who} changes ships from {} to {}",
                        ships(m.ships),
                        ships(now)
                    ),
                    effect,
                );
            }
        }
        for n in &h.members {
            let at = normal(&n.path);
            if b.members.iter().any(|m| normal(&m.path) == at) || !in_dir(&b.dir, &at) {
                continue;
            }
            if unshipped(None, n.ships) {
                push(
                    format!(
                        "[crates] member {:?} at {:?} changes ships from unset to {}",
                        n.name,
                        n.path,
                        ships(n.ships)
                    ),
                    effect,
                );
            }
        }
        for pair in h.stand_ins.iter().filter(|p| !b.stand_ins.contains(p)) {
            push(
                format!("[crates] stand_ins adds {pair:?}"),
                "so the text it rewrites is no longer read as written",
            );
        }
        for (key, here, there) in [
            ("test_cfgs", &h.test_cfgs, &b.test_cfgs),
            ("test_features", &h.test_features, &b.test_features),
            ("verus_features", &h.verus_features, &b.verus_features),
        ] {
            for n in here.iter().filter(|n| !there.contains(n)) {
                push(
                    format!("[crates] {key} adds {n:?}"),
                    "so the items under it leave the shipped tally",
                );
            }
        }
        // Compared as atoms, so respacing one is not a change.
        for (key, here, there) in [
            ("verus_sets", &h.verus_sets, base.verus_set_atoms()),
            (
                "verus_never_sets",
                &h.verus_never_sets,
                base.verus_never_atoms(),
            ),
        ] {
            for n in here
                .iter()
                .filter(|n| atom(n).is_none_or(|a| !there.contains(&a)))
            {
                push(
                    format!("[crates] {key} adds {n:?}"),
                    "so the counter takes Verus's build to settle it, and the functions it guards may count as verified",
                );
            }
        }
        for (section, here, there) in [
            (
                "recursion",
                self.recursion.is_some(),
                base.recursion.is_some(),
            ),
            ("reach", self.reach.is_some(), base.reach.is_some()),
            ("inputs", self.inputs.is_some(), base.inputs.is_some()),
            ("mints", self.mints.is_some(), base.mints.is_some()),
            ("badge", self.badge.is_some(), base.badge.is_some()),
        ] {
            if there && !here {
                push(
                    format!("the [{section}] section is gone"),
                    "so its gate no longer runs",
                );
            }
        }
        if let (Some(h), Some(b)) = (&self.badge, &base.badge)
            && normal(&h.path) != normal(&b.path)
        {
            push(
                format!("[badge] path changes from {:?} to {:?}", b.path, h.path),
                "so the badge the base held to FLOOR is no longer held",
            );
        }
        if let (Some(h), Some(b)) = (&self.reach, &base.reach) {
            for (key, here, there) in [
                ("exports_macro", &h.exports_macro, &b.exports_macro),
                ("fn_ptr_macro", &h.fn_ptr_macro, &b.fn_ptr_macro),
                ("conversion", &h.conversion, &b.conversion),
            ] {
                match (there, here) {
                    (Some(_), None) => push(
                        format!("[reach] {key} is gone"),
                        "so the entry points it named are no longer walked",
                    ),
                    (Some(t), Some(n)) if t != n => push(
                        format!("[reach] {key} changes from {t:?} to {n:?}"),
                        "so the entry points the old name named are no longer walked",
                    ),
                    _ => {}
                }
            }
        }
        if let (Some(h), Some(b)) = (&self.inputs, &base.inputs) {
            for k in b.crates.iter().filter(|k| !h.crates.contains(k)) {
                push(
                    format!("[inputs] crates drops {k:?}"),
                    "so the walk no longer follows calls through it",
                );
            }
            for r in b.roots.iter().filter(|r| !h.roots.contains(r)) {
                let owner = r.owner.as_ref().map_or(String::new(), |o| format!("{o}::"));
                push(
                    format!("[inputs] roots drops {owner}{} in {}", r.name, r.file),
                    "so the walk no longer starts there",
                );
            }
            if b.lazy && !h.lazy {
                push(
                    "[inputs] lazy is turned off".into(),
                    "so the lazy builders are no longer roots",
                );
            }
        }
        out
    }

    /// This configuration's narrowings of `base` ([`Config::narrowed_from`])
    /// held to its `[[narrowed]]` lines, as FLOOR's figures are held to its
    /// `lowered` lines: a narrowing passes when a new line names its
    /// change, so a narrowing is never silent and an old reason never
    /// covers a new one. Each of `base`'s lines is history and holds one
    /// head line with its change, the one with its reason when there is
    /// one, so a reworded history line is still history; the head's lines
    /// beyond those are new. A narrowing made again after an earlier line
    /// named it (a test cfg dropped and added back, a section restored and
    /// retired again) is named by one more line with the same change. A
    /// new line that names no narrowing fails, so no reason stands for a
    /// change that was not made. `more` are narrowings found outside the
    /// configuration (a crate's Cargo.toml gaining `publish = false`),
    /// held to the lines the same way.
    pub(crate) fn explain(&self, base: &Config, more: Vec<Narrowing>) -> Explained {
        let mut new: Vec<&Narrowed> = self.narrowed.iter().collect();
        let mut history: Vec<&Narrowed> = Vec::new();
        for b in &base.narrowed {
            match new
                .iter()
                .position(|n| n.change == b.change && n.reason == b.reason)
            {
                Some(k) => {
                    new.remove(k);
                }
                None => history.push(b),
            }
        }
        for b in history {
            if let Some(k) = new.iter().position(|n| n.change == b.change) {
                new.remove(k);
            }
        }
        let mut out = Explained::default();
        for n in self.narrowed_from(base).into_iter().chain(more) {
            match new.iter().position(|l| l.change == n.change) {
                Some(k) => out.named.push((n, new.remove(k).reason.clone())),
                None => out.unnamed.push(n),
            }
        }
        out.stale = new.into_iter().map(|l| l.change.clone()).collect();
        out
    }

    /// Whether this configuration reads the crate at `path` (relative to
    /// the root) and leaves whether it ships to its Cargo.toml: a crate of
    /// `dir`, or a member there without `ships`.
    pub(crate) fn reads_by_rule(&self, path: &str) -> bool {
        let at = normal(path);
        match self.crates.members.iter().find(|m| normal(&m.path) == at) {
            Some(m) => m.ships.is_none(),
            None => self
                .crates
                .dir
                .as_ref()
                .is_some_and(|d| at.parent() == Some(normal(d).as_path())),
        }
    }
}

/// One way a configuration narrows its base's ([`Config::narrowed_from`]):
/// the change, which a `[[narrowed]]` line names exactly, and what it turns
/// off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Narrowing {
    pub(crate) change: String,
    pub(crate) effect: String,
}

impl std::fmt::Display for Narrowing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}, {}", self.change, self.effect)
    }
}

/// A `[[narrowed]]` line: a narrowing the configuration makes on purpose,
/// named by its change as the run prints it, and why.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Narrowed {
    pub(crate) change: String,
    pub(crate) reason: String,
}

/// [`Config::explain`]'s answer: each narrowing a new `[[narrowed]]` line
/// names, with its reason; each one no such line names; and the change
/// of each new line that names no narrowing.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Explained {
    pub(crate) named: Vec<(Narrowing, String)>,
    pub(crate) unnamed: Vec<Narrowing>,
    pub(crate) stale: Vec<String>,
}

/// Whether replacing `from` with `to` can change an occurrence of `name`:
/// whether `name` can sit in some text so that it overlaps a character
/// of `from` that `to` changes, agreeing with `from` wherever the two
/// overlap. Text `from` shares with `to` at its start or end is left as
/// it was, so a name that touches only that part is safe (challoc's
/// `let tracked =` keeps `let track` and ` =`).
fn rewrites(from: &str, to: &str, name: &str) -> bool {
    if !from.is_empty() && from == to {
        return false;
    }
    let f: Vec<char> = from.chars().collect();
    let t: Vec<char> = to.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let prefix = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let suffix = f
        .iter()
        .rev()
        .zip(t.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(f.len() - prefix);
    // The changed characters of `from`, half open; empty when `to` only
    // inserts, at the point between the kept parts.
    let (lo, hi) = (prefix as isize, (f.len() - suffix) as isize);
    let (f_len, n_len) = (f.len() as isize, n.len() as isize);
    // `name` placed with its first character at `from`'s offset `o`.
    (1 - n_len..f_len.max(1)).any(|o| {
        let end = o + n_len;
        let touches = if lo == hi {
            o < lo && lo < end
        } else {
            o < hi && lo < end
        };
        touches
            && n.iter().enumerate().all(|(k, ch)| {
                let at = o + k as isize;
                at < 0 || at >= f_len || f[at as usize] == *ch
            })
    })
}

/// `text` as one `cfg` atom, without whitespace, as `cfg_eval` compares
/// atoms: a name (`miri`), or a name, `=` and a string (`feature="jit"`);
/// `None` for anything else, such as a predicate (`not(miri)`).
fn atom(text: &str) -> Option<String> {
    let w: String = text.split_whitespace().collect();
    let is_name = |n: &str| {
        n.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let ok = match w.split_once('=') {
        None => is_name(&w),
        Some((name, value)) => {
            is_name(name)
                && value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"')
                && !value[1..value.len() - 1].contains('"')
        }
    };
    ok.then_some(w)
}

/// `path` as [`Path`] compares it, without `.` components, so `./crates/a`
/// and `crates/a/` name the directory `crates/a` does.
pub(crate) fn normal(path: &str) -> PathBuf {
    Path::new(path)
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect()
}

/// `text` with each of `pairs`' first strings replaced by its second.
pub(crate) fn stand_in(pairs: &[[String; 2]], text: &str) -> String {
    pairs
        .iter()
        .fold(text.to_owned(), |t, [from, to]| t.replace(from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_reads() {
        let c = Config::parse("[crates]\ndir = \"crates\"\n[floor]\n").expect("parse");
        assert_eq!(c.crates.dir.as_deref(), Some("crates"));
        assert_eq!(c.floor.path, "verus/FLOOR");
        assert_eq!(c.floor.checked, "checked");
        assert_eq!(c.floor.renamed_from, None);
        assert_eq!(c.rlimit.path, "verus/RLIMIT");
        // `[floor]` is given, with every key defaulted; `[rlimit]` is not.
        assert!(c.floor.declared && !c.rlimit.declared);
        assert_eq!(c.test_atoms(), ["test", "kani"]);
        assert!(c.recursion.is_none() && c.reach.is_none());
        assert!(c.inputs.is_none() && c.mints.is_none() && c.ledger.is_none());
    }

    #[test]
    fn test_features_are_atoms() {
        let c = Config::parse(
            "[crates]\ndir = \"crates\"\ntest_cfgs = [\"test\"]\ntest_features = [\"test-util\"]\n[floor]\n",
        )
        .expect("parse");
        assert_eq!(c.test_atoms(), ["test", "feature=\"test-util\""]);
    }

    #[test]
    fn bad_configs_fail() {
        for (text, want) in [
            ("[floor]\n", "missing field `crates`"),
            ("[crates]\n[floor]\n", "names no crate"),
            (
                "[crates]\ndir = \"c\"\n[floor]\n[reach]\npth = \"x\"\n",
                "unknown field",
            ),
            (
                "[crates]\ndir = \"c\"\n[floor]\n[recursoin]\ntable = \"x\"\n",
                "unknown field",
            ),
            (
                "[crates]\ndir = \"c\"\n[floor]\n[mints]\npath = \"x\"\n",
                "missing field",
            ),
            ("[crates", "TOML parse error"),
        ] {
            let got = Config::parse(text).map(|_| ()).unwrap_err();
            assert!(got.contains(want), "{text:?}: got = {got}, want = {want}");
        }
    }

    /// pcre3's and challoc's stand-ins pass. Negative: a stand-in that
    /// rewrites a name the classifier reads, by containing it, by being
    /// part of it, or by overlapping its end, is refused, and so is one of
    /// a configured name or an empty `from`.
    #[test]
    fn stand_ins_may_not_rewrite_what_the_classifier_reads() {
        let with = |pairs: &str, extra: &str| {
            Config::parse(&format!(
                "[crates]\ndir = \"crates\"\ntest_features = [\"test-util\"]\nstand_ins = [{pairs}]\n{extra}"
            ))
        };
        for ok in [
            r#"["assert::", "assrtx::"]"#,
            r#"["let tracked =", "let trackd_ ="]"#,
            r#"["same", "same"]"#,
        ] {
            if let Err(e) = with(ok, "") {
                panic!("{ok}: got = {e}, wanted = ok");
            }
        }
        for (bad, extra, name) in [
            (r#"["external_body", "externalxbody"]"#, "", "external_body"),
            (r#"["assume(", "assum_("]"#, "", "assume"),
            (r#"["admit", "admix"]"#, "", "admit"),
            (
                r#"["Tracked::assume_new", "Tracked::assume_neW"]"#,
                "",
                "assume_new",
            ),
            (r#"["verus_verify", "verus_verifx"]"#, "", "verus_verify"),
            (r#"["verus_spec", "verus_spex"]"#, "", "verus_spec"),
            (r#"["discharged_by", "discharged_bx"]"#, "", "discharged_by"),
            (r#"["body]", "bodx]"]"#, "", "external_body"),
            (r#"["y]", "x]"]"#, "", "external_body"),
            (r##"["#[cfg(", "#[cfx("]"##, "", "cfg"),
            (r#"["(test)", "(tesx)"]"#, "", "test"),
            (r#"["\"test-util\"", "\"test-utix\""]"#, "", "test-util"),
            (r#"["fn ", "fx "]"#, "", "fn"),
            (r#"["unsafe(", "unsafx("]"#, "", "unsafe"),
            (r#"["", "x"]"#, "", "external_body"),
            (
                r#"["(miri)", "(mirx)"]"#,
                "verus_never_sets = [\"miri\"]\n",
                "miri",
            ),
            (
                r#"["\"jit\"", "\"jix\""]"#,
                "verus_sets = ['feature = \"jit\"']\n",
                "jit",
            ),
            (
                r#"["\"linux\"", "\"linuz\""]"#,
                "verus_sets = ['target_os = \"linux\"']\n",
                "linux",
            ),
            (
                r#"["c_mint(", "c_minx("]"#,
                "[mints]\npath = \"m\"\nmint = \"c_mint\"\ntoken_trait = \"CToken\"\nhome = \"h\"\n",
                "c_mint",
            ),
        ] {
            match with(bad, extra) {
                Ok(_) => panic!("{bad}: got = ok, wanted = refused"),
                Err(e) => assert!(
                    e.contains(&format!("would rewrite `{name}`")),
                    "{bad}: got = {e}, wanted = rewrites `{name}`"
                ),
            }
        }
    }

    /// pcre3's configuration, which every gate section has, with a member
    /// and a Verus feature so that every `[crates]` key it holds is set.
    const FULL: &str = r#"
[crates]
dir = "crates"
members = [{ name = "extra", path = "extra", ships = true }]
test_cfgs = ["test", "kani"]
test_features = ["test-util"]
verus_features = ["verus"]
verus_sets = ['feature = "jit"']
verus_never_sets = ["miri"]
stand_ins = [["assert::", "assrtx::"]]
[recursion]
table = "verus/theorems/RN1.md"
[reach]
path = "verus/REACH"
exports_macro = "exports"
fn_ptr_macro = "fn_ptr"
conversion = "from_c"
[inputs]
table = "verus/theorems/CD1.md"
crates = ["pcre3", "pcre3-syntax", "pcre3-unicode"]
roots = [
  { file = "crates/pcre3/src/regex.rs", owner = "Regex", name = "new" },
  { file = "crates/pcre3/src/serialize.rs", name = "encode_as" },
]
lazy = true
describe = "Regex::new"
[badge]
path = "README.md"
[mints]
path = "verus/MINTS"
mint = "c_mint"
token_trait = "CToken"
home = "crates/pcre3-capi/src/object.rs"
"#;

    /// A configuration that keeps every gate, or widens or renames one,
    /// is not narrowed. Negative: removing each gate section, a `[reach]`
    /// key, an `[inputs]` crate or root, or `lazy`, renaming a `[reach]`
    /// key's macro or conversion, removing or changing `[crates] dir`,
    /// dropping a member, moving it or turning its `ships` off (also both
    /// at once, under a new name at its path, or for a crate of the base's
    /// `dir`), and adding a `test_cfgs`, `test_features` or
    /// `verus_features` name, a `verus_sets` or `verus_never_sets` atom,
    /// or a `stand_ins` pair, each names what it turned off. Removing or
    /// respacing a `verus_sets` or `verus_never_sets` atom is not.
    #[test]
    fn a_gate_removed_or_narrowed_since_the_base_fails() {
        let shown = |n: &[Narrowing]| n.iter().map(ToString::to_string).collect::<Vec<_>>();
        let base = Config::parse(FULL).expect("parses");
        let edit = |from: &str, to: &str| {
            assert!(FULL.contains(from), "{from:?} is not in the fixture");
            Config::parse(&FULL.replacen(from, to, 1)).expect("parses")
        };
        for (from, to) in [
            ("", ""),
            ("\"pcre3-unicode\"]", "\"pcre3-unicode\", \"pcre3-capi\"]"),
            ("verus/theorems/RN1.md", "verus/theorems/RN2.md"),
            ("mint = \"c_mint\"", "mint = \"c_make\""),
            ("path = \"verus/REACH\"", "path = \"verus/reach/REACH\""),
            (
                "[recursion]",
                "[floor]\npath = \"verus/SHARES\"\n[rlimit]\npath = \"verus/HEADROOM\"\n[recursion]",
            ),
            (
                "ships = true }]",
                "ships = true }, { name = \"more\", path = \"more\" }]",
            ),
            // A new crate the base does not read may start unshipped, and
            // a crate of the base's `dir` may become a member as it was.
            (
                "ships = true }]",
                "ships = true }, { name = \"more\", path = \"more\", ships = false }]",
            ),
            (
                "dir = \"crates\"\nmembers = [",
                "dir = \"crates\"\nmembers = [{ name = \"b\", path = \"vendor/b\", ships = false }, ",
            ),
            ("stand_ins = [[\"assert::\", \"assrtx::\"]]\n", ""),
            ("\"test\", \"kani\"]", "\"test\"]"),
            ("test_features = [\"test-util\"]\n", ""),
            ("verus_features = [\"verus\"]\n", ""),
            // The badge's path spelt another way, or left to its default.
            ("path = \"README.md\"", "path = \"./README.md\""),
            ("path = \"README.md\"\n", ""),
            ("verus_sets = ['feature = \"jit\"']\n", ""),
            ("verus_never_sets = [\"miri\"]\n", ""),
            ("'feature = \"jit\"'", "'feature=\"jit\"'"),
            ("[\"miri\"]", "[\" miri \"]"),
        ] {
            let got = edit(from, to).narrowed_from(&base);
            assert!(got.is_empty(), "{to:?}: got = {got:?}, wanted = none");
        }
        let section = |name: &str| {
            let start = FULL.find(&format!("[{name}]")).expect("section");
            let end = FULL[start + 1..]
                .find("\n[")
                .map_or(FULL.len(), |e| start + 1 + e + 1);
            FULL[start..end].to_owned()
        };
        // No base gate: nothing to narrow.
        let bare = Config::parse(&section("crates")).expect("parses");
        assert!(base.narrowed_from(&bare).is_empty());
        for name in ["recursion", "reach", "inputs", "mints", "badge"] {
            let got = shown(&edit(&section(name), "").narrowed_from(&base));
            let want = format!("the [{name}] section is gone, so its gate no longer runs");
            assert_eq!(
                got,
                std::slice::from_ref(&want),
                "[{name}]: got = {got:?}, wanted = {want:?}"
            );
        }
        for (from, to, want) in [
            (
                "exports_macro = \"exports\"\n",
                "",
                "[reach] exports_macro is gone",
            ),
            (
                "fn_ptr_macro = \"fn_ptr\"\n",
                "",
                "[reach] fn_ptr_macro is gone",
            ),
            (
                "conversion = \"from_c\"\n",
                "",
                "[reach] conversion is gone",
            ),
            (
                "exports_macro = \"exports\"",
                "exports_macro = \"exportz\"",
                "[reach] exports_macro changes from \"exports\" to \"exportz\"",
            ),
            (
                "fn_ptr_macro = \"fn_ptr\"",
                "fn_ptr_macro = \"fn_ptx\"",
                "[reach] fn_ptr_macro changes from \"fn_ptr\" to \"fn_ptx\"",
            ),
            (
                "conversion = \"from_c\"",
                "conversion = \"from_x\"",
                "[reach] conversion changes from \"from_c\" to \"from_x\"",
            ),
            (
                ", \"pcre3-unicode\"",
                "",
                "[inputs] crates drops \"pcre3-unicode\"",
            ),
            (
                "  { file = \"crates/pcre3/src/serialize.rs\", name = \"encode_as\" },\n",
                "",
                "[inputs] roots drops encode_as in crates/pcre3/src/serialize.rs",
            ),
            (
                "owner = \"Regex\", ",
                "",
                "[inputs] roots drops Regex::new in crates/pcre3/src/regex.rs",
            ),
            ("lazy = true\n", "", "[inputs] lazy is turned off"),
            (
                "path = \"README.md\"",
                "path = \"docs/README.md\"",
                "[badge] path changes from \"README.md\" to \"docs/README.md\"",
            ),
            (
                "dir = \"crates\"\n",
                "",
                "[crates] dir is no longer \"crates\"",
            ),
            (
                "dir = \"crates\"",
                "dir = \"vendor\"",
                "[crates] dir is no longer \"crates\"",
            ),
            (
                "{ name = \"extra\", path = \"extra\", ships = true }",
                "",
                "[crates] members drops \"extra\"",
            ),
            (
                "ships = true",
                "ships = false",
                "[crates] member \"extra\" changes ships from true to false",
            ),
            (
                ", ships = true",
                "",
                "[crates] member \"extra\" changes ships from true to unset",
            ),
            (
                "\"kani\"]",
                "\"kani\", \"unix\"]",
                "[crates] test_cfgs adds \"unix\"",
            ),
            (
                "[\"verus\"]",
                "[\"verus\", \"fast\"]",
                "[crates] verus_features adds \"fast\"",
            ),
            (
                "[\"test-util\"]",
                "[\"test-util\", \"hardened\"]",
                "[crates] test_features adds \"hardened\"",
            ),
            (
                "['feature = \"jit\"']",
                "['feature = \"jit\"', 'target_os = \"linux\"']",
                "[crates] verus_sets adds \"target_os = \\\"linux\\\"\"",
            ),
            (
                "[\"miri\"]",
                "[\"miri\", \"loom\"]",
                "[crates] verus_never_sets adds \"loom\"",
            ),
            (
                "path = \"extra\"",
                "path = \"crates/pcre3\"",
                "[crates] member \"extra\" moves from \"extra\" to \"crates/pcre3\"",
            ),
            // A rename that keeps the crate shipped, and a directory given
            // way to a member with `ships` unset, as the base read it,
            // change only what is read.
            (
                "name = \"extra\", path",
                "name = \"renamed\", path",
                "[crates] members drops \"extra\"",
            ),
            (
                "dir = \"crates\"\nmembers = [",
                "members = [{ name = \"a\", path = \"crates/a\" }, ",
                "[crates] dir is no longer \"crates\"",
            ),
            (
                "[[\"assert::\", \"assrtx::\"]]",
                "[[\"assert::\", \"assrtx::\"], [\"xyzzy::\", \"xyzzz::\"]]",
                "[crates] stand_ins adds [\"xyzzy::\", \"xyzzz::\"]",
            ),
        ] {
            let got = shown(&edit(from, to).narrowed_from(&base));
            assert!(
                got.len() == 1 && got[0].starts_with(want),
                "{from:?} to {to:?}: got = {got:?}, wanted = [{want}..]"
            );
        }
        // A member's `path` and `ships` are each compared, so a move that
        // also unships it makes two narrowings. A member under a new name
        // at a base member's path, or in the base's `dir`, is the crate
        // the base read there: turning its `ships` off is a narrowing of
        // its own, beside the drop or the directory change.
        for (from, to, want) in [
            (
                "{ name = \"extra\", path = \"extra\", ships = true }",
                "{ name = \"extra\", path = \"moved\", ships = false }",
                &[
                    "[crates] member \"extra\" moves from \"extra\" to \"moved\"",
                    "[crates] member \"extra\" changes ships from true to false",
                ][..],
            ),
            (
                "{ name = \"extra\", path = \"extra\", ships = true }",
                "{ name = \"renamed\", path = \"./extra/\", ships = false }",
                &[
                    "[crates] members drops \"extra\"",
                    "[crates] member \"renamed\" at \"./extra/\" changes ships from true to false",
                ][..],
            ),
            (
                "{ name = \"extra\", path = \"extra\", ships = true }",
                "{ name = \"renamed\", path = \"extra\" }",
                &[
                    "[crates] members drops \"extra\"",
                    "[crates] member \"renamed\" at \"extra\" changes ships from true to unset",
                ][..],
            ),
            (
                "dir = \"crates\"\nmembers = [",
                "members = [{ name = \"a\", path = \"crates/a\", ships = false }, ",
                &[
                    "[crates] dir is no longer \"crates\"",
                    "[crates] member \"a\" at \"crates/a\" changes ships from unset to false",
                ][..],
            ),
        ] {
            let got = shown(&edit(from, to).narrowed_from(&base));
            let changes: Vec<&str> = got
                .iter()
                .map(|g| g.split(", so ").next().unwrap_or_default())
                .collect();
            assert_eq!(
                changes, want,
                "{from:?} to {to:?}: got = {got:?}, wanted = {want:?}"
            );
        }
        // The path comparison holds under any name, both ways between `dir`
        // and members: a base member (here unshipped) moved onto a crate of
        // the base's `dir` that shipped by its Cargo.toml, and a
        // member with `ships = true` handed to a `dir` that reads it with
        // `ships` unset. Negative: a member handed to a `dir` with `ships`
        // unset before and after is only the drop.
        let crates = |text: &str| Config::parse(&format!("[crates]\n{text}")).expect("parses");
        for (base, head, want) in [
            (
                "dir = \"crates\"\nmembers = [{ name = \"extra\", path = \"extra\", ships = false }]\n",
                "members = [{ name = \"extra\", path = \"crates/a\", ships = false }]\n",
                &[
                    "[crates] dir is no longer \"crates\"",
                    "[crates] member \"extra\" moves from \"extra\" to \"crates/a\"",
                    "[crates] member \"extra\" at \"crates/a\" changes ships from unset to false",
                ][..],
            ),
            (
                "members = [{ name = \"a\", path = \"crates/a\", ships = true }]\n",
                "dir = \"crates\"\n",
                &[
                    "[crates] members drops \"a\"",
                    "[crates] crate at \"crates/a\" through dir \"crates\" changes ships from true to unset",
                ][..],
            ),
            (
                "members = [{ name = \"a\", path = \"crates/a\" }]\n",
                "dir = \"crates\"\n",
                &["[crates] members drops \"a\""][..],
            ),
            // A member under a name the base has, taking over another base
            // member's path, is compared with the crate the base read there.
            (
                "members = [{ name = \"a\", path = \"a\", ships = true }, { name = \"extra\", path = \"extra\" }]\n",
                "members = [{ name = \"extra\", path = \"a\", ships = false }]\n",
                &[
                    "[crates] members drops \"a\"",
                    "[crates] member \"extra\" moves from \"extra\" to \"a\"",
                    "[crates] member \"extra\" changes ships from unset to false",
                    "[crates] member \"extra\" at \"a\" changes ships from true to false",
                ][..],
            ),
        ] {
            let got = shown(&crates(head).narrowed_from(&crates(base)));
            let changes: Vec<&str> = got
                .iter()
                .map(|g| g.split(", so ").next().unwrap_or_default())
                .collect();
            assert_eq!(
                changes, want,
                "{base:?} to {head:?}: got = {got:?}, wanted = {want:?}"
            );
        }
    }

    /// A narrowing passes when a new `[[narrowed]]` line names its change,
    /// as a lowered FLOOR figure passes with a `lowered` line the base does
    /// not have; a narrowing made again is named by one more line than the
    /// base has with its change. Negative: no line, a line naming another
    /// change, and a line the base already has, as written or reworded (an
    /// old reason never covers a new narrowing), each leave it unnamed; a
    /// new line that names no narrowing, or a second for one narrowing, is
    /// stale; a line carried from the base with nothing narrowed is
    /// history, as FLOOR keeps its `lowered` lines.
    #[test]
    fn a_narrowing_passes_only_with_a_new_reason_line() {
        let mints = "the [mints] section is gone";
        let line = |change: &str| {
            format!("[[narrowed]]\nchange = {change:?}\nreason = \"the C API keeps no tokens\"\n")
        };
        let start = FULL.find("[mints]").expect("section");
        let without = &FULL[..start];
        let cfg = |text: &str| Config::parse(text).expect("parses");
        let base = cfg(FULL);
        let got = cfg(&format!("{without}{}", line(mints))).explain(&base, Vec::new());
        assert_eq!(
            (got.named.len(), got.unnamed.len(), got.stale.len()),
            (1, 0, 0),
            "named: got = {got:?}"
        );
        assert_eq!(got.named[0].0.change, mints);
        assert_eq!(got.named[0].1, "the C API keeps no tokens");
        let reach = "the [reach] section is gone";
        for (text, base, unnamed, stale) in [
            (without.to_owned(), FULL.to_owned(), vec![mints], vec![]),
            (
                format!("{without}{}", line(reach)),
                FULL.to_owned(),
                vec![mints],
                vec![reach],
            ),
            (
                format!("{FULL}{}", line(mints)),
                FULL.to_owned(),
                vec![],
                vec![mints],
            ),
            (
                format!("{without}{}", line(mints)),
                format!("{FULL}{}", line(mints)),
                vec![mints],
                vec![],
            ),
            (
                format!("{FULL}{}", line(mints)),
                format!("{FULL}{}", line(mints)),
                vec![],
                vec![],
            ),
        ] {
            let got = cfg(&text).explain(&cfg(&base), Vec::new());
            let got_unnamed: Vec<&str> = got.unnamed.iter().map(|n| n.change.as_str()).collect();
            assert!(
                got.named.is_empty() && got_unnamed == unnamed && got.stale == stale,
                "{text:?} against {base:?}: got = {got:?}, wanted = unnamed {unnamed:?}, stale {stale:?}"
            );
        }
        // A narrowing made again after a base line named it (the section
        // restored, then retired again) is named by one more line with that
        // change, whichever order the two lines take, and the run prints the
        // new reason; the history line alone, or reworded, still names
        // nothing new. Two new lines for one narrowing leave one stale.
        let again =
            |reason: &str| format!("[[narrowed]]\nchange = {mints:?}\nreason = {reason:?}\n");
        let history = cfg(&format!("{FULL}{}", line(mints)));
        for (text, named, unnamed, stale) in [
            (
                format!("{without}{}{}", line(mints), again("retired again")),
                Some("retired again"),
                0,
                0,
            ),
            (
                format!("{without}{}{}", again("retired again"), line(mints)),
                Some("retired again"),
                0,
                0,
            ),
            (format!("{without}{}", line(mints)), None, 1, 0),
            (format!("{without}{}", again("reworded")), None, 1, 0),
            (
                format!(
                    "{without}{}{}{}",
                    line(mints),
                    again("retired again"),
                    again("twice")
                ),
                Some("retired again"),
                0,
                1,
            ),
        ] {
            let got = cfg(&text).explain(&history, Vec::new());
            let got_named = got.named.first().map(|(_, r)| r.as_str());
            assert!(
                got_named == named && got.unnamed.len() == unnamed && got.stale.len() == stale,
                "{text:?}: got = {got:?}, wanted = named {named:?}, {unnamed} unnamed, {stale} stale"
            );
        }
        // A renamed member: a line for the drop leaves the shipping change
        // unnamed.
        let renamed = FULL.replacen(
            "{ name = \"extra\", path = \"extra\", ships = true }",
            "{ name = \"renamed\", path = \"extra\", ships = false }",
            1,
        );
        let got = cfg(&format!(
            "{renamed}{}",
            line("[crates] members drops \"extra\"")
        ))
        .explain(&base, Vec::new());
        let got_unnamed: Vec<&str> = got.unnamed.iter().map(|n| n.change.as_str()).collect();
        assert!(
            got.named.len() == 1
                && got_unnamed
                    == [
                        "[crates] member \"renamed\" at \"extra\" changes ships from true to false"
                    ],
            "a rename that unships: got = {got:?}, wanted = the drop named, the shipping change unnamed"
        );
        for (text, want) in [
            (
                "[[narrowed]]\nchange = \"x\"\nreason = \" \"\n",
                "needs a reason",
            ),
            (
                "[[narrowed]]\nchange = \"\"\nreason = \"why\"\n",
                "needs a change",
            ),
            ("[[narrowed]]\nchange = \"x\"\n", "missing field `reason`"),
            (
                "[[narrowed]]\nchange = \"x\"\nreason = \"why\"\nwhen = \"now\"\n",
                "unknown field",
            ),
        ] {
            let got = Config::parse(&format!("{without}{text}"))
                .map(|_| ())
                .unwrap_err();
            assert!(got.contains(want), "{text:?}: got = {got}, wanted = {want}");
        }
    }

    #[test]
    fn stand_ins_keep_length() {
        let pairs = [["assert::".to_string(), "assrtx::".to_string()]];
        let got = stand_in(&pairs, "use assert::x; assert::y();");
        assert_eq!(got, "use assrtx::x; assrtx::y();");
    }
}
