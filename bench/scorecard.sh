#!/bin/sh
# Scorecard (bench/README.md, docs/plan.md Phase 1.5 task 5c): the trend
# record of the implementation loop. One run measures the fixed row set
#
#   larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN
#   valkey1 valkey1-free cfrac espresso mstress glibc-simple redis
#   python1 python1-malloc
#
# under challoc (REV, default the working tree) and jemalloc through
# bench/ab.sh (ROUNDS timed rounds plus a warm-up, variants interleaved,
# sorted scores, median RSS; BASELINES=cached takes jemalloc from the cached
# baseline with ab.sh's sanity round) and appends ONE line for (date, commit)
# to bench/results/scorecard-<arch>.md: every row as a signed percentage
# against jemalloc (positive = challoc worse, bench/lib.sh worse_pct), the
# RSS ratio challoc/jemalloc for larson10 and valkey1, and a soak column
# group (write and delete throughput, peak RSS, post-delete RSS, each as
# challoc vs jemalloc) read from the most recent Valkey soak summary for the
# same commit under bench/results/ or target/valkey/soak-quick/, "-" when
# there is none. A line for the same (date, commit) is replaced, not
# duplicated. Run inside the dev container: `make scorecard`, or on a VM
# `bench/gce/challoc-bench scorecard <arm64|amd64>` (which fetches the file).
#
#   bench/scorecard.sh --render [aarch64|x86_64]   print the table(s) sorted by
#                                                  date (works on the host too)
#
#   REV=tree            challoc rev (any git rev; "tree" = working tree, marked
#                       "<hash>+" when src/ has uncommitted changes)
#   ROUNDS=3            timed rounds (plus warm-up)
#   ROWS=<the fixed set>  a subset for a partial run; the other columns read "-"
#   BASELINES=cached    jemalloc from target/baselines/<arch>-<hash>.md, which
#                       must cover every row (make baseline BENCHES="$ROWS")
#   VALKEY_MODEL_SECS=10, VALKEY_MODEL_N=850000, PIN   as in bench/lib.sh
#   HOST_KIND           recorded in the note column
#   SCORECARD_FILE      override the output file (tests)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
COLS="larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN valkey1 valkey1-free cfrac espresso mstress glibc-simple redis python1 python1-malloc"
HEADER="| date | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 | soak write | soak delete | soak peak RSS | soak post-delete RSS | note |"
SEP="|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"
# The column set grew once: python1 and python1-malloc (Phase 2.5 task 1,
# 2026-09-18) were inserted after redis. pad_old_lines rewrites a line of the
# previous shape (two cells fewer) with "-" in the two new cells, so older
# scorecard lines stay aligned under the current header.
pad_old_lines() {
  awk -F'|' -v want="$(echo "$HEADER" | awk -F'|' '{print NF}')" '
    /^\| 20[0-9][0-9]-/ && NF == want-2 {
      out = $1
      for (i = 2; i <= NF; i++) { out = out "|" $i; if (i == 16) out = out "| - | - " }
      print out; next }
    {print}'
}

