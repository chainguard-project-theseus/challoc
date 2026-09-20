#!/bin/sh
# percpu-campaign.sh: the acceptance rows of plan Phase 2.5 task 8 (per-CPU
# caches through rseq mm_cid), run as a campaign on the VM host (lane D,
# measurement only). The same suites run under the default and under
# `percpu_arena:percpu`, so the two halves compare row by row:
#
#   test         make test (release build, unit and C tests, the 512 MiB
#                cgroup group, the churn and lazy containers)
#   ab           make ab, the Phase 0 set plus larson64 (64 threads on 16
#                CPUs) and the Valkey model rows, challoc against jemalloc,
#                ROUNDS timed rounds in one run; in MODE=percpu the run has
#                three variants, default=tree, percpu=tree:percpu_arena:percpu
#                and jemalloc, so default and percpu are the same-run pair
#                (bench/README.md, "Protocol tier"); mstress is the x86_64
#                RSS row the task re-measures
#   calloc       bench/probes/calloc-zero-campaign.sh (check_zero under the
#                api:: load), CALLOC_ROUNDS runs of stress 8 50 10 per
#                configuration, bounded by CALLOC_MAX_SECS, plus 10 standalone
#                stress 16 100 20; MODE=percpu adds the percpu configuration
#   cgroup       make test-cgroup-churn, test-cgroup-lazy, test-cgroup-oscillate
#                (OSC_SECS) under the mode
#   valkey-suite make valkey STAGE=memefficiency (the defrag tests; VALKEY_FULL=1
#                adds STAGE=full)
#   valkey-soak  make valkey-soak (the Phase 1 protocol, SOAK_DURATION s per
#                allocator) into bench/results/<stem>-soak/ (the soak names
#                its files itself); MODE=baseline stores jemalloc's soak as the cached
#                baseline (SAVE_BASELINE=1) so MODE=percpu can run challoc alone
#                against it (BASELINES=cached) when the cache exists
#   ruby-bench   make ruby-bench ALLOCS='challoc jemalloc', ROUNDS rounds
#   ruby-test    make ruby STAGE=test (Ruby's test-all; slow)
#   ruby-soak    make ruby-soak, 16 workers, RUBY_SOAK_DURATION s
#   ruby-idle    make ruby-soak with THREADS=16 IDLE_THREADS (default 48, so
#                64 threads on a 16-vCPU VM) for 300 s: "RSS bounded by cores,
#                not threads"
#
# The percpu half puts challoc under the key through CHALLOC_CONF (read by
# challoc after MALLOC_CONF and by nothing else; the Makefile forwards it into
# every container), except in `ab`, where the variant's own MALLOC_CONF does
# it so the default variant of the same run stays default. Steps that need a
# build (Valkey, Ruby) expect `make valkey STAGE=build`, `make ruby STAGE=build`
# and `make ruby STAGE=build ALLOC=glibc` to have run; `make bench-build` for
# the ab rows.
#
# Output: bench/results/<DATE>-<arch>-<LANE>-percpu-<MODE>.md, one section per
# step with the lines that carry the verdict and the path of the step's own
# file (<stem>-ab.md, -ruby.md, -ruby-soak.md, -ruby-idle.md,
# -valkey-soak.md and CSVs); raw logs under target/<LANE>/percpu-<MODE>/.
#
#   MODE=baseline|percpu   STEPS="test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle"
#   DATE (UTC today)  LANE=lane-d  ROUNDS=5  SOAK_DURATION=1200  RUBY_SOAK_DURATION=600
#   IDLE_THREADS=48  VALKEY_FULL=0  CALLOC_ROUNDS=100  CALLOC_MAX_SECS=300  OSC_SECS=300
#   COMMIT (git)  HOST_CPU (the Makefile's)
#   CGROUP_PEAK=1  passed to the soak steps (their in-container memory.peak resets per
#                  phase); every step's line also carries the largest memory.peak of the
#                  step's container cgroups, read from the host (plan Phase 2.5 task 17)
set -u
cd "$(dirname "$0")/.."
ROOT=$(pwd)
MODE=${MODE:-baseline}
case $MODE in baseline|percpu) ;; *) echo "MODE must be baseline or percpu" >&2; exit 2;; esac
STEPS=${STEPS:-"test ab calloc cgroup valkey-suite valkey-soak ruby-bench ruby-test ruby-soak ruby-idle"}
DATE=${DATE:-$(date -u +%F)}
LANE=${LANE:-lane-d}
ROUNDS=${ROUNDS:-5}
SOAK_DURATION=${SOAK_DURATION:-1200}
RUBY_SOAK_DURATION=${RUBY_SOAK_DURATION:-600}
IDLE_THREADS=${IDLE_THREADS:-48}
VALKEY_FULL=${VALKEY_FULL:-0}
CALLOC_ROUNDS=${CALLOC_ROUNDS:-100}
CALLOC_MAX_SECS=${CALLOC_MAX_SECS:-300}
OSC_SECS=${OSC_SECS:-300}
ARCH=$(uname -m)
COMMIT=${COMMIT:-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)}
KEY=percpu_arena:percpu
CONF=; [ "$MODE" = percpu ] && CONF=$KEY
# STEM is the host-side path of the results files, RSTEM the same path as
# the container sees it (the tree is mounted at /src and make's cwd is /src).
STEM=$ROOT/bench/results/$DATE-$ARCH-$LANE-percpu-$MODE
RSTEM=bench/results/$DATE-$ARCH-$LANE-percpu-$MODE
SUMMARY=$STEM.md
# Raw logs: not under target/ (on a VM that is a Docker volume the host
# user cannot write) but a git-ignored .tmp-* directory of the tree, which
# `challoc-bench sync` neither ships nor deletes.
OUT=${OUT:-$ROOT/.tmp-percpu-$MODE}
mkdir -p "$OUT" "$ROOT/bench/results"
T0=$(date +%s)
NPROC=$(nproc)

