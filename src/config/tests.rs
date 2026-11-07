use super::*;
use crate::layout::{CLASSES, NBINS, NCLASSES};
use std::vec::Vec;

fn parse(text: &str) -> (Config, Vec<std::string::String>) {
    let mut cfg = Config::DEFAULT;
    let mut issues = Vec::new();
    let n = cfg.parse(text.as_bytes(), |i: Issue<'_>| {
        issues.push(std::format!("{i}"))
    });
    assert_eq!(n, issues.len());
    (cfg, issues)
}

fn clean(text: &str) -> Config {
    let (cfg, issues) = parse(text);
    assert!(issues.is_empty(), "{text}: {issues:?}");
    cfg
}

/// Every value in `bad` is refused for `key`: one issue, `invalid conf
/// value` (or `malformed` for the empty value, which has no
/// `key:value` shape), and the config left at the defaults.
fn rejects_bad_values(key: &str, bad: &[&str]) {
    for bad in bad {
        let text = std::format!("{key}:{bad}");
        let (cfg, issues) = parse(&text);
        assert_eq!(cfg, Config::DEFAULT, "{text} changed the config");
        assert_eq!(issues.len(), 1, "{text}: {issues:?}");
        let expect = if bad.is_empty() {
            "malformed"
        } else {
            "invalid conf value \""
        };
        assert!(issues[0].contains(expect), "{text}: {}", issues[0]);
    }
}

#[test]
fn defaults_match_the_design_table() {
    let d = Config::DEFAULT;
    assert_eq!(d.background_thread, None, "lazy start");
    assert_eq!(d.dirty_decay_ms, 10_000);
    assert_eq!(d.narenas, 0);
    assert_eq!(d.percpu_arena, PercpuArena::Disabled, "jemalloc's default");
    assert!(d.tcache);
    assert_eq!(d.tcache_max, 32 * 1024);
    assert_eq!(d.tcache_nslots_small_max, 1024);
    assert_eq!(d.lg_tcache_nslots_mul, 1);
    assert_eq!(d.thp, Thp::Never);
    assert!(d.lazy_band);
    assert!(!clean("lazy_band:false").lazy_band);
    assert!(d.guards);
    assert_eq!(d.guard_slab_interval, 8);
    assert_eq!(d.huge_retain_max, 64 << 20);
    assert_eq!(d.huge_retain_max, crate::huge::DEFAULT_RETAIN_MAX);
    assert!(
        !d.zero_on_free
            && !d.slot_randomize
            && !d.double_free_scan
            && !d.exact_sized_free
            && !d.abort_conf
            && !d.check_zero
    );
    assert!(!d.pac_pointers, "signed pointers are opt-in");
    assert!(d.cgroup);
    assert_eq!(d.effective_guard_slab_interval(), 8);
}

#[test]
fn empty_and_separator_only_input_changes_nothing() {
    for text in ["", ",", ",,,"] {
        assert_eq!(clean(text), Config::DEFAULT, "{text:?}");
    }
}

#[test]
fn every_bool_key_parses_true_and_false() {
    let keys = [
        "background_thread",
        "tcache",
        "lazy_band",
        "guards",
        "zero_on_free",
        "pac_pointers",
        "slot_randomize",
        "double_free_scan",
        "exact_sized_free",
        "abort_conf",
        "cgroup",
        "check_zero",
    ];
    for key in keys {
        for v in [true, false] {
            let text = std::format!("{key}:{v}");
            assert_eq!(
                clean(&text).get(key.as_bytes()),
                Some(Value::Bool(v)),
                "{key}:{v}"
            );
        }
        rejects_bad_values(key, &["yes", "1", "TRUE", "", "truex"]);
    }
}

/// `narenas:0` (the default) resolves to `min(4 * ncpus, 64)`; an
/// explicit value is taken as is.
#[test]
fn narenas_default_resolves_from_the_cpu_count() {
    let d = Config::DEFAULT;
    assert_eq!(d.effective_narenas(1), 4);
    assert_eq!(
        d.effective_narenas(0),
        4,
        "a failed probe counts as one CPU"
    );
    assert_eq!(d.effective_narenas(4), 16);
    assert_eq!(d.effective_narenas(16), 64);
    assert_eq!(d.effective_narenas(192), 64);
    assert_eq!(d.effective_narenas(u32::MAX), 64);
    assert_eq!(clean("narenas:1").effective_narenas(16), 1);
    assert_eq!(clean("narenas:16").effective_narenas(1), 16);
    assert_eq!(clean("narenas:0").effective_narenas(2), 8);
}

