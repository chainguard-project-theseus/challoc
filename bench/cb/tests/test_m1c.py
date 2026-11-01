"""M1c: knob and container syntax, driver rows and their metric rows,
the unpaired (one-variant-per-vm) comparison."""

import json
import os
import random
import tempfile
import unittest

from cb import knobs, plan, report, spec, stats
from cb.variants import cargo_feature_args


class TestVariantSyntax(unittest.TestCase):
    refs = {"base": "b", "head": "h"}

    def test_knobs(self):
        v = plan.parse_variant("llc=head@slice_ext:off+knob:sched.llc_balancing=1+knob:thp.64kB=madvise", self.refs)
        self.assertEqual((v.label, v.source, v.conf), ("llc", "h", "slice_ext:off"))
        self.assertEqual(v.knobs, {"sched.llc_balancing": "1", "thp.64kB": "madvise"})
        v = plan.parse_variant("je=jemalloc+knob:thp=never", self.refs)
        self.assertEqual((v.kind, v.knobs), ("stock", {"thp": "never"}))
        v = plan.parse_variant({"name": "t", "src": "head", "knobs": {"sysctl.vm.overcommit_memory": 1}}, self.refs)
        self.assertEqual(v.knobs, {"sysctl.vm.overcommit_memory": "1"})

    def test_bad_knob(self):
        with self.assertRaises(spec.SpecError):
            plan.parse_variant("x=head+knob:nope=1", self.refs)
        with self.assertRaises(spec.SpecError):
            plan.parse_variant("x=head+knob:thp", self.refs)

    def test_knob_paths(self):
        self.assertEqual(knobs.path_of("thp"), "/sys/kernel/mm/transparent_hugepage/enabled")
        self.assertEqual(knobs.path_of("thp.64kB"), "/sys/kernel/mm/transparent_hugepage/hugepages-64kB/enabled")
        self.assertEqual(knobs.path_of("rseq.slice_ext_nsec"), "/sys/kernel/debug/rseq/slice_ext_nsec")
        self.assertEqual(knobs.path_of("sysctl.vm.max_map_count"), "/proc/sys/vm/max_map_count")

    def test_allocs(self):
        je = plan.parse_variant("jemalloc", self.refs)
        gl = plan.parse_variant("glibc", self.refs)
        hd = plan.parse_variant("head", self.refs)
        self.assertTrue(je.can_run([], ["challoc", "jemalloc"]))
        self.assertFalse(gl.can_run([], ["challoc", "jemalloc"]))
        self.assertTrue(hd.can_run([], ["challoc"]))

    def test_features(self):
        self.assertEqual(cargo_feature_args(""), [])
        self.assertEqual(cargo_feature_args("quantum8"), ["--no-default-features", "--features", "quantum8"])
        self.assertEqual(cargo_feature_args("jemalloc-soname"), ["--features", "jemalloc-soname"])


class TestContainer(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(spec.parse_container("privileged"), {"profile": "privileged"})
        c = spec.parse_container({"profile": "default", "cpus": 4, "cpuset": "0-3", "memory": "8g"})
        self.assertEqual(c, {"profile": "default", "cpus": "4", "memory": "8g", "cpuset": "0-3"})
        self.assertEqual(spec.container_key(c), "default,cpus=4,mem=8g,cpuset=0-3")
        with self.assertRaises(spec.SpecError):
            spec.parse_container("root")
        with self.assertRaises(spec.SpecError):
            spec.parse_container({"profile": "default", "gpus": 1})

    def test_launch_arg(self):
        from cb.gce import parse_container_arg

        self.assertEqual(parse_container_arg("privileged,cpuset=0-3"), {"profile": "privileged", "cpuset": "0-3"})
        self.assertEqual(parse_container_arg("cpus=2"), {"profile": "default", "cpus": "2"})

    def test_suite_container(self):
        rows = spec.load_rows()
        s = spec.load_suite("sidecar", rows)
        self.assertEqual(s.container, {"profile": "perfmon", "memory": "512m"})


class TestDriverRows(unittest.TestCase):
    def test_rows(self):
        rows = spec.load_rows()
        v = rows.rows["valkey-soak"]
        self.assertTrue(v.is_driver)
        self.assertEqual((v.placement, v.features, v.metric), ("one-variant-per-vm", "quantum8", "write_mt_rps"))
        m = rows.rows["valkey-soak.rss_settle_mib"]
        self.assertEqual((m.kind, m.parent, m.better, m.klass), ("metric", "valkey-soak", "lower", "soak-rss"))
        self.assertNotIn("valkey-soak.rss_settle_mib", spec.select_rows("all", rows))
        with self.assertRaises(spec.SpecError):
            spec.parse_suite("x", {"rows": ["valkey-soak.vmhwm_mib"], "variants": ["a=base", "b=base"]}, rows)

    def test_driver_needs_metric(self):
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"kind": "soak", "driver": "drivers/valkey-soak.sh"}}})
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"kind": "soak", "driver": "drivers/none.sh", "metric": "m"}}})


