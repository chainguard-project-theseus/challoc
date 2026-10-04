"""cb launch planning: shard packing, the VM table, the image lock (no
gcloud calls)."""

import argparse
import os
import tempfile
import unittest

from cb import gce, plan, spec


class Shards(unittest.TestCase):
    def setUp(self):
        self.rows = spec.load_rows()
        self.quick = spec.load_suite("quick", self.rows)

    def test_every_row_once_within_budget(self):
        s = self.quick
        shards = plan.pack_shards(self.rows, s.rows, 2, s.blocks, s.warmup_blocks, s.shard_seconds)
        flat = [r for sh in shards for r in sh]
        self.assertEqual(sorted(flat), sorted(s.rows))
        for sh in shards:
            cost = sum(plan.row_vm_seconds(self.rows, r, 2, s.blocks, s.warmup_blocks) for r in sh)
            self.assertTrue(len(sh) == 1 or cost <= s.shard_seconds, (sh, cost))

    def test_long_row_gets_its_own_shard(self):
        shards = plan.pack_shards(self.rows, ["larson1", "churn1024"], 2, 3, 1, 10)
        self.assertEqual(shards, [["larson1"], ["churn1024"]])

    def test_quick_fits_one_wave(self):
        # 32 VMs at once is the launch cap; quick on the two gate cells must fit.
        s = self.quick
        shards = plan.pack_shards(self.rows, s.rows, 2, s.blocks, s.warmup_blocks, s.shard_seconds)
        self.assertLessEqual(len(shards) * s.replicas * 2, 32)


class VmTable(unittest.TestCase):
    def args(self, **kw):
        d = dict(suite="quick", base="HEAD", head="HEAD", variants=None, controls=None, rows=None, cells=None, cells_path=None,
                 replicas=None, blocks=None, warmup=None, looks=None, extend_replicas=None, shard_seconds=None, seed=7,
                 max_vms=None, project="t-project", bucket="t-bucket", service_account="t-sa@t-project.iam.gserviceaccount.com", run_id="t-run", out=None, no_pmu=False,
                 no_image=False, no_reschedule=False, allow_cost=False, dry_run=True)
        d.update(kw)
        return argparse.Namespace(**d)

    def test_names_and_seeds(self):
        L = gce.Launch(self.args())
        L.plan()
        self.assertEqual(L.run, "t-run")
        self.assertEqual(len(L.vms), len(L.shards) * L.replicas * 2)
        for name, v in L.vms.items():
            self.assertTrue(name.startswith("cb-") and len(name) <= 63, name)
            self.assertEqual(name, name.lower())
            self.assertIn(v["cell"], ("ref-arm64", "ref-amd64"))
        self.assertEqual(len({v["seed"] for v in L.vms.values()}), len(L.vms))
        # A/A: both labels resolve to one commit and one tree.
        trees = {v["tree"] for v in L.variants if v["kind"] == "challoc"}
        self.assertEqual(len(trees), 1)

    def test_reschedule_names(self):
        L = gce.Launch(self.args(cells="ref-amd64", replicas=1, no_reschedule=False))
        L.plan()
        L.upload_launch = lambda: None
        first = next(iter(L.vms))
        L._failed(first, "test")
        again = [n for n in L.vms if n.endswith("-a1")]
        self.assertEqual(len(again), 1)
        self.assertEqual(L.vms[again[0]]["rows"], L.vms[first]["rows"])
        self.assertTrue(L.state[again[0]]["spot"])
        L._failed(again[0], "test")
        third = [n for n in L.vms if n.endswith("-a2")]
        self.assertFalse(L.state[third[0]]["spot"], "the third attempt is on demand")
        L._failed(third[0], "test")
        self.assertFalse(any(n.endswith("-a3") for n in L.vms))

    def test_perf_bundle_metadata(self):
        # The lock's perf-<arch> line reaches every VM of that arch as cb-perf.
        L = gce.Launch(self.args())
        L.plan()
        L.launch_obj = {"object": "runs/t/launch.x.json", "sha256": "11"}
        L.harness_obj = {"object": "src/h.tar.gz", "sha256": "22"}
        L.sources, L.image = {}, {}
        L.perf = {"arm64": "tools/perf-a.tar.gz=33"}
        md = dict(kv.split("=", 1) for kv in L.metadata("ref-arm64")[3:].split("|"))
        self.assertEqual(md["cb-perf"], "tools/perf-a.tar.gz=33")
        md = dict(kv.split("=", 1) for kv in L.metadata("ref-amd64")[3:].split("|"))
        self.assertEqual(md["cb-perf"], "")

    def test_tree_variant_is_refused(self):
        L = gce.Launch(self.args(variants="base,x=tree"))
        with self.assertRaises(spec.SpecError):
            L.plan()


class ImageLock(unittest.TestCase):
    def test_parse(self):
        with tempfile.TemporaryDirectory() as td:
            p = os.path.join(td, "image.lock")
            with open(p, "w") as f:
                f.write('# c\nrepo = "r/x"\narm64 = "r/x@sha256:ab"  # rev date\nperf-arm64 = "tools/p.tar.gz=cd"  # perf\n')
            self.assertEqual(gce.read_image_lock(p), {"repo": "r/x", "arm64": "r/x@sha256:ab", "perf-arm64": "tools/p.tar.gz=cd"})


if __name__ == "__main__":
    unittest.main()