#[test]
fn every_numeric_key_parses_and_clamps() {
    // (key, in-range value, below-min input, clamped min, above-max input, clamped max)
    let cases: [(&str, i64, &str, i64, &str, i64); 9] = [
        (
            "dirty_decay_ms",
            5000,
            "-2",
            -1,
            "9999999999999999",
            MAX_INTERVAL_MS,
        ),
        (
            "huge_dirty_decay_ms",
            2000,
            "-2",
            -1,
            "9999999999999999",
            MAX_INTERVAL_MS,
        ),
        (
            "muzzy_decay_ms",
            0,
            "-5",
            -1,
            "9999999999999999",
            MAX_INTERVAL_MS,
        ),
        ("narenas", 4, "-1", 0, "99999999999", u32::MAX as i64),
        (
            "tcache_max",
            4096,
            "-1",
            0,
            "9999999",
            crate::layout::MEDIUM_MAX as i64,
        ),
        (
            "tcache_nslots_small_max",
            1000,
            "1",
            MIN_CAP as i64,
            "1000000",
            MAX_NSLOTS as i64,
        ),
        ("lg_tcache_nslots_mul", 3, "-17", -16, "17", 16),
        (
            "guard_slab_interval",
            2,
            "-1",
            0,
            "5000000",
            MAX_GUARD_SLAB_INTERVAL as i64,
        ),
        (
            "huge_retain_max",
            1 << 20,
            "-1",
            0,
            "9999999999999999",
            MAX_HUGE_RETAIN_MAX as i64,
        ),
    ];
    for (key, ok, low, min, high, max) in cases {
        let as_value = |v: i64| match key {
            "tcache_max" | "huge_retain_max" => Value::Size(v as usize),
            "narenas" | "tcache_nslots_small_max" | "guard_slab_interval" => Value::Uint(v as u64),
            _ => Value::Int(v),
        };
        let cfg = clean(&std::format!("{key}:{ok}"));
        assert_eq!(cfg.get(key.as_bytes()), Some(as_value(ok)), "{key}:{ok}");

        let (cfg, issues) = parse(&std::format!("{key}:{low}"));
        assert_eq!(
            cfg.get(key.as_bytes()),
            Some(as_value(min)),
            "{key}:{low} clamps to min"
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

        let (cfg, issues) = parse(&std::format!("{key}:{high}"));
        assert_eq!(
            cfg.get(key.as_bytes()),
            Some(as_value(max)),
            "{key}:{high} clamps to max"
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("out-of-range"), "{}", issues[0]);

        for bad in ["x", "1x", "0x", "--1", "1.5", " 1", "true"] {
            let (cfg, issues) = parse(&std::format!("{key}:{bad}"));
            assert_eq!(cfg, Config::DEFAULT, "{key}:{bad} changed the config");
            assert_eq!(issues.len(), 1, "{key}:{bad}");
            assert!(
                issues[0].contains("invalid conf value"),
                "{key}:{bad}: {}",
                issues[0]
            );
        }
    }
}

#[test]
fn integers_accept_hex_octal_and_signs() {
    assert_eq!(parse_int(b"0"), Some(0));
    assert_eq!(parse_int(b"42"), Some(42));
    assert_eq!(parse_int(b"+42"), Some(42));
    assert_eq!(parse_int(b"-1"), Some(-1));
    assert_eq!(parse_int(b"0x20"), Some(32));
    assert_eq!(parse_int(b"0X1f"), Some(31));
    assert_eq!(parse_int(b"010"), Some(8));
    assert_eq!(parse_int(b"9223372036854775807"), Some(i64::MAX));
    assert_eq!(parse_int(b"-9223372036854775808"), Some(i64::MIN));
    assert_eq!(parse_int(b"9223372036854775808"), None);
    assert_eq!(parse_int(b"-9223372036854775809"), None);
    assert_eq!(parse_int(b"99999999999999999999"), None);
    assert_eq!(parse_int(b""), None);
    assert_eq!(parse_int(b"-"), None);
    assert_eq!(parse_int(b"0x"), None);
    assert_eq!(parse_int(b"08"), None);
    assert_eq!(parse_int(b"1_000"), None);
    assert_eq!(
        clean("tcache_nslots_small_max:0x100").tcache_nslots_small_max,
        256
    );
}

#[test]
fn thp_values() {
    assert_eq!(clean("thp:always").thp, Thp::Always);
    assert_eq!(clean("thp:auto").thp, Thp::Auto);
    assert_eq!(clean("thp:never").thp, Thp::Never);
    assert_eq!(clean("thp:default").thp, Thp::Never);
    assert_eq!(
        clean("thp:always").get(b"thp"),
        Some(Value::Str(Name::Thp(Thp::Always)))
    );
    assert_eq!(
        clean("thp:auto").get(b"thp"),
        Some(Value::Str(Name::Thp(Thp::Auto)))
    );
    assert_eq!(Config::DEFAULT.thp, Thp::Never);
    let (cfg, issues) = parse("thp:madvise");
    assert_eq!(cfg.thp, Thp::Never);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("invalid conf value \"madvise\" for thp"));
    // Accepted under abort_conf.
    assert_eq!(clean("thp:auto,abort_conf:true").thp, Thp::Auto);
}

