#!/bin/sh
# PostgreSQL benchmark rows (docs/plan.md Phase 3 task 5 (b)): Wolfi's
# postgresql-17 (server, pgbench, psql from the image) driven by pgbench
# under five allocators, the allocator in the postmaster's environment so
# every forked backend and auxiliary process inherits it. Runs inside the
# dev container: `make postgres-bench`, or on a VM
# `bench/gce/challoc-bench run arm64 'make postgres-bench HOST_KIND=...'`.
#
# The container runs as root and postgres refuses to; the script creates the
# OS user OSUSER (pgb) on first use and runs initdb, pg_ctl and the RSS
# sampler as that user through busybox `su`. PGDATA lives under
# target/postgres/data (initdb --locale=C -E UTF8, trust auth on a unix
# socket under target/postgres/run), loaded once with `pgbench -i -s SCALE`
# and reused by every allocator: only the server processes change, the
# data files do not. Server options are passed on the command line (`-o`),
# postgresql.conf is untouched: max_connections MAX_CONNECTIONS (the 500-
# client row), shared_buffers SHARED_BUFFERS (the scale-50 database fits, so
# the select rows serve from shared memory and the allocator is what
# differs between processes), huge_pages=off, synchronous_commit=off,
# checkpoint_timeout=30min and max_wal_size=8GB (no checkpoint inside a
# 60 s row), log_min_messages=warning.
#
# Allocators (ALLOCS): challoc (REV, default the working tree, built into
# target/postgres/lib), jemalloc (Wolfi's /usr/lib/libjemalloc.so.2), glibc
# (no preload), mimalloc (bench/build.sh's libmimalloc.so), tcmalloc
# (Wolfi's tcmalloc-minimal). LD_PRELOAD goes on `pg_ctl start`, so the
# postmaster and everything it forks run on it; pgbench itself (the client,
# JOBS threads) runs as root on glibc malloc, the same binary for every
# allocator. Before the rounds every allocator's binding is checked in
# /proc/<postmaster>/maps and in one live backend's maps: exactly the
# expected library is mapped.
#
# Rows (ROWS), each one pgbench run against a server started for the
# allocator (restarted per allocator per round):
#   select100   pgbench -S -c CLIENTS_A -j JOBS -T DURATION      select-only, 100 backends
#   tpcb100     pgbench    -c CLIENTS_A -j JOBS -T DURATION      TPC-B (read/write), 100 backends
#   select500   pgbench -S -c CLIENTS_B -j JOBS -T DURATION      select-only, 500 backends
#   connect20   pgbench -C -c CONN_CLIENTS -j CONN_JOBS -T CONN_DURATION
#               a new connection per transaction (fork + backend init +
#               TPC-B + exit): the per-connection latency row
# Scores per run: tps (pgbench's line; connect20 includes reconnection
# times), latency average (ms), p99 (ms) of the transaction latencies
# pgbench logs with `-l --sampling-rate=SAMPLING_RATE` (a SAMPLING_RATE
# share of the transactions; the connect20 latency excludes the connect,
# which pgbench reports separately as "average connection time", the
# conn ms column). RSS: bench/probes/pg-rss-sampler.sh reads smaps_rollup
# for every postgres process of OSUSER every INTERVAL s while pgbench runs;
# the row's peak (max over the samples of the sum of Pss) and final (the
# last sample with live backends) of the sum of Pss and of the sum of
# Pss_Anon (the anonymous share: heaps and stacks, not shared_buffers),
# and at the final sample the per-backend median Pss and Pss_Anon and the
# backend count. Higher tps is better; lower latency and RSS are better.
#
#   ROUNDS=5      timed rounds after one discarded warm-up, allocators
#                 interleaved within a round, every tps listed sorted
#   SCALE=50      pgbench -i scale (5M accounts rows, about 750 MB)
#   DURATION=60 CONN_DURATION=30 INTERVAL=5 SAMPLING_RATE=0.05
#   JOBS=16 CLIENTS_A=100 CLIENTS_B=500 CONN_CLIENTS=20 CONN_JOBS=4
#   SHARED_BUFFERS=1GB MAX_CONNECTIONS=600
#   RESULT=path   markdown output (default bench/results/<date>-<arch>-postgres.md)
#   OUT=dir       raw logs, samples and score files (default target/postgres/bench-out)
#   REV=tree      challoc rev (any git rev; "tree" = the working tree)
#   REINIT=0      1: drop PGDATA and initdb + load again
#   REPORT_ONLY=0 1: render RESULT again from the samples and scores already
#                 under OUT (no build, no rounds; the server is started once
#                 for its version and settings); the commit, date and load
#                 lines of an existing RESULT are kept
#   HOST_CPU, HOST_KIND   recorded in the header (the Makefile passes HOST_CPU)
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
. "$ROOT/bench/lib.sh"
ALLOCS=${ALLOCS:-"challoc jemalloc glibc mimalloc tcmalloc"}
ROUNDS=${ROUNDS:-5}
ROWS=${ROWS:-"select100 tpcb100 select500 connect20"}
REV=${REV:-tree}
SCALE=${SCALE:-50}
DURATION=${DURATION:-60}
CONN_DURATION=${CONN_DURATION:-30}
INTERVAL=${INTERVAL:-5}
SAMPLING_RATE=${SAMPLING_RATE:-0.05}
JOBS=${JOBS:-16}
CLIENTS_A=${CLIENTS_A:-100}
CLIENTS_B=${CLIENTS_B:-500}
CONN_CLIENTS=${CONN_CLIENTS:-20}
CONN_JOBS=${CONN_JOBS:-4}
SHARED_BUFFERS=${SHARED_BUFFERS:-1GB}
MAX_CONNECTIONS=${MAX_CONNECTIONS:-600}
REINIT=${REINIT:-0}
REPORT_ONLY=${REPORT_ONLY:-0}
OSUSER=${OSUSER:-pgb}
DBUSER=$OSUSER
DB=postgres
WORK=$ROOT/target/postgres
PGDATA=$WORK/data
SOCK=$WORK/run
PGLOGS=$WORK/logs
OUT=${OUT:-$WORK/bench-out}
RESULT=${RESULT:-$ROOT/bench/results/$DATE-$ARCH-postgres.md}
SAMPLER=$ROOT/bench/probes/pg-rss-sampler.sh
PG_OPTS="-c listen_addresses= -c unix_socket_directories=$SOCK -c max_connections=$MAX_CONNECTIONS -c shared_buffers=$SHARED_BUFFERS -c huge_pages=off -c synchronous_commit=off -c checkpoint_timeout=30min -c max_wal_size=8GB -c log_min_messages=warning"
T_START=$(date +%s)
LOAD0=$(loadavg)

