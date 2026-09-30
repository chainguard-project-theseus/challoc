#!/bin/sh
# Ruby benchmark rows (docs/plan.md Phase 1.5 task 4b): Ruby 3.4 built
# `--with-jemalloc` by tests/consumers/ruby-build.sh, measured under five allocators
# with a fixed subset of ruby/benchmark run through benchmark-driver, one
# real command (`gem list`) and a GC-shaped script (bench/ruby/gc_churn.rb).
# Runs inside the dev container: `make ruby-bench`, or on a VM
# `bench/gce/challoc-bench make arm64 ruby-bench HOST_KIND=...`. Needs
# `make ruby STAGE=build` and `make ruby STAGE=build ALLOC=glibc` first (the
# two Ruby builds under target/ruby/); this script re-stages challoc's
# libjemalloc.so.2 from the working tree, installs both builds into their
# prefixes (once) and clones benchmark-driver (once).
#
# Allocators (ALLOCS):
#   challoc    the --with-jemalloc ruby; its DT_NEEDED libjemalloc.so.2 resolves
#              through the rpath to challoc's stage (quantum16, jemalloc-soname)
#   jemalloc   the same binary with LD_PRELOAD=/usr/lib/libjemalloc.so.2
#              (Wolfi's jemalloc 5.3; the soname matches, so the loader binds
#              DT_NEEDED to the preloaded copy and the stage is never mapped)
#   glibc      the --without-jemalloc build (Ruby then calls malloc_trim after
#              a full GC and has no malloc_usable_size-based accounting)
#   mimalloc   the --without-jemalloc build with LD_PRELOAD of the mimalloc
#              built by bench/build.sh
#   tcmalloc   the --without-jemalloc build with LD_PRELOAD of Wolfi's
#              tcmalloc-minimal (gperftools, /usr/lib/libtcmalloc_minimal.so.4)
# Every variant's /proc/self/maps is checked before the rounds: exactly the
# expected allocator library is mapped.
#
# Rows (ROWS): each is one process per round, wall time and max RSS from
# bench/rt around the measured ruby (benchmark-driver's `-e` executable is a
# wrapper that execs rt); lower is better.
#   startup                ruby -e 1 with RubyGems enabled
#   so_binary_trees, hash_aref_str, array_sample_100k__1k, array_sort_block,
#   so_nbody, app_fib      ruby/benchmark/*.rb through benchmark-driver
#                          --runner time (ruby --disable-gems, as Ruby's own
#                          `make benchmark` does). so_binary_trees is slot
#                          allocation (3-element arrays live in Ruby's own heap
#                          pages), the two array rows malloc 8 KiB buffers per
#                          iteration (sample results, sort temporaries), so_nbody
#                          and app_fib are the controls that allocate nothing
#   erb_render             ruby/benchmark/erb_render.yml (1.5M renders)
#   gem_list               `gem list` on the installed prefix (RubyGems specs)
#   gc_churn               bench/ruby/gc_churn.rb: long-lived medium buffers,
#                          bursts of short-lived ones, then 10 forced full GCs;
#                          GC.stat counts and RSS before/after the forced GCs
#                          go into their own table
#
#   ROUNDS=5     timed rounds after one discarded warm-up, allocators
#                interleaved within a round, every score listed sorted
#   RESULT=path  markdown output (default bench/results/<date>-<arch>-ruby.md)
#   OUT=dir      raw logs and score files (default target/ruby/bench-out)
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc glibc mimalloc tcmalloc"}
ROUNDS=${ROUNDS:-5}
ROWS=${ROWS:-"startup so_binary_trees hash_aref_str array_sample_100k__1k array_sort_block so_nbody app_fib erb_render gem_list gc_churn"}
WORK=$ROOT/target/ruby
OUT=${OUT:-$WORK/bench-out}
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH-ruby.md}
RUBY_TAG=${RUBY_TAG:-v3_4_10}
SRC=$WORK/src-$RUBY_TAG
COMPAT=$WORK/jemalloc-compat
STAGE_LIB=$COMPAT/stage/lib
BD=$WORK/benchmark-driver
GC_ROUNDS=${GC_ROUNDS:-10}
GC_FORCED=${GC_FORCED:-10}
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
ensure_bench_programs
for a in $ALLOCS; do case $a in challoc|jemalloc|glibc|mimalloc|tcmalloc) ;; *) die "unknown allocator '$a'";; esac; done
rm -rf "$OUT"; mkdir -p "$OUT" "$(dirname "$RESULT")"