/// `percpu_arena`: jemalloc's three values, anything else reported and
/// left at the default.
#[test]
fn percpu_arena_values() {
    assert!(
        KEYS.contains(&"percpu_arena"),
        "a real key, readable through opt.percpu_arena"
    );
    for (value, want) in [
        ("disabled", PercpuArena::Disabled),
        ("percpu", PercpuArena::Percpu),
        ("phycpu", PercpuArena::Phycpu),
    ] {
        let cfg = clean(&std::format!("percpu_arena:{value},abort_conf:true"));
        assert_eq!(cfg.percpu_arena, want, "{value}");
        assert_eq!(
            cfg.get(b"percpu_arena"),
            Some(Value::Str(Name::PercpuArena(want))),
            "{value}"
        );
        assert_eq!(want.name().to_str(), Ok(value));
        assert!(cfg.abort_conf);
    }
    rejects_bad_values(
        "percpu_arena",
        &[
            "true", "false", "1", "on", "auto", "off", "PERCPU", "", "percpux",
        ],
    );
    // The last pair wins, as for every key.
    assert_eq!(
        clean("percpu_arena:percpu,percpu_arena:disabled").percpu_arena,
        PercpuArena::Disabled
    );
    assert_eq!(
        clean("percpu_arena:\"percpu\"").percpu_arena,
        PercpuArena::Percpu
    );
}

/// The one inert key: a bool by value, no field, unreadable, one
/// `Issue::Inert` per pair (what `abort_conf` aborts on), and in no
/// other table.
#[test]
fn inert_keys_are_reported_and_rejected_under_abort_conf() {
    assert_eq!(INERT_KEYS, &["stats_print"]);
    for key in INERT_KEYS {
        assert!(!KEYS.contains(key) && !IGNORED_KEYS.contains(key), "{key}");
        let value = "true";
        let text = std::format!("{key}:{value}");
        let (cfg, issues) = parse(&text);
        assert_eq!(issues.len(), 1, "{text}: {issues:?}");
        assert_eq!(
            issues[0],
            std::format!("conf key {key} is accepted but has no effect (not implemented)"),
            "{text}"
        );
        assert_eq!(cfg, Config::DEFAULT, "{key} has no field");
        assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
        let (cfg, issues) = parse(&std::format!("{key}:nonsense"));
        assert_eq!(cfg, Config::DEFAULT, "{key}:nonsense changed the config");
        assert_eq!(issues.len(), 1, "{key}:nonsense: {issues:?}");
        assert!(
            issues[0].contains("invalid conf value"),
            "{key}:nonsense: {}",
            issues[0]
        );
    }
    // The two hardening keys are real, opt-in keys: no report, stored,
    // readable, and `abort_conf` accepts them.
    for key in ["zero_on_free", "slot_randomize"] {
        let (cfg, issues) = parse(&std::format!("{key}:true,abort_conf:true"));
        assert!(issues.is_empty(), "{key}: {issues:?}");
        assert_eq!(cfg.get(key.as_bytes()), Some(Value::Bool(true)), "{key}");
        assert!(cfg.abort_conf);
    }
    let (cfg, _) = parse("zero_on_free:true,slot_randomize:true");
    assert!(cfg.zero_on_free && cfg.slot_randomize);
    // Mixed with real keys the real ones still apply and the inert ones
    // are counted: what `load` aborts on under `abort_conf`.
    let (cfg, issues) = parse("narenas:3,thp:always,cgroup:false,abort_conf:true,stats_print:true");
    assert_eq!(cfg.narenas, 3);
    assert!(cfg.abort_conf && !cfg.cgroup && cfg.thp == Thp::Always);
    assert_eq!(issues.len(), 1, "cgroup and thp are real keys: {issues:?}");
}

