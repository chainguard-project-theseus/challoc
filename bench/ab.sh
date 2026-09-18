#!/bin/sh
# Protocol tier of the benchmark harness (bench/README.md): the A/B the
# results files in bench/results/ are written from, replacing the ad-hoc
# .tmp-itNN-inner.sh scripts. Run inside the dev container: `make ab
# VARIANTS='...'` or, on a VM, `bench/gce/challoc-bench ab arm64 VARIANTS='...'`.
#
# Every variant is a library built from a git rev (or the working tree) into
# its own CARGO_TARGET_DIR under target/ab/, optionally with a MALLOC_CONF, or
# one of the stock allocators. ROUNDS timed rounds plus one discarded warm-up;
# within a round every variant runs every bench (interleaved, so drift hits
# all variants alike); every run's score is listed sorted, RSS is bench/rt's
# median maxrss in KiB, and the load average at the start is recorded. The
# output is the markdown table shape of bench/results/*-gce-*.md.
#
#   VARIANTS="old=HEAD new=tree jemalloc"
#       space-separated "name=rev[:MALLOC_CONF]" or a stock allocator name
#       (jemalloc, glibc, mimalloc). rev is any git rev, or "tree" for the
#       working tree. Two variants with the same rev share one build, so
#       `narenas16=HEAD~1:narenas:16` costs no extra compile next to `old=HEAD~1`.
#   ROUNDS=5                 timed rounds (plus one warm-up)
#   BENCHES="larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN"
#       rows from bench/lib.sh bench_args (also larsonN xmallocN valkey1 valkey1-free valkey1-usable
#       cfrac espresso mstress glibc-simple redis ruby-churn)
#   ADAPTIVE=1               stop after 3 timed rounds when, on every bench, no two
#                            variants' sorted lists overlap (the answer is already clear)
#   BASELINES=cached         stock variants come from target/baselines/<arch>-<hash>.md
#                            (bench/baseline.sh) instead of running; jemalloc still runs
#                            the warm-up and one sanity round that must land within
#                            BASELINE_TOLERANCE (3) percent of the cached median on every
#                            bench, otherwise the cache is stale (exit 2). Acceptance runs
#                            keep the default: everything measured in the same run.
#   RESULT=path              write the markdown there (default target/ab/result.md); the
#                            tables are printed either way
#   HOST_CPU, HOST_KIND      recorded in the header (the Makefile passes HOST_CPU)
#   TITLE="A/B"              first heading word
#   OUT=target/ab/out        raw logs and score files
#   VALKEY_MODEL_SECS=10     seconds of the valkey1 / valkey1-free rows (bench/lib.sh)
#   PIN=                     1 or a CPU list: run every row under bench/pin.c (experiment)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
VARIANTS=${VARIANTS:-"old=HEAD new=tree jemalloc"}
ROUNDS=${ROUNDS:-5}
BENCHES=${BENCHES:-"larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN"}
ADAPTIVE=${ADAPTIVE:-0}
BASELINES=${BASELINES:-}
BASELINE_TOLERANCE=${BASELINE_TOLERANCE:-3}
ABDIR=$ROOT/target/ab
OUT=${OUT:-$ABDIR/out}
RESULT=${RESULT:-$ABDIR/result.md}
TITLE=${TITLE:-A/B}
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
rm -rf "$OUT"; mkdir -p "$OUT/v" "$(dirname "$RESULT")"

