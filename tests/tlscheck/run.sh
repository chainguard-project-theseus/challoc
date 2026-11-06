#!/bin/sh
# tests/tlscheck/run.sh: the C lifecycle tests and a bounded fuzz of the
# debug TLS check (src/heap/tlscheck.rs) under LD_PRELOAD of a check build:
#
#   RUSTFLAGS="--cfg challoc_tls_check" cargo build --release
#   sh tests/tlscheck/run.sh
#
# Every mode of lifecycle.c (fork, exitrace, churn, dlopen with tlslib.c,
# keyafter) must exit 0 and print `ok <mode>`, then fuzz.c runs for
# TLSCHECK_FUZZ_SECS seconds (default 120; 0 skips it) with
# TLSCHECK_FUZZ_THREADS threads (default 16) from TLSCHECK_FUZZ_SEED
# (default random, printed; it fixes each thread's operation sequence, not
# the interleaving); it must see a fork. A broken slot-to-heap
# link aborts the process, which fails the run. Both programs read the
# check's counters (challoc_tls_check_stats) and fail on a library built
# without the check. The library is CHALLOC_LIB, by default
# target/release/libchalloc.so under CARGO_TARGET_DIR or the tree. The
# programs are compiled by scripts/ctest-cc.sh.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
LIB=${CHALLOC_LIB:-${CARGO_TARGET_DIR:-$ROOT/target}/release/libchalloc.so}
[ -f "$LIB" ] || { echo "tlscheck: $LIB missing: RUSTFLAGS=\"--cfg challoc_tls_check\" cargo build --release first" >&2; exit 2; }
LIB=$(cd "$(dirname "$LIB")" && pwd)/$(basename "$LIB")
export CHALLOC_LIB="$LIB"
secs=${TLSCHECK_FUZZ_SECS:-120}
threads=${TLSCHECK_FUZZ_THREADS:-16}
seed=${TLSCHECK_FUZZ_SEED:-$(od -An -N4 -tu4 /dev/urandom | tr -d ' ')}
for v in "$secs" "$threads" "$seed"; do
  case $v in
    ''|*[!0-9]*) echo "tlscheck: TLSCHECK_FUZZ_SECS, _THREADS and _SEED must be decimal integers, got '$v'" >&2; exit 2 ;;
  esac
done
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
cc_test() { sh "$ROOT/scripts/ctest-cc.sh" "$@"; }
cc_test "$ROOT/tests/tlscheck/lifecycle.c" -o "$out/lifecycle"
cc_test "$ROOT/tests/tlscheck/fuzz.c" -o "$out/fuzz"
cc_test -shared -fPIC "$ROOT/tests/tlscheck/tlslib.c" -o "$out/libtlslib.so"
status=0
for mode in fork exitrace churn dlopen keyafter; do
  start=$(date +%s)
  rc=0
  LD_PRELOAD=$LIB timeout 900 "$out/lifecycle" "$mode" "$out/libtlslib.so" >"$out/$mode.log" 2>&1 || rc=$?
  cat "$out/$mode.log"
  echo "tlscheck: lifecycle $mode rc=$rc $(( $(date +%s) - start ))s"
  if [ "$rc" != 0 ] || ! grep -qx "ok $mode" "$out/$mode.log"; then
    echo "tlscheck: FAIL lifecycle $mode" >&2
    status=1
  fi
done
if [ "$secs" -gt 0 ]; then
  echo "tlscheck: fuzz $secs s, seed $seed, $threads threads"
  rc=0
  LD_PRELOAD=$LIB timeout $((secs + 120)) "$out/fuzz" "$secs" "$seed" "$threads" >"$out/fuzz.log" 2>&1 || rc=$?
  cat "$out/fuzz.log"
  if [ "$rc" != 0 ] || ! grep -qx "ok fuzz" "$out/fuzz.log"; then
    echo "tlscheck: FAIL fuzz rc=$rc (rerun: TLSCHECK_FUZZ_SEED=$seed TLSCHECK_FUZZ_SECS=$secs TLSCHECK_FUZZ_THREADS=$threads)" >&2
    status=1
  fi
fi
[ "$status" = 0 ] && echo "tlscheck: ok"
exit $status
