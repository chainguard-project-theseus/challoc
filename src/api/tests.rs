use super::*;
use std::string::String;
use std::thread;
use std::vec::Vec;

#[test]
fn init_is_idempotent_and_concurrent() {
    let handles: Vec<_> = (0..16)
        .map(|_| thread::spawn(|| shared() as *const Shared as usize))
        .collect();
    let first = shared() as *const Shared as usize;
    for h in handles {
        assert_eq!(h.join().unwrap(), first);
    }
    assert!(is_ready());
    assert!(shared_if_ready().is_some());
    assert!(shared().has_thread_key());
    let f = features();
    assert_eq!(f.page_size, shared().regions.page_size());
}

/// `challoc_features()` is the `Display` of a full probe,
/// NUL-terminated, static, and the same bytes on every call: under
/// `thp:never` (the default, so the tests' configuration), where init
/// skips the per-size THP files, the line built at the first request
/// still lists every enabled mTHP size, as a fresh full probe does.
#[test]
fn challoc_features_is_the_probe_line() {
    let p = challoc_features();
    assert!(!p.is_null());
    assert_eq!(p, challoc_features());
    // SAFETY: `challoc_features` returns a static NUL-terminated line.
    let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap();
    let full = crate::sys::features::Features::probe(true);
    assert_eq!(s, std::format!("{full}"), "the line is the full probe's");
    assert_eq!(s, std::format!("{}", features()));
    assert_eq!(s, feature_line());
    assert!(s.starts_with("page_size="), "{s}");
    assert!(s.ends_with(" pac=yes") || s.ends_with(" pac=no"), "{s}");
    assert!(!s.contains('\n'));
    for kv in s.split(' ') {
        let (k, v) = kv
            .split_once('=')
            .unwrap_or_else(|| panic!("`{kv}` in {s}"));
        assert!(!k.is_empty() && !v.is_empty(), "`{kv}` in {s}");
    }
}

/// A `fork` discovers a deferred cgroup in its prepare handler, so the
/// child inherits the record (it reads no files of its own) and the
/// parent has it too.
#[test]
fn a_forked_child_inherits_the_discovered_cgroup() {
    // Init registers the fork handlers.
    let _ = shared();
    // SAFETY: the test binary is multithreaded; the child uses only challoc's
    // state, which its fork handlers keep consistent, and leaves through
    // `_exit`.
    let pid = unsafe { ::libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let code = if shared().cgroup().is_some() { 0 } else { 1 };
        // SAFETY: `_exit` has no preconditions; the child must not unwind.
        unsafe { ::libc::_exit(code) };
    }
    let mut status = 0;
    // SAFETY: `pid` is this process's child; `status` is a local.
    assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(::libc::WIFEXITED(status), "child status {status:#x}");
    assert_eq!(::libc::WEXITSTATUS(status), 0, "the child has no record");
    assert!(shared().cgroup().is_some(), "nor the parent");
}

/// A feature line another thread was building when the process forked
/// is built again by the child, not waited for: the child (one thread
/// here, so the state can be set without racing other tests) marks the
/// line as being built, forks, and its own child must get the line
/// within the alarm.
#[test]
fn a_forked_child_does_not_wait_for_a_line_being_built() {
    let expect = feature_line().len();
    // SAFETY: as in `a_forked_child_inherits_the_discovered_cgroup`: each
    // child uses only challoc's state and leaves through `_exit`.
    let pid = unsafe { ::libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        LINE_STATE.store(LINE_BUILDING, Ordering::Release);
        // SAFETY: as above.
        let gc = unsafe { ::libc::fork() };
        if gc == 0 {
            // SAFETY: `alarm` has no preconditions.
            unsafe { ::libc::alarm(10) };
            let p = challoc_features();
            // SAFETY: `challoc_features` returns a static NUL-terminated line.
            let n = unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes().len();
            // SAFETY: `_exit` has no preconditions.
            unsafe { ::libc::_exit(if n == expect { 0 } else { 2 }) };
        }
        let mut status = 0;
        let code = if gc > 0
            // SAFETY: `gc` is this process's child; `status` is a local.
            && unsafe { ::libc::waitpid(gc, &mut status, 0) } == gc
            && ::libc::WIFEXITED(status)
        {
            ::libc::WEXITSTATUS(status)
        } else {
            3
        };
        // SAFETY: `_exit` has no preconditions.
        unsafe { ::libc::_exit(code) };
    }
    let mut status = 0;
    // SAFETY: `pid` is this process's child; `status` is a local.
    assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(::libc::WIFEXITED(status), "child status {status:#x}");
    assert_eq!(
        ::libc::WEXITSTATUS(status),
        0,
        "the grandchild spun on the line (3: killed by the alarm) or got another one (2)"
    );
}

