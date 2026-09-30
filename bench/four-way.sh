#!/bin/sh
# bench/four-way.sh: assemble the comparison matrix of docs/plan.md Phase 2.5
# task 1 from one VM's same-run outputs. Pure sh + awk, runs on the host or
# in the container; it reads, never measures.
#
#   AB=target/results/<date>-<arch>-gce-four-way-ab.md     bench/ab.sh (Phase 0 set + Valkey model
#                                                            rows, VARIANTS='challoc=... jemalloc glibc
#                                                            mimalloc tcmalloc')
#   RUBY=target/results/<date>-<arch>-ruby.md               bench/ruby-bench.sh
#   PYTHON=target/results/<date>-<arch>-python.md           bench/python-bench.sh
#   CGROUP="logs..."                                        make test-cgroup and make
#                                                            test-cgroup-compare output (OUTCOME
#                                                            and RESULT lines)
#   RESULT=target/results/<date>-<arch>-gce-four-way.md     the file written
#   ALLOCS="jemalloc glibc mimalloc tcmalloc"               the columns challoc is compared with
#   NOTE="..."                                              extra header bullet(s)
#
# One table: every row's score under each allocator with its peak RSS in
# parentheses, then challoc against each of the other allocators as
# "time / RSS" signed percentages, positive = challoc worse (slower, lower
# throughput, more RSS). A "Rows more than 10% behind" list follows for the
# attribution the plan asks for; the attribution text is written by hand
# under it.
set -eu
AB=${AB:?}; RESULT=${RESULT:?}
RUBY=${RUBY:-}; PYTHON=${PYTHON:-}; CGROUP=${CGROUP:-}
ALLOCS=${ALLOCS:-"jemalloc glibc mimalloc tcmalloc"}
NOTE=${NOTE:-}
TMP=${TMPDIR:-/tmp}/four-way.$$; mkdir -p "$TMP"; trap 'rm -rf "$TMP"' EXIT

# cells: "group|row|unit|hib|alloc|score|rss_mib" lines (hib: 1 = higher is better)
CELLS=$TMP/cells; : > "$CELLS"

# ---- ab.sh: Sorted scores (median of the list) + the unit column of Medians ----
awk -F'|' -v out="$CELLS" '
  function trim(s) { gsub(/^ +| +$/, "", s); return s }
  /^## Sorted scores/ {sec="s"; next} /^## Medians/ {sec="m"; next}
  sec=="s" && /^\| / && $2 !~ /program|---/ {
    p=trim($2); v=trim($3); l=trim($4); r=trim($5); sub(/ \(FAILED\)/, "", l)
    n=split(l, a, " "); med=(n ? a[int((n+1)/2)] : "-")
    score[p, v]=med; rss[p, v]=(r=="-" ? "-" : sprintf("%.1f", r/1024)); if (!(p in seen)) {order[++np]=p; seen[p]=1}; vars[v]=1 }
  sec=="m" && /^\| / && $2 !~ /program|---/ { unit[trim($2)]=trim($3) }
  END {
    for (i=1; i<=np; i++) { p=order[i]; hib=(p ~ /^(larson|xmalloc|valkey|redis)/) ? 1 : 0
      g=(p ~ /^valkey/) ? "Valkey model" : "Phase 0 set"
      for (v in vars) printf "%s|%s|%s|%d|%s|%s|%s\n", g, p, unit[p], hib, v, score[p, v], rss[p, v] >> out } }' "$AB"

# ---- ruby-bench.sh / python-bench.sh: the Medians table ("<alloc> s | <alloc> RSS MiB") ----
medians_table() { # medians_table FILE GROUP PREFIX
  awk -F'|' -v out="$CELLS" -v g="$2" -v pre="$3" '
    function trim(s) { gsub(/^ +| +$/, "", s); return s }
    /^## Medians/ {sec=1; next} sec && /^## / {sec=0}
    sec && /^\| row \|/ { for (i=3; i<=NF; i++) { h=trim($i); if (h ~ / s$/) { sub(/ s$/, "", h); scol[h]=i; rcol[h]=i+1 } } next }
    sec && /^\| / && $2 !~ /---/ { row=trim($2); for (a in scol) printf "%s|%s%s|s|0|%s|%s|%s\n", g, pre, row, a, trim($scol[a]), trim($rcol[a]) >> out }' "$1"
}
[ -n "$RUBY" ] && medians_table "$RUBY" "Ruby 3.4" "ruby "
[ -n "$PYTHON" ] && medians_table "$PYTHON" "Python 3.13" "python "

