#!/bin/sh
# bench/probes/spe-probe.sh: the `spe-probe` row (bench/rows.toml). Prints
# the host's perf event sources and, where the Statistical Profiling
# Extension is exposed (arm_spe_0: an Arm CPU with SPE, firmware that
# describes it, the arm_spe_pmu module loaded by the modprobe:arm_spe_pmu
# prep), records PROGRAM under the variant with SPE and summarises it:
# samples per synthesized event (memory, l1d-miss, tlb-access, tlb-miss,
# llc-miss, branch-miss, ...), the symbols of the tlb-miss samples and how
# many distinct 4 KiB data pages they touched (#60: the pages one larson
# operation reaches). Needs perf on PATH (the cb perf bundle).
#
#   sh bench/probes/spe-probe.sh PROGRAM ARGS...
#
# The variant's LD_PRELOAD/MALLOC_CONF reach PROGRAM only, not perf.
# Exit 0 whether or not SPE exists (the answer is the log); 1 when perf is
# missing or the recording fails on a host that has arm_spe_0.
set -u
PRE=${LD_PRELOAD:-} CONF=${MALLOC_CONF:-}
unset LD_PRELOAD MALLOC_CONF
D=/sys/bus/event_source/devices
echo "spe-probe: $(uname -r) $(uname -m) paranoid $(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null) caps $(grep CapEff /proc/self/status | cut -f2)"
echo "spe-probe: event sources: $(ls "$D" 2>/dev/null | tr '\n' ' ')"
for c in "$D"/*/caps; do [ -d "$c" ] && echo "spe-probe: ${c%/caps}: caps $(cd "$c" && for f in *; do printf '%s=%s ' "$f" "$(cat "$f")"; done)"; done 2>/dev/null
command -v perf >/dev/null || { echo "spe-probe: no perf on PATH"; exit 1; }
echo "spe-probe: $(perf version)"
perf list 2>/dev/null | grep -i arm_spe | head -5
SPE=$(ls -d "$D"/arm_spe_* 2>/dev/null | head -1)
if [ -z "$SPE" ]; then
  echo "spe-probe: result no-spe (no arm_spe_* event source)"
  exit 0
fi
SPE=${SPE##*/}
echo "spe-probe: $SPE format: $(ls "$D/$SPE/format" | tr '\n' ' ')"
O=$(mktemp -d)
# Loads and stores, user space only, a sample every 1024 operations.
EV="$SPE/ts_enable=1,load_filter=1,store_filter=1,jitter=1/u"
if ! perf record -q -e "$EV" -c 1024 -o "$O/spe.data" -- env ${PRE:+LD_PRELOAD=$PRE} ${CONF:+MALLOC_CONF=$CONF} "$@" > "$O/prog.out" 2> "$O/record.err"; then
  echo "spe-probe: result record-failed: $(tail -3 "$O/record.err" | tr '\n' ' ')"
  rm -rf "$O"; exit 1
fi
tail -2 "$O/prog.out"
echo "spe-probe: data $(du -k "$O/spe.data" | cut -f1) KiB"
perf script -i "$O/spe.data" -F event,addr,ip,sym 2> "$O/script.err" > "$O/script.txt"
echo "spe-probe: samples per event:"
awk '{ e = $1; sub(/:$/, "", e); n[e]++ } END { for (e in n) printf "  %-16s %d\n", e, n[e] }' "$O/script.txt" | sort -k2 -rn | head -20
echo "spe-probe: tlb-miss samples by symbol:"
awk '$1 ~ /^tlb-miss/ { n[$NF]++ } END { for (s in n) printf "  %8d %s\n", n[s], s }' "$O/script.txt" | sort -rn | head -15
awk '$1 ~ /^tlb-miss/ { a = $2; if (a ~ /^[0-9a-f]+$/) { p[substr(a, 1, length(a) - 3)]++; t++ } }
     END { n = 0; for (k in p) n++; printf "spe-probe: tlb-miss data addresses %d, distinct 4 KiB pages %d\n", t, n }' "$O/script.txt"
echo "spe-probe: result spe ($SPE)"
rm -rf "$O"
exit 0