# The cgroup peak per row (plan Phase 2.5 task 17): every step's containers
# are fresh cgroups (`docker run --rm`), so their memory.peak is the row's
# peak by construction and needs no reset; this runs on the VM host, where
# the containers' cgroups are /sys/fs/cgroup/system.slice/docker-<id>.scope
# (the systemd driver) or /sys/fs/cgroup/docker/<id> (cgroupfs), and
# memory.peak is world-readable. peak_poll samples them every 2 s while a
# step runs; the step's line gets the largest per-container peak and the
# container count. The soak steps also get CGROUP_PEAK=1, so their own
# per-phase resets happen inside the container (bench/valkey-soak.sh,
# bench/ruby-soak.sh).
CGROUP_PEAK=${CGROUP_PEAK:-1}
peak_poll() { # peak_poll FILE: "path bytes" lines until FILE.stop exists
  : > "$1"
  while [ ! -e "$1.stop" ]; do
    for f in /sys/fs/cgroup/system.slice/docker-*.scope/memory.peak /sys/fs/cgroup/docker/*/memory.peak; do
      [ -r "$f" ] && echo "$f $(cat "$f" 2>/dev/null)" >> "$1"
    done
    sleep 2
  done
}
peak_summary() { # peak_summary FILE: "max X MiB over N containers" or a reason
  [ -s "$1" ] || { echo "no container cgroup seen (no /sys/fs/cgroup/{system.slice/docker-*.scope,docker/*} readable on this host, or the step ran under 2 s)"; return; }
  awk '{ if ($2+0 > m[$1]) m[$1] = $2+0 } END { for (k in m) { n++; if (m[k] > max) max = m[k] } printf "max %.1f MiB over %d container cgroup(s)\n", max/1048576, n }' "$1"
}
# One make invocation with the mode's CHALLOC_CONF (when $1 is "conf"),
# logged to $OUT/<name>.log; prints the rc, the wall time and the cgroup
# peak of the step's containers.
run_make() { # run_make NAME conf|plain make-args...
  name=$1; how=$2; shift 2
  s=$(date +%s)
  rm -f "$OUT/$name.peak.stop"; peak_poll "$OUT/$name.peak" & pp=$!
  if [ "$how" = conf ] && [ -n "$CONF" ]; then
    make "$@" CHALLOC_CONF="$CONF" > "$OUT/$name.log" 2>&1; rc=$?
  else
    make "$@" > "$OUT/$name.log" 2>&1; rc=$?
  fi
  touch "$OUT/$name.peak.stop"; wait $pp 2>/dev/null
  e=$(date +%s)
  echo "-- $name: make $* ${CONF:+(CHALLOC_CONF=$CONF)}: rc=$rc in $((e - s)) s; cgroup memory.peak $(peak_summary "$OUT/$name.peak")" | tee -a "$SUMMARY"
  return $rc
}
say() { echo "$@" | tee -a "$SUMMARY"; }
lines() { # lines FILE PATTERN [MAX]: matching lines, verbatim, as a code block
  n=${3:-40}
  { echo; echo '```'; grep -h -E "$2" "$1" 2>/dev/null | head -n "$n"; echo '```'; echo; } >> "$SUMMARY"
}
has_step() { case " $STEPS " in *" $1 "*) return 0;; *) return 1;; esac; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo -; }

# ---- header -------------------------------------------------------------------------
make run CMD='cargo build --release >/dev/null 2>&1; sh scripts/ctest-cc.sh tests/c/features.c -o target/lane-features && LD_PRELOAD=/src/target/release/libchalloc.so target/lane-features' > "$OUT/features.log" 2>&1
FEATURES=$(grep -h '^features:' "$OUT/features.log" | tail -1)
modeline=
[ -n "$CONF" ] && modeline=", challoc under CHALLOC_CONF=$CONF in every step but ab (there the variant percpu=tree:$KEY carries the key and default=tree is the same run default)"
{
  echo "# Per-CPU campaign (plan Phase 2.5 task 8 acceptance rows), $DATE, $ARCH, mode \`$MODE\`"
  echo
  echo "- commit: $COMMIT"
  echo "- date: $(date -u +%FT%TZ)"
  echo "- host: ${HOST_CPU:-unknown CPU}, $NPROC CPUs; kernel $(uname -r); docker $(docker --version 2>/dev/null | sed 's/,.*//')"
  echo "- $FEATURES"
  echo "- mode: $MODE$modeline"
  [ "$MODE" = baseline ] && echo "- baseline half: the default configuration alone (the key is accepted and ignored before task 8 lands; nothing here runs under it). The percpu half is the same script with MODE=percpu after the merge."
  echo "- steps: $STEPS; ROUNDS=$ROUNDS SOAK_DURATION=$SOAK_DURATION RUBY_SOAK_DURATION=$RUBY_SOAK_DURATION IDLE_THREADS=$IDLE_THREADS CALLOC_ROUNDS=$CALLOC_ROUNDS CALLOC_MAX_SECS=$CALLOC_MAX_SECS OSC_SECS=$OSC_SECS VALKEY_FULL=$VALKEY_FULL"
  echo "- driver: bench/percpu-campaign.sh (\`make percpu-campaign MODE=$MODE\`), raw logs $(basename "$OUT")/ on the host that ran it"
  echo "- load at start: $(loadavg)"
  echo
} > "$SUMMARY"

