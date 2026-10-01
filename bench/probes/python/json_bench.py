"""json_bench.py: JSON encode/decode of a large structure (the python rows of bench/rows.toml).

Builds a list of RECORDS dicts (ints, floats, short and medium strings, a
tag list and a nested metrics dict, about 6 MB as JSON), then ROUNDS times
json.dumps it, json.loads the text back and checks the record count. The
serialiser's output buffer and the multi-megabyte text go through malloc
under every PYTHONMALLOC setting; the decoded objects (dicts, lists, most
strings) are under pymalloc's 512-byte threshold and reach the allocator
only with PYTHONMALLOC=malloc. Deterministic (seeded). Usage:
json_bench.py [ROUNDS] [RECORDS].
"""
import json
import random
import sys
import time

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 40
RECORDS = int(sys.argv[2]) if len(sys.argv) > 2 else 20000

rng = random.Random(4141)
WORDS = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
         "hotel", "india", "juliet", "kilo", "lima", "mike", "november"]


def record(i):
    return {
        "id": i,
        "name": "-".join(rng.choice(WORDS) for _ in range(3)),
        "email": f"user{i}@{rng.choice(WORDS)}.example.com",
        "score": rng.random() * 1000.0,
        "active": bool(i & 1),
        "tags": [rng.choice(WORDS) for _ in range(rng.randint(1, 6))],
        "metrics": {w: rng.randint(0, 1 << 20) for w in rng.sample(WORDS, 5)},
        "note": " ".join(rng.choice(WORDS) for _ in range(rng.randint(4, 24))),
    }


data = [record(i) for i in range(RECORDS)]
t0 = time.perf_counter()
total = 0
for _ in range(ROUNDS):
    text = json.dumps(data)
    back = json.loads(text)
    if len(back) != RECORDS:
        sys.exit("json round trip lost records")
    total += len(text)
    del text, back
dt = time.perf_counter() - t0
print(f"json rounds={ROUNDS} records={RECORDS} bytes_per_round={total // ROUNDS} secs={dt:.3f}")
