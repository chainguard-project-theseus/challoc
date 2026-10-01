"""Declarative config: rows (bench/rows.toml), suites (bench/suites/*.toml)
and cells (bench/cells/*.toml). Loading, overlay merge, validation.

Everything here is pure: no subprocesses, no environment reads beyond the
paths it is given, so the planner and the unit tests can use it anywhere.
"""

from __future__ import annotations

import copy
import dataclasses
import glob
import os
import re
import tomllib
from typing import Any

BENCH_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT_DIR = os.path.dirname(BENCH_DIR)

# Public cell files may only name images from these projects; anything
# else (a private image project) belongs in an internal overlay,
# bench/cells/internal-*.toml, which is deleted before publication. An
# overlay may list `[lint] markers = [...]`: strings that must then appear
# in no public file (cell files, cb's code, rows, suites). The markers live
# only in the overlay, so the public tree never names them.
PUBLIC_IMAGE_PROJECTS = (
    "ubuntu-os-cloud",
    "debian-cloud",
    "almalinux-cloud",
    "rocky-linux-cloud",
    "centos-cloud",
    "rhel-cloud",
    "cos-cloud",
    "fedora-coreos-cloud",
    "fedora-cloud",
    "suse-cloud",
    "opensuse-cloud",
)

PARSERS = ("larson", "xmalloc", "valkey_model.write", "churn.occ", "wall", "kv", "driver")
# Row kinds: `timed` (one program per run, scored by a parser), `soak` and
# `driver` (a script under bench/drivers/ that writes its metrics to
# RESULT_JSONL, scored by one of them), `metric` (a derived row: one extra
# metric of a driver row, compared on its own; never run).
ROW_KINDS = ("timed", "soak", "driver", "metric")
PLACEMENTS = ("paired", "one-variant-per-vm")
CLASSES_DEFAULT = {
    "st": {"delta_pct": 1.0},
    "mt": {"delta_pct": 2.0},
    "server": {"delta_pct": 3.0},
    "soak": {"delta_pct": 3.0},
}
# Pocock per-look alpha for 1, 2 or 3 looks (two-sided 0.05 overall).
POCOCK_ALPHA = {1: 0.05, 2: 0.0294, 3: 0.0221}


class SpecError(Exception):
    pass


@dataclasses.dataclass
class Row:
    name: str
    cmd: list[str]
    parser: str
    unit: str
    better: str  # "higher" | "lower"
    klass: str
    threads: int = 1
    env: dict[str, str] = dataclasses.field(default_factory=dict)
    fixed_work: bool = False
    secs: float | None = None
    ops: int | None = None
    requires: list[str] = dataclasses.field(default_factory=list)
    check: dict[str, float] = dataclasses.field(default_factory=dict)
    est_s: float = 10.0
    needs: list[str] = dataclasses.field(default_factory=list)
    kind: str = "timed"
    # Driver rows (kind soak or driver): the script (relative to bench/),
    # the metric that is the score, and the extra metrics compared as rows
    # of their own (`<row>.<metric>`, kind metric, parent = this row).
    driver: str = ""
    metric: str = ""
    metrics: list[dict[str, Any]] = dataclasses.field(default_factory=list)
    parent: str = ""
    # Cargo features the variant library needs for this row ("quantum8":
    # Valkey's allocator_defrag.c asserts an 8-byte quantum; "jemalloc-soname"
    # for the --with-jemalloc Ruby); empty: the default build.
    features: str = ""
    # paired: every VM runs every variant in ABBA blocks; one-variant-per-vm:
    # a VM runs one variant (soaks: replicas per variant in parallel,
    # compared unpaired).
    placement: str = "paired"
    # The variant sources the row can run (challoc = any challoc build,
    # or a stock allocator name); empty: all.
    allocs: list[str] = dataclasses.field(default_factory=list)
    timeout_s: float | None = None

    @property
    def higher_is_better(self) -> bool:
        return self.better == "higher"

    @property
    def is_driver(self) -> bool:
        return self.kind in ("soak", "driver")


@dataclasses.dataclass
class Rows:
    rows: dict[str, Row]
    vars: dict[str, str]
    classes: dict[str, dict[str, float]]

    def delta_pct(self, row: Row) -> float:
        return float(self.classes.get(row.klass, {}).get("delta_pct", 1.0))


