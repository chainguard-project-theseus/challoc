use super::*;
use std::vec::Vec;

// The values the tests below lock; their invariant is not stated.
impl LockSpec for u32 {}
impl LockSpec for u64 {}
impl LockSpec for [u64; 4] {}
impl<T> LockSpec for Vec<T> {}

/// Every lock axiom outside `lock.rs` states [`assumed_inv`] or
/// [`assumed_frame`], never [`lock_inv`], [`lock_frame`] or a
/// [`LockSpec`] method: an axiom on those could contradict a real impl
/// added later and prove `false`, while one on the `assumed_*`
/// functions constrains only the types whose impls delegate to them.
/// The four names are matched as words, so a method call
/// (`v.lock_spec_inv()`), a type path (`Inner::lock_spec_inv(v)`), a
/// generic call (`T::lock_spec_inv(v)`) and a turbofish all count, and
/// no function outside a `LockSpec` impl takes the method names. No lock
/// axiom names, in its parameters, a type whose impl defines the
/// function the axiom states (does not delegate it): such an axiom
/// constrains nothing the type's holders use, so it is dead and its
/// doc misleads. An axiom is an `axiom fn` item through the first line
/// that ends in `;`, and a lock axiom one that names any of these
/// functions. Types are compared by path, a bare name read as an item
/// of the module whose file names it. Not seen: an `assumed_*` call
/// spelled with a space before `(`, a `use .. as` rename, a trusted
/// fact outside an `axiom fn` (an `external_body` proof, an `assume`),
/// and a type named only inside `ensures`. `lock.rs`, whose axiom
/// `axiom_acquired` is generic over `T`, is not scanned. What the scan
/// reads each impl to define is what `DEFINED` lists, so a lock whose
/// axioms were retired is one the scan sees, and a leftover axiom on it
/// fails here.
#[test]
fn lock_axioms_state_assumed_specs() {
    use std::string::String;
    // (type, defines `inv`, defines `frame`) for each impl that defines
    // either; every other impl delegates both.
    const DEFINED: &[(&str, bool, bool)] = &[
        ("crate::arena::Created", true, true),
        ("crate::class::Inner", true, true),
        ("crate::class::sets::[Lazy; SET_GROUPS]", true, true),
        ("crate::class::table::TableInner", true, true),
        ("crate::heap::tcaches::()", true, true),
        ("crate::huge::Inner", true, true),
        ("crate::meta::Inner", true, true),
        ("crate::purge::background::State", true, true),
        ("crate::stats::ArenaStats", true, true),
        ("crate::stats::Snapshot", true, true),
    ];
    // Spelled in parts, so that this test's own lines match nothing.
    let named = [
        std::format!("{}{}", "lock_", "inv"),
        std::format!("{}{}", "lock_", "frame"),
        std::format!("{}{}", "lock_spec_", "inv"),
        std::format!("{}{}", "lock_spec_", "frame"),
    ];
    let assumed_inv = std::format!("{}{}", "assumed_", "inv(");
    let assumed_frame = std::format!("{}{}", "assumed_", "frame(");
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // A use of `name`, not the tail of a longer name.
    let calls = |t: &str, name: &str| t.match_indices(name).any(|(i, _)| !t[..i].ends_with(ident));
    // `name` as a whole word.
    let word = |t: &str, name: &str| {
        t.match_indices(name)
            .any(|(i, _)| !t[..i].ends_with(ident) && !t[i + name.len()..].starts_with(ident))
    };
    let key = crate::testutil::type_key;
    // Per impl: (type, where, defines `inv`, defines `frame`), from the
    // scan the unlock-rule test shares; per lock axiom: (where, module,
    // text).
    let scanned = crate::testutil::lock_spec_impls();
    let impls: Vec<(String, String, bool, bool)> = scanned
        .iter()
        .map(|s| {
            (
                s.ty.clone(),
                std::format!("{}:{}", s.rel, s.start + 1),
                s.inv,
                s.frame,
            )
        })
        .collect();
    let mut axioms: Vec<(String, String, String)> = Vec::new();
    // Every `axiom fn` the scan reads, lock axiom or not: no lock axiom
    // is left outside `lock.rs`, so the scan's health is that it sees
    // the others.
    let mut seen = 0;
    for (rel, text) in crate::testutil::shipped_sources() {
        if rel == "lock.rs" {
            continue;
        }
        let m = crate::testutil::module_of(&rel);
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim_start();
            let at = std::format!("{rel}:{}", i + 1);
            if let Some(s) = scanned.iter().find(|s| s.rel == rel && s.start == i) {
                // An impl's body is the shared scan's; skip it.
                i = s.end;
            } else if named[2..].iter().any(|n| word(t, &std::format!("fn {n}"))) {
                // The names stay unique: no item outside a `LockSpec`
                // impl defines them.
                panic!("{at}: a function named as a `LockSpec` method outside an impl of it");
            } else if !t.starts_with("//") && t.contains("axiom fn ") {
                seen += 1;
                let mut item = String::new();
                loop {
                    item.push_str(lines[i]);
                    item.push('\n');
                    if lines[i].trim_end().ends_with(';') {
                        break;
                    }
                    i += 1;
                }
                if named.iter().any(|n| word(&item, n))
                    || [&assumed_inv, &assumed_frame]
                        .iter()
                        .any(|n| calls(&item, n))
                {
                    axioms.push((at, m.clone(), item));
                }
            }
            i += 1;
        }
    }
    assert!(!impls.is_empty(), "no `LockSpec` impl found");
    assert!(seen > 0, "no `axiom fn` found");
    for (k, imp, inv, frame) in &impls {
        let want = DEFINED
            .iter()
            .find(|d| d.0 == k.as_str())
            .map_or((false, false), |d| (d.1, d.2));
        assert!(
            (*inv, *frame) == want,
            "{imp}: `{k}`'s `LockSpec` impl defines (inv, frame) {:?}; `DEFINED` says {want:?}",
            (*inv, *frame)
        );
    }
    for d in DEFINED {
        assert!(
            impls.iter().any(|(k, ..)| k == d.0),
            "`DEFINED` names `{}`, which has no `LockSpec` impl",
            d.0
        );
    }
    for (at, m, item) in &axioms {
        for n in &named {
            assert!(
                !word(item, n),
                "{at}: a lock axiom names `{n}`; state `assumed_inv`/`assumed_frame` instead"
            );
        }
        // The parameters' types: the first `(..)` after the name, split
        // at its top-level commas.
        let name = item.find("axiom fn ").unwrap();
        let open = name + item[name..].find('(').unwrap();
        let mut depth = 0;
        let mut params = Vec::new();
        let mut cur = String::new();
        for c in item[open + 1..].chars() {
            match c {
                '(' | '[' | '<' => depth += 1,
                ')' | ']' | '>' if depth > 0 => depth -= 1,
                ')' => break,
                ',' if depth == 0 => {
                    params.push(core::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        params.push(cur);
        for p in params.iter().filter_map(|p| p.split_once(':')) {
            let ty = key(m, p.1);
            for (k, imp, inv, frame) in &impls {
                let dead =
                    (*inv && calls(item, &assumed_inv)) || (*frame && calls(item, &assumed_frame));
                assert!(
                    !(*k == ty && dead),
                    "{at}: a lock axiom on `{ty}` states a function its `LockSpec` impl at \
                     {imp} defines; remove the axiom"
                );
            }
        }
    }
}

/// `axiom_acquired` mints a permission of a lock's cell, and two alive
/// at once would prove `false` (vstd's `PointsTo::is_exclusive`): it is
/// called only where `Lock::lock` and `Lock::try_lock` build their
/// guard and where `Lock::lock_held_perm` hands the permission to its
/// holder, each right after an acquire. It is private to `lock.rs`.
#[test]
fn permission_axioms_are_called_only_at_acquisitions() {
    // Spelled in parts, so that this test's own lines match nothing.
    // A call is the name then `(` or a turbofish `::`.
    let acquired = std::format!("{}{}", "axiom_", "acquired");
    let calls = |t: &str, name: &str| {
        t.contains(std::format!("{name}(").as_str())
            || t.contains(std::format!("{name}::").as_str())
    };
    let mut a = Vec::new();
    let mut func = "";
    for line in include_str!("../lock.rs").lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        if let Some(i) = t.find("fn ") {
            func = t[i + 3..].split(['(', '<']).next().unwrap();
        }
        if calls(t, &acquired) && !t.starts_with("axiom fn") {
            a.push(func);
        }
    }
    assert_eq!(a, ["lock", "try_lock", "lock_held_perm"]);
}

/// `in_child_fork_window` passes `axiom_child_window` the window's
/// record as its writers leave it: the axiom's one use is that
/// function, the record's statics are named only in the functions
/// listed, `fork_window_open` stores the opener's `getpid` and then its
/// `pthread_self`, `fork_window_close` stores 0, and the axiom's
/// arguments are the reads its doc names, each bound once.
#[test]
fn fork_record_uses_are_listed() {
    use crate::testutil::{assert_only_at, grant_sites};
    // Spelled in parts, so that this test's lines match nothing.
    let window = concat!("pub(crate) fn in_child_", "fork_window(");
    let open = concat!("pub(crate) fn fork_window_", "open(");
    let close = concat!("pub(crate) fn fork_window_", "close(");
    assert_only_at(
        concat!("axiom_", "child_window("),
        &[("lock.rs:", window, 1)],
    );
    // Every line that names one of the two statics, but their
    // declarations, is in a listed function, as many times as listed:
    // no reference to them leaves these functions.
    let named = |name: &str, fns: &[(&str, usize)]| {
        let mut n = std::vec![0; fns.len()];
        for (sig, at) in grant_sites(name) {
            if at.contains("exec static ") {
                continue;
            }
            let i = fns
                .iter()
                .position(|(f, _)| at.starts_with("lock.rs:") && sig.starts_with(f))
                .unwrap_or_else(|| {
                    panic!("{at}: got = `{name}` in `{sig}`, want = only in the listed functions")
                });
            n[i] += 1;
        }
        for ((f, want), got) in fns.iter().zip(n) {
            assert_eq!(
                got, *want,
                "`{f}`: got = {got} lines naming `{name}`, want = {want}"
            );
        }
    };
    named(
        concat!("FORK_", "HOLDER"),
        &[
            (concat!("fn fork_holder_", "is_me("), 1),
            (open, 1),
            (window, 1),
            (close, 1),
        ],
    );
    named(concat!("FORK_", "PID"), &[(open, 1), (window, 1)]);
    // A function's code, from its item line to its closing brace,
    // without comment lines.
    let src = include_str!("../lock.rs");
    let body = |item: &str| -> std::string::String {
        let at: Vec<_> = src.match_indices(item).map(|(i, _)| i).collect();
        assert_eq!(
            at.len(),
            1,
            "`{item}`: got = {} matches, want = 1",
            at.len()
        );
        let b = &src[at[0]..];
        b[..b.find("\n}\n").unwrap()]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let o = body(open);
    let pid = o.find(concat!("FORK_", "PID.store(crate::sys::getpid() as u32, "));
    let holder = o.find(concat!(
        "FORK_",
        "HOLDER.store(crate::sys::pthread_self(), "
    ));
    assert!(
        matches!((pid, holder), (Some(p), Some(h)) if p < h),
        "fork_window_open: got = {pid:?} and {holder:?}, want = the pid stored, then the holder"
    );
    assert!(
        body(close).contains(concat!("FORK_", "HOLDER.store(0, ")),
        "fork_window_close: got = no store of 0, want = one"
    );
    let w = body(window);
    let reads = [
        concat!("let h = FORK_", "HOLDER.load(Ordering::Relaxed);"),
        "let me = crate::sys::pthread_self();",
        concat!("let pid = FORK_", "PID.load(Ordering::Relaxed);"),
        "let now = crate::sys::getpid() as u32;",
        concat!("axiom_", "child_window(h, me, pid, now);"),
    ];
    let mut from = 0;
    for r in reads {
        let i = w[from..].find(r).unwrap_or_else(|| {
            panic!("in_child_fork_window: got = no `{r}` after the reads before it, want = one")
        });
        from += i + r.len();
    }
    // No other binding shadows a read, and none is mutable.
    assert!(
        !w.contains("let mut "),
        "in_child_fork_window: got = a `let mut`, want = none"
    );
    for n in ["h", "me", "pid", "now"] {
        let k = [" ", ":", "="]
            .iter()
            .map(|c| w.matches(&std::format!("let {n}{c}")).count())
            .sum::<usize>();
        assert_eq!(
            k, 1,
            "in_child_fork_window: got = {k} bindings of `{n}`, want = 1"
        );
    }
}

/// `Lock::value_ptr` admits any pointer, and its projection is defined
/// only through one into a live `Lock<T>`: its one caller is
/// `Arena::put_stats`, which passes its own parameter `p`, the pointer
/// its permission names (`requires old(perm).ptr() == p`), at `T =
/// ArenaStats`, one of the `T`s `kani_lock` runs it at. The only other
/// calls are Kani harnesses' (`cfg(kani)`, never in a build): `kani_lock`'s
/// and the two in `arena::kani_put_stats::put_stats_writes_empty`, which
/// pass a pointer to its own `Lock<ArenaStats>`. Any call spelling that ends in `value_ptr(`
/// counts (`Self::` and `Lock::<T>::` included), and the name is never
/// used but as a call (no function pointer to it); an associated
/// function cannot be imported under another name. In `put_stats`'s
/// body `p` appears only in its two statements, so nothing rebinds it.
#[test]
fn value_ptr_called_only_by_put_stats() {
    use crate::testutil::{assert_only_at, grant_sites};
    // Spelled in parts, so that this test's lines match nothing.
    let name = concat!("value", "_ptr");
    let call = concat!("value", "_ptr(");
    let put = concat!("unsafe fn put_", "stats(");
    assert_only_at(
        call,
        &[
            ("arena.rs:", put, 1),
            ("arena.rs:", "fn put_stats_writes_empty() {", 2),
            ("lock.rs:", "fn on<T>()", 1),
        ],
    );
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    for (_, at) in grant_sites(name) {
        let text = at
            .split_once(": `")
            .map_or("", |(_, t)| t.strip_suffix('`').unwrap_or(t));
        for (i, _) in text.match_indices(name) {
            let before = text[..i].chars().next_back().is_some_and(ident);
            let after = text[i + name.len()..].chars().next();
            assert!(
                before || after.is_some_and(|c| c == '(' || ident(c)),
                "{at}: got = `{name}` not called, want = only calls"
            );
        }
    }
    let src = include_str!("../arena.rs");
    let at: Vec<_> = src.match_indices(put).map(|(i, _)| i).collect();
    assert_eq!(at.len(), 1, "`{put}`: got = {} matches, want = 1", at.len());
    let item = &src[at[0]..];
    let end = item
        .find("\n    }\n")
        .unwrap_or_else(|| panic!("put_stats: got = no `}}` at 4 spaces, want = its body's end"));
    let item: Vec<&str> = item[..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .collect();
    let sig = concat!(
        "(p: *mut Lock<Arena",
        "Stats>, Tracked(perm): Tracked<&mut PointsTo<Lock<ArenaStats>>>, "
    );
    assert!(
        item[0].contains(sig),
        "put_stats: got = `{}`, want = `p` and the permission at `Lock<ArenaStats>`",
        item[0]
    );
    let line = |l: &str| {
        item.iter()
            .position(|x| *x == l)
            .unwrap_or_else(|| panic!("put_stats: got = no `{l}` line, want = one"))
    };
    let (requires, ensures, body) = (line("requires"), line("ensures"), line("{"));
    assert!(
        item[requires..ensures].contains(&"old(perm).ptr() == p,"),
        "put_stats: got = no `old(perm).ptr() == p`, want = the permission names `p`"
    );
    let uses: Vec<&str> = item[body..]
        .iter()
        .copied()
        .filter(|l| l.split(|c: char| !ident(c)).any(|w| w == "p"))
        .collect();
    let want = [
        "core::ptr::write_bytes(p, 0, 1);",
        concat!("(*Lock::value", "_ptr(p)).index = index;"),
    ];
    assert_eq!(
        uses, want,
        "put_stats: got = {uses:?} naming `p`, want = {want:?}"
    );
    assert!(
        include_str!("../lock.rs").contains(concat!("on::<crate::stats::Arena", "Stats>();")),
        "kani_lock: got = no `ArenaStats` instance, want = put_stats' `T`"
    );
}

/// `axiom_init_entry` and `axiom_thread_entry` assert that no
/// allocator lock is held where they are called, which only the
/// caller's context shows: each use is pinned to the function its doc
/// names, one token each, so a new one fails here until the doc argues
/// its premise. (`axiom_c_entry`'s uses are the export bodies:
/// `api::tests::c_call_tokens_only_in_export_bodies`.)
#[test]
fn root_lock_order_uses_are_listed() {
    use crate::testutil::assert_only_at;
    assert_only_at(
        concat!("axiom_", "init_entry("),
        &[("heap/shared.rs:", "pub unsafe fn init(", 1)],
    );
    assert_only_at(
        concat!("axiom_", "thread_entry("),
        &[
            ("purge/background.rs:", "extern \"C\" fn main(", 1),
            (
                "heap/thread.rs:",
                "pub unsafe extern \"C\" fn heap_destructor(",
                1,
            ),
        ],
    );
}

#[test]
fn raw_lock_state_machine() {
    let l = RawLock::new(Ghost::assume_new());
    assert!(!l.is_locked());
    assert!(l.try_lock());
    assert!(l.is_locked());
    assert!(!l.try_lock());
    l.release();
    assert!(!l.is_locked());
    assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
    l.lock();
    assert!(!l.try_lock());
    l.release();
    assert!(!l.is_locked());
}

#[test]
fn guard_releases_on_drop() {
    let l = Lock::new(5u32, Ghost::assume_new());
    {
        let mut g = l.lock_t();
        *g += 1;
        assert!(l.try_lock().is_none());
        assert!(l.raw().is_locked());
    }
    assert!(!l.raw().is_locked());
    assert_eq!(*l.lock_t(), 6);
    assert_eq!(*l.try_lock().unwrap(), 6);
}

/// Many threads hammer one counter: every increment must land, and the
/// lock must end up unlocked with no waiter bit stuck.
#[test]
fn contended_counter_is_exact() {
    const THREADS: u64 = 8;
    const ITERS: u64 = 50_000;
    let l = Lock::new(0u64, Ghost::assume_new());
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                for _ in 0..ITERS {
                    *l.lock_t() += 1;
                }
            });
        }
    });
    assert_eq!(*l.lock_t(), THREADS * ITERS);
    assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
}

