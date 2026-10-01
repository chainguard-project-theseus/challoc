"""ONNX Runtime CPU inference loop: the large-buffer row (`ort-mlp`).

    python3 mlp.py make OUT.onnx         write the model (bench/build.sh)
    python3 mlp.py run MODEL ITERS [T]   ITERS inferences on T intra-op threads

The model is a three-layer MLP (1024 -> 4096 -> 4096 -> 1024, float32,
96 MiB of weights, seed 1), written with a minimal protobuf encoder so the
image needs only onnxruntime and numpy. The session runs with the CPU
memory arena off (`enable_cpu_mem_arena = False`), so every activation
(batch x 4096 floats: 0.5 to 4 MiB) and every output is a malloc/free of
the allocator under test rather than a slice of ORT's own arena; batch
sizes cycle through a fixed list so the sizes vary run to run the same
way. Fixed work: scored by wall time.
"""

import sys

LAYERS = [(1024, 4096), (4096, 4096), (4096, 1024)]
BATCHES = [32, 64, 128, 256, 48, 200, 96, 160]


def _varint(n: int) -> bytes:
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _key(field: int, wire: int) -> bytes:
    return _varint(field << 3 | wire)


def _int(field: int, v: int) -> bytes:
    return _key(field, 0) + _varint(v)


def _bytes(field: int, b: bytes) -> bytes:
    return _key(field, 2) + _varint(len(b)) + b


def _str(field: int, s: str) -> bytes:
    return _bytes(field, s.encode())


def _value_info(name: str, dims: list) -> bytes:
    shape = b"".join(_bytes(1, _str(2, d) if isinstance(d, str) else _int(1, d)) for d in dims)
    tensor_type = _int(1, 1) + _bytes(2, shape)  # elem_type FLOAT
    return _str(1, name) + _bytes(2, _bytes(1, tensor_type))


def _node(op: str, inputs: list, outputs: list, name: str) -> bytes:
    return b"".join(_str(1, i) for i in inputs) + b"".join(_str(2, o) for o in outputs) + _str(3, name) + _str(4, op)


def make(out: str) -> None:
    import numpy as np

    rng = np.random.default_rng(1)
    nodes, inits = [], []
    x = "x"
    for i, (fin, fout) in enumerate(LAYERS):
        w = (rng.standard_normal((fin, fout), dtype=np.float32) / np.float32(fin**0.5)).astype("<f4")
        inits.append(_int(1, fin) + _int(1, fout) + _int(2, 1) + _str(8, f"w{i}") + _bytes(9, w.tobytes()))
        y = f"h{i}" if i < len(LAYERS) - 1 else "y"
        nodes.append(_node("MatMul", [x, f"w{i}"], [f"m{i}" if y != "y" else "y"], f"matmul{i}"))
        if y != "y":
            nodes.append(_node("Relu", [f"m{i}"], [y], f"relu{i}"))
        x = y
    graph = b"".join(_bytes(1, n) for n in nodes) + _str(2, "mlp") + b"".join(_bytes(5, t) for t in inits)
    graph += _bytes(11, _value_info("x", ["batch", LAYERS[0][0]])) + _bytes(12, _value_info("y", ["batch", LAYERS[-1][1]]))
    model = _int(1, 8) + _str(2, "challoc-bench") + _bytes(7, graph) + _bytes(8, _str(1, "") + _int(2, 17))
    with open(out, "wb") as f:
        f.write(model)


def run(model: str, iters: int, threads: int) -> None:
    import numpy as np
    import onnxruntime as ort

    so = ort.SessionOptions()
    so.enable_cpu_mem_arena = False
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    sess = ort.InferenceSession(model, sess_options=so, providers=["CPUExecutionProvider"])
    rng = np.random.default_rng(2)
    inputs = [rng.standard_normal((b, LAYERS[0][0]), dtype=np.float32) for b in BATCHES]
    total = 0.0
    for i in range(iters):
        (y,) = sess.run(None, {"x": inputs[i % len(inputs)]})
        total += float(y[0, 0])
    print(f"ort iters={iters} threads={threads} checksum={total:.6f} ort={ort.__version__}")


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "make":
        make(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "run":
        run(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]) if len(sys.argv) > 4 else 4)
    else:
        sys.exit(__doc__)
