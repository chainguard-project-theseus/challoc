#!/bin/sh
# codegen-diff.sh: prove that a source change did not change the machine code.
#
#   sh scripts/codegen-diff.sh capture [DIR]   build both quantum variants in
#                                             release and save the .so, its
#                                             disassembly and section sizes
#                                             under DIR (default
#                                             target/codegen-baseline)
#   sh scripts/codegen-diff.sh compare [DIR]   rebuild and compare with DIR
#
# The default DIR is under target/, so in the dev container it lives in the
# per-architecture target volume next to the builds and `cargo clean`
# removes it; capture and compare on the same architecture.
#
# Runs inside the dev container (needs cargo, objdump, readelf, objcopy,
# python3; c++filt, when present, adds demangled names).
# `compare` prints, per variant:
#   .text hash          identical => byte-identical machine code, done
# and when the hash differs, per function paired by name with its baseline:
#   exact               instruction text matches once addresses are stripped
#   reloc-only          matches once immediates are masked too (moving code
#                       between files shifts .rodata and every PC-relative
#                       offset with it; the instructions are the same)
#   padding-only        matches once trailing alignment nops are dropped too
#                       (the function moved and the gap to the next aligned
#                       symbol changed)
#   erratum-stub        GNU ld's Cortex-A53 erratum 843419 workaround: an
#                       `adrp` at page offset 0xff8/0xffc became `adr` (or
#                       its load a branch to a stub), and the stub follows
#                       the body; .text can grow by a 4 KiB stub page
#   changed             anything else, with its full unified diff, plus
#                       every baseline function left with no counterpart
# A new name pairs with a leftover baseline function whose name differs
# only in the crate hash or impl disambiguator, else with one of the same
# instructions, as a rename. Each padding-only, erratum-stub, renamed and
# changed function gets a line naming it and its baseline. A refactor
# that moves code without changing it shows zero changed functions. One
# more thing the toolchain does on its own can still show up: moving a
# `static` between files moves its slot in the AArch64 merged-globals blob
# (ldr/ldur encodings flip in unrelated functions), so statics stay in
# their original file. Anything in `changed` is a real code change and
# needs the benchmark protocol.
set -eu
MODE=${1:-}; DIR=${2:-target/codegen-baseline}
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
# image) pairs every new function with its baseline function by name,
# classifies the pair (the list above), and prints the report: the counts,
# one line per function that is neither exact nor reloc-only, the baseline
# functions with no counterpart, and the full diff of every changed one.
FNCMP="$(dirname "$0")/codegen-fncmp.py"
# The container runs as root over a tree a Linux host user owns, which
# git refuses as "dubious ownership" without safe.directory.
GIT="git -c safe.directory=*"

case "$MODE" in
  capture)
    $GIT rev-parse HEAD > "$DIR/commit.txt"
    for v in q16 q8; do
      so=$(build $v); cp "$so" "$DIR/lib-$v.so"
      disasm "$so" > "$DIR/disasm-$v.txt"
      echo "$v: .text $(text_hash "$so") $(sections "$so") functions $(grep -c '^[0-9a-f]* <.*>:$' "$DIR/disasm-$v.txt")"
    done
    echo "captured at $(cat "$DIR/commit.txt")";;
  compare)
    echo "baseline $(cat "$DIR/commit.txt"), tree $($GIT rev-parse HEAD)$($GIT diff --quiet HEAD -- src Cargo.toml build.rs || echo ' + uncommitted src changes')"
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
        python3 "$FNCMP" "$DIR/disasm-$v.txt" "$DIR/disasm-$v-new.txt" > "$DIR/report-$v.txt" ||
          { echo "   scripts/codegen-fncmp.py failed (exit $?)"; rc=2; }
        sed 's/^/   /' "$DIR/report-$v.txt"
        grep -q 'changed 0$' "$DIR/report-$v.txt" || rc=2
      fi
    done
    exit $rc;;
  *) echo "unknown mode $MODE" >&2; exit 1;;
esac
