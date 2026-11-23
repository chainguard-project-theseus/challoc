#!/bin/sh
# ci/kani-regular-sharded-test.sh: ci/kani-regular-sharded.sh against a
# stand-in make, in a scratch tree, with no Kani. The stand-in prints the
# `kani: shard S of N: K of T harnesses` line the kit's kani.sh prints,
# splitting FAKE_TOTAL harnesses over the shards, and FAKE_MODE breaks
# shard 2: `drop` loses a harness from its count, `total` reports a
# different T with the same K, `fail` exits 1, `silent` prints no shard
# line, and `copy` makes the tree copy for shard 2 fail (a stand-in tar
# on PATH) after shard 1 has started. Each case must give its exit
# status: a count that is not a whole number from 1 to 999 exits 2 with
# no shard started, a full split exits 0, and every broken one exits 1;
# after a failed copy, the shard already started must still be waited
# for and reported.
set -eu

script=$(cd "$(dirname "$0")" && pwd)/kani-regular-sharded.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/fake-make" <<'EOF'
#!/bin/sh
s=; n=
for a in "$@"; do
  case "$a" in
    KANI_SHARD=*) s=${a#*=} ;;
    KANI_SHARDS=*) n=${a#*=} ;;
  esac
done
total=${FAKE_TOTAL:-10}
k=$((total / n))
[ "$s" -ne 1 ] || k=$((k + total % n))
if [ "$s" -eq 2 ]; then
  case "${FAKE_MODE:-ok}" in
    drop) k=$((k - 1)) ;;
    total) echo "kani: shard $s of $n: $k of $((total + 1)) harnesses"; exit 0 ;;
    fail) echo "kani: shard $s of $n: $k of $total harnesses"; exit 1 ;;
    silent) exit 0 ;;
  esac
fi
echo "kani: shard $s of $n: $k of $total harnesses"
EOF
chmod +x "$tmp/fake-make"

# A tar that fails the extract into shard 2's copy, for FAKE_MODE=copy,
# and is the real tar otherwise.
mkdir -p "$tmp/bin"
real_tar=$(command -v tar)
cat > "$tmp/bin/tar" <<EOF
#!/bin/sh
if [ "\${FAKE_MODE:-}" = copy ]; then
  case " \$* " in
    *" -C "*"/2 "*) cat > /dev/null; exit 1 ;;
  esac
fi
exec "$real_tar" "\$@"
EOF
chmod +x "$tmp/bin/tar"

mkdir -p "$tmp/tree/ci"
: > "$tmp/tree/ci/kani-weights.txt"

bad=0
# expect STATUS MODE SHARDS: run the script, which must exit STATUS.
expect() {
  want=$1 mode=$2 shards=$3
  rc=0
  (cd "$tmp/tree" && PATH="$tmp/bin:$PATH" FAKE_MODE=$mode MAKE="$tmp/fake-make" sh "$script" 16 "$shards") > "$tmp/out" 2>&1 || rc=$?
  if [ "$rc" -eq "$want" ]; then
    echo "PASS shards='$shards' mode=$mode: exit $rc"
  else
    echo "FAIL shards='$shards' mode=$mode: exit $rc, wanted $want"
    cat "$tmp/out"
    bad=1
  fi
  if [ "$want" -eq 2 ] && [ -e "$tmp/tree/target/kani-shards/q16/1.log" ]; then
    echo "FAIL shards='$shards': a shard started"
    bad=1
  fi
  if [ "$mode" = copy ] && ! grep -q '^=== shard 1 of 4, quantum 16: passed$' "$tmp/out"; then
    echo "FAIL mode=copy: shard 1 was not waited for and reported"
    cat "$tmp/out"
    bad=1
  fi
  rm -rf "$tmp/tree/target"
}

for shards in 0 -1 8x '' 007 1000 99999999999999999999; do
  expect 2 ok "$shards"
done
expect 0 ok 1
expect 0 ok 4
expect 0 ok 8
expect 1 drop 4
expect 1 total 4
expect 1 fail 4
expect 1 silent 4
expect 1 copy 4

[ "$bad" -eq 0 ] || exit 1
echo "PASS kani-regular-sharded-test"
