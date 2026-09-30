"""Planning: variants, block order, seeds.

A plan is pure data (JSON-serialisable): which rows, which variants, and
for every block the row order and, per row, the order the variants run in.

Order within a block. With k variants a block runs one sequence s of a
Williams design followed by its mirror reverse(s): with k = 2 that is ABBA
(or BAAB), and in general every variant appears twice at mirror positions,
so a linear drift over the block cancels in every pairwise contrast (the
mean position of each variant is the same). Successive blocks, and
successive rows within a block, rotate through the Williams sequences, so
first-order carry-over is balanced too. Rows are shuffled per block with a
recorded seed. A row that some variants cannot run (requires = mallctl)
keeps the sequence of the others, which is still mirror-symmetric.
"""

from __future__ import annotations

import dataclasses
import hashlib
import random
from typing import Any

from .spec import Rows, SpecError, Suite

STOCK = {
    "jemalloc": {"lib": "/usr/lib/libjemalloc.so.2", "mallctl": True},
    "tcmalloc": {"lib": "/usr/lib/libtcmalloc_minimal.so.4", "mallctl": False},
    "mimalloc": {"lib": "{bin}/libmimalloc.so", "mallctl": False},
    "glibc": {"lib": "", "mallctl": False},
}


@dataclasses.dataclass
class VariantSpec:
    """A variant before it is built: a label, where its library comes from,
    and its MALLOC_CONF."""

    label: str
    kind: str  # "challoc" | "stock"
    source: str  # a git rev, "tree", or the stock allocator's name
    conf: str = ""
    role: str = "variant"  # "variant" | "control"

    @property
    def mallctl(self) -> bool:
        return self.kind == "challoc" or bool(STOCK.get(self.source, {}).get("mallctl"))

    def can_run(self, requires: list[str]) -> bool:
        for r in requires:
            if r == "mallctl" and not self.mallctl:
                return False
            if r == "challoc" and self.kind != "challoc":
                return False
        return True


def parse_variant(item: Any, refs: dict[str, str], role: str = "variant") -> VariantSpec:
    """`base`, `head` (their revs from refs), `label=base`, `label=git:REV`,
    `label=tree`, `jemalloc`/`glibc`/`tcmalloc`/`mimalloc`, or a table
    {name, src, conf}. A trailing `@CONF` sets MALLOC_CONF."""
    if isinstance(item, dict):
        label = item.get("name") or item.get("label")
        src = item.get("src", label)
        conf = item.get("conf", "")
        if not label:
            raise SpecError(f"variant {item!r}: needs a name")
    else:
        s = str(item)
        conf = ""
        if "@" in s:
            s, conf = s.split("@", 1)
        label, _, src = s.partition("=")
        src = src or label
    if src in STOCK:
        return VariantSpec(label, "stock", src, conf, role)
    if src.startswith("git:"):
        return VariantSpec(label, "challoc", src[4:], conf, role)
    if src == "tree":
        return VariantSpec(label, "challoc", "tree", conf, role)
    if src in refs:
        return VariantSpec(label, "challoc", refs[src], conf, role)
    raise SpecError(f"variant {item!r}: source {src!r} is not base, head, git:REV, tree or a stock allocator")


def williams(k: int) -> list[list[int]]:
    """A Williams design for k treatments: every treatment follows every
    other equally often. k sequences for even k, 2k for odd k."""
    if k <= 0:
        return []
    if k == 1:
        return [[0]]
    first = [0]
    lo, hi = 1, k - 1
    take_lo = True
    while len(first) < k:
        if take_lo:
            first.append(lo)
            lo += 1
        else:
            first.append(hi)
            hi -= 1
        take_lo = not take_lo
    seqs = [[(x + i) % k for x in first] for i in range(k)]
    if k % 2 == 1:
        seqs += [list(reversed(s)) for s in seqs]
    return seqs


@dataclasses.dataclass
class Block:
    index: int
    warmup: bool
    seed: int
    rows: list[str]
    order: dict[str, list[str]]  # row -> variant labels in run order