# ---- variants -----------------------------------------------------------------
# $OUT/v/<name>.{rev,conf,lib,cached}; NAMES keeps the order given.
NAMES=
for spec in $VARIANTS; do
  conf=; cached=0
  case $spec in
    *=*) name=${spec%%=*}; rest=${spec#*=}
         case $rest in *:*) rev=${rest%%:*}; conf=${rest#*:};; *) rev=$rest;; esac
         if is_stock "$rev"; then lib=$(stock_lib "$rev"); rev=stock:$rev
         else lib=; rev=$(resolve_rev "$rev") || exit 1; [ -n "$rev" ] || exit 1; fi;;
    *)   name=$spec; is_stock "$name" || die "variant '$spec' is not name=rev[:conf] and not a stock allocator"
         lib=$(stock_lib "$name"); rev=stock:$name;;
  esac
  case $name in *[!A-Za-z0-9_.-]*) die "variant name '$name' must be [A-Za-z0-9_.-]";; esac
  [ -e "$OUT/v/$name.rev" ] && die "variant '$name' given twice"
  [ "$BASELINES" = cached ] && case $rev in stock:*) cached=1;; esac
  echo "$rev" > "$OUT/v/$name.rev"; echo "$conf" > "$OUT/v/$name.conf"; echo "$lib" > "$OUT/v/$name.lib"; echo "$cached" > "$OUT/v/$name.cached"
  NAMES="$NAMES $name"
done
NAMES=${NAMES# }
v_rev() { cat "$OUT/v/$1.rev"; }; v_conf() { cat "$OUT/v/$1.conf"; }; v_lib() { cat "$OUT/v/$1.lib"; }
v_cached() { [ "$(cat "$OUT/v/$1.cached")" = 1 ]; }
for b in $BENCHES; do bench_args "$b" >/dev/null || die "unknown or unavailable bench $b"; done

BASELINE=
if [ "$BASELINES" = cached ]; then
  BASELINE=$(baseline_file)
  [ -f "$BASELINE" ] || die "BASELINES=cached but no $BASELINE for this bench build; run 'make baseline' on this machine first"
  for b in $BENCHES; do [ -n "$(baseline_row "$BASELINE" "$b" jemalloc)" ] || die "$BASELINE has no jemalloc row for $b; rerun 'make baseline' with BENCHES covering it"; done
  # The sanity variant is jemalloc whether or not it was asked for.
  case " $NAMES " in *" jemalloc "*) ;; *) echo stock:jemalloc > "$OUT/v/jemalloc.rev"; : > "$OUT/v/jemalloc.conf"; stock_lib jemalloc > "$OUT/v/jemalloc.lib"; echo 1 > "$OUT/v/jemalloc.cached"; NAMES="$NAMES jemalloc";; esac
fi

# ---- builds: one per distinct rev -------------------------------------------
echo "== $TITLE: $STAMP $ARCH nproc $PROCS load $LOAD0, HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')"
for v in $NAMES; do
  rev=$(v_rev "$v")
  case $rev in stock:*) continue;; esac
  if [ "$rev" = tree ]; then tdir=$ABDIR/tree; else tdir=$ABDIR/$(echo "$rev" | cut -c1-12); fi
  if [ ! -e "$OUT/built.$(basename "$tdir")" ]; then
    printf '== building %s (%s) into %s ... ' "$v" "$(echo "$rev" | cut -c1-12)" "${tdir#"$ROOT"/}"
    build_lib "$rev" "$tdir"; echo "done, $(so_hash "$tdir/release/libchalloc.so")"
    : > "$OUT/built.$(basename "$tdir")"
  fi
  echo "$tdir/release/libchalloc.so" > "$OUT/v/$v.lib"
done

# ---- rounds -------------------------------------------------------------------
runs_this_round() { # runs_this_round VARIANT ROUND
  v_cached "$1" || return 0
  [ "$1" = jemalloc ] && [ "$2" -le 1 ] && return 0
  return 1
}
all_separated() { # 0 when on every bench no two variants' sorted lists overlap
  for b in $BENCHES; do
    for v in $NAMES; do
      after=0
      for w in $NAMES; do
        if [ $after = 1 ]; then
          lists_overlap "$(list_of "$b" "$v")" "$(list_of "$b" "$w")" && return 1
        fi
        [ "$w" = "$v" ] && after=1
      done
    done
  done
  return 0
}
list_of() { # list_of BENCH VARIANT: sorted timed scores (cached variants: from the baseline)
  if v_cached "$2"; then row=$(baseline_row "$BASELINE" "$1" "$2"); echo "${row%%|*}"; else sorted_scores "$OUT/$1.$2.scores" 1; fi
}
rss_of() { if v_cached "$2"; then row=$(baseline_row "$BASELINE" "$1" "$2"); echo "${row##*|}"; else median_rss "$OUT/$1.$2.scores" 1; fi; }