# ---- test ------------------------------------------------------------------------------
if has_step test; then
  say "## test: \`make test\`${CONF:+ under CHALLOC_CONF=$CONF}"
  run_make test conf test
  lines "$OUT/test.log" '^test result:|^OUTCOME |^failures:|panicked at|^challoc: ' 40
fi

# ---- ab --------------------------------------------------------------------------------
if has_step ab; then
  say "## ab: the Phase 0 set, larson64 and the Valkey model rows, challoc against jemalloc ($ROUNDS rounds, one run)"
  if [ "$MODE" = percpu ]; then VARIANTS="default=tree percpu=tree:$KEY jemalloc"; else VARIANTS="default=tree jemalloc"; fi
  BENCHES="larson1 larson10 larson16 larson64 xmalloc alloc-test1 alloc-testN cfrac espresso mstress glibc-simple redis valkey1 valkey1-free"
  run_make ab plain ab VARIANTS="$VARIANTS" BENCHES="$BENCHES" ROUNDS="$ROUNDS" RESULT="$RSTEM-ab.md" TITLE="per-CPU campaign ($MODE)" HOST_KIND="GCE $(uname -m) lane VM"
  say "- variants: $VARIANTS; rows: $BENCHES; table: \`$(basename "$STEM")-ab.md\` (the Medians section below)"
  { echo; awk '/^## Medians/,0' "$STEM-ab.md" 2>/dev/null | head -40; echo; } >> "$SUMMARY"
fi

