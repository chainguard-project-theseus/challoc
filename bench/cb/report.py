"""From samples to verdicts: summary.json and the markdown table.

Input: one or more run directories (samples.jsonl, blocks.jsonl,
manifest.json, host.json). Samples are grouped by comparability key and
never pooled across keys; within a key every (vm, block) is one paired
unit (stats.py). Gate comparisons are every non-control variant against
the first variant of the run (quick: head vs base; aa: aa vs base);
context comparisons are every gate variant against every control
(jemalloc, glibc), reported and never gated.
"""

from __future__ import annotations

import collections
import json
import math
import os
import statistics
from typing import Any

from . import stats
from .spec import Rows

NOISE_STEAL_PCT = 1.0
VM_FLAGGED_FRACTION = 0.2


def _jsonl(path: str) -> list[dict[str, Any]]:
    out = []
    if not os.path.exists(path):
        return out
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def load_dir(d: str) -> dict[str, Any]:
    def j(name: str) -> Any:
        p = os.path.join(d, name)
        return json.load(open(p)) if os.path.exists(p) else {}

    return {
        "dir": d,
        "samples": _jsonl(os.path.join(d, "samples.jsonl")),
        "blocks": _jsonl(os.path.join(d, "blocks.jsonl")),
        "manifest": j("manifest.json"),
        "host": j("host.json"),
        "plan": j("plan.json"),
    }


def _vm_key(s: dict[str, Any]) -> str:
    # One run on one host is one replicate; two local runs on one machine
    # count as two (they are separate processes, builds cached or not).
    return f"{s['vm']['name']}#{s['run']}"


def flag_blocks(blocks: list[dict[str, Any]]) -> tuple[set[tuple[str, int]], dict[str, str]]:
    """(flagged (vm, block) pairs, excluded vm -> reason) from the block
    health records: a sentinel more than 3 MAD from the vm's median, or
    steal above 1%; a vm with more than 20% flagged blocks is excluded."""
    flagged: set[tuple[str, int]] = set()
    excluded: dict[str, str] = {}
    by_vm: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for b in blocks:
        if not b.get("warmup"):
            by_vm[f"{b['vm']}#{b['run']}"].append(b)
    for vm, bs in by_vm.items():
        sent = {b["block"]: b["sentinel"]["total_ms"] for b in bs if b.get("sentinel", {}).get("total_ms") is not None}
        for i in stats.outside_mad(sent):
            flagged.add((vm, i))
        for b in bs:
            if b.get("steal_pct", 0) > NOISE_STEAL_PCT:
                flagged.add((vm, b["block"]))
        n = sum(1 for (v, _) in flagged if v == vm)
        if bs and n / len(bs) > VM_FLAGGED_FRACTION and len(bs) >= 5:
            excluded[vm] = f"{n} of {len(bs)} blocks flagged (sentinel or steal)"
    return flagged, excluded


def _median(xs: list[float]) -> float | None:
    return statistics.median(xs) if xs else None


