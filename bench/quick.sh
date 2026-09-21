#!/bin/sh
# Fast tier of the benchmark harness (bench/README.md): a go/no-go signal for
# one implementation-loop iteration in about two minutes on a reference VM.
# Run inside the dev container: `make quick [OLD=rev]` or, on a VM,
# `bench/gce/challoc-bench quick arm64 [OLD=rev]`.
#
# Builds the working tree into target/quick/new and OLD (default HEAD, so an
# uncommitted change is measured against its own base) from `git archive`
# into target/quick/old, checks that both compiled and that the two .so files
# differ exactly when the sources do, then runs ONE round of
#
#   larson1  larson 5 8 1000 5000 100 4141 1     M ops/s, higher is better
#   larson10 larson 5 8 1000 5000 100 4141 10    M ops/s
#   xmalloc  xmalloc-test -w 10 -t 5 -s 64       M frees/s, higher is better
#   alloc-test1                                  seconds, lower is better
#   valkey1  bench/probes/valkey_model.c, 850k keys, 2.5 s write + 2.5 s delete,
#            frees through sdallocx           write-phase M ops/s, higher is better
#   churn    bench/probes/churn.c CLASS CHURN_SECS s    slabs deleted -> end
#
# under old, new and jemalloc, and prints one table. Exit status 1 when new
# is more than QUICK_TOLERANCE percent worse than old on any row or the churn
# probe under new does not compact (end occupancy of the class below
# CHURN_MIN_OCC percent; Phase 1 acceptance 3 (c)'s "469 -> under 230 slabs" is
# about 90% with 64 KiB slabs, and occupancy does not move with slab size).
#
# One round is a signal, not a measurement: a pass here means "worth the
# protocol tier" (bench/ab.sh), a fail means "abandon or rework". Numbers
# from this script never go into bench/results/.
#
#   OLD=HEAD                 rev of the base library (any git rev; "tree" = working tree)
#   NEW=tree                 rev of the candidate (default: the working tree)
#   NEW_CONF= OLD_CONF=      MALLOC_CONF for the new / old library's rows (e.g.
#                            NEW_CONF=percpu_arena:percpu judges an opt-in mode
#                            against the base's default; both default empty)
#   QUICK_TOLERANCE=5        percent new may trail old before the exit status says no
#   QUICK_CONFIRM_ROUNDS=2   extra rounds of old and new for a row that failed the gate once
#   QUICK_BENCHES="larson1 larson10 xmalloc alloc-test1 valkey1"
#   VALKEY_MODEL_SECS=5      seconds of the Valkey model row (half write, half delete)
#   CHURN_SECS=8 CHURN_CLASS=1024 CHURN_MIN_OCC=90
#   BASELINES=cached         take the jemalloc column from target/baselines/<arch>-<hash>.md
#                            (bench/baseline.sh) and run only QUICK_SANITY (larson1) under
#                            jemalloc; that run must land within BASELINE_TOLERANCE (3) percent
#                            of the cached median or the cache is stale (exit 2)
#   PERF=1                   after the table, one run of PERF_BENCHES (larson1 valkey1) under
#                            old, new and jemalloc with `perf stat` and a per-operation table
#                            (cycles, instructions, L1D and dTLB misses per op). Needs perf in
#                            the container; the VMs have it on the host only, so there use
#                            `bench/gce/challoc-bench quick-perf <vm>` (bench/gce/quick-perf.sh),
#                            which prints the same table with perf attached from the host.
#   QUICK_BUILD_ONLY=1       stop after building old and new (quick-perf uses the builds)
#   CHALLOC_THP_COUNTERS=1   after the table, the THP counter deltas around each run (plan
#                            Phase 2.5 task 17; bench/lib.sh thp_snapshot)
#   OUT=target/quick/out     raw logs
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
VALKEY_MODEL_SECS=${VALKEY_MODEL_SECS:-5}
. "$ROOT/bench/lib.sh"
OLD=${OLD:-HEAD}
NEW=${NEW:-tree}
OLD_CONF=${OLD_CONF:-}
NEW_CONF=${NEW_CONF:-}
QUICK_TOLERANCE=${QUICK_TOLERANCE:-5}
QUICK_BENCHES=${QUICK_BENCHES:-"larson1 larson10 xmalloc alloc-test1 valkey1"}
PERF=${PERF:-0}
PERF_BENCHES=${PERF_BENCHES:-"larson1 valkey1"}
PERF_EVENTS=${PERF_EVENTS:-"cycles,instructions,L1-dcache-load-misses,dTLB-load-misses"}
QUICK_BUILD_ONLY=${QUICK_BUILD_ONLY:-0}
CHURN_SECS=${CHURN_SECS:-8}
CHURN_CLASS=${CHURN_CLASS:-1024}
CHURN_MIN_OCC=${CHURN_MIN_OCC:-90}
BASELINES=${BASELINES:-}
BASELINE_TOLERANCE=${BASELINE_TOLERANCE:-3}
QUICK_SANITY=${QUICK_SANITY:-larson1}
OUT=${OUT:-$ROOT/target/quick/out}
QDIR=$ROOT/target/quick
T_START=$(date +%s)