# ---- builds, stage, installs ------------------------------------------------
# The --with-jemalloc build serves challoc and jemalloc, the glibc build
# serves glibc, mimalloc and tcmalloc.
build_of() { case $1 in challoc|jemalloc) echo challoc;; *) echo glibc;; esac; }
for a in $ALLOCS; do
  b=$(build_of "$a")
  [ -x "$WORK/build-$b/ruby" ] || die "no Ruby build at target/ruby/build-$b: run 'make ruby STAGE=build$([ "$b" = glibc ] && echo ' ALLOC=glibc')' first"
done
case " $ALLOCS " in *" challoc "*|*" jemalloc "*)
  echo "== staging challoc as libjemalloc.so.2 from the working tree (HEAD $(commit_short)$(tree_dirty && echo ' + uncommitted src changes'))"
  touch "$ROOT/src/lib.rs"
  TARGET_DIR=$COMPAT QUANTUM=16 sh "$ROOT/scripts/jemalloc-compat.sh" > "$OUT/stage.log" 2>&1 || { tail -20 "$OUT/stage.log" >&2; die "jemalloc-compat stage failed"; }
  grep -q 'Compiling challoc' "$OUT/stage.log" || echo "   (cargo reused the previous stage build: sources unchanged since)"
  STAGE_HASH=$(so_hash "$STAGE_LIB/libjemalloc.so.2");;
esac
# install-<build>: the build directory's ruby has no stdlib path; the
# configured prefix (target/ruby/install-<build>) does. Redone when the
# build's ruby is newer than the installed one.
for b in $(for a in $ALLOCS; do build_of "$a"; done | sort -u); do
  inst=$WORK/install-$b
  if [ ! -x "$inst/bin/ruby" ] || [ "$WORK/build-$b/ruby" -nt "$inst/bin/ruby" ]; then
    printf '== installing the %s build into %s ... ' "$b" "${inst#"$ROOT"/}"
    make -C "$WORK/build-$b" install > "$OUT/install-$b.log" 2>&1 || { tail -20 "$OUT/install-$b.log" >&2; die "make install ($b) failed"; }
    echo done
  fi
done
if [ ! -d "$BD/.git" ]; then
  echo "== cloning benchmark-driver into ${BD#"$ROOT"/}"
  git clone -q --depth 1 https://github.com/benchmark-driver/benchmark-driver "$BD" || die "benchmark-driver clone failed"
fi
BD_COMMIT=$($GIT -C "$BD" rev-parse --short HEAD)
BD_VERSION=$(sed -n "s/.*VERSION = '\(.*\)'.*/\1/p" "$BD/lib/benchmark_driver/version.rb")

# ---- per-allocator ruby command lines -------------------------------------------
inst_of() { echo "$WORK/install-$(build_of "$1")"; }
preload_of() { # the LD_PRELOAD value, empty for challoc and glibc
  case $1 in jemalloc) echo "$JEMALLOC_LIB";; mimalloc) echo "$BIN/libmimalloc.so";; tcmalloc) echo "$TCMALLOC_LIB";; *) echo "";; esac
}
# ruby_cmd ALLOC: "env [LD_PRELOAD=..] LD_LIBRARY_PATH=.. <prefix>/bin/ruby"
ruby_cmd() {
  inst=$(inst_of "$1"); pre=$(preload_of "$1")
  echo "env ${pre:+LD_PRELOAD=$pre }LD_LIBRARY_PATH=$inst/lib $inst/bin/ruby"
}
# The executable benchmark-driver runs: rt around the measured ruby.
for a in $ALLOCS; do
  printf '#!/bin/sh\nexec "%s" %s --disable-gems "$@"\n' "$BIN/rt" "$(ruby_cmd "$a")" > "$OUT/wrap-$a.sh"
  chmod +x "$OUT/wrap-$a.sh"
