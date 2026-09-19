# Python 3.13 benchmark rows, 2026-09-19, aarch64

- commit: 1339eb6
- date: 2026-09-19T04:22:43Z
- host: Neoverse-V2, 16 CPUs visible to the container; GCE c4a-standard-16 (Google Axion, Neoverse V2), zone us-east4-a
- kernel: 7.0.0-1011-gcp, container: Wolfi
- python: Python 3.13.15 (Wolfi python-3.13 3.13.15_git20260918, pymalloc built in); modes: default = PYTHONMALLOC unset (pymalloc for objects under 512 bytes, the allocator sees the rest), malloc = PYTHONMALLOC=malloc (every object through the allocator)
- workloads (bench/probes/python/): json = json_bench.py (json.dumps/loads of 20000 records, 40 rounds); dict_list = dict_list_bench.py (2000 live dicts and lists, 30000 replacements); regex = regex_bench.py (four patterns over 3.5 MB, 6 rounds); object_graph = object_graph_bench.py (150000-node tree, BFS, detach and rebuild, 2 rounds)
- load average at start: 1.45 1.96 3.06; at end: 1.00 1.05 1.79; 863 s total
- method: 5 timed rounds after one discarded warm-up, allocators interleaved per round, one python3 process per row and round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: LD_PRELOAD=/src/target/python/lib/release/libchalloc.so (working tree, HEAD 1339eb6, sha256 c84cb6b229e38f29)
  - jemalloc: LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1)
  - glibc: no LD_PRELOAD (glibc 2.44 malloc)
  - mimalloc: LD_PRELOAD=/src/target/bench-bin/libmimalloc.so (mimalloc v3.5.2, bench/build.sh)
  - tcmalloc: LD_PRELOAD=/usr/lib/libtcmalloc_minimal.so.4 (Wolfi tcmalloc-minimal 2.18.1, gperftools without profiler hooks)

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| json-default | challoc | 3.989 4.001 4.012 4.014 4.053 | 76448 |
| json-default | jemalloc | 4.027 4.031 4.046 4.049 4.061 | 85196 |
| json-default | glibc | 3.901 3.907 3.945 3.965 3.985 | 69764 |
| json-default | mimalloc | 3.958 3.971 3.977 3.988 4.007 | 116428 |
| json-default | tcmalloc | 3.987 3.998 4.015 4.016 4.019 | 92252 |
| json-malloc | challoc | 4.271 4.278 4.284 4.284 4.344 | 77328 |
| json-malloc | jemalloc | 4.219 4.224 4.226 4.231 4.246 | 76928 |
| json-malloc | glibc | 4.245 4.265 4.269 4.273 4.274 | 73056 |
| json-malloc | mimalloc | 3.976 3.979 3.987 4.002 4.004 | 95384 |
| json-malloc | tcmalloc | 4.127 4.146 4.158 4.166 4.178 | 91772 |
| dict_list-default | challoc | 3.300 3.306 3.310 3.337 3.339 | 237132 |
| dict_list-default | jemalloc | 3.318 3.342 3.359 3.359 3.385 | 242032 |
| dict_list-default | glibc | 3.269 3.277 3.290 3.294 3.305 | 222528 |
| dict_list-default | mimalloc | 3.269 3.280 3.311 3.318 3.320 | 262968 |
| dict_list-default | tcmalloc | 3.267 3.287 3.289 3.304 3.304 | 239824 |
| dict_list-malloc | challoc | 4.677 4.685 4.712 4.721 4.722 | 238016 |
| dict_list-malloc | jemalloc | 4.564 4.578 4.581 4.588 4.636 | 249056 |
| dict_list-malloc | glibc | 4.760 4.777 4.780 4.785 4.789 | 282864 |
| dict_list-malloc | mimalloc | 3.657 3.659 3.666 3.713 3.717 | 269264 |
| dict_list-malloc | tcmalloc | 3.984 3.985 4.016 4.024 4.025 | 236880 |
| regex-default | challoc | 3.490 3.492 3.495 3.495 3.500 | 70420 |
| regex-default | jemalloc | 3.479 3.486 3.486 3.493 3.501 | 73624 |
| regex-default | glibc | 3.499 3.499 3.500 3.503 3.506 | 65040 |
| regex-default | mimalloc | 3.493 3.497 3.497 3.500 3.507 | 103784 |
| regex-default | tcmalloc | 3.479 3.481 3.483 3.486 3.528 | 77648 |
| regex-malloc | challoc | 3.557 3.560 3.565 3.568 3.573 | 74348 |
| regex-malloc | jemalloc | 3.534 3.535 3.536 3.542 3.544 | 73488 |
| regex-malloc | glibc | 3.564 3.565 3.566 3.567 3.569 | 72096 |
| regex-malloc | mimalloc | 3.383 3.384 3.384 3.389 3.391 | 91356 |
| regex-malloc | tcmalloc | 3.485 3.487 3.489 3.494 3.502 | 76584 |
| object_graph-default | challoc | 2.803 2.856 2.868 2.940 2.942 | 200176 |
| object_graph-default | jemalloc | 2.801 2.806 2.893 2.941 2.952 | 200148 |
| object_graph-default | glibc | 2.803 2.906 2.928 2.938 2.948 | 197604 |
| object_graph-default | mimalloc | 2.802 2.803 2.851 2.854 2.854 | 227584 |
| object_graph-default | tcmalloc | 2.800 2.806 2.852 2.853 2.946 | 206196 |
| object_graph-malloc | challoc | 2.948 2.994 3.003 3.003 3.092 | 199920 |
| object_graph-malloc | jemalloc | 2.707 2.714 2.818 2.861 2.873 | 200600 |
| object_graph-malloc | glibc | 2.665 2.686 2.713 2.773 2.795 | 208792 |
| object_graph-malloc | mimalloc | 2.399 2.407 2.452 2.474 2.512 | 205940 |
| object_graph-malloc | tcmalloc | 2.856 2.885 2.998 3.013 3.014 | 202356 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | glibc s | glibc RSS MiB | mimalloc s | mimalloc RSS MiB | tcmalloc s | tcmalloc RSS MiB | challoc vs jemalloc time | RSS | challoc vs glibc time | RSS | challoc vs mimalloc time | RSS | challoc vs tcmalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| json-default | 4.012 | 74.7 | 4.046 | 83.2 | 3.945 | 68.1 | 3.977 | 113.7 | 4.015 | 90.1 | -0.8% | -10.3% | +1.7% | +9.6% | +0.9% | -34.3% | -0.1% | -17.1% |
| json-malloc | 4.284 | 75.5 | 4.226 | 75.1 | 4.269 | 71.3 | 3.987 | 93.1 | 4.158 | 89.6 | +1.4% | +0.5% | +0.4% | +5.8% | +7.4% | -18.9% | +3.0% | -15.7% |
| dict_list-default | 3.310 | 231.6 | 3.359 | 236.4 | 3.290 | 217.3 | 3.311 | 256.8 | 3.289 | 234.2 | -1.5% | -2.0% | +0.6% | +6.6% | -0.0% | -9.8% | +0.6% | -1.1% |
| dict_list-malloc | 4.712 | 232.4 | 4.581 | 243.2 | 4.780 | 276.2 | 3.666 | 263.0 | 4.016 | 231.3 | +2.9% | -4.4% | -1.4% | -15.9% | +28.5% | -11.6% | +17.3% | +0.5% |
| regex-default | 3.495 | 68.8 | 3.486 | 71.9 | 3.500 | 63.5 | 3.497 | 101.4 | 3.483 | 75.8 | +0.3% | -4.4% | -0.1% | +8.3% | -0.1% | -32.1% | +0.3% | -9.3% |
| regex-malloc | 3.565 | 72.6 | 3.536 | 71.8 | 3.566 | 70.4 | 3.384 | 89.2 | 3.489 | 74.8 | +0.8% | +1.2% | -0.0% | +3.1% | +5.3% | -18.6% | +2.2% | -2.9% |
| object_graph-default | 2.868 | 195.5 | 2.893 | 195.5 | 2.928 | 193.0 | 2.851 | 222.2 | 2.852 | 201.4 | -0.9% | +0.0% | -2.0% | +1.3% | +0.6% | -12.0% | +0.6% | -2.9% |
| object_graph-malloc | 3.003 | 195.2 | 2.818 | 195.9 | 2.713 | 203.9 | 2.452 | 201.1 | 2.998 | 197.6 | +6.6% | -0.3% | +10.7% | -4.2% | +22.5% | -2.9% | +0.2% | -1.2% |

Positive percentages mean challoc did worse than that allocator (slower, or more RSS).