# ---- container tests: RESULT + OUTCOME lines ----
# churn rows: RSS 2 s after freeing (MiB kept; lower is better); oscillate: verdict and cycles.
if [ -n "$CGROUP" ]; then
  cat $CGROUP | awk -v out="$CELLS" '
    /^== (churn|oscillate) under / { alloc=$4; sub(/^\(/, "", $5) }
    /^RESULT churn / { for (i=3; i<=NF; i++) { split($i, kv, "="); r[kv[1]]=kv[2] } }
    /^RESULT oscillate / { for (i=3; i<=NF; i++) { split($i, kv, "="); o[kv[1]]=kv[2] } }
    /^OUTCOME churn / {
      split($3, a, "="); al=a[2]; if (al ~ /-off$/) next
      match($0, /args=.[^\x27]*/); split(substr($0, RSTART+6, RLENGTH-6), ar, " "); mib=ar[2]
      lim=(mib==400 ? "512m" : "256m")
      v=$0; sub(/.*verdict=/, "", v); sub(/ .*/, "", v)
      printf "container|churn %s: RSS 2 s after freeing %s MiB|MiB kept|0|%s|%s|%s\n", lim, mib, al, (v=="ok" ? r["rss_2s"] : "FAILED"), r["rss_peak"] >> out; delete r }
    /^OUTCOME oscillate / {
      split($3, a, "="); al=a[2]; if (al ~ /-off$/) next
      v=$0; sub(/.*verdict=/, "", v); sub(/ .*/, "", v)
      cyc=("cycles" in o) ? ", " o["cycles"] " cycles" : ""
      printf "container|oscillate 512m: 200..480 MiB live, 300 s|verdict|0|%s|%s|%s\n", al, v cyc, ("rss_peak" in o ? o["rss_peak"] : "") >> out; delete o }' 
fi

# ---- the table ----
pct() { awk -v a="$1" -v b="$2" -v hib="$3" 'BEGIN { if (a=="-"||b=="-"||a==""||b==""||a~/[^0-9.]/||b~/[^0-9.]/||b==0||a==0) {print "-"; exit}; if (hib) printf "%+.1f%%", (b/a-1)*100; else printf "%+.1f%%", (a/b-1)*100 }'; }
cell() { awk -F'|' -v g="$1" -v r="$2" -v a="$3" -v f="$4" '$1==g && $2==r && $5==a {print $f; exit}' "$CELLS"; }
hdr="| group | row | unit | challoc |"; sep="|---|---|---|---:|"
for a in $ALLOCS; do hdr="$hdr $a |"; sep="$sep---:|"; done
for a in $ALLOCS; do hdr="$hdr vs $a (time / RSS) |"; sep="$sep---:|"; done
BEHIND=$TMP/behind; : > "$BEHIND"
{
  echo "$hdr"; echo "$sep"
  awk -F'|' '!seen[$1 SUBSEP $2]++ {print $1 "|" $2 "|" $3 "|" $4}' "$CELLS" | while IFS='|' read -r g r u hib; do
    cs=$(cell "$g" "$r" challoc 6); cr=$(cell "$g" "$r" challoc 7)
    line="| $g | $r | $u | ${cs:--}${cr:+ ($cr MiB)} |"; deltas=
    for a in $ALLOCS; do
      s=$(cell "$g" "$r" "$a" 6); rr=$(cell "$g" "$r" "$a" 7)
      line="$line ${s:--}${rr:+ ($rr MiB)} |"
      dt=$(pct "$cs" "$s" "$hib"); dr=$(pct "$cr" "$rr" 0)
      deltas="$deltas $dt / $dr |"
      for k in "time $dt" "RSS $dr"; do
        v=${k#* }; case $v in +*) awk -v v="${v%\%}" 'BEGIN {exit !(v+0 > 10)}' && echo "- $g / $r: challoc ${k%% *} $v against $a" >> "$BEHIND";; esac
      done
    done
    echo "$line$deltas"
  done
} > "$TMP/table"

# ---- header material from the ab file ----
h() { awk -v k="$1" 'index($0, "- " k ":") == 1 {sub(/^- [^:]*: /, ""); print; exit}' "$AB"; }
ARCH=$(head -1 "$AB" | sed 's/.*, //')
{
  echo "# challoc comparison matrix (four-way), $(h date | cut -c1-10), $ARCH"
  echo
  echo "- commit: $(h commit)"
  echo "- date: $(h date)"
  echo "- host: $(h host)"
  echo "- kernel: $(h kernel)"
  awk '/^- (jemalloc-[0-9]|tcmalloc-minimal-[0-9]|python-3\.[0-9]|mimalloc-bench |mimalloc v|rust-[0-9])/ {print}' "$AB"
  echo "- allocators (same binaries, LD_PRELOAD only; challoc from the commit above):"
  awk '/^- variants:/ {p=1; next} p && /^  - / {print} p && !/^  - / {exit}' "$AB"
  [ -n "$RUBY" ] && echo "- Ruby rows: $(awk 'index($0, "- ruby:") == 1 {sub(/^- ruby: /, ""); print; exit}' "$RUBY")"
  [ -n "$PYTHON" ] && echo "- Python rows: $(awk 'index($0, "- python:") == 1 {sub(/^- python: /, ""); print; exit}' "$PYTHON")"
  [ -n "$NOTE" ] && echo "- $NOTE"
  echo "- method: every cell comes from one run per VM on an idle VM (load under 1 at the start): the Phase 0 set and the Valkey model rows from one \`bench/ab.sh\` run (5 timed rounds after a warm-up, variants interleaved per round, median of the sorted scores, median peak RSS from bench/rt); the Ruby rows from one \`bench/ruby-bench.sh\` run and the Python rows from one \`bench/python-bench.sh\` run (same protocol, wall seconds); the container rows from \`make test-cgroup\` (challoc) and \`make test-cgroup-compare CGROUP_ALLOCS='jemalloc mimalloc tcmalloc'\` (one memory-limited container per run, RSS 2 s after freeing the churn, and the 300 s oscillation's verdict). Positive percentages mean challoc did worse than that allocator (slower, lower throughput, or more peak RSS); time is the score's direction (throughput rows compare throughput). The Valkey soak (\`make valkey-soak\`) is jemalloc-only: it drives Valkey's own \`je_\` allocator API (\`MEMORY MALLOC-STATS\`, active defrag, \`jemalloc_purge\`), which no other allocator has, so it is not a row here; its latest comparison is in the scorecard's soak columns."
  echo "- sources: \`${AB##*/}\`${RUBY:+, \`${RUBY##*/}\`}${PYTHON:+, \`${PYTHON##*/}\`}${CGROUP:+, the container logs (OUTCOME lines quoted in section 3)}"
  echo
  echo "## 1. The table"
  echo
  cat "$TMP/table"
  echo
  echo "Units: larsonN and valkey rows M ops/s, xmallocN M frees/s, redis k req/s (higher is better); the other Phase 0, Ruby and Python rows wall seconds (lower is better); container churn rows MiB of RSS kept 2 s after the frees (lower is better), the oscillation row its verdict and cycle count. RSS in parentheses is bench/rt's peak (MiB) of the measured process (the churn rows: the program's own RSS peak)."
  echo
  echo "## 2. Rows where challoc is more than 10% behind an allocator"
  echo
  if [ -s "$BEHIND" ]; then cat "$BEHIND"; else echo "(none)"; fi
  echo
  echo "Attribution (plan Phase 2.5 task 1 acceptance: every row above has one before anything is changed for it):"
  echo
  echo "(to be written from the VM's quick-perf / perf / smaps runs)"
  echo
  if [ -n "$CGROUP" ]; then
    echo "## 3. Container tests, OUTCOME lines"
    echo
    echo '```'
    cat $CGROUP | grep -E '^(OUTCOME|RESULT) ' | grep -v 'alloc=challoc-off' || true
    echo '```'
    echo
  fi
} > "$RESULT"
echo "== wrote $RESULT"
cat "$TMP/table"
echo
echo "behind by more than 10%:"; cat "$BEHIND"
