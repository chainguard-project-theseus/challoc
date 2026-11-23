"""The coverage rows (mimalloc-bench's rest, applications, startup,
sidecar, consumer runtimes), the regex parser, the maxrss metric and the
jemalloc release-tarball variants."""

import os
import unittest

from cb import plan as planmod
from cb import report
from cb import rows as rowparse
from cb import spec


class CoverageRows(unittest.TestCase):
    def setUp(self):
        self.r = spec.load_rows()

    def test_new_rows_exist(self):
        for name in (
            "larsonN-sized", "cache-scratch1", "cache-scratchN", "cache-thrash1", "cache-thrashN",
            "malloc-large", "rptestN", "sh6benchN", "sh8benchN", "glibc-thread", "barnes", "gs",
            "z3", "lua", "rbstress1", "rbstressN", "mleak10", "mleak100",
            "rocksdb", "nginx-proxy", "compile-sqlite", "ort-mlp",
            "startup-true", "startup-cc", "startup-py", "startup-ruby", "startup-node", "startup-java",
            "sidecar-fluentbit", "ruby-gc-churn", "node-server", "pg-select100", "pg-connect20",
        ):
            self.assertIn(name, self.r.rows)

    def test_drivers_exist(self):
        for row in self.r.rows.values():
            if row.is_driver:
                self.assertTrue(os.path.exists(os.path.join(spec.BENCH_DIR, row.driver)), row.driver)

    def test_jemalloc_linked_programs_skip_glibc(self):
        # Wolfi's ruby and fluent-bit link libjemalloc.so.2: only builds
        # with that soname replace it.
        for name in ("rbstress1", "ruby-binary-trees", "startup-ruby", "sidecar-fluentbit"):
            row = self.r.rows[name]
            self.assertEqual(row.features, "jemalloc-soname", name)
            self.assertNotIn("glibc", row.allocs, name)
            glibc = planmod.parse_variant("glibc", {})
            self.assertFalse(glibc.can_run(row.requires, row.allocs))
            je54 = planmod.parse_variant("jemalloc54", {})
            self.assertTrue(je54.can_run(row.requires, row.allocs))

    def test_suites(self):
        full = spec.load_suite("full", self.r)
        apps = spec.load_suite("apps", self.r)
        quick = spec.load_suite("quick", self.r)
        self.assertTrue(set(apps.rows) <= set(full.rows) | {"lua", "gs", "z3", "valkey1"})
        self.assertEqual(full.variants, ["jemalloc54", "challoc=head"])
        self.assertEqual(full.controls, ["glibc"])
        # quick is unchanged by the coverage rows.
        self.assertEqual(quick.rows, ["larson1", "larson10", "xmalloc", "alloc-test1", "valkey1", "valkey1-free", "churn1024"])
        for name in full.rows:
            self.assertNotEqual(self.r.rows[name].placement, "one-variant-per-vm", name)
        side = spec.load_suite("sidecar", self.r)
        self.assertEqual(side.container["memory"], "512m")

    def test_jemalloc_tarball_variants(self):
        for name, ver in (("jemalloc53", "5.3.0"), ("jemalloc54", "5.4.0")):
            v = planmod.parse_variant(name, {})
            self.assertEqual(v.kind, "stock")
            self.assertTrue(v.mallctl)
            self.assertEqual(planmod.STOCK[name]["lib"], "{bin}/jemalloc-" + ver + "/libjemalloc.so.2")


class BenchPrograms(unittest.TestCase):
    def test_nested_paths_under_bin(self):
        # {bin}/inputs/ort-mlp.onnx is present when bin/inputs/ort-mlp.onnx
        # is, not bin/ort-mlp.onnx (a launch VM must not run build.sh).
        import tempfile

        from cb import run as runmod

        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "inputs"))
            open(os.path.join(d, "inputs", "ort-mlp.onnx"), "w").close()
            rows = [spec.load_rows().rows["ort-mlp"]]
            runmod.ensure_bench_programs("/nonexistent", d, rows, allow_build=False)
            os.unlink(os.path.join(d, "inputs", "ort-mlp.onnx"))
            with self.assertRaises(runmod.BuildError):
                runmod.ensure_bench_programs("/nonexistent", d, rows, allow_build=False)