@dataclasses.dataclass
class Suite:
    name: str
    description: str
    kind: str  # "compare" | "smoke"
    cells: str | list[str]
    variants: list[Any]
    controls: list[Any]
    rows: list[str]
    replicas: int
    blocks: int
    warmup_blocks: int
    looks: int
    extend_replicas: int
    shard_seconds: int
    budget_usd: float
    local: dict[str, Any]
    raw: dict[str, Any]
    container: dict[str, Any] | None = None
    prep: list[str] = dataclasses.field(default_factory=list)

    @property
    def alpha(self) -> float:
        return POCOCK_ALPHA.get(self.looks, 0.05 / max(1, self.looks))


@dataclasses.dataclass
class Cell:
    name: str
    provider: str
    fields: dict[str, Any]
    tags: list[str]
    source: str

    def get(self, key: str, default: Any = None) -> Any:
        return self.fields.get(key, default)


def _load_toml(path: str) -> dict[str, Any]:
    try:
        with open(path, "rb") as f:
            return tomllib.load(f)
    except FileNotFoundError:
        raise SpecError(f"{path}: not found") from None
    except tomllib.TOMLDecodeError as e:
        raise SpecError(f"{path}: {e}") from None


# ---- rows --------------------------------------------------------------------


def parse_rows(doc: dict[str, Any], source: str = "rows.toml") -> Rows:
    vars_ = {k: str(v) for k, v in doc.get("vars", {}).items()}
    classes = copy.deepcopy(CLASSES_DEFAULT)
    for k, v in doc.get("class", {}).items():
        classes[k] = dict(v)
    rows: dict[str, Row] = {}
    for name, r in doc.get("row", {}).items():
        where = f"{source}: row {name}"
        parser = r.get("parser")
        if parser is None and isinstance(r.get("score"), dict):
            parser = r["score"].get("parser")
        kind = r.get("kind", "timed")
        if kind not in ROW_KINDS or kind == "metric":
            raise SpecError(f"{where}: kind must be timed, soak or driver")
        if kind in ("soak", "driver"):
            if not r.get("driver"):
                raise SpecError(f"{where}: a {kind} row needs driver = \"drivers/<script>\"")
            if not os.path.exists(os.path.join(BENCH_DIR, r["driver"])):
                raise SpecError(f"{where}: driver {r['driver']} does not exist under bench/")
            if not r.get("metric"):
                raise SpecError(f"{where}: a {kind} row needs metric = \"<the score metric>\"")
            parser = "driver"
        if r.get("placement", "paired") not in PLACEMENTS:
            raise SpecError(f"{where}: placement must be one of {', '.join(PLACEMENTS)}")
        if kind == "timed":
            if not isinstance(r.get("cmd"), list) or not r["cmd"]:
                raise SpecError(f"{where}: cmd must be a non-empty list")
            if parser not in PARSERS:
                raise SpecError(f"{where}: parser {parser!r} is not one of {', '.join(PARSERS)}")
            better = r.get("better", "higher")
            if better not in ("higher", "lower"):
                raise SpecError(f"{where}: better must be higher or lower")
        else:
            better = r.get("better", "higher")
        klass = r.get("class", "st")
        if klass not in classes:
            raise SpecError(f"{where}: class {klass!r} has no [class] entry")
        for req in r.get("requires", []):
            if req not in ("mallctl", "challoc"):
                raise SpecError(f"{where}: unknown requirement {req!r}")
        rows[name] = Row(
            name=name,
            cmd=[str(a) for a in r.get("cmd", [])],
            parser=parser or "",
            unit=r.get("unit", ""),
            better=better,
            klass=klass,
            threads=int(r.get("threads", 1)),
            env={k: str(v) for k, v in r.get("env", {}).items()},
            fixed_work=bool(r.get("fixed_work", False)),
            secs=float(r["secs"]) if "secs" in r else None,
            ops=int(r["ops"]) if "ops" in r else None,
            requires=list(r.get("requires", [])),
            check={k: float(v) for k, v in r.get("check", {}).items()},
            est_s=float(r.get("est_s", 10)),
            needs=list(r.get("needs", [])),
            kind=kind,
            driver=str(r.get("driver", "")),
            metric=str(r.get("metric", "")),
            metrics=[dict(m) for m in r.get("metrics", [])],
            features=str(r.get("features", "")),
            placement=str(r.get("placement", "paired")),
            allocs=[str(a) for a in r.get("allocs", [])],
            timeout_s=float(r["timeout_s"]) if "timeout_s" in r else None,
        )
        for m in r.get("metrics", []):
            if "name" not in m:
                raise SpecError(f"{where}: every metrics entry needs a name")
            mk = m.get("class", klass)
            if mk not in classes:
                raise SpecError(f"{where}: metric {m['name']}: class {mk!r} has no [class] entry")
            dn = f"{name}.{m['name']}"
            rows[dn] = Row(
                name=dn,
                cmd=[],
                parser="",
                unit=str(m.get("unit", "")),
                better=str(m.get("better", "lower")),
                klass=mk,
                kind="metric",
                parent=name,
                placement=rows[name].placement,
                est_s=0,
            )
    return Rows(rows=rows, vars=vars_, classes=classes)