/// `huge_dirty_decay_ms` (design 7): unset it follows `dirty_decay_ms`
/// (the default reads as the slab period, and moves with it); set, it
/// is the huge tier's own period with `dirty_decay_ms`'s range, `-1`
/// never and `0` at once, and it round-trips through `set`/`get`.
#[test]
fn huge_dirty_decay_ms_follows_dirty_decay_ms_until_set() {
    let d = Config::DEFAULT;
    assert_eq!(d.huge_dirty_decay_ms, None);
    assert_eq!(d.huge_decay_ms(), d.dirty_decay_ms);
    assert_eq!(d.get(b"huge_dirty_decay_ms"), Some(Value::Int(10_000)));
    let cfg = clean("dirty_decay_ms:3000");
    assert_eq!(
        cfg.huge_dirty_decay_ms, None,
        "the slab key leaves it following"
    );
    assert_eq!(cfg.huge_decay_ms(), 3000);
    assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(3000)));
    let cfg = clean("huge_dirty_decay_ms:2000,dirty_decay_ms:3000");
    assert_eq!(cfg.huge_dirty_decay_ms, Some(2000));
    assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (2000, 3000));
    assert_eq!(cfg.get(b"huge_dirty_decay_ms"), Some(Value::Int(2000)));
    assert_eq!(
        cfg.get(b"dirty_decay_ms"),
        Some(Value::Int(3000)),
        "the huge key leaves the slab key alone"
    );
    assert_eq!(clean("huge_dirty_decay_ms:0").huge_dirty_decay_ms, Some(0));
    assert_eq!(
        clean("huge_dirty_decay_ms:-1").huge_dirty_decay_ms,
        Some(-1)
    );
    assert_eq!(
        clean("huge_dirty_decay_ms:-1").get(b"huge_dirty_decay_ms"),
        Some(Value::Int(-1))
    );
    // Set to the slab period explicitly it stays pinned there when
    // the slab period later differs.
    let cfg = clean("huge_dirty_decay_ms:10000,dirty_decay_ms:500");
    assert_eq!((cfg.huge_decay_ms(), cfg.dirty_decay_ms), (10_000, 500));
    let (cfg, issues) = parse("huge_dirty_decay_ms:-2");
    assert_eq!(
        cfg.huge_dirty_decay_ms,
        Some(-1),
        "clamped like dirty_decay_ms"
    );
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("out-of-range conf value \"-2\" for huge_dirty_decay_ms"),
        "{}",
        issues[0]
    );
}

#[test]
fn huge_retain_max_is_a_byte_count() {
    assert_eq!(clean("huge_retain_max:0").huge_retain_max, 0);
    assert_eq!(clean("huge_retain_max:0x100000").huge_retain_max, 1 << 20);
    assert_eq!(
        clean("huge_retain_max:1099511627776").huge_retain_max,
        MAX_HUGE_RETAIN_MAX
    );
    assert_eq!(
        clean("huge_retain_max:1").get(b"huge_retain_max"),
        Some(Value::Size(1))
    );
    let (cfg, issues) = parse("huge_retain_max:1099511627777");
    assert_eq!(
        cfg.huge_retain_max, MAX_HUGE_RETAIN_MAX,
        "clamped to the huge region"
    );
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("out-of-range conf value \"1099511627777\" for huge_retain_max"),
        "{}",
        issues[0]
    );
}

#[test]
fn compatibility_keys_are_accepted_with_any_value() {
    for key in IGNORED_KEYS {
        for value in ["true", "false", "17", "alloc", "whatever"] {
            let cfg = clean(&std::format!("{key}:{value}"));
            assert_eq!(cfg, Config::DEFAULT, "{key}:{value} changed the config");
            assert_eq!(cfg.get(key.as_bytes()), None);
        }
    }
}

#[test]
fn unknown_keys_are_reported_and_skipped() {
    let (cfg, issues) = parse("bogus:1,tcache:false,another_bogus:true");
    assert!(!cfg.tcache);
    assert_eq!(issues.len(), 2);
    assert!(
        issues[0].contains("unknown conf key \"bogus\""),
        "{}",
        issues[0]
    );
    assert!(
        issues[1].contains("unknown conf key \"another_bogus\""),
        "{}",
        issues[1]
    );
    // Keys are case sensitive and not trimmed, as in jemalloc.
    for text in [
        "Tcache:false",
        " tcache:false",
        "tcache :false",
        "tcache: false",
    ] {
        let (cfg, issues) = parse(text);
        assert_eq!(cfg, Config::DEFAULT, "{text:?}");
        assert_eq!(issues.len(), 1, "{text:?}");
    }
}

#[test]
fn malformed_pairs_are_reported_and_the_rest_still_applies() {
    let (cfg, issues) = parse("nocolon,tcache:false,:novalue,nokey:,narenas:3");
    assert!(!cfg.tcache);
    assert_eq!(cfg.narenas, 3);
    assert_eq!(issues.len(), 3);
    assert!(
        issues[0].contains("malformed conf pair \"nocolon\""),
        "{}",
        issues[0]
    );
    assert!(
        issues[1].contains("malformed conf pair \":novalue\""),
        "{}",
        issues[1]
    );
    assert!(
        issues[2].contains("malformed conf pair \"nokey:\""),
        "{}",
        issues[2]
    );
    // A lone colon and a value with a second colon.
    let (_, issues) = parse(":");
    assert_eq!(issues.len(), 1);
    let (cfg, issues) = parse("narenas:1:2");
    assert_eq!(
        cfg.narenas,
        Config::DEFAULT.narenas,
        "an invalid value leaves the default"
    );
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("invalid conf value \"1:2\""),
        "{}",
        issues[0]
    );
}

#[test]
fn quoted_values_and_the_last_pair_win() {
    assert_eq!(clean("narenas:\"7\"").narenas, 7);
    assert_eq!(clean("thp:\"always\"").thp, Thp::Always);
    // A quoted value may contain a comma.
    let (cfg, issues) = parse("thp:\"al,ways\",narenas:2");
    assert_eq!(cfg.narenas, 2);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("\"al,ways\""), "{}", issues[0]);
    // An unterminated quote is malformed.
    let (_, issues) = parse("narenas:\"3");
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("malformed conf pair"), "{}", issues[0]);
    // Later pairs override earlier ones; a trailing comma is fine.
    let cfg = clean("narenas:2,narenas:5,tcache:false,tcache:true,");
    assert_eq!(cfg.narenas, 5);
    assert!(cfg.tcache);
}