def compare_row(
    samples: list[dict[str, Any]],
    row: str,
    cand: str,
    ref: str,
    *,
    delta_pct: float,
    alpha: float,
    flagged: set[tuple[str, int]],
    excluded: dict[str, str],
    fixed_work: bool,
    placement: str = "paired",
    aa_history: list[float] | None = None,
) -> dict[str, Any]:
    rs = [s for s in samples if s["row"] == row and s["variant"] in (cand, ref) and not s.get("warmup")]
    broken = [s for s in rs if s.get("score") is None or s.get("error")]
    higher = (rs[0]["better"] == "higher") if rs else True
    cells: dict[tuple[str, int], dict[str, list[dict[str, Any]]]] = collections.defaultdict(lambda: collections.defaultdict(list))
    n_flagged = 0
    for s in rs:
        vm = _vm_key(s)
        if vm in excluded:
            continue
        if (vm, s["block"]) in flagged:
            n_flagged += 1
            continue
        if s.get("score") is None:
            continue
        cells[(vm, s["block"])][s["variant"]].append(s)
    groups: stats.Groups = collections.defaultdict(list)
    pmc_groups: dict[str, stats.Groups] = {"instructions": collections.defaultdict(list), "cycles": collections.defaultdict(list)}
    block_index: dict[str, list[tuple[int, float]]] = collections.defaultdict(list)
    for (vm, blk), by in sorted(cells.items()):
        if cand not in by or ref not in by:
            continue
        xc = [stats.orient(s["score"], higher) for s in by[cand]]
        xr = [stats.orient(s["score"], higher) for s in by[ref]]
        groups[vm].append(statistics.fmean(xc) - statistics.fmean(xr))
        block_index[vm].append((blk, statistics.fmean(xr)))
        for ev, g in pmc_groups.items():
            def per_op(s: dict[str, Any]) -> float | None:
                p = s.get("pmc") or {}
                v = p.get(ev)
                if not v:
                    return None
                ops = s.get("ops")
                return v / ops if ops else (v if fixed_work else None)

            pc = [per_op(s) for s in by[cand]]
            pr = [per_op(s) for s in by[ref]]
            if all(x for x in pc + pr):
                g[vm].append(statistics.fmean(math.log(x) for x in pc) - statistics.fmean(math.log(x) for x in pr))
    level = 1.0 - alpha
    if placement == "one-variant-per-vm":
        # Each VM ran one variant: each VM's mean oriented log score per
        # variant, compared unpaired.
        per_vm: dict[str, dict[str, list[float]]] = collections.defaultdict(lambda: collections.defaultdict(list))
        for (vm, _blk), by in cells.items():
            for lab, ss in by.items():
                per_vm[lab][vm] += [stats.orient(x["score"], higher) for x in ss]
        cmp_: Any = stats.Unpaired(
            [statistics.fmean(v) for v in per_vm[cand].values()],
            [statistics.fmean(v) for v in per_vm[ref].values()],
            level=level,
        )
    else:
        # A paired row is only ever compared paired: with no (vm, block)
        # that ran both variants it has no estimate (inconclusive).
        cmp_ = stats.Comparison(dict(groups), level=level)
        if not groups and not broken:
            cmp_.flags.append("no paired blocks")
    m, calibrated = stats.margin(delta_pct, aa_history or [])
    lo, hi = cmp_.ci_pct
    v = stats.verdict(cmp_.est_pct, lo, hi, m, broken=bool(broken))
    flags = list(cmp_.flags)
    if not calibrated:
        flags.append("uncalibrated")
    if n_flagged:
        flags.append(f"{n_flagged} runs in flagged blocks excluded")
    for lab in (cand, ref):
        xs = [s["score"] for s in rs if s["variant"] == lab and s.get("score") is not None]
        if stats.bimodal(xs):
            flags.append(f"bimodal {lab}")
    # Drift of the reference across blocks (Spearman over block index).
    for vm, pts in block_index.items():
        if len(pts) >= 5:
            rho = stats.spearman([p[0] for p in pts], [p[1] for p in pts])
            if abs(rho) > 0.8:
                flags.append(f"drift {ref} rho={rho:+.2f}")
    pmc_delta: dict[str, Any] = {}
    for ev, g in pmc_groups.items():
        g2 = {k: val for k, val in g.items() if val}
        if g2 and sum(len(x) for x in g2.values()) >= 2:
            c2 = stats.Comparison(g2, level=level)
            pmc_delta[ev] = {"est_pct": c2.est_pct, "ci": list(c2.ci_pct)}
    if "instructions" in pmc_delta:
        ins = pmc_delta["instructions"]["est_pct"]
        if abs(cmp_.est_pct) > m and abs(ins) < 0.5:
            flags.append("placement?")
        if fixed_work and abs(ins) > 1.0:
            flags.append(f"instructions/op {ins:+.1f}%")
    med = {lab: _median([s["score"] for s in rs if s["variant"] == lab and s.get("score") is not None]) for lab in (cand, ref)}
    rss = {lab: _median([s["rt"]["maxrss_kib"] / 1024 for s in rs if s["variant"] == lab and s.get("rt", {}).get("maxrss_kib")]) for lab in (cand, ref)}
    return {
        "row": row,
        "candidate": cand,
        "reference": ref,
        "unit": rs[0]["unit"] if rs else "",
        "better": "higher" if higher else "lower",
        "median": med,
        "rss_mib": rss,
        "est_pct": None if math.isnan(cmp_.est_pct) else cmp_.est_pct,
        "ci": [lo, hi],
        "level": level,
        "p": cmp_.p,
        "margin_pct": m,
        "calibrated": calibrated,
        "n_vm": cmp_.n_hosts,
        "n_blocks": cmp_.n_blocks,
        "i2": cmp_.i2,
        "q_p": cmp_.q_p,
        "outliers": cmp_.outliers,
        "verdict": v,
        "broken": [f"{s['variant']} block {s['block']}: {s.get('error') or 'no score'}" for s in broken][:5],
        "flags": flags,
        "pmc_delta": pmc_delta,
    }


