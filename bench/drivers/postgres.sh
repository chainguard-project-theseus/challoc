#!/bin/sh
# bench/drivers/postgres.sh: the PostgreSQL rows (Wolfi's postgresql-17).
# The server runs as a non-root user (pgb; postgres refuses root) with the
# allocator in the postmaster's environment, so every backend and auxiliary
# process it forks inherits it; pgbench, the client, runs on glibc malloc.
# The data set (initdb --locale=C, `pgbench -i -s SCALE`) is made once per
# container under PG_WORK and shared by every run: the data files do not
# depend on the server's allocator, and pgbench truncates its history table
# before each run. One run is one server start, one pgbench run, a fast
# stop.
#
#   ROW=select100   pgbench -S -c 100 -j 16 -T SECS      (select-only)
#   ROW=tpcb100     pgbench -c 100 -j 16 -T SECS         (TPC-B)
#   ROW=select500   pgbench -S -c 500 -j 16 -T SECS
#   ROW=connect20   pgbench -C -c 20 -j 4 -T SECS        (a connection per
#                   transaction: fork and backend start-up)
#
# Metrics: tps (the score), lat_ms (pgbench's average latency), conn_ms
# (connect20: the average connection time), pss_peak_mib (smaps_rollup Pss
# summed over every postgres process, the largest sample of the run, from
# bench/probes/pg-rss-sampler.sh running as pgb).
. "$(dirname "$0")/cb.sh"
ROW=${ROW:?ROW=select100|tpcb100|select500|connect20}
SECS=${SECS:-30}
SCALE=${SCALE:-50}
PG_WORK=${PG_WORK:-/var/tmp/cb-postgres}
OSUSER=pgb
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
for b in postgres pg_ctl initdb pgbench psql; do command -v "$b" >/dev/null || die "no $b in this image"; done
[ "$(id -u)" = 0 ] || die "must run as root in the container (it creates $OSUSER)"
case $ROW in
  select100) ARGS="-S -c 100 -j 16 -T $SECS";;
  tpcb100)   ARGS="-c 100 -j 16 -T $SECS";;
  select500) ARGS="-S -c 500 -j 16 -T $SECS";;
  connect20) ARGS="-C -c 20 -j 4 -T $SECS";;
  *) die "unknown ROW=$ROW";;
esac
id "$OSUSER" >/dev/null 2>&1 || adduser -D -H -s /bin/sh "$OSUSER" >/dev/null 2>&1 || die "cannot create user $OSUSER"
PGDATA=$PG_WORK/data-s$SCALE
SOCK=$PG_WORK/run
mkdir -p "$PG_WORK" "$SOCK"
chown "$OSUSER:$OSUSER" "$PG_WORK" "$SOCK"
chmod 755 "$CB_OUT"
as_pg() { su -s /bin/sh "$OSUSER" -c "$*"; }
psql_() { psql -h "$SOCK" -U "$OSUSER" -X -Atq "$@" postgres; }
OPTS="-c listen_addresses= -c unix_socket_directories=$SOCK -c max_connections=600 -c shared_buffers=1GB -c huge_pages=off -c synchronous_commit=off -c checkpoint_timeout=30min -c max_wal_size=8GB -c log_min_messages=warning"
LOGF=$PG_WORK/server.log
start() { # start [ENV...]: pg_ctl start with the environment given
  as_pg "env $* pg_ctl -D $PGDATA -w -t 120 -l $LOGF -o '$OPTS' start" > "$CB_OUT/start.out" 2>&1 \
    || { tail -5 "$CB_OUT/start.out" "$LOGF" >&2; die "server did not start"; }
}
stop() { as_pg "pg_ctl -D $PGDATA -m fast -w -t 120 stop" > "$CB_OUT/stop.out" 2>&1 || true; }
cleanup() { touch "$CB_OUT/stop"; as_pg "pg_ctl -D $PGDATA -m immediate stop" >/dev/null 2>&1 || true; }

if [ ! -f "$PG_WORK/ready-s$SCALE" ]; then
  log "initdb and pgbench -i -s $SCALE into $PGDATA (once per container)"
  rm -rf "$PGDATA"; mkdir -p "$PGDATA"; chown "$OSUSER:$OSUSER" "$PGDATA"; chmod 700 "$PGDATA"
  as_pg "initdb -D $PGDATA -U $OSUSER --auth=trust -E UTF8 --locale=C" > "$CB_OUT/initdb.out" 2>&1 || { tail -20 "$CB_OUT/initdb.out" >&2; die "initdb failed"; }
  start
  pgbench -h "$SOCK" -U "$OSUSER" -i -s "$SCALE" -q postgres > "$CB_OUT/init.out" 2>&1 || { stop; tail -5 "$CB_OUT/init.out" >&2; die "pgbench -i failed"; }
  psql_ -c checkpoint >/dev/null
  stop
  touch "$PG_WORK/ready-s$SCALE"
fi

trap cleanup EXIT
rm -f "$CB_OUT/stop"
# shellcheck disable=SC2046 # alloc_env prints VAR=value words for start
start $(alloc_env)
PM=$(head -1 "$PGDATA/postmaster.pid")
i=0; until psql_ -c 'select 1' >/dev/null 2>&1; do i=$((i + 1)); [ $i -gt 100 ] && die "server not answering on $SOCK"; sleep 0.1; done
# The postmaster's maps, as the owner (root has no CAP_SYS_PTRACE here).
want=; [ -n "$CB_LIB" ] && want=$(readlink -f "$CB_LIB")
got=$(as_pg "grep -oE '/[^ ]*/lib(challoc|jemalloc|mimalloc|tcmalloc)[^/ ]*\.so[.0-9]*' /proc/$PM/maps" | sort -u | while read -r f; do readlink -f "$f"; done | sort -u | tr '\n' ' ' | sed 's/ $//')
[ "$got" = "$want" ] || die "postmaster $PM maps '${got:-nothing}', expected '${want:-no allocator library}'"

as_pg "sh $ROOT/bench/probes/pg-rss-sampler.sh $OSUSER $OSUSER $PGDATA 2 $CB_OUT" > "$CB_OUT/samples" 2>/dev/null &
sp=$!
# shellcheck disable=SC2086
pgbench -h "$SOCK" -U "$OSUSER" $ARGS postgres > "$CB_OUT/pgbench.out" 2>&1 || { touch "$CB_OUT/stop"; tail -10 "$CB_OUT/pgbench.out" >&2; die "pgbench failed"; }
touch "$CB_OUT/stop"; wait "$sp" || true
stop
tps=$(awk '/^tps = / { printf "%.1f", $3 }' "$CB_OUT/pgbench.out")
[ -n "$tps" ] || die "no tps line"
metric tps "$tps"
metric lat_ms "$(awk '/^latency average = / { print $4 }' "$CB_OUT/pgbench.out")"
metric conn_ms "$(awk '/^average connection time = / { print $5 }' "$CB_OUT/pgbench.out")"
metric pss_peak_mib "$(awk '$1 == "S" { if (s > m) m = s; s = 0; next } $1 == "P" { s += $4 } END { if (s > m) m = s; if (m) printf "%.3f", m / 1024 }' "$CB_OUT/samples")"
emit