take_bench_lock
for a in $ALLOCS; do case $a in challoc) ;; *) is_stock "$a" || die "unknown allocator '$a'";; esac; done
for row in $ROWS; do case $row in select100|tpcb100|select500|connect20) ;; *) die "unknown row '$row'";; esac; done
for b in postgres pg_ctl initdb pgbench psql; do command -v "$b" >/dev/null 2>&1 || die "no $b in this container: rebuild the image from Dockerfile.dev (postgresql-17, postgresql-17-client)"; done
case " $ALLOCS " in *" mimalloc "*) ensure_bench_programs;; esac
[ "$(id -u)" = 0 ] || die "must run as root in the container (creates user $OSUSER and runs the server as it)"
id "$OSUSER" >/dev/null 2>&1 || adduser -D -H -s /bin/sh "$OSUSER" || die "cannot create user $OSUSER"
[ "$REPORT_ONLY" = 1 ] || rm -rf "$OUT"
mkdir -p "$OUT" "$WORK" "$SOCK" "$PGLOGS" "$(dirname "$RESULT")"
chown "$OSUSER:$OSUSER" "$SOCK" "$PGLOGS"
# as_pg CMD...: run a command line as the postgres OS user (busybox su; the
# environment for the server goes inside the command line, `env LD_PRELOAD=...`).
as_pg() { su -s /bin/sh "$OSUSER" -c "$*"; }
PG_VERSION=$(postgres --version)
PGBENCH_VERSION=$(pgbench --version)

# ---- the challoc library ----------------------------------------------------------
CHALLOC_LIB=
case " $ALLOCS " in *" challoc "*)
  CHALLOC_LIB=$WORK/lib/release/libchalloc.so
  if [ "$REPORT_ONLY" = 1 ]; then
    [ -f "$CHALLOC_LIB" ] || die "REPORT_ONLY=1 but no $CHALLOC_LIB from the measured run"
  else
    rev=$(resolve_rev "$REV")
    printf '== building challoc (%s) into target/postgres/lib ... ' "$(rev_label "$rev")"
    build_lib "$rev" "$WORK/lib"
    chmod 644 "$CHALLOC_LIB"
    echo "done, $(so_hash "$CHALLOC_LIB")"
  fi;;
