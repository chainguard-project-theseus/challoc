"""dict_list_bench.py: dict and list churn (bench/python-bench.sh).

Keeps a ring of LIVE containers and, ITERS times, replaces the oldest with
a new dict (1..2000 string keys) or list (1..10000 ints, grown by append,
then sliced and extended), with lookups and deletions on the way. Dict
tables and list buffers beyond a few hundred bytes are allocated through
the C allocator whatever PYTHONMALLOC says; the small containers and their
keys go through pymalloc unless PYTHONMALLOC=malloc. Deterministic.
Usage: dict_list_bench.py [ITERS] [LIVE].
"""
import random
import sys
import time

ITERS = int(sys.argv[1]) if len(sys.argv) > 1 else 30000
LIVE = int(sys.argv[2]) if len(sys.argv) > 2 else 2000

rng = random.Random(4141)
KEYS = [f"key{i:05d}" for i in range(2000)]


def make_dict(n):
    d = {}
    for k in KEYS[:n]:
        d[k] = len(k) * n
    # churn half the keys
    for k in KEYS[: n // 2]:
        del d[k]
    for k in KEYS[: n // 2]:
        d[k + "x"] = n
    return d


def make_list(n):
    lst = []
    for i in range(n):
        lst.append(i * 3)
    lst = lst[n // 4:] + lst[: n // 4]
    lst.extend(range(n // 8))
    return lst


ring = [None] * LIVE
t0 = time.perf_counter()
checksum = 0
for i in range(ITERS):
    slot = i % LIVE
    r = rng.random()
    if r < 0.5:
        n = int(1 + 1999 * r * r * 4)  # skewed to small dicts, up to 2000 keys
        if n > 2000:
            n = 2000
        ring[slot] = make_dict(n)
        checksum += len(ring[slot])
    else:
        n = int(1 + 9999 * (r - 0.5) ** 2 * 4)
        if n > 10000:
            n = 10000
        ring[slot] = make_list(n)
        checksum += ring[slot][-1]
    if i % 97 == 0:
        # a lookup sweep over the live set
        checksum += sum(len(x) for x in ring if x is not None) & 0xFFFF
dt = time.perf_counter() - t0
print(f"dict_list iters={ITERS} live={LIVE} checksum={checksum} secs={dt:.3f}")
