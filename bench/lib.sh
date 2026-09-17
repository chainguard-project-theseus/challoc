# bench/lib.sh: helpers shared by quick.sh, ab.sh and baseline.sh (the two
# benchmark tiers, bench/README.md). POSIX sh; source it after `ROOT` is set
# and the caller has `cd`ed to the repository root inside the dev container.
#
# What lives here: the protocol rows as command lines (the same programs and
# parameters bench/run.sh uses, with fixed thread counts so the rows are
# comparable across hosts), running one row under one library and scoring it,
# building a variant library from a git rev or the working tree into its own
# CARGO_TARGET_DIR, sorted-list/median arithmetic, the cached-baseline file
# name, and the bench lock every tier shares with run.sh.

BIN=$ROOT/target/bench-bin
PROCS=$(nproc)
ARCH=$(uname -m)
DATE=$(date -u +%F)
STAMP=$(date -u +%FT%TZ)
# The repository is bind-mounted and owned by another uid, so git needs
# safe.directory; the worktree case also needs the common .git mounted (the
# Makefile does that).
GIT="git -c safe.directory=*"
JEMALLOC_LIB=/usr/lib/libjemalloc.so.2

die() { echo "${0##*/}: $*" >&2; exit 1; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg; }
commit_short() { $GIT -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown; }
# tree_dirty: 0 if the working tree differs from HEAD in what the library is
# built from.
tree_dirty() { ! $GIT -C "$ROOT" diff --quiet HEAD -- src Cargo.toml Cargo.lock build.rs 2>/dev/null; }

# take_bench_lock: one protocol run per target volume (see bench/run.sh).
take_bench_lock() {
  exec 9>"$ROOT/target/bench.lock"
  flock -n 9 || die "another bench run holds target/bench.lock; wait for it or check docker ps"
}

# ensure_bench_programs: bench/build.sh is idempotent but slow to even check
# (cmake); skip it when the programs this tier needs are already there.
ensure_bench_programs() {
  for p in rt larson xmalloc-test alloc-test cfrac espresso mstress glibc-simple libmimalloc.so versions.txt; do
    [ -e "$BIN/$p" ] || { echo "== bench programs missing ($p): running bench/build.sh"; sh "$ROOT/bench/build.sh" >"$ROOT/target/bench-build.log" 2>&1 || { tail -20 "$ROOT/target/bench-build.log" >&2; die "bench/build.sh failed"; }; break; }
  done
}
# ensure_churn: the churn probe (bench/probes/churn.c) is built on demand.
ensure_churn() {
  [ "$BIN/churn" -nt "$ROOT/bench/probes/churn.c" ] || cc -O2 -Wall -o "$BIN/churn" "$ROOT/bench/probes/churn.c" || die "churn.c did not compile"
}

# ---- protocol rows ----------------------------------------------------------

stock_lib() { # stock_lib ALLOC: LD_PRELOAD path for jemalloc|glibc|mimalloc, 1 otherwise
  case $1 in
    jemalloc) echo "$JEMALLOC_LIB";;
    mimalloc) echo "$BIN/libmimalloc.so";;
    glibc)    echo "";;
    *) return 1;;
  esac
}
is_stock() { case $1 in jemalloc|glibc|mimalloc) return 0;; *) return 1;; esac; }

# bench_args BENCH: the command line of one row. larson/xmalloc rows are fixed
# duration (5 s) and scored by the throughput they print; the rest by wall time.
bench_args() {
  case $1 in
    larson1)      echo "$BIN/larson 5 8 1000 5000 100 4141 1";;
    larson10)     echo "$BIN/larson 5 8 1000 5000 100 4141 10";;
    larson16)     echo "$BIN/larson 5 8 1000 5000 100 4141 16";;
    larsonN)      echo "$BIN/larson 5 8 1000 5000 100 4141 $PROCS";;
    xmalloc)      echo "$BIN/xmalloc-test -w 10 -t 5 -s 64";;
    xmallocN)     echo "$BIN/xmalloc-test -w $PROCS -t 5 -s 64";;
    alloc-test1)  echo "$BIN/alloc-test 1";;
    alloc-testN)  n=$PROCS; [ "$n" -gt 16 ] && n=16; echo "$BIN/alloc-test $n";;
    cfrac)        echo "$BIN/cfrac 17545186520507317056371138836327483792789528";;
    espresso)     echo "$BIN/espresso $ROOT/target/bench-src/mimalloc-bench/bench/espresso/largest.espresso";;
    mstress)      echo "$BIN/mstress $PROCS 50 25";;
    glibc-simple) echo "$BIN/glibc-simple";;
    *) return 1;;
  esac
}
higher_is_better() { case $1 in larson*|xmalloc*) return 0;; *) return 1;; esac; }
unit_of() { case $1 in larson*) echo "M ops/s";; xmalloc*) echo "M frees/s";; *) echo "s";; esac; }