/// An unterminated quote ends its pair at the next comma and is reported
/// as malformed, even for a key whose value is never validated, so the
/// pairs after it still apply and `abort_conf` sees the problem. A quote
/// opens only at the start of the value, never after a later colon.
#[test]
fn an_unterminated_quote_is_reported_and_ends_at_the_next_comma() {
    let (cfg, issues) = parse("junk:\"x,narenas:1,abort_conf:true");
    assert_eq!(cfg.narenas, 1);
    assert!(cfg.abort_conf);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
        issues[0].contains("malformed conf pair \"junk:\"x\""),
        "{}",
        issues[0]
    );
    // A value opened with a quote must end with one.
    let (_, issues) = parse("junk:\"x\"y,zero:\"");
    assert_eq!(issues.len(), 2, "{issues:?}");
    assert!(issues.iter().all(|i| i.contains("malformed")), "{issues:?}");
    // Only the value is quoted: a quote after a later colon does not
    // swallow the pairs after it.
    let cfg = clean("junk:a:\"x,narenas:1,tcache:\"false\"");
    assert_eq!(cfg.narenas, 1);
    assert!(!cfg.tcache);
    // A later quoted value cannot close an earlier unterminated quote
    // unnoticed: the swallowed pair holds an inner quote, which a
    // quoted value never contains, so it is reported.
    let (_, issues) = parse("junk:\"x,narenas:1,abort_conf:true,thp:\"always\"");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].contains("malformed"), "{}", issues[0]);
}

#[test]
fn redis_and_valkey_conf_strings_parse_clean() {
    let cfg = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
    assert_eq!(cfg.lg_tcache_nslots_mul, 3);
    assert_eq!(cfg.tcache_nslots_small_max, 1000);
    let cfg =
        clean("background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0,narenas:2,abort_conf:true");
    assert!(cfg.background_thread == Some(true) && cfg.abort_conf);
    assert_eq!(cfg.dirty_decay_ms, 0);
    assert_eq!(cfg.narenas, 2);
    // jemalloc's own `percpu_arena` values parse clean under `abort_conf`.
    let cfg = clean("percpu_arena:disabled,abort_conf:true");
    assert_eq!(cfg.percpu_arena, PercpuArena::Disabled);
    assert_eq!(
        clean("percpu_arena:phycpu,narenas:4").percpu_arena,
        PercpuArena::Phycpu
    );
}

#[test]
fn get_covers_every_key_and_only_those() {
    let cfg = Config::DEFAULT;
    for key in KEYS {
        assert!(cfg.get(key.as_bytes()).is_some(), "{key} unreadable");
        // Every readable key also parses (with a value of its type).
        let value = match cfg.get(key.as_bytes()).unwrap() {
            Value::Bool(b) => std::format!("{b}"),
            Value::Int(i) => std::format!("{i}"),
            Value::Uint(u) => std::format!("{u}"),
            Value::Size(n) => std::format!("{n}"),
            Value::Str(s) => s.cstr().to_str().unwrap().into(),
        };
        let text = std::format!("{key}:{value}");
        let parsed = clean(&text);
        if *key == "huge_dirty_decay_ms" {
            // Its default is "follow `dirty_decay_ms`" (`None`), which
            // reads back as that period; setting the reading pins it.
            assert_eq!(
                parsed.get(key.as_bytes()),
                cfg.get(key.as_bytes()),
                "{text}"
            );
            assert_eq!(
                parsed.huge_dirty_decay_ms,
                Some(cfg.dirty_decay_ms),
                "{text}"
            );
            assert_eq!(
                Config {
                    huge_dirty_decay_ms: None,
                    ..parsed
                },
                cfg,
                "{text} changed another field"
            );
        } else if *key == "background_thread" {
            // Its default is the lazy start (`None`), which reads back
            // `false`; setting the reading pins the opt-out.
            assert_eq!(
                parsed.get(key.as_bytes()),
                cfg.get(key.as_bytes()),
                "{text}"
            );
            assert_eq!(
                Config {
                    background_thread: cfg.background_thread,
                    ..parsed
                },
                cfg,
                "{text} changed another field"
            );
        } else {
            assert_eq!(parsed, cfg, "{key}:{value} round trip");
        }
    }
    assert_eq!(cfg.get(b"nonsense"), None);
    assert_eq!(cfg.get(b""), None);
    for key in IGNORED_KEYS {
        assert_eq!(cfg.get(key.as_bytes()), None, "{key}");
    }
}

