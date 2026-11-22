"""trim_bench.py: buffers cut to their length (the python-trim row of bench/rows.toml).

A reader that does not know how much is coming allocates for the most it
asks for and gives the rest back once it knows: CPython's os.read(fd, n)
and FileIO.read(n) allocate n bytes and resize the result to what the read
returned (_PyBytes_Resize), and a bytearray cut below half its allocation
(`del buf[k:]`) is reallocated to the new length; Ruby's IO#read does the
same at its end (rb_str_resize). Above the largest slab class (256 KiB)
each trim is a realloc of a huge run to fewer pages. An allocator that
moves the run copies everything the buffer keeps at every trim and faults
in the new run's pages, with both runs resident at the peak; one that
shrinks the run in place, or remaps it, copies nothing and gives the tail
back. pymalloc serves only objects up to 512 bytes, so the row is the same
under PYTHONMALLOC=malloc. Prints one line of seconds per case; the row
scores the process's wall time and peak RSS. Deterministic.
Usage: trim_bench.py [FILE_MIB] [READS] [TRIMS].
"""
import os
import sys
import tempfile
import time

FILE_MIB = int(sys.argv[1]) if len(sys.argv) > 1 else 48
READS = int(sys.argv[2]) if len(sys.argv) > 2 else 40
TRIMS = int(sys.argv[3]) if len(sys.argv) > 3 else 40
MIB = 1 << 20


def os_read_trim(path):
    """os.read for more than the file holds: allocated for the request,
    trimmed to the file's length."""
    total = 0
    fd = os.open(path, os.O_RDONLY)
    try:
        for _ in range(READS):
            os.lseek(fd, 0, os.SEEK_SET)
            data = os.read(fd, (FILE_MIB + FILE_MIB // 3) * MIB)
            total += len(data) + data[-1]
            del data
    finally:
        os.close(fd)
    return total


def fileio_read_trim(path):
    """The same through FileIO.read(n), what open(path, 'rb',
    buffering=0).read(n) is."""
    total = 0
    with open(path, "rb", buffering=0) as f:
        for _ in range(READS):
            f.seek(0)
            data = f.read((FILE_MIB + FILE_MIB // 3) * MIB)
            total += len(data) + data[0]
            del data
    return total


def bytearray_cut(_path):
    """A bytearray filled, then cut to a third: below half its
    allocation, so it is reallocated to the new length."""
    total = 0
    piece = b"q" * MIB
    for i in range(TRIMS):
        buf = bytearray(piece) * FILE_MIB
        buf[i] = 1
        del buf[(FILE_MIB // 3) * MIB :]
        total += len(buf) + buf[i]
        del buf
    return total


def main():
    fd, path = tempfile.mkstemp(prefix="trim_bench.")
    try:
        chunk = bytes(range(256)) * (MIB // 256)
        with os.fdopen(fd, "wb") as f:
            for _ in range(FILE_MIB):
                f.write(chunk)
        out = []
        total = 0
        for name, fn in (
            ("os_read_trim", os_read_trim),
            ("fileio_read_trim", fileio_read_trim),
            ("bytearray_cut", bytearray_cut),
        ):
            t = time.perf_counter()
            total += fn(path)
            out.append("%s=%.3f" % (name, time.perf_counter() - t))
        print(" ".join(out), "checksum=%d" % total)
    finally:
        os.unlink(path)


main()