@dataclasses.dataclass
class Plan:
    run_id: str
    suite: str
    kind: str
    cell: str
    rows: list[str]
    variants: list[VariantSpec]
    blocks: list[Block]
    seed: int
    alpha: float
    looks: int

    def to_json(self) -> dict[str, Any]:
        return {
            "schema": "challoc.bench.plan/v1",
            "run": self.run_id,
            "suite": self.suite,
            "kind": self.kind,
            "cell": self.cell,
            "rows": self.rows,
            "variants": [dataclasses.asdict(v) for v in self.variants],
            "blocks": [dataclasses.asdict(b) for b in self.blocks],
            "seed": self.seed,
            "alpha": self.alpha,
            "looks": self.looks,
        }

    @staticmethod
    def from_json(d: dict[str, Any]) -> "Plan":
        return Plan(
            run_id=d["run"],
            suite=d["suite"],
            kind=d["kind"],
            cell=d["cell"],
            rows=d["rows"],
            variants=[VariantSpec(**v) for v in d["variants"]],
            blocks=[Block(**b) for b in d["blocks"]],
            seed=d["seed"],
            alpha=d["alpha"],
            looks=d["looks"],
        )

    def runs(self) -> int:
        return sum(len(o) for b in self.blocks for o in b.order.values())

    def est_seconds(self, rows: Rows) -> float:
        return sum(rows.rows[r].est_s * len(o) for b in self.blocks for r, o in b.order.items())


def derive_seed(*parts: Any) -> int:
    h = hashlib.sha256("/".join(str(p) for p in parts).encode()).digest()
    return int.from_bytes(h[:4], "little")


def block_order(labels: list[str], block: int, row_index: int) -> list[str]:
    """The run order of one row in one block: a Williams sequence (rotated
    by block and row) followed by its mirror."""
    seqs = williams(len(labels))
    s = seqs[(block + row_index) % len(seqs)]
    first = [labels[i] for i in s]
    if len(labels) == 1:
        return first
    return first + list(reversed(first))


def make_plan(
    suite: Suite,
    rows: Rows,
    variants: list[VariantSpec],
    *,
    run_id: str,
    cell: str,
    blocks: int,
    warmup_blocks: int,
    seed: int,
    row_names: list[str] | None = None,
) -> Plan:
    names = list(row_names or suite.rows)
    for n in names:
        if n not in rows.rows:
            raise SpecError(f"unknown row {n!r}")
    labels = [v.label for v in variants]
    if len(set(labels)) != len(labels):
        raise SpecError(f"duplicate variant labels in {labels}")
    out: list[Block] = []
    for b in range(warmup_blocks + blocks):
        bseed = derive_seed(seed, "block", b)
        order_rows = list(names)
        random.Random(bseed).shuffle(order_rows)
        order: dict[str, list[str]] = {}
        for ri, r in enumerate(order_rows):
            req = rows.rows[r].requires
            able = [v.label for v in variants if v.can_run(req)]
            if not able:
                continue
            # Rotate by the row's position in the suite (not the shuffled
            # one) so a row sees every sequence over the blocks.
            order[r] = block_order(able, b, names.index(r))
        out.append(Block(b, b < warmup_blocks, bseed, [r for r in order_rows if r in order], order))
    return Plan(
        run_id=run_id,
        suite=suite.name,
        kind=suite.kind,
        cell=cell,
        rows=names,
        variants=variants,
        blocks=out,
        seed=seed,
        alpha=suite.alpha,
        looks=suite.looks,
    )


def env_pad(seed: int, block: int, row: str, pos: int) -> int:
    """The random environment pad (0..4096 bytes) of one run, reproducible
    from the plan seed. Moves the initial stack, so a result that depends on
    one stack alignment shows up as spread instead of as an effect."""
    return random.Random(derive_seed(seed, "pad", block, row, pos)).randrange(0, 4097)


def row_vm_seconds(rows: Rows, row: str, n_variants: int, blocks: int, warmup_blocks: int) -> float:
    """Expected seconds one VM spends on one row: every block runs each
    variant twice (a Williams sequence and its mirror; once with a single
    variant)."""
    per_block = n_variants * (2 if n_variants > 1 else 1)
    return rows.rows[row].est_s * per_block * (blocks + warmup_blocks)


def pack_shards(rows: Rows, row_names: list[str], n_variants: int, blocks: int, warmup_blocks: int, shard_seconds: float) -> list[list[str]]:
    """Rows packed into shards (one shard = the rows one VM runs) of about
    shard_seconds each: first-fit decreasing, a row longer than the budget
    gets a shard of its own. Rows keep the suite's order inside a shard."""
    cost = {r: row_vm_seconds(rows, r, n_variants, blocks, warmup_blocks) for r in row_names}
    bins: list[tuple[float, list[str]]] = []
    for r in sorted(row_names, key=lambda x: (-cost[x], row_names.index(x))):
        for i, (used, members) in enumerate(bins):
            if used + cost[r] <= shard_seconds:
                bins[i] = (used + cost[r], members + [r])
                break
        else:
            bins.append((cost[r], [r]))
    shards = [sorted(m, key=row_names.index) for _, m in bins]
    return sorted(shards, key=lambda m: row_names.index(m[0]))