def load_rows(path: str | None = None) -> Rows:
    path = path or os.path.join(BENCH_DIR, "rows.toml")
    return parse_rows(_load_toml(path), os.path.basename(path))


_VAR = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")


def expand(s: str, values: dict[str, str]) -> str:
    """Replace {name} from values; an unknown name is an error."""

    def sub(m: re.Match[str]) -> str:
        k = m.group(1)
        if k not in values:
            raise SpecError(f"unknown variable {{{k}}} in {s!r}")
        return values[k]

    return _VAR.sub(sub, s)


# ---- suites ------------------------------------------------------------------

SUITE_DEFAULTS: dict[str, Any] = {
    "description": "",
    "kind": "compare",
    "cells": "local",
    "variants": ["base", "head"],
    "controls": [],
    "rows": [],
    "replicas": 1,
    "blocks": 3,
    "warmup_blocks": 1,
    "looks": 1,
    "extend_replicas": 0,
    "shard_seconds": 150,
    "budget_usd": 0.0,
    "local": {},
    # The container profile on the cells: "default" (the runtime's default
    # seccomp), "privileged", "perfmon" (CAP_PERFMON only), or a table
    # {profile, cpus, memory, cpuset}; overrides the cells' `container`.
    "container": None,
    # Host preparation added to every cell's own (bench/cb/boot.sh): e.g.
    # ["thp:64kB=madvise"].
    "prep": [],
    # Metrics the suite wants recorded beside the score (informational).
    "record": [],
}


def _suite_doc(name: str, suites_dir: str, seen: tuple[str, ...] = ()) -> dict[str, Any]:
    if name in seen:
        raise SpecError(f"suite {name}: extends loop {' -> '.join(seen + (name,))}")
    path = os.path.join(suites_dir, f"{name}.toml")
    doc = _load_toml(path)
    parent = doc.pop("extends", None)
    if parent is None:
        return doc
    base = _suite_doc(parent, suites_dir, seen + (name,))
    merged = copy.deepcopy(base)
    for k, v in doc.items():
        if isinstance(v, dict) and isinstance(merged.get(k), dict):
            merged[k] = {**merged[k], **v}
        else:
            merged[k] = v
    return merged


def parse_suite(name: str, doc: dict[str, Any], rows: Rows | None = None) -> Suite:
    d = {**SUITE_DEFAULTS, **doc}
    unknown = set(doc) - set(SUITE_DEFAULTS)
    if unknown:
        raise SpecError(f"suite {name}: unknown keys {', '.join(sorted(unknown))}")
    row_names = d["rows"]
    if isinstance(row_names, str):
        if rows is None:
            raise SpecError(f"suite {name}: a row selector needs the row table")
        row_names = select_rows(row_names, rows)
    if rows is not None:
        for r in row_names:
            if r not in rows.rows:
                raise SpecError(f"suite {name}: unknown row {r!r}")
            if rows.rows[r].kind == "metric":
                raise SpecError(f"suite {name}: {r} is a metric of row {rows.rows[r].parent}, not a row to run")
    if d["kind"] not in ("compare", "smoke"):
        raise SpecError(f"suite {name}: kind must be compare or smoke")
    if d["kind"] == "compare" and len(d["variants"]) < 2:
        raise SpecError(f"suite {name}: a compare suite needs at least two variants")
    return Suite(
        name=name,
        description=d["description"],
        kind=d["kind"],
        cells=d["cells"],
        variants=list(d["variants"]),
        controls=list(d["controls"]),
        rows=list(row_names),
        replicas=int(d["replicas"]),
        blocks=int(d["blocks"]),
        warmup_blocks=int(d["warmup_blocks"]),
        looks=int(d["looks"]),
        extend_replicas=int(d["extend_replicas"]),
        shard_seconds=int(d["shard_seconds"]),
        budget_usd=float(d["budget_usd"]),
        local=dict(d["local"]),
        raw=doc,
        container=parse_container(d["container"], f"suite {name}") if d["container"] is not None else None,
        prep=[str(p) for p in d["prep"]],
    )


CONTAINER_PROFILES = ("default", "privileged", "perfmon")


