#!/usr/bin/env bash
# bench/gce/thp-ab.sh prep|run: the THP A/B on a THP=always host (Phase 2.5
# task 14/16, lane E, 2026-09-20; results in
# bench/results/2026-09-20-aarch64-cgos-thp-ab.md). Runs natively on a
# Chainguard OS cell VM (LD_PRELOAD of the container-built libraries on the
# host's glibc 2.44) from ~/challoc after `challoc-bench matrix lab cgos-*`.
#
#   prep  build the dev image, the library, the bench binaries and the two C
#         probes in the container (the host has no cc/make), copy what the
#         native rows need to ~/native/target/bench-bin;
#   run   the A/B (the default), writing RUN lines and a summary table to
#         stdout and the per-run outputs to ~/thp-ab-out/.
#
# Five settings on the same tree:
#   a  challoc, MALLOC_CONF unset             (thp:never, the shipped default: MADV_NOHUGEPAGE on every reservation)
#   b  challoc, thp:auto                      (no advice either way: the pre-task-16 behaviour on this host)
#   c  challoc, thp:always                    (MADV_HUGEPAGE on the regions)
#   d  challoc, thp:never,thp_process:advised (the task 16 prctl on top of the default)
#   j  jemalloc                               (the reference)
# Rows valkey1, valkey1-free, larson16; one discarded warm-up round then
# ROUNDS (7) timed rounds, interleaved (round > row > setting); per run the
# score, peak Rss and peak AnonHugePages from /proc/<pid>/smaps_rollup at
# 200 ms, THP_enabled from /proc/<pid>/status about 1 s in, and the
# /proc/vmstat thp_* deltas around the run (nothing else runs on the VM).
# The host's THP `enabled` is never written. Launch it as a transient
# systemd unit (a plain ssh job dies with the session on this OS):
#   sudo systemd-run --collect --quiet --unit=challoc-thpab-$(date +%s) --uid=$(id -u) --gid=$(id -g) \
#     --setenv=HOME=$HOME --working-directory=$HOME/challoc \
#     bash -c 'bash bench/gce/thp-ab.sh prep > ~/logs/thp-ab-prep.log 2>&1; bash bench/gce/thp-ab.sh run > ~/logs/thp-ab.log 2>&1; echo $? > ~/logs/ab-done'
set -u
export PATH="$HOME/bin:$PATH"
cd "$HOME/challoc" || exit 1
if [ "${1:-run}" = prep ]; then
  echo "# prep on $(hostname): $(uname -m) $(uname -r), $(date -u +%FT%TZ)"
  s=$(date +%s)
  make image > ~/logs/prep-image.log 2>&1; echo "make image rc=$? in $(( $(date +%s) - s )) s"
  s=$(date +%s)
  make build > ~/logs/prep-build.log 2>&1; echo "make build rc=$? in $(( $(date +%s) - s )) s"
  s=$(date +%s)
  make bench-build > ~/logs/prep-bench-build.log 2>&1; echo "make bench-build rc=$? in $(( $(date +%s) - s )) s"
  tail -3 ~/logs/prep-bench-build.log
  s=$(date +%s)
  make run CMD='set -e; cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c tests/c/thp.c -o target/thp-ab-thp -ldl; cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c tests/c/features.c -o target/thp-ab-features -ldl; rm -rf /src/.tmp-native; mkdir -p /src/.tmp-native/target/bench-bin; cp target/bench-bin/rt target/bench-bin/larson target/bench-bin/valkey_model target/release/libchalloc.so /usr/lib/libjemalloc.so.2 target/thp-ab-thp target/thp-ab-features /src/.tmp-native/target/bench-bin/; ls -l target/release/libchalloc.so /usr/lib/libjemalloc.so.2; chmod -R a+rX /src/.tmp-native' > ~/logs/prep-copyout.log 2>&1; echo "copy-out rc=$? in $(( $(date +%s) - s )) s"
  cat ~/logs/prep-copyout.log
  rm -rf ~/native/target; mkdir -p ~/native/target
  cp -r "$HOME/challoc/.tmp-native/target/bench-bin" ~/native/target/
  ls -l ~/native/target/bench-bin
  echo "# native check: $(LD_PRELOAD=$HOME/native/target/bench-bin/libchalloc.so $HOME/native/target/bench-bin/thp-ab-features 2>&1 | grep -m1 '^features:')"
  echo "# prep end $(date -u +%FT%TZ)"
  exit 0
