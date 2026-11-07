//! The decay period and curve (design 8): `dirty_decay_ms` as a
//! nanosecond period ([`NEVER`] for "never purge"), the window divided into
//! [`STEPS`] steps, and [`WEIGHTS`], the share of a cohort that may still be
//! dirty at each step of its age. [`Curve`] turns an age into a step and a
//! weighted sum into an allowance; the walks that apply it live with their
//! owners (`ClassState::purge_curve`, `Huge::purge_curve`), the step gate
//! that runs them is `purge::Decayer`.

#[cfg(verus_keep_ghost)]
use vstd::arithmetic::div_mod::lemma_div_is_ordered;
use vstd::prelude::*;

verus! {

/// `decay_ns` value meaning "never purge" (`dirty_decay_ms:-1`).
pub const NEVER: u64 = u64::MAX;

/// `dirty_decay_ms` to a nanosecond period; negative means [`NEVER`].
pub const fn decay_ns_from_ms(ms: i64) -> (ns: u64)
    ensures
        ms < 0 ==> ns == NEVER,
        ms >= 0 ==> ns == if ms * 1_000_000 >= NEVER { (NEVER - 1) as int } else { ms * 1_000_000 },
{
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
pub exec static WEIGHTS: [u32; STEPS] = build_weights();

const fn build_weights() -> (w: [u32; STEPS])
    ensures
        forall|s: int| 0 <= s < STEPS ==> w[s] <= WEIGHT_ONE,
        forall|s: int| 0 <= s < STEPS - 1 ==> w[s] > 0,
        w[STEPS - 1] == 0,
        forall|s: int| 1 <= s < STEPS ==> #[trigger] w[s] <= w[s - 1],
{
    let mut w = [0u32; STEPS];
    let n = STEPS as i128;
    let n5 = n * n * n * n * n;
    let mut s = 0;
    while s < STEPS
        invariant
            n == 200,
            n5 == 320_000_000_000,
            s <= STEPS,
            forall|j: int| 0 <= j < s ==> w[j] <= WEIGHT_ONE,
            forall|j: int| 0 <= j < s && j < STEPS - 1 ==> w[j] > 0,
            s == STEPS ==> w[STEPS - 1] == 0,
            forall|j: int| 0 <= j < s ==> w[j] == weight_at(j + 1),
            forall|j: int| 1 <= j < s ==> #[trigger] w[j] <= w[j - 1],
        decreases STEPS - s,
    {
        let t = (s + 1) as i128;
        proof {
            smootherstep_bounds(t as int);
            weight_one_value();
        }
        // smootherstep(t / n), scaled by n^5.
        let num = 6 * t * t * t * t * t - 15 * t * t * t * t * n + 10 * t * t * t * n * n;
        proof {
            weight_bounds(t as int, num as int);
            if s > 0 {
                weight_non_increasing(s as int);
            }
        }
        w[s] = (((n5 - num) * WEIGHT_ONE as i128 + n5 / 2) / n5) as u32;
        s += 1;
    }
    w
}

/// The smootherstep numerator at `t / 200`, scaled by `200^5`, as the
/// exec arithmetic of [`build_weights`] spells it.
spec fn smoother(t: int) -> int {
    6 * t * t * t * t * t - 15 * t * t * t * t * 200 + 10 * t * t * t * 200 * 200
}

/// The weight [`build_weights`] computes for `t = s + 1`.
spec fn weight_at(t: int) -> int {
    ((320_000_000_000 - smoother(t)) * 0x100_0000 + 160_000_000_000) / 320_000_000_000
}

/// The numerator rises from `t` to the last step.
spec fn rises_from(t: int) -> bool
    decreases 200 - t,
{
    if t >= 200 { true } else { smoother(t) <= smoother(t + 1) && rises_from(t + 1) }
}

proof fn lemma_rises_from(k: int, t: int)
    requires
        1 <= k <= t < 200,
        rises_from(k),
    ensures
        smoother(t) <= smoother(t + 1),
    decreases t - k,
{
    if k < t {
        lemma_rises_from(k + 1, t);
    }
}

/// The weights never rise (the numerator's derivative is
/// `30 t^2 (t - n)^2`); checked by evaluation over the 199 steps.
proof fn weight_non_increasing(t: int)
    requires
        1 <= t < 200,
    ensures
        weight_at(t + 1) <= weight_at(t),
{
    assert(rises_from(1)) by (compute_only);
    lemma_rises_from(1, t);
    lemma_div_is_ordered(
        (320_000_000_000 - smoother(t + 1)) * 0x100_0000 + 160_000_000_000,
        (320_000_000_000 - smoother(t)) * 0x100_0000 + 160_000_000_000,
        320_000_000_000,
    );
}

/// For `1 <= t <= 200` the partial products of the smootherstep
/// numerator's terms fit well within `i128`.
proof fn smootherstep_bounds(t: int)
    requires
        1 <= t <= 200,
    ensures
        0 <= 6 * t <= 1_200,
        0 <= 6 * t * t <= 240_000,
        0 <= 6 * t * t * t <= 48_000_000,
        0 <= 6 * t * t * t * t <= 9_600_000_000,
        0 <= 6 * t * t * t * t * t <= 1_920_000_000_000,
        0 <= 15 * t <= 3_000,
        0 <= 15 * t * t <= 600_000,
        0 <= 15 * t * t * t <= 120_000_000,
        0 <= 15 * t * t * t * t <= 24_000_000_000,
        0 <= 15 * t * t * t * t * 200 <= 4_800_000_000_000,
        0 <= 10 * t <= 2_000,
        0 <= 10 * t * t <= 400_000,
        0 <= 10 * t * t * t <= 80_000_000,
        0 <= 10 * t * t * t * 200 <= 16_000_000_000,
        0 <= 10 * t * t * t * 200 * 200 <= 3_200_000_000_000,
{
    scaled_powers(6, t);
    scaled_powers(15, t);
    scaled_powers(10, t);
    assert(0 <= 6 * t * t * t * t <= 9_600_000_000);
    assert(0 <= 15 * t * t * t * t <= 24_000_000_000);
    assert(0 <= 10 * t * t * t <= 80_000_000);
    assert(0 <= 15 * t * t * t * t * 200 <= 4_800_000_000_000) by (nonlinear_arith)
        requires 0 <= 15 * t * t * t * t <= 24_000_000_000;
    assert(0 <= 10 * t * t * t * 200 <= 16_000_000_000) by (nonlinear_arith)
        requires 0 <= 10 * t * t * t <= 80_000_000;
    assert(0 <= 10 * t * t * t * 200 * 200 <= 3_200_000_000_000) by (nonlinear_arith)
        requires 0 <= 10 * t * t * t * 200 <= 16_000_000_000;
}

/// `k * t^i <= k * 200^i` for `i` up to 5, spelled as the left-nested
/// products the exec arithmetic forms.
proof fn scaled_powers(k: int, t: int)
    requires
        0 <= k <= 15,
        1 <= t <= 200,
    ensures
        0 <= k * t <= k * 200,
        0 <= k * t * t <= k * 40_000,
        0 <= k * t * t * t <= k * 8_000_000,
        0 <= k * t * t * t * t <= k * 1_600_000_000,
        0 <= k * t * t * t * t * t <= k * 320_000_000_000,
{
    assert(0 <= t * t <= 40_000) by (nonlinear_arith)
        requires 1 <= t <= 200;
    assert(0 <= t * t * t <= 8_000_000) by (nonlinear_arith)
        requires 1 <= t <= 200, 0 <= t * t <= 40_000;
    assert(0 <= t * t * t * t <= 1_600_000_000) by (nonlinear_arith)
        requires 1 <= t <= 200, 0 <= t * t * t <= 8_000_000;
    assert(0 <= t * t * t * t * t <= 320_000_000_000) by (nonlinear_arith)
        requires 1 <= t <= 200, 0 <= t * t * t * t <= 1_600_000_000;
    assert(0 <= k * t <= k * 200) by (nonlinear_arith)
        requires 0 <= k <= 15, 1 <= t <= 200;
    assert(k * t * t == k * (t * t) && k * t * t * t == k * (t * t * t) && k * t * t * t * t == k * (t * t * t * t)
        && k * t * t * t * t * t == k * (t * t * t * t * t)) by (nonlinear_arith);
    assert(0 <= k * (t * t) <= k * 40_000) by (nonlinear_arith)
        requires 0 <= k, 0 <= t * t <= 40_000;
    assert(0 <= k * (t * t * t) <= k * 8_000_000) by (nonlinear_arith)
        requires 0 <= k, 0 <= t * t * t <= 8_000_000;
    assert(0 <= k * (t * t * t * t) <= k * 1_600_000_000) by (nonlinear_arith)
        requires 0 <= k, 0 <= t * t * t * t <= 1_600_000_000;
    assert(0 <= k * (t * t * t * t * t) <= k * 320_000_000_000) by (nonlinear_arith)
        requires 0 <= k, 0 <= t * t * t * t * t <= 320_000_000_000;
}

/// [`WEIGHT_ONE`] as a number.
proof fn weight_one_value()
    ensures
        WEIGHT_ONE == 0x100_0000,
{
    assert((1u64 << 24u64) == 0x100_0000) by (bit_vector);
}

/// The weight of step `t - 1` is in `[0, WEIGHT_ONE]`, zero at `t = 200`
/// and positive before it.
proof fn weight_bounds(t: int, num: int)
    requires
        1 <= t <= 200,
        num == 6 * t * t * t * t * t - 15 * t * t * t * t * 200 + 10 * t * t * t * 200 * 200,
    ensures
        ({
            let v = ((320_000_000_000 - num) * 0x100_0000 + 160_000_000_000) / 320_000_000_000;
            &&& 0 <= v <= 0x100_0000
            &&& t < 200 ==> v > 0
            &&& t == 200 ==> v == 0
        }),
{
    let n = 200int;
    let m = n - t;
    // 1 - smootherstep(x) = smootherstep(1 - x): the complement is the
    // same polynomial in `m = n - t`, and both cubic factors are positive.
    assert(320_000_000_000 - num == m * m * m * (6 * m * m - 15 * m * n + 10 * n * n)) by (nonlinear_arith)
        requires m == n - t, n == 200, num == 6 * t * t * t * t * t - 15 * t * t * t * t * 200 + 10 * t * t * t * 200 * 200;
    assert(num == t * t * t * (6 * t * t - 15 * t * n + 10 * n * n)) by (nonlinear_arith)
        requires n == 200, num == 6 * t * t * t * t * t - 15 * t * t * t * t * 200 + 10 * t * t * t * 200 * 200;
    assert(16 * (6 * m * m - 15 * m * n + 10 * n * n) == 6 * (4 * m - 5 * n) * (4 * m - 5 * n) + 10 * n * n) by (nonlinear_arith);
    assert(16 * (6 * t * t - 15 * t * n + 10 * n * n) == 6 * (4 * t - 5 * n) * (4 * t - 5 * n) + 10 * n * n) by (nonlinear_arith);
    assert(0 <= (4 * m - 5 * n) * (4 * m - 5 * n)) by (nonlinear_arith);
    assert(0 <= (4 * t - 5 * n) * (4 * t - 5 * n)) by (nonlinear_arith);
    assert(0 <= m * m * m) by (nonlinear_arith)
        requires 0 <= m;
    assert(0 < t * t * t) by (nonlinear_arith)
        requires 1 <= t;
    let cm = 6 * m * m - 15 * m * n + 10 * n * n;
    let ct = 6 * t * t - 15 * t * n + 10 * n * n;
    assert(cm >= 25_000) by (nonlinear_arith)
        requires
            n == 200,
            cm == 6 * m * m - 15 * m * n + 10 * n * n,
            16 * (6 * m * m - 15 * m * n + 10 * n * n) == 6 * (4 * m - 5 * n) * (4 * m - 5 * n) + 10 * n * n,
            0 <= (4 * m - 5 * n) * (4 * m - 5 * n);
    assert(ct >= 25_000) by (nonlinear_arith)
        requires
            n == 200,
            ct == 6 * t * t - 15 * t * n + 10 * n * n,
            16 * (6 * t * t - 15 * t * n + 10 * n * n) == 6 * (4 * t - 5 * n) * (4 * t - 5 * n) + 10 * n * n,
            0 <= (4 * t - 5 * n) * (4 * t - 5 * n);
    assert(0 <= num) by (nonlinear_arith)
        requires num == t * t * t * ct, 0 < t * t * t, ct >= 25_000;
    assert(0 <= 320_000_000_000 - num) by (nonlinear_arith)
        requires 320_000_000_000 - num == m * m * m * cm, 0 <= m * m * m, cm >= 25_000;
    let x = (320_000_000_000 - num) * 0x100_0000 + 160_000_000_000;
    if t < 200 {
        assert(1 <= m * m * m) by (nonlinear_arith)
            requires 1 <= m;
        assert(25_000 <= 320_000_000_000 - num) by (nonlinear_arith)
            requires 320_000_000_000 - num == m * m * m * cm, 1 <= m * m * m, cm >= 25_000;
    }
    assert(x <= 320_000_000_000 * 0x100_0000 + 160_000_000_000) by (nonlinear_arith)
        requires x == (320_000_000_000 - num) * 0x100_0000 + 160_000_000_000, 0 <= num;
    assert(0 <= x);
}

/// The two ways [`Curve::step`] scales an age under its period: an age
/// past `u64::MAX / STEPS` is under a period of at least `STEPS`, so the
/// step is non-zero; any other age times `STEPS` fits in `u64`.
proof fn step_scaling(age: u64, decay_ns: u64)
    requires
        age < decay_ns,
    ensures
        age > u64::MAX / 200 ==> decay_ns / 200 > 0,
        age <= u64::MAX / 200 ==> age * 200 <= u64::MAX,
        decay_ns > 0,
{
    assert(u64::MAX / 200 == 92_233_720_368_547_758);
    if age <= u64::MAX / 200 {
        assert(age * 200 <= 92_233_720_368_547_758 * 200) by (nonlinear_arith)
            requires
                age <= 92_233_720_368_547_758int;
    }
}

/// The decay curve of one period (design 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Curve {
    decay_ns: u64,
}

impl Curve {
    pub closed spec fn period(&self) -> u64 {
        self.decay_ns
    }

    pub const fn new(decay_ns: u64) -> (c: Curve)
        ensures
            c.period() == decay_ns,
    {
        Curve { decay_ns }
    }

    /// Length of one step; 0 for a zero period, where everything is past
    /// the window at once.
    #[inline]
    pub const fn step_ns(&self) -> (ns: u64)
        ensures
            ns == self.period() / STEPS as u64,
    {
        self.decay_ns / STEPS as u64
    }

    /// The step something dirty since `dirty_at` is in at `now`:
    /// `STEPS - 1` (weight zero) for anything in or past the last step.
    pub fn step(&self, now: u64, dirty_at: u64) -> (s: usize)
        ensures
            s < STEPS,
            now - dirty_at >= self.period() ==> s == STEPS - 1,
    {
        let age = now.saturating_sub(dirty_at);
        if age >= self.decay_ns {
            return STEPS - 1;
        }
        // `u64` only: the 128-bit division helper on aarch64 drags
        // `rust_eh_personality` into the `no_std` library. An age too large
        // to scale is under a period whose step is at least `u64::MAX /
        // STEPS`, so dividing by the step loses nothing that matters.
        proof {
            step_scaling(age, self.decay_ns);
        }
        let s = if age > u64::MAX / STEPS as u64 {
            age / self.step_ns()
        } else {
            age * STEPS as u64 / self.decay_ns
        };
        (s as usize).min(STEPS - 1)
    }

    /// Weight of `step`, in units of [`WEIGHT_ONE`].
    #[inline]
    pub fn weight(step: usize) -> (w: u64)
        requires
            step < STEPS,
        ensures
            w <= u32::MAX,
    {
        WEIGHTS[step] as u64
    }

    /// Units (slabs, pages) that may stay dirty given `weighted`, the sum
    /// of `units * weight(step)` over everything dirty: the curve's value
    /// rounded up, so a lone slab is not purged before its cohort's weight
    /// reaches zero (jemalloc rounds its page count down; here the unit is
    /// a slab of 16 pages, and a purge-and-refault of it for one unit of
    /// rounding is not worth it).
    #[inline]
    pub fn allowance(weighted: u64) -> (units: usize)
        ensures
            units == (weighted + WEIGHT_ONE - 1) / WEIGHT_ONE as int,
    {
        proof {
            weight_one_value();
        }
        crate::vspec::div_ceil_u64(weighted, WEIGHT_ONE) as usize
    }

    /// [`allowance`](Curve::allowance) under the cgroup policy's
    /// retention cap (design 11): with `keep16` below
    /// [`KEEP_ALL`](crate::policy::KEEP_ALL), the smaller of that and
    /// `keep16`'s share of the `total` units.
    #[inline(always)]
    pub fn capped_allowance(weighted: u64, total: usize, keep16: u32) -> (units: usize)
        ensures
            keep16 >= crate::policy::KEEP_ALL ==> units == (weighted + WEIGHT_ONE - 1) / WEIGHT_ONE as int,
            keep16 < crate::policy::KEEP_ALL ==> units == vstd::math::min(
                (weighted + WEIGHT_ONE - 1) / WEIGHT_ONE as int,
                total * keep16 / 0x1_0000,
            ),
    {
        let allow = Curve::allowance(weighted);
        proof {
            crate::policy::frac_one_value();
        }
        if keep16 < crate::policy::KEEP_ALL {
            allow.min(crate::policy::mul16(total as u64, keep16) as usize)
        } else {
            allow
        }
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    /// The curve is jemalloc's: just under one at the start, zero only at
    /// the last step, half at mid-window, non-increasing, and symmetric
    /// about the middle.
    #[test]
    // Pairs each step with its mirror.
    #[allow(clippy::needless_range_loop)]
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
            assert!(
                sum.abs_diff(WEIGHT_ONE) <= 1,
                "steps {s} and {mirror}: {sum}"
            );
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
        assert_eq!(
            decay_ns_from_ms(crate::config::MAX_INTERVAL_MS),
            1_000_000_000_000_000_000
        );
        assert_ne!(decay_ns_from_ms(i64::MAX), NEVER);
    }
}
