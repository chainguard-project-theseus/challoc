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


if __name__ == "__main__":
    unittest.main()