# ---- calloc ----------------------------------------------------------------------------
if has_step calloc; then
  say "## calloc: the calloc-zero campaign (\`check_zero:true\` under the api:: load; $CALLOC_ROUNDS x stress 8 50 10 per configuration, at most $CALLOC_MAX_SECS s each, then 10 x stress 16 100 20 standalone)"
  if [ "$MODE" = percpu ]; then CONFIGS="percpu:check_zero:true,$KEY default:check_zero:true"; else CONFIGS="default:check_zero:true"; fi
  # make run forwards no environment: the knobs ride inside CMD. The
  # standalone runs take the mode's key through CHALLOC_CONF (run_make conf).
  run_make calloc conf run CMD="ROUNDS=$CALLOC_ROUNDS MAX_SECS=$CALLOC_MAX_SECS STANDALONE=10 SKIP_SUITE=1 CONFIGS=\"$CONFIGS\" sh bench/probes/calloc-zero-campaign.sh"
  lines "$OUT/calloc.log" '^== |^RESULT |^!! |^suite:|^probe:' 40
fi

# ---- cgroup ------------------------------------------------------------------------------
if has_step cgroup; then
  say "## cgroup: the container tests${CONF:+ under CHALLOC_CONF=$CONF} (churn at 512 and 256 MiB, lazy at 512 MiB, oscillation $OSC_SECS s at 512 MiB)"
  run_make cgroup-churn conf test-cgroup-churn
  run_make cgroup-lazy conf test-cgroup-lazy
  run_make cgroup-oscillate conf test-cgroup-oscillate OSC_SECS="$OSC_SECS"
  cat "$OUT/cgroup-churn.log" "$OUT/cgroup-lazy.log" "$OUT/cgroup-oscillate.log" > "$OUT/cgroup.log" 2>/dev/null
  lines "$OUT/cgroup.log" '^OUTCOME |^RESULT |^FAIL |oom_kill|^challoc: stats.cgroup' 40
fi

# ---- valkey-suite ------------------------------------------------------------------------
if has_step valkey-suite; then
  full=; [ "$VALKEY_FULL" = 1 ] && full=" and STAGE=full"
  say "## valkey-suite: \`make valkey STAGE=memefficiency\`$full${CONF:+ under CHALLOC_CONF=$CONF}"
  run_make valkey-memefficiency conf valkey STAGE=memefficiency
  lines "$OUT/valkey-memefficiency.log" '^\[ok\]|^\[err\]|^\[exception\]|All tests passed|tests? failed|^!!|malloc=|Execution time' 60
  if [ "$VALKEY_FULL" = 1 ]; then
    run_make valkey-full conf valkey STAGE=full
    lines "$OUT/valkey-full.log" '^\[err\]|^\[exception\]|All tests passed|tests? failed|^!!|units? \(|Execution time' 40
  fi
fi

