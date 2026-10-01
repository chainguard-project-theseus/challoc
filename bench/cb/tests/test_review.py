"""The launcher's VM bookkeeping and the verdict rules around it: the
project cap, cb gc's selection, per-VM deadlines, lost shards, VM names,
boot disks, the second look, per-row margins, paired rows, smoke launches
(no gcloud calls)."""

import argparse
import datetime
import json
import os
import tempfile
import time
import unittest

from cb import gce, report, spec, stats
from cb.tests.test_report import synth_run


def launch_args(**kw):
    d = dict(suite="quick", base="HEAD", head="HEAD", variants=None, controls=None, rows=None, cells=None, cells_path=None,
             replicas=None, blocks=None, warmup=None, looks=None, extend_replicas=None, shard_seconds=None, seed=7,
             max_vms=None, project=None, bucket=None, service_account=None, run_id="t-run", out=None, no_pmu=False,
             no_image=False, no_reschedule=False, allow_cost=False, dry_run=True)
    d.update(kw)
    return argparse.Namespace(**d)


class FakeApi:
    def __init__(self, project_vms):
        self.project_vms = project_vms

    def instances(self, flt):
        return self.project_vms


class ProjectCap(unittest.TestCase):
    def test_counts_every_launch(self):
        L = gce.Launch(launch_args())
        L.plan()
        L.project_max_vcpus = 2000
        # 120 VMs of 16 vCPUs from other launches: 1920 in use, room for 5
        # more 16-vCPU VMs (80), not for this launch's 30.
        other = [{"name": f"cb-x-{i}", "status": "RUNNING", "machineType": "zones/z/machineTypes/c4-standard-16"} for i in range(120)]
        other.append({"name": "cb-y", "status": "TERMINATED", "machineType": "zones/z/machineTypes/c4-standard-16"})
        L.api = FakeApi(other)
        created = []

        def fake_group(cell, names, spot, max_run):
            created.extend(names)
            other.extend({"name": n, "status": "RUNNING", "machineType": "c4-standard-16"} for n in names)
            for n in names:
                L.state[n].update(status="created", created=time.time(), zone="z")
            return []

        L._create_group = fake_group
        n_before = len(L.pending)
        L.create_pending()
        self.assertEqual(len(created), 5)
        self.assertEqual(len(L.pending), n_before - 5)
        # Nothing more fits while the others hold the project.
        L.create_pending()
        self.assertEqual(len(created), 5)

    def test_vcpus(self):
        self.assertEqual(gce.vcpus("c4-standard-16"), 16)
        self.assertEqual(gce.vcpus("zones/us-east4-a/machineTypes/c4a-standard-4"), 4)
        self.assertEqual(gce.vcpus("c3-standard-192-metal"), 192)


class Gc(unittest.TestCase):
    now = datetime.datetime(2026, 10, 1, 3, 0, tzinfo=datetime.timezone.utc)

    def vm(self, name, run, status, age_min, mrd_s=2700):
        t = self.now - datetime.timedelta(minutes=age_min)
        return {"name": name, "zone": "zones/us-east4-a", "status": status, "creationTimestamp": t.isoformat(),
                "labels": {"cb-run": run}, "scheduling": {"maxRunDuration": {"seconds": str(mrd_s)}}}

    def test_never_kills_a_live_launch(self):
        items = [
            self.vm("cb-a", "mine", "RUNNING", 5),
            self.vm("cb-b", "other", "RUNNING", 80, mrd_s=4560),  # another session's, within its max-run
            self.vm("cb-c", "other", "TERMINATED", 120),
            self.vm("cb-d", "old", "RUNNING", 120, mrd_s=2700),  # past max-run + 10 min
        ]
        names = {i["name"] for i, _ in gce.gc_select(items, self.now, run=None, older_than_min=0)}
        self.assertEqual(names, {"cb-c", "cb-d"})
        names = {i["name"] for i, _ in gce.gc_select(items, self.now, run=None, older_than_min=60)}
        self.assertEqual(names, {"cb-c", "cb-d"})

    def test_run_scoped(self):
        items = [self.vm("cb-a", "mine", "RUNNING", 5), self.vm("cb-b", "other", "RUNNING", 5)]
        names = {i["name"] for i, _ in gce.gc_select(items, self.now, run="mine", older_than_min=0)}
        self.assertEqual(names, {"cb-a"})