/// The three tables and `set` agree: every name in `KEYS`,
/// `IGNORED_KEYS` and `INERT_KEYS` is one `set` knows (a bad value is
/// refused as a value, never as an unknown key; an ignored key takes
/// any value; an inert key is reported as inert), the tables are
/// disjoint, and a name in none of them is [`Issue::UnknownKey`], so a
/// key added to one table that another also names (or a typo in a
/// test) fails here rather than in `opt.<key>` or `malloc_stats_print`.
#[test]
fn set_knows_exactly_the_tabled_keys() {
    for key in KEYS.iter().chain(IGNORED_KEYS).chain(INERT_KEYS) {
        let mut cfg = Config::DEFAULT;
        let r = cfg.set(key.as_bytes(), b"nonsense-value");
        assert!(!matches!(r, Err(Issue::UnknownKey(_))), "{key}: {r:?}");
        if IGNORED_KEYS.contains(key) {
            assert_eq!(r, Ok(()), "{key} takes any value");
        } else if INERT_KEYS.contains(key) {
            assert_eq!(
                r,
                Err(Issue::BadValue {
                    key: key.as_bytes(),
                    value: b"nonsense-value"
                }),
                "{key}"
            );
            assert_eq!(
                cfg.set(key.as_bytes(), b"true"),
                Err(Issue::Inert(key.as_bytes())),
                "{key}"
            );
        } else {
            assert_eq!(
                r,
                Err(Issue::BadValue {
                    key: key.as_bytes(),
                    value: b"nonsense-value"
                }),
                "{key}"
            );
            assert!(cfg.get(key.as_bytes()).is_some(), "{key} is readable");
        }
        let n = [
            KEYS.contains(key),
            IGNORED_KEYS.contains(key),
            INERT_KEYS.contains(key),
        ];
        assert_eq!(
            n.iter().filter(|&&b| b).count(),
            1,
            "{key} is in exactly one table"
        );
    }
    for unknown in [
        "nonsense",
        "tcache_maxx",
        "Tcache",
        "stats_prin",
        "thp_proces",
        "",
    ] {
        let mut cfg = Config::DEFAULT;
        assert_eq!(
            cfg.set(unknown.as_bytes(), b"true"),
            Err(Issue::UnknownKey(unknown.as_bytes())),
            "{unknown:?}"
        );
        assert_eq!(cfg, Config::DEFAULT);
    }
}

/// `KEYS` is `TABLE`'s names in `TABLE`'s order (what `mallctl`
/// indexes the `opt.` MIB by), every name is unique, and every entry's
/// reader and parser are the field's: reading an entry, setting a
/// different value of its type through `set` and reading it back
/// changes that entry and no other. Replaces a hand-kept count: a new
/// key is one `TABLE` entry, and it is checked here whatever its name.
#[test]
fn the_table_is_the_one_spelling_of_every_key() {
    assert_eq!(KEYS.len(), TABLE.len());
    for (i, key) in TABLE.iter().enumerate() {
        assert_eq!(KEYS[i], key.name);
        assert_eq!(
            KEYS.iter().filter(|k| **k == key.name).count(),
            1,
            "{} is named once",
            key.name
        );
        assert!(
            !IGNORED_KEYS.contains(&key.name) && !INERT_KEYS.contains(&key.name),
            "{}",
            key.name
        );
        // A value of the key's type that differs from the default.
        let d = Config::DEFAULT;
        let other = match (key.read.0)(&d) {
            Value::Bool(b) => std::format!("{}", !b),
            Value::Int(i) => std::format!("{}", if i == 0 { 1 } else { 0 }),
            // Doubling stays in every unsigned key's range and keeps
            // `guard_slab_interval` a power of two.
            Value::Uint(u) => std::format!("{}", if u == 0 { 2 } else { u * 2 }),
            Value::Size(n) => std::format!("{}", if n == 0 { 2 } else { n * 2 }),
            Value::Str(s) => match key.name {
                "percpu_arena" => "percpu".into(),
                "thp" => "always".into(),
                _ => panic!("{}: a string key with no test value ({s:?})", key.name),
            },
        };
        let mut cfg = d;
        assert_eq!(
            cfg.set(key.name.as_bytes(), other.as_bytes()),
            Ok(()),
            "{}:{other}",
            key.name
        );
        assert_ne!(
            (key.read.0)(&cfg),
            (key.read.0)(&d),
            "{}:{other} read back through its own entry",
            key.name
        );
        assert_eq!(
            cfg.get(key.name.as_bytes()),
            Some((key.read.0)(&cfg)),
            "{}",
            key.name
        );
        for o in TABLE.iter().filter(|o| o.name != key.name) {
            if key.name == "dirty_decay_ms" && o.name == "huge_dirty_decay_ms" {
                // Unset, the huge period reads as the slab period
                // (design 7): the one reading that moves with another key.
                assert_eq!(
                    (o.read.0)(&cfg),
                    (key.read.0)(&cfg),
                    "huge_dirty_decay_ms follows dirty_decay_ms"
                );
                continue;
            }
            assert_eq!(
                (o.read.0)(&cfg),
                (o.read.0)(&d),
                "{}:{other} changed {}",
                key.name,
                o.name
            );
        }
    }
    // The aliases of the named-value keys: one entry, several spellings.
    assert_eq!(clean("thp:default").thp, clean("thp:never").thp);
}

