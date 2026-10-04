# bench/drivers/lib.sh: what the long drivers (valkey-soak.sh,
# ruby-soak.sh, jvm-bench.sh) share. POSIX sh; source it after `ROOT` is set
# and the caller has `cd`ed to the repository root inside the bench or dev
# container. The small drivers use bench/drivers/cb.sh instead.
#
# What lives here: building a challoc library from a git rev or the
# working tree into its own CARGO_TARGET_DIR (a driver run by hand, outside
# cb), the stock allocators' paths and versions, sorted-list/median and
# percentage arithmetic for the drivers' markdown summaries, the cgroup
# memory.peak sidecar, the bench lock, and the results-header and
# /proc/<pid>/maps helpers.
#
# POSIX sh has no `local`: every function here writes plain globals, and
# some callers rely on them: record_rt sets n el rss ex; check_score_lines
# row a n; maps_expect want; ensure_probe src; build_lib rev tdir srcdir;
# resolve_rev nothing. A driver that keeps its own n, row or a across one of
# these calls must expect it changed.

# Under cb the bench programs are in CB_BIN (bench/drivers/cb.sh).
BIN=${CB_BIN:-$ROOT/target/bench-bin}
PROCS=$(nproc)
ARCH=$(uname -m)
DATE=$(date -u +%F)
STAMP=$(date -u +%FT%TZ)
# The repository is bind-mounted and owned by another uid, so git needs
# safe.directory; the worktree case also needs the common .git mounted (the
# Makefile does that).
GIT="git -c safe.directory=*"
JEMALLOC_LIB=/usr/lib/libjemalloc.so.2
# Wolfi's tcmalloc-minimal (gperftools, no profiler hooks): the fourth stock
# allocator.
TCMALLOC_LIB=/usr/lib/libtcmalloc_minimal.so.4

die() { echo "${0##*/}: $*" >&2; exit 1; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg; }
commit_short() { $GIT -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown; }
# tree_dirty: 0 if the working tree differs from HEAD in what the library is
# built from. Only git's answer counts: `git diff --quiet` exits 1 for a
# difference and 128 when it cannot answer (a .git without HEAD's commit
# object); a git that cannot answer stops the run: the label would be a
# guess. Under cb (CB_OUT set) the tree is a `git archive` with no
# repository: the variant's commit is the label, and nothing is dirty.
tree_dirty() {
  [ -z "${CB_OUT:-}" ] || return 1
  td_err=$($GIT -C "$ROOT" diff --quiet HEAD -- src Cargo.toml Cargo.lock build.rs 2>&1); td_rc=$?
  case $td_rc in
    0) return 1;;
    1) return 0;;
    *) die "tree_dirty: 'git diff HEAD' failed ($td_rc): $td_err (is .git complete?)";;
  esac
}

# take_bench_lock: one protocol run per target volume.
# The lock is the flock on fd 9, released by the kernel when every holder of
# the descriptor exits. A server the harness starts in the background
# inherits fd 9 and keeps the lock for as long as it lives, so a harness
# that dies with its server up blocks the next harness in the container.
# Two defences: every server start closes the descriptor for
# the child (`... 9>&- &`, so the child never holds the lock), and the trap
# installed here kills the children the harness registered in BENCH_PIDS
# (space-separated; each one's own children first, `rt` and `env` wrappers
# forward no signal) and runs the harness's own cleanup, named in
# BENCH_CLEANUP (a function; set it instead of trapping EXIT yourself, a
# second `trap ... EXIT` would replace this one). INT and TERM turn into an
# exit so the EXIT trap runs once. The lock file itself stays: unlinking it
# would let a waiter that had already opened the old inode and a newcomer
# on the new one both take "the" lock.
BENCH_PIDS=
BENCH_CLEANUP=
take_bench_lock() {
  exec 9>"$ROOT/target/bench.lock"
  flock -n 9 || die "another bench run holds target/bench.lock; wait for it or check docker ps"
  trap bench_exit EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
}
bench_exit() {
  be_st=$?
  trap - EXIT INT TERM
  [ -z "$BENCH_CLEANUP" ] || $BENCH_CLEANUP
  for be_p in $BENCH_PIDS; do pkill -P "$be_p" 2>/dev/null || true; kill "$be_p" 2>/dev/null || true; done
  exec 9>&-
  exit $be_st
}

