"""regex_bench.py: regular expressions over generated text (bench/python-bench.sh).

Generates a text of WORDS pseudo-words, numbers, dates and e-mail-like
tokens (about 3 MB), then ROUNDS times runs findall, finditer with group
extraction, sub and split with a fixed set of patterns and builds result
lists. Match objects, the group strings and the result lists are small
(pymalloc unless PYTHONMALLOC=malloc); the substituted copies of the text
are multi-megabyte strings that always come from malloc. Deterministic.
Usage: regex_bench.py [ROUNDS] [WORDS].
"""
import random
import re
import sys
import time

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 6
WORDS = int(sys.argv[2]) if len(sys.argv) > 2 else 400000

rng = random.Random(4141)
ALPHA = "abcdefghijklmnopqrstuvwxyz"


def token(i):
    r = rng.random()
    if r < 0.70:
        return "".join(rng.choice(ALPHA) for _ in range(rng.randint(2, 10)))
    if r < 0.85:
        return str(rng.randint(0, 10 ** 6))
    if r < 0.93:
        return f"{rng.randint(1990, 2030)}-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}"
    return f"{''.join(rng.choice(ALPHA) for _ in range(5))}{i}@{rng.choice(ALPHA)}mail.example.org"


text = " ".join(token(i) for i in range(WORDS))
PATTERNS = [
    re.compile(r"\b\d{4}-\d{2}-\d{2}\b"),
    re.compile(r"\b[a-z0-9]+@[a-z]+\.example\.org\b"),
    re.compile(r"\b(\w+) (\d+)\b"),
    re.compile(r"\b[aeiou]\w{3,}\b"),
]
t0 = time.perf_counter()
count = 0
for _ in range(ROUNDS):
    dates = PATTERNS[0].findall(text)
    mails = PATTERNS[1].findall(text)
    pairs = [(m.group(1), int(m.group(2))) for m in PATTERNS[2].finditer(text)]
    vowels = PATTERNS[3].findall(text)
    replaced = PATTERNS[1].sub("<mail>", text)
    replaced = PATTERNS[0].sub(lambda m: m.group(0).replace("-", "/"), replaced)
    parts = re.split(r"\s+", replaced)
    count += len(dates) + len(mails) + len(pairs) + len(vowels) + len(parts)
    del dates, mails, pairs, vowels, replaced, parts
dt = time.perf_counter() - t0
print(f"regex rounds={ROUNDS} words={WORDS} text_bytes={len(text)} matches={count} secs={dt:.3f}")