esac
lib_of() { default_lib_of "$1"; }

# ---- cluster ------------------------------------------------------------------------
[ "$REINIT" = 1 ] && rm -rf "$PGDATA"
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  rm -rf "$PGDATA"; mkdir -p "$PGDATA"; chown "$OSUSER:$OSUSER" "$PGDATA"; chmod 700 "$PGDATA"
  printf '== initdb into %s ... ' "${PGDATA#"$ROOT"/}"
  as_pg "initdb -D $PGDATA -U $DBUSER --auth=trust -E UTF8 --locale=C" > "$OUT/initdb.log" 2>&1 || { tail -20 "$OUT/initdb.log" >&2; die "initdb failed"; }
  echo done
fi
psql_() { psql -h "$SOCK" -U "$DBUSER" -X -Atq "$@" "$DB"; }
pg_start() { # pg_start ALLOC: start the server with the allocator in its environment
  lib=$(lib_of "$1")
  as_pg "env ${lib:+LD_PRELOAD=$lib }pg_ctl -D $PGDATA -w -t 120 -l $PGLOGS/$1.log -o '$PG_OPTS' start" > "$OUT/start.$1.log" 2>&1 9>&- \
    || { tail -5 "$OUT/start.$1.log" "$PGLOGS/$1.log" >&2; die "server did not start under $1"; }
  i=0; until psql_ -c 'select 1' >/dev/null 2>&1; do i=$((i+1)); [ $i -gt 100 ] && die "server under $1 not answering on $SOCK"; sleep 0.1; done
  PM_PID=$(head -1 "$PGDATA/postmaster.pid")
}
pg_stop() { as_pg "pg_ctl -D $PGDATA -m fast -w -t 120 stop" > "$OUT/stop.log" 2>&1 || { tail -5 "$OUT/stop.log" >&2; die "server did not stop"; }; PM_PID=; }
PM_PID=
cleanup() { [ -n "$PM_PID" ] && as_pg "pg_ctl -D $PGDATA -m immediate stop" >/dev/null 2>&1; touch "$OUT/stop" 2>/dev/null; :; }
BENCH_CLEANUP=cleanup   # run by the bench lock's EXIT/INT/TERM trap (bench/lib.sh take_bench_lock)
# The data set, loaded once under glibc (the data files are the same
# whatever the server's allocator) and kept when the scale matches.
pg_start glibc
have=$(psql_ -c "select count(*) from pgbench_branches" 2>/dev/null || echo 0)
if [ "$have" != "$SCALE" ]; then
  printf '== pgbench -i -s %s ... ' "$SCALE"
  pgbench -h "$SOCK" -U "$DBUSER" -i -s "$SCALE" -q "$DB" > "$OUT/init.log" 2>&1 || { tail -5 "$OUT/init.log" >&2; die "pgbench -i failed"; }
  psql_ -c "checkpoint" >/dev/null
  tail -1 "$OUT/init.log"
fi
DB_SIZE=$(psql_ -c "select pg_size_pretty(pg_database_size('$DB'))")
ACCOUNTS=$(psql_ -c "select count(*) from pgbench_accounts")
SETTINGS=$(psql_ -c "select name || '=' || setting from pg_settings where name in ('shared_buffers','max_connections','huge_pages','synchronous_commit','checkpoint_timeout','max_wal_size','block_size','jit') order by name" | tr '\n' ' ')
pg_stop
echo "== $PG_VERSION (Wolfi postgresql-17 $(apk_version postgresql-17)); scale $SCALE, $ACCOUNTS accounts, database $DB_SIZE; $SETTINGS"