def expand_metrics(samples: list[dict[str, Any]], rows: Rows) -> list[dict[str, Any]]:
    """Every sample of a driver row with `metrics` also becomes one sample
    per metric (row `<row>.<metric>`, the metric's value as the score), so
    RSS after settle or p99 get a verdict of their own."""
    out = list(samples)
    for s in samples:
        r = rows.rows.get(s["row"])
        if r is None or not r.metrics:
            continue
        for m in r.metrics:
            d = rows.rows[f"{r.name}.{m['name']}"]
            v = (s.get("extra") or {}).get(m["name"])
            parent_ok = s.get("score") is not None and not s.get("error")
            ok = parent_ok and isinstance(v, (int, float)) and v > 0
            if parent_ok and not ok:
                # The driver ran but did not report this figure (a phase
                # too short to happen): no sample, not a broken run.
                continue
            out.append({**s, "row": d.name, "score": float(v) if ok else None, "unit": d.unit, "better": d.better, "error": "" if ok else (s.get("error") or "broken"), "pmc": None})
    return out


def aa_key(cell: str, row: str) -> str:
    return f"{cell}/{row}"


def load_aa_history(path: str) -> dict[str, list[float]]:
    """A/A estimates by cell and row (aa_key) from a JSONL file, one line
    per (A/A run, cell, row): {"cell": ..., "row": ..., "est_pct": ...}.
    A line without its cell and row cannot calibrate a row's margin."""
    out: dict[str, list[float]] = collections.defaultdict(list)
    with open(path) as f:
        for i, ln in enumerate(f, 1):
            if not ln.strip():
                continue
            d = json.loads(ln)
            if not d.get("cell") or not d.get("row") or d.get("est_pct") is None:
                raise ValueError(f"{path}:{i}: an A/A history line needs cell, row and est_pct")
            out[aa_key(d["cell"], d["row"])].append(float(d["est_pct"]))
    return dict(out)