fi
BIN=$HOME/native/target/bench-bin
T=/sys/kernel/mm/transparent_hugepage
ROUNDS=${ROUNDS:-7}
N=${VALKEY_MODEL_N:-850000}; SECS=${VALKEY_MODEL_SECS:-10}
SETS="a b c d j"
ROWS="valkey1 valkey1-free larson16"
OUT=$HOME/thp-ab-out; rm -rf "$OUT"; mkdir -p "$OUT"
say() { echo; echo "#### $* ($(date -u +%T))"; }
vm() { awk -v k="$1" '$1 == k { print $2 }' /proc/vmstat; }
conf_of() { case $1 in a) echo "";; b) echo "thp:auto";; c) echo "thp:always";; d) echo "thp:never,thp_process:advised";; j) echo "";; esac; }
lib_of() { case $1 in j) echo "$BIN/libjemalloc.so.2";; *) echo "$BIN/libchalloc.so";; esac; }
name_of() { case $1 in a) echo "challoc default (thp:never)";; b) echo "challoc thp:auto";; c) echo "challoc thp:always";; d) echo "challoc thp:never,thp_process:advised";; j) echo "jemalloc";; esac; }
args_of() { case $1 in
  valkey1)      echo "$BIN/valkey_model -n $N -t $SECS -s";;
  valkey1-free) echo "$BIN/valkey_model -n $N -t $SECS -s -f free";;
  larson16)     echo "$BIN/larson 5 8 1000 5000 100 4141 16";;
esac; }
tree() { h=$(cat .git/HEAD 2>/dev/null); case $h in ref:*) cut -c1-7 ".git/${h#ref: }" 2>/dev/null;; *) echo "${h:0:7}";; esac; }
image() { curl -s -H Metadata-Flavor:Google http://metadata.google.internal/computeMetadata/v1/instance/image | sed 's|.*/||'; }
mtype() { curl -s -H Metadata-Flavor:Google http://metadata.google.internal/computeMetadata/v1/instance/machine-type | sed 's|.*/||'; }

echo "# thp-ab on $(hostname): $(uname -m) $(uname -r), $(nproc) CPUs, $(mtype), image $(image), $(date -u +%FT%TZ), tree $(tree)"
echo "# glibc: $(/usr/lib/libc.so.6 | head -1)"
echo "# cpu: $(grep -m1 -i 'implementer' /proc/cpuinfo | tr -s ' ') $(grep -m1 -i 'CPU part' /proc/cpuinfo | tr -s ' ')"
echo "# host THP (as shipped, not written by this script): enabled $(cat $T/enabled | tr -d '\n'); defrag $(cat $T/defrag | tr -d '\n'); hugepages-2048kB $(cat $T/hugepages-2048kB/enabled | tr -d '\n'); shrink_underused $(cat $T/shrink_underused 2>/dev/null)"
printf '# mTHP sizes:'; for d in $T/hugepages-*kB; do [ -f "$d/enabled" ] && printf ' %s=%s' "${d##*/hugepages-}" "$(grep -o '\[.*\]' "$d/enabled" | tr -d '[]')"; done; echo
echo "# khugepaged: $(for f in $T/khugepaged/*; do printf '%s=%s ' "${f##*/}" "$(cat "$f")"; done)"
echo "# rows: $ROWS; settings: $SETS; $ROUNDS timed rounds after one warm-up, interleaved round > row > setting"
echo "# valkey_model -n $N -t $SECS; larson 5 8 1000 5000 100 4141 16"
echo "# load at start: $(cut -d' ' -f1-3 /proc/loadavg)"
for s in a j; do l=$(lib_of $s); echo "# $s: $l mapped=$(LD_PRELOAD="$l" cat /proc/self/maps 2>&1 | grep -c "$(basename "$l")")"; done

say "the thp: report line, the feature line and the kernel's view per setting (tests/c/thp.c and features.c, natively)"
for s in a b c d; do
  case $s in a) m=never;; b) m=auto;; c) m=always;; d) m=process;; esac
  cf=$(conf_of $s)
  echo "-- setting $s: $(name_of $s); MALLOC_CONF='${cf}'; thp-ab-thp $m"
  if [ -n "$cf" ]; then
    MALLOC_CONF="$cf" LD_PRELOAD="$BIN/libchalloc.so" "$BIN/thp-ab-thp" "$m" 2>&1 | sed 's/^/  /'
    echo "  $(MALLOC_CONF="$cf" LD_PRELOAD="$BIN/libchalloc.so" "$BIN/thp-ab-features" 2>&1 | grep -m1 '^features:')"
  else
    LD_PRELOAD="$BIN/libchalloc.so" "$BIN/thp-ab-thp" "$m" 2>&1 | sed 's/^/  /'
    echo "  $(LD_PRELOAD="$BIN/libchalloc.so" "$BIN/thp-ab-features" 2>&1 | grep -m1 '^features:')"
  fi