# ensure_bench_programs: bench/build.sh is idempotent but slow to even check
# (cmake); skip it when the programs this tier needs are already there.
ensure_bench_programs() {
  for p in rt larson xmalloc-test alloc-test cfrac espresso mstress glibc-simple libmimalloc.so versions.txt; do
    [ -e "$BIN/$p" ] || { echo "== bench programs missing ($p): running bench/build.sh"; sh "$ROOT/bench/build.sh" >"$ROOT/target/bench-build.log" 2>&1 || { tail -20 "$ROOT/target/bench-build.log" >&2; die "bench/build.sh failed"; }; break; }
  done
}
# ensure_probe NAME: bench/probes/NAME.c (churn, valkey_model) or bench/NAME.c
# (pin) compiled on demand into $BIN/NAME; bench/build.sh builds them too.
ensure_probe() {
  src=$ROOT/bench/probes/$1.c; [ -f "$src" ] || src=$ROOT/bench/$1.c
  [ -f "$src" ] || die "no source for probe $1"
  mkdir -p "$BIN"
  [ "$BIN/$1" -nt "$src" ] || cc -O2 -Wall -Wextra -Werror -o "$BIN/$1" "$src" || die "$1.c did not compile"
}

# ---- cgroup memory.peak per phase ---------------------------------------------
# The kernel's `memory.peak` is a high-water mark since the cgroup's birth;
# from 6.12 a write resets it, but only for the open file description that
# wrote (a later `cat` still shows the birth peak), so the reset and the
# reads go through one long-lived descriptor: the sidecar
# bench/probes/cgpeak.c, driven by the phase file the soak harness writes
# (DIR/phase names the phase, DIR/stop ends the run) and answering with
# DIR/cgpeak.state (`yes <path>` or `no <reason>`), DIR/cgpeak.<phase> (the
# phase's peak in bytes) and DIR/cgpeak.phase (the acknowledged phase). The
# write is probed, never assumed: Docker mounts the container's cgroup tree
# read-only and the nsdelegate boundary would refuse the write from inside
# the namespace anyway, so `make ... CGROUP_PEAK=1` starts the container
# with --cgroupns=host and /sys/fs/cgroup mounted read-write (the
# Makefile); on a kernel before 6.12 the write then fails with EINVAL. The
# reason is printed once and the callers fall back to VmHWM. The peak is the
# whole container's (server, load generators, harness), unlike VmHWM.
CG_PEAK_STATE=unprobed; CG_PEAK_REASON=; CG_PEAK_PID=
# cgroup_peak_start DIR: start the sidecar on DIR (DIR/phase should already
# name the first phase); waits for its verdict, prints it once. 0 when the
# reset is available.
cgroup_peak_start() {
  ensure_probe cgpeak
  rm -f "$1/cgpeak.state" "$1/cgpeak.phase"
  "$BIN/cgpeak" "$1" &
  CG_PEAK_PID=$!
  i=0; while [ ! -e "$1/cgpeak.state" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
  cg_state=$(cat "$1/cgpeak.state" 2>/dev/null || echo "no the cgpeak sidecar gave no verdict in 5 s")
  case $cg_state in
    yes*) CG_PEAK_STATE=yes; CG_PEAK_REASON=${cg_state#yes };;
    *)    CG_PEAK_STATE=no; CG_PEAK_REASON=${cg_state#no }; wait "$CG_PEAK_PID" 2>/dev/null || true; CG_PEAK_PID=;;
  esac
  if [ "$CG_PEAK_STATE" = yes ]; then echo "== cgroup memory.peak: $CG_PEAK_REASON accepts a reset (kernel $(uname -r)); per-phase peaks are the cgroup's" >&2
  else echo "== cgroup memory.peak: not reset per phase, VmHWM only: $CG_PEAK_REASON" >&2; fi
  [ "$CG_PEAK_STATE" = yes ]
}
# cgroup_peak_phase DIR NAME: the harness has written NAME to DIR/phase;
# wait (up to 3 s) until the sidecar has recorded the previous phase and
# reset, so the new phase's load starts after the reset. No-op without it.
cgroup_peak_phase() {
  [ "$CG_PEAK_STATE" = yes ] || return 0
  i=0; while [ "$(cat "$1/cgpeak.phase" 2>/dev/null)" != "$2" ] && [ $i -lt 30 ]; do sleep 0.1; i=$((i+1)); done
  [ "$(cat "$1/cgpeak.phase" 2>/dev/null)" = "$2" ] || echo "== cgroup memory.peak: the sidecar did not acknowledge phase $2 within 3 s" >&2
}
# cgroup_peak_stop DIR: the harness has touched DIR/stop; wait for the last
# phase's record.
cgroup_peak_stop() { [ -n "$CG_PEAK_PID" ] && { wait "$CG_PEAK_PID" 2>/dev/null || true; CG_PEAK_PID=; }; :; }
# cgroup_peak_mib DIR PHASE: the phase's peak in MiB, "-" without a record.
cgroup_peak_mib() { awk -v f="$1/cgpeak.$2" 'BEGIN {if ((getline b < f) > 0 && b+0 > 0) printf "%.1f", b/1048576; else print "-"}'; }
# cgroup_peak_file_mib DIR PHASE: the cgroup's page cache (memory.stat file)
# at the end of PHASE in MiB, "-" without a record; the peak counts it.
cgroup_peak_file_mib() { awk -v f="$1/cgpeak.$2.file" 'BEGIN {if ((getline b < f) > 0 && b+0 >= 0) printf "%.1f", b/1048576; else print "-"}'; }
# cgroup_peak_note: one line for a results file header.
cgroup_peak_note() {
  if [ "$CG_PEAK_STATE" = yes ]; then echo "cgroup memory.peak reset per phase through $CG_PEAK_REASON (kernel $(uname -r), bench/probes/cgpeak.c); the cgroup peak counts every process of the container and its page cache (memory.stat file, given per phase below), VmHWM the server alone"
  else echo "cgroup memory.peak not reset per phase, VmHWM only: $CG_PEAK_REASON"; fi
}

# ---- stock allocators -------------------------------------------------------

stock_lib() { # stock_lib ALLOC: LD_PRELOAD path for jemalloc|glibc|mimalloc|tcmalloc, 1 otherwise
  case $1 in
    jemalloc) echo "$JEMALLOC_LIB";;
    mimalloc) echo "$BIN/libmimalloc.so";;
    tcmalloc) echo "$TCMALLOC_LIB";;
    glibc)    echo "";;
    *) return 1;;
  esac
}
is_stock() { case $1 in jemalloc|glibc|mimalloc|tcmalloc) return 0;; *) return 1;; esac; }
# apk_version PKG: the installed version of a Wolfi package ("5.3.1" for
# jemalloc-5.3.1-r3), empty when it is not installed.
apk_version() { apk list --installed 2>/dev/null | awk -v p="^$1-[0-9]" '$1 ~ p {print $1}' | sed "s/^$1-//;s/-r[0-9]*\$//" | head -1; }
# stock_version ALLOC: a short version string for a stock allocator.
stock_version() {
  case $1 in
    jemalloc) echo "jemalloc $(apk_version jemalloc)";;
    tcmalloc) echo "tcmalloc-minimal $(apk_version tcmalloc-minimal)";;
    mimalloc) echo "mimalloc $(awk '/^mimalloc /{print $2}' "$BIN/versions.txt" 2>/dev/null)";;
    glibc)    echo "glibc $(apk_version glibc-2.44 || true)";;
  esac
}