done
# benchmark-driver's host process runs on the challoc-built ruby if present
# (the host process is not measured).
host=$(for a in $ALLOCS; do echo "$a"; done | head -1)
DRIVER="$(ruby_cmd "$host") --disable-gems -I $BD/lib $BD/exe/benchmark-driver"

# ---- binding check ------------------------------------------------------------
echo "== ruby: $($(ruby_cmd "$host") -v)"
for a in $ALLOCS; do
  mapped=$($(ruby_cmd "$a") --disable-gems -e 'puts File.readlines("/proc/self/maps").grep(%r{/lib(jemalloc|mimalloc|tcmalloc)[^/ ]*\.so}).map { |l| l.split[5] }.uniq.sort.join(" ")')
  case $a in
    challoc)  want=$STAGE_LIB/libjemalloc.so.2;;
    jemalloc) want=$JEMALLOC_LIB;;
    glibc)    want=;;
    mimalloc) want=$BIN/libmimalloc.so;;
    tcmalloc) want=$(readlink -f "$TCMALLOC_LIB");;   # maps shows the resolved .so.4.6.5
  esac
  [ "$mapped" = "$want" ] || die "$a: ruby maps '$mapped', expected '${want:-nothing}'"
  echo "   $a: maps ${mapped:-no allocator library (glibc malloc)}"
  echo "$mapped" > "$OUT/mapped.$a"
done

# ---- one row ------------------------------------------------------------------
bench_file() { # bench_file ROW: the ruby/benchmark file, or 1 for the script rows
  for ext in rb yml; do [ -f "$SRC/benchmark/$1.$ext" ] && { echo "$SRC/benchmark/$1.$ext"; return 0; }; done
  return 1
}
for row in $ROWS; do
  case $row in startup|gem_list|gc_churn) ;; *) bench_file "$row" >/dev/null || die "no $SRC/benchmark/$row.{rb,yml}";; esac
done
# run_ruby_row ROW ALLOC: appends "wall_s maxrss_kb exit" to $OUT/ROW.ALLOC.scores.
run_ruby_row() {
  row=$1; a=$2; tag=$row.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out; rt=$OUT/$tag.$n.rt
  inst=$(inst_of "$a")
  set -- $(ruby_cmd "$a")
  case $row in
    startup)  RT_OUT=$rt "$BIN/rt" "$@" -e 1 > "$log" 2>&1 || true;;
    gem_list) RT_OUT=$rt "$BIN/rt" "$@" "$inst/bin/gem" list > "$log" 2>&1 || true;;
    gc_churn) RT_OUT=$rt "$BIN/rt" "$@" --disable-gems "$ROOT/bench/ruby/gc_churn.rb" "$GC_ROUNDS" "$GC_FORCED" > "$log" 2>&1 || true;;
    *)        RT_OUT=$rt $DRIVER --runner time -o simple -e "$a::$OUT/wrap-$a.sh" "$(bench_file "$row")" > "$log" 2>&1 || true;;
  esac
  record_rt "$tag" "$rt" "$log"
}

# ---- rounds -------------------------------------------------------------------
echo "== $ROUNDS rounds + warm-up x $(echo $ROWS | wc -w) rows x $(echo $ALLOCS | wc -w) allocators ($ALLOCS); load $LOAD0"
r=0
while [ $r -le "$ROUNDS" ]; do
  for a in $ALLOCS; do
    for row in $ROWS; do run_ruby_row "$row" "$a"; done
  done
  printf '   round %d done, load %s, %s\n' $r "$(loadavg)" "$(date -u +%T)"
  r=$((r+1))
done
check_score_lines "$ROWS" "$ALLOCS" $((ROUNDS+1))

