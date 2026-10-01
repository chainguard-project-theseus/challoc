#!/bin/sh
# bench/drivers/fleetbench.sh: a Fleetbench binary (bench/Dockerfile.cb's
# fleetbench stage, built against the system malloc) under the allocator,
# Google Benchmark's JSON output, scored by the geometric mean of the
# selected benchmarks' real time per iteration.
#
#   PROGRAM    proto_benchmark | empirical_driver
#   FILTER     --benchmark_filter (default: all of the program's defaults)
#   MIN_TIME   --benchmark_min_time per benchmark (default 0.5s)
#
# Metrics: geomean_ns (the score, lower is better), benchmarks (how many
# ran), maxrss_mib.
. "$(dirname "$0")/cb.sh"
PROGRAM=${PROGRAM:?PROGRAM=proto_benchmark|empirical_driver}
FILTER=${FILTER:-}
MIN_TIME=${MIN_TIME:-0.5s}
B=$CB_BIN/fleetbench/$PROGRAM
[ -x "$B" ] || die "no $B (the bench image's fleetbench stage)"
timed run "$B" ${FILTER:+--benchmark_filter=$FILTER} --benchmark_min_time="$MIN_TIME" \
  --benchmark_format=json --benchmark_out="$CB_OUT/bench.json" --benchmark_out_format=json
out=$(python3 - "$CB_OUT/bench.json" <<'EOF'
import json, math, sys
d = json.load(open(sys.argv[1]))
scale = {"ns": 1.0, "us": 1e3, "ms": 1e6, "s": 1e9}
ts = [b["real_time"] * scale[b.get("time_unit", "ns")] for b in d.get("benchmarks", [])
      if b.get("run_type", "iteration") == "iteration" and b.get("real_time", 0) > 0 and not b.get("error_occurred")]
if not ts:
    sys.exit("no benchmark results")
print(f"{math.exp(sum(math.log(t) for t in ts) / len(ts)):.3f} {len(ts)}")
EOF
) || die "no results in $CB_OUT/bench.json"
metric geomean_ns "${out% *}"
metric benchmarks "${out#* }"
metric maxrss_mib "$(mib "$MAXRSS_KIB")"
emit
