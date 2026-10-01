"""Score parsers for the rows of bench/rows.toml, ported from bench/lib.sh
(run_row's awk scorers, ops_of, run_churn).

Each parser takes the program's combined stdout/stderr and the rt record
(bench/tools/rt.c, RT_JSON) and returns a Parsed: the score, the operation
count (for per-operation counters; None when the program reports none) and
extra metrics. A parser that cannot find its line returns score None, which
the executor records as a broken run.
"""

from __future__ import annotations

import dataclasses
import re
from typing import Any, Callable

from .spec import Row


@dataclasses.dataclass
class Parsed:
    score: float | None
    ops: float | None = None
    extra: dict[str, float] = dataclasses.field(default_factory=dict)
    error: str = ""


def _num(s: str) -> float | None:
    try:
        return float(s.replace(",", ""))
    except ValueError:
        return None


def parse_larson(out: str, rt: dict[str, Any], row: Row) -> Parsed:
    # "Throughput =  44512345 operations per second, relative time: ..."
    # larson's relative time is 1e9/throughput, not the run time: the ops
    # are throughput x the fixed duration (lib.sh ops_of).
    for line in out.splitlines():
        if "Throughput" in line:
            f = line.split()
            if len(f) >= 3 and (v := _num(f[2])) is not None:
                secs = row.secs or 5.0
                return Parsed(round(v / 1e6, 3), ops=v * secs)
    return Parsed(None, error="no Throughput line")


def parse_xmalloc(out: str, rt: dict[str, Any], row: Row) -> Parsed:
    # "rtime: 5.000, free/sec: 123.456 M" (field 4, M frees/s)
    for line in out.splitlines():
        if "free/sec" in line:
            f = line.split()
            if len(f) >= 4 and (v := _num(f[3])) is not None:
                secs = row.secs or 5.0
                return Parsed(v, ops=v * 1e6 * secs)
    return Parsed(None, error="no free/sec line")


def parse_valkey_write(out: str, rt: dict[str, Any], row: Row) -> Parsed:
    # "ops total=<n> write_mops=<x> delete_mops=<y> total_mops=<z> live=<k> rss_kb=<r> hwm_kb=<h> ..."
    for line in out.splitlines():
        if line.startswith("ops "):
            kv = dict(p.split("=", 1) for p in line.split()[1:] if "=" in p)
            w = _num(kv.get("write_mops", ""))
            if w is None:
                break
            extra = {}
            for k in ("delete_mops", "total_mops", "rss_kb", "hwm_kb", "live"):
                if (v := _num(kv.get(k, ""))) is not None:
                    extra[k] = v
            ops = _num(kv.get("total", ""))
            return Parsed(w, ops=ops, extra=extra)
    return Parsed(None, error="no 'ops ' line")


_CHURN = re.compile(r"^\s*[\d.]+\s+(\S+)\s+curslabs\s+(\d+).*?occ\s+([\d.]+)%")


def parse_churn(out: str, rt: dict[str, Any], row: Row) -> Parsed:
    # "%6.1f %-8s curslabs %6zu curregs %7zu nonfull %6zu occ %5.1f%% ..."; the
    # first "deleted" line is the post-delete state, "end" the final one.
    deleted = end = occ = None
    ops_s = None
    for line in out.splitlines():
        m = _CHURN.match(line)
        if m:
            tag, slabs, o = m.group(1), int(m.group(2)), float(m.group(3))
            if tag == "deleted" and deleted is None:
                deleted = slabs
            elif tag == "end":
                end, occ = slabs, o
        elif line.startswith("ops "):
            m2 = re.search(r"\(([\d.]+)/s\)", line)
            if m2:
                ops_s = float(m2.group(1))
    if occ is None:
        return Parsed(None, error="no 'end' line")
    extra: dict[str, float] = {"end_slabs": float(end or 0)}
    if deleted is not None:
        extra["deleted_slabs"] = float(deleted)
    if ops_s is not None:
        extra["ops_per_s"] = ops_s
    return Parsed(occ, extra=extra)


def parse_wall(out: str, rt: dict[str, Any], row: Row) -> Parsed:
    w = rt.get("wall_s")
    if w is None:
        return Parsed(None, error="no rt record")
    return Parsed(round(float(w), 6), ops=float(row.ops) if row.ops else None)


PARSERS: dict[str, Callable[[str, dict[str, Any], Row], Parsed]] = {
    "larson": parse_larson,
    "xmalloc": parse_xmalloc,
    "valkey_model.write": parse_valkey_write,
    "churn.occ": parse_churn,
    "wall": parse_wall,
}


def parse(row: Row, out: str, rt: dict[str, Any]) -> Parsed:
    p = PARSERS[row.parser](out, rt, row)
    if p.score is not None and p.score <= 0:
        return Parsed(None, error=f"non-positive score {p.score}")
    return p
