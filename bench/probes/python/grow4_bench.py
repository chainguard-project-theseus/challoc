"""grow4_bench.py: four containers grown in turn (the python-grow4 row of bench/rows.toml).

CPython grows a bytearray, a BytesIO and a list by an eighth through
realloc; above the largest slab class (256 KiB) each step is a realloc of a
huge run. One buffer grown alone is the last run and grows in place; four
grown in turn are not, and each one's growth finds another's run behind its
guard unless the allocator left room there when it last moved it. Every
step that cannot grow in place copies the whole buffer. Prints one line of
seconds per case; the row scores the process's wall time and peak RSS.
Deterministic.
Usage: grow4_bench.py [BYTES_MIB] [LIST_M].
"""
import io
import sys
import time

BYTES_MIB = int(sys.argv[1]) if len(sys.argv) > 1 else 64
LIST_M = int(sys.argv[2]) if len(sys.argv) > 2 else 8
MIB = 1 << 20
N = 4


def bytearray4(n):
    bufs = [bytearray() for _ in range(N)]
    piece = b"z" * 1024
    for _ in range(n // 1024):
        for b in bufs:
            b += piece
    return sum(len(b) for b in bufs)


def bytesio4(n):
    fs = [io.BytesIO() for _ in range(N)]
    piece = b"w" * 1024
    for _ in range(n // 1024):
        for f in fs:
            f.write(piece)
    return sum(f.tell() for f in fs)


def list4(n):
    ls = [[] for _ in range(N)]
    for _ in range(n):
        for lst in ls:
            lst.append(None)
    return sum(len(lst) for lst in ls)


CASES = (
    ("bytearray4", bytearray4, BYTES_MIB * MIB),
    ("bytesio4", bytesio4, BYTES_MIB * MIB),
    ("list4", list4, LIST_M * 1_000_000),
)

out = []
total = 0
for name, fn, n in CASES:
    t = time.perf_counter()
    total += fn(n)
    out.append("%s=%.3f" % (name, time.perf_counter() - t))
print(" ".join(out), "checksum=%d" % total)