done

# run ROUND ROW SET: one run, one RUN line.
run() {
  local r=$1 b=$2 s=$3 cf l log pid maxa maxr te n x fa0 ff0 sp0 ds0 t0 ex sc
  cf=$(conf_of "$s"); l=$(lib_of "$s")
  set -- $(args_of "$b")
  log=$OUT/$b.$s.$r.out
  fa0=$(vm thp_fault_alloc); ff0=$(vm thp_fault_fallback); sp0=$(vm thp_split_page); ds0=$(vm thp_deferred_split_page)
  t0=$(date +%s.%N)
  if [ -n "$cf" ]; then env MALLOC_CONF="$cf" LD_PRELOAD="$l" "$@" > "$log" 2>&1 &
  else env LD_PRELOAD="$l" "$@" > "$log" 2>&1 & fi
  pid=$!; maxa=0; maxr=0; te=-; n=0
  while kill -0 "$pid" 2>/dev/null; do
    x=$(awk '/^AnonHugePages/ { a = $2 } /^Rss:/ { r = $2 } END { print a + 0, r + 0 }' "/proc/$pid/smaps_rollup" 2>/dev/null)
    set -- $x; [ "${1:-0}" -gt "$maxa" ] && maxa=$1; [ "${2:-0}" -gt "$maxr" ] && maxr=$2
    n=$((n + 1)); [ "$n" -eq 5 ] && te=$(awk '/^THP_enabled/ { print $2 }' "/proc/$pid/status" 2>/dev/null)
    sleep 0.2
  done
  wait "$pid"; ex=$?
  case $b in
    larson*) sc=$(awk '/Throughput/ {printf "%.1f", $3/1e6}' "$log");;
    valkey*) sc=$(awk '/^ops / {for (i=2;i<=NF;i++) {split($i, p, "="); if (p[1]=="write_mops") print p[2]}}' "$log");;
  esac
  [ "$ex" = 0 ] || sc=-
  echo "RUN round=$r row=$b set=$s score=${sc:--} rss_kib=$maxr ahp_kib=$maxa thp_enabled=${te:--} fault_alloc=$(( $(vm thp_fault_alloc) - fa0 )) fault_fallback=$(( $(vm thp_fault_fallback) - ff0 )) split=$(( $(vm thp_split_page) - sp0 )) deferred_split=$(( $(vm thp_deferred_split_page) - ds0 )) exit=$ex secs=$(awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN {printf "%.1f", b - a}') load=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/runs.txt"
}