class Deadlines(unittest.TestCase):
    def test_per_vm(self):
        L = gce.Launch(launch_args(cells="ref-amd64", replicas=1))
        L.plan()
        a, b = list(L.vms)[:2]
        L.state[a]["created"] = 1000.0
        L.state[b]["created"] = 5000.0
        # A VM of a later wave gets its own full max-run.
        self.assertEqual(L.vm_deadline(b) - L.vm_deadline(a), 4000.0)
        self.assertEqual(L.vm_deadline(a), 1000.0 + gce._dur_s(L.vm_max_run(a)) + 120)


class LostShards(unittest.TestCase):
    def test_done_after_two_reschedules(self):
        st = {
            "x": {"base": "s0", "status": "failed"},
            "x-a1": {"base": "s0", "status": "failed"},
            "x-a2": {"base": "s0", "status": "done"},
            "y": {"base": "s1", "status": "failed"},
            "y-a1": {"base": "s1", "status": "timeout"},
            "z": {"base": "s2", "status": "done"},
        }
        self.assertEqual(gce.lost_shards(st), ["s1"])

    def test_from_a_launch(self):
        L = gce.Launch(launch_args(cells="ref-amd64", replicas=1))
        L.plan()
        L.upload_launch = lambda: None
        first = next(iter(L.vms))
        L._failed(first, "t")
        a1 = next(n for n in L.vms if n.endswith("-a1"))
        L._failed(a1, "t")
        a2 = next(n for n in L.vms if n.endswith("-a2"))
        L.state[a2]["status"] = "done"
        self.assertEqual(gce.lost_shards(L.state), [])


class Names(unittest.TestCase):
    def test_slugs_do_not_collide(self):
        a = gce.cell_slug("fl-u2404-64k-arm-privileged")
        b = gce.cell_slug("fl-u2404-64k-arm-privileged2")
        self.assertNotEqual(a, b)
        self.assertLessEqual(len(a), 20)
        self.assertEqual(gce.cell_slug("ref_amd64"), "ref-amd64")
        self.assertEqual(gce.cell_slug("k-64k-arm64"), "k-64k-arm64")

    def test_vm_names_unique(self):
        L = gce.Launch(launch_args(cells="tag:exp-floor", rows="floor-test", variants="head", controls="", replicas=1))
        L.plan()
        bases = [L.state[n]["base"] for n in L.vms]
        self.assertEqual(len(bases), len(set(bases)))
        for n in L.vms:
            self.assertLessEqual(len(n), 63)


class Disks(unittest.TestCase):
    def test_default_disk(self):
        self.assertEqual(gce.default_disk("e2-standard-8"), "pd-balanced")
        self.assertEqual(gce.default_disk("n2-standard-8"), "pd-balanced")
        self.assertEqual(gce.default_disk("c4-standard-16"), "hyperdisk-balanced")
        self.assertEqual(gce.default_disk("c4a-standard-16"), "hyperdisk-balanced")


def gate(row, verdict, holm=False, key="k"):
    return {"kind": "gate", "row": row, "verdict": verdict, "holm": holm, "comparability": key}


class SecondLook(unittest.TestCase):
    def test_extends_unresolved_rows(self):
        summary = {"results": [
            gate("larson1", "inconclusive"),
            gate("larson10", "worse", holm=False),
            gate("xmalloc", "worse", holm=True),
            gate("valkey1", "better", holm=False),
            gate("churn1024", "same"),
            {"kind": "context", "row": "alloc-test1", "verdict": "inconclusive", "comparability": "k"},
        ]}
        got = gce.look2_rows(summary, ["larson1", "larson10", "xmalloc", "valkey1", "churn1024", "alloc-test1"])
        self.assertEqual(got, {"k": ["larson1", "larson10", "valkey1"]})

    def test_headline_for_unrejected_worse(self):
        # The case the second look exists for: a worse row Holm does not
        # reject makes the headline INCONCLUSIVE.
        h = stats.headline({"a": "worse", "b": "same"}, {"a": 0.04, "b": 0.5}, 0.0294)
        self.assertEqual(h, "INCONCLUSIVE")
        self.assertEqual(gce.look2_rows({"results": [gate("a", "worse")]}, ["a"]), {"k": ["a"]})


