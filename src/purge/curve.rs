//! The decay period and curve (design section 8): `dirty_decay_ms` as a
//! nanosecond period ([`NEVER`] for "never purge"), the window divided into
//! [`STEPS`] steps, and [`WEIGHTS`], the share of a cohort that may still be
//! dirty at each step of its age. [`Curve`] turns an age into a step and a
//! weighted sum into an allowance; the walks that apply it live with their
//! owners (`ClassState::purge_curve`, `Huge::purge_curve`), the step gate
//! that runs them is `purge::Decayer`.

/// `decay_ns` value meaning "never purge" (`dirty_decay_ms:-1`).
pub const NEVER: u64 = u64::MAX;

/// `dirty_decay_ms` to a nanosecond period; negative means [`NEVER`].
pub const fn decay_ns_from_ms(ms: i64) -> u64 {
    if ms < 0 {
        NEVER
    } else {
        let ns = (ms as u64).saturating_mul(1_000_000);
        if ns == NEVER { NEVER - 1 } else { ns }
    }
}

/// Steps of the decay curve over one window (jemalloc's
/// `SMOOTHSTEP_NSTEPS`).
pub const STEPS: usize = 200;
/// Fixed-point one of the curve weights (jemalloc's `SMOOTHSTEP_BFP`).
pub const WEIGHT_ONE: u64 = 1 << 24;

/// `WEIGHTS[s]`: the share of a cohort of dirty memory that may stay dirty
/// while its age is in step `s` of the window, as
/// `WEIGHT_ONE * (1 - smootherstep((s + 1) / STEPS))` (jemalloc's
/// `h_steps`, variant "smoother": `6x^5 - 15x^4 + 10x^3`). Non-increasing,
/// from just under one to zero at `STEPS - 1`, positive before that.
pub static WEIGHTS: [u32; STEPS] = build_weights();

const fn build_weights() -> [u32; STEPS] {
    let mut w = [0u32; STEPS];
    let n = STEPS as i128;
    let n5 = n * n * n * n * n;
    let mut s = 0;
    while s < STEPS {
        let t = (s + 1) as i128;
        // smootherstep(t / n), scaled by n^5.
        let num = 6 * t * t * t * t * t - 15 * t * t * t * t * n + 10 * t * t * t * n * n;
        w[s] = (((n5 - num) * WEIGHT_ONE as i128 + n5 / 2) / n5) as u32;
        s += 1;
    }
    w
}

/// The decay curve of one period (design section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Curve {
    decay_ns: u64,
}

impl Curve {
    pub const fn new(decay_ns: u64) -> Curve {
        Curve { decay_ns }
    }

    /// Length of one step; 0 for a zero period, where everything is past
    /// the window at once.
    #[inline]
    pub const fn step_ns(&self) -> u64 {
        self.decay_ns / STEPS as u64
    }

    /// The step something dirty since `dirty_at` is in at `now`:
    /// `STEPS - 1` (weight zero) for anything in or past the last step.
    pub fn step(&self, now: u64, dirty_at: u64) -> usize {
        let age = now.saturating_sub(dirty_at);
        if age >= self.decay_ns {
            return STEPS - 1;
        }
        // `u64` only: the 128-bit division helper on aarch64 drags
        // `rust_eh_personality` into the `no_std` library. An age too large
        // to scale is under a period whose step is at least `u64::MAX /
        // STEPS`, so dividing by the step loses nothing that matters.
        let s = if age > u64::MAX / STEPS as u64 { age / self.step_ns() } else { age * STEPS as u64 / self.decay_ns };
        (s as usize).min(STEPS - 1)
    }

    /// Weight of `step`, in units of [`WEIGHT_ONE`].
    #[inline]
    pub fn weight(step: usize) -> u64 {
        WEIGHTS[step] as u64
    }