say "the runs (round 0 is the warm-up)"
r=0
while [ "$r" -le "$ROUNDS" ]; do
  for b in $ROWS; do for s in $SETS; do run "$r" "$b" "$s"; done; done
  r=$((r + 1))
done
echo "# load at end: $(cut -d' ' -f1-3 /proc/loadavg)"

say "summary (rounds 1..$ROUNDS; sorted scores, medians; MiB = KiB/1024; vs = challoc median against jemalloc's, positive = challoc worse)"
awk -v rows="$ROWS" -v sets="$SETS" '
function med(arr, n,   i, j, t, a) { for (i = 1; i <= n; i++) a[i] = arr[i]; for (i = 2; i <= n; i++) { t = a[i]; for (j = i - 1; j >= 1 && a[j] > t; j--) a[j + 1] = a[j]; a[j + 1] = t } return a[int((n + 1) / 2)] }
function sorted(arr, n,   i, j, t, a, s) { for (i = 1; i <= n; i++) a[i] = arr[i]; for (i = 2; i <= n; i++) { t = a[i]; for (j = i - 1; j >= 1 && a[j] > t; j--) a[j + 1] = a[j]; a[j + 1] = t } s = ""; for (i = 1; i <= n; i++) s = s (i > 1 ? " " : "") a[i]; return s }
$1 == "RUN" {
  for (i = 2; i <= NF; i++) { split($i, kv, "="); v[kv[1]] = kv[2] }
  if (v["round"] == 0 || v["score"] == "-") next
  k = v["row"] SUBSEP v["set"]; n[k]++
  sc[k, n[k]] = v["score"]; rss[k, n[k]] = v["rss_kib"]; ahp[k, n[k]] = v["ahp_kib"]; fa[k, n[k]] = v["fault_alloc"]; ds[k, n[k]] = v["deferred_split"]
  if (v["ahp_kib"] + 0 > ahpmax[k]) ahpmax[k] = v["ahp_kib"] + 0
  te[k] = v["thp_enabled"]
}
END {
  split(rows, R, " "); split(sets, S, " ")
  print "| row | setting | sorted scores | median | vs jemalloc | median peak Rss MiB | median peak AnonHugePages MiB (max) | median thp_fault_alloc | median thp_deferred_split_page | THP_enabled |"
  print "|---|---|---|---:|---:|---:|---:|---:|---:|---:|"
  for (ri = 1; ri in R; ri++) {
    b = R[ri]; kj = b SUBSEP "j"
    for (i = 1; i <= n[kj]; i++) tmp[i] = sc[kj, i]; mj = med(tmp, n[kj])
    for (si = 1; si in S; si++) {
      s = S[si]; k = b SUBSEP s
      if (!n[k]) continue
      for (i = 1; i <= n[k]; i++) tmp[i] = sc[k, i]; m = med(tmp, n[k]); lst = sorted(tmp, n[k])
      for (i = 1; i <= n[k]; i++) tmp[i] = rss[k, i]; mr = med(tmp, n[k])
      for (i = 1; i <= n[k]; i++) tmp[i] = ahp[k, i]; ma = med(tmp, n[k])
      for (i = 1; i <= n[k]; i++) tmp[i] = fa[k, i]; mf = med(tmp, n[k])
      for (i = 1; i <= n[k]; i++) tmp[i] = ds[k, i]; md = med(tmp, n[k])
      vs = (s == "j" || mj == 0) ? "-" : sprintf("%+.1f%%", (mj - m) / mj * 100)
      printf "| %s | %s | %s | %s | %s | %.1f | %.1f (%.1f) | %d | %d | %s |\n", b, s, lst, m, vs, mr / 1024, ma / 1024, ahpmax[k] / 1024, mf, md, te[k]
    }
  }
}' "$HOME/logs/thp-ab.log"
echo "# thp-ab end $(date -u +%FT%TZ)"
