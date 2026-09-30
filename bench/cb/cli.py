"""The cb command line.

    cb run SUITE [--base REV] [--head REV] [--rows a,b] [--blocks N] ...
    cb launch SUITE [--base REV] [--head REV] [--cells SEL] [--replicas N] ...
    cb gc [--older-than MIN] [--run RUN] [--dry-run]
    cb compare RUN_DIR... [--cand LABEL --ref LABEL] [--json]
    cb report RUN_DIR [--json]
    cb plan SUITE [--blocks N] [--variants ...]
    cb rows | cb suites | cb cells [SELECTOR] [--lint]
    cb build REV...
    cb preflight | cb host

Exit codes (run, compare, report): 0 neutral or improvement, 1 regression
or broken, 2 inconclusive, 3 infrastructure failure.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

from . import host as hostmod
from . import report
from .plan import make_plan, parse_variant
from .spec import ROOT_DIR, SpecError, list_suites, lint_public, load_cells, load_rows, load_suite, select_cells
from .variants import BuildError


def _add_run_args(p: argparse.ArgumentParser) -> None:
    p.add_argument("suite", nargs="?", help="a suite under bench/suites (quick, aa, smoke) or a path to one")
    p.add_argument("--base", default="main", help="rev of the `base` variant (default main)")
    p.add_argument("--head", default="HEAD", help="rev of the `head` variant (default HEAD)")
    p.add_argument("--variants", help="comma list overriding the suite's variants (base,head,aa=base,x=git:REV,jemalloc,glibc,...; LABEL=SRC@MALLOC_CONF)")
    p.add_argument("--controls", help="comma list overriding the suite's controls (e.g. jemalloc,glibc; empty for none)")
    p.add_argument("--rows", help="comma list overriding the suite's rows")
    p.add_argument("--blocks", type=int, help="timed blocks on this host (default the suite's [local].blocks)")
    p.add_argument("--warmup", type=int, help="discarded warm-up blocks (default the suite's warmup_blocks)")
    p.add_argument("--seed", type=int, help="plan seed (default random, recorded)")
    p.add_argument("--var", action="append", help="NAME=VALUE for a {NAME} in rows.toml (e.g. valkey_secs=10)")
    p.add_argument("--cell", default="local", help="the cell this host is (default local)")
    p.add_argument("--cells-path", action="append", help="extra cell files or directories (overlays)")
    p.add_argument("--rows-file", help="a rows file other than bench/rows.toml")
    p.add_argument("--bin", help="directory of the mimalloc-bench programs (default target/bench-bin)")
    p.add_argument("--out", help="output directory (default target/results/cb/<run>)")
    p.add_argument("--run-id", help="run id (default <utc>-<head>-<suite>-<rand>)")
    p.add_argument("--smoke", action="store_true", help="one block, no warm-up, no verdict: exit 0 when every run parsed")
    p.add_argument("--build-only", action="store_true", help="build the variants and stop")
    p.add_argument("--dry-run", action="store_true", help="print the plan and stop (after the builds)")
    p.add_argument("--no-bench-build", action="store_true", help="fail instead of running bench/build.sh when programs are missing")
    p.add_argument("--launch", help="a cb launch file: run this VM's entry of it (bench/cb/boot.sh)")
    p.add_argument("--vm", help="with --launch: this VM's name")
    p.add_argument("--sources", help="with --launch: directory of the shipped <tree>.tar.gz sources")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="cb", description="challoc benchmark runner (bench/README.md)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    _add_run_args(sub.add_parser("run", help="run a suite on this host (dev container or Linux)"))

    from .gce import add_launch_args

    add_launch_args(sub.add_parser("launch", help="run a suite on GCE cells: spot VMs, one per (cell, shard, replicate); verdicts pooled"))
    p = sub.add_parser("gc", help="delete stray cb VMs (name cb-*, label purpose=challoc-bench)")
    p.add_argument("--older-than", type=float, default=60, help="minutes (default 60; 0 for all)")
    p.add_argument("--run", help="only this run's VMs")
    p.add_argument("--project")
    p.add_argument("--dry-run", action="store_true")

    p = sub.add_parser("compare", help="verdicts from existing run directories (pooled when their comparability keys match)")
    p.add_argument("dirs", nargs="+")
    p.add_argument("--cand", help="candidate label (default: every gate variant)")
    p.add_argument("--ref", help="reference label (default: the run's first variant)")
    p.add_argument("--aa-history", help="JSONL of past A/A summaries: margin calibration")
    p.add_argument("--json", action="store_true")

    p = sub.add_parser("report", help="re-render one run's summary")
    p.add_argument("dir")
    p.add_argument("--json", action="store_true")

    p = sub.add_parser("plan", help="print a suite's plan without running it")
    p.add_argument("suite")
    p.add_argument("--blocks", type=int)
    p.add_argument("--warmup", type=int)
    p.add_argument("--variants")
    p.add_argument("--rows")
    p.add_argument("--seed", type=int, default=1)

    sub.add_parser("rows", help="list the rows")
    sub.add_parser("suites", help="list the suites")
    p = sub.add_parser("cells", help="list the cells (optionally a selector: tag:gate, a,b)")
    p.add_argument("selector", nargs="?")
    p.add_argument("--cells-path", action="append")
    p.add_argument("--lint", action="store_true", help="fail when a public file names an internal image or project")

    p = sub.add_parser("build", help="build challoc variants from revs (in the dev container)")
    p.add_argument("revs", nargs="+")

    sub.add_parser("preflight", help="host-side checks before a local run: load, foreign containers")
    sub.add_parser("host", help="print this host's capture (host.json)")

    a = ap.parse_args(argv)
    try:
        return _dispatch(a)
    except (SpecError, BuildError) as e:
        print(f"cb: {e}", file=sys.stderr)
        return 3


def _dispatch(a: argparse.Namespace) -> int:
    if a.cmd == "run":
        from .run import execute

        if not a.suite and not a.launch:
            raise SpecError("cb run: a suite (or --launch FILE --vm NAME)")
        return execute(a)
    if a.cmd in ("launch", "gc"):
        from . import gce

        try:
            return gce.launch(a) if a.cmd == "launch" else gce.gc(a)
        except gce.InfraError as e:
            print(f"cb: {e}", file=sys.stderr)
            return 3
    if a.cmd in ("compare", "report"):
        rows = load_rows()
        if a.cmd == "report":
            s = report.analyze_dirs([a.dir], rows)
        else:
            hist = []
            if a.aa_history:
                with open(a.aa_history) as f:
                    hist = [json.loads(ln)["est_pct"] for ln in f if ln.strip()]
            s = report.analyze_dirs(a.dirs, rows, cand=a.cand, ref=a.ref or (None if not a.cand else _first_label(a.dirs[0])), aa_history=hist)
        print(json.dumps(s, indent=1, default=str) if a.json else report.render(s), end="" if not a.json else "\n")
        return s["exit"]
    if a.cmd == "plan":
        rows = load_rows()
        suite = load_suite(a.suite, rows)
        refs = {"base": "main", "head": "HEAD"}
        items = a.variants.split(",") if a.variants else suite.variants
        specs = [parse_variant(v, refs) for v in items] + [parse_variant(c, refs, "control") for c in suite.controls]
        plan = make_plan(
            suite,
            rows,
            specs,
            run_id="plan",
            cell="local",
            blocks=a.blocks or int(suite.local.get("blocks", suite.blocks)),
            warmup_blocks=suite.warmup_blocks if a.warmup is None else a.warmup,
            seed=a.seed,
            row_names=a.rows.split(",") if a.rows else None,
        )
        for b in plan.blocks:
            print(f"block {b.index}{' (warm-up)' if b.warmup else ''}:")
            for r in b.rows:
                print(f"  {r:<14} {' '.join(b.order[r])}")
        print(f"{plan.runs()} runs, about {plan.est_seconds(rows) / 60:.1f} min on one host; alpha {plan.alpha} per look")
        return 0
    if a.cmd == "rows":
        rows = load_rows()
        for r in rows.rows.values():
            print(f"{r.name:<22} {r.klass:<3} {r.better:<6} {r.unit:<12} {' '.join(r.cmd)}")
        return 0
    if a.cmd == "suites":
        rows = load_rows()
        for n in list_suites():
            s = load_suite(n, rows)
            print(f"{n:<8} {s.description}")
        return 0
    if a.cmd == "cells":
        if a.lint:
            probs = lint_public()
            for p in probs:
                print(p)
            return 1 if probs else 0
        cells = load_cells(a.cells_path or [])
        names = select_cells(a.selector, cells) if a.selector else list(cells)
        for n in names:
            c = cells[n]
            host = c.get("host")
            hs = f"{host.get('project')}/{host.get('family')}" if isinstance(host, dict) else (host or "-")
            print(f"{n:<20} {c.provider:<5} {str(c.get('machine', '-')):<18} {hs:<45} {','.join(c.tags)}  ({os.path.relpath(c.source, ROOT_DIR)})")
        return 0
    if a.cmd == "build":
        from .plan import VariantSpec
        from .variants import prepare

        specs = [VariantSpec(r, "challoc", r) for r in a.revs]
        for v in prepare(ROOT_DIR, specs, os.path.join(ROOT_DIR, "target", "cb"), {}):
            print(f"{v.label} {v.commit[:12]} {v.lib} {v.sha256[:16]}{' (cached)' if v.cached else ''}")
        return 0
    if a.cmd == "preflight":
        w = hostmod.preflight()
        for x in w:
            print(f"cb preflight: WARNING {x}")
        return 0
    if a.cmd == "host":
        print(json.dumps(hostmod.capture("local"), indent=1))
        return 0
    return 3


def _first_label(d: str) -> str | None:
    m = report.load_dir(d)["manifest"]
    vs = [v["label"] for v in m.get("variants", []) if v.get("role") != "control"]
    return vs[0] if vs else None