# ---- binding check ------------------------------------------------------------------
# maps_of PID: the allocator libraries mapped by PID, read as the owner.
maps_of() { as_pg "grep -oE '/[^ ]*/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\.so[.0-9]*' /proc/$1/maps" | sort -u | tr '\n' ' ' | sed 's/ $//'; }
[ "$REPORT_ONLY" = 1 ] || for a in $ALLOCS; do
  pg_start "$a"
  lib=$(lib_of "$a"); want=; [ -n "$lib" ] && want=$(readlink -f "$lib")
  pm_maps=$(maps_of "$PM_PID")
  psql_ -c "select pg_sleep(5)" >/dev/null 2>&1 9>&- &
  sl=$!
  bp=; i=0
  until [ -n "$bp" ] || [ $i -gt 40 ]; do bp=$(psql_ -c "select pid from pg_stat_activity where query like '%pg_sleep%' and pid <> pg_backend_pid()"); i=$((i+1)); sleep 0.1; done
  [ -n "$bp" ] || die "$a: no backend found for the maps check"
  be_maps=$(maps_of "$bp")
  wait $sl || true
  pg_stop
  [ "$pm_maps" = "$want" ] || die "$a: postmaster $PM_PID maps '$pm_maps', expected '${want:-nothing}'"
  [ "$be_maps" = "$want" ] || die "$a: backend $bp maps '$be_maps', expected '${want:-nothing}'"
  echo "   $a: postmaster and backend map ${want:-no allocator library (glibc malloc)}"
  echo "$want" > "$OUT/mapped.$a"
done

# ---- one row --------------------------------------------------------------------------
row_args() {
  case $1 in
    select100) echo "-S -c $CLIENTS_A -j $JOBS -T $DURATION";;
    tpcb100)   echo "-c $CLIENTS_A -j $JOBS -T $DURATION";;
    select500) echo "-S -c $CLIENTS_B -j $JOBS -T $DURATION";;
    connect20) echo "-C -c $CONN_CLIENTS -j $CONN_JOBS -T $CONN_DURATION";;
  esac
}
row_clients() { case $1 in select100|tpcb100) echo "$CLIENTS_A";; select500) echo "$CLIENTS_B";; connect20) echo "$CONN_CLIENTS";; esac; }
# p99_of PREFIX: p99 in ms of column 3 (transaction time, us) of pgbench's
# -l files; "-" without any line.
p99_of() {
  cat "$1".* 2>/dev/null | awk '{print $3}' | sort -n | awk '{a[NR]=$1} END {if (!NR) {print "-"; exit} i=int(NR*0.99+0.999); if (i<1) i=1; printf "%.3f", a[i]/1000}'
}
# run_pg_row ROW ALLOC: the server for ALLOC is running. Appends
# "tps lat_ms p99_ms conn_ms exit" to $OUT/ROW.ALLOC.scores; the samples go
# to $OUT/ROW.ALLOC.<n>.samples, pgbench's output to .out.
run_pg_row() {
  row=$1; a=$2; tag=$row.$a
  [ -f "$OUT/$tag.scores" ] || : > "$OUT/$tag.scores"
  n=$(wc -l < "$OUT/$tag.scores")
  log=$OUT/$tag.$n.out
  rm -f "$OUT/stop"
  as_pg "sh $SAMPLER $OSUSER $DBUSER $PGDATA $INTERVAL $OUT" > "$OUT/$tag.$n.samples" 9>&- &
  sp=$!
  set -- $(row_args "$row")
  ex=0
  pgbench -h "$SOCK" -U "$DBUSER" "$@" -l --sampling-rate="$SAMPLING_RATE" --log-prefix="$OUT/$tag.$n.lat" "$DB" > "$log" 2>&1 || ex=$?
  touch "$OUT/stop"; wait $sp || true
  tps=$(awk '/^tps = / {printf "%.0f", $3}' "$log")
  lat=$(awk '/^latency average = / {print $4}' "$log")
  conn=$(awk '/^average connection time = / {print $5}' "$log")
  p99=$(p99_of "$OUT/$tag.$n.lat")
  rm -f "$OUT/$tag.$n.lat".*
  [ -n "$tps" ] || { [ "$ex" = 0 ] && ex=1; }
  [ "$ex" = 0 ] || { tps=-; echo "  $tag run $n failed (exit $ex), see $log" >&2; }
  echo "${tps:--} ${lat:--} ${p99:--} ${conn:--} $ex" >> "$OUT/$tag.scores"
}

# ---- rounds -------------------------------------------------------------------------
[ "$REPORT_ONLY" = 1 ] && echo "== REPORT_ONLY: rendering from $OUT" || echo "== $ROUNDS rounds + warm-up x $(echo $ROWS | wc -w) rows x $(echo $ALLOCS | wc -w) allocators ($ALLOCS); load $LOAD0"
r=0
[ "$REPORT_ONLY" = 1 ] && r=$((ROUNDS+1))
while [ $r -le "$ROUNDS" ]; do
  for a in $ALLOCS; do
    pg_start "$a"
    for row in $ROWS; do run_pg_row "$row" "$a"; done
    pg_stop
  done
  printf '   round %d done, load %s, %s\n' $r "$(loadavg)" "$(date -u +%T)"
  r=$((r+1))
