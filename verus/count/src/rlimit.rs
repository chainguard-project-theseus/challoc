//! The rlimit headroom check (verus/README.md, "Headroom"): how much of its
//! SMT resource limit each verified function uses, from the statistics
//! `cargo verus verify -- --time-expanded --output-json` prints, held to
//! the share verus/RLIMIT allows.
//!
//! Z3's rlimit count is deterministic for a given query, so a function at
//! 95% of its limit passes every run of its own pull request and fails the
//! first time a change to a function it shares a module with (Verus runs a
//! module's functions in one Z3 process) or to a definition its proof
//! unfolds tips it over. The check fails a pull request that leaves any
//! function above the allowed share, so the headroom is restored before
//! merge rather than on main.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// Z3's resource count per unit of `--rlimit` or `#[verifier::rlimit(N)]`
/// (rust_verify's `RLIMIT_PER_SECOND_Z3`).
const PER_UNIT: f32 = 3_000_000.0;

/// The limit a function runs under with no `#[verifier::rlimit]`: Verus's
/// `--rlimit` default, which `make verus` does not change.
pub const DEFAULT_RLIMIT: f32 = 10.0;

/// The rlimit count a limit of `units` allows, as Verus computes it.
pub fn count_for(units: f32) -> u64 {
    (units * PER_UNIT).min(u32::MAX as f32) as u64
}

/// What one crate's statistics say: the functions Verus checked, by the
/// name it gives them (`crate::module::Type::name`), with the rlimit count
/// each used, summed over its queries; the names it gives more than one
/// function; and its result line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CrateStats {
    pub used: BTreeMap<String, u64>,
    pub folded: BTreeSet<String>,
    pub verified: u64,
    pub errors: u64,
}

/// Reads the JSON documents `--output-json` prints, one per crate Verus
/// checks (vstd's too, when cargo rebuilds it), into the statistics of
/// each crate a function is named under, as Verus spells the crate
/// (`pcre3_syntax`). A document's result line goes to the crate most of
/// its functions are named under. A document with no per-function
/// breakdown (Verus run without `--time-expanded`) is an error.
pub fn read_stats(text: &str) -> Result<BTreeMap<String, CrateStats>, String> {
    let mut out: BTreeMap<String, CrateStats> = BTreeMap::new();
    for doc in serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>() {
        let doc = doc.map_err(|e| format!("not Verus's --output-json: {e}"))?;
        let modules = doc
            .pointer("/times-ms/smt/smt-run-module-times")
            .and_then(|m| m.as_array())
            .ok_or("no per-module SMT times (run Verus with --time-expanded --output-json)")?;
        let mut per_crate: BTreeMap<String, usize> = BTreeMap::new();
        for m in modules {
            let fns = m
                .get("function-breakdown")
                .and_then(|f| f.as_array())
                .ok_or("a module with no function-breakdown (run Verus with --time-expanded)")?;
            for f in fns {
                let (Some(name), Some(used)) = (
                    f.get("function").and_then(|n| n.as_str()),
                    f.get("rlimit").and_then(|r| r.as_u64()),
                ) else {
                    return Err(format!("a function with no name or rlimit count: {f}"));
                };
                let k = name.split("::").next().unwrap_or("").to_owned();
                *per_crate.entry(k.clone()).or_default() += 1;
                // Two functions Verus gives one friendly name (one trait's
                // impls for two instances of a type) keep the larger count,
                // and the name is noted: [`shares`] refuses one a limit
                // names, as that limit may be the other function's.
                let s = out.entry(k).or_default();
                match s.used.entry(name.to_owned()) {
                    Entry::Vacant(e) => {
                        e.insert(used);
                    }
                    Entry::Occupied(mut e) => {
                        let v = e.get_mut();
                        *v = (*v).max(used);
                        s.folded.insert(name.to_owned());
                    }
                }
            }
        }
        let Some((krate, _)) = per_crate.iter().max_by_key(|(_, n)| **n) else {
            continue;
        };
        let results = doc.get("verification-results");
        let field = |k: &str| results.and_then(|r| r.get(k)).and_then(|v| v.as_u64());
        let s = out.entry(krate.clone()).or_default();
        s.verified = field("verified").unwrap_or(0);
        s.errors = field("errors").unwrap_or(0);
    }
    Ok(out)
}

