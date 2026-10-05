//! loom models of challoc's atomics protocols. challoc's statics are
//! built in `const` context, which loom cannot construct, so each model
//! here is a copy of the code it checks, step by step, naming the
//! function each step mirrors. `tests/pins.rs` checks that the source
//! still has the copied lines, in order.
//!
//! ```sh
//! cd tests/loom && cargo test --release
//! ```
//!
//! Stock loom 0.7.2; every model is exhaustive unless `LOOM_MAX_PREEMPTIONS`
//! sets a bound.
//! `LOOM_LOG=info` prints loom's execution count per model.

#[cfg(test)]
mod background;
#[cfg(test)]
mod futex;