/// Every `.rs` file under `src/` but the modules declared under
/// `#[cfg(test)]`, as (path relative to `src/`, text), in path order.
fn lock_sources() -> Vec<(String, String)> {
    use std::string::ToString;
    /// Every `.rs` file under `dir` as (path relative to `src/`, text).
    fn sources(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let rel = if rel.is_empty() {
                name
            } else {
                std::format!("{rel}/{name}")
            };
            if path.is_dir() {
                sources(&path, &rel, out);
            } else if rel.ends_with(".rs") {
                out.push((rel, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    /// The module paths (relative to `src/`, without `.rs`) that `rel`
    /// declares as `#[cfg(test)] mod name;`.
    fn test_modules(rel: &str, text: &str, out: &mut Vec<String>) {
        let stem = rel.strip_suffix(".rs").unwrap();
        let dir = match stem.rsplit_once('/') {
            Some((d, "mod")) => d.to_string(),
            None if stem == "lib" || stem == "mod" => String::new(),
            _ => stem.to_string(),
        };
        let mut cfg_test = false;
        for line in text.lines() {
            let t = line.trim();
            let decl = t
                .strip_prefix("pub(crate) ")
                .or_else(|| t.strip_prefix("pub "))
                .unwrap_or(t);
            if let (true, Some(name)) = (
                cfg_test,
                decl.strip_prefix("mod ").and_then(|m| m.strip_suffix(';')),
            ) {
                out.push(if dir.is_empty() {
                    name.to_string()
                } else {
                    std::format!("{dir}/{name}")
                });
            }
            cfg_test = t == "#[cfg(test)]";
        }
    }
    let mut files = Vec::new();
    sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        "",
        &mut files,
    );
    let mut test_only = Vec::new();
    for (rel, text) in &files {
        test_modules(rel, text, &mut test_only);
    }
    files.retain(|(rel, _)| {
        let stem = rel.strip_suffix(".rs").unwrap();
        !test_only
            .iter()
            .any(|m| stem == m || rel.starts_with(&std::format!("{m}/")))
    });
    files
}

/// Every `Lock<..>` (or bare `RawLock`) field and static in `src/`, as
/// `file:Owner.field` (`file:static.NAME` for statics), read from the
/// source tree so the fork handlers cannot miss a lock added later.
/// `lock.rs` defines the types and is skipped; so are the modules
/// declared under `#[cfg(test)]` and each file's `mod tests`.
fn lock_inventory() -> Vec<String> {
    lock_decls().into_iter().map(|(key, _)| key).collect()
}

/// [`lock_inventory`], each lock with whether it is private to its
/// module (declared without `pub`).
fn lock_decls() -> Vec<(String, bool)> {
    lock_decls_typed()
        .into_iter()
        .map(|(key, private, _)| (key, private))
        .collect()
}

/// [`lock_decls`], each lock also with the type it protects as written
/// in its declaration (`T` of `Lock<T>`, inside any wrapper such as
/// `Padded<..>`), empty for a bare `RawLock`.
fn lock_decls_typed() -> Vec<(String, bool, String)> {
    use std::string::ToString;
    // `T` of the first `Lock<T>` in `ty`, up to its matching `>`.
    let elem = |ty: &str| -> String {
        let Some(at) = ty.find("Lock<") else {
            return String::new();
        };
        let rest = &ty[at + "Lock<".len()..];
        let mut depth = 1;
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                return rest[..i].trim().to_string();
            }
        }
        String::new()
    };
    let mut out = Vec::new();
    for (rel, text) in &lock_sources() {
        if rel == "lock.rs" {
            continue;
        }
        let mut owner = String::new();
        for line in text.lines() {
            let t = line.trim();
            if t == "mod tests {" {
                break;
            }
            if t.starts_with("//")
                || t.starts_with("use ")
                || t.contains(" fn ")
                || t.starts_with("fn ")
            {
                continue;
            }
            let decl = t
                .strip_prefix("pub(crate) ")
                .or_else(|| t.strip_prefix("pub "))
                .unwrap_or(t);
            let private = decl.len() == t.len();
            if let Some(rest) = decl.strip_prefix("struct ") {
                owner = rest
                    .split(['<', '{', '(', ' ', ';'])
                    .next()
                    .unwrap()
                    .to_string();
                continue;
            }
            let is_lock = |ty: &str| ty.contains("Lock<") || ty.contains("RawLock");
            // `exec static` is how `verus!` spells a static.
            if let Some(rest) = decl
                .strip_prefix("static ")
                .or_else(|| decl.strip_prefix("exec static "))
            {
                if let Some((name, ty)) = rest.split_once(':')
                    && is_lock(ty)
                {
                    out.push((
                        std::format!("{rel}:static.{}", name.trim()),
                        private,
                        elem(ty),
                    ));
                }
                continue;
            }
            if let Some((name, ty)) = decl.split_once(':') {
                let name = name.trim();
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && is_lock(ty)
                    && !ty.contains("->")
                {
                    out.push((std::format!("{rel}:{owner}.{name}"), private, elem(ty)));
                }
            }
        }
    }
    out.sort();
    out
}

/// The locks whose holders outside the tests end in an explicit
/// [`LockGuard::unlock`](crate::lock::LockGuard::unlock), keyed as in
/// [`lock_inventory`]: the unlock's `requires` checks the lock's
/// invariant and frame, where a guard that drops releases with nothing
/// checked. A lock joins the list, with its holders outside the tests
/// (`file:fn`), once every holder unlocks.
const UNLOCKED_LOCKS: &[(&str, &[&str])] = &[
    (
        "arena.rs:Arena.stats",
        &[
            "arena.rs:stats_into",
            "arena.rs:add_stats_to",
            "arena.rs:store_stats",
        ],
    ),
    (
        "arena.rs:Arenas.lock",
        &["arena.rs:assign", "arena.rs:create", "arena.rs:materialise"],
    ),
    (
        "class/mod.rs:ClassState.inner",
        &[
            "class/mod.rs:plan",
            "class/mod.rs:reopen",
            "class/mod.rs:on_live_zero",
            "class/mod.rs:bin_stats",
            "class/mod.rs:lowest_partial",
            "class/mod.rs:is_partial",
            "class/decay.rs:empty_bytes",
            "class/decay.rs:purge_empty",
            "class/decay.rs:purge_curve",
        ],
    ),
    ("class/sets.rs:SetTable.tables", &["class/sets.rs:ensure"]),
    (
        "class/table.rs:SlabTable.inner",
        &[
            "class/table.rs:mirror_watermark",
            "class/table.rs:guards",
            "class/table.rs:take_group",
            "class/table.rs:commit_run",
            "class/table.rs:commit_slab",
        ],
    ),
    (
        "heap/shared.rs:Shared.stats",
        &[
            "stats.rs:collect_into",
            "stats.rs:snapshot",
            "stats.rs:arena_stats_all_into",
        ],
    ),
    (
        "heap/tcaches.rs:Tcaches.lock",
        &["heap/tcaches.rs:create", "heap/tcaches.rs:destroy"],
    ),
    (
        "huge/mod.rs:Huge.inner",
        &[
            "huge/mod.rs:alloc_aligned",
            "huge/free.rs:free_run",
            "huge/free.rs:usable_size",
            "huge/free.rs:lookup",
            "huge/decay.rs:purge_empty",
            "huge/decay.rs:purge_curve",
            "huge/decay.rs:empty_bytes",
            "huge/decay.rs:stats",
        ],
    ),
    (
        "meta.rs:Meta.inner",
        &[
            "meta.rs:alloc",
            "meta.rs:alloc_raw",
            "meta.rs:alloc_zeroed",
            "meta.rs:free",
            "meta.rs:reserve",
            "meta.rs:reserve_table",
            "meta.rs:commit",
            "meta.rs:reserve_committed_raw",
            "meta.rs:stats",
        ],
    ),
    (
        "purge/background.rs:static.STATE",
        &[
            "purge/background.rs:request",
            "purge/background.rs:start",
            "purge/background.rs:present_or_creating",
            "purge/background.rs:start_requested",
            "purge/background.rs:auto_tick",
            "purge/background.rs:main",
        ],
    ),
];

/// Every guard of a lock in [`UNLOCKED_LOCKS`], outside the tests and
/// `#[cfg(test)]` items, ends in an explicit `unlock()`. An acquisition
/// (`x.field.lock(`, `.try_lock(` or `.lock_t(`, a method chain read as
/// one line) is matched to its lock by the field's name, by the files
/// that can name the field (a private field: its module and the modules
/// under it), and for `self.field` by the type of the enclosing `impl`.
/// A static may also be acquired through its accessor (`state().lock(`):
/// a private function of the static's file whose body ends in
/// `&NAME`. Each acquisition is bound by a `let` that ends with it (a
/// guard chained into a call drops at the end of the statement), and
/// the guard's scope runs to the end of the block the `let` stands in.
/// In that scope every `return` follows the guard's `unlock();`, one
/// more `unlock();` stands at the `let`'s own indentation (so on the
/// path to the block's end), no `?` appears, and every `break` or
/// `continue` stands inside a loop opened in that scope (with its
/// label, if it names one), so none is an early exit. A function may
/// hold such guards in blocks one after
/// another, never two at once.
/// The functions so checked are exactly the holders the list names. Nor
/// does the owner hand out a reference to such a lock: no `self.field`
/// of one is borrowed, nor a static of one outside its accessor, whose
/// result is used at once (`.lock(`, `.value_held_mut(`, ..). Not seen: an
/// acquisition through a reference taken elsewhere, a path-qualified
/// or raw borrow (`&self::NAME`, `&raw const NAME`), a path-qualified
/// accessor call (`self::state().lock(`), a static used directly
/// (`NAME.raw()`; no raw-token path reaches the value), UFCS
/// (`Lock::lock(&x, ..)`), and macros. A panic aborts outside the tests
/// (`panic = "abort"`), so it never releases.
#[test]
fn guards_end_in_unlock() {
    use std::string::ToString;
    let decls = lock_decls();
    for (key, _) in UNLOCKED_LOCKS {
        assert!(
            decls.iter().any(|(k, _)| k == key),
            "{key} is not a lock in src/"
        );
    }
    // The module a file is, as a path relative to `src/`.
    let module = |rel: &str| {
        let stem = rel.strip_suffix(".rs").unwrap();
        let stem = stem.strip_suffix("/mod").unwrap_or(stem);
        String::from(if stem == "lib" { "" } else { stem })
    };
    // Each lock as (key, its module, owner, field, private); a static's
    // owner is `static`.
    let locks: Vec<(String, String, String, String, bool)> = decls
        .iter()
        .map(|(key, private)| {
            let (rel, path) = key.split_once(':').unwrap();
            let (owner, name) = path.split_once('.').unwrap();
            (
                key.clone(),
                module(rel),
                owner.to_string(),
                name.to_string(),
                *private,
            )
        })
        .collect();
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // The locks a use of field (or static) `name` through receiver
    // `recv` ("" for a static, "?" for an expression) may be, in module `m` inside an `impl` of
    // `owner`.
    let candidates = |m: &str, owner: &str, recv: &str, name: &str| -> Vec<String> {
        locks
            .iter()
            .filter(|(_, lm, lo, ln, private)| {
                ln == name
                    && (recv.is_empty() == (lo == "static"))
                    && (recv != "self" || lo == owner)
                    && (!private
                        || lm.is_empty()
                        || m == lm
                        || m.starts_with(&std::format!("{lm}/")))
            })
            .map(|(key, ..)| key.clone())
            .collect()
    };
    // The receiver and name of the path that ends at byte `end` of
    // `l` (`self.inner` gives ("self", "inner"), `STATE` ("",
    // "STATE"), `x[i].inner` and `f().inner` ("?", "inner")), and the
    // byte it starts at.
    let path_before = |l: &str, end: usize| -> (String, String, usize) {
        let start = l[..end]
            .rfind(|c: char| !(ident(c) || c == '.'))
            .map_or(0, |i| i + 1);
        let segs: Vec<&str> = l[start..end].split('.').collect();
        match segs[..] {
            [.., "", name] => ("?".to_string(), name.to_string(), start),
            [.., recv, name] => (recv.to_string(), name.to_string(), start),
            [name] => (String::new(), name.to_string(), start),
            [] => unreachable!(),
        }
    };
    let indent = |l: &str| l.len() - l.trim_start().len();
    let code = |l: &str| l.split("//").next().unwrap().trim().to_string();
    let acquire = [".lock(", ".try_lock(", ".lock_t("];
    let mut checked: Vec<(String, String)> = Vec::new();
    for (rel, text) in &lock_sources() {
        if rel == "lock.rs" {
            continue;
        }
        let m = module(rel);
        // The lines outside `mod tests` and `#[cfg(test)]` items, with
        // each method chain's continuation lines (`.name`) joined to
        // the line they continue.
        let mut lines: Vec<String> = Vec::new();
        // While skipping a `#[cfg(test)]` item: its bracket depth, and
        // whether a brace or bracket has opened.
        let mut skip: Option<(i32, bool)> = None;
        let mut cfg_test = false;
        for line in text.lines() {
            let t = line.trim();
            if t == "mod tests {" {
                break;
            }
            if cfg_test && !t.starts_with("#[") && !t.starts_with("//") {
                cfg_test = false;
                skip = Some((0, false));
            }
            if let Some((depth, opened)) = &mut skip {
                for c in code(line).chars() {
                    match c {
                        '{' | '[' => {
                            *depth += 1;
                            *opened = true;
                        }
                        '(' => *depth += 1,
                        '}' | ']' | ')' => *depth -= 1,
                        _ => {}
                    }
                }
                let t = code(line);
                if *depth == 0 && (*opened || t.ends_with(';') || t.ends_with(',')) {
                    skip = None;
                }
                continue;
            }
            if t == "#[cfg(test)]" {
                cfg_test = true;
                continue;
            }
            if t.starts_with('.')
                && t[1..].starts_with(ident)
                && let Some(prev) = lines.last_mut()
            {
                prev.push_str(t);
            } else {
                lines.push(line.to_string());
            }
        }
        // Each function as (name, indentation, line range of its body),
        // and the type each line's enclosing `impl` is for.
        let mut fns: Vec<(String, usize, usize, usize)> = Vec::new();
        let mut impls: Vec<(String, usize)> = Vec::new();
        let mut owner_at: Vec<String> = Vec::new();
        for (k, line) in lines.iter().enumerate() {
            let t = code(line);
            if impls
                .last()
                .is_some_and(|(_, ind)| *line == std::format!("{}}}", " ".repeat(*ind)))
            {
                impls.pop();
            }
            let head = t.strip_prefix("unsafe ").unwrap_or(&t);
            if let Some(rest) = head
                .strip_prefix("impl")
                .filter(|r| r.starts_with([' ', '<']))
            {
                // Skip the impl's own generics, then take the type after
                // `for` (a trait impl) or the type itself.
                let mut depth = 0;
                let mut at = 0;
                for (i, c) in rest.char_indices() {
                    match c {
                        '<' => depth += 1,
                        '>' => depth -= 1,
                        _ if depth == 0 && !c.is_whitespace() => {
                            at = i;
                            break;
                        }
                        _ => {}
                    }
                }
                let ty = rest[at..].split('{').next().unwrap();
                let ty = ty.rsplit_once(" for ").map_or(ty, |(_, t)| t);
                let ty = ty.split(['<', ' ']).next().unwrap();
                let ty = ty.rsplit("::").next().unwrap();
                if !t.ends_with('}') {
                    impls.push((ty.to_string(), indent(line)));
                }
            }
            owner_at.push(impls.last().map_or(String::new(), |(o, _)| o.clone()));
            let open = fns.last().is_some_and(|f| f.3 == usize::MAX);
            if t.starts_with("fn ") || t.contains(" fn ") {
                if open {
                    fns.last_mut().unwrap().3 = k;
                }
                let name = t[t.find("fn ").unwrap() + 3..]
                    .split(['(', '<'])
                    .next()
                    .unwrap()
                    .to_string();
                fns.push((name, indent(line), k + 1, usize::MAX));
            } else if open && t == "}" && indent(line) <= fns.last().unwrap().1 {
                fns.last_mut().unwrap().3 = k;
            }
        }
        if let Some(f) = fns.last_mut().filter(|f| f.3 == usize::MAX) {
            f.3 = lines.len();
        }
        // Each accessor of a guarded static, as (its name, the lock, the
        // line of its `&NAME`): a private function whose body ends in
        // `&NAME`, a static of this file in [`UNLOCKED_LOCKS`]. A public
        // one is no accessor, and its `&NAME` fails as an escape below.
        let accessors: Vec<(String, String, usize)> = fns
            .iter()
            .filter(|(_, _, from, _)| code(&lines[from - 1]).starts_with("fn "))
            .filter_map(|(name, _, from, to)| {
                let k = (*from..*to).rev().find(|&k| !code(&lines[k]).is_empty())?;
                let t = code(&lines[k]);
                let target = t.strip_prefix('&').filter(|s| s.chars().all(ident))?;
                let key = candidates(&m, "", "", target)
                    .into_iter()
                    .find(|key| UNLOCKED_LOCKS.iter().any(|(k, _)| k == key))?;
                Some((name.clone(), key, k))
            })
            .collect();
        // The locks a use of the path ending at byte `end` of line `k`
        // may be, and the first of them in [`UNLOCKED_LOCKS`], if any.
        // A path that is a call `f()` of an accessor is its static.
        let resolve = |k: usize, end: usize| -> (Vec<String>, Option<String>) {
            let (recv, name, _) = path_before(&lines[k], end);
            if name.is_empty()
                && let Some(s) = lines[k][..end].strip_suffix("()")
            {
                let at = s.rfind(|c: char| !ident(c)).map_or(0, |i| i + 1);
                if !s[..at].ends_with(['.', ':'])
                    && let Some((_, key, _)) = accessors.iter().find(|(f, ..)| *f == s[at..])
                {
                    return (std::vec![key.clone()], Some(key.clone()));
                }
            }
            let found = candidates(&m, &owner_at[k], &recv, &name);
            let hit = found
                .iter()
                .find(|key| UNLOCKED_LOCKS.iter().any(|(k, _)| k == key))
                .cloned();
            (found, hit)
        };
        // The guarded lock an acquisition ending at byte `end` of line
        // `k` takes, if any; an acquisition that may be a guarded lock
        // must be exactly one lock.
        let guarded = |k: usize, end: usize| -> Option<String> {
            let (found, hit) = resolve(k, end);
            assert!(
                hit.is_none() || found.len() == 1,
                "{rel}: `{}` may be any of {found:?}",
                lines[k].trim()
            );
            hit
        };
        // No reference to a guarded lock escapes through its owner: no
        // `self.field` of a guarded lock, outside a string literal, is
        // borrowed (an accessor returning `&self.field` would let other
        // code acquire it under another name), nor a guarded static
        // but in its accessor.
        for (k, line) in lines.iter().enumerate() {
            let l = line.split("//").next().unwrap();
            for name in UNLOCKED_LOCKS
                .iter()
                .map(|(key, _)| key.rsplit('.').next().unwrap())
            {
                for (i, _) in l.match_indices(name) {
                    let end = i + name.len();
                    let after = &l[end..];
                    if (i > 0 && ident(l[..i].chars().next_back().unwrap()))
                        || after.starts_with(ident)
                        || after.starts_with('(')
                        || l[..i].matches('"').count() % 2 == 1
                    {
                        continue;
                    }
                    let (recv, _, start) = path_before(l, end);
                    let (_, Some(key)) = resolve(k, end) else {
                        continue;
                    };
                    let fixed = recv.is_empty() && key.contains(":static.");
                    if (recv != "self" && !fixed)
                        || (fixed && accessors.iter().any(|(_, _, a)| *a == k))
                    {
                        continue;
                    }
                    let before = l[..start].trim_end();
                    assert!(
                        !(before.ends_with('&') && !before.ends_with("&&"))
                            && !before.ends_with("&mut"),
                        "{rel}: a reference to {key} escapes: `{}`",
                        l.trim()
                    );
                }
            }
        }
        // An accessor's result is used at once, never kept or passed on:
        // each call is followed by the method it serves.
        let uses = [
            ".lock(",
            ".try_lock(",
            ".lock_t(",
            ".lock_held_perm(",
            ".unlock_held_perm(",
            ".value_held_mut(",
        ];
        for line in &lines {
            let l = line.split("//").next().unwrap();
            for (f, key, _) in &accessors {
                let call = std::format!("{f}()");
                for (i, _) in l.match_indices(call.as_str()) {
                    if (i > 0 && ident(l[..i].chars().next_back().unwrap()))
                        || l[..i].ends_with("fn ")
                    {
                        continue;
                    }
                    assert!(
                        uses.iter().any(|u| l[i + call.len()..].starts_with(u)),
                        "{rel}: the accessor {f} of {key} is used other than at once: `{}`",
                        l.trim()
                    );
                }
            }
        }
        let mut seen = 0;
        for (k, line) in lines.iter().enumerate() {
            for a in acquire {
                let l = line.split("//").next().unwrap();
                for (i, _) in l.match_indices(a) {
                    if guarded(k, i).is_some() {
                        seen += 1;
                    }
                }
            }
        }
        for (name, ind, from, to) in &fns {
            let body = &lines[*from..*to];
            let mut hits = Vec::new();
            for (k, line) in body.iter().enumerate() {
                let l = line.split("//").next().unwrap();
                for a in acquire {
                    for (i, _) in l.match_indices(a) {
                        if let Some(key) = guarded(from + k, i) {
                            hits.push((k, i + a.len(), key));
                        }
                    }
                }
            }
            seen -= hits.len();
            // The end of the previous guard's scope, as a body line.
            let mut free_from = 0;
            for (a, open, key) in &hits {
                let (a, open) = (*a, *open);
                assert!(
                    a >= free_from,
                    "{rel}: {name}: {key} is acquired while a guard is in scope"
                );
                // The acquisition ends the statement, which is a `let`.
                let mut depth = 1;
                let mut rest = String::new();
                'outer: for (j, l) in body[a..].iter().enumerate() {
                    let l = if j == 0 { &l[open..] } else { l.as_str() };
                    for (i, c) in l.char_indices() {
                        match c {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        if depth == 0 {
                            rest = l[i + 1..].trim().to_string();
                            break 'outer;
                        }
                    }
                }
                assert!(
                    rest == ";",
                    "{rel}: {name}: the guard of {key} is chained into a call"
                );
                let decl = body[a].trim();
                let at = indent(&body[a]);
                assert!(
                    decl.starts_with("let ") && at >= ind + 4,
                    "{rel}: {name}: the guard of {key} is not bound with `let`"
                );
                let guard = decl
                    .trim_start_matches("let ")
                    .trim_start_matches("mut ")
                    .split([' ', '=', ':'])
                    .next()
                    .unwrap();
                let release = std::format!("{guard}.unlock();");
                // The guard's scope: up to the first line indented less
                // than its `let`, the end of the enclosing block.
                let after = &body[a + 1..];
                let scope = after
                    .iter()
                    .position(|l| !code(l).is_empty() && indent(l) < at)
                    .unwrap_or(after.len());
                free_from = a + 1 + scope;
                let mut tail_unlock = false;
                // The loops opened in the guard's scope, as (indent,
                // label): a `break` or `continue` stays in the scope
                // only inside one of them (with its label, if it names
                // one).
                let mut loops: Vec<(usize, String)> = Vec::new();
                for (k, l) in after[..scope].iter().enumerate() {
                    let t = code(l);
                    assert!(
                        !t.contains('?'),
                        "{rel}: {name}: `?` after the acquisition of {key}"
                    );
                    if !t.is_empty() {
                        let li = indent(l);
                        while loops
                            .last()
                            .is_some_and(|(o, _)| li < *o || (li == *o && t.starts_with('}')))
                        {
                            loops.pop();
                        }
                    }
                    let (label, head) = match t.split_once(": ") {
                        Some((lb, h)) if lb.starts_with('\'') => (lb.to_string(), h),
                        _ => (String::new(), t.as_str()),
                    };
                    if head == "loop"
                        || head.starts_with("loop ")
                        || head.starts_with("while ")
                        || head.starts_with("for ")
                    {
                        loops.push((indent(l), label));
                    }
                    for w in ["break", "continue"] {
                        for (i, _) in t.match_indices(w) {
                            let rest = &t[i + w.len()..];
                            if (i > 0 && ident(t[..i].chars().next_back().unwrap()))
                                || rest.starts_with(ident)
                            {
                                continue;
                            }
                            let to = rest.trim_start();
                            let target = if to.starts_with('\'') {
                                to.split(|c: char| c != '\'' && !ident(c)).next().unwrap()
                            } else {
                                ""
                            };
                            assert!(
                                loops
                                    .iter()
                                    .any(|(_, lb)| target.is_empty() || lb == target),
                                "{rel}: {name}: `{w}` out of the scope of the guard of {key}"
                            );
                        }
                    }
                    if t.starts_with("return") || t.contains(" return ") {
                        assert!(
                            k > 0 && after[k - 1].trim() == release,
                            "{rel}: {name}: a `return` without `{release}` before it"
                        );
                    }
                    if t == release && indent(l) == at {
                        tail_unlock = true;
                    }
                }
                assert!(
                    tail_unlock,
                    "{rel}: {name}: no `{release}` on the path to the end of its block"
                );
                let holder = (key.clone(), std::format!("{rel}:{name}"));
                if !checked.contains(&holder) {
                    checked.push(holder);
                }
            }
        }
        assert_eq!(seen, 0, "{rel}: an acquisition outside a function");
    }
    for (key, holders) in UNLOCKED_LOCKS {
        let mut found: Vec<&str> = checked
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, f)| f.as_str())
            .collect();
        let mut want = holders.to_vec();
        found.sort_unstable();
        want.sort_unstable();
        assert_eq!(found, want, "{key}: the holders found differ from the list");
    }
}

