import collections
import unittest

from cb import plan, spec
from cb.plan import VariantSpec, parse_variant


class Williams(unittest.TestCase):
    def test_balance(self):
        for k in range(2, 8):
            seqs = plan.williams(k)
            self.assertEqual(len(seqs), k if k % 2 == 0 else 2 * k)
            # every sequence is a permutation
            for s in seqs:
                self.assertEqual(sorted(s), list(range(k)))
            # every treatment appears in every position equally often
            for pos in range(k):
                c = collections.Counter(s[pos] for s in seqs)
                self.assertEqual(len(set(c.values())), 1, (k, pos, c))
            # every ordered pair (a then b) is adjacent equally often
            pairs = collections.Counter((s[i], s[i + 1]) for s in seqs for i in range(k - 1))
            self.assertEqual(len(pairs), k * (k - 1))
            self.assertEqual(len(set(pairs.values())), 1, (k, pairs))

    def test_two_is_abba(self):
        self.assertEqual(plan.block_order(["A", "B"], 0, 0), ["A", "B", "B", "A"])
        self.assertEqual(plan.block_order(["A", "B"], 1, 0), ["B", "A", "A", "B"])

    def test_mirror_cancels_linear_drift(self):
        for k in range(2, 6):
            labels = [chr(65 + i) for i in range(k)]
            for b in range(4):
                order = plan.block_order(labels, b, 0)
                self.assertEqual(len(order), 2 * k)
                mean_pos = {lab: sum(i for i, x in enumerate(order) if x == lab) / 2 for lab in labels}
                self.assertEqual(len(set(mean_pos.values())), 1, order)


def small_rows():
    return spec.parse_rows(
        {
            "row": {
                "a": {"cmd": ["x"], "parser": "wall", "better": "lower", "class": "st"},
                "b": {"cmd": ["y"], "parser": "wall", "better": "lower", "class": "mt"},
                "c": {"cmd": ["z"], "parser": "churn.occ", "requires": ["mallctl"], "class": "st"},
            }
        }
    )


class Plans(unittest.TestCase):
    def setUp(self):
        self.rows = small_rows()
        self.suite = spec.parse_suite("t", {"rows": ["a", "b", "c"], "variants": ["base", "head"], "looks": 2}, self.rows)
        refs = {"base": "main", "head": "HEAD"}
        self.specs = [parse_variant(v, refs) for v in ("base", "head", "glibc")]

    def make(self, seed=7):
        return plan.make_plan(self.suite, self.rows, self.specs, run_id="r", cell="local", blocks=4, warmup_blocks=1, seed=seed)

    def test_structure(self):
        p = self.make()
        self.assertEqual(len(p.blocks), 5)
        self.assertTrue(p.blocks[0].warmup)
        self.assertFalse(any(b.warmup for b in p.blocks[1:]))
        self.assertAlmostEqual(p.alpha, 0.0294)
        for b in p.blocks:
            self.assertEqual(sorted(b.rows), ["a", "b", "c"])
            self.assertEqual(sorted(b.order["a"]), ["base", "base", "glibc", "glibc", "head", "head"])
            # glibc has no mallctl: row c runs base and head only
            self.assertEqual(sorted(b.order["c"]), ["base", "base", "head", "head"])

    def test_reproducible(self):
        self.assertEqual(self.make(3).to_json(), self.make(3).to_json())
        self.assertNotEqual([b.rows for b in self.make(3).blocks], [b.rows for b in self.make(4).blocks])

    def test_roundtrip(self):
        p = self.make()
        self.assertEqual(plan.Plan.from_json(p.to_json()).to_json(), p.to_json())

    def test_first_position_balanced(self):
        # Over the blocks, each variant starts a row equally often.
        p = plan.make_plan(self.suite, self.rows, self.specs[:2], run_id="r", cell="local", blocks=4, warmup_blocks=0, seed=1)
        first = collections.Counter(b.order["a"][0] for b in p.blocks)
        self.assertEqual(first["base"], first["head"])

    def test_env_pad(self):
        pads = {plan.env_pad(1, b, "a", pos) for b in range(20) for pos in range(4)}
        self.assertTrue(all(0 <= x <= 4096 for x in pads))
        self.assertGreater(len(pads), 40)
        self.assertEqual(plan.env_pad(1, 2, "a", 3), plan.env_pad(1, 2, "a", 3))

    def test_duplicate_labels(self):
        with self.assertRaises(spec.SpecError):
            plan.make_plan(self.suite, self.rows, [self.specs[0], self.specs[0]], run_id="r", cell="local", blocks=1, warmup_blocks=0, seed=1)


class Variants(unittest.TestCase):
    def test_parse(self):
        refs = {"base": "origin/main", "head": "HEAD"}
        self.assertEqual(parse_variant("base", refs), VariantSpec("base", "challoc", "origin/main"))
        self.assertEqual(parse_variant("aa=base", refs), VariantSpec("aa", "challoc", "origin/main"))
        self.assertEqual(parse_variant("x=git:abc123", refs), VariantSpec("x", "challoc", "abc123"))
        self.assertEqual(parse_variant("thp=head@thp:always", refs), VariantSpec("thp", "challoc", "HEAD", "thp:always"))
        v = parse_variant("jemalloc", refs, "control")
        self.assertEqual((v.kind, v.role, v.mallctl), ("stock", "control", True))
        self.assertFalse(parse_variant("glibc", refs).mallctl)
        self.assertEqual(parse_variant({"name": "t", "src": "head", "conf": "a:b"}, refs), VariantSpec("t", "challoc", "HEAD", "a:b"))
        with self.assertRaises(spec.SpecError):
            parse_variant("nope", refs)

    def test_can_run(self):
        refs = {"base": "b", "head": "h"}
        self.assertTrue(parse_variant("head", refs).can_run(["challoc", "mallctl"]))
        self.assertFalse(parse_variant("jemalloc", refs).can_run(["challoc"]))
        self.assertTrue(parse_variant("jemalloc", refs).can_run(["mallctl"]))


if __name__ == "__main__":
    unittest.main()