echo "== $ROUNDS rounds + warm-up x $(echo $BENCHES | wc -w) benches x $(echo $NAMES | wc -w) variants ($NAMES)"
r=0; DONE_ROUNDS=$ROUNDS
while [ $r -le "$ROUNDS" ]; do
  for v in $NAMES; do
    runs_this_round "$v" $r || continue
    for b in $BENCHES; do run_row "$b" "$(v_lib "$v")" "$(v_conf "$v")" "$b.$v"; done
  done
  printf '   round %d done, load %s, %s\n' $r "$(loadavg)" "$(date -u +%T)"
  if [ "$ADAPTIVE" = 1 ] && [ $r -eq 3 ] && [ "$ROUNDS" -gt 3 ] && all_separated; then
    echo "   adaptive: every pair of variants is separated on every bench after 3 rounds; stopping"
    DONE_ROUNDS=3; break
  fi
  r=$((r+1))
done

# Every non-cached score file must hold warm-up + DONE_ROUNDS lines.
for b in $BENCHES; do
  for v in $NAMES; do
    v_cached "$v" && continue
    n=$(wc -l < "$OUT/$b.$v.scores")
    [ "$n" -eq $((DONE_ROUNDS+1)) ] || die "$OUT/$b.$v.scores has $n lines, expected $((DONE_ROUNDS+1)): concurrent run?"
  done
done

# ---- cache staleness --------------------------------------------------------
stale=0; SANITY_NOTE=
if [ -n "$BASELINE" ]; then
  for b in $BENCHES; do
    s=$(awk 'NR==2 {print $1}' "$OUT/$b.jemalloc.scores")
    m=$(median_list "$(list_of "$b" jemalloc)")
    dev=$(worse_pct "$b" "$s" "$m")
    SANITY_NOTE="$SANITY_NOTE $b $s vs $m ($dev);"
    awk -v d="$dev" -v t="$BASELINE_TOLERANCE" 'BEGIN {gsub(/[%+]/, "", d); if (d=="-" || (d<0?-d:d) > t) exit 1}' || stale=1
  done
fi

