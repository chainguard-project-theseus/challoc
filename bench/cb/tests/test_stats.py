import math
import random
import unittest

from cb import stats


class SpecialFunctions(unittest.TestCase):
    def test_t_quantiles(self):
        # Reference values (two-sided 95%: t_{0.975, df}).
        for df, want in ((1, 12.7062), (2, 4.3027), (5, 2.5706), (10, 2.2281), (30, 2.0423), (1000, 1.9623)):
            self.assertAlmostEqual(stats.t_ppf(0.975, df), want, places=3)
        self.assertAlmostEqual(stats.t_ppf(0.5, 7), 0.0)
        self.assertAlmostEqual(stats.t_ppf(0.025, 5), -2.5706, places=3)

    def test_t_sf(self):
        self.assertAlmostEqual(stats.t_sf(0.0, 4), 0.5)
        self.assertAlmostEqual(stats.t_sf(2.7764, 4), 0.025, places=4)

    def test_chi2_sf(self):
        self.assertAlmostEqual(stats.chi2_sf(3.841, 1), 0.05, places=3)
        self.assertAlmostEqual(stats.chi2_sf(18.307, 10), 0.05, places=3)
        self.assertAlmostEqual(stats.chi2_sf(0.0, 3), 1.0)

    def test_betainc_symmetry(self):
        self.assertAlmostEqual(stats.betainc(2.0, 3.0, 0.4) + stats.betainc(3.0, 2.0, 0.6), 1.0, places=10)


class Orientation(unittest.TestCase):
    def test_orient_positive_is_worse(self):
        # Throughput down 10% and time up 10% are both positive.
        self.assertGreater(stats.orient(90, True) - stats.orient(100, True), 0)
        self.assertGreater(stats.orient(1.1, False) - stats.orient(1.0, False), 0)
        self.assertAlmostEqual(stats.to_pct(stats.from_pct(3.0)), 3.0)


class Verdicts(unittest.TestCase):
    def test_rules(self):
        v = stats.verdict
        self.assertEqual(v(5.0, 3.0, 7.0, 1.0), "worse")
        self.assertEqual(v(-5.0, -7.0, -3.0, 1.0), "better")
        self.assertEqual(v(0.1, -0.5, 0.7, 1.0), "same")
        self.assertEqual(v(0.5, 0.2, 0.8, 1.0), "sub-threshold")
        self.assertEqual(v(-0.5, -0.8, -0.2, 1.0), "sub-threshold")
        # significant, estimate under the margin, CI reaching past it: not a pass
        self.assertEqual(v(0.8, 0.1, 3.0, 1.0), "inconclusive")
        # estimate past the margin but not significant
        self.assertEqual(v(2.0, -1.0, 5.0, 1.0), "inconclusive")
        self.assertEqual(v(0.0, -0.1, 0.1, 1.0, broken=True), "broken")
        self.assertEqual(v(float("nan"), -1, 1, 1.0), "inconclusive")

    def test_margin_calibration(self):
        self.assertEqual(stats.margin(1.0, [0.1] * 5), (1.0, False))
        m, cal = stats.margin(1.0, [0.2] * 13 + [3.0])
        self.assertTrue(cal)
        self.assertGreater(m, 1.0)
        self.assertEqual(stats.margin(2.0, [0.1] * 20), (2.0, True))

    def test_holm(self):
        r = stats.holm({"a": 0.001, "b": 0.02, "c": 0.04}, 0.05)
        self.assertEqual(r, {"a": True, "b": True, "c": True})
        r = stats.holm({"a": 0.001, "b": 0.03, "c": 0.04}, 0.05)
        self.assertEqual(r, {"a": True, "b": False, "c": False})

    def test_bh(self):
        q = stats.bh({"a": 0.01, "b": 0.04, "c": 0.03})
        self.assertAlmostEqual(q["a"], 0.03)
        self.assertAlmostEqual(q["b"], 0.04)
        self.assertAlmostEqual(q["c"], 0.04)

    def test_headline(self):
        h = stats.headline
        self.assertEqual(h({"a": "same", "b": "sub-threshold"}, {"a": 0.5, "b": 0.01}, 0.05), "NEUTRAL")
        self.assertEqual(h({"a": "worse", "b": "same"}, {"a": 0.001, "b": 0.5}, 0.05), "REGRESSION")
        self.assertEqual(h({"a": "better", "b": "same"}, {"a": 0.001, "b": 0.5}, 0.05), "IMPROVEMENT")
        self.assertEqual(h({"a": "inconclusive", "b": "same"}, {"a": 0.2, "b": 0.5}, 0.05), "INCONCLUSIVE")
        self.assertEqual(h({"a": "broken", "b": "better"}, {"a": 1, "b": 0.001}, 0.05), "BROKEN")
        # A worse that does not survive Holm is not a regression claim.
        self.assertEqual(h({"a": "worse", "b": "same", "c": "same"}, {"a": 0.04, "b": 0.001, "c": 0.002}, 0.03), "INCONCLUSIVE")
        self.assertEqual(stats.HEADLINE_EXIT["REGRESSION"], 1)
        self.assertEqual(stats.HEADLINE_EXIT["INCONCLUSIVE"], 2)