# ---- --render: the tables sorted by date, header first ---------------------
render() {
  case ${1:-} in
    "")    set -- "$ROOT"/bench/results/scorecard-*.md;;
    *.md)  ;;                                          # a file path (a SCORECARD_FILE test file)
    *)     set -- "$ROOT/bench/results/scorecard-$1.md";;
  esac
  for f in "$@"; do
    [ -f "$f" ] || { echo "scorecard: no $f" >&2; continue; }
    echo "## ${f##*/}"
    echo
    echo "$HEADER"; echo "$SEP"
    grep '^| 20[0-9][0-9]-' "$f" | pad_old_lines | sort -t '|' -k2,2 -s
    echo
  done
  echo "Percentages: challoc against jemalloc, positive = challoc worse (slower, lower throughput). RSS columns: challoc/jemalloc ratio. Soak columns: the soak summary's challoc vs jemalloc for the worse of the two generators' throughput rows, peak RSS (VmHWM) and RSS after settle. Lines marked '(from ...)' in the note were reconstructed from that results file."
}
if [ "${1:-}" = --render ]; then render "${2:-}"; exit 0; fi
[ $# -eq 0 ] || { echo "usage: bench/scorecard.sh [--render [aarch64|x86_64]]" >&2; exit 2; }

# ---- a run ------------------------------------------------------------------
. "$ROOT/bench/lib.sh"
REV=${REV:-tree}
ROUNDS=${ROUNDS:-3}
ROWS=${ROWS:-$COLS}
BASELINES=${BASELINES:-}
SCDIR=$ROOT/target/scorecard
RESULT=$SCDIR/result.md
OUT=$SCDIR/out
FILE=${SCORECARD_FILE:-$ROOT/bench/results/scorecard-$ARCH.md}
T_START=$(date +%s)
mkdir -p "$SCDIR"

for b in $ROWS; do case " $COLS " in *" $b "*) ;; *) die "row $b is not a scorecard column ($COLS)";; esac; done
rev=$(resolve_rev "$REV")
if [ "$rev" = tree ]; then commit=$(commit_short); tree_dirty && commit="$commit+"; else commit=$(echo "$rev" | cut -c1-7); fi
echo "== scorecard: $STAMP $ARCH, challoc $REV ($commit) vs jemalloc, $ROUNDS rounds, rows: $ROWS${BASELINES:+; jemalloc from the cached baseline (sanity round)}"
[ -z "$BASELINES" ] || echo "== (the cache must cover every row; if ab.sh stops on a missing row run: make baseline BENCHES=\"$ROWS\")"
set +e
VARIANTS="challoc=$REV jemalloc" ROUNDS=$ROUNDS BENCHES="$ROWS" RESULT=$RESULT OUT=$OUT TITLE=scorecard ADAPTIVE=0 BASELINES="$BASELINES" sh "$ROOT/bench/ab.sh"
st=$?
set -e
[ "$st" != 2 ] || die "the cached jemalloc baseline is stale (ab.sh exit 2): rerun 'make baseline BENCHES=\"$ROWS\"' or run without BASELINES=cached; nothing appended"
[ "$st" = 0 ] || die "bench/ab.sh failed (exit $st); nothing appended"

# ---- cells from the ab.sh result ----------------------------------------------
median_of() { row=$(baseline_row "$RESULT" "$1" "$2"); median_list "${row%%|*}"; }
rss_of() { row=$(baseline_row "$RESULT" "$1" "$2"); echo "${row##*|}"; }
cell() { # cell ROW: challoc vs jemalloc, "-" when the row did not run
  case " $ROWS " in *" $1 "*) ;; *) echo "-"; return;; esac
  worse_pct "$1" "$(median_of "$1" challoc)" "$(median_of "$1" jemalloc)"
}
ratio() { # ratio ROW: challoc RSS / jemalloc RSS
  case " $ROWS " in *" $1 "*) ;; *) echo "-"; return;; esac
  awk -v c="$(rss_of "$1" challoc)" -v j="$(rss_of "$1" jemalloc)" 'BEGIN {if (c=="-"||j=="-"||c==""||j==""||j==0) print "-"; else printf "%.2f", c/j}'
}

