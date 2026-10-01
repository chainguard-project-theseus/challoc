import unittest

from cb import rows as rowparse
from cb import spec


class RealFiles(unittest.TestCase):
    def test_rows_load(self):
        r = spec.load_rows()
        for name in ("larson1", "larson10", "xmalloc", "alloc-test1", "valkey1", "valkey1-free", "churn1024", "mstress"):
            self.assertIn(name, r.rows)
        self.assertEqual(r.delta_pct(r.rows["larson1"]), 1.0)
        self.assertEqual(r.delta_pct(r.rows["larson10"]), 2.0)
        self.assertEqual(r.rows["churn1024"].requires, ["mallctl"])

    def test_suites_load(self):
        r = spec.load_rows()
        for n in spec.list_suites():
            s = spec.load_suite(n, r)
            for row in s.rows:
                self.assertIn(row, r.rows)
        aa = spec.load_suite("aa", r)
        quick = spec.load_suite("quick", r)
        self.assertEqual(aa.rows, quick.rows)
        self.assertEqual(aa.variants, ["base", "aa=base"])
        self.assertEqual(aa.local["blocks"], quick.local["blocks"])

    def test_cells(self):
        cells = spec.load_cells()
        self.assertEqual(spec.select_cells("tag:gate", cells), ["ref-arm64", "ref-amd64"])
        self.assertEqual(cells["local"].provider, "local")
        self.assertEqual(cells["cpu-turin"].get("host"), cells["ref-amd64"].get("host"))
        self.assertEqual(cells["cpu-turin"].get("machine"), "c4d-standard-16")
        self.assertEqual(cells["cpu-altra"].get("zones"), ["us-central1-a", "us-central1-b"])
        self.assertEqual(cells["k70-gcp"].get("machine"), "c4-standard-16")
        self.assertNotIn("gate", cells["k70-gcp"].tags)

    def test_no_internal_names_in_public_files(self):
        self.assertEqual(spec.lint_public(), [])

    def test_expand(self):
        self.assertEqual(spec.expand("{bin}/x {n}", {"bin": "/b", "n": "3"}), "/b/x 3")
        with self.assertRaises(spec.SpecError):
            spec.expand("{nope}", {})

    def test_select_rows(self):
        r = spec.load_rows()
        st = spec.select_rows("all:class=st", r)
        self.assertIn("larson1", st)
        self.assertNotIn("larson10", st)
        self.assertEqual(spec.select_rows("a,b", r), ["a", "b"])

    def test_bad_row(self):
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"cmd": ["a"], "parser": "nope"}}})
        with self.assertRaises(spec.SpecError):
            spec.parse_rows({"row": {"x": {"cmd": ["a"], "parser": "wall", "requires": ["gpu"]}}})


class Parsers(unittest.TestCase):
    def setUp(self):
        self.rows = spec.load_rows().rows

    def test_larson(self):
        out = "Throughput =  44512345 operations per second, relative time: 22.466s.\n"
        p = rowparse.parse(self.rows["larson1"], out, {})
        self.assertAlmostEqual(p.score, 44.512, places=3)
        self.assertEqual(p.ops, 44512345 * 5)

    def test_xmalloc(self):
        out = "rtime: 5.000, free/sec: 83.186 M\n"
        p = rowparse.parse(self.rows["xmalloc"], out, {})
        self.assertAlmostEqual(p.score, 83.186)

    def test_valkey(self):
        out = "fill: ...\nops total=12345678 write_mops=7.410 delete_mops=9.100 total_mops=8.2 live=600000 rss_kb=571232 hwm_kb=580000 free=sdallocx usable=0\n"
        p = rowparse.parse(self.rows["valkey1"], out, {})
        self.assertAlmostEqual(p.score, 7.41)
        self.assertEqual(p.ops, 12345678)
        self.assertEqual(p.extra["rss_kb"], 571232)

    def test_churn(self):
        out = (
            "   0.0 full     curslabs    469 curregs   30000 nonfull      0 occ 100.0%  active  30.0 MiB resident  31.0 MiB\n"
            "   0.1 deleted  curslabs    469 curregs   13200 nonfull    469 occ  44.0%  active  30.0 MiB resident  31.0 MiB\n"
            "   8.0 end      curslabs    210 curregs   13200 nonfull     12 occ  96.6%  active  13.0 MiB resident  14.0 MiB\n"
            "ops 1234567 (154321/s)\n"
        )
        p = rowparse.parse(self.rows["churn1024"], out, {})
        self.assertAlmostEqual(p.score, 96.6)
        self.assertEqual(p.extra["deleted_slabs"], 469)
        self.assertEqual(p.extra["end_slabs"], 210)
        self.assertEqual(p.extra["ops_per_s"], 154321)

    def test_wall(self):
        p = rowparse.parse(self.rows["alloc-test1"], "", {"wall_s": 3.25})
        self.assertEqual(p.score, 3.25)
        p = rowparse.parse(self.rows["glibc-simple"], "", {"wall_s": 1.0})
        self.assertEqual(p.ops, 64000000)

    def test_missing(self):
        self.assertIsNone(rowparse.parse(self.rows["larson1"], "Segmentation fault\n", {}).score)
        self.assertIsNone(rowparse.parse(self.rows["valkey1"], "", {}).score)
        self.assertIsNone(rowparse.parse(self.rows["alloc-test1"], "", {}).score)


if __name__ == "__main__":
    unittest.main()