def analyze(runs: list[dict[str, Any]], rows: Rows, *, cand: str | None = None, ref: str | None = None, aa_history: dict[str, list[float]] | None = None) -> dict[str, Any]:
    samples = expand_metrics([s for r in runs for s in r["samples"]], rows)
    blocks = [b for r in runs for b in r["blocks"]]
    keys = sorted({s["comparability"] for s in samples})
    manifest = runs[0]["manifest"] if runs else {}
    plan = runs[0]["plan"] if runs else {}
    # Every run's variants (a one-variant-per-vm VM lists only its own), in
    # the launch's order where the manifests carry it.
    variants = []
    for r in runs:
        for v in r["manifest"].get("variants") or r["plan"].get("variants") or []:
            if v.get("label") not in [x.get("label") for x in variants]:
                variants.append(v)
    order = next((r["manifest"]["launch_variants"] for r in runs if r["manifest"].get("launch_variants")), None)
    if order:
        variants.sort(key=lambda v: order.index(v["label"]) if v["label"] in order else len(order))
    labels = [v.get("label") for v in variants]
    controls = [v.get("label") for v in variants if v.get("role") == "control"]
    gate_labels = [lab for lab in labels if lab not in controls]
    if cand and ref:
        gate_pairs = [(cand, ref)]
    else:
        gate_pairs = [(c, gate_labels[0]) for c in gate_labels[1:]]
    context_pairs = [(c, k) for c in gate_labels[1:] or gate_labels[:1] for k in controls]
    if cand and ref:
        context_pairs = [(cand, k) for k in controls if k not in (cand, ref)]
    alpha = float(plan.get("alpha", 0.0294))
    flagged, excluded = flag_blocks(blocks)
    # A launch's VMs each ran one shard (a subset of the rows): the row
    # order is the union of their plans, in order of first appearance.
    row_order: list[str] = []
    for r in runs:
        for name in r["plan"].get("rows") or []:
            if name not in row_order:
                row_order.append(name)
    row_order += sorted({s["row"] for s in samples} - set(row_order))
    # A driver row's metric rows right after it.
    metric_of = {n: rows.rows[n].parent for n in row_order if n in rows.rows and rows.rows[n].kind == "metric"}
    ordered: list[str] = []
    for name in row_order:
        if name in metric_of and metric_of[name] in row_order:
            continue
        ordered.append(name)
        ordered += sorted((m for m in row_order if metric_of.get(m) == name), key=list(rows.rows).index)
    row_order = ordered
    results = []
    for key in keys:
        ks = [s for s in samples if s["comparability"] == key]
        present = {s["row"] for s in ks}
        for kind, pairs in (("gate", gate_pairs), ("context", context_pairs)):
            for c, r in pairs:
                for row in row_order:
                    if row not in present or row not in rows.rows:
                        continue
                    vs = {s["variant"] for s in ks if s["row"] == row}
                    if c not in vs or r not in vs:
                        continue
                    cell = next((s["cell"] for s in ks if s["row"] == row), "")
                    res = compare_row(
                        ks,
                        row,
                        c,
                        r,
                        delta_pct=rows.delta_pct(rows.rows[row]),
                        alpha=alpha,
                        flagged=flagged,
                        excluded=excluded,
                        fixed_work=rows.rows[row].fixed_work,
                        placement=rows.rows[row].placement,
                        aa_history=(aa_history or {}).get(aa_key(cell, row)),
                    )
                    res["kind"] = kind
                    res["comparability"] = key
                    res["cell"] = cell
                    results.append(res)
    gate = [r for r in results if r["kind"] == "gate"]
    vd = {f"{r['row']}|{r['comparability']}|{r['candidate']}": r["verdict"] for r in gate}
    pv = {f"{r['row']}|{r['comparability']}|{r['candidate']}": r["p"] for r in gate}
    head = stats.headline(vd, pv, alpha) if gate else "INFRA"
    holm = stats.holm(pv, alpha) if pv else {}
    for r in gate:
        r["holm"] = holm.get(f"{r['row']}|{r['comparability']}|{r['candidate']}", False)
    info = [r for r in results if r["kind"] == "context"]
    if info:
        q = stats.bh({str(i): r["p"] for i, r in enumerate(info)})
        for i, r in enumerate(info):
            r["q"] = q[str(i)]
    pmc_notes = sorted({s.get("pmc_note", "") for s in samples if not s.get("pmc") or "cycles" not in (s.get("pmc") or {})} - {""})
    # What each comparability key booted: the kernel uname -r reported, the
    # image family asked for, the features line, the container profile.
    key_hosts: dict[str, dict[str, Any]] = {}
    for r in runs:
        h = r["host"]
        k = h.get("comparability")
        if k and k not in key_hosts:
            key_hosts[k] = {
                "cell": h.get("cell"),
                "kernel": h.get("kernel"),
                "cpu": (h.get("cpu") or {}).get("model"),
                "requested": h.get("requested"),
                "features": h.get("features"),
                "container": h.get("container_profile"),
                "thp": (h.get("thp") or {}).get("enabled"),
            }
    return {
        "schema": "challoc.bench.summary/v1",
        "runs": list(dict.fromkeys(r["manifest"].get("run") for r in runs)),
        "n_vms": len({_vm_key(s) for s in samples}),
        "suite": manifest.get("suite"),
        "cells": sorted({s["cell"] for s in samples}),
        "comparability": keys,
        "indicative": any(r["host"].get("indicative") for r in runs),
        "alpha": alpha,
        "variants": variants,
        "notes": manifest.get("notes", []),
        "host": runs[0]["host"] if runs else {},
        "key_hosts": key_hosts,
        "excluded_vms": excluded,
        "flagged_blocks": len(flagged),
        "results": results,
        "headline": head,
        "exit": stats.HEADLINE_EXIT.get(head, 3),
        "counters": "none: " + "; ".join(pmc_notes[:2]) if pmc_notes else "per-run perf_event_open groups",
        "wall_s": sum(r["manifest"].get("wall_s", 0) or 0 for r in runs),
        "n_samples": len(samples),
    }


