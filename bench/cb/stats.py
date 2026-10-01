"""Statistics for paired allocator comparisons (stdlib only).

The unit of analysis is one block on one host ("VM"): within the block each
variant ran at mirror positions (ABBA, or a Williams sequence and its
reverse), so the block's contrast

    d = mean(x_cand) - mean(x_ref),   x = ln(score)   (lower is better)
                                      x = -ln(score)  (higher is better)

cancels linear drift and the host's offset, and is oriented so that d > 0
means the candidate did worse. For a 2-variant ABBA block this is exactly
1/2 [(ln B1 - ln A1) + (ln B2 - ln A2)] with the sign of "worse".

Estimate: the mean over hosts of each host's mean d, reported as a
percentage 100 (e^d - 1). CI: the Student-t interval on the k per-host
means, df = k - 1. A host's mean carries both the within-host block noise
and the host x variant interaction, so the interval covers both whatever
the number of blocks per host, with no pre-test choosing between models.
With one host there is no host-to-host spread to measure: the interval is
the t-interval on that host's blocks (df = n - 1), flagged "VM effect
unestimated" (a local run; indicative only). Cochran's Q and I^2 (pooled
within-host variance) still flag a host-dependent effect and name the
outlying hosts; they do not change the interval.

Verdicts against the margin m (percent), CI at level 1 - alpha:
    broken         any run of the row failed (nonzero exit, no score, a check)
    worse          CI lower bound > 0 and estimate > m
    better         CI upper bound < 0 and estimate < -m
    same           CI inside (-m, m), not significant
    sub-threshold  CI inside (-m, m) and excluding 0 (a real but small change)
    inconclusive   anything else
Headline over the gate rows (Holm across them for worse/better):
    BROKEN > REGRESSION > IMPROVEMENT > NEUTRAL > INCONCLUSIVE.
"""

from __future__ import annotations

import math
import statistics
from typing import Iterable, Sequence

# ---- special functions ---------------------------------------------------------


def _betacf(a: float, b: float, x: float) -> float:
    # Lentz's continued fraction for the incomplete beta (Numerical Recipes).
    tiny = 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 300):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        de = d * c
        h *= de
        if abs(de - 1.0) < 1e-14:
            break
    return h


def betainc(a: float, b: float, x: float) -> float:
    """Regularized incomplete beta I_x(a, b)."""
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    lbeta = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log1p(-x)
    if x < (a + 1.0) / (a + b + 2.0):
        return math.exp(lbeta) * _betacf(a, b, x) / a
    return 1.0 - math.exp(lbeta) * _betacf(b, a, 1.0 - x) / b


def t_sf(t: float, df: float) -> float:
    """P(T > t) for Student's t with df degrees of freedom."""
    x = df / (df + t * t)
    p = 0.5 * betainc(df / 2.0, 0.5, x)
    return p if t >= 0 else 1.0 - p


def t_ppf(q: float, df: float) -> float:
    """The q quantile of Student's t (bisection on t_sf)."""
    if not 0.0 < q < 1.0:
        raise ValueError("q must be in (0, 1)")
    if q == 0.5:
        return 0.0
    target = 1.0 - q  # P(T > t) = 1 - q
    lo, hi = -1e3, 1e3
    for _ in range(200):
        mid = (lo + hi) / 2.0
        if t_sf(mid, df) > target:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2.0


def gammainc_upper(s: float, x: float) -> float:
    """Regularized upper incomplete gamma Q(s, x)."""
    if x <= 0:
        return 1.0
    if x < s + 1.0:
        # series for P, then Q = 1 - P
        term = 1.0 / s
        total = term
        n = s
        for _ in range(1000):
            n += 1.0
            term *= x / n
            total += term
            if abs(term) < abs(total) * 1e-15:
                break
        return max(0.0, 1.0 - total * math.exp(-x + s * math.log(x) - math.lgamma(s)))
    # continued fraction for Q
    tiny = 1e-300
    b = x + 1.0 - s
    c = 1.0 / tiny
    d = 1.0 / b
    h = d
    for i in range(1, 1000):
        an = -i * (i - s)
        b += 2.0
        d = an * d + b
        d = d if abs(d) > tiny else tiny
        c = b + an / c
        c = c if abs(c) > tiny else tiny
        d = 1.0 / d
        de = d * c
        h *= de
        if abs(de - 1.0) < 1e-15:
            break
    return math.exp(-x + s * math.log(x) - math.lgamma(s)) * h