/// The rule of `lock::LockSpec`'s doc: an impl that defines its lock
/// invariant, or a frame other than `true`, is for a type whose every
/// lock is in [`UNLOCKED_LOCKS`], so each release checks both
/// ([`guards_end_in_unlock`]); a frame of `true` is exempt, since no
/// release can break it. The impls are the scan that
/// `lock::tests::lock_axioms_state_assumed_specs` reads too
/// ([`crate::testutil::lock_spec_impls`]): an impl defines what it does
/// not delegate to `assumed_inv` or `assumed_frame`, and its frame is
/// `true` when the body of its `lock_spec_frame` is that one word. A
/// lock's type is the `T` of its declaration ([`lock_decls_typed`]),
/// named through a `use crate::..` or `use super::..` of it in the
/// declaring file or else in that file's module. Every lock's type must
/// be some impl's (`Lock::new` needs `T: LockSpec`), so a spelling the
/// test misreads fails instead of escaping.
#[test]
fn defined_lock_specs_are_unlocked_locks() {
    use std::string::ToString;
    let sources = lock_sources();
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let module = crate::testutil::module_of;
    let name = crate::testutil::type_key;
    // The type `ty`, written in file `rel` with text `text`, by its
    // full path: through a `use crate::..` or `use super::..` of it
    // (one path or one flat group, possibly over several lines) before
    // the file's `mod tests`, else in the file's module.
    let resolve = |rel: &str, text: &str, ty: &str| -> String {
        let m = module(rel);
        if !ty.is_empty() && ty.chars().all(ident) {
            let mut uses = Vec::new();
            let mut open: Option<String> = None;
            for line in text.lines() {
                let t = line.trim();
                if t == "mod tests {" {
                    break;
                }
                if let Some(mut s) = open.take() {
                    s.push_str(t);
                    if t.ends_with(';') {
                        uses.push(s);
                    } else {
                        open = Some(s);
                    }
                    continue;
                }
                let t = ["pub(crate) ", "pub(super) ", "pub "]
                    .iter()
                    .find_map(|p| t.strip_prefix(p))
                    .unwrap_or(t);
                if let Some(rest) = t.strip_prefix("use ") {
                    if rest.ends_with(';') {
                        uses.push(rest.to_string());
                    } else {
                        open = Some(rest.to_string());
                    }
                }
            }
            let parent = m.rsplit_once("::").map_or("crate", |(p, _)| p).to_string();
            for u in &uses {
                let path: String = u.trim_end_matches(';').split_whitespace().collect();
                let full = if let Some(p) = path.strip_prefix("crate::") {
                    std::format!("crate::{p}")
                } else if let Some(p) = path.strip_prefix("super::") {
                    std::format!("{parent}::{p}")
                } else {
                    continue;
                };
                if full.ends_with(std::format!("::{ty}").as_str()) {
                    return full;
                }
                if let Some((base, group)) = full.split_once("::{")
                    && group
                        .strip_suffix('}')
                        .is_some_and(|g| g.split(',').any(|n| n == ty))
                {
                    return std::format!("{base}::{ty}");
                }
            }
        }
        name(&m, ty)
    };
    let locks: Vec<(String, String)> = lock_decls_typed()
        .into_iter()
        .filter(|(_, _, ty)| !ty.is_empty())
        .map(|(key, _, ty)| {
            let rel = key.split_once(':').unwrap().0;
            let text = &sources.iter().find(|(r, _)| r == rel).unwrap().1;
            (key.clone(), resolve(rel, text, &ty))
        })
        .collect();
    let impls = crate::testutil::lock_spec_impls();
    assert!(!impls.is_empty(), "no `LockSpec` impl was found in src/");
    // Fail closed: `Lock::new` needs `T: LockSpec`, so a lock whose
    // type names no scanned impl is a spelling this test misread, and
    // would otherwise escape the rule below.
    for (key, ty) in &locks {
        assert!(
            impls.iter().any(|s| s.ty == *ty),
            "{key}: its type reads as `{ty}`, which no `LockSpec` impl in src/ is for; spell it so it resolves (`use crate::..::T;` or `Lock<crate::..::T>`)"
        );
    }
    for s in &impls {
        if !s.inv && !(s.frame && !s.true_frame) {
            continue;
        }
        let at = std::format!("{}:{}", s.rel, s.start + 1);
        let of: Vec<&String> = locks
            .iter()
            .filter(|(_, t)| *t == s.ty)
            .map(|(k, _)| k)
            .collect();
        assert!(
            !of.is_empty(),
            "{at}: no lock of `{}` was found in src/",
            s.ty
        );
        for key in of {
            assert!(
                UNLOCKED_LOCKS.iter().any(|(k, _)| k == key),
                "{at}: `{}` defines its lock invariant or a frame other than `true`, but its lock {key} is not in UNLOCKED_LOCKS: its holders' releases check nothing (`lock::LockSpec`)",
                s.ty
            );
        }
    }
}

/// The locks `fork_prepare` takes, keyed as in [`lock_inventory`].
/// Adding a lock to the crate means adding it here *and* to the
/// handlers; `assert_locks` refuses a key it has no assertion for.
const FORK_LOCKS: &[&str] = &[
    "arena.rs:Arena.stats",
    "arena.rs:Arenas.lock",
    "heap/tcaches.rs:Tcaches.lock",
    "class/mod.rs:ClassState.inner",
    "class/sets.rs:SetTable.tables",
    "class/table.rs:SlabTable.inner",
    "heap/shared.rs:Shared.stats",
    "huge/mod.rs:Huge.inner",
    "meta.rs:Meta.inner",
    "purge/background.rs:static.STATE",
];

/// Assert that every lock in [`FORK_LOCKS`] is held (`locked`) or free
/// (`!locked`), for the arenas in `arenas`.
fn assert_locks(s: &Shared, arenas: &[u16], locked: bool) {
    let state = |held: bool, what: &str| {
        assert_eq!(
            held,
            locked,
            "{what} {}",
            if locked { "not locked" } else { "still locked" }
        );
    };
    for key in FORK_LOCKS {
        match *key {
            "arena.rs:Arena.stats" => {
                for &a in arenas {
                    state(
                        s.arena(a, vstd::prelude::Tracked::assume_new())
                            .unwrap()
                            .0
                            .stats_raw_lock()
                            .is_locked(),
                        &std::format!("arena {a} stats"),
                    );
                }
            }
            "arena.rs:Arenas.lock" => state(s.arenas.raw_lock().is_locked(), "arena registry"),
            "heap/tcaches.rs:Tcaches.lock" => {
                state(s.tcaches.raw_lock().is_locked(), "tcache table")
            }
            "class/mod.rs:ClassState.inner" => {
                for &a in arenas {
                    for c in 0..NCLASSES {
                        state(
                            s.arena(a, vstd::prelude::Tracked::assume_new())
                                .unwrap()
                                .0
                                .classes[c]
                                .raw_lock()
                                .is_locked(),
                            &std::format!("arena {a} class {c}"),
                        );
                    }
                }
            }
            "class/sets.rs:SetTable.tables" => {
                for &a in arenas {
                    state(
                        s.arena(a, vstd::prelude::Tracked::assume_new())
                            .unwrap()
                            .0
                            .sets()
                            .raw_lock()
                            .is_locked(),
                        &std::format!("arena {a} sets"),
                    );
                }
            }
            "class/table.rs:SlabTable.inner" => {
                for c in 0..NCLASSES {
                    state(
                        s.tables()[c].raw_lock().is_locked(),
                        &std::format!("table {c}"),
                    );
                }
            }
            "heap/shared.rs:Shared.stats" => state(s.stats.raw().is_locked(), "process stats"),
            "huge/mod.rs:Huge.inner" => state(s.huge.raw_lock().is_locked(), "huge"),
            "meta.rs:Meta.inner" => state(s.meta.raw_lock().is_locked(), "meta"),
            "purge/background.rs:static.STATE" => {
                state(purge::background::is_locked(), "background state")
            }
            other => panic!("no fork-handler assertion for lock {other}"),
        }
    }
}