#[test]
fn cache_caps_follow_the_ladder_and_the_overrides() {
    let d = Config::DEFAULT;
    // Classes above tcache_max (32 KiB, jemalloc's default) are not cached.
    fn cached(cfg: &Config) -> impl Iterator<Item = usize> {
        let max = cfg.tcache_max;
        (0..NCLASSES).filter(move |&c| CLASSES[c].size() <= max)
    }
    fn uncached(cfg: &Config) -> impl Iterator<Item = usize> {
        let max = cfg.tcache_max;
        (0..NCLASSES).filter(move |&c| CLASSES[c].size() > max)
    }
    assert!(
        cached(&d).count() >= 36 && uncached(&d).count() >= 8,
        "{} cached, {} above",
        cached(&d).count(),
        uncached(&d).count()
    );
    for c in cached(&d) {
        assert_eq!(d.cap_for(0, 0, c), ladder(c, MAX_CAP, 1));
        assert_eq!(d.cap_for(0, 0, c), crate::heap::default_cap(c), "class {c}");
    }
    for c in uncached(&d) {
        assert_eq!(
            d.cap_for(0, 0, c),
            0,
            "class {c} ({} bytes) is above tcache_max",
            CLASSES[c].size()
        );
        assert!(
            crate::heap::default_cap(c) >= MIN_CAP,
            "the ladder itself never reaches zero"
        );
    }
    // Redis: x4 ladder clamped at 1000, never below MIN_CAP.
    let redis = clean("lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000");
    for c in cached(&redis) {
        let size = CLASSES[c].size();
        let cap = redis.cap_for(0, 0, c);
        assert!(
            (MIN_CAP..=1000).contains(&cap),
            "class {c} ({size} bytes): {cap}"
        );
        if size <= 512 {
            assert_eq!(cap, 1000, "class {c} ({size} bytes)");
        }
        assert!(
            cap >= d.cap_for(0, 0, c).min(1000),
            "class {c} never shrinks below the default ladder"
        );
    }
    // A small max caps every class; lg_mul below 1 shrinks the ladder.
    let small = clean("tcache_nslots_small_max:16");
    assert!(
        cached(&small).all(|c| small.cap_for(0, 0, c) <= 16 && small.cap_for(0, 0, c) >= MIN_CAP)
    );
    let shrunk = clean("lg_tcache_nslots_mul:0");
    assert_eq!(shrunk.cap_for(0, 0, 0), 512);
    assert_eq!(shrunk.cap_for(0, 0, NCLASSES - 1), 0);
    assert_eq!(
        shrunk.cap_for(0, 0, cached(&shrunk).last().unwrap()),
        MIN_CAP
    );
    let grown = clean("lg_tcache_nslots_mul:16,tcache_nslots_small_max:65536");
    assert!(cached(&grown).all(|c| grown.cap_for(0, 0, c) == 65536));
    // tcache_max moves the boundary: every slab class, or none.
    let all = clean("tcache_max:262144");
    assert!((0..NCLASSES).all(|c| all.cap_for(0, 0, c) >= MIN_CAP));
    assert_eq!(all.cap_for(0, 0, NCLASSES - 1), MIN_CAP);
    let none = clean("tcache_max:0");
    assert!((0..NCLASSES).all(|c| none.cap_for(0, 0, c) == 0));
    let k4 = clean("tcache_max:4096");
    assert!((0..NCLASSES).all(|c| (k4.cap_for(0, 0, c) == 0) == (CLASSES[c].size() > 4096)));
    // Out of range clamps and reports, like every numeric key.
    assert_eq!(clean("tcache_max:32768").tcache_max, 32768);
    // tcache:false turns caching off entirely.
    let off = clean("tcache:false");
    assert!((0..NCLASSES).all(|c| off.cap_for(0, 0, c) == 0));
    // Extreme shifts do not overflow.
    assert_eq!(ladder(0, MAX_NSLOTS, -16), MIN_CAP);
    assert_eq!(ladder(NCLASSES - 1, MAX_NSLOTS, 16), MAX_NSLOTS);
}