/// A function's `#[verifier::rlimit(N)]`, read from its source: the name
/// Verus gives it, where it is, and N (`None` for `infinity`).
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub name: String,
    pub at: String,
    pub units: Option<f32>,
}

/// verus/RLIMIT: the largest share of its limit a function may use, in
/// tenths of a percent; the functions allowed above it, each with the
/// share it may not exceed; and the `#[verifier::rlimit(N)]` raises it
/// gives a reason for, as a function's name and N (`None` for
/// `infinity`).
#[derive(Debug, Default, PartialEq)]
pub struct RlimitFile {
    pub max_share: u32,
    pub allow: BTreeMap<String, u32>,
    pub raised: Vec<(String, Option<f32>)>,
}

/// `N` or `infinity`, as `#[verifier::rlimit(..)]` takes it.
pub fn parse_units(s: &str) -> Option<Option<f32>> {
    if s == "infinity" {
        return Some(None);
    }
    s.parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(Some)
}

/// Reads verus/RLIMIT: `#` comments, one `max-share P` line, any number of
/// `allow NAME P` lines (NAME as Verus names the function, P above
/// `max-share`; P may pass 100, to 1000, as the share sums a function's
/// queries, each under the limit on its own), and any number of
/// `rlimit-raised NAME N REASON` lines.
pub fn parse_rlimit_file(text: &str) -> Result<RlimitFile, String> {
    let mut max_share = None;
    let mut allow = BTreeMap::new();
    let mut raised = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let n = i + 1;
        let share = |v: &str| {
            crate::parse_tenths_to(v, 10_000)
                .ok_or_else(|| format!("line {n}: {v:?} is not P or P.D"))
        };
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["max-share", p] => {
                if max_share.replace(share(p)?).is_some() {
                    return Err(format!("line {n}: max-share given twice"));
                }
            }
            ["allow", name, p] => {
                if allow.insert((*name).to_owned(), share(p)?).is_some() {
                    return Err(format!("line {n}: {name} allowed twice"));
                }
            }
            ["rlimit-raised", name, units, reason @ ..] => {
                if reason.is_empty() {
                    return Err(format!("line {n}: an rlimit-raised line needs a reason"));
                }
                let u = parse_units(units)
                    .ok_or_else(|| format!("line {n}: {units:?} is not a number or infinity"))?;
                raised.push(((*name).to_owned(), u));
            }
            _ => {
                return Err(format!(
                    "line {n}: want `max-share P`, `allow NAME P` or `rlimit-raised NAME N REASON`"
                ));
            }
        }
    }
    let max_share = max_share.ok_or("want a max-share line")?;
    if let Some((name, _)) = allow.iter().find(|(_, p)| **p <= max_share) {
        return Err(format!(
            "{name} is allowed no more than max-share; drop its allow line"
        ));
    }
    Ok(RlimitFile {
        max_share,
        allow,
        raised,
    })
}

/// A function's use of its limit: its name, the count it used, the count
/// it is allowed, and the share, in tenths of a percent rounded up.
#[derive(Debug, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub used: u64,
    pub limit: u64,
    pub tenths: u32,
}

