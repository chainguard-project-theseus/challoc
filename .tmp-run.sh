RUNS=1 ALLOCS="challoc jemalloc" BENCHES="larson xmalloc-test redis mstress" PROFILE_SECS=3 RESULT=target/bench-out-smoke.md OUT=target/bench-out-smoke sh bench/run.sh 2>&1 | tail -12
sed -n '/^### larson \/ challoc/,/^### mstress \/ challoc/p' target/bench-out-smoke.md
grep -A4 'top 10 challoc' target/bench-out-smoke.md