/// Long holds force the futex path (spinning cannot outlast them) and
/// every waiter must still get through.
#[test]
fn sleepers_are_woken() {
    let l = Lock::new(Vec::new(), Ghost::assume_new());
    std::thread::scope(|s| {
        for t in 0..4u32 {
            let l = &l;
            s.spawn(move || {
                for i in 0..20u32 {
                    let mut g = l.lock_t();
                    g.push(t * 100 + i);
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
            });
        }
    });
    {
        let v = l.lock_t();
        assert_eq!(v.len(), 80);
        for t in 0..4u32 {
            let mine: Vec<u32> = v.iter().copied().filter(|x| x / 100 == t).collect();
            assert_eq!(mine, (0..20).map(|i| t * 100 + i).collect::<Vec<_>>());
        }
    }
    assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
}

/// One waiter spinning while the holder sleeps past the spin phase:
/// the waiter must reach the futex (announce `CONTENDED`), stay parked
/// while the lock is held, and be woken by the release.
///
/// Every wait is bounded by `BOUND` and fails instead of hanging, and
/// the waiter is a detached thread, not a scoped one: `thread::scope`
/// joins its threads on the way out even when the body panicked, so
/// an assertion that failed while the lock was still held would leave
/// the waiter parked on a lock nobody releases and the test thread
/// parked in the join, with the panic message held by the harness.
/// The announce is waited for, and the lock is released before
/// anything is asserted about it.
#[test]
fn a_parked_waiter_is_released() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    /// Long enough for a thread to be scheduled and woken on a host
    /// under load; a wait that outlasts it is a failure, not a delay.
    const BOUND: Duration = Duration::from_secs(10);
    /// Poll `done` every millisecond for up to `BOUND`.
    fn within_bound(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + BOUND;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let l = Arc::new(RawLock::new(Ghost::assume_new()));
    l.lock();
    let waiter = {
        let l = Arc::clone(&l);
        std::thread::spawn(move || {
            l.lock();
            let acquired_at = Instant::now();
            l.release();
            acquired_at
        })
    };
    // The announce: the spin phase ends with the swap to `CONTENDED`.
    if !within_bound(|| l.state.load(Ordering::Relaxed) == CONTENDED) {
        l.release();
        panic!("the waiter did not announce itself within {BOUND:?}");
    }
    // Parked: while the lock stays held the word stays `CONTENDED`
    // and the waiter does not get through. Read, then release, then
    // assert, so a failure leaves nothing parked.
    std::thread::sleep(Duration::from_millis(20));
    let word = l.state.load(Ordering::Relaxed);
    let through = waiter.is_finished();
    let released_at = Instant::now();
    l.release();
    assert_eq!(word, CONTENDED, "the word changed while the lock was held");
    assert!(!through, "the waiter got through while the lock was held");
    // Released: the wake reaches the waiter. On failure the waiter
    // stays parked (a lost wake); the test fails rather than joins.
    assert!(
        within_bound(|| waiter.is_finished()),
        "the waiter was not woken within {BOUND:?} of the release"
    );
    let acquired_at = waiter.join().unwrap();
    assert!(
        acquired_at >= released_at,
        "acquired {:?} before the release",
        released_at - acquired_at
    );
    assert_eq!(l.state.load(Ordering::Relaxed), UNLOCKED);
}

/// Weak-memory stress of `RawLock` (ignored; `CHALLOC_STRESS_SECS`).
/// Twice the CPUs take one lock, check that its four plain words are
/// equal and bump them all. A holder that reads two different words,
/// or a final value below the acquisitions, saw a critical section
/// whose writes were not ordered before its own: the edge from
/// `release`'s `swap(Release)` to the next `try_acquire` or
/// `swap(CONTENDED, Acquire)` (WEAKMEM.md, P2). Twice the CPUs keep
/// some lockers on the futex path.
#[test]
#[ignore]
fn stress_lock_edges() {
    use core::sync::atomic::{AtomicBool, AtomicU64};
    let threads = 2 * crate::testutil::stress_threads();
    let l = Lock::new([0u64; 4], Ghost::assume_new());
    let stop = AtomicBool::new(false);
    let torn = AtomicU64::new(0);
    let first_torn = AtomicU64::new(u64::MAX);
    let started = std::time::Instant::now();
    let total: u64 = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let mut g = l.lock_t();
                        let v = g[0];
                        if g[1] != v || g[2] != v || g[3] != v {
                            torn.fetch_add(1, Ordering::Relaxed);
                            first_torn.fetch_min(v, Ordering::Relaxed);
                        }
                        let m = v.max(g[1]).max(g[2]).max(g[3]) + 1;
                        g[0] = m;
                        g[1] = m;
                        g[2] = m;
                        g[3] = m;
                        drop(g);
                        n += 1;
                    }
                    n
                })
            })
            .collect();
        std::thread::sleep(crate::testutil::stress_duration());
        stop.store(true, Ordering::Relaxed);
        hs.into_iter().map(|h| h.join().unwrap()).sum()
    });
    let end = *l.lock_t();
    let lost = total - end[0].min(total);
    crate::testutil::stress_report(
        "lock_edges",
        &std::format!(
            "threads={threads} acquisitions={total} secs={:.1} torn={} first_torn_at={} lost={lost}",
            started.elapsed().as_secs_f64(),
            torn.load(Ordering::Relaxed),
            first_torn.load(Ordering::Relaxed) as i64,
        ),
    );
    assert_eq!(torn.load(Ordering::Relaxed), 0, "a holder read torn words");
    assert_eq!(end, [total; 4], "lost updates: {lost}");
    assert_eq!(l.raw().state.load(Ordering::Relaxed), UNLOCKED);
}