/// `resume::axiom_fork_resumed` restates the fork hold, whose creation
/// lock cell permission is exclusive: a second call would prove
/// `false`. Its one call is in `resume::resumed`, and `resumed`'s one
/// call is in `fork_release`, which each handler calls once.
#[test]
fn fork_hold_is_resumed_once() {
    // Spelled in parts, so that this test's own lines match nothing.
    let src = include_str!("mod.rs");
    let calls = |name: &str| {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(name) && !l.contains("fn "))
            .count()
    };
    let axiom = std::format!("{}{}(", "axiom_fork_", "resumed");
    let resumed = std::format!("{}{}(", "resume::", "resumed");
    let release = std::format!("{}{}(", "fork_", "release");
    assert_eq!(calls(&axiom), 1, "calls of the resume axiom");
    assert_eq!(calls(&resumed), 1, "calls of resume::resumed");
    assert_eq!(calls(&release), 2, "calls of fork_release");
    // The other once-per-handler premises: the state lock's hold, in
    // the parent and the child handler, and the mask's, in the release;
    // the prepare's lock-order root and the thread's mask, in the
    // prepare.
    let state = std::format!("{}{}(", "axiom_fork_resumed_", "state");
    let sig = std::format!("{}{}(", "axiom_fork_resumed_", "sig");
    let entry = std::format!("{}{}(", "axiom_fork_", "entry");
    let mask = std::format!("{}{}(", "axiom_thread_", "mask");
    assert_eq!(calls(&state), 2, "calls of the state hold's premise");
    assert_eq!(calls(&sig), 1, "calls of the mask token's premise");
    let lock = include_str!("../lock.rs");
    let sys = include_str!("../sys/mod.rs");
    let calls_in = |text: &str, name: &str| {
        text.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(name) && !l.contains("fn "))
            .count()
    };
    assert_eq!(
        calls(&entry) + calls_in(lock, &entry),
        1,
        "calls of the prepare's lock-order premise"
    );
    assert_eq!(
        calls(&mask) + calls_in(sys, &mask),
        1,
        "calls of the thread mask premise"
    );
}