def chi2_sf(x: float, df: float) -> float:
    return gammainc_upper(df / 2.0, x / 2.0)


# ---- orientation and units -----------------------------------------------------


def orient(score: float, higher_is_better: bool) -> float:
    """The log score oriented so that larger means worse."""
    return -math.log(score) if higher_is_better else math.log(score)


def to_pct(d: float) -> float:
    return (math.exp(d) - 1.0) * 100.0


def from_pct(p: float) -> float:
    return math.log1p(p / 100.0)


# ---- the paired estimate and its CI ----------------------------------------------

Groups = dict[str, list[float]]  # host -> block contrasts


def estimate(groups: Groups) -> float:
    """Mean over hosts of each host's mean contrast."""
    means = [statistics.fmean(v) for v in groups.values() if v]
    return statistics.fmean(means) if means else float("nan")


def _quantile(sorted_xs: Sequence[float], q: float) -> float:
    n = len(sorted_xs)
    if n == 1:
        return sorted_xs[0]
    pos = q * (n - 1)
    i = int(math.floor(pos))
    j = min(i + 1, n - 1)
    return sorted_xs[i] + (sorted_xs[j] - sorted_xs[i]) * (pos - i)


def t_interval(xs: Sequence[float], level: float) -> tuple[float, float, float]:
    """Student-t interval on the mean of xs (df = n - 1). Returns (lo, hi,
    two-sided p for a mean of 0)."""
    n = len(xs)
    if n < 2:
        return (-math.inf, math.inf, 1.0)
    est = statistics.fmean(xs)
    se = statistics.stdev(xs) / math.sqrt(n)
    if se == 0:
        return (est, est, 0.0 if est != 0 else 1.0)
    tq = t_ppf(1.0 - (1.0 - level) / 2.0, n - 1)
    p = 2.0 * t_sf(abs(est) / se, n - 1)
    return (est - tq * se, est + tq * se, min(1.0, p))


def f_sf(f: float, d1: float, d2: float) -> float:
    """P(F > f) for the F distribution with (d1, d2) degrees of freedom."""
    if f <= 0:
        return 1.0
    return betainc(d2 / 2.0, d1 / 2.0, d2 / (d2 + d1 * f))


def cochran_q(groups: Groups) -> tuple[float, float, float]:
    """Heterogeneity of the per-host mean contrasts: Cochran's Q with I^2.
    The within-host variance is pooled over hosts (the blocks of every
    host are the same experiment), so with 3 blocks per host the weights
    are not the noisy per-host variances that inflate Q; Q is then
    (k - 1) F of the one-way ANOVA and its p comes from F(k - 1, N - k).
    Returns (Q, p, I2); p = 1 and I2 = 0 when it cannot be computed."""
    gs = [v for v in groups.values() if len(v) >= 2]
    k = len(gs)
    n_tot = sum(len(v) for v in gs)
    if k < 2 or n_tot - k < 1:
        return (0.0, 1.0, 0.0)
    ss_within = sum((len(v) - 1) * statistics.variance(v) for v in gs)
    s2 = ss_within / (n_tot - k)
    if s2 <= 0:
        return (0.0, 1.0, 0.0)
    grand = sum(sum(v) for v in gs) / n_tot
    q = sum(len(v) * (statistics.fmean(v) - grand) ** 2 for v in gs) / s2
    df = k - 1
    p = f_sf(q / df, df, n_tot - k)
    i2 = max(0.0, (q - df) / q) if q > 0 else 0.0
    return (q, p, i2)


def outlying_hosts(groups: Groups, z: float = 2.0) -> list[str]:
    """Hosts whose mean contrast is more than z standard errors from the
    mean of the others."""
    out = []
    for h, v in groups.items():
        others = [x for g, vv in groups.items() if g != h for x in vv]
        if len(v) < 2 or len(others) < 2:
            continue
        se = math.sqrt(statistics.variance(v) / len(v) + statistics.variance(others) / len(others))
        if se > 0 and abs(statistics.fmean(v) - statistics.fmean(others)) / se > z:
            out.append(h)
    return out


