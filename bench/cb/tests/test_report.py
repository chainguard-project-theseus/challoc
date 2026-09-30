"""End-to-end analysis on synthetic samples (no processes run)."""

import json
import os
import random
import tempfile
import unittest

from cb import plan, report, spec


def synth_run(tmp, effects, blocks=6, sigma=0.004, seed=0, broken_row=None, run="r1"):
    """A run directory like cb run writes, with head's score moved by
    effects[row] percent (positive = worse) against base."""
    rows = spec.load_rows()
    rng = random.Random(seed)
    suite = spec.load_suite("quick", rows)
    specs = [plan.parse_variant(v, {"base": "b", "head": "h"}) for v in ("base", "head")] + [plan.parse_variant("jemalloc", {}, "control")]
    p = plan.make_plan(suite, rows, specs, run_id=run, cell="local", blocks=blocks, warmup_blocks=1, seed=seed)
    d = os.path.join(tmp, run)
    os.makedirs(d)
    key = "local:test/cpu/6.0/img/default"
    with open(os.path.join(d, "samples.jsonl"), "w") as f, open(os.path.join(d, "blocks.jsonl"), "w") as fb:
        for b in p.blocks:
            fb.write(json.dumps({"vm": "local:t", "run": run, "block": b.index, "warmup": b.warmup, "sentinel": {"total_ms": 50 + rng.random()}, "steal_pct": 0.0}) + "\n")
            for r in b.rows:
                row = rows.rows[r]
                base = 10.0
                for pos, lab in enumerate(b.order[r]):
                    eff = effects.get(r, 0.0) if lab == "head" else (-3.0 if lab == "jemalloc" else 0.0)
                    drift = 1 + 0.002 * pos  # linear drift inside the block
                    noise = rng.gauss(0, sigma)
                    worse = (1 + eff / 100) * drift
                    score = base / worse if row.higher_is_better else base * worse
                    score *= 1 + noise
                    s = {
                        "run": run,
                        "cell": "local",
                        "comparability": key,
                        "vm": {"name": "local:t"},
                        "block": b.index,
                        "pos": pos,
                        "warmup": b.warmup,
                        "row": r,
                        "variant": lab,
                        "score": None if (broken_row == r and lab == "head" and b.index == 2) else score,
                        "error": "exit 139" if (broken_row == r and lab == "head" and b.index == 2) else "",
                        "unit": row.unit,
                        "better": row.better,
                        "rt": {"maxrss_kib": 20480, "exit": 0},
                        "ops": 1e6,
                        "pmc": {"instructions": 5e8 * (1 + (eff / 100 if lab == "head" else 0)), "cycles": 1e9},
                    }
                    f.write(json.dumps(s) + "\n")
    manifest = {"run": run, "suite": "quick", "variants": [{"label": v.label, "kind": v.kind, "source": v.source, "conf": "", "role": v.role, "commit": "0" * 40, "lib": "", "lib_sha256": ""} for v in specs]}
    json.dump(manifest, open(os.path.join(d, "manifest.json"), "w"))
    json.dump(p.to_json(), open(os.path.join(d, "plan.json"), "w"))
    json.dump({"indicative": True, "cpu": {}, "thp": {}}, open(os.path.join(d, "host.json"), "w"))
    return d


class Analysis(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.rows = spec.load_rows()

    def verdicts(self, s, kind="gate"):
        return {r["row"]: r["verdict"] for r in s["results"] if r["kind"] == kind}

    def test_aa_is_neutral(self):
        d = synth_run(self.tmp, {})
        s = report.analyze_dirs([d], self.rows)
        v = self.verdicts(s)
        self.assertEqual(set(v.values()) - {"same", "sub-threshold"}, set(), v)
        self.assertEqual(s["headline"], "NEUTRAL")
        self.assertEqual(s["exit"], 0)
        # the drift inside each block cancelled: estimates near zero
        for r in s["results"]:
            if r["kind"] == "gate":
                self.assertLess(abs(r["est_pct"]), 0.5, r)

    def test_regression(self):
        d = synth_run(self.tmp, {"larson1": 6.0})
        s = report.analyze_dirs([d], self.rows)
        v = self.verdicts(s)
        self.assertEqual(v["larson1"], "worse")
        self.assertEqual(s["headline"], "REGRESSION")
        self.assertEqual(s["exit"], 1)
        r = next(r for r in s["results"] if r["row"] == "larson1" and r["kind"] == "gate")
        self.assertAlmostEqual(r["est_pct"], 6.0, delta=0.5)
        self.assertAlmostEqual(r["pmc_delta"]["instructions"]["est_pct"], 6.0, delta=0.1)

    def test_improvement(self):
        d = synth_run(self.tmp, {"alloc-test1": -8.0})
        s = report.analyze_dirs([d], self.rows)
        self.assertEqual(self.verdicts(s)["alloc-test1"], "better")
        self.assertEqual(s["headline"], "IMPROVEMENT")

    def test_broken(self):
        d = synth_run(self.tmp, {}, broken_row="xmalloc")
        s = report.analyze_dirs([d], self.rows)
        self.assertEqual(self.verdicts(s)["xmalloc"], "broken")
        self.assertEqual(s["headline"], "BROKEN")

    def test_context_against_control(self):
        d = synth_run(self.tmp, {})
        s = report.analyze_dirs([d], self.rows)
        ctx = self.verdicts(s, "context")
        # jemalloc is 3% better than base/head everywhere: head is worse
        self.assertEqual(ctx["larson1"], "worse")
        self.assertNotIn("churn1024", {r["row"] for r in s["results"] if r["kind"] == "context" and r["candidate"] == "glibc"})

    def test_pooling_two_runs(self):
        a = synth_run(self.tmp, {}, seed=1, run="r1")
        b = synth_run(self.tmp, {}, seed=2, run="r2")
        s = report.analyze_dirs([a, b], self.rows)
        r = next(r for r in s["results"] if r["kind"] == "gate")
        self.assertEqual(r["n_vm"], 2)
        self.assertEqual(r["n_blocks"], 12)

    def test_render(self):
        d = synth_run(self.tmp, {"valkey1": 5.0})
        text = report.render(report.analyze_dirs([d], self.rows))
        self.assertIn("| valkey1 |", text)
        self.assertIn("**REGRESSION**", text)


if __name__ == "__main__":
    unittest.main()
