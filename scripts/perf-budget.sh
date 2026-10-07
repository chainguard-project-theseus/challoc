#!/bin/sh
# perf-budget.sh: check the hot exports' fast-path cost bounds
# (verus/THEOREMS.md theorem 45) on an x86_64 release build.
#
#   sh scripts/perf-budget.sh [q16|q8] [LIB]
#
# Builds the checker (scripts/perf-budget.rs, std only) with rustc, then,
# unless LIB names an already built libchalloc.so, builds the library with
# the pinned toolchain (`cargo +1.95.0 build --release`; PERF_TOOLCHAIN
# overrides the `+1.95.0`, PERF_TOOLCHAIN= uses the default toolchain),
# runs a self-test (five mutations of the disassembly and three of the
# table, each of which the checker must reject) and checks the library
# against verus/perf-budget.txt (q16, the default) or
# verus/perf-budget-q8.txt.
# The tables are pinned to rustc 1.95.0; another compiler may lay the
# fast paths out differently, and the check then fails with the measured
# counts. On a deliberate change, `--print` gives the new table lines:
#   /path/to/perf-budget --print LIB verus/perf-budget.txt
# Needs GNU objdump; x86_64 only (an aarch64 library is refused).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
VARIANT=${1:-q16}
LIB=${2:-}
TC=${PERF_TOOLCHAIN-+1.95.0}
case "$VARIANT" in
  q16) table=$root/verus/perf-budget.txt; features=;;
  q8)  table=$root/verus/perf-budget-q8.txt; features="--no-default-features --features quantum8";;
  *) echo "usage: $0 [q16|q8] [LIB]" >&2; exit 2;;
esac
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
trap 'exit 130' INT TERM
# shellcheck disable=SC2086
rustc ${TC:+"$TC"} -O --edition 2021 "$here/perf-budget.rs" -o "$out/perf-budget"
if [ -z "$LIB" ]; then
  tgt=${CARGO_TARGET_DIR:-$root/target}
  [ "$VARIANT" = q8 ] && tgt=$tgt/q8
  # shellcheck disable=SC2086
  (cd "$root" && CARGO_TARGET_DIR=$tgt cargo ${TC:+"$TC"} build --release -q $features)
  LIB=$tgt/release/libchalloc.so
fi
rustc ${TC:+"$TC"} --version
echo "library $LIB ($VARIANT)"

# Self-test: each mutation of `objdump -d`'s output (malloc's body only)
# must make the checker fail with the named message.
cat > "$out/objdump" <<'EOF'
#!/bin/sh
case "$*" in
  *-d*) objdump "$@" | awk -v m="$MUT" '
    /^[0-9a-f]+ <malloc>:$/ { inm = 1; entry = $1; sub(/^0+/, "", entry) }
    /^$/ { inm = 0 }
    inm && !done && m == "load" && /\tmov +r[a-z0-9]+,r[a-z0-9]+$/ {
      sub(/\tmov .*/, "\tmov    rax,QWORD PTR [rdi]"); done = 1 }
    inm && !done && m == "call" && /\tret/ { sub(/\tret.*/, "\tcall   " entry " <malloc>"); done = 1 }
    inm && !done && m == "loop" && /\tret/ { sub(/\tret.*/, "\tjmp    " entry " <malloc>"); done = 1 }
    inm && !done && m == "unknown" && /\tret/ { sub(/\tret.*/, "\tvzeroupper"); done = 1 }
    # The second instruction after the entry, deleted: a gap.
    inm && m == "gap" && /^ *[0-9a-f]+:\t/ { n++; if (n == 2) next }
    { print }';;
  *) exec objdump "$@";;
esac
EOF
chmod +x "$out/objdump"
for mut in "load:max loads GREW" "call:P2 malloc: call" "loop:P3: cycle" "unknown:unclassified instruction" "gap:fell out of the body"; do
  m=${mut%%:*}; want=${mut#*:}
  if MUT=$m PERF_BUDGET_OBJDUMP="$out/objdump" "$out/perf-budget" "$LIB" "$table" > "$out/log" 2>&1; then
    echo "perf-budget self-test: mutation '$m' was not rejected" >&2; exit 1
  fi
  grep -q "$want" "$out/log" || { echo "perf-budget self-test: mutation '$m' failed without '$want':" >&2; cat "$out/log" >&2; exit 1; }
done
# Mutations of the table: a count that shrank, a configuration cell at a
# stale offset, a change to a fast path below the worst one.
for mut in "shrank:max insns shrank" "cell:is stale" "path:the fast paths' counts"; do
  m=${mut%%:*}; want=${mut#*:}
  case "$m" in
    shrank) sed 's/^\(budget  *malloc  *-  *\)\([0-9]*\)/\1999/' "$table";;
    cell) sed 's/^\(cell free_flags [^ ]*\) [^ ]* /\1 0x0 /' "$table";;
    path) sed 's/^\(paths  *free  *-  *\)[^ ]* /\1/' "$table";;
  esac > "$out/table"
  if "$out/perf-budget" "$LIB" "$out/table" > "$out/log" 2>&1; then
    echo "perf-budget self-test: table mutation '$m' was not rejected" >&2; exit 1
  fi
  grep -q "$want" "$out/log" || { echo "perf-budget self-test: table mutation '$m' failed without '$want':" >&2; cat "$out/log" >&2; exit 1; }
done
echo "self-test: 8 mutations rejected"
"$out/perf-budget" "$LIB" "$table"