    /// Units (slabs, pages) that may stay dirty given `weighted`, the sum
    /// of `units * weight(step)` over everything dirty: the curve's value
    /// rounded up, so a lone slab is not purged before its cohort's weight
    /// reaches zero (jemalloc rounds its page count down; here the unit is
    /// a slab of 16 pages, and a purge-and-refault of it for one unit of
    /// rounding is not worth it).
    #[inline]
    pub fn allowance(weighted: u64) -> usize {
        weighted.div_ceil(WEIGHT_ONE) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curve is jemalloc's: just under one at the start, zero only at
    /// the last step, half at mid-window, non-increasing, and symmetric
    /// about the middle.
    #[test]
    fn weights_are_a_smootherstep_from_one_to_zero() {
        // 1 - smootherstep(1 / 200) = 1 - 1.24e-6 (jemalloc: 0x14 of 2^24).
        assert_eq!(WEIGHTS[0] as u64, WEIGHT_ONE - 21);
        assert_eq!(WEIGHTS[STEPS - 2], 21);
        assert_eq!(WEIGHTS[STEPS - 1], 0);
        // 1 - smootherstep(100 / 200) = 0.5 exactly.
        assert_eq!(WEIGHTS[STEPS / 2 - 1] as u64, WEIGHT_ONE / 2);
        for s in 1..STEPS {
            assert!(WEIGHTS[s] <= WEIGHTS[s - 1], "step {s}");
        }
        // smootherstep(1 - x) = 1 - smootherstep(x).
        for s in 0..STEPS - 1 {
            let mirror = STEPS - 2 - s;
            let sum = WEIGHTS[s] as u64 + WEIGHTS[mirror] as u64;
            assert!(sum.abs_diff(WEIGHT_ONE) <= 1, "steps {s} and {mirror}: {sum}");
        }
        // A quarter of the way in, 1 - smootherstep(0.25) = 0.896...
        let q = WEIGHTS[STEPS / 4 - 1] as f64 / WEIGHT_ONE as f64;
        assert!((q - 0.896484375).abs() < 0.001, "{q}");
    }

    #[test]
    fn steps_and_allowance() {
        let c = Curve::new(200_000);
        assert_eq!(c.step_ns(), 1000);
        assert_eq!(c.step(5000, 5000), 0);
        assert_eq!(c.step(5999, 5000), 0);
        assert_eq!(c.step(6000, 5000), 1);
        assert_eq!(c.step(5000 + 100_000, 5000), 100);
        assert_eq!(c.step(5000 + 199_000, 5000), STEPS - 1);
        assert_eq!(c.step(u64::MAX, 5000), STEPS - 1);
        // `now` before `dirty_at` (a stale clock read) is age zero.
        assert_eq!(c.step(4000, 5000), 0);
        // A zero period puts everything in the last step.
        assert_eq!(Curve::new(0).step(5000, 5000), STEPS - 1);
        assert_eq!(Curve::new(0).step_ns(), 0);
        // Ages beyond the scaling range, under a period longer still.
        let huge = Curve::new(u64::MAX - 1);
        assert_eq!(huge.step(u64::MAX / 2, 0), STEPS / 2);
        assert_eq!(huge.step(u64::MAX - 1, 0), STEPS - 1);
        assert_eq!(huge.step(1000, 0), 0);
        // A period shorter than the step count still ends.
        assert_eq!(Curve::new(7).step(5007, 5000), STEPS - 1);
        assert_eq!(Curve::new(7).step(5000, 5000), 0);
        // The allowance rounds up.
        assert_eq!(Curve::allowance(0), 0);
        assert_eq!(Curve::allowance(1), 1);
        assert_eq!(Curve::allowance(WEIGHT_ONE), 1);
        assert_eq!(Curve::allowance(WEIGHT_ONE + 1), 2);
        assert_eq!(Curve::allowance(100 * Curve::weight(STEPS / 2 - 1)), 50);
        assert_eq!(Curve::allowance(100 * Curve::weight(STEPS - 1)), 0);
        // Half of a cohort of 100 at mid-window, all of it at the end, and
        // none of it in the first step.
        assert_eq!(Curve::allowance(100 * Curve::weight(0)), 100);
    }

    #[test]
    fn decay_period_from_ms() {
        assert_eq!(decay_ns_from_ms(-1), NEVER);
        assert_eq!(decay_ns_from_ms(-1000), NEVER);
        assert_eq!(decay_ns_from_ms(0), 0);
        assert_eq!(decay_ns_from_ms(10_000), 10_000_000_000);
        assert_eq!(decay_ns_from_ms(crate::config::MAX_INTERVAL_MS), 1_000_000_000_000_000_000);
        assert_ne!(decay_ns_from_ms(i64::MAX), NEVER);
    }
}
