#!/bin/sh
# Sample profile of larson under challoc, standing in for `perf record` (Wolfi
# ships no perf). Runs larson once with bench/sampler.so preloaded after
# libchalloc.so, then attributes the sampled PCs inside libchalloc.so to
# functions (nm) and instructions (objdump), flagging lock-prefixed or
# atomic read-modify-write instructions. Prints a markdown section.
#   sh bench/profile.sh OUTDIR        (PROFILE_SECS=20 larson seconds, SAMPLER_HZ=1000)
#
# ITIMER_PROF fires at most once per kernel tick of process CPU time, so the
# effective rate is a few hundred samples per second; a 20 s run gives enough
# samples for a stable top 10. The outlined LSE helpers (`__aarch64_cas4_acq`
# and friends) are counted as atomic RMW symbols wholesale.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
BIN=$ROOT/target/bench-bin
OUT=${1:-$ROOT/target/bench-out}
mkdir -p "$OUT"
LIB=$ROOT/target/release/libchalloc.so
PROCS=$(nproc)
ARCH=$(uname -m)
PROFILE_SECS=${PROFILE_SECS:-20}

rm -f "$OUT/larson.samples"
SAMPLER_OUT=$OUT/larson.samples SAMPLER_HZ=${SAMPLER_HZ:-1000} \
  LD_PRELOAD="$LIB:$BIN/sampler.so" \
  "$BIN/larson" "$PROFILE_SECS" 8 1000 5000 100 4141 "$PROCS" >"$OUT/larson.profile.out" 2>&1

nm -n --defined-only "$LIB" | c++filt | awk '$2 ~ /^[tTwW]$/ {print $1, substr($0, index($0, $3))}' >"$OUT/challoc.nm"
objdump -d --no-show-raw-insn "$LIB" | awk '/^ *[0-9a-f]+:/ {sub(":", "", $1); print $1, $2}' >"$OUT/challoc.insn"

case $ARCH in
  aarch64) RMW='^(ld(add|clr|set|eor|smax|smin|umax|umin|xr|axr|xp|axp)|st(xr|lxr|xp|lxp)|swp|cas)';;
  x86_64)  RMW='^(lock|xchg|cmpxchg|xadd)';;
  *) RMW='^$';;
esac

echo
echo "## larson profile under challoc ($ARCH, $PROFILE_SECS s run, SIGPROF sampler at ${SAMPLER_HZ:-1000} Hz of CPU time)"
echo
echo '```'
awk -v rmw="$RMW" '
function hex(s,  i, c, v) { v = 0; s = tolower(s); for (i = 1; i <= length(s); i++) v = v * 16 + index("0123456789abcdef", substr(s, i, 1)) - 1; return v }
function sym(a,  lo, hi, mid) { lo = 1; hi = ns; if (a < sa[1]) return "<before first symbol>"; while (lo < hi) { mid = int((lo + hi + 1) / 2); if (sa[mid] <= a) lo = mid; else hi = mid - 1 } return sn[lo] }
function sortkeys(arr, keys,  k, n, i, j, t) { n = 0; for (k in arr) keys[++n] = k; for (i = 2; i <= n; i++) { t = keys[i]; for (j = i - 1; j >= 1 && arr[keys[j]] < arr[t]; j--) keys[j + 1] = keys[j]; keys[j + 1] = t } return n }
FILENAME == ARGV[1] { ns++; sa[ns] = hex($1); sn[ns] = substr($0, index($0, " ") + 1); next }
FILENAME == ARGV[2] { mn[hex($1)] = $2; next }
/^#/ { print; next }
{
  c = $1; mod = $2; total += c; modc[mod] += c
  if (mod != "libchalloc.so") next
  ch += c; a = hex($3); s = sym(a); m = (a in mn) ? mn[a] : "?"
  symc[s] += c; insc[a] += c; insm[a] = m; inss[a] = s
  if (m ~ rmw || s ~ /^__aarch64_(cas|swp|ldadd|ldclr|ldset|ldeor)/) { rmwc[s] += c; rmwtot += c }
}
END {
  printf "total samples %d; in libchalloc.so %d (%.1f%%)\n\n", total, ch, total ? 100.0 * ch / total : 0
  print "by module:"; n = sortkeys(modc, k); for (i = 1; i <= n && i <= 8; i++) printf "  %7d %5.1f%%  %s\n", modc[k[i]], 100.0 * modc[k[i]] / total, k[i]
  printf "\ntop 10 challoc symbols (rmw = samples on lock/atomic RMW instructions inside that symbol; outlined LSE helpers count whole):\n"
  n = sortkeys(symc, k)
  for (i = 1; i <= n && i <= 10; i++) printf "  %7d %5.1f%%  rmw=%-5d %s\n", symc[k[i]], 100.0 * symc[k[i]] / ch, rmwc[k[i]] + 0, k[i]
  printf "\nsamples on lock/atomic RMW instructions anywhere in challoc: %d (%.2f%% of challoc samples)\n", rmwtot, ch ? 100.0 * rmwtot / ch : 0
  printf "\ntop 15 challoc instructions:\n"
  n = sortkeys(insc, k)
  for (i = 1; i <= n && i <= 15; i++) printf "  %7d %5.1f%%  %8x  %-8s %s\n", insc[k[i]], 100.0 * insc[k[i]] / ch, k[i], insm[k[i]], inss[k[i]]
}' "$OUT/challoc.nm" "$OUT/challoc.insn" "$OUT/larson.samples"
echo '```'
echo
echo "larson output:"
echo
echo '```'
grep -i 'throughput\|ops\|Program' "$OUT/larson.profile.out" | head -5
echo '```'