take_bench_lock
ensure_bench_programs
ensure_churn
rm -rf "$OUT"; mkdir -p "$OUT"

# ---- builds -----------------------------------------------------------------
old_rev=$(resolve_rev "$OLD"); new_rev=$(resolve_rev "$NEW")
echo "== quick: $STAMP $ARCH nproc $PROCS load $(loadavg)"
echo "== old = $OLD ($(rev_label "$old_rev")), new = $NEW ($(if [ "$new_rev" = tree ]; then echo "HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')"; else rev_label "$new_rev"; fi))"
printf '== building old ... '; build_lib "$old_rev" "$QDIR/old"; echo done
printf '== building new ... '; build_lib "$new_rev" "$QDIR/new"; echo done
OLD_SO=$QDIR/old/release/libchalloc.so; NEW_SO=$QDIR/new/release/libchalloc.so
old_h=$(so_hash "$OLD_SO"); new_h=$(so_hash "$NEW_SO")
# Do the sources differ between the two variants?
src_differ=0
if [ "$old_rev" != "${old_rev#dir:}" ] || [ "$new_rev" != "${new_rev#dir:}" ]; then
  # A source directory (dir:PATH) is compared by content with the other
  # side when that side is the tree or another directory; against a git rev
  # the sources are taken to differ.
  src_of() { case $1 in tree) echo "$ROOT";; dir:*) echo "$ROOT/${1#dir:}";; *) echo "";; esac; }
  o=$(src_of "$old_rev"); n=$(src_of "$new_rev")
  if [ -z "$o" ] || [ -z "$n" ]; then src_differ=1
  elif ! diff -rq "$o/src" "$n/src" >/dev/null 2>&1 || ! cmp -s "$o/Cargo.toml" "$n/Cargo.toml"; then src_differ=1; fi
elif [ "$new_rev" = tree ]; then
  if [ "$old_rev" = tree ]; then src_differ=0
  else $GIT -C "$ROOT" diff --quiet "$old_rev" -- src Cargo.toml Cargo.lock build.rs 2>/dev/null || src_differ=1; fi
elif [ "$old_rev" = tree ]; then
  $GIT -C "$ROOT" diff --quiet "$new_rev" -- src Cargo.toml Cargo.lock build.rs 2>/dev/null || src_differ=1
else
  $GIT -C "$ROOT" diff --quiet "$old_rev" "$new_rev" -- src Cargo.toml Cargo.lock build.rs 2>/dev/null || src_differ=1
fi
BUILD_NOTE=
if [ "$old_h" = "$new_h" ]; then
  if [ $src_differ = 1 ]; then
    BUILD_NOTE="NOTE: sources differ but old and new are byte-identical ($old_h): the change does not affect the library"
  else BUILD_NOTE="old and new are the same build ($old_h): no source change between $OLD and $NEW"; fi
else
  if [ $src_differ = 0 ]; then
    BUILD_NOTE="WARNING: sources identical but the .so files differ ($old_h vs $new_h): nondeterministic build?"
  else BUILD_NOTE="old $old_h, new $new_h"; fi
fi
echo "== $BUILD_NOTE"
if [ "$QUICK_BUILD_ONLY" = 1 ]; then
  ensure_probe valkey_model
  echo "== QUICK_BUILD_ONLY: libraries built into target/quick/{old,new}/release/libchalloc.so, probes into target/bench-bin; not running the rows"
  exit 0
fi

# ---- cached jemalloc column -------------------------------------------------
BASELINE=
if [ "$BASELINES" = cached ]; then
  BASELINE=$(baseline_file)
  [ -f "$BASELINE" ] || die "BASELINES=cached but no $BASELINE for this bench build; run 'make baseline' on this machine first"
  echo "== jemalloc column from cache $BASELINE (date $(baseline_date "$BASELINE")); sanity round on: $QUICK_SANITY"
fi