# ---- valkey-soak -------------------------------------------------------------------------
if has_step valkey-soak; then
  RD=$RSTEM-soak
  # The cached jemalloc soak lives in the target volume: ask a container.
  cached=no
  make run CMD="test -f target/baselines/valkey-soak-jemalloc-$ARCH.csv" > /dev/null 2>&1 && cached=yes
  if [ "$MODE" = percpu ] && [ "$cached" = yes ]; then
    say "## valkey-soak: \`make valkey-soak\` challoc under CHALLOC_CONF=$CONF for $SOAK_DURATION s against the cached jemalloc soak of the baseline half (BASELINES=cached)"
    run_make valkey-soak conf valkey-soak ALLOCS=challoc BASELINES=cached DURATION="$SOAK_DURATION" RESULT_DIR="$RD" COMMIT="$COMMIT" CGROUP_PEAK="$CGROUP_PEAK"
  else
    under=; [ -n "$CONF" ] && under=", challoc under CHALLOC_CONF=$CONF"
    say "## valkey-soak: \`make valkey-soak\` (challoc and bundled jemalloc, $SOAK_DURATION s of load each, the Phase 1 protocol$under; the jemalloc soak saved as the cached baseline)"
    run_make valkey-soak conf valkey-soak DURATION="$SOAK_DURATION" RESULT_DIR="$RD" SAVE_BASELINE=1 COMMIT="$COMMIT" CGROUP_PEAK="$CGROUP_PEAK"
  fi
  md=$(ls -t "$ROOT/$RD"/*-valkey-soak.md 2>/dev/null | head -1)
  if [ -n "$md" ]; then
    say "- summary: \`$(basename "$RD")/$(basename "$md")\` (CSVs beside it)"
    { echo; awk '/^## /{n++} n>=1 && n<=2' "$md" | head -60; echo; } >> "$SUMMARY"
  else
    say "- no soak summary produced (see $(basename "$OUT")/valkey-soak.log)"
    lines "$OUT/valkey-soak.log" 'FAIL|error|Error|No such' 10
  fi
fi

# ---- ruby-bench ----------------------------------------------------------------------------
if has_step ruby-bench; then
  say "## ruby-bench: the Ruby rows, challoc against jemalloc ($ROUNDS rounds)${CONF:+, challoc under CHALLOC_CONF=$CONF}"
  run_make ruby-bench conf ruby-bench ALLOCS="challoc jemalloc" ROUNDS="$ROUNDS" RESULT="$RSTEM-ruby.md" HOST_KIND="GCE $(uname -m) lane VM"
  say "- table: \`$(basename "$STEM")-ruby.md\`"
  { echo; awk '/^## /{n++} n==1' "$STEM-ruby.md" 2>/dev/null | head -30; echo; } >> "$SUMMARY"
fi

# ---- ruby-test -----------------------------------------------------------------------------
if has_step ruby-test; then
  say "## ruby-test: \`make ruby STAGE=test\` (Ruby's test-all on the --with-jemalloc build)${CONF:+ under CHALLOC_CONF=$CONF}"
  run_make ruby-test conf ruby STAGE=test
  lines "$OUT/ruby-test.log" 'tests, .* assertions|^[0-9]+\) (Failure|Error)|challoc:|Aborted|^== ' 40
fi

# ---- ruby-soak -----------------------------------------------------------------------------
if has_step ruby-soak; then
  say "## ruby-soak: \`make ruby-soak\` 16 workers, $RUBY_SOAK_DURATION s, challoc against jemalloc${CONF:+, challoc under CHALLOC_CONF=$CONF}"
  run_make ruby-soak conf ruby-soak ALLOCS="challoc jemalloc" DURATION="$RUBY_SOAK_DURATION" RESULT="$RSTEM-ruby-soak.md" COMMIT="$COMMIT" CGROUP_PEAK="$CGROUP_PEAK"
  arenas=$(grep -h -m1 -E '^Arenas:' "$STEM-ruby-soak-challoc-mstats.txt" 2>/dev/null || echo -)
  say "- table: \`$(basename "$STEM")-ruby-soak.md\`; arenas from the report: $arenas"
  { echo; awk '/^## RSS/,/^Positive/' "$STEM-ruby-soak.md" 2>/dev/null; echo; } >> "$SUMMARY"
fi

# ---- ruby-idle -----------------------------------------------------------------------------
if has_step ruby-idle; then
  say "## ruby-idle: \`make ruby-soak\` 16 workers + $IDLE_THREADS mostly idle threads on $NPROC CPUs, 300 s${CONF:+, challoc under CHALLOC_CONF=$CONF} (RSS bounded by cores, not threads)"
  run_make ruby-idle conf ruby-soak ALLOCS="challoc jemalloc" THREADS=16 IDLE_THREADS="$IDLE_THREADS" DURATION=300 COMPACT_AT=150 RESULT="$RSTEM-ruby-idle.md" COMMIT="$COMMIT" CGROUP_PEAK="$CGROUP_PEAK"
  arenas=$(grep -h -m1 -E '^Arenas:' "$STEM-ruby-idle-challoc-mstats.txt" 2>/dev/null || echo -)
  # ruby-soak.sh prints each run's summary line (tail -1 of the .out) to
  # its stdout, which run_make logged; the .out files live in the volume.
  idle=$(grep -h 'idle_threads=' "$OUT/ruby-idle.log" 2>/dev/null | sed 's/.*\(threads=[^ ]* idle_threads=[^ ]* idle_bursts=[^ ]* nproc=[^ ]*\).*/\1/' | sort -u | tr '\n' ';')
  say "- table: \`$(basename "$STEM")-ruby-idle.md\`; arenas from the report: $arenas; summary lines: $idle"
  { echo; awk '/^## RSS/,/^Positive/' "$STEM-ruby-idle.md" 2>/dev/null; echo; } >> "$SUMMARY"
fi

say "## done: $(( $(date +%s) - T0 )) s, load at end $(loadavg)"