class Comparison:
    """The paired comparison of one row: candidate vs reference."""

    def __init__(self, groups: Groups, *, level: float):
        self.groups = {h: v for h, v in groups.items() if v}
        self.n_hosts = len(self.groups)
        self.n_blocks = sum(len(v) for v in self.groups.values())
        self.level = level
        self.flags: list[str] = []
        self.outliers: list[str] = []
        self.q, self.q_p, self.i2 = cochran_q(self.groups)
        if self.n_blocks == 0:
            self.est = float("nan")
            self.lo, self.hi, self.p = -math.inf, math.inf, 1.0
            return
        self.est = estimate(self.groups)
        self.host_dependent = self.n_hosts >= 3 and (self.q_p < 0.10 or self.i2 > 0.5)
        if self.host_dependent:
            self.flags.append("host-dependent")
            self.outliers = outlying_hosts(self.groups)
        if self.n_hosts >= 2:
            self.lo, self.hi, self.p = t_interval([statistics.fmean(v) for v in self.groups.values()], level)
        else:
            self.flags.append("VM effect unestimated")
            self.lo, self.hi, self.p = t_interval(next(iter(self.groups.values())), level)

    @property
    def est_pct(self) -> float:
        return to_pct(self.est)

    @property
    def ci_pct(self) -> tuple[float, float]:
        return (to_pct(self.lo) if self.lo > -math.inf else -100.0, to_pct(self.hi) if self.hi < math.inf else math.inf)


class Unpaired:
    """The unpaired comparison of one row (one-variant-per-vm soaks): each
    VM contributes the mean oriented log score of the one variant it ran;
    Welch's t interval on the log scale across VMs. Same interface as
    Comparison."""

    def __init__(self, cand: list[float], ref: list[float], *, level: float):
        self.n_hosts = len(cand) + len(ref)
        self.n_blocks = self.n_hosts
        self.level = level
        self.flags = ["unpaired (one variant per VM)"]
        self.outliers: list[str] = []
        self.q = self.q_p = self.i2 = float("nan")
        if not cand or not ref:
            self.est, self.lo, self.hi, self.p = float("nan"), -math.inf, math.inf, 1.0
            return
        self.est = statistics.fmean(cand) - statistics.fmean(ref)
        if len(cand) < 2 or len(ref) < 2:
            self.flags.append("fewer than 2 VMs per variant")
            self.lo, self.hi, self.p = -math.inf, math.inf, 1.0
            return
        vc, vr = statistics.variance(cand) / len(cand), statistics.variance(ref) / len(ref)
        se = math.sqrt(vc + vr)
        if se == 0:
            self.lo = self.hi = self.est
            self.p = 0.0 if self.est else 1.0
            return
        df = (vc + vr) ** 2 / ((vc**2 / (len(cand) - 1) if len(cand) > 1 else 0) + (vr**2 / (len(ref) - 1) if len(ref) > 1 else 0))
        t = t_ppf(1.0 - (1.0 - level) / 2.0, df)
        self.lo, self.hi = self.est - t * se, self.est + t * se
        self.p = min(1.0, 2.0 * t_sf(abs(self.est) / se, df))

    @property
    def est_pct(self) -> float:
        return to_pct(self.est)

    @property
    def ci_pct(self) -> tuple[float, float]:
        return (to_pct(self.lo) if self.lo > -math.inf else -100.0, to_pct(self.hi) if self.hi < math.inf else math.inf)


# ---- margins and verdicts ----------------------------------------------------------

MIN_CALIBRATION = 14


def margin(delta_pct: float, aa_estimates_pct: Iterable[float] = ()) -> tuple[float, bool]:
    """m = max(delta, q99 of |A/A estimates|) once MIN_CALIBRATION A/A
    results exist; delta and uncalibrated before that. The estimates are
    one row's on one cell (report.py keys the history by cell and row): a
    pooled list would let the noisiest row set every row's margin. Below
    100 estimates the q99 is close to their maximum."""
    aa = sorted(abs(x) for x in aa_estimates_pct)
    if len(aa) < MIN_CALIBRATION:
        return (delta_pct, False)
    return (max(delta_pct, _quantile(aa, 0.99)), True)


VERDICTS = ("broken", "worse", "better", "sub-threshold", "same", "inconclusive")


def verdict(est_pct: float, lo_pct: float, hi_pct: float, m: float, broken: bool = False) -> str:
    if broken:
        return "broken"
    if math.isnan(est_pct):
        return "inconclusive"
    if lo_pct > 0 and est_pct > m:
        return "worse"
    if hi_pct < 0 and est_pct < -m:
        return "better"
    if -m < lo_pct and hi_pct < m:
        return "sub-threshold" if (lo_pct > 0 or hi_pct < 0) else "same"
    return "inconclusive"


