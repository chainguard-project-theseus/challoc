#!/bin/sh
# Ruby soak (docs/plan.md Phase 3 prerequisite (c)): the contrib/ruby/build.sh
# Ruby (`--with-jemalloc` against the compat libjemalloc.so.2) running
# bench/ruby/soak.rb, a Sidekiq-shaped process (16 mostly idle threads,
# bursts of short-lived strings and arrays, a slowly growing long-lived set,
# a full GC every 30 s, GC.compact once at minute 5) for DURATION seconds
# per allocator, RSS and allocator stats sampled every INTERVAL seconds.
# Every Ruby row of bench/ruby-bench.sh ends within seconds, before
# challoc's dirty_decay_ms (10 s) can fire, and the --with-jemalloc Ruby
# never calls malloc_trim, so this is where long-lived residency under
# each allocator is measured. Runs inside the dev container: `make
# ruby-soak`, or on a VM detached (`setsid nohup make ruby-soak ...`). Needs
# `make ruby STAGE=build` (the challoc build only; the same binary serves
# every allocator here).
#
# Allocators (ALLOCS), same binary and seed for all:
#   challoc      DT_NEEDED libjemalloc.so.2 -> the working tree's stage (quantum16)
#   jemalloc     LD_PRELOAD=/usr/lib/libjemalloc.so.2 (Wolfi's jemalloc; same
#                soname, so the stage is never mapped)
#   challoc-bg   challoc with MALLOC_CONF=background_thread:true (the decision
#                the plan asks for: does Ruby's shape need the background thread)
#   jemalloc-bg  jemalloc with MALLOC_CONF=background_thread:true (optional)
#   challoc-1arena  challoc with MALLOC_CONF=narenas:1 (optional diagnostic: the
#                default gives every thread its own arena, so 16 workers build 16
#                arenas whose slabs are never shared)
# Every variant's /proc/self/maps is checked before its run.
#
#   DURATION=600  INTERVAL=5  GC_EVERY=30  COMPACT_AT=300  THREADS=16  SEED=4141
#   RESULT=path   markdown output (default bench/results/<date>-<arch>-ruby-soak.md);
#                 the per-allocator CSVs land beside it as <result stem>-<alloc>.csv
#                 and the allocator's malloc_stats_print as <result stem>-<alloc>-mstats.txt
#   OUT=dir       raw logs (default target/ruby/soak-out)
#   COMMIT=hash   the commit named in the header (default: git, "unknown" where the
#                 synced tree has no usable .git, as on a VM)
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc challoc-bg"}
DURATION=${DURATION:-600}
INTERVAL=${INTERVAL:-5}
GC_EVERY=${GC_EVERY:-30}
COMPACT_AT=${COMPACT_AT:-300}
THREADS=${THREADS:-16}
SEED=${SEED:-4141}
WORK=$ROOT/target/ruby
OUT=${OUT:-$WORK/soak-out}
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH-ruby-soak.md}
STEM=${RESULT%.md}
RUBY_TAG=${RUBY_TAG:-v3_4_10}
COMPAT=$WORK/jemalloc-compat
STAGE_LIB=$COMPAT/stage/lib
INST=$WORK/install-challoc
COMMIT=${COMMIT:-$(commit_short)$(tree_dirty && echo ' (uncommitted changes in src/)')}
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
for a in $ALLOCS; do case $a in challoc|jemalloc|challoc-bg|jemalloc-bg|challoc-1arena) ;; *) die "unknown allocator '$a'";; esac; done
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- build, stage, install -------------------------------------------------------
[ -x "$WORK/build-challoc/ruby" ] || die "no Ruby build at target/ruby/build-challoc: run 'make ruby STAGE=build' first"
echo "== staging challoc as libjemalloc.so.2 from the working tree ($COMMIT)"
touch "$ROOT/src/lib.rs"
TARGET_DIR=$COMPAT QUANTUM=16 sh "$ROOT/scripts/jemalloc-compat.sh" > "$OUT/stage.log" 2>&1 || { tail -20 "$OUT/stage.log" >&2; die "jemalloc-compat stage failed"; }
STAGE_HASH=$(so_hash "$STAGE_LIB/libjemalloc.so.2")
if [ ! -x "$INST/bin/ruby" ] || [ "$WORK/build-challoc/ruby" -nt "$INST/bin/ruby" ]; then
  printf '== installing the challoc build into %s ... ' "${INST#"$ROOT"/}"
  make -C "$WORK/build-challoc" install > "$OUT/install.log" 2>&1 || { tail -20 "$OUT/install.log" >&2; die "make install failed"; }
  echo done
