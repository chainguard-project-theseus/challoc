//! loom models of challoc's atomics protocols. challoc's statics are
//! built in `const` context, which loom cannot construct, so each model
//! here is a copy of the code it checks, step by step, naming the
//! function each step mirrors. `tests/pins.rs` checks that the source
//! still has the copied lines, in order.
//!
//! ```sh
//! sh tests/loom/run.sh
//! ```
//!
//! `run.sh` runs the models under loom 0.7.2 with `loom-0.7.2-dpor.patch`:
//! stock loom's DPOR may never run an atomic store ahead of another
//! thread's earlier load when the storing thread loaded the atomic last,
//! and misses executions the models need (`dpor.rs`, which fails without
//! the patch).
//!
//! Every model is exhaustive but `lock::loom_lock_three_threads` and its
//! control `lock::loom_lock_dropped_mark_loses_a_wake_up`, bounded at 5
//! preemptions. `LOOM_MAX_PREEMPTIONS` sets a bound for all of them
//! (`run.sh` unsets it). `LOOM_LOG=info` prints loom's execution count per
//! model.

#[cfg(test)]
mod background;
#[cfg(test)]
mod bitmap;
#[cfg(test)]
mod count;
#[cfg(test)]
mod dpor;
#[cfg(test)]
mod exit;
#[cfg(test)]
mod flags;
#[cfg(test)]
mod free;
#[cfg(test)]
mod futex;
#[cfg(test)]
mod init;
#[cfg(test)]
mod lock;
#[cfg(test)]
mod narenas;
#[cfg(test)]
mod oncecell;
#[cfg(test)]
mod position;
#[cfg(test)]
mod publish;
#[cfg(test)]
mod slab;
#[cfg(test)]
mod watermark;

/// Run `f` under every interleaving loom reaches (exhaustive: no
/// preemption bound unless `LOOM_MAX_PREEMPTIONS` sets one).
#[cfg(test)]
fn model<F: Fn() + Sync + Send + 'static>(f: F) {
    loom::model::Builder::new().check(f);
}

/// Run `f` with at most `bound` preemptions (unless
/// `LOOM_MAX_PREEMPTIONS` sets another), for a model too large to
/// exhaust. loom's documentation finds a bound of 2 or 3 enough for most
/// bugs.
#[cfg(test)]
fn model_bounded<F: Fn() + Sync + Send + 'static>(bound: usize, f: F) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(bound);
    }
    b.check(f);
}

/// Leaked, not an `Arc`: a failed model ends the execution with threads
/// parked, and dropping a loom object while that panic unwinds aborts the
/// test binary.
#[cfg(test)]
fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}
