"""grow_bench.py: containers grown by appending (the python-grow row of bench/rows.toml).

CPython grows a str (`s += piece`, resized in place when the name holds the
only reference), a list, a bytearray and a BytesIO through realloc: the str
to the exact new size at every step, the others by an eighth. Above the
largest slab class (256 KiB) each of those is a realloc of a huge run. An
allocator that moves the run copies it at every step that crosses a page
(the str: once per 4 KiB appended) or at every eighth (the rest), and holds
the old and the new run at the peak; one that grows the run in place, or
remaps it, copies nothing. Two strs grown in turn cannot both be the last
run, so that case measures the moves that remain. pymalloc serves only
objects up to 512 bytes, so the row is the same under PYTHONMALLOC=malloc.
Prints one line of seconds per case; the row scores the process's wall time
and peak RSS. Deterministic.
Usage: grow_bench.py [STR_MIB] [LIST_M] [BYTES_MIB].
"""
import io
import sys
import time

STR_MIB = int(sys.argv[1]) if len(sys.argv) > 1 else 16
LIST_M = int(sys.argv[2]) if len(sys.argv) > 2 else 20
BYTES_MIB = int(sys.argv[3]) if len(sys.argv) > 3 else 256
MIB = 1 << 20


def str_concat(n):
    s = ""
    piece = "x" * 100
    for _ in range(n // 100):
        s += piece
    return len(s)


def str_pair(n):
    a = ""
    b = ""
    piece = "y" * 100
    for _ in range(n // 200):
        a += piece
        b += piece
    return len(a) + len(b)


def list_append(n):
    lst = []
    append = lst.append
    for _ in range(n):
        append(None)
    return len(lst)


def bytearray_extend(n):
    buf = bytearray()
    piece = b"z" * 1024
    for _ in range(n // 1024):
        buf += piece
    return len(buf)


def bytesio_write(n):
    f = io.BytesIO()
    write = f.write
    piece = b"w" * 1024
    for _ in range(n // 1024):
        write(piece)
    return f.tell()


CASES = (
    ("str_concat", str_concat, STR_MIB * MIB),
    ("str_pair", str_pair, STR_MIB * MIB),
    ("list_append", list_append, LIST_M * 1_000_000),
    ("bytearray_extend", bytearray_extend, BYTES_MIB * MIB),
    ("bytesio_write", bytesio_write, BYTES_MIB * MIB),
)

out = []
total = 0
for name, fn, n in CASES:
    t = time.perf_counter()
    total += fn(n)
    out.append("%s=%.3f" % (name, time.perf_counter() - t))
print(" ".join(out), "checksum=%d" % total)
