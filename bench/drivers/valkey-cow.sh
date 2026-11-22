#!/bin/sh
# bench/drivers/valkey-cow.sh: Valkey's (or Redis's, SERVER=redis) fork
# copy-on-write under a BGSAVE (the valkey-cow* and redis-cow* rows,
# bench/rows.toml; docs/design.md section 5, "Fork epochs"). One run is one
# fresh server from bench/drivers/kv.sh under the variant's allocator
# (challoc: the runpath build with the server's contrib series, Valkey's
# 0001 to 0004 or Redis's 0001 to 0006, so a library with fork epochs runs
# the epoch around the save unless its MALLOC_CONF has cow_epoch:false;
# jemalloc: the bundled jemalloc 5.3.0 build, unpatched), driven by
# bench/cow/cowbench.py: a fill of KEYS keys of SIZE bytes, a BGSAVE
# stretched by rdb-key-save-delay, WRITES random keys overwritten at a
# constant rate while the child runs, the child's Private_Dirty sampled.
#
#   MODE=cow   the copy: cow_mib (rdb_last_cow_size, the score), peak_mib
#              (parent RSS plus child dirty at their peak), rss_after_mib
#   MODE=tput  SET throughput (valkey-benchmark or redis-benchmark, 50
#              clients, pipeline 16)
#              with no child (set_outside_rps, the score) and during the
#              save (set_inside_rps)
#   MODE=rec   the overwrite, then RECOVER seconds with activedefrag on:
#              rss_end_rel_pct (RSS at the end as a percentage of the RSS
#              before the save, the score), recover_s (seconds until RSS is
#              within 5% of it, at least 1; RECOVER when never)
#   MODE=drain the overwrite with activedefrag on throughout, and the
#              benchmark's GET then SET (50 clients, pipeline 16) right after
#              the child exits, while the fork epoch's drain runs its defrag
#              cycle: set_drain_rps (the score), set_drain_p99_ms,
#              get_drain_rps, get_drain_p99_ms; the same with no child
#              before the save (set_quiet_rps, get_quiet_rps), drain_s
#              (seconds from the child's exit until no defrag cycle runs,
#              DRAIN_WAIT when never) and drain_moves_s (until the cycle's
#              hits last rose). Pair head with
#              noepoch=head@cow_epoch:false for the drain against no drain.
#   SERVER=valkey KEYS=10000000 SIZE=64 WRITES=1000000 DELAY=-10 RECOVER=120
#   DRAIN_WAIT=330 (challoc's drain window is 300 s from the child's exit)
. "$(dirname "$0")/cb.sh"
ROOT=${CB_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
KV_SERVER=${SERVER:-valkey}
. "$ROOT/bench/drivers/kv.sh"
MODE=${MODE:-cow}
KEYS=${KEYS:-10000000}
SIZE=${SIZE:-64}
WRITES=${WRITES:-1000000}
DELAY=${DELAY:--10}
RECOVER=${RECOVER:-120}
DRAIN_WAIT=${DRAIN_WAIT:-330}
kv_ensure_jemalloc
kv_resolve "$CB_ALLOC" native
conf_env=${KV_PRELOAD:+LD_PRELOAD=$KV_PRELOAD}
conf_word=$(kv_conf_env "$CB_ALLOC" "${CB_CONF:-}") || exit 1
conf_env="$conf_env${conf_word:+ $conf_word}"
PORT=$(port_from 16800)
D=$CB_OUT/cow
mkdir -p "$D"
log "$CB_ALLOC ($KV_BUILD build) $MODE: $KV_SRV"
set -- --server "$KV_SRV" --label "$CB_ALLOC" --port "$PORT" --keys "$KEYS" --vsize "$SIZE" --save-delay "$DELAY"
case $MODE in
  cow) set -- "$@" --writes "$WRITES";;
  tput) set -- "$@" --bench "$KV_BENCH";;
  rec) set -- "$@" --writes "$WRITES" --recover "$RECOVER" --defrag;;
  drain) set -- "$@" --writes "$WRITES" --bench "$KV_BENCH" --drain-bench --drain-wait "$DRAIN_WAIT";;
  *) die "MODE must be cow, tput, rec or drain, got '$MODE'";;
esac
# shellcheck disable=SC2086 # conf_env is VAR=value words
env $conf_env COWBENCH_DATA="$D" python3 "$ROOT/bench/cow/cowbench.py" "$@" > "$D/run.json" || die "cowbench failed"
python3 - "$D/run.json" "$MODE" > "$D/metrics" <<'PY' || die "cowbench gave no result"
import json, sys
r = json.loads(open(sys.argv[1]).read())
mode = sys.argv[2]
M = 1 << 20
out = {}
if mode == "cow":
    out = {"cow_mib": r["rdb_last_cow_size"] / M, "peak_mib": r["peak_parent_rss_plus_child_dirty"] / M,
           "rss_after_mib": r["rss_after_save"] / M}
elif mode == "tput":
    out = {"set_outside_rps": r["set_ops_s"]["outside"], "set_inside_rps": r["set_ops_s"]["inside"]}
elif mode == "drain":
    d, q = r["drain"], r["quiet"]
    out = {"set_drain_rps": d["set"]["rps"], "set_drain_p99_ms": d["set"]["p99_ms"],
           "get_drain_rps": d["get"]["rps"], "get_drain_p99_ms": d["get"]["p99_ms"],
           "set_quiet_rps": q["set"]["rps"], "get_quiet_rps": q["get"]["rps"], "drain_s": d["s"],
           "drain_moves_s": d["moves_s"]}
else:
    b = r["rss_before"]
    within = [t for t, rss, *_ in r["recover"] if rss <= b * 1.05]
    out = {"rss_end_rel_pct": r["rss_end"] / b * 100, "recover_s": max(1, within[0] if within else r["recover"][-1][0]),
           "cow_mib": r["rdb_last_cow_size"] / M}
for k, v in out.items():
    print(k, "%.3f" % v)
PY
while read -r k v; do metric "$k" "$v"; done < "$D/metrics"
emit
