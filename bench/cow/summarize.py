#!/usr/bin/env python3
"""Medians and ranges per (label, vsize, writes) over the stage files."""
import json, sys, statistics
from collections import defaultdict

rows = []
for f in sys.argv[1:]:
    for l in open(f):
        rows.append(json.loads(l))

def cls(r):
    """class-region (slab) vs huge-region COW bytes, by span size."""
    slab = huge = other = 0
    for k, v in r["cow_by_vma"].items():
        if k.startswith("span5734"):
            slab += v
        elif k.startswith("span10") or k.startswith("span65"):
            huge += v
        else:
            other += v
    return slab, huge, other

g = defaultdict(list)
for r in rows:
    if r["label"].endswith("-tput") or r["label"].endswith("-rec"):
        continue
    g[(r["vsize"], r["writes"], r["label"])].append(r)

M = 1e6
print("COW (session measurements, child Private_Dirty peak = rdb_last_cow_size), MB: median [min-max], n")
print("%-5s %-8s %-9s %-22s %-22s %-10s %-10s %-8s" % ("vsize", "writes", "build", "cow", "peak parent+child", "slab", "huge", "rss+"))
for key in sorted(g):
    rs = g[key]
    cow = [r["rdb_last_cow_size"] / M for r in rs]
    peak = [r["peak_parent_rss_plus_child_dirty"] / M for r in rs]
    sl = [cls(r)[0] / M for r in rs]
    hu = [cls(r)[1] / M for r in rs]
    grow = [(r["rss_after_save"] - r["rss_before"]) / M for r in rs]
    f = lambda xs: "%6.0f [%4.0f-%4.0f]" % (statistics.median(xs), min(xs), max(xs))
    print("%-5d %-8d %-9s %-22s %-22s %-10.0f %-10.0f %-8.0f n=%d" % (key[0], key[1], key[2], f(cow), f(peak),
          statistics.median(sl), statistics.median(hu), statistics.median(grow), len(rs)))

print()
print("SET throughput (valkey-benchmark -c 50 -P 16, ops/s): outside the save / inside the save")
t = defaultdict(list)
for r in rows:
    if r["label"].endswith("-tput"):
        t[(r["label"], r["vsize"])].append(r["set_ops_s"])
for key in sorted(t):
    o = [x["outside"] for x in t[key]]
    i = [x["inside"] for x in t[key]]
    print("%-10s %5d outside %s inside %s" % (key[0], key[1], sorted(o), sorted(i)))

print()
print("Recovery (activedefrag yes after the save), MB")
for r in rows:
    if r["label"].endswith("-rec"):
        s = r["recover"]
        print("%-9s %5d writes=%d rss_before=%.0f rss_after_save=%.0f rss_end(120s)=%.0f used_memory_end=%.0f cow=%.0f" % (
            r["label"], r["vsize"], r["writes"], r["rss_before"] / M, r["rss_after_save"] / M, s[-1][1] / M, s[-1][2] / M,
            r["rdb_last_cow_size"] / M))
