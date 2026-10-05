//! The derived `PartialEq` impls that the `PartialEqSpecImpl` seams
//! describe (`obeys_eq_spec()` and `eq_spec(a, b) == (*a == *b)`, Verus's
//! structural equality). For an enum without fields, structural equality
//! is equality of the variant; for `Errno(i32)` it is equality of the
//! field. Each harness checks `==` and `!=` against that for every pair of
//! values (no bound: the variants are listed, the field is any `i32`).

use crate::config::{PercpuArena, Thp};
use crate::huge::RunState;
use crate::slab;
use crate::sys::Errno;
use crate::sys::cgroup;
use crate::sys::thp_host::ThpSetting;

/// Every variant, by index: `pick(variants)` is `variants[i]` for a
/// symbolic `i < variants.len()`, returned with `i`. The arrays are
/// written out by hand; each harness checks its array against an
/// exhaustive `match` (a new variant fails to compile there until it
/// gets an index), but one given an index and left out of the array
/// would go unchecked: extend both.
fn pick<T: Copy>(variants: &[T]) -> (usize, T) {
    let i: usize = kani::any();
    kani::assume(i < variants.len());
    (i, variants[i])
}

/// `a == b` and `a != b` both agree with the variant index.
fn check_variants<T: Copy + PartialEq>(variants: &[T]) {
    let (i, a) = pick(variants);
    let (j, b) = pick(variants);
    assert_eq!(a == b, i == j);
    assert_eq!(a != b, i != j);
}

fn slab_state_index(s: slab::State) -> usize {
    match s {
        slab::State::Fresh => 0,
        slab::State::Active => 1,
        slab::State::Empty => 2,
        slab::State::Purged => 3,
    }
}

/// S67 `PartialEqSpecImpl for State` (`slab::State`, class/mod.rs).
#[kani::proof]
fn eq_slab_state() {
    let v = [
        slab::State::Fresh,
        slab::State::Active,
        slab::State::Empty,
        slab::State::Purged,
    ];
    for (i, s) in v.iter().enumerate() {
        assert_eq!(slab_state_index(*s), i);
    }
    check_variants(&v);
}

fn thp_index(t: Thp) -> usize {
    match t {
        Thp::Never => 0,
        Thp::Auto => 1,
        Thp::Always => 2,
    }
}

/// S305 `PartialEqSpecImpl for config::Thp`.
#[kani::proof]
fn eq_config_thp() {
    let v = [Thp::Never, Thp::Auto, Thp::Always];
    for (i, t) in v.iter().enumerate() {
        assert_eq!(thp_index(*t), i);
    }
    check_variants(&v);
}

fn thp_setting_index(t: ThpSetting) -> usize {
    match t {
        ThpSetting::Always => 0,
        ThpSetting::Inherit => 1,
        ThpSetting::Madvise => 2,
        ThpSetting::Never => 3,
    }
}

/// S306 `PartialEqSpecImpl for sys::thp_host::ThpSetting`.
#[kani::proof]
fn eq_thp_setting() {
    let v = [
        ThpSetting::Always,
        ThpSetting::Inherit,
        ThpSetting::Madvise,
        ThpSetting::Never,
    ];
    for (i, t) in v.iter().enumerate() {
        assert_eq!(thp_setting_index(*t), i);
    }
    check_variants(&v);
}

fn run_state_index(s: RunState) -> usize {
    match s {
        RunState::Active => 0,
        RunState::Empty => 1,
        RunState::Purged => 2,
    }
}

/// S307 `PartialEqSpecImpl for huge::RunState`.
#[kani::proof]
fn eq_run_state() {
    let v = [RunState::Active, RunState::Empty, RunState::Purged];
    for (i, s) in v.iter().enumerate() {
        assert_eq!(run_state_index(*s), i);
    }
    check_variants(&v);
}

fn percpu_arena_index(p: PercpuArena) -> usize {
    match p {
        PercpuArena::Disabled => 0,
        PercpuArena::Percpu => 1,
        PercpuArena::Phycpu => 2,
    }
}

/// S318 `PartialEqSpecImpl for config::PercpuArena`.
#[kani::proof]
fn eq_percpu_arena() {
    let v = [
        PercpuArena::Disabled,
        PercpuArena::Percpu,
        PercpuArena::Phycpu,
    ];
    for (i, p) in v.iter().enumerate() {
        assert_eq!(percpu_arena_index(*p), i);
    }
    check_variants(&v);
}

fn cgroup_state_index(s: cgroup::State) -> usize {
    match s {
        cgroup::State::Disabled => 0,
        cgroup::State::NoUnifiedPath => 1,
        cgroup::State::NoMount => 2,
        cgroup::State::PathTooLong => 3,
        cgroup::State::Tracked => 4,
    }
}

/// S357 `PartialEqSpecImpl for sys::cgroup::State`.
#[kani::proof]
fn eq_cgroup_state() {
    let v = [
        cgroup::State::Disabled,
        cgroup::State::NoUnifiedPath,
        cgroup::State::NoMount,
        cgroup::State::PathTooLong,
        cgroup::State::Tracked,
    ];
    for (i, s) in v.iter().enumerate() {
        assert_eq!(cgroup_state_index(*s), i);
    }
    check_variants(&v);
}

/// S358 `PartialEqSpecImpl for sys::Errno`: the derived `==` of the one
/// field, for every pair of `i32`s.
#[kani::proof]
fn eq_errno() {
    let a: i32 = kani::any();
    let b: i32 = kani::any();
    assert_eq!(Errno(a) == Errno(b), a == b);
    assert_eq!(Errno(a) != Errno(b), a != b);
}
