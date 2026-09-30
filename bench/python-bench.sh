#!/bin/sh
# Python benchmark rows (docs/plan.md Phase 2.5 task 1): the stdlib workloads
# under bench/probes/python/ on the image's python3 (Wolfi python-3.13),
# measured under five allocators via LD_PRELOAD, each workload two ways:
# PYTHONMALLOC unset (pymalloc serves objects under 512 bytes, the allocator
# sees the rest) and PYTHONMALLOC=malloc (every object through the
# allocator). Runs inside the dev container: `make python-bench`, or on a VM
# `bench/gce/challoc-bench make arm64 python-bench HOST_KIND=...`.
#
# pyperformance 1.14.0 does `pip install` in the container (checked on both
# VMs, 2026-09-18) but is not used: its standard set is 100 benchmarks run by
# pyperf's own worker-process and repetition protocol (about an hour per
# interpreter configuration; ten configurations here), which does not fit
# the harness's protocol of one process per row under bench/rt, five
# interleaved rounds and peak RSS. The workloads here are fixed stdlib
# scripts instead (the plan's fallback):
#
#   json          bench/probes/python/json_bench.py: json.dumps/json.loads of a
#                 20000-record structure (7 MB of text), 40 rounds
#   dict_list     dict_list_bench.py: a ring of 2000 live dicts (1..2000 keys) and
#                 lists (1..10000 ints), 30000 replacements with lookups and deletes
#   regex         regex_bench.py: findall/finditer/sub/split with four patterns over
#                 3.5 MB of generated text, 6 rounds
#   object_graph  object_graph_bench.py: a 150000-instance tree with cross links,
#                 BFS walk, subtree detach and rebuild, 2 rounds
#
# Rows are WORKLOAD-MODE (json-default, json-malloc, ...), one process per
# round under bench/rt: wall time (s, lower is better) and max RSS.
#
# Allocators (ALLOCS): challoc (REV, default the working tree, built into
# target/python/lib), jemalloc (Wolfi's /usr/lib/libjemalloc.so.2), glibc (no
# preload), mimalloc (bench/build.sh's libmimalloc.so), tcmalloc (Wolfi's
# tcmalloc-minimal). Every allocator's /proc/self/maps is checked once
# before the rounds: exactly the expected library is mapped.
#
#   ROUNDS=5     timed rounds after one discarded warm-up, allocators
#                interleaved within a round, every score listed sorted
#   WORKLOADS="json dict_list regex object_graph"   MODES="default malloc"
#   RESULT=path  markdown output (default target/results/<date>-<arch>-python.md)
#   OUT=dir      raw logs and score files (default target/python/bench-out)
#   REV=tree     challoc rev (any git rev; "tree" = the working tree)
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc glibc mimalloc tcmalloc"}
ROUNDS=${ROUNDS:-5}
WORKLOADS=${WORKLOADS:-"json dict_list regex object_graph"}
MODES=${MODES:-"default malloc"}
REV=${REV:-tree}
WORK=$ROOT/target/python
OUT=${OUT:-$WORK/bench-out}
RESULT=${RESULT:-$ROOT/target/results/$DATE-$ARCH-python.md}
PROBES=$ROOT/bench/probes/python
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
python_available || die "no $PYTHON"
for a in $ALLOCS; do case $a in challoc) ;; *) is_stock "$a" || die "unknown allocator '$a'";; esac; done
for w in $WORKLOADS; do [ -f "$PROBES/${w}_bench.py" ] || die "no $PROBES/${w}_bench.py"; done
for m in $MODES; do case $m in default|malloc) ;; *) die "unknown mode '$m' (default|malloc)";; esac; done
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- the challoc library --------------------------------------------------------
CHALLOC_LIB=
case " $ALLOCS " in *" challoc "*)
  rev=$(resolve_rev "$REV")
  printf '== building challoc (%s) into target/python/lib ... ' "$(rev_label "$rev")"
  build_lib "$rev" "$WORK/lib"; CHALLOC_LIB=$WORK/lib/release/libchalloc.so
  echo "done, $(so_hash "$CHALLOC_LIB")";;
esac
lib_of() { default_lib_of "$1"; }
mode_env() { case $1 in malloc) echo "PYTHONMALLOC=malloc";; *) echo "";; esac; }

# ---- binding check ----------------------------------------------------------------
PY_VERSION=$("$PYTHON" --version 2>&1)
echo "== python: $PY_VERSION (Wolfi python-3.13 $(apk_version python-3.13)), pymalloc built in: $("$PYTHON" -c 'import sysconfig; print(sysconfig.get_config_var("WITH_PYMALLOC"))')"
for a in $ALLOCS; do
  lib=$(lib_of "$a")
  mapped=$(env ${lib:+LD_PRELOAD=$lib} "$PYTHON" -c 'import re; print(" ".join(sorted({l.split()[5] for l in open("/proc/self/maps") if re.search(r"/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\.so", l)})))')
  # /proc/self/maps shows the resolved file (libtcmalloc_minimal.so.4.6.5 for the .so.4 link)
  maps_expect "$a" python "$lib" "$mapped"