def analyze_dirs(dirs: list[str], rows: Rows, **kw: Any) -> dict[str, Any]:
    return analyze([load_dir(d) for d in dirs], rows, **kw)


def _fmt(x: float | None, nd: int = 3) -> str:
    if x is None:
        return "-"
    if abs(x) >= 100:
        return f"{x:.1f}"
    return f"{x:.{nd}g}"


def _pct(x: float | None) -> str:
    if x is None or (isinstance(x, float) and (math.isnan(x) or math.isinf(x))):
        return "-"
    return f"{x:+.1f}%"


def render(summary: dict[str, Any]) -> str:
    out = []
    h = summary.get("host", {})
    lvl = summary["results"][0]["level"] if summary["results"] else 1 - summary["alpha"]
    out.append(f"# cb {summary.get('suite')}: {', '.join(r for r in summary['runs'] if r)}")
    out.append("")
    ind = " (indicative: one local host, never pooled with GCE cells, never a verdict of record)" if summary.get("indicative") else ""
    out.append(f"- cell: {', '.join(summary['cells'])}{ind}")
    kh = summary.get("key_hosts") or {}
    for k in summary["comparability"]:
        out.append(f"- comparability: `{k}`")
        x = kh.get(k)
        if x and len(summary["comparability"]) > 1:
            req = x.get("requested") or {}
            asked = f", image {req.get('image')} (family {req.get('family')})" if req.get("family") else ""
            out.append(f"  - {x.get('cell')}: kernel {x.get('kernel')}{asked}, {x.get('cpu')}, THP {x.get('thp')}, container {(x.get('container') or {}).get('profile', '-')}")
            if x.get("features"):
                out.append(f"  - {x['features']}")
    if h:
        out.append(f"- host: {h.get('arch')} {h.get('cpu', {}).get('model')}, {h.get('nproc')} CPUs, kernel {h.get('kernel')}, THP {h.get('thp', {}).get('enabled')}, governor {h.get('governor')}, {h.get('glibc')}, load at start {h.get('quiet_gate', {}).get('load1', '-')}")
        if h.get("features"):
            out.append(f"- {h['features']}")
        if h.get("requested", {}).get("family") and len(summary["comparability"]) <= 1:
            out.append(f"- image: {h['requested'].get('image')} (family {h['requested'].get('family')}), container {(h.get('container_profile') or {}).get('profile', '-')}")
    for v in summary["variants"]:
        src = f"{v['source']} = {v['commit'][:12]}" if v.get("kind") == "challoc" else v.get("source")
        conf = f", MALLOC_CONF={v['conf']}" if v.get("conf") else ""
        role = " (control)" if v.get("role") == "control" else ""
        out.append(f"- {v['label']}{role}: {src}{conf}, {v.get('lib') or 'no LD_PRELOAD'} sha256 {v.get('lib_sha256', '')[:16]}")
    for n in summary.get("notes", []):
        out.append(f"- {n}")
    out.append(f"- counters: {summary.get('counters')}")
    if summary.get("excluded_vms"):
        for vm, why in summary["excluded_vms"].items():
            out.append(f"- excluded {vm}: {why}")
    out.append("")
    for kind, title in (("gate", "Gate"), ("context", "Context (not gated)")):
        res = [r for r in summary["results"] if r["kind"] == kind]
        if not res:
            continue
        out.append(f"## {title}")
        out.append("")
        multi = len(summary["comparability"]) > 1
        cellh, cellsep = ("cell | ", "---|") if multi else ("", "")
        out.append(f"| {cellh}row | unit | {res[0]['reference']} | candidate | cand vs ref | {lvl * 100:.1f}% CI | margin | verdict | VMs | blocks | flags |")
        out.append(f"|{cellsep}---|---|---:|---:|---:|---|---:|---|---:|---:|---|")
        for r in res:
            ref, cand = r["reference"], r["candidate"]
            v = r["verdict"]
            if v == "sub-threshold":
                v = f"sub-threshold (~{_pct(r['est_pct'])})"
            if kind == "gate" and r.get("holm") and r["verdict"] in ("worse", "better"):
                v += " (Holm)"
            flags = list(r["flags"])
            if r.get("pmc_delta", {}).get("instructions"):
                flags.append(f"instr/op {_pct(r['pmc_delta']['instructions']['est_pct'])}")
            if r.get("broken"):
                flags.append("broken: " + "; ".join(r["broken"][:2]))
            flags = [f for f in flags if f not in ("uncalibrated", "few blocks", "VM effect unestimated")] + (
                ["uncalibrated"] if "uncalibrated" in r["flags"] else []
            )
            out.append(
                (f"| {r.get('cell', '')} " if multi else "")
                + f"| {r['row']} | {r['unit']} | {_fmt(r['median'][ref])} | {cand} {_fmt(r['median'][cand])} | {_pct(r['est_pct'])} | "
                f"[{_pct(r['ci'][0])}, {_pct(r['ci'][1])}] | {r['margin_pct']:.1f}% | {v} | {r['n_vm']} | {r['n_blocks']} | {', '.join(flags)} |"
            )
        out.append("")
    common = sorted({f for r in summary["results"] for f in r["flags"] if f in ("few blocks", "VM effect unestimated")})
    out.append(
        "Positive percentages mean the candidate did worse (slower, lower throughput, lower occupancy); "
        "medians are of the raw scores, the estimate is the paired within-block contrast."
        + (f" All rows: {', '.join(common)}." if common else "")
    )
    out.append("")
    out.append(f"**{summary['headline']}** (exit {summary['exit']}), {summary['n_samples']} runs, {summary.get('wall_s', 0) / 60:.1f} min")
    return "\n".join(out) + "\n"