# run_row BENCH LIB CONF TAG: one run of BENCH with LD_PRELOAD=LIB and
# MALLOC_CONF=CONF (empty: unset). Appends "score maxrss_kb exit" to
# $OUT/TAG.scores; the program's output is $OUT/TAG.<n>.out. A failed run
# scores "-" so the sorted lists stay aligned.
run_row() {
  bench=$1; lib=$2; conf=$3; tag=$4
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  bench_args "$bench" >/dev/null || die "unknown bench $bench"
  set -- $(bench_args "$bench")
  if [ -n "$conf" ]; then
    RT_OUT=$rt "$BIN/rt" env MALLOC_CONF="$conf" LD_PRELOAD="$lib" "$@" > "$log" 2>&1 || true
  else
    RT_OUT=$rt "$BIN/rt" env LD_PRELOAD="$lib" "$@" > "$log" 2>&1 || true
  fi
  read -r el _us _sy rss ex < "$rt"
  case $bench in
    larson*)  s=$(awk '/Throughput/ {printf "%.1f", $3/1e6}' "$log");;
    xmalloc*) s=$(awk '/free\/sec/ {sub(",", "", $4); print $4}' "$log");;
    *)        s=$(awk -v e="$el" 'BEGIN {printf "%.3f", e}');;
  esac
  [ "$ex" = 0 ] || { s=-; echo "  $tag run $n failed (exit $ex), see $log" >&2; }
  echo "${s:--} $rss $ex" >> "$OUT/$tag.scores"
}

# run_churn LIB CONF TAG SECS CLASS: the churn probe (bench/probes/churn.c,
# Phase 1 acceptance 3 (c)). Writes "deleted_slabs end_slabs ops_per_s exit
# end_occupancy_pct" to $OUT/TAG.churn. The occupancy is what decides
# compaction: it does not depend on the class's slab size, while the slab
# count does (469 -> under 230 in the plan is 64 KiB slabs, i.e. about 90%).
run_churn() {
  lib=$1; conf=$2; tag=$3; secs=$4; class=$5
  log=$OUT/$tag.churn.out
  if [ -n "$conf" ]; then
    env MALLOC_CONF="$conf" LD_PRELOAD="$lib" "$BIN/churn" "$class" 30000 "$secs" 0.56 > "$log" 2>&1; ex=$?
  else
    env LD_PRELOAD="$lib" "$BIN/churn" "$class" 30000 "$secs" 0.56 > "$log" 2>&1; ex=$?
  fi
  awk -v ex="$ex" '
    $2=="deleted" && !d {d=$4}
    $2=="end" {e=$4; occ=$10; sub(/%/, "", occ)}
    /^ops/ {o=$3; gsub(/[^0-9.]/, "", o)}
    END {printf "%s %s %s %s %s\n", d?d:"-", e?e:"-", o?o:"-", ex, occ?occ:"-"}' "$log" > "$OUT/$tag.churn"
}

# ---- arithmetic on score files ---------------------------------------------

# sorted_scores FILE SKIP: column 1 of FILE from line SKIP+1 on, numerically
# sorted, space-separated ("-" entries dropped).
sorted_scores() { awk -v s="$2" 'NR>s && $1!="-" {print $1}' "$1" | sort -n | tr '\n' ' ' | sed 's/ $//'; }
# median_rss FILE SKIP: median of column 2 (maxrss KiB) from line SKIP+1 on.
median_rss() { awk -v s="$2" 'NR>s {print $2}' "$1" | sort -n | awk '{a[NR]=$1} END {if (NR) print a[int((NR+1)/2)]; else print "-"}'; }
# median_list "v1 v2 ...": median of a sorted list.
median_list() { echo "$1" | tr ' ' '\n' | awk 'NF {a[++n]=$1} END {if (n) print a[int((n+1)/2)]; else print "-"}'; }
# any_failed FILE SKIP: 1 if any run from line SKIP+1 exited non-zero.
any_failed() { awk -v s="$2" 'NR>s && $3!=0 {f=1} END {print f+0}' "$1"; }
# worse_pct BENCH NEW REF: signed percentage, positive when NEW did worse
# than REF on BENCH (run.sh's orientation); "-" when either is missing.
worse_pct() {
  if higher_is_better "$1"; then a=$3; b=$2; else a=$2; b=$3; fi
  awk -v a="$a" -v b="$b" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||b==0) print "-"; else printf "%+.1f%%", (a/b-1)*100}'
}
# pct_of A B: (A/B-1)*100 signed.
pct_of() { awk -v a="$1" -v b="$2" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||b==0) print "-"; else printf "%+.1f%%", (a/b-1)*100}'; }
# lists_overlap "list1" "list2": 0 when the ranges [min,max] intersect.
lists_overlap() {
  awk -v x="$1" -v y="$2" 'BEGIN {
    nx=split(x, a, " "); ny=split(y, b, " ")
    if (!nx || !ny) exit 0
    exit !(a[1] <= b[ny] && b[1] <= a[nx]) }'
}
kib_to_mib() { awk -v k="$1" 'BEGIN {if (k=="-"||k=="") print "-"; else printf "%.1f", k/1024}'; }