/// Every function in `stats` of one of `crates` with the share of its limit
/// it used, the largest first. A function's limit is its
/// `#[verifier::rlimit]`, from `limits`, or the default; one with
/// `infinity` has none and is left out. A limit is an error when it names
/// no function the statistics have, in a crate they cover (the check would
/// hold that function to the default), or a name two functions share, in
/// the source or in the statistics (the count kept may be the other's,
/// held to the wrong limit).
pub fn shares(
    stats: &BTreeMap<String, CrateStats>,
    crates: &[String],
    limits: &[Limit],
) -> Result<Vec<Share>, String> {
    let mut by_name: BTreeMap<&str, &Limit> = BTreeMap::new();
    for l in limits {
        if let Some(other) = by_name.insert(l.name.as_str(), l) {
            return Err(format!(
                "{} and {}: #[verifier::rlimit] on two functions Verus names {}",
                other.at, l.at, l.name
            ));
        }
    }
    for l in limits {
        let k = l.name.split("::").next().unwrap_or("");
        let Some(s) = stats.get(k) else { continue };
        if !s.used.contains_key(&l.name) {
            return Err(format!(
                "{}: #[verifier::rlimit] on {}, which Verus's statistics do not name",
                l.at, l.name
            ));
        }
        if s.folded.contains(&l.name) {
            return Err(format!(
                "{}: #[verifier::rlimit] on {}, a name Verus's statistics give two functions, so the count kept may be the other's",
                l.at, l.name
            ));
        }
    }
    let mut out = Vec::new();
    for s in crates.iter().filter_map(|k| stats.get(k)) {
        for (name, &used) in &s.used {
            let units = match by_name.get(name.as_str()) {
                Some(l) => l.units,
                None => Some(DEFAULT_RLIMIT),
            };
            let Some(units) = units else { continue };
            let limit = count_for(units);
            let tenths = u32::try_from((used * 1000).div_ceil(limit.max(1))).unwrap_or(u32::MAX);
            out.push(Share {
                name: name.clone(),
                used,
                limit,
                tenths,
            });
        }
    }
    out.sort_by(|a, b| b.tenths.cmp(&a.tenths).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Whether every share is within verus/RLIMIT, with a report: a line for
/// each function over its allowed share, naming it and its share, then the
/// `top` largest shares. An allowed function now within `max-share`, or
/// one the statistics do not name, is noted, so its allow line goes.
/// `list` is the RLIMIT file's path, for the report.
pub fn check(shares: &[Share], file: &RlimitFile, top: usize, list: &str) -> (bool, String) {
    let mut ok = true;
    let mut report = String::new();
    let pct = |t: u32| crate::shown(t);
    for s in shares {
        let allowed = file.allow.get(&s.name).copied();
        let cap = allowed.unwrap_or(file.max_share);
        if s.tenths > cap {
            ok = false;
            report += &format!(
                "FAIL rlimit headroom: {} uses {}% of its rlimit ({} of {}), above {}% ({}); split the proof, give it #[verifier::spinoff_prover], or narrow its context\n",
                s.name,
                pct(s.tenths),
                s.used,
                s.limit,
                pct(cap),
                if allowed.is_some() {
                    format!("its allow line in {list}")
                } else {
                    format!("{list}'s max-share")
                },
            );
        } else if allowed.is_some() && s.tenths <= file.max_share {
            report += &format!(
                "ok rlimit headroom: {} uses {}%, within max-share; drop its allow line from {list}\n",
                s.name,
                pct(s.tenths)
            );
        }
    }
    for name in file.allow.keys() {
        if !shares.iter().any(|s| &s.name == name) {
            report += &format!(
                "ok rlimit headroom: {list} allows {name}, which the statistics do not name\n"
            );
        }
    }
    report += &format!(
        "{} rlimit headroom: {} functions, max-share {}%; the largest shares:\n",
        if ok { "ok" } else { "FAIL" },
        shares.len(),
        pct(file.max_share)
    );
    for s in shares.iter().take(top) {
        report += &format!(
            "  {:>5}%  {:>10} of {:>10}  {}\n",
            pct(s.tenths),
            s.used,
            s.limit,
            s.name
        );
    }
    (ok, report)
}

/// Whether the statistics cover each of `crates`, with a line for each
/// they leave out. Cargo keeps a crate's earlier result and then Verus
/// prints nothing for it, so outside CI a missing crate is only named;
/// with `strict` (a run against a base revision, as the verus check's)
/// it fails, so statistics lost on the way (an empty file, Verus printing
/// them elsewhere) never pass for a check of nothing.
pub fn check_covered(
    crates: &[String],
    stats: &BTreeMap<String, CrateStats>,
    strict: bool,
) -> (bool, String) {
    let mut report = String::new();
    let missing: Vec<&String> = crates.iter().filter(|k| !stats.contains_key(*k)).collect();
    for k in &missing {
        report += &format!(
            "{} rlimit headroom: no statistics for {k}{}\n",
            if strict { "FAIL" } else { "ok" },
            if strict {
                String::new()
            } else {
                format!(
                    ", whose earlier result cargo kept (`cargo clean -p {}` checks it again)",
                    k.replace('_', "-")
                )
            }
        );
    }
    (!strict || missing.is_empty(), report)
}

/// `N` as written in `#[verifier::rlimit(N)]`, or `infinity`.
fn shown_units(u: Option<f32>) -> String {
    u.map_or("infinity".into(), |v| v.to_string())
}

/// Whether no function's limit rose past the base revision's without a
/// reason, with a line for each that rose: a function's limit in `head`
/// (its `#[verifier::rlimit(N)]`, or the default without one) above the
/// one it had in `base` (the default for a function `base` gives none)
/// passes only with an `rlimit-raised NAME N REASON` line in `head_file`
/// for that function and N that `base_file` does not have. A function of
/// `present` (every function Verus checks in the head, with where it
/// starts) that lost its attribute runs under the default, so dropping a
/// limit below the default raises it the same way. A raised limit lowers
/// the share as well as an allow line would, so it is held the same way.
/// `list` is the RLIMIT file's path, for the report.
pub fn check_raised(
    head: &[Limit],
    present: &BTreeMap<String, String>,
    base: &[Limit],
    head_file: &RlimitFile,
    base_file: &RlimitFile,
    list: &str,
) -> (bool, String) {
    // `None` (infinity) above every number.
    let key = |u: Option<f32>| u.unwrap_or(f32::INFINITY);
    // Each head function's limit and where it starts: its attribute's, or
    // the default for a base limit whose attribute is gone.
    let dropped = base.iter().filter_map(|b| {
        let at = present.get(&b.name)?;
        (!head.iter().any(|h| h.name == b.name)).then_some((&b.name, at, Some(DEFAULT_RLIMIT)))
    });
    let now: Vec<(&String, &String, Option<f32>)> = head
        .iter()
        .map(|h| (&h.name, &h.at, h.units))
        .chain(dropped)
        .collect();
    let mut ok = true;
    let mut report = String::new();
    for (name, at, units) in now {
        let was = base
            .iter()
            .find(|b| b.name == *name)
            .map_or(Some(DEFAULT_RLIMIT), |b| b.units);
        if key(units) <= key(was) {
            continue;
        }
        let line = (name.clone(), units);
        if head_file.raised.contains(&line) && !base_file.raised.contains(&line) {
            report += &format!(
                "ok rlimit headroom: {at} raises {name}'s rlimit from {} to {}, with a reason\n",
                shown_units(was),
                shown_units(units)
            );
        } else {
            ok = false;
            report += &format!(
                "FAIL rlimit headroom: {at} raises {name}'s rlimit from {} to {} with no new `rlimit-raised {name} {} <reason>` line in {list}\n",
                shown_units(was),
                shown_units(units),
                shown_units(units)
            );
        }
    }
    (ok, report)
}

/// Whether verus/RLIMIT keeps the base revision's bounds: `max-share` no
/// higher, no allow line the base does not have, and none raised. The
/// allowlist only shrinks. `list` is the RLIMIT file's path, for the
/// report.
pub fn check_ratchet(head: &RlimitFile, base: &RlimitFile, list: &str) -> (bool, String) {
    let mut ok = true;
    let mut report = String::new();
    if head.max_share > base.max_share {
        ok = false;
        report += &format!(
            "FAIL rlimit headroom: {list} raises max-share from {}% to {}%\n",
            crate::shown(base.max_share),
            crate::shown(head.max_share)
        );
    }
    for (name, &p) in &head.allow {
        match base.allow.get(name) {
            None => {
                ok = false;
                report += &format!(
                    "FAIL rlimit headroom: {list} allows {name}, which the base does not; restore its headroom instead\n"
                );
            }
            Some(&b) if p > b => {
                ok = false;
                report += &format!(
                    "FAIL rlimit headroom: {list} raises {name}'s allowed share from {}% to {}%\n",
                    crate::shown(b),
                    crate::shown(p)
                );
            }
            Some(_) => {}
        }
    }
    (ok, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One crate's `--output-json` document, its functions given as
    /// `(name, rlimit count)`.
    fn doc(fns: &[(&str, u64)], verified: u64) -> String {
        let breakdown: Vec<String> = fns
            .iter()
            .map(|(n, r)| {
                format!(r#"{{"function":"{n}","mode:":"exec","time":1,"time-micros":1,"rlimit":{r},"success":true}}"#)
            })
            .collect();
        format!(
            r#"{{"times-ms":{{"smt":{{"smt-run-module-times":[{{"module":"m","rlimit":0,"function-breakdown":[{}]}}]}}}},"verification-results":{{"success":true,"verified":{verified},"errors":0}}}}"#,
            breakdown.join(",")
        )
    }

    fn file(text: &str) -> RlimitFile {
        parse_rlimit_file(text).expect("parses")
    }

    #[test]
    fn statistics_are_read_per_crate() {
        let text = format!(
            "{}\n{}",
            doc(&[("vstd::seq::lemma", 5), ("core::x", 1)], 9),
            doc(
                &[("k::a", 7), ("k::T::b", 3), ("k::a", 2), ("vstd::y", 1)],
                4
            )
        );
        let got = read_stats(&text).expect("reads");
        let k = &got["k"];
        assert_eq!(
            k.used["k::a"], 7,
            "k::a: got = {}, wanted = 7",
            k.used["k::a"]
        );
        assert_eq!(
            k.used.len(),
            2,
            "k functions: got = {}, wanted = 2",
            k.used.len()
        );
        assert_eq!(
            k.verified, 4,
            "k verified: got = {}, wanted = 4",
            k.verified
        );
        assert_eq!(
            got["vstd"].verified, 9,
            "vstd verified: got = {}, wanted = 9",
            got["vstd"].verified
        );
        // Negative: not JSON, and a document without --time-expanded.
        assert!(
            read_stats("verification results:: 3 verified").is_err(),
            "plain text: got = ok, wanted = an error"
        );
        assert!(
            read_stats(r#"{"verification-results":{"verified":1}}"#).is_err(),
            "no breakdown: got = ok, wanted = an error"
        );
    }

    #[test]
    fn the_rlimit_file_parses_and_rejects_what_it_should() {
        let got = file("# c\nmax-share 70\nallow k::a 85.5 # why\n");
        assert_eq!(
            file("max-share 70\nallow k::a 115\n").allow.get("k::a"),
            Some(&1150),
            "allow above 100: got = none, wanted = 1150"
        );
        assert_eq!(
            got.max_share, 700,
            "max-share: got = {}, wanted = 700",
            got.max_share
        );
        assert_eq!(
            got.allow.get("k::a"),
            Some(&855),
            "allow: got = {:?}, wanted = 855",
            got.allow
        );
        for bad in [
            "",
            "allow k::a 80",
            "max-share 70\nmax-share 60",
            "max-share 70\nallow k::a 80\nallow k::a 90",
            "max-share 70\nallow k::a 60",
            "max-share 7x",
            "max-share 1000.1",
            "max-share 70\nrlimit-raised k::a 30",
            "max-share 70\nrlimit-raised k::a lots why",
            "max-share 70 extra",
        ] {
            assert!(
                parse_rlimit_file(bad).is_err(),
                "{bad:?}: got = ok, wanted = an error"
            );
        }
    }

    #[test]
    fn shares_use_each_function_s_own_limit() {
        let stats = read_stats(&doc(
            &[
                ("k::big", 45_000_000),
                ("k::small", 3_000_000),
                ("k::free", 99_000_000_000),
                ("k::x", 0),
            ],
            4,
        ))
        .expect("reads");
        let limits = vec![
            Limit {
                name: "k::big".into(),
                at: "a.rs:1".into(),
                units: Some(60.0),
            },
            Limit {
                name: "k::free".into(),
                at: "a.rs:2".into(),
                units: None,
            },
        ];
        let got: Vec<(String, u32)> = shares(&stats, &["k".into()], &limits)
            .expect("shares")
            .into_iter()
            .map(|s| (s.name, s.tenths))
            .collect();
        // big: 45M of 180M; small: 3M of the default 30M; free has no limit.
        let want = vec![
            ("k::big".to_owned(), 250),
            ("k::small".to_owned(), 100),
            ("k::x".to_owned(), 0),
        ];
        assert_eq!(got, want, "shares: got = {got:?}, wanted = {want:?}");
        // A crate not asked for is left out.
        let got = shares(&stats, &["j".into()], &limits).expect("shares");
        assert!(got.is_empty(), "other crate: got = {got:?}, wanted = []");
        // Negative: a limit on a function the statistics do not name.
        let stray = vec![Limit {
            name: "k::gone".into(),
            at: "a.rs:3".into(),
            units: Some(30.0),
        }];
        assert!(
            shares(&stats, &["k".into()], &stray).is_err(),
            "stray limit: got = ok, wanted = an error"
        );
    }

    #[test]
    fn a_limit_on_a_shared_name_is_refused() {
        // Two functions Verus names k::T::f: one at 100M under rlimit(60)
        // (55%), one at 29M under the default (97%). Keeping the larger
        // count and the one limit would show 55% and hide the 97%.
        let stats = read_stats(&doc(
            &[("k::T::f", 100_000_000), ("k::T::f", 29_000_000)],
            2,
        ))
        .expect("reads");
        assert!(
            stats["k"].folded.contains("k::T::f"),
            "folded: got = {:?}, wanted = k::T::f",
            stats["k"].folded
        );
        let limit = |at: &str| Limit {
            name: "k::T::f".into(),
            at: at.into(),
            units: Some(60.0),
        };
        let got = shares(&stats, &["k".into()], &[limit("a.rs:1")]);
        assert!(
            got.as_ref()
                .is_err_and(|e| e.contains("give two functions")),
            "folded name with a limit: got = {got:?}, wanted = an error"
        );
        // Two limits on one name in the source, whatever the statistics.
        let stats = read_stats(&doc(&[("k::T::f", 1)], 1)).expect("reads");
        let got = shares(&stats, &["k".into()], &[limit("a.rs:1"), limit("b.rs:2")]);
        assert!(
            got.as_ref()
                .is_err_and(|e| e.contains("two functions Verus names")),
            "two limits on one name: got = {got:?}, wanted = an error"
        );
        // A folded name with no limit is held to the default by its larger
        // count, which is that function's own.
        let stats =
            read_stats(&doc(&[("k::g", 9_000_000), ("k::g", 3_000_000)], 2)).expect("reads");
        let got: Vec<u32> = shares(&stats, &["k".into()], &[])
            .expect("shares")
            .iter()
            .map(|s| s.tenths)
            .collect();
        assert_eq!(
            got,
            vec![300],
            "folded, no limit: got = {got:?}, wanted = [300]"
        );
    }

    #[test]
    fn the_check_names_each_function_over_its_share() {
        let share = |name: &str, tenths: u32| Share {
            name: name.into(),
            used: u64::from(tenths) * 30_000,
            limit: 30_000_000,
            tenths,
        };
        let all = vec![
            share("k::over", 812),
            share("k::allowed", 850),
            share("k::near", 700),
            share("k::low", 10),
        ];
        let (ok, report) = check(
            &all,
            &file("max-share 70\nallow k::allowed 86\n"),
            3,
            "x/RLIMIT",
        );
        assert!(!ok, "ok: got = true, wanted = false");
        assert!(
            report.contains("above 70.0% (x/RLIMIT's max-share)"),
            "report: got = {report}, wanted = the configured file named"
        );
        assert!(
            report.contains("FAIL rlimit headroom: k::over uses 81.2%"),
            "report: got = {report}, wanted = a FAIL naming k::over at 81.2%"
        );
        assert_eq!(
            report.matches("FAIL rlimit headroom: k::").count(),
            1,
            "FAIL lines: got = {report}, wanted = 1"
        );
        // The allowed function going past its own share fails too.
        let (ok, report) = check(
            &all,
            &file("max-share 82\nallow k::allowed 84\n"),
            3,
            "verus/RLIMIT",
        );
        assert!(
            !ok && report.contains("k::allowed uses 85.0%"),
            "allowed over: got = {report}, wanted = a FAIL"
        );
        // Within bounds, an allow line no longer needed is noted.
        let (ok, report) = check(
            &all,
            &file("max-share 90\nallow k::allowed 95\n"),
            3,
            "verus/RLIMIT",
        );
        assert!(ok, "ok: got = false, wanted = true ({report})");
        assert!(
            report.contains("drop its allow line"),
            "report: got = {report}, wanted = a note to drop k::allowed"
        );
    }

    #[test]
    fn the_rlimit_file_only_tightens() {
        let base = file("max-share 70\nallow k::a 90\nallow k::b 80\n");
        for (head, want) in [
            ("max-share 70\nallow k::a 90\n", true),
            ("max-share 65\nallow k::a 85\nallow k::b 80\n", true),
            ("max-share 75\nallow k::a 90\nallow k::b 80\n", false),
            ("max-share 70\nallow k::a 91\nallow k::b 80\n", false),
            ("max-share 70\nallow k::a 90\nallow k::c 80\n", false),
        ] {
            let (got, report) = check_ratchet(&file(head), &base, "verus/RLIMIT");
            assert_eq!(
                got, want,
                "{head:?}: got = {got}, wanted = {want} ({report})"
            );
        }
    }

    #[test]
    fn a_crate_without_statistics_fails_against_a_base() {
        let stats = read_stats(&doc(&[("k::a", 1)], 1)).expect("reads");
        let crates = vec!["k".to_owned(), "j".to_owned()];
        let (ok, report) = check_covered(&crates, &stats, false);
        assert!(ok, "no base: got = false, wanted = true ({report})");
        assert!(
            report.contains("ok rlimit headroom: no statistics for j"),
            "no base: got = {report}, wanted = a note for j"
        );
        let (ok, report) = check_covered(&crates, &stats, true);
        assert!(!ok, "against a base: got = true, wanted = false ({report})");
        assert!(
            report.contains("FAIL rlimit headroom: no statistics for j"),
            "against a base: got = {report}, wanted = a FAIL for j"
        );
        // An empty statistics file covers nothing.
        let (ok, _) = check_covered(&crates, &read_stats("").expect("reads"), true);
        assert!(!ok, "empty statistics: got = true, wanted = false");
        // Every crate covered passes either way.
        let (ok, report) = check_covered(&crates[..1], &stats, true);
        assert!(
            ok && report.is_empty(),
            "covered: got = {ok} {report:?}, wanted = true, no lines"
        );
    }

    #[test]
    fn a_raised_rlimit_needs_a_new_reason() {
        let limit = |name: &str, units: Option<f32>| Limit {
            name: name.into(),
            at: "a.rs:1".into(),
            units,
        };
        let base = vec![
            limit("k::a", Some(30.0)),
            limit("k::b", Some(60.0)),
            limit("k::d", Some(5.0)),
        ];
        let none = file("max-share 70\n");
        let reasoned = file("max-share 70\nrlimit-raised k::a 45 split pending\n");
        let to_default = file("max-share 70\nrlimit-raised k::d 10 default suffices\n");
        // The head functions Verus checks, beside those with a limit.
        let gone = BTreeMap::new();
        let present = |names: &[&str]| -> BTreeMap<String, String> {
            names
                .iter()
                .map(|n| (n.to_string(), "b.rs:2".to_string()))
                .collect()
        };
        for (head, there, head_file, base_file, want) in [
            // Kept, lowered, deleted, or new at or below the default.
            (vec![limit("k::a", Some(30.0))], &gone, &none, &none, true),
            (vec![limit("k::b", Some(20.0))], &gone, &none, &none, true),
            (vec![], &gone, &none, &none, true),
            (vec![limit("k::c", Some(10.0))], &gone, &none, &none, true),
            (
                vec![limit("k::d", Some(5.0))],
                &present(&["k::d"]),
                &none,
                &none,
                true,
            ),
            // An attribute above the default dropped: the limit falls.
            (vec![], &present(&["k::a", "k::b"]), &none, &none, true),
            // Raised, or new above the default, with no reason.
            (vec![limit("k::a", Some(45.0))], &gone, &none, &none, false),
            (vec![limit("k::c", Some(15.0))], &gone, &none, &none, false),
            (vec![limit("k::b", None)], &gone, &none, &none, false),
            // An attribute below the default dropped from a function that
            // is still there: the limit rises to the default.
            (vec![], &present(&["k::d"]), &none, &none, false),
            (vec![], &present(&["k::d"]), &to_default, &none, true),
            (vec![], &present(&["k::d"]), &to_default, &to_default, false),
            // Raised with a reason new in the change, or one the base had.
            (
                vec![limit("k::a", Some(45.0))],
                &gone,
                &reasoned,
                &none,
                true,
            ),
            (
                vec![limit("k::a", Some(45.0))],
                &gone,
                &reasoned,
                &reasoned,
                false,
            ),
            // A reason for another value does not cover it.
            (
                vec![limit("k::a", Some(50.0))],
                &gone,
                &reasoned,
                &none,
                false,
            ),
        ] {
            let (got, report) =
                check_raised(&head, there, &base, head_file, base_file, "verus/RLIMIT");
            assert_eq!(
                got, want,
                "{head:?} {there:?}: got = {got}, wanted = {want} ({report})"
            );
        }
        let (_, report) = check_raised(
            &[],
            &present(&["k::d"]),
            &base,
            &none,
            &none,
            "verus/RLIMIT",
        );
        assert_eq!(
            report,
            "FAIL rlimit headroom: b.rs:2 raises k::d's rlimit from 5 to 10 with no new `rlimit-raised k::d 10 <reason>` line in verus/RLIMIT\n"
        );
    }
}