def simulate_runs(dirs: list[str], rows: Rows, *, k: int, draws: int, seed: int = 1) -> dict[str, Any]:
    """The false-positive rate of a k-replicate launch, from an A/A fan-out
    with more replicates (`cb fp`): every draw picks k replicates per cell
    (a replicate is one VM per shard, as in a real launch), analyzes only
    their samples, and counts each gate row's verdict and the headline.
    In an A/A every `worse` or `better` is a false positive."""
    import itertools
    import random as _random

    loaded = [load_dir(d) for d in dirs]
    by_cell: dict[str, dict[int, list[dict[str, Any]]]] = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in loaded:
        h = r["host"]
        rep = (h.get("launch") or {}).get("replicate")
        if rep is None or not r["samples"]:
            continue
        by_cell[h.get("cell", "?")][int(rep)].append(r)
    rng = _random.Random(seed)
    combos = {c: list(itertools.combinations(sorted(reps), k)) for c, reps in by_cell.items()}
    for c, cs in combos.items():
        if not cs:
            raise ValueError(f"cell {c}: fewer than {k} replicates")
    counts: dict[str, collections.Counter[str]] = collections.defaultdict(collections.Counter)
    heads: collections.Counter[str] = collections.Counter()
    ests: dict[str, list[float]] = collections.defaultdict(list)
    for _ in range(draws):
        pick = []
        for c, cs in combos.items():
            for rep in rng.choice(cs):
                pick += by_cell[c][rep]
        s = analyze(pick, rows)
        heads[s["headline"]] += 1
        for res in s["results"]:
            if res["kind"] != "gate":
                continue
            key = f"{res.get('cell')}/{res['row']}"
            counts[key][res["verdict"]] += 1
            if res.get("est_pct") is not None:
                ests[key].append(res["est_pct"])
    per_row = {}
    for key, cnt in sorted(counts.items()):
        n = sum(cnt.values())
        xs = sorted(abs(x) for x in ests[key])
        per_row[key] = {
            "draws": n,
            "verdicts": dict(cnt),
            "fp_rate": (cnt["worse"] + cnt["better"]) / n if n else None,
            "resolved_rate": (cnt["same"] + cnt["sub-threshold"]) / n if n else None,
            "q99_abs_est_pct": xs[min(len(xs) - 1, int(0.99 * len(xs)))] if xs else None,
        }
    total = sum(heads.values())
    return {
        "k": k,
        "draws": draws,
        "replicates": {c: sorted(r) for c, r in by_cell.items()},
        "rows": per_row,
        "headline": dict(heads),
        "headline_fp_rate": (heads["REGRESSION"] + heads["IMPROVEMENT"]) / total if total else None,
        "row_fp_rate": sum(c["worse"] + c["better"] for c in counts.values()) / max(1, sum(sum(c.values()) for c in counts.values())),
    }