# ---- report -----------------------------------------------------------------
describe() { # describe VARIANT
  rev=$(v_rev "$1"); conf=$(v_conf "$1")
  case $rev in
    stock:jemalloc) d="Wolfi's $JEMALLOC_LIB ($(apk list --installed 2>/dev/null | awk '/^jemalloc-[0-9]/ {print $1}' | sed 's/^jemalloc-//;s/-r[0-9]*$//' | head -1))";;
    stock:glibc)    d="glibc malloc (no LD_PRELOAD)";;
    stock:mimalloc) d="$BIN/libmimalloc.so ($(awk '/^mimalloc /{print $2}' "$BIN/versions.txt"))";;
    tree)           d="working tree (HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')) built into target/ab/tree";;
    *)              d="$(echo "$rev" | cut -c1-12) built into target/ab/$(echo "$rev" | cut -c1-12)";;
  esac
  [ -n "$conf" ] && d="$d, MALLOC_CONF=$conf"
  v_cached "$1" && d="$d, CACHED from $BASELINE"
  echo "$d"
}
first=${NAMES%% *}
{
  echo "# challoc $TITLE, $DATE, $ARCH"
  echo
  echo "- commit: $(commit_short)$(tree_dirty && echo ' (uncommitted changes in src/)')"
  echo "- date: $STAMP"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container"
  echo "- kernel: $(uname -r) (${HOST_KIND:-$(case $(uname -r) in *linuxkit*) echo "Docker Desktop VM";; *) echo "host kernel";; esac)}), container: Wolfi"
  sed 's/^/- /' "$BIN/versions.txt" | grep -E 'mimalloc-bench|jemalloc-[0-9]|rust-' || true
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- method: $DONE_ROUNDS timed rounds after one discarded warm-up$([ "$DONE_ROUNDS" != "$ROUNDS" ] && echo " (ADAPTIVE stop, $ROUNDS asked)"), variants interleaved per round, every run's score listed sorted; RSS is bench/rt's median maxrss (KiB) over the timed runs; identical binaries, only LD_PRELOAD and MALLOC_CONF change. Positive percentages mean the variant did worse than $first."
  echo "- programs: larson 5 8 1000 5000 100 4141 N (larsonN), M ops/s, higher is better; xmalloc-test -w 10 -t 5 -s 64 (xmalloc), M frees/s, higher is better; alloc-test 1 and alloc-test min(nproc,16) (alloc-testN), seconds, lower is better; valkey_model -n $VALKEY_MODEL_N -t $VALKEY_MODEL_SECS (valkey1: frees through sdallocx where exported; valkey1-free: plain free), write-phase M ops/s, higher is better, RSS is the peak (bench/probes/valkey_model.c); redis: redis-benchmark -r 1000000 -n $REDIS_N -P 16 lpush/lrange, k req/s, RSS is redis-server's; ruby-churn: bench/ruby/gc_churn.rb 10 10 on the glibc Ruby build; others: wall seconds$([ -n "$PIN" ] && [ "$PIN" != 0 ] && echo "; PIN=$PIN (rows pinned with bench/pin.c, an experiment)")"
  echo "- variants:"
  for v in $NAMES; do echo "  - $v: $(describe "$v")"; done
  if [ -n "$BASELINE" ]; then
    echo "- cached baseline: $BASELINE (date $(baseline_date "$BASELINE")); jemalloc sanity round vs cached median:$SANITY_NOTE $([ $stale = 1 ] && echo "STALE (more than $BASELINE_TOLERANCE% off)" || echo "within $BASELINE_TOLERANCE%")"
  fi
  echo
  echo "## Sorted scores"
  echo
  echo "| program | variant | sorted scores | maxrss KiB |"
  echo "|---|---|---|---:|"
  for b in $BENCHES; do
    for v in $NAMES; do
      flag=; v_cached "$v" || { [ "$(any_failed "$OUT/$b.$v.scores" 1)" = 0 ] || flag=" (FAILED)"; }
      echo "| $b | $v | $(list_of "$b" "$v")$flag | $(rss_of "$b" "$v") |"
    done
  done
  echo
  echo "## Medians"
  echo
  hdr="| program | unit |"; sep="|---|---|"
  for v in $NAMES; do hdr="$hdr $v | $v RSS MiB |"; sep="$sep---:|---:|"; done
  for v in $NAMES; do [ "$v" = "$first" ] || { hdr="$hdr $v vs $first |"; sep="$sep---:|"; }; done
  echo "$hdr"; echo "$sep"
  for b in $BENCHES; do
    row="| $b | $(unit_of "$b") |"; deltas=
    fm=$(median_list "$(list_of "$b" "$first")")
    for v in $NAMES; do
      m=$(median_list "$(list_of "$b" "$v")")
      row="$row $m | $(kib_to_mib "$(rss_of "$b" "$v")") |"
      [ "$v" = "$first" ] || deltas="$deltas $(worse_pct "$b" "$m" "$fm") |"
    done
    echo "$row$deltas"
  done
  echo
  echo "Positive percentages mean the variant did worse than $first (slower or lower throughput)."
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Sorted scores/,$p' "$RESULT"
[ $stale = 0 ] || { echo "ab: jemalloc sanity round more than $BASELINE_TOLERANCE% off the cached median: the baseline is STALE, rerun 'make baseline'" >&2; exit 2; }
