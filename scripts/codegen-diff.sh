#!/bin/sh
# codegen-diff.sh: prove that a source change did not change the machine code.
#
#   sh scripts/codegen-diff.sh capture [DIR]   build both quantum variants in
#                                             release and save the .so, its
#                                             disassembly and section sizes
#                                             under DIR (default .tmp-codegen)
#   sh scripts/codegen-diff.sh compare [DIR]   rebuild and compare with DIR
#
# Runs inside the dev container (needs cargo, objdump, readelf, objcopy).
# `compare` prints, per variant:
#   .text hash          identical => byte-identical machine code, done
#   exact               functions whose instruction text matches a baseline
#                       function exactly once addresses are stripped
#   reloc-only          functions that match once immediates are masked too
#                       (moving code between files shifts .rodata and every
#                       PC-relative offset with it; the instructions are the
#                       same)
#   changed             functions with no counterpart, with a unified diff
# A refactor that moves code without changing it shows zero changed
# functions and a mix of exact and reloc-only. Two things the toolchain
# does on its own can still show up: GNU ld's Cortex-A53 erratum 843419
# workaround rewrites an `adrp` that lands at page offset 0xff8/0xffc into
# a veneer (one function changes by a branch, .text grows by a 4 KiB stub
# section), and moving a `static` between files moves its slot in the
# AArch64 merged-globals blob (ldr/ldur encodings flip in unrelated
# functions), so statics stay in their original file. Anything else in
# `changed` is a real code change and needs the benchmark protocol.
set -eu
MODE=${1:-}; DIR=${2:-.tmp-codegen}
[ -n "$MODE" ] || { sed -n '2,/^set -eu/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 1; }
mkdir -p "$DIR"

build() { # build VARIANT -> prints path of the .so
  case "$1" in
    q16) cargo build --release -q 2>&1 | tail -3 >&2; echo target/release/libchalloc.so;;
    q8)  CARGO_TARGET_DIR=target/q8 cargo build --release -q --no-default-features --features quantum8 2>&1 | tail -3 >&2; echo target/q8/release/libchalloc.so;;
  esac
}
text_hash() { objcopy -O binary --only-section=.text "$1" "$DIR/.text.bin" && sha256sum "$DIR/.text.bin" | cut -c1-16; rm -f "$DIR/.text.bin"; }
sections() { readelf -S -W "$1" | awk '$2 ~ /^\.(text|rodata|data\.rel\.ro|data|bss)$/ {printf "%s=%s ", $2, $6}'; }
disasm() { objdump -d --no-show-raw-insn "$1"; }
# Per-function comparison: scripts/codegen-fncmp.py (python3, in the dev
# image) matches every new function to a baseline function by exact
# instruction text, then by text with immediates masked, and prints a
# unified diff for anything left.
FNCMP="$(dirname "$0")/codegen-fncmp.py"

case "$MODE" in
  capture)
    git rev-parse HEAD > "$DIR/commit.txt"
    for v in q16 q8; do
      so=$(build $v); cp "$so" "$DIR/lib-$v.so"
      disasm "$so" > "$DIR/disasm-$v.txt"
      echo "$v: .text $(text_hash "$so") $(sections "$so") functions $(grep -c '^[0-9a-f]* <.*>:$' "$DIR/disasm-$v.txt")"
    done
    echo "captured at $(cat "$DIR/commit.txt")";;
  compare)
    echo "baseline $(cat "$DIR/commit.txt"), tree $(git rev-parse HEAD)$(git diff --quiet HEAD -- src Cargo.toml build.rs || echo ' + uncommitted src changes')"
    rc=0
    for v in q16 q8; do
      so=$(build $v)
      oh=$(text_hash "$DIR/lib-$v.so"); nh=$(text_hash "$so")
      disasm "$so" > "$DIR/disasm-$v-new.txt"
      echo "== $v"
      echo "   .text hash   old $oh  new $nh $( [ "$oh" = "$nh" ] && echo IDENTICAL || echo differs)"
      echo "   sections old: $(sections "$DIR/lib-$v.so")"
      echo "   sections new: $(sections "$so")"
      if [ "$oh" != "$nh" ]; then
        python3 "$FNCMP" "$DIR/disasm-$v.txt" "$DIR/disasm-$v-new.txt" > "$DIR/report-$v.txt" || true
        sed 's/^/   /' "$DIR/report-$v.txt" | head -60
        grep -q 'changed 0$' "$DIR/report-$v.txt" || rc=2
      fi
    done
    exit $rc;;
  *) echo "unknown mode $MODE" >&2; exit 1;;
esac