def variance_components(dirs: list[str], cand: str | None = None, ref: str | None = None) -> dict[str, Any]:
    """Per (cell, row) of an A/A (or any paired) launch: the within-VM
    spread of the per-block contrast (sigma_w, percent) and the spread of
    the VM means beyond it (sigma_vm: the VM x variant interaction), and
    the CI half-width they predict for R VMs of B blocks,
    2 * sqrt(sigma_vm^2 / R + sigma_w^2 / (R * B)), which is what `replicas`
    and `blocks` trade."""
    loaded = [load_dir(d) for d in dirs]
    samples = [s for r in loaded for s in r["samples"] if not s.get("warmup") and s.get("score") is not None]
    labels = []
    for r in loaded:
        for v in r["manifest"].get("variants", []):
            if v.get("role") != "control" and v["label"] not in labels:
                labels.append(v["label"])
    ref = ref or labels[0]
    cand = cand or labels[1]
    cells: dict[tuple[str, str], dict[str, dict[int, dict[str, list[float]]]]] = collections.defaultdict(
        lambda: collections.defaultdict(lambda: collections.defaultdict(lambda: collections.defaultdict(list)))
    )
    for s in samples:
        if s["variant"] in (cand, ref):
            cells[(s["cell"], s["row"])][_vm_key(s)][s["block"]][s["variant"]].append(stats.orient(s["score"], s["better"] == "higher"))
    out = {}
    for (cell, row), vms in sorted(cells.items()):
        means, within, nb = [], [], []
        for blocks in vms.values():
            ds = [statistics.fmean(b[cand]) - statistics.fmean(b[ref]) for b in blocks.values() if b.get(cand) and b.get(ref)]
            if len(ds) >= 2:
                means.append(statistics.fmean(ds))
                within.append(statistics.variance(ds))
                nb.append(len(ds))
        if len(means) < 3:
            continue
        sw2 = statistics.fmean(within)
        b = statistics.fmean(nb)
        svm2 = max(0.0, statistics.variance(means) - sw2 / b)
        pred = {f"{R}x{B}": round(200 * math.sqrt(svm2 / R + sw2 / (R * B)), 2) for R in (3, 4, 6, 8) for B in (3, 5)}
        out[f"{cell}/{row}"] = {"n_vm": len(means), "sigma_w_pct": round(100 * math.sqrt(sw2), 3), "sigma_vm_pct": round(100 * math.sqrt(svm2), 3), "halfwidth_pct": pred}
    return out