class Margins(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.rows = spec.load_rows()

    def test_per_row_history(self):
        d = synth_run(self.tmp, {})
        noisy = [5.0] * 20  # one noisy row's A/A history
        quiet = [0.1] * 20
        hist = {report.aa_key("local", "larson1"): noisy, report.aa_key("local", "xmalloc"): quiet}
        s = report.analyze_dirs([d], self.rows, aa_history=hist)
        by = {r["row"]: r for r in s["results"] if r["kind"] == "gate"}
        self.assertAlmostEqual(by["larson1"]["margin_pct"], 5.0)
        self.assertTrue(by["larson1"]["calibrated"])
        # The quiet row keeps its own margin, not the noisy row's.
        self.assertLess(by["xmalloc"]["margin_pct"], 5.0)
        self.assertTrue(by["xmalloc"]["calibrated"])
        # A row without history is uncalibrated.
        self.assertFalse(by["valkey1"]["calibrated"])

    def test_history_lines_need_row_and_cell(self):
        p = os.path.join(self.tmp, "h.jsonl")
        with open(p, "w") as f:
            f.write(json.dumps({"est_pct": 1.0}) + "\n")
        with self.assertRaises(ValueError):
            report.load_aa_history(p)
        with open(p, "w") as f:
            f.write(json.dumps({"cell": "c", "row": "r", "est_pct": 1.0}) + "\n")
        self.assertEqual(report.load_aa_history(p), {"c/r": [1.0]})


class PairedRows(unittest.TestCase):
    def test_no_unpaired_fallback(self):
        # A paired row whose blocks each lack one variant (every block of
        # head flagged, say): no estimate, never Welch on VM means.
        samples = []
        for vm in ("a", "b", "c"):
            for blk, lab in ((1, "base"), (2, "head")):
                samples.append({"run": "r", "vm": {"name": vm}, "block": blk, "row": "larson1", "variant": lab,
                                "score": 10.0 if lab == "base" else 9.0, "error": "", "better": "higher", "unit": "M ops/s"})
        r = report.compare_row(samples, "larson1", "head", "base", delta_pct=2.0, alpha=0.05, flagged=set(), excluded={}, fixed_work=False)
        self.assertEqual(r["verdict"], "inconclusive")
        self.assertIn("no paired blocks", r["flags"])
        self.assertNotIn("unpaired (one variant per VM)", r["flags"])
        u = report.compare_row(samples, "larson1", "head", "base", delta_pct=2.0, alpha=0.05, flagged=set(), excluded={}, fixed_work=False, placement="one-variant-per-vm")
        self.assertIn("unpaired (one variant per VM)", u["flags"])


class Smoke(unittest.TestCase):
    def test_pass_and_broken(self):
        tmp = tempfile.mkdtemp()
        rows = spec.load_rows()
        d = synth_run(tmp, {})
        s = report.analyze_dirs([d], rows)
        gce.smoke_headline(s, {"v": {"base": "s0", "status": "done"}}, [d])
        self.assertEqual((s["headline"], s["exit"]), ("PASS", 0))
        d2 = synth_run(tmp, {}, broken_row="larson1", run="r2")
        s = report.analyze_dirs([d2], rows)
        gce.smoke_headline(s, {"v": {"base": "s0", "status": "done"}}, [d2])
        self.assertEqual((s["headline"], s["exit"]), ("BROKEN", 1))
        s = report.analyze_dirs([d], rows)
        gce.smoke_headline(s, {"v": {"base": "s0", "status": "failed"}}, [d])
        self.assertEqual(s["headline"], "BROKEN")


class Containers(unittest.TestCase):
    def test_cpus_shorthand(self):
        self.assertEqual(spec.parse_container("cpus2"), {"profile": "perfmon", "cpus": "2"})
        self.assertEqual(spec.parse_container("privileged"), {"profile": "privileged"})


if __name__ == "__main__":
    unittest.main()
