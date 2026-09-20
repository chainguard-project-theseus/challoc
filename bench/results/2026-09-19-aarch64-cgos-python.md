# Python 3.13 benchmark rows, 2026-09-19, aarch64

- commit: 4864d4b
- date: 2026-09-19T23:22:01Z
- host: Neoverse-V2 (Axion), 8 CPUs visible to the container; Chainguard OS 6.18.49-r2-gcp-6.18, GCE c4a-standard-8 (Neoverse-V2 (Axion)), zone us-east4-a, 8 vCPU, podman --network=host --privileged
- kernel: 6.18.49-r2-gcp-6.18, container: Wolfi
- python: Python 3.13.15 (Wolfi python-3.13 3.13.15_git20260918, pymalloc built in); modes: default = PYTHONMALLOC unset (pymalloc for objects under 512 bytes, the allocator sees the rest), malloc = PYTHONMALLOC=malloc (every object through the allocator)
- workloads (bench/probes/python/): json = json_bench.py (json.dumps/loads of 20000 records, 40 rounds); dict_list = dict_list_bench.py (2000 live dicts and lists, 30000 replacements); regex = regex_bench.py (four patterns over 3.5 MB, 6 rounds); object_graph = object_graph_bench.py (150000-node tree, BFS, detach and rebuild, 2 rounds)
- load average at start: 1.01 1.54 2.06; at end: 1.00 1.06 1.54; 587 s total
- method: 3 timed rounds after one discarded warm-up, allocators interleaved per round, one python3 process per row and round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). Percentages are challoc against the other allocator: positive means challoc is slower or larger.
- allocators:
  - challoc: LD_PRELOAD=/src/target/python/lib/release/libchalloc.so (working tree, HEAD 4864d4b, sha256 58afede248f5ea5a)
  - jemalloc: LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi jemalloc 5.3.1)
  - glibc: no LD_PRELOAD (glibc 2.44 malloc)
  - mimalloc: LD_PRELOAD=/src/target/bench-bin/libmimalloc.so (mimalloc v3.5.2, bench/build.sh)
  - tcmalloc: LD_PRELOAD=/usr/lib/libtcmalloc_minimal.so.4 (Wolfi tcmalloc-minimal 2.18.1, gperftools without profiler hooks)

## Sorted scores (wall seconds)

| row | allocator | sorted wall s | maxrss KiB |
|---|---|---|---:|
| json-default | challoc | 4.055 4.074 4.082 | 91420 |
| json-default | jemalloc | 3.999 4.026 4.039 | 97436 |
| json-default | glibc | 3.982 4.004 4.036 | 69928 |
| json-default | mimalloc | 4.015 4.019 4.058 | 116360 |
| json-default | tcmalloc | 4.027 4.066 4.067 | 93456 |
| json-malloc | challoc | 4.154 4.188 4.215 | 92064 |
| json-malloc | jemalloc | 4.058 4.081 4.099 | 86892 |
| json-malloc | glibc | 4.386 4.410 4.460 | 72748 |
| json-malloc | mimalloc | 4.021 4.034 4.060 | 91212 |
| json-malloc | tcmalloc | 4.178 4.215 4.242 | 93152 |
| dict_list-default | challoc | 3.329 3.349 3.355 | 247628 |
| dict_list-default | jemalloc | 3.316 3.317 3.374 | 248512 |
| dict_list-default | glibc | 3.305 3.310 3.314 | 222948 |
| dict_list-default | mimalloc | 3.323 3.330 3.341 | 262836 |
| dict_list-default | tcmalloc | 3.278 3.309 3.309 | 241700 |
| dict_list-malloc | challoc | 4.413 4.425 4.451 | 245036 |
| dict_list-malloc | jemalloc | 4.459 4.462 4.485 | 255240 |
| dict_list-malloc | glibc | 4.784 4.790 4.790 | 282028 |
| dict_list-malloc | mimalloc | 3.749 3.750 3.756 | 267208 |
| dict_list-malloc | tcmalloc | 4.039 4.041 4.049 | 237352 |
| regex-default | challoc | 3.521 3.521 3.524 | 85192 |
| regex-default | jemalloc | 3.493 3.493 3.526 | 85044 |
| regex-default | glibc | 3.508 3.513 3.515 | 65212 |
| regex-default | mimalloc | 3.506 3.506 3.512 | 103660 |
| regex-default | tcmalloc | 3.497 3.503 3.720 | 79496 |
| regex-malloc | challoc | 3.504 3.512 3.535 | 89244 |
| regex-malloc | jemalloc | 3.445 3.451 3.454 | 78136 |
| regex-malloc | glibc | 3.565 3.568 3.579 | 71656 |
| regex-malloc | mimalloc | 3.392 3.398 3.399 | 91236 |
| regex-malloc | tcmalloc | 3.482 3.485 3.494 | 77996 |
| object_graph-default | challoc | 3.006 3.191 3.199 | 215224 |
| object_graph-default | jemalloc | 3.076 3.119 3.178 | 208640 |
| object_graph-default | glibc | 3.070 3.103 3.125 | 197624 |
| object_graph-default | mimalloc | 3.041 3.052 3.088 | 227444 |
| object_graph-default | tcmalloc | 3.035 3.135 3.167 | 206864 |
| object_graph-malloc | challoc | 3.240 3.249 3.256 | 214928 |
| object_graph-malloc | jemalloc | 2.683 2.703 2.822 | 206908 |
| object_graph-malloc | glibc | 2.900 2.923 3.076 | 208948 |
| object_graph-malloc | mimalloc | 2.655 2.697 2.728 | 205932 |
| object_graph-malloc | tcmalloc | 3.196 3.217 3.279 | 205360 |