def holm(pvals: dict[str, float], alpha: float) -> dict[str, bool]:
    """Holm's step-down: which hypotheses are rejected at family-wise alpha."""
    order = sorted(pvals, key=lambda k: pvals[k])
    m = len(order)
    out = {k: False for k in pvals}
    for i, k in enumerate(order):
        if pvals[k] <= alpha / (m - i):
            out[k] = True
        else:
            break
    return out


def bh(pvals: dict[str, float]) -> dict[str, float]:
    """Benjamini-Hochberg q-values."""
    order = sorted(pvals, key=lambda k: pvals[k])
    m = len(order)
    q: dict[str, float] = {}
    prev = 1.0
    for rank in range(m, 0, -1):
        k = order[rank - 1]
        prev = min(prev, pvals[k] * m / rank)
        q[k] = prev
    return q


HEADLINE_EXIT = {"NEUTRAL": 0, "PASS": 0, "IMPROVEMENT": 0, "REGRESSION": 1, "BROKEN": 1, "INCONCLUSIVE": 2, "INFRA": 3}


def headline(verdicts: dict[str, str], pvals: dict[str, float], alpha: float) -> str:
    """The run's one-word answer over the gate rows."""
    if not verdicts:
        return "INFRA"
    if any(v == "broken" for v in verdicts.values()):
        return "BROKEN"
    rejected = holm(pvals, alpha)
    if any(v == "worse" and rejected.get(k) for k, v in verdicts.items()):
        return "REGRESSION"
    if any(v == "worse" for v in verdicts.values()):
        return "INCONCLUSIVE"
    if any(v == "better" and rejected.get(k) for k, v in verdicts.items()):
        return "IMPROVEMENT"
    if all(v in ("same", "sub-threshold", "better") for v in verdicts.values()):
        return "NEUTRAL"
    return "INCONCLUSIVE"


# ---- noise flags -------------------------------------------------------------------


def mad(xs: Sequence[float]) -> float:
    med = statistics.median(xs)
    return statistics.median([abs(x - med) for x in xs])


def outside_mad(xs: dict[int, float], k: float = 3.0) -> list[int]:
    """Keys whose value is more than k MAD from the median (MAD 0: none)."""
    vals = list(xs.values())
    if len(vals) < 4:
        return []
    med = statistics.median(vals)
    m = mad(vals)
    if m == 0:
        return []
    return [i for i, v in xs.items() if abs(v - med) > k * 1.4826 * m]


def bimodal(xs: Sequence[float], min_n: int = 6, min_spread: float = 0.02) -> bool:
    """Sorted-gap test: the largest gap between neighbours is more than half
    of the range, splits the sample into two groups of at least two, and is
    more than 4x the median gap; a range under min_spread of the median
    (quantised scores such as an occupancy) never counts."""
    s = sorted(xs)
    n = len(s)
    if n < min_n or s[-1] == s[0]:
        return False
    if (s[-1] - s[0]) < min_spread * abs(statistics.median(s)):
        return False
    gaps = [s[i + 1] - s[i] for i in range(n - 1)]
    g = max(gaps)
    i = gaps.index(g)
    med = statistics.median(gaps)
    return g > 0.5 * (s[-1] - s[0]) and 2 <= i + 1 <= n - 2 and (med == 0 or g > 4 * med)


def spearman(xs: Sequence[float], ys: Sequence[float]) -> float:
    def ranks(v: Sequence[float]) -> list[float]:
        idx = sorted(range(len(v)), key=lambda i: v[i])
        r = [0.0] * len(v)
        i = 0
        while i < len(idx):
            j = i
            while j + 1 < len(idx) and v[idx[j + 1]] == v[idx[i]]:
                j += 1
            for t in range(i, j + 1):
                r[idx[t]] = (i + j) / 2.0 + 1
            i = j + 1
        return r

    if len(xs) < 3:
        return 0.0
    rx, ry = ranks(xs), ranks(ys)
    mx, my = statistics.fmean(rx), statistics.fmean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    den = math.sqrt(sum((a - mx) ** 2 for a in rx) * sum((b - my) ** 2 for b in ry))
    return num / den if den else 0.0