def soak_dir(tmp, vm, variant, score, rss, run="soak1"):
    d = os.path.join(tmp, vm)
    os.makedirs(d)
    key = "c4-standard-16/cpu/7.0/img/default"
    s = {
        "run": run, "cell": "ref-amd64", "comparability": key, "vm": {"name": vm}, "block": 0, "pos": 0,
        "warmup": False, "row": "valkey-soak", "variant": variant, "score": score, "unit": "req/s", "better": "higher",
        "extra": {"write_mt_rps": score, "rss_settle_mib": rss, "vmhwm_mib": rss * 2, "delete_mt_rps": score, "write_mt_p99_ms": 1.0},
        "rt": {}, "pmc": None, "error": "",
    }
    with open(os.path.join(d, "samples.jsonl"), "w") as f:
        f.write(json.dumps(s) + "\n")
    with open(os.path.join(d, "blocks.jsonl"), "w") as f:
        f.write(json.dumps({"vm": vm, "run": run, "block": 0, "warmup": False, "sentinel": {"total_ms": 50}, "steal_pct": 0}) + "\n")
    m = {"run": run, "suite": "soak", "variants": [{"label": "jemalloc", "kind": "stock", "source": "jemalloc"}, {"label": "head", "kind": "challoc", "source": "h", "commit": "h"}]}
    with open(os.path.join(d, "manifest.json"), "w") as f:
        json.dump(m, f)
    with open(os.path.join(d, "host.json"), "w") as f:
        json.dump({"cell": "ref-amd64", "comparability": key}, f)
    with open(os.path.join(d, "plan.json"), "w") as f:
        json.dump({"rows": ["valkey-soak"], "alpha": 0.05}, f)
    return d


class TestUnpaired(unittest.TestCase):
    def test_welch(self):
        u = stats.Unpaired([0.10, 0.11, 0.09], [0.0, 0.01, -0.01], level=0.95)
        self.assertAlmostEqual(u.est, 0.10, places=6)
        self.assertGreater(u.lo, 0)
        self.assertLess(u.p, 0.01)
        u1 = stats.Unpaired([0.1], [0.0, 0.01], level=0.95)
        self.assertEqual(u1.hi, float("inf"))

    def test_soak_analysis(self):
        rng = random.Random(3)
        with tempfile.TemporaryDirectory() as tmp:
            dirs = []
            for i in range(3):
                dirs.append(soak_dir(tmp, f"je{i}", "jemalloc", 100000 * (1 + rng.gauss(0, 0.003)), 500 * (1 + rng.gauss(0, 0.002))))
                # head: 10% less RSS after settle, the same throughput
                dirs.append(soak_dir(tmp, f"hd{i}", "head", 100000 * (1 + rng.gauss(0, 0.003)), 450 * (1 + rng.gauss(0, 0.002))))
            s = report.analyze_dirs(dirs, spec.load_rows())
            by = {r["row"]: r for r in s["results"] if r["kind"] == "gate"}
            self.assertEqual(by["valkey-soak.rss_settle_mib"]["verdict"], "better")
            self.assertIn(by["valkey-soak"]["verdict"], ("same", "inconclusive", "sub-threshold"))
            self.assertIn("unpaired (one variant per VM)", by["valkey-soak"]["flags"])
            self.assertEqual(by["valkey-soak"]["n_vm"], 6)
            order = [r["row"] for r in s["results"] if r["kind"] == "gate"]
            self.assertEqual(order[0], "valkey-soak")


if __name__ == "__main__":
    unittest.main()