# ---- variant libraries ------------------------------------------------------

# resolve_rev REV: full hash of a git rev, or "tree" for the working tree.
resolve_rev() {
  case $1 in tree|.|WORKTREE) echo tree;; *) $GIT -C "$ROOT" rev-parse --verify "$1^{commit}" 2>/dev/null || die "cannot resolve rev '$1' (in a worktree the Makefile must mount the common .git)";; esac
}
# build_lib REV TARGETDIR: build libchalloc.so from REV ("tree" = this
# working tree, otherwise `git archive REV` into TARGETDIR/src-tree) with
# CARGO_TARGET_DIR=TARGETDIR. Paths are remapped so identical sources give an
# identical .so and the callers can tell "no code change" from "stale build".
# Fails unless cargo succeeded and the .so exists. Prints nothing on success.
build_lib() {
  rev=$1; tdir=$2
  mkdir -p "$tdir"
  if [ "$rev" = tree ]; then
    srcdir=$ROOT
    # Restored or rsynced files may carry old mtimes and cargo would skip the
    # rebuild; touching the crate root forces it (~seconds).
    touch "$ROOT/src/lib.rs"
  else
    srcdir=$tdir/src-tree
    rm -rf "$srcdir"; mkdir -p "$srcdir"
    $GIT -C "$ROOT" archive "$rev" | tar -x -C "$srcdir" || die "git archive $rev failed"
    [ -f "$srcdir/Cargo.toml" ] || die "git archive $rev produced no tree"
    # git archive stamps every file with the commit time, which is older
    # than the previous build in this target dir: cargo would call the old
    # library fresh. Touch the tree so it always recompiles.
    find "$srcdir" -type f -exec touch {} +
  fi
  if ! (cd "$srcdir" && RUSTFLAGS="--remap-path-prefix=$srcdir=/challoc" CARGO_TARGET_DIR=$tdir cargo build --release > "$tdir/build.log" 2>&1); then
    grep -E '^(error|warning)' -A5 "$tdir/build.log" | head -40 >&2
    die "build of $rev into $tdir failed (see $tdir/build.log)"
  fi
  # "compiled" means cargo actually ran rustc on the crate this time; a
  # "Finished" without "Compiling challoc" is a stale reuse.
  grep -q 'Compiling challoc' "$tdir/build.log" || die "cargo reused a previous build in $tdir instead of compiling $rev (stale mtimes?); see $tdir/build.log"
  [ -f "$tdir/release/libchalloc.so" ] || die "build of $rev produced no $tdir/release/libchalloc.so"
}
so_hash() { sha256sum "$1" | cut -c1-16; }

# ---- cached baselines --------------------------------------------------------

BASELINE_DIR=$ROOT/target/baselines
# bench_build_hash: identifies the benchmark build this VM runs (program
# versions, kernel, CPU count); a cached baseline is only valid for the same.
bench_build_hash() { { cat "$BIN/versions.txt" 2>/dev/null; uname -r; nproc; } | sha256sum | cut -c1-12; }
baseline_file() { echo "$BASELINE_DIR/$ARCH-$(bench_build_hash).md"; }
# baseline_row FILE BENCH VARIANT: "sorted scores|maxrss" from a baseline (or
# any ab.sh results) file, empty if absent.
baseline_row() { awk -F'|' -v b="$2" -v v="$3" '{gsub(/^ +| +$/, "", $2); gsub(/^ +| +$/, "", $3); gsub(/^ +| +$/, "", $4); gsub(/^ +| +$/, "", $5)} $2==b && $3==v {print $4 "|" $5; exit}' "$1"; }
baseline_date() { awk -F': ' '/^- date: /{print $2; exit}' "$1"; }