def parse_container(c: Any, where: str = "container") -> dict[str, Any]:
    """A container profile: a name, or {profile, cpus, memory, cpuset}:
    cpus is a CFS quota (--cpus: the threads run on every CPU and are
    throttled), cpuset pins the container to CPUs (--cpuset-cpus "0-3":
    more threads than CPUs, preempted by each other). Returns the table
    form."""
    if isinstance(c, str):
        # cpusN: a CPU quota of N CPUs without a cpuset (the affinity mask
        # stays the host's), with CAP_PERFMON.
        m = re.fullmatch(r"cpus(\d+(?:\.\d+)?)", c)
        c = {"profile": "perfmon", "cpus": m.group(1)} if m else {"profile": c}
    if not isinstance(c, dict):
        raise SpecError(f"{where}: container must be a profile name or a table")
    out = {"profile": str(c.get("profile", "default"))}
    if out["profile"] not in CONTAINER_PROFILES:
        raise SpecError(f"{where}: container profile {out['profile']!r} is not one of {', '.join(CONTAINER_PROFILES)}")
    if c.get("cpus") is not None:
        out["cpus"] = str(c["cpus"])
    if c.get("memory") is not None:
        out["memory"] = str(c["memory"])
    if c.get("cpuset") is not None:
        out["cpuset"] = str(c["cpuset"])
    unknown = set(c) - {"profile", "cpus", "memory", "cpuset"}
    if unknown:
        raise SpecError(f"{where}: unknown container keys {', '.join(sorted(unknown))}")
    return out


def container_key(c: dict[str, Any] | str | None) -> str:
    """The container part of the comparability key: profile plus limits."""
    c = parse_container(c or "default")
    s = c["profile"]
    if c.get("cpus"):
        s += f",cpus={c['cpus']}"
    if c.get("memory"):
        s += f",mem={c['memory']}"
    if c.get("cpuset"):
        s += f",cpuset={c['cpuset']}"
    return s


def load_suite(name: str, rows: Rows | None = None, suites_dir: str | None = None) -> Suite:
    suites_dir = suites_dir or os.path.join(BENCH_DIR, "suites")
    if name.endswith(".toml") or os.sep in name:
        suites_dir, base = os.path.split(os.path.abspath(name))
        name = base.removesuffix(".toml")
    return parse_suite(name, _suite_doc(name, suites_dir), rows)


def list_suites(suites_dir: str | None = None) -> list[str]:
    suites_dir = suites_dir or os.path.join(BENCH_DIR, "suites")
    return sorted(os.path.basename(p)[:-5] for p in glob.glob(os.path.join(suites_dir, "*.toml")))


def select_rows(selector: str, rows: Rows) -> list[str]:
    """`all`, `all:class!=soak`, `all:class=st`, or a comma list."""
    if not selector.startswith("all"):
        return [s.strip() for s in selector.split(",") if s.strip()]
    names = [n for n, r in rows.rows.items() if r.kind != "metric"]
    if ":" in selector:
        cond = selector.split(":", 1)[1]
        m = re.fullmatch(r"(\w+)(!=|=)(\w+)", cond)
        if not m:
            raise SpecError(f"row selector {selector!r}: expected all:<field>=<value> or !=")
        field, op, value = m.groups()
        attr = "klass" if field == "class" else field

        def keep(r: Row) -> bool:
            v = str(getattr(r, attr, ""))
            return (v == value) if op == "=" else (v != value)

        names = [n for n in names if keep(rows.rows[n])]
    return names


# ---- cells -------------------------------------------------------------------


def load_cells(paths: list[str] | None = None) -> dict[str, Cell]:
    """Every bench/cells/*.toml (sorted; public.toml first) merged by name,
    plus any extra paths (files or directories) given."""
    files: list[str] = []
    cells_dir = os.path.join(BENCH_DIR, "cells")
    default = sorted(glob.glob(os.path.join(cells_dir, "*.toml")), key=lambda p: (os.path.basename(p) != "public.toml", p))
    for p in default + list(paths or []):
        if os.path.isdir(p):
            files.extend(sorted(glob.glob(os.path.join(p, "*.toml"))))
        else:
            files.append(p)
    defaults: dict[str, Any] = {}
    shapes: dict[str, dict[str, Any]] = {}
    raw: dict[str, tuple[dict[str, Any], str]] = {}
    for f in files:
        doc = _load_toml(f)
        defaults.update(doc.get("defaults", {}))
        for k, v in doc.get("shape", {}).items():
            shapes[k] = {**shapes.get(k, {}), **v}
        for k, v in doc.get("cell", {}).items():
            prev = raw.get(k, ({}, f))[0]
            raw[k] = ({**prev, **v}, f)
    return resolve_cells(defaults, shapes, raw)