# ---- one round --------------------------------------------------------------
VARIANTS="old new jemalloc"
lib_of() { case $1 in old) echo "$OLD_SO";; new) echo "$NEW_SO";; jemalloc) echo "$JEMALLOC_LIB";; esac; }
conf_of() { case $1 in old) echo "$OLD_CONF";; new) echo "$NEW_CONF";; *) echo "";; esac; }
[ -z "$OLD_CONF$NEW_CONF" ] || echo "== MALLOC_CONF: old '${OLD_CONF}', new '${NEW_CONF}'"
runs_jemalloc() { # runs_jemalloc BENCH: does jemalloc run BENCH this time?
  [ -z "$BASELINE" ] && return 0
  case " $QUICK_SANITY " in *" $1 "*) return 0;; *) return 1;; esac
}
for b in $QUICK_BENCHES; do
  printf '  %-12s' "$b"
  for v in $VARIANTS; do
    if [ "$v" = jemalloc ] && ! runs_jemalloc "$b"; then printf ' %s(cached)' "$v"; continue; fi
    run_row "$b" "$(lib_of "$v")" "$(conf_of "$v")" "$b.$v"; printf ' %s' "$v"
  done
  echo
done
printf '  %-12s' "churn $CHURN_CLASS"
for v in $VARIANTS; do run_churn "$(lib_of "$v")" "$(conf_of "$v")" "$v" "$CHURN_SECS" "$CHURN_CLASS"; printf ' %s' "$v"; done
echo

# ---- confirmation ---------------------------------------------------------------
# A single round on the reference VMs spreads 5..9% on larson (iteration 20 saw
# +5.1% on larson1 that three rounds put at -3%), so a row that fails the gate
# on one round gets QUICK_CONFIRM_ROUNDS more rounds of old and new, and the
# verdict uses the median. A row that passes on one round is not rerun.
QUICK_CONFIRM_ROUNDS=${QUICK_CONFIRM_ROUNDS:-2}
first_score() { awk 'NR==1 {print $1}' "$OUT/$1.$2.scores"; }
confirm=""
for b in $QUICK_BENCHES; do
  d=$(worse_pct "$b" "$(first_score "$b" new)" "$(first_score "$b" old)")
  awk -v d="$d" -v t="$QUICK_TOLERANCE" 'BEGIN {gsub(/[%+]/, "", d); if (d=="-" || d+0 > t+0) exit 1}' || confirm="$confirm $b"
done
if [ -n "$confirm" ] && [ "$QUICK_CONFIRM_ROUNDS" -gt 0 ]; then
  printf '  confirming%s: %s more rounds of old and new' "$confirm" "$QUICK_CONFIRM_ROUNDS"
  r=1
  while [ "$r" -le "$QUICK_CONFIRM_ROUNDS" ]; do
    for b in $confirm; do for v in old new; do run_row "$b" "$(lib_of "$v")" "$(conf_of "$v")" "$b.$v"; done; done
    printf ' .'; r=$((r+1))
  done
  echo
fi

# ---- table --------------------------------------------------------------------
# Medians over every round a row has (one, or 1 + QUICK_CONFIRM_ROUNDS).
score_of() { median_list "$(awk '$1!="-" {print $1}' "$OUT/$1.$2.scores" | sort -n | tr '\n' ' ')"; }
rss_of()   { median_list "$(awk '$1!="-" {print $2}' "$OUT/$1.$2.scores" | sort -n | tr '\n' ' ')"; }
rounds_of() { wc -l < "$OUT/$1.$2.scores" | tr -d ' '; }
fail=0; stale=0
echo
echo "| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |"
echo "|---|---|---:|---:|---:|---:|---:|"
for b in $QUICK_BENCHES; do
  o=$(score_of "$b" old); n=$(score_of "$b" new)
  or=$(kib_to_mib "$(rss_of "$b" old)"); nr=$(kib_to_mib "$(rss_of "$b" new)")
  jnote=
  if [ -n "$BASELINE" ]; then
    row=$(baseline_row "$BASELINE" "$b" jemalloc)
    j=$(median_list "${row%%|*}"); jr=$(kib_to_mib "${row##*|}"); jnote=" (cached)"
    if runs_jemalloc "$b"; then
      s=$(score_of "$b" jemalloc)
      dev=$(worse_pct "$b" "$s" "$j")
      jnote=" (cached; sanity $s, $dev)"
      awk -v d="$dev" -v t="$BASELINE_TOLERANCE" 'BEGIN {gsub(/[%+]/, "", d); if (d=="-" || (d<0?-d:d) > t) exit 1}' || stale=1
    fi
  else
    j=$(score_of "$b" jemalloc); jr=$(kib_to_mib "$(rss_of "$b" jemalloc)")
  fi
  d_old=$(worse_pct "$b" "$n" "$o"); d_je=$(worse_pct "$b" "$n" "$j")
  awk -v d="$d_old" -v t="$QUICK_TOLERANCE" 'BEGIN {gsub(/[%+]/, "", d); if (d=="-" || d+0 > t+0) exit 1}' || fail=1
  rn=$(rounds_of "$b" new); rnote=; [ "$rn" -gt 1 ] && rnote=" (median of $rn)"
  echo "| $b | $(unit_of "$b") | $o ($or MiB) | $n ($nr MiB)$rnote | $j ($jr MiB)$jnote | $d_old | $d_je |"