/// `cap_for` is the one rule for which classes are cached where: the
/// home ladder, nothing for a shared class (above `tcache_max`) or
/// with `tcache:false`, and nothing above `BIN_MAX` for another arena.
#[test]
fn cap_for_is_the_one_rule() {
    let d = Config::DEFAULT;
    for c in 0..NCLASSES {
        let size = CLASSES[c].size();
        assert_eq!(d.shared_class(c), size > d.tcache_max, "class {c}");
        assert_eq!(d.cap_for(3, 3, c), d.cap_for(0, 0, c), "class {c}");
        assert_eq!(d.cap_for(3, 3, c) == 0, d.shared_class(c), "class {c}");
        let foreign = d.cap_for(3, 0, c);
        assert_eq!(
            foreign,
            if c >= NBINS { 0 } else { d.cap_for(0, 0, c) },
            "class {c}"
        );
        assert_eq!(d.cap_for(0, 3, c), foreign);
    }
    let none = clean("tcache:false");
    assert!((0..NCLASSES).all(|c| none.cap_for(0, 0, c) == 0 && none.cap_for(0, 1, c) == 0));
    let k4 = clean("tcache_max:4096");
    assert!((0..NCLASSES).all(|c| k4.shared_class(c) == (CLASSES[c].size() > 4096)));
}

#[test]
fn guards_off_zeroes_the_interval() {
    assert_eq!(clean("guards:false").effective_guard_slab_interval(), 0);
    assert_eq!(
        clean("guard_slab_interval:0").effective_guard_slab_interval(),
        0
    );
    assert_eq!(
        clean("guard_slab_interval:2").effective_guard_slab_interval(),
        2
    );
    // 1 would leave no slabs to allocate: out of range, clamped to 2.
    let (cfg, issues) = parse("guard_slab_interval:1");
    assert_eq!(cfg.effective_guard_slab_interval(), 2);
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("out-of-range conf value \"1\" for guard_slab_interval"),
        "{}",
        issues[0]
    );
    // The interval is a power of two (the sized path's guard test is a
    // mask): anything else rounds up, is reported, and the rounded value
    // is what `opt.guard_slab_interval` returns; the maximum is one.
    for (text, want) in [("3", 4u32), ("5", 8), ("12", 16), ("4194303", 1 << 22)] {
        let (cfg, issues) = parse(&std::format!("guard_slab_interval:{text}"));
        assert_eq!(cfg.effective_guard_slab_interval(), want, "{text}");
        assert_eq!(
            cfg.get(b"guard_slab_interval"),
            Some(Value::Uint(want as u64))
        );
        assert_eq!(issues.len(), 1, "{text}");
        assert_eq!(
            issues[0],
            std::format!(
                "conf value \"{text}\" for guard_slab_interval is not a power of two (rounded up to {want})"
            )
        );
    }
    for text in ["0", "2", "4", "8", "64", "4194304"] {
        let (cfg, issues) = parse(&std::format!("guard_slab_interval:{text}"));
        assert_eq!(cfg.guard_slab_interval, text.parse::<u32>().unwrap());
        assert!(issues.is_empty(), "{text}: {issues:?}");
    }
    // Under `abort_conf` a rounded value counts as a problem, as the
    // clamp does (`load` aborts on any issue).
    let (cfg, issues) = parse("guard_slab_interval:6,abort_conf:true");
    assert!(cfg.abort_conf && cfg.guard_slab_interval == 8 && issues.len() == 1);
}

/// What `parse_source` keeps of a cut source: every pair before the
/// one that runs to the cut, a quoted value's commas included.
#[test]
fn parse_source_drops_only_the_pair_at_the_cut() {
    let cases: &[(&str, bool, u32, usize)] = &[
        // (text, cut, narenas, issues)
        ("narenas:7", false, 7, 0),
        ("narenas:7", true, Config::DEFAULT.narenas, 1),
        ("narenas:7,tcache:fal", true, 7, 1),
        // The cut fell right after a comma: no pair is split.
        ("tcache:false,narenas:7,", true, 7, 1),
        // An open quote ends its pair at the next comma: that pair is
        // reported as malformed, and the cut drops the pair after it.
        ("narenas:7,junk:\"a,b", true, 7, 2),
        ("narenas:7,no_such_key:1,narenas:8", true, 7, 2),
    ];
    for &(text, cut, narenas, count) in cases {
        let mut cfg = Config::DEFAULT;
        let mut issues = Vec::new();
        let n = cfg.parse_source(text.as_bytes(), cut, |i: Issue<'_>| {
            issues.push(std::format!("{i}"))
        });
        assert_eq!(n, issues.len(), "{text}");
        assert_eq!(cfg.narenas, narenas, "{text}: {issues:?}");
        assert_eq!(issues.len(), count, "{text}: {issues:?}");
        if cut {
            assert!(issues.last().unwrap().contains("longer than"), "{text}");
        }
    }
}

#[test]
fn issue_display_is_printable_and_bounded() {
    let long: Vec<u8> = (0..200u8).collect();
    let text = std::format!("{}", Issue::UnknownKey(&long));
    assert!(text.contains("..."));
    assert!(text.len() < 120, "{text}");
    assert!(
        !text.bytes().any(|b| !(0x20..0x7f).contains(&b)),
        "{text:?}"
    );
    assert_eq!(
        std::format!("{}", Issue::Malformed(b"a\tb")),
        "malformed conf pair \"a?b\""
    );
}