/// The once-cells' fork-child resets, which take the permissions a
/// thread of the parent held (`axiom_line_orphaned`,
/// `sys::cgroup::axiom_cell_orphaned`), run only in the child's fork
/// handler: across every non-test source file, each axiom has one
/// call, in its reset, each reset one caller, `fork_child` (through
/// `Shared::cgroup_fork_child` for the cgroup record), and
/// `fork_child` is named only where `pthread_atfork` registers it.
#[test]
fn fork_child_resets_are_child_only() {
    use std::string::ToString;
    /// Every `.rs` file under `dir` but the loom and Kani trees and the
    /// `tests.rs` files of test modules, as (path, text before its first
    /// inline `#[cfg(test)]` test module).
    fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.to_str().unwrap().to_string();
            if path.is_dir() {
                if !name.ends_with("loom_tests") && !name.ends_with("kani") {
                    sources(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("/tests.rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let cut = ["tests", "linux_tests", "pure_tests"]
                    .iter()
                    .filter_map(|m| text.find(&std::format!("\n#[cfg(test)]\nmod {m} {{")))
                    .min()
                    .unwrap_or(text.len());
                out.push((name, text[..cut].to_string()));
            }
        }
    }
    let mut files = Vec::new();
    sources(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut files,
    );
    // Calls: lines naming `name` that are neither comments nor the
    // item's own declaration.
    let calls = |name: &str| -> Vec<String> {
        files
            .iter()
            .flat_map(|(f, src)| {
                src.lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .filter(|l| l.contains(name) && !l.contains("fn "))
                    .map(move |l| std::format!("{f}: {}", l.trim()))
            })
            .collect()
    };
    let api = include_str!("mod.rs");
    let child = api
        .split("extern \"C\" fn fork_child() {")
        .nth(1)
        .and_then(|s| s.split("\n}\n").next())
        .unwrap_or("");
    let one = |name: &str, file: &str| {
        let found = calls(name);
        assert_eq!(found.len(), 1, "calls of {name}: {found:?}");
        assert!(
            found[0].contains(file),
            "{name} called outside {file}: {found:?}"
        );
    };
    one("axiom_line_orphaned(", "src/api/mod.rs");
    one("axiom_cell_orphaned(", "src/sys/cgroup.rs");
    // The child handler's resets, and each wait's early one for a fork
    // child's handler in the window (`lock::in_child_fork_window`).
    let two = |name: &str, files: [&str; 2]| {
        let mut found = calls(name);
        found.sort();
        assert_eq!(found.len(), 2, "calls of {name}: {found:?}");
        for (f, file) in found.iter().zip(files) {
            assert!(
                f.contains(file),
                "{name} called outside {files:?}: {found:?}"
            );
        }
    };
    two(
        "feature_line_fork_child(",
        ["src/api/mod.rs", "src/api/mod.rs"],
    );
    one("cgroup_fork_child(", "src/api/mod.rs");
    two(".fork_child()", ["src/heap/shared.rs", "src/sys/cgroup.rs"]);
    assert!(
        child.contains("feature_line_fork_child(")
            && child.contains("cgroup_fork_child(")
            && child.contains("reveal(crate::sys::cgroup::in_fork_child)"),
        "the resets and the fork-child capability are fork_child's"
    );
    let handler = calls("(fork_child)");
    assert!(
        handler.len() == 1 && handler[0].contains("pthread_atfork"),
        "fork_child is named outside its registration: {handler:?}"
    );
    assert!(files.len() > 50, "the source tree was not found");
}

/// The set of locks the handlers cover is exactly the set of locks in
/// the crate (design 9).
#[test]
fn fork_lock_list_matches_the_source_tree() {
    let found = lock_inventory();
    let mut want: Vec<String> = FORK_LOCKS
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    want.sort();
    assert_eq!(
        found, want,
        "locks in src/ differ from the fork handlers' list: add every new lock to \
         fork_prepare/fork_release in nesting order, to FORK_LOCKS and to assert_locks"
    );
}

/// The text between the first `(` at or after byte `from` of `text`
/// and the `)` that closes it.
fn parenthesized(text: &str, from: usize) -> &str {
    let open = from + text[from..].find('(').unwrap();
    let mut depth = 0;
    for (k, c) in text[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..open + k];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced parentheses from byte {from}")
}

/// `list` split at its top-level commas, each part trimmed, empty
/// parts (a trailing comma's) dropped.
fn top_level(list: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    for (k, c) in list.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(list[start..k].trim());
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push(list[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// The parameters of the nearest `fn` item above the line `at`
/// (`file:line: ..`, as `testutil::grant_sites` gives it), after
/// checking that no `let` between the `fn` line and `at` rebinds one
/// (`let p = q;`), so that a parameter's name still means the
/// argument there; with the file's text and the byte range of the line.
/// Only lines that start with `let` are read: a rebinding by `if let`,
/// `match`, `for` or a closure is not seen.
fn own_params(
    what: &str,
    at: &str,
) -> (
    std::string::String,
    core::ops::Range<usize>,
    Vec<std::string::String>,
) {
    let (file, rest) = at.split_once(':').unwrap();
    let line: usize = rest.split_once(':').unwrap().0.parse().unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let text = std::fs::read_to_string(src.join(file)).unwrap();
    let starts: Vec<usize> = core::iter::once(0)
        .chain(text.match_indices('\n').map(|(k, _)| k + 1))
        .collect();
    let item = (0..line)
        .rev()
        .find(|&i| crate::testutil::fn_item(text[starts[i]..].lines().next().unwrap().trim()))
        .unwrap_or_else(|| panic!("{at}: no `fn` item above `{what}`"));
    let params: Vec<std::string::String> = top_level(parenthesized(&text, starts[item]))
        .into_iter()
        .map(|p| p.split_once(':').unwrap().0.trim())
        .map(|p| p.strip_prefix("mut ").unwrap_or(p).into())
        .collect();
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for l in text[starts[item]..starts[line - 1]].lines().skip(1) {
        let Some(pat) = l.trim().strip_prefix("let ") else {
            continue;
        };
        let pat = pat.split(['=', ':']).next().unwrap();
        for p in &params {
            let rebinds = pat
                .match_indices(p.as_str())
                .any(|(k, _)| !pat[..k].ends_with(ident) && !pat[k + p.len()..].starts_with(ident));
            assert!(
                !rebinds,
                "{at}: `{}` rebinds the parameter `{p}` before `{what}`",
                l.trim()
            );
        }
    }
    let end = starts.get(line).copied().unwrap_or(text.len());
    (text, starts[line - 1]..end, params)
}

/// Each mint `mint` on the line `at` (`file:line: ..`, as
/// `testutil::grant_sites` gives it) names only its function's own
/// parameters: each field of the `CEntry` an `axiom_c_call` builds, or
/// the out-pointer an `axiom_c_out` takes, is a parameter of the
/// nearest `fn` item above it, as is or cast with `as`, no parameter
/// fills two fields, and no `let` between the `fn` line and the mint
/// rebinds a parameter ([`own_params`]). So the token is for the
/// arguments of the C call that entered challoc, not for another
/// pointer the body could compute.
fn own_arguments(mint: &str, at: &str) {
    let (text, line, params) = own_params(mint, at);
    let params: Vec<&str> = params.iter().map(|p| p.as_str()).collect();
    let calls: Vec<usize> = text[line.clone()]
        .match_indices(mint)
        .map(|(k, _)| line.start + k)
        .collect();
    assert!(!calls.is_empty(), "{at}: no `{mint}` on the line");
    for call in calls {
        let args = parenthesized(&text, call);
        let named: Vec<&str> = if mint.contains("c_out") {
            let args = top_level(args);
            assert!(
                args.len() == 2 && (args[1] == "true" || args[1] == "false"),
                "{at}: `{mint}` with arguments {args:?}"
            );
            std::vec![args[0]]
        } else {
            let (entry, fields) = args
                .split_once('{')
                .unwrap_or_else(|| panic!("{at}: `{mint}` of no `CEntry` literal"));
            assert!(
                entry.trim().starts_with("CEntry::"),
                "{at}: `{mint}` of `{entry}`, not a `CEntry` literal"
            );
            let fields = fields.trim_end().strip_suffix('}').unwrap();
            top_level(fields)
                .into_iter()
                .map(|f| f.split_once(':').map_or(f, |(_, v)| v.trim()))
                .collect()
        };
        assert!(!named.is_empty(), "{at}: `{mint}` names no argument");
        for (k, v) in named.iter().enumerate() {
            let v = v.split_once(" as ").map_or(*v, |(v, _)| v.trim());
            assert!(
                params.contains(&v),
                "{at}: `{mint}` names `{v}`, which is not a parameter of its function \
                 ({params:?}): a C call's token is for that call's own arguments"
            );
            let twice = named[..k]
                .iter()
                .any(|w| w.split_once(" as ").map_or(*w, |(w, _)| w.trim()) == v);
            assert!(!twice, "{at}: `{mint}` names the parameter `{v}` twice");
        }
    }
}

/// Each call `call` (a helper's name and its `(`) on the line `at`
/// passes the helper, in order, its own parameters' names, each a
/// parameter of the calling arm, not rebound before the call
/// ([`own_params`]). The helper's mint names its parameters
/// ([`own_arguments`]), so this makes them the arm's: the arguments of
/// the C call that entered challoc. `helper` is the start of the
/// helper's `fn` line in the same file.
fn passes_own_arguments(call: &str, helper: &str, at: &str) {
    let (text, line, params) = own_params(call, at);
    let def = text
        .find(helper)
        .unwrap_or_else(|| panic!("{at}: no `{helper}` in the file"));
    let want = top_level(parenthesized(&text, def))
        .into_iter()
        .map(|p| p.split_once(':').unwrap().0.trim())
        .map(|p| p.strip_prefix("mut ").unwrap_or(p))
        .collect::<Vec<_>>();
    let calls: Vec<usize> = text[line.clone()]
        .match_indices(call)
        .map(|(k, _)| line.start + k)
        .collect();
    assert!(!calls.is_empty(), "{at}: no `{call}` on the line");
    for c in calls {
        let got = top_level(parenthesized(&text, c));
        assert_eq!(
            got, want,
            "{at}: `{call}` arguments: got = {got:?}, want = {want:?} (the helper's \
             parameters, in order)"
        );
        for a in &got {
            assert!(
                params.iter().any(|p| p == a),
                "{at}: `{call}` passes `{a}`, which is not a parameter of the arm \
                 ({params:?}): the helper's token is for the C call's own arguments"
            );
        }
    }
}

/// The C-call tokens (`axiom_c_call` of each `api` module, `libc`'s
/// `axiom_c_out`) and the errno loan (`vspec::errno::lend`) are minted
/// only in an export body, the Rust side of one C call: an
/// `extern "C" fn` or an arm `fn x / je_x(..)` of a
/// `verified_export_pair!`, or one of the helpers such an arm calls
/// (`stats_print_entry`, `nametomib_call`, `memptr_out`), each called
/// by one arm. At most one mint per body: two tokens for one C call
/// would let `heap::live::axiom_c_claim` mint two `Live` for one block,
/// or two write permissions for one out-pointer, either proving
/// `false`. And each token is for the C call's own arguments
/// ([`own_arguments`]). The 4 out-pointer tokens are minted each in one
/// spelling: in the three allocating `challoc_*_usable` exports as the
/// token their hand-back names ([`out_tokens`]), and in `memptr_out`,
/// which only the `posix_memalign` pair calls, as the token its
/// hand-back names; so each export's `c_out_entry` and `c_out` name its
/// one call.
#[test]
fn c_call_tokens_only_in_export_bodies() {
    let helpers = [
        ("api/print.rs:", concat!("fn stats_print_", "entry(")),
        ("api/mallctl.rs:", concat!("fn nametomib_", "call(")),
        ("api/libc.rs:", concat!("fn memptr_", "out(")),
    ];
    // Spelled in parts, so that this test's own lines match nothing.
    for mint in [
        concat!("axiom_", "c_call("),
        concat!("axiom_", "c_out("),
        concat!("errno::", "lend()"),
    ] {
        let sites: Vec<_> = crate::testutil::grant_sites(mint)
            .into_iter()
            .filter(|(_, at)| !at.contains("axiom fn "))
            .collect();
        assert!(!sites.is_empty(), "no call of `{mint}` under src/");
        for (sig, at) in &sites {
            // The fork handlers are `extern "C"` but no C call into challoc.
            let export = (sig.contains("extern \"C\" fn ") && !sig.contains("fn fork_"))
                || (sig.starts_with("fn ") && sig.contains(" / je_"));
            let helper = helpers
                .iter()
                .any(|(file, f)| at.starts_with(file) && sig.starts_with(f));
            assert!(
                at.starts_with("api/") && (export || helper),
                "{at}: `{mint}` in `{sig}`, which is not an export body: a C-call \
                 token is minted only where one C call enters challoc"
            );
            let file = at.split_once(':').unwrap().0;
            // Every mint on every line of the body: a second one on
            // the same line is a second token too.
            let n: usize = sites
                .iter()
                .filter(|(s, a)| s == sig && a.split_once(':').unwrap().0 == file)
                .map(|(_, a)| {
                    a.split_once(": `")
                        .map_or("", |(_, t)| t)
                        .matches(mint)
                        .count()
                })
                .sum();
            assert_eq!(
                n, 1,
                "{file} `{sig}`: got = {n} calls of `{mint}`, want = 1 (one token per C call)"
            );
            if !mint.starts_with("errno") {
                own_arguments(mint, at);
            }
            // Out-pointer tokens, each in its one spelling: directly in
            // the three `challoc_*_usable` exports, as the token their
            // hand-back names ([`out_tokens`]), and in `memptr_out`.
            if mint.contains("c_out") {
                let usable = std::format!("`{}`", out_tokens()[1].0);
                let memptr = concat!("`let tracked o = axiom_", "c_out(out, false);`");
                assert!(
                    at.starts_with("api/libc.rs:")
                        && ((export && export_head(sig) && at.ends_with(&usable))
                            || (helper && at.ends_with(memptr))),
                    "{at}: an out-pointer token minted in `{sig}`, not as {usable} in \
                     an export or {memptr} in `memptr_out`"
                );
            }
        }
        if mint.contains("c_out") {
            assert_eq!(sites.len(), 4, "out-pointer token mints");
        }
    }
    // An arm that mints through a helper mints nothing itself: that
    // would be two tokens for one C call.
    for (file, arm, mint) in [
        (
            "api/print.rs:",
            "fn malloc_stats_print / je_malloc_stats_print(",
            concat!("axiom_", "c_call("),
        ),
        (
            "api/mallctl.rs:",
            "fn mallctlnametomib / je_mallctlnametomib(",
            concat!("axiom_", "c_call("),
        ),
        (
            "api/libc.rs:",
            "fn posix_memalign / je_posix_memalign(",
            concat!("axiom_", "c_out("),
        ),
    ] {
        let direct: Vec<_> = crate::testutil::grant_sites(mint)
            .into_iter()
            .filter(|(sig, at)| at.starts_with(file) && sig.starts_with(arm))
            .map(|(_, at)| at)
            .collect();
        assert!(
            direct.is_empty(),
            "`{arm}` mints `{mint}` through its helper and also directly: {direct:?}"
        );
    }
    crate::testutil::assert_only_at(
        concat!("stats_print_", "entry("),
        &[(
            "api/print.rs:",
            "fn malloc_stats_print / je_malloc_stats_print(",
            1,
        )],
    );
    crate::testutil::assert_only_at(
        concat!("nametomib_", "call("),
        &[(
            "api/mallctl.rs:",
            "fn mallctlnametomib / je_mallctlnametomib(",
            1,
        )],
    );
    // Each arm passes its helper its own arguments, so the helper's
    // token ([`own_arguments`]) is for the C call's.
    for (file, helper, call) in [
        (
            "api/print.rs:",
            concat!("fn stats_print_", "entry("),
            concat!("stats_print_", "entry("),
        ),
        (
            "api/mallctl.rs:",
            concat!("fn nametomib_", "call("),
            concat!("nametomib_", "call("),
        ),
    ] {
        let at: Vec<_> = crate::testutil::grant_sites(call)
            .into_iter()
            .filter(|(sig, at)| at.starts_with(file) && !sig.starts_with(helper))
            .map(|(_, at)| at)
            .collect();
        assert_eq!(at.len(), 1, "{file} lines calling `{call}`: {at:?}");
        passes_own_arguments(call, helper, &at[0]);
    }
    // `memptr_out` is called once, by the `posix_memalign` pair, as
    // the token that pair's hand-back names ([`out_tokens`]).
    let memptr = concat!("memptr_", "out(");
    crate::testutil::assert_only_at(
        memptr,
        &[("api/libc.rs:", "fn posix_memalign / je_posix_memalign(", 1)],
    );
    let call = std::format!("`{}`", out_tokens()[0].0);
    for (sig, at) in crate::testutil::grant_sites(memptr) {
        assert!(
            sig.starts_with(concat!("fn memptr_", "out(")) || at.ends_with(&call),
            "{at}: `{memptr}` in `{sig}`, not as {call}"
        );
    }
}

/// Every function that takes a lock's guard (`Lock::lock`, `Lock::try_lock`,
/// `Lock::lock_t`) is one its `lock_inv` axiom's doc, or its `LockSpec`
/// impl's where the impl defines the invariant, names among the lock's
/// holders: `LockGuard::drop` checks nothing, so each such doc carries
/// the argument that every holder leaves the invariant true (or, for a
/// defined invariant, ends in `LockGuard::unlock`), and a holder the
/// doc does not name has no such argument. The
/// table lists, per lock of [`lock_inventory`], each holder as (file,
/// function, the field or static its call names), copied by hand from
/// the doc: the docs are not parsed, so the two change together. A
/// holder matches a row by file, function and receiver name, not by
/// the lock's type, so a row filed under the wrong lock is not caught
/// when the field names agree. Raw
/// access, which skips the guard, is only the fork handlers':
/// `lock_held` and `unlock_held` (and their `_perm` forms) in
/// `fork_prepare`, `fork_release` and the arena and background-thread
/// helpers they call, and a write through a hold's permission,
/// `value_held_mut`, once, in the background thread's `fork_child`
/// (`RawLock::lock` is not scanned). `lock.rs`, which defines the calls,
/// is skipped, as are test files and modules.
#[test]
fn lock_holders_match_inv_docs() {
    use std::string::ToString;
    type Holders = &'static [(&'static str, &'static str, &'static str)];
    const HOLDERS: &[(&str, Holders)] = &[
        // `stats::ArenaStats`'s `LockSpec` impl.
        (
            "arena.rs:Arena.stats",
            &[
                ("arena.rs:", "pub fn stats_into(", "stats"),
                ("arena.rs:", "pub fn add_stats_to(", "stats"),
                ("arena.rs:", "pub fn store_stats(", "stats"),
            ],
        ),
        // `arena::Created`'s `LockSpec` impl.
        (
            "arena.rs:Arenas.lock",
            &[
                ("arena.rs:", "pub fn assign(", "lock"),
                ("arena.rs:", "pub fn create(", "lock"),
                ("arena.rs:", "fn materialise<", "lock"),
            ],
        ),
        // `class::Inner`'s `LockSpec` impl.
        (
            "class/mod.rs:ClassState.inner",
            &[
                ("class/mod.rs:", "fn plan(", "inner"),
                ("class/mod.rs:", "fn reopen(", "inner"),
                ("class/mod.rs:", "fn on_live_zero(", "inner"),
                ("class/mod.rs:", "pub fn bin_stats(", "inner"),
                ("class/mod.rs:", "pub fn lowest_partial(", "inner"),
                ("class/mod.rs:", "pub fn is_partial(", "inner"),
                ("class/mod.rs:", "pub(crate) fn set_membership(", "inner"),
                ("class/decay.rs:", "pub fn empty_bytes(", "inner"),
                ("class/decay.rs:", "pub fn purge_empty(", "inner"),
                ("class/decay.rs:", "fn purge_curve(", "inner"),
            ],
        ),
        // `class::sets`'s `LockSpec` impl for `[Lazy; SET_GROUPS]`.
        (
            "class/sets.rs:SetTable.tables",
            &[
                ("class/sets.rs:", "fn ensure(", "tables"),
                ("class/sets.rs:", "pub fn committed_ranges(", "tables"),
            ],
        ),
        // `class::table::TableInner`'s `LockSpec` impl.
        (
            "class/table.rs:SlabTable.inner",
            &[
                (
                    "class/table.rs:",
                    "pub unsafe fn mirror_watermark(",
                    "inner",
                ),
                ("class/table.rs:", "pub fn guards(", "inner"),
                ("class/table.rs:", "pub(super) fn take_group(", "inner"),
                ("class/table.rs:", "pub(super) fn commit_run(", "inner"),
                ("class/table.rs:", "pub(super) fn commit_slab(", "inner"),
                (
                    "class/table.rs:",
                    "pub(crate) fn corrupt_owner_word(",
                    "inner",
                ),
            ],
        ),
        // `heap::tcaches`'s `LockSpec` impl for `()`.
        (
            "heap/tcaches.rs:Tcaches.lock",
            &[
                ("heap/tcaches.rs:", "pub fn create(", "lock"),
                ("heap/tcaches.rs:", "pub fn destroy(", "lock"),
            ],
        ),
        // `stats::Snapshot`'s `LockSpec` impl.
        (
            "heap/shared.rs:Shared.stats",
            &[
                ("stats.rs:", "fn collect_into(", "stats"),
                ("stats.rs:", "pub fn snapshot(", "stats"),
                ("stats.rs:", "pub fn arena_stats_all_into(", "stats"),
            ],
        ),
        // `huge::Inner`'s `LockSpec` impl.
        (
            "huge/mod.rs:Huge.inner",
            &[
                ("huge/mod.rs:", "fn alloc_aligned(", "inner"),
                ("huge/free.rs:", "pub fn free_run(", "inner"),
                ("huge/free.rs:", "pub fn usable_size(", "inner"),
                ("huge/free.rs:", "pub fn lookup(", "inner"),
                ("huge/decay.rs:", "pub fn purge_empty(", "inner"),
                ("huge/decay.rs:", "fn purge_curve(", "inner"),
                ("huge/decay.rs:", "pub fn empty_bytes(", "inner"),
                ("huge/decay.rs:", "pub fn stats(", "inner"),
            ],
        ),
        // `meta::Inner`'s `LockSpec` impl.
        (
            "meta.rs:Meta.inner",
            &[
                ("meta.rs:", "pub fn alloc(", "inner"),
                ("meta.rs:", "pub fn alloc_raw(", "inner"),
                ("meta.rs:", "pub fn alloc_zeroed(", "inner"),
                ("meta.rs:", "pub fn free(", "inner"),
                ("meta.rs:", "pub fn reserve(", "inner"),
                ("meta.rs:", "pub fn reserve_table(", "inner"),
                ("meta.rs:", "pub fn commit(", "inner"),
                ("meta.rs:", "pub fn reserve_committed_raw(", "inner"),
                ("meta.rs:", "pub fn stats(", "inner"),
                ("meta.rs:", "pub fn set_eager_for_test(", "inner"),
                ("meta.rs:", "pub fn eager_allowed(", "inner"),
                ("meta.rs:", "pub fn base(", "inner"),
                ("meta.rs:", "pub fn top(", "inner"),
            ],
        ),
        // `purge::background::State`'s `LockSpec` impl.
        (
            "purge/background.rs:static.STATE",
            &[
                ("purge/background.rs:", "pub fn request(", "state()"),
                ("purge/background.rs:", "pub fn start(", "state()"),
                (
                    "purge/background.rs:",
                    "pub fn present_or_creating(",
                    "state()",
                ),
                ("purge/background.rs:", "pub fn start_requested(", "state()"),
                ("purge/background.rs:", "pub fn auto_tick(", "state()"),
                ("purge/background.rs:", "extern \"C\" fn main(", "state()"),
                ("purge/background.rs:", "pub fn join_exited(", "STATE"),
                ("purge/background.rs:", "pub fn stack(", "STATE"),
                ("purge/background.rs:", "pub fn thread(", "STATE"),
            ],
        ),
    ];
    // The raw acquisitions: (file, function).
    const RAW: &[(&str, &str)] = &[
        ("api/mod.rs:", "extern \"C\" fn fork_prepare("),
        ("api/mod.rs:", "fn fork_release("),
        ("arena.rs:", "pub(crate) fn lock_held("),
        ("arena.rs:", "pub(crate) fn unlock_held("),
        ("purge/background.rs:", "pub fn fork_lock("),
        ("purge/background.rs:", "pub fn fork_unlock("),
    ];
    // Spelled in parts, so that this test's own lines match nothing
    // (they are skipped anyway, as test code).
    let guards = [
        concat!(".lock(", "Tracked"),
        concat!(".try_", "lock("),
        concat!(".lock_", "t()"),
    ];
    let raw = [concat!(".lock_", "held"), concat!(".unlock_", "held")];
    let writes = concat!(".value_", "held_mut(");
    // The field, static or accessor call (`state()`) a call is made on.
    let receiver = |before: &str| {
        let (stem, call) = match before.strip_suffix("()") {
            Some(s) => (s, "()"),
            None => (before, ""),
        };
        let start = stem
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |k| k + 1);
        std::format!("{}{call}", &stem[start..])
    };
    let mut holders = Vec::new();
    let mut raws = Vec::new();
    let mut sets = Vec::new();
    for (rel, text) in crate::testutil::shipped_sources() {
        if rel == "lock.rs" {
            continue;
        }
        let mut sig = String::new();
        let mut prev = String::new();
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if crate::testutil::fn_item(t) {
                sig = t.to_string();
            }
            let at = std::format!("{rel}:{}", i + 1);
            // A guard call whose arguments wrap to the next line.
            let wrapped = t.strip_suffix(concat!(".lock", "(")).map(str::len);
            for k in guards.iter().filter_map(|g| t.find(g)).chain(wrapped) {
                // A call chained from the line above (`self.inner`
                // then `.lock(..)`) names its receiver there.
                let before = if k == 0 {
                    prev.clone()
                } else {
                    t[..k].to_string()
                };
                holders.push((rel.clone(), sig.clone(), receiver(&before), at.clone()));
            }
            if raw.iter().any(|r| t.contains(r)) {
                raws.push((rel.clone(), sig.clone(), at.clone()));
            }
            if t.contains(writes) {
                sets.push((rel.clone(), sig.clone(), at.clone()));
            }
            if !t.is_empty() {
                prev = t.to_string();
            }
        }
    }
    let matches = |rel: &str, sig: &str, file: &str, f: &str| {
        std::format!("{rel}:").starts_with(file) && sig.starts_with(f)
    };
    let listed = || {
        HOLDERS
            .iter()
            .flat_map(|(key, rows)| rows.iter().map(move |row| (*key, row)))
    };
    for (rel, sig, recv, at) in &holders {
        assert!(
            listed().any(|(_, (file, f, r))| matches(rel, sig, file, f) && recv == r),
            "{at}: `{sig}` takes the guard of `{recv}`, and no lock's holder list has it. \
             Name it in that lock's `lock_inv` axiom doc (or `LockSpec` impl doc), with how it leaves the invariant \
             true (or that it only reads), and add it to this test's table, in one change."
        );
    }
    for (key, (file, f, r)) in listed() {
        assert!(
            holders
                .iter()
                .any(|(rel, sig, recv, _)| matches(rel, sig, file, f) && recv == r),
            "{key}: {file} `{f}` no longer takes the guard of `{r}`: drop it from this \
             table and from the lock's `lock_inv` axiom or `LockSpec` impl doc"
        );
    }
    let mut keys: Vec<String> = HOLDERS.iter().map(|h| h.0.to_string()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(
        keys,
        lock_inventory(),
        "every lock in src/ has its holders listed here, from its `lock_inv` axiom's or `LockSpec` impl's doc"
    );
    for (rel, sig, at) in &raws {
        assert!(
            RAW.iter().any(|(file, f)| matches(rel, sig, file, f)),
            "{at}: raw lock access in `{sig}`, outside the fork handlers"
        );
    }
    for (file, f) in RAW {
        assert!(
            raws.iter().any(|(rel, sig, _)| matches(rel, sig, file, f)),
            "{file} `{f}` no longer takes a lock raw: drop it from this list"
        );
    }
    assert!(
        sets.len() == 1
            && sets[0].0 == "purge/background.rs"
            && sets[0].1.starts_with("pub fn fork_child("),
        "value_held_mut is called once, in the background thread's fork_child: {sets:?}"
    );
}

/// The handlers take every lock and release every lock, on both fork
/// sides. The checks run in a fork child: in this process another test
/// can take a heap lock (a huge allocation, a refill) the moment the
/// handlers release it, so "every lock free" is only exact where no
/// other thread runs. The real `fork` runs the handlers through
/// `pthread_atfork`, so the child starts from `fork_child`'s release.
#[test]
fn fork_handlers_take_and_release_every_lock() {
    let _serial = crate::testutil::global_state();
    let s = shared();
    // A second arena so the handlers have more than arena 0 to cover.
    let extra = s
        .arenas
        .create(
            s,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        )
        .unwrap();
    let arenas = [0, extra];
    crate::testutil::in_fork_child("fork handlers", 60_000, || {
        assert_locks(s, &arenas, false);
        assert!(!purge::background::present());
        assert!(!purge::background::requested());
        fork_prepare();
        assert_locks(s, &arenas, true);
        fork_parent();
        assert_locks(s, &arenas, false);
        fork_prepare();
        assert_locks(s, &arenas, true);
        fork_child();
        assert_locks(s, &arenas, false);
        assert!(!purge::background::present());
        assert!(!purge::background::requested());
    });
}

/// The background thread starts on request, is visible under
/// `/proc/self/task` with its name, purges without any slow-path
/// tick, is gone from the bookkeeping in a fork child while the
/// parent's keeps running, and stops when the request is withdrawn.
#[test]
fn background_thread_starts_purges_and_is_absent_after_fork() {
    use std::string::ToString;
    fn tasks() -> std::vec::Vec<std::string::String> {
        let mut names = std::vec::Vec::new();
        for e in std::fs::read_dir("/proc/self/task").unwrap() {
            let p = e.unwrap().path().join("comm");
            names.push(
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            );
        }
        names
    }
    let _serial = crate::testutil::global_state();
    let s = shared();
    assert!(!tasks().iter().any(|n| n == "challoc-bg"));
    purge::background::request(
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(
        purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new()
        ),
        "background thread did not start"
    );
    assert!(purge::background::present());
    assert!(
        purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new()
        ),
        "second start is idempotent"
    );
    // The name is set by the thread itself; give it a moment.
    let mut seen = false;
    for _ in 0..200 {
        if tasks().iter().any(|n| n == "challoc-bg") {
            seen = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(seen, "no task named challoc-bg: {:?}", tasks());

    // A 1 ms decay: free a huge run, wait, and the thread purges it
    // with nobody else touching the allocator.
    let old_ns = s.decay_ns();
    s.set_decay_ms(1, Tracked::assume_new(), Tracked::assume_new());
    let (p, _, _) = s.alloc_huge(
        8 << 20,
        4096,
        0,
        false,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    // SAFETY: `p` is a live 8 MiB huge allocation.
    unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
    let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
    s.free_huge(
        page,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    // No check that the run is still `Empty` here: with a 1 ms decay
    // the thread can purge it before this thread runs again (seen on
    // a loaded Docker VM); the loop below is the assertion.
    let mut purged = false;
    for _ in 0..400 {
        if s.huge.oldest_empty() == 0 && s.huge.stats(Tracked::assume_new()).empty == 0 {
            purged = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        purged,
        "background thread did not purge the freed run: {:?}",
        s.huge.stats(Tracked::assume_new())
    );
    s.set_decay_ms(
        (old_ns / 1_000_000) as i64,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );

    // A real fork: the child handler forgets the thread and the
    // request in the child, and leaves this process's thread alone.
    // The child checks with atomics and the state lock only and
    // answers through its exit status.
    assert!(purge::background::thread().is_some(), "no thread record");
    // The child keeps the stack reservation, for its own thread to
    // reuse: a fresh one per fork generation would leak.
    let stack = purge::background::stack();
    assert!(stack != 0, "no stack reservation");
    // SAFETY: the test binary is multithreaded; the child uses only atomics
    // and challoc's state lock, which its fork handlers keep consistent,
    // and leaves through `_exit`.
    let pid = unsafe { ::libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let bad = [
            purge::background::present(),
            purge::background::requested(),
            purge::background::is_locked(),
            purge::background::thread().is_some(),
            purge::background::stack() != stack,
        ];
        let code = bad
            .iter()
            .enumerate()
            .fold(0, |m, (i, &b)| m | ((b as i32) << i));
        // SAFETY: `_exit` has no preconditions; the child must not unwind.
        unsafe { ::libc::_exit(code) };
    }
    let mut status = 0;
    // SAFETY: `pid` is this process's child; `status` is a local.
    assert_eq!(unsafe { ::libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        ::libc::WIFEXITED(status),
        "fork child did not exit: status {status:#x}"
    );
    assert_eq!(
        ::libc::WEXITSTATUS(status),
        0,
        "fork child still sees (bit 0) present, (1) requested, (2) the state lock held, (3) a thread record, or (4) a stack other than the parent's"
    );
    assert!(
        purge::background::present() && purge::background::requested(),
        "the fork stopped the parent's thread"
    );

    // Withdraw the request: the kick wakes the thread and it exits
    // there; the exit is asynchronous, so wait for it, bounded, and
    // join it.
    purge::background::request(
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let mut stopped = false;
    for _ in 0..400 {
        if !purge::background::present() {
            stopped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        stopped,
        "background thread did not stop at the kick: {:?}",
        tasks()
    );
    assert!(purge::background::join_exited(), "no exited thread to join");
    assert!(purge::background::thread().is_none());
    // Joined, the task leaves /proc/self/task as the kernel reaps it.
    let mut gone = false;
    for _ in 0..400 {
        if !tasks().iter().any(|n| n == "challoc-bg") {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        gone,
        "background thread still running after the join: {:?}",
        tasks()
    );
}

/// The lazy start's rule (design 8.1): the burst threshold starts at
/// the first look; the floor only once every look for the hold has
/// seen it; a look under the floor ends the stretch.
#[test]
fn lazy_start_rule() {
    use core::sync::atomic::AtomicU64;
    use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS, auto_due};
    let since = AtomicU64::new(0);
    let t = 5_000_000_000u64;
    assert!(auto_due(AUTO_BURST, t, &since), "burst at the first look");
    assert!(!auto_due(AUTO_FLOOR - 1, t, &since));
    assert!(
        !auto_due(AUTO_FLOOR - 1, t + 10 * AUTO_HOLD_NS, &since),
        "under the floor never"
    );
    assert!(
        !auto_due(AUTO_FLOOR, t, &since),
        "the floor starts a stretch"
    );
    assert!(!auto_due(AUTO_BURST - 1, t + AUTO_HOLD_NS / 2, &since));
    assert!(
        auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since),
        "held for the hold"
    );
    // A dip under the floor restarts the stretch.
    since.store(0, core::sync::atomic::Ordering::Relaxed);
    assert!(!auto_due(AUTO_FLOOR, t, &since));
    assert!(!auto_due(AUTO_FLOOR / 2, t + AUTO_HOLD_NS / 2, &since));
    assert!(!auto_due(AUTO_FLOOR, t + AUTO_HOLD_NS, &since));
    assert!(!auto_due(
        AUTO_FLOOR,
        t + AUTO_HOLD_NS + AUTO_HOLD_NS / 2,
        &since
    ));
    assert!(auto_due(AUTO_FLOOR, t + 2 * AUTO_HOLD_NS, &since));
}

/// The lazy start end to end on one allocator, on injected tick
/// times: nothing dirty starts nothing; 8 MiB dirty (over the floor,
/// under the burst threshold) starts nothing at the first look or
/// half a hold later; once the hold has passed a free's tick only
/// requests the thread and the next allocation's tick starts it;
/// the start leaves the lazy start armed; an explicit `request`
/// disarms it.
#[test]
// The constant check pins the test size between the floor and the burst.
#[allow(clippy::assertions_on_constants)]
fn lazy_start_waits_for_dirty_memory() {
    use purge::background::{AUTO_BURST, AUTO_FLOOR, AUTO_HOLD_NS};
    let _serial = crate::testutil::global_state();
    let s = crate::testutil::shared();
    assert!(!purge::background::present());
    // A window long enough that nothing decays between the looks.
    let old_ns = s.decay_ns();
    s.set_decay_ms(1_000_000, Tracked::assume_new(), Tracked::assume_new());
    assert!((8u64 << 20) >= AUTO_FLOOR && (8u64 << 20) < AUTO_BURST);
    purge::background::set_auto(Some(s));
    let t0 = crate::sys::now_coarse_ns();
    purge::on_tick(
        s,
        t0,
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    purge::on_tick(
        s,
        t0 + 2 * AUTO_HOLD_NS,
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let clean = purge::background::present();
    let (p, _, _) = s.alloc_huge(
        8 << 20,
        4096,
        0,
        false,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    // SAFETY: `p` is a live 8 MiB huge allocation.
    unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
    s.free_huge(
        (p as usize - s.regions.huge_base) >> s.regions.lg_page,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    let t1 = t0 + 3 * AUTO_HOLD_NS;
    purge::on_tick(
        s,
        t1,
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let first = purge::background::present();
    purge::on_tick(
        s,
        t1 + AUTO_HOLD_NS / 2,
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let half = purge::background::present();
    // A free's tick past the hold requests the thread but never
    // creates it (glibc frees under locks `pthread_create` takes);
    // the next allocation's tick does.
    purge::on_tick(
        s,
        t1 + AUTO_HOLD_NS + 1,
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let freed = purge::background::present();
    let pending = purge::background::requested();
    purge::on_tick(
        s,
        t1 + AUTO_HOLD_NS + 2,
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let held = purge::background::present();
    let armed = purge::background::auto();
    purge::background::request(
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let disarmed = !purge::background::auto();
    for _ in 0..400 {
        if !purge::background::present() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let stopped = !purge::background::present();
    if stopped {
        purge::background::join_exited();
    }
    purge::background::set_auto(None);
    s.huge
        .purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
    s.set_decay_ms(
        (old_ns / 1_000_000) as i64,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );

    assert!(!clean, "nothing dirty started the thread");
    assert!(!first, "8 MiB started the thread at the first look");
    assert!(!half, "8 MiB started the thread before the hold");
    assert!(!freed, "a free's tick created the thread");
    assert!(pending, "a free's tick past the hold did not request it");
    assert!(held, "8 MiB held past the hold did not start the thread");
    assert!(armed, "a successful start disarmed the lazy start");
    assert!(disarmed, "request(false) left the lazy start armed");
    assert!(stopped, "background thread did not stop");
}

/// A first `Empty` that lands while `start` is publishing the thread
/// is purged: the thread has already scanned and gone to sleep with
/// no deadline, and the free's `wake` does nothing because the
/// thread is not yet present, so `start` must kick it once it is.
/// Run alone, it fails without that kick; in the full suite another
/// test's first `Empty` can wake the thread instead and hide it.
#[test]
fn background_thread_purges_an_empty_from_its_startup() {
    use core::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;
    let _serial = crate::testutil::global_state();
    // A private allocator: the process-wide one is shared with the
    // tests that do not take `global_state`, whose huge frees could
    // leave a run `Empty` before the hook's.
    let s = crate::testutil::shared();
    assert!(!purge::background::present());
    let old_ns = s.decay_ns();
    s.set_decay_ms(1, Tracked::assume_new(), Tracked::assume_new());
    let (p, _, _) = s.alloc_huge(
        8 << 20,
        4096,
        0,
        false,
        vstd::prelude::Tracked::assume_new(),
        vstd::prelude::Tracked::assume_new(),
    );
    assert!(!p.is_null());
    // SAFETY: `p` is a live 8 MiB huge allocation.
    unsafe { core::ptr::write_bytes(p, 1, 8 << 20) };
    let page = (p as usize - s.regions.huge_base) >> s.regions.lg_page;
    // Nothing else `Empty` in the huge allocator, so the free in the
    // hook is its first.
    s.huge
        .purge_empty(u64::MAX, Tracked::assume_new(), Tracked::assume_new());
    // 0: the hook did not run; 1: the thread never slept without a
    // deadline; 2: the free was not the first `Empty`; 3: ready.
    let hook_state = Arc::new(AtomicU8::new(0));
    let hook_seen = Arc::clone(&hook_state);
    let waits = purge::background::UNTIMED_WAITS.load(Ordering::SeqCst);
    *purge::background::BEFORE_PUBLISH.lock().unwrap() = Some(std::boxed::Box::new(move || {
        // No panics here: unwinding out of `start` would leave the
        // state marked as creating for every later test.
        let mut asleep = false;
        for _ in 0..1000 {
            if purge::background::UNTIMED_WAITS.load(Ordering::SeqCst) != waits {
                asleep = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !asleep {
            hook_seen.store(1, Ordering::SeqCst);
            return;
        }
        let none_before = s.huge.oldest_empty() == 0;
        s.free_huge(
            page,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
        let first = none_before && s.huge.oldest_empty() != 0;
        hook_seen.store(if first { 3 } else { 2 }, Ordering::SeqCst);
    }));
    purge::background::request(
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let started = purge::background::start(
        s,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let mut purged = false;
    if started && hook_state.load(Ordering::SeqCst) == 3 {
        for _ in 0..400 {
            if s.huge.oldest_empty() == 0 && s.huge.stats(Tracked::assume_new()).empty == 0 {
                purged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // Clean up before asserting, so a failure here leaves no thread
    // or hook behind for the other background tests.
    purge::background::BEFORE_PUBLISH.lock().unwrap().take();
    if hook_state.load(Ordering::SeqCst) < 2 {
        s.free_huge(
            page,
            vstd::prelude::Tracked::assume_new(),
            vstd::prelude::Tracked::assume_new(),
        );
    }
    s.set_decay_ms(
        (old_ns / 1_000_000) as i64,
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    purge::background::request(
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    for _ in 0..400 {
        if !purge::background::present() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let stopped = !purge::background::present();
    if stopped {
        purge::background::join_exited();
    }
    assert!(started, "background thread did not start");
    assert_eq!(
        hook_state.load(Ordering::SeqCst),
        3,
        "hook: 0 did not run, 1 the thread never slept without a deadline, \
         2 the free was not the first Empty"
    );
    assert!(
        purged,
        "background thread slept through a first Empty from its startup: {:?}",
        s.huge.stats(Tracked::assume_new())
    );
    assert!(stopped, "background thread did not stop");
}

#[test]
fn background_thread_that_exits_before_its_start_publishes_is_not_present() {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let _serial = crate::testutil::global_state();
    let s = crate::testutil::shared();
    assert!(!purge::background::present());
    // The request is withdrawn while `start` is between creating the
    // thread and publishing it, and the new thread sees that and exits
    // first: `start` must not then publish a thread that is gone.
    let exited = Arc::new(AtomicBool::new(false));
    let exited_seen = Arc::clone(&exited);
    let exits = purge::background::EXITS.load(Ordering::SeqCst);
    *purge::background::BEFORE_PUBLISH.lock().unwrap() = Some(std::boxed::Box::new(move || {
        // No panics here: unwinding out of `start` would leave the
        // state marked as creating for every later test.
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        for _ in 0..1000 {
            if purge::background::EXITS.load(Ordering::SeqCst) != exits {
                exited_seen.store(true, Ordering::SeqCst);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }));
    purge::background::request(
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    purge::background::start(
        s,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    purge::background::BEFORE_PUBLISH.lock().unwrap().take();
    let present_after_exit = purge::background::present();
    // Asked again, the thread must come back: a stale `present` would
    // make `start` return at once with no thread running.
    purge::background::request(
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let restarted = purge::background::start(
        s,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let exits_before_stop = purge::background::EXITS.load(Ordering::SeqCst);
    purge::background::request(
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    let mut stopped = false;
    for _ in 0..400 {
        if purge::background::EXITS.load(Ordering::SeqCst) != exits_before_stop
            && !purge::background::present()
        {
            stopped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if stopped {
        purge::background::join_exited();
    }
    assert!(
        exited.load(Ordering::SeqCst),
        "the new thread did not exit in the hook"
    );
    assert!(
        !present_after_exit,
        "start published a thread that had already exited"
    );
    assert!(restarted, "background thread did not restart");
    assert!(stopped, "restarted background thread did not stop");
}

/// One stack for the life of the process (design 8.1): a
/// stop/start cycle joins the exited thread and hands the same
/// reservation to the next one, so toggling `background_thread` does
/// not grow the metadata region.
#[test]
fn background_stack_is_reused_across_restarts() {
    fn wait_absent() {
        for _ in 0..400 {
            if !purge::background::present() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("background thread did not stop");
    }
    let _serial = crate::testutil::global_state();
    let s = shared();
    assert!(!purge::background::present());
    purge::background::request(
        true,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    assert!(
        purge::background::start(
            s,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new()
        ),
        "background thread did not start"
    );
    let stack = purge::background::stack();
    assert_ne!(stack, 0);
    for cycle in 0..8 {
        purge::background::request(
            false,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        wait_absent();
        purge::background::request(
            true,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(
            purge::background::start(
                s,
                Tracked::assume_new(),
                Tracked::assume_new(),
                Tracked::assume_new()
            ),
            "restart {cycle} failed"
        );
        assert!(purge::background::present());
        assert_eq!(
            purge::background::stack(),
            stack,
            "restart {cycle} took a new stack"
        );
    }
    purge::background::request(
        false,
        Tracked::assume_new(),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
    wait_absent();
    assert!(purge::background::join_exited());
    assert!(purge::background::thread().is_none());
}

/// The calling thread's signal mask, as the kernel's 64-bit word.
fn current_mask() -> u64 {
    // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
    let mut set: ::libc::sigset_t = unsafe { core::mem::zeroed() };
    let rc =
        // SAFETY: a null new set only reads the mask into `set`, a local.
        unsafe { ::libc::pthread_sigmask(::libc::SIG_BLOCK, core::ptr::null(), &mut set) };
    assert_eq!(rc, 0);
    // The kernel mask is the first 8 bytes of glibc's 128-byte set.
    // SAFETY: `sigset_t` is 128 bytes, so its first 8 are readable.
    unsafe { core::ptr::read_unaligned(&set as *const _ as *const u64) }
}

/// glibc's SIGCANCEL (32) and SIGSETXID (33), never blocked.
const GLIBC_INTERNAL: u64 = (1 << 31) | (1 << 32);

/// `mask` without `SIGKILL` and `SIGSTOP`, which the kernel never blocks,
/// and glibc's internal two, which the handlers leave unblocked.
fn blockable(mask: u64) -> u64 {
    const KILL_STOP: u64 = (1 << (::libc::SIGKILL - 1)) | (1 << (::libc::SIGSTOP - 1));
    mask & !KILL_STOP & !GLIBC_INTERNAL
}

/// The handlers block every signal but glibc's internal two while the
/// locks are held and restore the caller's mask (whatever it was)
/// afterwards, on both fork sides.
#[test]
fn fork_handlers_block_signals_while_locked() {
    let _serial = crate::testutil::global_state();
    let all = blockable(!0);
    // Start from a distinctive mask so "restored" is not "left blocked".
    // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
    let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
    // SAFETY: as above.
    let mut before: ::libc::sigset_t = unsafe { core::mem::zeroed() };
    // SAFETY: `want` and `before` are local sets.
    unsafe {
        ::libc::sigemptyset(&mut want);
        ::libc::sigaddset(&mut want, ::libc::SIGUSR2);
        ::libc::sigaddset(&mut want, ::libc::SIGWINCH);
        assert_eq!(
            ::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, &mut before),
            0
        );
    }
    let start = current_mask();
    assert_eq!(
        start,
        (1 << (::libc::SIGUSR2 - 1)) | (1 << (::libc::SIGWINCH - 1))
    );

    fork_prepare();
    assert_eq!(
        blockable(current_mask()),
        all,
        "prepare did not block every signal"
    );
    assert_eq!(
        current_mask() & GLIBC_INTERNAL,
        0,
        "prepare blocked SIGCANCEL or SIGSETXID"
    );
    fork_parent();
    assert_eq!(
        current_mask(),
        start,
        "parent handler did not restore the mask"
    );

    fork_prepare();
    assert_eq!(blockable(current_mask()), all);
    fork_child();
    assert_eq!(
        current_mask(),
        start,
        "child handler did not restore the mask"
    );

    unsafe { ::libc::pthread_sigmask(::libc::SIG_SETMASK, &before, core::ptr::null_mut()) };
}

/// Two threads with different masks run the prepare and parent
/// handlers concurrently (glibc serialises them; the shared
/// `FORK_SIGMASK` slot must not need it): each gets its own mask back,
/// because a prepare saves only once it holds every lock, and the
/// release reads before its first unlock.
#[test]
fn concurrent_fork_handlers_restore_each_threads_mask() {
    let _serial = crate::testutil::global_state();
    let all = blockable(!0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = [::libc::SIGUSR1, ::libc::SIGUSR2]
        .into_iter()
        .map(|signo| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                // SAFETY: an all-zero `sigset_t` is a valid (empty) set.
                let mut want: ::libc::sigset_t = unsafe { core::mem::zeroed() };
                // SAFETY: `want` is a local set; the new mask is this
                // test thread's own.
                let rc = unsafe {
                    ::libc::sigemptyset(&mut want);
                    ::libc::sigaddset(&mut want, signo);
                    ::libc::pthread_sigmask(::libc::SIG_SETMASK, &want, core::ptr::null_mut())
                };
                let start = current_mask();
                // Past the barrier before any assert, so a failure here
                // cannot leave the other thread waiting.
                barrier.wait();
                assert_eq!(rc, 0);
                assert_eq!(start, 1 << (signo - 1));
                for round in 0..200 {
                    fork_prepare();
                    // Asserted after the release: a panic with every
                    // lock held would hang the other thread.
                    let during = blockable(current_mask());
                    fork_parent();
                    assert_eq!(
                        during, all,
                        "round {round}: prepare left a signal unblocked"
                    );
                    assert_eq!(
                        current_mask(),
                        start,
                        "round {round}: another thread's mask came back"
                    );
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
}

/// The C export modules the export-boundary checks below cover.
const EXPORT_FILES: [&str; 3] = ["api/libc.rs", "api/cxx.rs", "api/jemalloc.rs"];

/// Whether `t` (a trimmed line) is the first line of a C export: an
/// `extern "C"` function item, or one `fn name / je_name(..)` entry of
/// `verified_export_pair!`, whose one body serves both names.
fn export_head(t: &str) -> bool {
    let pair = t.starts_with("fn ") && t.split('(').next().is_some_and(|h| h.contains(" / "));
    !t.starts_with("//") && (pair || (t.contains("extern \"C") && t.contains(" fn ")))
}

/// The exports in `text` (a source file's `mod exports`), as each
/// one's first line and its body's statements: trimmed, with comments
/// and blank lines dropped, a call spread over several lines joined
/// into one, and the body's closing brace left out.
fn export_bodies(text: &str) -> Vec<(String, Vec<String>)> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i];
        if !export_head(t) {
            i += 1;
            continue;
        }
        let head = String::from(t);
        // The body opens at the end of the first line, the head's own
        // included, that ends in ` {` (`{` alone, or `] {` after the
        // pair's `ensures [..]`).
        while lines[i] != "{" && !lines[i].ends_with(" {") {
            i += 1;
        }
        i += 1;
        let mut depth = 1i32;
        let mut body: Vec<String> = Vec::new();
        let mut open_call: Option<String> = None;
        while depth > 0 {
            let l = lines[i];
            i += 1;
            depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
            if depth == 0 || l.is_empty() || l.starts_with("//") {
                continue;
            }
            let joined = match open_call.take() {
                Some(prev) => prev + l,
                None => String::from(l),
            };
            if joined.matches('(').count() > joined.matches(')').count() {
                open_call = Some(joined);
            } else {
                body.push(joined);
            }
        }
        out.push((head, body));
    }
    out
}

/// The lending of an export's `errno` token, spelled in parts so that
/// this module's own lines are not lend sites.
fn lend_line() -> String {
    std::format!(
        "let Tracked(mut errno) = crate::vspec::errno::{}();",
        "lend"
    )
}

/// The `failed` conditions an export may pass to
/// `axiom_errno_at_return` (`vspec::errno`'s doc), one per kind of
/// export: a null result, except a `realloc` (or `reallocarray`) of a
/// block to size 0, and `posix_memalign`'s `ENOMEM`.
const FAILED: [&str; 4] = [
    "r@.addr == 0",
    "r@.addr == 0 && !(p@.addr != 0 && size == 0)",
    "r@.addr == 0 && !(p@.addr != 0 && n * m == 0)",
    "r == ENOMEM",
];

/// The [`FAILED`] condition of the export whose head is `head`: the
/// `posix_memalign` pair's, `reallocarray`'s, the `realloc` forms'
/// (the pair and `challoc_realloc_usable`), or a null result.
fn failed_for(head: &str) -> &'static str {
    if head.contains("fn posix_memalign") {
        FAILED[3]
    } else if head.contains("fn reallocarray") {
        FAILED[2]
    } else if head.contains("realloc") {
        FAILED[1]
    } else {
        FAILED[0]
    }
}

/// The statement that hands a token back, with condition `failed`.
fn axiom_line(axiom: &str, failed: &str) -> String {
    std::format!("crate::vspec::errno::{axiom}(errno, {failed});")
}

/// The out-pointer tokens an export body may mint (`axiom_c_out`,
/// directly or through `memptr_out`), each with the statement that
/// hands it back (`axiom_out_at_return`), spelled in parts so that this
/// module's own lines are neither mint nor hand-back sites.
fn out_tokens() -> [(String, String); 2] {
    let back = std::format!("{}{}", "axiom_out_", "at_return");
    [
        (
            std::format!(
                "let Tracked(mut oout) = {}(out as *mut *mut u8);",
                "memptr_out"
            ),
            std::format!("{back}(oout, out as *mut *mut u8);"),
        ),
        (
            std::format!("let tracked mut uout = {}(usable, true);", "axiom_c_out"),
            std::format!("{back}(uout, usable);"),
        ),
    ]
}

/// Checks one export's body against the one shape an export may have,
/// and returns whether it hands its `errno` token back
/// (`axiom_errno_at_return`, named in `axiom`) and whether it hands an
/// out-pointer token back ([`out_tokens`]). The body sets up its call
/// (the root grants, the token it lends, the C caller's out-pointer
/// token), then calls its Rust entry point once, through `super::`,
/// and returns what the call returned: as the body's tail, or bound to
/// `r`, followed by the axiom on its `errno` token and the hand-back of
/// the out-pointer token it minted, then `r`.
fn check_export_body(at: &str, body: &[String], axiom: &str) -> (bool, bool) {
    // The root grants spelled in parts, so that these lines are not
    // grant sites (`testutil::grant_sites`).
    let grant = |g: &str| std::format!("crate::sys::grant_{g}();");
    let outs = out_tokens();
    let setup = [
        String::from("proof {"),
        grant("syscalls"),
        grant("aborts"),
        String::from("}"),
        lend_line(),
        outs[0].0.clone(),
        outs[1].0.clone(),
        String::from("let tracked mut left = Ghost(crate::purge::Left::none());"),
    ];
    let call = body
        .iter()
        .position(|s| s.starts_with("super::") || s.starts_with("let r = super::"))
        .unwrap_or_else(|| panic!("{at}: no `super::` call that is the result or binds `r`"));
    for s in &body[..call] {
        assert!(setup.contains(s), "{at}: `{s}` before the call");
    }
    let lends = body[..call].iter().filter(|s| **s == lend_line()).count();
    assert!(
        lends <= 1,
        "{at}: got = {lends} lent tokens, want = at most 1"
    );
    let minted: Vec<&String> = outs
        .iter()
        .filter(|(mint, _)| body[..call].contains(mint))
        .map(|(_, back)| back)
        .collect();
    assert!(
        minted.len() <= 1
            && outs
                .iter()
                .all(|(mint, _)| body[..call].iter().filter(|s| *s == mint).count() <= 1),
        "{at}: more than 1 out-pointer token minted, want = at most 1"
    );
    let rest: Vec<&str> = body[call + 1..].iter().map(String::as_str).collect();
    let s = &body[call];
    if s.starts_with("let r = ") {
        // One statement: the only `;` ends it.
        assert_eq!(s.find(';'), Some(s.len() - 1), "{at}: `{s}`");
        let back = axiom_line(axiom, failed_for(at));
        let mut want = Vec::from(["proof {", back.as_str()]);
        want.extend(minted.iter().map(|b| b.as_str()));
        want.extend(["}", "r"]);
        assert_eq!(
            rest, want,
            "{at}: after `{s}`: got = {rest:?}, want = the axiom on the token with this export's `failed` condition, the hand-back of its out-pointer token, then `r`"
        );
        assert_eq!(lends, 1, "{at}: hands back a token it did not lend");
        (true, !minted.is_empty())
    } else {
        assert!(!s.contains(';'), "{at}: the call `{s}` is not the result");
        assert!(
            rest.is_empty(),
            "{at}: got = {rest:?} after the call, want = nothing"
        );
        assert!(
            minted.is_empty(),
            "{at}: mints an out-pointer token it does not hand back"
        );
        (false, false)
    }
}

/// Every C export of `api::libc`, `api::cxx` and `api::jemalloc`
/// returns its Rust entry point's result unchanged
/// (`check_export_body`), so a nullable export cannot drop a block and
/// return null even where its `ensures` cannot tell the two apart (the
/// entry point states nothing about `errno`: `operator new`'s
/// `nothrow_t` forms, `mallocx`, `rallocx`), and cannot write or read
/// its `errno` token around the call. And the `errno` premises of
/// `vspec::errno::axiom_errno_at_return` hold: the axiom is named only
/// in those exports' bodies, right after the call, on the token the
/// body lent, with that export's `failed` condition from [`FAILED`];
/// tokens are lent only by exports, one each; an export body mints at
/// most one out-pointer token (where they are minted:
/// [`c_call_tokens_only_in_export_bodies`]), and it goes back through
/// `axiom_out_at_return`, named only there, right after the call; and
/// no code names an `exports` module, so nothing verified calls an
/// export.
#[test]
fn exports_return_their_callees_result() {
    // Spelled in parts, so that this test's own lines match nothing.
    let axiom = std::format!("{}{}", "axiom_errno_", "at_return");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut handed_back = 0;
    let mut outs_back = 0;
    for file in EXPORT_FILES {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        let start = text
            .find("\nmod exports {")
            .unwrap_or_else(|| panic!("{file}: no `mod exports`"));
        let end = text[start..]
            .find("\n#[cfg(test)]")
            .map_or(text.len(), |e| start + e);
        let region = &text[start..end];
        let exports = export_bodies(region);
        // Each `extern "C"` item carries one symbol attribute, so the
        // parser found every export the module has.
        let attrs = region
            .lines()
            .map(str::trim)
            .filter(|t| {
                t.starts_with("#[unsafe(no_mangle)]") || t.starts_with("#[unsafe(export_name")
            })
            .count();
        let items = exports
            .iter()
            .filter(|(h, _)| h.contains("extern \"C"))
            .count();
        assert_eq!(items, attrs, "{file}: extern \"C\" items parsed");
        assert!(!exports.is_empty(), "{file}: no exports found");
        for (head, body) in &exports {
            let at = std::format!("{file}: `{head}`");
            let (errno_back, out_back) = check_export_body(&at, body, &axiom);
            handed_back += usize::from(errno_back);
            outs_back += usize::from(out_back);
        }
    }
    // 13 bodies in `api::libc` (each pair's body counted once): the
    // exports whose entry point states `errno`.
    assert_eq!(handed_back, 13, "exports that hand their token back");
    let sites: Vec<_> = crate::testutil::grant_sites(&axiom)
        .into_iter()
        .filter(|(_, at)| !at.contains("axiom fn "))
        .collect();
    for (sig, at) in &sites {
        assert!(
            EXPORT_FILES
                .iter()
                .any(|f| at.starts_with(&std::format!("{f}:")))
                && export_head(sig)
                && at.ends_with(&std::format!("`{}`", axiom_line(&axiom, failed_for(sig)))),
            "{at}: the errno axiom in `{sig}`"
        );
    }
    assert_eq!(sites.len(), handed_back, "errno axiom calls");
    // 4 bodies in `api::libc` (the `posix_memalign` pair counted once,
    // and the three allocating `challoc_*_usable` exports) hand an
    // out-pointer token back, each only there, right after the call.
    assert_eq!(outs_back, 4, "exports that hand an out-pointer token back");
    let outs = out_tokens();
    let out_axiom = std::format!("{}{}", "axiom_out_", "at_return");
    let out_sites: Vec<_> = crate::testutil::grant_sites(&out_axiom)
        .into_iter()
        .filter(|(_, at)| !at.contains("axiom fn "))
        .collect();
    for (sig, at) in &out_sites {
        assert!(
            at.starts_with("api/libc.rs:")
                && export_head(sig)
                && outs
                    .iter()
                    .any(|(_, back)| at.ends_with(&std::format!("`{back}`"))),
            "{at}: the out-pointer axiom in `{sig}`"
        );
    }
    assert_eq!(out_sites.len(), outs_back, "out-pointer axiom calls");
    let lend = lend_line();
    for (sig, at) in crate::testutil::grant_sites(&std::format!("{}{}", "errno::", "lend()")) {
        assert!(
            export_head(&sig) && at.ends_with(&std::format!("`{lend}`")),
            "{at}: an errno token lent in `{sig}`"
        );
    }
    // No code names an `exports` module (a path through one, or a
    // `use` of one), so no verified code calls an export and no proof
    // sees two returns (`vspec::errno::c_errno`).
    let mut files = Vec::new();
    crate::testutil::walk(&src, &mut files);
    let name = std::format!("{}{}", "expo", "rts");
    let path = std::format!("{name}::");
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            let names = t.contains(path.as_str())
                || ((t.starts_with("use ") || t.starts_with("pub use "))
                    && t.contains(name.as_str()));
            assert!(
                t.starts_with("//") || !names,
                "{}:{}: `{t}`",
                f.display(),
                i + 1
            );
        }
    }
}