def resolve_cells(defaults: dict[str, Any], shapes: dict[str, dict[str, Any]], raw: dict[str, tuple[dict[str, Any], str]]) -> dict[str, Cell]:
    out: dict[str, Cell] = {}

    def resolve(name: str, stack: tuple[str, ...] = ()) -> Cell:
        if name in out:
            return out[name]
        if name in stack:
            raise SpecError(f"cell {name}: reference loop {' -> '.join(stack + (name,))}")
        if name not in raw:
            raise SpecError(f"unknown cell {name!r}")
        c, src = raw[name]
        if "alias" in c:
            target = resolve(c["alias"], stack + (name,))
            fields = {**target.fields, "alias": c["alias"]}
            # An alias is another name for a cell (a rung of a ladder);
            # it does not inherit the target's tags, or `tag:gate` would
            # select the same host twice.
            tags = list(c.get("tags", []))
            cell = Cell(name, target.provider, fields, tags, src)
            out[name] = cell
            return cell
        fields: dict[str, Any] = dict(defaults)
        shape = c.get("shape")
        if isinstance(shape, str):
            if shape not in shapes:
                raise SpecError(f"cell {name}: unknown shape {shape!r}")
            fields.update(shapes[shape])
            fields["shape_name"] = shape
        elif isinstance(shape, dict):
            fields.update(shape)
        fields.update({k: v for k, v in c.items() if k != "shape"})
        host = c.get("host")
        if isinstance(host, str) and host.startswith("@"):
            fields["host"] = resolve(host[1:], stack + (name,)).fields.get("host")
        cell = Cell(name, str(fields.get("provider", "gce")), fields, list(c.get("tags", [])), src)
        out[name] = cell
        return cell

    for n in raw:
        resolve(n)
    return out


def select_cells(selector: str | list[str], cells: dict[str, Cell]) -> list[str]:
    """A list of names, a comma list, or `tag:a,tag:b` (union)."""
    if isinstance(selector, list):
        names = selector
    else:
        parts = [p.strip() for p in selector.split(",") if p.strip()]
        names = []
        for p in parts:
            if p.startswith("tag:"):
                t = p[4:]
                names.extend(n for n, c in cells.items() if t in c.tags and n not in names)
            elif p not in names:
                names.append(p)
    for n in names:
        if n not in cells:
            raise SpecError(f"unknown cell {n!r}")
    return names


def lint_public(paths: list[str] | None = None) -> list[str]:
    """Problems in the public tree: a public cell whose host image project
    is not a public one, and any internal overlay marker in a public file
    (cell files, bench/cb itself, rows, suites). Returns the offending
    lines."""
    problems = []
    cells_dir = os.path.join(BENCH_DIR, "cells")
    overlays = [p for p in glob.glob(os.path.join(cells_dir, "*.toml")) if os.path.basename(p).startswith("internal")]
    public_cells = [p for p in glob.glob(os.path.join(cells_dir, "*.toml")) if p not in overlays]
    markers: list[str] = []
    for p in overlays:
        markers += [str(m).lower() for m in _load_toml(p).get("lint", {}).get("markers", [])]
    for p in public_cells:
        doc = _load_toml(p)
        for name, c in doc.get("cell", {}).items():
            host = c.get("host")
            if isinstance(host, dict) and host.get("project") and host["project"] not in PUBLIC_IMAGE_PROJECTS:
                problems.append(f"{os.path.relpath(p, ROOT_DIR)}: cell {name}: image project {host['project']!r} is not public (move the cell to an internal overlay)")
    files = public_cells + glob.glob(os.path.join(BENCH_DIR, "cb", "*.py")) + glob.glob(os.path.join(BENCH_DIR, "cb", "*.sh"))
    files += glob.glob(os.path.join(BENCH_DIR, "cb", "tests", "*.py"))
    files += [os.path.join(BENCH_DIR, f) for f in ("rows.toml", "README.md", "bench.mk", "Dockerfile.cb")]
    files += glob.glob(os.path.join(BENCH_DIR, "suites", "*.toml"))
    for f in paths or files:
        with open(f, encoding="utf-8") as fh:
            for i, line in enumerate(fh, 1):
                for m in markers:
                    if m and m in line.lower():
                        problems.append(f"{os.path.relpath(f, ROOT_DIR)}:{i}: internal marker")
    return problems