done
check_score_lines "$ROWS" "$ALLOCS" $((ROUNDS+1))

# ---- report ---------------------------------------------------------------------------
# col_median FILE COL: median of column COL over the timed runs (line 2 on).
col_median() { awk -v c="$2" 'NR>1 && $c!="-" {print $c}' "$1" | sort -n | awk '{a[NR]=$1} END {if (NR) print a[int((NR+1)/2)]; else print "-"}'; }
pg_failed() { awk 'NR>1 && $5!=0 {f=1} END {print f+0}' "$1"; }
list_of() { sorted_scores "$OUT/$1.$2.scores" 1; }
tps_of() { median_list "$(list_of "$1" "$2")"; }
lat_of() { col_median "$OUT/$1.$2.scores" 2; }
p99_med() { col_median "$OUT/$1.$2.scores" 3; }
conn_of() { col_median "$OUT/$1.$2.scores" 4; }
# rss_stats FILE: one samples file -> "peak_pss final_pss peak_anon final_anon
# backends backend_pss_med backend_anon_med last_pss samples" in KiB. final = the
# last sample whose backend count is at least 90% of the run's maximum (a
# sample of 500 smaps_rollup files takes seconds and can straddle the end
# of the row while the backends exit, counting only part of them; such a
# partial sample is not the row's end state), or the last sample when no
# sample saw a backend (connect20's backends live milliseconds); last = the
# very last sample. Peaks are over every sample.
rss_stats() {
  awk '
    function med(arr, n,   i, j, t) {   # insertion sort, n <= a few hundred
      for (i = 2; i <= n; i++) { t = arr[i]; j = i - 1; while (j > 0 && arr[j] > t) { arr[j+1] = arr[j]; j-- } arr[j+1] = t }
      return n ? arr[int((n+1)/2)] : "-"
    }
    function flush() {
      if (!have) return
      ns++; S[ns] = sum; A[ns] = anon; NB[ns] = nb
      BM[ns] = (nb > 0) ? med(bp, nb) : "-"; AM[ns] = (nb > 0) ? med(ba, nb) : "-"
      if (sum > peak) peak = sum; if (anon > peaka) peaka = anon; if (nb > maxnb) maxnb = nb
    }
    $1 == "S" { flush(); have = 1; sum = 0; anon = 0; nb = 0; next }
    $1 == "P" { sum += $4; anon += $5; if ($3 == "backend") { nb++; bp[nb] = $4; ba[nb] = $5 } }
    END {
      flush()
      if (!ns) { print "0 0 0 0 0 - - 0 0"; exit }
      f = ns; if (maxnb > 0) { for (i = ns; i >= 1; i--) if (NB[i] >= 0.9 * maxnb) { f = i; break } }
      printf "%s %s %s %s %s %s %s %s %s\n", peak+0, S[f]+0, peaka+0, A[f]+0, NB[f]+0, BM[f], AM[f], S[ns]+0, ns }' "$1"
}
# rss_med ROW ALLOC FIELD: median over the timed runs of rss_stats field FIELD (1..9), KiB (9: sample count).
rss_med() {
  i=1
  while [ $i -le "$ROUNDS" ]; do
    [ -f "$OUT/$1.$2.$i.samples" ] && rss_stats "$OUT/$1.$2.$i.samples" | awk -v f="$3" '{print $f}'
    i=$((i+1))
  done | awk '$1!="-"' | sort -n | awk '{a[NR]=$1} END {if (NR) print a[int((NR+1)/2)]; else print "-"}'
}
rss_mib() { kib_to_mib "$(rss_med "$1" "$2" "$3")"; }
describe() { case $1 in challoc) describe_challoc "$CHALLOC_LIB" "$REV";; *) describe_stock "$1";; esac; }
has() { case " $ALLOCS " in *" $1 "*) return 0;; *) return 1;; esac; }
# delta ROW ALLOC: challoc against ALLOC on tps (sign flipped: positive =
# challoc slower), latency, p99, peak Pss, final Pss, per-backend Pss.
delta() {
  row=$1; a=$2
  printf ' %s | %s | %s | %s | %s | %s |' \
    "$(pct_of "$(tps_of "$row" "$a")" "$(tps_of "$row" challoc)")" \
    "$(pct_of "$(lat_of "$row" challoc)" "$(lat_of "$row" "$a")")" \
    "$(pct_of "$(p99_med "$row" challoc)" "$(p99_med "$row" "$a")")" \
    "$(pct_of "$(rss_med "$row" challoc 1)" "$(rss_med "$row" "$a" 1)")" \
    "$(pct_of "$(rss_med "$row" challoc 2)" "$(rss_med "$row" "$a" 2)")" \
    "$(pct_of "$(rss_med "$row" challoc 6)" "$(rss_med "$row" "$a" 6)")"
}
# In REPORT_ONLY mode the measured run's commit, date, load and duration
# lines are kept from the existing file (the tables are re-rendered from
# its raw samples and scores; the library is the one that run built).
COMMIT_LINE="- commit: $(commit_short)$(tree_dirty && echo ' (uncommitted changes in src/)' || true)"   # an assignment takes the substitution's status: || true for set -e
DATE_LINE="- date: $STAMP"
LOAD_LINE="- load average at start: $LOAD0; at end: $(loadavg); $(( $(date +%s) - T_START )) s total"
TITLE_LINE="# PostgreSQL 17 benchmark rows, $DATE, $ARCH"
if [ "$REPORT_ONLY" = 1 ] && [ -f "$RESULT" ]; then
  for v in COMMIT_LINE:commit DATE_LINE:date LOAD_LINE:"load average" TITLE_LINE:"# PostgreSQL"; do
    k=${v#*:}; l=$(grep -m1 "^\(- \)\?$k" "$RESULT" || true); [ -n "$l" ] && eval "${v%%:*}=\$l"
  done
  LOAD_LINE="$LOAD_LINE; tables re-rendered from the run's samples on $STAMP (REPORT_ONLY)"
fi
{
  echo "$TITLE_LINE"
  echo
  echo "$COMMIT_LINE"
  echo "$DATE_LINE"
  echo "- host: ${HOST_CPU:-unknown CPU}, $PROCS CPUs visible to the container${HOST_KIND:+; $HOST_KIND}"
  echo "- kernel: $(uname -r), container: Wolfi"
  echo "- server: $PG_VERSION, $PGBENCH_VERSION (Wolfi postgresql-17 $(apk_version postgresql-17)); PGDATA target/postgres/data (initdb --locale=C -E UTF8, OS and database user $OSUSER, trust auth on a unix socket), started by \`pg_ctl start\` as $OSUSER with the allocator in its environment (LD_PRELOAD), so the postmaster and every backend and auxiliary process it forks run on it; pgbench (the client, its -j threads) runs as root on glibc malloc, the same binary for every allocator. Options on the command line: $SETTINGS(postgresql.conf untouched otherwise)"
  echo "- data: pgbench -i -s $SCALE ($ACCOUNTS accounts rows), database size $DB_SIZE, loaded once and shared by every allocator (the data files do not depend on the server's allocator)"
  echo "- rows: select100 = \`pgbench $(row_args select100)\` (select-only); tpcb100 = \`pgbench $(row_args tpcb100)\` (TPC-B); select500 = \`pgbench $(row_args select500)\` (select-only, max_connections $MAX_CONNECTIONS); connect20 = \`pgbench $(row_args connect20)\` (a new connection per transaction: fork, backend start-up, one TPC-B transaction, exit); all with pgbench's default pre-run vacuum of pgbench_branches/tellers and truncate of pgbench_history (outside the timed window) and -l --sampling-rate=$SAMPLING_RATE for the latency percentiles"
  echo "$LOAD_LINE"
  echo "- method: $ROUNDS timed rounds after one discarded warm-up, allocators interleaved per round (one server start per allocator per round, the rows in the order above against it, then a fast stop), every run's tps listed sorted. tps and latency average are pgbench's lines (connect20's tps includes the reconnection times); p99 is of the transaction latencies pgbench logged (a $SAMPLING_RATE sample; excludes the connect in connect20, whose \"average connection time\" is the conn ms column). RSS is the sum of smaps_rollup Pss over every postgres process of user $OSUSER (postmaster, backends, auxiliary processes; Pss counts a shared_buffers page once across them), sampled every $INTERVAL s plus the pass's own duration while pgbench runs (bench/probes/pg-rss-sampler.sh; the samples-per-run column says how many passes a run got): peak = the largest sample of the run, final = the last sample with at least 90% of the run's maximum backend count (a sample of 500 processes takes seconds and can straddle the row's end while the backends exit); Pss_Anon is the anonymous share of the same sums (the allocators' heaps and stacks; shared_buffers is Pss_Shmem and identical for every allocator); per-backend = the median over the backends of one backend's Pss / Pss_Anon at the final sample (the 100- and 500-connection points). Every RSS figure is the median over the timed runs, in MiB. Percentages are challoc against the other allocator: positive means challoc is slower (fewer tps), higher latency or larger."
  echo "- allocators (postmaster and one live backend checked in /proc/<pid>/maps before the rounds):"
  for a in $ALLOCS; do echo "  - $a: $(describe "$a")"; done
  echo
  echo "## Sorted scores (tps, higher is better)"
  echo
  echo "| row | allocator | sorted tps | latency avg ms | p99 ms | conn ms |"
  echo "|---|---|---|---:|---:|---:|"
  for row in $ROWS; do for a in $ALLOCS; do
    flag=; [ "$(pg_failed "$OUT/$row.$a.scores")" = 0 ] || flag=" (FAILED)"
    echo "| $row | $a | $(list_of "$row" "$a")$flag | $(lat_of "$row" "$a") | $(p99_med "$row" "$a") | $(conn_of "$row" "$a") |"
  done; done
  echo
  echo "## Medians: throughput and latency"
  echo
  hdr="| row |"; sep="|---|"
  for a in $ALLOCS; do hdr="$hdr $a tps | lat ms | p99 ms |"; sep="$sep---:|---:|---:|"; done
  echo "$hdr"; echo "$sep"
  for row in $ROWS; do
    line="| $row |"
    for a in $ALLOCS; do line="$line $(tps_of "$row" "$a") | $(lat_of "$row" "$a") | $(p99_med "$row" "$a") |"; done
    echo "$line"
  done
  if case " $ROWS " in *" connect20 "*) true;; *) false;; esac; then
    echo
    echo "connect20, average connection time (ms, pgbench's line, median over the timed runs): $(for a in $ALLOCS; do printf '%s %s; ' "$a" "$(conn_of connect20 "$a")"; done | sed 's/; $//')."
  fi
  echo
  echo "## Medians: RSS (MiB, sum of smaps_rollup Pss over every postgres process)"
  echo
  echo "| row | allocator | peak Pss | final Pss | peak Pss_Anon | final Pss_Anon | backends at final | per-backend Pss | per-backend Pss_Anon | samples per run |"
  echo "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|"
  for row in $ROWS; do for a in $ALLOCS; do
    echo "| $row | $a | $(rss_mib "$row" "$a" 1) | $(rss_mib "$row" "$a" 2) | $(rss_mib "$row" "$a" 3) | $(rss_mib "$row" "$a" 4) | $(rss_med "$row" "$a" 5) | $(rss_mib "$row" "$a" 6) | $(rss_mib "$row" "$a" 7) | $(rss_med "$row" "$a" 9) |"
  done; done
  echo
  echo "\"samples per run\" is the median number of sampler passes in a run: one pass reads smaps_rollup for every postgres process and, with 500 backends on a saturated host, takes tens of seconds, so the 500-client row has few complete samples and its peak and final are the same steady-state pass. The sampler records each pass's duration in its S lines (bench/probes/pg-rss-sampler.sh)."
  if has challoc; then
    echo
    echo "## challoc against glibc and jemalloc"
    echo
    hdr="| row |"; sep="|---|"
    for a in glibc jemalloc mimalloc tcmalloc; do has "$a" && { hdr="$hdr vs $a: tps | latency | p99 | peak Pss | final Pss | backend Pss |"; sep="$sep---:|---:|---:|---:|---:|---:|"; }; done
    echo "$hdr"; echo "$sep"
    for row in $ROWS; do
      line="| $row |"
      for a in glibc jemalloc mimalloc tcmalloc; do has "$a" && line="$line$(delta "$row" "$a")"; done
      echo "$line"
    done
    echo
    echo "Positive percentages mean challoc did worse than that allocator (fewer tps, higher latency, more RSS)."
  fi
} > "$RESULT"
echo "== wrote $RESULT"
sed -n '/^## Medians: throughput/,$p' "$RESULT"