# ---- report -------------------------------------------------------------------
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
rss_of() { median_rss "$OUT/$1.$2.scores" 1; }
med_of() { median_list "$(list_of "$1" "$2")"; }
# gc_kv KEY ALLOC: median over the timed runs of KEY from gc_churn's output line.
gc_kv() {
  k=$1; a=$2; i=1
  while [ $i -le "$ROUNDS" ]; do
    [ -f "$OUT/gc_churn.$a.$i.out" ] && awk -v k="$k" '{for (i = 1; i <= NF; i++) {split($i, p, "="); if (p[1] == k) print p[2]}}' "$OUT/gc_churn.$a.$i.out"
    i=$((i+1))
  done | sort -n | awk '{a[NR]=$1} END {if (NR) print a[int((NR+1)/2)]; else print "-"}'
}
describe() {
  case $1 in
    challoc)  echo "the --with-jemalloc ruby, DT_NEEDED libjemalloc.so.2 bound via rpath to $STAGE_LIB/libjemalloc.so.2 (challoc, quantum16, sha256 $STAGE_HASH)";;
    jemalloc) echo "the same --with-jemalloc ruby with LD_PRELOAD=$JEMALLOC_LIB (Wolfi $(stock_version jemalloc)); the stage is not mapped";;
    glibc)    echo "the --without-jemalloc ruby, glibc malloc";;
    mimalloc) echo "the --without-jemalloc ruby with LD_PRELOAD=$BIN/libmimalloc.so ($(awk '/^mimalloc /{print $2}' "$BIN/versions.txt"))";;
    tcmalloc) echo "the --without-jemalloc ruby with LD_PRELOAD=$TCMALLOC_LIB (Wolfi tcmalloc-minimal $(apk_version tcmalloc-minimal))";;
  esac
}
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
{
  result_header "Ruby 3.4 benchmark rows"
  echo "- ruby: $($(ruby_cmd "$host") -v) (tag $RUBY_TAG built by tests/consumers/ruby-build.sh with Wolfi's flags, --enable-yjit configured but YJIT not enabled at run time), installed under target/ruby/install-{challoc,glibc}"
  echo "- benchmark-driver $BD_VERSION ($BD_COMMIT), --runner time -o simple, one job per process, ruby --disable-gems for the ruby/benchmark rows"
  echo "- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
  echo "- method: $ROUNDS timed rounds after one discarded warm-up, allocators interleaved per round, every run's wall time listed sorted; wall time (s, lower is better) and max RSS (bench/rt's maxrss, median KiB) are of the measured ruby process only (benchmark-driver's host process is outside rt). gc_churn runs bench/ruby/gc_churn.rb $GC_ROUNDS $GC_FORCED. Percentages are challoc against the other allocator: positive means challoc is slower or larger."
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
  if case " $ROWS " in *" gc_churn "*) true;; *) false;; esac; then
    echo
    echo "## gc_churn: GC.stat and RSS around $GC_FORCED forced full GCs (medians over the timed runs)"
    echo
    echo "| allocator | GC count | major | minor | allocated objects | heap pages | RSS before forced GCs MiB | RSS after MiB | after vs before | VmHWM MiB |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    for a in $ALLOCS; do
      b=$(gc_kv rss_before_kb "$a"); af=$(gc_kv rss_after_kb "$a")
      echo "| $a | $(gc_kv gc_count "$a") | $(gc_kv major "$a") | $(gc_kv minor "$a") | $(gc_kv allocated_objects "$a") | $(gc_kv heap_pages "$a") | $(kib_to_mib "$b") | $(kib_to_mib "$af") | $(pct_of "$af" "$b") | $(kib_to_mib "$(gc_kv hwm_kb "$a")") |"
    done
    echo
    echo "GC counts are the process total (bursts plus the $GC_FORCED forced cycles); \"RSS before\" is after the last burst, before the forced cycles. Ruby's GC calls malloc_trim after a full GC only in the --without-jemalloc build (glibc, mimalloc, tcmalloc rows)."
  fi
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Medians/,$p' "$RESULT"