def synth(effect_pct, hosts, blocks, sigma=0.01, host_sd=0.0, seed=0):
    rng = random.Random(seed)
    d = stats.from_pct(effect_pct)
    g = {}
    for h in range(hosts):
        off = rng.gauss(0, host_sd)
        g[f"vm{h}"] = [d + off + rng.gauss(0, sigma) for _ in range(blocks)]
    return g


class Comparisons(unittest.TestCase):
    def test_detects_regression(self):
        c = stats.Comparison(synth(5.0, 4, 3, sigma=0.01), level=0.97, resamples=2000)
        lo, hi = c.ci_pct
        self.assertGreater(lo, 0)
        self.assertAlmostEqual(c.est_pct, 5.0, delta=1.0)
        self.assertEqual(stats.verdict(c.est_pct, lo, hi, 2.0), "worse")

    def test_null_is_same(self):
        c = stats.Comparison(synth(0.0, 4, 3, sigma=0.003), level=0.97, resamples=2000)
        lo, hi = c.ci_pct
        self.assertEqual(stats.verdict(c.est_pct, lo, hi, 1.0), "same")

    def test_single_host_flag(self):
        c = stats.Comparison(synth(0.0, 1, 6), level=0.95, resamples=500)
        self.assertIn("VM effect unestimated", c.flags)
        self.assertIn("few blocks", c.flags)

    def test_host_dependence(self):
        g = synth(0.0, 6, 5, sigma=0.002)
        g["vm0"] = [x + 0.05 for x in g["vm0"]]
        c = stats.Comparison(g, level=0.95, resamples=500)
        self.assertIn("host-dependent", c.flags)
        self.assertIn("vm0", c.outliers)
        q, p, i2 = stats.cochran_q(g)
        self.assertLess(p, 0.10)
        self.assertGreater(i2, 0.5)

    def test_homogeneous_hosts(self):
        # With no host effect the test rejects at about its level, even
        # with the quick suite's 3 blocks per host.
        hits = sum(stats.cochran_q(synth(1.0, 4, 3, sigma=0.01, seed=s))[1] < 0.10 for s in range(300))
        self.assertLess(hits / 300, 0.16)

    def test_f_sf(self):
        self.assertAlmostEqual(stats.f_sf(3.0, 3, 20), 0.0548, places=3)

    def test_false_positive_rate(self):
        # A/A at alpha 0.05: the fraction of CIs that exclude 0 stays near
        # alpha even with only a handful of blocks (the t fallback).
        rejections = 0
        trials = 200
        for t in range(trials):
            c = stats.Comparison(synth(0.0, 1, 6, sigma=0.02, seed=100 + t), level=0.95, resamples=400, seed=t)
            if c.lo > 0 or c.hi < 0:
                rejections += 1
        self.assertLess(rejections / trials, 0.10)

    def test_empty(self):
        c = stats.Comparison({}, level=0.95)
        self.assertTrue(math.isnan(c.est))


class NoiseFlags(unittest.TestCase):
    def test_outside_mad(self):
        xs = {0: 50.0, 1: 51.0, 2: 49.5, 3: 50.5, 4: 80.0}
        self.assertEqual(stats.outside_mad(xs), [4])
        self.assertEqual(stats.outside_mad({0: 1.0, 1: 1.0, 2: 1.0, 3: 1.0}), [])

    def test_bimodal(self):
        self.assertTrue(stats.bimodal([1.0, 1.01, 1.02, 2.0, 2.01, 2.02]))
        self.assertFalse(stats.bimodal([1.0, 1.1, 1.2, 1.3, 1.4, 1.5]))

    def test_spearman(self):
        self.assertAlmostEqual(stats.spearman([1, 2, 3, 4], [10, 20, 30, 40]), 1.0)
        self.assertAlmostEqual(stats.spearman([1, 2, 3, 4], [4, 3, 2, 1]), -1.0)


if __name__ == "__main__":
    unittest.main()