# ---- the soak column group ------------------------------------------------------
# The newest soak summary for this commit on this architecture: the acceptance
# soaks under bench/results/ and the quick soaks under target/valkey/soak-quick/.
soak_file() {
  c=${commit%+}
  for f in $(ls -t "$ROOT"/bench/results/*-"$ARCH"-*valkey-soak*.md "$ROOT"/target/valkey/soak-quick/*-"$ARCH"-*valkey-soak*.md 2>/dev/null); do
    grep -q "^- commit: $c" "$f" && { echo "$f"; return 0; }
  done
  return 1
}
# soak_pct FILE PATTERN: the "challoc vs jemalloc" column of the summary rows
# whose label matches PATTERN; with several matches the worst (largest,
# positive = challoc worse).
soak_pct() {
  awk -F'|' -v p="$2" '
    $2 ~ p && NF >= 5 { v=$(NF-1); gsub(/[ %+]/, "", v); if (v != "-" && v != "" && (!seen || v+0 > best+0)) { best=v; seen=1 } }
    END { if (!seen) print "-"; else printf "%+.1f%%", best }' "$1"
}
SOAK=$(soak_file || true)
if [ -n "$SOAK" ]; then
  s_write=$(soak_pct "$SOAK" '^ write: .* req/s')
  s_delete=$(soak_pct "$SOAK" '^ delete: .* req/s')
  s_peak=$(soak_pct "$SOAK" '^ peak RSS MiB')
  s_post=$(soak_pct "$SOAK" '^ RSS MiB after settle')
  soak_note="soak ${SOAK#"$ROOT"/}"
  grep -q 'CACHED BASELINE' "$SOAK" && soak_note="$soak_note (quick soak, cached jemalloc)"
else
  s_write=-; s_delete=-; s_peak=-; s_post=-; soak_note="no soak for $commit"
fi

# ---- the line --------------------------------------------------------------------
note="$ROUNDS rounds${BASELINES:+, jemalloc cached}${HOST_KIND:+, $HOST_KIND}; $soak_note"
line="| $DATE | $commit |"
for b in $COLS; do line="$line $(cell "$b") |"; done
line="$line $(ratio larson10) | $(ratio valkey1) | $s_write | $s_delete | $s_peak | $s_post | $note |"

# An existing file written under the previous column set is migrated in
# place: header and separator replaced, old lines padded (pad_old_lines).
if [ -f "$FILE" ] && ! grep -qxF -- "$HEADER" "$FILE"; then
  awk -v H="$HEADER" -v S="$SEP" '/^\| date \| commit \|/ {print H; next} /^\|---\|---\|/ {print S; next} {print}' "$FILE" | pad_old_lines > "$FILE.tmp" && mv "$FILE.tmp" "$FILE"
  echo "== migrated ${FILE#"$ROOT"/} to the current column set (python1, python1-malloc after redis; older lines read - there)"
fi
if [ ! -f "$FILE" ]; then
  {
    echo "# challoc scorecard, $ARCH"
    echo
    echo "One line per (date, commit): the fixed row set under challoc and jemalloc, 3 interleaved rounds (bench/scorecard.sh, \`make scorecard\`, \`challoc-bench scorecard <vm>\`), as signed percentages against jemalloc (positive = challoc worse), RSS ratios challoc/jemalloc for larson10 and valkey1, and the most recent Valkey soak for the commit (challoc vs jemalloc: the worse of the two generators' throughput rows per phase, peak RSS, RSS after settle). Reference numbers are the GCE VMs' (bench/gce/README.md); a line from another machine says so in its note. \`bench/scorecard.sh --render\` prints this table sorted by date."
    echo
    echo "$HEADER"
    echo "$SEP"
  } > "$FILE"
fi
prefix="| $DATE | $commit |"
if grep -qF -- "$prefix" "$FILE"; then
  # Replace the line, keeping the old cell wherever this run has none ("-"),
  # so a partial ROWS rerun refreshes its rows without blanking the rest.
  awk -F'|' -v p="$prefix" -v l="$line" '
    index($0, p) == 1 {
      n = split(l, new, "|")
      for (i = 1; i <= n; i++) { c = new[i]; gsub(/^ +| +$/, "", c); if (c == "-" && i <= NF) new[i] = $i }
      out = new[1]; for (i = 2; i <= n; i++) out = out "|" new[i]
      print out; next }
    {print}' "$FILE" > "$FILE.tmp" && mv "$FILE.tmp" "$FILE"
  line=$(grep -F -- "$prefix" "$FILE")
  echo "== replaced the $DATE $commit line in ${FILE#"$ROOT"/} (cells this run did not measure kept from the old line)"
else
  echo "$line" >> "$FILE"
  echo "== appended to ${FILE#"$ROOT"/}"
fi
echo "$HEADER"; echo "$SEP"; echo "$line"
echo "== $(( $(date +%s) - T_START )) s total; render the trend with: bench/scorecard.sh --render $ARCH"