## Medians

| row | challoc s | challoc RSS MiB | jemalloc s | jemalloc RSS MiB | glibc s | glibc RSS MiB | mimalloc s | mimalloc RSS MiB | tcmalloc s | tcmalloc RSS MiB | challoc vs jemalloc time | RSS | challoc vs glibc time | RSS | challoc vs mimalloc time | RSS | challoc vs tcmalloc time | RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| json-default | 4.074 | 89.3 | 4.026 | 95.2 | 4.004 | 68.3 | 4.019 | 113.6 | 4.066 | 91.3 | +1.2% | -6.2% | +1.7% | +30.7% | +1.4% | -21.4% | +0.2% | -2.2% |
| json-malloc | 4.188 | 89.9 | 4.081 | 84.9 | 4.410 | 71.0 | 4.034 | 89.1 | 4.215 | 91.0 | +2.6% | +6.0% | -5.0% | +26.6% | +3.8% | +0.9% | -0.6% | -1.2% |
| dict_list-default | 3.349 | 241.8 | 3.317 | 242.7 | 3.310 | 217.7 | 3.330 | 256.7 | 3.309 | 236.0 | +1.0% | -0.4% | +1.2% | +11.1% | +0.6% | -5.8% | +1.2% | +2.5% |
| dict_list-malloc | 4.425 | 239.3 | 4.462 | 249.3 | 4.790 | 275.4 | 3.750 | 260.9 | 4.041 | 231.8 | -0.8% | -4.0% | -7.6% | -13.1% | +18.0% | -8.3% | +9.5% | +3.2% |
| regex-default | 3.521 | 83.2 | 3.493 | 83.1 | 3.513 | 63.7 | 3.506 | 101.2 | 3.503 | 77.6 | +0.8% | +0.2% | +0.2% | +30.6% | +0.4% | -17.8% | +0.5% | +7.2% |
| regex-malloc | 3.512 | 87.2 | 3.451 | 76.3 | 3.568 | 70.0 | 3.398 | 89.1 | 3.485 | 76.2 | +1.8% | +14.2% | -1.6% | +24.5% | +3.4% | -2.2% | +0.8% | +14.4% |
| object_graph-default | 3.191 | 210.2 | 3.119 | 203.8 | 3.103 | 193.0 | 3.052 | 222.1 | 3.135 | 202.0 | +2.3% | +3.2% | +2.8% | +8.9% | +4.6% | -5.4% | +1.8% | +4.0% |
| object_graph-malloc | 3.249 | 209.9 | 2.703 | 202.1 | 2.923 | 204.1 | 2.697 | 201.1 | 3.217 | 200.5 | +20.2% | +3.9% | +11.2% | +2.9% | +20.5% | +4.4% | +1.0% | +4.7% |

Positive percentages mean challoc did worse than that allocator (slower, or more RSS).