fi

# ---- per-allocator command lines ----------------------------------------------------
# ruby_env ALLOC: the `env` assignments in front of the ruby binary.
ruby_env() {
  case $1 in
    challoc)     echo "LD_LIBRARY_PATH=$INST/lib";;
    jemalloc)    echo "LD_PRELOAD=$JEMALLOC_LIB LD_LIBRARY_PATH=$INST/lib";;
    challoc-bg)  echo "MALLOC_CONF=background_thread:true LD_LIBRARY_PATH=$INST/lib";;
    jemalloc-bg) echo "LD_PRELOAD=$JEMALLOC_LIB MALLOC_CONF=background_thread:true LD_LIBRARY_PATH=$INST/lib";;
    challoc-1arena) echo "MALLOC_CONF=narenas:1 LD_LIBRARY_PATH=$INST/lib";;
  esac
}
run_ruby() { a=$1; shift; env $(ruby_env "$a") "$INST/bin/ruby" "$@"; }

echo "== ruby: $(run_ruby challoc -v)"
for a in $ALLOCS; do
  mapped=$(run_ruby "$a" -e 'puts File.readlines("/proc/self/maps").grep(%r{/libjemalloc[^/ ]*\.so}).map { |l| l.split[5] }.uniq.sort.join(" ")')
  case $a in challoc*) want=$STAGE_LIB/libjemalloc.so.2;; jemalloc*) want=$JEMALLOC_LIB;; esac
  [ "$mapped" = "$want" ] || die "$a: ruby maps '$mapped', expected '$want'"
  echo "   $a: maps $mapped"
done
fiddle=$(run_ruby challoc -e 'begin; require "fiddle"; puts "yes"; rescue LoadError; puts "no"; end')
echo "   fiddle: $fiddle (allocator stats through mallctl$([ "$fiddle" = yes ] || echo ' skipped'))"

# ---- the runs --------------------------------------------------------------------------
echo "== $DURATION s per allocator ($ALLOCS), $THREADS threads, sample every $INTERVAL s, full GC every $GC_EVERY s, GC.compact at $COMPACT_AT s, seed $SEED; load $LOAD0"
for a in $ALLOCS; do
  csv=$STEM-$a.csv
  printf '   %s: started %s ... ' "$a" "$(date -u +%T)"
  if run_ruby "$a" "$ROOT/bench/ruby/soak.rb" "$DURATION" "$csv" "$SEED" "$THREADS" "$INTERVAL" "$GC_EVERY" "$COMPACT_AT" > "$OUT/$a.out" 2> "$OUT/$a.err"; then
    echo "done $(date -u +%T), load $(loadavg)"
  else
    echo "FAILED (exit $?), see $OUT/$a.err"; tail -5 "$OUT/$a.err" >&2
  fi
  tail -1 "$OUT/$a.out"
  # The allocator's report, when the run printed one (fiddle + malloc_stats_print).
  if grep -q '^=== malloc_stats_print' "$OUT/$a.err"; then
    sed -n '/^=== malloc_stats_print/,$p' "$OUT/$a.err" > "$STEM-$a-mstats.txt"
  fi
done

