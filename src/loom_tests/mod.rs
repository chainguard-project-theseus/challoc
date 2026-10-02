//! Model checking of the atomics protocols with loom (`verus/WEAKMEM.md`).
//!
//! Built only under `cfg(loom)`, where `crate::sync` re-exports loom's
//! atomics and the crate is cut down to the modules whose real code is
//! checked here (`lib.rs`): `lock` (P2, the futex replaced by the
//! stand-in in `futex`) and `bitmap` (P1, with `vspec::word`). The
//! background wake-up (P3) lives in statics built in `const` context,
//! which loom cannot construct, so `background.rs` checks a copy that
//! cites the lines it mirrors.
//!
//! ```sh
//! RUSTFLAGS="--cfg loom" cargo test --release loom_
//! ```
//!
//! Every model is exhaustive but `lock::loom_lock_three_threads`, bounded
//! at 5 preemptions (6 s; 6 takes 26 s, 7 takes 87 s, and no bound
//! did not finish in 25 minutes). `LOOM_MAX_PREEMPTIONS` sets a bound for
//! all of them.

// `cfg(test)` again on each module: `verus-count` (`verus/TODO.md`)
// skips a module only by the attribute on its own declaration.

#[cfg(test)]
mod background;
#[cfg(test)]
mod bitmap;
#[cfg(test)]
pub(crate) mod futex;
#[cfg(test)]
mod lock;

/// Run `f` under every interleaving loom reaches (exhaustive: no
/// preemption bound unless `LOOM_MAX_PREEMPTIONS` sets one).
fn model<F: Fn() + Sync + Send + 'static>(f: F) {
    loom::model::Builder::new().check(f);
}

/// Run `f` with at most `bound` preemptions (unless
/// `LOOM_MAX_PREEMPTIONS` sets another), for a model too large to
/// exhaust. loom's documentation finds a bound of 2 or 3 enough for most
/// bugs.
fn model_bounded<F: Fn() + Sync + Send + 'static>(bound: usize, f: F) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(bound);
    }
    b.check(f);
}