done
# churn row: slabs after the random 56% delete -> slabs at the end of the churn
churn_cell() { awk '{printf "%s -> %s (%s%%)", $1, $2, $5}' "$OUT/$1.churn"; }
churn_ok() { awk -v m="$CHURN_MIN_OCC" '{ if ($1=="-" || $2=="-" || $5=="-" || $4!=0 || $5+0 < m) exit 1 }' "$OUT/$1.churn"; }
nverdict=$(churn_ok new && echo "compacts" || echo "DOES NOT COMPACT")
[ "$nverdict" = compacts ] || fail=1
echo "| churn $CHURN_CLASS, ${CHURN_SECS}s | slabs deleted -> end (occ) | $(churn_cell old) | $(churn_cell new) | $(churn_cell jemalloc) | new $nverdict (end occupancy >= $CHURN_MIN_OCC%) | $(awk '{print $3}' "$OUT/new.churn") ops/s |"
echo
echo "Positive percentages mean new did worse (slower, lower throughput). One round each, $ARCH, nproc $PROCS, load at start $(loadavg | cut -d' ' -f1), $(( $(date +%s) - T_START )) s total. $BUILD_NOTE."
if [ "$CHALLOC_THP_COUNTERS" = 1 ]; then
  echo
  echo "THP counters around each run (deltas of /proc/vmstat thp_* and hugepages-<size>kB/stats/*, host-global; sysfs: $(thp_state)):"
  echo
  echo "| row | variant | deltas (first round) |"
  echo "|---|---|---|"
  for b in $QUICK_BENCHES; do for v in $VARIANTS; do [ -f "$OUT/$b.$v.0.thp" ] && echo "| $b | $v | $(cat "$OUT/$b.$v.0.thp") |"; done; done
fi

# ---- per-operation counters (PERF=1) ---------------------------------------------
# One run per (row, variant) under `perf stat -x,` with the four events the
# attribution files use, divided by the operation count the program reports
# (bench/lib.sh ops_of). This is the container-side form; on the VMs perf
# lives on the host and `challoc-bench quick-perf <vm>` prints the same table.
if [ "$PERF" = 1 ]; then
  echo
  if ! command -v perf >/dev/null 2>&1; then
    echo "PERF=1: no perf in this container (Wolfi has none). On a VM run 'bench/gce/challoc-bench quick-perf <arm64|amd64>' for the per-operation table (perf attaches from the host)."
  else
    echo "| row | variant | M ops/s | cycles/op | instr/op | L1D miss/op | dTLB miss/op |"
    echo "|---|---|---:|---:|---:|---:|---:|"
    for b in $PERF_BENCHES; do
      for v in $VARIANTS; do
        set -- $(bench_args "$b")
        perf stat -x, -e "$PERF_EVENTS" -o "$OUT/perf.$b.$v.csv" -- env MALLOC_CONF="$(conf_of "$v")" LD_PRELOAD="$(lib_of "$v")" "$@" > "$OUT/perf.$b.$v.out" 2>&1 || true
        ops=$(ops_of "$b" "$OUT/perf.$b.$v.out")
        secs=$(secs_of "$b")
        awk -F, -v ops="$ops" -v secs="$secs" -v row="$b" -v var="$v" '
          /^#/ || NF < 3 {next}
          {c[$3]=$1}
          END {
            mops = (ops!="-" && secs!="-" && secs>0) ? sprintf("%.2f", ops/secs/1e6) : "-"
            if (ops=="-" || ops==0) { printf "| %s | %s | %s | - | - | - | - |\n", row, var, mops; exit }
            printf "| %s | %s | %s | %.1f | %.1f | %.3f | %.3f |\n", row, var, mops, c["cycles"]/ops, c["instructions"]/ops, c["L1-dcache-load-misses"]/ops, c["dTLB-load-misses"]/ops
          }' "$OUT/perf.$b.$v.csv"
      done
    done
    echo
    echo "Per-operation counters from one run each (perf stat -x, -e $PERF_EVENTS on the program's process; ops from the program's own count). Compare variants within a row; these are for attribution, not for the gate."
  fi
fi
[ $stale = 0 ] || { echo "quick: the jemalloc sanity run is more than $BASELINE_TOLERANCE% off the cached median: the baseline is STALE, rerun 'make baseline'" >&2; exit 2; }
if [ $fail = 1 ]; then
  echo "quick: NO-GO (new trails old by more than $QUICK_TOLERANCE% on a row, or does not compact)"; exit 1
fi
echo "quick: go (new within $QUICK_TOLERANCE% of old on every row; churn compacts)"
