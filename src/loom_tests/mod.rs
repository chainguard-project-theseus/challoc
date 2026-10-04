//! Model checking of the atomics protocols with loom (`verus/WEAKMEM.md`).
//!
//! Built only under `cfg(loom)`, where `crate::sync` re-exports loom's
//! atomics and the crate is cut down to the modules whose real code is
//! checked here (`lib.rs`): `lock` (P2, the futex replaced by the
//! stand-in in `futex`) and `bitmap` (P1, with `vspec::word`). The
//! background wake-up (P3), the background thread's exit and the
//! publication of `Shared` (P4) live in statics built in `const` context,
//! which loom cannot construct, so `background.rs`, `exit.rs` and
//! `init.rs` check copies that cite the code they mirror.
//!
//! ```sh
//! sh verus/loom/run.sh
//! ```
//!
//! The script runs these under loom 0.7.2 with
//! `verus/loom/loom-0.7.2-dpor.patch`: stock loom's DPOR never runs an
//! atomic store ahead of another thread's earlier load when the storing
//! thread loaded the atomic last, and misses executions the models need
//! (`dpor.rs`, which fails without the patch).
//!
//! Every model is exhaustive but `lock::loom_lock_three_threads` and its
//! control `lock::loom_lock_dropped_mark_loses_a_wake_up`, bounded at 5
//! preemptions (no bound did not finish in 25 minutes).
//! `LOOM_MAX_PREEMPTIONS` sets a bound for all of them (`run.sh` unsets
//! it).

// `cfg(test)` again on each module: `verus-count` (`verus/TODO.md`)
// skips a module only by the attribute on its own declaration.

#[cfg(test)]
mod background;
#[cfg(test)]
mod bitmap;
#[cfg(test)]
mod dpor;
#[cfg(test)]
mod exit;
#[cfg(test)]
pub(crate) mod futex;
#[cfg(test)]
mod init;
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