# ---- report ------------------------------------------------------------------------------
kv() { awk -v k="$2" '{for (i = 1; i <= NF; i++) {split($i, p, "="); if (p[1] == k) print p[2]}}' "$OUT/$1.out" | tail -1; }
mib() { awk -v b="$1" 'BEGIN {if (b=="-"||b=="") print "-"; else printf "%.1f", b/1048576}'; }
# col ALLOC WHAT: one figure from the CSV, in MiB.
#   peak_rss      max vmrss over the samples        hwm      final VmHWM
#   at_T          vmrss of the last row with t<=T   final    the final-fullgc row's vmrss
#   min_after_gc  min vmrss over fullgc rows        compact  vmrss of the compact row
#   alloc_<k>_final / alloc_<k>_at_T  the allocator column at the end / at T
col() {
  awk -F, -v what="$2" '
    NR==1 { for (i=1;i<=NF;i++) c[$i]=i; next }
    { t=$c["t_s"]; rss=$c["vmrss_kb"]
      if (rss>peak) peak=rss
      if ($c["event"]=="fullgc" && (mingc==0 || rss<mingc)) mingc=rss
      if ($c["event"]=="compact") compact=rss
      last=$0; lastrss=rss; hwm=$c["vmhwm_kb"]
      if (what ~ /(^|_)at_[0-9]+$/) { T=what; sub(/.*at_/, "", T); if (t<=T+0) { at=rss; atrow=$0 } }
    }
    END {
      if (what=="peak_rss") printf "%.1f", peak/1024
      else if (what=="hwm") printf "%.1f", hwm/1024
      else if (what=="final") printf "%.1f", lastrss/1024
      else if (what=="min_after_gc") printf "%.1f", mingc/1024
      else if (what=="compact") { if (compact) printf "%.1f", compact/1024; else print "-" }
      else if (what ~ /^alloc_/) {
        k=what; sub(/_final$/, "", k); sub(/_at_[0-9]+$/, "", k)
        row = (what ~ /_final$/) ? last : atrow
        n=split(row, f, ","); v=f[c[k]]
        if (v=="-"||v=="") print "-"; else printf "%.1f", v/1048576
      }
      else if (what ~ /^at_/) printf "%.1f", at/1024
    }' "$STEM-$1.csv"
}
describe() {
  case $1 in
    challoc)     echo "the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to $STAGE_LIB/libjemalloc.so.2 (challoc, quantum16, sha256 $STAGE_HASH), default MALLOC_CONF";;
    jemalloc)    echo "the same ruby with LD_PRELOAD=$JEMALLOC_LIB (Wolfi jemalloc $(apk_version jemalloc)); the stage is not mapped";;
    challoc-bg)  echo "as challoc, with MALLOC_CONF=background_thread:true";;
    jemalloc-bg) echo "as jemalloc, with MALLOC_CONF=background_thread:true";;
    challoc-1arena) echo "as challoc, with MALLOC_CONF=narenas:1 (one arena for all threads)";;
  esac
}
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
M5=$COMPACT_AT; M10=$DURATION
{
  echo "# Ruby soak, $DATE, $ARCH"
  echo
  echo "- commit: $COMMIT"
  echo "- date: $STAMP"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container${HOST_KIND:+; $HOST_KIND}"
  echo "- kernel: $(uname -r), container: Wolfi"
  echo "- ruby: $(run_ruby challoc -v) (tag $RUBY_TAG built by contrib/ruby/build.sh, --with-jemalloc), installed under target/ruby/install-challoc; allocator stats through fiddle -> mallctl: $fiddle"
  echo "- workload: bench/ruby/soak.rb $DURATION s, $THREADS threads, seed $SEED, sample every $INTERVAL s, full GC every $GC_EVERY s, GC.compact at $COMPACT_AT s, one full GC after the workers stop (the final row)"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- allocators (run in this order):"
  for a in $ALLOCS; do echo "  - $a: $(describe "$a")"; done
  echo
  echo "## RSS (MiB, VmRSS from /proc/self/status; VmHWM is the kernel's high-water mark)"
  echo
  hdr="| measure |"; sep="|---|"
  for a in $ALLOCS; do hdr="$hdr $a |"; sep="$sep---:|"; done
  if has challoc && has jemalloc; then hdr="$hdr challoc vs jemalloc |"; sep="$sep---:|"; fi
  if has challoc && has challoc-bg; then hdr="$hdr challoc-bg vs challoc |"; sep="$sep---:|"; fi
  if has challoc && has challoc-1arena; then hdr="$hdr challoc-1arena vs challoc |"; sep="$sep---:|"; fi
  echo "$hdr"; echo "$sep"
  rowc() { # label what
    r="| $1 |"
    for a in $ALLOCS; do r="$r $(col "$a" "$2") |"; done
    if has challoc && has jemalloc; then r="$r $(pct_of "$(col challoc "$2")" "$(col jemalloc "$2")") |"; fi
    if has challoc && has challoc-bg; then r="$r $(pct_of "$(col challoc-bg "$2")" "$(col challoc "$2")") |"; fi
    if has challoc && has challoc-1arena; then r="$r $(pct_of "$(col challoc-1arena "$2")" "$(col challoc "$2")") |"; fi
    echo "$r"
  }
  rowc "peak sampled VmRSS" peak_rss
  rowc "VmHWM at the end" hwm
  rowc "at minute 1 (t=60)" at_60
  rowc "at minute 5, before GC.compact (t=$((M5 - INTERVAL)))" at_$((M5 - INTERVAL))
  rowc "at minute 5, after GC.compact (t=$M5)" compact
  rowc "at minute 10 (t=$M10)" at_$M10
  rowc "after the last full GC (final)" final
  rowc "lowest right after a periodic full GC" min_after_gc
  echo
  echo "Positive percentages: the first-named allocator is larger."
  echo
  echo "## Allocator stats (MiB, mallctl stats.* after an epoch bump)"
  echo
  echo "| allocator | allocated final | active final | resident final | retained final | mapped final | resident at t=$M5 | resident at t=$M10 | resident / VmRSS final |"
  echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|"
  for a in $ALLOCS; do
    res=$(col "$a" alloc_resident_final); fin=$(col "$a" final)
    ratio=$(awk -v r="$res" -v f="$fin" 'BEGIN {if (r=="-"||f==0) print "-"; else printf "%.2f", r/f}')
    echo "| $a | $(col "$a" alloc_allocated_final) | $(col "$a" alloc_active_final) | $res | $(col "$a" alloc_retained_final) | $(col "$a" alloc_mapped_final) | $(col "$a" alloc_resident_at_$M5) | $(col "$a" alloc_resident_at_$M10) | $ratio |"
  done
  echo
  echo "## Work done and GC"
  echo
  echo "| allocator | jobs | live objects | GC count | major | minor | heap pages final |"
  echo "|---|---:|---:|---:|---:|---:|---:|"
  for a in $ALLOCS; do
    echo "| $a | $(kv "$a" jobs) | $(kv "$a" live_objects) | $(kv "$a" gc_count) | $(kv "$a" major) | $(kv "$a" minor) | $(kv "$a" heap_pages) |"
  done
  echo
  echo "The run is time-bounded, so the job counts differ slightly between allocators; the long-lived set grows with the jobs (one string and one array per four jobs per thread)."
  echo
  echo "## RSS over time (MiB; every ${GC_EVERY}th second is a forced full GC, the row at t=$COMPACT_AT is GC.compact)"
  echo
  hdr="| t s | event |"; sep="|---:|---|"
  for a in $ALLOCS; do hdr="$hdr $a VmRSS | $a resident | $a retained |"; sep="$sep---:|---:|---:|"; done
  echo "$hdr"; echo "$sep"
  first=$(echo $ALLOCS | cut -d' ' -f1)
  awk -F, 'NR>1 && ($1 % 30 == 0 || $2=="compact" || $2=="final-fullgc") {print $1 "," $2}' "$STEM-$first.csv" | while IFS=, read -r t ev; do
    r="| $t | $ev |"
    for a in $ALLOCS; do
      r="$r $(awk -F, -v t="$t" -v ev="$ev" 'NR==1 {for (i=1;i<=NF;i++) c[$i]=i; next} ($1==t && (ev!="final-fullgc" || $2==ev)) {rss=$c["vmrss_kb"]; res=$c["alloc_resident"]; ret=$c["alloc_retained"]} END {printf "%.1f | %s | %s", rss/1024, (res=="-"||res=="")?"-":sprintf("%.1f", res/1048576), (ret=="-"||ret=="")?"-":sprintf("%.1f", ret/1048576)}' "$STEM-$a.csv") |"
    done
    echo "$r"
  done
  echo
  echo "## Samples"
  echo
  for a in $ALLOCS; do echo "- $a: \`${STEM#"$ROOT"/}-$a.csv\` ($(($(wc -l < "$STEM-$a.csv") - 1)) rows)$([ -f "$STEM-$a-mstats.txt" ] && echo ", allocator report \`${STEM#"$ROOT"/}-$a-mstats.txt\`"); summary line: \`$(tail -1 "$OUT/$a.out")\`"; done
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## RSS (MiB/,/^## RSS over time/p' "$RESULT"