higher_is_better() { case $1 in larson*|xmalloc*|valkey*|redis) return 0;; *) return 1;; esac; }

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
# pct_of A B: (A/B-1)*100 signed.
pct_of() { awk -v a="$1" -v b="$2" 'BEGIN {if (a=="-"||b=="-"||a==""||b==""||b==0) print "-"; else printf "%+.1f%%", (a/b-1)*100}'; }
kib_to_mib() { awk -v k="$1" 'BEGIN {if (k=="-"||k=="") print "-"; else printf "%.1f", k/1024}'; }

# ---- variant libraries ------------------------------------------------------

# resolve_rev REV: full hash of a git rev, "tree" for the working tree, or
# "dir:PATH" for a source directory under ROOT that holds a Cargo.toml (an
# A/B copy such as `.tmp-slab16`, made by
# hand from `src Cargo.toml Cargo.lock build.rs`; `.tmp-*` is gitignored
# and never synced to a VM, so the copy is made where it is built).
resolve_rev() {
  case $1 in
    tree|.|WORKTREE) echo tree;;
    dir:*) d=${1#dir:}; [ -f "$ROOT/$d/Cargo.toml" ] || die "source directory '$d' has no Cargo.toml under $ROOT"; echo "dir:$d";;
    *) if [ -f "$ROOT/$1/Cargo.toml" ] && [ "$1" != "${1#.}" ]; then echo "dir:$1"
       else $GIT -C "$ROOT" rev-parse --verify "$1^{commit}" 2>/dev/null || die "cannot resolve rev '$1' (in a worktree the Makefile must mount the common .git)"; fi;;
  esac
}
# rev_dirname REV: the target directory name of a rev's build under
# target/ab/ and friends: "tree", the directory's basename for "dir:PATH",
# or the first 12 characters of a hash. Never spaces or colons: the name
# ends up in an LD_PRELOAD path, which splits on both (the labels of
# rev_label below are for tables, not paths).
rev_dirname() { case $1 in tree) echo tree;; dir:*) basename "${1#dir:}";; *) echo "$1" | cut -c1-12;; esac; }
# build_lib REV TARGETDIR: build libchalloc.so from REV ("tree" = this
# working tree, "dir:PATH" = that source directory in place, otherwise
# `git archive REV` into TARGETDIR/src-tree) with CARGO_TARGET_DIR=TARGETDIR.
# Paths are remapped so identical sources give an identical .so and the
# callers can tell "no code change" from "stale build". Fails unless cargo
# succeeded and the .so exists. Prints nothing on success.
build_lib() {
  rev=$1; tdir=$2
  mkdir -p "$tdir"
  if [ "$rev" = tree ]; then
    srcdir=$ROOT
    # Restored or rsynced files may carry old mtimes and cargo would skip the
    # rebuild; touching the crate root forces it (~seconds).
    touch "$ROOT/src/lib.rs"
  elif [ "$rev" != "${rev#dir:}" ]; then
    srcdir=$ROOT/${rev#dir:}
    [ -f "$srcdir/Cargo.toml" ] || die "source directory $srcdir has no Cargo.toml"
    touch "$srcdir/src/lib.rs"
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

# ---- results files of a driver run by hand --------------------------------
# Rounds of one process per row under rt, allocators interleaved, a
# markdown results file with a common header.

# tree_label: "working tree, HEAD <short>[ + uncommitted src changes]".
tree_label() { echo "working tree, HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes')"; }
# rev_label REV: tree_label for "tree", else the rev cut to 12 characters
# (the build line's label; the header's describe line keeps REV as spelled).
rev_label() { case $1 in tree) tree_label;; dir:*) echo "source directory ${1#dir:}";; *) echo "$1" | cut -c1-12;; esac; }
# default_lib_of ALLOC: the LD_PRELOAD path of a harness allocator: challoc
# is $CHALLOC_LIB (the harness's own build, set before the rounds), the
# rest stock_lib. A harness with more variants wraps it (jvm-bench.sh).
default_lib_of() { case $1 in challoc) echo "$CHALLOC_LIB";; *) stock_lib "$1";; esac; }
# describe_stock ALLOC: the results-header line of a stock allocator.
describe_stock() {
  case $1 in
    jemalloc) echo "LD_PRELOAD=$JEMALLOC_LIB (Wolfi $(stock_version jemalloc))";;
    glibc)    echo "no LD_PRELOAD ($(stock_version glibc) malloc)";;
    mimalloc) echo "LD_PRELOAD=$BIN/libmimalloc.so ($(stock_version mimalloc), bench/build.sh)";;
    tcmalloc) echo "LD_PRELOAD=$TCMALLOC_LIB (Wolfi $(stock_version tcmalloc), gperftools without profiler hooks)";;
    *) return 1;;
  esac
}
# describe_challoc LIB REV: the same line for the harness's challoc build
# (REV as the caller spelled it; "tree" = the working tree).
describe_challoc() { echo "LD_PRELOAD=$1 ($([ "$2" = tree ] && tree_label || echo "$2"), sha256 $(so_hash "$1"))"; }
# record_rt TAG RT LOG: read rt's "elapsed user sys maxrss exit" line for
# run n of TAG (n = the lines already in $OUT/TAG.scores) and append
# "elapsed maxrss exit" to it; a missing or malformed rt line, or a non-zero
# exit, scores "-" and is said on stderr. Sets n el rss ex.
record_rt() {
  n=$(wc -l < "$OUT/$1.scores")
  if [ -s "$2" ] && [ "$(wc -l < "$2")" = 1 ]; then
    read -r el _us _sy rss ex < "$2"
  else
    el=-; rss=-; ex=1; echo "  $1 run $n: rt wrote $(wc -l < "$2" 2>/dev/null || echo 0) lines, expected 1; see $3" >&2
  fi
  [ "$ex" = 0 ] || { el=-; echo "  $1 run $n failed (exit $ex), see $3" >&2; }
  echo "$el $rss $ex" >> "$OUT/$1.scores"
}
# check_score_lines ROWS ALLOCS EXPECTED: every $OUT/ROW.ALLOC.scores must
# have EXPECTED lines (the warm-up plus the timed rounds); more means a
# concurrent run wrote into OUT.
check_score_lines() {
  for row in $1; do for a in $2; do
    n=$(wc -l < "$OUT/$row.$a.scores")
    [ "$n" -eq "$3" ] || die "$OUT/$row.$a.scores has $n lines, expected $3: concurrent run?"
  done; done
}
# result_header TITLE: the first lines of a results file: the title with
# the date and architecture, commit, timestamp, host, kernel.
result_header() {
  echo "# $1, $DATE, $ARCH"
  echo
  echo "- commit: $(commit_short)$(tree_dirty && echo ' (uncommitted changes in src/)')"
  echo "- date: $STAMP"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container${HOST_KIND:+; $HOST_KIND}"
  echo "- kernel: $(uname -r), container: Wolfi"
}
# maps_expect ALLOC WHAT LIB MAPPED [SUFFIX]: the binding check. MAPPED (the
# allocator libraries WHAT mapped, from /proc/<pid>/maps) must be exactly
# LIB resolved (readlink -f: maps shows libtcmalloc_minimal.so.4.6.5 for
# the .so.4 link), or nothing when LIB is empty (glibc). Dies otherwise,
# prints the "   ALLOC: maps ..." line with SUFFIX appended. Sets want.
maps_expect() {
  want=; [ -n "$3" ] && want=$(readlink -f "$3")
  [ "$4" = "$want" ] || die "$1: $2 maps '$4', expected '${want:-nothing}'"
  echo "   $1: maps ${4:-no allocator library (glibc malloc)}${5:-}"
}

# The Valkey driver's cached jemalloc soak (BASELINES=cached).
BASELINE_DIR=$ROOT/target/baselines