done
# PYTHONMALLOC=malloc must be accepted (an unknown value makes python exit 1).
env PYTHONMALLOC=malloc "$PYTHON" -c 'pass' || die "PYTHONMALLOC=malloc rejected by $PYTHON"

# ---- one row --------------------------------------------------------------------------
# run_py_row WORKLOAD MODE ALLOC: appends "wall_s maxrss_kb exit" to $OUT/W-M.ALLOC.scores.
run_py_row() {
  w=$1; m=$2; a=$3; tag=$w-$m.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  lib=$(lib_of "$a")
  RT_OUT=$rt "$BIN/rt" env ${lib:+LD_PRELOAD=$lib} $(mode_env "$m") "$PYTHON" "$PROBES/${w}_bench.py" > "$log" 2>&1 || true
  record_rt "$tag" "$rt" "$log"
}
ROWS=; for w in $WORKLOADS; do for m in $MODES; do ROWS="$ROWS $w-$m"; done; done; ROWS=${ROWS# }

# ---- rounds -------------------------------------------------------------------------
echo "== $ROUNDS rounds + warm-up x $(echo $ROWS | wc -w) rows x $(echo $ALLOCS | wc -w) allocators ($ALLOCS); load $LOAD0"
r=0
while [ $r -le "$ROUNDS" ]; do
  for a in $ALLOCS; do
    for w in $WORKLOADS; do for m in $MODES; do run_py_row "$w" "$m" "$a"; done; done
  done
  printf '   round %d done, load %s, %s\n' $r "$(loadavg)" "$(date -u +%T)"
  r=$((r+1))
done
check_score_lines "$ROWS" "$ALLOCS" $((ROUNDS+1))

# ---- report ---------------------------------------------------------------------------
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
rss_of() { median_rss "$OUT/$1.$2.scores" 1; }
med_of() { median_list "$(list_of "$1" "$2")"; }
describe() { case $1 in challoc) describe_challoc "$CHALLOC_LIB" "$REV";; *) describe_stock "$1";; esac; }
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
{
  result_header "Python 3.13 benchmark rows"
  echo "- python: $PY_VERSION (Wolfi python-3.13 $(apk_version python-3.13), pymalloc built in); modes: default = PYTHONMALLOC unset (pymalloc for objects under 512 bytes, the allocator sees the rest), malloc = PYTHONMALLOC=malloc (every object through the allocator)"
  echo "- workloads (bench/probes/python/): json = json_bench.py (json.dumps/loads of 20000 records, 40 rounds); dict_list = dict_list_bench.py (2000 live dicts and lists, 30000 replacements); regex = regex_bench.py (four patterns over 3.5 MB, 6 rounds); object_graph = object_graph_bench.py (150000-node tree, BFS, detach and rebuild, 2 rounds)"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- method: $ROUNDS timed rounds after one discarded warm-up, allocators interleaved per round, one python3 process per row and round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB). Percentages are challoc against the other allocator: positive means challoc is slower or larger."
  echo "- allocators:"
  for a in $ALLOCS; do echo "  - $a: $(describe "$a")"; done
  echo
  echo "## Sorted scores (wall seconds)"
  echo
  echo "| row | allocator | sorted wall s | maxrss KiB |"
  echo "|---|---|---|---:|"
  for row in $ROWS; do for a in $ALLOCS; do
    flag=; [ "$(any_failed "$OUT/$row.$a.scores" 1)" = 0 ] || flag=" (FAILED)"
    echo "| $row | $a | $(list_of "$row" "$a")$flag | $(rss_of "$row" "$a") |"
  done; done
  echo
  echo "## Medians"
  echo
  hdr="| row |"; sep="|---|"
  for a in $ALLOCS; do hdr="$hdr $a s | $a RSS MiB |"; sep="$sep---:|---:|"; done
  if has challoc; then for a in $ALLOCS; do [ "$a" = challoc ] || { hdr="$hdr challoc vs $a time | RSS |"; sep="$sep---:|---:|"; }; done; fi
  echo "$hdr"; echo "$sep"
  for row in $ROWS; do
    line="| $row |"; deltas=
    for a in $ALLOCS; do line="$line $(med_of "$row" "$a") | $(kib_to_mib "$(rss_of "$row" "$a")") |"; done
    if has challoc; then
      cm=$(med_of "$row" challoc); cr=$(rss_of "$row" challoc)
      for a in $ALLOCS; do [ "$a" = challoc ] || deltas="$deltas $(pct_of "$cm" "$(med_of "$row" "$a")") | $(pct_of "$cr" "$(rss_of "$row" "$a")") |"; done
    fi
    echo "$line$deltas"
  done
  echo
  echo "Positive percentages mean challoc did worse than that allocator (slower, or more RSS)."
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Medians/,$p' "$RESULT"