class RegexParser(unittest.TestCase):
    def setUp(self):
        self.rows = spec.load_rows().rows

    def test_glibc_thread(self):
        p = rowparse.parse(self.rows["glibc-thread"], "{json}\n123456789 iterations\n", {"maxrss_kib": 2048})
        self.assertEqual(p.score, 123456789)
        self.assertEqual(p.extra["maxrss_mib"], 2.0)

    def test_rptest_last_match(self):
        out = "crt  32 threads random ... ops: .....7000 memory ops/CPU second (1MiB peak)\n8060541 memory ops/CPU second (197MiB peak)\n"
        self.assertEqual(rowparse.parse(self.rows["rptestN"], out, {}).score, 8060541)

    def test_no_match(self):
        p = rowparse.parse(self.rows["glibc-thread"], "nothing\n", {})
        self.assertIsNone(p.score)
        self.assertIn("no match", p.error)

    def test_bad_pattern(self):
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"cmd": ["a"], "parser": "regex", "pattern": "no group"}}})
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"cmd": ["a"], "parser": "regex"}}})
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"cmd": ["a"], "parser": "regex", "pattern": "(("}}})


class MaxrssMetric(unittest.TestCase):
    def test_timed_row_metric_expands(self):
        r = spec.load_rows()
        p = rowparse.parse(r.rows["mleak100"], "", {"wall_s": 1.5, "maxrss_kib": 10240})
        self.assertEqual(p.extra["maxrss_mib"], 10.0)
        s = {"row": "mleak100", "score": p.score, "extra": p.extra, "error": ""}
        out = report.expand_metrics([s], r)
        m = [x for x in out if x["row"] == "mleak100.maxrss_mib"]
        self.assertEqual(len(m), 1)
        self.assertEqual(m[0]["score"], 10.0)
        self.assertEqual(m[0]["better"], "lower")


class ZeroMetric(unittest.TestCase):
    def test_a_zero_is_broken_metric_breaks_its_row(self):
        # valkey-cow.sh's drain_ended is 100 for a run whose cycles stopped
        # and 0 for one that hit DRAIN_WAIT. The 0 is a broken sample: the
        # comparison of a mix of ended and capped runs reports the row
        # broken, naming the capped runs, in both placements, and never
        # takes the log of 0. A metric without `zero_is_broken` still
        # drops a 0 (not reported).
        r = spec.load_rows()
        runs = []
        for vm, lab, e in (("a", "base", 100), ("b", "base", 100), ("c", "base", 100),
                           ("d", "head", 100), ("e", "head", 0), ("f", "head", 0)):
            for blk, var in ((1, "base"), (2, "head")):
                runs.append({"run": "r", "vm": {"name": vm}, "block": blk, "row": "valkey-cow-drain", "variant": var,
                             "score": 800000.0, "error": "", "better": "higher", "unit": "req/s",
                             "extra": {"drain_ended": e if var == lab else 100, "drain_moves_s": 30.0, "drain_hits_s": 0}})
        out = report.expand_metrics(runs, r)
        ended = [x for x in out if x["row"] == "valkey-cow-drain.drain_ended"]
        self.assertEqual(len(ended), 12)
        capped = [x for x in ended if x["error"]]
        self.assertEqual(len(capped), 2)
        self.assertTrue(all(x["score"] is None and x["error"] == "drain_ended 0" for x in capped))
        self.assertFalse([x for x in out if x["row"] == "valkey-cow-drain.drain_hits_s"])
        for placement in ("paired", "one-variant-per-vm"):
            c = report.compare_row(out, "valkey-cow-drain.drain_ended", "head", "base", delta_pct=2.0, alpha=0.05,
                                   flagged=set(), excluded={}, fixed_work=False, placement=placement)
            self.assertEqual(c["verdict"], "broken", placement)
            self.assertIn("head block 2: drain_ended 0", c["broken"])


if __name__ == "__main__":
    unittest.main()
